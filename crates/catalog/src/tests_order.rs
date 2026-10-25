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
    library_of(PHOTOS)
}

fn library_of(n: u64) -> Catalog {
    let mut c = Catalog::new();
    for i in 1..=n {
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

/// Each key follows only what its comparison reads: a single op of each kind, on every photo, with several
/// values, moves the photo in every order that reads what it changed (and leaves the others alone).
#[test]
fn every_kind_of_edit_alone_leaves_every_order_right() {
    for kind in 0..12 {
        for n in 0..PHOTOS + 2 {
            for v in 0..6 {
                let mut c = library();
                agree(&c).unwrap();
                if c.apply(op(kind, n, v)).is_ok() {
                    agree(&c).unwrap_or_else(|e| panic!("kind {kind} photo {n} value {v}: {e}"));
                }
            }
        }
    }
}

/// More photos touched than are moved in the list one by one: they are put back together.
#[test]
fn many_edits_at_once_leave_every_order_right() {
    let mut c = library_of(80);
    agree(&c).unwrap();
    for i in 1..=40u64 {
        c.apply(Op::SetRating { id: PhotoId(i * 2), rating: (i % 6) as u8 }).unwrap();
        c.apply(Op::SetCaptured { id: PhotoId(i), captured: Some(format!("2024-0{}-0{}T01:00:00", 1 + i % 9, 1 + i % 7)) }).unwrap();
    }
    agree(&c).unwrap();
    c.apply(Op::RemovePhoto { id: PhotoId(7) }).unwrap();
    c.apply(Op::SetRating { id: PhotoId(9), rating: 5 }).unwrap();
    agree(&c).unwrap();
}

/// Once the changes are no longer remembered, a small query drops the kept order and sorts its own matches, where a
/// big one makes the order again.
#[test]
fn a_small_query_does_not_remake_an_order_after_forgotten_changes() {
    let mut c = library_of(400);
    for (i, r) in [(100, 5u8), (200, 5)] {
        c.apply(Op::SetRating { id: PhotoId(i), rating: r }).unwrap();
    }
    let sort = Sort { key: SortKey::Rating, ascending: false, ..Sort::default() };
    let few = Filter { rating: 5, rating_op: RatingOp::Exactly, ..Filter::default() };
    c.kept_ids(&sort);
    assert_eq!(c.index.orders.count(), 1);
    for i in 0..9000u64 {
        c.apply(Op::SetRating { id: PhotoId(i % 400 + 1), rating: (i % 4) as u8 }).unwrap();
    }
    for i in [100, 200] {
        c.apply(Op::SetRating { id: PhotoId(i), rating: 5 }).unwrap();
    }
    let mut want: Vec<&Photo> = c.photos().map(|p| p.as_ref()).filter(|p| few.matches(p, &c)).collect();
    want.sort_by(|a, b| sort.order(a, b));
    assert_eq!(c.query(&few, &sort), want.iter().map(|p| p.id).collect::<Vec<_>>());
    assert_eq!(c.index.orders.count(), 0);
}

/// A library just loaded remembers no changes (its revision was set from outside): with no op since, a kept order
/// is still right, so alternating a big view and a small one builds it once.
#[test]
fn a_loaded_library_builds_an_order_once() {
    let mut loaded = Catalog::from_snapshot(&library_of(400).to_snapshot()).unwrap();
    // (a loaded library takes a revision of its own, which is not the one its changes are remembered at)
    loaded.revision = 1_000_000;
    let sort = Sort { key: SortKey::FileName, ascending: true, ..Sort::default() };
    // (a few photos: sorted alone, never a reason to drop the order)
    let few = Filter { rating: 5, rating_op: RatingOp::Exactly, ..Filter::default() };
    let all = Filter::default();
    let first = loaded.query(&all, &sort);
    let kept = loaded.index.orders.held(sort.key).unwrap();
    loaded.query(&few, &sort);
    assert_eq!(loaded.query(&all, &sort), first);
    let again = loaded.index.orders.held(sort.key).unwrap();
    assert!(Arc::ptr_eq(&kept, &again), "the order was made again");
}

/// How many times every photo was sorted from scratch for `$c`'s orders while `$body` ran.
macro_rules! fresh_sorts {
    ($c:expr, $body:expr) => {{
        let before = $c.full_sorts();
        $body;
        $c.full_sorts() - before
    }};
}

/// Photos added (keys before, among and after the ones there, and tied with them) and removed, in one batch or many:
/// every order follows without sorting every photo again, and equals a fresh sort.
#[test]
fn photos_added_and_removed_leave_every_order_right_without_a_full_sort() {
    let mut c = library_of(300);
    agree(&c).unwrap();
    let new = |i: u64| {
        let mut p = photo(i);
        // before (2024), among (2025-0x) and after (2027) the dates there; names that tie with the others'
        p.captured = match i % 3 {
            0 => Some("2024-12-31T00:00:00".into()),
            1 => Some("2025-01-01T00:00:00".into()),
            _ => Some("2027-01-01T00:00:00".into()),
        };
        p.imported = format!("2026-01-0{}T00:00:00", 1 + i % 5);
        p.rating = (i % 6) as u8;
        p.file_size = i % 5;
        p
    };
    let n = fresh_sorts!(c, {
        c.apply(Op::Batch { ops: (301..=340).map(|i| Op::AddPhoto { photo: Box::new(new(i)) }).collect() }).unwrap();
        agree(&c).unwrap_or_else(|e| panic!("{e}"));
        // several imports in a row, each followed by a query
        for i in 341..=360 {
            c.apply(Op::AddPhoto { photo: Box::new(new(i)) }).unwrap();
            if i % 7 == 0 {
                agree(&c).unwrap_or_else(|e| panic!("{e}"));
            }
        }
        // an import undone, and some photos removed
        c.apply(Op::Batch { ops: (301..=340).map(|i| Op::RemovePhoto { id: PhotoId(i) }).collect() }).unwrap();
        c.apply(Op::Batch { ops: (1..=30).map(|i| Op::RemovePhoto { id: PhotoId(i * 3) }).collect() }).unwrap();
        agree(&c).unwrap_or_else(|e| panic!("{e}"));
    });
    // (`agree` makes a fresh sort per sort to compare with: 12 each time, five times)
    assert_eq!(n, 12 * 5, "only the sorts the test itself makes: {n}");
}

/// What changed is no longer remembered (more ops than the memory holds, between two queries): the photos that
/// came or went are found by their ids and the order is sorted again from where it was, not from every photo.
#[test]
fn an_order_follows_added_photos_when_the_changes_are_no_longer_remembered() {
    let mut c = library_of(300);
    agree(&c).unwrap();
    // (more edits than the memory holds, of the photos' ratings among others)
    for i in 0..9000u64 {
        c.apply(Op::SetRating { id: PhotoId(i % 300 + 1), rating: (i % 6) as u8 }).unwrap();
    }
    c.apply(Op::Batch { ops: (301..=340).map(|i| Op::AddPhoto { photo: Box::new(photo(i)) }).collect() }).unwrap();
    c.apply(Op::Batch { ops: (1..=20).map(|i| Op::RemovePhoto { id: PhotoId(i * 5) }).collect() }).unwrap();
    for sort in sorts() {
        let got;
        let n = fresh_sorts!(c, got = c.kept_ids(&sort));
        assert_eq!(n, 0, "{sort:?} was sorted from every photo");
        assert_eq!(got, Some(c.sorted_from_scratch(&sort)), "{sort:?}");
    }
}

/// An edit of a sort key still moves its photo: with the changes remembered, and with them forgotten (where the
/// photo is found by sorting, not by being named).
#[test]
fn an_edited_sort_key_is_still_sorted_again() {
    let mut c = library_of(300);
    agree(&c).unwrap();
    c.apply(Op::SetRating { id: PhotoId(4), rating: 5 }).unwrap();
    c.apply(Op::SetCaptured { id: PhotoId(9), captured: Some("2030-01-01T00:00:00".into()) }).unwrap();
    c.apply(Op::SetFile { id: PhotoId(12), file_name: "zzz.jpg".into(), source: Source::Demo { scene: 1 } }).unwrap();
    agree(&c).unwrap();
    for i in 0..9000u64 {
        c.apply(Op::SetRating { id: PhotoId(i % 300 + 1), rating: (i % 6) as u8 }).unwrap();
    }
    c.apply(Op::SetRating { id: PhotoId(77), rating: 5 }).unwrap();
    c.apply(Op::SetCaptured { id: PhotoId(78), captured: Some("2000-01-01T00:00:00".into()) }).unwrap();
    c.apply(Op::SetFile { id: PhotoId(79), file_name: "AAA.jpg".into(), source: Source::Demo { scene: 1 } }).unwrap();
    agree(&c).unwrap();
}

/// A library big enough for its first sort to be made on another thread.
fn big() -> Catalog {
    let mut c = Catalog::new();
    c.apply(Op::Batch { ops: (1..=50_000).map(|i| Op::AddPhoto { photo: Box::new(photo(i)) }).collect() }).unwrap();
    c
}

fn sort_of(key: SortKey) -> Sort {
    Sort { key, ascending: false, ..Sort::default() }
}

/// An order made on another thread equals a fresh sort, and the first query for it sorts nothing.
#[test]
fn an_order_made_on_another_thread_equals_a_fresh_sort() {
    let c = big();
    for key in KEYS {
        let sort = sort_of(key);
        c.prebuild_order(&sort);
        assert!(c.index.orders.pending(key), "{key:?}: not started");
        c.index.orders.wait_built(key);
        let got;
        assert_eq!(fresh_sorts!(c, got = c.kept_ids(&sort)), 0, "{key:?}: sorted every photo");
        assert_eq!(got, Some(c.sorted_from_scratch(&sort)), "{key:?}");
        assert!(!c.index.orders.pending(key));
    }
}

/// What changed while it was being made (photos added and removed, keys edited, more ops than are remembered) is
/// brought in when it is used: the order is right for the catalog as it is then.
#[test]
fn an_order_made_for_an_older_revision_is_brought_up_to_date() {
    for many in [false, true] {
        let mut c = big();
        let sorts = [sort_of(SortKey::CaptureDate), sort_of(SortKey::Rating), sort_of(SortKey::FileName)];
        for s in &sorts {
            c.prebuild_order(s);
        }
        c.apply(Op::Batch { ops: (50_001..=50_040).map(|i| Op::AddPhoto { photo: Box::new(photo(i)) }).collect() }).unwrap();
        c.apply(Op::Batch { ops: (1..=20).map(|i| Op::RemovePhoto { id: PhotoId(i * 7) }).collect() }).unwrap();
        c.apply(Op::SetRating { id: PhotoId(9), rating: 5 }).unwrap();
        c.apply(Op::SetCaptured { id: PhotoId(10), captured: Some("2031-01-01T00:00:00".into()) }).unwrap();
        c.apply(Op::SetFile { id: PhotoId(11), file_name: "AAA.jpg".into(), source: Source::Demo { scene: 1 } }).unwrap();
        if many {
            for i in 0..9000u64 {
                c.apply(Op::SetRating { id: PhotoId(i % 300 + 200), rating: (i % 6) as u8 }).unwrap();
            }
        }
        for s in &sorts {
            c.index.orders.wait_built(s.key);
            let got;
            assert_eq!(fresh_sorts!(c, got = c.kept_ids(s)), 0, "{s:?}: sorted every photo");
            assert_eq!(got, Some(c.sorted_from_scratch(s)), "{s:?} (many: {many})");
        }
    }
}

/// A result is for the catalog that asked, in the key it asked: a copy, another key, a library that was forgotten or
/// dropped, and a library too small for a thread get nothing of it.
#[test]
fn an_order_made_on_another_thread_reaches_only_the_catalog_and_key_it_was_made_for() {
    let c = big();
    let sort = sort_of(SortKey::Rating);
    c.prebuild_order(&sort);
    c.prebuild_order(&sort);
    assert_eq!(c.index.orders.count(), 0);
    let copy = c.clone();
    assert!(!copy.index.orders.pending(SortKey::Rating), "a copy starts without");
    c.index.orders.wait_built(SortKey::Rating);
    // another key sorts for itself
    let other = sort_of(SortKey::FileSize);
    let got;
    assert_eq!(fresh_sorts!(c, got = c.kept_ids(&other)), 1);
    assert_eq!(got, Some(c.sorted_from_scratch(&other)));
    // forgotten before it was used
    let mut c = c;
    c.index.orders.forget();
    assert!(!c.index.orders.pending(SortKey::Rating));
    assert_eq!(fresh_sorts!(c, drop(c.kept_ids(&sort))), 1, "the result of a forgotten order was used");
    // dropped while being made
    let gone = big();
    gone.prebuild_order(&sort);
    drop(gone);
    // too small, and a shuffle
    let small = library_of(400);
    small.prebuild_order(&sort);
    c.prebuild_order(&Sort { key: SortKey::Random, ..Sort::default() });
    assert!(!small.index.orders.pending(SortKey::Rating));
    assert!(!c.index.orders.pending(SortKey::Random));
}

/// A big view that sorts for itself first leaves nothing of an order being made for it: the worker is overtaken and
/// its result dropped, not held.
#[test]
fn a_query_that_sorts_for_itself_drops_the_order_being_made() {
    let c = big();
    let sort = sort_of(SortKey::CaptureDate);
    c.prebuild_order(&sort);
    assert!(c.index.orders.pending(sort.key));
    // (asked at once: the worker is not done, or if it is, its result is used; either way nothing is left)
    assert_eq!(c.kept_ids(&sort), Some(c.sorted_from_scratch(&sort)));
    assert!(!c.index.orders.pending(sort.key), "an order is still being held");
    assert_eq!(c.index.orders.count(), 1);
}

/// What the order's thread holds (the engine's activity row) is made on the calling thread, only when a thread is
/// started, and is dropped by the thread once the order is ready: not before the sort, and not never.
#[test]
fn the_thread_making_an_order_holds_what_it_was_given_until_the_order_is_ready() {
    use std::sync::mpsc::{Sender, channel};
    use std::thread::ThreadId;
    /// Says on which thread it was made and on which it was dropped.
    struct Row(ThreadId, Sender<(ThreadId, ThreadId)>);
    impl Drop for Row {
        fn drop(&mut self) {
            let _ = self.1.send((self.0, std::thread::current().id()));
        }
    }
    let (tx, rx) = channel();
    let row = || Row(std::thread::current().id(), tx.clone());

    // nothing is started (too small, a shuffle, an order made or being made): nothing is held
    library_of(400).prebuild_order_holding(&sort_of(SortKey::Rating), row);
    let c = big();
    c.prebuild_order_holding(&Sort { key: SortKey::Random, ..Sort::default() }, row);
    assert!(rx.try_recv().is_err(), "a row was made where no thread starts");

    let sort = sort_of(SortKey::Rating);
    c.prebuild_order_holding(&sort, row);
    c.prebuild_order_holding(&sort, row);
    let (made_on, dropped_on) = rx.recv_timeout(std::time::Duration::from_secs(60)).expect("the row is dropped when the order is made");
    assert_eq!(made_on, std::thread::current().id(), "made on the thread that asked");
    assert_ne!(dropped_on, made_on, "dropped by the thread making the order");
    assert!(c.order_ready(sort.key), "dropped before the order was handed over");
    assert!(rx.try_recv().is_err(), "a second row was made for an order already being made");
}
