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
                    .push(format!("{path} was not imported: it is {why}. To import from it, choose a folder inside it with File → Import Photos…"));
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

/// Prepare only the selected settings directory before installing the emergency hook. A new profile
/// may not have reached the normal logger yet; if preparation fails, keep the stderr fallback.
fn prepare_panic_log(path: std::path::PathBuf) -> Option<std::path::PathBuf> {
    let mut dirs = std::fs::DirBuilder::new();
    dirs.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        dirs.mode(0o700);
    }
    dirs.create(path.parent()?).ok()?;
    Some(path)
}

/// The panic hooks: the engine guard's (standard error, and the crash log when there is one), then
/// the log file's.
pub(crate) fn install_panic_hooks(logger: Option<&'static super::logging::AppLogger>, in_memory: bool) {
    if let Some(path) = panic_log_path(in_memory).and_then(prepare_panic_log) {
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
             Settings → Performance → Use the GPU for rendering turns the GPU back on"
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

    /// The emergency hook works before the logger creates a new profile's directory, but leaves
    /// no files or settings directory in no-persistence modes. A child isolates the panic hook
    /// and active profile; both possible settings paths and the temp directory are synthetic.
    #[test]
    fn a_panic_is_kept_in_the_settings_folder_not_the_temp_folder() {
        if let Ok(mode) = std::env::var("LIGHTCRAFT_TEST_PANIC_LOG") {
            let root = std::path::PathBuf::from(std::env::var_os("LIGHTCRAFT_TEST_PANIC_ROOT").unwrap());
            let settings = if mode == "nightly" {
                let profile = root.join("nightly");
                let settings = profile.join("Settings");
                lightcraft_engine::config::set_profile_root(profile).unwrap();
                settings
            } else {
                root.join("settings")
            };
            assert_eq!(config_dir(), Some(settings.clone()));
            assert!(!settings.exists(), "the logger has not initialized these settings");
            if mode == "existing" {
                std::fs::create_dir(&settings).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o750)).unwrap();
                }
            }
            #[cfg(unix)]
            let before = if mode == "existing" {
                use std::os::unix::fs::PermissionsExt;
                Some(std::fs::metadata(&settings).unwrap().permissions().mode())
            } else {
                None
            };

            install_panic_hooks(None, mode == "memory");
            let persists = mode != "memory" && mode != "no-prefs";
            if persists {
                assert!(settings.is_dir(), "{mode}: settings directory was not prepared");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let after = std::fs::metadata(&settings).unwrap().permissions().mode();
                    if let Some(before) = before {
                        assert_eq!(after, before, "existing settings permissions were changed");
                    } else {
                        assert_eq!(after & 0o077, 0, "{mode}: fresh settings directory is not private");
                    }
                }
            } else {
                assert!(!settings.exists(), "{mode}: settings directory was created by startup");
            }
            assert!(std::thread::spawn(|| panic!("lc-2572 probe")).join().is_err());
            let kept = std::fs::read_to_string(settings.join("lightcraft-panics.log")).unwrap_or_default();
            assert_eq!(kept.contains("lc-2572 probe"), persists, "{mode}: {kept:?}");
            if !persists {
                assert!(!settings.exists(), "{mode}: the hook created a settings directory");
            }
            assert!(!root.join("temp/lightcraft-panics.log").exists(), "{mode}: crash log fell back to temp");
            if mode == "nightly" {
                assert!(!root.join("settings").exists(), "Nightly did not initialize the other profile");
            }
            return;
        }

        let scratch = std::env::temp_dir().join(format!("lightcraft-panic-log-{}", std::process::id()));
        std::fs::create_dir(&scratch).unwrap();
        for mode in ["first-run", "nightly", "existing", "memory", "no-prefs"] {
            let root = scratch.join(mode);
            let temp = root.join("temp");
            std::fs::create_dir_all(&temp).unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", "launch::config_path_tests::a_panic_is_kept_in_the_settings_folder_not_the_temp_folder", "--nocapture"])
                .env("LIGHTCRAFT_TEST_PANIC_LOG", mode)
                .env("LIGHTCRAFT_TEST_PANIC_ROOT", &root)
                .env("LIGHTCRAFT_CONFIG_DIR", root.join("settings"))
                .env_remove("LIGHTCRAFT_NO_PREFS")
                .env("TMPDIR", &temp)
                .env("TMP", &temp)
                .env("TEMP", &temp);
            if mode == "no-prefs" {
                child.env("LIGHTCRAFT_NO_PREFS", "1");
            }
            let out = child.output().unwrap();
            assert!(out.status.success(), "{mode}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        }
        std::fs::remove_dir_all(&scratch).unwrap();
    }
}
