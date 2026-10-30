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

/// A folder spelled with `..` is the folder the library knows by its plain path: its photos follow.
#[test]
fn a_folder_spelled_with_dot_dot_still_takes_its_photos_along() {
    let dir = Scratch::new("dotdot");
    let (mut s, a, _b, archive) = two_folders(&dir);
    let spelled = format!("{a}/../b");
    let r = s.execute("folder.move", &json!({"paths": [spelled], "into": archive})).unwrap();
    assert_eq!(r["relinked"], 1);
    assert_eq!(r["paths"], json!([dir.path("archive/b")]));
    assert!(dir.0.join("archive/b").is_dir() && !dir.0.join("b").exists());
    assert_eq!(photo_paths(&s), [dir.path("a/one.jpg"), dir.path("archive/b/two.jpg")]);
}

/// The destination spelled with `..` is stored plainly, or a later move of the folder would miss its photos.
#[test]
fn a_destination_spelled_with_dot_dot_stores_plain_photo_paths() {
    let dir = Scratch::new("dotdot-into");
    let (mut s, a, _b, archive) = two_folders(&dir);
    let spelled = format!("{archive}/../archive");
    s.execute("folder.move", &json!({"paths": [a], "into": spelled})).unwrap();
    assert_eq!(photo_paths(&s), [dir.path("archive/a/one.jpg"), dir.path("b/two.jpg")]);
}

/// On Unix `\` is a name character: a folder called `w\` is that folder, not its sibling `w`.
#[cfg(unix)]
#[test]
fn a_trailing_backslash_is_part_of_the_folders_name_on_unix() {
    let dir = Scratch::new("backslash");
    let (mut s, _a, _b, archive) = two_folders(&dir);
    let (plain, odd) = (dir.dir("w"), dir.dir("w\\"));
    add(&mut s, &format!("{plain}/p.jpg"));
    add(&mut s, &format!("{odd}/q.jpg"));
    s.execute("folder.move", &json!({"paths": [odd], "into": archive})).unwrap();
    assert!(dir.0.join("w").is_dir(), "the sibling stays");
    assert!(dir.0.join("archive").join("w\\").is_dir());
    let paths = photo_paths(&s);
    assert!(paths.contains(&format!("{}/w\\/q.jpg", dir.path("archive"))) && paths.contains(&format!("{plain}/p.jpg")), "{paths:?}");
    // and as the destination
    let into = dir.dir("dest\\");
    let plain_dest = dir.dir("dest");
    s.execute("folder.move", &json!({"paths": [dir.path("b")], "into": into})).unwrap();
    assert!(dir.0.join("dest\\").join("b").is_dir() && !dir.0.join("dest").join("b").exists(), "{plain_dest}");
}

/// A photo stored under a path that still holds `..` goes along with its folder, and is stored plainly after.
#[test]
fn a_photo_stored_with_dot_dot_follows_a_batch_move() {
    let dir = Scratch::new("stored-dotdot");
    let (mut s, a, b, archive) = two_folders(&dir);
    add(&mut s, &format!("{a}/../b/three.jpg"));
    std::fs::write(std::path::Path::new(&b).join("three.jpg"), b"x").unwrap();
    let r = s.execute("folder.move", &json!({"paths": [b], "into": archive})).unwrap();
    assert_eq!(r["relinked"], 2);
    assert_eq!(photo_paths(&s), [dir.path("a/one.jpg"), dir.path("archive/b/three.jpg"), dir.path("archive/b/two.jpg")]);
}

/// A photo stored as `<a>/../b/…` is in `b`, not in `a`: moving `a` leaves it alone.
#[test]
fn a_photo_stored_with_dot_dot_is_not_taken_along_by_the_folder_it_passes_through() {
    let dir = Scratch::new("stored-dotdot-other");
    let (mut s, a, _b, archive) = two_folders(&dir);
    let stored = format!("{a}/../b/three.jpg");
    add(&mut s, &stored);
    let r = s.execute("folder.move", &json!({"paths": [a], "into": archive})).unwrap();
    assert_eq!(r["relinked"], 1);
    assert!(photo_paths(&s).contains(&stored), "{:?}", photo_paths(&s));
}

/// The single-folder form spells its source plainly too: a folder given with `..` takes its photos along.
#[test]
fn a_single_folder_spelled_with_dot_dot_takes_its_photos_along() {
    let dir = Scratch::new("single-dotdot");
    let (mut s, a, _b, archive) = two_folders(&dir);
    let r = s.execute("folder.move", &json!({"path": format!("{a}/../b"), "into": archive})).unwrap();
    assert_eq!(r["relinked"], 1);
    assert_eq!(photo_paths(&s), [dir.path("a/one.jpg"), dir.path("archive/b/two.jpg")]);
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

/// The real disk, except that chosen folder moves fail (`(from, to)` pairs), to take a step half way.
struct Flaky {
    fail: Vec<(String, String)>,
}

impl crate::rename::MoveFs for Flaky {
    fn exists(&self, p: &std::path::Path) -> bool {
        crate::rename::RealFs.exists(p)
    }
    fn is_file(&self, p: &std::path::Path) -> bool {
        crate::rename::RealFs.is_file(p)
    }
    fn same_file(&self, a: &std::path::Path, b: &std::path::Path) -> bool {
        crate::rename::RealFs.same_file(a, b)
    }
    fn rename_no_replace(&self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> {
        crate::rename::RealFs.rename_no_replace(a, b)
    }
    fn copy_no_replace(&self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> {
        crate::rename::RealFs.copy_no_replace(a, b)
    }
    fn remove_file(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::rename::RealFs.remove_file(p)
    }
    fn stem_sibling(&self, sidecar: &std::path::Path) -> Option<String> {
        crate::rename::RealFs.stem_sibling(sidecar)
    }
    fn move_folder(&self, from: &str, to: &str) -> Result<(), String> {
        if self.fail.iter().any(|(f, t)| f == from && t == to) {
            return Err(format!("{from} → {to}: injected failure"));
        }
        crate::cmd::browse::folder_on_disk(from, to)
    }
}

#[test]
fn an_undo_whose_rollback_fails_keeps_the_history_and_the_photos_matching_the_disk() {
    let dir = Scratch::new("rollback");
    let (mut s, a, b, archive) = two_folders(&dir);
    std::fs::write(dir.0.join("a/one.jpg"), b"1").unwrap();
    std::fs::write(dir.0.join("b/two.jpg"), b"2").unwrap();
    s.execute("folder.label", &json!({"path": a.clone(), "label": "red"})).unwrap();
    s.execute("folder.move", &json!({"paths": [a.clone(), b.clone()], "into": archive})).unwrap();
    assert_eq!(photo_paths(&s), [dir.path("archive/a/one.jpg"), dir.path("archive/b/two.jpg")]);
    let steps = s.undo.len();
    // Undo puts `a` back, then `b` can't go, and `a` can't be sent back to the archive
    let flaky = Flaky { fail: vec![(dir.path("archive/b"), b.clone()), (a.clone(), dir.path("archive/a"))] };
    let e = s.undo_step_with(&flaky).unwrap_err().to_string();
    assert!(e.contains(&dir.path("archive/b")) || e.contains("can't move the folder back"), "{e}");
    assert!(e.contains(&format!("{a} (")) && e.contains("could not be moved back"), "the stranded folder is named: {e}");
    // `a` is where Undo put it, and the library follows it; `b` never moved
    assert!(dir.0.join("a/one.jpg").exists() && dir.0.join("archive/b/two.jpg").exists());
    assert_eq!(photo_paths(&s), [dir.path("a/one.jpg"), dir.path("archive/b/two.jpg")], "the catalog matches the disk");
    assert_eq!(s.undo.len(), steps, "the step is still there to try again");
    assert!(s.catalog.folder_record(&a).is_some_and(|r| r.label.is_some()), "the folder's label stays with it");
    assert!(s.catalog.folder_record(&dir.path("archive/a")).is_none());
    assert!(s.redo.is_empty());
    // the retry doesn't try to move `a` again and finishes the step
    s.undo_step_with(&Flaky { fail: Vec::new() }).unwrap();
    assert_eq!(photo_paths(&s), [dir.path("a/one.jpg"), dir.path("b/two.jpg")]);
    assert!(dir.0.join("a/one.jpg").exists() && dir.0.join("b/two.jpg").exists() && !dir.0.join("archive/a").exists());
    // and Redo takes both back into the archive
    s.redo_step_with(&Flaky { fail: Vec::new() }).unwrap();
    assert_eq!(photo_paths(&s), [dir.path("archive/a/one.jpg"), dir.path("archive/b/two.jpg")]);
    assert!(dir.0.join("archive/a/one.jpg").exists() && dir.0.join("archive/b/two.jpg").exists());
}
