//! The Smart Rules dialog's live count while the library changes under it (an import commits a batch every
//! frame): it is made on a worker thread, never by a frame; the last count stays shown, marked "updating", and the
//! right one arrives once the changes stop; a rule edit starts its own count.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // (a count of the demo's few photos ends at once: make it take as long as a large library's does)
    h.app.caches.rules_count.slow = Some(Duration::from_millis(40));
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// What the dialog's rules match now, counted the plain way.
fn matching(h: &Headless) -> usize {
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    h.app.session.catalog.count(&lightcraft_catalog::Filter { rule_set: Some(rules.clone()), ..Default::default() })
}

/// Commit one more change to the library, as an import's batch does, and run a frame.
fn change(h: &mut Headless, ids: &[u64], i: usize) {
    let rx = h
        .send("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[i % ids.len()]], "keywords": [format!("batch{i}")]}}))
        .expect("the control channel is open");
    h.step();
    assert_eq!(rx.try_recv().map(|r| r["ok"].clone()).ok(), Some(json!(true)), "the change was made in that frame");
}

#[test]
fn the_count_is_not_made_by_frames_while_the_library_changes_and_ends_right() {
    let mut h = demo();
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    let r = h.request("engine.execute", json!({"command": "dialog.smartAlbum", "params": {"name": "Live"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.caches.rules_count.last().is_some()), "the first count arrives");
    let revision = h.app.session.catalog.revision;
    // a revision every frame
    let mut marked = false;
    for i in 0..60 {
        change(&mut h, &ids, i);
        marked |= has(&h, "label:countUpdating");
    }
    assert!(h.app.session.catalog.revision >= revision + 60, "a revision per frame");
    assert!(marked, "while the library changes, the count shown says it is updating");
    assert_eq!(h.app.caches.rules_count.ui_thread_scans, 0, "no frame counted the library");
    // the changes stop: the right count arrives, and the mark goes
    let revision = h.app.session.catalog.revision;
    assert!(h.step_until(T, |h| h.app.caches.rules_count.last().is_some_and(|(_, r)| r == revision) && !has(h, "label:countUpdating")));
    assert_eq!(h.app.caches.rules_count.last().map(|(n, _)| n), Some(matching(&h)));
    assert_eq!(h.app.caches.rules_count.ui_thread_scans, 0);
}

#[test]
fn a_rule_edit_counts_for_its_own_rules() {
    let mut h = demo();
    let r = h.request("engine.execute", json!({"command": "dialog.smartAlbum", "params": {"name": "Edit"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.caches.rules_count.last().is_some()));
    let all = matching(&h);
    assert!(all > 0, "the demo library has photos");
    // rules no photo meets
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    rules.rules = vec![serde_json::from_value(json!({"field": "keywords", "op": "contains", "value": "no-such-keyword"})).unwrap()];
    h.step();
    assert!(!has(&h, "label:countUpdating"), "the count of the rules before ({all}) isn't shown for these");
    assert!(h.step_until(T, |h| h.app.caches.rules_count.last().map(|(n, _)| n) == Some(0)), "the edit's own count arrives");
    assert_eq!(matching(&h), 0);
    assert_eq!(h.app.caches.rules_count.ui_thread_scans, 0);
}
