//! Headless tests of the editable keymap (Help ▸ Keyboard Shortcuts): a changed shortcut fires
//! its command and not the old one's, the editor records a key press without running it, Esc
//! cancels recording without closing the dialog, and the keymap survives a save/load of `ui.json`.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{Dialog, ViewMode};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
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

/// On Windows and Linux the windowing layer turns Ctrl+C / Ctrl+V into clipboard events, not key
/// presses: outside a text field they still copy the edit settings, and with ⇧ open Paste Selected
/// Settings.
#[test]
fn clipboard_events_run_the_copy_and_paste_shortcuts() {
    let mut h = demo();
    assert!(h.app.session.clipboard.is_none());
    h.events.push(egui::Event::Copy);
    h.settle(SETTLE);
    assert!(h.app.session.clipboard.is_some(), "Ctrl+C copied the edit settings");
    let shift_cmd = egui::Modifiers::SHIFT | egui::Modifiers::COMMAND;
    h.events.extend([egui::Event::ModifiersChanged(shift_cmd), egui::Event::Paste("text".into())]);
    h.settle(SETTLE);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::PasteSettings { .. })), "⇧Ctrl+V: {:?}", h.app.ui.dialog);
}

/// Windows also pastes with ⇧Insert and cuts with ⇧Delete: the clipboard events they become are
/// the plain Paste / Cut, not ⇧Ctrl+V (Paste Selected Settings).
#[test]
fn shift_insert_pastes_plainly() {
    let mut h = demo();
    h.events.push(egui::Event::Copy);
    h.settle(SETTLE);
    let undo = h.app.session.undo.len();
    h.events.extend([egui::Event::ModifiersChanged(egui::Modifiers::SHIFT), egui::Event::Paste("text".into())]);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.dialog, None, "no Paste Selected Settings");
    assert_eq!(h.app.session.undo.len(), undo + 1, "the settings were pasted");
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
fn select(h: &mut Headless, id: lightcraft_catalog::PhotoId) {
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
}

/// The demo library in Photo Grid with its middle photo selected: the photos in view, and that one.
fn grid_demo() -> (Headless, Vec<lightcraft_catalog::PhotoId>, lightcraft_catalog::PhotoId) {
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

/// The declared key of Open Library. Visualize HDR has none by default: ⇧H cycles the Tool Overlay
/// (issue #224); the user can give Visualize HDR a key in the keymap.
#[test]
fn open_library_and_visualize_hdr_keys_are_declared() {
    use crate::shortcuts::{Keymap, parse, shortcut_of};

    let keymap = Keymap::new();
    assert_eq!(shortcut_of(&keymap, "app.openLibrary"), Some("Cmd+O"));
    assert!(parse("Cmd+O").is_some());
    assert_eq!(shortcut_of(&keymap, "view.visualizeHdr"), None);
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

    select(&mut h, last);
    view(&mut h, json!({ "view": "detail" }));
    press(&mut h, "Home", &[]);
    assert_eq!(h.app.session.active(), Some(last), "Home is a grid key");

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

/// Home / End and Shift+Left / Shift+Right belong to the grid: while the full-screen preview covers it
/// they leave the selection alone.
#[test]
fn grid_navigation_keys_do_nothing_in_the_full_screen_preview() {
    let (mut h, vis, middle) = grid_demo();
    h.app.run("view.fullScreenPreview", json!({})).unwrap();
    assert!(h.app.ui.fullscreen);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid, "the grid is the view behind the preview");
    for (key, mods) in [("Home", &[][..]), ("End", &[][..]), ("Right", &["shift"][..]), ("Left", &["shift"][..])] {
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

/// The crop aspect lock is a first-class row of Help - Keyboard Shortcuts: A, listed in the
/// editor like any other command, sharing the key with Visualize Spots as a contextual pair.
#[test]
fn lock_aspect_ratio_is_a_listed_row_on_a() {
    use crate::shortcuts::{CONTEXTUAL, Keymap, bindable, shortcut_of};
    let b = bindable().iter().find(|b| b.id == "crop.lockAspect").expect("crop.lockAspect is mappable");
    assert_eq!(b.label, "Lock Aspect Ratio");
    assert_eq!(b.default, Some("A"));
    assert_eq!(shortcut_of(&Keymap::new(), "crop.lockAspect"), Some("A"));
    assert!(CONTEXTUAL.contains(&("view.visualizeSpots", "crop.lockAspect")), "A is disambiguated by context");
    // the editor lists it: the row is on screen and records keys for it
    let mut h = demo();
    record(&mut h, "lock", "crop.lockAspect");
    key(&mut h, "Escape", false);
    assert_eq!(shortcut_of(&h.app.ui.settings.keymap, "crop.lockAspect"), Some("A"), "Esc cancels the recording: A stays");
    key(&mut h, "Escape", false);
    assert!(h.app.ui.dialog.is_none());
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
