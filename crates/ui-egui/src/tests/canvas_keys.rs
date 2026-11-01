//! The canvas tools by keyboard: crop handles, spot removals, gradient handles and mask pins are
//! reached with Tab or focus, moved with the arrow keys (Shift: further), one undo step a press.

use std::time::Duration;

use lightcraft_develop::MaskShape;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services, canvas_keys};

const T: Duration = Duration::from_secs(20);

fn detail(panel: &str) -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // pins on whatever the pointer does: these tests move handles with the keyboard, and a headless
    // pointer is never over the photo, where the Tool Overlay's default Auto shows them
    h.app.ui.tool_overlay = crate::state::ToolOverlay::Always;
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": panel}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn develop(h: &Headless) -> lightcraft_develop::DevelopSettings {
    let id = h.app.session.active().expect("active photo");
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn focus(h: &mut Headless, key: &str) {
    h.step();
    h.view.ctx.memory_mut(|m| m.request_focus(canvas_keys::id(key)));
    h.step();
    h.step();
    assert_eq!(h.view.ctx.memory(|m| m.focused()), Some(canvas_keys::id(key)), "{key} takes the focus");
}

fn key(h: &mut Headless, name: &str, shift: bool) {
    let r = h.request("ui.key", json!({"key": name, "shift": shift}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

#[test]
fn crop_corner_moves_with_the_arrows_and_shift_moves_further() {
    let mut h = detail("panel.crop");
    let photo = h.app.session.active();
    let r0 = develop(&h).crop.geometry.rect;
    focus(&mut h, "crop:2");
    key(&mut h, "left", false);
    let r1 = develop(&h).crop.geometry.rect;
    assert!(r1.x1 < r0.x1 - 0.001 && (r1.x0 - r0.x0).abs() < 1e-9, "{r0:?} -> {r1:?}");
    key(&mut h, "up", true);
    let r2 = develop(&h).crop.geometry.rect;
    assert!(r2.y1 < r1.y1 - 0.03, "Shift is a big step: {r1:?} -> {r2:?}");
    // the arrows moved the handle, not to another photo
    assert_eq!(h.app.session.active(), photo);
    // one undo step a press
    exec(&mut h, "edit.undo", json!({}));
    assert!((develop(&h).crop.geometry.rect.y1 - r1.y1).abs() < 1e-9);
    // the whole box moves with the box's handle
    focus(&mut h, "crop:8");
    let before = develop(&h).crop.geometry.rect;
    key(&mut h, "right", false);
    let after = develop(&h).crop.geometry.rect;
    assert!((after.x1 - after.x0 - (before.x1 - before.x0)).abs() < 1e-6, "{before:?} -> {after:?}");
}

#[test]
fn tab_reaches_the_crop_handles() {
    let mut h = detail("panel.crop");
    let handles: Vec<egui::Id> = (0..9).map(|i| canvas_keys::id(&format!("crop:{i}"))).collect();
    let mut reached = false;
    for _ in 0..200 {
        key(&mut h, "tab", false);
        if h.view.ctx.memory(|m| m.focused()).is_some_and(|f| handles.contains(&f)) {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab reaches a crop handle");
}

#[test]
fn spots_are_added_and_moved_by_keyboard() {
    let mut h = detail("panel.remove");
    assert!(develop(&h).spots.is_empty());
    focus(&mut h, "spot:new");
    key(&mut h, "right", true);
    key(&mut h, "enter", false);
    let spots = develop(&h).spots;
    assert_eq!(spots.len(), 1, "Return adds a spot");
    let p = spots[0].points[0];
    assert!((p.x - 0.55).abs() < 1e-6 && (p.y - 0.5).abs() < 1e-6, "{p:?}");
    focus(&mut h, "spot:0:target");
    key(&mut h, "down", false);
    let q = develop(&h).spots[0].points[0];
    assert!((q.y - p.y - canvas_keys::STEP).abs() < 1e-6, "{p:?} -> {q:?}");
    if develop(&h).spots[0].source_offset.is_some() {
        let o = develop(&h).spots[0].source_offset;
        focus(&mut h, "spot:0:source");
        key(&mut h, "left", false);
        assert_ne!(develop(&h).spots[0].source_offset, o, "the source moves");
    }
}

#[test]
fn gradient_handles_and_pins_move_by_keyboard() {
    let mut h = detail("panel.masking");
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.5, 0.2], "end": [0.5, 0.7]}));
    h.step();
    let mid = h.app.session.active_mask.expect("a selected mask");
    focus(&mut h, &format!("mask:{mid}:0:2"));
    key(&mut h, "down", false);
    let MaskShape::Linear { start, end } = develop(&h).masks[0].components[0].shape.clone() else { panic!("linear") };
    assert!((end.y - 0.7 - canvas_keys::STEP).abs() < 1e-3 && (start.y - 0.2).abs() < 1e-6, "{start:?} {end:?}");
    // the pin moves the whole gradient
    let r = h.request("ui.set", json!({"maskPins": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    focus(&mut h, &format!("mask:{mid}:0:0"));
    key(&mut h, "right", true);
    let MaskShape::Linear { start: s2, end: e2 } = develop(&h).masks[0].components[0].shape.clone() else { panic!("linear") };
    assert!((s2.x - start.x - canvas_keys::BIG_STEP).abs() < 1e-3 && (e2.x - end.x - canvas_keys::BIG_STEP).abs() < 1e-3);
    // a radial gradient: its right edge widens it
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.2, "ry": 0.1}));
    h.step();
    let rid = h.app.session.active_mask.expect("the radial mask");
    let rx = |h: &Headless| match develop(h).masks.iter().find(|m| m.id == rid).map(|m| m.components[0].shape.clone()) {
        Some(MaskShape::Radial { rx, .. }) => rx,
        other => panic!("{other:?}"),
    };
    let rx0 = rx(&h);
    focus(&mut h, &format!("mask:{rid}:0:1"));
    key(&mut h, "right", true);
    assert!(rx(&h) > rx0, "{rx0} -> {}", rx(&h));
}
