//! Portable native baseline/SDK-candidate qualification over the real live-filter callers.
//! The candidate is rejected if it cannot traverse the callers' real native neighbors.
use std::time::Duration;

use egui::{Event, ImeEvent};
use serde_json::{Value, json};

use super::*;
use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
const KEYWORDS: &str = "field:keywordFilter";
const EFFECTS: &str = "field:effectSearch";

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

fn has(h: &Headless, widget: &str) -> bool {
    h.app.widgets.iter().any(|(name, _)| name == widget)
}

/// This suite runs unchanged against the SDK candidate and its native baseline.
fn focused_on(h: &Headless, widget: &str) -> bool {
    let native = id(widget);
    h.view.ctx.memory(|memory| memory.has_focus(native))
        || craft::ui::runtime::focus::focused_rec(&h.view.ctx).is_some_and(|rec| {
            // The candidate's component is the genuine hit owner in its registered field.
            h.app.widget_hits.iter().any(|(name, hit)| name == widget && hit.id == rec.stop.id)
        })
}

/// Identify the real focused widget through its response and the named caller action's
/// published geometry. Native action aliases are hover probes, not their focus owners.
fn action_focused(h: &Headless, widget: &str) -> bool {
    let Some((_, rect)) = h.app.widgets.iter().find(|(name, _)| name == widget) else { return false };
    h.view
        .ctx
        .memory(|memory| memory.focused())
        .and_then(|id| h.view.ctx.read_response(id))
        .is_some_and(|response| response.rect == *rect && response.sense.senses_click())
}

fn tab_to_action(h: &mut Headless, widget: &str) {
    for _ in 0..24 {
        key(h, "Tab", false);
        if action_focused(h, widget) {
            return;
        }
    }
    panic!("Tab did not reach actual caller action {widget}; focus={:?}", h.view.ctx.memory(|memory| memory.focused()));
}

fn keywords() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    ask(&mut h, "ui.set", json!({"view": "photoGrid", "right": "keywords"}));
    ask(&mut h, "engine.execute", json!({"command": "keyword.create", "params": {"name": "Seam unique keyword"}}));
    assert!(has(&h, KEYWORDS));
    h
}

fn keyword_filter(h: &Headless) -> String {
    h.view.ctx.data(|data| data.get_temp(egui::Id::unique("keyword-list-filter"))).unwrap_or_default()
}

fn effects() -> Headless {
    let mut h = keywords();
    h.app.session.mask_effects = vec![
        lightcraft_engine::cmd::mask_effects::MaskEffect { name: "Seam Sky".into(), ..Default::default() },
        lightcraft_engine::cmd::mask_effects::MaskEffect { name: "Seam Skin".into(), ..Default::default() },
    ];
    h.app.ui.dialog = Some(Dialog::ManageEffects {
        search: String::new(),
        selected: vec!["Seam Sky".into()],
        rename: None,
        confirm_delete: false,
        new_group: String::new(),
    });
    h.settle(SETTLE);
    assert!(has(&h, EFFECTS));
    h
}

fn effect_filter(h: &Headless) -> &str {
    let Some(Dialog::ManageEffects { search, .. }) = &h.app.ui.dialog else { panic!("manager closed") };
    search
}

#[test]
fn keyword_filter_is_live_and_escape_restores_without_document_history() {
    let mut h = keywords();
    let undo = h.app.session.undo.len();
    click(&mut h, KEYWORDS);
    assert!(focused_on(&h, KEYWORDS));
    h.app.synthetic.push(Event::Text("Seam unique".into()));
    h.step();
    assert_eq!(keyword_filter(&h), "Seam unique", "caller sees input in the input pass");
    assert!(has(&h, "keywordRow:Seam unique keyword"));
    assert!(!has(&h, "keywordRow:mountains"), "same-pass filtering, not Return-only");
    assert!(focused_on(&h, KEYWORDS), "typing keeps the actual text owner focused");
    key(&mut h, "Escape", false);
    assert_eq!(keyword_filter(&h), "");
    assert!(has(&h, "keywordRow:mountains"));
    assert_eq!(h.app.session.undo.len(), undo);
}

#[test]
fn filter_menu_paste_and_clearing_preserve_caller_values() {
    let mut h = keywords();
    click(&mut h, KEYWORDS);
    h.view.clipboard = "Seam unique".into();
    ask(&mut h, "ui.clickWidget", json!({"id": KEYWORDS, "button": "right"}));
    let alias = format!("{KEYWORDS}:paste");
    assert!(has(&h, &alias));
    let hit = h.app.widget_hits.iter().find(|(name, _)| name == &alias).expect("menu hit").1;
    assert!(hit.interact_rect.is_positive(), "the menu alias has a real, unclipped interaction area");
    click(&mut h, &alias);
    assert_eq!(keyword_filter(&h), "Seam unique");
    assert!(focused_on(&h, KEYWORDS), "paste returns to the actual text owner");
    ask(&mut h, "ui.key", json!({"key": "A", "cmd": true}));
    key(&mut h, "Backspace", false);
    assert_eq!(keyword_filter(&h), "");
    assert!(has(&h, "keywordRow:mountains"));
    assert!(focused_on(&h, KEYWORDS));
}

#[test]
fn keyword_filter_commits_unicode_once_without_document_history() {
    let mut h = keywords();
    click(&mut h, KEYWORDS);
    let undo = h.app.session.undo.len();
    h.app.synthetic.push(Event::Ime(ImeEvent::Preedit { text: "東京".into(), active_range_chars: None }));
    h.step();
    // Native egui exposes preedit in its temporary string; the SDK keeps it separate.
    // The shared observable requirement is one committed Unicode value with no history.
    assert!(focused_on(&h, KEYWORDS));
    assert_eq!(h.app.session.undo.len(), undo);
    h.app.synthetic.push(Event::Ime(ImeEvent::Commit("東京".into())));
    h.step();
    assert_eq!(keyword_filter(&h), "東京");
    assert!(focused_on(&h, KEYWORDS));
    key(&mut h, "Escape", false);
    assert_eq!(keyword_filter(&h), "");
    assert_eq!(h.app.session.undo.len(), undo);
}

#[test]
fn effect_filter_is_live_without_committing_or_renaming_presets() {
    let mut h = effects();
    let before = h.app.session.mask_effects.clone();
    let undo = h.app.session.undo.len();
    click(&mut h, EFFECTS);
    h.app.synthetic.push(Event::Text("Sky".into()));
    h.step();
    assert_eq!(effect_filter(&h), "Sky");
    assert!(has(&h, "effectRow:Seam Sky"));
    assert!(!has(&h, "effectRow:Seam Skin"));
    assert!(focused_on(&h, EFFECTS));
    key(&mut h, "Escape", false);
    assert!(h.app.ui.dialog.is_none(), "the native manager's Escape closes the dialog");
    assert_eq!(h.app.session.mask_effects, before);
    assert_eq!(h.app.session.undo.len(), undo);
}

#[test]
fn keyword_filter_tab_reaches_native_create_and_shift_tab_returns() {
    let mut h = keywords();
    click(&mut h, KEYWORDS);
    key(&mut h, "Tab", false);
    assert!(action_focused(&h, "keywordList:create"), "Tab must reach the actual caller + action");
    key(&mut h, "Tab", true);
    assert!(focused_on(&h, KEYWORDS), "reverse traversal returns to the filter");
    key(&mut h, "Tab", false);
    key(&mut h, "Enter", false);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::KeywordTag { .. })), "the native + really received Tab, not a dummy stop");
}

#[test]
fn effect_filter_reaches_actual_rename_action_returns_and_preserves_native_escape() {
    let mut h = effects();
    click(&mut h, EFFECTS);
    ask(&mut h, "ui.text", json!({"text": "Sky"}));
    // This action is made by ui.add_enabled(egui::Button), not the shared Button seam.
    // Its automation id names its exact rectangle; Enter below proves the control acts.
    tab_to_action(&mut h, "button:effectRename");
    for _ in 0..12 {
        key(&mut h, "Tab", true);
        if focused_on(&h, EFFECTS) {
            break;
        }
    }
    assert!(focused_on(&h, EFFECTS), "reverse traversal from the actual native neighbor returns");
    ask(&mut h, "ui.text", json!({"text": " new"}));
    key(&mut h, "Escape", false);
    assert!(h.app.ui.dialog.is_none(), "preserve the native manager's Escape close behavior");
    // A fresh identical manager proves activation of the named action, not just its geometry.
    let mut h = effects();
    click(&mut h, EFFECTS);
    ask(&mut h, "ui.text", json!({"text": "Sky"}));
    tab_to_action(&mut h, "button:effectRename");
    key(&mut h, "Enter", false);
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::ManageEffects { rename: Some((old, _)), .. }) if old == "Seam Sky"),
        "Enter must activate the actual effect Rename action reached by Tab"
    );
}
