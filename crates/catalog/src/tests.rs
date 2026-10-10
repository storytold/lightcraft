use std::sync::Arc;

use lightcraft_develop::DevelopSettings;

use super::*;

fn photo(c: &mut Catalog, name: &str, date: &str) -> PhotoId {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::Demo { scene: 1 }, name, "JPEG", 6000, 4000, "2026-09-30T10:00:00");
    p.captured = Some(date.to_string());
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

#[test]
fn apply_and_inverse_roundtrip() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "a.jpg", "2026-04-01T10:00:00");
    let before = c.clone();
    let ops = vec![
        Op::SetRating { id: a, rating: 4 },
        Op::SetFlag { id: a, flag: Flag::Pick },
        Op::SetLabel { id: a, label: Some(ColorLabel::Red) },
        Op::SetDevelop {
            id: a,
            settings: Arc::new(DevelopSettings { treatment: lightcraft_develop::Treatment::Bw, ..Default::default() }),
            label: "B&W".into(),
            edited: Some("x".into()),
        },
    ];
    let mut invs = Vec::new();
    for op in ops {
        invs.push(c.apply(op).unwrap());
    }
    assert_eq!(c.photo(a).unwrap().rating, 4);
    for inv in invs.into_iter().rev() {
        c.apply(inv).unwrap();
    }
    assert_eq!(c.photos().collect::<Vec<_>>(), before.photos().collect::<Vec<_>>());
}

#[test]
fn invalid_ops_change_nothing() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "a.jpg", "2026-04-01");
    let snap = c.to_snapshot();
    assert!(c.apply(Op::SetRating { id: a, rating: 9 }).is_err());
    assert!(c.apply(Op::SetRating { id: PhotoId(999), rating: 1 }).is_err());
    // batch with a failing op rolls back
    let r = c.apply(Op::Batch { ops: vec![Op::SetRating { id: a, rating: 2 }, Op::SetFlag { id: PhotoId(77), flag: Flag::Pick }] });
    assert!(r.is_err());
    assert_eq!(c.to_snapshot(), snap);
}

#[test]
fn albums_and_folders() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "a.jpg", "2026-04-01");
    let f = c.alloc_album_id();
    c.apply(Op::AddAlbum {
        album: Album { id: f, name: "Trips".into(), parent: None, folder: true, photos: vec![], cover: None, smart: None, quick: false, order: None },
    })
    .unwrap();
    let al = c.alloc_album_id();
    c.apply(Op::AddAlbum {
        album: Album {
            id: al,
            name: "Alps".into(),
            parent: Some(f),
            folder: false,
            photos: vec![a],
            cover: None,
            smart: None,
            quick: false,
            order: None,
        },
    })
    .unwrap();
    assert_eq!(c.albums_of(a), vec![al]);
    assert!(c.apply(Op::RemoveAlbum { id: f }).is_err(), "non-empty folder");
    assert!(c.apply(Op::SetAlbumPhotos { id: f, photos: vec![a] }).is_err(), "folders hold no photos");
    assert!(c.apply(Op::MoveAlbum { id: f, parent: Some(f) }).is_err());
    let del = c.delete_permanently_ops(a);
    let inv = c.apply(del).unwrap();
    assert!(c.photo(a).is_none() && c.album(al).unwrap().photos.is_empty());
    c.apply(inv).unwrap();
    assert!(c.photo(a).is_some() && c.album(al).unwrap().photos == vec![a]);
}

#[test]
fn filter_search_sort() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "beach.jpg", "2026-05-01T10:00:00");
    let b = photo(&mut c, "alps.jpg", "2026-04-01T10:00:00");
    let d = photo(&mut c, "city.jpg", "2025-12-24T10:00:00");
    c.apply(Op::SetRating { id: b, rating: 5 }).unwrap();
    c.apply(Op::SetFlag { id: d, flag: Flag::Reject }).unwrap();
    let mut m = c.photo(a).unwrap().meta.clone();
    m.keywords = vec!["ocean".into(), "summer".into()];
    m.iso = Some(1600);
    c.apply(Op::SetMeta { id: a, meta: Box::new(m) }).unwrap();

    let all = c.query(&Filter::default(), &Sort::default());
    assert_eq!(all, vec![a, b, d], "newest first");
    let asc = c.query(&Filter::default(), &Sort { key: SortKey::FileName, ascending: true, ..Default::default() });
    assert_eq!(asc, vec![b, a, d]);
    assert_eq!(c.query(&Filter { rating: 4, ..Default::default() }, &Sort::default()), vec![b]);
    assert_eq!(c.query(&Filter { flag: Some(Flag::Reject), ..Default::default() }, &Sort::default()), vec![d]);
    assert_eq!(c.query(&Filter { text: "summer".into(), ..Default::default() }, &Sort::default()), vec![a]);
    assert_eq!(c.query(&Filter { text: "iso:>800".into(), ..Default::default() }, &Sort::default()), vec![a]);
    assert_eq!(c.query(&Filter { text: "date:2025".into(), ..Default::default() }, &Sort::default()), vec![d]);
    assert_eq!(c.query(&Filter { date: Some("2026-04".into()), ..Default::default() }, &Sort::default()), vec![b]);
    c.apply(Op::SetDeleted { id: d, deleted: true }).unwrap();
    assert_eq!(c.query(&Filter::default(), &Sort::default()).len(), 2);
    assert_eq!(c.query(&Filter { deleted: true, ..Default::default() }, &Sort::default()), vec![d]);
    let g = c.date_groups();
    assert_eq!(g[0].year, "2026");
    assert_eq!(g[0].count, 2);
    assert_eq!(g[0].months.iter().map(|m| m.1).sum::<usize>(), 2);
    assert_eq!(g[0].days.iter().map(|d| d.1).sum::<usize>(), 2);
    assert!(g[0].days.iter().all(|(d, _)| d.len() == 10 && d.starts_with("2026")));
    assert!(g[0].days.windows(2).all(|w| w[0].0 > w[1].0), "newest first");
    assert_eq!(c.keywords(), vec![("ocean".to_string(), 1), ("summer".to_string(), 1)]);
}

#[test]
fn snapshot_and_log_replay() {
    let mut c = Catalog::new();
    let snap0 = c.to_snapshot();
    let mut log = String::new();
    let a = c.alloc_photo_id();
    let ops = vec![
        Op::AddPhoto { photo: Box::new(Photo::new(a, Source::File { path: "/x/a.jpg".into() }, "a.jpg", "JPEG", 10, 10, "t")) },
        Op::SetRating { id: a, rating: 3 },
        Op::SetFlag { id: a, flag: Flag::Pick },
    ];
    for op in ops {
        log.push_str(&Catalog::op_to_log_line(&op));
        c.apply(op).unwrap();
    }
    let mut r = Catalog::from_snapshot(&snap0).unwrap();
    // torn last line is tolerated
    let torn = format!("{log}{{\"op\":\"setRat");
    assert_eq!(r.replay(&torn).unwrap(), 3);
    assert_eq!(r.to_snapshot(), c.to_snapshot());
    assert!(Catalog::from_snapshot("{nope").is_err());
}

proptest::proptest! {
    #[test]
    fn random_ops_undo_to_start(ratings in proptest::collection::vec((0usize..3, 0u8..6, 0u8..3), 1..40)) {
        let mut c = Catalog::new();
        let ids: Vec<PhotoId> = (0..3).map(|i| photo(&mut c, &format!("p{i}.jpg"), "2026-01-01")).collect();
        let start = c.to_snapshot();
        let mut invs = Vec::new();
        for (i, r, f) in ratings {
            let op = if f == 0 { Op::SetRating { id: ids[i], rating: r } } else { Op::SetFlag { id: ids[i], flag: [Flag::None, Flag::Pick, Flag::Reject][f as usize] } };
            if let Ok(inv) = c.apply(op) { invs.push(inv); }
        }
        for inv in invs.into_iter().rev() { c.apply(inv).unwrap(); }
        proptest::prop_assert_eq!(c.to_snapshot(), start);
    }
}

#[test]
fn smart_albums_update_live() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "a.jpg", "2026-04-01T10:00:00");
    let b = photo(&mut c, "b.jpg", "2026-05-02T10:00:00");
    let z = photo(&mut c, "z.jpg", "2025-12-31T10:00:00");
    let id = c.alloc_album_id();
    let rules = Filter { rating: 3, date_from: Some("2026-01-01".into()), date_to: Some("2026-04".into()), ..Default::default() };
    c.apply(Op::AddAlbum { album: Album { smart: Some(Box::new(rules)), ..Album::new(id, "Good spring") } }).unwrap();
    assert!(c.album_photos(id).is_empty());
    for p in [a, b, z] {
        c.apply(Op::SetRating { id: p, rating: 4 }).unwrap();
    }
    // b is after the range, z before it
    assert_eq!(c.album_photos(id), vec![a]);
    assert_eq!(c.album_count(id), 1);
    assert_eq!(c.albums_of(a), vec![id]);
    // via the album filter (what the grid uses)
    assert_eq!(c.query(&Filter { album: Some(id), ..Default::default() }, &Sort::default()), vec![a]);
    // deleted photos drop out
    c.apply(Op::SetDeleted { id: a, deleted: true }).unwrap();
    assert!(c.album_photos(id).is_empty());
    c.apply(Op::SetDeleted { id: a, deleted: false }).unwrap();
    // change the rules; inverse restores
    let inv = c.apply(Op::SetAlbumRules { id, rules: Box::new(Filter { rating: 4, rating_op: RatingOp::Exactly, ..Default::default() }) }).unwrap();
    assert_eq!(c.album_photos(id), vec![a, b, z]);
    c.apply(inv).unwrap();
    assert_eq!(c.album_photos(id), vec![a]);
    // smart albums hold no photos and can't nest smart rules
    assert!(c.apply(Op::SetAlbumPhotos { id, photos: vec![b] }).is_err());
    let id2 = c.alloc_album_id();
    let bad = Filter { album: Some(id), ..Default::default() };
    assert!(c.apply(Op::AddAlbum { album: Album { smart: Some(Box::new(bad)), ..Album::new(id2, "x") } }).is_err());
    assert!(c.apply(Op::SetAlbumRules { id, rules: Box::new(Filter { deleted: true, ..Default::default() }) }).is_err());
    // rules on a manual album are rejected
    let manual = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album::new(manual, "m") }).unwrap();
    assert!(c.apply(Op::SetAlbumRules { id: manual, rules: Box::new(Filter::default()) }).is_err());
    // a smart album may narrow a manual album
    c.apply(Op::SetAlbumPhotos { id: manual, photos: vec![b, z] }).unwrap();
    c.apply(Op::SetAlbumRules { id, rules: Box::new(Filter { album: Some(manual), lens: Some(String::new()), ..Default::default() }) }).unwrap();
    assert_eq!(c.album_photos(id), vec![b, z]);
    // persisted in snapshots
    let back = Catalog::from_snapshot(&c.to_snapshot()).unwrap();
    assert_eq!(back.album_photos(id), vec![b, z]);
    assert!(Filter { rating: 3, keyword: Some("sea".into()), ..Default::default() }.describe().contains("rating ≥ 3, keyword sea"));
}

#[test]
fn capture_times_display_like_the_info_panel() {
    use crate::dates::display_time;
    assert_eq!(display_time("2022-03-30T10:11:11"), "March 30, 2022 at 10:11:11 AM");
    assert_eq!(display_time("2022-03-30T22:05:01.5-04:00"), "March 30, 2022 at 10:05:01 PM");
    assert_eq!(display_time("2022-01-02T00:00:00"), "January 2, 2022 at 12:00:00 AM");
    assert_eq!(display_time("2022-07-04"), "July 4, 2022");
    assert_eq!(display_time("someday"), "someday");
}

#[test]
fn folder_paths() {
    use crate::query::in_folder;
    assert!(in_folder("/a/b/c.jpg", "/a/b", false));
    assert!(in_folder("/a/b/c.jpg", "/a/b/", false));
    assert!(!in_folder("/a/b/x/c.jpg", "/a/b", false));
    assert!(in_folder("/a/b/x/c.jpg", "/a/b", true));
    assert!(!in_folder("/a/bc/d.jpg", "/a/b", true), "a sibling with the same prefix");
    assert!(in_folder("C:\\Pics\\a.jpg", "C:\\Pics", false));
}

#[test]
fn merge_results_are_recognised() {
    use crate::query::merged_kind;
    assert_eq!(merged_kind("IMG_1-HDR.dng"), Some("hdr"));
    assert_eq!(merged_kind("IMG_1-Pano.dng"), Some("panorama"));
    assert_eq!(merged_kind("IMG_1-HDR-Pano.dng"), Some("hdrPanorama"));
    assert_eq!(merged_kind("IMG_1-HDR-2.dng"), Some("hdr"), "a second merge of the same photo");
    assert_eq!(merged_kind("hdr.jpg"), None);
    assert_eq!(merged_kind("panorama.jpg"), None);
}

/// Preview-only raws (an undecodable raw variant shown from its embedded JPEG): the reason is
/// stored with the photo, old catalogs without it still load, and Reload's op sets / clears it
/// with an exact inverse that survives serialisation (the journal).
#[test]
fn preview_only_survives_serde_and_set_content_undo() {
    let mut c = Catalog::new();
    let a = c.alloc_photo_id();
    let mut p = Photo::new(a, Source::File { path: "/x/DSC_0001.NEF".into() }, "DSC_0001.NEF", "NEF", 6000, 4000, "2026-10-01T00:00:00");
    p.kind = MediaKind::Raw;
    p.preview_only = Some("Nikon Huffman-compressed NEF".into());
    assert!(!p.develops_raw());
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    // the catalog round-trips it
    let back: Catalog = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(back.photo(a).unwrap().preview_only.as_deref(), Some("Nikon Huffman-compressed NEF"));
    // an older catalog (no field) loads as decodable
    let mut v = serde_json::to_value(&c).unwrap();
    let photos = v["photos"].as_object_mut().unwrap();
    let ph = photos.values_mut().next().unwrap();
    ph.as_object_mut().unwrap().remove("preview_only").expect("serialised when set");
    let old: Catalog = serde_json::from_value(v).unwrap();
    assert_eq!(old.photo(a).unwrap().preview_only, None);
    assert!(old.photo(a).unwrap().develops_raw());
    // Reload: the file decodes now → cleared; undo (through the journal's JSON) restores it
    let op = Op::SetContent { id: a, width: 6016, height: 4000, file_size: 1, content_hash: None, preview_only: None };
    let inv = c.apply(op).unwrap();
    assert!(c.photo(a).unwrap().develops_raw());
    let inv: Op = serde_json::from_str(&serde_json::to_string(&inv).unwrap()).unwrap();
    c.apply(inv).unwrap();
    assert_eq!(c.photo(a).unwrap().preview_only.as_deref(), Some("Nikon Huffman-compressed NEF"));
    assert_eq!(c.photo(a).unwrap().width, 6000);
}

/// Equivalent spellings of one folder (separators, trailing or doubled separators, `.`/`..`,
/// drive-letter case, verbatim and UNC prefixes) share an identity; different folders don't.
#[test]
fn folder_identity_ignores_spelling() {
    use crate::query::{folder_key, folder_within};
    let same = |a: &str, b: &str| assert_eq!(folder_key(a), folder_key(b), "{a} vs {b}");
    let differ = |a: &str, b: &str| assert_ne!(folder_key(a), folder_key(b), "{a} vs {b}");
    for (a, b) in [
        ("D:/Example/Photos", "D:\\Example\\Photos"),
        ("D:/Example/Photos", "d:\\Example\\Photos\\"),
        ("D:\\Example/Photos", "D:/Example//Photos/."),
        ("D:/Example/Photos", "D:/Example/Other/../Photos"),
        ("D:/Example/Photos", "\\\\?\\D:\\Example\\Photos"),
        ("\\\\server\\share\\Photos", "//server/share/Photos/"),
        ("\\\\?\\UNC\\server\\share\\Photos", "//server/share/Photos"),
        ("/home/me/Photos", "/home/me/Photos/"),
        ("/home/me/Photos", "/home//me/./Photos"),
        ("/", "//"),
        ("C:\\", "c:/"),
    ] {
        same(a, b);
    }
    differ("D:/Example/Photos", "D:/Example/Photos2");
    differ("D:/Example/Photos", "E:/Example/Photos");
    differ("/home/me/Photos", "/home/me");
    assert_eq!(folder_key("/a/../../b"), "/b", "`..` stops at the root");
    assert!(folder_within("D:\\Example\\Photos\\2026", "D:/Example/Photos/"));
    assert!(folder_within("D:/Example/Photos", "d:\\Example\\Photos"));
    assert!(!folder_within("D:/Example/Photos2", "D:/Example/Photos"));
    assert!(folder_within("/a/b", "/"));
    assert!(folder_within("C:\\x", "c:\\"));
}

/// People come from named *face* regions: counted once per photo, case-insensitive, most photos
/// first; pets and unnamed faces are not people; the `person` filter and `person:` token match.
#[test]
fn people_from_named_face_regions() {
    use lightcraft_meta::{Rect, Region, RegionKind};
    let region = |name: Option<&str>, kind: RegionKind| Region {
        rect: Rect { x0: 0.4, y0: 0.4, x1: 0.6, y1: 0.6 },
        kind,
        name: name.map(str::to_string),
        description: None,
    };
    let mut c = Catalog::new();
    let mut add = |name: &str, regions: Vec<Region>| {
        let id = c.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 1 }, name, "JPEG", 6000, 4000, "2026-09-30T10:00:00");
        p.meta.regions = regions;
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let a = add(
        "a.jpg",
        vec![region(Some("Jane Doe"), RegionKind::Face), region(Some("jane doe"), RegionKind::Face), region(Some("Rex"), RegionKind::Pet)],
    );
    let b =
        add("b.jpg", vec![region(Some("JANE DOE"), RegionKind::Face), region(Some("John Roe"), RegionKind::Face), region(None, RegionKind::Face)]);
    let big = Rect { x0: 0.2, y0: 0.2, x1: 0.8, y1: 0.8 };
    let d = add("d.jpg", vec![Region { rect: big, ..region(Some("John Roe"), RegionKind::Face) }]);
    let e = add("e.jpg", vec![region(Some("Sam"), RegionKind::Face)]);
    add("f.jpg", vec![]);
    let people = c.people();
    let summary: Vec<(&str, usize)> = people.iter().map(|p| (p.name.as_str(), p.count)).collect();
    assert_eq!(summary, vec![("Jane Doe", 2), ("John Roe", 2), ("Sam", 1)], "once per photo, pets and unnamed faces left out");
    // the picture is the person's largest face; ties go to the lower photo id
    assert_eq!((people[0].photo, people[0].face), (a, Rect { x0: 0.4, y0: 0.4, x1: 0.6, y1: 0.6 }));
    assert_eq!((people[1].photo, people[1].face), (d, big), "the larger face wins over an earlier, smaller one");

    let q = |f: Filter| c.query(&f, &Sort::default());
    assert_eq!(q(Filter { person: Some("jane doe".into()), ..Default::default() }).len(), 2);
    let mut got = q(Filter { person: Some("John Roe".into()), ..Default::default() });
    got.sort();
    assert_eq!(got, vec![b, d]);
    assert!(q(Filter { person: Some("Rex".into()), ..Default::default() }).is_empty(), "a pet is not a person");
    assert_eq!(q(Filter { text: "person:SAM".into(), ..Default::default() }), vec![e], "the search token finds a person, any case");
    assert!(q(Filter { person: Some("Jane Doe".into()), ..Default::default() }).contains(&a));

    // other filters narrow who is offered (so picking a person never ends in an empty grid); the
    // `person` filter itself does not
    c.apply(Op::SetRating { id: b, rating: 3 }).unwrap();
    let names = |f: Filter| c.people_in(&f).into_iter().map(|p| (p.name, p.count)).collect::<Vec<_>>();
    let rated = Filter { rating: 1, ..Default::default() };
    assert_eq!(
        names(rated.clone()),
        vec![("JANE DOE".to_string(), 1), ("John Roe".to_string(), 1)],
        "only the rated photo's people, counted within it, spelled as first seen there"
    );
    assert_eq!(
        names(Filter { person: Some("Sam".into()), ..rated }),
        names(Filter { rating: 1, ..Default::default() }),
        "the person filter is ignored"
    );
    assert_eq!(names(Filter { rating: 5, ..Default::default() }), vec![], "nobody in the filtered photos");
}

/// Most photos have no regions: the field is left out of the catalog JSON then, and a catalog written
/// before regions existed (no `regions` key) reads with none.
#[test]
fn empty_regions_are_not_serialized_and_default_when_missing() {
    let m = Meta { keywords: vec!["k".into()], ..Default::default() };
    let v = serde_json::to_value(&m).unwrap();
    assert!(v.get("regions").is_none(), "{v}");
    let back: Meta = serde_json::from_value(v).unwrap();
    assert!(back.regions.is_empty());
    let mut with = m.clone();
    with.regions.push(lightcraft_meta::Region {
        rect: lightcraft_meta::Rect { x0: 0.1, y0: 0.1, x1: 0.2, y1: 0.2 },
        kind: lightcraft_meta::RegionKind::Face,
        name: Some("A".into()),
        description: None,
    });
    let back: Meta = serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
    assert_eq!(back, with);
}

// ---- random sort: Given a seed, the order is a stable, reproducible shuffle of the matching photos

fn many(c: &mut Catalog, n: usize) -> Vec<PhotoId> {
    (0..n).map(|i| photo(c, &format!("p{i:03}.jpg"), &format!("2026-04-{:02}T10:00:00", i % 28 + 1))).collect()
}

fn random(seed: u64) -> Sort {
    Sort { key: SortKey::Random, seed, ..Default::default() }
}

#[test]
fn random_sort_is_a_reproducible_permutation() {
    let mut c = Catalog::new();
    let ids = many(&mut c, 50);
    let a = c.query(&Filter::default(), &random(7));
    assert_eq!(a, c.query(&Filter::default(), &random(7)), "same seed, same order");
    let mut sorted = a.clone();
    sorted.sort();
    let mut all = ids.clone();
    all.sort();
    assert_eq!(sorted, all, "every photo exactly once");
    assert_ne!(a, c.query(&Filter::default(), &random(8)), "another seed, another order");
    assert_ne!(a, c.query(&Filter::default(), &Sort::default()), "not just the date order");
}

#[test]
fn random_sort_keeps_the_relative_order_when_the_set_changes() {
    let mut c = Catalog::new();
    let ids = many(&mut c, 30);
    let before = c.query(&Filter::default(), &random(3));
    let added = photo(&mut c, "new.jpg", "2026-05-01T10:00:00");
    c.apply(Op::SetRating { id: ids[0], rating: 5 }).unwrap();
    let after: Vec<PhotoId> = c.query(&Filter::default(), &random(3)).into_iter().filter(|id| *id != added).collect();
    assert_eq!(after, before, "adding a photo or editing metadata must not reshuffle the others");
}

#[test]
fn random_sort_respects_the_filter_and_handles_tiny_sets() {
    let mut c = Catalog::new();
    assert!(c.query(&Filter::default(), &random(1)).is_empty());
    let ids = many(&mut c, 10);
    c.apply(Op::SetRating { id: ids[4], rating: 5 }).unwrap();
    assert_eq!(c.query(&Filter { rating: 5, ..Default::default() }, &random(1)), vec![ids[4]]);
}

#[test]
fn random_sort_direction_reverses_and_serde_defaults_hold() {
    let mut c = Catalog::new();
    many(&mut c, 20);
    let desc = c.query(&Filter::default(), &Sort { ascending: false, ..random(5) });
    let mut asc = c.query(&Filter::default(), &Sort { ascending: true, ..random(5) });
    asc.reverse();
    assert_eq!(desc, asc);
    // saved sorts from before the seed existed still load
    let old: Sort = serde_json::from_str(r#"{"key":"fileName","ascending":true}"#).unwrap();
    assert_eq!(old.seed, 0);
    assert_eq!(serde_json::from_str::<Sort>(r#"{"key":"random","seed":9}"#).unwrap(), Sort { key: SortKey::Random, seed: 9, ..Default::default() });
}

#[test]
fn random_sort_has_no_date_headers() {
    let mut c = Catalog::new();
    let ids = many(&mut c, 5);
    assert!(c.date_runs(&ids, SortKey::Random, GroupBy::Day).is_empty());
}

/// Reload stores the lens data a file carries; the inverse (through the journal's JSON) takes it back.
#[test]
fn set_embedded_lens_is_undoable_and_journaled() {
    let mut c = Catalog::default();
    let a = c.alloc_photo_id();
    c.apply(Op::AddPhoto {
        photo: Box::new(Photo::new(a, Source::File { path: "/x.rw2".into() }, "x.rw2", "RW2", 4000, 3000, "2026-10-06T00:00:00")),
    })
    .unwrap();
    let lens = lightcraft_develop::EmbeddedLens { warp: Some(Default::default()), vignette: None };
    let op = Op::SetEmbeddedLens { id: a, lens: Some(Box::new(lens)) };
    let op: Op = serde_json::from_str(&serde_json::to_string(&op).unwrap()).unwrap();
    let inv = c.apply(op).unwrap();
    assert_eq!(c.photo(a).unwrap().embedded_lens, Some(lens));
    let inv: Op = serde_json::from_str(&serde_json::to_string(&inv).unwrap()).unwrap();
    c.apply(inv).unwrap();
    assert_eq!(c.photo(a).unwrap().embedded_lens, None);
}

/// A saved smart album whose rules no longer check (the album a rule tests was deleted) is found,
/// with its problems; a good one has none; an old album operator is read as it was meant.
#[test]
fn smart_album_problems_flag_stale_rules() {
    let mut c = Catalog::new();
    c.apply(Op::AddAlbum { album: Album::new(AlbumId(1), "Trip") }).unwrap();
    let smart = |id: u64, rules: serde_json::Value| {
        let f: Filter = serde_json::from_value(serde_json::json!({"ruleSet": {"rules": rules}})).unwrap();
        Album { smart: Some(Box::new(f)), ..Album::new(AlbumId(id), "Smart") }
    };
    c.apply(Op::AddAlbum { album: smart(2, serde_json::json!([{"field": "album", "op": "is", "value": 1}])) }).unwrap();
    c.apply(Op::AddAlbum { album: smart(3, serde_json::json!([{"field": "album", "op": "gte", "value": 1}])) }).unwrap();
    assert!(c.smart_album_problems(AlbumId(2)).is_empty());
    assert!(c.smart_album_problems(AlbumId(3)).is_empty(), "an old operator is upgraded, not a problem");
    assert!(c.smart_album_problems(AlbumId(1)).is_empty(), "a plain album has no rules");
    c.apply(Op::RemoveAlbum { id: AlbumId(1) }).unwrap();
    let p = c.smart_album_problems(AlbumId(2));
    assert_eq!(p.iter().map(ToString::to_string).collect::<Vec<_>>(), vec!["rule 1: no album 1".to_string()]);
}

/// A smart album can test another smart album: "Keywords contain travel" and "Album isn't Excluded
/// Photos" (red or rejected) leaves out the excluded photos, and follows that album's rules as they
/// change.
#[test]
fn a_smart_album_can_exclude_another() {
    let mut c = Catalog::new();
    let smart = |id: u64, name: &str, rules: serde_json::Value| {
        let f: Filter = serde_json::from_value(serde_json::json!({"ruleSet": rules})).unwrap();
        Album { smart: Some(Box::new(f)), ..Album::new(AlbumId(id), name) }
    };
    c.apply(Op::AddAlbum {
        album: smart(
            1,
            "Excluded Photos",
            serde_json::json!({"match": "any", "rules": [
            {"field": "label", "op": "is", "value": "red"}, {"field": "flag", "op": "is", "value": "reject"}]}),
        ),
    })
    .unwrap();
    let travel = |c: &mut Catalog, name: &str, label: Option<ColorLabel>, flag: Flag| {
        let id = c.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 1 }, name, "JPEG", 6000, 4000, "2026-09-30T10:00:00");
        p.meta.keywords = vec!["travel".into()];
        p.label = label;
        p.flag = flag;
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let keep = travel(&mut c, "keep.jpg", None, Flag::Pick);
    let red = travel(&mut c, "red.jpg", Some(ColorLabel::Red), Flag::None);
    let rejected = travel(&mut c, "rejected.jpg", None, Flag::Reject);
    let rules: RuleSet = serde_json::from_value(serde_json::json!({"rules": [
        {"field": "keywords", "op": "contains", "value": "travel"}, {"field": "album", "op": "isNot", "value": 1}]}))
    .unwrap();
    assert!(rules.check(&c).is_empty(), "a smart album can be tested: {:?}", rules.check(&c));
    let matched = |c: &Catalog| c.photos().filter(|p| rules.matches(p, c)).map(|p| p.id).collect::<Vec<_>>();
    assert_eq!(matched(&c), vec![keep]);
    // "is" works the other way round
    let inside: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "album", "op": "is", "value": 1}]})).unwrap();
    assert_eq!(c.photos().filter(|p| inside.matches(p, &c)).count(), 2);
    let _ = (red, rejected);
}

/// An album can't include itself, directly or through other smart albums: the check refuses the
/// loop for the album being edited, and a loop saved anyway (an older version, a hand-edited file)
/// neither recurses forever nor crashes: inside it, the album being evaluated counts as holding no
/// photos, and every album in it is reported.
#[test]
fn smart_album_loops_are_refused_and_survived() {
    let mut c = Catalog::new();
    let refers = |to: u64| -> Filter {
        serde_json::from_value(serde_json::json!({"ruleSet": {"rules": [{"field": "album", "op": "isNot", "value": to}]}})).unwrap()
    };
    let smart = |id: u64, f: Filter| Album { smart: Some(Box::new(f)), ..Album::new(AlbumId(id), "S") };
    // A → B → C
    c.apply(Op::AddAlbum { album: smart(3, Filter::default()) }).unwrap();
    c.apply(Op::AddAlbum { album: smart(2, refers(3)) }).unwrap();
    c.apply(Op::AddAlbum { album: smart(1, refers(2)) }).unwrap();
    let rules = |to: u64| refers(to).rule_set.unwrap();
    assert!(rules(1).check_for(&c, Some(AlbumId(3))).iter().any(|p| p.issue == crate::rules::Issue::AlbumLoop), "C → A closes A → B → C → A");
    assert!(rules(3).check_for(&c, Some(AlbumId(3))).iter().any(|p| p.issue == crate::rules::Issue::AlbumLoop), "an album testing itself");
    assert!(rules(1).check_for(&c, None).is_empty(), "a new album can't be in a loop yet");
    assert!(rules(3).check_for(&c, Some(AlbumId(1))).is_empty(), "A testing C again is no loop");
    assert!(!c.album_reaches(AlbumId(3), AlbumId(1)) && c.album_reaches(AlbumId(1), AlbumId(3)));
    // a loop saved anyway: C → A
    c.apply(Op::SetAlbumRules { id: AlbumId(3), rules: Box::new(refers(1)) }).unwrap();
    let id = photo(&mut c, "a.jpg", "2026-09-01T10:00:00");
    let p = c.photo(id).unwrap().clone();
    for a in [1, 2, 3] {
        let _ = c.album_contains(AlbumId(a), &p); // returns, no stack overflow
        assert!(c.smart_album_problems(AlbumId(a)).iter().any(|p| p.issue == crate::rules::Issue::AlbumLoop), "album {a} is reported");
    }
}

/// Summaries name the albums Album rules test ("album isn't “Excluded Photos”"), not their ids;
/// one that is gone shows as its number.
#[test]
fn summaries_name_albums() {
    let mut c = Catalog::new();
    c.apply(Op::AddAlbum { album: Album::new(AlbumId(4), "Excluded Photos") }).unwrap();
    let rules: RuleSet = serde_json::from_value(serde_json::json!({"rules": [
        {"field": "keywords", "op": "contains", "value": "travel"},
        {"group": {"match": "any", "rules": [{"field": "album", "op": "isNot", "value": 4}, {"field": "album", "op": "is", "value": 9}]}}]}))
    .unwrap();
    assert_eq!(rules.describe_with(&c), "keywords contains travel and (album isn't “Excluded Photos” or album is #9)");
    let f = Filter { rule_set: Some(rules), ..Default::default() };
    assert!(f.describe_with(&c).contains("album isn't “Excluded Photos”"));
}

/// A chain of smart albums each testing the one before twice ("all of: album is X(k-1), album is
/// X(k-1)") costs a pass per album, not 2^depth: each album's answer for a photo is worked out once
/// per question. 25 albums deep stays instant.
#[test]
fn chains_of_smart_albums_stay_linear() {
    let mut c = Catalog::new();
    let smart = |id: u64, f: Filter| Album { smart: Some(Box::new(f)), ..Album::new(AlbumId(id), "X") };
    c.apply(Op::AddAlbum { album: smart(1, Filter::default()) }).unwrap();
    for k in 2..=26u64 {
        let f: Filter = serde_json::from_value(serde_json::json!({"ruleSet": {"rules": [
            {"field": "album", "op": "is", "value": k - 1}, {"field": "album", "op": "is", "value": k - 1}]}}))
        .unwrap();
        c.apply(Op::AddAlbum { album: smart(k, f) }).unwrap();
    }
    for i in 0..20 {
        photo(&mut c, &format!("p{i}.jpg"), "2026-09-01T10:00:00");
    }
    let start = std::time::Instant::now();
    assert_eq!(c.album_count(AlbumId(26)), 20);
    assert!(start.elapsed() < std::time::Duration::from_secs(2), "took {:?}", start.elapsed());
}

/// A smart album's own album filter (not only its rules) counts in loops: A filtered to "in A" is
/// reported, and so is A filtered to B while B's rules test A.
#[test]
fn an_album_filter_counts_in_loops() {
    let mut c = Catalog::new();
    let smart = |id: u64, f: Filter| Album { smart: Some(Box::new(f)), ..Album::new(AlbumId(id), "S") };
    c.apply(Op::AddAlbum { album: smart(1, Filter { album: Some(AlbumId(1)), ..Default::default() }) }).unwrap();
    let loops = |c: &Catalog, id: u64| c.smart_album_problems(AlbumId(id)).iter().any(|p| p.issue == crate::rules::Issue::AlbumLoop);
    assert!(loops(&c, 1), "filtered to itself");
    let tests_3: Filter = serde_json::from_value(serde_json::json!({"ruleSet": {"rules": [{"field": "album", "op": "is", "value": 3}]}})).unwrap();
    c.apply(Op::AddAlbum { album: smart(2, Filter { album: Some(AlbumId(3)), ..Default::default() }) }).unwrap();
    c.apply(Op::AddAlbum {
        album: smart(3, serde_json::from_value(serde_json::json!({"ruleSet": {"rules": [{"field": "album", "op": "is", "value": 2}]}})).unwrap()),
    })
    .unwrap();
    assert!(c.album_reaches(AlbumId(3), AlbumId(2)) && c.album_reaches(AlbumId(2), AlbumId(3)));
    assert!(loops(&c, 2) && loops(&c, 3));
    let _ = tests_3;
    let shown: Vec<String> = c.smart_album_problems(AlbumId(1)).iter().map(ToString::to_string).collect();
    assert!(shown.iter().all(|s| !s.starts_with("rule :")), "{shown:?}");
}

#[test]
fn undated_photos_group_under_unknown_date_and_sort_together() {
    let mut c = Catalog::new();
    let add_photo = |c: &mut Catalog, name: &str, captured: Option<&str>, imported: &str| {
        let id = c.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 0 }, name, "JPEG", 100, 100, imported);
        p.captured = captured.map(str::to_string);
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let p_dated1 = add_photo(&mut c, "dated1.jpg", Some("2026-05-10T12:00:00"), "2026-10-01T00:00:00");
    let p_dated2 = add_photo(&mut c, "dated2.jpg", Some("2026-05-20T12:00:00"), "2026-10-01T00:00:00");
    let p_undated1 = add_photo(&mut c, "undated1.jpg", None, "2026-10-05T00:00:00");
    let p_undated2 = add_photo(&mut c, "undated2.jpg", None, "2026-10-02T00:00:00");

    // Descending sort (default, newest first): dated photos first (newest to oldest), undated photos at the end
    let desc = c.query(&Filter::default(), &Sort { key: SortKey::CaptureDate, ascending: false, ..Default::default() });
    assert_eq!(desc, vec![p_dated2, p_dated1, p_undated1, p_undated2]);

    // Ascending sort (oldest first): undated photos at the beginning, dated photos from oldest to newest
    let asc = c.query(&Filter::default(), &Sort { key: SortKey::CaptureDate, ascending: true, ..Default::default() });
    assert_eq!(asc, vec![p_undated2, p_undated1, p_dated1, p_dated2]);

    // date_runs groups undated photos under Unknown Date
    let runs = c.date_runs(&desc, SortKey::CaptureDate, GroupBy::Day);
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0].key, "2026-05-20");
    assert_eq!(runs[1].key, "2026-05-10");
    assert_eq!(runs[2].key, "");
    assert_eq!(runs[2].label, "Unknown Date");
    assert_eq!(runs[2].count, 2);

    // date_groups sidebar tree only counts photos with capture dates
    let groups = c.date_groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].year, "2026");
    assert_eq!(groups[0].count, 2);

    // Filter by capture date prefix does not match undated photos
    let filter_day = Filter { date: Some("2026-10-05".into()), ..Default::default() };
    assert_eq!(c.query(&filter_day, &Sort::default()), vec![]);
    let filter_year = Filter { date: Some("2026".into()), ..Default::default() };
    assert_eq!(c.query(&filter_year, &Sort { key: SortKey::CaptureDate, ascending: false, ..Default::default() }), vec![p_dated2, p_dated1]);
}
