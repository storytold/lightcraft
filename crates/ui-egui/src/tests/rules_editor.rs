//! The smart-album rule editor's layout: a rule's + and − never cover its value (#2353), so both
//! calendars of a "between" can be clicked (#2352), and hovering an item of the field menu doesn't
//! make it wrap (#2354).

use std::time::Duration;

use lightcraft_catalog::rules::{FIELD_GROUPS, TOP_LEVEL_FIELDS};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The rule editor holding `rules`, opened after a narrower dialog: the dialogs share one window,
/// which keeps the width the last one left it at.
fn editor(rules: serde_json::Value) -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("engine.execute", json!({"command": "dialog.newSmartAlbum", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h.app.ui.dialog = None;
    h.settle(SETTLE);
    let r = h.request("engine.execute", json!({"command": "dialog.smartAlbum", "params": {"name": "Dates"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let Some(crate::state::Dialog::SmartRules { rules: r, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    *r = serde_json::from_value(json!({"rules": rules})).unwrap();
    h.settle(SETTLE);
    h
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn request(h: &mut Headless, method: &str, id: &str) {
    let r = h.request(method, json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{method} {id}: {r}");
    h.settle(SETTLE);
}

fn rule_count(h: &Headless) -> usize {
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    rules.rules.len()
}

/// A "between" on a date is the longest rule: its + and − follow its second date and calendar
/// instead of being drawn over them, the dialog widens to hold it, and every row's buttons stay
/// lined up at the right.
#[test]
fn add_and_remove_never_cover_a_between_dates_rule() {
    let h = editor(json!([
        {"field": "captureDate", "op": "between", "value": ["2026-02-01", "2026-02-28"]},
        {"field": "rating", "op": "gte", "value": 3},
    ]));
    let dialog = rect(&h, "dialog:window");
    for b in ["button:ruleAdd-rules-0", "button:ruleRemove-rules-0"] {
        let b_rect = rect(&h, b);
        assert!(dialog.contains_rect(b_rect), "{b} {b_rect:?} is outside the dialog {dialog:?}");
        for v in ["field:rules-0-a", "datePicker:rules-0-a", "field:rules-0-b", "datePicker:rules-0-b"] {
            let v_rect = rect(&h, v);
            assert!(dialog.contains_rect(v_rect), "{v} {v_rect:?} is outside the dialog {dialog:?}");
            assert!(!b_rect.intersects(v_rect), "{b} {b_rect:?} covers {v} {v_rect:?}");
        }
    }
    let (long, short) = (rect(&h, "button:ruleAdd-rules-0"), rect(&h, "button:ruleAdd-rules-1"));
    assert!((long.right() - short.right()).abs() < 0.5, "the rows' + don't line up: {long:?} vs {short:?}");
}

/// The end date's calendar is there and opens when clicked; the click doesn't land on the + that
/// used to cover it (and add a rule).
#[test]
fn the_end_dates_calendar_opens() {
    let mut h = editor(json!([{"field": "captureDate", "op": "between", "value": ["2026-03", "2026-06"]}]));
    request(&mut h, "ui.clickWidget", "datePicker:rules-0-b");
    assert!(has(&h, "datePickerCell:2026-06:rules-0-b"), "the end date's calendar opened on its month");
    assert_eq!(rule_count(&h), 1, "no rule was added");
}

/// Hovering any field in the field menu, top-level or in a group's submenu, keeps it the size it
/// was: a hovered item used to need 2 points more, so the longest name of a submenu sized to it
/// (Date ▸ Capture Date, Keywords & People ▸ Keyword Count) wrapped onto a second line and the
/// menu jumped.
#[test]
fn hovering_a_field_keeps_its_size() {
    let mut h = editor(json!([{"field": "rating", "op": "gte", "value": 3}]));
    request(&mut h, "ui.clickWidget", "ruleField:rules-0");
    let hover = |h: &mut Headless, id: &str| {
        let item = format!("ruleFieldItem:{id}:rules-0");
        let before = rect(h, &item);
        request(h, "ui.hoverWidget", &item);
        let after = rect(h, &item);
        assert_eq!(after.size(), before.size(), "{id} changed size when hovered: {before:?} → {after:?}");
    };
    for id in TOP_LEVEL_FIELDS {
        hover(&mut h, id);
    }
    for (group, ids) in FIELD_GROUPS {
        request(&mut h, "ui.hoverWidget", &format!("ruleFieldGroup:{group}:rules-0"));
        for id in *ids {
            hover(&mut h, id);
        }
    }
}
