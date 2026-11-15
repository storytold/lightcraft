//! Ported from OneSlips 1138a5edd: Add / Subtract / Intersect offer every kind of mask Create New Mask does, in the
//! same order (Background, People, Remembered and Color were missing), and a People or Remembered pick from there
//! combines with the selected mask.

use std::time::Duration;

use lcc_develop::{MaskOp, MaskShape};
use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn masking() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "moduleBar": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "panel.masking"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn develop(h: &Headless) -> lcc_develop::DevelopSettings {
    (*h.app.session.develop_of(h.app.session.active().unwrap()).unwrap_or_default()).clone()
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
}

fn widgets(h: &Headless, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = h.app.widgets.iter().filter(|(id, _)| id.starts_with(prefix)).map(|(id, _)| id.replace(prefix, "")).collect();
    v.dedup();
    v
}

#[test]
fn add_subtract_and_intersect_offer_every_mask_kind() {
    let mut h = masking();
    let tiles: Vec<String> = widgets(&h, "maskNew:");
    assert!(tiles.len() > 3, "{tiles:?}");
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5]}));
    h.step();
    for (button, op) in [("button:maskAddComp", "add"), ("button:maskSubComp", "subtract"), ("button:maskIntComp", "intersect")] {
        click(&mut h, button);
        h.step();
        let menu = widgets(&h, &format!("maskComp:{op}:"));
        // (painting only adds or erases)
        let want: Vec<String> = tiles.iter().filter(|k| op != "intersect" || *k != "brush").cloned().collect();
        assert_eq!(menu, want, "{op}");
        let r = h.request("ui.key", json!({"key": "Escape"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }
    // the user's case: a radial, minus the background
    let before = develop(&h).masks[0].components.len();
    click(&mut h, "button:maskSubComp");
    h.step();
    click(&mut h, "maskComp:subtract:background");
    let m = &develop(&h).masks[0];
    assert_eq!(m.components.len(), before + 1);
    assert!(matches!(m.components.last().map(|c| (&c.shape, c.op)), Some((MaskShape::Background, MaskOp::Subtract))));
    // Remembered from Add opens its picker for the selected mask; the Remembered tile (a new mask) forgets the op
    click(&mut h, "button:maskAddComp");
    h.step();
    click(&mut h, "maskComp:add:concept");
    h.step();
    assert!(h.app.ui.concept_pick);
    assert_eq!(h.app.mask_pick_op.as_deref(), Some("add"));
    click(&mut h, "maskNew:concept");
    h.step();
    assert!(!h.app.ui.concept_pick && h.app.mask_pick_op.is_none());
}

/// Issue #1144: Create New Mask offers Depth with the other tiles (it was only in the Tools menu), and the tile does
/// what the menu item does: a Depth Range mask with the depth model, else no mask and the AI Models settings.
#[test]
fn create_new_mask_offers_a_depth_tile() {
    let mut h = masking();
    assert!(widgets(&h, "maskNew:").iter().any(|k| k == "depthRange"), "a Depth tile in Create New Mask");
    let before = develop(&h).masks.len();
    click(&mut h, "maskNew:depthRange");
    h.step();
    if h.app.session.segmenter.open_model_installed("depth") {
        assert_eq!(develop(&h).masks.len(), before + 1);
        assert!(matches!(develop(&h).masks.last().map(|m| &m.components[0].shape), Some(MaskShape::DepthRange { .. })));
    } else {
        assert_eq!(develop(&h).masks.len(), before, "no mask without the depth model");
    }
}

/// Tools ▸ Create New Mask ▸ Select People / Color Range / Luminance Range do what the Masking panel's tiles do.
#[test]
fn create_new_mask_menu_items_start_the_tiles_tools() {
    let mut h = masking();
    exec(&mut h, "panel.remove", json!({}));
    exec(&mut h, "tool.people", json!({}));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Masking);
    assert!(h.app.ui.people_pick.is_some(), "the People picker is open");
    exec(&mut h, "tool.colorRange", json!({}));
    assert!(h.app.ui.people_pick.is_none(), "one picker or tool at a time");
    assert_eq!((h.app.ui.tool.as_str(), h.app.ui.mask_new.as_ref().map(|(_, k)| k.as_str())), ("colorRange", Some("colorRange")));
    let before = develop(&h).masks.len();
    exec(&mut h, "tool.luminanceRange", json!({}));
    let d = develop(&h);
    assert_eq!(d.masks.len(), before + 1);
    assert!(matches!(d.masks.last().map(|m| &m.components[0].shape), Some(MaskShape::LuminanceRange { .. })));
    // Depth Range: without the depth model no empty mask is left behind (the AI Models settings
    // offer the download instead)
    let before = develop(&h).masks.len();
    exec(&mut h, "tool.depthRange", json!({}));
    h.step();
    if h.app.session.segmenter.open_model_installed("depth") {
        lcc_testkit::skip!(Model, "the depth model is installed here");
    } else {
        assert_eq!(develop(&h).masks.len(), before, "no mask without the depth model");
    }
    h.app.ui.dialog = None;
    // Tools ▸ Point Color Selector opens Color Mixer ▸ Point Color with its picker armed
    exec(&mut h, "tool.pointColor", json!({}));
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit);
    assert!(h.app.ui.flyout_open("pointColor"));
    assert_eq!(h.app.ui.tool, "pointColor");
}

/// Remove Options ▸ Use Generative AI: with a service set up it switches the Remove tool to Generative and back; without
/// one it opens the setup and leaves the tool alone.
#[test]
fn use_generative_ai_toggles_the_remove_tool_or_opens_the_setup() {
    let mut h = masking();
    h.app.ui.genfill.providers = Some((std::time::Instant::now(), vec![]));
    let r = exec(&mut h, "remove.useGenerative", json!({}));
    assert_eq!(r["setup"], true, "{r}");
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "genfill"));
    assert_ne!(h.app.ui.tool, "generate");
    h.app.ui.dialog = None;
    h.app.ui.genfill.providers = Some((std::time::Instant::now(), vec![json!({"id": "x", "configured": true})]));
    exec(&mut h, "remove.useGenerative", json!({}));
    assert_eq!((h.app.ui.right, h.app.ui.tool.as_str()), (crate::state::RightPanel::Remove, "generate"));
    assert_eq!(crate::menubar::checked(&h.app, "remove.useGenerative"), Some(true));
    exec(&mut h, "remove.useGenerative", json!({}));
    assert_eq!(h.app.ui.tool, "remove");
    assert_eq!(crate::menubar::checked(&h.app, "remove.useGenerative"), Some(false));
}

/// Remove Options ▸ Detect Objects is a checked toggle that opens the Remove tool; without an Object model a stroke
/// painted while it is on is an error, never a spot or a mask.
#[test]
fn detect_objects_toggles_and_a_stroke_without_a_model_adds_nothing() {
    let mut h = masking();
    assert_eq!(crate::menubar::checked(&h.app, "remove.detectObjects"), Some(false));
    exec(&mut h, "remove.detectObjects", json!({}));
    assert_eq!(crate::menubar::checked(&h.app, "remove.detectObjects"), Some(true));
    assert_eq!((h.app.ui.right, h.app.ui.tool.as_str()), (crate::state::RightPanel::Remove, "remove"));
    let id = h.app.session.active().expect("photo");
    let counts = |h: &Headless| h.app.session.develop_of(id).map_or((0, 0), |d| (d.masks.len(), d.spots.len()));
    let before = counts(&h);
    if lcc_engine::segment::Segmenter::AVAILABLE {
        assert!(h.app.run("spot.detectObject", json!({"points": [[0.4, 0.4], [0.5, 0.5]]})).is_err());
    }
    assert_eq!(counts(&h).1, before.1, "no spot without the model");
    exec(&mut h, "remove.detectObjects", json!({"on": false}));
    assert_eq!(crate::menubar::checked(&h.app, "remove.detectObjects"), Some(false));
}
