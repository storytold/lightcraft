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

/// The emergency crash log: `lightcraft-panics.log` in the settings folder, next to `ui.json` (an
/// isolated profile's with `LIGHTCRAFT_CONFIG_DIR`). Never the system temp folder: on Linux and
/// FreeBSD every user shares it, so another could create the file first or put a link there (issue
/// #2572). None (panics then go to standard error, and to the log file when there is one) for a
/// `--memory` session, which writes nothing (issue #164), with `LIGHTCRAFT_NO_PREFS` or with no
/// settings folder.
fn panic_log_path(in_memory: bool) -> Option<std::path::PathBuf> {
    panic_log_path_with(in_memory, std::env::var_os("LIGHTCRAFT_NO_PREFS").is_some(), config_dir)
}

/// `config` (the settings folder) is looked up only when the crash log is kept.
fn panic_log_path_with(in_memory: bool, no_prefs: bool, config: impl FnOnce() -> Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    if in_memory || no_prefs {
        return None;
    }
    config().map(|d| d.join("lightcraft-panics.log"))
}

/// The panic hooks: the engine guard's (standard error, and the crash log when there is one), then
/// the log file's.
pub(crate) fn install_panic_hooks(logger: Option<&'static super::logging::AppLogger>, in_memory: bool) {
    if let Some(path) = panic_log_path(in_memory) {
        lightcraft_engine::guard::install_hook(path);
    }
    if let Some(logger) = logger {
        super::logging::record_panics(logger);
    }
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

    /// #2572: the crash log is in the settings folder (an isolated profile's too: `config_dir` honours
    /// `LIGHTCRAFT_CONFIG_DIR`); without one (an empty override, no home folder), with
    /// `LIGHTCRAFT_NO_PREFS` or in a `--memory` session there is none, and the last two never look the
    /// folder up.
    #[test]
    fn panic_log_lives_in_the_settings_folder() {
        let settings = || Some(std::path::PathBuf::from("settings"));
        assert_eq!(panic_log_path_with(false, false, settings), Some(std::path::Path::new("settings").join("lightcraft-panics.log")));
        assert_eq!(panic_log_path_with(false, false, || None), None);
        let unasked = || -> Option<std::path::PathBuf> { panic!("the settings folder was looked up") };
        assert_eq!(panic_log_path_with(false, true, unasked), None);
        assert_eq!(panic_log_path_with(true, false, unasked), None);
    }

    /// #2572: a panic is kept in `lightcraft-panics.log` in the settings folder, never in the system
    /// temp folder (shared by every user on Linux and FreeBSD), and a `--memory` session keeps it
    /// nowhere. The hook is process-wide, so the panic happens in a child (this test binary again)
    /// with its own temp and home folders and no `LIGHTCRAFT_CONFIG_DIR`.
    #[test]
    fn a_panic_is_kept_in_the_settings_folder_not_the_temp_folder() {
        if let Some(mode) = std::env::var_os("LIGHTCRAFT_TEST_PANIC_LOG") {
            let settings = config_dir().unwrap();
            std::fs::create_dir_all(&settings).unwrap();
            println!("settings folder {}", settings.display());
            install_panic_hooks(None, mode == "memory");
            assert!(std::thread::spawn(|| panic!("lc-2572 probe")).join().is_err());
            return;
        }
        let scratch = std::env::temp_dir().join(format!("lightcraft-panic-log-{}", std::process::id()));
        for memory in [false, true] {
            let _ = std::fs::remove_dir_all(&scratch);
            let (temp, home) = (scratch.join("temp"), scratch.join("home"));
            std::fs::create_dir_all(&temp).unwrap();
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "launch::config_path_tests::a_panic_is_kept_in_the_settings_folder_not_the_temp_folder", "--nocapture"])
                .env("LIGHTCRAFT_TEST_PANIC_LOG", if memory { "memory" } else { "1" })
                .env_remove("LIGHTCRAFT_CONFIG_DIR")
                .env_remove("LIGHTCRAFT_NO_PREFS")
                .env("TMPDIR", &temp)
                .env("TMP", &temp)
                .env("TEMP", &temp)
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("APPDATA", home.join("AppData"))
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
            // Anywhere in a line: with one test thread (`RUST_TEST_THREADS=1`, as `xtask ci` sets on a
            // small machine) libtest prints the test's name first, on the same line.
            let settings = stdout.lines().find_map(|l| l.split_once("settings folder ").map(|(_, p)| p)).expect("the child didn't run the test");
            let kept = std::fs::read_to_string(std::path::Path::new(settings).join("lightcraft-panics.log")).unwrap_or_default();
            assert_eq!(kept.contains("lc-2572 probe"), !memory, "--memory {memory}: {kept:?} in {settings}");
            assert!(!temp.join("lightcraft-panics.log").exists(), "--memory {memory}: the panic was kept in the temp folder");
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
