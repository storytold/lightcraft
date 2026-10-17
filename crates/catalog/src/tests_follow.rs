//! Following edits from what they touched ([`Catalog::touched_since`], [`Catalog::follow`],
//! [`crate::tally::AlbumTally`]) gives what working everything out again gives: through random
//! photo-local edits, edits that change more than photos (which are not followed), every sort of
//! this fork's query (its sort cache and Custom Order included) and a held view.

use std::collections::HashSet;

use proptest::prelude::*;

use super::*;
use crate::tally::AlbumTally;

const PHOTOS: u64 = 12;
const SMART: AlbumId = AlbumId(1);
const PLAIN: AlbumId = AlbumId(2);

fn library() -> Catalog {
    let mut c = Catalog::new();
    for i in 0..PHOTOS {
        // names equal but for case, capture times shared and missing: ties for the id to break
        let name = ["a.jpg", "A.jpg", "b.JPG", "Été.jpg", "été.jpg", "c.jpg"][(i % 6) as usize];
        let mut p = Photo::new(PhotoId(i + 1), Source::File { path: format!("/p/{}/{name}", i % 3) }, name, "JPEG", 60, 40, "2026-01-01T00:00:00");
        p.captured = (i % 4 != 0).then(|| format!("2025-0{}-01T00:00:00", 1 + i % 3));
        p.rating = (i % 6) as u8;
        p.file_size = i % 5;
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let mut smart = Album::new(SMART, "Good");
    smart.smart = Some(Box::new(Filter { rating: 3, ..Default::default() }));
    c.apply(Op::AddAlbum { album: smart }).unwrap();
    let mut plain = Album::new(PLAIN, "Picked by hand");
    plain.photos = vec![PhotoId(5), PhotoId(2), PhotoId(9), PhotoId(11)];
    c.apply(Op::AddAlbum { album: plain }).unwrap();
    c.apply(Op::SetAlbumPhotoOrder { id: PLAIN, order: vec![PhotoId(9), PhotoId(5)] }).unwrap();
    c
}

fn edit(c: &Catalog, kind: u8, n: u64, v: u8) -> Op {
    let id = PhotoId(n % PHOTOS + 1);
    match kind % 9 {
        0 => Op::SetRating { id, rating: v % 6 },
        1 => Op::SetFlag { id, flag: [Flag::None, Flag::Pick, Flag::Reject][(v % 3) as usize] },
        2 => Op::SetLabel { id, label: ColorLabel::ALL.get((v % 6) as usize).copied() },
        3 => {
            let mut meta = c.photo(id).map(|p| p.meta.clone()).unwrap_or_default();
            meta.keywords = if v.is_multiple_of(2) { vec!["trip".into()] } else { Vec::new() };
            Op::SetMeta { id, meta: Box::new(meta) }
        }
        4 => Op::SetCaptured { id, captured: (!v.is_multiple_of(3)).then(|| format!("2025-0{}-01T00:00:00", 1 + v % 4)) },
        5 => Op::SetDeleted { id, deleted: v.is_multiple_of(2) },
        // several photos in one step
        6 => Op::Batch { ops: vec![Op::SetRating { id, rating: v % 6 }, Op::SetFlag { id: PhotoId((n + 1) % PHOTOS + 1), flag: Flag::Pick }] },
        // more than photos of their own: not followed
        7 => Op::SetAlbumPhotos { id: PLAIN, photos: (1..=PHOTOS).filter(|i| (i + u64::from(v)).is_multiple_of(3)).map(PhotoId).collect() },
        _ => Op::SetLabelName { label: ColorLabel::Red, name: v.is_multiple_of(2).then(|| "Best".into()) },
    }
}

fn views() -> Vec<(Filter, Sort)> {
    let filters = [
        Filter::default(),
        Filter { rating: 3, ..Default::default() },
        Filter { flag: Some(Flag::Pick), ..Default::default() },
        Filter { album: Some(SMART), ..Default::default() },
        Filter { album: Some(PLAIN), ..Default::default() },
        Filter { keyword: Some("trip".into()), ..Default::default() },
    ];
    let keys = [
        SortKey::CaptureDate,
        SortKey::ImportDate,
        SortKey::EditDate,
        SortKey::FileName,
        SortKey::Rating,
        SortKey::FileSize,
        SortKey::Random,
        SortKey::Custom,
    ];
    let mut out = Vec::new();
    for f in &filters {
        for key in keys {
            for ascending in [true, false] {
                out.push((f.clone(), Sort { key, ascending, seed: 5, ..Default::default() }));
            }
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn followed_views_and_counts_are_what_a_full_pass_gives(steps in proptest::collection::vec((0u8..9, 0u64..64, any::<u8>()), 1..30)) {
        let mut c = library();
        let views = views();
        // the views' lists, held views' lists (in the library, not deleted), and the catalog revision of both
        let within = Filter::default();
        let mut lists: Vec<Vec<PhotoId>> = views.iter().map(|(f, s)| c.query(f, s)).collect();
        let mut held = lists.clone();
        let mut revision = c.revision;
        let mut tally = AlbumTally::of(&c);
        for (kind, n, v) in steps {
            let op = edit(&c, kind, n, v);
            let _ = c.apply(op);
            let touched = c.touched_since(revision);
            for (i, (f, s)) in views.iter().enumerate() {
                let whole = c.query(f, s);
                let keep: HashSet<PhotoId> = held[i].iter().copied().collect();
                let whole_held = c.query_keeping(f, s, &keep, &within);
                if let Some(t) = &touched {
                    prop_assert_eq!(&c.follow(&lists[i], t, f, s, None), &whole, "{:?} {:?}", f, s);
                    prop_assert_eq!(&c.follow(&held[i], t, f, s, Some(&within)), &whole_held, "held {:?} {:?}", f, s);
                }
                lists[i] = whole;
                held[i] = whole_held;
            }
            tally.update(&c);
            let fresh = AlbumTally::of(&c);
            prop_assert_eq!(tally.counts(), fresh.counts());
            revision = c.revision;
        }
    }
}

/// Plug-in fields change outside `apply` and are still followed; a revision set from outside is not.
#[test]
fn plugin_fields_and_a_revision_set_from_outside() {
    let mut c = library();
    let before = c.revision;
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("p/k".to_string(), serde_json::json!(1));
    c.set_plugin_fields(PhotoId(3), fields.clone()).unwrap();
    assert_eq!(c.touched_since(before), Some(vec![PhotoId(3)]));
    assert!(c.aspects_since(before).is_some_and(|a| a.any_of(Aspects::META)));
    assert!(c.set_plugin_fields(PhotoId(999), fields).is_err(), "no photo: nothing changes");
    assert_eq!(c.touched_since(before), Some(vec![PhotoId(3)]));
    let old = c.revision;
    c.revision += 1000;
    assert_eq!(c.touched_since(old), None);
    assert_eq!(c.aspects_since(c.revision), None);
}
