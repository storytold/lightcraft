//! A slider's thumb stays under the pointer while it is dragged, as in Lightroom Classic (fork-owned): past an end
//! it waits there until the pointer comes back over it, and a thumb grabbed off-centre doesn't jump.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
const ID: &str = "light.blacks";

fn develop() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"module": "develop", "view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn track(h: &Headless) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| *w == format!("slider:{ID}")).map(|(_, r)| *r).expect("the slider")
}

fn blacks(h: &Headless) -> f64 {
    let id = h.app.session.active().expect("a photo");
    h.app.session.develop_of(id).expect("its settings").light.blacks
}

/// One pointer event a frame, as a real drag arrives.
fn send(h: &mut Headless, e: egui::Event) {
    h.app.synthetic.push(e);
    h.step();
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
}

/// Moves the pointer from `a` to `b` in `n` frames.
fn glide(h: &mut Headless, a: egui::Pos2, b: egui::Pos2, n: usize) {
    for i in 1..=n {
        send(h, egui::Event::PointerMoved(a + (b - a) * (i as f32 / n as f32)));
    }
}

fn spec() -> lcc_develop::ControlSpec {
    *lcc_develop::controls::find(ID).expect("the Blacks control")
}

#[test]
fn the_thumb_stays_under_the_pointer_past_the_end_off_centre_and_lags_with_acceleration() {
    let mut h = develop();
    let r = track(&h);
    let s = spec();
    let thumb = egui::pos2(r.left() + s.position(blacks(&h)) as f32 * r.width(), r.center().y);
    send(&mut h, egui::Event::PointerMoved(thumb));
    send(&mut h, button(thumb, true));
    // slowly, far past the left end
    let far = egui::pos2(r.left() - 200.0, r.center().y);
    glide(&mut h, thumb, far, 40);
    assert_eq!(blacks(&h), s.min, "at the end");
    // back to a quarter of the track: the thumb is there, not beyond it
    let quarter = egui::pos2(r.left() + 0.25 * r.width(), r.center().y);
    glide(&mut h, far, quarter, 6);
    send(&mut h, button(quarter, false));
    h.settle(SETTLE);
    let want = s.value_at(0.25);
    assert!((blacks(&h) - want).abs() <= s.step.max(1.0), "under the pointer: {} (want {want})", blacks(&h));
    // a thumb grabbed off-centre moves with the pointer without jumping
    let before = blacks(&h);
    let centre = r.left() + s.position(before) as f32 * r.width();
    let grab = egui::pos2(centre + 4.0, r.center().y);
    send(&mut h, egui::Event::PointerMoved(grab));
    send(&mut h, button(grab, true));
    let to = grab + egui::vec2(0.2 * r.width(), 0.0);
    glide(&mut h, grab, to, 30);
    send(&mut h, button(to, false));
    h.settle(SETTLE);
    let want = s.value_at(s.position(before) + 0.2);
    assert!((blacks(&h) - want).abs() <= s.step.max(1.0), "moved with the pointer: {} (want {want})", blacks(&h));
    // with acceleration on the slider, a slow drag moves the thumb less than the pointer
    h.app.ui.settings.slider_control = crate::theme::SliderControl::AccelSlider;
    h.step();
    let before = blacks(&h);
    let thumb = egui::pos2(r.left() + s.position(before) as f32 * r.width(), r.center().y);
    send(&mut h, egui::Event::PointerMoved(thumb));
    send(&mut h, button(thumb, true));
    let to = thumb + egui::vec2(0.2 * r.width(), 0.0);
    glide(&mut h, thumb, to, 200);
    send(&mut h, button(to, false));
    h.settle(SETTLE);
    let under = s.value_at(s.position(before) + 0.2);
    assert!(blacks(&h) > before && blacks(&h) < under - s.step.max(1.0), "lags the pointer: {} (pointer at {under})", blacks(&h));
}
