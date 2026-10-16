//! Settings ▸ General ▸ Backups says where the library stands and lets a backup be made from
//! there: Back Up Now beside Last backup, a caution when the folder holds no backup of the library
//! and nothing is set to make one, a note or caution when the backups' folder is on the library's
//! own drive, and a caution when a folder that was chosen is not there.
//!
//! Before, a new library read "Never / Never / Not yet" in the same grey as everything else, the
//! one sentence that asks for another drive stood under a folder that was on the same one without
//! a word, and making the first backup meant leaving Settings for the File menu.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// A folder under the system's temporary one, removed again when the test ends, passed or not.
struct TempFolder(PathBuf);

impl TempFolder {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("lc-ui-backupstatus-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The app with a library on disk in `dir/library`, Settings open on General.
fn settings_of_a_library_in(dir: &Path) -> Headless {
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("library"), false).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1300.0, 1000.0], 1.0);
    h.app.ui.dialog = Some(Dialog::Settings { tab: "general".into() });
    steps(&mut h);
    h
}

fn steps(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "click {id}: {r}");
    steps(h);
}

fn says(h: &Headless, part: &str) -> bool {
    h.painted_text().iter().any(|text| text.contains(part))
}

fn backups(h: &mut Headless) -> usize {
    h.app.run("library.backups", json!({})).unwrap()["backups"].as_array().map_or(0, Vec::len)
}

fn running(h: &mut Headless) -> bool {
    let status: Value = h.app.run("library.backupStatus", json!({})).unwrap();
    !status.is_null() && status["finished"] != true
}

const NEVER: &str = "No backup of this library is in this folder";
const SAME_DRIVE: &str = "On the same drive as the library";
const NOT_THERE: &str = "This folder is not there now";

#[test]
fn a_library_never_backed_up_with_nothing_set_to_do_it_says_so() {
    let dir = TempFolder::new("never");
    // Given a new library: no backup yet, none while open, none when quitting
    let mut h = settings_of_a_library_in(&dir.0);
    // Then Backups says so, beyond "Not yet"
    assert!(says(&h, "Not yet"));
    assert!(says(&h, NEVER), "{:?}", h.painted_text().iter().filter(|t| t.contains("back")).collect::<Vec<_>>());
    // When it is set to be backed up once a week
    h.app.run("library.backupPreferences", json!({"every": "week"})).unwrap();
    steps(&mut h);
    // Then there is nothing to caution about: the first backup is on its way by itself
    assert!(!says(&h, NEVER), "a schedule answers it");
    // And without a schedule, a backup made answers it too
    h.app.run("library.backupPreferences", json!({"every": "never"})).unwrap();
    steps(&mut h);
    assert!(says(&h, NEVER));
    h.app.session.execute("library.backup", &json!({})).unwrap();
    steps(&mut h);
    assert!(!says(&h, NEVER) && !says(&h, "Not yet"), "a backup answers it");
}

#[test]
fn back_up_now_makes_a_backup_from_settings() {
    let dir = TempFolder::new("now");
    let mut h = settings_of_a_library_in(&dir.0);
    assert_eq!(backups(&mut h), 0);
    // When Back Up Now is pressed
    click(&mut h, "button:settingsBackupNow");
    assert!(h.step_until(T, |h| h.app.session.last_backup.is_some()), "the backup is made");
    steps(&mut h);
    // Then the library has a backup, Last backup says when, and Settings is still open
    assert_eq!(backups(&mut h), 1);
    assert!(!says(&h, "Not yet") && !says(&h, NEVER));
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "general".into() }));
}

#[test]
fn while_a_backup_is_made_the_row_says_so_and_a_second_cannot_be_started() {
    let dir = TempFolder::new("running");
    let mut h = settings_of_a_library_in(&dir.0);
    // Given a backup held up mid-way
    h.app.session.backup_hold.store(true, Ordering::SeqCst);
    click(&mut h, "button:settingsBackupNow");
    assert!(running(&mut h), "the backup is under way");
    // Then the row says a backup is being made
    assert!(says(&h, "Backing up"), "{:?}", h.painted_text().iter().filter(|t| t.contains("ack")).collect::<Vec<_>>());
    // And pressing the button again starts nothing and says nothing went wrong: it is not on offer
    h.app.ui.toast = None;
    let _ = h.request("ui.clickWidget", json!({"id": "button:settingsBackupNow"}), T);
    steps(&mut h);
    assert!(h.app.ui.toast.is_none(), "no refusal is shown: {:?}", h.app.ui.toast);
    // When the backup goes on and ends
    h.app.session.backup_hold.store(false, Ordering::SeqCst);
    assert!(h.step_until(T, |h| h.app.session.last_backup.is_some()));
    steps(&mut h);
    // Then there is one backup, the row says when it was made, and another can be made
    assert_eq!(backups(&mut h), 1);
    assert!(!says(&h, "Backing up"));
    click(&mut h, "button:settingsBackupNow");
    let again = (0..400).any(|_| {
        h.step();
        backups(&mut h) == 2
    });
    assert!(again, "Back Up Now is on offer again");
}

#[test]
fn a_backup_folder_on_the_librarys_own_drive_is_cautioned_about() {
    let dir = TempFolder::new("drive");
    // Given backups going to the usual place, Backups in the library folder
    let mut h = settings_of_a_library_in(&dir.0);
    // Then Backups says that folder is on the library's drive
    assert!(says(&h, SAME_DRIVE), "{:?}", h.painted_text().iter().filter(|t| t.contains("drive")).collect::<Vec<_>>());
    // And a folder chosen beside the library, on that drive too, is cautioned about as well
    let beside = dir.0.join("beside");
    std::fs::create_dir_all(&beside).unwrap();
    h.app.run("library.backupPreferences", json!({"dir": beside.to_string_lossy()})).unwrap();
    // (what the folder is like is looked at again when the folder changes)
    assert!(h.step_until(T, |h| h.painted_text().iter().any(|t| t.contains("beside"))));
    steps(&mut h);
    assert!(says(&h, SAME_DRIVE));
}

/// A drive other than the library's cannot be made in a test; `/dev` is a file system of its own
/// on every Unix, and a folder for backups need not be there until the first backup.
#[cfg(unix)]
#[test]
fn a_backup_folder_on_another_drive_is_not_cautioned_about() {
    let dir = TempFolder::new("other");
    let mut h = settings_of_a_library_in(&dir.0);
    assert!(says(&h, SAME_DRIVE));
    h.app.run("library.backupPreferences", json!({"dir": "/dev/lightcraft-backups-test"})).unwrap();
    steps(&mut h);
    assert!(says(&h, "/dev/lightcraft-backups-test"), "the folder is the one set");
    assert!(!says(&h, SAME_DRIVE), "another drive is not cautioned about");
}

#[test]
fn a_chosen_folder_that_is_not_there_is_not_called_the_librarys_drive() {
    let dir = TempFolder::new("missing");
    let mut h = settings_of_a_library_in(&dir.0);
    // Given backups set to go to a folder on a drive that is not connected: neither the folder
    // nor the folder above it is there
    let away = dir.0.join("not-connected").join("backups");
    h.app.run("library.backupPreferences", json!({"dir": away.to_string_lossy()})).unwrap();
    steps(&mut h);
    // Then Backups does not claim it is on the library's drive: it says the folder is not there
    assert!(!says(&h, SAME_DRIVE), "nothing is known about a folder that is not there");
    assert!(says(&h, NOT_THERE), "{:?}", h.painted_text().iter().filter(|t| t.contains("folder")).collect::<Vec<_>>());
    // And it says nothing about what that folder holds, which nobody can see
    assert!(!says(&h, NEVER), "what a folder that is not there holds is not known");
    // And the same for the drive's own top folder, whose folder above (where drives are mounted) is there
    let top = dir.0.join("not-connected-drive");
    h.app.run("library.backupPreferences", json!({"dir": top.to_string_lossy()})).unwrap();
    steps(&mut h);
    assert!(
        says(&h, NOT_THERE) && !says(&h, SAME_DRIVE) && !says(&h, NEVER),
        "{:?}",
        h.painted_text().iter().filter(|t| t.contains("folder") || t.contains("drive")).collect::<Vec<_>>()
    );
    // When that drive is connected and Settings is looked at again, it is a folder like any other
    std::fs::create_dir_all(&top).unwrap();
    h.app.ui.dialog = None;
    steps(&mut h);
    h.app.ui.dialog = Some(Dialog::Settings { tab: "general".into() });
    steps(&mut h);
    assert!(!says(&h, NOT_THERE) && says(&h, SAME_DRIVE) && says(&h, NEVER));
}

#[test]
fn a_folder_that_holds_a_backup_is_not_said_to_hold_none() {
    let dir = TempFolder::new("holds");
    let mut h = settings_of_a_library_in(&dir.0);
    // Given a backup made to a folder of the person's own
    let own = dir.0.join("own");
    std::fs::create_dir_all(&own).unwrap();
    h.app.run("library.backupPreferences", json!({"dir": own.to_string_lossy()})).unwrap();
    h.app.session.execute("library.backup", &json!({})).unwrap();
    // When backups are put back to the library folder and then to that folder again (which
    // forgets when the last backup was: "Not yet")
    h.app.run("library.backupPreferences", json!({"dir": null})).unwrap();
    steps(&mut h);
    assert!(says(&h, NEVER), "the library folder holds no backup");
    h.app.run("library.backupPreferences", json!({"dir": own.to_string_lossy()})).unwrap();
    steps(&mut h);
    // Then the folder, which holds a backup of this library, is not said to hold none
    assert_eq!(backups(&mut h), 1);
    assert!(!says(&h, NEVER), "a backup of this library is in the folder");
}
