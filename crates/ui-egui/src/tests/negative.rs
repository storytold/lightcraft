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

#[test]
fn sync_roll_asks_then_measures_the_frames_in_the_background() {
    let mut h = edit_panel();
    let ids: Vec<u64> = h.app.session.catalog.photos().take(3).map(|p| p.id.0).collect();
    req(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": ids, "active": ids[0]}}));
    req(&mut h, "engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.6, "ids": [ids[1]]}}));
    req(&mut h, "engine.execute", json!({"command": "dialog.syncRoll", "params": {}}));
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::ConvertNegative { roll, .. }) if roll == &ids), "{:?}", h.app.ui.dialog);
    req(&mut h, "ui.dialog.confirm", json!({}));
    // the frames are read on a worker, with a row in the activity stack, then written as one step
    let rows = h.app.session.activity.list();
    assert!(rows.iter().any(|r| r.kind == "negativeRoll" && r.label == crate::negative_roll::LABEL), "{rows:?}");
    assert!(h.step_until(T, |h| h.app.tasks.is_empty()));
    assert!(h.app.session.activity.list().iter().all(|r| r.kind != "negativeRoll"), "the row is gone");
    let first = h.app.session.develop_of(lightcraft_engine::catalog::PhotoId(ids[0])).unwrap().negative.analysis;
    for id in &ids {
        let n = h.app.session.develop_of(lightcraft_engine::catalog::PhotoId(*id)).unwrap().negative;
        assert!(n.locked && n.converts(), "frame {id}");
        assert_eq!(n.analysis.map(|a| a.hi), first.map(|a| a.hi), "one analysis for the roll");
    }
    assert_eq!(h.app.session.develop_of(lightcraft_engine::catalog::PhotoId(ids[1])).unwrap().light.exposure, 0.0, "edits on the negative reset");
}

#[test]
fn convert_confirms_on_the_photo_it_warned_about() {
    let mut h = edit_panel();
    let ids: Vec<u64> = h.app.session.catalog.photos().take(2).map(|p| p.id.0).collect();
    req(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0]], "active": ids[0]}}));
    req(&mut h, "engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 1.0}}));
    req(&mut h, "engine.execute", json!({"command": "dialog.convertNegative", "params": {}}));
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::ConvertNegative { photo: Some(p), .. }) if *p == ids[0]));
    // the active photo changes while the dialog is open
    h.app.session.execute("library.select", &json!({"ids": [ids[1]], "active": ids[1]})).unwrap();
    req(&mut h, "ui.dialog.confirm", json!({}));
    let of = |h: &Headless, id: u64| h.app.session.develop_of(lightcraft_engine::catalog::PhotoId(id)).unwrap();
    assert!(of(&h, ids[0]).negative.converts(), "the photo the warning named");
    assert!(!of(&h, ids[1]).negative.converts(), "not the one active at confirmation");
}

#[test]
fn convert_refuses_edits_made_after_its_warning_and_a_bad_film() {
    let mut h = edit_panel();
    let r = h.request("engine.execute", json!({"command": "dialog.convertNegative", "params": {"film": 7}}), T);
    assert_eq!(r["ok"], false, "a film type that isn't one is refused before any dialog: {r}");
    req(&mut h, "engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 1.0}}));
    req(&mut h, "engine.execute", json!({"command": "dialog.convertNegative", "params": {}}));
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::ConvertNegative { reset, .. }) if reset == &["Light".to_string()]));
    // a Color edit arrives (an agent) while the warning names only Light
    h.app.session.execute("develop.set", &json!({"control": "color.saturation", "value": 30})).unwrap();
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert!(!develop(&h).negative.converts(), "not converted: the warning didn't list Color ({r})");
    assert_eq!(develop(&h).color.saturation, 30.0);
}
