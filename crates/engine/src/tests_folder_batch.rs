//! Moving several folders into one (`folder.move {paths, into}`).
//!
//! * Given folders with photos, an agent moves them all into one folder: every folder and every
//!   photo follows, and one Undo (one Redo) puts them all back (moves them all again).
//! * A batch that can't be done as a whole (into itself, one inside another, a name taken, two
//!   with one name, a folder that isn't there) is refused with a plain sentence and nothing
//!   moves, nothing joins the history.
//! * A folder that fails when its turn comes puts the ones already moved back.

use lightcraft_catalog::{Op, Photo, Source};
use serde_json::json;

use crate::Session;

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-folder-batch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).components().collect::<std::path::PathBuf>().to_string_lossy().to_string()
    }
    fn dir(&self, rel: &str) -> String {
        std::fs::create_dir_all(self.0.join(rel)).unwrap();
        self.path(rel)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn add(s: &mut Session, path: &str) {
    let id = s.catalog.alloc_photo_id();
    let p = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
}

fn photo_paths(s: &Session) -> Vec<String> {
    let mut v: Vec<String> = s
        .catalog
        .photos()
        .filter_map(|p| match &p.source {
            Source::File { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    v.sort();
    v
}

/// Two folders with a photo each, and an empty `archive` to move them into.
fn two_folders(dir: &Scratch) -> (Session, String, String, String) {
    let (a, b, archive) = (dir.dir("a"), dir.dir("b"), dir.dir("archive"));
    let mut s = Session::new();
    add(&mut s, &format!("{a}/one.jpg"));
    add(&mut s, &format!("{b}/two.jpg"));
    (s, a, b, archive)
}

#[test]
fn several_folders_move_into_one_as_one_undo_step() {
    let dir = Scratch::new("move");
    let (mut s, a, b, archive) = two_folders(&dir);
    let before = s.undo.len();
    let r = s.execute("folder.move", &json!({"paths": [a, b], "into": archive})).unwrap();
    assert_eq!(r["relinked"], 2);
    assert_eq!(r["paths"], json!([dir.path("archive/a"), dir.path("archive/b")]));
    assert!(dir.0.join("archive/a").is_dir() && dir.0.join("archive/b").is_dir());
    assert!(!dir.0.join("a").exists() && !dir.0.join("b").exists());
    assert_eq!(photo_paths(&s), [dir.path("archive/a/one.jpg"), dir.path("archive/b/two.jpg")]);
    assert_eq!(s.undo.len(), before + 1, "one step for both");

    s.execute("edit.undo", &json!({})).unwrap();
    assert!(dir.0.join("a").is_dir() && dir.0.join("b").is_dir());
    assert!(!dir.0.join("archive/a").exists() && !dir.0.join("archive/b").exists());
    assert_eq!(photo_paths(&s), [dir.path("a/one.jpg"), dir.path("b/two.jpg")]);

    s.execute("edit.redo", &json!({})).unwrap();
    assert!(dir.0.join("archive/a").is_dir() && dir.0.join("archive/b").is_dir());
    assert_eq!(photo_paths(&s), [dir.path("archive/a/one.jpg"), dir.path("archive/b/two.jpg")]);
}

#[test]
fn one_folder_in_a_list_moves_like_one_folder() {
    let dir = Scratch::new("one");
    let (mut s, a, _b, archive) = two_folders(&dir);
    let r = s.execute("folder.move", &json!({"paths": [a], "into": archive})).unwrap();
    assert_eq!(r["paths"], json!([dir.path("archive/a")]));
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(dir.0.join("a").is_dir());
}

/// Refused as a whole: the sentence says why, and neither disk nor library nor history changed.
fn refused(dir: &Scratch, s: &mut Session, params: serde_json::Value, says: &str) {
    let (files, undo) = (photo_paths(s), s.undo.len());
    let on_disk = |d: &Scratch| {
        let mut v: Vec<String> = walk(&d.0);
        v.sort();
        v
    };
    let disk = on_disk(dir);
    let e = s.execute("folder.move", &params).unwrap_err().to_string();
    assert!(e.contains(says), "{e:?} should say {says:?}");
    assert_eq!(photo_paths(s), files, "no photo moved: {e}");
    assert_eq!(s.undo.len(), undo, "no history step: {e}");
    assert_eq!(on_disk(dir), disk, "nothing moved on disk: {e}");
}

fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        out.push(e.path().to_string_lossy().to_string());
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        }
    }
    out
}

#[test]
fn a_batch_that_cannot_move_as_a_whole_is_refused_and_nothing_moves() {
    let dir = Scratch::new("refuse");
    let (mut s, a, b, archive) = two_folders(&dir);
    // into itself, or into a folder inside one of them
    let inside = dir.dir("a/inner");
    refused(&dir, &mut s, json!({"paths": [b.clone(), a.clone()], "into": a.clone()}), "into itself");
    refused(&dir, &mut s, json!({"paths": [b.clone(), a.clone()], "into": inside}), "into itself");
    // one inside another of the batch
    refused(&dir, &mut s, json!({"paths": [a.clone(), dir.path("a/inner")], "into": archive.clone()}), "is inside “a”");
    // a name already taken in the destination
    dir.dir("archive/b");
    refused(&dir, &mut s, json!({"paths": [a.clone(), b.clone()], "into": archive.clone()}), "already has a folder called “b”");
    // two with one name
    let other = dir.dir("elsewhere/a");
    refused(&dir, &mut s, json!({"paths": [a.clone(), other], "into": dir.dir("fresh")}), "two of the folders are called “a”");
    // a folder that isn't there, a destination that isn't there
    refused(&dir, &mut s, json!({"paths": [a.clone(), dir.path("gone")], "into": archive.clone()}), "“gone” is not a folder");
    refused(&dir, &mut s, json!({"paths": [a.clone()], "into": dir.path("nowhere")}), "is not a folder");
    // no folder at all
    let e = s.execute("folder.move", &json!({"paths": [], "into": archive})).unwrap_err().to_string();
    assert!(e.contains("at least one folder"), "{e}");
}

#[test]
fn a_folder_that_fails_late_puts_the_ones_already_moved_back() {
    let dir = Scratch::new("late");
    let (a, b, archive) = (dir.dir("a"), dir.dir("b"), dir.dir("archive"));
    let mut s = Session::new();
    // the library's own folder can't move: it holds the second one, so `a` has already gone
    s.open_library(dir.0.join("b").join("lib"), false).unwrap();
    add(&mut s, &format!("{a}/one.jpg"));
    add(&mut s, &format!("{b}/two.jpg"));
    let before = s.undo.len();
    let e = s.execute("folder.move", &json!({"paths": [a, b], "into": archive})).unwrap_err().to_string();
    assert!(e.contains("library"), "{e}");
    assert!(dir.0.join("a").is_dir() && dir.0.join("b").is_dir(), "both are where they were");
    assert!(!dir.0.join("archive/a").exists());
    assert_eq!(photo_paths(&s), [dir.path("a/one.jpg"), dir.path("b/two.jpg")]);
    assert_eq!(s.undo.len(), before, "no step left behind");
    assert!(s.redo.is_empty());
}

/// A batch whose first folder is refused (the library's own) changed nothing, so what could be
/// redone still can.
#[test]
fn a_batch_refused_at_its_first_folder_keeps_the_redo_history() {
    let dir = Scratch::new("redo");
    let (a, b, archive) = (dir.dir("a"), dir.dir("b"), dir.dir("archive"));
    let mut s = Session::new();
    s.open_library(dir.0.join("b").join("lib"), false).unwrap();
    add(&mut s, &format!("{a}/one.jpg"));
    s.execute("folder.move", &json!({"path": a, "into": archive.clone()})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.redo.len(), 1);
    s.execute("folder.move", &json!({"paths": [b], "into": archive})).unwrap_err();
    assert_eq!(s.redo.len(), 1, "the move that was undone can still be redone");
}

/// More than 200 folders at once are refused before anything is looked at.
#[test]
fn more_than_200_folders_at_once_are_refused() {
    let dir = Scratch::new("many");
    let archive = dir.dir("archive");
    let paths: Vec<String> = (0..201).map(|i| format!("{}/f{i}", dir.path("src"))).collect();
    let e = Session::new().execute("folder.move", &json!({"paths": paths, "into": archive})).unwrap_err().to_string();
    assert!(e.contains("at most 200"), "{e}");
}

/// A batch that fails after some folders moved, and puts them back, leaves the history as it was:
/// what had been undone before can still be redone.
#[test]
fn a_batch_that_fails_late_keeps_the_redo_history() {
    let dir = Scratch::new("redo-late");
    let (a, c, archive) = (dir.dir("a"), dir.dir("c"), dir.dir("archive"));
    let b = dir.dir("b");
    let mut s = Session::new();
    s.open_library(dir.0.join("b").join("lib"), false).unwrap();
    add(&mut s, &format!("{a}/one.jpg"));
    add(&mut s, &format!("{c}/two.jpg"));
    s.execute("folder.move", &json!({"path": c, "into": archive.clone()})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.redo.len(), 1);
    // `a` moves, then the library's own folder refuses and `a` goes back
    s.execute("folder.move", &json!({"paths": [a, b], "into": archive})).unwrap_err();
    assert!(dir.0.join("a").is_dir());
    assert_eq!(s.redo.len(), 1, "the undone move is still there to redo");
    s.execute("edit.redo", &json!({})).unwrap();
    assert!(dir.0.join("archive/c").is_dir() && !dir.0.join("c").exists());
}
