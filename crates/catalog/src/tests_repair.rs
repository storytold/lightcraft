//! A damaged or hand-edited snapshot that still parses is made to hold together on load
//! ([`Catalog::repair`]): found by fuzzing snapshots of a real library.

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape, SegMask};

use crate::journal::SNAPSHOT;
use crate::*;

fn album(id: u64, parent: Option<u64>, folder: bool, photos: Vec<PhotoId>) -> Album {
    Album {
        id: AlbumId(id),
        name: format!("A{id}"),
        parent: parent.map(AlbumId),
        folder,
        photos,
        cover: None,
        smart: None,
        quick: false,
        order: None,
    }
}

/// A catalog of four photos, a stack of two, a folder with an album in it.
fn sound() -> (Catalog, Vec<PhotoId>) {
    let mut c = Catalog::new();
    let mut ids = Vec::new();
    for i in 0..4 {
        let id = c.alloc_photo_id();
        c.apply(Op::AddPhoto {
            photo: Box::new(Photo::new(id, Source::File { path: format!("/p/{i}.jpg") }, &format!("{i}.jpg"), "JPEG", 60, 40, "2026-01-01")),
        })
        .unwrap();
        ids.push(id);
    }
    let sid = c.alloc_stack_id();
    c.apply(Op::AddStack { stack: Stack { id: sid, photos: vec![ids[0], ids[1]], collapsed: true } }).unwrap();
    let (f, a) = (c.alloc_album_id(), c.alloc_album_id());
    c.apply(Op::AddAlbum { album: album(f.0, None, true, vec![]) }).unwrap();
    c.apply(Op::AddAlbum { album: album(a.0, Some(f.0), false, vec![ids[2]]) }).unwrap();
    (c, ids)
}

/// The snapshot file of `c` with `edit` applied to its JSON, loaded through the journal.
fn load_edited(c: &Catalog, edit: impl FnOnce(&mut serde_json::Value)) -> (Catalog, LoadReport) {
    let m = MemStore::new();
    let (mut j, _, _) = Journal::open(Box::new(m.clone())).unwrap();
    j.snapshot(c).unwrap();
    let mut v: serde_json::Value = serde_json::from_slice(&m.get(SNAPSHOT).unwrap()).unwrap();
    edit(&mut v["catalog"]);
    m.set(SNAPSHOT, serde_json::to_vec(&v).unwrap());
    drop(j);
    let (_, c, report) = Journal::open(Box::new(m)).unwrap();
    (c, report)
}

/// A stack whose `id` differs from the key it is filed under made `arrange_stacks` index the map
/// with an id it doesn't hold, which panics: the grid of any library with such a stack.
#[test]
fn a_stack_filed_under_another_id_is_arranged() {
    let (c, ids) = sound();
    let (c, report) = load_edited(&c, |v| {
        let key = v["stacks"].as_object().unwrap().keys().next().unwrap().clone();
        v["stacks"][&key]["id"] = 77.into();
    });
    assert!(report.repaired.iter().any(|r| r.contains("stack 77 was filed as")), "{:?}", report.repaired);
    assert_eq!(c.arrange_stacks(&ids), vec![ids[0], ids[2], ids[3]], "the collapsed stack shows its top");
    assert!(c.stacks().all(|s| c.stack_of(s.photos[0]).is_some_and(|t| t.id == s.id)));
}

/// Stacks of missing, repeated or too few photos (or one photo in two stacks) are put right: a
/// stack of none had no top (`photos[0]`).
#[test]
fn stacks_keep_only_photos_in_the_catalog() {
    let (c, ids) = sound();
    let (c, report) = load_edited(&c, |v| {
        let key = v["stacks"].as_object().unwrap().keys().next().unwrap().clone();
        v["stacks"][&key]["photos"] = serde_json::json!([ids[0].0, 999, ids[0].0, ids[1].0]);
        v["stacks"]["8"] = serde_json::json!({"id": 8, "photos": []});
        v["stacks"]["9"] = serde_json::json!({"id": 9, "photos": [ids[1].0, ids[3].0, ids[2].0]});
    });
    let stacks: Vec<(StackId, Vec<PhotoId>)> = c.stacks().map(|s| (s.id, s.photos.clone())).collect();
    assert_eq!(stacks.len(), 2, "{stacks:?} {:?}", report.repaired);
    assert_eq!(stacks[0].1, vec![ids[0], ids[1]], "missing and repeated photos left out");
    assert_eq!(stacks[1].1, vec![ids[3], ids[2]], "a photo already in a stack stays in the first");
    assert!(!c.arrange_stacks(&ids).is_empty());
}

/// Folders inside each other (a loop), or under an album or a missing folder, move to the top
/// level: walking up from them never ends otherwise, and moving any folder there hung.
#[test]
fn folder_loops_and_missing_parents_move_to_the_top() {
    let (c, _) = sound();
    let (mut c, report) = load_edited(&c, |v| {
        v["albums"]["10"] = serde_json::to_value(album(10, Some(11), true, vec![])).unwrap();
        v["albums"]["11"] = serde_json::to_value(album(11, Some(10), true, vec![])).unwrap();
        v["albums"]["12"] = serde_json::to_value(album(12, Some(12), true, vec![])).unwrap();
        v["albums"]["13"] = serde_json::to_value(album(13, Some(404), false, vec![])).unwrap();
    });
    assert!(!report.repaired.is_empty());
    for id in [10, 11, 12, 13] {
        let a = c.album(AlbumId(id)).unwrap();
        assert!(
            a.parent.is_none() || a.parent.is_some_and(|p| c.album(p).is_some_and(|f| f.folder && f.parent.is_none())),
            "album {id}: {:?}",
            a.parent
        );
    }
    // moving a folder around terminates either way
    let _ = c.apply(Op::MoveAlbum { id: AlbumId(10), parent: Some(AlbumId(11)) });
    let _ = c.apply(Op::MoveAlbum { id: AlbumId(11), parent: Some(AlbumId(10)) });
}

/// Check the postcondition needed by every consumer that follows parent folders. This inspects
/// the repaired catalog, not its cache or a second implementation of the repair algorithm.
fn assert_all_album_chains_reach_roots(c: &Catalog) {
    for album in c.albums.values() {
        let mut seen = std::collections::HashSet::new();
        let mut current = album;
        while let Some(parent) = current.parent {
            assert!(seen.insert(current.id), "album {} still reaches a cycle", album.id.0);
            current = c.albums.get(&parent).expect("every retained parent exists");
            assert!(current.folder, "every retained parent is a folder");
        }
    }
}

#[test]
fn a_child_visited_before_a_folder_cycle_keeps_its_content_and_gets_a_rooted_chain() {
    let (c, ids) = sound();
    let (mut c, report) = load_edited(&c, |v| {
        // BTreeMap order visits10 first: cached Bad11/12 used to be skipped without repair.
        v["albums"]["10"] = serde_json::to_value(album(10, Some(11), false, vec![ids[2]])).unwrap();
        v["albums"]["11"] = serde_json::to_value(album(11, Some(12), true, vec![])).unwrap();
        v["albums"]["12"] = serde_json::to_value(album(12, Some(11), true, vec![])).unwrap();
    });
    assert_all_album_chains_reach_roots(&c);
    assert_eq!(c.album(AlbumId(10)).unwrap().parent, Some(AlbumId(11)), "the child remains in its actual folder");
    assert_eq!(c.album(AlbumId(10)).unwrap().photos, vec![ids[2]], "repair preserves the child's photos");
    assert_eq!(c.album(AlbumId(11)).unwrap().parent, None, "one cycle edge is repaired");
    assert_eq!(c.album(AlbumId(12)).unwrap().parent, Some(AlbumId(11)), "the other folder edge is retained");
    assert!(report.repaired.iter().any(|message| message.starts_with("album 11:")), "{:?}", report.repaired);
    assert!(c.repair().is_empty(), "a second repair is idempotent");
}

#[test]
fn a_child_visited_before_a_folder_with_a_missing_parent_does_not_hide_the_bad_ancestor() {
    let (c, ids) = sound();
    let (mut c, report) = load_edited(&c, |v| {
        v["albums"]["10"] = serde_json::to_value(album(10, Some(11), false, vec![ids[2]])).unwrap();
        v["albums"]["11"] = serde_json::to_value(album(11, Some(999), true, vec![])).unwrap();
    });
    assert_all_album_chains_reach_roots(&c);
    assert_eq!(c.album(AlbumId(10)).unwrap().parent, Some(AlbumId(11)));
    assert_eq!(c.album(AlbumId(10)).unwrap().photos, vec![ids[2]]);
    assert_eq!(c.album(AlbumId(11)).unwrap().parent, None, "repair the missing-parent edge itself");
    assert!(report.repaired.iter().any(|message| message.starts_with("album 11:")), "{:?}", report.repaired);
    assert!(c.repair().is_empty());
}

#[test]
fn a_long_valid_folder_chain_is_retained_when_its_child_is_visited_first() {
    let (c, ids) = sound();
    let (mut c, report) = load_edited(&c, |v| {
        v["albums"]["10"] = serde_json::to_value(album(10, Some(100), false, vec![ids[2]])).unwrap();
        for id in 100..612 {
            v["albums"][id.to_string()] = serde_json::to_value(album(id, Some(id + 1), true, vec![])).unwrap();
        }
        v["albums"]["612"] = serde_json::to_value(album(612, None, true, vec![])).unwrap();
    });
    assert_all_album_chains_reach_roots(&c);
    assert_eq!(c.album(AlbumId(10)).unwrap().parent, Some(AlbumId(100)));
    assert_eq!(c.album(AlbumId(10)).unwrap().photos, vec![ids[2]]);
    for id in 100..612 {
        assert_eq!(c.album(AlbumId(id)).unwrap().parent, Some(AlbumId(id + 1)), "valid edge{id} remains");
    }
    assert_eq!(c.album(AlbumId(612)).unwrap().parent, None);
    assert!(!report.repaired.iter().any(|message| message.starts_with("album ")), "valid parents must not be repaired: {:?}", report.repaired);
    assert!(c.repair().is_empty());
}

/// Id counters left at the top (or behind the ids in use) by a damaged file: allocating no longer
/// overflows (a panic in debug builds) and never hands out an id already in use.
#[test]
fn id_counters_are_moved_past_the_ids_in_use() {
    let (c, ids) = sound();
    let (mut c, report) = load_edited(&c, |v| {
        v["next_photo"] = 1.into();
        v["next_album"] = 1.into();
        v["next_stack"] = 1.into();
    });
    assert!(report.repaired.iter().any(|r| r.contains("id counters")), "{:?}", report.repaired);
    assert!(c.alloc_photo_id().0 > ids[3].0);
    let (mut c, _) = load_edited(&c, |v| v["next_photo"] = u64::MAX.into());
    assert_eq!((c.alloc_photo_id().0, c.alloc_photo_id().0), (u64::MAX, u64::MAX), "saturates");
    let p = Photo::new(PhotoId(u64::MAX), Source::Demo { scene: 1 }, "a", "JPEG", 1, 1, "t");
    c.apply(Op::AddPhoto { photo: Box::new(p.clone()) }).unwrap();
    assert!(c.apply(Op::AddPhoto { photo: Box::new(p) }).is_err(), "the id is taken: refused, not overwritten");
}

/// History was bounded only where an edit adds a step, so a snapshot holding more steps kept
/// every one in memory on each load (#2579). The newest are kept and the cursor stays on its step.
#[test]
fn history_beyond_the_limit_is_trimmed_on_load() {
    let (mut c, ids) = sound();
    let extra = HISTORY_LIMIT * 5;
    let step = HistoryStep { label: "s0".into(), settings: std::sync::Arc::new(lightcraft_develop::DevelopSettings::default()) };
    c.apply(Op::PushHistory { id: ids[0], step }).unwrap();
    c.apply(Op::SetHistoryCursor { id: ids[0], cursor: Some(extra - 3) }).unwrap();
    let (loaded, report) = load_edited(&c, |v| {
        let history = &mut v["photos"][ids[0].0.to_string()]["history"];
        let one = history[0].clone();
        *history = (0..extra)
            .map(|i| {
                let mut s = one.clone();
                s["label"] = format!("s{i}").into();
                s
            })
            .collect();
    });
    let p = loaded.photo(ids[0]).unwrap();
    assert_eq!(p.history.len(), HISTORY_LIMIT);
    assert_eq!(p.history[0].label, format!("s{}", extra - HISTORY_LIMIT));
    assert_eq!(p.history_index().map(|i| p.history[i].label.clone()), Some(format!("s{}", extra - 3)));
    assert_eq!(report.repaired.len(), 1, "{:?}", report.repaired);
}

/// A sound catalog loads unchanged.
#[test]
fn a_sound_snapshot_needs_no_repair() {
    let (c, _) = sound();
    let (loaded, report) = load_edited(&c, |_| {});
    assert!(report.repaired.is_empty(), "{:?}", report.repaired);
    assert_eq!(loaded.to_snapshot(), c.to_snapshot());
}

/// Found by fuzzing: an album filed under one id but naming another (here: the folder's) put
/// itself inside itself, and listing the folder tree overflowed the stack (an abort, which no
/// panic guard catches, every time the library opened).
#[test]
fn an_album_filed_under_another_id_gets_its_own() {
    let (c, _) = sound();
    let folder = c.albums().find(|a| a.folder).unwrap().id;
    let inner = c.albums().find(|a| !a.folder).unwrap().id;
    let (c, report) = load_edited(&c, |v| v["albums"][inner.0.to_string()]["id"] = folder.0.into());
    assert!(report.repaired.iter().any(|r| r.contains(&format!("album {} was filed as {}", folder.0, inner.0))), "{:?}", report.repaired);
    let a = c.album(inner).unwrap();
    assert_eq!((a.id, a.parent), (inner, Some(folder)));
    assert_eq!(c.album_children(Some(folder)).iter().map(|a| a.id).collect::<Vec<_>>(), vec![inner]);
}

/// What was worked out from the albums (whether rules may be tested cheapest first) is worked out
/// again when a repair changes the albums.
#[test]
fn a_repair_forgets_what_was_worked_out_from_the_albums() {
    let (mut c, _) = sound();
    let smart = c.alloc_album_id();
    c.apply(Op::AddAlbum { album: Album { smart: Some(Box::default()), ..album(smart.0, None, false, vec![]) } }).unwrap();
    assert!(!c.keeps_written_order(), "worked out, and remembered");
    // damaged outside `apply`: the smart album tests itself, and an album is filed under another id
    let rules: Filter =
        serde_json::from_value(serde_json::json!({"ruleSet": {"match": "all", "rules": [{"field": "album", "op": "is", "value": smart.0}]}}))
            .unwrap();
    if let Some(a) = c.albums.get_mut(&smart) {
        a.smart = Some(Box::new(rules));
    }
    let inner = c.albums().find(|a| !a.folder && a.smart.is_none()).unwrap().id;
    if let Some(mut a) = c.albums.remove(&inner) {
        a.id = AlbumId(900);
        c.albums.insert(inner, a);
    }
    assert!(!c.repair().is_empty());
    assert!(c.keeps_written_order(), "the loop the repair left is seen");
}

// ---- repair copies only what it repairs ----

/// Develop settings with one object mask whose segmentation is `seg`.
fn settings_with(seg: SegMask) -> Arc<DevelopSettings> {
    let mut s = DevelopSettings::default();
    s.masks.push(Mask {
        id: 1,
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Object {
                hint: vec![lightcraft_geom::Point::new(0.5, 0.5)],
                exclude: vec![],
                seg: Some(seg),
                detail: vec![],
                edge: 0.0,
            },
        }],
        ..Default::default()
    });
    Arc::new(s)
}

/// The segmentation of the first mask of `s`.
fn first_seg(s: &DevelopSettings) -> SegMask {
    let mut found = None;
    s.visit_seg_masks(&mut |m| found = found.take().or_else(|| Some(m.clone())));
    found.unwrap()
}

/// A catalog this build wrote is shared with whoever else holds its photos (the journal's record of
/// what is on disk, a clone being measured): repairing it has nothing to change, so it copies
/// nothing, neither a photo nor the settings it holds.
#[test]
fn a_repair_that_changes_nothing_copies_no_photo() {
    let (mut c, _) = sound();
    let held = c.clone();
    assert!(c.repair().is_empty());
    for (id, p) in &c.photos {
        let theirs = &held.photos[id];
        assert!(Arc::ptr_eq(p, theirs), "photo {} was copied by a repair that changed nothing", id.0);
    }
}

/// The settings are shared too when only a photo is unique (its holders share the settings with
/// other holders, as a history step shares the settings it was made from): a repair with no grid
/// reference to resolve leaves them as they are.
#[test]
fn a_repair_that_resolves_no_grid_reference_copies_no_settings() {
    let (mut c, ids) = sound();
    let settings = settings_with(SegMask::from_logits(8, &[0.5f32; 64]));
    for id in &ids {
        let Some(p) = c.photos.get_mut(id).map(Arc::make_mut) else { continue };
        p.develop = settings.clone();
        p.history.push(HistoryStep { label: "Exposure".into(), settings: settings.clone() });
    }
    assert!(Arc::strong_count(&settings) > 1);
    assert!(c.repair().is_empty());
    for id in &ids {
        let p = &c.photos[id];
        assert!(Arc::ptr_eq(&p.develop, &settings), "photo {}: develop settings were copied", id.0);
        assert!(Arc::ptr_eq(&p.history[0].settings, &settings), "photo {}: a history step's settings were copied", id.0);
    }
}

/// A repair copies the photos it has something to fix and only those, and still fixes each: a photo
/// filed under another id, one with a grid reference (in its settings and a history step) to
/// resolve, one with more History than it keeps. The fourth, sound, stays the very photo that
/// was shared.
#[test]
fn a_repair_copies_only_the_photos_it_repairs_and_still_repairs_them() {
    let (mut c, ids) = sound();
    let seg = SegMask::from_logits(8, &[0.5f32; 64]);
    let blob = seg.data.clone();
    c.grids = vec![blob.clone()];
    let by_ref = SegMask { data: String::new(), grid: Some(0), ..seg };
    if let Some(p) = c.photos.get_mut(&ids[0]).map(Arc::make_mut) {
        p.id = PhotoId(900);
    }
    if let Some(p) = c.photos.get_mut(&ids[1]).map(Arc::make_mut) {
        p.develop = settings_with(by_ref.clone());
        p.history.push(HistoryStep { label: "Mask".into(), settings: settings_with(by_ref) });
    }
    if let Some(p) = c.photos.get_mut(&ids[2]).map(Arc::make_mut) {
        let step = HistoryStep { label: "Exposure".into(), settings: Arc::new(DevelopSettings::default()) };
        p.history = vec![step; HISTORY_LIMIT + 3];
    }
    let held = c.clone();

    let report = c.repair();
    assert_eq!(report.len(), 2, "{report:?}");
    assert!(report.iter().any(|r| r.contains("was filed as")), "{report:?}");
    assert!(report.iter().any(|r| r.contains("History step(s) beyond")), "{report:?}");

    // repaired, each on its own copy
    assert_eq!(c.photos[&ids[0]].id, ids[0]);
    for settings in c.photos[&ids[1]].settings_holders() {
        let seg = first_seg(settings);
        assert_eq!((seg.grid, seg.data.as_str()), (None, blob.as_str()), "the reference is resolved in every holder");
    }
    assert_eq!(c.photos[&ids[2]].history.len(), HISTORY_LIMIT);
    assert!(c.grids.is_empty(), "the snapshot's table is not kept");
    for id in &ids[..3] {
        assert!(!Arc::ptr_eq(&c.photos[id], &held.photos[id]), "photo {} had something to repair and was repaired in place of the shared one", id.0);
    }
    // the sound one: not copied
    assert!(Arc::ptr_eq(&c.photos[&ids[3]], &held.photos[&ids[3]]), "a sound photo was copied");
    // what was shared is as it was
    assert_eq!(held.photos[&ids[0]].id, PhotoId(900));
    assert_eq!(held.photos[&ids[2]].history.len(), HISTORY_LIMIT + 3);
    assert!(c.repair().is_empty(), "a second repair is idempotent");
}

/// Of a photo that has a reference to resolve, only the holders with one are copied: a History step
/// made from settings that are still shared elsewhere keeps the shared settings.
#[test]
fn a_repair_resolves_the_holders_with_a_reference_and_leaves_the_others_shared() {
    let (mut c, ids) = sound();
    let seg = SegMask::from_logits(8, &[0.5f32; 64]);
    let blob = seg.data.clone();
    c.grids = vec![blob.clone()];
    let plain = Arc::new(DevelopSettings::default());
    if let Some(p) = c.photos.get_mut(&ids[0]).map(Arc::make_mut) {
        p.develop = settings_with(SegMask { data: String::new(), grid: Some(0), ..seg });
        p.history.push(HistoryStep { label: "Exposure".into(), settings: plain.clone() });
    }
    let held = c.clone();
    assert!(c.repair().is_empty(), "resolving a reference is how a snapshot reads, not damage");
    let p = &c.photos[&ids[0]];
    let resolved = first_seg(&p.develop);
    assert_eq!((resolved.grid, resolved.data.as_str()), (None, blob.as_str()));
    assert!(Arc::ptr_eq(&p.history[0].settings, &plain), "settings without a reference were copied");
    assert_eq!(first_seg(&held.photos[&ids[0]].develop).grid, Some(0), "what was shared is as it was");
}
