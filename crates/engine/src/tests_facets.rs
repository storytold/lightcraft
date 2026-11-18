//! `library.facets` (issue #218): the values each metadata field takes among the photos in view, with counts.

use lcc_catalog::Op;
use serde_json::json;

use crate::Session;

/// The demo library with three photos given a camera and keywords of their own (the others keep theirs).
fn library() -> (Session, Vec<lcc_catalog::PhotoId>) {
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().filter(|p| p.in_library()).take(3).map(|p| p.id).collect();
    for (id, camera, keywords) in
        [(ids[0], "Facet Cam A", vec!["qq-one"]), (ids[1], "Facet Cam A", vec!["qq-one", "qq-two"]), (ids[2], "Facet Cam B", vec!["qq-two"])]
    {
        let mut m = s.catalog.photo(id).unwrap().meta.clone();
        m.camera = camera.into();
        m.keywords = keywords.into_iter().map(String::from).collect();
        s.catalog.apply(Op::SetMeta { id, meta: Box::new(m) }).unwrap();
    }
    (s, ids)
}

fn values(r: &serde_json::Value, field: &str) -> Vec<(String, u64)> {
    r[field]["values"].as_array().unwrap().iter().map(|v| (v["value"].as_str().unwrap().to_string(), v["count"].as_u64().unwrap())).collect()
}

#[test]
fn counts_the_values_of_the_photos_in_view_bounded_and_without_a_fields_own_constraint() {
    let (mut s, _) = library();
    let shown = s.visible().len() as u64;
    let r = s.execute("library.facets", &json!({"fields": ["camera", "keyword", "rating"]})).unwrap();
    assert_eq!(r["camera"]["photos"], shown);
    let cams = values(&r, "camera");
    assert!(cams.contains(&("Facet Cam A".to_string(), 2)) && cams.contains(&("Facet Cam B".to_string(), 1)), "{cams:?}");
    let with_camera: u64 = cams.iter().map(|c| c.1).sum();
    assert_eq!(with_camera + r["camera"]["none"].as_u64().unwrap(), shown, "every photo once, with a camera or without");
    assert_eq!(r["camera"]["distinct"].as_u64().unwrap() as usize, cams.len());
    // a photo with several keywords counts once under each
    let kws = values(&r, "keyword");
    assert!(kws.contains(&("qq-one".to_string(), 2)) && kws.contains(&("qq-two".to_string(), 2)), "{kws:?}");
    // most photos first; rating by value
    assert!(cams.windows(2).all(|w| w[0].1 >= w[1].1));
    let ratings: Vec<u32> = values(&r, "rating").iter().map(|v| v.0.parse().unwrap()).collect();
    assert!(ratings.windows(2).all(|w| w[0] < w[1]), "{ratings:?}");
    assert!(r.get("lens").is_none(), "only the fields asked for");

    // unknown fields are rejected and the values bounded
    assert!(s.execute("library.facets", &json!({"fields": ["camera", "shoe size"]})).is_err());
    assert!(s.execute("library.facets", &json!({"fields": "camera"})).is_err());
    assert!(s.execute("library.facets", &json!({"limit": -1})).is_err());
    let r = s.execute("library.facets", &json!({"fields": ["keyword"], "limit": 1})).unwrap();
    assert_eq!(r["keyword"]["values"].as_array().unwrap().len(), 1);
    assert!(r["keyword"]["distinct"].as_u64().unwrap() > 1, "distinct counts them all");
    let all = s.execute("library.facets", &json!({})).unwrap();
    assert_eq!(all.as_object().unwrap().len(), crate::cmd::facets::fields().len());

    // a field narrowed by the filter still lists the values it could take; the other fields count only what matches
    s.execute("library.filter", &json!({"camera": "Facet Cam A"})).unwrap();
    let r = s.execute("library.facets", &json!({"fields": ["camera", "keyword"]})).unwrap();
    let cams = values(&r, "camera");
    assert!(cams.iter().any(|c| c.0 == "Facet Cam B"), "the other camera is still offered: {cams:?}");
    assert_eq!(r["keyword"]["photos"], 2, "keywords of the two Facet Cam A photos");
    assert_eq!(values(&r, "keyword"), vec![("qq-one".to_string(), 2), ("qq-two".to_string(), 1)]);
}

/// `library.facetPick`: hostile arguments are errors that leave the filter alone, never a panic; then every kind of pick.
#[test]
fn picks_narrow_cascade_take_unknown_and_single_values_and_refuse_bad_ones() {
    let (mut s, ids) = library();
    for p in [
        json!({}),
        json!({"field": "shoe size", "value": "9"}),
        json!({"field": "camera", "value": 3}),
        json!({"field": "camera", "value": "  "}),
        json!({"field": "rating", "value": "9"}),
        json!({"field": "rating", "none": true}),
        json!({"field": "camera", "none": "yes"}),
        json!({"field": "rating", "value": "-1"}),
        json!({"field": "flag", "value": "maybe"}),
        json!({"field": "kind", "value": "hologram"}),
        json!({"field": "year", "value": "20x6"}),
        json!({"field": "year", "value": "é202"}),
        json!({"field": "label", "value": "Ultraviolet"}),
    ] {
        assert!(s.execute("library.facetPick", &p).is_err(), "{p}");
        assert_eq!(s.filter, lcc_catalog::Filter::default(), "{p} left the filter alone");
    }

    // Issue #218: a Metadata column's click (`library.facetPick`) narrows the view; the next column counts only what
    // is left (the columns cascade), a field with several values chosen shows any of them, and the field picked keeps
    // listing its other values.
    let r = s.execute("library.facetPick", &json!({"field": "camera", "value": "Facet Cam A"})).unwrap();
    assert_eq!(r["count"], 2, "{r}");
    assert_eq!(r["chosen"], json!(["Facet Cam A"]));
    let f = s.execute("library.facets", &json!({"fields": ["camera", "keyword"]})).unwrap();
    assert_eq!(f["camera"]["chosen"], json!(["Facet Cam A"]));
    assert!(values(&f, "camera").iter().any(|c| c.0 == "Facet Cam B"), "the camera column keeps its other values");
    assert_eq!(values(&f, "keyword"), vec![("qq-one".to_string(), 2), ("qq-two".to_string(), 1)], "the keyword column cascades");
    // a second camera: either matches
    let r = s.execute("library.facetPick", &json!({"field": "camera", "value": "Facet Cam B"})).unwrap();
    assert_eq!(r["count"], 3);
    // the next column narrows further
    let r = s.execute("library.facetPick", &json!({"field": "keyword", "value": "qq-two"})).unwrap();
    assert_eq!(r["count"], 2);
    let mut shown = s.visible().to_vec();
    shown.sort();
    let mut want = vec![ids[1], ids[2]];
    want.sort();
    assert_eq!(shown, want);
    // a click on a chosen value takes it off (any case); no value clears the field
    let r = s.execute("library.facetPick", &json!({"field": "camera", "value": "facet cam b"})).unwrap();
    assert_eq!(r["chosen"], json!(["Facet Cam A"]));
    s.execute("library.facetPick", &json!({"field": "camera"})).unwrap();
    s.execute("library.facetPick", &json!({"field": "keyword", "value": null})).unwrap();
    assert_eq!(s.filter, lcc_catalog::Filter::default(), "nothing left of the picks");

    // Photos without a value can be picked (issue #777): unrated photos, photos without a keyword or camera.
    let shown = s.visible().len();
    // unrated
    s.execute("library.facetPick", &json!({"field": "rating", "value": "0"})).unwrap();
    let v = s.visible().to_vec();
    assert!(v.iter().all(|id| s.catalog.photo(*id).unwrap().rating == 0));
    s.execute("library.facetPick", &json!({"field": "rating", "value": null})).unwrap();
    // no keyword
    let r = s.execute("library.facetPick", &json!({"field": "keyword", "none": true})).unwrap();
    assert_eq!(r["none"], true);
    let without: Vec<_> = s.visible().to_vec();
    assert!(without.iter().all(|id| s.catalog.photo(*id).unwrap().meta.keywords.is_empty()));
    assert!(!without.iter().any(|id| ids.contains(id)), "the photos with keywords are left out");
    let f = s.execute("library.facets", &json!({"fields": ["keyword"]})).unwrap();
    assert_eq!(f["keyword"]["noneChosen"], true);
    // a value as well: those photos, or the ones without
    s.execute("library.facetPick", &json!({"field": "keyword", "value": "qq-two"})).unwrap();
    assert_eq!(s.visible().len(), without.len() + 2);
    // clearing the column drops both
    s.execute("library.facetPick", &json!({"field": "keyword", "value": null})).unwrap();
    assert_eq!(s.visible().len(), shown);
    assert_eq!(s.filter, lcc_catalog::Filter::default());

    // Single-valued fields (flag, rating, kind, year) hold the last value clicked; a second click clears it.
    s.execute("photo.rate", &json!({"ids": [ids[0].0], "rating": 4})).unwrap();
    s.execute("library.facetPick", &json!({"field": "rating", "value": "4"})).unwrap();
    assert!(!s.visible().is_empty());
    let shown = s.visible().to_vec();
    assert!(shown.iter().all(|id| s.catalog.photo(*id).unwrap().rating == 4));
    assert_eq!(s.execute("library.facets", &json!({"fields": ["rating"]})).unwrap()["rating"]["chosen"], json!(["4"]));
    s.execute("library.facetPick", &json!({"field": "rating", "value": "4"})).unwrap();
    assert_eq!(s.filter, lcc_catalog::Filter::default(), "cleared");
    for (field, value) in [("flag", "pick"), ("kind", "image"), ("year", "2026")] {
        s.execute("library.facetPick", &json!({"field": field, "value": value})).unwrap();
        let f = s.execute("library.facets", &json!({"fields": [field]})).unwrap();
        assert_eq!(f[field]["chosen"], json!([value]), "{field}");
        s.execute("library.facetPick", &json!({"field": field, "value": value})).unwrap();
    }
    assert_eq!(s.filter, lcc_catalog::Filter::default());

    // The fields without a filter field of their own (issue #777): file format, IPTC text, aspect ratio.
    let mut m = s.catalog.photo(ids[0]).unwrap().meta.clone();
    m.headline = "Facet Headline".into();
    s.catalog.apply(Op::SetMeta { id: ids[0], meta: Box::new(m) }).unwrap();
    let r = s.execute("library.facets", &json!({"fields": ["format", "headline", "aspect", "preset", "ai"]})).unwrap();
    assert!(!values(&r, "format").is_empty(), "every demo photo has a format");
    assert!(!values(&r, "aspect").is_empty(), "every demo photo has a size");
    assert_eq!(values(&r, "headline"), vec![("Facet Headline".to_string(), 1)]);
    s.execute("library.facetPick", &json!({"field": "headline", "value": "facet headline"})).unwrap();
    assert_eq!(s.visible().to_vec(), vec![ids[0]]);
    s.execute("library.facetPick", &json!({"field": "headline", "value": "Facet Headline"})).unwrap();
    s.execute("library.facetPick", &json!({"field": "headline", "none": true})).unwrap();
    assert!(!s.visible().contains(&ids[0]));
    let format = values(&r, "format")[0].0.clone();
    s.execute("library.facetPick", &json!({"field": "headline", "value": null})).unwrap();
    s.execute("library.facetPick", &json!({"field": "format", "value": format})).unwrap();
    let v = s.visible().to_vec();
    assert!(!v.is_empty() && v.iter().all(|id| s.catalog.photo(*id).unwrap().format.eq_ignore_ascii_case(&format)));
}
