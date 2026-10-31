use super::prefs::config_dir;

pub(crate) const HELP: &str = "\
lightcraft — photo library + raw developer

USAGE: lightcraft [OPTIONS] [FILES/FOLDERS…]   (files and folders are imported)

OPTIONS:
  --library DIR    open (or create) this library (env LIGHTCRAFT_LIBRARY; default: the last library opened, or
                   a list of the recent ones with Settings > General > At launch: Ask me or Alt/Option held,
                   else ~/Pictures/LightCraft Library)
  --no-demo        don't seed a new library with the procedural demo photos
  --memory         throwaway in-memory session (alias --demo); nothing is saved
  --control PORT   serve the JSON-lines control channel on 127.0.0.1:PORT (env LIGHTCRAFT_CONTROL_PORT;
                   see docs/control-protocol.md)
  --version, --help

ENVIRONMENT:
  LIGHTCRAFT_CONFIG_DIR=DIR   use this settings folder instead of the platform default (empty = no settings folder).
                   Without it, a portable.txt file next to the program puts the folder beside it (LightCraftData)
  LIGHTCRAFT_GPU_BACKEND=dx12|vulkan|metal|auto|off   graphics backend (default: DX12 on Windows, Metal on macOS,
                   Vulkan on Linux; off = render on the CPU); else WGPU_BACKEND. LIGHTCRAFT_GPU=0: CPU rendering.
  LIGHTCRAFT_SAM3_DIR=DIR   the SAM 3 model for Object / Describe masks (default: <settings folder>/models/sam3;
                   optional: LightCraft offers to download it when first needed)
  LIGHTCRAFT_SAM3_MIRRORS=URL,…   where to download the SAM 3 model from (base URLs, tried in order)
  LIGHTCRAFT_LOG=info|debug   more log detail from LightCraft itself (default: info from LightCraft, warnings
                   from other crates); else RUST_LOG=<env_logger directives>. Records go to stderr and to
                   <settings folder>/logs/lightcraft.log (previous runs: lightcraft.1.log, lightcraft.2.log)
";

/// The launch arguments' files and folders to import (absolute), and a notice for each one that
/// is not imported (see [`lightcraft_engine::import::launch_path`]; issue #374).
pub(crate) fn launch_imports(args: &[String]) -> (Vec<String>, Vec<String>) {
    let cwd = std::env::current_dir().ok();
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|h| !h.is_empty()).map(std::path::PathBuf::from);
    launch_imports_in(args, cwd.as_deref(), home.as_deref())
}

pub(crate) fn launch_imports_in(args: &[String], cwd: Option<&std::path::Path>, home: Option<&std::path::Path>) -> (Vec<String>, Vec<String>) {
    use lightcraft_engine::import::{LaunchPath, launch_path};
    let (mut files, mut notices) = (Vec::new(), Vec::new());
    for a in args {
        match launch_path(a, cwd, home, lightcraft_engine::devices::dcim_in) {
            Some(LaunchPath::Import(p)) => files.push(p),
            Some(LaunchPath::Refused { path, why }) => {
                log::warn!("not importing {path} given at launch: {why}");
                notices
                    .push(format!("{path} was not imported: it is {why}. To import from it, choose a folder inside it with File ▸ Import Photos…"));
            }
            None => {}
        }
    }
    (files, notices)
}

/// Where the log files live: `logs` in the settings folder, next to `ui.json` (see `logging`).
/// None with `LIGHTCRAFT_NO_PREFS` (tests, scripts), so those runs don't rotate away the user's logs.
pub(crate) fn log_dir() -> Option<std::path::PathBuf> {
    log_dir_with(|name| std::env::var_os(name), config_dir)
}

/// [`log_dir`] with the environment and the settings folder passed in (`config` is asked only
/// when the logs are kept).
fn log_dir_with(env: impl Fn(&str) -> Option<std::ffi::OsString>, config: impl FnOnce() -> Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    if env("LIGHTCRAFT_NO_PREFS").is_some() {
        return None;
    }
    config().map(|d| d.join("logs"))
}

/// The control port `value` names; a value that isn't a port is logged and ignored (`what` is
/// the option or variable it came from). Logged before the log file opens, so it is kept for it.
pub(crate) fn control_port_from(what: &str, value: Option<String>) -> Option<u16> {
    let value = value?;
    let port = value.trim().parse().ok();
    if port.is_none() {
        log::warn!("{what} {value:?} is not a port number; no control server");
    }
    port
}

/// The emergency crash log, `lightcraft-panics.log` beside the other logs in the user's own
/// settings folder (an isolated profile's with `LIGHTCRAFT_CONFIG_DIR`). Never the shared temp
/// folder, where another user on the machine could create the file first, read it, or point it
/// at one of the user's files (issue #2572). `None` (standard error only) where no logs are kept.
pub(crate) fn panic_log_path() -> Option<std::path::PathBuf> {
    log_dir().map(|d| d.join("lightcraft-panics.log"))
}

/// What to tell the user when LightCraft stopped while running (`error`), and why when the GPU
/// was lost (`lost`): GPU rendering is off from the next start.
pub(crate) fn stopped_text(error: &str, lost: Option<&str>) -> String {
    match lost {
        Some(reason) => format!(
            "{error}. The graphics driver reset the GPU ({reason}). LightCraft renders on the CPU from its next start; \
             Settings ▸ Performance ▸ Use the GPU for rendering turns the GPU back on"
        ),
        None => error.to_string(),
    }
}

/// LightCraft stopped while its window was up: say so in the log and in a native message box
/// naming the log file (as [`startup_failed`] does for a start that failed).
pub(crate) fn stopped_while_running(text: &str, log_file: Option<&std::path::Path>) {
    log::error!("LightCraft stopped: {text}");
    if std::env::var_os("LIGHTCRAFT_NO_PREFS").is_some() {
        return; // tests and scripts: no dialog to click away
    }
    let (title, text) = lightcraft_ui_egui::i18n::stopped_message(text, log_file.map(|p| p.display().to_string()).as_deref());
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(text)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// The window could not start (e.g. no usable graphics device): say so in the log and in a native
/// message box naming the log file, instead of exiting without a trace — the release builds have
/// no console on Windows, and an app started from Finder shows no standard error (issue #260).
pub(crate) fn startup_failed(error: &str, log_file: Option<&std::path::Path>) {
    log::error!("LightCraft could not open its window: {error}");
    if std::env::var_os("LIGHTCRAFT_NO_PREFS").is_some() {
        return; // tests and scripts: no dialog to click away
    }
    let (title, text) = lightcraft_ui_egui::i18n::startup_failed_message(error, log_file.map(|p| p.display().to_string()).as_deref());
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(text)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(test)]
mod config_path_tests {
    use super::*;

    /// Issue #2572: the logs, the panic log among them, go to the settings folder (an isolated
    /// profile's too), and nowhere when no logs are kept; no fallback to the shared temp folder.
    #[test]
    fn logs_stay_in_the_settings_folder() {
        let no_env = |_: &str| None;
        assert_eq!(log_dir_with(no_env, || Some("isolated".into())), Some(std::path::PathBuf::from("isolated/logs")));
        assert_eq!(log_dir_with(no_env, || None), None);
        let no_prefs = |name: &str| (name == "LIGHTCRAFT_NO_PREFS").then(|| "1".into());
        assert_eq!(log_dir_with(no_prefs, || panic!("the settings folder isn't asked for")), None);
    }
}
