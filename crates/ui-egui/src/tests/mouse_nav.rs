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

/// Whatever keeps the arrow keys keeps the buttons too: a notice such as the quit prompt, the
/// blocking library window, shortcut recording and a text field with the keyboard. Each case is
/// checked with the buttons doing nothing while it holds, then stepping again once it is gone, so
/// the case really was in force when the button was pressed (issue #3303).
#[test]
fn whatever_keeps_the_arrow_keys_keeps_the_buttons() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let ids = h.app.session.visible_cloned();
    let first = ids[2];
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0], "active": first.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.active(), Some(first));
    // with nothing in the way Forward steps on and Back comes back, so each case below starts on `first`
    let steps_again = |h: &mut Headless, case: &str| {
        press(h, egui::PointerButton::Extra2);
        assert_ne!(h.app.session.active(), Some(first), "{case}: once it is gone, Forward steps again");
        press(h, egui::PointerButton::Extra1);
        assert_eq!(h.app.session.active(), Some(first), "{case}: and Back comes back");
    };

    // a notice: the quit prompt owns every key
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.settle(SETTLE);
    assert!(crate::panels::notices::blocks_input(&h.app));
    for button in [egui::PointerButton::Extra2, egui::PointerButton::Extra1] {
        press(&mut h, button);
        assert_eq!(h.app.session.active(), Some(first), "the quit prompt keeps {button:?}");
    }
    h.app.quit_prompt = None;
    h.settle(SETTLE);
    steps_again(&mut h, "quit prompt");

    // the blocking library window (here the launch list)
    h.app.library_problem = Some(crate::panels::library_problem::LibraryProblem::choose_later(&h.app.ui.settings));
    h.settle(SETTLE);
    assert!(crate::panels::library_problem::blocks_dialog_input(&h.app));
    for button in [egui::PointerButton::Extra2, egui::PointerButton::Extra1] {
        press(&mut h, button);
        assert_eq!(h.app.session.active(), Some(first), "the library window keeps {button:?}");
    }
    h.app.library_problem = None;
    h.settle(SETTLE);
    steps_again(&mut h, "library window");

    // shortcut recording, in the Keyboard Shortcuts window
    h.app.run("app.shortcuts", json!({})).unwrap();
    h.settle(SETTLE);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Shortcuts)));
    h.app.recording_shortcut = Some("view.survey".into());
    for button in [egui::PointerButton::Extra2, egui::PointerButton::Extra1] {
        press(&mut h, button);
        assert_eq!(h.app.session.active(), Some(first), "shortcut recording keeps {button:?}");
        assert!(h.app.recording_shortcut.is_some(), "{button:?} does not end the recording");
    }
    h.app.recording_shortcut = None;
    h.app.ui.dialog = None;
    h.settle(SETTLE);
    steps_again(&mut h, "shortcut recording");

    // a text field with the keyboard: the search field
    h.app.ui.focus_search = true;
    h.settle(SETTLE);
    let search = crate::text_field::id("field:search");
    assert!(h.view.ctx.memory(|m| m.has_focus(search)), "the search field has the keyboard");
    for button in [egui::PointerButton::Extra2, egui::PointerButton::Extra1] {
        press(&mut h, button);
        assert_eq!(h.app.session.active(), Some(first), "a focused text field keeps {button:?}");
        assert!(h.view.ctx.memory(|m| m.has_focus(search)), "{button:?} leaves the search field focused");
    }
    h.view.ctx.memory_mut(|m| m.surrender_focus(search));
    h.settle(SETTLE);
    steps_again(&mut h, "text field");
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

/// Back / Forward pressed while a slider is being dragged do nothing: stepping to another photo mid-drag would
/// hand the rest of the drag to that photo (review of #3324). Once the drag ends they step again.
#[test]
fn the_buttons_wait_for_a_slider_drag_to_end() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let ids = h.app.session.visible_cloned();
    let (first, second) = (ids[2], ids[3]);
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0], "active": first.0}}), T);
    h.request("ui.set", json!({"view": "detail"}), T);
    h.settle(SETTLE);
    let at = h.app.widgets.iter().find(|(id, _)| id == "slider:light.exposure").map(|(_, r)| r.center()).expect("the Exposure slider");
    let button = |button, pos, pressed| egui::Event::PointerButton { pos, button, pressed, modifiers: Default::default() };
    // the drag: pressed on the slider and moving
    h.app.synthetic.extend([egui::Event::PointerMoved(at), button(egui::PointerButton::Primary, at, true)]);
    h.settle(SETTLE);
    h.app.synthetic.push(egui::Event::PointerMoved(at + egui::vec2(20.0, 0.0)));
    h.settle(SETTLE);
    let to = at + egui::vec2(40.0, 0.0);
    h.app.synthetic.extend([
        egui::Event::PointerMoved(to),
        button(egui::PointerButton::Extra2, to, true),
        button(egui::PointerButton::Extra2, to, false),
    ]);
    h.settle(SETTLE);
    assert_eq!(h.app.session.active(), Some(first), "Forward mid-drag keeps the photo");
    h.app.synthetic.push(button(egui::PointerButton::Primary, to, false));
    h.settle(SETTLE);
    press(&mut h, egui::PointerButton::Extra2);
    assert_eq!(h.app.session.active(), Some(second), "after the drag Forward steps again");
}
