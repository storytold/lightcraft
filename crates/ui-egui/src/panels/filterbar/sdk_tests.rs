//! SDK-only locator contract; portable physical-input tests stay in `tests.rs`.
use super::tests::{ask, fixture, visible};
use serde_json::json;

#[test]
fn filter_kind_sdk_row_locator_is_unique_and_clicks_real_raw_row() {
    let mut h = fixture(1900.0);
    ask(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-kind"}));
    for value in ["all", "image", "raw", "video", "hdr", "panorama", "hdrPanorama"] {
        let id = format!("filter:kind-{value}");
        assert_eq!(h.app.widgets.iter().filter(|(name, _)| name == &id).count(), 1, "one real-pass locator for {id}");
        assert_eq!(h.app.widget_hits.iter().filter(|(name, _)| name == &id).count(), 1, "one actual input owner for {id}");
    }
    ask(&mut h, "ui.clickWidget", json!({"id": "filter:kind-raw"}));
    assert_eq!(visible(&mut h), vec![102]);
    assert_eq!(h.app.session.filter.kind, Some(lightcraft_catalog::MediaKind::Raw));
}
