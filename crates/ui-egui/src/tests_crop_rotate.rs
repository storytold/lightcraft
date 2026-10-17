//! Rotating in the crop tool (issue #534): outside the crop box the pointer shows that a drag
//! rotates, a drag shows the angle as it changes, and the Straighten value takes an exact angle.

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

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn angle(h: &Headless) -> f64 {
    let id = h.app.session.active().expect("a photo");
    h.app.session.catalog.photo(id).expect("the photo").develop.crop.geometry.angle
}

/// The rotation pivots on the crop box's own centre, not the photo's. A box drawn off-centre used
/// to swing around the photo's middle, so the angle came out of a different origin than the pointer
/// moved on, and the box turned by however much happened to be left over.
#[test]
fn the_rotation_pivots_on_the_box_centre() {
    let mut h = crop_tool();
    // a box well left of and below the photo's centre, so the two pivots cannot agree
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.05, 0.3, 0.45, 0.7]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let img = h.app.image_rect.expect("the photo on screen");
    // the overlay's own corners, not `rect.center()` mapped as image coords (that is the photo's frame)
    let (cx, cy) = {
        let p = |i| corner(&h, i);
        ((p(0).x + p(1).x + p(2).x + p(3).x) / 4.0, (p(0).y + p(1).y + p(2).y + p(3).y) / 4.0)
    };
    let radius = img.width() * 0.30;
    // pointer positions on an arc about the box centre; the first drag event is where the gesture
    // begins, so that is where the angle is measured from
    let at = |deg: f64| {
        let a = deg.to_radians();
        ((cx + radius * a.cos() as f32 - img.left()) / img.width(), (cy + radius * a.sin() as f32 - img.top()) / img.height())
    };
    let (press_deg, to_deg) = (10.0_f64, -25.0_f64);
    let (from, to) = (at(press_deg - 4.0), at(to_deg));
    let press = at(press_deg);
    let r = h.request(
        "ui.pointer",
        json!({"events": [
            {"kind": "down", "x": press.0, "y": press.1},
            {"kind": "drag", "x": from.0, "y": from.1},
            {"kind": "drag", "x": to.0, "y": to.1},
            {"kind": "drag", "x": to.0, "y": to.1},
        ]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    // the box turns by the pointer's travel about its own centre, measured from where the button went
    // down, and the straighten angle is that correction, so it comes out negated
    let want = press_deg - to_deg;
    assert!((angle(&h) - want).abs() < 0.01, "the pointer turned {want}° about the box centre, the box gave {}", angle(&h));
}

/// A handle grabbed on press stays that handle, however far the pointer travels before the drag
/// registers. Judged on the pointer's live position instead of where the button went down, a fast
/// or long first move cleared the handle's 12 px radius and the drag silently became a rotate,
/// which then ran into the ±45° clamp.
#[test]
fn a_handle_stays_a_handle_however_far_the_pointer_moves_first() {
    let mut h = crop_tool();
    let before = angle(&h);
    let img = h.app.image_rect.expect("the photo on screen");
    fn handle_at(h: &Headless, i: usize) -> egui::Pos2 {
        h.app.widgets.iter().find(|(w, _)| *w == format!("cropHandle:{i}")).map(|(_, r)| r.center()).expect("a handle")
    }
    for i in 0..8_usize {
        let p = handle_at(&h, i);
        let x = f64::from((p.x - img.left()) / img.width());
        let y = f64::from((p.y - img.top()) / img.height());
        // press on the handle, then jump far enough to leave its 12 px radius before the first drag
        let r = h.request(
            "ui.pointer",
            json!({"events": [{"kind": "down", "x": x, "y": y}, {"kind": "drag", "x": x, "y": y - 0.12}, {"kind": "drag", "x": x, "y": y - 0.16}]}),
            T,
        );
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(angle(&h), before, "handle {i}: a handle drag must not turn the crop");
        h.request("ui.pointer", json!({"events": [{"kind": "up", "x": x, "y": y - 0.16}]}), T);
        h.settle(SETTLE);
    }
}

/// A rotate drag that starts at a left corner and crosses the box's left midline used to jump to
/// the ±45° stop: atan2 wraps at ±π, the raw `a - a0` is ~±315°, and the clamp snaps. The
/// shortest turn keeps the sign it had before the cut.
fn inset_crop(h: &mut Headless) {
    let r = h.request("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.25, 0.25, 0.75, 0.75]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

fn img_xy(h: &Headless, p: egui::Pos2) -> (f64, f64) {
    let img = h.app.image_rect.expect("the photo on screen");
    (f64::from((p.x - img.left()) / img.width()), f64::from((p.y - img.top()) / img.height()))
}

#[test]
fn dragging_down_the_left_does_not_snap_to_minus_45() {
    let mut h = crop_tool();
    inset_crop(&mut h);
    let tl = corner(&h, 0);
    let bl = corner(&h, 3);
    let mid_y = (tl.y + bl.y) / 2.0;
    // just left of the top-left handle: outside the box, so this is a rotate, not a resize
    let press = img_xy(&h, egui::pos2(tl.x - 20.0, tl.y));
    let past = img_xy(&h, egui::pos2(tl.x - 20.0, mid_y + (bl.y - tl.y) * 0.15));
    let r = h.request(
        "ui.pointer",
        json!({"events": [
            {"kind": "down", "x": press.0, "y": press.1},
            {"kind": "drag", "x": press.0, "y": press.1 + 0.02},
            {"kind": "drag", "x": past.0, "y": past.1},
        ]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let a = angle(&h);
    assert!(a > -44.0, "crossed the left midline: snapped to the −45° stop ({a})");
    assert_ne!(a, 0.0, "the drag did rotate");
}

#[test]
fn dragging_up_the_left_does_not_snap_to_plus_45() {
    let mut h = crop_tool();
    inset_crop(&mut h);
    let tl = corner(&h, 0);
    let bl = corner(&h, 3);
    let mid_y = (tl.y + bl.y) / 2.0;
    let press = img_xy(&h, egui::pos2(bl.x - 20.0, bl.y));
    let past = img_xy(&h, egui::pos2(bl.x - 20.0, mid_y - (bl.y - tl.y) * 0.15));
    let r = h.request(
        "ui.pointer",
        json!({"events": [
            {"kind": "down", "x": press.0, "y": press.1},
            {"kind": "drag", "x": press.0, "y": press.1 - 0.02},
            {"kind": "drag", "x": past.0, "y": past.1},
        ]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let a = angle(&h);
    assert!(a < 44.0, "crossed the left midline: snapped to the +45° stop ({a})");
    assert_ne!(a, 0.0, "the drag did rotate");
}

/// The crop box lies inside the rotated photo (issue #742: a typed or slid angle shrinks it to fit,
/// as the rotate handle does).
fn crop_fits(h: &Headless) -> bool {
    let id = h.app.session.active().expect("a photo");
    let p = h.app.session.catalog.photo(id).expect("the photo");
    let (w, h) = (p.width as f64, p.height as f64);
    let (w, h) = if p.develop.orientation.swaps_axes() { (h, w) } else { (w, h) };
    let g = p.develop.crop.geometry;
    g.rect != lightcraft_geom::Rect::UNIT && g.is_within_image(w, h)
}

fn hover(h: &mut Headless, p: egui::Pos2) {
    let r = h.request("ui.move", json!({"x": p.x, "y": p.y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

fn corner(h: &Headless, i: usize) -> egui::Pos2 {
    h.app.widgets.iter().find(|(w, _)| *w == format!("cropHandle:{i}")).map(|(_, r)| r.center()).expect("a corner handle")
}

/// Degrees in (-180, 180]; screen angles run the other way once wrapped.
fn wrap(d: f32) -> f64 {
    let d = d.to_degrees() as f64;
    (d + 180.0) % 360.0 - 180.0
}

/// The crop box turns the way the pointer does (issue #962). Measured on screen, where y grows
/// downwards, so a falling angle is counter-clockwise. The drag starts left of the photo and
/// moves down: on the left, downward is counter-clockwise, whatever the photo's proportions, so
/// the expected turn needs no arithmetic. An edge is the probe, not a diagonal: the box also
/// shrinks to fit as it turns (issue #742), by different amounts per axis, and that bends a
/// diagonal without turning anything.
#[test]
fn the_crop_box_follows_the_pointer() {
    let mut h = crop_tool();
    let before = corner(&h, 1) - corner(&h, 0);
    let r = h.request(
        "ui.pointer",
        json!({"events": [
            {"kind": "down", "x": -0.05, "y": 0.2},
            {"kind": "drag", "x": -0.05, "y": 0.25},
            {"kind": "drag", "x": -0.05, "y": 0.3},
        ]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, "cropAngleReadout"), "the drag is rotating at all");
    let turn = wrap((corner(&h, 1) - corner(&h, 0)).angle() - before.angle());
    assert!(
        turn < -1.0,
        "dragging down on the left turns the pointer counter-clockwise, so the box must too, not the other way: it turned {turn:.2}°"
    );
}

/// Outside the crop box, where a drag rotates, the pointer becomes a curved double arrow drawn at
/// the pointer (no system cursor has one); inside it is the move cursor, with no rotation glyph.
#[test]
fn outside_the_crop_box_the_pointer_shows_rotation() {
    let mut h = crop_tool();
    let img = h.app.image_rect.expect("the photo on screen");
    hover(&mut h, egui::pos2(img.left() - 24.0, img.center().y));
    assert_eq!(h.last_cursor, egui::CursorIcon::None, "the system cursor gives way to the glyph");
    assert!(has(&h, "cropRotateCursor"), "the rotation glyph is drawn at the pointer");
    hover(&mut h, img.center());
    assert_eq!(h.last_cursor, egui::CursorIcon::Move);
    assert!(!has(&h, "cropRotateCursor"));
}

/// Dragging outside the box rotates, and the angle is shown next to the pointer while it moves.
#[test]
fn rotating_shows_the_angle() {
    let mut h = crop_tool();
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": -0.05, "y": 0.2}, {"kind": "drag", "x": -0.05, "y": 0.25}, {"kind": "drag", "x": -0.05, "y": 0.3}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(angle(&h) != 0.0, "the drag rotated the crop");
    assert!(has(&h, "cropAngleReadout"), "the angle is shown while rotating");
    let r = h.request("ui.pointer", json!({"events": [{"kind": "up", "x": -0.05, "y": 0.3}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!has(&h, "cropAngleReadout"), "and gone once released");
}

/// The Straighten value takes an exact angle: click it, type, Enter.
#[test]
fn the_straighten_value_takes_an_exact_angle() {
    let mut h = crop_tool();
    let r = h.request("ui.clickWidget", json!({"id": "sliderValue:crop.angle"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    for key in ["a", "Backspace"] {
        // select what is there and clear it
        let r = h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    let r = h.request("ui.text", json!({"text": "2.5"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.key", json!({"key": "Enter"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!((angle(&h) - 2.5).abs() < 1e-9, "typed 2.5°, got {}", angle(&h));
    assert!(crop_fits(&h), "the Straighten value shrinks the crop to fit the photo");
}

/// The angle next to the pointer reads like the Straighten value, in degrees.
#[test]
fn the_angle_reads_like_the_straighten_value() {
    use crate::panels::detail::crop_angle_label;
    assert_eq!(crop_angle_label(0.0), "0.00°", "as the Angle field shows it at rest");
    assert_eq!(crop_angle_label(2.5), "+2.50°");
    assert_eq!(crop_angle_label(-12.254), "-12.25°");
    assert_eq!(crop_angle_label(-0.001), "0.00°", "no minus zero, and every zero the same");
}

/// Hovering the Straighten value says that a value can be typed there.
#[test]
fn the_straighten_value_says_it_can_be_typed() {
    let mut h = crop_tool();
    let r = h.request("ui.hoverWidget", json!({"id": "sliderValue:crop.angle"}), T);
    assert_eq!(r["ok"], true, "{r}");
    // tooltips wait for the pointer to rest
    let _ = h.step_until(Duration::from_secs(5), |h| h.painted_text().iter().any(|t| t.contains("type")));
    assert!(
        h.painted_text().iter().any(|t| t == "Click to type a value, drag to change it"),
        "{:?}",
        h.painted_text().iter().filter(|t| t.contains("lick")).collect::<Vec<_>>()
    );
}

/// With ⌘ held a drag draws a level line instead of rotating, so the pointer is the crosshair, not
/// the rotation arrow.
#[test]
fn command_held_shows_the_straighten_crosshair() {
    let mut h = crop_tool();
    let img = h.app.image_rect.expect("the photo on screen");
    h.events.push(egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
    h.events.push(egui::Event::PointerMoved(egui::pos2(img.left() - 24.0, img.center().y)));
    h.settle(SETTLE);
    assert_eq!(h.last_cursor, egui::CursorIcon::Crosshair);
    assert!(!has(&h, "cropRotateCursor"));
}

/// The Crop panel has an angle field: click it, type an exact angle, Return.
#[test]
fn the_crop_panel_has_an_angle_field() {
    let mut h = crop_tool();
    let r = h.request("ui.clickWidget", json!({"id": "cropAngleField"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    for key in ["a", "Backspace"] {
        let r = h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    let r = h.request("ui.text", json!({"text": "-3.25"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.key", json!({"key": "Enter"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!((angle(&h) + 3.25).abs() < 1e-9, "typed -3.25°, got {}", angle(&h));
    assert!(crop_fits(&h), "the Angle field shrinks the crop to fit the photo");
}

/// Rotating with the pointer at the canvas's corner keeps the angle readout on the canvas (and so
/// on screen), rather than spilling past its edge.
#[test]
fn the_readout_stays_on_the_canvas() {
    let mut h = crop_tool();
    let img = h.app.image_rect.expect("the photo on screen");
    let canvas = h.app.canvas_rect.expect("the canvas");
    let corner = canvas.right_bottom() - egui::vec2(4.0, 4.0);
    let (x, y) = ((corner.x - img.left()) / img.width(), (corner.y - img.top()) / img.height());
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": x, "y": y - 0.1}, {"kind": "drag", "x": x, "y": y - 0.05}, {"kind": "drag", "x": x, "y": y}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let readout = h.app.widgets.iter().find(|(w, _)| w == "cropAngleReadout").map(|(_, r)| *r).expect("the readout while rotating");
    assert!(canvas.contains_rect(readout), "{readout:?} in {canvas:?}");
}

/// Dragging the angle field changes the angle as one undo step, like the slider.
#[test]
fn dragging_the_angle_field_is_one_undo_step() {
    let mut h = crop_tool();
    let undo = h.app.session.undo.len();
    let r = h.request("ui.dragWidget", json!({"id": "cropAngleField", "dx": 40.0, "dy": 0.0, "steps": 6}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(angle(&h) > 0.0, "dragged right: {}", angle(&h));
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step for the whole drag");
}

/// Typing into the Angle field one key at a time applies the angle once, on Return: no undo step
/// or render per keystroke, no half-typed angles (-3° on the way to -3.25°).
#[test]
fn typing_in_the_angle_field_applies_on_return() {
    let mut h = crop_tool();
    let r = h.request("ui.clickWidget", json!({"id": "cropAngleField"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    for key in ["a", "Backspace"] {
        let r = h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    let (before, undo) = (angle(&h), h.app.session.undo.len());
    for ch in ["-", "3", ".", "2", "5"] {
        let r = h.request("ui.text", json!({"text": ch}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    assert_eq!((angle(&h), h.app.session.undo.len()), (before, undo), "nothing applied while typing");
    let r = h.request("ui.key", json!({"key": "Enter"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!((angle(&h) + 3.25).abs() < 1e-9, "applied on Return: {}", angle(&h));
    assert_eq!(h.app.session.undo.len(), undo + 1, "as one undo step");
}

/// Rotating in the margin beside a narrow photo keeps the readout next to the pointer: it is kept
/// on the canvas, not pinned to the photo's edge.
#[test]
fn the_readout_follows_the_pointer_in_the_margin() {
    let mut h = crop_tool();
    let img = h.app.image_rect.expect("the photo on screen");
    let canvas = h.app.canvas_rect.expect("the canvas");
    assert!(img.left() - canvas.left() > 150.0, "this test needs a margin beside the photo");
    let at = egui::pos2(canvas.left() + 40.0, img.center().y);
    let x = (at.x - img.left()) / img.width();
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": x, "y": 0.4}, {"kind": "drag", "x": x, "y": 0.45}, {"kind": "drag", "x": x, "y": 0.5}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let readout = h.app.widgets.iter().find(|(w, _)| w == "cropAngleReadout").map(|(_, r)| *r).expect("the readout while rotating");
    assert!(readout.distance_to_pos(at) < 60.0, "{readout:?} near {at:?}");
}

/// Placing the readout never panics, whatever the bounds (a NaN or an empty rect).
#[test]
fn placing_the_readout_never_panics() {
    use crate::panels::detail::readout_rect;
    let size = egui::vec2(50.0, 20.0);
    for bounds in [egui::Rect::NAN, egui::Rect::NOTHING, egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0))] {
        let r = readout_rect(egui::pos2(5.0, 5.0), size, bounds);
        assert_eq!(r.size(), size, "{bounds:?}");
    }
    let r = readout_rect(egui::pos2(f32::NAN, 5.0), size, egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0)));
    assert_eq!(r.size(), size);
}

/// Esc in the Angle field gives up the typed angle and keeps the old one, as in the slider's value.
#[test]
fn escape_in_the_angle_field_keeps_the_angle() {
    let mut h = crop_tool();
    let r = h.request("ui.clickWidget", json!({"id": "cropAngleField"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    for key in ["a", "Backspace"] {
        let r = h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    let r = h.request("ui.text", json!({"text": "7"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let (before, undo) = (angle(&h), h.app.session.undo.len());
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!((angle(&h), h.app.session.undo.len()), (before, undo), "Esc applied nothing");
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Crop, "and only left the field");
}

/// Leaving the Angle field another way than Esc (Tab here) applies the typed angle, as Return does.
#[test]
fn leaving_the_angle_field_applies_the_angle() {
    let mut h = crop_tool();
    let r = h.request("ui.clickWidget", json!({"id": "cropAngleField"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    for key in ["a", "Backspace"] {
        let r = h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    let r = h.request("ui.text", json!({"text": "4.5"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let undo = h.app.session.undo.len();
    let r = h.request("ui.key", json!({"key": "Tab"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!((angle(&h) - 4.5).abs() < 1e-9, "applied on leaving: {}", angle(&h));
    assert_eq!(h.app.session.undo.len(), undo + 1);
}

/// A move or resize drag keeps its own pointer when it runs past the box (the box stops at the
/// photo's edge): the rotation glyph would say the drag rotates.
#[test]
fn moving_the_box_never_shows_rotation() {
    let mut h = crop_tool();
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": 0.5, "y": 0.5}, {"kind": "drag", "x": 0.8, "y": 0.5}, {"kind": "drag", "x": 1.2, "y": 0.5}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.last_cursor, egui::CursorIcon::Move, "still moving");
    assert!(!has(&h, "cropRotateCursor"));
    assert_eq!(angle(&h), 0.0, "nothing rotated");
}

/// Closing the crop tool in the middle of a rotation (Return) leaves nothing behind: reopened,
/// the box shows the move pointer and no angle.
#[test]
fn a_rotation_cut_short_leaves_nothing_behind() {
    let mut h = crop_tool();
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": -0.05, "y": 0.2}, {"kind": "drag", "x": -0.05, "y": 0.25}, {"kind": "drag", "x": -0.05, "y": 0.3}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, "cropAngleReadout"), "rotating");
    for (method, params) in [
        ("engine.execute", json!({"command": "tool.done"})),
        ("ui.pointer", json!({"events": [{"kind": "up", "x": -0.05, "y": 0.3}]})),
        ("engine.execute", json!({"command": "panel.crop"})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    let img = h.app.image_rect.expect("the photo on screen");
    hover(&mut h, img.center());
    assert_eq!(h.last_cursor, egui::CursorIcon::Move);
    assert!(!has(&h, "cropAngleReadout"), "no angle");
    assert!(!has(&h, "cropRotateCursor"), "no rotation glyph");
}

/// The Angle field reads a typed angle as the Straighten value does: a decimal comma, a degree
/// sign, and an angle past ±45° held at the limit.
#[test]
fn the_angle_field_reads_angles_as_people_type_them() {
    let mut h = crop_tool();
    for (typed, want) in [("-3,25", -3.25), ("1.5°", 1.5), ("60", 45.0)] {
        let r = h.request("ui.clickWidget", json!({"id": "cropAngleField"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        for key in ["a", "Backspace"] {
            let r = h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T);
            assert_eq!(r["ok"], true, "{r}");
        }
        let r = h.request("ui.text", json!({"text": typed}), T);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("ui.key", json!({"key": "Enter"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!((angle(&h) - want).abs() < 1e-9, "{typed:?}: want {want}, got {}", angle(&h));
    }
}

/// The Angle field writes an angle as the readout does: a sign on a turned angle.
#[test]
fn the_angle_field_writes_the_angle_like_the_readout() {
    let mut h = crop_tool();
    let r = h.request("engine.execute", json!({"command": "crop.straighten", "params": {"angle": 2.5}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let texts = h.painted_text();
    // the field's number follows its label (its "°" is painted on its own)
    let field = texts.iter().position(|t| t == "Angle").and_then(|i| texts.get(i + 1));
    assert_eq!(field.map(String::as_str), Some("+2.50"), "{texts:?}");
}

/// Issue #731: the dimming outside a rotated crop box stays outside it. The photo is shown
/// unrotated under the box, so a pixel inside the box at -45° looks exactly as it does at 0°,
/// where the box is the whole photo and nothing is dimmed; the photo's corner, outside the
/// box, is darker. (The dimming was four quads joining photo and box corners, which crossed the
/// box once rotated: dimmed wedges inside it, doubly dark ones outside, growing with the angle.)
#[test]
fn rotating_the_crop_dims_nothing_inside_the_box() {
    let mut h = crop_tool();
    let plain = h.paint();
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "crop.angle", "value": -45.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(angle(&h), -45.0);
    let rotated = h.paint();
    let img = h.app.image_rect.expect("the photo on screen");
    let corner = |i: usize| h.app.widgets.iter().find(|(w, _)| *w == format!("cropHandle:{i}")).map(|(_, r)| r.center()).expect("a corner handle");
    let c = [corner(0), corner(1), corner(2), corner(3)];
    let at = |u: f32, v: f32| c[0] + (c[1] - c[0]) * u + (c[3] - c[0]) * v;
    let pixel = |im: &egui::ColorImage, p: egui::Pos2| im.pixels[p.y.round() as usize * im.size[0] + p.x.round() as usize];
    let mut checked = 0;
    for u in [0.15, 0.25, 0.5, 0.75, 0.85] {
        for v in [0.15, 0.25, 0.5, 0.75, 0.85] {
            let p = at(u, v);
            // clear of the 0° guide lines (thirds of the photo) and of the photo's edge
            let thirds = |t: f32, lo: f32, len: f32| [1.0 / 3.0, 2.0 / 3.0].iter().any(|f| (t - (lo + f * len)).abs() < 3.0);
            if !img.shrink(4.0).contains(p) || thirds(p.x, img.left(), img.width()) || thirds(p.y, img.top(), img.height()) {
                continue;
            }
            assert_eq!(pixel(&rotated, p), pixel(&plain, p), "inside the box at ({u}, {v}) = {p:?}");
            checked += 1;
        }
    }
    assert!(checked >= 12, "{checked} points inside the box compared");
    let corner_px = img.left_top() + egui::vec2(6.0, 6.0);
    let lum = |c: egui::Color32| u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b());
    assert!(lum(pixel(&rotated, corner_px)) < lum(pixel(&plain, corner_px)), "the photo's corner, outside the box, is dimmed");
}
