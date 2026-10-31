//! File ▸ Restore Library from Backup… in the app: the backups in the folder the library's
//! backups go to are listed to choose from (newest first), one is restored, and a backup kept
//! elsewhere is found with Choose Folder….

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-restorelist-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The app with a library on disk in `dir/library`, the clock it goes by, and the folder the
/// folder dialog answers with (none: it is closed without one).
fn app_with_a_library_in(dir: &Path) -> (Headless, Arc<Mutex<String>>, Arc<Mutex<Option<String>>>) {
    let mut session = lightcraft_engine::Session::new().with_fs();
    let now = Arc::new(Mutex::new("2026-01-01T09:00:00".to_string()));
    let clock = now.clone();
    session.clock = Box::new(move || clock.lock().unwrap().clone());
    session.open_library(dir.join("library"), true).unwrap();
    let chosen = Arc::new(Mutex::new(None::<String>));
    let answer = chosen.clone();
    let services = Services { pick_folder: Some(Box::new(move || answer.lock().unwrap().clone())), ..Default::default() };
    let mut h = Headless::new(LightcraftApp::new(session, services), [1300.0, 820.0], 1.0);
    h.step();
    (h, now, chosen)
}

/// Back the library up at `time`, rated `rating` then: the backup's folder.
fn back_up_at(h: &mut Headless, now: &Arc<Mutex<String>>, time: &str, rating: u8) -> String {
    *now.lock().unwrap() = time.to_string();
    h.app.run("photo.rate", json!({"rating": rating})).unwrap();
    let r = h.app.run("library.backup", json!({"background": false})).unwrap();
    assert!(r["error"].is_null(), "{r}");
    r["report"]["dir"].as_str().unwrap().to_string()
}

fn click(h: &mut Headless, id: &str) {
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

/// What the dialog lists: (folder it lists, the backups' names, the one selected).
fn listed(h: &Headless) -> Option<(String, Vec<String>, Option<usize>)> {
    match &h.app.ui.dialog {
        Some(Dialog::RestoreBackup { dir, backups, selected }) => {
            Some((dir.clone(), backups.iter().map(|b| b["name"].as_str().unwrap_or_default().to_string()).collect(), *selected))
        }
        _ => None,
    }
}

/// Wait for the restore that was started to be over; how it ended.
fn restored(h: &mut Headless) -> Value {
    assert!(h.step_until(T, |h| h.app.session.restore.as_ref().is_some_and(|r| !r.running())), "a restore was started, and ended");
    h.app.session.execute("library.restoreStatus", &json!({})).unwrap()
}

fn rating(h: &Headless) -> u8 {
    let id = h.app.session.selection.active.unwrap();
    h.app.session.catalog.photo(id).unwrap().rating
}

/// Given a library with three backups, when Restore Library from Backup… is chosen, then they
/// are listed newest first with the newest chosen, and nothing is restored yet. When the oldest
/// is clicked and then Restore, the library is as it was at that backup.
#[test]
fn the_librarys_backups_are_listed_and_the_one_chosen_is_restored() {
    let dir = temp("choose");
    let (mut h, now, _) = app_with_a_library_in(&dir);
    back_up_at(&mut h, &now, "2026-01-10T09:00:00", 1);
    back_up_at(&mut h, &now, "2026-02-10T09:00:00", 2);
    back_up_at(&mut h, &now, "2026-03-10T09:00:00", 3);
    h.app.run("photo.rate", json!({"rating": 5})).unwrap();

    assert_eq!(h.app.run("library.restore", json!({})), Ok(Value::Null), "the dialog is up: nothing is done yet");
    let (folder, names, selected) = listed(&h).expect("the list of backups");
    assert_eq!(PathBuf::from(folder), dir.join("library").join("Backups"));
    assert_eq!(names, ["2026-03-10 09.00.00 UTC", "2026-02-10 09.00.00 UTC", "2026-01-10 09.00.00 UTC"]);
    assert_eq!(selected, Some(0), "the newest");
    assert!(h.app.session.restore.is_none() && rating(&h) == 5);
    h.step();
    for id in ["row:restoreBackup-0", "row:restoreBackup-2", "button:dialogOk", "button:dialogCancel", "button:restoreChooseFolder"] {
        assert!(has(&mut h, id), "{id}");
    }

    click(&mut h, "row:restoreBackup-2");
    assert_eq!(listed(&h).map(|l| l.2), Some(Some(2)));
    click(&mut h, "button:dialogOk");
    assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
    let done = restored(&mut h);
    assert!(done["error"].is_null(), "{done}");
    assert_eq!(rating(&h), 1, "as it was in January");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a library that was never backed up, then the dialog says there are no backups in its
/// folder, Restore does nothing, and Cancel leaves the library as it is.
#[test]
fn a_library_without_backups_has_none_to_choose() {
    let dir = temp("none");
    let (mut h, _, _) = app_with_a_library_in(&dir);
    h.app.run("library.restore", json!({})).unwrap();
    assert_eq!(listed(&h).map(|l| (l.1, l.2)), Some((Vec::new(), None)));
    h.step();
    assert!(!has(&mut h, "row:restoreBackup-0"));
    click(&mut h, "button:dialogOk");
    assert!(listed(&h).is_some() && h.app.session.restore.is_none(), "nothing to restore: nothing happens");
    click(&mut h, "button:dialogCancel");
    assert!(h.app.ui.dialog.is_none() && h.app.session.restore.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a folder for backups that holds only another library's, then they are listed (a
/// library can be put back from any backup) but none is chosen beforehand: taking another
/// library's catalog in place of this one is for the person to choose.
#[test]
fn another_librarys_backups_are_listed_but_not_chosen_beforehand() {
    let (dir, shared) = (temp("others"), temp("others-shared"));
    let (mut h, now, _) = app_with_a_library_in(&dir);
    h.app.run("library.backupPreferences", json!({"dir": shared.to_string_lossy()})).unwrap();
    back_up_at(&mut h, &now, "2026-01-10T09:00:00", 1);
    // the same folder is where a second library's backups go
    h.app.session.close_library().unwrap();
    h.app.session.open_library(dir.join("second"), true).unwrap();
    h.app.run("library.backupPreferences", json!({"dir": shared.to_string_lossy()})).unwrap();
    h.app.run("library.restore", json!({})).unwrap();
    let (_, names, selected) = listed(&h).expect("the list");
    assert_eq!((names.len(), selected), (1, None));
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(h.app.session.restore.is_none(), "none chosen: none restored");
    // its own, once it has one, is the one chosen
    click(&mut h, "button:dialogCancel");
    back_up_at(&mut h, &now, "2026-02-10T09:00:00", 2);
    back_up_at(&mut h, &now, "2025-12-01T09:00:00", 3);
    h.app.run("library.restore", json!({})).unwrap();
    let (_, names, selected) = listed(&h).expect("the list");
    assert_eq!(names.len(), 3);
    assert_eq!(selected.map(|i| names[i].as_str()), Some("2026-02-10 09.00.00 UTC"), "its own newest by when it was made: {names:?}");
    for d in [dir, shared] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Given backups kept in another folder, when Choose Folder… is pressed and that folder is
/// chosen, then the list is of the backups there; and when a backup itself is chosen, it is the
/// one restored. A folder dialog closed without a folder changes nothing.
#[test]
fn a_backup_kept_elsewhere_is_found_with_choose_folder() {
    let (dir, vault) = (temp("elsewhere"), temp("elsewhere-vault"));
    let (mut h, now, chosen) = app_with_a_library_in(&dir);
    *now.lock().unwrap() = "2026-01-10T09:00:00".into();
    h.app.run("photo.rate", json!({"rating": 4})).unwrap();
    let kept = h.app.run("library.backup", json!({"dir": vault.to_string_lossy(), "background": false})).unwrap()["report"]["dir"]
        .as_str()
        .unwrap()
        .to_string();
    h.app.run("photo.rate", json!({"rating": 2})).unwrap();

    h.app.run("library.restore", json!({})).unwrap();
    assert_eq!(listed(&h).map(|l| l.1.len()), Some(0), "none in the library's own folder");
    h.step();
    // closed without a folder
    click(&mut h, "button:restoreChooseFolder");
    assert_eq!(listed(&h).map(|l| l.1.len()), Some(0));
    // the folder the backups are in
    *chosen.lock().unwrap() = Some(vault.to_string_lossy().into_owned());
    click(&mut h, "button:restoreChooseFolder");
    let (folder, names, selected) = listed(&h).expect("the list");
    assert_eq!((PathBuf::from(folder), names.len(), selected), (vault.clone(), 1, Some(0)));
    assert!(h.app.session.restore.is_none());
    // the backup itself
    *chosen.lock().unwrap() = Some(kept);
    click(&mut h, "button:restoreChooseFolder");
    assert!(h.app.ui.dialog.is_none());
    let done = restored(&mut h);
    assert!(done["error"].is_null(), "{done}");
    assert_eq!(rating(&h), 4);
    // and a folder that is neither says so, and nothing is restored
    let before = h.app.session.restore.clone().map(|r| Arc::as_ptr(&r) as usize);
    *chosen.lock().unwrap() = Some(dir.to_string_lossy().into_owned());
    h.app.run("library.restore", json!({})).unwrap();
    h.step();
    click(&mut h, "button:restoreChooseFolder");
    assert_eq!(h.app.session.restore.clone().map(|r| Arc::as_ptr(&r) as usize), before);
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("backup")), "{:?}", h.app.ui.toast);
    for d in [dir, vault] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// An agent that says which backup (`dir`) is asked nothing: the restore starts, as before.
#[test]
fn a_restore_that_says_which_backup_asks_nothing() {
    let dir = temp("agent");
    let (mut h, now, _) = app_with_a_library_in(&dir);
    let made = back_up_at(&mut h, &now, "2026-01-10T09:00:00", 3);
    h.app.run("photo.rate", json!({"rating": 1})).unwrap();
    let r = h.app.run("library.restore", json!({"dir": made})).unwrap();
    assert_eq!(r["running"], json!(true), "{r}");
    assert!(h.app.ui.dialog.is_none());
    assert!(restored(&mut h)["error"].is_null());
    assert_eq!(rating(&h), 3);
    // and a folder of backups given so is not a backup: said, not listed
    let e = h.app.run("library.restore", json!({"dir": dir.join("library").join("Backups").to_string_lossy()})).unwrap_err();
    assert!(e.contains("backup"), "{e}");
    assert!(h.app.ui.dialog.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the list of backups, when the one chosen is gone by the time Restore is pressed (an
/// old one removed since), then nothing is restored, the reason is said, and the list stays
/// open as the folder now is, with nothing chosen for the person.
#[test]
fn a_backup_that_is_gone_when_restore_is_pressed_leaves_the_list_as_the_folder_is() {
    let dir = temp("gone");
    let (mut h, now, _) = app_with_a_library_in(&dir);
    back_up_at(&mut h, &now, "2026-01-10T09:00:00", 1);
    let newest = back_up_at(&mut h, &now, "2026-02-10T09:00:00", 2);
    h.app.run("library.restore", json!({})).unwrap();
    h.step();
    std::fs::remove_dir_all(&newest).unwrap();
    click(&mut h, "button:dialogOk");
    let (_, names, selected) = listed(&h).expect("the list stays");
    assert_eq!((names, selected), (vec!["2026-01-10 09.00.00 UTC".to_string()], None));
    assert!(h.app.session.restore.is_none() && h.app.ui.toast.is_some(), "{:?}", h.app.ui.toast);
    assert_eq!(rating(&h), 2, "the library is as it was");
    // and a restore into another folder says which backup: the list is of backups for this library
    click(&mut h, "button:dialogCancel");
    assert!(h.app.run("library.restore", json!({"to": dir.join("elsewhere").to_string_lossy()})).is_err());
    assert!(h.app.ui.dialog.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Where the folder dialog is the system's own and answers later (the desktop app): the answer
/// lists a folder of backups, or restores a backup and closes the list; an answer that comes
/// when another dialog is open by then leaves that dialog alone, and one that comes when a
/// backup has begun restores nothing.
#[test]
fn a_folder_dialog_that_answers_later_is_taken_as_it_finds_the_app() {
    use std::sync::mpsc::{Sender, channel};
    let (dir, vault) = (temp("later"), temp("later-vault"));
    let (mut h, now, _) = app_with_a_library_in(&dir);
    let asked: Arc<Mutex<Vec<Sender<Vec<String>>>>> = Default::default();
    let a = asked.clone();
    h.app.services.pick_folder = None;
    h.app.services.picker = Some(Box::new(move |_| {
        let (tx, rx) = channel();
        a.lock().unwrap().push(tx);
        Ok(rx)
    }));
    *now.lock().unwrap() = "2026-01-10T09:00:00".into();
    h.app.run("photo.rate", json!({"rating": 4})).unwrap();
    let kept = h.app.run("library.backup", json!({"dir": vault.to_string_lossy(), "background": false})).unwrap()["report"]["dir"]
        .as_str()
        .unwrap()
        .to_string();
    h.app.run("photo.rate", json!({"rating": 2})).unwrap();
    let answer = |h: &mut Headless, with: &str| {
        asked.lock().unwrap().pop().expect("a folder was asked for").send(vec![with.to_string()]).unwrap();
        for _ in 0..4 {
            h.step();
        }
    };

    // a folder of backups: listed
    h.app.run("library.restore", json!({})).unwrap();
    h.step();
    click(&mut h, "button:restoreChooseFolder");
    assert_eq!(listed(&h).map(|l| l.1.len()), Some(0), "the folder dialog is up: nothing changed yet");
    answer(&mut h, &vault.to_string_lossy());
    assert_eq!(listed(&h).map(|l| (PathBuf::from(l.0), l.1.len())), Some((vault.clone(), 1)));

    // the answer comes when another dialog is open: that one stays, and it is said
    click(&mut h, "button:restoreChooseFolder");
    h.app.ui.dialog = Some(Dialog::WhatsNew);
    answer(&mut h, &vault.to_string_lossy());
    assert_eq!(h.app.ui.dialog, Some(Dialog::WhatsNew));
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("another dialog")), "{:?}", h.app.ui.toast);
    h.app.ui.dialog = None;

    // the answer comes when a backup has begun: nothing is listed or restored
    h.app.run("library.restore", json!({})).unwrap();
    h.step();
    click(&mut h, "button:restoreChooseFolder");
    h.app.ui.dialog = None;
    h.app.session.backup_hold.store(true, std::sync::atomic::Ordering::SeqCst);
    h.app.run("library.backup", json!({"background": true})).unwrap();
    answer(&mut h, &kept);
    assert!(h.app.ui.dialog.is_none() && h.app.session.restore.is_none());
    h.app.session.backup_hold.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(h.step_until(T, |h| h.app.session.backup.as_ref().is_some_and(|b| b.outcome().is_some())));

    // a backup: restored, and the list is closed
    h.app.run("library.restore", json!({})).unwrap();
    h.step();
    click(&mut h, "button:restoreChooseFolder");
    answer(&mut h, &kept);
    assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
    assert!(restored(&mut h)["error"].is_null());
    assert_eq!(rating(&h), 4);
    for d in [dir, vault] {
        let _ = std::fs::remove_dir_all(d);
    }
}
