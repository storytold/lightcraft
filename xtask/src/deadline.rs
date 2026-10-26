//! Time limits for the test processes `cargo xtask` waits on (#3060).
//!
//! Stable libtest has no per-test timeout, so a test that hangs (a GPU device that never answers,
//! a deadlock) used to hang `ci` and `cargo xtask test` with it, and with them the machine-wide CI
//! slot ([`crate::ci_lock`]) that every other run on the machine waits for. Here the process that
//! starts the tests bounds them: each test binary gets [`TEST_BINARY_TIMEOUT_ENV`] minutes
//! ([`DEFAULT_TEST_BINARY_TIMEOUT_MIN`] by default; `0` or `off` for no limit).
//!
//! - A test binary xtask starts itself ([`run_test_binary`]: `cargo xtask test`, `ci --quick`) is
//!   timed from its start.
//! - A `cargo test` run ([`run_cargo_tests`]: the full `ci`'s test and heif steps, doctests) is timed
//!   per test binary, from the `Running` (or `Doc-tests`) line cargo prints as it starts each one;
//!   the build before the first one isn't bounded. Under `cargo nextest run` (no such lines) nextest's
//!   own `slow-timeout` in `.config/nextest.toml` ends a hung test instead.
//!
//! The output streams through as it arrives, and is read on the way for libtest's lines: `running N
//! tests`, `test NAME ... ok` as each test finishes, and `test NAME has been running for over 60
//! seconds`. On timeout the binary and every process it started are killed (on Unix: frozen with
//! SIGSTOP so nothing more forks, then SIGKILL; on Windows `taskkill /T /F`), and the report names the
//! binary, the tests still running (those warned about and not finished, or the one a single-threaded
//! run printed `test NAME ... ` for) and the tests that never reported a result (from `--list`).
//!
//! The child stays in xtask's process group rather than one of its own, so Ctrl-C in the terminal
//! still reaches the test binaries as before; that's why the kill walks the process tree (`ps`)
//! rather than signalling a group.

use std::collections::BTreeSet;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

/// The limit, in minutes, on each test binary's run.
pub const TEST_BINARY_TIMEOUT_ENV: &str = "LIGHTCRAFT_TEST_BINARY_TIMEOUT";
/// The default of [`TEST_BINARY_TIMEOUT_ENV`]: the slowest binary, the ui-egui library tests, takes
/// about 5 minutes here under `ci`'s caps.
pub const DEFAULT_TEST_BINARY_TIMEOUT_MIN: u64 = 30;
/// The longest limit a variable may set (a week); `0` or `off` means none.
const MAX_MINUTES: f64 = 7.0 * 24.0 * 60.0;
/// How often the wait checks the child and the clock.
const TICK: Duration = Duration::from_millis(50);
/// How long, after the child ends, to wait for its output pipes to close (a process it started
/// that outlives it may hold them open).
const DRAIN: Duration = Duration::from_secs(5);
/// How long the `--list` run of a timed-out binary may take (it runs no tests: milliseconds).
const LIST_LIMIT: Duration = Duration::from_secs(15);
/// The longest line of output parsed for test names; longer ones stream through unparsed.
const MAX_LINE: usize = 64 * 1024;
/// How many never-finished tests the timeout report names.
const NAMED: usize = 20;

/// A time limit in minutes from `value` (the variable `var`'s), `default_min` when it's unset or
/// blank. `Ok(None)` is no limit (`0`, `off`, `none`, `never`). Fractions are allowed (`0.5`); a
/// value that isn't a number of minutes from 0 to a week is an error naming the variable.
pub fn parse_minutes(var: &str, value: Option<&str>, default_min: u64) -> Result<Option<Duration>, String> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(Some(Duration::from_secs(default_min.saturating_mul(60))));
    };
    if matches!(v.to_ascii_lowercase().as_str(), "off" | "none" | "never") {
        return Ok(None);
    }
    let bad = || format!("{var}={v}: not a number of minutes (e.g. 30 or 0.5; 0 or off for no limit; at most {MAX_MINUTES} = a week)");
    let minutes: f64 = v.parse().map_err(|_| bad())?;
    if !minutes.is_finite() || minutes < 0.0 || minutes > MAX_MINUTES {
        return Err(bad());
    }
    if minutes <= 0.0 {
        return Ok(None);
    }
    Duration::try_from_secs_f64(minutes * 60.0).map(Some).map_err(|_| bad())
}

/// [`parse_minutes`] for the environment variable `var` (one that isn't Unicode is an error).
pub fn minutes_from_env(var: &str, default_min: u64) -> Result<Option<Duration>, String> {
    let value = match std::env::var(var) {
        Ok(v) => Some(v),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(v)) => return Err(format!("{var}={}: not a number of minutes", v.to_string_lossy())),
    };
    parse_minutes(var, value.as_deref(), default_min)
}

/// The per-binary test limit from [`TEST_BINARY_TIMEOUT_ENV`].
pub fn test_binary_timeout() -> Result<Option<Duration>, String> {
    minutes_from_env(TEST_BINARY_TIMEOUT_ENV, DEFAULT_TEST_BINARY_TIMEOUT_MIN)
}

/// `30 min`, `1.5 min`, `20 s`, `300 ms`: a limit as the reports show it.
pub fn show(limit: Duration) -> String {
    let secs = limit.as_secs_f64();
    let minutes = secs / 60.0;
    if secs >= 60.0 && (minutes * 10.0).fract().abs() < 1e-6 {
        format!("{} min", (minutes * 10.0).round() / 10.0)
    } else if secs >= 1.0 {
        format!("{} s", (secs * 10.0).round() / 10.0)
    } else {
        format!("{} ms", limit.as_millis())
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `line` without ANSI escape sequences (cargo's and libtest's colours).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // CSI: ESC [ parameters… final byte in @..~; anything else: drop the ESC and the next char
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// What a test run's output said so far: the test binary cargo started last and, from libtest's
/// lines, how many tests it runs, which finished and which ran long.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// The test binary cargo started last, as its `Running` / `Doc-tests` line names it.
    pub binary: Option<String>,
    /// That binary's executable (the path in the `Running` line's parentheses).
    pub exe: Option<PathBuf>,
    /// `running N tests`.
    pub total: Option<usize>,
    /// The tests that reported a result (`test NAME ... ok`, `FAILED`, `ignored`).
    pub finished: BTreeSet<String>,
    /// The tests libtest said had been running for over 60 seconds, in that order.
    pub slow: Vec<String>,
    /// The unfinished last line of stdout: `test NAME ... ` while a single-threaded run runs NAME.
    pub pending: String,
}

impl Progress {
    /// Read one whole line of output. True when it is cargo starting a test binary.
    pub fn line(&mut self, raw: &str) -> bool {
        let clean = strip_ansi(raw);
        let line = clean.trim_end();
        let text = line.trim_start();
        // cargo right-aligns its status word, so its lines start with spaces
        let cargo_status = line.starts_with(' ');
        if let Some(rest) = text.strip_prefix("Running ").filter(|_| cargo_status) {
            let exe = rest.strip_suffix(')').and_then(|r| r.rsplit_once(" (")).map(|(_, path)| PathBuf::from(path));
            self.start_binary(rest.to_owned(), exe);
            return true;
        }
        if let Some(rest) = text.strip_prefix("Doc-tests ").filter(|_| cargo_status) {
            self.start_binary(format!("doctests of {rest}"), None);
            return true;
        }
        if let Some(rest) = text.strip_prefix("running ") {
            let mut words = rest.split_whitespace();
            if let (Some(n), Some("test" | "tests"), None) = (words.next().and_then(|n| n.parse().ok()), words.next(), words.next()) {
                self.reset();
                self.total = Some(n);
            }
            return false;
        }
        if let Some(rest) = text.strip_prefix("test ") {
            if let Some((name, _result)) = rest.split_once(" ... ") {
                self.finished.insert(name.to_owned());
            } else if let Some((name, _)) = rest.split_once(" has been running for over ")
                && !self.slow.iter().any(|s| s == name)
            {
                self.slow.push(name.to_owned());
            }
        }
        false
    }

    /// Cargo started a test binary. The test lines stay until its `running N tests`: cargo's line
    /// comes on stderr and the binary's on stdout, read by two threads, so the first lines of the
    /// new binary can be read before cargo's.
    fn start_binary(&mut self, name: String, exe: Option<PathBuf>) {
        self.binary = Some(name);
        self.exe = exe;
    }

    fn reset(&mut self) {
        self.total = None;
        self.finished.clear();
        self.slow.clear();
        self.pending.clear();
    }

    /// The tests still running: warned about as slow and not finished, then the one a
    /// single-threaded run started last.
    pub fn running(&self) -> Vec<String> {
        let mut running: Vec<String> = self.slow.iter().filter(|s| !self.finished.contains(*s)).cloned().collect();
        let pending = strip_ansi(&self.pending);
        let started = pending.trim().strip_prefix("test ").and_then(|p| p.strip_suffix(" ...")).filter(|n| !n.contains(" ... "));
        if let Some(name) = started.filter(|n| !self.finished.contains(*n) && !running.iter().any(|r| r == n)) {
            running.push(name.to_owned());
        }
        running
    }
}

/// When a run's time limit starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    /// When the process starts: a test binary run directly.
    Start,
    /// At each test binary cargo starts (its `Running` / `Doc-tests` line): the limit is per binary,
    /// and the build before the first isn't bounded.
    EachBinary,
}

/// How a watched run ended.
#[derive(Debug)]
pub enum Ended {
    Exited(ExitStatus),
    /// Out of time: killed, with what its output said by then.
    TimedOut(Progress),
}

struct Watch {
    progress: Progress,
    /// When the current limit started, `None` while it doesn't run.
    since: Option<Instant>,
}

/// Run `cmd` with its output streamed through and read for libtest's lines, until it exits or
/// `limit` runs out (timed as `clock` says); then kill it and every process it started.
pub fn watch(mut cmd: Command, limit: Duration, clock: Clock) -> Result<Ended, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("failed to start: {e}"))?;
    let state = Arc::new(Mutex::new(Watch { progress: Progress::default(), since: (clock == Clock::Start).then(Instant::now) }));
    let (done_tx, done_rx) = mpsc::channel();
    let mut readers = 0;
    let pipes: [(Option<Box<dyn Read + Send>>, bool); 2] = [
        (child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), true),
        (child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>), false),
    ];
    for (pipe, is_stdout) in pipes {
        let Some(pipe) = pipe else { continue };
        let (state, done) = (Arc::clone(&state), done_tx.clone());
        let spawned = std::thread::Builder::new().name("xtask-output".into()).spawn(move || {
            pump(pipe, is_stdout, clock, &state);
            let _ = done.send(());
        });
        if let Err(e) = spawned {
            kill_tree(&mut child);
            return Err(format!("could not start a thread to read its output: {e}"));
        }
        readers += 1;
    }
    drop(done_tx);
    let drain = |readers: usize| {
        let until = Instant::now() + DRAIN;
        for _ in 0..readers {
            if done_rx.recv_timeout(until.saturating_duration_since(Instant::now())).is_err() {
                break;
            }
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                drain(readers);
                return Ok(Ended::Exited(status));
            }
            Ok(None) => {}
            Err(e) => {
                kill_tree(&mut child);
                return Err(format!("waiting for it failed: {e}"));
            }
        }
        if lock(&state).since.is_some_and(|since| since.elapsed() >= limit) {
            kill_tree(&mut child);
            drain(readers);
            return Ok(Ended::TimedOut(lock(&state).progress.clone()));
        }
        std::thread::sleep(TICK);
    }
}

/// Copy `pipe` to xtask's stdout or stderr as it arrives, feeding whole lines to the progress.
fn pump(mut pipe: Box<dyn Read + Send>, is_stdout: bool, clock: Clock, state: &Mutex<Watch>) {
    let mut buf = [0u8; 8192];
    let mut line: Vec<u8> = Vec::new();
    let mut overlong = false;
    loop {
        let n = match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let chunk = buf.get(..n).unwrap_or_default();
        if is_stdout {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(chunk).and_then(|()| out.flush());
        } else {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(chunk).and_then(|()| err.flush());
        }
        let mut watch = lock(state);
        for piece in chunk.split_inclusive(|b| *b == b'\n') {
            if !overlong {
                line.extend_from_slice(piece);
            }
            if line.len() > MAX_LINE {
                line.clear();
                overlong = true;
            }
            if piece.ends_with(b"\n") {
                if !overlong && watch.progress.line(&String::from_utf8_lossy(&line)) && clock == Clock::EachBinary {
                    watch.since = Some(Instant::now());
                }
                line.clear();
                overlong = false;
            }
        }
        if is_stdout {
            watch.progress.pending = String::from_utf8_lossy(&line).into_owned();
        }
    }
}

/// Kill `child` and every process it started, and reap it.
pub fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // freeze the tree first so nothing forks while it is collected, then kill it all
        let mut seen: BTreeSet<u32> = BTreeSet::from([child.id()]);
        let mut fresh: Vec<u32> = vec![child.id()];
        for _ in 0..8 {
            signal("STOP", &fresh);
            fresh = descendants(child.id()).into_iter().filter(|p| !seen.contains(p)).collect();
            if fresh.is_empty() {
                break;
            }
            seen.extend(&fresh);
        }
        signal("KILL", &seen.iter().copied().collect::<Vec<_>>());
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill").args(["/PID", &child.id().to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn signal(name: &str, pids: &[u32]) {
    if pids.is_empty() {
        return;
    }
    let _ = Command::new("kill").arg(format!("-{name}")).args(pids.iter().map(u32::to_string)).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// The processes descended from `root`, from `ps`'s pid / parent pid table (empty if `ps` fails).
#[cfg(unix)]
fn descendants(root: u32) -> Vec<u32> {
    let Ok(out) = Command::new("ps").args(["-A", "-o", "pid=", "-o", "ppid="]).stderr(Stdio::null()).output() else {
        return Vec::new();
    };
    descendants_in(&String::from_utf8_lossy(&out.stdout), root)
}

/// The descendants of `root` in a `pid ppid` table, one process per line.
#[cfg_attr(not(unix), allow(dead_code))]
fn descendants_in(table: &str, root: u32) -> Vec<u32> {
    let pairs: Vec<(u32, u32)> = table
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some((w.next()?.parse().ok()?, w.next()?.parse().ok()?))
        })
        .collect();
    let mut found: BTreeSet<u32> = BTreeSet::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, _) in pairs.iter().filter(|(pid, ppid)| *ppid == parent && *pid != root) {
            if found.insert(*pid) {
                frontier.push(*pid);
            }
        }
    }
    found.into_iter().collect()
}

/// A copy of `cmd`: its program, arguments, directory and environment changes.
fn copy_command(cmd: &Command) -> Command {
    let mut c = Command::new(cmd.get_program());
    c.args(cmd.get_args());
    if let Some(dir) = cmd.get_current_dir() {
        c.current_dir(dir);
    }
    for (key, value) in cmd.get_envs() {
        match value {
            Some(v) => c.env(key, v),
            None => c.env_remove(key),
        };
    }
    c
}

/// The tests a test binary would run, from `c` (the binary with its arguments) plus `--list`;
/// `None` when that fails or takes over [`LIST_LIMIT`].
fn list_tests(mut c: Command) -> Option<Vec<String>> {
    c.arg("--list").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = c.spawn().ok()?;
    let Some(mut stdout) = child.stdout.take() else {
        kill_tree(&mut child);
        return None;
    };
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::Builder::new().spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let text = reader.ok().and_then(|_| rx.recv_timeout(LIST_LIMIT).ok());
    let Some(text) = text else {
        kill_tree(&mut child);
        return None;
    };
    let status = child.wait().ok()?;
    status.success().then(|| text.lines().filter_map(|l| l.strip_suffix(": test")).map(str::to_owned).collect())
}

/// What to tell about a test binary that ran out of `limit`: the lines to print and the one-line
/// error. `listed` is every test it would run (`--list`), when known.
pub fn report(limit: Duration, binary: &str, progress: &Progress, listed: Option<&[String]>) -> (Vec<String>, String) {
    let running = progress.running();
    let mut lines = vec![format!("timed out after {}: {binary}; killed it and the processes it started", show(limit))];
    if running.is_empty() {
        lines.push("  no test was reported as running (libtest warns about a test after 60 s)".to_owned());
    } else {
        lines.push(format!("  still running: {}", running.join(", ")));
    }
    let never: Option<Vec<&String>> = listed.map(|all| all.iter().filter(|t| !progress.finished.contains(*t) && !running.contains(t)).collect());
    match (never, progress.total) {
        (Some(never), _) if !never.is_empty() => {
            let shown: Vec<&str> = never.iter().take(NAMED).map(|s| s.as_str()).collect();
            let more = never.len().saturating_sub(NAMED);
            let more = if more > 0 { format!(" and {more} more") } else { String::new() };
            lines.push(format!(
                "  no result from {} other test(s) of {} (not started, or started less than 60 s before): {}{more}",
                never.len(),
                listed.map_or(0, <[String]>::len),
                shown.join(", ")
            ));
        }
        (None, Some(total)) => {
            lines.push(format!("  {} of {total} test(s) had finished", progress.finished.len().min(total)));
        }
        _ => {}
    }
    lines.push(format!("  {TEST_BINARY_TIMEOUT_ENV}=<minutes> changes the limit (0 for none)"));
    let still = if running.is_empty() { String::new() } else { format!(" (still running: {})", running.join(", ")) };
    (lines, format!("timed out after {}{still}", show(limit)))
}

fn print(lines: &[String]) {
    let mut err = std::io::stderr().lock();
    if !lines.is_empty() {
        // the killed binary's last line may have no end (`test NAME ... `)
        let _ = writeln!(err);
    }
    for line in lines {
        let _ = writeln!(err, "{line}");
    }
}

/// Whether `cmd`'s arguments already choose libtest's colours.
fn sets_color(cmd: &Command) -> bool {
    cmd.get_args().any(|a| a.to_str().is_some_and(|a| a == "--color" || a.starts_with("--color=")))
}

/// Run a test binary (`cmd`, already given its arguments) under `limit` (`None`: no limit, and the
/// output isn't piped). The error says how it ended: its exit status, the signal that killed it,
/// that it ran out of time (and the tests still running), or why it didn't start.
pub fn run_test_binary(cmd: Command, label: &str, limit: Option<Duration>) -> Result<(), String> {
    let mut lines = Vec::new();
    let result = test_binary(cmd, label, limit, &mut lines);
    print(&lines);
    result
}

/// [`run_test_binary`], with the timeout report added to `shown` rather than printed.
fn test_binary(mut cmd: Command, label: &str, limit: Option<Duration>, shown: &mut Vec<String>) -> Result<(), String> {
    let Some(limit) = limit else {
        let status = cmd.status().map_err(|e| format!("failed to start: {e}"))?;
        return if status.success() { Ok(()) } else { Err(crate::test::describe(status)) };
    };
    let lister = copy_command(&cmd);
    // its stdout is a pipe now: keep libtest's colours when ours is a terminal
    if std::io::stdout().is_terminal() && !sets_color(&cmd) {
        cmd.args(["--color", "always"]);
    }
    match watch(cmd, limit, Clock::Start)? {
        Ended::Exited(status) if status.success() => Ok(()),
        Ended::Exited(status) => Err(crate::test::describe(status)),
        Ended::TimedOut(progress) => {
            let listed = list_tests(lister);
            let (lines, error) = report(limit, label, &progress, listed.as_deref());
            shown.extend(lines);
            Err(error)
        }
    }
}

/// Run `cmd`, a `cargo test` (or `cargo nextest run`) command, with each test binary it starts
/// under `limit` ([`Clock::EachBinary`]; `None`: no limit, and the output isn't piped).
pub fn run_cargo_tests(cmd: Command, limit: Option<Duration>) -> Result<(), String> {
    let mut lines = Vec::new();
    let result = cargo_tests(cmd, limit, &mut lines);
    print(&lines);
    result
}

/// [`run_cargo_tests`], with the timeout report added to `shown` rather than printed.
fn cargo_tests(mut cmd: Command, limit: Option<Duration>, shown: &mut Vec<String>) -> Result<(), String> {
    let Some(limit) = limit else {
        let status = cmd.status().map_err(|e| format!("failed to spawn: {e}"))?;
        return if status.success() { Ok(()) } else { Err(format!("exited with {status}")) };
    };
    // cargo's stderr is a pipe now: keep its colours when ours is a terminal
    if std::io::stderr().is_terminal() && std::env::var_os("CARGO_TERM_COLOR").is_none() {
        cmd.env("CARGO_TERM_COLOR", "always");
    }
    let dir = cmd.get_current_dir().map(Path::to_path_buf);
    match watch(cmd, limit, Clock::EachBinary)? {
        Ended::Exited(status) if status.success() => Ok(()),
        Ended::Exited(status) => Err(format!("exited with {status}")),
        Ended::TimedOut(progress) => {
            let listed = progress.exe.as_ref().and_then(|exe| {
                let exe = match &dir {
                    Some(dir) if exe.is_relative() => dir.join(exe),
                    _ => exe.clone(),
                };
                list_tests(Command::new(exe))
            });
            let binary = progress.binary.clone().unwrap_or_else(|| "a test binary".to_owned());
            let (lines, error) = report(limit, &binary, &progress, listed.as_deref());
            shown.extend(lines);
            Err(format!("{error}: {binary}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn limits_parse_as_minutes_and_hostile_values_are_errors() {
        let p = |v: Option<&str>| parse_minutes("V", v, 30);
        assert_eq!(p(None), Ok(Some(MIN * 30)), "unset: the default");
        assert_eq!(p(Some("  ")), Ok(Some(MIN * 30)), "blank: the default");
        assert_eq!(p(Some("45")), Ok(Some(MIN * 45)));
        assert_eq!(p(Some(" 0.5 ")), Ok(Some(Duration::from_secs(30))));
        assert_eq!(p(Some("1e1")), Ok(Some(MIN * 10)));
        for none in ["0", "0.0", "-0", "off", "OFF", "none", "never"] {
            assert_eq!(p(Some(none)), Ok(None), "{none}");
        }
        for bad in ["-1", "abc", "30m", "NaN", "inf", "-inf", "1e300", "10081", "18446744073709551616"] {
            let err = p(Some(bad)).unwrap_err();
            assert!(err.starts_with(&format!("V={bad}:")) && err.contains("0 or off for no limit"), "{err}");
        }
        assert_eq!(p(Some("10080")), Ok(Some(MIN * 10080)), "a week is allowed");
        assert_eq!(parse_minutes("V", None, u64::MAX), Ok(Some(Duration::from_secs(u64::MAX))), "saturates");
    }

    #[test]
    fn limits_show_in_minutes_seconds_or_milliseconds() {
        assert_eq!(show(MIN * 30), "30 min");
        assert_eq!(show(Duration::from_secs(90)), "1.5 min");
        assert_eq!(show(Duration::from_secs(20)), "20 s");
        assert_eq!(show(Duration::from_secs(61)), "61 s");
        assert_eq!(show(Duration::from_millis(1200)), "1.2 s");
        assert_eq!(show(Duration::from_millis(300)), "300 ms");
    }

    #[test]
    fn reads_libtest_and_cargo_lines() {
        let mut p = Progress::default();
        assert!(p.line("     Running unittests src/lib.rs (target/debug/deps/geom-1a2b)\n"));
        assert_eq!(p.binary.as_deref(), Some("unittests src/lib.rs (target/debug/deps/geom-1a2b)"));
        assert_eq!(p.exe, Some(PathBuf::from("target/debug/deps/geom-1a2b")));
        assert!(!p.line("running 4 tests"));
        assert_eq!(p.total, Some(4));
        p.line("test a::one ... ok");
        p.line("test a::two ... \u{1b}[31mFAILED\u{1b}[0m");
        p.line("test a::three has been running for over 60 seconds");
        p.line("test a::four has been running for over 60 seconds");
        p.line("test a::four ... ok");
        p.line("test result: FAILED. 2 passed; 1 failed");
        p.line("Running is not cargo's when it starts the line");
        assert_eq!(p.finished.iter().map(String::as_str).collect::<Vec<_>>(), ["a::four", "a::one", "a::two"]);
        assert_eq!(p.running(), ["a::three"], "warned about and not finished");
        assert!(p.line("\u{1b}[1m\u{1b}[92m   Doc-tests\u{1b}[0m lightcraft_geom"), "coloured, as with CARGO_TERM_COLOR=always");
        assert_eq!(p.binary.as_deref(), Some("doctests of lightcraft_geom"));
        assert_eq!(p.exe, None);
        assert_eq!(p.total, Some(4), "the test lines stay until the new binary's own `running` line");
        p.line("running 1 test");
        assert_eq!((p.total, p.finished.len(), p.running().len()), (Some(1), 0, 0), "which starts afresh");
        p.pending = "test src/lib.rs - f (line 3) ... ".to_owned();
        assert_eq!(p.running(), ["src/lib.rs - f (line 3)"], "a single-threaded run prints the name as the test starts");
    }

    #[test]
    fn finds_every_descendant_in_a_process_table() {
        let table = "  1 0\n 10 1\n 11 10\n 12 11\n 13 10\n 20 1\n 21 20\n junk\n 14 12";
        assert_eq!(descendants_in(table, 10), [11, 12, 13, 14]);
        assert!(descendants_in(table, 99).is_empty());
        assert_eq!(descendants_in("5 6\n6 5", 5), [6], "a cycle ends");
    }

    #[test]
    fn the_report_names_the_binary_the_running_tests_and_the_rest() {
        let mut p = Progress::default();
        for l in ["running 4 tests", "test a ... ok", "test b has been running for over 60 seconds"] {
            p.line(l);
        }
        let listed: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
        let (lines, error) = report(MIN * 30, "geom (geom-1)", &p, Some(&listed));
        assert_eq!(error, "timed out after 30 min (still running: b)");
        let text = lines.join("\n");
        assert!(text.starts_with("timed out after 30 min: geom (geom-1); killed it"), "{text}");
        assert!(text.contains("still running: b") && text.contains("no result from 2 other test(s) of 4") && text.contains(": c, d"), "{text}");
        assert!(text.contains(TEST_BINARY_TIMEOUT_ENV), "says how to change the limit: {text}");
        let (lines, _) = report(MIN, "x", &p, None);
        assert!(lines.join("\n").contains("1 of 4 test(s) had finished"), "without --list: {lines:?}");
        let many: Vec<String> = (0..30).map(|i| format!("t{i}")).collect();
        let (lines, _) = report(MIN, "x", &Progress::default(), Some(&many));
        let text = lines.join("\n");
        assert!(text.contains("no test was reported as running") && text.contains("t19 and 10 more") && !text.contains("t20"), "{text}");
    }

    /// Whether process `pid` is gone (or a zombie no one has reaped yet).
    #[cfg(unix)]
    fn gone(pid: &str) -> bool {
        let out = Command::new("ps").args(["-o", "stat=", "-p", pid]).output().unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        stat.trim().is_empty() || stat.trim().starts_with('Z')
    }

    #[cfg(unix)]
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-deadline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn wait_gone(pid: &str) -> bool {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if gone(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_binary_is_killed_with_the_processes_it_started_and_named() {
        let dir = scratch("hung");
        let pidfile = dir.join("grandchild.pid");
        // a "test binary" that reports one test, warns about another, starts a grandchild and hangs;
        // `sh -c SCRIPT --list` (how the report lists the tests) sets $0 to `--list`
        let script = format!(
            "case \"$0\" in --list) printf 'a: test\\nb: test\\nc: test\\n\\n3 tests, 0 benchmarks\\n'; exit;; esac; \
             printf 'running 3 tests\\ntest a ... ok\\ntest b has been running for over 60 seconds\\n'; \
             sleep 300 & echo $! > '{}'; wait",
            pidfile.display()
        );
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", &script]);
        let t = Instant::now();
        let mut shown = Vec::new();
        let err = test_binary(cmd, "fake (sh)", Some(Duration::from_secs(2)), &mut shown).unwrap_err();
        assert!(t.elapsed() < Duration::from_secs(14), "the limit ended it, and --list answered: {:?}", t.elapsed());
        assert_eq!(err, "timed out after 2 s (still running: b)");
        let shown = shown.join("\n");
        assert!(shown.starts_with("timed out after 2 s: fake (sh); killed it"), "{shown}");
        assert!(shown.contains("no result from 1 other test(s) of 3") && shown.ends_with("(0 for none)"), "c, from --list: {shown}");
        let grandchild = std::fs::read_to_string(&pidfile).unwrap();
        assert!(wait_gone(grandchild.trim()), "the sleep it started was killed too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_that_ends_in_time_passes_or_fails_as_before() {
        let sh = |script: &str| {
            let mut c = Command::new("/bin/sh");
            c.args(["-c", script]);
            run_test_binary(c, "fake (sh)", Some(MIN))
        };
        assert_eq!(sh("echo running 0 tests"), Ok(()));
        assert_eq!(sh("exit 3"), Err("exit status: 3".to_owned()));
        assert!(sh("kill -9 $$").unwrap_err().contains("SIGKILL"));
    }

    #[cfg(unix)]
    #[test]
    fn under_cargo_each_binary_gets_the_limit_and_the_build_does_not_count() {
        // the "build" outlasts the limit, then each "binary" finishes in time
        let script = "sleep 1; \
            echo '     Running unittests src/lib.rs (target/debug/deps/a-1)' >&2; echo 'running 1 test'; echo 'test x ... ok'; \
            echo '     Running tests/it/main.rs (target/debug/deps/it-2)' >&2; echo 'running 0 tests'";
        let mut c = Command::new("/bin/sh");
        c.args(["-c", script]);
        assert_eq!(run_cargo_tests(c, Some(Duration::from_millis(500))), Ok(()));

        let dir = scratch("cargo");
        let pidfile = dir.join("binary.pid");
        let script = format!(
            "echo '     Running tests/it/main.rs (target/debug/deps/it-2)' >&2; \
             sh -c 'echo running 2 tests; printf \"test slow ... \"; sleep 300' & echo $! > '{}'; wait",
            pidfile.display()
        );
        let mut c = Command::new("/bin/sh");
        c.args(["-c", &script]);
        let mut shown = Vec::new();
        let err = cargo_tests(c, Some(Duration::from_secs(2)), &mut shown).unwrap_err();
        assert!(shown.first().is_some_and(|l| l.starts_with("timed out after 2 s: tests/it/main.rs")), "{shown:?}");
        assert_eq!(err, "timed out after 2 s (still running: slow): tests/it/main.rs (target/debug/deps/it-2)");
        let binary = std::fs::read_to_string(&pidfile).unwrap();
        assert!(wait_gone(binary.trim()), "the test binary cargo started was killed too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn no_limit_runs_as_before() {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "exit 0"]);
        assert_eq!(run_cargo_tests(c, None), Ok(()));
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "exit 4"]);
        assert_eq!(run_test_binary(c, "fake", None), Err("exit status: 4".to_owned()));
    }
}
