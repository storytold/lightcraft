//! Characterization of Settings Tab ownership boundaries (every Settings control is a framework control).
use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};
use serde_json::{Value, json};
use std::time::Duration;
/// A generous settle budget: the point is that the dialog settles, and hosted runners are several times slower
/// than a developer machine.
const T: Duration = Duration::from_secs(90);
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
/// The preset picker is a framework `Select`: its list is a framework popup above the Settings dialog.
fn popup_open(h: &Headless) -> bool {
    crate::dialogs::popup_open(&h.view.ctx)
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
fn a_raw_batch_of_tab_presses_and_releases_moves_once_per_press_in_its_own_direction() {
    let mut h = demo();
    ask(&mut h, "ui.clickWidget", json!({"id":"field:settings.copyright"}));
    assert_eq!(focused(&h).as_deref(), Some("field:settings.copyright"));
    // The framework moves the focus once for each Tab press of a batch: two forward presses walk two stops.
    h.events.extend([raw_tab(false, true), raw_tab(false, false), raw_tab(false, true), raw_tab(false, false)]);
    h.step();
    steps(&mut h);
    eprintln!("SETTINGS_RAW_TWO_FORWARD={:?}", focused(&h));
    assert_eq!(focused(&h).as_deref(), Some("check:settings.autoWriteXmp"));
    // A forward press then a reverse one walks a stop on and back to where it was.
    h.events.extend([raw_tab(false, true), raw_tab(false, false), raw_tab(true, true), raw_tab(true, false)]);
    h.step();
    steps(&mut h);
    eprintln!("SETTINGS_RAW_FORWARD_THEN_REVERSE={:?}", focused(&h));
    assert_eq!(focused(&h).as_deref(), Some("check:settings.autoWriteXmp"));
    // A release by itself is not a focus movement.
    h.events.push(raw_tab(false, false));
    h.step();
    steps(&mut h);
    assert_eq!(focused(&h).as_deref(), Some("check:settings.autoWriteXmp"));
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
fn an_open_preset_combo_closes_on_tab_and_escape_stays_out_of_the_settings_page() {
    let mut h = demo();
    let before = h.app.session.import_defaults.clone();
    ask(&mut h, "ui.clickWidget", json!({"id":"combo:settingsRawPreset"}));
    assert!(popup_open(&h));
    // The framework list closes when Tab leaves it, choosing nothing, and the focus moves on.
    ask(&mut h, "ui.key", json!({"key":"Tab"}));
    eprintln!("SETTINGS_COMBO_TAB_FOCUS={:?} popup={}", focused(&h), popup_open(&h));
    assert!(!popup_open(&h));
    assert_eq!(h.app.session.import_defaults, before);
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "import".into() }));
    // Escape closes an open list and does not reach the page: Settings stays.
    ask(&mut h, "ui.clickWidget", json!({"id":"combo:settingsRawPreset"}));
    assert!(popup_open(&h));
    ask(&mut h, "ui.key", json!({"key":"Escape"}));
    assert!(!popup_open(&h));
    assert_eq!(h.app.session.import_defaults, before);
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
fn coalesced_tab_then_text_goes_to_the_field_the_tab_moved_to() {
    let mut h = demo();
    let copyright = h.app.session.import_defaults.copyright.clone();
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
    // The framework moves the focus first, so the text that arrived with the Tab belongs to the
    // field that has the keyboard now, Creator; Copyright is left as it was.
    assert_eq!(h.app.session.import_defaults.creator, "Coalesced Creator Draft");
    assert_eq!(h.app.session.import_defaults.copyright, copyright);
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
