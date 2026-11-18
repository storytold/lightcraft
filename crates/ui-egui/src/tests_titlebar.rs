//! The integrated title bar behaves like a native one: drag the empty space to move the window,
//! double-click it to zoom and again to restore. Buttons and the search field keep their clicks.

use std::time::Duration;

use egui::ViewportCommand;
use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

/// Given the app with the integrated title bar (macOS) in a 1200×800 window.
fn integrated() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
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

/// A pause longer than egui's multi-click window, so the next click starts a new count.
fn pause(h: &mut Headless) {
    for _ in 0..30 {
        h.step();
    }
}

// Scenario: buttons and the search field keep their clicks and drags; only the empty bar is a window handle,
// dragged to move the window and double-clicked to zoom and restore it
#[test]
fn only_the_empty_bar_moves_and_zooms_the_window() {
    let mut h = integrated();
    let p = empty_spot(&h);
    h.request("ui.click", json!({"x": p.x, "y": p.y}), T);
    play(&mut h);
    assert!(h.window_commands.is_empty(), "a single click on the empty bar: {:?}", h.window_commands);
    pause(&mut h);
    let r = widget(&h, "icon:help");
    h.request("ui.drag", json!({"x": r.center().x, "y": r.center().y, "toX": r.center().x - 150.0, "toY": r.center().y + 30.0}), T);
    play(&mut h);
    assert!(h.window_commands.is_empty(), "a drag off a button (as in a native title bar): {:?}", h.window_commands);
    pause(&mut h);
    // (Esc closes whatever a click on the button opened)
    h.request("ui.key", json!({"key": "escape"}), T);
    pause(&mut h);
    // the search field: a double click selects a word, a drag selects text (#1646)
    let r = widget(&h, "field:search");
    let x = r.left() + r.width() * 0.35;
    h.request("ui.click", json!({"x": x, "y": r.center().y}), T);
    h.request("ui.text", json!({"text": "landscape"}), T);
    play(&mut h);
    h.request("ui.drag", json!({"x": x, "y": r.center().y, "toX": x + 60.0, "toY": r.center().y}), T);
    play(&mut h);
    assert!(h.window_commands.is_empty(), "a drag within the search field: {:?}", h.window_commands);
    pause(&mut h);
    h.request("ui.click", json!({"x": r.center().x, "y": r.center().y, "count": 2}), T);
    play(&mut h);
    assert!(h.window_commands.is_empty(), "a double click in the search field: {:?}", h.window_commands);
    pause(&mut h);
    // the empty bar
    h.request("ui.drag", json!({"x": p.x, "y": p.y, "toX": p.x + 120.0, "toY": p.y + 40.0}), T);
    play(&mut h);
    assert_eq!(h.window_commands, vec![ViewportCommand::StartDrag], "dragging the empty bar moves the window");
    pause(&mut h);
    h.request("ui.click", json!({"x": p.x, "y": p.y, "count": 2}), T);
    play(&mut h);
    assert_eq!(h.window_commands, vec![ViewportCommand::StartDrag, ViewportCommand::Maximized(true)], "a double click zooms");
    pause(&mut h);
    h.request("ui.click", json!({"x": p.x, "y": p.y, "count": 2}), T);
    play(&mut h);
    assert_eq!(h.window_commands.last(), Some(&ViewportCommand::Maximized(false)), "and again restores: {:?}", h.window_commands);
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

// Scenario: a window with its native title bar (Windows, Linux) leaves the bar alone
#[test]
fn without_the_integrated_title_bar_the_bar_is_not_a_window_handle() {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(Duration::from_secs(120));
    assert!(!h.app.integrated_titlebar);
    let bar = h.app.widgets.iter().find(|(w, _)| w == "region:titlebar");
    assert!(bar.is_none());
}
