//! The import review's "Batch edits" section: the recipe in the dialog is what the import sets on
//! the photos it adds, "Save as Preset" needs a name and a recipe, and cancelling the review
//! changes neither the catalog nor the presets.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use crate::dialogs::import::ImportDialog;
use crate::headless::Headless;
use crate::state::{Dialog, ViewMode};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A folder of two small PNGs.
fn folder(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-import-recipe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..2u8 {
        let data: Vec<[u8; 4]> = (0..40 * 30).map(|k| [(k % 40 * 6) as u8, i * 90, 7, 255]).collect();
        let img = lightcraft_raster::Rgba8 { width: 40, height: 30, data };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join(format!("img{i}.png")), png).unwrap();
    }
    dir
}

/// The app with an empty library and the import review open on a folder of two photos.
fn review(tag: &str) -> (Headless, PathBuf) {
    let dir = folder(tag);
    let app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 1400.0], 1.0);
    h.app.ui.view = ViewMode::PhotoGrid;
    let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), T);
    assert_eq!(r["result"]["scanning"], true, "{r}");
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the review opens");
    h.settle(SETTLE);
    (h, dir)
}

fn dialog(h: &mut Headless) -> &mut ImportDialog {
    match &mut h.app.ui.dialog {
        Some(Dialog::Import { opts }) => opts,
        other => panic!("no import review: {other:?}"),
    }
}

/// Where a widget is and where the review's window is: `[x, y, w, h]` each.
fn rects(h: &mut Headless, id: &str) -> ([f64; 4], [f64; 4]) {
    let r = h.request("ui.widgets", json!({}), T);
    let all = r["result"].as_array().expect("widgets");
    let find = |name: &str| -> [f64; 4] {
        let w = all.iter().find(|w| w["id"] == name).unwrap_or_else(|| panic!("no widget {name}"));
        [0usize, 1, 2, 3].map(|i| w["rect"][i].as_f64().unwrap())
    };
    (find(id), find("dialog:window"))
}

/// Click a widget of the review, scrolling the review first when the widget is below its window
/// (the review's content scrolls; the Batch edits section is at the bottom of it).
fn click(h: &mut Headless, id: &str) {
    for _ in 0..40 {
        let (w, win) = rects(h, id);
        if w[1] + w[3] <= win[1] + win[3] - 30.0 {
            break;
        }
        h.request("ui.move", json!({"x": win[0] + win[2] / 2.0, "y": win[1] + win[3] / 2.0}), T);
        h.request("ui.scroll", json!({"dy": -400.0}), T);
        h.settle(SETTLE);
    }
    let (w, win) = rects(h, id);
    assert!(w[1] >= win[1] && w[1] + w[3] <= win[1] + win[3], "{id} {w:?} is inside the window {win:?}");
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.clone().map(|t| t.0).unwrap_or_default()
}

/// Wait for the import the review started to finish.
fn finish_import(h: &mut Headless) {
    let t0 = std::time::Instant::now();
    while h.app.import.is_some() && t0.elapsed() < Duration::from_secs(60) {
        h.step();
    }
    assert!(h.app.import.is_none(), "the import finished");
    h.settle(SETTLE);
}

/// "Save as Preset" saves nothing without a name or without a recipe, and with both saves exactly
/// the recipe as a preset, leaving the review's own choices (its preset, the photos) as they were.
#[test]
fn save_as_preset_needs_a_name_and_a_recipe() {
    let (mut h, dir) = review("save");
    let presets0 = h.app.session.presets.len();
    click(&mut h, "button:importRecipeToggle");
    assert!(
        h.request("ui.widgets", json!({"filter": "importRecipeSave"}), T)["result"].as_array().is_some_and(|w| !w.is_empty()),
        "the section is open"
    );

    dialog(&mut h).recipe_name = "Warm trip".into();
    click(&mut h, "button:importRecipeSave");
    assert_eq!(h.app.session.presets.len(), presets0, "a name alone saves nothing");
    assert_eq!(toast(&h), "", "the button is off: pressing it does not even ask the engine (which would refuse with a message)");

    let d = dialog(&mut h);
    d.recipe_name = "  ".into();
    d.recipe.insert("light.exposure".into(), 0.5);
    click(&mut h, "button:importRecipeSave");
    assert_eq!(h.app.session.presets.len(), presets0, "a recipe alone saves nothing");
    assert_eq!(toast(&h), "", "the button is off without a name too");

    dialog(&mut h).recipe_name = " Warm trip ".into();
    click(&mut h, "button:importRecipeSave");
    assert_eq!(h.app.session.presets.len(), presets0 + 1, "{}", toast(&h));
    let saved = h.app.session.presets.iter().find(|p| p.name == "Warm trip").expect("the preset is named as typed, trimmed");
    assert_eq!(saved.settings, json!({"light": {"exposure": 0.5}}), "only what the recipe names");
    assert_eq!(toast(&h), "Saved as a preset");
    assert_eq!(dialog(&mut h).preset, "", "the review's own preset choice is not replaced");
    assert_eq!(h.app.session.catalog.len(), 0, "nothing was imported");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Cancelling the review with a recipe in it adds no photo and saves no preset.
#[test]
fn cancelling_the_review_discards_the_recipe() {
    let (mut h, dir) = review("cancel");
    let presets0 = h.app.session.presets.len();
    let undo0 = h.app.session.undo.len();
    let d = dialog(&mut h);
    d.recipe.insert("light.exposure".into(), 0.5);
    d.recipe_name = "Never saved".into();
    h.settle(SETTLE);
    let r = h.request("ui.dialog.cancel", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.dialog.is_none() && h.app.import.is_none());
    assert_eq!((h.app.session.catalog.len(), h.app.session.presets.len(), h.app.session.undo.len()), (0, presets0, undo0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Confirming the review with a recipe imports the photos with it: each has the values and the
/// Import adjustments step, and the import is still one undo step.
#[test]
fn confirming_with_a_recipe_sets_it_on_the_new_photos() {
    let (mut h, dir) = review("confirm");
    let undo0 = h.app.session.undo.len();
    let d = dialog(&mut h);
    d.recipe.insert("light.exposure".into(), 0.5);
    d.recipe.insert("color.vibrance".into(), 20.0);
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    finish_import(&mut h);
    assert_eq!(h.app.session.catalog.len(), 2);
    for p in h.app.session.catalog.photos() {
        assert_eq!((p.develop.light.exposure, p.develop.color.vibrance), (0.5, 20.0), "{}", p.file_name);
        assert!(p.history.iter().any(|s| s.label == "Import adjustments"), "{}", p.file_name);
    }
    assert_eq!(h.app.session.undo.len(), undo0 + 1, "one undo step");
    h.request("engine.execute", json!({"command": "edit.undo"}), T);
    assert_eq!(h.app.session.catalog.len(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Confirming without a recipe adds no Import adjustments step.
#[test]
fn confirming_without_a_recipe_adds_no_adjustment_step() {
    let (mut h, dir) = review("plain");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    finish_import(&mut h);
    assert_eq!(h.app.session.catalog.len(), 2);
    assert!(h.app.session.catalog.photos().all(|p| !p.history.iter().any(|s| s.label == "Import adjustments")));
    let _ = std::fs::remove_dir_all(&dir);
}
