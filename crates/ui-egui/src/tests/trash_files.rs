//! Move Files to Trash… in the app (see `lightcraft_engine::trash`): the one way out of the
//! library that takes a photo's file along, and it always asks first.
//!
//! The tests use a Trash of their own (a folder in a scratch directory).
//!
//! Scenarios:
//! - Given a photo selected, when Photo ▸ Move Files to Trash… is chosen, then a question says
//!   what will happen and nothing has happened yet; Cancel leaves photo and file; OK takes the
//!   photo out of the library and its file to the Trash, and says so; Undo brings both back.
//! - The same from a photo's menu in the grid, for a photo in the library and for one in
//!   Recently Deleted; from Recently Deleted all of them can go at once.
//! - With "Confirm before moving photos to Recently Deleted" on, the Delete question offers Move Files to Trash… as a
//!   second answer, which leads to the same question.
//! - Given photos on a disk without a Trash, then after the first question, and before any file
//!   moves, a second one asks whether their files may be erased for good; Cancel there leaves
//!   them in the library with their files (the others still go, and nothing more is asked); OK
//!   erases them, and everything is one undo step.
//! - A file that can't be moved stays with its photo, and a notice says why.
//! - Delete itself, with or without its question, still moves photos to Recently Deleted and
//!   touches no file.
//! - The question closes at once and the files move in the background, under a row whose ✕
//!   stops the work after the file in hand; a second Move Files to Trash waits for the first;
//!   quitting waits for the file in hand and keeps what moved.

// (a Unix feature: Move Files to Trash… is not available on Windows or in the browser yet)
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::trash::{Bin, BinError, TrashHere};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::panels::trash_files::TrashAsk;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-trash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        Scratch(dir)
    }
    fn file(&self, rel: &str) -> String {
        let p = self.0.join("photos").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x").unwrap();
        p.to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Holds back files moved through a `slow` folder until it is opened, counting them as they come.
#[derive(Default)]
struct Gate {
    /// (open, how many arrived)
    state: Mutex<(bool, usize)>,
    changed: Condvar,
}

impl Gate {
    fn pass(&self) {
        let mut s = self.state.lock().unwrap();
        s.1 += 1;
        self.changed.notify_all();
        while !s.0 {
            s = self.changed.wait(s).unwrap();
        }
    }
    fn open(&self) {
        self.state.lock().unwrap().0 = true;
        self.changed.notify_all();
    }
    /// How many files arrived so far.
    fn arrived(&self) -> usize {
        self.state.lock().unwrap().1
    }
}

/// Run frames until `n` files arrived at `gate` (the work starts from a frame, once the disks
/// were surveyed). `false`: not within `T`.
fn wait_arrived(h: &mut Headless, gate: &Gate, n: usize) -> bool {
    h.step_until(T, |_| gate.arrived() >= n)
}

/// Whether `path` has a part named `part` (a folder of that name on the way).
fn under(path: &Path, part: &str) -> bool {
    path.components().any(|c| c.as_os_str() == part)
}

/// A Trash that is a folder. Paths through a `nas` folder are on a disk without a Trash (which
/// the disk says before anything moves), through a `far` folder on one that says so only when a
/// file is tried (as on Linux); paths through a `stuck` folder or naming a `stuck` file can't be
/// moved; paths through a `slow` folder wait at its [`Gate`].
struct FolderBin {
    dir: PathBuf,
    next: Mutex<u32>,
    gate: Arc<Gate>,
}

impl Bin for FolderBin {
    fn trash(&self, path: &Path) -> Result<Option<PathBuf>, BinError> {
        if under(path, "nas") || under(path, "far") {
            return Err(BinError::NoTrash);
        }
        if path.file_stem().is_some_and(|s| s == "stuck") || under(path, "stuck") {
            return Err(BinError::Failed("the file is in use".into()));
        }
        if under(path, "slow") {
            self.gate.pass();
        }
        let mut n = self.next.lock().unwrap();
        *n += 1;
        let to = self.dir.join(format!("{}-{}", *n, path.file_name().unwrap().to_string_lossy()));
        std::fs::rename(path, &to).map_err(|e| BinError::Failed(e.to_string()))?;
        Ok(Some(to))
    }

    fn trash_here(&self, folder: &Path) -> TrashHere {
        if under(folder, "nas") { TrashHere::No } else { TrashHere::Unknown }
    }
}

/// The app showing a library of the files `rel` (made in `dir`), the first one selected.
fn app(dir: &Scratch, rel: &[&str]) -> (Headless, Vec<u64>, Vec<String>) {
    let (h, ids, paths, gate) = app_gated(dir, rel);
    gate.open();
    (h, ids, paths)
}

/// [`app`], with the gate files in a `slow` folder wait at, closed.
fn app_gated(dir: &Scratch, rel: &[&str]) -> (Headless, Vec<u64>, Vec<String>, Arc<Gate>) {
    let mut session = lightcraft_engine::Session::new();
    let gate = Arc::new(Gate::default());
    session.trash = Arc::new(FolderBin { dir: dir.0.join("bin"), next: Mutex::new(0), gate: gate.clone() });
    let (mut ids, mut paths) = (Vec::new(), Vec::new());
    for r in rel {
        let path = dir.file(r);
        let id = session.catalog.alloc_photo_id();
        let name = Path::new(&path).file_name().unwrap().to_string_lossy().to_string();
        let p = Photo::new(id, Source::File { path: path.clone() }, &name, "JPEG", 60, 40, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id.0);
        paths.push(path);
    }
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    ask(&mut h, "ui.set", json!({"view": "photoGrid"}));
    run(&mut h, "library.select", json!({"ids": [ids[0]]}));
    (h, ids, paths, gate)
}

fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
    r
}

fn run(h: &mut Headless, command: &str, params: Value) -> Value {
    ask(h, "engine.execute", json!({"command": command, "params": params}))["result"].clone()
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id}));
}

fn in_library(h: &Headless, id: u64) -> bool {
    h.app.session.catalog.photo(PhotoId(id)).is_some()
}

fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// Let the Erase question's button become available (by the app's own clock).
fn wait_for_erase(h: &mut Headless) {
    let Some(Dialog::TrashFiles(TrashAsk::Erase { since, .. })) = h.app.ui.dialog.clone() else { panic!("no Erase question: {:?}", h.app.ui.dialog) };
    let ready = since + crate::panels::trash_files::ERASE_SETTLE_SECONDS + 0.05;
    assert!(h.step_until(T, |h| h.app.now() >= ready), "the app's clock moves on");
    h.step();
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

#[test]
fn move_files_to_trash_asks_first_and_undo_brings_photo_and_file_back() {
    let dir = Scratch::new("ask");
    let (mut h, ids, paths) = app(&dir, &["a.jpg", "b.jpg"]);
    run(&mut h, "dialog.trashFiles", json!({}));
    assert_eq!(
        h.app.ui.dialog,
        Some(Dialog::TrashFiles(TrashAsk::Trash { ids: vec![ids[0]], emptying: false, since: None, library: h.app.session.library_generation() }))
    );
    assert!(in_library(&h, ids[0]) && exists(&paths[0]), "asking does nothing");
    ask(&mut h, "ui.dialog.cancel", json!({}));
    assert!(in_library(&h, ids[0]) && exists(&paths[0]), "Cancel leaves photo and file");
    run(&mut h, "dialog.trashFiles", json!({}));
    click(&mut h, "button:dialogOk");
    assert!(!in_library(&h, ids[0]) && !exists(&paths[0]), "out of the library, the file in the Trash");
    assert!(in_library(&h, ids[1]) && exists(&paths[1]));
    assert_eq!(h.app.ui.dialog, None);
    assert!(toast(&h).contains("Trash"), "it says what it did: {:?}", toast(&h));
    run(&mut h, "edit.undo", json!({}));
    assert!(in_library(&h, ids[0]) && exists(&paths[0]), "Undo brings both back");
}

#[test]
fn a_question_left_open_while_another_library_opens_trashes_and_erases_nothing() {
    for erase in [false, true] {
        let dir = Scratch::new(if erase { "other-library-erase" } else { "other-library" });
        let (mut h, ids, paths) = app(&dir, &["a.jpg", "nas/b.jpg"]);
        run(&mut h, "library.select", json!({"ids": [ids[if erase { 1 } else { 0 }]]}));
        run(&mut h, "dialog.trashFiles", json!({}));
        if erase {
            click(&mut h, "button:dialogOk");
            wait_for_erase(&mut h);
        }
        assert!(matches!(h.app.ui.dialog, Some(Dialog::TrashFiles(_))), "{:?}", h.app.ui.dialog);
        // another library opens meanwhile (the menu bar and agents reach past a dialog): the
        // numbers in the question are other photos there
        h.app.session.open_library(dir.0.join("other/lib"), false).unwrap();
        h.settle(SETTLE);
        if erase {
            // the run that asked closes its question itself
            assert!(!has(&h, "button:dialogOk"), "the Erase question closed");
        } else {
            click(&mut h, "button:dialogOk");
        }
        assert!(toast(&h).contains("Another library was opened"), "{:?}", toast(&h));
        assert!(paths.iter().all(|p| exists(p)), "no file went anywhere");
        assert_eq!(h.app.ui.dialog, None);
        assert!(h.app.trash_run.is_none());
    }
}

#[test]
fn a_photo_s_menu_in_the_grid_offers_it_in_the_library_and_in_recently_deleted() {
    let dir = Scratch::new("menu");
    let (mut h, ids, paths) = app(&dir, &["a.jpg", "b.jpg", "c.jpg"]);
    ask(&mut h, "ui.clickWidget", json!({"id": format!("thumb:{}", ids[0]), "button": "right"}));
    assert!(has(&h, "button:contextTrashFiles"), "beside Delete Photo");
    click(&mut h, "button:contextTrashFiles");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Trash { .. }))));
    ask(&mut h, "ui.dialog.cancel", json!({}));
    // in Recently Deleted: one photo, then all that are left
    run(&mut h, "library.select", json!({"ids": [ids[0], ids[1], ids[2]]}));
    run(&mut h, "photo.delete", json!({}));
    assert!(paths.iter().all(|p| exists(p)), "Delete touches no file");
    run(&mut h, "library.source", json!({"kind": "recentlyDeleted"}));
    run(&mut h, "library.select", json!({"ids": [ids[0]]}));
    ask(&mut h, "ui.clickWidget", json!({"id": format!("thumb:{}", ids[0]), "button": "right"}));
    click(&mut h, "button:contextTrashFiles");
    click(&mut h, "button:dialogOk");
    assert!(!in_library(&h, ids[0]) && !exists(&paths[0]) && exists(&paths[1]));
    ask(&mut h, "ui.clickWidget", json!({"id": format!("thumb:{}", ids[1]), "button": "right"}));
    click(&mut h, "button:contextEmptyTrashFiles");
    assert_eq!(
        h.app.ui.dialog,
        Some(Dialog::TrashFiles(TrashAsk::Trash {
            ids: vec![ids[1], ids[2]],
            emptying: true,
            since: None,
            library: h.app.session.library_generation()
        }))
    );
    let undo = h.app.session.undo.len();
    click(&mut h, "button:dialogOk");
    assert!(!exists(&paths[1]) && !exists(&paths[2]) && !in_library(&h, ids[2]));
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
}

#[test]
fn the_delete_question_offers_move_files_to_trash_as_a_second_answer() {
    let dir = Scratch::new("confirm");
    let (mut h, ids, paths) = app(&dir, &["a.jpg", "b.jpg"]);
    h.app.ui.settings.confirm_delete = true;
    ask(&mut h, "ui.key", json!({"key": "delete"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmDelete { count: 1 }));
    assert!(has(&h, "button:dialogTrashFiles"));
    click(&mut h, "button:dialogTrashFiles");
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Trash { ids: asked, since: Some(_), .. })) if *asked == vec![ids[0]]),
        "the other question: {:?}",
        h.app.ui.dialog
    );
    // a second click of a double-click does not answer it: its button waits a moment
    click(&mut h, "button:dialogOk");
    assert!(in_library(&h, ids[0]) && exists(&paths[0]), "still nothing done");
    ask(&mut h, "ui.dialog.cancel", json!({}));
    // Delete's own answer is what it was: to Recently Deleted, no file touched
    ask(&mut h, "ui.key", json!({"key": "delete"}));
    click(&mut h, "button:dialogOk");
    assert!(h.app.session.catalog.photo(PhotoId(ids[0])).unwrap().deleted && exists(&paths[0]));
    // …and without the question too
    h.app.ui.settings.confirm_delete = false;
    run(&mut h, "library.select", json!({"ids": [ids[1]]}));
    ask(&mut h, "ui.key", json!({"key": "delete"}));
    assert!(h.app.session.catalog.photo(PhotoId(ids[1])).unwrap().deleted && exists(&paths[1]));
    assert_eq!(h.app.ui.dialog, None);
}

#[test]
fn photos_on_a_disk_without_a_trash_get_a_second_question_before_anything_is_erased() {
    let dir = Scratch::new("far-disk");
    let (mut h, ids, paths) = app(&dir, &["a.jpg", "nas/b.jpg", "nas/c.jpg"]);
    run(&mut h, "library.select", json!({"ids": ids}));
    run(&mut h, "dialog.trashFiles", json!({}));
    click(&mut h, "button:dialogOk");
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Erase { ids: asked, .. })) if *asked == vec![ids[1], ids[2]]),
        "the others are asked about: {:?}",
        h.app.ui.dialog
    );
    assert!(in_library(&h, ids[0]) && exists(&paths[0]), "before anything moves, also the file that can go to the Trash");
    // a second click meant for the first question does not answer this one
    click(&mut h, "button:dialogOk");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Erase { .. }))), "its button waits a moment");
    assert!(in_library(&h, ids[1]) && exists(&paths[1]) && exists(&paths[2]), "nothing of them is touched yet");
    wait_for_erase(&mut h);
    click(&mut h, "button:dialogOk");
    assert!(ids.iter().all(|id| !in_library(&h, *id)) && paths.iter().all(|p| !exists(p)));
    assert_eq!(h.app.ui.dialog, None);
    assert!(toast(&h).to_lowercase().contains("erased"), "{:?}", toast(&h));
    // one step: the file in the Trash comes back, the erased ones are shown as missing
    run(&mut h, "edit.undo", json!({}));
    assert!(ids.iter().all(|id| in_library(&h, *id)) && exists(&paths[0]) && !exists(&paths[1]));
}

/// The rows in the activity stack (`activity.list`).
fn rows(h: &mut Headless) -> Vec<Value> {
    let r = h.request("engine.execute", json!({"command": "activity.list", "params": {}}), T);
    r["result"]["tasks"].as_array().cloned().unwrap_or_default()
}

/// Answer the question with OK without waiting for the work: the click is queued input, so
/// frames run until the run has started.
fn confirm_and_go(h: &mut Headless) {
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.trash_run.is_some()), "the work starts: {:?}", h.app.ui.dialog);
}

#[test]
fn the_question_closes_at_once_and_the_files_move_in_the_background() {
    let dir = Scratch::new("background");
    let (mut h, ids, paths, gate) = app_gated(&dir, &["slow/a.jpg", "slow/b.jpg"]);
    run(&mut h, "library.select", json!({"ids": ids}));
    run(&mut h, "dialog.trashFiles", json!({}));
    confirm_and_go(&mut h);
    assert_eq!(h.app.ui.dialog, None, "the question closed with the click");
    let listed = rows(&mut h);
    assert!(listed.iter().any(|r| r["kind"] == "trash" && r["cancellable"] == true), "{listed:?}");
    assert!(ids.iter().all(|id| in_library(&h, *id)), "the photos leave when their files have moved");
    gate.open();
    assert!(h.step_until(SETTLE, |h| h.app.trash_run.is_none()), "the work ends");
    assert!(paths.iter().all(|p| !exists(p)) && ids.iter().all(|id| !in_library(&h, *id)));
    assert!(toast(&h).contains("Files moved to the Trash"), "{:?}", toast(&h));
    assert!(rows(&mut h).is_empty(), "the row goes with the run");
}

#[test]
fn cancelling_the_row_keeps_the_photos_not_reached() {
    let dir = Scratch::new("row-cancel");
    let (mut h, ids, paths, gate) = app_gated(&dir, &["slow/a.jpg", "slow/b.jpg", "slow/c.jpg"]);
    run(&mut h, "library.select", json!({"ids": ids}));
    run(&mut h, "dialog.trashFiles", json!({}));
    confirm_and_go(&mut h);
    assert!(wait_arrived(&mut h, &gate, 1), "the first file is on its way");
    let r = h.request("engine.execute", json!({"command": "activity.cancel", "params": {"all": true}}), T);
    assert_eq!(r["ok"], true, "{r}");
    gate.open();
    assert!(h.step_until(SETTLE, |h| h.app.trash_run.is_none()), "it ends");
    assert!(!in_library(&h, ids[0]) && !exists(&paths[0]), "the file in hand went");
    assert!(ids[1..].iter().all(|id| in_library(&h, *id)) && paths[1..].iter().all(|p| exists(p)), "the others stay");
    assert!(toast(&h).contains("not reached stay in the library: 2"), "{:?}", toast(&h));
}

#[test]
fn cancel_on_the_erase_question_still_moves_the_rest_and_asks_no_more() {
    let dir = Scratch::new("erase-cancel");
    let (mut h, ids, paths) = app(&dir, &["nas/a.jpg", "b.jpg"]);
    run(&mut h, "library.select", json!({"ids": ids}));
    run(&mut h, "dialog.trashFiles", json!({}));
    click(&mut h, "button:dialogOk");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Erase { .. }))), "{:?}", h.app.ui.dialog);
    ask(&mut h, "ui.dialog.cancel", json!({}));
    assert!(h.app.trash_run.is_none());
    assert!(!in_library(&h, ids[1]) && !exists(&paths[1]), "the other one went");
    assert!(in_library(&h, ids[0]) && exists(&paths[0]), "Cancel keeps the photo and its file");
    assert_eq!(h.app.ui.dialog, None, "and is not asked about again");
}

#[test]
fn a_second_move_to_trash_waits_for_the_first() {
    let dir = Scratch::new("second");
    let (mut h, ids, paths, gate) = app_gated(&dir, &["slow/a.jpg", "b.jpg"]);
    run(&mut h, "dialog.trashFiles", json!({}));
    confirm_and_go(&mut h);
    assert!(wait_arrived(&mut h, &gate, 1));
    h.app.session.selection.ids = vec![PhotoId(ids[1])];
    assert!(!crate::menus::ui_enabled(&h.app, "dialog.trashFiles"), "the menu items are off while a run is at work");
    // (the one that is clicked anyway, from a menu of an older frame, says why it does nothing)
    let refused = h.app.run("dialog.trashFiles", json!({})).unwrap_err();
    assert!(refused.contains("already being moved"), "{refused}");
    let r = h.request("engine.execute", json!({"command": "dialog.trashFiles", "params": {}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("already being moved"), "{r}");
    // the Delete question does not offer it either: its answer would lose the Delete
    h.app.ui.dialog = Some(Dialog::ConfirmDelete { count: 1 });
    h.step();
    assert!(has(&h, "button:dialogOk") && !has(&h, "button:dialogTrashFiles"), "no second answer while files move");
    h.app.ui.dialog = None;
    gate.open();
    assert!(h.step_until(SETTLE, |h| h.app.trash_run.is_none()));
    assert!(!exists(&paths[0]) && exists(&paths[1]));
}

/// A disk found to have no Trash only as its files are tried gets the Erase question after the
/// run; a dialog opened meanwhile is not replaced, and the question comes when it closes.
#[test]
fn a_later_erase_question_waits_for_another_dialog() {
    let dir = Scratch::new("later");
    let (mut h, ids, paths, gate) = app_gated(&dir, &["slow/a.jpg", "far/b.jpg"]);
    run(&mut h, "library.select", json!({"ids": ids}));
    run(&mut h, "dialog.trashFiles", json!({}));
    confirm_and_go(&mut h);
    assert!(wait_arrived(&mut h, &gate, 1));
    let r = h.request("engine.execute", json!({"command": "dialog.rename", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::Rename { .. }))));
    gate.open();
    assert!(h.step_until(SETTLE, |h| h.app.trash_run.is_none()));
    assert!(!exists(&paths[0]) && exists(&paths[1]));
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Rename { .. })), "not replaced: {:?}", h.app.ui.dialog);
    ask(&mut h, "ui.dialog.cancel", json!({}));
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Erase { ids: asked, .. })) if *asked == vec![ids[1]]),
        "asked once the window is free: {:?}",
        h.app.ui.dialog
    );
}

#[test]
fn quitting_waits_for_the_file_in_hand() {
    let dir = Scratch::new("quit");
    let (mut h, ids, paths, gate) = app_gated(&dir, &["slow/a.jpg", "slow/b.jpg", "slow/c.jpg"]);
    run(&mut h, "library.select", json!({"ids": ids}));
    run(&mut h, "dialog.trashFiles", json!({}));
    confirm_and_go(&mut h);
    assert!(wait_arrived(&mut h, &gate, 1));
    // (as Quit Anyway does: the stop is set before the file in hand is let go)
    h.app.session.activity.cancel_all();
    let opener = {
        let gate = gate.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            gate.open();
        })
    };
    h.app.finish_background_file_work();
    opener.join().unwrap();
    assert!(h.app.trash_run.is_none());
    assert!(!in_library(&h, ids[0]) && !exists(&paths[0]), "the file in hand went, and its photo left");
    assert_eq!(std::fs::read_dir(dir.0.join("bin")).unwrap().count(), 1, "it is in the Trash");
    assert!(ids[1..].iter().all(|id| in_library(&h, *id)) && paths[1..].iter().all(|p| exists(p)));
}

#[test]
fn a_file_that_can_t_be_moved_stays_with_its_photo_and_a_notice_says_why() {
    let dir = Scratch::new("in-use");
    let (mut h, ids, paths) = app(&dir, &["a.jpg", "stuck.jpg"]);
    run(&mut h, "library.select", json!({"ids": ids}));
    run(&mut h, "dialog.trashFiles", json!({}));
    click(&mut h, "button:dialogOk");
    assert!(!in_library(&h, ids[0]) && in_library(&h, ids[1]) && exists(&paths[1]));
    assert!(toast(&h).contains("in use"), "{:?}", toast(&h));
    // nothing could go at all: the refusal is the notice
    run(&mut h, "library.select", json!({"ids": [ids[1]]}));
    run(&mut h, "dialog.trashFiles", json!({}));
    click(&mut h, "button:dialogOk");
    assert!(in_library(&h, ids[1]) && exists(&paths[1]));
    assert!(toast(&h).contains("in use"), "{:?}", toast(&h));
    assert_eq!(h.app.ui.dialog, None);
}

#[test]
fn nothing_to_move_is_not_asked_about() {
    let dir = Scratch::new("nothing");
    let (mut h, _, _) = app(&dir, &["a.jpg"]);
    // nothing in Recently Deleted
    let r = h.request("engine.execute", json!({"command": "dialog.trashFiles", "params": {"recentlyDeleted": true}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(h.app.ui.dialog, None);
}

/// A key can open the question; no key can be given to the command behind it.
#[test]
fn no_shortcut_can_move_files_without_the_question() {
    let ids: Vec<&str> = crate::shortcuts::bindable().iter().map(|b| b.id).collect();
    assert!(ids.contains(&"dialog.trashFiles") && !ids.contains(&"photo.trashFiles"), "only the question");
}

/// The Erase question, which cannot be undone as far as the files go, and the Trash question
/// beside it, are never the eye-catching default: their confirm stays plain.
#[test]
fn the_questions_of_moving_files_confirm_with_a_plain_button() {
    use crate::panels::dialogs::{ConfirmStyle, confirm_style};
    let dir = Scratch::new("plain");
    let (mut h, ids, _) = app(&dir, &["a.jpg", "nas/b.jpg"]);
    run(&mut h, "library.select", json!({"ids": ids}));
    run(&mut h, "dialog.trashFiles", json!({}));
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Trash { .. }))));
    assert_eq!(confirm_style(h.app.ui.dialog.as_ref().unwrap()), ConfirmStyle::Plain);
    click(&mut h, "button:dialogOk");
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Erase { .. }))));
    assert_eq!(confirm_style(h.app.ui.dialog.as_ref().unwrap()), ConfirmStyle::Plain);
}
