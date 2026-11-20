//! A middle-button drag (the mouse wheel pressed) pans the zoomed photo in Detail whatever tool is
//! active: the healing brush doesn't paint meanwhile, and the hand cursor shows.

use std::time::Duration;

use egui::{Event, PointerButton, Pos2, vec2};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn spots(h: &Headless) -> usize {
    let id = h.app.session.active().expect("active photo");
    h.app.session.develop_of(id).unwrap_or_default().spots.len()
}

/// Press `button` at `from`, move to `to` over a few frames and release; the cursor seen mid-drag.
fn drag(h: &mut Headless, button: PointerButton, from: Pos2, to: Pos2) -> egui::CursorIcon {
    let modifiers = egui::Modifiers::NONE;
    h.app.synthetic.push(Event::PointerMoved(from));
    h.app.synthetic.push(Event::PointerButton { pos: from, button, pressed: true, modifiers });
    h.step();
    for i in 1..=6 {
        h.app.synthetic.push(Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    let cursor = h.last_cursor;
    h.app.synthetic.push(Event::PointerButton { pos: to, button, pressed: false, modifiers });
    h.step();
    h.step();
    cursor
}

#[test]
fn a_middle_drag_pans_with_the_healing_brush_and_adds_no_spot() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for (method, params) in [
        ("ui.set", json!({"view": "detail"})),
        ("engine.execute", json!({"command": "panel.remove"})),
        ("engine.execute", json!({"command": "view.zoom100"})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }
    h.step();
    let at = h.app.image_rect.expect("the photo is shown").center();
    let pan = h.app.ui.pan;
    let cursor = drag(&mut h, PointerButton::Middle, at, at + vec2(-150.0, -100.0));
    assert_eq!(cursor, egui::CursorIcon::Grabbing, "the hand shows while panning");
    assert_ne!(h.app.ui.pan, pan, "the middle drag pans");
    assert_eq!(spots(&h), 0, "the healing brush doesn't paint during a middle drag");
    // the left button still paints a spot, and doesn't pan
    let pan = h.app.ui.pan;
    let at = h.app.image_rect.expect("the photo is shown").center();
    drag(&mut h, PointerButton::Primary, at, at + vec2(40.0, 0.0));
    assert_eq!(spots(&h), 1, "a left drag paints a spot");
    assert_eq!(h.app.ui.pan, pan);
}
