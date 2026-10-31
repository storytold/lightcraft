//! Restoring a library from a backup ([`crate::restore`]): the library becomes what the backup
//! holds, what it replaces is kept, and a restore cut short is finished later.

use std::path::{Path, PathBuf};

use crate::backup::{self, BackupError, BackupReport};
use crate::journal::{LOG, SNAPSHOT};
use crate::restore::{self, PENDING, REPLACED, RestoreOptions, RestoreReport};
use crate::tests_backup::{add, back_up, files, library, opened, run, same_file, temp};
use crate::*;

const ALSO: &[&str] = &["presets.json", "view.json"];

fn options(aside: &str) -> RestoreOptions<'_> {
    RestoreOptions { also: ALSO, aside }
}

fn restored(backup: &Path, library: &Path, aside: &str) -> RestoreReport {
    restore::restore(backup, library, &options(aside), &()).unwrap()
}

/// A library that was backed up (10 photos, presets `[]`) and then went on: 20 photos, other
/// presets. Returns the backup, the catalog as backed up, and the catalog as it is now.
fn backed_up_and_changed(dir: &Path) -> (BackupReport, Catalog, Catalog) {
    let (mut j, mut c) = library(dir, 10);
    let made = back_up(dir, &dir.join("Backups"), "first");
    let then = opened(&made.dir);
    run(&mut j, &mut c, add(11..=20));
    j.snapshot(&c).unwrap();
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(2), rating: 5 }]);
    std::fs::write(dir.join("presets.json"), b"[1]").unwrap();
    drop(j);
    (made, then, opened(dir))
}

/// The library folder as it is right now, were it opened without anyone finishing a restore:
/// when it has a snapshot, it is all of the one catalog or all of the other (the snapshot is the
/// first file moved out and the last moved in).
fn assert_whole_if_it_has_a_snapshot(dir: &Path, then: &Catalog, now: &Catalog, stop: usize) {
    if !dir.join(SNAPSHOT).exists() {
        return;
    }
    let copy = temp("restore-state");
    std::fs::create_dir_all(copy.join("segments")).unwrap();
    for name in files(dir).into_iter().filter(|n| n == SNAPSHOT || n == LOG || n.starts_with("segments/")) {
        std::fs::copy(dir.join(&name), copy.join(&name)).unwrap();
    }
    let got = Journal::open(Box::new(FsStore::open(&copy).unwrap())).unwrap_or_else(|e| panic!("stopped at step {stop}: {e}")).1;
    assert!(same(&got, then) || same(&got, now), "stopped at step {stop}: a mix of two catalogs");
    let _ = std::fs::remove_dir_all(&copy);
}

/// Stop a restore of a fresh library at the first point where `want` holds of its folder.
fn stopped_where(tag: &str, want: impl Fn(&Path) -> bool) -> (PathBuf, BackupReport, Catalog, Catalog) {
    for stop in 0..60 {
        let dir = temp(tag);
        let (made, then, now) = backed_up_and_changed(&dir);
        restore::stop_at(Some(stop));
        let result = restore::restore(&made.dir, &dir, &options("before"), &());
        restore::stop_at(None);
        if result.is_err() && want(&dir) {
            return (dir, made, then, now);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    panic!("a restore never stops there");
}

fn phase(dir: &Path) -> Option<String> {
    let note: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(PENDING)).ok()?).ok()?;
    note["phase"].as_str().map(str::to_string)
}

fn same(a: &Catalog, b: &Catalog) -> bool {
    a.to_snapshot() == b.to_snapshot()
}

/// Given a library that changed since it was backed up, when it is restored from the backup,
/// then it opens as the backup, what it was is kept in `Replaced/<name>` (a library folder too),
/// the backup is as it was made and shares no file with the library, and nothing else is left.
#[test]
fn a_restored_library_is_the_backup_and_what_it_replaced_is_kept() {
    let dir = temp("restore");
    let (made, then, now) = backed_up_and_changed(&dir);
    assert!(!same(&then, &now));
    let r = restored(&made.dir, &dir, "before");
    assert!(same(&opened(&dir), &then));
    assert_eq!(std::fs::read(dir.join("presets.json")).unwrap(), b"[]");
    let aside = dir.join(REPLACED).join("before");
    assert_eq!(r.aside.as_deref(), Some(aside.as_path()));
    assert!(same(&opened(&aside), &now));
    assert_eq!(std::fs::read(aside.join("presets.json")).unwrap(), b"[1]");
    assert_eq!((r.seq, r.photos, r.library.as_path(), r.backup.as_path()), (made.seq, made.photos, dir.as_path(), made.dir.as_path()));
    assert_eq!(r.files, made.files);
    assert!(r.skipped.is_empty());
    // the backup is untouched, and not shared with the library (which goes on changing)
    assert_eq!(backup::check(&made.dir).unwrap().contents, made.contents);
    for name in made.contents.keys() {
        assert_ne!(same_file(&made.dir.join(name), &dir.join(name)), Some(true), "{name}");
    }
    let top: Vec<String> = files(&dir).into_iter().filter(|n| !n.starts_with("segments/")).collect();
    assert_eq!(top, ["Backups", "Replaced", "catalog.log", "catalog.snap", "presets.json"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A backup restores into a folder that holds no library: nothing is set aside.
#[test]
fn a_backup_restores_into_a_new_folder() {
    let dir = temp("restore-new");
    let (made, then, _) = backed_up_and_changed(&dir);
    let to = temp("restore-new-to").join("made").join("here");
    let r = restored(&made.dir, &to, "before");
    assert_eq!(r.aside, None);
    assert!(same(&opened(&to), &then));
    assert!(!to.join(REPLACED).exists());
    // and a second time, over the first
    let again = restored(&made.dir, &to, "again");
    assert_eq!(again.aside, Some(to.join(REPLACED).join("again")));
    assert!(same(&opened(&to), &then));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A backup that isn't what it was when it was made is not restored, and the library is as it
/// was: nothing moved, nothing left.
#[test]
fn a_backup_that_was_touched_is_not_restored() {
    let dir = temp("restore-touched");
    let (made, _, now) = backed_up_and_changed(&dir);
    let before = files(&dir);
    let segment = made.contents.keys().find(|n| n.starts_with("segments/")).unwrap();
    let mut bytes = std::fs::read(made.dir.join(segment)).unwrap();
    bytes[10] ^= 1;
    std::fs::write(made.dir.join(segment), &bytes).unwrap();
    let e = restore::restore(&made.dir, &dir, &options("before"), &()).unwrap_err();
    assert!(matches!(e, BackupError::Changed(_)), "{e}");
    assert_eq!(files(&dir), before);
    assert!(same(&opened(&dir), &now));
    // a folder that is no backup
    let e = restore::restore(&dir.join("Backups"), &dir, &options("before"), &()).unwrap_err();
    assert!(matches!(e, BackupError::Changed(_)), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// What can't be restored says why, before anything is touched.
#[test]
fn what_cannot_be_restored_says_why() {
    let dir = temp("restore-refused");
    let (made, _, now) = backed_up_and_changed(&dir);
    let before = files(&dir);
    let refused = |backup: &Path, library: &Path, o: &RestoreOptions<'_>| restore::restore(backup, library, o, &()).unwrap_err();
    // a name for what is replaced that is no folder name, or is taken
    for name in ["", "..", "a/b", "x.partial"] {
        assert!(matches!(refused(&made.dir, &dir, &options(name)), BackupError::Name(_)), "{name:?}");
    }
    std::fs::create_dir_all(dir.join(REPLACED).join("taken")).unwrap();
    assert!(matches!(refused(&made.dir, &dir, &options("taken")), BackupError::Exists(_)));
    std::fs::remove_dir_all(dir.join(REPLACED)).unwrap();
    // files to restore that are the catalog's own, or not plain names
    for bad in ["catalog.snap", "restore.json", "../x", "segments"] {
        let o = RestoreOptions { also: &[bad], aside: "before" };
        assert!(matches!(refused(&made.dir, &dir, &o), BackupError::Name(_)), "{bad:?}");
    }
    // a backup is not restored into itself, nor into another backup
    assert!(matches!(refused(&made.dir, &made.dir, &options("before")), BackupError::Restore(_)));
    let other = back_up(&dir, &dir.join("Backups"), "second");
    assert!(matches!(refused(&made.dir, &other.dir, &options("before")), BackupError::Restore(_)));
    std::fs::remove_dir_all(&other.dir).unwrap();
    // a stop asked for while the backup is copied
    struct Stop;
    impl backup::Watch for Stop {
        fn cancelled(&self) -> bool {
            true
        }
    }
    assert!(matches!(restore::restore(&made.dir, &dir, &options("before"), &Stop).unwrap_err(), BackupError::Cancelled));
    assert_eq!(files(&dir), before);
    assert!(same(&opened(&dir), &now));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Only the catalog and the files asked for are restored: another file of the backup stays out
/// (and is named), and the library's own stays where it is.
#[test]
fn only_the_files_asked_for_are_restored() {
    let dir = temp("restore-also");
    let (j, _) = library(&dir, 10);
    drop(j);
    std::fs::write(dir.join("view.json"), b"{\"then\":1}").unwrap();
    let made =
        backup::back_up(&dir, &dir.join("Backups"), &backup::BackupOptions { name: "first", also: &["presets.json", "view.json"] }, &()).unwrap();
    std::fs::write(dir.join("view.json"), b"{\"now\":1}").unwrap();
    std::fs::write(dir.join("presets.json"), b"[2]").unwrap();
    let r = restore::restore(&made.dir, &dir, &RestoreOptions { also: &["presets.json"], aside: "before" }, &()).unwrap();
    assert_eq!(r.skipped, ["view.json"]);
    assert_eq!(std::fs::read(dir.join("view.json")).unwrap(), b"{\"now\":1}");
    assert_eq!(std::fs::read(dir.join("presets.json")).unwrap(), b"[]");
    assert!(!dir.join(REPLACED).join("before").join("view.json").exists());
    assert_eq!(r.files, made.files - 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file the library has and the backup doesn't (a log, presets) doesn't stay behind to be
/// read with the restored catalog.
#[test]
fn what_the_backup_does_not_hold_is_set_aside_too() {
    let dir = temp("restore-less");
    let (mut j, c) = library(&dir, 10);
    std::fs::remove_file(dir.join("presets.json")).unwrap();
    // compacted: the backup has an empty log, or none
    j.snapshot(&c).unwrap();
    let made = back_up(&dir, &dir.join("Backups"), "first");
    let then = opened(&made.dir);
    let mut c = c;
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(3), rating: 2 }]);
    drop(j);
    std::fs::write(dir.join("presets.json"), b"[3]").unwrap();
    restored(&made.dir, &dir, "before");
    assert!(same(&opened(&dir), &then));
    assert_eq!(opened(&dir).photo(PhotoId(3)).unwrap().rating, 0);
    assert!(!dir.join("presets.json").exists());
    assert!(dir.join(REPLACED).join("before").join("presets.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a restore that stops at any point (the program ends, the disk fails), when the library
/// is next asked to finish it, then the library is either still what it was or all the backup,
/// never a mix, and nothing of the restore is left behind either way.
#[test]
fn a_restore_cut_short_is_finished_or_never_started() {
    let mut stopped_before_the_swap = 0;
    let mut finished_later = 0;
    for stop in 0.. {
        let dir = temp("restore-stop");
        let (made, then, now) = backed_up_and_changed(&dir);
        let before = files(&dir);
        restore::stop_at(Some(stop));
        let result = restore::restore(&made.dir, &dir, &options("before"), &());
        restore::stop_at(None);
        if result.is_ok() {
            assert!(same(&opened(&dir), &then));
            assert!(stop >= 6, "the swap takes several steps ({stop})");
            let _ = std::fs::remove_dir_all(&dir);
            break;
        }
        let pending = dir.join(PENDING).exists();
        assert_whole_if_it_has_a_snapshot(&dir, &then, &now, stop);
        match restore::finish(&dir).unwrap() {
            Some(r) => {
                assert!(pending);
                finished_later += 1;
                assert!(same(&opened(&dir), &then), "stopped at step {stop}");
                assert!(same(&opened(&r.aside.unwrap()), &now), "stopped at step {stop}");
            }
            None => {
                assert!(!pending);
                stopped_before_the_swap += 1;
                assert!(same(&opened(&dir), &now), "stopped at step {stop}");
            }
        }
        assert!(!dir.join(PENDING).exists());
        if !pending {
            // never begun: nothing of it is left, not even an empty folder
            assert_eq!(files(&dir), before, "stopped at step {stop}");
        }
        assert_eq!(restore::finish(&dir).unwrap(), None);
        // and the library can be restored after it
        restored(&made.dir, &dir, "later");
        assert!(same(&opened(&dir), &then), "stopped at step {stop}");
        let top: Vec<String> = files(&dir).into_iter().filter(|n| !n.starts_with("segments/")).collect();
        assert_eq!(top, ["Backups", "Replaced", "catalog.log", "catalog.snap", "presets.json"], "stopped at step {stop}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(stopped_before_the_swap >= 1 && finished_later >= 5, "{stopped_before_the_swap} {finished_later}");
}

/// The note of a restore under way is a file like any other: one that can't be read, or that
/// names anything but files of the library folder, stops the restore from being finished (and
/// another from being started) without moving a thing.
#[test]
fn a_damaged_note_of_a_restore_moves_nothing() {
    let dir = temp("restore-note");
    let (made, _, _) = backed_up_and_changed(&dir);
    let outside = temp("restore-note-outside");
    std::fs::write(outside.join("precious"), b"x").unwrap();
    let notes = [
        "not json".to_string(),
        serde_json::json!({"phase": "aside", "aside": "../../x", "old": ["catalog.snap"], "new": []}).to_string(),
        serde_json::json!({"phase": "aside", "aside": "x", "old": [format!("../{}/precious", outside.file_name().unwrap().to_string_lossy())], "new": []}).to_string(),
        serde_json::json!({"phase": "place", "aside": "x", "old": [], "new": ["a/b"]}).to_string(),
        serde_json::json!({"phase": "aside", "aside": "x", "old": ["Backups"], "new": []}).to_string(),
    ];
    for note in notes {
        std::fs::write(dir.join(PENDING), &note).unwrap();
        let before = files(&dir);
        assert!(matches!(restore::finish(&dir), Err(BackupError::Restore(_))), "{note}");
        assert!(matches!(restore::restore(&made.dir, &dir, &options("before"), &()), Err(BackupError::Restore(_))), "{note}");
        assert_eq!(files(&dir), before, "{note}");
        assert!(outside.join("precious").exists());
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

/// Something that turns up in the library folder while it is restored (a program without the
/// lock made a log) is set aside as well, not written over and not in the way.
#[test]
fn a_file_that_turns_up_during_a_restore_is_set_aside() {
    let dir = temp("restore-intruder");
    let (made, then, now_of_intruder) = backed_up_and_changed(&dir);
    // stopped once everything old is aside, and then a log appears
    let mut stop = 0;
    loop {
        restore::stop_at(Some(stop));
        let result = restore::restore(&made.dir, &dir, &options("before"), &());
        restore::stop_at(None);
        assert!(result.is_err(), "never got to an empty folder");
        if dir.join(PENDING).exists() && !dir.join(SNAPSHOT).exists() && !dir.join(LOG).exists() && !dir.join("segments").exists() {
            break;
        }
        restore::finish(&dir).unwrap();
        // (start again from the library as it was)
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _ = backed_up_and_changed(&dir);
        stop += 1;
    }
    std::fs::write(dir.join(LOG), b"an intruder\n").unwrap();
    let r = restore::finish(&dir).unwrap().unwrap();
    assert!(same(&opened(&dir), &then));
    let aside = r.aside.unwrap();
    let kept: Vec<PathBuf> =
        std::fs::read_dir(&aside).unwrap().map(|e| e.unwrap().path()).filter(|p| std::fs::read(p).is_ok_and(|b| b == b"an intruder\n")).collect();
    assert_eq!(kept.len(), 1, "{:?}", files(&aside));
    // and nothing that was set aside before it was written over
    assert!(same(&opened(&aside), &now_of_intruder));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The backups of a folder are listed newest first, each with the folder it is in now.
#[test]
fn the_backups_of_a_folder_are_listed_newest_first() {
    let dir = temp("restore-list");
    let (j, _) = library(&dir, 10);
    drop(j);
    let dest = dir.join("Backups");
    assert!(backup::list(&dest).is_empty());
    let first = back_up(&dir, &dest, "b first");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let second = back_up(&dir, &dest, "a second");
    // what is no finished backup is not listed
    std::fs::create_dir_all(dest.join("other")).unwrap();
    std::fs::create_dir_all(dest.join("half.partial")).unwrap();
    std::fs::write(dest.join("half.partial").join("backup.json"), b"{}").unwrap();
    let listed = backup::list(&dest);
    assert_eq!(listed.iter().map(|r| r.dir.clone()).collect::<Vec<_>>(), [second.dir.clone(), first.dir.clone()]);
    assert_eq!(listed[1].contents, first.contents);
    // moved: the folder it is in now
    let moved = temp("restore-list-moved");
    std::fs::rename(&second.dir, moved.join("x")).unwrap();
    assert_eq!(backup::list(&moved)[0].dir, moved.join("x"));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&moved);
}

/// A backup made by a newer LightCraft is refused before anything of the library is moved.
#[test]
fn a_backup_this_version_cannot_open_is_refused_before_the_library_is_touched() {
    let dir = temp("restore-newer");
    let (made, _, now) = backed_up_and_changed(&dir);
    // a backup whose snapshot says it is of a later format, and whose marker agrees with it
    let snap = String::from_utf8(std::fs::read(made.dir.join(SNAPSHOT)).unwrap()).unwrap();
    let newer = snap.replacen(&format!("\"version\":{}", crate::journal::VERSION), "\"version\":99", 1);
    assert_ne!(snap, newer);
    std::fs::write(made.dir.join(SNAPSHOT), &newer).unwrap();
    let mut marker: BackupReport = serde_json::from_slice(&std::fs::read(made.dir.join(backup::MARKER)).unwrap()).unwrap();
    marker.contents.insert(SNAPSHOT.into(), backup::Held { bytes: newer.len() as u64, crc: crc32fast::hash(newer.as_bytes()) });
    std::fs::write(made.dir.join(backup::MARKER), serde_json::to_vec(&marker).unwrap()).unwrap();
    backup::check(&made.dir).unwrap();
    let before = files(&dir);
    let e = restore::restore(&made.dir, &dir, &options("before"), &()).unwrap_err();
    assert!(matches!(&e, BackupError::Restore(why) if why.contains("newer")), "{e}");
    assert_eq!(files(&dir), before);
    assert!(same(&opened(&dir), &now));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a library that was restored, when the restore is taken back, then the library is what
/// it was before, what the restore had put there is kept instead, and the folder that held the
/// library meanwhile is gone. Cut short at any point, it is the one or the other.
#[test]
fn a_restore_is_taken_back() {
    for stop in 0.. {
        let dir = temp("restore-back");
        let (made, then, now) = backed_up_and_changed(&dir);
        let r = restored(&made.dir, &dir, "before");
        restore::stop_at(Some(stop));
        let result = restore::take_back(&r, "not opened");
        restore::stop_at(None);
        assert_whole_if_it_has_a_snapshot(&dir, &then, &now, stop);
        let finished = restore::finish(&dir).unwrap();
        if result.is_err() && finished.is_none() {
            // never begun: still the restored library
            assert!(same(&opened(&dir), &then), "stopped at step {stop}");
            let _ = std::fs::remove_dir_all(&dir);
            continue;
        }
        assert!(same(&opened(&dir), &now), "stopped at step {stop}");
        assert_eq!(std::fs::read(dir.join("presets.json")).unwrap(), b"[1]");
        assert!(same(&opened(&dir.join(REPLACED).join("not opened")), &then), "stopped at step {stop}");
        assert!(!dir.join(REPLACED).join("before").exists(), "stopped at step {stop}");
        assert!(!dir.join(PENDING).exists());
        let _ = std::fs::remove_dir_all(&dir);
        if result.is_ok() {
            assert!(stop >= 6);
            break;
        }
    }
    // a restore into an empty folder, taken back: the folder holds no library again
    let dir = temp("restore-back-empty");
    let (made, then, _) = backed_up_and_changed(&dir);
    let to = temp("restore-back-to");
    let r = restored(&made.dir, &to, "before");
    restore::take_back(&r, "not opened").unwrap();
    assert!(!to.join(SNAPSHOT).exists() && !to.join(LOG).exists() && !to.join("segments").exists());
    assert!(same(&opened(&to.join(REPLACED).join("not opened")), &then));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&to);
}

/// A settings file of the library that is a link (kept with other settings elsewhere) is moved
/// aside as the link it is: the restore is made, and the file it points to is not touched.
#[cfg(unix)]
#[test]
fn a_linked_settings_file_is_set_aside_as_a_link() {
    let dir = temp("restore-link");
    let (made, then, _) = backed_up_and_changed(&dir);
    let elsewhere = temp("restore-link-elsewhere");
    std::fs::rename(dir.join("presets.json"), elsewhere.join("presets.json")).unwrap();
    std::os::unix::fs::symlink(elsewhere.join("presets.json"), dir.join("presets.json")).unwrap();
    restored(&made.dir, &dir, "before");
    assert!(same(&opened(&dir), &then));
    assert!(!dir.join(PENDING).exists() && !dir.join("restore.partial").exists());
    assert_eq!(std::fs::read(elsewhere.join("presets.json")).unwrap(), b"[1]");
    assert!(std::fs::symlink_metadata(dir.join(REPLACED).join("before").join("presets.json")).unwrap().file_type().is_symlink());
    assert!(std::fs::symlink_metadata(dir.join("presets.json")).unwrap().is_file());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&elsewhere);
}

/// What a restore can't do is found out before its note is written: nothing is left that would
/// keep the library from opening, and nothing was moved.
#[test]
fn a_restore_that_cannot_be_made_leaves_no_note() {
    // taking back a restore whose report names what is no file of a catalog
    let dir = temp("restore-no-note");
    let (made, then, _) = backed_up_and_changed(&dir);
    let mut r = restored(&made.dir, &dir, "before");
    let before = files(&dir);
    r.placed.push("Backups".into());
    assert!(matches!(restore::take_back(&r, "undone"), Err(BackupError::Restore(_))));
    r.placed.pop();
    r.placed.push("catalog.lock".into());
    assert!(matches!(restore::take_back(&r, "undone"), Err(BackupError::Restore(_))));
    r.placed.pop();
    // a name that is taken, a folder that is not one of `Replaced`, a name that is none
    assert!(matches!(restore::take_back(&r, "before"), Err(BackupError::Exists(_))));
    assert!(matches!(restore::take_back(&r, "a/b"), Err(BackupError::Name(_))));
    let elsewhere = RestoreReport { aside: Some(made.dir.clone()), ..r.clone() };
    assert!(matches!(restore::take_back(&elsewhere, "undone"), Err(BackupError::Restore(_))));
    assert_eq!(files(&dir), before);
    assert_eq!(restore::finish(&dir).unwrap(), None);
    assert!(same(&opened(&dir), &then));
    // two copies of a backup made for one library: the one left without its files says so
    let a = restore::stage(&made.dir, &dir, &options("a"), &()).unwrap();
    let b = restore::stage(&made.dir, &dir, &options("b"), &()).unwrap();
    drop(a);
    assert!(matches!(b.replace(), Err(BackupError::Restore(_))));
    assert_eq!(files(&dir), before);
    assert!(same(&opened(&dir), &then));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Taking a restore back moves back what the restore moved out, and only that: what else is in
/// that folder by then (it was opened as a library: a lock file, a cache) stays there, and the
/// library's own lock file is not replaced.
#[test]
fn taking_back_moves_only_what_the_restore_moved() {
    let dir = temp("restore-back-only");
    let (made, then, now) = backed_up_and_changed(&dir);
    let r = restored(&made.dir, &dir, "before");
    assert_eq!(r.replaced, ["catalog.snap", "catalog.log", "segments", "presets.json"]);
    let aside = dir.join(REPLACED).join("before");
    std::fs::create_dir_all(aside.join("thumbs")).unwrap();
    std::fs::write(aside.join("catalog.lock"), b"").unwrap();
    std::fs::write(dir.join("catalog.lock"), b"held by this session").unwrap();
    // (with a note pending, not at all)
    std::fs::write(dir.join(PENDING), b"{").unwrap();
    assert!(matches!(restore::take_back(&r, "undone"), Err(BackupError::Restore(_))));
    std::fs::remove_file(dir.join(PENDING)).unwrap();
    restore::take_back(&r, "undone").unwrap();
    assert!(same(&opened(&dir), &now));
    assert_eq!(std::fs::read(dir.join("catalog.lock")).unwrap(), b"held by this session");
    assert_eq!(files(&aside), ["catalog.lock", "thumbs"]);
    assert!(same(&opened(&dir.join(REPLACED).join("undone")), &then));
    assert!(!dir.join(PENDING).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restore whose copy of the backup is gone when it is to be completed: while only the
/// library's own files were moved, they are moved back and the library is as it was; once the
/// backup's are being moved in, nothing is done and the error says where things are. Never a
/// library reported restored that has no catalog.
#[test]
fn a_restore_without_its_copy_of_the_backup_is_not_completed() {
    let (dir, _, _, now) = stopped_where("restore-gone-aside", |d| phase(d).as_deref() == Some("aside") && !d.join(SNAPSHOT).exists());
    std::fs::remove_dir_all(dir.join("restore.partial")).unwrap();
    assert_eq!(restore::finish(&dir).unwrap(), None);
    assert!(same(&opened(&dir), &now));
    let top: Vec<String> = files(&dir).into_iter().filter(|n| !n.starts_with("segments/")).collect();
    assert_eq!(top, ["Backups", "catalog.log", "catalog.snap", "presets.json"]);
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, _, _, now) =
        stopped_where("restore-gone-place", |d| phase(d).as_deref() == Some("place") && d.join("segments").exists() && !d.join(SNAPSHOT).exists());
    std::fs::remove_dir_all(dir.join("restore.partial")).unwrap();
    let before = files(&dir);
    for _ in 0..2 {
        let e = restore::finish(&dir).unwrap_err();
        assert!(matches!(&e, BackupError::Restore(why) if why.contains("catalog.snap") && why.contains("Replaced")), "{e}");
    }
    assert_eq!(files(&dir), before);
    assert!(same(&opened(&dir.join(REPLACED).join("before")), &now));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file that turns up where one of the backup's is about to be moved in is set aside under a
/// name of its own: not written over, and nothing set aside before it is.
#[test]
fn a_file_in_the_way_of_the_backups_is_set_aside_under_its_own_name() {
    let (dir, _, then, now) = stopped_where("restore-in-the-way", |d| phase(d).as_deref() == Some("place") && !d.join(SNAPSHOT).exists());
    std::fs::write(dir.join(SNAPSHOT), b"an intruder").unwrap();
    let r = restore::finish(&dir).unwrap().unwrap();
    assert!(same(&opened(&dir), &then));
    let aside = r.aside.unwrap();
    assert_eq!(std::fs::read(aside.join("catalog.snap (2)")).unwrap(), b"an intruder");
    assert!(same(&opened(&aside), &now));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Links where a restore expects its own files and folders are not followed: a file elsewhere is
/// not written through one, and nothing is moved out of the library folder or into it from
/// elsewhere.
#[cfg(unix)]
#[test]
fn links_are_not_followed_out_of_the_library_folder() {
    use std::os::unix::fs::symlink;
    let outside = temp("restore-links-outside");
    // the note's own temporary file
    let dir = temp("restore-links-tmp");
    let (made, then, _) = backed_up_and_changed(&dir);
    std::fs::write(outside.join("precious"), b"my thesis").unwrap();
    symlink(outside.join("precious"), dir.join("restore.json.tmp")).unwrap();
    restored(&made.dir, &dir, "before");
    assert_eq!(std::fs::read(outside.join("precious")).unwrap(), b"my thesis");
    assert!(same(&opened(&dir), &then));
    let _ = std::fs::remove_dir_all(&dir);
    // `Replaced`, the folder in it, and the copy of the backup
    let note = |old: &[&str], new: &[&str], phase: &str| serde_json::json!({"phase": phase, "aside": "x", "old": old, "new": new}).to_string();
    for link in [REPLACED, "restore.partial"] {
        let dir = temp("restore-links");
        let (made, _, now) = backed_up_and_changed(&dir);
        std::fs::write(outside.join("catalog.log"), b"someone else's\n").unwrap();
        symlink(&outside, dir.join(link)).unwrap();
        let before = files(&dir);
        for text in [note(&["catalog.snap", "catalog.log", "segments"], &[], "aside"), note(&[], &["catalog.log"], "place")] {
            std::fs::write(dir.join(PENDING), text).unwrap();
            assert!(matches!(restore::finish(&dir), Err(BackupError::Restore(_))), "{link}");
        }
        std::fs::remove_file(dir.join(PENDING)).unwrap();
        // nor is a restore begun with it there
        assert!(matches!(restore::restore(&made.dir, &dir, &options("x"), &()), Err(BackupError::Restore(_))), "{link}");
        assert_eq!(files(&dir), before, "{link}");
        assert_eq!(std::fs::read(outside.join("catalog.log")).unwrap(), b"someone else's\n");
        assert_eq!(files(&outside), ["catalog.log", "precious"]);
        assert!(same(&opened(&dir), &now));
        let _ = std::fs::remove_dir_all(&dir);
    }
    // the folder in `Replaced`
    let dir = temp("restore-links-aside");
    let (_, _, now) = backed_up_and_changed(&dir);
    std::fs::create_dir_all(dir.join(REPLACED)).unwrap();
    symlink(&outside, dir.join(REPLACED).join("x")).unwrap();
    std::fs::write(dir.join(PENDING), note(&["catalog.snap", "catalog.log", "segments"], &[], "aside")).unwrap();
    assert!(matches!(restore::finish(&dir), Err(BackupError::Restore(_))));
    std::fs::remove_file(dir.join(PENDING)).unwrap();
    assert!(same(&opened(&dir), &now));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

/// A note moves files of a catalog and nothing else of the folder: not the lock's files, not a
/// backup's marker, not the restore's own, whatever their case.
#[test]
fn a_note_does_not_move_the_locks_files_or_the_restores_own() {
    let dir = temp("restore-reserved");
    let (_, _, now) = backed_up_and_changed(&dir);
    for name in
        ["catalog.lock", "Catalog.Lock", "catalog.lock.owner", "backup.json", "RESTORE.JSON", "restore.partial", "replaced", "restore.json.tmp"]
    {
        std::fs::write(dir.join("catalog.lock"), b"").unwrap();
        std::fs::write(dir.join(PENDING), serde_json::json!({"phase": "aside", "aside": "x", "old": ["catalog.snap", name], "new": []}).to_string())
            .unwrap();
        let before = files(&dir);
        assert!(matches!(restore::finish(&dir), Err(BackupError::Restore(_))), "{name}");
        assert_eq!(files(&dir), before, "{name}");
    }
    std::fs::remove_file(dir.join(PENDING)).unwrap();
    assert!(same(&opened(&dir), &now));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A backup is only read: nothing is restored into its own folder or a folder in it, and it
/// passes its check afterwards as before.
#[test]
fn nothing_is_restored_into_the_backups_own_folder() {
    let dir = temp("restore-into-backup");
    let (made, _, _) = backed_up_and_changed(&dir);
    for to in [made.dir.join("copy"), made.dir.join("segments"), made.dir.join("a").join("b")] {
        let e = restore::restore(&made.dir, &to, &options("before"), &()).unwrap_err();
        assert!(matches!(e, BackupError::Restore(_)), "{e}");
        assert!(!made.dir.join("copy").exists() && !made.dir.join("a").exists());
    }
    assert_eq!(backup::check(&made.dir).unwrap().contents, made.contents);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A folder named like the restore's own that holds somebody's files is not cleared away, and
/// `Replaced` that is no folder is not worked around: the restore is refused, and they stay.
#[test]
fn what_is_in_the_restores_way_and_not_its_own_stays() {
    let dir = temp("restore-in-the-way-2");
    let (made, _, now) = backed_up_and_changed(&dir);
    std::fs::create_dir_all(dir.join("restore.partial")).unwrap();
    std::fs::write(dir.join("restore.partial").join("notes.txt"), b"mine").unwrap();
    assert!(matches!(restore::restore(&made.dir, &dir, &options("before"), &()), Err(BackupError::Restore(_))));
    assert_eq!(std::fs::read(dir.join("restore.partial").join("notes.txt")).unwrap(), b"mine");
    std::fs::remove_dir_all(dir.join("restore.partial")).unwrap();
    // (what a restore left there is cleared away)
    std::fs::create_dir_all(dir.join("restore.partial").join("segments")).unwrap();
    std::fs::write(dir.join("restore.partial").join("catalog.log"), b"").unwrap();
    std::fs::write(dir.join(REPLACED), b"a file").unwrap();
    assert!(matches!(restore::restore(&made.dir, &dir, &options("before"), &()), Err(BackupError::Restore(_))));
    assert_eq!(std::fs::read(dir.join(REPLACED)).unwrap(), b"a file");
    std::fs::remove_file(dir.join(REPLACED)).unwrap();
    assert!(same(&opened(&dir), &now));
    restored(&made.dir, &dir, "before");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file of the backup that changes after the backup was checked, while it is being copied, is
/// found out as it is copied: the library is not touched.
#[test]
fn a_backup_that_changes_while_it_is_copied_is_not_restored() {
    struct Spoil(PathBuf, std::sync::atomic::AtomicBool);
    impl backup::Watch for Spoil {
        fn progress(&self, name: &str, _: u64, _: u64) {
            if name.starts_with("segments/") && !self.1.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let mut bytes = std::fs::read(self.0.join(name)).unwrap();
                bytes[10] ^= 1;
                std::fs::write(self.0.join(name), bytes).unwrap();
            }
        }
    }
    let dir = temp("restore-spoiled");
    let (made, _, now) = backed_up_and_changed(&dir);
    let before = files(&dir);
    let spoil = Spoil(made.dir.clone(), Default::default());
    let e = restore::restore(&made.dir, &dir, &options("before"), &spoil).unwrap_err();
    assert!(spoil.1.load(std::sync::atomic::Ordering::SeqCst));
    assert!(matches!(e, BackupError::Changed(_)), "{e}");
    assert_eq!(files(&dir), before);
    assert!(same(&opened(&dir), &now));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A backup whose log holds a change this version doesn't know, or that holds no snapshot, is
/// refused before the library is touched.
#[test]
fn a_backup_with_a_newer_log_or_no_snapshot_is_refused() {
    let relist = |dir: &Path, name: &str, bytes: Option<&[u8]>| {
        let mut marker: BackupReport = serde_json::from_slice(&std::fs::read(dir.join(backup::MARKER)).unwrap()).unwrap();
        match bytes {
            Some(bytes) => {
                std::fs::write(dir.join(name), bytes).unwrap();
                marker.contents.insert(name.into(), backup::Held { bytes: bytes.len() as u64, crc: crc32fast::hash(bytes) });
            }
            None => {
                std::fs::remove_file(dir.join(name)).unwrap();
                marker.contents.remove(name);
            }
        }
        std::fs::write(dir.join(backup::MARKER), serde_json::to_vec(&marker).unwrap()).unwrap();
        backup::check(dir).unwrap();
    };
    let dir = temp("restore-newer-log");
    let (made, _, now) = backed_up_and_changed(&dir);
    let before = files(&dir);
    let body = r#"{"op":"fromTheFuture","id":1}"#;
    let mut log = std::fs::read(made.dir.join(LOG)).unwrap();
    log.extend_from_slice(format!("{{\"seq\":99999,\"crc\":{},\"op\":{body}}}\n", crc32fast::hash(body.as_bytes())).as_bytes());
    relist(&made.dir, LOG, Some(&log));
    let e = restore::restore(&made.dir, &dir, &options("before"), &()).unwrap_err();
    assert!(matches!(&e, BackupError::Restore(why) if why.contains("newer")), "{e}");
    assert_eq!(files(&dir), before);
    relist(&made.dir, LOG, None);
    relist(&made.dir, SNAPSHOT, None);
    let e = restore::restore(&made.dir, &dir, &options("before"), &()).unwrap_err();
    assert!(matches!(&e, BackupError::Changed(why) if why.contains(SNAPSHOT)), "{e}");
    assert_eq!(files(&dir), before);
    assert!(same(&opened(&dir), &now));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The copy of a backup that a restore left when the program ended before it moved a file is
/// cleared away when asked; a folder of that name with somebody's files in it, or one a restore
/// under way still needs, is not.
#[test]
fn a_leftover_copy_of_a_backup_is_cleared_away() {
    let dir = temp("restore-leftover");
    let (made, _, now) = backed_up_and_changed(&dir);
    let staged = restore::stage(&made.dir, &dir, &options("before"), &()).unwrap();
    std::mem::forget(staged);
    assert!(dir.join("restore.partial").join(SNAPSHOT).is_file());
    restore::clear_leftover(&dir);
    assert!(!dir.join("restore.partial").exists());
    assert!(same(&opened(&dir), &now));
    // somebody's
    std::fs::create_dir_all(dir.join("restore.partial")).unwrap();
    std::fs::write(dir.join("restore.partial").join("notes.txt"), b"mine").unwrap();
    restore::clear_leftover(&dir);
    assert_eq!(std::fs::read(dir.join("restore.partial").join("notes.txt")).unwrap(), b"mine");
    let _ = std::fs::remove_dir_all(&dir);
    // a restore under way
    let (dir, _, then, _) = stopped_where("restore-leftover-needed", |d| phase(d).is_some());
    restore::clear_leftover(&dir);
    assert!(dir.join("restore.partial").is_dir());
    restore::finish(&dir).unwrap().unwrap();
    assert!(same(&opened(&dir), &then));
    let _ = std::fs::remove_dir_all(&dir);
}
