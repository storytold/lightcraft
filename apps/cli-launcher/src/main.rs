//! `<cli>.exe` in the Windows packages: a console program that runs the app's executable beside it
//! as the command-line tool (`<binary>.exe --cli ARGS…`) and hands back its exit code.
//!
//! The app is a GUI-subsystem program on Windows, so a terminal doesn't wait for it and its output
//! has no console to go to. This launcher is a console program: the terminal waits for it, and the
//! app inherits its standard input, output and error, so the tool reads and writes the terminal as
//! a console program would. Elsewhere the packages start the app's executable under the CLI's
//! name instead (a link), and this program isn't shipped.
//!
//! The app is put in a job that ends it when this program ends ([`tie_to_launcher`]), so a Ctrl+C
//! stops the app too. The option it passes is the app's `CLI_OPTION` (`apps/app/src/main.rs`).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The option that starts the app's executable as the command-line tool.
const CLI_OPTION: &str = "--cli";

fn main() {
    let app = std::env::current_exe().ok().and_then(|exe| app_beside(&exe));
    let Some(app) = app else {
        eprintln!("{}: can't tell which folder this program is in, to find {} there", lcc_brand::CLI_BINARY, app_file_name());
        std::process::exit(1);
    };
    // made before the app starts, so a failure is reported before the app writes anything
    let job = tie_to_launcher();
    let mut child = match Command::new(&app).arg(CLI_OPTION).args(std::env::args_os().skip(1)).spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("{}: could not start {}: {e}", lcc_brand::CLI_BINARY, app.display());
            std::process::exit(1);
        }
    };
    // an app that has already finished (a quick command) can't join the job, and needn't
    if let Some(job) = &job
        && let Err(e) = job.add(&child)
        && !matches!(child.try_wait(), Ok(Some(_)))
    {
        eprintln!("{}: {e}; stopping this program may leave {} running", lcc_brand::CLI_BINARY, app_file_name());
    }
    match child.wait() {
        // ended by a signal (not on Windows): a failure, as the shell would report it
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("{}: lost track of {}: {e}", lcc_brand::CLI_BINARY, app.display());
            std::process::exit(1);
        }
    }
}

/// The job that ends the app when this program ends, however it ends: the app has no console, so a
/// Ctrl+C or a closed console reaches only this program (`lcc_winjob`). Programs the app starts
/// (an external editor) stay out of it and keep running. Off Windows there is none,
/// and none is needed (this program isn't shipped there).
fn tie_to_launcher() -> Option<lcc_winjob::Job> {
    match lcc_winjob::Job::new() {
        Ok(job) => Some(job),
        Err(lcc_winjob::Error::Unsupported) => None,
        Err(e) => {
            eprintln!("{}: {e}; stopping this program may leave {} running", lcc_brand::CLI_BINARY, app_file_name());
            None
        }
    }
}

/// The app's executable in the folder of `exe` (this program).
fn app_beside(exe: &Path) -> Option<PathBuf> {
    Some(exe.parent()?.join(app_file_name()))
}

fn app_file_name() -> String {
    format!("{}{}", lcc_brand::BINARY, std::env::consts::EXE_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_is_looked_for_beside_the_launcher() {
        let exe = Path::new("install").join("folder").join(format!("{}{}", lcc_brand::CLI_BINARY, std::env::consts::EXE_SUFFIX));
        assert_eq!(app_beside(&exe), Some(Path::new("install").join("folder").join(app_file_name())));
    }
}
