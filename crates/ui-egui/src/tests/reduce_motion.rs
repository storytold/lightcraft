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
    assert!(back.settings.reduce_motion);
    assert!(!crate::state::AppSettings::default().reduce_motion, "off by default");
}

#[test]
fn the_command_toggles_and_sets_the_setting() {
    let mut h = detail();
    assert_eq!(run(&mut h, "view.reduceMotion", json!({}))["on"], true);
    assert!(h.app.ui.settings.reduce_motion);
    assert_eq!(run(&mut h, "view.reduceMotion", json!({}))["on"], false);
    assert_eq!(run(&mut h, "view.reduceMotion", json!({"on": true}))["on"], true);
    assert_eq!(run(&mut h, "view.reduceMotion", json!({"on": true}))["on"], true, "set, not toggled");
    assert_eq!(run(&mut h, "view.reduceMotion", json!({"on": false}))["on"], false);
}

#[test]
fn egui_easing_stops_with_reduce_motion_on_and_returns_when_off() {
    let mut h = detail();
    assert_eq!(h.view.ctx.global_style().animation_time, 0.08);
    run(&mut h, "view.reduceMotion", json!({"on": true}));
    assert_eq!(h.view.ctx.global_style().animation_time, 0.0);
    // a theme change restyles egui: the easing stays off
    run(&mut h, "view.appearance", json!({"mode": "light"}));
    assert_eq!(h.view.ctx.global_style().animation_time, 0.0);
    run(&mut h, "view.reduceMotion", json!({"on": false}));
    assert_eq!(h.view.ctx.global_style().animation_time, 0.08);
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
