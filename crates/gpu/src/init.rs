//! A time limit on creating a GPU device (issue #3068).
//!
//! Creating the wgpu instance, adapter and device, and compiling the kernels after it, are calls
//! into the graphics driver, and on Linux the driver may talk to the display server first. With an
//! unresponsive one (a crash-looping WSLg compositor, a socket that accepts and never answers)
//! those calls never return, and wgpu's native calls are synchronous (`pollster::block_on` gets an
//! already-finished future), so there is nothing to time out from the calling thread.
//!
//! So every device (rendering, denoise, faces) is made by [`create`] on a thread of its own, and
//! the caller waits at most [`limit`] (`LIGHTCRAFT_GPU_INIT_TIMEOUT`). Past it the caller gets an
//! `Err` that it caches like any other creation failure — the GPU is unavailable for the process —
//! and the blocked thread is left behind. That thread keeps what has to last as long as the driver
//! work does: its share of the GPU work in flight (`exit.rs`: a quit then reports the GPU busy
//! instead of tearing the driver down under it) and the crash sentinel (`backend.rs`: a quit
//! leaves the marker, so the next launch starts on the CPU). While one is left behind, later creations (another kind
//! of device) fail at once with the same reason instead of leaving another thread behind, and
//! that lasts until every creation thread started so far has come out of the driver (one started
//! before the limit passed may be stuck in it too); should the driver come back, the late device
//! is leaked (never dropped into a driver that just hung) and creations are tried again.
//!
//! The error scopes popped after creation (`scope.pop()`) need no limit: on native wgpu they
//! resolve at once, and they are thread-local, so they could not move to another thread anyway.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use std::time::Duration;

use crate::backend::InitMarker;
use crate::exit::Gate;

/// The environment variable that changes [`DEFAULT_LIMIT`] (whole seconds).
pub(crate) const LIMIT_ENV: &str = "LIGHTCRAFT_GPU_INIT_TIMEOUT";
/// How long device creation may take by default. Normally well under a second, but a first run
/// that compiles every kernel with a slow driver compiler (DX12's FXC) takes seconds: generous.
pub(crate) const DEFAULT_LIMIT: Duration = Duration::from_secs(30);
/// The longest limit [`LIMIT_ENV`] can ask for (an hour).
const MAX_LIMIT_SECS: u64 = 3600;

/// How long a device creation may take: [`LIMIT_ENV`], else [`DEFAULT_LIMIT`]. Read once.
pub(crate) fn limit() -> Duration {
    static LIMIT: OnceLock<Duration> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        let v = std::env::var(LIMIT_ENV).ok();
        parse_limit(v.as_deref()).unwrap_or_else(|| {
            log::warn!(
                "gpu: {LIMIT_ENV}={:?} is not a whole number of seconds from 1 to {MAX_LIMIT_SECS}; using {} s",
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

/// Device creations admitted and not yet out of the driver, and the reason given when the first
/// of them was given up on.
pub(crate) struct Stuck(Mutex<StuckState>);

struct StuckState {
    /// Creations admitted whose work has not ended: waited for, or given up on and left behind.
    inside: usize,
    /// Set when a creation is given up on; cleared once `inside` is back to 0.
    reason: Option<String>,
}

impl Stuck {
    pub(crate) const fn new() -> Stuck {
        Stuck(Mutex::new(StuckState { inside: 0, reason: None }))
    }

    fn lock(&self) -> MutexGuard<'_, StuckState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Why device creation is not tried now (one given up on is still in the driver), or `None`.
    #[cfg(test)]
    fn reason(&self) -> Option<String> {
        self.lock().reason.clone()
    }

    /// Count a creation in, unless one given up on is still in the driver (then why not).
    fn admit(&self) -> Result<(), String> {
        let mut s = self.lock();
        if let Some(why) = &s.reason {
            return Err(why.clone());
        }
        s.inside = s.inside.saturating_add(1);
        Ok(())
    }
}

impl StuckState {
    /// An admitted creation is out of the driver (or never went in); the last one out lets
    /// creations be tried again.
    fn leave(&mut self) {
        self.inside = self.inside.saturating_sub(1);
        if self.inside == 0 {
            self.reason = None;
        }
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

/// Why no device is created once the process is ending ([`crate::begin_shutdown`]).
const CLOSING: &str = "LightCraft is closing";

/// What a creation thread carries besides the work: the stuck-state it reports to, the shutdown
/// gate it counts as GPU work on, and the crash sentinel it runs under.
struct Scope {
    stuck: &'static Stuck,
    gate: &'static Gate,
    marker: InitMarker,
}

/// Create a device on `backends` with `make` on a thread of its own and wait at most [`limit`] for
/// it. A panic in it, or a deadline that passes, is an `Err` saying so.
///
/// The thread, not the caller, holds what must last as long as the driver work does: it counts as
/// GPU work in flight ([`crate::wait_idle`] is not idle until it ends, issue #620) and runs under
/// the crash sentinel ([`crate::backend::set_init_marker`]). So a creation left behind past its
/// limit keeps both: a quit meanwhile reports the GPU busy, and leaves the marker for the next
/// launch, which then starts on the CPU.
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
    // counted from here until the thread's driver work ends, so that a creation given up on meanwhile
    // keeps new ones out until this one is out too
    stuck.admit()?;
    // the thread's own share of "GPU work in flight", released when the driver work really ends
    let Some(work) = gate.enter() else {
        stuck.lock().leave();
        return Err(CLOSING.into());
    };
    let phase = Arc::new(AtomicU8::new(WAITING));
    let (tx, rx) = mpsc::sync_channel(1);
    let worker_phase = Arc::clone(&phase);
    let spawned = std::thread::Builder::new().name("lc-gpu-device".into()).spawn(move || {
        let _work = work;
        let r =
            marker.run(backends, || std::panic::catch_unwind(std::panic::AssertUnwindSafe(make)).unwrap_or_else(|_| Err(format!("{WHAT} panicked"))));
        {
            let mut s = stuck.lock();
            s.leave();
            if worker_phase.load(Ordering::Relaxed) == ABANDONED {
                // the caller gave up: never drop a device into the driver that just hung
                drop(s);
                log::warn!("gpu: a device creation that was given up on has finished after all");
                std::mem::forget(r);
                return;
            }
            worker_phase.store(DONE, Ordering::Relaxed);
        }
        let _ = tx.send(r);
    });
    if let Err(e) = spawned {
        stuck.lock().leave();
        return Err(format!("{WHAT}: could not start its thread ({e})"));
    }
    match rx.recv_timeout(limit) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(format!("{WHAT} panicked")),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let mut s = stuck.lock();
            if phase.load(Ordering::Relaxed) == DONE {
                // it finished just now: the result is on its way
                drop(s);
                return rx.recv().unwrap_or_else(|_| Err(format!("{WHAT} panicked")));
            }
            phase.store(ABANDONED, Ordering::Relaxed);
            let why =
                format!("GPU {WHAT} did not finish in {} ({LIMIT_ENV}): the graphics driver or display server may not be responding", show(limit));
            log::warn!("gpu: {why}; the GPU is not used");
            Err(s.reason.get_or_insert(why).clone())
        }
    }
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
        assert!(why.contains(LIMIT_ENV), "the message names the setting: {why}");
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

    // Given two creations in the driver at once (B started before A's limit passed) and A given
    // up on, A coming back does not let new creations in while B is still inside: they keep
    // failing at once until B is out too
    #[test]
    fn a_creation_still_inside_keeps_new_ones_out_after_one_given_up_on_returns() {
        static STUCK: Stuck = Stuck::new();
        static GATE: Gate = Gate::new();
        static ACTIVE: Mutex<u32> = Mutex::new(0);
        let local = Local::new("two-inside", &STUCK, &GATE, &ACTIVE);
        // B: waited for (long limit), inside the driver until released
        let (release_b, blocked_b) = mpsc::channel::<()>();
        let (b_in, b_entered) = mpsc::channel::<()>();
        let scope = local.scope();
        let b = std::thread::spawn(move || {
            create_on(scope, Duration::from_secs(30), B, move || -> Result<u32, String> {
                let _ = b_in.send(());
                let _ = blocked_b.recv();
                Ok(2)
            })
        });
        b_entered.recv_timeout(Duration::from_secs(20)).expect("B never reached the driver");
        // A: given up on after 100 ms
        let (release_a, blocked_a) = mpsc::channel::<()>();
        let scope = local.scope();
        let why = within_20_s(move || {
            create_on(scope, Duration::from_millis(100), B, move || -> Result<u32, String> {
                let _ = blocked_a.recv();
                Ok(1)
            })
        })
        .expect_err("A is given up on");

        // A comes back; B is still inside
        drop(release_a);
        eventually("A never came out of the driver", || STUCK.lock().inside == 1);
        assert_eq!(STUCK.reason(), Some(why.clone()), "the reason stays while B is inside");
        let started = Arc::new(AtomicBool::new(false));
        let s3 = Arc::clone(&started);
        let scope = local.scope();
        let c = within_20_s(move || {
            create_on(scope, Duration::from_secs(30), B, move || {
                s3.store(true, Ordering::Relaxed);
                Ok(3)
            })
        });
        assert_eq!(c, Err(why), "C fails at once");
        assert!(!started.load(Ordering::Relaxed), "nothing new went into the driver");

        // B comes out: its caller gets the device, and creations are tried again
        drop(release_b);
        assert_eq!(b.join().ok(), Some(Ok(2)));
        assert!(STUCK.reason().is_none());
        assert_eq!(
            within_20_s({
                let scope = local.scope();
                move || create_on(scope, Duration::from_secs(30), B, || Ok(4))
            }),
            Ok(4)
        );
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

    // Given LIGHTCRAFT_GPU_INIT_TIMEOUT, whole seconds from 1 up to an hour are used; anything
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
