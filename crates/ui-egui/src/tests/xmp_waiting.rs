//! Automatic XMP writes that wait for their folder (a network share that dropped out) show a row in
//! the activity stack for as long as they wait: a person who sees no sidecar next to a file has
//! somewhere to look, and the row goes when the writes are made.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(60);

/// An app with `n` imported PNGs in a folder that takes no writes, auto-write on; `None` when this
/// user writes there anyway (root).
fn app_over_read_only_folder(tag: &str, n: u8) -> Option<(Headless, std::path::PathBuf)> {
    let dir = std::env::temp_dir().join(format!("lc-xmp-waiting-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..n {
        let img = lightcraft_raster::Rgba8 { width: 16, height: 12, data: vec![[40 + i, 90, 160, 255]; 16 * 12] };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join(format!("p{i}.png")), png).unwrap();
    }
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    session.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    // the assertions read English, whatever LIGHTCRAFT_LANGUAGE says
    h.app.ui.language = crate::i18n::Locale::En;
    h.settle(T);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(dir.join(".probe"), b"x").is_ok() {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        return None;
    }
    Some((h, dir))
}

fn open_folder(dir: &std::path::Path) {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn waiting_rows(h: &Headless) -> Vec<lightcraft_engine::activity::TaskInfo> {
    h.app.session.activity.list().into_iter().filter(|t| t.kind == "xmpWaiting").collect()
}

/// Given automatic XMP writes that can't reach their folder, the activity stack says they wait (with how many),
/// keeps the one row across the re-checks, and drops it once they are written.
#[test]
fn writes_waiting_for_their_folder_have_a_row_until_they_are_made() {
    let Some((mut h, dir)) = app_over_read_only_folder("row", 2) else { return };
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    h.request("engine.execute", json!({"command": "photo.rate", "params": {"ids": ids, "rating": 4}}), T);
    h.step();
    assert_eq!(h.app.session.unsaved_sidecars(), 2, "both writes wait");
    let rows = waiting_rows(&h);
    assert_eq!(rows.len(), 1, "{:?}", h.app.session.activity.list());
    // no "0 of 2": nothing here counts up, so the row says how many wait and carries no bar
    assert_eq!((rows[0].label.as_str(), rows[0].total, rows[0].cancellable), ("Waiting to write XMP sidecars", 2, false), "{rows:?}");
    assert_eq!(crate::panels::activity::detail_line(&rows[0]), "Photos waiting: 2", "{rows:?}");
    let id = rows[0].id;

    // a re-check that finds the folder still shut leaves the same row, not a new one
    h.app.session.retry_unsaved_sidecars();
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(waiting_rows(&h).iter().map(|r| r.id).collect::<Vec<_>>(), vec![id], "one row all along");

    // the folder takes writes again
    open_folder(&dir);
    // a retry skips a file whose availability the cache hasn't answered yet (its worker thread may lag on a busy
    // machine), so retry until the writes are made rather than counting on one call; the frame's own 10 s retry is held
    // off so that every write is counted here
    h.app.ui.sidecar_retry_at = f64::INFINITY;
    let t0 = std::time::Instant::now();
    let mut written = 0;
    while written < 2 && t0.elapsed() < T {
        written += h.app.session.retry_unsaved_sidecars();
        h.step();
    }
    assert_eq!(written, 2, "still waiting: {}", h.app.session.unsaved_sidecars());
    assert!(waiting_rows(&h).is_empty(), "written: no row");
    assert!((0..2).all(|i| dir.join(format!("p{i}.xmp")).exists()), "both sidecars are on disk");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A write that is not waiting any more because auto-write was turned off takes its row with it.
#[test]
fn turning_auto_write_off_drops_the_waiting_row() {
    let Some((mut h, dir)) = app_over_read_only_folder("off", 1) else { return };
    h.request("engine.execute", json!({"command": "photo.rate", "params": {"rating": 3}}), T);
    h.step();
    assert_eq!(waiting_rows(&h).len(), 1);
    h.request("engine.execute", json!({"command": "library.xmpPreferences", "params": {"autoWrite": false}}), T);
    h.app.session.retry_unsaved_sidecars();
    h.step();
    assert!(waiting_rows(&h).is_empty());
    open_folder(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}
