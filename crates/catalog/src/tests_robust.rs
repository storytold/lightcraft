//! P6.2 never-crash harnesses for everything the catalog reads from disk: the JSON snapshot (every
//! format version, v1 → current upgrade paths), the log (ops that pass their CRC but carry hostile
//! values), the v4 store (truncated / garbled `catalog.redb`, with the backup named in the error),
//! catalogs opened for import (`transfer::load_readonly`), keyword lists and the small settings
//! files, plus full-disk saves. Seeded mutation loops from `dac-fuzzkit`: plain `cargo test`.

use std::path::Path;

use crate::db::DB_FILE;
use crate::journal::{LOG, SNAPSHOT, VERSION, decode_record, encode_record};
use crate::library::{self, CatalogSettings, RecentCatalogs};
use crate::tests_v4::{Scratch, fill};
use crate::*;

/// A populated in-memory library: its catalog and the ops that built it.
fn sample() -> (Catalog, Vec<Op>) {
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    fill(&mut j, &mut c, 8);
    let log = String::from_utf8(m.get(LOG).unwrap()).unwrap();
    let ops = log.lines().filter_map(decode_record).map(|(_, op)| op).collect();
    (c, ops)
}

fn snap_file(version: u32, seq: u64, c: &Catalog) -> String {
    format!("{{\"format\":\"dac-catalog\",\"version\":{version},\"seq\":{seq},\"catalog\":{}}}\n", c.to_snapshot())
}

/// Open from these bytes; when it opens, use it (append, snapshot, reopen) like a session would.
fn open_and_use(snap: Option<&[u8]>, log: Option<&[u8]>) {
    let m = MemStore::new();
    if let Some(s) = snap {
        m.set(SNAPSHOT, s.to_vec());
    }
    if let Some(l) = log {
        m.set(LOG, l.to_vec());
    }
    let Ok((mut j, mut c, _)) = Journal::open(Box::new(m.clone())) else { return };
    let _ = c.query(&Filter::default(), &Sort::default());
    let _ = c.keyword_tree();
    let id = c.alloc_photo_id();
    let op = Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: "/x.jpg".into() }, "x.jpg", "JPEG", 2, 2, "2026-01-01")) };
    if c.apply(op.clone()).is_ok() {
        let _ = j.append(&[op]);
    }
    let _ = j.snapshot(&c);
    drop(j);
    let _ = Journal::open(Box::new(m));
}

#[test]
fn snapshots_of_every_format_never_panic() {
    let (c, _) = sample();
    let seeds: Vec<String> = (1..=VERSION).map(|v| snap_file(v, 3, &c)).collect();
    let seed_refs: Vec<&str> = seeds.iter().map(String::as_str).collect();
    dac_fuzzkit::run_json("catalog.snapshot", &seed_refs, 400, |s| open_and_use(Some(s.as_bytes()), None));
}

#[test]
fn log_records_with_valid_crcs_never_panic() {
    let (_, ops) = sample();
    let bodies: Vec<String> = ops.iter().map(|op| serde_json::to_string(op).unwrap()).collect();
    let seeds: Vec<&str> = bodies.iter().map(String::as_str).collect();
    let (base, _) = sample();
    // mutate the op, then sign it: the CRC only proves the line is what was written, not that
    // the op makes sense (a newer build, a hand edit, a bug elsewhere)
    dac_fuzzkit::run_json("catalog.log", &seeds, 1500, |body| {
        let crc = crc32fast::hash(body.as_bytes());
        let line = format!("{{\"seq\":9,\"crc\":{crc},\"op\":{body}}}");
        if let Some((_, op)) = decode_record(&line) {
            let mut c = base.clone();
            if c.apply(op.clone()).is_ok() {
                let _ = c.query(&Filter::default(), &Sort::default());
                let _ = c.to_snapshot();
            }
            // replayed on top of an empty library too
            let _ = Catalog::new().apply(op);
        }
    });
}

#[test]
fn damaged_logs_never_panic() {
    let (c, ops) = sample();
    let log: String = ops.iter().enumerate().map(|(i, op)| encode_record(i as u64 + 1, op) + "\n").collect();
    let snap = snap_file(VERSION, 2, &c);
    dac_fuzzkit::run("catalog.logfile", &[log.as_bytes()], 300, |l| {
        open_and_use(None, Some(l));
        open_and_use(Some(snap.as_bytes()), Some(l));
    });
}

/// A v4 library on disk with a few photos, closed.
fn v4_library(tag: &str) -> Scratch {
    let s = Scratch::new(tag);
    let (mut j, mut c, _) = Journal::open(Box::new(FsStore::open(s.path()).unwrap())).unwrap();
    fill(&mut j, &mut c, 12);
    j.snapshot(&c).unwrap();
    s
}

fn open_dir(dir: &Path) -> Result<(Journal, Catalog, LoadReport)> {
    Journal::open(Box::new(FsStore::open(dir).map_err(|e| CatalogError::Io(e.to_string()))?))
}

#[test]
fn garbled_store_and_log_never_panic() {
    let s = v4_library("robust-db");
    let db = s.path().join(DB_FILE);
    let good = std::fs::read(&db).unwrap();
    let log = s.path().join(LOG);
    let good_log = std::fs::read(&log).unwrap_or_default();
    // each open is a real redb open: a bounded number of mutants (raise with DAC_FUZZ_ITERS)
    let mut rng = dac_fuzzkit::Rng::new(0x5eed);
    for _ in 0..dac_fuzzkit::iterations(60) {
        let mut b = good.clone();
        match rng.below(3) {
            0 => b.truncate(rng.below(b.len() + 1)),
            1 => {
                for _ in 0..1 + rng.below(64) {
                    let at = rng.below(b.len());
                    b[at] ^= 1 << rng.below(8);
                }
            }
            _ => {
                let at = rng.below(b.len());
                let n = rng.below(4096).min(b.len() - at);
                for x in &mut b[at..at + n] {
                    *x = rng.byte();
                }
            }
        }
        std::fs::write(&db, &b).unwrap();
        std::fs::write(&log, dac_fuzzkit::mutate(&mut rng, &good_log, 4)).unwrap();
        if let Ok((mut j, c, _)) = open_dir(s.path()) {
            let _ = j.check_integrity(&c);
        }
        let _ = transfer::load_readonly(s.path());
    }
}

#[test]
fn corrupt_store_error_names_the_backup() {
    let s = v4_library("robust-backup");
    // no backup yet: the error says where backups would be
    let db = s.path().join(DB_FILE);
    let good = std::fs::read(&db).unwrap();
    std::fs::write(&db, &good[..good.len() / 3]).unwrap();
    let Err(e) = open_dir(s.path()) else { panic!("a truncated store opened") };
    let msg = e.to_string();
    assert!(matches!(e, CatalogError::Corrupt(_)), "{msg}");
    assert!(msg.contains("No backup of this catalog was found in") && msg.contains(library::BACKUPS_DIR), "{msg}");
    // with a backup: the error names it
    std::fs::write(&db, &good).unwrap();
    let (mut j, c, _) = open_dir(s.path()).unwrap();
    let backup = j.backup(&c, &s.path().join(library::BACKUPS_DIR), 3).unwrap();
    drop(j);
    std::fs::write(&db, b"garbage garbage garbage garbage garbage garbage garbage garbage").unwrap();
    let msg = open_dir(s.path()).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(msg.contains(&backup.display().to_string()), "{msg}");
    assert_eq!(library::latest_backup(s.path()), Some(backup.clone()));
    // and the backup really restores the library
    std::fs::copy(backup.join(DB_FILE), &db).unwrap();
    let (_, c2, _) = open_dir(s.path()).unwrap();
    assert_eq!(c2.len(), c.len());
}

#[test]
fn catalogs_opened_for_import_never_panic() {
    // a v3 (JSON) library handed to File → Import from Another Catalog
    let (c, ops) = sample();
    let snap = snap_file(VERSION, 2, &c);
    let log: String = ops.iter().enumerate().map(|(i, op)| encode_record(i as u64 + 1, op) + "\n").collect();
    let s = Scratch::new("robust-import");
    dac_fuzzkit::run_json("catalog.import", &[&snap], 150, |t| {
        std::fs::write(s.path().join(SNAPSHOT), t).unwrap();
        std::fs::write(s.path().join(LOG), &log).unwrap();
        if let Ok(other) = transfer::load_readonly(s.path()) {
            let plan = transfer::plan_import(&c, &other);
            let mut mine = c.clone();
            for rule in [transfer::ConflictRule::Keep, transfer::ConflictRule::ReplaceSettingsAndMetadata] {
                let op = plan.ops(&mut mine.clone(), &other, rule);
                let _ = mine.apply(op);
            }
        }
    });
}

#[test]
fn settings_files_and_keyword_lists_never_panic() {
    let s = Scratch::new("robust-settings");
    let settings = serde_json::to_string(&CatalogSettings { keep_backups: 5, ..CatalogSettings::default() }).unwrap();
    dac_fuzzkit::run_json("catalog.settings", &[&settings], 500, |t| {
        std::fs::write(s.path().join(library::SETTINGS_FILE), t).unwrap();
        let cs = CatalogSettings::load(s.path());
        let _ = cs.backup_due(i64::MAX);
        let _ = cs.backup_due(i64::MIN);
        let _ = library::backup_hint(s.path());
    });
    let recent = r#"{"paths":["/a/b","/c"],"default":"/a/b","promptAtStartup":true}"#;
    let file = s.path().join("recent.json");
    dac_fuzzkit::run_json("catalog.recent", &[recent], 500, |t| {
        std::fs::write(&file, t).unwrap();
        let _ = RecentCatalogs::load(&file);
    });
    let kw = "People\n\tAnna\n\t\t{Annie}\n[Places]\n\tParis\n~Hidden\n";
    dac_fuzzkit::run_str("catalog.keywords", &[kw], 3000, |t| {
        if let Ok(list) = keywords::parse_keyword_list(t) {
            let mut c = Catalog::new();
            for (path, info) in list {
                let _ = c.apply(Op::SetKeyword { path, info: Some(info) });
            }
            let _ = c.keyword_tree();
        }
    });
}

/// A store on a disk with `room` bytes left: writes past it fail with "no space left" after
/// writing what fits (as `write(2)` does).
#[derive(Clone)]
struct FullDisk {
    files: MemStore,
    room: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl FullDisk {
    fn take(&self, n: usize) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;
        let left = self.room.load(Ordering::SeqCst);
        if (n as u64) > left {
            self.room.store(0, Ordering::SeqCst);
            return Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "No space left on device"));
        }
        self.room.store(left - n as u64, Ordering::SeqCst);
        Ok(())
    }
}

impl Store for FullDisk {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        // temp file + rename: on failure the old file stays
        self.take(data.len())?;
        self.files.write_atomic(name, data)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;
        let left = usize::try_from(self.room.load(Ordering::SeqCst)).unwrap_or(usize::MAX);
        if data.len() > left {
            self.files.append(name, &data[..left])?;
            self.room.store(0, Ordering::SeqCst);
            return Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "No space left on device"));
        }
        self.take(data.len())?;
        self.files.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.files.truncate(name, len)
    }
    fn describe(&self) -> String {
        "full disk".into()
    }
}

fn add_ops(c: &mut Catalog, n: usize) -> Vec<Op> {
    (0..n)
        .map(|i| {
            let id = c.alloc_photo_id();
            let op = Op::AddPhoto {
                photo: Box::new(Photo::new(id, Source::File { path: format!("/full/{i}.jpg") }, "f.jpg", "JPEG", 2, 2, "2026-01-01")),
            };
            c.apply(op.clone()).unwrap();
            op
        })
        .collect()
}

#[test]
fn full_disk_during_catalog_saves_keeps_the_library() {
    for room in [0u64, 50, 400, 3000, 20_000] {
        let files = MemStore::new();
        let disk = FullDisk { files: files.clone(), room: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX)) };
        let (mut j, mut c, _) = Journal::open(Box::new(disk.clone())).unwrap();
        let first = add_ops(&mut c, 5);
        j.append(&first).unwrap();
        j.snapshot(&c).unwrap();
        // the disk fills up
        disk.room.store(room, std::sync::atomic::Ordering::SeqCst);
        let mut saved = 5;
        for _ in 0..20 {
            let ops = add_ops(&mut c, 3);
            match j.append(&ops) {
                Ok(()) => saved += 3,
                Err(e) => {
                    assert!(matches!(e, CatalogError::Io(_)), "{e}");
                    break;
                }
            }
        }
        let snap = j.snapshot(&c);
        drop(j);
        // reopened (with space again): every append that succeeded is there, nothing is damaged
        let (_, c2, r) = Journal::open(Box::new(files)).unwrap();
        assert!(r.damaged.is_none(), "room {room}: {r:?}");
        if snap.is_ok() {
            assert_eq!(c2.len(), c.len(), "room {room}");
        } else {
            assert_eq!(c2.len(), saved, "room {room}");
        }
    }
}

#[test]
fn full_disk_during_settings_backup_and_catalog_export() {
    let s = v4_library("robust-full");
    let (mut j, c, _) = open_dir(s.path()).unwrap();
    {
        let _full = safe_file::fail_writes_after(10);
        assert!(CatalogSettings::default().save(s.path()).is_err());
    }
    // the settings file that was there is intact
    let _ = CatalogSettings::load(s.path());
    let opts = transfer::ExportOptions { photos: c.photos().map(|p| p.id).collect(), ..Default::default() };
    let out = Scratch::new("robust-full-export");
    let r = {
        let _full = safe_file::fail_writes_after(0);
        transfer::export_catalog(&c, &opts, out.path(), "Exported")
    };
    if let Ok(rep) = r {
        // whatever reported success must open
        assert!(transfer::load_readonly(&rep.entry).is_ok());
    }
    drop(j.backup(&c, &s.path().join("bk"), 1));
}
