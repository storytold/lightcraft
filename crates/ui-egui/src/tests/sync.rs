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
//! * A Synchronize Folder and an import never run at once: one asked for while the other runs waits in line behind it
//!   and starts by itself, planning its work then (an import checks for duplicates against the library as it is
//!   when it starts; a synchronize scans the folder again).
//! * A synchronize that waited removes, relinks or reads sidecars of only the photos its dialog showed: a photo that
//!   went missing, moved or got a sidecar update while it waited stays as it is, and the end says so.
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

fn sync_dialog(dir: &Scratch, remove_missing: bool) -> Dialog {
    Dialog::SynchronizeFolder {
        path: dir.path("trip"),
        name: "trip".into(),
        disk: false,
        counts: Some(crate::state::SyncCounts { new: 1, ..Default::default() }),
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
    write_png(&dir.path("elsewhere/e.png"), 9);
    let r = crate::import::start_paths(&mut h.app, vec![dir.path("trip"), dir.path("elsewhere/e.png")]).expect("it waits instead of being refused");
    let id = r["queued"].as_u64().unwrap_or_else(|| panic!("queued: {r}"));
    let waiting = h.app.session.activity.waiting();
    assert_eq!(waiting.iter().map(|t| (t.id, t.kind, t.label.as_str())).collect::<Vec<_>>(), vec![(id, "import", "Importing")]);
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.starts_with("Queued")), "{:?}", h.app.ui.toast);
    assert!(h.step_until(SETTLE, idle), "both ran");
    let c = h.app.session.catalog.photos().filter(|p| p.in_library() && p.file_name == "c.png").count();
    assert_eq!(c, 1, "the import found c already in the library when its turn came");
    assert!(in_library(&h).contains(&"e.png".to_string()), "and it ran: {:?}", in_library(&h));
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
    let said = h.app.ui.toast.iter().map(|t| t.0.clone()).collect::<String>();
    assert!(said.contains("Left alone: 1"), "{said}");
}

/// A sidecar dated an hour from now, so that it is newer than the photo whatever the clock's grain.
fn write_sidecar(png: &str, rating: u8) {
    let xmp = std::path::Path::new(png).with_extension("xmp");
    let body = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{rating}"/>
</rdf:RDF></x:xmpmeta>"#
    );
    std::fs::write(&xmp, body).unwrap();
    let f = std::fs::File::options().write(true).open(&xmp).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(3600)).unwrap();
}

fn rating_of(h: &Headless, file: &str) -> u8 {
    h.app.session.catalog.photos().find(|p| p.file_name == file).map(|p| p.rating).unwrap_or_else(|| panic!("no {file}"))
}

/// Given a synchronize that will read the one sidecar update its dialog showed (a), waiting behind an import, when
/// another photo's sidecar (b) changes while it waits, then at its turn it reads a's sidecar only: reading replaces a
/// photo's rating, labels, metadata and edits, which nobody agreed to for b. The end says one more was left alone.
#[test]
fn a_waiting_synchronize_reads_only_the_sidecars_its_dialog_showed() {
    let dir = Scratch::new("reviewed-meta");
    let mut h = imported_folder(&dir, || write_sidecar(&dir.path("trip/a.png"), 4));
    open_and_scan(&mut h, &dir.path("trip"));
    assert_eq!(counts(&h).metadata, 1, "the dialog shows a's update");
    let hold = h.app.session.activity.start("import", "Importing", lightcraft_engine::activity::Cancel::Yes);
    let mut dlg = h.app.ui.dialog.take().expect("the dialog is open");
    if let Dialog::SynchronizeFolder { read_metadata, .. } = &mut dlg {
        *read_metadata = true;
    }
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg);
    assert!(r.as_ref().is_ok_and(|v| v.get("queued").is_some()), "it waits for the import: {r:?}");
    write_sidecar(&dir.path("trip/b.png"), 5);
    drop(hold);
    assert!(h.step_until(SETTLE, idle), "it ran");
    assert_eq!((rating_of(&h, "a.png"), rating_of(&h, "b.png")), (4, 0), "a (shown) was read, b (changed since) was not");
    let said = h.app.ui.toast.iter().map(|t| t.0.clone()).collect::<String>();
    assert!(said.contains("Left alone: 1"), "{said}");
}

/// Given a line that is full, when a synchronize is confirmed while an import runs, then it is refused with the dialog
/// still holding what it showed: confirmed again once there is room, it removes the missing photo it listed.
#[test]
fn a_synchronize_refused_for_a_full_line_keeps_what_its_dialog_showed() {
    use lightcraft_engine::activity::{MAX_WAITING, Request};
    let dir = Scratch::new("full-line");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    tick_remove_missing(&mut h);
    let hold = h.app.session.activity.start("import", "Importing", lightcraft_engine::activity::Cancel::Yes);
    for _ in 0..MAX_WAITING {
        h.app.session.activity.enqueue("import", "Importing", Request { command: "app:import".into(), params: json!({}) }).unwrap();
    }
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_err(), "no room in the line");
    h.app.session.activity.cancel_all();
    assert!(h.app.session.activity.waiting().is_empty());
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg);
    assert!(r.as_ref().is_ok_and(|v| v.get("queued").is_some()), "now it waits: {r:?}");
    drop(hold);
    assert!(h.step_until(SETTLE, idle), "it ran");
    assert_eq!(in_library(&h), vec!["a.png", "c.png"], "b, which the dialog showed missing, was removed");
}

/// Given a Local folder being read, when a synchronize is confirmed, then it waits: the read becomes an import when
/// it finishes, and the two would run side by side.
#[test]
fn a_synchronize_confirmed_while_a_local_folder_is_read_waits() {
    let dir = Scratch::new("browse");
    let mut h = changed_folder(&dir);
    let _scan = crate::import::stall_scan_of(&mut h.app, true);
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &sync_dialog(&dir, false));
    assert!(r.as_ref().is_ok_and(|v| v.get("queued").is_some()), "{r:?}");
    assert!(h.app.sync_run.is_none());
    h.app.scan = None;
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
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok_and(|v| v["queued"].as_u64().is_some()), "it waits");
    drop(hold);
    // one frame: it starts scanning; the next library opens before the scan is looked at
    assert!(h.step_until(T, |h| h.app.sync_queued.is_some()), "it started scanning again");
    h.app.session.open_library(dir.path("other"), false).unwrap();
    assert!(h.step_until(SETTLE, idle), "it ended");
    assert_eq!(h.app.session.catalog.photos().count(), 0, "nothing came into the new library, nothing was removed");
    let said = h.app.ui.toast.iter().map(|t| t.0.clone()).collect::<String>();
    assert!(said.contains("Another library was opened"), "{said}");
}

/// Given a waiting synchronize that has started scanning its folder again, when a photo of the folder changes before the
/// scan is in (the scan did not see it), then nothing is done and it says so: what the scan saw is not what is there.
#[test]
fn a_waiting_synchronize_does_nothing_on_a_scan_the_library_moved_past() {
    let dir = Scratch::new("moved-past");
    let mut h = changed_folder(&dir);
    open_and_scan(&mut h, &dir.path("trip"));
    tick_remove_missing(&mut h);
    let hold = h.app.session.activity.start("import", "Importing", lightcraft_engine::activity::Cancel::Yes);
    let dlg = h.app.ui.dialog.take().expect("the dialog is open");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_ok_and(|v| v["queued"].as_u64().is_some()), "it waits");
    // the scan reads the new file through a probe that waits for the gate: it starts before the rating and ends after it,
    // however fast the machine is (a scan that ended within its first frame would run before the rating was made)
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let g = gate.clone();
    h.app.session.media.file_probe = Some(std::sync::Arc::new(move |p: &str| {
        let t0 = std::time::Instant::now();
        while !g.load(std::sync::atomic::Ordering::Acquire) && t0.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(lightcraft_engine::media::ProbeInfo { width: 8, height: 8, format: "PNG".into(), content_hash: Some(p.to_string()), ..Default::default() })
    }));
    drop(hold);
    assert!(h.step_until(T, |h| h.app.sync_queued.is_some()), "it started scanning again");
    let a = h.app.session.catalog.photos().find(|p| p.file_name == "a.png").unwrap().id;
    let r = h.request("engine.execute", json!({"command": "photo.rate", "params": {"ids": [a.0], "rating": 3}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.sync_queued.is_some(), "the scan is still going when the photo changed");
    gate.store(true, std::sync::atomic::Ordering::Release);
    assert!(h.step_until(SETTLE, idle), "it ended");
    assert_eq!(in_library(&h), vec!["a.png", "b.png"], "the missing b stays, c did not come in");
    let said = h.app.ui.toast.iter().map(|t| t.0.clone()).collect::<String>();
    assert!(said.contains("Photos of the folder changed while it was read"), "{said}");
}

/// Given a synchronize waiting in line, when its folder is gone by its turn, then it ends saying so and changes
/// nothing.
#[test]
fn a_waiting_synchronize_whose_folder_went_away_says_so() {
    let dir = Scratch::new("gone");
    let mut h = changed_folder(&dir);
    let hold = h.app.session.activity.start("import", "Importing", lightcraft_engine::activity::Cancel::Yes);
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &sync_dialog(&dir, false));
    assert!(r.is_ok_and(|v| v["queued"].as_u64().is_some()), "it waits");
    std::fs::rename(dir.path("trip"), dir.path("trip-away")).unwrap();
    drop(hold);
    assert!(h.step_until(SETTLE, idle), "it ended");
    let said = h.app.ui.toast.iter().map(|t| t.0.clone()).collect::<String>();
    assert!(said.contains("isn't there"), "{said}");
    assert!(!in_library(&h).contains(&"c.png".to_string()), "{:?}", in_library(&h));
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
