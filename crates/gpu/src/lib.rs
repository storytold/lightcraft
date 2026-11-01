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
#[cfg(not(target_arch = "wasm32"))]
mod gate;
#[cfg(all(feature = "denoise", not(target_arch = "wasm32")))]
pub mod nn;
#[cfg(not(target_arch = "wasm32"))]
mod params;
#[cfg(not(target_arch = "wasm32"))]
mod render;
#[cfg(not(target_arch = "wasm32"))]
mod slot;
#[cfg(not(target_arch = "wasm32"))]
mod vram;

#[cfg(not(target_arch = "wasm32"))]
pub use render::{DisplayFrame, GpuStages, Output};
/// The wgpu the renderer is built on (the same crate eframe uses: hosts hand over its device).
#[cfg(not(target_arch = "wasm32"))]
pub use wgpu;

#[cfg(not(target_arch = "wasm32"))]
pub use slot::{EventKind, GpuEvent, MAX_RECOVERIES};

/// Run `f` with this thread's GPU renders counted as batch work (an export's photos): they take
/// turns at the GPU sized by its memory, behind the window's own renders (see `gate.rs`).
pub fn as_batch<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(not(target_arch = "wasm32"))]
    {
        gate::as_batch(f)
    }
    #[cfg(target_arch = "wasm32")]
    {
        f()
    }
}

/// Run `f`, several batch renders of one photo (an export's SDR and HDR renditions, `src_px`
/// source and `out_px` output pixels), in one turn at the GPU: the stages the first leaves on the
/// device for the second stay its own. Outside batch work, or without a device, it is just `f`.
pub fn in_one_turn<R>(src_px: usize, out_px: usize, f: impl FnOnce() -> R) -> R {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(g) = existing_device().filter(|_| enabled()) else { return f() };
        let vram = g.vram.unwrap_or_else(|| vram::assumed(&g.info));
        gate::in_one_turn(export_job_bytes(src_px, out_px), vram, &shutting_down, f)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src_px, out_px);
        f()
    }
}

/// The GPU memory the export gate counts for one photo's export: a render of `out_px` output
/// pixels from a source of `src_px`. A gain map export's HDR rendition adds nothing to it: it
/// renders from the stages the SDR render left, and measured, its peak is the SDR export's.
pub fn export_job_bytes(src_px: usize, out_px: usize) -> u64 {
    gate::job_bytes(src_px, out_px)
}

/// Batch renders don't start for `d` from now: the window needs the GPU (it resizes, a frame was
/// slow). Renders already running finish.
pub fn hold_batch(d: std::time::Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    gate::hold(d);
    #[cfg(target_arch = "wasm32")]
    let _ = d;
}

/// How much batch work may be on the GPU at once: device `bytes` (`None`: a share of the card's
/// memory) and at most `jobs` renders (`None`: up to 4). From the install settings ("Test my
/// machine" measures them).
pub fn set_batch_limits(bytes: Option<u64>, jobs: Option<usize>) {
    #[cfg(not(target_arch = "wasm32"))]
    gate::set_limits(bytes, jobs);
    #[cfg(target_arch = "wasm32")]
    let _ = (bytes, jobs);
}

/// Batch work on the GPU now and so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatchUsage {
    /// Batch renders on the GPU now, and the device bytes they hold.
    pub jobs: usize,
    pub bytes: u64,
    /// Batch renders waiting for their turn now.
    pub waiting: usize,
    /// The device bytes and renders batch work may use ([`set_batch_limits`]).
    pub limit_bytes: u64,
    pub limit_jobs: usize,
    /// Batch renders that had to wait so far, and their total wait (ms).
    pub waits: u64,
    pub waited_ms: u64,
}

/// See [`BatchUsage`] (zero without a device).
pub fn batch_usage() -> BatchUsage {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (jobs, bytes, waiting, waits, waited_ms) = gate::usage();
        let vram = adapter_memory().unwrap_or(0);
        let (limit_bytes, limit_jobs) = gate::limits(vram);
        BatchUsage { jobs, bytes, waiting, limit_bytes, limit_jobs, waits, waited_ms }
    }
    #[cfg(target_arch = "wasm32")]
    {
        BatchUsage::default()
    }
}

/// The graphics card's memory (bytes): what the system reports, else an assumption by kind of
/// adapter; `None` before the device exists or without one.
pub fn adapter_memory() -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        existing_device().map(|g| g.vram.unwrap_or_else(|| vram::assumed(&g.info)))
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// What happened to the GPU in this session after event number `after` (0: everything kept), in
/// plain words: it stopped responding, it was restarted, LightCraft gave up on it.
#[cfg(not(target_arch = "wasm32"))]
pub fn events_after(after: u64) -> Vec<GpuEvent> {
    slot::events_after(after)
}

/// Lost devices replaced so far in this session.
pub fn recoveries() -> u32 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::recoveries()
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// Treat the device in use as lost, as a driver reset would (tests of the recovery): renders go to
/// the CPU, and a new device replaces it after the pause.
#[doc(hidden)]
pub fn simulate_device_loss(reason: &str) {
    mark_broken(0, reason);
}

/// Which device renders now (each one created gets the next number); `None` without one (tests).
#[doc(hidden)]
pub fn device_generation() -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::current().map(|g| g.generation)
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// Measure [`GpuMemory::peak`] from now on (a benchmark of one render).
pub fn reset_memory_peak() {
    #[cfg(not(target_arch = "wasm32"))]
    ctx::reset_peak();
}

/// Unit tests that create the device or swap the crash sentinel's path take turns.
#[cfg(test)]
pub(crate) static DEVICE_TEST_LOCK: Mutex<()> = Mutex::new(());

static ENABLED: AtomicBool = AtomicBool::new(true);
/// The latest render that fell back to the CPU, and why.
static LAST_FALLBACK: Mutex<Option<String>> = Mutex::new(None);

/// The device in use failed (a device error, a lost device, a render that panicked): renders go
/// to the CPU until it is replaced ([`slot`]: after a short pause, the next render creates a new
/// device). `generation` 0: whichever device is in use.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn mark_broken(generation: u64, reason: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    slot::lose(generation, reason);
    #[cfg(target_arch = "wasm32")]
    let _ = (generation, reason);
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
    if let Some(r) = broken_here() {
        return Some(format!("stopped after a GPU failure: {r}"));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(r) = slot::lost_reason() {
            return Some(if slot::gave_up() {
                format!("stopped after a GPU failure: {r}")
            } else {
                format!("paused after a GPU failure, restarting the graphics card: {r}")
            });
        }
        slot::creation_error()
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
/// failures are not undone here (a new device replaces a lost one: see `slot.rs`), nor anything another thread did.
#[doc(hidden)]
pub fn reset_failures() {
    let _ = BROKEN_HERE.try_with(|b| *b.borrow_mut() = None);
}

/// Allow or forbid GPU rendering at runtime (a preference). `LIGHTCRAFT_GPU=0` forbids it for the
/// whole process regardless.
pub fn set_enabled(on: bool) {
    // (SeqCst, as `warm_up`'s thread reads it after releasing its start: a switch back on is
    // never missed there)
    ENABLED.store(on, Ordering::SeqCst);
}

/// Whether GPU rendering is allowed and the device is usable now (environment, preference, no
/// failure waiting for the device to be replaced, no simulated failure on this thread).
pub fn enabled() -> bool {
    allowed() && lost().is_none()
}

/// GPU rendering allowed (environment, preference, no simulated failure on this thread), whatever
/// the device's state.
fn allowed() -> bool {
    ENABLED.load(Ordering::Relaxed) && !env_disabled() && broken_here().is_none() && !ON_CPU.try_with(|c| c.get()).unwrap_or(false)
}

/// Most points in one spot stroke rendered on the GPU (a long brush stroke has a few hundred).
pub const MAX_GPU_SPOT_POINTS: usize = 4096;

thread_local! {
    /// This thread's renders stay on the CPU ([`on_cpu`]).
    static ON_CPU: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with this thread's renders on the CPU (a comparison of the two in a test); the rest of
/// the process keeps the GPU.
pub fn on_cpu<R>(f: impl FnOnce() -> R) -> R {
    /// Puts the thread's flag back, also when `f` panics (review of PR #3530).
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            let was = self.0;
            let _ = ON_CPU.try_with(|c| c.set(was));
        }
    }
    let _restore = Restore(ON_CPU.with(|c| c.replace(true)));
    f()
}

/// Why the device in use failed, while it waits to be replaced (`None`: it works, or there is none).
pub fn lost() -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::lost_reason()
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
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

/// Set while [`adopt_window_device`] is about to run or running: [`device`] and [`warm_up`] then
/// don't create a device of their own (review of PR #1065: the settings' warm-up raced the
/// adoption thread and could win, so the window's device went unused).
#[cfg(not(target_arch = "wasm32"))]
static WINDOW_PENDING: AtomicBool = AtomicBool::new(false);

/// Announce (before spawning it) that [`adopt_window_device`] will run, or withdraw that when it
/// can't (`false`). Until it has finished, renders take the CPU path and [`warm_up`] does nothing.
pub fn set_window_device_pending(pending: bool) {
    #[cfg(not(target_arch = "wasm32"))]
    WINDOW_PENDING.store(pending, Ordering::Release);
    #[cfg(target_arch = "wasm32")]
    let _ = pending;
}

/// The render device: created on first use; after a failure, replaced once a short pause is over
/// (`None` meanwhile, and while GPU rendering is turned off: render on the CPU). See [`slot`].
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn device() -> Option<Arc<ctx::Gpu>> {
    // turned off (preference, environment): don't even load the driver (issue #136)
    if env_disabled() || !ENABLED.load(Ordering::Relaxed) {
        return slot::current();
    }
    if let Some(g) = slot::current() {
        return Some(g);
    }
    // the window's device is being adopted: render on the CPU until it is, rather than create a
    // device of our own first (which would leave the window's unused)
    if WINDOW_PENDING.load(Ordering::Acquire) {
        return None;
    }
    // creating the device is a long call into the driver: not once the process is ending
    let _work = exit::enter()?;
    slot::get(new_device)
}

/// A device of our own on the best adapter (see [`ctx::Gpu::new`]), under the crash sentinel.
#[cfg(not(target_arch = "wasm32"))]
fn new_device() -> Result<ctx::Gpu, String> {
    let Some(backends) = backend::compute_backends() else { return Err("disabled by LIGHTCRAFT_GPU_BACKEND=off".into()) };
    backend::with_init_marker(backends, || {
        std::panic::catch_unwind(|| ctx::Gpu::new(backends)).unwrap_or_else(|_| Err("device creation panicked".into()))
    })
}

/// The device if it has been created, even one that failed (never creates one).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn existing_device() -> Option<Arc<ctx::Gpu>> {
    slot::existing()
}

/// Create the device and compile the kernels on a background thread now (once the window is up),
/// so the first render doesn't wait ~0.3–0.4 s for it and the UI thread never does. Does nothing
/// while GPU rendering is turned off. Repeated calls start one thread at most.
pub fn warm_up() {
    #[cfg(not(target_arch = "wasm32"))]
    if !env_disabled() && ENABLED.load(Ordering::SeqCst) && !slot::tried() && !WINDOW_PENDING.load(Ordering::Acquire) {
        static STARTED: AtomicBool = AtomicBool::new(false);
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let spawned = std::thread::Builder::new().name("lc-gpu-init".into()).spawn(|| {
            loop {
                let _ = device();
                if slot::tried() || shutting_down() || env_disabled() {
                    break;
                }
                // turned off before the thread ran (nothing created): a later call may start
                // again. Released first, then the preference read again: a call made while this
                // thread still held the flag (turned back on meanwhile) returned without starting
                // one, so this thread tries again itself rather than leave "starting" for good.
                // The window's device being adopted meanwhile: that creates the device, not this.
                STARTED.store(false, Ordering::SeqCst);
                if WINDOW_PENDING.load(Ordering::Acquire) || !ENABLED.load(Ordering::SeqCst) || STARTED.swap(true, Ordering::SeqCst) {
                    break;
                }
            }
        });
        if spawned.is_err() {
            STARTED.store(false, Ordering::SeqCst);
        }
    }
}

/// Has device creation finished (successfully or not)? Never blocks — for status displays.
pub fn ready() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        env_disabled() || slot::tried()
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
    pub peak: u64,
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
/// the CPU): the kernel writes 8-bit output and float HDR (a gain map's HDR rendition, without
/// an overlay), so 16-bit and linear float exports render on the CPU, and so do soft proofs
/// against an output-device profile (`device_proof`: its 3D table is CPU-side) and Visualize HDR.
/// [`try_render`] follows it, and the loupe sizes its slider-drag drafts by it (issue #330), so the
/// two can't drift apart.
pub fn handles(depth: lightcraft_pipeline::OutputDepth, device_proof: bool, overlay: lightcraft_pipeline::Overlay) -> bool {
    use lightcraft_pipeline::{OutputDepth, Overlay};
    let depth_ok = depth == OutputDepth::U8 || (depth == OutputDepth::F32Hdr && overlay == Overlay::None);
    depth_ok && !device_proof && overlay != Overlay::HdrRange
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
        match run(src, info, s, req, stages, false)? {
            render::Output::Image(r) => Ok(r),
            // (not asked for)
            render::Output::Display { .. } => Err(Fallback::NotTried),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src, info, s, req, stages);
        Err(Fallback::NotTried)
    }
}

/// [`render`] for the window: when the device is the window's ([`adopt_window_device`]) and the
/// render allows it, the result stays on the device as a texture to draw ([`Output::Display`]);
/// otherwise pixels as from [`render`]. `None`: render on the CPU instead.
#[cfg(not(target_arch = "wasm32"))]
pub fn render_display(src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest, stages: Option<&StageCache>) -> Option<Output> {
    run(src, info, s, req, stages, true).ok()
}

/// Whether renders run on the window's device (and so can be handed over as textures).
pub fn on_window_device() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        existing_device().is_some_and(|g| g.window)
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// Render on the window's device (eframe's) instead of a device of its own, so finished views can
/// be drawn without reading them back ([`render_display`]). Builds the kernels on it (~0.4–2 s:
/// call it off the UI thread), in place of [`warm_up`]. When that fails, or the window's device
/// doesn't suit (a software adapter, too few storage buffers), a device of its own is created as
/// before; `Err` says why the window's wasn't used. Does nothing once a device exists.
#[cfg(not(target_arch = "wasm32"))]
pub fn adopt_window_device(device: wgpu::Device, queue: wgpu::Queue, info: wgpu::AdapterInfo) -> Result<(), String> {
    /// Whatever the outcome, renders may create or use a device again afterwards.
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            WINDOW_PENDING.store(false, Ordering::Release);
        }
    }
    let _done = Done;
    if env_disabled() || !ENABLED.load(Ordering::Relaxed) {
        return Err("GPU rendering is off".into());
    }
    let _work = exit::enter().ok_or("LightCraft is closing")?;
    let mut declined = None;
    let g = slot::adopt(|| {
        let adopted = if info.device_type == wgpu::DeviceType::Cpu {
            Err(format!("software adapter ({}) skipped: the CPU pipeline is faster", info.name))
        } else {
            // (building the kernels is a long call into the driver: the crash sentinel covers it)
            backend::with_init_marker(wgpu::Backends::from(info.backend), || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx::Gpu::adopt(device, queue, info)))
                    .unwrap_or_else(|_| Err("kernel setup panicked".into()))
            })
        };
        adopted.or_else(|e| {
            log::warn!("gpu: not rendering on the window's device ({e}); creating one");
            declined = Some(e);
            new_device()
        })
    });
    match (g, declined) {
        (Ok(g), None) if g.window => Ok(()),
        (_, Some(e)) => Err(e),
        (Ok(_), None) => Err("a device of its own was created first".into()),
        (Err(e), None) => Err(e),
    }
}

/// The pixels of a texture on `device` (a [`DisplayFrame`]'s, or any `Rgba8Unorm` texture with
/// `COPY_SRC`): for a headless screenshot of a view drawn straight from the GPU, and for tests. The
/// app never needs them to draw. Waits for the GPU (at most a few seconds); `Err` when the copy or
/// the wait fails.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: usize,
    height: usize,
) -> Result<lightcraft_raster::Rgba8, String> {
    // the device's generation when it is the renderer's (the window's adopted), for its device-lost record
    let generation = slot::existing().filter(|g| g.device == *device).map_or(0, |g| g.generation);
    ctx::read_texture(device, generation, queue, texture, width, height)
}

#[cfg(not(target_arch = "wasm32"))]
fn run(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    display: bool,
) -> Result<Output, Fallback> {
    {
        // sections switched off with their eye render as if at their defaults (issue #316)
        let effective = s.effective();
        let s: &DevelopSettings = &effective;
        let device_proof = req.proof.is_some_and(|p| lightcraft_pipeline::proof_profile(p.profile).is_some());
        if !allowed() || !handles(req.depth, device_proof, req.overlay) {
            return Err(Fallback::NotTried);
        }
        let s = &*lightcraft_pipeline::settings_for(s, req);
        // content-aware Fill spots (`spots::fill_box`) have no GPU kernel: the CPU renders the photo
        if s.spots.iter().any(|sp| sp.mode == lightcraft_develop::SpotMode::Fill) {
            return Err(Fallback::NotTried);
        }
        // a stroke with more points than a brush makes (a hostile or broken XMP) is one GPU
        // dispatch whose every pixel walks all of them: the CPU renders it (review of PR #3530)
        if s.spots.iter().any(|sp| sp.points.len() > MAX_GPU_SPOT_POINTS) {
            return Err(Fallback::NotTried);
        }
        // (a lost device is replaced here once its pause is over; until then the CPU renders)
        let gpu = device().ok_or(Fallback::NotTried)?;
        // An export's turn at the GPU (see `gate`): it waits while other exports use the card's
        // memory, while a view renders and while the window asked for room. Views never wait.
        let out_px = match req.window {
            Some(w) => w.w.saturating_mul(w.h),
            None => {
                let (w, h) = lightcraft_pipeline::output_size(src.width, src.height, s, req);
                w.saturating_mul(h)
            }
        };
        let vram = gpu.vram.unwrap_or_else(|| vram::assumed(&gpu.info));
        // The plan (Upright, the masks' tone ranges, red eye, the settings resolved) is the
        // CPU's work: made before the turn, so an export waiting for the GPU never holds it with
        // the processor's part of its render (note 28).
        let t = render::profiling().then(std::time::Instant::now);
        let plan = lightcraft_pipeline::plan(src, info, s, req);
        if let Some(t) = t {
            eprintln!("  gpu plan (before the turn): {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        }
        let _turn = gate::enter(gate::job_bytes(src.width.saturating_mul(src.height), out_px), vram, &shutting_down).ok_or(Fallback::NotTried)?;
        // in flight until this returns; not tried once the process is ending (issue #620)
        let _work = exit::enter().ok_or(Fallback::NotTried)?;
        let ext = stages.map(|c| c.extension::<GpuStages>());
        // stages cached on a device that was replaced are no use on this one
        if let Some(e) = &ext {
            e.adopt_generation(gpu.generation);
        }
        let fault = take_fault();
        // A simulated failure stops GPU use on this thread only (see `BROKEN_HERE`).
        let stop = |reason: &str| if fault.is_some() { mark_broken_here(reason) } else { mark_broken(gpu.generation, reason) };
        let _ = ctx::take_failure(); // nothing left over from an earlier render on this thread
        let (r, scoped) = {
            let _scope = ctx::RenderScope::new(&gpu);
            let errors = ctx::ErrorScopes::push(&gpu);
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                render::render(&gpu, plan, src, info, req, ext.as_deref(), stages, fault, display)
            }));
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
                log::error!("gpu: render panicked; rendering on the CPU until the device is replaced");
                stop("a GPU render panicked");
                if let Some(e) = &ext {
                    e.clear();
                }
                "a GPU render panicked; the CPU renders this photo and LightCraft restarts the graphics card".to_string()
            }
            (Ok(r), Some(f)) => {
                // the view's cached device stages may hold results of the failed passes
                if let Some(e) = &ext {
                    e.clear();
                }
                let what = match &r {
                    Some(Output::Image(r)) => format!("{}×{}", r.image.width, r.image.height),
                    Some(Output::Display { frame, .. }) => format!("{}×{}", frame.width, frame.height),
                    None => "GPU render".into(),
                };
                let reason = match f.kind {
                    ctx::FailKind::Fatal => {
                        stop(&f.reason);
                        format!("{what}: {}; the CPU renders this photo and LightCraft restarts the graphics card", f.reason)
                    }
                    ctx::FailKind::OutOfMemory => {
                        // an upload that ran out of memory left a zeroed buffer; the shared source
                        // may be one, and the next render of the photo would reuse it
                        render::release_shared_source();
                        gpu.trim(0);
                        format!("{what}: {}", f.reason)
                    }
                    ctx::FailKind::Limit | ctx::FailKind::Blank | ctx::FailKind::Busy => format!("{what}: {}", f.reason),
                };
                log::warn!("gpu: rendering on the CPU instead: {reason}");
                if render::profiling() {
                    eprintln!("  gpu fallback to the CPU: {reason}");
                }
                reason
            }
            // a device error on another thread or a lost device during the render: its result
            // can't be trusted
            (Ok(Some(_)), None) if !slot::is_current(gpu.generation) || slot::lost_reason().is_some() => {
                if let Some(e) = &ext {
                    e.clear();
                }
                format!("device failure during the render: {}", unavailable_reason().unwrap_or_default())
            }
            (Ok(Some(r)), None) => return Ok(r),
            // every path of `render::render` that gives up records a failure first; should one
            // not, the CPU renders it rather than nothing
            (Ok(None), None) => "GPU render: no image came back".to_string(),
        };
        record_fallback(reason.clone());
        Err(Fallback::Failed(reason))
    }
}

/// Tests: parse and validate a WGSL module with naga, as wgpu does before it hands a shader to the driver. Needs no
/// GPU, so kernels that don't build (a duplicate definition after a merge, a type error) fail every test run, not only
/// the GPU ones.
#[cfg(test)]
pub(crate) fn validate_wgsl(name: &str, src: &str) -> Result<(), String> {
    use wgpu::naga;
    let module = naga::front::wgsl::parse_str(src).map_err(|e| format!("{name}: {}", e.emit_to_string(src)))?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map(|_| ())
        .map_err(|e| format!("{name}: {}", e.emit_to_string(src)))
}

#[cfg(test)]
mod tests {
    use lightcraft_pipeline::{OutputDepth, Overlay};

    /// Review of PR #3530: a panic inside `on_cpu` leaves the thread's renders on the GPU again.
    #[test]
    fn a_panic_in_on_cpu_leaves_the_thread_as_it_was() {
        let on = || super::ON_CPU.with(|c| c.get());
        assert!(!on());
        assert!(std::panic::catch_unwind(|| super::on_cpu(|| panic!("render"))).is_err());
        assert!(!on(), "still on the CPU after the panic");
        super::on_cpu(|| {
            let _ = std::panic::catch_unwind(|| super::on_cpu(|| panic!("inner")));
            assert!(on(), "the outer on_cpu still holds");
        });
        assert!(!on());
    }

    /// The helper refuses what a driver would: a function defined twice (what an automatic merge of two branches
    /// touching the same kernel can leave).
    #[test]
    fn wgsl_validation_catches_a_duplicate_definition() {
        let ok = "fn f() -> f32 { return 1.0; }\n@compute @workgroup_size(1) fn main() { _ = f(); }\n";
        assert_eq!(super::validate_wgsl("ok", ok), Ok(()));
        let twice = format!("fn f() -> f32 {{ return 2.0; }}\n{ok}");
        assert!(super::validate_wgsl("twice", &twice).is_err());
    }

    // Given a request the 8-bit kernel can't draw (high bit depth, a soft proof, Visualize HDR),
    // the GPU doesn't take it; the loupe and diagnostic overlays it can draw stay on the GPU
    #[test]
    fn the_gpu_handles_8_bit_unproofed_renders_without_the_hdr_range() {
        assert!(super::handles(OutputDepth::U8, false, Overlay::None));
        assert!(super::handles(OutputDepth::U8, false, Overlay::Spots(50)));
        assert!(!super::handles(OutputDepth::U16, false, Overlay::None));
        assert!(!super::handles(OutputDepth::U8, true, Overlay::None));
        assert!(!super::handles(OutputDepth::U8, false, Overlay::HdrRange));
        // the float HDR rendition of a gain map (no overlay); 16-bit and linear float stay on the CPU
        assert!(super::handles(OutputDepth::F32Hdr, false, Overlay::None));
        assert!(!super::handles(OutputDepth::F32Hdr, false, Overlay::Spots(50)));
        assert!(!super::handles(OutputDepth::F32Linear, false, Overlay::None));
    }
}
