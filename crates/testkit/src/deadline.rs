//! Deadlines for test waits. Stable libtest has no per-test timeout, so a wait that never ends
//! hangs its test binary with no message (and keeps the app's workers alive). These helpers give
//! every wait a limit and fail the test with what it was waiting for.
//!
//! Every limit is stretched by [`time_scale`] (`LIGHTCRAFT_TIME_SCALE`), so a slow or shared
//! machine can give all of them more time at once.

use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// The environment variable that stretches (or shrinks) every test time limit: a factor, `1` by
/// default. `LIGHTCRAFT_TIME_SCALE=3` gives each wait three times as long.
pub const TIME_SCALE_ENV: &str = "LIGHTCRAFT_TIME_SCALE";

/// The smallest factor [`time_scale`] returns: shrinking limits further only makes tests flaky.
pub const MIN_TIME_SCALE: f64 = 0.1;
/// The largest factor [`time_scale`] returns: past it a limit is as good as none.
pub const MAX_TIME_SCALE: f64 = 1000.0;

/// The longest a poll of [`wait_until`] sleeps between two checks of its condition.
const MAX_POLL: Duration = Duration::from_millis(50);

/// The factor every test time limit is multiplied by: `LIGHTCRAFT_TIME_SCALE`, read once per
/// process. Unset or empty means `1`. A number is clamped to [`MIN_TIME_SCALE`]..=[`MAX_TIME_SCALE`];
/// anything else (not a number, NaN, infinite, zero or negative) is ignored with a note on stderr,
/// and the factor stays `1`.
pub fn time_scale() -> f64 {
    static SCALE: OnceLock<f64> = OnceLock::new();
    *SCALE.get_or_init(|| {
        let value = std::env::var(TIME_SCALE_ENV).ok();
        let (scale, note) = parse_time_scale(value.as_deref());
        if let Some(note) = note {
            crate::note(&format!("{TIME_SCALE_ENV}: {note}"));
        }
        scale
    })
}

/// `d` stretched by [`time_scale`]. Saturates at [`Duration::MAX`] instead of overflowing.
pub fn scaled(d: Duration) -> Duration {
    scale_by(d, time_scale())
}

/// `d × scale`, saturating; a factor that isn't a positive number leaves `d` as it is.
fn scale_by(d: Duration, scale: f64) -> Duration {
    if !scale.is_finite() || scale <= 0.0 {
        return d;
    }
    Duration::try_from_secs_f64(d.as_secs_f64() * scale).unwrap_or(Duration::MAX)
}

/// The factor `LIGHTCRAFT_TIME_SCALE=value` means, and a note when the value was ignored or
/// clamped.
fn parse_time_scale(value: Option<&str>) -> (f64, Option<String>) {
    let Some(text) = value.map(str::trim).filter(|v| !v.is_empty()) else { return (1.0, None) };
    match text.parse::<f64>() {
        Ok(s) if s.is_finite() && s > 0.0 => {
            let clamped = s.clamp(MIN_TIME_SCALE, MAX_TIME_SCALE);
            let note = (clamped != s).then(|| format!("{text} is outside {MIN_TIME_SCALE}..={MAX_TIME_SCALE}; using {clamped}"));
            (clamped, note)
        }
        _ => (1.0, Some(format!("{text:?} is not a positive number; using 1"))),
    }
}

/// Wait until `cond` holds, checking it with a growing pause (1 ms up to 50 ms). Fails the test
/// (panics) with `"{what}" not reached after {elapsed}` once [`scaled`]`(timeout)` has passed.
/// `what` names the state waited for, as the failure should read: `"the scan's first row"`.
///
/// Also bounds waits on other threads (a gated worker job, an injected probe): the panic ends
/// that thread's job instead of leaving it spinning after the test is gone.
#[allow(clippy::panic)] // failing the test with what it waited for is the point
#[track_caller]
pub fn wait_until(timeout: Duration, what: &str, mut cond: impl FnMut() -> bool) {
    let limit = scaled(timeout);
    let start = Instant::now();
    let mut pause = Duration::from_millis(1);
    loop {
        if cond() {
            return;
        }
        let elapsed = start.elapsed();
        if elapsed >= limit {
            panic!("\"{what}\" not reached after {elapsed:.2?} (limit {limit:?}: {timeout:?} × {TIME_SCALE_ENV} {})", time_scale());
        }
        std::thread::sleep(pause.min(limit.saturating_sub(elapsed)));
        pause = (pause * 2).min(MAX_POLL);
    }
}

/// Receive the next message from `rx`, waiting at most [`scaled`]`(timeout)`. Fails the test
/// (panics) with `"{what}" not received after {elapsed}` when it doesn't come, or when every
/// sender is gone first.
#[allow(clippy::panic)] // failing the test with what it waited for is the point
#[track_caller]
pub fn recv_within<T>(rx: &Receiver<T>, timeout: Duration, what: &str) -> T {
    let limit = scaled(timeout);
    let start = Instant::now();
    match rx.recv_timeout(limit) {
        Ok(v) => v,
        Err(RecvTimeoutError::Timeout) => {
            panic!("\"{what}\" not received after {:.2?} (limit {limit:?}: {timeout:?} × {TIME_SCALE_ENV} {})", start.elapsed(), time_scale())
        }
        Err(RecvTimeoutError::Disconnected) => panic!("\"{what}\" never sent: the sender hung up after {:.2?}", start.elapsed()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn the_time_scale_reads_a_positive_factor_and_ignores_the_rest() {
        assert_eq!(parse_time_scale(None), (1.0, None));
        assert_eq!(parse_time_scale(Some("")), (1.0, None));
        assert_eq!(parse_time_scale(Some("  ")), (1.0, None));
        assert_eq!(parse_time_scale(Some("2.5")), (2.5, None));
        assert_eq!(parse_time_scale(Some(" 3 ")), (3.0, None));
        for bad in ["0", "-2", "NaN", "nan", "inf", "-inf", "fast", "1e400", "0x10"] {
            let (scale, note) = parse_time_scale(Some(bad));
            assert_eq!(scale, 1.0, "{bad}");
            assert!(note.is_some_and(|n| n.contains("not a positive number")), "{bad}");
        }
        let (scale, note) = parse_time_scale(Some("1e9"));
        assert_eq!(scale, MAX_TIME_SCALE);
        assert!(note.is_some_and(|n| n.contains("outside")));
        let (scale, note) = parse_time_scale(Some("0.0001"));
        assert_eq!(scale, MIN_TIME_SCALE);
        assert!(note.is_some());
    }

    #[test]
    fn scaling_saturates_instead_of_overflowing() {
        assert_eq!(scale_by(Duration::from_secs(10), 2.0), Duration::from_secs(20));
        assert_eq!(scale_by(Duration::from_secs(10), 0.5), Duration::from_secs(5));
        assert_eq!(scale_by(Duration::MAX, MAX_TIME_SCALE), Duration::MAX);
        assert_eq!(scale_by(Duration::from_secs(10), f64::NAN), Duration::from_secs(10));
        assert_eq!(scale_by(Duration::from_secs(10), -1.0), Duration::from_secs(10));
        assert_eq!(scale_by(Duration::ZERO, 3.0), Duration::ZERO);
    }

    #[test]
    fn wait_until_returns_once_the_condition_holds() {
        let checks = AtomicUsize::new(0);
        let start = Instant::now();
        wait_until(Duration::from_secs(30), "the third check", || checks.fetch_add(1, Ordering::SeqCst) >= 2);
        assert_eq!(checks.load(Ordering::SeqCst), 3);
        assert!(start.elapsed() < Duration::from_secs(10), "it doesn't wait out the limit");
    }

    #[test]
    fn wait_until_sees_a_change_made_by_another_thread() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f = flag.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            f.store(true, Ordering::SeqCst);
        });
        wait_until(Duration::from_secs(30), "the flag", || flag.load(Ordering::SeqCst));
        t.join().unwrap();
    }

    #[test]
    #[should_panic(expected = "\"the gate opens\" not reached after")]
    fn wait_until_fails_the_test_with_what_it_waited_for() {
        wait_until(Duration::from_millis(20), "the gate opens", || false);
    }

    #[test]
    fn recv_within_returns_the_message() {
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tx.send(7).unwrap();
        });
        assert_eq!(recv_within(&rx, Duration::from_secs(30), "the job's result"), 7);
        t.join().unwrap();
    }

    #[test]
    #[should_panic(expected = "\"the job starts\" not received after")]
    fn recv_within_fails_the_test_when_nothing_comes() {
        let (_tx, rx) = std::sync::mpsc::channel::<()>();
        recv_within(&rx, Duration::from_millis(20), "the job starts");
    }

    #[test]
    #[should_panic(expected = "\"the job starts\" never sent: the sender hung up")]
    fn recv_within_fails_the_test_when_the_sender_is_gone() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        drop(tx);
        recv_within(&rx, Duration::from_secs(30), "the job starts");
    }
}
