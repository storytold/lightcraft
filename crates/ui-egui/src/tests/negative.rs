//! Headless tests of the Negative section: Convert asks before resetting edits made on the negative, converts at once
//! when there are none, and the film-base eyedropper is a tool of its own.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn edit_panel() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h
}

fn req(h: &mut Headless, method: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    r["result"].clone()
}

fn develop(h: &Headless) -> std::sync::Arc<lightcraft_develop::DevelopSettings> {
    h.app.session.develop_of(h.app.session.active().expect("active photo")).expect("settings")
}

#[test]
fn convert_asks_before_resetting_edits_made_on_the_negative() {
    let mut h = edit_panel();
    req(&mut h, "engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 1.0}}));
    req(&mut h, "engine.execute", json!({"command": "dialog.convertNegative", "params": {}}));
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::ConvertNegative { reset, .. }) if reset == &["Light".to_string()]), "{:?}", h.app.ui.dialog);
    assert!(!develop(&h).negative.converts(), "nothing converted before confirming");
    req(&mut h, "ui.clickWidget", json!({"id": "button:dialogCancel"}));
    assert!(!develop(&h).negative.converts() && develop(&h).light.exposure == 1.0, "Cancel changes nothing");
    req(&mut h, "engine.execute", json!({"command": "dialog.convertNegative", "params": {}}));
    h.step();
    req(&mut h, "ui.dialog.confirm", json!({}));
    let d = develop(&h);
    assert!(d.negative.converts() && d.light.exposure == 0.0, "converted, Light reset");
}

#[test]
fn convert_without_edits_converts_at_once_and_the_eyedropper_is_a_tool() {
    let mut h = edit_panel();
    req(&mut h, "engine.execute", json!({"command": "dialog.convertNegative", "params": {"film": "bwNegative"}}));
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert_eq!(develop(&h).negative.film, lightcraft_develop::FilmType::BwNegative);
    req(&mut h, "engine.execute", json!({"command": "tool.filmBase", "params": {}}));
    assert_eq!(h.app.ui.tool, "filmBase");
}
