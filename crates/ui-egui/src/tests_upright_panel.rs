//! Headless tests of Upright's controls in the Transform section (issue #1274): the Guided Upright
//! tool has its own icon, the redundant "Upright" heading is gone and the active mode is named
//! instead, and switching modes never moves the mode buttons (what read as "Upright" flashing).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// Develop with Upright's controls on screen: the Crop panel's Geometry, or (Classic) the Edit
/// panel's Transform section.
fn transform() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.open_sections = vec!["geometry".to_string()];
    h.settle(SETTLE);
    if rect(&h, "button:upright-off").is_none() {
        let r = h.request("engine.execute", json!({"command": "panel.crop"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    assert!(rect(&h, "button:upright-off").is_some(), "Upright's modes are on screen");
    h
}

fn upright(h: &Headless) -> lcc_develop::Upright {
    let id = h.app.session.active().expect("a photo");
    h.app.session.catalog.photo(id).expect("the photo").develop.geometry.upright
}

fn click(h: &mut Headless, id: &str) {
    let c = rect(h, id).unwrap_or_else(|| panic!("{id} is on screen")).center();
    h.time += 1.0;
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

#[test]
fn guided_icon_turns_guided_upright_on_and_the_mode_is_named() {
    let mut h = transform();
    assert!(rect(&h, "label:upright").is_none(), "no separate Upright heading");
    assert!(rect(&h, "label:uprightMode").is_some(), "the mode in use is named");
    assert!(rect(&h, "icon:uprightGuidedTool").is_some(), "Guided Upright has its icon");
    click(&mut h, "icon:uprightGuidedTool");
    assert_eq!(upright(&h), lcc_develop::Upright::Guided, "the icon switches Upright to Guided");
    assert_eq!(h.app.ui.tool, "guidedUpright", "ready to draw guides");
    click(&mut h, "icon:uprightGuidedTool");
    assert!(h.app.ui.tool.is_empty(), "a second click puts the tool down");
    assert_eq!(upright(&h), lcc_develop::Upright::Guided, "and keeps the mode");
}

#[test]
fn switching_modes_never_moves_the_mode_buttons() {
    let mut h = transform();
    let before = rect(&h, "button:upright-full").expect("Full");
    for mode in ["upright-level", "upright-off", "upright-guided", "upright-off"] {
        click(&mut h, &format!("button:{mode}"));
        let now = rect(&h, "button:upright-full").expect("Full");
        assert!((now.top() - before.top()).abs() < 0.5, "after {mode} the modes moved: {before:?} → {now:?}");
    }
}
