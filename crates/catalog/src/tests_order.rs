//! The kept sort orders ([`crate::order`]) equal a fresh full sort after every op that can move a photo in
//! one, its inverse, and a batch that fails and is rolled back, for every sort key both ways, so a query
//! that picks from them gives what sorting its matches gives.

use proptest::prelude::*;

use super::*;

const PHOTOS: u64 = 9;

const KEYS: [SortKey; 6] = [SortKey::CaptureDate, SortKey::ImportDate, SortKey::EditDate, SortKey::FileName, SortKey::Rating, SortKey::FileSize];

fn sorts() -> impl Iterator<Item = Sort> {
    KEYS.into_iter().flat_map(|key| [true, false].map(|ascending| Sort { key, ascending, ..Sort::default() }))
}

/// Many ties on every key, so the tie-breaks matter: few names (two only in another case), few dates.
fn photo(i: u64) -> Photo {
    let names = ["IMG_1.jpg", "img_1.JPG", "IMG_2.jpg", "Été.jpg"];
    let mut p =
        Photo::new(PhotoId(i), Source::Demo { scene: 1 }, names[(i % 4) as usize], "JPEG", 60, 40, &format!("2026-01-0{}T00:00:00", 1 + i % 3));
    p.captured = i.is_multiple_of(2).then(|| format!("2025-0{}-01T00:00:00", 1 + i % 2));
    p.rating = (i % 3) as u8;
    p.file_size = i % 2;
    p.edited = i.is_multiple_of(4).then(|| "2026-02-01T00:00:00".to_string());
    p
}

fn library() -> Catalog {
    let mut c = Catalog::new();
    for i in 1..=PHOTOS {
        c.apply(Op::AddPhoto { photo: Box::new(photo(i)) }).unwrap();
    }
    c
}

/// The kept order of each sort, against a fresh sort of every photo.
fn agree(c: &Catalog) -> std::result::Result<(), TestCaseError> {
    for sort in sorts() {
        prop_assert_eq!(c.kept_ids(&sort), Some(c.sorted_from_scratch(&sort)), "{:?}", sort);
    }
    Ok(())
}

fn op(kind: u8, n: u64, v: u8) -> Op {
    let id = PhotoId(n % (PHOTOS + 2) + 1);
    match kind % 12 {
        0 => Op::AddPhoto { photo: Box::new(photo(n % (PHOTOS + 4) + 1 + u64::from(v))) },
        1 => Op::RemovePhoto { id },
        2 => Op::SetRating { id, rating: v % 6 },
        3 => Op::SetCaptured { id, captured: (!v.is_multiple_of(3)).then(|| format!("2025-0{}-0{}T10:00:00", 1 + v % 3, 1 + v % 2)) },
        4 => Op::SetFile { id, file_name: ["IMG_1.jpg", "zzz.jpg", "AAA.jpg"][(v % 3) as usize].into(), source: Source::Demo { scene: 1 } },
        5 => Op::Relink { id, file_name: ["IMG_2.jpg", "b.jpg"][(v % 2) as usize].into(), source: Source::Demo { scene: 2 }, format: None },
        6 => Op::SetContent {
            id,
            width: 1,
            height: 1,
            file_size: u64::from(v % 3),
            content_hash: None,
            preview_only: None,
            preview_checked: String::new(),
        },
        7 => Op::SetDevelop {
            id,
            settings: Default::default(),
            label: String::new(),
            edited: v.is_multiple_of(2).then(|| format!("2026-02-0{}T00:00:00", 1 + v % 3)),
        },
        8 => Op::SetDeleted { id, deleted: v.is_multiple_of(2) },
        // an edit no order reads
        9 => Op::SetFlag { id, flag: Flag::Pick },
        // what is applied is taken back: the last op fails (no such photo) half the time
        10 => Op::Batch {
            ops: vec![
                Op::SetRating { id, rating: v % 6 },
                Op::SetFile { id: PhotoId(n % PHOTOS + 1), file_name: "q.jpg".into(), source: Source::Demo { scene: 1 } },
                Op::SetCaptured { id, captured: Some(if v.is_multiple_of(2) { "not a date".into() } else { "2024-05-05T05:05:05".into() }) },
            ],
        },
        _ => Op::SetAlbumPhotos { id: AlbumId(1), photos: vec![id] },
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn kept_orders_equal_a_fresh_sort(steps in proptest::collection::vec((0u8..12, 0u64..16, 0u8..255, 0u8..3), 1..30)) {
        let mut c = library();
        // built before the ops, so every op has to be followed
        agree(&c)?;
        let mut inverses = Vec::new();
        for (kind, n, v, asks) in steps {
            if let Ok(inv) = c.apply(op(kind, n, v)) {
                inverses.push(inv);
            }
            // (asked after some ops and not others, so several are followed at once)
            if asks != 0 {
                agree(&c)?;
            }
        }
        agree(&c)?;
        for inv in inverses.into_iter().rev() {
            c.apply(inv).unwrap();
            agree(&c)?;
        }
        // a clone and a snapshot make their own
        agree(&c.clone())?;
        agree(&Catalog::from_snapshot(&c.to_snapshot()).unwrap())?;
    }
}

#[test]
fn a_query_in_every_order_equals_sorting_its_matches() {
    let mut c = library();
    let filter = Filter { rating: 1, ..Filter::default() };
    for sort in sorts() {
        c.kept_ids(&sort);
    }
    c.apply(Op::SetRating { id: PhotoId(3), rating: 4 }).unwrap();
    c.apply(Op::SetFile { id: PhotoId(5), file_name: "AAA.jpg".into(), source: Source::Demo { scene: 1 } }).unwrap();
    for sort in sorts() {
        let mut want: Vec<&Photo> = c.photos().map(|p| p.as_ref()).filter(|p| filter.matches(p, &c)).collect();
        want.sort_by(|a, b| sort.order(a, b));
        assert_eq!(c.query(&filter, &sort), want.iter().map(|p| p.id).collect::<Vec<_>>(), "{sort:?}");
    }
}

/// The changes remembered are limited: after more than that, the order is made again, and is still right.
#[test]
fn an_order_is_made_again_when_the_changes_are_no_longer_remembered() {
    let mut c = library();
    let sort = Sort { key: SortKey::Rating, ascending: false, ..Sort::default() };
    c.kept_ids(&sort);
    for i in 0..9000u64 {
        c.apply(Op::SetRating { id: PhotoId(i % PHOTOS + 1), rating: (i % 6) as u8 }).unwrap();
    }
    assert_eq!(c.kept_ids(&sort), Some(c.sorted_from_scratch(&sort)));
}

/// A query of a small share of the library (a folder, a search) on a key with no kept order sorts its own matches:
/// sorting the whole library for it would cost far more than the query.
#[test]
fn a_narrow_query_does_not_build_an_order_for_the_whole_library() {
    let mut c = Catalog::new();
    for i in 1..=400 {
        let mut p = photo(i);
        p.rating = if i % 100 == 0 { 5 } else { u8::from(i % 4 == 1) };
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let sort = Sort { key: SortKey::FileName, ascending: false, ..Sort::default() };
    let sorted = |f: &Filter| {
        let mut want: Vec<&Photo> = c.photos().map(|p| p.as_ref()).filter(|p| f.matches(p, &c)).collect();
        want.sort_by(|a, b| sort.order(a, b));
        want.iter().map(|p| p.id).collect::<Vec<_>>()
    };
    let few = Filter { rating: 5, rating_op: RatingOp::Exactly, ..Filter::default() };
    assert_eq!(c.query(&few, &sort), sorted(&few));
    assert_eq!(c.query(&few, &sort), sorted(&few));
    assert_eq!(c.index.orders.count(), 0, "no order for 4 of 400 photos, however often asked");
    // about a quarter: sorted alone the first time, an order is made when the key is asked again
    let third = Filter { rating: 1, rating_op: RatingOp::Exactly, ..Filter::default() };
    assert_eq!(c.query(&third, &sort), sorted(&third));
    assert_eq!(c.index.orders.count(), 0);
    assert_eq!(c.query(&third, &sort), sorted(&third));
    assert_eq!(c.index.orders.count(), 1);
}
