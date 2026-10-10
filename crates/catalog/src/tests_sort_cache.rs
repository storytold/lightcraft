//! The kept whole-catalog sort orders (`query::SortCache`) give exactly what a fresh sort gives,
//! whatever ops ran in between.

use std::sync::Arc;

use dac_develop::DevelopSettings;

use crate::*;

fn photo(c: &mut Catalog, i: u64) -> Photo {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(
        id,
        Source::File { path: format!("/pics/{}/IMG_{:05}.jpg", i % 7, (i * 7919) % 5000) },
        &format!("IMG_{:05}.jpg", (i * 7919) % 5000),
        "JPEG",
        10,
        10,
        &format!("2026-0{}-01T00:00:00", 1 + i % 3),
    );
    // many equal capture times: the tie-breaks matter
    p.captured = if i.is_multiple_of(11) { None } else { Some(format!("20{:02}-01-01T00:00:00", 10 + i % 5)) };
    p.rating = (i % 6) as u8;
    p.file_size = (i * 31) % 1000;
    p.deleted = i.is_multiple_of(13);
    p
}

fn sorts() -> Vec<Sort> {
    let keys = [SortKey::CaptureDate, SortKey::ImportDate, SortKey::EditDate, SortKey::FileName, SortKey::Rating, SortKey::FileSize, SortKey::Random];
    keys.iter().flat_map(|k| [true, false].map(|asc| Sort { key: *k, ascending: asc, seed: 7, ..Default::default() })).collect()
}

fn filters() -> Vec<Filter> {
    vec![Filter::default(), Filter { rating: 3, ..Default::default() }, Filter { text: "IMG_01".into(), ..Default::default() }]
}

fn check(c: &Catalog, what: &str) {
    for s in sorts() {
        for f in filters() {
            assert_eq!(c.query(&f, &s), c.query_with(&f, &s, false), "{what}: {s:?} {f:?}");
        }
    }
}

#[test]
fn kept_orders_match_a_fresh_sort_across_ops() {
    let mut c = Catalog::new();
    for i in 0..3000 {
        let p = photo(&mut c, i);
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    check(&c, "fresh");
    let ids: Vec<PhotoId> = c.photos().map(|p| p.id).collect();
    let edits: Vec<(&str, Op)> = vec![
        ("rating", Op::SetRating { id: ids[5], rating: 5 }),
        ("captured", Op::SetCaptured { id: ids[6], captured: Some("1999-01-01T00:00:00".into()) }),
        (
            "develop",
            Op::SetDevelop {
                id: ids[7],
                settings: Arc::new(DevelopSettings::default()),
                label: "x".into(),
                edited: Some("2030-01-01T00:00:00".into()),
            },
        ),
        ("file", Op::SetFile { id: ids[8], file_name: "aaa.jpg".into(), source: Source::File { path: "/pics/aaa.jpg".into() } }),
        ("content", Op::SetContent { id: ids[9], width: 1, height: 1, file_size: 999_999, content_hash: None, preview_only: None }),
        ("remove", Op::RemovePhoto { id: ids[10] }),
        ("deleted", Op::SetDeleted { id: ids[11], deleted: true }),
        ("batch", Op::Batch { ops: vec![Op::SetRating { id: ids[12], rating: 0 }, Op::SetCaptured { id: ids[13], captured: None }] }),
    ];
    for (what, op) in edits {
        let inv = c.apply(op).unwrap();
        check(&c, what);
        c.apply(inv).unwrap();
        check(&c, &format!("{what} undone"));
    }
    let p = photo(&mut c, 9999);
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    check(&c, "added");
    // a clone starts without kept orders and agrees too
    check(&c.clone(), "clone");
}
