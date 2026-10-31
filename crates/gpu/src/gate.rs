//! Turns at each graphics card for batch work (exports, preview builds, background preparation),
//! so many photos rendering side by side share a card without over-committing its memory, and the
//! window's own work (the loupe, the grid, drawing frames) always comes first.
//!
//! Each card ([`Role`]: the editing card, the second card) has a gate of its own: its own budget
//! ([`crate::set_batch_limits_for`]; by default [`SHARE_PERCENT`] of the editing card's memory,
//! [`HELPER_SHARE_PERCENT`] of the second card's, which draws no window: decision 5), its own most
//! jobs at once, its own counts ([`usage`]). A thread marks its renders as batch work with
//! [`crate::as_batch`]; each batch render then holds a [`Permit`] for the device memory it needs
//! ([`job_bytes`]) on the card it renders on while it runs: it waits while the renders already
//! holding permits use the budget or the most jobs at once. One batch render may always run,
//! whatever its size (a photo bigger than the budget still uses the GPU, alone). Interactive
//! renders never wait: they are only counted.
//!
//! **The editing card yields to the interface** (§8 of the dual-GPU design, 31 Oct 2026): batch
//! work there also waits while an interactive render is on the card, while the window asked for
//! room ([`crate::hold_batch`], e.g. while it resizes), and — once the app tracks interaction
//! ([`crate::track_interaction`], the desktop app does) — until nothing was touched and no view
//! rendered for [`idle`] (~1 s). With a second card working, the editing card takes one batch job
//! at a time. A batch render already running there checks between its submissions
//! ([`throttle`], [`yield_to_interaction`]): the moment a slider, brush, pan or photo change starts
//! it waits after its current submission (a few ms) until the interface is idle again, so it never
//! holds the card for a whole render while a draft waits. The second card has no window: batch
//! work there never waits for the interface.
//!
//! Between its submissions a batch render waits for its previous one to finish ([`throttle`]), so
//! the queue never holds more than about one submission per export: the window's frames slot in
//! between instead of waiting behind seconds of export work (with 14–16 exports queued at once
//! the window drew no frame for 5 s, and resizing it failed with "Failed to wait for GPU to come
//! idle").

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::Role;

struct State {
    /// Device bytes the running batch renders hold.
    held: u64,
    /// Batch renders running.
    jobs: usize,
    /// Interactive renders running.
    interactive: usize,
    /// When the last interactive render ended.
    interactive_ended: Option<Instant>,
    /// Batch renders wait until then (the window asked for the GPU).
    hold_until: Option<Instant>,
    /// Batch renders waiting for their turn now.
    waiting: usize,
    /// Since when batch renders run without a break (`None`: none runs now).
    busy_since: Option<Instant>,
    /// Batch work done (the device bytes of the turns given back) and the time it took with
    /// batch renders running: the card's measured speed ([`rate`]), which shares batch photos
    /// out between the cards ([`order`]).
    done: u64,
    busy: Duration,
}

/// One card's gate.
struct Gate {
    state: Mutex<State>,
    cv: Condvar,
    /// Device bytes batch renders may hold together (0: [`auto_limit`]).
    limit: AtomicU64,
    /// Most batch renders at once (0: no other limit than memory, at most [`MAX_JOBS`]).
    jobs: AtomicUsize,
    /// Batch renders that waited for their turn, and for how long in all (ms): diagnostics.
    waits: AtomicU64,
    waited_ms: AtomicU64,
    /// Running batch renders that paused for the interface, and for how long in all (ms).
    pauses: AtomicU64,
    paused_ms: AtomicU64,
}

impl Gate {
    const fn new() -> Gate {
        Gate {
            state: Mutex::new(State {
                held: 0,
                jobs: 0,
                interactive: 0,
                interactive_ended: None,
                hold_until: None,
                waiting: 0,
                busy_since: None,
                done: 0,
                busy: Duration::ZERO,
            }),
            cv: Condvar::new(),
            limit: AtomicU64::new(0),
            jobs: AtomicUsize::new(0),
            waits: AtomicU64::new(0),
            waited_ms: AtomicU64::new(0),
            pauses: AtomicU64::new(0),
            paused_ms: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

static GATES: [Gate; 2] = [Gate::new(), Gate::new()];

fn gate(role: Role) -> &'static Gate {
    match role {
        Role::Primary => &GATES[0],
        Role::Helper => &GATES[1],
    }
}

/// The most batch renders on a card at once by default.
pub const MAX_JOBS: usize = 4;

/// Share of the editing card's memory batch renders may use (the window, the views and the driver
/// keep the rest).
const SHARE_PERCENT: u64 = 55;
/// Share of the second card's memory: it draws no window and renders no view (decision 5).
const HELPER_SHARE_PERCENT: u64 = 80;

// ---------------------------------------------------------------- the interface's idle clock

/// Whether batch work on the editing card waits for the interface to be idle ([`track`]).
static TRACK: AtomicBool = AtomicBool::new(false);
/// How long the interface must be untouched before batch work uses the editing card (ms).
static IDLE_MS: AtomicU64 = AtomicU64::new(1000);
/// When the interface was last touched, ms after [`epoch`] (0: never).
static LAST_INPUT_MS: AtomicU64 = AtomicU64::new(0);

fn epoch() -> Instant {
    static E: OnceLock<Instant> = OnceLock::new();
    *E.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    // (+1: 0 means "never")
    u64::try_from(epoch().elapsed().as_millis()).unwrap_or(u64::MAX).saturating_add(1)
}

/// Turn the idle rule on (the desktop app) or off (the default: headless sessions, the CLI,
/// tests keep the old behaviour).
pub(crate) fn track(on: bool) {
    TRACK.store(on, Ordering::Relaxed);
    gate(Role::Primary).cv.notify_all();
}

pub(crate) fn tracking() -> bool {
    TRACK.load(Ordering::Relaxed)
}

/// The interface was touched now (input, a slider, a pan, a photo change).
pub(crate) fn note_interaction() {
    LAST_INPUT_MS.store(now_ms(), Ordering::Relaxed);
}

/// How long the interface must be untouched before batch work uses the editing card.
pub(crate) fn idle() -> Duration {
    Duration::from_millis(IDLE_MS.load(Ordering::Relaxed))
}

/// Set [`idle`] (tests).
pub(crate) fn set_idle(d: Duration) {
    IDLE_MS.store(u64::try_from(d.as_millis()).unwrap_or(u64::MAX), Ordering::Relaxed);
}

/// Whether the interface leaves the editing card to batch work now: no view rendering, and (when
/// tracked) nothing touched and no view rendered for [`idle`].
fn interface_quiet(s: &State, now: Instant) -> bool {
    if s.interactive > 0 {
        return false;
    }
    if !tracking() {
        return true;
    }
    let idle = idle();
    let since_input = Duration::from_millis(now_ms().saturating_sub(LAST_INPUT_MS.load(Ordering::Relaxed)));
    let input_quiet = LAST_INPUT_MS.load(Ordering::Relaxed) == 0 || since_input >= idle;
    let views_quiet = s.interactive_ended.is_none_or(|t| now.saturating_duration_since(t) >= idle);
    input_quiet && views_quiet
}

/// Whether a batch render on the editing card would have to wait for the interface now.
pub(crate) fn interface_busy() -> bool {
    !interface_quiet(&gate(Role::Primary).lock(), Instant::now())
}

// ------------------------------------------------------------------------------------- turns

thread_local! {
    static BATCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// This thread holds a turn on this card for several renders ([`in_one_turn`]): they don't
    /// queue again.
    static IN_TURN: std::cell::Cell<Option<Role>> = const { std::cell::Cell::new(None) };
}

fn in_turn() -> Option<Role> {
    IN_TURN.try_with(|c| c.get()).ok().flatten()
}

/// Run `f` (several batch renders on card `role`, e.g. an export's SDR and HDR renditions of one
/// photo, which share their device stages) in one turn needing `bytes`: the GPU memory the first
/// render leaves cached for the second is never another export's to take meanwhile. Outside
/// batch work it is just `f`.
pub(crate) fn in_one_turn<R>(role: Role, bytes: u64, vram: u64, stop: &dyn Fn() -> bool, f: impl FnOnce() -> R) -> R {
    if !is_batch() || in_turn() == Some(role) {
        return f();
    }
    let permit = enter(role, bytes, vram, stop);
    let was = IN_TURN.try_with(|c| c.replace(permit.as_ref().map(|_| role))).ok().flatten();
    // (put back on unwind too; the turn is given back after the thread's mark)
    let _turn = Turn { _permit: permit, was };
    f()
}

/// A turn taken without waiting ([`try_turn`]): this thread's renders on its card run in it until
/// it is dropped, as inside [`in_one_turn`].
pub(crate) struct Turn {
    _permit: Option<Permit>,
    was: Option<Role>,
}

impl Drop for Turn {
    fn drop(&mut self) {
        let was = self.was;
        let _ = IN_TURN.try_with(|c| c.set(was));
    }
}

/// [`in_one_turn`]'s turn on card `role` if it is free now; `None` when it would have to wait
/// (the caller renders elsewhere: on the other card, or the processor). Outside batch work, and
/// inside a turn already taken on that card, always a turn.
pub(crate) fn try_turn(role: Role, bytes: u64, vram: u64) -> Option<Turn> {
    let was = in_turn();
    if !is_batch() || was == Some(role) {
        return Some(Turn { _permit: None, was });
    }
    let permit = enter(role, bytes, vram, &|| true)?;
    let _ = IN_TURN.try_with(|c| c.set(Some(role)));
    Some(Turn { _permit: Some(permit), was })
}

/// Puts this thread's batch flag back when dropped (also when the work panics: an export's photo
/// that panics, caught further up, must not leave the thread's later renders counted as batch work).
struct BatchFlag(bool);

impl Drop for BatchFlag {
    fn drop(&mut self) {
        let was = self.0;
        let _ = BATCH.try_with(|b| b.set(was));
    }
}

/// Run `f` with this thread's GPU renders counted as batch work.
pub(crate) fn as_batch<R>(f: impl FnOnce() -> R) -> R {
    let _flag = BatchFlag(BATCH.try_with(|b| b.replace(true)).unwrap_or(false));
    f()
}

/// Are this thread's renders batch work?
pub(crate) fn is_batch() -> bool {
    BATCH.try_with(|b| b.get()).unwrap_or(false)
}

/// Device memory a render of `out_px` output pixels from a source of `src_px` pixels holds at its
/// peak: the uploaded source (12 B/px), its framed copy and the stages it keeps while finishing
/// (measured: a 45 MP export peaks at ~[`OUT_BYTES_PER_PX`] B per output pixel).
pub(crate) fn job_bytes(src_px: usize, out_px: usize) -> u64 {
    (src_px as u64).saturating_mul(12).saturating_add((out_px as u64).saturating_mul(OUT_BYTES_PER_PX))
}

/// Device bytes per output pixel at a render's peak (see [`job_bytes`]).
pub(crate) const OUT_BYTES_PER_PX: u64 = 72;

/// The default budget of batch renders on card `role` with `vram` bytes.
pub(crate) fn auto_limit(role: Role, vram: u64) -> u64 {
    vram / 100
        * match role {
            Role::Primary => SHARE_PERCENT,
            Role::Helper => HELPER_SHARE_PERCENT,
        }
}

/// Set card `role`'s budget and most jobs (`None`: automatic).
pub(crate) fn set_limits(role: Role, bytes: Option<u64>, jobs: Option<usize>) {
    let g = gate(role);
    g.limit.store(bytes.unwrap_or(0), Ordering::Relaxed);
    g.jobs.store(jobs.unwrap_or(0), Ordering::Relaxed);
    g.cv.notify_all();
}

/// Card `role`'s budget and most jobs as set ([`set_limits`]; `None`: automatic).
pub(crate) fn limits_setting(role: Role) -> (Option<u64>, Option<usize>) {
    let g = gate(role);
    let (bytes, jobs) = (g.limit.load(Ordering::Relaxed), g.jobs.load(Ordering::Relaxed));
    ((bytes != 0).then_some(bytes), (jobs != 0).then_some(jobs))
}

/// The budget and the most jobs in force on card `role` with `vram` bytes. With the interface
/// tracked and a second card working, the editing card takes one batch job at a time (§8).
pub(crate) fn limits(role: Role, vram: u64) -> (u64, usize) {
    let g = gate(role);
    let bytes = match g.limit.load(Ordering::Relaxed) {
        0 => auto_limit(role, vram),
        b => b,
    };
    let jobs = match g.jobs.load(Ordering::Relaxed) {
        0 => MAX_JOBS,
        j => j,
    };
    let jobs = if role == Role::Primary && tracking() && crate::helper_ready() { jobs.min(1) } else { jobs };
    (bytes, jobs)
}

/// Batch renders on the editing card wait at least `d` from now before they start (the window
/// needs the GPU).
pub(crate) fn hold(d: Duration) {
    let until = Instant::now() + d;
    let mut s = gate(Role::Primary).lock();
    s.hold_until = Some(s.hold_until.map_or(until, |t| t.max(until)));
}

// ------------------------------------------------------------ sharing photos between cards

/// How much batch work time a card's measured speed ([`rate`]) remembers (roughly; older work
/// counts half each time this much more has passed).
const RATE_WINDOW: Duration = Duration::from_secs(120);

/// Card `s`'s measured speed for batch work: device bytes of turns given back per second with
/// batch renders running (several at once included). `None` before it finished any.
fn rate(s: &State, now: Instant) -> Option<f64> {
    let busy = s.busy.saturating_add(s.busy_since.map_or(Duration::ZERO, |t| now.saturating_duration_since(t)));
    let secs = busy.as_secs_f64();
    (s.done > 0 && secs > 0.001).then(|| s.done as f64 / secs).filter(|r| r.is_finite() && *r > 0.0)
}

/// The cards to try, in order, for a batch photo needing `bytes` (`helper`: the second card is
/// ready): the one that would finish it soonest — its photos running now plus this one, at its
/// measured speed ([`rate`]; both alike until each has finished some) — so both cards work in
/// proportion to their speed and a build of two photos at a time still uses both. On a tie, the
/// second card. While the app tracks the interface (§8) the second card takes a photo first
/// whenever it has none: the editing card only gets one when the second card is working already
/// (and its gate hands it out only while the interface is idle, one at a time).
pub(crate) fn order(bytes: u64, helper: bool) -> Vec<Role> {
    if !helper {
        return vec![Role::Primary];
    }
    let now = Instant::now();
    let (h_held, h_jobs, h_rate) = {
        let s = gate(Role::Helper).lock();
        (s.held, s.jobs, rate(&s, now))
    };
    let (p_held, p_rate) = {
        let s = gate(Role::Primary).lock();
        (s.held, rate(&s, now))
    };
    if tracking() && h_jobs == 0 {
        return vec![Role::Helper, Role::Primary];
    }
    let (h_rate, p_rate) = match (h_rate, p_rate) {
        (Some(h), Some(p)) => (h, p),
        _ => (1.0, 1.0),
    };
    let finish = |held: u64, rate: f64| held.saturating_add(bytes) as f64 / rate;
    if finish(h_held, h_rate) <= finish(p_held, p_rate) { vec![Role::Helper, Role::Primary] } else { vec![Role::Primary, Role::Helper] }
}

/// Card `role`'s measured speed for batch work (device bytes per second; `None` before it
/// finished any): diagnostics.
pub(crate) fn measured_rate(role: Role) -> Option<f64> {
    rate(&gate(role).lock(), Instant::now())
}

/// Forget the cards' measured speeds (tests).
#[cfg(test)]
fn forget_rates() {
    for r in Role::ALL {
        let mut s = gate(r).lock();
        s.done = 0;
        s.busy = Duration::ZERO;
        s.busy_since = None;
    }
}

/// Record a measured speed for card `role` (tests: fake cards).
#[cfg(test)]
fn set_rate(role: Role, done: u64, busy: Duration) {
    let mut s = gate(role).lock();
    s.done = done;
    s.busy = busy;
}

/// A render's turn at a card; given back when dropped.
pub(crate) struct Permit {
    role: Role,
    bytes: u64,
    batch: bool,
    /// Counted at the gate (not a render inside a turn already taken).
    held: bool,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if !self.held {
            return;
        }
        let g = gate(self.role);
        let mut s = g.lock();
        if self.batch {
            s.held = s.held.saturating_sub(self.bytes);
            s.jobs = s.jobs.saturating_sub(1);
            s.done = s.done.saturating_add(self.bytes);
            if s.jobs == 0
                && let Some(t) = s.busy_since.take()
            {
                s.busy = s.busy.saturating_add(t.elapsed());
            }
            // (the recent past counts most: halve the record now and then)
            if s.busy > RATE_WINDOW {
                s.busy /= 2;
                s.done /= 2;
            }
        } else {
            s.interactive = s.interactive.saturating_sub(1);
            s.interactive_ended = Some(Instant::now());
        }
        drop(s);
        g.cv.notify_all();
    }
}

/// Take a turn on card `role` for a render needing `bytes` on a card with `vram` bytes: at once
/// for interactive work, else when it fits (see the module doc). `None` when `stop()` says to
/// give up while waiting (the app is closing, or the caller only tries): the render then runs
/// elsewhere.
pub(crate) fn enter(role: Role, bytes: u64, vram: u64, stop: &dyn Fn() -> bool) -> Option<Permit> {
    // (a render inside a turn already taken on this card: nothing more to hold)
    if is_batch() && in_turn() == Some(role) {
        return Some(Permit { role, bytes: 0, batch: false, held: false });
    }
    let g = gate(role);
    let mut s = g.lock();
    if !is_batch() {
        s.interactive += 1;
        return Some(Permit { role, bytes, batch: false, held: true });
    }
    let start = Instant::now();
    let mut waited = false;
    loop {
        let (limit, jobs) = limits(role, vram);
        let now = Instant::now();
        let held_ok = s.jobs == 0 || s.held.saturating_add(bytes) <= limit;
        let jobs_ok = s.jobs < jobs.max(1);
        // the second card has no window: nothing of the interface to wait for
        let window_ok = role == Role::Helper || (interface_quiet(&s, now) && s.hold_until.is_none_or(|t| now >= t));
        if held_ok && jobs_ok && window_ok {
            if s.jobs == 0 {
                s.busy_since = Some(now);
            }
            s.held = s.held.saturating_add(bytes);
            s.jobs += 1;
            if waited {
                s.waiting = s.waiting.saturating_sub(1);
                g.waits.fetch_add(1, Ordering::Relaxed);
                g.waited_ms.fetch_add(start.elapsed().as_millis() as u64, Ordering::Relaxed);
            }
            return Some(Permit { role, bytes, batch: true, held: true });
        }
        if !waited {
            waited = true;
            s.waiting += 1;
        }
        if stop() {
            s.waiting = s.waiting.saturating_sub(1);
            return None;
        }
        // (woken by a permit given back; the timeout re-checks a hold or an idle clock that ran
        // out, and `stop`)
        s = g.cv.wait_timeout(s, Duration::from_millis(50)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
    }
}

/// Card `role`'s gate now: (batch renders running, device bytes they hold, renders waiting, batch
/// renders that waited so far and their total wait in ms, running batch renders that paused for
/// the interface and their total pause in ms).
pub(crate) fn usage(role: Role) -> (usize, u64, usize, u64, u64, u64, u64) {
    let g = gate(role);
    let s = g.lock();
    (
        s.jobs,
        s.held,
        s.waiting,
        g.waits.load(Ordering::Relaxed),
        g.waited_ms.load(Ordering::Relaxed),
        g.pauses.load(Ordering::Relaxed),
        g.paused_ms.load(Ordering::Relaxed),
    )
}

/// Between a batch render's submissions on card `role`: wait for its previous one (`prev`) to
/// finish first, so the queue stays short for the window, and on the editing card wait while the
/// interface is busy ([`yield_to_interaction`]). `false`: the wait failed (timeout, a lost device,
/// the app closing): the caller fails the render. The wait goes through [`crate::ctx::Gpu::poll`],
/// which never panics and records a lost device.
pub(crate) fn throttle(gpu: &crate::ctx::Gpu, prev: Option<wgpu::SubmissionIndex>, role: Role) -> bool {
    if !is_batch() {
        return true;
    }
    if let Some(prev) = prev
        && gpu.poll(wgpu::PollType::Wait { submission_index: Some(prev), timeout: Some(THROTTLE_TIMEOUT) }).is_err()
    {
        return false;
    }
    role != Role::Primary || yield_to_interaction(&crate::shutting_down)
}

/// A batch render on the editing card between two submissions: while the interface is busy (a
/// view renders, or it was touched within [`idle`] — only when tracked), wait. `false` when
/// `stop()` says to give up (the app is closing). Interactive renders never come here.
pub(crate) fn yield_to_interaction(stop: &dyn Fn() -> bool) -> bool {
    if !is_batch() || !tracking() {
        return true;
    }
    let g = gate(Role::Primary);
    let mut s = g.lock();
    let start = Instant::now();
    let mut paused = false;
    while !interface_quiet(&s, Instant::now()) {
        if stop() {
            return false;
        }
        paused = true;
        s = g.cv.wait_timeout(s, Duration::from_millis(25)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
    }
    if paused {
        g.pauses.fetch_add(1, Ordering::Relaxed);
        g.paused_ms.fetch_add(start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
    true
}

/// Longest a batch render waits for its previous submission before it gives the GPU up.
const THROTTLE_TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate's statics are shared: tests that use them take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    const P: Role = Role::Primary;
    const H: Role = Role::Helper;

    #[test]
    fn job_memory_grows_with_the_pixels_and_never_overflows() {
        assert_eq!(job_bytes(0, 0), 0);
        assert!(job_bytes(45_000_000, 45_000_000) > 3 << 30);
        assert!(job_bytes(45_000_000, 2_000_000) < job_bytes(45_000_000, 45_000_000));
        assert_eq!(job_bytes(usize::MAX, usize::MAX), u64::MAX);
        assert_eq!(auto_limit(P, 12 << 30), (12u64 << 30) / 100 * SHARE_PERCENT);
        // the second card draws no window: a bigger share (decision 5)
        assert_eq!(auto_limit(H, 12 << 30), (12u64 << 30) / 100 * HELPER_SHARE_PERCENT);
        assert!(auto_limit(H, 12 << 30) > auto_limit(P, 12 << 30));
    }

    /// Review of PR #3506: Test my machine changes the second card's job limit for one round and
    /// puts back what was set, not Automatic.
    #[test]
    fn a_cards_limits_as_set_can_be_put_back() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_limits(H, None, Some(2));
        let (bytes, jobs) = limits_setting(H);
        assert_eq!((bytes, jobs), (None, Some(2)));
        set_limits(H, None, Some(4));
        set_limits(H, bytes, jobs);
        assert_eq!(limits_setting(H), (None, Some(2)), "the earlier setting, not Automatic");
        set_limits(H, Some(1 << 30), None);
        assert_eq!(limits_setting(H), (Some(1 << 30), None));
        set_limits(H, None, None);
        assert_eq!(limits_setting(H), (None, None));
    }

    #[test]
    fn batch_renders_share_the_budget_and_one_always_runs() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_limits(P, Some(10 << 30), Some(3));
        let never = || false;
        as_batch(|| {
            // bigger than the budget: alone, it still runs
            let big = enter(P, 20 << 30, 0, &never).unwrap();
            assert_eq!(usage(P).0, 1);
            drop(big);
            let a = enter(P, 4 << 30, 0, &never).unwrap();
            let b = enter(P, 4 << 30, 0, &never).unwrap();
            assert_eq!(usage(P).1, 8 << 30);
            // a third one doesn't fit: it waits until one is given back
            let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = started.clone();
            let t = std::thread::spawn(move || {
                as_batch(|| {
                    let p = enter(P, 4 << 30, 0, &|| false);
                    flag.store(true, Ordering::SeqCst);
                    drop(p);
                })
            });
            std::thread::sleep(Duration::from_millis(250));
            assert!(!started.load(Ordering::SeqCst), "over the budget: waits");
            drop(a);
            t.join().unwrap();
            assert!(started.load(Ordering::SeqCst));
            drop(b);
        });
        assert_eq!(usage(P).0, 0);
        set_limits(P, None, None);
    }

    /// Each card has its own budget and counts: the second card being full never holds a photo
    /// back from the editing card, nor the other way round.
    #[test]
    fn each_card_has_a_budget_of_its_own() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_limits(P, Some(4 << 30), Some(1));
        set_limits(H, Some(4 << 30), Some(1));
        as_batch(|| {
            let on_editing = enter(P, 3 << 30, 0, &|| false).unwrap();
            // the editing card is full; the second card is free
            assert!(enter(P, 1 << 30, 0, &|| true).is_none(), "the editing card's one job is taken");
            let on_helper = enter(H, 3 << 30, 0, &|| true).expect("the second card has its own budget");
            assert_eq!((usage(P).0, usage(P).1), (1, 3 << 30));
            assert_eq!((usage(H).0, usage(H).1), (1, 3 << 30));
            assert!(enter(H, 1 << 30, 0, &|| true).is_none(), "and its own most jobs");
            drop(on_editing);
            assert!(enter(P, 1 << 30, 0, &|| true).is_some(), "the editing card frees on its own");
            drop(on_helper);
        });
        assert_eq!((usage(P).0, usage(H).0), (0, 0));
        // turns are per card: a turn on one doesn't cover renders on the other
        as_batch(|| {
            let t = try_turn(H, 1 << 20, 0).expect("free");
            assert_eq!(usage(H).0, 1);
            assert!(enter(H, 1 << 20, 0, &|| true).is_some_and(|p| !p.held), "inside the turn: no new permit");
            let p = enter(P, 1 << 20, 0, &|| true).expect("the editing card is free");
            assert!(p.held, "the other card's renders still take their own turn");
            drop(p);
            drop(t);
        });
        assert_eq!((usage(P).0, usage(H).0), (0, 0));
        set_limits(P, None, None);
        set_limits(H, None, None);
    }

    #[test]
    fn the_window_comes_first() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_limits(P, Some(10 << 30), None);
        // an interactive render never waits, and holds batch work back while it runs
        let view = enter(P, 9 << 30, 0, &|| false).unwrap();
        let gave_up = as_batch(|| {
            let t = Instant::now();
            enter(P, 1 << 20, 0, &|| t.elapsed() > Duration::from_millis(300)).is_none()
        });
        assert!(gave_up, "a batch render waits while the view renders");
        // not on the second card: it draws no window
        assert!(as_batch(|| enter(H, 1 << 20, 0, &|| true)).is_some(), "the second card doesn't wait for views");
        drop(view);
        assert!(as_batch(|| enter(P, 1 << 20, 0, &|| false)).is_some());
        // a hold asked for by the window
        hold(Duration::from_millis(300));
        let t = Instant::now();
        let p = as_batch(|| enter(P, 1 << 20, 0, &|| false));
        assert!(p.is_some() && t.elapsed() >= Duration::from_millis(250), "waited for the hold: {:?}", t.elapsed());
        drop(p);
        set_limits(P, None, None);
    }

    /// §8: with the interface tracked, batch work takes the editing card only after ~1 s without
    /// input or view renders; a batch render already running pauses between submissions the
    /// moment interaction starts, and goes on once it is idle again. The second card never waits.
    #[test]
    fn the_editing_card_yields_to_interaction() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_idle(Duration::from_millis(300));
        track(true);
        note_interaction();
        assert!(interface_busy());
        as_batch(|| {
            // just touched: no turn on the editing card, one at once on the second card
            assert!(try_turn(P, 1 << 20, 0).is_none(), "touched within the idle time: the editing card is the interface's");
            assert!(try_turn(H, 1 << 20, 0).is_some());
            // waiting for it: granted once idle
            let t = Instant::now();
            let p = enter(P, 1 << 20, 0, &|| false);
            assert!(p.is_some() && t.elapsed() >= Duration::from_millis(200), "waited for the idle time: {:?}", t.elapsed());
            drop(p);
            // a view render ending restarts the idle clock
            drop(as_batch_off(|| enter(P, 1 << 20, 0, &|| false)));
            assert!(interface_busy(), "a view just rendered");
            // a running batch render pauses between submissions while the interface is busy
            note_interaction();
            let t = Instant::now();
            assert!(yield_to_interaction(&|| false));
            assert!(t.elapsed() >= Duration::from_millis(200), "paused until idle: {:?}", t.elapsed());
            assert!(usage(P).5 >= 1, "the pause is counted");
            // idle: no pause
            let t = Instant::now();
            assert!(yield_to_interaction(&|| false));
            assert!(t.elapsed() < Duration::from_millis(100));
            // the app closing ends a pause
            note_interaction();
            assert!(!yield_to_interaction(&|| true));
        });
        // views and the untracked default never pause
        assert!(yield_to_interaction(&|| false), "not batch work");
        track(false);
        note_interaction();
        assert!(!interface_busy(), "untracked: only running views count");
        assert!(as_batch(|| try_turn(P, 1 << 20, 0)).is_some());
        set_idle(Duration::from_millis(1000));
    }

    /// Run `f` as interactive work even inside a batch scope (test helper).
    fn as_batch_off<R>(f: impl FnOnce() -> R) -> R {
        let was = BATCH.with(|b| b.replace(false));
        let r = f();
        BATCH.with(|b| b.set(was));
        r
    }

    #[test]
    fn a_turn_is_taken_only_when_free() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_limits(P, Some(10 << 30), Some(1));
        as_batch(|| {
            let t = try_turn(P, 1 << 20, 0).expect("free");
            assert_eq!(usage(P).0, 1);
            // the renders inside it don't queue again
            assert!(enter(P, 1 << 20, 0, &|| true).is_some());
            assert_eq!(usage(P).0, 1);
            // another thread's photo finds it taken: no waiting
            let other = std::thread::spawn(|| as_batch(|| try_turn(P, 1 << 20, 0).is_none())).join().unwrap();
            assert!(other, "taken: the other photo renders on the CPU");
            drop(t);
            assert_eq!(usage(P).0, 0);
            assert_eq!(in_turn(), None);
        });
        set_limits(P, None, None);
    }

    /// Take a permit for a batch photo on the card [`order`] picks first that has room (as
    /// `batch_turn_any` does with the real cards).
    fn take(bytes: u64) -> Option<(Role, Permit)> {
        order(bytes, true).into_iter().find_map(|r| enter(r, bytes, 0, &|| true).map(|p| (r, p)))
    }

    /// Two fake cards: a build of two photos at a time uses both while the interface is idle,
    /// only the second card while it is in use, and photos go to the cards in proportion to
    /// their speed (the measured 69 s with both cards vs 52 s on the 5070 alone: every photo went
    /// to the second card while it had room).
    #[test]
    fn photos_go_to_both_cards_in_proportion_to_their_speed() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_limits(P, Some(1 << 40), Some(4));
        set_limits(H, Some(1 << 40), Some(4));
        forget_rates();
        let mb = 1u64 << 20;
        as_batch(|| {
            // nothing measured yet, untracked (the CLI): the first photo to the second card, the
            // second to the editing card
            let a = take(mb).unwrap();
            let b = take(mb).unwrap();
            assert_eq!((a.0, b.0), (H, P), "two lanes: one photo on each card");
            drop((a, b));

            // the app, interface idle: the second card first, then the editing card
            forget_rates();
            set_idle(Duration::from_millis(200));
            track(true);
            note_interaction();
            std::thread::sleep(Duration::from_millis(250));
            assert!(!interface_busy());
            let a = take(mb).unwrap();
            let b = take(mb).unwrap();
            assert_eq!((a.0, b.0), (H, P), "idle: both cards work");
            drop(b);
            // in use: the editing card takes nothing, the second card everything
            note_interaction();
            let c = take(mb).unwrap();
            let d = take(mb).unwrap();
            assert_eq!((c.0, d.0), (H, H), "the interface is in use: only the second card");
            drop((a, c, d));
            track(false);
            set_idle(Duration::from_millis(1000));

            // measured: the editing card twice as fast -> two photos there for each on the second
            forget_rates();
            set_rate(P, 2000 * mb, Duration::from_secs(1));
            set_rate(H, 1000 * mb, Duration::from_secs(1));
            let held: Vec<_> = (0..6).map(|_| take(mb).unwrap()).collect();
            let on_p = held.iter().filter(|(r, _)| *r == P).count();
            assert_eq!(on_p, 4, "4 of 6 on the faster card: {:?}", held.iter().map(|h| h.0).collect::<Vec<_>>());
            assert_eq!(held.first().map(|h| h.0), Some(P), "alone, a photo goes to the faster card");
            drop(held);
            // the second card full: the editing card takes what it can
            set_limits(H, Some(1 << 40), Some(1));
            let held: Vec<_> = (0..4).map(|_| take(mb).unwrap()).collect();
            assert_eq!(held.iter().filter(|(r, _)| *r == H).count(), 1, "its one job, the rest on the editing card");
            drop(held);
        });
        // without a second card: the editing card only
        assert_eq!(order(1, false), vec![P]);
        forget_rates();
        set_limits(P, None, None);
        set_limits(H, None, None);
    }

    /// Finished batch work measures the card's speed; it stays finite and positive.
    #[test]
    fn a_cards_speed_is_measured_from_its_batch_work() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget_rates();
        assert!(measured_rate(H).is_none(), "nothing done yet");
        as_batch(|| {
            let p = enter(H, 64 << 20, 0, &|| false).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            drop(p);
        });
        let r = measured_rate(H).expect("measured");
        assert!(r.is_finite() && r > 0.0 && r < (64u64 << 20) as f64 / 0.019, "{r}");
        forget_rates();
    }

    /// A batch photo that panics (caught further up, as an export's are) leaves the thread as it
    /// was: not batch work any more, no turn held. Before, `as_batch` and `in_one_turn` left their
    /// thread marks set, so the thread's later renders (the loupe's, on a worker reused) were
    /// counted as batch work and skipped the gate.
    #[test]
    fn a_panicking_batch_photo_leaves_the_thread_as_it_was() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let r = std::panic::catch_unwind(|| as_batch(|| panic!("a photo's render panicked")));
        assert!(r.is_err());
        assert!(!is_batch(), "the thread is interactive again");
        as_batch(|| {
            let r = std::panic::catch_unwind(|| in_one_turn(P, 1 << 20, 0, &|| false, || panic!("the HDR rendition panicked")));
            assert!(r.is_err());
            assert_eq!(in_turn(), None, "no turn left marked on the thread");
            assert!(is_batch(), "the outer batch scope is still on");
        });
        assert_eq!(usage(P).0, 0, "the turn was given back");
        assert!(!is_batch());
    }

    #[test]
    fn a_closing_app_stops_the_wait() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_limits(P, Some(1 << 20), Some(1));
        as_batch(|| {
            let held = enter(P, 1 << 20, 0, &|| false).unwrap();
            assert!(enter(P, 1 << 20, 0, &|| true).is_none());
            drop(held);
        });
        set_limits(P, None, None);
    }
}
