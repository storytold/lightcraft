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

/// Share of the loupe render's pixels painted exactly `c`.
fn share_of(h: &Headless, c: [u8; 3]) -> f64 {
    let t = h.app.renderer.textures.get(&crate::render::Slot::Main).expect("loupe render");
    let px = &t.pixels.as_ref().expect("pixels kept").pixels;
    let n = px.iter().filter(|p| [p.r(), p.g(), p.b()] == c).count();
    n as f64 / px.len().max(1) as f64
}

/// Issue #3620: J (clipping warnings) paints the clipped pixels themselves, red where a channel
/// is blown to white and blue where one is crushed to black, not just a frame around the photo.
#[test]
fn j_paints_clipped_highlights_red_and_clipped_shadows_blue() {
    let mut h = detail();
    let set = |h: &mut Headless, ev: f64| {
        let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": ev}}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    };
    set(&mut h, 5.0);
    let (red, blue) = ([255, 0, 0], [0, 0, 255]);
    assert!(share_of(&h, red) < 0.1, "without J the blown photo is not painted red: {}", share_of(&h, red));
    let r = h.request("ui.key", json!({"key": "J"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.show_clipping);
    assert!(share_of(&h, red) > 0.5, "blown highlights shown in red: {}", share_of(&h, red));
    set(&mut h, -5.0);
    // the demo photo's shadows don't reach black on Exposure alone: Blacks crushes them
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.blacks", "value": -100.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(share_of(&h, blue) > 0.01, "crushed shadows shown in blue: {}", share_of(&h, blue));
    assert!(share_of(&h, red) < 0.01, "nothing blown at -5 EV: {}", share_of(&h, red));
}
