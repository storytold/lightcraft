//! Ending a run that ran out of time: its direct child and every process it started that xtask can
//! signal safely, and never a process xtask didn't start (#3060; the module docs of [`super`] say
//! how the processes are found).
//!
//! **No signal by a pid that may name another process by now.** A pid read from `/proc` passes to
//! another process as soon as its process exits and is reaped, and a parent that ignores `SIGCHLD`
//! has its children reaped by the kernel even while it is stopped. So the only process signalled
//! by its pid is the direct child, which xtask hasn't reaped yet, and only while xtask doesn't
//! ignore `SIGCHLD` itself (inherited from whatever started it; then the kernel reaps xtask's
//! children too, and on Linux the direct child is treated like the others below). On Linux every
//! other one is held by a pidfd ([`Held`]), opened *before* what makes it ours is read again, and
//! signalled only through it, so if its pid passed to another process in between, the signal fails
//! as "gone" and reaches no one. What makes a process ours: the run's marker in its environment, or
//! a parent that is one of the held processes and that its pidfd says hasn't exited (asked after
//! the child's parent pid is read, so that pid can't have passed to another process meanwhile; when
//! it can't be asked, the child isn't trusted). Where there are no pidfds (Unix other than Linux, a
//! Linux kernel before 5.3), only the direct child is killed; under the full `ci` that is cargo,
//! so the test binary it started may keep running, and the report says so.
//!
//! **Nothing waits without a bound, and a frozen process is continued if it can't be killed.**
//! Every signal's result is kept. A process whose SIGKILL fails is sent it once more, and if that
//! fails too it is sent SIGCONT so that it runs on rather than staying stopped (it was frozen only
//! so it couldn't start more processes while the others were found); the report names it either
//! way, says if SIGCONT failed too, and says when the processes couldn't all be looked at or held
//! (out of file descriptors, say). The direct child (and `taskkill` on Windows) is waited for a
//! bounded time.

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
    // whether the direct child is unreaped until xtask waits for it, so its pid is its own
    #[cfg(target_os = "linux")]
    let own = !sigchld_ignored().unwrap_or(false);
    #[cfg(target_os = "linux")]
    let mut note = kill_all(child.as_ref().map(|c| c.id()), own, marker, &mut Real);
    #[cfg(not(target_os = "linux"))]
    let (own, mut note) = (true, kill_all(child.as_ref().map(|c| c.id()), marker));
    if let Some(child) = child {
        if own {
            let _ = child.kill();
        }
        if wait_for(child, AFTER_KILL).is_none() && own {
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

/// Whether xtask ignores `SIGCHLD` (so the kernel reaps its children without a wait), from
/// `/proc/self/status`; `None` when that can't be read.
#[cfg(target_os = "linux")]
fn sigchld_ignored() -> Option<bool> {
    ignores_sigchld(&std::fs::read_to_string("/proc/self/status").ok()?)
}

/// Whether a `/proc/<pid>/status` text's `SigIgn` mask holds `SIGCHLD`.
#[cfg(target_os = "linux")]
fn ignores_sigchld(status: &str) -> Option<bool> {
    let mask = status.lines().find_map(|l| l.strip_prefix("SigIgn:"))?;
    let mask = u64::from_str_radix(mask.trim(), 16).ok()?;
    let bit = u32::try_from(rustix::process::Signal::CHILD.as_raw()).ok()?.checked_sub(1)?;
    Some(mask.checked_shr(bit).is_some_and(|m| m & 1 == 1))
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
    child_only(
        root,
        "this OS has no pidfds, and a bare pid may name another process by the time it is signalled; under cargo, the test binary is one of them",
    )
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

/// A look through `/proc`.
#[cfg(target_os = "linux")]
#[derive(Default)]
pub(super) struct Scan {
    /// The process table: pid, parent pid.
    table: Vec<(u32, u32)>,
    /// The pids whose environment holds the run's marker.
    marked: BTreeSet<u32>,
    /// The processes that couldn't be looked at, and why.
    failed: Vec<String>,
}

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

    /// Hold the direct child while it is unreaped, so its pid is its own.
    fn hold_root(&mut self, pid: u32) -> Held {
        Held { pid, frozen: false, fd: open_pidfd(pid).ok() }
    }

    fn send(&mut self, held: &Held, sig: Sig) -> Result<(), Unsent> {
        send(held, sig)
    }

    /// Whether the process has exited (`Ok(None)`: no pidfd to ask, so it's the unreaped child;
    /// an error when its pidfd can't be asked).
    fn exited(&mut self, held: &Held) -> Result<Option<bool>, String> {
        exited(held)
    }
}

/// The real `/proc` and signals.
#[cfg(target_os = "linux")]
pub(super) struct Real;

#[cfg(target_os = "linux")]
impl Os for Real {}

/// Freeze and kill `root` and what it started (see [`kill_run`]); what was done. `own`: `root` is
/// unreaped until xtask waits for it, so its pid is its own; else it is held only by its marker.
///
/// Everything found is frozen with SIGSTOP until no new one turns up, a level of the tree a round,
/// then killed with SIGKILL, the last found first. Then the failures are dealt with (module docs).
#[cfg(target_os = "linux")]
pub(super) fn kill_all(root: Option<u32>, own: bool, marker: &Marker, os: &mut dyn Os) -> String {
    if !std::path::Path::new("/proc/self/environ").exists() {
        return child_only(root, "no /proc to find them in");
    }
    let entry = marker.entry();
    let mut end = Ending::default();
    if !own && root.is_some() {
        end.problems.push("xtask ignores SIGCHLD, so its child's pid was trusted only with the run's marker".to_owned());
    }
    let rounds = freeze(root.map(|pid| (pid, own)), &entry, os, &mut end);
    let kill_failed = kill(os, &end.held);
    end.problems.extend(thaw(os, &end.held, &kill_failed));
    let mut alive = wait_exited(os, &end.held, &entry, &mut end.problems);
    // those are told of above
    alive.retain(|p| !kill_failed.iter().any(|(i, _)| end.held.get(*i).is_some_and(|h| h.pid == *p)));
    let started = end.held.iter().filter(|h| Some(h.pid) != root).count();
    let mut note = summary(root, started);
    if let Some(root) = root.filter(|r| !end.held.iter().any(|h| h.pid == *r)) {
        note.push_str(&format!("; but pid {root} wasn't signalled: it no longer named the run's process (it may have exited and been reaped)"));
    }
    if rounds == ROUNDS {
        note.push_str(&format!("; it kept starting processes, so xtask stopped looking after {ROUNDS} rounds"));
    }
    if let Some((n, why)) = end.unheld.first().map(|why| (end.unheld.len(), why)) {
        note.push_str(&format!("; {n} process(es) it started couldn't be held by a pidfd or looked at ({why}), so they weren't signalled"));
    }
    if let Some((n, why)) = end.unseen.first().map(|why| (end.unseen.len(), why)) {
        note.push_str(&format!("; {n} process(es) of this user couldn't be looked at ({why}); any of them it started weren't signalled"));
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
    /// Why each process the scans couldn't look at couldn't be.
    unseen: BTreeSet<String>,
}

/// Hold and stop `root` (with whether its pid is its own) and then, a round at a time, every new
/// process found; how many rounds ran.
#[cfg(target_os = "linux")]
fn freeze(root: Option<(u32, bool)>, entry: &str, os: &mut dyn Os, end: &mut Ending) -> usize {
    let mut fresh: Vec<Held> = Vec::new();
    match root {
        Some((pid, true)) => fresh.push(os.hold_root(pid)),
        // reaped by the kernel perhaps, so its pid may be another's: held only as a marked process
        Some((pid, false)) => match os.hold(pid, entry) {
            Hold::Held(held, _, true) => fresh.push(held),
            Hold::Held(..) | Hold::Not => {}
            Hold::Unheld(why) => end.unheld.push(why),
        },
        None => {}
    }
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
        let scan = match os.scan(entry) {
            Ok(scan) => scan,
            Err(e) => {
                end.problems.push(format!("could not look for the processes it started ({e}), so some may still run"));
                return round;
            }
        };
        end.unseen.extend(scan.failed);
        let mut found = children_of(&scan.table, &stopped);
        found.extend(scan.marked);
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
/// processes: one whose pidfd says it hasn't exited now, so it held that pid when it was read (a
/// pid passes to another process only once its process has exited and been reaped). The direct
/// child is held without a pidfd only while it is unreaped, so its pid is its own. A pidfd that
/// can't be asked proves nothing.
#[cfg(target_os = "linux")]
fn has_live_parent(os: &mut dyn Os, held: &[Held], ppid: u32) -> bool {
    held.iter().filter(|h| h.pid == ppid).any(|h| matches!(os.exited(h), Ok(None | Some(false))))
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
/// A held process that can't be asked counts as alive; a look that fails is added to `problems`.
#[cfg(target_os = "linux")]
fn wait_exited(os: &mut dyn Os, held: &[Held], entry: &str, problems: &mut Vec<String>) -> Vec<u32> {
    let until = Instant::now() + AFTER_KILL;
    loop {
        let mut alive: BTreeSet<u32> = held.iter().filter(|h| matches!(os.exited(h), Ok(Some(false)) | Err(_))).map(|h| h.pid).collect();
        let scan = os.scan(entry);
        if let Ok(scan) = &scan {
            alive.extend(&scan.marked);
        }
        if alive.is_empty() || Instant::now() >= until {
            match scan {
                Err(e) => problems.push(format!("could not check that they are gone ({e})")),
                Ok(scan) if !scan.failed.is_empty() => {
                    problems.push(format!("could not check that they are gone ({})", scan.failed.join(", ")));
                }
                Ok(_) => {}
            }
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
fn exited(held: &Held) -> Result<Option<bool>, String> {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let Some(fd) = held.fd.as_ref() else { return Ok(None) };
    let mut fds = [PollFd::new(fd, PollFlags::IN)];
    // a pidfd is readable once its process has exited
    let now = Timespec { tv_sec: 0, tv_nsec: 0 };
    rustix::event::poll(&mut fds, Some(&now)).map_err(|e| format!("poll: {e}"))?;
    let revents = fds.first().map_or(PollFlags::empty(), PollFd::revents);
    if revents.intersects(PollFlags::ERR | PollFlags::NVAL) {
        return Err(format!("poll: {revents:?}"));
    }
    Ok(Some(revents.contains(PollFlags::IN)))
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
    let mut scan = Scan::default();
    for e in std::fs::read_dir("/proc").map_err(|e| format!("/proc: {e}"))? {
        let e = match e {
            Ok(e) => e,
            Err(e) => {
                scan.failed.push(format!("/proc: {e}"));
                continue;
            }
        };
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        match about(pid, entry) {
            Ok(Some((ppid, has))) => {
                scan.table.push((pid, ppid));
                if has {
                    scan.marked.insert(pid);
                }
            }
            Ok(None) => {}
            Err(e) => scan.failed.push(e),
        }
    }
    Ok(scan)
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
    // bytes: the command name in it is the process's to choose, and needn't be UTF-8
    let stat = match std::fs::read(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    let Some(ppid) = parent_in_stat(&stat) else { return Ok(None) };
    // a process that made itself undumpable (as ssh-agent does) can't have its environment read:
    // counted as unmarked, else every kill would report those of the user's session
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
pub(super) fn parent_in_stat(stat: &[u8]) -> Option<u32> {
    let close = stat.iter().rposition(|b| *b == b')')?;
    let rest = std::str::from_utf8(stat.get(close.checked_add(1)?..)?).ok()?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// This user's processes whose environment holds exactly `entry`.
#[cfg(all(test, target_os = "linux"))]
pub(super) fn marked(entry: &str) -> Vec<u32> {
    proc_scan(entry).map(|s| s.marked.into_iter().collect()).unwrap_or_default()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn the_parent_is_read_past_any_parenthesis_in_the_command_name() {
        assert_eq!(parent_in_stat(b"42 (sleep) S 7 42 42 0 -1"), Some(7));
        assert_eq!(parent_in_stat(b"42 (a) b) (c) R 9 1"), Some(9));
        assert_eq!(parent_in_stat(b"42 (\xff\xfe x) S 8 1"), Some(8), "a command name that isn't UTF-8");
        assert_eq!(parent_in_stat(b"42 (sh) Z"), None);
        assert_eq!(parent_in_stat(b"garbage"), None);
    }

    /// The `SigIgn` mask read for `SIGCHLD`, from a real process that ignores it (an ignored
    /// signal stays ignored across exec) and one that doesn't.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_that_ignores_sigchld_is_told_from_one_that_does_not() {
        let status = |trap: &str| {
            // bash: dash keeps SIGCHLD for itself and doesn't ignore it for `trap ''`
            let out = std::process::Command::new("bash").args(["-c", &format!("{trap} exec cat /proc/self/status")]).output().unwrap();
            ignores_sigchld(&String::from_utf8_lossy(&out.stdout))
        };
        assert_eq!(status("trap '' CHLD;"), Some(true));
        assert_eq!(status(""), Some(false));
        assert_eq!(ignores_sigchld("Name: x\nSigIgn:\t0000000000010000\n"), Some(true), "SIGCHLD is 17 on Linux");
        assert_eq!(ignores_sigchld("SigIgn: zz"), None);
        assert_eq!(ignores_sigchld(""), None);
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
    pub(in crate::deadline) fn state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(')')?.1.split_whitespace().next()?.chars().next()
    }

    /// Kills (through a pidfd) and reaps what a test started when it drops, also when it fails.
    #[cfg(target_os = "linux")]
    struct Cleanup {
        children: Vec<Child>,
        others: Vec<Held>,
        dir: Option<std::path::PathBuf>,
    }

    #[cfg(target_os = "linux")]
    impl Cleanup {
        /// Before the test starts anything: these tests need pidfds (Linux 5.3).
        fn new() -> Self {
            assert!(open_pidfd(std::process::id()).is_ok(), "this kernel has pidfds (Linux 5.3)");
            Self { children: Vec::new(), others: Vec::new(), dir: None }
        }
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
        let fd = held.fd.is_some();
        cleanup.others.push(held);
        assert!(fd, "the grandchild is held by a pidfd");
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
        let mut cleanup = Cleanup::new();
        let (root, marker, grandchild) = marked_sh("sleep 300 & echo $! > PIDFILE; wait", "thaw", &mut cleanup);
        let mut os = KillFails { root, refused: Vec::new() };
        let note = kill_all(Some(root), true, &marker, &mut os);
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
            let mut scan = proc_scan(entry)?;
            scan.marked.extend(self.decoy.take());
            Ok(scan)
        }
    }

    /// A pid found by its marker that names another process by the time it is held is never
    /// signalled: what makes it ours is read again once it is held by a pidfd (the old code
    /// stopped and killed whatever the pid named by then).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pid_that_passed_to_another_process_is_not_signalled() {
        let mut cleanup = Cleanup::new();
        let decoy = decoy(&mut cleanup);
        let (root, marker, grandchild) = marked_sh("sleep 300 & echo $! > PIDFILE; wait", "reused", &mut cleanup);
        let note = kill_all(Some(root), true, &marker, &mut PidReused { decoy: Some(decoy) });
        let s = state(decoy);
        assert!(s.is_some_and(|s| s != 'T' && s != 'Z'), "the decoy runs on: state {s:?}");
        assert!(state(grandchild).is_none_or(|s| s == 'Z'), "the run's own processes were killed");
        assert!(note.starts_with("killed it and the 1 process(es) it started") && !note.contains("still alive"), "{note}");
    }

    /// The real OS, except that the first scan also reports `decoy` as a child of `parent`, as if
    /// `parent` (held, then exited) had been reaped and its pid passed to the decoy's parent; and,
    /// with `poll_fails`, no pidfd can be asked whether its process has exited.
    #[cfg(target_os = "linux")]
    struct ParentReused {
        decoy: u32,
        parent: u32,
        once: bool,
        poll_fails: bool,
    }

    #[cfg(target_os = "linux")]
    impl Os for ParentReused {
        fn scan(&mut self, entry: &str) -> Result<Scan, String> {
            let mut scan = proc_scan(entry)?;
            if std::mem::take(&mut self.once) {
                scan.table.push((self.decoy, self.parent));
            }
            Ok(scan)
        }

        fn exited(&mut self, held: &Held) -> Result<Option<bool>, String> {
            if self.poll_fails && held.fd.is_some() {
                return Err("poll refused by the test".to_owned());
            }
            exited(held)
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
        let mut cleanup = Cleanup::new();
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
        let note = kill_all(Some(root), true, &marker, &mut ParentReused { decoy, parent: root, once: true, poll_fails: false });
        let s = state(decoy);
        assert!(s.is_some_and(|s| s != 'T' && s != 'Z'), "the decoy runs on: state {s:?}");
        assert!(note.starts_with("killed it; it had started no other process"), "{note}");
    }

    /// A parent whose pidfd can't be asked whether it has exited vouches for no child: the poll
    /// failing proves nothing (its own marked processes are still found by the marker).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_parent_that_cannot_be_asked_vouches_for_no_child() {
        let mut cleanup = Cleanup::new();
        let decoy = decoy(&mut cleanup);
        let (root, marker, grandchild) = marked_sh("sleep 300 & echo $! > PIDFILE; wait", "poll", &mut cleanup);
        // the root's pidfd is the one asked: held through the marker path, it has one
        let mut os = ParentReused { decoy, parent: root, once: true, poll_fails: true };
        let note = kill_all(Some(root), true, &marker, &mut os);
        let s = state(decoy);
        assert!(s.is_some_and(|s| s != 'T' && s != 'Z'), "the decoy runs on: state {s:?}");
        let until = Instant::now() + Duration::from_secs(5);
        while state(grandchild).is_some_and(|s| s != 'Z') && Instant::now() < until {
            std::thread::sleep(TICK);
        }
        assert!(state(grandchild).is_none_or(|s| s == 'Z'), "the marked grandchild was killed: {note}");
    }

    /// When xtask ignores SIGCHLD the kernel may have reaped the direct child, so its pid is
    /// trusted only with the run's marker: a pid that names an unmarked process is left alone.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_that_may_have_been_reaped_is_trusted_only_with_the_marker() {
        let mut cleanup = Cleanup::new();
        let decoy = decoy(&mut cleanup);
        let note = kill_all(Some(decoy), false, &Marker::new(), &mut Real);
        let s = state(decoy);
        assert!(s.is_some_and(|s| s != 'T' && s != 'Z'), "an unmarked process under the child's pid runs on: state {s:?}");
        assert!(note.contains("xtask ignores SIGCHLD"), "{note}");
        // and a marked child is still killed
        let (root, marker, grandchild) = marked_sh("sleep 300 & echo $! > PIDFILE; wait", "sigign", &mut cleanup);
        let note = kill_all(Some(root), false, &marker, &mut Real);
        assert!(note.starts_with("killed it and the 1 process(es) it started"), "{note}");
        assert!(state(grandchild).is_none_or(|s| s == 'Z'));
    }

    /// A pidfd refers to its process alone: once that process has exited and been reaped, a
    /// signal through it fails as gone, whatever holds the pid now.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_held_process_that_was_reaped_gets_no_signal() {
        let mut cleanup = Cleanup::new();
        let pid = decoy(&mut cleanup);
        let held = Real.hold_root(pid);
        assert!(held.fd.is_some(), "held by a pidfd");
        assert_eq!(exited(&held), Ok(Some(false)));
        if let Some(c) = cleanup.children.last_mut() {
            c.kill().unwrap();
            c.wait().unwrap();
        }
        assert_eq!(exited(&held), Ok(Some(true)));
        assert_eq!(send(&held, Sig::Stop), Err(Unsent::Gone));
    }
}
