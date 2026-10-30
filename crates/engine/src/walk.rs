//! Bounded folder walks (issue #374): every recursive listing of the disk — import, Local's
//! Include Subfolders, Find Missing Photos, preset and LUT folders, MCP — goes through
//! [`files_in`], which stops at a depth and a number of directory entries instead of walking a
//! whole drive, follows a symbolic link or junction to a folder at most once (no loops), and
//! decides what is a folder from the directory listing itself (no extra open per entry, which
//! is what makes a large walk slow on Windows).
//!
//! It never chooses where to walk: callers pass a folder the user named. Nothing in the app walks
//! at startup or on the UI thread.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// How far a walk may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Folder levels below the starting folder that are listed (its own entries are level 1).
    pub max_depth: usize,
    /// Directory entries looked at in all (files and folders, hidden ones included).
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_depth: MAX_DEPTH, max_entries: MAX_ENTRIES }
    }
}

/// Folder levels a walk descends at most; photo trees are a handful of levels deep.
pub const MAX_DEPTH: usize = 32;
/// Directory entries a walk looks at at most: far more than any photo folder holds, far fewer
/// than a whole system drive.
pub const MAX_ENTRIES: usize = 1_000_000;

/// What a walk found.
#[derive(Debug, Default, PartialEq)]
pub struct Walked {
    /// The files `keep` accepted, in path order, folder by folder (depth first).
    pub files: Vec<PathBuf>,
    /// The walk stopped at a [`Limits`] bound: there may be more files.
    pub truncated: bool,
    /// Directory entries looked at.
    pub entries: usize,
}

/// Whether the entry has a dot-prefixed name or a Windows or macOS hidden attribute.
pub fn is_hidden_entry(entry: &std::fs::DirEntry) -> bool {
    hidden_with_metadata(&entry.path(), || entry.metadata())
}

/// Whether the path has a dot-prefixed name or a Windows or macOS hidden attribute.
pub fn is_hidden_path(path: &Path) -> bool {
    hidden_with_metadata(path, || std::fs::symlink_metadata(path))
}

fn hidden_with_metadata(path: &Path, metadata: impl FnOnce() -> std::io::Result<std::fs::Metadata>) -> bool {
    if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        metadata().is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::darwin::fs::MetadataExt;
        const UF_HIDDEN: u32 = 0x0000_8000;
        metadata().is_ok_and(|m| m.st_flags() & UF_HIDDEN != 0)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = metadata;
        false
    }
}

/// What a folder's listing says an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Folder,
    /// A link, or an entry whose kind the listing did not say.
    Other,
}

/// What [`files_in_noting`] hands over for each folder it lists: the folder, the name and kind
/// of every entry in it (hidden ones too), and whether that is all of them (not when the walk
/// stopped inside the folder, or an entry could not be read), and the folder's modification time when the walk read
/// it and it was old enough to show a later change ([`files_in_noting_timed`]; else `None`). It answers whether to go
/// on: `false` ends the walk there (a scan cancelled; [`Walked::truncated`] is left alone: who stopped it knows).
pub type NoteFolder<'a> = &'a mut dyn FnMut(&Path, Vec<(std::ffi::OsString, EntryKind)>, bool, Option<std::time::SystemTime>) -> bool;

/// How old a folder's modification time must be, when the folder is read, for a change after the
/// listing to show in it. File systems keep folder times in whole seconds (HFS+, ext3) or two
/// (FAT, and SMB1's DOS times): a file added in the same tick as the listing would leave the
/// time as it was. Two seconds is the coarsest of them (git's "racily clean" rule, for the same reason).
const TIME_GRAIN: std::time::Duration = std::time::Duration::from_secs(2);

/// `dir`'s modification time, when it is old enough ([`TIME_GRAIN`]) that a change after now
/// would move it: read before the folder is listed, so nothing added during the listing hides.
fn settled_time(dir: &Path) -> Option<std::time::SystemTime> {
    let modified = std::fs::metadata(dir).and_then(|m| m.modified()).ok()?;
    let now = std::time::SystemTime::now();
    modified.checked_add(TIME_GRAIN).filter(|t| *t <= now).map(|_| modified)
}

/// The files below folder `root` that `keep` accepts. Hidden entries (names starting with `.`)
/// are left out, the folder `skip` (e.g. the library's own) is never entered, and the walk stops
/// at `limits` (see [`Walked::truncated`]). Unreadable folders are skipped.
pub fn files_in(root: &Path, skip: Option<&Path>, limits: Limits, keep: impl FnMut(&Path) -> bool) -> Walked {
    walk(root, skip, limits, keep, None)
}

/// [`files_in`], handing the listing of every folder it reads to `note`: who walks a folder to
/// find its files has, with that, the answer to "is this name there" for everything in it, and
/// need not ask the disk again (a round trip for each name on a network share).
pub fn files_in_noting(root: &Path, skip: Option<&Path>, limits: Limits, keep: impl FnMut(&Path) -> bool, note: NoteFolder) -> Walked {
    walk(root, skip, limits, keep, Some((note, false)))
}

/// [`files_in_noting`], also reading each folder's modification time (before it is listed; a round
/// trip on a network share) and handing it over when it is old enough to show a later change.
pub fn files_in_noting_timed(root: &Path, skip: Option<&Path>, limits: Limits, keep: impl FnMut(&Path) -> bool, note: NoteFolder) -> Walked {
    walk(root, skip, limits, keep, Some((note, true)))
}

fn walk(root: &Path, skip: Option<&Path>, limits: Limits, mut keep: impl FnMut(&Path) -> bool, mut note: Option<(NoteFolder, bool)>) -> Walked {
    let mut out = Walked::default();
    // canonical folders reached through a link, so a link loop is walked at most once
    let mut linked: HashSet<PathBuf> = HashSet::new();
    if let Ok(c) = std::fs::canonicalize(root) {
        linked.insert(c);
    }
    // Include aliases to the library in the exclusion, not just its lexical path.
    let skip_canonical = skip.and_then(|p| std::fs::canonicalize(p).ok());
    // (path, depth of the folder it is in, is a folder); popped in path order
    let mut stack: Vec<(PathBuf, usize, bool)> = vec![(root.to_path_buf(), 0, true)];
    while let Some((path, depth, is_dir)) = stack.pop() {
        if !is_dir {
            if keep(&path) {
                out.files.push(path);
            }
            continue;
        }
        if skip.is_some_and(|s| path == s) || skip_canonical.as_ref().is_some_and(|s| std::fs::canonicalize(&path).is_ok_and(|p| p.starts_with(s))) {
            continue;
        }
        if depth >= limits.max_depth {
            out.truncated = true;
            continue;
        }
        let modified = if note.as_ref().is_some_and(|n| n.1) { settled_time(&path) } else { None };
        let Ok(rd) = std::fs::read_dir(&path) else { continue };
        let mut children: Vec<(PathBuf, bool)> = Vec::new();
        // (the folder's listing, for who asked for it; all of it unless something below says otherwise)
        let mut listed = note.is_some().then(Vec::new);
        let mut complete = true;
        for e in rd {
            let Ok(e) = e else {
                complete = false;
                continue;
            };
            if out.entries >= limits.max_entries {
                out.truncated = true;
                complete = false;
                break;
            }
            out.entries = out.entries.saturating_add(1);
            let p = e.path();
            // (a hidden entry's kind is only asked for when the listing is wanted; every other
            // entry's is asked once)
            let is_hidden = is_hidden_entry(&e);
            if is_hidden && listed.is_none() {
                continue;
            }
            let ft = e.file_type();
            if let Some(listed) = listed.as_mut() {
                let kind = match &ft {
                    Ok(t) if t.is_symlink() => EntryKind::Other,
                    Ok(t) if t.is_dir() => EntryKind::Folder,
                    Ok(t) if t.is_file() => EntryKind::File,
                    _ => EntryKind::Other,
                };
                listed.push((e.file_name(), kind));
            }
            if is_hidden {
                continue;
            }
            let Ok(ft) = ft else { continue };
            let dir = if ft.is_symlink() {
                // a link (or a Windows junction): a folder is entered once, a file is a file
                match std::fs::metadata(&p) {
                    Ok(m) if m.is_dir() => {
                        let Ok(c) = std::fs::canonicalize(&p) else { continue };
                        if !linked.insert(c) {
                            continue;
                        }
                        true
                    }
                    _ => false,
                }
            } else {
                ft.is_dir()
            };
            children.push((p, dir));
        }
        if let (Some((note, _)), Some(listed)) = (note.as_mut(), listed)
            && !note(&path, listed, complete, modified)
        {
            break;
        }
        children.sort();
        let below = depth.saturating_add(1);
        stack.extend(children.into_iter().rev().map(|(p, d)| (p, below, d)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-walk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn hidden_names_and_platform_attributes_are_excluded_from_walks() {
        let root = tmp("hidden-attributes");
        for folder in [".dot", "Hidden"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
            std::fs::write(root.join(folder).join("inside.jpg"), b"x").unwrap();
        }
        for file in ["Shown.jpg", ".dot.jpg", "hidden.jpg"] {
            std::fs::write(root.join(file), b"x").unwrap();
        }
        for name in ["Hidden", "hidden.jpg"] {
            #[cfg(windows)]
            assert!(std::process::Command::new("attrib").arg("+h").arg(root.join(name)).status().unwrap().success());
            #[cfg(target_os = "macos")]
            assert!(std::process::Command::new("chflags").arg("hidden").arg(root.join(name)).status().unwrap().success());
            assert_eq!(is_hidden_path(&root.join(name)), cfg!(any(windows, target_os = "macos")));
        }
        let mut found = files_in(&root, None, Limits::default(), |_| true).files;
        found.sort();
        let mut expected = vec![root.join("Shown.jpg")];
        if !cfg!(any(windows, target_os = "macos")) {
            expected.extend([root.join("Hidden/inside.jpg"), root.join("hidden.jpg")]);
        }
        expected.sort();
        assert_eq!(found, expected);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Given a folder with files, a folder inside it, a hidden file and a link, when it is walked
    /// with its listings noted, then the walk finds what it finds without them, and each folder's
    /// listing holds every entry by name with its kind (hidden ones too, a link as neither file nor
    /// folder) and says it is all of them; a walk that stops inside a folder says it is not.
    #[test]
    fn a_walk_notes_each_folders_listing() {
        let root = tmp("noting");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for name in ["a.jpg", ".hidden", "sub/b.jpg"] {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("a.jpg"), root.join("link.jpg")).unwrap();
        let mut noted: Vec<(PathBuf, Vec<(String, EntryKind)>, bool)> = Vec::new();
        let walked = files_in_noting(&root, None, Limits::default(), |_| true, &mut |dir, entries, complete, _| {
            let mut entries: Vec<(String, EntryKind)> = entries.into_iter().map(|(n, k)| (n.to_string_lossy().to_string(), k)).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            noted.push((dir.to_path_buf(), entries, complete));
            true
        });
        assert_eq!(walked, files_in(&root, None, Limits::default(), |_| true), "the same walk, noted or not");
        let named = |n: &str, k: EntryKind| (n.to_string(), k);
        let mut top = vec![named(".hidden", EntryKind::File), named("a.jpg", EntryKind::File), named("sub", EntryKind::Folder)];
        if cfg!(unix) {
            top.insert(2, named("link.jpg", EntryKind::Other));
        }
        assert_eq!(noted, [(root.clone(), top, true), (root.join("sub"), vec![named("b.jpg", EntryKind::File)], true)]);

        // stopped inside the first folder: its listing is not all of it
        let mut cut = Vec::new();
        let walked = files_in_noting(&root, None, Limits { max_depth: 8, max_entries: 2 }, |_| true, &mut |dir, entries, complete, _| {
            cut.push((dir.to_path_buf(), entries.len(), complete));
            true
        });
        assert!(walked.truncated);
        assert_eq!(cut.first().map(|(dir, n, complete)| (dir.clone(), *n, *complete)), Some((root.clone(), 2, false)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_come_in_path_order_without_hidden_ones_or_the_skipped_folder() {
        let d = tmp("order");
        for f in ["b/2.jpg", "a/1.jpg", "a/x/0.jpg", "c.jpg", ".hidden/h.jpg", "a/.h.jpg", "lib/l.jpg", "a/n.txt"] {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        let lib = d.join("lib");
        let w = files_in(&d, Some(&lib), Limits::default(), |p| p.extension().is_some_and(|e| e == "jpg"));
        let rel: Vec<String> = w.files.iter().map(|p| p.strip_prefix(&d).unwrap().to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(rel, ["a/1.jpg", "a/x/0.jpg", "b/2.jpg", "c.jpg"]);
        assert!(!w.truncated);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Issue #374: a walk never runs on without end — a tree deeper than the limit, or with more
    /// entries than the limit, stops and says so.
    #[test]
    fn deep_and_wide_trees_stop_at_the_limits() {
        let d = tmp("bounds");
        let mut deep = d.join("deep");
        for i in 0..12 {
            deep = deep.join(format!("l{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("bottom.jpg"), b"x").unwrap();
        let wide = d.join("wide");
        std::fs::create_dir_all(&wide).unwrap();
        for i in 0..50 {
            std::fs::write(wide.join(format!("{i:02}.jpg")), b"x").unwrap();
        }
        let all = |_: &Path| true;
        let w = files_in(&d.join("deep"), None, Limits { max_depth: 5, max_entries: 1000 }, all);
        assert!(w.files.is_empty() && w.truncated, "{w:?}");
        let w = files_in(&d.join("deep"), None, Limits { max_depth: 13, max_entries: 1000 }, all);
        assert_eq!((w.files.len(), w.truncated), (1, false), "{w:?}");
        let w = files_in(&wide, None, Limits { max_depth: 5, max_entries: 20 }, all);
        assert_eq!((w.files.len(), w.entries, w.truncated), (20, 20, true));
        let w = files_in(&wide, None, Limits::default(), all);
        assert_eq!((w.files.len(), w.truncated), (50, false));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Folder imports must not revisit the user's own library through symlinks
    /// or through an alias to the library's parent.
    #[cfg(unix)]
    #[test]
    fn skipped_library_stays_skipped_through_symlinks_and_ancestors() {
        let d = tmp("skip-alias");
        let root = d.join("incoming");
        let library = d.join("library");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(library.join("Originals")).unwrap();
        std::fs::write(root.join("ordinary.jpg"), b"photo").unwrap();
        std::fs::write(library.join("Originals").join("library-only.jpg"), b"original").unwrap();
        std::os::unix::fs::symlink(&library, root.join("library-link")).unwrap();
        std::os::unix::fs::symlink(&d, root.join("parent-link")).unwrap();

        let photos = files_in(&root, Some(&library), Limits::default(), |p| p.extension().is_some_and(|e| e == "jpg"));
        assert!(photos.files.contains(&root.join("ordinary.jpg")));
        assert!(!photos.files.iter().any(|p| p.ends_with("library-only.jpg")), "{photos:?}");

        // Scanning a subfolder already inside the excluded library is also forbidden.
        let inside = files_in(&library.join("Originals"), Some(&library), Limits::default(), |_| true);
        assert!(inside.files.is_empty(), "{inside:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A link back up the tree (a loop) is followed at most once: the walk ends.
    #[cfg(unix)]
    #[test]
    fn link_loops_end() {
        let d = tmp("loop");
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/p.jpg"), b"x").unwrap();
        std::os::unix::fs::symlink(&d, d.join("a/b/up")).unwrap();
        std::os::unix::fs::symlink(d.join("a"), d.join("alias")).unwrap();
        let w = files_in(&d, None, Limits::default(), |_| true);
        assert!(!w.truncated, "{w:?}");
        assert!(w.files.len() <= 2, "{w:?}");
        assert!(w.files.iter().any(|p| p.ends_with("a/b/p.jpg")), "{w:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
