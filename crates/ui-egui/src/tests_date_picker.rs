//! The date picker in the smart-album rule editor: picking days, months and years, moving between
//! months, and the bounds a "between" puts on each of its two dates.

use std::time::Duration;

use dac_catalog::Rule;
use serde_json::json;

use crate::headless::Headless;
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn editor(rules: serde_json::Value) -> Headless {
    let app = DacApp::new(dac_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("engine.execute", json!({"command": "dialog.smartAlbum", "params": {"name": "Dates"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let Some(crate::state::Dialog::SmartRules { rules: r, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    *r = serde_json::from_value(json!({"rules": rules})).unwrap();
    h.settle(SETTLE);
    h
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn value(h: &Headless, i: usize) -> serde_json::Value {
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    let Some(Rule::Field { value, .. }) = rules.rules.get(i) else { panic!("no rule {i}") };
    value.clone()
}

/// A day, then a month, then a year: each writes the date at that precision and closes the picker.
#[test]
fn pick_a_day_a_month_and_a_year() {
    let mut h = editor(json!([{"field": "captureDate", "op": "is", "value": "2026-01-20"}]));
    click(&mut h, "datePicker:rules-0");
    assert!(has(&h, "datePickerCell:2026-01-20:rules-0"), "opens on the rule's month");
    click(&mut h, "datePickerNext:rules-0");
    click(&mut h, "datePickerCell:2026-02-14:rules-0");
    assert_eq!(value(&h, 0), json!("2026-02-14"));
    assert!(!has(&h, "datePickerCell:2026-02-14:rules-0"), "picking closes it");
    click(&mut h, "datePicker:rules-0");
    click(&mut h, "datePickerPrecision:month:rules-0");
    click(&mut h, "datePickerCell:2026-08:rules-0");
    assert_eq!(value(&h, 0), json!("2026-08"));
    click(&mut h, "datePicker:rules-0");
    assert!(has(&h, "datePickerCell:2026-08:rules-0"), "a month opens at month precision");
    click(&mut h, "datePickerPrecision:year:rules-0");
    click(&mut h, "datePickerPrev:rules-0");
    click(&mut h, "datePickerCell:2015:rules-0");
    assert_eq!(value(&h, 0), json!("2015"));
}

/// Each date of a "between" can't pass the other: the earlier one can't be picked after the later.
#[test]
fn between_dates_stay_in_order() {
    let mut h = editor(json!([{"field": "captureDate", "op": "between", "value": ["2026-03", "2026-06"]}]));
    click(&mut h, "datePicker:rules-0-a");
    click(&mut h, "datePickerCell:2026-09:rules-0-a");
    assert_eq!(value(&h, 0), json!(["2026-03", "2026-06"]), "after the later date: not taken");
    click(&mut h, "datePickerCell:2026-05:rules-0-a");
    assert_eq!(value(&h, 0), json!(["2026-05", "2026-06"]));
    click(&mut h, "datePicker:rules-0-b");
    click(&mut h, "datePickerCell:2026-04:rules-0-b");
    assert_eq!(value(&h, 0), json!(["2026-05", "2026-06"]), "before the earlier date: not taken");
    click(&mut h, "datePickerCell:2026-12:rules-0-b");
    assert_eq!(value(&h, 0), json!(["2026-05", "2026-12"]));
}

/// A rule without a readable date opens on today's month (the session's clock), and opening it
/// leaves the rule as written.
#[test]
fn an_unreadable_date_opens_on_this_month() {
    let mut h = editor(json!([{"field": "captureDate", "op": "after", "value": "banana"}]));
    h.app.session.clock = Box::new(|| "2026-10-09T12:00:00".to_string());
    h.settle(SETTLE);
    click(&mut h, "datePicker:rules-0");
    assert!(has(&h, "datePickerCell:2026-10-09:rules-0"));
    assert_eq!(value(&h, 0), json!("banana"), "opening doesn't change the rule");
}

/// Esc closes the calendar only, not the rule editor around it (and its unsaved edits).
#[test]
fn escape_closes_only_the_calendar() {
    let mut h = editor(json!([{"field": "captureDate", "op": "is", "value": "2026-01-20"}]));
    click(&mut h, "datePicker:rules-0");
    assert!(has(&h, "datePickerCell:2026-01-20:rules-0"));
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!has(&h, "datePickerCell:2026-01-20:rules-0"), "the calendar closed");
    assert!(h.app.ui.dialog.is_some(), "the editor stays open");
}

/// Esc with a dropdown open (here the field menu) closes the dropdown, not the editor.
#[test]
fn escape_closes_only_a_dropdown() {
    let mut h = editor(json!([{"field": "rating", "op": "gte", "value": 3}]));
    click(&mut h, "ruleField:rules-0");
    assert!(has(&h, "ruleFieldItem:rating:rules-0"));
    let r = h.request("ui.key", json!({"key": "Escape"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!has(&h, "ruleFieldItem:rating:rules-0"), "the menu closed");
    assert!(h.app.ui.dialog.is_some(), "the editor stays open");
}

/// Only date rules get calendars: a "between" on a text field (written by an agent; the check marks
/// it) shows its two values as text, without date pickers that would write dates into it.
#[test]
fn only_dates_get_calendars() {
    let h = editor(json!([{"field": "title", "op": "between", "value": ["a", "b"]}]));
    assert!(!has(&h, "datePicker:rules-0-a") && !has(&h, "datePicker:rules-0-b"));
    assert!(has(&h, "ruleProblem:rules-0"), "and the operator is marked");
}
