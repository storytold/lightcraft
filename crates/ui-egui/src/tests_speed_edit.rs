//! Headless tests of the speed-edit keys (plan item 5.10): hold a key in Develop and scroll or
//! drag on the photo to move its slider, one history step per gesture, while a tap still runs
//! the key's own shortcut.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn develop_loupe() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("engine.execute", json!({"command": "module.develop", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    // an edit before the test's own, so the photo's history already has its Import step
    ask(&mut h, "engine.execute", json!({"command": "develop.set", "params": {"control": "color.vibrance", "value": 5.0}}));
    h
}

fn ask(h: &mut Headless, cmd: &str, p: serde_json::Value) {
    let r = h.request(cmd, p, T);
    assert_eq!(r["ok"], true, "{cmd}: {r}");
    h.step();
}

fn value(h: &Headless, id: &str) -> f64 {
    let photo = h.app.session.active().expect("active photo");
    let d = h.app.session.develop_of(photo).unwrap_or_default();
    lcc_develop::controls::get(&d, id).expect("control")
}

fn history_len(h: &Headless) -> usize {
    let photo = h.app.session.active().expect("active photo");
    h.app.session.catalog.photo(photo).map_or(0, |p| p.history.len())
}

fn hover_middle(h: &mut Headless) {
    ask(h, "ui.pointer", json!({"events": [{"kind": "move", "x": 0.5, "y": 0.5}]}));
}

#[test]
fn hold_e_and_scroll_moves_exposure_as_one_undo_step() {
    let mut h = develop_loupe();
    hover_middle(&mut h);
    let (before, steps) = (value(&h, "light.exposure"), history_len(&h));
    let zoom = h.app.ui.zoom;
    ask(&mut h, "ui.key", json!({"key": "E", "action": "down"}));
    assert!(h.app.speed_edit.is_some(), "holding E starts a speed edit");
    let line = f64::from(crate::theme::WHEEL_LINE_POINTS);
    for _ in 0..3 {
        ask(&mut h, "ui.scroll", json!({"dy": line}));
    }
    let raised = value(&h, "light.exposure");
    assert!(raised > before, "scrolling up raises exposure: {before} → {raised}");
    assert_eq!(h.app.ui.zoom, zoom, "the scroll moved the slider, not the view");
    ask(&mut h, "ui.key", json!({"key": "E", "action": "up"}));
    h.step();
    assert!(h.app.speed_edit.is_none());
    assert_eq!(history_len(&h), steps + 1, "the whole gesture is one history step");
    ask(&mut h, "engine.execute", json!({"command": "edit.undo", "params": {}}));
    assert!((value(&h, "light.exposure") - before).abs() < 1e-9, "one undo takes the whole gesture back");
}

#[test]
fn hold_c_and_drag_moves_contrast_without_panning() {
    let mut h = develop_loupe();
    hover_middle(&mut h);
    let before = value(&h, "light.contrast");
    let steps = history_len(&h);
    ask(&mut h, "ui.key", json!({"key": "C", "action": "down"}));
    ask(
        &mut h,
        "ui.pointer",
        json!({"events": [
            {"kind": "down", "x": 0.5, "y": 0.5},
            {"kind": "drag", "x": 0.55, "y": 0.5},
            {"kind": "drag", "x": 0.6, "y": 0.5},
            {"kind": "drag", "x": 0.7, "y": 0.5},
            {"kind": "up", "x": 0.7, "y": 0.5}
        ]}),
    );
    h.step();
    let after = value(&h, "light.contrast");
    assert!(after > before, "dragging right raises contrast: {before} → {after}");
    ask(&mut h, "ui.key", json!({"key": "C", "action": "up"}));
    h.step();
    assert_eq!(history_len(&h), steps + 1, "one history step per gesture");
}

#[test]
fn a_tap_still_runs_the_keys_shortcut_and_changes_nothing() {
    let mut h = develop_loupe();
    // give the key a shortcut whose effect is easy to see, and the speed edit
    ask(&mut h, "engine.execute", json!({"command": "app.setShortcut", "params": {"id": "app.shortcuts", "shortcut": "Q"}}));
    ask(&mut h, "engine.execute", json!({"command": "app.setSpeedKey", "params": {"key": "Q", "control": "light.shadows"}}));
    let (before, steps) = (value(&h, "light.shadows"), history_len(&h));
    ask(&mut h, "ui.key", json!({"key": "Q", "action": "down"}));
    assert!(h.app.ui.dialog.is_none(), "the press waits while the key is held");
    ask(&mut h, "ui.key", json!({"key": "Q", "action": "up"}));
    h.step();
    assert!(h.app.ui.dialog.is_some(), "a tap that adjusted nothing runs the key's shortcut on release");
    assert_eq!((value(&h, "light.shadows"), history_len(&h)), (before, steps));
}

#[test]
fn remapped_and_removed_keys_follow_the_users_map() {
    let mut h = develop_loupe();
    hover_middle(&mut h);
    ask(&mut h, "engine.execute", json!({"command": "app.setSpeedKey", "params": {"key": "E", "control": "wb.temp"}}));
    let (temp, exposure) = (value(&h, "wb.temp"), value(&h, "light.exposure"));
    ask(&mut h, "ui.key", json!({"key": "E", "action": "down"}));
    ask(&mut h, "ui.scroll", json!({"dy": -f64::from(crate::theme::WHEEL_LINE_POINTS) * 2.0}));
    ask(&mut h, "ui.key", json!({"key": "E", "action": "up"}));
    assert!(value(&h, "wb.temp") < temp, "E now lowers temperature on a scroll down");
    assert_eq!(value(&h, "light.exposure"), exposure);
    // removed: E is an ordinary key again and the scroll is the loupe's
    ask(&mut h, "engine.execute", json!({"command": "app.setSpeedKey", "params": {"key": "E", "control": null}}));
    ask(&mut h, "ui.key", json!({"key": "E", "action": "down"}));
    assert!(h.app.speed_edit.is_none());
    ask(&mut h, "ui.key", json!({"key": "E", "action": "up"}));
}

#[test]
fn outside_develop_the_keys_are_plain_shortcuts() {
    let services = Services { png: None, ..Default::default() };
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.run("module.library", json!({})).expect("Library");
    h.step();
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Library, "{:?}", h.app.ui.view);
    ask(&mut h, "ui.key", json!({"key": "E", "action": "down"}));
    assert!(h.app.speed_edit.is_none(), "Library's E is its own key: {:?} {:?}", h.app.ui.module, h.app.ui.view);
    ask(&mut h, "ui.key", json!({"key": "E", "action": "up"}));
}

#[test]
fn the_reference_lists_the_speed_keys() {
    let mut h = develop_loupe();
    let (own, _) = crate::help_overlay::rows(&h.app, crate::module::ModuleId::Develop, false);
    assert!(own.iter().any(|(_, what)| what == crate::i18n::tr("Exposure")), "{own:?}");
    ask(&mut h, "engine.execute", json!({"command": "app.shortcuts", "params": {}}));
    let r = h.request("ui.widgets", json!({}), T);
    assert!(r.to_string().contains("speedKey:light.exposure"), "the Keyboard Shortcuts dialog lists the speed keys");
    let r = h.request("engine.execute", json!({"command": "app.speedKeys", "params": {}}), T);
    assert!(r.to_string().contains("light.exposure"), "{r}");
}
