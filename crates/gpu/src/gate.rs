//! Turns at the GPU for batch work (exports), so many photos rendering side by side share the
//! graphics card without over-committing its memory, and the window's own work (the loupe, the
//! grid, drawing frames) always comes first.
//!
//! A thread marks its renders as batch work with [`crate::as_batch`]. Each batch render then holds
//! a [`Permit`] for the device memory it needs ([`job_bytes`]) while it runs: it waits while the
//! renders already holding permits use the budget ([`crate::set_batch_limits`]; by default a share
//! of the card's memory, [`auto_limit`]) or the most jobs at once, while an interactive render is
//! on the GPU, and while the window asked for room ([`crate::hold_batch`], e.g. while it resizes).
//! One batch render may always run, whatever its size (a photo bigger than the budget still uses
//! the GPU, alone). Interactive renders never wait: they are only counted.
//!
//! Between its submissions a batch render waits for its previous one to finish ([`throttle`]), so
//! the queue never holds more than about one submission per export: the window's frames slot in
//! between instead of waiting behind seconds of export work (with 14–16 exports queued at once
//! the window drew no frame for 5 s, and resizing it failed with "Failed to wait for GPU to come
//! idle").

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    /// Device bytes the running batch renders hold.
    held: u64,
    /// Batch renders running.
    jobs: usize,
    /// Interactive renders running.
    interactive: usize,
    /// Batch renders wait until then (the window asked for the GPU).
    hold_until: Option<Instant>,
    /// Batch renders waiting for their turn now.
    waiting: usize,
}

static STATE: Mutex<State> = Mutex::new(State { held: 0, jobs: 0, interactive: 0, hold_until: None, waiting: 0 });
static CV: Condvar = Condvar::new();
/// Device bytes batch renders may hold together (0: [`auto_limit`]).
static LIMIT: AtomicU64 = AtomicU64::new(0);
/// Most batch renders at once (0: no other limit than memory, at most [`MAX_JOBS`]).
static JOBS: AtomicUsize = AtomicUsize::new(0);
/// Batch renders that waited for their turn, and for how long in all (ms): diagnostics.
static WAITS: AtomicU64 = AtomicU64::new(0);
static WAITED_MS: AtomicU64 = AtomicU64::new(0);

/// The most batch renders on the GPU at once by default.
pub const MAX_JOBS: usize = 4;

/// Share of the card's memory batch renders may use (the window, the views and the driver keep the rest).
const SHARE_PERCENT: u64 = 55;

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

thread_local! {
    static BATCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// This thread holds a turn for several renders ([`in_one_turn`]): they don't queue again.
    static IN_TURN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` (several batch renders, e.g. an export's SDR and HDR renditions of one photo, which
/// share their device stages) in one turn needing `bytes`: the GPU memory the first render
/// leaves cached for the second is never another export's to take meanwhile. Outside batch work
/// it is just `f`.
pub(crate) fn in_one_turn<R>(bytes: u64, vram: u64, stop: &dyn Fn() -> bool, f: impl FnOnce() -> R) -> R {
    if !is_batch() || IN_TURN.try_with(|c| c.get()).unwrap_or(false) {
        return f();
    }
    let turn = enter(bytes, vram, stop);
    let _restore = Restore(&IN_TURN, IN_TURN.with(|c| c.replace(turn.is_some())));
    let r = f();
    drop(_restore);
    drop(turn);
    r
}

/// Run `f` with this thread's GPU renders counted as batch work.
pub(crate) fn as_batch<R>(f: impl FnOnce() -> R) -> R {
    let _restore = Restore(&BATCH, BATCH.with(|b| b.replace(true)));
    f()
}

/// Puts a thread's flag back as it was when dropped, also when `f` panics (a panic caught by the
/// export's guard must not leave the thread in batch work or in a turn: review of PR #3532).
struct Restore(&'static std::thread::LocalKey<std::cell::Cell<bool>>, bool);

impl Drop for Restore {
    fn drop(&mut self) {
        let was = self.1;
        let _ = self.0.try_with(|c| c.set(was));
    }
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

/// The default budget of batch renders on a card with `vram` bytes: [`SHARE_PERCENT`] of it.
pub(crate) fn auto_limit(vram: u64) -> u64 {
    vram / 100 * SHARE_PERCENT
}

/// Set the budget and the most jobs (`None`: automatic).
pub(crate) fn set_limits(bytes: Option<u64>, jobs: Option<usize>) {
    LIMIT.store(bytes.unwrap_or(0), Ordering::Relaxed);
    JOBS.store(jobs.unwrap_or(0), Ordering::Relaxed);
    CV.notify_all();
}

/// The budget and the most jobs in force for a card with `vram` bytes.
pub(crate) fn limits(vram: u64) -> (u64, usize) {
    let bytes = match LIMIT.load(Ordering::Relaxed) {
        0 => auto_limit(vram),
        b => b,
    };
    let jobs = match JOBS.load(Ordering::Relaxed) {
        0 => MAX_JOBS,
        j => j,
    };
    (bytes, jobs)
}

/// Batch renders wait at least `d` from now before they start (the window needs the GPU).
pub(crate) fn hold(d: Duration) {
    let until = Instant::now() + d;
    let mut s = state();
    s.hold_until = Some(s.hold_until.map_or(until, |t| t.max(until)));
}

/// A render's turn at the GPU; given back when dropped.
pub(crate) struct Permit {
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
        let mut s = state();
        if self.batch {
            s.held = s.held.saturating_sub(self.bytes);
            s.jobs = s.jobs.saturating_sub(1);
        } else {
            s.interactive = s.interactive.saturating_sub(1);
        }
        drop(s);
        CV.notify_all();
    }
}

/// Take a turn for a render needing `bytes` on a card with `vram` bytes: at once for interactive
/// work, else when it fits (see the module doc). `None` when `stop()` says to give up while
/// waiting (the app is closing): the render then runs on the CPU.
pub(crate) fn enter(bytes: u64, vram: u64, stop: &dyn Fn() -> bool) -> Option<Permit> {
    // (a render inside a turn already taken: nothing more to hold)
    if is_batch() && IN_TURN.try_with(|c| c.get()).unwrap_or(false) {
        return Some(Permit { bytes: 0, batch: false, held: false });
    }
    let mut s = state();
    if !is_batch() {
        s.interactive += 1;
        return Some(Permit { bytes, batch: false, held: true });
    }
    let start = Instant::now();
    let mut waited = false;
    loop {
        let (limit, jobs) = limits(vram);
        let now = Instant::now();
        let held_ok = s.jobs == 0 || s.held.saturating_add(bytes) <= limit;
        let jobs_ok = s.jobs < jobs.max(1);
        let window_ok = s.interactive == 0 && s.hold_until.is_none_or(|t| now >= t);
        if held_ok && jobs_ok && window_ok {
            s.held = s.held.saturating_add(bytes);
            s.jobs += 1;
            if waited {
                s.waiting = s.waiting.saturating_sub(1);
                WAITS.fetch_add(1, Ordering::Relaxed);
                WAITED_MS.fetch_add(start.elapsed().as_millis() as u64, Ordering::Relaxed);
            }
            return Some(Permit { bytes, batch: true, held: true });
        }
        if !waited {
            waited = true;
            s.waiting += 1;
        }
        if stop() {
            s.waiting = s.waiting.saturating_sub(1);
            return None;
        }
        // (woken by a permit given back; the timeout re-checks a hold that ran out and `stop`)
        s = CV.wait_timeout(s, Duration::from_millis(100)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
    }
}

/// The gate's state now: (batch renders running, device bytes they hold, renders waiting, batch
/// renders that waited so far and their total wait in ms).
pub(crate) fn usage() -> (usize, u64, usize, u64, u64) {
    let s = state();
    (s.jobs, s.held, s.waiting, WAITS.load(Ordering::Relaxed), WAITED_MS.load(Ordering::Relaxed))
}

/// Between a batch render's submissions: wait for its previous one (`prev`) to finish first, so
/// the queue stays short for the window (see the module doc). `false`: the wait failed (timeout,
/// a lost device): the caller fails the render.
/// The wait goes through [`crate::ctx::Gpu::poll`], which never panics and records a lost device.
pub(crate) fn throttle(gpu: &crate::ctx::Gpu, prev: Option<wgpu::SubmissionIndex>) -> bool {
    let Some(prev) = prev.filter(|_| is_batch()) else { return true };
    gpu.poll(wgpu::PollType::Wait { submission_index: Some(prev), timeout: Some(THROTTLE_TIMEOUT) }).is_ok()
}

/// Longest a batch render waits for its previous submission before it gives the GPU up.
const THROTTLE_TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate's statics are shared: tests that use them take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Review of PR #3532: a panic inside batch work or a turn leaves the thread's flags as they
    /// were (the export guard catches the panic and the lane goes on with its next photo).
    #[test]
    fn a_panic_in_batch_work_leaves_the_thread_as_it_was() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(!is_batch());
        let caught = std::panic::catch_unwind(|| as_batch(|| panic!("render")));
        assert!(caught.is_err());
        assert!(!is_batch(), "still batch work after the panic");
        let caught = std::panic::catch_unwind(|| as_batch(|| in_one_turn(0, 1 << 30, &|| false, || panic!("render"))));
        assert!(caught.is_err());
        assert!(!is_batch() && !IN_TURN.with(|c| c.get()), "still in a turn after the panic");
        // nested: the outer value comes back
        as_batch(|| {
            let _ = std::panic::catch_unwind(|| as_batch(|| panic!("inner")));
            assert!(is_batch());
        });
    }

    #[test]
    fn job_memory_grows_with_the_pixels_and_never_overflows() {
        assert_eq!(job_bytes(0, 0), 0);
        assert!(job_bytes(45_000_000, 45_000_000) > 3 << 30);
        assert!(job_bytes(45_000_000, 2_000_000) < job_bytes(45_000_000, 45_000_000));
        assert_eq!(job_bytes(usize::MAX, usize::MAX), u64::MAX);
        assert_eq!(auto_limit(12 << 30), (12u64 << 30) / 100 * SHARE_PERCENT);
    }

    #[test]
    fn batch_renders_share_the_budget_and_one_always_runs() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_limits(Some(10 << 30), Some(3));
        let never = || false;
        as_batch(|| {
            // bigger than the budget: alone, it still runs
            let big = enter(20 << 30, 0, &never).unwrap();
            assert_eq!(usage().0, 1);
            drop(big);
            let a = enter(4 << 30, 0, &never).unwrap();
            let b = enter(4 << 30, 0, &never).unwrap();
            assert_eq!(usage().1, 8 << 30);
            // a third one doesn't fit: it waits until one is given back
            let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = started.clone();
            let t = std::thread::spawn(move || {
                as_batch(|| {
                    let p = enter(4 << 30, 0, &|| false);
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
        assert_eq!(usage().0, 0);
        set_limits(None, None);
    }

    #[test]
    fn the_window_comes_first() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_limits(Some(10 << 30), None);
        // an interactive render never waits, and holds batch work back while it runs
        let view = enter(9 << 30, 0, &|| false).unwrap();
        let gave_up = as_batch(|| {
            let t = Instant::now();
            enter(1 << 20, 0, &|| t.elapsed() > Duration::from_millis(300)).is_none()
        });
        assert!(gave_up, "a batch render waits while the view renders");
        drop(view);
        assert!(as_batch(|| enter(1 << 20, 0, &|| false)).is_some());
        // a hold asked for by the window
        hold(Duration::from_millis(300));
        let t = Instant::now();
        let p = as_batch(|| enter(1 << 20, 0, &|| false));
        assert!(p.is_some() && t.elapsed() >= Duration::from_millis(250), "waited for the hold: {:?}", t.elapsed());
        drop(p);
        set_limits(None, None);
    }

    #[test]
    fn a_closing_app_stops_the_wait() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_limits(Some(1 << 20), Some(1));
        as_batch(|| {
            let held = enter(1 << 20, 0, &|| false).unwrap();
            assert!(enter(1 << 20, 0, &|| true).is_none());
            drop(held);
        });
        set_limits(None, None);
    }
}
