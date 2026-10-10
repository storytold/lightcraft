//! Headless tests of the View ▸ Zoom shortcuts and modifier-wheel zoom: Cmd+= / Cmd+- / Cmd+0
//! (Ctrl on Windows and Linux) zoom the photo and never egui's interface scale, which would
//! otherwise take the same keys at the end of the frame; Ctrl+wheel over the photo in the Edit
//! view zooms it.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Zoom;
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn edit_view() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = DacApp::new(dac_engine::Session::with_demo(), services);
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
    // (the fork zooms by Lightroom Classic's fixed levels: from Fit in this window the next is 1:3)
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0 / 3.0), "{:?}", h.app.ui.zoom);
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
