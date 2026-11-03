//! Ending a run that ran out of time: its direct child and every process it started that xtask can
//! signal safely, and never a process xtask didn't start (#3060; the module docs of [`super`] say
//! how the processes are found).
//!
//! **No signal by a pid that may name another process by now.** A pid read from `/proc` passes to
//! another process as soon as its process exits and is reaped, and a parent that ignores `SIGCHLD`
//! has its children reaped by the kernel even while it is stopped. So the only process signalled
//! by its pid is the direct child, which xtask hasn't reaped yet. On Linux every other one is held
//! by a pidfd ([`Held`]), opened *before* what makes it ours is read again, and signalled only
//! through it, so if its pid passed to another process in between, the signal fails as "gone" and
//! reaches no one. What makes a process ours: the run's marker in its environment, or a parent
//! that is one of the held processes and hasn't exited (checked through the parent's pidfd after
//! the child's parent pid is read, so that pid can't have passed to another process meanwhile).
//! Where there are no pidfds (Unix other than Linux, a Linux kernel before 5.3), the processes the
//! child started aren't signalled, and the report says they may still run.
//!
//! **Nothing is left frozen, and nothing waits without a bound.** Every signal's result is kept. A
//! process whose SIGKILL fails is sent it once more, and if that fails too it is sent SIGCONT, so
//! it runs on rather than staying stopped (it was frozen only so it couldn't start more processes
//! while the others were found); the report names it either way, and says when the processes
//! couldn't all be looked for or held (out of file descriptors, say). The direct child (and
//! `taskkill` on Windows) is waited for a bounded time.

#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

use super::{Marker, TICK};

/// How long the direct child is waited for after the kill, and the others to exit.
const AFTER_KILL: Duration = Duration::from_secs(2);
/// How many rounds of looking for new processes (each a level of the tree) before killing.
#[cfg(target_os = "linux")]
const ROUNDS: usize = 64;

/// Kill everything a run started that can be signalled safely, reap its direct child, and say
/// what was done.
///
/// `child` is the run's direct child while it hasn't been reaped (so its pid is still its own):
/// it is signalled, plus what it started. `None` once it has been reaped: then only processes
/// carrying the marker are, never the old pid, which may belong to another process by now.
pub(super) fn kill_run(child: Option<&mut Child>, marker: &Marker) -> String {
    #[cfg(target_os = "linux")]
    let mut note = kill_all(child.as_ref().map(|c| c.id()), marker, &mut Real);
    #[cfg(not(target_os = "linux"))]
    let mut note = kill_all(child.as_ref().map(|c| c.id()), marker);
    if let Some(child) = child {
        // unreaped until the wait below succeeds: its pid is still its own
        let _ = child.kill();
        if wait_for(child, AFTER_KILL).is_none() {
            note.push_str(&format!("; it was still running {} s after the kill (pid {})", AFTER_KILL.as_secs(), child.id()));
        }
    }
    note
}

/// Wait at most `limit` for `child` to exit; its status if it did (and was reaped).
fn wait_for(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < until => std::thread::sleep(TICK),
            _ => return None,
        }
    }
}

/// What the kill says when it can signal only the direct child.
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn child_only(root: Option<u32>, why: &str) -> String {
    match root {
        Some(_) => format!("killed it; the processes it started weren't signalled ({why}), and may still run"),
        None => format!("the processes it started that outlived it weren't looked for ({why}), and may still run"),
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn kill_all(root: Option<u32>, _marker: &Marker) -> String {
    child_only(root, "this OS has no pidfds, and a bare pid may name another process by the time it is signalled")
}

#[cfg(windows)]
pub(super) fn kill_all(root: Option<u32>, _marker: &Marker) -> String {
    use std::process::{Command, Stdio};
    let Some(root) = root else {
        return "on this OS the processes it started that outlived it can't be found, and may still run".to_owned();
    };
    let spawned = Command::new("taskkill").args(["/PID", &root.to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    let mut taskkill = match spawned {
        Ok(t) => t,
        Err(e) => return format!("could not run taskkill ({e}): the processes it started may still run"),
    };
    match wait_for(&mut taskkill, Duration::from_secs(10)) {
        Some(s) if s.success() => "killed it and its process tree (taskkill /T); processes that outlived their parent may still run".to_owned(),
        Some(s) => format!("taskkill /T failed ({s}): the processes it started may still run"),
        None => {
            let _ = taskkill.kill();
            let _ = wait_for(&mut taskkill, AFTER_KILL);
            "taskkill /T didn't finish in 10 s: the processes it started may still run".to_owned()
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(super) fn kill_all(root: Option<u32>, _marker: &Marker) -> String {
    child_only(root, "not supported on this OS")
}

/// A signal sent while ending a run.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sig {
    Stop,
    Kill,
    Cont,
}

/// Why a signal wasn't delivered.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unsent {
    /// The process is gone (with a pidfd: it exited and was reaped, whatever holds its pid now).
    Gone,
    /// The signal failed: why.
    Failed(String),
}

/// A process to signal: its pid and a pidfd that refers to it alone, whatever its pid passes to
/// after it exits. `fd` is `None` only for the unreaped direct child, whose pid is its own.
#[cfg(target_os = "linux")]
pub(super) struct Held {
    pid: u32,
    /// Whether the kill stopped it (only those are continued when they can't be killed).
    frozen: bool,
    fd: Option<rustix::fd::OwnedFd>,
}

/// What [`Os::hold`] made of a pid that looked like one of the run's processes.
#[cfg(target_os = "linux")]
pub(super) enum Hold {
    /// Held, and what `/proc` said of it after that: its parent pid and whether it carries the marker.
    Held(Held, u32, bool),
    /// It exited, or it isn't this user's.
    Not,
    /// It can't be held or looked at, so it isn't signalled: why.
    Unheld(String),
}

/// A look through `/proc`: the process table (pid, parent pid) and the pids whose environment
/// holds the run's marker.
#[cfg(target_os = "linux")]
pub(super) type Scan = (Vec<(u32, u32)>, BTreeSet<u32>);

/// Where the kill looks for processes and how it signals them; tests replace a part.
#[cfg(target_os = "linux")]
pub(super) trait Os {
    fn scan(&mut self, entry: &str) -> Result<Scan, String> {
        proc_scan(entry)
    }

    /// Hold `pid` by a pidfd and read again what may make it ours.
    fn hold(&mut self, pid: u32, entry: &str) -> Hold {
        hold(pid, entry)
    }

    /// Hold the direct child (unreaped, so its pid is its own).
    fn hold_root(&mut self, pid: u32) -> Held {
        Held { pid, frozen: false, fd: open_pidfd(pid).ok() }
    }

    fn send(&mut self, held: &Held, sig: Sig) -> Result<(), Unsent> {
        send(held, sig)
    }

    /// Whether the process has exited (`None`: no pidfd to tell, so it's the unreaped child).
    fn exited(&mut self, held: &Held) -> Option<bool> {
        exited(held)
    }
}

/// The real `/proc` and signals.
#[cfg(target_os = "linux")]
pub(super) struct Real;

#[cfg(target_os = "linux")]
impl Os for Real {}

/// Freeze and kill `root` (unreaped) and what it started (see [`kill_run`]); what was done.
///
/// Everything found is frozen with SIGSTOP until no new one turns up, a level of the tree a round,
/// then killed with SIGKILL, the last found first. Then the failures are dealt with (module docs).
#[cfg(target_os = "linux")]
pub(super) fn kill_all(root: Option<u32>, marker: &Marker, os: &mut dyn Os) -> String {
    if !std::path::Path::new("/proc/self/environ").exists() {
        return child_only(root, "no /proc to find them in");
    }
    let entry = marker.entry();
    let mut end = Ending::default();
    let rounds = freeze(root, &entry, os, &mut end);
    let kill_failed = kill(os, &end.held);
    end.problems.extend(thaw(os, &end.held, &kill_failed));
    let mut alive = wait_exited(os, &end.held, &entry);
    // those are told of above
    alive.retain(|p| !kill_failed.iter().any(|(i, _)| end.held.get(*i).is_some_and(|h| h.pid == *p)));
    let started = end.held.iter().filter(|h| Some(h.pid) != root).count();
    let mut note = summary(root, started);
    if rounds == ROUNDS {
        note.push_str(&format!("; it kept starting processes, so xtask stopped looking after {ROUNDS} rounds"));
    }
    if let Some((n, why)) = end.unheld.first().map(|why| (end.unheld.len(), why)) {
        note.push_str(&format!("; {n} process(es) it started couldn't be held by a pidfd or looked at ({why}), so they weren't signalled"));
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
#[cfg(target_os = "linux")]
#[derive(Default)]
struct Ending {
    held: Vec<Held>,
    problems: Vec<String>,
    /// Why each process found that couldn't be held wasn't.
    unheld: Vec<String>,
}

/// Hold and stop `root` and then, a round at a time, every new process found; how many rounds ran.
#[cfg(target_os = "linux")]
fn freeze(root: Option<u32>, entry: &str, os: &mut dyn Os, end: &mut Ending) -> usize {
    let mut fresh: Vec<Held> = root.map(|pid| os.hold_root(pid)).into_iter().collect();
    // the stopped ones, whose children are looked for, and every pid already dealt with
    let (mut stopped, mut seen) = (BTreeSet::new(), BTreeSet::from([std::process::id()]));
    for round in 1..=ROUNDS {
        for mut held in fresh.drain(..) {
            match os.send(&held, Sig::Stop) {
                Ok(()) => {
                    held.frozen = true;
                    stopped.insert(held.pid);
                }
                // reaped (whatever holds its pid now): nothing to do
                Err(Unsent::Gone) => continue,
                // not frozen, but still ours: it is killed with the rest
                Err(Unsent::Failed(e)) => end.problems.push(format!("could not stop pid {} ({e})", held.pid)),
            }
            seen.insert(held.pid);
            end.held.push(held);
        }
        let (table, marked) = match os.scan(entry) {
            Ok(scan) => scan,
            Err(e) => {
                end.problems.push(format!("could not look for the processes it started ({e}), so some may still run"));
                return round;
            }
        };
        let mut found = children_of(&table, &stopped);
        found.extend(marked);
        found.retain(|p| !seen.contains(p));
        for pid in found {
            match os.hold(pid, entry) {
                Hold::Held(held, ppid, marked) if marked || has_live_parent(os, &end.held, ppid) => fresh.push(held),
                // not ours after all: its pid passed to another process since the scan
                Hold::Held(..) | Hold::Not => {}
                Hold::Unheld(why) => {
                    seen.insert(pid);
                    end.unheld.push(why);
                }
            }
        }
        if fresh.is_empty() {
            return round;
        }
    }
    ROUNDS
}

/// Whether `ppid`, read from a child's `/proc` after the child was held, is one of the held
/// processes: one that hasn't exited now, so it held that pid when it was read (a pid passes to
/// another process only once its process has exited and been reaped). The direct child, held
/// without a pidfd, is unreaped, so its pid is its own.
#[cfg(target_os = "linux")]
fn has_live_parent(os: &mut dyn Os, held: &[Held], ppid: u32) -> bool {
    held.iter().filter(|h| h.pid == ppid).any(|h| os.exited(h) != Some(true))
}

/// SIGKILL every held process, the last found first, and once more those it failed for; the
/// indexes (into `held`) and errors of the ones it failed for twice.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn wait_exited(os: &mut dyn Os, held: &[Held], entry: &str) -> Vec<u32> {
    let until = Instant::now() + AFTER_KILL;
    loop {
        let mut alive: BTreeSet<u32> = held.iter().filter(|h| os.exited(h) == Some(false)).map(|h| h.pid).collect();
        alive.extend(os.scan(entry).map(|(_, marked)| marked).unwrap_or_default());
        if alive.is_empty() || Instant::now() >= until {
            return alive.into_iter().collect();
        }
        std::thread::sleep(TICK);
    }
}

/// What the kill did, before its problems.
#[cfg(target_os = "linux")]
fn summary(root: Option<u32>, started: usize) -> String {
    match (root, started) {
        (Some(_), 0) => "killed it; it had started no other process (looked for in its process tree and by the run's environment marker)".to_owned(),
        (Some(_), n) => format!(
            "killed it and the {n} process(es) it started (found in its process tree and by the run's environment marker, so reparented ones too)"
        ),
        (None, 0) => "no process it started was left (looked for by the run's environment marker)".to_owned(),
        (None, n) => format!("killed the {n} process(es) it started that outlived it (found by the run's environment marker)"),
    }
}

#[cfg(target_os = "linux")]
fn send(held: &Held, sig: Sig) -> Result<(), Unsent> {
    use rustix::process::Signal;
    let sig = match sig {
        Sig::Stop => Signal::STOP,
        Sig::Kill => Signal::KILL,
        Sig::Cont => Signal::CONT,
    };
    let unsent = |e: rustix::io::Errno| if e == rustix::io::Errno::SRCH { Unsent::Gone } else { Unsent::Failed(e.to_string()) };
    if let Some(fd) = &held.fd {
        return rustix::process::pidfd_send_signal(fd, sig).map_err(unsent);
    }
    // no pidfd: only the unreaped direct child is held this way
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

#[cfg(target_os = "linux")]
fn open_pidfd(pid: u32) -> Result<rustix::fd::OwnedFd, rustix::io::Errno> {
    let pid = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw).ok_or(rustix::io::Errno::INVAL)?;
    rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
}

/// Hold `pid` by a pidfd, then read again what may make it ours: once the pidfd is open, a signal
/// through it reaches this process or none, so if the pid passed to another process before the
/// read, the stop that follows fails as gone.
#[cfg(target_os = "linux")]
fn hold(pid: u32, entry: &str) -> Hold {
    let fd = match open_pidfd(pid) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::SRCH) => return Hold::Not,
        Err(e) => return Hold::Unheld(format!("pidfd: {e}")),
    };
    match about(pid, entry) {
        Ok(Some((ppid, marked))) => Hold::Held(Held { pid, frozen: false, fd: Some(fd) }, ppid, marked),
        Ok(None) => Hold::Not,
        Err(e) => Hold::Unheld(e),
    }
}

/// The direct children of `parents` in a process table.
#[cfg(target_os = "linux")]
pub(super) fn children_of(table: &[(u32, u32)], parents: &BTreeSet<u32>) -> BTreeSet<u32> {
    table.iter().filter(|(pid, ppid)| parents.contains(ppid) && !parents.contains(pid)).map(|(pid, _)| *pid).collect()
}

/// This user's processes in `/proc`: the table (pid, parent pid) and the pids whose environment
/// holds exactly `entry`. An error when `/proc` can't be listed or a process can't be looked at
/// for a reason other than its exit (out of file descriptors, say).
#[cfg(target_os = "linux")]
fn proc_scan(entry: &str) -> Result<Scan, String> {
    let (mut table, mut marked) = (Vec::new(), BTreeSet::new());
    let dir = std::fs::read_dir("/proc").map_err(|e| format!("/proc: {e}"))?;
    for pid in dir.filter_map(Result::ok).filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok()) {
        if let Some((ppid, has)) = about(pid, entry)? {
            table.push((pid, ppid));
            if has {
                marked.insert(pid);
            }
        }
    }
    Ok((table, marked))
}

/// Whether an error reading a process's `/proc` files means it has gone (or is another user's).
#[cfg(target_os = "linux")]
fn gone(e: &std::io::Error) -> bool {
    // ESRCH: it exited while being read; EACCES / EPERM: another user's, or no longer dumpable
    matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied) || e.raw_os_error() == Some(3)
}

/// What `/proc` says of `pid` if it is this user's: its parent and whether its environment holds
/// exactly `entry` (a zombie's is empty). `None` when it has gone or is another user's; an error
/// when it can't be looked at for another reason.
#[cfg(target_os = "linux")]
fn about(pid: u32, entry: &str) -> Result<Option<(u32, bool)>, String> {
    use std::os::unix::fs::MetadataExt;
    let failed = |e: std::io::Error| if gone(&e) { Ok(None) } else { Err(format!("/proc/{pid}: {e}")) };
    let me = std::fs::metadata("/proc/self").map_err(|e| format!("/proc/self: {e}"))?.uid();
    match std::fs::metadata(format!("/proc/{pid}")) {
        Ok(m) if m.uid() == me => {}
        Ok(_) => return Ok(None),
        Err(e) => return failed(e),
    }
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    let Some(ppid) = parent_in_stat(&stat) else { return Ok(None) };
    let marked = match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(env) => super::has_entry(&env, entry),
        Err(e) if gone(&e) => false,
        Err(e) => return Err(format!("/proc/{pid}/environ: {e}")),
    };
    Ok(Some((ppid, marked)))
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
    proc_scan(entry).map(|(_, m)| m.into_iter().collect()).unwrap_or_default()
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

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_the_children_of_stopped_processes_in_a_process_table() {
        let table = [(1, 0), (10, 1), (11, 10), (12, 11), (13, 10), (20, 1), (21, 20), (14, 12)];
        assert_eq!(children_of(&table, &BTreeSet::from([10])), BTreeSet::from([11, 13]), "only direct children");
        assert_eq!(children_of(&table, &BTreeSet::from([10, 11, 13])), BTreeSet::from([12]));
        assert!(children_of(&table, &BTreeSet::from([99])).is_empty());
        assert!(children_of(&[(5, 6), (6, 5)], &BTreeSet::from([5, 6])).is_empty(), "a cycle ends");
    }

    /// The state letter of a process, or `None` once it is gone.
    #[cfg(target_os = "linux")]
    fn state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(')')?.1.split_whitespace().next()?.chars().next()
    }

    /// Kills (through a pidfd) and reaps what a test started when it drops, also when it fails.
    #[cfg(target_os = "linux")]
    #[derive(Default)]
    struct Cleanup {
        children: Vec<Child>,
        others: Vec<Held>,
        dir: Option<std::path::PathBuf>,
    }

    #[cfg(target_os = "linux")]
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for h in &self.others {
                let _ = send(h, Sig::Kill);
            }
            for c in &mut self.children {
                let _ = c.kill();
                let _ = wait_for(c, Duration::from_secs(5));
            }
            if let Some(dir) = &self.dir {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// `sh` running `script` with the marker, and the pid it writes to PIDFILE (waited for, and
    /// held so the cleanup can kill it).
    #[cfg(target_os = "linux")]
    fn marked_sh(script: &str, name: &str, cleanup: &mut Cleanup) -> (u32, Marker, u32) {
        let dir = std::env::temp_dir().join(format!("xtask-kill-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        cleanup.dir = Some(dir.clone());
        let pidfile = dir.join("pid");
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", &script.replace("PIDFILE", &pidfile.display().to_string())]).stdin(std::process::Stdio::null());
        let (child, marker) = super::super::spawn_marked(&mut c).unwrap();
        let root = child.id();
        cleanup.children.push(child);
        let until = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            if let Some(pid) = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse().ok()) {
                break pid;
            }
            assert!(Instant::now() < until, "the script wrote no pid");
            std::thread::sleep(TICK);
        };
        let held = Real.hold_root(pid);
        assert!(held.fd.is_some(), "this kernel has pidfds (Linux 5.3)");
        cleanup.others.push(held);
        (root, marker, pid)
    }

    /// A process with no marker, for the kill to leave alone.
    #[cfg(target_os = "linux")]
    fn decoy(cleanup: &mut Cleanup) -> u32 {
        let c = std::process::Command::new("sleep").arg("300").stdin(std::process::Stdio::null()).spawn().unwrap();
        let pid = c.id();
        cleanup.children.push(c);
        pid
    }

    /// The real OS, except that SIGKILL fails for every process but the direct child.
    #[cfg(target_os = "linux")]
    struct KillFails {
        root: u32,
        refused: Vec<u32>,
    }

    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_whose_kill_fails_is_not_left_stopped() {
        let mut cleanup = Cleanup::default();
        let (root, marker, grandchild) = marked_sh("sleep 300 & echo $! > PIDFILE; wait", "thaw", &mut cleanup);
        let mut os = KillFails { root, refused: Vec::new() };
        let note = kill_all(Some(root), &marker, &mut os);
        let s = state(grandchild);
        assert!(s.is_some_and(|s| s != 'T'), "the grandchild was continued, not left stopped: state {s:?}");
        assert_eq!(os.refused.iter().filter(|p| **p == grandchild).count(), 2, "tried twice: {:?}", os.refused);
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
        fn scan(&mut self, entry: &str) -> Result<Scan, String> {
            let (table, mut marked) = proc_scan(entry)?;
            marked.extend(self.decoy.take());
            Ok((table, marked))
        }
    }

    /// A pid found by its marker that names another process by the time it is held is never
    /// signalled: what makes it ours is read again once it is held by a pidfd (the old code
    /// stopped and killed whatever the pid named by then).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pid_that_passed_to_another_process_is_not_signalled() {
        let mut cleanup = Cleanup::default();
        let decoy = decoy(&mut cleanup);
        let (root, marker, grandchild) = marked_sh("sleep 300 & echo $! > PIDFILE; wait", "reused", &mut cleanup);
        let note = kill_all(Some(root), &marker, &mut PidReused { decoy: Some(decoy) });
        let s = state(decoy);
        assert!(s.is_some_and(|s| s != 'T' && s != 'Z'), "the decoy runs on: state {s:?}");
        assert!(state(grandchild).is_none_or(|s| s == 'Z'), "the run's own processes were killed");
        assert!(note.starts_with("killed it and the 1 process(es) it started") && !note.contains("still alive"), "{note}");
    }

    /// The real OS, except that the first scan also reports `decoy` as a child of `parent`, as if
    /// `parent` (held, then exited) had been reaped and its pid passed to the decoy's parent.
    #[cfg(target_os = "linux")]
    struct ParentReused {
        decoy: u32,
        parent: u32,
        once: bool,
    }

    #[cfg(target_os = "linux")]
    impl Os for ParentReused {
        fn scan(&mut self, entry: &str) -> Result<Scan, String> {
            let (mut table, marked) = proc_scan(entry)?;
            if std::mem::take(&mut self.once) {
                table.push((self.decoy, self.parent));
            }
            Ok((table, marked))
        }

        fn hold(&mut self, pid: u32, entry: &str) -> Hold {
            match hold(pid, entry) {
                Hold::Held(h, _, marked) if pid == self.decoy => Hold::Held(h, self.parent, marked),
                other => other,
            }
        }
    }

    /// A child is ours by its parent only while that parent hasn't exited: a stop that succeeded
    /// once doesn't prove it (it succeeds on a zombie), and an exited parent's pid can pass to
    /// another process, whose children are not ours.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_children_of_a_parent_that_exited_are_not_trusted() {
        let mut cleanup = Cleanup::default();
        let decoy = decoy(&mut cleanup);
        // a direct child that has exited but isn't reaped: a zombie, held by its pidfd
        let mut c = std::process::Command::new("true");
        let (child, marker) = super::super::spawn_marked(&mut c).unwrap();
        let root = child.id();
        cleanup.children.push(child);
        let until = Instant::now() + Duration::from_secs(10);
        while state(root) != Some('Z') {
            assert!(Instant::now() < until, "`true` didn't exit");
            std::thread::sleep(TICK);
        }
        let note = kill_all(Some(root), &marker, &mut ParentReused { decoy, parent: root, once: true });
        let s = state(decoy);
        assert!(s.is_some_and(|s| s != 'T' && s != 'Z'), "the decoy runs on: state {s:?}");
        assert!(note.starts_with("killed it; it had started no other process"), "{note}");
    }

    /// A pidfd refers to its process alone: once that process has exited and been reaped, a
    /// signal through it fails as gone, whatever holds the pid now.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_held_process_that_was_reaped_gets_no_signal() {
        let mut cleanup = Cleanup::default();
        let pid = decoy(&mut cleanup);
        let held = Real.hold_root(pid);
        assert!(held.fd.is_some(), "this kernel has pidfds (Linux 5.3)");
        assert_eq!(exited(&held), Some(false));
        let mut c = cleanup.children.pop().unwrap();
        c.kill().unwrap();
        c.wait().unwrap();
        assert_eq!(exited(&held), Some(true));
        assert_eq!(send(&held, Sig::Stop), Err(Unsent::Gone));
    }
}
