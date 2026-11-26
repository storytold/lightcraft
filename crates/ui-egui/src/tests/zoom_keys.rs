//! Headless tests of the View ▸ Zoom shortcuts and modifier-wheel zoom: Cmd+= / Cmd+- / Cmd+0
//! (Ctrl on Windows and Linux) zoom the photo and never egui's interface scale, which would
//! otherwise take the same keys at the end of the frame; Ctrl+wheel over the photo in the Edit
//! view zooms it. Also the grid thumbnail-size keys `=` / `-` (Classic's Library Grid keys), which
//! share the physical keys with Cmd+= / Cmd+- but act on the grids only.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Zoom;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn edit_view() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn key(h: &mut Headless, k: &str) {
    let r = h.request("ui.key", json!({"key": k, "cmd": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
fn zoom_shortcuts_zoom_the_photo_and_leave_the_interface_scale_alone() {
    let mut h = edit_view();
    assert_eq!(h.app.ui.zoom, Zoom::Fit);
    let scale = h.view.ctx.zoom_factor();
    // Cmd+= (Ctrl+= on Windows / Linux) is View ▸ Zoom In: the photo, not the UI
    key(&mut h, "=");
    assert_eq!(h.app.ui.zoom, Zoom::Percent(50.0), "{:?}", h.app.ui.zoom);
    assert_eq!(h.view.ctx.zoom_factor(), scale, "Cmd+= scaled the interface");
    // Cmd+Plus (a keypad or layout that reports `+`) is egui's other interface-zoom key
    key(&mut h, "Plus");
    assert_eq!(h.view.ctx.zoom_factor(), scale, "Cmd+Plus scaled the interface");
    key(&mut h, "-");
    assert_eq!(h.app.ui.zoom, Zoom::Percent(25.0), "{:?}", h.app.ui.zoom);
    assert_eq!(h.view.ctx.zoom_factor(), scale, "Cmd+- scaled the interface");
    key(&mut h, "-");
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "{:?}", h.app.ui.zoom);
    assert_eq!(h.view.ctx.zoom_factor(), scale, "Cmd+- scaled the interface");
    // Cmd+0 is Zoom to Fit; egui would reset its interface scale with the same key
    h.app.run("view.navigate", json!({"zoom": {"percent": 200.0}})).unwrap();
    h.view.ctx.set_zoom_factor(1.2);
    h.step();
    h.step();
    assert!((h.view.ctx.zoom_factor() - 1.2).abs() < 0.001);
    key(&mut h, "0");
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "{:?}", h.app.ui.zoom);
    assert!((h.view.ctx.zoom_factor() - 1.2).abs() < 0.001, "Cmd+0 reset the interface scale");
}

#[test]
fn ctrl_wheel_over_the_photo_zooms_it_in_the_edit_view() {
    let mut h = edit_view();
    let img = h.app.image_rect.unwrap();
    let canvas = h.app.canvas_rect.unwrap();
    assert!(canvas.contains(img.center()), "{img:?} outside {canvas:?}");
    let r = h.request("ui.move", json!({"x": img.center().x, "y": img.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.scroll", json!({"dy": 120.0, "ctrl": true, "cmd": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..40 {
        h.step();
    }
    assert!(matches!(h.app.ui.zoom, Zoom::Percent(p) if p > 0.0), "Ctrl+wheel did not zoom: {:?}", h.app.ui.zoom);
    assert!(h.app.image_rect.unwrap().width() > img.width(), "the photo did not grow");
    // a plain wheel over the photo pans (or does nothing at Fit): never zooms further
    let zoomed = h.app.image_rect.unwrap();
    let r = h.request("ui.scroll", json!({"dy": 120.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..40 {
        h.step();
    }
    assert!((h.app.image_rect.unwrap().width() - zoomed.width()).abs() < 0.01, "a plain wheel changed the zoom");
}

fn grid_view(view: crate::state::ViewMode) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.settings.gpu = false;
    app.ui.view = view;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.step();
    h.step();
    h
}

fn press(h: &mut Headless, k: &str, shift: bool, cmd: bool) {
    let r = h.request("ui.key", json!({"key": k, "shift": shift, "cmd": cmd}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
fn equals_and_minus_step_the_grid_thumbnail_size_and_clamp() {
    use crate::state::{THUMB_SIZE_MAX, THUMB_SIZE_MIN, THUMB_SIZE_STEP, ViewMode};
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::People] {
        let mut h = grid_view(view);
        h.app.ui.thumb_size = 220.0;
        press(&mut h, "=", false, false);
        assert_eq!(h.app.ui.thumb_size, 220.0 + THUMB_SIZE_STEP, "{view:?}: =");
        press(&mut h, "-", false, false);
        press(&mut h, "-", false, false);
        assert_eq!(h.app.ui.thumb_size, 220.0 - THUMB_SIZE_STEP, "{view:?}: - -");
        // `+` (Shift+= on a US layout, or the keypad) is the same key for the same job
        press(&mut h, "Plus", true, false);
        press(&mut h, "Plus", false, false);
        assert_eq!(h.app.ui.thumb_size, 220.0 + THUMB_SIZE_STEP, "{view:?}: +");
        // clamped at both ends of the bottom-bar slider's range, however many presses
        h.app.ui.thumb_size = THUMB_SIZE_MAX - 5.0;
        for _ in 0..3 {
            press(&mut h, "=", false, false);
            assert!(h.app.ui.thumb_size <= THUMB_SIZE_MAX);
        }
        assert_eq!(h.app.ui.thumb_size, THUMB_SIZE_MAX, "{view:?}");
        h.app.ui.thumb_size = THUMB_SIZE_MIN + 5.0;
        for _ in 0..3 {
            press(&mut h, "-", false, false);
            assert!(h.app.ui.thumb_size >= THUMB_SIZE_MIN);
        }
        assert_eq!(h.app.ui.thumb_size, THUMB_SIZE_MIN, "{view:?}");
        // the whole range is walked in whole steps from either end
        let mut presses = 0;
        while h.app.ui.thumb_size < THUMB_SIZE_MAX {
            press(&mut h, "=", false, false);
            presses += 1;
            assert!(presses <= 20, "never reached the maximum");
        }
        assert_eq!(presses, ((THUMB_SIZE_MAX - THUMB_SIZE_MIN) / THUMB_SIZE_STEP).ceil() as usize);
    }
}

#[test]
fn thumbnail_size_keys_do_nothing_outside_the_grids() {
    use crate::state::ViewMode;
    for view in [ViewMode::Detail, ViewMode::Compare, ViewMode::Survey, ViewMode::Reference] {
        let mut h = grid_view(view);
        h.app.ui.thumb_size = 220.0;
        let zoom = h.app.ui.zoom;
        for (k, shift) in [("=", false), ("-", false), ("Plus", true)] {
            press(&mut h, k, shift, false);
        }
        assert_eq!(h.app.ui.thumb_size, 220.0, "{view:?}");
        assert_eq!(h.app.ui.zoom, zoom, "{view:?}: a bare = / - is not a zoom key");
        // the command says why, and the menu shows it unavailable
        let r = h.request("ui.menu.invoke", json!({"id": "view.thumbSizeUp"}), T);
        assert_eq!(r["ok"], false, "{view:?}: {r}");
        assert_eq!(h.app.ui.thumb_size, 220.0);
        let list = h.request("ui.menu.list", json!({}), T);
        let entry = list["result"].as_array().and_then(|a| a.iter().find(|e| e["id"] == "view.thumbSizeUp")).cloned().unwrap_or_default();
        assert_eq!(entry["enabled"], false, "{view:?}: {entry}");
    }
}

#[test]
fn cmd_equals_and_minus_still_zoom_and_leave_the_thumbnail_size_alone() {
    // in the loupe: the photo zooms, the grid size is untouched
    let mut h = edit_view();
    h.app.ui.thumb_size = 220.0;
    key(&mut h, "=");
    assert_eq!(h.app.ui.zoom, Zoom::Percent(50.0));
    key(&mut h, "-");
    assert_eq!(h.app.ui.zoom, Zoom::Percent(25.0));
    assert_eq!(h.app.ui.thumb_size, 220.0);
    // in a grid Cmd+= / Cmd+- are View ▸ Zoom In / Out, not the size keys
    let mut h = grid_view(crate::state::ViewMode::PhotoGrid);
    h.app.ui.thumb_size = 220.0;
    key(&mut h, "=");
    key(&mut h, "-");
    assert_eq!(h.app.ui.thumb_size, 220.0, "Cmd+= / Cmd+- changed the grid size");
}

#[test]
fn search_field_keeps_equals_and_minus() {
    let mut h = grid_view(crate::state::ViewMode::PhotoGrid);
    h.app.ui.thumb_size = 220.0;
    let r = h.request("ui.clickWidget", json!({"id": "field:search"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    for (k, text) in [("-", "-"), ("=", "="), ("Plus", "+")] {
        press(&mut h, k, false, false);
        let r = h.request("ui.text", json!({"text": text}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }
    assert_eq!(h.app.ui.thumb_size, 220.0, "typing in the search field resized the grid");
    assert_eq!(h.app.ui.search, "-=+", "the search field did not receive the keys");
}

#[test]
fn thumbnail_size_commands_run_from_the_control_channel_and_persist() {
    let mut h = grid_view(crate::state::ViewMode::SquareGrid);
    h.app.ui.thumb_size = 120.0;
    let r = h.request("ui.menu.invoke", json!({"id": "view.thumbSizeUp"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["size"], 150.0);
    let r = h.request("engine.execute", json!({"command": "view.thumbSizeDown"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.thumb_size, 120.0);
    let r = h.request("ui.menu.invoke", json!({"id": "view.thumbSizeUp"}), T);
    assert_eq!(r["ok"], true, "{r}");
    // the size is saved with the rest of the UI state, like the slider's, and restored clamped
    let saved = serde_json::to_value(&h.app.ui).unwrap();
    assert_eq!(saved["thumbSize"], 150.0);
    let loaded = serde_json::from_value::<crate::state::UiState>(saved).unwrap().sanitized();
    assert_eq!(loaded.thumb_size, 150.0);
    let mut wild = serde_json::to_value(&h.app.ui).unwrap();
    wild["thumbSize"] = json!(9000.0);
    assert_eq!(serde_json::from_value::<crate::state::UiState>(wild).unwrap().sanitized().thumb_size, crate::state::THUMB_SIZE_MAX);
}
