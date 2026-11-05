//! Fault injection for the catalog log's writer thread (tests only: nothing in the product
//! installs a hook, and with none installed a check is one uncontended lock of an empty list).
//!
//! A test makes the drive under one library directory slow (a hook that sleeps) or failing (a
//! hook that returns an error) and proves that a flag never waits for it and that nothing handed
//! over is lost: `crates/catalog/src/tests_log_thread.rs`, `crates/engine/src/tests/slow_drive.rs`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// Where the log thread is about to touch the drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Writing bytes to the log (or to the file that replaces it).
    Write,
    /// Syncing them.
    Sync,
}

/// Runs before each [`Step`] in the hooked directory: an error fails that step.
pub type Hook = Arc<dyn Fn(Step) -> io::Result<()> + Send + Sync>;

static HOOKS: Mutex<Vec<(PathBuf, Hook)>> = Mutex::new(Vec::new());

/// Run `hook` before every write and sync of the catalog log in `dir` (replacing an earlier one).
#[doc(hidden)]
pub fn install(dir: &Path, hook: Hook) {
    let mut hooks = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
    hooks.retain(|(d, _)| d != dir);
    hooks.push((dir.to_path_buf(), hook));
}

/// Remove the hook of `dir`.
#[doc(hidden)]
pub fn remove(dir: &Path) {
    HOOKS.lock().unwrap_or_else(PoisonError::into_inner).retain(|(d, _)| d != dir);
}

static ON_CALLER: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Write the log in `dir` on the caller's thread, syncing before each append returns, as builds
/// before PERF-STEP-STALL did: a test's "before" (stores opened afterwards; the hooks run there too).
#[doc(hidden)]
pub fn write_on_caller(dir: &Path, on: bool) {
    let mut dirs = ON_CALLER.lock().unwrap_or_else(PoisonError::into_inner);
    dirs.retain(|d| d != dir);
    if on {
        dirs.push(dir.to_path_buf());
    }
}

pub(crate) fn on_caller(dir: &Path) -> bool {
    let dirs = ON_CALLER.lock().unwrap_or_else(PoisonError::into_inner);
    !dirs.is_empty() && dirs.iter().any(|d| d == dir)
}

pub(crate) fn check(dir: &Path, step: Step) -> io::Result<()> {
    let hook = {
        let hooks = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
        if hooks.is_empty() {
            return Ok(());
        }
        hooks.iter().find(|(d, _)| d == dir).map(|(_, h)| h.clone())
    };
    match hook {
        Some(h) => h(step),
        None => Ok(()),
    }
}
