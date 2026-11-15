//! The filter bar's stars and flags narrow the Catalog / All Photographs view ([`LibrarySource::All`]),
//! as they narrow a collection or a folder (issue #1191): what the grid shows is the catalog's own
//! query of the source's filter, whatever the source is.
//!
//! Scenarios:
//! - Given photos with different stars and flags in All Photographs, when a star rule (≥, =, ≤) or a
//!   flag rule (pick, reject, unflagged) is set, then the view is exactly the photos that match.
//! - Given a rule set while a collection is shown, when All Photographs is chosen again
//!   (Lock Filters on, the default), then the rule still narrows the view.
//! - Given rules set and then cleared the way the filter bar and the chips clear them, then the view
//!   is whole again.

use lcc_catalog::{Filter, Flag, Op, Photo, Source};
use serde_json::json;

use crate::{LibrarySource, Session};

fn library() -> (Session, Vec<lcc_catalog::PhotoId>) {
    let mut s = Session::new();
    let mut ids = Vec::new();
    for i in 0..12 {
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 1 }, &format!("p{i}.jpg"), "JPEG", 60, 40, "2026-02-01T10:00:00");
        p.rating = (i % 4) as u8;
        p.flag = match i % 3 {
            0 => Flag::Pick,
            1 => Flag::None,
            _ => Flag::Reject,
        };
        s.commit("Add Photo", Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    (s, ids)
}

/// The view the rule asks for, worked out from the catalog alone.
fn wanted(s: &Session, rating: u8, op: &str, flag: Option<Flag>) -> Vec<lcc_catalog::PhotoId> {
    let mut want: Vec<_> = s
        .catalog
        .photos()
        .filter(|p| match op {
            "exactly" => p.rating == rating,
            "atMost" => p.rating <= rating,
            _ => p.rating >= rating,
        })
        .filter(|p| flag.is_none_or(|f| p.flag == f))
        .map(|p| p.id)
        .collect();
    want.sort();
    want
}

fn show(s: &mut Session, rating: u8, op: &str, flag: Option<&str>) -> Vec<lcc_catalog::PhotoId> {
    let mut p = json!({"rating": rating, "ratingOp": op});
    match flag {
        Some(f) => p["flag"] = json!(f),
        None => p["flag"] = serde_json::Value::Null,
    }
    s.execute("library.filter", &p).unwrap();
    let mut shown = s.visible_cloned();
    shown.sort();
    shown
}

#[test]
fn star_and_flag_rules_narrow_all_photographs() {
    let (mut s, _) = library();
    assert_eq!(s.source, LibrarySource::All);
    assert_eq!(s.visible_cloned().len(), 12);
    let cases = [
        (2u8, "atLeast", None, None),
        (3, "exactly", None, None),
        (1, "atMost", None, None),
        (0, "atLeast", Some("pick"), Some(Flag::Pick)),
        (0, "atLeast", Some("reject"), Some(Flag::Reject)),
        (0, "atLeast", Some("none"), Some(Flag::None)),
    ];
    for (rating, op, flag, flag_value) in cases {
        let want = wanted(&s, rating, op, flag_value);
        let shown = show(&mut s, rating, op, flag);
        assert_eq!(shown, want, "rating {rating} {op}, flag {flag:?}");
        assert!(!shown.is_empty() && shown.len() < 12, "the rule narrows: {shown:?}");
    }
}

#[test]
fn a_filter_set_elsewhere_still_applies_back_in_all_photographs() {
    let (mut s, ids) = library();
    let album = s.execute("album.create", &json!({"name": "Trip"})).unwrap()["id"].as_u64().unwrap();
    s.execute("library.select", &json!({"ids": [ids[0].0, ids[4].0, ids[8].0]})).unwrap();
    s.execute("album.addPhotos", &json!({"id": album})).unwrap();
    // with a rule on, to the collection and back (Lock Filters on, the default)
    s.execute("library.filter", &json!({"rating": 2, "ratingOp": "atLeast", "flag": "pick"})).unwrap();
    let in_all = s.visible_cloned();
    s.execute("library.source", &json!({"kind": "album", "id": album})).unwrap();
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(s.source, LibrarySource::All);
    assert_eq!(s.visible_cloned(), in_all, "the rule still narrows All Photographs");
    assert_eq!(in_all, wanted(&s, 2, "atLeast", Some(Flag::Pick)));
}

#[test]
fn clearing_the_rules_the_way_the_bar_and_chips_do_shows_everything_again() {
    let (mut s, _) = library();
    s.execute("library.filter", &json!({"rating": 3, "ratingOp": "atLeast", "flag": "pick"})).unwrap();
    assert!(s.visible_cloned().len() < 12);
    // the bar's star click on its own star and the chip's clear: rating back to 0 / atLeast, flag null
    s.execute("library.filter", &json!({"rating": 0, "ratingOp": "atLeast", "flag": null})).unwrap();
    assert_eq!(s.visible_cloned().len(), 12);
    assert_eq!(s.filter, Filter::default());
}
