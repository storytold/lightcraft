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
    /// v9: `Photo.preview_checked` and `Op::SetContent.preview_checked`.
    const NEWEST_FIELDS: u32 = 9;
    let newest = since(&Op::SetHistoryCursor { id: PhotoId(1), cursor: None });
    assert_eq!(newest.max(NEWEST_FIELDS), VERSION, "the newest op's or fields' version must be the current format version");
}

// ---- an upgrade keeps what it replaces

/// A store whose writes of some files fail.
struct Refusing {
    files: MemStore,
    refuse: fn(&str) -> bool,
}

impl Store for Refusing {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if (self.refuse)(name) {
            return Err(std::io::Error::other("disk full"));
        }
        self.files.write_atomic(name, data)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.files.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.files.truncate(name, len)
    }
    fn remove(&mut self, name: &str) -> std::io::Result<()> {
        self.files.remove(name)
    }
    fn describe(&self) -> String {
        "refusing".into()
    }
}

/// The library `fixtures/v<N>-catalog.*` holds, written by the build of format `N`.
fn fixture(version: u32) -> Option<(&'static str, &'static str)> {
    match version {
        6 => Some((include_str!("fixtures/v6-catalog.snap.json"), include_str!("fixtures/v6-catalog.log.jsonl"))),
        _ => None,
    }
}

/// A format 6 library (the fixture) in a store.
fn v6_library() -> (MemStore, Vec<u8>, Vec<u8>) {
    let (snap, log) = fixture(6).unwrap();
    let m = MemStore::new();
    m.set(SNAPSHOT, snap.as_bytes().to_vec());
    m.set(LOG, log.as_bytes().to_vec());
    (m, snap.as_bytes().to_vec(), log.as_bytes().to_vec())
}

/// Given a library in an older format, when it is opened (which rewrites it in this build's
/// format, and older builds can't read that), then the files it was are kept beside it, byte for
/// byte, under names that say their format and the change they end at; the report names them;
/// and put back under their own names they are the library as it was.
#[test]
fn an_upgrade_keeps_the_files_it_replaces() {
    let (m, snap, log) = v6_library();
    let (_, upgraded, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(r.upgraded_from, Some(6));
    assert_eq!(r.kept, ["catalog.snap.v6-8", "catalog.log.v6-8"]);
    assert_eq!((m.get("catalog.snap.v6-8"), m.get("catalog.log.v6-8")), (Some(snap.clone()), Some(log.clone())));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));

    // the way back: the kept files under their own names
    let back = MemStore::new();
    back.set(SNAPSHOT, m.get("catalog.snap.v6-8").unwrap());
    back.set(LOG, m.get("catalog.log.v6-8").unwrap());
    assert_eq!(snapshot_version(&back), 6);
    let (_, mut as_it_was, _) = Journal::open(Box::new(back)).unwrap();
    as_it_was.revision = upgraded.revision;
    assert_eq!(as_it_was, upgraded);
}

/// Given a library that is in this build's format already, or a new one, when it is opened,
/// then nothing is kept: there is nothing an older build could have read.
#[test]
fn nothing_is_kept_without_an_upgrade() {
    let m = MemStore::new();
    let (_, _, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert!(r.created && r.kept.is_empty());
    let (_, _, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert!(r.kept.is_empty() && r.upgraded_from.is_none());
    assert_eq!(m.files.lock().unwrap().keys().collect::<Vec<_>>(), [SNAPSHOT]);
}

/// Given a library from before snapshots (only a log), when it is opened, then the log is kept
/// (as format 0: there was no number yet) and no snapshot is made up for it.
#[test]
fn an_upgrade_of_a_log_only_library_keeps_the_log() {
    let mut c = Catalog::new();
    let op = photo(&mut c);
    let log = format!("{}\n", encode_record(1, &op)).into_bytes();
    let m = MemStore::new();
    m.set(LOG, log.clone());
    let (_, _, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!((r.upgraded_from, r.kept.as_slice()), (Some(0), ["catalog.log.v0-1".to_string()].as_slice()));
    assert_eq!(m.get("catalog.log.v0-1"), Some(log));
    assert_eq!(m.get("catalog.snap.v0-1"), None);
}

/// Given an older library whose files can't be kept (the disk is full), when it is opened, then
/// it is not upgraded without its way back: it opens as it is, with its files as they were and
/// nothing half kept, and is upgraded once there is room.
#[test]
fn a_library_that_cannot_be_kept_is_not_upgraded() {
    let (m, snap, log) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name.starts_with("catalog.log.v") };
    let (_, c, r) = Journal::open(Box::new(store)).unwrap();
    assert_eq!((c.photos().count(), r.upgraded_from, r.kept.len()), (3, None, 0));
    assert_eq!((m.get(SNAPSHOT), m.get(LOG)), (Some(snap), Some(log)));
    assert_eq!(m.files.lock().unwrap().keys().collect::<Vec<_>>(), [LOG, SNAPSHOT], "nothing half kept");

    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!((c.photos().count(), r.upgraded_from, r.kept.len()), (3, Some(6), 2));
}

/// Given an older library on disk whose files can't be kept (the name of one is taken by a directory), when it is
/// opened, then it is not upgraded, and no copy or half-written staging file of the files it tried to keep is left
/// lying about in the library's folder.
#[test]
fn a_library_on_disk_that_cannot_be_kept_leaves_nothing_lying_about() {
    let dir = std::env::temp_dir().join(format!("lc-keep-fails-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("catalog.log.v6-8")).unwrap();
    let (snap, log) = fixture(6).unwrap();
    std::fs::write(dir.join(SNAPSHOT), snap).unwrap();
    std::fs::write(dir.join(LOG), log).unwrap();

    let (_, c, r) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    assert_eq!((c.photos().count(), r.upgraded_from, r.kept.len()), (3, None, 0));
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, [LOG, "catalog.log.v6-8", SNAPSHOT], "nothing but the library and the name that was taken");
    assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), snap.as_bytes());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given an older library whose upgrade can't be written, when it is opened, then it opens as it
/// was, with no copy of it left lying about: there is nothing to go back from.
#[test]
fn a_failed_upgrade_leaves_no_copy() {
    let (m, snap, log) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name == SNAPSHOT };
    let (_, c, r) = Journal::open(Box::new(store)).unwrap();
    assert_eq!((c.photos().count(), r.upgraded_from, r.kept.len()), (3, None, 0));
    assert_eq!((m.get(SNAPSHOT), m.get(LOG)), (Some(snap), Some(log)));
    assert_eq!(m.files.lock().unwrap().keys().collect::<Vec<_>>(), [LOG, SNAPSHOT]);
}

/// Given an older library whose upgrade is written but whose log then can't be emptied, when it
/// is opened, then it is upgraded all the same (the log's changes are in the snapshot, and are
/// passed over when it is read), and the files it was are kept: its way back is not thrown away
/// because something failed after the upgrade.
#[test]
fn an_upgrade_that_is_written_keeps_its_copies_whatever_fails_after() {
    let (m, snap, log) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name == LOG };
    let (_, c, r) = Journal::open(Box::new(store)).unwrap();
    assert_eq!((c.photos().count(), r.upgraded_from), (3, Some(6)));
    assert_eq!(r.kept, ["catalog.snap.v6-8", "catalog.log.v6-8"]);
    assert_eq!((m.get("catalog.snap.v6-8"), m.get("catalog.log.v6-8")), (Some(snap), Some(log.clone())));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
    // the log it could not empty is all in the snapshot
    assert_eq!(m.get(LOG), Some(log));
    let (_, mut again, r) = Journal::open(Box::new(m)).unwrap();
    again.revision = c.revision;
    assert_eq!((again, r.replayed, r.upgraded_from), (c, 0, None));
}

/// Given an older library whose log is damaged, when it is opened (which saves what could be
/// read in this build's format), then that is an upgrade like any other: the snapshot it was is
/// kept beside the damaged log.
#[test]
fn a_damaged_older_library_keeps_its_snapshot() {
    let (m, snap, log) = v6_library();
    // a record in the middle that is not what was written, with good ones after it
    let mut lines: Vec<String> = String::from_utf8(log).unwrap().lines().map(str::to_string).collect();
    lines[1] = lines[1].replacen("\"crc\":", "\"crc\":1", 1);
    let damaged = format!("{}\n", lines.join("\n")).into_bytes();
    m.set(LOG, damaged.clone());
    let (_, _, r) = Journal::open(Box::new(m.clone())).unwrap();
    let kept_log = r.damaged.clone().expect("damaged");
    assert_eq!((r.upgraded_from, r.kept.as_slice()), (Some(6), ["catalog.snap.v6-4".to_string()].as_slice()), "{r:?}");
    assert_eq!((m.get("catalog.snap.v6-4"), m.get(&kept_log)), (Some(snap), Some(damaged)));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

/// Given an older library whose log is damaged, when the upgrade is written but the damaged log
/// can't be replaced, then the library is refused (working in it would add changes after the
/// damage, where the next open would not read them), with the snapshot it was kept; and it
/// opens, repaired, once the log can be written.
#[test]
fn a_damaged_older_library_is_not_worked_in_with_its_damaged_log_in_place() {
    let (m, snap, log) = v6_library();
    let mut lines: Vec<String> = String::from_utf8(log).unwrap().lines().map(str::to_string).collect();
    lines[1] = lines[1].replacen("\"crc\":", "\"crc\":1", 1);
    m.set(LOG, format!("{}\n", lines.join("\n")).into_bytes());
    let store = Refusing { files: m.clone(), refuse: |name| name == LOG };
    let e = Journal::open(Box::new(store)).err().expect("refused");
    assert!(matches!(&e, CatalogError::Io(why) if why.contains("damaged log")), "{e}");
    assert_eq!(m.get("catalog.snap.v6-4"), Some(snap), "its way back stays");

    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(c.photos().count(), 3);
    assert!(r.damaged.is_some() && m.get(LOG).unwrap().is_empty(), "{r:?}");
}

/// Given an older library whose upgrade fails in a way that leaves it unknown whether it was
/// written (the snapshot can't be read afterwards), then the files it was are kept: the way
/// back is given up only when the library is known to be as it was.
#[test]
fn an_upgrade_that_cannot_be_checked_keeps_its_copies() {
    /// Fails the write of the snapshot, and every read of it after that.
    struct Blind {
        files: MemStore,
        failed: bool,
    }
    impl Store for Blind {
        fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
            if self.failed && name == SNAPSHOT {
                return Err(std::io::Error::other("unreadable"));
            }
            self.files.read(name)
        }
        fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
            if name == SNAPSHOT {
                self.failed = true;
                return Err(std::io::Error::other("the share went away"));
            }
            self.files.write_atomic(name, data)
        }
        fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
            self.files.append(name, data)
        }
        fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
            self.files.truncate(name, len)
        }
        fn remove(&mut self, name: &str) -> std::io::Result<()> {
            self.files.remove(name)
        }
        fn describe(&self) -> String {
            "blind".into()
        }
    }
    let (m, snap, log) = v6_library();
    let e = Journal::open(Box::new(Blind { files: m.clone(), failed: false })).err().expect("refused");
    assert!(matches!(&e, CatalogError::Io(why) if why.contains("can't be told")), "{e}");
    assert_eq!((m.get("catalog.snap.v6-8"), m.get("catalog.log.v6-8")), (Some(snap), Some(log)));
}

/// Given an older library that was not upgraded at open (its files can't be kept), when a later
/// snapshot would upgrade it (a compaction, or closing), then that snapshot is not written: the
/// library stays as it was, with nothing half kept, and is never upgraded without its way back.
#[test]
fn a_library_that_could_not_be_kept_is_not_upgraded_by_a_later_snapshot_either() {
    let (m, snap, log) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name.starts_with("catalog.log.v") };
    let (mut j, c, r) = Journal::open(Box::new(store)).unwrap();
    assert_eq!(r.upgraded_from, None);
    assert!(j.snapshot(&c).is_err(), "closing");
    assert!(j.snapshot_in_background(&c).is_err(), "a compaction");
    assert_eq!((m.get(SNAPSHOT), m.get(LOG)), (Some(snap), Some(log)));
    assert_eq!(m.files.lock().unwrap().keys().collect::<Vec<_>>(), [LOG, SNAPSHOT], "nothing half kept");
}

/// Given an older library whose upgrade could not be written at open (a full disk), when a later
/// snapshot is written (the disk has room again), then the files it was are kept first, as the
/// open would have kept them, and the library is upgraded.
#[test]
fn a_library_left_in_its_old_format_keeps_its_files_when_a_later_snapshot_upgrades_it() {
    static FULL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    let (m, snap, log) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name == SNAPSHOT && FULL.load(std::sync::atomic::Ordering::SeqCst) };
    let (mut j, c, r) = Journal::open(Box::new(store)).unwrap();
    assert_eq!((r.upgraded_from, r.kept.len()), (None, 0));
    assert!(j.snapshot(&c).is_err(), "still full");
    assert_eq!(m.files.lock().unwrap().keys().collect::<Vec<_>>(), [LOG, "catalog.log.v6-8", SNAPSHOT, "catalog.snap.v6-8"], "kept, not upgraded");
    FULL.store(false, std::sync::atomic::Ordering::SeqCst);
    j.snapshot(&c).unwrap();
    assert_eq!((m.get("catalog.snap.v6-8"), m.get("catalog.log.v6-8")), (Some(snap), Some(log)));
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
}

/// Given an older library whose log is damaged and whose snapshot can't be kept, when it is
/// opened, then it is refused and left as it was (it is not repaired without its way back).
#[test]
fn a_damaged_older_library_whose_snapshot_cannot_be_kept_is_refused() {
    let (m, snap, log) = v6_library();
    let mut lines: Vec<String> = String::from_utf8(log).unwrap().lines().map(str::to_string).collect();
    lines[1] = lines[1].replacen("\"crc\":", "\"crc\":1", 1);
    let damaged = format!("{}\n", lines.join("\n")).into_bytes();
    m.set(LOG, damaged.clone());
    let store = Refusing { files: m.clone(), refuse: |name| name.starts_with("catalog.snap.v") };
    let e = Journal::open(Box::new(store)).err().expect("refused");
    assert!(matches!(&e, CatalogError::Io(why) if why.contains("can't be kept")), "{e}");
    assert_eq!((m.get(SNAPSHOT), m.get(LOG)), (Some(snap), Some(damaged)));
}

/// Given an older library that was not upgraded at open (its files can't be kept), when a later
/// snapshot upgrades it, then the files that snapshot kept are told once, after it is written
/// (not before, not for a snapshot that failed, and the open's report has none).
#[test]
fn the_files_a_later_snapshot_keeps_are_told_once_it_is_written() {
    static FULL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    let (m, _, _) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name == SNAPSHOT && FULL.load(std::sync::atomic::Ordering::SeqCst) };
    let (mut j, c, r) = Journal::open(Box::new(store)).unwrap();
    assert!(r.kept.is_empty());
    assert!(j.snapshot(&c).is_err(), "still full");
    assert!(j.take_kept().is_empty(), "kept for a snapshot that is not written: nothing to tell yet");
    FULL.store(false, std::sync::atomic::Ordering::SeqCst);
    j.snapshot(&c).unwrap();
    assert_eq!(j.take_kept(), ["catalog.snap.v6-8", "catalog.log.v6-8"]);
    assert!(j.take_kept().is_empty(), "once");
}

/// Given a library opened in its old format (its upgrade could not be written) that is then
/// changed, in this build's ops, before any snapshot is tried, when a later snapshot upgrades it,
/// then the files kept are the library as it was at open, under the names the open would have
/// given them: not the log as it is now, and nothing piles up. (An older build can read them
/// whatever was appended since.)
#[test]
fn files_kept_by_a_later_snapshot_are_the_library_as_it_was_at_open() {
    static FULL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    let (m, snap, log) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name == SNAPSHOT && FULL.load(std::sync::atomic::Ordering::SeqCst) };
    let (mut j, c, _) = Journal::open(Box::new(store)).unwrap();
    j.append(&[Op::SetLabelName { label: ColorLabel::ALL[0], name: Some("x".into()) }]).unwrap();
    assert!(m.get(LOG).unwrap().len() > log.len(), "the log has grown");
    assert!(j.snapshot(&c).is_err(), "still full");
    FULL.store(false, std::sync::atomic::Ordering::SeqCst);
    j.snapshot(&c).unwrap();
    assert_eq!(j.take_kept(), ["catalog.snap.v6-8", "catalog.log.v6-8"]);
    assert_eq!((m.get("catalog.snap.v6-8"), m.get("catalog.log.v6-8")), (Some(snap), Some(log)));
    assert_eq!(m.files.lock().unwrap().keys().filter(|k| k.contains(".v6-")).count(), 2, "{:?}", m.files.lock().unwrap().keys());
}

/// Given a library left in its old format whose snapshot is in this build's format already (a
/// write that landed and reported a failure, before anything was kept), when snapshots are
/// written, then each of them works: the upgrade is done, with nothing to keep (the old files
/// are gone from under that name), and nothing is refused until the library is reopened.
#[test]
fn a_snapshot_that_landed_is_the_upgrade_done() {
    static FULL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    let (m, _, log) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name == SNAPSHOT && FULL.load(std::sync::atomic::Ordering::SeqCst) };
    let (mut j, c, _) = Journal::open(Box::new(store)).unwrap();
    FULL.store(false, std::sync::atomic::Ordering::SeqCst);
    m.set(SNAPSHOT, format!("{{\"format\":\"lightcraft-catalog\",\"version\":{VERSION},\"seq\":8,\"catalog\":{}}}\n", c.to_snapshot()).into_bytes());
    j.snapshot(&c).unwrap();
    j.snapshot(&c).unwrap();
    assert!(j.take_kept().is_empty());
    assert_eq!(snapshot_version(&m), u64::from(VERSION));
    assert!(!m.files.lock().unwrap().keys().any(|k| k.contains(".v6-")), "nothing kept from a snapshot that is not the old one");
    let _ = log;
}

/// Given a library left in its old format, when a snapshot lands after its files were kept but
/// reports a failure, and the snapshot is tried again, then the retry goes through, and the files
/// told are the old library, not the snapshot that is there now.
#[test]
fn a_snapshot_that_landed_after_its_files_were_kept_is_tried_again() {
    static LAND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    struct Landing {
        inner: Refusing,
    }
    impl Store for Landing {
        fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
            self.inner.read(name)
        }
        fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
            use std::sync::atomic::Ordering::SeqCst;
            if name == SNAPSHOT && LAND.load(SeqCst) == 1 {
                // lands, and says it did not (the share dropped the answer)
                self.inner.files.write_atomic(name, data)?;
                LAND.store(2, SeqCst);
                return Err(std::io::Error::other("the share went away"));
            }
            self.inner.write_atomic(name, data)
        }
        fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
            self.inner.append(name, data)
        }
        fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
            self.inner.truncate(name, len)
        }
        fn remove(&mut self, name: &str) -> std::io::Result<()> {
            self.inner.remove(name)
        }
        fn describe(&self) -> String {
            "landing".into()
        }
    }
    let (m, snap, log) = v6_library();
    let store =
        Landing { inner: Refusing { files: m.clone(), refuse: |name| name == SNAPSHOT && LAND.load(std::sync::atomic::Ordering::SeqCst) == 0 } };
    let (mut j, c, _) = Journal::open(Box::new(store)).unwrap();
    LAND.store(1, std::sync::atomic::Ordering::SeqCst);
    assert!(j.snapshot(&c).is_err(), "landed, and said it did not");
    assert_eq!(snapshot_version(&m), u64::from(VERSION), "it did land");
    j.snapshot(&c).unwrap();
    assert_eq!(j.take_kept(), ["catalog.snap.v6-8", "catalog.log.v6-8"]);
    assert_eq!((m.get("catalog.snap.v6-8"), m.get("catalog.log.v6-8")), (Some(snap), Some(log)), "the old library, not the snapshot that landed");
    assert!(j.take_kept().is_empty(), "told once");
}

/// Given a library left in its old format whose snapshot failed after its files were kept, when
/// the snapshot is tried again (a compaction retries as the log grows), then the files are not
/// copied again: a large library is not written out in full at every retry.
#[test]
fn files_kept_for_a_failed_snapshot_are_not_written_again_on_a_retry() {
    static FULL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    let (m, _, _) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name == SNAPSHOT && FULL.load(std::sync::atomic::Ordering::SeqCst) };
    let (mut j, c, _) = Journal::open(Box::new(store)).unwrap();
    assert!(j.snapshot(&c).is_err());
    m.set("catalog.snap.v6-8", b"marker".to_vec());
    assert!(j.snapshot(&c).is_err(), "still full");
    assert_eq!(m.get("catalog.snap.v6-8"), Some(b"marker".to_vec()), "not written again");
    FULL.store(false, std::sync::atomic::Ordering::SeqCst);
    j.snapshot(&c).unwrap();
    assert_eq!(j.take_kept(), ["catalog.snap.v6-8", "catalog.log.v6-8"]);
}
