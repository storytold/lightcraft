//! How a command ends badly: a usage error is the person's to fix (exit 2); any other failure is
//! exit 1.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    /// The command line can't be run as given (a combination clap can't express, a name that
    /// isn't valid Unicode). Exit 2.
    #[error("{0}")]
    Usage(String),
    /// An argument that isn't valid Unicode where the engine takes text.
    #[error("{} is not valid Unicode", .0.display())]
    NotUnicode(std::path::PathBuf),
    /// `run` finished its steps and some failed (each failure is on stdout).
    #[error("{0} command(s) failed")]
    StepsFailed(usize),
    /// Any other failure, with the engine's or the file system's own message.
    #[error("{0}")]
    Failed(String),
}

impl CliError {
    /// The exit code: 2 for a usage error, 1 for everything else.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::NotUnicode(_) => 2,
            Self::StepsFailed(_) | Self::Failed(_) => 1,
        }
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        Self::Failed(message.to_string())
    }
}
