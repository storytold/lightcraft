//! Headless tests of the Lightroom-style crop interaction (opted in with
//! `cropInteractionStyle: "lightroom"`; the default is Photoshop): inside the box a drag pans the
//! photo under the fixed frame, in the dimmed area over the image it draws a new box, in the margin
//! it rotates. The Photoshop style keeps the old behaviour (the frame moves under a still pointer).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
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

/// The crop panel with the Lightroom canvas opted in (the default is Photoshop).
fn crop_tool_lr() -> Headless {
    let mut h = crop_tool();
    let r = h.request("ui.clickWidget", json!({"id": "button:cropStyle-lightroom"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.settings.crop_lightroom());
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn rect(h: &Headless) -> lightcraft_geom::Rect {
    let id = h.app.session.active().expect("a photo");
    h.app.session.catalog.photo(id).expect("the photo").develop.crop.geometry.rect
}

fn angle(h: &Headless) -> f64 {
    let id = h.app.session.active().expect("a photo");
    h.app.session.catalog.photo(id).expect("the photo").develop.crop.geometry.angle
}

fn pixel(im: &egui::ColorImage, p: egui::Pos2) -> egui::Color32 {
    im.pixels[p.y.round() as usize * im.size[0] + p.x.round() as usize]
}

/// The crop interaction defaults to the Photoshop style (the tool's earlier behaviour), so an
/// existing user's canvas is unchanged until they opt in; the Crop panel offers both styles.
#[test]
fn the_crop_interaction_defaults_to_photoshop() {
    let h = crop_tool();
    assert_eq!(h.app.ui.settings.crop_interaction_style, "photoshop");
    assert!(!h.app.ui.settings.crop_lightroom());
    assert!(has(&h, "button:cropStyle-lightroom"), "the style combo offers Lightroom");
    assert!(has(&h, "button:cropStyle-photoshop"), "and Photoshop");
}

/// The style combo in the Crop panel switches the stored setting, and with it the behaviour:
/// Lightroom is a click away, and clicking it back keeps the old canvas.
#[test]
fn the_style_combo_switches_the_crop_interaction() {
    let mut h = crop_tool();
    let r = h.request("ui.clickWidget", json!({"id": "button:cropStyle-lightroom"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.settings.crop_interaction_style, "lightroom");
    assert!(h.app.ui.settings.crop_lightroom());
    let r = h.request("ui.clickWidget", json!({"id": "button:cropStyle-photoshop"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.settings.crop_interaction_style, "photoshop");
    assert!(!h.app.ui.settings.crop_lightroom());
}

/// In the Lightroom style the frame is fixed: a drag inside the box slides the photo under it (the
/// applied rect moves opposite the pointer), and the painted photo slides with the pointer.
#[test]
fn dragging_inside_the_box_pans_the_photo_under_the_frame() {
    let mut h = crop_tool_lr();
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.25, 0.25, 0.75, 0.75]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let before = h.paint();
    let img = h.app.image_rect.expect("the photo on screen");
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": 0.5, "y": 0.5}, {"kind": "drag", "x": 0.55, "y": 0.5}, {"kind": "drag", "x": 0.6, "y": 0.5}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let b = rect(&h);
    assert!((b.x0 - 0.15).abs() < 1e-3 && (b.x1 - 0.65).abs() < 1e-3, "the applied rect moved opposite the pointer: {b:?}");
    assert!((b.y0 - 0.25).abs() < 1e-3 && (b.y1 - 0.75).abs() < 1e-3, "and not vertically");
    // the photo slid under the fixed frame: the same spot shows a different part of it
    let at = egui::pos2(img.left() + 0.45 * img.width(), img.center().y);
    assert_ne!(pixel(&h.paint(), at), pixel(&before, at), "the photo slid with the pointer");
    // the display offset survives the release: the photo keeps sitting where the pan left it
    assert_ne!(h.app.ui.crop_pan, egui::Vec2::ZERO, "the pan persists after the release");
    // closing the tool clears it: the photo is back under the frame
    let r = h.request("engine.execute", json!({"command": "tool.done"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.crop_pan, egui::Vec2::ZERO, "closing the tool clears the pan");
}

/// In the dimmed area over the photo (outside the box) a drag draws a new box from the press
/// point, as in Lightroom's crop tool.
#[test]
fn dragging_in_the_dimmed_area_draws_a_new_box() {
    let mut h = crop_tool_lr();
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.1, 0.1, 0.6, 0.6]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": 0.75, "y": 0.7}, {"kind": "drag", "x": 0.85, "y": 0.8}, {"kind": "drag", "x": 0.9, "y": 0.9}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let b = rect(&h);
    assert!((b.x0 - 0.75).abs() < 1e-3 && (b.x1 - 0.9).abs() < 1e-3, "drawn from the press point: {b:?}");
    assert!((b.y0 - 0.7).abs() < 1e-3 && (b.y1 - 0.9).abs() < 1e-3, "and to the release point");
}

/// A new box drawn while the photo is turned still lands under the pointer: the Lightroom pointer
/// coordinates slide by the pan only — rotating them again (the Photoshop path) would skew the
/// box away from where it was drawn.
#[test]
fn drawing_a_new_box_at_an_angle_follows_the_pointer() {
    let mut h = crop_tool_lr();
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "crop.angle", "value": 12.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.15, 0.15, 0.45, 0.45]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(angle(&h) != 0.0, "the photo is turned");
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": 0.55, "y": 0.7}, {"kind": "drag", "x": 0.6, "y": 0.75}, {"kind": "drag", "x": 0.65, "y": 0.8}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let b = rect(&h);
    assert!((b.x0 - 0.55).abs() < 1e-3 && (b.x1 - 0.65).abs() < 1e-3, "drawn from the press point despite the angle: {b:?}");
    assert!((b.y0 - 0.7).abs() < 1e-3 && (b.y1 - 0.8).abs() < 1e-3, "and to the release point: {b:?}");
}

/// In the margin beside the photo a drag rotates, as in both styles; the angle readout follows.
#[test]
fn dragging_in_the_margin_rotates() {
    let mut h = crop_tool_lr();
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": -0.05, "y": 0.2}, {"kind": "drag", "x": -0.05, "y": 0.25}, {"kind": "drag", "x": -0.05, "y": 0.3}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(angle(&h) != 0.0, "the drag rotated the crop");
    assert!(has(&h, "cropAngleReadout"), "the angle is shown while rotating");
}

/// The straighten zone sits next to the selection: a drag in the strip right around the crop box
/// rotates (with the angle readout and the straighten grid), instead of drawing a new box over the
/// dimmed photo, and the box itself is not redrawn from the press point.
#[test]
fn dragging_in_the_strip_around_the_box_rotates() {
    let mut h = crop_tool_lr();
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.1, 0.1, 0.6, 0.6]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let img = h.app.image_rect.expect("the photo on screen");
    // 30 px right of the box's right edge at mid-height: inside the photo, within the strip
    let x = (0.6 + 30.0 / img.width()) as f64;
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": x, "y": 0.35}, {"kind": "drag", "x": x, "y": 0.45}, {"kind": "drag", "x": x, "y": 0.55}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(angle(&h) != 0.0, "the drag in the strip rotated the crop");
    assert!(has(&h, "cropAngleReadout"), "the angle is shown while rotating");
    assert!(has(&h, "cropStraightenGrid"), "the straighten grid is shown inside the selection");
    let r = h.request("ui.pointer", json!({"events": [{"kind": "up", "x": x, "y": 0.55}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let b = rect(&h);
    assert!((b.x0 - x).abs() > 0.01 && (b.y0 - 0.35).abs() > 0.01, "no new box was drawn from the press point: {b:?}");
    assert!(!has(&h, "cropStraightenGrid"), "the grid goes away with the drag");
    assert!(!has(&h, "cropAngleReadout"), "and so does the readout");
}

/// The rotation glyph shows in the strip around the box (the straighten zone); deeper in the
/// dimmed photo — where a press draws a new box — it does not.
#[test]
fn the_pointer_offers_rotation_in_the_strip_around_the_box() {
    let mut h = crop_tool_lr();
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.1, 0.1, 0.6, 0.6]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let img = h.app.image_rect.expect("the photo on screen");
    let hover = |h: &mut Headless, x: f32, y: f32| {
        let p = egui::pos2(img.left() + x * img.width(), img.top() + y * img.height());
        let r = h.request("ui.move", json!({"x": p.x, "y": p.y}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    };
    hover(&mut h, 0.6 + 30.0 / img.width(), 0.35);
    assert_eq!(h.last_cursor, egui::CursorIcon::None, "the system cursor gives way to the glyph");
    assert!(has(&h, "cropRotateCursor"), "the rotation glyph is drawn next to the box");
    hover(&mut h, 0.8, 0.75);
    assert_eq!(h.last_cursor, egui::CursorIcon::Crosshair, "deep in the dimmed photo a press draws a new box");
    assert!(!has(&h, "cropRotateCursor"));
}

/// The corner handles resize in both styles: the Lightroom style only changes what a drag inside
/// the box does.
#[test]
fn dragging_a_handle_still_resizes_the_box() {
    let mut h = crop_tool_lr();
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.2, 0.2, 0.8, 0.8]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let before = rect(&h);
    let r = h.request("ui.dragWidget", json!({"id": "cropHandle:0", "dx": 40.0, "dy": 40.0, "steps": 6}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let b = rect(&h);
    assert!(b.x0 > before.x0 && b.y0 > before.y0, "the top-left handle moved in: {b:?}");
    assert!((b.x1 - before.x1).abs() < 1e-6 && (b.y1 - before.y1).abs() < 1e-6, "the far edges held: {b:?} vs {before:?}");
}

/// In the Photoshop style the old behaviour holds: a drag inside the box moves it with the pointer
/// (the photo stays put), and the Lightroom style is one setting away.
#[test]
fn photoshop_style_moves_the_box_with_the_pointer() {
    let mut h = crop_tool();
    let r = h.request("ui.set", json!({"settings": {"cropInteractionStyle": "photoshop"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.25, 0.25, 0.75, 0.75]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let before = h.paint();
    let img = h.app.image_rect.expect("the photo on screen");
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": 0.5, "y": 0.5}, {"kind": "drag", "x": 0.55, "y": 0.5}, {"kind": "drag", "x": 0.6, "y": 0.5}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let b = rect(&h);
    assert!((b.x0 - 0.35).abs() < 1e-3 && (b.x1 - 0.85).abs() < 1e-3, "the box followed the pointer: {b:?}");
    // release: without it the interaction never ends and the paint below would compare the
    // draft the drag leaves on screen with the finished render `before` was taken from
    let r = h.request("ui.pointer", json!({"events": [{"kind": "up", "x": 0.6, "y": 0.5}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.session.interaction.is_none(), "the drag ended at rest");
    // the photo stayed put: the same spot shows the same pixel
    let at = egui::pos2(img.left() + 0.5 * img.width(), img.center().y);
    assert_eq!(pixel(&h.paint(), at), pixel(&before, at), "the photo did not move");
}

/// In the Lightroom style the photo turns under the frame: at an angle the painted pixels are the
/// turned photo's, where the Photoshop style shows the photo unrotated under a turned box (same
/// box, same angle, same spot on screen).
#[test]
fn the_lightroom_style_turns_the_photo_under_the_frame() {
    let mut h = crop_tool_lr();
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "crop.angle", "value": -30.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let turned = h.paint();
    let r = h.request("ui.set", json!({"settings": {"cropInteractionStyle": "photoshop"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let straight = h.paint();
    let b = rect(&h);
    let img = h.app.image_rect.expect("the photo on screen");
    // a spot inside the box, off its centre: the turned photo shows a different part of itself
    // (the crop rect is normalized; the spot goes to screen through the photo's own rect)
    let at = egui::pos2(img.left() + (b.center().x + 0.1) as f32 * img.width(), img.top() + b.center().y as f32 * img.height());
    assert!(img.shrink(4.0).contains(at), "the spot is on the photo");
    assert_ne!(pixel(&turned, at), pixel(&straight, at), "the Lightroom style turned the photo");
}

/// The keyboard moves a handle the way the pointer does: under the Lightroom style the arrows
/// follow the screen — the box is axis-aligned, so at an angle the drag must not unturn the
/// coordinates (the Photoshop path) and skew the box away from the arrow's own direction.
#[test]
fn keyboard_arrows_at_an_angle_move_the_box_without_skew() {
    let mut h = crop_tool_lr();
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "crop.angle", "value": 12.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.2, 0.2, 0.6, 0.6]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(angle(&h) != 0.0, "the photo is turned");
    // the bottom-right corner, moved left: only x changes, the bottom edge holds
    h.step();
    h.view.ctx.memory_mut(|m| m.request_focus(crate::canvas_keys::id("crop:2")));
    h.step();
    h.step();
    assert_eq!(h.view.ctx.memory(|m| m.focused()), Some(crate::canvas_keys::id("crop:2")), "the handle takes the focus");
    let before = rect(&h);
    let r = h.request("ui.key", json!({"key": "left"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let after = rect(&h);
    assert!(after.x1 < before.x1 - 1e-4, "the right edge moved with the arrow: {before:?} -> {after:?}");
    assert!((after.y1 - before.y1).abs() < 1e-6, "and not vertically (no skew): {after:?}");
}
