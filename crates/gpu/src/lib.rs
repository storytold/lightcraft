//! The LightCraft develop pipeline on the GPU (wgpu compute, WGSL kernels).
//!
//! The CPU pipeline (`lightcraft-pipeline`) is the reference: every kernel here is a port of a CPU
//! stage, both read the same resolved parameters ([`lightcraft_pipeline::Plan`],
//! [`lightcraft_pipeline::finish::FinishParams`]), and the equivalence tests (`tests/`) render the
//! same settings on both and bound the difference in 8-bit sRGB. Stages without a kernel run on the
//! CPU inside the same render (per-stage hybrid); see `docs/gpu-pipeline.md`.
//!
//! Which backends wgpu may load (DX12 only on Windows; `LIGHTCRAFT_GPU_BACKEND`, `WGPU_BACKEND`) and
//! the crash sentinel around device creation: [`backend`] (issue #136).
//!
//! Use [`render`]: it returns `None` when the GPU is unavailable, disabled (`LIGHTCRAFT_GPU=0`,
//! `LIGHTCRAFT_GPU_BACKEND=off` or [`set_enabled`]), the render does not fit the device, or the device reported an error, ran out
//! of memory or returned an incomplete image — callers then render on the CPU. [`try_render`] is
//! the same call reporting why *this* render fell back ([`Fallback`]); [`unavailable_reason`] and
//! [`last_fallback`] (the latest fallback in the process) feed the status displays (`ui.inspect` →
//! `perf.gpuReason` / `perf.gpuFallback`).
//! The browser build has no GPU path yet (WebGPU device creation is asynchronous): everything here
//! compiles to the CPU fallback on wasm32.
//!
//! Before the process ends: [`begin_shutdown`], then [`wait_idle`] (issue #620, see `exit.rs`).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::{RenderRequest, Rendered, SourceInfo, StageCache};
use lightcraft_raster::Rgb32f;

#[cfg(not(target_arch = "wasm32"))]
pub mod backend;
#[cfg(not(target_arch = "wasm32"))]
mod ctx;
#[cfg(not(target_arch = "wasm32"))]
mod exit;
#[cfg(not(target_arch = "wasm32"))]
pub mod faces;
#[cfg(all(feature = "denoise", not(target_arch = "wasm32")))]
pub mod nn;
#[cfg(not(target_arch = "wasm32"))]
mod params;
#[cfg(not(target_arch = "wasm32"))]
mod render;

#[cfg(not(target_arch = "wasm32"))]
pub use render::GpuStages;

static ENABLED: AtomicBool = AtomicBool::new(true);
/// Set when a GPU render failed: the process stays on the CPU from then on.
static BROKEN: AtomicBool = AtomicBool::new(false);

/// Why the GPU stopped being used (the first fatal failure).
static BROKEN_REASON: Mutex<Option<String>> = Mutex::new(None);
/// The latest render that fell back to the CPU, and why.
static LAST_FALLBACK: Mutex<Option<String>> = Mutex::new(None);

/// Stop using the GPU for the rest of the process (after a device error).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn mark_broken(reason: &str) {
    let mut r = BROKEN_REASON.lock().unwrap_or_else(|e| e.into_inner());
    if r.is_none() {
        *r = Some(reason.to_string());
    }
    BROKEN.store(true, Ordering::Relaxed);
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn record_fallback(reason: String) {
    *LAST_FALLBACK.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
}

/// Why renders do not use the GPU, or `None` when they do (or will, once the device is created).
/// E.g. "disabled by LIGHTCRAFT_GPU=0", "software adapter (llvmpipe …) skipped", "device lost …".
/// Never blocks.
pub fn unavailable_reason() -> Option<String> {
    if let Some(why) = env_off_reason() {
        return Some(why.into());
    }
    if !ENABLED.load(Ordering::Relaxed) {
        return Some("disabled by the GPU rendering preference (app.gpu)".into());
    }
    if shutting_down() {
        return Some("LightCraft is closing".into());
    }
    if BROKEN.load(Ordering::Relaxed) {
        let r = BROKEN_REASON.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| "device error".into());
        return Some(format!("stopped after a GPU failure: {r}"));
    }
    if let Some(r) = broken_here() {
        return Some(format!("stopped after a GPU failure: {r}"));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        match GPU.get() {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        Some("the browser build has no GPU path yet".into())
    }
}

/// The latest render in the process that fell back from the GPU to the CPU and why (device limit,
/// out of memory, device error, incomplete or blank result), if any. A diagnostic for status
/// displays (`ui.inspect` → `perf.gpuFallback`): renders on other threads overwrite it, so a caller
/// that wants the reason for *its own* render uses [`try_render`], which returns it.
pub fn last_fallback() -> Option<String> {
    LAST_FALLBACK.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Why [`try_render`] made no GPU image: the caller renders on the CPU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// The GPU was not tried: it is unavailable, disabled or closing ([`unavailable_reason`]), or
    /// the request is one only the CPU renders (high bit depth, soft proof, HDR range overlay).
    NotTried,
    /// The render was tried on the GPU and failed, for this reason (the same text
    /// [`last_fallback`] reports). The CPU renders it instead.
    Failed(String),
}

impl Fallback {
    /// The failure's reason; `None` when the GPU was not tried.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Fallback::NotTried => None,
            Fallback::Failed(r) => Some(r),
        }
    }
}

/// A simulated GPU failure for the next [`render`] on this thread (tests of the CPU fallback).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A command the device rejects (validation error).
    Validation,
    /// The per-pixel stage's work never runs (what a driver that silently drops or resets a
    /// submission looks like: the readback holds no image).
    DropWork,
    /// A storage-buffer limit of this many bytes for this render.
    Limit(u64),
}

thread_local! {
    static FAULT: std::cell::Cell<Option<Fault>> = const { std::cell::Cell::new(None) };
    /// Why a simulated failure ([`inject_fault`]) stopped GPU use on this thread. A simulated
    /// fatal failure behaves like a real one for the thread that asked for it — [`render`] returns
    /// `None`, [`available`] is false, [`unavailable_reason`] says why — but leaves the rest of
    /// the process on the GPU: tests of the fallback run beside other tests rendering on it.
    static BROKEN_HERE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Make the next [`render`] on this thread fail with `f` (tests). A fatal `f` then stops GPU use
/// on this thread only, until [`reset_failures`].
#[doc(hidden)]
pub fn inject_fault(f: Fault) {
    FAULT.with(|c| c.set(Some(f)));
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn take_fault() -> Option<Fault> {
    FAULT.with(|c| c.take())
}

/// Stop using the GPU on this thread (a simulated fatal failure).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn mark_broken_here(reason: &str) {
    let _ = BROKEN_HERE.try_with(|b| {
        let mut b = b.borrow_mut();
        if b.is_none() {
            *b = Some(reason.to_string());
        }
    });
}

/// Why a simulated failure stopped GPU use on this thread, if one did.
fn broken_here() -> Option<String> {
    BROKEN_HERE.try_with(|b| b.borrow().clone()).ok().flatten()
}

/// Use the GPU again on this thread after a simulated failure ([`inject_fault`]; tests). Real
/// failures are not undone: the process stays on the CPU, and nothing another thread did changes.
#[doc(hidden)]
pub fn reset_failures() {
    let _ = BROKEN_HERE.try_with(|b| *b.borrow_mut() = None);
}

/// Allow or forbid GPU rendering at runtime (a preference). `LIGHTCRAFT_GPU=0` forbids it for the
/// whole process regardless.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Whether GPU rendering is allowed (environment, preference, no earlier failure — real, or
/// simulated on this thread).
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed) && !BROKEN.load(Ordering::Relaxed) && !env_disabled() && broken_here().is_none()
}

/// The process is about to end: no GPU work starts from now on, on any thread. [`render`] returns
/// `None` (callers render on the CPU), the devices are not created and denoise tiles fail over to
/// the CPU. There is no way back. Follow it with [`wait_idle`].
pub fn begin_shutdown() {
    #[cfg(not(target_arch = "wasm32"))]
    exit::begin_shutdown();
}

/// Whether [`begin_shutdown`] was called.
pub fn shutting_down() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        exit::shutting_down()
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// Wait up to `timeout` for the GPU work in flight on every thread to end. `true`: none is left,
/// and after [`begin_shutdown`] none can start, so the process may end; `false`: the deadline
/// passed with a render (or a device being created) still inside the driver.
pub fn wait_idle(timeout: std::time::Duration) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        exit::wait_idle(timeout)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = timeout;
        true
    }
}

/// Why GPU compute that has a device of its own (denoise, faces) is switched off by the user's settings, or `None`: the
/// environment switches and the rendering preference, but not a render-device failure (the devices are separate).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn switched_off() -> Option<String> {
    if let Some(why) = env_off_reason() {
        return Some(why.into());
    }
    (!ENABLED.load(Ordering::Relaxed)).then(|| "disabled by the GPU rendering preference (app.gpu)".into())
}

/// Why the environment switches the GPU off for the whole process (`LIGHTCRAFT_GPU=0` or
/// `LIGHTCRAFT_GPU_BACKEND=off`), or `None`. Code with a GPU path of its own (the SAM 3 model on
/// Metal) follows it too.
pub fn env_off_reason() -> Option<&'static str> {
    if !env_disabled() {
        return None;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if backend::env_off() {
        return Some("disabled by LIGHTCRAFT_GPU_BACKEND=off");
    }
    Some("disabled by LIGHTCRAFT_GPU=0")
}

/// `LIGHTCRAFT_GPU=0` (or `LIGHTCRAFT_GPU_BACKEND=off`): no GPU for the whole process.
fn env_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        let off = std::env::var("LIGHTCRAFT_GPU").is_ok_and(|v| matches!(v.trim(), "0" | "off" | "false" | "no"));
        #[cfg(not(target_arch = "wasm32"))]
        let off = off || backend::env_off();
        off
    })
}

#[cfg(not(target_arch = "wasm32"))]
static GPU: std::sync::OnceLock<Result<ctx::Gpu, String>> = std::sync::OnceLock::new();

/// The window's device, lent by [`share_window_device`]: the render device is created on it.
#[cfg(not(target_arch = "wasm32"))]
static WINDOW: Mutex<Option<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)>> = Mutex::new(None);

/// Render on the window's wgpu device instead of a device of LightCraft's own (issue #1228), so
/// that renders reach the screen as textures on it ([`try_render_texture`]) with no copy back to
/// the CPU. Call it once the window is up, before anything renders, then [`warm_up`]. Does nothing
/// once the render device exists; a device that can't run the kernels (a software adapter, too few
/// storage buffers) leaves renders on a device of their own, with read-back pixels as before.
/// The window's device must have been requested with the adapter's limits
/// ([`window_device_limits`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn share_window_device(device: wgpu::Device, queue: wgpu::Queue, info: wgpu::AdapterInfo) {
    if GPU.get().is_none() {
        *WINDOW.lock().unwrap_or_else(|e| e.into_inner()) = Some((device, queue, info));
    }
}

/// The limits to request the window's device with so renders can share it
/// ([`share_window_device`]): [`window_limits`] for this adapter.
#[cfg(not(target_arch = "wasm32"))]
pub fn window_device_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    window_limits(adapter.get_info().backend, adapter.limits())
}

/// The window device's limits on an adapter of `backend` that supports `adapter`: the adapter's
/// own, as the render device asks for. A GL adapter keeps egui's WebGL2 limits with its 8192 px
/// texture side for 4k+ displays (the kernels never run there). Never above `adapter`: a device
/// asked for more than the adapter has isn't created, and the window doesn't open.
#[cfg(not(target_arch = "wasm32"))]
pub fn window_limits(backend: wgpu::Backend, adapter: wgpu::Limits) -> wgpu::Limits {
    if backend == wgpu::Backend::Gl {
        let side = adapter.max_texture_dimension_2d.min(8192);
        return wgpu::Limits { max_texture_dimension_2d: side, ..wgpu::Limits::downlevel_webgl2_defaults() };
    }
    adapter
}

/// Set when the device lent by [`share_window_device`] was lost (driver reset, GPU hang).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
static WINDOW_LOST: AtomicBool = AtomicBool::new(false);

/// Whether the window's device, lent to renders ([`share_window_device`]), was lost: renders
/// have gone back to the CPU, but the window can't draw on it again either. The app then offers
/// a restart (with a device of its own for renders: `LIGHTCRAFT_GPU_TEXTURES=0`). A device lost
/// while renders had their own leaves the window alone, and this stays false.
pub fn window_device_lost() -> bool {
    WINDOW_LOST.load(Ordering::Relaxed)
}

/// Whether renders run on the window's device, so [`try_render_texture`] can hand them over
/// without a readback. Never creates the device.
pub fn shares_window_device() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        existing_device().is_some_and(|g| g.shared)
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn device() -> Option<&'static ctx::Gpu> {
    // turned off (preference, environment): don't even load the driver (issue #136)
    if env_disabled() || !ENABLED.load(Ordering::Relaxed) {
        return existing_device();
    }
    if let Some(g) = GPU.get() {
        return g.as_ref().ok();
    }
    // creating the device is a long call into the driver: not once the process is ending
    let _work = exit::enter()?;
    GPU.get_or_init(|| {
        let Some(backends) = backend::compute_backends() else { return Err("disabled by LIGHTCRAFT_GPU_BACKEND=off".into()) };
        // the window's device, when its backend is one renders may use
        let window = WINDOW.lock().unwrap_or_else(|e| e.into_inner()).take().filter(|(_, _, info)| backends.contains(info.backend.into()));
        if let Some((device, queue, info)) = window {
            let backend: wgpu::Backends = info.backend.into();
            let name = info.name.clone();
            let r = backend::with_init_marker(backend, move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || ctx::Gpu::shared(device, queue, info)))
                    .unwrap_or_else(|_| Err("kernel build panicked".into()))
            });
            match r {
                Ok(g) => return Ok(g),
                // e.g. a software window adapter: a device of our own may still be better
                Err(why) => log::info!("gpu: not rendering on the window's device ({name}): {why}"),
            }
        }
        let r = backend::with_init_marker(backends, || {
            std::panic::catch_unwind(|| ctx::Gpu::new(backends)).unwrap_or_else(|_| Err("device creation panicked".into()))
        });
        if let Err(why) = &r {
            log::warn!("gpu: rendering on the CPU: {why}");
        }
        r
    })
    .as_ref()
    .ok()
}

/// The device if it has been created (never creates it).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn existing_device() -> Option<&'static ctx::Gpu> {
    GPU.get().and_then(|g| g.as_ref().ok())
}

/// Create the device and compile the kernels on a background thread now (once the window is up),
/// so the first render doesn't wait ~0.3–0.4 s for it and the UI thread never does. Does nothing
/// while GPU rendering is turned off.
pub fn warm_up() {
    #[cfg(not(target_arch = "wasm32"))]
    if !env_disabled() && ENABLED.load(Ordering::Relaxed) && GPU.get().is_none() {
        let _ = std::thread::Builder::new().name("lc-gpu-init".into()).spawn(|| {
            let _ = device();
        });
    }
}

/// Has device creation finished (successfully or not)? Never blocks — for status displays.
pub fn ready() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        env_disabled() || GPU.get().is_some()
    }
    #[cfg(target_arch = "wasm32")]
    {
        true
    }
}

/// Whether a usable GPU adapter exists and GPU rendering is enabled (creates the device on first
/// use).
pub fn available() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        enabled() && device().is_some()
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// The adapter's name and backend, e.g. "Apple M4 Pro (Metal)".
pub fn adapter_name() -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        device().map(|g| format!("{} ({:?})", g.info.name, g.info.backend))
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// Device buffers held by the renderer.
#[cfg(not(target_arch = "wasm32"))]
pub use ctx::GpuMemory;

/// Device buffers held by the renderer.
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuMemory {
    pub allocated: u64,
    pub pooled: u64,
    pub retired: u64,
}

/// Device memory held by the renderer's buffers (zero without a device).
pub fn memory() -> GpuMemory {
    #[cfg(not(target_arch = "wasm32"))]
    {
        ctx::memory()
    }
    #[cfg(target_arch = "wasm32")]
    {
        GpuMemory::default()
    }
}

/// Keep at most `bytes` of recycled buffers in the free pool from now on (trimming it now).
pub fn set_pool_limit(bytes: u64) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        ctx::POOL_LIMIT.store(bytes, Ordering::Relaxed);
        trim_pool(bytes);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = bytes;
}

/// Free recycled buffers until at most `keep` bytes stay pooled (e.g. when the app goes idle).
pub fn trim_pool(keep: u64) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(g) = existing_device()
        && let Some(_work) = exit::enter()
    {
        if keep == 0 {
            render::release_shared_source();
        }
        g.trim(keep);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = keep;
}

/// Device bytes of the one big source (a zoom window's original) shared by every view of the photo.
pub fn shared_source_bytes() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        render::shared_source_bytes()
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// Big sources uploaded to the shared copy so far (a diagnostic: one big photo is uploaded once, not once per window).
pub fn source_uploads() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        render::source_uploads()
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// Device bytes held by a view's GPU stages (kept with its [`StageCache`]); 0 when it has none.
pub fn stage_bytes(stages: &StageCache) -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        stages.peek_extension::<GpuStages>().map(|g| g.bytes()).unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = stages;
        0
    }
}

/// Render `src` with `s` on the GPU, reusing the device-resident stages kept with `stages` (the
/// view's CPU stage cache). `None`: render on the CPU instead. [`try_render`] also says why.
pub fn render(src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, stages: Option<&StageCache>) -> Option<Rendered> {
    try_render(src, info, s, req, stages).ok()
}

/// Whether the GPU renders requests of this kind at all (a given render can still fall back to
/// the CPU): the kernel writes 8-bit output, so high-bit-depth exports render on the CPU, and so do
/// soft proofs against an output-device profile (`device_proof`: its 3D table is CPU-side) and
/// Visualize HDR, which needs the float HDR render. [`try_render`] follows it, and the loupe sizes
/// its slider-drag drafts by it (issue #330), so the two can't drift apart.
pub fn handles(depth: lightcraft_pipeline::OutputDepth, device_proof: bool, overlay: lightcraft_pipeline::Overlay) -> bool {
    depth == lightcraft_pipeline::OutputDepth::U8 && !device_proof && overlay != lightcraft_pipeline::Overlay::HdrRange
}

/// Whether `req` soft-proofs against an output-device profile, which [`handles`] leaves to the CPU.
fn device_proof(req: &RenderRequest) -> bool {
    req.proof.is_some_and(|p| lightcraft_pipeline::proof_profile(p.profile).is_some())
}

/// [`render`], reporting why this render made no GPU image: [`Fallback::NotTried`] when the GPU
/// was not used (callers can ask [`unavailable_reason`]), [`Fallback::Failed`] with the reason
/// when the GPU render failed and the CPU must render it. The reason belongs to this call, unlike
/// [`last_fallback`], which the latest render on any thread overwrites.
pub fn try_render(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
) -> Result<Rendered, Fallback> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        attempt(
            s,
            req,
            stages,
            |r: &Rendered| (r.image.width, r.image.height),
            |gpu, s, ext, fault| render::render(gpu, src, info, s, req, ext, fault),
        )
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src, info, s, req, stages);
        Err(Fallback::NotTried)
    }
}

/// A render on the window's device, ready to draw ([`try_render_texture`]): `width × height`
/// sRGB-encoded RGBA8 pixels in an `Rgba8Unorm` texture, egui's own texture format.
pub struct GpuTexture {
    #[cfg(not(target_arch = "wasm32"))]
    pub texture: wgpu::Texture,
    pub width: usize,
    pub height: usize,
}

/// Whether [`try_render_texture`] takes requests of this kind: what [`handles`] takes, without an
/// overlay (overlays are drawn on read-back pixels), and only while renders share the window's
/// device ([`shares_window_device`]).
pub fn texture_handles(req: &RenderRequest) -> bool {
    handles(req.depth, device_proof(req), req.overlay) && req.overlay == lightcraft_pipeline::Overlay::None && shares_window_device()
}

/// [`try_render`] into a texture on the window's device, with no copy of the image back to the
/// CPU (issue #1228): for views that only draw it, such as the loupe's zoom window. There is no
/// histogram. [`Fallback::NotTried`] when [`texture_handles`] says no; the caller then renders
/// with [`try_render`] (or on the CPU). `curves`: a display profile's per-channel correction
/// ([`lightcraft_codecs`' `DisplayProfile::correction_curves`]), applied on the way into the texture.
pub fn try_render_texture(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    curves: Option<&[[u8; 256]; 3]>,
) -> Result<GpuTexture, Fallback> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if !texture_handles(req) {
            return Err(Fallback::NotTried);
        }
        attempt(
            s,
            req,
            stages,
            |t: &GpuTexture| (t.width, t.height),
            |gpu, s, ext, fault| {
                if gpu.shared { render::render_texture(gpu, src, info, s, req, ext, fault, curves) } else { None }
            },
        )
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src, info, s, req, stages, curves);
        Err(Fallback::NotTried)
    }
}

/// The pixels of a render handed over as a texture ([`try_render_texture`]), read back from the
/// device: for what can't draw it there (a headless screenshot). `None` once the device is gone.
pub fn read_texture(t: &GpuTexture) -> Option<lightcraft_raster::Rgba8> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _work = exit::enter()?;
        let gpu = existing_device()?;
        gpu.read_texture(&t.texture).map_err(|e| log::warn!("gpu: reading a render texture back: {e}")).ok()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = t;
        None
    }
}

/// `LIGHTCRAFT_GPU_LOSE_AFTER=<n>`: after `n` GPU renders the device is destroyed, as a driver
/// reset or GPU hang loses it — to see the app recover (renders back on the CPU; with a device
/// shared with the window, the restart it offers). A testing switch, like `LIGHTCRAFT_GPU_LIMITS`.
#[cfg(not(target_arch = "wasm32"))]
fn lose_device_when_asked(gpu: &ctx::Gpu) {
    static AFTER: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    static DONE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Some(n) = *AFTER.get_or_init(|| std::env::var("LIGHTCRAFT_GPU_LOSE_AFTER").ok().and_then(|v| v.trim().parse().ok())) else { return };
    if DONE.fetch_add(1, Ordering::Relaxed) + 1 == n {
        log::warn!("gpu: LIGHTCRAFT_GPU_LOSE_AFTER={n}: destroying the device");
        gpu.device.destroy();
        let _ = gpu.device.poll(wgpu::PollType::Poll);
    }
}

/// Run `go` on the GPU with the render's settings, error scopes and failure accounting: its
/// result, or why the CPU must render instead. `size` names a result in a failure's reason.
#[cfg(not(target_arch = "wasm32"))]
fn attempt<T>(
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    size: impl Fn(&T) -> (usize, usize),
    go: impl FnOnce(&ctx::Gpu, &DevelopSettings, Option<&GpuStages>, Option<Fault>) -> Option<T>,
) -> Result<T, Fallback> {
    // sections switched off with their eye render as if at their defaults (issue #316)
    let effective = s.effective();
    let s: &DevelopSettings = &effective;
    if !enabled() || !handles(req.depth, device_proof(req), req.overlay) {
        return Err(Fallback::NotTried);
    }
    let s = &*lightcraft_pipeline::settings_for(s, req);
    // in flight until this returns; not tried once the process is ending (issue #620)
    let _work = exit::enter().ok_or(Fallback::NotTried)?;
    let gpu = device().ok_or(Fallback::NotTried)?;
    let ext = stages.map(|c| c.extension::<GpuStages>());
    let fault = take_fault();
    // A simulated failure stops GPU use on this thread only (see `BROKEN_HERE`).
    let stop = |reason: &str| if fault.is_some() { mark_broken_here(reason) } else { mark_broken(reason) };
    let _ = ctx::take_failure(); // nothing left over from an earlier render on this thread
    let (r, scoped) = {
        let _scope = ctx::RenderScope::new(gpu);
        let errors = ctx::ErrorScopes::push(gpu);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| go(gpu, s, ext.as_deref(), fault)));
        (r, errors.pop())
    };
    // An error the device raised explains the readback failures that follow it; a buffer
    // over the limit explains the errors its placeholder raises.
    let failure = match (ctx::take_failure(), scoped) {
        (Some(f), _) if f.kind == ctx::FailKind::Limit => Some(f),
        (f, scoped) => scoped.or(f),
    };
    let reason = match (r, failure) {
        (Err(_), _) => {
            log::error!("gpu: render failed; using the CPU pipeline from now on");
            stop("a GPU render panicked");
            "a GPU render panicked; the GPU is not used again".to_string()
        }
        (Ok(r), Some(f)) => {
            // the view's cached device stages may hold results of the failed passes
            if let Some(e) = &ext {
                e.clear();
            }
            let what = match r.as_ref().map(&size) {
                Some((w, h)) => format!("{w}×{h}"),
                None => "GPU render".into(),
            };
            let reason = match f.kind {
                ctx::FailKind::Fatal => {
                    stop(&f.reason);
                    format!("{what}: {}; the GPU is not used again", f.reason)
                }
                ctx::FailKind::OutOfMemory => {
                    gpu.trim(0);
                    format!("{what}: {}", f.reason)
                }
                ctx::FailKind::Limit | ctx::FailKind::Blank => format!("{what}: {}", f.reason),
            };
            log::warn!("gpu: rendering on the CPU instead: {reason}");
            if render::profiling() {
                eprintln!("  gpu fallback to the CPU: {reason}");
            }
            reason
        }
        // a device error on another thread or a lost device during the render
        (Ok(Some(_)), None) if BROKEN.load(Ordering::Relaxed) => {
            format!("device failure during the render: {}", unavailable_reason().unwrap_or_default())
        }
        (Ok(Some(r)), None) => {
            lose_device_when_asked(gpu);
            return Ok(r);
        }
        // every path of `render::render` that gives up records a failure first; should one
        // not, the CPU renders it rather than nothing
        (Ok(None), None) => "GPU render: no image came back".to_string(),
    };
    record_fallback(reason.clone());
    Err(Fallback::Failed(reason))
}

#[cfg(test)]
mod tests {
    use lightcraft_pipeline::{OutputDepth, Overlay};

    // Given a request the 8-bit kernel can't draw (high bit depth, a soft proof, Visualize HDR),
    // the GPU doesn't take it; the loupe and diagnostic overlays it can draw stay on the GPU
    #[test]
    fn the_gpu_handles_8_bit_unproofed_renders_without_the_hdr_range() {
        assert!(super::handles(OutputDepth::U8, false, Overlay::None));
        assert!(super::handles(OutputDepth::U8, false, Overlay::Spots(50)));
        assert!(!super::handles(OutputDepth::U16, false, Overlay::None));
        assert!(!super::handles(OutputDepth::U8, true, Overlay::None));
        assert!(!super::handles(OutputDepth::U8, false, Overlay::HdrRange));
    }

    // The window device never asks for more than the adapter has: an adapter with a 4096 px
    // texture side (older or mobile GPUs, software adapters) would refuse 8192, and the window
    // wouldn't open; a larger side is the adapter's own
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_window_device_never_asks_above_the_adapter() {
        let adapter = |side| wgpu::Limits { max_texture_dimension_2d: side, ..wgpu::Limits::downlevel_defaults() };
        for backend in [wgpu::Backend::Metal, wgpu::Backend::Vulkan, wgpu::Backend::Dx12, wgpu::Backend::Gl] {
            assert_eq!(super::window_limits(backend, adapter(4096)).max_texture_dimension_2d, 4096, "{backend:?}");
        }
        assert_eq!(super::window_limits(wgpu::Backend::Metal, adapter(16384)).max_texture_dimension_2d, 16384);
        assert_eq!(super::window_limits(wgpu::Backend::Gl, adapter(16384)).max_texture_dimension_2d, 8192);
        assert_eq!(super::window_limits(wgpu::Backend::Vulkan, adapter(4096)), adapter(4096));
    }
}
