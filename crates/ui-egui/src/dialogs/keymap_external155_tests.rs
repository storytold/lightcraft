use crate::LightcraftApp;
use crate::headless::Headless;
use crate::state::{Dialog, ViewMode};
use serde_json::{Value, json};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let result = h.request(method, params, WAIT);
    assert_eq!(result["ok"], true, "{method}: {result}");
    h.settle(WAIT);
    result
}

fn editor() -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
    app.ui.view = ViewMode::PhotoGrid;
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    ask(&mut h, "engine.execute", json!({"command": "app.shortcuts"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::Shortcuts));
    h
}

fn search(h: &mut Headless, text: &str) {
    ask(h, "ui.clickWidget", json!({"id": "field:shortcutsSearch"}));
    ask(h, "ui.key", json!({"key": "A", "cmd": true}));
    ask(h, "ui.text", json!({"text": text}));
}

fn click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id}));
}

fn binding<'a>(h: &'a Headless, id: &str) -> Option<&'a str> {
    crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, id)
}

#[test]
fn keymap_external155_search_capture_conflict_and_restore() {
    let mut h = editor();
    search(&mut h, "view.survey");
    let widgets = ask(&mut h, "ui.widgets", json!({})).to_string();
    assert!(widgets.contains("button:shortcut-view.survey"), "{widgets}");
    assert!(!widgets.contains("button:shortcut-view.detail"), "search filters commands: {widgets}");
    let original_view = h.app.ui.view;
    click(&mut h, "button:shortcut-view.survey");
    assert_eq!(h.app.recording_shortcut.as_deref(), Some("view.survey"));
    ask(&mut h, "ui.key", json!({"key": "D"}));
    assert_eq!(h.app.recording_shortcut, None);
    assert_eq!(binding(&h, "view.survey"), Some("D"));
    assert_eq!(binding(&h, "view.detail"), None, "conflicting default yields to the assigned key");
    assert_eq!(h.app.ui.view, original_view, "capture must not also open Detail");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Shortcuts));
    click(&mut h, "button:shortcutReset-view.survey");
    assert_eq!(binding(&h, "view.survey"), Some("N"));
    assert_eq!(binding(&h, "view.detail"), None, "restoring the new owner does not erase another command's explicit unbinding");
    click(&mut h, "button:shortcutsResetAll");
    assert_eq!(binding(&h, "view.detail"), Some("D"));
    assert!(h.app.ui.settings.keymap.is_empty());
}

#[test]
fn keymap_external155_clear_show_all_restore_and_reset_all() {
    let mut h = editor();
    search(&mut h, "view.survey");
    click(&mut h, "button:shortcutClear-view.survey");
    assert_eq!(binding(&h, "view.survey"), None);
    let widgets = ask(&mut h, "ui.widgets", json!({})).to_string();
    assert!(!widgets.contains("button:shortcut-view.survey"), "unbound row hidden by default: {widgets}");
    click(&mut h, "check:shortcuts.showAll");
    let widgets = ask(&mut h, "ui.widgets", json!({})).to_string();
    assert!(widgets.contains("button:shortcut-view.survey"), "show all reveals unbound row: {widgets}");
    // Clicking a disabled clear target is a no-op, not a new binding.
    let cleared = h.app.ui.settings.keymap.clone();
    click(&mut h, "button:shortcutClear-view.survey");
    assert_eq!(h.app.ui.settings.keymap, cleared);
    click(&mut h, "button:shortcutReset-view.survey");
    assert_eq!(binding(&h, "view.survey"), Some("N"));
    click(&mut h, "button:shortcut-view.survey");
    ask(&mut h, "ui.key", json!({"key": "F9"}));
    assert_eq!(binding(&h, "view.survey"), Some("F9"));
    ask(&mut h, "engine.execute", json!({"command": "app.setShortcut", "params": {"id": "view.detail", "shortcut": "F10"}}));
    click(&mut h, "button:shortcutsResetAll");
    assert!(h.app.ui.settings.keymap.is_empty());
    assert_eq!(binding(&h, "view.survey"), Some("N"));
    assert_eq!(binding(&h, "view.detail"), Some("D"));
    assert_eq!(h.app.recording_shortcut, None);
}

#[test]
fn keymap_external155_escape_and_reserved_capture_preserve_bindings() {
    let mut h = editor();
    search(&mut h, "view.survey");
    let original = h.app.ui.settings.keymap.clone();
    click(&mut h, "button:shortcut-view.survey");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert_eq!(h.app.recording_shortcut, None);
    assert_eq!(h.app.ui.dialog, Some(Dialog::Shortcuts), "Escape cancels capture, not its dialog");
    assert_eq!(h.app.ui.settings.keymap, original);
    click(&mut h, "button:shortcut-view.survey");
    ask(&mut h, "ui.key", json!({"key": "Q", "cmd": true}));
    assert_eq!(h.app.recording_shortcut, None);
    assert_eq!(h.app.ui.settings.keymap, original, "reserved chord cannot change settings");
    assert_eq!(binding(&h, "view.survey"), Some("N"));
    assert_eq!(h.app.ui.dialog, Some(Dialog::Shortcuts));
}

#[test]
fn keymap_external155_measuring_does_not_take_capture_or_register_controls() {
    let ctx = egui::Context::default();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
    app.install_framework(&ctx);
    app.recording_shortcut = Some("view.survey".into());
    let raw = egui::RawInput {
        events: vec![egui::Event::Key {
            key: egui::Key::F9,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        ..Default::default()
    };
    let mut output = ctx.run_ui(raw, |ui| {
        let t = crate::theme::Tokens::get(ui.ctx());
        crate::widgets::take_registries(&ctx);
        let mut sizing = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
        super::body(&mut app, &mut sizing, &t);
        assert_eq!(app.recording_shortcut.as_deref(), Some("view.survey"));
        assert!(app.ui.settings.keymap.is_empty());
        let (rects, hits) = crate::widgets::take_registries(&ctx);
        assert!(rects.is_empty() && hits.is_empty(), "native sizing registered controls");
        craft::ui::runtime::measure::run(&ctx, || super::body(&mut app, ui, &t));
        assert_eq!(app.recording_shortcut.as_deref(), Some("view.survey"));
        assert!(app.ui.settings.keymap.is_empty());
        let (rects, hits) = crate::widgets::take_registries(&ctx);
        assert!(rects.is_empty() && hits.is_empty(), "SDK measuring registered controls");
        // (the pass that had the F9 still has it: a rerun of the pass, which egui gives no events, has none to keep)
        if ctx.current_pass_index() == 0 {
            assert!(ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::F9, pressed: true, .. }))));
        }
    });
    output.textures_delta.clear();
}
