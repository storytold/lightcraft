//! Issue #793: Settings' ‹ back button clears the macOS traffic lights (an integrated title bar
//! draws them over the header's top-left corner), and gives their room back in full screen, where
//! the host hides them — the same 78 the top bar's sidebar button starts at.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {id}"))
}

fn open(integrated_titlebar: bool, fullscreen: bool) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
    app.integrated_titlebar = integrated_titlebar;
    app.window_is_fullscreen = fullscreen;
    // (the full-window Settings, with its ‹: the Modern layout's; Classic shows a window)
    app.ui.modern.enabled = true;
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.dialog = Some(Dialog::Settings { tab: "general".into() });
    // (the first request switches the widget list on)
    h.request("ui.widgets", json!({}), T);
    for _ in 0..6 {
        h.step();
    }
    h
}

// Scenario: with an integrated title bar the ‹ button starts right of the traffic lights; without one (and in
// macOS full screen, where the lights hide) nothing moves
#[test]
fn back_clears_the_traffic_lights_only_where_they_are() {
    let plain = rect(&open(false, false), "settings:back");
    let lights = rect(&open(true, false), "settings:back");
    assert!(lights.left() > plain.left() + 40.0, "clear of the traffic lights: {lights:?} against {plain:?}");
    let fullscreen = rect(&open(true, true), "settings:back");
    assert_eq!(fullscreen.left(), plain.left(), "no lights in full screen: no offset");
}
