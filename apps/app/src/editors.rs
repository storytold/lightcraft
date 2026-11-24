//! Starting external editors for Photo ▸ Edit In: PhotoCraft, found where its installers put it,
//! or any other application the user named in Settings ▸ External Editing.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The name the UI asks for when it means PhotoCraft (`lcc_ui_egui::state::PHOTOCRAFT`).
const PHOTOCRAFT: &str = lcc_ui_egui::state::PHOTOCRAFT;
/// PhotoCraft's application id (Flatpak, desktop entry), from `[third_party]` in `brand.toml`.
const PHOTOCRAFT_ID: &str = lcc_brand::THIRD_PARTY_PHOTOCRAFT_APP_ID;

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

/// Open every file in `paths` with one start of `app`, after `args` (a saved Edit In preset's
/// menu item on a batch). An argument holding `{file}` takes each file's path where it stands, so
/// then every file is a start of its own. Without arguments, as [`open_all_with`].
pub fn open_preset(paths: &[String], app: &str, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return match all_command(launcher(app)?, paths) {
            Some(mut c) => c.spawn().map(|_| ()).map_err(|e| e.to_string()),
            None => paths.iter().try_for_each(|p| open_with(p, app)),
        };
    }
    for mut c in preset_commands(launcher(app)?, paths, args)? {
        c.spawn().map(|_| ()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The starts that open all of `paths` in `launcher` with `args`: one, unless an argument holds
/// `{file}` (then one per file, the path where it stands). The system default application takes
/// no arguments.
fn preset_commands(launcher: Launcher, paths: &[String], args: &[String]) -> Result<Vec<Command>, String> {
    if launcher == Launcher::Default {
        return Err("the system's default application can't be given arguments: name the editor's program in the preset".into());
    }
    if args.iter().any(|a| a.contains("{file}")) {
        return Ok(paths
            .iter()
            .map(|f| {
                let with: Vec<String> = args.iter().map(|a| a.replace("{file}", f)).collect();
                start(&launcher, &[], &with)
            })
            .collect());
    }
    Ok(vec![start(&launcher, paths, args)])
}

/// One start of `launcher` with its arguments and then `paths` (`open` hands an application what
/// follows `--args`, so its files go first; a Flatpak's files sit between the `@@` marks).
fn start(launcher: &Launcher, paths: &[String], args: &[String]) -> Command {
    match launcher {
        Launcher::Program(p) => {
            let mut c = Command::new(p);
            c.args(args).args(paths);
            c
        }
        Launcher::MacApp(app) => {
            let mut c = Command::new("open");
            c.args(["-a", app.as_str()]).args(paths).arg("--args").args(args);
            c
        }
        Launcher::Flatpak => {
            let mut c = Command::new("flatpak");
            c.args(["run", "--file-forwarding", PHOTOCRAFT_ID, "@@"]).args(paths).arg("@@").args(args);
            c
        }
        Launcher::Default => Command::new("true"),
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
    find_photocraft().ok_or_else(|| "PhotoCraft isn't installed. Install it, or give where it is in Settings ▸ External Editing.".to_string())
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
    let on_path = |name: &Path| resolve_program(name, &path_dirs);
    let from_entry = || {
        desktop_entry_dirs(env).into_iter().find_map(|d| {
            let text = std::fs::read_to_string(d.join(format!("{PHOTOCRAFT_ID}.desktop"))).ok()?;
            desktop_exec(&text)
                // a wrapper (`Exec=env VAR=1 /opt/PhotoCraft/photocraft %F`) would be started with the photo only:
                // skip it and look further
                .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().contains("photocraft")))
                .and_then(|p| on_path(&p))
        })
    };
    let appimage = || {
        let home = env("HOME")?;
        let dirs = [Path::new(&home).join("Applications"), Path::new(&home).join(".local/bin")];
        // both folders, so the newest version wins whichever it is in
        let mut found: Vec<PathBuf> = dirs
            .iter()
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flat_map(|rd| rd.flatten().map(|e| e.path()).filter(|p| is_photocraft_appimage(p) && p.is_file()).collect::<Vec<_>>())
            .collect();
        // the newest version by name (1.10 after 1.9)
        found.sort_by_key(|p| version_key(&p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()));
        found.pop()
    };
    if let Some(p) = on_path(Path::new("photocraft")).or_else(from_entry).or_else(appimage) {
        return Some(Launcher::Program(p));
    }
    let flatpak = Command::new("flatpak").args(["info", PHOTOCRAFT_ID]).output().is_ok_and(|o| o.status.success());
    flatpak.then_some(Launcher::Flatpak)
}

/// A program from a desktop entry or the PATH: a path as it is (if it is a file), a bare name
/// looked for in `dirs`. A leading `/` counts as absolute on every platform (desktop entries are
/// a POSIX format; `Path::is_absolute` wants a drive letter on Windows, where the tests run too).
fn resolve_program(name: &Path, dirs: &[PathBuf]) -> Option<PathBuf> {
    if name.is_absolute() || name.starts_with("/") {
        return Some(name.to_path_buf()).filter(|p| p.is_file());
    }
    dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// A sort key that orders the digit runs in a name by value (`1.9` before `1.10`).
fn version_key(name: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let mut rest = name;
    while !rest.is_empty() {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let (num, tail) = rest.split_at(digits);
        let text_len = tail.find(|c: char| c.is_ascii_digit()).unwrap_or(tail.len());
        let (text, next) = tail.split_at(text_len);
        out.push((num.parse().unwrap_or(u64::MAX), text.to_string()));
        rest = next;
    }
    out
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

    /// A preset's menu item: one start with the arguments and every file of a batch; an argument
    /// holding `{file}` is a start per file.
    #[test]
    fn preset_items_start_the_editor_once_with_arguments() {
        let paths = [r"D:\Pictures\a.dng".to_string(), r"D:\Pictures\b c.dng".to_string()];
        let args = |c: &Command| c.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let cs = preset_commands(Launcher::Program(PathBuf::from(r"C:\DxO\PureRAW.exe")), &paths, &["-last".to_string()]).unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(
            (cs[0].get_program().to_string_lossy().into_owned(), args(&cs[0])),
            (r"C:\DxO\PureRAW.exe".to_string(), vec!["-last".to_string(), paths[0].clone(), paths[1].clone()])
        );
        let cs = preset_commands(Launcher::MacApp("PureRAW".into()), &paths, &["-last".to_string()]).unwrap();
        assert_eq!(args(&cs[0]), ["-a", "PureRAW", &paths[0], &paths[1], "--args", "-last"], "the files go before `open`'s `--args`");
        let cs = preset_commands(Launcher::Program(PathBuf::from("/opt/denoise")), &paths, &["--shot={file}".to_string()]).unwrap();
        assert_eq!(cs.len(), 2, "an argument with a path in it: one start per file");
        assert_eq!(args(&cs[1]), [format!("--shot={}", paths[1])]);
        assert!(args(&cs[1]).iter().all(|a| !a.contains(paths[0].as_str())), "{:?}", args(&cs[1]));
        let e = preset_commands(Launcher::Default, &paths, &["-x".to_string()]).unwrap_err();
        assert!(e.contains("default application"), "{e}");
    }

    #[test]
    fn windows_lookup_prefers_the_recorded_install_folder() {
        let env = |k: &str| match k {
            "ProgramFiles" => Some(r"C:\Program Files".to_string()),
            "LOCALAPPDATA" => Some(r"C:\Users\Simon\AppData\Local".to_string()),
            _ => None,
        };
        let c = windows_candidates(Some(r"D:\Apps\PhotoCraft\".into()), Some(r#""C:\Program Files\PhotoCraft\photocraft.exe""#.into()), env);
        let c: Vec<String> = c.iter().map(|p| p.to_string_lossy().replace('/', "\\")).collect();
        assert!(c[0].starts_with(r"D:\Apps\PhotoCraft") && c[0].ends_with("photocraft.exe"), "{c:?}");
        assert_eq!(c[1], r"C:\Program Files\PhotoCraft\photocraft.exe", "App Paths, quotes dropped");
        assert!(c[2].starts_with(r"C:\Program Files") && c[2].ends_with("photocraft.exe"), "{c:?}");
        assert!(c[3].contains("Programs") && c[3].ends_with("photocraft.exe"), "{c:?}");
        assert_eq!(c.len(), 4);
    }

    #[test]
    fn reg_query_output_is_read() {
        let out = "\r\nHKEY_LOCAL_MACHINE\\Software\\PhotoCraft\r\n    InstallDir    REG_SZ    C:\\Program Files\\PhotoCraft\\\r\n\r\n";
        assert_eq!(reg_value(out).as_deref(), Some(r"C:\Program Files\PhotoCraft\"));
        let default = "\r\nHKEY_LOCAL_MACHINE\\...\\photocraft.exe\r\n    (Default)    REG_SZ    C:\\Program Files\\PhotoCraft\\photocraft.exe\r\n";
        assert_eq!(reg_value(default).as_deref(), Some(r"C:\Program Files\PhotoCraft\photocraft.exe"));
        assert_eq!(reg_value("ERROR: The system was unable to find the specified registry key or value."), None);
    }

    #[test]
    fn desktop_entries_give_their_program() {
        let appimage = "[Desktop Entry]\nName=PhotoCraft\nExec=\"/home/simon/Apps/photocraft-1.2-linux-x86_64.AppImage\" %F\n";
        assert_eq!(desktop_exec(appimage), Some(PathBuf::from("/home/simon/Apps/photocraft-1.2-linux-x86_64.AppImage")));
        assert_eq!(desktop_exec("Exec=/opt/photocraft/photocraft %F\n"), Some(PathBuf::from("/opt/photocraft/photocraft")));
        assert_eq!(desktop_exec("Exec=photocraft %F\nTryExec=photocraft\n"), Some(PathBuf::from("photocraft")), "a bare name, for a PATH lookup");
        assert_eq!(desktop_exec("[Desktop Entry]\n"), None);
        let dirs = desktop_entry_dirs(|k| (k == "HOME").then(|| "/home/simon".to_string()));
        assert_eq!(dirs[0], PathBuf::from("/home/simon/.local/share/applications"));
        assert!(dirs.contains(&PathBuf::from("/usr/share/applications")));
    }

    #[test]
    fn a_bare_exec_name_is_found_on_the_path() {
        let a = std::env::temp_dir().join(format!("lc-editors-a-{}", std::process::id()));
        let b = std::env::temp_dir().join(format!("lc-editors-b-{}", std::process::id()));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("photocraft"), b"").unwrap();
        let dirs = vec![a.clone(), b.clone()];
        assert_eq!(resolve_program(Path::new("photocraft"), &dirs), Some(b.join("photocraft")));
        assert_eq!(resolve_program(Path::new("nothere"), &dirs), None);
        let abs = b.join("photocraft");
        assert_eq!(resolve_program(&abs, &[]), Some(abs.clone()), "a path needs no PATH");
        assert_eq!(resolve_program(&b.join("missing"), &dirs), None);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn appimage_versions_sort_numerically() {
        let mut names = ["photocraft-1.10-x86_64.appimage", "photocraft-1.9-x86_64.appimage", "photocraft-1.2-x86_64.appimage"];
        names.sort_by_key(|n| version_key(n));
        assert_eq!(names[2], "photocraft-1.10-x86_64.appimage");
        assert_eq!(names[0], "photocraft-1.2-x86_64.appimage");
    }

    #[test]
    fn appimages_and_spotlight_results_are_recognised() {
        assert!(is_photocraft_appimage(Path::new("/home/s/Applications/PhotoCraft-1.4.0-x86_64.AppImage")));
        assert!(is_photocraft_appimage(Path::new("/home/s/.local/bin/photocraft.appimage")));
        assert!(!is_photocraft_appimage(Path::new("/home/s/Applications/Other-1.0.AppImage")));
        assert!(!is_photocraft_appimage(Path::new("/home/s/Applications/photocraft.desktop")));
        let found = mdfind_app("\n/Volumes/X/Apps/PhotoCraft.app\n/Users/s/PhotoCraft.app\n");
        assert_eq!(found, Some(PathBuf::from("/Volumes/X/Apps/PhotoCraft.app")));
        assert_eq!(mdfind_app(""), None);
    }

    #[test]
    fn a_failed_open_says_why() {
        assert_eq!(open_result(true, ""), Ok(()));
        let e = open_result(false, "Unable to find application named 'PhotoCraft'\n");
        assert_eq!(e, Err("Unable to find application named 'PhotoCraft'".into()));
        assert!(open_result(false, "").is_err());
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
