//! Headless tests of the mouse's Back and Forward buttons (issue #3303).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{Dialog, ViewMode};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Press and release one mouse button over the photo, with real egui pointer events.
fn press(h: &mut Headless, button: egui::PointerButton) {
    let pos = h.app.canvas_rect.map_or(egui::pos2(600.0, 400.0), |r| r.center());
    h.app.synthetic.push(egui::Event::PointerMoved(pos));
    h.app.synthetic.push(egui::Event::PointerButton { pos, button, pressed: true, modifiers: Default::default() });
    h.app.synthetic.push(egui::Event::PointerButton { pos, button, pressed: false, modifiers: Default::default() });
    h.settle(SETTLE);
}

/// Forward (Extra2) shows the next photo and Back (Extra1) the previous one, as → and ← do; a
/// dialog keeps them, as it keeps the keys.
#[test]
fn mouse_back_and_forward_buttons_step_through_the_photos() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let ids = h.app.session.visible_cloned();
    let (first, second) = (ids[2], ids[3]);
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0], "active": first.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!((h.app.ui.view, h.app.session.active()), (ViewMode::Detail, Some(first)));

    press(&mut h, egui::PointerButton::Extra2);
    assert_eq!(h.app.session.active(), Some(second), "Forward shows the next photo");
    press(&mut h, egui::PointerButton::Extra1);
    assert_eq!(h.app.session.active(), Some(first), "Back shows the previous one");

    // an open dialog keeps the buttons
    h.app.ui.dialog = Some(Dialog::About);
    press(&mut h, egui::PointerButton::Extra2);
    assert_eq!(h.app.session.active(), Some(first));
}

/// Each press steps once, in the order pressed, also when several arrive in one frame's input
/// (a fast double press, or a frame that took long): two Forward presses show the photo two
/// on, and on the last photo Forward (which stays there) then Back shows the one before it
/// (issue #3303).
#[test]
fn presses_in_one_frame_step_once_each_in_order() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let ids = h.app.session.visible_cloned();
    let (first, third) = (ids[2], ids[4]);
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0], "active": first.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let pos = h.app.canvas_rect.map_or(egui::pos2(600.0, 400.0), |r| r.center());
    // the host's input for one frame, as a real host hands it over (`synthetic` would spread the
    // presses over several frames)
    let one_frame = |h: &mut Headless, buttons: &[egui::PointerButton]| {
        h.events.push(egui::Event::PointerMoved(pos));
        for button in buttons {
            for pressed in [true, false] {
                h.events.push(egui::Event::PointerButton { pos, button: *button, pressed, modifiers: Default::default() });
            }
        }
        h.step();
        h.settle(SETTLE);
    };
    one_frame(&mut h, &[egui::PointerButton::Extra2, egui::PointerButton::Extra2]);
    assert_eq!(h.app.session.active(), Some(third), "two Forward presses show the photo two on");
    let (last, before_last) = (ids[ids.len() - 1], ids[ids.len() - 2]);
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [last.0], "active": last.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    one_frame(&mut h, &[egui::PointerButton::Extra2]);
    assert_eq!(h.app.session.active(), Some(last), "Forward on the last photo stays there");
    one_frame(&mut h, &[egui::PointerButton::Extra2, egui::PointerButton::Extra1]);
    assert_eq!(h.app.session.active(), Some(before_last), "Forward, then Back");
}

/// The develop settings of photo `id`.
fn develop(h: &Headless, id: lightcraft_catalog::PhotoId) -> lightcraft_develop::DevelopSettings {
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

/// Forward pressed over an Edit slider shows the next photo and moves no slider. egui never sees
/// the button: the slider can't take a press that wobbles a little before the release as a drag
/// to the pointer, on the photo navigation switches to (issue #3303).
#[test]
fn forward_over_a_slider_changes_no_develop_setting() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let ids = h.app.session.visible_cloned();
    let (first, second) = (ids[2], ids[3]);
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0], "active": first.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let track = h.app.widgets.iter().rev().find(|(w, _)| w == "slider:light.exposure").map(|(_, r)| *r).expect("the Exposure slider is on screen");
    // well right of the slider's middle (0 EV): a drag the slider took would set about +3 EV
    let at = egui::pos2(track.left() + track.width() * 0.8, track.center().y);
    let (before_first, before_second) = (develop(&h, first), develop(&h, second));
    for (button, shown) in [(egui::PointerButton::Extra2, second), (egui::PointerButton::Extra1, first)] {
        let moved = at + egui::vec2(12.0, 0.0);
        h.app.synthetic.push(egui::Event::PointerMoved(at));
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed: true, modifiers: Default::default() });
        h.app.synthetic.push(egui::Event::PointerMoved(moved));
        h.app.synthetic.push(egui::Event::PointerButton { pos: moved, button, pressed: false, modifiers: Default::default() });
        h.settle(SETTLE);
        assert_eq!(h.app.session.active(), Some(shown), "{button:?} shows the photo next to it");
        assert_eq!(develop(&h, first), before_first, "{button:?}: the first photo keeps its settings");
        assert_eq!(develop(&h, second), before_second, "{button:?}: the second photo keeps its settings");
    }
}
