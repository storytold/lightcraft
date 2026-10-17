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
//! * Synchronize hands the work to the background at once: the dialog closes, the app keeps
//!   answering, and the photos arrive over the next frames under a row in the activity stack, as one undo
//!   step.
//! * A Synchronize Folder and an import never run at once: one asked for while the other runs waits in line behind it
//!   and starts by itself, planning its work then (an import checks for duplicates against the library as it is
//!   when it starts; a synchronize scans the folder again).
//! * A synchronize that waited removes or relinks only the photos its dialog showed: a photo that went missing while
//!   it waited stays, and the end says so.
//! * The dialog's own scan doesn't hold up an import: it only reads.

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
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    write_png(&dir.path("trip/c.png"), 3);
    std::fs::remove_file(dir.path("trip/b.png")).unwrap();
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

fn sync_dialog(dir: &Scratch, remove_missing: bool) -> Dialog {
    Dialog::SynchronizeFolder {
        path: dir.path("trip"),
        name: "trip".into(),
        disk: false,
        counts: Some(Default::default()),
        import_new: true,
        relink_moved: true,
        remove_missing,
        read_metadata: false,
    }
}

/// Every task done, nothing waiting.
fn idle(h: &Headless) -> bool {
    h.app.sync_run.is_none() && h.app.import.is_none() && h.app.session.activity.list().is_empty()
}

/// Given a synchronize running, when the same folder is dropped on the window, then the import waits in line behind
/// it and starts by itself once it ends, adding nothing twice (it plans its work then). The other way round: a
/// synchronize confirmed while an import runs waits for it, and then synchronizes.
#[test]
fn a_synchronize_and_an_import_wait_for_each_other() {
    let dir = Scratch::new("exclusive");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok());
    // dropping the same folder on the window while the run works would add its files twice
    let r = crate::import::start_paths(&mut h.app, vec![dir.path("trip")]).expect("it waits instead of being refused");
    let id = r["queued"].as_u64().unwrap_or_else(|| panic!("queued: {r}"));
    let waiting = h.app.session.activity.waiting();
    assert_eq!(waiting.iter().map(|t| (t.id, t.kind, t.label.as_str())).collect::<Vec<_>>(), vec![(id, "import", "Importing")]);
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.starts_with("Queued")), "{:?}", h.app.ui.toast);
    assert!(h.step_until(SETTLE, idle), "both ran");
    let c = h.app.session.catalog.photos().filter(|p| p.in_library() && p.file_name == "c.png").count();
    assert_eq!(c, 1, "the import found c already in the library when its turn came");
    // and the other way round: an import is running (no frame has run since it started)
    write_png(&dir.path("trip/d.png"), 4);
    let r = crate::import::start_paths(&mut h.app, vec![dir.path("trip/a.png")]);
    assert!(r.as_ref().is_ok_and(|v| v.get("importing").is_some()), "{r:?}");
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &sync_dialog(&dir, false));
    assert!(r.as_ref().is_ok_and(|v| v.get("queued").is_some()), "the synchronize waits: {r:?}");
    assert!(h.app.sync_run.is_none());
    assert!(h.step_until(SETTLE, idle), "both ran");
    assert!(in_library(&h).contains(&"d.png".to_string()), "the synchronize scanned the folder at its turn: {:?}", in_library(&h));
}

/// Given a synchronize that will remove the one missing photo its dialog showed (b), waiting behind an import, when
/// another photo of the folder (a) goes missing while it waits, then at its turn it removes b only: a stays, and the
/// end says one more is missing and was left alone.
#[test]
fn a_waiting_synchronize_removes_only_the_missing_photos_its_dialog_showed() {
    let dir = Scratch::new("reviewed");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    tick_remove_missing(&mut h);
    let hold = h.app.session.activity.start("import", "Importing", lightcraft_engine::activity::Cancel::Yes);
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg);
    assert!(r.as_ref().is_ok_and(|v| v.get("queued").is_some()), "it waits for the import: {r:?}");
    std::fs::remove_file(dir.path("trip/a.png")).unwrap();
    drop(hold);
    assert!(h.step_until(SETTLE, idle), "it ran");
    assert_eq!(in_library(&h), vec!["a.png", "c.png"], "b (shown) was removed, a (gone since) was not");
    let said = h.app.session.activity.finished().into_iter().map(|f| format!("{} {}", f.summary, f.detail)).collect::<String>()
        + &h.app.ui.toast.iter().map(|t| t.0.clone()).collect::<String>();
    assert!(said.contains("1 more missing"), "{said}");
}

/// Given a waiting synchronize that has started scanning its folder again, when another library is opened before the
/// scan is in, then it stops without touching the new library: its photo ids were the old library's.
#[test]
fn a_waiting_synchronize_never_acts_on_another_library() {
    let dir = Scratch::new("switch");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    tick_remove_missing(&mut h);
    let hold = h.app.session.activity.start("import", "Importing", lightcraft_engine::activity::Cancel::Yes);
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    let id = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).ok().and_then(|v| v["queued"].as_u64()).expect("it waits");
    drop(hold);
    // one frame: it starts scanning; the next library opens before the scan is looked at
    assert!(h.step_until(T, |h| h.app.sync_queued.is_some()), "it started scanning again");
    h.app.session.open_library(dir.path("other"), false).unwrap();
    assert!(h.step_until(SETTLE, idle), "it ended");
    assert_eq!(h.app.session.catalog.photos().count(), 0, "nothing came into the new library, nothing was removed");
    let row = h.app.session.activity.finished().into_iter().find(|f| f.id == id).expect("its row");
    assert_eq!((row.ending, row.retryable), (lightcraft_engine::activity::Ending::Stopped, false), "{row:?}");
}

/// Given a synchronize waiting in line, when its folder is gone by its turn, then it ends saying so, and can be
/// retried once the folder is back.
#[test]
fn a_waiting_synchronize_whose_folder_went_away_can_be_retried() {
    let dir = Scratch::new("gone");
    let mut h = changed_folder(&dir);
    let hold = h.app.session.activity.start("import", "Importing", lightcraft_engine::activity::Cancel::Yes);
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &sync_dialog(&dir, false));
    let id = r.ok().and_then(|v| v["queued"].as_u64()).expect("it waits");
    std::fs::rename(dir.path("trip"), dir.path("trip-away")).unwrap();
    drop(hold);
    assert!(h.step_until(SETTLE, idle), "it ended");
    let row = h.app.session.activity.finished().into_iter().find(|f| f.id == id).expect("a row for it, under the id it waited with");
    assert_eq!((row.ending, row.retryable), (lightcraft_engine::activity::Ending::Failed, true), "{row:?}");
    std::fs::rename(dir.path("trip-away"), dir.path("trip")).unwrap();
    let r = h.request("engine.execute", json!({"command": "activity.retry", "params": {"id": id}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(SETTLE, idle), "it ran again");
    assert!(in_library(&h).contains(&"c.png".to_string()), "{:?}", in_library(&h));
}

/// The dialog's scan only reads: an import dropped on the window while it scans starts at once.
#[test]
fn the_dialogs_scan_doesnt_hold_up_an_import() {
    let dir = Scratch::new("scan-free");
    let mut h = changed_folder(&dir);
    crate::sync::open(&mut h.app, &dir.path("trip"), "trip", false).unwrap();
    let r = crate::import::start_paths(&mut h.app, vec![dir.path("trip/c.png")]);
    assert!(r.as_ref().is_ok_and(|v| v.get("importing").is_some()), "{r:?}");
    h.settle(SETTLE);
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
    let row = h.app.session.activity.last_finished().expect("a stopped run leaves a row");
    assert_eq!((row.label.as_str(), row.ending), ("Synchronizing folder", lightcraft_engine::activity::Ending::Stopped));
    assert!(row.summary.contains("trip: Stopped"), "it names the folder and says how it ended: {:?}", row.summary);
}

#[test]
fn a_synchronize_that_ran_to_its_end_leaves_a_row_saying_what_it_did() {
    let dir = Scratch::new("donerow");
    let mut h = changed_folder(&dir);
    h.app.session.activity.keep_after(0);
    open_and_scan(&mut h, &dir.path("trip"));
    synchronize(&mut h);
    let row = h.app.session.activity.last_finished().expect("a row for the run");
    assert_eq!((row.label.as_str(), row.ending), ("Synchronizing folder", lightcraft_engine::activity::Ending::Done));
    assert!(row.summary.ends_with("trip: Imported 1"), "it names the folder and says what it did: {:?}", row.summary);
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
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].detail.as_str()), ("scan", "Looking for changes", "trip"));
    assert!(rows[0].cancellable);
    h.app.session.activity.cancel(rows[0].id).unwrap();
    h.step();
    assert!(h.app.sync.is_none(), "✕ stops the scan");
    assert!(h.app.ui.dialog.is_none(), "and closes its dialog: there is nothing to show");
    let row = h.app.session.activity.last_finished().expect("a stopped scan leaves a row");
    assert_eq!((row.label.as_str(), row.ending), ("Looking for changes", lightcraft_engine::activity::Ending::Stopped));
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

// ---- Synchronize N Folders

/// Which folders Synchronize N Folders goes through: in the sidebar's order as drawn (a folder's insides before the
/// next folder, whatever the names' characters), each once; a folder inside another chosen one is taken in by it, and
/// a whole disk is listed but left out, saying why.
#[test]
fn several_folders_go_in_the_sidebars_order_without_those_inside_another() {
    let sidebar = ["/pics", "/pics/2024", "/pics/2024/Jan", "/pics/2024-02", "/pics/Archive", "/pics/home", "/pics/home/garden", "/Volumes/Card"];
    let chosen = ["/pics/2024-02", "/pics/home/garden", "/pics/2024/Jan", "/pics/home", "/pics/Archive", "/pics/home", "/Volumes/Card"];
    let (folders, covered) = crate::sync::many_folders(&chosen.map(String::from), &sidebar.map(String::from));
    let listed: Vec<(&str, bool)> = folders.iter().map(|f| (f.path.as_str(), f.skipped.is_some())).collect();
    assert_eq!(
        listed,
        vec![("/pics/2024/Jan", false), ("/pics/2024-02", false), ("/pics/Archive", false), ("/pics/home", false), ("/Volumes/Card", true)]
    );
    assert_eq!(folders[3].name, "pics/home");
    assert_eq!(covered.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["/pics/home/garden"]);
}

/// Given a folder taken in by another chosen one, when that one can't be synchronized after all (it holds disks, its
/// scan died), then the folder comes back into the dialog right after it, to be scanned itself.
#[test]
fn a_folder_taken_in_by_one_that_cant_be_synchronized_comes_back() {
    let chosen = ["/mnt", "/mnt/card/2024", "/mnt/card/2024/day 1", "/pics"].map(String::from);
    let (mut folders, mut covered) = crate::sync::many_folders(&chosen, &chosen);
    assert_eq!(folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["/mnt", "/pics"]);
    crate::sync::skip(&mut folders, &mut covered, 0, "holds disks".into());
    assert_eq!(folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["/mnt", "/mnt/card/2024", "/pics"]);
    assert_eq!(covered.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["/mnt/card/2024/day 1"], "still taken in by 2024");
}

/// `trip` (c new, b missing), `home` (g new), `still` (unchanged) and `gone` (its folder deleted), all imported.
fn changed_folders(dir: &Scratch) -> Headless {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    write_png(&dir.path("home/f.png"), 6);
    write_png(&dir.path("still/s.png"), 9);
    write_png(&dir.path("gone/x.png"), 12);
    let mut session = lightcraft_engine::Session::new().with_fs();
    let dirs: Vec<String> = ["trip", "home", "still", "gone"].iter().map(|d| dir.path(d)).collect();
    session.execute("library.import", &json!({"paths": dirs})).unwrap();
    write_png(&dir.path("trip/c.png"), 3);
    std::fs::remove_file(dir.path("trip/b.png")).unwrap();
    write_png(&dir.path("home/g.png"), 7);
    std::fs::remove_dir_all(dir.path("gone")).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1200.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

/// Choose `folders` in the sidebar, right-click the first one's row and pick Synchronize N Folders…; wait for the
/// dialog's scans.
fn open_many_and_scan(h: &mut Headless, folders: &[String]) {
    let r = h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "libraryFolder", "paths": folders}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let row = format!("source:libfolder:{}", folders[0]);
    assert!(h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| *w == row)), "no row {row}");
    let c = h.app.widgets.iter().find(|(w, _)| *w == row).map(|(_, r)| r.center()).unwrap();
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": "sideMenu:folders:synchronize"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::SynchronizeFolders { .. })), "the dialog opens at once: {:?}", h.app.ui.dialog);
    assert!(h.step_until(T, |h| h.app.ui.dialog.as_ref().is_some_and(crate::sync::many_scanned)), "every folder was scanned");
}

fn many_folders_of(h: &Headless) -> Vec<crate::state::SyncFolder> {
    match &h.app.ui.dialog {
        Some(Dialog::SynchronizeFolders { folders, .. }) => folders.clone(),
        other => panic!("not the folders' dialog: {other:?}"),
    }
}

/// Given four folders chosen in the sidebar (two changed, one unchanged, one gone), when Synchronize 4 Folders… is
/// picked, then one dialog scans them one after another and says what changed in each; and Synchronize (with Remove
/// missing ticked) puts the two changed ones in line, in the sidebar's order, each named in its row, and they run one
/// after another: the new photos come in, the missing one goes, and the unchanged and gone folders are left alone.
#[test]
fn several_folders_are_scanned_in_one_dialog_and_synchronized_one_after_another() {
    let dir = Scratch::new("many");
    let mut h = changed_folders(&dir);
    let chosen: Vec<String> = ["trip", "still", "gone", "home"].iter().map(|d| dir.path(d)).collect();
    open_many_and_scan(&mut h, &chosen);
    let folders = many_folders_of(&h);
    let found: Vec<(String, Option<(bool, usize, usize)>)> = folders
        .iter()
        .map(|f| (f.path.rsplit('/').next().unwrap_or("").to_string(), f.counts.as_ref().map(|c| (c.offline, c.new, c.missing))))
        .collect();
    assert_eq!(
        found,
        vec![
            ("gone".to_string(), Some((true, 0, 0))),
            ("home".to_string(), Some((false, 1, 0))),
            ("still".to_string(), Some((false, 0, 0))),
            ("trip".to_string(), Some((false, 1, 1))),
        ]
    );
    if let Some(Dialog::SynchronizeFolders { remove_missing, .. }) = &mut h.app.ui.dialog {
        *remove_missing = true;
    }
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).expect("it synchronizes");
    assert_eq!(r["queued"].as_array().map(Vec::len), Some(2), "home and trip only: {r}");
    let waiting: Vec<(&str, String)> = h.app.session.activity.waiting().iter().map(|t| (t.kind, t.detail.clone())).collect();
    assert_eq!(waiting, vec![("sync", folders[1].name.clone()), ("sync", folders[3].name.clone())], "in the sidebar's order, named");
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("2 folders")), "{:?}", h.app.ui.toast);
    assert!(h.step_until(SETTLE, idle), "both ran");
    assert_eq!(in_library(&h), vec!["a.png", "c.png", "f.png", "g.png", "s.png", "x.png"], "b went; the gone folder's x stays");
}

/// Given the folders' dialog scanned in one library, when another library is opened before Synchronize, then it does
/// nothing: the photos it found are the other library's.
#[test]
fn the_folders_dialog_acts_only_in_the_library_it_was_opened_in() {
    let dir = Scratch::new("many-switch");
    let mut h = changed_folders(&dir);
    open_many_and_scan(&mut h, &[dir.path("trip"), dir.path("home")]);
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    h.app.session.open_library(dir.path("other"), false).unwrap();
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg);
    assert!(r.is_err(), "{r:?}");
    assert!(h.app.session.activity.waiting().is_empty(), "nothing waits");
}

/// The folders' scans have one row; its ✕ stops them and closes the dialog, which then opens nothing later.
#[test]
fn the_folders_scans_row_cross_closes_their_dialog() {
    let dir = Scratch::new("many-cross");
    let mut h = changed_folders(&dir);
    crate::sync::open_many(&mut h.app, &[dir.path("trip"), dir.path("home")]).unwrap();
    let rows = h.app.session.activity.list();
    assert_eq!((rows.len(), rows[0].kind, rows[0].label.as_str()), (1, "scan", "Looking for changes"), "{rows:?}");
    h.app.session.activity.cancel(rows[0].id).unwrap();
    h.step();
    assert!(h.app.sync_scans.is_none() && h.app.ui.dialog.is_none());
    let row = h.app.session.activity.last_finished().expect("a stopped scan leaves a row");
    assert_eq!(row.ending, lightcraft_engine::activity::Ending::Stopped);
    h.settle(SETTLE);
    assert!(h.app.ui.dialog.is_none(), "a stopped scan opens nothing later");
}
