//! Caller-level probes run unchanged on native baseline and proposed SDK caller.
//! All input uses the real control channel; focus is never assigned by the test.
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
const FILTER: &str = "field:keywordFilter";

fn ask(h: &mut Headless, method: &str, params: Value) {
    let answer = h.request(method, params.clone(), T);
    assert_eq!(answer["ok"], true, "{method} {params}: {answer}");
    h.settle(SETTLE);
}

fn click(h: &mut Headless, widget: &str) {
    ask(h, "ui.clickWidget", json!({"id": widget}));
}

fn key(h: &mut Headless, name: &str, shift: bool) {
    ask(h, "ui.key", json!({"key": name, "shift": shift}));
}

fn text(h: &mut Headless, value: &str) {
    ask(h, "ui.text", json!({"text": value}));
}

fn focused(h: &Headless, widget: &str) -> bool {
    crate::text_field::focused(&h.view.ctx, widget)
        || craft::ui::runtime::focus::focused_rec(&h.view.ctx)
            .is_some_and(|rec| h.app.widget_hits.iter().any(|(name, hit)| name == widget && hit.id == rec.stop.id))
}

/// Resolve the actual clickable row/button Response, not its automation hover alias.
fn action_focused(h: &Headless, widget: &str) -> bool {
    let Some((_, rect)) = h.app.widgets.iter().find(|(name, _)| name == widget) else { return false };
    h.view
        .ctx
        .memory(|memory| memory.focused())
        .and_then(|id| h.view.ctx.read_response(id))
        .is_some_and(|response| response.rect == *rect && response.sense.senses_click())
}

fn keywords() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    ask(&mut h, "ui.set", json!({"view": "photoGrid", "right": "keywords"}));
    for name in ["Seam boundary A", "Seam boundary Z"] {
        ask(&mut h, "engine.execute", json!({"command": "keyword.create", "params": {"name": name}}));
    }
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": []}}));
    click(&mut h, FILTER);
    text(&mut h, "Seam boundary");
    h
}

fn filter(h: &Headless) -> String {
    h.view.ctx.data(|data| data.get_temp(egui::Id::unique("keyword-list-filter"))).unwrap_or_default()
}

#[test]
fn actual_first_row_follows_header_and_enter_selects_that_row() {
    let mut h = keywords();
    // Enable Delete without assigning keyboard focus or manufacturing a stop.
    click(&mut h, "keywordRow:Seam boundary Z");
    click(&mut h, FILTER);
    let undo = h.app.session.undo.len();
    key(&mut h, "Tab", false);
    assert!(action_focused(&h, "keywordList:create"), "filter -> actual Create");
    key(&mut h, "Tab", false);
    assert!(action_focused(&h, "keywordList:delete"), "Create -> actual enabled Delete");
    key(&mut h, "Tab", false);
    assert!(action_focused(&h, "keywordRow:Seam boundary A"), "Delete -> actual FIRST row, not another SDK control");
    key(&mut h, "Enter", false);
    assert_eq!(h.app.ui.keyword_list_selected.as_deref(), Some("Seam boundary A"), "Enter activates the focused first row");
    assert_eq!(h.app.session.undo.len(), undo);
}

#[test]
fn first_row_shift_tab_returns_to_actual_delete_then_create_then_filter() {
    let mut h = keywords();
    // Native row clicks select a keyword but do not themselves take keyboard focus.
    // Reach the actual first row by the same real Tab path the native baseline passed.
    click(&mut h, "keywordRow:Seam boundary A");
    click(&mut h, FILTER);
    for _ in 0..3 {
        key(&mut h, "Tab", false);
    }
    assert!(action_focused(&h, "keywordRow:Seam boundary A"), "Tab gives the real FIRST row focus");
    key(&mut h, "Tab", true);
    assert!(action_focused(&h, "keywordList:delete"), "FIRST row -> actual Delete");
    key(&mut h, "Tab", true);
    assert!(action_focused(&h, "keywordList:create"));
    key(&mut h, "Tab", true);
    assert!(focused(&h, FILTER));
}

#[test]
fn actual_last_row_transitions_to_its_arrow_and_back_and_enter_selects_it() {
    let mut h = keywords();
    click(&mut h, "keywordRow:Seam boundary A");
    click(&mut h, FILTER);
    for _ in 0..3 {
        key(&mut h, "Tab", false);
    }
    assert!(action_focused(&h, "keywordRow:Seam boundary A"), "Tab starts on the actual FIRST row");
    // The two filtered leaf rows have no triangle; their checkboxes are disabled with no
    // selected photos. Each row's real Show Photos arrow remains its next focus stop.
    key(&mut h, "Tab", false);
    assert!(action_focused(&h, "keywordShow:Seam boundary A"));
    key(&mut h, "Tab", false);
    assert!(action_focused(&h, "keywordRow:Seam boundary Z"), "previous row's arrow -> actual LAST row");
    key(&mut h, "Tab", false);
    assert!(action_focused(&h, "keywordShow:Seam boundary Z"), "LAST row -> its actual action");
    key(&mut h, "Tab", true);
    assert!(action_focused(&h, "keywordRow:Seam boundary Z"), "last action -> actual LAST row");
    let undo = h.app.session.undo.len();
    key(&mut h, "Enter", false);
    assert_eq!(h.app.ui.keyword_list_selected.as_deref(), Some("Seam boundary Z"));
    assert_eq!(h.app.session.undo.len(), undo);
}

#[test]
fn create_and_delete_keep_real_actions_and_chosen_parent() {
    let mut h = keywords();
    click(&mut h, "keywordRow:Seam boundary Z");
    let undo = h.app.session.undo.len();
    click(&mut h, FILTER);
    key(&mut h, "Tab", false);
    key(&mut h, "Enter", false);
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::KeywordTag { parent: Some(parent), .. }) if parent == "Seam boundary Z"));
    assert_eq!(h.app.session.undo.len(), undo, "opening a dialog is no document edit");
}

#[test]
fn caret_selection_return_and_escape_keep_filter_out_of_document_history() {
    let mut h = keywords();
    let undo = h.app.session.undo.len();
    key(&mut h, "End", false);
    key(&mut h, "Left", true);
    text(&mut h, "X");
    assert_eq!(filter(&h), "Seam boundarX", "typing replaces just the selected character");
    key(&mut h, "Enter", false);
    assert_eq!(filter(&h), "Seam boundarX");
    assert!(!focused(&h, FILTER), "single-line Return ends the edit");
    click(&mut h, FILTER);
    key(&mut h, "End", false);
    text(&mut h, " unfinished");
    key(&mut h, "Escape", false);
    assert_eq!(filter(&h), "Seam boundarX", "Escape restores the value at the start of this edit");
    assert_eq!(h.app.session.undo.len(), undo);
}

#[test]
fn hidden_panel_keeps_filter_but_reopening_starts_a_fresh_edit() {
    let mut h = keywords();
    let undo = h.app.session.undo.len();
    h.app.ui.toggle_folded("keywordList");
    h.settle(SETTLE);
    h.app.ui.toggle_folded("keywordList");
    h.settle(SETTLE);
    assert_eq!(filter(&h), "Seam boundary");
    click(&mut h, FILTER);
    key(&mut h, "End", false);
    text(&mut h, " unfinished");
    key(&mut h, "Escape", false);
    assert_eq!(filter(&h), "Seam boundary", "hidden edit cannot restore a stale empty value");
    assert_eq!(h.app.session.undo.len(), undo);
}
