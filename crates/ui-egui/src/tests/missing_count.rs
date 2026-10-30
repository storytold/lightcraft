//! The sidebar's Missing Photos count while photos come into a large library.
//!
//! * Every file is checked once (one at a time, after what the panels ask), then only the files
//!   that came in: never a check of the whole library per batch on the share the photos are on.
//! * An edit that moves no file asks about no file.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);

fn photo(id: PhotoId, folder: &str, name: &str) -> Photo {
    Photo::new(id, Source::File { path: format!("/lightcraft-counts/{folder}/{name}") }, name, "JPEG", 60, 40, "2026-10-16T08:00:00")
}

/// The app on a library of `n` photos in two folders (`gone` of them named so: the probe says
/// those files are not there), its sidebar open.
fn app(n: usize, gone: usize, probe: lightcraft_engine::availability::ExistsProbe) -> Headless {
    let mut s = lightcraft_engine::Session::new();
    let ops: Vec<Op> = (0..n)
        .map(|i| {
            let name = if i < gone { format!("gone_{i:05}.jpg") } else { format!("IMG_{i:05}.jpg") };
            Op::AddPhoto { photo: Box::new(photo(s.catalog.alloc_photo_id(), if i % 2 == 0 { "Entities/Z" } else { "LG/007" }, &name)) }
        })
        .collect();
    s.catalog.apply(Op::Batch { ops }).unwrap();
    s.media.availability.set_probe(probe);
    let app = LightcraftApp::new(s, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(T));
    h
}

fn there(path: &str) -> bool {
    !path.contains("gone")
}

fn missing(h: &mut Headless) -> usize {
    let library = h.app.session.library_generation();
    let s = &h.app.session;
    h.app.caches.missing_count(&s.catalog, library, &s.media.availability)
}

/// Frames until the count is `n` (the sidebar draws every frame: it starts the checks).
fn reach(h: &mut Headless, n: usize) -> usize {
    let t0 = std::time::Instant::now();
    while missing(h) != n && t0.elapsed() < T {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    missing(h)
}

/// `k` photos added to `folder` as one step, the way an import's batch lands.
fn batch(h: &mut Headless, k: usize, folder: &str, gone: usize) {
    let ops: Vec<Op> = (0..k)
        .map(|i| {
            let id = h.app.session.catalog.alloc_photo_id();
            let name = if i < gone { format!("gone_new_{}.jpg", id.0) } else { format!("NEW_{}.jpg", id.0) };
            Op::AddPhoto { photo: Box::new(photo(id, folder, &name)) }
        })
        .collect();
    h.app.session.execute_fn("folder.synchronize", |s| s.commit("Import", Op::Batch { ops }).map(|()| Value::Null)).unwrap();
}

#[test]
fn the_missing_count_checks_each_file_once_then_only_the_files_that_came_in() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let mut h = app(
        400,
        10,
        Arc::new(move |p: &str| {
            c.fetch_add(1, Ordering::SeqCst);
            there(p)
        }),
    );
    assert_eq!(reach(&mut h, 10), 10, "every file checked once");
    assert!(h.step_until(T, |h| h.app.session.media.availability.sweeping() == 0 && h.app.session.media.availability.pending() == 0));
    let before = calls.load(Ordering::SeqCst);
    assert!(before >= 400, "each file was checked ({before})");
    batch(&mut h, 16, "LG/007", 3);
    assert_eq!(reach(&mut h, 13), 13, "the files that came in are checked");
    let after = calls.load(Ordering::SeqCst) - before;
    assert!(after < 100, "only the files that came in (and the grid's cells), not the library again: {after} checks");
}

/// A probe that logs every path it checks (the ones named `gone…` are not there).
fn logging(log: &Arc<Mutex<Vec<String>>>) -> lightcraft_engine::availability::ExistsProbe {
    let l = log.clone();
    Arc::new(move |p: &str| {
        l.lock().unwrap_or_else(PoisonError::into_inner).push(p.to_string());
        there(p)
    })
}

fn checked(log: &Arc<Mutex<Vec<String>>>) -> usize {
    log.lock().unwrap_or_else(PoisonError::into_inner).len()
}

#[test]
fn a_rating_or_an_album_asks_about_no_file_and_a_photo_moved_to_a_missing_file_is_counted() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut h = app(600, 0, logging(&log));
    assert_eq!(reach(&mut h, 0), 0);
    assert!(h.step_until(T, |h| h.file_checks_done()));
    // a photo nobody has asked about since the cache was cleared: its file is not known to be fresh
    let log = Arc::new(Mutex::new(Vec::new()));
    h.app.session.media.availability.set_probe(logging(&log));
    assert!(h.step_until(T, |h| h.file_checks_done()));
    let seen: std::collections::HashSet<String> = log.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned().collect();
    let quiet = h
        .app
        .session
        .catalog
        .photos()
        .find(|p| p.library_file().is_some_and(|f| !seen.contains(f)))
        .map(|p| (p.id, p.library_file().unwrap_or_default().to_string()))
        .expect("a photo the grid did not ask about");
    let before = checked(&log);
    let rate = Op::SetRating { id: quiet.0, rating: 3 };
    h.app.session.execute_fn("photo.rate", |s| s.commit("Rate", rate).map(|()| Value::Null)).unwrap();
    // (a photo added to an album, the other edit that changes no file)
    let album_id = h.app.session.catalog.alloc_album_id();
    let album = lightcraft_catalog::Album { photos: vec![quiet.0], ..lightcraft_catalog::Album::new(album_id, "Trip") };
    h.app.session.execute_fn("album.new", |s| s.commit("Album", Op::AddAlbum { album }).map(|()| Value::Null)).unwrap();
    for _ in 0..20 {
        h.step();
        missing(&mut h);
    }
    assert!(h.step_until(T, |h| h.file_checks_done()));
    assert_eq!(checked(&log), before, "no file was checked for a rating or an album: {quiet:?}");
    assert_eq!(h.app.session.media.availability.sweeping(), 0, "and no check of every file began");
    // a photo that moves to a file that is not there is counted once the check finds it so
    let op = Op::Relink {
        id: quiet.0,
        file_name: "gone_x.jpg".into(),
        source: Source::File { path: "/lightcraft-counts/A/gone_x.jpg".into() },
        format: None,
    };
    h.app.session.execute_fn("photo.relink", |s| s.commit("Relink", op).map(|()| Value::Null)).unwrap();
    assert_eq!(reach(&mut h, 1), 1, "the file the photo points at now is checked and found gone");
}

#[test]
fn a_batch_into_a_large_library_with_a_file_gone_is_followed_without_a_walk_over_every_photo() {
    let mut h = app(20_000, 1, Arc::new(there));
    assert_eq!(reach(&mut h, 1), 1);
    assert!(h.step_until(T, |h| h.file_checks_done()));
    // (the count settles: whatever is still to be found gone is found)
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(reach(&mut h, 1), 1);
    let passes = h.app.caches.missing.walks();
    for b in 0..10 {
        batch(&mut h, 16, "LG/007", usize::from(b == 0));
        let expect = 2;
        assert_eq!(reach(&mut h, expect), expect);
    }
    assert!(h.step_until(T, |h| h.file_checks_done()));
    let more = h.app.caches.missing.walks() - passes;
    assert_eq!(more, 0, "no walk over every photo for a batch, or for a file found gone: {more} walks in 10 batches");
}

#[test]
fn the_sweep_gives_way_to_a_scan_the_app_holds_and_goes_on_when_it_ends() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut s = lightcraft_engine::Session::new();
    let ops: Vec<Op> =
        (0..600).map(|i| Op::AddPhoto { photo: Box::new(photo(s.catalog.alloc_photo_id(), "Z", &format!("IMG_{i:04}.jpg"))) }).collect();
    s.catalog.apply(Op::Batch { ops }).unwrap();
    s.media.availability.set_probe(logging(&log));
    // (a review scan under way: the guard is what the app's give-way reads)
    let scan = s.activity.start("scan", "Reading photos", lightcraft_engine::activity::Cancel::No);
    let app = LightcraftApp::new(s, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    let asked = checked(&log);
    assert!(asked < 300, "only what the grid shows is asked while the scan runs ({asked})");
    assert!(
        h.app.session.media.availability.sweeping() > 300,
        "the sweep waits: {} left, listing {}",
        h.app.session.media.availability.sweeping(),
        h.app.caches.files_listing.load(Ordering::SeqCst)
    );
    drop(scan);
    assert!(h.step_until(T, |h| h.file_checks_done()), "the sweep goes on when the scan ends");
    assert!(checked(&log) >= 600, "every file was checked in the end");
}

#[test]
fn opening_another_library_checks_its_files_before_the_rest_of_the_first_ones() {
    let files = {
        let mut a = lightcraft_engine::availability::Availability::default();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let l = log.clone();
        a.set_probe(Arc::new(move |p: &str| {
            std::thread::sleep(Duration::from_millis(3));
            l.lock().unwrap_or_else(PoisonError::into_inner).push(p.to_string());
            true
        }));
        a.run_in_background(Arc::new(|| {}));
        (a, log)
    };
    let (files, log) = files;
    let library = |dir: &str, n: usize| {
        let mut c = lightcraft_catalog::Catalog::new();
        let ops: Vec<Op> = (0..n).map(|i| Op::AddPhoto { photo: Box::new(photo(c.alloc_photo_id(), dir, &format!("IMG_{i:04}.jpg"))) }).collect();
        c.apply(Op::Batch { ops }).unwrap();
        c
    };
    let (first, second) = (library("first", 400), library("second", 5));
    let mut caches = crate::Caches::default();
    caches.check_files(&first, 1, &files, 0.0);
    let t0 = std::time::Instant::now();
    while checked(&log) < 5 && t0.elapsed() < T {
        std::thread::sleep(Duration::from_millis(2));
    }
    // library 2 is opened while the first one's files are being checked
    let t0 = std::time::Instant::now();
    while t0.elapsed() < T {
        caches.check_files(&second, 2, &files, 1.0);
        let seen = log.lock().unwrap_or_else(PoisonError::into_inner);
        if seen.iter().filter(|p| p.contains("/second/")).count() == 5 {
            break;
        }
        drop(seen);
        std::thread::sleep(Duration::from_millis(2));
    }
    let seen = log.lock().unwrap_or_else(PoisonError::into_inner);
    let firsts = seen.iter().filter(|p| p.contains("/first/")).count();
    assert_eq!(seen.iter().filter(|p| p.contains("/second/")).count(), 5, "the new library's files are checked");
    assert!(firsts < 200, "not after the old library's whole list: {firsts} of its 400 were checked");
}

/// Given the thread that lists the library's files panics,
/// When it ends,
/// Then the "listing" flag is clear again, so the next check starts a sweep instead of seeing one forever.
#[test]
fn a_listing_thread_that_panics_still_clears_the_flag() {
    let flag = Arc::new(AtomicBool::new(false));
    crate::caches::run_listing(&flag, || panic!("the listing failed")).unwrap();
    let started = std::time::Instant::now();
    while flag.load(Ordering::Acquire) {
        assert!(started.elapsed() < T, "the flag stayed set after the thread panicked");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Given every file is there, the checks have ended and nothing else asks for frames (no card scan, no watched folder),
/// When the app sits idle,
/// Then the sidebar still asks for a frame no later than the next sweep is due, so the sweep isn't left waiting for unrelated activity.
#[test]
fn an_idle_app_still_asks_for_a_frame_by_the_time_the_next_sweep_is_due() {
    let mut h = app(40, 0, Arc::new(|_: &str| true));
    // (the card scan asks for a frame every few seconds while this is on, which would hide the gap)
    h.app.session.import_defaults.show_on_card = false;
    assert!(h.step_until(T, |h| h.app.session.media.availability.sweeping() == 0 && h.app.session.media.availability.pending() == 0));
    // frames until nothing else asks for one within a few seconds (the renderer's idle trim does, once, for 3 s)
    let t0 = std::time::Instant::now();
    h.step();
    while h.view.repaint_after < Duration::from_secs(10) && t0.elapsed() < T {
        std::thread::sleep(Duration::from_millis(50));
        h.step();
    }
    assert!(h.view.repaint_after >= Duration::from_secs(10), "frames were still asked for every few seconds after {T:?}: {:?}", h.view.repaint_after);
    assert!(h.view.repaint_after <= Duration::from_secs(300), "no frame was asked for within the sweep interval: {:?}", h.view.repaint_after);
}
