//! Headless tests of leaving the crop tool.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{RightPanel, ViewMode};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Esc in the crop tool closes the tool and keeps the photo in Detail (and the crop); only the
/// next Esc goes back to the grid. It used to jump straight to the grid.
#[test]
fn escape_leaves_the_crop_tool_before_the_view() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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

/// X in the crop tool swaps the crop's orientation once (issue #1866). X is both Reject and Swap
/// Aspect, and both bindings fired: the crop turned portrait and straight back to landscape.
#[test]
fn x_in_the_crop_tool_swaps_the_aspect_once() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let landscape =
        |h: &Headless, id: &lightcraft_catalog::PhotoId| h.app.session.catalog.photo(*id).is_some_and(|p| p.width > p.height && !p.is_edited());
    let id = h.app.session.visible_cloned().into_iter().find(|id| landscape(&h, id)).unwrap();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    h.app.ui.view = ViewMode::Detail;
    h.request("engine.execute", json!({"command": "panel.crop"}), T);
    let r = h.request("engine.execute", json!({"command": "crop.aspect", "params": {"aspect": "3x4"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.right, RightPanel::Crop);
    let crop = |h: &Headless| {
        let p = h.app.session.catalog.photo(id).unwrap();
        let r = p.develop.crop.geometry.rect_px(p.width as f64, p.height as f64);
        (r.width() > r.height(), p.develop.crop.aspect)
    };
    assert_eq!(crop(&h), (true, Some((400, 300))), "a 3:4 crop follows the landscape photo");
    let undo = h.app.session.undo.len();
    h.request("ui.key", json!({"key": "x"}), T);
    h.settle(SETTLE);
    assert_eq!(crop(&h), (false, Some((300, 400))), "X turns the crop portrait");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one swap, one undo step");
    assert_eq!(h.app.session.catalog.photo(id).unwrap().flag, lightcraft_catalog::Flag::None, "X doesn't reject while cropping");
    h.request("ui.key", json!({"key": "x"}), T);
    h.settle(SETTLE);
    assert_eq!(crop(&h), (true, Some((400, 300))), "X again turns it back");
}

/// Zoomed in, the crop box reaches past the bottom of the loupe: its frame, guides and handles
/// stop at the loupe and are not painted over the filmstrip below it (#3966).
#[test]
fn the_crop_box_is_not_painted_over_the_filmstrip() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[1];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    h.app.ui.view = ViewMode::Detail;
    h.app.ui.filmstrip = true;
    let r = h.request("engine.execute", json!({"command": "panel.crop"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.zoom = crate::state::Zoom::Percent(400.0);
    h.app.ui.pan = (0.5, 0.5);
    h.settle(SETTLE);
    let canvas = h.app.canvas_rect.expect("the loupe is drawn");
    assert!(canvas.bottom() < 900.0 - 20.0, "the filmstrip shows below the loupe ({canvas:?})");
    // the crop box's frame: a closed white line through its four corners
    let white = egui::epaint::ColorMode::Solid(egui::Color32::WHITE);
    let frames: Vec<_> = h
        .view
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::epaint::Shape::Path(p) if p.closed && p.points.len() == 4 && p.stroke.width == 1.0 && p.stroke.color == white => Some((c, p)),
            _ => None,
        })
        .collect();
    assert_eq!(frames.len(), 1, "the crop box is drawn once");
    let (c, p) = frames[0];
    assert!(p.points.iter().any(|q| q.y > canvas.bottom() + 50.0), "zoomed in, the box reaches past the loupe ({:?}, loupe {canvas:?})", p.points);
    assert!(c.clip_rect.bottom() <= canvas.bottom() + 0.5, "the box is clipped to the loupe, not {:?} (loupe {canvas:?})", c.clip_rect);
}
