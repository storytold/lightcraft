//! Starting external editors for Photo ▸ Edit In: PhotoCraft, found where its installers put it,
//! or any other application the user named in Settings ▸ External Editing.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The name the UI asks for when it means PhotoCraft (`lightcraft_ui_egui::state::PHOTOCRAFT`).
const PHOTOCRAFT: &str = lightcraft_ui_egui::state::PHOTOCRAFT;
/// PhotoCraft's application id (Flatpak, desktop entry).
const PHOTOCRAFT_ID: &str = "ai.storyteller.photocraft";

/// How an editor is started.
#[derive(Debug, PartialEq)]
enum Launcher {
    /// The system's default application for the file.
    Default,
    /// A program file, given the photo as its argument.
    Program(PathBuf),
    /// macOS: `open -a <app>` (an application name or bundle path).
    MacApp(String),
    /// The PhotoCraft Flatpak, with the file forwarded into its sandbox.
    Flatpak,
}

/// Whether `app` can be started (Settings ▸ External Editing names it; "" = the system default):
/// for PhotoCraft, that it is installed. The error says what to do instead.
pub fn check(app: &str) -> Result<(), String> {
    launcher(app).map(|_| ())
}

/// Open `path` in `app` (see [`check`]). Spawned, never waited for: the editor runs alongside.
pub fn open_with(path: &str, app: &str) -> Result<(), String> {
    let mut c = match launcher(app)? {
        Launcher::Default if cfg!(target_os = "macos") => return run_open(&[path]),
        Launcher::Default if cfg!(target_os = "windows") => windows_default_command(path),
        Launcher::Default => {
            let mut c = Command::new("xdg-open");
            c.arg(path);
            c
        }
        Launcher::MacApp(app) => return run_open(&["-a", &app, path]),
        Launcher::Program(p) => {
            let mut c = Command::new(p);
            c.arg(path);
            c
        }
        Launcher::Flatpak => {
            let mut c = Command::new("flatpak");
            c.args(["run", "--file-forwarding", PHOTOCRAFT_ID, "@@", path, "@@"]);
            c
        }
    };
    c.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// macOS: run `open` and wait for it (it returns as soon as LaunchServices has handed the file
/// over), so an application it can't find or start is reported instead of silently doing nothing.
fn run_open(args: &[&str]) -> Result<(), String> {
    let out = Command::new("open").args(args).output().map_err(|e| e.to_string())?;
    open_result(out.status.success(), &String::from_utf8_lossy(&out.stderr))
}

/// What `open` said, as a result: its own message when it failed.
fn open_result(ok: bool, stderr: &str) -> Result<(), String> {
    if ok {
        return Ok(());
    }
    let msg = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("the application couldn't be started");
    Err(msg.to_string())
}

/// Open every file in `paths` with one start of `app`, as a file manager hands over a selection:
/// an editor that keeps a single window (DxO PhotoLab) loses the files that arrive while it is
/// still starting when it is started once per file. The system default opens them one by one
/// (Explorer and `xdg-open` take one file).
pub fn open_all_with(paths: &[String], app: &str) -> Result<(), String> {
    match all_command(launcher(app)?, paths) {
        Some(mut c) => c.spawn().map(|_| ()).map_err(|e| e.to_string()),
        None => paths.iter().try_for_each(|p| open_with(p, app)),
    }
}

/// The one command that opens all of `paths` in `launcher` (none for the system default).
fn all_command(launcher: Launcher, paths: &[String]) -> Option<Command> {
    match launcher {
        Launcher::Default => None,
        Launcher::MacApp(app) => {
            let mut c = Command::new("open");
            c.args(["-a", &app]).args(paths);
            Some(c)
        }
        Launcher::Program(p) => {
            let mut c = Command::new(p);
            c.args(paths);
            Some(c)
        }
        Launcher::Flatpak => {
            let mut c = Command::new("flatpak");
            c.args(["run", "--file-forwarding", PHOTOCRAFT_ID, "@@"]).args(paths).arg("@@");
            Some(c)
        }
    }
}

/// Windows: open `path` in its default application through Explorer, given the path as its one
/// argument. Not `cmd /C start`: cmd.exe would read `&`, `^`, `%` or `|` in a file name as its own
/// syntax (running what follows an `&` as a command).
fn windows_default_command(path: &str) -> Command {
    let mut c = Command::new("explorer.exe");
    c.arg(path.replace('/', "\\"));
    c
}

fn launcher(app: &str) -> Result<Launcher, String> {
    let app = app.trim();
    if app.is_empty() {
        return Ok(Launcher::Default);
    }
    if !app.eq_ignore_ascii_case(PHOTOCRAFT) {
        return Ok(if cfg!(target_os = "macos") { Launcher::MacApp(app.to_string()) } else { Launcher::Program(PathBuf::from(app)) });
    }
    find_photocraft().ok_or_else(|| {
        "PhotoCraft isn't installed. Get it from getartcraft.com/apps/photocraft, or give where it is in Settings ▸ External Editing.".to_string()
    })
}

fn find_photocraft() -> Option<Launcher> {
    let env = |k: &str| std::env::var_os(k).map(|v| v.to_string_lossy().to_string());
    if cfg!(target_os = "macos") {
        // the usual places, then wherever Spotlight knows it (by bundle id or bundle name)
        let home = env("HOME").map(|h| Path::new(&h).join("Applications/PhotoCraft.app"));
        let usual = [Some(PathBuf::from("/Applications/PhotoCraft.app")), home].into_iter().flatten().find(|p| p.is_dir());
        let spotlight = || {
            let query = format!("kMDItemCFBundleIdentifier == '{PHOTOCRAFT_ID}' || kMDItemFSName == 'PhotoCraft.app'");
            let out = Command::new("mdfind").arg(query).output().ok().filter(|o| o.status.success())?;
            mdfind_app(&String::from_utf8_lossy(&out.stdout))
        };
        return usual.or_else(spotlight).map(|p| Launcher::MacApp(p.to_string_lossy().to_string()));
    }
    if cfg!(target_os = "windows") {
        let install_dir = reg_query(r"HKLM\Software\PhotoCraft", Some("InstallDir"));
        let app_path = reg_query(r"HKLM\Software\Microsoft\Windows\CurrentVersion\App Paths\photocraft.exe", None);
        return windows_candidates(install_dir, app_path, env).into_iter().find(|p| p.is_file()).map(Launcher::Program);
    }
    // Linux, FreeBSD: on the PATH (.deb, .rpm, tarball), a desktop entry (AppImage, which
    // registers one on first run; its Exec a path or a name on the PATH), an AppImage where people
    // keep them, else the Flatpak
    let path_dirs: Vec<PathBuf> = env("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).collect();
    let on_path = |name: &Path| -> Option<PathBuf> {
        if name.is_absolute() || name.starts_with("/") {
            return Some(name.to_path_buf()).filter(|p| p.is_file());
        }
        path_dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
    };
    let from_entry = || {
        desktop_entry_dirs(env).into_iter().find_map(|d| {
            let text = std::fs::read_to_string(d.join(format!("{PHOTOCRAFT_ID}.desktop"))).ok()?;
            desktop_exec(&text).and_then(|p| on_path(&p))
        })
    };
    let appimage = || {
        let home = env("HOME")?;
        let dirs = [Path::new(&home).join("Applications"), Path::new(&home).join(".local/bin")];
        dirs.iter().find_map(|d| {
            let mut found: Vec<PathBuf> =
                std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).filter(|p| is_photocraft_appimage(p) && p.is_file()).collect();
            // the newest version by name
            found.sort();
            found.pop()
        })
    };
    if let Some(p) = on_path(Path::new("photocraft")).or_else(from_entry).or_else(appimage) {
        return Some(Launcher::Program(p));
    }
    let flatpak = Command::new("flatpak").args(["info", PHOTOCRAFT_ID]).output().is_ok_and(|o| o.status.success());
    flatpak.then_some(Launcher::Flatpak)
}

/// Where the Windows installers put `photocraft.exe`, most specific first: the folder the MSI
/// recorded (`HKLM\Software\PhotoCraft\InstallDir`), its App Paths entry, then the default
/// folders (per machine, then per user).
fn windows_candidates(install_dir: Option<String>, app_path: Option<String>, env: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    out.extend(install_dir.map(|d| Path::new(&d).join("photocraft.exe")));
    out.extend(app_path.map(|p| PathBuf::from(p.trim_matches('"'))));
    for var in ["ProgramW6432", "ProgramFiles"] {
        out.extend(env(var).map(|d| Path::new(&d).join("PhotoCraft").join("photocraft.exe")));
    }
    out.extend(env("LOCALAPPDATA").map(|d| Path::new(&d).join("Programs").join("PhotoCraft").join("photocraft.exe")));
    out
}

/// A string value from the Windows registry (`reg query`; `None` = the key's default value).
fn reg_query(key: &str, name: Option<&str>) -> Option<String> {
    let mut c = Command::new("reg");
    c.args(["query", key]);
    match name {
        Some(n) => c.args(["/v", n]),
        None => c.arg("/ve"),
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    let out = c.output().ok().filter(|o| o.status.success())?;
    reg_value(&String::from_utf8_lossy(&out.stdout))
}

/// The value in `reg query` output: the text after `REG_SZ` (or `REG_EXPAND_SZ`) on its line.
fn reg_value(output: &str) -> Option<String> {
    output.lines().find_map(|l| {
        let (_, v) = l.split_once("REG_EXPAND_SZ").or_else(|| l.split_once("REG_SZ"))?;
        Some(v.trim().to_string()).filter(|v| !v.is_empty())
    })
}

/// The folders desktop entries live in (XDG base directories).
fn desktop_entry_dirs(env: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match env("XDG_DATA_HOME") {
        Some(d) if !d.is_empty() => dirs.push(PathBuf::from(d)),
        _ => dirs.extend(env("HOME").map(|h| Path::new(&h).join(".local/share"))),
    }
    let system = env("XDG_DATA_DIRS").filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs.into_iter().map(|d| d.join("applications")).collect()
}

/// The program a desktop entry's `Exec=` line starts (quoted or not, its field codes dropped).
fn desktop_exec(entry: &str) -> Option<PathBuf> {
    let exec = entry.lines().find_map(|l| l.strip_prefix("Exec="))?.trim();
    let program = match exec.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => exec.split_whitespace().next()?,
    };
    // a path, or a bare name (`photocraft %F`) the caller looks for on the PATH
    (!program.is_empty()).then(|| PathBuf::from(program))
}

/// An AppImage of PhotoCraft by its file name (`PhotoCraft-1.2-x86_64.AppImage`, any case).
fn is_photocraft_appimage(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    name.contains("photocraft") && name.ends_with(".appimage")
}

/// The first application bundle in `mdfind` output (one path per line).
fn mdfind_app(output: &str) -> Option<PathBuf> {
    output.lines().map(str::trim).find(|l| l.ends_with(".app")).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file name with cmd.exe syntax in it goes to Explorer as one plain argument, not
    /// through a shell.
    #[test]
    fn windows_default_open_goes_through_explorer_not_cmd() {
        let path = r"C:/Edits/a & b ^c %PATH% | d.psd";
        let c = windows_default_command(path);
        assert_eq!(c.get_program(), "explorer.exe");
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, [r"C:\Edits\a & b ^c %PATH% | d.psd"]);
    }

    /// Open Original with several photos: one start of the editor with every file, which a
    /// single-window editor (DxO PhotoLab) needs to get them all.
    #[test]
    fn several_files_go_to_one_start_of_the_editor() {
        let paths = [r"D:\Pictures\a.dng".to_string(), r"D:\Pictures\b c.dng".to_string()];
        let args = |c: &Command| c.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let c = all_command(Launcher::Program(PathBuf::from(r"C:\DxO\DxO.PhotoLab.exe")), &paths).unwrap();
        assert_eq!(c.get_program(), r"C:\DxO\DxO.PhotoLab.exe");
        assert_eq!(args(&c), paths);
        let c = all_command(Launcher::MacApp("DxO PhotoLab 8".into()), &paths).unwrap();
        assert_eq!(
            (c.get_program().to_string_lossy().into_owned(), args(&c)),
            ("open".into(), vec!["-a".into(), "DxO PhotoLab 8".into(), paths[0].clone(), paths[1].clone()])
        );
        let c = all_command(Launcher::Flatpak, &paths).unwrap();
        assert_eq!(args(&c), ["run", "--file-forwarding", PHOTOCRAFT_ID, "@@", &paths[0], &paths[1], "@@"]);
        assert!(all_command(Launcher::Default, &paths).is_none(), "the system default opens them one by one");
    }

    #[test]
    fn a_failed_open_is_an_error() {
        assert!(open_result(true, "").is_ok());
        assert!(open_result(false, "Unable to find application\n").is_err());
        assert!(open_result(false, "").is_err(), "even when it says nothing");
    }

    #[test]
    fn odd_detection_input_does_not_panic() {
        for text in ["", "Exec=", "Exec=\"", "Exec=\"\"", "\u{0}\u{fffd}", "Exec= %F", "[Desktop Entry]\nExec=\u{e9}\u{301}"] {
            let _ = desktop_exec(text);
            let _ = mdfind_app(text);
            let _ = reg_value(text);
        }
        for p in ["", "/", ".AppImage", "\u{e9}.appimage", "a/../b"] {
            let _ = is_photocraft_appimage(Path::new(p));
        }
        let _ = desktop_entry_dirs(|_| None);
        let _ = windows_candidates(None, Some("\"".into()), |_| None);
    }

    #[test]
    fn other_editors_are_used_as_named() {
        assert_eq!(launcher("").unwrap(), Launcher::Default);
        assert_eq!(launcher("  ").unwrap(), Launcher::Default);
        let named = launcher("/usr/bin/gimp").unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(named, Launcher::MacApp("/usr/bin/gimp".into()));
        } else {
            assert_eq!(named, Launcher::Program("/usr/bin/gimp".into()));
        }
    }
}
