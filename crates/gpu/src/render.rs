//! The GPU renderer: the CPU pipeline's stages, evaluated on the device where a kernel exists and
//! on the CPU otherwise (per-stage hybrid). Stage results stay on the device and are cached per
//! view exactly like [`lightcraft_pipeline::StageCache`] (same keys, see [`lightcraft_pipeline::Plan`]).

use std::sync::{Arc, Mutex};

use lightcraft_develop::DevelopSettings;
use lightcraft_geom::Orientation;
use lightcraft_pipeline::finish::{FinishParams, FinishParamsInput, MASK_TERMS, mask_terms};
use lightcraft_pipeline::geometry::SampleMode;
use lightcraft_pipeline::local::plane_at;
use lightcraft_pipeline::masks::{AlphaCache, AlphaKeys, alpha_key};
use lightcraft_pipeline::{Overlay, PixelWindow, Plan, RenderRequest, Rendered, SourceInfo, local};
use lightcraft_raster::resample::Filter;
use lightcraft_raster::{Histogram, Plane, Rgb32f, Rgba8};

use crate::ctx::{Buf, FailKind, Gpu, fail, groups1, groups2};
use crate::params::{Present, finish_block};

/// Device-resident stages of one view, kept next to the CPU [`lightcraft_pipeline::StageCache`]
/// (as its extension). Holds the last two output sizes, like the CPU cache.
#[derive(Default)]
pub struct GpuStages {
    airlight: lightcraft_pipeline::airlight::AirlightCache,
    airlight_frame: lightcraft_pipeline::airlight::FrameCache<AirlightFrame>,
    entries: Mutex<Vec<Entry>>,
    /// The uploaded source (a view renders one photo at several sizes).
    source: Mutex<Option<(Arc<Rgb32f>, Arc<Buf>)>>,
}

const CAPACITY: usize = 2;

#[derive(Clone)]
struct Entry {
    src: Arc<Rgb32f>,
    geo: u64,
    sampled: Arc<Buf>,
    lin: Option<(u64, Arc<Buf>)>,
    full_airlight: Option<u64>,
    planes: Planes,
    /// Alphas of the last render's masks, as [`lightcraft_pipeline::masks::AlphaCache`] keeps them
    /// on the CPU (same keys).
    masks: AlphaCache<Buf>,
}

/// Spatial planes of one linear image (`key` = its `lin_key`), each tagged with its radius.
#[derive(Clone, Default)]
struct Planes {
    key: u64,
    log_l: Option<Arc<Buf>>,
    base: Option<(local::BaseKey, Arc<Buf>)>,
    clarity: Option<(u32, Arc<Buf>)>,
    texture: Option<(u32, Arc<Buf>)>,
    dark: Option<(u32, Arc<Buf>, f32)>,
    chroma: Option<(u32, Arc<Buf>)>,
}

impl GpuStages {
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

    fn source(&self, src: &Arc<Rgb32f>, upload: impl FnOnce() -> Buf) -> Arc<Buf> {
        let mut g = self.source.lock().unwrap_or_else(|e| e.into_inner());
        match &*g {
            Some((s, b)) if Arc::ptr_eq(s, src) => b.clone(),
            _ => {
                *g = None; // free the previous photo's buffer first
                let b = Arc::new(upload());
                *g = Some((src.clone(), b.clone()));
                b
            }
        }
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
        let mut total = 0;
        let mut add = |b: &Arc<Buf>| {
            let p = Arc::as_ptr(b);
            if !seen.contains(&p) {
                seen.push(p);
                total += b.len * 4;
            }
        };
        if let Some(full) = self.airlight_frame.value() {
            if let Some(sampled) = &full.sampled {
                add(sampled);
            }
            add(&full.lin);
            add(&full.dark);
        }
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
            p.dark.iter().for_each(|(_, b, _)| add(b));
            p.chroma.iter().for_each(|(_, b)| add(b));
            e.masks.planes().for_each(&mut add);
        }
        if let Some((_, b)) = &*self.source.lock().unwrap_or_else(|e| e.into_inner()) {
            add(b);
        }
        total
    }

    /// Drop everything (e.g. when memory is needed).
    pub fn clear(&self) {
        self.trim_images();
        self.airlight.clear();
    }

    /// Drop device buffers while retaining the small source-keyed airlight cache.
    pub fn trim_images(&self) {
        self.lock().clear();
        self.airlight_frame.clear();
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
}

/// Kernel invocations recorded before a render submits them (under one full-size pass over a
/// 24 MP export, several passes over a preview): no single submission runs long.
const FLUSH_INVOCATIONS: u64 = 16 << 20;

/// Pixels per dispatch of the per-pixel stage (the heaviest kernel).
const BAND_PIXELS: usize = 4 << 20;

impl<'a> Cx<'a> {
    pub fn new(gpu: &'a Gpu) -> Cx<'a> {
        Cx { gpu, enc: Some(gpu.encoder()), pending: 0 }
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

    /// Submit what is recorded so far without waiting. Renders submit stage by stage and every
    /// [`FLUSH_INVOCATIONS`]: long submissions are what GPU watchdogs reset (Windows TDR, the
    /// i915 hang check; issue #78's iGPU spent seconds on an export recorded as one submission),
    /// and buffers dropped by earlier work become reusable by later work (lower peak memory).
    pub fn flush(&mut self) {
        self.pending = 0;
        if let Some(enc) = self.enc.take() {
            self.gpu.submit([enc.finish()]);
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

    /// Copy a rectangular set of rows without resampling or reading it back.
    fn crop(&mut self, b: &Buf, width: usize, nc: usize, rect: PixelWindow) -> Arc<Buf> {
        let out = self.gpu.buffer(rect.w * rect.h * nc);
        let enc = self.enc.get_or_insert_with(|| self.gpu.encoder());
        for y in 0..rect.h {
            let src_offset = ((rect.y + y) * width + rect.x) * nc * 4;
            let dst_offset = y * rect.w * nc * 4;
            enc.copy_buffer_to_buffer(b.raw(), src_offset as u64, out.raw(), dst_offset as u64, (rect.w * nc * 4) as u64);
        }
        Arc::new(out)
    }

    /// `len` values of `b` from `offset` (both in 32-bit words), as a new buffer (recorded).
    pub fn slice(&mut self, b: &Buf, offset: usize, len: usize) -> Buf {
        let out = self.gpu.buffer(len);
        let enc = self.enc.get_or_insert_with(|| self.gpu.encoder());
        enc.copy_buffer_to_buffer(b.raw(), (offset * 4) as u64, out.raw(), 0, (len * 4) as u64);
        out
    }

    /// Copy all of `src` into `dst` at `offset` (32-bit words; recorded).
    pub fn copy_into(&mut self, src: &Buf, dst: &Buf, offset: usize) {
        let enc = self.enc.get_or_insert_with(|| self.gpu.encoder());
        enc.copy_buffer_to_buffer(src.raw(), 0, dst.raw(), (offset * 4) as u64, (src.len * 4) as u64);
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
// Filters (twins of `lightcraft_raster::blur` / `resample` and `lightcraft_pipeline::local`)

/// Rows each `box_v` thread slides its running sum over (at least; longer for large radii, so the
/// priming sum stays a small share of the work).
const CHUNK: usize = 16;

/// One box pass of radius `r` (clamped edges) over a `w × h` image of `nc` interleaved channels,
/// `src → dst`: horizontal or vertical.
fn box_pass(cx: &mut Cx<'_>, horizontal: bool, src: &Buf, dst: &Buf, (w, h): (usize, usize), nc: usize, r: usize) {
    let chunk = (2 * r + 1).clamp(CHUNK, 128);
    let p = [w as u32, h as u32, nc as u32, r as u32, chunk as u32];
    let (k, groups) = if horizontal { ("box_h", groups2(w, h, [64, 4])) } else { ("box_v", groups2(w, h.div_ceil(chunk), [64, 4])) };
    cx.run(k, &p, &[Some(src), Some(dst)], groups);
}

/// Gaussian blur of a `w × h` image of `nc` interleaved channels: three box passes each way
/// (`lightcraft_raster::blur::gaussian`).
pub(crate) fn gaussian(cx: &mut Cx<'_>, src: &Buf, w: usize, h: usize, nc: usize, sigma: f32) -> Buf {
    let Some(radii) = lightcraft_raster::blur::radii_for(sigma, w, h) else {
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

/// Consume the input and alternate it with one scratch buffer. The box kernels
/// and their pass order are identical to the borrowed blur.
fn gaussian_owned(cx: &mut Cx<'_>, mut src: Buf, w: usize, h: usize, nc: usize, sigma: f32) -> Buf {
    let Some(radii) = lightcraft_raster::blur::radii_for(sigma, w, h) else { return src };
    let mut scratch = cx.gpu.buffer(w * h * nc);
    for horizontal in [true, false] {
        for r in radii.into_iter().filter(|r| *r > 0) {
            box_pass(cx, horizontal, &src, &scratch, (w, h), nc, r);
            std::mem::swap(&mut src, &mut scratch);
        }
    }
    src
}

/// Resample taps of `lightcraft_raster::resample::resize`, packed for the `resize_*` kernels.
fn taps(src: usize, dst: usize, filter: Filter) -> Vec<u32> {
    let w = lightcraft_raster::resample::weights(src, dst, filter);
    pack_taps(&w)
}

fn pack_taps(w: &[(usize, Vec<f32>)]) -> Vec<u32> {
    let dst = w.len();
    let mut t = Vec::with_capacity(dst * 3);
    let mut weights = Vec::new();
    let base = dst * 3;
    for (lo, ws) in w {
        t.extend_from_slice(&[*lo as u32, ws.len() as u32, (base + weights.len()) as u32]);
        weights.extend(ws.iter().map(|v| v.to_bits()));
    }
    t.extend(weights);
    t
}

/// `lightcraft_raster::resample::resize` of an `nc`-channel image.
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

/// Windowed resampling uses exactly the CPU's full-frame coefficient grid.
fn resize_window(cx: &mut Cx<'_>, src: &Buf, (sw, sh): (usize, usize), nc: usize, taps: &local::ResampleTaps) -> Buf {
    let [wx, wy] = taps;
    let (dw, dh) = (wx.len(), wy.len());
    let tx = cx.gpu.upload(&pack_taps(wx));
    let tmp = cx.gpu.buffer(dw * sh * nc);
    cx.run("resize_h", &[sw as u32, sh as u32, dw as u32, nc as u32], &[Some(src), Some(&tx), Some(&tmp)], groups2(dw, sh, [16, 16]));
    let ty = cx.gpu.upload(&pack_taps(wy));
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
    drop(ab);
    cx.flush();
    q
}

/// Blurred (a, b) coefficients, interleaved.
fn guided_coeffs(cx: &mut Cx<'_>, p: &Buf, w: usize, h: usize, sigma: f32, eps: f32) -> Buf {
    let n = w * h;
    let pp = cx.gpu.buffer(2 * n);
    map(cx, "guided_pre", n, &[], [Some(p), None, None], &pp);
    let m = gaussian_owned(cx, pp, w, h, 2, sigma);
    let ab = cx.gpu.buffer(2 * n);
    map(cx, "guided_ab", n, &[eps.to_bits()], [Some(&m), None, None], &ab);
    drop(m);
    cx.flush();
    gaussian_owned(cx, ab, w, h, 2, sigma)
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

/// The highlights/shadows base (`local::tone_base`; `detail` 0: V1, no median).
fn tone_base(cx: &mut Cx<'_>, l: &Buf, plan: &Plan<'_>, sigma: f32, detail: usize) -> Buf {
    if detail == 0 {
        return guided_fast(cx, l, plan, sigma, local::BASE_EPS);
    }
    let m = median_hv(cx, l, plan.w, plan.h, detail);
    guided_fast(cx, &m, plan, sigma, local::BASE_EPS)
}

/// Fast guided filter (`local::guided_fast`): coefficients on a subsampled grid.
fn guided_fast(cx: &mut Cx<'_>, p: &Buf, plan: &Plan<'_>, sigma: f32, eps: f32) -> Buf {
    let (w, h) = (plan.w, plan.h);
    let s = local::guided_fast_step(sigma);
    if s <= 1 {
        return guided(cx, p, w, h, sigma, eps);
    }
    let up = if let Some(grid) = local::GuidedGrid::for_window(w, h, s, plan.guidance_view) {
        let (lw, lh) = (grid.down[0].len(), grid.down[1].len());
        let lo = resize_window(cx, p, (w, h), 1, &grid.down);
        let ab = guided_coeffs(cx, &lo, lw, lh, sigma / s as f32, eps);
        resize_window(cx, &ab, (lw, lh), 2, &grid.up)
    } else {
        let (lw, lh) = (w.div_ceil(s).max(1), h.div_ceil(s).max(1));
        let lo = resize(cx, p, (w, h), (lw, lh), 1, Filter::Box);
        let ab = guided_coeffs(cx, &lo, lw, lh, sigma / s as f32, eps);
        resize(cx, &ab, (lw, lh), (w, h), 2, Filter::Bilinear)
    };
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

fn affine_bits(a: &lightcraft_geom::Affine) -> [u32; 6] {
    a.0.map(|v| (v as f32).to_bits())
}

/// The `sample_warp` kernel's parameters (layout documented in `geom.wgsl`).
fn warp_params(
    wp: &lightcraft_pipeline::optics::Warp,
    base: (usize, usize),
    out: (usize, usize),
    o2t: &lightcraft_geom::Affine,
    sx: f64,
    sy: f64,
) -> Vec<u32> {
    let f = |v: f64| (v as f32).to_bits();
    let mut p = vec![base.0 as u32, base.1 as u32, out.0 as u32, out.1 as u32];
    p.extend(affine_bits(o2t));
    p.extend([f(sx), f(sy), f(wp.w), f(wp.h)]);
    let persp = wp.persp != lightcraft_geom::Homography::IDENTITY;
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
    debug_assert_eq!(p.len(), 65);
    p
}

/// Output rows per block of [`coverage_mask`]; a block is one mask word (32 px) wide. 16 rows measured
/// ~20 % faster than 8 at 6000 × 4000 (fewer interval evaluations, few more edge pixels).
const COVER_ROWS: usize = 16;

/// The largest source (bytes on the device) a view's stages keep uploaded between renders.
const RETAIN_SOURCE_BYTES: usize = 96 << 20;

/// Big sources uploaded to the shared copy so far (diagnostics and tests).
static SOURCE_UPLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The one big source (the original a zoom window is cut from) kept on the device for all the
/// views of the photo: the Before and After windows and every pan share it, instead of each
/// view's stages holding (or re-uploading) 288 MB of a 24 MP photo. It does not count as a view's
/// stages; it is released with the buffer pool when the app idles.
static SHARED_SOURCE: std::sync::Mutex<Option<(std::sync::Weak<Rgb32f>, Arc<Buf>)>> = std::sync::Mutex::new(None);

fn shared_source(src: &Arc<Rgb32f>, upload: impl FnOnce() -> Buf) -> Arc<Buf> {
    let mut g = SHARED_SOURCE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((weak, buf)) = &*g
        && weak.upgrade().is_some_and(|s| Arc::ptr_eq(&s, src))
    {
        return buf.clone();
    }
    // the previous photo's copy goes (on a render thread it is retired with the render's other
    // buffers, so for a moment both exist)
    *g = None;
    SOURCE_UPLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let buf = Arc::new(upload());
    *g = Some((Arc::downgrade(src), buf.clone()));
    buf
}

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

/// The reference framing decision ([`Warp::covers`](lightcraft_pipeline::optics::Warp::covers)) for every
/// output pixel, one bit per pixel, rows padded to 32-bit words. Blocks of 32 × [`COVER_ROWS`] pixels whose
/// interval bounds place them clearly inside or outside the image are filled at once
/// ([`Warp::block_coverage`](lightcraft_pipeline::optics::Warp::block_coverage), the same formulas evaluated
/// with outward-rounded intervals); only blocks near the image edge evaluate each pixel. Same bits as
/// evaluating every pixel.
fn coverage_mask(wp: &lightcraft_pipeline::optics::Warp, o2t: &lightcraft_geom::Affine, w: usize, h: usize) -> Vec<u32> {
    let words = w.div_ceil(32);
    let mut coverage = vec![0u32; words * h];
    if words == 0 {
        return coverage;
    }
    lightcraft_raster::par_rows(&mut coverage, words * COVER_ROWS, |band, rows| {
        let y0 = band * COVER_ROWS;
        let y1 = y0 + rows.len() / words;
        for word in 0..words {
            let (x0, x1) = (word * 32, (word * 32 + 32).min(w));
            let full = if x1 - x0 == 32 { u32::MAX } else { (1u32 << (x1 - x0)) - 1 };
            let block = wp.block_coverage(o2t, x0, x1, y0, y1);
            for (y, row) in (y0..y1).zip(rows.chunks_exact_mut(words)) {
                let Some(bits) = row.get_mut(word) else { continue };
                *bits = match block {
                    Some(true) => full,
                    Some(false) => 0,
                    None => (x0..x1).filter(|&x| wp.covers(o2t, x, y)).fold(0, |b, x| b | 1 << (x - x0)),
                };
            }
        }
    });
    coverage
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
        cx.run("orient", &p, &[Some(&src_buf), Some(&out), None], groups2(sp.ow, sp.oh, [16, 16]));
        Arc::new(out)
    };
    let (base, (bw, bh)) = match sp.prefilter {
        Some(d) => (Arc::new(resize(cx, &oriented, (sp.ow, sp.oh), d, 3, Filter::Mitchell)), d),
        None => (oriented, (sp.ow, sp.oh)),
    };
    let mut affine = |xf: &lightcraft_geom::Affine| {
        let out = cx.gpu.buffer(w * h * 3);
        let mut p = vec![bw as u32, bh as u32, w as u32, h as u32];
        p.extend(affine_bits(xf));
        cx.run("sample_affine", &p, &[Some(&base), Some(&out), None], groups2(w, h, [16, 16]));
        Arc::new(out)
    };
    match (&sp.mode, fr.warp.as_ref()) {
        (SampleMode::Copy, _) => base,
        (SampleMode::Affine(xf), _) => affine(xf),
        // `sample_plan` plans a warp only when there is one; without it the same mapping is affine
        (SampleMode::Warp(o2t), None) => affine(&(lightcraft_geom::Affine::scale(sp.sx, sp.sy) * *o2t)),
        (SampleMode::Warp(o2t), Some(wp)) => {
            let out = cx.gpu.buffer(w * h * 3);
            let p = warp_params(wp, (bw, bh), (w, h), o2t, sp.sx, sp.sy);
            // f32 rounding can cross a source edge. Keep the reference f64
            // framing decision in a bit mask; color sampling stays on the GPU.
            let coverage = cx.gpu.upload(&coverage_mask(wp, o2t, w, h));
            cx.run("sample_warp", &p, &[Some(&base), Some(&out), Some(&coverage)], groups2(w, h, [16, 16]));
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

fn sample_source(cx: &mut Cx<'_>, src: &Arc<Rgb32f>, info: &SourceInfo, plan: &Plan<'_>, stages: Option<&GpuStages>, host: &mut Host) -> Arc<Buf> {
    let gpu = cx.gpu;
    let (w, h) = (plan.w, plan.h);
    // A WarpRectilinear2 chain goes to the CPU: its degree-14 polynomial needs f64.
    if gpu.fits(src.data.len() * 3) && !plan.frame.warp.as_ref().is_some_and(|w| w.needs_f64()) {
        let upload = || gpu.upload(rgb_words(src));
        // A large source uses the shared copy; window stages keep only their sampled pixels.
        let src_buf = match stages {
            Some(c) if src.data.len() * 12 <= RETAIN_SOURCE_BYTES => c.source(src, upload),
            Some(_) => shared_source(src, upload),
            None => Arc::new(upload()),
        };
        sample(cx, src, src_buf, plan)
    } else {
        let img = plan.frame.sample(src, w, h);
        let b = Arc::new(gpu.upload(rgb_words(&img)));
        if lightcraft_pipeline::lin_needs_cpu(&plan.settings, info) {
            host.sampled = Some(img);
        }
        b
    }
}

pub(crate) fn profiling() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LIGHTCRAFT_PROFILE").is_some())
}

/// Render on `gpu`, reusing `stages` (if given). `None` (or a result the caller discards) when the
/// render failed: the reason is recorded with [`crate::ctx::fail`].
pub fn render(
    gpu: &Gpu,
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&GpuStages>,
    fault: Option<crate::Fault>,
) -> Option<Rendered> {
    let mut t = profiling().then(std::time::Instant::now);
    // Each stage is submitted on its own; under `LIGHTCRAFT_PROFILE` also waited for, so the
    // timings are real.
    let lap = |what: &str, t: &mut Option<std::time::Instant>, cx: &mut Cx<'_>| match t {
        Some(t) => {
            cx.sync();
            eprintln!("  gpu {what}: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
            *t = std::time::Instant::now();
        }
        None => cx.flush(),
    };
    let plan = lightcraft_pipeline::plan(src, info, s, req);
    let (w, h) = (plan.w, plan.h);
    let n = w * h;
    let _limit = match fault {
        Some(crate::Fault::Limit(bytes)) => Some(crate::ctx::LimitOverride::new(bytes)),
        _ => None,
    };
    if !gpu.fits(n * 3) {
        fail(
            FailKind::Limit,
            format!("{w}×{h} needs {} MiB buffers, over the device's {} MiB storage-buffer limit", (n * 12).div_ceil(1 << 20), gpu.limit() >> 20),
        );
        return None;
    }
    let full = plan.needs_airlight.then(|| lightcraft_pipeline::plan(src, info, s, &RenderRequest { window: None, ..*req }));
    if req.window.is_some()
        && let Err(why) = lightcraft_pipeline::airlight::check_pixels(w, h)
    {
        fail(FailKind::Limit, why);
        return None;
    }
    let cached = stages.and_then(|c| c.get(src, plan.geo)).filter(|e| e.full_airlight.is_some() == plan.needs_airlight);
    let mut prepared_work = None;
    let fixed_air = if let Some(full) = full.as_ref().filter(|full| req.is_bounded_window() || full.lin_key != plan.lin_key) {
        let budget = stages.map_or(lightcraft_pipeline::airlight::PREPARE_BUDGET, |c| c.airlight.budget());
        if req.is_bounded_window() {
            Some(lightcraft_pipeline::airlight::proxy(src, info, full))
        } else if req.is_draft_window() {
            Some(stages.and_then(|c| c.airlight.get(src, full.lin_key)).unwrap_or_else(|| lightcraft_pipeline::airlight::proxy(src, info, full)))
        } else if lightcraft_pipeline::airlight::check_prepare_budget(full, Some(&plan), budget, true).is_err()
            || lightcraft_pipeline::airlight::check_pixels(src.width, src.height).is_err()
        {
            if let Some(cache) = stages {
                cache.airlight_frame.clear();
            }
            Some(lightcraft_pipeline::airlight::proxy(src, info, full))
        } else {
            let saved = stages.and_then(|cache| cache.airlight_frame.get(src));
            // The full-frame statistic depends on the source and upstream settings,
            // not on a retained window entry. Keep cropping a matching preparation
            // when it survives, since that also saves the window's upstream work.
            let ready = stages.and_then(|cache| cache.airlight.get(src, full.lin_key)).filter(|_| {
                saved.as_ref().is_none_or(|(key, _)| *key != full.lin_key)
                    || cached
                        .as_ref()
                        .is_some_and(|e| e.full_airlight == Some(full.lin_key) && e.lin.as_ref().is_some_and(|(k, _)| *k == plan.lin_key))
            });
            if let Some(air) = ready {
                Some(air)
            } else {
                let prepared = match saved {
                    Some((key, value)) if key == full.lin_key => Ok((value.air, Some(value))),
                    other => {
                        let sampled = other.filter(|(_, data)| data.geo == full.geo).and_then(|(_, data)| data.sampled.clone());
                        if let Some(cache) = stages {
                            cache.airlight_frame.clear();
                        }
                        frame_airlight(gpu, src, info, full, &plan, stages, sampled).map(|(air, data)| {
                            let data = data.map(Arc::new);
                            if let (Some(cache), Some(data)) = (stages, &data) {
                                cache.airlight_frame.put(src, full.lin_key, data.clone(), data.bytes());
                            }
                            (air, data)
                        })
                    }
                };
                let (air, prepared) = match prepared {
                    Ok(value) => value,
                    Err(why) => {
                        fail(FailKind::Limit, why);
                        return None;
                    }
                };
                prepared_work = prepared.map(|data| data.window(gpu, &plan));
                if let Some(cache) = stages {
                    cache.airlight.remember(src, full.lin_key, air);
                }
                Some(air)
            }
        }
    } else {
        None
    };
    let s = &*plan.settings;
    let mut host = Host::default();
    let mut cx = Cx::new(gpu);
    lap("plan", &mut t, &mut cx);

    // 1. geometry (on the CPU when the source exceeds the device's buffer limit)
    let sampled = match (prepared_work.as_ref().and_then(|work| work.sampled.clone()), &cached) {
        (Some(sampled), _) => sampled,
        (_, Some(e)) => e.sampled.clone(),
        _ => sample_source(&mut cx, src, info, &plan, stages, &mut host),
    };
    lap("sample", &mut t, &mut cx);

    // 2. white balance, defringe, spots, noise reduction
    let lin = match prepared_work
        .as_ref()
        .map(|work| (plan.lin_key, work.lin.clone()))
        .or_else(|| cached.as_ref().and_then(|e| e.lin.clone()).filter(|(k, _)| *k == plan.lin_key))
    {
        Some((_, b)) => b,
        None => Arc::new(linear(&mut cx, &sampled, info, &plan, &mut host)),
    };
    lap("wb/nr", &mut t, &mut cx);

    // 3. spatial planes
    let mut alphas = cached.as_ref().map(|e| e.masks.clone()).unwrap_or_default();
    let mut planes = match cached.map(|e| e.planes) {
        Some(p) if p.key == plan.lin_key => p,
        _ => Planes { key: plan.lin_key, ..Default::default() },
    };
    if let Some(work) = prepared_work.take() {
        let sigma = (0.02 * plan.px_per_long as f32).max(1.0);
        planes.dark = Some((sigma.to_bits(), work.dark, fixed_air.unwrap_or(1.0)));
    }
    let mut prep = prepare(&mut cx, &lin, &plan, req, &mut planes);
    // a window dehazes with the whole frame's airlight
    if let Some(stages) = stages
        && !req.is_bounded_window()
        && !req.is_draft_window()
        && (req.window.is_none() || full.as_ref().is_some_and(|p| p.lin_key == plan.lin_key))
        && prep.dark.is_some()
    {
        stages.airlight.remember(src, plan.lin_key, prep.air);
    }
    if let Some(a) = fixed_air {
        prep.air = a;
    }
    lap("planes", &mut t, &mut cx);

    // 4. masks
    let (masks, terms) = masks(&mut cx, &lin, &prep, &plan, &mut host, &mut alphas);
    lap("masks", &mut t, &mut cx);
    // (a bounded window is a stand-in: its stages would only evict the exact window's)
    if let Some(c) = stages.filter(|_| !req.is_bounded_window()) {
        c.put(Entry {
            src: src.clone(),
            geo: plan.geo,
            sampled,
            lin: Some((plan.lin_key, lin.clone())),
            planes,
            masks: alphas,
            full_airlight: full.as_ref().map(|p| p.lin_key),
        });
    }

    // 5. per-pixel stage
    let mut fp = FinishParams::new(FinishParamsInput {
        settings: s,
        frame: &plan.frame,
        info,
        image_size: (w, h),
        px_per_long: plan.px_per_long,
        air_pre: prep.air,
        space: req.space,
    });
    // soft proofing as the CPU's `finish` sets it up (an output-device table never gets here: see `render`)
    fp.proof = req.proof.map(|pr| pr.params(req.space));
    if let Some(d) = &req.display {
        fp.for_display(d, req.proof);
    }
    let present = Present {
        clarity: prep.clarity.is_some(),
        texture: prep.texture.is_some(),
        dark: prep.dark.is_some(),
        chroma: masks.is_some() && prep.chroma.is_some(),
    };
    let finish_keep = plan.keep.filter(|_| req.overlay == Overlay::None);
    let crop = plan.keep.filter(|_| finish_keep.is_none());
    let output = finish_keep.unwrap_or(PixelWindow { x: 0, y: 0, w, h });
    let out_n = output.w * output.h;
    let (p, aux) = finish_block(&fp, &terms, &present, output);
    let aux = gpu.upload(&aux);
    // cleared, so that work which never ran cannot pass for an image (see the check below)
    let out = cx.zeroed(out_n);
    if fault == Some(crate::Fault::Validation) {
        // a dispatch the device rejects (too many workgroups)
        cx.run("box_h", &[0; 5], &[Some(&aux), Some(&out)], [gpu.device.limits().max_compute_workgroups_per_dimension + 1, 1, 1]);
    }
    // in bands of rows: each dispatch (and submission) stays short on a slow GPU
    let rows = (BAND_PIXELS / output.w.max(1) / 16 * 16).max(16); // whole workgroups: bands never overlap
    let y0_at = crate::params::index("Y0").unwrap_or(0);
    let mut p = p;
    for y0 in (0..output.h).step_by(rows).filter(|_| fault != Some(crate::Fault::DropWork)) {
        p[y0_at] = y0 as u32;
        cx.run(
            "main",
            &p,
            &[
                Some(&lin),
                Some(&prep.log_l),
                Some(&prep.base),
                prep.clarity.as_deref(),
                prep.texture.as_deref(),
                prep.dark.as_deref(),
                masks.as_ref(),
                Some(&aux),
                Some(&out),
            ],
            groups2(output.w, rows.min(output.h - y0), [16, 16]),
        );
    }
    // the alpha a mask overlay shows: copied out before the readback below submits
    let overlay_mask = req.overlay.mask(s).map(|m| {
        let list = s.masks.iter().filter(|m| m.visible && !m.components.is_empty());
        match (list.map(|m| m.id).position(|id| id == m.id), masks.as_ref()) {
            (Some(mi), Some(all)) => Ok(cx.slice(all, mi * n, n)),
            _ => Err(m),
        }
    });
    // every visible mask's alpha, for `Overlay::AllMasks` (one tint over all masks at once).
    // The masks buffer holds each visible mask's alpha back-to-back in the same `list` order the
    // single-mask overlay uses, so read the whole buffer back in ONE GPU->CPU transfer and split it
    // on the CPU, instead of one synchronous readback per mask (which stalls the pipeline N times).
    let all_overlay_alphas: Option<Vec<Plane>> = if matches!(req.overlay, lightcraft_pipeline::Overlay::AllMasks { .. }) {
        masks.as_ref().map(|all| {
            let list: Vec<_> = s.masks.iter().filter(|m| m.visible && !m.components.is_empty()).collect();
            if list.is_empty() {
                return Vec::new();
            }
            let raw: Vec<f32> = cx.read(all, n * list.len());
            let mut alphas = Vec::with_capacity(list.len());
            for mi in 0..list.len() {
                let s = mi * n;
                alphas.push(Plane { width: w, height: h, data: raw[s..s + n].to_vec() });
            }
            alphas
        })
    } else {
        None
    };
    let data: Vec<[u8; 4]> = cx.read(&out, out_n);
    let image = Rgba8 { width: output.w, height: output.h, data };
    lap("finish + readback", &mut t, &mut cx);
    if crate::ctx::failed() {
        return None;
    }
    // The per-pixel kernel writes alpha 255 everywhere and `out` was cleared: a pixel with any
    // other alpha means work silently did not run (a driver that reset or dropped a submission
    // without reporting it — issue #78's all-black exports).
    let unwritten = unwritten(&image.data);
    if unwritten > 0 {
        fail(FailKind::Fatal, format!("the GPU returned an incomplete image ({unwritten} of {out_n} pixels unwritten)"));
        return None;
    }
    let histogram = match crop {
        Some(k) => Histogram::of_srgb8(&image.crop(k.x, k.y, k.w, k.h)),
        None => Histogram::of_srgb8(&image),
    };
    lap("histogram", &mut t, &mut cx);
    // An entirely black result (upstream work that did not run would give that too): redo it on
    // the CPU, which costs time only for the rare photo that really is black.
    if n >= 4096 && [&histogram.r, &histogram.g, &histogram.b].iter().all(|c| c.first() == Some(&histogram.total)) {
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
            lightcraft_pipeline::masks::evaluate_one(m, &plan.frame, w, h, &img, &l, s.light.exposure as f32, plan.mattes.as_deref())
        }
    });
    lightcraft_pipeline::visualize::apply(&mut image, req.overlay, &plan, overlay_mask.as_ref(), all_overlay_alphas.as_deref());
    // Overlay paths finished the work image; return the requested window.
    if let Some(k) = crop {
        image = image.crop(k.x, k.y, k.w, k.h);
    }
    Some(Rendered { image, histogram, deep: None })
}

/// Pixels whose alpha is not 255 (cheap: no early exit, so it vectorizes).
fn unwritten(data: &[[u8; 4]]) -> usize {
    data.chunks(4096).filter(|c| c.iter().fold(255u8, |a, p| a & p[3]) != 255).map(|c| c.iter().filter(|p| p[3] != 255).count()).sum()
}

/// The white-balanced, retouched, denoised image.
fn linear(cx: &mut Cx<'_>, sampled: &Buf, info: &SourceInfo, plan: &Plan<'_>, host: &mut Host) -> Buf {
    let (w, h) = (plan.w, plan.h);
    let n = w * h;
    let s = &*plan.settings;
    let img = if lightcraft_pipeline::lin_needs_cpu(s, info) {
        // defringe / spot removal / local tone mapping: CPU
        let mut img = match host.sampled.take() {
            Some(i) => i,
            None => cx.read_rgb(sampled, w, h),
        };
        lightcraft_pipeline::lin_cpu(&mut img, info, plan);
        cx.gpu.upload(rgb_words(&img))
    } else {
        let m = local::wb_matrix_for(info, s);
        let mut p = vec![m.is_some() as u32];
        p.extend(m.unwrap_or_default().iter().flatten().map(|v| v.to_bits()));
        let out = cx.gpu.buffer(n * 3);
        map(cx, "wb_k", n, &p, [Some(sampled), None, None], &out);
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
    denoise(cx, img, plan)
}

/// Noise reduction (`local::denoise`).
fn denoise(cx: &mut Cx<'_>, img: Buf, plan: &Plan<'_>) -> Buf {
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
    // Recycle the luminance scratch before allocating RGB colour scratch.
    cx.flush();
    if let Some(nr) = col {
        let chroma = cx.gpu.buffer(n * 3);
        map(cx, "chroma_k", n, &[], [Some(&img), None, None], &chroma);
        let b = gaussian_owned(cx, chroma, w, h, 3, nr.sigma);
        let out = cx.gpu.buffer(n * 3);
        map(cx, "nr_col_owned", n, &[nr.t.to_bits(), 0], [Some(&img), None, Some(&b)], &out);
        img = out;
    }
    cx.flush();
    img
}

/// The planes the per-pixel stage reads.
struct Prep {
    log_l: Arc<Buf>,
    base: Arc<Buf>,
    clarity: Option<Arc<Buf>>,
    texture: Option<Arc<Buf>>,
    dark: Option<Arc<Buf>>,
    /// Blurred chromaticity (local Moiré / Noise).
    chroma: Option<Arc<Buf>>,
    air: f32,
}

/// Use the same geometry, linear stages, dark-channel kernel and sample selection as a
/// full GPU render. If that frame exceeds device limits, its reference is the CPU fallback.
struct AirlightWork {
    sampled: Option<Arc<Buf>>,
    lin: Arc<Buf>,
    dark: Arc<Buf>,
}
struct AirlightFrame {
    geo: u64,
    w: usize,
    sampled: Option<Arc<Buf>>,
    lin: Arc<Buf>,
    dark: Arc<Buf>,
    air: f32,
}
impl AirlightFrame {
    fn window(&self, gpu: &Gpu, plan: &Plan<'_>) -> AirlightWork {
        let (x, y) = plan.frame.view.map_or((0, 0), |v| (v.x as usize, v.y as usize));
        let r = PixelWindow { x, y, w: plan.w, h: plan.h };
        let mut cx = Cx::new(gpu);
        let work = AirlightWork {
            sampled: self.sampled.as_ref().map(|s| cx.crop(s, self.w, 3, r)),
            lin: cx.crop(&self.lin, self.w, 3, r),
            dark: cx.crop(&self.dark, self.w, 1, r),
        };
        cx.flush();
        work
    }
    fn bytes(&self) -> usize {
        (self.sampled.as_ref().map_or(0, |s| s.len) + self.lin.len + self.dark.len) * 4
    }
}

fn frame_airlight(
    gpu: &Gpu,
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    full: &Plan<'_>,
    plan: &Plan<'_>,
    stages: Option<&GpuStages>,
    sampled: Option<Arc<Buf>>,
) -> Result<(f32, Option<AirlightFrame>), String> {
    #[cfg(test)]
    tests::FRAME_AIRLIGHT_PASSES.with(|n| n.set(n.get() + 1));
    lightcraft_pipeline::airlight::check_prepare_budget(full, Some(plan), lightcraft_pipeline::airlight::PREPARE_BUDGET, true)?;
    lightcraft_pipeline::airlight::check_pixels(src.width, src.height)?;
    let (w, h) = (full.w, full.h);
    let n = w * h;
    if !gpu.fits(n * 3) {
        return lightcraft_pipeline::airlight::compute(src, info, full).map(|air| (air, None));
    }
    let mut cx = Cx::new(gpu);
    let mut host = Host::default();
    let sampled = if let Some(sampled) = sampled {
        sampled
    } else if gpu.fits(src.data.len() * 3) && !full.frame.warp.as_ref().is_some_and(|w| w.needs_f64()) {
        let upload = || gpu.upload(rgb_words(src));
        let source = match stages {
            Some(c) if src.data.len() * 12 <= RETAIN_SOURCE_BYTES => c.source(src, upload),
            Some(_) => shared_source(src, upload),
            None => Arc::new(upload()),
        };
        sample(&mut cx, src, source, full)
    } else {
        let img = full.frame.sample(src, w, h);
        let b = Arc::new(gpu.upload(rgb_words(&img)));
        if lightcraft_pipeline::lin_needs_cpu(&full.settings, info) {
            host.sampled = Some(img);
        }
        b
    };
    let lin = Arc::new(linear(&mut cx, &sampled, info, full, &mut host));
    let d0 = cx.gpu.buffer(n);
    map(&mut cx, "dark_k", n, &[], [Some(&lin), None, None], &d0);
    let dark = gaussian_owned(&mut cx, d0, w, h, 1, (0.02 * full.px_per_long as f32).max(1.0));
    let m = n.div_ceil(local::AIRLIGHT_STEP);
    let sub = cx.gpu.buffer(m);
    map(&mut cx, "subsample", m, &[local::AIRLIGHT_STEP as u32], [Some(&dark), None, None], &sub);
    let air = local::airlight_of(cx.read(&sub, m));
    let sampled = (n * 28 <= lightcraft_pipeline::airlight::FrameCache::<AirlightFrame>::BUDGET).then_some(sampled);
    Ok((air, Some(AirlightFrame { geo: full.geo, w, sampled, lin, dark: Arc::new(dark), air })))
}

/// The spatial planes (`local::prepare`), reusing those in `planes`.
fn prepare(cx: &mut Cx<'_>, lin: &Buf, plan: &Plan<'_>, req: &RenderRequest, planes: &mut Planes) -> Prep {
    let (w, h) = (plan.w, plan.h);
    let n = w * h;
    let sig = local::plane_sigmas(&plan.settings, plan.px_per_long, req.quality);
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
            plane_at(&mut planes.base, key, || tone_base(cx, &log_l, plan, sg, sig.base_detail))
        }
        None => log_l.clone(),
    };
    let clarity = sig.clarity.map(|sg| plane_at(&mut planes.clarity, sg.to_bits(), || guided_fast(cx, &log_l, plan, sg, local::CLARITY_EPS)));
    let texture = sig.texture.map(|sg| plane_at(&mut planes.texture, sg.to_bits(), || gaussian(cx, &log_l, w, h, 1, sg)));
    let (dark, air) = match sig.dark {
        None => (None, 1.0),
        Some(sg) => match &planes.dark {
            Some((k, d, air)) if *k == sg.to_bits() => (Some(d.clone()), *air),
            _ => {
                let d0 = cx.gpu.buffer(n);
                map(cx, "dark_k", n, &[], [Some(lin), None, None], &d0);
                let d = gaussian(cx, &d0, w, h, 1, sg);
                // airlight: a percentile of every 7th value, as on the CPU
                let m = n.div_ceil(local::AIRLIGHT_STEP);
                let sub = cx.gpu.buffer(m);
                map(cx, "subsample", m, &[local::AIRLIGHT_STEP as u32], [Some(&d), None, None], &sub);
                let air = local::airlight_of(cx.read(&sub, m));
                cx.flush();
                let b = Arc::new(d);
                planes.dark = Some((sg.to_bits(), b.clone(), air));
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
    Prep { log_l, base, clarity, texture, dark, chroma, air }
}

/// Most brush dabs the mask kernel evaluates per pixel (more: the CPU rasterizes the brush).
const MAX_DABS: usize = 4096;

/// The `shape` kernel's brush data for `dabs`: stroke records (12 words: dab offset, dab count,
/// r, hard, flow, density, erase, bbox x0 y0 x1 y1, auto) then the dab centres.
fn brush_aux(dabs: &[&lightcraft_pipeline::masks::BrushDabs], erase: bool) -> Vec<f32> {
    let mut aux = Vec::new();
    let mut off = 12 * dabs.len();
    for d in dabs {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for q in &d.dabs {
            (x0, y0, x1, y1) = (x0.min(q.x - d.r), y0.min(q.y - d.r), x1.max(q.x + d.r), y1.max(q.y + d.r));
        }
        aux.extend([off as f32, d.dabs.len() as f32, d.r as f32, d.hard as f32, d.flow, d.density, (erase && d.erase) as u8 as f32]);
        aux.extend([x0, y0, x1, y1].map(|v| v as f32));
        aux.push(d.auto as u8 as f32);
        off += 2 * d.dabs.len();
    }
    for d in dabs {
        aux.extend(d.dabs.iter().flat_map(|q| [q.x as f32, q.y as f32]));
    }
    aux
}

/// Cross guided filter (`masks::guided_cross`) of `p` steered by `guide`, max-combined with `p`.
fn guided_cross_max(cx: &mut Cx<'_>, guide: &Buf, p: &Buf, w: usize, h: usize, sigma: f32, eps: f32) -> Buf {
    let n = w * h;
    let (m0, m1) = (cx.gpu.buffer(2 * n), cx.gpu.buffer(2 * n));
    map(cx, "xguided_pre", n, &[0], [Some(guide), Some(p), None], &m0);
    map(cx, "xguided_pre", n, &[1], [Some(guide), Some(p), None], &m1);
    let (b0, b1) = (gaussian(cx, &m0, w, h, 2, sigma), gaussian(cx, &m1, w, h, 2, sigma));
    let ab = cx.gpu.buffer(2 * n);
    map(cx, "xguided_ab", n, &[eps.to_bits()], [Some(&b0), Some(&b1), None], &ab);
    let ab = gaussian(cx, &ab, w, h, 2, sigma);
    let q = cx.gpu.buffer(n);
    map(cx, "xguided_apply", n, &[], [Some(guide), Some(&ab), Some(p)], &q);
    q
}

/// Evaluate the masks (`lightcraft_pipeline::masks::evaluate`): their alpha planes (concatenated,
/// followed by the blurred chromaticity when local Moiré / Noise need it) and their adjustment
/// terms. Shapes without a kernel (Sky, Subject, …) run on the CPU. A mask whose alpha `alphas`
/// holds (same [`alpha_key`]) is copied from there; `alphas` keeps this render's.
fn masks(
    cx: &mut Cx<'_>,
    lin: &Buf,
    prep: &Prep,
    plan: &Plan<'_>,
    host: &mut Host,
    alphas: &mut AlphaCache<Buf>,
) -> (Option<Buf>, Vec<[f32; MASK_TERMS]>) {
    use lightcraft_develop::{MaskOp, MaskShape};
    let s = &*plan.settings;
    let list: Vec<_> = s.masks.iter().filter(|m| m.visible && !m.components.is_empty()).collect();
    let old = alphas.take(plan.mattes.as_ref());
    if list.is_empty() {
        return (None, Vec::new());
    }
    let keys = AlphaKeys { geo: plan.geo, image: plan.lin_key };
    let mut used = Vec::with_capacity(list.len());
    let (w, h) = (plan.w, plan.h);
    let n = w * h;
    let frame = &plan.frame;
    let ev = s.light.exposure as f32;
    let long = frame.ow.max(frame.oh);
    let [a, b, c, d, e, f] = frame.out_to_norm(w, h).0;
    let (kx, ky) = (frame.ow / long, frame.oh / long);
    let aff = [a * kx, b * ky, c * kx, d * ky, e * kx, f * ky].map(|v| (v as f32).to_bits());
    let alpha = cx.zeroed(n * list.len() + if prep.chroma.is_some() { 3 * n } else { 0 });
    if let Some(ch) = &prep.chroma {
        cx.copy_into(ch, &alpha, n * list.len());
    }
    let comp_plane = cx.gpu.buffer(n);
    for (mi, m) in list.iter().enumerate() {
        let key = alpha_key(m, keys, ev);
        if let Some((_, b)) = old.iter().find(|(k, _)| *k == key) {
            cx.copy_into(b, &alpha, mi * n);
            used.push((key, b.clone()));
            continue;
        }
        let mut first = true;
        for comp in &m.components {
            let op = match comp.op {
                _ if first && comp.op != MaskOp::Intersect => 0u32,
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
                MaskShape::ColorRange { samples, refine } => {
                    let tol = lightcraft_pipeline::masks::color_range_tolerance(*refine);
                    p.extend([tol.to_bits(), ev.exp2().to_bits(), samples.len() as u32]);
                    aux.extend(samples.iter().flat_map(|s| s.map(|v| v as f32)));
                    Some(3)
                }
                MaskShape::Brush { strokes } => {
                    let dabs: Vec<_> = strokes.iter().map(|st| lightcraft_pipeline::masks::brush_dabs(st, frame, w, h)).collect();
                    // pen pressure varies radius and flow per dab, which the kernel doesn't take: the CPU rasterizes it
                    if dabs.iter().map(|d| d.dabs.len()).sum::<usize>() > MAX_DABS || dabs.iter().any(|d| !d.pressure.is_empty()) {
                        None
                    } else if dabs.iter().any(|d| d.auto) {
                        // Auto Mask: stroke by stroke (each one refined before it's combined)
                        let brush = cx.zeroed(n);
                        let stroke = cx.gpu.buffer(n);
                        for d in &dabs {
                            let mut q = p.clone();
                            q[2] = 4;
                            q[3] = 0;
                            q.push(1);
                            let aux = cx.gpu.upload(&brush_aux(&[d], false));
                            cx.run("shape", &q, &[Some(lin), Some(&prep.log_l), Some(&aux), Some(&stroke), Some(&brush)], groups2(w, h, [16, 16]));
                            let refined = d.auto.then(|| {
                                let (sigma, eps) = lightcraft_pipeline::masks::auto_refine(d.r);
                                guided_cross_max(cx, &prep.log_l, &stroke, w, h, sigma, eps)
                            });
                            let op = if d.erase { 2 } else { 1 };
                            cx.run(
                                "combine",
                                &[n as u32, 0, 0, op],
                                &[None, None, None, Some(refined.as_ref().unwrap_or(&stroke)), Some(&brush)],
                                groups1(n),
                            );
                        }
                        if comp.invert {
                            cx.run("finalize", &[n as u32, 0, 0, 1, 1f32.to_bits()], &[None, None, None, Some(&stroke), Some(&brush)], groups1(n));
                        }
                        own = Some(brush);
                        None
                    } else {
                        p.push(dabs.len() as u32);
                        aux = brush_aux(&dabs.iter().collect::<Vec<_>>(), true);
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
                    cx.run("shape", &p, &[Some(lin), Some(&prep.log_l), aux.as_ref(), Some(&comp_plane), Some(&alpha)], groups2(w, h, [16, 16]));
                    None
                }
                None => {
                    // no kernel: evaluate on the CPU
                    let img = host.lin.get_or_insert_with(|| Arc::new(cx.read_rgb(lin, w, h))).clone();
                    let l = host.log_l.get_or_insert_with(|| Arc::new(cx.read_plane(&prep.log_l, w, h))).clone();
                    let mut v = lightcraft_pipeline::masks::shape_alpha(&comp.shape, frame, w, h, &img, &l, ev, plan.mattes.as_deref());
                    if comp.invert {
                        v.data.iter_mut().for_each(|x| *x = 1.0 - *x);
                    }
                    Some(cx.gpu.upload(&v.data))
                }
            };
            let cbuf = plane.as_ref().unwrap_or(&comp_plane);
            let k = [n as u32, 0, mi as u32, op];
            cx.run("combine", &k, &[None, None, None, Some(cbuf), Some(&alpha)], groups1(n));
        }
        let amt = (m.adjust.amount / 100.0) as f32;
        cx.run(
            "finalize",
            &[n as u32, 0, mi as u32, m.invert as u32, amt.to_bits()],
            &[None, None, None, Some(&comp_plane), Some(&alpha)],
            groups1(n),
        );
        used.push((key, Arc::new(cx.slice(&alpha, mi * n, n))));
    }
    alphas.keep(plan.mattes.as_ref(), used);
    let terms = list.iter().map(|m| mask_terms(&m.adjust)).collect();
    (Some(alpha), terms)
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        pub(super) static FRAME_AIRLIGHT_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[test]
    fn gpu_stage_trimming_retains_only_matching_airlight_scalars() {
        let Some(gpu) = crate::device() else {
            lightcraft_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        let _scope = crate::ctx::RenderScope::new(gpu);
        let mut src = Arc::new(Rgb32f::from_fn(601, 403, |x, y| {
            let l = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0;
            [l, l * 0.82, l * 1.15]
        }));
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.effects.dehaze = 40.0;
        let req = RenderRequest { window: Some(PixelWindow { x: 201, y: 177, w: 121, h: 83 }), ..RenderRequest::fit(601, 403) };
        let cache = lightcraft_pipeline::StageCache::default();
        let stages = cache.extension::<GpuStages>();
        render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap();
        for step in 0..6 {
            crate::trim_stages(&cache);
            assert_eq!(stages.bytes(), 0);
            assert_eq!(Arc::strong_count(&src), 1, "trimming must release the source too");
            assert!(Arc::ptr_eq(&stages, &cache.peek_extension::<GpuStages>().unwrap()));
            match step {
                0 => s.light.exposure = 0.5,
                1 => s.effects.dehaze = 80.0,
                2 => s.detail.nr_color = 50.0,
                3 => {
                    s.wb.mode = lightcraft_develop::WbMode::Custom;
                    s.wb.temp = 8500.0;
                }
                4 => s.crop.geometry.angle = 3.0,
                5 => src = Arc::new((*src).clone()),
                _ => {}
            }
            FRAME_AIRLIGHT_PASSES.with(|n| n.set(0));
            let actual = render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap().image;
            assert_eq!(FRAME_AIRLIGHT_PASSES.with(|n| n.get()), usize::from(step >= 2), "step={step}");
            let fresh = render(gpu, &src, &info, &s, &req, None, None).unwrap().image;
            let max = actual.data.iter().flatten().zip(fresh.data.iter().flatten()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            assert!(max <= 1, "step={step} max={max}");
        }
        cache.clear();
        assert!(cache.peek_extension::<GpuStages>().is_none(), "a full reset must still forget the statistic");
    }

    #[test]
    fn evicted_gpu_windows_reuse_the_full_statistic() {
        let Some(gpu) = crate::device() else {
            lightcraft_testkit::skip!(Gpu, "no GPU adapter");
            return;
        };
        let _scope = crate::ctx::RenderScope::new(gpu);
        let src = Arc::new(Rgb32f::from_fn(1201, 803, |x, y| {
            let l = if (x / 3 + y / 5) % 7 == 0 { 0.003 } else { 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0 };
            [l, l * 0.82, l * 1.15]
        }));
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.effects.dehaze = 40.0;
        s.effects.clarity = 30.0;
        s.detail.nr_luminance = 30.0;
        s.detail.nr_color = 25.0;
        let req = RenderRequest { window: Some(PixelWindow { x: 401, y: 277, w: 121, h: 83 }), ..RenderRequest::fit(1201, 803) };
        let stages = GpuStages::default();
        render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap();
        let mut cold_ms = f64::MAX;
        let mut reused_ms = f64::MAX;
        let mut repeated_passes = 0;
        for step in 0..5 {
            s.light.exposure = step as f64 * 0.1;
            let req = RenderRequest { window: Some(PixelWindow { x: 401 + step * 3, y: 277, w: 121, h: 83 }), ..req };
            // Simulate app trimming at modest size. Both timed paths must upload
            // their source, so the comparison isolates retained scalar reuse.
            stages.trim_images();
            FRAME_AIRLIGHT_PASSES.with(|n| n.set(0));
            let start = std::time::Instant::now();
            let reused = render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap().image;
            reused_ms = reused_ms.min(start.elapsed().as_secs_f64() * 1000.0);
            let passes = FRAME_AIRLIGHT_PASSES.with(|n| n.get());
            repeated_passes += passes;
            let start = std::time::Instant::now();
            let fresh = render(gpu, &src, &info, &s, &req, None, None).unwrap().image;
            cold_ms = cold_ms.min(start.elapsed().as_secs_f64() * 1000.0);
            let max = reused.data.iter().flatten().zip(fresh.data.iter().flatten()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
            eprintln!("evicted window step={step} full_passes={passes} max={max}");
            assert!(max <= 1, "scalar reuse changed requested pixels by {max} codes");
        }
        eprintln!("synthetic 1201x803, min of 5: fresh={cold_ms:.2} ms, scalar reuse={reused_ms:.2} ms");
        assert_eq!(repeated_passes, 0, "a matching scalar must not require another full preparation");

        for process in [lightcraft_develop::Process::V1, lightcraft_develop::Process::V2] {
            s.process = process.version();
            s.effects.dehaze = 100.0;
            s.detail.nr_luminance = 80.0;
            s.detail.nr_color = 70.0;
            let req = RenderRequest { max_w: 1801, max_h: 1207, ..req };
            let full = render(gpu, &src, &info, &s, &RenderRequest { window: None, ..req }, None, None).unwrap().image;
            render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap();
            for (x, y) in [(0, 0), (601, 0), (701, 477), (1680, 1124)] {
                stages.trim_images();
                let req = RenderRequest { window: Some(PixelWindow { x, y, w: 121, h: 83 }), ..req };
                FRAME_AIRLIGHT_PASSES.with(|n| n.set(0));
                let actual = render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap().image;
                assert_eq!(FRAME_AIRLIGHT_PASSES.with(|n| n.get()), 0);
                let fresh = render(gpu, &src, &info, &s, &req, None, None).unwrap().image;
                let max = actual.data.iter().flatten().zip(fresh.data.iter().flatten()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                assert!(max <= 1, "{process:?} ({x},{y}) max={max}");
                let crop = full.crop(x, y, actual.width, actual.height);
                let max = actual.data.iter().flatten().zip(crop.data.iter().flatten()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                assert!(max <= 1, "full-render crop {process:?} ({x},{y}) max={max}");
            }
        }
    }

    #[test]
    fn temporary_windows_preserve_exact_gpu_preparation() {
        let Some(gpu) = crate::device() else { return };
        let _scope = crate::ctx::RenderScope::new(gpu);
        let src = Arc::new(Rgb32f::from_fn(401, 267, |x, y| {
            let l = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0;
            [l, l * 0.82, l * 1.15]
        }));
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.effects.dehaze = 40.0;
        s.effects.clarity = 30.0;
        let req = RenderRequest { window: Some(PixelWindow { x: 101, y: 77, w: 121, h: 83 }), ..RenderRequest::fit(1201, 803) };
        let mut stages = GpuStages::default();
        let expected = render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap().image;
        let prepared = stages.airlight_frame.value().unwrap();
        let full = lightcraft_pipeline::plan(&src, &info, &s, &RenderRequest { window: None, ..req });
        let scalar = stages.airlight.get(&src, full.lin_key);
        let entries = stages.lock().len();
        for window in [req.window, Some(PixelWindow { x: 0, y: 0, w: 1201, h: 803 })] {
            let bounded = RenderRequest { bounded_window: true, window, ..req };
            let warm = render(gpu, &src, &info, &s, &bounded, Some(&stages), None).unwrap().image;
            assert_eq!(warm, render(gpu, &src, &info, &s, &bounded, None, None).unwrap().image);
            assert_eq!(stages.lock().len(), entries);
            assert!(Arc::ptr_eq(&prepared, &stages.airlight_frame.value().unwrap()));
            assert_eq!(stages.airlight.get(&src, full.lin_key), scalar);
            let fresh = GpuStages::default();
            render(gpu, &src, &info, &s, &bounded, Some(&fresh), None).unwrap();
            assert!(fresh.lock().is_empty());
            assert!(fresh.airlight.get(&src, full.lin_key).is_none());
            assert!(fresh.airlight_frame.value().is_none());
        }
        stages.airlight = lightcraft_pipeline::airlight::AirlightCache::with_budget(1);
        stages.airlight.remember(&src, full.lin_key, 0.25);
        render(gpu, &src, &info, &s, &RenderRequest { quality: lightcraft_pipeline::Quality::Draft, ..req }, Some(&stages), None).unwrap();
        assert!(Arc::ptr_eq(&prepared, &stages.airlight_frame.value().unwrap()));
        assert_eq!(stages.airlight.get(&src, full.lin_key), Some(0.25));
        stages.airlight = lightcraft_pipeline::airlight::AirlightCache::default();
        assert_eq!(render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap().image, expected);
        s.detail.nr_luminance = 50.0;
        render(gpu, &src, &info, &s, &RenderRequest { quality: lightcraft_pipeline::Quality::Draft, ..req }, Some(&stages), None).unwrap();
        assert_eq!(
            render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap().image,
            render(gpu, &src, &info, &s, &req, None, None).unwrap().image
        );
    }

    #[test]
    fn device_limited_full_frame_reuses_its_cpu_statistic_on_a_pan() {
        let Some(gpu) = crate::device() else { return };
        let _scope = crate::ctx::RenderScope::new(gpu);
        let _limit = crate::ctx::LimitOverride::new(4 << 20);
        let src = Arc::new(Rgb32f::from_fn(1281, 853, |x, y| {
            let l = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0;
            [l, l * 0.82, l * 1.15]
        }));
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.effects.dehaze = 15.0;
        let stages = GpuStages::default();
        let req = RenderRequest { window: Some(PixelWindow { x: 400, y: 300, w: 121, h: 83 }), ..RenderRequest::fit(1281, 853) };
        let full = lightcraft_pipeline::plan(&src, &info, &s, &RenderRequest { window: None, ..req });
        assert!(!gpu.fits(full.w * full.h * 3));
        render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap();
        assert!(stages.airlight_frame.value().is_none());
        assert!(stages.airlight.get(&src, full.lin_key).is_some());
        // A second full pass would overwrite this marker with the source's airlight.
        stages.airlight.remember(&src, full.lin_key, 0.25);
        let pan = RenderRequest { window: Some(PixelWindow { x: 410, y: 300, w: 121, h: 83 }), ..req };
        render(gpu, &src, &info, &s, &pan, Some(&stages), None).unwrap();
        assert_eq!(stages.airlight.get(&src, full.lin_key), Some(0.25));
    }

    #[test]
    fn consuming_nr_buffers_preserves_the_borrowed_kernels_bits() {
        let Some(gpu) = crate::device() else { return };
        let _scope = crate::ctx::RenderScope::new(gpu);
        let (w, h) = (257, 83);
        let img = Rgb32f::from_fn(w, h, |x, y| {
            let l = if (x + y) % 7 == 0 { 0.00000001 } else { 0.01 + ((x * 17 + y * 23) % 103) as f32 / 30.0 };
            [l, l * 0.82, l * 1.15]
        });
        let mut cx = Cx::new(gpu);
        let source = gpu.upload(rgb_words(&img));
        for sigma in [0., 1., 4.5, 30.] {
            let original = cx.gpu.buffer(w * h * 3);
            map(&mut cx, "chroma_k", w * h, &[], [Some(&source), None, None], &original);
            let blur = gaussian(&mut cx, &original, w, h, 3, sigma);
            let copied = cx.copy(&original);
            let consumed = gaussian_owned(&mut cx, copied, w, h, 3, sigma);
            let a: Vec<f32> = cx.read(&blur, w * h * 3);
            let b: Vec<f32> = cx.read(&consumed, w * h * 3);
            assert!(a.iter().zip(&b).all(|(a, b)| a.to_bits() == b.to_bits()), "blur sigma={sigma}");
            for amount in [0.1875f32, 0.75] {
                let borrowed = cx.gpu.buffer(w * h * 3);
                map(&mut cx, "nr_col", w * h, &[amount.to_bits()], [Some(&source), Some(&original), Some(&blur)], &borrowed);
                let owned = cx.gpu.buffer(w * h * 3);
                map(&mut cx, "nr_col_owned", w * h, &[amount.to_bits(), 0], [Some(&source), None, Some(&consumed)], &owned);
                let a: Vec<f32> = cx.read(&borrowed, w * h * 3);
                let b: Vec<f32> = cx.read(&owned, w * h * 3);
                assert!(a.iter().zip(&b).all(|(a, b)| a.to_bits() == b.to_bits()), "colour NR sigma={sigma}, amount={amount}");
            }
        }
    }

    #[test]
    fn small_preparation_budget_uses_proxy_and_keeps_window_stages_warm() {
        let Some(gpu) = crate::device() else { return };
        let _scope = crate::ctx::RenderScope::new(gpu);
        let src = Arc::new(Rgb32f::from_fn(1201, 803, |x, y| {
            let l = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0;
            [l, l * 0.82, l * 1.15]
        }));
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        s.effects.dehaze = 15.0;
        s.detail.nr_color = 25.0;
        let stages = GpuStages { airlight: lightcraft_pipeline::airlight::AirlightCache::with_budget(1), ..Default::default() };
        for step in 0..3 {
            s.light.exposure = step as f64 * 0.1;
            let req = RenderRequest { window: Some(PixelWindow { x: 400 + step * 3, y: 300, w: 121, h: 83 }), ..RenderRequest::fit(1201, 803) };
            let warm = render(gpu, &src, &info, &s, &req, Some(&stages), None).unwrap();
            let fresh = GpuStages { airlight: lightcraft_pipeline::airlight::AirlightCache::with_budget(1), ..Default::default() };
            assert_eq!(warm.image, render(gpu, &src, &info, &s, &req, Some(&fresh), None).unwrap().image);
            assert!(stages.airlight_frame.value().is_none());
            assert_eq!((warm.image.width, warm.image.height), (121, 83));
        }
    }

    #[test]
    fn coverage_mask_matches_per_pixel_decision() {
        use lightcraft_develop::{EmbeddedLens, EmbeddedWarp};
        let lens = EmbeddedLens {
            warp: Some(EmbeddedWarp {
                planes: [[1.0, -0.03, 0.01, 0.0, 0.001, -0.002]; 3],
                center: lightcraft_geom::Point::new(0.52, 0.48),
                radius: 0.6,
            }),
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
            let f = lightcraft_pipeline::geometry::Frame::with_lens(900, 600, &s, true, Some(&lens));
            let wp = f.warp.as_ref().expect("warp");
            // widths that do and don't fill the last word; heights that do and don't fill the last band
            for (w, h) in [f.fit(700, 700), (333, 221), (64, 16), (1, 1)] {
                let o2t = f.out_to_oriented(w, h);
                let mask = coverage_mask(wp, &o2t, w, h);
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
        use lightcraft_geom::{Affine, Homography};
        let (w, h) = (900, 600);
        let mut wp = lightcraft_pipeline::optics::Warp::identity(w as f64, h as f64);
        // centred coordinates: v = (y − 300) / 450, denominator 2·v + m8, exactly 0 at y = 75.5
        let m8 = -((75.5 - 300.0) / 450.0 * 2.0);
        wp.persp_inv = Homography([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0, m8]);
        wp.persp = wp.persp_inv.inverse().expect("invertible");
        let o2t = Affine::IDENTITY;
        assert_eq!(wp.block_coverage(&o2t, 0, 32, 64, 80), None, "the block holding the horizon");
        assert_eq!(wp.block_coverage(&o2t, 0, 32, 0, 16), Some(false), "beyond the horizon");
        assert_eq!(wp.block_coverage(&o2t, 448, 480, 288, 304), Some(true), "the centre");
        let mask = coverage_mask(&wp, &o2t, w, h);
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

    /// The blur against `lightcraft_raster::blur::gaussian`: widths that do and don't fill the last
    /// workgroup, windows wider than the image, and 1, 2 (as two planes) and 3 channels.
    #[test]
    fn gaussian_matches_cpu() {
        use lightcraft_raster::blur;
        let Some(gpu) = crate::device() else { return };
        let _scope = crate::ctx::RenderScope::new(gpu);
        let v = |x: usize, y: usize, c: usize| ((x * 7 + y * 13 + c * 5) % 17) as f32 / 16.0 + if x % 61 < 3 { 2.0 } else { 0.0 };
        for (w, h) in [(1000, 5), (257, 3), (5, 4), (1, 9)] {
            for sigma in [0.8f32, 2.0, 4.5, 30.0, 250.0, 1000.0] {
                let planes: Vec<Plane> = (0..3).map(|c| Plane::from_fn(w, h, |x, y| v(x, y, c))).collect();
                let want: Vec<Plane> = planes.iter().map(|p| blur::gaussian(p, sigma)).collect();
                for nc in [1, 2, 3] {
                    let words: Vec<f32> = (0..w * h).flat_map(|i| (0..nc).map(move |c| (i, c))).map(|(i, c)| planes[c].data[i]).collect();
                    let mut cx = Cx::new(gpu);
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

    /// Kernel timings on a 24 MP plane: `cargo test --release -p lightcraft-gpu -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_kernels() {
        let Some(gpu) = crate::device() else { return };
        let _scope = crate::ctx::RenderScope::new(gpu);
        let (w, h) = (6000, 4000);
        eprintln!("{} ({:?})", gpu.info.name, gpu.info.backend);
        let src = gpu.upload(&vec![0.5f32; w * h * 3]);
        let time = |name: &str, f: &mut dyn FnMut(&mut Cx<'_>)| {
            let mut best = f64::MAX;
            for _ in 0..5 {
                let mut cx = Cx::new(gpu);
                cx.sync();
                let t = std::time::Instant::now();
                f(&mut cx);
                cx.sync();
                best = best.min(t.elapsed().as_secs_f64() * 1e3);
            }
            eprintln!("{name:<40} {best:.1} ms");
        };
        for nc in [1, 2, 3] {
            let out = gpu.buffer(w * h * nc);
            // noise reduction's radii, Texture's and Dehaze's at 6000 px
            for r in [2, 11, 120] {
                for (k, horizontal) in [("box_h", true), ("box_v", false)] {
                    time(&format!("{k} nc={nc} r={r}"), &mut |cx| box_pass(cx, horizontal, &src, &out, (w, h), nc, r));
                }
            }
        }
        for nc in [1, 2, 3] {
            time(&format!("gaussian nc={nc} sigma=2"), &mut |cx| drop(gaussian(cx, &src, w, h, nc, 2.0)));
        }
        time("gaussian nc=3 sigma=4.5 (colour NR)", &mut |cx| drop(gaussian(cx, &src, w, h, 3, 4.5)));
        time("map log_lum", &mut |cx| {
            let o = cx.gpu.buffer(w * h);
            map(cx, "log_lum_k", w * h, &[], [Some(&src), None, None], &o);
        });
        time("guided sigma=1.75", &mut |cx| drop(guided(cx, &src, w, h, 1.75, 0.01)));
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
