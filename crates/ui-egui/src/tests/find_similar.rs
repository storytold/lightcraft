//! Find Similar Photos reads every photo of the library the first time, which left the window not
//! responding for as long as that took (issue #3468): the photos are read on a worker thread under a
//! row of the activity stack.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_raster::Rgb32f;
use serde_json::{Value, json};

use crate::find_similar::LABEL;
use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(60);
const PHOTOS: u64 = 12;
/// How long a read waits for a drive that isn't opened, and how long the frames may take while it does.
const GUARD: Duration = Duration::from_secs(20);
const FRAMES: Duration = Duration::from_secs(15);

/// The photos' files: a read waits for `open` (a slow drive), at most `guard`, and is counted, as
/// are the reads made on the UI thread.
struct Drive {
    open: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
    on_ui: Arc<AtomicUsize>,
}

fn app_on_a_slow_drive(guard: Duration) -> (Headless, Drive) {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
    for i in 0..PHOTOS {
        let path = format!("/drive/IMG_{i:04}.arw");
        let p = Photo::new(PhotoId(i + 1), Source::File { path }, &format!("IMG_{i:04}.arw"), "ARW", 64, 64, "2026-01-01T00:00:00");
        app.session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let drive = Drive { open: Arc::new(AtomicBool::new(false)), reads: Arc::new(AtomicUsize::new(0)), on_ui: Arc::new(AtomicUsize::new(0)) };
    let (ui, open, reads, on_ui) = (std::thread::current().id(), drive.open.clone(), drive.reads.clone(), drive.on_ui.clone());
    let deadline = Instant::now() + guard;
    app.session.media.file_loader = Some(Arc::new(move |path: &str, _| {
        // (the grid reads the photos for its thumbnails too, on threads of its own)
        let thread = std::thread::current();
        if thread.name().is_some_and(|n| n.starts_with("similar-") || n.contains(LABEL)) {
            reads.fetch_add(1, Ordering::SeqCst);
        }
        if thread.id() == ui {
            on_ui.fetch_add(1, Ordering::SeqCst);
        }
        // (a hang guard: the read goes on after it, so a regression fails the test instead of hanging it)
        while !open.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        // even photos get brighter to the right, odd ones downwards
        let across = path.bytes().rfind(u8::is_ascii_digit).is_some_and(|d| d % 2 == 0);
        let image = Rgb32f::from_fn(64, 64, |x, y| {
            let v = if across { x } else { y };
            [v as f32 / 64.0; 3]
        });
        Ok((image, Default::default()))
    }));
    (Headless::new(app, [1200.0, 800.0], 1.0), drive)
}

fn rows(h: &Headless) -> Vec<lightcraft_engine::activity::TaskInfo> {
    h.app.session.activity.list().into_iter().filter(|r| r.kind == "similar").collect()
}

/// The row's wait is over once the task has gone.
fn until_done(h: &mut Headless) {
    assert!(h.step_until(T, |h| h.app.tasks.is_empty()), "the search never ended");
}

fn ask(h: &mut Headless) -> Receiver<Value> {
    h.send("engine.execute", json!({"command": "library.findSimilar", "params": {"id": 1}})).unwrap()
}

fn answer(h: &mut Headless, rx: &Receiver<Value>) -> Value {
    let t0 = Instant::now();
    loop {
        h.step();
        if let Ok(v) = rx.try_recv() {
            return v;
        }
        assert!(t0.elapsed() < T, "never answered");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The issue's report: from the menu, the command answers at once while the files are read, the
/// window takes its frames, the row counts and can stop, and the grid shows what was found.
#[test]
fn a_search_from_the_menu_reads_off_the_ui_thread_and_the_window_goes_on() {
    let (mut h, drive) = app_on_a_slow_drive(GUARD);
    h.app.ui.view = ViewMode::Detail;
    let t0 = Instant::now();
    let r = h.app.run("library.findSimilar", json!({"id": 1})).unwrap();
    assert_eq!(r, json!({"background": true, "count": PHOTOS}));
    for _ in 0..3 {
        h.step();
    }
    // nothing has been read yet: the command and the frames waited for none of it
    assert!(t0.elapsed() < FRAMES, "the window waited for the files: {:?}", t0.elapsed());
    assert_eq!(h.app.session.filter, Default::default(), "no answer before the files are read");
    let row = &rows(&h)[0];
    assert_eq!(row.label, LABEL);
    assert_eq!(row.total, PHOTOS);
    assert!(row.cancellable, "the row can stop the search");

    drive.open.store(true, Ordering::SeqCst);
    until_done(&mut h);
    assert_eq!(drive.on_ui.load(Ordering::SeqCst), 0, "a file was read on the UI thread");
    assert_eq!(drive.reads.load(Ordering::SeqCst), PHOTOS as usize);
    assert!(rows(&h).is_empty(), "the row goes with the search");
    let shown = h.app.session.filter.only.len();
    assert_eq!(shown, PHOTOS as usize / 2, "the photos alike are shown");
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.starts_with(&format!("{shown} similar photos")), "{toast}");
}

/// One search at a time: the menu row is off while one runs and on again after it, and a second
/// request is refused without touching the first.
#[test]
fn a_second_search_waits_for_the_first() {
    // what the menu bar draws the row from
    let row_is_on = |h: &Headless| crate::menus::menu_entries(&h.app).into_iter().find(|e| e.id == "library.findSimilar").is_some_and(|e| e.enabled);
    let (mut h, drive) = app_on_a_slow_drive(GUARD);
    assert!(row_is_on(&h));
    h.app.run("library.findSimilar", json!({"id": 1})).unwrap();
    assert!(!row_is_on(&h), "the row is off while one runs");
    let e = h.app.run("library.findSimilar", json!({"id": 2})).unwrap_err();
    assert!(e.contains("already running"), "{e}");
    assert_eq!(rows(&h).len(), 1);
    drive.open.store(true, Ordering::SeqCst);
    until_done(&mut h);
    assert!(row_is_on(&h));
    assert!(h.app.session.filter.only.contains(&PhotoId(1)), "the first search answered");
}

/// ✕ on the row: the view stays as it was, nothing is said, and the next search doesn't read again
/// what was read.
#[test]
fn stopping_leaves_the_view_alone_and_the_next_search_reads_only_the_rest() {
    let (mut h, drive) = app_on_a_slow_drive(GUARD);
    h.app.ui.view = ViewMode::Detail;
    h.app.run("library.findSimilar", json!({"id": 1})).unwrap();
    // stopped while the searched photo is being read: that one read is all it does
    assert!(h.step_until(T, |_| drive.reads.load(Ordering::SeqCst) == 1), "the first read never started");
    h.app.session.activity.cancel(rows(&h)[0].id).unwrap();
    drive.open.store(true, Ordering::SeqCst);
    until_done(&mut h);
    assert!(rows(&h).is_empty());
    assert_eq!(h.app.session.filter, Default::default());
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    assert!(h.app.ui.toast.is_none(), "{:?}", h.app.ui.toast);
    assert_eq!(drive.reads.load(Ordering::SeqCst), 1);

    let r = h.app.run("library.findSimilar", json!({"id": 1})).unwrap();
    assert_eq!(r["count"], PHOTOS - 1, "what was read isn't read again");
    until_done(&mut h);
    assert_eq!(drive.reads.load(Ordering::SeqCst), PHOTOS as usize);
    assert_eq!(h.app.session.filter.only.len(), PHOTOS as usize / 2);
}

/// Nothing to read: the answer comes on the next frames, with no photo read.
#[test]
fn a_search_of_what_is_known_reads_nothing() {
    let (mut h, drive) = app_on_a_slow_drive(Duration::ZERO);
    drive.open.store(true, Ordering::SeqCst);
    h.app.run("library.findSimilar", json!({"id": 1})).unwrap();
    until_done(&mut h);
    let reads = drive.reads.load(Ordering::SeqCst);
    let r = h.app.run("library.findSimilar", json!({"id": 2})).unwrap();
    assert_eq!(r["count"], 0);
    until_done(&mut h);
    assert_eq!(drive.reads.load(Ordering::SeqCst), reads);
    assert!(h.app.session.filter.only.contains(&PhotoId(2)));
}

/// A refusal comes at once and starts nothing (no photo active, a photo the library doesn't have).
#[test]
fn a_search_that_cannot_start_leaves_no_row() {
    let (mut h, drive) = app_on_a_slow_drive(Duration::ZERO);
    assert!(h.app.run("library.findSimilar", json!({"id": 999})).is_err());
    assert!(h.app.run("library.findSimilar", json!({"id": "x"})).is_err());
    assert!(h.app.tasks.is_empty());
    assert!(rows(&h).is_empty());
    assert_eq!(drive.reads.load(Ordering::SeqCst), 0);
}

/// A searched photo that can't be read says so where the person is.
#[test]
fn a_photo_that_cannot_be_read_says_so() {
    let (mut h, _) = app_on_a_slow_drive(Duration::ZERO);
    h.app.session.media.file_loader = Some(Arc::new(|path: &str, _| Err(format!("{path}: gone"))));
    h.app.run("library.findSimilar", json!({"id": 1})).unwrap();
    until_done(&mut h);
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("can't read the photo"), "{toast}");
    assert_eq!(h.app.session.filter, Default::default());
}

/// A control request is answered when the search ends, with the command's `{photos}`, and the window
/// takes its frames meanwhile. An agent's view isn't switched for it.
#[test]
fn a_control_request_is_answered_when_the_search_ends() {
    let (mut h, drive) = app_on_a_slow_drive(GUARD);
    h.app.ui.view = ViewMode::Detail;
    let t0 = Instant::now();
    let rx = ask(&mut h);
    for _ in 0..3 {
        h.step();
    }
    assert!(t0.elapsed() < FRAMES, "the frames waited for the files: {:?}", t0.elapsed());
    assert!(rx.try_recv().is_err(), "no answer before the files are read");
    assert_eq!(rows(&h).len(), 1);
    drive.open.store(true, Ordering::SeqCst);
    let reply = answer(&mut h, &rx);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"]["photos"][0]["id"], 1, "{reply}");
    assert_eq!(reply["result"]["photos"].as_array().unwrap().len(), PHOTOS as usize / 2, "{reply}");
    assert_eq!(drive.on_ui.load(Ordering::SeqCst), 0);
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    assert!(h.app.ui.toast.is_none(), "{:?}", h.app.ui.toast);
}

/// `activity.cancel` stops the search; the request is told so.
#[test]
fn stopping_answers_the_request_with_an_error() {
    let (mut h, drive) = app_on_a_slow_drive(GUARD);
    let rx = ask(&mut h);
    for _ in 0..3 {
        h.step();
    }
    let id = rows(&h)[0].id;
    let r = h.request("engine.execute", json!({"command": "activity.cancel", "params": {"id": id}}), T);
    assert_eq!(r["ok"], true, "{r}");
    drive.open.store(true, Ordering::SeqCst);
    let reply = answer(&mut h, &rx);
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().is_some_and(|e| e.contains("stopped")), "{reply}");
    assert_eq!(h.app.session.filter, Default::default());
}

/// A request that comes while a search runs is refused at once, not left to wait.
#[test]
fn a_second_request_is_refused_at_once() {
    let (mut h, drive) = app_on_a_slow_drive(GUARD);
    let first = ask(&mut h);
    for _ in 0..3 {
        h.step();
    }
    let second = ask(&mut h);
    let refused = answer(&mut h, &second);
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(refused["error"].as_str().is_some_and(|e| e.contains("already running")), "{refused}");
    drive.open.store(true, Ordering::SeqCst);
    assert_eq!(answer(&mut h, &first)["ok"], true);
}

/// A caller whose request timed out asks again: what was read stays known, so it isn't read again.
#[test]
fn a_request_nobody_waits_for_any_more_leaves_what_it_read() {
    let (mut h, drive) = app_on_a_slow_drive(Duration::ZERO);
    drive.open.store(true, Ordering::SeqCst);
    drop(ask(&mut h));
    // (a frame takes the request and starts the search)
    h.step();
    until_done(&mut h);
    let reads = drive.reads.load(Ordering::SeqCst);
    assert_eq!(reads, PHOTOS as usize);
    let rx = ask(&mut h);
    assert_eq!(answer(&mut h, &rx)["ok"], true);
    assert_eq!(drive.reads.load(Ordering::SeqCst), reads);
}

/// Another library was opened: the search stops, and neither its answer nor what it read reaches the
/// new library.
#[test]
fn another_library_stops_the_search_and_gets_nothing_from_it() {
    let (mut h, drive) = app_on_a_slow_drive(GUARD);
    let rx = ask(&mut h);
    for _ in 0..3 {
        h.step();
    }
    let dir = std::env::temp_dir().join(format!("lc-ui-similar-other-{}", std::process::id()));
    h.app.session.open_library(&dir, false).unwrap();
    h.step();
    drive.open.store(true, Ordering::SeqCst);
    let reply = answer(&mut h, &rx);
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().is_some_and(|e| e.contains("another library")), "{reply}");
    let read = drive.reads.load(Ordering::SeqCst);
    assert!(read < PHOTOS as usize, "it stopped reading: {read}");
    assert!(h.app.session.signatures.is_empty());
    assert_eq!(h.app.session.filter, Default::default());
    let _ = std::fs::remove_dir_all(dir);
}
