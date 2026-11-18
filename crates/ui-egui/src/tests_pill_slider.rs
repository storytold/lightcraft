//! The pill slider (fork-owned, the Modern layout's): a tap leaves the value alone, a drag anywhere on
//! the bar moves it by the drag's travel over the bar, a double-click resets it.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::theme::SliderLayout;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
const ID: &str = "light.shadows";

fn develop() -> Headless {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.settings.slider_layout = SliderLayout::Pill;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"module": "develop", "view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn pill(h: &Headless) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| *w == format!("slider:{ID}")).map(|(_, r)| *r).expect("the slider")
}

fn shadows(h: &Headless) -> f64 {
    let id = h.app.session.active().expect("a photo");
    h.app.session.develop_of(id).expect("its settings").light.shadows
}

fn send(h: &mut Headless, e: egui::Event) {
    h.app.synthetic.push(e);
    h.step();
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
}

fn glide(h: &mut Headless, a: egui::Pos2, b: egui::Pos2, n: usize) {
    for i in 1..=n {
        send(h, egui::Event::PointerMoved(a + (b - a) * (i as f32 / n as f32)));
    }
}

#[test]
fn a_tap_leaves_the_value_a_drag_moves_it_by_its_travel_and_a_double_click_resets_it() {
    let mut h = develop();
    let r = pill(&h);
    assert!(r.height() >= 36.0, "finger-sized: {r:?}");
    let before = shadows(&h);
    // a tap near the right end: no jump
    let near_end = egui::pos2(r.right() - 90.0, r.center().y);
    send(&mut h, egui::Event::PointerMoved(near_end));
    send(&mut h, button(near_end, true));
    send(&mut h, button(near_end, false));
    h.settle(SETTLE);
    assert_eq!(shadows(&h), before, "a tap doesn't change it");
    // a drag from the left part, a quarter of the bar to the right: a quarter of the range up
    let start = egui::pos2(r.left() + 30.0, r.center().y);
    let end = start + egui::vec2(r.width() * 0.25, 0.0);
    send(&mut h, egui::Event::PointerMoved(start));
    send(&mut h, button(start, true));
    glide(&mut h, start, end, 20);
    send(&mut h, button(end, false));
    h.settle(SETTLE);
    let spec = lcc_develop::controls::find(ID).expect("Shadows");
    let want = (before + (spec.max - spec.min) * 0.25).min(spec.max);
    assert!((shadows(&h) - want).abs() <= 3.0, "moved by the drag: {} (want about {want})", shadows(&h));
    // a double-click resets it
    assert_ne!(shadows(&h), 0.0);
    let at = pill(&h).center();
    send(&mut h, egui::Event::PointerMoved(at));
    for _ in 0..2 {
        send(&mut h, button(at, true));
        send(&mut h, button(at, false));
    }
    h.settle(SETTLE);
    assert_eq!(shadows(&h), 0.0, "a double-click resets it");
}
