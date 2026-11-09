use std::sync::Arc;

use lightcraft_develop::DevelopSettings;
use serde_json::json;

use super::*;

fn photo(c: &mut Catalog, name: &str, date: &str) -> PhotoId {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::Demo { scene: 1 }, name, "JPEG", 6000, 4000, "2026-09-30T10:00:00");
    p.captured = Some(date.to_string());
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

/// ORF probes record both supported and preview-only outcomes, without changing other formats.
#[test]
fn orf_support_checks_keep_a_stamp_even_when_sensor_data_decodes() {
    let mut p = Photo::new(PhotoId(1), Source::File { path: "a.orf".into() }, "a.orf", "ORF", 4, 3, "2026-01-01");
    p.kind = MediaKind::Raw;
    assert!(p.preview_only_stale(), "legacy supported ORF needs the withdrawal recheck");
    p.set_preview_only(None);
    assert_eq!(p.preview_checked, APP_VERSION);
    assert!(!p.preview_only_stale());
    assert!(p.develops_raw(), "supported uncompressed ORF remains sensor-decodable");
    p.set_preview_only(Some("Olympus compressed ORF".into()));
    assert_eq!(p.preview_checked, APP_VERSION);
    assert!(!p.preview_only_stale());
    assert!(!p.develops_raw());
    p.format = "DNG".into();
    p.set_preview_only(None);
    assert!(p.preview_checked.is_empty(), "other supported formats keep their existing semantics");
    assert!(!p.preview_only_stale());
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

/// A search word with a colon whose left part is not a known field ("10:30", "Re:") is plain
/// text, not a fielded token that matches nothing.
/// `flags` (#2588) matches any of its flags; a filter saved before it, with the single `flag`, reads
/// and filters as it did, and a filter without `flags` is written without the key.
#[test]
fn filter_flags_any_of_and_the_old_single_flag() {
    let mut c = Catalog::new();
    let a = photo(&mut c, "a.jpg", "2026-01-01T10:00:00");
    let b = photo(&mut c, "b.jpg", "2026-01-02T10:00:00");
    let d = photo(&mut c, "d.jpg", "2026-01-03T10:00:00");
    c.apply(Op::SetFlag { id: a, flag: Flag::Pick }).unwrap();
    c.apply(Op::SetFlag { id: d, flag: Flag::Reject }).unwrap();
    let f = Filter { flags: vec![Flag::Pick, Flag::None], ..Default::default() };
    assert_eq!(c.query(&f, &Sort::default()), vec![b, a]);
    let f = Filter { flags: vec![Flag::Reject, Flag::None], ..Default::default() };
    assert_eq!(c.query(&f, &Sort::default()), vec![d, b]);
    let saved: Filter = serde_json::from_value(serde_json::json!({"text": "", "rating": 0, "flag": "reject", "label": null})).unwrap();
    assert!(saved.flags.is_empty());
    assert_eq!(c.query(&saved, &Sort::default()), vec![d]);
    let written = serde_json::to_value(&saved).unwrap();
    assert_eq!(written.get("flag"), Some(&serde_json::json!("reject")));
    assert!(written.get("flags").is_none(), "{written}");
}