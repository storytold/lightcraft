//! Moving photos to another folder as a job the app runs off the UI thread (`move_job`), and the
//! record that lets the next open follow a move cut short (`move_intent`):
//!
//! * the activity row shows how far it has got;
//! * while a move runs, nothing else moves files or opens another library, and the moving photos'
//!   sidecars are written once they arrive, at the new place;
//! * the checks that read the disk refuse on the worker, before anything moves;
//! * a record is on disk before the first file moves and gone once the library is saved;
//! * a move cut short (the app killed) is followed the next time the library opens.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;
use crate::rename::{MoveFs, RealFs};

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
    fn records(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.library().join("moves"))
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect())
            .unwrap_or_default()
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
    let held = dir.library().join("moves/held-sidecars");
    assert!(held.is_file(), "the held write is on disk too, for a run that ends before the move");
    let out = work.run();
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    assert!(!held.exists(), "and gone once it is made: the next open mustn't make it again");
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
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    s.selection = crate::Selection::single(a);
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 5})).unwrap();
    // the files moved, then the worker died before saying so: no outcome reaches the session
    drop(work.run());
    let r = s.execute_fn("photo.moveToFolder", |s| Ok(json!(finish.abandon(s)))).unwrap();
    assert_eq!(r["relinked"], 1, "{r}");
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"), "the library follows the file");
    let xmp = std::fs::read_to_string(dir.path("share/a.xmp")).expect("the held sidecar is written where the photo is");
    assert!(xmp.contains("xmp:Rating=\"5\"") || xmp.contains("<xmp:Rating>5</xmp:Rating>"), "{xmp}");
    assert!(!s.move_running() && s.activity.list().is_empty());
    assert!(dir.records().is_empty(), "{:?}", dir.records());
}

#[test]
fn a_worker_that_dies_in_a_library_without_a_folder_still_has_its_files_followed() {
    // no library folder: no record is written, so nothing but the plans says where the files went
    let dir = Scratch::new("abandon-no-record");
    let mut s = Session::new();
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let b = add(&mut s, &dir.path("local/b.jpg"), "B");
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0, b.0], "folder": dir.path("share")})).unwrap().unwrap();
    drop(work.run());
    // a file that never arrived (the worker died before it): it stays where it is
    std::fs::rename(dir.path("share/b.jpg"), dir.path("local/b.jpg")).unwrap();
    let r = s.execute_fn("photo.moveToFolder", |s| Ok(json!(finish.abandon(s)))).unwrap();
    assert_eq!(r["relinked"], 1, "{r}");
    assert_eq!(r["problems"], json!([]), "{r}");
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"), "the library follows the file that arrived");
    assert_eq!(file_of(&s, b), dir.path("local/b.jpg"), "and leaves the one that did not");
    assert!(!s.move_running() && s.activity.list().is_empty());
}

#[test]
fn without_a_record_a_sidecar_the_worker_did_not_reach_follows_its_photo() {
    let dir = Scratch::new("abandon-no-record-sidecar");
    let mut s = Session::new();
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    drop(work.run());
    // the worker died between the photo file and its sidecar
    std::fs::write(dir.path("local/a.xmp"), "<edits/>").unwrap();
    let r = s.execute_fn("photo.moveToFolder", |s| Ok(json!(finish.abandon(s)))).unwrap();
    assert_eq!(r["relinked"], 1, "{r}");
    assert_eq!(r["problems"], json!([]), "{r}");
    assert!(Path::new(&dir.path("share/a.xmp")).is_file() && !Path::new(&dir.path("local/a.xmp")).exists(), "{r}");
}

#[test]
fn a_dead_workers_message_is_not_about_the_records_of_earlier_runs() {
    let dir = Scratch::new("abandon-earlier-records");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    // a record an earlier run left that this version can't read (the open reported it; it stays as it is)
    let earlier = dir.library().join("moves/move-1-0-0.json");
    std::fs::create_dir_all(earlier.parent().unwrap()).unwrap();
    std::fs::write(&earlier, "not a record").unwrap();
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    drop(work.run());
    let r = s.execute_fn("photo.moveToFolder", |s| Ok(json!(finish.abandon(s)))).unwrap();
    assert_eq!(r["relinked"], 1, "{r}");
    assert_eq!(r["problems"], json!([]), "every file of this move was followed: {r}");
    assert_eq!(std::fs::read_to_string(&earlier).unwrap(), "not a record", "left as it is");
}

#[test]
fn a_file_with_the_same_bytes_that_is_another_photo_of_the_library_is_never_removed_as_a_copy() {
    let dir = Scratch::new("copy-is-a-photo");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "same");
    let b = add(&mut s, &dir.path("share/a.jpg"), "same");
    // a record whose move was refused at the first file (the name was taken) and then cut short
    crate::move_intent::write(&dir.library(), &[(dir.path("local/a.jpg"), dir.path("share/a.jpg"))]).unwrap();
    let r = s.recover_moves();
    assert_eq!(r.copies_removed, 0, "{r:?}");
    assert!(matches!(r.problems.as_slice(), [crate::move_intent::MoveProblem::BothPlaces { .. }]), "{r:?}");
    assert!(Path::new(&dir.path("share/a.jpg")).is_file(), "another photo's file stays");
    assert_eq!((file_of(&s, a), file_of(&s, b)), (dir.path("local/a.jpg"), dir.path("share/a.jpg")));
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
    assert!(dir.records().is_empty(), "no record for a move that never began");
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

/// The disk, checking that the move's record is in the library before the first file moves.
struct Recorded<'a> {
    dir: &'a Scratch,
    /// What the record names (a file name).
    needle: &'static str,
    seen: Cell<bool>,
}

impl MoveFs for Recorded<'_> {
    fn exists(&self, p: &Path) -> bool {
        RealFs.exists(p)
    }
    fn is_file(&self, p: &Path) -> bool {
        RealFs.is_file(p)
    }
    fn same_file(&self, a: &Path, b: &Path) -> bool {
        RealFs.same_file(a, b)
    }
    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        if !self.seen.get() {
            let records = self.dir.records();
            assert_eq!(records.len(), 1, "the record is written before the first file moves");
            let text = std::fs::read_to_string(&records[0]).unwrap();
            assert!(text.contains(self.needle), "{text}");
            self.seen.set(true);
        }
        RealFs.rename_no_replace(a, b)
    }
    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        RealFs.copy_no_replace(a, b)
    }
    fn remove_file(&self, p: &Path) -> std::io::Result<()> {
        RealFs.remove_file(p)
    }
    fn stem_sibling(&self, sidecar: &Path) -> Option<String> {
        RealFs.stem_sibling(sidecar)
    }
}

#[test]
fn the_record_is_on_disk_before_the_first_file_moves_and_gone_once_saved() {
    let dir = Scratch::new("record");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    let fs = Recorded { dir: &dir, needle: "share", seen: Cell::new(false) };
    let out = work.run_with(&fs);
    assert!(fs.seen.get());
    s.execute_fn("photo.moveToFolder", |s| finish.finish(s, out)).unwrap();
    assert!(dir.records().is_empty(), "{:?}", dir.records());
}

#[test]
fn a_rename_and_its_undo_note_their_files_before_the_first_moves() {
    let dir = Scratch::new("rename-record");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let plan = crate::rename::RenamePlan {
        id: a.0,
        from: "a.jpg".into(),
        to: "b.jpg".into(),
        from_path: Some(dir.path("local/a.jpg")),
        to_path: Some(dir.path("local/b.jpg")),
    };
    let fs = Recorded { dir: &dir, needle: "b.jpg", seen: Cell::new(false) };
    s.execute_fn("photo.rename", |s| Ok(json!({"renamed": s.apply_rename_with(&fs, &[plan])?}))).unwrap();
    assert!(fs.seen.get(), "the rename moved a file with no record");
    assert!(dir.records().is_empty(), "{:?}", dir.records());
    // the Undo moves the file back, noted the same way
    let fs = Recorded { dir: &dir, needle: "a.jpg", seen: Cell::new(false) };
    s.execute_fn("edit.undo", |s| Ok(json!({"label": s.undo_step_with(&fs)?}))).unwrap();
    assert!(fs.seen.get(), "the Undo moved a file with no record");
    assert_eq!(file_of(&s, a), dir.path("local/a.jpg"));
    assert!(dir.records().is_empty(), "{:?}", dir.records());
}

#[test]
fn a_move_cut_short_is_followed_when_the_library_opens_again() {
    let dir = Scratch::new("crash");
    let (a, b, c, d, e, h);
    {
        let mut s = open(&dir);
        a = add(&mut s, &dir.path("local/a.jpg"), "A");
        std::fs::write(dir.path("local/a.xmp"), "<a-edits/>").unwrap();
        b = add(&mut s, &dir.path("local/b.jpg"), "B");
        c = add(&mut s, &dir.path("local/c.jpg"), "C");
        d = add(&mut s, &dir.path("local/d.jpg"), "D");
        std::fs::create_dir_all(dir.path("usb")).unwrap();
        e = add(&mut s, &dir.path("usb/e.jpg"), "E");
        h = add(&mut s, &dir.path("local/h.jpg"), "H");
        let mut moves: Vec<(String, String)> =
            ["a", "b", "c", "d"].iter().map(|f| (dir.path(&format!("local/{f}.jpg")), dir.path(&format!("share/{f}.jpg")))).collect();
        moves.push((dir.path("usb/e.jpg"), dir.path("share/e.jpg")));
        moves.push((dir.path("local/h.jpg"), dir.path("share/h.jpg")));
        crate::move_intent::write(&dir.library(), &moves).unwrap();
        // killed part-way: a's file arrived and its sidecar didn't; b's copy arrived and the
        // original wasn't removed yet; c never started; d's disk isn't connected now; e's copy
        // arrived (maybe never checked) and its drive is unplugged now
        std::fs::rename(dir.path("local/a.jpg"), dir.path("share/a.jpg")).unwrap();
        std::fs::copy(dir.path("local/b.jpg"), dir.path("share/b.jpg")).unwrap();
        std::fs::write(dir.path("share/.b.jpg.lc-partial-1-0"), "cut off").unwrap();
        std::fs::remove_file(dir.path("local/d.jpg")).unwrap();
        std::fs::copy(dir.path("usb/e.jpg"), dir.path("share/e.jpg")).unwrap();
        std::fs::rename(dir.path("usb"), dir.path("usb-unplugged")).unwrap();
        // h: cut between linking the new name and unlinking the old (same volume)
        std::fs::hard_link(dir.path("local/h.jpg"), dir.path("share/h.jpg")).unwrap();
        // (the app is gone: no finish, no save of a move)
    }
    let mut s = open(&dir);
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"), "the library follows the file that arrived");
    assert_eq!(std::fs::read_to_string(dir.path("share/a.xmp")).unwrap(), "<a-edits/>", "and its sidecar follows it");
    assert!(!Path::new(&dir.path("local/a.xmp")).exists());
    assert_eq!(file_of(&s, b), dir.path("local/b.jpg"), "the original stays where the library has it");
    assert!(!Path::new(&dir.path("share/b.jpg")).exists(), "our copy of it is removed");
    assert!(!Path::new(&dir.path("share/.b.jpg.lc-partial-1-0")).exists(), "a partial copy is removed");
    assert_eq!(file_of(&s, c), dir.path("local/c.jpg"));
    assert_eq!(file_of(&s, d), dir.path("local/d.jpg"));
    assert_eq!(file_of(&s, e), dir.path("usb/e.jpg"), "a source whose folder is gone may only be unplugged: not followed");
    assert_eq!(file_of(&s, h), dir.path("local/h.jpg"));
    assert!(Path::new(&dir.path("local/h.jpg")).is_file() && !Path::new(&dir.path("share/h.jpg")).exists(), "the second name goes, the file stays");
    assert!(s.take_library_warnings().is_empty(), "a move cut short isn't a settings problem");
    let info = s.execute("library.info", &json!({})).unwrap();
    assert_eq!(info["recoveredMoves"]["relinked"], 1, "{info}");
    let recovered = s.take_recovered_moves().unwrap();
    assert_eq!((recovered.relinked, recovered.waiting), (1, 2), "{recovered:?}");
    assert!(s.take_recovered_moves().is_none(), "handed out once");
    let notice = recovered.notice().unwrap();
    assert!(notice.contains("1 photo was being moved") && notice.contains("2 files that were being moved"), "{notice}");
    // d's and e's moves are kept for the next open; the rest of the record is done
    let records = dir.records();
    assert_eq!(records.len(), 1);
    let text = std::fs::read_to_string(&records[0]).unwrap();
    assert!(text.contains("d.jpg") && text.contains("e.jpg") && !text.contains("a.jpg"), "{text}");
    // the relink is one undo step that moves the file back
    assert_eq!(s.undo.last().unwrap().label, "Finish Moving 1 Photo");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(file_of(&s, a), dir.path("local/a.jpg"));
    assert!(Path::new(&dir.path("local/a.jpg")).is_file());
    drop(s);
    // and the relink was saved: it survives the next open, which has nothing more to do for a
    let s = open(&dir);
    assert_eq!(file_of(&s, a), dir.path("local/a.jpg"));
}

#[test]
fn edits_held_back_during_a_move_cut_short_reach_the_sidecar_at_the_next_open() {
    let dir = Scratch::new("held-restart");
    let (a, b);
    {
        let mut s = open(&dir);
        s.xmp.auto_write = true;
        s.save_prefs().unwrap();
        a = add(&mut s, &dir.path("local/a.jpg"), "photo");
        // b moves too, untouched: its sidecar is another app's and must stay as it is
        b = add(&mut s, &dir.path("local/b.jpg"), "other");
        std::fs::write(dir.path("local/b.xmp"), "<b-other-app/>").unwrap();
        s.selection = crate::Selection::single(a);
        s.execute("photo.rate", &json!({"ids": [a.0], "rating": 1})).unwrap();
        let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0, b.0], "folder": dir.path("share")})).unwrap().unwrap();
        s.execute("photo.rate", &json!({"ids": [a.0], "rating": 5})).unwrap();
        assert!(s.held_sidecars.contains(&a), "the write waits for the file to arrive");
        drop(work.run());
        // the app is gone after the file moved and before the move finished
        drop(finish);
    }
    let mut s = open(&dir);
    s.selection = crate::Selection::single(a);
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"));
    assert_eq!(s.catalog.photo(a).unwrap().rating, 5);
    let xmp = std::fs::read_to_string(dir.path("share/a.xmp")).unwrap();
    assert!(xmp.contains("xmp:Rating=\"5\"") || xmp.contains("<xmp:Rating>5</xmp:Rating>"), "{xmp}");
    // reading the sidecar back keeps the edit
    s.execute("photo.readMetadataFromFile", &json!({"ids": [a.0]})).unwrap();
    assert_eq!(s.catalog.photo(a).unwrap().rating, 5);
    assert_eq!(file_of(&s, b), dir.path("share/b.jpg"));
    assert_eq!(std::fs::read_to_string(dir.path("share/b.xmp")).unwrap(), "<b-other-app/>", "only held writes are made");
    assert!(!dir.library().join("moves/held-sidecars").exists(), "the list goes once its writes are made");
}

#[test]
fn a_held_write_whose_photo_is_out_of_reach_waits_for_the_open_that_reaches_it() {
    let dir = Scratch::new("held-waiting");
    std::fs::create_dir_all(dir.path("usb")).unwrap();
    let a;
    {
        let mut s = open(&dir);
        s.xmp.auto_write = true;
        s.save_prefs().unwrap();
        a = add(&mut s, &dir.path("usb/a.jpg"), "photo");
        s.selection = crate::Selection::single(a);
        let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
        s.execute("photo.rate", &json!({"ids": [a.0], "rating": 5})).unwrap();
        crate::move_intent::write(&dir.library(), &[(dir.path("usb/a.jpg"), dir.path("share/a.jpg"))]).unwrap();
        // the app is gone before the first file moved, and the disk is unplugged after
        drop((work, finish));
    }
    std::fs::rename(dir.path("usb"), dir.path("usb-unplugged")).unwrap();
    let held = dir.library().join("moves/held-sidecars");
    {
        let s = open(&dir);
        assert_eq!(file_of(&s, a), dir.path("usb/a.jpg"), "the move waits");
        assert!(held.is_file(), "the write it couldn't make stays listed");
    }
    std::fs::rename(dir.path("usb-unplugged"), dir.path("usb")).unwrap();
    let s = open(&dir);
    assert_eq!(file_of(&s, a), dir.path("usb/a.jpg"), "the file never left");
    let xmp = std::fs::read_to_string(dir.path("usb/a.xmp")).unwrap();
    assert!(xmp.contains("xmp:Rating=\"5\"") || xmp.contains("<xmp:Rating>5</xmp:Rating>"), "{xmp}");
    assert!(!held.exists(), "made at last, and the list goes");
}

#[test]
fn a_listed_write_made_later_in_the_session_leaves_the_list() {
    let dir = Scratch::new("held-late");
    let mut s = open(&dir);
    s.xmp.auto_write = true;
    let a = add(&mut s, &dir.path("local/a.jpg"), "photo");
    // a held write that failed when its move ended (the share dropped out): listed, waiting
    s.held_sidecars.insert(a);
    s.save_held_sidecars();
    s.held_sidecars.clear();
    s.unsaved_sidecars.insert(a);
    s.save_held_sidecars();
    let held = dir.library().join("moves/held-sidecars");
    assert!(held.is_file());
    // the next edit writes it: the next open mustn't write it again over what's there by then
    s.selection = crate::Selection::single(a);
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 3})).unwrap();
    assert_eq!(s.unsaved_sidecars(), 0);
    assert!(!held.exists());
}

/// The disk, refusing to move sidecars (a lock another app holds on them).
struct SidecarsStuck;

impl MoveFs for SidecarsStuck {
    fn exists(&self, p: &Path) -> bool {
        RealFs.exists(p)
    }
    fn is_file(&self, p: &Path) -> bool {
        RealFs.is_file(p)
    }
    fn same_file(&self, a: &Path, b: &Path) -> bool {
        RealFs.same_file(a, b)
    }
    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        if a.extension().is_some_and(|x| x == "xmp") {
            return Err(std::io::Error::other("in use"));
        }
        RealFs.rename_no_replace(a, b)
    }
    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        RealFs.copy_no_replace(a, b)
    }
    fn remove_file(&self, p: &Path) -> std::io::Result<()> {
        RealFs.remove_file(p)
    }
    fn stem_sibling(&self, sidecar: &Path) -> Option<String> {
        RealFs.stem_sibling(sidecar)
    }
}

#[test]
fn a_sidecar_that_could_not_follow_its_file_is_taken_along_the_next_time() {
    let dir = Scratch::new("sidecar-stuck");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    std::fs::write(dir.path("local/a.xmp"), "<a-edits/>").unwrap();
    crate::move_intent::write(&dir.library(), &[(dir.path("local/a.jpg"), dir.path("share/a.jpg"))]).unwrap();
    // cut short after the file moved and before its sidecar did
    std::fs::rename(dir.path("local/a.jpg"), dir.path("share/a.jpg")).unwrap();
    let r = s.recover_moves_with(&SidecarsStuck, true);
    assert_eq!(r.relinked, 1);
    assert!(matches!(r.problems.as_slice(), [crate::move_intent::MoveProblem::SidecarStayed { .. }]), "{r:?}");
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"), "the photo follows its file");
    assert_eq!(dir.records().len(), 1, "the record keeps the move for its sidecar");
    // the next time, the sidecar follows; the photo isn't relinked again
    let r = s.recover_moves_with(&RealFs, true);
    assert_eq!((r.relinked, r.problems.len()), (0, 0), "{r:?}");
    assert_eq!(std::fs::read_to_string(dir.path("share/a.xmp")).unwrap(), "<a-edits/>");
    assert!(!Path::new(&dir.path("local/a.xmp")).exists());
    assert!(dir.records().is_empty());
}

#[test]
fn a_library_open_without_its_lock_leaves_every_record_and_file_as_it_is() {
    let dir = Scratch::new("unlocked");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let b = add(&mut s, &dir.path("local/b.jpg"), "B");
    let record = [(dir.path("local/a.jpg"), dir.path("share/a.jpg")), (dir.path("local/b.jpg"), dir.path("share/b.jpg"))];
    crate::move_intent::write(&dir.library(), &record).unwrap();
    // another program may be in the middle of this move: a's file has arrived, b's copy is being made
    std::fs::rename(dir.path("local/a.jpg"), dir.path("share/a.jpg")).unwrap();
    std::fs::copy(dir.path("local/b.jpg"), dir.path("share/b.jpg")).unwrap();
    std::fs::write(dir.path("share/.b.jpg.lc-partial-1-0"), "being written").unwrap();
    std::fs::write(dir.library().join("moves/x.tmp"), "being written").unwrap();
    let before = std::fs::read(&dir.records()[0]).unwrap();
    let r = s.recover_moves_with(&RealFs, false);
    assert!(matches!(r.problems.as_slice(), [crate::move_intent::MoveProblem::Unprotected]), "{r:?}");
    assert_eq!((r.relinked, r.copies_removed), (0, 0), "{r:?}");
    assert!(Path::new(&dir.path("share/b.jpg")).is_file() && Path::new(&dir.path("local/b.jpg")).is_file());
    assert!(Path::new(&dir.path("share/.b.jpg.lc-partial-1-0")).exists(), "a partial file may be being written");
    assert!(dir.library().join("moves/x.tmp").exists(), "a record being written may be another program's");
    assert_eq!((file_of(&s, a), file_of(&s, b)), (dir.path("local/a.jpg"), dir.path("local/b.jpg")), "nothing is followed");
    assert_eq!(std::fs::read(&dir.records()[0]).unwrap(), before, "the record is as it was");
    // with the lock, the same state is followed and settled
    let r = s.recover_moves_with(&RealFs, true);
    assert_eq!((r.relinked, r.copies_removed), (1, 1), "{r:?}");
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"));
    assert!(!Path::new(&dir.path("share/b.jpg")).exists());
    assert!(dir.records().is_empty());
}

#[test]
fn a_library_that_cannot_be_locked_leaves_the_move_for_an_open_that_can() {
    let dir = Scratch::new("unlockable");
    let a;
    {
        let mut s = open(&dir);
        a = add(&mut s, &dir.path("local/a.jpg"), "A");
        crate::move_intent::write(&dir.library(), &[(dir.path("local/a.jpg"), dir.path("share/a.jpg"))]).unwrap();
        s.held_sidecars.insert(a);
        s.save_held_sidecars();
        // the app is killed after the file arrived
        std::fs::rename(dir.path("local/a.jpg"), dir.path("share/a.jpg")).unwrap();
    }
    // a library whose lock file can't be locked (a directory stands in for a file system without locks)
    let lock = dir.library().join("catalog.lock");
    let _ = std::fs::remove_file(&lock);
    std::fs::create_dir_all(&lock).unwrap();
    {
        let mut s = open(&dir);
        let r = s.take_recovered_moves().expect("the open says it left the move alone");
        assert!(matches!(r.problems.as_slice(), [crate::move_intent::MoveProblem::Unprotected]), "{r:?}");
        assert_eq!(file_of(&s, a), dir.path("local/a.jpg"), "nothing is followed without the lock");
        s.save_held_sidecars();
        assert!(dir.library().join("moves/held-sidecars").is_file(), "the edits the earlier run held back wait for an open with the lock");
        assert_eq!(dir.records().len(), 1);
    }
    std::fs::remove_dir_all(&lock).unwrap();
    let s = open(&dir);
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"), "the next open with the lock follows it");
    assert!(dir.records().is_empty());
}

/// Make the library unlockable (a directory stands in for a lock file) before it is opened.
fn make_unlockable(dir: &Scratch) {
    let lock = dir.library().join("catalog.lock");
    let _ = std::fs::remove_file(&lock);
    std::fs::create_dir_all(&lock).unwrap();
}

#[test]
fn without_the_lock_only_the_writes_this_session_listed_leave_the_held_list() {
    let dir = Scratch::new("unlocked-held");
    let (a, b);
    {
        let mut s = open(&dir);
        a = add(&mut s, &dir.path("local/a.jpg"), "A");
        b = add(&mut s, &dir.path("local/b.jpg"), "B");
    }
    make_unlockable(&dir);
    let held = dir.library().join("moves/held-sidecars");
    // an earlier run (or another program) left b's write on the list
    std::fs::create_dir_all(dir.library().join("moves")).unwrap();
    std::fs::write(&held, serde_json::to_vec(&[b]).unwrap()).unwrap();
    let mut s = open(&dir);
    s.held_sidecars.insert(a);
    s.save_held_sidecars();
    let listed = |p: &Path| -> Vec<PhotoId> { serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap() };
    assert_eq!(listed(&held), vec![a, b]);
    // the move ends and a's write is made: it leaves the list, b's entry isn't this session's to take back
    s.held_sidecars.clear();
    s.save_held_sidecars();
    assert_eq!(listed(&held), vec![b], "the entry another run left stays for an open with the lock");
}

#[test]
fn without_the_lock_a_worker_that_dies_still_has_its_own_move_followed() {
    let dir = Scratch::new("unlocked-abandon");
    let a;
    {
        let mut s = open(&dir);
        a = add(&mut s, &dir.path("local/a.jpg"), "A");
        add(&mut s, &dir.path("local/b.jpg"), "B");
    }
    make_unlockable(&dir);
    // another program's record, which stays untouched
    let other = crate::move_intent::write(&dir.library(), &[(dir.path("local/b.jpg"), dir.path("share/b.jpg"))]).unwrap();
    let mut s = open(&dir);
    let taken = s.take_recovered_moves().expect("the open leaves the other record alone");
    assert!(matches!(taken.problems.as_slice(), [crate::move_intent::MoveProblem::Unprotected]), "{taken:?}");
    let before = std::fs::read(other.path()).unwrap();
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    // the files moved, then the worker died before saying so
    drop(work.run());
    let r = s.execute_fn("photo.moveToFolder", |s| Ok(json!(finish.abandon(s)))).unwrap();
    assert_eq!(r["relinked"], 1, "{r}");
    // (the other program's record was reported when the library opened; every file this process moved was followed)
    assert_eq!(r["problems"], json!([]), "no `unprotected` when only this worker's own record was followed: {r}");
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"), "the library follows the file this process moved");
    assert_eq!(dir.records().len(), 1, "only the other program's record is left");
    assert_eq!(std::fs::read(other.path()).unwrap(), before);
}

#[test]
fn a_list_that_could_not_be_written_is_not_counted_as_listed() {
    let dir = Scratch::new("unlocked-held-fail");
    let a;
    {
        let mut s = open(&dir);
        a = add(&mut s, &dir.path("local/a.jpg"), "A");
    }
    make_unlockable(&dir);
    let mut s = open(&dir);
    // the list can't be written (a folder stands in its place)
    std::fs::create_dir_all(dir.library().join("moves/held-sidecars")).unwrap();
    s.held_sidecars.insert(a);
    s.save_held_sidecars();
    assert!(!s.held_listed.contains(&a), "the disk doesn't name it, so a later save mustn't treat a stray entry as this session's");
}

#[test]
fn without_the_lock_a_dead_workers_cleanup_leaves_the_copies_and_partials_another_program_may_own() {
    let dir = Scratch::new("unlocked-abandon-both");
    let a;
    {
        let mut s = open(&dir);
        a = add(&mut s, &dir.path("local/a.jpg"), "A");
    }
    make_unlockable(&dir);
    let mut s = open(&dir);
    let _ = s.take_recovered_moves();
    let (work, finish) = crate::move_job::start(&mut s, &json!({"ids": [a.0], "folder": dir.path("share")})).unwrap().unwrap();
    drop(work.run());
    // another program is moving the same photo to the same folder: its copy is at both places, its partial is being written
    std::fs::copy(dir.path("share/a.jpg"), dir.path("local/a.jpg")).unwrap();
    std::fs::write(dir.path("share/.a.jpg.lc-partial-1-0"), "being written").unwrap();
    let r = s.execute_fn("photo.moveToFolder", |s| Ok(json!(finish.abandon(s)))).unwrap();
    assert_eq!(r["copiesRemoved"], 0, "{r}");
    assert!(Path::new(&dir.path("share/a.jpg")).is_file() && Path::new(&dir.path("local/a.jpg")).is_file(), "{r}");
    assert!(Path::new(&dir.path("share/.a.jpg.lc-partial-1-0")).exists(), "another process's partial copy stays");
}

#[test]
fn the_records_of_one_session_are_followed_when_the_library_is_opened_again() {
    let dir = Scratch::new("reopen");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let record = crate::move_intent::write(&dir.library(), &[(dir.path("local/a.jpg"), dir.path("share/a.jpg"))]).unwrap();
    std::fs::rename(dir.path("local/a.jpg"), dir.path("share/a.jpg")).unwrap();
    // the move ended, but the library didn't follow (or its change isn't saved): the record waits for the save
    s.settle_moves(Some(record));
    s.open_library(dir.library(), false).unwrap();
    assert_eq!(file_of(&s, a), dir.path("share/a.jpg"), "the open follows a record the session had not seen saved");
    assert!(dir.records().is_empty());
}

#[test]
fn a_settled_record_stays_while_the_library_has_changes_not_yet_saved() {
    let dir = Scratch::new("settled-unsaved");
    let mut s = open(&dir);
    add(&mut s, &dir.path("local/a.jpg"), "A");
    let record = crate::move_intent::write(&dir.library(), &[(dir.path("local/a.jpg"), dir.path("share/a.jpg"))]).unwrap();
    s.settle_moves(Some(record));
    // a save that is failing leaves changes queued: the record must outlive them
    s.pending_log.push(Op::Batch { ops: Vec::new() });
    s.clear_settled_moves();
    assert_eq!(dir.records().len(), 1, "the record goes only once the library has saved what follows the move");
    s.pending_log.clear();
    s.clear_settled_moves();
    assert!(dir.records().is_empty(), "{:?}", dir.records());
}

#[test]
fn the_library_says_whether_it_followed_the_files_that_could_not_go_back() {
    let dir = Scratch::new("unfollowed");
    let mut s = open(&dir);
    let a = add(&mut s, &dir.path("local/a.jpg"), "A");
    let (from, to) = (dir.path("local/a.jpg"), dir.path("share/a.jpg"));
    let stuck = || crate::rename::BatchMoveError { message: "stopped".into(), stuck: vec![(from.clone(), to.clone())], pending_sidecars: Vec::new() };
    let set_file = |id| Op::SetFile { id, file_name: "a.jpg".into(), source: Source::File { path: to.clone() } };
    // the photo is gone: the library can't point at the file, which stays at the new place (the record stays)
    let (message, _, followed) = s.follow_stuck(&set_file(PhotoId(9999)), stuck(), false);
    assert!(!followed, "{message}");
    assert!(message.contains("the library could not follow"), "{message}");
    let (_, _, followed) = s.follow_stuck(&set_file(a), stuck(), false);
    assert!(followed);
    assert_eq!(file_of(&s, a), to);
}

#[test]
fn removed_copies_are_told() {
    let r = crate::move_intent::Recovered { copies_removed: 2, ..Default::default() };
    assert!(!r.is_empty());
    assert!(r.notice().unwrap().contains("2 copies of files that were being moved were removed"), "{:?}", r.notice());
}
