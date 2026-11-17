//! Headless test of the Alt-drag previews (issue #937) through real input: a slider drag, Alt
//! (Option) pressed and released during it, and the loupe's pixels at each step.

use std::time::Duration;

use egui::{Event, Modifiers, PointerButton};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn detail() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 1400.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    if !h.app.ui.section_open("light") {
        h.app.ui.toggle_section("light");
    }
    h.app.renderer.keep_pixels = true;
    h.settle(SETTLE);
    h
}

/// Share of the loupe render's pixels whose channels are all 0 or 255 (a clipping view).
fn binary_share(h: &Headless) -> f64 {
    let t = h.app.renderer.textures.get(&crate::render::Slot::Main).expect("loupe render");
    let px = &t.pixels.as_ref().expect("pixels kept").pixels;
    let binary = px.iter().filter(|c| [c.r(), c.g(), c.b()].iter().all(|v| *v == 0 || *v == 255)).count();
    binary as f64 / px.len().max(1) as f64
}

#[test]
fn alt_during_a_blacks_drag_shows_shadow_clipping_until_released() {
    let mut h = detail();
    assert!(binary_share(&h) < 0.5, "the photo itself is no clipping view");
    let track = h.app.widgets.iter().find(|(w, _)| w == "slider:light.blacks").map(|(_, r)| *r).expect("Blacks slider on screen");
    let from = track.center();
    let before = h.app.session.develop_of(h.app.session.active().expect("photo")).expect("develop").light.blacks;
    // a plain drag to the left, button held: the photo as usual
    h.app.synthetic.push(Event::PointerMoved(from));
    h.app.synthetic.push(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    for i in 1..=8 {
        h.app.synthetic.push(Event::PointerMoved(from - egui::vec2(6.0 * i as f32, 0.0)));
    }
    h.settle(SETTLE);
    assert_eq!(h.app.ui.dragging_control.as_deref(), Some("light.blacks"));
    assert!(binary_share(&h) < 0.5, "no preview without Alt");
    // Alt pressed mid-drag: the shadow clipping view
    h.app.synthetic.push(Event::ModifiersChanged(Modifiers::ALT));
    h.settle(SETTLE);
    assert!(binary_share(&h) > 0.99, "the clipping view: {}", binary_share(&h));
    // Alt released: the photo again, the drag still on
    h.app.synthetic.push(Event::ModifiersChanged(Modifiers::NONE));
    h.settle(SETTLE);
    assert!(binary_share(&h) < 0.5, "the photo is back");
    h.app.synthetic.push(Event::PointerButton {
        pos: from - egui::vec2(48.0, 0.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.settle(SETTLE);
    assert_eq!(h.app.ui.dragging_control, None);
    let after = h.app.session.develop_of(h.app.session.active().expect("photo")).expect("develop").light.blacks;
    assert!(after < before, "the drag itself still moved Blacks: {before} → {after}");
}
