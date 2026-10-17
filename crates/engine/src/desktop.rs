//! The desktop hand-offs: showing a file in the system file manager and starting another
//! program (an external editor, or the file's default application).
//!
//! They go through [`Desktop`] so that headless sessions and tests never start desktop programs:
//! a session in a unit test, and every headless UI harness, records the request with
//! [`Recorder`] instead; the real app uses [`System`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// What the session asked the desktop to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Show this file in the system file manager.
    Reveal(PathBuf),
    /// Start this command line and return at once.
    Launch(Vec<String>),
    /// Start this command line and wait for it to exit.
    RunAndWait(Vec<String>),
}

/// The hand-offs a session makes to the desktop.
pub trait Desktop: Send + Sync {
    /// Show `file` in the system file manager.
    fn reveal(&self, file: &Path) -> Result<(), String>;
    /// Start `line` (program, then arguments) without waiting for it.
    fn launch(&self, line: &[String]) -> Result<(), String>;
    /// Start `line` and wait for it. → whether it exited successfully
    fn run_and_wait(&self, line: &[String]) -> Result<bool, String>;
}

/// The real desktop: spawns the file manager or the program.
#[derive(Clone, Copy, Debug, Default)]
pub struct System;

impl Desktop for System {
    #[cfg(not(target_arch = "wasm32"))]
    fn reveal(&self, file: &Path) -> Result<(), String> {
        lcc_updater::reveal(file).map_err(|e| e.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    fn reveal(&self, _: &Path) -> Result<(), String> {
        Err("not available in the browser".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn launch(&self, line: &[String]) -> Result<(), String> {
        let (prog, rest) = line.split_first().ok_or("no program")?;
        std::process::Command::new(prog).args(rest).spawn().map(|_| ()).map_err(|e| format!("{prog}: {e}"))
    }
    #[cfg(target_arch = "wasm32")]
    fn launch(&self, _: &[String]) -> Result<(), String> {
        Err("not available in the browser".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn run_and_wait(&self, line: &[String]) -> Result<bool, String> {
        let (prog, rest) = line.split_first().ok_or("no program")?;
        let status = std::process::Command::new(prog).args(rest).stdin(std::process::Stdio::null()).status().map_err(|e| format!("{prog}: {e}"))?;
        Ok(status.success())
    }
    #[cfg(target_arch = "wasm32")]
    fn run_and_wait(&self, _: &[String]) -> Result<bool, String> {
        Err("not available in the browser".into())
    }
}

/// A desktop that only records what was asked (headless sessions and tests). Every request
/// succeeds; [`Desktop::run_and_wait`] reports a successful exit.
#[derive(Debug, Default)]
pub struct Recorder {
    requests: Mutex<Vec<Request>>,
}

impl Recorder {
    /// The requests so far, oldest first.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    fn push(&self, r: Request) {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).push(r);
    }
}

impl Desktop for Recorder {
    fn reveal(&self, file: &Path) -> Result<(), String> {
        self.push(Request::Reveal(file.to_path_buf()));
        Ok(())
    }
    fn launch(&self, line: &[String]) -> Result<(), String> {
        self.push(Request::Launch(line.to_vec()));
        Ok(())
    }
    fn run_and_wait(&self, line: &[String]) -> Result<bool, String> {
        self.push(Request::RunAndWait(line.to_vec()));
        Ok(true)
    }
}

/// The desktop a new session gets: the real one, except in this crate's unit tests.
pub(crate) fn default_desktop() -> std::sync::Arc<dyn Desktop> {
    if cfg!(test) { std::sync::Arc::new(Recorder::default()) } else { std::sync::Arc::new(System) }
}
