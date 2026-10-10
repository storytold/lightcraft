//! Headless tests of the editable keymap (Help ▸ Keyboard Shortcuts): a changed shortcut fires
//! its command and not the old one's, the editor records a key press without running it, Esc
//! cancels recording without closing the dialog, and the keymap survives a save/load of `ui.json`.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{Dialog, ViewMode};
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = DacApp::new(dac_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn key(h: &mut Headless, k: &str, shift: bool) {
    let r = h.request("ui.key", json!({"key": k, "shift": shift}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
fn rebound_shortcut_fires_and_the_old_key_does_not() {
    let mut h = demo();
    let r = h.app.run("app.setShortcut", json!({"id": "view.survey", "shortcut": "Shift+K"})).unwrap();
    assert_eq!(r["shortcut"], "Shift+K", "{r}");
    // the old key (N) no longer opens Survey…
    key(&mut h, "N", false);
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    // …the new one does, and menus show it
    key(&mut h, "K", true);
    assert_eq!(h.app.ui.view, ViewMode::Survey);
    let entry = crate::menus::menu_entries(&h.app).into_iter().find(|e| e.id == "view.survey").unwrap();
    assert_eq!(entry.shortcut.as_deref(), Some("Shift+K"));
    // the keymap is saved with the UI state
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap();
    assert_eq!(back.settings.keymap.get("view.survey").map(String::as_str), Some("Shift+K"));
}

#[test]
fn the_editor_records_a_key_and_esc_cancels_without_closing() {
    let mut h = demo();
    h.app.ui.settings.keymap_set = crate::shortcuts::KeymapSet::Alternative;
    h.app.run("app.shortcuts", json!({})).unwrap();
    h.step();
    h.step();
    // the list is longer than the dialog: narrow it so the row is on screen
    click(&mut h, "field:shortcutsSearch");
    assert_eq!(h.request("ui.text", json!({"text": "compare"}), T)["ok"], true);
    h.step();
    h.step();
    // record Shift+J for Compare: the key press is taken, not run
    click(&mut h, "button:shortcut-view.compare");
    assert_eq!(h.app.recording_shortcut.as_deref(), Some("view.compare"));
    key(&mut h, "J", true);
    assert_eq!(h.app.recording_shortcut, None);
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.compare"), Some("Shift+J"));
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Shortcuts)));
    // Esc while recording cancels; the dialog stays open and nothing changes
    click(&mut h, "button:shortcut-view.compare");
    key(&mut h, "Escape", false);
    assert_eq!(h.app.recording_shortcut, None);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Shortcuts)));
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.compare"), Some("Shift+J"));
    // the next Esc closes it; the new key works
    key(&mut h, "Escape", false);
    assert!(h.app.ui.dialog.is_none());
    key(&mut h, "J", true);
    assert_eq!(h.app.ui.view, ViewMode::Compare);
    // Reset All restores the declared keys
    h.app.run("app.resetShortcuts", json!({})).unwrap();
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.compare"), Some("Shift+C"));
}

#[test]
fn a_key_given_to_a_command_overrides_a_fixed_key() {
    let mut h = demo();
    let before = h.app.session.active().and_then(|id| h.app.session.catalog.photo(id)).map(|p| p.rating);
    // 3 (rate ★★★) now opens Survey instead
    h.app.run("app.setShortcut", json!({"id": "view.survey", "shortcut": "3"})).unwrap();
    key(&mut h, "3", false);
    assert_eq!(h.app.ui.view, ViewMode::Survey);
    let after = h.app.session.active().and_then(|id| h.app.session.catalog.photo(id)).map(|p| p.rating);
    assert_eq!(before, after, "the rating key must not fire too");
}

/// Open the editor, filtered to `search`, and start recording for `id`.
fn record(h: &mut Headless, search: &str, id: &str) {
    if h.app.ui.dialog.is_none() {
        h.app.run("app.shortcuts", json!({})).unwrap();
        h.step();
        h.step();
        click(h, "field:shortcutsSearch");
        assert_eq!(h.request("ui.text", json!({"text": search}), T)["ok"], true);
        h.step();
        h.step();
    }
    click(h, &format!("button:shortcut-{id}"));
    assert_eq!(h.app.recording_shortcut.as_deref(), Some(id));
}

/// Pressing ⌘ (a key event of its own) before S used to record `Cmd+SuperLeft`, so no ⌘ shortcut
/// could be set; ⌘C/⌘X/⌘V arrive as copy/cut/paste events, not keys, and were ignored.
#[test]
fn modifiers_wait_for_their_key_and_clipboard_keys_record() {
    let mut h = demo();
    record(&mut h, "survey", "view.survey");
    let r = h.request("ui.key", json!({"key": "SuperLeft", "cmd": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.recording_shortcut.as_deref(), Some("view.survey"), "still waiting after ⌘ alone");
    let r = h.request("ui.key", json!({"key": "K", "cmd": true, "shift": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.recording_shortcut, None);
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.survey"), Some("Cmd+Shift+K"));
    // ⌘⌥C: the windowing layer sends a Copy event with the modifiers held
    record(&mut h, "survey", "view.survey");
    let m = egui::Modifiers { alt: true, ..egui::Modifiers::COMMAND };
    h.app.synthetic.push(egui::Event::Key { key: egui::Key::AltLeft, physical_key: None, pressed: true, repeat: false, modifiers: m });
    h.app.synthetic.push(egui::Event::Copy);
    h.step();
    h.step();
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.survey"), Some("Cmd+Alt+C"));
    // a saved modifier-only shortcut (from the bug) means no shortcut
    assert_eq!(crate::shortcuts::parse("Cmd+SuperLeft"), None);
}

// ----------------------------------------------------------------------------------- Classic set (P1.3)

/// In the Classic set no two commands share a key (except the declared contextual partners), and
/// every Classic entry names a bindable command and parses.
#[test]
fn classic_keys_parse_and_do_not_conflict() {
    use crate::shortcuts::{CLASSIC, CONTEXTUAL, KeymapSet, bindable, default_in, find_bindable, parse};
    for (id, sc) in CLASSIC {
        assert!(find_bindable(id).is_some(), "{id} is a command");
        if let Some(sc) = sc {
            assert!(parse(sc).is_some(), "{sc} parses");
        }
    }
    let keys: Vec<(&str, (egui::Modifiers, egui::Key))> =
        bindable().iter().filter_map(|b| default_in(KeymapSet::Classic, b).and_then(parse).map(|k| (b.id, k))).collect();
    for (i, (a, k)) in keys.iter().enumerate() {
        for (b, k2) in &keys[i + 1..] {
            let partners = CONTEXTUAL.iter().any(|(x, y)| (x == a && y == b) || (x == b && y == a));
            assert!(k != k2 || partners, "{a} and {b} share a key in the Classic set");
        }
    }
}

#[test]
fn classic_keys_switch_modules_views_and_panels() {
    let mut h = demo();
    assert_eq!(h.app.ui.settings.keymap_set, crate::shortcuts::KeymapSet::Classic, "Classic is the default");
    // G grid, E loupe (Library), D Develop
    key(&mut h, "G", false);
    assert!(matches!(h.app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid));
    key(&mut h, "E", false);
    assert_eq!((h.app.ui.view, h.app.ui.module), (ViewMode::Detail, crate::module::ModuleId::Library));
    key(&mut h, "D", false);
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
    // R crop, Q remove, ⇧W masking
    key(&mut h, "R", false);
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Crop);
    key(&mut h, "Q", false);
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Remove);
    key(&mut h, "W", true);
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Masking);
    // ⌘⌥1…7 and ⌘⌥↑
    let r = h.request("ui.key", json!({"key": "3", "cmd": true, "alt": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Map);
    h.request("ui.key", json!({"key": "ArrowUp", "cmd": true, "alt": true}), T);
    h.step();
    h.step();
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Develop);
    // Tab hides both side panels, F5 the module bar, T the toolbar, L lights out
    key(&mut h, "Tab", false);
    assert!(!h.app.ui.left_panel && !h.app.ui.right_edge);
    key(&mut h, "F5", false);
    assert!(!h.app.ui.module_bar);
    key(&mut h, "T", false);
    assert!(!h.app.ui.toolbar);
    key(&mut h, "L", false);
    assert_eq!(h.app.ui.lights_out, crate::module::LightsOut::Dim);
    key(&mut h, "Tab", true);
    assert!(h.app.ui.left_panel && h.app.ui.right_edge && h.app.ui.module_bar, "⇧Tab shows all");
    // G: the Library grid; [ / ] rate there, ` toggles the flag
    key(&mut h, "G", false);
    let id = h.app.session.active().unwrap();
    h.app.run("photo.rate", json!({"rating": 2})).unwrap();
    key(&mut h, "]", false);
    assert_eq!(h.app.session.catalog.photo(id).unwrap().rating, 3);
    key(&mut h, "[", false);
    key(&mut h, "[", false);
    assert_eq!(h.app.session.catalog.photo(id).unwrap().rating, 1);
    key(&mut h, "`", false);
    assert_eq!(h.app.session.catalog.photo(id).unwrap().flag, dac_catalog::Flag::Pick);
    key(&mut h, "C", false);
    assert_eq!(h.app.ui.view, ViewMode::Compare);
    // ⇧E: the secondary window's loupe (not Export)
    key(&mut h, "E", true);
    assert!(h.app.ui.second_window && h.app.ui.dialog.is_none());
}

#[test]
fn the_alternative_set_restores_the_previous_keys_and_keymaps_round_trip() {
    let mut h = demo();
    h.app.run("app.keymapSet", json!({"set": "alternative"})).unwrap();
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.detail"), Some("D"));
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "panel.crop"), Some("C"));
    h.app.run("app.keymapSet", json!({"set": "classic"})).unwrap();
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "panel.crop"), Some("R"));
    assert!(h.app.run("app.keymapSet", json!({"set": "emacs"})).is_err());
    // export / import a keymap file
    h.app.run("app.setShortcut", json!({"id": "view.survey", "shortcut": "Cmd+Shift+K"})).unwrap();
    let dir = std::env::temp_dir().join(format!("keymap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("keys.json");
    h.app.run("app.keymapExport", json!({"path": path.to_str().unwrap()})).unwrap();
    h.app.run("app.resetShortcuts", json!({})).unwrap();
    h.app.run("app.keymapSet", json!({"set": "alternative"})).unwrap();
    let r = h.app.run("app.keymapImport", json!({"path": path.to_str().unwrap()})).unwrap();
    assert_eq!(r["set"], "classic");
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.survey"), Some("Cmd+Shift+K"));
    // hostile files are errors or skipped entries, never panics
    std::fs::write(&path, b"{\"keymap\": {\"no.such\": \"K\", \"view.survey\": 7, \"view.people\": \"Hyper+X\"}}").unwrap();
    let r = h.app.run("app.keymapImport", json!({"path": path.to_str().unwrap()})).unwrap();
    assert_eq!(r["skipped"].as_array().unwrap().len(), 3);
    std::fs::write(&path, b"not json").unwrap();
    assert!(h.app.run("app.keymapImport", json!({"path": path.to_str().unwrap()})).is_err());
    std::fs::write(&path, b"[1,2]").unwrap();
    assert!(h.app.run("app.keymapImport", json!({"path": path.to_str().unwrap()})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
    // the set survives a save/load of ui.json
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap();
    assert_eq!(back.settings.keymap_set, h.app.ui.settings.keymap_set);
}
