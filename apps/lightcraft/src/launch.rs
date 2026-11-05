use super::prefs::config_dir;

pub(crate) const HELP: &str = "\
lightcraft — photo library + raw developer

USAGE: lightcraft [OPTIONS] [FILES/FOLDERS…]   (files and folders are imported; a library's catalog.snap or
                                               folder opens that library)

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
pub(crate) fn launch_imports(args: &[String]) -> LaunchArgs {
    let cwd = std::env::current_dir().ok();
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|h| !h.is_empty()).map(std::path::PathBuf::from);
    launch_imports_in(args, cwd.as_deref(), home.as_deref())
}

/// The library a launch argument names, after `--library` (`flag`) and `LIGHTCRAFT_LIBRARY` (`env`):
/// the library to pass on as the flag, and whether the argument chose it (it is then remembered).
pub(crate) fn launch_library_choice(
    flag: Option<std::path::PathBuf>,
    env: Option<&std::path::PathBuf>,
    argument: Option<std::path::PathBuf>,
) -> (Option<std::path::PathBuf>, bool) {
    match (flag, env, argument) {
        (Some(flag), _, _) => (Some(flag), false),
        (None, None, Some(argument)) => (Some(argument), true),
        _ => (None, false),
    }
}

/// What the launch arguments ask for: files and folders to import, a library to open (a library's `catalog.snap`
/// or folder, double-clicked in the file manager), and a notice for each argument
/// left alone.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct LaunchArgs {
    pub(crate) files: Vec<String>,
    pub(crate) library: Option<std::path::PathBuf>,
    pub(crate) notices: Vec<String>,
}

pub(crate) fn launch_imports_in(args: &[String], cwd: Option<&std::path::Path>, home: Option<&std::path::Path>) -> LaunchArgs {
    use lightcraft_engine::import::{LaunchPath, launch_deadline, launch_path_until};
    let (mut files, mut notices, mut library) = (Vec::new(), Vec::new(), None);
    // one deadline for every argument's library check: they all run before the window opens
    let deadline = launch_deadline();
    for a in args {
        match launch_path_until(a, cwd, home, lightcraft_engine::devices::dcim_in, deadline) {
            Some(LaunchPath::Library(dir)) if library.is_none() => library = Some(std::path::PathBuf::from(dir)),
            Some(LaunchPath::Library(dir)) => {
                log::warn!("one library at a time: not opening {dir} given at launch");
                notices.push(format!("{dir} was not opened: LightCraft opens one library at a time. Open it with File ▸ Open Library…"));
            }
            Some(LaunchPath::Import(p)) => files.push(p),
            Some(LaunchPath::Refused { path, why }) => {
                log::warn!("not importing {path} given at launch: {why}");
                notices
                    .push(format!("{path} was not imported: it is {why}. To import from it, choose a folder inside it with File ▸ Import Photos…"));
            }
            None => {}
        }
    }
    LaunchArgs { files, library, notices }
}

/// Where the log files live: `logs` in the settings folder, next to `ui.json` (see `logging`).
/// None with `LIGHTCRAFT_NO_PREFS` (tests, scripts), so those runs don't rotate away the user's logs.
pub(crate) fn log_dir() -> Option<std::path::PathBuf> {
    if std::env::var_os("LIGHTCRAFT_NO_PREFS").is_some() {
        return None;
    }
    config_dir().map(|d| d.join("logs"))
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

/// An isolated profile keeps the emergency crash log with its other app data. With no
/// override (or an empty override), retain the historical system-temp location.
pub(crate) fn panic_log_path_with(env: impl Fn(&str) -> Option<std::ffi::OsString>, temp: std::path::PathBuf) -> std::path::PathBuf {
    env("LIGHTCRAFT_CONFIG_DIR").filter(|value| !value.is_empty()).map(std::path::PathBuf::from).unwrap_or(temp).join("lightcraft-panics.log")
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

    #[test]
    fn config_override_keeps_panic_log_in_isolated_profile() {
        let temp = std::path::PathBuf::from("temp");
        assert_eq!(panic_log_path_with(|_| Some("isolated".into()), temp.clone()), std::path::PathBuf::from("isolated/lightcraft-panics.log"));
        assert_eq!(panic_log_path_with(|_| None, temp.clone()), temp.join("lightcraft-panics.log"));
        assert_eq!(panic_log_path_with(|_| Some("".into()), temp.clone()), temp.join("lightcraft-panics.log"));
    }
}
