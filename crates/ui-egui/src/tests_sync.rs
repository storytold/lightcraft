//! Synchronize Folder in the app (see `lightcraft_engine::sync`).
//!
//! Scenarios, in the words of someone whose folder changed outside LightCraft:
//!
//! * Right-click a folder in the sidebar's Folders section ▸ Synchronize Folder…: a dialog opens
//!   at once and scans the folder without holding up the app, then says how many photos are new,
//!   missing, or have metadata updates.
//! * Right-click a folder in the sidebar's Local section: Synchronize Folder… is there when the
//!   folder holds library photos (or has a folder with some inside), and not when it holds none.
//! * Synchronize with the defaults imports the new photos and leaves missing ones alone.
//! * Ticking "Remove missing photos" also moves those to Recently Deleted.
//! * Cancel leaves the library as it was.
//! * A folder that has nothing to bring in only says so: the dialog has one Close and no
//!   choices, and closing it starts nothing.
//! * Synchronize hands the work to the background at once: the dialog closes, the app keeps
//!   answering, and the photos arrive over the next frames under a row in the activity stack, as one undo
//!   step.
//! * While the folder is listed (minutes, on a network share), the scan's row says how many
//!   folders and files it has listed, with a bar that moves on its own, not "0 of 64085"; then it
//!   counts the files and photos checked.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-sync-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    /// The path as the folder tree writes it (forward slashes; widget ids carry it).
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().replace('\\', "/")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_png(path: &str, seed: u8) {
    let (w, h) = (24usize, 16usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 13) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The app with `trip/a.png` and `trip/b.png` imported, then `c.png` added and `b.png` deleted
/// on disk behind its back.
fn changed_folder(dir: &Scratch) -> Headless {
    imported_folder(dir, || {
        write_png(&dir.path("trip/c.png"), 3);
        std::fs::remove_file(dir.path("trip/b.png")).unwrap();
    })
}

/// The app with `trip/a.png` and `trip/b.png` imported, and `then` done on disk after.
fn imported_folder(dir: &Scratch, then: impl FnOnce()) -> Headless {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    then();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn in_library(h: &Headless) -> Vec<String> {
    let mut v: Vec<String> = h.app.session.catalog.photos().filter(|p| p.in_library()).map(|p| p.file_name.clone()).collect();
    v.sort();
    v
}

/// Open the dialog from the folder's menu and wait for the scan.
fn open_and_scan(h: &mut Headless, folder: &str) {
    // the rows above it hold one folder each and open by themselves
    let row = format!("source:libfolder:{folder}");
    assert!(h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| *w == row)), "no row {row}");
    let before = h.app.widgets.iter().find(|(w, _)| *w == row).map(|(_, r)| *r).unwrap_or_else(|| panic!("no row {row}"));
    let panel = h.app.widgets.iter().find(|(w, _)| w == "panel:left_panel").map(|(_, r)| *r).expect("left panel");
    // Off-screen rows are registered too. Preserve the default layout when this row is already visible.
    let before_visible = before.intersect(panel);
    let before_drawn = h.app.widgets.iter().any(|(w, _)| *w == format!("label:libfolder:{folder}"));
    if !before_drawn || !before_visible.is_positive() || before_visible.height() < before.height() - 0.5 {
        crate::tests_panels::scroll_sidebar(h, panel.center().y - before.center().y);
    }
    let rect = h.app.widgets.iter().find(|(w, _)| *w == row).map(|(_, r)| *r).unwrap_or_else(|| panic!("no row {row}"));
    let panel = h.app.widgets.iter().find(|(w, _)| w == "panel:left_panel").map(|(_, r)| *r).expect("left panel");
    let visible = rect.intersect(panel);
    let c = visible.center();
    let drawn = h.app.widgets.iter().any(|(w, _)| *w == format!("label:libfolder:{folder}"));
    let clips: Vec<_> = h.view.shapes.iter().filter(|s| s.shape.visual_bounding_rect().intersects(rect)).map(|s| s.clip_rect).collect();
    assert!(
        drawn && visible.is_positive() && visible.height() >= rect.height() - 0.5,
        "folder row must be drawn and vertically in view: before={before:?} row={rect:?} panel={panel:?} visible={visible:?} click={c:?} drawn={drawn} paint_clips={clips:?}"
    );
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "folderSynchronize"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::SynchronizeFolder { .. })), "the dialog opens at once");
    let scanned = h.step_until(T, |h| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { counts: Some(_), .. })));
    assert!(scanned, "the scan finished: {:?}", h.app.ui.dialog);
}

/// Synchronize: the dialog closes and the work goes to the background at once (nothing has
/// changed yet when the confirm returns); then wait for it.
fn synchronize(h: &mut Headless) {
    let before = in_library(h);
    // the dialog's confirm itself, between frames: it only hands the work over
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg);
    assert!(r.is_ok(), "{r:?}");
    assert!(h.app.sync_run.is_some(), "the work runs in the background");
    assert_eq!(in_library(h), before, "nothing was done on the UI thread");
    assert!(h.step_until(T, |h| h.app.sync_run.is_none()), "the work finishes");
}

/// Tick "Remove missing photos" and wait until the click has landed. A click is queued input:
/// on a loaded machine the dialog can still be settling into place when it arrives, so it is
/// clicked again (at the box's current place) until it lands.
fn tick_remove_missing(h: &mut Headless) {
    let ticked = |h: &Headless| matches!(h.app.ui.dialog, Some(Dialog::SynchronizeFolder { remove_missing: true, .. }));
    for _ in 0..5 {
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "syncRemoveMissing"}), T);
        assert_eq!(r["ok"], true, "{r}");
        if h.step_until(Duration::from_secs(3), ticked) {
            return;
        }
    }
    panic!("the box never ticked: {:?}", h.app.ui.dialog);
}

fn counts(h: &Headless) -> crate::state::SyncCounts {
    match &h.app.ui.dialog {
        Some(Dialog::SynchronizeFolder { counts: Some(c), .. }) => c.clone(),
        d => panic!("no scanned dialog: {d:?}"),
    }
}

#[test]
fn the_dialog_says_what_changed_in_the_folder() {
    let dir = Scratch::new("counts");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    let c = counts(&h);
    assert_eq!((c.new, c.missing, c.metadata), (1, 1, 0), "{c:?}");
    assert_eq!(in_library(&h), vec!["a.png", "b.png"], "scanning changed nothing");
}

#[test]
fn synchronizing_with_the_defaults_imports_the_new_photos_only() {
    let dir = Scratch::new("defaults");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    synchronize(&mut h);
    assert_eq!(in_library(&h), vec!["a.png", "b.png", "c.png"], "c came in; the missing b stays");
}

#[test]
fn ticking_remove_missing_also_removes_the_missing_photos() {
    let dir = Scratch::new("remove");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    tick_remove_missing(&mut h);
    synchronize(&mut h);
    assert_eq!(in_library(&h), vec!["a.png", "c.png"], "b went to Recently Deleted");
    let r = h.request("engine.execute", json!({"command": "edit.undo", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(in_library(&h), vec!["a.png", "b.png"], "one undo step");
}

#[test]
fn cancel_leaves_the_library_as_it_was() {
    let dir = Scratch::new("cancel");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    h.request("ui.key", json!({"key": "escape"}), T);
    h.step();
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(in_library(&h), vec!["a.png", "b.png"]);
    assert!(h.app.session.folder_changes.is_none(), "the scan is let go");
}

fn shown(h: &Headless, widget: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == widget)
}

#[test]
fn a_folder_with_nothing_to_bring_in_is_only_reported() {
    let dir = Scratch::new("uptodate");
    let mut h = imported_folder(&dir, || {});
    open_and_scan(&mut h, &dir.path("trip"));
    h.step();
    assert!(shown(&h, "button:dialogOk"), "one button, to close it");
    for gone in ["button:dialogCancel", "syncImportNew", "syncRelinkMoved", "syncRemoveMissing", "syncReadMetadata"] {
        assert!(!shown(&h, gone), "nothing to choose: {gone}");
    }
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.ui.dialog.is_none()), "the button closes it");
    assert!(h.app.sync_run.is_none() && h.app.session.activity.list().is_empty(), "and starts nothing");
    assert!(h.app.session.folder_changes.is_none(), "the scan is let go");
    assert_eq!(in_library(&h), vec!["a.png", "b.png"]);
}

#[test]
fn confirming_a_folder_with_nothing_to_bring_in_starts_nothing() {
    let dir = Scratch::new("uptodate-agent");
    let mut h = imported_folder(&dir, || {});
    open_and_scan(&mut h, &dir.path("trip"));
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["upToDate"], true, "{r}");
    assert!(h.app.ui.dialog.is_none());
    assert!(h.app.sync_run.is_none() && h.app.session.activity.list().is_empty(), "no job that does nothing");
    h.step();
    assert!(h.app.session.folder_changes.is_none(), "the scan is let go");
}

#[test]
fn an_agents_scan_is_kept_between_frames() {
    let dir = Scratch::new("agent");
    let mut h = changed_folder(&dir);
    let r = h.request("engine.execute", json!({"command": "folder.scanChanges", "params": {"path": dir.path("trip")}}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..5 {
        h.step();
    }
    assert!(h.app.session.folder_changes.is_some(), "the app doesn't drop a scan it didn't make");
}

#[test]
fn a_scan_gone_stale_in_the_dialog_is_made_again_in_the_background() {
    let dir = Scratch::new("restale");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    tick_remove_missing(&mut h);
    // a photo of the folder changes while the dialog is open
    let a = h.app.session.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id;
    let r = h.request("engine.execute", json!({"command": "photo.rate", "params": {"ids": [a.0], "rating": 3}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], false, "not done on a stale scan: {r}");
    assert_eq!(in_library(&h), vec!["a.png", "b.png"]);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::SynchronizeFolder { .. })), "the dialog stays open");
    let rescanned = h.step_until(T, |h| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { counts: Some(_), .. })));
    assert!(rescanned, "and scans again");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::SynchronizeFolder { remove_missing: true, .. })), "keeping what was ticked");
    synchronize(&mut h);
    assert_eq!(in_library(&h), vec!["a.png", "c.png"], "c came in, the missing b went");
}

#[test]
fn a_stopped_synchronize_says_it_was_stopped_and_stays_at_most_one_step() {
    let dir = Scratch::new("stop");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    let steps = h.app.session.undo.len();
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok());
    // Cancel (what the row's ✕ does), before any frame committed anything
    h.app.sync_run.as_mut().expect("running").cancel();
    assert!(h.step_until(T, |h| h.app.sync_run.is_none()), "it ends");
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("Stopped"), "never 'up to date': {toast:?}");
    assert!(h.app.session.undo.len() <= steps + 1, "what was done by then is at most one step");
}

#[test]
fn a_synchronize_and_an_import_never_run_at_once() {
    let dir = Scratch::new("exclusive");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok());
    // dropping the same folder on the window while the run works would add its files twice
    let r = crate::import::start_paths(&mut h.app, vec![dir.path("trip")]);
    assert!(r.is_err(), "no import while synchronizing: {r:?}");
    assert!(h.step_until(T, |h| h.app.sync_run.is_none()));
    // and the other way round: an import is running (no frame has run since it started)
    let r = crate::import::start_paths(&mut h.app, vec![dir.path("trip/a.png")]);
    assert!(r.is_ok(), "{r:?}");
    assert!(h.app.import.is_some());
    let d = Dialog::SynchronizeFolder {
        path: dir.path("trip"),
        name: "trip".into(),
        disk: false,
        counts: Some(crate::state::SyncCounts { new: 1, ..Default::default() }),
        import_new: true,
        relink_moved: true,
        remove_missing: false,
        read_metadata: false,
    };
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &d);
    assert!(r.is_err() && h.app.sync_run.is_none(), "no synchronize while importing: {r:?}");
}

#[test]
fn a_synchronize_shows_a_row_and_its_cross_stops_it() {
    let dir = Scratch::new("row");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok());
    let rows = h.app.session.activity.list();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].label.as_str()), ("sync", "Synchronizing folder"));
    assert!(rows[0].detail.ends_with("trip"), "the folder as the dialog named it: {:?}", rows[0].detail);
    assert!(rows[0].cancellable);
    // the row's ✕ (as `activity.cancel`): what was done stays, and the end says it was stopped
    h.app.session.activity.cancel(rows[0].id).unwrap();
    assert!(h.step_until(T, |h| h.app.sync_run.is_none()), "it ends");
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("Stopped"), "{toast:?}");
    assert!(h.app.session.activity.list().is_empty(), "the row goes with the run");
}

#[test]
fn the_folders_scan_shows_a_row_and_its_cross_closes_the_dialog() {
    let dir = Scratch::new("scanrow");
    let mut h = changed_folder(&dir);
    // a scan that can't finish until the test lets it: reading the new file waits on a flag
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let g = gate.clone();
    h.app.session.media.file_probe = Some(std::sync::Arc::new(move |_: &str| {
        while !g.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(lightcraft_engine::media::ProbeInfo { format: "PNG".into(), ..Default::default() })
    }));
    crate::sync::open(&mut h.app, &dir.path("trip"), "trip", false).unwrap();
    let rows = h.app.session.activity.list();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].detail.as_str()), ("sync", "Looking for changes", "trip"));
    assert!(rows[0].cancellable);
    h.app.session.activity.cancel(rows[0].id).unwrap();
    h.step();
    assert!(h.app.sync.is_none(), "✕ stops the scan");
    assert!(h.app.ui.dialog.is_none(), "and closes its dialog: there is nothing to show");
    gate.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(h.app.session.activity.list().is_empty());
    h.settle(SETTLE);
    assert!(h.app.ui.dialog.is_none(), "a stopped scan opens nothing later");
}

/// Right-click the Local row of `folder`; whether Synchronize Folder… is in its menu.
fn local_menu_offers_synchronize(h: &mut Headless, folder: &str) -> bool {
    let row = format!("source:local:{folder}");
    assert!(h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| *w == row)), "no row {row}");
    let rect = h.app.widgets.iter().find(|(w, _)| *w == row).map(|(_, r)| *r).unwrap();
    let c = rect.center();
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    h.app.widgets.iter().any(|(w, _)| *w == "folderSynchronize")
}

#[test]
fn a_local_folder_with_library_photos_offers_synchronize() {
    let dir = Scratch::new("local-menu");
    let mut h = changed_folder(&dir);
    std::fs::create_dir_all(dir.path("empty")).unwrap();
    // Local writes paths the way the OS does
    let native = |f: &str| dir.0.join(f).to_string_lossy().to_string();
    for f in ["trip", "empty"] {
        let r = h.request("engine.execute", json!({"command": "local.addRoot", "params": {"path": native(f)}}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.step();
    assert!(!local_menu_offers_synchronize(&mut h, &native("empty")), "no library photos in it: nothing to synchronize");
    // close that menu, then open the other folder's
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(local_menu_offers_synchronize(&mut h, &native("trip")), "the folder holds library photos");
    let r = h.request("ui.clickWidget", json!({"id": "folderSynchronize"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::SynchronizeFolder { .. })), "the dialog opens at once");
    assert!(h.step_until(T, |h| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { counts: Some(_), .. }))));
}

#[test]
fn while_the_folder_is_listed_its_row_says_what_was_listed_and_its_bar_moves_on_its_own() {
    use std::sync::atomic::Ordering::Relaxed;
    let p = lightcraft_engine::sync::SyncProgress::default();
    p.to_check.store(64085, Relaxed);
    p.folders_listed.store(312, Relaxed);
    p.files_listed.store(18402, Relaxed);
    let (done, total, detail) = crate::sync::scan_row(&p, "photos/Various");
    assert_eq!((done, total), (0, 0), "a bar that moves on its own, not 0 of 64085");
    assert_eq!(detail, "Listing folders — folders: 312, files: 18402 · photos/Various");
    // the listing is over: the files and photos checked, of how many
    p.listed.store(true, Relaxed);
    p.checked.store(10, Relaxed);
    assert_eq!(crate::sync::scan_row(&p, "photos/Various"), (10, 64085, "photos/Various".to_string()));
}

// ---- the scan need not be waited for ----
//
// * While the folder is scanned, the dialog's button is Continue in Background: the dialog
//   closes, the scan goes on under its row, and the app is free for other things.
// * When the scan is done nothing pops up: a notice in the activity stack says what was found.
//   A click on it opens the dialog with that result; ✕ lets go of it.
// * A scan that found nothing to do only says so.
// * ✕ on the scan's row stops a background scan too, and no notice follows.

/// A gate for the scan of `h`: it can't finish until the returned flag is set (reading the new
/// file waits on it).
fn gated(h: &mut Headless) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let g = gate.clone();
    h.app.session.media.file_probe = Some(std::sync::Arc::new(move |_: &str| {
        while !g.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(lightcraft_engine::media::ProbeInfo { format: "PNG".into(), ..Default::default() })
    }));
    gate
}

/// Open the dialog on `trip` and press Continue in Background while the scan is held.
fn scan_in_background(h: &mut Headless, dir: &Scratch) {
    crate::sync::open(&mut h.app, &dir.path("trip"), "trip", false).unwrap();
    let hidden = |h: &Headless| h.app.ui.dialog.is_none();
    for _ in 0..5 {
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        if h.step_until(Duration::from_secs(3), hidden) {
            break;
        }
    }
    assert!(h.app.ui.dialog.is_none(), "the dialog closes: {:?}", h.app.ui.dialog);
    assert!(h.app.sync.is_some(), "and the scan goes on");
}

fn has_widget(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click_widget(h: &mut Headless, id: &str, landed: impl Fn(&Headless) -> bool) {
    for _ in 0..5 {
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": id}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        if h.step_until(Duration::from_secs(3), &landed) {
            return;
        }
    }
    panic!("the click on {id} never landed: {:?}", h.app.ui.dialog);
}

#[test]
fn a_scan_continued_in_the_background_ends_in_a_notice_that_opens_its_result() {
    let dir = Scratch::new("background");
    let mut h = changed_folder(&dir);
    let gate = gated(&mut h);
    scan_in_background(&mut h, &dir);
    let rows = h.app.session.activity.list();
    assert_eq!((rows.len(), rows[0].label.as_str()), (1, "Looking for changes"), "its row stays: {rows:?}");

    // meanwhile the app is free: a photo is rated, frames go by
    let id = h.app.session.catalog.photos().find(|p| p.file_name == "a.png").map(|p| p.id.0).unwrap();
    assert!(h.app.run("library.select", json!({"ids": [id]})).is_ok());
    assert!(h.app.run("photo.rate", json!({"rating": 3})).is_ok());
    for _ in 0..5 {
        h.step();
    }
    assert!(h.app.sync.is_some() && h.app.sync_ready.is_none(), "still scanning");

    gate.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(h.step_until(T, |h| h.app.sync.is_none()), "the scan ends");
    h.step();
    h.step();
    assert!(h.app.ui.dialog.is_none(), "nothing pops up: {:?}", h.app.ui.dialog);
    let ready = h.app.sync_ready.clone().expect("a notice waits");
    assert_eq!((ready.counts.new, ready.counts.missing, ready.changes()), (1, 1, 2), "{ready:?}");
    assert!(has_widget(&h, "activity:scanReady"), "shown in the activity stack");
    assert!(h.app.session.activity.list().is_empty(), "the scan's own row is gone");
    // it stays until it is looked at
    for _ in 0..20 {
        h.step();
    }
    assert!(h.app.sync_ready.is_some() && h.app.session.folder_changes.is_some());

    click_widget(&mut h, "activity:scanReady:review", |h| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { counts: Some(_), .. })));
    let c = counts(&h);
    assert_eq!((c.new, c.missing), (1, 1), "the dialog shows what the scan found: {c:?}");
    assert!(h.app.sync_ready.is_none() && h.app.sync.is_none(), "without scanning again");
    // (with the rating undone the folder's photos are as scanned: the scan is still current)
    h.app.session.undo_step().unwrap();
    synchronize(&mut h);
    assert_eq!(in_library(&h), vec!["a.png", "b.png", "c.png"]);
}

#[test]
fn dismissing_the_notice_lets_go_of_what_the_scan_found() {
    let dir = Scratch::new("dismiss");
    let mut h = changed_folder(&dir);
    let gate = gated(&mut h);
    scan_in_background(&mut h, &dir);
    gate.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(h.step_until(T, |h| h.app.sync_ready.is_some()), "a notice waits");

    click_widget(&mut h, "activity:scanReady:dismiss", |h| h.app.sync_ready.is_none());

    assert!(h.app.session.folder_changes.is_none(), "the scan is let go of");
    assert!(h.app.ui.dialog.is_none());
    h.step();
    assert!(!has_widget(&h, "activity:scanReady"));
    assert_eq!(in_library(&h), vec!["a.png", "b.png"], "nothing was changed");
}

#[test]
fn a_background_scan_that_found_nothing_only_says_so() {
    let dir = Scratch::new("background-none");
    let mut h = changed_folder(&dir);
    // bring the library up to date first
    open_and_scan(&mut h, &dir.path("trip"));
    tick_remove_missing(&mut h);
    synchronize(&mut h);
    h.settle(SETTLE);

    crate::sync::open(&mut h.app, &dir.path("trip"), "trip", false).unwrap();
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    // (pressed before the scan can have ended: no frame has run since it started)
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg);
    assert!(r.is_ok(), "{r:?}");
    assert!(h.step_until(T, |h| h.app.sync.is_none()), "the scan ends");
    assert!(h.app.sync_ready.is_none(), "no notice: there is nothing to look at");
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("up to date"), "{toast:?}");
    assert!(h.app.session.folder_changes.is_none());
}

#[test]
fn the_cross_on_its_row_stops_a_background_scan_without_a_notice() {
    let dir = Scratch::new("background-stop");
    let mut h = changed_folder(&dir);
    let gate = gated(&mut h);
    scan_in_background(&mut h, &dir);
    // another dialog is open meanwhile: it is not closed by the scan ending
    h.app.ui.dialog = Some(Dialog::About);
    let rows = h.app.session.activity.list();
    h.app.session.activity.cancel(rows[0].id).unwrap();
    h.step();
    assert!(h.app.sync.is_none(), "✕ stops the scan");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::About)), "{:?}", h.app.ui.dialog);
    gate.store(true, std::sync::atomic::Ordering::Relaxed);
    h.app.ui.dialog = None;
    h.settle(SETTLE);
    assert!(h.app.sync_ready.is_none() && h.app.ui.dialog.is_none(), "a stopped scan says nothing later");
}

#[test]
fn opening_the_dialog_again_replaces_a_waiting_notice() {
    let dir = Scratch::new("background-again");
    let mut h = changed_folder(&dir);
    let gate = gated(&mut h);
    scan_in_background(&mut h, &dir);
    gate.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(h.step_until(T, |h| h.app.sync_ready.is_some()));
    // Synchronize Folder… from the menu once more: a fresh scan, not two answers
    open_and_scan(&mut h, &dir.path("trip"));
    assert!(h.app.sync_ready.is_none(), "the old notice is gone");
    let c = counts(&h);
    assert_eq!((c.new, c.missing), (1, 1), "{c:?}");
}

#[test]
fn a_result_looked_at_after_the_folder_changed_is_scanned_again_not_acted_on() {
    let dir = Scratch::new("background-stale");
    let mut h = changed_folder(&dir);
    let gate = gated(&mut h);
    scan_in_background(&mut h, &dir);
    gate.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(h.step_until(T, |h| h.app.sync_ready.is_some()));
    // the person went on working in the folder before looking
    let id = h.app.session.catalog.photos().find(|p| p.file_name == "a.png").map(|p| p.id.0).unwrap();
    h.app.run("library.select", json!({"ids": [id]})).unwrap();
    h.app.run("photo.rate", json!({"rating": 3})).unwrap();
    crate::sync::review(&mut h.app);
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    let before = in_library(&h);
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg);
    assert!(r.is_err() && h.app.sync_run.is_none(), "what was shown is no longer so: {r:?}");
    assert_eq!(in_library(&h), before);
    assert!(h.app.sync.is_some(), "it is scanned again");
}

#[test]
fn another_folder_is_not_scanned_over_a_background_scan_or_its_notice() {
    let dir = Scratch::new("background-other");
    write_png(&dir.path("other/x.png"), 9);
    let mut h = changed_folder(&dir);
    h.app.session.execute("library.import", &json!({"paths": [dir.path("other")]})).unwrap();
    let gate = gated(&mut h);
    scan_in_background(&mut h, &dir);

    let e = crate::sync::open(&mut h.app, &dir.path("other"), "other", false).unwrap_err();
    assert!(e.contains("still being scanned") && e.contains("trip"), "{e}");
    assert!(h.app.sync.is_some() && h.app.ui.dialog.is_none(), "the scan goes on");

    gate.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(h.step_until(T, |h| h.app.sync_ready.is_some()));
    let e = crate::sync::open(&mut h.app, &dir.path("other"), "other", false).unwrap_err();
    assert!(e.contains("changes found in") && e.contains("trip"), "{e}");
    // and a folder that can't be synchronized at all takes nothing away either
    assert!(crate::sync::open(&mut h.app, &dir.path("nowhere"), "nowhere", false).is_err());
    assert!(h.app.sync_ready.is_some() && h.app.session.folder_changes.is_some(), "the notice still waits");

    // once it is dismissed the other folder can be scanned
    crate::sync::dismiss(&mut h.app);
    assert!(crate::sync::open(&mut h.app, &dir.path("other"), "other", false).is_ok());
}

#[test]
fn what_was_ticked_comes_back_with_the_result() {
    let dir = Scratch::new("background-ticks");
    let mut h = changed_folder(&dir);
    let gate = gated(&mut h);
    crate::sync::open(&mut h.app, &dir.path("trip"), "trip", false).unwrap();
    // a dialog that had scanned before and scans again still has its ticks
    let mut dlg = h.app.ui.dialog.take().expect("the dialog is open");
    if let Dialog::SynchronizeFolder { import_new, remove_missing, .. } = &mut dlg {
        (*import_new, *remove_missing) = (false, true);
    }
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok());
    gate.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(h.step_until(T, |h| h.app.sync_ready.is_some()));
    // another dialog is open: the notice waits rather than take it away
    h.app.ui.dialog = Some(Dialog::About);
    crate::sync::review(&mut h.app);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::About)) && h.app.sync_ready.is_some());
    h.app.ui.dialog = None;
    crate::sync::review(&mut h.app);
    assert!(
        matches!(h.app.ui.dialog, Some(Dialog::SynchronizeFolder { import_new: false, remove_missing: true, counts: Some(_), .. })),
        "{:?}",
        h.app.ui.dialog
    );
    // closing the dialog lets go of the scan, as for one that was never in the background
    h.app.ui.dialog = None;
    h.step();
    assert!(h.app.session.folder_changes.is_none());
}

#[test]
fn a_folder_that_isnt_there_is_a_notice_too_and_another_library_gets_none() {
    let dir = Scratch::new("background-offline");
    let mut h = changed_folder(&dir);
    std::fs::rename(dir.0.join("trip"), dir.0.join("trip-elsewhere")).unwrap();
    crate::sync::open(&mut h.app, &dir.path("trip"), "trip", false).unwrap();
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok());
    assert!(h.step_until(T, |h| h.app.sync_ready.is_some()), "a notice waits");
    assert!(h.app.sync_ready.as_ref().is_some_and(|r| r.counts.offline));

    // File ▸ Open Library: the notice was about the library before
    crate::sync::library_changed(&mut h.app);
    assert!(h.app.sync_ready.is_none() && h.app.session.folder_changes.is_none());
}

#[test]
fn synchronize_waits_a_moment_after_its_button_stopped_being_continue_in_background() {
    let dir = Scratch::new("settle");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    // a click meant for Continue in Background, landing as the scan ends
    let before = in_library(&h);
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.sync_run.is_none() && matches!(h.app.ui.dialog, Some(Dialog::SynchronizeFolder { .. })), "nothing was started");
    assert_eq!(in_library(&h), before);
    assert!(!crate::sync::settled(&h.app, &h.view.ctx));
}
