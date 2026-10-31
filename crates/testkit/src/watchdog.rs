//! A watchdog for test binaries. Stable libtest has no per-test timeout: a test stuck in a wait
//! keeps its binary (and the app's render workers) alive for good, and the run only looks busy.
//! A test that calls [`watch`] (ui-egui's `Headless::new` does it for every headless test) is
//! registered under its thread's name, which libtest sets to the test's path. One thread per
//! process checks the registered tests; once one of them has run longer than the limit it prints
//! every registered test still running, with how long each has run, and ends the process with
//! [`WATCHDOG_EXIT_CODE`]. A
//! stuck run then ends with a list of names instead of hanging.
//!
//! **The limit** is `LIGHTCRAFT_TEST_WATCHDOG_MIN` minutes per test (fractions allowed),
//! [`DEFAULT_WATCHDOG_MINUTES`] when unset, stretched by [`crate::time_scale`]. `0` or `off`
//! turns the watchdog off (for a test paused in a debugger). Anything else that isn't a positive
//! number is ignored with a note on stderr, and the default applies.
//!
//! **Why 30 minutes by default, and on.** Off unless set would leave exactly the runs that need it
//! unguarded: an ad-hoc `cargo test` that hangs. The limit is per test, not per binary, so a long
//! binary (the ui-egui unit tests take minutes at 4 threads, far more on a cold debug build on a
//! loaded machine) never trips it; the slowest single test takes about a minute under load, so 30
//! minutes is far past anything but a test that will never finish, and `LIGHTCRAFT_TIME_SCALE`
//! stretches it on a machine that is slower still.
//!
//! **How it ends the process.** A helper thread prints the report and calls `std::process::exit`
//! with [`WATCHDOG_EXIT_CODE`], so a stuck run normally leaves no crash report or core dump
//! behind. The process is stuck by definition, so that can get stuck too: a stuck test may hold
//! stderr's lock, an exit handler (a GPU driver's) may hang. The watchdog thread itself owns the
//! hard deadline and does no I/O: it aborts the process if it still runs [`ABORT_AFTER_EXIT`]
//! later, or at once if the helper thread can't be started.
//!
//! For testkit's own tests only, `LIGHTCRAFT_TEST_WATCHDOG_GRACE_MS` replaces that grace period
//! (whole milliseconds, held to 100 ms..=10 min; anything else keeps [`ABORT_AFTER_EXIT`]), so
//! the test of the abort doesn't wait ten seconds.
//!
//! The report goes straight to the process's stderr: libtest captures the output of a test's
//! threads (the watchdog's thread starts from the first watched test's), and a captured report
//! would be lost with the process.

use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

/// The environment variable holding the per-test limit in minutes (`0` or `off`: no watchdog).
pub const WATCHDOG_ENV: &str = "LIGHTCRAFT_TEST_WATCHDOG_MIN";

/// The per-test limit in minutes when [`WATCHDOG_ENV`] is unset, before [`crate::time_scale`].
pub const DEFAULT_WATCHDOG_MINUTES: f64 = 30.0;

/// The exit status of a test binary the watchdog ended: 124, as `timeout(1)` uses.
pub const WATCHDOG_EXIT_CODE: i32 = 124;

/// How long printing the report and exiting may take before the watchdog aborts the process.
pub const ABORT_AFTER_EXIT: Duration = Duration::from_secs(10);

/// Replaces [`ABORT_AFTER_EXIT`], for testkit's own tests (milliseconds).
const GRACE_ENV: &str = "LIGHTCRAFT_TEST_WATCHDOG_GRACE_MS";

/// The grace period `LIGHTCRAFT_TEST_WATCHDOG_GRACE_MS=value` means: whole milliseconds held to
/// 100 ms..=10 min; unset, empty or not a whole number keeps [`ABORT_AFTER_EXIT`].
fn parse_grace(value: Option<&str>) -> Duration {
    match value.map(str::trim).and_then(|v| v.parse::<u64>().ok()) {
        Some(ms) => Duration::from_millis(ms.clamp(100, 600_000)),
        None => ABORT_AFTER_EXIT,
    }
}

/// One registered test.
struct Entry {
    id: u64,
    test: String,
    since: Instant,
    /// Alive while the registering thread runs: libtest runs each test on a thread of its own, so
    /// a registration whose guard was leaked (or kept in a static) is dropped with its thread
    /// instead of tripping the watchdog later in the run.
    thread: Weak<()>,
}

static WATCHED: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static THREAD_ALIVE: Arc<()> = Arc::new(());
}

/// A test's registration with the watchdog; dropping it (at the end of the test, also when the
/// test panics) ends the registration.
#[must_use = "the test is watched only while the guard lives: `let _watch = watch();`"]
#[derive(Debug)]
pub struct Watch {
    id: u64,
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut watched = WATCHED.lock().unwrap_or_else(PoisonError::into_inner);
        watched.retain(|e| e.id != self.id);
    }
}

/// Register the current test (by its thread's name) with the watchdog until the returned guard
/// is dropped, and start the watchdog thread on the first call. Hold the guard for the whole test:
/// `let _watch = lightcraft_testkit::watch();`.
pub fn watch() -> Watch {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let thread = std::thread::current();
    let test = thread.name().unwrap_or("<unnamed thread>").to_owned();
    let alive = THREAD_ALIVE.try_with(Arc::downgrade).unwrap_or_default();
    WATCHED.lock().unwrap_or_else(PoisonError::into_inner).push(Entry { id, test, since: Instant::now(), thread: alive });
    if let Some(limit) = limit() {
        start(limit);
    }
    Watch { id }
}

/// The per-test limit ([`WATCHDOG_ENV`] × [`crate::time_scale`]), read once; `None` when off.
fn limit() -> Option<Duration> {
    static LIMIT: OnceLock<Option<Duration>> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        let value = std::env::var(WATCHDOG_ENV).ok();
        let (minutes, note) = parse_minutes(value.as_deref());
        if let Some(note) = note {
            crate::note(&format!("{WATCHDOG_ENV}: {note}"));
        }
        let minutes = minutes?;
        // a limit too large for a Duration is no limit
        Duration::try_from_secs_f64(minutes * 60.0).ok().map(crate::scaled)
    })
}

/// The minutes `LIGHTCRAFT_TEST_WATCHDOG_MIN=value` means (`None`: off) and a note when the value
/// was ignored.
fn parse_minutes(value: Option<&str>) -> (Option<f64>, Option<String>) {
    let Some(text) = value.map(str::trim).filter(|v| !v.is_empty()) else { return (Some(DEFAULT_WATCHDOG_MINUTES), None) };
    if text.eq_ignore_ascii_case("off") {
        return (None, None);
    }
    match text.parse::<f64>() {
        Ok(0.0) => (None, None),
        Ok(m) if m.is_finite() && m > 0.0 => (Some(m), None),
        _ => (Some(DEFAULT_WATCHDOG_MINUTES), Some(format!("{text:?} is not a number of minutes (or 0 / off); using {DEFAULT_WATCHDOG_MINUTES}"))),
    }
}

/// Start the watchdog thread, once per process.
fn start(limit: Duration) {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        // read here, not when the watchdog fires: a stuck process may hold the environment's lock
        let grace = parse_grace(std::env::var(GRACE_ENV).ok().as_deref());
        // check often enough that a short limit (tests of the watchdog) isn't overshot by much
        let poll = (limit / 20).clamp(Duration::from_millis(10), Duration::from_secs(5));
        let spawned = std::thread::Builder::new().name("lightcraft-testkit-watchdog".into()).spawn(move || {
            loop {
                std::thread::sleep(poll);
                let report = overdue(&WATCHED.lock().unwrap_or_else(PoisonError::into_inner), Instant::now(), limit);
                if let Some(report) = report {
                    end_process(report, grace);
                }
            }
        });
        if let Err(e) = spawned {
            crate::note(&format!("{WATCHDOG_ENV}: can't start the test watchdog ({e}); hung tests won't be stopped"));
        }
    });
}

/// End the process for a stuck test, on the watchdog thread. A helper thread prints `report` and
/// exits with [`WATCHDOG_EXIT_CODE`]; this thread does no I/O (stderr's lock may be held by the
/// stuck test) and aborts the process if it is still running `grace` ([`ABORT_AFTER_EXIT`]) later,
/// or at once when the helper can't be started.
fn end_process(report: String, grace: Duration) -> ! {
    let reporter = std::thread::Builder::new().name("lightcraft-testkit-watchdog-exit".into()).spawn(move || {
        // straight to stderr: `eprintln!` would go to the capture of the test that started the watchdog
        let _ = writeln!(std::io::stderr(), "{report}");
        std::process::exit(WATCHDOG_EXIT_CODE)
    });
    if reporter.is_ok() {
        std::thread::sleep(grace);
    }
    std::process::abort()
}

/// When a live registered test has run for `limit` or longer: the report printed before the
/// process ends, naming every live registered test, longest-running first.
fn overdue(entries: &[Entry], now: Instant, limit: Duration) -> Option<String> {
    let mut live: Vec<(&str, Duration)> =
        entries.iter().filter(|e| e.thread.strong_count() > 0).map(|e| (e.test.as_str(), now.saturating_duration_since(e.since))).collect();
    live.sort_by_key(|&(_, ran)| std::cmp::Reverse(ran));
    let &(stuck, ran) = live.first().filter(|(_, ran)| *ran >= limit)?;
    let mut report = format!(
        "lightcraft-testkit watchdog: `{stuck}` has run for {}, past the limit of {} ({WATCHDOG_ENV} × {}); \
         ending the test binary. Watched tests still running:",
        minutes(ran),
        minutes(limit),
        crate::TIME_SCALE_ENV,
    );
    for (test, ran) in &live {
        let _ = write!(report, "\n    {test} ({})", minutes(*ran));
    }
    Some(report)
}

/// `95.5 s` → `1m 35.5s`.
fn minutes(d: Duration) -> String {
    let secs = d.as_secs_f64();
    let whole = (secs / 60.0).floor();
    format!("{whole}m {:.1}s", secs - whole * 60.0)
}

/// The names of the watched tests still running, as the watchdog would list them (a test can
/// check that it is watched).
pub fn watched() -> Vec<String> {
    let watched = WATCHED.lock().unwrap_or_else(PoisonError::into_inner);
    watched.iter().filter(|e| e.thread.strong_count() > 0).map(|e| e.test.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set for the child process [`a_stuck_test_ends_its_binary_with_the_tests_still_running`] starts.
    const CHILD_ENV: &str = "LIGHTCRAFT_TESTKIT_WATCHDOG_CHILD";

    fn entry(test: &str, since: Instant, thread: &Arc<()>) -> Entry {
        Entry { id: 0, test: test.into(), since, thread: Arc::downgrade(thread) }
    }

    #[test]
    fn the_limit_reads_minutes_and_off() {
        assert_eq!(parse_minutes(None), (Some(DEFAULT_WATCHDOG_MINUTES), None));
        assert_eq!(parse_minutes(Some(" ")), (Some(DEFAULT_WATCHDOG_MINUTES), None));
        assert_eq!(parse_minutes(Some("45")), (Some(45.0), None));
        assert_eq!(parse_minutes(Some("0.5")), (Some(0.5), None));
        assert_eq!(parse_minutes(Some("0")), (None, None));
        assert_eq!(parse_minutes(Some("OFF")), (None, None));
        for bad in ["-1", "NaN", "inf", "ten", "1e400"] {
            let (minutes, note) = parse_minutes(Some(bad));
            assert_eq!(minutes, Some(DEFAULT_WATCHDOG_MINUTES), "{bad}");
            assert!(note.is_some(), "{bad}");
        }
    }

    #[test]
    fn a_test_past_the_limit_is_reported_with_everything_still_running() {
        let base = Instant::now();
        let (alive, gone) = (Arc::new(()), Arc::new(()));
        let entries = vec![
            entry("ui::quick", base + Duration::from_secs(50), &alive),
            entry("ui::stuck", base, &alive),
            entry("ui::leaked_by_a_finished_thread", base, &gone),
        ];
        drop(gone);
        let limit = Duration::from_secs(60);
        assert_eq!(overdue(&entries, base + Duration::from_secs(59), limit), None, "nothing is past the limit yet");
        let report = overdue(&entries, base + Duration::from_secs(60), limit).unwrap();
        assert!(report.contains("`ui::stuck` has run for 1m 0.0s, past the limit of 1m 0.0s"), "{report}");
        let stuck = report.find("\n    ui::stuck (1m 0.0s)").unwrap();
        let quick = report.find("\n    ui::quick (0m 10.0s)").unwrap();
        assert!(stuck < quick, "longest first: {report}");
        assert!(!report.contains("leaked"), "a finished thread's registration doesn't count: {report}");
    }

    #[test]
    fn nothing_is_overdue_without_live_tests() {
        let base = Instant::now();
        assert_eq!(overdue(&[], base + Duration::from_secs(3600), Duration::from_secs(1)), None);
        let gone = Arc::new(());
        let entries = vec![entry("t", base, &gone)];
        drop(gone);
        assert_eq!(overdue(&entries, base + Duration::from_secs(3600), Duration::from_secs(1)), None);
    }

    #[test]
    fn a_watch_registers_the_test_until_it_ends() {
        let me = std::thread::current().name().unwrap_or("<unnamed thread>").to_owned();
        let count = || watched().iter().filter(|t| **t == me).count();
        let before = count();
        let w = watch();
        assert_eq!(count(), before + 1);
        drop(w);
        assert_eq!(count(), before);
    }

    #[test]
    fn a_test_that_panics_or_leaks_its_watch_is_unregistered() {
        let named = |name: &str| watched().iter().filter(|t| *t == name).count();
        let panicked = std::thread::Builder::new()
            .name("testkit::panicking_test".into())
            .spawn(|| {
                let _watch = watch();
                panic!("the test fails");
            })
            .unwrap()
            .join();
        assert!(panicked.is_err());
        assert_eq!(named("testkit::panicking_test"), 0, "unwinding drops the guard");
        std::thread::Builder::new().name("testkit::leaking_test".into()).spawn(|| std::mem::forget(watch())).unwrap().join().unwrap();
        assert_eq!(named("testkit::leaking_test"), 0, "the thread ended with the test");
    }

    /// Run as a child by [`a_stuck_test_ends_its_binary_with_the_tests_still_running`]: a test stuck
    /// in a wait. Ends on its own after a minute, so a watchdog that never fires fails the parent
    /// rather than hanging it.
    #[test]
    #[ignore = "a stuck test: run as a child process by a_stuck_test_ends_its_binary_with_the_tests_still_running"]
    fn stuck_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        let _watch = watch();
        std::thread::sleep(Duration::from_secs(60));
    }

    /// Run as a child by [`a_stuck_test_holding_stderr_still_ends_its_binary`]: a test stuck while
    /// it holds stderr's lock, so the report can't be printed.
    #[test]
    #[ignore = "a stuck test: run as a child process by a_stuck_test_holding_stderr_still_ends_its_binary"]
    fn stuck_child_holding_stderr() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        let _watch = watch();
        let _stderr = std::io::stderr().lock();
        std::thread::sleep(Duration::from_secs(60));
    }

    /// Run as a child by [`a_mistyped_setting_is_reported_by_a_passing_test`]: a watched test that
    /// passes, so libtest shows none of its captured output.
    #[test]
    #[ignore = "run as a child process by a_mistyped_setting_is_reported_by_a_passing_test"]
    fn passing_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        let _watch = watch();
    }

    /// A mistyped `LIGHTCRAFT_TEST_WATCHDOG_MIN` or `LIGHTCRAFT_TIME_SCALE` is read on the first
    /// watched test's thread; its note reaches the process's stderr even though that test passes
    /// (with `eprintln!` it went to the test's capture, which libtest drops).
    #[test]
    fn a_mistyped_setting_is_reported_by_a_passing_test() {
        let c = run_stuck_child("watchdog::tests::passing_child", &[(WATCHDOG_ENV, "soon"), (crate::TIME_SCALE_ENV, "fast")], child_deadline());
        let (ran, stderr) = (c.ran, &c.stderr);
        let status = c.status.unwrap_or_else(|| panic!("the passing child still ran after {ran:?}: {stderr}"));
        assert!(status.success(), "{status:?}: {stderr}");
        assert!(stderr.contains(&format!("{WATCHDOG_ENV}: \"soon\" is not a number of minutes")), "{stderr}");
        assert!(stderr.contains(&format!("{}: \"fast\" is not a positive number", crate::TIME_SCALE_ENV)), "{stderr}");
    }

    /// How a child test run by [`run_stuck_child`] ended.
    struct Child {
        /// `None`: still running after the deadline, and killed.
        status: Option<std::process::ExitStatus>,
        stderr: String,
        ran: Duration,
        /// Run under `prlimit --core=1`, so an abort leaves no core dump.
        no_core: bool,
    }

    /// How long a stuck child may run before the parent kills it: 45 s stretched by the time scale,
    /// never shrunk, since the child's watchdog runs at scale 1 and the child still has to start.
    fn child_deadline() -> Duration {
        let d = Duration::from_secs(45);
        crate::scaled(d).max(d)
    }

    /// Run the ignored test `name` of this binary as a child under a watchdog of 0.3 s (and `env`),
    /// killing it if it still runs after `deadline`.
    fn run_stuck_child(name: &str, env: &[(&str, &str)], deadline: Duration) -> Child {
        let start = Instant::now();
        let exe = std::env::current_exe().unwrap();
        let configure = |mut c: std::process::Command| {
            // captured, as in a normal run: the report must get out anyway
            c.args(["--ignored", "--exact", name, "--test-threads", "1"])
                .env(CHILD_ENV, "1")
                .env(WATCHDOG_ENV, "0.005")
                .env(crate::TIME_SCALE_ENV, "1")
                .envs(env.iter().copied())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped());
            c
        };
        // Linux: an aborted child leaves no core dump. A limit of 1 byte (not 0) also stops a
        // core_pattern pipe (systemd-coredump, WSL's crash capture); without prlimit, run it as is.
        let limited = if cfg!(target_os = "linux") {
            let mut c = std::process::Command::new("prlimit");
            c.arg("--core=1:1").arg(&exe);
            configure(c).spawn().ok()
        } else {
            None
        };
        let no_core = limited.is_some();
        let mut child = match limited {
            Some(c) => c,
            None => configure(std::process::Command::new(&exe)).spawn().unwrap(),
        };
        let ended = loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if start.elapsed() > deadline {
                let _ = child.kill();
                break false;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let ran = start.elapsed();
        let out = child.wait_with_output().unwrap();
        Child { status: ended.then_some(out.status), stderr: String::from_utf8_lossy(&out.stderr).into_owned(), ran, no_core }
    }

    #[test]
    fn the_grace_period_reads_whole_milliseconds_within_bounds() {
        assert_eq!(parse_grace(None), ABORT_AFTER_EXIT);
        assert_eq!(parse_grace(Some("")), ABORT_AFTER_EXIT);
        assert_eq!(parse_grace(Some(" 500 ")), Duration::from_millis(500));
        assert_eq!(parse_grace(Some("0")), Duration::from_millis(100));
        assert_eq!(parse_grace(Some("99999999999999999")), Duration::from_secs(600));
        for bad in ["-5", "1.5", "NaN", "soon", "18446744073709551616"] {
            assert_eq!(parse_grace(Some(bad)), ABORT_AFTER_EXIT, "{bad}");
        }
    }

    #[test]
    fn a_stuck_test_ends_its_binary_with_the_tests_still_running() {
        let c = run_stuck_child("watchdog::tests::stuck_child", &[], child_deadline());
        let (ran, stderr) = (c.ran, &c.stderr);
        let status = c.status.unwrap_or_else(|| panic!("the watchdog didn't end the stuck test in {ran:?}: {stderr}"));
        assert_eq!(status.code(), Some(WATCHDOG_EXIT_CODE), "the watchdog ends the run: {stderr}");
        assert!(stderr.contains("lightcraft-testkit watchdog: `watchdog::tests::stuck_child` has run for"), "{stderr}");
        assert!(stderr.contains("\n    watchdog::tests::stuck_child ("), "{stderr}");
    }

    /// The report can't be printed while the stuck test holds stderr's lock: the watchdog thread
    /// aborts the process once the grace period has passed instead of waiting for it.
    #[test]
    fn a_stuck_test_holding_stderr_still_ends_its_binary() {
        let grace = Duration::from_millis(500);
        let c = run_stuck_child("watchdog::tests::stuck_child_holding_stderr", &[(GRACE_ENV, "500")], child_deadline());
        let (ran, stderr) = (c.ran, &c.stderr);
        let status = c.status.unwrap_or_else(|| panic!("the stuck test still ran after {ran:?}: {stderr}"));
        assert!(!status.success(), "{status:?}");
        assert_ne!(status.code(), Some(WATCHDOG_EXIT_CODE), "it can't have printed and exited: {stderr}");
        assert!(ran >= grace, "aborted after the grace period, not before: {ran:?}");
        #[cfg(unix)]
        if c.no_core {
            use std::os::unix::process::ExitStatusExt as _;
            assert!(!status.core_dumped(), "the abort left a core dump: {status:?}");
        }
    }
}
