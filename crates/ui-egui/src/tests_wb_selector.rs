//! Headless tests of the White Balance Selector's button.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The White Balance Selector button in the Edit panel does what W does: from Compare it opens
/// the active photo in Detail with the selector armed (it used to arm a tool Compare ignores).
/// Clicking it again disarms it.
#[test]
fn wb_selector_button_in_compare_opens_detail_like_w() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]], "active": vis[0]}}), T);
    let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = crate::state::RightPanel::Edit;
    h.settle(SETTLE);
    // open the Color section, where the selector sits
    if !h.app.widgets.iter().any(|(w, _)| w == "icon:wbPicker") {
        h.request("ui.clickWidget", json!({"id": "section:color"}), T);
        h.settle(SETTLE);
    }
    assert_eq!(h.app.ui.view, ViewMode::Compare);
    let active = h.app.session.selection.active;
    let r = h.request("ui.clickWidget", json!({"id": "icon:wbPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.view, ViewMode::Detail, "the button leaves Compare like W");
    assert_eq!(h.app.ui.tool, "wbPicker");
    assert_eq!(h.app.session.selection.active, active, "on the photo that was active in Compare");
    let r = h.request("ui.clickWidget", json!({"id": "icon:wbPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "a second click disarms it");
}

/// Hovering the photo with the selector armed shows its loupe (#644); `tool.wbPicker {sample}`
/// sets the averaged square, snapped to an offered size, and a click still picks and disarms.
#[test]
fn wb_selector_shows_a_loupe_picks_on_a_click_and_w_and_the_wheel_work_it() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[2];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    h.app.ui.view = ViewMode::Detail;
    h.settle(SETTLE);
    let r = h.request("engine.execute", json!({"command": "tool.wbPicker", "params": {"sample": 8}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.wb_sample, 9, "snapped to an offered size");
    h.settle(SETTLE);
    let img = h.app.image_rect.expect("photo on screen");
    let loupe = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w == "canvas:wbLoupe");
    assert!(!loupe(&h), "no loupe before the pointer is over the photo");
    let pos = img.center();
    h.app.synthetic.push(egui::Event::PointerMoved(pos));
    h.settle(SETTLE);
    assert!(loupe(&h), "the loupe follows the pointer over the photo");
    // the sampled colour is read out right under the loupe (#1094)
    let rect = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r);
    let (Some(box_), Some(band)) = (rect(&h, "canvas:wbLoupe"), rect(&h, "canvas:wbLoupeReadout")) else {
        panic!("a readout beside the loupe: {:?}", h.app.widgets.iter().map(|(w, _)| w).collect::<Vec<_>>());
    };
    assert!(band.top() >= box_.bottom() || band.bottom() <= box_.top(), "beside the loupe, not over it");
    assert!(h.painted_text().iter().any(|t| t.starts_with("R ") && t.contains(" G ") && t.ends_with('%')), "{:?}", h.painted_text());
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "one click picks and disarms");
    let wb = h.app.session.catalog.photo(id).map(|p| p.develop.wb.mode);
    assert_eq!(wb, Some(lcc_develop::WbMode::Custom));
    // OneSlips (test notes 2026-10-25 note 13): W again puts the selector away without sampling, and while it is
    // armed the mouse wheel steps the sample's size instead of zooming
    let temp_tint = |h: &Headless| h.app.session.catalog.photo(id).map(|p| (p.develop.wb.temp, p.develop.wb.tint));
    let before = temp_tint(&h);
    h.request("ui.key", json!({"key": "w"}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "wbPicker", "W arms the selector");
    h.request("ui.key", json!({"key": "w"}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "W again puts it away");
    assert_eq!(temp_tint(&h), before, "without sampling");
    h.request("ui.key", json!({"key": "w"}), T);
    h.settle(SETTLE);
    let img = h.app.image_rect.expect("photo on screen");
    h.app.synthetic.push(egui::Event::PointerMoved(img.center()));
    h.settle(SETTLE);
    let (size, zoom) = (h.app.ui.wb_sample, h.app.ui.zoom);
    h.app.synthetic.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 1.0),
        phase: egui::TouchPhase::Move,
        modifiers: Default::default(),
    });
    h.settle(SETTLE);
    assert_ne!(h.app.ui.wb_sample, size, "the wheel changes the sample");
    assert_eq!(h.app.ui.zoom, zoom, "and doesn't zoom");
    assert_eq!(h.app.ui.tool, "wbPicker", "still armed");
}
