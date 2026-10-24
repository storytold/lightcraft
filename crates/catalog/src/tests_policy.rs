//! When the log is compacted into a snapshot ([`SnapshotPolicy`]): once it has grown to a share
//! of the snapshot's size, so the cost of saving follows what changed, not the library's size.

use crate::journal::SNAPSHOT;
use crate::*;

fn add(c: &mut Catalog) -> Op {
    let id = c.alloc_photo_id();
    let op = Op::AddPhoto {
        photo: Box::new(Photo::new(
            id,
            Source::File { path: format!("/pictures/2019/trip/IMG_{:06}.jpg", id.0) },
            "IMG.jpg",
            "JPEG",
            4032,
            3024,
            "2026-10-10T12:00:00",
        )),
    };
    c.apply(op.clone()).unwrap();
    op
}

fn rate(c: &mut Catalog, k: u64) -> Op {
    // (whatever ids the catalog gave its photos)
    let id = c.photos().nth((k % 50) as usize).map(|p| p.id).unwrap();
    let op = Op::SetRating { id, rating: (k % 6) as u8 };
    c.apply(op.clone()).unwrap();
    op
}

/// A library of `n` photos, saved and compacted.
fn library(n: u64) -> (MemStore, Journal, Catalog) {
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    let ops: Vec<Op> = (0..n).map(|_| add(&mut c)).collect();
    j.append(&ops).unwrap();
    j.snapshot(&c).unwrap();
    (m, j, c)
}

/// Given a small library, when 2000 changes were logged, then the log is compacted, as it always was.
#[test]
fn a_small_library_compacts_after_2000_changes() {
    let (_m, mut j, mut c) = library(50);
    for k in 0..1999 {
        let op = rate(&mut c, k);
        j.append(&[op]).unwrap();
        assert!(!j.wants_snapshot(), "after {} changes", k + 1);
    }
    let op = rate(&mut c, 1999);
    j.append(&[op]).unwrap();
    assert!(j.wants_snapshot());
}

/// Given a large library, when 2000 small changes were logged, then the snapshot is not rewritten
/// for them; it is once the log has grown to a quarter of the snapshot.
#[test]
fn a_large_library_compacts_when_the_log_is_a_quarter_of_the_snapshot() {
    let (_m, mut j, mut c) = library(20_000);
    let snapshot = j.snapshot_bytes();
    assert!(snapshot > 2_000_000, "{snapshot}");
    let mut k = 0;
    while j.log_bytes() < snapshot / 4 {
        assert!(!j.wants_snapshot(), "{} records, {} of {snapshot} bytes", j.log_records(), j.log_bytes());
        let ops: Vec<Op> = (0..500).map(|i| rate(&mut c, k + i)).collect();
        j.append(&ops).unwrap();
        k += 500;
    }
    assert!(j.log_records() > 4000, "{}", j.log_records());
    assert!(j.wants_snapshot());
    j.snapshot(&c).unwrap();
    assert!(!j.wants_snapshot());
}

/// The size the rule goes by is the snapshot's, also right after the library was opened.
#[test]
fn the_snapshot_size_is_known_after_reopening() {
    let (m, j, _c) = library(2000);
    let written = j.snapshot_bytes();
    assert_eq!(written, m.get(SNAPSHOT).unwrap().len() as u64);
    drop(j);
    let (j, _, _) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(j.snapshot_bytes(), written);
    // a new library's first snapshot counts too
    let (j, _, _) = Journal::open(Box::new(MemStore::new())).unwrap();
    assert!(j.snapshot_bytes() > 0);
}

/// However large the snapshot, a log is never left to grow past the cap: opening the library
/// replays all of it.
#[test]
fn a_log_at_the_cap_is_compacted_whatever_the_snapshot_size() {
    let p = SnapshotPolicy::default();
    let huge = 100 << 30;
    assert!(!p.due(1_000_000, p.log_cap_bytes - 1, huge));
    assert!(p.due(1_000_000, p.log_cap_bytes, huge));
    // and a short log is left alone whatever its share of a tiny snapshot
    assert!(!p.due(10, 4096, 100));
    // without a share, length alone decides (the old rule)
    let q = SnapshotPolicy { snapshot_share: 0, ..p };
    assert!(q.due(2000, 1, huge) && !q.due(1999, 1, huge));
}

/// Given an import of many photos, when the log is compacted whenever it is due, then all the
/// snapshots written together are a few times the final one, not one per 2000 photos.
#[test]
fn an_import_rewrites_the_snapshot_a_bounded_number_of_times() {
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    let (mut compactions, mut written) = (0u64, 0u64);
    for _ in 0..80 {
        let ops: Vec<Op> = (0..500).map(|_| add(&mut c)).collect();
        j.append(&ops).unwrap();
        if j.wants_snapshot() {
            j.snapshot(&c).unwrap();
            compactions += 1;
            written += j.snapshot_bytes();
        }
    }
    j.snapshot(&c).unwrap();
    let last = j.snapshot_bytes();
    // 40,000 photos: every 2000 records would be 20 compactions writing ~10x the final snapshot
    assert!(compactions <= 12, "{compactions} compactions");
    assert!(written <= 5 * last, "{written} bytes of snapshots for a final one of {last}");
    drop(j);
    let (_, loaded, _) = Journal::open(Box::new(m)).unwrap();
    assert_eq!(loaded.len(), 40_000);
}

/// Given a small library with changes, when it is closed, then its log is compacted (the next
/// open replays nothing); with no changes there is nothing to write.
#[test]
fn closing_a_small_library_compacts_its_log() {
    let (_m, mut j, mut c) = library(50);
    assert!(!j.wants_snapshot_on_close(), "nothing changed");
    let op = rate(&mut c, 1);
    j.append(&[op]).unwrap();
    assert!(j.wants_snapshot_on_close());
}

/// Given a library whose snapshot is large, when it is closed after a few changes, then the
/// snapshot is not rewritten for them. It is once the log is long (2000 changes), though that
/// is not yet its share of the snapshot: after a quit a log isn't left long, so a damaged record
/// in it never costs more changes than it used to.
#[test]
fn closing_a_large_library_keeps_a_short_log_and_compacts_a_long_one() {
    let (_m, mut j, mut c) = library(20_000);
    j.policy.close_bytes = j.snapshot_bytes() - 1;
    for k in 0..1999 {
        let op = rate(&mut c, k);
        j.append(&[op]).unwrap();
    }
    assert!(!j.wants_snapshot_on_close(), "{} changes", j.log_records());
    let op = rate(&mut c, 1999);
    j.append(&[op]).unwrap();
    assert!(j.wants_snapshot_on_close());
    assert!(!j.wants_snapshot(), "during the session the log may still grow to its share of the snapshot");
}

/// A store whose writes fail while `full` is set.
struct Fillable {
    files: MemStore,
    full: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Store for Fillable {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if self.full.load(std::sync::atomic::Ordering::SeqCst) {
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
    fn describe(&self) -> String {
        "fillable".into()
    }
}

/// Given a library in an older format whose upgrade could not be written when it was opened (the
/// disk was full), when it is closed once there is room, then the snapshot is written in this
/// build's format whatever the log and the snapshot's size: closing must not leave an older
/// snapshot with this build's records after it.
#[test]
fn an_upgrade_that_could_not_be_written_is_written_on_close() {
    let m = MemStore::new();
    let older = format!("{{\"format\":\"lightcraft-catalog\",\"version\":8,\"seq\":0,\"catalog\":{}}}\n", Catalog::new().to_snapshot());
    m.set(SNAPSHOT, older.into_bytes());
    let full = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (mut j, c, _) = Journal::open(Box::new(Fillable { files: m.clone(), full: full.clone() })).unwrap();
    assert_eq!(head_version(&m), 8, "the upgrade could not be written");
    // large, with nothing in the log: closing would write nothing but for the upgrade
    j.policy.close_bytes = 0;
    assert!(j.wants_snapshot_on_close());
    full.store(false, std::sync::atomic::Ordering::SeqCst);
    j.snapshot(&c).unwrap();
    assert!(!j.wants_snapshot_on_close(), "written: nothing left to do");
    assert_eq!(head_version(&m), crate::journal::VERSION);
}

fn head_version(m: &MemStore) -> u32 {
    let head: serde_json::Value = serde_json::from_slice(&m.get(SNAPSHOT).unwrap()).unwrap();
    head["version"].as_u64().unwrap() as u32
}
