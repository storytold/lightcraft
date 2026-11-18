//! The GPU renderer: the CPU pipeline's stages, evaluated on the device where a kernel exists and
//! on the CPU otherwise (per-stage hybrid). Stage results stay on the device and are cached per
//! view exactly like [`lcc_pipeline::StageCache`] (same keys, see [`lcc_pipeline::Plan`]).

use std::sync::{Arc, Mutex};

use lcc_color::spline::Lut1;
use lcc_geom::Orientation;
use lcc_pipeline::finish::{FinishParams, MASK_TERMS, local_curve_luts, mask_terms};
use lcc_pipeline::geometry::SampleMode;
use lcc_pipeline::local::plane_at;
use lcc_pipeline::nrc_joint::JointNoise;
use lcc_pipeline::{Plan, RenderRequest, Rendered, SourceInfo, local};
use lcc_raster::resample::Filter;
use lcc_raster::{Histogram, Plane, Rgb32f, Rgba8};

use crate::ctx::{Buf, FailKind, Gpu, fail, groups1, groups2};
use crate::params::{Present, finish_block};

/// Device-resident stages of one view, kept next to the CPU [`lcc_pipeline::StageCache`]
/// (as its extension). Holds the last two output sizes, like the CPU cache.
#[derive(Default)]
pub struct GpuStages {
    entries: Mutex<Vec<Entry>>,
    /// The uploaded source (a view renders one photo at several sizes).
    source: Mutex<Option<(Arc<Rgb32f>, Arc<Buf>)>>,
    /// Held by the render uploading `source` (never by a reader of it).
    uploading: Mutex<()>,
    /// The device generation the buffers belong to (0: none yet; see [`GpuStages::adopt_generation`]).
    generation: std::sync::atomic::AtomicU64,
}

const CAPACITY: usize = 2;

#[derive(Clone)]
struct Entry {
    src: Arc<Rgb32f>,
    geo: u64,
    sampled: Arc<Buf>,
    lin: Option<(u64, Arc<Buf>)>,
    /// The joint colour NR's noise model of the whole frame, keyed by `Plan::noise_key`.
    colour_noise: Option<(u64, JointNoise)>,
    planes: Planes,
}

/// Spatial planes of one linear image (`key` = its `lin_key`), each tagged with its radius.
#[derive(Clone, Default)]
struct Planes {
    key: u64,
    log_l: Option<Arc<Buf>>,
    base: Option<(local::BaseKey, Arc<Buf>)>,
    clarity: Option<(u32, Arc<Buf>)>,
    texture: Option<(u32, Arc<Buf>)>,
    sharpen: Option<(u32, Arc<Buf>)>,
    dark: Option<(u64, Arc<Buf>, f32)>,
    chroma: Option<(u32, Arc<Buf>)>,
    /// The masks' alpha planes (and the chromaticity after them): see [`MaskBufs`].
    masks: Option<MaskBufs>,
}

/// The masks' alpha buffer of one image, kept for the next render: keyed by
/// [`lcc_pipeline::masks::alpha_key`], the chromaticity plane appended to it, and the
/// mattes it was evaluated with (by identity). A slider drag that does not touch the masks reuses
/// it: AI masks are evaluated on the CPU, and re-evaluating them every tick (a readback of the
/// image, the matte resampled, an upload) was most of a drag's render time.
#[derive(Clone)]
struct MaskBufs {
    key: u64,
    chroma: Option<u32>,
    mattes: Option<Arc<lcc_pipeline::masks::Mattes>>,
    alpha: Arc<MaskAlpha>,
}

/// The masks' alpha planes. Back to back in one buffer (then the blurred chromaticity) when that
/// fits the device's storage-buffer limit; otherwise one buffer per mask, which the finish kernel
/// reads band by band (a 45 MP export with 13 masks needs 2,220 MiB, over the 2,047 MiB limit,
/// and went to the CPU at a sixth of the speed). Both are evaluated by the same kernels, so they
/// hold the same values.
enum MaskAlpha {
    Joined(Buf),
    Split { planes: Vec<Buf>, chroma: Option<Arc<Buf>> },
}

impl MaskAlpha {
    /// 32-bit words held.
    fn words(&self) -> usize {
        match self {
            MaskAlpha::Joined(b) => b.len,
            MaskAlpha::Split { planes, .. } => planes.iter().map(|p| p.len).sum(),
        }
    }

    /// A copy of mask `mi`'s plane of `n` pixels (recorded); `None` past the last mask.
    fn plane(&self, cx: &mut Cx<'_>, mi: usize, n: usize) -> Option<Buf> {
        match self {
            MaskAlpha::Joined(b) => (mi.checked_add(1)?.checked_mul(n)? <= b.len).then(|| cx.slice(b, mi * n, n)),
            MaskAlpha::Split { planes, .. } => planes.get(mi).map(|p| cx.slice(p, 0, n.min(p.len))),
        }
    }
}

impl MaskBufs {
    fn fits(&self, key: u64, chroma: Option<u32>, mattes: Option<&Arc<lcc_pipeline::masks::Mattes>>) -> bool {
        self.key == key
            && self.chroma == chroma
            && match (&self.mattes, mattes) {
                (None, None) => true,
                (Some(a), Some(b)) => lcc_pipeline::masks::Mattes::same(a, b),
                _ => false,
            }
    }
}

impl GpuStages {
    /// Use these stages on the device of `generation`: what was cached on another one (a device
    /// that failed and was replaced) is dropped first.
    pub(crate) fn adopt_generation(&self, generation: u64) {
        use std::sync::atomic::Ordering;
        if self.generation.swap(generation, Ordering::Relaxed) != generation {
            self.clear();
        }
    }

    fn get(&self, src: &Arc<Rgb32f>, geo: u64) -> Option<Entry> {
        self.lock().iter().find(|e| e.geo == geo && Arc::ptr_eq(&e.src, src)).cloned()
    }

    fn put(&self, e: Entry) {
        let mut v = self.lock();
        v.retain(|o| !(o.geo == e.geo && Arc::ptr_eq(&o.src, &e.src)));
        v.push(e);
        while v.len() > CAPACITY {
            v.remove(0);
        }
    }

    /// The uploaded copy of `src`, uploading it (`upload`) when the view has another photo's or none.
    /// The upload (a conversion and a copy of up to [`RETAIN_SOURCE_BYTES`]) runs with the lock
    /// released: the UI thread reads these stages' size every frame ([`Self::bytes`]), so it must
    /// never wait for an upload.
    fn source(&self, src: &Arc<Rgb32f>, upload: impl FnOnce() -> Buf) -> Arc<Buf> {
        // (one upload at a time per view; only its renders wait here)
        let _one = self.uploading.lock().unwrap_or_else(|e| e.into_inner());
        let old = {
            let mut g = self.source.lock().unwrap_or_else(|e| e.into_inner());
            match &*g {
                Some((s, b)) if Arc::ptr_eq(s, src) => return b.clone(),
                _ => g.take(), // free the previous photo's buffer first
            }
        };
        drop(old);
        let b = Arc::new(upload());
        *self.source.lock().unwrap_or_else(|e| e.into_inner()) = Some((src.clone(), b.clone()));
        b
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Number of cached output sizes.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Device bytes held (each buffer counted once).
    pub fn bytes(&self) -> usize {
        let mut seen: Vec<*const Buf> = Vec::new();
        let mut seen_masks: Vec<*const MaskAlpha> = Vec::new();
        let mut mask_bytes = 0;
        let mut total = 0;
        let mut add = |b: &Arc<Buf>| {
            let p = Arc::as_ptr(b);
            if !seen.contains(&p) {
                seen.push(p);
                total += b.len * 4;
            }
        };
        for e in self.lock().iter() {
            add(&e.sampled);
            if let Some((_, b)) = &e.lin {
                add(b);
            }
            let p = &e.planes;
            p.log_l.iter().for_each(&mut add);
            p.base.iter().for_each(|(_, b)| add(b));
            p.clarity.iter().for_each(|(_, b)| add(b));
            p.texture.iter().for_each(|(_, b)| add(b));
            p.sharpen.iter().for_each(|(_, b)| add(b));
            p.dark.iter().for_each(|(_, b, _)| add(b));
            p.chroma.iter().for_each(|(_, b)| add(b));
            if let Some(m) = &p.masks {
                let a = Arc::as_ptr(&m.alpha);
                if !seen_masks.contains(&a) {
                    seen_masks.push(a);
                    mask_bytes += m.alpha.words() * 4;
                }
            }
        }
        if let Some((_, b)) = &*self.source.lock().unwrap_or_else(|e| e.into_inner()) {
            add(b);
        }
        total + mask_bytes
    }

    /// Drop everything (e.g. when memory is needed).
    pub fn clear(&self) {
        self.lock().clear();
        *self.source.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A render in progress: the device and the commands recorded so far. Reading a result back
/// submits everything recorded before it.
pub(crate) struct Cx<'a> {
    pub gpu: &'a Gpu,
    enc: Option<wgpu::CommandEncoder>,
    /// Kernel invocations recorded since the last submit.
    pending: u64,
    /// The last submission (a batch render waits for it before the next: see `gate::throttle`).
    last: Option<wgpu::SubmissionIndex>,
}

/// Kernel invocations recorded before a render submits them (under one full-size pass over a
/// 24 MP export, several passes over a preview): no single submission runs long.
const FLUSH_INVOCATIONS: u64 = 16 << 20;

/// Longest [`Cx::pace`] waits for a step before it gives the GPU up.
const PACE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Pixels per dispatch of the per-pixel stage (the heaviest kernel).
const BAND_PIXELS: usize = 4 << 20;

impl<'a> Cx<'a> {
    pub fn new(gpu: &'a Gpu) -> Cx<'a> {
        Cx { gpu, enc: Some(gpu.encoder()), pending: 0, last: None }
    }

    /// Record kernel `name` (see [`Gpu::run`]).
    pub fn run(&mut self, name: &str, p: &[u32], bufs: &[Option<&Buf>], groups: [u32; 3]) {
        let enc = self.enc.get_or_insert_with(|| self.gpu.encoder());
        self.gpu.run(enc, name, p, bufs, groups);
        // every kernel runs 256 invocations per workgroup
        self.pending += groups.iter().map(|g| *g as u64).product::<u64>() * 256;
        if self.pending >= FLUSH_INVOCATIONS {
            self.flush();
        }
    }

    /// Submit what is recorded and read back `len` values of `b`.
    pub fn read<T: bytemuck::Pod>(&mut self, b: &Buf, len: usize) -> Vec<T> {
        self.pending = 0;
        let enc = self.enc.take().unwrap_or_else(|| self.gpu.encoder());
        self.gpu.finish_and_read(enc, b, len)
    }

    /// [`Self::read`] of consecutive parts of `b`, each into its own vector (one readback).
    pub fn read_parts<T: bytemuck::Pod>(&mut self, b: &Buf, lens: &[usize]) -> Vec<Vec<T>> {
        self.pending = 0;
        let enc = self.enc.take().unwrap_or_else(|| self.gpu.encoder());
        self.gpu.finish_and_read_parts(enc, b, lens)
    }

    /// Submit what is recorded so far without waiting. Renders submit stage by stage and every
    /// [`FLUSH_INVOCATIONS`]: long submissions are what GPU watchdogs reset (Windows TDR, the
    /// i915 hang check; issue #78's iGPU spent seconds on an export recorded as one submission),
    /// and buffers dropped by earlier work become reusable by later work (lower peak memory).
    pub fn flush(&mut self) {
        self.pending = 0;
        if let Some(enc) = self.enc.take() {
            // an export keeps one submission queued at a time: the window's renders slot in between
            if !crate::gate::throttle(self.gpu, self.last.take()) {
                fail(FailKind::Busy, "the GPU did not finish an earlier step in time (busy)".into());
                return;
            }
            self.last = Some(self.gpu.submit([enc.finish()]));
        }
    }

    /// Submit what is recorded and wait for the submission before it: work recorded in steps (an
    /// Auto Mask stroke each) then keeps at most one step queued ahead of the window's frames,
    /// which slot in between. (A batch render's [`Self::flush`] waits already.)
    pub fn pace(&mut self) {
        let before = self.last.clone();
        self.flush();
        if crate::gate::is_batch() || crate::ctx::failed() {
            return;
        }
        if let Some(prev) = before
            && self.gpu.poll(wgpu::PollType::Wait { submission_index: Some(prev), timeout: Some(PACE_TIMEOUT) }).is_err()
        {
            fail(FailKind::Busy, "the GPU did not finish an earlier step in time (busy)".into());
        }
    }

    /// Submit what is recorded and wait for it (profiling only: stage timings).
    pub fn sync(&mut self) {
        self.pending = 0;
        if let Some(enc) = self.enc.take() {
            self.gpu.submit([enc.finish()]);
        }
        let _ = self.gpu.poll(wgpu::PollType::wait_indefinitely());
    }

    /// A zero-filled buffer (the clear is recorded).
    pub fn zeroed(&mut self, len: usize) -> Buf {
        let out = self.gpu.buffer(len);
        let enc = self.enc.get_or_insert_with(|| self.gpu.encoder());
        enc.clear_buffer(out.raw(), 0, None);
        out
    }

    /// A copy of `b` (recorded).
    pub fn copy(&mut self, b: &Buf) -> Buf {
        let out = self.gpu.buffer(b.len);
        let enc = self.enc.get_or_insert_with(|| self.gpu.encoder());
        enc.copy_buffer_to_buffer(b.raw(), 0, out.raw(), 0, (b.len * 4) as u64);
        out
    }

    /// `len` values of `b` from `offset` (both in 32-bit words), as a new buffer (recorded).
    pub fn slice(&mut self, b: &Buf, offset: usize, len: usize) -> Buf {
        let out = self.gpu.buffer(len);
        let enc = self.enc.get_or_insert_with(|| self.gpu.encoder());
        enc.copy_buffer_to_buffer(b.raw(), (offset * 4) as u64, out.raw(), 0, (len * 4) as u64);
        out
    }

    /// Copy `len` words of `src` from `from` into `dst` at `to` (recorded). A range past either
    /// buffer fails the render over to the CPU instead of raising a validation error.
    pub fn copy_range(&mut self, src: &Buf, from: usize, dst: &Buf, to: usize, len: usize) {
        if from.checked_add(len).is_none_or(|e| e > src.len) || to.checked_add(len).is_none_or(|e| e > dst.len) {
            fail(FailKind::Fatal, format!("copy of {len} values at {from} → {to} past a buffer of {} / {}", src.len, dst.len));
            return;
        }
        if len == 0 {
            return;
        }
        let enc = self.enc.get_or_insert_with(|| self.gpu.encoder());
        enc.copy_buffer_to_buffer(src.raw(), (from * 4) as u64, dst.raw(), (to * 4) as u64, (len * 4) as u64);
    }

    /// Copy all of `src` into `dst` at `offset` (32-bit words; recorded).
    pub fn copy_into(&mut self, src: &Buf, dst: &Buf, offset: usize) {
        let enc = self.enc.get_or_insert_with(|| self.gpu.encoder());
        enc.copy_buffer_to_buffer(src.raw(), 0, dst.raw(), (offset * 4) as u64, (src.len * 4) as u64);
    }

    /// Copy `w × h` texels from `b` (rows of `stride` 32-bit texels, a multiple of 64) into
    /// `texture` (recorded).
    pub fn copy_to_texture(&mut self, b: &Buf, stride: usize, texture: &wgpu::Texture, w: usize, h: usize) {
        let enc = self.enc.get_or_insert_with(|| self.gpu.encoder());
        enc.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: b.raw(),
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some((stride * 4) as u32), rows_per_image: Some(h as u32) },
            },
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        );
    }

    pub fn read_rgb(&mut self, b: &Buf, w: usize, h: usize) -> Rgb32f {
        Rgb32f { width: w, height: h, data: self.read(b, w * h * 3) }
    }

    pub fn read_plane(&mut self, b: &Buf, w: usize, h: usize) -> Plane {
        Plane { width: w, height: h, data: self.read(b, w * h) }
    }
}

pub(crate) fn rgb_words(img: &Rgb32f) -> &[f32] {
    bytemuck::cast_slice(&img.data)
}

// ---------------------------------------------------------------------------------------------
// Filters (twins of `lcc_raster::blur` / `resample` and `lcc_pipeline::local`)

/// Rows each `box_v` thread slides its running sum over (at least; longer for large radii, so the
/// priming sum stays a small share of the work).
const CHUNK: usize = 16;

/// One box pass of radius `r` (clamped edges) over a `w × h` image of `nc` interleaved channels,
/// `src → dst`: horizontal or vertical.
fn box_pass(cx: &mut Cx<'_>, horizontal: bool, src: &Buf, dst: &Buf, (w, h): (usize, usize), nc: usize, r: usize) {
    let plan = box_plan(horizontal, r);
    let p = [w as u32, h as u32, nc as u32, r as u32, plan.chunk as u32];
    let groups = match plan.kernel {
        "box_h" => groups2(w, h, [64, 4]),
        "box_h_slide" => groups2(w.div_ceil(plan.chunk), h, [64, 4]),
        _ => groups2(w, h.div_ceil(plan.chunk), [64, 4]),
    };
    cx.run(plan.kernel, &p, &[Some(src), Some(dst)], groups);
}

/// Radius from which the horizontal box pass slides a running sum ([`box_plan`]).
const BOX_H_SLIDE_FROM: usize = 32;

/// How one box pass runs: its kernel, the run of pixels each sliding thread covers, and the source
/// reads it costs per output pixel.
#[derive(Clone, Copy, Debug)]
struct BoxPlan {
    kernel: &'static str,
    chunk: usize,
    #[cfg_attr(not(test), allow(dead_code))]
    reads_per_px: f32,
}

/// The box pass for radius `r`. Work per pixel stays bounded whatever the radius (a few reads), so a
/// large blur can't make one dispatch run long enough for a driver watchdog (Windows TDR, ~2 s) to reset
/// the GPU: small horizontal radii read their window per pixel (cache friendly), larger ones and every
/// vertical pass slide a running sum over a run at least as long as the window.
fn box_plan(horizontal: bool, r: usize) -> BoxPlan {
    let window = r.saturating_mul(2).saturating_add(1);
    if horizontal && r < BOX_H_SLIDE_FROM {
        return BoxPlan { kernel: "box_h", chunk: 1, reads_per_px: window as f32 };
    }
    let chunk = window.max(CHUNK);
    // priming (window reads) plus two reads per pixel of the run
    let reads_per_px = (window as f32 + 2.0 * chunk as f32) / chunk as f32;
    BoxPlan { kernel: if horizontal { "box_h_slide" } else { "box_v" }, chunk, reads_per_px }
}

/// Gaussian blur of a `w × h` image of `nc` interleaved channels: three box passes each way
/// (`lcc_raster::blur::gaussian`).
pub(crate) fn gaussian(cx: &mut Cx<'_>, src: &Buf, w: usize, h: usize, nc: usize, sigma: f32) -> Buf {
    let Some(radii) = lcc_raster::blur::radii_for(sigma, w, h) else {
        return cx.copy(src);
    };
    let passes: Vec<(bool, usize)> = radii.iter().map(|r| (true, *r)).chain(radii.iter().map(|r| (false, *r))).filter(|p| p.1 > 0).collect();
    if passes.is_empty() {
        return cx.copy(src);
    }
    // ping-pong between two buffers
    let mut bufs = [cx.gpu.buffer(w * h * nc), cx.gpu.buffer(w * h * nc)];
    for (i, (horizontal, r)) in passes.iter().enumerate() {
        let (out, prev) = (&bufs[i % 2], &bufs[(i + 1) % 2]);
        box_pass(cx, *horizontal, if i == 0 { src } else { prev }, out, (w, h), nc, *r);
    }
    let last = (passes.len() - 1) % 2;
    let [a, b] = std::mem::replace(&mut bufs, [cx.gpu.buffer(0), cx.gpu.buffer(0)]);
    if last == 0 { a } else { b }
}

/// Exact Gaussian (`lcc_raster::blur::gaussian_taps`) of a one-channel plane.
pub(crate) fn gaussian_fir(cx: &mut Cx<'_>, src: &Buf, w: usize, h: usize, sigma: f32) -> Buf {
    let taps = lcc_raster::blur::gauss_taps(sigma);
    let taps_buf = cx.gpu.upload(&taps);
    let r = (taps.len() / 2) as u32;
    let (tmp, out) = (cx.gpu.buffer(w * h), cx.gpu.buffer(w * h));
    let p = [w as u32, h as u32, r];
    cx.run("gauss_h", &p, &[Some(src), Some(&taps_buf), Some(&tmp)], groups2(w, h, [16, 16]));
    cx.run("gauss_v", &p, &[Some(&tmp), Some(&taps_buf), Some(&out)], groups2(w, h, [16, 16]));
    out
}

/// Resample taps of `lcc_raster::resample::resize`, packed for the `resize_*` kernels.
fn taps(src: usize, dst: usize, filter: Filter) -> Vec<u32> {
    let w = lcc_raster::resample::weights(src, dst, filter);
    let mut t = Vec::with_capacity(dst * 3);
    let mut weights = Vec::new();
    let base = dst * 3;
    for (lo, ws) in &w {
        t.extend_from_slice(&[*lo as u32, ws.len() as u32, (base + weights.len()) as u32]);
        weights.extend(ws.iter().map(|v| v.to_bits()));
    }
    t.extend(weights);
    t
}

/// `lcc_raster::resample::resize` of an `nc`-channel image.
pub(crate) fn resize(cx: &mut Cx<'_>, src: &Buf, (sw, sh): (usize, usize), (dw, dh): (usize, usize), nc: usize, filter: Filter) -> Buf {
    let (dw, dh) = (dw.max(1), dh.max(1));
    if (sw, sh) == (dw, dh) {
        return cx.copy(src);
    }
    let tx = cx.gpu.upload(&taps(sw, dw, filter));
    let tmp = cx.gpu.buffer(dw * sh * nc);
    cx.run("resize_h", &[sw as u32, sh as u32, dw as u32, nc as u32], &[Some(src), Some(&tx), Some(&tmp)], groups2(dw, sh, [16, 16]));
    let ty = cx.gpu.upload(&taps(sh, dh, filter));
    let out = cx.gpu.buffer(dw * dh * nc);
    cx.run("resize_v", &[dw as u32, sh as u32, dh as u32, nc as u32], &[Some(&tmp), Some(&ty), Some(&out)], groups2(dw, dh, [16, 16]));
    out
}

/// An element-wise kernel of the `map` module over `n` items.
pub(crate) fn map(cx: &mut Cx<'_>, k: &str, n: usize, extra: &[u32], ins: [Option<&Buf>; 3], out: &Buf) {
    let mut p = vec![n as u32];
    p.extend_from_slice(extra);
    cx.run(k, &p, &[ins[0], ins[1], ins[2], Some(out)], groups1(n));
}

/// Self-guided filter of a plane (`local::guided`).
fn guided(cx: &mut Cx<'_>, p: &Buf, w: usize, h: usize, sigma: f32, eps: f32) -> Buf {
    let ab = guided_coeffs(cx, p, w, h, sigma, eps);
    let q = cx.gpu.buffer(w * h);
    map(cx, "guided_apply", w * h, &[], [Some(p), Some(&ab), None], &q);
    q
}

/// Blurred (a, b) coefficients, interleaved.
fn guided_coeffs(cx: &mut Cx<'_>, p: &Buf, w: usize, h: usize, sigma: f32, eps: f32) -> Buf {
    let n = w * h;
    let pp = cx.gpu.buffer(2 * n);
    map(cx, "guided_pre", n, &[], [Some(p), None, None], &pp);
    let m = gaussian(cx, &pp, w, h, 2, sigma);
    let ab = cx.gpu.buffer(2 * n);
    map(cx, "guided_ab", n, &[eps.to_bits()], [Some(&m), None, None], &ab);
    gaussian(cx, &ab, w, h, 2, sigma)
}

/// Separable median of a plane (`local::median_hv`).
fn median_hv(cx: &mut Cx<'_>, p: &Buf, w: usize, h: usize, r: usize) -> Buf {
    if r == 0 || w * h == 0 {
        return cx.copy(p);
    }
    let pr = [w as u32, h as u32, 1, r.min(local::BASE_DETAIL_MAX) as u32];
    let tmp = cx.gpu.buffer(w * h);
    cx.run("median_h", &pr, &[Some(p), Some(&tmp)], groups2(w, h, [16, 16]));
    let out = cx.gpu.buffer(w * h);
    cx.run("median_v", &pr, &[Some(&tmp), Some(&out)], groups2(w, h, [16, 16]));
    out
}

/// Separable minimum of a plane (`local::min_hv`).
fn min_hv(cx: &mut Cx<'_>, p: Buf, w: usize, h: usize, r: usize) -> Buf {
    if r == 0 || w * h == 0 {
        return p;
    }
    let pr = [w as u32, h as u32, 1, r.min(local::DEHAZE_PATCH_MAX) as u32];
    let tmp = cx.gpu.buffer(w * h);
    cx.run("min_h", &pr, &[Some(&p), Some(&tmp)], groups2(w, h, [16, 16]));
    let out = cx.gpu.buffer(w * h);
    cx.run("min_v", &pr, &[Some(&tmp), Some(&out)], groups2(w, h, [16, 16]));
    out
}

/// The highlights/shadows base (`local::tone_base`; `detail` 0: V1, no median).
fn tone_base(cx: &mut Cx<'_>, l: &Buf, w: usize, h: usize, sigma: f32, detail: usize) -> Buf {
    if detail == 0 {
        return guided_fast(cx, l, w, h, sigma, local::BASE_EPS);
    }
    let m = median_hv(cx, l, w, h, detail);
    guided_fast(cx, &m, w, h, sigma, local::BASE_EPS)
}

/// Fast guided filter (`local::guided_fast`): coefficients on a subsampled grid.
fn guided_fast(cx: &mut Cx<'_>, p: &Buf, w: usize, h: usize, sigma: f32, eps: f32) -> Buf {
    let s = local::guided_fast_step(sigma);
    if s <= 1 {
        return guided(cx, p, w, h, sigma, eps);
    }
    let (lw, lh) = (w.div_ceil(s).max(1), h.div_ceil(s).max(1));
    let lo = resize(cx, p, (w, h), (lw, lh), 1, Filter::Box);
    let ab = guided_coeffs(cx, &lo, lw, lh, sigma / s as f32, eps);
    let up = resize(cx, &ab, (lw, lh), (w, h), 2, Filter::Bilinear);
    let q = cx.gpu.buffer(w * h);
    map(cx, "guided_apply", w * h, &[], [Some(p), Some(&up), None], &q);
    q
}

// ---------------------------------------------------------------------------------------------
// Geometry (`Frame::sample`)

/// Integer pixel map `(x, y) → (m0·x + m1·y + m2, m3·x + m4·y + m5)` from an oriented image back
/// to the source of `w × h` (`Image::oriented`: optional horizontal flip, then quarter turns).
pub(crate) fn orient_map(o: Orientation, w: usize, h: usize) -> [i32; 6] {
    let (flip, turns) = o.to_parts();
    let (w, h) = (w as i32, h as i32);
    let mut ops: Vec<fn(i32, i32) -> [i32; 6]> = Vec::new();
    if flip {
        ops.push(|w, _| [-1, 0, w - 1, 0, 1, 0]);
    }
    let cw: fn(i32, i32) -> [i32; 6] = |_, h| [0, 1, 0, -1, 0, h - 1];
    let half: fn(i32, i32) -> [i32; 6] = |w, h| [-1, 0, w - 1, 0, -1, h - 1];
    match turns {
        1 => ops.push(cw),
        2 => ops.push(half),
        3 => {
            ops.push(half);
            ops.push(cw);
        }
        _ => {}
    }
    // compose: T ← T ∘ op, tracking each op's input size
    let mut t = [1, 0, 0, 0, 1, 0];
    let (mut cw_, mut ch_) = (w, h);
    for op in ops {
        let o = op(cw_, ch_);
        t = [
            t[0] * o[0] + t[1] * o[3],
            t[0] * o[1] + t[1] * o[4],
            t[0] * o[2] + t[1] * o[5] + t[2],
            t[3] * o[0] + t[4] * o[3],
            t[3] * o[1] + t[4] * o[4],
            t[3] * o[2] + t[4] * o[5] + t[5],
        ];
        if o[0] == 0 {
            (cw_, ch_) = (ch_, cw_);
        }
    }
    t
}

fn affine_bits(a: &lcc_geom::Affine) -> [u32; 6] {
    a.0.map(|v| (v as f32).to_bits())
}

/// The `sample_warp` kernel's parameters (layout documented in `geom.wgsl`).
fn warp_params(wp: &lcc_pipeline::optics::Warp, base: (usize, usize), out: (usize, usize), o2t: &lcc_geom::Affine, sx: f64, sy: f64) -> Vec<u32> {
    let f = |v: f64| (v as f32).to_bits();
    let mut p = vec![base.0 as u32, base.1 as u32, out.0 as u32, out.1 as u32];
    p.extend(affine_bits(o2t));
    p.extend([f(sx), f(sy), f(wp.w), f(wp.h)]);
    let persp = wp.persp != lcc_geom::Homography::IDENTITY;
    p.push(persp as u32);
    p.extend(wp.persp_inv.0.map(f));
    p.push(f(wp.k1));
    p.extend(wp.ca.map(f));
    p.push(f(wp.lens_dist));
    let warp = wp.lens.and_then(|l| l.warp);
    p.push(warp.is_some() as u32);
    let w = warp.unwrap_or_default();
    p.extend(w.planes.iter().flatten().map(|v| f(*v)));
    p.extend([f(w.center.x), f(w.center.y), f(w.radius)]);
    p.extend([f(wp.vig_stops), f(wp.vig_power), f(wp.lens_vig)]);
    let vig = wp.lens.and_then(|l| l.vignette);
    p.push(vig.is_some() as u32);
    let v = vig.unwrap_or_default();
    p.extend(v.k.map(f));
    p.extend([f(v.center.x), f(v.center.y), f(v.radius)]);
    p.push(wp.per_channel() as u32);
    p.push(wp.has_gain() as u32);
    // the flat-field gain (`Warp::field`): on?, amount, nodes per side; its EV grid is the `field` binding
    let amount = wp.field_amount();
    p.extend([(amount != 0.0) as u32, amount.to_bits(), lcc_pipeline::vignetting_calibration::FIELD_NODES as u32]);
    debug_assert_eq!(p.len(), 68);
    p
}

/// The largest source (bytes on the device) a view's stages keep uploaded between renders.
const RETAIN_SOURCE_BYTES: usize = 96 << 20;

/// Big sources uploaded to the shared copy so far (diagnostics and tests).
static SOURCE_UPLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The one big source (the original a zoom window is cut from) kept on the device for all the
/// views of the photo: the Before and After windows and every pan share it, instead of each
/// view's stages holding (or re-uploading) 288 MB of a 24 MP photo. It does not count as a view's
/// stages; it is released with the buffer pool when the app idles.
static SHARED_SOURCE: std::sync::Mutex<Option<(std::sync::Weak<Rgb32f>, Arc<Buf>)>> = std::sync::Mutex::new(None);

/// The shared copy of `src` ([`SHARED_SOURCE`]), uploading it when it is another photo's. The
/// conversion and upload (288 MB for 24 MP, up to seconds on a busy card) run with the lock
/// released: the UI thread reads its size (`ui.inspect`) and frees it when idle, and waited for the
/// whole upload (1.5 s when a view first zoomed to 100 %).
fn shared_source(src: &Arc<Rgb32f>, generation: u64, upload: impl FnOnce() -> Buf) -> Arc<Buf> {
    // (one upload at a time: a second view of the photo waits for the first's copy instead of
    // making its own; only renders wait here, never a reader of the size)
    let _one = SHARED_UPLOAD.lock().unwrap_or_else(|e| e.into_inner());
    let old = {
        let mut g = SHARED_SOURCE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((weak, buf)) = &*g
            && weak.upgrade().is_some_and(|s| Arc::ptr_eq(&s, src))
            && buf.generation() == generation
        {
            return buf.clone();
        }
        // the previous photo's copy goes (on a render thread it is retired with the render's
        // other buffers, so for a moment both exist)
        g.take()
    };
    drop(old);
    SOURCE_UPLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let buf = Arc::new(upload());
    // an upload that failed (out of memory, over the limit) holds a placeholder: this render
    // fails, and no other render may be handed it as the photo
    if !crate::ctx::failed() {
        *SHARED_SOURCE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Arc::downgrade(src), buf.clone()));
    }
    buf
}

/// Held by the render uploading the shared source ([`shared_source`]), never by a reader.
static SHARED_UPLOAD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Free the shared big source (see [`shared_source`]).
pub(crate) fn release_shared_source() {
    *SHARED_SOURCE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Device bytes of the shared big source, 0 when there is none.
pub(crate) fn shared_source_bytes() -> usize {
    SHARED_SOURCE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, |(_, b)| b.len * 4)
}

pub(crate) fn source_uploads() -> u64 {
    SOURCE_UPLOADS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Resample the source into the output frame (`Frame::sample`). May return the source buffer
/// itself when the frame is the identity.
fn sample(cx: &mut Cx<'_>, src: &Arc<Rgb32f>, src_buf: Arc<Buf>, plan: &Plan<'_>) -> Arc<Buf> {
    let (w, h) = (plan.w, plan.h);
    let fr = &plan.frame;
    let sp = fr.sample_plan(src.width, src.height, w, h);
    let oriented = if fr.orient == Orientation::Normal {
        src_buf
    } else {
        let out = cx.gpu.buffer(sp.ow * sp.oh * 3);
        let mut p = vec![src.width as u32, src.height as u32, sp.ow as u32, sp.oh as u32];
        p.extend(orient_map(fr.orient, src.width, src.height).map(|v| v as u32));
        cx.run("orient", &p, &[Some(&src_buf), Some(&out), None, None], groups2(sp.ow, sp.oh, [16, 16]));
        Arc::new(out)
    };
    let (base, (bw, bh)) = match sp.prefilter {
        Some(d) => (Arc::new(resize(cx, &oriented, (sp.ow, sp.oh), d, 3, fr.downscale)), d),
        None => (oriented, (sp.ow, sp.oh)),
    };
    let mut affine = |xf: &lcc_geom::Affine| {
        let out = cx.gpu.buffer(w * h * 3);
        let mut p = vec![bw as u32, bh as u32, w as u32, h as u32];
        p.extend(affine_bits(xf));
        cx.run("sample_affine", &p, &[Some(&base), Some(&out), None, None], groups2(w, h, [16, 16]));
        Arc::new(out)
    };
    match (&sp.mode, fr.warp.as_ref()) {
        (SampleMode::Copy, _) => base,
        (SampleMode::Affine(xf), _) => affine(xf),
        // `sample_plan` plans a warp only when there is one; without it the same mapping is affine
        (SampleMode::Warp(o2t), None) => affine(&(lcc_geom::Affine::scale(sp.sx, sp.sy) * *o2t)),
        (SampleMode::Warp(o2t), Some(wp)) => {
            let out = cx.gpu.buffer(w * h * 3);
            let p = warp_params(wp, (bw, bh), (w, h), o2t, sp.sx, sp.sy);
            // f32 rounding can cross a source edge. Keep the reference f64
            // framing decision in a bit mask; color sampling stays on the GPU.
            let coverage = cx.gpu.upload(wp.coverage(o2t, w, h).words());
            // (64 × 64 values, uploaded with the render: a placeholder binding without a field)
            let field = wp.field.as_ref().filter(|f| f.amount != 0.0).map(|f| cx.gpu.upload(f.ev.as_slice()));
            cx.run("sample_warp", &p, &[Some(&base), Some(&out), Some(&coverage), field.as_ref()], groups2(w, h, [16, 16]));
            Arc::new(out)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The render

/// CPU copies made along the way (for stages that still run on the CPU).
#[derive(Default)]
struct Host {
    sampled: Option<Rgb32f>,
    lin: Option<Arc<Rgb32f>>,
    log_l: Option<Arc<Plane>>,
}

pub(crate) fn profiling() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| lcc_brand::env_is_set("PROFILE"))
}

/// A finished render on the window's device, ready to draw: an `Rgba8Unorm` texture (the 8-bit
/// output, sRGB-curve encoded, as egui's own textures) that never went through host memory.
#[derive(Clone, Debug)]
pub struct DisplayFrame {
    pub texture: wgpu::Texture,
    pub width: usize,
    pub height: usize,
}

/// Whether a render of this plan can end in a texture instead of a readback: the device is the
/// window's, the output is 8-bit, nothing is drawn over the image on the CPU (overlays) or cut
/// from it (a window that spots grew), and the texture fits the device.
fn can_display(gpu: &Gpu, plan: &Plan<'_>, req: &RenderRequest) -> bool {
    let max = gpu.device.limits().max_texture_dimension_2d as usize;
    gpu.window
        && req.depth == lcc_pipeline::OutputDepth::U8
        && req.overlay == lcc_pipeline::Overlay::None
        && plan.keep.is_none()
        && plan.w <= max
        && plan.h <= max
        && plan.w.saturating_mul(plan.h) > 0
}

/// Render on `gpu`, reusing `stages` (if given). `None` (or a result the caller discards) when the
/// render failed: the reason is recorded with [`crate::ctx::fail`]. With `display` and when
/// [`can_display`], the image stays on the device: the frame goes into `display` and the result's
/// image is empty (0 × 0; its histogram and coverage are the render's).
#[allow(clippy::too_many_arguments)]
pub fn render(
    gpu: &Gpu,
    plan: &Plan<'_>,
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    req: &RenderRequest,
    stages: Option<&GpuStages>,
    fault: Option<crate::Fault>,
    fit: Option<crate::Fit<'_>>,
    texels: Option<(f32, &mut Vec<u16>)>,
    display: Option<&mut Option<DisplayFrame>>,
) -> Option<Rendered> {
    let _in_flight = InFlight::new();
    let mut t = profiling().then(std::time::Instant::now);
    // Each stage is submitted on its own; under `{ENV_PREFIX}_PROFILE` also waited for, so the
    // timings are real.
    let lap = |what: &str, t: &mut Option<std::time::Instant>, cx: &mut Cx<'_>| match t {
        Some(t) => {
            cx.sync();
            eprintln!("  gpu {what}: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
            *t = std::time::Instant::now();
        }
        None => cx.flush(),
    };
    let (w, h) = (plan.w, plan.h);
    let Some(n) = w.checked_mul(h).filter(|n| n.checked_mul(12).is_some()) else {
        fail(FailKind::Limit, format!("{w}×{h} output is too large"));
        return None;
    };
    let _limit = match fault {
        Some(crate::Fault::Limit(bytes)) => Some(crate::ctx::LimitOverride::new(bytes)),
        _ => None,
    };
    let _oom = (fault == Some(crate::Fault::OutOfMemory)).then(crate::ctx::SimulatedOom::new);
    // an HDR render: linear float samples after the 8-bit image and its histogram (`finish.wgsl`, `DEEP`)
    let deep = req.depth == lcc_pipeline::OutputDepth::F32Hdr;
    if !gpu.fits(n * if deep { 4 } else { 3 } + if deep { HIST_WORDS } else { 0 }) {
        fail(
            FailKind::Limit,
            format!("{w}×{h} needs {} MiB buffers, over the device's {} MiB storage-buffer limit", (n * 12).div_ceil(1 << 20), gpu.limit() >> 20),
        );
        return None;
    }
    let s = &*plan.settings;
    let cached = stages.and_then(|c| c.get(src, plan.geo));
    let mut host = Host::default();
    let mut cx = Cx::new(gpu);
    lap("plan", &mut t, &mut cx);

    // 1. geometry (on the CPU when the source exceeds the device's buffer limit)
    let sampled = match &cached {
        Some(e) => e.sampled.clone(),
        // (a WarpRectilinear2 chain also goes to the CPU: its degree-14 polynomial needs f64)
        None if gpu.fits(src.data.len() * 3) && !plan.frame.warp.as_ref().is_some_and(|w| w.needs_f64()) => {
            let upload = || gpu.upload(rgb_words(src));
            // (a source too big for a view's stages to keep is the one shared copy: a window is
            // cut from the photo's own pixels, and what its stages keep is its own sampled pixels)
            let src_buf = match stages {
                Some(c) if src.data.len() * 12 <= RETAIN_SOURCE_BYTES => c.source(src, upload),
                // (an export keeps its own: photos side by side would take the one copy from each other)
                Some(_) if !crate::gate::is_batch() => shared_source(src, gpu.generation, upload),
                Some(_) => Arc::new(upload()),
                None => Arc::new(upload()),
            };
            sample(&mut cx, src, src_buf, plan)
        }
        None => {
            let img = plan.frame.sample(src, w, h);
            let b = Arc::new(gpu.upload(rgb_words(&img)));
            host.sampled = Some(img);
            b
        }
    };
    lap("sample", &mut t, &mut cx);

    // 2. white balance, defringe, spots, noise reduction
    let mut colour_noise = cached.as_ref().and_then(|e| e.colour_noise).filter(|(k, _)| *k == plan.noise_key);
    let lin = match cached.as_ref().and_then(|e| e.lin.clone()).filter(|(k, _)| *k == plan.lin_key) {
        Some((_, b)) => b,
        None => {
            if colour_noise.is_none() {
                colour_noise = lcc_pipeline::colour_noise(src, info, plan).map(|n| (plan.noise_key, n));
            }
            Arc::new(linear(&mut cx, &sampled, info, plan, &mut host, colour_noise.as_ref().map(|c| &c.1)))
        }
    };
    lap("wb/nr", &mut t, &mut cx);

    // 3. spatial planes
    let mut planes = match cached.map(|e| e.planes) {
        Some(p) if p.key == plan.lin_key => p,
        _ => Planes { key: plan.lin_key, ..Default::default() },
    };
    let mut prep = prepare(&mut cx, &lin, plan, req, &mut planes);
    // a window dehazes with the whole frame's airlight
    if let Some(a) = plan.fixed_air {
        prep.air = a;
    }
    lap("planes", &mut t, &mut cx);

    // 4. masks (kept with the planes)
    let (masks, terms, curves) = masks(&mut cx, src, &lin, &prep, plan, &mut host, &mut planes);
    lap("masks", &mut t, &mut cx);
    if let Some(c) = stages {
        c.put(Entry { src: src.clone(), geo: plan.geo, sampled, lin: Some((plan.lin_key, lin.clone())), colour_noise, planes });
    }

    // 5. per-pixel stage
    let mut fp = FinishParams::new(s, &plan.frame, info, w, h, plan.px_per_long, prep.air, req.space);
    // soft proofing as the CPU's `finish` sets it up (an output-device table never gets here: see `render`)
    fp.proof = req.proof.map(|pr| pr.params(req.space));
    // (a deep render is in the output space, as on the CPU: `finish::finish_deep`)
    if let Some(d) = req.display.as_ref().filter(|_| !deep) {
        fp.for_display(d, req.proof);
    }
    let present = Present {
        clarity: prep.clarity.is_some(),
        texture: prep.texture.is_some(),
        sharpen: prep.sharpen.is_some(),
        dark: prep.dark.is_some(),
        chroma: masks.is_some() && prep.chroma.is_some(),
    };
    // the finish kernel's `tex` binding: the texture plane, then the sharpening plane (one binding
    // for both keeps the kernel within the storage-buffer count every supported device has)
    let tex_buf: Option<Arc<Buf>> = match (&prep.texture, &prep.sharpen) {
        (Some(t), Some(sh)) => {
            let both = cx.gpu.buffer(2 * n);
            cx.copy_into(t, &both, 0);
            cx.copy_into(sh, &both, n);
            Some(Arc::new(both))
        }
        (Some(b), None) | (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    };
    let (p, aux) = finish_block(&fp, &terms, &curves, &present);
    if crate::ctx::failed() {
        return None; // a malformed parameter block failed the render: the CPU fallback redoes it
    }
    let aux = gpu.upload(&aux);
    // cleared, so that work which never ran cannot pass for an image (see the check below)
    // the histogram's words follow the image (one readback for both)
    let out = cx.zeroed(n + HIST_WORDS + if deep { 3 * n } else { 0 });
    if fault == Some(crate::Fault::Validation) {
        // a dispatch the device rejects (too many workgroups)
        cx.run("box_h", &[0; 5], &[Some(&aux), Some(&out)], [gpu.device.limits().max_compute_workgroups_per_dimension + 1, 1, 1]);
    }
    // in bands of rows: each dispatch (and submission) stays short on a slow GPU
    let mut rows = (BAND_PIXELS / w.max(1) / 16 * 16).max(16); // whole workgroups: bands never overlap
    let y0_at = crate::params::index("Y0").unwrap_or(0);
    let mut p = p;
    if deep {
        for (name, v) in [("DEEP", 1), ("DEEP_OFF", (n + HIST_WORDS) as u32)] {
            if let Some(slot) = crate::params::index(name).and_then(|i| p.get_mut(i)) {
                *slot = v;
            }
        }
    }
    // masks too big for one buffer: each band's rows of every plane (then of the chromaticity)
    // are copied into one band buffer, which the kernel reads at MASK_N / MASK_I0
    let split = match masks.as_deref() {
        Some(MaskAlpha::Split { planes, chroma }) => Some((planes, chroma.as_ref().filter(|_| present.chroma))),
        _ => None,
    };
    let band = split.map(|(planes, chroma)| {
        let per_pixel = (planes.len() + if chroma.is_some() { 3 } else { 0 }).max(1);
        let fit = usize::try_from(gpu.limit() / 4).unwrap_or(usize::MAX) / per_pixel.saturating_mul(w).max(1) / 16 * 16;
        rows = rows.min(fit.max(16));
        cx.gpu.buffer(per_pixel * rows.min(h) * w)
    });
    let band_at = (crate::params::index("MASK_N"), crate::params::index("MASK_I0"));
    for y0 in (0..h).step_by(rows).filter(|_| fault != Some(crate::Fault::DropWork)) {
        p[y0_at] = y0 as u32;
        if let (Some((planes, chroma)), Some(band), (Some(mn_at), Some(mi_at))) = (split, &band, band_at) {
            let (nb, i0) = (rows.min(h - y0) * w, y0 * w);
            for (m, plane) in planes.iter().enumerate() {
                cx.copy_range(plane, i0, band, m * nb, nb);
            }
            if let Some(ch) = chroma {
                cx.copy_range(ch, 3 * i0, band, planes.len() * nb, 3 * nb);
            }
            if let (Some(a), Some(b)) = (p.get_mut(mn_at), u32::try_from(nb).ok()) {
                *a = b;
            }
            if let (Some(a), Some(b)) = (p.get_mut(mi_at), u32::try_from(i0).ok()) {
                *a = b;
            }
        }
        let mask_buf = match masks.as_deref() {
            Some(MaskAlpha::Joined(b)) => Some(b),
            Some(MaskAlpha::Split { .. }) => band.as_ref(),
            None => None,
        };
        cx.run(
            "main",
            &p,
            &[
                Some(&lin),
                Some(&prep.log_l),
                Some(&prep.base),
                prep.clarity.as_deref(),
                tex_buf.as_deref(),
                prep.dark.as_deref(),
                mask_buf,
                Some(&aux),
                Some(&out),
            ],
            groups2(w, rows.min(h - y0), [16, 16]),
        );
    }
    if let Some(slot) = display.filter(|_| can_display(gpu, plan, req)) {
        let coverage = plan.frame.coverage(src.width, src.height, w, h).map(Arc::new);
        let (frame, histogram) = to_texture(&mut cx, &out, w, h, coverage.as_deref())?;
        lap("finish + present", &mut t, &mut cx);
        *slot = Some(frame);
        return Some(Rendered {
            image: Rgba8 { width: 0, height: 0, data: Vec::new() },
            histogram,
            deep: None,
            warnings: Vec::new(),
            coverage,
            alpha: None,
        });
    }
    // the alpha a mask overlay shows: copied out before the readback below submits
    let overlay_mask = req.overlay.mask(s).map(|m| {
        let list = s.masks.iter().filter(|m| m.visible && !m.components.is_empty()).take(lcc_pipeline::masks::max_masks(w, h));
        match list.map(|m| m.id).position(|id| id == m.id).zip(masks.as_ref()).and_then(|(mi, all)| all.plane(&mut cx, mi, n)) {
            Some(plane) => Ok(plane),
            None => Err(m),
        }
    });
    // every visible mask's alpha, for `Overlay::AllMasks` (one tint over all masks at once).
    // The masks buffer holds each visible mask's alpha back-to-back in the same `list` order the
    // single-mask overlay uses, so read the whole buffer back in ONE GPU->CPU transfer and split it
    // on the CPU, instead of one synchronous readback per mask (which stalls the pipeline N times).
    let all_overlay_alphas: Option<Vec<Plane>> = if matches!(req.overlay, lcc_pipeline::Overlay::AllMasks { .. }) {
        masks.as_ref().map(|all| {
            let list: Vec<_> = s.masks.iter().filter(|m| m.visible && !m.components.is_empty()).take(lcc_pipeline::masks::max_masks(w, h)).collect();
            if list.is_empty() {
                return Vec::new();
            }
            let all = match &**all {
                MaskAlpha::Joined(b) => b,
                // (planes too big for one buffer: one readback each)
                MaskAlpha::Split { planes, .. } => {
                    return planes.iter().take(list.len()).map(|b| Plane { width: w, height: h, data: cx.read(b, n.min(b.len)) }).collect();
                }
            };
            let raw: Vec<f32> = cx.read(all, n * list.len());
            let mut alphas = Vec::with_capacity(list.len());
            for mi in 0..list.len() {
                let s = mi * n;
                alphas.push(Plane { width: w, height: h, data: raw.get(s..s + n).map(<[f32]>::to_vec).unwrap_or_default() });
            }
            alphas
        })
    } else {
        None
    };
    // the blank border past the photo's edge is shown but isn't part of the photo (issue #211)
    let coverage = plan.frame.coverage(src.width, src.height, w, h).map(|c| match plan.keep {
        Some(k) => Arc::new(c.crop(k.x, k.y, k.w, k.h)),
        None => Arc::new(c),
    });
    // the histogram of the kept window, binned on the GPU beside the image it reads
    let window = plan.keep.map_or((0, 0, w, h), |k| {
        let (x0, y0) = (k.x.min(w), k.y.min(h));
        (x0, y0, k.w.min(w - x0), k.h.min(h - y0))
    });
    let hist = histogram_on_gpu(&mut cx, &out, w, window, coverage.as_deref());
    // The display fit (`lcc_raster::resample::fit_rgba8`) here instead of on the CPU: only the
    // fitted image comes back. The histogram is the full frame's, as without it. Not for what the
    // CPU still changes after the readback (overlays, a grown window cut back) or HDR renders.
    let (fit, mut full) = fit.map_or((None, None), |f| (Some(f.size), f.full));
    let fitted = fit
        .filter(|_| !deep && req.overlay == lcc_pipeline::Overlay::None && plan.keep.is_none())
        .map(|(fw, fh)| lcc_raster::resample::fit_size(w, h, fw.max(1), fh.max(1)))
        .filter(|&(dw, dh)| dw < w || dh < h);
    // (with the full-size image after the fitted one and the histogram, for a caller that keeps it:
    // the view preview stays at the level it was rendered at; only what is drawn is fitted. One
    // readback for all three.)
    let (out, w, h, n, full_size) = match fitted {
        Some((dw, dh)) => {
            let dn = dw * dh;
            let rgb = cx.gpu.buffer(3 * n);
            cx.run("unpack_rgb8", &[n as u32], &[Some(&out), Some(&rgb)], groups1(n));
            let small = resize(&mut cx, &rgb, (w, h), (dw, dh), 3, Filter::Lanczos3);
            let keep = full.is_some();
            let packed = cx.zeroed(dn + HIST_WORDS + if keep { out.len } else { 0 });
            cx.run("pack_rgb8", &[dn as u32], &[Some(&small), Some(&packed)], groups1(dn));
            if keep {
                cx.copy_into(&out, &packed, dn + HIST_WORDS);
            }
            (packed, dw, dh, dn, keep.then_some((w, h)))
        }
        None => (out, w, h, n, None),
    };
    cx.copy_into(&hist, &out, n);
    // An HDR view's texture asked for: its floats become `Rgba16Float` texels for that headroom
    // here (`hdr_texels.wgsl`), and only those come back (8 bytes a pixel instead of 12), with no
    // unpacking or encoding on the CPU
    let texels = texels.filter(|_| deep);
    let extra = match (&texels, deep) {
        (Some((headroom, _)), true) => {
            let tex = cx.zeroed(2 * n);
            cx.run("hdr_texels", &[n as u32, (n + HIST_WORDS) as u32, headroom.to_bits()], &[Some(&out), Some(&tex)], groups1(n));
            cx.copy_into(&tex, &out, n + HIST_WORDS);
            2 * n
        }
        (None, true) => 3 * n,
        _ => 0,
    };
    let mut data: Vec<[u8; 4]> = match (full_size, full.as_mut()) {
        // (the full-size image straight into its own vector: no copy of it on the CPU)
        (Some((fw, fh)), Some(slot)) => {
            let mut parts = cx.read_parts(&out, &[n + HIST_WORDS + extra, fw * fh]);
            **slot = parts.pop().map(|data| Rgba8 { width: fw, height: fh, data });
            parts.pop().unwrap_or_default()
        }
        _ => cx.read(&out, n + HIST_WORDS + extra),
    };
    let tail = if deep { data.split_off((n + HIST_WORDS).min(data.len())) } else { Vec::new() };
    let texel_words = if texels.is_some() { tail.len() } else { 0 };
    let samples: Vec<f32> = if texels.is_none() { tail.iter().map(|b| f32::from_le_bytes(*b)).collect() } else { Vec::new() };
    if let Some((_, out_texels)) = texels {
        out_texels.clear();
        out_texels.extend(tail.iter().flat_map(|b| [u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]])]));
        // a window the joint colour NR grew is cut back to the request (four texel values per pixel)
        if let Some(k) = plan.keep.filter(|_| texel_words == 2 * n) {
            let cut = crop_rows(out_texels, w, 4, k);
            *out_texels = cut;
        }
    }
    let hist_words: Vec<u32> = data.split_off(n.min(data.len())).into_iter().map(u32::from_le_bytes).collect();
    let image = Rgba8 { width: w, height: h, data };
    lap("finish + readback", &mut t, &mut cx);
    if crate::ctx::failed() {
        return None;
    }
    // The per-pixel kernel writes alpha 255 everywhere and `out` was cleared: a pixel with any
    // other alpha means work silently did not run (a driver that reset or dropped a submission
    // without reporting it — issue #78's all-black exports).
    let unwritten = unwritten(&image.data);
    if unwritten > 0 {
        fail(FailKind::Fatal, format!("the GPU returned an incomplete image ({unwritten} of {n} pixels unwritten)"));
        return None;
    }
    let histogram = histogram_words(&hist_words);
    lap("histogram", &mut t, &mut cx);
    // An entirely black result (upstream work that did not run would give that too): redo it on
    // the CPU, which costs time only for the rare photo that really is black.
    if n >= 4096 && histogram.total > 0 && [&histogram.r, &histogram.g, &histogram.b].iter().all(|c| c.first() == Some(&histogram.total)) {
        fail(FailKind::Blank, "the GPU result is entirely black; rendering it on the CPU to be sure".into());
        return None;
    }
    let mut image = image;
    let overlay_mask = overlay_mask.map(|r| match r {
        Ok(b) => cx.read_plane(&b, w, h),
        // a hidden mask: evaluate it on the CPU
        Err(m) => {
            let img = host.lin.get_or_insert_with(|| Arc::new(cx.read_rgb(&lin, w, h))).clone();
            let l = host.log_l.get_or_insert_with(|| Arc::new(cx.read_plane(&prep.log_l, w, h))).clone();
            lcc_pipeline::masks::evaluate_one(m, &plan.frame, w, h, &img, &l, s.light.exposure as f32, plan.mattes.as_deref())
        }
    });
    if deep && texel_words > 0 {
        if texel_words != 2 * n {
            fail(FailKind::Fatal, format!("the GPU returned an incomplete HDR texture ({texel_words} of {} words)", 2 * n));
            return None;
        }
        // (the texture's texels went out above, cut back to a window the joint colour NR grew; no
        // float samples come back)
        if let Some(k) = plan.keep {
            image = image.crop(k.x, k.y, k.w, k.h);
        }
        return Some(Rendered { image, histogram, deep: None, warnings: Vec::new(), coverage, alpha: None });
    }
    if deep {
        if samples.len() != 3 * n {
            fail(FailKind::Fatal, format!("the GPU returned an incomplete HDR image ({} of {} samples)", samples.len(), 3 * n));
            return None;
        }
        // as on the CPU (`finish_deep`): the whole frame in the output space, no overlays
        let deep = lcc_pipeline::DeepImage { width: w, height: h, space: req.space, samples: lcc_pipeline::DeepSamples::F32(samples) };
        // (a window the joint colour NR grew is cut back to the request, as on the CPU)
        let (deep, image) = match plan.keep {
            Some(k) => (deep.crop(k.x, k.y, k.w, k.h), image.crop(k.x, k.y, k.w, k.h)),
            None => (deep, image),
        };
        return Some(Rendered { image, histogram, deep: Some(deep), warnings: Vec::new(), coverage, alpha: None });
    }
    lcc_pipeline::visualize::apply(&mut image, req.overlay, plan, overlay_mask.as_ref(), all_overlay_alphas.as_deref());
    // spots grew the window the render worked on: cut it back to the request
    if let Some(k) = plan.keep {
        image = image.crop(k.x, k.y, k.w, k.h);
    }
    Some(Rendered { image, histogram, deep: None, warnings: Vec::new(), coverage, alpha: None })
}

/// Words of the `hist_k` kernel's output: five 256-bin channels, the total, the vectorscope grid and
/// the waveform planes.
const HIST_WORDS: usize = 5 * Histogram::BINS + 1 + Histogram::SCOPE_CELLS + Histogram::WAVE_PLANES * Histogram::WAVE_CELLS;

/// Record the `hist_k` kernel over the `(x0, y0, w, h)` window of the packed image `out` (`stride` pixels
/// wide): the samples, bins, scope and waveform [`Histogram::of_srgb8_covered`] gives the window.
fn histogram_on_gpu(cx: &mut Cx<'_>, out: &Buf, stride: usize, window: (usize, usize, usize, usize), coverage: Option<&lcc_raster::Coverage>) -> Buf {
    let (x0, y0, w, h) = window;
    // the same sampling as the CPU oracle: at most ~1 MP, on a regular grid
    let step = (((w * h) as f64 / 1_000_000.0).sqrt().ceil() as usize).max(1);
    let (across, rows) = (w.div_ceil(step), h.div_ceil(step));
    let coverage = coverage.filter(|c| c.width() == w && c.height() == h);
    let cov = coverage.map(|c| cx.gpu.upload(c.words()));
    let row_words = lcc_raster::Coverage::row_words(w);
    let p = [stride, x0, y0, w, h, step, across, rows, usize::from(cov.is_some()), row_words].map(|v| v as u32);
    let hist = cx.zeroed(HIST_WORDS);
    cx.run("hist_k", &p, &[Some(out), cov.as_ref(), Some(&hist)], groups1(across * rows));
    hist
}

/// The [`Histogram`] from the `hist_k` kernel's words (waveform counts saturate as the CPU's do).
fn histogram_words(words: &[u32]) -> Histogram {
    let n = Histogram::BINS;
    let part = |a: usize, len: usize| words.get(a..a + len).map_or_else(|| vec![0; len], <[u32]>::to_vec);
    let wave_at = 5 * n + 1 + Histogram::SCOPE_CELLS;
    let wave = part(wave_at, Histogram::WAVE_PLANES * Histogram::WAVE_CELLS).into_iter().map(|c| c.min(u16::MAX as u32) as u16).collect();
    Histogram {
        r: part(0, n),
        g: part(n, n),
        b: part(2 * n, n),
        luma: part(3 * n, n),
        chroma: part(4 * n, n),
        total: words.get(5 * n).copied().unwrap_or(0),
        scope: part(5 * n + 1, Histogram::SCOPE_CELLS),
        wave,
        hdr: None,
    }
}

/// The finished output `out` (`w × h` packed RGBA8, then room for the histogram) as a texture on
/// the device, with its histogram; the same checks as a readback (every pixel written, not
/// entirely black), done on the device: only the histogram and a pixel count come back.
fn to_texture(cx: &mut Cx<'_>, out: &Buf, w: usize, h: usize, coverage: Option<&lcc_raster::Coverage>) -> Option<(DisplayFrame, Histogram)> {
    let n = w * h;
    let hist = histogram_on_gpu(cx, out, w, (0, 0, w, h), coverage);
    let count = cx.zeroed(1);
    cx.run("unwritten", &[n as u32], &[Some(out), None, Some(&count)], groups1(n));
    // texture copies take rows of a multiple of 256 bytes: pad them unless they already are
    let stride = w.next_multiple_of(64);
    let padded = (stride != w).then(|| {
        let b = cx.gpu.buffer(stride * h);
        cx.run("pad_rows", &[w as u32, h as u32, stride as u32], &[Some(out), Some(&b), None], groups2(w, h, [16, 16]));
        b
    });
    let texture = cx.gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("view"),
        size: wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        // (COPY_SRC: a screenshot or a test can read it back)
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    cx.copy_to_texture(padded.as_ref().unwrap_or(out), stride, &texture, w, h);
    let words: Vec<u32> = cx.read(&hist, HIST_WORDS);
    let left = cx.read::<u32>(&count, 1).first().copied().unwrap_or(0);
    if crate::ctx::failed() {
        return None;
    }
    if left > 0 {
        fail(FailKind::Fatal, format!("the GPU returned an incomplete image ({left} of {n} pixels unwritten)"));
        return None;
    }
    let histogram = histogram_words(&words);
    if n >= 4096 && histogram.total > 0 && [&histogram.r, &histogram.g, &histogram.b].iter().all(|c| c.first() == Some(&histogram.total)) {
        fail(FailKind::Blank, "the GPU result is entirely black; rendering it on the CPU to be sure".into());
        return None;
    }
    Some((DisplayFrame { texture, width: w, height: h }, histogram))
}

/// The `k` window of an image `w` pixels wide stored as `per` values per pixel (clamped to the image).
fn crop_rows<T: Copy>(v: &[T], w: usize, per: usize, k: lcc_pipeline::PixelWindow) -> Vec<T> {
    let rows = v.len() / (w.max(1) * per.max(1));
    let (x0, y0) = (k.x.min(w), k.y.min(rows));
    let (cw, ch) = (k.w.min(w - x0), k.h.min(rows - y0));
    let mut out = Vec::with_capacity(cw * ch * per);
    for y in y0..y0 + ch {
        let at = (y * w + x0) * per;
        out.extend_from_slice(v.get(at..at + cw * per).unwrap_or_default());
    }
    out
}

/// Pixels whose alpha is not 255 (cheap: no early exit, so it vectorizes).
fn unwritten(data: &[[u8; 4]]) -> usize {
    data.chunks(4096).filter(|c| c.iter().fold(255u8, |a, p| a & p[3]) != 255).map(|c| c.iter().filter(|p| p[3] != 255).count()).sum()
}

/// The white-balanced, retouched, denoised image (`colour_noise`: see `denoise`).
fn linear(cx: &mut Cx<'_>, sampled: &Buf, info: &SourceInfo, plan: &Plan<'_>, host: &mut Host, colour_noise: Option<&JointNoise>) -> Buf {
    let (w, h) = (plan.w, plan.h);
    let n = w * h;
    let s = &*plan.settings;
    let img = if lcc_pipeline::lin_needs_cpu(s, info) {
        // content-aware Fill spots / the camera's local tone map: the whole stage on the CPU
        let mut img = match host.sampled.take() {
            Some(i) => i,
            None => cx.read_rgb(sampled, w, h),
        };
        lcc_pipeline::lin_cpu(&mut img, info, plan);
        cx.gpu.upload(rgb_words(&img))
    } else {
        let m = local::wb_matrix_for(info, s);
        let mut p = vec![m.is_some() as u32];
        p.extend(m.unwrap_or_default().iter().flatten().map(|v| v.to_bits()));
        let out = cx.gpu.buffer(n * 3);
        map(cx, "wb_k", n, &p, [Some(sampled), None, None], &out);
        // `lin_cpu`'s order: white balance, defringe, spots, red eye
        let out = defringe(cx, out, plan);
        let out = spots(cx, out, plan);
        if plan.eyes.is_empty() {
            out
        } else {
            let mut p = vec![w as u32, plan.eyes.len() as u32];
            p.extend(plan.eyes.iter().flat_map(|e| e.words()).map(f32::to_bits));
            let eyed = cx.gpu.buffer(n * 3);
            map(cx, "redeye_k", n, &p, [Some(&out), None, None], &eyed);
            eyed
        }
    };
    let img = denoise(cx, img, plan, colour_noise);
    if !lcc_pipeline::skin_needs_cpu(s) {
        return img;
    }
    // Smooth Skin on the device when every bucket's region fits its buffers (`skin_gpu`)
    let skin_done = skin_gpu(cx, &img, plan);
    if skin_done && !lcc_pipeline::retouch::active(s) {
        return img;
    }
    // the retouch of people's parts (and Smooth Skin when it didn't fit): the CPU's (the same code
    // renders both, so they match)
    let mut host_img = cx.read_rgb(&img, w, h);
    lcc_pipeline::timed("smooth skin", || {
        if !skin_done {
            lcc_pipeline::skin::apply(&mut host_img, s, &plan.frame, plan.px_per_long);
        }
    });
    lcc_pipeline::timed("retouch", || lcc_pipeline::retouch::apply(&mut host_img, s, &plan.frame, plan.px_per_long));
    let buf = cx.gpu.upload(rgb_words(&host_img));
    host.lin = Some(Arc::new(host_img));
    buf
}

/// Smooth Skin (`lcc_pipeline::skin`) on the device, in place on `img`, from the CPU's plan
/// (coverage and regions): `true` when done, `false` when there is nothing to do or a region is
/// over the device's buffer limit (the CPU does it then).
fn skin_gpu(cx: &mut Cx<'_>, img: &Buf, plan: &Plan<'_>) -> bool {
    use lcc_pipeline::skin;
    let (w, h) = (plan.w, plan.h);
    let Some(work) = skin::plan(w, h, &plan.settings, &plan.frame, plan.px_per_long) else { return false };
    if work.buckets.iter().any(|b| !cx.gpu.fits(b.region[2] * b.region[3] * 4)) || !cx.gpu.fits(work.cover.data.len()) {
        return false;
    }
    let t = profiling().then(std::time::Instant::now);
    let cov = cx.gpu.upload(&work.cover.data);
    let (ox, oy) = work.origin;
    let (cw, ch) = (work.cover.width, work.cover.height);
    let [kr, kg, kb, eps] = skin::LAB_CONSTANTS;
    let consts = skin::combine_constants(&work.params);
    for b in &work.buckets {
        let [rx0, ry0, rw, rh] = b.region;
        let groups = groups2(rw, rh, [16, 16]);
        let mut p = vec![w as u32, rx0 as u32, ry0 as u32, rw as u32, rh as u32, ox as u32, oy as u32, cw as u32, ch as u32];
        p.extend([skin::MIN_COVER, kr, kg, kb, eps].map(f32::to_bits));
        let lab = cx.gpu.buffer(rw * rh * 4);
        cx.run("skin_lab", &p, &[Some(img), Some(&cov), Some(&lab), None, None, None], groups);
        let fine = gaussian(cx, &lab, rw, rh, 4, b.fine_sigma());
        // the wide bands on reduced copies (`skin::Reduced`)
        let band = |cx: &mut Cx<'_>, sigma: f64| -> (Buf, usize, usize, usize) {
            let (f, blur) = skin::reduction(sigma);
            if f == 1 {
                return (gaussian(cx, &lab, rw, rh, 4, blur), 1, rw, rh);
            }
            let (dw, dh) = (rw.div_ceil(f), rh.div_ceil(f));
            let small = cx.gpu.buffer(dw * dh * 4);
            // (the unused writable binding gets a buffer of its own: the shared placeholder is bound read-only too)
            let scratch = cx.gpu.buffer(1);
            cx.run(
                "skin_reduce",
                &[rw as u32, rh as u32, f as u32, dw as u32, dh as u32],
                &[Some(&scratch), None, Some(&small), Some(&lab), None, None],
                groups2(dw, dh, [16, 16]),
            );
            (gaussian(cx, &small, dw, dh, 4, blur), f, dw, dh)
        };
        let (mid, mf, mw, mh) = band(cx, b.mid_sigma());
        let (low, lf, lw, lh) = band(cx, b.low_sigma());
        let mut mask = [0u32; 2];
        for &m in &b.members {
            if let Some(word) = mask.get_mut(m / 32) {
                *word |= 1 << (m % 32);
            }
        }
        let faces = work.faces.len().min(64);
        let mut p = vec![w as u32, rx0 as u32, ry0 as u32, rw as u32, rh as u32, ox as u32, oy as u32, cw as u32, ch as u32];
        p.extend([mf, mw, mh, lf, lw, lh].map(|v| v as u32));
        p.extend([b.all as u32, faces as u32, mask[0], mask[1]]);
        p.extend(consts.map(f32::to_bits));
        p.extend([skin::MIN_COVER, skin::SHINE[0], skin::SHINE[1], kr, kg, kb, eps].map(f32::to_bits));
        p.extend(work.faces.iter().take(64).flat_map(|f| [f.x as f32, f.y as f32, f.d as f32]).map(f32::to_bits));
        cx.run("skin_combine", &p, &[Some(img), Some(&cov), Some(&lab), Some(&fine), Some(&mid), Some(&low)], groups);
    }
    if let Some(t) = t {
        cx.sync();
        eprintln!("    gpu smooth skin ({} buckets): {:.1} ms", work.buckets.len(), t.elapsed().as_secs_f64() * 1e3);
    }
    true
}

/// Defringe (`optics::defringe`) of the white-balanced image.
fn defringe(cx: &mut Cx<'_>, img: Buf, plan: &Plan<'_>) -> Buf {
    use lcc_pipeline::optics;
    let (w, h) = (plan.w, plan.h);
    let Some(dp) = optics::defringe_params(&plan.settings, plan.px_per_long / optics::DEFRINGE_REF_LONG, w, h) else { return img };
    let n = w * h;
    let lab = cx.gpu.buffer(n * 3);
    map(cx, "fringe_lab", n, &[], [Some(&img), None, None], &lab);
    // the lightness channel, then its min and max around each pixel
    let l = cx.gpu.buffer(n);
    map(cx, "subsample", n, &[3], [Some(&lab), None, None], &l);
    let r = dp.radius.min(w.max(h)) as u32;
    let filtered = |cx: &mut Cx<'_>, max: u32| {
        let tmp = cx.gpu.buffer(n);
        cx.run("minmax", &[w as u32, h as u32, r, 0, max], &[Some(&l), Some(&tmp)], groups2(w, h, [16, 16]));
        let out = cx.gpu.buffer(n);
        cx.run("minmax", &[w as u32, h as u32, r, 1, max], &[Some(&tmp), Some(&out)], groups2(w, h, [16, 16]));
        out
    };
    let (lo, hi) = (filtered(cx, 0), filtered(cx, 1));
    // the kernel writes only the pixels it changes
    let out = cx.copy(&img);
    let p = [dp.purple.0, dp.purple.1, dp.green.0, dp.green.1, dp.purple_strength, dp.green_strength].map(f32::to_bits);
    map(cx, "fringe_apply", n, &p, [Some(&lab), Some(&lo), Some(&hi)], &out);
    out
}

/// Spot removal (`spots::apply`; Clone, Heal, Remove), spot by spot on `img` in place: each spot
/// reads the image as the earlier ones left it.
fn spots(cx: &mut Cx<'_>, img: Buf, plan: &Plan<'_>) -> Buf {
    use lcc_pipeline::spots;
    let (w, h) = (plan.w, plan.h);
    for spot in &plan.settings.spots {
        let Some(l) = spots::layout(spot, &plan.frame, w, h, plan.px_per_long) else { continue };
        let Some(&first) = l.pts.first() else { continue };
        let offset = match l.offset {
            Some(o) => o,
            None => auto_source(cx, &img, w, h, first, l.r),
        };
        let pts = cx.gpu.upload(&l.pts.iter().flat_map(|p| [p.0, p.1]).collect::<Vec<f32>>());
        let nb = l.bw * l.bh;
        let (al, sp, df, wt) = (cx.gpu.buffer(nb), cx.gpu.buffer(3 * nb), cx.gpu.buffer(3 * nb), cx.gpu.buffer(nb));
        let mut p = vec![w as u32, h as u32, l.bx as u32, l.by as u32, l.bw as u32, l.bh as u32, l.pts.len() as u32];
        p.extend([l.r, l.inner, l.opacity, offset.0, offset.1].map(f32::to_bits));
        p.push(l.heal as u32);
        let groups = groups2(l.bw, l.bh, [16, 16]);
        cx.run("spot_prep", &p, &[Some(&img), Some(&pts), Some(&al), Some(&sp), Some(&df), Some(&wt)], groups);
        // Heal: the tone correction is the masked difference's normalized blur
        let blurred = l.heal.then(|| {
            let sigma = (l.r * 0.6).max(1.0);
            (gaussian(cx, &df, l.bw, l.bh, 3, sigma), gaussian(cx, &wt, l.bw, l.bh, 1, sigma))
        });
        let (bd, bm) = blurred.as_ref().map_or((&df, &wt), |(d, m)| (d, m));
        cx.run("spot_apply", &p, &[Some(&img), Some(&pts), Some(&al), Some(&sp), Some(bd), Some(bm)], groups);
    }
    img
}

/// `spots::auto_source` on the device: every candidate scored at once, only the scores read back.
fn auto_source(cx: &mut Cx<'_>, img: &Buf, w: usize, h: usize, target: (f32, f32), r: f32) -> (f32, f32) {
    use lcc_pipeline::spots;
    let cands = spots::candidates(r);
    let mut data: Vec<f32> = cands.iter().flat_map(|d| [d.0, d.1]).collect();
    data.extend(spots::ring_dirs().iter().flat_map(|d| [d.0, d.1]));
    let pts = cx.gpu.upload(&data);
    let [around, within] = spots::score_radii(r);
    let mut p = vec![w as u32, h as u32];
    p.extend([target.0, target.1].iter().chain(&around).chain(&within).map(|v| v.to_bits()));
    let scores = cx.gpu.buffer(cands.len());
    cx.run("spot_score", &p, &[Some(img), Some(&pts), Some(&scores), None, None, None], [cands.len().div_ceil(64) as u32, 1, 1]);
    let scores: Vec<f32> = cx.read(&scores, cands.len());
    spots::rank(&cands, &scores).first().copied().unwrap_or_else(|| spots::fallback_source(r))
}

/// Noise reduction (`local::denoise`). `colour_noise`: the whole frame's noise model for the joint
/// colour NR of process V8 (`lcc_pipeline::colour_noise`); `None`: measured on `img`.
fn denoise(cx: &mut Cx<'_>, img: Buf, plan: &Plan<'_>, colour_noise: Option<&JointNoise>) -> Buf {
    let (w, h) = (plan.w, plan.h);
    let n = w * h;
    let (lum, col) = local::nr_params(&plan.settings, plan.detail_scale);
    let mut img = img;
    if let Some(nr) = lum {
        let l = cx.gpu.buffer(n);
        map(cx, "log_lum_k", n, &[], [Some(&img), None, None], &l);
        let f = guided(cx, &l, w, h, nr.sigma, nr.eps);
        let out = cx.gpu.buffer(n * 3);
        map(cx, "nr_lum", n, &[nr.k.to_bits()], [Some(&img), Some(&l), Some(&f)], &out);
        img = out;
    }
    if col.is_some() && local::joint_colour_nr(&plan.settings) {
        let Some(p) = local::nrc_params(&plan.settings, plan.detail_scale) else { return img };
        let (detail, amount) = local::nrc_sliders(&plan.settings);
        if let Some(out) = color_nr_joint(cx, &img, w, h, &p, detail, amount, colour_noise, None) {
            return out;
        }
        // too small for the noise samples, not even one band fits the device, or a batch render holds
        // the bands while this one is interactive: this step on the CPU
        let mut host = cx.read_rgb(&img, w, h);
        let mut d = (*plan.settings).clone();
        d.detail.nr_luminance = 0.0;
        local::denoise_with(&mut host, &d, plan.detail_scale, false, colour_noise);
        return cx.gpu.upload(rgb_words(&host));
    }
    if let Some(nr) = col {
        // luminance-weighted chromaticity: blur max(rgb, 0), take rgb / Y of the blur, and pull
        // speckle outliers against the 3 × 3 median chromaticity all the way in
        // (`local::colour_nr`, `local::SPECKLE_LO` / `SPECKLE_HI`)
        let clamped = cx.gpu.buffer(n * 3);
        map(cx, "clamp0_k", n, &[], [Some(&img), None, None], &clamped);
        let b = gaussian(cx, &clamped, w, h, 3, nr.sigma);
        let med = cx.gpu.buffer(n * 3);
        map(cx, "chroma_med", n, &[w as u32], [Some(&img), None, None], &med);
        let out = cx.gpu.buffer(n * 3);
        map(cx, "nr_col", n, &[nr.t.to_bits(), local::SPECKLE_LO.to_bits(), local::SPECKLE_HI.to_bits()], [Some(&img), Some(&med), Some(&b)], &out);
        img = out;
    }
    img
}

/// Run a `nrcj.wgsl` kernel on `n` items with inputs `a`, `b` and output `out`.
fn nrcj(cx: &mut Cx<'_>, k: &str, n: usize, p: &[u32], a: &Buf, b: Option<&Buf>, out: &Buf) {
    cx.run(k, p, &[Some(a), b, Some(out)], groups1(n));
}

/// Device memory left aside when the joint NR plans its bands from the device's memory (the rest of
/// the render, the display).
const VRAM_RESERVE: u64 = 384 << 20;

/// Band budget (bytes) when the device's memory is unknown.
const FIXED_BAND_BUDGET: usize = 2048 << 20;

/// Renders on the device right now ([`render`]). An export batch renders photos side by side: while one
/// plans its noise reduction bands, each of the others may still allocate the buffers of its other stages.
static RENDERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The bytes the joint colour noise reduction's bands may take when pinned with
/// [`crate::set_nr_band_budget`] (0: not pinned, planned from the device by [`band_budget_from`]).
pub(crate) static NR_BAND_BUDGET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Held while a noise reduction plans its bands and runs them: two plans made at once from the same
/// memory would together ask for more than there is.
static BANDS: Bands = Bands::new();

/// A lock that knows whether a batch render holds it ([`Bands::take`]).
struct Bands {
    /// `Some(batch)` while held, by a batch render or not.
    held: Mutex<Option<bool>>,
    freed: std::sync::Condvar,
}

/// [`BANDS`] held; dropping it lets the next render in.
struct BandsGuard<'a>(&'a Bands);

impl Bands {
    const fn new() -> Self {
        Bands { held: Mutex::new(None), freed: std::sync::Condvar::new() }
    }

    /// Wait for the bands and take them. An interactive render never waits for a batch render's bands
    /// (`None`: it runs the step on the CPU): between submissions a batch render may yield to the
    /// interface (`gate::throttle`) and wait for those interactive renders to finish, so both would hang.
    /// (Even on the second card, which doesn't yield: whether a device is the second card changes when
    /// the card is turned off or another is chosen, mid-render.) Batch renders wait for any holder;
    /// interactive ones for another interactive one, which always finishes.
    fn take(&self) -> Option<BandsGuard<'_>> {
        self.take_as(crate::gate::is_batch())
    }

    /// [`Bands::take`] for a render that is `batch` or not.
    fn take_as(&self, batch: bool) -> Option<BandsGuard<'_>> {
        let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match *held {
                None => {
                    *held = Some(batch);
                    return Some(BandsGuard(self));
                }
                Some(true) if !batch => return None,
                Some(_) => held = self.freed.wait(held).unwrap_or_else(std::sync::PoisonError::into_inner),
            }
        }
    }
}

impl Drop for BandsGuard<'_> {
    fn drop(&mut self) {
        *self.0.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.0.freed.notify_all();
    }
}

/// Counts a render in [`RENDERS`] while it lives.
struct InFlight;

impl InFlight {
    fn new() -> Self {
        RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        InFlight
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        RENDERS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The band budget from the device's memory `vram` (`lcc_gpumem`: what the backend says the device
/// offers, not what is free right now), of which half is left to other programs and the display, less
/// our own buffers in use (`allocated` minus the `pooled` ones we reuse) and [`VRAM_RESERVE`] for each
/// render on the device (this one's other stages and every other render's).
fn band_budget_from(vram: u64, allocated: u64, pooled: u64, renders: usize) -> usize {
    let reserve = VRAM_RESERVE.saturating_mul(renders.max(1) as u64);
    let in_use = allocated.saturating_sub(pooled);
    usize::try_from((vram / 2).saturating_sub(in_use).saturating_sub(reserve)).unwrap_or(usize::MAX)
}

/// The joint colour noise reduction (`lcc_pipeline::nrc_joint`) on the GPU, the same steps as the
/// CPU: only the noise samples are read back (the noise model is fitted on the CPU, by the same code).
/// Large images are filtered in horizontal bands whose halo covers every step's reach, so each kept pixel
/// gets the whole-image result; the noise model is the whole image's, or `frame_noise` (the whole frame's,
/// for a window). `budget`: the bytes the bands may take (`None`: planned from the device). `None` when
/// the image is too small for the noise samples, no band fits the device, or an interactive render finds
/// a batch render holding the bands ([`Bands::take`]) (the caller runs this step on the CPU).
#[allow(clippy::too_many_arguments)]
fn color_nr_joint(
    cx: &mut Cx<'_>,
    img: &Buf,
    w: usize,
    h: usize,
    p: &local::NrcParams,
    detail: f32,
    amount: f32,
    frame_noise: Option<&JointNoise>,
    budget: Option<usize>,
) -> Option<Buf> {
    use lcc_pipeline::nrc_joint;
    let strength = nrc_joint::strength(amount);
    let brake = nrc_joint::speck_brake(amount);
    if w < 3 || h < 3 {
        return None;
    }
    if strength <= 0.0 {
        return Some(cx.copy(img));
    }
    let n = w.checked_mul(h)?;
    // the kernels index with 32-bit words: 3 values per pixel must stay below that
    if n.checked_mul(3).is_none_or(|v| u32::try_from(v).is_err()) {
        return None;
    }
    // One band plan at a time (`BANDS`), held from the noise samples until the bands are done; an
    // interactive render that finds it held by a batch render runs this step on the CPU
    // (`Bands::take`), before it spends anything on the samples.
    let Some(_bands) = BANDS.take() else {
        if profiling() {
            eprintln!("  gpu joint: bands in use by a batch render, colour NR on the CPU");
        }
        return None;
    };
    let started = profiling().then(std::time::Instant::now);
    let mut noise = match frame_noise {
        Some(noise) => *noise,
        None => {
            // noise samples of the whole image: same pixels as the CPU, fitted by the same code
            let stride = nrc_joint::noise_sample_stride(n);
            let count = n.div_ceil(stride);
            let smp = cx.gpu.buffer(11 * count);
            nrcj(cx, "nrcj_samples", count, &[count as u32, w as u32, h as u32, stride as u32], img, None, &smp);
            let raw: Vec<f32> = cx.read(&smp, 11 * count);
            drop(smp);
            if let Some(t) = started {
                eprintln!("  gpu joint samples + read: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
            }
            let samples: Vec<nrc_joint::NoiseSample> = raw
                .as_chunks::<11>()
                .0
                .iter()
                .filter(|s| s[0] == 1.0 && s.iter().all(|v| v.is_finite()))
                .map(|s| (s[1], [[s[2], s[3], s[4]], [s[5], s[6], s[7]], [s[8], s[9], s[10]]]))
                .collect();
            nrc_joint::noise_from_samples(samples)
        }
    };
    noise.tolerance = nrc_joint::tolerance(detail);
    if let Some(t) = started {
        eprintln!("  gpu joint noise model: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    }
    let sigma = p.passes.last().map_or(0.0, |q| q.sigma);
    let plan = nrc_joint::scale_plan(sigma, w, h);
    // rows a kept pixel depends on (the same reach a CPU window is grown by)
    let halo = nrc_joint::reach(&plan);
    // Memory: the whole output (3 values per pixel) is held besides the input; a band then holds about
    // 12 values per pixel (guide 3, two chroma buffers 2 + 2, paths 2, weights 2; up to 12 while the
    // speck brake and the guide run). The budget for both together is `band_budget_from` the device's
    // memory where its backend reports it (`lcc_gpumem`, `Gpu::vram`), else a fixed 2 GB;
    // a budget pinned with `set_nr_band_budget`, else `<PREFIX>_NR_GPU_BAND_MB`, overrides it.
    let pinned = match NR_BAND_BUDGET.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        bytes => Some(bytes),
    };
    let from_env = || lcc_brand::env("NR_GPU_BAND_MB").and_then(|v| v.trim().parse::<usize>().ok()).map(|mb| mb.saturating_mul(1 << 20));
    let pinned_used = budget.is_none() && pinned.is_some();
    let fixed = budget.or(pinned).or_else(from_env);
    let renders = RENDERS.load(std::sync::atomic::Ordering::Relaxed);
    let memory = crate::ctx::memory();
    let measured = cx.gpu.vram.map(|vram| band_budget_from(vram, memory.allocated, memory.pooled, renders));
    let budget = fixed.or(measured).unwrap_or(FIXED_BAND_BUDGET);
    if profiling() {
        eprintln!(
            "  gpu joint budget: {} MB ({}, {renders} render(s) on the device)",
            budget >> 20,
            if pinned_used {
                "pinned"
            } else if fixed.is_some() {
                "NR_GPU_BAND_MB"
            } else if measured.is_some() {
                "device memory"
            } else {
                "fixed default"
            }
        );
    }
    let out_bytes = 12 * n;
    let per_row = 48 * w;
    // no band may exceed the device's largest buffer either (3 values per pixel are the largest)
    let limit_rows = (usize::try_from(cx.gpu.limit()).unwrap_or(usize::MAX) / (12 * w)).max(1);
    let rows = (budget.saturating_sub(out_bytes) / per_row).min(limit_rows);
    let core = if rows >= h {
        h
    } else if rows > 2 * halo {
        rows - 2 * halo
    } else {
        // not even one band with its halo fits: this stage runs on the CPU (the rest of the render stays here)
        return None;
    };
    let out = cx.gpu.buffer(3 * n);
    let mut y0 = 0;
    while y0 < h {
        let y1 = (y0 + core).min(h);
        let (b0, b1) = if core >= h { (0, h) } else { (y0.saturating_sub(halo), (y1 + halo).min(h)) };
        let filtered = joint_band(cx, img, w, b0, b1 - b0, &noise, brake, &plan);
        let kept = (y1 - y0) * w;
        nrcj(cx, "nrcj_recon", kept, &[kept as u32, strength.to_bits(), (y0 * w) as u32, (b0 * w) as u32], img, Some(&filtered), &out);
        drop(filtered);
        // wait: the driver frees a dropped buffer only once the GPU is done with it, and a band's
        // buffers are large (without waiting, freed memory piled up to the whole device's)
        cx.sync();
        y0 = y1;
    }
    Some(out)
}

/// The filtered chroma pairs of rows `b0 .. b0 + bh` of the image, treated as an image of their own.
#[allow(clippy::too_many_arguments)]
fn joint_band(
    cx: &mut Cx<'_>,
    img: &Buf,
    w: usize,
    b0: usize,
    bh: usize,
    noise: &lcc_pipeline::nrc_joint::JointNoise,
    brake: lcc_pipeline::nrc_joint::SpeckBrake,
    plan: &[(usize, f32)],
) -> Buf {
    let n = w * bh;
    let mut np = vec![n as u32, w as u32, bh as u32];
    np.extend(noise.model.alpha.iter().chain(&noise.model.beta).chain(&noise.ymax).map(|v| v.to_bits()));
    np.push(noise.tolerance.to_bits());
    let mut yuv = cx.gpu.buffer(3 * n);
    nrcj(cx, "nrcj_yuv", n, &[n as u32, (b0 * w) as u32], img, None, &yuv);
    if brake.share > 0.0 {
        let mut sp = np.clone();
        sp.extend([brake.threshold.to_bits(), brake.share.to_bits()]);
        let despecked = cx.gpu.buffer(3 * n);
        nrcj(cx, "nrcj_speck", n, &sp, &yuv, None, &despecked);
        yuv = despecked;
    }
    let blurred = gaussian(cx, &yuv, w, bh, 3, 0.8);
    let guide = cx.gpu.buffer(3 * n);
    nrcj(cx, "nrcj_pilot", n, &np, &yuv, Some(&blurred), &guide);
    drop(blurred);
    let mut filtered = cx.gpu.buffer(2 * n);
    nrcj(cx, "nrcj_uv", n, &[n as u32], &yuv, None, &filtered);
    drop(yuv);
    // dropped buffers only become reusable (and their memory free) once the GPU is done with them
    cx.sync();
    // paths (exp(−path), as on the CPU) and pair weights; the next scale's paths go to the scratch buffer,
    // free after the diffusion steps, as on the CPU
    let mut paths = cx.gpu.buffer(2 * n);
    nrcj(cx, "nrcj_links1", n, &np, &guide, None, &paths);
    let links = cx.gpu.buffer(2 * n);
    let mut lp = np.clone();
    lp.push(1);
    nrcj(cx, "nrcj_links", n, &lp, &guide, Some(&paths), &links);
    let mut scratch = cx.gpu.buffer(2 * n);
    for (k, &(delta, rate)) in plan.iter().enumerate() {
        let dp = [n as u32, w as u32, bh as u32, delta as u32, rate.to_bits()];
        for _ in 0..2 {
            nrcj(cx, "nrcj_dh", n, &dp, &filtered, Some(&links), &scratch);
            std::mem::swap(&mut filtered, &mut scratch);
            nrcj(cx, "nrcj_dv", n, &dp, &filtered, Some(&links), &scratch);
            std::mem::swap(&mut filtered, &mut scratch);
        }
        if let Some(&(next, _)) = plan.get(k + 1) {
            nrcj(cx, "nrcj_paths", n, &[n as u32, w as u32, bh as u32, delta as u32], &paths, None, &scratch);
            std::mem::swap(&mut paths, &mut scratch);
            let mut lp = np.clone();
            lp.push(next as u32);
            nrcj(cx, "nrcj_links", n, &lp, &guide, Some(&paths), &links);
        }
    }
    drop((scratch, paths, links, guide));
    cx.sync();
    filtered
}

/// The planes the per-pixel stage reads.
struct Prep {
    log_l: Arc<Buf>,
    base: Arc<Buf>,
    clarity: Option<Arc<Buf>>,
    texture: Option<Arc<Buf>>,
    sharpen: Option<Arc<Buf>>,
    dark: Option<Arc<Buf>>,
    /// Blurred chromaticity (local Moiré / Noise).
    chroma: Option<Arc<Buf>>,
    air: f32,
}

/// The spatial planes (`local::prepare`), reusing those in `planes`.
fn prepare(cx: &mut Cx<'_>, lin: &Buf, plan: &Plan<'_>, req: &RenderRequest, planes: &mut Planes) -> Prep {
    let (w, h) = (plan.w, plan.h);
    let n = w * h;
    let sig = local::plane_sigmas(&plan.settings, plan.px_per_long, plan.frame.ow.max(plan.frame.oh), req.quality);
    let log_l = match &planes.log_l {
        Some(b) => b.clone(),
        None => {
            let l = cx.gpu.buffer(n);
            map(cx, "log_lum_k", n, &[], [Some(lin), None, None], &l);
            let b = Arc::new(l);
            planes.log_l = Some(b.clone());
            b
        }
    };
    let base = match sig.base {
        Some(sg) => {
            let key = local::base_key(&plan.settings, sg, sig.base_detail);
            plane_at(&mut planes.base, key, || tone_base(cx, &log_l, w, h, sg, sig.base_detail))
        }
        None => log_l.clone(),
    };
    let clarity = sig.clarity.map(|sg| plane_at(&mut planes.clarity, sg.to_bits(), || guided_fast(cx, &log_l, w, h, sg, local::CLARITY_EPS)));
    let texture = sig.texture.map(|sg| plane_at(&mut planes.texture, sg.to_bits(), || gaussian(cx, &log_l, w, h, 1, sg)));
    let sharpen = sig.sharpen.map(|sg| plane_at(&mut planes.sharpen, sg.to_bits(), || gaussian_fir(cx, &log_l, w, h, sg)));
    let (dark, air) = match sig.dark {
        None => (None, 1.0),
        Some(sg) => match &planes.dark {
            Some((k, d, air)) if *k == local::dark_key(sg, sig.dark_patch) => (Some(d.clone()), *air),
            _ => {
                let d0 = cx.gpu.buffer(n);
                map(cx, "dark_k", n, &[], [Some(lin), None, None], &d0);
                let d0 = min_hv(cx, d0, w, h, sig.dark_patch);
                let d = gaussian(cx, &d0, w, h, 1, sg);
                // airlight: a percentile of every 7th value, as on the CPU
                let m = n.div_ceil(local::AIRLIGHT_STEP);
                let sub = cx.gpu.buffer(m);
                map(cx, "subsample", m, &[local::AIRLIGHT_STEP as u32], [Some(&d), None, None], &sub);
                let air = local::airlight_of(cx.read(&sub, m));
                cx.flush();
                let b = Arc::new(d);
                planes.dark = Some((local::dark_key(sg, sig.dark_patch), b.clone(), air));
                (Some(b), air)
            }
        },
    };
    let chroma = sig.chroma.map(|sg| {
        plane_at(&mut planes.chroma, sg.to_bits(), || {
            let ch = cx.gpu.buffer(n * 3);
            map(cx, "chroma_k", n, &[], [Some(lin), None, None], &ch);
            gaussian(cx, &ch, w, h, 3, sg)
        })
    });
    Prep { log_l, base, clarity, texture, sharpen, dark, chroma, air }
}

/// Most brush dabs the mask kernel takes (more: the CPU rasterizes the brush).
const MAX_DABS: usize = 65_536;
/// Most words of brush data (stroke records, dab centres, tile lists): indices are stored as
/// `f32`, exact below 2²⁴.
const MAX_BRUSH_WORDS: usize = 8 << 20;
/// Words per stroke record in [`brush_aux`].
const STROKE_WORDS: usize = 20;

/// The `shape` kernel's brush data for `dabs` in a `w × h` output, or `None` when it would be too
/// big (the CPU rasterizes the brush then): stroke records ([`STROKE_WORDS`] words: dab offset, dab
/// count, r, hard, flow, density, erase, bbox x0 y0 x1 y1, auto, Lightroom model (1/0), its dab profile's a and p, Auto Mask range scale, tile table offset, tiles across,
/// tiles down, tile size), then each stroke's dab centres, tile table and tile lists.
///
/// Each stroke's dabs are binned into square tiles (about a quarter radius across): a pixel looks
/// only at the dabs that reach its tile, in stroke order, instead of every dab of the stroke. A
/// dress brushed over at 100 % zoom (thousands of dabs, millions of pixels) kept the GPU busy for
/// tens of seconds (TEST-NOTES 2026-10-16 item 28).
fn brush_aux(dabs: &[&lcc_pipeline::masks::BrushDabs], erase: bool, w: usize, h: usize) -> Option<Vec<f32>> {
    let mut head = Vec::with_capacity(STROKE_WORDS * dabs.len());
    let mut tail: Vec<f32> = Vec::new();
    let base = STROKE_WORDS * dabs.len();
    for d in dabs {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for q in &d.dabs {
            (x0, y0, x1, y1) = (x0.min(q.x - d.r), y0.min(q.y - d.r), x1.max(q.x + d.r), y1.max(q.y + d.r));
        }
        let off = base + tail.len();
        tail.extend(d.dabs.iter().flat_map(|q| [q.x as f32, q.y as f32]));
        // tiles of about a quarter radius: a dab reaches ~9 × 9 of them, a pixel looks at less than
        // twice the dabs that touch it
        let ts = (d.r / 4.0).round().clamp(8.0, 256.0);
        let ts = if ts.is_finite() { ts } else { 256.0 };
        let (tx, ty) = (((w as f64) / ts).ceil().max(1.0) as usize, ((h as f64) / ts).ceil().max(1.0) as usize);
        let mut lists: Vec<Vec<u32>> = vec![Vec::new(); tx.checked_mul(ty)?];
        let mut entries = 0usize;
        for (j, q) in d.dabs.iter().enumerate() {
            let reaches = q.x + d.r > 0.0 && q.x - d.r < w as f64 && q.y + d.r > 0.0 && q.y - d.r < h as f64;
            if !reaches {
                continue;
            }
            let j = u32::try_from(j).ok()?;
            let cell = |v: f64, n: usize| ((v / ts).floor().max(0.0) as usize).min(n - 1);
            let (cx0, cx1, cy0, cy1) = (cell(q.x - d.r, tx), cell(q.x + d.r, tx), cell(q.y - d.r, ty), cell(q.y + d.r, ty));
            entries = entries.saturating_add((cx1 - cx0 + 1) * (cy1 - cy0 + 1));
            if entries > MAX_BRUSH_WORDS {
                return None;
            }
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    lists.get_mut(cy * tx + cx)?.push(j);
                }
            }
        }
        let table = base + tail.len();
        let mut start = table + 2 * lists.len();
        for l in &lists {
            tail.extend([start as f32, l.len() as f32]);
            start += l.len();
        }
        for l in &lists {
            tail.extend(l.iter().map(|j| *j as f32));
        }
        if base + tail.len() > MAX_BRUSH_WORDS {
            return None;
        }
        head.extend([off as f32, d.dabs.len() as f32, d.r as f32, d.hard as f32, d.flow, d.density, (erase && d.erase) as u8 as f32]);
        head.extend([x0, y0, x1, y1].map(|v| v as f32));
        head.push(d.auto as u8 as f32);
        let (a, p) = d.lrc.unwrap_or((0.0, 0.0));
        head.extend([d.lrc.is_some() as u8 as f32, a as f32, p as f32, d.auto_scale]);
        head.extend([table as f32, tx as f32, ty as f32, ts as f32]);
    }
    head.extend(tail);
    Some(head)
}

/// How [`guided_cross`] combines the filtered plane with its input.
#[derive(Clone, Copy)]
enum Combine {
    /// The larger of the two (Auto Mask strokes).
    Max,
    /// The input moved towards the filtered plane by this much (Refine Edges).
    Mix(f32),
}

/// Cross guided filter (`masks::guided_cross`) of `p` steered by `guide`, combined with `p`.
#[allow(clippy::too_many_arguments)]
fn guided_cross(cx: &mut Cx<'_>, guide: &Buf, p: &Buf, w: usize, h: usize, sigma: f32, eps: f32, how: Combine) -> Buf {
    let n = w * h;
    let (m0, m1) = (cx.gpu.buffer(2 * n), cx.gpu.buffer(2 * n));
    map(cx, "xguided_pre", n, &[0], [Some(guide), Some(p), None], &m0);
    map(cx, "xguided_pre", n, &[1], [Some(guide), Some(p), None], &m1);
    let (b0, b1) = (gaussian(cx, &m0, w, h, 2, sigma), gaussian(cx, &m1, w, h, 2, sigma));
    let ab = cx.gpu.buffer(2 * n);
    map(cx, "xguided_ab", n, &[eps.to_bits()], [Some(&b0), Some(&b1), None], &ab);
    let ab = gaussian(cx, &ab, w, h, 2, sigma);
    let q = cx.gpu.buffer(n);
    let mode = match how {
        Combine::Max => [0, 0],
        Combine::Mix(k) => [1, k.to_bits()],
    };
    map(cx, "xguided_apply", n, &mode, [Some(guide), Some(&ab), Some(p)], &q);
    q
}

/// Where an Auto Mask stroke is rendered: the photo, the plan (its pixels' keys), the render's
/// [`crate::strokes::pass`] and how the stroke combines with the ones before it (`combine`'s op).
struct StrokeAt<'p, 'a> {
    src: &'p Arc<Rgb32f>,
    plan: &'p Plan<'a>,
    pass: u64,
    op: u32,
    /// The frame strokes are kept for ([`crate::strokes::view`]).
    view: u64,
    /// The mask is placed on the photo before its warp (its strokes land elsewhere in the frame).
    on_photo: bool,
}

/// What an Auto Mask stroke combined into the plane took from the store: blocks taken, blocks
/// it covers.
#[derive(Clone, Copy, Default)]
struct StrokeUse {
    reused: usize,
    blocks: usize,
}

/// A rectangle (x0, y0, x1, y1; exclusive ends).
type Rect = (usize, usize, usize, usize);

fn meet(a: Rect, b: Rect) -> Option<Rect> {
    let r = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    (r.0 < r.2 && r.1 < r.3).then_some(r)
}

/// One Auto Mask stroke `d` (the stroke `st`, its brush data `aux`, the `shape` parameters `q`)
/// combined into `brush`: stamped, refined over the rectangle it can reach
/// (`masks::auto_stroke_reach`, as the CPU does), and kept in 16 bits for the next render
/// ([`crate::strokes`]) in blocks of the whole frame, so the other windows of the frame (a zoomed
/// view's tiles) take the blocks they share from there and work out only the rest.
///
/// A block is kept only when the rectangle refined holds every pixel the refine reads for it (its
/// support, [`refine_support`], inside the rectangle or at the stroke's own reach): what this
/// window worked out there is then what any window of the frame does. Blocks near the window's
/// edge, in its margin, are used here and not kept.
///
/// Each stroke worked out is submitted on its own and the render waits for the one before it, so
/// the window's frames slot in between: a dozen big strokes queued at once, each refined over the
/// whole window, held the interface back for over a second at 100 %.
#[allow(clippy::too_many_arguments)]
fn auto_stroke(
    cx: &mut Cx<'_>,
    d: &lcc_pipeline::masks::BrushDabs,
    st: &lcc_develop::BrushStroke,
    aux: &[f32],
    q: &[u32],
    lin: &Buf,
    log_l: &Buf,
    stroke: &Buf,
    brush: &Buf,
    at: &StrokeAt<'_, '_>,
) -> StrokeUse {
    use crate::strokes::{BLOCK, Kept};
    use lcc_pipeline::masks::{auto_refine, auto_stroke_reach, auto_stroke_reach_in, auto_stroke_sigma};
    let (w, h) = (at.plan.w, at.plan.h);
    // (a stroke that reaches no pixel of this plane paints nothing)
    let Some(local) = auto_stroke_reach(d, w, h) else { return StrokeUse::default() };
    // this plane in the whole frame, and the stroke's reach there (at least what it reaches here)
    let ff = &at.plan.filter_frame;
    let (ox, oy) = (ff.x as usize, ff.y as usize);
    let need = (local.0 + ox, local.1 + oy, local.2 + ox, local.3 + oy);
    let (fw, fh) = ((ff.full_w as usize).max(ox + w), (ff.full_h as usize).max(oy + h));
    let whole = auto_stroke_reach_in(d, (ox as f64, oy as f64), fw, fh).unwrap_or(need);
    let whole = (whole.0.min(need.0), whole.1.min(need.1), whole.2.max(need.2), whole.3.max(need.3));
    let generation = cx.gpu.generation;
    let batch = crate::gate::is_batch();
    // the stroke itself, not its dabs (their positions in a window carry its rounding)
    let stroke_key = {
        use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
        let brush = (
            d.r.to_bits(),
            d.hard.to_bits(),
            d.flow.to_bits(),
            d.density.to_bits(),
            d.auto_scale.to_bits(),
            d.lrc.map(|(a, p)| (a.to_bits(), p.to_bits())),
        );
        BuildHasherDefault::<DefaultHasher>::default().hash_one((format!("{st:?}"), d.dabs.len(), brush, d.erase, at.op, at.on_photo))
    };
    // blocks this window worked out up to its own edge, which only it can use
    let loose = crate::strokes::loose_key(at.view, stroke_key, (at.plan.geo, at.plan.lin_key, need));
    // the blocks of the frame this plane needs: each one's part of the stroke, and of this plane
    let mut blocks: Vec<(u64, Rect, Rect, Option<(Rect, Arc<Kept>)>)> = Vec::new();
    let mut cells: Vec<(usize, usize)> = Vec::new();
    for by in need.1 / BLOCK..need.3.div_ceil(BLOCK) {
        for bx in need.0 / BLOCK..need.2.div_ceil(BLOCK) {
            let cell = (bx * BLOCK, by * BLOCK, (bx + 1) * BLOCK, (by + 1) * BLOCK);
            let (Some(part), Some(here)) = (meet(cell, whole), meet(cell, need)) else { continue };
            let key = crate::strokes::key(at.view, stroke_key, bx, by);
            let kept = crate::strokes::get(at.src, key, at.pass, generation, batch).filter(|k| k.rect == part);
            let kept = kept.map(|k| (part, k)).or_else(|| {
                let edge = crate::strokes::key(at.view, loose, bx, by);
                crate::strokes::get(at.src, edge, at.pass, generation, batch).filter(|k| k.rect == here).map(|k| (here, k))
            });
            blocks.push((key, part, here, kept));
            cells.push((bx, by));
        }
    }
    let mut used = StrokeUse { reused: blocks.iter().filter(|b| b.3.is_some()).count(), blocks: blocks.len() };
    // the blocks not kept: worked out over their bounding box and the refine's support around it
    let missing = blocks.iter().filter(|b| b.3.is_none()).map(|b| b.2).reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)));
    let fresh = missing.and_then(|m| {
        let sigma = auto_stroke_sigma(d);
        let support = refine_support(sigma);
        let grown = (m.0.saturating_sub(support), m.1.saturating_sub(support), m.2.saturating_add(support), m.3.saturating_add(support));
        let c = meet(grown, need)?;
        let (rw, rh) = (c.2 - c.0, c.3 - c.1);
        let rn = rw * rh;
        let pr = [w as u32, (c.0 - ox) as u32, (c.1 - oy) as u32, rw as u32, rh as u32];
        crate::strokes::note_refined();
        let cap = crate::strokes::store_bytes(cx.gpu.vram);
        let aux = cx.gpu.upload(aux);
        cx.run("shape", q, &[Some(lin), Some(log_l), Some(&aux), Some(stroke), Some(brush)], groups2(w, h, [16, 16]));
        let cut = |cx: &mut Cx<'_>, plane: &Buf| {
            let out = cx.gpu.buffer(rn);
            cx.run("crop", &pr, &[Some(plane), None, Some(&out)], groups2(rw, rh, [16, 16]));
            out
        };
        let s_r = cut(cx, stroke);
        // (a Lightroom-model stroke keeps its pixel-sharp cut)
        let q_r = if d.lrc.is_none() {
            let l_r = cut(cx, log_l);
            let (sigma, eps) = auto_refine(d.r);
            guided_cross(cx, &l_r, &s_r, rw, rh, sigma, eps, Combine::Max)
        } else {
            s_r
        };
        let packed = cx.gpu.buffer(rn.div_ceil(2));
        cx.run("pack16", &[rn as u32], &[Some(&q_r), Some(&packed), None], groups1(rn.div_ceil(2)));
        let packed = Arc::new(Kept { rect: c, packed });
        // keep the blocks whose refine saw all it reads: each side inside the rectangle by the
        // support, or the stroke's own edge (not kept when the render failed: its buffers may
        // hold nothing)
        if !crate::ctx::failed() {
            // `to`: where each side may stop instead (the stroke's reach, give or take `slack`,
            // for a block any window may use; this window's own edge for one only it may use)
            let sees = |p: Rect, to: Rect, slack: usize| {
                p.0 >= c.0
                    && p.1 >= c.1
                    && p.2 <= c.2
                    && p.3 <= c.3
                    && (c.0 <= to.0 + slack || p.0 >= c.0 + support)
                    && (c.1 <= to.1 + slack || p.1 >= c.1 + support)
                    && (c.2 + slack >= to.2 || p.2 + support <= c.2)
                    && (c.3 + slack >= to.3 || p.3 + support <= c.3)
            };
            for (bx_by, (key, part, here, kept)) in cells.iter().zip(&mut blocks) {
                if kept.is_some() {
                    continue;
                }
                // (a block this window saw too little around: what it worked out there is kept
                // for this window, so its next render refines nothing)
                let (key, part, loose) = if sees(*part, whole, REACH_SLACK) {
                    (*key, *part, false)
                } else if sees(*here, need, 0) {
                    (crate::strokes::key(at.view, loose, bx_by.0, bx_by.1), *here, true)
                } else {
                    continue;
                };
                let (bw, bh) = (part.2 - part.0, part.3 - part.1);
                let bn = bw * bh;
                let one = cx.gpu.buffer(bn);
                cx.run(
                    "crop",
                    &[rw as u32, (part.0 - c.0) as u32, (part.1 - c.1) as u32, bw as u32, bh as u32],
                    &[Some(&q_r), None, Some(&one)],
                    groups2(bw, bh, [16, 16]),
                );
                let bpacked = cx.gpu.buffer(bn.div_ceil(2));
                cx.run("pack16", &[bn as u32], &[Some(&one), Some(&bpacked), None], groups1(bn.div_ceil(2)));
                let k = Arc::new(Kept { rect: part, packed: bpacked });
                if loose {
                    crate::strokes::put_loose(at.src, key, at.view, at.pass, batch, cap, k);
                } else {
                    crate::strokes::put(at.src, key, at.view, at.pass, batch, cap, k);
                }
            }
        }
        cx.pace();
        Some((c, packed))
    });
    // every block's part of this plane, from the store or from what was just worked out
    for (_, _, here, kept) in &blocks {
        let (src_rect, packed) = match (kept, &fresh) {
            (Some((rect, k)), _) => (*rect, &k.packed),
            (None, Some((c, p))) if meet(*here, *c) == Some(*here) => (*c, &p.packed),
            // (outside what was worked out: nothing the stroke paints here)
            _ => {
                used.blocks = used.blocks.saturating_sub(1);
                continue;
            }
        };
        let (rw, rh) = (here.2 - here.0, here.3 - here.1);
        let pc = [
            w as u32,
            (here.0 - ox) as u32,
            (here.1 - oy) as u32,
            rw as u32,
            rh as u32,
            d.density.to_bits(),
            at.op,
            (src_rect.2 - src_rect.0) as u32,
            (here.0 - src_rect.0) as u32,
            (here.1 - src_rect.1) as u32,
        ];
        cx.run("combine16", &pc, &[None, Some(packed), Some(brush)], groups2(rw, rh, [16, 16]));
    }
    used
}

/// Pixels short of a stroke's reach (`masks::auto_stroke_reach`) a window may stop and still count
/// as reaching its edge: the reach ends two pixels past what the refine reads (dab extent and six
/// sigmas), and a window grown for the stroke (`spots::window_for_reads`: two and a half radii)
/// stops there, give or take a pixel of rounding.
const REACH_SLACK: usize = 4;

/// Pixels around a pixel the Auto Mask refine (`guided_cross` at `sigma`) reads: two Gaussians,
/// each three box passes, plus a pixel of slack.
fn refine_support(sigma: f32) -> usize {
    if sigma <= 0.0 {
        return 0;
    }
    let one: usize = lcc_raster::blur::box_radii(sigma).iter().sum();
    one.saturating_mul(2).saturating_add(1)
}

/// Evaluate the masks (`lcc_pipeline::masks::evaluate`): their alpha planes (concatenated,
/// followed by the blurred chromaticity when local Moiré / Noise need it; kept with the view's
/// planes, [`MaskBufs`]), their adjustment terms and their local Curves. Shapes without a kernel (Sky, Subject, …) run on the CPU.
#[allow(clippy::too_many_arguments)]
fn masks(
    cx: &mut Cx<'_>,
    src: &Arc<Rgb32f>,
    lin: &Buf,
    prep: &Prep,
    plan: &Plan<'_>,
    host: &mut Host,
    planes: &mut Planes,
) -> (Option<Arc<MaskAlpha>>, Vec<[f32; MASK_TERMS]>, Vec<Option<[Lut1; 3]>>) {
    let s = &*plan.settings;
    // as the CPU path keeps them: the first `max_masks`, so over-cap documents render the same
    let list: Vec<_> =
        s.masks.iter().filter(|m| m.visible && !m.components.is_empty()).take(lcc_pipeline::masks::max_masks(plan.w, plan.h)).collect();
    if list.is_empty() {
        return (None, Vec::new(), Vec::new());
    }
    let mut terms: Vec<_> = list.iter().map(|m| mask_terms(&m.adjust)).collect();
    lcc_pipeline::finish::with_tone(&mut terms, &plan.mask_tone);
    let curves = list.iter().map(|m| local_curve_luts(&m.curve)).collect();
    let key = lcc_pipeline::masks::alpha_key(&s.masks, s.light.exposure as f32, plan.w, plan.h, plan.mattes.as_deref());
    let chroma = planes.chroma.as_ref().filter(|_| prep.chroma.is_some()).map(|(k, _)| *k);
    if let Some(m) = planes.masks.as_ref().filter(|m| m.fits(key, chroma, plan.mattes.as_ref())) {
        return (Some(m.alpha.clone()), terms, curves);
    }
    let alpha = Arc::new(mask_alpha(cx, src, lin, prep, plan, host, &list));
    planes.masks = Some(MaskBufs { key, chroma, mattes: plan.mattes.clone(), alpha: alpha.clone() });
    (Some(alpha), terms, curves)
}

/// The alpha planes of the visible masks `list`: concatenated, then the blurred chromaticity, or
/// one buffer per mask when together they exceed the device's buffer limit ([`MaskAlpha`]).
#[allow(clippy::too_many_arguments)]
fn mask_alpha(
    cx: &mut Cx<'_>,
    src: &Arc<Rgb32f>,
    lin: &Buf,
    prep: &Prep,
    plan: &Plan<'_>,
    host: &mut Host,
    list: &[&lcc_develop::Mask],
) -> MaskAlpha {
    let n = plan.w * plan.h;
    let joined = n.checked_mul(list.len()).and_then(|v| v.checked_add(if prep.chroma.is_some() { 3 * n } else { 0 }));
    match joined.filter(|&len| cx.gpu.fits(len)) {
        Some(len) => {
            let alpha = cx.zeroed(len);
            if let Some(ch) = &prep.chroma {
                cx.copy_into(ch, &alpha, n * list.len());
            }
            let comp_plane = cx.gpu.buffer(n);
            for (mi, m) in list.iter().enumerate() {
                mask_plane(cx, src, lin, prep, plan, host, m, &alpha, mi, &comp_plane);
            }
            MaskAlpha::Joined(alpha)
        }
        None => {
            let comp_plane = cx.gpu.buffer(n);
            let planes = list
                .iter()
                .map(|m| {
                    let alpha = cx.zeroed(n);
                    mask_plane(cx, src, lin, prep, plan, host, m, &alpha, 0, &comp_plane);
                    alpha
                })
                .collect();
            MaskAlpha::Split { planes, chroma: prep.chroma.clone() }
        }
    }
}

/// Evaluate mask `m` into plane `mi` of `alpha` (`comp_plane`: scratch for one component).
#[allow(clippy::too_many_arguments)]
fn mask_plane(
    cx: &mut Cx<'_>,
    src: &Arc<Rgb32f>,
    lin: &Buf,
    prep: &Prep,
    plan: &Plan<'_>,
    host: &mut Host,
    m: &lcc_develop::Mask,
    alpha: &Buf,
    mi: usize,
    comp_plane: &Buf,
) {
    use lcc_develop::{MaskOp, MaskShape};
    let s = &*plan.settings;
    let (w, h) = (plan.w, plan.h);
    let n = w * h;
    let frame = &plan.frame;
    let ev = s.light.exposure as f32;
    let long = frame.ow.max(frame.oh);
    let [a, b, c, d, e, f] = frame.out_to_norm(w, h).0;
    let (kx, ky) = (frame.ow / long, frame.oh / long);
    let aff = [a * kx, b * ky, c * kx, d * ky, e * kx, f * ky].map(|v| (v as f32).to_bits());
    {
        // its coordinates on the photo before the warp (`masks::evaluate_one`): through a lens or
        // perspective warp the kernels' affine positions don't hold, so its gradients run on the CPU
        let mframe = plan.frame.for_mask(m.on_photo);
        let frame = &*mframe;
        let warped = frame.mask_warp().is_some();
        let mut first = true;
        for comp in &m.components {
            let op = match comp.op {
                _ if first => 0u32,
                MaskOp::Add => 1,
                MaskOp::Subtract => 2,
                MaskOp::Intersect => 3,
            };
            first = false;
            let mut p = vec![w as u32, h as u32, 0, comp.invert as u32];
            p.extend_from_slice(&aff);
            let f32s = |v: &[f64]| v.iter().map(|x| (*x as f32).to_bits()).collect::<Vec<u32>>();
            let mut aux: Vec<f32> = Vec::new();
            let mut own: Option<Buf> = None;
            let kind = match &comp.shape {
                MaskShape::Linear { .. } | MaskShape::Radial { .. } if warped => None,
                MaskShape::Linear { start, end } => {
                    let (a, b) = (frame.norm_to_long(*start), frame.norm_to_long(*end));
                    let d = b - a;
                    p.extend(f32s(&[a.x, a.y, d.x, d.y, d.dot(d).max(1e-12)]));
                    Some(0)
                }
                MaskShape::Radial { center, rx, ry, angle, feather, invert } => {
                    let c = frame.norm_to_long(*center);
                    let (sn, co) = (-angle.to_radians()).sin_cos();
                    p.extend(f32s(&[c.x, c.y, co, sn, rx.max(1e-6), ry.max(1e-6), (*feather / 100.0).clamp(0.0, 1.0)]));
                    p.push(*invert as u32);
                    Some(1)
                }
                MaskShape::LuminanceRange { lo, hi, lo_feather, hi_feather } => {
                    p.extend(f32s(&[*lo, *hi, lo_feather.max(1e-3), hi_feather.max(1e-3)]));
                    p.push(ev.to_bits());
                    Some(2)
                }
                MaskShape::ColorRange { samples, refine, .. } => {
                    let tol = lcc_pipeline::masks::color_range_tolerance(*refine);
                    p.extend([tol.to_bits(), ev.exp2().to_bits(), samples.len() as u32]);
                    aux.extend(samples.iter().flat_map(|s| s.map(|v| v as f32)));
                    Some(3)
                }
                MaskShape::Brush { strokes } => {
                    // The kernel takes at most MAX_DABS across the whole mask, so no more than
                    // that (plus one, to tell "at it" from "past it") is ever built: each stroke
                    // may otherwise construct two million dabs before the sum below rejects them
                    // all (#1520).
                    let mut room = MAX_DABS + 1;
                    let dabs: Vec<_> = strokes
                        .iter()
                        .map(|st| {
                            let d = lcc_pipeline::masks::brush_dabs_within(st, frame, w, h, room);
                            room -= d.dabs.len().min(room);
                            d
                        })
                        .collect();
                    // pen pressure varies radius and flow per dab, which the kernel doesn't take: the CPU rasterizes it
                    let auxes: Option<Vec<Vec<f32>>> =
                        if dabs.iter().map(|d| d.dabs.len()).sum::<usize>() > MAX_DABS || dabs.iter().any(|d| !d.pressure.is_empty()) {
                            None
                        } else if dabs.iter().any(|d| d.auto) {
                            dabs.iter().map(|d| brush_aux(&[d], false, w, h)).collect()
                        } else {
                            brush_aux(&dabs.iter().collect::<Vec<_>>(), true, w, h).map(|a| vec![a])
                        };
                    if auxes.is_none() {
                        None
                    } else if dabs.iter().any(|d| d.auto) {
                        // Auto Mask: stroke by stroke (each one refined before it's combined)
                        let brush = cx.zeroed(n);
                        let stroke = cx.gpu.buffer(n);
                        let pass = crate::strokes::pass();
                        let view = crate::strokes::view(plan.frame_lin_key, cx.gpu.generation);
                        let (mut kept, mut auto) = (StrokeUse::default(), 0);
                        for ((d, aux), st) in dabs.iter().zip(auxes.unwrap_or_default()).zip(strokes) {
                            // a Lightroom-model stroke adds up with the ones before it (`masks::combine_stroke`)
                            let op = if d.erase {
                                2
                            } else if d.lrc.is_some() {
                                4
                            } else {
                                1
                            };
                            let mut q = p.clone();
                            q[2] = 4;
                            q[3] = 0;
                            q.push(1);
                            if d.auto {
                                let at = StrokeAt { src, plan, pass, op, view, on_photo: m.on_photo };
                                let u = auto_stroke(cx, d, st, &aux, &q, lin, &prep.log_l, &stroke, &brush, &at);
                                kept.reused += u.reused;
                                kept.blocks += u.blocks;
                                auto += 1;
                                continue;
                            }
                            let aux = cx.gpu.upload(&aux);
                            cx.run("shape", &q, &[Some(lin), Some(&prep.log_l), Some(&aux), Some(&stroke), Some(&brush)], groups2(w, h, [16, 16]));
                            cx.run("combine", &[n as u32, d.density.to_bits(), 0, op], &[None, None, None, Some(&stroke), Some(&brush)], groups1(n));
                        }
                        if profiling() {
                            eprintln!(
                                "  gpu auto mask strokes: {auto}, {} of {} blocks kept from before ({w}×{h} at {}, {}; {} MB kept; batch {})",
                                kept.reused,
                                kept.blocks,
                                plan.filter_frame.x,
                                plan.filter_frame.y,
                                crate::strokes::bytes() >> 20,
                                crate::gate::is_batch()
                            );
                        }
                        if comp.invert {
                            cx.run("finalize", &[n as u32, 0, 0, 1, 1f32.to_bits()], &[None, None, None, Some(&stroke), Some(&brush)], groups1(n));
                        }
                        own = Some(brush);
                        None
                    } else {
                        p.push(dabs.len() as u32);
                        aux = auxes.and_then(|a| a.into_iter().next()).unwrap_or_default();
                        Some(4)
                    }
                }
                _ => None,
            };
            let plane = match kind {
                _ if own.is_some() => own,
                Some(k) => {
                    p[2] = k;
                    let aux = (!aux.is_empty()).then(|| cx.gpu.upload(&aux));
                    cx.run("shape", &p, &[Some(lin), Some(&prep.log_l), aux.as_ref(), Some(comp_plane), Some(alpha)], groups2(w, h, [16, 16]));
                    None
                }
                None => {
                    // no kernel: evaluate on the CPU
                    let img = host.lin.get_or_insert_with(|| Arc::new(cx.read_rgb(lin, w, h))).clone();
                    let l = host.log_l.get_or_insert_with(|| Arc::new(cx.read_plane(&prep.log_l, w, h))).clone();
                    let mut v = lcc_pipeline::masks::shape_alpha(&comp.shape, frame, w, h, &img, &l, ev, plan.mattes.as_deref());
                    if comp.invert {
                        v.data.iter_mut().for_each(|x| *x = 1.0 - *x);
                    }
                    Some(cx.gpu.upload(&v.data))
                }
            };
            let cbuf = plane.as_ref().unwrap_or(comp_plane);
            let k = [n as u32, 0, mi as u32, op];
            // An inverted mask is inverted again at the end, so combine on its visible side:
            // invert the plane, combine, invert back. Op 0 is the plain copy of the first
            // component (see `combine` in mask.wgsl); ops 1..=3 are add, subtract, intersect.
            // `finalize` params: [n, 0, mask index, invert = 1, amount = 1.0 (no scaling)];
            // it reads and writes only `alpha`.
            let flip = m.invert && op != 0;
            let fk = [n as u32, 0, mi as u32, 1, 1f32.to_bits()];
            if flip {
                cx.run("finalize", &fk, &[None, None, None, None, Some(alpha)], groups1(n));
            }
            cx.run("combine", &k, &[None, None, None, Some(cbuf), Some(alpha)], groups1(n));
            if flip {
                cx.run("finalize", &fk, &[None, None, None, None, Some(alpha)], groups1(n));
            }
        }
        // Refine Edges and Feather (`masks::evaluate_one`): on the combined alpha, before the
        // mask's invert and amount
        if m.refine > 0.0 || m.feather > 0.0 {
            let mut plane = cx.slice(alpha, mi * n, n);
            if m.refine > 0.0 {
                let k = (m.refine / 100.0).clamp(0.0, 1.0) as f32;
                let sigma = (0.04 * frame.output_long(w, h) as f32 * k).max(1.0);
                plane = guided_cross(cx, &prep.log_l, &plane, w, h, sigma, 0.02, Combine::Mix(k.sqrt()));
            }
            if m.feather > 0.0 {
                let k = (m.feather / 100.0).clamp(0.0, 1.0) as f32;
                plane = gaussian(cx, &plane, w, h, 1, 0.02 * frame.output_long(w, h) as f32 * k);
            }
            cx.copy_into(&plane, alpha, mi * n);
        }
        let amt = (m.adjust.amount / 100.0) as f32;
        cx.run("finalize", &[n as u32, 0, mi as u32, m.invert as u32, amt.to_bits()], &[None, None, None, Some(comp_plane), Some(alpha)], groups1(n));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_develop::DevelopSettings;

    /// The colour NR of processes V1–V7 (`local::colour_nr`) on the device matches the CPU: the 3 × 3
    /// chromaticity median exactly (it is a selection), the whole step to float rounding. The median's
    /// insertion sort over a private array came out wrong on Vulkan, where Windows' compute moved with
    /// issue #934 (up to 81 8-bit levels in the equivalence tests); DX12 had hidden it.
    #[test]
    fn colour_nr_before_v8_matches_the_cpu() {
        let Some(gpu) = crate::device() else {
            lcc_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        let _scope = crate::ctx::RenderScope::new(&gpu);
        // odd sizes (a last workgroup part full), hashed colour noise with some negative values
        let (w, h) = (97usize, 61usize);
        let hash = |x: usize, y: usize, c: usize| {
            let mut v = (x as u32).wrapping_mul(0x9e37_79b9) ^ (y as u32).wrapping_mul(0x85eb_ca6b) ^ (c as u32 + 1).wrapping_mul(0xc2b2_ae35) | 1;
            v ^= v << 13;
            v ^= v >> 17;
            v ^= v << 5;
            v as f32 / u32::MAX as f32
        };
        let img = Rgb32f::from_fn(w, h, |x, y| [0, 1, 2].map(|c| hash(x, y, c) * 0.4 - 0.02));
        let n = w * h;
        let mut cx = Cx::new(&gpu);
        let src = gpu.upload(rgb_words(&img));
        let med = cx.gpu.buffer(n * 3);
        map(&mut cx, "chroma_med", n, &[w as u32], [Some(&src), None, None], &med);
        let gpu_med = cx.read_rgb(&med, w, h);
        let chroma = Rgb32f::from_fn(w, h, |x, y| {
            let c = img.get(x, y).map(|v| v.max(0.0));
            let yl = lcc_color::luminance_2020(c).max(1e-6);
            c.map(|v| v / yl)
        });
        for y in 0..h {
            for x in 0..w {
                let mut v = [[0.0f32; 9]; 3];
                for (k, (dx, dy)) in (-1..=1isize).flat_map(|dy| (-1..=1isize).map(move |dx| (dx, dy))).enumerate() {
                    let c = chroma.get_clamped(x as isize + dx, y as isize + dy);
                    for ch in 0..3 {
                        v[ch][k] = c[ch];
                    }
                }
                for ch in 0..3 {
                    v[ch].sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let (want, got) = (v[ch][4], gpu_med.get(x, y)[ch]);
                    assert!((want - got).abs() <= 1e-6 * want.abs().max(1.0), "median at ({x}, {y}) channel {ch}: GPU {got}, CPU {want}");
                }
            }
        }
        // the whole step, as `denoise` runs it
        let mut s = DevelopSettings { process: lcc_develop::Process::V7.version(), ..Default::default() };
        s.detail.nr_color = 25.0;
        let Some(nr) = local::nr_params(&s, 1.0).1 else { panic!("Color 25 has parameters") };
        let clamped = cx.gpu.buffer(n * 3);
        map(&mut cx, "clamp0_k", n, &[], [Some(&src), None, None], &clamped);
        let blurred = gaussian(&mut cx, &clamped, w, h, 3, nr.sigma);
        let out = cx.gpu.buffer(n * 3);
        map(
            &mut cx,
            "nr_col",
            n,
            &[nr.t.to_bits(), local::SPECKLE_LO.to_bits(), local::SPECKLE_HI.to_bits()],
            [Some(&src), Some(&med), Some(&blurred)],
            &out,
        );
        let gpu_out = cx.read_rgb(&out, w, h);
        let mut cpu = img.clone();
        local::denoise_with(&mut cpu, &s, 1.0, false, None);
        let worst = gpu_out.data.iter().zip(&cpu.data).flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs())).fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "colour NR V7, GPU vs CPU: {worst}");
    }

    /// Runs `upload_with` on a thread whose upload takes 800 ms, and returns how long `read` took
    /// on this thread while that upload ran.
    fn read_during_slow_upload(
        upload_with: impl FnOnce(Box<dyn FnOnce() -> Buf + Send>) + Send + 'static,
        read: impl FnOnce(),
    ) -> std::time::Duration {
        let (started, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            upload_with(Box::new(move || {
                let _ = started.send(());
                std::thread::sleep(std::time::Duration::from_millis(800));
                Buf::detached(16)
            }))
        });
        wait.recv().expect("the upload started");
        let t = std::time::Instant::now();
        read();
        let took = t.elapsed();
        worker.join().expect("the upload finished");
        took
    }

    /// The UI thread reads a view's stage size every frame (`trim_stages`) and for `ui.inspect`;
    /// it waited for the whole upload of the view's source. It never waits now.
    #[test]
    fn a_views_size_is_read_at_once_while_its_source_uploads() {
        let stages = Arc::new(GpuStages::default());
        let src = Arc::new(Rgb32f::new(4, 4));
        let (s, src2) = (stages.clone(), src.clone());
        let took = read_during_slow_upload(
            move |upload| {
                s.source(&src2, upload);
            },
            || {
                let _ = stages.bytes();
            },
        );
        assert!(took < std::time::Duration::from_millis(400), "the size waited {took:?} for the upload");
        assert_eq!(stages.bytes(), 64, "the uploaded source counts once it is there");
        let again = stages.source(&src, || Buf::detached(99));
        assert_eq!(again.len, 16, "the same photo is not uploaded again");
    }

    /// The same for the one shared big source: `ui.inspect` blocked on it when a view first zoomed
    /// to 100 %, and the idle trim (UI thread) takes it too.
    #[test]
    fn the_shared_sources_size_is_read_at_once_while_it_uploads() {
        let src = Arc::new(Rgb32f::new(4, 4));
        let src2 = src.clone();
        let took = read_during_slow_upload(
            move |upload| {
                let _ = shared_source(&src2, 0, upload);
            },
            || {
                let _ = shared_source_bytes();
            },
        );
        assert!(took < std::time::Duration::from_millis(400), "the size waited {took:?} for the upload");
        assert_eq!(shared_source(&src, 0, || Buf::detached(99)).len, 16, "kept for the photo's other views");
        release_shared_source();
    }

    #[test]
    fn coverage_mask_matches_per_pixel_decision() {
        use lcc_develop::{EmbeddedLens, EmbeddedWarp};
        let lens = EmbeddedLens {
            warp: Some(EmbeddedWarp { planes: [[1.0, -0.03, 0.01, 0.0, 0.001, -0.002]; 3], center: lcc_geom::Point::new(0.52, 0.48), radius: 0.6 }),
            vignette: None,
            stages: Default::default(),
        };
        let mut s = DevelopSettings::default();
        s.optics.lens_profile = true;
        s.optics.distortion = -40.0;
        s.geometry.horizontal = -15.0;
        s.geometry.rotate = 3.0;
        for orientation in [Orientation::Normal, Orientation::Rotate270] {
            s.orientation = orientation;
            let f = lcc_pipeline::geometry::Frame::with_lens(900, 600, &s, true, Some(&lens));
            let wp = f.warp.as_ref().expect("warp");
            // widths that do and don't fill the last word; heights that do and don't fill the last band
            for (w, h) in [f.fit(700, 700), (333, 221), (64, 16), (1, 1)] {
                let o2t = f.out_to_oriented(w, h);
                let cov = wp.coverage(&o2t, w, h);
                let mask = cov.words();
                let words = w.div_ceil(32);
                assert_eq!(mask.len(), words * h);
                for y in 0..h {
                    for x in 0..w {
                        let bit = mask[y * words + x / 32] >> (x % 32) & 1 == 1;
                        assert_eq!(bit, wp.covers(&o2t, x, y), "{orientation:?} {w}x{h}: pixel {x},{y}");
                    }
                    let pad = mask[y * words + words - 1] >> (w % 32);
                    assert!(w.is_multiple_of(32) || pad == 0, "padding bits set");
                }
            }
        }
    }

    /// The perspective horizon in view: its denominator is exactly 0 on output row 75 (clamped to 1e-300) and
    /// changes sign there, so the blocks around it are undecided (clamp, and division by a range that may hold
    /// zero) and are decided per pixel, while blocks clear of it are filled at once.
    #[test]
    fn coverage_mask_with_the_horizon_in_view() {
        use lcc_geom::{Affine, Homography};
        let (w, h) = (900, 600);
        let mut wp = lcc_pipeline::optics::Warp::identity(w as f64, h as f64);
        // centred coordinates: v = (y − 300) / 450, denominator 2·v + m8, exactly 0 at y = 75.5
        let m8 = -((75.5 - 300.0) / 450.0 * 2.0);
        wp.persp_inv = Homography([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0, m8]);
        wp.persp = wp.persp_inv.inverse().expect("invertible");
        let o2t = Affine::IDENTITY;
        assert_eq!(wp.block_coverage(&o2t, 0, 32, 64, 80), None, "the block holding the horizon");
        assert_eq!(wp.block_coverage(&o2t, 0, 32, 0, 16), Some(false), "beyond the horizon");
        assert_eq!(wp.block_coverage(&o2t, 448, 480, 288, 304), Some(true), "the centre");
        let cov = wp.coverage(&o2t, w, h);
        let mask = cov.words();
        let words = w.div_ceil(32);
        let mut covered = 0;
        for y in 0..h {
            for x in 0..w {
                let bit = mask[y * words + x / 32] >> (x % 32) & 1 == 1;
                assert_eq!(bit, wp.covers(&o2t, x, y), "pixel {x},{y}");
                covered += bit as usize;
            }
        }
        assert!(covered > 0 && covered < w * h, "{covered} px covered");
    }

    /// A box pass costs a bounded number of reads per pixel at any radius (large blurs once made a
    /// single dispatch long enough to trip a driver watchdog).
    #[test]
    fn box_pass_work_per_pixel_is_bounded_for_any_radius() {
        for horizontal in [true, false] {
            for r in [0usize, 1, 5, 31, 32, 64, 120, 1000, 100_000] {
                let p = box_plan(horizontal, r);
                assert!(p.reads_per_px <= 2.0 * BOX_H_SLIDE_FROM as f32 + 1.0, "{horizontal} r={r}: {p:?}");
                assert!(p.chunk >= 1);
            }
        }
    }

    /// Two exports side by side: each band plan leaves room for the other render, and our own buffers in
    /// use count against the device's memory (pooled ones are reused).
    #[test]
    fn band_budget_leaves_room_for_every_render_on_the_device() {
        let vram = 8000u64 << 20;
        let alone = band_budget_from(vram, 600 << 20, 200 << 20, 1);
        assert_eq!(alone, (4000usize << 20) - (400 << 20) - VRAM_RESERVE as usize);
        assert_eq!(band_budget_from(vram, 600 << 20, 200 << 20, 0), alone, "the render itself is always counted");
        assert_eq!(band_budget_from(vram, 600 << 20, 200 << 20, 2), alone - VRAM_RESERVE as usize);
        assert_eq!(band_budget_from(100 << 20, 0, 0, 3), 0, "nothing left: no band fits, the CPU runs the step");
        assert_eq!(band_budget_from(vram, 0, 1 << 40, 1), (4000usize << 20) - VRAM_RESERVE as usize, "more pooled than allocated");
    }

    /// An interactive render never waits for the bands a batch render holds (that one may be waiting for
    /// the interface to go quiet: both would hang); it runs the step on the CPU instead. It does wait
    /// for another interactive render's, and a batch render waits for any.
    #[test]
    fn an_interactive_render_never_waits_for_a_batch_renders_bands() {
        use std::sync::mpsc::channel;
        use std::time::Duration;
        // a lock of its own: other tests here take `BANDS`
        static TEST_BANDS: Bands = Bands::new();
        let on = |batch: bool, f: Box<dyn FnOnce() + Send>| std::thread::spawn(move || if batch { crate::gate::as_batch(f) } else { f() });
        let hold = |batch: bool| {
            let (taken_tx, taken_rx) = channel();
            let (release_tx, release_rx) = channel::<()>();
            let holder = on(
                batch,
                Box::new(move || {
                    let guard = TEST_BANDS.take();
                    let _ = taken_tx.send(guard.is_some());
                    let _ = release_rx.recv();
                }),
            );
            assert_eq!(taken_rx.recv_timeout(Duration::from_secs(5)), Ok(true), "the first render takes the bands");
            (holder, release_tx)
        };
        let ask = |batch: bool| {
            let (tx, rx) = channel();
            let asker = on(
                batch,
                Box::new(move || {
                    let _ = tx.send(TEST_BANDS.take().is_some());
                }),
            );
            (asker, rx)
        };

        // a batch render holds them: an interactive one goes on without, at once
        let (holder, release) = hold(true);
        let (asker, got) = ask(false);
        let answer = got.recv_timeout(Duration::from_secs(5));
        let _ = release.send(());
        assert_eq!(answer, Ok(false), "the interactive render runs the step on the CPU");
        assert!(asker.join().is_ok() && holder.join().is_ok());

        // otherwise the asker waits its turn, then takes them
        for (holder_batch, asker_batch) in [(false, false), (false, true), (true, true)] {
            let (holder, release) = hold(holder_batch);
            let (asker, got) = ask(asker_batch);
            let case = format!("holder batch {holder_batch}, asker batch {asker_batch}");
            assert!(got.recv_timeout(Duration::from_millis(200)).is_err(), "{case}: waits while they are held");
            let _ = release.send(());
            assert_eq!(got.recv_timeout(Duration::from_secs(5)), Ok(true), "{case}: takes them once free");
            assert!(asker.join().is_ok() && holder.join().is_ok());
        }
    }

    /// The joint colour NR on the device in bands of rows (a small budget) gives the result of one piece
    /// and of the CPU; a budget too small for one band with its halo returns `None` (the CPU runs it).
    #[test]
    fn joint_colour_nr_bands_match_one_piece_and_the_cpu() {
        let Some(gpu) = crate::device() else {
            lcc_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        let _scope = crate::ctx::RenderScope::new(&gpu);
        let (w, h) = (300usize, 700usize);
        // colour regions with hashed chroma noise
        let noise = |x: usize, y: usize, c: usize| {
            let mut v = (x as u32).wrapping_mul(0x9e37_79b9) ^ (y as u32).wrapping_mul(0x85eb_ca6b) ^ (c as u32 + 1).wrapping_mul(0xc2b2_ae35) | 1;
            v ^= v << 13;
            v ^= v >> 17;
            v ^= v << 5;
            (v as f32 / u32::MAX as f32 - 0.5) * 0.01
        };
        let img = Rgb32f::from_fn(w, h, |x, y| {
            let base = if x < 150 {
                [0.05, 0.03, 0.02]
            } else if y < 300 {
                [0.02, 0.04, 0.06]
            } else {
                [0.03; 3]
            };
            [base[0] + noise(x, y, 0), base[1] + noise(x, y, 1), base[2] + noise(x, y, 2)]
        });
        let mut s = DevelopSettings { process: lcc_develop::Process::V8.version(), ..Default::default() };
        s.detail.nr_color = 100.0;
        s.detail.nr_color_smoothness = 0.0;
        let p = local::nrc_params(&s, 1.0).expect("Color 100 has parameters");
        let (detail, amount) = local::nrc_sliders(&s);
        let mut cpu = img.clone();
        local::denoise_with(&mut cpu, &s, 1.0, false, None);
        let mut cx = Cx::new(&gpu);
        let src = gpu.upload(rgb_words(&img));
        let mut run = |budget: usize| color_nr_joint(&mut cx, &src, w, h, &p, detail, amount, None, Some(budget)).map(|b| cx.read_rgb(&b, w, h));
        let whole = run(1 << 30).expect("one piece fits 1 GB");
        // (the halo at Color 100 is 168 rows: a band of 420 rows holds 84 of its own, so 9 bands)
        let banded = run(12 * w * h + 48 * w * 420).expect("bands of 420 rows fit");
        assert!(run(12 * w * h + 48 * w * 8).is_none(), "8 rows can't hold a band and its halo");
        let worst =
            |a: &Rgb32f, b: &Rgb32f| a.data.iter().zip(&b.data).flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs())).fold(0.0f32, f32::max);
        assert!(worst(&whole, &banded) < 1e-3, "bands vs one piece: {}", worst(&whole, &banded));
        assert!(worst(&whole, &cpu) < 1e-3, "GPU vs CPU: {}", worst(&whole, &cpu));
        // where a loupe tile falls back to the CPU (`Bands::take`) it sits beside GPU tiles: no visible seam,
        // at most one 8-bit level apart once encoded
        let levels = |a: &Rgb32f, b: &Rgb32f| {
            let enc = |v: f32| lcc_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0;
            a.data.iter().zip(&b.data).flat_map(|(p, q)| (0..3).map(move |c| (enc(p[c]) - enc(q[c])).abs())).fold(0.0f32, f32::max)
        };
        eprintln!("GPU vs CPU: {} linear, {} 8-bit levels", worst(&whole, &cpu), levels(&whole, &cpu));
        assert!(levels(&whole, &cpu) < 1.0, "GPU vs CPU: {} levels", levels(&whole, &cpu));
    }

    /// The blur against `lcc_raster::blur::gaussian`: widths that do and don't fill the last
    /// workgroup, windows wider than the image, and 1, 2 (as two planes) and 3 channels.
    #[test]
    fn gaussian_matches_cpu() {
        use lcc_raster::blur;
        let Some(gpu) = crate::device() else {
            lcc_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        let _scope = crate::ctx::RenderScope::new(&gpu);
        let v = |x: usize, y: usize, c: usize| ((x * 7 + y * 13 + c * 5) % 17) as f32 / 16.0 + if x % 61 < 3 { 2.0 } else { 0.0 };
        for (w, h) in [(1000, 5), (257, 3), (5, 4), (1, 9)] {
            for sigma in [0.8f32, 2.0, 4.5, 30.0, 250.0, 1000.0] {
                let planes: Vec<Plane> = (0..3).map(|c| Plane::from_fn(w, h, |x, y| v(x, y, c))).collect();
                let want: Vec<Plane> = planes.iter().map(|p| blur::gaussian(p, sigma)).collect();
                for nc in [1, 2, 3] {
                    let words: Vec<f32> = (0..w * h).flat_map(|i| (0..nc).map(move |c| (i, c))).map(|(i, c)| planes[c].data[i]).collect();
                    let mut cx = Cx::new(&gpu);
                    let src = gpu.upload(&words);
                    let out = gaussian(&mut cx, &src, w, h, nc, sigma);
                    let got: Vec<f32> = cx.read(&out, w * h * nc);
                    for (i, g) in got.iter().enumerate() {
                        let want = want[i % nc].data[i / nc];
                        assert!((g - want).abs() < 1e-5 * (1.0 + sigma / 10.0), "{w}×{h} nc={nc} sigma={sigma}: word {i} {g} vs {want}");
                    }
                }
            }
        }
    }

    #[test]
    fn orient_map_matches_image_oriented() {
        use Orientation::*;
        let img = Rgb32f::from_fn(5, 3, |x, y| [x as f32, y as f32, 0.0]);
        for o in [Normal, Rotate90, Rotate180, Rotate270, FlipH, Transverse, FlipV, Transpose] {
            let r = img.oriented(o);
            let m = orient_map(o, 5, 3);
            for y in 0..r.height {
                for x in 0..r.width {
                    let (x, y) = (x as i32, y as i32);
                    let (sx, sy) = (m[0] * x + m[1] * y + m[2], m[3] * x + m[4] * y + m[5]);
                    assert_eq!(r.get(x as usize, y as usize), img.get(sx as usize, sy as usize), "{o:?} at {x},{y}");
                }
            }
        }
    }
}
