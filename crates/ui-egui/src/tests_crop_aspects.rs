//! Headless tests of the Crop panel's aspect menu with the user's own ratios (issue #1854): saved
//! ratios listed by name, hidden built-ins left out, the Custom row saving a named ratio, and the
//! Edit List mode reordering, deleting and hiding.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn crop_panel() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "panel.crop"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

/// Ids of the widgets on screen whose id contains `filter`.
fn widgets(h: &mut Headless, filter: &str) -> Vec<String> {
    let r = h.request("ui.widgets", json!({"filter": filter}), T);
    r["result"].as_array().map(|a| a.iter().filter_map(|w| w["id"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn open_menu(h: &mut Headless) {
    click(h, "dropdown:cropAspect");
    assert!(!widgets(h, "cropAspectPreset:").is_empty(), "the aspect menu is open");
}

/// Replace the text of a field with `text`.
fn type_into(h: &mut Headless, field: &str, text: &str) {
    click(h, field);
    for key in ["a", "Backspace"] {
        let r = h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    let r = h.request("ui.text", json!({"text": text}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

fn custom_names(h: &Headless) -> Vec<String> {
    h.app.session.crop_aspects.custom.iter().map(|a| a.name.clone()).collect()
}

#[test]
fn the_aspect_button_names_a_saved_ratio() {
    let custom = [lightcraft_engine::cmd::crop_aspects::CustomAspect { name: "Instagram Portrait".into(), aspect: [1080.0, 1350.0] }];
    let label = |a| crate::panels::right::aspect_label(a, Some(1.5), &custom);
    assert_eq!(label(Some((400, 500))), "Instagram Portrait", "the user's name, in either orientation");
    assert_eq!(label(Some((500, 400))), "Instagram Portrait");
    assert_eq!(label(Some((100, 100))), "1 × 1");
    assert_eq!(label(Some((300, 200))), "Original");
    assert_eq!(label(None), "Free");
}

#[test]
fn a_saved_ratio_is_in_the_menu_and_locks_the_crop() {
    let mut h = crop_panel();
    exec(&mut h, "crop.saveAspect", json!({"name": "Instagram Portrait", "aspect": [4, 5]}));
    open_menu(&mut h);
    assert_eq!(widgets(&mut h, "cropAspectSaved:"), ["cropAspectSaved:0"]);
    click(&mut h, "cropAspectSaved:0");
    assert_eq!(h.app.crop_tool_aspect(), Some((400, 500)));
    assert!(widgets(&mut h, "cropAspectSaved:").is_empty(), "picking a ratio closes the menu");
}

#[test]
fn a_hidden_built_in_ratio_leaves_the_menu() {
    let mut h = crop_panel();
    exec(&mut h, "crop.hideAspect", json!({"aspect": "16x9"}));
    open_menu(&mut h);
    let shown = widgets(&mut h, "cropAspectPreset:");
    assert!(shown.iter().any(|w| w == "cropAspectPreset:1x1"), "{shown:?}");
    assert!(shown.iter().any(|w| w == "cropAspectPreset:free"), "{shown:?}");
    assert!(!shown.iter().any(|w| w == "cropAspectPreset:16x9"), "{shown:?}");
}

#[test]
fn the_custom_row_saves_a_named_ratio_and_uses_it() {
    let mut h = crop_panel();
    open_menu(&mut h);
    type_into(&mut h, "field:cropCustomW", "1080");
    type_into(&mut h, "field:cropCustomH", "1350");
    type_into(&mut h, "field:cropCustomName", "Instagram");
    click(&mut h, "button:cropCustomSave");
    h.settle(SETTLE);
    assert_eq!(custom_names(&h), ["Instagram"]);
    assert_eq!(h.app.session.crop_aspects.custom[0].aspect, [1080.0, 1350.0]);
    assert_eq!(h.app.crop_tool_aspect(), Some((108000, 135000)), "and the crop is locked to it");
    // without a name, Save does nothing
    open_menu(&mut h);
    type_into(&mut h, "field:cropCustomName", "");
    click(&mut h, "button:cropCustomSave");
    assert_eq!(custom_names(&h), ["Instagram"]);
}

#[test]
fn edit_list_reorders_deletes_and_hides_without_closing_the_menu() {
    let mut h = crop_panel();
    for (name, w, hgt) in [("A4", 210, 297), ("Print", 7, 5), ("Banner", 3, 1)] {
        exec(&mut h, "crop.saveAspect", json!({"name": name, "aspect": [w, hgt]}));
    }
    open_menu(&mut h);
    click(&mut h, "button:cropAspectEdit");
    click(&mut h, "button:cropAspectUp-1");
    assert_eq!(custom_names(&h), ["Print", "A4", "Banner"]);
    click(&mut h, "button:cropAspectDown-0");
    assert_eq!(custom_names(&h), ["A4", "Print", "Banner"]);
    click(&mut h, "button:cropAspectDelete-2");
    assert_eq!(custom_names(&h), ["A4", "Print"]);
    click(&mut h, "check:cropAspectShow:4x5");
    assert_eq!(h.app.session.crop_aspects.hidden, ["4x5"]);
    // Free and Original can't be hidden: no checkbox for them
    assert!(widgets(&mut h, "check:cropAspectShow:free").is_empty());
    assert!(widgets(&mut h, "check:cropAspectShow:original").is_empty());
    // the menu stayed open through all of it; Done leaves the edit mode
    click(&mut h, "button:cropAspectEdit");
    let shown = widgets(&mut h, "cropAspectPreset:");
    assert!(!shown.iter().any(|w| w == "cropAspectPreset:4x5"), "{shown:?}");
    assert_eq!(widgets(&mut h, "cropAspectSaved:").len(), 2);
}
