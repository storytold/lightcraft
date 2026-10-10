//! The v4 store (`db`), the catalog folder (`library`), remote identities and the format-4 fields.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dac_develop::DevelopSettings;

use crate::db::{DB_FILE, Index};
use crate::journal::{LOG, SNAPSHOT, encode_record};
use crate::library::{self, BackupSchedule, CatalogSettings, RecentCatalogs};
use crate::*;

/// A scratch folder removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("dac-v4-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn photo(c: &mut Catalog, i: u64) -> Photo {
    let id = c.alloc_photo_id();
    let mut p = Photo::new(
        id,
        Source::File { path: format!("/pics/{}/IMG_{i:05}.jpg", 2020 + i % 3) },
        &format!("IMG_{i:05}.jpg"),
        "JPEG",
        40,
        30,
        "2026-01-01T00:00:00",
    );
    p.captured = Some(format!("{}-0{}-1{}T10:00:00", 2020 + i % 3, 1 + i % 9, i % 10));
    p.meta.camera = ["Cam A", "Cam B"][(i % 2) as usize].into();
    p.meta.keywords = vec![["travel", "birds", "Family"][(i % 3) as usize].into()];
    p.rating = (i % 6) as u8;
    if i.is_multiple_of(4) {
        let mut d = DevelopSettings::default();
        d.light.exposure = i as f64 * 0.01;
        let s = Arc::new(d);
        p.develop = s.clone();
        p.edited = Some("2026-01-02T00:00:00".into());
        p.history = vec![HistoryStep { label: "Exposure".into(), settings: s.clone() }];
        p.versions = vec![Version { name: "v".into(), created: "2026-01-02T00:00:00".into(), settings: s, auto: false }];
    }
    if i.is_multiple_of(5) {
        p.import_look = Some(Arc::new(DevelopSettings::default()));
    }
    p
}

/// A library with photos, an album, a stack, a link, a preview entry, the format-4 fields.
fn fill(j: &mut Journal, c: &mut Catalog, n: u64) {
    let mut ops: Vec<Op> = (0..n).map(|i| Op::AddPhoto { photo: Box::new(photo(c, i)) }).collect();
    let ids: Vec<PhotoId> = ops.iter().filter_map(|o| if let Op::AddPhoto { photo } = o { Some(photo.id) } else { None }).collect();
    let mut album = Album::new(c.alloc_album_id(), "Best");
    album.photos = ids.iter().copied().take(3).collect();
    ops.push(Op::AddAlbum { album });
    if ids.len() >= 5 {
        ops.push(Op::AddStack { stack: Stack { id: c.alloc_stack_id(), photos: vec![ids[3], ids[4]], collapsed: true } });
    }
    ops.push(Op::SetSha1 { id: ids[0], sha1: Some("A9993E364706816ABA3E25717850C26C9CD0D89D".into()) });
    ops.push(Op::SetXmpStamp { id: ids[0], stamp: Some(XmpStamp { fingerprint: 7, file: Some(SidecarStat { mtime: 5, size: 9 }) }) });
    ops.push(Catalog::link_remote_op(link(ids[1], "asset-1")));
    ops.push(Op::SetPreview {
        id: ids[1],
        entry: Some(PreviewEntry { size: 2048, one_to_one: false, settings_hash: 3, built_at: "2026-01-01T00:00:00".into() }),
    });
    ops.push(Op::SetLabelName { label: ColorLabel::Red, name: Some("Print".into()) });
    for op in &ops {
        c.apply(op.clone()).unwrap();
    }
    j.append(&ops).unwrap();
}

fn link(id: PhotoId, remote: &str) -> RemoteIdentity {
    RemoteIdentity {
        photo_id: id,
        service: "immich".into(),
        account_id: "https://photos.example/u1".into(),
        remote_id: remote.into(),
        remote_checksum: Some("qZk+NkcGgWq6PiVxeFDCbJzQ2J0=".into()),
        remote_updated_at: None,
        last_synced_at: Some("2026-01-01T00:00:00".into()),
        sync_state: SyncState::Synced,
    }
}

fn open(dir: &Path) -> (Journal, Catalog, LoadReport) {
    Journal::open(Box::new(FsStore::open(dir).unwrap())).unwrap()
}

#[test]
fn roundtrip_through_checkpoints_and_the_log() {
    let s = Scratch::new("rt");
    let (mut j, mut c, r) = open(s.path());
    assert!(r.created && j.has_db());
    assert!(s.path().join(DB_FILE).exists() && library::find_entry(s.path()).is_some());
    fill(&mut j, &mut c, 40);
    drop(j);
    // replayed from the log
    let (mut j, c2, r) = open(s.path());
    assert_eq!(c2.to_snapshot(), c.to_snapshot());
    assert!(r.replayed > 0);
    j.snapshot(&c2).unwrap();
    assert_eq!(std::fs::metadata(s.path().join(LOG)).unwrap().len(), 0);
    drop(j);
    // from the store alone
    let (_, c3, r) = open(s.path());
    assert_eq!((r.replayed, r.failed), (0, 0));
    assert_eq!(c3.to_snapshot(), c.to_snapshot());
    // unedited photos share one settings value in memory
    let unedited: Vec<&Arc<Photo>> = c3.photos().filter(|p| p.edited.is_none()).collect();
    assert!(unedited.len() > 2 && unedited.windows(2).all(|w| Arc::ptr_eq(&w[0].develop, &w[1].develop)));
    // and an edited photo's develop, History step and Version are one value too
    let e = c3.photos().find(|p| p.edited.is_some()).unwrap();
    assert!(Arc::ptr_eq(&e.develop, &e.history[0].settings) && Arc::ptr_eq(&e.develop, &e.versions[0].settings));
}

#[test]
fn checkpoints_write_only_what_changed() {
    let s = Scratch::new("inc");
    let (mut j, mut c, _) = open(s.path());
    fill(&mut j, &mut c, 30);
    j.snapshot(&c).unwrap();
    assert_eq!(j.db().unwrap().last_checkpoint().photos_written, 30);
    let ids: Vec<PhotoId> = c.photos().map(|p| p.id).collect();
    let ops = [Op::SetRating { id: ids[7], rating: 5 }, Op::SetFlag { id: ids[8], flag: Flag::Pick }];
    for op in &ops {
        c.apply(op.clone()).unwrap();
    }
    j.append(&ops).unwrap();
    j.snapshot(&c).unwrap();
    assert_eq!(j.db().unwrap().last_checkpoint().photos_written, 2);
    let del = c.delete_photos_permanently_ops(&[ids[1], ids[2]]);
    c.apply(del.clone()).unwrap();
    j.append(&[del]).unwrap();
    j.snapshot(&c).unwrap();
    drop(j);
    let (_, c2, _) = open(s.path());
    assert_eq!(c2.to_snapshot(), c.to_snapshot());
    assert!(c2.photo(ids[1]).is_none() && c2.remote_of(ids[1]).next().is_none() && c2.preview_entry(ids[1]).is_none());
}

#[test]
fn lazy_reads_and_indexes_follow_the_catalog() {
    let s = Scratch::new("lazy");
    let (mut j, mut c, _) = open(s.path());
    fill(&mut j, &mut c, 30);
    j.snapshot(&c).unwrap();
    let ids: Vec<PhotoId> = c.photos().map(|p| p.id).collect();
    // a keyword changes: the index follows at the next checkpoint
    let mut meta = c.photo(ids[0]).unwrap().meta.clone();
    meta.keywords = vec!["Owls".into()];
    let op = Op::SetMeta { id: ids[0], meta: Box::new(meta) };
    c.apply(op.clone()).unwrap();
    j.append(&[op]).unwrap();
    j.snapshot(&c).unwrap();
    let db = j.db().unwrap();
    assert_eq!(db.photo_count().unwrap(), 30);
    for id in [ids[0], ids[4]] {
        assert_eq!(db.read_photo(id).unwrap().as_ref(), Some(c.photo(id).unwrap().as_ref()));
    }
    assert_eq!(db.read_photo(PhotoId(9999)).unwrap(), None);
    let page = db.page_ids(None, 10).unwrap();
    assert_eq!(page, ids[..10]);
    assert_eq!(db.page_ids(page.last().copied(), 100).unwrap(), ids[10..]);
    assert_eq!(db.ids_where(Index::Keyword("owls")).unwrap(), vec![ids[0]]);
    let want = |f: &dyn Fn(&Photo) -> bool| c.photos().filter(|p| f(p)).map(|p| p.id).collect::<Vec<_>>();
    assert_eq!(db.ids_where(Index::Keyword("family")).unwrap(), want(&|p| p.meta.keywords.iter().any(|k| k == "Family")));
    assert_eq!(db.ids_where(Index::Camera("cam a")).unwrap(), want(&|p| p.meta.camera == "Cam A"));
    assert_eq!(db.ids_where(Index::Captured("2021")).unwrap(), want(&|p| p.captured.as_deref().is_some_and(|d| d.starts_with("2021"))));
    assert_eq!(db.ids_where(Index::Folder("/pics/2022")).unwrap(), want(&|p| folder_of(p).as_deref() == Some("/pics/2022")));
    assert_eq!(db.photo_of_remote("immich", "https://photos.example/u1", "asset-1").unwrap(), Some(ids[1]));
    assert_eq!(db.remote_of_photo(ids[1]).unwrap(), vec![link(ids[1], "asset-1")]);
}

#[test]
fn remote_links_are_indexed_both_ways_and_undoable() {
    let mut c = Catalog::new();
    let a = c.alloc_photo_id();
    let b = c.alloc_photo_id();
    for id in [a, b] {
        c.apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Demo { scene: 0 }, "x.jpg", "JPEG", 1, 1, "t")) }).unwrap();
    }
    let undo = c.apply(Catalog::link_remote_op(link(a, "r1"))).unwrap();
    assert_eq!(c.photo_of_remote("immich", "https://photos.example/u1", "r1"), Some(a));
    // one remote asset, one photo
    assert!(matches!(c.apply(Catalog::link_remote_op(link(b, "r1"))), Err(CatalogError::Invalid(_))));
    // relinking the same photo on the same account replaces its link
    let mut l = link(a, "r2");
    l.sync_state = SyncState::LocalChanged;
    c.apply(Catalog::link_remote_op(l.clone())).unwrap();
    assert_eq!(c.photo_of_remote("immich", "https://photos.example/u1", "r1"), None);
    assert_eq!(c.remote_of(a).collect::<Vec<_>>(), vec![&l]);
    // no link for a photo that isn't there; a key that doesn't match its record
    assert!(c.apply(Catalog::link_remote_op(link(PhotoId(77), "r3"))).is_err());
    let mut bad = link(a, "r4");
    bad.service = "other".into();
    assert!(c.apply(Op::SetRemote { photo: a, service: "immich".into(), account_id: bad.account_id.clone(), record: Some(Box::new(bad)) }).is_err());
    // the JSON snapshot keeps them (and rebuilds the reverse index)
    let back = Catalog::from_snapshot(&c.to_snapshot()).unwrap();
    assert_eq!(back.photo_of_remote("immich", "https://photos.example/u1", "r2"), Some(a));
    // undoing the first link (its inverse: no link on that account) unlinks the photo
    c.apply(undo).unwrap();
    assert_eq!(c.remote_of(a).count(), 0);
    assert_eq!(c.photo_of_remote("immich", "https://photos.example/u1", "r2"), None);
}

#[test]
fn sha1_is_validated_and_found() {
    let mut c = Catalog::new();
    let a = c.alloc_photo_id();
    c.apply(Op::AddPhoto { photo: Box::new(Photo::new(a, Source::Demo { scene: 0 }, "x.jpg", "JPEG", 1, 1, "t")) }).unwrap();
    assert!(c.apply(Op::SetSha1 { id: a, sha1: Some("abc".into()) }).is_err());
    assert!(c.apply(Op::SetSha1 { id: a, sha1: Some("z".repeat(40)) }).is_err());
    let undo = c.apply(Op::SetSha1 { id: a, sha1: Some("A".repeat(40)) }).unwrap();
    assert_eq!(c.photo(a).unwrap().sha1.as_deref(), Some("a".repeat(40).as_str()));
    assert_eq!(c.photos_with_sha1(&"A".repeat(40)), vec![a]);
    c.apply(undo).unwrap();
    assert_eq!(c.photo(a).unwrap().sha1, None);
}

/// A v3 library (JSON snapshot in format 3 + log records after it) is migrated on open: backed
/// up byte for byte, every photo and op carried over, and opened as v4 from then on; builds
/// before v4 refuse the folder.
#[test]
fn v3_library_is_migrated_with_a_backup() {
    let n: u64 = std::env::var("DAC_MIGRATE_PHOTOS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
    let s = Scratch::new("mig");
    let mut c = Catalog::new();
    for i in 0..n {
        let p = photo(&mut c, i);
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let ids: Vec<PhotoId> = c.photos().map(|p| p.id).collect();
    let snap = format!("{{\"format\":\"dac-catalog\",\"version\":3,\"seq\":{n},\"catalog\":{}}}\n", c.to_snapshot());
    let mut log = String::new();
    for (k, id) in ids.iter().take(5).enumerate() {
        let op = Op::SetRating { id: *id, rating: 5 };
        c.apply(op.clone()).unwrap();
        log.push_str(&encode_record(n + 1 + k as u64, &op));
        log.push('\n');
    }
    std::fs::write(s.path().join(SNAPSHOT), &snap).unwrap();
    std::fs::write(s.path().join(LOG), &log).unwrap();

    let t = std::time::Instant::now();
    let (j, got, r) = open(s.path());
    eprintln!("migrated {n} photos in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    assert_eq!(r.upgraded_from, Some(3));
    assert_eq!(got.to_snapshot(), c.to_snapshot());
    assert_eq!(j.seq(), n + 5);
    drop(j);
    // the backup holds the v3 files unchanged
    let backups: Vec<PathBuf> = std::fs::read_dir(s.path().join("backups")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read_to_string(backups[0].join(SNAPSHOT)).unwrap(), snap);
    assert_eq!(std::fs::read_to_string(backups[0].join(LOG)).unwrap(), log);
    // older builds see a format they don't know (and this build's JSON reader refuses it too)
    assert!(matches!(Journal::open(Box::new(FsStore::open_json(s.path()).unwrap())), Err(CatalogError::Corrupt(_))));
    // opened again: v4 directly
    let (_, again, r) = open(s.path());
    assert_eq!((r.upgraded_from, r.replayed), (None, 0));
    assert_eq!(again.to_snapshot(), c.to_snapshot());
}

/// A crash mid-migration (a partial store under its temporary name) is redone from the v3 files.
#[test]
fn interrupted_migration_is_redone() {
    let s = Scratch::new("mig-crash");
    let mut c = Catalog::new();
    for i in 0..10 {
        let p = photo(&mut c, i);
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    std::fs::write(s.path().join(SNAPSHOT), format!("{{\"format\":\"dac-catalog\",\"version\":3,\"seq\":10,\"catalog\":{}}}\n", c.to_snapshot()))
        .unwrap();
    std::fs::write(s.path().join(format!("{DB_FILE}.migrating")), b"partial").unwrap();
    let (_, got, r) = open(s.path());
    assert_eq!(r.upgraded_from, Some(3));
    assert_eq!(got.to_snapshot(), c.to_snapshot());
}

#[test]
fn damaged_or_missing_store_is_an_error_not_a_crash() {
    // garbage in place of the store
    let s = Scratch::new("bad");
    std::fs::write(s.path().join(DB_FILE), b"this is not a database at all, just text that is long enough").unwrap();
    assert!(Journal::open(Box::new(FsStore::open(s.path()).unwrap())).is_err());
    // a truncated store
    let s2 = Scratch::new("trunc");
    {
        let (mut j, mut c, _) = open(s2.path());
        fill(&mut j, &mut c, 20);
        j.snapshot(&c).unwrap();
    }
    let p = s2.path().join(DB_FILE);
    let bytes = std::fs::read(&p).unwrap();
    for cut in [bytes.len() / 2, 4096, 100] {
        std::fs::write(&p, &bytes[..cut.min(bytes.len())]).unwrap();
        let _ = Journal::open(Box::new(FsStore::open(s2.path()).unwrap()));
    }
    // random bytes flipped across the file: errors or a load with skipped records, never a panic
    for seed in 0..16u64 {
        let mut b = bytes.clone();
        for k in 0..32u64 {
            let at = (query::mix64(seed * 1000 + k) % b.len() as u64) as usize;
            b[at] ^= 0x5a;
        }
        std::fs::write(&p, &b).unwrap();
        let _ = Journal::open(Box::new(FsStore::open(s2.path()).unwrap()));
    }
    // the stub without its store
    let s3 = Scratch::new("lost");
    {
        open(s3.path());
    }
    std::fs::remove_file(s3.path().join(DB_FILE)).unwrap();
    assert!(matches!(Journal::open(Box::new(FsStore::open(s3.path()).unwrap())), Err(CatalogError::Corrupt(_))));
}

#[test]
fn a_store_changed_by_another_writer_is_rewritten_whole() {
    // (the app allows one session per library, `LibraryLock`; this is the store's own guard)
    let s = Scratch::new("twice");
    let (mut a, mut ca, _) = open(s.path());
    fill(&mut a, &mut ca, 10);
    a.snapshot(&ca).unwrap();
    let (mut b, mut cb, _) = open(s.path());
    let ids: Vec<PhotoId> = ca.photos().map(|p| p.id).collect();
    // a removes a photo and checkpoints; b, still on the old baseline, rates another
    let del = ca.delete_photos_permanently_ops(&[ids[2]]);
    ca.apply(del.clone()).unwrap();
    a.append(&[del]).unwrap();
    a.snapshot(&ca).unwrap();
    let op = Op::SetRating { id: ids[5], rating: 1 };
    cb.apply(op.clone()).unwrap();
    b.append(&[op]).unwrap();
    b.snapshot(&cb).unwrap();
    drop((a, b));
    // the store is b's catalog, whole (not b's change patched over a's)
    let db = crate::db::CatalogDb::open(&s.path().join(DB_FILE)).and_then(|mut d| d.load()).unwrap();
    assert_eq!(db.0.to_snapshot(), cb.to_snapshot());
}

#[test]
fn a_newer_store_is_refused_untouched() {
    let s = Scratch::new("newer");
    {
        let (mut j, mut c, _) = open(s.path());
        fill(&mut j, &mut c, 3);
        j.snapshot(&c).unwrap();
    }
    {
        let db = redb::Database::open(s.path().join(DB_FILE)).unwrap();
        let w = db.begin_write().unwrap();
        w.open_table(redb::TableDefinition::<&str, &[u8]>::new("meta")).unwrap().insert("version", b"99".as_slice()).unwrap();
        w.commit().unwrap();
    }
    let before = std::fs::read(s.path().join(LOG)).unwrap();
    assert!(matches!(Journal::open(Box::new(FsStore::open(s.path()).unwrap())), Err(CatalogError::Newer(_))));
    // nothing was written: the log and the stub are as they were, and it is still refused
    assert_eq!(std::fs::read(s.path().join(LOG)).unwrap(), before);
    assert!(matches!(Journal::open(Box::new(FsStore::open(s.path()).unwrap())), Err(CatalogError::Newer(_))));
}

#[test]
fn catalog_folder_new_open_recent() {
    let s = Scratch::new("lib");
    let entry = library::create(s.path(), "Family Photos").unwrap();
    assert_eq!(entry.extension().unwrap(), dac_brand::CATALOG_EXT);
    assert_eq!(entry.parent().unwrap(), s.path().join("Family Photos"));
    assert!(library::create(s.path(), "Family Photos").is_err(), "not over an existing catalog");
    assert!(library::create(s.path(), "../x").is_err() && library::create(s.path(), " ").is_err());
    {
        let (mut j, mut c, _) = library::open(&entry).unwrap();
        fill(&mut j, &mut c, 4);
    }
    let (_, c, _) = library::open(&s.path().join("Family Photos")).unwrap();
    assert_eq!(c.len(), 4);
    // not catalogs
    std::fs::write(s.path().join("x.txt"), b"{}").unwrap();
    assert!(library::open(&s.path().join("x.txt")).is_err());
    let fake = s.path().join(format!("fake.{}", dac_brand::CATALOG_EXT));
    std::fs::write(&fake, b"\xff\xfe garbage").unwrap();
    assert!(library::open(&fake).is_err());
    assert!(library::open(&s.path().join("missing")).is_err());

    let file = s.path().join("recent.json");
    let mut r = RecentCatalogs::load(&file);
    assert_eq!(r, RecentCatalogs::default());
    r.touch(&entry);
    r.touch(&fake);
    r.touch(&entry);
    assert_eq!(r.paths, vec![entry.clone(), fake.clone()]);
    r.save(&file).unwrap();
    let r = RecentCatalogs::load(&file);
    assert_eq!(r.startup(false), Some(entry.clone()));
    assert_eq!(r.startup(true), None, "Alt held: the chooser");
    std::fs::write(&file, b"not json").unwrap();
    assert_eq!(RecentCatalogs::load(&file), RecentCatalogs::default());
}

#[test]
fn backup_integrity_optimise() {
    let s = Scratch::new("bk");
    let (mut j, mut c, _) = open(s.path());
    fill(&mut j, &mut c, 25);
    let report = j.check_integrity(&c).unwrap();
    assert!(report.ok(), "{report:?}");
    let root = s.path().join("elsewhere");
    let mut made = Vec::new();
    for _ in 0..3 {
        made.push(j.backup(&c, &root, 2).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    let left: Vec<PathBuf> = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(left.len(), 2, "pruned to 2: {left:?}");
    // a backup opens as a catalog of its own
    {
        let copy = Scratch::new("bk-copy");
        for f in std::fs::read_dir(&made[2]).unwrap() {
            let f = f.unwrap().path();
            std::fs::copy(&f, copy.path().join(f.file_name().unwrap())).unwrap();
        }
        let (_, back, _) = open(copy.path());
        assert_eq!(back.to_snapshot(), c.to_snapshot());
    }
    // optimise drops settings nothing uses and keeps the catalog
    let ids: Vec<PhotoId> = c.photos().filter(|p| p.edited.is_some()).map(|p| p.id).collect();
    let del = c.delete_photos_permanently_ops(&ids);
    c.apply(del.clone()).unwrap();
    j.append(&[del]).unwrap();
    let o = j.optimize(&c).unwrap();
    assert!(o.bytes_after > 0);
    assert!(j.check_integrity(&c).unwrap().ok());
    drop(j);
    let (_, back, _) = open(s.path());
    assert_eq!(back.to_snapshot(), c.to_snapshot());

    // the schedule
    let mut st = CatalogSettings::load(s.path());
    st.backup = BackupSchedule::Weekly;
    st.last_backup = Some(1_000_000);
    assert!(st.backup_due(1_000_000 + 7 * 86_400) && !st.backup_due(1_000_000 + 86_400));
    st.backup = BackupSchedule::Never;
    assert!(!st.backup_due(i64::MAX));
    st.backup = BackupSchedule::EveryExit;
    assert!(st.backup_due(1_000_000));
}

/// The sort keys' integer prefixes order exactly like the strings (property).
#[test]
fn prefix_sort_matches_string_sort() {
    use proptest::prelude::*;
    let mut runner = proptest::test_runner::TestRunner::default();
    let date = proptest::option::of("20[0-9]{2}-[01][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-5][0-9](\\.[0-9]{1,3})?");
    runner
        .run(&proptest::collection::vec((date, "20[0-9]{2}-0[1-9]", any::<bool>()), 1..40), |rows| {
            let mut c = Catalog::new();
            for (cap, imp, _) in &rows {
                let id = c.alloc_photo_id();
                let mut p = Photo::new(id, Source::Demo { scene: 0 }, "a.jpg", "JPEG", 1, 1, imp);
                p.captured = cap.clone();
                c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            }
            for ascending in [true, false] {
                let sort = Sort { ascending, ..Default::default() };
                let got = c.query(&Filter::default(), &sort);
                let mut want: Vec<&Photo> = c.photos().map(|p| p.as_ref()).collect();
                want.sort_by(|a, b| {
                    let o = a.captured.cmp(&b.captured).then_with(|| a.imported.cmp(&b.imported)).then(a.id.cmp(&b.id));
                    if ascending { o } else { o.reverse() }
                });
                prop_assert_eq!(got, want.iter().map(|p| p.id).collect::<Vec<_>>());
            }
            Ok(())
        })
        .unwrap();
}

/// Sorting a library above the parallel threshold gives the single-threaded order, for every key.
#[test]
fn large_sorts_match_the_plain_sort() {
    let mut c = Catalog::new();
    for i in 0..25_000u64 {
        let id = c.alloc_photo_id();
        let mut p =
            Photo::new(id, Source::Demo { scene: 0 }, &format!("N{}.jpg", query::mix64(i) % 997), "JPEG", 1, 1, &format!("2026-0{}-01", 1 + i % 9));
        p.captured = (i % 7 != 0).then(|| format!("2020-01-{:02}T10:00:00 and a long tail {}", 1 + i % 28, query::mix64(i) % 5));
        p.rating = (i % 6) as u8;
        p.file_size = query::mix64(i) % 1000;
        p.edited = (i % 3 == 0).then(|| format!("2026-02-{:02}", 1 + i % 28));
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let all: Vec<&Photo> = c.photos().map(|p| p.as_ref()).collect();
    for key in [SortKey::CaptureDate, SortKey::ImportDate, SortKey::EditDate, SortKey::FileName, SortKey::Rating, SortKey::FileSize, SortKey::Random]
    {
        for ascending in [true, false] {
            let sort = Sort { key, ascending, seed: 7, ..Default::default() };
            let mut want = all.clone();
            want.sort_by(|a, b| {
                let o = match key {
                    SortKey::CaptureDate => a.captured.cmp(&b.captured).then_with(|| a.imported.cmp(&b.imported)),
                    SortKey::ImportDate => a.imported.cmp(&b.imported),
                    SortKey::EditDate => a.edited.cmp(&b.edited),
                    SortKey::FileName => a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()),
                    SortKey::Rating => a.rating.cmp(&b.rating),
                    SortKey::FileSize => a.file_size.cmp(&b.file_size),
                    SortKey::Random => query::mix64(query::mix64(7) ^ a.id.0).cmp(&query::mix64(query::mix64(7) ^ b.id.0)),
                }
                .then(a.id.cmp(&b.id));
                if ascending { o } else { o.reverse() }
            });
            assert_eq!(c.query(&Filter::default(), &sort), want.iter().map(|p| p.id).collect::<Vec<_>>(), "{key:?} {ascending}");
        }
    }
}

#[test]
fn export_and_import_catalogs() {
    use crate::transfer::{self, ConflictRule, ExportOptions};
    let s = Scratch::new("xfer");
    // originals on disk for two photos
    let files = s.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    let (mut j, mut c, _) = open(&s.path().join("main"));
    fill(&mut j, &mut c, 12);
    let ids: Vec<PhotoId> = c.photos().map(|p| p.id).collect();
    for id in &ids[..2] {
        let path = files.join(format!("f{}.jpg", id.0));
        std::fs::write(&path, b"jpeg bytes").unwrap();
        let op = Op::Relink {
            id: *id,
            file_name: format!("f{}.jpg", id.0),
            source: Source::File { path: path.to_string_lossy().into_owned() },
            format: None,
        };
        c.apply(op.clone()).unwrap();
        j.append(&[op]).unwrap();
    }
    let pick = vec![ids[0], ids[1], ids[3], ids[4], ids[5]];
    let rep =
        transfer::export_catalog(&c, &ExportOptions { photos: pick.clone(), include_originals: true, include_previews: true }, s.path(), "Trip")
            .unwrap();
    assert_eq!((rep.photos, rep.originals_copied, rep.stacks), (5, 2, 1));
    assert_eq!(rep.originals_failed.len(), 3, "the other three have no file: {:?}", rep.originals_failed);
    let sub = transfer::load_readonly(&rep.entry).unwrap();
    assert_eq!(sub.len(), 5);
    assert!(sub.photo(ids[0]).unwrap().sha1.is_some() && sub.remote_of(ids[1]).count() == 1 && sub.preview_entry(ids[1]).is_some());
    let Source::File { path } = &sub.photo(ids[0]).unwrap().source else { panic!() };
    assert!(Path::new(path).starts_with(s.path().join("Trip").join("Originals")) && Path::new(path).exists());
    assert_eq!(sub.albums().find(|a| a.name == "Best").unwrap().photos, vec![ids[0], ids[1]]);

    // edit the exported catalog: a rating, a develop change, a new photo
    {
        let (mut tj, mut tc, _) = library::open(&rep.entry).unwrap();
        let mut d = (*tc.photo(ids[3]).unwrap().develop).clone();
        d.light.exposure = 2.0;
        let id_new = tc.alloc_photo_id();
        let ops = vec![
            Op::SetRating { id: ids[4], rating: 1 },
            Op::SetDevelop { id: ids[3], settings: Arc::new(d), label: "Exposure".into(), edited: Some("2026-03-01T00:00:00".into()) },
            Op::AddPhoto { photo: Box::new(Photo::new(id_new, Source::File { path: "/pics/new/N.jpg".into() }, "N.jpg", "JPEG", 2, 2, "t")) },
            Op::SetAlbumPhotos { id: tc.albums().find(|a| a.name == "Best").unwrap().id, photos: vec![ids[0], ids[1], id_new] },
        ];
        for op in &ops {
            tc.apply(op.clone()).unwrap();
        }
        tj.append(&ops).unwrap();
    }
    // the source isn't modified by reading it
    let before = std::fs::read(Path::new(&rep.entry).parent().unwrap().join(LOG)).unwrap();
    let other = transfer::load_readonly(&rep.entry).unwrap();
    assert_eq!(std::fs::read(Path::new(&rep.entry).parent().unwrap().join(LOG)).unwrap(), before);

    // the moved originals don't match the library's files: match the other three only
    let plan = transfer::plan_import(&c, &other);
    assert_eq!(plan.new_photos.len(), 3, "two relocated originals and the new photo: {plan:?}");
    let changed: Vec<(PhotoId, bool, bool)> = plan.changed.iter().map(|x| (x.target, x.settings_differ, x.metadata_differ)).collect();
    assert_eq!(changed, vec![(ids[3], true, false), (ids[4], false, true)]);
    assert_eq!(plan.unchanged, 1);
    assert_eq!(plan.extended_albums, vec!["Best".to_string()]);

    // keep: only the new photos come in
    let mut keep = c.clone();
    let op = plan.ops(&mut keep, &other, ConflictRule::Keep);
    keep.apply(op).unwrap();
    assert_eq!(keep.len(), c.len() + 3);
    assert_eq!(keep.photo(ids[4]).unwrap().rating, c.photo(ids[4]).unwrap().rating);
    // replace settings and metadata, then undo it all in one step
    let mut both = c.clone();
    let op = plan.ops(&mut both, &other, ConflictRule::ReplaceSettingsAndMetadata);
    let undo = both.apply(op).unwrap();
    assert_eq!(both.photo(ids[4]).unwrap().rating, 1);
    assert_eq!(both.photo(ids[3]).unwrap().develop.light.exposure, 2.0);
    let best = both.albums().find(|a| a.name == "Best").unwrap();
    assert_eq!(best.photos.len(), c.albums().find(|a| a.name == "Best").unwrap().photos.len() + 3, "the three new photos");
    both.apply(undo).unwrap();
    assert_eq!(both.photos().map(|p| p.as_ref().clone()).collect::<Vec<_>>(), c.photos().map(|p| p.as_ref().clone()).collect::<Vec<_>>());
}
