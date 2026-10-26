//! Location rules through the commands (LRC-MAP-FILTER): `library.filter` and smart albums with
//! Has GPS and GPS Area, and a view's own narrowing ([`crate::Session::narrow`]) over the filter.

use lightcraft_catalog::{PhotoId, RuleSet};
use serde_json::{Value, json};

use crate::Session;

/// The demo library with Paris, Fiji and Samoa on its first three photos; the rest have no GPS.
fn tagged() -> (Session, Vec<PhotoId>) {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(3).collect();
    for (id, gps) in ids.iter().zip([[48.8584, 2.2945], [-17.7, 178.0], [-13.8, -172.0]]) {
        s.execute("photo.setGps", &json!({"ids": [id.0], "lat": gps[0], "lon": gps[1]})).unwrap();
    }
    (s, ids)
}

fn sorted(mut v: Vec<PhotoId>) -> Vec<PhotoId> {
    v.sort();
    v
}

fn area(south: f64, west: f64, north: f64, east: f64) -> Value {
    json!({"south": south, "west": west, "north": north, "east": east})
}

#[test]
fn library_filter_takes_gps_rules() {
    let (mut s, ids) = tagged();
    let total = s.visible_cloned().len();
    // the Pacific, across the antimeridian
    let pacific = json!({"ruleSet": {"rules": [{"field": "gpsArea", "op": "inside", "value": area(-30.0, 170.0, 30.0, -170.0)}]}});
    s.execute("library.filter", &pacific).unwrap();
    assert_eq!(sorted(s.visible_cloned()), sorted(vec![ids[1], ids[2]]));
    s.execute("library.filter", &json!({"ruleSet": {"rules": [{"field": "hasGps", "op": "is", "value": false}]}})).unwrap();
    assert_eq!(s.visible_cloned().len(), total - 3);
    assert!(ids.iter().all(|id| !s.visible_cloned().contains(id)));
    // a box that means nothing is refused, and the filter stays as it was
    let before = s.filter.clone();
    for bad in [area(10.0, 0.0, -10.0, 5.0), area(0.0, 0.0, 95.0, 5.0), json!({"south": 0}), json!("Paris")] {
        let r = s.execute("library.filter", &json!({"ruleSet": {"rules": [{"field": "gpsArea", "op": "inside", "value": bad}]}}));
        assert!(r.is_err(), "{bad}");
        assert_eq!(s.filter, before);
    }
    // the field is listed for agents, in the Location group
    let fields = s.execute("album.ruleFields", &json!({})).unwrap();
    let f = fields.as_array().unwrap().iter().find(|f| f["field"] == "gpsArea").cloned().unwrap();
    assert_eq!((f["kind"].as_str(), f["group"].as_str()), (Some("area"), Some("Location")), "{f}");
}

#[test]
fn a_smart_album_keeps_the_photos_in_an_area() {
    let (mut s, ids) = tagged();
    let r = s
        .execute(
            "album.createSmart",
            &json!({"name": "Europe", "rules": {"ruleSet": {"rules": [{"field": "gpsArea", "op": "inside", "value": area(35.0, -10.0, 60.0, 30.0)}]}}}),
        )
        .unwrap();
    assert_eq!(r["count"], 1, "{r}");
    // moving Fiji to Rome puts it in
    s.execute("photo.setGps", &json!({"ids": [ids[1].0], "lat": 41.9, "lon": 12.5})).unwrap();
    let album = lightcraft_catalog::AlbumId(r["id"].as_u64().unwrap());
    assert_eq!(s.catalog.album_count(album), 2);
    assert!(
        s.execute(
            "album.createSmart",
            &json!({"name": "Bad", "rules": {"ruleSet": {"rules": [{"field": "gpsArea", "op": "inside", "value": null}]}}})
        )
        .is_err()
    );
}

#[test]
fn a_views_narrowing_sits_on_the_filter_and_goes_away_cleanly() {
    let (mut s, ids) = tagged();
    s.execute("library.filter", &json!({"text": ""})).unwrap();
    let (base_gen, all) = s.visible_shared();
    let filter = s.filter.clone();
    let rules: RuleSet = serde_json::from_value(json!({"rules": [{"field": "hasGps", "op": "is", "value": true}]})).unwrap();
    s.narrow = Some(rules);
    let (narrowed_gen, tagged) = s.visible_shared();
    assert_ne!(narrowed_gen, base_gen, "a new list");
    assert_eq!(sorted(tagged.to_vec()), sorted(ids.clone()));
    // the library's own filter is untouched, and the list before narrowing is still there
    assert_eq!(s.filter, filter);
    assert_eq!(s.visible_base_shared().1.len(), all.len());
    // the same narrowing again: the same list (no recomputation)
    assert_eq!(s.visible_shared().0, narrowed_gen);
    // it follows the catalog: an untagged photo tagged now joins
    let other = *all.iter().find(|id| !ids.contains(id)).unwrap();
    s.execute("photo.setGps", &json!({"ids": [other.0], "lat": 1.0, "lon": 2.0})).unwrap();
    assert_eq!(s.visible_cloned().len(), 4);
    // and the filter still applies under it
    s.execute("library.filter", &json!({"ruleSet": {"rules": [{"field": "gpsArea", "op": "inside", "value": area(-30.0, 170.0, 30.0, -170.0)}]}}))
        .unwrap();
    assert_eq!(sorted(s.visible_cloned()), sorted(vec![ids[1], ids[2]]));
    s.execute("library.filter", &json!({"ruleSet": null})).unwrap();
    s.narrow = None;
    assert_eq!(s.visible_cloned().len(), all.len());
}

#[test]
fn narrowing_an_empty_library_is_empty() {
    let mut s = Session::new();
    s.narrow =
        Some(serde_json::from_value(json!({"rules": [{"field": "gpsArea", "op": "inside", "value": area(-90.0, -180.0, 90.0, 180.0)}]})).unwrap());
    assert!(s.visible().is_empty());
    assert!(s.visible_base_shared().1.is_empty());
}
