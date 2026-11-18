//! Import ▸ Import from lists This PC: every drive and volume as a folder tree (tests see one fake
//! drive, the temporary folder, so they never touch this machine's drives), above the usual places.

use std::time::Duration;

use serde_json::json;

use crate::LccApp;
use crate::headless::Headless;
use crate::state::ViewMode;

const T: Duration = Duration::from_secs(60);

/// Both tests use the one test drive (a folder in the temporary folder, removed at the end of each).
static DRIVE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn import_from_lists_the_drives_as_folder_trees() {
    let _drive = DRIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services { png: None, ..Default::default() });
    app.ui.view = ViewMode::PhotoGrid;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    let r = h.request("engine.execute", json!({"command": "dialog.import", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let root = crate::import_tab::test_drive();
    std::fs::create_dir_all(root.join("DCIM")).unwrap();
    let drive = root.to_string_lossy().to_string();
    let row = format!("source:local:{drive}");
    // the drives are read on a worker thread: they show a few frames later
    h.step_until(Duration::from_secs(10), |h| h.app.widgets.iter().any(|(w, _)| *w == row));
    assert!(h.app.widgets.iter().any(|(w, _)| *w == row), "the drive's row in Import from");
    assert!(h.app.widgets.iter().any(|(w, _)| *w == format!("folderToggle:{drive}")), "it opens as a tree");
    // a click browses it
    assert_eq!(h.request("ui.clickWidget", json!({"id": row}), T)["ok"], true);
    h.settle(T);
    let browsed = h.app.session.browse.as_ref().map(|b| std::path::PathBuf::from(&b.path));
    assert_eq!(browsed.as_deref(), Some(root.as_path()));
    let _ = std::fs::remove_dir_all(&root);
}

/// Press, a frame, release at `at` with the pointer moved there first: how a mouse (or a finger) comes in.
fn press_release_at(h: &mut Headless, at: egui::Pos2) {
    let primary = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.push(egui::Event::PointerMoved(at));
    h.step();
    h.events.push(primary(true));
    h.step();
    h.events.push(primary(false));
    h.settle(T);
}

/// The arrow of a folder in Import from only opens or closes its subfolder list; the name browses it
/// (issue #1234). A card is listed as a drive of its own and also lies inside the disk it is mounted
/// on (`/` ▸ `/run/media/me/CARD`): its two rows used to share one arrow id, so the copy drawn first
/// had no arrow of its own and a click on it fell through to its row and browsed the folder.
#[test]
fn import_folder_arrow_only_expands_and_the_name_loads() {
    let _drive = DRIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services { png: None, ..Default::default() });
    app.ui.view = ViewMode::PhotoGrid;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    let r = h.request("engine.execute", json!({"command": "dialog.import", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let root = crate::import_tab::test_drive();
    let card = crate::import_tab::test_card();
    std::fs::create_dir_all(card.join("DCIM")).unwrap();
    let (drive, card) = (root.to_string_lossy().to_string(), card.to_string_lossy().to_string());
    let rect = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r);
    let browsed = |h: &Headless| h.app.session.browse.as_ref().map(|b| b.path.clone());
    let (drive_arrow, card_row, card_arrow) = (format!("folderToggle:{drive}"), format!("source:local:{card}"), format!("folderToggle:{card}"));
    h.step_until(Duration::from_secs(10), |h| rect(h, &drive_arrow).is_some());
    // the disk's arrow opens it: the card is listed inside it, above the card's own row
    let Some(arrow) = rect(&h, &drive_arrow) else { panic!("the disk's arrow") };
    press_release_at(&mut h, arrow.center());
    h.step_until(Duration::from_secs(10), |h| h.app.widgets.iter().filter(|(w, _)| *w == card_arrow).count() == 2);
    assert_eq!(browsed(&h), None, "the arrow must not load the folder");
    // the name loads it (the folder is read on a worker thread)
    let Some(name) = rect(&h, &card_row) else { panic!("the card's row") };
    press_release_at(&mut h, name.center());
    h.step_until(Duration::from_secs(10), |h| h.app.session.browse.is_some());
    let loaded = browsed(&h);
    assert_eq!(loaded.as_deref().map(std::path::Path::new), Some(root.join("Card").as_path()), "the name browses the folder");
    // each copy's arrow is its own: neither loads anything, both open and close their row (the rows are looked
    // up again for every click: opening a row moves the ones below it)
    for copy in 0..2 {
        for _ in 0..2 {
            let arrows: Vec<egui::Rect> = h.app.widgets.iter().filter(|(w, _)| *w == card_arrow).map(|(_, r)| *r).collect();
            assert!(arrows.len() >= 2, "the card is listed inside the disk and as a drive");
            press_release_at(&mut h, arrows[copy].center());
            assert_eq!(browsed(&h), loaded, "the arrow of copy {copy} must not load the folder");
        }
    }
    // and the name still loads a folder after its arrows were used
    let Some(name) = rect(&h, &format!("source:local:{drive}")) else { panic!("the disk's row") };
    press_release_at(&mut h, name.center());
    h.step_until(Duration::from_secs(10), |h| browsed(h).as_deref().map(std::path::Path::new) == Some(root.as_path()));
    assert_eq!(browsed(&h).as_deref().map(std::path::Path::new), Some(root.as_path()), "the disk's name browses it");
    let _ = std::fs::remove_dir_all(&root);
}
