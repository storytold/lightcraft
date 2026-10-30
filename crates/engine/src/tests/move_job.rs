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

/// Undoing the import of a photo whose file is on its way to another folder is a catalog-only step
/// (no file to move), but it would leave the move's finish with no photo to point at the new place.
#[test]
fn undoing_the_import_of_a_moving_photo_waits_for_the_move() {
    let dir = Scratch::new("busy-remove");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    let e = s.execute("edit.undo", &json!({})).unwrap_err().to_string();
    assert_eq!(e, "1 photo is being moved to another folder: wait for the move to finish");
    assert!(s.catalog.photo(a).is_some(), "the photo stays while its file moves");
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"));
    // once the move is over, the steps undo in turn: the move, then the import
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(file_of(&s, a), dir.path("local/a.jpg"));
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.photo(a).is_none());
}

/// The same for undoing a relink (Convert to DNG, Find Missing Photos): it would point the photo
/// back at the old file while its current one is on its way to another folder.
#[test]
fn undoing_a_relink_of_a_moving_photo_waits_for_the_move() {
    let dir = Scratch::new("busy-relink");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    std::fs::write(dir.path("local/a2.jpg"), "A2").unwrap();
    let to = dir.path("local/a2.jpg");
    s.execute_fn("test.relink", |s| {
        s.commit("Relink", crate::cmd::missing::relink_op(a, &to))?;
        Ok(Value::Null)
    })
    .unwrap();
    assert_eq!(file_of(&s, a), to);
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    let e = s.execute("edit.undo", &json!({})).unwrap_err().to_string();
    assert!(e.contains("being moved"), "{e}");
    assert_eq!(file_of(&s, a), to, "the photo keeps pointing at the file that moves");
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    assert_eq!(file_of(&s, a), dir.path("share/a2.jpg"));
}

/// A photo whose file is on its way to another folder is refused everything that would take it out
/// of the library or point it at another file; the library keeps it where the move's finish looks.
/// Run with the move in flight, then let it finish and check it still followed the file.
fn moving_one(tag: &str) -> (Scratch, Session, PhotoId, crate::move_job::MoveWork, crate::move_job::MoveFinish) {
    let dir = Scratch::new(tag);
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    (dir, s, a, work, finish)
}

fn finish_and_check(s: &mut Session, dir: &Scratch, a: PhotoId, work: crate::move_job::MoveWork, finish: crate::move_job::MoveFinish) {
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    assert_eq!(file_of(s, a), dir.path("share/a.jpg"), "the library followed the file");
}

#[test]
fn delete_permanently_waits_for_the_move_of_the_photos_it_would_remove() {
    let dir = Scratch::new("busy-delete");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let b = add(&mut s, &dir.path("local/b.jpg"), "B");
    let c = add(&mut s, &dir.path("local/c.jpg"), "C");
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0, b.0], "folder": dir.path("share")})).unwrap().unwrap();
    s.selection = crate::Selection::single(a);
    let steps = s.undo.len();
    let err = s.execute("photo.deletePermanently", &json!({"ids": [a.0]})).unwrap_err();
    assert_eq!(err.kind(), crate::ErrorKind::MoveInProgress, "{err}");
    let e = err.to_string();
    assert_eq!(e, "1 photo is being moved to another folder: wait for the move to finish");
    // a batch is refused as a whole when any of its photos moves, and says how many
    let e = s.execute("photo.deletePermanently", &json!({"ids": [a.0, b.0, c.0]})).unwrap_err().to_string();
    assert_eq!(e, "2 photos are being moved to another folder: wait for the move to finish");
    assert!([a, b, c].iter().all(|id| s.catalog.photo(*id).is_some()), "nothing was removed");
    assert_eq!(s.undo.len(), steps);
    // a photo that stays is not held back
    s.execute("photo.deletePermanently", &json!({"ids": [c.0]})).unwrap();
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"));
    s.execute("photo.deletePermanently", &json!({"ids": [a.0]})).unwrap();
    assert!(s.catalog.photo(a).is_none(), "once the move is over it goes");
}

#[test]
fn empty_recently_deleted_waits_for_the_move_of_a_trashed_photo() {
    let (dir, mut s, a, work, finish) = moving_one("busy-empty");
    // the photo is put in Recently Deleted while its file moves: that detaches nothing
    s.selection = crate::Selection::single(a);
    s.execute("photo.delete", &json!({"ids": [a.0]})).unwrap();
    let e = s.execute("library.emptyRecentlyDeleted", &json!({})).unwrap_err().to_string();
    assert_eq!(e, "1 photo is being moved to another folder: wait for the move to finish");
    assert!(s.catalog.photo(a).is_some());
    finish_and_check(&mut s, &dir, a, work, finish);
    s.execute("library.emptyRecentlyDeleted", &json!({})).unwrap();
    assert!(s.catalog.photo(a).is_none());
}

#[test]
fn the_imports_purge_of_a_trashed_photo_waits_for_its_move() {
    let dir = Scratch::new("busy-purge");
    let mut s = open(&dir);
    let path = dir.path("local/a.png");
    let img =
        lightcraft_raster::Rgba8 { width: 24, height: 16, data: (0..24 * 16).map(|i| [(i % 24 * 9) as u8, (i / 24 * 11) as u8, 7, 255]).collect() };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let imported = s.execute("library.import", &json!({"paths": [path]})).unwrap();
    let a = PhotoId(imported["imported"][0].as_u64().unwrap());
    s.execute("photo.delete", &json!({"ids": [a.0]})).unwrap();
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    // importing the same file as new would delete the trashed record its file belongs to
    let e = s.execute("library.import", &json!({"paths": [path], "onDeleted": "fresh"})).unwrap_err().to_string();
    assert_eq!(e, "1 photo is being moved to another folder: wait for the move to finish");
    assert!(s.catalog.photo(a).is_some_and(|p| p.deleted), "the trashed record stays");
    assert_eq!(s.catalog.len(), 1, "and nothing was added");
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    assert_eq!(file_of(&s, a), dir.path("share/a.png"));
}

#[test]
fn convert_to_dng_waits_for_the_move_of_the_raw() {
    let dir = Scratch::new("busy-convert");
    let mut s = open(&dir);
    let path = dir.path("local/a.arw");
    std::fs::write(&path, "RAW").unwrap();
    let a = s.catalog.alloc_photo_id();
    let mut p = Photo::new(a, Source::File { path: path.clone() }, "a.arw", "ARW", 60, 40, "2026-01-01T10:00:00");
    p.kind = lightcraft_catalog::MediaKind::Raw;
    s.execute_fn("test.add", |s| {
        s.commit("Add", Op::AddPhoto { photo: Box::new(p) })?;
        Ok(Value::Null)
    })
    .unwrap();
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    s.selection = crate::Selection::single(a);
    let e = s.execute("photo.convertToDng", &json!({"ids": [a.0]})).unwrap_err().to_string();
    assert!(e.contains("1 photo is being moved to another folder"), "{e}");
    assert!(!Path::new(&dir.path("local/a.dng")).exists(), "no DNG was written beside the moving file");
    assert_eq!(file_of(&s, a), dir.path("local/a.arw"));
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    assert_eq!(file_of(&s, a), dir.path("share/a.arw"));
}

#[test]
fn find_missing_and_locate_wait_for_the_move_of_the_photos_they_would_relink() {
    let (dir, mut s, a, work, finish) = moving_one("busy-find");
    std::fs::write(dir.path("local/a2.jpg"), "A2").unwrap();
    let from = dir.path("local/a.jpg");
    let to = dir.path("local/a2.jpg");
    let found = json!({"found": [{"id": a.0, "from": from, "to": to, "by": "name"}], "missing": 0, "ambiguous": []});
    let e = s.execute("library.findMissing", &found).unwrap_err().to_string();
    assert_eq!(e, "1 photo is being moved to another folder: wait for the move to finish");
    let e = s.execute("photo.relink", &json!({"id": a.0, "path": to})).unwrap_err().to_string();
    assert_eq!(e, "1 photo is being moved to another folder: wait for the move to finish");
    assert_eq!(file_of(&s, a), from, "the photo keeps pointing at the file that moves");
    finish_and_check(&mut s, &dir, a, work, finish);
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
