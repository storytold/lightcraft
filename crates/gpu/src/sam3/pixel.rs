//! The mask decoder of SAM 3's text detector on the GPU (`lightcraft-segment`'s `detector.rs` is the reference): the
//! DETR encoder's memory (72 × 72 tokens of 256) attends to the prompt and is added to it (`prompt_cross_attn`, after
//! its layer norm), then the pixel decoder brings it up to 288 × 288 in two stages, each a nearest 2 × up-sampling plus
//! the detector neck's level of that size (144², then 288²) → 3 × 3 convolution → GroupNorm (8 groups, eps 1e-5) →
//! ReLU; last, every row of a `[r, 256]` matrix (with its bias) is a 1 × 1 projection of the decoder's output, an image
//! `[r, 288, 288]`. The detector passes its 200 mask embeddings with the instance projection folded in, so these are the
//! per-query mask logits. ~0.9–1.4 s per prompt on the CPU through candle, most of it the 288² convolution.
//!
//! Images are pixel rows (`[pixels, channels]`), as in the necks (`sam3/neck.rs`): the convolutions are the necks'
//! implicit GEMM, the projection the backbone's matrix product (`[pixels, 256] · [r, 256]ᵀ`, then `to_chw`), the
//! prompt attention the DETR encoder's kernels (`sam3/detr.rs`). `wgsl/sam3_pixel.wgsl` adds the up-sampling, which
//! adds the channels-first neck level as uploaded, and GroupNorm + ReLU: two-pass statistics per chunk of 256 pixels,
//! merged per group (the parallel variance formula), then applied in place.
//!
//! Loaded on the first prompt, like the encoder: kernels, 6 MB of weights, ~280 MB of work buffers. A prompt uploads
//! the memory, the prompt's tokens and padding bias, both neck levels (106 MB; they stay on the CPU with the image's
//! features, see `docs/gpu-pipeline.md`) and the rows, submits once and reads `r · 288²` numbers back. The dispatches
//! are recorded once per row count.

use std::sync::{Mutex, OnceLock, PoisonError};

use lightcraft_segment::WeightReader;

use super::detr::{self, Operand};
use super::neck::{self, conv_weights, scoped};
use super::{Dev, Graph, Precision, Recorded, buffer, pipeline, u32_of, upload};

#[cfg(test)]
mod tests;

/// Where the decoder's weights are in the checkpoint.
const PREFIX: &str = "detector_model.mask_decoder";
/// The memory's side (72 × 72 tokens), the width of everything, the prompt's tokens.
const GRID: usize = 72;
const WIDTH: usize = 256;
const TEXT: usize = 32;
const HEAD_DIM: usize = 32;
/// The decoder's stages (each doubles the side): 144², 288².
const STAGES: usize = 2;
/// The output's side.
const SIDE: usize = GRID << STAGES;
/// GroupNorm's groups and eps (`candle_nn::group_norm(8, 256, 1e-5)`), and PyTorch's layer norm eps.
const GROUPS: usize = 8;
const EPS: f32 = 1e-5;
/// Pixels per workgroup of the GroupNorm statistics.
const CHUNK: usize = 256;

struct Kernels {
    upsample_add: wgpu::ComputePipeline,
    group_stats: wgpu::ComputePipeline,
    group_merge: wgpu::ComputePipeline,
    group_norm_relu: wgpu::ComputePipeline,
}

static KERNELS: OnceLock<Result<Kernels, String>> = OnceLock::new();

/// The decoder's own kernels, compiled on first use.
fn kernels(dev: &'static Dev) -> Result<&'static Kernels, String> {
    KERNELS
        .get_or_init(|| {
            let validation = dev.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let source = include_str!("../wgsl/sam3_pixel.wgsl");
            let pipe = |entry: &str| pipeline(&dev.device, &dev.pipeline_layout, "sam3 pixel", source, entry);
            let k = Kernels {
                upsample_add: pipe("upsample_add"),
                group_stats: pipe("group_stats"),
                group_merge: pipe("group_merge"),
                group_norm_relu: pipe("group_norm_relu"),
            };
            match pollster::block_on(validation.pop()) {
                Some(e) => Err(format!("{}: the SAM 3 mask decoder kernels did not compile ({e})", dev.info.name)),
                None => Ok(k),
            }
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Floats of the GroupNorm statistics of chunks for an image of `pixels`, `groups` groups.
fn partials_len(pixels: usize, groups: usize) -> usize {
    pixels.div_ceil(CHUNK) * groups * 2
}

impl Graph {
    /// Nearest up-sampling of the rows `src` (`[from², channels]`) to `to × to`, plus the channels-first image `skip`
    /// (`[channels, to, to]`), into the rows `dst` (`[to², channels]`).
    fn upsample_add(
        &mut self,
        k: &'static Kernels,
        [src, skip, dst]: [&wgpu::Buffer; 3],
        channels: usize,
        (from, to): (usize, usize),
    ) -> Result<(), String> {
        if from == 0 || to < from || channels == 0 {
            return Err("an up-sampling the SAM 3 kernel cannot do".into());
        }
        let count = channels.checked_mul(to * to).ok_or("an image too large")?;
        let (g, row) = self.flat(count.div_ceil(256))?;
        let params = [u32_of(channels)?, u32_of(to)?, u32_of(from)?, 0, 0, 0, u32_of(count)?, row];
        self.push(&k.upsample_add, params, [src, skip, &self.dev.dummy, dst], g);
        Ok(())
    }

    /// GroupNorm (`groups` groups of channels, eps) and ReLU in place on the rows `x` (`[pixels, channels]`, channels
    /// ≤ 256), with the per-channel `gamma` and `beta`; `partials` (at least [`partials_len`] floats) and `stats`
    /// (2 · groups) hold the statistics on the way.
    #[allow(clippy::too_many_arguments)]
    fn group_norm_relu(
        &mut self,
        k: &'static Kernels,
        [x, gamma, beta]: [&wgpu::Buffer; 3],
        [partials, stats]: [&wgpu::Buffer; 2],
        (pixels, channels, groups): (usize, usize, usize),
        eps: f32,
    ) -> Result<(), String> {
        if pixels == 0 || channels == 0 || channels > 256 || groups == 0 || !channels.is_multiple_of(groups) {
            return Err("a GroupNorm the SAM 3 kernel cannot do".into());
        }
        let dummy = &self.dev.dummy;
        let chunks = pixels.div_ceil(CHUNK);
        let (g, row) = self.flat(chunks)?;
        let params = [u32_of(channels)?, u32_of(pixels)?, u32_of(CHUNK)?, u32_of(groups)?, 0, 0, 0, row];
        self.push(&k.group_stats, params, [x, dummy, dummy, partials], g);
        let params = [u32_of(chunks)?, u32_of(pixels)?, u32_of(CHUNK)?, u32_of(groups)?, eps.to_bits(), u32_of(channels / groups)?, 0, 0];
        self.push(&k.group_merge, params, [partials, dummy, dummy, stats], [u32_of(groups)?, 1, 1]);
        let count = pixels.checked_mul(channels).ok_or("an image too large")?;
        let (g, row) = self.flat(count.div_ceil(256))?;
        self.push(&k.group_norm_relu, [u32_of(channels)?, 0, 0, u32_of(groups)?, 0, 0, u32_of(count)?, row], [stats, gamma, beta, x], g);
        Ok(())
    }
}

/// The decoder's weights on the device: (weight, bias) pairs, the prompt attention's k | v concatenated, the
/// convolutions as rows `[cout, 3, 3, cin]`.
struct Weights {
    ln: [wgpu::Buffer; 2],
    q: [wgpu::Buffer; 2],
    kv: [wgpu::Buffer; 2],
    o: [wgpu::Buffer; 2],
    convs: Vec<[wgpu::Buffer; 2]>,
    norms: Vec<[wgpu::Buffer; 2]>,
}

/// Reads the decoder's tensors, checking their shapes.
struct Loader<'a> {
    weights: &'a WeightReader,
    bytes: u64,
}

impl Loader<'_> {
    /// The tensors `names` (each of `shape`) concatenated along the first axis.
    fn read(&mut self, names: &[String], shape: &[usize]) -> Result<Vec<f32>, String> {
        let mut all = Vec::with_capacity(names.len() * shape.iter().product::<usize>());
        for name in names {
            let full = format!("{PREFIX}.{name}");
            let (got, data) = self.weights.read(&full).map_err(|e| format!("{full}: {e}"))?;
            if got != shape {
                return Err(format!("{full}: shape {got:?}, expected {shape:?}"));
            }
            all.extend(data);
        }
        self.bytes += all.len() as u64 * 4;
        Ok(all)
    }

    /// A pair of `{name}.weight` (`[n, k]`, several concatenated along n) and `{name}.bias` (or a norm's scale and shift
    /// with `k` 0), uploaded.
    fn pair(&mut self, dev: &Dev, names: &[String], n: usize, k: usize) -> Result<[wgpu::Buffer; 2], String> {
        let w: Vec<String> = names.iter().map(|n| format!("{n}.weight")).collect();
        let b: Vec<String> = names.iter().map(|n| format!("{n}.bias")).collect();
        let shape: &[usize] = if k == 0 { &[n] } else { &[n, k] };
        let label = names.first().map_or("weight", String::as_str);
        Ok([upload(dev, label, &self.read(&w, shape)?), upload(dev, label, &self.read(&b, &[n])?)])
    }

    fn load(&mut self, dev: &Dev) -> Result<Weights, String> {
        let a = "prompt_cross_attn";
        let proj = |ps: &[&str]| -> Vec<String> { ps.iter().map(|p| format!("{a}.{p}_proj")).collect() };
        let mut convs = Vec::with_capacity(STAGES);
        let mut norms = Vec::with_capacity(STAGES);
        for i in 0..STAGES {
            let name = format!("pixel_decoder.conv_layers.{i}");
            let w = self.read(&[format!("{name}.weight")], &[WIDTH, WIDTH, 3, 3])?;
            let b = self.read(&[format!("{name}.bias")], &[WIDTH])?;
            convs.push(conv_weights(dev, &name, &w, &b, (WIDTH, WIDTH, 3)));
            norms.push(self.pair(dev, &[format!("pixel_decoder.norms.{i}")], WIDTH, 0)?);
        }
        Ok(Weights {
            ln: self.pair(dev, &["prompt_cross_attn_norm".to_string()], WIDTH, 0)?,
            q: self.pair(dev, &proj(&["q"]), WIDTH, WIDTH)?,
            kv: self.pair(dev, &proj(&["k", "v"]), WIDTH, WIDTH)?,
            o: self.pair(dev, &proj(&["o"]), WIDTH, WIDTH)?,
            convs,
            norms,
        })
    }
}

/// The buffers a prompt writes and reads, and its dispatches (recorded for one row count).
struct Work {
    /// The memory in, then the memory after the prompt attention.
    x: wgpu::Buffer,
    text: wgpu::Buffer,
    /// The prompt attention's k | v of the prompt (`[32, 512]`), then its padding bias (32 floats).
    text_kv: wgpu::Buffer,
    /// The neck's levels as uploaded (`[256, 144, 144]`, `[256, 288, 288]`); the projection's channels-first result
    /// ends in the second.
    skip1: wgpu::Buffer,
    skip0: wgpu::Buffer,
    /// Two 288² images the steps alternate between (the prompt attention's smaller ones too).
    a: wgpu::Buffer,
    b: wgpu::Buffer,
    rows: wgpu::Buffer,
    row_bias: wgpu::Buffer,
    partials: wgpu::Buffer,
    stats: wgpu::Buffer,
    staging: wgpu::Buffer,
    recorded: Option<(usize, Recorded)>,
}

impl Work {
    fn new(dev: &Dev) -> Work {
        let image = |label: &str| buffer(dev, label, SIDE * SIDE * WIDTH);
        Work {
            x: buffer(dev, "mask memory", GRID * GRID * WIDTH),
            text: buffer(dev, "mask prompt", TEXT * WIDTH),
            text_kv: buffer(dev, "mask prompt k|v", TEXT * 2 * WIDTH + TEXT),
            skip1: buffer(dev, "mask level 1", WIDTH * (SIDE / 2) * (SIDE / 2)),
            skip0: image("mask level 0"),
            a: image("mask ping"),
            b: image("mask pong"),
            rows: buffer(dev, "mask rows", WIDTH * WIDTH),
            row_bias: buffer(dev, "mask row bias", WIDTH),
            partials: buffer(dev, "mask norm partials", partials_len(SIDE * SIDE, GROUPS)),
            stats: buffer(dev, "mask norm stats", 2 * GROUPS),
            staging: dev.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mask read-back"),
                size: (SIDE * SIDE * WIDTH * 4) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            recorded: None,
        }
    }

    /// Bytes on the device: the read-back buffer is host memory on discrete GPUs and not counted.
    fn bytes() -> u64 {
        let floats = GRID * GRID * WIDTH
            + TEXT * (3 * WIDTH + 1)
            + WIDTH * (SIDE / 2) * (SIDE / 2)
            + 3 * SIDE * SIDE * WIDTH
            + WIDTH * (WIDTH + 1)
            + partials_len(SIDE * SIDE, GROUPS)
            + 2 * GROUPS;
        (floats * 4) as u64
    }
}

/// The dispatches of a prompt with `r` rows: everything after the uploads, the result in `work.skip0`.
fn record(dev: &'static Dev, w: &Weights, work: &Work, r: usize) -> Result<Recorded, String> {
    let (kd, kn, k) = (detr::kernels(dev)?, neck::kernels(dev)?, kernels(dev)?);
    let mut g = Graph::new(dev);
    let tokens = GRID * GRID;
    // the memory attends to the prompt (padding masked): x += o(attention(q(ln(x)), k | v(text)))
    g.layernorm_torch([&work.x, &w.ln[0], &w.ln[1], &work.a], tokens, WIDTH)?;
    g.matmul([&work.a, &w.q[0], &w.q[1], &work.b], (tokens, WIDTH, WIDTH), 1, false, false, Precision::F32)?;
    g.matmul([&work.text, &w.kv[0], &w.kv[1], &work.text_kv], (TEXT, 2 * WIDTH, WIDTH), 1, false, false, Precision::F32)?;
    let q = Operand { buffer: &work.b, stride: WIDTH, offset: 0 };
    let (tk, tv) =
        (Operand { buffer: &work.text_kv, stride: 2 * WIDTH, offset: 0 }, Operand { buffer: &work.text_kv, stride: 2 * WIDTH, offset: WIDTH });
    g.attention_detr(kd, [q, tk, tv], Some(TEXT * 2 * WIDTH), &work.a, (tokens, TEXT, WIDTH / HEAD_DIM))?;
    g.matmul([&work.a, &w.o[0], &w.o[1], &work.x], (tokens, WIDTH, WIDTH), 1, false, true, Precision::F32)?;
    // the pixel decoder: up-sample + the neck's level → 3 × 3 convolution → GroupNorm → ReLU, at 144² and at 288²
    let mut cur = &work.x;
    let mut side = GRID;
    for (i, skip) in [&work.skip1, &work.skip0].into_iter().enumerate() {
        let (Some(conv), Some(norm)) = (w.convs.get(i), w.norms.get(i)) else { return Err("a pixel decoder stage without weights".into()) };
        g.upsample_add(k, [cur, skip, &work.a], WIDTH, (side, 2 * side))?;
        side *= 2;
        g.conv3(kn, [&work.a, &conv[0], &conv[1], &work.b], side, (WIDTH, WIDTH))?;
        g.group_norm_relu(k, [&work.b, &norm[0], &norm[1]], [&work.partials, &work.stats], (side * side, WIDTH, GROUPS), EPS)?;
        cur = &work.b;
    }
    // each row a 1 × 1 projection of the decoder's output: [pixels, r], then channels-first into the spent level 0
    g.matmul([&work.b, &work.rows, &work.row_bias, &work.a], (side * side, r, WIDTH), 1, false, false, Precision::F32)?;
    g.channels_first(&work.a, &work.skip0, r, side, side)?;
    Ok(g.record())
}

/// The decoder with its weights on the device.
struct Built {
    weights: Weights,
    /// One prompt at a time (they share the work buffers).
    work: Mutex<Work>,
    weight_bytes: u64,
}

fn build(dev: &'static Dev, weights: &WeightReader) -> Result<Built, String> {
    // every kernel compiles before anything is uploaded
    detr::kernels(dev)?;
    neck::kernels(dev)?;
    kernels(dev)?;
    scoped(dev, "the SAM 3 mask decoder", || {
        let mut ld = Loader { weights, bytes: 0 };
        let w = ld.load(dev)?;
        Ok(Built { weights: w, work: Mutex::new(Work::new(dev)), weight_bytes: ld.bytes })
    })
}

/// The mask decoder, built on first use from the checkpoint the backbone came from.
pub(super) struct MaskDecoder {
    weights: WeightReader,
    built: OnceLock<Result<Built, String>>,
}

impl MaskDecoder {
    pub(super) fn new(weights: WeightReader) -> Self {
        Self { weights, built: OnceLock::new() }
    }

    /// Bytes of device memory the weights and the work buffers take, once built.
    pub(super) fn bytes(&self) -> Option<(u64, u64)> {
        self.built.get().and_then(|b| b.as_ref().ok()).map(|b| (b.weight_bytes, Work::bytes()))
    }

    /// `[r, 288, 288]` for the encoder memory `[5184, 256]`, the prompt's tokens `[32, 256]` (`valid`: its real ones),
    /// the detector neck's levels `[256, 144, 144]` and `[256, 288, 288]` (channels-first) and the rows `[r, 256]`
    /// with their bias `[r]` (1 ≤ r ≤ 256).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run(
        &self,
        dev: &'static Dev,
        memory: &[f32],
        text: &[f32],
        valid: &[bool],
        [level1, level0]: [&[f32]; 2],
        rows: &[f32],
        row_bias: &[f32],
    ) -> Result<Vec<f32>, String> {
        let r = row_bias.len();
        let (l1, l0) = (WIDTH * (SIDE / 2) * (SIDE / 2), WIDTH * SIDE * SIDE);
        if memory.len() != GRID * GRID * WIDTH || text.len() != TEXT * WIDTH || valid.len() != TEXT || level1.len() != l1 || level0.len() != l0 {
            return Err(format!(
                "{} memory, {} prompt values, {} prompt tokens and levels of {} and {} values, expected {GRID}² × {WIDTH}, {TEXT} × {WIDTH}, {TEXT}, {l1} and {l0}",
                memory.len(),
                text.len(),
                valid.len(),
                level1.len(),
                level0.len()
            ));
        }
        if r == 0 || r > WIDTH || rows.len() != r * WIDTH {
            return Err(format!("{} row values and {r} biases, expected r × {WIDTH} and r (1 ≤ r ≤ {WIDTH})", rows.len()));
        }
        let b = self.built.get_or_init(|| build(dev, &self.weights)).as_ref().map_err(Clone::clone)?;
        let bias: Vec<f32> = valid.iter().map(|&ok| if ok { 0.0 } else { f32::MIN }).collect();
        let mut work = b.work.lock().unwrap_or_else(PoisonError::into_inner);
        if work.recorded.as_ref().is_none_or(|(n, _)| *n != r) {
            let recorded = scoped(dev, "the SAM 3 mask decoder", || record(dev, &b.weights, &work, r))?;
            work.recorded = Some((r, recorded));
        }
        let q = &dev.queue;
        q.write_buffer(&work.x, 0, bytemuck::cast_slice(memory));
        q.write_buffer(&work.text, 0, bytemuck::cast_slice(text));
        q.write_buffer(&work.text_kv, (TEXT * 2 * WIDTH * 4) as u64, bytemuck::cast_slice(&bias));
        q.write_buffer(&work.skip1, 0, bytemuck::cast_slice(level1));
        q.write_buffer(&work.skip0, 0, bytemuck::cast_slice(level0));
        q.write_buffer(&work.rows, 0, bytemuck::cast_slice(rows));
        q.write_buffer(&work.row_bias, 0, bytemuck::cast_slice(row_bias));
        let Some((_, recorded)) = &work.recorded else { return Err("the mask decoder was not recorded".into()) };
        let out = recorded.run("the SAM 3 mask decoder", dev, &work.skip0, &work.staging, r * SIDE * SIDE)?;
        if !out.iter().fold(true, |ok, v| ok & v.is_finite()) {
            return Err("the GPU mask decoder produced non-finite values".into());
        }
        Ok(out)
    }
}
