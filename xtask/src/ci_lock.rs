//! One `cargo xtask ci` at a time per machine.
//!
//! Every CI run builds the workspace several times with one job per CPU. Several worktrees running
//! it at once (parallel agents) slow each other down far more than waiting in turn would, and make
//! the machine unusable meanwhile. So `ci` first takes one of `LIGHTCRAFT_CI_SLOTS` (default 1)
//! lock files in a per-user cache directory, shared by every checkout and worktree, and holds it
//! until it exits. The OS drops the lock when the process ends, crashed or not.
//!
//! - `LIGHTCRAFT_CI_SLOTS=N`: N runs at once (1–64).
//! - `LIGHTCRAFT_CI_LOCK=off`: no lock (e.g. a CI runner that only ever runs one job).
//! - `LIGHTCRAFT_CI_LOCK=<dir>`: keep the lock files there instead.
//! - `LIGHTCRAFT_CI_LOCK_WAIT=MIN`: give up after waiting MIN minutes for a slot (default
//!   [`DEFAULT_WAIT_MIN`]; `0` or `off` waits as long as it takes), with an error naming the run
//!   that holds it (#3060). A full `ci` takes about 15 minutes on a 16-core machine, and a hung test
//!   binary can't hold a slot past its own limit any more ([`crate::deadline`]), so two hours is
//!   several runs ahead in the queue; a wait longer than that means something is stuck.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_secs(2);

/// How long to wait for a slot, in minutes.
pub const WAIT_ENV: &str = "LIGHTCRAFT_CI_LOCK_WAIT";
/// The default of [`WAIT_ENV`].
pub const DEFAULT_WAIT_MIN: u64 = 120;

/// A held CI slot; dropping it releases the slot.
pub struct Held {
    _file: File,
    owner: PathBuf,
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.owner);
    }
}

/// Where the lock files live, or `None` when the lock is switched off.
fn lock_dir() -> Option<PathBuf> {
    match std::env::var_os("LIGHTCRAFT_CI_LOCK") {
        Some(v) if matches!(v.to_str(), Some("off" | "0" | "false" | "no")) => return None,
        Some(v) if !v.is_empty() => return Some(PathBuf::from(v)),
        _ => {}
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    Some(cache.join("lightcraft"))
}

fn slots() -> usize {
    std::env::var("LIGHTCRAFT_CI_SLOTS").ok().and_then(|s| s.trim().parse().ok()).unwrap_or(1).clamp(1, 64)
}

fn slot_path(dir: &Path, slot: usize) -> PathBuf {
    if slot == 0 { dir.join("ci.lock") } else { dir.join(format!("ci-{}.lock", slot + 1)) }
}

fn owner_path(lock: &Path) -> PathBuf {
    lock.with_extension("lock.owner")
}

/// What to do when the lock itself can't be set up (read-only home, sandbox, network mount).
const LOCK_HINT: &str = "set LIGHTCRAFT_CI_LOCK=off to run without the lock, or LIGHTCRAFT_CI_LOCK=<dir> to keep it elsewhere";

/// Take the first free slot of `slots` in `dir`, or `None` when all are held.
pub fn try_acquire(dir: &Path, slots: usize, owner: &str) -> Result<Option<Held>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("ci lock: {}: {e} ({LOCK_HINT})", dir.display()))?;
    for slot in 0..slots.max(1) {
        let path = slot_path(dir, slot);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| format!("ci lock: {}: {e} ({LOCK_HINT})", path.display()))?;
        match file.try_lock() {
            Ok(()) => {
                // a sidecar file, not the lock file: Windows locks are mandatory, so others couldn't read it
                let owner_file = owner_path(&path);
                let _ = std::fs::File::create(&owner_file).and_then(|mut f| f.write_all(owner.as_bytes()));
                return Ok(Some(Held { _file: file, owner: owner_file }));
            }
            Err(TryLockError::WouldBlock) => continue,
            Err(TryLockError::Error(e)) => return Err(format!("ci lock: {}: {e} ({LOCK_HINT})", path.display())),
        }
    }
    Ok(None)
}

/// Who holds each slot, as they wrote it.
fn holders(dir: &Path, slots: usize) -> Vec<String> {
    (0..slots.max(1))
        .filter_map(|slot| std::fs::read_to_string(owner_path(&slot_path(dir, slot))).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Wait for a CI slot (see the module docs). `None` when the lock is off.
pub fn acquire(root: &Path, command: &str) -> Result<Option<Held>, String> {
    let Some(dir) = lock_dir() else {
        return Ok(None);
    };
    let wait = crate::deadline::minutes_from_env(WAIT_ENV, DEFAULT_WAIT_MIN)?;
    let owner = format!("pid {} in {}: {command}", std::process::id(), root.display());
    wait_for_slot(&dir, slots(), &owner, wait, POLL)
}

/// Take a slot of `slots` in `dir`, polling every `poll` for at most `wait` (`None`: no limit).
fn wait_for_slot(dir: &Path, slots: usize, owner: &str, wait: Option<Duration>, poll: Duration) -> Result<Option<Held>, String> {
    if let Some(held) = try_acquire(dir, slots, owner)? {
        return Ok(Some(held));
    }
    let waiting = Instant::now();
    let limit = wait.map_or_else(|| "no time limit".to_owned(), |w| format!("for up to {}", crate::deadline::show(w)));
    eprintln!("=== ci: waiting for another CI run to finish ({slots} at a time; LIGHTCRAFT_CI_SLOTS changes it), {limit} ({WAIT_ENV}) ===");
    for h in holders(dir, slots) {
        eprintln!("  running: {h}");
    }
    loop {
        std::thread::sleep(poll);
        if let Some(held) = try_acquire(dir, slots, owner)? {
            eprintln!("=== ci: waited {:.0} s for a CI slot ===", waiting.elapsed().as_secs_f64());
            return Ok(Some(held));
        }
        if let Some(wait) = wait.filter(|w| waiting.elapsed() >= *w) {
            return Err(gave_up(wait, &holders(dir, slots)));
        }
    }
}

/// The error when the wait for a slot ran out: who holds the slots, and what to do.
fn gave_up(wait: Duration, holders: &[String]) -> String {
    let held_by = if holders.is_empty() { "a run that didn't record itself".to_owned() } else { holders.join("; ") };
    format!(
        "ci lock: gave up after waiting {} for a CI slot, held by: {held_by}. If that run is stuck, end it; \
         {WAIT_ENV}=<minutes> waits longer (0 for as long as it takes), LIGHTCRAFT_CI_SLOTS=2 runs beside it",
        crate::deadline::show(wait)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-ci-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn one_slot_is_exclusive_until_dropped() {
        let dir = scratch("one");
        let first = try_acquire(&dir, 1, "first").unwrap().expect("free slot");
        assert!(try_acquire(&dir, 1, "second").unwrap().is_none());
        assert_eq!(holders(&dir, 1), ["first"]);
        drop(first);
        assert!(holders(&dir, 1).is_empty());
        assert!(try_acquire(&dir, 1, "third").unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_wait_for_a_held_slot_ends_with_the_holder_named() {
        let dir = scratch("wait");
        let _held = try_acquire(&dir, 1, "pid 1 in /w: cargo xtask ci").unwrap().expect("free slot");
        let t = Instant::now();
        let err = wait_for_slot(&dir, 1, "second", Some(Duration::from_millis(300)), Duration::from_millis(20)).err().expect("gave up");
        assert!(t.elapsed() >= Duration::from_millis(300) && t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
        assert!(err.contains("gave up after waiting 300 ms") && err.contains("held by: pid 1 in /w: cargo xtask ci"), "{err}");
        assert!(err.contains("LIGHTCRAFT_CI_LOCK_WAIT=<minutes>"), "says how to wait longer: {err}");
        assert_eq!(holders(&dir, 1), ["pid 1 in /w: cargo xtask ci"], "giving up leaves the holder alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_slot_freed_within_the_wait_is_taken() {
        let dir = scratch("freed");
        let held = try_acquire(&dir, 1, "first").unwrap().expect("free slot");
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(held);
        });
        let got = wait_for_slot(&dir, 1, "second", Some(Duration::from_secs(30)), Duration::from_millis(20)).unwrap();
        assert!(got.is_some());
        assert_eq!(holders(&dir, 1), ["second"]);
        release.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn giving_up_names_an_unrecorded_holder_too() {
        assert!(
            gave_up(Duration::from_secs(7200), &[]).contains("gave up after waiting 120 min for a CI slot, held by: a run that didn't record itself")
        );
    }

    #[test]
    fn more_slots_admit_more_runs() {
        let dir = scratch("two");
        let a = try_acquire(&dir, 2, "a").unwrap();
        let b = try_acquire(&dir, 2, "b").unwrap();
        assert!(a.is_some() && b.is_some());
        assert!(try_acquire(&dir, 2, "c").unwrap().is_none());
        assert_eq!(holders(&dir, 2), ["a", "b"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
