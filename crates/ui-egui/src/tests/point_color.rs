//! Headless tests of the Point Color block: the Edit panel's (the photo's samples) and the Masking
//! panel's (the selected mask's samples), each with its own eyedropper.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn open_detail() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 3200.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h
}

/// Edit panel with the Point Color flyout open.
fn global_open() -> Headless {
    let mut h = open_detail();
    h.app.ui.right = crate::state::RightPanel::Edit;
    if !h.app.ui.section_open("color") {
        h.app.ui.toggle_section("color");
    }
    if !h.app.ui.flyout_open("pointColor") {
        h.app.ui.toggle_flyout("pointColor");
    }
    h.step();
    h.step();
    h
}

/// Masking panel, a radial mask selected, its Point Color block open.
fn mask_open() -> Headless {
    let mut h = open_detail();
    exec(&mut h, "panel.masking", json!({}));
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    h.step();
    click(&mut h, "section:mask:pointColor");
    h.step();
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
}

fn has(h: &mut Headless, id: &str) -> bool {
    h.request("ui.widgets", json!({"filter": id}), T)["result"].as_array().is_some_and(|w| w.iter().any(|w| w["id"] == id))
}

fn click_photo(h: &mut Headless, x: f64, y: f64) {
    let r = h.request("ui.pointer", json!({"events": [{"kind": "down", "x": x, "y": y}, {"kind": "up", "x": x, "y": y}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

fn develop(h: &Headless) -> lightcraft_develop::DevelopSettings {
    let id = h.app.session.active().expect("active photo");
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn mask_samples(h: &Headless) -> Vec<lightcraft_develop::PointColor> {
    develop(h).masks.iter().find(|m| Some(m.id) == h.app.session.active_mask).map(|m| m.point_colors.clone()).expect("a selected mask")
}

fn drag_slider(h: &mut Headless, dx: f64) {
    let r = h.request("ui.dragWidget", json!({"id": "slider:pointColor.hueShift", "fx": 0.5, "fy": 0.5, "dx": dx, "steps": 8}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

#[test]
fn the_masks_point_color_block_samples_edits_and_deletes_the_masks_own_samples() {
    let mut h = mask_open();
    let all = h.request("ui.widgets", json!({}), T).to_string();
    assert!(all.contains("icon:maskPointColorPicker"), "the block shows for the selected mask: {all}");
    assert!(!has(&mut h, "slider:pointColor.hueShift"), "no sample, no sliders");
    // arm the eyedropper, click the photo: the mask gets the sample, the photo doesn't
    click(&mut h, "icon:maskPointColorPicker");
    assert_eq!(h.app.ui.tool, "maskPointColor");
    click_photo(&mut h, 0.3, 0.5);
    assert_eq!(mask_samples(&h).len(), 1, "the click sampled into the mask");
    assert!(develop(&h).point_colors.is_empty(), "the photo's own Point Color is untouched");
    assert!(h.app.ui.tool.is_empty(), "picking ends with the click");
    assert!(!h.app.ui.mask_overlay, "a mask point colour edit hides the overlay");
    // a slider sends mask.pointColor.set
    assert!(has(&mut h, "maskPointColor:0"), "a swatch per sample");
    drag_slider(&mut h, 60.0);
    assert!(mask_samples(&h)[0].hue_shift > 10.0, "hue shift written to the mask: {:?}", mask_samples(&h));
    assert_eq!(develop(&h).point_colors.len(), 0);
    // Escape disarms the eyedropper, a second click on its button too
    click(&mut h, "icon:maskPointColorPicker");
    assert_eq!(h.app.ui.tool, "maskPointColor");
    click(&mut h, "icon:maskPointColorPicker");
    assert!(h.app.ui.tool.is_empty());
    click(&mut h, "icon:maskPointColorPicker");
    exec(&mut h, "view.back", json!({}));
    assert!(h.app.ui.tool.is_empty(), "Escape disarms it");
    // Delete removes the selected swatch
    click(&mut h, "button:maskPointColorDelete");
    assert!(mask_samples(&h).is_empty());
}

#[test]
fn the_masks_eyedropper_stops_offering_a_ninth_sample_and_toasts_a_refusal() {
    let mut h = mask_open();
    for i in 0..8 {
        exec(&mut h, "mask.pointColor.pick", json!({"x": 0.1 + 0.05 * i as f64, "y": 0.5}));
    }
    h.step();
    h.step();
    // the button is disabled when full; arming it anyway and clicking the photo shows the engine's refusal
    h.app.ui.tool = "maskPointColor".into();
    h.step();
    click_photo(&mut h, 0.5, 0.5);
    assert_eq!(mask_samples(&h).len(), 8);
    assert!(h.app.ui.toast.as_ref().is_some_and(|(t, ..)| t.contains("point colours")), "{:?}", h.app.ui.toast);
    assert!(h.app.ui.tool.is_empty());
}

#[test]
fn the_edit_panels_point_color_still_edits_the_photo() {
    let mut h = global_open();
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    exec(&mut h, "panel.edit", json!({}));
    h.app.ui.right = crate::state::RightPanel::Edit;
    h.step();
    h.step();
    let all = h.request("ui.widgets", json!({}), T).to_string();
    assert!(all.contains("icon:pointColorPicker"), "{all}");
    click(&mut h, "icon:pointColorPicker");
    assert_eq!(h.app.ui.tool, "pointColor");
    click_photo(&mut h, 0.3, 0.5);
    assert_eq!(develop(&h).point_colors.len(), 1, "the photo got the sample");
    assert!(develop(&h).masks.iter().all(|m| m.point_colors.is_empty()), "no mask was touched");
    drag_slider(&mut h, 60.0);
    assert!(develop(&h).point_colors[0].hue_shift > 10.0, "develop.set wrote the photo's sample");
    assert!(develop(&h).masks.iter().all(|m| m.point_colors.is_empty()));
    click(&mut h, "button:pointColorDelete");
    assert!(develop(&h).point_colors.is_empty());
}

#[test]
fn the_masks_eyedropper_samples_in_the_full_screen_preview_instead_of_zooming() {
    let mut h = mask_open();
    h.app.run("view.fullScreenPreview", json!({})).unwrap();
    assert!(h.app.ui.fullscreen);
    h.app.ui.tool = "maskPointColor".into();
    h.step();
    h.step();
    click_photo(&mut h, 0.3, 0.5);
    assert_eq!(mask_samples(&h).len(), 1, "the click sampled into the mask");
    assert!(h.app.ui.tool.is_empty(), "picking ends with the click");
    assert!(matches!(h.app.ui.zoom, crate::state::Zoom::Fit), "the click did not zoom the photo");
}

#[test]
fn the_masks_eyedropper_is_disarmed_when_its_block_closes_or_no_mask_is_selected() {
    let mut h = mask_open();
    click(&mut h, "icon:maskPointColorPicker");
    assert_eq!(h.app.ui.tool, "maskPointColor");
    // closing the block disarms it
    click(&mut h, "section:mask:pointColor");
    h.step();
    assert!(h.app.ui.tool.is_empty(), "closing the Point Color block disarms the eyedropper");
    // armed with the block open, deselecting every mask disarms it too
    click(&mut h, "section:mask:pointColor");
    click(&mut h, "icon:maskPointColorPicker");
    assert_eq!(h.app.ui.tool, "maskPointColor");
    h.app.session.active_mask = None;
    h.step();
    h.step();
    assert!(h.app.ui.tool.is_empty(), "no selected mask, no eyedropper");
}
