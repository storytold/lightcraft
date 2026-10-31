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

/// A scratch folder, removed when the test ends (also when it fails).
struct Temp(PathBuf);

impl std::ops::Deref for Temp {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for Temp {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> Temp {
    let dir = std::env::temp_dir().join(format!("lc-ui-backupschedule-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Temp(dir)
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
}

/// Given a library to be backed up once a day that never was, when the app a person has open
/// runs, then a backup is made by itself, a toast says where it went, and no second one follows.
#[test]
fn a_backup_that_is_due_is_made_while_the_library_is_open() {
    let dir = temp("due");
    let mut h = app_with_a_library_in(&dir);
    // (as the app a person has open: the headless host does not back up by itself)
    h.app.backs_up_by_itself = true;
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
}

/// Given the same library in a headless session (a snapshot, an agent's), then nothing is backed
/// up by itself: the session may end moments later.
#[test]
fn a_headless_session_does_not_back_up_by_itself() {
    let dir = temp("headless");
    let mut h = app_with_a_library_in(&dir);
    assert!(!h.app.backs_up_by_itself);
    prefs(&mut h, json!({"every": "day"}));
    for _ in 0..40 {
        h.step();
    }
    assert_eq!(backups(&dir), 0);
    assert!(h.app.session.last_backup.is_none() && h.app.session.backup_tried.is_none());
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
    assert!(h.step_until(T, |h| h.quit_requested()), "quits once the backup is made: {:?}", h.app.quit_prompt);
    assert_eq!(backups(&dir), 1);
    assert!(h.app.quit_prompt.is_none() && may_close(&mut h.app));
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
    assert!(h.step_until(T, |h| h.quit_requested()) && h.app.quit_prompt.is_none(), "{:?}", h.app.quit_prompt);
    assert_eq!(backups(&dir), 0);
    assert!(may_close(&mut h.app), "the window may close: not asked twice");
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
    assert!(h.step_until(T, |h| h.quit_requested()), "quits once the backup is made: {:?}", h.app.quit_prompt);
    assert_eq!(backups(&dir), 1);
}

/// Given the backup on quitting can't be made (the folder for backups can't be), then LightCraft
/// does not quit as if it had been, and is not kept from quitting either: it says why and asks.
/// Cancel goes back to work (the next quit starts over); Quit Anyway quits without the backup.
#[test]
fn a_backup_on_quitting_that_fails_asks_whether_to_quit_anyway() {
    let dir = temp("fails");
    let blocked = dir.join("a-file");
    std::fs::write(&blocked, b"x").unwrap();
    let mut h = app_with_a_library_in(&dir);
    prefs(&mut h, json!({"onClose": "always", "dir": blocked.to_string_lossy()}));
    assert!(!may_close(&mut h.app));
    assert!(h.step_until(T, |h| matches!(h.app.quit_prompt, Some(QuitPrompt::BackupFailed(_)))), "{:?}", h.app.quit_prompt);
    assert!(h.app.quit_prompt.as_ref().unwrap().text().contains("not backed up"));
    assert!(!h.quit_requested(), "not quit");
    click(&mut h, "button:quitCancel");
    assert!(h.app.quit_prompt.is_none() && h.app.quit_backup == QuitBackup::Undecided && !h.quit_requested());

    // again, and this time quit all the same
    assert!(!may_close(&mut h.app));
    assert!(h.step_until(T, |h| matches!(h.app.quit_prompt, Some(QuitPrompt::BackupFailed(_)))), "{:?}", h.app.quit_prompt);
    click(&mut h, "button:quitAnyway");
    assert!(h.step_until(T, |h| h.quit_requested()), "{:?}", h.app.quit_prompt);
    assert_eq!(backups(&dir), 0);
}

/// Given a backup of the library is running already when the person quits (one made by hand, or
/// the one that was due), then it is the backup the quit waits for: it is not stopped as a task
/// in the way, and no second one is started.
#[test]
fn a_backup_that_runs_already_is_the_one_quitting_waits_for() {
    use std::sync::atomic::Ordering;
    let dir = temp("already");
    let mut h = app_with_a_library_in(&dir);
    prefs(&mut h, json!({"onClose": "always"}));
    h.app.session.backup_hold.store(true, Ordering::SeqCst);
    h.app.run("library.backup", json!({"background": true})).unwrap();
    assert!(!may_close(&mut h.app));
    assert!(matches!(h.app.quit_prompt, Some(QuitPrompt::BackingUp(_))), "waited for, not asked about as a task: {:?}", h.app.quit_prompt);
    let job = h.app.session.backup.clone().unwrap();
    assert!(!job.cancel.load(Ordering::Relaxed), "not stopped");
    h.app.session.backup_hold.store(false, Ordering::SeqCst);
    assert!(h.step_until(T, |h| h.quit_requested()), "{:?}", h.app.quit_prompt);
    assert_eq!(backups(&dir), 1, "the one backup");
}

/// "Quit Without Waiting" stops the backup and quits; nothing half made is left as a backup.
/// "Cancel" goes back to work: the backup is still made, and LightCraft does not quit by itself.
#[test]
fn the_wait_for_the_backup_can_be_cut_short_or_called_off() {
    use std::sync::atomic::Ordering;
    let dir = temp("wait");
    let mut h = app_with_a_library_in(&dir);
    prefs(&mut h, json!({"onClose": "always"}));
    h.app.session.backup_hold.store(true, Ordering::SeqCst);
    assert!(!may_close(&mut h.app));
    h.step();
    assert!(has(&mut h, "button:quitNoWait") && has(&mut h, "button:quitCancel"));
    click(&mut h, "button:quitCancel");
    assert!(h.app.quit_prompt.is_none() && h.app.quit_backup == QuitBackup::Undecided);
    h.app.session.backup_hold.store(false, Ordering::SeqCst);
    assert!(h.step_until(T, |h| h.app.session.last_backup.is_some()), "the backup goes on");
    for _ in 0..10 {
        h.step();
    }
    assert!(!h.quit_requested(), "called off: no quit by itself");
    assert_eq!(backups(&dir), 1);

    h.app.session.backup_hold.store(true, Ordering::SeqCst);
    assert!(!may_close(&mut h.app));
    h.step();
    click(&mut h, "button:quitNoWait");
    assert!(h.step_until(T, |h| h.quit_requested()), "{:?}", h.app.quit_prompt);
    h.app.session.backup_hold.store(false, Ordering::SeqCst);
    assert!(h.step_until(T, |h| h.app.session.backup.as_ref().is_some_and(|b| b.outcome().is_some())));
    assert_eq!(backups(&dir), 1, "the stopped one is no backup");
}

/// Given the backup was skipped in a quit that then did not happen (a task had started, and its
/// question was cancelled), then the next quit asks about the backup again.
#[test]
fn a_quit_that_is_called_off_starts_over_next_time() {
    let dir = temp("called-off");
    let mut h = app_with_a_library_in(&dir);
    prefs(&mut h, json!({"onClose": "ask"}));
    assert!(!may_close(&mut h.app));
    h.step();
    assert!(matches!(h.app.quit_prompt, Some(QuitPrompt::BackUp(_))), "{:?}", h.app.quit_prompt);
    // a task starts behind the question (an import an agent began, say)
    let task = h.app.session.activity.start("import", "Importing", lightcraft_engine::activity::Cancel::Yes);
    click(&mut h, "button:quitSkipBackup");
    assert!(matches!(h.app.quit_prompt, Some(QuitPrompt::Tasks(_))), "the task is asked about: {:?}", h.app.quit_prompt);
    assert_eq!(h.app.quit_backup, QuitBackup::Settled);
    click(&mut h, "button:quitCancel");
    assert!(h.app.quit_prompt.is_none() && !h.quit_requested());
    assert_eq!(h.app.quit_backup, QuitBackup::Undecided, "called off: the next quit starts over");
    drop(task);
    assert!(!may_close(&mut h.app), "asked again");
    assert!(matches!(h.app.quit_prompt, Some(QuitPrompt::BackUp(_))), "{:?}", h.app.quit_prompt);
}

/// Given a quit that waits for its backup, when another question of quitting comes up and is
/// cancelled, then LightCraft does not quit by itself when the backup ends.
#[test]
fn a_quit_cancelled_while_it_waited_does_not_happen_by_itself() {
    use std::sync::atomic::Ordering;
    let dir = temp("no-surprise");
    let mut h = app_with_a_library_in(&dir);
    prefs(&mut h, json!({"onClose": "always"}));
    h.app.session.backup_hold.store(true, Ordering::SeqCst);
    assert!(!may_close(&mut h.app));
    assert_eq!(h.app.quit_backup, QuitBackup::Running);
    // a task starts, and the window's close button is pressed again
    let task = h.app.session.activity.start("import", "Importing", lightcraft_engine::activity::Cancel::Yes);
    assert!(!may_close(&mut h.app));
    h.step();
    assert!(matches!(h.app.quit_prompt, Some(QuitPrompt::Tasks(_))), "{:?}", h.app.quit_prompt);
    // (the awaited backup is not among the tasks quitting would stop)
    assert!(!h.app.quit_prompt.as_ref().unwrap().text().contains("Backing up"), "{:?}", h.app.quit_prompt);
    click(&mut h, "button:quitCancel");
    assert_eq!(h.app.quit_backup, QuitBackup::Undecided);
    h.app.session.backup_hold.store(false, Ordering::SeqCst);
    assert!(h.step_until(T, |h| h.app.session.last_backup.is_some()), "the backup is still made");
    for _ in 0..10 {
        h.step();
    }
    assert!(!h.quit_requested(), "no quit by itself");
    drop(task);
}

/// Given a backup of another library is still running after a switch, when the person quits a
/// library that is backed up on quitting, then that other backup is asked about like any task:
/// it is not the backup this quit waits for.
#[test]
fn another_librarys_backup_is_a_task_like_any_other() {
    use std::sync::atomic::Ordering;
    let dir = temp("other-library");
    let mut h = app_with_a_library_in(&dir);
    h.app.session.backup_hold.store(true, Ordering::SeqCst);
    h.app.run("library.backup", json!({"background": true})).unwrap();
    // on to another library, which is backed up when LightCraft quits
    h.app.run("app.openLibrary", json!({"path": dir.join("second").to_string_lossy()})).unwrap();
    prefs(&mut h, json!({"onClose": "always"}));
    assert!(!may_close(&mut h.app));
    let prompt = h.app.quit_prompt.clone();
    h.app.session.backup_hold.store(false, Ordering::SeqCst);
    assert!(matches!(&prompt, Some(QuitPrompt::Tasks(text)) if text.contains("Backing up")), "{prompt:?}");
}

/// Given a library to be backed up once a day whose backup can't be started, then the person is
/// told (not only the log).
#[test]
fn a_due_backup_that_cannot_start_is_said() {
    let dir = temp("no-start");
    let mut h = app_with_a_library_in(&dir);
    h.app.backs_up_by_itself = true;
    h.app.session.backup_dir = Some(dir.join("library/segments"));
    prefs(&mut h, json!({"every": "day"}));
    assert!(h.step_until(T, |h| h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("not backed up"))), "{:?}", h.app.ui.toast);
    assert_eq!(backups(&dir), 0);
}

/// A library that is not a folder on disk is not asked about: there is no backup to make.
#[test]
fn a_library_in_memory_quits_without_a_question() {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1300.0, 820.0], 1.0);
    h.app.session.backup_schedule.on_close = lightcraft_engine::backup_schedule::BackupOnClose::Ask;
    h.step();
    assert!(may_close(&mut h.app));
}

/// Given a key bound to Back Up Library, when it is pressed, then the backup goes on in the
/// background like one from the menu (the window is not held up until it is made); the control
/// channel's own `library.backup` still returns when the backup is made.
#[test]
fn a_key_bound_to_back_up_library_backs_up_in_the_background() {
    use std::sync::atomic::Ordering;
    let dir = temp("key");
    let mut h = app_with_a_library_in(&dir);
    h.app.run("app.setShortcut", json!({"id": "library.backup", "shortcut": "F9"})).unwrap();
    h.app.session.backup_hold.store(true, Ordering::SeqCst);
    // (a key that waited for the backup would wait for ever: let go after a while, to fail)
    let hold = h.app.session.backup_hold.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        hold.store(false, Ordering::SeqCst);
    });
    let r = h.request("ui.key", json!({"key": "F9"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let running = h.app.session.backup.as_ref().is_some_and(|b| !b.finished.load(Ordering::Relaxed));
    h.app.session.backup_hold.store(false, Ordering::SeqCst);
    assert!(running, "the key's backup waited for the backup to be made");
    assert!(h.step_until(T, |_| backups(&dir) == 1));
}

/// Given Settings ▸ General ▸ Backups, when "3 months" is clicked under Keep backups, then the
/// library keeps its backups for ninety days; "Forever" keeps them all again.
#[test]
fn how_long_backups_are_kept_is_chosen_in_settings() {
    let dir = temp("keep");
    let mut h = app_with_a_library_in(&dir);
    assert_eq!(h.request("ui.key", json!({"key": ",", "cmd": true}), T)["ok"], true);
    assert!(has(&mut h, "button:settingsBackupKeep-0") && has(&mut h, "button:settingsBackupKeep-4"));
    assert!(prefs(&mut h, json!({}))["keepDays"].is_null(), "forever until chosen");
    click(&mut h, "button:settingsBackupKeep-2");
    assert_eq!(prefs(&mut h, json!({}))["keepDays"], json!(90));
    click(&mut h, "button:settingsBackupKeep-0");
    assert!(prefs(&mut h, json!({}))["keepDays"].is_null());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a library that keeps its backups for a month and has four from a year ago, when it is
/// backed up, then the toast that says where the backup went also says that older backups were
/// removed: nothing of the person's goes in silence.
#[test]
fn the_app_says_when_a_backup_removed_older_ones() {
    let dir = temp("says");
    let mut h = app_with_a_library_in(&dir);
    let now = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let clock = now.clone();
    h.app.session.clock = Box::new(move || clock.lock().unwrap().clone());
    for day in 10..14 {
        *now.lock().unwrap() = format!("2025-01-{day}T09:00:00");
        h.app.run("library.backup", json!({"background": false})).unwrap();
    }
    assert!(h.step_until(T, |h| h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("Backed up the library") && !t.0.contains("Older backups"))));
    prefs(&mut h, json!({"keepDays": 30}));
    *now.lock().unwrap() = "2026-01-10T09:00:00".into();
    h.app.run("library.backup", json!({"background": false})).unwrap();
    assert!(
        h.step_until(T, |h| h
            .app
            .ui
            .toast
            .as_ref()
            .is_some_and(|t| t.0.contains("Backed up the library") && t.0.contains("Older backups removed: 2"))),
        "{:?}",
        h.app.ui.toast
    );
    assert_eq!(backups(&dir), 3);
    let _ = std::fs::remove_dir_all(&dir);
}
