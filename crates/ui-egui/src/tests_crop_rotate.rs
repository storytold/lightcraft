//! Rotating in the crop tool (issue #534): outside the crop box the pointer shows that a drag
//! rotates, a drag shows the angle as it changes, and the Straighten value takes an exact angle.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn crop_tool() -> Headless {
    let app = DacApp::new(dac_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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

fn hover(h: &mut Headless, p: egui::Pos2) {
    let r = h.request("ui.move", json!({"x": p.x, "y": p.y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
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
        h.painted_text().iter().any(|t| t == "Click to type a value"),
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
