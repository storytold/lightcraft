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

#[test]
fn v1_library_loads_and_is_upgraded() {
    let (base, log, full) = legacy_parts();
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":1,\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    m.set(LOG, log.into_bytes());
    let (mut j, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(c.to_snapshot(), full.to_snapshot());
    assert_eq!((r.replayed, r.upgraded_from), (1, Some(1)));
    // rewritten in this format before anything is appended
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
    assert!(m.get(LOG).unwrap().is_empty());
    assert_eq!(j.seq(), 3);
    j.append(&[Op::SetLabelName { label: ColorLabel::ALL[0], name: Some("x".into()) }]).unwrap();
    drop(j);
    let (_, _, r) = Journal::open(Box::new(m)).unwrap();
    assert_eq!((r.replayed, r.upgraded_from), (1, None));
}

/// A library saved before albums could be ordered by hand opens as it was and is rewritten in the current format.
#[test]
fn v2_library_loads_and_is_upgraded() {
    let (base, log, full) = legacy_parts();
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":2,\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    m.set(LOG, log.into_bytes());
    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(c.to_snapshot(), full.to_snapshot());
    assert_eq!((r.replayed, r.upgraded_from), (1, Some(2)));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

/// A library saved before keywords could be listed on their own opens as it was and is rewritten
/// in the current format.
#[test]
fn v3_library_loads_and_is_upgraded() {
    let (base, log, full) = legacy_parts();
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":3,\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    m.set(LOG, log.into_bytes());
    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(c.to_snapshot(), full.to_snapshot());
    assert_eq!((r.replayed, r.upgraded_from), (1, Some(3)));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

/// A v6 library (before preview-only photos recorded which version decided it, #662) opens with
/// an empty stamp on its preview-only photos, so they are rechecked once, and is rewritten in the
/// current format.
#[test]
fn v6_preview_only_photo_loads_unstamped_and_is_upgraded() {
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: "/p/a.cr3".into() }, "a.cr3", "CR3", 160, 120, "2026-01-01");
    p.kind = MediaKind::Raw;
    p.preview_only = Some("Cr3 files are not decoded yet".into());
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let snap = c.to_snapshot();
    assert!(!snap.contains("preview_checked"), "an empty stamp isn't written");
    let m = MemStore::new();
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":6,\"seq\":1,\"catalog\":{snap}}}\n").into_bytes());
    let (_, loaded, r) = Journal::open(Box::new(m.clone())).unwrap();
    let p = loaded.photo(id).unwrap();
    assert_eq!(p.preview_only.as_deref(), Some("Cr3 files are not decoded yet"));
    assert_eq!(p.preview_checked, "");
    assert_eq!(r.upgraded_from, Some(6));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
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
    assert!(msg.contains("newer version of LightCraft") && msg.contains("left unchanged"), "{msg}");
    assert_eq!(*m.files.lock().unwrap(), before, "nothing modified, nothing renamed");
}

#[test]
fn newer_snapshot_is_refused_untouched() {
    let (base, log, _) = legacy_parts();
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!("{{\"format\":\"lightcraft-catalog\",\"version\":{},\"seq\":2,\"catalog\":{}}}\n", VERSION + 1, base.to_snapshot()).into_bytes(),
    );
    m.set(LOG, log.into_bytes());
    assert_refused_untouched(&m);
    // even when its catalog doesn't parse as this version's
    let m = MemStore::new();
    m.set(
        SNAPSHOT,
        format!("{{\"format\":\"lightcraft-catalog\",\"version\":{},\"seq\":2,\"catalog\":{{\"photos\":7}}}}\n", VERSION + 1).into_bytes(),
    );
    assert_refused_untouched(&m);
    let e = Journal::open(Box::new(m)).err().unwrap().to_string();
    assert!(e.contains(&format!("v{}", VERSION + 1)), "{e}");
}

/// Issue #3962: `catalog.snap` at stamp `version` holding `base`, with `extra` spliced in as more
/// top-level members (empty: an ordinary snapshot).
fn snap_at(version: u32, base: &Catalog, extra: &str) -> Vec<u8> {
    format!("{{\"format\":\"lightcraft-catalog\",\"version\":{version},{extra}\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes()
}

/// The readable stamps the layout tests run at: this build's and an older one.
const READABLE: [u32; 2] = [VERSION, VERSION - 1];

/// A snapshot at a readable stamp with a top-level key this format never writes (a fork that kept
/// its photos elsewhere) is refused untouched, not loaded as a catalog without them and then saved
/// over by the upgrade or the next snapshot (#3962). Before the fix the open succeeded.
#[test]
fn unknown_top_level_key_is_refused_untouched() {
    let (base, log, _) = legacy_parts();
    for version in READABLE {
        for (key, extra) in [("segments", r#""segments":["0001.seg","0002.seg"],"#), ("photoIndex", r#""photoIndex":{"count":2},"#)] {
            let m = MemStore::new();
            m.set(SNAPSHOT, snap_at(version, &base, extra));
            m.set(LOG, log.clone().into_bytes());
            assert_refused_untouched(&m);
            let e = Journal::open(Box::new(m)).err().unwrap().to_string();
            assert!(e.contains(&format!("{key:?}")) && e.contains("layout isn't one this version reads"), "v{version}: {e}");
        }
    }
}

/// A `segments` folder next to an otherwise ordinary snapshot: the snapshot isn't the whole
/// library, so it is refused untouched (#3962). Also with no snapshot at all, where a new empty
/// one would otherwise be written beside the folder. Before the fix the open succeeded.
#[test]
fn segments_folder_is_refused_untouched() {
    let (base, log, _) = legacy_parts();
    for version in READABLE {
        let m = MemStore::new();
        m.set(SNAPSHOT, snap_at(version, &base, ""));
        m.set(LOG, log.clone().into_bytes());
        m.set("segments/0001.seg", b"photos live here".to_vec());
        assert_refused_untouched(&m);
        let e = Journal::open(Box::new(m)).err().unwrap().to_string();
        assert!(e.contains("\"segments\" folder") && e.contains("layout isn't one this version reads"), "v{version}: {e}");
    }
    // no snapshot, with and without a log: refused for the folder, and no catalog.snap or
    // catalog.log appears beside it
    for log in [None, Some(log)] {
        let m = MemStore::new();
        m.set("segments/0001.seg", b"photos live here".to_vec());
        if let Some(log) = &log {
            m.set(LOG, log.clone().into_bytes());
        }
        assert_refused_untouched(&m);
        let e = Journal::open(Box::new(m.clone())).err().unwrap().to_string();
        assert!(e.contains("\"segments\" folder") && e.contains("layout isn't one this version reads"), "log {}: {e}", log.is_some());
        assert_eq!(m.get(SNAPSHOT), None, "no snapshot written beside the folder");
        assert_eq!(m.get(LOG).is_some(), log.is_some(), "no log created");
    }
}

/// A store that can't tell whether a `segments` folder is there fails the open with that error,
/// before anything is written: guessing "no folder" would let the upgrade or a new snapshot save
/// over a library it couldn't see (#3962). With a snapshot and without one.
#[test]
fn a_folder_check_that_fails_stops_the_open_untouched() {
    let (base, log, _) = legacy_parts();
    for snap in [Some(snap_at(VERSION - 1, &base, "")), None] {
        let t = crate::tests_torn_append::Torn::default();
        if let Some(snap) = &snap {
            t.files.set(SNAPSHOT, snap.clone());
        }
        t.files.set(LOG, log.clone().into_bytes());
        t.folder_fails.store(true, std::sync::atomic::Ordering::SeqCst);
        let before = t.files.files.lock().unwrap().clone();
        let e = Journal::open(Box::new(t.clone())).err().expect("the open fails");
        assert!(matches!(&e, CatalogError::Io(m) if m.contains("folder listing refused")), "snapshot {}: {e:?}", snap.is_some());
        assert_eq!(*t.files.files.lock().unwrap(), before, "nothing written, nothing renamed");
        // the same store once it can answer: the library opens
        t.folder_fails.store(false, std::sync::atomic::Ordering::SeqCst);
        Journal::open(Box::new(t)).unwrap();
    }
}

/// A snapshot whose top level isn't an object is corrupt, not newer and not a catalog, and keeps
/// its bytes (#3962): an array, a string, a number or `null`, and an array that reads as the
/// stamp (`[format, version]`, which the header check takes) and so reaches the key check.
#[test]
fn non_object_snapshot_is_corrupt_and_untouched() {
    let stamp_array = format!("[\"lightcraft-catalog\",{VERSION}]\n");
    for top in ["[]", "\"lightcraft-catalog\"", "7", "null", stamp_array.as_str()] {
        let m = MemStore::new();
        m.set(SNAPSHOT, top.as_bytes().to_vec());
        let e = Journal::open(Box::new(m.clone())).err().expect("refused");
        assert!(matches!(&e, CatalogError::Corrupt(msg) if msg.starts_with(SNAPSHOT)), "{top:.20}: {e:?}");
        assert_eq!(m.get(SNAPSHOT).as_deref(), Some(top.as_bytes()), "{top:.20}: snapshot untouched");
        assert_eq!(m.get(LOG), None, "{top:.20}: no log created");
    }
}

/// The checks behind the one-pass read keep their order (#3989): a snapshot whose keys come in
/// another order than this build writes them, or whose catalog doesn't parse, still gets the
/// answer of stamp, then layout, then catalog — not the first problem the pass ran into.
#[test]
fn snapshot_checks_keep_their_order_off_the_one_pass_read() {
    let (base, log, full) = legacy_parts();
    let cat = base.to_snapshot();
    let open = |snap: String| {
        let m = MemStore::new();
        m.set(SNAPSHOT, snap.into_bytes());
        m.set(LOG, log.clone().into_bytes());
        (Journal::open(Box::new(m.clone())), m)
    };
    // the catalog first, the stamp last: still a readable library
    let (r, _) = open(format!("{{\"catalog\":{cat},\"seq\":2,\"version\":{VERSION},\"format\":\"lightcraft-catalog\"}}"));
    assert_eq!(r.unwrap().1.to_snapshot(), full.to_snapshot());
    // an unknown key after a catalog that doesn't parse: refused for the key, not corrupt
    let (r, m) = open(format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{{\"photos\":7}},\"segments\":[]}}"));
    assert!(matches!(r.err(), Some(CatalogError::Newer(e)) if e.contains("\"segments\"")));
    assert_refused_untouched(&m);
    // a newer stamp after the catalog: refused for the version
    let (r, _) = open(format!("{{\"format\":\"lightcraft-catalog\",\"catalog\":{cat},\"seq\":2,\"version\":{}}}", VERSION + 1));
    assert!(matches!(r.err(), Some(CatalogError::Newer(e)) if e.contains(&format!("v{}", VERSION + 1))));
    // a key twice: corrupt, as the header check finds it
    let (r, _) = open(format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"version\":{VERSION},\"seq\":2,\"catalog\":{cat}}}"));
    assert!(matches!(r.err(), Some(CatalogError::Corrupt(e)) if e.contains("duplicate field")));
}

/// The same refusal through the native store, where the folder is a real directory: the snapshot
/// and log keep their bytes and no file is added (#3962). Before the fix the open succeeded.
#[test]
fn segments_folder_on_disk_is_refused_untouched() {
    let (base, log, _) = legacy_parts();
    for version in READABLE {
        let dir = std::env::temp_dir().join(format!("lc-layout-{version}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("segments")).unwrap();
        std::fs::write(dir.join("segments").join("0001.seg"), b"photos live here").unwrap();
        let snap = snap_at(version, &base, "");
        std::fs::write(dir.join(SNAPSHOT), &snap).unwrap();
        std::fs::write(dir.join(LOG), log.as_bytes()).unwrap();
        let before = names(&dir);
        let e = Journal::open(Box::new(FsStore::open(&dir).unwrap())).err().expect("refused");
        assert!(matches!(e, CatalogError::Newer(_)), "{e:?}");
        assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), snap);
        assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), log.as_bytes());
        assert_eq!(names(&dir), before, "no file added or renamed");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// The entries of `dir`, sorted (what an open may not add to or rename).
fn names(dir: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    v.sort();
    v
}

/// The native store's `has_folder` on disk (#3962): a regular file named `segments` is no folder
/// (the library opens), a `segments` folder with no snapshot beside it is refused and no
/// `catalog.snap` or `catalog.log` is created, and a link to a folder counts as the folder.
#[test]
fn segments_entries_on_disk() {
    let (base, log, full) = legacy_parts();
    let dir = std::env::temp_dir().join(format!("lc-layout-entries-{}", std::process::id()));

    // a file called `segments`: no folder, the library opens
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("segments"), b"not a folder").unwrap();
    std::fs::write(dir.join(SNAPSHOT), snap_at(VERSION, &base, "")).unwrap();
    std::fs::write(dir.join(LOG), log.as_bytes()).unwrap();
    let mut store = FsStore::open(&dir).unwrap();
    assert!(!store.has_folder("segments").unwrap());
    let (_, c, _) = Journal::open(Box::new(store)).unwrap();
    assert_eq!(c.to_snapshot(), full.to_snapshot());
    assert_eq!(std::fs::read(dir.join("segments")).unwrap(), b"not a folder");

    // a `segments` folder and nothing else: refused, nothing created beside it
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("segments")).unwrap();
    std::fs::write(dir.join("segments").join("0001.seg"), b"photos live here").unwrap();
    let before = names(&dir);
    let e = Journal::open(Box::new(FsStore::open(&dir).unwrap())).err().expect("refused");
    assert!(matches!(&e, CatalogError::Newer(m) if m.contains("\"segments\" folder")), "{e:?}");
    assert_eq!(names(&dir), before, "no catalog.snap or catalog.log created");

    // a link to a folder elsewhere: what another build would read, so refused too
    #[cfg(unix)]
    {
        let elsewhere = std::env::temp_dir().join(format!("lc-layout-elsewhere-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&elsewhere);
        std::fs::create_dir_all(&elsewhere).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("segments")).unwrap();
        let snap = snap_at(VERSION, &base, "");
        std::fs::write(dir.join(SNAPSHOT), &snap).unwrap();
        let before = names(&dir);
        assert!(FsStore::open(&dir).unwrap().has_folder("segments").unwrap());
        let e = Journal::open(Box::new(FsStore::open(&dir).unwrap())).err().expect("refused");
        assert!(matches!(e, CatalogError::Newer(_)), "{e:?}");
        assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), snap);
        assert_eq!(names(&dir), before, "no file added or renamed");
        assert_eq!(names(&elsewhere), Vec::<std::ffi::OsString>::new(), "nothing written through the link");
        std::fs::remove_dir_all(&elsewhere).unwrap();
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Controls for #3962: an ordinary library at a readable stamp still opens with its photos (a
/// `segments` *file* or a key-like string inside the catalog is no folder or top-level key), and a
/// stamp above [`VERSION`] is still refused for its version, even with an unknown key beside it.
#[test]
fn ordinary_layout_still_opens_and_a_newer_stamp_still_names_its_version() {
    let (base, log, full) = legacy_parts();
    for version in READABLE {
        let m = MemStore::new();
        m.set(SNAPSHOT, snap_at(version, &base, ""));
        m.set(LOG, log.clone().into_bytes());
        m.set("segments", b"not a folder".to_vec());
        let (_, c, r) = Journal::open(Box::new(m)).unwrap();
        assert_eq!(c.to_snapshot(), full.to_snapshot());
        assert_eq!(r.upgraded_from, (version != VERSION).then_some(version));
    }
    let m = MemStore::new();
    m.set(SNAPSHOT, snap_at(VERSION + 1, &base, r#""segments":[],"#));
    assert_refused_untouched(&m);
    let e = Journal::open(Box::new(m)).err().unwrap().to_string();
    assert!(e.contains(&format!("is catalog format v{}", VERSION + 1)), "{e}");
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
    let snap = format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot());
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
    m.set(
        SNAPSHOT,
        format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes(),
    );
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
            Op::SetEmbeddedLens { .. } => 4,
            Op::SetKeyword { .. } => 5,
            Op::SetFolderRecord { .. } => 6,
            Op::SetAsShotWb { .. } => 7,
            Op::SetHistoryCursor { .. } => 8,
        }
    }
    /// v13: Lens Blur in develop settings and mask adjustments (v12: `Catalog.grids` and `SegMask.grid`; v11: `EmbeddedLens.source`;
    /// v10: `Mask.on_photo`; v9: `Photo.preview_checked` and `Op::SetContent.preview_checked`).
    const NEWEST_FIELDS: u32 = 13;
    let newest = since(&Op::SetHistoryCursor { id: PhotoId(1), cursor: None });
    assert_eq!(newest.max(NEWEST_FIELDS), VERSION, "the newest op's or fields' version must be the current format version");
}
