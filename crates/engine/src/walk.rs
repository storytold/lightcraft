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

fn hidden(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

/// The files below folder `root` that `keep` accepts. Hidden entries (names starting with `.`)
/// are left out, the folder `skip` (e.g. the library's own) is never entered, and the walk stops
/// at `limits` (see [`Walked::truncated`]). Unreadable folders are skipped.
pub fn files_in(root: &Path, skip: Option<&Path>, limits: Limits, mut keep: impl FnMut(&Path) -> bool) -> Walked {
    let mut out = Walked::default();
    // canonical folders reached through a link, so a link loop is walked at most once
    let mut linked: HashSet<PathBuf> = HashSet::new();
    if let Ok(c) = std::fs::canonicalize(root) {
        linked.insert(c);
    }
    // (path, depth of the folder it is in, is a folder); popped in path order
    let mut stack: Vec<(PathBuf, usize, bool)> = vec![(root.to_path_buf(), 0, true)];
    while let Some((path, depth, is_dir)) = stack.pop() {
        if !is_dir {
            if keep(&path) {
                out.files.push(path);
            }
            continue;
        }
        if skip.is_some_and(|s| path == s) {
            continue;
        }
        if depth >= limits.max_depth {
            out.truncated = true;
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&path) else { continue };
        let mut children: Vec<(PathBuf, bool)> = Vec::new();
        for e in rd.flatten() {
            if out.entries >= limits.max_entries {
                out.truncated = true;
                break;
            }
            out.entries = out.entries.saturating_add(1);
            let p = e.path();
            if hidden(&p) {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
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
