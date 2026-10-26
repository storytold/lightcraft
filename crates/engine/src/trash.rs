//! Move files to the system trash (issue #1706: Delete from Disk).
//!
//! A file is only ever **moved** (renamed) into a trash: never copied and deleted, never removed.
//! When it can't be moved (no trash on that volume, no permission, another disk than the trash),
//! the move fails, the file stays where it was and the caller is told why.
//!
//! - **Linux / BSD:** the freedesktop.org Trash specification: the home trash
//!   (`$XDG_DATA_HOME/Trash`, by default `~/.local/share/Trash`) for files on its disk, else the
//!   disk's own `$topdir/.Trash/$uid` (an administrator-made `.Trash` with the sticky bit) or
//!   `$topdir/.Trash-$uid`. Each file gets a `.trashinfo` (original path, deletion time) so file
//!   managers can restore it.
//! - **macOS:** `~/.Trash` for files on the startup disk's volume, else the disk's
//!   `.Trashes/$uid` (made by the Finder on every disk it can trash on).
//! - **Windows:** the drive's Recycle Bin (`X:\$Recycle.Bin\<SID>`): the file becomes `$R…` and an
//!   `$I…` record (format 2, as Windows 10 and 11 write it) keeps its path, size and deletion time
//!   for Explorer's Restore. Network shares have no Recycle Bin and are refused.
//! - **Web:** not available.
//!
//! [`TrashRoot::Folder`] (also from the `LIGHTCRAFT_TRASH_DIR` environment variable) replaces the
//! system trash with one folder laid out like a freedesktop trash, on every platform: tests use it
//! and never touch the real trash.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Environment variable naming a folder to use instead of the system trash (tests, sandboxes).
pub const TRASH_DIR_ENV: &str = "LIGHTCRAFT_TRASH_DIR";

/// Where trashed files go.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TrashRoot {
    /// The platform's trash (see the module docs).
    #[default]
    System,
    /// This folder, laid out like a freedesktop trash (`files/`, `info/`). Files on another disk
    /// than the folder can't be moved there and fail.
    Folder(PathBuf),
}

impl TrashRoot {
    /// [`TrashRoot::Folder`] when [`TRASH_DIR_ENV`] names a folder, else the system trash.
    pub fn from_env() -> TrashRoot {
        match std::env::var_os(TRASH_DIR_ENV) {
            Some(d) if !d.is_empty() => TrashRoot::Folder(PathBuf::from(d)),
            _ => TrashRoot::System,
        }
    }

    /// What the trash is called where it is used (dialogs, messages).
    pub fn name(&self) -> &'static str {
        match self {
            TrashRoot::System if cfg!(windows) => "Recycle Bin",
            _ => "Trash",
        }
    }
}

/// Whether files can be trashed in this build at all. Only Linux and the BSDs have it on: the
/// Windows and macOS code is written but not yet tested on those systems, so it stays off until it is
/// (the command, the menu and the dialog all ask here).
pub fn available() -> Result<(), String> {
    if cfg!(target_arch = "wasm32") {
        Err("Delete from Disk isn't available in the web version".into())
    } else if cfg!(any(windows, target_os = "macos")) {
        Err("Delete from Disk is turned off on Windows and macOS until it has been tested there".into())
    } else {
        Ok(())
    }
}

/// Move `path` (a file or a link, not a folder) to the trash; the place it went. `now` is the
/// deletion time for the restore record, ISO 8601 (`2026-10-15T09:30:00`, longer strings are
/// cut): the session clock, which is UTC, so a file manager may show it off by the time zone
/// (the specification asks for local time, which needs the zone database).
pub fn trash(path: &Path, root: &TrashRoot, now: &str) -> Result<PathBuf, String> {
    available()?;
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() {
        return Err(format!("{}: is a folder, not a photo", path.display()));
    }
    let abs = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let now = now.get(..19).unwrap_or(now);
    match root {
        TrashRoot::Folder(dir) => freedesktop(dir, &abs, &path_bytes(&abs), now),
        TrashRoot::System => system(&abs, now),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn system(file: &Path, now: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;
    let dev = std::fs::symlink_metadata(file).map_err(|e| format!("{}: {e}", file.display()))?.dev();
    let home = home_trash().ok_or("no home folder (HOME is not set), so there is no Trash")?;
    if device_of_nearest(&home) == Some(dev) {
        return freedesktop(&home, file, &path_bytes(file), now);
    }
    // another disk: its own trash, the path recorded relative to the disk's top folder
    let top = mount_top(file, dev);
    let rel = file.strip_prefix(&top).map(path_bytes).unwrap_or_else(|_| path_bytes(file));
    let uid = uid().ok_or("can't tell which user this is (HOME is not set)")?;
    let shared = top.join(".Trash");
    if let Ok(m) = std::fs::symlink_metadata(&shared) {
        use std::os::unix::fs::PermissionsExt;
        if m.is_dir() && m.permissions().mode() & 0o1000 != 0 {
            let mine = shared.join(uid.to_string());
            if make_private_dir(&mine).is_ok()
                && let Ok(p) = freedesktop(&mine, file, &rel, now)
            {
                return Ok(p);
            }
        }
    }
    let own = top.join(format!(".Trash-{uid}"));
    make_private_dir(&own).map_err(|e| format!("the disk at {} has no Trash LightCraft may use ({e}); the file was not deleted", top.display()))?;
    freedesktop(&own, file, &rel, now)
}

#[cfg(target_os = "macos")]
fn system(file: &Path, _now: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;
    let dev = std::fs::symlink_metadata(file).map_err(|e| format!("{}: {e}", file.display()))?.dev();
    let home =
        std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from).ok_or("no home folder (HOME is not set), so there is no Trash")?;
    let home_trash = home.join(".Trash");
    let dir = if device_of_nearest(&home_trash) == Some(dev) {
        home_trash
    } else {
        let top = mount_top(file, dev);
        let trashes = top.join(".Trashes");
        if !std::fs::symlink_metadata(&trashes).is_ok_and(|m| m.is_dir()) {
            return Err(format!("the disk at {} has no Trash; the file was not deleted", top.display()));
        }
        let mine = trashes.join(uid().ok_or("can't tell which user this is")?.to_string());
        make_private_dir(&mine).map_err(|e| format!("can't use the Trash of the disk at {}: {e}; the file was not deleted", top.display()))?;
        mine
    };
    let name = file.file_name().ok_or_else(|| format!("{}: no file name", file.display()))?;
    for i in 0..10_000 {
        let to = dir.join(candidate(name, i));
        match std::fs::symlink_metadata(&to) {
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("can't reach the Trash ({e}); the file was not deleted")),
        }
        return std::fs::rename(file, &to).map(|()| to).map_err(|e| format!("{}: can't move it to the Trash: {e}", file.display()));
    }
    Err(format!("{}: no free name in the Trash", file.display()))
}

#[cfg(windows)]
fn system(file: &Path, _now: &str) -> Result<PathBuf, String> {
    recycle(file)
}

#[cfg(not(any(unix, windows)))]
fn system(_file: &Path, _now: &str) -> Result<PathBuf, String> {
    Err("there is no Trash here".into())
}

/// `$XDG_DATA_HOME/Trash` (an absolute `XDG_DATA_HOME`), else `~/.local/share/Trash`.
#[cfg(all(unix, not(target_os = "macos")))]
fn home_trash() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|d| d.is_absolute());
    let data = data.or_else(|| std::env::var_os("HOME").filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("Trash"))
}

/// The device of `p`, or of its nearest existing folder (a trash not made yet).
#[cfg(unix)]
fn device_of_nearest(p: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    p.ancestors().find_map(|a| std::fs::metadata(a).ok()).map(|m| m.dev())
}

/// The top folder of the disk `file` is on: its highest ancestor on the same device.
#[cfg(unix)]
fn mount_top(file: &Path, dev: u64) -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let mut top = file.parent().unwrap_or(file).to_path_buf();
    for a in file.ancestors().skip(1) {
        match std::fs::metadata(a) {
            Ok(m) if m.dev() == dev => top = a.to_path_buf(),
            _ => break,
        }
    }
    top
}

/// This user's id: the owner of the home folder.
#[cfg(unix)]
fn uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    std::fs::metadata(home).ok().map(|m| m.uid())
}

/// Make `dir` (and its parents) readable by its owner only; fine when it is there already as a
/// real folder (not a link).
fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)?;
    if std::fs::symlink_metadata(dir)?.is_dir() { Ok(()) } else { Err(std::io::Error::other("not a folder")) }
}

/// Move `file` into the freedesktop trash `trash` (`files/` + `info/`), recording `original` (the
/// path as the restore record states it, bytes) and `now`. The `.trashinfo` is made first under a
/// name nobody holds (created exclusively), then the file is renamed beside it; when the rename
/// fails the record is removed again and the file stays.
fn freedesktop(trash: &Path, file: &Path, original: &[u8], now: &str) -> Result<PathBuf, String> {
    let (files, info) = (trash.join("files"), trash.join("info"));
    for d in [&files, &info] {
        make_private_dir(d).map_err(|e| format!("can't use the Trash at {}: {e}; the file was not deleted", trash.display()))?;
    }
    let name = file.file_name().ok_or_else(|| format!("{}: no file name", file.display()))?;
    let record = format!("[Trash Info]\nPath={}\nDeletionDate={now}\n", percent_encode(original));
    for i in 0..10_000 {
        let n = candidate(name, i);
        let mut info_name = n.clone();
        info_name.push(".trashinfo");
        let info_path = info.join(&info_name);
        let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(&info_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("can't write to the Trash at {}: {e}; the file was not deleted", trash.display())),
        };
        let to = files.join(&n);
        if std::fs::symlink_metadata(&to).is_ok() {
            drop(f);
            let _ = std::fs::remove_file(&info_path);
            continue;
        }
        let written = f.write_all(record.as_bytes()).and_then(|()| f.sync_all());
        drop(f);
        let moved = written.and_then(|()| std::fs::rename(file, &to));
        return match moved {
            Ok(()) => Ok(to),
            Err(e) => {
                let _ = std::fs::remove_file(&info_path);
                let why = if is_cross_device(&e) { "it is on another disk than the Trash".to_string() } else { e.to_string() };
                Err(format!("{}: can't move it to the Trash ({why}); the file was not deleted", file.display()))
            }
        };
    }
    Err(format!("{}: no free name in the Trash", file.display()))
}

fn is_cross_device(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::CrossesDevices
}

/// The `i`th name tried for `name` in a trash: the name, then `stem (2).ext`, `stem (3).ext`…
fn candidate(name: &OsStr, i: u32) -> OsString {
    if i == 0 {
        return name.to_os_string();
    }
    let s = name.to_string_lossy();
    match s.rfind('.').filter(|&d| d > 0) {
        Some(d) => format!("{} ({}){}", s.get(..d).unwrap_or_default(), i + 1, s.get(d..).unwrap_or_default()).into(),
        None => format!("{s} ({})", i + 1).into(),
    }
}

/// A path's bytes as stored (Unix) or as UTF-8.
fn path_bytes(p: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        p.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        p.to_string_lossy().into_owned().into_bytes()
    }
}

/// URL-escape a path for a `.trashinfo` (RFC 2396; `/` kept).
fn percent_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------------- Windows

/// The Recycle Bin record (`$I…` file, format 2) for a file of `size` bytes deleted at
/// `filetime` (100 ns ticks since 1601) from `original`.
#[cfg_attr(not(windows), allow(dead_code))]
fn recycle_record(original: &str, size: u64, filetime: u64) -> Vec<u8> {
    let name: Vec<u16> = original.encode_utf16().chain(std::iter::once(0)).collect();
    let mut v = Vec::with_capacity(28 + name.len() * 2);
    v.extend_from_slice(&2u64.to_le_bytes());
    v.extend_from_slice(&size.to_le_bytes());
    v.extend_from_slice(&filetime.to_le_bytes());
    v.extend_from_slice(&u32::try_from(name.len()).unwrap_or(u32::MAX).to_le_bytes());
    for c in name {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v
}

/// The drive letter of an absolute local path (`C:\…`, `\\?\C:\…`); `None` for shares.
#[cfg_attr(not(windows), allow(dead_code))]
fn drive_of(p: &Path) -> Option<char> {
    let s = p.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let mut c = s.chars();
    match (c.next(), c.next(), c.next()) {
        (Some(d), Some(':'), Some('\\' | '/')) if d.is_ascii_alphabetic() => Some(d.to_ascii_uppercase()),
        _ => None,
    }
}

/// The SID in `whoami /user /fo csv /nh` output (`"pc\me","S-1-5-21-…"`).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_sid(out: &str) -> Option<String> {
    let sid = out.trim().rsplit(',').next()?.trim().trim_matches('"');
    (sid.starts_with("S-1-") && sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then(|| sid.to_string())
}

#[cfg(windows)]
fn recycle(file: &Path) -> Result<PathBuf, String> {
    use std::sync::OnceLock;
    static SID: OnceLock<Option<String>> = OnceLock::new();
    let drive = drive_of(file).ok_or_else(|| format!("{}: network shares have no Recycle Bin; the file was not deleted", file.display()))?;
    let sid = SID
        .get_or_init(|| {
            let out = std::process::Command::new("whoami").args(["/user", "/fo", "csv", "/nh"]).output().ok()?;
            parse_sid(&String::from_utf8_lossy(&out.stdout))
        })
        .clone()
        .ok_or("can't tell which user this is (whoami /user failed); the file was not deleted")?;
    let bin = PathBuf::from(format!("{drive}:\\$Recycle.Bin\\{sid}"));
    if !bin.is_dir() {
        return Err(format!(
            "drive {drive}: has no Recycle Bin for this user yet (delete any file on it with Explorer once); the file was not deleted"
        ));
    }
    let size = std::fs::symlink_metadata(file).map(|m| m.len()).map_err(|e| format!("{}: {e}", file.display()))?;
    let ext = file.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let ticks = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs().saturating_add(11_644_473_600)).saturating_mul(10_000_000).saturating_add(u64::from(d.subsec_nanos() / 100)))
        .unwrap_or(0);
    let record = recycle_record(&file.to_string_lossy(), size, ticks);
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut seed = ticks ^ size.rotate_left(17);
    for _ in 0..10_000 {
        let mut id = String::new();
        for _ in 0..6 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let k = usize::try_from((seed >> 33) % 36).unwrap_or(0);
            id.push(char::from(*ALPHABET.get(k).unwrap_or(&b'A')));
        }
        let (info, to) = (bin.join(format!("$I{id}{ext}")), bin.join(format!("$R{id}{ext}")));
        if std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(&info) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("can't write to the Recycle Bin on {drive}: ({e}); the file was not deleted")),
        };
        let written = f.write_all(&record).and_then(|()| f.sync_all());
        drop(f);
        return match written.and_then(|()| std::fs::rename(file, &to)) {
            Ok(()) => Ok(to),
            Err(e) => {
                let _ = std::fs::remove_file(&info);
                Err(format!("{}: can't move it to the Recycle Bin ({e}); the file was not deleted", file.display()))
            }
        };
    }
    Err(format!("{}: no free name in the Recycle Bin", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-trash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    // needs a working trash: Delete from Disk is off on Windows and macOS (`available`)
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_file_moves_into_the_folder_trash_with_its_restore_record() {
        let d = dir("move");
        let (photos, bin) = (d.join("my photos"), d.join("bin"));
        std::fs::create_dir_all(&photos).unwrap();
        let f = photos.join("IMG 1.jpg");
        std::fs::write(&f, b"jpeg").unwrap();
        let root = TrashRoot::Folder(bin.clone());
        let to = trash(&f, &root, "2026-10-15T09:30:00+02:00").unwrap();
        assert!(!f.exists(), "the file left its folder");
        assert_eq!(to, bin.join("files/IMG 1.jpg"));
        assert_eq!(std::fs::read(&to).unwrap(), b"jpeg");
        let info = std::fs::read_to_string(bin.join("info/IMG 1.jpg.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=/"), "{info}");
        assert!(info.contains("/my%20photos/IMG%201.jpg\n"), "{info}");
        assert!(info.contains("DeletionDate=2026-10-15T09:30:00\n"), "{info}");

        // the same name again: a second entry, the first kept
        std::fs::write(&f, b"again").unwrap();
        let to2 = trash(&f, &root, "2026-10-15T09:31:00").unwrap();
        assert_eq!(to2, bin.join("files/IMG 1 (2).jpg"));
        assert_eq!(std::fs::read(&to).unwrap(), b"jpeg");
        assert!(bin.join("info/IMG 1 (2).jpg.trashinfo").is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    // needs a working trash: Delete from Disk is off on Windows and macOS (`available`)
    #[cfg(all(unix, not(target_os = "macos")))]
    fn an_unusable_trash_leaves_the_file_where_it_was() {
        let d = dir("fail");
        let f = d.join("keep.jpg");
        std::fs::write(&f, b"jpeg").unwrap();
        // the "trash" is a file: nothing can be made in it
        let blocker = d.join("bin");
        std::fs::write(&blocker, b"").unwrap();
        let e = trash(&f, &TrashRoot::Folder(blocker), "2026-10-15T09:30:00").unwrap_err();
        assert!(e.contains("not deleted"), "{e}");
        assert_eq!(std::fs::read(&f).unwrap(), b"jpeg");
        // a missing file and a folder are refused
        assert!(trash(&d.join("nope.jpg"), &TrashRoot::Folder(d.join("t")), "").is_err());
        assert!(trash(&d, &TrashRoot::Folder(d.join("t2")), "").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn names_and_paths_are_escaped() {
        assert_eq!(candidate(OsStr::new("a.CR3"), 0), OsString::from("a.CR3"));
        assert_eq!(candidate(OsStr::new("a.CR3"), 1), OsString::from("a (2).CR3"));
        assert_eq!(candidate(OsStr::new(".hidden"), 2), OsString::from(".hidden (3)"));
        assert_eq!(candidate(OsStr::new("noext"), 1), OsString::from("noext (2)"));
        assert_eq!(percent_encode("/a b/é%.jpg".as_bytes()), "/a%20b/%C3%A9%25.jpg");
    }

    #[test]
    fn the_gate_is_on_only_where_the_trash_is_tested() {
        assert_eq!(available().is_ok(), cfg!(all(unix, not(target_os = "macos"))));
    }

    #[test]
    fn windows_records_and_drives() {
        let r = recycle_record("C:\\a.jpg", 10, 7);
        assert_eq!(r.len(), 28 + 9 * 2);
        assert_eq!(r.get(..8).unwrap(), &2u64.to_le_bytes());
        assert_eq!(r.get(8..16).unwrap(), &10u64.to_le_bytes());
        assert_eq!(r.get(24..28).unwrap(), &9u32.to_le_bytes());
        assert_eq!(r.get(28..30).unwrap(), &[b'C', 0]);
        assert_eq!(drive_of(Path::new(r"c:\Photos\a.jpg")), Some('C'));
        assert_eq!(drive_of(Path::new(r"\\?\D:\a.jpg")), Some('D'));
        assert_eq!(drive_of(Path::new(r"\\server\share\a.jpg")), None);
        assert_eq!(parse_sid("\"pc\\me\",\"S-1-5-21-1-2-3-1001\"\r\n").as_deref(), Some("S-1-5-21-1-2-3-1001"));
        assert_eq!(parse_sid("nonsense"), None);
    }
}
