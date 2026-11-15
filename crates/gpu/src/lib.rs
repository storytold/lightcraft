//! The develop pipeline on the GPU (wgpu compute, WGSL kernels).
//!
//! The CPU pipeline (`lcc-pipeline`) is the reference: every kernel here is a port of a CPU
//! stage, both read the same resolved parameters ([`lcc_pipeline::Plan`],
//! [`lcc_pipeline::finish::FinishParams`]), and the equivalence tests (`tests/`) render the
//! same settings on both and bound the difference in 8-bit sRGB. Stages without a kernel run on the
//! CPU inside the same render (per-stage hybrid); see `docs/gpu-pipeline.md`.
//!
//! Which backends wgpu may load (DX12 only on Windows; `{ENV_PREFIX}_GPU_BACKEND`, `WGPU_BACKEND`) and
//! the crash sentinel around device creation: [`backend`] (issue #136). Device creation has a time
//! limit (`{ENV_PREFIX}_GPU_INIT_TIMEOUT`, issue #3068, see `init.rs`).
//!
//! Use [`render`]: it returns `None` when the GPU is unavailable, disabled (`{ENV_PREFIX}_GPU=0`,
//! `{ENV_PREFIX}_GPU_BACKEND=off` or [`set_enabled`]), the render does not fit the device, or the device reported an error, ran out
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

#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lcc_develop::DevelopSettings;
use lcc_pipeline::{RenderRequest, Rendered, SourceInfo, StageCache};
use lcc_raster::Rgb32f;

#[cfg(not(target_arch = "wasm32"))]
pub mod ai;
#[cfg(not(target_arch = "wasm32"))]
pub mod backend;
#[cfg(all(feature = "denoise", not(target_arch = "wasm32")))]
pub mod bm3d;
#[cfg(not(target_arch = "wasm32"))]
mod budget;
#[cfg(not(target_arch = "wasm32"))]
pub mod cards;
#[cfg(not(target_arch = "wasm32"))]
pub mod clip;
#[cfg(not(target_arch = "wasm32"))]
mod ctx;
#[cfg(not(target_arch = "wasm32"))]
mod exit;
#[cfg(not(target_arch = "wasm32"))]
pub mod faces;
#[cfg(not(target_arch = "wasm32"))]
mod gate;
#[cfg(not(target_arch = "wasm32"))]
mod helper;
#[cfg(not(target_arch = "wasm32"))]
mod init;
#[cfg(not(target_arch = "wasm32"))]
mod instance;
#[cfg(all(feature = "denoise", not(target_arch = "wasm32")))]
pub mod nn;
#[cfg(not(target_arch = "wasm32"))]
mod params;
#[cfg(not(target_arch = "wasm32"))]
mod render;
#[cfg(not(target_arch = "wasm32"))]
mod slot;
#[cfg(not(target_arch = "wasm32"))]
mod strokes;
#[cfg(not(target_arch = "wasm32"))]
mod upload;
#[cfg(not(target_arch = "wasm32"))]
pub mod video_lut;

#[cfg(not(target_arch = "wasm32"))]
pub use render::{DisplayFrame, GpuStages};
/// The wgpu the renderer is built on (the same crate eframe uses: hosts hand over its device).
#[cfg(not(target_arch = "wasm32"))]
pub use wgpu;

/// Every wgpu instance comes from here: one at a time, with the Vulkan loader kept loaded.
#[cfg(not(target_arch = "wasm32"))]
pub use instance::new_instance;
#[cfg(not(target_arch = "wasm32"))]
pub use slot::{EventKind, GpuEvent, MAX_RECOVERIES};

/// Run `f` with this thread's GPU renders counted as batch work (an export's photos): they take
/// turns at the GPU, behind the window's own renders (see `gate.rs`).
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

/// Device bytes a batch photo of `src_px` source and `out_px` output pixels holds in its turn: its
/// renders' own, plus an HDR rendition's float output.
#[cfg(not(target_arch = "wasm32"))]
fn turn_bytes(src_px: usize, out_px: usize) -> u64 {
    gate::job_bytes(src_px, out_px).saturating_add((out_px as u64).saturating_mul(16))
}

/// Run `f`, several batch renders of one photo (an export's SDR and HDR renditions, `src_px`
/// source and `out_px` output pixels), in one turn at the GPU: the stages the first leaves on the
/// device for the second stay its own. Outside batch work, or without a device, it is just `f`.
pub fn in_one_turn<R>(src_px: usize, out_px: usize, f: impl FnOnce() -> R) -> R {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // (the HDR rendition's float output on top of the renders' own memory)
        let bytes = turn_bytes(src_px, out_px);
        // inside a photo's turn already taken (`wait_batch_turn_any` …): that card's, as it is
        if gate::in_turn() {
            return f();
        }
        // batch work goes to the second card when there is one
        if let Some(h) = batch_helper() {
            let vram = h.vram.unwrap_or(h.max_buffer);
            return gate::on_helper(|| gate::in_one_turn(bytes, vram, &shutting_down, f));
        }
        let Some(g) = existing_device().filter(|_| enabled()) else { return f() };
        let vram = g.vram.unwrap_or(g.max_buffer);
        gate::in_one_turn(bytes, vram, &shutting_down, f)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src_px, out_px);
        f()
    }
}

/// A photo's turn at a card ([`try_batch_turn`], [`batch_turn_any`], [`wait_batch_turn_any`]),
/// held until dropped. While it is held, this thread's renders go to that card and run in the
/// turn.
pub struct BatchTurn {
    #[cfg(not(target_arch = "wasm32"))]
    _turn: Option<gate::Turn>,
    /// The turn is the second card's.
    helper: bool,
}

impl BatchTurn {
    /// Whether the photo renders on the second graphics card (else the editing card, or the
    /// processor without a device).
    pub fn on_helper(&self) -> bool {
        self.helper
    }

    /// The name of the card the photo renders on (`None` without a device: the processor).
    pub fn card_name(&self) -> Option<String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.helper { helper::current() } else { slot::current() }.map(|g| g.info.name.clone())
        }
        #[cfg(target_arch = "wasm32")]
        {
            None
        }
    }
}

/// [`in_one_turn`]'s turn for a photo of `src_px` source and `out_px` output pixels, if it is free
/// now: its renders then run in it until the result is dropped. `None` when it would have to
/// wait for other exports: the caller renders the photo on the CPU instead ([`on_cpu`]), so the
/// processor and the GPU export photos at once. Without a device (or outside batch work) there
/// is nothing to wait for: always a turn.
pub fn try_batch_turn(src_px: usize, out_px: usize) -> Option<BatchTurn> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let bytes = turn_bytes(src_px, out_px);
        // the card that would finish it soonest first (`gate::order`), then the other: both
        // cards export at once
        if batch_helper().is_some()
            && let Some(t) = gate::order(bytes, true).into_iter().find_map(|h| try_turn_on(h, bytes))
        {
            return Some(t);
        }
        let Some(g) = existing_device().filter(|_| enabled()) else { return Some(BatchTurn { _turn: None, helper: false }) };
        let vram = g.vram.unwrap_or(g.max_buffer);
        gate::try_turn(bytes, vram, false).map(|t| BatchTurn { _turn: Some(t), helper: false })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src_px, out_px);
        Some(BatchTurn { helper: false })
    }
}

/// A turn for the photo needing `bytes` on the second card (`helper`) or the editing card alone,
/// if that card has a device and room now: this thread's renders go to it while the turn is held.
/// `None`: no device there, or it is busy (or, on the editing card, the interface is).
#[cfg(not(target_arch = "wasm32"))]
fn try_turn_on(helper: bool, bytes: u64) -> Option<BatchTurn> {
    if !allowed() || gate::cpu_only() {
        return None;
    }
    // (the editing card's device is made here on first use, as a render would; the second card's
    // only by its own set-up)
    let g = if helper { helper::current()? } else { device()? };
    let vram = g.vram.unwrap_or(g.max_buffer);
    gate::try_turn(bytes, vram, helper).map(|t| BatchTurn { _turn: Some(t), helper })
}

/// [`batch_turn_any`] on one card only: the second card (`helper`) or the card for editing, if it
/// has a device and room now (an export told to use one card). `None`: no device there, or busy.
pub fn try_batch_turn_on(helper: bool, src_px: usize, out_px: usize) -> Option<BatchTurn> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        try_turn_on(helper, turn_bytes(src_px, out_px))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (helper, src_px, out_px);
        None
    }
}

/// Whether the second graphics card is set up and takes work now. Never blocks.
pub fn helper_ready() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        allowed() && helper::current().is_some()
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// A turn for a batch photo of `src_px` source and `out_px` output pixels on whichever card has
/// room now and would finish it soonest: each card's photos running now plus this one, at the
/// card's measured speed for batch work, so both cards work in proportion to their speed and a
/// build of two photos at a time puts one on each. While the desktop app tracks the interface
/// ([`track_interaction`]) the second card takes a photo first whenever it has none, and the
/// editing card hands out turns only while the interface is idle (one at a time). `None`: both are
/// busy, or there is no GPU: the caller renders the photo on the processor. Take it inside
/// [`as_batch`]; this thread's renders go to the card while the turn is held.
pub fn batch_turn_any(src_px: usize, out_px: usize) -> Option<BatchTurn> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let bytes = turn_bytes(src_px, out_px);
        let helper = allowed() && helper::current().is_some();
        gate::order(bytes, helper).into_iter().find_map(|h| try_turn_on(h, bytes))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src_px, out_px);
        None
    }
}

/// [`batch_turn_any`], waiting until one card has room (a GPU-only export, a preview build).
/// `None` only when `stop()` says to give up (cancelled, the app closing), or at once when there
/// is no GPU to wait for (render on the processor).
pub fn wait_batch_turn_any(src_px: usize, out_px: usize, stop: &dyn Fn() -> bool) -> Option<BatchTurn> {
    loop {
        if let Some(t) = batch_turn_any(src_px, out_px) {
            return Some(t);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let gpu = allowed() && !gate::cpu_only() && (slot::current().is_some() || helper::current().is_some());
        #[cfg(target_arch = "wasm32")]
        let gpu = false;
        if !gpu || stop() || shutting_down() {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
}

/// How many cards take batch work now: the editing card (when the GPU is used: its device made,
/// or not tried yet) and the second card when it is ready. 0 without a GPU. Never blocks.
pub fn batch_cards() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if !allowed() {
            return 0;
        }
        let editing = slot::current().is_some() || !slot::tried();
        usize::from(editing) + usize::from(helper::current().is_some())
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// How many batch renders each card takes at once now: (the editing card's, the second card's; 0
/// when it isn't ready). Never blocks.
pub fn batch_jobs_per_card() -> (usize, usize) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let editing = existing_device().map_or(gate::MAX_JOBS, |g| gate::limits_of(false, g.vram.unwrap_or(g.max_buffer)).1);
        let helper = helper::current().filter(|_| allowed()).map_or(0, |g| gate::limits_of(true, g.vram.unwrap_or(g.max_buffer)).1);
        (editing, helper)
    }
    #[cfg(target_arch = "wasm32")]
    {
        (1, 0)
    }
}

/// How many photos batch work should render side by side to keep every card busy, for a caller
/// that runs `per_card` on one card: `per_card` times [`batch_cards`] (at least `per_card`, at
/// least 1). Callers still cap it by host memory (decoded sources) as before.
pub fn batch_lanes_hint(per_card: usize) -> usize {
    per_card.max(1).saturating_mul(batch_cards().max(1))
}

/// The editing card's (or, `helper`, the second card's) measured speed for batch work, in device
/// bytes of finished turns per second with batch work on it (`None` before it finished any): what
/// [`batch_turn_any`] shares photos by.
pub fn batch_rate_of(helper: bool) -> Option<f64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        gate::measured_rate(helper)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = helper;
        None
    }
}

/// GPU renders so far: `[interactive, batch]` on the editing card, then on the second card.
#[cfg(not(target_arch = "wasm32"))]
static RENDERS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    /// [`RENDERS`] of this thread only (tests run side by side).
    static THREAD_RENDERS: std::cell::Cell<[u64; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// Count one GPU render that made its image on the second card (`helper`) or the editing card.
#[cfg(not(target_arch = "wasm32"))]
fn count_render(helper: bool) {
    let i = usize::from(gate::is_batch()) + if helper { 2 } else { 0 };
    if let Some(n) = RENDERS.get(i) {
        n.fetch_add(1, Ordering::Relaxed);
    }
    let _ = THREAD_RENDERS.try_with(|c| {
        let mut n = c.get();
        if let Some(k) = n.get_mut(i) {
            *k = k.saturating_add(1);
        }
        c.set(n);
    });
}

/// GPU renders since the process started, by card and kind of work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderCounts {
    /// On the editing card: for what is on screen, and batch work (exports, preview builds).
    pub editing_interactive: u64,
    pub editing_batch: u64,
    /// On the second graphics card (thumbnails sent there count as interactive).
    pub helper_interactive: u64,
    pub helper_batch: u64,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
impl RenderCounts {
    fn of(n: [u64; 4]) -> RenderCounts {
        RenderCounts { editing_interactive: n[0], editing_batch: n[1], helper_interactive: n[2], helper_batch: n[3] }
    }
}

/// See [`RenderCounts`] (the whole process).
pub fn render_counts() -> RenderCounts {
    #[cfg(not(target_arch = "wasm32"))]
    {
        RenderCounts::of(RENDERS.each_ref().map(|n| n.load(Ordering::Relaxed)))
    }
    #[cfg(target_arch = "wasm32")]
    {
        RenderCounts::default()
    }
}

/// [`render_counts`] of this thread only (tests).
#[doc(hidden)]
pub fn thread_render_counts() -> RenderCounts {
    #[cfg(not(target_arch = "wasm32"))]
    {
        RenderCounts::of(THREAD_RENDERS.try_with(std::cell::Cell::get).unwrap_or_default())
    }
    #[cfg(target_arch = "wasm32")]
    {
        RenderCounts::default()
    }
}

/// The interface was touched (input, a slider, a brush, a pan, a photo change): batch work leaves
/// the editing card to it for about a second (only once [`track_interaction`] is on). The desktop
/// app calls it on every frame with input.
pub fn note_interaction() {
    #[cfg(not(target_arch = "wasm32"))]
    gate::note_interaction();
}

/// Let batch work on the editing card yield to the interface: turns there only after ~1 s with no
/// input and no view render, one job at a time while a second card works, and a running batch
/// render pauses between submissions the moment interaction starts. Off by default (the CLI, MCP
/// and tests behave as before); the desktop app turns it on.
pub fn track_interaction(on: bool) {
    #[cfg(not(target_arch = "wasm32"))]
    gate::track(on);
    #[cfg(target_arch = "wasm32")]
    let _ = on;
}

/// Whether batch work would have to leave the editing card to the interface now (a view renders,
/// or with [`track_interaction`] on, input or a view render within the last ~1 s).
pub fn interface_busy() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        gate::interface_busy()
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// How long the interface must be untouched before batch work uses the editing card (tests).
#[doc(hidden)]
pub fn set_interaction_idle(d: std::time::Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    gate::set_idle(d);
    #[cfg(target_arch = "wasm32")]
    let _ = d;
}

/// Run `f` with this thread's renders on the CPU ([`try_render`] doesn't try the GPU): a photo
/// whose turn at a busy GPU isn't free.
pub fn on_cpu<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(not(target_arch = "wasm32"))]
    {
        gate::on_cpu(f)
    }
    #[cfg(target_arch = "wasm32")]
    {
        f()
    }
}

/// Batch renders don't start for `d` from now: the window needs the GPU. Renders already running
/// finish.
pub fn hold_batch(d: std::time::Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    gate::hold(d);
    #[cfg(target_arch = "wasm32")]
    let _ = d;
}

/// How much batch work may be on the GPU at once: device `bytes` (`None`: a share of the card's
/// memory) and at most `jobs` renders (`None`: up to 4).
pub fn set_batch_limits(bytes: Option<u64>, jobs: Option<usize>) {
    #[cfg(not(target_arch = "wasm32"))]
    gate::set_limits(bytes, jobs);
    #[cfg(target_arch = "wasm32")]
    let _ = (bytes, jobs);
}

/// Pin the bytes the joint colour noise reduction's bands may take on the device, for every render
/// from now on (`None`: plan them from the device's memory and the renders on it again). Planned from
/// the device, the plan follows the renders in flight, and when no band fits the step runs on the CPU,
/// which agrees with the device only to within an 8-bit level; a pinned plan stays put, so two renders
/// of one photo take the same path. For tests that compare renders bit for bit
/// (`<PREFIX>_NR_GPU_BAND_MB` pins it from outside).
#[doc(hidden)]
pub fn set_nr_band_budget(bytes: Option<usize>) {
    #[cfg(not(target_arch = "wasm32"))]
    render::NR_BAND_BUDGET.store(bytes.map_or(0, |b| b.max(1)), Ordering::Relaxed);
    #[cfg(target_arch = "wasm32")]
    let _ = bytes;
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
    /// Running batch renders on the editing card that paused for the interface so far, and their
    /// total pause (ms).
    pub pauses: u64,
    pub paused_ms: u64,
}

/// See [`BatchUsage`] (zero without a device).
pub fn batch_usage() -> BatchUsage {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (jobs, bytes, waiting, waits, waited_ms) = gate::usage();
        let (limit_bytes, limit_jobs) = gate::limits(adapter_memory().unwrap_or(0));
        let (pauses, paused_ms) = gate::pauses();
        BatchUsage { jobs, bytes, waiting, limit_bytes, limit_jobs, waits, waited_ms, pauses, paused_ms }
    }
    #[cfg(target_arch = "wasm32")]
    {
        BatchUsage::default()
    }
}

/// The graphics card's memory (bytes) as its backend reports it (`lcc_gpumem`), else the largest
/// buffer the device allows; `None` before the device exists or without one.
pub fn adapter_memory() -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        existing_device().map(|g| g.vram.unwrap_or(g.max_buffer))
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// What happened to the GPU in this session after event number `after` (0: everything kept), in
/// plain words: it stopped responding, it was restarted, the app gave up on it.
#[cfg(not(target_arch = "wasm32"))]
pub fn events_after(after: u64) -> Vec<GpuEvent> {
    slot::events_after(after)
}

/// Why the GPU last failed in this session (a lost device, a device error), even when a new device
/// has replaced it since; `None` when it never did.
pub fn last_loss() -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::last_loss()
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
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

/// Treat the second graphics card's device as lost (tests): it is not used again until it is
/// chosen again; the editing card stays. `false` without one.
#[doc(hidden)]
pub fn simulate_helper_loss(reason: &str) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        helper::current().is_some_and(|g| helper::lose(g.generation, reason))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = reason;
        false
    }
}

/// Which device renders now (each one created gets the next number); `None` without one.
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

static ENABLED: AtomicBool = AtomicBool::new(true);
/// The latest render that fell back to the CPU, and why.
static LAST_FALLBACK: Mutex<Option<String>> = Mutex::new(None);

/// The device in use failed (a device error, a lost device, a render that panicked): renders go
/// to the CPU until it is replaced (`slot.rs`: after a short pause, a new device). `generation`
/// 0: whichever device is in use.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn mark_broken(generation: u64, reason: &str) {
    // (the second card's failure stops only it)
    #[cfg(not(target_arch = "wasm32"))]
    if !helper::lose(generation, reason) {
        slot::lose(generation, reason);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (generation, reason);
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn record_fallback(reason: String) {
    *LAST_FALLBACK.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
}

/// Why renders do not use the GPU, or `None` when they do (or will, once the device is created).
/// E.g. "disabled by {ENV_PREFIX}_GPU=0", "software adapter (llvmpipe …) skipped", "device lost …".
/// Never blocks.
pub fn unavailable_reason() -> Option<String> {
    if let Some(why) = env_off_reason() {
        return Some(why);
    }
    if !ENABLED.load(Ordering::Relaxed) {
        return Some("disabled by the GPU rendering preference (app.gpu)".into());
    }
    if shutting_down() {
        return Some("the app is closing".into());
    }
    if let Some(r) = broken_here() {
        return Some(format!("stopped after a GPU failure: {r}"));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(r) = slot::lost_reason() {
            return Some(if slot::recoveries() >= slot::MAX_RECOVERIES {
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
    /// Every buffer the render asks for finds the device out of memory, on the first attempt
    /// only: the retry with the device to itself succeeds.
    OutOfMemory,
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
/// failures are not undone here (a new device replaces a lost one: see `slot.rs`), nor anything
/// another thread did.
#[doc(hidden)]
pub fn reset_failures() {
    let _ = BROKEN_HERE.try_with(|b| *b.borrow_mut() = None);
}

/// Allow or forbid GPU rendering at runtime (a preference). `{ENV_PREFIX}_GPU=0` forbids it for the
/// whole process regardless.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Whether GPU rendering is allowed and the device is usable now (environment, preference, no
/// failure waiting for the device to be replaced, no simulated failure on this thread).
pub fn enabled() -> bool {
    allowed() && lost().is_none()
}

/// GPU rendering allowed (environment, preference, no simulated failure on this thread), whatever
/// the device's state (a lost device is replaced by the next [`device`] call after the pause).
fn allowed() -> bool {
    ENABLED.load(Ordering::Relaxed) && !env_disabled() && broken_here().is_none()
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
        return Some(why);
    }
    (!ENABLED.load(Ordering::Relaxed)).then(|| "disabled by the GPU rendering preference (app.gpu)".into())
}

/// Why the environment switches the GPU off for the whole process (`{ENV_PREFIX}_GPU=0` or
/// `{ENV_PREFIX}_GPU_BACKEND=off`), or `None`. Code with a GPU path of its own (the SAM 3 model on
/// Metal) follows it too.
pub fn env_off_reason() -> Option<String> {
    if !env_disabled() {
        return None;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if backend::env_off() {
        return Some(format!("disabled by {}=off", lcc_brand::env_var("GPU_BACKEND")));
    }
    Some(format!("disabled by {}=0", lcc_brand::env_var("GPU")))
}

/// `{ENV_PREFIX}_GPU=0` (or `{ENV_PREFIX}_GPU_BACKEND=off`): no GPU for the whole process.
fn env_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        let off = lcc_brand::env("GPU").is_some_and(|v| matches!(v.trim(), "0" | "off" | "false" | "no"));
        #[cfg(not(target_arch = "wasm32"))]
        let off = off || backend::env_off();
        off
    })
}

/// The render device: created on first use; after a failure, replaced once a short pause is over
/// (`None` meanwhile, and while GPU rendering is turned off: render on the CPU). See `slot.rs`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn device() -> Option<Arc<ctx::Gpu>> {
    // turned off (preference, environment): don't even load the driver (issue #136)
    if env_disabled() || !ENABLED.load(Ordering::Relaxed) {
        return slot::current();
    }
    if let Some(g) = slot::current() {
        return Some(g);
    }
    // creating the device is a long call into the driver: not once the process is ending
    let _work = exit::enter()?;
    slot::get(new_device)
}

/// A device of our own on the best adapter (see [`ctx::Gpu::new`]): on a thread of its own (under
/// the crash sentinel, counted as GPU work), waited for at most `init::limit()`: a driver or
/// display server that never answers leaves the GPU unavailable instead of every render waiting
/// forever (#3068).
#[cfg(not(target_arch = "wasm32"))]
fn new_device() -> Result<ctx::Gpu, String> {
    let Some(backends) = backend::compute_backends() else { return Err(format!("disabled by {}=off", lcc_brand::env_var("GPU_BACKEND"))) };
    // the window's device, when the host offered it, the editing card is the screen's and this
    // isn't a replacement after a loss (that is always one of our own)
    let window = WINDOW_DEVICE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().filter(|_| slot::lost_reason().is_none());
    init::create(backends, move || editing_device(backends, window))
}

/// The editing card's device ([`cards::resolve_card`]): on the window's device when it is that
/// card and suits, else one of its own on it.
#[cfg(not(target_arch = "wasm32"))]
fn editing_device(backends: wgpu::Backends, window: Option<WindowDevice>) -> Result<ctx::Gpu, String> {
    let instance = ctx::instance(backends);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let entries = remember_adapters(&adapters);
    let saved = cards::card();
    let res = cards::resolve_card(&entries, backend::wanted_adapter().as_deref(), saved.as_ref());
    if let Some(k) = res.missing.as_ref().filter(|k| cards::first_report_of_missing(k)) {
        slot::event(
            EventKind::Lost,
            format!("The graphics card chosen for editing ({}) isn't there; {} uses the automatic choice for now.", k.name, lcc_brand::DISPLAY_NAME),
        );
    }
    let chosen = res.index.and_then(|i| entries.get(i).zip(adapters.get(i)));
    *EDITING_CARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = chosen.map(|(e, _)| e.key.clone());
    let on_screen = chosen.is_none_or(|(e, _)| e.drives_screen);
    if let Some(w) = window.filter(|_| on_screen) {
        if w.info.device_type == wgpu::DeviceType::Cpu {
            *WINDOW_DECLINED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(format!("software adapter ({}) skipped: the CPU pipeline is faster", w.info.name));
        } else {
            match ctx::Gpu::adopt(w.device, w.queue, w.info, w.vram) {
                Ok(g) => {
                    *WINDOW_DECLINED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    if chosen.is_none() {
                        let key = cards::AdapterKey::of(&g.info, 1);
                        *EDITING_CARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                            entries.iter().find(|e| e.key.same_model(&key)).map(|e| e.key.clone());
                    }
                    return Ok(g);
                }
                Err(e) => {
                    log::warn!("gpu: not rendering on the window's device ({e}); creating one");
                    *WINDOW_DECLINED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(e);
                }
            }
        }
    }
    match chosen {
        Some((_, a)) => ctx::Gpu::on_adapter(a),
        // (no card chosen or the screen's unknown: wgpu's high-performance pick, which says why it
        // can't render)
        None => {
            let g = ctx::Gpu::new(backends)?;
            let key = cards::AdapterKey::of(&g.info, 1);
            *EDITING_CARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                entries.iter().find(|e| e.key.same_model(&key)).map(|e| e.key.clone());
            Ok(g)
        }
    }
}

/// The cards `adapters` are, as the settings list them (remembered for [`adapters`]).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn remember_adapters(adapters: &[wgpu::Adapter]) -> Vec<cards::AdapterEntry> {
    let infos: Vec<_> =
        adapters.iter().map(|a| (a.get_info(), a.limits().max_storage_buffers_per_shader_stage, lcc_gpumem::adapter_memory(a))).collect();
    let entries = cards::entries_from(&infos, cards::screen().as_ref());
    cards::set_listed(entries.clone());
    entries
}

/// The editing card's key once its device was made (`None`: wgpu's own pick, or none yet).
#[cfg(not(target_arch = "wasm32"))]
static EDITING_CARD: Mutex<Option<cards::AdapterKey>> = Mutex::new(None);

/// The graphics cards the app can list, as last seen (listed when the device is first made, or
/// by [`refresh_adapters`]). Never waits for a driver.
#[cfg(not(target_arch = "wasm32"))]
pub fn adapters() -> Vec<cards::AdapterEntry> {
    cards::listed().unwrap_or_default()
}

/// List the graphics cards again (asks the drivers: not on the UI thread).
#[cfg(not(target_arch = "wasm32"))]
pub fn refresh_adapters() -> Vec<cards::AdapterEntry> {
    let Some(backends) = backend::compute_backends() else { return Vec::new() };
    let instance = ctx::instance(backends);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    remember_adapters(&adapters)
}

/// The graphics card for editing as the settings say (`""` or `auto`: Automatic, else a card's
/// key, see [`cards::AdapterKey`]). A change applies at once: the new device is made on a thread
/// of its own while renders stay on the current one, then they move over (and the second card is
/// chosen again). `true` when the choice changed.
#[cfg(not(target_arch = "wasm32"))]
pub fn set_card(setting: &str) -> bool {
    let key = cards::AdapterKey::parse(setting);
    if !cards::set_card(key) {
        return false;
    }
    if slot::tried() && !env_disabled() {
        SWITCHING.store(true, Ordering::Relaxed);
        let spawned = std::thread::Builder::new().name("lc-gpu-card".into()).spawn(|| {
            if let Some(_work) = exit::enter() {
                slot::swap(new_device());
                start_helper();
            }
            SWITCHING.store(false, Ordering::Relaxed);
        });
        if spawned.is_err() {
            SWITCHING.store(false, Ordering::Relaxed);
        }
    }
    true
}

/// A new editing card is being set up ([`set_card`]).
#[cfg(not(target_arch = "wasm32"))]
static SWITCHING: AtomicBool = AtomicBool::new(false);

/// The editing card: (the saved choice, the card renders run on, whether a change is being set
/// up). Keys are [`cards::AdapterKey`] text.
#[cfg(not(target_arch = "wasm32"))]
pub fn card_status() -> (Option<String>, Option<String>, bool) {
    let in_use = slot::current().and(EDITING_CARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone());
    (cards::card().map(|k| k.to_string()), in_use.map(|k| k.to_string()), SWITCHING.load(Ordering::Relaxed))
}

/// The second graphics card as the settings say (`""` Automatic, `off`, or a card's key; see
/// [`cards::HelperChoice`]; `<PREFIX>_GPU_HELPER` wins). Applies at once. `true` when it changed.
#[cfg(not(target_arch = "wasm32"))]
pub fn set_helper(setting: &str) -> bool {
    if !cards::set_helper(cards::HelperChoice::parse_setting(setting)) {
        return false;
    }
    if slot::tried() {
        let _ = std::thread::Builder::new().name("lc-gpu-helper-pick".into()).spawn(start_helper);
    }
    true
}

/// Choose and set up the second card now (asks the drivers: not on the UI thread).
#[cfg(not(target_arch = "wasm32"))]
fn start_helper() {
    if env_disabled() || !ENABLED.load(Ordering::Relaxed) {
        helper::stop();
        return;
    }
    let Some(backends) = backend::compute_backends() else { return helper::stop() };
    let instance = ctx::instance(backends);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let entries = remember_adapters(&adapters);
    let editing = EDITING_CARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let primary = editing.and_then(|k| entries.iter().position(|e| e.key == k));
    let pins = backend::wanted_adapter().is_some();
    let pick = cards::resolve_helper(&entries, primary, &cards::helper_choice(), pins);
    helper::restart(pick.and_then(|i| adapters.into_iter().nth(i)));
}

/// The card the first AI device goes to (AI Denoise in `nn.rs`, the face models in `faces.rs`,
/// which keep a device on each card through [`ai_adapter_on`]): the second graphics card when one
/// is chosen or found, so models never take the editing card's memory or turns, else the editing
/// card, else wgpu's high-performance pick.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn ai_adapter(instance: &wgpu::Instance, backends: wgpu::Backends) -> Result<wgpu::Adapter, String> {
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let entries = remember_adapters(&adapters);
    let res = cards::resolve_card(&entries, backend::wanted_adapter().as_deref(), cards::card().as_ref());
    let editing = EDITING_CARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let primary = editing.and_then(|k| entries.iter().position(|e| e.key == k)).or(res.index);
    let pins = backend::wanted_adapter().is_some();
    let pick = cards::resolve_helper(&entries, primary, &cards::helper_choice(), pins).or(primary);
    match pick.and_then(|i| adapters.into_iter().nth(i)) {
        Some(a) => {
            log::info!("gpu: AI models on {}", a.get_info().name);
            Ok(a)
        }
        None => backend::pick_adapter(instance, backends),
    }
}

/// The adapter for AI work on the second graphics card (`second`: `Err` when there is none, or it is
/// turned off) or on the card for editing. AI Denoise and the face models keep a device on each
/// ([`ai::Card`]).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn ai_adapter_on(instance: &wgpu::Instance, backends: wgpu::Backends, second: bool) -> Result<wgpu::Adapter, String> {
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let entries = remember_adapters(&adapters);
    let res = cards::resolve_card(&entries, backend::wanted_adapter().as_deref(), cards::card().as_ref());
    let editing = EDITING_CARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let primary = editing.and_then(|k| entries.iter().position(|e| e.key == k)).or(res.index);
    if second {
        let pins = backend::wanted_adapter().is_some();
        return cards::resolve_helper(&entries, primary, &cards::helper_choice(), pins)
            .and_then(|i| adapters.into_iter().nth(i))
            .ok_or_else(|| ai::NO_SECOND_CARD.to_string());
    }
    match primary.and_then(|i| adapters.into_iter().nth(i)) {
        Some(a) => Ok(a),
        None => backend::pick_adapter(instance, backends),
    }
}

/// The card the next AI device would be made on ([`ai_adapter`]); asks the drivers.
#[cfg(not(target_arch = "wasm32"))]
pub fn ai_card() -> Option<String> {
    let backends = backend::compute_backends()?;
    ai_adapter(&ctx::instance(backends), backends).ok().map(|a| a.get_info().name)
}

/// Batch work on the second card now: (renders running, device bytes they hold, renders waiting).
#[cfg(not(target_arch = "wasm32"))]
pub fn helper_usage() -> (usize, u64, usize) {
    gate::helper_usage()
}

/// The second card: (`off`, `starting`, `ready`, `failed` or `lost`, its name or why not).
#[cfg(not(target_arch = "wasm32"))]
pub fn helper_status() -> (&'static str, Option<String>) {
    helper::status()
}

/// The second card's device for this thread's batch work, when there is one ready.
#[cfg(not(target_arch = "wasm32"))]
fn batch_helper() -> Option<Arc<ctx::Gpu>> {
    if !allowed() || gate::cpu_only() || !(gate::is_batch() || PREFER_HELPER.try_with(|c| c.get()).unwrap_or(false)) {
        return None;
    }
    helper::current()
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    /// This thread's renders go to the second card when there is one ([`prefer_helper`]).
    static PREFER_HELPER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with this thread's renders on the second graphics card when one is ready (thumbnails,
/// grid previews: work that isn't the open photo's view), at its gate. Otherwise as without.
pub fn prefer_helper<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(not(target_arch = "wasm32"))]
    {
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = PREFER_HELPER.try_with(|c| c.set(self.0));
            }
        }
        let _restore = Restore(PREFER_HELPER.with(|c| c.replace(true)));
        f()
    }
    #[cfg(target_arch = "wasm32")]
    {
        f()
    }
}

/// The window's device, handed over by [`adopt_window_device`] for the first device to use.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct WindowDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    vram: Option<u64>,
}

#[cfg(not(target_arch = "wasm32"))]
static WINDOW_DEVICE: std::sync::Mutex<Option<WindowDevice>> = std::sync::Mutex::new(None);
/// Why the window's device was not used (it did not suit).
#[cfg(not(target_arch = "wasm32"))]
static WINDOW_DECLINED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Offer the window's device (eframe's) for the renderer's first device, instead of one of its
/// own, so finished views can be drawn without reading them back ([`try_render_display`]). Cheap:
/// the kernels are built on it when the device is first needed ([`warm_up`] or a render, off the UI
/// thread). A device that doesn't suit (a software adapter, too few storage buffers, kernels that
/// fail) is declined and one of its own is made as before. Does nothing once a device exists. A
/// device that replaces a lost one is always one of its own (renders come back as pixels again).
#[cfg(not(target_arch = "wasm32"))]
pub fn offer_window_device(device: wgpu::Device, queue: wgpu::Queue, adapter: &wgpu::Adapter) {
    if env_disabled() {
        return;
    }
    let info = adapter.get_info();
    cards::set_screen(cards::ScreenCard::of(&info));
    backend::note_window_backend(info.backend);
    let vram = lcc_gpumem::adapter_memory(adapter);
    *WINDOW_DEVICE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(WindowDevice { device, queue, info, vram });
}

/// [`offer_window_device`], then make the device now (~0.4–2 s: off the UI thread). `Err` says
/// why the window's device isn't the one renders run on.
#[cfg(not(target_arch = "wasm32"))]
pub fn adopt_window_device(device: wgpu::Device, queue: wgpu::Queue, adapter: &wgpu::Adapter) -> Result<(), String> {
    if env_disabled() || !ENABLED.load(Ordering::Relaxed) {
        return Err("GPU rendering is off".into());
    }
    if slot::tried() {
        return Err("a device of its own was created first".into());
    }
    offer_window_device(device, queue, adapter);
    let g = self::device();
    if let Some(e) = WINDOW_DECLINED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
        return Err(e);
    }
    match g {
        Some(g) if g.window => Ok(()),
        Some(_) => Err("a device of its own was created first".into()),
        None => Err(unavailable_reason().unwrap_or_else(|| "no GPU device".into())),
    }
}

/// The pixels of a texture on `device` (a [`DisplayFrame`]'s, or any `Rgba8Unorm` texture with
/// `COPY_SRC`): for a headless screenshot of a view drawn straight from the GPU, and for tests. The
/// app never needs them to draw. Waits for the GPU (at most a minute); `Err` when the copy or
/// the wait fails.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: usize,
    height: usize,
) -> Result<lcc_raster::Rgba8, String> {
    ctx::read_texture(device, queue, texture, width, height)
}

/// Whether renders run on the window's device (and so can be handed over as textures).
pub fn on_window_device() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::current().is_some_and(|g| g.window)
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// The device if it has been created, even one that failed (never creates one).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn existing_device() -> Option<Arc<ctx::Gpu>> {
    slot::existing()
}

/// Create the device and compile the kernels on a background thread now (once the window is up),
/// so the first render doesn't wait ~0.3–0.4 s for it and the UI thread never does. Does nothing
/// while GPU rendering is turned off.
pub fn warm_up() {
    #[cfg(not(target_arch = "wasm32"))]
    if !env_disabled() && ENABLED.load(Ordering::Relaxed) && !slot::tried() {
        let _ = std::thread::Builder::new().name("lc-gpu-init".into()).spawn(|| {
            if device().is_some() {
                // the second card beside it (renders never wait for that)
                start_helper();
            }
        });
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
        allowed() && device().is_some()
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

/// Measure [`GpuMemory::peak`] from now on (a benchmark).
pub fn reset_memory_peak() {
    #[cfg(not(target_arch = "wasm32"))]
    ctx::reset_peak();
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

/// Let each card's free pool hold up to `1/share` of the card's own memory while it works, when
/// that is more than [`set_pool_limit`]'s bytes (`share` 0, the default: that limit alone). The
/// idle trim ([`trim_pool`]) still empties it. See `ctx::pool_limit` for why.
pub fn set_pool_vram_share(share: u64) {
    #[cfg(not(target_arch = "wasm32"))]
    ctx::POOL_VRAM_SHARE.store(share, Ordering::Relaxed);
    #[cfg(target_arch = "wasm32")]
    let _ = share;
}

/// Keep at most `bytes` of recycled buffers in the free pool from now on (trimming it now), or
/// more while a card works when [`set_pool_vram_share`] allows it.
pub fn set_pool_limit(bytes: u64) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        ctx::POOL_LIMIT.store(bytes, Ordering::Relaxed);
        trim_pool(bytes);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = bytes;
}

/// Free recycled buffers until at most `keep` bytes stay pooled on each card (e.g. when the app
/// goes idle): the editing card's and the second card's pools. (Only the editing card's was
/// trimmed: after a two-card export or 1:1 build the second card kept its pool, up to the pool
/// limit, for the rest of the session.)
pub fn trim_pool(keep: u64) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(_work) = exit::enter() {
        if let Some(g) = existing_device() {
            if keep == 0 {
                render::release_shared_source();
                strokes::clear();
            }
            g.trim(keep);
        }
        if let Some(g) = helper::current() {
            g.trim(keep);
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = keep;
}

/// Device bytes of the Auto Mask strokes kept between renders (every device).
pub fn kept_stroke_bytes() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        strokes::bytes()
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// Blocks of Auto Mask strokes this thread's renders have taken from those kept so far (a window
/// of a frame reusing what another window of it worked out counts its blocks here).
pub fn reused_stroke_blocks() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        strokes::reused()
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// Auto Mask strokes this thread's renders refined so far, whole or in part (none when every
/// stroke a render needs was kept).
pub fn refined_auto_mask_strokes() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        strokes::refined()
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
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

/// Grade one video frame ([`lcc_pipeline::grade`]): on the GPU when it is available, else on the
/// CPU. Returns the sRGB 8-bit frame and whether the GPU rendered it.
pub fn grade_frame(img: &Arc<Rgb32f>, settings: &DevelopSettings, quality: lcc_pipeline::Quality) -> (lcc_raster::Rgba8, bool) {
    use lcc_pipeline::grade;
    let s = grade::video_settings(settings);
    let req = grade::grade_request(img.width, img.height, quality);
    let info = grade::video_source_info();
    match try_render(img, &info, &s, &req, None) {
        Ok(r) => (r.image, true),
        Err(_) => (lcc_pipeline::render(img, &info, &s, &req).image, false),
    }
}

/// Whether the GPU renders requests of this kind at all (a given render can still fall back to
/// the CPU): the kernel writes 8-bit output or, for HDR renders ([`OutputDepth::F32Hdr`]: HDR
/// views, HDR exports, gain maps), linear float; 16-bit and SDR float exports render on the CPU, and
/// so do soft proofs against an output-device profile (`device_proof`: its 3D table is CPU-side)
/// and Visualize HDR, which needs both renditions at once.
///
/// [`OutputDepth::F32Hdr`]: lcc_pipeline::OutputDepth::F32Hdr [`try_render`] follows it,
/// and the loupe sizes its slider-drag drafts by it (issue #330), so the two can't drift apart.
pub fn handles(depth: lcc_pipeline::OutputDepth, device_proof: bool, overlay: lcc_pipeline::Overlay) -> bool {
    matches!(depth, lcc_pipeline::OutputDepth::U8 | lcc_pipeline::OutputDepth::F32Hdr) && !device_proof && overlay != lcc_pipeline::Overlay::HdrRange
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
    try_render_fitted(src, info, s, req, stages, None)
}

/// [`try_render`], with the result shrunk on the device to fit `fit` (physical pixels) with
/// `lcc_raster::resample::fit_rgba8`'s Lanczos filter, when it is larger (the loupe's display fit):
/// only the fitted image is read back. The histogram stays the full render's. HDR renders,
/// overlays and windows whose spots grew them come back unfitted, for the caller to fit.
pub fn try_render_fitted(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    fit: Option<(usize, usize)>,
) -> Result<Rendered, Fallback> {
    try_render_with(src, info, s, req, stages, fit.map(|size| Fit { size, full: None }), None)
}

/// The display fit asked of a render: the size the image is drawn at, and where to put the
/// full-size image too when the caller keeps that (left `None` when no fit was made).
pub(crate) struct Fit<'a> {
    pub size: (usize, usize),
    pub full: Option<&'a mut Option<lcc_raster::Rgba8>>,
}

/// [`try_render_fitted`] that also reads back the full-size render when it fits one: for a view
/// whose render is kept at its own level (the photo's view preview) while only the fitted image
/// is drawn. The second value is `None` when nothing was fitted (the render is the full image).
pub fn try_render_fitted_keeping_full(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    fit: (usize, usize),
) -> Result<(Rendered, Option<lcc_raster::Rgba8>), Fallback> {
    let mut full = None;
    let r = try_render_with(src, info, s, req, stages, Some(Fit { size: fit, full: Some(&mut full) }), None)?;
    Ok((r, full))
}

/// [`try_render`] of an HDR render ([`lcc_pipeline::OutputDepth::F32Hdr`]) for an HDR display:
/// its floats become the HDR texture's `Rgba16Float` texels for `headroom` on the device, as
/// `lcc_engine::hdr_texture::encode_rgba16f` makes them, and only those come back with the 8-bit
/// image and histogram (the result has no `deep` samples). Four halves per pixel.
pub fn try_render_hdr_texels(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    headroom: f32,
) -> Result<(Rendered, Vec<u16>), Fallback> {
    if req.depth != lcc_pipeline::OutputDepth::F32Hdr {
        return Err(Fallback::NotTried);
    }
    let mut texels = Vec::new();
    let r = try_render_with(src, info, s, req, stages, None, Some((headroom, &mut texels)))?;
    Ok((r, texels))
}

/// [`try_render`] for the window: when the device is the window's ([`adopt_window_device`]) and the
/// render allows it (8-bit, no overlay, no window that spots grew), the image stays on the device
/// as a texture to draw (the second value; the result's image is then empty, 0 × 0, and its
/// histogram is the render's). Otherwise the pixels, as from [`try_render`], and `None`.
#[cfg(not(target_arch = "wasm32"))]
pub fn try_render_display(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
) -> Result<(Rendered, Option<DisplayFrame>), Fallback> {
    let mut frame = None;
    let r = try_render_inner(src, info, s, req, stages, None, None, Some(&mut frame))?;
    Ok((r, frame))
}

fn try_render_with(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    fit: Option<Fit<'_>>,
    texels: Option<(f32, &mut Vec<u16>)>,
) -> Result<Rendered, Fallback> {
    try_render_inner(
        src,
        info,
        s,
        req,
        stages,
        fit,
        texels,
        #[cfg(not(target_arch = "wasm32"))]
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn try_render_inner(
    src: &Arc<Rgb32f>,
    info: &SourceInfo,
    s: &DevelopSettings,
    req: &RenderRequest,
    stages: Option<&StageCache>,
    mut fit: Option<Fit<'_>>,
    mut texels: Option<(f32, &mut Vec<u16>)>,
    #[cfg(not(target_arch = "wasm32"))] mut display: Option<&mut Option<DisplayFrame>>,
) -> Result<Rendered, Fallback> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // sections switched off with their eye render as if at their defaults (issue #316)
        let effective = s.effective();
        let s: &DevelopSettings = &effective;
        let device_proof = req.proof.is_some_and(|p| lcc_pipeline::proof_profile(p.profile).is_some());
        // pixel-filter plug-ins run in the CPU pipeline
        // and film dust removal from a scanner's infrared pass (the step, and its views)
        if !allowed()
            || gate::cpu_only()
            || !handles(req.depth, device_proof, req.overlay)
            || lcc_pipeline::plugin_filters::any_active(s)
            || lcc_pipeline::film_dust::needs_cpu(info, s, req.overlay)
        {
            return Err(Fallback::NotTried);
        }
        let s = &*lcc_pipeline::settings_for(s, req);
        // in flight until this returns; not tried once the process is ending (issue #620). Batch
        // work goes to the second card when there is one (inside a turn: the turn's card)
        let on_helper = if gate::in_turn() { gate::uses_helper() } else { batch_helper().is_some() };
        let gpu = match on_helper.then(helper::current).flatten() {
            Some(h) => h,
            None => device().ok_or(Fallback::NotTried)?,
        };
        let on_helper = helper::is_helper(gpu.generation);
        // The plan (Upright, the masks' tone ranges, red eye, the settings resolved) is the
        // CPU's work: made before the turn, so an export waiting for the GPU never holds it with
        // the processor's part of its render.
        let plan = lcc_pipeline::plan(src, info, s, req);
        // An export's turn at the GPU (see `gate.rs`): it waits while other exports use the card's
        // memory, while a view renders and while the window asked for room. Views never wait.
        let out_px = match req.window {
            Some(w) => w.w.saturating_mul(w.h),
            None => {
                let (w, h) = lcc_pipeline::output_size(src.width, src.height, s, req);
                w.saturating_mul(h)
            }
        };
        let vram = gpu.vram.unwrap_or(gpu.max_buffer);
        let enter = || gate::enter(gate::job_bytes(src.width.saturating_mul(src.height), out_px), vram, &shutting_down);
        let _turn = if on_helper { gate::on_helper(enter) } else { enter() }.ok_or(Fallback::NotTried)?;
        let _work = exit::enter().ok_or(Fallback::NotTried)?;
        let ext = stages.map(|c| c.extension::<GpuStages>());
        // stages cached on a device that was replaced are no use on this one
        if let Some(e) = &ext {
            e.adopt_generation(gpu.generation);
        }
        let mut fault = take_fault();
        // A simulated failure stops GPU use on this thread only (see `BROKEN_HERE`).
        let simulated = fault.is_some();
        let stop = |reason: &str| if simulated { mark_broken_here(reason) } else { mark_broken(gpu.generation, reason) };
        // Renders share the device's memory: wait for room instead of running out of it, and after
        // running out anyway (the size is an estimate) try once more with the device to this
        // render alone before the CPU takes over (upstream #1764).
        // (each card's memory its own)
        static BUDGET: std::sync::OnceLock<budget::Budget> = std::sync::OnceLock::new();
        static HELPER_BUDGET: std::sync::OnceLock<budget::Budget> = std::sync::OnceLock::new();
        let budget = if on_helper { &HELPER_BUDGET } else { &BUDGET }.get_or_init(|| budget::Budget::new(budget::initial(gpu.max_buffer)));
        // (a window of a zoomed view works at the window's size from the one shared copy of the
        // original: counted at the whole source's size, a 1:1 view's tiles of a 45 MP raw ran one
        // at a time under the budget)
        let need = budget::need(src.width, src.height, req.window.map(|w| (w.w, w.h)));
        let mut alone = false;
        let reason = loop {
            let _permit = budget.admit(need, alone, gate::is_batch(), shutting_down).ok_or(Fallback::NotTried)?;
            let _ = ctx::take_failure(); // nothing left over from an earlier render on this thread
            let (r, scoped) = {
                let _scope = ctx::RenderScope::new(&gpu);
                let errors = ctx::ErrorScopes::push(&gpu);
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let fit = fit.as_mut().map(|f| Fit { size: f.size, full: f.full.as_deref_mut() });
                    render::render(
                        &gpu,
                        &plan,
                        src,
                        info,
                        req,
                        ext.as_deref(),
                        fault,
                        fit,
                        texels.as_mut().map(|(h, t)| (*h, &mut **t)),
                        display.as_deref_mut(),
                    )
                }));
                (r, errors.pop())
            };
            // An error the device raised explains the readback failures that follow it; a buffer
            // over the limit or out of memory explains the errors its placeholder raises.
            let failure = match (ctx::take_failure(), scoped) {
                (Some(f), _) if matches!(f.kind, ctx::FailKind::Limit | ctx::FailKind::OutOfMemory) => Some(f),
                (f, scoped) => scoped.or(f),
            };
            break match (r, failure) {
                (Err(_), _) => {
                    log::error!("gpu: render failed; using the CPU pipeline until the device is replaced");
                    stop("a GPU render panicked");
                    "a GPU render panicked; the device is replaced".to_string()
                }
                (Ok(r), Some(f)) => {
                    // the view's cached device stages may hold results of the failed passes
                    if let Some(e) = &ext {
                        e.clear();
                    }
                    let what = match &r {
                        Some(r) => format!("{}×{}", r.image.width, r.image.height),
                        None => "GPU render".into(),
                    };
                    let reason = match f.kind {
                        ctx::FailKind::Fatal => {
                            stop(&f.reason);
                            format!("{what}: {}; the device is replaced", f.reason)
                        }
                        ctx::FailKind::OutOfMemory => {
                            gpu.trim(0);
                            budget.out_of_memory();
                            if !alone {
                                fault = fault.filter(|f| *f != Fault::OutOfMemory);
                                log::info!("gpu: {what}: {}; retrying with the device to this render alone", f.reason);
                                alone = true;
                                continue;
                            }
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
                // a device error on another thread or a lost device during the render
                (Ok(Some(_)), None) if lost().is_some() => {
                    format!("device failure during the render: {}", unavailable_reason().unwrap_or_default())
                }
                (Ok(Some(r)), None) => {
                    budget.succeeded();
                    count_render(on_helper);
                    return Ok(r);
                }
                // every path of `render::render` that gives up records a failure first; should one
                // not, the CPU renders it rather than nothing
                (Ok(None), None) => "GPU render: no image came back".to_string(),
            };
        };
        // Auto Mask strokes kept by the failed render may hold results of its failed passes
        strokes::clear();
        record_fallback(reason.clone());
        Err(Fallback::Failed(reason))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src, info, s, req, stages, fit, texels);
        Err(Fallback::NotTried)
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
    use lcc_pipeline::{OutputDepth, Overlay};

    /// The helper refuses what a driver would: a function defined twice (what an automatic merge of two branches
    /// touching the same kernel can leave).
    #[test]
    fn wgsl_validation_catches_a_duplicate_definition() {
        let ok = "fn f() -> f32 { return 1.0; }\n@compute @workgroup_size(1) fn main() { _ = f(); }\n";
        assert_eq!(super::validate_wgsl("ok", ok), Ok(()));
        let twice = format!("fn f() -> f32 {{ return 2.0; }}\n{ok}");
        assert!(super::validate_wgsl("twice", &twice).is_err());
    }

    // Given a request the kernel can't draw (16-bit or SDR float, a soft proof, Visualize HDR),
    // the GPU doesn't take it; the loupe, HDR renders and diagnostic overlays stay on the GPU
    #[test]
    fn the_gpu_handles_8_bit_and_hdr_unproofed_renders_without_the_hdr_range() {
        assert!(super::handles(OutputDepth::U8, false, Overlay::None));
        assert!(super::handles(OutputDepth::F32Hdr, false, Overlay::None));
        assert!(!super::handles(OutputDepth::F32Linear, false, Overlay::None));
        assert!(super::handles(OutputDepth::U8, false, Overlay::Spots(50)));
        assert!(!super::handles(OutputDepth::U16, false, Overlay::None));
        assert!(!super::handles(OutputDepth::U8, true, Overlay::None));
        assert!(!super::handles(OutputDepth::U8, false, Overlay::HdrRange));
    }
}
