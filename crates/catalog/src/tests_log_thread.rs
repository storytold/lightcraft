//! PERF-STEP-STALL: the catalog log's own thread ([`crate::log_writer`]). Appends return at once
//! however slow the drive is; everything handed over is written in order; a write that fails is
//! held, reported and retried, never dropped, and the log never has a gap.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::fault::{self, Step};
use crate::store::{FsStore, Store};

const LOG: &str = "catalog.log";

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-log-thread-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let t = Instant::now();
    while !done() {
        assert!(t.elapsed() < Duration::from_secs(20), "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Appends are written by the thread in order; a read through the store sees every one; a cut
/// and a replace come after the appends before them; dropping the store writes what is left.
#[test]
fn appends_are_written_in_order_and_cuts_and_replaces_come_after_them() {
    let dir = temp_dir("order");
    let mut store = FsStore::open(&dir).unwrap();
    let mut want = Vec::new();
    for i in 0..200u32 {
        let line = format!("{i}\n");
        store.append(LOG, line.as_bytes()).unwrap();
        want.extend_from_slice(line.as_bytes());
        if i % 50 == 0 {
            assert_eq!(store.read(LOG).unwrap().unwrap(), want, "all of it, through the store");
        }
    }
    store.flush().unwrap();
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), want, "all of it, in the file");
    store.truncate(LOG, 4).unwrap();
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"0\n1\n");
    // never longer than it is
    store.truncate(LOG, 400).unwrap();
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"0\n1\n");
    store.append(LOG, b"x\n").unwrap();
    store.write_atomic(LOG, b"tail\n").unwrap();
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"tail\n");
    store.append(LOG, b"more\n").unwrap();
    drop(store);
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"tail\nmore\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// (a) Each sync takes 300 ms: 100 appends still return at once, and a flush then finds all of
/// them in the file (before, each append waited for its own sync: 30 s for these).
#[test]
fn an_append_never_waits_for_a_slow_sync() {
    let dir = temp_dir("slow");
    let mut store = FsStore::open(&dir).unwrap();
    store.append(LOG, b"first\n").unwrap();
    let syncs = Arc::new(AtomicUsize::new(0));
    let n = syncs.clone();
    fault::install(
        &dir,
        Arc::new(move |step| {
            if step == Step::Sync {
                n.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(300));
            }
            Ok(())
        }),
    );
    let mut want = b"first\n".to_vec();
    let mut slowest = Duration::ZERO;
    for i in 0..100u32 {
        let line = format!("{i}\n");
        let t = Instant::now();
        store.append(LOG, line.as_bytes()).unwrap();
        slowest = slowest.max(t.elapsed());
        want.extend_from_slice(line.as_bytes());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(slowest < Duration::from_millis(50), "an append waited {slowest:?}");
    store.flush().unwrap();
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), want);
    // batched: far fewer syncs than appends
    assert!(syncs.load(Ordering::SeqCst) < 50, "{} syncs", syncs.load(Ordering::SeqCst));
    fault::remove(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// (c) Writes fail for a while: what was handed over is held (reported, with the reason), later
/// appends are refused so the caller keeps them, and once writes work again the held bytes land,
/// in order, with no fragment of the failed attempts and no gap.
#[test]
fn a_failed_write_is_held_reported_and_retried() {
    let dir = temp_dir("fail");
    let mut store = FsStore::open(&dir).unwrap();
    store.append(LOG, b"0\n").unwrap();
    store.flush().unwrap();
    let fail = Arc::new(AtomicBool::new(true));
    let f = fail.clone();
    fault::install(&dir, Arc::new(move |_| if f.load(Ordering::SeqCst) { Err(std::io::Error::other("disk full (test)")) } else { Ok(()) }));
    store.append(LOG, b"1\n").unwrap();
    store.append(LOG, b"2\n").unwrap();
    wait_until("the failure is published", || store.unwritten().is_some());
    let (held, why) = store.unwritten().unwrap();
    assert!(why.contains("disk full"), "{why}");
    assert!(held >= 1, "{held}");
    let refused = store.append(LOG, b"3\n");
    assert!(refused.is_err(), "refused while held");
    assert!(store.flush().is_err(), "a flush says it isn't written");
    // a read through the store still sees what was handed over
    assert_eq!(store.read(LOG).unwrap().unwrap(), b"0\n1\n2\n");
    // a cut within what is held keeps it (the journal cuts back to its own length on an error)
    store.truncate(LOG, 6).unwrap();
    assert_eq!(store.read(LOG).unwrap().unwrap(), b"0\n1\n2\n");
    // a fragment of a failed write left in the file is cut before the retry
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(dir.join(LOG)).unwrap();
        file.write_all(b"{\"seq\":").unwrap();
    }
    fail.store(false, Ordering::SeqCst);
    wait_until("retried on its own", || store.unwritten().is_none());
    store.append(LOG, b"3\n").unwrap();
    store.flush().unwrap();
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"0\n1\n2\n3\n");
    fault::remove(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

/// (b, c) Through the journal: ops appended while the drive is slow or failing all reopen, and a
/// journal over a log that failed has no gap (no damage on load).
#[test]
fn the_journal_reopens_every_op_after_a_slow_then_failing_drive() {
    use crate::{Catalog, Journal, Op, Photo, PhotoId, Source};
    let dir = temp_dir("journal");
    let (mut j, mut c, _) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let f = fail.clone();
    fault::install(
        &dir,
        Arc::new(move |step| {
            if f.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("network share dropped (test)"));
            }
            if step == Step::Sync {
                std::thread::sleep(Duration::from_millis(40));
            }
            Ok(())
        }),
    );
    let mut queued: Vec<Op> = Vec::new();
    for id in 1..=60u64 {
        if id == 20 {
            fail.store(true, Ordering::SeqCst);
        }
        if id == 40 {
            fail.store(false, Ordering::SeqCst);
        }
        let p = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, &format!("p{id}.jpg"), "JPEG", 4, 3, "2026-11-02");
        let op = Op::AddPhoto { photo: Box::new(p) };
        c.apply(op.clone()).unwrap();
        queued.push(op);
        // as the engine does: a failed append keeps its ops queued for the next one
        if j.append(&queued).is_ok() {
            queued.clear();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    wait_until("everything written", || {
        if !queued.is_empty() && j.append(&queued).is_ok() {
            queued.clear();
        }
        queued.is_empty() && j.unwritten().is_none()
    });
    j.flush().unwrap();
    let expect = c.to_snapshot();
    drop(j);
    fault::remove(&dir);
    let (_, again, report): (_, Catalog, _) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    assert!(report.damaged.is_none(), "{report:?}");
    assert_eq!(again.to_snapshot(), expect);
    assert_eq!(again.len(), 60);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A sync that fails leaves the written bytes past the synced length in the file; a cut to exactly
/// that length removes them from the file too, so they never come back on the next open (review
/// of #3557).
#[test]
fn a_cut_to_the_synced_length_removes_bytes_a_failed_sync_left() {
    let dir = temp_dir("cut-synced");
    let mut store = FsStore::open(&dir).unwrap();
    store
        .append(
            LOG, b"0
",
        )
        .unwrap();
    store.flush().unwrap();
    fault::install(&dir, Arc::new(|step| if step == Step::Sync { Err(std::io::Error::other("sync failed (test)")) } else { Ok(()) }));
    store
        .append(
            LOG, b"1
",
        )
        .unwrap();
    wait_until("the failure is published", || store.unwritten().is_some());
    assert_eq!(
        std::fs::read(dir.join(LOG)).unwrap(),
        b"0
1
",
        "written, not synced"
    );
    store.truncate(LOG, 2).unwrap();
    fault::remove(&dir);
    drop(store);
    assert_eq!(
        std::fs::read(dir.join(LOG)).unwrap(),
        b"0
"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
