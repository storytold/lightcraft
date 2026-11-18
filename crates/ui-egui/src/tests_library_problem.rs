//! Issue #100: a library that can't be opened at launch is never replaced by a silent in-memory
//! demo session. The window says why and offers Try Again / Choose Another Library… / Continue
//! Without Saving / Quit; a temporary session shows a banner and never writes to the library.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::library_problem::LibraryProblem;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

#[test]
fn unopenable_library_asks_instead_of_running_a_demo() {
    let dir = lcc_testkit::TempDir::new("lc-ui-libproblem");
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 3, 9, 255]; 64] };
    let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    let photo = dir.join("a.png");
    std::fs::write(&photo, png).unwrap();

    // another program has the library open: the launch fails like the desktop host's would
    let held = lcc_catalog::LibraryLock::acquire(&lib, "another program").unwrap();
    let mut session = lcc_engine::Session::new().with_fs();
    let err = session.open_library(&lib, true).unwrap_err().to_string();
    let png = |img: &lcc_raster::Rgba8| {
        lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(img), &lcc_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let mut app = LccApp::new(
        session,
        Services {
            png: Some(Box::new(png)),
            write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
            ..Default::default()
        },
    );
    app.library_problem =
        Some(LibraryProblem { pending_import: vec![photo.to_string_lossy().to_string()], ..LibraryProblem::new(lib.to_string_lossy(), err) });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert!(h.app.session.catalog.is_empty(), "no demo photos");
    for b in ["button:libraryRetry", "button:libraryChoose", "button:libraryTemporary", "button:libraryQuit"] {
        assert!(has(&h, b), "{b}");
    }
    if let Some(p) = lcc_brand::env_os("TEST_SHOTS") {
        let r = h.request("ui.screenshot", json!({"path": std::path::Path::new(&p).join("dialog.png").to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    let inspect = h.request("ui.inspect", json!({}), T);
    let p = &inspect["result"]["libraryProblem"];
    assert!(p["error"].as_str().unwrap().contains("already open in another program"), "{p}");
    assert_eq!(p["temporarySession"], false);

    // still locked: Try Again keeps the window, with the error
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryRetry"}), T)["ok"], true);
    h.step();
    assert!(h.app.session.library.is_none());
    assert!(has(&h, "button:libraryRetry"));

    // Continue Without Saving: a banner, the command-line photo in the temporary session, and
    // nothing written into the library folder
    let before: Vec<_> = std::fs::read_dir(&lib).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryTemporary"}), T)["ok"], true);
    h.step();
    h.step();
    assert!(!has(&h, "button:libraryRetry"));
    assert!(has(&h, "indicator:temporarySession"));
    assert!(h.app.session.library.is_none());
    // imported on the import worker (issue #374: never synchronously on the UI thread)
    h.step_until(T, |h| h.app.import.is_none() && h.app.session.catalog.len() == 1);
    assert_eq!(h.app.session.catalog.len(), 1, "the command-line photo is imported into the temporary session");
    if let Some(p) = lcc_brand::env_os("TEST_SHOTS") {
        let r = h.request("ui.screenshot", json!({"path": std::path::Path::new(&p).join("banner.png").to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.request("engine.execute", json!({"command": "photo.rate", "params": {"rating": 3}}), T);
    let after: Vec<_> = std::fs::read_dir(&lib).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(before, after, "the temporary session never writes to the library");

    // the other program quits; Open Library… → Try Again opens it, the banner goes away
    drop(held);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryReopen"}), T)["ok"], true);
    h.step();
    assert!(has(&h, "button:libraryRetry"));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:libraryRetry"}), T)["ok"], true);
    h.step();
    h.step();
    assert!(h.app.session.library.is_some());
    assert!(h.app.library_problem.is_none());
    assert!(!has(&h, "indicator:temporarySession") && !has(&h, "button:libraryRetry"));
    assert!(h.app.session.catalog.is_empty(), "an opened (new) library is never seeded with demo photos here");
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library in an older catalog format is upgraded on a worker thread with a progress window,
/// then opened (command-line files imported after).
#[test]
fn old_catalog_is_upgraded_with_progress_then_opened() {
    let dir = lcc_testkit::TempDir::new("lc-ui-libupgrade");
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let mut c = lcc_catalog::Catalog::new();
    for i in 0..3 {
        let id = c.alloc_photo_id();
        let p = lcc_catalog::Photo::new(
            id,
            lcc_catalog::Source::File { path: format!("/nowhere/{i}.jpg") },
            &format!("{i}.jpg"),
            "JPEG",
            8,
            8,
            "2026-01-01T00:00:00",
        );
        c.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let snap = format!("{{\"format\":\"lcc-catalog\",\"version\":3,\"seq\":3,\"catalog\":{}}}\n", c.to_snapshot());
    std::fs::write(lib.join("catalog.snap"), snap).unwrap();
    assert!(lcc_engine::library::needs_migration(&lib));

    let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), Services::default());
    crate::panels::library_problem::start_upgrade(&mut app, lib.clone(), Vec::new());
    assert!(app.library_problem.as_ref().is_some_and(|p| p.upgrading));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let t0 = std::time::Instant::now();
    while h.app.session.library.is_none() {
        assert!(t0.elapsed() < T, "the upgrade never finished: {:?}", h.app.library_problem);
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.step();
    assert!(h.app.library_problem.is_none(), "{:?}", h.app.library_problem);
    assert_eq!(h.app.session.catalog.len(), 3);
    assert!(!lcc_engine::library::needs_migration(&lib));
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Files that waited for a library find an import running: they are kept and imported once it
/// ends, instead of being dropped (upstream #3403).
#[test]
fn files_waiting_for_a_library_wait_for_a_running_import() {
    let dir = lcc_testkit::TempDir::new("lc-ui-waiting-import");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..300u32 {
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[(i % 251) as u8, (i / 251) as u8, 9, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join(format!("{i}.png")), png).unwrap();
    }
    let late = lcc_testkit::TempFile::new("lc-ui-waiting-late", "late.png");
    let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[1, 2, 3, 255]; 64] };
    std::fs::write(&late, lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap()).unwrap();
    let mut h = Headless::new(LccApp::new(lcc_engine::Session::new(), Services { png: None, ..Default::default() }), [1200.0, 900.0], 1.0);
    h.settle(Duration::from_secs(20));
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).unwrap();
    h.app.waiting_import = vec![late.to_string_lossy().to_string()];
    h.step();
    assert!(!h.app.waiting_import.is_empty(), "kept while the import runs");
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_secs(60) && (!h.app.waiting_import.is_empty() || crate::import::running(&h.app)) {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(h.app.waiting_import.is_empty() && !crate::import::running(&h.app), "imported after the first import ended");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&late);
}

/// The catalog opens on a worker thread once the window is up ([`start_open`]): a small window
/// says so while it runs (the launch open used to block the window's first frames), and is gone
/// when the library is there; command-line files are imported into it after, as behind the
/// upgrade window.
#[test]
fn catalog_opens_on_a_worker_with_a_window() {
    let dir = lcc_testkit::TempDir::new("lc-ui-libopen");
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    // a library in the current format: the open is the plain one the worker hands back
    let mut made = lcc_engine::Session::new().with_fs();
    made.open_library(&lib, false).unwrap();
    drop(made);
    let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 3, 9, 255]; 64] };
    let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    let photo = dir.join("a.png");
    std::fs::write(&photo, png).unwrap();

    // the window while the catalog opens: a bar (sweeping, or an upgrade's phase) and no way out
    // — it is not a choice. A task of the open's label keeps it up, as the real worker would
    // (one whose worker stopped becomes the problem window instead)
    let mut held = LccApp::new(lcc_engine::Session::new().with_fs(), Services::default());
    held.library_problem = Some(LibraryProblem { opening: true, ..LibraryProblem::new(lib.to_string_lossy(), "") });
    let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wait = {
        let release = release.clone();
        move || {
            while !release.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    crate::tasks::spawn(&mut held, "Open catalog", None, wait, |_, _, _| {}).unwrap();
    let mut h = Headless::new(held, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert!(has(&h, "progress:libraryOpening"), "the opening window shows");
    let inspect = h.request("ui.inspect", json!({}), T);
    let p = &inspect["result"]["libraryProblem"];
    assert_eq!(p["opening"], true, "{p}");
    assert_eq!(p["upgrading"], false, "{p}");
    release.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(h);

    // the open itself: the worker prepares the library (a format upgrade on the way), then it
    // opens; the window goes away and the waiting file is imported
    let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), Services::default());
    crate::panels::library_problem::start_open(&mut app, lib.clone(), vec![photo.to_string_lossy().to_string()], false, false);
    assert!(app.library_problem.as_ref().is_some_and(|p| p.opening));
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let t0 = std::time::Instant::now();
    while h.app.session.library.is_none() {
        assert!(t0.elapsed() < T, "the open never finished: {:?}", h.app.library_problem);
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.step();
    assert!(h.app.library_problem.is_none(), "{:?}", h.app.library_problem);
    assert!(!has(&h, "progress:libraryOpening"), "the window is gone");
    // imported on the import worker once the library is open (issue #374)
    h.step_until(T, |h| h.app.import.is_none() && h.app.session.catalog.len() == 1);
    assert_eq!(h.app.session.catalog.len(), 1, "the command-line photo is imported into the library");
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A catalog the worker can't open (another program has it) ends as the usual problem window
/// with the error, never a stuck opening.
#[test]
fn an_unopenable_catalog_ends_as_the_problem_window() {
    let dir = lcc_testkit::TempDir::new("lc-ui-libopen-fail");
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    let mut made = lcc_engine::Session::new().with_fs();
    made.open_library(&lib, false).unwrap();
    drop(made);
    let held = lcc_catalog::LibraryLock::acquire(&lib, "another program").unwrap();

    let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), Services::default());
    crate::panels::library_problem::start_open(&mut app, lib.clone(), Vec::new(), false, false);
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let t0 = std::time::Instant::now();
    while h.app.library_problem.as_ref().is_none_or(|p| p.opening) {
        assert!(t0.elapsed() < T, "the open never finished");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    let p = h.app.library_problem.as_ref().unwrap();
    assert!(p.error.contains("already open in another program"), "{}", p.error);
    // (the first frame only readies the fonts; the window is drawn on the next, and its centred
    // position settles once the wrapped error text has its height)
    h.settle(Duration::from_secs(5));
    assert!(has(&h, "button:libraryRetry"), "the usual problem window");

    // the other program quits; Try Again opens it and the problem is over
    drop(held);
    assert!(h.click_as_user("button:libraryRetry"), "the Try Again button is on screen");
    let t0 = std::time::Instant::now();
    while h.app.library_problem.is_some() || h.app.session.library.is_none() {
        assert!(t0.elapsed() < T, "the retry never finished: {:?}", h.app.library_problem);
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An opening whose worker stops without an answer (it panicked) becomes the problem window,
/// never a window without a way out.
#[test]
fn a_stopped_open_ends_as_the_problem_window() {
    let mut app = LccApp::new(lcc_engine::Session::new(), Services::default());
    app.library_problem = Some(LibraryProblem { opening: true, ..LibraryProblem::new("/nowhere", "") });
    let mut h = Headless::new(app, [800.0, 600.0], 1.0);
    h.step();
    h.step();
    let p = h.app.library_problem.as_ref().expect("still a problem");
    assert!(!p.opening, "no longer an opening");
    assert!(!p.error.is_empty(), "says what happened: {}", p.error);
    assert!(has(&h, "button:libraryRetry"), "the usual problem window, with a way out");
    drop(h);
}
