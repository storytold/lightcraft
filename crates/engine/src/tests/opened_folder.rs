//! Issue #3240: an open library writes only to the folder it opened. When its path leads to another
//! folder (a share mounted over it, or the folder moved away and another put in its place), saving
//! stops with the reason, nothing is written there, and the changes are kept until the folder that
//! was opened is back. A folder's identity is its device and inode number, so these run on Unix.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lightcraft_catalog::{LibraryLock, OpenedFolder};
use serde_json::json;

use crate::{EngineError, Session};

/// What the reason for a refused save says.
const REFUSED: &str = "is no longer the folder this library was opened from";

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-opened-folder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// What `dir` holds, by name: a file's bytes (a folder is listed, empty).
fn files_in(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let bytes = if e.path().is_dir() { Vec::new() } else { std::fs::read(e.path()).unwrap() };
            (e.file_name().to_string_lossy().into_owned(), bytes)
        })
        .collect();
    files.sort();
    files
}

/// Another library's folder where `lib` was, as a share mounted over it would look: the folder that
/// was opened is moved to the path returned, out of the path's reach.
fn put_another_library_at(lib: &Path) -> PathBuf {
    let away = lib.with_extension("away");
    std::fs::rename(lib, &away).unwrap();
    std::fs::create_dir(lib).unwrap();
    let theirs: [(&str, &[u8]); 4] =
        [("catalog.snap", b"their snapshot"), ("catalog.log", b"their log\n"), ("catalog.lock.owner", b"their note"), ("presets.json", b"{}")];
    for (name, bytes) in theirs {
        std::fs::write(lib.join(name), bytes).unwrap();
    }
    away
}

/// A copy of `lib`'s files (the lock's note included) in a folder of its own beside it, as a folder
/// copied while the library was open would be.
fn copy_of(lib: &Path) -> PathBuf {
    let copy = lib.with_extension("copy");
    std::fs::create_dir(&copy).unwrap();
    for e in std::fs::read_dir(lib).unwrap().map(Result::unwrap).filter(|e| e.path().is_file()) {
        std::fs::copy(e.path(), copy.join(e.file_name())).unwrap();
    }
    copy
}

/// `copy` where `lib` was, as a copy put in its place would be: the folder that was opened is
/// moved to the path returned.
fn swap_in(lib: &Path, copy: &Path) -> PathBuf {
    let away = lib.with_extension("away");
    std::fs::rename(lib, &away).unwrap();
    std::fs::rename(copy, lib).unwrap();
    away
}

/// A copy of `lib`'s files (the lock's note included) where `lib` was: the folder that was opened
/// is moved to the path returned.
fn put_a_copy_at(lib: &Path) -> PathBuf {
    swap_in(lib, &copy_of(lib))
}

/// What a crash now would leave: the catalog files at `lib`, copied and opened elsewhere.
fn after_a_crash(lib: &Path) -> Session {
    let crashed = lib.with_extension("crashed");
    std::fs::create_dir_all(&crashed).unwrap();
    for name in ["catalog.snap", "catalog.log"] {
        if lib.join(name).exists() {
            std::fs::copy(lib.join(name), crashed.join(name)).unwrap();
        }
    }
    let mut s = Session::new();
    s.open_library(&crashed, false).unwrap();
    s
}

/// The demo library at `lib`, open, with one change (a rating of 2) already on disk.
fn open_with_a_saved_change(lib: &Path) -> Session {
    let mut s = Session::new();
    s.open_library(lib, true).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    s.flush_log().unwrap();
    assert_eq!(s.unsaved(), None);
    s
}

/// The acceptance case: the folder at the path is replaced while the library is open. The next
/// save is refused with the reason, the new folder (and the owner note in it) is untouched, and so
/// is the folder that was opened: the session stops saving.
#[test]
fn a_folder_that_replaced_the_open_library_is_not_written_to() {
    let base = scratch("replaced");
    let lib = base.join("Lib");
    let mut s = open_with_a_saved_change(&lib);
    let id = s.selection.active.unwrap();

    let away = put_another_library_at(&lib);
    let (theirs, ours) = (files_in(&lib), files_in(&away));

    // two changes after the swap, then quitting: the first is applied in memory and handed to the log's
    // thread, which can't write it where the path leads now; the second is refused as it is made
    let _ = s.execute("photo.rate", &json!({"rating": 5}));
    let applied = s.catalog.photo(id).unwrap().rating;
    let flushed = s.flush_log();
    let held = s.unsaved().map(|(n, why)| (n, why.to_owned()));
    let second = s.execute("photo.rate", &json!({"rating": 4}));
    let closed = s.close_library();

    // what matters most: nothing was written, into either folder
    assert_eq!(files_in(&lib), theirs, "nothing is written into the folder at the path now, its owner note included");
    assert_eq!(files_in(&away), ours, "nor into the folder that was opened: saving stops");

    // and each step said why
    assert_eq!(applied, 5, "the change is applied");
    let e = flushed.expect_err("the folder at the path isn't the one that was opened");
    assert!(e.to_string().contains(REFUSED), "{e}");
    let (n, why) = held.expect("saved in memory only");
    assert!(n >= 1 && why.contains(REFUSED) && why.contains(&lib.display().to_string()), "{n}: {why}");
    let e = second.expect_err("not saved");
    assert!(matches!(e, EngineError::NotSaved(_)) && e.to_string().contains(REFUSED), "{e:?}");
    let e = closed.expect_err("nowhere to write");
    assert!(e.to_string().contains(REFUSED), "{e}");

    drop(s);
    assert_eq!(files_in(&lib), theirs, "not even when the session ends: its owner note stays");
    assert_eq!(files_in(&away), ours);
    let _ = std::fs::remove_dir_all(&base);
}

/// The inverse: with the folder that was opened back at the path, what was held and what was
/// queued is written into it, and the library reopens with both.
#[test]
fn saving_goes_on_when_the_folder_that_was_opened_is_back() {
    let base = scratch("back");
    let lib = base.join("Lib");
    let mut s = open_with_a_saved_change(&lib);
    let id = s.selection.active.unwrap();

    let away = put_another_library_at(&lib);
    let _ = s.execute("photo.rate", &json!({"rating": 5}));
    s.flush_log().expect_err("refused");
    s.execute("photo.rate", &json!({"rating": 4})).expect_err("refused");
    assert!(s.unsaved().is_some());

    std::fs::rename(&lib, base.join("Lib.theirs")).unwrap();
    std::fs::rename(&away, &lib).unwrap();
    s.flush_log().expect("what the log's thread held is written");
    s.persist().expect("and what was queued");
    s.flush_log().unwrap();
    assert_eq!(s.unsaved(), None);
    s.close_library().unwrap();
    drop(s);

    let mut again = Session::new();
    again.open_library(&lib, true).unwrap();
    assert_eq!(again.catalog.photo(id).unwrap().rating, 4, "both changes were saved, in order");
    let _ = std::fs::remove_dir_all(&base);
}

/// Reopening the path of a replaced folder doesn't carry the old lock over: it is on the folder
/// that was opened, so the folder at the path now gets its own, and is refused while another
/// program has it open.
#[test]
fn reopening_the_path_of_a_replaced_folder_takes_that_folders_lock() {
    let base = scratch("relock");
    let lib = base.join("Lib");
    let mut s = open_with_a_saved_change(&lib);

    let _away = put_another_library_at(&lib);
    let other_program = LibraryLock::acquire(&lib, "other").unwrap();
    let e = s.open_library(&lib, false).expect_err("the folder at the path is in use");
    assert!(matches!(e, EngineError::LibraryInUse(_)), "{e:?}");
    assert!(s.library.is_some(), "the library that was open stays open");
    drop(other_program);
    let _ = std::fs::remove_dir_all(&base);
}

/// The thumbnail cache is bound to the folder too: a thumbnail finished after the swap is kept in
/// memory, and no `thumbs` folder is made in the folder at the path now.
#[test]
fn thumbnails_are_not_cached_into_a_folder_that_replaced_the_open_library() {
    let base = scratch("thumbs");
    let lib = base.join("Lib");
    let s = open_with_a_saved_change(&lib);
    let _away = put_another_library_at(&lib);
    let theirs = files_in(&lib);

    let key = lightcraft_preview::hash_bytes(b"a thumbnail");
    s.media.rendered.put(key, Arc::new(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[10, 20, 30, 255]; 16] }));
    assert!(s.media.rendered.get(key).is_some(), "kept in memory");
    assert_eq!(files_in(&lib), theirs, "no thumbnail folder is made in the folder at the path now");
    let _ = std::fs::remove_dir_all(&base);
}

/// A Move import takes the originals off the card only once the library is on disk: the catalog
/// log's thread is handed the records at once and looks at the folder when it writes them, so the
/// move waits for that. With the folder replaced the originals stay (the session holds the records
/// of the copies, which are not on disk).
#[test]
fn a_move_import_keeps_its_originals_while_the_library_cannot_be_saved() {
    let base = scratch("move");
    let (lib, card, dest) = (base.join("Lib"), base.join("card"), base.join("dest"));
    let mut s = Session::new().with_fs();
    s.clock = Box::new(|| "2026-01-14T05:58:48".to_string());
    s.open_library(&lib, true).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    s.flush_log().unwrap();
    let _away = put_another_library_at(&lib);
    super::import_move::write_png(&card.join("a.png"), 1);

    let params = json!({"paths": [card.to_string_lossy()], "mode": "move", "destination": dest.to_string_lossy()});
    let r = s.execute("library.import", &params).unwrap();
    assert!(card.join("a.png").is_file(), "the original stays while the library that points at its copy is not on disk: {r}");
    assert_eq!(r["moved"].as_array().map_or(0, Vec::len), 0, "{r}");
    let reason = r["kept"][0]["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("could not be saved") && reason.contains(REFUSED), "{r}");
    let _ = std::fs::remove_dir_all(&base);
}

/// What tells the folder when its numbers can't is the note this session's lock wrote into it: a copy
/// of the library's folder (the note in it too) put where the opened one was counts as the opened
/// folder, so saving goes on and the closing snapshot is written into it. (The case this stands for
/// is a drive that numbers its folders anew each time it is mounted; a mount can't be made in a
/// test. A folder with no such note, or another one, never counts: the tests above.)
#[test]
fn a_folder_that_holds_the_note_of_the_open_library_counts_as_the_folder_that_was_opened() {
    let base = scratch("note");
    let lib = base.join("Lib");
    let mut s = open_with_a_saved_change(&lib);
    let id = s.selection.active.unwrap();
    assert!(lib.join("catalog.lock.owner").is_file(), "the lock wrote its note");

    let away = put_a_copy_at(&lib);
    let snapshot = std::fs::read(away.join("catalog.snap")).ok();
    s.execute("photo.rate", &json!({"rating": 5})).expect("the folder at the path holds this session's note: saving goes on");
    s.flush_log().expect("the log's thread is not refused either");
    assert_eq!(s.unsaved(), None);
    s.close_library().expect("the closing snapshot is written into the folder at the path");
    drop(s);
    assert_eq!(std::fs::read(away.join("catalog.snap")).ok(), snapshot, "the folder that was moved away keeps its snapshot");

    let mut again = Session::new();
    again.open_library(&lib, true).unwrap();
    assert_eq!(again.catalog.photo(id).unwrap().rating, 5, "the folder at the path has the change made after the swap");
    let _ = std::fs::remove_dir_all(&base);
}

/// A copy made while the library was open counts as it, but its log lacks what was written after the
/// copy was made: appending to it would leave a gap, and the file the log's thread had open is in the
/// folder that was moved away, so a change made after the swap would be saved nowhere the path leads
/// to. The thread refuses a log it didn't write and holds what it was handed, and the snapshot the
/// frame loop takes for held records (it holds every change) replaces the copy's log: a crash right
/// after has the change made before the swap and the one made after it.
#[test]
fn a_copy_that_lacks_changes_is_made_whole_by_a_snapshot() {
    let base = scratch("copy-gap");
    let lib = base.join("Lib");
    let mut s = open_with_a_saved_change(&lib);
    let id = s.selection.active.unwrap();

    let copy = copy_of(&lib);
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    s.flush_log().unwrap();
    let away = swap_in(&lib, &copy);
    let ours = files_in(&away);

    s.execute("photo.rate", &json!({"rating": 5})).unwrap();
    let _ = s.flush_log();
    s.execute("photo.rate", &json!({"rating": 4})).expect("a second change while the first is held is taken: only a snapshot heals the log");
    let t = std::time::Instant::now();
    while s.unsaved().is_some() || s.library.as_ref().unwrap().journal().snapshot_running() {
        assert!(t.elapsed() < std::time::Duration::from_secs(20), "never made whole: {:?}", s.unsaved());
        s.persist_if_dirty();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(files_in(&away), ours, "nothing was written into the folder that was moved away");
    assert_eq!(after_a_crash(&lib).catalog.photo(id).unwrap().rating, 4, "a crash now loses nothing");
    s.close_library().unwrap();
    drop(s);
    let mut again = Session::new();
    again.open_library(&lib, true).unwrap();
    assert_eq!(again.catalog.photo(id).unwrap().rating, 4);
    let _ = std::fs::remove_dir_all(&base);
}

/// The lock is taken in the folder the path leads to, a moment after the stores recorded the folder:
/// a path that came to lead to another folder in between (a share mounted as the library opens) is
/// refused, and the lock taken there is let go again with its note.
#[test]
fn a_lock_taken_in_another_folder_than_the_stores_recorded_is_refused() {
    let base = scratch("lock-race");
    let (recorded, now) = (base.join("Recorded"), base.join("Now"));
    std::fs::create_dir_all(&recorded).unwrap();
    std::fs::create_dir_all(&now).unwrap();
    let folder = OpenedFolder::at(&recorded).unwrap();

    let e = crate::library::lock_in(&now, &folder).expect_err("the path leads to another folder than the stores recorded");
    assert!(e.to_string().contains("changed while the library was being opened"), "{e}");
    assert!(!now.join("catalog.lock.owner").exists(), "the lock taken there is let go again, its note too");

    drop(crate::library::lock_in(&recorded, &folder).expect("the folder the stores recorded"));
    let _ = std::fs::remove_dir_all(&base);
}

/// Checking a path before the library open now is closed: the lock this session holds is on the
/// folder that was opened, so for the folder that replaced it the answer is what opening it would
/// say, that another program has it open, and nothing is closed first.
#[test]
fn checking_the_path_of_a_replaced_folder_finds_the_lock_of_the_folder_there_now() {
    let base = scratch("check");
    let lib = base.join("Lib");
    let s = open_with_a_saved_change(&lib);
    s.check_library(&lib).expect("the library that is open keeps its lock when it is opened again");

    let _away = put_another_library_at(&lib);
    let other_program = LibraryLock::acquire(&lib, "other").unwrap();
    let e = s.check_library(&lib).expect_err("another program has the folder at the path open");
    assert!(matches!(e, EngineError::LibraryInUse(_)), "{e:?}");
    drop(other_program);
    let _ = std::fs::remove_dir_all(&base);
}
