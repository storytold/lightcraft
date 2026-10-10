//! Headless tests of Library's Classic panels (P1.4): the panel stacks, Folders, Collections,
//! Quick Develop, Keywording and Keyword List.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::module::PanelId;
use crate::panels::classic;
use crate::{DacApp, Services};

pub(crate) const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

pub(crate) fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = DacApp::new(dac_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.settle(SETTLE);
    run(&mut h, "module.library", json!({}));
    h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true, "right": "none"}), T);
    h.step();
    h.step();
    h
}

pub(crate) fn run(h: &mut Headless, id: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
    r["result"].clone()
}

pub(crate) fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

pub(crate) fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

/// Library's columns are Classic panel stacks: every panel has a foldable header, Solo Mode
/// keeps one open per side, hidden panels go away, and the order is the user's.
#[test]
fn library_columns_are_classic_panels_with_solo_mode() {
    let mut h = demo();
    for p in ["catalog", "folders", "collections", "quickDevelop", "keywording", "keywordList", "metadata"] {
        assert!(has(&h, &format!("classicPanel:{p}")), "{p}");
    }
    assert!(has(&h, "sidebarSection:navigator"));
    // a click folds a panel and unfolds it again
    click(&mut h, "classicPanel:catalog");
    assert!(!classic::is_open(&h.app, PanelId::Catalog) && !has(&h, "source:all"));
    click(&mut h, "classicPanel:catalog");
    assert!(classic::is_open(&h.app, PanelId::Catalog) && has(&h, "source:all"));
    // solo mode: opening one panel folds the others on its side only
    run(&mut h, "panel.solo", json!({"on": true}));
    classic::set_open(&mut h.app, PanelId::Folders, false);
    classic::set_open(&mut h.app, PanelId::Folders, true);
    assert!(classic::is_open(&h.app, PanelId::Folders));
    assert!(!classic::is_open(&h.app, PanelId::Catalog) && !classic::is_open(&h.app, PanelId::Collections));
    assert!(classic::is_open(&h.app, PanelId::Keywording), "the other side is left alone");
    // hidden panels leave the column; the order is the user's
    run(&mut h, "panel.show", json!({"panel": "quickDevelop", "visible": false}));
    run(&mut h, "panel.order", json!({"order": ["metadata", "keywording"]}));
    assert!(!has(&h, "classicPanel:quickDevelop"));
    let y = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.top()).unwrap();
    assert!(y(&h, "classicPanel:metadata") < y(&h, "classicPanel:keywording"));
}
