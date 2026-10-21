//! The sidebar's counts of the whole library (folders, dates, keywords, All Photos) while an import or a
//! Synchronize Folder lands batch after batch in a large library.
//!
//! Scenarios, in the words of someone importing 20,000 photos into a library of 180,000:
//!
//! * Each batch shows up in the sidebar's counts on the frame it lands, without the window
//!   counting every photo of the library again.
//! * A change too large to follow photo by photo (a library opened, thousands of photos at once)
//!   is counted off the UI thread; the counts shown stay until the new ones are made.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lightcraft_catalog::library_tally::{LibraryTally, MAX_FOLLOWED};
use lightcraft_catalog::{Catalog, FolderNode, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);

fn photo(id: PhotoId, folder: &str, name: &str) -> Photo {
    Photo::new(id, Source::File { path: format!("/lightcraft-counts/{folder}/{name}") }, name, "JPEG", 60, 40, "2026-10-16T08:00:00")
}

/// The app on a library of `n` photos in two folders, its sidebar open and every count made. Whole counts are made off
/// the UI thread whatever the library's size (the app does so past a few thousand photos).
fn app(n: usize) -> Headless {
    let mut s = lightcraft_engine::Session::new();
    let ops: Vec<Op> = (0..n)
        .map(|i| {
            let name = format!("IMG_{i:05}.jpg");
            Op::AddPhoto { photo: Box::new(photo(s.catalog.alloc_photo_id(), if i % 2 == 0 { "Entities/Z" } else { "LG/007" }, &name)) }
        })
        .collect();
    s.catalog.apply(Op::Batch { ops }).unwrap();
    let mut app = LightcraftApp::new(s, Services { png: None, ..Default::default() });
    app.caches.library.count_in_background_past(0);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(T));
    h
}

fn find(nodes: &[FolderNode], name: &str) -> Option<usize> {
    nodes.iter().find_map(|n| if n.name == name { Some(n.count) } else { find(&n.children, name) })
}

/// The photos the sidebar shows in folder `name`.
fn folder_count(h: &mut Headless, name: &str) -> Option<usize> {
    let library = h.app.session.library_generation();
    find(&h.app.caches.folder_tree(&h.app.session.catalog, library), name)
}

/// `k` photos added to `folder` as one step, the way an import's batch lands.
fn batch(h: &mut Headless, k: usize, folder: &str) {
    let ops: Vec<Op> = (0..k)
        .map(|_| {
            let id = h.app.session.catalog.alloc_photo_id();
            Op::AddPhoto { photo: Box::new(photo(id, folder, &format!("NEW_{}.jpg", id.0))) }
        })
        .collect();
    h.app.session.execute_fn("folder.synchronize", |s| s.commit("Import", Op::Batch { ops }).map(|()| Value::Null)).unwrap();
}

#[test]
fn a_batch_imported_into_a_large_library_is_counted_photo_by_photo_on_the_frame_it_lands() {
    let mut h = app(400);
    let made = h.app.caches.library.whole_counts();
    assert_eq!(folder_count(&mut h, "Z"), Some(200));
    batch(&mut h, 16, "Entities/Z");
    h.step();
    assert_eq!(folder_count(&mut h, "Z"), Some(216), "the batch is in the sidebar at once");
    let library = h.app.session.library_generation();
    assert_eq!(h.app.caches.counts(&h.app.session.catalog, library).total, 416);
    assert_eq!(h.app.caches.library.whole_counts(), made, "no count of every photo, here or in the background");
    assert!(!h.app.caches.library.busy());
}

#[test]
fn a_change_too_large_to_follow_is_counted_off_the_ui_thread_and_the_counts_shown_stay_meanwhile() {
    let mut h = app(400);
    // the count of every photo waits until the test lets it go
    let gate = Arc::new(Mutex::new(()));
    let held = gate.lock().unwrap_or_else(PoisonError::into_inner);
    let counted_on = Arc::new(Mutex::new(Vec::new()));
    let (g, on) = (gate.clone(), counted_on.clone());
    h.app.caches.library.count_with(Arc::new(move |cat: &Catalog| {
        drop(g.lock().unwrap_or_else(PoisonError::into_inner));
        on.lock().unwrap_or_else(PoisonError::into_inner).push(std::thread::current().id());
        LibraryTally::of(cat)
    }));
    batch(&mut h, MAX_FOLLOWED + 1, "Entities/Z");
    for _ in 0..5 {
        h.step();
        assert_eq!(folder_count(&mut h, "Z"), Some(200), "the counts shown before stay while the new ones are made");
        assert!(h.app.caches.library.busy(), "counted in the background");
    }
    drop(held);
    assert!(h.settle(T));
    assert_eq!(folder_count(&mut h, "Z"), Some(200 + MAX_FOLLOWED + 1));
    let on = counted_on.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(on.len(), 1, "counted once");
    assert!(on.iter().all(|t| *t != std::thread::current().id()), "never on the UI thread");
}

#[test]
fn a_rating_given_to_thousands_of_photos_is_not_a_count_of_the_library() {
    let mut h = app(MAX_FOLLOWED + 100);
    let made = h.app.caches.library.whole_counts();
    let ids: Vec<u64> = h.app.session.catalog.photos().map(|p| p.id.0).collect();
    h.app.session.execute("library.select", &json!({"ids": ids})).unwrap();
    h.app.session.execute("photo.rate", &json!({"rating": 3})).unwrap();
    h.step();
    assert_eq!(h.app.caches.library.whole_counts(), made, "a rating changes no count the sidebar shows");
    assert!(!h.app.caches.library.busy());
}

#[test]
fn another_library_opened_while_one_is_counted_never_shows_its_counts() {
    let library = |n: usize| {
        let mut c = Catalog::new();
        let ops: Vec<Op> = (0..n).map(|i| Op::AddPhoto { photo: Box::new(photo(c.alloc_photo_id(), "A", &format!("IMG_{i}.jpg"))) }).collect();
        c.apply(Op::Batch { ops }).unwrap();
        c
    };
    let (first, second) = (library(30), library(7));
    let mut work = crate::library_work::LibraryWork::default();
    work.count_in_background_past(0);
    let gate = Arc::new(Mutex::new(()));
    let held = gate.lock().unwrap_or_else(PoisonError::into_inner);
    let g = gate.clone();
    work.count_with(Arc::new(move |cat: &Catalog| {
        drop(g.lock().unwrap_or_else(PoisonError::into_inner));
        LibraryTally::of(cat)
    }));
    assert!(work.current(&first, 1).is_none(), "nothing before the first count");
    assert!(work.busy());
    // the second library opens while the first one's count runs
    assert!(work.current(&second, 2).is_none());
    drop(held);
    let t0 = std::time::Instant::now();
    let total = loop {
        if let Some(t) = work.current(&second, 2) {
            break t.counts().total;
        }
        assert!(t0.elapsed() < T, "the second library is counted");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(total, 7, "the second library's count, never the first's");
}

#[test]
fn a_folder_tree_made_while_the_counts_lag_is_not_a_fresh_one() {
    let mut h = app(400);
    let library = h.app.session.library_generation();
    assert_eq!(folder_count(&mut h, "Z"), Some(200));
    assert!(h.app.caches.folder_tree_if_fresh(&h.app.session.catalog, library).is_some(), "counted and built: fresh");
    // the count of every photo waits until the test lets it go
    let gate = Arc::new(Mutex::new(()));
    let held = gate.lock().unwrap_or_else(PoisonError::into_inner);
    let g = gate.clone();
    h.app.caches.library.count_with(Arc::new(move |cat: &Catalog| {
        drop(g.lock().unwrap_or_else(PoisonError::into_inner));
        LibraryTally::of(cat)
    }));
    batch(&mut h, MAX_FOLLOWED + 1, "Entities/New");
    for _ in 0..5 {
        h.step();
        assert!(h.app.caches.library.busy(), "counted in the background");
        assert!(
            h.app.caches.folder_tree_if_fresh(&h.app.session.catalog, library).is_none(),
            "the tree shown is the one from before the batch: it is not the library's folders now"
        );
    }
    drop(held);
    assert!(h.settle(T));
    assert_eq!(folder_count(&mut h, "New"), Some(MAX_FOLLOWED + 1));
    assert!(h.app.caches.folder_tree_if_fresh(&h.app.session.catalog, library).is_some(), "counted again: fresh");
}
