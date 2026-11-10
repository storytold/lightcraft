//! Photo ▸ Enhance ▸ Super Resolution in the desktop app: a cancellable row in the activity stack.

use std::path::PathBuf;
use std::time::Duration;

use lightcraft_engine::activity::Unit;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

// generous: the job runs on a worker thread that a busy machine (a full workspace test run) slows a lot
const T: Duration = Duration::from_secs(60);

/// A library with one PNG, selected, and a models folder; with `upscaler`, an `.onnx` the settings name as the
/// upscaler in use.
fn setup(tag: &str, upscaler: Option<&[u8]>) -> (Headless, PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-ui-enhance-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let models = dir.join("models");
    std::fs::create_dir_all(&models).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(400, 300, |x, y| [(x % 256) as u8, (y % 256) as u8, 120, 255]);
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    let photo = dir.join("photo.png");
    std::fs::write(&photo, lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
    if let Some(onnx) = upscaler {
        let home = models.join("stand-in-upscaler");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("model.onnx"), onnx).unwrap();
        let manifest = json!({"id": "stand-in-upscaler", "name": "Stand-in", "version": "1", "domain": "rgbToRgb4x", "tile": 64, "overlap": 16, "gain": {"kind": "none"}});
        std::fs::write(home.join("denoise-model.json"), manifest.to_string()).unwrap();
        std::fs::write(models.join("settings.json"), json!({"upscaleModel": "stand-in-upscaler"}).to_string()).unwrap();
    }
    let mut session = lightcraft_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": [photo.to_string_lossy()]})).unwrap();
    session.execute("library.select", &json!({"ids": r["imported"]})).unwrap();
    session.set_denoise_models_dir(Some(models));
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    (h, dir)
}

#[test]
fn super_resolution_shows_a_row_runs_once_and_cancels() {
    // a stand-in `.onnx`: the job is cancelled before it would be read
    let (mut h, dir) = setup("row", Some(b"a stand-in"));
    let before = h.app.session.catalog.len();
    crate::enhance::start(&mut h.app, 2).unwrap();
    // the row is up before the first frame: a job that ends at once is finished (and its row removed) only by a frame
    let rows = h.app.session.activity.list();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].unit), ("enhance", "Super resolution", Unit::Percent));
    assert!(rows[0].cancellable);
    assert_eq!(rows[0].total, 1000);
    let e = crate::enhance::start(&mut h.app, 1).unwrap_err();
    assert!(e.contains("already running"), "{e}");
    // its result goes into this library: no switch while it runs
    let e = crate::panels::settings::can_switch_library(&h.app).unwrap_err();
    assert!(e.contains("Super Resolution"), "{e}");

    h.app.session.activity.cancel(rows[0].id).unwrap();
    assert!(h.step_until(T, |h| h.app.enhance.task.is_none()));
    assert_eq!(h.app.session.catalog.len(), before, "no photo added");
    assert!(h.app.session.activity.list().is_empty());
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("Super resolution cancelled"), "{toast}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn without_an_upscaler_nothing_starts() {
    let (mut h, dir) = setup("none", None);
    let e = crate::enhance::start(&mut h.app, 2).unwrap_err();
    assert!(e.contains("no upscaler is installed"), "{e}");
    assert!(h.app.enhance.task.is_none() && h.app.session.activity.list().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_finished_super_resolution_adds_the_photo_and_says_so() {
    // a small upscaler of random weights, shaped like the real one: it runs, its picture doesn't matter
    let (mut h, dir) = setup("done", Some(&lightcraft_denoise::synthetic::upscaler_onnx(4, 0, 4, 7)));
    let before = h.app.session.catalog.len();
    crate::enhance::start(&mut h.app, 2).unwrap();
    assert!(h.step_until(T, |h| h.app.enhance.task.is_none()));
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("Super resolution added: 800 × 600"), "{toast}");
    assert_eq!(h.app.session.catalog.len(), before + 1);
    assert!(h.app.session.activity.list().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}
