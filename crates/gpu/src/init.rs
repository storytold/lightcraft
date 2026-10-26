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
//! and the blocked thread is left behind. While one is left behind, later creations (another kind
//! of device) fail at once with the same reason instead of leaving another thread behind; should
//! the driver come back, the late device is leaked (never dropped into a driver that just hung)
//! and creations are tried again.
//!
//! The error scopes popped after creation (`scope.pop()`) need no limit: on native wgpu they
//! resolve at once, and they are thread-local, so they could not move to another thread anyway.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use std::time::Duration;

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

/// Run device creation `make` on a thread of its own and wait at most [`limit`] for it. A panic in
/// it, or a deadline that passes, is an `Err` saying so (the blocked thread is left behind).
pub(crate) fn create<T: Send + 'static>(what: &str, make: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    create_on(&STUCK, limit(), what, make)
}

/// [`create`] with an explicit limit and stuck-state (the tests use their own, so that a test's
/// stuck creation cannot stop the real devices that other tests in the process create).
fn create_on<T: Send + 'static>(
    stuck: &'static Stuck,
    limit: Duration,
    what: &str,
    make: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    if let Some(why) = stuck.reason() {
        return Err(why);
    }
    let phase = Arc::new(AtomicU8::new(WAITING));
    let (tx, rx) = mpsc::sync_channel(1);
    let worker_phase = Arc::clone(&phase);
    let panicked = format!("{what} panicked");
    std::thread::Builder::new()
        .name("lc-gpu-device".into())
        .spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(make)).unwrap_or_else(|_| Err(panicked));
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
                    return;
                }
                worker_phase.store(DONE, Ordering::Relaxed);
            }
            let _ = tx.send(r);
        })
        .map_err(|e| format!("{what}: could not start its thread ({e})"))?;
    match rx.recv_timeout(limit) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(format!("{what} panicked")),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let mut s = stuck.lock();
            if phase.load(Ordering::Relaxed) == DONE {
                // it finished just now: the result is on its way
                drop(s);
                return rx.recv().unwrap_or_else(|_| Err(format!("{what} panicked")));
            }
            phase.store(ABANDONED, Ordering::Relaxed);
            let why = format!("GPU {what} did not finish in {} ({LIMIT_ENV}): the graphics driver or display server is not responding", show(limit));
            log::warn!("gpu: {why}; the GPU is not used");
            s.threads = s.threads.saturating_add(1);
            Err(s.reason.get_or_insert(why).clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    /// Runs `f` on a thread and fails the test if it has not returned after 20 s (instead of
    /// hanging the test binary, which is what a creation without a limit did).
    fn within_20_s<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(20)).expect("device creation still blocked after 20 s: it has no time limit")
    }

    // Given a driver call that never returns (an unresponsive display server), device creation
    // fails after the limit with a reason; a second creation then fails at once without starting
    // another blocked thread; once the driver comes back, creation works again
    #[test]
    fn a_device_creation_that_never_returns_fails_after_the_limit() {
        static STUCK: Stuck = Stuck::new();
        let (release, blocked) = mpsc::channel::<()>();
        let t0 = Instant::now();
        let r = within_20_s(move || {
            create_on(&STUCK, Duration::from_millis(200), "device creation", move || -> Result<u32, String> {
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
        let again = within_20_s(move || {
            create_on(&STUCK, Duration::from_secs(30), "device creation", move || {
                s2.store(true, Ordering::Relaxed);
                Ok(2)
            })
        });
        assert_eq!(again, Err(why.clone()), "the same reason, at once");
        assert!(t1.elapsed() < Duration::from_secs(10));
        assert!(!started.load(Ordering::Relaxed), "nothing new went into the driver");

        // the driver answers after all: the late result is let go and creation is tried again
        drop(release);
        let t2 = Instant::now();
        while STUCK.reason().is_some() {
            assert!(t2.elapsed() < Duration::from_secs(20), "the released thread never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(within_20_s(|| create_on(&STUCK, Duration::from_secs(30), "device creation", || Ok(3))), Ok(3));
    }

    // Given a creation that returns in time, its result (device or reason) comes back unchanged
    #[test]
    fn a_creation_in_time_returns_its_result() {
        static STUCK: Stuck = Stuck::new();
        assert_eq!(create_on(&STUCK, Duration::from_secs(30), "device creation", || Ok(7)), Ok(7));
        let no = create_on::<u32>(&STUCK, Duration::from_secs(30), "device creation", || Err("no GPU adapter".into()));
        assert_eq!(no, Err("no GPU adapter".to_string()));
        assert!(STUCK.reason().is_none());
    }

    // Given a creation that panics (inside the driver or ours), the caller gets an error, not the panic
    #[test]
    fn a_creation_that_panics_is_an_error() {
        static STUCK: Stuck = Stuck::new();
        #[allow(clippy::panic)]
        let r = create_on::<u32>(&STUCK, Duration::from_secs(30), "device creation", || panic!("driver bug (expected in this test)"));
        assert_eq!(r, Err("device creation panicked".to_string()));
        assert!(STUCK.reason().is_none());
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
