//! The derived indexes ([`crate::index`]) answer as a scan of the catalog would, through every op,
//! inverse, failed batch, snapshot, replay and clone; and the faster query paths give the same
//! results as before.

use proptest::prelude::*;

use super::*;

const PHOTOS: u64 = 8;

fn library() -> Catalog {
    let mut c = Catalog::new();
    for i in 0..PHOTOS {
        let p = Photo::new(PhotoId(i + 1), Source::Demo { scene: 1 }, &format!("p{i}.jpg"), "JPEG", 60, 40, "2026-01-01T00:00:00");
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    c
}

/// What the indexes must say, worked out by scanning (as the catalog did before it had them).
fn scanned(c: &Catalog) -> (Vec<Option<StackId>>, Vec<Vec<bool>>) {
    let photos: Vec<PhotoId> = (1..=PHOTOS + 1).map(PhotoId).collect();
    let stacks = photos.iter().map(|p| c.stacks().find(|s| s.photos.contains(p)).map(|s| s.id)).collect();
    let albums = (1..=4)
        .map(|a| {
            photos
                .iter()
                .map(|p| c.photo(*p).is_some() && c.album(AlbumId(a)).is_some_and(|al| al.smart.is_none() && al.photos.contains(p)))
                .collect()
        })
        .collect();
    (stacks, albums)
}

/// What the indexes say.
fn indexed(c: &Catalog) -> (Vec<Option<StackId>>, Vec<Vec<bool>>) {
    let photos: Vec<PhotoId> = (1..=PHOTOS + 1).map(PhotoId).collect();
    let stacks = photos.iter().map(|p| c.stack_of(*p).map(|s| s.id)).collect();
    let albums = (1..=4)
        .map(|a| {
            photos
                .iter()
                .map(|p| c.album(AlbumId(a)).is_some_and(|al| al.smart.is_none()) && c.photo(*p).is_some_and(|ph| c.album_contains(AlbumId(a), ph)))
                .collect()
        })
        .collect();
    (stacks, albums)
}

/// Photo ids from bits (the extra id names a photo that isn't there: such ops fail).
fn ids(bits: u16) -> Vec<PhotoId> {
    (0..=PHOTOS).filter(|i| bits & (1 << i) != 0).map(|i| PhotoId(i + 1)).collect()
}

fn op(c: &Catalog, kind: u8, n: u64, bits: u16, collapsed: bool) -> Op {
    match kind % 8 {
        0 => Op::AddStack { stack: Stack { id: StackId(n % 4 + 1), photos: ids(bits), collapsed } },
        1 => Op::RemoveStack { id: StackId(n % 4 + 1) },
        2 => Op::SetStack { id: StackId(n % 4 + 1), photos: ids(bits), collapsed },
        3 => {
            let mut a = Album::new(AlbumId(n % 4 + 1), "A");
            a.photos = ids(bits);
            Op::AddAlbum { album: a }
        }
        4 => Op::RemoveAlbum { id: AlbumId(n % 4 + 1) },
        5 => Op::SetAlbumPhotos { id: AlbumId(n % 4 + 1), photos: ids(bits) },
        // a batch whose last op fails half the time: what it did is rolled back
        6 => Op::Batch {
            ops: vec![
                Op::SetAlbumPhotos { id: AlbumId(n % 4 + 1), photos: ids(bits) },
                Op::SetStack { id: StackId(n % 4 + 1), photos: ids(bits.rotate_left(3)), collapsed },
                Op::AddStack { stack: Stack { id: StackId(n % 4 + 1), photos: ids(!bits), collapsed } },
            ],
        },
        // as the engine deletes a photo: out of its albums and stacks too, in one batch
        _ => c.delete_permanently_ops(PhotoId(n % (PHOTOS + 1) + 1)),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn indexes_agree_with_a_scan(steps in proptest::collection::vec((0u8..8, 0u64..16, 0u16..512, any::<bool>()), 1..40)) {
        let mut c = library();
        let start = c.to_snapshot();
        // built before the ops, so every op has to keep them up to date
        prop_assert_eq!(indexed(&c), scanned(&c));
        let mut log = String::new();
        let mut inverses = Vec::new();
        for (kind, n, bits, collapsed) in steps {
            let op = op(&c, kind, n, bits, collapsed);
            if let Ok(inv) = c.apply(op.clone()) {
                log.push_str(&Catalog::op_to_log_line(&op));
                inverses.push(inv);
            }
            prop_assert_eq!(indexed(&c), scanned(&c));
        }
        // a snapshot, a replayed journal and a clone build their own
        let snap = Catalog::from_snapshot(&c.to_snapshot()).unwrap();
        prop_assert_eq!(indexed(&snap), scanned(&c));
        let mut replayed = Catalog::from_snapshot(&start).unwrap();
        prop_assert_eq!(indexed(&replayed), scanned(&replayed));
        replayed.replay(&log).unwrap();
        prop_assert_eq!(indexed(&replayed), scanned(&c));
        prop_assert_eq!(indexed(&c.clone()), scanned(&c));
        prop_assert!(c.clone() == c, "the indexes don't take part in equality");
        // undone step by step
        for inv in inverses.into_iter().rev() {
            c.apply(inv).unwrap();
            prop_assert_eq!(indexed(&c), scanned(&c));
        }
        prop_assert!(c.stacks().next().is_none() && c.albums().next().is_none(), "all undone");
    }
}

#[test]
fn a_photo_in_another_stack_is_refused_by_the_index() {
    let mut c = library();
    c.apply(Op::AddStack { stack: Stack { id: StackId(1), photos: vec![PhotoId(1), PhotoId(2)], collapsed: false } }).unwrap();
    let e = c.apply(Op::AddStack { stack: Stack { id: StackId(2), photos: vec![PhotoId(2), PhotoId(3)], collapsed: false } }).unwrap_err();
    assert_eq!(e, CatalogError::Invalid("a photo can be in only one stack".into()));
    // its own stack may keep it
    c.apply(Op::SetStack { id: StackId(1), photos: vec![PhotoId(2), PhotoId(1), PhotoId(3)], collapsed: true }).unwrap();
    assert_eq!(c.stack_of(PhotoId(3)).map(|s| s.id), Some(StackId(1)));
}

/// The sort keys are worked out once per photo now; the order is the one the comparisons gave.
#[test]
fn sorted_queries_keep_their_order() {
    let mut c = Catalog::new();
    let names = ["b.jpg", "A.jpg", "a.jpg", "Été.jpg", "été.jpg", "C.JPG", "b.jpg"];
    for (i, n) in names.iter().enumerate() {
        let mut p = Photo::new(PhotoId(i as u64 + 1), Source::Demo { scene: 1 }, n, "JPEG", 60, 40, &format!("2026-01-0{}T00:00:00", 7 - i % 3));
        p.captured = (i % 2 == 0).then(|| format!("2025-0{}-01T00:00:00", 1 + i % 4));
        p.rating = (i % 3) as u8;
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let all = Filter::default();
    for key in [SortKey::CaptureDate, SortKey::ImportDate, SortKey::EditDate, SortKey::FileName, SortKey::Rating, SortKey::FileSize, SortKey::Random]
    {
        for ascending in [true, false] {
            let sort = Sort { key, ascending, seed: 7, ..Default::default() };
            // the comparison the query sorts by
            let mut want: Vec<&Photo> = c.photos().map(|p| p.as_ref()).collect();
            want.sort_by(|a, b| sort.order(a, b));
            let want: Vec<PhotoId> = want.iter().map(|p| p.id).collect();
            assert_eq!(c.query(&all, &sort), want, "{key:?} ascending {ascending}");
        }
    }
    // `only` as a set gives the photos it names
    let only = Filter { only: vec![PhotoId(3), PhotoId(1), PhotoId(3)], ..Default::default() };
    assert_eq!(c.query(&only, &Sort { key: SortKey::FileName, ascending: true, ..Default::default() }), vec![PhotoId(3), PhotoId(1)]);
}

/// Rules name flags, labels and kinds as their variants in lower case (what `{:?}` gave).
#[test]
fn rule_names_are_the_variants_in_lower_case() {
    let mut c = Catalog::new();
    let mut n = 0;
    for flag in [Flag::None, Flag::Pick, Flag::Reject] {
        for label in
            [None, Some(ColorLabel::Red), Some(ColorLabel::Yellow), Some(ColorLabel::Green), Some(ColorLabel::Blue), Some(ColorLabel::Purple)]
        {
            for kind in [MediaKind::Image, MediaKind::Raw, MediaKind::Video] {
                n += 1;
                let mut p = Photo::new(PhotoId(n), Source::Demo { scene: 1 }, "x.jpg", "JPEG", 60, 40, "2026-01-01T00:00:00");
                (p.flag, p.label, p.kind) = (flag, label, kind);
                c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            }
        }
    }
    c.apply(Op::SetLabelName { label: ColorLabel::Blue, name: Some("Sky".into()) }).unwrap();
    let count = |field: &str, value: String| {
        let rules: RuleSet =
            serde_json::from_value(serde_json::json!({"match": "all", "rules": [{"field": field, "op": "is", "value": value}]})).unwrap();
        c.query(&Filter { rule_set: Some(rules), ..Default::default() }, &Sort::default()).len()
    };
    for flag in [Flag::None, Flag::Pick, Flag::Reject] {
        assert_eq!(count("flag", format!("{flag:?}").to_lowercase()), 18, "{flag:?}");
    }
    assert_eq!(count("flag", "picked".into()), 18);
    for label in ColorLabel::ALL {
        assert_eq!(count("label", format!("{label:?}").to_uppercase()), 9, "{label:?}");
    }
    assert_eq!(count("label", "sky".into()), 9, "a custom name");
    assert_eq!(count("label", "none".into()), 9);
    for kind in [MediaKind::Image, MediaKind::Raw, MediaKind::Video] {
        assert_eq!(count("kind", format!("{kind:?}")), 18, "{kind:?}");
    }
}

/// A hand-edited file can put a photo in two stacks (ops refuse it): loading repairs it (the photo
/// stays in the first) and the stack index gives the answers a scan of the stacks gives, before
/// and after either stack goes.
#[test]
fn a_photo_in_two_stacks_read_from_a_file_answers_as_a_scan() {
    let mut c = library();
    c.apply(Op::AddStack { stack: Stack { id: StackId(1), photos: vec![PhotoId(1), PhotoId(2)], collapsed: false } }).unwrap();
    c.apply(Op::AddStack { stack: Stack { id: StackId(2), photos: vec![PhotoId(3), PhotoId(4)], collapsed: false } }).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&c.to_snapshot()).unwrap();
    v["stacks"]["2"]["photos"] = serde_json::json!([2, 3, 4]);
    let mut c = Catalog::from_snapshot(&v.to_string()).unwrap();
    assert_eq!(indexed(&c), scanned(&c));
    assert_eq!(c.stack_of(PhotoId(2)).map(|s| s.id), Some(StackId(1)));
    // the second stack can't be changed to keep it, as before
    assert!(c.apply(Op::SetStack { id: StackId(2), photos: vec![PhotoId(2), PhotoId(3)], collapsed: false }).is_err());
    c.apply(Op::RemoveStack { id: StackId(1) }).unwrap();
    assert_eq!(indexed(&c), scanned(&c));
    assert_eq!(c.stack_of(PhotoId(2)).map(|s| s.id), None, "the repair took it out of the second stack");
}
