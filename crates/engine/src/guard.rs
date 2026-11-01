//! Last-resort panic guard (never-crash standard, `AGENTS.md`): a panic that escapes a command
//! becomes that command's error and the session (catalog, library, undo history) stays usable.
//! It's a safety net, not a way to report errors: commands return `Err` themselves.

use std::any::Any;
use std::io::Write;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;

/// The text of a panic payload (`panic!("…")` gives a `&str` or a `String`).
pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    match (payload.downcast_ref::<&str>(), payload.downcast_ref::<String>()) {
        (Some(s), _) => (*s).to_string(),
        (_, Some(s)) => s.clone(),
        _ => "unknown panic".into(),
    }
}

/// Run `f`; a panic inside it becomes `Err("<what> failed unexpectedly: <message>")`. On wasm32
/// (built with `panic = "abort"`) nothing can be caught and this just runs `f`.
pub fn catch<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|p| {
        let msg = format!("{what} failed unexpectedly: {}", panic_message(p.as_ref()));
        log::error!("{msg}");
        msg
    })
}

/// Install a panic hook that keeps the default report (stderr) and also appends the panic's
/// location and message to `log` (best effort, its folder made when missing), so a panic in a GUI
/// session leaves a trace. `log` belongs in a folder of the user's own, never a shared one.
pub fn install_hook(log: Option<PathBuf>) {
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        default(info);
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let Some(log) = &log else { return };
        if let Ok(mut f) = open_panic_log(log) {
            let _ = writeln!(f, "{secs} [{thread}] panic at {at}: {}", panic_message(info.payload()));
        }
    }));
}

/// Opens the panic log for appending, making its folder when missing. On Unix a new log is
/// readable by its owner only (0600): panic messages can name the user's files.
fn open_panic_log(log: &std::path::Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut open = std::fs::OpenOptions::new();
    open.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    open.open(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #2572: the panic log's folder is made when missing, and on Unix a new log is the
    /// owner's alone; appending keeps what is there.
    #[test]
    fn the_panic_log_is_made_for_its_owner_only() {
        let dir = std::env::temp_dir().join(format!("lightcraft-panic-log-{}", std::process::id()));
        let log = dir.join("logs").join("lightcraft-panics.log");
        writeln!(open_panic_log(&log).unwrap(), "one").unwrap();
        writeln!(open_panic_log(&log).unwrap(), "two").unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "one\ntwo\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panic_becomes_an_error() {
        assert_eq!(catch("ok", || 7), Ok(7));
        let e = catch("`demo.cmd`", || -> u32 { panic!("boom {}", 1) }).unwrap_err();
        assert_eq!(e, "`demo.cmd` failed unexpectedly: boom 1");
        assert!(catch("x", || std::panic::panic_any(5u8)).unwrap_err().ends_with("unknown panic"));
    }
}
