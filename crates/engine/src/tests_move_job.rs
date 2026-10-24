//! Moving photos to another folder as a job the app runs off the UI thread (`move_job`):
//!
//! * the activity row shows how far it has got;
//! * while a move runs, nothing else moves files or opens another library, and the moving photos'
//!   sidecars are written once they arrive, at the new place;
//! * the checks that read the disk refuse on the worker, before anything moves;
//! * a worker that dies leaves the library following the files that had arrived.

use std::path::{Path, PathBuf};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;

/// A scratch folder with a library, `local/` and `share/`, gone with the test.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-movejob-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("local")).unwrap();
        std::fs::create_dir_all(dir.join("share")).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().to_string()
    }
    fn library(&self) -> PathBuf {
        self.0.join("library")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open(dir: &Scratch) -> Session {
    let mut s = Session::new();
    s.open_library(dir.library(), false).unwrap();
    s
}

/// A photo file `path` in the library (journaled, so it survives a reopen).
fn add(s: &mut Session, path: &str, body: &str) -> PhotoId {
    std::fs::write(path, body).unwrap();
    let id = s.catalog.alloc_photo_id();
    let name = Path::new(path).file_name().unwrap().to_string_lossy().to_string();
    let p = Photo::new(id, Source::File { path: path.into() }, &name, "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.execute_fn("test.add", |s| {
        s.commit("Add", Op::AddPhoto { photo: Box::new(p) })?;
        Ok(Value::Null)
    })
    .unwrap();
    id
}

fn file_of(s: &Session, id: PhotoId) -> String {
    match &s.catalog.photo(id).unwrap().source {
        Source::File { path } => path.clone(),
        _ => panic!("not a file"),
    }
}

#[test]
fn the_activity_row_shows_progress_and_the_move_is_one_undo_step() {
    let dir = Scratch::new("progress");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    std::fs::write(dir.path("local/a.xmp"), "<edits/>").unwrap();
    let b = add(&mut s, &dir.path("local/b.jpg"), "B");
    let steps = s.undo.len();
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0, b.0], "folder": dir.path("share")})).unwrap().unwrap();
    let row = s.activity.list().into_iter().find(|t| t.kind == "move").unwrap();
    assert_eq!((row.label.as_str(), row.done, row.total, row.cancellable), ("Moving photos", 0, 2, false));
    // the files move while the session isn't asked for anything; the row has caught up when they are done
    let out = work.run();
    let row = s.activity.list().into_iter().find(|t| t.kind == "move").unwrap();
    assert_eq!((row.done, row.total, row.detail.as_str()), (2, 2, "b.jpg"));
    // the library only follows when the session finishes the move
    assert_eq!(file_of(&s, a), dir.path("local/a.jpg"));
    let r = s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    assert_eq!(r, json!({"moved": 2, "folder": dir.path("share")}));
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"));
    assert_eq!(file_of(&s, b), dir.path("share/b.jpg"));
    assert!(Path::new(&dir.path("share/a.xmp")).is_file() && !Path::new(&dir.path("local/a.xmp")).exists(), "the sidecar goes along");
    assert!(s.activity.list().is_empty(), "the row is gone");
    assert!(!s.move_running());
    assert_eq!(s.undo.len(), steps + 1);
    assert_eq!(s.undo.last().unwrap().label, "Move 2 Photos to “share”");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(file_of(&s, a), dir.path("local/a.jpg"));
    assert!(Path::new(&dir.path("local/a.xmp")).is_file() && Path::new(&dir.path("local/b.jpg")).is_file());
}

#[test]
fn a_running_move_holds_back_other_file_moves_and_another_library() {
    let dir = Scratch::new("busy");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let b = add(&mut s, &dir.path("local/b.jpg"), "B");
    // a step that moved files, to undo while the next move runs
    s.execute("photo.moveToFolder", &json!({"ids": [b.0], "folder": dir.path("share")})).unwrap();
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    assert!(s.move_running());
    let busy = |r: crate::Result<Value>| {
        let e = r.unwrap_err().to_string();
        assert!(e.contains("being moved"), "{e}");
    };
    busy(s.execute("photo.moveToFolder", &json!({"ids": [b.0], "folder": dir.path("local")})));
    s.selection = crate::Selection::single(b);
    busy(s.execute("photo.rename", &json!({"ids": [b.0], "template": "renamed"})));
    busy(s.execute("folder.rename", &json!({"path": dir.path("share"), "name": "nas"})));
    busy(s.execute("edit.undo", &json!({})));
    let other = dir.0.join("other-library");
    let e = Session::open_library(&mut s, &other, false).map(|_| ()).unwrap_err().to_string();
    assert!(e.contains("being moved"), "{e}");
    // a switch checks before it closes anything: refused there, the library stays open as it is
    let e = s.check_library(&other).unwrap_err().to_string();
    assert!(e.contains("being moved"), "{e}");
    // an edit that moves no file still goes
    s.execute("photo.rate", &json!({"ids": [b.0], "rating": 3})).unwrap();
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    // and once it's over, all of it works again
    s.execute("edit.undo", &json!({})).unwrap();
    s.open_library(&other, false).unwrap();
}

#[test]
fn a_running_move_refuses_opening_before_touching_the_target() {
    let dir = Scratch::new("busy-open");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    // a target that doesn't exist yet: refused without creating its folder or lock file
    let missing = dir.0.join("not-yet");
    let e = s.open_library(&missing, false).map(|_| ()).unwrap_err().to_string();
    assert!(e.contains("being moved"), "{e}");
    assert!(!missing.exists(), "the refused open left nothing behind");
    // a target another holder has locked: the move is what's reported, not the lock
    let held = dir.0.join("held");
    let _lock = {
        let mut other = Session::new();
        other.open_library(&held, false).unwrap();
        other
    };
    let e = s.open_library(&held, false).map(|_| ()).unwrap_err().to_string();
    assert!(e.contains("being moved"), "{e}");
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
}

#[test]
fn a_moving_photos_sidecar_is_written_once_it_arrives_where_it_arrives() {
    let dir = Scratch::new("sidecar");
    let mut s = open(&dir);
    s.xmp.auto_write = true;
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    s.selection = crate::Selection::single(a);
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 4})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().rating, 4, "the edit is kept");
    assert!(!Path::new(&dir.path("local/a.xmp")).exists(), "nothing is written behind the moving file");
    assert_eq!(s.unsaved_sidecars(), 0, "held for the move, not a failed write to warn about");
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    let xmp = std::fs::read_to_string(dir.path("share/a.xmp")).expect("written at the new place");
    assert!(xmp.contains("xmp:Rating=\"4\"") || xmp.contains("<xmp:Rating>4</xmp:Rating>"), "{xmp}");
    assert!(!Path::new(&dir.path("local/a.xmp")).exists());
    assert_eq!(s.unsaved_sidecars(), 0);
}

#[test]
fn a_move_whose_worker_dies_is_followed_and_its_held_sidecars_are_written() {
    let dir = Scratch::new("abandon");
    let mut s = open(&dir);
    s.xmp.auto_write = true;
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let b = add(&mut s, &dir.path("local/b.jpg"), "B");
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0, b.0], "folder": dir.path("share")})).unwrap().unwrap();
    s.selection = crate::Selection::single(a);
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 5})).unwrap();
    // the worker died after the first file, with the second half-copied: no outcome reaches the session
    std::fs::rename(dir.path("local/a.jpg"), dir.path("share/a.jpg")).unwrap();
    std::fs::write(dir.path("share/b.jpg"), "B (partial)").unwrap();
    drop(work);
    let r = s.execute_fn("photo.moveToFolder", |s| finish.abandon(s)).unwrap();
    assert_eq!(r, json!({"moved": 1, "folder": dir.path("share")}));
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"), "the library follows the file that arrived");
    assert_eq!(file_of(&s, b), dir.path("local/b.jpg"), "and leaves the one that did not");
    let xmp = std::fs::read_to_string(dir.path("share/a.xmp")).expect("the held sidecar is written where the photo is");
    assert!(xmp.contains("xmp:Rating=\"5\"") || xmp.contains("<xmp:Rating>5</xmp:Rating>"), "{xmp}");
    assert!(!s.move_running() && s.activity.list().is_empty());
}

#[test]
fn checks_that_read_the_disk_refuse_on_the_worker_before_anything_moves() {
    let dir = Scratch::new("refuse");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let b = add(&mut s, &dir.path("local/b.jpg"), "B");
    // the name is taken after the move was planned: the worker sees it
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0, b.0], "folder": dir.path("share")})).unwrap().unwrap();
    std::fs::write(dir.path("share/b.jpg"), "someone else's").unwrap();
    let out = work.run();
    let e = s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap_err().to_string();
    assert!(e.contains("b.jpg already in") && e.contains("nothing was moved"), "{e}");
    assert_eq!(file_of(&s, a), dir.path("local/a.jpg"));
    assert!(Path::new(&dir.path("local/a.jpg")).is_file() && !Path::new(&dir.path("share/a.jpg")).exists());
    assert_eq!(std::fs::read_to_string(dir.path("share/b.jpg")).unwrap(), "someone else's");
    assert!(!s.move_running());
}

#[test]
fn held_sidecars_are_not_written_when_auto_write_was_switched_off_during_the_move() {
    let dir = Scratch::new("autowrite-off");
    let mut s = open(&dir);
    s.xmp.auto_write = true;
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    s.selection = crate::Selection::single(a);
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 4})).unwrap();
    s.xmp.auto_write = false;
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    assert!(!Path::new(&dir.path("share/a.xmp")).exists() && !Path::new(&dir.path("local/a.xmp")).exists());
    assert_eq!(s.unsaved_sidecars(), 0);
}
