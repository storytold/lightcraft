//! Observe original/native-preservation preset-popup keyboard behavior; do not invent parity expectations.
use crate::{LightcraftApp, Services, headless::Headless};
use serde_json::{Value, json};
use std::time::Duration;
const T: Duration = Duration::from_secs(20);
/// The preset picker is a framework `Select`: its list is a framework popup above the Settings dialog.
fn popup_open(h: &Headless) -> bool {
    crate::dialogs::popup_open(&h.view.ctx)
}
fn steps(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}
fn ask(h: &mut Headless, method: &str, params: Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    steps(h);
}
fn snapshot(h: &Headless) -> Value {
    let id = h.view.ctx.memory(|m| m.focused());
    let response = id.and_then(|id| h.view.ctx.read_response(id));
    let names: Vec<String> = id
        .map(|id| {
            let exact: Vec<String> = h.app.widget_hits.iter().filter(|(_, hit)| hit.id == id).map(|(name, _)| name.clone()).collect();
            if !exact.is_empty() {
                exact
            } else {
                response
                    .as_ref()
                    .map(|r| h.app.widgets.iter().filter(|(_, rect)| *rect == r.rect).map(|(name, _)| name.clone()).collect())
                    .unwrap_or_default()
            }
        })
        .unwrap_or_default();
    json!({"focusId":id.map(|id|format!("{id:?}")),"focusNames":names,"response":response.map(|r|json!({"id":format!("{:?}",r.id),"layer":format!("{:?}",r.layer_id),"sense":format!("{:?}",r.sense),"enabled":r.enabled(),"rect":[r.rect.min.x,r.rect.min.y,r.rect.max.x,r.rect.max.y]})),"popupOpen":popup_open(h),"dialog":format!("{:?}",h.app.ui.dialog),"rawPreset":h.app.session.import_defaults.raw_preset,"otherPreset":h.app.session.import_defaults.other_preset,"perCamera":h.app.session.import_defaults.per_camera})
}
fn probe(key: &str, tab_first: bool) {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1400.0, 1100.0], 1.0);
    assert!(h.settle(T));
    ask(&mut h, "engine.execute", json!({"command":"app.settings","params":{"tab":"import"}}));
    let before = snapshot(&h);
    ask(&mut h, "ui.clickWidget", json!({"id":"combo:settingsRawPreset"}));
    assert!(popup_open(&h), "setup must open actual raw-default combo");
    let opened = snapshot(&h);
    let mut trace = vec![json!({"event":"before","state":before}), json!({"event":"opened","state":opened})];
    if tab_first {
        ask(&mut h, "ui.key", json!({"key":"Tab"}));
        trace.push(json!({"event":"Tab","state":snapshot(&h)}));
    }
    ask(&mut h, "ui.key", json!({"key":key}));
    trace.push(json!({"event":key,"state":snapshot(&h)}));
    if popup_open(&h) {
        ask(&mut h, "ui.key", json!({"key":"Escape"}));
        trace.push(json!({"event":"cleanup Escape","state":snapshot(&h)}));
    }
    eprintln!("SETTINGS_POPUP_TRACE={}", json!({"case":format!("{}{}",if tab_first{"Tab+"}else{"direct "},key),"steps":trace}));
    // Only setup is asserted here. Actual focus/underlying-page changes are compared to the
    // native receipt rather than silently declared good or bad by this characterization.
}
#[test]
fn raw_preset_popup_tab_then_enter_records_actual_owner_and_activation() {
    probe("Enter", true);
}
#[test]
fn raw_preset_popup_tab_then_space_records_actual_owner_and_activation() {
    probe("Space", true);
}
#[test]
fn raw_preset_popup_tab_then_escape_records_actual_owner_and_dismissal() {
    probe("Escape", true);
}
#[test]
fn raw_preset_popup_direct_keys_record_actual_owner_and_activation() {
    for key in ["Enter", "Space", "Escape"] {
        probe(key, false);
    }
}
