//! Headless tests of Settings ▸ Interface ▸ Reduce motion (`view.reduceMotion`): the click-zoom lands
//! at once, egui's own easing stops, the activity bar's sweep stands still; with it off, all as before.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Zoom;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn detail() -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
    h.request("ui.set", json!({"view": "detail", "right": "none"}), T);
    h.settle(SETTLE);
    h
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    r["result"].clone()
}

/// Click the middle of the photo with real pointer events, stopping on the frame that handled the click.
fn click_photo(h: &mut Headless) {
    let pos = h.app.image_rect.expect("image").center();
    h.app.synthetic.extend([
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
    ]);
    while !h.app.synthetic.is_empty() {
        h.step();
    }
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0), "the click zoomed in");
}

#[test]
fn a_click_zoom_lands_on_the_next_frame_with_reduce_motion_on() {
    let mut h = detail();
    run(&mut h, "view.reduceMotion", json!({"on": true}));
    click_photo(&mut h);
    h.step();
    let next = h.app.image_rect.unwrap();
    assert!(!h.app.ui.zoom_anim, "the animation ended");
    h.settle(SETTLE);
    let settled = h.app.image_rect.unwrap();
    assert_eq!(next, settled, "the next frame already shows the target");
}

#[test]
fn a_click_zoom_still_animates_with_reduce_motion_off() {
    let mut h = detail();
    click_photo(&mut h);
    h.step();
    let next = h.app.image_rect.unwrap();
    assert!(h.app.ui.zoom_anim, "still tweening");
    h.settle(SETTLE);
    assert_ne!(next, h.app.image_rect.unwrap(), "an intermediate frame exists");
}

#[test]
fn z_zooms_at_once_with_reduce_motion_on() {
    let mut h = detail();
    run(&mut h, "view.reduceMotion", json!({"on": true}));
    h.request("ui.key", json!({"key": "z"}), T);
    h.step();
    let next = h.app.image_rect.unwrap();
    h.settle(SETTLE);
    assert!(!h.app.ui.zoom_anim);
    assert_eq!(next, h.app.image_rect.unwrap());
}

#[test]
fn the_setting_survives_saving_and_loading_the_ui_settings() {
    let mut h = detail();
    run(&mut h, "view.reduceMotion", json!({"on": true}));
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap().sanitized();
    assert_eq!(back.settings.reduce_motion, Some(true));
    assert_eq!(crate::state::AppSettings::default().reduce_motion, None, "follows the system by default");
    let mut old = serde_json::to_value(&h.app.ui).unwrap();
    old["settings"].as_object_mut().unwrap().remove("reduceMotion").expect("saved under its camelCase key");
    let loaded = serde_json::from_value::<crate::UiState>(old).unwrap().sanitized();
    assert_eq!(loaded.settings.reduce_motion, None, "state saved before the setting existed still loads");
}

#[test]
fn the_command_toggles_and_sets_the_setting() {
    let mut h = detail();
    assert_eq!(run(&mut h, "view.reduceMotion", json!({}))["on"], true);
    assert_eq!(h.app.ui.settings.reduce_motion, Some(true));
    assert_eq!(run(&mut h, "view.reduceMotion", json!({}))["on"], false);
    assert_eq!(run(&mut h, "view.reduceMotion", json!({"on": true}))["on"], true);
    assert_eq!(run(&mut h, "view.reduceMotion", json!({"on": true}))["on"], true, "set, not toggled");
    assert_eq!(run(&mut h, "view.reduceMotion", json!({"on": false}))["on"], false);
}

#[test]
fn the_command_refuses_params_that_are_not_true_or_false_and_changes_nothing() {
    let mut h = detail();
    run(&mut h, "view.reduceMotion", json!({"on": true}));
    for (params, says) in [
        (json!({"on": "false"}), "`on` must be true or false"),
        (json!({"on": 0}), "`on` must be true or false"),
        (json!({"follow": "yes"}), "`follow` must be true or false"),
        (json!({"follow": 1, "on": false}), "`follow` must be true or false"),
    ] {
        let r = h.request("engine.execute", json!({"command": "view.reduceMotion", "params": params}), T);
        assert_eq!(r["ok"], false, "{params}: {r}");
        assert!(r.to_string().contains(says), "{params}: {r}");
        assert_eq!(h.app.ui.settings.reduce_motion, Some(true), "{params} changed the setting");
    }
}

#[test]
fn egui_easing_stops_with_reduce_motion_on_and_returns_when_off() {
    let mut h = detail();
    let still = egui::style::ScrollAnimation::none();
    assert_eq!(h.view.ctx.global_style().animation_time, 0.08);
    assert_eq!(h.view.ctx.global_style().scroll_animation, egui::style::ScrollAnimation::default());
    run(&mut h, "view.reduceMotion", json!({"on": true}));
    assert_eq!(h.view.ctx.global_style().animation_time, 0.0);
    assert_eq!(h.view.ctx.global_style().scroll_animation, still, "scrolling to a photo jumps");
    // a theme change restyles egui: the easing stays off
    run(&mut h, "view.appearance", json!({"mode": "light"}));
    assert_eq!(h.view.ctx.global_style().animation_time, 0.0);
    assert_eq!(h.view.ctx.global_style().scroll_animation, still);
    run(&mut h, "view.reduceMotion", json!({"on": false}));
    assert_eq!(h.view.ctx.global_style().animation_time, 0.08);
    assert_eq!(h.view.ctx.global_style().scroll_animation, egui::style::ScrollAnimation::default());
}

#[test]
fn the_activity_bar_stands_still_with_reduce_motion_on() {
    use crate::progress::sweep_span;
    let still: Vec<_> = (0..120).map(|f| sweep_span(200.0, f as f64 / 60.0, true)).collect();
    assert!(still.windows(2).all(|w| w[0] == w[1]), "no movement between frames");
    assert!(still[0].1 > still[0].0, "but something is shown");
    let moving: Vec<_> = (0..120).map(|f| sweep_span(200.0, f as f64 / 60.0, false)).collect();
    assert!(moving.windows(2).any(|w| w[0] != w[1]), "it sweeps when motion is on");
}

/// A headless app whose system says `system` (shared, so a test can change it).
fn with_system(system: std::rc::Rc<std::cell::Cell<Option<bool>>>) -> Headless {
    let mut app = LightcraftApp::new(
        lightcraft_engine::Session::with_demo(),
        Services { png: None, system_reduce_motion: Some(Box::new(move || system.get())), ..Default::default() },
    );
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
    h.request("ui.set", json!({"view": "detail", "right": "none"}), T);
    h.settle(SETTLE);
    h
}

#[test]
fn the_system_setting_is_followed_until_the_user_chooses() {
    let system = std::rc::Rc::new(std::cell::Cell::new(Some(true)));
    let mut h = with_system(system.clone());
    assert_eq!(h.app.ui.settings.reduce_motion, None);
    assert_eq!(h.view.ctx.global_style().animation_time, 0.0, "the system says reduce");
    click_photo(&mut h);
    h.step();
    let next = h.app.image_rect.unwrap();
    h.settle(SETTLE);
    assert_eq!(next, h.app.image_rect.unwrap(), "zoomed at once");
    click_photo_back(&mut h);
    // the user chooses to keep motion: the system no longer decides
    let r = run(&mut h, "view.reduceMotion", json!({"on": false}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(false), json!(false)));
    click_photo(&mut h);
    h.step();
    let next = h.app.image_rect.unwrap();
    h.settle(SETTLE);
    assert_ne!(next, h.app.image_rect.unwrap(), "animates although the system says reduce");
    click_photo_back(&mut h);
    // back to following
    let r = run(&mut h, "view.reduceMotion", json!({"follow": true}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(true), json!(true)));
    assert_eq!(h.app.ui.settings.reduce_motion, None);
    // the system switches while the app runs: noticed within about a second
    system.set(Some(false));
    for _ in 0..90 {
        h.step();
    }
    assert!(!h.app.reduce_motion());
    assert_eq!(h.view.ctx.global_style().animation_time, 0.08);
    // no params toggles what is in effect
    let r = run(&mut h, "view.reduceMotion", json!({}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(true), json!(false)));
}

fn click_photo_back(h: &mut Headless) {
    let pos = h.app.image_rect.expect("image").center();
    h.app.synthetic.extend([
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
    ]);
    while !h.app.synthetic.is_empty() {
        h.step();
    }
    h.settle(SETTLE);
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "back at Fit");
}

#[test]
fn follow_false_is_the_same_as_no_params() {
    let mut h = detail();
    let r = run(&mut h, "view.reduceMotion", json!({"follow": false}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(true), json!(false)), "toggled like no params");
}

#[test]
fn the_settings_window_can_hand_the_choice_back_to_the_system() {
    let system = std::rc::Rc::new(std::cell::Cell::new(Some(true)));
    let mut h = with_system(system.clone());
    run(&mut h, "app.settings", json!({"tab": "interface"}));
    h.settle(SETTLE);
    let no_button = h.request("ui.clickWidget", json!({"id": "button:settings.reduceMotionFollow"}), T);
    assert_ne!(no_button["ok"], true, "no button while following the system");
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.reduceMotion"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.settings.reduce_motion, Some(false), "the tick box shows the system's true and the click turns it off");
    let r = h.request("ui.clickWidget", json!({"id": "button:settings.reduceMotionFollow"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.settings.reduce_motion, None);
    assert!(h.app.reduce_motion(), "follows the system again");
}

#[test]
fn without_a_system_setting_there_is_nothing_to_hand_back() {
    let mut h = detail();
    run(&mut h, "app.settings", json!({"tab": "interface"}));
    run(&mut h, "view.reduceMotion", json!({"on": true}));
    h.settle(SETTLE);
    assert!(h.app.widgets.iter().any(|(id, _)| id == "check:settings.reduceMotion"), "the Interface tab is drawn");
    let r = h.request("ui.clickWidget", json!({"id": "button:settings.reduceMotionFollow"}), T);
    assert_ne!(r["ok"], true, "{r}");
}

#[test]
fn follow_true_is_refused_where_there_is_no_system_setting_and_changes_nothing() {
    let mut h = detail();
    run(&mut h, "view.reduceMotion", json!({"on": true}));
    let r = h.request("engine.execute", json!({"command": "view.reduceMotion", "params": {"follow": true}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("no Reduce Motion setting to follow"), "{r}");
    assert_eq!(h.app.ui.settings.reduce_motion, Some(true), "the choice made stays");
    assert!(h.app.reduce_motion());
    // (and with a system setting to read, the same command is accepted: the_system_setting_is_followed_until_the_user_chooses)
}

#[test]
fn the_sync_spinner_is_a_still_dot_when_reduced() {
    let ctx = egui::Context::default();
    let shapes = |reduced: bool| {
        crate::motion::sync(&ctx, reduced);
        let mut out = ctx.run_ui(Default::default(), crate::motion::spinner);
        out.textures_delta.clear();
        out.shapes.iter().map(|s| format!("{:?}", s.shape)).collect::<Vec<_>>()
    };
    let (a, b) = (shapes(true), shapes(true));
    assert_eq!(a, b, "still");
    assert!(a.iter().any(|s| s.contains("Circle")), "a dot is painted: {a:?}");
}
