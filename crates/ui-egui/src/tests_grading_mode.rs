//! The Edit panel's grading modes (Eboda, the Lightroom Classic panels; Witchcraft, the DaVinci
//! Resolve style palettes): the `view.gradingMode` command, the header's segmented control, and
//! that both modes edit the same photo settings.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{AppSettings, GradingMode};
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 2400.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn grade(h: &mut Headless, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": "view.gradingMode", "params": params}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    r["result"].clone()
}

#[test]
fn the_command_cycles_selects_and_rejects_unknown_modes() {
    let mut h = demo();
    assert_eq!(h.app.ui.settings.grading_mode, GradingMode::Witchcraft, "Witchcraft is the default");
    let r = grade(&mut h, json!({}));
    assert_eq!(r["mode"], "eboda");
    assert_eq!(h.app.ui.settings.grading_mode, GradingMode::Eboda);
    let r = grade(&mut h, json!({"mode": "witchcraft"}));
    assert_eq!(r["mode"], "witchcraft");
    assert_eq!(h.app.ui.settings.grading_mode, GradingMode::Witchcraft);
    let r = h.request("engine.execute", json!({"command": "view.gradingMode", "params": {"mode": "nope"}}), T);
    assert_eq!(r["ok"], false, "an unknown mode is an error, not a panic: {r}");
}

#[test]
fn the_settings_row_switches_modes_and_the_edit_header_stays_clear() {
    let mut h = demo();
    // the switch lives in Settings ▸ General, not on the Edit panel's face
    assert!(!h.app.widgets.iter().any(|(w, _)| w.starts_with("button:gradingMode-")), "no header toggle");
    h.app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "general".into() });
    h.settle(SETTLE);
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:settingsGradingMode-1"), "the Settings row");
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsGradingMode-1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.settings.grading_mode, GradingMode::Witchcraft);
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsGradingMode-0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.settings.grading_mode, GradingMode::Eboda);
}

#[test]
fn grading_mode_round_trips_and_unknown_values_read_as_the_default() {
    let s: AppSettings = serde_json::from_str(r#"{"gradingMode":"eboda"}"#).unwrap();
    assert_eq!(s.grading_mode, GradingMode::Eboda);
    let s: AppSettings = serde_json::from_str(r#"{"gradingMode":"nope"}"#).unwrap();
    assert_eq!(s.grading_mode, GradingMode::Witchcraft, "unknown reads as the default");
    let j = serde_json::to_value(AppSettings { grading_mode: GradingMode::Witchcraft, ..Default::default() }).unwrap();
    assert_eq!(j["gradingMode"], "witchcraft");
}
