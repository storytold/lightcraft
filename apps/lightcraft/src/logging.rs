//! The desktop app's logger: `log::` records go to standard error and to a log file.
//!
//! It replaces the stderr-only `StderrLog` that `main.rs` used to install. A launch from a
//! desktop menu or the Dock has no terminal, so the file is what a bug report can attach:
//! `<settings folder>/logs/lightcraft.log`, next to `ui.json` (`config_dir` in `main.rs`; Linux
//! `$XDG_CONFIG_HOME/lightcraft/logs`, by default `~/.config/lightcraft/logs`). Never in the
//! library folder. Each start moves the previous log to `lightcraft.1.log` (and that one to `.2`),
//! so the log of a run that crashed survives the next launch. Runs with `LIGHTCRAFT_NO_PREFS`
//! (tests, scripts) log to standard error only, so they don't rotate away the user's own logs.
//!
//! Levels ([`filter_spec`]): `info` for LightCraft's own crates, `warn` for everything else (wgpu
//! and naga are chatty). `LIGHTCRAFT_LOG` keeps the meaning it had with the old logger: `info` or
//! `debug` lowers LightCraft's own crates to that level, any other value means warnings and errors
//! only. Without it, `RUST_LOG` replaces the default with env_logger-style directives: `debug`,
//! `warn,lightcraft_pipeline=trace`, `wgpu_core=info`. A directive ending in `*` matches every
//! target that starts with it (`lightcraft*=debug`).
//!
//! Records logged before the settings folder is known are kept (up to [`MAX_PENDING`]) and
//! written once the file is attached. Writing never panics: a file that can't be created or
//! written leaves standard error as the only sink. Panics that escape everything are added to the
//! file by [`AppLogger::record_panic`] (the default hook already printed them on standard error).
//!
//! This is the desktop app's logger only; `lightcraft-cli` doesn't share a process with it.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError, TryLockError};
use std::time::SystemTime;

use log::LevelFilter;

/// The current log file's name inside the log folder.
pub const LOG_FILE: &str = "lightcraft.log";
/// How many previous logs are kept (`lightcraft.1.log` … `lightcraft.<KEEP>.log`).
pub const KEEP: usize = 2;
/// The log file stops growing past this size (a runaway warning can't fill the disk).
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Records kept in memory until the log file is attached.
pub const MAX_PENDING: usize = 512;
/// The built-in filter when neither `LIGHTCRAFT_LOG` nor `RUST_LOG` says otherwise.
pub const DEFAULT_FILTER: &str = "warn,lightcraft*=info";

/// The filter directives for this run from `LIGHTCRAFT_LOG` and `RUST_LOG` (blank = unset).
///
/// `LIGHTCRAFT_LOG` wins when set and means what it did for the old stderr logger: `info` or
/// `debug` for LightCraft's own crates (warnings and errors from everything else), any other value
/// warnings and errors only. Else `RUST_LOG` as env_logger directives, else [`DEFAULT_FILTER`].
pub fn filter_spec(lightcraft_log: Option<&str>, rust_log: Option<&str>) -> String {
    let set = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned);
    if let Some(level) = set(lightcraft_log) {
        return match level.as_str() {
            "debug" => "warn,lightcraft*=debug".to_owned(),
            "info" => "warn,lightcraft*=info".to_owned(),
            _ => "warn".to_owned(),
        };
    }
    set(rust_log).unwrap_or_else(|| DEFAULT_FILTER.to_owned())
}

/// Per-target level filter parsed from env_logger-style directives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    default: LevelFilter,
    directives: Vec<(String, LevelFilter)>,
}

impl Filter {
    /// Parse `spec`; unknown levels and empty parts are skipped. Without a bare level, targets no
    /// directive names log errors only.
    pub fn parse(spec: &str) -> Filter {
        let mut f = Filter { default: LevelFilter::Error, directives: Vec::new() };
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part.split_once('=') {
                Some((name, level)) => {
                    let name = name.trim();
                    if let (false, Some(level)) = (name.is_empty(), parse_level(level)) {
                        f.directives.push((name.to_owned(), level));
                    }
                }
                None => match parse_level(part) {
                    Some(level) => f.default = level,
                    // A bare target name: everything from it (env_logger does the same).
                    None => f.directives.push((part.to_owned(), LevelFilter::Trace)),
                },
            }
        }
        f
    }

    /// The level that applies to `target` (the most specific matching directive wins).
    pub fn level_for(&self, target: &str) -> LevelFilter {
        self.directives.iter().filter(|(name, _)| matches(name, target)).max_by_key(|(name, _)| name.len()).map_or(self.default, |(_, level)| *level)
    }

    /// The most verbose level any target can reach (for `log::set_max_level`).
    pub fn max(&self) -> LevelFilter {
        self.directives.iter().map(|(_, level)| *level).fold(self.default, Ord::max)
    }
}

fn parse_level(s: &str) -> Option<LevelFilter> {
    s.trim().parse().ok()
}

/// `name` matches `target` itself and its submodules (`a` matches `a` and `a::b`, not `ab`);
/// `name*` matches every target starting with `name`.
fn matches(name: &str, target: &str) -> bool {
    match name.strip_suffix('*') {
        Some(prefix) => target.starts_with(prefix),
        None => target.strip_prefix(name).is_some_and(|rest| rest.is_empty() || rest.starts_with("::")),
    }
}

/// `2026-10-08T07:59:17.728Z` (UTC, milliseconds). A clock before 1970 reads as the epoch.
/// A clock far past chrono's year range still converts (the calendar repeats every 400 years), so
/// logging never panics and never silently resets to the epoch for a convertible instant.
pub fn timestamp(t: SystemTime) -> String {
    let since = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    // Pre-1970 is clamped to the epoch by `unwrap_or_default` above. `as_secs` is `u64`; a value
    // past `i64::MAX` seconds saturates rather than falling back to the epoch.
    let secs = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
    let (y, t) = utc_parts(secs, since.subsec_nanos());
    // `{:04}` rather than `%Y`, which writes a `+` before years past 9999.
    format!("{y:04}-{}", t.format("%m-%dT%H:%M:%S%.3fZ"))
}

/// Unix seconds (+ nanos) → (proleptic Gregorian year, the same wall-clock instant moved into
/// chrono's range by whole 400-year cycles). The calendar repeats every 400 years (146_097 days),
/// so month, day and time of day come from chrono and only the year is carried, which keeps every
/// `i64` exact. Replaces the old hand-rolled `civil_from_days`; chrono is the only calendar logic.
fn utc_parts(secs: i64, nanos: u32) -> (i64, chrono::NaiveDateTime) {
    use chrono::Datelike;
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (cycles, rest) = (days.div_euclid(146_097), days.rem_euclid(146_097));
    // `rest + 719_163` (1970-01-01 is CE day 719_163) is always a valid CE day, and `sod` < 86_400.
    let date = i32::try_from(rest + 719_163).ok().and_then(chrono::NaiveDate::from_num_days_from_ce_opt);
    let time = u32::try_from(sod).ok().and_then(|s| chrono::NaiveTime::from_num_seconds_from_midnight_opt(s, nanos.min(999_999_999)));
    match (date, time) {
        (Some(d), Some(t)) => {
            let y = i128::from(d.year()) + i128::from(cycles) * 400;
            (i64::try_from(y).unwrap_or(if y < 0 { i64::MIN } else { i64::MAX }), d.and_time(t))
        }
        _ => (1970, chrono::NaiveDateTime::default()), // unreachable
    }
}

fn numbered(dir: &Path, n: usize) -> PathBuf {
    if n == 0 { dir.join(LOG_FILE) } else { dir.join(format!("lightcraft.{n}.log")) }
}

/// Shift the previous logs up one (`.log` → `.1.log` → … → `.<KEEP>.log`, the oldest dropped)
/// and create a fresh, empty log file in `dir` (created if missing).
pub fn rotate(dir: &Path) -> std::io::Result<(PathBuf, File)> {
    std::fs::create_dir_all(dir)?;
    for n in (1..=KEEP).rev() {
        match std::fs::rename(numbered(dir, n - 1), numbered(dir, n)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    let path = numbered(dir, 0);
    let file = File::create(&path)?;
    Ok((path, file))
}

/// One formatted log line: `<timestamp> <LEVEL> [<thread>] <target>: <message>\n`.
pub fn format_line(ts: &str, level: log::Level, thread: &str, target: &str, message: &str) -> String {
    let message = message.strip_suffix('\n').unwrap_or(message);
    format!("{ts} {level:<5} [{thread}] {target}: {message}\n")
}

/// Where formatted lines go besides standard error: kept in memory until a file is attached,
/// then appended to it until it reaches its size cap.
pub struct Sink {
    file: Option<File>,
    pending: Vec<String>,
    dropped: usize,
    written: u64,
    max_bytes: u64,
    capped: bool,
}

impl Sink {
    pub fn new(max_bytes: u64) -> Sink {
        Sink { file: None, pending: Vec::new(), dropped: 0, written: 0, max_bytes, capped: false }
    }

    /// Append `line` to the file, or keep it until [`Sink::attach`].
    pub fn write(&mut self, line: &str) {
        if self.file.is_none() {
            if self.capped {
                // No file is coming ([`Sink::no_file`]): standard error had the line.
            } else if self.pending.len() < MAX_PENDING {
                self.pending.push(line.to_owned());
            } else {
                self.dropped = self.dropped.saturating_add(1);
            }
            return;
        }
        self.append(line);
    }

    /// Start writing to `file` (a fresh one: the size cap starts over), first the lines kept so far.
    pub fn attach(&mut self, file: File) {
        self.file = Some(file);
        self.written = 0;
        self.capped = false;
        for line in std::mem::take(&mut self.pending) {
            self.append(&line);
        }
        if self.dropped > 0 {
            let note = format!("{} earlier log lines were dropped before the log file was opened\n", self.dropped);
            self.dropped = 0;
            self.append(&note);
        }
    }

    /// There will be no log file: forget the lines kept for it and keep no more.
    pub fn no_file(&mut self) {
        self.file = None;
        self.pending = Vec::new();
        self.dropped = 0;
        self.capped = true;
    }

    fn append(&mut self, line: &str) {
        if self.capped {
            return;
        }
        let Some(file) = self.file.as_mut() else { return };
        let len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        let text = if self.written.saturating_add(len) > self.max_bytes {
            self.capped = true;
            format!("log file reached {} bytes; later records go to standard error only\n", self.max_bytes)
        } else {
            self.written = self.written.saturating_add(len);
            line.to_owned()
        };
        // A full disk or a vanished file must not take the app down: drop the file sink.
        if file.write_all(text.as_bytes()).is_err() {
            self.file = None;
            self.capped = true;
        }
    }
}

/// The installed logger: a [`Filter`], standard error and a [`Sink`].
pub struct AppLogger {
    filter: Filter,
    stderr: bool,
    sink: Mutex<Sink>,
}

impl AppLogger {
    pub fn new(filter: Filter, stderr: bool) -> AppLogger {
        AppLogger { filter, stderr, sink: Mutex::new(Sink::new(MAX_FILE_BYTES)) }
    }

    /// Rotate the logs in `dir` and send records to the fresh file; returns its path. On an error
    /// standard error stays the only sink.
    pub fn attach_dir(&self, dir: &Path) -> Result<PathBuf, String> {
        // A thread that panicked while logging leaves the sink usable: take it back.
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        match rotate(dir) {
            Ok((path, file)) => {
                sink.attach(file);
                Ok(path)
            }
            Err(e) => {
                sink.no_file();
                Err(format!("{}: {e}", dir.display()))
            }
        }
    }

    /// No log file for this run (`LIGHTCRAFT_NO_PREFS`): log to standard error only.
    pub fn no_file(&self) {
        self.sink.lock().unwrap_or_else(PoisonError::into_inner).no_file();
    }

    /// Add a panic report to the log file only (the default panic hook has printed it on standard
    /// error), whatever the filter. Skipped when the sink is busy: the panic may have happened on
    /// this very thread while it held the lock, and waiting would hang the app.
    pub fn record_panic(&self, thread: &str, report: &str) {
        let line = format_line(&timestamp(SystemTime::now()), log::Level::Error, thread, "lightcraft", report);
        let mut sink = match self.sink.try_lock() {
            Ok(sink) => sink,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        sink.write(&line);
    }
}

impl log::Log for AppLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.filter.level_for(metadata.target())
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Formatted before the sink is locked, so a message that logs while it is formatted can't deadlock.
        let thread = std::thread::current();
        let line =
            format_line(&timestamp(SystemTime::now()), record.level(), thread.name().unwrap_or("?"), record.target(), &record.args().to_string());
        if self.stderr {
            // No terminal (a Windows GUI build, a closed pipe) is not an error worth reporting.
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
        self.sink.lock().unwrap_or_else(PoisonError::into_inner).write(&line);
    }

    fn flush(&self) {}
}

/// Install the logger (filter from [`filter_spec`]); `None` when another logger was installed
/// first. Call [`AppLogger::attach_dir`] (or [`AppLogger::no_file`]) once the arguments are parsed.
pub fn install() -> Option<&'static AppLogger> {
    static LOGGER: OnceLock<AppLogger> = OnceLock::new();
    let var = |name| std::env::var(name).ok();
    let filter = Filter::parse(&filter_spec(var("LIGHTCRAFT_LOG").as_deref(), var("RUST_LOG").as_deref()));
    let max = filter.max();
    let logger = LOGGER.get_or_init(|| AppLogger::new(filter, true));
    log::set_logger(logger).ok()?;
    log::set_max_level(max);
    Some(logger)
}

/// Chain a panic hook after the current one (the engine guard's: standard error plus
/// `lightcraft-panics.log` in the temp folder) that also puts the panic in the log file.
pub fn record_panics(logger: &'static AppLogger) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let thread = std::thread::current();
        let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        let report = format!("panic{at}: {}", lightcraft_engine::guard::panic_message(info.payload()));
        logger.record_panic(thread.name().unwrap_or("?"), &report);
    }));
}

#[cfg(test)]
mod tests;
