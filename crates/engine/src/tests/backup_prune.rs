//! How long a library keeps its backups: forever unless chosen (`library.backupPreferences`,
//! `keepDays`). After a backup is made, the library's own backups older than that are removed,
//! never its newest three, and nothing that is not certainly an old backup of it;
//! `library.pruneBackups` says which, and removes them when told to.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-engine-prune-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A library on disk in `dir`, and the clock its session goes by.
fn open(dir: &Path) -> (Session, Arc<Mutex<String>>) {
    let mut s = Session::new();
    let now = Arc::new(Mutex::new("2026-01-10T09:00:00".to_string()));
    let clock = now.clone();
    s.clock = Box::new(move || clock.lock().unwrap().clone());
    s.open_library(dir, true).unwrap();
    (s, now)
}

/// Back the library up at `time`; the backup's answer.
fn back_up_at(s: &mut Session, now: &Arc<Mutex<String>>, time: &str) -> Value {
    *now.lock().unwrap() = time.to_string();
    let r = s.execute("library.backup", &json!({})).unwrap();
    assert!(r["error"].is_null(), "{r}");
    s.persist_if_dirty();
    r
}

fn prefs(s: &mut Session, p: Value) -> Value {
    s.execute("library.backupPreferences", &p).unwrap()
}

/// The backups in a folder, by name.
fn backups(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(dir).map(|d| d.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect()).unwrap_or_default();
    names.sort();
    names
}

/// Given a library, then it keeps its backups forever until told otherwise; how long is
/// answered back and saved with the library; and a time that is none is refused.
#[test]
fn backups_are_kept_forever_until_chosen_otherwise() {
    let dir = temp_dir("prefs");
    let (mut s, now) = open(&dir);
    assert!(prefs(&mut s, json!({}))["keepDays"].is_null());
    for t in ["2026-01-10T09:00:00", "2027-01-10T09:00:00", "2029-01-10T09:00:00"] {
        back_up_at(&mut s, &now, t);
    }
    assert_eq!(backups(&dir.join("Backups")).len(), 3, "nothing is removed that was not asked for");

    assert_eq!(prefs(&mut s, json!({"keepDays": 90}))["keepDays"], json!(90));
    for bad in [json!({"keepDays": 0}), json!({"keepDays": -3}), json!({"keepDays": "a month"}), json!({"keepDays": 1.5})] {
        let e = s.execute("library.backupPreferences", &bad).unwrap_err().to_string();
        assert!(e.contains("keepDays"), "{bad}: {e}");
    }
    assert_eq!(prefs(&mut s, json!({}))["keepDays"], json!(90));
    s.close_library().unwrap();
    drop(s);
    let (mut s, _) = open(&dir);
    assert_eq!(prefs(&mut s, json!({}))["keepDays"], json!(90));
    assert!(prefs(&mut s, json!({"keepDays": null}))["keepDays"].is_null(), "forever again");
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a library that keeps its backups for 30 days, with backups of the 10th and the 30th of
/// January and the 24th of February, when it is backed up on the 20th of March, then the one of
/// the 10th of January is removed after the new one is made (the backup says so), and the
/// others stay. A year later the next backup takes the oldest again, and the three newest stay,
/// old as two of them are: age is told by clocks, and a wrong one must not cost every backup.
#[test]
fn backups_older_than_a_library_keeps_them_go_after_the_next_one_is_made() {
    let dir = temp_dir("after");
    let (mut s, now) = open(&dir);
    prefs(&mut s, json!({"keepDays": 30}));
    for t in ["2026-01-10T09:00:00", "2026-01-30T09:00:00", "2026-02-24T09:00:00"] {
        let r = back_up_at(&mut s, &now, t);
        assert_eq!(r["removed"], json!([]), "{t}: three are always kept: {r}");
    }
    let r = back_up_at(&mut s, &now, "2026-03-20T09:00:00");
    assert_eq!(r["removed"], json!(["2026-01-10 09.00.00 UTC"]), "{r}");
    assert_eq!(backups(&dir.join("Backups")), ["2026-01-30 09.00.00 UTC", "2026-02-24 09.00.00 UTC", "2026-03-20 09.00.00 UTC"]);

    let r = back_up_at(&mut s, &now, "2027-03-20T09:00:00");
    assert_eq!(r["removed"], json!(["2026-01-30 09.00.00 UTC"]), "{r}");
    assert_eq!(backups(&dir.join("Backups")), ["2026-02-24 09.00.00 UTC", "2026-03-20 09.00.00 UTC", "2027-03-20 09.00.00 UTC"]);
    // and what is left are whole backups
    for b in backups(&dir.join("Backups")) {
        s.execute("library.checkBackup", &json!({"dir": dir.join("Backups").join(b).to_string_lossy()})).unwrap();
    }
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Nothing is removed by a backup that was not made, or by one made to some other folder for
/// once: the library's own backups are where they are, and only a new one there makes room.
#[test]
fn only_a_backup_that_was_made_to_the_librarys_folder_removes_old_ones() {
    let (dir, other, blocked) = (temp_dir("only"), temp_dir("only-other"), temp_dir("only-blocked"));
    let (mut s, now) = open(&dir);
    for t in ["2026-01-10T09:00:00", "2026-01-12T09:00:00", "2026-01-14T09:00:00", "2026-01-16T09:00:00", "2026-01-18T09:00:00"] {
        back_up_at(&mut s, &now, t);
    }
    prefs(&mut s, json!({"keepDays": 30}));
    *now.lock().unwrap() = "2027-01-01T09:00:00".into();
    // elsewhere, for once
    let r = s.execute("library.backup", &json!({"dir": other.to_string_lossy()})).unwrap();
    assert!(r["error"].is_null() && r["removed"].as_array().is_none_or(Vec::is_empty), "{r}");
    // and one that fails (its folder can't be made)
    std::fs::create_dir_all(&blocked).unwrap();
    std::fs::write(blocked.join("a-file"), b"x").unwrap();
    let r = s.execute("library.backup", &json!({"dir": blocked.join("a-file").to_string_lossy()})).unwrap();
    assert!(r["error"].is_string(), "{r}");
    assert_eq!(backups(&dir.join("Backups")).len(), 5, "all still there");
    // (one made there does make room)
    let r = back_up_at(&mut s, &now, "2027-01-02T09:00:00");
    assert_eq!(r["removed"].as_array().map(Vec::len), Some(3), "{r}");
    drop(s);
    for d in [dir, other, blocked] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Given two libraries whose backups go to the same folder, when one of them removes its old
/// backups, then the other's stay, however old: a library removes only what is its own.
#[test]
fn a_library_removes_only_its_own_backups() {
    let (a, b, shared) = (temp_dir("own-a"), temp_dir("own-b"), temp_dir("own-shared"));
    let (mut s, now) = open(&a);
    prefs(&mut s, json!({"dir": shared.to_string_lossy()}));
    back_up_at(&mut s, &now, "2026-01-10T09:00:00");
    s.close_library().unwrap();
    s.open_library(&b, true).unwrap();
    prefs(&mut s, json!({"dir": shared.to_string_lossy(), "keepDays": 30}));
    for t in ["2026-01-11T09:00:00", "2026-01-12T09:00:00", "2026-01-13T09:00:00", "2026-01-14T09:00:00"] {
        back_up_at(&mut s, &now, t);
    }
    let r = back_up_at(&mut s, &now, "2028-01-11T09:00:00");
    assert_eq!(r["removed"], json!(["2026-01-11 09.00.00 UTC", "2026-01-12 09.00.00 UTC"]), "{r}");
    assert_eq!(
        backups(&shared),
        ["2026-01-10 09.00.00 UTC", "2026-01-13 09.00.00 UTC", "2026-01-14 09.00.00 UTC", "2028-01-11 09.00.00 UTC"],
        "A's of 2026 stays, older than all of B's"
    );
    drop(s);
    for d in [a, b, shared] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Given a library whose backups go to a folder outside it, when the library is deleted and a
/// new one is made in the same place, then the new one's backups never remove the old one's:
/// they are of another library, though it was at the same path.
#[test]
fn a_library_made_anew_in_the_same_place_leaves_the_old_ones_backups() {
    let (dir, shared) = (temp_dir("anew"), temp_dir("anew-shared"));
    let (mut s, now) = open(&dir);
    prefs(&mut s, json!({"dir": shared.to_string_lossy()}));
    let old = ["2026-01-10 09.00.00 UTC", "2026-01-11 09.00.00 UTC", "2026-01-12 09.00.00 UTC", "2026-01-13 09.00.00 UTC"];
    for t in ["2026-01-10T09:00:00", "2026-01-11T09:00:00", "2026-01-12T09:00:00", "2026-01-13T09:00:00"] {
        back_up_at(&mut s, &now, t);
    }
    let first = s.library_id.clone().expect("a library that was backed up has an id");
    s.close_library().unwrap();
    drop(s);
    // the id is the library's, saved with it
    let (s, _) = open(&dir);
    assert_eq!(s.library_id.as_ref(), Some(&first));
    drop(s);

    std::fs::remove_dir_all(&dir).unwrap();
    let (mut s, now) = open(&dir);
    prefs(&mut s, json!({"dir": shared.to_string_lossy(), "keepDays": 30}));
    for t in ["2028-01-10T09:00:00", "2028-01-11T09:00:00", "2028-01-12T09:00:00", "2028-01-13T09:00:00"] {
        let r = back_up_at(&mut s, &now, t);
        assert_eq!(r["removed"], json!([]), "{r}");
    }
    assert_ne!(s.library_id.as_ref(), Some(&first), "another library");
    let kept = backups(&shared);
    assert!(old.iter().all(|b| kept.iter().any(|k| k == b)), "{kept:?}");
    assert_eq!(kept.len(), 8);
    // and asked outright, none of them is this library's to remove
    let r = s.execute("library.pruneBackups", &json!({"keepDays": 1})).unwrap();
    assert_eq!(r["outdated"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(r["outdated"][0]["name"], json!("2028-01-10 09.00.00 UTC"), "its own oldest, and none of 2026: {r}");
    drop(s);
    for d in [dir, shared] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Given a clock that was ten years ahead when one backup was made, then that backup costs no
/// more than the rule allows (the three newest stay), and once the clock is right again the
/// backup dated in years to come is neither removed nor counted as the newest: the backups made
/// since are kept as if it were not there.
#[test]
fn a_clock_that_was_wrong_costs_no_more_than_the_oldest_backups() {
    let dir = temp_dir("clock");
    let (mut s, now) = open(&dir);
    for t in ["2026-01-10T09:00:00", "2026-01-20T09:00:00", "2026-01-30T09:00:00"] {
        back_up_at(&mut s, &now, t);
    }
    prefs(&mut s, json!({"keepDays": 30}));
    // the clock jumps ahead: every backup looks ten years old
    let r = back_up_at(&mut s, &now, "2036-02-01T09:00:00");
    assert_eq!(r["removed"], json!(["2026-01-10 09.00.00 UTC"]), "only the oldest, not all of them: {r}");
    // and is right again
    let r = back_up_at(&mut s, &now, "2026-02-05T09:00:00");
    assert_eq!(r["removed"], json!([]), "{r}");
    let r = back_up_at(&mut s, &now, "2026-04-05T09:00:00");
    assert_eq!(r["removed"], json!(["2026-01-20 09.00.00 UTC"]), "the fourth newest of the ones not yet to come: {r}");
    let r = back_up_at(&mut s, &now, "2026-06-05T09:00:00");
    assert_eq!(r["removed"], json!(["2026-01-30 09.00.00 UTC"]), "{r}");
    assert_eq!(
        backups(&dir.join("Backups")),
        ["2026-02-05 09.00.00 UTC", "2026-04-05 09.00.00 UTC", "2026-06-05 09.00.00 UTC", "2036-02-01 09.00.00 UTC"],
        "three of now, and the one dated in years to come"
    );
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given an old backup the person set aside under a name of their own, then it is theirs: it
/// is not among the backups the library no longer keeps, and stays when those are removed.
#[test]
fn a_backup_set_aside_under_another_name_is_kept() {
    let dir = temp_dir("aside");
    let (mut s, now) = open(&dir);
    for t in ["2026-01-10T09:00:00", "2026-01-11T09:00:00", "2026-01-12T09:00:00", "2026-01-13T09:00:00", "2026-01-14T09:00:00"] {
        back_up_at(&mut s, &now, t);
    }
    let folder = dir.join("Backups");
    std::fs::rename(folder.join("2026-01-10 09.00.00 UTC"), folder.join("before the big edit")).unwrap();
    *now.lock().unwrap() = "2027-01-01T09:00:00".into();
    let r = s.execute("library.pruneBackups", &json!({"keepDays": 30, "remove": true})).unwrap();
    assert_eq!(r["removed"], json!(["2026-01-11 09.00.00 UTC"]), "{r}");
    assert_eq!(r["keptAtLeast"], json!(3));
    assert!(folder.join("before the big edit").join("backup.json").is_file());
    // (and it is a backup still: it can be checked, and restored from)
    s.execute("library.checkBackup", &json!({"dir": folder.join("before the big edit").to_string_lossy()})).unwrap();
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a restore of the library that is under way (it reads a backup, and then puts another
/// library in this one's place), then no backup is made and none is removed until it is over.
#[test]
fn nothing_is_backed_up_or_removed_while_the_library_is_restored() {
    use std::sync::atomic::Ordering;
    let dir = temp_dir("restoring");
    let (mut s, now) = open(&dir);
    for t in ["2026-01-10T09:00:00", "2026-01-11T09:00:00", "2026-01-12T09:00:00", "2026-01-13T09:00:00", "2026-01-14T09:00:00"] {
        back_up_at(&mut s, &now, t);
    }
    let folder = dir.join("Backups");
    *now.lock().unwrap() = "2027-01-01T09:00:00".into();
    s.restore_hold.store(true, Ordering::SeqCst);
    // from the oldest: the one that would go first
    let from = folder.join("2026-01-10 09.00.00 UTC");
    let r = s.execute("library.restore", &json!({"dir": from.to_string_lossy(), "background": true})).unwrap();
    assert_eq!(r["running"], json!(true), "{r}");
    for (cmd, p) in [("library.pruneBackups", json!({"keepDays": 30, "remove": true})), ("library.backup", json!({}))] {
        let e = s.execute(cmd, &p).unwrap_err().to_string();
        assert!(e.contains("a restore of this library is under way"), "{cmd}: {e}");
    }
    assert_eq!(backups(&folder).len(), 5);
    s.restore_hold.store(false, Ordering::SeqCst);
    let mut done = Value::Null;
    for _ in 0..5000 {
        done = s.execute("library.restoreStatus", &json!({})).unwrap();
        if done["running"] != json!(true) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(done["error"].is_null() && !done["restored"].is_null(), "{done}");
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// `library.pruneBackups` says which backups are older than the library keeps them (or than
/// `keepDays` given for once) without removing any; with `remove` it removes them. A library
/// that keeps its backups forever has nothing to say unless a time is given.
#[test]
fn the_backups_that_would_go_can_be_asked_for_first() {
    let dir = temp_dir("ask");
    let (mut s, now) = open(&dir);
    for t in ["2026-01-10T09:00:00", "2026-02-10T09:00:00", "2026-03-10T09:00:00", "2026-04-10T09:00:00", "2026-06-10T09:00:00"] {
        back_up_at(&mut s, &now, t);
    }
    *now.lock().unwrap() = "2026-06-20T09:00:00".into();
    let e = s.execute("library.pruneBackups", &json!({})).unwrap_err().to_string();
    assert!(e.contains("keepDays"), "{e}");
    let r = s.execute("library.pruneBackups", &json!({"keepDays": 60})).unwrap();
    let outdated: Vec<&str> = r["outdated"].as_array().unwrap().iter().map(|b| b["name"].as_str().unwrap()).collect();
    assert_eq!(outdated, ["2026-01-10 09.00.00 UTC", "2026-02-10 09.00.00 UTC"], "{r}");
    assert_eq!(r["removed"], json!([]));
    assert_eq!(backups(&dir.join("Backups")).len(), 5, "asked, not removed");

    prefs(&mut s, json!({"keepDays": 150}));
    let r = s.execute("library.pruneBackups", &json!({"remove": true})).unwrap();
    assert_eq!(r["removed"], json!(["2026-01-10 09.00.00 UTC"]), "{r}");
    assert_eq!(backups(&dir.join("Backups")).len(), 4);
    assert!(s.activity.list().is_empty());
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a backup restored into another folder, then the library there is another library: it
/// has its own id from its first backup on, so that the two never take each other's backups
/// for their own. The open library keeps its id.
#[test]
fn a_library_restored_into_another_folder_is_another_library() {
    let (dir, to) = (temp_dir("fresh"), temp_dir("fresh-to"));
    let (mut s, now) = open(&dir);
    let made = back_up_at(&mut s, &now, "2026-01-10T09:00:00");
    let id = s.library_id.clone().expect("an id");
    assert_eq!(made["report"]["libraryId"], json!(id));
    s.execute("library.restore", &json!({"dir": made["report"]["dir"], "to": to.to_string_lossy()})).unwrap();
    assert_eq!(s.library_id.as_ref(), Some(&id));
    drop(s);
    let (mut there, now) = open(&to);
    assert_eq!(there.library_id, None, "not the id of the library the backup was made of");
    back_up_at(&mut there, &now, "2026-01-11T09:00:00");
    assert!(there.library_id.as_ref().is_some_and(|other| *other != id));
    drop(there);
    for d in [dir, to] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// How long backups are kept is the library's choice now, not the backup's from then: a
/// restore keeps it (and the library is the same library still: its id stays).
#[test]
fn a_restore_keeps_how_long_backups_are_kept() {
    let dir = temp_dir("restore-keeps");
    let (mut s, now) = open(&dir);
    let made = back_up_at(&mut s, &now, "2026-01-10T09:00:00");
    let id = s.library_id.clone();
    prefs(&mut s, json!({"keepDays": 90}));
    s.execute("library.restore", &json!({"dir": made["report"]["dir"]})).unwrap();
    assert_eq!(prefs(&mut s, json!({}))["keepDays"], json!(90));
    assert_eq!(s.library_id, id);
    s.close_library().unwrap();
    drop(s);
    let (mut s, _) = open(&dir);
    assert_eq!(prefs(&mut s, json!({}))["keepDays"], json!(90), "and it is saved");
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given an old backup that something else was put into, when the library's old backups go,
/// then that one stays with everything in it, and the backup says it could not be removed and
/// why; the others go as they would.
#[test]
fn an_old_backup_that_holds_something_else_stays_and_is_named() {
    let dir = temp_dir("holds");
    let (mut s, now) = open(&dir);
    for t in ["2026-01-10T09:00:00", "2026-01-11T09:00:00", "2026-01-12T09:00:00", "2026-01-13T09:00:00"] {
        back_up_at(&mut s, &now, t);
    }
    let folder = dir.join("Backups");
    std::fs::write(folder.join("2026-01-10 09.00.00 UTC").join("notes.txt"), b"mine").unwrap();
    prefs(&mut s, json!({"keepDays": 30}));
    let r = back_up_at(&mut s, &now, "2027-01-01T09:00:00");
    assert_eq!(r["removed"], json!(["2026-01-11 09.00.00 UTC"]), "{r}");
    let kept = r["notRemoved"].as_array().unwrap();
    assert!(kept.len() == 1 && kept[0].as_str().is_some_and(|why| why.starts_with("2026-01-10 09.00.00 UTC") && why.contains("notes.txt")), "{r}");
    assert!(r["error"].is_null() && r["report"].is_object(), "the backup itself was made: {r}");
    assert!(folder.join("2026-01-10 09.00.00 UTC").join("notes.txt").is_file());
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a library whose folder for backups is one backups may not go to (set by hand in its
/// preferences file), then nothing is looked for or removed there.
#[test]
fn old_backups_are_not_looked_for_where_backups_may_not_go() {
    let dir = temp_dir("not-there");
    let (mut s, now) = open(&dir);
    back_up_at(&mut s, &now, "2026-01-10T09:00:00");
    s.backup_dir = Some(dir.join("segments"));
    let e = s.execute("library.pruneBackups", &json!({"keepDays": 1, "remove": true})).unwrap_err().to_string();
    assert!(e.contains("can't be used"), "{e}");
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}
