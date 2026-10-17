//! Custom Order: `album.orderPhotos`, `album.setPhotoOrder` and `library.sort {key: custom}` —
//! one undo step each, kept across reopening the library, and followed by Rename `{seq}`.

use lcc_catalog::{AlbumId, PhotoId, SortKey};
use serde_json::{Value, json};

use crate::{Selection, Session};

/// A demo session showing a new album with the first five demo photos.
fn album(s: &mut Session) -> (u64, Vec<u64>) {
    let ids: Vec<u64> = s.visible_cloned().iter().take(5).map(|p| p.0).collect();
    let a = s.execute("album.create", &json!({"name": "Story"})).unwrap()["id"].as_u64().unwrap();
    s.selection = Selection::single(PhotoId(ids[0]));
    s.execute("album.addPhotos", &json!({"id": a, "ids": ids})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    (a, ids)
}

fn shown(s: &mut Session) -> Vec<u64> {
    s.visible_cloned().iter().map(|p| p.0).collect()
}

fn order(r: &Value) -> Vec<u64> {
    r["order"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect()
}

#[test]
fn switching_to_custom_starts_from_the_shown_order() {
    let mut s = Session::with_demo();
    let (a, _) = album(&mut s);
    s.execute("library.sort", &json!({"key": "fileName", "ascending": false})).unwrap();
    let before = shown(&mut s);
    s.execute("library.sort", &json!({"key": "custom"})).unwrap();
    assert_eq!((s.sort.key, s.sort.ascending), (SortKey::Custom, true));
    assert_eq!(shown(&mut s), before);
    let saved: Vec<u64> = s.catalog.album(AlbumId(a)).unwrap().photo_order.iter().map(|p| p.0).collect();
    assert_eq!(saved, before);
}

#[test]
fn custom_needs_a_regular_album() {
    let mut s = Session::with_demo();
    assert!(s.execute("library.sort", &json!({"key": "custom"})).is_err(), "All Photos has no custom order");
    assert_eq!(s.sort.key, SortKey::CaptureDate);
    let (a, _) = album(&mut s);
    s.execute("library.sort", &json!({"key": "custom"})).unwrap();
    // elsewhere the same sort falls back to capture date
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    let all = shown(&mut s);
    s.execute("library.sort", &json!({"key": "captureDate", "ascending": true})).unwrap();
    assert_eq!(shown(&mut s), all);
    let smart = s.execute("album.createSmart", &json!({"name": "Any"})).unwrap()["id"].as_u64().unwrap();
    assert!(s.execute("album.setPhotoOrder", &json!({"album": smart, "ids": []})).is_err());
    assert!(s.execute("album.orderPhotos", &json!({"album": a + 1000, "ids": [1]})).is_err());
}

#[test]
fn order_photos_moves_before_a_photo_or_to_the_end_and_undoes() {
    let mut s = Session::with_demo();
    let (a, _) = album(&mut s);
    s.execute("library.sort", &json!({"key": "custom"})).unwrap();
    let o = shown(&mut s);
    // two photos (listed out of order, one twice) before the first: they keep their order
    let r = s.execute("album.orderPhotos", &json!({"ids": [o[4], o[2], o[4]], "before": o[0]})).unwrap();
    assert_eq!(order(&r), vec![o[2], o[4], o[0], o[1], o[3]]);
    assert_eq!(shown(&mut s), order(&r));
    let r = s.execute("album.orderPhotos", &json!({"album": a, "ids": [o[2]]})).unwrap();
    assert_eq!(order(&r), vec![o[4], o[0], o[1], o[3], o[2]], "no `before`: to the end");
    // before one of the moved photos: before the next one that stays
    let r = s.execute("album.orderPhotos", &json!({"ids": [o[0], o[1]], "before": o[1]})).unwrap();
    assert_eq!(order(&r), vec![o[4], o[0], o[1], o[3], o[2]]);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(shown(&mut s), vec![o[2], o[4], o[0], o[1], o[3]], "one undo step per move");
    // unknown ids are errors and change nothing
    let other = s.catalog.photos().map(|p| p.id.0).find(|id| !o.contains(id)).unwrap();
    assert!(s.execute("album.orderPhotos", &json!({"ids": [other]})).is_err());
    assert!(s.execute("album.orderPhotos", &json!({"ids": [o[0]], "before": other})).is_err());
    assert!(s.execute("album.orderPhotos", &json!({"ids": ["x"]})).is_err());
    assert!(s.execute("album.orderPhotos", &json!({"ids": []})).is_err());
    assert_eq!(shown(&mut s), vec![o[2], o[4], o[0], o[1], o[3]]);
}

#[test]
fn moving_in_another_sort_switches_to_custom() {
    let mut s = Session::with_demo();
    album(&mut s);
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
    let o = shown(&mut s);
    s.execute("album.orderPhotos", &json!({"ids": [o[0]]})).unwrap();
    assert_eq!(s.sort.key, SortKey::Custom);
    assert_eq!(shown(&mut s), vec![o[1], o[2], o[3], o[4], o[0]]);
}

#[test]
fn set_photo_order_lists_the_rest_after_and_persists() {
    let dir = std::env::temp_dir().join(format!("lc-custom-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Session::new();
    s.open_library(&dir, true).unwrap();
    let (a, ids) = album(&mut s);
    let r = s.execute("album.setPhotoOrder", &json!({"album": a, "ids": [ids[3], ids[1], ids[3]]})).unwrap();
    assert_eq!(order(&r), vec![ids[3], ids[1], ids[0], ids[2], ids[4]]);
    drop(s);
    let mut s = Session::new();
    s.open_library(&dir, false).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    s.execute("library.sort", &json!({"key": "custom"})).unwrap();
    assert_eq!(shown(&mut s), vec![ids[3], ids[1], ids[0], ids[2], ids[4]], "kept across reopening");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rename_numbers_the_selection_in_custom_order() {
    let mut s = Session::with_demo();
    let (a, ids) = album(&mut s);
    s.execute("album.setPhotoOrder", &json!({"album": a, "ids": [ids[4], ids[0]]})).unwrap();
    s.execute("library.sort", &json!({"key": "custom"})).unwrap();
    // selected out of grid order: numbered in grid order all the same
    s.selection = Selection { ids: vec![PhotoId(ids[0]), PhotoId(ids[4])], active: Some(PhotoId(ids[0])) };
    let plans = s.execute("photo.renamePreview", &json!({"template": "Story-{seq}"})).unwrap();
    let got: Vec<(u64, String)> =
        plans.as_array().unwrap().iter().map(|p| (p["id"].as_u64().unwrap(), p["to"].as_str().unwrap().to_string())).collect();
    assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), vec![ids[4], ids[0]]);
    assert!(got[0].1.starts_with("Story-1.") && got[1].1.starts_with("Story-2."), "{got:?}");
}

// Feature: several orders of the same photos — Duplicate Album keeps the Custom Order, and each
// copy is then arranged on its own (Lightroom Classic's way: one collection per order).
#[test]
fn duplicated_albums_keep_and_then_own_their_orders() {
    let mut s = Session::with_demo();
    let (a, ids) = album(&mut s);
    let rev: Vec<u64> = ids.iter().rev().copied().collect();
    s.execute("album.setPhotoOrder", &json!({"album": a, "ids": rev})).unwrap();
    let b = s.execute("album.duplicate", &json!({"id": a})).unwrap()["id"].as_u64().unwrap();
    let order_of = |s: &Session, id: u64| -> Vec<u64> { s.catalog.album_photo_order(AlbumId(id)).iter().map(|p| p.0).collect() };
    assert_eq!(order_of(&s, b), rev);
    assert_eq!(s.catalog.album(AlbumId(b)).unwrap().name, "Story copy");
    s.execute("album.setPhotoOrder", &json!({"album": b, "ids": ids})).unwrap();
    assert_eq!((order_of(&s, a), order_of(&s, b)), (rev.clone(), ids.clone()));
    s.execute("edit.undo", &json!({})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.album(AlbumId(b)).is_none(), "one undo step each");
    // not for folders, smart albums or unknown ids
    let f = s.execute("album.create", &json!({"name": "F", "folder": true})).unwrap()["id"].as_u64().unwrap();
    for id in [f, 999_999] {
        assert!(s.execute("album.duplicate", &json!({"id": id})).is_err());
    }
    assert!(s.execute("album.duplicate", &json!({})).is_err());
}

// Feature: a new album from selected photos starts in the order they are shown.
#[test]
fn new_album_from_selection_keeps_the_shown_order() {
    let mut s = Session::with_demo();
    let (a, ids) = album(&mut s);
    let rev: Vec<u64> = ids.iter().rev().copied().collect();
    s.execute("album.setPhotoOrder", &json!({"album": a, "ids": rev})).unwrap();
    s.execute("library.sort", &json!({"key": "custom"})).unwrap();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    let b = s.execute("album.create", &json!({"name": "Pick", "addSelected": true})).unwrap()["id"].as_u64().unwrap();
    let got: Vec<u64> = s.catalog.album_photo_order(AlbumId(b)).iter().map(|p| p.0).collect();
    assert_eq!(got, rev);
}
