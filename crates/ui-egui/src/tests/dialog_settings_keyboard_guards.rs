//! Native/candidate characterization of Settings Tab ownership boundaries.
use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};
use serde_json::{Value, json};
use std::time::Duration;
const T: Duration = Duration::from_secs(20);
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
fn demo() -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1400.0, 1100.0], 1.0);
    assert!(h.settle(T));
    ask(&mut h, "engine.execute", json!({"command":"app.settings","params":{"tab":"import"}}));
    h
}
fn focused(h: &Headless) -> Option<String> {
    let id = h.view.ctx.memory(|m| m.focused())?;
    if let Some((name, _)) = h.app.widget_hits.iter().find(|(_, hit)| hit.id == id) {
        return Some(name.clone());
    }
    let rect = h.view.ctx.read_response(id)?.rect;
    h.app.widgets.iter().find(|(_, r)| *r == rect).map(|(name, _)| name.clone())
}
fn raw_tab(shift: bool, pressed: bool) -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Tab,
        physical_key: Some(egui::Key::Tab),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers { shift, ..Default::default() },
    }
}

#[test]
fn a_raw_batch_of_tab_presses_and_releases_matches_the_native_last_direction() {
    let mut h = demo();
    ask(&mut h, "ui.clickWidget", json!({"id":"field:settings.copyright"}));
    assert_eq!(focused(&h).as_deref(), Some("field:settings.copyright"));
    h.events.extend([raw_tab(false, true), raw_tab(false, false), raw_tab(false, true), raw_tab(false, false)]);
    h.step();
    steps(&mut h);
    eprintln!("SETTINGS_RAW_TWO_FORWARD={:?}", focused(&h));
    assert_eq!(focused(&h).as_deref(), Some("field:settings.creator"));
    h.events.extend([raw_tab(false, true), raw_tab(false, false), raw_tab(true, true), raw_tab(true, false)]);
    h.step();
    steps(&mut h);
    eprintln!("SETTINGS_RAW_FORWARD_THEN_REVERSE={:?}", focused(&h));
    assert_eq!(focused(&h).as_deref(), Some("field:settings.copyright"));
    // A release by itself is not a focus movement.
    h.events.push(raw_tab(false, false));
    h.step();
    steps(&mut h);
    assert_eq!(focused(&h).as_deref(), Some("field:settings.copyright"));
}

#[test]
fn modified_tab_leaves_a_real_text_draft_with_its_owner() {
    let mut h = demo();
    let before = h.app.session.import_defaults.copyright.clone();
    ask(&mut h, "ui.clickWidget", json!({"id":"field:settings.copyright"}));
    ask(&mut h, "ui.text", json!({"text":"Modified Tab Draft"}));
    for params in [json!({"key":"Tab","alt":true}), json!({"key":"Tab","ctrl":true}), json!({"key":"Tab","cmd":true})] {
        ask(&mut h, "ui.key", params);
        assert_eq!(focused(&h).as_deref(), Some("field:settings.copyright"));
        assert_eq!(h.app.session.import_defaults.copyright, before);
    }
    ask(&mut h, "ui.key", json!({"key":"Escape"}));
    assert_eq!(h.app.session.import_defaults.copyright, before);
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "import".into() }));
}

#[test]
fn an_open_native_preset_combo_keeps_tab_and_escape_out_of_the_settings_page() {
    let mut h = demo();
    let before = h.app.session.import_defaults.clone();
    ask(&mut h, "ui.clickWidget", json!({"id":"combo:settingsRawPreset"}));
    assert!(egui::Popup::is_any_open(&h.view.ctx));
    ask(&mut h, "ui.key", json!({"key":"Tab"}));
    eprintln!("SETTINGS_COMBO_TAB_FOCUS={:?} popup={}", focused(&h), egui::Popup::is_any_open(&h.view.ctx));
    assert!(egui::Popup::is_any_open(&h.view.ctx));
    assert_eq!(h.app.session.import_defaults, before);
    ask(&mut h, "ui.key", json!({"key":"Escape"}));
    assert!(!egui::Popup::is_any_open(&h.view.ctx));
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "import".into() }));
}

#[test]
fn changing_the_settings_tab_drops_the_old_field_and_disabled_chooser_is_not_a_stop() {
    let mut h = demo();
    ask(&mut h, "ui.clickWidget", json!({"id":"field:settings.copyright"}));
    ask(&mut h, "ui.text", json!({"text":"Tab Switch Draft"}));
    ask(&mut h, "ui.clickWidget", json!({"id":"button:settingsTab-general"}));
    assert_eq!(h.app.session.import_defaults.copyright, "Tab Switch Draft");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "general".into() }));
    ask(&mut h, "ui.clickWidget", json!({"id":"button:settingsTab-import"}));
    // Actual keyboard traversal must skip the disabled native folder picker.
    for _ in 0..64 {
        ask(&mut h, "ui.key", json!({"key":"Tab"}));
        assert_ne!(focused(&h).as_deref(), Some("button:settingsAutoFolder"));
        if focused(&h).as_deref() == Some("button:dialogOk") {
            break;
        }
    }
    assert_eq!(focused(&h).as_deref(), Some("button:dialogOk"));
    assert_eq!(h.app.session.import_defaults.copyright, "Tab Switch Draft");
}

#[test]
fn a_tab_consumed_before_a_replacement_dialog_does_not_leak_to_its_fields() {
    let mut h = demo();
    ask(&mut h, "ui.clickWidget", json!({"id": "field:settings.copyright"}));
    let mut replaced = false;
    h.set_after_raw_input_hook(move |app| {
        if !replaced {
            app.ui.dialog = Some(Dialog::TextPrompt {
                title: "Settings replacement guard".into(),
                hint: "Name".into(),
                value: String::new(),
                command: "metadata.savePreset".into(),
                params: json!({}),
                key: "name".into(),
            });
            replaced = true;
        }
    });
    h.events.push(raw_tab(false, true));
    h.step();
    steps(&mut h);
    assert_eq!(focused(&h).as_deref(), Some("field:prompt"));
    h.events.push(raw_tab(false, false));
    h.step();
    steps(&mut h);
    assert_eq!(focused(&h).as_deref(), Some("field:prompt"), "an owned key release is not a new traversal");
    ask(&mut h, "ui.key", json!({"key": "Tab"}));
    assert_eq!(focused(&h).as_deref(), Some("button:dialogCancel"), "the replacement owns its own next Tab");
}

fn reach(h: &mut Headless, name: &str) {
    for _ in 0..64 {
        if focused(h).as_deref() == Some(name) {
            return;
        }
        ask(h, "ui.key", json!({"key":"Tab"}));
    }
    panic!("Tab did not reach {name}: {:?}", focused(h));
}

#[test]
fn coalesced_tab_then_enter_activates_the_new_real_sidecar_choice() {
    let mut h = demo();
    ask(&mut h, "ui.clickWidget", json!({"id":"button:settingsXmpNaming-0"}));
    reach(&mut h, "button:settingsXmpNaming-0");
    let enter = |pressed| egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: Some(egui::Key::Enter),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    h.events.extend([raw_tab(false, true), raw_tab(false, false), enter(true), enter(false)]);
    h.step();
    steps(&mut h);
    let value = h.app.run("library.xmpPreferences", json!({})).unwrap();
    eprintln!("SETTINGS_COALESCED_TAB_ENTER focus={:?} xmp={value}", focused(&h));
    assert_eq!(focused(&h).as_deref(), Some("button:settingsXmpNaming-1"));
    assert_eq!(value["naming"], "full", "Enter belongs to the newly focused real choice");
}

#[test]
fn coalesced_tab_then_text_preserves_native_departing_field_order() {
    let mut h = demo();
    ask(&mut h, "ui.clickWidget", json!({"id":"field:settings.copyright"}));
    h.events.extend([raw_tab(false, true), raw_tab(false, false), egui::Event::Text("Coalesced Creator Draft".into())]);
    h.step();
    steps(&mut h);
    eprintln!(
        "SETTINGS_COALESCED_TAB_TEXT focus={:?} copyright={:?} creator={:?}",
        focused(&h),
        h.app.session.import_defaults.copyright,
        h.app.session.import_defaults.creator
    );
    assert_eq!(focused(&h).as_deref(), Some("field:settings.creator"));
    ask(&mut h, "ui.key", json!({"key":"Enter"}));
    // Native TextEdit handles this pass's text on the departing field before Away commits.
    // Focus has moved to Creator, but the text must not be silently rerouted there.
    assert_eq!(h.app.session.import_defaults.creator, "");
    assert_eq!(h.app.session.import_defaults.copyright, "Coalesced Creator Draft");
}

#[test]
fn held_tab_repeat_moves_once_per_real_render_without_release_activation() {
    let mut h = demo();
    ask(&mut h, "ui.clickWidget", json!({"id":"field:settings.copyright"}));
    h.events.push(raw_tab(false, true));
    h.step();
    steps(&mut h);
    assert_eq!(focused(&h).as_deref(), Some("field:settings.creator"));
    let mut repeated = raw_tab(false, true);
    if let egui::Event::Key { repeat, .. } = &mut repeated {
        *repeat = true;
    }
    h.events.push(repeated);
    h.step();
    steps(&mut h);
    assert_eq!(focused(&h).as_deref(), Some("check:settings.autoWriteXmp"));
    let before = h.app.session.xmp.auto_write;
    h.events.push(raw_tab(false, false));
    h.step();
    steps(&mut h);
    assert_eq!(focused(&h).as_deref(), Some("check:settings.autoWriteXmp"));
    assert_eq!(h.app.session.xmp.auto_write, before, "Tab release cannot activate the new checkbox");
}
