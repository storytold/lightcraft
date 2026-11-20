//! Move Folder To… in a folder's menu (issue #3140): the folder dialog runs off the UI thread
//! where the host can show it there, as the other file dialogs do (#191).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::pick::{PickKind, PickRequest};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-move-folder-{tag}-{}", std::process::id()));
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
    let img = lightcraft_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 13) as u8, seed, 255]);
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

type Asked = Arc<Mutex<Vec<(PickRequest, Sender<Vec<String>>)>>>;

/// The app with `trip/a.png` imported, a host that shows dialogs elsewhere (each request is
/// recorded with the sender that answers it), and a synchronous folder dialog that only counts
/// how often it was opened on the UI thread.
fn app_with_trip(dir: &Scratch) -> (Headless, Asked, Arc<AtomicUsize>) {
    app_with_trip_in(dir, None)
}

/// [`app_with_trip`], with the photo imported into the library at `library` (`None`: in memory).
fn app_with_trip_in(dir: &Scratch, library: Option<&str>) -> (Headless, Asked, Arc<AtomicUsize>) {
    write_png(&dir.path("trip/a.png"), 1);
    let mut session = lightcraft_engine::Session::new().with_fs();
    if let Some(library) = library {
        session.open_library(library, false).unwrap();
    }
    session.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    let mut app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let asked: Asked = Default::default();
    let a = asked.clone();
    app.services.picker = Some(Box::new(move |req| {
        let (tx, rx) = channel();
        a.lock().unwrap().push((req, tx));
        Ok(rx)
    }));
    let blocking = Arc::new(AtomicUsize::new(0));
    let b = blocking.clone();
    app.services.pick_folder = Some(Box::new(move || {
        b.fetch_add(1, Ordering::Relaxed);
        None
    }));
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    (h, asked, blocking)
}

/// Right-click the row `row` in the sidebar, scrolled into view first.
fn open_row_menu(h: &mut Headless, row: &str) {
    assert!(h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| w == row)), "no row {row}");
    let rect = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r);
    let (Some(before), Some(panel)) = (rect(h, row), rect(h, "panel:left_panel")) else { panic!("no row {row} or left panel") };
    if before.intersect(panel).height() < before.height() - 0.5 {
        crate::tests_panels::scroll_sidebar(h, panel.center().y - before.center().y);
    }
    let c = rect(h, row).expect("the row").center();
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

/// Move Folder To… in the Folders and Local sections asks for the destination off the UI thread,
/// never with the blocking dialog, then moves the folder with the answer and says so.
#[test]
fn move_folder_to_asks_off_the_ui_thread_and_moves_with_the_answer() {
    let dir = Scratch::new("folders");
    std::fs::create_dir_all(dir.path("dest")).unwrap();
    let (mut h, asked, blocking) = app_with_trip(&dir);
    open_row_menu(&mut h, &format!("source:libfolder:{}", dir.path("trip")));
    let r = h.request("ui.clickWidget", json!({"id": "folderMoveTo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(blocking.load(Ordering::Relaxed), 0, "the UI thread never waits on a dialog");
    {
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "one dialog");
        assert_eq!(asked[0].0.kind, PickKind::Folder);
    }
    assert!(std::path::Path::new(&dir.path("trip/a.png")).exists(), "nothing moves before the answer");
    // the user chooses the destination: the folder moves there and its photo follows
    asked.lock().unwrap()[0].1.send(vec![dir.path("dest")]).unwrap();
    h.settle(SETTLE);
    assert!(h.app.pending_picks.is_empty());
    assert!(std::path::Path::new(&dir.path("dest/trip/a.png")).exists(), "moved on disk");
    assert!(!std::path::Path::new(&dir.path("trip")).exists());
    let moved = h.app.session.catalog.photos().filter(|p| p.in_library()).all(|p| match &p.source {
        lightcraft_catalog::Source::File { path } => path.replace('\\', "/").starts_with(&dir.path("dest/trip")),
        lightcraft_catalog::Source::Demo { .. } => false,
    });
    assert!(moved, "the photo is relinked");
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("1 photo(s) relinked"), "{toast:?}");

    // the same from Local (paths written the way the OS does): back to where it was
    let moved_trip = dir.0.join("dest").join("trip").to_string_lossy().to_string();
    let r = h.request("engine.execute", json!({"command": "local.addRoot", "params": {"path": moved_trip}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    open_row_menu(&mut h, &format!("source:local:{moved_trip}"));
    let r = h.request("ui.clickWidget", json!({"id": "folderMoveTo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(blocking.load(Ordering::Relaxed), 0);
    assert_eq!(asked.lock().unwrap().len(), 2, "its own dialog");
    asked.lock().unwrap()[1].1.send(vec![dir.0.to_string_lossy().to_string()]).unwrap();
    h.settle(SETTLE);
    assert!(std::path::Path::new(&dir.path("trip/a.png")).exists(), "moved back");
}

/// Issue #3140: while Move Folder To… waits for its destination, the library can't be switched
/// (the move would run in the other library, moving the folder with nothing to relink). The answer
/// moves the folder in the library it was asked in; then switching works again.
#[test]
fn no_library_switch_while_move_folder_to_waits_for_its_destination() {
    let dir = Scratch::new("switch");
    std::fs::create_dir_all(dir.path("dest")).unwrap();
    let (first, other) = (dir.path("First"), dir.path("Other"));
    let (mut h, asked, _) = app_with_trip_in(&dir, Some(&first));
    open_row_menu(&mut h, &format!("source:libfolder:{}", dir.path("trip")));
    let r = h.request("ui.clickWidget", json!({"id": "folderMoveTo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(asked.lock().unwrap().len(), 1, "the dialog is up");
    let library = |h: &Headless| h.app.session.library.as_ref().map(|l| l.dir.to_string_lossy().replace('\\', "/"));
    // another library (from the control channel or an agent, say) is refused meanwhile
    let e = h.app.run("app.openLibrary", json!({"path": other})).expect_err("no switch while the dialog is up");
    assert!(e.contains("Move Folder To…"), "{e}");
    assert_eq!(library(&h), Some(first.clone()), "the first library stays open");
    // the answer moves the folder there, and its photo follows
    asked.lock().unwrap()[0].1.send(vec![dir.path("dest")]).unwrap();
    h.settle(SETTLE);
    assert!(h.app.pending_picks.is_empty());
    assert!(std::path::Path::new(&dir.path("dest/trip/a.png")).exists(), "moved on disk");
    assert_eq!(library(&h), Some(first));
    let relinked = h.app.session.catalog.photos().filter(|p| p.in_library()).all(|p| match &p.source {
        lightcraft_catalog::Source::File { path } => path.replace('\\', "/").starts_with(&dir.path("dest/trip")),
        lightcraft_catalog::Source::Demo { .. } => false,
    });
    assert!(relinked, "the photo is relinked in the first library");
    // and now the switch goes through
    h.app.run("app.openLibrary", json!({"path": other})).expect("the switch works once the move is done");
    assert_eq!(library(&h), Some(other));
}

/// Issue #3140: while Open Library… waits for its folder, Move Folder To… can't start (both
/// dialogs answer in the order opened, so the library could switch before the move ran, which
/// would then move the folder in the other library with nothing to relink): the item is disabled,
/// no destination is asked for, and the folder stays. Once the library dialog is cancelled it works.
#[test]
fn move_folder_to_waits_for_open_library_to_finish() {
    let dir = Scratch::new("after-open");
    std::fs::create_dir_all(dir.path("dest")).unwrap();
    let (mut h, asked, blocking) = app_with_trip(&dir);
    assert_eq!(h.app.run("app.openLibrary", json!({})).ok(), Some(serde_json::Value::Null));
    assert_eq!(asked.lock().unwrap().len(), 1, "the Open Library dialog is up");
    open_row_menu(&mut h, &format!("source:libfolder:{}", dir.path("trip")));
    let _ = h.request("ui.clickWidget", json!({"id": "folderMoveTo"}), T);
    h.settle(SETTLE);
    assert_eq!(asked.lock().unwrap().len(), 1, "no destination is asked for");
    assert_eq!(blocking.load(Ordering::Relaxed), 0, "not on the UI thread either");
    assert!(!h.app.pending_picks.iter().any(|p| p.command == "folder.move"), "nothing waits to move");
    assert!(std::path::Path::new(&dir.path("trip/a.png")).exists(), "the folder stays");
    // the Open Library dialog is cancelled: Move Folder To… asks for its destination again
    asked.lock().unwrap()[0].1.send(vec![]).unwrap();
    h.settle(SETTLE);
    assert!(h.app.pending_picks.is_empty());
    open_row_menu(&mut h, &format!("source:libfolder:{}", dir.path("trip")));
    let r = h.request("ui.clickWidget", json!({"id": "folderMoveTo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(asked.lock().unwrap().len(), 2, "the destination is asked for");
    assert!(h.app.pending_picks.iter().any(|p| p.command == "folder.move"));
}
