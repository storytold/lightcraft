//! Settings ▸ General ▸ Backups: where the library's backups go is shown, chosen and put back to
//! the usual place there (`library.backupPreferences`), for a library that is a folder on disk.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(10);

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-backupfolder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The app with a library on disk in `dir/library`, Settings open on General.
fn settings_of_a_library_in(dir: &Path) -> Headless {
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("library"), false).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1300.0, 820.0], 1.0);
    open_settings(&mut h);
    h
}

/// Settings, opened as a person opens it (⌘,): on General.
fn open_settings(h: &mut Headless) {
    let r = h.request("ui.key", json!({"key": ",", "cmd": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "general".into() }));
}

fn has(h: &mut Headless, id: &str) -> bool {
    h.request("ui.widgets", json!({"filter": id}), T)["result"].as_array().is_some_and(|w| w.iter().any(|w| w["id"] == id))
}

fn click(h: &mut Headless, id: &str) -> Value {
    h.request("ui.clickWidget", json!({"id": id}), T)
}

/// The folder the library's backups go to, as the command (and so Settings) says it.
fn folder_shown(h: &mut Headless) -> String {
    h.app.run("library.backupPreferences", json!({})).unwrap()["folder"].as_str().unwrap_or_default().to_string()
}

fn backup_dir(h: &mut Headless) -> Value {
    h.app.run("library.backupPreferences", json!({})).unwrap()["dir"].clone()
}

/// Given a library on disk whose backups go to the usual place, when Settings ▸ General is
/// open, then Backups shows that place (Backups in the library folder) and there is nothing to
/// put back.
#[test]
fn settings_show_where_backups_go() {
    let dir = temp("shown");
    let mut h = settings_of_a_library_in(&dir);
    assert!(has(&mut h, "button:settingsBackupChoose") && has(&mut h, "button:settingsBackupReset"));
    let shown = folder_shown(&mut h);
    assert!(Path::new(&shown).ends_with("library/Backups"), "{shown}");
    assert_eq!(backup_dir(&mut h), Value::Null);
    // nothing to put back: the button is there, and does nothing
    let r = click(&mut h, "button:settingsBackupReset");
    assert_eq!(backup_dir(&mut h), Value::Null, "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given that library, when Choose Folder… is clicked and a folder is picked, then the library's
/// backups go there: Settings shows it, it is saved with the library, and the next backup is
/// made in it. When Use Library Folder is clicked, they go to the usual place again.
#[test]
fn the_backup_folder_is_chosen_and_put_back_in_settings() {
    let dir = temp("chosen");
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut h = settings_of_a_library_in(&dir);
    let picked = elsewhere.to_string_lossy().into_owned();
    h.app.services.pick_folder = Some(Box::new(move || Some(picked.clone())));

    let r = click(&mut h, "button:settingsBackupChoose");
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(backup_dir(&mut h), json!(elsewhere.to_string_lossy()));
    assert_eq!(folder_shown(&mut h), elsewhere.to_string_lossy());
    // saved with the library, and used
    let prefs = std::fs::read_to_string(dir.join("library/prefs.json")).unwrap();
    assert!(prefs.contains("elsewhere"), "{prefs}");
    let made = h.app.session.execute("library.backup", &json!({})).unwrap();
    assert!(Path::new(made["report"]["dir"].as_str().unwrap()).starts_with(&elsewhere), "{made}");
    // (the line that says where the backups stand has changed, and may take another height:
    // the button is clicked where it is drawn now)
    for _ in 0..4 {
        h.step();
    }

    let r = click(&mut h, "button:settingsBackupReset");
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(backup_dir(&mut h), Value::Null);
    assert!(Path::new(&folder_shown(&mut h)).ends_with("library/Backups"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the folder dialog is closed without a folder, then nothing changes and nothing is said.
#[test]
fn closing_the_folder_dialog_changes_nothing() {
    let dir = temp("cancelled");
    let mut h = settings_of_a_library_in(&dir);
    h.app.services.pick_folder = Some(Box::new(|| None));
    click(&mut h, "button:settingsBackupChoose");
    assert_eq!(backup_dir(&mut h), Value::Null);
    assert!(h.app.ui.toast.is_none(), "{:?}", h.app.ui.toast);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a folder that can't be used (one of the library's own: its `segments`), when it is
/// picked, then the folder stays what it was and the person is told why.
#[test]
fn a_folder_that_is_refused_says_why() {
    let dir = temp("refused");
    let mut h = settings_of_a_library_in(&dir);
    let own = dir.join("library/segments");
    std::fs::create_dir_all(&own).unwrap();
    let picked = own.to_string_lossy().into_owned();
    h.app.services.pick_folder = Some(Box::new(move || Some(picked.clone())));
    click(&mut h, "button:settingsBackupChoose");
    assert_eq!(backup_dir(&mut h), Value::Null);
    let said = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(said.contains("inside the library folder"), "{said:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given Settings is left open on General, then showing the Backups section runs no command:
/// the record of what was done in the session (`journal.list`) is what it was.
#[test]
fn showing_the_section_runs_no_command() {
    let dir = temp("quiet");
    let mut h = settings_of_a_library_in(&dir);
    assert!(has(&mut h, "button:settingsBackupChoose"));
    let before = h.app.session.journal.clone();
    for _ in 0..30 {
        h.step();
    }
    assert_eq!(h.app.session.journal, before);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a library that is not a folder on disk (the demo, in memory), then Settings has no
/// Backups section: there is nothing to back up to a folder.
#[test]
fn a_library_in_memory_has_no_backups_section() {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1300.0, 820.0], 1.0);
    open_settings(&mut h);
    assert!(has(&mut h, "button:settingsOpenLibrary"), "Settings is open on General");
    assert!(!has(&mut h, "button:settingsBackupChoose") && !has(&mut h, "button:settingsBackupReset"));
}

/// Agents choose the folder with the command as before; with `choose` and no `dir` the app asks
/// for it, and without a folder dialog it says so instead of doing nothing.
#[test]
fn the_command_asks_for_a_folder_in_the_app() {
    let dir = temp("command");
    let elsewhere = dir.join("asked");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut h = settings_of_a_library_in(&dir);
    let e = h.app.run("library.backupPreferences", json!({"choose": true})).unwrap_err();
    assert!(e.contains("no folder dialog"), "{e}");
    // (`choose: false` is not asking)
    let never = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let asked = never.clone();
    h.app.services.pick_folder = Some(Box::new(move || {
        asked.store(true, std::sync::atomic::Ordering::Relaxed);
        None
    }));
    let r = h.app.run("library.backupPreferences", json!({"choose": false})).unwrap();
    assert!(r["dir"].is_null() && !never.load(std::sync::atomic::Ordering::Relaxed), "{r}");
    let picked = elsewhere.to_string_lossy().into_owned();
    h.app.services.pick_folder = Some(Box::new(move || Some(picked.clone())));
    let r = h.app.run("library.backupPreferences", json!({"choose": true})).unwrap();
    assert_eq!(r["dir"], json!(elsewhere.to_string_lossy()));
    let _ = std::fs::remove_dir_all(&dir);
}
