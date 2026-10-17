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

/// The notice's Reload from Disk re-reads the file in the background (a row in the activity stack,
/// the window not waiting for the decode) and says what came of it.
#[test]
fn notice_reload_button_rereads_the_file() {
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-recheck-reload-{}", std::process::id())));
    let (mut app, id) = app_with_stale_photo(&dir.0);
    let ctx = egui::Context::default();
    crate::panels::edit::reload_preview_only(&mut app, &ctx, id);
    let rows = app.session.activity.list();
    let row = rows.iter().find(|r| r.kind == "reload").unwrap_or_else(|| panic!("no reload row: {rows:?}"));
    assert_eq!(row.label, crate::reload::LABEL);
    assert!(row.cancellable);
    assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
    assert!(app.session.activity.list().iter().all(|r| r.kind != "reload"), "the row goes with the task");
    assert_eq!(app.session.catalog.photo(id).unwrap().preview_only, None);
    assert!(app.ui.toast.as_ref().is_some_and(|(t, _, _)| t == "This raw is now developed from its raw data"), "{:?}", app.ui.toast);
    // one undo step
    assert_eq!(app.session.undo.last().map(|u| u.label.as_str()), Some("Reload Changed Files"));
}

/// Reload from Disk on a selection returns at once and reads the files on a worker thread; `wait`
/// runs it in place; a cancelled reload changes nothing it didn't read.
#[test]
fn reload_of_a_selection_runs_in_the_background() {
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-reload-sel-{}", std::process::id())));
    let (mut app, id) = app_with_stale_photo(&dir.0);
    let ctx = egui::Context::default();
    app.session.selection = lightcraft_engine::Selection { ids: vec![id], active: Some(id) };
    let r = app.run("photo.reload", json!({})).unwrap();
    assert_eq!(r, json!({"background": true, "count": 1}));
    assert!(app.tasks.is_running(crate::reload::LABEL));
    assert!(app.session.activity.list().iter().any(|r| r.kind == "reload"));
    assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(10)));
    assert_eq!(app.session.catalog.photo(id).unwrap().preview_only, None);

    // agents can wait for the result: the engine's reply, nothing left running
    let r = app.run("photo.reload", json!({"ids": [id.0], "wait": true})).unwrap();
    assert!(r.get("reloaded").is_some(), "{r}");
    assert!(app.tasks.is_empty());

    // cancelled before any file is read: nothing changes
    let p = app.session.catalog.photo(id).unwrap().clone();
    app.session
        .catalog
        .apply(lightcraft_catalog::Op::SetContent {
            id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: p.content_hash.clone(),
            preview_only: Some("Cr3 files are not decoded yet".into()),
            preview_checked: "0.5.0".into(),
        })
        .unwrap();
    let items = lightcraft_engine::cmd::reload::plan(&app.session, &json!({"ids": [id.0]})).unwrap();
    let probe = lightcraft_engine::cmd::reload::file_probe(&app.session);
    let cancelled = std::sync::atomic::AtomicBool::new(true);
    let results = lightcraft_engine::cmd::reload::probe(probe.as_ref(), &items, &cancelled, &|_| {});
    assert!(lightcraft_engine::cmd::reload::apply(&mut app.session, items, results).unwrap().is_empty());
    assert!(app.session.catalog.photo(id).unwrap().preview_only.is_some());
}

/// With no photo selected the command is refused at once, as before (no task, no row).
#[test]
fn reload_with_nothing_selected_starts_nothing() {
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-reload-none-{}", std::process::id())));
    let (mut app, _) = app_with_stale_photo(&dir.0);
    app.session.selection = lightcraft_engine::Selection::default();
    let _ = app.run("photo.reload", json!({}));
    assert!(app.tasks.is_empty());
    assert!(app.session.activity.list().iter().all(|r| r.kind != "reload"));
}
