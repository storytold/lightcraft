//! Headless tests of Space held over a tool on the photo (issue #1895): the view's hand pans and
//! click-zooms while the tool (Remove, a mask brush, Crop) gets neither, a quick tap still toggles
//! the zoom, and a held key's auto-repeat never toggles it again.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{RightPanel, Zoom};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn detail(panel: Option<&str>) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.settings.gpu = false;
    // Space and click zoom go straight to the ratio, not to the camera's focus point
    app.ui.settings.zoom_to_focus = false;
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    if let Some(panel) = panel {
        let r = h.request("engine.execute", json!({"command": panel}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.step();
    h
}

fn space(h: &mut Headless, action: &str) {
    let r = h.request("ui.key", json!({"key": "Space", "action": action}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

fn pointer(h: &mut Headless, events: serde_json::Value) {
    let r = h.request("ui.pointer", json!({"events": events}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

/// A point `dx`, `dy` (screen points) from the middle of the canvas, in the normalized image
/// coordinates `ui.pointer` takes: zoomed in, the photo's own middle can be off screen.
fn at(h: &Headless, dx: f32, dy: f32) -> (f32, f32) {
    let (img, canvas) = (h.app.image_rect.unwrap(), h.app.canvas_rect.unwrap());
    let q = canvas.center() + egui::vec2(dx, dy);
    ((q.x - img.left()) / img.width(), (q.y - img.top()) / img.height())
}

/// A drag of 120 × 40 points across the middle of the canvas.
fn drag(h: &mut Headless) {
    let p: Vec<(f32, f32)> = [(0.0, 0.0), (20.0, 5.0), (60.0, 20.0), (120.0, 40.0)].iter().map(|&(x, y)| at(h, x, y)).collect();
    pointer(
        h,
        json!([
            {"kind": "down", "x": p[0].0, "y": p[0].1},
            {"kind": "drag", "x": p[1].0, "y": p[1].1},
            {"kind": "drag", "x": p[2].0, "y": p[2].1},
            {"kind": "drag", "x": p[3].0, "y": p[3].1},
            {"kind": "up", "x": p[3].0, "y": p[3].1}
        ]),
    );
}

fn click(h: &mut Headless, (x, y): (f32, f32)) {
    pointer(h, json!([{"kind": "down", "x": x, "y": y}, {"kind": "up", "x": x, "y": y}]));
}

fn develop(h: &Headless) -> lightcraft_develop::DevelopSettings {
    let id = h.app.session.active().expect("active photo");
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn zoom_in(h: &mut Headless) {
    h.app.run("view.navigate", json!({"zoom": {"percent": 200.0}, "pan": [0.5, 0.5]})).unwrap();
    h.step();
    h.step();
    let (img, canvas) = (h.app.image_rect.unwrap(), h.app.canvas_rect.unwrap());
    assert!(img.width() > canvas.width(), "not zoomed: {img:?} in {canvas:?}");
}

fn hand_shown(h: &Headless) -> bool {
    h.app.widgets.iter().any(|(id, _)| id == "loupe:hand")
}

#[test]
fn a_held_space_toggles_the_zoom_once_however_long_it_repeats() {
    // no tool: the press toggles at once, as before; the key's auto-repeat used to flip it back
    // and forth for as long as it was held
    let mut h = detail(None);
    assert_eq!(h.app.ui.zoom, Zoom::Fit);
    space(&mut h, "down");
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));
    assert!(h.app.space_hand.is_none(), "no tool: no hand");
    // further presses of a key that is down are the system's auto-repeat
    for _ in 0..3 {
        space(&mut h, "down");
    }
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0), "a repeat toggled the zoom");
    space(&mut h, "up");
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0), "the release toggled the zoom");
    // the next press is a new one
    space(&mut h, "press");
    assert_eq!(h.app.ui.zoom, Zoom::Fit);
}

#[test]
fn space_held_over_the_healing_brush_pans_and_paints_no_spot() {
    let mut h = detail(Some("panel.remove"));
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    zoom_in(&mut h);
    let spots = develop(&h).spots.len();
    let pan = h.app.ui.pan;
    space(&mut h, "down");
    assert!(h.app.space_hand.is_some());
    assert_eq!(h.app.ui.zoom, Zoom::Percent(200.0), "the press zoomed");
    pointer(&mut h, json!([{"kind": "move", "x": 0.5, "y": 0.5}]));
    assert!(hand_shown(&h));
    assert_eq!(h.last_cursor, egui::CursorIcon::Grab);
    drag(&mut h);
    assert_eq!(develop(&h).spots.len(), spots, "the drag painted a spot");
    assert_ne!(h.app.ui.pan, pan, "the drag did not pan");
    assert_eq!(h.app.ui.zoom, Zoom::Percent(200.0));
    // letting go after a drag is not a tap: the zoom stays
    space(&mut h, "up");
    assert!(h.app.space_hand.is_none());
    assert!(!hand_shown(&h));
    assert_eq!(h.app.ui.zoom, Zoom::Percent(200.0), "the release toggled the zoom");
    // the tool has its pointer back: the same drag paints a spot and leaves the view alone
    let pan = h.app.ui.pan;
    drag(&mut h);
    assert_eq!(develop(&h).spots.len(), spots + 1, "the brush did not paint after the release");
    assert_eq!(h.app.ui.pan, pan);
}

#[test]
fn space_held_click_zooms_to_the_point_clicked_and_back() {
    let mut h = detail(Some("panel.remove"));
    let spots = develop(&h).spots.len();
    space(&mut h, "down");
    pointer(&mut h, json!([{"kind": "move", "x": 0.25, "y": 0.3}]));
    assert_eq!(h.last_cursor, egui::CursorIcon::ZoomIn, "at Fit the hand zooms in");
    click(&mut h, (0.25, 0.3));
    assert_eq!(h.app.ui.zoom, Zoom::Percent(h.app.ui.click_zoom as f32));
    let (u, v) = h.app.ui.pan;
    assert!((u - 0.25).abs() < 0.02 && (v - 0.3).abs() < 0.02, "zoomed to {:?}, not the point clicked", h.app.ui.pan);
    // zoomed in, a click goes back to Fit
    for _ in 0..30 {
        h.step();
    }
    let middle = at(&h, 0.0, 0.0);
    click(&mut h, middle);
    assert_eq!(h.app.ui.zoom, Zoom::Fit);
    space(&mut h, "up");
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "the release toggled the zoom");
    assert_eq!(develop(&h).spots.len(), spots, "a click painted a spot");
}

#[test]
fn a_tap_over_a_tool_toggles_on_release_and_a_long_hold_does_not() {
    let mut h = detail(Some("panel.remove"));
    let spots = develop(&h).spots.len();
    space(&mut h, "press");
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0), "a tap did not toggle the zoom");
    assert!(h.app.space_hand.is_none());
    space(&mut h, "down");
    space(&mut h, "up");
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "a quick press and release did not toggle back");
    // held a while with no click, then let go: changed one's mind, nothing happens
    space(&mut h, "down");
    let steps = (crate::space_hand::TAP * 60.0).ceil() as usize + 10;
    for _ in 0..steps {
        h.step();
    }
    // the key's auto-repeat in the meantime keeps the hand
    space(&mut h, "down");
    assert!(h.app.space_hand.is_some(), "a repeat ended the hand");
    space(&mut h, "up");
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "a long hold toggled the zoom");
    assert_eq!(develop(&h).spots.len(), spots);
}

#[test]
fn space_held_over_a_mask_brush_paints_nothing() {
    let mut h = detail(Some("panel.masking"));
    let r = h.request("ui.clickWidget", json!({"id": "maskNew:brush"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"brushSize": 0.15, "brushFeather": 0.0, "brushFlow": 100.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    zoom_in(&mut h);
    let before = develop(&h);
    let pan = h.app.ui.pan;
    space(&mut h, "down");
    drag(&mut h);
    space(&mut h, "up");
    assert_eq!(develop(&h), before, "the drag painted the mask");
    assert_ne!(h.app.ui.pan, pan, "the drag did not pan");
    assert!(h.app.brush_live.is_none());
}

#[test]
fn space_held_over_the_crop_tool_pans_and_leaves_the_crop() {
    let mut h = detail(Some("panel.crop"));
    assert_eq!(h.app.ui.right, RightPanel::Crop);
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.1, 0.1, 0.8, 0.9]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    zoom_in(&mut h);
    let before = develop(&h);
    let pan = h.app.ui.pan;
    space(&mut h, "down");
    drag(&mut h);
    space(&mut h, "up");
    assert_eq!(develop(&h), before, "the drag moved the crop");
    assert_ne!(h.app.ui.pan, pan, "the drag did not pan");
}
