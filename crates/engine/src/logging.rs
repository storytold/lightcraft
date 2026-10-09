//! `log` records on stderr for the native desktop app and CLI (#168). The web build installs
//! eframe's `WebLogger` instead.
//!
//! Warnings and errors are shown by default. `LIGHTCRAFT_LOG` or `RUST_LOG` picks another level
//! (`off`, `error`, `warn`, `info`, `debug`, `trace`); `LIGHTCRAFT_LOG` wins when both are set.
//! `RUST_LOG` also accepts per-target directives (`lightcraft=debug,wgpu=warn`): a bare level
//! applies to everything, and the most verbose `lightcraft…=level` directive sets LightCraft's.
//! Below warnings only LightCraft's own records are shown, so `info` and `debug` aren't drowned
//! out by the GPU and windowing libraries.

use log::{Level, LevelFilter, Metadata, Record};

/// Records shown at a level, written to stderr as `<prefix>: LEVEL target: message`.
pub struct StderrLog {
    prefix: &'static str,
    level: LevelFilter,
}

impl StderrLog {
    pub fn new(prefix: &'static str, level: LevelFilter) -> Self {
        Self { prefix, level }
    }
}

impl log::Log for StderrLog {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= self.level && (m.level() <= Level::Warn || m.target().starts_with("lightcraft"))
    }
    fn log(&self, r: &Record) {
        if self.enabled(r.metadata()) {
            eprintln!("{}: {} {}: {}", self.prefix, r.level(), r.target(), r.args());
        }
    }
    fn flush(&self) {}
}

fn parse_level(s: &str) -> Option<LevelFilter> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

/// The level that `LIGHTCRAFT_LOG` / `RUST_LOG` ask for; warnings when neither names one.
pub fn level_from(lightcraft_log: Option<&str>, rust_log: Option<&str>) -> LevelFilter {
    if let Some(l) = lightcraft_log.and_then(parse_level) {
        return l;
    }
    let Some(rust_log) = rust_log else { return LevelFilter::Warn };
    let mut bare = None;
    let mut ours: Option<LevelFilter> = None;
    for directive in rust_log.split(',') {
        match directive.split_once('=') {
            None => bare = parse_level(directive).or(bare),
            Some((target, level)) if target.trim().starts_with("lightcraft") => {
                if let Some(l) = parse_level(level) {
                    ours = Some(ours.map_or(l, |o| o.max(l)));
                }
            }
            Some(_) => {}
        }
    }
    ours.or(bare).unwrap_or(LevelFilter::Warn)
}

/// Install the stderr logger at the level the environment asks for. Does nothing when a logger is
/// already installed.
pub fn install(prefix: &'static str) {
    let level = level_from(std::env::var("LIGHTCRAFT_LOG").ok().as_deref(), std::env::var("RUST_LOG").ok().as_deref());
    static LOGGER: std::sync::OnceLock<StderrLog> = std::sync::OnceLock::new();
    if log::set_logger(LOGGER.get_or_init(|| StderrLog::new(prefix, level))).is_ok() {
        log::set_max_level(level);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Log;

    #[test]
    fn warnings_by_default_and_either_variable_picks_the_level() {
        assert_eq!(level_from(None, None), LevelFilter::Warn);
        assert_eq!(level_from(None, Some("")), LevelFilter::Warn);
        assert_eq!(level_from(None, Some("warn")), LevelFilter::Warn);
        assert_eq!(level_from(None, Some("TRACE")), LevelFilter::Trace);
        assert_eq!(level_from(None, Some("off")), LevelFilter::Off);
        assert_eq!(level_from(Some("info"), None), LevelFilter::Info);
        assert_eq!(level_from(Some("error"), Some("debug")), LevelFilter::Error, "LIGHTCRAFT_LOG wins");
        assert_eq!(level_from(Some("nonsense"), Some("debug")), LevelFilter::Debug, "an unknown LIGHTCRAFT_LOG is ignored");
    }

    #[test]
    fn rust_log_directives_set_lightcraft_s_level() {
        assert_eq!(level_from(None, Some("lightcraft=debug,wgpu=warn")), LevelFilter::Debug);
        assert_eq!(level_from(None, Some("wgpu_core=trace")), LevelFilter::Warn, "other crates' directives don't apply");
        assert_eq!(level_from(None, Some("error,dac_gpu=info")), LevelFilter::Info);
        assert_eq!(level_from(None, Some("dac_gpu=info,dac_engine=trace")), LevelFilter::Trace);
        assert_eq!(level_from(None, Some("info,naga=off")), LevelFilter::Info);
    }

    #[test]
    fn only_lightcraft_records_below_warnings() {
        let log = StderrLog::new("test", LevelFilter::Debug);
        let meta = |level, target| Metadata::builder().level(level).target(target).build();
        assert!(log.enabled(&meta(Level::Warn, "wgpu_core::device")));
        assert!(log.enabled(&meta(Level::Debug, "dac_gpu::ctx")));
        assert!(!log.enabled(&meta(Level::Info, "wgpu_core::device")));
        assert!(!log.enabled(&meta(Level::Trace, "dac_gpu::ctx")));
        let quiet = StderrLog::new("test", LevelFilter::Off);
        assert!(!quiet.enabled(&meta(Level::Error, "dac_engine")));
    }
}
