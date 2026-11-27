//! SAM 3's feature pyramids on the GPU: the necks of `lightcraft-segment` (`neck.rs`, the reference) for the click
//! decoder (`tracker_neck`) and the text detector (`detector_model.vision_encoder.neck`), the three finest levels of
//! each (288, 144 and 72 per side, 256 channels), from the backbone's `[1024, 72, 72]`.
//!
//! Images are kept as pixel rows (`[pixels, channels]`), so every step is a product of the backbone's matrix kernel:
//! - a 2 × 2 stride-2 transposed convolution is one product per input pixel, `[pixels, cin] · [cin, 4 · cout]`
//!   (the checkpoint's `[cin, cout, 2, 2]` laid out as `[(ky, kx, cout), cin]` on upload), whose row holds the
//!   pixel's four outputs one after the other; GELU (erf) between the two of level 0 runs in its epilogue;
//! - `proj1` (1 × 1) is a plain product, and does not care in which order the pixels come;
//! - `unshuffle` (`wgsl/sam3_neck.wgsl`) then puts the pixels of the transposed convolutions in row-major order;
//! - `proj2` (3 × 3, padding 1) is the same kernel with an A-tile loader that reads the 9 · cin numbers of a pixel's
//!   neighbourhood straight from the image (`wgsl/sam3_conv.wgsl`): an implicit GEMM, the im2col matrix (764 MB at
//!   288²) never exists;
//! - `to_chw` (`wgsl/sam3_ops.wgsl`) writes the channels-first result.
//!
//! Nothing is uploaded with the backbone: the kernels compile, the work buffers (~190 MB, shared by both necks) are
//! made and a neck's weights (~27 MB) are uploaded when that neck is first asked for. A level is one submission
//! and one read-back, so no buffer exceeds the backbone's limits. Level 3 (36², a max pool) is not needed by either
//! head and is not run here.

use std::sync::{Mutex, OnceLock, PoisonError};

use lightcraft_segment::{NeckKind, WeightReader};

use super::{Dev, GRID, Graph, HIDDEN, Matmul, Precision, Recorded, TILE, TIMEOUT, buffer, matmul_source_with, pipeline, u32_of, upload};

#[cfg(test)]
pub(super) mod tests;

/// Channels of every level.
const FPN: usize = 256;
/// Levels run here: 288², 144² and 72².
const LEVELS: usize = 3;

/// The sizes of a neck: the backbone's side and channels, and the channels of the levels (SAM 3's, or small ones
/// in tests).
#[derive(Clone, Copy, Debug)]
struct Sizes {
    grid: usize,
    hidden: usize,
    fpn: usize,
}

const SAM3: Sizes = Sizes { grid: GRID, hidden: HIDDEN, fpn: FPN };

impl Sizes {
    /// Floats of the largest step of level 0: the second transposed convolution's output (4 · grid² · hidden), or a
    /// 256-channel image of (4 · grid)² (both 85 MB for SAM 3).
    fn largest(self) -> usize {
        self.grid * self.grid * (4 * self.hidden).max(16 * self.fpn)
    }
}

/// Where a neck's tensors are in the checkpoint.
fn prefix(which: NeckKind) -> &'static str {
    match which {
        NeckKind::Tracker => "tracker_neck",
        NeckKind::Detector => "detector_model.vision_encoder.neck",
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// kernels

pub(super) struct Kernels {
    /// The 3 × 3 convolution: the matrix product with the implicit-im2col A loader.
    conv: Matmul,
    to_rows: wgpu::ComputePipeline,
    unshuffle: wgpu::ComputePipeline,
}

static KERNELS: OnceLock<Result<Kernels, String>> = OnceLock::new();

/// The neck's own kernels, compiled on first use (the backbone's are compiled with the device; the mask decoder uses
/// the 3 × 3 convolution too).
pub(super) fn kernels(dev: &'static Dev) -> Result<&'static Kernels, String> {
    KERNELS
        .get_or_init(|| {
            let validation = dev.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let pipe = |label: &str, source: &str, entry: &str| pipeline(&dev.device, &dev.pipeline_layout, label, source, entry);
            let conv = matmul_source_with(TILE, false, Some(include_str!("../wgsl/sam3_conv.wgsl")));
            let ops = include_str!("../wgsl/sam3_neck.wgsl");
            let k = Kernels {
                conv: Matmul { pipeline: pipe("sam3 conv3x3", &conv, "main"), tile: TILE },
                to_rows: pipe("sam3 neck", ops, "to_rows"),
                unshuffle: pipe("sam3 neck", ops, "unshuffle"),
            };
            if let Some(e) = pollster::block_on(validation.pop()) {
                return Err(format!("{}: the SAM 3 neck kernels did not compile ({e})", dev.info.name));
            }
            Ok(k)
        })
        .as_ref()
        .map_err(Clone::clone)
}

impl Graph {
    /// The 3 × 3 convolution (padding 1) of the image `x` (`[side², cin]`, cin a multiple of 4) with `w`
    /// (`[cout, 3, 3, cin]`) and `bias` (`[cout]`) into `y` (`[side², cout]`).
    pub(super) fn conv3(
        &mut self,
        k: &'static Kernels,
        [x, w, bias, y]: [&wgpu::Buffer; 4],
        side: usize,
        (cin, cout): (usize, usize),
    ) -> Result<(), String> {
        if side == 0 || !cin.is_multiple_of(4) || cin == 0 || cout == 0 {
            return Err(format!("a {side}² {cin} → {cout} convolution the SAM 3 kernel cannot do"));
        }
        let (m, depth) = (side * side, 9 * cin);
        let params = [u32_of(m)?, u32_of(cout)?, u32_of(depth)?, 1, 0, 0, u32_of(side)?, 0];
        let groups = [u32_of(cout.div_ceil(k.conv.tile.bn()))?, u32_of(m.div_ceil(k.conv.tile.bm()))?, 1];
        if groups[1] > self.dev.limits.max_compute_workgroups_per_dimension {
            return Err("an image too large for the SAM 3 convolution kernel".into());
        }
        self.push(&k.conv.pipeline, params, [x, w, bias, y], groups);
        Ok(())
    }

    /// `[channels, pixels]` (channels-first) to `[pixels, channels]`.
    fn channels_last(&mut self, k: &'static Kernels, src: &wgpu::Buffer, dst: &wgpu::Buffer, channels: usize, pixels: usize) -> Result<(), String> {
        let count = channels.checked_mul(pixels).ok_or("an image too large")?;
        let (g, row) = self.flat(count.div_ceil(256))?;
        let dummy = &self.dev.dummy;
        self.push(&k.to_rows, [u32_of(channels)?, u32_of(pixels)?, 0, 0, 0, 0, u32_of(count)?, row], [src, dummy, dummy, dst], g);
        Ok(())
    }

    /// The rows of a `side × side` image after `ups` 2 × 2 stride-2 transposed convolutions (in their order, see
    /// `wgsl/sam3_neck.wgsl`) in row-major order.
    fn unshuffle(
        &mut self,
        k: &'static Kernels,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        channels: usize,
        side: usize,
        ups: usize,
    ) -> Result<(), String> {
        if ups >= 16 || !side.is_multiple_of(1 << ups) {
            return Err("a reorder the SAM 3 kernel cannot do".into());
        }
        let count = channels.checked_mul(side * side).ok_or("an image too large")?;
        let (g, row) = self.flat(count.div_ceil(256))?;
        let dummy = &self.dev.dummy;
        self.push(&k.unshuffle, [u32_of(channels)?, u32_of(side)?, u32_of(ups)?, 0, 0, 0, u32_of(count)?, row], [src, dummy, dummy, dst], g);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// weights

/// A transposed convolution's weights `[cin, cout, 2, 2]` as the rows of a product: `[(ky · 2 + kx) · cout + co, ci]`.
fn transposed_rows(w: &[f32], cin: usize, cout: usize) -> Vec<f32> {
    let mut out = vec![0f32; w.len()];
    for ci in 0..cin {
        for co in 0..cout {
            for q in 0..4 {
                if let (Some(o), Some(v)) = (out.get_mut((q * cout + co) * cin + ci), w.get((ci * cout + co) * 4 + q)) {
                    *o = *v;
                }
            }
        }
    }
    out
}

/// A convolution's weights `[cout, cin, k, k]` in the order of the convolution's A columns: `[cout, k, k, cin]`.
fn conv_rows(w: &[f32], cout: usize, cin: usize, k: usize) -> Vec<f32> {
    let taps = k * k;
    let mut out = vec![0f32; w.len()];
    for co in 0..cout {
        for ci in 0..cin {
            for t in 0..taps {
                if let (Some(o), Some(v)) = (out.get_mut((co * taps + t) * cin + ci), w.get((co * cin + ci) * taps + t)) {
                    *o = *v;
                }
            }
        }
    }
    out
}

/// A transposed convolution's weight `[cin, cout, 2, 2]` and bias on the device: rows `[4 · cout, cin]`, and the
/// bias repeated for the four outputs.
fn transposed_weights(dev: &Dev, label: &str, w: &[f32], b: &[f32], cin: usize, cout: usize) -> [wgpu::Buffer; 2] {
    [upload(dev, label, &transposed_rows(w, cin, cout)), upload(dev, label, &b.repeat(4))]
}

/// A `k × k` convolution's weight `[cout, cin, k, k]` and bias on the device: rows `[cout, k, k, cin]`.
pub(super) fn conv_weights(dev: &Dev, label: &str, w: &[f32], b: &[f32], (cin, cout, k): (usize, usize, usize)) -> [wgpu::Buffer; 2] {
    [upload(dev, label, &conv_rows(w, cout, cin, k)), upload(dev, label, b)]
}

/// One level's weights on the device: the transposed convolutions (none, one or two), `proj1`, `proj2`.
struct LevelWeights {
    up: Vec<[wgpu::Buffer; 2]>,
    proj1: [wgpu::Buffer; 2],
    proj2: [wgpu::Buffer; 2],
}

/// Reads a neck's tensors, checking their shapes.
struct Loader<'a> {
    weights: &'a WeightReader,
    dev: &'static Dev,
    prefix: &'static str,
    bytes: u64,
}

impl Loader<'_> {
    fn read(&self, name: &str, shape: &[usize]) -> Result<Vec<f32>, String> {
        let full = format!("{}.{name}", self.prefix);
        let (got, data) = self.weights.read(&full).map_err(|e| format!("{full}: {e}"))?;
        if got != shape {
            return Err(format!("{full}: shape {got:?}, expected {shape:?}"));
        }
        Ok(data)
    }

    /// A 2 × 2 stride-2 transposed convolution.
    fn transposed(&mut self, name: &str, cin: usize, cout: usize) -> Result<[wgpu::Buffer; 2], String> {
        let w = self.read(&format!("{name}.weight"), &[cin, cout, 2, 2])?;
        let b = self.read(&format!("{name}.bias"), &[cout])?;
        self.bytes += (w.len() + 4 * b.len()) as u64 * 4;
        Ok(transposed_weights(self.dev, name, &w, &b, cin, cout))
    }

    /// A `k × k` convolution.
    fn conv(&mut self, name: &str, cin: usize, cout: usize, k: usize) -> Result<[wgpu::Buffer; 2], String> {
        let w = self.read(&format!("{name}.weight"), &[cout, cin, k, k])?;
        let b = self.read(&format!("{name}.bias"), &[cout])?;
        self.bytes += (w.len() + b.len()) as u64 * 4;
        Ok(conv_weights(self.dev, name, &w, &b, (cin, cout, k)))
    }

    /// Level `i` (0: 4× up, 1: 2× up, 2: as is).
    fn level(&mut self, i: usize) -> Result<LevelWeights, String> {
        let l = format!("fpn_layers.{i}");
        let s = format!("{l}.scale_layers");
        let (up, inter) = match i {
            0 => (
                vec![self.transposed(&format!("{s}.0"), HIDDEN, HIDDEN / 2)?, self.transposed(&format!("{s}.2"), HIDDEN / 2, HIDDEN / 4)?],
                HIDDEN / 4,
            ),
            1 => (vec![self.transposed(&format!("{s}.0"), HIDDEN, HIDDEN / 2)?], HIDDEN / 2),
            2 => (Vec::new(), HIDDEN),
            _ => return Err(format!("level {i} of the SAM 3 neck does not run on the GPU")),
        };
        Ok(LevelWeights { up, proj1: self.conv(&format!("{l}.proj1"), inter, FPN, 1)?, proj2: self.conv(&format!("{l}.proj2"), FPN, FPN, 3)? })
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// the necks

/// The work buffers, shared by both necks (one runs at a time): the backbone as pixel rows, two buffers the steps
/// alternate between (the backbone's upload lands in the first), and the read-back.
struct Work {
    x: wgpu::Buffer,
    a: wgpu::Buffer,
    b: wgpu::Buffer,
    staging: wgpu::Buffer,
}

impl Work {
    fn new(dev: &Dev, sizes: Sizes) -> Work {
        let largest = sizes.largest();
        Work {
            x: buffer(dev, "sam3 neck input", sizes.hidden * sizes.grid * sizes.grid),
            a: buffer(dev, "sam3 neck ping", largest),
            b: buffer(dev, "sam3 neck pong", largest),
            staging: dev.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sam3 neck read-back"),
                size: (largest.max(4) * 4) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
        }
    }

    /// Bytes on the device: the read-back buffer is host memory on discrete GPUs and not counted.
    fn bytes(sizes: Sizes) -> u64 {
        ((sizes.hidden * sizes.grid * sizes.grid + 2 * sizes.largest()) * 4) as u64
    }
}

/// One level, recorded: its dispatches, and the buffer its `[256, side, side]` result ends in (`len` floats).
struct Level {
    recorded: Recorded,
    out: wgpu::Buffer,
    len: usize,
}

/// A neck with its weights uploaded and its levels recorded.
struct Built {
    levels: Vec<Level>,
    weight_bytes: u64,
}

/// The dispatches of level `i` from the pixel rows in `work.x` (level 0 first fills them from the channels-first
/// upload in `work.a`).
fn record_level(dev: &'static Dev, k: &'static Kernels, work: &Work, w: &LevelWeights, i: usize, sizes: Sizes) -> Result<Level, String> {
    let Sizes { grid, hidden, fpn } = sizes;
    let mut g = Graph::new(dev);
    if i == 0 {
        g.channels_last(k, &work.a, &work.x, hidden, grid * grid)?;
    }
    let mut spare = [&work.a, &work.b].into_iter().cycle();
    let mut next = || spare.next().ok_or_else(|| "no work buffer".to_string());
    let (mut cur, mut pixels, mut channels, mut side) = (&work.x, grid * grid, hidden, grid);
    for (j, [weight, bias]) in w.up.iter().enumerate() {
        let cout = channels / 2;
        let dst = next()?;
        // GELU between the two of level 0
        g.matmul([cur, weight, bias, dst], (pixels, 4 * cout, channels), 1, j + 1 < w.up.len(), false, Precision::F32)?;
        (cur, pixels, channels, side) = (dst, pixels * 4, cout, side * 2);
    }
    let dst = next()?;
    g.matmul([cur, &w.proj1[0], &w.proj1[1], dst], (pixels, fpn, channels), 1, false, false, Precision::F32)?;
    cur = dst;
    if !w.up.is_empty() {
        let dst = next()?;
        g.unshuffle(k, cur, dst, fpn, side, w.up.len())?;
        cur = dst;
    }
    let dst = next()?;
    g.conv3(k, [cur, &w.proj2[0], &w.proj2[1], dst], side, (fpn, fpn))?;
    cur = dst;
    let dst = next()?;
    g.channels_first(cur, dst, fpn, side, side)?;
    Ok(Level { recorded: g.record(), out: dst.clone(), len: fpn * side * side })
}

fn build(dev: &'static Dev, k: &'static Kernels, weights: &WeightReader, which: NeckKind, work: &Work) -> Result<Built, String> {
    let mut ld = Loader { weights, dev, prefix: prefix(which), bytes: 0 };
    let mut levels = Vec::with_capacity(LEVELS);
    for i in 0..LEVELS {
        let w = ld.level(i)?;
        levels.push(record_level(dev, k, work, &w, i, SAM3)?);
    }
    Ok(Built { levels, weight_bytes: ld.bytes })
}

/// Run `f` (buffer creation and uploads for `stage`), turning the device's out-of-memory and validation errors into an
/// `Err` that names the stage.
pub(super) fn scoped<T>(dev: &Dev, stage: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let oom = dev.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation = dev.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let made = f();
    // let the uploads land before the scopes close
    let _ = dev.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(TIMEOUT) });
    let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
    if let Some(e) = oom {
        return Err(format!("not enough GPU memory for {stage}: {e}"));
    }
    if let Some(e) = validation {
        return Err(format!("the GPU rejected {stage}: {e}"));
    }
    made
}

#[derive(Default)]
struct State {
    work: Option<Work>,
    tracker: Option<Built>,
    detector: Option<Built>,
}

/// Both necks of a backbone on the GPU, each built when first run.
pub(super) struct Necks {
    weights: WeightReader,
    state: Mutex<State>,
}

impl Necks {
    pub(super) fn new(weights: WeightReader) -> Necks {
        Necks { weights, state: Mutex::new(State::default()) }
    }

    /// The `n` finest levels of the `which` neck for `backbone` (`[1024, 72, 72]`), each `[256, s, s]`.
    pub(super) fn run(&self, dev: &'static Dev, which: NeckKind, backbone: &[f32], n: usize) -> Result<Vec<Vec<f32>>, String> {
        if n > LEVELS {
            return Err(format!("{n} levels asked for; the GPU neck makes the {LEVELS} finest"));
        }
        if backbone.len() != HIDDEN * GRID * GRID {
            return Err(format!("{} backbone values, expected {HIDDEN} × {GRID} × {GRID}", backbone.len()));
        }
        let Some(_work) = crate::exit::enter() else { return Err("LightCraft is closing".into()) };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let State { work, tracker, detector } = &mut *state;
        let work = match work {
            Some(w) => w,
            None => work.insert(scoped(dev, "the SAM 3 necks", || Ok(Work::new(dev, SAM3)))?),
        };
        let built = match which {
            NeckKind::Tracker => tracker,
            NeckKind::Detector => detector,
        };
        let built = match built {
            Some(b) => b,
            None => {
                let k = kernels(dev)?;
                built.insert(scoped(dev, "a SAM 3 neck", || build(dev, k, &self.weights, which, work))?)
            }
        };
        dev.queue.write_buffer(&work.a, 0, bytemuck::cast_slice(backbone));
        built.levels.iter().take(n).map(|l| l.recorded.run("a SAM 3 neck", dev, &l.out, &work.staging, l.len)).collect()
    }

    /// Device memory the necks built so far take (weights and work buffers).
    fn bytes(&self) -> u64 {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let weights: u64 = [&state.tracker, &state.detector].into_iter().flatten().map(|b| b.weight_bytes).sum();
        weights + if state.work.is_some() { Work::bytes(SAM3) } else { 0 }
    }
}

impl super::Sam3Gpu {
    /// Bytes of device memory the necks run so far take (0 before the first): their weights and the work buffers
    /// they share.
    pub fn neck_bytes(&self) -> u64 {
        self.necks.bytes()
    }
}
