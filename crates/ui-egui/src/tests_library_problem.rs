//! Issue #100: a library that can't be opened at launch is never replaced by a silent in-memory
//! demo session. The window says why and offers Try Again / Choose Another Library… / Continue
//! Without Saving / Quit; a temporary session shows a banner and never writes to the library.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::library_problem::LibraryProblem;
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

#[test]
fn unopenable_library_asks_instead_of_running_a_demo() {
    let dir = std::env::temp_dir().join(format!("lc-ui-libproblem-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let img = dac_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 3, 9, 255]; 64] };
    let png = dac_codecs::encode_png(&dac_codecs::EncodeImage::rgba8(&img), &dac_codecs::EncodeMeta::default()).unwrap();
    let photo = dir.join("a.png");
    std::fs::write(&photo, png).unwrap();

    // another program has the library open: the launch fails like the desktop host's would
    let held = dac_catalog::LibraryLock::acquire(&lib, "another program").unwrap();
    let mut session = dac_engine::Session::new().with_fs();
    let err = session.open_library(&lib, true).unwrap_err().to_string();
    let png = |img: &dac_raster::Rgba8| {
        dac_codecs::encode_png(&dac_codecs::EncodeImage::rgba8(img), &dac_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let mut app = DacApp::new(
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
    if let Some(p) = dac_brand::env_os("TEST_SHOTS") {
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
    if let Some(p) = dac_brand::env_os("TEST_SHOTS") {
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
    let dir = std::env::temp_dir().join(format!("lc-ui-libupgrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let mut c = dac_catalog::Catalog::new();
    for i in 0..3 {
        let id = c.alloc_photo_id();
        let p = dac_catalog::Photo::new(
            id,
            dac_catalog::Source::File { path: format!("/nowhere/{i}.jpg") },
            &format!("{i}.jpg"),
            "JPEG",
            8,
            8,
            "2026-01-01T00:00:00",
        );
        c.apply(dac_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let snap = format!("{{\"format\":\"dac-catalog\",\"version\":3,\"seq\":3,\"catalog\":{}}}\n", c.to_snapshot());
    std::fs::write(lib.join("catalog.snap"), snap).unwrap();
    assert!(dac_engine::library::needs_migration(&lib));

    let mut app = DacApp::new(dac_engine::Session::new().with_fs(), Services::default());
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
    assert!(!dac_engine::library::needs_migration(&lib));
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}
