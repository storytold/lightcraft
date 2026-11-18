use std::sync::Arc;

use lcc_develop::DevelopSettings;
use serde_json::json;

use super::*;

fn photo(c: &mut Catalog, name: &str, date: &str) -> PhotoId {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::Demo { scene: 1 }, name, "JPEG", 6000, 4000, "2026-09-30T10:00:00");
    p.captured = Some(date.to_string());
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

/// An edit built in code with more masks than the limit (a script adding one a command at a time)
/// is stored cut to the limit: every edit goes through `apply` (issue #2121).
#[test]
fn edits_are_stored_within_list_limits() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "a.jpg", "2026-04-01T10:00:00");
    let many = DevelopSettings { masks: vec![Default::default(); lcc_develop::MAX_MASKS + 50], ..Default::default() };
    c.apply(Op::SetDevelop { id: a, settings: Arc::new(many), label: "Masks".into(), edited: None }).unwrap();
    assert_eq!(c.photo(a).unwrap().develop.masks.len(), lcc_develop::MAX_MASKS);
    // within the limits the same settings are kept, not copied
    let few = Arc::new(DevelopSettings { masks: vec![Default::default(); 3], ..Default::default() });
    c.apply(Op::SetDevelop { id: a, settings: few.clone(), label: "Masks".into(), edited: None }).unwrap();
    assert!(Arc::ptr_eq(&c.photo(a).unwrap().develop, &few));
}

#[cfg(test)]
mod filter_deserialize_tests {
    use super::*;

    /// #2241: a filter key that is not one of `Filter`'s fields (a typo or a guessed name such as
    /// `cameraModel` instead of `camera`) is rejected, not silently dropped: an agent or script that
    /// filters by `cameraModel` must not be handed the whole library and told the filter worked.
    #[test]
    fn unknown_filter_key_is_rejected_not_ignored() {
        let e = serde_json::from_value::<Filter>(json!({ "cameraModel": "X2" })).unwrap_err().to_string();
        assert!(e.contains("cameraModel"), "the unknown key is named: {e}");

        let e = serde_json::from_value::<Filter>(json!({ "foo": 1 })).unwrap_err().to_string();
        assert!(e.contains("foo"), "the unknown key is named: {e}");

        // the valid key is still accepted; `camera` is the field `cameraModel` was close to.
        serde_json::from_value::<Filter>(json!({ "camera": "X2" })).unwrap();
    }
}

#[test]
fn date_token_matches_time_part_case_insensitively() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "a.jpg", "2026-05-01T10:00:00");
    let q = |c: &Catalog, text: &str| c.query(&Filter { text: text.into(), ..Default::default() }, &Sort::default());
    assert_eq!(q(&c, "date:2026-05-01T10"), vec![a]);
    assert_eq!(q(&c, "date:2026-05-01t10"), vec![a]);
    assert_eq!(q(&c, "date:2026-05"), vec![a]);
    assert!(q(&c, "date:2026-05-01T11").is_empty());
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
            settings: Arc::new(DevelopSettings { treatment: lcc_develop::Treatment::Bw, ..Default::default() }),
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
        album: Album {
            id: f,
            name: "Trips".into(),
            parent: None,
            folder: true,
            photos: vec![],
            cover: None,
            smart: None,
            quick: false,
            order: None,
            creation: None,
            photo_order: Vec::new(),
        },
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
            creation: None,
            photo_order: Vec::new(),
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
    assert_eq!(all, vec![d, b, a], "oldest first, as in Lightroom Classic");
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

/// A search word with a colon whose left part is not a known field ("10:30", "Re:") is plain
/// text, not a fielded token that matches nothing.
#[test]
fn free_text_with_a_colon_searches_the_text_fields() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "meeting.jpg", "2026-05-01T10:00:00");
    let b = photo(&mut c, "reply.jpg", "2026-04-01T10:00:00");
    let _other = photo(&mut c, "other.jpg", "2026-03-01T10:00:00");
    let mut m = c.photo(a).unwrap().meta.clone();
    m.caption = "Meeting at 10:30".into();
    m.iso = Some(1600);
    c.apply(Op::SetMeta { id: a, meta: Box::new(m) }).unwrap();
    let mut m = c.photo(b).unwrap().meta.clone();
    m.title = "Re: the brief".into();
    c.apply(Op::SetMeta { id: b, meta: Box::new(m) }).unwrap();
    let q = |text: &str| c.query(&Filter { text: text.into(), ..Default::default() }, &Sort::default());
    assert_eq!(q("10:30"), vec![a], "a word with a colon is searched in the caption");
    assert_eq!(q("Re:"), vec![b], "a word ending in a colon is searched in the title");
    assert_eq!(q("RE: brief"), vec![b], "case-insensitive, one token per word");
    assert_eq!(q("iso:>800"), vec![a], "known fields stay fielded");
    assert_eq!(q("11:45"), Vec::<PhotoId>::new(), "an unknown field matching no text finds nothing");
}

/// A plug-in field search still finds photos by that field, and a known field is never also
/// searched as text.
#[test]
fn plugin_and_known_fields_are_not_searched_as_text() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "a.jpg", "2026-05-01T10:00:00");
    let b = photo(&mut c, "b.jpg", "2026-04-01T10:00:00");
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("com.ex.jobs/job".to_string(), serde_json::json!("J-77"));
    c.set_plugin_fields(a, fields).unwrap();
    let mut m = c.photo(b).unwrap().meta.clone();
    m.caption = "rating:3 stars".into();
    c.apply(Op::SetMeta { id: b, meta: Box::new(m) }).unwrap();
    c.apply(Op::SetRating { id: a, rating: 3 }).unwrap();
    let q = |text: &str| c.query(&Filter { text: text.into(), ..Default::default() }, &Sort::default());
    assert_eq!(q("job:j-77"), vec![a], "a plug-in field by its key");
    assert_eq!(q("job:j-78"), Vec::<PhotoId>::new(), "a plug-in field value that doesn't match");
    assert_eq!(q("rating:3"), vec![a], "rating:3 tests the rating only, not a caption that says rating:3");
}

/// `f:` / `aperture:` and `focal:` are stored as `f32`; a typed value that isn't exact in binary
/// (2.8, 5.6) must still match, and the comparisons must count the photo's own value right.
#[test]
fn search_tokens_match_f32_aperture_and_focal() {
    let mut c = Catalog::new();
    let mut at = |name: &str, f: f32, focal: f32| {
        let id = photo(&mut c, name, "2026-05-01T10:00:00");
        let mut m = c.photo(id).unwrap().meta.clone();
        m.aperture = Some(f);
        m.focal_mm = Some(focal);
        c.apply(Op::SetMeta { id, meta: Box::new(m) }).unwrap();
        id
    };
    let (f28, f56, f4) = (at("f28.jpg", 2.8, 5.6), at("f56.jpg", 5.6, 50.0), at("f4.jpg", 4.0, 35.0));
    let q = |t: &str| {
        let mut v = c.query(&Filter { text: t.into(), ..Default::default() }, &Sort::default());
        v.sort();
        v
    };
    assert_eq!(q("f:2.8"), vec![f28], "f:2.8");
    assert_eq!(q("aperture:5.6"), vec![f56], "aperture:5.6");
    assert_eq!(q("aperture:=2.8"), vec![f28], "aperture:=2.8");
    assert_eq!(q("f:4"), vec![f4], "f:4");
    assert_eq!(q("focal:5.6"), vec![f28], "focal:5.6");
    assert!(q("f:2.9").is_empty(), "f:2.9");
    assert!(q("f:<2.8").is_empty(), "f:<2.8 leaves out f/2.8");
    assert_eq!(q("f:<=2.8"), vec![f28], "f:<=2.8 includes f/2.8");
    assert_eq!(q("f:>=2.8"), vec![f28, f56, f4], "f:>=2.8 includes f/2.8");
    assert_eq!(q("f:>2.8"), vec![f56, f4], "f:>2.8 leaves out f/2.8");
    assert_eq!(q("f:<=5.6"), vec![f28, f56, f4], "f:<=5.6 includes f/5.6");
    assert!(q("focal:<5.6").is_empty(), "focal:<5.6");
}

/// Issue #213: several values of one facet match any of them; facets still all have to match.
#[test]
fn multi_choice_facets_match_any_value_and_every_facet() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "a.jpg", "2026-05-01T10:00:00");
    let b = photo(&mut c, "b.jpg", "2026-05-02T10:00:00");
    let d = photo(&mut c, "d.jpg", "2026-05-03T10:00:00");
    for (id, kw, camera, lens) in [
        (a, "travel|italy", "Z 8", "NIKKOR Z 24-120mm f/4 S"),
        (b, "family", "D750", "AF-S NIKKOR 50mm f/1.8G"),
        (d, "cats", "Z 8", "NIKKOR Z 20mm f/1.8 S"),
    ] {
        let mut m = c.photo(id).unwrap().meta.clone();
        m.keywords = vec![kw.into()];
        m.camera = camera.into();
        m.lens = lens.into();
        c.apply(Op::SetMeta { id, meta: Box::new(m) }).unwrap();
    }
    let q = |f: Filter| c.query(&f, &Sort::default());
    let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(q(Filter { keywords: strs(&["travel", "family"]), ..Default::default() }), vec![a, b], "a parent keyword finds its children");
    assert_eq!(q(Filter { cameras: strs(&["d750", "nikon z 9"]), ..Default::default() }), vec![b]);
    assert_eq!(q(Filter { lenses: strs(&["Z 20mm", "50mm"]), ..Default::default() }), vec![b, d]);
    // across facets every one must match
    assert_eq!(q(Filter { keywords: strs(&["travel", "cats"]), cameras: strs(&["Z 8"]), lenses: strs(&["24-120"]), ..Default::default() }), vec![a]);
    // the single-value field and its list both apply
    assert_eq!(q(Filter { camera: Some("Z 8".into()), keywords: strs(&["family", "cats"]), ..Default::default() }), vec![d]);
    assert_eq!(q(Filter { keywords: Vec::new(), ..Default::default() }).len(), 3, "an empty list doesn't filter");
    let f = Filter { keywords: strs(&["travel", "family"]), ..Default::default() };
    assert!(f.describe().contains("keyword travel or family"), "{}", f.describe());
    // older filters (no lists) read back, and empty lists aren't written
    let json = serde_json::to_value(Filter { camera: Some("Z 8".into()), ..Default::default() }).unwrap();
    assert!(json.get("cameras").is_none(), "{json}");
    let back: Filter = serde_json::from_value(serde_json::json!({"keyword": "cats"})).unwrap();
    assert!(back.keywords.is_empty() && back.keyword.as_deref() == Some("cats"));
}

/// Issue #683: a metadata multi-select can demand all of its values (AND) instead of any of them (OR),
/// per column: keywords, people, cameras and lenses each carry their own operator.
#[test]
fn metadata_multi_select_matches_all_values_in_and_mode() {
    use lcc_meta::{Rect, Region, RegionKind};
    let region = |name: &str| Region {
        rect: Rect { x0: 0.4, y0: 0.4, x1: 0.6, y1: 0.6 },
        kind: RegionKind::Face,
        name: Some(name.to_string()),
        description: None,
    };
    let mut c = Catalog::new();
    let mut add = |name: &str, keywords: &[&str], camera: &str, lens: &str, people: &[&str]| {
        let id = c.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 1 }, name, "JPEG", 6000, 4000, "2026-09-30T10:00:00");
        p.meta.keywords = keywords.iter().map(|k| k.to_string()).collect();
        p.meta.camera = camera.into();
        p.meta.lens = lens.into();
        p.meta.regions = people.iter().map(|n| region(n)).collect();
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let both = add("both.jpg", &["landscape", "sunset"], "Z 8", "24-120", &["Amy", "Ben"]);
    let only_kw = add("kw.jpg", &["landscape"], "Z 8", "20mm", &["Amy"]);
    let other = add("other.jpg", &["portrait"], "D750", "50mm", &["Cleo"]);
    let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let q = |f: Filter| {
        let mut ids = c.query(&f, &Sort::default());
        ids.sort();
        ids
    };

    // OR stays the default, everywhere
    assert_eq!(
        q(Filter { keywords: strs(&["landscape", "portrait"]), ..Default::default() }),
        vec![both, only_kw, other],
        "the default is any of them"
    );
    // AND on keywords: only the photo tagged with both
    assert_eq!(q(Filter { keywords: strs(&["landscape", "sunset"]), keyword_op: MatchOp::All, ..Default::default() }), vec![both]);
    // AND on people
    assert_eq!(q(Filter { people: strs(&["amy", "ben"]), person_op: MatchOp::All, ..Default::default() }), vec![both]);
    assert_eq!(q(Filter { people: strs(&["amy", "ben"]), person_op: MatchOp::Any, ..Default::default() }), vec![both, only_kw]);
    // AND on lenses; a value no photo combines with the others empties the result
    assert_eq!(q(Filter { lenses: strs(&["24-120", "20mm"]), lens_op: MatchOp::All, ..Default::default() }), Vec::<PhotoId>::new());
    // the operators of different lists apply independently
    assert_eq!(
        q(Filter {
            keywords: strs(&["landscape", "portrait"]),
            keyword_op: MatchOp::Any,
            people: strs(&["amy", "ben"]),
            person_op: MatchOp::All,
            ..Default::default()
        }),
        vec![both]
    );
    // the summary says "and" in AND mode
    let f = Filter { keywords: strs(&["landscape", "sunset"]), keyword_op: MatchOp::All, ..Default::default() };
    assert!(f.describe().contains("keyword landscape and sunset"), "{}", f.describe());
    // the operators serialize (camelCase); like ratingOp, the default is written too
    let json = serde_json::to_value(&f).unwrap();
    assert_eq!(json.get("keywordOp"), Some(&serde_json::json!("all")));
    let plain = serde_json::to_value(Filter { keywords: strs(&["x"]), ..Default::default() }).unwrap();
    assert_eq!(plain.get("keywordOp"), Some(&serde_json::json!("any")), "{plain}");
    let back: Filter = serde_json::from_value(serde_json::json!({"people": ["amy"], "personOp": "all", "lensOp": "any"})).unwrap();
    assert_eq!(back.person_op, MatchOp::All);
    assert_eq!(back.lens_op, MatchOp::Any);
    // cameras have no operator: a photo has only one camera
    assert!(serde_json::from_value::<Filter>(serde_json::json!({"cameraOp": "all"})).is_err());
    assert_eq!(back.keyword_op, MatchOp::Any, "an absent operator reads as OR");
}

/// A `captured` string that is not ASCII (the Lightroom importer copies `captureTime` into an
/// `AddPhoto` op verbatim, and a JSON snapshot is valid UTF-8 whatever it holds) must not be
/// sliced on a byte index in `date_groups`: grouping counts the well-formed dates and ignores
/// the rest instead of panicking on a char boundary.
#[test]
fn date_groups_ignores_a_non_ascii_captured() {
    let mut c = Catalog::new();
    let ok = photo(&mut c, "ok.jpg", "2026-05-01T10:00:00");
    // `AddPhoto` takes the field as it is, like `library.importLightroom` building the op
    let bad = photo(&mut c, "bad.jpg", "日本語日本語");
    assert_ne!(ok, bad);
    let g = c.date_groups();
    assert_eq!(g.len(), 1, "the one well-formed date still groups");
    assert_eq!((g[0].year.as_str(), g[0].count), ("2026", 1));
    assert_eq!(g[0].days.iter().map(|d| d.1).sum::<usize>(), 1);
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
fn folder_paths() {
    use crate::query::in_folder;
    assert!(in_folder("/a/b/c.jpg", "/a/b", false));
    assert!(in_folder("/a/b/c.jpg", "/a/b/", false));
    assert!(!in_folder("/a/b/x/c.jpg", "/a/b", false));
    assert!(in_folder("/a/b/x/c.jpg", "/a/b", true));
    assert!(!in_folder("/a/bc/d.jpg", "/a/b", true), "a sibling with the same prefix");
    assert!(in_folder("C:\\Pics\\a.jpg", "C:\\Pics", false));
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
    // a newer build still can't decode it: the reason and the stamp of the version that decided
    // it are recorded together (upstream #662)
    let op = Op::SetContent {
        id: a,
        width: 6000,
        height: 4000,
        file_size: 1,
        content_hash: None,
        preview_only: Some("Nikon Huffman-compressed NEF".into()),
        preview_checked: "0.6.0".into(),
    };
    c.apply(op).unwrap();
    assert_eq!(c.photo(a).unwrap().preview_checked, "0.6.0");
    assert!(c.photo(a).unwrap().preview_only_stale(), "decided by another version");
    // Reload: the file decodes now → cleared, with its stamp; undo (through the journal's JSON)
    // restores both
    let op =
        Op::SetContent { id: a, width: 6016, height: 4000, file_size: 1, content_hash: None, preview_only: None, preview_checked: String::new() };
    let inv = c.apply(op).unwrap();
    assert!(c.photo(a).unwrap().develops_raw());
    assert_eq!(c.photo(a).unwrap().preview_checked, "");
    let inv: Op = serde_json::from_str(&serde_json::to_string(&inv).unwrap()).unwrap();
    c.apply(inv).unwrap();
    assert_eq!(c.photo(a).unwrap().preview_only.as_deref(), Some("Nikon Huffman-compressed NEF"));
    assert_eq!(c.photo(a).unwrap().preview_checked, "0.6.0");
    // this build's own decision is stamped with its version, and isn't stale
    let mut p = (**c.photo(a).unwrap()).clone();
    p.set_preview_only(Some("still not decoded".into()));
    assert_eq!(p.preview_checked, APP_VERSION);
    assert!(!p.preview_only_stale());
    p.set_preview_only(None);
    assert_eq!(p.preview_checked, "", "the stamp goes with the flag");
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
    use lcc_meta::{Rect, Region, RegionKind};
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
        names(Filter { person: Some("Sam".into()), ..rated.clone() }),
        names(Filter { rating: 1, ..Default::default() }),
        "the person filter is ignored"
    );
    assert_eq!(names(Filter { people: vec!["Sam".into()], ..rated }), names(Filter { rating: 1, ..Default::default() }), "and so is its list (#213)");
    assert_eq!(names(Filter { rating: 5, ..Default::default() }), vec![], "nobody in the filtered photos");
}

// ---- random sort: Given a seed, the order is a stable, reproducible shuffle of the matching photos

fn many(c: &mut Catalog, n: usize) -> Vec<PhotoId> {
    (0..n).map(|i| photo(c, &format!("p{i:03}.jpg"), &format!("2026-04-{:02}T10:00:00", i % 28 + 1))).collect()
}

fn random(seed: u64) -> Sort {
    Sort { key: SortKey::Random, seed, ..Default::default() }
}

#[test]
fn random_sort_is_a_stable_reproducible_shuffle() {
    let mut c = Catalog::new();
    assert!(c.query(&Filter::default(), &random(1)).is_empty(), "an empty library");
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
    let mut asc = c.query(&Filter::default(), &Sort { ascending: true, ..random(7) });
    asc.reverse();
    assert_eq!(c.query(&Filter::default(), &Sort { ascending: false, ..random(7) }), asc, "descending is the reverse");
    assert!(c.date_runs(&ids, SortKey::Random, GroupBy::Day).is_empty(), "a shuffle has no date headers");
    // adding a photo or editing metadata must not reshuffle the others, and the filter still applies
    let added = photo(&mut c, "new.jpg", "2026-05-01T10:00:00");
    c.apply(Op::SetRating { id: ids[4], rating: 5 }).unwrap();
    let after: Vec<PhotoId> = c.query(&Filter::default(), &random(7)).into_iter().filter(|id| *id != added).collect();
    assert_eq!(after, a);
    assert_eq!(c.query(&Filter { rating: 5, ..Default::default() }, &random(1)), vec![ids[4]]);
    // saved sorts from before the seed existed still load
    assert_eq!(serde_json::from_str::<Sort>(r#"{"key":"fileName","ascending":true}"#).unwrap().seed, 0);
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
    let lens = lcc_develop::EmbeddedLens { warp: Some(Default::default()), vignette: None, stages: Default::default() };
    let op = Op::SetEmbeddedLens { id: a, lens: Some(Box::new(lens)) };
    let op: Op = serde_json::from_str(&serde_json::to_string(&op).unwrap()).unwrap();
    let inv = c.apply(op).unwrap();
    assert_eq!(c.photo(a).unwrap().embedded_lens, Some(Box::new(lens)));
    let inv: Op = serde_json::from_str(&serde_json::to_string(&inv).unwrap()).unwrap();
    c.apply(inv).unwrap();
    assert_eq!(c.photo(a).unwrap().embedded_lens, None);
}

/// A decode or Reload stores the as-shot white a raw reads through its camera's matrices; the inverse (through the
/// journal's JSON) takes it back, and a value that is not a number is refused.
#[test]
fn set_as_shot_wb_is_undoable_journaled_and_checked() {
    let mut c = Catalog::default();
    let a = c.alloc_photo_id();
    let mut p = Photo::new(a, Source::File { path: "/x.arw".into() }, "x.arw", "ARW", 4000, 3000, "2026-10-06T00:00:00");
    p.as_shot_wb = Some((6500.0, 0.0));
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let op = Op::SetAsShotWb { id: a, wb: Some((4986.0, -2.0)) };
    let op: Op = serde_json::from_str(&serde_json::to_string(&op).unwrap()).unwrap();
    let inv = c.apply(op).unwrap();
    assert_eq!(c.photo(a).unwrap().as_shot_wb, Some((4986.0, -2.0)));
    let inv: Op = serde_json::from_str(&serde_json::to_string(&inv).unwrap()).unwrap();
    c.apply(inv).unwrap();
    assert_eq!(c.photo(a).unwrap().as_shot_wb, Some((6500.0, 0.0)));
    assert!(c.apply(Op::SetAsShotWb { id: a, wb: Some((f64::NAN, 0.0)) }).is_err());
    assert!(c.apply(Op::SetAsShotWb { id: PhotoId(999), wb: None }).is_err());
    assert_eq!(c.photo(a).unwrap().as_shot_wb, Some((6500.0, 0.0)));
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
    assert_eq!(p.iter().map(ToString::to_string).collect::<Vec<_>>(), vec!["rule 1: no collection 1".to_string()]);
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
    assert_eq!(rules.describe_with(&c), "keywords contains travel and (collection isn't “Excluded Photos” or collection is #9)");
    let f = Filter { rule_set: Some(rules), ..Default::default() };
    assert!(f.describe_with(&c).contains("collection isn't “Excluded Photos”"));
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
    assert!(start.elapsed() < std::time::Duration::from_secs(120), "took {:?}", start.elapsed());
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

    // Descending sort (newest first): dated photos first (newest to oldest), undated photos at the end
    let desc = c.query(&Filter::default(), &Sort { key: SortKey::CaptureDate, ascending: false, ..Default::default() });
    assert_eq!(desc, vec![p_dated2, p_dated1, p_undated1, p_undated2]);

    // Ascending sort (the default, oldest first): undated photos at the beginning, dated photos from oldest to newest
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

#[test]
fn saved_locations_apply_undo_and_privacy() {
    let mut c = Catalog::new();
    let home = lcc_geo::SavedLocation::new("Home", 48.85, 2.35, 300.0, true).unwrap();
    let inv = c.apply(Op::SetSavedLocation { name: "home".into(), location: Some(home.clone()) }).unwrap();
    assert_eq!(c.saved_location("HOME"), Some(&home));
    assert!(c.is_private_location((48.851, 2.351)));
    assert!(!c.is_private_location((48.9, 2.35)));
    c.apply(inv).unwrap();
    assert!(c.saved_location("home").is_none());
    assert!(!c.is_private_location((48.851, 2.351)));
    // invalid locations are refused, not stored
    let bad = lcc_geo::SavedLocation { radius: f64::NAN, ..home };
    assert!(c.apply(Op::SetSavedLocation { name: "x".into(), location: Some(bad) }).is_err());
    assert!(c.apply(Op::SetSavedLocation { name: " ".into(), location: None }).is_err());
}

#[test]
fn filter_by_face_count_and_matches_spec() {
    use crate::query::matches_face_spec;
    use lcc_meta::{Rect, Region, RegionKind};

    // 1. Test matches_face_spec
    assert!(matches_face_spec(0, "0"));
    assert!(matches_face_spec(0, "none"));
    assert!(!matches_face_spec(1, "0"));
    assert!(matches_face_spec(1, "1"));
    assert!(matches_face_spec(2, "=2"));
    assert!(matches_face_spec(3, "3+"));
    assert!(matches_face_spec(4, "3+"));
    assert!(!matches_face_spec(2, "3+"));
    assert!(matches_face_spec(3, ">=3"));
    assert!(matches_face_spec(4, ">3"));
    assert!(!matches_face_spec(3, ">3"));
    assert!(matches_face_spec(2, "<=2"));
    assert!(matches_face_spec(1, "<2"));
    assert!(matches_face_spec(2, "1-3"));
    assert!(matches_face_spec(1, "1-3"));
    assert!(matches_face_spec(3, "1-3"));
    assert!(!matches_face_spec(4, "1-3"));
    assert!(matches_face_spec(2, "1..3"));
    assert!(matches_face_spec(2, "any"));
    assert!(!matches_face_spec(0, "any"));
    // reversed ranges cover the same counts; unreadable specs are rejected where they're read
    assert!(matches_face_spec(3, "5-2") && matches_face_spec(2, "5..2") && !matches_face_spec(6, "5-2"));
    for bad in ["abc", "3 faces", "<0", "-", "+", "1-x", ">", "99999999999999999999999"] {
        assert!(!matches_face_spec(0, bad) && !matches_face_spec(3, bad), "{bad}");
        let r: std::result::Result<Filter, _> = serde_json::from_value(serde_json::json!({"faces": bad}));
        assert!(r.is_err_and(|e| e.to_string().contains("faces")), "{bad} must be rejected");
    }

    // 2. Test deserialization of Filter with faces
    let f1: Filter = serde_json::from_value(serde_json::json!({"faces": 2})).unwrap();
    assert_eq!(f1.faces.as_deref(), Some("2"));

    let f2: Filter = serde_json::from_value(serde_json::json!({"faces": "3+"})).unwrap();
    assert_eq!(f2.faces.as_deref(), Some("3+"));

    let f3: Filter = serde_json::from_value(serde_json::json!({"faces": null})).unwrap();
    assert_eq!(f3.faces, None);

    let f4: Filter = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(f4.faces, None);

    // 3. Catalog query with face count
    let mut c = Catalog::new();
    let region = |name: Option<&str>, kind: RegionKind| Region {
        rect: Rect { x0: 0.1, y0: 0.1, x1: 0.2, y1: 0.2 },
        kind,
        name: name.map(str::to_string),
        description: None,
    };

    let make_photo = |id: u64, regions: Vec<Region>| {
        let mut p = Photo::new(
            PhotoId(id),
            crate::Source::File { path: format!("/x/test_{id}.jpg") },
            &format!("test_{id}.jpg"),
            "JPEG",
            100,
            100,
            "2026-04-12T10:00:00Z",
        );
        p.captured = Some("2026-04-12T10:00:00Z".into());
        p.meta.regions = regions;
        p
    };

    let p0 = make_photo(1, vec![]);
    let p1 = make_photo(2, vec![region(Some("Alice"), RegionKind::Face)]);
    let p2 = make_photo(3, vec![region(Some("Alice"), RegionKind::Face), region(Some("Bob"), RegionKind::Face)]);
    let p3 = make_photo(
        4,
        vec![
            region(Some("Alice"), RegionKind::Face),
            region(None, RegionKind::Face), // unnamed face
            region(Some("Charlie"), RegionKind::Face),
            region(Some("Pet"), RegionKind::Pet), // pet should not count towards face_count
        ],
    );

    assert_eq!(p0.face_count(), 0);
    assert_eq!(p1.face_count(), 1);
    assert_eq!(p2.face_count(), 2);
    assert_eq!(p3.face_count(), 3);

    for p in [p0, p1, p2, p3] {
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }

    let q = |f: &Filter| c.query(f, &Sort::default());

    // Exact 1 face
    let f_one = Filter { faces: Some("1".into()), ..Default::default() };
    assert_eq!(q(&f_one), vec![PhotoId(2)]);

    // Exact 0 faces
    let f_zero = Filter { faces: Some("0".into()), ..Default::default() };
    assert_eq!(q(&f_zero), vec![PhotoId(1)]);

    // 2+ faces
    let f_two_plus = Filter { faces: Some("2+".into()), ..Default::default() };
    assert_eq!(q(&f_two_plus), vec![PhotoId(3), PhotoId(4)]);

    // Range 1-2 faces
    let f_range = Filter { faces: Some("1-2".into()), ..Default::default() };
    assert_eq!(q(&f_range), vec![PhotoId(2), PhotoId(3)]);

    // 4. Token search in text: faces:1, faces:3+, faces:none
    let f_tok1 = Filter { text: "faces:1".into(), ..Default::default() };
    assert_eq!(q(&f_tok1), vec![PhotoId(2)]);

    let f_tok_none = Filter { text: "faces:none".into(), ..Default::default() };
    assert_eq!(q(&f_tok_none), vec![PhotoId(1)]);

    let f_tok_plus = Filter { text: "faces:>=2".into(), ..Default::default() };
    assert_eq!(q(&f_tok_plus), vec![PhotoId(3), PhotoId(4)]);

    // 5. Describe
    assert_eq!(f_two_plus.describe(), "faces 2+");
}

/// What the catalog remembers of its latest ops: which photos each changed and what of them, as long as that can be
/// said photo by photo.
#[test]
fn the_catalog_remembers_which_photos_its_latest_ops_changed_and_what_of_them() {
    let mut c = Catalog::default();
    let photo = |c: &mut Catalog, name: &str| {
        let id = c.alloc_photo_id();
        c.apply(Op::AddPhoto {
            photo: Box::new(Photo::new(id, Source::File { path: format!("/p/{name}") }, name, "JPEG", 60, 40, "2026-01-01T10:00:00")),
        })
        .unwrap();
        id
    };
    let (a, b) = (photo(&mut c, "a.jpg"), photo(&mut c, "b.jpg"));
    let start = c.revision;
    assert_eq!(c.touched_since(start), Some(vec![]));
    assert_eq!(c.aspects_since(start), Some(Aspects::NONE));
    // a rating, then a flag on another photo, as one batch
    c.apply(Op::SetRating { id: a, rating: 3 }).unwrap();
    assert_eq!(c.touched_since(start), Some(vec![a]));
    assert_eq!(c.aspects_since(start), Some(Aspects::RATING));
    let after_rating = c.revision;
    c.apply(Op::Batch { ops: vec![Op::SetFlag { id: b, flag: Flag::Pick }, Op::SetRating { id: a, rating: 4 }] }).unwrap();
    assert_eq!(c.touched_since(start), Some(vec![a, b]), "each once");
    assert_eq!(c.touched_since(after_rating), Some(vec![a, b]));
    assert_eq!(c.aspects_since(after_rating), Some(Aspects::FLAG.with(Aspects::RATING)));
    assert_eq!(c.touched_since(c.revision), Some(vec![]));
    // a photo that goes counts for everything
    let before = c.revision;
    c.apply(Op::RemovePhoto { id: b }).unwrap();
    assert_eq!(c.touched_since(before), Some(vec![b]));
    assert!(c.aspects_since(before).is_some_and(|x| x.any_of(Aspects::PRESENCE) && x.any_of(Aspects::FILE)));
    // an op that changes more than photos of its own: nothing is said of what came before it
    let before = c.revision;
    let album = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album::new(album, "Trip") }).unwrap();
    assert_eq!(c.touched_since(before), None);
    assert_eq!(c.aspects_since(start), None);
    assert_eq!(c.touched_since(c.revision), Some(vec![]), "from after it, it is known again");
    // a revision that is not this catalog's past
    assert_eq!(c.touched_since(c.revision + 5), None);
    // a revision set from outside (a library just loaded): nothing is known of how the catalog got there, and after
    // the next op nothing of before it
    let old = c.revision;
    c.revision = 7 << 32;
    assert_eq!(c.touched_since(old), None);
    assert_eq!(c.aspects_since(c.revision), None);
    let loaded = c.revision;
    c.apply(Op::SetRating { id: a, rating: 1 }).unwrap();
    assert_eq!(c.touched_since(loaded), Some(vec![a]));
    assert_eq!(c.touched_since(old), None);
    // an op that fails changes nothing and is not remembered
    let before = c.revision;
    assert!(c.apply(Op::SetRating { id: PhotoId(9_999), rating: 1 }).is_err());
    assert_eq!(c.revision, before);
    assert_eq!(c.touched_since(before), Some(vec![]));
    c.apply(Op::SetFlag { id: a, flag: Flag::Pick }).unwrap();
    assert_eq!(c.touched_since(before), Some(vec![a]));
    assert!(c.aspects_since(before).is_some_and(|x| x.any_of(Aspects::FLAG)));
    // a batch that fails midway is undone as a whole; a batch in a batch is remembered photo by photo; a batch
    // with an op that changes more than photos is not
    let before = c.revision;
    assert!(c.apply(Op::Batch { ops: vec![Op::SetRating { id: a, rating: 5 }, Op::SetRating { id: PhotoId(9_999), rating: 1 }] }).is_err());
    assert_eq!((c.revision, c.photo(a).map(|p| p.rating)), (before, Some(1)));
    c.apply(Op::Batch { ops: vec![Op::Batch { ops: vec![Op::SetRating { id: a, rating: 2 }] }] }).unwrap();
    // (the failed batch is not remembered: only the batch that worked)
    assert_eq!(c.touched_since(before), Some(vec![a]));
    let before = c.revision;
    c.apply(Op::Batch { ops: vec![Op::SetRating { id: a, rating: 3 }, Op::RenameAlbum { id: album, name: "Journey".into() }] }).unwrap();
    assert_eq!(c.touched_since(before), None);
    // the keyword list changes no photo: what was touched stays known, and the list is said to have changed
    let before = c.revision;
    c.apply(Op::SetKeyword { path: "travel|Rome".into(), info: Some(Default::default()) }).unwrap();
    assert_eq!(c.touched_since(before), Some(vec![]));
    assert_eq!(c.aspects_since(before), Some(Aspects::KEYWORD_LIST));
    assert_eq!(c.aspects_since(c.revision), Some(Aspects::NONE), "and from after it, not");
    // more ops than are remembered: the oldest are answered with "anything"
    let long_ago = c.revision;
    for i in 0..9000u32 {
        c.apply(Op::SetRating { id: a, rating: (i % 5) as u8 }).unwrap();
    }
    assert_eq!(c.touched_since(long_ago), None);
    assert_eq!(c.touched_since(c.revision - 10), Some(vec![a]));
}

/// An op that fails leaves nothing behind/// that does not even exist), and, for a batch rolled back, none of what its first ops had touched.
#[test]
fn an_op_that_fails_leaves_no_touched_photos_behind() {
    let mut c = Catalog::default();
    let add = |c: &mut Catalog, name: &str| {
        let id = c.alloc_photo_id();
        c.apply(Op::AddPhoto {
            photo: Box::new(Photo::new(id, Source::File { path: format!("/p/{name}") }, name, "JPEG", 60, 40, "2026-01-01T10:00:00")),
        })
        .unwrap();
        id
    };
    let (a, b) = (add(&mut c, "a.jpg"), add(&mut c, "b.jpg"));
    let missing = PhotoId(9_999);
    let start = c.revision;
    // every op that names a photo, on one that does not exist
    for op in [Op::SetRating { id: missing, rating: 1 }, Op::SetFlag { id: missing, flag: Flag::Pick }, Op::RemovePhoto { id: missing }] {
        assert!(c.apply(op).is_err());
        assert_eq!(c.revision, start, "a failed op makes no revision");
        assert_eq!(c.touched_since(start), Some(vec![]));
        assert_eq!(c.aspects_since(start), Some(Aspects::NONE));
    }
    // an op that changes more than photos and fails does not make earlier revisions unknown
    assert!(c.apply(Op::RenameAlbum { id: AlbumId(77), name: "x".into() }).is_err());
    assert_eq!(c.touched_since(start), Some(vec![]));
    // a batch that fails on its second op names nothing of its first, nor of the rollback
    assert!(c.apply(Op::Batch { ops: vec![Op::SetRating { id: a, rating: 5 }, Op::SetRating { id: missing, rating: 1 }] }).is_err());
    assert_eq!(c.revision, start);
    assert_eq!(c.touched_since(start), Some(vec![]));
    assert_eq!(c.aspects_since(start), Some(Aspects::NONE));
    // a keyword op that fails, alone or inside a batch that is rolled back, does not say the keyword list changed
    assert!(c.apply(Op::SetKeyword { path: " | ".into(), info: Some(Default::default()) }).is_err());
    assert_eq!(c.aspects_since(start), Some(Aspects::NONE));
    let ok = Op::SetKeyword { path: "travel|Rome".into(), info: Some(Default::default()) };
    assert!(c.apply(Op::Batch { ops: vec![ok, Op::SetRating { id: missing, rating: 1 }] }).is_err());
    assert_eq!(c.revision, start);
    assert_eq!(c.aspects_since(start), Some(Aspects::NONE));
    assert_eq!(c.photo(a).map(|p| p.rating), Some(0));
    // a batch that ends in a failed removal of a photo that exists
    assert!(c.apply(Op::Batch { ops: vec![Op::RemovePhoto { id: b }, Op::RemovePhoto { id: b }] }).is_err());
    assert!(c.photo(b).is_some());
    assert_eq!(c.touched_since(start), Some(vec![]));
    // what succeeded after the failures is remembered, and only that
    c.apply(Op::SetRating { id: a, rating: 2 }).unwrap();
    assert_eq!(c.touched_since(start), Some(vec![a]));
    assert_eq!(c.aspects_since(start), Some(Aspects::RATING));
}

/// A batch the rollback of which fails halfway leaves the catalog changed (here a stack is lost), so what the
/// catalog remembers must not say that nothing changed: anything may have.
#[test]
fn a_batch_that_cannot_be_rolled_back_whole_makes_earlier_changes_unknown() {
    let mut c = Catalog::default();
    let mut ids = Vec::new();
    for name in ["a.jpg", "b.jpg"] {
        let id = c.alloc_photo_id();
        let photo = Photo::new(id, Source::File { path: format!("/p/{name}") }, name, "JPEG", 60, 40, "2026-01-01T10:00:00");
        c.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        ids.push(id);
    }
    let stack = c.alloc_stack_id();
    c.apply(Op::AddStack { stack: Stack { id: stack, photos: ids.clone(), collapsed: false } }).unwrap();
    let start = c.revision;
    // the photo goes, then its stack, then an op fails: putting the stack back needs the photo, which comes later
    let failing =
        Op::Batch { ops: vec![Op::RemovePhoto { id: ids[0] }, Op::RemoveStack { id: stack }, Op::SetRating { id: PhotoId(9_999), rating: 1 }] };
    assert!(c.apply(failing).is_err());
    assert_eq!(c.revision, start + 1, "the catalog changed, so what is kept for the revision before is out of date");
    assert!(c.photo(ids[0]).is_some() && c.stack(stack).is_none(), "the rollback went only part of the way");
    assert_eq!(c.touched_since(start), None, "so nothing is said of what changed");
    assert_eq!(c.touched_since(c.revision), Some(vec![]), "from after it, it is known again");
}

/// A rating filter never lets an unrated photo through "at least one star", and the text search's
/// `rating:>=N` / `rating:<=N` ask what the filter bar's ≥ / ≤ ask.
#[test]
fn rating_filter_ops_and_unrated_photos() {
    let mut c = Catalog::new();
    let unrated = photo(&mut c, "a.jpg", "2026-01-01T10:00:00");
    let one = photo(&mut c, "b.jpg", "2026-01-02T10:00:00");
    let three = photo(&mut c, "c.jpg", "2026-01-03T10:00:00");
    c.apply(Op::SetRating { id: one, rating: 1 }).unwrap();
    c.apply(Op::SetRating { id: three, rating: 3 }).unwrap();
    let q = |f: Filter| c.query(&f, &Sort::default());
    let r = |rating, rating_op| Filter { rating, rating_op, ..Default::default() };
    assert_eq!(q(r(1, RatingOp::AtLeast)), vec![one, three]);
    assert_eq!(q(r(1, RatingOp::Exactly)), vec![one]);
    assert_eq!(q(r(1, RatingOp::AtMost)), vec![unrated, one], "Lightroom's ≤ counts unrated photos as below one star");
    assert_eq!(q(r(3, RatingOp::Exactly)), vec![three]);
    let text = |t: &str| q(Filter { text: t.into(), ..Default::default() });
    assert_eq!(text("rating:>=1"), vec![one, three]);
    assert_eq!(text("rating:>0"), vec![one, three]);
    assert_eq!(text("rating:<=1"), vec![unrated, one]);
    assert_eq!(text("rating:1"), vec![one]);
    assert!(text("rating:>=x").is_empty(), "not a number: nothing matches, no panic");
}

/// The label filter's "no label" (issue #1192): `labelsNone` alone shows only the unlabeled photos,
/// it ANDs with the colour multi-select, the text search's `label:none` asks for the same, and it
/// is part of the quick culling filters the filter bar counts around.
#[test]
fn label_filter_none() {
    let mut c = Catalog::new();
    let plain = photo(&mut c, "a.jpg", "2026-01-01T10:00:00");
    let red = photo(&mut c, "b.jpg", "2026-01-02T10:00:00");
    let green = photo(&mut c, "c.jpg", "2026-01-03T10:00:00");
    c.apply(Op::SetLabel { id: red, label: Some(ColorLabel::Red) }).unwrap();
    c.apply(Op::SetLabel { id: green, label: Some(ColorLabel::Green) }).unwrap();
    let q = |f: Filter| c.query(&f, &Sort::default());
    let f = Filter { labels_none: true, ..Default::default() };
    assert_eq!(q(f.clone()), vec![plain]);
    assert_eq!(
        q(Filter { labels: vec![ColorLabel::Red], labels_none: true, ..Default::default() }),
        Vec::<PhotoId>::new(),
        "no photo carries both nothing and red"
    );
    let text = |t: &str| q(Filter { text: t.into(), ..Default::default() });
    assert_eq!(text("label:none"), vec![plain]);
    assert_eq!(text("color:none"), vec![plain]);
    assert!(text("label:red").contains(&red) && !text("label:red").contains(&plain));
    // the chip and smart-album summary name it
    assert!(f.describe().contains("label none"));
    // the quick culling filters the filter bar counts without include it
    assert!(!f.without_quick().labels_none);
    let t = c.tally(&f);
    assert_eq!(t.total, 1);
}

/// Photos with one import time and capture time, as one import batch or a burst gives them, added
/// with ids that run opposite to their names.
fn batch_with_reversed_ids(c: &mut Catalog) -> Vec<PhotoId> {
    let names = ["DSC_0001.ARW", "dsc_0002.ARW", "DSC_0003.ARW", "DSC_0004.ARW"];
    let mut ids = Vec::new();
    for (n, name) in names.iter().enumerate() {
        let id = PhotoId(1000 - n as u64);
        let mut p = Photo::new(id, Source::Demo { scene: 1 }, name, "ARW", 10, 10, "2026-10-10T09:00:00");
        p.captured = Some("2026-10-01T12:00:00".into());
        p.rating = 3;
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    ids // in name order
}

/// One import batch, a burst within one second, equal ratings, edit times and sizes all sort by file name, not id;
/// descending is the exact reverse.
#[test]
fn photos_that_tie_on_any_key_sort_by_file_name_not_id() {
    let mut c = Catalog::new();
    let by_name = batch_with_reversed_ids(&mut c);
    for key in [SortKey::ImportDate, SortKey::CaptureDate, SortKey::Rating, SortKey::EditDate, SortKey::FileSize, SortKey::Custom] {
        let up = c.query(&Filter::default(), &Sort { key, ascending: true, ..Default::default() });
        assert_eq!(up, by_name, "{key:?}");
        let down = c.query(&Filter::default(), &Sort { key, ascending: false, ..Default::default() });
        assert_eq!(down, by_name.iter().rev().copied().collect::<Vec<_>>(), "{key:?} descending");
    }
}

/// A photo whose name changes moves among the photos it ties with, in a library large enough that
/// its sort orders are kept between queries.
#[test]
fn a_renamed_photo_moves_among_the_photos_it_ties_with() {
    let mut c = Catalog::new();
    for i in 0..3000u64 {
        let id = c.alloc_photo_id();
        let p =
            Photo::new(id, Source::File { path: format!("/pics/IMG_{i:05}.jpg") }, &format!("IMG_{i:05}.jpg"), "JPEG", 10, 10, "2026-01-01T00:00:00");
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let sort = Sort { key: SortKey::ImportDate, ascending: true, ..Default::default() };
    let before = c.query(&Filter::default(), &sort);
    let last = *before.last().unwrap();
    c.apply(Op::SetFile { id: last, file_name: "AAA.jpg".into(), source: Source::File { path: "/pics/AAA.jpg".into() } }).unwrap();
    assert_eq!(c.query(&Filter::default(), &sort).first(), Some(&last), "first by name now");
}

/// Search Photos finds named people in free text, and a quoted value keeps a name of several words
/// together: `person:"Jane Doe"` (issue #1449). Malformed quotes narrow the result or change nothing.
#[test]
fn search_text_finds_people_and_quoted_values() {
    use lcc_meta::{Rect, Region, RegionKind};
    let region = |name: &str, kind: RegionKind| Region {
        rect: Rect { x0: 0.4, y0: 0.4, x1: 0.6, y1: 0.6 },
        kind,
        name: Some(name.to_string()),
        description: None,
    };
    let mut c = Catalog::new();
    let mut add = |name: &str, regions: Vec<Region>, keywords: &[&str]| {
        let id = c.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 1 }, name, "JPEG", 6000, 4000, "2026-09-30T10:00:00");
        p.meta.regions = regions;
        p.meta.keywords = keywords.iter().map(|k| k.to_string()).collect();
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let jane = add("a.jpg", vec![region("Jane Doe", RegionKind::Face), region("Rex", RegionKind::Pet)], &[]);
    let both = add("b.jpg", vec![region("Jane Roe", RegionKind::Face), region("John Doe", RegionKind::Face)], &[]);
    let zoe = add("c.jpg", vec![region("Zoë Ångström", RegionKind::Face)], &["New York"]);
    let none = add("d.jpg", vec![], &["york minster"]);
    let q = |text: &str| {
        let mut ids = c.query(&Filter { text: text.into(), ..Default::default() }, &Sort::default());
        ids.sort();
        ids
    };

    // free text: a person's name, any case, any part of it; pets are not people
    assert_eq!(q("jane"), vec![jane, both]);
    assert_eq!(q("DOE"), vec![jane, both]);
    assert_eq!(q("ångström"), vec![zoe]);
    assert!(q("rex").is_empty(), "a pet is not a person");
    // words are still ANDed: b.jpg has a Jane and a Doe, a quoted phrase asks for them together
    assert_eq!(q("jane doe"), vec![jane, both]);
    assert_eq!(q("\"jane doe\""), vec![jane]);

    // fielded: the exact name, of several words when quoted
    assert_eq!(q("person:\"Jane Doe\""), vec![jane]);
    assert_eq!(q("who:\"jane roe\""), vec![both]);
    assert_eq!(q("person:\"Zoë Ångström\""), vec![zoe]);
    assert!(q("person:jane").is_empty(), "person: is the whole name, as before");
    assert!(q("person:Jane Doe").is_empty(), "unquoted, the second word is its own token, as before");
    assert_eq!(q("keyword:\"new york\""), vec![zoe], "other fields take quoted values too");
    assert_eq!(q("person:\"Jane Doe\" a.jpg"), vec![jane], "tokens after a quoted one still count");

    // malformed input never widens the match and never panics
    assert_eq!(q("person:\"Jane Doe"), vec![jane], "an unclosed quote runs to the end");
    assert!(q("person:\"Jane Doe a.jpg").is_empty(), "…and takes the rest of the text with it");
    assert!(q("person:\"\"").is_empty());
    assert!(q("person:").is_empty());
    assert_eq!(q("\""), vec![jane, both, zoe, none], "a lone quote is an empty phrase, like an empty search");
    assert_eq!(q("\"\" jane"), vec![jane, both]);
    assert!(q("\"jane:doe\"").is_empty(), "a quoted phrase is free text even with a colon");
    assert!(q("jane\\\"").is_empty(), "a backslash is an ordinary character");
    let long = format!("person:\"{}", "é ".repeat(50_000));
    assert!(q(&long).is_empty());
    assert_eq!(q("   \t jane \u{3000} "), vec![jane, both], "any whitespace separates tokens");
}

#[test]
fn search_boolean_tokens_reject_invalid_values() {
    let mut c = Catalog::new();
    photo(&mut c, "original.jpg", "2026-05-01T10:00:00");
    let q = |text: &str| c.query(&Filter { text: text.into(), ..Default::default() }, &Sort::default());
    for field in ["edited", "copy", "virtual", "video"] {
        for value in ["", "garbage", "maybe", "0", "1", "trueish"] {
            assert!(q(&format!("{field}:{value}")).is_empty(), "{field}:{value}");
        }
    }
    assert_eq!(q("edited:no").len(), 1, "unedited photo matches edited:no");
    assert!(q("edited:yes").is_empty());
}

/// Any photo can carry a Copy Name (a Lightroom catalog gives some a short title there): only a virtual copy is shown
/// with its name as one, and Import from Catalog matches a photo that isn't a copy by its file alone.
#[test]
fn a_copy_name_on_a_photo_that_is_not_a_virtual_copy_does_not_make_it_one() {
    let add = |c: &mut Catalog, name: Option<&str>, copy_of: Option<PhotoId>| {
        let id = c.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: "/pics/trip/a.jpg".into() }, "a.jpg", "JPEG", 4, 3, "2026-01-01T00:00:00");
        p.copy_name = name.map(str::to_string);
        p.copy_of = copy_of;
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let mut mine = Catalog::new();
    let master = add(&mut mine, Some("Web title"), None);
    let copy = add(&mut mine, Some("Mono"), Some(master));
    assert_eq!(mine.photo(master).unwrap().virtual_copy_name(), None, "a master with a Copy Name is no virtual copy");
    assert_eq!(mine.photo(copy).unwrap().virtual_copy_name(), Some("Mono"));
    // the other library has the same file with no Copy Name (imported before it was kept), and the same copy
    let mut other = Catalog::new();
    let other_master = add(&mut other, None, None);
    add(&mut other, Some("Mono"), Some(other_master));
    let plan = transfer::plan_import(&mine, &other);
    assert!(plan.new_photos.is_empty(), "the file and the copy match whatever the master's Copy Name: {plan:?}");
    // a copy is still told apart by its name
    add(&mut other, Some("Sepia"), Some(other_master));
    assert_eq!(transfer::plan_import(&mine, &other).new_photos.len(), 1);
}
