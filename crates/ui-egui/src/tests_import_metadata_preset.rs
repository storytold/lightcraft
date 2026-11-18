//! Tests for applying and creating metadata presets directly in the Import dialog (issue #467).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn app() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(SETTLE);
    h
}

fn create_test_images(tag: &str, count: usize) -> lcc_testkit::TempDir {
    let dir = lcc_testkit::TempDir::new(&format!("lc-test-import-meta-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..count {
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[(i * 30) as u8, 10, 20, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join(format!("photo_{i}.png")), png).unwrap();
    }
    dir
}

#[test]
fn metadata_preset_field_always_visible_in_import_dialog() {
    let dir = create_test_images("visible", 1);
    let mut h = app();

    // Verify session starts with no metadata presets
    assert!(h.app.session.metadata_presets.is_empty());

    let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), T);
    assert_eq!(r["result"]["scanning"], true, "{r}");
    h.settle(SETTLE);
    h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));

    let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import dialog") };
    assert_eq!(opts.candidates.len(), 1);
    assert_eq!(opts.metadata_preset, "");

    // Check that combo:importMetadata is registered and present in UI widgets
    let widgets = h.request("ui.widgets", json!({"filter": "combo:importMetadata"}), T);
    assert!(widgets.to_string().contains("combo:importMetadata"), "combo:importMetadata must be visible even with 0 presets: {widgets}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_defaults_metadata_preset_is_preselected_in_dialog_and_applied() {
    let dir = create_test_images("preselected", 1);
    let mut h = app();

    // Create a metadata preset and set it as the import default
    h.request(
        "engine.execute",
        json!({
            "command": "metadata.savePreset",
            "params": {
                "name": "Studio Rights",
                "fields": {
                    "creator": "Jane Doe",
                    "copyright": "© 2026 Jane Doe"
                }
            }
        }),
        T,
    );
    h.request(
        "engine.execute",
        json!({
            "command": "library.preferences",
            "params": {
                "import": {
                    "metadataPreset": "Studio Rights"
                }
            }
        }),
        T,
    );

    let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), T);
    assert_eq!(r["result"]["scanning"], true, "{r}");
    h.settle(SETTLE);
    h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));

    let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import dialog") };
    assert_eq!(opts.metadata_preset, "Studio Rights", "default metadata preset from settings must be preselected");

    let confirm = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(confirm["ok"], true, "{confirm}");
    h.settle(SETTLE);
    h.step_until(SETTLE, |h| h.app.import.is_none());

    // Verify imported photo received the preset metadata
    let photo = h.app.session.catalog.photos().find(|p| p.file_name == "photo_0.png").expect("imported photo");
    assert_eq!(photo.meta.creator, "Jane Doe");
    assert_eq!(photo.meta.copyright, "© 2026 Jane Doe");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_new_metadata_preset_inline_in_import_dialog() {
    let dir = create_test_images("create", 1);
    let mut h = app();

    let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), T);
    assert_eq!(r["result"]["scanning"], true, "{r}");
    h.settle(SETTLE);
    h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));

    // Open the metadata dropdown and click "New Preset…"
    let open_combo = h.request("ui.clickWidget", json!({"id": "combo:importMetadata"}), T);
    assert_eq!(open_combo["ok"], true, "{open_combo}");
    h.settle(SETTLE);

    let click_new = h.request("ui.clickWidget", json!({"id": "button:importMetaNewPreset"}), T);
    assert_eq!(click_new["ok"], true, "{click_new}");
    h.settle(SETTLE);

    // Set inline creation fields in the dialog
    if let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog {
        assert!(opts.creating_meta_preset);
        opts.new_meta_name = "Field Work".into();
        opts.new_meta_creator = "Alex Smith".into();
        opts.new_meta_copyright = "© 2026 Alex Smith".into();
    } else {
        panic!("no import dialog");
    }
    h.settle(SETTLE);

    // Click Save Preset
    let save = h.request("ui.clickWidget", json!({"id": "button:importMetaPresetSave"}), T);
    assert_eq!(save["ok"], true, "{save}");
    h.settle(SETTLE);

    if let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog {
        assert_eq!(opts.metadata_preset, "Field Work");
        assert!(!opts.creating_meta_preset);
    } else {
        panic!("dialog was closed prematurely");
    }

    assert!(h.app.session.metadata_presets.iter().any(|m| m.name == "Field Work"), "preset was saved to session");

    let confirm = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(confirm["ok"], true, "{confirm}");
    h.settle(SETTLE);
    h.step_until(SETTLE, |h| h.app.import.is_none());

    // Verify imported photo received the newly created preset's metadata
    let photo = h.app.session.catalog.photos().find(|p| p.file_name == "photo_0.png").expect("imported photo");
    assert_eq!(photo.meta.creator, "Alex Smith");
    assert_eq!(photo.meta.copyright, "© 2026 Alex Smith");

    let _ = std::fs::remove_dir_all(&dir);
}
