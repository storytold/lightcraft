//! `library.restore`: the library becomes what a backup holds, what it was is kept, and the
//! session goes on in the restored library.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use crate::{EngineError, Session};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-engine-restore-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn open(dir: &Path) -> Session {
    let mut s = Session::new();
    s.open_library(dir, true).unwrap();
    s
}

fn back_up(s: &mut Session) -> PathBuf {
    let r = s.execute("library.backup", &json!({})).unwrap();
    PathBuf::from(r["report"]["dir"].as_str().unwrap())
}

fn restore(s: &mut Session, made: &Path) -> Value {
    s.execute("library.restore", &json!({"dir": made.to_string_lossy()})).unwrap()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(dir).map(|d| d.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect()).unwrap_or_default();
    names.sort();
    names
}

/// Given a library that changed since it was backed up, when it is restored from the backup,
/// then the session shows the library as backed up and goes on working in it, and what the
/// library was is kept in `Replaced/<the time>`, which restores in turn.
#[test]
fn a_restored_library_is_the_backup_and_the_session_goes_on_in_it() {
    let dir = temp_dir("in-place");
    let mut s = open(&dir);
    // (not the photo a library opens on)
    let id = *s.visible_cloned().last().unwrap();
    assert_ne!(Some(id), s.selection.active);
    s.selection = crate::Selection { ids: vec![id], active: Some(id) };
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let made = back_up(&mut s);
    let then = s.catalog.to_snapshot();
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    s.execute("photo.flag", &json!({"flag": "pick"})).unwrap();
    // (not saved yet when the restore is asked for: it is kept all the same)
    s.commit("Rate", lightcraft_catalog::Op::SetRating { id, rating: 2 }).unwrap();
    let now = s.catalog.to_snapshot();

    let r = restore(&mut s, &made);
    assert_eq!(r["opened"], json!(true));
    assert_eq!(r["restored"]["library"].as_str(), Some(dir.to_string_lossy().as_ref()));
    assert_eq!(s.catalog.to_snapshot(), then);
    assert_eq!(s.catalog.photo(id).unwrap().rating, 4);
    assert!(s.undo.is_empty() && s.pending_log.is_empty());
    assert_eq!(s.selection.active, Some(id), "still on the photo it was on");
    assert!(s.activity.list().is_empty());
    let aside = PathBuf::from(r["restored"]["aside"].as_str().unwrap());
    assert_eq!(aside, dir.join("Replaced").join("2026-09-30 12.00.00 UTC"));
    // nothing of the restore is left in the folder, and the backup is as it was made
    let left = names(&dir);
    assert!(!left.iter().any(|n| n.starts_with("restore.")), "{left:?}");
    assert_eq!(s.execute("library.checkBackup", &json!({"dir": made.to_string_lossy()})).unwrap()["ok"], json!(true));

    // the session goes on in the restored library: changes are saved there
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    let after = s.catalog.to_snapshot();
    drop(s);
    let mut s = open(&dir);
    assert_eq!(s.catalog.to_snapshot(), after);
    assert_eq!(s.catalog.photo(id).unwrap().rating, 3);
    // what the library was opens as a library, with the change that was not yet saved
    let was = open(&aside);
    assert_eq!(was.catalog.to_snapshot(), now);
    assert_eq!(was.catalog.photo(id).unwrap().rating, 2);
    drop(was);
    // a second restore at the same time of day has a folder of its own for what it replaces
    let again = restore(&mut s, &made);
    assert_eq!(again["restored"]["aside"].as_str(), Some(dir.join("Replaced").join("2026-09-30 12.00.00 UTC (2)").to_string_lossy().as_ref()));
    assert_eq!(s.catalog.to_snapshot(), then);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A backup is not a folder to work in: opening it as a library is refused, and leaves it as it
/// was made (no lock file, nothing written).
#[test]
fn a_backup_is_not_opened_as_a_library() {
    let dir = temp_dir("not-opened");
    let mut s = open(&dir);
    let made = back_up(&mut s);
    let before = names(&made);
    let mut other = Session::new();
    let e = other.open_library(&made, false).unwrap_err();
    assert!(e.to_string().contains("backup") && e.to_string().contains("library.restore"), "{e}");
    assert!(other.library.is_none());
    assert_eq!(names(&made), before);
    // naming one of its files is naming it: also refused, the lock not taken, nothing written
    for file in [lightcraft_catalog::journal::SNAPSHOT, lightcraft_catalog::journal::LOG] {
        let e = other.open_library(made.join(file), false).unwrap_err();
        assert!(e.to_string().contains("backup") && e.to_string().contains("library.restore"), "{file}: {e}");
        let e = other.check_library(made.join(file)).unwrap_err();
        assert!(e.to_string().contains("backup"), "{file}: {e}");
        assert!(other.library.is_none());
        assert_eq!(names(&made), before, "{file}");
    }
    assert!(other.check_library(&made).is_err());
    assert_eq!(s.execute("library.checkBackup", &json!({"dir": made.to_string_lossy()})).unwrap()["ok"], json!(true));
    // nor is the open library replaced by it that way
    let e = s.open_library(&made, false).unwrap_err();
    assert!(e.to_string().contains("backup"), "{e}");
    assert_eq!(s.library.as_ref().map(|l| l.dir.clone()), Some(dir.clone()));
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// With `to`, a backup is restored into another folder, which then opens as the library that
/// was backed up; the open library is not touched. The settings files come along.
#[test]
fn a_backup_restores_into_another_folder() {
    let (dir, to) = (temp_dir("to"), temp_dir("to-there"));
    let mut s = open(&dir);
    let id = s.selection.active.unwrap();
    s.execute("photo.rate", &json!({"rating": 5})).unwrap();
    s.execute("library.backupPreferences", &json!({"dir": null})).unwrap();
    let made = back_up(&mut s);
    let then = s.catalog.to_snapshot();
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    let r = s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "to": to.to_string_lossy()})).unwrap();
    assert_eq!((r["opened"].as_bool(), r["restored"]["aside"].is_null()), (Some(false), true));
    assert_eq!(s.catalog.photo(id).unwrap().rating, 1, "the open library is as it was");
    assert_eq!(s.library.as_ref().map(|l| l.dir.clone()), Some(dir.clone()));
    assert!(to.join("prefs.json").is_file());
    let there = open(&to);
    assert_eq!(there.catalog.to_snapshot(), then);
    // a folder that is open elsewhere is not restored into
    let e = s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "to": to.to_string_lossy()})).unwrap_err();
    assert!(matches!(e, EngineError::LibraryInUse(_)), "{e}");
    drop(there);
    // and with no library open at all, `to` says where
    let mut none = Session::new();
    assert!(none.execute("library.restore", &json!({"dir": made.to_string_lossy()})).is_err());
    let elsewhere = temp_dir("to-elsewhere");
    none.execute("library.restore", &json!({"dir": made.to_string_lossy(), "to": elsewhere.to_string_lossy()})).unwrap();
    assert_eq!(open(&elsewhere).catalog.to_snapshot(), then);
    drop(s);
    for d in [dir, to, elsewhere] {
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// While a library is restored it has a row in the activity stack.
#[test]
fn a_restore_shows_in_the_activity_stack() {
    let dir = temp_dir("row");
    let mut s = open(&dir);
    let made = back_up(&mut s);
    s.restore_hold.store(true, Ordering::SeqCst);
    let (activity, hold) = (s.activity.clone(), s.restore_hold.clone());
    let watcher = std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let mut seen = Vec::new();
        while seen.is_empty() && t0.elapsed() < std::time::Duration::from_secs(20) {
            seen = activity.list();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        hold.store(false, Ordering::SeqCst);
        seen
    });
    restore(&mut s, &made);
    let rows = watcher.join().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].cancellable), ("restore", "Restoring the library", false));
    assert_eq!(rows[0].unit, crate::activity::Unit::Bytes);
    assert!(s.activity.list().is_empty());
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What can't be restored says why, and the library is as it was and goes on being saved.
#[test]
fn what_cannot_be_restored_leaves_the_library_as_it_was() {
    let dir = temp_dir("refused");
    let mut s = open(&dir);
    let id = s.selection.active.unwrap();
    let made = back_up(&mut s);
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    let now = s.catalog.to_snapshot();
    let before = names(&dir);
    let refused = |s: &mut Session, p: Value| s.execute("library.restore", &p).unwrap_err().to_string();
    refused(&mut s, json!({}));
    refused(&mut s, json!({"dir": "relative/backup"}));
    refused(&mut s, json!({"dir": 7}));
    refused(&mut s, json!({"dir": made.to_string_lossy(), "to": "relative"}));
    // a folder that is no backup: the library itself, a folder that isn't there
    assert!(refused(&mut s, json!({"dir": dir.to_string_lossy()})).contains("not a backup"));
    refused(&mut s, json!({"dir": dir.join("nowhere").to_string_lossy()}));
    // into a backup
    assert!(refused(&mut s, json!({"dir": made.to_string_lossy(), "to": made.to_string_lossy()})).contains("is in the backup"));
    // the parameter that is wrong is the one named
    assert!(refused(&mut s, json!({"dir": made.to_string_lossy(), "to": "relative"})).contains("`to`"));
    assert!(refused(&mut s, json!({"dir": 7})).contains("`dir`"));
    // a backup that was edited
    let other = back_up(&mut s);
    let snap = other.join("catalog.snap");
    std::fs::write(&snap, std::fs::read_to_string(&snap).unwrap().replacen("\"seq\":", "\"seq\": ", 1)).unwrap();
    assert!(refused(&mut s, json!({"dir": other.to_string_lossy()})).contains("catalog.snap"));
    std::fs::remove_dir_all(&other).unwrap();
    // while a backup of the library runs
    s.backup_hold.store(true, Ordering::SeqCst);
    s.execute("library.backup", &json!({"background": true})).unwrap();
    assert!(refused(&mut s, json!({"dir": made.to_string_lossy()})).contains("is running"));
    s.backup_hold.store(false, Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    while s.execute("library.backupStatus", &json!({})).unwrap()["running"] == json!(true) && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let running = PathBuf::from(s.execute("library.backupStatus", &json!({})).unwrap()["report"]["dir"].as_str().unwrap());
    std::fs::remove_dir_all(&running).unwrap();

    assert_eq!(names(&dir), before);
    assert_eq!(s.catalog.to_snapshot(), now);
    assert!(s.activity.list().is_empty());
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    drop(s);
    assert_eq!(open(&dir).catalog.photo(id).unwrap().rating, 3);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restore that was cut short after it began to move files is finished when the library is
/// next opened, and the library says so.
#[test]
fn a_restore_cut_short_is_finished_when_the_library_is_opened() {
    let dir = temp_dir("cut-short");
    let mut s = open(&dir);
    let id = s.selection.active.unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let made = back_up(&mut s);
    let then = s.catalog.to_snapshot();
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    drop(s);
    // as a restore leaves the folder when the program ends after its note is written: the
    // backup's files copied, the note of what is to be moved, nothing moved yet
    let staged = dir.join("restore.partial");
    std::fs::create_dir_all(staged.join("segments")).unwrap();
    let mut new = Vec::new();
    for name in ["catalog.log", "presets.json", "catalog.snap"] {
        if made.join(name).is_file() {
            std::fs::copy(made.join(name), staged.join(name)).unwrap();
            new.push(name);
        }
    }
    for segment in names(&made.join("segments")) {
        std::fs::copy(made.join("segments").join(&segment), staged.join("segments").join(&segment)).unwrap();
    }
    new.insert(0, "segments");
    let note = json!({"phase": "aside", "aside": "before", "old": ["catalog.snap", "catalog.log", "segments", "presets.json"], "new": new});
    std::fs::write(dir.join("restore.json"), note.to_string()).unwrap();

    let mut s = open(&dir);
    assert_eq!(s.catalog.to_snapshot(), then);
    assert_eq!(s.catalog.photo(id).unwrap().rating, 4);
    let warnings = s.take_library_warnings();
    assert!(warnings.iter().any(|w| w.contains("restore") && w.contains("before")), "{warnings:?}");
    assert!(!dir.join("restore.json").exists() && !staged.exists());
    assert_eq!(open_at(&dir.join("Replaced").join("before")).photo(id).unwrap().rating, 1);
    // a note that can't be used: the library is not opened as a mix, and nothing is moved
    drop(s);
    std::fs::write(dir.join("restore.json"), b"{").unwrap();
    let before = names(&dir);
    let mut again = Session::new();
    let e = again.open_library(&dir, false).unwrap_err();
    assert!(e.to_string().contains("restore"), "{e}");
    assert_eq!(names(&dir), before);
    let _ = std::fs::remove_dir_all(&dir);
}

fn open_at(dir: &Path) -> lightcraft_catalog::Catalog {
    open(dir).catalog.clone()
}

/// The backups of the library are listed newest first, from the folder its backups go to or
/// the one asked for.
#[test]
fn the_librarys_backups_are_listed() {
    let (dir, there) = (temp_dir("list"), temp_dir("list-there"));
    let mut s = open(&dir);
    let none = s.execute("library.backups", &json!({})).unwrap();
    assert_eq!(none["dir"].as_str(), Some(dir.join("Backups").to_string_lossy().as_ref()));
    assert_eq!(none["backups"], json!([]));
    let first = back_up(&mut s);
    let second = back_up(&mut s);
    let listed = s.execute("library.backups", &json!({})).unwrap();
    let dirs: Vec<&str> = listed["backups"].as_array().unwrap().iter().map(|b| b["dir"].as_str().unwrap()).collect();
    let mut expect = vec![first.to_string_lossy().into_owned(), second.to_string_lossy().into_owned()];
    expect.sort();
    let mut got: Vec<String> = dirs.iter().map(|d| d.to_string()).collect();
    got.sort();
    assert_eq!(got, expect);
    assert_eq!(listed["backups"][0]["photos"].as_u64(), Some(s.catalog.len() as u64));
    // another folder
    s.execute("library.backup", &json!({"dir": there.to_string_lossy()})).unwrap();
    let elsewhere = s.execute("library.backups", &json!({"dir": there.to_string_lossy()})).unwrap();
    assert_eq!(elsewhere["backups"].as_array().unwrap().len(), 1);
    assert!(s.execute("library.backups", &json!({"dir": "relative"})).is_err());
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&there);
}

/// The folder the library's backups go to is the library's choice now, not the backup's from
/// then: a restore keeps it.
#[test]
fn a_restore_keeps_where_backups_go() {
    let (dir, there) = (temp_dir("keeps"), temp_dir("keeps-there"));
    let mut s = open(&dir);
    let made = back_up(&mut s);
    s.execute("library.backupPreferences", &json!({"dir": there.to_string_lossy()})).unwrap();
    restore(&mut s, &made);
    assert_eq!(s.execute("library.backupPreferences", &json!({})).unwrap()["dir"].as_str(), Some(there.to_string_lossy().as_ref()));
    drop(s);
    let mut s = open(&dir);
    assert_eq!(s.execute("library.backupPreferences", &json!({})).unwrap()["dir"].as_str(), Some(there.to_string_lossy().as_ref()));
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&there);
}

/// A backup whose files are what its marker lists but that doesn't open as a catalog: the
/// restore is taken back, the session is in the library as it was, and its changes go on being
/// saved there.
#[test]
fn a_restored_library_that_does_not_open_is_taken_back() {
    let dir = temp_dir("taken-back");
    let mut s = open(&dir);
    let id = s.selection.active.unwrap();
    let made = back_up(&mut s);
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    let now = s.catalog.to_snapshot();
    // a segment of the backup that is no catalog data, listed by the snapshot and the marker as
    // it now is: every checksum is right, and it opens as nothing
    let mut snap: Value = serde_json::from_slice(&std::fs::read(made.join("catalog.snap")).unwrap()).unwrap();
    let name = snap["segments"][0]["name"].as_str().unwrap().to_string();
    let junk = b"{\"not\": \"photos\"}".to_vec();
    snap["segments"][0]["bytes"] = json!(junk.len());
    snap["segments"][0]["crc"] = json!(crc32fast::hash(&junk));
    let snap = serde_json::to_vec(&snap).unwrap();
    std::fs::write(made.join(&name), &junk).unwrap();
    std::fs::write(made.join("catalog.snap"), &snap).unwrap();
    let mut marker: Value = serde_json::from_slice(&std::fs::read(made.join("backup.json")).unwrap()).unwrap();
    marker["contents"][&name] = json!({"bytes": junk.len(), "crc": crc32fast::hash(&junk)});
    marker["contents"]["catalog.snap"] = json!({"bytes": snap.len(), "crc": crc32fast::hash(&snap)});
    std::fs::write(made.join("backup.json"), marker.to_string()).unwrap();
    assert_eq!(s.execute("library.checkBackup", &json!({"dir": made.to_string_lossy()})).unwrap()["ok"], json!(true));
    // (restored into a folder of its own, it is a library that doesn't open)
    let probe = temp_dir("taken-back-probe");
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "to": probe.to_string_lossy()})).unwrap();
    assert!(Session::new().open_library(&probe, false).is_err());
    let _ = std::fs::remove_dir_all(&probe);
    // what of the session is the library's: its presets, where its smart previews go
    s.execute("preset.create", &json!({"name": "Mine"})).unwrap();
    let presets: Vec<String> = s.presets.iter().map(|p| p.id.clone()).collect();
    let smart = temp_dir("taken-back-smart");
    s.media.smart_dir = Some(smart.clone());
    let e = s.execute("library.restore", &json!({"dir": made.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("as it was"), "{e}");
    assert_eq!(s.catalog.to_snapshot(), now);
    assert!(dir.join("Replaced").join("2026-09-30 12.00.00 UTC (not opened)").join("catalog.snap").is_file());
    assert!(!dir.join("Replaced").join("2026-09-30 12.00.00 UTC").exists());
    assert!(!dir.join("restore.json").exists());
    assert_eq!(s.presets.iter().map(|p| p.id.clone()).collect::<Vec<_>>(), presets);
    assert_eq!(s.media.smart_dir, Some(smart));
    // the session still holds the library's lock: nobody else opens it
    assert!(matches!(Session::new().open_library(&dir, false), Err(EngineError::LibraryInUse(_))));
    s.execute("photo.rate", &json!({"rating": 5})).unwrap();
    drop(s);
    assert_eq!(open(&dir).catalog.photo(id).unwrap().rating, 5);
    let _ = std::fs::remove_dir_all(&dir);
}

/// After a restore the session's presets are the restored library's: one made since the backup
/// is not shown as the library's, and not written back into it.
#[test]
fn a_restored_librarys_presets_are_the_backups() {
    let dir = temp_dir("presets");
    let mut s = open(&dir);
    let user = |s: &Session| s.presets.iter().filter(|p| !p.builtin).map(|p| p.name.clone()).collect::<Vec<_>>();
    s.execute("preset.create", &json!({"name": "Before"})).unwrap();
    let made = back_up(&mut s);
    s.execute("preset.create", &json!({"name": "After"})).unwrap();
    assert_eq!(user(&s), ["Before", "After"]);
    let builtin = s.presets.iter().filter(|p| p.builtin).count();
    restore(&mut s, &made);
    assert_eq!(user(&s), ["Before"]);
    assert_eq!(s.presets.iter().filter(|p| p.builtin).count(), builtin);
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    drop(s);
    assert_eq!(user(&open(&dir)), ["Before"]);
    // what was replaced has the one made since
    assert_eq!(user(&open(&dir.join("Replaced").join("2026-09-30 12.00.00 UTC"))), ["Before", "After"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `to` is a folder of its own: not in the backup (which is only read, and passes its check
/// afterwards), not in the open library, and not a folder of other things. A restore that is
/// refused leaves no folder and no lock file behind. `to` naming the open library is a restore
/// of it.
#[test]
fn a_backup_is_restored_into_a_folder_of_its_own() {
    let dir = temp_dir("own-folder");
    let mut s = open(&dir);
    let made = back_up(&mut s);
    let then = s.catalog.to_snapshot();
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    let before = (names(&made), names(&dir), names(&dir.join("segments")));
    let refused = |s: &mut Session, to: &Path| {
        s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "to": to.to_string_lossy()})).unwrap_err().to_string()
    };
    for to in [made.join("sub"), made.join("segments"), made.join("a").join("b")] {
        assert!(refused(&mut s, &to).contains("is in the backup"), "{}", to.display());
    }
    for to in [dir.join("segments"), dir.join("Backups"), dir.join("new")] {
        assert!(refused(&mut s, &to).contains("open library's folder"), "{}", to.display());
    }
    // a folder of somebody's files that is no library
    let plain = temp_dir("own-folder-plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("prefs.json"), b"mine").unwrap();
    assert!(refused(&mut s, &plain).contains("not a library"));
    assert_eq!(names(&plain), ["prefs.json"]);
    assert_eq!(std::fs::read(plain.join("prefs.json")).unwrap(), b"mine");
    assert_eq!((names(&made), names(&dir), names(&dir.join("segments"))), before);
    assert_eq!(s.execute("library.checkBackup", &json!({"dir": made.to_string_lossy()})).unwrap()["ok"], json!(true));
    // refused once the folder would have been made: nothing of it stays
    let other = back_up(&mut s);
    let snap = other.join("catalog.snap");
    std::fs::write(&snap, std::fs::read_to_string(&snap).unwrap().replacen("\"seq\":", "\"seq\": ", 1)).unwrap();
    let fresh = temp_dir("own-folder-fresh");
    assert!(s.execute("library.restore", &json!({"dir": other.to_string_lossy(), "to": fresh.to_string_lossy()})).is_err());
    assert!(!fresh.exists());
    std::fs::create_dir_all(&fresh).unwrap();
    assert!(s.execute("library.restore", &json!({"dir": other.to_string_lossy(), "to": fresh.to_string_lossy()})).is_err());
    assert!(names(&fresh).is_empty(), "{:?}", names(&fresh));
    // the open library by another spelling of its path is the open library
    let spelled = dir.join("Backups").join("..");
    let r = s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "to": spelled.to_string_lossy()})).unwrap();
    assert_eq!(r["opened"], json!(true));
    assert_eq!(s.catalog.to_snapshot(), then);
    drop(s);
    for d in [dir, plain, fresh] {
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// A library is not restored while other work is under way in it.
#[test]
fn a_library_at_work_is_not_restored() {
    let dir = temp_dir("at-work");
    let mut s = open(&dir);
    let made = back_up(&mut s);
    let task = s.activity.start("import", "Importing photos", crate::activity::Cancel::No);
    let e = s.execute("library.restore", &json!({"dir": made.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("Importing photos"), "{e}");
    // (into another folder it is)
    let to = temp_dir("at-work-to");
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "to": to.to_string_lossy()})).unwrap();
    drop(task);
    restore(&mut s, &made);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&to);
}

/// A restore that can't move the library's files out (its segments folder can't be moved) puts
/// back the ones it did move: the session is in the library as it was, and goes on saving there.
#[cfg(unix)]
#[test]
fn a_restore_that_cannot_move_the_librarys_files_is_undone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("undone");
    let mut s = open(&dir);
    let id = s.selection.active.unwrap();
    let made = back_up(&mut s);
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    // nothing left to write when the library is closed, and then its segments folder can't be
    // moved: the snapshot and the log go, the folder stays
    s.compact_library().unwrap();
    let now = s.catalog.to_snapshot();
    let before = names(&dir);
    let segments = dir.join("segments");
    let mode = |m: u32| std::fs::set_permissions(&segments, std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o555);
    let writable = std::fs::write(segments.join("probe"), b"").is_ok();
    let _ = std::fs::remove_file(segments.join("probe"));
    let r = s.execute("library.restore", &json!({"dir": made.to_string_lossy()}));
    mode(0o755);
    if writable && r.is_ok() {
        // (run as root: nothing can be made to fail this way)
        eprintln!("skipped: a folder without write permission can be moved here");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let e = r.unwrap_err().to_string();
    assert!(e.contains("segments") && e.matches("the library is as it was").count() == 1, "{e}");
    assert_eq!(names(&dir), before);
    assert_eq!(s.catalog.to_snapshot(), now);
    assert!(s.activity.list().is_empty());
    s.execute("photo.rate", &json!({"rating": 5})).unwrap();
    drop(s);
    assert_eq!(open(&dir).catalog.photo(id).unwrap().rating, 5);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restore that stopped while it was moving the library's files, and could neither go on nor
/// go back: the session changes and writes nothing more (its files are no longer the library's)
/// and says why, and opening the library again, which is never refused, completes the restore.
#[test]
fn a_restore_stuck_half_way_writes_nothing_until_the_library_is_opened_again() {
    let dir = temp_dir("stuck");
    let mut s = open(&dir);
    let id = s.selection.active.unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let made = back_up(&mut s);
    let then = s.catalog.to_snapshot();
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    s.execute("library.backupPreferences", &json!({"dir": null})).unwrap();
    s.close_library().unwrap();
    // the folder as such a restore leaves it: the backup copied, the note written, the snapshot
    // moved out; and the session as `library.restore` leaves it then
    let staged = dir.join("restore.partial");
    std::fs::create_dir_all(staged.join("segments")).unwrap();
    let mut new = vec!["segments"];
    for name in ["catalog.log", "presets.json", "catalog.snap"] {
        if made.join(name).is_file() {
            std::fs::copy(made.join(name), staged.join(name)).unwrap();
            new.push(name);
        }
    }
    for segment in names(&made.join("segments")) {
        std::fs::copy(made.join("segments").join(&segment), staged.join("segments").join(&segment)).unwrap();
    }
    std::fs::write(
        dir.join("restore.json"),
        json!({"phase": "aside", "aside": "before", "old": ["catalog.snap", "catalog.log", "segments", "presets.json"], "new": new}).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("Replaced").join("before")).unwrap();
    std::fs::rename(dir.join("catalog.snap"), dir.join("Replaced").join("before").join("catalog.snap")).unwrap();
    s.library.as_mut().unwrap().stuck = Some("the restore stopped; open the library again to finish it".into());
    let before =
        (names(&dir), names(&dir.join("segments")), std::fs::read(dir.join("prefs.json")).unwrap(), std::fs::read(dir.join("view.json")).ok());

    // nothing is changed, and nothing written, while it is so
    for (cmd, params) in [
        ("photo.rate", json!({"rating": 5})),
        ("library.backupPreferences", json!({"dir": "/elsewhere"})),
        ("library.restore", json!({"dir": made.to_string_lossy()})),
        ("edit.undo", json!({})),
    ] {
        let e = s.execute(cmd, &params).unwrap_err().to_string();
        assert!(e.contains("open the library again"), "{cmd}: {e}");
    }
    assert!(s.commit("Rate", lightcraft_catalog::Op::SetRating { id, rating: 5 }).is_err());
    assert_eq!(s.catalog.photo(id).unwrap().rating, 1);
    assert!(s.pending_log.is_empty());
    s.selection = crate::Selection::default();
    s.save_view();
    assert!(s.save_prefs().is_err());
    s.persist().unwrap();
    assert!(s.compact_library().is_err());
    assert_eq!(
        (names(&dir), names(&dir.join("segments")), std::fs::read(dir.join("prefs.json")).unwrap(), std::fs::read(dir.join("view.json")).ok()),
        before
    );
    // the way out is open
    s.close_library().unwrap();
    s.open_library(&dir, false).unwrap();
    assert_eq!(s.catalog.to_snapshot(), then);
    assert!(!dir.join("restore.json").exists());
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    drop(s);
    assert_eq!(open(&dir).catalog.photo(id).unwrap().rating, 2);
    assert_eq!(open(&dir.join("Replaced").join("before")).catalog.photo(id).unwrap().rating, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Finish a restore that runs in the background as the app's frame loop does: `None` if it
/// doesn't end.
fn pump(s: &mut Session) -> Option<Value> {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(20) {
        s.persist_if_dirty();
        let status = s.execute("library.restoreStatus", &json!({})).unwrap();
        if status["running"] == json!(false) {
            return Some(status);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    None
}

/// With `background`, the backup is copied and checked on a worker while the library stays in
/// use; its files are moved into place afterwards, between commands. A change made while the
/// copy was under way is in what the restore replaced.
#[test]
fn a_restore_in_the_background_leaves_the_library_in_use_until_the_copy_is_made() {
    let dir = temp_dir("background");
    let mut s = open(&dir);
    let id = s.selection.active.unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let made = back_up(&mut s);
    let then = s.catalog.to_snapshot();
    assert!(s.execute("library.restoreStatus", &json!({})).unwrap().is_null());
    s.restore_hold.store(true, Ordering::SeqCst);
    let r = s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    assert_eq!((r["running"].as_bool(), r["restored"].is_null(), r["error"].is_null()), (Some(true), true, true));
    assert_eq!(r["backup"].as_str(), Some(made.to_string_lossy().as_ref()));
    let rows = s.activity.list();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].cancellable), ("restore", "Restoring the library", true));
    // the library is in use meanwhile, and nothing of it has moved
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    s.persist_if_dirty();
    assert_eq!(s.catalog.photo(id).unwrap().rating, 1);
    assert!(dir.join("catalog.snap").is_file() && !dir.join("Replaced").exists());
    // one at a time
    let e = s.execute("library.restore", &json!({"dir": made.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("a restore of this library is under way"), "{e}");
    s.restore_hold.store(false, Ordering::SeqCst);
    let done = pump(&mut s).expect("the restore ends");
    assert!(done["error"].is_null() && done["cancelled"] == json!(false), "{done}");
    assert_eq!(done["restored"]["library"].as_str(), Some(dir.to_string_lossy().as_ref()));
    assert_eq!(s.catalog.to_snapshot(), then);
    assert!(s.activity.list().is_empty());
    let aside = PathBuf::from(done["restored"]["aside"].as_str().unwrap());
    drop(s);
    assert_eq!(open(&aside).catalog.photo(id).unwrap().rating, 1, "the change made during the copy is kept");
    assert_eq!(open(&dir).catalog.photo(id).unwrap().rating, 4);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restore in the background can be stopped while the backup is copied (its row's ✕): the
/// library is untouched, nothing of the copy is left, and it says so. One of a backup that is
/// not sound ends the same way, with the reason.
#[test]
fn a_restore_in_the_background_can_be_stopped_and_says_why_it_failed() {
    let dir = temp_dir("background-stop");
    let mut s = open(&dir);
    let made = back_up(&mut s);
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    let now = s.catalog.to_snapshot();
    let before = names(&dir);
    s.restore_hold.store(true, Ordering::SeqCst);
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    let row = s.activity.list()[0].id;
    s.activity.cancel(row).unwrap();
    s.restore_hold.store(false, Ordering::SeqCst);
    let done = pump(&mut s).expect("the restore ends");
    assert_eq!(done["cancelled"], json!(true), "{done}");
    assert!(done["restored"].is_null() && done["error"].as_str().is_some_and(|e| e.contains("was stopped")), "{done}");
    assert_eq!(names(&dir), before);
    assert_eq!(s.catalog.to_snapshot(), now);
    assert!(s.activity.list().is_empty());
    // a backup that was edited
    let snap = made.join("catalog.snap");
    std::fs::write(&snap, std::fs::read_to_string(&snap).unwrap().replacen("\"seq\":", "\"seq\": ", 1)).unwrap();
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    let done = pump(&mut s).expect("the restore ends");
    assert!(done["cancelled"] == json!(false) && done["error"].as_str().is_some_and(|e| e.contains("catalog.snap")), "{done}");
    assert_eq!(names(&dir), before);
    assert_eq!(s.catalog.to_snapshot(), now);
    // and the next one is made
    std::fs::remove_dir_all(&made).unwrap();
    let made = back_up(&mut s);
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    assert!(pump(&mut s).expect("the restore ends")["error"].is_null());
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The backup's files are not moved in while other work is under way in the library, or a
/// slider is held: the restore waits, and is made once that is over.
#[test]
fn a_restore_in_the_background_waits_for_other_work_to_end() {
    let dir = temp_dir("background-waits");
    let mut s = open(&dir);
    let made = back_up(&mut s);
    let then = s.catalog.to_snapshot();
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    let other = s.activity.start("import", "Importing photos", crate::activity::Cancel::No);
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_millis(400) {
        s.persist_if_dirty();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let waiting = s.execute("library.restoreStatus", &json!({})).unwrap();
    assert_eq!(waiting["running"], json!(true), "{waiting}");
    assert_ne!(s.catalog.to_snapshot(), then);
    assert!(dir.join("restore.partial").is_dir() && !dir.join("Replaced").exists());
    drop(other);
    assert!(pump(&mut s).expect("the restore ends")["error"].is_null());
    assert_eq!(s.catalog.to_snapshot(), then);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restore belongs to the library it is of: when another library is opened before the copy is
/// moved into place, it is given up, the first library is as it was (nothing of the copy left),
/// and the other library is not touched.
#[test]
fn a_restore_in_the_background_is_given_up_when_another_library_is_opened() {
    let (a, b) = (temp_dir("background-a"), temp_dir("background-b"));
    let mut s = open(&a);
    let made = back_up(&mut s);
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    let now = s.catalog.to_snapshot();
    s.restore_hold.store(true, Ordering::SeqCst);
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    s.close_library().unwrap();
    s.open_library(&b, true).unwrap();
    let other = s.catalog.to_snapshot();
    assert!(s.execute("library.restoreStatus", &json!({})).unwrap().is_null());
    // given up as the other library was opened, not when somebody next asks
    assert_eq!(s.restore.as_ref().map(|r| r.running()), Some(false));
    s.restore_hold.store(false, Ordering::SeqCst);
    // the copy stops: its row goes, and it leaves nothing in the library that is no longer open
    let t0 = std::time::Instant::now();
    while !s.activity.list().is_empty() && t0.elapsed() < std::time::Duration::from_secs(20) {
        assert!(!a.join("restore.partial").join("catalog.snap").exists(), "the backup was copied all the same");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(s.activity.list().is_empty());
    // and the library that is open is not held up by it
    let of_b = back_up(&mut s);
    restore(&mut s, &of_b);
    assert!(!a.join("restore.partial").exists() && !a.join("Replaced").exists() && !a.join("restore.json").exists());
    assert_eq!(s.catalog.to_snapshot(), other);
    assert_eq!(s.library.as_ref().map(|l| l.dir.clone()), Some(b.clone()));
    drop(s);
    assert_eq!(open(&a).catalog.to_snapshot(), now);
    for d in [a, b] {
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// Wait until the backup of a restore in the background is copied (its row says so).
fn copied(s: &Session) {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(20) {
        if s.activity.list().iter().any(|t| t.kind == "restore" && t.detail == "copied") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the backup is never copied");
}

/// The app's frame loop alone sees a restore in the background through (nobody asks for its
/// status), and so does asking for its status alone (where there is no frame loop).
#[test]
fn a_restore_in_the_background_is_seen_through_by_the_frame_loop_or_by_asking() {
    for frames in [true, false] {
        let dir = temp_dir(if frames { "background-frames" } else { "background-asked" });
        let mut s = open(&dir);
        let made = back_up(&mut s);
        let then = s.catalog.to_snapshot();
        s.execute("photo.rate", &json!({"rating": 3})).unwrap();
        s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
        let job = s.restore.clone().unwrap();
        let t0 = std::time::Instant::now();
        while job.running() && t0.elapsed() < std::time::Duration::from_secs(20) {
            if frames {
                s.persist_if_dirty();
            } else {
                s.execute("library.restoreStatus", &json!({})).unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(job.outcome().is_some_and(|o| o.is_ok()), "frames: {frames}: {:?}", job.outcome());
        assert_eq!(s.catalog.to_snapshot(), then, "frames: {frames}");
        assert!(s.activity.list().is_empty());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Once the backup is copied, the restore waits while a slider is held (its row says the copy
/// is made, and can still stop it), and goes on when it is let go. Stopped then, the library is
/// as it was and nothing of the copy is left.
#[test]
fn a_copied_backup_waits_for_a_held_slider_and_can_still_be_stopped() {
    let dir = temp_dir("background-held");
    let mut s = open(&dir);
    let made = back_up(&mut s);
    let then = s.catalog.to_snapshot();
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    let now = s.catalog.to_snapshot();
    let before = names(&dir);
    s.begin_interaction("Exposure").unwrap();
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    copied(&s);
    for _ in 0..20 {
        s.persist_if_dirty();
    }
    let rows = s.activity.list();
    assert_eq!((rows.len(), rows[0].done, rows[0].cancellable), (1, rows[0].total, true), "{rows:?}");
    assert!(rows[0].total > 0 && dir.join("restore.partial").join("catalog.snap").is_file());
    assert_eq!(s.catalog.to_snapshot(), now);
    // stopped while it waits
    s.activity.cancel(rows[0].id).unwrap();
    s.persist_if_dirty();
    let done = s.execute("library.restoreStatus", &json!({})).unwrap();
    assert!(done["cancelled"] == json!(true) && done["error"].as_str().is_some_and(|e| e.contains("was stopped")), "{done}");
    assert!(s.activity.list().is_empty());
    assert_eq!(s.catalog.to_snapshot(), now);
    // and one that is let go on
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    copied(&s);
    s.persist_if_dirty();
    assert_eq!(s.execute("library.restoreStatus", &json!({})).unwrap()["running"], json!(true));
    s.end_interaction().unwrap();
    assert!(pump(&mut s).expect("the restore ends")["error"].is_null());
    assert_eq!(s.catalog.to_snapshot(), then);
    drop(s);
    let _ = (before, std::fs::remove_dir_all(&dir));
}

/// A backup that is copied and waiting is given up when its library is opened again, or another
/// is: the copy is removed there and then. And a copy that a program that ended left behind is
/// cleared away when the library is opened.
#[test]
fn a_copy_of_a_backup_does_not_outlive_its_restore() {
    let (dir, other) = (temp_dir("background-copy"), temp_dir("background-copy-other"));
    let mut s = open(&dir);
    let made = back_up(&mut s);
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    let now = s.catalog.to_snapshot();
    let before = names(&dir);
    for again in [true, false] {
        s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
        let blocker = s.activity.start("import", "Importing photos", crate::activity::Cancel::No);
        copied(&s);
        assert!(dir.join("restore.partial").is_dir());
        drop(blocker);
        s.close_library().unwrap();
        s.open_library(if again { &dir } else { &other }, true).unwrap();
        assert!(!dir.join("restore.partial").exists(), "again: {again}");
        assert!(s.activity.list().is_empty());
        s.close_library().unwrap();
        s.open_library(&dir, false).unwrap();
        let status = s.execute("library.restoreStatus", &json!({})).unwrap();
        assert!(status["running"] == json!(false) && status["error"].as_str().is_some_and(|e| e.contains("as it was")), "{status}");
        assert_eq!(s.catalog.to_snapshot(), now);
        assert_eq!(names(&dir), before);
    }
    // left by a program that ended while it copied
    drop(s);
    std::fs::create_dir_all(dir.join("restore.partial").join("segments")).unwrap();
    std::fs::copy(made.join("catalog.snap"), dir.join("restore.partial").join("catalog.snap")).unwrap();
    let s = open(&dir);
    assert!(!dir.join("restore.partial").exists());
    assert_eq!(s.catalog.to_snapshot(), now);
    drop(s);
    for d in [dir, other] {
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// The command is not offered while it can't run: a restore or a backup of the library is under
/// way.
#[test]
fn restore_is_not_offered_while_a_restore_or_backup_runs() {
    let dir = temp_dir("offered");
    let mut s = open(&dir);
    let made = back_up(&mut s);
    let enabled = |s: &Session| (crate::cmd::command_specs().iter().find(|c| c.id == "library.restore").unwrap().enabled)(s);
    assert_eq!(enabled(&s), Ok(()));
    s.backup_hold.store(true, Ordering::SeqCst);
    s.execute("library.backup", &json!({"background": true})).unwrap();
    assert!(enabled(&s).is_err_and(|e| e.contains("backup")));
    s.backup_hold.store(false, Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    while !s.activity.list().is_empty() && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    s.restore_hold.store(true, Ordering::SeqCst);
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    assert!(enabled(&s).is_err_and(|e| e.contains("restore")));
    s.restore_hold.store(false, Ordering::SeqCst);
    pump(&mut s).expect("the restore ends");
    assert_eq!(enabled(&s), Ok(()));
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restore that was given up has a worker that is still at work in the library folder until it
/// sees so. It must not clear away what another restore of the library copied there meanwhile:
/// no other restore is started (or its copy cleared as a leftover) while it is, and the worker
/// that was given up before it began copies nothing.
#[test]
fn given_up_worker_must_not_remove_the_next_restores_copy() {
    let dir = temp_dir("given-up-worker");
    let mut s = open(&dir);
    let made = back_up(&mut s);
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    let enabled = |s: &Session| (crate::cmd::command_specs().iter().find(|c| c.id == "library.restore").unwrap().enabled)(s);
    s.restore_hold.store(true, Ordering::SeqCst);
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    // the library is opened again: the restore is given up, its worker is held where it is
    s.close_library().unwrap();
    s.open_library(&dir, false).unwrap();
    assert_eq!(s.restore.as_ref().map(|r| r.running()), Some(false));
    // another restore is refused while the worker of the first is there (it would share `restore.partial`)
    let e = s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap_err();
    assert!(e.to_string().contains("still being stopped"), "{e}");
    assert!(enabled(&s).is_err_and(|e| e.contains("stopped")));
    // its status stays `running` while the worker is at work, so that whoever waits for it to end
    // (the command line) does not start another restore into the refusal
    let status = s.execute("library.restoreStatus", &json!({})).unwrap();
    assert_eq!(status["running"], json!(true), "{status}");
    s.restore_hold.store(false, Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    while !s.activity.list().is_empty() && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(s.activity.list().is_empty());
    // the worker that was given up copied nothing, and left nothing
    assert!(!dir.join("restore.partial").exists() && !dir.join("Replaced").exists());
    // and a restore is possible again once it is over
    assert_eq!(enabled(&s), Ok(()));
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    assert!(pump(&mut s).expect("the restore ends")["error"].is_null());
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An open that fails leaves a restore that is under way alone: it is given up only once another
/// library is open (or this one again), not by an attempt that came to nothing.
#[test]
fn a_failed_open_does_not_give_up_a_restore() {
    let dir = temp_dir("failed-open");
    let mut s = open(&dir);
    let made = back_up(&mut s);
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    let then = s.catalog.to_snapshot();
    s.restore_hold.store(true, Ordering::SeqCst);
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    // (a file that is none of a library's own: a library is a folder)
    let file = dir.join("not-a-library.txt");
    std::fs::write(&file, b"x").unwrap();
    assert!(s.open_library(&file, false).is_err());
    assert_eq!(s.restore.as_ref().map(|r| r.running()), Some(true), "the restore was given up by an open that failed");
    s.restore_hold.store(false, Ordering::SeqCst);
    let done = pump(&mut s).expect("the restore ends");
    assert!(done["error"].is_null(), "{done}");
    assert_ne!(s.catalog.to_snapshot(), then);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restore that was given up has a worker in the library folder that wakes after another
/// session took the folder over (the first one let go of its lock) and copied a backup into it:
/// the worker sees it was given up before it clears or makes anything, so the other session's
/// copy is still there and its restore is made.
#[test]
fn a_given_up_worker_that_wakes_late_leaves_the_next_sessions_copy_alone() {
    let (dir, other) = (temp_dir("late-worker"), temp_dir("late-worker-other"));
    let mut first = open(&dir);
    let made = back_up(&mut first);
    let then = first.catalog.to_snapshot();
    first.execute("photo.rate", &json!({"rating": 3})).unwrap();
    first.persist_if_dirty();
    first.restore_hold.store(true, Ordering::SeqCst);
    first.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    // given up as another library is opened: the folder's lock is let go, the worker is still held
    first.close_library().unwrap();
    first.open_library(&other, true).unwrap();
    assert_eq!(first.restore.as_ref().map(|r| r.running()), Some(false));
    // another session has the folder, and its restore has copied the backup in
    let mut second = open(&dir);
    second.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    copied(&second);
    assert!(dir.join("restore.partial").join("catalog.snap").is_file());
    // the first worker wakes
    first.restore_hold.store(false, Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    while !first.activity.list().is_empty() && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(first.activity.list().is_empty());
    assert!(dir.join("restore.partial").join("catalog.snap").is_file(), "the other session's copy was removed");
    assert!(pump(&mut second).expect("the restore ends")["error"].is_null());
    assert_eq!(second.catalog.to_snapshot(), then);
    drop((first, second));
    for d in [dir, other] {
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// A restore into another folder (`to`) is refused as well while the worker of a given-up
/// restore of that folder is still at work in it.
#[test]
fn a_restore_into_a_folder_whose_given_up_worker_still_runs_is_refused() {
    let (dir, other) = (temp_dir("to-late"), temp_dir("to-late-other"));
    let mut s = open(&dir);
    let made = back_up(&mut s);
    s.restore_hold.store(true, Ordering::SeqCst);
    s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "background": true})).unwrap();
    s.close_library().unwrap();
    s.open_library(&other, true).unwrap();
    let e = s.execute("library.restore", &json!({"dir": made.to_string_lossy(), "to": dir.to_string_lossy()})).unwrap_err();
    assert!(e.to_string().contains("still being stopped"), "{e}");
    s.restore_hold.store(false, Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    while !s.activity.list().is_empty() && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    drop(s);
    for d in [dir, other] {
        let _ = std::fs::remove_dir_all(&d);
    }
}
