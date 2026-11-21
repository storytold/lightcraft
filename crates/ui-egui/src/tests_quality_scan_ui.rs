//! The Image Quality Scanner in the Library (issue #1313): the filter bar's quality picker and score
//! floor appear once a scan has scored a photo, pick the verdict, and the chips say what is on.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn ok(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    h.settle(SETTLE);
}

fn drawn(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// The demo library with the filter bar over its grid.
fn filter_bar() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    h.app.ui.filter_bar = true;
    h.settle(SETTLE);
    h
}

/// Nothing scored yet: no quality picker. One scan later it is there, its rows set the filter, and the
/// chip says the same.
#[test]
fn the_quality_picker_appears_once_a_scan_has_scored_a_photo() {
    let mut h = filter_bar();
    assert!(!drawn(&h, "dropdown:filter-quality"), "a catalog no scan reached shows no quality picker");
    // score the whole (small) demo catalog, to the end: the scan itself is the engine's test's
    let r = h.request("engine.execute", json!({"command": "photo.qualityScan", "params": {"scope": "catalog", "wait": true}}), SETTLE);
    assert_eq!(r["ok"], true, "{r}");
    assert!(r["result"]["measured"].as_u64().is_some_and(|n| n > 0), "{r}");
    h.settle(SETTLE);
    assert!(drawn(&h, "dropdown:filter-quality"), "the picker appears once a photo carries a score");
    // "Quality: rejected" narrows the view to the photos the scan rejected, and the chip names it
    ok(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-quality"}));
    ok(&mut h, "ui.clickWidget", json!({"id": "filterItem:quality:3"}));
    assert_eq!(h.app.session.filter.quality.as_deref(), Some("rejected"));
    let rejected = h.app.session.visible_cloned().len();
    // the view is the photos whose verdict the scan stored as rejected, no others
    let stored = h
        .app
        .session
        .catalog
        .photos()
        .filter(|p| p.analysis.and_then(|a| a.quality).is_some_and(|q| q.verdict == lcc_catalog::QualityVerdict::Rejected))
        .count();
    assert_eq!(rejected, stored, "the picker shows exactly the rejected photos");
    let chips: Vec<String> = lcc_engine::filter_chips(&h.app.session.filter, &h.app.session.catalog).into_iter().map(|c| c.label).collect();
    assert!(chips.iter().any(|c| c == "Quality: rejected"), "{chips:?}");
    // "Any photo" clears it
    ok(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-quality"}));
    ok(&mut h, "ui.clickWidget", json!({"id": "filterItem:quality:0"}));
    assert!(h.app.session.filter.quality.is_none());
    let chips = lcc_engine::filter_chips(&h.app.session.filter, &h.app.session.catalog);
    assert!(chips.iter().all(|c| !c.label.starts_with("Quality: ")), "the chip goes with the filter");
}

/// The score floor is a button that becomes a slider and comes off again with its ×.
#[test]
fn the_score_floor_is_set_and_cleared_from_the_filter_bar() {
    let mut h = filter_bar();
    let r = h.request("engine.execute", json!({"command": "photo.qualityScan", "params": {"scope": "catalog", "wait": true}}), SETTLE);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    ok(&mut h, "ui.clickWidget", json!({"id": "button:qualityMinSet"}));
    assert_eq!(h.app.session.filter.quality_min, Some(50.0), "the button sets a floor to start from");
    assert!(drawn(&h, "slider:qualityMin"), "a floor is a slider once it is set");
    let chips: Vec<String> = lcc_engine::filter_chips(&h.app.session.filter, &h.app.session.catalog).into_iter().map(|c| c.label).collect();
    assert!(chips.iter().any(|c| c == "Quality score at least: 50"), "{chips:?}");
    // the chip's clear takes the floor off, and the button comes back
    ok(&mut h, "ui.clickWidget", json!({"id": "button:qualityMinClear"}));
    assert_eq!(h.app.session.filter.quality_min, None);
    assert!(drawn(&h, "button:qualityMinSet"), "without a floor the button is back");
    let chips = lcc_engine::filter_chips(&h.app.session.filter, &h.app.session.catalog);
    assert!(chips.iter().all(|c| !c.label.starts_with("Quality score")), "the chip goes with the floor");
}

/// The Library menu opens the dialog, and Scan hands the settings to `photo.qualityScan`: the scores
/// land on the photos, and the dialog reopens as it was left.
#[test]
fn the_menu_opens_the_scanner_and_a_scan_lands_on_the_photos() {
    let mut h = filter_bar();
    ok(&mut h, "ui.menu.invoke", json!({"id": "dialog.qualityScan"}));
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::QualityScanner { .. })), "{:?}", h.app.ui.dialog);
    for id in ["radio:qualityScope:selection", "radio:qualityScope:catalog", "check:qualityApplyFlags", "check:qualityRescan"] {
        assert!(drawn(&h, id), "{id} is not drawn");
    }
    ok(&mut h, "ui.dialog.confirm", json!({}));
    // the scan runs in the background: the frames pump it to the end (or it is stopped, leaving the demo quiet)
    let mut measured = 0;
    for _ in 0..600 {
        h.step();
        let r = h.request("engine.execute", json!({"command": "photo.qualityScan", "params": {"status": true}}), T);
        if r["result"]["active"] == false {
            measured = h.app.session.catalog.quality_scanned();
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(measured > 0, "the scan scored photos, and the picker reads the catalog's score count");
    assert!(drawn(&h, "dropdown:filter-quality"));
    // what the dialog confirmed is what reopens
    let kept = h.app.ui.quality_scan_settings.clone().expect("the dialog keeps what was confirmed");
    assert_eq!(kept.scope, "selection");
    assert_eq!((kept.accepted, kept.rejected), (70.0, 25.0));
}
