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
//!   per test binary, from the `Running` (or `Doc-tests`) line cargo prints as it starts each one,
//!   and again from the binary's own `running N tests` line; the build before the first one isn't
//!   bounded. Cargo runs loud for this (`CARGO_TERM_QUIET=false`, no `-q`): a quiet cargo prints no
//!   `Running` lines. Under `cargo nextest run` (no such lines) nextest's own `slow-timeout` in
//!   `.config/nextest.toml` ends a hung test instead.
//!
//! The output streams through as it arrives, and is read on the way for libtest's lines: `running N
//! tests`, `test NAME ... ok` as each test finishes (or `test NAME ... ` as it starts, when one runs
//! at a time), and `test NAME has been running for over 60 seconds`. What is kept of it is bounded
//! ([`MAX_LINE`], [`NAME_BUDGET`]). On timeout the binary and every process it started are killed,
//! and the report names the binary, the tests still running and the tests that never reported a
//! result (from `--list`, itself bounded in time and size).
//!
//! Containment, and never a process xtask didn't start: every process xtask watches gets an
//! environment variable of its own ([`Marker`], `LIGHTCRAFT_XTASK_RUN_<pid>_<nonce>_<n>=1`, the
//! nonce random per xtask process so an earlier xtask with the same pid had another) that
//! everything it starts inherits. On timeout the binary (while unreaped, so its pid is its own) and
//! its process tree are frozen with SIGSTOP level by level, a process only once its parent is
//! stopped and so can't reap it and free its pid; on Linux also every process of this user whose
//! `/proc/<pid>/environ` holds the exact entry, which finds those that outlived their parent. Once
//! no new one turns up they are killed with SIGKILL; the report says how many, names any still
//! alive, and says when the OS doesn't allow looking for the reparented ones (Unix other than
//! Linux: only the tree; Windows: `taskkill /T /F`). Only a process that clears its environment
//! escapes. The child stays in xtask's process group rather than one of its own, so Ctrl-C in the
//! terminal still reaches the test binaries as before.
//!
//! A timeout also turns off the optional probes after it in the same run ([`any_timed_out`]): the
//! GPU probe, which runs bounded too ([`capture_cargo`], [`probe_limit`]), and the translation
//! gaps report.

use std::collections::BTreeSet;
use std::hash::{BuildHasher, Hasher};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The limit, in minutes, on each test binary's run.
pub const TEST_BINARY_TIMEOUT_ENV: &str = "LIGHTCRAFT_TEST_BINARY_TIMEOUT";
/// The default of [`TEST_BINARY_TIMEOUT_ENV`]: the slowest binary, the ui-egui library tests, takes
/// about 5 minutes here under `ci`'s caps.
pub const DEFAULT_TEST_BINARY_TIMEOUT_MIN: u64 = 30;
/// The longest limit a variable may set (a week); `0` or `off` means none.
const MAX_MINUTES: f64 = 7.0 * 24.0 * 60.0;
/// How often the waits check the child and the clock.
const TICK: Duration = Duration::from_millis(50);
/// How long, after the child ends, to wait for its output pipes to close (a process it started
/// that outlives it may hold them open).
const DRAIN: Duration = Duration::from_secs(5);
/// How long the `--list` run of a timed-out binary may take, start to exit (it runs no tests).
const LIST_LIMIT: Duration = Duration::from_secs(15);
/// The most `--list` output read; the rest is cut.
const LIST_MAX_BYTES: usize = 16 << 20;
/// The longest line of output parsed for test names; longer ones stream through unparsed.
const MAX_LINE: usize = 64 * 1024;
/// The bytes of test names one [`Names`] keeps (finished, slow or listed tests of one binary).
const NAME_BUDGET: usize = 8 << 20;
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

/// `30 min`, `1.5 min`, `20 s`, `1.2 s`, `300 ms`: a limit as the reports show it.
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

/// Splits output into lines as it arrives, dropping (not parsing) lines over [`MAX_LINE`].
#[derive(Default)]
struct Lines {
    buf: Vec<u8>,
    overlong: bool,
}

impl Lines {
    /// Feed `chunk`; `each` gets every whole line it completes.
    fn feed(&mut self, chunk: &[u8], mut each: impl FnMut(&str)) {
        for piece in chunk.split_inclusive(|b| *b == b'\n') {
            if !self.overlong {
                self.buf.extend_from_slice(piece);
                if self.buf.len() > MAX_LINE {
                    self.buf.clear();
                    self.overlong = true;
                }
            }
            if piece.ends_with(b"\n") {
                if !self.overlong {
                    each(&String::from_utf8_lossy(&self.buf));
                }
                self.buf.clear();
                self.overlong = false;
            }
        }
    }

    /// The line begun and not yet ended.
    fn rest(&self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }
}

/// Test names in the order first seen, without repeats, up to [`NAME_BUDGET`] bytes; past that
/// they are dropped and [`Names::cut`] says so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Names {
    order: Vec<String>,
    index: BTreeSet<String>,
    bytes: usize,
    budget: usize,
    /// Some names were dropped for the budget.
    pub cut: bool,
}

impl Default for Names {
    fn default() -> Self {
        Self::with_budget(NAME_BUDGET)
    }
}

impl Names {
    fn with_budget(budget: usize) -> Self {
        Self { order: Vec::new(), index: BTreeSet::new(), bytes: 0, budget, cut: false }
    }

    fn insert(&mut self, name: &str) {
        if self.index.contains(name) {
            return;
        }
        // kept twice (order and index)
        let cost = name.len().saturating_mul(2);
        if self.bytes.saturating_add(cost) > self.budget {
            self.cut = true;
            return;
        }
        self.bytes += cost;
        self.order.push(name.to_owned());
        self.index.insert(name.to_owned());
    }

    fn contains(&self, name: &str) -> bool {
        self.index.contains(name)
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    fn iter(&self) -> impl Iterator<Item = &str> {
        self.order.iter().map(String::as_str)
    }

    fn clear(&mut self) {
        *self = Self::with_budget(self.budget);
    }
}

/// Whether `result`, what follows `test NAME ... `, is one of libtest's results.
fn is_result(result: &str) -> bool {
    let r = result.trim();
    matches!(r, "ok" | "FAILED" | "ignored")
        || r.starts_with("ignored, ")
        || r.starts_with("bench:")
        || r.starts_with("ok <")
        || r.starts_with("FAILED <")
        || r.starts_with("FAILED (")
}

/// `test NAME ... REST` (REST may be empty: a test a single-threaded run just started): the name
/// and the rest.
fn test_line(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("test ")?;
    if let Some((name, result)) = rest.split_once(" ... ") {
        return Some((name, result));
    }
    rest.trim_end().strip_suffix(" ...").map(|name| (name, ""))
}

/// What a line of output meant for the clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Cargo started a test binary (`Running`, `Doc-tests`).
    Binary,
    /// A test binary started its tests (`running N tests`).
    Tests,
    Other,
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
    /// The tests that reported a result (`test NAME ... ok`, `FAILED`, `ignored`, `bench:`).
    pub finished: Names,
    /// The tests libtest said had been running for over 60 seconds, in that order.
    pub slow: Names,
    /// The test a single-threaded run started (`test NAME ... `) and has no result for yet; the
    /// test's own output may come between.
    pub active: Option<String>,
    /// The unfinished last line of stdout: `test NAME ... ` while a single-threaded run runs NAME.
    pub pending: String,
}

impl Progress {
    /// Read one whole line of output.
    pub fn line(&mut self, raw: &str) -> Event {
        let clean = strip_ansi(raw);
        let line = clean.trim_end();
        let text = line.trim_start();
        // cargo right-aligns its status word, so its lines start with spaces
        let cargo_status = line.starts_with(' ');
        if let Some(rest) = text.strip_prefix("Running ").filter(|_| cargo_status) {
            let exe = rest.strip_suffix(')').and_then(|r| r.rsplit_once(" (")).map(|(_, path)| PathBuf::from(path));
            self.start_binary(rest.to_owned(), exe);
            return Event::Binary;
        }
        if let Some(rest) = text.strip_prefix("Doc-tests ").filter(|_| cargo_status) {
            self.start_binary(format!("doctests of {rest}"), None);
            return Event::Binary;
        }
        if let Some(rest) = text.strip_prefix("running ") {
            let mut words = rest.split_whitespace();
            if let (Some(n), Some("test" | "tests"), None) = (words.next().and_then(|n| n.parse().ok()), words.next(), words.next()) {
                self.reset();
                self.total = Some(n);
                return Event::Tests;
            }
            return Event::Other;
        }
        if let Some((name, result)) = test_line(text) {
            if is_result(result) {
                self.finish(name);
            } else {
                // started; what follows is the test's own output (single-threaded, --nocapture)
                self.active = Some(name.to_owned());
            }
            return Event::Other;
        }
        if let Some((name, _)) = text.strip_prefix("test ").and_then(|r| r.split_once(" has been running for over ")) {
            self.slow.insert(name);
            return Event::Other;
        }
        // the result of the active test, after its output
        if is_result(text)
            && let Some(name) = self.active.take()
        {
            self.finished.insert(&name);
        }
        Event::Other
    }

    fn finish(&mut self, name: &str) {
        self.finished.insert(name);
        if self.active.as_deref() == Some(name) {
            self.active = None;
        }
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
        self.active = None;
        self.pending.clear();
    }

    /// The tests still running: warned about as slow and not finished, then the one a
    /// single-threaded run started last.
    pub fn running(&self) -> Vec<String> {
        let mut running: Vec<String> = self.slow.iter().filter(|s| !self.finished.contains(s)).map(str::to_owned).collect();
        let pending = strip_ansi(&self.pending);
        let started = test_line(pending.trim_start()).filter(|(_, result)| !is_result(result)).map(|(name, _)| name);
        for name in [self.active.as_deref(), started].into_iter().flatten() {
            if !self.finished.contains(name) && !running.iter().any(|r| r == name) {
                running.push(name.to_owned());
            }
        }
        running
    }
}

/// When a run's time limit starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    /// When the process starts: a test binary run directly.
    Start,
    /// At each test binary cargo starts (its `Running` / `Doc-tests` line, and its own `running N
    /// tests` line): the limit is per binary, and the build before the first isn't bounded.
    EachBinary,
}

/// The environment variable that marks every process one watched run starts (see the module
/// docs). Its name holds xtask's pid, a random number drawn once per xtask process and a run
/// counter, so it is this run's alone: nested runs keep each other's, and the leftovers of an
/// earlier xtask that had the same pid carry a different one.
pub struct Marker {
    key: String,
}

/// The value every marker has; a process is marked only by the exact entry `KEY=1`.
const MARK_VALUE: &str = "1";

/// A number drawn once per xtask process: std's randomly seeded hasher, fed the pid and the time.
fn process_nonce() -> u64 {
    static NONCE: OnceLock<u64> = OnceLock::new();
    *NONCE.get_or_init(|| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.finish()
    })
}

impl Marker {
    fn new() -> Self {
        static RUNS: AtomicU64 = AtomicU64::new(0);
        Self::with(std::process::id(), process_nonce(), RUNS.fetch_add(1, Ordering::Relaxed))
    }

    fn with(pid: u32, nonce: u64, run: u64) -> Self {
        Self { key: format!("LIGHTCRAFT_XTASK_RUN_{pid}_{nonce:016x}_{run}") }
    }

    /// The exact environment entry that marks the run's processes.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn entry(&self) -> String {
        format!("{}={MARK_VALUE}", self.key)
    }
}

/// Spawn `cmd` with a fresh [`Marker`].
fn spawn_marked(cmd: &mut Command) -> std::io::Result<(Child, Marker)> {
    let marker = Marker::new();
    cmd.env(&marker.key, MARK_VALUE);
    Ok((cmd.spawn()?, marker))
}

/// Set when a watched run runs out of time: the optional probes after it (the GPU probe, the
/// translation gaps report) are skipped, as what hung the tests would hang them too.
static TIMED_OUT: AtomicBool = AtomicBool::new(false);

/// Whether a watched run in this xtask process ran out of time.
pub fn any_timed_out() -> bool {
    TIMED_OUT.load(Ordering::Relaxed)
}

/// How a watched run ended.
#[derive(Debug)]
pub enum Ended {
    Exited(ExitStatus),
    /// Out of time: killed (what the kill did), with what its output said by then.
    TimedOut(Box<Progress>, String),
}

/// Where a watched run's output goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Through to xtask's stdout and stderr, as it arrives.
    Stream,
    /// Kept: stdout up to [`CAPTURE_MAX`] bytes, the last [`STDERR_TAIL`] lines of stderr.
    Capture,
}

/// The most stdout a captured run keeps.
const CAPTURE_MAX: usize = 1 << 20;
/// How many of the last stderr lines a captured run keeps.
const STDERR_TAIL: usize = 20;

/// A watched run: how it ended and, when captured, its output.
#[derive(Debug)]
pub struct Watched {
    pub ended: Ended,
    pub stdout: String,
    pub stderr_tail: Vec<String>,
}

struct Watch {
    progress: Progress,
    /// When the current limit started, `None` while it doesn't run.
    since: Option<Instant>,
    stdout: Vec<u8>,
    stderr_tail: std::collections::VecDeque<String>,
}

/// Run `cmd` with its output streamed through (or captured) and read for libtest's lines, until it
/// exits or `limit` runs out (timed as `clock` says); then kill it and every process it started.
pub fn watch(mut cmd: Command, limit: Duration, clock: Clock, output: Output) -> Result<Watched, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let (mut child, marker) = spawn_marked(&mut cmd).map_err(|e| format!("failed to start: {e}"))?;
    let state = Arc::new(Mutex::new(Watch {
        progress: Progress::default(),
        since: (clock == Clock::Start).then(Instant::now),
        stdout: Vec::new(),
        stderr_tail: std::collections::VecDeque::new(),
    }));
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
            pump(pipe, is_stdout, clock, output, &state);
            let _ = done.send(());
        });
        if let Err(e) = spawned {
            kill_run(Some(&mut child), &marker);
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
    let finish = |ended: Ended| {
        let mut w = lock(&state);
        let stdout = String::from_utf8_lossy(&std::mem::take(&mut w.stdout)).into_owned();
        Watched { ended, stdout, stderr_tail: w.stderr_tail.drain(..).collect() }
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                drain(readers);
                return Ok(finish(Ended::Exited(status)));
            }
            Ok(None) => {}
            Err(e) => {
                kill_run(Some(&mut child), &marker);
                return Err(format!("waiting for it failed: {e}"));
            }
        }
        if lock(&state).since.is_some_and(|since| since.elapsed() >= limit) {
            TIMED_OUT.store(true, Ordering::Relaxed);
            // not reaped yet (try_wait said it runs): its pid is still its own
            let killed = kill_run(Some(&mut child), &marker);
            drain(readers);
            let progress = Box::new(lock(&state).progress.clone());
            return Ok(finish(Ended::TimedOut(progress, killed)));
        }
        std::thread::sleep(TICK);
    }
}

/// Copy `pipe` to xtask's stdout or stderr as it arrives (or keep it, bounded), feeding whole lines
/// to the progress.
fn pump(mut pipe: Box<dyn Read + Send>, is_stdout: bool, clock: Clock, output: Output, state: &Mutex<Watch>) {
    let mut buf = [0u8; 8192];
    let mut lines = Lines::default();
    loop {
        let n = match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let chunk = buf.get(..n).unwrap_or_default();
        let mut watch = lock(state);
        match (output, is_stdout) {
            (Output::Stream, true) => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(chunk).and_then(|()| out.flush());
            }
            (Output::Stream, false) => {
                let mut err = std::io::stderr().lock();
                let _ = err.write_all(chunk).and_then(|()| err.flush());
            }
            (Output::Capture, true) => {
                let room = CAPTURE_MAX.saturating_sub(watch.stdout.len());
                watch.stdout.extend_from_slice(chunk.get(..room.min(chunk.len())).unwrap_or_default());
            }
            (Output::Capture, false) => {}
        }
        lines.feed(chunk, |line| {
            let event = watch.progress.line(line);
            if clock == Clock::EachBinary && event != Event::Other {
                watch.since = Some(Instant::now());
            }
            if output == Output::Capture && !is_stdout {
                if watch.stderr_tail.len() >= STDERR_TAIL {
                    watch.stderr_tail.pop_front();
                }
                watch.stderr_tail.push_back(line.trim_end().to_owned());
            }
        });
        if is_stdout {
            watch.progress.pending = lines.rest();
        }
    }
}

/// Kill everything a run started, reap its direct child, and say what was done.
///
/// `child` is the run's direct child while it hasn't been reaped (so its pid is still its own):
/// it and its process tree are signalled, plus every process carrying the run's `marker`. `None`
/// once it has been reaped: then only processes carrying the marker are, never the old pid, which
/// may belong to another process by now.
fn kill_run(child: Option<&mut Child>, marker: &Marker) -> String {
    let root = child.as_ref().map(|c| c.id());
    let note = kill_all(root, marker);
    if let Some(child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
    note
}

/// Whether this OS lets xtask find a run's processes by their marker (Linux: `/proc/*/environ`).
#[cfg_attr(not(unix), allow(dead_code))]
fn can_find_marked() -> bool {
    cfg!(target_os = "linux") && Path::new("/proc/self/environ").exists()
}

/// Freeze and kill `root` (unreaped) and what it started (see [`kill_run`]); what was done.
///
/// Only processes xtask started are signalled. One found through the process tree is signalled only
/// once its parent is stopped: a stopped parent can't reap it, so its pid can't pass to another
/// process meanwhile. One found by its marker carries this run's exact entry, which nothing else
/// has. Everything found is frozen with SIGSTOP until no new one turns up, then killed with
/// SIGKILL, the last found first.
#[cfg(unix)]
fn kill_all(root: Option<u32>, marker: &Marker) -> String {
    let me = std::process::id();
    let entry = marker.entry();
    let by_marker = can_find_marked();
    let mut order: Vec<u32> = Vec::new();
    let mut stopped: BTreeSet<u32> = BTreeSet::new();
    let mut fresh: Vec<u32> = root.into_iter().collect();
    for _ in 0..64 {
        signal("STOP", &fresh);
        stopped.extend(&fresh);
        order.extend(&fresh);
        let mut found = children_of(&process_table(), &stopped);
        if by_marker {
            found.extend(marked(&entry));
        }
        found.retain(|p| *p != me && !stopped.contains(p));
        if found.is_empty() {
            break;
        }
        fresh = found.into_iter().collect();
    }
    signal("KILL", &order.iter().rev().copied().collect::<Vec<_>>());
    let started = order.len().saturating_sub(usize::from(root.is_some()));
    if !by_marker {
        return match root {
            Some(_) => format!(
                "killed it and the {started} process(es) it started that were still its descendants; on this OS processes that \
                 outlived their parent can't be found, and may still run"
            ),
            None => "on this OS the processes it started that outlived it can't be found, and may still run".to_owned(),
        };
    }
    // a killed process loses its environment as it dies, before it is reaped
    let until = Instant::now() + Duration::from_secs(2);
    let mut alive = marked(&entry);
    while !alive.is_empty() && Instant::now() < until {
        std::thread::sleep(TICK);
        alive = marked(&entry);
    }
    let mut note = match (root, started) {
        (Some(_), 0) => "killed it; it had started no other process (looked for in its process tree and by the run's environment marker)".to_owned(),
        (Some(_), n) => format!(
            "killed it and the {n} process(es) it started (found in its process tree and by the run's environment marker, so reparented ones too)"
        ),
        (None, 0) => "no process it started was left (looked for by the run's environment marker)".to_owned(),
        (None, n) => format!("killed the {n} process(es) it started that outlived it (found by the run's environment marker)"),
    };
    if !alive.is_empty() {
        let pids: Vec<String> = alive.iter().map(u32::to_string).collect();
        note.push_str(&format!("; still alive after the kill: pid {} (they carry {})", pids.join(", "), marker.key));
    }
    note
}

#[cfg(windows)]
fn kill_all(root: Option<u32>, _marker: &Marker) -> String {
    let Some(root) = root else {
        return "on this OS the processes it started that outlived it can't be found, and may still run".to_owned();
    };
    let _ = Command::new("taskkill").args(["/PID", &root.to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    "killed it and its process tree (taskkill /T); processes that outlived their parent may still run".to_owned()
}

#[cfg(not(any(unix, windows)))]
fn kill_all(_root: Option<u32>, _marker: &Marker) -> String {
    "killed it; processes it started may still run".to_owned()
}

#[cfg(unix)]
fn signal(name: &str, pids: &[u32]) {
    if pids.is_empty() {
        return;
    }
    let _ = Command::new("kill").arg(format!("-{name}")).args(pids.iter().map(u32::to_string)).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// Every process's pid and parent pid, from `ps` (empty if it fails).
#[cfg(unix)]
fn process_table() -> Vec<(u32, u32)> {
    let Ok(out) = Command::new("ps").args(["-A", "-o", "pid=", "-o", "ppid="]).stderr(Stdio::null()).output() else {
        return Vec::new();
    };
    parse_process_table(&String::from_utf8_lossy(&out.stdout))
}

/// The `pid ppid` pairs of a process table, one process per line.
#[cfg_attr(not(unix), allow(dead_code))]
fn parse_process_table(table: &str) -> Vec<(u32, u32)> {
    table
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some((w.next()?.parse().ok()?, w.next()?.parse().ok()?))
        })
        .collect()
}

/// The direct children of `parents` in a process table.
#[cfg_attr(not(unix), allow(dead_code))]
fn children_of(table: &[(u32, u32)], parents: &BTreeSet<u32>) -> BTreeSet<u32> {
    table.iter().filter(|(pid, ppid)| parents.contains(ppid) && !parents.contains(pid)).map(|(pid, _)| *pid).collect()
}

/// Whether a NUL-separated environment block holds exactly `entry` (`KEY=VALUE`), not as part of
/// another variable's name or value.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn has_entry(environ: &[u8], entry: &str) -> bool {
    environ.split(|b| *b == 0).any(|var| var == entry.as_bytes())
}

/// This user's processes whose environment holds exactly `entry` (`/proc/<pid>/environ`; a
/// process that can't be read is skipped, and zombies have no environment).
#[cfg(target_os = "linux")]
fn marked(entry: &str) -> Vec<u32> {
    use std::os::unix::fs::MetadataExt;
    let (Ok(me), Ok(dir)) = (std::fs::metadata("/proc/self"), std::fs::read_dir("/proc")) else {
        return Vec::new();
    };
    dir.filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| std::fs::metadata(format!("/proc/{pid}")).is_ok_and(|m| m.uid() == me.uid()))
        .filter(|pid| std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|env| has_entry(&env, entry)))
        .collect()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn marked(_entry: &str) -> Vec<u32> {
    Vec::new()
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

/// The tests a test binary would run (`--list`), as far as they were read.
#[derive(Debug)]
pub struct Listed {
    pub names: Names,
    /// The output passed [`LIST_MAX_BYTES`] or the names their budget: the list is incomplete.
    pub cut: bool,
}

/// The tests a test binary would run, from `c` (the binary with its arguments) plus `--list`;
/// `None` when that fails or takes over `limit` from start to exit. At most `max_bytes` of its
/// output are read. Whatever it started is killed when it is done, early or not, including
/// processes that outlived it and hold its output open.
fn list_tests(c: Command, limit: Duration, max_bytes: usize) -> Option<Listed> {
    let mut c = c;
    c.arg("--list").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let until = Instant::now() + limit;
    let (mut child, marker) = spawn_marked(&mut c).ok()?;
    let Some(mut stdout) = child.stdout.take() else {
        kill_run(Some(&mut child), &marker);
        return None;
    };
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::Builder::new().name("xtask-list".into()).spawn(move || {
        let (mut names, mut lines, mut read, mut buf) = (Names::default(), Lines::default(), 0usize, [0u8; 8192]);
        let mut cut = false;
        loop {
            let n = match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let room = max_bytes.saturating_sub(read);
            let chunk = buf.get(..n.min(room)).unwrap_or_default();
            read = read.saturating_add(chunk.len());
            lines.feed(chunk, |line| {
                if let Some(name) = line.trim_end().strip_suffix(": test") {
                    names.insert(name);
                }
            });
            if n > room {
                cut = true;
                break;
            }
        }
        let cut = cut || names.cut;
        let _ = tx.send(Listed { names, cut });
    });
    if reader.is_err() {
        kill_run(Some(&mut child), &marker);
        return None;
    }
    let (mut status, mut listed) = (None, None);
    let result = loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(s) => status = s,
                Err(_) => break None,
            }
        }
        if listed.is_none() {
            match rx.try_recv() {
                Ok(l) => listed = Some(l),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => break None,
            }
        }
        match (status, listed.as_ref()) {
            // read as much as it may: whatever more it writes (or how the cut pipe ends it) doesn't matter
            (_, Some(l)) if l.cut => break listed,
            (Some(s), Some(_)) => break listed.filter(|_| s.success()),
            _ => {}
        }
        if Instant::now() >= until {
            break None;
        }
        std::thread::sleep(TICK);
    };
    // its pid is signalled only while it is unreaped; after that, only the marked processes
    if status.is_none() {
        kill_run(Some(&mut child), &marker);
    } else {
        kill_run(None, &marker);
    }
    result
}

/// What to tell about a test binary that ran out of `limit`: the lines to print and the one-line
/// error. `listed` is every test it would run (`--list`), when known; `killed` what the kill did.
pub fn report(limit: Duration, binary: &str, progress: &Progress, listed: Option<&Listed>, killed: &str) -> (Vec<String>, String) {
    let running = progress.running();
    let mut lines = vec![format!("timed out after {}: {binary}", show(limit)), format!("  {killed}")];
    if running.is_empty() {
        lines.push("  no test was reported as running (libtest warns about a test after 60 s)".to_owned());
    } else {
        lines.push(format!("  still running: {}", running.join(", ")));
    }
    let never: Option<Vec<&str>> =
        listed.map(|l| l.names.iter().filter(|t| !progress.finished.contains(t) && !running.iter().any(|r| r.as_str() == *t)).collect());
    match (never, progress.total) {
        (Some(never), _) if !never.is_empty() => {
            let shown: Vec<&str> = never.iter().take(NAMED).copied().collect();
            let more = never.len().saturating_sub(NAMED);
            let more = if more > 0 { format!(" and {more} more") } else { String::new() };
            lines.push(format!(
                "  no result from {} other test(s) of {} (not started, or started less than 60 s before): {}{more}",
                never.len(),
                listed.map_or(0, |l| l.names.len()),
                shown.join(", ")
            ));
        }
        (None, Some(total)) => {
            lines.push(format!("  {} of {total} test(s) had finished", progress.finished.len().min(total)));
        }
        _ => {}
    }
    if listed.is_some_and(|l| l.cut) || progress.finished.cut || progress.slow.cut {
        lines.push("  (the output named more tests than xtask keeps, so these lists may be incomplete)".to_owned());
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
    match watch(cmd, limit, Clock::Start, Output::Stream)?.ended {
        Ended::Exited(status) if status.success() => Ok(()),
        Ended::Exited(status) => Err(crate::test::describe(status)),
        Ended::TimedOut(progress, killed) => {
            let listed = list_tests(lister, LIST_LIMIT, LIST_MAX_BYTES);
            let (lines, error) = report(limit, label, &progress, listed.as_ref(), &killed);
            shown.extend(lines);
            Err(error)
        }
    }
}

/// `cmd`, a cargo command, made to print the `Running` lines the per-binary clock starts from: no
/// `-q` / `--quiet` among cargo's own arguments (those before `--`), and `CARGO_TERM_QUIET=false`
/// over any quiet setting in the environment or cargo's config.
fn loud(cmd: &Command) -> Command {
    let mut c = Command::new(cmd.get_program());
    let mut cargos = true;
    for arg in cmd.get_args() {
        if arg == "--" {
            cargos = false;
        }
        if !(cargos && (arg == "-q" || arg == "--quiet")) {
            c.arg(arg);
        }
    }
    if let Some(dir) = cmd.get_current_dir() {
        c.current_dir(dir);
    }
    for (key, value) in cmd.get_envs() {
        match value {
            Some(v) => c.env(key, v),
            None => c.env_remove(key),
        };
    }
    c.env("CARGO_TERM_QUIET", "false");
    c
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
fn cargo_tests(cmd: Command, limit: Option<Duration>, shown: &mut Vec<String>) -> Result<(), String> {
    let mut cmd = cmd;
    let Some(limit) = limit else {
        let status = cmd.status().map_err(|e| format!("failed to spawn: {e}"))?;
        return if status.success() { Ok(()) } else { Err(format!("exited with {status}")) };
    };
    let mut cmd = loud(&cmd);
    // cargo's stderr is a pipe now: keep its colours when ours is a terminal
    if std::io::stderr().is_terminal() && std::env::var_os("CARGO_TERM_COLOR").is_none() {
        cmd.env("CARGO_TERM_COLOR", "always");
    }
    let dir = cmd.get_current_dir().map(Path::to_path_buf);
    match watch(cmd, limit, Clock::EachBinary, Output::Stream)?.ended {
        Ended::Exited(status) if status.success() => Ok(()),
        Ended::Exited(status) => Err(format!("exited with {status}")),
        Ended::TimedOut(progress, killed) => {
            let listed = progress.exe.as_ref().and_then(|exe| {
                let exe = match &dir {
                    Some(dir) if exe.is_relative() => dir.join(exe),
                    _ => exe.clone(),
                };
                list_tests(Command::new(exe), LIST_LIMIT, LIST_MAX_BYTES)
            });
            let binary = progress.binary.clone().unwrap_or_else(|| "a test binary".to_owned());
            let (lines, error) = report(limit, &binary, &progress, listed.as_ref(), &killed);
            shown.extend(lines);
            Err(format!("{error}: {binary}"))
        }
    }
}

/// The longest a probe of the machine (the GPU probe) may run, per test binary it starts.
const PROBE_LIMIT: Duration = Duration::from_secs(5 * 60);

/// The limit of a probe: [`PROBE_LIMIT`], or the test binaries' limit when that is shorter. A
/// probe is optional, so it keeps its own limit when the tests have none.
pub fn probe_limit(test_limit: Option<Duration>) -> Duration {
    test_limit.map_or(PROBE_LIMIT, |l| l.min(PROBE_LIMIT))
}

/// A captured cargo run: how it ended (`Err`: out of time, and what was killed) and its output.
#[derive(Debug)]
pub struct Captured {
    pub status: Result<ExitStatus, String>,
    pub stdout: String,
    pub stderr_tail: Vec<String>,
}

/// Run `cmd`, a cargo command that runs test binaries, with its output kept rather than shown
/// ([`Output::Capture`]) and each test binary it starts under `limit`, as [`run_cargo_tests`] does.
pub fn capture_cargo(cmd: &Command, limit: Duration) -> Result<Captured, String> {
    let watched = watch(loud(cmd), limit, Clock::EachBinary, Output::Capture)?;
    let status = match watched.ended {
        Ended::Exited(status) => Ok(status),
        Ended::TimedOut(progress, killed) => {
            let binary = progress.binary.clone().unwrap_or_else(|| "a test binary".to_owned());
            Err(format!("timed out after {}: {binary} ({killed})", show(limit)))
        }
    };
    Ok(Captured { status, stdout: watched.stdout, stderr_tail: watched.stderr_tail })
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
        assert_eq!(p.line("     Running unittests src/lib.rs (target/debug/deps/geom-1a2b)\n"), Event::Binary);
        assert_eq!(p.binary.as_deref(), Some("unittests src/lib.rs (target/debug/deps/geom-1a2b)"));
        assert_eq!(p.exe, Some(PathBuf::from("target/debug/deps/geom-1a2b")));
        assert_eq!(p.line("running 4 tests"), Event::Tests);
        assert_eq!(p.total, Some(4));
        p.line("test a::one ... ok");
        p.line("test a::two ... \u{1b}[31mFAILED\u{1b}[0m");
        p.line("test a::three has been running for over 60 seconds");
        p.line("test a::four has been running for over 60 seconds");
        p.line("test a::four ... ok");
        p.line("test result: FAILED. 2 passed; 1 failed");
        assert_eq!(p.line("Running is not cargo's when it starts the line"), Event::Other);
        assert_eq!(p.finished.iter().collect::<BTreeSet<_>>(), BTreeSet::from(["a::four", "a::one", "a::two"]));
        assert_eq!(p.running(), ["a::three"], "warned about and not finished");
        assert_eq!(p.line("\u{1b}[1m\u{1b}[92m   Doc-tests\u{1b}[0m lightcraft_geom"), Event::Binary, "coloured, as with CARGO_TERM_COLOR=always");
        assert_eq!(p.binary.as_deref(), Some("doctests of lightcraft_geom"));
        assert_eq!(p.exe, None);
        assert_eq!(p.total, Some(4), "the test lines stay until the new binary's own `running` line");
        p.line("running 1 test");
        assert_eq!((p.total, p.finished.len(), p.running().len()), (Some(1), 0, 0), "which starts afresh");
        p.pending = "test src/lib.rs - f (line 3) ... ".to_owned();
        assert_eq!(p.running(), ["src/lib.rs - f (line 3)"], "a single-threaded run prints the name as the test starts");
    }

    #[test]
    fn only_libtest_results_finish_a_test_and_its_output_between_does_not() {
        let mut p = Progress::default();
        p.line("running 5 tests");
        // single-threaded --nocapture: the test's output follows its name on the same line
        p.line("test a::hang ... connecting to server");
        p.line("still connecting");
        p.line("test a::hang has been running for over 60 seconds");
        assert!(!p.finished.contains("a::hang"));
        assert_eq!(p.running(), ["a::hang"]);
        // the result on a line of its own, after the output
        p.line("ok");
        assert!(p.finished.contains("a::hang") && p.running().is_empty());
        p.line("test b ... ");
        p.line("log line");
        p.line("FAILED");
        p.line("test c ... ignored, needs a GPU");
        p.line("test d ... bench:      1,234 ns/iter (+/- 56)");
        p.line("test e ... ok <0.003s>");
        assert_eq!(p.finished.iter().collect::<Vec<_>>(), ["a::hang", "b", "c", "d", "e"]);
        p.line("ok");
        assert_eq!(p.finished.len(), 5, "a stray result with no test running is nobody's");
        for not_a_result in ["okay", "ok then", "FAILED to connect", "ignoredx"] {
            assert!(!is_result(not_a_result), "{not_a_result}");
        }
    }

    #[test]
    fn what_is_kept_of_the_output_is_bounded() {
        let mut names = Names::with_budget(100);
        for i in 0..100 {
            names.insert(&format!("test_{i:03}"));
        }
        assert!(names.cut && names.len() == 6, "{} names kept (16 bytes each)", names.len());
        names.insert("test_000");
        assert_eq!(names.len(), 6, "no repeats");
        let mut lines = Lines::default();
        let mut seen = Vec::new();
        let long = vec![b'x'; MAX_LINE + 10];
        lines.feed(&long, |l| seen.push(l.len()));
        assert!(lines.rest().is_empty(), "an overlong line isn't kept");
        lines.feed(b"xx\nshort\npart", |l| seen.push(l.len()));
        assert_eq!(seen, [6], "the overlong line is skipped to its end; `short` is read");
        assert_eq!(lines.rest(), "part");
    }

    #[test]
    fn finds_the_children_of_stopped_processes_in_a_process_table() {
        let table = parse_process_table("  1 0\n 10 1\n 11 10\n 12 11\n 13 10\n 20 1\n 21 20\n junk\n 14 12");
        assert_eq!(table.len(), 8);
        assert_eq!(children_of(&table, &BTreeSet::from([10])), BTreeSet::from([11, 13]), "only direct children: their parent is stopped");
        assert_eq!(children_of(&table, &BTreeSet::from([10, 11, 13])), BTreeSet::from([12]));
        assert!(children_of(&table, &BTreeSet::from([99])).is_empty());
        assert!(children_of(&parse_process_table("5 6\n6 5"), &BTreeSet::from([5, 6])).is_empty(), "a cycle ends");
    }

    #[test]
    fn a_marker_is_an_exact_environment_entry_of_this_process_and_run() {
        let m = Marker::with(42, 0xabc, 3);
        assert_eq!(m.entry(), "LIGHTCRAFT_XTASK_RUN_42_0000000000000abc_3=1");
        let env = |vars: &[&str]| vars.join("\0").into_bytes();
        assert!(has_entry(&env(&["HOME=/h", &m.entry(), "PATH=/bin"]), &m.entry()));
        assert!(!has_entry(&env(&[&format!("OTHER={}", m.entry())]), &m.entry()), "another variable's value");
        assert!(!has_entry(&env(&[&format!("X{}", m.entry())]), &m.entry()), "another variable's name");
        assert!(!has_entry(&env(&[&format!("{}0", m.entry())]), &m.entry()), "another value");
        assert!(!has_entry(&env(&[&Marker::with(42, 0xabd, 3).entry()]), &m.entry()), "the same pid and run of another xtask");
        let (a, b) = (Marker::new(), Marker::new());
        assert_ne!(a.entry(), b.entry(), "each run its own");
        assert!(a.key.starts_with(&format!("LIGHTCRAFT_XTASK_RUN_{}_{:016x}_", std::process::id(), process_nonce())));
    }

    /// Only a process carrying the exact entry is found: not one with the text in an argument, in
    /// another variable, or with an earlier xtask's marker of the same pid.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_processes_with_the_exact_marker_are_found() {
        let marker = Marker::new();
        let stale = Marker::with(std::process::id(), process_nonce() ^ 1, 0);
        let spawn = |args: &[&str], env: Option<(&str, &str)>| {
            let mut c = Command::new("/bin/sh");
            // `read` is a builtin: the shell itself waits, with these arguments and environment
            c.args(["-c", "read x"]).args(args).stdin(Stdio::piped()).stdout(Stdio::null());
            if let Some((k, v)) = env {
                c.env(k, v);
            }
            c.spawn().unwrap()
        };
        let mut ours = spawn(&[], Some((&marker.key, MARK_VALUE)));
        let other_value = format!("x {}", marker.entry());
        let mut decoys = [
            spawn(&["sh", &marker.entry()], None),
            spawn(&[], Some(("OTHER", &other_value))),
            spawn(&[], Some((&stale.key, MARK_VALUE))),
            spawn(&[], Some((&marker.key, "2"))),
        ];
        let until = Instant::now() + Duration::from_secs(10);
        while marked(&marker.entry()).is_empty() && Instant::now() < until {
            std::thread::sleep(TICK);
        }
        // give the decoys time to start too, then look again
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(marked(&marker.entry()), [ours.id()]);
        for c in decoys.iter_mut().chain([&mut ours]) {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    #[test]
    fn the_report_names_the_binary_the_running_tests_and_the_rest() {
        let mut p = Progress::default();
        for l in ["running 4 tests", "test a ... ok", "test b has been running for over 60 seconds"] {
            p.line(l);
        }
        let mut names = Names::default();
        for n in ["a", "b", "c", "d"] {
            names.insert(n);
        }
        let listed = Listed { names, cut: false };
        let (lines, error) = report(MIN * 30, "geom (geom-1)", &p, Some(&listed), "killed it");
        assert_eq!(error, "timed out after 30 min (still running: b)");
        let text = lines.join("\n");
        assert!(text.starts_with("timed out after 30 min: geom (geom-1)\n  killed it\n"), "{text}");
        assert!(text.contains("still running: b") && text.contains("no result from 2 other test(s) of 4") && text.contains(": c, d"), "{text}");
        assert!(text.contains(TEST_BINARY_TIMEOUT_ENV) && !text.contains("incomplete"), "{text}");
        let (lines, _) = report(MIN, "x", &p, None, "");
        assert!(lines.join("\n").contains("1 of 4 test(s) had finished"), "without --list: {lines:?}");
        let mut names = Names::default();
        for i in 0..30 {
            names.insert(&format!("t{i}"));
        }
        let (lines, _) = report(MIN, "x", &Progress::default(), Some(&Listed { names, cut: true }), "");
        let text = lines.join("\n");
        assert!(text.contains("no test was reported as running") && text.contains("t19 and 10 more") && !text.contains("t20"), "{text}");
        assert!(text.contains("may be incomplete"), "a cut list says so: {text}");
    }

    #[test]
    fn loud_cargo_drops_quiet_flags_before_the_separator_and_overrides_the_config() {
        let mut c = Command::new("cargo");
        c.args(["test", "-q", "--workspace", "--quiet", "--", "-q", "--quiet"]).env("CARGO_TERM_QUIET", "true").env("X", "1");
        let l = loud(&c);
        assert_eq!(l.get_args().collect::<Vec<_>>(), ["test", "--workspace", "--", "-q", "--quiet"], "the test binaries' -q stays");
        let envs: Vec<_> = l.get_envs().collect();
        assert!(envs.contains(&(std::ffi::OsStr::new("CARGO_TERM_QUIET"), Some(std::ffi::OsStr::new("false")))), "{envs:?}");
        assert!(envs.contains(&(std::ffi::OsStr::new("X"), Some(std::ffi::OsStr::new("1")))));
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
    fn sh(script: &str) -> Command {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", script]);
        c
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
        let t = Instant::now();
        let mut shown = Vec::new();
        let err = test_binary(sh(&script), "fake (sh)", Some(Duration::from_secs(2)), &mut shown).unwrap_err();
        assert!(t.elapsed() < Duration::from_secs(14), "the limit ended it, and --list answered: {:?}", t.elapsed());
        assert_eq!(err, "timed out after 2 s (still running: b)");
        let shown = shown.join("\n");
        assert!(shown.starts_with("timed out after 2 s: fake (sh)\n  killed it and the 1 process(es) it started"), "{shown}");
        assert!(shown.contains("no result from 1 other test(s) of 3") && shown.ends_with("(0 for none)"), "c, from --list: {shown}");
        let grandchild = std::fs::read_to_string(&pidfile).unwrap();
        assert!(wait_gone(grandchild.trim()), "the sleep it started was killed too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_that_outlived_its_parent_is_killed_too() {
        let dir = scratch("orphan");
        let pidfile = dir.join("orphan.pid");
        // the subshell starts a sleep and exits: the sleep is reparented, no longer a descendant
        let script = format!("case \"$0\" in --list) exit;; esac; (sleep 300 & echo $! > '{}'); sleep 300", pidfile.display());
        let mut shown = Vec::new();
        let err = test_binary(sh(&script), "fake (sh)", Some(Duration::from_secs(1)), &mut shown).unwrap_err();
        assert!(err.starts_with("timed out after 1 s"), "{err}");
        let orphan = std::fs::read_to_string(&pidfile).unwrap();
        assert!(wait_gone(orphan.trim()), "the reparented sleep was found by its marker and killed: {shown:?}");
        let killed = shown.get(1).cloned().unwrap_or_default();
        assert!(killed.contains("2 process(es)") && killed.contains("reparented ones too") && !killed.contains("still alive"), "{killed}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_binary_that_started_nothing_is_reported_as_such() {
        let mut shown = Vec::new();
        let script = "case \"$0\" in --list) exit;; esac; exec sleep 300";
        assert!(test_binary(sh(script), "fake", Some(Duration::from_millis(500)), &mut shown).is_err());
        assert_eq!(
            shown.get(1).map(String::as_str),
            Some("  killed it; it had started no other process (looked for in its process tree and by the run's environment marker)")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_list_run_that_hangs_is_bounded_and_killed() {
        let dir = scratch("list");
        let pidfile = dir.join("list.pid");
        // closes its stdout, so the output is done, but never exits
        let script = format!("exec >&-; sleep 300 & echo $! > '{}'; wait", pidfile.display());
        let t = Instant::now();
        assert!(list_tests(sh(&script), Duration::from_secs(1), LIST_MAX_BYTES).is_none());
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        let pid = std::fs::read_to_string(&pidfile).unwrap();
        assert!(wait_gone(pid.trim()), "what it started was killed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `--list` run that exits but leaves a process holding its output: past the deadline that
    /// process is killed by its marker (the exited run's own pid is not signalled).
    #[cfg(target_os = "linux")]
    #[test]
    fn what_an_exited_list_run_left_behind_is_killed() {
        let dir = scratch("list-left");
        let (hung, cut) = (dir.join("hung.pid"), dir.join("cut.pid"));
        let script = format!("sleep 300 & echo $! > '{}'; exit 0", hung.display());
        let t = Instant::now();
        assert!(list_tests(sh(&script), Duration::from_secs(1), LIST_MAX_BYTES).is_none(), "its output never ended");
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        let pid = std::fs::read_to_string(&hung).unwrap();
        assert!(wait_gone(pid.trim()), "the sleep holding its output was killed");
        // the same when the output is cut short
        let script =
            format!("sleep 300 & echo $! > '{}'; i=0; while [ $i -lt 1000 ]; do echo \"t$i: test\"; i=$((i+1)); done; exit 0", cut.display());
        assert!(list_tests(sh(&script), MIN, 1000).is_some_and(|l| l.cut));
        let pid = std::fs::read_to_string(&cut).unwrap();
        assert!(wait_gone(pid.trim()), "the sleep of a cut list was killed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_captured_cargo_run_is_bounded_and_keeps_its_output() {
        let ok = capture_cargo(&sh("echo 'gpu_probe: {}'; echo warn >&2; echo last >&2"), MIN).unwrap();
        assert!(ok.status.as_ref().is_ok_and(ExitStatus::success));
        assert_eq!((ok.stdout.as_str(), ok.stderr_tail.as_slice()), ("gpu_probe: {}\n", ["warn".to_owned(), "last".to_owned()].as_slice()));
        // a probe whose GPU never answers: the binary cargo started hangs
        let script = "echo '     Running tests/it/main.rs (target/debug/deps/it-2)' >&2; i=0; \
            while [ $i -lt 30 ]; do echo \"line $i\" >&2; i=$((i+1)); done; head -c 3000000 /dev/zero; sleep 300";
        let t = Instant::now();
        let hung = capture_cargo(&sh(script), Duration::from_secs(1)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        let err = hung.status.unwrap_err();
        assert!(err.starts_with("timed out after 1 s: tests/it/main.rs (target/debug/deps/it-2) (killed it"), "{err}");
        assert_eq!(hung.stdout.len(), CAPTURE_MAX, "stdout is kept up to its cap");
        assert_eq!(hung.stderr_tail.len(), STDERR_TAIL);
        assert_eq!(hung.stderr_tail.last().map(String::as_str), Some("line 29"));
        assert!(any_timed_out(), "the optional probes after it are skipped");
        assert_eq!(probe_limit(None), PROBE_LIMIT);
        assert_eq!(probe_limit(Some(MIN)), MIN);
        assert_eq!(probe_limit(Some(MIN * 60)), PROBE_LIMIT);
    }

    #[cfg(unix)]
    #[test]
    fn a_list_run_is_read_only_so_far() {
        let listed = list_tests(sh("i=0; while [ $i -lt 1000 ]; do echo \"t$i: test\"; i=$((i+1)); done"), MIN, 1000).unwrap();
        assert!(listed.cut && listed.names.len() > 50 && listed.names.len() < 200, "{} names", listed.names.len());
        assert_eq!(listed.names.iter().next(), Some("t0"));
        let listed = list_tests(sh("echo 'a: test'; echo 'b: bench'; echo; echo '1 test, 1 benchmark'"), MIN, LIST_MAX_BYTES).unwrap();
        assert_eq!((listed.names.iter().collect::<Vec<_>>(), listed.cut), (vec!["a"], false));
        assert!(list_tests(sh("echo 'a: test'; exit 1"), MIN, LIST_MAX_BYTES).is_none(), "a failed --list lists nothing");
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_that_ends_in_time_passes_or_fails_as_before() {
        let run = |script: &str| run_test_binary(sh(script), "fake (sh)", Some(MIN));
        assert_eq!(run("echo running 0 tests"), Ok(()));
        assert_eq!(run("exit 3"), Err("exit status: 3".to_owned()));
        assert!(run("kill -9 $$").unwrap_err().contains("SIGKILL"));
    }

    #[cfg(unix)]
    #[test]
    fn under_cargo_each_binary_gets_the_limit_and_the_build_does_not_count() {
        // the "build" outlasts the limit, then each "binary" finishes in time
        let script = "sleep 1; \
            echo '     Running unittests src/lib.rs (target/debug/deps/a-1)' >&2; echo 'running 1 test'; echo 'test x ... ok'; \
            echo '     Running tests/it/main.rs (target/debug/deps/it-2)' >&2; echo 'running 0 tests'";
        assert_eq!(run_cargo_tests(sh(script), Some(Duration::from_millis(500))), Ok(()));

        let dir = scratch("cargo");
        let pidfile = dir.join("binary.pid");
        let script = format!(
            "echo '     Running tests/it/main.rs (target/debug/deps/it-2)' >&2; \
             sh -c 'echo running 2 tests; printf \"test slow ... \"; sleep 300' & echo $! > '{}'; wait",
            pidfile.display()
        );
        let mut shown = Vec::new();
        let err = cargo_tests(sh(&script), Some(Duration::from_secs(2)), &mut shown).unwrap_err();
        assert!(shown.first().is_some_and(|l| l.starts_with("timed out after 2 s: tests/it/main.rs")), "{shown:?}");
        assert_eq!(err, "timed out after 2 s (still running: slow): tests/it/main.rs (target/debug/deps/it-2)");
        let binary = std::fs::read_to_string(&pidfile).unwrap();
        assert!(wait_gone(binary.trim()), "the test binary cargo started was killed too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_quiet_cargo_configuration_still_starts_the_clock() {
        // a "cargo" that prints no Running line when quiet (-q or CARGO_TERM_QUIET=true), then
        // starts a binary that hangs before it prints anything
        let script = "quiet=; for a in \"$@\"; do [ \"$a\" = -q ] && quiet=1; done; [ \"$CARGO_TERM_QUIET\" = true ] && quiet=1; \
            [ -z \"$quiet\" ] && echo '     Running tests/it/main.rs (target/debug/deps/it-2)' >&2; sleep 300";
        let mut c = sh(script);
        c.args(["cargo", "test", "-q"]).env("CARGO_TERM_QUIET", "true");
        let t = Instant::now();
        let err = cargo_tests(c, Some(Duration::from_secs(1)), &mut Vec::new()).unwrap_err();
        assert!(t.elapsed() < Duration::from_secs(10) && err.starts_with("timed out after 1 s"), "{err} after {:?}", t.elapsed());

        // and without any Running line at all, the binary's own `running N tests` starts it
        let err = cargo_tests(sh("echo 'running 1 test'; sleep 300"), Some(Duration::from_secs(1)), &mut Vec::new()).unwrap_err();
        assert_eq!(err, "timed out after 1 s: a test binary");
    }

    #[cfg(unix)]
    #[test]
    fn no_limit_runs_as_before() {
        assert_eq!(run_cargo_tests(sh("exit 0"), None), Ok(()));
        assert_eq!(run_test_binary(sh("exit 4"), "fake", None), Err("exit status: 4".to_owned()));
    }
}
