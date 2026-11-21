//! Headless tests of focus peaking (#4396): View ▸ Focus Peaking paints the sharp edges of the photo
//! in Detail and Survey, as a view setting that never touches the edit, and refuses malformed
//! parameters.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::render::Slot;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
const GREEN: [u8; 3] = [0, 255, 0];

fn demo(view: &str) -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.app.renderer.keep_pixels = true;
    let r = h.request("ui.set", json!({"view": view}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn run(h: &mut Headless, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": "view.focusPeaking", "params": params}), T);
    h.settle(SETTLE);
    r
}

fn share(t: &crate::render::Tex, c: [u8; 3]) -> f64 {
    let px = &t.pixels.as_ref().expect("pixels kept").pixels;
    px.iter().filter(|p| [p.r(), p.g(), p.b()] == c).count() as f64 / px.len().max(1) as f64
}

fn loupe_share(h: &Headless) -> f64 {
    share(h.app.renderer.textures.get(&Slot::Main).expect("loupe render"), GREEN)
}

#[test]
fn focus_peaking_paints_edges_in_detail_without_touching_the_edit() {
    let mut h = demo("detail");
    let id = h.app.session.active().expect("a photo");
    let before = h.app.session.catalog.photo(id).expect("photo").clone();
    assert!(!h.app.ui.focus_peaking, "off by default");
    assert!(loupe_share(&h) < 0.001, "no peaking colour without it: {}", loupe_share(&h));
    let r = run(&mut h, json!({"color": [0, 255, 0], "sensitivity": 60}));
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"], json!({"on": false, "color": "#00ff00", "sensitivity": 60.0}), "a setting alone leaves it off");
    let r = run(&mut h, json!({}));
    assert_eq!(r["result"]["on"], true, "{r}");
    let on = loupe_share(&h);
    assert!(on > 0.0005 && on < 0.6, "the demo photo's few sharp edges painted, not the whole photo: {on}");
    let more = run(&mut h, json!({"sensitivity": 100}));
    assert_eq!(more["ok"], true, "{more}");
    assert!(loupe_share(&h) > on, "higher sensitivity marks more: {} vs {on}", loupe_share(&h));
    let after = h.app.session.catalog.photo(id).expect("photo").clone();
    assert_eq!((after.develop.clone(), after.history.len()), (before.develop.clone(), before.history.len()), "no edit, no history step");
    let r = run(&mut h, json!({"on": false}));
    assert_eq!(r["result"]["on"], false, "{r}");
    assert!(loupe_share(&h) < 0.001, "off again: {}", loupe_share(&h));
}

#[test]
fn focus_peaking_paints_survey_tiles() {
    let mut h = demo("photoGrid");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "survey"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let tiles = |h: &Headless| -> Vec<f64> {
        h.app.renderer.textures.iter().filter(|(s, t)| matches!(s, Slot::Compare(_)) && t.pixels.is_some()).map(|(_, t)| share(t, GREEN)).collect()
    };
    assert!(tiles(&h).len() >= 2, "survey tiles rendered");
    assert!(tiles(&h).iter().all(|s| *s < 0.001), "{:?}", tiles(&h));
    let r = run(&mut h, json!({"on": true, "color": "#00ff00"}));
    assert_eq!(r["ok"], true, "{r}");
    let on = tiles(&h);
    assert!(on.len() >= 2 && on.iter().all(|s| *s > 0.002), "every tile shows peaking: {on:?}");
}

#[test]
fn focus_peaking_refuses_malformed_parameters_and_changes_nothing() {
    let mut h = demo("detail");
    let start = (h.app.ui.focus_peaking, h.app.ui.peaking_color, h.app.ui.peaking_sensitivity);
    for bad in [
        json!({"on": "yes"}),
        json!({"on": 1}),
        json!({"color": "red"}),
        json!({"color": "#12345"}),
        json!({"color": [255, 0]}),
        json!({"color": [256, 0, 0]}),
        json!({"color": [1.5, 0, 0]}),
        json!({"color": [-1, 0, 0]}),
        json!({"sensitivity": 101}),
        json!({"sensitivity": -1}),
        json!({"sensitivity": "high"}),
        json!({"on": true, "sensitivity": 500}),
    ] {
        let r = run(&mut h, bad.clone());
        assert_eq!(r["ok"], false, "{bad} must be refused: {r}");
        assert!(r.to_string().contains("view.focusPeaking"), "the error names the command: {r}");
        assert_eq!((h.app.ui.focus_peaking, h.app.ui.peaking_color, h.app.ui.peaking_sensitivity), start, "{bad} changed nothing");
    }
}

#[test]
fn focus_peaking_is_in_the_view_menu_and_settings_are_sanitized() {
    assert!(crate::menus::ui_commands().any(|c| c.0 == "view.focusPeaking" && c.3 == "View"));
    let mut ui = crate::state::UiState { peaking_sensitivity: f32::NAN, ..Default::default() }.sanitized();
    assert_eq!(ui.peaking_sensitivity, lightcraft_pipeline::visualize::PEAKING_SENSITIVITY as f32);
    ui.peaking_sensitivity = 900.0;
    assert_eq!(ui.sanitized().peaking_sensitivity, 100.0);
}
