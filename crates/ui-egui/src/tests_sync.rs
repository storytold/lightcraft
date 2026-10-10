//! Synchronize Folder in the app (see `dac_engine::sync`).
//!
//! Scenarios, in the words of someone whose folder changed outside the app:
//!
//! * Right-click a folder in the sidebar's Folders section ▸ Synchronize Folder…: a dialog opens
//!   at once and scans the folder without holding up the app, then says how many photos are new,
//!   missing, or have metadata updates.
//! * Synchronize with the defaults imports the new photos and leaves missing ones alone.
//! * Ticking "Remove missing photos" also moves those to Recently Deleted.
//! * Cancel leaves the library as it was.
//! * Synchronize hands the work to the background at once: the dialog closes, the app keeps
//!   answering, and the photos arrive over the next frames under a row in the activity stack, as one undo
//!   step.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{DacApp, Services};

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
    let img = dac_raster::Rgba8 { width: w, height: h, data };
    let bytes = dac_codecs::encode_png(&dac_codecs::EncodeImage::rgba8(&img), &dac_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The app with `trip/a.png` and `trip/b.png` imported, then `c.png` added and `b.png` deleted
/// on disk behind its back.
fn changed_folder(dir: &Scratch) -> Headless {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let mut session = dac_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    write_png(&dir.path("trip/c.png"), 3);
    std::fs::remove_file(dir.path("trip/b.png")).unwrap();
    let app = DacApp::new(session, Services { png: None, ..Default::default() });
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
    let rect = h.app.widgets.iter().find(|(w, _)| *w == row).map(|(_, r)| *r).unwrap_or_else(|| panic!("no row {row}"));
    let c = rect.center();
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
        counts: Some(Default::default()),
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
        Ok(dac_engine::media::ProbeInfo { format: "PNG".into(), ..Default::default() })
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
