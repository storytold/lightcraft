//! Adjustable opacity for the area outside a crop.

use std::time::Duration;

use egui::{Color32, Pos2, Rect};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::{DEFAULT_CROP_MASK_OPACITY, UiState};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn crop_tool() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for (method, params) in [("ui.set", json!({"view": "detail"})), ("engine.execute", json!({"command": "panel.crop"}))] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    h
}

fn set_opacity(h: &mut Headless, opacity: Value) -> Value {
    h.request("engine.execute", json!({"command": "view.cropMaskOpacity", "params": {"opacity": opacity}}), T)
}

fn pixel(image: &egui::ColorImage, p: Pos2) -> Color32 {
    image.pixels[p.y.round() as usize * image.size[0] + p.x.round() as usize]
}

fn text_shapes(shape: &egui::Shape, out: &mut Vec<egui::epaint::TextShape>) {
    match shape {
        egui::Shape::Text(text) => out.push(text.clone()),
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| text_shapes(s, out)),
        _ => {}
    }
}

fn text_rect(h: &Headless, label: &str) -> Option<Rect> {
    let mut texts = Vec::new();
    h.view.painted_shapes().iter().for_each(|s| text_shapes(s, &mut texts));
    texts.into_iter().find(|t| t.galley.job.text == label).map(|t| Rect::from_min_size(t.pos, t.galley.size()))
}

fn click(h: &mut Headless, at: Pos2, button: &str) {
    let r = h.request("ui.click", json!({"x": at.x, "y": at.y, "button": button}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

#[test]
fn crop_mask_context_menu_sets_opacity() {
    let mut h = crop_tool();
    let img = h.app.image_rect.expect("the photo is on screen");
    click(&mut h, img.center(), "right");
    assert!(h.painted_text().iter().any(|t| t == "Crop Mask"), "the Crop Mask submenu is available on the photo");

    let menu = text_rect(&h, "Crop Mask").expect("the submenu label is painted");
    click(&mut h, menu.center(), "left");
    let labels = h.painted_text();
    for label in ["Off", "20%", "50%", "80%", "100%"] {
        assert!(labels.iter().any(|t| t == label), "missing {label}: {labels:?}");
    }

    let level = text_rect(&h, "80%").expect("80% is a menu choice");
    click(&mut h, level.center(), "left");
    assert_eq!(h.app.ui.crop_mask_opacity, 80);
}

#[test]
fn crop_mask_opacity_command_accepts_supported_values_and_rejects_others() {
    let mut h = crop_tool();
    for opacity in [0, 20, 50, 80, 100] {
        let r = set_opacity(&mut h, json!(opacity));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.crop_mask_opacity, opacity);
    }
    for bad in [json!(1), json!(101), json!("50"), json!(-1), json!({})] {
        let before = h.app.ui.crop_mask_opacity;
        let r = set_opacity(&mut h, bad);
        assert_eq!(r["ok"], false, "{r}");
        assert_eq!(h.app.ui.crop_mask_opacity, before, "a rejected choice must leave the setting unchanged");
    }
}

#[test]
fn crop_mask_opacity_is_saved_leniently() {
    let default = UiState::default();
    assert_eq!(default.crop_mask_opacity, DEFAULT_CROP_MASK_OPACITY);

    let mut saved = serde_json::to_value(&default).expect("UiState serializes");
    saved.as_object_mut().expect("UiState serializes as an object").remove("cropMaskOpacity");
    let restored: UiState = serde_json::from_value(saved).expect("older settings without the field still load");
    assert_eq!(restored.crop_mask_opacity, DEFAULT_CROP_MASK_OPACITY);

    let mut saved = serde_json::to_value(&default).expect("UiState serializes");
    saved["cropMaskOpacity"] = json!(37);
    let restored: UiState = serde_json::from_value(saved).expect("an unsupported saved level falls back");
    assert_eq!(restored.crop_mask_opacity, DEFAULT_CROP_MASK_OPACITY);

    let mut saved = serde_json::to_value(&default).expect("UiState serializes");
    saved["cropMaskOpacity"] = json!(80);
    let restored: UiState = serde_json::from_value(saved).expect("a supported level restores");
    assert_eq!(restored.crop_mask_opacity, 80);
}

#[test]
fn crop_mask_opacity_changes_the_outside_area_dimming() {
    let mut h = crop_tool();
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.2, 0.2, 0.8, 0.8]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let img = h.app.image_rect.expect("the photo is on screen");
    let outside = img.left_top() + egui::vec2(6.0, 6.0);
    let inside = img.center();

    set_opacity(&mut h, json!(0));
    h.settle(SETTLE);
    let baseline = h.paint();
    let baseline_outside = pixel(&baseline, outside);
    let baseline_inside = pixel(&baseline, inside);
    let luma = |c: Color32| u16::from(c.r()) + u16::from(c.g()) + u16::from(c.b());
    assert!(luma(baseline_outside) > 120, "the outside sample should contain visible photo detail: {baseline_outside:?}");

    let mut previous_luma = luma(baseline_outside);
    for opacity in [20, 50, 80, 100] {
        set_opacity(&mut h, json!(opacity));
        h.settle(SETTLE);
        let rendered = h.paint();
        let current_inside = pixel(&rendered, inside);
        let current_outside = pixel(&rendered, outside);
        let current_luma = luma(current_outside);
        assert_eq!(current_inside, baseline_inside, "opacity only changes pixels outside the crop");
        assert!(current_luma <= previous_luma, "dimming should increase with opacity: {opacity}% {current_outside:?}");
        if opacity > 0 {
            assert!(current_luma < luma(baseline_outside), "{opacity}% darkens outside pixels: {current_outside:?}");
        }
        previous_luma = current_luma;
    }
    assert_eq!(pixel(&h.paint(), outside), Color32::BLACK, "100% completely dims the outside area");
}
