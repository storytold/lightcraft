//! The filter bar's camera / lens / keyword dropdowns: a search box that takes the typing as soon
//! as the dropdown opens, narrows the rows, keeps the "Any …" row, and starts empty next time.

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

fn rows(h: &Headless, picker: &str) -> Vec<String> {
    let prefix = format!("filterItem:{picker}:");
    h.app.widgets.iter().filter(|(w, _)| w.starts_with(&prefix)).map(|(w, _)| w.clone()).collect()
}

#[test]
fn typing_narrows_the_keyword_rows_and_keeps_any() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    // the filter bar shows over the grid
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    h.app.ui.filter_bar = true;
    h.settle(SETTLE);
    let keywords = h.app.caches.filter_values(&h.app.session.catalog).keywords.clone();
    assert!(keywords.len() >= 2, "the demo library has keywords: {keywords:?}");
    let target = keywords[0].clone();
    ok(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-keyword"}));
    let all = rows(&h, "keyword");
    assert_eq!(all.len(), keywords.len() + 1, "every keyword and Any");
    // no click into the box first: it has the focus when the dropdown opens
    ok(&mut h, "ui.text", json!({"text": target.to_uppercase()}));
    let narrowed = rows(&h, "keyword");
    let matching = keywords.iter().filter(|k| k.to_lowercase().contains(&target.to_lowercase())).count();
    assert_eq!(narrowed.len(), matching + 1, "matches (any case) and Any: {narrowed:?}");
    assert!(narrowed.contains(&"filterItem:keyword:0".to_string()), "the Any row stays");
    assert!(narrowed.len() < all.len());
    // closed and opened again: the search starts empty, and other pickers never saw it
    ok(&mut h, "ui.key", json!({"key": "Escape"}));
    ok(&mut h, "ui.key", json!({"key": "Escape"}));
    ok(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-camera"}));
    let cameras = h.app.caches.filter_values(&h.app.session.catalog).cameras.len();
    assert_eq!(rows(&h, "camera").len(), cameras + 1, "the camera picker isn't filtered by the keyword search");
}
