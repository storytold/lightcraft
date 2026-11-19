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
    /// v14: `Op::Relink.kind`; v13: Lens Blur in develop settings and mask adjustments (v12: `Catalog.grids` and `SegMask.grid`; v11: `EmbeddedLens.source`;
    /// v10: `Mask.on_photo`; v9: `Photo.preview_checked` and `Op::SetContent.preview_checked`).
    const NEWEST_FIELDS: u32 = 14;
    let newest = since(&Op::SetHistoryCursor { id: PhotoId(1), cursor: None });
    assert_eq!(newest.max(NEWEST_FIELDS), VERSION, "the newest op's or fields' version must be the current format version");
}

#[test]
fn v13_relink_replays_without_kind_and_new_classification_has_an_exact_inverse() {
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: "/old.dng".into() }, "old.dng", "DNG", 4, 3, "");
    p.kind = MediaKind::Raw;
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let legacy: Op = serde_json::from_value(serde_json::json!({
        "op": "relink", "id": id.0, "file_name": "moved.dng",
        "source": {"type": "file", "path": "/moved.dng"}, "format": "DNG"
    }))
    .unwrap();
    let record = encode_record(1, &legacy);
    assert!(!record.contains("\"kind\""), "old path-only records omit classification");
    let store = MemStore::new();
    store.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":13,\"seq\":0,\"catalog\":{}}}\n", c.to_snapshot()).into_bytes());
    store.set(LOG, format!("{record}\n").into_bytes());
    let (mut journal, mut c, report) = Journal::open(Box::new(store.clone())).unwrap();
    assert_eq!((report.replayed, report.upgraded_from), (1, Some(13)));
    assert_eq!(snapshot_version(&store), u64::from(VERSION));
    assert_eq!(c.photo(id).unwrap().file_name, "moved.dng");
    assert_eq!(c.photo(id).unwrap().source, Source::File { path: "/moved.dng".into() });
    let before = c.photo(id).unwrap().clone();
    assert_eq!(before.kind, MediaKind::Raw);
    let updated = Op::Relink {
        id,
        file_name: "image.png".into(),
        source: Source::File { path: "/image.png".into() },
        format: Some("PNG".into()),
        kind: Some(MediaKind::Image),
    };
    let inverse = c.apply(updated.clone()).unwrap();
    assert_eq!((c.photo(id).unwrap().kind, c.photo(id).unwrap().format.as_str()), (MediaKind::Image, "PNG"));
    assert!(matches!(&inverse, Op::Relink { kind: Some(MediaKind::Raw), format: Some(format), .. } if format == "DNG"));
    c.apply(inverse).unwrap();
    assert_eq!(c.photo(id).unwrap(), &before);
    c.apply(updated.clone()).unwrap();
    journal.append(&[updated]).unwrap();
    drop(journal);
    let (_, reopened, _) = Journal::open(Box::new(store)).unwrap();
    assert_eq!((reopened.photo(id).unwrap().kind, reopened.photo(id).unwrap().format.as_str()), (MediaKind::Image, "PNG"));
}

/// Fail only the selected atomic replacement; log appends remain writable.
struct FailedReplacement {
    files: MemStore,
    name: &'static str,
}

impl Store for FailedReplacement {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if name == self.name {
            return Err(std::io::Error::other(format!("can't replace {name}")));
        }
        self.files.write_atomic(name, data)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.files.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.files.truncate(name, len)
    }
    fn describe(&self) -> String {
        self.files.describe()
    }
}

#[test]
fn format_upgrade_snapshot_failure_refuses_legacy_writes() {
    for with_log in [false, true] {
        let (base, log, _) = legacy_parts();
        let files = MemStore::new();
        files.set(
            SNAPSHOT,
            format!("{{\"format\":\"lightcraft-catalog\",\"version\":13,\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes(),
        );
        if with_log {
            files.set(LOG, log.into_bytes());
        }
        let before = files.files.lock().unwrap().clone();
        let error =
            Journal::open(Box::new(FailedReplacement { files: files.clone(), name: SNAPSHOT })).err().expect("the v14 barrier must be durable");
        assert!(matches!(error, CatalogError::Io(_)), "{error:?}");
        assert!(error.to_string().contains(SNAPSHOT), "{error}");
        assert_eq!(*files.files.lock().unwrap(), before, "no v14 changes may be appended under the v13 snapshot");
    }
}

#[test]
fn format_upgrade_new_library_failure_is_refused() {
    let files = MemStore::new();
    let error =
        Journal::open(Box::new(FailedReplacement { files: files.clone(), name: SNAPSHOT })).err().expect("a new library needs its format header");
    assert!(matches!(error, CatalogError::Io(_)), "{error:?}");
    assert!(files.files.lock().unwrap().is_empty(), "never return a writable headerless log");
}

#[test]
fn format_upgrade_log_reset_failure_keeps_durable_v14_and_all_edits() {
    let (base, log, _) = legacy_parts();
    let files = MemStore::new();
    files.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":13,\"seq\":2,\"catalog\":{}}}\n", base.to_snapshot()).into_bytes());
    files.set(LOG, log.clone().into_bytes());
    let (mut journal, mut catalog, report) = Journal::open(Box::new(FailedReplacement { files: files.clone(), name: LOG })).unwrap();
    assert_eq!(report.upgraded_from, Some(13));
    assert_eq!(snapshot_version(&files), u64::from(VERSION), "older builds must refuse the library even when log reset fails");
    assert_eq!(files.get(LOG).unwrap(), log.as_bytes());
    assert_eq!((journal.seq(), journal.snapshot_seq()), (3, 3));
    let id = catalog.photos().next().unwrap().id;
    let relink = Op::Relink {
        id,
        file_name: "raw.dng".into(),
        source: Source::File { path: "/raw.dng".into() },
        format: Some("DNG".into()),
        kind: Some(MediaKind::Raw),
    };
    catalog.apply(relink.clone()).unwrap();
    journal.append(&[relink]).unwrap();
    drop(journal);
    let (journal, reopened, report) = Journal::open(Box::new(files)).unwrap();
    assert_eq!((report.stale, report.replayed, report.upgraded_from), (1, 1, None));
    assert_eq!((journal.seq(), journal.snapshot_seq()), (4, 3));
    assert_eq!(reopened.to_snapshot(), catalog.to_snapshot());
    assert_eq!(reopened.photo(id).unwrap().kind, MediaKind::Raw);
}
