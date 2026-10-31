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
/// The graphics cards, and which one renders what.
#[cfg(not(target_arch = "wasm32"))]
pub mod cards;

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

// --------------------------------------------------------------- two cards: roles and routing

/// Which graphics card a render runs on. On a machine with one card there is only `Primary`; with
/// a second card (Settings ▸ Performance ▸ Second graphics card) background work also goes to the
/// `Helper` (design §3, §8). Interactive work (the loupe, compare, visible thumbnails, display
/// textures) is always `Primary`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Role {
    /// The editing card: the one the photo you're working on renders on (the screen's, unless
    /// another is chosen). Batch work yields to the interface on it.
    #[default]
    Primary,
    /// The second card: batch work only, never a view; its results come back as pixels.
    Helper,
}

impl Role {
    pub const ALL: [Role; 2] = [Role::Primary, Role::Helper];

    /// `editing`, `helper` (logs, the crash marker, `app.gpu`).
    pub fn id(self) -> &'static str {
        match self {
            Role::Primary => "editing",
            Role::Helper => "helper",
        }
    }
}

thread_local! {
    /// The card this thread's renders go to ([`with_role`]).
    static ROLE: std::cell::Cell<Role> = const { std::cell::Cell::new(Role::Primary) };
}

/// The card this thread's GPU renders go to ([`with_role`]; `Primary` unless set).
pub fn current_role() -> Role {
    ROLE.try_with(|r| r.get()).unwrap_or_default()
}

/// Sets this thread's role until dropped (restored on unwind too).
struct RoleGuard(Role);

impl RoleGuard {
    fn set(role: Role) -> RoleGuard {
        RoleGuard(ROLE.try_with(|r| r.replace(role)).unwrap_or_default())
    }
}

impl Drop for RoleGuard {
    fn drop(&mut self) {
        let was = self.0;
        let _ = ROLE.try_with(|r| r.set(was));
    }
}

/// Run `f` with this thread's GPU renders on card `role` — strictly: when that card has no device
/// ready (the second card not made yet, turned off, lost) they render on the CPU. Every call here
/// that acts on "the device" ([`render`], [`try_render`], [`in_one_turn`], [`enabled`],
/// [`available`], [`unavailable_reason`], [`adapter_memory`] …) acts on this card inside `f`.
/// Batch work normally takes [`batch_turn_any`] instead, which picks the card and routes for it.
pub fn with_role<R>(role: Role, f: impl FnOnce() -> R) -> R {
    let _role = RoleGuard::set(role);
    f()
}

/// Run `f` on the second card when it is ready ([`helper_ready`]), else as without it (the
/// editing card). For background work that doesn't take turns ([`batch_turn_any`] does this per
/// photo).
pub fn on_helper<R>(f: impl FnOnce() -> R) -> R {
    if helper_ready() { with_role(Role::Helper, f) } else { f() }
}

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

/// Device bytes a batch photo of `src_px` source and `out_px` output pixels holds in its turn:
/// its renders' own, plus an HDR rendition's float output.
#[cfg(not(target_arch = "wasm32"))]
fn turn_bytes(src_px: usize, out_px: usize) -> u64 {
    gate::job_bytes(src_px, out_px).saturating_add((out_px as u64).saturating_mul(16))
}

/// The card's memory for its gate (what the system says, else an assumption by kind).
#[cfg(not(target_arch = "wasm32"))]
fn gate_vram(g: &ctx::Gpu) -> u64 {
    g.vram.unwrap_or_else(|| vram::assumed(&g.info))
}

/// Run `f`, several batch renders of one photo (an export's SDR and HDR renditions, `src_px`
/// source and `out_px` output pixels), in one turn at this thread's card: the stages the first
/// leaves on the device for the second stay its own. Outside batch work, inside a [`BatchTurn`],
/// or without a device, it is just `f`.
pub fn in_one_turn<R>(src_px: usize, out_px: usize, f: impl FnOnce() -> R) -> R {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let role = current_role();
        let Some(g) = existing_device().filter(|_| enabled()) else { return f() };
        gate::in_one_turn(role, turn_bytes(src_px, out_px), gate_vram(&g), &shutting_down, f)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src_px, out_px);
        f()
    }
}

/// A photo's turn at a card ([`batch_turn_any`], [`try_batch_turn`] …), held until dropped. While
/// it is held, this thread's renders go to that card ([`BatchTurn::role`]) and run in the turn.
pub struct BatchTurn {
    // (dropped in this order: the gate's turn, then the thread's role back)
    #[cfg(not(target_arch = "wasm32"))]
    _turn: Option<gate::Turn>,
    _route: Option<RoleGuard>,
    role: Role,
}

impl BatchTurn {
    /// The card the photo renders on.
    pub fn role(&self) -> Role {
        self.role
    }

    /// The name of that card (`None` without a device: the photo renders on the CPU).
    pub fn card_name(&self) -> Option<String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            slot::current(self.role).map(|g| g.info.name.clone())
        }
        #[cfg(target_arch = "wasm32")]
        {
            None
        }
    }
}

/// [`in_one_turn`]'s turn for a photo of `src_px` source and `out_px` output pixels on this
/// thread's card, if it is free now: its renders then run in it until the result is dropped.
/// `None` when it would have to wait for other exports: the caller renders the photo on the CPU
/// instead ([`on_cpu`]), so the processor and the GPU export photos at once. Without a device (or
/// outside batch work) there is nothing to wait for: always a turn.
pub fn try_batch_turn(src_px: usize, out_px: usize) -> Option<BatchTurn> {
    let role = current_role();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(g) = existing_device().filter(|_| enabled()) else { return Some(BatchTurn { _turn: None, _route: None, role }) };
        gate::try_turn(role, turn_bytes(src_px, out_px), gate_vram(&g)).map(|t| BatchTurn { _turn: Some(t), _route: None, role })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (src_px, out_px);
        Some(BatchTurn { _route: None, role })
    }
}

/// A turn for the photo on card `role` alone, if that card has a device and room now: this
/// thread's renders go to it while the turn is held. `None`: no device there, or it is busy (or,
/// on the editing card, the interface is: §8).
pub fn try_batch_turn_on(role: Role, src_px: usize, out_px: usize) -> Option<BatchTurn> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if !allowed() {
            return None;
        }
        // (the editing card's device is made here on first use, as a render would; the second
        // card's only by its warm-up)
        let g = device_for(role)?;
        let route = RoleGuard::set(role);
        let turn = gate::try_turn(role, turn_bytes(src_px, out_px), gate_vram(&g))?;
        Some(BatchTurn { _turn: Some(turn), _route: Some(route), role })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (role, src_px, out_px);
        None
    }
}

/// A turn for a batch photo on whichever card has room now and would finish it soonest: each
/// card's photos running now plus this one, at the card's measured speed for batch work — so both
/// cards work in proportion to their speed, and a build of two photos at a time puts one on each.
/// While the desktop app tracks the interface (§8) the second card takes a photo first whenever it
/// has none, and the editing card's gate hands out turns only while the interface is idle (one at
/// a time). `None`: both are busy, or there is no GPU — the caller renders the photo on the CPU
/// (an export's `mix` mode), as [`try_batch_turn`]'s `None` did. Take it inside [`as_batch`];
/// this thread's renders go to the card while the turn is held.
pub fn batch_turn_any(src_px: usize, out_px: usize) -> Option<BatchTurn> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        gate::order(turn_bytes(src_px, out_px), helper_ready()).into_iter().find_map(|role| try_batch_turn_on(role, src_px, out_px))
    }
    #[cfg(target_arch = "wasm32")]
    {
        try_batch_turn_on(Role::Primary, src_px, out_px)
    }
}

/// How many cards take batch work now: the editing card (when the GPU is used: its device made,
/// or not tried yet) and the second card when it is ready ([`helper_ready`]). 0 without a GPU.
/// Never blocks.
pub fn batch_cards() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if !allowed() {
            return 0;
        }
        let primary = slot::current(Role::Primary).is_some() || !slot::tried(Role::Primary);
        usize::from(primary) + usize::from(helper_ready())
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// How many photos batch work should render side by side to keep every card busy, for a caller
/// that runs `per_card` on one card: `per_card` × [`batch_cards`] (at least `per_card`, at least
/// 1). Callers still cap it by host memory (decoded sources) as before.
pub fn batch_lanes_hint(per_card: usize) -> usize {
    per_card.max(1).saturating_mul(batch_cards().max(1))
}

/// Card `role`'s measured speed for batch work, in device bytes of finished turns per second with
/// batch work on it (`None` before it finished any): what [`batch_turn_any`] shares photos by.
pub fn batch_rate_of(role: Role) -> Option<f64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        gate::measured_rate(role)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = role;
        None
    }
}

/// [`batch_turn_any`], waiting until one card has room (a GPU-only export, a preview build).
/// `None` only when `stop()` says to give up (cancelled, the app closing) — or at once when there
/// is no GPU to wait for (render on the CPU).
pub fn wait_batch_turn_any(src_px: usize, out_px: usize, stop: &dyn Fn() -> bool) -> Option<BatchTurn> {
    loop {
        if let Some(t) = batch_turn_any(src_px, out_px) {
            return Some(t);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let gpu = allowed() && (slot::current(Role::Primary).is_some() || helper_ready());
        #[cfg(target_arch = "wasm32")]
        let gpu = false;
        if !gpu || stop() || shutting_down() {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
}

/// Batch renders on the editing card don't start for `d` from now: the window needs the GPU (it
/// resizes, a frame was slow). Renders already running finish.
pub fn hold_batch(d: std::time::Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    gate::hold(d);
    #[cfg(target_arch = "wasm32")]
    let _ = d;
}

/// The interface was touched (input, a slider, a brush, a pan, a photo change): batch work leaves
/// the editing card to it for about a second (§8; only once [`track_interaction`] is on). The
/// desktop app calls it on every frame with input.
pub fn note_interaction() {
    #[cfg(not(target_arch = "wasm32"))]
    gate::note_interaction();
}

/// Let batch work on the editing card yield to the interface (§8): turns there only after ~1 s
/// with no input and no view render, one job at a time while the second card works, and a running
/// batch render pauses between submissions the moment interaction starts. Off by default (the CLI,
/// MCP, tests behave as before); the desktop app turns it on.
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

/// How much batch work may be on the editing card at once: device `bytes` (`None`: a share of the
/// card's memory) and at most `jobs` renders (`None`: up to 4). From the install settings ("Test
/// my machine" measures them). [`set_batch_limits_for`] sets the second card's.
pub fn set_batch_limits(bytes: Option<u64>, jobs: Option<usize>) {
    set_batch_limits_for(Role::Primary, bytes, jobs);
}

/// [`set_batch_limits`] for card `role` (the second card's default share is 80 % of its memory:
/// it draws no window).
pub fn set_batch_limits_for(role: Role, bytes: Option<u64>, jobs: Option<usize>) {
    #[cfg(not(target_arch = "wasm32"))]
    gate::set_limits(role, bytes, jobs);
    #[cfg(target_arch = "wasm32")]
    let _ = (role, bytes, jobs);
}

/// Batch work on a card now and so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatchUsage {
    /// Batch renders on the card now, and the device bytes they hold.
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
    /// Running batch renders that paused for the interface so far, and their total pause (ms; the
    /// editing card only, §8).
    pub pauses: u64,
    pub paused_ms: u64,
}

/// The editing card's [`BatchUsage`] (zero without a device).
pub fn batch_usage() -> BatchUsage {
    batch_usage_of(Role::Primary)
}

/// Card `role`'s [`BatchUsage`].
pub fn batch_usage_of(role: Role) -> BatchUsage {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (jobs, bytes, waiting, waits, waited_ms, pauses, paused_ms) = gate::usage(role);
        let vram = adapter_memory_of(role).unwrap_or(0);
        let (limit_bytes, limit_jobs) = gate::limits(role, vram);
        BatchUsage { jobs, bytes, waiting, limit_bytes, limit_jobs, waits, waited_ms, pauses, paused_ms }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = role;
        BatchUsage::default()
    }
}

/// This thread's card's memory (bytes): what the system reports, else an assumption by kind of
/// adapter; `None` before the device exists or without one.
pub fn adapter_memory() -> Option<u64> {
    adapter_memory_of(current_role())
}

/// [`adapter_memory`] of card `role`.
pub fn adapter_memory_of(role: Role) -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::existing(role).map(|g| gate_vram(&g))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = role;
        None
    }
}

/// What happened to the GPU in this session after event number `after` (0: everything kept), in
/// plain words: it stopped responding, it was restarted, LightCraft gave up on it.
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

/// Lost devices of this thread's card replaced so far in this session.
pub fn recoveries() -> u32 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::recoveries(current_role())
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// Treat this thread's card's device as lost, as a driver reset would (tests of the recovery):
/// renders go elsewhere, and a new device replaces it after the pause.
#[doc(hidden)]
pub fn simulate_device_loss(reason: &str) {
    mark_broken(0, reason);
}

/// Which device renders on this thread's card now (each one created gets the next number); `None`
/// without one (tests).
#[doc(hidden)]
pub fn device_generation() -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::current(current_role()).map(|g| g.generation)
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
        let role = current_role();
        if let Some(r) = slot::lost_reason(role) {
            return Some(if slot::recoveries(role) >= slot::MAX_RECOVERIES {
                format!("stopped after a GPU failure: {r}")
            } else {
                format!("paused after a GPU failure, restarting the graphics card: {r}")
            });
        }
        match role {
            Role::Primary => slot::creation_error(role),
            Role::Helper => slot::creation_error(role)
                .or_else(|| slot::absent_reason(role))
                .or_else(|| (!slot::tried(role)).then(|| "the second graphics card isn't ready yet".to_string())),
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

thread_local! {
    /// This thread's renders stay on the CPU ([`on_cpu`]).
    static ON_CPU: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with this thread's renders on the CPU (an export set to the processor, a benchmark's
/// CPU run); the rest of the process keeps the GPU.
pub fn on_cpu<R>(f: impl FnOnce() -> R) -> R {
    let was = ON_CPU.with(|c| c.replace(true));
    let r = f();
    ON_CPU.with(|c| c.set(was));
    r
}

/// Why this thread's card's device failed, while it waits to be replaced (`None`: it works, or
/// there is none).
pub fn lost() -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::lost_reason(current_role())
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

/// This thread's card's render device ([`current_role`]): the editing card's is created on first
/// use, the second card's only by its warm-up ([`warm_up`]); after a failure, each is replaced once
/// a short pause is over (`None` meanwhile, and while GPU rendering is turned off: render on the
/// CPU). See [`slot`].
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn device() -> Option<Arc<ctx::Gpu>> {
    device_for(current_role())
}

/// [`device`] of card `role`.
#[cfg(not(target_arch = "wasm32"))]
fn device_for(role: Role) -> Option<Arc<ctx::Gpu>> {
    // turned off (preference, environment): don't even load the driver (issue #136)
    if env_disabled() || !ENABLED.load(Ordering::Relaxed) {
        return slot::current(role);
    }
    if let Some(g) = slot::current(role) {
        return Some(g);
    }
    // the window's device is being adopted: render on the CPU until it is, rather than create a
    // device of our own first (which would leave the window's unused)
    if WINDOW_PENDING.load(Ordering::Acquire) {
        return None;
    }
    // creating the device is a long call into the driver: not once the process is ending
    let _work = exit::enter()?;
    slot::get(role, new_device_for_role, role == Role::Primary)
}

/// A new device for card `role` (its first, or one replacing a lost one).
#[cfg(not(target_arch = "wasm32"))]
fn new_device_for_role(role: Role) -> Result<ctx::Gpu, String> {
    match role {
        Role::Primary => new_device(),
        Role::Helper => new_helper_device().and_then(|g| g.ok_or_else(|| "no second graphics card to use".to_string())),
    }
}

/// A device of our own on the editing card ([`cards::resolve_card`]), under the crash sentinel
/// (which names the card: a crash there costs the next launch only that choice). When the card
/// chosen in the settings can't render, Automatic is used instead and a notice says why.
#[cfg(not(target_arch = "wasm32"))]
fn new_device() -> Result<ctx::Gpu, String> {
    let Some(backends) = backend::compute_backends() else { return Err("disabled by LIGHTCRAFT_GPU_BACKEND=off".into()) };
    match new_device_on(backends, false) {
        Err((why, true)) => {
            log::warn!("gpu: the card chosen in the settings can't render ({why}); using Automatic");
            let auto = new_device_on(backends, true).map_err(|(e, _)| e);
            if let Ok(g) = &auto {
                slot::event(
                    Role::Primary,
                    EventKind::CardMissing,
                    format!("LightCraft couldn't use the graphics card chosen in Settings ({why}), so it uses Automatic ({}).", g.info.name),
                );
            }
            auto
        }
        r => r.map_err(|(e, _)| e),
    }
}

/// A device on the editing card (`automatic`: ignoring the setting). `Err((why, chosen))`:
/// `chosen` when it was the card chosen in the settings that failed.
#[cfg(not(target_arch = "wasm32"))]
fn new_device_on(backends: wgpu::Backends, automatic: bool) -> Result<ctx::Gpu, (String, bool)> {
    let (_instance, p) = backend::with_marker("graphics card listing", || {
        std::panic::catch_unwind(|| {
            let instance = backend::compute_instance(backends);
            backend::pick_editing(&instance, backends, automatic).map(|p| (instance, p))
        })
        .unwrap_or_else(|_| Err("listing the graphics cards panicked".into()))
    })
    .map_err(|e| (e, false))?;
    if !automatic {
        report_missing(&p.resolution, &p.entries);
    }
    let chosen = p.resolution.how == cards::How::Chosen;
    let (adapter, key) = (p.adapter, p.key);
    backend::with_init_marker_for(backends, key.as_ref(), "editing", || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx::Gpu::on_adapter(adapter, key.clone(), Role::Primary)))
            .unwrap_or_else(|_| Err("device creation panicked".into()))
    })
    .map_err(|e| (e, chosen))
}

/// This thread's card's device if it has been created, even one that failed (never creates one).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn existing_device() -> Option<Arc<ctx::Gpu>> {
    slot::existing(current_role())
}

/// Create the device and compile the kernels on a background thread now (once the window is up),
/// so the first render doesn't wait ~0.3–0.4 s for it and the UI thread never does; then the second
/// card's ([`set_helper`]: Automatic uses one when there is another card). Does nothing while GPU
/// rendering is turned off. Repeated calls start one thread at most.
pub fn warm_up() {
    #[cfg(not(target_arch = "wasm32"))]
    if !env_disabled() && ENABLED.load(Ordering::SeqCst) && slot::tried(Role::Primary) {
        // the editing card is set up: only the second card may still need its warm-up
        sync_helper();
    } else if !env_disabled() && ENABLED.load(Ordering::SeqCst) && !WINDOW_PENDING.load(Ordering::Acquire) {
        static STARTED: AtomicBool = AtomicBool::new(false);
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let spawned = std::thread::Builder::new().name("lc-gpu-init".into()).spawn(|| {
            loop {
                let _ = device_for(Role::Primary);
                if slot::tried(Role::Primary) || shutting_down() || env_disabled() {
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
            sync_helper();
        });
        if spawned.is_err() {
            STARTED.store(false, Ordering::SeqCst);
        }
    }
}

/// Has the editing card's device creation finished (successfully or not)? Never blocks — for status
/// displays.
pub fn ready() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        env_disabled() || slot::tried(Role::Primary)
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

/// Device memory held by the renderer's buffers, both cards together (zero without a device).
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

/// [`memory`] of card `role` alone (`peak`: its own since [`reset_memory_peak`]).
pub fn memory_of(role: Role) -> GpuMemory {
    #[cfg(not(target_arch = "wasm32"))]
    {
        ctx::memory_of(role)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = role;
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

/// Free recycled buffers until at most `keep` bytes stay pooled on each card (e.g. when the app
/// goes idle).
pub fn trim_pool(keep: u64) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(_work) = exit::enter() {
        if keep == 0 {
            render::release_shared_source();
        }
        for role in Role::ALL {
            if let Some(g) = slot::existing(role) {
                g.trim(keep);
            }
        }
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

/// Whether the editing card's renders run on the window's device (and so can be handed over as
/// textures).
pub fn on_window_device() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::existing(Role::Primary).is_some_and(|g| g.window)
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
    set_screen_card(&info);
    if env_disabled() || !ENABLED.load(Ordering::Relaxed) {
        return Err("GPU rendering is off".into());
    }
    let _work = exit::enter().ok_or("LightCraft is closing")?;
    // the second card builds its kernels beside the editing card's, below normal priority (both
    // took ~27 s one after the other on a busy machine: the second card was ready ~56 s after
    // launch); the screen's card is known now, so it can tell which card is the other one
    sync_helper();
    let Some(backends) = backend::compute_backends() else { return Err("disabled by LIGHTCRAFT_GPU_BACKEND=off".into()) };
    *WINDOW.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(WindowDevice { device: device.clone(), queue: queue.clone(), info: info.clone(), declined: false });
    let mut declined = None;
    let g = slot::adopt(|| {
        // another card chosen for editing (decision 2): a device of its own there
        let target = editing_target(backends);
        if !target.window {
            declined = Some(format!("the graphics card for editing ({}) isn't the screen's: renders come back as pixels", target.name));
            return new_device();
        }
        adopt_on(device, queue, info, target.key.clone()).or_else(|e| {
            log::warn!("gpu: not rendering on the window's device ({e}); creating one");
            if let Some(w) = WINDOW.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
                w.declined = true;
            }
            declined = Some(e);
            new_device()
        })
    });
    sync_helper();
    match (g, declined) {
        (Ok(g), None) if g.window => Ok(()),
        (_, Some(e)) => Err(e),
        (Ok(_), None) => Err("a device of its own was created first".into()),
        (Err(e), None) => Err(e),
    }
}

// ------------------------------------------------------------------------- choosing the card

/// A graphics card as Settings ▸ Performance lists it ([`adapters`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardInfo {
    /// What the settings save for it (`dx12:10de:2f04:NVIDIA GeForce RTX 5070#1`): pass it to
    /// [`set_card`].
    pub key: String,
    pub name: String,
    /// `dedicated`, `builtIn`, `virtual`, `software`, `other`.
    pub kind: &'static str,
    /// Its own memory (bytes), when the system says.
    pub memory: Option<u64>,
    /// wgpu's backend (`dx12`, `vulkan`, `metal`).
    pub backend: String,
    /// The screen is connected to it (the window is drawn on it).
    pub drives_screen: bool,
    /// LightCraft can render on it; else why not.
    pub usable: bool,
    pub why_not: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
impl From<cards::AdapterEntry> for CardInfo {
    fn from(e: cards::AdapterEntry) -> CardInfo {
        CardInfo {
            key: e.key.to_string(),
            name: e.name,
            kind: e.kind.id(),
            memory: e.memory,
            backend: e.backend,
            drives_screen: e.drives_screen,
            usable: e.usable,
            why_not: e.why_not,
        }
    }
}

/// The graphics cards found so far, without looking now (never blocks: for the settings); `None`
/// until they were listed ([`list_adapters`]; the GPU's warm-up lists them).
pub fn adapters() -> Option<Vec<CardInfo>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        cards::listed().map(|l| l.into_iter().map(CardInfo::from).collect())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// List the graphics cards now (a call into each driver: never on the UI thread). While the GPU is
/// turned off (preference, environment) no driver is loaded: what was found before, else nothing.
pub fn list_adapters() -> Vec<CardInfo> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if env_disabled() || !ENABLED.load(Ordering::Relaxed) {
            return adapters().unwrap_or_default();
        }
        let Some(backends) = backend::compute_backends() else { return Vec::new() };
        let Some(_work) = exit::enter() else { return adapters().unwrap_or_default() };
        let listed = backend::with_marker("graphics card listing", || {
            std::panic::catch_unwind(|| backend::enumerate(&backend::compute_instance(backends), backends).1).unwrap_or_default()
        });
        listed.into_iter().map(CardInfo::from).collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        Vec::new()
    }
}

/// The graphics card for editing (Settings ▸ Performance ▸ Graphics card for editing): `""`,
/// `auto` or `automatic` for Automatic (the screen's card), a card's key ([`CardInfo::key`]), or
/// part of the name of a card found ([`adapters`]). Applied at once: renders keep using the card
/// they are on until the new one is ready (its kernels can take up to ~30 s to build), then move
/// over. A key of a card that isn't there is kept: Automatic renders meanwhile, and a notice says
/// so once per session. `Err`: text that names no card.
pub fn set_card(setting: &str) -> Result<(), String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let key = card_key_for(setting)?;
        if cards::set_card(key) {
            switch_editing_card();
        }
        Ok(())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = setting;
        Ok(())
    }
}

/// The key [`set_card`] makes of `setting` (`None`: Automatic).
#[cfg(not(target_arch = "wasm32"))]
fn card_key_for(setting: &str) -> Result<Option<cards::AdapterKey>, String> {
    let t = setting.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("auto") || t.eq_ignore_ascii_case("automatic") {
        return Ok(None);
    }
    if let Some(k) = cards::AdapterKey::parse(t) {
        return Ok(Some(k));
    }
    let listed = cards::listed().unwrap_or_default();
    listed
        .iter()
        .find(|e| e.usable && backend::adapter_name_matches(t, &e.name))
        .map(|e| Some(e.key.clone()))
        .ok_or_else(|| format!("no graphics card found is called like \"{t}\" (app.gpu lists them under adapters)"))
}

/// The editing card's setting in force: a key, or `""` for Automatic.
pub fn card_setting() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        cards::card().map(|k| k.to_string()).unwrap_or_default()
    }
    #[cfg(target_arch = "wasm32")]
    {
        String::new()
    }
}

/// The name in a card's key (`dx12:10de:2783:NVIDIA GeForce RTX 4070 SUPER#1` → the name), for a
/// card that isn't there; `None` for text that isn't a key.
pub fn card_name(key: &str) -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        cards::AdapterKey::parse(key).map(|k| k.name)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = key;
        None
    }
}

/// The name of the card photos are edited on now (the render device's), without creating it.
pub fn card_in_use() -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        slot::existing(Role::Primary).map(|g| g.info.name.clone())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// Whether renders are moving to another card ([`set_card`]) now.
pub fn switching_card() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        SWITCHES.load(Ordering::Relaxed) > 0
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// Switches to another card running now.
#[cfg(not(target_arch = "wasm32"))]
static SWITCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The card the window is drawn on (the screen's: Automatic edits there). The desktop app tells it
/// as soon as its window has a renderer, whether or not GPU rendering is on.
#[cfg(not(target_arch = "wasm32"))]
pub fn set_screen_card(info: &wgpu::AdapterInfo) {
    if info.device_type != wgpu::DeviceType::Cpu {
        cards::set_screen(cards::ScreenCard::of(info));
    }
}

/// The window's device, kept so a switch back to the screen's card renders on it again.
#[cfg(not(target_arch = "wasm32"))]
struct WindowDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    /// Rendering on it failed once (its kernels didn't build): not tried again.
    declined: bool,
}

#[cfg(not(target_arch = "wasm32"))]
static WINDOW: Mutex<Option<WindowDevice>> = Mutex::new(None);

/// Where editing renders go now: the window's device, or a device of its own on card `key`.
#[cfg(not(target_arch = "wasm32"))]
struct Target {
    key: Option<cards::AdapterKey>,
    window: bool,
    name: String,
}

/// The editing card now (see [`cards::resolve_card`]), and whether that is the window's device:
/// Automatic and the screen's card render there; another card renders on a device of its own and
/// its renders come back as pixels (decision 2).
#[cfg(not(target_arch = "wasm32"))]
fn editing_target(backends: wgpu::Backends) -> Target {
    let entries = backend::with_marker("graphics card listing", || {
        std::panic::catch_unwind(|| backend::enumerate(&backend::compute_instance(backends), backends).1).unwrap_or_default()
    });
    let r = cards::resolve_card(&entries, backend::wanted_adapter().as_deref(), cards::card().as_ref());
    report_missing(&r, &entries);
    let window_ok = WINDOW.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_some_and(|w| !w.declined);
    let entry = r.index.and_then(|i| entries.get(i));
    Target {
        key: entry.map(|e| e.key.clone()),
        window: window_ok && entry.is_none_or(|e| e.drives_screen),
        name: entry.map_or_else(|| "the default graphics card".to_string(), |e| e.name.clone()),
    }
}

/// The notice for a chosen card that isn't there, once per session (decision 4: the choice is
/// kept, Automatic renders).
#[cfg(not(target_arch = "wasm32"))]
fn report_missing(r: &cards::Resolution, entries: &[cards::AdapterEntry]) {
    let Some(k) = &r.missing else { return };
    if !cards::first_report_of_missing(k) {
        return;
    }
    let auto = r.index.and_then(|i| entries.get(i)).map_or_else(|| "the default graphics card".to_string(), |e| e.name.clone());
    log::warn!("gpu: the card chosen in the settings ({k}) isn't there; using {auto}");
    slot::event(
        Role::Primary,
        EventKind::CardMissing,
        format!("The graphics card chosen in Settings ({}) isn't available, so LightCraft uses Automatic ({auto}).", k.name),
    );
}

/// Move editing renders to the card chosen now, on a thread of its own (building the kernels takes
/// seconds): renders stay on the current device until the new one is ready. Nothing to do before
/// the device exists (the choice applies when it is made) or while GPU rendering is off.
#[cfg(not(target_arch = "wasm32"))]
fn switch_editing_card() {
    if env_disabled() || !ENABLED.load(Ordering::Relaxed) || !slot::tried(Role::Primary) {
        return;
    }
    SWITCHES.fetch_add(1, Ordering::Relaxed);
    let spawned = std::thread::Builder::new().name("lc-gpu-switch".into()).spawn(|| {
        if let Some(_work) = exit::enter() {
            {
                let _one = slot::creating(Role::Primary);
                switch_now();
            }
            // the second card may be the new editing card, or the old one free to help now
            sync_helper();
        }
        SWITCHES.fetch_sub(1, Ordering::Relaxed);
    });
    if let Err(e) = spawned {
        log::warn!("gpu: can't switch cards: {e}");
        SWITCHES.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn switch_now() {
    let Some(backends) = backend::compute_backends() else { return };
    let target = editing_target(backends);
    let current = slot::existing(Role::Primary);
    let there = current.as_ref().is_some_and(|g| g.window == target.window && (target.window || g.key == target.key));
    if there && slot::lost_reason(Role::Primary).is_none() {
        return;
    }
    let from = current.as_ref().map_or_else(|| "the processor".to_string(), |g| format!("the {}", g.info.name));
    drop(current);
    let made = if target.window { adopt_stored_window(&target) } else { new_device() };
    match made {
        Ok(g) => {
            let name = g.info.name.clone();
            log::info!("gpu: editing renders move to {name}{}", if g.window { " (the window's device)" } else { "" });
            slot::put(Role::Primary, g);
            slot::event(Role::Primary, EventKind::CardSwitched, format!("Photos now render on the {name}."));
        }
        Err(why) => {
            log::warn!("gpu: switching to {} failed: {why}", target.name);
            slot::event(
                Role::Primary,
                EventKind::CardSwitched,
                format!("LightCraft couldn't switch to the {} ({why}); photos keep rendering on {from}.", target.name),
            );
        }
    }
}

/// The renderer on the window's device kept by [`adopt_window_device`] (a switch back to the
/// screen's card), under the crash sentinel.
#[cfg(not(target_arch = "wasm32"))]
fn adopt_stored_window(target: &Target) -> Result<ctx::Gpu, String> {
    let w = WINDOW.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().map(|w| (w.device.clone(), w.queue.clone(), w.info.clone()));
    let Some((device, queue, info)) = w else { return Err("the window's device is gone".into()) };
    let made = adopt_on(device, queue, info, target.key.clone());
    if made.is_err()
        && let Some(w) = WINDOW.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut()
    {
        w.declined = true;
    }
    made
}

/// The renderer on the window's device (`info`'s card `key`), under the crash sentinel; never a
/// panic.
#[cfg(not(target_arch = "wasm32"))]
fn adopt_on(device: wgpu::Device, queue: wgpu::Queue, info: wgpu::AdapterInfo, key: Option<cards::AdapterKey>) -> Result<ctx::Gpu, String> {
    if info.device_type == wgpu::DeviceType::Cpu {
        return Err(format!("software adapter ({}) skipped: the CPU pipeline is faster", info.name));
    }
    // (building the kernels is a long call into the driver: the crash sentinel covers it)
    backend::with_init_marker_for(wgpu::Backends::from(info.backend), key.as_ref(), "editing", || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx::Gpu::adopt(device, queue, info)))
            .unwrap_or_else(|_| Err("kernel setup panicked".into()))
            .map(|mut g| {
                g.key = key.clone();
                g
            })
    })
}

// ------------------------------------------------------------------------- the second card

/// The second graphics card (Settings ▸ Performance ▸ Second graphics card): `""`, `auto` or
/// `automatic` for Automatic (another usable card than the editing one, dedicated first; none
/// while `LIGHTCRAFT_GPU_ADAPTER` keeps the work on one card), `off` for none, a card's key
/// ([`CardInfo::key`]), part of the name of a card found, or `same` (a second device on the editing
/// card: tests and benchmarks of the two-device logic on one card). Applied at once on a thread of
/// its own: batch work uses the new card once its kernels are built. `LIGHTCRAFT_GPU_HELPER`
/// overrides it. `Err`: text that names no card.
pub fn set_helper(setting: &str) -> Result<(), String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let t = setting.trim();
        let choice = match t.to_ascii_lowercase().as_str() {
            "same" => cards::HelperChoice::Same,
            _ => match cards::HelperChoice::parse_setting(t) {
                cards::HelperChoice::Auto if !matches!(t.to_ascii_lowercase().as_str(), "" | "auto" | "automatic") => {
                    let listed = cards::listed().unwrap_or_default();
                    let e = listed.iter().find(|e| e.usable && backend::adapter_name_matches(t, &e.name));
                    cards::HelperChoice::Card(
                        e.map(|e| e.key.clone())
                            .ok_or_else(|| format!("no graphics card found is called like \"{t}\" (app.gpu lists them under adapters)"))?,
                    )
                }
                c => c,
            },
        };
        if cards::set_helper(choice) {
            sync_helper();
        }
        Ok(())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = setting;
        Ok(())
    }
}

/// The second card's setting in force (`""` Automatic, `off`, `same` or a card's key).
pub fn helper_setting() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        cards::helper_setting().setting()
    }
    #[cfg(target_arch = "wasm32")]
    {
        String::new()
    }
}

/// Whether batch work can go to the second card now: its device is made and not lost, and GPU
/// rendering is allowed on this thread. Never blocks.
pub fn helper_ready() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // (a lost one is replaced here once its pause is over, on a thread of its own)
        allowed() && device_for(Role::Helper).is_some()
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// What the second card is doing, for Settings and `app.gpu`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HelperStatus {
    /// `off` (turned off, or GPU rendering is), `none` (no other card to use), `notStarted` (made
    /// once it is known which card edits: the app's warm-up), `starting` (being set up, or a new
    /// choice being applied), `ready`, `lost` (stopped responding: restarted after a pause),
    /// `failed` (its device couldn't be made).
    pub state: &'static str,
    /// The card's name (ready, lost).
    pub name: Option<String>,
    /// Why it is not ready, in plain words.
    pub reason: Option<String>,
}

/// The second card's [`HelperStatus`]. Never blocks.
pub fn helper_status() -> HelperStatus {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let st = |state, name: Option<String>, reason: Option<String>| HelperStatus { state, name, reason };
        if let Some(why) = env_off_reason() {
            return st("off", None, Some(why.into()));
        }
        if !ENABLED.load(Ordering::Relaxed) {
            return st("off", None, Some("GPU rendering is turned off".into()));
        }
        let name = slot::existing(Role::Helper).map(|g| g.info.name.clone());
        // (a new choice being applied: not the old one's state, which a script polling for
        // `ready` would take for the answer)
        if helper_syncing() {
            return st("starting", name, None);
        }
        if let Some(r) = slot::lost_reason(Role::Helper) {
            return st("lost", name, Some(r));
        }
        if slot::current(Role::Helper).is_some() {
            return st("ready", name, None);
        }
        if let Some(r) = slot::absent_reason(Role::Helper) {
            let off = cards::helper_choice() == cards::HelperChoice::Off;
            return st(if off { "off" } else { "none" }, None, Some(r));
        }
        if let Some(r) = slot::creation_error(Role::Helper) {
            return st("failed", None, Some(r));
        }
        st("notStarted", None, None)
    }
    #[cfg(target_arch = "wasm32")]
    {
        HelperStatus { state: "off", name: None, reason: Some("the browser build has no GPU path yet".into()) }
    }
}

/// The second card's sync: (one is running, another was asked for meanwhile).
#[cfg(not(target_arch = "wasm32"))]
static HELPER_SYNC: Mutex<(bool, bool)> = Mutex::new((false, false));

#[cfg(not(target_arch = "wasm32"))]
fn helper_syncing() -> bool {
    HELPER_SYNC.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0
}

/// Make the second card's device match the settings now, on a thread of its own (`lc-gpu-helper`):
/// made, replaced by another card's, or released. Once at a time; asked again meanwhile, it runs
/// once more after. Nothing before it is known which card edits: the editing card's device tried
/// ([`warm_up`] calls it after), or the screen's card told ([`adopt_window_device`] calls it
/// before building the editing card's kernels, so both cards build theirs at once).
#[cfg(not(target_arch = "wasm32"))]
fn sync_helper() {
    {
        let mut s = HELPER_SYNC.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.0 {
            s.1 = true;
            return;
        }
        *s = (true, false);
    }
    let spawned = std::thread::Builder::new().name("lc-gpu-helper".into()).spawn(|| {
        // below normal priority: the editing card's set-up and the window come first
        lightcraft_sysmem::lower_thread_priority();
        loop {
            if let Some(_work) = exit::enter() {
                sync_helper_now();
            }
            let mut s = HELPER_SYNC.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if s.1 {
                s.1 = false;
            } else {
                s.0 = false;
                break;
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("gpu: can't set up the second graphics card: {e}");
        *HELPER_SYNC.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (false, false);
    }
}

/// Wait (at most `timeout`) until the second card's device matches the settings ([`set_helper`],
/// the warm-up): `true` when no sync runs any more (tests, `app.gpu {wait}`).
pub fn wait_helper(timeout: std::time::Duration) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let end = std::time::Instant::now() + timeout;
        while helper_syncing() {
            if std::time::Instant::now() >= end {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        true
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = timeout;
        true
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn sync_helper_now() {
    // (which card is the editing one must be known: its device tried, or the screen's card told)
    let editing_known = slot::tried(Role::Primary) || cards::screen().is_some();
    if env_disabled() || !ENABLED.load(Ordering::Relaxed) || !editing_known {
        return;
    }
    let Some(backends) = backend::compute_backends() else { return };
    let _one = slot::creating(Role::Helper);
    match helper_pick(backends) {
        Err(why) => {
            if slot::existing(Role::Helper).is_some() || slot::absent_reason(Role::Helper).as_deref() != Some(why.as_str()) {
                log::info!("gpu: no second graphics card: {why}");
                slot::clear(Role::Helper, false, why);
            }
        }
        Ok((adapter, key, instance)) => {
            let current = slot::existing(Role::Helper);
            if current.as_ref().is_some_and(|g| g.key == key) && slot::lost_reason(Role::Helper).is_none() {
                return;
            }
            drop(current);
            match make_helper(backends, adapter, key) {
                Ok(g) => {
                    log::info!("gpu: the second graphics card ({}) helps with batch work", g.info.name);
                    slot::put(Role::Helper, g);
                }
                Err(why) => {
                    log::warn!("gpu: the second graphics card can't render: {why}");
                    slot::clear(Role::Helper, true, why);
                }
            }
            drop(instance);
        }
    }
}

/// The second card's adapter now ([`cards::resolve_helper`]), with its key and the instance it
/// lives on; `Err`: there is none to use, and why.
#[cfg(not(target_arch = "wasm32"))]
fn helper_pick(backends: wgpu::Backends) -> Result<(wgpu::Adapter, Option<cards::AdapterKey>, wgpu::Instance), String> {
    let choice = cards::helper_choice();
    let pins = backend::wanted_adapter().is_some();
    match &choice {
        cards::HelperChoice::Off => {
            return Err(if cards::helper_variable().is_some() { "turned off by LIGHTCRAFT_GPU_HELPER" } else { "turned off in Settings" }.into());
        }
        cards::HelperChoice::Auto if pins => return Err("LIGHTCRAFT_GPU_ADAPTER keeps the work on one graphics card".into()),
        _ => {}
    }
    let (instance, all, entries) = backend::with_marker("graphics card listing", || {
        std::panic::catch_unwind(|| {
            let instance = backend::compute_instance(backends);
            let (all, entries) = backend::enumerate(&instance, backends);
            (instance, all, entries)
        })
        .map_err(|_| "listing the graphics cards panicked".to_string())
    })?;
    let primary = match slot::existing(Role::Primary) {
        Some(g) => {
            let key = g.key.clone().unwrap_or_else(|| cards::AdapterKey::of(&g.info, 1));
            entries.iter().position(|e| e.key == key).or_else(|| entries.iter().position(|e| e.key.same_model(&key)))
        }
        // the editing card's device is still being made (its kernels build beside these): the
        // card it will be on, as it resolves it
        None => cards::resolve_card(&entries, backend::wanted_adapter().as_deref(), cards::card().as_ref()).index,
    };
    let Some(i) = cards::resolve_helper(&entries, primary, &choice, pins) else {
        return Err(match &choice {
            cards::HelperChoice::Card(k) => format!("the {} isn't available", k.name),
            cards::HelperChoice::Same => "there is no graphics card for editing".into(),
            _ => "no other graphics card found".into(),
        });
    };
    let key = entries.get(i).map(|e| e.key.clone());
    let adapter = all.into_iter().nth(i).ok_or("the graphics card went away")?;
    Ok((adapter, key, instance))
}

/// A device of the second card's on `adapter`, under the crash sentinel (`role=helper`: a crash
/// there costs the next launch only the second card).
#[cfg(not(target_arch = "wasm32"))]
fn make_helper(backends: wgpu::Backends, adapter: wgpu::Adapter, key: Option<cards::AdapterKey>) -> Result<ctx::Gpu, String> {
    backend::with_init_marker_for(backends, key.as_ref(), Role::Helper.id(), || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx::Gpu::on_adapter(adapter, key.clone(), Role::Helper)))
            .unwrap_or_else(|_| Err("device creation panicked".into()))
    })
}

/// A new second-card device (replacing a lost one): `Ok(None)` when the settings no longer name
/// one.
#[cfg(not(target_arch = "wasm32"))]
fn new_helper_device() -> Result<Option<ctx::Gpu>, String> {
    let Some(backends) = backend::compute_backends() else { return Ok(None) };
    match helper_pick(backends) {
        Err(_) => Ok(None),
        Ok((adapter, key, _instance)) => make_helper(backends, adapter, key).map(Some),
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
    let generation = [Role::Primary, Role::Helper]
        .into_iter()
        .filter_map(slot::existing)
        .find(|g| g.device == *device)
        .map_or(0, |g| g.generation);
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
        let _turn =
            gate::enter(gpu.role, gate::job_bytes(src.width.saturating_mul(src.height), out_px), vram, &shutting_down).ok_or(Fallback::NotTried)?;
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
            (Ok(Some(_)), None) if !slot::is_current(gpu.generation) || slot::lost_reason(gpu.role).is_some() => {
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
