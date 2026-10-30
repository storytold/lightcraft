//! Locating a library folder that is somewhere else now, in the app (see [`crate::relocate`];
//! issue #1112).
//!
//! Scenarios, in the words of someone whose folder is not where the library looks for it:
//!
//! * Synchronize Folder… on a folder that isn't there says so, and its button is Locate Missing
//!   Folder…: choose where the folder is now, the photos point there, and the dialog scans the
//!   folder at its new place (the "isn't there" message is gone).
//! * Choosing a folder that holds none of the photos changes nothing and says why; the dialog
//!   stays, to choose again.
//! * Update Folder Location… on a Folders row does the same without the dialog.
//! * When only some of the photos are in the chosen folder, a dialog asks first; Cancel leaves
//!   the library as it was.
//! * The files are looked for in the background under a row in the activity stack; its ✕ stops
//!   it and nothing is changed.
//! * A dialog opened meanwhile is not taken away by the question, and a Synchronize dialog that
//!   was closed is not opened again.
//!
//! More scenarios, from the review of the pull request, are at the end.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::relocate::Request;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-relocate-{tag}-{}", std::process::id()));
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

/// The app with `trip/a.png` and `trip/b.png` imported, then the folder moved to
/// `archive/journey` behind its back. The folder dialog answers `chosen` (a path below the
/// scratch folder).
fn moved_folder(dir: &Scratch, chosen: &str) -> Headless {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    std::fs::create_dir_all(dir.0.join("archive")).unwrap();
    std::fs::rename(dir.0.join("trip"), dir.0.join("archive/journey")).unwrap();
    let chosen = dir.path(chosen);
    let png = |img: &lightcraft_raster::Rgba8| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let services = Services {
        png: Some(Box::new(png)),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        pick_folder: Some(Box::new(move || Some(chosen.clone()))),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(session, services), [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

/// Where the library looks for its photos' files.
fn files(h: &Headless) -> Vec<String> {
    let mut v: Vec<String> = h
        .app
        .session
        .catalog
        .photos()
        .filter_map(|p| match &p.source {
            lightcraft_catalog::Source::File { path } => Some(path.replace('\\', "/")),
            _ => None,
        })
        .collect();
    v.sort();
    v
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

/// With `LIGHTCRAFT_TEST_SHOTS` set, save what is on screen there as `name` (to look at the result).
fn shot(h: &mut Headless, name: &str) {
    if let Ok(p) = std::env::var("LIGHTCRAFT_TEST_SHOTS") {
        let r = h.request("ui.screenshot", json!({"path": std::path::Path::new(&p).join(name).to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
}

/// Right-click the folder's row in Folders and choose the item with widget id `item`.
fn row_menu(h: &mut Headless, folder: &str, item: &str) {
    let row = format!("source:libfolder:{folder}");
    assert!(h.step_until(T, |h| h.app.widgets.iter().any(|(w, _)| *w == row)), "no row {row}");
    let rect = h.app.widgets.iter().find(|(w, _)| *w == row).map(|(_, r)| *r).unwrap_or_else(|| panic!("no row {row}"));
    let c = rect.center();
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": item}), T);
    assert_eq!(r["ok"], true, "{item}: {r}");
    h.step();
}

/// Synchronize Folder… on the folder, scanned: it isn't there.
fn open_offline(h: &mut Headless, folder: &str) {
    row_menu(h, folder, "folderSynchronize");
    let scanned = h.step_until(T, |h| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { counts: Some(_), .. })));
    assert!(scanned, "the scan finished: {:?}", h.app.ui.dialog);
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { counts: Some(c), .. }) if c.offline), "{:?}", h.app.ui.dialog);
}

/// Press the dialog's button and wait until the click has landed (`landed`). A click is queued
/// input: on a loaded machine the dialog can still be settling into place when it arrives, so it
/// is clicked again until it lands.
fn press_ok(h: &mut Headless, landed: impl Fn(&Headless) -> bool) {
    for _ in 0..5 {
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{r}");
        if h.step_until(Duration::from_secs(3), &landed) {
            return;
        }
    }
    panic!("the button never took: {:?} · {:?}", h.app.ui.dialog, toast(h));
}

#[test]
fn a_folder_that_isnt_there_is_located_from_its_dialog_and_scanned_at_its_new_place() {
    let dir = Scratch::new("dialog");
    let mut h = moved_folder(&dir, "archive/journey");
    let steps = h.app.session.undo.len();
    open_offline(&mut h, &dir.path("trip"));
    shot(&mut h, "locate-offline.png");

    // the dialog's button is Locate Missing Folder…; the folder dialog answers `archive/journey`
    let now = dir.path("archive/journey");
    press_ok(
        &mut h,
        |h| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { path, counts: Some(c), .. }) if path.replace('\\', "/") == now && !c.offline),
    );

    assert_eq!(files(&h), [dir.path("archive/journey/a.png"), dir.path("archive/journey/b.png")], "the photos point at the folder chosen");
    assert_eq!(h.app.session.undo.len(), steps + 1, "one undo step");
    assert!(toast(&h).contains("Folder located"), "{:?}", toast(&h));
    shot(&mut h, "locate-done.png");
    assert!(h.app.relocate.is_none() && h.app.session.activity.list().iter().all(|r| r.kind != "relocate"), "its row is gone");
    match &h.app.ui.dialog {
        Some(Dialog::SynchronizeFolder { counts: Some(c), .. }) => {
            assert_eq!((c.new, c.missing, c.moved), (0, 0, 0), "the library is up to date with the folder: {c:?}")
        }
        d => panic!("{d:?}"),
    }
}

#[test]
fn a_folder_that_holds_none_of_the_photos_is_not_taken_and_the_dialog_stays() {
    let dir = Scratch::new("wrong");
    // one level too high
    let mut h = moved_folder(&dir, "archive");
    let (before, steps) = (files(&h), h.app.session.undo.len());
    open_offline(&mut h, &dir.path("trip"));

    press_ok(&mut h, |h| h.app.relocate.is_none() && toast(h).contains("None of this folder's photos"));

    assert_eq!(files(&h), before, "nothing changed");
    assert_eq!(h.app.session.undo.len(), steps);
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { path, counts: Some(c), .. }) if path.replace('\\', "/") == dir.path("trip") && c.offline),
        "the dialog stays, to choose again: {:?}",
        h.app.ui.dialog
    );
}

#[test]
fn update_folder_location_on_a_folders_row_relinks_without_a_dialog() {
    let dir = Scratch::new("row");
    let mut h = moved_folder(&dir, "archive/journey");

    row_menu(&mut h, &dir.path("trip"), "folderLocate");
    assert!(h.step_until(T, |h| h.app.relocate.is_none() && toast(h).contains("Folder located")), "{:?}", toast(&h));

    assert_eq!(files(&h), [dir.path("archive/journey/a.png"), dir.path("archive/journey/b.png")]);
    assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
    // undo points the photos back where they were
    h.app.session.undo_step().unwrap();
    assert_eq!(files(&h), [dir.path("trip/a.png"), dir.path("trip/b.png")]);
}

#[test]
fn a_folder_that_holds_only_some_of_the_photos_asks_first() {
    let dir = Scratch::new("some");
    let mut h = moved_folder(&dir, "archive/journey");
    std::fs::remove_file(dir.path("archive/journey/b.png")).unwrap();
    let before = files(&h);

    // Cancel: the library stays as it was
    row_menu(&mut h, &dir.path("trip"), "folderLocate");
    let asked = |h: &Headless| matches!(&h.app.ui.dialog, Some(Dialog::LocateFolder { found: 1, total: 2, .. }));
    assert!(h.step_until(T, asked), "{:?} · {:?}", h.app.ui.dialog, toast(&h));
    assert_eq!(files(&h), before, "nothing changed while it asks");
    shot(&mut h, "locate-some.png");
    h.app.ui.dialog = None;
    h.step();
    h.step();
    assert!(h.app.relocate_asked.is_none(), "what was found goes with the question");
    assert_eq!(files(&h), before);

    // Locate Anyway: both photos follow, one of them missing at the new place
    row_menu(&mut h, &dir.path("trip"), "folderLocate");
    assert!(h.step_until(T, asked), "{:?}", h.app.ui.dialog);
    press_ok(&mut h, |h| h.app.ui.dialog.is_none() && toast(h).contains("Folder located"));
    assert_eq!(files(&h), [dir.path("archive/journey/a.png"), dir.path("archive/journey/b.png")]);
    let missing = h.app.session.execute("library.missing", &json!({})).unwrap();
    assert_eq!(missing.as_array().map(Vec::len), Some(1), "{missing}");
}

#[test]
fn the_files_are_looked_for_under_a_row_whose_cross_stops_it() {
    let dir = Scratch::new("activity");
    let mut h = moved_folder(&dir, "archive/journey");
    let before = files(&h);
    let request = Request { path: dir.path("trip"), to: dir.path("archive/journey"), disk: false, incomplete: false, synchronize: false, asks: true };

    // started between frames: nothing is collected until the next one
    let r = crate::relocate::start(&mut h.app, request.clone());
    assert!(r.is_ok(), "{r:?}");
    let rows = h.app.session.activity.list();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].label.as_str()), ("relocate", "Locating folder"));
    assert!(rows[0].detail.ends_with("trip"), "the folder by its name: {:?}", rows[0].detail);
    assert!(rows[0].cancellable);
    // one at a time
    assert!(crate::relocate::start(&mut h.app, request.clone()).is_err());

    // the row's ✕ (as `activity.cancel`)
    h.app.session.activity.cancel(rows[0].id).unwrap();
    assert!(h.step_until(T, |h| h.app.relocate.is_none()), "it ends");
    h.step();
    assert_eq!(files(&h), before, "stopped: nothing changed");
    let last = h.app.relocate_last.clone().unwrap_or_default();
    assert_eq!((last["cancelled"].as_bool(), last["folder"].as_str().map(|f| f.replace('\\', "/"))), (Some(true), Some(dir.path("trip"))), "{last}");
    assert!(h.app.session.activity.list().is_empty(), "the row goes with it");

    // an import running: its photos could be the folder's
    let r = crate::import::start_paths(&mut h.app, vec![dir.path("archive/journey/a.png")]);
    assert!(r.is_ok() && h.app.import.is_some(), "{r:?}");
    let r = crate::relocate::start(&mut h.app, request);
    assert!(r.is_err() && h.app.relocate.is_none(), "not while importing: {r:?}");
}

/// Given the looking is cancelled while its worker is still reading (a share that doesn't answer),
/// When frames go by,
/// Then its row and the one-at-a-time rule stay until the worker has ended, and only then is it said to be cancelled.
#[test]
fn a_cancelled_looking_keeps_its_row_until_its_worker_has_ended() {
    let dir = Scratch::new("held-worker");
    let mut h = moved_folder(&dir, "archive/journey");
    let request = Request { path: dir.path("trip"), to: dir.path("archive/journey"), disk: false, incomplete: false, synchronize: false, asks: true };
    let worker = crate::relocate::track_held_worker(&mut h.app, request.clone());
    let row = h.app.session.activity.list()[0].id;
    h.app.session.activity.cancel(row).unwrap();
    h.step();
    h.step();
    assert!(h.app.relocate.is_some(), "the worker hasn't ended");
    assert_eq!(h.app.session.activity.list().len(), 1, "its row stays");
    assert!(crate::relocate::start(&mut h.app, request).is_err(), "no second worker beside the first");
    assert!(h.app.relocate_last.is_none(), "{:?}", h.app.relocate_last);

    worker.send(Err("locating the folder was stopped: nothing was changed".to_string())).unwrap();
    assert!(h.step_until(T, |h| h.app.relocate.is_none()), "it ends with its worker");
    let last = h.app.relocate_last.clone().unwrap_or_default();
    assert_eq!(last["cancelled"].as_bool(), Some(true), "{last}");
    assert!(h.app.session.activity.list().is_empty(), "the row goes with it");
}

/// Given the lookings of libraries that were left have workers that never answer (a share that is down),
/// When a looking is started in each next library,
/// Then the app holds at most `MAX_LEFT_BEHIND` of those workers: a looking is refused beside that many, and starts again once some have ended.
#[test]
fn workers_left_behind_by_library_switches_are_bounded() {
    let dir = Scratch::new("stale-bound");
    let mut h = moved_folder(&dir, "archive/journey");
    let request = Request { path: dir.path("trip"), to: dir.path("archive/journey"), disk: false, incomplete: false, synchronize: false, asks: true };
    // (a folder no photo came from passes the bound and is refused after it, by what the library says: that tells the two refusals apart)
    let nowhere = Request { path: dir.path("nowhere"), ..request.clone() };
    let bound = crate::relocate::MAX_LEFT_BEHIND;
    let mut workers = Vec::new();
    let mut hold_one = |h: &mut Headless| {
        workers.push(crate::relocate::track_held_worker(&mut h.app, request.clone()));
        // the library is switched: the next frame lets go of the looking
        h.app.relocate_library = h.app.relocate_library.wrapping_add(1);
        h.step();
        assert!(h.app.relocate.is_none());
    };
    for _ in 1..bound {
        hold_one(&mut h);
    }
    let why = crate::relocate::start(&mut h.app, nowhere.clone()).unwrap_err();
    assert!(why.contains("no photo"), "below the bound a looking goes on to be read: {why}");
    hold_one(&mut h);
    assert_eq!(h.app.relocate_left_behind.len(), bound);
    let why = crate::relocate::start(&mut h.app, nowhere.clone()).unwrap_err();
    assert!(why.contains("already being located"), "workers that never end are not left to pile up: {why}");

    for w in &workers {
        w.send(Err("locating the folder was stopped: nothing was changed".to_string())).unwrap();
    }
    h.step();
    assert!(h.app.relocate_left_behind.is_empty());
    let why = crate::relocate::start(&mut h.app, nowhere).unwrap_err();
    assert!(why.contains("no photo"), "once they have ended a looking is read again: {why}");
}

#[test]
fn a_refusal_that_needs_no_disk_is_said_at_once() {
    let dir = Scratch::new("refused");
    let mut h = moved_folder(&dir, "archive/journey");
    // a folder the library has no photo from
    let r = h.app.run("folder.locate", json!({"folder": dir.path("nothing"), "path": dir.path("archive/journey")}));
    assert!(r.as_ref().is_err_and(|e| e.contains("no photo in the library")), "{r:?}");
    assert!(h.app.relocate.is_none() && h.app.session.activity.list().is_empty());
    let r = h.app.run("folder.locate", json!({"path": dir.path("archive/journey")}));
    assert!(r.is_err(), "{r:?}");
}

#[test]
fn the_question_waits_for_another_dialog_to_be_done_with() {
    let dir = Scratch::new("waits");
    let mut h = moved_folder(&dir, "archive/journey");
    std::fs::remove_file(dir.path("archive/journey/b.png")).unwrap();
    let request = Request { path: dir.path("trip"), to: dir.path("archive/journey"), disk: false, incomplete: false, synchronize: false, asks: true };
    assert!(crate::relocate::start(&mut h.app, request).is_ok());
    // something else was opened while the files were looked for
    h.app.ui.dialog = Some(Dialog::About);
    assert!(h.step_until(T, |h| h.app.relocate.is_none()), "the looking ends");
    h.step();
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::About)), "it is not taken away: {:?}", h.app.ui.dialog);
    assert!(h.app.relocate_asked.is_some(), "the question is kept");
    h.app.ui.dialog = None;
    assert!(
        h.step_until(T, |h| matches!(&h.app.ui.dialog, Some(Dialog::LocateFolder { found: 1, total: 2, .. }))),
        "then it is asked: {:?}",
        h.app.ui.dialog
    );
}

#[test]
fn synchronize_is_not_opened_again_once_its_dialog_was_closed() {
    let dir = Scratch::new("closed");
    let mut h = moved_folder(&dir, "archive/journey");
    // the folder dialog's answer arrives after the Synchronize dialog was cancelled
    let r = h.app.run("folder.locate", json!({"folder": dir.path("trip"), "path": dir.path("archive/journey"), "synchronize": true}));
    assert!(r.is_ok(), "{r:?}");
    assert!(h.step_until(T, |h| h.app.relocate.is_none() && toast(h).contains("Folder located")), "{:?}", toast(&h));
    h.step();
    assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
}

// ── Review of #1122 ──────────────────────────────────────────────────────────────────────────
//
// * An agent that confirms the dialog of a folder that isn't there (`ui.dialog.confirm`) gets
//   what the person's click gets: the folder is located and scanned at its new place.
// * A Synchronize dialog closed while the files were looked for is not opened again.
// * A file at the new place that is not the size the library recorded is said in the question.
// * `folder.updateLocation` from an agent, in the app, looks for the files on a worker thread
//   too; `ui.inspect` → `relocate` says how it went.

#[test]
fn an_agent_confirming_the_dialog_of_a_folder_that_isnt_there_gets_the_scan_too() {
    let dir = Scratch::new("agent");
    let mut h = moved_folder(&dir, "archive/journey");
    open_offline(&mut h, &dir.path("trip"));

    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");

    let now = dir.path("archive/journey");
    let scanned = h.step_until(
        T,
        |h| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { path, counts: Some(c), .. }) if path.replace('\\', "/") == now && !c.offline),
    );
    assert!(scanned, "the dialog scans the folder at its new place: {:?} · {:?}", h.app.ui.dialog, toast(&h));
    assert_eq!(files(&h), [dir.path("archive/journey/a.png"), dir.path("archive/journey/b.png")]);
}

#[test]
fn the_question_replaces_the_synchronize_dialog_of_a_folder_written_with_a_trailing_separator() {
    let dir = Scratch::new("trailing");
    let mut h = moved_folder(&dir, "archive/journey");
    std::fs::remove_file(dir.path("archive/journey/b.png")).unwrap();
    open_offline(&mut h, &dir.path("trip"));
    // an agent writes the folder with a trailing separator; the dialog has it without
    let request = Request {
        path: format!("{}/", dir.path("trip")),
        to: dir.path("archive/journey"),
        disk: false,
        incomplete: false,
        synchronize: true,
        asks: true,
    };
    assert!(crate::relocate::start(&mut h.app, request).is_ok());

    let asked = |h: &Headless| matches!(&h.app.ui.dialog, Some(Dialog::LocateFolder { found: 1, total: 2, .. }));
    assert!(h.step_until(T, asked), "the question is not held back by the dialog of the same folder: {:?} · {:?}", h.app.ui.dialog, toast(&h));
}

#[test]
fn a_dialog_closed_while_the_files_were_looked_for_is_not_opened_again() {
    let dir = Scratch::new("closed-looking");
    let mut h = moved_folder(&dir, "archive/journey");
    open_offline(&mut h, &dir.path("trip"));
    let request = Request { path: dir.path("trip"), to: dir.path("archive/journey"), disk: false, incomplete: false, synchronize: true, asks: true };
    assert!(crate::relocate::start(&mut h.app, request).is_ok());
    // Cancel, before the files were all looked for
    h.app.ui.dialog = None;

    assert!(h.step_until(T, |h| h.app.relocate.is_none() && toast(h).contains("Folder located")), "{:?}", toast(&h));
    h.step();
    assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
}

#[test]
fn a_file_that_is_not_the_size_the_library_recorded_is_said_in_the_question() {
    let dir = Scratch::new("other-size");
    let mut h = moved_folder(&dir, "archive/journey");
    // another picture under the same name
    let img = lightcraft_raster::Rgba8 {
        width: 40,
        height: 30,
        data: (0..1200usize).map(|i| [(i * 7) as u8, (i * 3) as u8, (i / 5) as u8, 255]).collect(),
    };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(dir.path("archive/journey/b.png"), bytes).unwrap();

    row_menu(&mut h, &dir.path("trip"), "folderLocate");
    let asked = |h: &Headless| matches!(&h.app.ui.dialog, Some(Dialog::LocateFolder { found: 1, total: 2, different: 1, .. }));
    assert!(h.step_until(T, asked), "{:?} · {:?}", h.app.ui.dialog, toast(&h));
    h.step();
    let text = h.painted_text();
    assert!(text.iter().any(|t| t.contains("photos whose file there is another size are linked to that file")), "{text:?}");
    assert!(!text.iter().any(|t| t.contains("The other photos stay missing there")), "{text:?}");
    shot(&mut h, "locate-different.png");
}

/// `folder.updateLocation` as an agent calls it in the app, and what `ui.inspect` says of it.
fn update_location(h: &mut Headless, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": "folder.updateLocation", "params": params}), T);
    assert_eq!(r["ok"], true, "{r}");
    // (the way of the person's: a worker thread under a row that can be stopped, see
    // `the_files_are_looked_for_under_a_row_whose_cross_stops_it`)
    assert_eq!(r["result"], json!({"background": true}), "the reply does not wait for the disk: {r}");
    assert!(h.step_until(T, |h| h.app.relocate.is_none()), "it ends");
    h.step();
    let r = h.request("ui.inspect", json!({}), T);
    assert_eq!(r["result"]["relocate"]["running"], json!(null), "{}", r["result"]["relocate"]);
    r["result"]["relocate"]["last"].clone()
}

#[test]
fn an_agents_update_folder_location_never_looks_on_the_apps_thread() {
    let dir = Scratch::new("agent-update");
    let mut h = moved_folder(&dir, "archive/journey");
    std::fs::remove_file(dir.path("archive/journey/b.png")).unwrap();
    let before = files(&h);
    let params = json!({"folder": dir.path("trip"), "path": dir.path("archive/journey")});

    // only some of the files: an agent is told why not, as by the command anywhere else, and
    // the person is not asked about a step that isn't theirs
    let last = update_location(&mut h, params.clone());
    assert!(last["error"].as_str().is_some_and(|e| e.contains("only 1 of the 2 files") && e.contains("incomplete: true")), "{last}");
    assert!(h.app.ui.dialog.is_none() && h.app.relocate_asked.is_none(), "{:?}", h.app.ui.dialog);
    assert_eq!(files(&h), before);

    // none of them
    let last = update_location(&mut h, json!({"folder": dir.path("trip"), "path": dir.path("archive")}));
    assert!(last["error"].as_str().is_some_and(|e| e.contains("none of the 2 files")), "{last}");
    assert_eq!(files(&h), before);

    // with `incomplete`: the command's result
    let mut anyway = params;
    anyway["incomplete"] = json!(true);
    let last = update_location(&mut h, anyway);
    assert_eq!((last["relinked"].as_u64(), last["found"].as_u64(), last["notFound"].as_u64()), (Some(2), Some(1), Some(1)), "{last}");
    assert_eq!(files(&h), [dir.path("archive/journey/a.png"), dir.path("archive/journey/b.png")]);

    // what needs no disk to refuse is refused in the reply
    let r = h.request(
        "engine.execute",
        json!({"command": "folder.updateLocation", "params": {"folder": dir.path("nothing"), "path": dir.path("archive")}}),
        T,
    );
    assert!(r["ok"] == false && r["error"].as_str().is_some_and(|e| e.contains("no photo in the library")), "{r}");
    let r = h.request("engine.execute", json!({"command": "folder.updateLocation", "params": {"path": dir.path("archive")}}), T);
    assert!(r["ok"] == false && r["error"].as_str().is_some_and(|e| e.contains("missing `folder`")), "{r}");
}

// ── Second review: a person and an agent at once ─────────────────────────────────────────────

fn inspect_relocate(h: &mut Headless) -> serde_json::Value {
    h.request("ui.inspect", json!({}), T)["result"]["relocate"].clone()
}

#[test]
fn locate_anyway_from_the_dialog_of_a_folder_that_isnt_there_scans_it_at_its_new_place() {
    let dir = Scratch::new("anyway-sync");
    let mut h = moved_folder(&dir, "archive/journey");
    std::fs::remove_file(dir.path("archive/journey/b.png")).unwrap();
    open_offline(&mut h, &dir.path("trip"));

    // Locate Missing Folder…: only some are there, so the question takes the dialog's place
    press_ok(&mut h, |h| matches!(&h.app.ui.dialog, Some(Dialog::LocateFolder { found: 1, total: 2, synchronize: true, .. })));
    // Locate Anyway: the folder is scanned where it is now
    let now = dir.path("archive/journey");
    press_ok(
        &mut h,
        |h| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { path, counts: Some(c), .. }) if path.replace('\\', "/") == now && !c.offline),
    );
    assert_eq!(files(&h), [dir.path("archive/journey/a.png"), dir.path("archive/journey/b.png")]);
}

#[test]
fn a_question_that_waits_is_not_overtaken_and_its_end_is_said() {
    let dir = Scratch::new("question-waits");
    let mut h = moved_folder(&dir, "archive/journey");
    std::fs::remove_file(dir.path("archive/journey/b.png")).unwrap();
    let before = files(&h);
    row_menu(&mut h, &dir.path("trip"), "folderLocate");
    assert!(h.step_until(T, |h| matches!(&h.app.ui.dialog, Some(Dialog::LocateFolder { .. }))), "{:?}", h.app.ui.dialog);
    assert_eq!(inspect_relocate(&mut h)["last"], json!(null), "it waits for the answer");

    // an agent's step meanwhile would make the answer be about a library that is no longer
    let params = json!({"folder": dir.path("trip"), "path": dir.path("archive/journey"), "incomplete": true});
    let r = h.request("engine.execute", json!({"command": "folder.updateLocation", "params": params}), T);
    assert!(r["ok"] == false && h.app.relocate.is_none(), "{r}");
    assert!(r["error"].as_str().is_some_and(|e| e.contains("question")), "why: {r}");
    let asking = inspect_relocate(&mut h)["asking"].clone();
    assert_eq!(
        (asking["folder"].as_str().map(|f| f.replace('\\', "/")), asking["shown"].as_bool()),
        (Some(dir.path("trip")), Some(true)),
        "{asking}"
    );
    assert_eq!(files(&h), before);

    // Cancel: that is how it ended, and for which folder
    let r = h.request("ui.dialog.cancel", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let last = inspect_relocate(&mut h)["last"].clone();
    assert_eq!(last["cancelled"], json!(true), "{last}");
    assert_eq!(last["folder"].as_str().map(|f| f.replace('\\', "/")), Some(dir.path("trip")), "{last}");
    assert!(h.app.relocate_asked.is_none());
}

#[test]
fn every_end_names_its_folder() {
    let dir = Scratch::new("names");
    let mut h = moved_folder(&dir, "archive/journey");
    let last = update_location(&mut h, json!({"folder": dir.path("trip"), "path": dir.path("archive")}));
    assert!(last["error"].is_string(), "{last}");
    assert_eq!(
        (last["folder"].as_str().map(|f| f.replace('\\', "/")), last["path"].as_str().map(|f| f.replace('\\', "/"))),
        (Some(dir.path("trip")), Some(dir.path("archive"))),
        "{last}"
    );
}

#[test]
fn an_agents_step_brings_the_open_dialog_of_that_folder_up_to_date() {
    let dir = Scratch::new("agent-stale");
    let mut h = moved_folder(&dir, "archive/journey");
    open_offline(&mut h, &dir.path("trip"));

    // (the folder written another way than the dialog has it)
    let last = update_location(&mut h, json!({"folder": format!("{}/", dir.path("trip")), "path": dir.path("archive/journey")}));
    assert_eq!(last["relinked"], json!(2), "{last}");

    // the dialog was about a folder the library no longer has: it is about the folder where it is now
    let now = dir.path("archive/journey");
    let scanned = h.step_until(
        T,
        |h| matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { path, counts: Some(c), .. }) if path.replace('\\', "/") == now && !c.offline),
    );
    assert!(scanned, "{:?}", h.app.ui.dialog);
}

#[test]
fn an_agents_confirm_that_is_refused_leaves_the_dialog_to_choose_again() {
    let dir = Scratch::new("agent-refused");
    let mut h = moved_folder(&dir, "archive/journey");
    open_offline(&mut h, &dir.path("trip"));
    h.app.services.pick_folder = None;

    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::SynchronizeFolder { counts: Some(c), .. }) if c.offline),
        "as after its button: {:?}",
        h.app.ui.dialog
    );
}

/// Library B open at `dir/other`, holding the same folder `trip` (files at the same place) as
/// the library the test started in: whatever is replayed on B finds its photos there.
/// `meanwhile` runs after the switch and before the next frame.
fn switch_to_a_library_with_the_same_folder(h: &mut Headless, dir: &Scratch, meanwhile: impl FnOnce()) {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    std::fs::create_dir_all(dir.0.join("other")).unwrap();
    h.app.run("app.openLibrary", json!({"path": dir.path("other")})).unwrap();
    meanwhile();
    h.app.session.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    h.settle(SETTLE);
    assert_eq!(files(h), [dir.path("trip/a.png"), dir.path("trip/b.png")]);
}

/// The Locate Missing Folder dialog is up when the library is switched; its answer is for the
/// library it was asked in (review of #1122: it would have relinked library B's photos).
#[test]
fn a_folder_dialog_answered_after_the_library_changed_relocates_nothing() {
    use std::sync::{Arc, Mutex, mpsc::Sender};
    let dir = Scratch::new("pick-stale");
    let mut h = moved_folder(&dir, "archive/journey");
    let senders: Arc<Mutex<Vec<Sender<Vec<String>>>>> = Default::default();
    let s = senders.clone();
    h.app.services.picker = Some(Box::new(move |_| {
        let (tx, rx) = std::sync::mpsc::channel();
        s.lock().unwrap().push(tx);
        Ok(rx)
    }));
    h.app.run("folder.locate", json!({"folder": dir.path("trip")})).unwrap();
    assert_eq!(h.app.pending_picks.len(), 1);

    // the answer arrives once the library is switched, before a frame took notice
    let answer = dir.path("archive/journey");
    switch_to_a_library_with_the_same_folder(&mut h, &dir, || senders.lock().unwrap()[0].send(vec![answer]).unwrap());
    h.settle(SETTLE);
    h.step();
    assert!(h.app.pending_picks.is_empty(), "the stale answer is let go of");
    assert!(!crate::pick::waiting(&h.app, "folder.locate"));
    assert!(h.app.relocate.is_none() && h.app.relocate_asked.is_none());
    assert_eq!(files(&h), [dir.path("trip/a.png"), dir.path("trip/b.png")], "library B's photos stayed");
    assert!(h.app.relocate_last.is_none(), "nothing ran on B: {:?}", h.app.relocate_last);
}

/// Files being looked for, or a question about them, belong to the library they were started in.
#[test]
fn a_relocation_started_in_another_library_is_dropped_when_the_library_changes() {
    let dir = Scratch::new("relocate-stale");
    let mut h = moved_folder(&dir, "archive/journey");
    std::fs::remove_file(dir.path("archive/journey/b.png")).unwrap();
    let request = Request { path: dir.path("trip"), to: dir.path("archive/journey"), disk: false, incomplete: false, synchronize: false, asks: true };

    // the question is up (some of the files are there) …
    assert!(crate::relocate::start(&mut h.app, request).is_ok());
    assert!(h.step_until(T, |h| matches!(&h.app.ui.dialog, Some(Dialog::LocateFolder { .. }))), "{:?}", h.app.ui.dialog);
    assert!(h.app.relocate_asked.is_some());

    // … and the library is switched: the question goes, and "Locate Anyway" has nothing to act on
    switch_to_a_library_with_the_same_folder(&mut h, &dir, || ());
    h.step();
    h.step();
    assert!(h.app.relocate_asked.is_none() && h.app.relocate.is_none());
    assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
    assert_eq!(files(&h), [dir.path("trip/a.png"), dir.path("trip/b.png")]);
    assert_eq!(h.app.relocate_last.as_ref().and_then(|l| l["cancelled"].as_bool()), Some(true), "{:?}", h.app.relocate_last);
}

/// F2 of the review of #1122: cancelling the synchronous dialog is not "no dialog here".
#[test]
fn a_cancelled_folder_dialog_is_not_an_error_but_a_host_without_one_is() {
    let mut cancelled =
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { pick_folder: Some(Box::new(|| None)), ..Default::default() });
    assert_eq!(cancelled.run("folder.locate", json!({"folder": "/somewhere"})).unwrap(), serde_json::Value::Null);
    let mut bare = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let e = bare.run("folder.locate", json!({"folder": "/somewhere"})).unwrap_err();
    assert!(e.contains("no folder dialog"), "{e}");
}

/// A library on disk at `dir/other` that holds the folder `trip` (its files at the same place as
/// in the library `moved_folder` made), ready to be opened.
fn prepared_other_library(dir: &Scratch) -> String {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("trip/b.png"), 2);
    let mut other = lightcraft_engine::Session::new().with_fs();
    other.open_library(dir.0.join("other"), true).unwrap();
    other.execute("library.import", &json!({"paths": [dir.path("trip")]})).unwrap();
    other.close_library().unwrap();
    dir.path("other")
}

/// Two answers close in one frame, the first of them opening another library: the second was
/// asked in the library left, and does not run in the new one.
#[test]
fn an_answer_after_one_that_opened_another_library_does_not_run_there() {
    use std::sync::{Arc, Mutex, mpsc::Sender};
    let dir = Scratch::new("pick-batch");
    let mut h = moved_folder(&dir, "archive/journey");
    let other = prepared_other_library(&dir);
    let senders: Arc<Mutex<Vec<Sender<Vec<String>>>>> = Default::default();
    let s = senders.clone();
    h.app.services.picker = Some(Box::new(move |_| {
        let (tx, rx) = std::sync::mpsc::channel();
        s.lock().unwrap().push(tx);
        Ok(rx)
    }));
    h.app.run("app.openLibrary", json!({})).unwrap();
    h.app.run("folder.locate", json!({"folder": dir.path("trip")})).unwrap();
    assert_eq!(h.app.pending_picks.len(), 2);
    senders.lock().unwrap()[0].send(vec![other]).unwrap();
    senders.lock().unwrap()[1].send(vec![dir.path("archive/journey")]).unwrap();
    h.settle(SETTLE);
    h.step();
    assert_eq!(files(&h), [dir.path("trip/a.png"), dir.path("trip/b.png")], "library B is open");
    assert!(h.app.relocate.is_none() && h.app.relocate_asked.is_none());
    assert!(h.app.relocate_last.is_none(), "nothing ran on B: {:?}", h.app.relocate_last);
}

/// A step of the library left that no frame has dropped yet does not refuse the next one: an
/// agent's `app.openLibrary` then `folder.locate` can arrive in one drain of the control channel.
#[test]
fn a_relocation_of_the_library_left_does_not_refuse_the_first_one_of_the_next() {
    let dir = Scratch::new("relocate-start-stale");
    let mut h = moved_folder(&dir, "archive/journey");
    let request = Request { path: dir.path("trip"), to: dir.path("archive/journey"), disk: false, incomplete: false, synchronize: false, asks: true };
    assert!(crate::relocate::start(&mut h.app, request.clone()).is_ok());
    let other = prepared_other_library(&dir);
    h.app.run("app.openLibrary", json!({"path": other})).unwrap();
    // no frame between
    let r = crate::relocate::start(&mut h.app, request);
    assert!(r.is_ok(), "{r:?}");
}
