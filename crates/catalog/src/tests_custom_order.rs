//! Custom Order: a regular album's photos in an order set by hand (`Album.photo_order`,
//! `Op::SetAlbumPhotoOrder`, `SortKey::Custom`).

use crate::*;

fn photo(c: &mut Catalog, captured: &str) -> PhotoId {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: format!("/p/{}.jpg", id.0) }, &format!("{}.jpg", id.0), "JPEG", 4, 3, "2026-01-01");
    p.captured = Some(captured.into());
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn setup() -> (Catalog, AlbumId, Vec<PhotoId>) {
    let mut c = Catalog::new();
    let ids: Vec<PhotoId> = (0..4).map(|i| photo(&mut c, &format!("2026-01-0{}T10:00:00", i + 1))).collect();
    let a = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { photos: ids.clone(), ..Album::new(a, "Story") } }).unwrap();
    (c, a, ids)
}

fn custom() -> Sort {
    Sort { key: SortKey::Custom, ascending: true, ..Default::default() }
}

#[test]
fn custom_order_sorts_the_album_and_undoes_as_one_step() {
    let (mut c, a, ids) = setup();
    let f = Filter { album: Some(a), ..Default::default() };
    assert_eq!(c.query(&f, &custom()), ids, "no order yet: the order photos were added");
    // repeats and non-members are dropped; the unlisted member follows
    let inv = c.apply(Op::SetAlbumPhotoOrder { id: a, order: vec![ids[2], ids[0], ids[2], PhotoId(999), ids[1]] }).unwrap();
    assert_eq!(c.album(a).unwrap().photo_order, vec![ids[2], ids[0], ids[1]]);
    assert_eq!(c.query(&f, &custom()), vec![ids[2], ids[0], ids[1], ids[3]]);
    assert_eq!(c.query(&f, &Sort { ascending: false, ..custom() }), vec![ids[3], ids[1], ids[0], ids[2]]);
    c.apply(inv).unwrap();
    assert!(c.album(a).unwrap().photo_order.is_empty());
}

#[test]
fn custom_falls_back_to_capture_date_outside_a_regular_album() {
    let (mut c, a, ids) = setup();
    c.apply(Op::SetAlbumPhotoOrder { id: a, order: vec![ids[3], ids[1]] }).unwrap();
    let all = c.query(&Filter::default(), &custom());
    assert_eq!(all, c.query(&Filter::default(), &Sort { key: SortKey::CaptureDate, ascending: true, ..Default::default() }));
    let s = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { smart: Some(Box::new(Filter::default())), ..Album::new(s, "Smart") } }).unwrap();
    assert!(c.apply(Op::SetAlbumPhotoOrder { id: s, order: vec![ids[0]] }).is_err(), "smart albums have no custom order");
    assert!(c.album_photo_order(s).is_empty());
    assert!(c.apply(Op::SetAlbumPhotoOrder { id: AlbumId(77), order: vec![] }).is_err(), "no such album");
}

#[test]
fn removed_photos_leave_the_order_and_come_back_at_the_end() {
    let (mut c, a, ids) = setup();
    c.apply(Op::SetAlbumPhotoOrder { id: a, order: vec![ids[3], ids[2], ids[1], ids[0]] }).unwrap();
    let without: Vec<PhotoId> = ids.iter().copied().filter(|p| *p != ids[2]).collect();
    let inv = c.apply(Op::SetAlbumPhotos { id: a, photos: without.clone() }).unwrap();
    assert_eq!(c.album(a).unwrap().photo_order, vec![ids[3], ids[1], ids[0]]);
    // undo puts the photo back in its place
    let redo = c.apply(inv).unwrap();
    assert_eq!(c.album_photo_order(a), vec![ids[3], ids[2], ids[1], ids[0]]);
    c.apply(redo).unwrap();
    c.apply(Op::SetAlbumPhotos { id: a, photos: ids.clone() }).unwrap();
    assert_eq!(c.album_photo_order(a), vec![ids[3], ids[1], ids[0], ids[2]], "added again: at the end");
}

#[test]
fn custom_order_survives_a_snapshot_and_journal_round_trip() {
    let (mut c, a, ids) = setup();
    c.apply(Op::SetAlbumPhotoOrder { id: a, order: vec![ids[1], ids[0]] }).unwrap();
    let back = Catalog::from_snapshot(&c.to_snapshot()).unwrap();
    assert_eq!(back.album(a).unwrap().photo_order, vec![ids[1], ids[0]]);
    let op = Op::SetAlbumPhotoOrder { id: a, order: vec![ids[3]] };
    let json = serde_json::to_string(&op).unwrap();
    assert_eq!(serde_json::from_str::<Op>(&json).unwrap(), op);
    // an album without one doesn't write the field (older builds read it unchanged)
    assert!(!Catalog::new().to_snapshot().contains("photo_order"));
}
