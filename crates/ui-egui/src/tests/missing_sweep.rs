//! `library.missing` from the control channel (an agent, `mcp --connect`) looks at every photo's
//! file, which on a share with hundreds of thousands of photos took minutes on the UI thread and
//! froze the window: the files are checked on a worker thread under a row of the activity stack.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(60);
const PHOTOS: u64 = 40;

/// Photos `IMG_0000` … on a share; the first ten are gone. The existence check waits for `open`
/// (a share that answers slowly), at most `guard`, and counts its calls, and those made on the UI thread.
struct Share {
    open: Arc<AtomicBool>,
    on_ui: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}

fn app_on_a_slow_share(guard: Duration) -> (Headless, Share) {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
    for i in 0..PHOTOS {
        let path = format!("/share/IMG_{i:04}.jpg");
        let p = Photo::new(PhotoId(i + 1), Source::File { path }, &format!("IMG_{i:04}.jpg"), "JPEG", 60, 40, "2026-01-01T00:00:00");
        app.session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let share = Share { open: Arc::new(AtomicBool::new(false)), on_ui: Arc::new(AtomicUsize::new(0)), calls: Arc::new(AtomicUsize::new(0)) };
    let (ui, open, on_ui, calls) = (std::thread::current().id(), share.open.clone(), share.on_ui.clone(), share.calls.clone());
    let deadline = Instant::now() + guard;
    app.session.media.availability.set_probe(Arc::new(move |f| {
        calls.fetch_add(1, Ordering::SeqCst);
        if std::thread::current().id() == ui {
            on_ui.fetch_add(1, Ordering::SeqCst);
        }
        // (a hang guard: the check goes on after it, so a regression fails the test instead of hanging it)
        while !open.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        !f.contains("_000")
    }));
    (Headless::new(app, [1200.0, 800.0], 1.0), share)
}

fn ask(h: &mut Headless) -> Receiver<Value> {
    h.send("engine.execute", json!({"command": "library.missing"})).unwrap()
}

/// Frames until `rx` has an answer.
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

fn missing_rows(h: &Headless) -> Vec<lightcraft_engine::activity::TaskInfo> {
    h.app.session.activity.list().into_iter().filter(|r| r.kind == "missing").collect()
}

#[test]
fn library_missing_is_checked_off_the_ui_thread_and_answered_when_done() {
    let (mut h, share) = app_on_a_slow_share(Duration::from_secs(3));
    let t0 = Instant::now();
    let rx = ask(&mut h);
    for _ in 0..3 {
        h.step();
    }
    // the share is still answering nothing: the window took its frames all the same
    assert!(t0.elapsed() < Duration::from_secs(2), "the frames waited for the files: {:?}", t0.elapsed());
    assert!(rx.try_recv().is_err(), "no answer before the files are checked");
    let rows = missing_rows(&h);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].cancellable, "the row can stop the check");
    assert_eq!(rows[0].label, crate::missing_sweep::LABEL);

    share.open.store(true, Ordering::SeqCst);
    let reply = answer(&mut h, &rx);
    assert_eq!(share.on_ui.load(Ordering::SeqCst), 0, "a file was looked at on the UI thread");
    assert_eq!(reply["ok"], true, "{reply}");
    let ids: Vec<u64> = reply["result"].as_array().unwrap().iter().map(|r| r["id"].as_u64().unwrap()).collect();
    assert_eq!(ids, (1..=10).collect::<Vec<_>>(), "the same [{{id, path}}] the command gives");
    assert_eq!(reply["result"][0]["path"], "/share/IMG_0000.jpg");
    assert!(h.step_until(T, |h| h.app.tasks.is_empty()));
    assert!(missing_rows(&h).is_empty(), "the row goes with the check");
}

/// A request that comes while a check runs gets that check's answer: the share isn't walked twice.
#[test]
fn a_second_request_joins_the_check_that_is_running() {
    let (mut h, share) = app_on_a_slow_share(Duration::from_secs(3));
    let (first, second) = (ask(&mut h), ask(&mut h));
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(missing_rows(&h).len(), 1, "one check for both");
    share.open.store(true, Ordering::SeqCst);
    let (a, b) = (answer(&mut h, &first), answer(&mut h, &second));
    assert_eq!(a["ok"], true, "{a}");
    assert_eq!(a, b);
    assert_eq!(share.on_ui.load(Ordering::SeqCst), 0);
}

/// ✕ on the row (or `activity.cancel`) stops the check; the request is told so.
#[test]
fn stopping_the_check_answers_the_request_with_an_error() {
    let (mut h, share) = app_on_a_slow_share(Duration::from_secs(3));
    let rx = ask(&mut h);
    for _ in 0..3 {
        h.step();
    }
    let id = missing_rows(&h)[0].id;
    let r = h.request("engine.execute", json!({"command": "activity.cancel", "params": {"id": id}}), T);
    assert_eq!(r["ok"], true, "{r}");
    share.open.store(true, Ordering::SeqCst);
    let reply = answer(&mut h, &rx);
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().is_some_and(|e| e.contains("stopped")), "{reply}");
    assert!(h.step_until(T, |h| h.app.tasks.is_empty()));
    assert!(missing_rows(&h).is_empty());
}

/// The control server gives a request 60 s, a check on a big share takes longer: the caller asks
/// again after its timeout and is given the answer the check ended with, not another walk.
#[test]
fn an_answer_nobody_waited_for_is_given_to_the_next_request() {
    let (mut h, share) = app_on_a_slow_share(Duration::from_secs(3));
    let gave_up = ask(&mut h);
    for _ in 0..3 {
        h.step();
    }
    drop(gave_up);
    share.open.store(true, Ordering::SeqCst);
    assert!(h.step_until(T, |h| h.app.tasks.is_empty()), "the check ends without a caller");
    assert_eq!(share.calls.load(Ordering::SeqCst), PHOTOS as usize);

    let again = ask(&mut h);
    let reply = answer(&mut h, &again);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"].as_array().map(Vec::len), Some(10), "{reply}");
    assert_eq!(share.calls.load(Ordering::SeqCst), PHOTOS as usize, "no second walk of the share");
    // given once: the next request checks again
    let third = ask(&mut h);
    assert_eq!(answer(&mut h, &third)["ok"], true);
    assert_eq!(share.calls.load(Ordering::SeqCst), 2 * PHOTOS as usize);
}

/// A check that panics is a failure told to the caller, not a request left waiting for its timeout.
#[test]
fn a_check_that_panics_is_answered_with_an_error() {
    let (mut h, _share) = app_on_a_slow_share(Duration::from_secs(3));
    h.app.session.media.availability.set_probe(Arc::new(|_| panic!("a file system that panics")));
    let rx = ask(&mut h);
    let reply = answer(&mut h, &rx);
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(reply["error"].as_str().is_some_and(|e| e.contains("failed unexpectedly")), "{reply}");
    assert!(h.step_until(T, |h| h.app.tasks.is_empty()));
    // and the next request is checked afresh
    h.app.session.media.availability.set_probe(Arc::new(|_| true));
    let rx = ask(&mut h);
    assert_eq!(answer(&mut h, &rx)["result"], json!([]));
}

fn new_photo(id: u64, path: &str) -> Box<Photo> {
    Box::new(Photo::new(PhotoId(id), Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T00:00:00"))
}

/// Photos change while the files are being checked (auto-import adds some every few seconds, a share check
/// takes minutes): the answer is the check's, kept to the photos that are still where it looked, so asking
/// again never has to be the answer. A photo that was missing and has been removed, moved to Recently
/// Deleted or relinked since is not reported; one added since was not checked and is not either.
#[test]
fn a_check_over_changing_photos_answers_what_still_holds() {
    let (mut h, share) = app_on_a_slow_share(Duration::from_secs(3));
    let rx = ask(&mut h);
    for _ in 0..3 {
        h.step();
    }
    let cat = &mut h.app.session.catalog;
    cat.apply(Op::RemovePhoto { id: PhotoId(1) }).unwrap();
    cat.apply(Op::SetDeleted { id: PhotoId(2), deleted: true }).unwrap();
    let relinked =
        Op::Relink { id: PhotoId(3), file_name: "y.jpg".into(), source: Source::File { path: "/share/IMG_9998.jpg".into() }, format: None };
    cat.apply(relinked).unwrap();
    cat.apply(Op::AddPhoto { photo: new_photo(PHOTOS + 1, "/share/IMG_0000x.jpg") }).unwrap();
    share.open.store(true, Ordering::SeqCst);
    let reply = answer(&mut h, &rx);
    assert_eq!(reply["ok"], true, "{reply}");
    let ids: Vec<u64> = reply["result"].as_array().unwrap().iter().map(|r| r["id"].as_u64().unwrap()).collect();
    assert_eq!(ids, (4..=10).collect::<Vec<_>>(), "{reply}");
}

/// A stop is a stop, whatever the photos did meanwhile.
#[test]
fn a_stopped_check_says_stopped_even_if_the_photos_changed() {
    let (mut h, share) = app_on_a_slow_share(Duration::from_secs(3));
    let rx = ask(&mut h);
    for _ in 0..3 {
        h.step();
    }
    h.app.session.catalog.apply(Op::AddPhoto { photo: new_photo(PHOTOS + 1, "/share/IMG_9999.jpg") }).unwrap();
    let id = missing_rows(&h)[0].id;
    let r = h.request("engine.execute", json!({"command": "activity.cancel", "params": {"id": id}}), T);
    assert_eq!(r["ok"], true, "{r}");
    share.open.store(true, Ordering::SeqCst);
    let reply = answer(&mut h, &rx);
    assert!(reply["error"].as_str().is_some_and(|e| e.contains("stopped")), "{reply}");
}

/// A check nobody waited for, over photos that changed while it ran, is not kept: the next request
/// checks afresh (and then sees the photo that came meanwhile).
#[test]
fn a_check_over_changed_photos_is_not_kept_for_a_later_request() {
    let (mut h, share) = app_on_a_slow_share(Duration::from_secs(3));
    drop(ask(&mut h));
    for _ in 0..3 {
        h.step();
    }
    h.app.session.catalog.apply(Op::AddPhoto { photo: new_photo(PHOTOS + 1, "/share/IMG_0000x.jpg") }).unwrap();
    share.open.store(true, Ordering::SeqCst);
    assert!(h.step_until(T, |h| h.app.tasks.is_empty()));
    let calls = share.calls.load(Ordering::SeqCst);
    let again = ask(&mut h);
    let reply = answer(&mut h, &again);
    assert_eq!(reply["result"].as_array().map(Vec::len), Some(11), "{reply}");
    assert!(share.calls.load(Ordering::SeqCst) > calls, "checked afresh");
}

/// An edit that leaves the photos' files alone (a rating) does not spoil the check.
#[test]
fn an_edit_that_leaves_the_files_alone_does_not_spoil_the_check() {
    let (mut h, share) = app_on_a_slow_share(Duration::from_secs(3));
    let rx = ask(&mut h);
    for _ in 0..3 {
        h.step();
    }
    h.app.session.catalog.apply(Op::SetRating { id: PhotoId(1), rating: 3 }).unwrap();
    share.open.store(true, Ordering::SeqCst);
    assert_eq!(answer(&mut h, &rx)["ok"], true);
}

/// A check that can't start for one library fails only the requests for that library.
#[test]
fn a_failed_start_fails_only_that_librarys_requests() {
    let mut state = crate::missing_sweep::State::default();
    let (a_tx, a_rx) = std::sync::mpsc::channel();
    let (b_tx, b_rx) = std::sync::mpsc::channel();
    state.waiting.push((1, a_tx));
    state.waiting.push((2, b_tx));
    state.fail(2, "Missing Photos: could not start");
    assert!(b_rx.try_recv().is_ok_and(|v| v["ok"] == false));
    assert!(a_rx.try_recv().is_err(), "the other library's request keeps waiting");
    assert_eq!(state.waiting.len(), 1);
}

/// An answer kept for a request that never came is not given once the photos have changed.
#[test]
fn a_kept_answer_is_not_given_after_the_photos_changed() {
    let (mut h, share) = app_on_a_slow_share(Duration::from_secs(3));
    drop(ask(&mut h));
    for _ in 0..3 {
        h.step();
    }
    share.open.store(true, Ordering::SeqCst);
    assert!(h.step_until(T, |h| h.app.tasks.is_empty()));
    let p =
        Photo::new(PhotoId(PHOTOS + 1), Source::File { path: "/share/IMG_0000x.jpg".into() }, "IMG_0000x.jpg", "JPEG", 60, 40, "2026-01-01T00:00:00");
    h.app.session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let calls = share.calls.load(Ordering::SeqCst);
    let again = ask(&mut h);
    let reply = answer(&mut h, &again);
    assert_eq!(reply["result"].as_array().map(Vec::len), Some(11), "{reply}");
    assert!(share.calls.load(Ordering::SeqCst) > calls);
}
