//! The backup schedule: a library can be backed up by itself while it is open (once a day, a
//! week or a month), and its backup on closing is a choice of its own (never, ask, always). Both
//! are off unless chosen, saved with the library, and set with `library.backupPreferences`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::Session;
use crate::backup_schedule::{BackupEvery, BackupOnClose};
use crate::cmd::backup;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-engine-schedule-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A library on disk in `dir`, and the clock its session goes by (set it to move time).
fn open(dir: &Path) -> (Session, Arc<Mutex<String>>) {
    let mut s = Session::new();
    let now = Arc::new(Mutex::new("2026-10-11T09:00:00".to_string()));
    let clock = now.clone();
    s.clock = Box::new(move || clock.lock().unwrap().clone());
    s.open_library(dir, true).unwrap();
    (s, now)
}

fn at(now: &Arc<Mutex<String>>, time: &str) {
    *now.lock().unwrap() = time.to_string();
}

fn prefs(s: &mut Session, p: Value) -> Value {
    s.execute("library.backupPreferences", &p).unwrap()
}

/// Let a backup that was started finish, and the session take note of it (as the app does
/// between commands).
fn finish(s: &mut Session) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while backup::status(s).is_some_and(|b| !b.finished.load(std::sync::atomic::Ordering::Relaxed)) {
        assert!(std::time::Instant::now() < deadline, "the backup did not finish");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    s.persist_if_dirty();
}

/// Whether a backup was due and was started (one that can't be started is its own scenario).
fn due_backup(s: &mut Session) -> bool {
    backup::back_up_if_due(s).unwrap()
}

fn backups_in(dir: &Path) -> usize {
    std::fs::read_dir(dir).map(|d| d.filter_map(|e| e.ok()).filter(|e| !e.file_name().to_string_lossy().ends_with(".partial")).count()).unwrap_or(0)
}

/// Given a library, then nothing backs it up by itself: both choices are off until made. When
/// they are made, they are answered back, saved with the library, and a choice that is none of
/// the ones there are is refused with the ones there are.
#[test]
fn the_schedule_is_off_until_chosen_and_is_saved_with_the_library() {
    let dir = temp_dir("prefs");
    let (mut s, _) = open(&dir);
    let p = prefs(&mut s, json!({}));
    assert_eq!((p["every"].as_str(), p["onClose"].as_str(), p["lastBackup"].is_null()), (Some("never"), Some("never"), true));
    assert!(!due_backup(&mut s), "nothing is due that was not asked for");

    let p = prefs(&mut s, json!({"every": "week", "onClose": "ask"}));
    assert_eq!((p["every"].as_str(), p["onClose"].as_str()), (Some("week"), Some("ask")));
    assert_eq!((s.backup_schedule.every, s.backup_schedule.on_close), (BackupEvery::Week, BackupOnClose::Ask));
    // one at a time leaves the other
    let p = prefs(&mut s, json!({"onClose": "always"}));
    assert_eq!((p["every"].as_str(), p["onClose"].as_str()), (Some("week"), Some("always")));
    for bad in [json!({"every": "fortnight"}), json!({"onClose": "sometimes"}), json!({"every": 7})] {
        let e = s.execute("library.backupPreferences", &bad).unwrap_err().to_string();
        assert!(e.contains("never") && (e.contains("day") || e.contains("ask")), "{bad}: {e}");
    }
    s.close_library().unwrap();
    drop(s);
    let (mut s, _) = open(&dir);
    let p = prefs(&mut s, json!({}));
    assert_eq!((p["every"].as_str(), p["onClose"].as_str()), (Some("week"), Some("always")));
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a library to be backed up once a day that never was, when the app looks, then a backup
/// is made in the background (it has its row in the activity stack) to the folder its backups
/// go to, and the library remembers when. It is not due again until a day has passed.
#[test]
fn a_library_is_backed_up_when_its_backup_is_due() {
    let dir = temp_dir("due");
    let (mut s, now) = open(&dir);
    prefs(&mut s, json!({"every": "day"}));
    assert!(due_backup(&mut s), "never backed up: due");
    assert!(!due_backup(&mut s), "one at a time");
    finish(&mut s);
    assert_eq!(backups_in(&dir.join("Backups")), 1);
    assert_eq!(prefs(&mut s, json!({}))["lastBackup"].as_str(), Some("2026-10-11T09:00:00"));
    assert!(s.activity.list().is_empty());

    at(&now, "2026-10-12T08:59:59");
    assert!(!due_backup(&mut s), "not yet a day");
    at(&now, "2026-10-12T09:00:00");
    assert!(due_backup(&mut s), "a day later");
    finish(&mut s);
    assert_eq!(backups_in(&dir.join("Backups")), 2);
    assert_eq!(prefs(&mut s, json!({}))["lastBackup"].as_str(), Some("2026-10-12T09:00:00"));

    // remembered with the library: opened again the same day, nothing is due
    s.close_library().unwrap();
    drop(s);
    let (mut s, now) = open(&dir);
    at(&now, "2026-10-12T18:00:00");
    assert!(!due_backup(&mut s));
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// A week is seven days and a month thirty, counted from the last backup.
#[test]
fn a_week_is_seven_days_and_a_month_thirty() {
    let dir = temp_dir("periods");
    let (mut s, now) = open(&dir);
    for (every, not_yet, due) in [("week", "2026-10-18T08:59:59", "2026-10-18T09:00:00"), ("month", "2026-11-10T08:59:59", "2026-11-10T09:00:00")] {
        at(&now, "2026-10-11T09:00:00");
        prefs(&mut s, json!({"every": "never"}));
        s.execute("library.backup", &json!({})).unwrap();
        s.persist_if_dirty();
        prefs(&mut s, json!({"every": every}));
        at(&now, not_yet);
        assert!(!due_backup(&mut s), "{every}: {not_yet}");
        at(&now, due);
        assert!(due_backup(&mut s), "{every}: {due}");
        finish(&mut s);
    }
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a backup made by hand (File ▸ Back Up Library, or an agent), then it counts: the
/// library was backed up, and the next one by itself is a period after that. A backup made to
/// some other folder for once does not count: the library's own backups are where they go.
#[test]
fn a_backup_made_by_hand_counts() {
    let (dir, other) = (temp_dir("by-hand"), temp_dir("by-hand-other"));
    let (mut s, now) = open(&dir);
    prefs(&mut s, json!({"every": "day"}));
    s.execute("library.backup", &json!({"dir": other.to_string_lossy()})).unwrap();
    s.persist_if_dirty();
    assert!(prefs(&mut s, json!({}))["lastBackup"].is_null(), "elsewhere, for once");
    s.execute("library.backup", &json!({})).unwrap();
    s.persist_if_dirty();
    assert_eq!(prefs(&mut s, json!({}))["lastBackup"].as_str(), Some("2026-10-11T09:00:00"));
    at(&now, "2026-10-11T20:00:00");
    assert!(!due_backup(&mut s));
    drop(s);
    for d in [dir, other] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Given a backup by itself that fails (the folder for backups can't be made), then it says so
/// once (`library.backupStatus` has the error), the library is not counted as backed up, and it
/// is tried again an hour later, not on every look.
#[test]
fn a_backup_that_fails_is_tried_again_later_not_at_once() {
    let (dir, outside) = (temp_dir("fails"), temp_dir("fails-outside"));
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("a-file"), b"x").unwrap();
    let (mut s, now) = open(&dir);
    prefs(&mut s, json!({"every": "day", "dir": outside.join("a-file").to_string_lossy()}));
    assert!(due_backup(&mut s));
    finish(&mut s);
    let status = s.execute("library.backupStatus", &json!({})).unwrap();
    assert!(status["error"].as_str().is_some() && status["report"].is_null(), "{status}");
    assert!(prefs(&mut s, json!({}))["lastBackup"].is_null());
    at(&now, "2026-10-11T09:59:59");
    assert!(!due_backup(&mut s), "not on every look");
    at(&now, "2026-10-11T10:00:00");
    assert!(due_backup(&mut s), "an hour later");
    finish(&mut s);
    drop(s);
    for d in [dir, outside] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Nothing is backed up by itself that can't be: a library that is not a folder on disk, or
/// while a backup of it runs already.
#[test]
fn a_library_in_memory_is_never_due() {
    let mut s = Session::with_demo();
    s.backup_schedule.every = BackupEvery::Day;
    assert!(!due_backup(&mut s));
    assert!(s.activity.list().is_empty());
}

/// Given a backup by itself that is due and can't even be started (the folder set for backups
/// is one the library may not use), then that is said (the app shows it), and it is not tried,
/// or said, again until an hour later.
#[test]
fn a_backup_that_cannot_be_started_says_so_and_waits_an_hour() {
    let dir = temp_dir("no-start");
    let (mut s, now) = open(&dir);
    prefs(&mut s, json!({"every": "day"}));
    // (as a preferences file from before the rule about folders, or one edited by hand)
    s.backup_dir = Some(s.library.as_ref().unwrap().dir.join("segments"));
    let e = backup::back_up_if_due(&mut s).unwrap_err();
    assert!(e.contains("can't be used"), "{e}");
    assert_eq!(backup::back_up_if_due(&mut s), Ok(false), "not on every look");
    at(&now, "2026-10-11T10:00:00");
    assert!(backup::back_up_if_due(&mut s).is_err(), "an hour later");
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a library that was backed up, when its backups are to go to another folder, then it
/// has not been backed up there: a backup is due at once, and Settings no longer names the old
/// one as the last. The same when the folder its backups were in is gone.
#[test]
fn the_last_backup_is_the_last_one_in_the_folder_backups_go_to() {
    let (dir, other) = (temp_dir("moved"), temp_dir("moved-other"));
    let (mut s, _) = open(&dir);
    prefs(&mut s, json!({"every": "month"}));
    s.execute("library.backup", &json!({})).unwrap();
    s.persist_if_dirty();
    assert!(!due_backup(&mut s));
    let p = prefs(&mut s, json!({"dir": other.to_string_lossy()}));
    assert!(p["lastBackup"].is_null(), "{p}");
    assert!(due_backup(&mut s), "not backed up there yet");
    finish(&mut s);
    assert_eq!(backups_in(&other), 1);
    assert!(!due_backup(&mut s));
    // the folder is gone (a drive that was wiped, a folder thrown away): the library is not
    // backed up any more, which the next look finds, says (`lastBackup`) and puts right
    std::fs::remove_dir_all(&other).unwrap();
    s.backup_tried = None;
    assert!(prefs(&mut s, json!({}))["lastBackup"].is_string(), "not known before the look");
    assert!(due_backup(&mut s), "its backups are gone: due");
    finish(&mut s);
    assert_eq!(backups_in(&other), 1);
    // (and had the backup not been made, it would not be named as the last)
    std::fs::remove_dir_all(&other).unwrap();
    std::fs::write(&other, b"not a folder").unwrap();
    s.backup_tried = None;
    assert!(due_backup(&mut s));
    finish(&mut s);
    assert!(prefs(&mut s, json!({}))["lastBackup"].is_null(), "gone, and the new one failed");
    let _ = std::fs::remove_file(&other);
    drop(s);
    for d in [dir, other] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Given a backup made while the clock was wrong (its time is yet to come), then the next one
/// is not put off until that date: it is due.
#[test]
fn a_last_backup_in_the_future_does_not_put_the_next_one_off() {
    let dir = temp_dir("future");
    let (mut s, now) = open(&dir);
    prefs(&mut s, json!({"every": "week"}));
    at(&now, "2031-01-01T00:00:00");
    s.execute("library.backup", &json!({})).unwrap();
    s.persist_if_dirty();
    at(&now, "2026-10-11T09:00:00");
    assert!(due_backup(&mut s));
    finish(&mut s);
    assert_eq!(prefs(&mut s, json!({}))["lastBackup"].as_str(), Some("2026-10-11T09:00:00"));
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a library with a schedule, when an older backup of it (from before the schedule was
/// chosen) is restored, then the schedule stays as it is now, like the folder backups go to: it
/// is the library's choice, not something the backup holds.
#[test]
fn a_restore_keeps_the_schedule() {
    let dir = temp_dir("restore");
    let (mut s, _) = open(&dir);
    let made = s.execute("library.backup", &json!({})).unwrap();
    let from = made["report"]["dir"].as_str().unwrap().to_string();
    s.persist_if_dirty();
    prefs(&mut s, json!({"every": "week", "onClose": "always", "keepDays": 90}));
    let last = prefs(&mut s, json!({}))["lastBackup"].clone();
    assert!(last.is_string());
    s.execute("library.restore", &json!({"dir": from})).unwrap();
    let p = prefs(&mut s, json!({}));
    assert_eq!((p["every"].as_str(), p["onClose"].as_str()), (Some("week"), Some("always")));
    assert_eq!(p["lastBackup"], last);
    assert_eq!(p["keepDays"], json!(90), "and how long backups are kept");
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
}

/// Given a preferences file with a choice this version doesn't know (a later version's), then
/// that choice is the usual one and the rest of the file is read: the preferences are not
/// thrown away for it.
#[test]
fn a_choice_this_version_does_not_know_costs_no_other_preference() {
    let (dir, other) = (temp_dir("unknown"), temp_dir("unknown-other"));
    let (mut s, _) = open(&dir);
    prefs(&mut s, json!({"dir": other.to_string_lossy(), "onClose": "ask"}));
    s.close_library().unwrap();
    drop(s);
    let file = dir.join("prefs.json");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"onClose\": \"ask\""), "{text}");
    std::fs::write(&file, text.replace("\"onClose\": \"ask\"", "\"onClose\": \"ask\", \"every\": \"fortnight\"")).unwrap();
    let (mut s, _) = open(&dir);
    let p = prefs(&mut s, json!({}));
    assert_eq!((p["every"].as_str(), p["onClose"].as_str()), (Some("never"), Some("ask")));
    assert_eq!(p["dir"].as_str(), Some(&*other.to_string_lossy()), "the rest of the file was read");
    assert!(s.take_library_warnings().iter().all(|w| !w.contains("prefs.json")));
    drop(s);
    for d in [dir, other] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Given a backup of the library's own folder that is running when the folder backups go to is
/// changed, then it ends in the old folder and does not count as a backup in the new one: the new
/// folder has none, so one is due there at once.
#[test]
fn a_backup_that_ends_after_the_folder_changed_does_not_count_in_the_new_folder() {
    let (dir, other) = (temp_dir("changed-while-running"), temp_dir("changed-while-running-other"));
    let (mut s, _) = open(&dir);
    prefs(&mut s, json!({"every": "month"}));
    s.backup_hold.store(true, std::sync::atomic::Ordering::SeqCst);
    s.execute("library.backup", &json!({"background": true})).unwrap();
    let p = prefs(&mut s, json!({"dir": other.to_string_lossy()}));
    assert!(p["lastBackup"].is_null(), "{p}");
    s.backup_hold.store(false, std::sync::atomic::Ordering::SeqCst);
    finish(&mut s);
    assert!(prefs(&mut s, json!({}))["lastBackup"].is_null(), "the backup is in the old folder");
    assert!(due_backup(&mut s), "nothing is backed up in the new folder");
    finish(&mut s);
    assert_eq!(backups_in(&other), 1);
    drop(s);
    for d in [dir, other] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Given a backup in the background that ends, when nobody but a script asks how it went
/// (`library.backupStatus`: no frame loop calls `persist_if_dirty`), then it counts as the last
/// backup and is saved with the library, so the next session does not find one due again.
#[test]
fn a_backup_in_the_background_is_noted_when_its_status_is_asked() {
    let dir = temp_dir("noted-by-status");
    let (mut s, _) = open(&dir);
    prefs(&mut s, json!({"every": "month"}));
    s.execute("library.backup", &json!({"background": true})).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while backup::status(&s).is_some_and(|b| !b.finished.load(std::sync::atomic::Ordering::Relaxed)) {
        assert!(std::time::Instant::now() < deadline, "the backup did not finish");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let status = s.execute("library.backupStatus", &json!({})).unwrap();
    assert_eq!(status["running"], json!(false), "{status}");
    assert_eq!(prefs(&mut s, json!({}))["lastBackup"].as_str(), Some("2026-10-11T09:00:00"));
    drop(s);
    let (mut again, _) = open(&dir);
    assert_eq!(prefs(&mut again, json!({}))["lastBackup"].as_str(), Some("2026-10-11T09:00:00"), "saved with the library");
    drop(again);
    let _ = std::fs::remove_dir_all(&dir);
}
