//! Note 50: a two-camera wedding sorted by Capture Time goes through the day across both bodies in
//! every view: a collection (filled one card after the other) and Recently Added (one import, one
//! import time for every photo) included. Custom Order keeps a collection's own order.

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::Session;

/// Camera 71's card first, then camera 74's, each in frame order, all from one import.
fn wedding() -> (Session, Vec<(PhotoId, &'static str)>) {
    let mut s = Session::new();
    let shots = [
        ("_71_8001.CR3", "2026-10-18T09:05:30.500+02:00"),
        ("_71_8500.CR3", "2026-10-18T15:00:01.000+02:00"),
        ("_71_9276.CR3", "2026-10-18T22:40:05.120+02:00"),
        ("_74_8160.CR3", "2026-10-18T09:02:00"),
        ("_74_9000.CR3", "2026-10-18T15:00:00"),
        ("_74_9901.CR3", "2026-10-18T23:10:00"),
    ];
    let mut ids = Vec::new();
    for (name, t) in shots {
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 1 }, name, "CR3", 600, 400, "2026-10-20T08:00:00");
        p.captured = Some(t.into());
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push((id, name));
    }
    (s, ids)
}

fn shown(s: &mut Session, ids: &[(PhotoId, &'static str)]) -> Vec<&'static str> {
    let names: std::collections::HashMap<PhotoId, &str> = ids.iter().copied().collect();
    s.visible_cloned().iter().filter_map(|id| names.get(id).copied()).collect()
}

const THROUGH_THE_DAY: [&str; 6] = ["_74_8160.CR3", "_71_8001.CR3", "_74_9000.CR3", "_71_8500.CR3", "_71_9276.CR3", "_74_9901.CR3"];

#[test]
fn every_view_sorted_by_capture_time_interleaves_the_cameras() {
    let (mut s, ids) = wedding();
    s.execute("library.sort", &json!({"key": "captureDate", "ascending": true})).unwrap();
    assert_eq!(shown(&mut s, &ids), THROUGH_THE_DAY, "All Photos");

    // Recently Added: the whole import shares one import time; inside it the sort chosen decides
    s.execute("library.source", &json!({"kind": "recentlyAdded"})).unwrap();
    assert_eq!(shown(&mut s, &ids), THROUGH_THE_DAY, "Recently Added");
    s.execute("library.sort", &json!({"key": "fileName"})).unwrap();
    let by_name: Vec<&str> = {
        let mut v: Vec<&str> = ids.iter().map(|(_, n)| *n).collect();
        v.sort();
        v
    };
    assert_eq!(shown(&mut s, &ids), by_name, "Recently Added by file name");

    // a collection filled card by card
    let album = s.execute("album.create", &json!({"name": "Wedding"})).unwrap()["id"].as_u64().unwrap();
    let all: Vec<u64> = ids.iter().map(|(id, _)| id.0).collect();
    s.execute("album.addPhotos", &json!({"id": album, "ids": all})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": album})).unwrap();
    s.execute("library.sort", &json!({"key": "captureDate", "ascending": true})).unwrap();
    assert_eq!(shown(&mut s, &ids), THROUGH_THE_DAY, "a collection sorted by capture time");
    // its own order is still there, as Custom Order
    s.execute("library.sort", &json!({"key": "custom", "ascending": true})).unwrap();
    let added: Vec<&str> = ids.iter().map(|(_, n)| *n).collect();
    assert_eq!(shown(&mut s, &ids), added, "Custom Order: the order photos were added in");
    s.execute("library.sort", &json!({"ascending": false})).unwrap();
    assert_eq!(shown(&mut s, &ids), added.iter().rev().copied().collect::<Vec<_>>());
}

/// A sort saved by an older build (`captureDate`) still reads as Capture Time.
#[test]
fn saved_capture_date_sorts_still_read() {
    let sort: lightcraft_catalog::Sort = serde_json::from_value(json!({"key": "captureDate", "ascending": true})).unwrap();
    assert_eq!(sort.key, lightcraft_catalog::SortKey::CaptureDate);
    assert_eq!(serde_json::to_value(lightcraft_catalog::SortKey::Custom).unwrap(), json!("custom"));
}
