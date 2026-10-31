//! Settings ▸ General ▸ Backups says where the library stands and lets a backup be made from
//! there: a line that says it comes first, with a sign and Back Up Now beside it, a caution when the folder holds no backup of the library
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
    // a fixed clock: a backup's name and the time shown for it do not depend on when the test runs
    session.clock = Box::new(|| "2026-01-01T12:00:00".to_string());
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
    status["running"] == true
}

const NEVER: &str = "No backup of this library is in this folder";
const SAME_DRIVE: &str = "On the same drive as the library";
const NOT_THERE: &str = "This folder is not there now";

#[test]
fn a_library_never_backed_up_with_nothing_set_to_do_it_says_so() {
    let dir = TempFolder::new("never");
    // Given a new library: no backup yet, none while open, none when quitting
    let mut h = settings_of_a_library_in(&dir.0);
    // Then Backups says so
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
    let t0 = std::time::Instant::now();
    let mut again = false;
    while t0.elapsed() < T {
        h.step();
        if backups(&mut h) == 2 {
            again = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
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
/// on every Unix, and is there: what is looked at is the drive, not a folder that is missing.
#[cfg(unix)]
#[test]
fn a_backup_folder_on_another_drive_is_not_cautioned_about() {
    let dir = TempFolder::new("other");
    // (where /dev is not a file system of its own, in a sandbox or a container, there is no other drive to use)
    if lightcraft_engine::cmd::backup::same_drive(&dir.0, std::path::Path::new("/dev")) != Some(false) {
        lightcraft_testkit::skip!(System, "/dev is not on another drive than {}", dir.0.display());
        return;
    }
    let mut h = settings_of_a_library_in(&dir.0);
    assert!(says(&h, SAME_DRIVE));
    h.app.run("library.backupPreferences", json!({"dir": "/dev"})).unwrap();
    steps(&mut h);
    assert!(says(&h, "/dev"), "the folder is the one set");
    assert!(!says(&h, NOT_THERE), "it is there: the drive is what is compared");
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

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// WCAG contrast of two opaque colours.
fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    let lum = |c: egui::Color32| {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    };
    let (a, b) = (lum(a), lum(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The colour and size a piece of text is drawn in: the first text shape that has `part`.
fn drawn(h: &Headless, part: &str) -> (egui::Color32, f32) {
    fn find(shape: &egui::Shape, part: &str) -> Option<(egui::Color32, f32)> {
        match shape {
            egui::Shape::Text(text) if text.galley.job.text.contains(part) => {
                let section = text.galley.job.sections.first()?;
                let own = Some(section.format.color).filter(|c| *c != egui::Color32::PLACEHOLDER);
                Some((text.override_text_color.or(own).unwrap_or(text.fallback_color), section.format.font_id.size))
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, part)),
            _ => None,
        }
    }
    h.view.painted_shapes().iter().find_map(|shape| find(shape, part)).unwrap_or_else(|| panic!("no text with {part:?} is drawn"))
}

#[test]
fn where_the_library_stands_comes_first_with_back_up_now_beside_it() {
    let dir = TempFolder::new("first");
    let h = settings_of_a_library_in(&dir.0);
    // Given Backups of a new library
    let (status, now, choose) =
        (rect(&h, "text:settingsBackupStatus"), rect(&h, "button:settingsBackupNow"), rect(&h, "button:settingsBackupChoose"));
    // Then what there is to know comes before what there is to set
    assert!(status.bottom() <= choose.top(), "the status {status:?} is above the folder's buttons {choose:?}");
    // And Back Up Now is on that line, after the words
    assert!(now.top() < status.bottom() && now.bottom() > status.top(), "Back Up Now {now:?} is on the status line {status:?}");
    assert!(now.left() >= status.right() - 0.5, "and after its words");
}

#[test]
fn a_library_without_a_backup_is_told_by_a_sign_and_plain_words_in_every_theme() {
    use crate::theme::{ThemeKind, Tokens};
    for kind in ThemeKind::ALL {
        let dir = TempFolder::new(&format!("sign-{}", kind.id()));
        let mut h = settings_of_a_library_in(&dir.0);
        h.app.run("view.theme", json!({"theme": kind.id()})).unwrap();
        steps(&mut h);
        let t = Tokens::for_kind(kind);
        // Given a library never backed up, with nothing set to do it
        // Then a sign stands before the words (the state is not told by a colour alone)
        assert!(has(&h, "icon:settingsBackupStatus:caution") && !has(&h, "icon:settingsBackupStatus:ok"), "{kind:?}");
        // And the words are body text: the size of a row's own text, in the text colour
        let (colour, size) = drawn(&h, NEVER);
        assert!(size >= 13.0, "{kind:?}: the words are {size} pt");
        assert!(contrast(colour, t.chrome) >= 7.0, "{kind:?}: the words are {:.2}:1", contrast(colour, t.chrome));
        // When a backup is made
        h.app.session.execute("library.backup", &json!({})).unwrap();
        steps(&mut h);
        // Then the sign is the one for "it is done", and the words say when
        assert!(has(&h, "icon:settingsBackupStatus:ok") && !has(&h, "icon:settingsBackupStatus:caution"), "{kind:?}");
        let (colour, size) = drawn(&h, "UTC");
        assert!(size >= 13.0 && contrast(colour, t.chrome) >= 7.0, "{kind:?}: when it was backed up is body text");
        assert!(says(&h, "Last backup"));
    }
}

#[test]
fn a_backup_that_is_set_to_be_made_is_not_cautioned_about() {
    let dir = TempFolder::new("set");
    let mut h = settings_of_a_library_in(&dir.0);
    h.app.run("library.backupPreferences", json!({"onClose": "always"})).unwrap();
    steps(&mut h);
    // Then there is no caution: the words say there is none yet, without the sign
    assert!(says(&h, "Not yet") && !says(&h, NEVER));
    assert!(has(&h, "text:settingsBackupStatus"), "said on the line that says where the library stands");
    assert!(!has(&h, "icon:settingsBackupStatus:caution") && !has(&h, "icon:settingsBackupStatus:ok"));
}

/// Settings closed and opened again: what the backups' folder is like is looked at anew.
fn look_again(h: &mut Headless) {
    h.app.ui.dialog = None;
    steps(h);
    h.app.ui.dialog = Some(Dialog::Settings { tab: "general".into() });
    steps(h);
}

fn folder_of_backups(h: &mut Headless) -> PathBuf {
    PathBuf::from(h.app.run("library.backups", json!({})).unwrap()["dir"].as_str().unwrap())
}

const OK: &str = "icon:settingsBackupStatus:ok";
const CAUTION: &str = "icon:settingsBackupStatus:caution";

#[test]
fn a_backup_that_is_no_longer_in_the_folder_is_not_ticked() {
    let dir = TempFolder::new("gone");
    let mut h = settings_of_a_library_in(&dir.0);
    // Given a backup made to the usual folder, which is remembered and ticked
    h.app.session.execute("library.backup", &json!({})).unwrap();
    steps(&mut h);
    assert!(has(&h, OK) && says(&h, "UTC"));
    // When the folder of backups is moved away by hand
    let folder = folder_of_backups(&mut h);
    std::fs::rename(&folder, dir.0.join("moved-away")).unwrap();
    look_again(&mut h);
    // Then nothing is ticked: no backup of this library is where backups go, and that is said
    assert!(h.app.session.last_backup.is_some(), "the time of the backup is still remembered");
    assert!(!has(&h, OK), "a backup that is gone is not ticked");
    assert!(has(&h, CAUTION) && says(&h, NEVER) && says(&h, "nothing is set"));
    // And with backups set to be made, it is still said that none is there, without the half about setting it
    h.app.run("library.backupPreferences", json!({"every": "week"})).unwrap();
    steps(&mut h);
    assert!(!has(&h, OK) && has(&h, CAUTION), "the backup that was made is gone");
    assert!(says(&h, "No backup of this library is in this folder.") && !says(&h, "nothing is set"));
}

#[test]
fn a_chosen_folder_emptied_of_its_backup_is_not_ticked() {
    let dir = TempFolder::new("emptied");
    let mut h = settings_of_a_library_in(&dir.0);
    let own = dir.0.join("own");
    std::fs::create_dir_all(&own).unwrap();
    h.app.run("library.backupPreferences", json!({"dir": own.to_string_lossy()})).unwrap();
    h.app.session.execute("library.backup", &json!({})).unwrap();
    steps(&mut h);
    assert!(has(&h, OK));
    // When what the folder held is removed
    std::fs::remove_dir_all(&own).unwrap();
    std::fs::create_dir_all(&own).unwrap();
    look_again(&mut h);
    assert!(!has(&h, OK) && has(&h, CAUTION) && says(&h, NEVER));
    // And when the folder itself is gone, it is said to be not there, with the sign of a caution
    std::fs::remove_dir_all(&own).unwrap();
    look_again(&mut h);
    assert!(says(&h, NOT_THERE) && has(&h, CAUTION) && !has(&h, OK));
}

#[test]
fn a_folder_that_holds_a_backup_says_when_it_was_made_though_the_session_forgot() {
    let dir = TempFolder::new("forgot");
    let mut h = settings_of_a_library_in(&dir.0);
    // Given a backup made to a folder of the person's own
    let own = dir.0.join("own");
    std::fs::create_dir_all(&own).unwrap();
    h.app.run("library.backupPreferences", json!({"dir": own.to_string_lossy()})).unwrap();
    h.app.session.execute("library.backup", &json!({})).unwrap();
    // When backups are put back to the library folder and then to that folder again, which
    // forgets when the last backup was
    h.app.run("library.backupPreferences", json!({"dir": null})).unwrap();
    steps(&mut h);
    h.app.run("library.backupPreferences", json!({"dir": own.to_string_lossy()})).unwrap();
    steps(&mut h);
    assert!(h.app.session.last_backup.is_none());
    // Then the row says when the backup in the folder was made, ticked: not "Not yet"
    assert!(has(&h, OK) && !has(&h, CAUTION));
    assert!(says(&h, "Last backup") && says(&h, "UTC") && !says(&h, "Not yet"), "{:?}", h.painted_text());
    // (the time the backup itself says, in the words of a time: its folder's name is not shown)
    let line = h.painted_text().into_iter().find(|t| t.starts_with("Last backup")).unwrap();
    assert!(line.contains("2026") && !line.contains("12.00.00"), "the backup's time is shown, not its name: {line}");
}

#[test]
fn a_backup_under_way_has_no_sign() {
    let dir = TempFolder::new("nosign");
    let mut h = settings_of_a_library_in(&dir.0);
    h.app.session.execute("library.backup", &json!({})).unwrap();
    steps(&mut h);
    h.app.session.backup_hold.store(true, Ordering::SeqCst);
    click(&mut h, "button:settingsBackupNow");
    assert!(running(&mut h) && says(&h, "Backing up"));
    // Then neither sign stands before "Backing up…": it is neither done nor wrong
    assert!(!has(&h, OK) && !has(&h, CAUTION));
    h.app.session.backup_hold.store(false, Ordering::SeqCst);
    assert!(h.step_until(T, |h| !h.app.widgets.is_empty() && h.painted_text().iter().any(|t| t.contains("UTC"))));
}
