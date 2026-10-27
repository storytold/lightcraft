//! Headless tests of View ▸ Lights Out (Shift+L): On → Dim → Off → On, Esc turns the lights back
//! on, and everything but the photo is veiled while the photo keeps its pixels.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::LightsOut;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn loupe() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn key(h: &mut Headless, k: &str, shift: bool) {
    let r = h.request("ui.key", json!({"key": k, "shift": shift}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

fn luma(img: &egui::ColorImage, p: egui::Pos2) -> u32 {
    let [w, _] = img.size;
    let c = img.pixels[(p.y as usize) * w + p.x as usize];
    c.r() as u32 + c.g() as u32 + c.b() as u32
}

#[test]
fn shift_l_cycles_lights_out_and_esc_turns_the_lights_back_on() {
    let mut h = loupe();
    assert_eq!(h.app.ui.lights_out, LightsOut::On);
    key(&mut h, "L", true);
    assert_eq!(h.app.ui.lights_out, LightsOut::Dim);
    key(&mut h, "L", true);
    assert_eq!(h.app.ui.lights_out, LightsOut::Off);
    key(&mut h, "L", true);
    assert_eq!(h.app.ui.lights_out, LightsOut::On);
    // plain L stays the Linear Gradient tool
    key(&mut h, "L", false);
    assert_eq!(h.app.ui.lights_out, LightsOut::On);
    key(&mut h, "Escape", false);
    key(&mut h, "L", true);
    key(&mut h, "Escape", false);
    assert_eq!(h.app.ui.lights_out, LightsOut::On);
    let r = h.request("engine.execute", json!({"command": "view.lightsOut", "params": {"mode": "dim"}}), T);
    assert_eq!(h.app.ui.lights_out, LightsOut::Dim, "{r}");
    let r = h.request("engine.execute", json!({"command": "view.lightsOut", "params": {"mode": "bogus"}}), T);
    assert_eq!(h.app.ui.lights_out, LightsOut::Dim, "{r}");
}

#[test]
fn lights_off_blacks_out_the_interface_but_not_the_photo() {
    let mut h = loupe();
    let before = h.snapshot(SETTLE);
    let img = h.app.image_rect.expect("the loupe shows the photo");
    let inside = img.center();
    let outside = egui::pos2(5.0, 5.0);
    assert!(!img.contains(outside));
    assert!(luma(&before, outside) > 0, "the title/top bar is not black to begin with");
    let r = h.request("engine.execute", json!({"command": "view.lightsOut", "params": {"mode": "off"}}), T);
    assert_eq!(h.app.ui.lights_out, LightsOut::Off, "{r}");
    let after = h.snapshot(SETTLE);
    assert_eq!(luma(&after, outside), 0, "the interface goes black");
    assert_eq!(luma(&after, inside), luma(&before, inside), "the photo keeps its pixels");
}
