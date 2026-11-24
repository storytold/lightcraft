//! Linux AppImage: registering a launcher entry and icon for the running image.
//!
//! A Wayland compositor (and an X11 taskbar) names a window by its app id and looks that id up among the
//! desktop entries in the XDG data directories; the icon comes from that entry's `Icon=`. The entry and the
//! icons shipped inside an AppImage are not on that search path, so an image that was never "integrated"
//! shows as the generic icon. When started from an AppImage (`APPIMAGE` is set) the app writes
//! `<data>/applications/<APP_ID>.desktop` into the user's data directory, pointing at itself, and the
//! hicolor icons beside it.
//!
//! The entry is the package's own template (`packaging/linux/<app_id>.desktop.in`) with the brand values
//! filled in by `build.rs`, so name, comment, categories and MIME list cannot drift from the package; only
//! `Exec=` differs: the path of this AppImage.
//!
//! Rules:
//! - Only when the AppImage runtime started this process: `APPIMAGE` names an existing regular file by an
//!   absolute path without control characters, and the program runs from inside the runtime's mount
//!   (`APPDIR`). The variables are inherited by everything an AppImage starts, so a build or a test run
//!   from a terminal inside another AppImage registers nothing.
//! - A desktop entry for the app id anywhere in `XDG_DATA_DIRS` (a system package), or in the user's data
//!   directory without our [`GENERATED_BY`] key (a launcher tool's, such as AppImageLauncher's), is left alone.
//! - An entry of ours is rewritten when it points at another AppImage (the file was moved or renamed); the
//!   icons are written when missing or of another size.
//! - Nothing here fails a start-up: every problem is a warning, and the work runs on a thread of its own.
//! - Nothing is ever deleted: an entry stays when its AppImage is gone.
//!
//! The decisions are pure functions of paths and texts ([`exec_for`], [`entry_for`], [`is_ours`],
//! [`data_home`], [`system_data_dirs`], [`plan`]); only [`install_from_env`] reads the environment.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::{Path, PathBuf};

// `TEMPLATE`: the package's desktop entry with the brand values filled in, `Exec=` holding `{{EXEC}}`;
// `ICONS`: `(path under hicolor/, bytes)`, installed under the app id (build.rs).
include!(concat!(env!("OUT_DIR"), "/desktop_assets.rs"));
embedded_desktop_assets!();

/// Value of the `X-Generated-By` key in an entry we wrote: the program name. Only such an entry is ours.
const GENERATED_BY: &str = lcc_brand::BINARY;
/// Our keys: who wrote the entry, and which AppImage it launches (the path as it is, not escaped).
const GENERATED_BY_KEY: &str = "X-Generated-By";
const SOURCE_KEY: &str = "X-AppImage-Source";

/// An entry of ours is a few hundred bytes; a larger file is not read.
const MAX_ENTRY_BYTES: u64 = 64 * 1024;
/// The AppImage path we accept is at most this long (the entry must stay under [`MAX_ENTRY_BYTES`]).
const MAX_PATH_CHARS: usize = 4096;

/// `Exec=` for a program at `path`: the escaping rule of the Desktop Entry Specification, inside double
/// quotes (so spaces and the reserved characters are safe), then ` %F`. `None` for a path that cannot be
/// written: empty, relative, too long, holding a control character (a newline would add a line to the
/// entry, and our source key is not escaped), or a double quote, dollar or backtick (readers of the
/// entry disagree on how those are escaped: `desktop-file-validate` 0.29 rejects the specification's
/// escapes, so such a path gets no entry).
pub fn exec_for(path: &str) -> Option<String> {
    if !usable_path(path) {
        return None;
    }
    let mut quoted = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            _ => quoted.push(c),
        }
    }
    Some(format!("\"{quoted}\" %F"))
}

fn usable_path(path: &str) -> bool {
    !path.is_empty()
        && path.starts_with('/')
        && path.chars().count() <= MAX_PATH_CHARS
        && !path.chars().any(|c| c.is_control() || matches!(c, '"' | '$' | '`'))
}

/// The desktop entry for an AppImage at `path`, our two keys appended; `None` when `path` is unusable.
pub fn entry_for(path: &str) -> Option<String> {
    let exec = exec_for(path)?;
    if TEMPLATE.is_empty() {
        return None;
    }
    let mut text = TEMPLATE.replacen("{{EXEC}}", &exec, 1);
    text.push_str(GENERATED_BY_KEY);
    text.push('=');
    text.push_str(GENERATED_BY);
    text.push('\n');
    text.push_str(SOURCE_KEY);
    text.push('=');
    text.push_str(path);
    text.push('\n');
    Some(text)
}

/// The value of `key` in the `[Desktop Entry]` group of `text`, `None` when the key is absent there.
/// Keys are matched as whole lines, so an `Exec=` inside another value does not count.
pub fn group_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().take_while(|l| !l.starts_with('[') || *l == "[Desktop Entry]").find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
}

/// Whether `text` is a desktop entry this module wrote: it carries both of our keys with our values.
pub fn is_ours(text: &str) -> bool {
    group_value(text, GENERATED_BY_KEY) == Some(GENERATED_BY) && recorded_source(text).is_some()
}

/// The AppImage path recorded in an entry: the value of our source key when it is a path we could have
/// written.
pub fn recorded_source(text: &str) -> Option<&str> {
    group_value(text, SOURCE_KEY).filter(|s| usable_path(s))
}

/// The user's data directory: `$XDG_DATA_HOME` when it is an absolute path, else `$HOME/.local/share`;
/// `None` when neither says (no home folder: nothing is written).
pub fn data_home(xdg_data_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(d) = xdg_data_home.filter(|d| d.starts_with('/') && !d.chars().any(char::is_control)) {
        return Some(PathBuf::from(d));
    }
    let home = home.filter(|h| h.starts_with('/') && !h.chars().any(char::is_control))?;
    Some(PathBuf::from(home).join(".local/share"))
}

/// The data directories that come before the user's: the absolute paths in `$XDG_DATA_DIRS`, or the
/// specification's default when it is unset or empty.
pub fn system_data_dirs(xdg_data_dirs: Option<&str>) -> Vec<PathBuf> {
    let list = xdg_data_dirs.filter(|l| !l.is_empty());
    let dirs: Vec<&str> = match list {
        Some(l) => l.split(':').filter(|d| d.starts_with('/') && !d.chars().any(char::is_control)).collect(),
        None => vec!["/usr/local/share", "/usr/share"],
    };
    dirs.into_iter().map(PathBuf::from).collect()
}

/// Where a desktop entry for the app id is looked up, in order: the system directories, then the user's.
pub fn entry_paths(system_dirs: &[PathBuf], data_home: &Path) -> Vec<PathBuf> {
    let name = format!("{}.desktop", lcc_brand::APP_ID);
    system_dirs.iter().map(|d| d.join("applications").join(&name)).chain(std::iter::once(data_home.join("applications").join(&name))).collect()
}

/// The hicolor icon destinations under `icons_dir` (`…/icons/hicolor`), in `ICONS` order.
pub fn icon_destinations(icons_dir: &Path) -> Vec<(PathBuf, &'static [u8])> {
    ICONS
        .iter()
        .map(|(rel, bytes)| {
            let rel = Path::new(rel);
            let ext = rel.extension().and_then(|e| e.to_str()).unwrap_or("png");
            let name = format!("{}.{}", lcc_brand::APP_ID, ext);
            (rel.parent().unwrap_or(Path::new("scalable/apps")).join(name), *bytes)
        })
        .map(|(rel, bytes)| (icons_dir.join(rel), bytes))
        .collect()
}

/// Whether to write the user's desktop entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Another entry is in charge (a system package's, a launcher tool's, the user's own), or ours already
    /// launches this AppImage and every icon is in place.
    Leave,
    /// Write the entry (there is none, or ours launches another AppImage) and the icons that differ.
    Install,
}

/// `entry_exists`: a file (or anything else) is at that path; `own_text`: the user's entry when it could be
/// read; `icon_differs`: an icon is missing or not of our size.
pub fn plan(system_entry_exists: bool, own_exists: bool, own_text: Option<&str>, image: &str, icon_differs: bool) -> Action {
    if system_entry_exists {
        return Action::Leave;
    }
    match own_text {
        // no file of ours to keep: write one
        _ if !own_exists => Action::Install,
        Some(text) if is_ours(text) => {
            if recorded_source(text) == Some(image) && !icon_differs {
                Action::Leave
            } else {
                Action::Install
            }
        }
        // there, but not ours or not readable: never overwritten
        _ => Action::Leave,
    }
}

/// Whether a file is there (a link, a directory or anything else counts as taken).
fn occupied(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Reads a small regular file; `None` when there is no such file, it is larger than [`MAX_ENTRY_BYTES`] or
/// cannot be read.
fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ENTRY_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Writes `bytes` to `path` atomically (a new temporary file in the same directory, then renamed over it),
/// creating the directory. A temporary file that could not be renamed is removed.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("entry");
    // two instances starting together get different names (pid, time, counter); `create_new` makes sure
    let mut last = None;
    for attempt in 0..16u32 {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let tmp = dir.join(format!(".{stem}.{stamp}.{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let written = (|| {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            let r = f.write_all(bytes).and_then(|()| f.sync_all());
            drop(f);
            r
        })();
        match written {
            Ok(()) => match std::fs::rename(&tmp, path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    last = Some(e);
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 15 => continue,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                last = Some(e);
            }
        }
        break;
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("could not create a temporary file")))
}

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether the installed icon is missing, not a regular file or not of our byte count.
fn icon_differs(path: &Path, bytes: &[u8]) -> bool {
    std::fs::metadata(path).map(|m| !m.is_file() || m.len() != bytes.len() as u64).unwrap_or(true)
}

/// Compares what is installed with what this AppImage needs and writes only what differs. The first error
/// is returned after everything was tried (the entry is written before the icons, as the entry is what the
/// taskbar needs); each candidate is read once, so a start where nothing changed costs a few `stat`s.
pub fn apply(image: &str, system_dirs: &[PathBuf], data_home: &Path) -> std::io::Result<()> {
    let paths = entry_paths(system_dirs, data_home);
    let Some((own_path, system_paths)) = paths.split_last() else { return Ok(()) };
    let system_entry_exists = system_paths.iter().any(|p| occupied(p));
    let own_exists = occupied(own_path);
    let own_text = own_exists.then(|| read_small(own_path)).flatten();
    let icons = icon_destinations(&data_home.join("icons").join("hicolor"));
    let icons_differ = icons.iter().any(|(p, bytes)| icon_differs(p, bytes));
    if plan(system_entry_exists, own_exists, own_text.as_deref(), image, icons_differ) == Action::Leave {
        return Ok(());
    }
    let Some(entry) = entry_for(image) else {
        return Err(std::io::Error::other("the AppImage path cannot be written to a desktop entry"));
    };
    let mut first = write_atomic(own_path, entry.as_bytes()).err();
    for (path, bytes) in &icons {
        if icon_differs(path, bytes)
            && let Err(e) = write_atomic(path, bytes)
        {
            first.get_or_insert(e);
        }
    }
    match first {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// The work for one AppImage path with the environment's directories.
pub fn integrate(image: &str) -> std::io::Result<()> {
    let system_dirs = system_data_dirs(std::env::var("XDG_DATA_DIRS").ok().as_deref());
    let xdg = std::env::var("XDG_DATA_HOME").ok();
    let home = std::env::var("HOME").ok();
    let Some(data) = data_home(xdg.as_deref(), home.as_deref()) else {
        return Err(std::io::Error::other("neither XDG_DATA_HOME nor HOME names a folder"));
    };
    apply(image, &system_dirs, &data)
}

/// Registers the running AppImage on a thread of its own, so nothing here is on the start-up path. Returns
/// whether a thread was started.
pub fn install_from_env() -> bool {
    #[cfg(target_os = "linux")]
    {
        // `APPIMAGE` is inherited by everything an AppImage starts (a terminal or an IDE run from one
        // passes it to `cargo test`, and to any program it builds): only the process the AppImage
        // runtime started runs from inside its mount, `APPDIR`.
        let inside =
            !lcc_brand::is_test_binary() && runs_inside_mount(std::env::var_os("APPDIR").as_deref(), std::env::current_exe().ok().as_deref());
        let Some(value) = std::env::var_os("APPIMAGE").filter(|_| inside) else { return false };
        let Some(image) = appimage_path(value.as_ref()) else {
            log::warn!("{}: the launcher entry was not registered: APPIMAGE does not name a file by an absolute path", lcc_brand::DISPLAY_NAME);
            return false;
        };
        std::thread::Builder::new()
            .name("desktop-entry".into())
            .spawn(move || {
                if let Err(e) = integrate(&image) {
                    log::warn!("{}: the launcher entry was not registered: {e}", lcc_brand::DISPLAY_NAME);
                }
            })
            .is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// `APPIMAGE` as a path we would launch: an existing regular file (a link to one is fine) by an absolute
/// path without control characters. Not canonicalised: the entry starts the file the user ran.
#[cfg(target_os = "linux")]
fn appimage_path(value: &std::ffi::OsStr) -> Option<String> {
    use std::os::unix::ffi::OsStrExt as _;
    let bytes = value.as_bytes();
    let s = std::str::from_utf8(bytes).ok()?;
    if !usable_path(s) || !std::fs::metadata(s).is_ok_and(|m| m.is_file()) {
        return None;
    }
    Some(s.to_string())
}

/// Whether the running program is `exe` inside the AppImage runtime's mount `appdir`: both absolute,
/// both existing, and the program's real path under the mount's real path (links followed).
#[cfg(target_os = "linux")]
fn runs_inside_mount(appdir: Option<&std::ffi::OsStr>, exe: Option<&std::path::Path>) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Some(appdir) = appdir.filter(|d| d.as_bytes().starts_with(b"/") && !d.is_empty()) else { return false };
    let Some(exe) = exe.filter(|e| e.is_absolute()) else { return false };
    let (Ok(dir), Ok(exe)) = (std::fs::canonicalize(appdir), std::fs::canonicalize(exe)) else { return false };
    exe.starts_with(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of this test's own, removed when the test ends.
    struct Tmp(PathBuf);

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp(tag: &str) -> Tmp {
        let dir = std::env::temp_dir().join(format!(
            "lcc-desktop-entry-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the test's temporary folder");
        Tmp(dir)
    }

    fn entry_name() -> String {
        format!("{}.desktop", lcc_brand::APP_ID)
    }

    #[test]
    fn exec_is_quoted_and_escaped() {
        assert_eq!(exec_for("/opt/Apps/app image.AppImage").as_deref(), Some("\"/opt/Apps/app image.AppImage\" %F"));
        assert_eq!(exec_for("/a\\b c.AppImage").as_deref(), Some("\"/a\\\\b c.AppImage\" %F"));
        // a double quote, dollar or backtick: no entry rather than one some launchers misread
        assert_eq!(exec_for("/a\"b.AppImage"), None);
        assert_eq!(exec_for("/a$b.AppImage"), None);
        assert_eq!(exec_for("/a\u{60}b.AppImage"), None);
        // unicode and the reserved characters are kept as they are inside the quotes
        assert_eq!(exec_for("/home/朱/фото;>&<|~.AppImage").as_deref(), Some("\"/home/朱/фото;>&<|~.AppImage\" %F"));
    }

    #[test]
    fn exec_refuses_paths_that_would_break_the_entry() {
        for bad in [
            "",
            "relative.AppImage",
            "./a.AppImage",
            "/a\nb.AppImage",
            "/a\r.AppImage",
            "/a\tb.AppImage",
            "/a\0b",
            "/a\"b.AppImage",
            "/a\u{7f}b.AppImage",
        ] {
            assert_eq!(exec_for(bad), None, "{bad:?}");
            assert_eq!(entry_for(bad), None, "{bad:?}");
        }
        let long = format!("/{}", "a".repeat(MAX_PATH_CHARS));
        assert_eq!(exec_for(&long), None);
    }

    #[test]
    fn entry_is_the_package_template_with_this_image() {
        let text = entry_for("/x/y image.AppImage").unwrap_or_default();
        assert!(text.contains("Exec=\"/x/y image.AppImage\" %F\n"));
        assert_eq!(group_value(&text, "Icon"), Some(lcc_brand::APP_ID));
        assert_eq!(group_value(&text, "StartupWMClass"), Some(lcc_brand::APP_ID));
        assert_eq!(group_value(&text, "Type"), Some("Application"));
        assert_eq!(group_value(&text, GENERATED_BY_KEY), Some(GENERATED_BY));
        assert_eq!(recorded_source(&text), Some("/x/y image.AppImage"));
        assert!(is_ours(&text));
        // the package's program name is gone, and no placeholder is left
        assert!(!text.contains("{{"));
        assert!(!text.lines().any(|l| l.starts_with("Exec=") && l.contains(lcc_brand::BINARY)));
        assert!(text.len() as u64 <= MAX_ENTRY_BYTES);
        // a key inside another value is not the key
        assert_eq!(group_value("Exec=a\nX=Exec=b", "Exec"), Some("a"));
    }

    #[test]
    fn only_our_own_entry_is_ours() {
        let foreign = "Exec=\"/x\" %F\n".to_string();
        assert!(!is_ours(&foreign));
        // the generated-by key alone, or with a source we could not have written, is not ours
        let half = format!("{foreign}{GENERATED_BY_KEY}={GENERATED_BY}\n");
        assert!(!is_ours(&half));
        let bad_source = format!("{half}{SOURCE_KEY}=relative\n");
        assert!(!is_ours(&bad_source));
        // keys in another group do not count
        let other_group = format!("[Desktop Action x]\n{GENERATED_BY_KEY}={GENERATED_BY}\n{SOURCE_KEY}=/x\n");
        assert!(!is_ours(&other_group));
    }

    #[test]
    fn directories_follow_the_spec() {
        assert_eq!(data_home(Some("/data"), Some("/home/u")), Some(PathBuf::from("/data")));
        assert_eq!(data_home(Some(""), Some("/home/u")), Some(PathBuf::from("/home/u/.local/share")));
        assert_eq!(data_home(Some("relative"), Some("/home/u")), Some(PathBuf::from("/home/u/.local/share")));
        assert_eq!(data_home(None, Some("/home/u")), Some(PathBuf::from("/home/u/.local/share")));
        assert_eq!(data_home(None, None), None);
        assert_eq!(data_home(None, Some("")), None);
        assert_eq!(system_data_dirs(None), vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")]);
        assert_eq!(system_data_dirs(Some("")), system_data_dirs(None));
        assert_eq!(system_data_dirs(Some("/usr/share:relative:/opt/share")), vec![PathBuf::from("/usr/share"), PathBuf::from("/opt/share")]);
        let dirs = system_data_dirs(Some("/s1:/s2"));
        let paths = entry_paths(&dirs, Path::new("/home/u/.local/share"));
        assert_eq!(paths.len(), 3);
        assert_eq!(paths.last().cloned(), Some(PathBuf::from("/home/u/.local/share/applications").join(entry_name())));
        assert!(paths.iter().all(|p| p.ends_with(entry_name())));
    }

    #[test]
    fn plan_rules() {
        use Action::{Install, Leave};
        let ours = entry_for("/now.AppImage").unwrap_or_default();
        // nothing there yet
        assert_eq!(plan(false, false, None, "/now.AppImage", false), Install);
        // a system package's entry wins over anything we would write
        assert_eq!(plan(true, false, None, "/now.AppImage", false), Leave);
        // a foreign entry in the user's directory is never touched, nor is one we cannot read
        let foreign = "Exec=other\n".to_string();
        assert_eq!(plan(false, true, Some(&foreign), "/now.AppImage", false), Leave);
        assert_eq!(plan(false, true, None, "/now.AppImage", false), Leave);
        // ours, current, icons in place: nothing is written
        assert_eq!(plan(false, true, Some(&ours), "/now.AppImage", false), Leave);
        // ours but the image moved, or an icon went missing
        assert_eq!(plan(false, true, Some(&ours), "/moved.AppImage", false), Install);
        assert_eq!(plan(false, true, Some(&ours), "/now.AppImage", true), Install);
    }

    #[test]
    fn icons_are_installed_under_the_app_id() {
        let dest = icon_destinations(Path::new("/d/icons/hicolor"));
        assert!(dest.len() >= 2);
        assert!(dest.iter().all(|(p, _)| p.starts_with("/d/icons/hicolor/")));
        for (p, bytes) in &dest {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            assert!(name == format!("{}.png", lcc_brand::APP_ID) || name == format!("{}.svg", lcc_brand::APP_ID), "{name}");
            assert!(!bytes.is_empty());
        }
        assert!(dest.iter().any(|(p, _)| p.extension().is_some_and(|e| e == "svg")));
        assert!(dest.iter().any(|(_, bytes)| bytes.starts_with(b"\x89PNG")));
    }

    #[test]
    fn install_rewrite_on_move_and_idempotence() {
        let holder = tmp("install");
        let root = &holder.0;
        let sys = vec![root.join("usr/share")];
        let data = root.join("data");
        let entry = data.join("applications").join(entry_name());
        let image = "/home/u/Apps/a image.AppImage";

        apply(image, &sys, &data).unwrap_or(());
        let first = std::fs::read_to_string(&entry).unwrap_or_default();
        assert_eq!(recorded_source(&first), Some(image));
        assert!(first.contains("Exec=\"/home/u/Apps/a image.AppImage\" %F\n"));
        let icons = icon_destinations(&data.join("icons/hicolor"));
        assert!(icons.iter().all(|(p, b)| std::fs::read(p).is_ok_and(|i| i == *b)));

        // a second start writes nothing: the files keep their timestamps
        let before: Vec<_> = std::iter::once(entry.as_path())
            .chain(icons.iter().map(|(p, _)| p.as_path()))
            .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
            .collect();
        apply(image, &sys, &data).unwrap_or(());
        let after: Vec<_> = std::iter::once(entry.as_path())
            .chain(icons.iter().map(|(p, _)| p.as_path()))
            .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
            .collect();
        assert_eq!(before, after);
        // no temporary file is left behind
        let leftovers: Vec<_> = std::fs::read_dir(entry.parent().unwrap_or(&data))
            .map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty());

        // the image was moved: the entry is rewritten, the icons are kept
        apply("/home/u/Elsewhere/re named.AppImage", &sys, &data).unwrap_or(());
        let moved = std::fs::read_to_string(&entry).unwrap_or_default();
        assert_eq!(recorded_source(&moved), Some("/home/u/Elsewhere/re named.AppImage"));
        assert!(moved.contains("Exec=\"/home/u/Elsewhere/re named.AppImage\" %F\n"));
        assert_eq!(after[1..], icons.iter().map(|(p, _)| std::fs::metadata(p).and_then(|m| m.modified()).ok()).collect::<Vec<_>>()[..]);

        // an icon removed by the user comes back, the entry is not rewritten
        let _ = std::fs::remove_file(&icons[0].0);
        apply("/home/u/Elsewhere/re named.AppImage", &sys, &data).unwrap_or(());
        assert!(icons[0].0.is_file());
        let again = std::fs::read_to_string(&entry).unwrap_or_default();
        assert_eq!(again, moved);
    }

    #[test]
    fn foreign_and_system_entries_are_left_alone() {
        let holder = tmp("foreign");
        let root = &holder.0;
        let data = root.join("data");
        let entry = data.join("applications").join(entry_name());
        std::fs::create_dir_all(entry.parent().unwrap_or(&data)).unwrap_or(());
        std::fs::write(&entry, "Name=someone else's\n").unwrap_or(());
        // a system entry ahead of ours: nothing is written, and the user's file stays as it was
        let sys = vec![root.join("usr/share")];
        std::fs::create_dir_all(root.join("usr/share/applications")).unwrap_or(());
        std::fs::write(root.join("usr/share/applications").join(entry_name()), "Name=package\n").unwrap_or(());
        apply("/i.AppImage", &sys, &data).unwrap_or(());
        assert_eq!(std::fs::read_to_string(&entry).unwrap_or_default(), "Name=someone else's\n");
        assert!(!data.join("icons").exists());
    }

    #[test]
    fn user_entry_of_someone_else_is_never_overwritten() {
        let holder = tmp("user-foreign");
        let root = &holder.0;
        let data = root.join("data");
        let entry = data.join("applications").join(entry_name());
        std::fs::create_dir_all(entry.parent().unwrap_or(&data)).unwrap_or(());
        std::fs::write(&entry, "Name=launcher tool\n").unwrap_or(());
        apply("/i.AppImage", &[], &data).unwrap_or(());
        assert_eq!(std::fs::read_to_string(&entry).unwrap_or_default(), "Name=launcher tool\n");
        // an unreadable giant file is not ours to replace either
        std::fs::write(&entry, vec![b'#'; MAX_ENTRY_BYTES as usize + 1]).unwrap_or(());
        apply("/i.AppImage", &[], &data).unwrap_or(());
        assert_eq!(std::fs::metadata(&entry).map(|m| m.len()).ok(), Some(MAX_ENTRY_BYTES + 1));
    }

    #[test]
    fn odd_image_paths_write_nothing_and_never_panic() {
        let holder = tmp("odd");
        let root = &holder.0;
        let data = root.join("data");
        for odd in ["", "relative.AppImage", "/a\nb.AppImage", "/a\u{85}.AppImage", "/x\0y", &format!("/{}", "a".repeat(MAX_PATH_CHARS + 1))] {
            let r = apply(odd, &[], &data);
            assert!(r.is_err(), "{odd:?}");
            assert!(!data.exists(), "{odd:?}");
        }
        // a path with every character that needs escaping gives an entry whose source reads back equal
        let wild = "/home/u/it's \\; & (x)/фото 100%.AppImage";
        apply(wild, &[], &data).unwrap_or(());
        let entry = data.join("applications").join(entry_name());
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        assert_eq!(recorded_source(&text), Some(wild));
        assert_eq!(group_value(&text, "Exec"), Some(exec_for(wild).as_deref().unwrap_or("")));
        assert_eq!(text.lines().count(), TEMPLATE.lines().count() + 2);
    }

    #[test]
    fn a_taken_destination_is_left_alone() {
        let holder = tmp("blocked");
        let root = &holder.0;
        let data = root.join("data");
        let entry = data.join("applications").join(entry_name());
        std::fs::create_dir_all(&entry).unwrap_or(());
        // the entry's place is a directory: not ours to replace, nothing is written
        apply("/i.AppImage", &[], &data).unwrap_or(());
        assert!(entry.is_dir());
        assert_eq!(std::fs::read_dir(&entry).map(|d| d.count()).ok(), Some(0));
        assert!(!data.join("icons").exists());
    }

    #[test]
    fn appimage_path_follows_links_and_refuses_the_rest() {
        let holder = tmp("image");
        let root = &holder.0;
        let real = root.join("real.AppImage");
        std::fs::write(&real, b"not really an AppImage").unwrap_or(());
        let link = root.join("link.AppImage");
        if std::os::unix::fs::symlink(&real, &link).is_err() {
            lcc_testkit::skip!(System, "this folder does not allow symbolic links");
            return;
        }
        // a link is launched as itself: the entry names the link, not what it points at
        assert_eq!(appimage_path(link.as_os_str()), Some(link.to_string_lossy().into_owned()));
        for bad in ["", "relative.AppImage", "/no/such/file.AppImage", root.to_string_lossy().as_ref()] {
            assert_eq!(appimage_path(std::ffi::OsStr::new(bad)), None, "{bad:?}");
        }
        // not UTF-8
        use std::os::unix::ffi::OsStrExt as _;
        assert_eq!(appimage_path(std::ffi::OsStr::from_bytes(b"/no/such/\xff.AppImage")), None);
    }

    #[test]
    fn only_a_program_inside_the_mount_is_the_appimage() {
        let holder = tmp("mount");
        let root = &holder.0;
        let mount = root.join(".mount_Xy12ab");
        let bin = mount.join("usr/bin");
        if std::fs::create_dir_all(&bin).is_err() {
            lcc_testkit::skip!(System, "this folder does not allow creating directories");
            return;
        }
        let exe = bin.join("app");
        std::fs::write(&exe, b"program").unwrap_or(());
        let link = root.join("link-to-app");
        let _ = std::os::unix::fs::symlink(&exe, &link);
        // the runtime's program, in the mount
        assert!(runs_inside_mount(Some(mount.as_os_str()), Some(&exe)));
        // a program elsewhere on this machine (a `cargo test` binary, a build in a source tree)
        assert!(!runs_inside_mount(Some(mount.as_os_str()), Some(std::path::Path::new("/usr/bin/true"))));
        assert!(!runs_inside_mount(Some(mount.as_os_str()), Some(&root.join("other"))));
        // no mount, a relative or empty one, no program
        assert!(!runs_inside_mount(None, Some(&exe)));
        assert!(!runs_inside_mount(Some(std::ffi::OsStr::new("")), Some(&exe)));
        assert!(!runs_inside_mount(Some(std::ffi::OsStr::new("relative/mount")), Some(&exe)));
        assert!(!runs_inside_mount(Some(mount.as_os_str()), Some(std::path::Path::new("bin/app"))));
        assert!(!runs_inside_mount(Some(mount.as_os_str()), None));
    }

    #[test]
    fn a_test_binary_never_registers_a_launcher() {
        // whatever this process inherited (a terminal or an IDE run from an AppImage passes `APPIMAGE`
        // on to `cargo test`), a test binary writes nothing
        assert!(lcc_brand::is_test_binary());
        assert!(!install_from_env());
    }

    #[test]
    fn two_instances_starting_together_leave_one_whole_entry() {
        let holder = tmp("parallel");
        let root = &holder.0;
        let sys = vec![root.join("usr/share")];
        let data = root.join("data");
        std::thread::scope(|scope| {
            for i in 0..8u32 {
                let (sys, data) = (sys.clone(), data.clone());
                scope.spawn(move || {
                    let image = format!("/home/u/Apps/copy {i}.AppImage");
                    apply(&image, &sys, &data).unwrap_or(());
                });
            }
        });
        let entry = data.join("applications").join(entry_name());
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        assert!(is_ours(&text));
        assert_eq!(group_value(&text, "Name"), Some(lcc_brand::DISPLAY_NAME));
        let recorded = recorded_source(&text).unwrap_or_default();
        assert!((0..8).any(|i| recorded == format!("/home/u/Apps/copy {i}.AppImage")), "{recorded}");
        for (p, bytes) in icon_destinations(&data.join("icons/hicolor")) {
            assert_eq!(std::fs::read(&p).ok().as_deref(), Some(bytes));
        }
        let leftovers =
            |d: &Path| std::fs::read_dir(d).map(|r| r.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count()).unwrap_or(0);
        assert_eq!(leftovers(entry.parent().unwrap_or(&data)), 0);
    }

    #[test]
    fn the_entry_passes_desktop_file_validate() {
        let holder = tmp("validate");
        let path = holder.0.join(entry_name());
        std::fs::write(&path, entry_for("/home/u/Apps/a.AppImage").unwrap_or_default()).unwrap_or(());
        let out = std::process::Command::new("/usr/bin/desktop-file-validate").arg(&path).output();
        let Ok(out) = out else {
            lcc_testkit::skip!(System, "desktop-file-validate is not installed");
            return;
        };
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
}
