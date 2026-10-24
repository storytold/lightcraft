//! A damaged or hand-edited snapshot that still parses is made to hold together on load
//! ([`Catalog::repair`]): found by fuzzing snapshots of a real library.

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
