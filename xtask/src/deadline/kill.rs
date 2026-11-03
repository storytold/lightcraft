//! Ending a run that ran out of time: its direct child and every process it started, and never a
//! process xtask didn't start (#3060; the module docs of [`super`] say how the processes are found).
//!
//! **No signal by a pid that may name another process by now.** A pid read from the process table
//! or `/proc` passes to another process as soon as its process exits and is reaped. On Linux each
//! process is therefore held by a pidfd ([`Held`]), opened *before* what makes it ours is read again
//! (its parent is one of the stopped processes, or its environment holds the run's marker), and
//! every signal goes through that pidfd: if the process exited and its pid was reused in between,
//! the signal fails as "gone" and reaches no one. Elsewhere on Unix (no pidfds; no marker lookup
//! either) a process is signalled by its pid only once its parent is stopped and so can't reap it;
//! a parent that ignores `SIGCHLD` has its children reaped by the kernel all the same, which is the
//! one gap left there.
//!
//! **Nothing is left frozen.** Every signal's result is kept. A process whose SIGKILL fails is sent
//! it once more, and if that fails too it is sent SIGCONT, so it runs on rather than staying
//! stopped (it was frozen only so it couldn't start more processes while the others were found);
//! the report names it either way. The direct child is waited for a bounded time.

use std::collections::BTreeSet;
#[cfg(unix)]
use std::process::Stdio;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use super::{Marker, TICK};

/// How long the direct child is waited for after the kill, and the others to exit.
const AFTER_KILL: Duration = Duration::from_secs(2);
/// How many rounds of looking for new processes (each a level of the tree) before killing.
#[cfg(unix)]
const ROUNDS: usize = 64;

/// Kill everything a run started, reap its direct child, and say what was done.
///
/// `child` is the run's direct child while it hasn't been reaped (so its pid is still its own):
/// it and its process tree are signalled, plus every process carrying the run's `marker`. `None`
/// once it has been reaped: then only processes carrying the marker are, never the old pid, which
/// may belong to another process by now.
pub(super) fn kill_run(child: Option<&mut Child>, marker: &Marker) -> String {
    #[cfg(unix)]
    let mut note = kill_all(child.as_ref().map(|c| c.id()), marker, &mut Real);
    #[cfg(not(unix))]
    let mut note = kill_all(child.as_ref().map(|c| c.id()), marker);
    if let Some(child) = child {
        // unreaped until the wait below succeeds: its pid is still its own
        let _ = child.kill();
        if !reaped(child, AFTER_KILL) {
            note.push_str(&format!("; it was still running {} s after the kill (pid {})", AFTER_KILL.as_secs(), child.id()));
        }
    }
    note
}

/// Wait at most `limit` for `child` to exit; whether it did (and was reaped).
fn reaped(child: &mut Child, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < until => std::thread::sleep(TICK),
            _ => return false,
        }
    }
}

/// Whether this OS lets xtask find a run's processes by their marker (Linux: `/proc/*/environ`).
#[cfg_attr(not(unix), allow(dead_code))]
fn can_find_marked() -> bool {
    cfg!(target_os = "linux") && std::path::Path::new("/proc/self/environ").exists()
}

/// A signal sent while ending a run.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sig {
    Stop,
    Kill,
    Cont,
}

/// Why a signal wasn't delivered.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unsent {
    /// The process is gone (with a pidfd: it exited, whatever holds its pid now).
    Gone,
    /// The signal failed: why.
    Failed(String),
}

/// A process to signal: its pid and, on Linux, a pidfd that refers to it alone, whatever its pid
/// passes to after it exits. `fd` is `None` only for a process whose parent is stopped (or the
/// unreaped direct child), whose pid can't pass to another process meanwhile.
#[cfg(unix)]
pub(super) struct Held {
    pid: u32,
    /// Whether the kill stopped it (only those are continued when they can't be killed).
    frozen: bool,
    #[cfg(target_os = "linux")]
    fd: Option<rustix::fd::OwnedFd>,
}

#[cfg(unix)]
impl Held {
    /// By its pid alone: only for a process whose pid can't be reused meanwhile.
    fn by_pid(pid: u32) -> Self {
        Self {
            pid,
            frozen: false,
            #[cfg(target_os = "linux")]
            fd: None,
        }
    }
}

/// What [`Os::hold`] made of a pid that looked like one of the run's processes.
#[cfg(unix)]
pub(super) enum Hold {
    Held(Held),
    /// It exited, or what made it look like ours no longer holds (its pid passed to another process).
    Not,
    /// It still looks like ours but can't be held, so it isn't signalled: why.
    Unheld(String),
}

/// Where the kill looks for processes and how it signals them; tests replace a part.
#[cfg(unix)]
pub(super) trait Os {
    /// The process table (pid, parent pid) and the pids whose environment holds `entry` (on
    /// Linux with `by_marker`; else none).
    fn scan(&mut self, entry: &str, by_marker: bool) -> (Vec<(u32, u32)>, BTreeSet<u32>) {
        scan(entry, by_marker)
    }

    /// Hold `pid`, found as a child of `stopped` or by `entry`, if it still is one.
    fn hold(&mut self, pid: u32, entry: &str, stopped: &BTreeSet<u32>, by_marker: bool) -> Hold {
        hold(pid, entry, stopped, by_marker)
    }

    /// Hold the direct child (unreaped, so its pid is its own).
    fn hold_root(&mut self, pid: u32) -> Held {
        hold_root(pid)
    }

    fn send(&mut self, held: &Held, sig: Sig) -> Result<(), Unsent> {
        send(held, sig)
    }

    /// Whether the process has exited (`None`: this OS can't tell without its pid).
    fn exited(&mut self, held: &Held) -> Option<bool> {
        exited(held)
    }
}

/// The real process table and signals.
#[cfg(unix)]
pub(super) struct Real;

#[cfg(unix)]
impl Os for Real {}

/// Freeze and kill `root` (unreaped) and what it started (see [`kill_run`]); what was done.
///
/// Everything found is frozen with SIGSTOP until no new one turns up, a level of the tree a round,
/// then killed with SIGKILL, the last found first. Then the failures are dealt with (module docs).
#[cfg(unix)]
pub(super) fn kill_all(root: Option<u32>, marker: &Marker, os: &mut dyn Os) -> String {
    let entry = marker.entry();
    let by_marker = can_find_marked();
    let mut end = Ending::default();
    let found = freeze(root, &entry, by_marker, os, &mut end);
    let kill_failed = kill(os, &end.held);
    end.problems.extend(thaw(os, &end.held, &kill_failed));
    let mut alive = if by_marker { wait_exited(os, &end.held, &entry) } else { Vec::new() };
    // those are told of above
    alive.retain(|p| !kill_failed.iter().any(|(i, _)| end.held.get(*i).is_some_and(|h| h.pid == *p)));
    let started = end.held.iter().filter(|h| Some(h.pid) != root).count();
    let mut note = summary(root, started, by_marker);
    if found == ROUNDS {
        note.push_str(&format!("; it kept starting processes, so xtask stopped looking after {ROUNDS} rounds"));
    }
    if !alive.is_empty() {
        let pids: Vec<String> = alive.iter().map(u32::to_string).collect();
        end.problems.push(format!("still alive after the kill: pid {} (they carry {})", pids.join(", "), marker.key));
    }
    for problem in &end.problems {
        note.push_str("; ");
        note.push_str(problem);
    }
    note
}

/// The processes held so far, in the order found, and what went wrong on the way.
#[cfg(unix)]
#[derive(Default)]
struct Ending {
    held: Vec<Held>,
    problems: Vec<String>,
}

/// Hold and stop `root` and then, a round at a time, every new process found; how many rounds ran.
#[cfg(unix)]
fn freeze(root: Option<u32>, entry: &str, by_marker: bool, os: &mut dyn Os, end: &mut Ending) -> usize {
    let me = std::process::id();
    let mut fresh: Vec<Held> = root.map(|pid| os.hold_root(pid)).into_iter().collect();
    // the stopped ones, whose children can't be reaped meanwhile, and every pid already dealt with
    let (mut stopped, mut seen) = (BTreeSet::new(), BTreeSet::from([me]));
    for round in 1..=ROUNDS {
        for mut held in fresh.drain(..) {
            match os.send(&held, Sig::Stop) {
                Ok(()) => {
                    held.frozen = true;
                    stopped.insert(held.pid);
                }
                // exited (or, with a pidfd, its pid passed to another process): nothing to do
                Err(Unsent::Gone) => continue,
                // not frozen, but still ours: it is killed with the rest
                Err(Unsent::Failed(e)) => end.problems.push(format!("could not stop pid {} ({e})", held.pid)),
            }
            seen.insert(held.pid);
            end.held.push(held);
        }
        let (table, marked) = os.scan(entry, by_marker);
        let mut found = children_of(&table, &stopped);
        found.extend(marked);
        found.retain(|p| !seen.contains(p));
        for pid in found {
            match os.hold(pid, entry, &stopped, by_marker) {
                Hold::Held(held) => fresh.push(held),
                Hold::Not => {}
                Hold::Unheld(why) => {
                    seen.insert(pid);
                    end.problems.push(format!("pid {pid} carries the run's marker but couldn't be held ({why}), so it wasn't signalled"));
                }
            }
        }
        if fresh.is_empty() {
            return round;
        }
    }
    ROUNDS
}

/// SIGKILL every held process, the last found first, and once more those it failed for; the
/// indexes (into `held`) and errors of the ones it failed for twice.
#[cfg(unix)]
fn kill(os: &mut dyn Os, held: &[Held]) -> Vec<(usize, String)> {
    let mut failed: Vec<usize> = Vec::new();
    for (i, h) in held.iter().enumerate().rev() {
        if let Err(Unsent::Failed(_)) = os.send(h, Sig::Kill) {
            failed.push(i);
        }
    }
    failed
        .into_iter()
        .filter_map(|i| match os.send(held.get(i)?, Sig::Kill) {
            Ok(()) | Err(Unsent::Gone) => None,
            Err(Unsent::Failed(e)) => Some((i, e)),
        })
        .collect()
}

/// SIGCONT the processes the kill stopped and SIGKILL failed for, so none stays stopped; what to
/// report of each.
///
/// Still ours to signal: a stopped process can't exit by itself, so its pid stays its own even
/// without a pidfd.
#[cfg(unix)]
fn thaw(os: &mut dyn Os, held: &[Held], failed: &[(usize, String)]) -> Vec<String> {
    failed
        .iter()
        .filter_map(|(i, e)| {
            let h = held.get(*i)?;
            if !h.frozen {
                return Some(format!("could not kill pid {} ({e}); it may still run", h.pid));
            }
            Some(match os.send(h, Sig::Cont) {
                Ok(()) => format!("could not kill pid {} ({e}), so it was continued with SIGCONT rather than left stopped", h.pid),
                Err(Unsent::Gone) => format!("could not kill pid {} ({e}), but it has exited", h.pid),
                Err(Unsent::Failed(c)) => format!("could not kill pid {} ({e}) nor continue it ({c}): it may be left stopped", h.pid),
            })
        })
        .collect()
}

/// Wait at most [`AFTER_KILL`] for the held processes to exit; the pids still alive then (held ones
/// that haven't exited, and any that still carry `entry`: a killed process loses its environment
/// as it dies, before it is reaped).
#[cfg(unix)]
fn wait_exited(os: &mut dyn Os, held: &[Held], entry: &str) -> Vec<u32> {
    let until = Instant::now() + AFTER_KILL;
    loop {
        let mut alive: BTreeSet<u32> = held.iter().filter(|h| os.exited(h) == Some(false)).map(|h| h.pid).collect();
        alive.extend(os.scan(entry, true).1);
        if alive.is_empty() || Instant::now() >= until {
            return alive.into_iter().collect();
        }
        std::thread::sleep(TICK);
    }
}

/// What the kill did, before its problems.
#[cfg(unix)]
fn summary(root: Option<u32>, started: usize, by_marker: bool) -> String {
    if !by_marker {
        return match root {
            Some(_) => format!(
                "killed it and the {started} process(es) it started that were still its descendants; on this OS processes that \
                 outlived their parent can't be found, and may still run"
            ),
            None => "on this OS the processes it started that outlived it can't be found, and may still run".to_owned(),
        };
    }
    match (root, started) {
        (Some(_), 0) => "killed it; it had started no other process (looked for in its process tree and by the run's environment marker)".to_owned(),
        (Some(_), n) => format!(
            "killed it and the {n} process(es) it started (found in its process tree and by the run's environment marker, so reparented ones too)"
        ),
        (None, 0) => "no process it started was left (looked for by the run's environment marker)".to_owned(),
        (None, n) => format!("killed the {n} process(es) it started that outlived it (found by the run's environment marker)"),
    }
}

#[cfg(windows)]
pub(super) fn kill_all(root: Option<u32>, _marker: &Marker) -> String {
    let Some(root) = root else {
        return "on this OS the processes it started that outlived it can't be found, and may still run".to_owned();
    };
    let _ = Command::new("taskkill")
        .args(["/PID", &root.to_string(), "/T", "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    "killed it and its process tree (taskkill /T); processes that outlived their parent may still run".to_owned()
}

#[cfg(not(any(unix, windows)))]
pub(super) fn kill_all(_root: Option<u32>, _marker: &Marker) -> String {
    "killed it; processes it started may still run".to_owned()
}

#[cfg(unix)]
fn send(held: &Held, sig: Sig) -> Result<(), Unsent> {
    use rustix::process::Signal;
    let sig = match sig {
        Sig::Stop => Signal::STOP,
        Sig::Kill => Signal::KILL,
        Sig::Cont => Signal::CONT,
    };
    let unsent = |e: rustix::io::Errno| if e == rustix::io::Errno::SRCH { Unsent::Gone } else { Unsent::Failed(e.to_string()) };
    #[cfg(target_os = "linux")]
    if let Some(fd) = &held.fd {
        return rustix::process::pidfd_send_signal(fd, sig).map_err(unsent);
    }
    let pid = i32::try_from(held.pid).ok().and_then(rustix::process::Pid::from_raw).ok_or_else(|| Unsent::Failed("not a pid".to_owned()))?;
    rustix::process::kill_process(pid, sig).map_err(unsent)
}

#[cfg(target_os = "linux")]
fn exited(held: &Held) -> Option<bool> {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let fd = held.fd.as_ref()?;
    let mut fds = [PollFd::new(fd, PollFlags::IN)];
    // a pidfd is readable once its process has exited
    let now = Timespec { tv_sec: 0, tv_nsec: 0 };
    rustix::event::poll(&mut fds, Some(&now)).ok()?;
    Some(fds.first().is_some_and(|f| !f.revents().is_empty()))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn exited(_held: &Held) -> Option<bool> {
    None
}

#[cfg(target_os = "linux")]
fn hold_root(pid: u32) -> Held {
    // the unreaped child's pid is its own, so a pidfd from it is too; without one its pid serves
    Held { pid, frozen: false, fd: open_pidfd(pid).ok() }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn hold_root(pid: u32) -> Held {
    Held::by_pid(pid)
}

#[cfg(target_os = "linux")]
fn open_pidfd(pid: u32) -> Result<rustix::fd::OwnedFd, rustix::io::Errno> {
    let pid = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw).ok_or(rustix::io::Errno::INVAL)?;
    rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
}

/// Hold `pid` by a pidfd, then read again what makes it ours: once the pidfd is open, a signal
/// through it reaches this process or none, so if the pid passed to another process before the
/// read, the stop that follows fails as gone.
#[cfg(target_os = "linux")]
fn hold(pid: u32, entry: &str, stopped: &BTreeSet<u32>, by_marker: bool) -> Hold {
    let ours = |about: Option<(u32, bool)>| about.is_some_and(|(ppid, marked)| marked || stopped.contains(&ppid));
    if !by_marker {
        // no /proc to read again: only a child of a stopped process, found in the process table
        return Hold::Held(Held::by_pid(pid));
    }
    match open_pidfd(pid) {
        Ok(fd) if ours(about(pid, entry)) => Hold::Held(Held { pid, frozen: false, fd: Some(fd) }),
        Ok(_) | Err(rustix::io::Errno::SRCH) => Hold::Not,
        // no pidfd (an old kernel, out of descriptors): a child of a stopped process can't be
        // reaped meanwhile, so its pid serves; one found only by its marker is left alone
        Err(e) => match about(pid, entry) {
            Some((ppid, _)) if stopped.contains(&ppid) => Hold::Held(Held::by_pid(pid)),
            Some((_, true)) => Hold::Unheld(e.to_string()),
            _ => Hold::Not,
        },
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn hold(pid: u32, _entry: &str, _stopped: &BTreeSet<u32>, _by_marker: bool) -> Hold {
    // found as a child of a stopped process, which can't reap it meanwhile
    Hold::Held(Held::by_pid(pid))
}

/// The process table, and the pids whose environment holds `entry`: from `/proc` on Linux (with
/// `by_marker`), else the table from `ps` and no marked pids.
#[cfg(unix)]
fn scan(entry: &str, by_marker: bool) -> (Vec<(u32, u32)>, BTreeSet<u32>) {
    #[cfg(target_os = "linux")]
    if by_marker {
        return proc_scan(entry);
    }
    let _ = (entry, by_marker);
    (process_table(), BTreeSet::new())
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
pub(super) fn parse_process_table(table: &str) -> Vec<(u32, u32)> {
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
pub(super) fn children_of(table: &[(u32, u32)], parents: &BTreeSet<u32>) -> BTreeSet<u32> {
    table.iter().filter(|(pid, ppid)| parents.contains(ppid) && !parents.contains(pid)).map(|(pid, _)| *pid).collect()
}

/// This user's processes in `/proc`: the table (pid, parent pid) and the pids whose environment
/// holds exactly `entry`.
#[cfg(target_os = "linux")]
fn proc_scan(entry: &str) -> (Vec<(u32, u32)>, BTreeSet<u32>) {
    let (mut table, mut marked) = (Vec::new(), BTreeSet::new());
    for pid in proc_pids() {
        if let Some((ppid, has)) = about(pid, entry) {
            table.push((pid, ppid));
            if has {
                marked.insert(pid);
            }
        }
    }
    (table, marked)
}

/// The pids in `/proc`.
#[cfg(target_os = "linux")]
fn proc_pids() -> Vec<u32> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.filter_map(Result::ok).filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok()).collect()
}

/// What `/proc` says of `pid` if it is this user's: its parent and whether its environment holds
/// exactly `entry` (a process that can't be read has none, and neither has a zombie).
#[cfg(target_os = "linux")]
fn about(pid: u32, entry: &str) -> Option<(u32, bool)> {
    use std::os::unix::fs::MetadataExt;
    let me = std::fs::metadata("/proc/self").ok()?.uid();
    if std::fs::metadata(format!("/proc/{pid}")).ok()?.uid() != me {
        return None;
    }
    let ppid = parent_in_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
    let marked = std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|env| super::has_entry(&env, entry));
    Some((ppid, marked))
}

/// The parent pid in a `/proc/<pid>/stat` line: `pid (comm) state ppid …`, where `comm` may hold
/// spaces and parentheses.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn parent_in_stat(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// This user's processes whose environment holds exactly `entry`.
#[cfg(all(test, target_os = "linux"))]
pub(super) fn marked(entry: &str) -> Vec<u32> {
    proc_scan(entry).1.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parent_is_read_past_any_parenthesis_in_the_command_name() {
        assert_eq!(parent_in_stat("42 (sleep) S 7 42 42 0 -1"), Some(7));
        assert_eq!(parent_in_stat("42 (a) b) (c) R 9 1"), Some(9));
        assert_eq!(parent_in_stat("42 (sh) Z"), None);
        assert_eq!(parent_in_stat("garbage"), None);
    }

    /// The state letter of a process (`ps`'s first `stat` letter), or `None` once it is gone.
    #[cfg(unix)]
    fn state(pid: u32) -> Option<char> {
        let out = Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().ok()?;
        String::from_utf8_lossy(&out.stdout).trim().chars().next()
    }

    /// `sh` running `script` with the marker, and the pid it writes to a file (waited for).
    #[cfg(unix)]
    fn marked_sh(script: &str, name: &str) -> (Child, Marker, u32, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("xtask-kill-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pid");
        let mut c = Command::new("/bin/sh");
        c.args(["-c", &script.replace("PIDFILE", &pidfile.display().to_string())]).stdin(Stdio::null());
        let (child, marker) = super::super::spawn_marked(&mut c).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            if let Some(pid) = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse().ok()) {
                break pid;
            }
            assert!(Instant::now() < until, "the script wrote no pid");
            std::thread::sleep(TICK);
        };
        (child, marker, pid, dir)
    }

    /// The real OS, except that SIGKILL fails for every process but the direct child.
    #[cfg(unix)]
    struct KillFails {
        root: u32,
        refused: Vec<u32>,
    }

    #[cfg(unix)]
    impl Os for KillFails {
        fn send(&mut self, held: &Held, sig: Sig) -> Result<(), Unsent> {
            if sig == Sig::Kill && held.pid != self.root {
                self.refused.push(held.pid);
                return Err(Unsent::Failed("refused by the test".to_owned()));
            }
            send(held, sig)
        }
    }

    /// When SIGKILL fails, the processes the kill froze are continued, not left stopped, and the
    /// report says so (the grandchild used to stay in state `T`).
    #[cfg(unix)]
    #[test]
    fn a_process_whose_kill_fails_is_not_left_stopped() {
        let (mut child, marker, grandchild, dir) = marked_sh("sleep 300 & echo $! > PIDFILE; wait", "thaw");
        let mut os = KillFails { root: child.id(), refused: Vec::new() };
        let note = kill_all(Some(child.id()), &marker, &mut os);
        assert_eq!(os.refused.iter().filter(|p| **p == grandchild).count(), 2, "tried twice: {:?}", os.refused);
        let s = state(grandchild);
        // clean up before asserting: the grandchild runs on, as it should
        let _ = Command::new("kill").args(["-KILL", &grandchild.to_string()]).status();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(s.is_some_and(|s| s != 'T'), "the grandchild was continued, not left stopped: state {s:?}");
        assert!(
            note.contains(&format!(
                "could not kill pid {grandchild} (refused by the test), so it was continued with SIGCONT rather than left stopped"
            )),
            "{note}"
        );
    }

    /// The real OS, except that the first scan also reports `decoy` as carrying the marker, as if
    /// the marked process it read had exited and its pid passed to `decoy` before it was signalled.
    #[cfg(target_os = "linux")]
    struct PidReused {
        decoy: Option<u32>,
    }

    #[cfg(target_os = "linux")]
    impl Os for PidReused {
        fn scan(&mut self, entry: &str, by_marker: bool) -> (Vec<(u32, u32)>, BTreeSet<u32>) {
            let (table, mut marked) = scan(entry, by_marker);
            marked.extend(self.decoy.take());
            (table, marked)
        }
    }

    /// A pid found by its marker that names another process by the time it is held is never
    /// signalled: what makes it ours is read again once it is held by a pidfd (the old code
    /// stopped and killed whatever the pid named by then).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pid_that_passed_to_another_process_is_not_signalled() {
        let mut decoy = Command::new("sleep").arg("300").stdin(Stdio::null()).spawn().unwrap();
        let (mut child, marker, grandchild, dir) = marked_sh("sleep 300 & echo $! > PIDFILE; wait", "reused");
        let note = kill_all(Some(child.id()), &marker, &mut PidReused { decoy: Some(decoy.id()) });
        let decoy_state = state(decoy.id());
        let _ = decoy.kill();
        let _ = decoy.wait();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(decoy_state.is_some_and(|s| s != 'T' && s != 'Z'), "the decoy runs on: state {decoy_state:?}");
        assert!(state(grandchild).is_none_or(|s| s == 'Z'), "the run's own processes were killed");
        assert!(note.starts_with("killed it and the 1 process(es) it started") && !note.contains("still alive"), "{note}");
    }

    /// A pidfd refers to its process alone: once that process has exited and been reaped, a
    /// signal through it fails as gone, whatever holds the pid now.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_held_process_that_was_reaped_gets_no_signal() {
        let mut c = Command::new("sleep").arg("300").spawn().unwrap();
        let held = hold_root(c.id());
        assert!(held.fd.is_some(), "this kernel has pidfds");
        assert_eq!(exited(&held), Some(false));
        c.kill().unwrap();
        c.wait().unwrap();
        assert_eq!(exited(&held), Some(true));
        assert_eq!(send(&held, Sig::Stop), Err(Unsent::Gone));
    }
}
