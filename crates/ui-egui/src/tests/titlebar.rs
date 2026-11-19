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

// Scenario: a panel floating over the bar keeps a drag on it from moving the window (macOS's
// title-bar band would otherwise take the drag); a bar control does too
#[test]
fn a_panel_floating_over_the_bar_keeps_the_window_from_moving() {
    let mut h = integrated();
    let bar = widget(&h, "region:titlebar");
    let empty = empty_spot(&h);
    let hover = |h: &mut Headless, p: egui::Pos2| {
        h.events.push(egui::Event::PointerMoved(p));
        play(h);
        craft_window::title_bar::window_movable(&h.view.ctx)
    };
    assert!(hover(&mut h, empty), "the empty bar moves the window");
    let help = widget(&h, "icon:help").center();
    assert!(!hover(&mut h, help), "a bar control");
    let params = json!({"operation": "float", "panel": "tools", "rect": [bar.left(), bar.top() + 4.0, 330.0, 500.0]});
    let r = h.request("engine.execute", json!({"command": "ui.dock", "params": params}), T);
    assert_eq!(r["ok"], true, "{r}");
    play(&mut h);
    assert!(!hover(&mut h, empty), "the floating panel covers the empty bar there");
}

// A dialog on the integrated title bar behaves like a macOS sheet: the backdrop leaves the bar
// undimmed (the window buttons AppKit draws over it stay lit beside a bar that looks live), while
// the bar's own controls stay inactive.

fn pixel(img: &egui::ColorImage, p: egui::Pos2) -> egui::Color32 {
    img.pixels[p.y as usize * img.size[0] + p.x as usize]
}

fn with_export_dialog(mut h: Headless) -> Headless {
    h.app.run("dialog.export", json!({})).unwrap();
    h.step();
    h.step();
    h.settle(T);
    h
}

// Scenario: the dialog's backdrop does not dim the integrated title bar
#[test]
fn a_dialog_leaves_the_integrated_title_bar_undimmed() {
    let mut h = integrated();
    let spot = empty_spot(&h);
    let below = egui::pos2(spot.x, widget(&h, "region:titlebar").bottom() + 60.0);
    let closed = h.snapshot(T);
    let mut h = with_export_dialog(h);
    let open = h.snapshot(T);
    assert_eq!(pixel(&open, spot), pixel(&closed, spot), "the bar keeps its color under a dialog");
    assert_ne!(pixel(&open, below), pixel(&closed, below), "the window below the bar is dimmed");
}

// Scenario: a window with its native title bar dims everything, the top bar included
#[test]
fn a_dialog_dims_the_top_bar_without_the_integrated_title_bar() {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(Duration::from_secs(120));
    let spot = egui::pos2(20.0, 20.0);
    let closed = h.snapshot(T);
    let mut h = with_export_dialog(h);
    let open = h.snapshot(T);
    assert_ne!(pixel(&open, spot), pixel(&closed, spot), "the top bar is dimmed");
}

// Scenario: the full-screen photo view draws no top bar, so a dialog opened there has no title bar band to spare: the
// frame the view is entered on still holds the bar's region from the frame before, and must not lay a window handle
// over the backdrop (the band that handle covers is the strip the backdrop would leave undimmed)
#[test]
fn a_dialog_in_the_full_screen_photo_view_has_no_title_bar_handle() {
    let mut h = integrated();
    assert!(h.app.widgets.iter().any(|(w, _)| w == "region:titlebar"), "the bar was drawn the frame before");
    h.app.run("view.fullScreenPreview", json!({})).unwrap();
    h.app.run("dialog.export", json!({})).unwrap();
    h.step();
    assert!(h.app.ui.fullscreen && h.app.ui.dialog.is_some());
    let handles = [0usize, 1].map(|i| egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("dialog-title-bar").with(i)));
    let handle = h.view.ctx.memory(|m| handles.iter().any(|layer| m.areas().visible_last_frame(layer)));
    assert!(!handle, "no title bar handle over the photo");
}

// Scenario: a press on the backdrop just below the bar, released on the bar, is not a click outside the dialog
#[test]
fn a_press_below_the_title_bar_released_on_it_keeps_the_dialog_open() {
    let mut h = integrated();
    h.app.ui.settings.close_dialogs_outside = true;
    let mut h = with_export_dialog(h);
    let spot = empty_spot(&h);
    let bar = widget(&h, "region:titlebar");
    let below = egui::pos2(spot.x, bar.bottom() + 1.0);
    let on_bar = egui::pos2(spot.x, bar.bottom() - 1.0);
    h.request("ui.drag", json!({"x": below.x, "y": below.y, "toX": on_bar.x, "toY": on_bar.y}), T);
    play(&mut h);
    assert!(h.app.ui.dialog.is_some(), "the release on the bar is not a click on the backdrop");
}

// Scenario: a click on the undimmed bar is not a click outside the dialog
#[test]
fn a_click_on_the_title_bar_keeps_the_dialog_open() {
    let mut h = integrated();
    h.app.ui.settings.close_dialogs_outside = true;
    let mut h = with_export_dialog(h);
    let at = empty_spot(&h);
    h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
    play(&mut h);
    assert!(h.app.ui.dialog.is_some(), "the bar is not the backdrop");
    let below = egui::pos2(at.x, widget(&h, "region:titlebar").bottom() + 60.0);
    h.request("ui.click", json!({"x": below.x, "y": below.y}), T);
    play(&mut h);
    assert!(h.app.ui.dialog.is_none(), "the backdrop below it still closes the dialog");
}

// Scenario: the bar's controls take no clicks under a dialog
#[test]
fn the_title_bar_buttons_are_inactive_under_a_dialog() {
    let mut h = with_export_dialog(integrated());
    let left = h.app.ui.left_panel;
    let at = widget(&h, "icon:sidebar").center();
    h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
    play(&mut h);
    assert_eq!(h.app.ui.left_panel, left, "the sidebar button does nothing");
    assert!(h.app.ui.dialog.is_some());
}

// Scenario: dragging the empty bar still moves the window under a dialog, whether the dialog is
// tall enough to reach into the bar (Export) or not (Copy Settings)
#[test]
fn dragging_the_empty_bar_moves_the_window_under_a_dialog() {
    for command in ["dialog.export", "dialog.copySettings"] {
        let mut h = integrated();
        h.app.run(command, json!({})).unwrap();
        h.step();
        h.step();
        h.settle(T);
        let p = empty_spot(&h);
        h.window_commands.clear();
        h.request("ui.drag", json!({"x": p.x, "y": p.y, "toX": p.x + 120.0, "toY": p.y + 40.0}), T);
        play(&mut h);
        assert_eq!(h.window_commands, vec![ViewportCommand::StartDrag], "{command}");
    }
}

// Scenario: a tall dialog reaching into the bar keeps a press on itself from moving the window
// (AppKit's `movable` is off while the pointer is over it), the bar beside it still moves it
#[test]
fn a_dialog_inside_the_bar_keeps_the_window_from_moving() {
    let mut h = with_export_dialog(integrated());
    let bar = widget(&h, "region:titlebar");
    let window = widget(&h, "dialog:window");
    assert!(window.top() < bar.bottom(), "the dialog reaches into the bar");
    let hover = |h: &mut Headless, p: egui::Pos2| {
        h.events.push(egui::Event::PointerMoved(p));
        play(h);
        craft_window::title_bar::window_movable(&h.view.ctx)
    };
    let spot = empty_spot(&h);
    assert!(hover(&mut h, spot), "the bar beside the dialog moves the window");
    assert!(!hover(&mut h, egui::pos2(window.center().x, bar.center().y)), "the dialog over the bar does not");
}

// Scenario: with a dropdown open in the dialog, the bar's handle stays out of the way: egui lifts an area that was not
// there the frame before above the others, so a handle drawn again as the pointer leaves the dialog would take the
// press meant for the dropdown's item
#[test]
fn the_title_bar_handle_stays_below_a_dropdown_open_in_the_dialog() {
    let mut h = with_export_dialog(integrated());
    let spot = empty_spot(&h);
    let handle_at = |h: &mut Headless| {
        h.events.push(egui::Event::PointerMoved(spot));
        play(h);
        let handles = [0usize, 1].map(|i| egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("dialog-title-bar").with(i)));
        h.view.ctx.memory(|m| handles.iter().any(|layer| m.areas().visible_last_frame(layer)))
    };
    assert!(handle_at(&mut h), "without a dropdown the bar beside the dialog is the handle's");
    let r = h.request("ui.clickWidget", json!({"id": "combo:exportPreset"}), T);
    assert_eq!(r["ok"], true, "{r}");
    play(&mut h);
    assert!(egui::Popup::is_any_open(&h.view.ctx), "the preset dropdown is open");
    assert!(!handle_at(&mut h), "with one open, no handle sits above it");
    assert!(egui::Popup::is_any_open(&h.view.ctx), "(and the pointer leaving the dialog did not close it)");
    // once the dropdown is closed the handle is back, so the bar beside the dialog moves the window again
    h.request("ui.key", json!({"key": "Escape"}), T);
    play(&mut h);
    assert!(!egui::Popup::is_any_open(&h.view.ctx), "Escape closes the dropdown");
    assert!(h.app.ui.dialog.is_some(), "and leaves the dialog");
    assert!(handle_at(&mut h), "the handle is back");
}
