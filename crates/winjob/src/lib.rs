//! Windows job objects: [`Job`] ties child processes' lives to this process's. A child added with
//! [`Job::add`] ends when the `Job` is dropped, and so when this process ends, however it ends
//! (returns, Ctrl+C or Ctrl+Break in its console, the console closed, killed, crashed): Windows
//! closes the job's last handle with the process and ends everything in the job
//! (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`).
//!
//! Only the children added are tied: the processes they start stay out of the job
//! (`JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`) and outlive it, as they would any parent. The app starts
//! programs that are meant to keep running (an external editor for Edit In, the mail program).
//!
//! The Windows command-line launcher (`apps/cli-launcher`) uses it for the app it starts. The app is
//! a GUI-subsystem program with no console, so a Ctrl+C in the terminal reaches only the launcher;
//! without the job, the app would go on running with no window once the launcher ended.
//!
//! The children end at once, as with `TerminateProcess`, with no chance to clean up: the same as a
//! console program ended by Ctrl+C's default handler. A child is tied from when it is added, so
//! add it straight after starting it.
//!
//! This is one of the crates allowed `unsafe` (three kernel32 calls and `CloseHandle`, in `win`). Its
//! API is safe: the job's handle is owned by [`Job`] and closed only on drop, and a child's handle is
//! borrowed from its `std::process::Child` for the call. On other platforms [`Job::new`] returns
//! [`Error::Unsupported`].
//!
//! Sources: Microsoft Win32 documentation (CreateJobObjectW, SetInformationJobObject,
//! JOBOBJECT_EXTENDED_LIMIT_INFORMATION, AssignProcessToJobObject). Own design otherwise.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::fmt;
use std::process::Child;

#[derive(Debug)]
pub enum Error {
    /// Not Windows.
    Unsupported,
    /// A Windows call failed: what was being done, and Windows' error.
    System(&'static str, std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported => f.write_str("job objects are only on Windows"),
            Error::System(what, e) => write!(f, "{what} failed: {e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A job whose processes end when it is dropped (or when this process ends).
pub struct Job {
    #[cfg(windows)]
    inner: win::Job,
}

impl Job {
    pub fn new() -> Result<Job> {
        #[cfg(windows)]
        {
            Ok(Job { inner: win::Job::new()? })
        }
        #[cfg(not(windows))]
        {
            Err(Error::Unsupported)
        }
    }

    /// Puts `child` (still running) in the job.
    pub fn add(&self, child: &Child) -> Result<()> {
        #[cfg(windows)]
        {
            self.inner.add(child)
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Err(Error::Unsupported)
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win;

#[cfg(test)]
mod tests;
