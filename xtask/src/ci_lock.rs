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

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_secs(2);

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

/// Take the first free slot of `slots` in `dir`, or `None` when all are held.
pub fn try_acquire(dir: &Path, slots: usize, owner: &str) -> Result<Option<Held>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("ci lock: {}: {e}", dir.display()))?;
    for slot in 0..slots.max(1) {
        let path = slot_path(dir, slot);
        let file =
            OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(|e| format!("ci lock: {}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {
                // a sidecar file, not the lock file: Windows locks are mandatory, so others couldn't read it
                let owner_file = owner_path(&path);
                let _ = std::fs::File::create(&owner_file).and_then(|mut f| f.write_all(owner.as_bytes()));
                return Ok(Some(Held { _file: file, owner: owner_file }));
            }
            Err(TryLockError::WouldBlock) => continue,
            Err(TryLockError::Error(e)) => return Err(format!("ci lock: {}: {e}", path.display())),
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
    let slots = slots();
    let owner = format!("pid {} in {}: {command}", std::process::id(), root.display());
    if let Some(held) = try_acquire(&dir, slots, &owner)? {
        return Ok(Some(held));
    }
    let waiting = Instant::now();
    eprintln!("=== ci: waiting for another CI run to finish ({slots} at a time; LIGHTCRAFT_CI_SLOTS changes it) ===");
    for h in holders(&dir, slots) {
        eprintln!("  running: {h}");
    }
    loop {
        std::thread::sleep(POLL);
        if let Some(held) = try_acquire(&dir, slots, &owner)? {
            eprintln!("=== ci: waited {:.0} s for a CI slot ===", waiting.elapsed().as_secs_f64());
            return Ok(Some(held));
        }
    }
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
