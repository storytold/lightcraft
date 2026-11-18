//! Issue #102: catalog format versions. Older formats load and are upgraded; a newer format is
//! refused with a clear error and left untouched (never read in part, truncated or set aside).

use crate::journal::{LOG, SNAPSHOT, VERSION, encode_record};
use crate::*;

fn photo(c: &mut Catalog) -> Op {
    let id = c.alloc_photo_id();
    Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: format!("/p/{}.jpg", id.0) }, "p.jpg", "JPEG", 4, 3, "2026-01-01")) }
}

/// A catalog with two photos and the log line for a third op on top of it.
fn legacy_parts() -> (Catalog, String, Catalog) {
    let mut c = Catalog::new();
    for _ in 0..2 {
        let op = photo(&mut c);
        c.apply(op).unwrap();
    }
    let base = c.clone();
    let id = c.photos().next().unwrap().id;
    let op = Op::SetRating { id, rating: 4 };
    c.apply(op.clone()).unwrap();
    (base, format!("{}\n", encode_record(3, &op)), c)
}

fn snapshot_version(m: &MemStore) -> u64 {
    let v: serde_json::Value = serde_json::from_slice(&m.get(SNAPSHOT).unwrap()).unwrap();
    v["version"].as_u64().unwrap()
}

/// Libraries of older formats (1: the first; 2: before albums could be ordered by hand; 3: before keywords could be
/// listed on their own; 5: before saved locations and creations) open as they were and are rewritten in the current
/// format before anything is appended.
#[test]
fn older_libraries_load_and_are_upgraded() {
    for version in [1u32, 2, 3, 5] {
        let (base, log, full) = legacy_parts();
        let m = MemStore::new();
        m.set(SNAPSHOT, format!("{{\"format\":\"lcc-catalog\",\"version\":{version},\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
        m.set(LOG, log.into_bytes());
        let (mut j, c, r) = Journal::open(Box::new(m.clone())).unwrap();
        assert_eq!(c.to_snapshot(), full.to_snapshot(), "v{version}");
        assert_eq!((r.replayed, r.upgraded_from), (1, Some(version)));
        assert_eq!(snapshot_version(&m), u64::from(VERSION), "v{version}");
        assert!(m.get(LOG).unwrap().is_empty(), "v{version}");
        assert_eq!(j.seq(), 3);
        j.append(&[Op::SetLabelName { label: ColorLabel::ALL[0], name: Some("x".into()) }]).unwrap();
        drop(j);
        let (_, _, r) = Journal::open(Box::new(m)).unwrap();
        assert_eq!((r.replayed, r.upgraded_from), (1, None), "v{version}");
    }
}

/// A v11 library (before preview-only photos recorded which version decided it, upstream #662) opens
/// with an empty stamp on its preview-only photos, so they are rechecked once, and is rewritten in
/// the current format.
#[test]
fn v11_preview_only_photo_loads_unstamped_and_is_upgraded() {
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: "/p/a.cr3".into() }, "a.cr3", "CR3", 160, 120, "2026-01-01");
    p.kind = MediaKind::Raw;
    p.preview_only = Some("Cr3 files are not decoded yet".into());
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let snap = c.to_snapshot();
    assert!(!snap.contains("preview_checked"), "an empty stamp isn't written");
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lcc-catalog\",\"version\":11,\"seq\":1,\"catalog\":{snap}}}\n").into_bytes());
    let (_, loaded, r) = Journal::open(Box::new(m.clone())).unwrap();
    let p = loaded.photo(id).unwrap();
    assert_eq!(p.preview_only.as_deref(), Some("Cr3 files are not decoded yet"));
    assert_eq!(p.preview_checked, "");
    assert!(p.preview_only_stale(), "unstamped: rechecked once");
    assert_eq!(r.upgraded_from, Some(11));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

/// A saved creation survives the journal, undo restores the plain album, and bad input is refused.
#[test]
fn saved_creation_round_trips_and_undoes() {
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    let id = AlbumId(77);
    let add = Op::AddAlbum { album: crate::Album::new(id, "Holiday Book") };
    c.apply(add.clone()).unwrap();
    j.append(std::slice::from_ref(&add)).unwrap();
    let creation = crate::Creation { kind: "book".into(), document: "{\"kind\":\"book\"}".into() };
    let set = Op::SetAlbumCreation { id, creation: Some(creation.clone()) };
    let undo = c.apply(set.clone()).unwrap();
    j.append(std::slice::from_ref(&set)).unwrap();
    drop(j);
    let (_, loaded, _) = Journal::open(Box::new(m)).unwrap();
    assert_eq!(loaded.albums.get(&id).and_then(|a| a.creation.clone()), Some(creation));
    c.apply(undo).unwrap();
    assert!(c.albums.get(&id).is_some_and(|a| a.creation.is_none()));
    let bad = crate::Creation { kind: "poster".into(), document: String::new() };
    assert!(c.apply(Op::SetAlbumCreation { id, creation: Some(bad) }).is_err());
    assert!(c.apply(Op::SetAlbumCreation { id: AlbumId(999), creation: None }).is_err());
}

#[test]
fn versionless_log_only_library_is_upgraded() {
    let mut c = Catalog::new();
    let op = photo(&mut c);
    c.apply(op.clone()).unwrap();
    let m = MemStore::new();
    m.set(LOG, format!("{}\n", encode_record(1, &op)).into_bytes());
    let (_, loaded, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(loaded.to_snapshot(), c.to_snapshot());
    assert_eq!(r.upgraded_from, Some(0));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

/// A new library gets a snapshot in this format at once, so an older build never sees a bare log
/// it would read in part.
#[test]
fn new_library_is_written_in_this_format() {
    let m = MemStore::new();
    let (_, _, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert!(r.created);
    assert_eq!(r.upgraded_from, None);
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

fn assert_refused_untouched(m: &MemStore) {
    let before = m.files.lock().unwrap().clone();
    let e = Journal::open(Box::new(m.clone())).err().expect("refused");
    assert!(matches!(e, CatalogError::Newer(_)), "{e:?}");
    let msg = e.to_string();
    assert!(msg.contains("newer version of the app") && msg.contains("left unchanged"), "{msg}");
    assert_eq!(*m.files.lock().unwrap(), before, "nothing modified, nothing renamed");
}

#[test]
fn newer_snapshot_is_refused_untouched() {
    let (base, log, _) = legacy_parts();
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!("{{\"format\":\"lcc-catalog\",\"version\":{},\"seq\":2,\"catalog\":{}}}\n", VERSION + 1, base.to_snapshot()).into_bytes(),
    );
    m.set(LOG, log.into_bytes());
    assert_refused_untouched(&m);
    // even when its catalog doesn't parse as this version's
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lcc-catalog\",\"version\":{},\"seq\":2,\"catalog\":{{\"photos\":7}}}}\n", VERSION + 1).into_bytes());
    assert_refused_untouched(&m);
    let e = Journal::open(Box::new(m)).err().unwrap().to_string();
    assert!(e.contains(&format!("v{}", VERSION + 1)), "{e}");
}

/// A log record with a valid CRC whose op this version doesn't know: refused, not taken for a
/// torn tail (truncated) or damage (renamed) — wherever it is in the log.
#[test]
fn unknown_op_in_the_log_is_refused_untouched() {
    let (base, log, _) = legacy_parts();
    let future = |seq: u64| {
        let body = r#"{"op":"fromTheFuture","id":1,"what":"something new"}"#;
        format!("{{\"seq\":{seq},\"crc\":{},\"op\":{body}}}\n", crc32fast::hash(body.as_bytes()))
    };
    let snap = format!("{{\"format\":\"lcc-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot());
    let id = base.photos().next().unwrap().id;
    let after = format!("{}\n", encode_record(5, &Op::SetRating { id, rating: 1 }));
    for log in [format!("{log}{}", future(4)), format!("{log}{}{after}", future(4)), format!("{}{log}", future(3))] {
        let m = MemStore::new();
        m.set(SNAPSHOT, snap.clone().into_bytes());
        m.set(LOG, log.into_bytes());
        assert_refused_untouched(&m);
    }
    // a known op with a field of an unknown shape is "newer" too
    let body = r#"{"op":"setRating","id":1,"rating":"five stars"}"#;
    let m = MemStore::new();
    m.set(SNAPSHOT, snap.into_bytes());
    m.set(LOG, format!("{log}{{\"seq\":4,\"crc\":{},\"op\":{body}}}\n", crc32fast::hash(body.as_bytes())).into_bytes());
    assert_refused_untouched(&m);
}

/// A CRC mismatch is still damage / a torn tail (not "newer").
#[test]
fn bad_crc_is_still_a_torn_tail() {
    let (base, log, _) = legacy_parts();
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lcc-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    let body = r#"{"op":"fromTheFuture"}"#;
    m.set(LOG, format!("{log}{{\"seq\":4,\"crc\":1,\"op\":{body}}}\n").into_bytes());
    let (_, _, r) = Journal::open(Box::new(m)).unwrap();
    assert!(r.torn_bytes > 0 && r.damaged.is_none(), "{r:?}");
}

/// Guard for the version rule (see `journal` → *Format versions*): adding an [`Op`] variant breaks
/// this match. When it does, bump `journal::VERSION`, add a row to the format table, and add the
/// variant here under the new version. A version that only adds serialized fields raises
/// `NEWEST_FIELDS` instead.
#[test]
fn op_variants_are_versioned() {
    fn since(op: &Op) -> u32 {
        match op {
            Op::AddPhoto { .. }
            | Op::RemovePhoto { .. }
            | Op::SetRating { .. }
            | Op::SetFlag { .. }
            | Op::SetLabel { .. }
            | Op::SetDevelop { .. }
            | Op::SetMeta { .. }
            | Op::SetDeleted { .. }
            | Op::SetLocal { .. }
            | Op::SetVersions { .. }
            | Op::SetHistory { .. }
            | Op::PushHistory { .. }
            | Op::AddAlbum { .. }
            | Op::RemoveAlbum { .. }
            | Op::RenameAlbum { .. }
            | Op::MoveAlbum { .. }
            | Op::SetAlbumPhotos { .. }
            | Op::SetAlbumCover { .. }
            | Op::SetAlbumRules { .. }
            | Op::AddStack { .. }
            | Op::RemoveStack { .. }
            | Op::SetStack { .. }
            | Op::SetCaptured { .. }
            | Op::SetAnalysis { .. }
            | Op::SetFile { .. }
            | Op::Relink { .. }
            | Op::SetContent { .. }
            | Op::SetLabelName { .. }
            | Op::Batch { .. } => 1,
            Op::SetBrowsed { .. } => 2,
            Op::SetAlbumOrder { .. } => 3,
            Op::SetSha1 { .. } | Op::SetKind { .. } | Op::SetXmpStamp { .. } | Op::SetRemote { .. } | Op::SetPreview { .. } => 4,
            Op::SetEmbeddedLens { .. } | Op::SetKeyword { .. } | Op::SetFolderRecord { .. } => 5,
            Op::SetSavedLocation { .. } => 6,
            Op::SetAlbumCreation { .. } => 7,
            Op::SetAsShotWb { .. } => 8,
            Op::SetAlbumPhotoOrder { .. } => 9,
            Op::SetVideoMeta { .. } | Op::SetVideoTrim { .. } | Op::SetVideoPoster { .. } => 10,
            Op::SetHistoryCursor { .. } => 11,
            Op::SetBefore { .. } => 16,
            Op::SetExported { .. } => 17,
            Op::AddVignettingMap { .. } | Op::SetVignettingMapState { .. } => 18,
        }
    }
    /// v15: `Analysis.eyes_open` / `aesthetic` / `cull_score`.
    const NEWEST_FIELDS: u32 = 15;
    let map = lcc_develop::EmbeddedVignettingMap::from_ev([4, 4], [2, 2], None, Default::default(), &[0.0; 4]).unwrap();
    let id = lcc_develop::VignettingMapId::of(&map);
    let newest = since(&Op::AddVignettingMap {
        id,
        record: Box::new(crate::VignettingMapRecord { map, photo: None, added: String::new(), withdrawn: false }),
    });
    assert_eq!(newest.max(NEWEST_FIELDS), VERSION, "the newest op's or fields' version must be the current format version");
}

/// A cleared position is remembered by the catalog itself, whoever sets the metadata, and comes back with the position
/// on undo; setting any position forgets it. A library from before the field opens with nothing remembered.
#[test]
fn a_cleared_gps_position_is_remembered_and_undone_with_it() {
    let mut c = Catalog::new();
    let op = photo(&mut c);
    c.apply(op).unwrap();
    let id = c.photos().next().unwrap().id;
    let set = |c: &mut Catalog, gps: Option<(f64, f64)>| {
        let meta = Meta { gps, ..c.photo(id).unwrap().meta.clone() };
        c.apply(Op::SetMeta { id, meta: Box::new(meta) }).unwrap()
    };
    let state = |c: &Catalog| (c.photo(id).unwrap().meta.gps, c.photo(id).unwrap().meta.gps_removed);
    set(&mut c, Some((10.5, 20.25)));
    assert_eq!(state(&c), (Some((10.5, 20.25)), None));
    let undo_clear = set(&mut c, None);
    assert_eq!(state(&c), (None, Some((10.5, 20.25))), "the position that was cleared");
    set(&mut c, None);
    assert_eq!(state(&c), (None, Some((10.5, 20.25))), "other edits keep it");
    c.apply(undo_clear).unwrap();
    assert_eq!(state(&c), (Some((10.5, 20.25)), None), "undo brings the position back and forgets the removal");
    set(&mut c, None);
    set(&mut c, Some((1.0, 2.0)));
    assert_eq!(state(&c), (Some((1.0, 2.0)), None), "a new position forgets it");

    // a library saved by the previous format has no such field and no removal
    let snap = c.to_snapshot();
    assert!(!snap.contains("gps_removed"), "unset, it is not written");
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lcc-catalog\",\"version\":12,\"seq\":2,\"catalog\":{snap}}}\n").into_bytes());
    let (_, loaded, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(loaded.photo(id).unwrap().meta.gps_removed, None);
    assert_eq!((r.upgraded_from, snapshot_version(&m)), (Some(12), u64::from(VERSION)));
}

/// A format-13 library upgrades with its face regions intact, and an ignored face (format 14) survives reopening.
#[test]
fn v13_library_upgrades_and_ignored_faces_round_trip() {
    let (mut base, _, _) = legacy_parts();
    let id = base.photos().next().unwrap().id;
    let mut meta = base.photo(id).unwrap().meta.clone();
    meta.regions = vec![lcc_meta::Region {
        rect: lcc_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.4, y1: 0.4 },
        kind: lcc_meta::RegionKind::Face,
        name: Some("Person".into()),
        description: Some("kept".into()),
    }];
    base.apply(Op::SetMeta { id, meta: Box::new(meta.clone()) }).unwrap();
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lcc-catalog\",\"version\":13,\"seq\":3,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    let (mut j, catalog, report) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(report.upgraded_from, Some(13));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
    assert_eq!(catalog.photo(id).unwrap().meta, meta);
    meta.regions[0].kind = lcc_meta::RegionKind::IgnoredFace;
    j.append(&[Op::SetMeta { id, meta: Box::new(meta.clone()) }]).unwrap();
    drop(j);
    let (_, reopened, report) = Journal::open(Box::new(m)).unwrap();
    assert_eq!(report.upgraded_from, None);
    assert_eq!(reopened.photo(id).unwrap().meta, meta);
}

/// A format-14 `Analysis` object without the score fields still loads (serde default).
#[test]
fn v14_analysis_json_gains_optional_score_fields() {
    let a: crate::Analysis = serde_json::from_str(r#"{"sharpness":50.0,"clipped":0.01,"group":null,"best":false}"#).unwrap();
    assert!(a.eyes_open.is_none() && a.aesthetic.is_none() && a.cull_score.is_none());
    assert_eq!(a.sharpness, 50.0);
}

/// Upstream #3962: `catalog.snap` at stamp `version` holding `base`, with `extra` spliced in as
/// more top-level members (empty: an ordinary snapshot).
fn snap_at(version: u32, base: &Catalog, extra: &str) -> Vec<u8> {
    format!("{{\"format\":\"lcc-catalog\",\"version\":{version},{extra}\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes()
}

/// A snapshot at a readable stamp with a top-level key this format never writes, or with a
/// `segments` folder beside it (or no snapshot at all), is refused untouched instead of loading
/// part of the library and saving over the rest.
#[test]
fn unknown_layout_is_refused_untouched() {
    let (base, log, _) = legacy_parts();
    for version in [VERSION, VERSION - 1] {
        let m = MemStore::new();
        m.set(SNAPSHOT, snap_at(version, &base, r#""segments":["0001.seg"],"#));
        m.set(LOG, log.clone().into_bytes());
        let before = m.files.lock().unwrap().clone();
        let e = Journal::open(Box::new(m.clone())).err().expect("refused");
        assert!(matches!(e, CatalogError::Newer(_)), "{e:?}");
        assert!(e.to_string().contains("layout isn't one this version reads"), "{e}");
        assert_eq!(*m.files.lock().unwrap(), before);

        let m = MemStore::new();
        m.set(SNAPSHOT, snap_at(version, &base, ""));
        m.set(LOG, log.clone().into_bytes());
        m.set("segments/0001.seg", b"photos live here".to_vec());
        let before = m.files.lock().unwrap().clone();
        let e = Journal::open(Box::new(m.clone())).err().expect("refused");
        assert!(matches!(e, CatalogError::Newer(_)), "{e:?}");
        assert_eq!(*m.files.lock().unwrap(), before);
    }
    let m = MemStore::new();
    m.set("segments/0001.seg", b"photos live here".to_vec());
    let before = m.files.lock().unwrap().clone();
    assert!(Journal::open(Box::new(m.clone())).is_err());
    assert_eq!(*m.files.lock().unwrap(), before);
}

/// Controls: an ordinary library still opens (a `segments` file is no folder), and a malformed
/// snapshot is an error, not a panic.
#[test]
fn ordinary_layout_still_opens_and_hostile_snapshots_do_not_panic() {
    let (base, log, full) = legacy_parts();
    let m = MemStore::new();
    m.set(SNAPSHOT, snap_at(VERSION, &base, ""));
    m.set(LOG, log.into_bytes());
    m.set("segments", b"not a folder".to_vec());
    let (_, c, _) = Journal::open(Box::new(m)).unwrap();
    assert_eq!(c.to_snapshot(), full.to_snapshot());
    for bad in [&b"[]"[..], b"null", b"{\"format\":\"lcc-catalog\",\"version\":", b"\xff\xfe"] {
        let m = MemStore::new();
        m.set(SNAPSHOT, bad.to_vec());
        assert!(Journal::open(Box::new(m)).is_err());
    }
}

/// The native store reports a real directory as a folder, and a missing one as none.
#[test]
fn fs_store_reports_folders() {
    let dir = std::env::temp_dir().join(format!("lcc-layout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("segments")).unwrap();
    let mut s = FsStore::open(&dir).unwrap();
    assert!(Store::has_folder(&mut s, "segments").unwrap());
    assert!(!Store::has_folder(&mut s, "nothing").unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Export status (format 17): a library from the previous format opens with every photo never exported, setting and
/// clearing it undoes, and an unset status is not written.
#[test]
fn exported_status_round_trips_and_old_libraries_load() {
    let (base, _, _) = legacy_parts();
    let id = base.photos().next().unwrap().id;
    let mut c = base.clone();
    assert!(!c.to_snapshot().contains("exported"), "unset, it is not written");
    let undo_set = c.apply(Op::SetExported { id, exported: Some("2026-11-10T08:00:00Z".into()) }).unwrap();
    assert_eq!(c.photo(id).unwrap().exported.as_deref(), Some("2026-11-10T08:00:00Z"));
    assert!(c.to_snapshot().contains("exported"));
    let undo_clear = c.apply(Op::SetExported { id, exported: None }).unwrap();
    assert_eq!(c.photo(id).unwrap().exported, None);
    c.apply(undo_clear).unwrap();
    assert!(c.photo(id).unwrap().exported.is_some(), "undo of a reset brings the status back");
    c.apply(undo_set).unwrap();
    assert_eq!(c.photo(id).unwrap().exported, None);

    // a format-16 snapshot (no such field) opens and is upgraded
    let m = MemStore::new();
    m.set(SNAPSHOT, snap_at(16, &base, ""));
    let (_, loaded, r) = Journal::open(Box::new(m)).unwrap();
    assert_eq!(loaded.photo(id).unwrap().exported, None);
    assert_eq!(r.upgraded_from, Some(16));
}
