//! A time limit on creating a GPU device (issue #3068).
//!
//! Creating the wgpu instance, adapter and device, and compiling the kernels after it, are calls
//! into the graphics driver, and on Linux the driver may talk to the display server first. With an
//! unresponsive one (a crash-looping WSLg compositor, a socket that accepts and never answers)
//! those calls never return, and wgpu's native calls are synchronous (`pollster::block_on` gets an
//! already-finished future), so there is nothing to time out from the calling thread.
//!
//! So every device (rendering, denoise, faces) is made by [`create`] on a thread of its own, and
//! the caller waits until [`limit`] (`<PREFIX>_GPU_INIT_TIMEOUT`) passes with no [`Progress`]: each
//! kernel built counts, so a first start that compiles every kernel slowly (an empty driver shader
//! cache after an install or a driver update: 30 s on an RTX 4070 SUPER and an RTX 5070 under DX12)
//! still ends on the GPU, while a driver call that never returns is given up on after the limit as
//! before (and any creation after [`limit`] times [`PROGRESS_CAP`] in all). Past it the caller gets an
//! `Err` that it caches like any other creation failure — the GPU is unavailable for the process —
//! and the blocked thread is left behind. That thread keeps what has to last as long as the driver
//! work does: its share of the GPU work in flight (`exit.rs`: a quit then reports the GPU busy
//! instead of tearing the driver down under it) and the crash sentinel (`backend.rs`: a quit
//! leaves the marker, so the next launch starts on the CPU). While one is left behind, later creations (another kind
//! of device) fail at once with the same reason instead of leaving another thread behind; should
//! the driver come back, the late device is leaked (never dropped into a driver that just hung)
//! and creations are tried again.
//!
//! The error scopes popped after creation (`scope.pop()`) need no limit: on native wgpu they
//! resolve at once, and they are thread-local, so they could not move to another thread anyway.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use std::time::{Duration, Instant};

use crate::backend::InitMarker;
use crate::exit::Gate;

/// The environment variable that changes [`DEFAULT_LIMIT`] (whole seconds).
pub(crate) fn limit_env() -> String {
    lcc_brand::env_var("GPU_INIT_TIMEOUT")
}
/// How long device creation may go without [`Progress`] by default. Normally well under a second
/// in all, but a first run compiles every kernel with the driver's compiler (DX12's FXC): generous.
pub(crate) const DEFAULT_LIMIT: Duration = Duration::from_secs(30);
/// How many times [`limit`] a creation that keeps making [`Progress`] may take in all (5 minutes by
/// default): a first start that compiles every kernel took 30 s on two current graphics cards.
const PROGRESS_CAP: u32 = 10;
/// The longest limit [`limit_env`] can ask for (an hour).
const MAX_LIMIT_SECS: u64 = 3600;

/// How long a device creation may take: [`limit_env`], else [`DEFAULT_LIMIT`]. Read once.
pub(crate) fn limit() -> Duration {
    static LIMIT: OnceLock<Duration> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        let v = lcc_brand::env("GPU_INIT_TIMEOUT");
        parse_limit(v.as_deref()).unwrap_or_else(|| {
            log::warn!(
                "gpu: {}={:?} is not a whole number of seconds from 1 to {MAX_LIMIT_SECS}; using {} s",
                limit_env(),
                v.unwrap_or_default(),
                DEFAULT_LIMIT.as_secs()
            );
            DEFAULT_LIMIT
        })
    })
}

/// The limit `v` (whole seconds, at most [`MAX_LIMIT_SECS`]) asks for; unset or blank is the
/// default; `None` when it is not a number of seconds ≥ 1.
fn parse_limit(v: Option<&str>) -> Option<Duration> {
    let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) else { return Some(DEFAULT_LIMIT) };
    match v.parse::<u64>() {
        Ok(secs) if secs >= 1 => Some(Duration::from_secs(secs.min(MAX_LIMIT_SECS))),
        // a number too big for u64 is still "wait as long as allowed"
        Err(e) if *e.kind() == std::num::IntErrorKind::PosOverflow => Some(Duration::from_secs(MAX_LIMIT_SECS)),
        _ => None,
    }
}

/// `d` for a message: "30 s", or "250 ms" under a second.
fn show(d: Duration) -> String {
    if d >= Duration::from_secs(1) { format!("{} s", d.as_secs()) } else { format!("{} ms", d.as_millis()) }
}

/// Device creations that are still inside the driver past their limit, and why the first was
/// given up on.
pub(crate) struct Stuck(Mutex<StuckState>);

struct StuckState {
    threads: usize,
    reason: Option<String>,
}

impl Stuck {
    pub(crate) const fn new() -> Stuck {
        Stuck(Mutex::new(StuckState { threads: 0, reason: None }))
    }

    fn lock(&self) -> MutexGuard<'_, StuckState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Why device creation is not tried now (one is still stuck), or `None`.
    fn reason(&self) -> Option<String> {
        let s = self.lock();
        if s.threads > 0 { s.reason.clone() } else { None }
    }
}

/// The process's stuck device creations (every kind of device: they share the driver).
static STUCK: Stuck = Stuck::new();

/// An attempt's state, changed only under the [`Stuck`] lock.
const WAITING: u8 = 0;
/// The worker finished in time; its result is (being) sent.
const DONE: u8 = 1;
/// The caller gave up waiting; the worker leaks its result.
const ABANDONED: u8 = 2;

thread_local! {
    /// The progress counter of the device creation running on this thread ([`create`]'s thread).
    static PROGRESS: RefCell<Option<Arc<AtomicU64>>> = const { RefCell::new(None) };
}

/// What the device creation on the current thread reports to [`create`]'s caller as it goes: each
/// [`Self::tick`] (a kernel built) restarts the wait. Taken on the creation thread and handed to the
/// threads it starts; does nothing outside a creation.
#[derive(Clone, Default)]
pub(crate) struct Progress(Option<Arc<AtomicU64>>);

impl Progress {
    /// The current thread's creation's progress.
    pub(crate) fn current() -> Progress {
        Progress(PROGRESS.with(|p| p.borrow().clone()))
    }

    /// One more step done.
    pub(crate) fn tick(&self) {
        if let Some(p) = &self.0 {
            p.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Why no device is created once the process is ending ([`crate::begin_shutdown`]).
const CLOSING: &str = "the app is closing";

/// What a creation thread carries besides the work: the stuck-state it reports to, the shutdown
/// gate it counts as GPU work on, and the crash sentinel it runs under.
struct Scope {
    stuck: &'static Stuck,
    gate: &'static Gate,
    marker: InitMarker,
}

/// Create a device on `backends` with `make` on a thread of its own and wait for it until [`limit`]
/// passes with no [`Progress`], at most [`PROGRESS_CAP`] times the limit in all. A panic in it, or a
/// deadline that passes, is an `Err` saying so.
///
/// The thread, not the caller, holds what must last as long as the driver work does: it counts as
/// GPU work in flight ([`crate::wait_idle`] is not idle until it ends, issue #620) and runs under
/// the crash sentinel ([`crate::backend::set_init_marker`]). So a creation left behind past its
/// limit keeps both: a quit meanwhile reports the GPU busy, and leaves the marker for the next
/// launch, which then starts on the CPU. A creation that finishes in time leaves the gate before
/// its result is handed over: once `create` returns, its thread is no longer in flight.
pub(crate) fn create<T: Send + 'static>(backends: wgpu::Backends, make: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let scope = Scope { stuck: &STUCK, gate: crate::exit::gate(), marker: InitMarker::current() };
    create_on(scope, limit(), backends, make)
}

/// [`create`] with an explicit limit and scope (the tests use their own, so that a test's stuck
/// creation cannot stop the real devices, or hold up the shutdown, of other tests in the process).
fn create_on<T: Send + 'static>(
    scope: Scope,
    limit: Duration,
    backends: wgpu::Backends,
    make: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    const WHAT: &str = "device creation";
    let Scope { stuck, gate, marker } = scope;
    if let Some(why) = stuck.reason() {
        return Err(why);
    }
    // the thread's own share of "GPU work in flight", released when the driver work really ends
    // (before the result is sent; with the thread when it was given up on)
    let work = gate.enter().ok_or(CLOSING)?;
    let phase = Arc::new(AtomicU8::new(WAITING));
    let (tx, rx) = mpsc::sync_channel(1);
    let worker_phase = Arc::clone(&phase);
    let progress = Arc::new(AtomicU64::new(0));
    let worker_progress = Arc::clone(&progress);
    std::thread::Builder::new()
        .name("lc-gpu-device".into())
        .spawn(move || {
            let work = work;
            PROGRESS.with(|p| *p.borrow_mut() = Some(worker_progress));
            let r = marker
                .run(backends, || std::panic::catch_unwind(std::panic::AssertUnwindSafe(make)).unwrap_or_else(|_| Err(format!("{WHAT} panicked"))));
            {
                let mut s = stuck.lock();
                if worker_phase.load(Ordering::Relaxed) == ABANDONED {
                    // the caller gave up: never drop a device into the driver that just hung
                    s.threads = s.threads.saturating_sub(1);
                    if s.threads == 0 {
                        s.reason = None;
                    }
                    drop(s);
                    log::warn!("gpu: a device creation that was given up on has finished after all");
                    std::mem::forget(r);
                    // (nobody waits for its result: its work in flight ends with the thread)
                    drop(work);
                    return;
                }
                worker_phase.store(DONE, Ordering::Relaxed);
            }
            // the driver work is over: leave before handing the result over, so that a caller that
            // has it never still sees this thread in flight (one that needs the gate held across
            // the hand-over holds a work of its own, as `crate::device` does)
            drop(work);
            if let Err(mpsc::SendError(r)) = tx.send(r) {
                // can't happen (once DONE, the caller waits for the result); never drop a device
                // into the driver outside the gate
                std::mem::forget(r);
            }
        })
        .map_err(|e| format!("{WHAT}: could not start its thread ({e})"))?;
    // checked a few times per limit: a creation that keeps making progress gets the limit again
    let start = Instant::now();
    let cap = limit.saturating_mul(PROGRESS_CAP);
    let slice = (limit / 8).max(Duration::from_millis(1));
    let (mut seen, mut last) = (0, start);
    loop {
        let now = Instant::now();
        let deadline = (last + limit).min(start + cap);
        let wait = deadline.saturating_duration_since(now).min(slice);
        match rx.recv_timeout(wait) {
            Ok(r) => return r,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(format!("{WHAT} panicked")),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let ticks = progress.load(Ordering::Relaxed);
                if ticks != seen {
                    (seen, last) = (ticks, Instant::now());
                }
                if Instant::now() < (last + limit).min(start + cap) {
                    continue;
                }
                break;
            }
        }
    }
    let mut s = stuck.lock();
    if phase.load(Ordering::Relaxed) == DONE {
        // it finished just now: the result is on its way
        drop(s);
        return rx.recv().unwrap_or_else(|_| Err(format!("{WHAT} panicked")));
    }
    phase.store(ABANDONED, Ordering::Relaxed);
    let why = if seen == 0 {
        format!("GPU {WHAT} did not finish in {} ({}): the graphics driver or display server is not responding", show(limit), limit_env())
    } else if last + limit > start + cap {
        format!("GPU {WHAT} did not finish in {} ({} times {}): the graphics driver is too slow", show(cap), PROGRESS_CAP, limit_env())
    } else {
        format!(
            "GPU {WHAT} stopped making progress for {} ({}) after {} kernels: the graphics driver or display server is not responding",
            show(limit),
            limit_env(),
            seen
        )
    };
    log::warn!("gpu: {why}; the GPU is not used");
    s.threads = s.threads.saturating_add(1);
    Err(s.reason.get_or_insert(why).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    const B: wgpu::Backends = wgpu::Backends::VULKAN;

    /// A test's own stuck-state, gate and marker (a file in a fresh folder of its own).
    struct Local {
        stuck: &'static Stuck,
        gate: &'static Gate,
        active: &'static Mutex<u32>,
        dir: PathBuf,
    }

    impl Local {
        fn new(name: &str, stuck: &'static Stuck, gate: &'static Gate, active: &'static Mutex<u32>) -> Local {
            let dir = std::env::temp_dir().join(format!("lc-gpu-init-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Local { stuck, gate, active, dir }
        }

        fn marker(&self) -> PathBuf {
            self.dir.join("gpu-init.marker")
        }

        fn scope(&self) -> Scope {
            Scope { stuck: self.stuck, gate: self.gate, marker: InitMarker::at(self.marker(), self.active) }
        }
    }

    impl Drop for Local {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Runs `f` on a thread and fails the test if it has not returned after 20 s (instead of
    /// hanging the test binary, which is what a creation without a limit did).
    fn within_20_s<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(20)).expect("device creation still blocked after 20 s: it has no time limit")
    }

    /// Waits up to 20 s for `done`.
    fn eventually(what: &str, done: impl Fn() -> bool) {
        let t0 = Instant::now();
        while !done() {
            assert!(t0.elapsed() < Duration::from_secs(20), "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // Given a driver call that never returns (an unresponsive display server), device creation
    // fails after the limit with a reason; a second creation then fails at once without starting
    // another blocked thread; once the driver comes back, creation works again
    #[test]
    fn a_device_creation_that_never_returns_fails_after_the_limit() {
        static STUCK: Stuck = Stuck::new();
        static GATE: Gate = Gate::new();
        static ACTIVE: Mutex<u32> = Mutex::new(0);
        let local = Local::new("limit", &STUCK, &GATE, &ACTIVE);
        let (release, blocked) = mpsc::channel::<()>();
        let t0 = Instant::now();
        let scope = local.scope();
        let r = within_20_s(move || {
            create_on(scope, Duration::from_millis(200), B, move || -> Result<u32, String> {
                let _ = blocked.recv(); // "inside the driver" until the test lets go
                Ok(1)
            })
        });
        let took = t0.elapsed();
        let why = r.expect_err("a creation past its limit is an error");
        assert!(why.contains("did not finish in 200 ms"), "{why}");
        assert!(why.contains(&limit_env()), "the message names the setting: {why}");
        assert!(took >= Duration::from_millis(200), "waited the limit: {took:?}");

        // another creation does not start a second blocked thread
        let started = Arc::new(AtomicBool::new(false));
        let s2 = Arc::clone(&started);
        let t1 = Instant::now();
        let scope = local.scope();
        let again = within_20_s(move || {
            create_on(scope, Duration::from_secs(30), B, move || {
                s2.store(true, Ordering::Relaxed);
                Ok(2)
            })
        });
        assert_eq!(again, Err(why.clone()), "the same reason, at once");
        assert!(t1.elapsed() < Duration::from_secs(10));
        assert!(!started.load(Ordering::Relaxed), "nothing new went into the driver");

        // the driver answers after all: the late result is let go and creation is tried again
        drop(release);
        eventually("the released thread never finished", || STUCK.reason().is_none());
        let scope = local.scope();
        assert_eq!(within_20_s(move || create_on(scope, Duration::from_secs(30), B, || Ok(3))), Ok(3));
    }

    /// A first start compiles every kernel, which can take longer than the limit (30 s on two
    /// current cards against a 30 s limit): as long as kernels keep coming, the creation is waited for.
    #[test]
    fn a_slow_creation_that_keeps_making_progress_is_waited_for() {
        static STUCK: Stuck = Stuck::new();
        static GATE: Gate = Gate::new();
        static ACTIVE: Mutex<u32> = Mutex::new(0);
        let local = Local::new("progress", &STUCK, &GATE, &ACTIVE);
        let t0 = Instant::now();
        let scope = local.scope();
        let r = within_20_s(move || {
            // a limit with room for a busy machine: beside the other tests, a step (or the return
            // after the last one) can be a few hundred ms late without anything hanging
            create_on(scope, Duration::from_secs(1), B, move || -> Result<u32, String> {
                // twelve "kernels", one every 125 ms: 1.5 s in all, one and a half times the limit
                let progress = Progress::current();
                let workers: Vec<_> = (0..2)
                    .map(|_| {
                        let progress = progress.clone();
                        std::thread::spawn(move || {
                            for _ in 0..6 {
                                std::thread::sleep(Duration::from_millis(250));
                                progress.tick();
                            }
                        })
                    })
                    .collect();
                for w in workers {
                    w.join().map_err(|_| "worker panicked".to_string())?;
                }
                Ok(7)
            })
        });
        assert_eq!(r, Ok(7), "a creation that keeps making progress gets the time it needs");
        assert!(t0.elapsed() >= Duration::from_millis(1500), "{:?}", t0.elapsed());
        assert!(STUCK.reason().is_none());
    }

    /// Progress that stops is a creation that hangs: given up on a limit after the last step.
    #[test]
    fn a_creation_that_stops_making_progress_fails_a_limit_after_its_last_step() {
        static STUCK: Stuck = Stuck::new();
        static GATE: Gate = Gate::new();
        static ACTIVE: Mutex<u32> = Mutex::new(0);
        let local = Local::new("stalled", &STUCK, &GATE, &ACTIVE);
        let (release, blocked) = mpsc::channel::<()>();
        let t0 = Instant::now();
        let scope = local.scope();
        let r = within_20_s(move || {
            create_on(scope, Duration::from_millis(200), B, move || -> Result<u32, String> {
                let progress = Progress::current();
                for _ in 0..4 {
                    std::thread::sleep(Duration::from_millis(100));
                    progress.tick();
                }
                let _ = blocked.recv(); // then stuck in the driver
                Ok(1)
            })
        });
        let took = t0.elapsed();
        let why = r.expect_err("a creation that stops making progress is an error");
        assert!(why.contains("stopped making progress for 200 ms"), "{why}");
        assert!(why.contains("after 4 kernels"), "{why}");
        assert!(took >= Duration::from_millis(600), "waited past the last step: {took:?}");
        drop(release);
        eventually("the released thread never finished", || STUCK.reason().is_none());
    }

    // Given a creation left behind past its limit, the process is not idle for a quit (issue #620:
    // no driver teardown under it) and the crash sentinel stays, until the driver work really
    // ends; the quit's own wait stays bounded meanwhile
    #[test]
    fn a_creation_left_behind_keeps_the_gpu_busy_and_the_marker_until_it_ends() {
        static STUCK: Stuck = Stuck::new();
        static GATE: Gate = Gate::new();
        static ACTIVE: Mutex<u32> = Mutex::new(0);
        let local = Local::new("busy", &STUCK, &GATE, &ACTIVE);
        let (release, blocked) = mpsc::channel::<()>();
        let scope = local.scope();
        let r = within_20_s(move || {
            create_on(scope, Duration::from_millis(100), B, move || -> Result<u32, String> {
                let _ = blocked.recv();
                Ok(1)
            })
        });
        assert!(r.is_err(), "given up on");
        let marker = local.marker();
        assert!(marker.exists(), "the sentinel stays while the driver work runs");
        let text = std::fs::read_to_string(&marker).unwrap_or_default();
        assert!(text.contains("device and kernel creation (backends") && text.contains("VULKAN"), "{text}");

        // a quit now: nothing new starts, and the wait for what is in flight ends at its deadline
        GATE.close();
        let t0 = Instant::now();
        assert!(!GATE.wait_idle(Duration::from_millis(100)), "the creation left behind is still in flight");
        assert!(t0.elapsed() < Duration::from_secs(10), "the quit's wait stays bounded");
        assert!(marker.exists(), "a quit now leaves the marker for the next launch");

        // and a creation asked for once the gate is closed does not start
        let scope = local.scope();
        assert_eq!(create_on::<u32>(scope, Duration::from_secs(30), B, || Ok(2)), Err(STUCK.reason().unwrap_or_default()));

        drop(release);
        assert!(GATE.wait_idle(Duration::from_secs(20)), "idle once the driver work ended");
        eventually("the marker is removed once the driver work ended", || !marker.exists());
    }

    // Given a creation that returns in time, its result (device or reason) comes back unchanged,
    // and neither the work in flight nor the marker is left
    #[test]
    fn a_creation_in_time_returns_its_result() {
        static STUCK: Stuck = Stuck::new();
        static GATE: Gate = Gate::new();
        static ACTIVE: Mutex<u32> = Mutex::new(0);
        let local = Local::new("in-time", &STUCK, &GATE, &ACTIVE);
        let marker = local.marker();
        let m = marker.clone();
        assert_eq!(create_on(local.scope(), Duration::from_secs(30), B, move || Ok(m.exists())), Ok(true), "under the marker");
        assert!(!marker.exists());
        let no = create_on::<u32>(local.scope(), Duration::from_secs(30), B, || Err("no GPU adapter".into()));
        assert_eq!(no, Err("no GPU adapter".to_string()));
        assert!(STUCK.reason().is_none());
        assert!(GATE.wait_idle(Duration::from_secs(20)));
        assert!(!marker.exists());
        // once the process is ending, nothing is created
        GATE.close();
        assert_eq!(create_on::<u32>(local.scope(), Duration::from_secs(30), B, || Ok(1)), Err(CLOSING.to_string()));
    }

    // Given a creation that returned (a device or a reason), its thread is no longer GPU work in
    // flight: a quit right after it doesn't wait for it (the flaky `wait_idle(ZERO)` right after
    // `available()` in tests/shutdown.rs, which found the creation thread still between handing
    // its result over and leaving the gate)
    #[test]
    fn a_creation_that_returned_is_no_longer_in_flight() {
        static STUCK: Stuck = Stuck::new();
        static GATE: Gate = Gate::new();
        static ACTIVE: Mutex<u32> = Mutex::new(0);
        let local = Local::new("returned", &STUCK, &GATE, &ACTIVE);
        let mut busy = 0;
        for i in 0..400u32 {
            let r = create_on(local.scope(), Duration::from_secs(30), B, move || if i % 2 == 0 { Ok(i) } else { Err("no GPU adapter".into()) });
            assert_eq!(r.is_ok(), i % 2 == 0, "{r:?}");
            if !GATE.wait_idle(Duration::ZERO) {
                busy += 1;
                assert!(GATE.wait_idle(Duration::from_secs(20)), "the creation thread ends");
            }
        }
        assert_eq!(busy, 0, "a returned creation was still in flight {busy} of 400 times");
    }

    // Given a creation that panics (inside the driver or ours), the caller gets an error, not the
    // panic, and the marker is removed (the process survived)
    #[test]
    fn a_creation_that_panics_is_an_error() {
        static STUCK: Stuck = Stuck::new();
        static GATE: Gate = Gate::new();
        static ACTIVE: Mutex<u32> = Mutex::new(0);
        let local = Local::new("panic", &STUCK, &GATE, &ACTIVE);
        #[allow(clippy::panic)]
        let r = create_on::<u32>(local.scope(), Duration::from_secs(30), B, || panic!("driver bug (expected in this test)"));
        assert_eq!(r, Err("device creation panicked".to_string()));
        assert!(STUCK.reason().is_none());
        assert!(GATE.wait_idle(Duration::from_secs(20)));
        assert!(!local.marker().exists());
    }

    // Given <PREFIX>_GPU_INIT_TIMEOUT, whole seconds from 1 up to an hour are used; anything
    // else is refused (and the default used)
    #[test]
    fn the_limit_comes_from_whole_seconds() {
        assert_eq!(parse_limit(None), Some(DEFAULT_LIMIT));
        assert_eq!(parse_limit(Some("  ")), Some(DEFAULT_LIMIT));
        assert_eq!(parse_limit(Some("5")), Some(Duration::from_secs(5)));
        assert_eq!(parse_limit(Some(" 120 ")), Some(Duration::from_secs(120)));
        assert_eq!(parse_limit(Some("999999")), Some(Duration::from_secs(MAX_LIMIT_SECS)));
        assert_eq!(parse_limit(Some("99999999999999999999999")), Some(Duration::from_secs(MAX_LIMIT_SECS)));
        for bad in ["0", "-3", "1.5", "abc", "NaN", "inf", "30s"] {
            assert_eq!(parse_limit(Some(bad)), None, "{bad}");
        }
        assert_eq!(show(Duration::from_secs(30)), "30 s");
        assert_eq!(show(Duration::from_millis(250)), "250 ms");
    }
}
