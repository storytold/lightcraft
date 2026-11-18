//! Headless tests of the editable keymap (Help ▸ Keyboard Shortcuts): a changed shortcut fires
//! its command and not the old one's, the editor records a key press without running it, Esc
//! cancels recording without closing the dialog, and the keymap survives a save/load of `ui.json`.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{Dialog, ViewMode};
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut session = lcc_engine::Session::with_demo();
    // newest first, the newest photo open: written around it
    session.execute("library.sort", &json!({"ascending": false})).unwrap();
    session.selection = lcc_engine::Selection::single(session.visible_cloned()[0]);
    let app = LccApp::new(session, services);
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

/// A declared key that yields to another command's saved key can be taken back with ↺.
#[test]
fn restore_takes_back_a_declared_key_another_command_holds() {
    use crate::shortcuts::shortcut_of;
    let mut h = demo();
    h.app.ui.settings.keymap.insert("photo.increaseRating".into(), "P".into());
    assert_eq!(shortcut_of(&h.app.ui.settings.keymap, "photo.pick"), None);
    h.app.run("app.shortcuts", json!({})).unwrap();
    h.step();
    h.step();
    click(&mut h, "check:shortcuts.showAll");
    click(&mut h, "field:shortcutsSearch");
    assert_eq!(h.request("ui.text", json!({"text": "flag as pick"}), T)["ok"], true);
    h.step();
    h.step();
    click(&mut h, "button:shortcutReset-photo.pick");
    assert_eq!(shortcut_of(&h.app.ui.settings.keymap, "photo.pick"), Some("P"));
    assert_eq!(shortcut_of(&h.app.ui.settings.keymap, "photo.increaseRating"), None);
    assert!(h.app.ui.settings.keymap.is_empty(), "{:?}", h.app.ui.settings.keymap);
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
    assert_eq!(h.app.session.catalog.photo(id).unwrap().flag, lcc_catalog::Flag::Pick);
    key(&mut h, "C", false);
    assert_eq!(h.app.ui.view, ViewMode::Compare);
    // ⇧E: the secondary window's loupe (not Export)
    key(&mut h, "E", true);
    assert!(h.app.ui.second_window && h.app.ui.dialog.is_none());
}

#[test]
fn the_alternative_set_restores_the_previous_keys_and_keymaps_round_trip() {
    let dir = lcc_testkit::TempDir::new("keymap");
    let mut h = demo();
    h.app.run("app.keymapSet", json!({"set": "alternative"})).unwrap();
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.detail"), Some("D"));
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "panel.crop"), Some("C"));
    h.app.run("app.keymapSet", json!({"set": "classic"})).unwrap();
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "panel.crop"), Some("R"));
    assert!(h.app.run("app.keymapSet", json!({"set": "emacs"})).is_err());
    // export / import a keymap file
    h.app.run("app.setShortcut", json!({"id": "view.survey", "shortcut": "Cmd+Shift+K"})).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("keys.json");
    h.app.run("app.keymapExport", json!({"path": path.to_str().unwrap()})).unwrap();
    h.app.run("app.resetShortcuts", json!({})).unwrap();
    h.app.run("app.keymapSet", json!({"set": "alternative"})).unwrap();
    let r = h.app.run("app.keymapImport", json!({"path": path.to_str().unwrap()})).unwrap();
    assert_eq!(r["set"], "classic");
    assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "view.survey"), Some("Cmd+Shift+K"));
    // hostile files are errors that change nothing (no half-applied set or keymap), never panics
    let before = (h.app.ui.settings.keymap_set, h.app.ui.settings.keymap.clone(), crate::shortcuts::active_set());
    let unchanged = |h: &Headless| (h.app.ui.settings.keymap_set, h.app.ui.settings.keymap.clone(), crate::shortcuts::active_set()) == before;
    std::fs::write(
        &path,
        b"{\"set\": \"alternative\", \"keymap\": {\"view.detail\": \"K\", \"no.such\": \"K\", \"view.survey\": 7, \"view.people\": \"Hyper+X\"}}",
    )
    .unwrap();
    let e = h.app.run("app.keymapImport", json!({"path": path.to_str().unwrap()})).unwrap_err();
    assert!(e.contains("no.such") && e.contains("view.survey") && e.contains("view.people"), "{e}");
    assert!(unchanged(&h));
    std::fs::write(&path, b"{\"set\": \"alternative\"}").unwrap();
    assert!(h.app.run("app.keymapImport", json!({"path": path.to_str().unwrap()})).is_err());
    assert!(unchanged(&h));
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

/// Classic's module keys (KEYC-COMPARE / KEYC-DEVELOP): backslash is the filter bar in the Library
/// grid and Show Original in a loupe; `=` / `-` thumbnail size; Home / End; ⌘U / ⇧⌘U in Develop.
#[test]
fn module_keys_are_scoped_by_module_and_view() {
    let mut h = demo();
    // (the Filter bar starts shown, as Lightroom Classic's: hidden here to see \ show it)
    h.app.ui.filter_bar = false;
    let press = |h: &mut Headless, k: &str, cmd: bool, shift: bool| {
        let r = h.request("ui.key", json!({"key": k, "cmd": cmd, "shift": shift}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
    };
    // Library loupe: backslash shows the original, the filter bar stays off
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    press(&mut h, "Backslash", false, false);
    assert!(!h.app.ui.filter_bar);
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    // grid: backslash toggles the filter bar
    key(&mut h, "G", false);
    press(&mut h, "Backslash", false, false);
    assert!(h.app.ui.filter_bar);
    // thumbnail size
    let size = h.app.ui.thumb_size;
    press(&mut h, "Equals", false, false);
    assert!(h.app.ui.thumb_size > size);
    press(&mut h, "Minus", false, false);
    press(&mut h, "Minus", false, false);
    assert!(h.app.ui.thumb_size < size);
    // Home / End
    let vis = h.app.session.visible_cloned();
    press(&mut h, "End", false, false);
    assert_eq!(h.app.session.active(), vis.last().copied());
    press(&mut h, "Home", false, false);
    assert_eq!(h.app.session.active(), vis.first().copied());
    // Develop: ⇧⌘U sets Auto white balance, ⌘U runs Auto (an undo step)
    key(&mut h, "D", false);
    h.settle(SETTLE);
    let undo = h.app.session.undo.len();
    press(&mut h, "U", true, true);
    let id = h.app.session.active().unwrap();
    assert_eq!(h.app.session.develop_of(id).unwrap().wb.mode, lcc_develop::WbMode::Auto);
    press(&mut h, "U", true, false);
    assert!(h.app.session.undo.len() >= undo + 2, "both keys made an edit");
    // ⇧Q starts a Luminance Range mask; ⌥⇧Q (spot mode cycling) without a spot is a no-op, not a crash
    let masks = h.app.session.develop_of(id).map(|d| d.masks.len()).unwrap_or(0);
    press(&mut h, "Q", false, true);
    let after = h.app.session.develop_of(id).map(|d| d.masks.len()).unwrap_or(0);
    assert_eq!(after, masks + 1, "Shift+Q adds a Luminance Range mask");
    let r = h.request("ui.key", json!({"key": "Q", "alt": true, "shift": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.run("spot.cycleMode", json!({})).is_err());
}

/// ⌘/ lists the current module's keys only, unless a search looks through everything.
#[test]
fn the_shortcuts_list_shows_the_current_modules_keys() {
    use crate::module::ModuleId;
    use crate::panels::keymap::in_module;
    assert!(in_module(ModuleId::Develop, "develop.auto"));
    assert!(in_module(ModuleId::Develop, "app.shortcuts"));
    assert!(!in_module(ModuleId::Develop, "album.toggleTarget"));
    assert!(in_module(ModuleId::Library, "album.toggleTarget"));
}

/// The Alternative set's edit-settings keys (Ctrl+C / Ctrl+V, ⇧Ctrl+V for Paste Selected).
fn alternative() -> Headless {
    let mut h = demo();
    h.app.ui.settings.keymap_set = crate::shortcuts::KeymapSet::Alternative;
    h
}

/// On Windows and Linux the windowing layer turns Ctrl+C / Ctrl+V into clipboard events, not key
/// presses: outside a text field they still copy the edit settings, and with ⇧ open Paste Selected
/// Settings (Alternative set; Classic's keys are below).
#[test]
fn clipboard_events_run_the_copy_and_paste_shortcuts() {
    let mut h = alternative();
    assert!(h.app.session.clipboard.is_none());
    h.events.push(egui::Event::Copy);
    h.settle(SETTLE);
    assert!(h.app.session.clipboard.is_some(), "Ctrl+C copied the edit settings");
    let shift_cmd = egui::Modifiers::SHIFT | egui::Modifiers::COMMAND;
    h.events.extend([egui::Event::ModifiersChanged(shift_cmd), egui::Event::Paste("text".into())]);
    h.settle(SETTLE);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::PasteSettings { .. })), "⇧Ctrl+V: {:?}", h.app.ui.dialog);
}

/// With no text on the system clipboard (empty, a screenshot) the windowing layer drops Ctrl+V's
/// press and sends only its release; the paste shortcuts still paste (Ctrl+V) and open Paste
/// Selected Settings (⇧Ctrl+V) (Alternative set).
#[test]
fn paste_shortcuts_work_with_no_text_on_the_clipboard() {
    let mut h = alternative();
    h.events.push(egui::Event::Copy);
    h.settle(SETTLE);
    assert!(h.app.session.clipboard.is_some());
    let undo = h.app.session.undo.len();
    let release = |m: egui::Modifiers| egui::Event::Key { key: egui::Key::V, physical_key: None, pressed: false, repeat: false, modifiers: m };
    h.events.extend([egui::Event::ModifiersChanged(egui::Modifiers::COMMAND), release(egui::Modifiers::COMMAND)]);
    h.settle(SETTLE);
    assert_eq!(h.app.session.undo.len(), undo + 1, "Ctrl+V pasted the settings");
    let shift_cmd = egui::Modifiers::SHIFT | egui::Modifiers::COMMAND;
    h.events.extend([egui::Event::ModifiersChanged(shift_cmd), release(shift_cmd)]);
    h.settle(SETTLE);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::PasteSettings { .. })), "⇧Ctrl+V: {:?}", h.app.ui.dialog);
    // a normal paste (press seen, then release) runs once, not twice
    h.app.ui.dialog = None;
    let undo = h.app.session.undo.len();
    h.events.extend([egui::Event::ModifiersChanged(egui::Modifiers::COMMAND), egui::Event::Paste("text".into())]);
    h.settle(SETTLE);
    h.events.push(release(egui::Modifiers::COMMAND));
    h.settle(SETTLE);
    assert_eq!(h.app.session.undo.len(), undo + 1, "one paste");
}

/// Windows also pastes with ⇧Insert and cuts with ⇧Delete: the clipboard events they become are
/// the plain Paste / Cut, not ⇧Ctrl+V (Paste Selected Settings).
#[test]
fn shift_insert_pastes_plainly() {
    let mut h = alternative();
    h.events.push(egui::Event::Copy);
    h.settle(SETTLE);
    let undo = h.app.session.undo.len();
    h.events.extend([egui::Event::ModifiersChanged(egui::Modifiers::SHIFT), egui::Event::Paste("text".into())]);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.dialog, None, "no Paste Selected Settings");
    assert_eq!(h.app.session.undo.len(), undo + 1, "the settings were pasted");
}

/// Classic's edit-settings keys reach their commands as clipboard events too: ⇧Ctrl+C opens Copy
/// Settings…, ⇧Ctrl+V pastes the settings (also when the windowing layer sends only the release),
/// and plain Ctrl+C / Ctrl+V leave the edit alone, as in Lightroom Classic.
#[test]
fn classic_clipboard_events_copy_and_paste_settings() {
    let mut h = demo();
    let shift_cmd = egui::Modifiers::SHIFT | egui::Modifiers::COMMAND;
    h.events.extend([egui::Event::ModifiersChanged(egui::Modifiers::COMMAND), egui::Event::Copy]);
    h.settle(SETTLE);
    assert!(h.app.session.clipboard.is_none(), "plain Ctrl+C copies no settings");
    h.events.extend([egui::Event::ModifiersChanged(shift_cmd), egui::Event::Copy]);
    h.settle(SETTLE);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::CopySettings { .. })), "⇧Ctrl+C: {:?}", h.app.ui.dialog);
    h.app.ui.dialog = None;
    h.app.run("develop.copy", json!({})).unwrap();
    // with no text on the clipboard the windowing layer sends only ⇧Ctrl+V's release: it pastes too
    let undo = h.app.session.undo.len();
    let release = egui::Event::Key { key: egui::Key::V, physical_key: None, pressed: false, repeat: false, modifiers: shift_cmd };
    h.events.extend([egui::Event::ModifiersChanged(shift_cmd), release]);
    h.settle(SETTLE);
    assert_eq!(h.app.session.undo.len(), undo + 1, "a release-only ⇧Ctrl+V pastes");
    let undo = h.app.session.undo.len();
    h.events.extend([egui::Event::ModifiersChanged(egui::Modifiers::COMMAND), egui::Event::Paste("text".into())]);
    h.settle(SETTLE);
    assert_eq!(h.app.session.undo.len(), undo, "plain Ctrl+V pastes no settings");
    h.events.extend([egui::Event::ModifiersChanged(shift_cmd), egui::Event::Paste("text".into())]);
    h.settle(SETTLE);
    assert_eq!(h.app.session.undo.len(), undo + 1, "⇧Ctrl+V pasted the settings");
    assert_eq!(h.app.ui.dialog, None);
}

/// ⌘⌥⇧C / ⌘⌥⇧V (Ctrl+Alt+Shift+C / V) copy and paste metadata, as in Lightroom Classic's Library,
/// both as key presses and as the clipboard events Windows and Linux turn them into; the edit
/// settings clipboard is left alone.
#[test]
fn cmd_alt_shift_c_and_v_copy_and_paste_metadata() {
    let mut h = demo();
    let ids: Vec<_> = h.app.session.visible().iter().copied().take(2).collect();
    let (a, b) = (ids[0], ids[1]);
    let title = |h: &Headless, id| h.app.session.catalog.photo(id).unwrap().meta.title.clone();
    h.app.run("library.select", json!({"ids": [a.0]})).unwrap();
    h.app.run("photo.setMeta", json!({"title": "Dawn", "creator": "Me"})).unwrap();
    h.app.run("library.select", json!({"ids": [b.0]})).unwrap();
    h.app.run("photo.setMeta", json!({"title": "Other"})).unwrap();
    let chord = json!({"cmd": true, "alt": true, "shift": true});
    let press = |h: &mut Headless, k: &str| {
        let mut p = chord.clone();
        p["key"] = json!(k);
        let r = h.request("ui.key", p, T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    };

    h.app.run("library.select", json!({"ids": [a.0]})).unwrap();
    press(&mut h, "C");
    assert_eq!(h.app.session.meta_clipboard.as_ref().map(|m| m["title"].clone()), Some(json!("Dawn")), "the key copied the metadata");
    h.app.run("library.select", json!({"ids": [b.0]})).unwrap();
    press(&mut h, "V");
    assert_eq!(title(&h, b), "Dawn", "the key pasted the metadata");
    assert_eq!(h.app.session.catalog.photo(b).unwrap().meta.creator, "Me");

    h.app.session.meta_clipboard = None;
    h.app.run("photo.setMeta", json!({"title": "Other"})).unwrap();
    let all = egui::Modifiers::COMMAND | egui::Modifiers::ALT | egui::Modifiers::SHIFT;
    h.app.run("library.select", json!({"ids": [a.0]})).unwrap();
    h.events.extend([egui::Event::ModifiersChanged(all), egui::Event::Copy]);
    h.settle(SETTLE);
    assert!(h.app.session.meta_clipboard.is_some(), "the Copy event with ⌥⇧ held copied the metadata");
    h.app.run("library.select", json!({"ids": [b.0]})).unwrap();
    h.events.extend([egui::Event::ModifiersChanged(all), egui::Event::Paste("text".into())]);
    h.settle(SETTLE);
    assert_eq!(title(&h, b), "Dawn", "the Paste event with ⌥⇧ held pasted the metadata");
    assert!(h.app.session.clipboard.is_none(), "no edit settings were copied");
    assert_eq!(h.app.ui.dialog, None, "no Paste Selected Settings");
}
/// The opening release can carry both physical shortcut keys and the replacement text.
#[test]
fn opening_numeric_release_keeps_keys_in_the_editor() {
    use egui::{Event, Key, Modifiers, PointerButton};

    for terminal in [Key::Enter, Key::Escape] {
        let mut h = demo();
        h.app.run("photo.rate", json!({"rating": 0})).unwrap();
        h.app.run("develop.set", json!({"values": {"light.exposure": 0.7}})).unwrap();
        h.app.run("app.setShortcut", json!({"id": "photo.increaseRating", "shortcut": "Enter"})).unwrap();
        let r = h.request("ui.set", json!({"view": "detail", "right": "edit", "openSections": ["light"], "histogram": false}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.settle(SETTLE));
        assert!(h.app.fonts_ready);
        assert!(h.app.recording_shortcut.is_none() && h.app.ui.name_edit.is_none());
        assert!(h.app.native_shortcuts.is_empty() && h.app.synthetic.is_empty() && h.events.is_empty());
        assert!(!h.view.ctx.egui_wants_keyboard_input());
        assert_eq!(crate::shortcuts::shortcut_of(&h.app.ui.settings.keymap, "photo.increaseRating"), Some("Enter"));
        let active = h.app.session.active().unwrap();
        let selection = h.app.session.selection.ids.clone();
        let (view, right, tool) = (h.app.ui.view, h.app.ui.right, h.app.ui.tool.clone());
        assert!(h.app.ui.dialog.is_none());
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 0);
        assert!((h.app.session.develop_of(active).unwrap().light.exposure - 0.7).abs() < 1e-9);
        let (undo, redo) = (h.app.session.undo.len(), h.app.session.redo.len());
        let pos = h.app.widgets.iter().rev().find(|(id, _)| id == "sliderValue:light.exposure").unwrap().1.center();
        h.events.push(Event::PointerMoved(pos));
        h.step();
        h.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        assert!(h.view.ctx.egui_wants_keyboard_input(), "the press focuses the editor before app shortcuts run");
        h.events.extend([
            Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE },
            Event::Key { key: Key::Num2, physical_key: Some(Key::Num2), pressed: true, repeat: false, modifiers: Modifiers::NONE },
            Event::Text("2".into()),
            Event::Key { key: terminal, physical_key: Some(terminal), pressed: true, repeat: false, modifiers: Modifiers::NONE },
        ]);
        h.step();
        // Observe the consuming frame directly, then exactly one empty frame: no request drains.
        for next_frame in [false, true] {
            if next_frame {
                assert!(h.events.is_empty() && h.app.synthetic.is_empty());
                h.step();
            }
            let expected = if terminal == Key::Enter { 2.0 } else { 0.7 };
            assert!((h.app.session.develop_of(active).unwrap().light.exposure - expected).abs() < 1e-9, "{terminal:?}, next={next_frame}");
            assert_eq!(h.app.session.undo.len(), undo + usize::from(terminal == Key::Enter), "{terminal:?}, next={next_frame}");
            assert_eq!(h.app.session.redo.len(), redo);
            assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 0, "neither Num2 nor the saved Enter shortcut leaked");
            assert_eq!(h.app.session.active(), Some(active));
            assert_eq!(h.app.session.selection.ids, selection);
            assert_eq!((h.app.ui.view, h.app.ui.right, &h.app.ui.tool), (view, right, &tool));
            assert!(h.app.ui.dialog.is_none());
            assert!(!h.view.ctx.egui_wants_keyboard_input());
            if next_frame {
                assert!(h.app.widgets.iter().all(|(id, _)| id != "sliderField:light.exposure"), "the editor is gone");
                assert!(h.app.widgets.iter().any(|(id, _)| id == "sliderValue:light.exposure"));
            }
        }
    }
}

/// Press `key` with the named modifiers held (`cmd`, `shift`, `alt`, `ctrl`).
fn press(h: &mut Headless, key: &str, mods: &[&str]) {
    let mut p = json!({ "key": key });
    for m in mods {
        p[*m] = json!(true);
    }
    let r = h.request("ui.key", p, T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

fn view(h: &mut Headless, p: serde_json::Value) {
    let r = h.request("ui.set", p, T);
    assert_eq!(r["ok"], true, "{r}");
}

/// Make `id` the only selected, active photo.
fn select(h: &mut Headless, id: lcc_catalog::PhotoId) {
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
}

/// The demo library in Photo Grid with its middle photo selected: the photos in view, and that one.
fn grid_demo() -> (Headless, Vec<lcc_catalog::PhotoId>, lcc_catalog::PhotoId) {
    let mut h = demo();
    view(&mut h, json!({ "view": "photoGrid" }));
    let vis = h.app.session.visible_cloned();
    assert!(vis.len() > 2, "{vis:?}");
    let middle = vis[vis.len() / 2];
    select(&mut h, middle);
    (h, vis, middle)
}

/// Count how often the folder picker is asked for; every ask is cancelled.
fn count_folder_picks(h: &mut Headless) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    h.app.services.pick_folder = Some(Box::new(move || {
        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        None
    }));
    calls
}

/// Opening a library uses the same folder picker as File > Open Library. Cancelling it
/// leaves the current photo and library intact.
#[test]
fn cmd_o_opens_the_library_picker_once() {
    let mut h = demo();
    let calls = count_folder_picks(&mut h);
    let active = h.app.session.active();
    let photos = h.app.session.catalog.len();
    press(&mut h, "O", &["cmd"]);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "one picker for Cmd+O");
    assert_eq!(h.app.session.active(), active);
    assert_eq!(h.app.session.catalog.len(), photos);
}

/// Home / End make the first / last photo of the view the active one, only in the library grids,
/// and yield to a key the user gave to another command.
#[test]
fn home_and_end_select_the_ends_of_the_grid() {
    let (mut h, vis, _) = grid_demo();
    let (first, last) = (vis[0], *vis.last().unwrap());

    press(&mut h, "Home", &[]);
    assert_eq!(h.app.session.active(), Some(first), "Home goes to the first photo");
    assert_eq!(h.app.session.selection.ids, vec![first], "and selects only it");
    press(&mut h, "End", &[]);
    assert_eq!(h.app.session.active(), Some(last), "End goes to the last photo");
    assert_eq!(h.app.session.selection.ids, vec![last]);

    view(&mut h, json!({ "view": "squareGrid" }));
    press(&mut h, "Home", &[]);
    assert_eq!(h.app.session.active(), Some(first), "the Square Grid has the keys too");

    // in the Classic set Home / End are Library module keys (`library.first` / `library.last`), so
    // the Library loupe has them too, as in Lightroom Classic
    select(&mut h, last);
    view(&mut h, json!({ "view": "detail" }));
    press(&mut h, "Home", &[]);
    assert_eq!(h.app.session.active(), Some(first), "Home goes to the first photo in the loupe");
    select(&mut h, last);

    view(&mut h, json!({ "view": "photoGrid" }));
    let r = h.app.run("app.setShortcut", json!({"id": "view.survey", "shortcut": "Home"})).unwrap();
    assert_eq!(r["shortcut"], "Home", "{r}");
    press(&mut h, "Home", &[]);
    assert_eq!(h.app.ui.view, ViewMode::Survey, "the user's Home binding fires");
    assert_eq!(h.app.session.active(), Some(last), "the grid key stayed out of the way");
}

/// Shift+Left / Shift+Right in a grid add the previous / next photo of the view to the selection and
/// make it active, stop at either end, stay out of the loupe and yield to the user's keymap.
#[test]
fn shift_arrows_extend_the_grid_selection() {
    let (mut h, vis, middle) = grid_demo();
    let at = vis.iter().position(|p| *p == middle).unwrap();
    let (previous, next) = (vis[at - 1], vis[at + 1]);

    press(&mut h, "Right", &["shift"]);
    assert_eq!(h.app.session.selection.ids, vec![middle, next], "Shift+Right adds the next photo");
    assert_eq!(h.app.session.active(), Some(next), "the added photo becomes active");
    // Shift+Left steps back onto a photo that is already selected: the selection stays, the active moves
    press(&mut h, "Left", &["shift"]);
    assert_eq!(h.app.session.selection.ids, vec![middle, next]);
    assert_eq!(h.app.session.active(), Some(middle));
    press(&mut h, "Left", &["shift"]);
    assert_eq!(h.app.session.selection.ids, vec![middle, next, previous], "Shift+Left adds the previous photo");
    assert_eq!(h.app.session.active(), Some(previous));

    // the ends of the view do nothing
    select(&mut h, vis[0]);
    press(&mut h, "Left", &["shift"]);
    assert_eq!(h.app.session.selection.ids, vec![vis[0]], "Shift+Left on the first photo is a no-op");
    let last = *vis.last().unwrap();
    select(&mut h, last);
    press(&mut h, "Right", &["shift"]);
    assert_eq!(h.app.session.selection.ids, vec![last], "Shift+Right on the last photo is a no-op");

    // outside a grid it is not the extension key
    select(&mut h, middle);
    view(&mut h, json!({ "view": "detail" }));
    press(&mut h, "Right", &["shift"]);
    assert_eq!(h.app.session.selection.ids, vec![middle], "the loupe has no grid extension key");

    // a key the user gave to a command wins
    view(&mut h, json!({ "view": "photoGrid" }));
    h.app.run("app.setShortcut", json!({"id": "view.survey", "shortcut": "Shift+Right"})).unwrap();
    press(&mut h, "Right", &["shift"]);
    assert_eq!(h.app.ui.view, ViewMode::Survey, "the user's Shift+Right binding fires");
    assert_eq!(h.app.session.selection.ids, vec![middle], "the grid key stayed out of the way");
}

/// A text field with the keyboard focus keeps every one of these keys.
#[test]
fn navigation_open_library_and_hdr_keys_wait_for_a_text_field() {
    let (mut h, vis, middle) = grid_demo();
    let calls = count_folder_picks(&mut h);
    let picks = || calls.load(std::sync::atomic::Ordering::SeqCst);
    let keys: [(&str, &[&str]); 6] = [("Home", &[]), ("End", &[]), ("Right", &["shift"]), ("Left", &["shift"]), ("O", &["cmd"]), ("H", &["shift"])];

    // with no text field focused, each of them acts
    press(&mut h, "Home", &[]);
    assert_eq!(h.app.session.active(), Some(vis[0]));
    press(&mut h, "End", &[]);
    assert_eq!(h.app.session.active(), vis.last().copied());
    select(&mut h, middle);
    press(&mut h, "Right", &["shift"]);
    assert_eq!(h.app.session.selection.ids.len(), 2, "Shift+Right extends");
    press(&mut h, "O", &["cmd"]);
    assert_eq!(picks(), 1, "Cmd+O asks for a folder");
    select(&mut h, middle);

    // with the search box focused, none does
    let r = h.request("ui.clickWidget", json!({"id": "field:search"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.view.ctx.egui_wants_keyboard_input(), "the search box has the keyboard");
    let before = (h.app.session.active(), h.app.session.selection.ids.clone(), h.app.ui.hdr_visualize, h.app.ui.view, picks());
    for (key, mods) in keys {
        press(&mut h, key, mods);
        let after = (h.app.session.active(), h.app.session.selection.ids.clone(), h.app.ui.hdr_visualize, h.app.ui.view, picks());
        assert_eq!(after, before, "{mods:?}+{key} reached the shortcuts while the search box had the focus");
    }
    assert!(h.app.ui.dialog.is_none());
}

/// Shift+Up / Shift+Down in a grid add every photo from the active one to the cell above / below it
/// (the cell plain Up / Down move to) and make that cell active; the top and bottom rows do nothing,
/// and the loupe has no such key.
#[test]
fn shift_up_down_extend_the_grid_selection_by_a_row() {
    let (mut h, vis, middle) = grid_demo();
    let at = vis.iter().position(|p| *p == middle).unwrap();

    press(&mut h, "Down", &["shift"]);
    let reached = h.app.session.active().unwrap();
    let to = vis.iter().position(|p| *p == reached).unwrap();
    assert!(to > at, "the active photo moved down the grid: {at} -> {to}");
    let mut got = h.app.session.selection.ids.clone();
    got.sort_by_key(|p| p.0);
    let mut want = vis[at..=to].to_vec();
    want.sort_by_key(|p| p.0);
    assert_eq!(got, want, "everything from the active photo to the cell below is selected");

    // Shift+Up from there reaches the row the selection began on again: nothing is deselected
    let before = h.app.session.selection.ids.len();
    press(&mut h, "Up", &["shift"]);
    assert!(h.app.session.selection.ids.len() >= before, "Shift+Up never removes photos");
    for p in &vis[at..=to] {
        assert!(h.app.session.selection.contains(*p), "{p:?} stays selected");
    }

    // the first row has no row above, the last none below: the selection stays as it is
    select(&mut h, vis[0]);
    press(&mut h, "Up", &["shift"]);
    assert_eq!(h.app.session.selection.ids, vec![vis[0]], "Shift+Up in the first row is a no-op");
    let last = *vis.last().unwrap();
    select(&mut h, last);
    press(&mut h, "Down", &["shift"]);
    assert_eq!(h.app.session.selection.ids, vec![last], "Shift+Down in the last row is a no-op");

    // outside a grid it is not the extension key
    select(&mut h, middle);
    view(&mut h, json!({ "view": "detail" }));
    press(&mut h, "Down", &["shift"]);
    assert_eq!(h.app.session.selection.ids, vec![middle], "the loupe has no grid extension key");
}

/// Ctrl+Shift+arrows extend the grid selection like Shift+arrows and keep a scattered selection:
/// the photos selected earlier stay, wherever they are.
#[test]
fn ctrl_shift_arrows_extend_the_grid_selection_and_keep_the_rest() {
    let (mut h, vis, middle) = grid_demo();
    let at = vis.iter().position(|p| *p == middle).unwrap();
    let (previous, next) = (vis[at - 1], vis[at + 1]);

    press(&mut h, "Right", &["cmd", "shift"]);
    assert_eq!(h.app.session.selection.ids, vec![middle, next], "Ctrl+Shift+Right adds the next photo");
    assert_eq!(h.app.session.active(), Some(next));
    press(&mut h, "Left", &["cmd", "shift"]);
    press(&mut h, "Left", &["cmd", "shift"]);
    assert_eq!(h.app.session.selection.ids, vec![middle, next, previous], "Ctrl+Shift+Left adds the previous photo");

    // a photo far away, selected earlier, stays selected
    let far = vis[0];
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [far.0], "mode": "add"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    press(&mut h, "Right", &["cmd", "shift"]);
    assert!(h.app.session.selection.contains(far), "the photo selected earlier stays");
    assert!(h.app.session.selection.contains(middle) && h.app.session.selection.contains(next));

    // downwards it adds the row, as Shift+Down does
    select(&mut h, middle);
    press(&mut h, "Down", &["cmd", "shift"]);
    let reached = h.app.session.active().unwrap();
    assert_ne!(reached, middle, "Ctrl+Shift+Down moved the active photo");
    assert!(h.app.session.selection.contains(middle), "the start of the selection stays");
    assert!(h.app.session.selection.ids.len() >= 2);
}

/// Shift+Left / Shift+Right belong to the grid: while the full-screen preview covers it they leave
/// the selection alone. (Home / End are Library module keys in the Classic set and step to the
/// first / last photo there too.)
#[test]
fn grid_navigation_keys_do_nothing_in_the_full_screen_preview() {
    let (mut h, vis, middle) = grid_demo();
    h.app.run("view.fullScreenPreview", json!({})).unwrap();
    assert!(h.app.ui.fullscreen);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid, "the grid is the view behind the preview");
    for (key, mods) in [("Right", &["shift"][..]), ("Left", &["shift"][..])] {
        press(&mut h, key, mods);
        assert_eq!(h.app.session.active(), Some(middle), "{mods:?}+{key} moved the active photo in the full-screen preview");
        assert_eq!(h.app.session.selection.ids, vec![middle], "{mods:?}+{key} changed the selection in the full-screen preview");
    }
    // back in the grid the keys work again
    h.app.run("view.fullScreenPreview", json!({})).unwrap();
    assert!(!h.app.ui.fullscreen);
    press(&mut h, "Home", &[]);
    assert_eq!(h.app.session.active(), Some(vis[0]));
}

/// A dialog owns the keyboard: Cmd+O does not ask for a folder behind it.
#[test]
fn cmd_o_does_not_fire_while_a_dialog_is_open() {
    let mut h = demo();
    let calls = count_folder_picks(&mut h);
    h.app.run("app.shortcuts", json!({})).unwrap();
    h.step();
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Shortcuts)));
    press(&mut h, "O", &["cmd"]);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "Cmd+O reached the shortcuts behind a dialog");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Shortcuts)));
    // with the dialog closed the same key asks for a folder
    h.app.ui.dialog = None;
    press(&mut h, "O", &["cmd"]);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// The dialog's header controls don't overlap: Reset All used to be squeezed onto the filter row
/// and drawn over "Only Library keys" (issue #154).
#[test]
fn shortcuts_header_controls_do_not_overlap() {
    let mut h = demo();
    for module in ["library", "develop"] {
        h.app.run("app.shortcuts", json!({})).unwrap();
        let _ = h.app.run(&format!("module.{module}"), json!({}));
        h.step();
        h.step();
        let rect = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"));
        let reset = rect(&h, "button:shortcutsResetAll");
        for id in ["field:shortcutsSearch", "check:shortcuts.showAll", "check:shortcuts.moduleOnly"] {
            let other = rect(&h, id);
            assert!(!reset.intersects(other), "{module}: Reset All {reset:?} overlaps {id} {other:?}");
        }
    }
}

/// X in the crop tool swaps the crop's orientation once: it fired both Reject's crop alias and
/// Rotate Crop Aspect, so the frame turned twice and looked unchanged (issue #293).
#[test]
fn x_in_the_crop_tool_swaps_the_crop_orientation() {
    let mut h = demo();
    key(&mut h, "D", false);
    key(&mut h, "R", false);
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Crop);
    let id = h.app.session.active().unwrap();
    let r = h.request("engine.execute", json!({"command": "crop.aspect", "params": {"aspect": "3x2"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let (w, ht) = h.app.session.develop_of(id).unwrap().crop.aspect.unwrap();
    assert_ne!(w, ht);
    let flag = h.app.session.catalog.photo(id).map(|p| p.flag);
    key(&mut h, "X", false);
    assert_eq!(h.app.session.develop_of(id).unwrap().crop.aspect, Some((ht, w)), "X swaps the locked aspect once");
    assert_eq!(h.app.session.catalog.photo(id).map(|p| p.flag), flag, "X does not reject while cropping");
}

/// `app.keymapExport` never replaces a catalogued original or its sidecar (the path can come from
/// MCP or the control channel); a free path still exports.
#[test]
fn keymap_export_refuses_to_overwrite_an_original_or_its_sidecar() {
    let dir = lcc_testkit::TempDir::new("keymap-guard");
    let mut h = demo();
    std::fs::create_dir_all(&dir).unwrap();
    let orig = dir.join("IMG_1.jpg");
    std::fs::write(&orig, b"original").unwrap();
    std::fs::write(dir.join("IMG_1.xmp"), b"<x/>").unwrap();
    let id = h.app.session.catalog.alloc_photo_id();
    let p = lcc_catalog::Photo::new(id, lcc_catalog::Source::File { path: orig.to_string_lossy().into() }, "IMG_1.jpg", "JPEG", 60, 40, "2026-01-01");
    h.app.session.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    for target in [orig.clone(), dir.join("IMG_1.xmp")] {
        let before = std::fs::read(&target).unwrap();
        assert!(h.app.run("app.keymapExport", json!({"path": target.to_str().unwrap()})).is_err(), "{}", target.display());
        assert_eq!(std::fs::read(&target).unwrap(), before);
    }
    assert!(h.app.run("app.keymapExport", json!({"path": dir.join("keys.json").to_str().unwrap()})).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A is the aspect lock while the Crop panel is open — Visualize Spots, the other A, must not take
/// the panel or turn its view on — and outside the panel A opens Visualize Spots as usual. The lock
/// is the tool's own (a fresh photo starts locked to its shape and the toggle may only record
/// "free", leaving the raw develop aspect untouched), so the test reads `crop_tool_aspect`.
#[test]
fn a_is_the_aspect_lock_under_crop_and_visualize_spots_elsewhere() {
    use crate::state::RightPanel;
    let mut h = demo();
    let aspect = |h: &Headless| h.app.crop_tool_aspect();
    let spots_before = h.app.ui.visualize_spots;
    h.app.run("panel.crop", json!({})).unwrap();
    h.step();
    h.step();
    assert_eq!(h.app.ui.right, RightPanel::Crop);
    let before = aspect(&h);
    key(&mut h, "A", false);
    assert_eq!(h.app.ui.right, RightPanel::Crop, "Visualize Spots must not take the panel");
    assert_eq!(h.app.ui.visualize_spots, spots_before, "nor turn its view on");
    assert_ne!(aspect(&h), before, "A toggles the aspect lock");
    key(&mut h, "A", false);
    assert_eq!(aspect(&h), before, "A toggles it back");
    // outside the panel the same key is Visualize Spots again
    h.app.ui.right = RightPanel::None;
    key(&mut h, "A", false);
    assert_eq!(h.app.ui.right, RightPanel::Remove, "A opens Remove with the visualization");
    assert!(h.app.ui.visualize_spots, "Visualize Spots is on");
    assert_eq!(aspect(&h), before, "and the aspect is untouched");
}

/// An installed filter plug-in with a `shortcut` in its manifest is a bindable *Add <name>*: its
/// key puts the filter on the selected photos and opens the Plug-in Filters section; the user
/// can rebind it like any shortcut.
#[test]
fn a_filter_plugins_declared_key_adds_it_to_the_selection() {
    let manifest = r#"{"id":"test.keymap.neg","name":"Neg","kind":"filter","shortcut":"Ctrl+Alt+N"}"#;
    let src = format!(
        r#"(module
  (memory (export "memory") 1)
  (data (i32.const 16) "{m}")
  (func (export "pc_abi_version") (result i32) (i32.const 1))
  (func (export "pc_manifest") (result i64) (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.const 16)))
  (func (export "pc_alloc") (param $n i32) (result i32) (i32.const 65536))
  (func (export "pc_filter") (param i32 i32 i32 i32 i32 i32 i32 i32) (result i32) (i32.const 0)))"#,
        m = manifest.replace('"', "\\\""),
        len = manifest.len()
    );
    lcc_engine::filter_plugins::install_in_memory(&wat::parse_str(&src).unwrap()).unwrap();
    let id = format!("{}test.keymap.neg", crate::shortcuts::PLUGIN_ADD);
    let b = crate::shortcuts::find_bindable(&id).expect("an installed filter is bindable");
    // New Smart Collection has Cmd+Alt+N: the same press as Ctrl+Alt+N except on macOS, where the
    // declared key is free; elsewhere the filter starts without one and the user gives it the key
    let mac = cfg!(target_os = "macos");
    assert_eq!((b.label, b.default()), ("Add Neg", if mac { Some("Ctrl+Alt+N") } else { None }));

    let mut h = demo();
    if !mac {
        // given the key, the filter takes it from New Smart Collection
        let r = h.app.run("app.setShortcut", json!({"id": id, "shortcut": "Ctrl+Alt+N"})).unwrap();
        assert!(r["removedFrom"].as_array().is_some_and(|l| l.iter().any(|c| c == "dialog.newSmartAlbum")), "{r}");
    }
    let ids: Vec<u64> = h.app.session.catalog.photos().take(2).map(|p| p.id.0).collect();
    h.app.run("library.select", json!({"ids": ids})).unwrap();
    h.step();
    let r = h.request("ui.key", json!({"key": "N", "ctrl": true, "alt": true}), Duration::from_secs(20));
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..3 {
        h.step();
    }
    for id in &ids {
        let d = h.app.session.develop_of(lcc_catalog::PhotoId(*id)).unwrap();
        assert_eq!(d.plugin_filters.len(), 1, "photo {id}");
        assert_eq!(d.plugin_filters[0].plugin, "test.keymap.neg");
    }
    assert!(h.app.ui.section_open(lcc_develop::PLUGIN_FILTERS_SECTION));
    // rebound: the new key adds, the old one no longer does
    h.app.run("app.setShortcut", json!({"id": id, "shortcut": "Cmd+Alt+Shift+N"})).unwrap();
    let r = h.request("ui.key", json!({"key": "N", "ctrl": true, "alt": true}), Duration::from_secs(20));
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.session.develop_of(lcc_catalog::PhotoId(ids[0])).unwrap().plugin_filters.len(), 1);
    lcc_engine::filter_plugins::uninstall("test.keymap.neg").unwrap();
}
