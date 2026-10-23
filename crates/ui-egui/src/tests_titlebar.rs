//! The integrated title bar behaves like a native one: drag the empty space to move the window,
//! double-click it to zoom and again to restore. Buttons and the search field keep their clicks.

use std::time::Duration;

use egui::ViewportCommand;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// Given LightCraft with the integrated title bar (macOS) in a 1200×800 window.
fn integrated() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.integrated_titlebar = true;
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(Duration::from_secs(120));
    h
}

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// Empty top-bar space: where the traffic lights sit, left of the sidebar button.
fn empty_spot(h: &Headless) -> egui::Pos2 {
    let bar = widget(h, "region:titlebar");
    egui::pos2(bar.left() + 20.0, bar.center().y)
}

fn play(h: &mut Headless) {
    for _ in 0..6 {
        h.step();
    }
}

// Scenario: double-clicking the empty top bar zooms the window, and again restores it
#[test]
fn double_click_on_the_empty_bar_zooms_then_restores() {
    let mut h = integrated();
    let p = empty_spot(&h);
    h.request("ui.click", json!({"x": p.x, "y": p.y, "count": 2}), T);
    play(&mut h);
    assert_eq!(h.window_commands, vec![ViewportCommand::Maximized(true)]);
    // a pause longer than egui's multi-click window, or the second pair would count as a triple click
    for _ in 0..30 {
        h.step();
    }
    h.request("ui.click", json!({"x": p.x, "y": p.y, "count": 2}), T);
    play(&mut h);
    assert_eq!(h.window_commands, vec![ViewportCommand::Maximized(true), ViewportCommand::Maximized(false)]);
}

// Scenario: dragging the empty top bar starts a window move
#[test]
fn dragging_the_empty_bar_moves_the_window() {
    let mut h = integrated();
    let p = empty_spot(&h);
    h.request("ui.drag", json!({"x": p.x, "y": p.y, "toX": p.x + 120.0, "toY": p.y + 40.0}), T);
    play(&mut h);
    assert_eq!(h.window_commands, vec![ViewportCommand::StartDrag]);
}

// Scenario: a single click on the empty bar does nothing
#[test]
fn single_click_on_the_empty_bar_does_nothing() {
    let mut h = integrated();
    let p = empty_spot(&h);
    h.request("ui.click", json!({"x": p.x, "y": p.y}), T);
    play(&mut h);
    assert!(h.window_commands.is_empty(), "{:?}", h.window_commands);
}

// Scenario: double-clicking a button does not zoom the window
#[test]
fn double_click_on_a_button_does_not_zoom() {
    let mut h = integrated();
    let r = widget(&h, "icon:help");
    h.request("ui.click", json!({"x": r.center().x, "y": r.center().y, "count": 2}), T);
    play(&mut h);
    assert!(h.window_commands.is_empty(), "{:?}", h.window_commands);
}

// Scenario: pressing a button and dragging off it does not move the window (as in a native title bar)
#[test]
fn dragging_from_a_button_does_not_move_the_window() {
    let mut h = integrated();
    let r = widget(&h, "icon:help");
    h.request("ui.drag", json!({"x": r.center().x, "y": r.center().y, "toX": r.center().x - 150.0, "toY": r.center().y + 30.0}), T);
    play(&mut h);
    assert!(h.window_commands.is_empty(), "{:?}", h.window_commands);
}

// Scenario: double-clicking the search field selects a word, it does not zoom the window
#[test]
fn double_click_in_the_search_field_does_not_zoom() {
    let mut h = integrated();
    let r = widget(&h, "field:search");
    h.request("ui.click", json!({"x": r.center().x, "y": r.center().y, "count": 2}), T);
    play(&mut h);
    assert!(h.window_commands.is_empty(), "{:?}", h.window_commands);
}

// Scenario: dragging within the search field selects text rather than moving the window (#1646)
#[test]
fn dragging_within_search_does_not_move_the_window() {
    let mut h = integrated();
    let r = widget(&h, "field:search");
    let x = r.left() + r.width() * 0.35;
    h.request("ui.click", json!({"x": x, "y": r.center().y}), T);
    h.request("ui.text", json!({"text": "landscape"}), T);
    play(&mut h);
    h.request("ui.drag", json!({"x": x, "y": r.center().y, "toX": x + 60.0, "toY": r.center().y}), T);
    play(&mut h);
    assert!(h.window_commands.is_empty(), "{:?}", h.window_commands);
}

// Scenario: a window with its native title bar (Windows, Linux) leaves the bar alone
#[test]
fn without_the_integrated_title_bar_the_bar_is_not_a_window_handle() {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(Duration::from_secs(120));
    assert!(!h.app.integrated_titlebar);
    let bar = h.app.widgets.iter().find(|(w, _)| w == "region:titlebar");
    assert!(bar.is_none());
}

// Scenario: the search field is centred on the window, whatever room the traffic lights take (#2365)
#[test]
fn the_search_field_stays_centred_on_the_window() {
    let mut h = integrated();
    let window_centre = 1200.0 / 2.0;
    let windowed = widget(&h, "field:search");
    assert_eq!(windowed.center().x, window_centre, "windowed, the field is centred on the window");
    // the filter icon rides with the field, so it moves whenever the field does
    let filter = widget(&h, "icon:filter").center().x;
    h.request("ui.key", json!({"key": "f", "cmd": true, "shift": true}), T);
    play(&mut h);
    assert!(h.app.window_is_fullscreen);
    let full_screen = widget(&h, "field:search");
    assert_eq!(full_screen.center().x, window_centre, "full screen leaves it centred");
    assert_eq!(full_screen.width(), windowed.width(), "and the same width");
    assert_eq!(widget(&h, "icon:filter").center().x, filter);
    h.request("ui.key", json!({"key": "f", "cmd": true, "shift": true}), T);
    play(&mut h);
    assert!(!h.app.window_is_fullscreen);
    assert_eq!(widget(&h, "field:search").center().x, window_centre, "and so does leaving it");
}

// Scenario: in full screen the traffic lights are gone, so the bar's buttons move into their space
#[test]
fn full_screen_moves_the_left_buttons_into_the_traffic_lights_space() {
    let mut h = integrated();
    let window_right = 1200.0;
    let windowed = widget(&h, "icon:sidebar").left();
    assert_eq!(windowed, 78.0, "windowed, the bar leaves room for the traffic lights");
    h.request("ui.key", json!({"key": "f", "cmd": true, "shift": true}), T);
    play(&mut h);
    assert!(h.app.window_is_fullscreen);
    let right_padding = window_right - widget(&h, "icon:discord").right();
    assert_eq!(widget(&h, "icon:sidebar").left(), right_padding, "My Photos as far from the left edge as Discord is from the right");
    h.request("ui.key", json!({"key": "f", "cmd": true, "shift": true}), T);
    play(&mut h);
    assert!(!h.app.window_is_fullscreen);
    assert_eq!(widget(&h, "icon:sidebar").left(), windowed, "leaving full screen restores the room for the traffic lights");
}
