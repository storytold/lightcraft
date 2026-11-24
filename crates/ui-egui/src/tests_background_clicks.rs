//! Settings ▸ Interface ▸ Click through (macOS): the setting, its command
//! `app.backgroundClicks`, and that the host is told when it changes. Whether AppKit then drops the
//! activating click is `lcc-firstclick`'s, and only a real window shows it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::AppSettings;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

/// A demo app whose host records each background-click choice it is given.
fn demo() -> (Headless, Arc<Mutex<Vec<bool>>>) {
    let told = Arc::new(Mutex::new(Vec::new()));
    let seen = told.clone();
    let services = Services {
        png: None,
        background_clicks: Some(Box::new(move |accept| {
            seen.lock().unwrap().push(accept);
            Ok(())
        })),
        ..Default::default()
    };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.step();
    (h, told)
}

fn clicks(h: &mut Headless, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": "app.backgroundClicks", "params": params}), T);
    h.step();
    h.step();
    r
}

#[test]
fn background_clicks_are_accepted_by_default_and_the_choice_is_saved() {
    assert!(AppSettings::default().accept_background_clicks, "a background click acts, as before");
    let s: AppSettings = serde_json::from_str("{}").unwrap();
    assert!(s.accept_background_clicks, "settings saved before the choice existed keep the default");
    let j = serde_json::to_value(AppSettings { accept_background_clicks: false, ..Default::default() }).unwrap();
    assert_eq!(j["acceptBackgroundClicks"], false);
    let s: AppSettings = serde_json::from_value(j).unwrap();
    assert!(!s.accept_background_clicks, "the choice reads back");
}

#[test]
fn the_host_is_told_the_choice_at_start_and_on_each_change() {
    let (mut h, told) = demo();
    assert_eq!(*told.lock().unwrap(), [true], "the saved choice, once, at start");
    h.app.ui.settings.accept_background_clicks = false;
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(*told.lock().unwrap(), [true, false], "a change is passed on once");
}

#[cfg(target_os = "macos")]
#[test]
fn the_command_sets_reports_and_rejects_bad_values() {
    let (mut h, told) = demo();
    let r = clicks(&mut h, json!({"accept": false}));
    assert_eq!(r["result"]["accept"], false, "{r}");
    assert!(!h.app.ui.settings.accept_background_clicks);
    assert_eq!(told.lock().unwrap().last(), Some(&false), "the host follows the command");
    let r = clicks(&mut h, json!({}));
    assert_eq!(r["result"]["accept"], false, "without accept it reports the choice: {r}");
    let r = clicks(&mut h, json!({"accept": "no"}));
    assert_eq!(r["ok"], false, "a value that isn't true or false is an error: {r}");
    assert!(!h.app.ui.settings.accept_background_clicks, "and changes nothing");
    let r = clicks(&mut h, json!({"accept": true}));
    assert_eq!(r["result"]["accept"], true, "{r}");
}

#[cfg(target_os = "macos")]
#[test]
fn the_settings_row_chooses_only_bringing_the_window_forward() {
    let (mut h, told) = demo();
    h.app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "interface".into() });
    h.step();
    crate::settings_screen::reveal(&mut h, "button:settingsBackgroundClicks-1");
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsBackgroundClicks-1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(!h.app.ui.settings.accept_background_clicks, "Only bring to front");
    assert_eq!(told.lock().unwrap().last(), Some(&false));
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsBackgroundClicks-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.ui.settings.accept_background_clicks, "Bring to front and click");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn the_choice_is_macos_only() {
    let (mut h, _) = demo();
    let r = clicks(&mut h, json!({"accept": false}));
    assert_eq!(r["ok"], false, "other systems always pass the click on: {r}");
    assert!(h.app.ui.settings.accept_background_clicks);
    h.app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "interface".into() });
    for _ in 0..3 {
        h.step();
    }
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("button:settingsBackgroundClicks-")), "no Settings row");
}
