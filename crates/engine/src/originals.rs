//! Never write over an original: exports, renders and other files LightCraft writes for the user
//! must not land on a catalogued photo's own file (or its XMP sidecar), whatever the conflict
//! setting or exact path asked for. [`OriginalGuard`] is a snapshot of the library's originals
//! that answers "would writing here replace one?" by file identity (the same file under another
//! spelling, through `..`, a link or a case-insensitive volume counts), not by comparing strings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(not(any(unix, windows)))]
use std::time::SystemTime;

use lightcraft_catalog::safe_file::{same_file, same_file_strict};
use lightcraft_catalog::{Catalog, Source};

/// The directory entry a screenshot is about to write, pinned at request admission.
///
/// A later write must not replace a path that was absent when the request was admitted (for
/// example a photo imported while the capture was waiting), nor a destination that was replaced by
/// another file in the meantime. [`Session::revalidate_write_target`] checks this immediately
/// before the write, and an absent destination is then published with create-new semantics.
#[derive(Clone, Debug)]
pub struct WriteTarget {
    path: String,
    state: WriteTargetState,
    /// The canonical parent folder at admission, when it existed: a retargeted parent symlink is a
    /// different destination even when the file name is unchanged.
    parent: Option<PathBuf>,
}

#[derive(Clone, Debug)]
enum WriteTargetState {
    /// The directory entry did not exist at admission.
    Absent,
    /// The directory entry existed; this is its identity (not its contents).
    Existing(FileIdentity),
}

/// Stable identity of a directory entry, from `symlink_metadata` (so replacing a symlink or a
/// hard link is a different entry). Unix uses device + inode; Windows uses the canonical path,
/// length and creation time; other platforms use a canonical/len/mtime fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    canonical: PathBuf,
    #[cfg(windows)]
    len: u64,
    #[cfg(windows)]
    created: u64,
    #[cfg(not(any(unix, windows)))]
    canonical: PathBuf,
    #[cfg(not(any(unix, windows)))]
    len: u64,
    #[cfg(not(any(unix, windows)))]
    modified: Option<SystemTime>,
}

impl FileIdentity {
    fn of(path: &Path, meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let _ = path;
            Self { dev: meta.dev(), ino: meta.ino() }
        }
        #[cfg(windows)]
        {
            // The volume serial/file index metadata is still unstable; use stable metadata that
            // changes when a different file replaces the entry, and keep the same in-place
            // content edits (which do not change the creation time or length) overwritable.
            use std::os::windows::fs::MetadataExt;
            Self { canonical: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()), len: meta.len(), created: meta.creation_time() }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Self { canonical: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()), len: meta.len(), modified: meta.modified().ok() }
        }
    }
}

impl WriteTarget {
    /// The destination path as the request named it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Was the directory entry absent at admission? Such a destination is published with
    /// create-new semantics, never with the overwriting writer.
    pub fn was_absent(&self) -> bool {
        matches!(&self.state, WriteTargetState::Absent)
    }
}

/// The library's originals and their sidecars, looked up by file name.
#[derive(Debug, Default)]
pub struct OriginalGuard {
    /// lower-case file name → (path on disk, what it is: "the original of IMG_1.CR3", …)
    by_name: HashMap<String, Vec<(PathBuf, String)>>,
    /// Every indexed entry, built on the first check whose target name did not match: a hard
    /// link or symlink may name an original differently from the catalogued path.
    others: std::sync::OnceLock<Vec<(PathBuf, String)>>,
}

impl OriginalGuard {
    /// The originals of every photo in `catalog`.
    pub fn new(catalog: &Catalog) -> Self {
        let mut g = Self::default();
        for p in catalog.photos() {
            if let Source::File { path } = &p.source {
                g.add(Path::new(path), &p.file_name);
            }
        }
        g
    }

    /// Protect `path` (an original named `label`) and its sidecars (`IMG_1.xmp`, `IMG_1.CR3.xmp`).
    pub fn add(&mut self, path: &Path, label: &str) {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else { return };
        self.others = std::sync::OnceLock::new();
        let mut put = |file: String, p: PathBuf, what: String| self.by_name.entry(file.to_lowercase()).or_default().push((p, what));
        put(name.clone(), path.to_path_buf(), format!("the original of {label}"));
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        put(format!("{stem}.xmp"), path.with_extension("xmp"), format!("the XMP sidecar of {label}"));
        put(format!("{name}.xmp"), PathBuf::from(format!("{}.xmp", path.to_string_lossy())), format!("the XMP sidecar of {label}"));
    }

    /// `Err` (a message for the user) when writing `target` would replace a protected file, or
    /// create a protected sidecar that isn't there yet.
    pub fn check(&self, target: &Path) -> Result<(), String> {
        self.check_target(target, false)
    }

    /// [`Self::check`] plus hard links: a screenshot target must not replace an original reached
    /// through a link with a different name. The per-entry identity scan is limited to this
    /// long-lived, user-visible destination check, not every ordinary export overwrite.
    pub fn check_identity(&self, target: &Path) -> Result<(), String> {
        self.check_target(target, true)
    }

    fn check_target(&self, target: &Path, hard_links: bool) -> Result<(), String> {
        if std::fs::symlink_metadata(target).is_err() {
            return self.check_new(target); // nothing there to replace
        }
        let mut names: Vec<String> = target.file_name().map(|n| n.to_string_lossy().to_lowercase()).into_iter().collect();
        if let Some(n) = std::fs::canonicalize(target).ok().and_then(|c| c.file_name().map(|n| n.to_string_lossy().to_lowercase())) {
            names.push(n);
        }
        // The identity check refuses a target whose file identity cannot be read, so an unreadable
        // hard link to an original is not taken for an ordinary file.
        let refuse_if_same = |p: &Path, what: &str| -> Result<(), String> {
            let same = if hard_links {
                same_file_strict(p, target).map_err(|e| {
                    format!(
                        "{}: LightCraft cannot tell whether this is {what} in the library ({e}) and never writes over an original (choose another folder or file name)",
                        target.display()
                    )
                })?
            } else {
                same_file(p, target)
            };
            if same {
                return Err(format!(
                    "{} is {what} in the library: LightCraft never writes over an original (choose another folder or file name)",
                    target.display()
                ));
            }
            Ok(())
        };
        for n in &names {
            for (p, what) in self.by_name.get(n).into_iter().flatten() {
                refuse_if_same(p, what)?;
            }
        }
        // An imported hard link or symlink can have a different name from its original. A write
        // to that entry would change what the library reads. Only do this fallback for existing
        // targets that the name index did not protect; the list is built once per guard, not once
        // per target. Ordinary exports keep the cheaper symlink-only scan.
        let others = self.others.get_or_init(|| self.by_name.values().flatten().cloned().collect());
        for (p, what) in others {
            if !hard_links && std::fs::read_link(p).is_err() {
                continue;
            }
            refuse_if_same(p, what)?;
        }
        Ok(())
    }

    /// A new file with a sidecar's name in that sidecar's folder would be read back as the
    /// photo's edits. The folders are compared by identity, so `sub/../` or a link counts.
    fn check_new(&self, target: &Path) -> Result<(), String> {
        if !is_xmp(target) {
            return Ok(());
        }
        let Some(name) = target.file_name().map(|n| n.to_string_lossy().to_lowercase()) else { return Ok(()) };
        for (p, what) in self.by_name.get(&name).into_iter().flatten() {
            if same_file(folder_of(p), folder_of(target)) {
                return Err(format!("{} would become {what} in the library (choose another folder or file name)", target.display()));
            }
        }
        Ok(())
    }
}

fn is_xmp(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}

/// The folder `path` is in (`.` for a bare file name).
fn folder_of(path: &Path) -> &Path {
    path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

impl crate::Session {
    /// A guard over this library's originals (see [`OriginalGuard`]).
    pub fn original_guard(&self) -> OriginalGuard {
        OriginalGuard::new(&self.catalog)
    }

    /// `Err` when writing `path` would replace a catalogued photo's original or sidecar, or
    /// create one of its sidecars. A free path that isn't an `.xmp` (the usual case) is answered
    /// without building the guard over the whole library.
    pub fn check_write_target(&self, path: &str) -> Result<(), String> {
        let path = Path::new(path);
        if std::fs::symlink_metadata(path).is_err() && !is_xmp(path) {
            return Ok(()); // nothing there to replace, and no sidecar to create
        }
        self.original_guard().check(path)
    }

    /// Pin the destination of a long-running write (a screenshot) now.
    ///
    /// The returned [`WriteTarget`] records whether the directory entry existed and, if so, its
    /// identity, plus the canonical parent folder. Pass it to [`Self::revalidate_write_target`]
    /// immediately before writing.
    pub fn pin_write_target(&self, path: &str) -> Result<WriteTarget, String> {
        let p = Path::new(path);
        let parent = p.parent().filter(|d| !d.as_os_str().is_empty()).and_then(|d| std::fs::canonicalize(d).ok());
        match std::fs::symlink_metadata(p) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // nothing there yet, but a new file with a photo's sidecar name would be read as its edits
                if is_xmp(p) {
                    self.original_guard().check(p)?;
                }
                Ok(WriteTarget { path: path.to_string(), state: WriteTargetState::Absent, parent })
            }
            Err(error) => Err(format!("{path}: {error}")),
            Ok(meta) => {
                // Refuse a catalogued original or sidecar (including a differently named hard
                // link) before the request is queued.
                self.original_guard().check_identity(p)?;
                Ok(WriteTarget { path: path.to_string(), state: WriteTargetState::Existing(FileIdentity::of(p, &meta)), parent })
            }
        }
    }

    /// Re-check a pinned target immediately before writing.
    ///
    /// An absent target must still be absent; an existing target must still name the same
    /// directory entry and must still pass the current catalog's original guard. A parent folder
    /// whose canonical path changed (a retargeted symlink) is refused too.
    pub fn revalidate_write_target(&self, target: &WriteTarget) -> Result<(), String> {
        if let Some(parent) = &target.parent {
            // Resolve the request's parent again: canonicalizing the saved resolved folder
            // would miss a directory symlink in the original path being retargeted.
            let now = Path::new(&target.path).parent().and_then(|path| std::fs::canonicalize(path).ok());
            match now {
                Some(now) if now == *parent => {}
                _ => {
                    return Err(format!(
                        "{}: the destination folder changed while the screenshot was being captured; refusing to write",
                        target.path
                    ));
                }
            }
        }
        match std::fs::symlink_metadata(&target.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match &target.state {
                WriteTargetState::Absent => Ok(()),
                WriteTargetState::Existing(_) => {
                    Err(format!("{}: the file was removed while the screenshot was being captured; refusing to create it again", target.path))
                }
            },
            Err(error) => Err(format!("{}: {error}", target.path)),
            Ok(meta) => match &target.state {
                WriteTargetState::Absent => Err(format!("{} appeared while the screenshot was being captured; refusing to replace it", target.path)),
                WriteTargetState::Existing(identity) => {
                    if FileIdentity::of(Path::new(&target.path), &meta) != *identity {
                        return Err(format!("{} changed while the screenshot was being captured; refusing to replace the new file", target.path));
                    }
                    // The catalog may have gained this file as an original since admission.
                    self.original_guard().check_identity(Path::new(&target.path))
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::{Op, Photo, PhotoId};

    #[test]
    fn originals_and_their_sidecars_are_protected_by_identity() {
        let d = std::env::temp_dir().join(format!("lc-originals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let orig = d.join("IMG_1.jpg");
        std::fs::write(&orig, b"original").unwrap();
        std::fs::write(d.join("IMG_1.xmp"), b"<x/>").unwrap();
        std::fs::write(d.join("other.jpg"), b"other").unwrap();
        let mut c = Catalog::default();
        let p = Photo::new(PhotoId(1), Source::File { path: orig.to_string_lossy().to_string() }, "IMG_1.jpg", "JPEG", 10, 10, "2026-01-01");
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        let g = OriginalGuard::new(&c);
        assert!(g.check(&orig).unwrap_err().contains("original of IMG_1.jpg"));
        assert!(g.check(&d.join("sub/../IMG_1.jpg")).is_err(), "another spelling of the same file");
        assert!(g.check(&d.join("IMG_1.xmp")).unwrap_err().contains("sidecar"));
        assert!(g.check(&d.join("other.jpg")).is_ok(), "an ordinary existing file");
        assert!(g.check(&d.join("new.jpg")).is_ok(), "a free name");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_sidecar_that_isnt_there_yet_is_protected_too() {
        let d = std::env::temp_dir().join(format!("lc-originals-new-sidecar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let orig = d.join("IMG_1.jpg");
        std::fs::write(&orig, b"original").unwrap();
        let mut c = Catalog::default();
        let p = Photo::new(PhotoId(1), Source::File { path: orig.to_string_lossy().to_string() }, "IMG_1.jpg", "JPEG", 10, 10, "2026-01-01");
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        let g = OriginalGuard::new(&c);
        assert!(g.check(&d.join("IMG_1.xmp")).unwrap_err().contains("XMP sidecar of IMG_1.jpg"));
        assert!(g.check(&d.join("IMG_1.jpg.xmp")).is_err(), "the appended spelling");
        assert!(g.check(&d.join("sub/../IMG_1.xmp")).is_err(), "another spelling of the folder");
        assert!(g.check(&d.join("sub/IMG_1.xmp")).is_ok(), "the same name in another folder");
        assert!(g.check(&d.join("other.xmp")).is_ok(), "no photo of that name");
        assert!(g.check(&d.join("missing/IMG_1.xmp")).is_ok(), "a folder that isn't there");
        // a write pinned ahead of time (a queued screenshot) is refused the same way
        let mut s = crate::Session::new();
        s.catalog = c;
        let pinned = s.pin_write_target(&d.join("IMG_1.xmp").to_string_lossy());
        assert!(pinned.unwrap_err().contains("XMP sidecar of IMG_1.jpg"));
        assert!(s.pin_write_target(&d.join("sub/IMG_1.xmp").to_string_lossy()).is_ok(), "the same name in another folder");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_targets_pin_absent_paths_and_recheck_the_current_library() {
        let d = std::env::temp_dir().join(format!("lc-write-targets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut s = crate::Session::new();
        let original = d.join("IMG_1.jpg");
        std::fs::write(&original, b"original").unwrap();
        let photo = Photo::new(PhotoId(1), Source::File { path: original.to_string_lossy().to_string() }, "IMG_1.jpg", "JPEG", 10, 10, "2026-01-01");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();

        // A path that was absent at admission is refused when anything appears there.
        let free = d.join("new-shot.png");
        let absent = s.pin_write_target(&free.to_string_lossy()).expect("free path");
        assert!(absent.was_absent());
        std::fs::write(&free, b"appeared").unwrap();
        assert!(s.revalidate_write_target(&absent).unwrap_err().contains("appeared"));
        std::fs::remove_file(&free).unwrap();

        // An ordinary existing path can still be overwritten in place (same entry).
        let allowed = d.join("old-shot.png");
        std::fs::write(&allowed, b"old").unwrap();
        let existing = s.pin_write_target(&allowed.to_string_lossy()).expect("ordinary file");
        assert!(!existing.was_absent());
        std::fs::write(&allowed, b"new").unwrap();
        assert!(s.revalidate_write_target(&existing).is_ok(), "an allowed screenshot refresh keeps working");

        // A hard link to an original is refused at admission, and an absent path that becomes
        // such a link is refused at write time.
        let linked = d.join("linked.png");
        std::fs::hard_link(&original, &linked).unwrap();
        assert!(s.pin_write_target(&linked.to_string_lossy()).unwrap_err().contains("original"));
        std::fs::remove_file(&linked).unwrap();
        let appears_as_link = d.join("late-link.png");
        let late = s.pin_write_target(&appears_as_link.to_string_lossy()).expect("free path");
        std::fs::hard_link(&original, &appears_as_link).unwrap();
        assert!(s.revalidate_write_target(&late).unwrap_err().contains("appeared"));

        // A path pinned as an ordinary file becomes an original through a catalog import while
        // the capture is queued: the current guard refuses the write.
        let imported = d.join("imported.png");
        std::fs::write(&imported, b"not yet imported").unwrap();
        let queued = s.pin_write_target(&imported.to_string_lossy()).expect("ordinary file");
        let p = Photo::new(PhotoId(2), Source::File { path: imported.to_string_lossy().to_string() }, "imported.png", "PNG", 10, 10, "2026-01-01");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        assert!(s.revalidate_write_target(&queued).unwrap_err().contains("original"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = d.join("link-shot.png");
            symlink(&original, &link).unwrap();
            assert!(s.pin_write_target(&link.to_string_lossy()).unwrap_err().contains("original"));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn a_hard_link_whose_identity_cannot_be_read_is_refused() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = std::env::temp_dir().join(format!("lc-unreadable-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut s = crate::Session::new();
        let original = d.join("IMG_1.jpg");
        std::fs::write(&original, b"original").unwrap();
        let photo = Photo::new(PhotoId(1), Source::File { path: original.to_string_lossy().to_string() }, "IMG_1.jpg", "JPEG", 10, 10, "2026-01-01");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        let linked = d.join("shot.png");
        std::fs::hard_link(&original, &linked).unwrap();

        // A handle that shares nothing makes every other open of the file fail, so the file
        // identity of either name cannot be read while it is held.
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&original).unwrap();
        let refused = s.pin_write_target(&linked.to_string_lossy()).unwrap_err();
        // Except with SeBackupPrivilege enabled (an admin's Git Bash, so every Windows CI gate leg):
        // a backup-semantics open of both names, as the guard's identity read uses, gets past the
        // held handle. The guard then sees the link and refuses it as the original instead (#3228).
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let backup_open = |p: &std::path::Path| std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(p).is_ok();
        if backup_open(&original) && backup_open(&linked) {
            assert!(refused.contains("is the original"), "{refused}");
            assert!(!refused.contains("cannot tell whether"), "{refused}");
            lightcraft_testkit::skip!(
                System,
                "a backup-semantics open gets past share_mode(0) here (SeBackupPrivilege is enabled), so the \"cannot tell whether\" refusal can't be tested"
            );
        } else {
            assert!(refused.contains("cannot tell whether"), "{refused}");
        }
        drop(held);

        let refused = s.pin_write_target(&linked.to_string_lossy()).unwrap_err();
        assert!(refused.contains("original"), "{refused}");
        assert!(!refused.contains("cannot tell whether"), "{refused}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
