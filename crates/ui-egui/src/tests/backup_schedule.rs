//! The backup schedule in the app: chosen in Settings ▸ General ▸ Backups, a backup that is due
//! is made by itself while the library is open, and quitting backs the library up first where
//! that was chosen (asking or not), then quits.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::panels::notices::may_close;
use crate::{LightcraftApp, QuitBackup, QuitPrompt, Services};

const T: Duration = Duration::from_secs(10);

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-backupschedule-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The app with a library on disk in `dir/library`.
fn app_with_a_library_in(dir: &Path) -> Headless {
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("library"), true).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1300.0, 820.0], 1.0);
    h.step();
    h
}

fn prefs(h: &mut Headless, p: Value) -> Value {
    h.app.run("library.backupPreferences", p).unwrap()
}

/// Click a widget and let the synthetic events (move, press, release) play out.
fn click(h: &mut Headless, id: &str) {
    // a window that just appeared is placed on its second frame
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    for _ in 0..4 {
        h.step();
    }
}

fn has(h: &mut Headless, id: &str) -> bool {
    h.request("ui.widgets", json!({"filter": id}), T)["result"].as_array().is_some_and(|w| w.iter().any(|w| w["id"] == id))
}

fn backups(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("library/Backups"))
        .map(|d| d.filter_map(|e| e.ok()).filter(|e| !e.file_name().to_string_lossy().ends_with(".partial")).count())
        .unwrap_or(0)
}

/// Given Settings ▸ General ▸ Backups, when "Once a week" and "Ask" are clicked, then the
/// library is to be backed up by itself once a week and asks when LightCraft quits; "Never"
/// turns each off again.
#[test]
fn the_schedule_is_chosen_in_settings() {
    let dir = temp("settings");
    let mut h = app_with_a_library_in(&dir);
    assert_eq!(h.request("ui.key", json!({"key": ",", "cmd": true}), T)["ok"], true);
    for id in ["settingsBackupEvery-0", "settingsBackupEvery-3", "settingsBackupOnClose-0", "settingsBackupOnClose-2"] {
        assert!(has(&mut h, &format!("button:{id}")), "{id}");
    }
    click(&mut h, "button:settingsBackupEvery-2");
    click(&mut h, "button:settingsBackupOnClose-1");
    let p = prefs(&mut h, json!({}));
    assert_eq!((p["every"].as_str(), p["onClose"].as_str()), (Some("week"), Some("ask")));
    click(&mut h, "button:settingsBackupEvery-0");
    click(&mut h, "button:settingsBackupOnClose-0");
    let p = prefs(&mut h, json!({}));
    assert_eq!((p["every"].as_str(), p["onClose"].as_str()), (Some("never"), Some("never")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a library to be backed up once a day that never was, when the app runs, then a backup
/// is made by itself, a toast says where it went, and no second one follows.
#[test]
fn a_backup_that_is_due_is_made_while_the_library_is_open() {
    let dir = temp("due");
    let mut h = app_with_a_library_in(&dir);
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(backups(&dir), 0, "nothing was chosen: nothing is made");
    prefs(&mut h, json!({"every": "day"}));
    assert!(h.step_until(T, |h| h.app.session.last_backup.is_some()), "made, and noted");
    assert_eq!(backups(&dir), 1);
    assert!(h.step_until(T, |h| h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("Backed up the library"))), "{:?}", h.app.ui.toast);
    for _ in 0..20 {
        h.step();
    }
    assert_eq!(backups(&dir), 1, "not due again");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a library that asks for a backup when LightCraft quits, when the person quits, then
/// they are asked. "Back Up" backs the library up and then quits; nothing else is asked.
#[test]
fn quitting_asks_for_a_backup_and_quits_once_it_is_made() {
    let dir = temp("ask");
    let mut h = app_with_a_library_in(&dir);
    prefs(&mut h, json!({"onClose": "ask"}));
    assert!(!may_close(&mut h.app), "asked first");
    h.step();
    assert!(matches!(h.app.quit_prompt, Some(QuitPrompt::BackUp(_))), "{:?}", h.app.quit_prompt);
    for b in ["button:quitBackUp", "button:quitSkipBackup", "button:quitCancel"] {
        assert!(has(&mut h, b), "{b}");
    }
    click(&mut h, "button:quitBackUp");
    // (the quit itself is the window closing: what is seen here is that nothing holds it back)
    assert!(h.step_until(T, |h| h.app.quit_backup == QuitBackup::Settled), "the backup is made: {:?}", h.app.quit_prompt);
    assert_eq!(backups(&dir), 1);
    assert!(h.app.quit_prompt.is_none() && may_close(&mut h.app));
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Skip" quits without a backup. "Cancel" goes back to work, and the next quit asks again.
#[test]
fn a_backup_on_quitting_can_be_skipped_or_the_quit_cancelled() {
    let dir = temp("skip");
    let mut h = app_with_a_library_in(&dir);
    prefs(&mut h, json!({"onClose": "ask"}));
    assert!(!may_close(&mut h.app));
    h.step();
    click(&mut h, "button:quitCancel");
    assert!(h.app.quit_prompt.is_none() && h.app.quit_backup == QuitBackup::Undecided, "{:?} {:?}", h.app.quit_prompt, h.app.quit_backup);
    h.step();

    assert!(!may_close(&mut h.app), "asked again");
    h.step();
    click(&mut h, "button:quitSkipBackup");
    assert!(h.app.quit_backup == QuitBackup::Settled && h.app.quit_prompt.is_none(), "{:?}", h.app.quit_prompt);
    assert_eq!(backups(&dir), 0);
    assert!(may_close(&mut h.app), "the window may close: not asked twice");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a library that is always backed up when LightCraft quits, when the person quits, then
/// it is backed up without a question (the prompt says so and offers to quit without waiting),
/// and LightCraft quits when the backup is made.
#[test]
fn quitting_always_backs_up_first_where_that_was_chosen() {
    let dir = temp("always");
    let mut h = app_with_a_library_in(&dir);
    prefs(&mut h, json!({"onClose": "always"}));
    assert!(!may_close(&mut h.app), "the backup first");
    assert!(matches!(h.app.quit_prompt, Some(QuitPrompt::BackingUp(_))), "{:?}", h.app.quit_prompt);
    assert!(h.step_until(T, |h| h.app.quit_backup == QuitBackup::Settled), "the backup is made: {:?}", h.app.quit_prompt);
    assert_eq!(backups(&dir), 1);
    assert!(h.app.quit_prompt.is_none() && may_close(&mut h.app));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the backup on quitting can't be made, then LightCraft does not quit as if it had been:
/// it says why, and the person decides (the next quit asks again).
#[test]
fn a_backup_on_quitting_that_fails_does_not_quit() {
    let dir = temp("fails");
    let blocked = dir.join("a-file");
    std::fs::write(&blocked, b"x").unwrap();
    let mut h = app_with_a_library_in(&dir);
    prefs(&mut h, json!({"onClose": "always", "dir": blocked.to_string_lossy()}));
    assert!(!may_close(&mut h.app));
    assert!(h.step_until(T, |h| h.app.quit_prompt.is_none()), "the wait is over");
    assert_eq!(h.app.quit_backup, QuitBackup::Undecided, "not quit: the next quit starts over");
    assert!(h.step_until(T, |h| h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("not backed up"))), "{:?}", h.app.ui.toast);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library that is not a folder on disk is not asked about: there is no backup to make.
#[test]
fn a_library_in_memory_quits_without_a_question() {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1300.0, 820.0], 1.0);
    h.app.session.backup_schedule.on_close = lightcraft_engine::backup_schedule::BackupOnClose::Ask;
    h.step();
    assert!(may_close(&mut h.app));
}
