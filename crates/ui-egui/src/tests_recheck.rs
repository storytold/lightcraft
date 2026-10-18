//! Issue #662: opening a library rechecks its preview-only raws in the background.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lightcraft_catalog::{Op, PhotoId};
use serde_json::json;

use crate::LightcraftApp;

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An app on a library at `dir/lib` with one photo whose preview-only flag an older version set.
/// Its file (a PNG) reads fine, as a raw a newer decoder handles would.
fn app_with_stale_photo(dir: &Path) -> (LightcraftApp, PhotoId) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("files")).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, 50, 255]);
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    let file = dir.join("files/one.png");
    std::fs::write(&file, lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("lib"), false).unwrap();
    let r = session.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let id = PhotoId(r["imported"][0].as_u64().unwrap());
    let p = session.catalog.photo(id).unwrap().clone();
    session
        .catalog
        .apply(Op::SetContent {
            id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: p.content_hash.clone(),
            preview_only: Some("Cr3 files are not decoded yet".into()),
            preview_checked: "0.5.0".into(),
        })
        .unwrap();
    (LightcraftApp::new(session, Default::default()), id)
}

#[test]
fn opening_a_library_rechecks_stale_preview_only_raws_in_the_background() {
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-recheck-{}", std::process::id())));
    let (mut app, id) = app_with_stale_photo(&dir.0);
    let ctx = egui::Context::default();
    crate::recheck::tick(&mut app);
    // it shows in the activity stack while it runs
    let rows = app.session.activity.list();
    assert!(rows.iter().any(|r| r.kind == "recheck" && r.label == crate::recheck::LABEL), "{rows:?}");
    assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
    assert!(app.session.activity.list().iter().all(|r| r.kind != "recheck"), "the row goes with the task");
    let p = app.session.catalog.photo(id).unwrap();
    assert_eq!(p.preview_only, None);
    assert!(app.ui.toast.as_ref().is_some_and(|(t, _, _)| t == "1 raw can now be developed from its raw data"), "{:?}", app.ui.toast);
    // once per opened library
    app.ui.toast = None;
    crate::recheck::tick(&mut app);
    assert!(app.tasks.is_empty());
}

/// A result for a library that was closed meanwhile is dropped, not applied to the new one.
#[test]
fn recheck_result_is_dropped_after_switching_libraries() {
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-recheck-switch-{}", std::process::id())));
    let (mut app, id) = app_with_stale_photo(&dir.0);
    let ctx = egui::Context::default();
    crate::recheck::tick(&mut app);
    app.run("app.openLibrary", json!({"path": dir.0.join("other").to_string_lossy()})).unwrap();
    assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
    assert!(app.ui.toast.is_none(), "{:?}", app.ui.toast);
    assert!(app.session.catalog.photo(id).is_none(), "the other library is untouched");
}

/// The notice's Reload from Disk re-reads the file and says what came of it.
#[test]
fn notice_reload_button_rereads_the_file() {
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-recheck-reload-{}", std::process::id())));
    let (mut app, id) = app_with_stale_photo(&dir.0);
    let ctx = egui::Context::default();
    crate::panels::edit::reload_preview_only(&mut app, &ctx, id);
    assert_eq!(app.session.catalog.photo(id).unwrap().preview_only, None);
    assert!(app.ui.toast.as_ref().is_some_and(|(t, _, _)| t == "This raw is now developed from its raw data"), "{:?}", app.ui.toast);
}
