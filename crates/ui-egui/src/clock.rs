/// A monotonic clock for the app's own timers.
///
/// `web_time::Instant` is backed by the browser's `performance.now()` on WASM and by
/// `std::time::Instant` natively: it never jumps with the wall clock and has no `Date.now()`
/// zero-origin problem. A headless host may supply its simulated time instead.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AppClock {
    start: web_time::Instant,
    pub(crate) simulated: Option<f64>,
}

impl AppClock {
    /// Start the clock at the current monotonic instant with no simulated time.
    pub(crate) fn new() -> Self {
        Self { start: web_time::Instant::now(), simulated: None }
    }

    /// Seconds on the clock: the supplied simulated time when set, otherwise time since `new`.
    pub(crate) fn now(&self) -> f64 {
        self.simulated.unwrap_or_else(|| self.start.elapsed().as_secs_f64())
    }
}

/// Monotonic milliseconds since the first call, for durations and deadlines.
pub fn monotonic_ms() -> f64 {
    static START: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(web_time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// Wall-clock milliseconds since the Unix epoch, only for names that must be unique in the world
/// (`library-restored-<ms>`), never for timing or deadlines.
pub fn now_ms() -> f64 {
    use web_time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}
