//! Headless tests of leaving the crop tool.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{RightPanel, ViewMode};
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Esc in the crop tool closes the tool and keeps the photo in Detail (and the crop); only the
/// next Esc goes back to the grid. It used to jump straight to the grid.
#[test]
fn escape_leaves_the_crop_tool_before_the_view() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[1];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    h.app.ui.view = ViewMode::Detail;
    let r = h.request("engine.execute", json!({"command": "panel.crop"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.1, 0.1, 0.8, 0.9]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.right, RightPanel::Crop);
    let crop = h.app.session.catalog.photo(id).map(|p| p.develop.crop.geometry.rect);
    h.request("ui.key", json!({"key": "Escape"}), T);
    h.settle(SETTLE);
    assert_ne!(h.app.ui.right, RightPanel::Crop, "Esc closes the crop tool");
    assert_eq!(h.app.ui.view, ViewMode::Detail, "and stays on the photo");
    assert_eq!(h.app.session.catalog.photo(id).map(|p| p.develop.crop.geometry.rect), crop, "the crop is kept");
    h.request("ui.key", json!({"key": "Escape"}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid, "the next Esc goes back to the grid");
}

/// Esc in the crop tool's Angle field only leaves the field: egui drops the field's focus as the
/// key arrives, so Esc used to run Back as well (to the grid; with the crop tool's own Esc step,
/// it would close the tool).
#[test]
fn escape_in_the_angle_field_only_leaves_the_field() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[1];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    h.app.ui.view = ViewMode::Detail;
    h.request("engine.execute", json!({"command": "panel.crop"}), T);
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "cropAngleField"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h.request("ui.text", json!({"text": "7"}), T);
    h.settle(SETTLE);
    h.request("ui.key", json!({"key": "Escape"}), T);
    h.settle(SETTLE);
    assert_eq!((h.app.ui.view, h.app.ui.right), (ViewMode::Detail, RightPanel::Crop), "Esc only left the field");
    // the field has let go: the next Esc is the crop tool's
    h.request("ui.key", json!({"key": "Escape"}), T);
    h.settle(SETTLE);
    assert_eq!((h.app.ui.view, h.app.ui.right == RightPanel::Crop), (ViewMode::Detail, false));
}

/// Arrow keys move the crop frame (⇧ ×10) and ⌥ arrows its right edge, so the crop tool works
/// without a pointer; → no longer steps to another photo while cropping.
#[test]
fn arrow_keys_move_and_resize_the_crop() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[1];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    h.app.ui.view = ViewMode::Detail;
    h.request("engine.execute", json!({"command": "panel.crop"}), T);
    h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.2, 0.2, 0.6, 0.6]}}), T);
    h.settle(SETTLE);
    let rect = |h: &Headless| h.app.session.catalog.photo(id).map(|p| p.develop.crop.geometry.rect);
    let r0 = rect(&h).unwrap();
    h.request("ui.key", json!({"key": "ArrowRight"}), T);
    h.settle(SETTLE);
    let r1 = rect(&h).unwrap();
    assert_eq!(h.app.session.active(), Some(id), "still on the same photo");
    assert!((r1.x0 - r0.x0 - 0.01).abs() < 1e-6 && (r1.x1 - r0.x1 - 0.01).abs() < 1e-6, "{r0:?} -> {r1:?}");
    h.request("ui.key", json!({"key": "ArrowDown", "shift": true}), T);
    h.settle(SETTLE);
    let r2 = rect(&h).unwrap();
    assert!((r2.y0 - r1.y0 - 0.1).abs() < 1e-6, "{r1:?} -> {r2:?}");
    h.request("ui.key", json!({"key": "ArrowLeft", "alt": true}), T);
    h.settle(SETTLE);
    let r3 = rect(&h).unwrap();
    assert!((r3.x0 - r2.x0).abs() < 1e-6 && (r3.x1 - r2.x1 + 0.01).abs() < 1e-6, "{r2:?} -> {r3:?}");
}
