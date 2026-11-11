//! Issue #3921 in the app: Convert with Adobe DNG Converter runs in the background with an activity row, and the
//! Preview only notice follows it (ready, converting, queued, failed). A stand-in program fails the conversion, so no
//! DNG has to be made here; the engine's tests cover a conversion that succeeds. (Shell scripts: Unix only.)
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_engine::dng_converter as dc;
use serde_json::json;

use super::Status;
use crate::LightcraftApp;

/// The converter's settings and cache folder are process-wide: one test at a time changes them.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        dc::set_settings(dc::Settings::default());
        dc::set_cache_dir(None);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An app with two photos whose files (PNGs, named as NEFs) the library shows as High Efficiency NEFs, and a converter
/// that fails after a moment.
fn app_with_he_photos(dir: &Path) -> (LightcraftApp, PhotoId, PhotoId) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("files")).unwrap();
    let converter = dir.join("convert.sh");
    std::fs::write(&converter, "#!/bin/sh\nsleep 0.3\nexit 3\n").unwrap();
    std::fs::set_permissions(&converter, std::fs::Permissions::from_mode(0o755)).unwrap();
    dc::set_cache_dir(Some(dir.join("cache")));
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("lib"), false).unwrap();
    let mut ids = Vec::new();
    for name in ["DSC_0001.NEF", "DSC_0002.NEF"] {
        let file = dir.join("files").join(name);
        // (different pixels: different content hashes, so neither is a duplicate of the other)
        let k = ids.len() as u8 * 40;
        let img = lightcraft_raster::Rgba8::from_fn(16, 12, move |x, y| [(x * 9) as u8, (y * 11) as u8, k, 255]);
        std::fs::write(&file, lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
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
                preview_only: Some("Nikon High Efficiency NEF (test)".into()),
                preview_checked: lightcraft_catalog::APP_VERSION.into(),
            })
            .unwrap();
        ids.push(id);
    }
    let mut app = LightcraftApp::new(session, Default::default());
    // as Settings ▸ Performance would set it (the app applies its own settings to the engine)
    app.ui.settings.dng_converter_path = converter.to_string_lossy().to_string();
    dc::set_settings(dc::Settings { path: Some(converter), ..Default::default() });
    (app, ids[0], ids[1])
}

#[test]
fn a_conversion_runs_in_the_background_and_the_notice_follows_it() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-dngconv-{}", std::process::id())));
    let (mut app, a, b) = app_with_he_photos(&dir.0);
    let ctx = egui::Context::default();
    assert_eq!(super::status(&app, a), Some(Status::Ready), "the Convert button shows");

    super::convert(&mut app, &ctx, a);
    // one row in the activity stack, cancellable, while it runs
    let rows = app.session.activity.list();
    let row = rows.iter().find(|r| r.kind == "dngConvert").unwrap_or_else(|| panic!("no dngConvert row: {rows:?}"));
    assert!(row.cancellable);
    assert_eq!(super::status(&app, a), Some(Status::Converting), "no Convert button while it runs");
    // a second photo waits for the first batch
    super::convert(&mut app, &ctx, b);
    assert_eq!(super::status(&app, b), Some(Status::Queued));
    assert_eq!(app.session.activity.list().iter().filter(|r| r.kind == "dngConvert").count(), 1);

    // the first fails: the notice says why and offers Convert again; the queued one starts
    assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(20)));
    assert!(matches!(super::status(&app, a), Some(Status::Failed(e)) if e.contains("failed")), "{:?}", super::status(&app, a));
    assert!(matches!(super::status(&app, b), Some(Status::Failed(_))), "{:?}", super::status(&app, b));
    assert!(app.session.activity.list().iter().all(|r| r.kind != "dngConvert"), "the row goes with the task");
    assert!(app.session.catalog.photo(a).unwrap().preview_only.is_some(), "still shown from its preview");
}

/// Automatic conversion is off by default, and runs once a session for a photo when it is on.
#[test]
fn automatic_conversion_is_opt_in_and_runs_once() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-dngconv-auto-{}", std::process::id())));
    let (mut app, a, _) = app_with_he_photos(&dir.0);
    let ctx = egui::Context::default();
    super::auto_convert(&mut app, &ctx, a);
    assert!(app.tasks.is_empty(), "off by default");
    let path = dc::settings().path;
    app.ui.settings.dng_converter_auto = true;
    dc::set_settings(dc::Settings { auto: true, path, ..Default::default() });
    // selected in the grid with the Edit panel open: not converted
    for view in [crate::state::ViewMode::PhotoGrid, crate::state::ViewMode::SquareGrid] {
        app.ui.view = view;
        super::auto_convert(&mut app, &ctx, a);
        assert!(app.tasks.is_empty(), "not converted from the grid ({view:?})");
    }
    app.ui.view = crate::state::ViewMode::Detail;
    super::auto_convert(&mut app, &ctx, a);
    assert!(app.session.activity.list().iter().any(|r| r.kind == "dngConvert"));
    assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(20)));
    super::auto_convert(&mut app, &ctx, a);
    assert!(app.tasks.is_empty(), "a failed automatic conversion isn't retried on every frame");
}

/// `photo.convertHighEfficiency` for photos that already convert in the background answers at once: they are not
/// handed to the engine, which would convert them again on the UI thread.
#[test]
fn a_command_for_photos_already_converting_is_answered_in_the_background() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = Dir(std::env::temp_dir().join(format!("lc-ui-dngconv-running-{}", std::process::id())));
    let (mut app, a, _) = app_with_he_photos(&dir.0);
    let ctx = egui::Context::default();
    super::convert(&mut app, &ctx, a);
    assert_eq!(super::status(&app, a), Some(Status::Converting));
    let r = super::command(&mut app, &json!({"ids": [a.0]}));
    assert_eq!(r, Some(Ok(json!({"background": true, "count": 0}))));
    assert_eq!(app.session.activity.list().iter().filter(|r| r.kind == "dngConvert").count(), 1, "no second conversion");
    assert!(crate::tasks::wait(&mut app, &ctx, Duration::from_secs(20)));
}
