//! Headless tests of the mask and Remove tools on the photo: the Colour / Luminance Range pickers, spot outlines and pins.

use std::time::Duration;

use lcc_develop::MaskShape;
use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn detail(panel: &str) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": panel}), T);
    assert_eq!(r["ok"], true, "{r}");
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn develop(h: &Headless) -> lcc_develop::DevelopSettings {
    let id = h.app.session.active().expect("active photo");
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
}

fn pointer(h: &mut Headless, events: serde_json::Value) {
    let r = h.request("ui.pointer", json!({"events": events}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

/// Pointer events with modifier `key` (`shift`, `alt`) held.
fn pointer_with(h: &mut Headless, events: serde_json::Value, key: &str) {
    let r = h.request("ui.pointer", json!({"events": events, key: true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

fn widgets(h: &Headless, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = h.app.widgets.iter().filter(|(id, _)| id.starts_with(prefix)).map(|(id, _)| id.clone()).collect();
    v.dedup();
    v
}
/// Colour Range: a click replaces, ⇧-click adds, a drag samples an area (one pin for it), ⌥-click
/// removes a pin, a pin dragged samples elsewhere; the panel shows the colours as swatches.
#[test]
fn colour_range_picks_add_remove_and_move() {
    let mut h = detail("panel.masking");
    click(&mut h, "maskNew:colorRange");
    // it arms like the other tools: no mask until the first click on the photo
    h.step();
    assert!(develop(&h).masks.is_empty(), "armed: no mask yet");
    assert_eq!(h.app.session.active_mask, None);
    pointer(&mut h, json!([{"kind": "down", "x": 0.3, "y": 0.3}, {"kind": "up", "x": 0.3, "y": 0.3}]));
    assert_eq!(h.app.ui.tool, "colorRange", "the picker stays on to add more");
    let samples = |h: &Headless| match develop(h).masks.first().and_then(|m| m.components.first()).map(|c| c.shape.clone()) {
        Some(MaskShape::ColorRange { samples, areas, .. }) => (samples.len(), lcc_develop::color_range_pins(&areas).len()),
        other => panic!("{other:?}"),
    };
    assert_eq!(samples(&h), (1, 1), "a click picks one colour, with its pin");
    pointer(&mut h, json!([{"kind": "down", "x": 0.7, "y": 0.6}, {"kind": "up", "x": 0.7, "y": 0.6}]));
    assert_eq!(samples(&h), (1, 1), "a second click replaces it");
    pointer_with(&mut h, json!([{"kind": "down", "x": 0.2, "y": 0.8}, {"kind": "up", "x": 0.2, "y": 0.8}]), "shift");
    assert_eq!(samples(&h), (2, 2), "⇧-click adds");
    // a drag over an area: up to three of its colours, one pin
    pointer_with(
        &mut h,
        json!([{"kind": "down", "x": 0.05, "y": 0.05}, {"kind": "drag", "x": 0.3, "y": 0.2},
               {"kind": "drag", "x": 0.6, "y": 0.45}, {"kind": "up", "x": 0.6, "y": 0.45}]),
        "shift",
    );
    let (n, pins) = samples(&h);
    assert_eq!(pins, 3, "the area has one pin");
    assert!((3..=5).contains(&n), "{n} samples");
    h.step();
    assert_eq!(widgets(&h, "colorPin:").len(), 3, "a pin per picked place on the photo");
    assert_eq!(widgets(&h, "colorSwatch:0:").len(), n, "a swatch per colour in the panel");
    // ⌥-click on the first pin removes it
    pointer_with(&mut h, json!([{"kind": "down", "x": 0.7, "y": 0.6}, {"kind": "up", "x": 0.7, "y": 0.6}]), "alt");
    assert_eq!(samples(&h).1, 2);
    // the command moves a pin and samples there again
    let r = exec(&mut h, "mask.colorPin", json!({"index": 0, "x": 0.4, "y": 0.4}));
    assert_eq!(r["pins"], 2, "{r}");
    // hostile arguments are errors, never a panic
    assert!(h.app.run("mask.colorPin", json!({"index": 99, "remove": true})).is_err());
    assert!(h.app.run("mask.sampleColor", json!({"rect": [0.1, 0.1]})).is_err());
    assert!(h.app.run("mask.sampleColor", json!({"x": f64::MAX, "y": 0.5})).is_err());
}

/// Luminance Range: a click picks a band round the tone there, a drag the tones of the area, ⇧
/// widens what is there.
#[test]
fn luminance_range_picks_from_the_photo() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "luminanceRange", "lo": 0.0, "hi": 1.0}));
    h.app.ui.tool = "lumRange".into();
    h.step();
    let range = |h: &Headless| match develop(h).masks[0].components[0].shape {
        MaskShape::LuminanceRange { lo, hi, .. } => (lo, hi),
        ref s => panic!("{s:?}"),
    };
    pointer(&mut h, json!([{"kind": "down", "x": 0.5, "y": 0.5}, {"kind": "up", "x": 0.5, "y": 0.5}]));
    let (lo, hi) = range(&h);
    assert!(hi - lo > 0.1 && hi - lo < 0.2, "a band round the tone: {lo}..{hi}");
    pointer(
        &mut h,
        json!([{"kind": "down", "x": 0.05, "y": 0.05}, {"kind": "drag", "x": 0.5, "y": 0.5}, {"kind": "drag", "x": 0.95, "y": 0.95}, {"kind": "up", "x": 0.95, "y": 0.95}]),
    );
    let (lo2, hi2) = range(&h);
    assert!(hi2 - lo2 > hi - lo, "the whole photo has more tones than a point: {lo2}..{hi2}");
    let r = exec(&mut h, "mask.sampleLuminance", json!({"x": 0.5, "y": 0.5, "add": true}));
    assert!(r["lo"].as_f64().unwrap() <= lo2 + 1e-9 && r["hi"].as_f64().unwrap() >= hi2 - 1e-9, "⇧ only widens: {r}");
}

/// A Remove spot under the pointer shows its outline and both pins, so either can be moved.
#[test]
fn a_hovered_spot_shows_both_pins() {
    let mut h = detail("panel.remove");
    let path: Vec<[f64; 2]> = (0..60).map(|i| [0.2 + i as f64 * 0.005, 0.5]).collect();
    exec(&mut h, "spot.add", json!({"mode": "heal", "points": path, "size": 0.02}));
    exec(&mut h, "spot.add", json!({"mode": "heal", "points": [[0.8, 0.2]], "size": 0.02}));
    // the other spot is selected: only its source shows until the pointer is over the first
    exec(&mut h, "spot.select", json!({"index": 1}));
    pointer(&mut h, json!([{"kind": "move", "x": 0.8, "y": 0.2}]));
    assert!(!widgets(&h, "spotSource:").contains(&"spotSource:0".to_string()));
    pointer(&mut h, json!([{"kind": "move", "x": 0.3, "y": 0.5}]));
    assert!(widgets(&h, "spotSource:").contains(&"spotSource:0".to_string()), "the hovered spot's source pin, to move it");
}

/// Object, with SAM 2.1 installed (`<PREFIX>_SAM2_DIR` = its folder): armed until the first
/// click on the photo, which makes the mask and selects what was clicked.
#[test]
fn object_is_armed_until_the_first_click_and_selects_there() {
    let Some(dir) = lcc_brand::env_os("SAM2_DIR").map(std::path::PathBuf::from) else {
        lcc_testkit::skip!(Model, "set {} to SAM 2.1's folder", lcc_brand::env_var("SAM2_DIR"));
        return;
    };
    if !lcc_engine::segment::Segmenter::AVAILABLE {
        lcc_testkit::skip!(Model, "a build without AI masks (--features lcc-engine/sam)");
        return;
    }
    let mut h = detail("panel.masking");
    h.app.session.segmenter.models_root = dir.parent().map(std::path::PathBuf::from);
    h.app.session.segmenter.dir = h.app.session.segmenter.models_root.as_ref().map(|r| r.join("sam3"));
    exec(&mut h, "ai.models.setObjectModel", json!({"model": "sam2"}));
    click(&mut h, "maskNew:object");
    h.step();
    assert!(develop(&h).masks.is_empty(), "armed: no mask yet");
    assert_eq!(h.app.ui.tool, "object");
    pointer(&mut h, json!([{"kind": "down", "x": 0.5, "y": 0.5}, {"kind": "up", "x": 0.5, "y": 0.5}]));
    let t = std::time::Instant::now();
    loop {
        h.step();
        let d = develop(&h);
        if let Some(MaskShape::Object { seg: Some(_), hint, .. }) = d.masks.first().and_then(|m| m.components.first()).map(|c| &c.shape) {
            assert_eq!(hint.len(), 1, "the click is the object's");
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(180), "no selection came back: {:?}", d.masks);
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(develop(&h).masks.len(), 1);
}
