//! Copy first, then upgrade (`crate::upgrade_backup`): an upgrade keeps a verified copy of the
//! library as it was, and a full disk, a crash or a damaged catalog mid-upgrade leaves the library
//! as the older version left it, with an error that says what to do.

use std::path::{Path, PathBuf};

use crate::db::DB_FILE;
use crate::journal::{LOG, SNAPSHOT, VERSION};
use crate::tests_v4::{Scratch, older_store, photo, stored_meta};
use crate::upgrade_backup::{FAULT, Fault, README};
use crate::*;

fn open(dir: &Path) -> Result<(Journal, Catalog, LoadReport)> {
    Journal::open(Box::new(FsStore::open(dir).unwrap()))
}

fn with_fault<T>(f: Fault, run: impl FnOnce() -> T) -> T {
    FAULT.with(|c| c.set(Some(f)));
    let out = run();
    FAULT.with(|c| c.set(None));
    out
}

/// The folders in `backups/` whose name starts with `prefix`.
fn kept(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir.join("backups")) else { return Vec::new() };
    rd.map(|e| e.unwrap().path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(prefix)).collect()
}

/// A v3 library (JSON snapshot) of 10 photos; returns its snapshot text.
fn v3_library(s: &Scratch) -> String {
    let mut c = Catalog::new();
    for i in 0..10 {
        let p = photo(&mut c, i);
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let snap = format!("{{\"format\":\"lcc-catalog\",\"version\":3,\"seq\":10,\"catalog\":{}}}\n", c.to_snapshot());
    std::fs::write(s.path().join(SNAPSHOT), &snap).unwrap();
    snap
}

#[test]
fn an_upgrade_keeps_a_verified_copy_the_older_version_opens() {
    let s = older_store("keep-copy", Some("11"));
    let log_before = std::fs::read(s.path().join(LOG)).unwrap_or_default();
    let (j, c, r) = open(s.path()).unwrap();
    drop(j);
    assert_eq!(r.upgraded_from, Some(11));
    let copy = r.upgrade_backup.clone().expect("the open names the copy it kept");
    assert_eq!(kept(s.path(), "before-v"), vec![copy.clone()]);
    assert_eq!(stored_meta(&copy, "version").as_deref(), Some("11"), "the copy is still in the older format");
    assert_eq!(stored_meta(s.path(), "version"), Some(VERSION.to_string()), "the library is upgraded");
    assert_eq!(std::fs::read(copy.join(LOG)).unwrap_or_default(), log_before);
    assert!(std::fs::read_to_string(copy.join(README)).unwrap().contains("To go back"));
    assert_eq!(library::latest_backup(s.path()), Some(copy.clone()), "offered as the backup to restore");
    // the copy holds the library's photos: opened (as the older build would), it has them all
    let (_, from_copy, _) = open(&copy).unwrap();
    assert_eq!(from_copy.len(), c.len());
    // a copy kept before an upgrade is never pruned with the scheduled backups
    assert!(!kept(s.path(), "before-v").is_empty());
}

#[test]
fn a_full_disk_while_copying_leaves_the_older_store_and_says_what_to_do() {
    let s = older_store("keep-full", Some("11"));
    let e = with_fault(Fault::DiskFull { after: 4096 }, || open(s.path())).err().expect("refused");
    let msg = e.to_string();
    assert!(matches!(e, CatalogError::Io(_)), "{e:?}");
    assert!(msg.contains("left unchanged") && msg.contains("free some disk space"), "{msg}");
    assert_eq!(stored_meta(s.path(), "version").as_deref(), Some("11"), "not upgraded");
    assert!(kept(s.path(), "before-v").is_empty() && kept(s.path(), ".before-v").is_empty(), "no partial copy left");
    // with room again, the same open upgrades it
    let (_, _, r) = open(s.path()).unwrap();
    assert_eq!(r.upgraded_from, Some(11));
}

#[test]
fn a_crash_while_copying_leaves_the_older_store_and_the_next_open_finishes() {
    let s = older_store("keep-crash", Some("11"));
    assert!(with_fault(Fault::Crash, || open(s.path())).is_err());
    assert_eq!(stored_meta(s.path(), "version").as_deref(), Some("11"), "the library is as the older build left it");
    assert_eq!(kept(s.path(), ".before-v").len(), 1, "the crash left its partial copy");
    assert!(kept(s.path(), "before-v").is_empty(), "never taken for a finished copy");
    let (_, _, r) = open(s.path()).unwrap();
    assert_eq!(r.upgraded_from, Some(11));
    assert!(kept(s.path(), ".before-v").is_empty(), "the partial copy is cleared");
    assert_eq!(kept(s.path(), "before-v").len(), 1);
}

#[test]
fn a_v3_migration_copies_first_and_a_full_disk_leaves_the_v3_files() {
    let s = Scratch::new("keep-v3-full");
    let snap = v3_library(&s);
    let e = with_fault(Fault::DiskFull { after: 100 }, || open(s.path())).err().expect("refused");
    assert!(e.to_string().contains("left unchanged"), "{e}");
    assert_eq!(std::fs::read_to_string(s.path().join(SNAPSHOT)).unwrap(), snap, "the v3 snapshot is untouched");
    assert!(!s.path().join(DB_FILE).exists(), "no store was written");
    assert!(library::needs_migration(s.path()), "still a v3 library");
    let (_, c, r) = open(s.path()).unwrap();
    assert_eq!((r.upgraded_from, c.len()), (Some(3), 10));
    let copy = r.upgrade_backup.expect("a copy");
    assert_eq!(std::fs::read_to_string(copy.join(SNAPSHOT)).unwrap(), snap);
}

#[test]
fn a_damaged_older_catalog_is_refused_before_anything_is_written() {
    // a store that isn't one
    let s = older_store("keep-corrupt", Some("11"));
    std::fs::write(s.path().join(DB_FILE), b"not a redb file at all").unwrap();
    let e = open(s.path()).err().expect("refused");
    assert!(matches!(e, CatalogError::Corrupt(_) | CatalogError::Io(_)), "{e:?}");
    assert_eq!(std::fs::read(s.path().join(DB_FILE)).unwrap(), b"not a redb file at all");
    assert!(kept(s.path(), "before-v").is_empty() && kept(s.path(), ".before-v").is_empty());
    // a v3 snapshot cut short: the copy is kept, the library stays v3 and opens nothing broken
    let s = Scratch::new("keep-v3-corrupt");
    let snap = v3_library(&s);
    let cut = &snap[..snap.len() / 2];
    std::fs::write(s.path().join(SNAPSHOT), cut).unwrap();
    let r = open(s.path());
    assert_eq!(std::fs::read_to_string(s.path().join(SNAPSHOT)).unwrap(), cut, "the snapshot is left as it was");
    if r.is_err() {
        assert!(!s.path().join(DB_FILE).exists(), "a refused migration writes no store");
    }
}

/// A store an update left in an older format is brought to the current one without opening the
/// library ([`library::prepare_upgrade`]) — what the app runs on a worker thread once its window
/// is up, so the open never blocks the first frame — keeping the verified copy and marking the
/// version as the open itself would, with the copy's bytes as progress.
#[test]
fn an_older_store_is_bumped_with_progress_before_the_open() {
    let s = older_store("prepare-bump", Some("17"));
    // the entry file padded (trailing spaces keep it the JSON it was; nothing in this flow reads
    // it), so the byte-verified copy spans several chunks and its progress is surely seen from
    // another thread
    if let Some(entry) = library::find_entry(s.path()) {
        let mut body = std::fs::read_to_string(&entry).unwrap_or_default();
        body.push_str(&" ".repeat(8 << 20));
        std::fs::write(&entry, body).unwrap();
    }
    // progress is visible from another thread while it runs (other tests may upgrade too: only
    // this library's reports count), and never goes backwards
    let ours = s.path().display().to_string();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let (stop, ours) = (stop.clone(), ours.clone());
        std::thread::spawn(move || {
            let (mut seen, mut furthest) = (Vec::new(), -1.0f32);
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(p) = crate::progress::migration().filter(|p| p.library == ours) {
                    assert!((0.0..=1.0).contains(&p.fraction()), "{p:?}");
                    assert!(p.fraction() >= furthest, "progress went backwards: {p:?}");
                    furthest = p.fraction();
                    assert!(!p.describe().is_empty());
                    if seen.last() != Some(&p.phase) {
                        seen.push(p.phase);
                    }
                }
                std::thread::yield_now();
            }
            seen
        })
    };
    assert!(library::prepare_upgrade(s.path()).unwrap());
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let seen = watcher.join().unwrap();
    assert_eq!(stored_meta(s.path(), "version"), Some(VERSION.to_string()), "the store is upgraded");
    assert_eq!(kept(s.path(), "before-v").len(), 1, "the older build's files are kept");
    assert_eq!(seen.first(), Some(&crate::progress::MigrationPhase::Backup), "no progress reported: {seen:?}");
    assert!(crate::progress::migration().is_none_or(|p| p.library != ours), "progress left behind");
    // again: nothing to do, and the open that follows is a plain one in the current format
    assert!(!library::prepare_upgrade(s.path()).unwrap());
    assert_eq!(kept(s.path(), "before-v").len(), 1, "no second copy");
    let (j, c, r) = open(s.path()).unwrap();
    drop(j);
    assert_eq!((r.upgraded_from, c.len()), (None, 3));
}

/// A store that isn't one is an error naming the backups, not a panic, and nothing is written;
/// the progress it might have started is cleared.
#[test]
fn a_damaged_store_is_refused_by_the_prepare_step() {
    let s = older_store("prepare-damaged", Some("17"));
    let ours = s.path().display().to_string();
    std::fs::write(s.path().join(DB_FILE), b"not a redb file at all").unwrap();
    let e = library::prepare_upgrade(s.path()).err().expect("refused");
    assert!(matches!(e, CatalogError::Corrupt(_) | CatalogError::Io(_)), "{e:?}");
    assert_eq!(std::fs::read(s.path().join(DB_FILE)).unwrap(), b"not a redb file at all");
    assert!(kept(s.path(), "before-v").is_empty() && kept(s.path(), ".before-v").is_empty(), "nothing written");
    assert!(crate::progress::migration().is_none_or(|p| p.library != ours), "progress left behind");
}
