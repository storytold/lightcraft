//! Issue #102: catalog format versions. Older formats load and are upgraded; a newer format is
//! refused with a clear error and left untouched (never read in part, truncated or set aside).

use crate::journal::{FORMATS, LOG, SNAPSHOT, VERSION, encode_record};
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
/// this match. When it does, add a row to `journal::FORMATS` (flagged `NEW_OP`) and the variant
/// here under the new number. A format that only adds serialized fields is a `FIELDS` row instead.
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
    // (formats 9 to 11 added no op: 9 added fields, 10 changed how records are stored,
    // `crate::stored`, and 11 where, `crate::segments`)
    let newest = since(&Op::SetHistoryCursor { id: PhotoId(1), cursor: None });
    // the newest format in this build that adds an op is the newest op's, and says so
    let adds_an_op = crate::journal::newest_op_format(FORMATS).expect("a format adds an op");
    assert_eq!(adds_an_op.version, newest, "the newest op's format is the newest row of FORMATS that adds one: {adds_an_op:?}");
    assert!(adds_an_op.adds.contains("Op::SetHistoryCursor"), "{adds_an_op:?}");
}

/// A format that only adds a field may name the op that carries it ("`Op::SetContent`'s `preview_checked`", as #974
/// does): which formats add an op is each row's own flag ([`crate::journal::Format::adds_op`]), never its wording.
#[test]
fn a_format_that_only_adds_a_field_may_name_an_op() {
    use crate::journal::{Format, newest_op_format};
    let row = |version, adds, adds_op| Format { version, written_by: "a build", adds, adds_op, here: true };
    let formats = [row(1, "", false), row(2, "`Op::SetBrowsed`", true), row(3, "`Op::SetContent`'s `preview_checked`", false)];
    assert_eq!(newest_op_format(&formats).map(|f| f.version), Some(2), "format 3 adds a field, whatever it names");
}

/// Given the list of catalog formats, then each row that says it adds an op names the op it adds: the flag and the
/// words don't part.
#[test]
fn a_format_that_adds_an_op_names_it() {
    for f in FORMATS.iter().filter(|f| f.adds_op) {
        assert!(f.adds.contains("`Op::"), "format {} adds an op: its row names it ({:?})", f.version, f.adds);
    }
}

/// Given the list of catalog formats, then each number is there once, in order and without a
/// gap, and what this build writes is the newest of them it has (a number spoken for by a change
/// that is not here yet, [`Format::here`] false, comes after it): a number never means two formats.
#[test]
fn a_format_number_means_one_format() {
    let numbers: Vec<u32> = FORMATS.iter().map(|f| f.version).collect();
    assert_eq!(numbers, (1..=FORMATS.len() as u32).collect::<Vec<_>>());
    let here: Vec<u32> = FORMATS.iter().filter(|f| f.here).map(|f| f.version).collect();
    assert_eq!(here, (1..=VERSION).collect::<Vec<_>>(), "this build has every format up to the one it writes, and none after");
}

/// The library a format's fixture holds (`fixtures/v<N>-…`, written by the build of format `N`):
/// its files, by their names in the library.
fn fixture(version: u32) -> Option<Vec<(&'static str, &'static str)>> {
    match version {
        6 => Some(vec![(SNAPSHOT, include_str!("fixtures/v6-catalog.snap.json")), (LOG, include_str!("fixtures/v6-catalog.log.jsonl"))]),
        7 => Some(vec![(SNAPSHOT, include_str!("fixtures/v7-catalog.snap.json")), (LOG, include_str!("fixtures/v7-catalog.log.jsonl"))]),
        8 => Some(vec![(SNAPSHOT, include_str!("fixtures/v8-catalog.snap.json")), (LOG, include_str!("fixtures/v8-catalog.log.jsonl"))]),
        9 => Some(vec![(SNAPSHOT, include_str!("fixtures/v9-catalog.snap.json")), (LOG, include_str!("fixtures/v9-catalog.log.jsonl"))]),
        10 => Some(vec![(SNAPSHOT, include_str!("fixtures/v10-catalog.snap.json")), (LOG, include_str!("fixtures/v10-catalog.log.jsonl"))]),
        // (in segments, which is what format 11 adds)
        11 => Some(vec![
            (SNAPSHOT, include_str!("fixtures/v11-catalog.snap.json")),
            (LOG, include_str!("fixtures/v11-catalog.log.jsonl")),
            ("segments/7-3.json", include_str!("fixtures/v11-segments-7-3.json")),
        ]),
        _ => None,
    }
}

/// A fixture's files in a store.
fn store_of(files: &[(&'static str, &'static str)]) -> MemStore {
    let m = MemStore::new();
    for (name, content) in files {
        m.set(name, content.as_bytes().to_vec());
    }
    m
}

/// Given a library written by the build of each format from 6 on (its fixture), when this build
/// opens it, then it reads as that library and is left in this build's format. A format whose
/// change is not in this build yet has no fixture here; every other one must have its own.
#[test]
fn every_format_has_a_fixture_that_opens_as_it_was() {
    for f in FORMATS.iter().filter(|f| f.version >= 6) {
        let expected = crate::tests_stored::fixture_library(f.version);
        let Some(files) = fixture(f.version) else {
            assert!(!f.here, "format {} has no fixture (tests_stored::write_this_formats_fixture writes it)", f.version);
            continue;
        };
        assert!(f.here, "format {} has a fixture: it is in this build", f.version);
        let snap = files.iter().find(|(name, _)| *name == SNAPSHOT).map(|(_, snap)| *snap).expect("a snapshot");
        assert!(snap.starts_with(&format!("{{\"format\":\"lightcraft-catalog\",\"version\":{},", f.version)), "v{}", f.version);
        let m = store_of(&files);
        let (_, mut loaded, r) = Journal::open(Box::new(m.clone())).unwrap();
        loaded.revision = expected.revision;
        assert_eq!(loaded, expected, "v{}", f.version);
        assert_eq!((r.failed, r.upgraded_from), (0, (f.version != VERSION).then_some(f.version)), "v{}", f.version);
        assert_eq!(snapshot_version(&m), u64::from(VERSION), "v{}", f.version);
    }
    // the format that put the photos in segments has a fixture that is in segments
    let v11 = fixture(11).unwrap();
    assert!(v11.iter().any(|(name, _)| name.starts_with(crate::journal::SEGMENT_PREFIX)) && v11[0].1.contains("\"photos\":{}"));
}

// ---- an upgrade keeps what it replaces

/// A store whose writes of some files fail.
pub(crate) struct Refusing {
    pub(crate) files: MemStore,
    pub(crate) refuse: fn(&str) -> bool,
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

/// A format 6 library (the fixture) in a store.
fn v6_library() -> (MemStore, Vec<u8>, Vec<u8>) {
    let m = store_of(&fixture(6).unwrap());
    let (snap, log) = (m.get(SNAPSHOT).unwrap(), m.get(LOG).unwrap());
    (m, snap, log)
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
/// it is refused, and says why: it is not upgraded without its way back, and it is not worked in
/// as it is (it states no defaults for the records this build would add). Its files stay as they
/// were, nothing half kept is left, and it opens once there is room.
#[test]
fn a_library_that_cannot_be_kept_is_not_opened() {
    let (m, snap, log) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name.starts_with("catalog.log.v") };
    let e = Journal::open(Box::new(store)).err().expect("refused");
    assert!(matches!(&e, CatalogError::Io(why) if why.contains("older format (v6)") && why.contains("can't be kept")), "{e}");
    assert_eq!((m.get(SNAPSHOT), m.get(LOG)), (Some(snap), Some(log)));
    assert_eq!(m.files.lock().unwrap().keys().collect::<Vec<_>>(), [LOG, SNAPSHOT], "nothing half kept");

    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!((c.photos().count(), r.upgraded_from, r.kept.len()), (3, Some(6), 2));
}

/// Given an older library on disk whose files can't be kept (the name of one is taken by a directory), when it is
/// opened, then it is refused, and no copy or half-written staging file of the files it tried to keep is left
/// lying about in the library's folder.
#[test]
fn a_library_on_disk_that_cannot_be_kept_leaves_nothing_lying_about() {
    let dir = std::env::temp_dir().join(format!("lc-keep-fails-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("catalog.log.v6-8")).unwrap();
    let files = fixture(6).unwrap();
    for (name, content) in &files {
        std::fs::write(dir.join(name), content).unwrap();
    }

    let e = Journal::open(Box::new(FsStore::open(&dir).unwrap())).err().expect("refused");
    assert!(matches!(&e, CatalogError::Io(why) if why.contains("older format (v6)") && why.contains("can't be kept")), "{e}");
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, [LOG, "catalog.log.v6-8", SNAPSHOT], "nothing but the library and the name that was taken");
    assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), files.iter().find(|(n, _)| *n == SNAPSHOT).unwrap().1.as_bytes());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given an older library whose upgrade can't be written, when it is opened, then it is refused
/// and is as it was, with no copy of it left lying about: there is nothing to go back from.
#[test]
fn a_failed_upgrade_leaves_no_copy() {
    let (m, snap, log) = v6_library();
    let store = Refusing { files: m.clone(), refuse: |name| name == SNAPSHOT };
    let e = Journal::open(Box::new(store)).err().expect("refused");
    assert!(matches!(&e, CatalogError::Io(why) if why.contains("can't be upgraded")), "{e}");
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

/// Given a place a new library can't be written to, when one is opened there, then it is
/// refused and nothing is left: a library without its first snapshot would be a log that states
/// no defaults for its records.
#[test]
fn a_new_library_that_cannot_be_written_is_not_opened() {
    let m = MemStore::new();
    let store = Refusing { files: m.clone(), refuse: |name| name == SNAPSHOT };
    let e = Journal::open(Box::new(store)).err().expect("refused");
    assert!(matches!(&e, CatalogError::Io(why) if why.contains("new library")), "{e}");
    assert!(m.files.lock().unwrap().is_empty());
}
