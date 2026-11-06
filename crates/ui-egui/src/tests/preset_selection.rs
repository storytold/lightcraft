//! Issues #1630 and #3553: the Presets panel selects several presets (⌘/Ctrl+click, Shift+click,
//! Select All) to copy, move or delete them at once.

use std::time::Duration;

use lightcraft_develop::Preset;
use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// A headless app on the demo library with the Presets panel open, holding three user presets in
/// group "AAA Mine" (first in the panel). Returns their ids.
fn app() -> (Headless, Vec<String>) {
    let mut session = lightcraft_engine::Session::with_demo();
    let presets = ["one", "two", "three"].map(|name| Preset {
        id: String::new(),
        name: name.into(),
        group: "AAA Mine".into(),
        settings: json!({"light": {"exposure": 0.5}}),
        favorite: false,
        builtin: false,
    });
    let ids = session.add_presets(presets.to_vec());
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.app.run("panel.presets", json!({})).unwrap();
    frames(&mut h);
    (h, ids)
}

fn frames(h: &mut Headless) {
    for _ in 0..5 {
        h.step();
    }
}

fn req(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    frames(h);
}

fn click(h: &mut Headless, id: &str, modifiers: serde_json::Value) {
    let mut params = json!({"id": id});
    if let (Some(p), Some(m)) = (params.as_object_mut(), modifiers.as_object()) {
        p.extend(m.clone());
    }
    req(h, "ui.clickWidget", params);
}

fn selected(h: &Headless) -> Vec<String> {
    h.app.ui.preset_selection.iter().cloned().collect()
}

fn sorted(ids: &[&String]) -> Vec<String> {
    let mut v: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    v.sort();
    v
}

fn laid_out(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Undo steps: applying a preset adds one, selecting must not.
fn undo_len(h: &Headless) -> usize {
    h.app.session.undo.len()
}

#[test]
fn command_click_selects_without_applying_and_a_plain_click_ends_the_selection() {
    let (mut h, ids) = app();
    let undo = undo_len(&h);
    click(&mut h, &format!("preset:{}", ids[0]), json!({"cmd": true}));
    click(&mut h, &format!("preset:{}", ids[2]), json!({"cmd": true}));
    assert_eq!(selected(&h), sorted(&[&ids[0], &ids[2]]));
    assert_eq!(undo_len(&h), undo, "selecting applies nothing");
    assert!(laid_out(&h, &format!("presetSelected:{}", ids[0])), "a selected row is marked");
    assert!(laid_out(&h, "presetSelectionActions"), "the bar shows");

    click(&mut h, &format!("preset:{}", ids[0]), json!({"cmd": true}));
    assert_eq!(selected(&h), sorted(&[&ids[2]]), "a second ⌘/Ctrl+click takes it out");

    click(&mut h, &format!("preset:{}", ids[1]), json!({}));
    assert!(selected(&h).is_empty());
    assert_eq!(undo_len(&h), undo, "a plain click ending a selection applies nothing");
    assert!(!laid_out(&h, "presetSelectionActions"), "the bar is gone");

    click(&mut h, &format!("preset:{}", ids[1]), json!({}));
    assert_eq!(undo_len(&h), undo + 1, "with no selection a plain click applies, as before");
}

#[test]
fn shift_click_extends_from_the_last_row_toggled_in_either_direction() {
    let (mut h, ids) = app();
    click(&mut h, &format!("preset:{}", ids[2]), json!({"cmd": true}));
    click(&mut h, &format!("preset:{}", ids[0]), json!({"shift": true}));
    assert_eq!(selected(&h), sorted(&[&ids[0], &ids[1], &ids[2]]));
}

/// A plain click applies its preset and marks where a Shift+click extends from.
#[test]
fn shift_click_after_a_plain_click_selects_the_range_and_applies_only_the_first() {
    let (mut h, ids) = app();
    let undo = undo_len(&h);
    click(&mut h, &format!("preset:{}", ids[0]), json!({}));
    assert_eq!(undo_len(&h), undo + 1, "the plain click applies");
    assert!(selected(&h).is_empty(), "and selects nothing yet");
    click(&mut h, &format!("preset:{}", ids[2]), json!({"shift": true}));
    assert_eq!(selected(&h), sorted(&[&ids[0], &ids[1], &ids[2]]));
    assert_eq!(undo_len(&h), undo + 1, "Shift+click applies nothing");
}

/// Double-Click to Apply: a click selects that preset alone and applies nothing, a double-click
/// applies it, and the setting is reached from the ⋯ menu.
#[test]
fn with_double_click_to_apply_a_click_selects_and_a_double_click_applies() {
    let (mut h, ids) = app();
    assert!(!h.app.ui.preset_double_click, "off by default");
    click(&mut h, "icon:presetMore", json!({}));
    click(&mut h, "presetMenu:doubleClick", json!({}));
    assert!(h.app.ui.preset_double_click);
    req(&mut h, "ui.key", json!({"key": "Escape"}));
    h.app.ui.dialog = None;
    let undo = undo_len(&h);

    click(&mut h, &format!("preset:{}", ids[0]), json!({}));
    assert_eq!(selected(&h), sorted(&[&ids[0]]));
    assert_eq!(undo_len(&h), undo, "a click applies nothing");
    assert!(!laid_out(&h, "presetSelectionActions"), "one preset brings up no bar");

    click(&mut h, &format!("preset:{}", ids[1]), json!({}));
    assert_eq!(selected(&h), sorted(&[&ids[1]]), "another click moves the selection");
    assert_eq!(undo_len(&h), undo);

    // past egui's double-click delay (0.3 s of simulated time), or these two clicks and the one
    // above would count as a triple click
    for _ in 0..30 {
        h.step();
    }
    click(&mut h, &format!("preset:{}", ids[1]), json!({"count": 2}));
    assert_eq!(undo_len(&h), undo + 1, "a double-click applies");
    assert_eq!(selected(&h), sorted(&[&ids[1]]));

    click(&mut h, &format!("preset:{}", ids[2]), json!({"shift": true}));
    assert_eq!(selected(&h), sorted(&[&ids[1], &ids[2]]), "Shift+click extends as in the other mode");
    assert_eq!(undo_len(&h), undo + 1);
}

/// The bar coming and going leaves the rows where they are: a double-click's second click lands on
/// the row of its first.
#[test]
fn the_selection_bar_leaves_the_rows_in_place() {
    let (mut h, ids) = app();
    let row = format!("preset:{}", ids[0]);
    let top = |h: &Headless| h.app.widgets.iter().find(|(w, _)| *w == row).map(|(_, r)| r.top()).unwrap();
    let before = top(&h);
    click(&mut h, &row, json!({"cmd": true}));
    click(&mut h, &format!("preset:{}", ids[1]), json!({"cmd": true}));
    assert!(laid_out(&h, "presetSelectionActions"));
    assert_eq!(top(&h), before);
    let list = h.app.widgets.iter().find(|(w, _)| w == "presetSelectionActions").map(|(_, r)| r.top()).unwrap();
    assert!(list > before, "the bar is under the list");
}

/// A right-click opens the menu and never applies the preset under it.
#[test]
fn a_right_click_applies_nothing() {
    let (mut h, ids) = app();
    let undo = undo_len(&h);
    click(&mut h, &format!("preset:{}", ids[0]), json!({"button": "right"}));
    assert_eq!(undo_len(&h), undo);
}

#[test]
fn the_panel_menu_selects_every_preset_and_the_bar_clears_them() {
    let (mut h, _) = app();
    click(&mut h, "icon:presetMore", json!({}));
    click(&mut h, "presetMenu:selectAll", json!({}));
    assert_eq!(selected(&h).len(), h.app.session.presets.len());
    click(&mut h, "icon:presetSelectionClear", json!({}));
    assert!(selected(&h).is_empty());
}

#[test]
fn a_group_header_selects_its_presets() {
    let (mut h, ids) = app();
    click(&mut h, "presetGroup:AAA Mine", json!({"button": "right"}));
    click(&mut h, "presetGroupMenu:selectAll", json!({}));
    assert_eq!(selected(&h), sorted(&[&ids[0], &ids[1], &ids[2]]));
}

/// The bar's Delete asks first, then removes the user presets and keeps the built-in ones.
#[test]
fn delete_from_the_bar_asks_and_keeps_built_in_presets() {
    let (mut h, ids) = app();
    let builtin = h.app.session.presets.iter().filter(|p| p.builtin).count();
    click(&mut h, "icon:presetMore", json!({}));
    click(&mut h, "presetMenu:selectAll", json!({}));
    click(&mut h, "presetSelectionActions", json!({}));
    click(&mut h, "presetBulk:delete", json!({}));
    let Some(Dialog::DeletePresets { ids: asked, builtin: kept, .. }) = h.app.ui.dialog.as_ref() else {
        panic!("the dialog asks first: {:?}", h.app.ui.dialog.is_some());
    };
    assert_eq!((asked.len() - kept, *kept), (3, builtin));
    assert_eq!(h.app.session.presets.iter().filter(|p| !p.builtin).count(), 3, "nothing is gone yet");
    let dialog = h.app.ui.dialog.take().unwrap();
    crate::dialogs::confirm_dialog(&mut h.app, &dialog).unwrap();
    frames(&mut h);
    assert!(!h.app.session.presets.iter().any(|p| ids.contains(&p.id)));
    assert_eq!(h.app.session.presets.len(), builtin);
    assert!(selected(&h).is_empty());
}

/// Presets gone from the list leave the selection: the count stays true, and importing them again
/// doesn't select them.
#[test]
fn presets_gone_leave_the_selection() {
    let (mut h, ids) = app();
    let other = h.app.session.presets.iter().find(|p| p.builtin).map(|p| p.id.clone()).unwrap();
    h.app.ui.preset_selection.extend(ids.iter().cloned());
    h.app.ui.preset_selection.insert(other.clone());
    frames(&mut h);
    h.app.run("preset.deleteGroup", json!({"name": "AAA Mine"})).unwrap();
    frames(&mut h);
    assert_eq!(selected(&h), vec![other], "only what still exists stays selected");
}

/// An answer to a question asked in another library deletes nothing.
#[test]
fn a_deletion_asked_in_another_library_deletes_nothing() {
    let (mut h, ids) = app();
    let library = h.app.session.library_generation() + 1;
    let stale = Dialog::DeletePresets { ids: ids.clone(), builtin: 0, library };
    assert!(crate::dialogs::confirm_dialog(&mut h.app, &stale).is_err());
    assert!(ids.iter().all(|id| h.app.session.presets.iter().any(|p| &p.id == id)));
}

/// At the very end of the list too, the bar going away leaves the rows where they are.
#[test]
fn the_bar_going_leaves_the_rows_in_place_at_the_end_of_the_list() {
    let (mut h, ids) = app();
    h.app.ui.preset_selection.extend(ids[..2].iter().cloned());
    frames(&mut h);
    let header = |h: &Headless| h.app.widgets.iter().find(|(w, _)| w == "presetGroup:AAA Mine").map(|(_, r)| r.top()).unwrap();
    let top = header(&h);
    req(&mut h, "ui.hoverWidget", json!({"id": "presetGroup:AAA Mine"}));
    req(&mut h, "ui.scroll", json!({"dy": -100_000.0}));
    for _ in 0..120 {
        h.step();
    }
    assert!(header(&h) < top - 100.0, "the list has scrolled to its end");
    let last = |h: &Headless| {
        let rows = h.app.widgets.iter().filter(|(w, _)| w.starts_with("preset:"));
        rows.max_by(|a, b| a.1.top().total_cmp(&b.1.top())).map(|(w, r)| (w.clone(), r.top())).unwrap()
    };
    let before = last(&h);
    h.app.ui.preset_selection.clear();
    for _ in 0..30 {
        h.step();
    }
    assert_eq!(last(&h), before);
}

/// The bar isn't part of the list: it stays in the window, in one place, wherever the list is scrolled.
#[test]
fn the_bar_stays_in_view_wherever_the_list_is_scrolled() {
    let (mut h, ids) = app();
    h.app.ui.preset_selection.extend(ids[..2].iter().cloned());
    frames(&mut h);
    let rect = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap();
    let at_top = rect(&h, "presetSelectionActions");
    assert!(at_top.bottom() <= 1000.0, "{at_top:?}");
    let header = rect(&h, "presetGroup:AAA Mine").top();
    assert!(header < at_top.top(), "the list starts above the bar");
    req(&mut h, "ui.hoverWidget", json!({"id": "presetGroup:AAA Mine"}));
    req(&mut h, "ui.scroll", json!({"dy": -100_000.0}));
    for _ in 0..120 {
        h.step();
    }
    assert!(rect(&h, "presetGroup:AAA Mine").top() < header - 100.0, "the list has scrolled to its end");
    assert_eq!(rect(&h, "presetSelectionActions"), at_top);
}

/// The question reads right for one preset to delete and one built-in preset staying.
#[test]
fn deleting_one_preset_with_one_built_in_says_so() {
    let (mut h, ids) = app();
    let builtin = h.app.session.presets.iter().find(|p| p.builtin).map(|p| p.id.clone()).unwrap();
    let library = h.app.session.library_generation();
    h.app.ui.dialog = Some(Dialog::DeletePresets { ids: vec![ids[0].clone(), builtin], builtin: 1, library });
    frames(&mut h);
    let text = h.painted_text();
    assert!(text.iter().any(|t| t.contains("Delete this preset?")), "{text:?}");
    assert!(text.iter().any(|t| t.contains("1 built-in preset stays")), "{text:?}");
}

/// A right-click inside a selection acts on all of it: here, copying into a group made empty.
#[test]
fn a_right_click_inside_the_selection_copies_all_of_it_into_an_empty_group() {
    let (mut h, ids) = app();
    h.app.run("preset.createGroup", json!({"name": "AAA Empty"})).unwrap();
    frames(&mut h);
    assert!(laid_out(&h, "presetGroup:AAA Empty"), "an empty group is listed");
    for id in &ids[..2] {
        click(&mut h, &format!("preset:{id}"), json!({"cmd": true}));
    }
    click(&mut h, &format!("preset:{}", ids[0]), json!({"button": "right"}));
    assert!(laid_out(&h, "presetBulk:delete"), "the menu acts on the selection");
    click(&mut h, "presetBulk:copy", json!({}));
    click(&mut h, "presetTarget:preset.copy:AAA Empty", json!({}));
    let copies = h.app.session.presets.iter().filter(|p| p.group == "AAA Empty").count();
    assert_eq!(copies, 2);
    assert_eq!(h.app.session.presets.iter().filter(|p| p.group == "AAA Mine").count(), 3, "the originals stay");
}
