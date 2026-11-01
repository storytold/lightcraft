//! PERF-STEP-STALL: culling on a slow or failing drive. A flag or rating never waits for the drive
//! (the catalog log is written and synced by a thread of its own), and nothing made meanwhile is
//! lost: it is on disk once the save lands, after a clean quit, after a quit with no close, and,
//! when the drive fails, held, retried and reported as unsaved until it is written.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lightcraft_catalog::fault::{self, Step};
use lightcraft_catalog::{Flag, PhotoId};
use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-engine-slow-drive-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn open(dir: &std::path::Path) -> Session {
    let mut s = Session::new();
    s.open_library(dir, true).unwrap();
    s
}

fn ids(s: &Session) -> Vec<PhotoId> {
    let mut v: Vec<PhotoId> = s.catalog.photos().map(|p| p.id).collect();
    v.sort();
    v
}

/// Every sync of the log in `dir` takes `sync` (a busy disk: Windows makes a write wait for a sync
/// of the same file in flight); `fail` makes every write fail while it is set.
fn slow_drive(dir: &std::path::Path, sync: Duration, fail: Arc<AtomicBool>) {
    fault::install(
        dir,
        Arc::new(move |step| {
            if fail.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("the drive is gone (test)"));
            }
            if step == Step::Sync {
                std::thread::sleep(sync);
            }
            Ok(())
        }),
    );
}

/// The flag or rating each photo got, as `(id, flag, rating)`.
fn culled(s: &Session) -> Vec<(PhotoId, Flag, u8)> {
    ids(s).into_iter().filter_map(|id| s.catalog.photo(id).map(|p| (id, p.flag, p.rating))).collect()
}

/// Flag and rate every photo the way a photographer culls: pick / reject in turn, a rating on
/// every other one. Returns the slowest command, in ms.
fn cull(s: &mut Session, rounds: usize) -> f64 {
    let all = ids(s);
    let mut slowest = 0.0f64;
    for k in 0..rounds {
        for (i, id) in all.iter().enumerate() {
            let flag = if (i + k) % 2 == 0 { "pick" } else { "reject" };
            let t = Instant::now();
            s.execute("photo.flag", &json!({"flag": flag, "ids": [id.0]})).unwrap();
            slowest = slowest.max(t.elapsed().as_secs_f64() * 1000.0);
            if i % 2 == 0 {
                let t = Instant::now();
                s.execute("photo.rate", &json!({"rating": ((i + k) % 5) as u8 + 1, "ids": [id.0]})).unwrap();
                slowest = slowest.max(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    slowest
}

/// (a) With every sync of the log taking 300 ms, a flag or rating returns long before one sync.
/// The same culling with the log written on the caller's thread (as before PERF-STEP-STALL) waits
/// for a sync on every key, which is what this test would see without the fix.
#[test]
fn a_flag_never_waits_for_a_slow_drive() {
    let sync = Duration::from_millis(300);
    let mut seen = Vec::new();
    for on_caller in [true, false] {
        let dir = temp_dir(if on_caller { "wait-before" } else { "wait" });
        fault::write_on_caller(&dir, on_caller);
        let mut s = open(&dir);
        slow_drive(&dir, sync, Arc::new(AtomicBool::new(false)));
        let t = Instant::now();
        let slowest = cull(&mut s, 1);
        let total = t.elapsed().as_secs_f64() * 1000.0;
        let n = s.library.as_ref().map_or(0, |l| l.journal().stats().appends);
        eprintln!(
            "{} drive syncing in 300 ms: slowest command {slowest:.1} ms, {total:.0} ms for the culling",
            if on_caller { "before (log written on the caller):" } else { "after (log on its own thread):" }
        );
        seen.push((slowest, total, n));
        drop(s);
        fault::remove(&dir);
        fault::write_on_caller(&dir, false);
        let _ = std::fs::remove_dir_all(&dir);
    }
    let ((before, _, _), (after, _, _)) = (seen[0], seen[1]);
    assert!(before >= 300.0, "the caller waited for its sync before: {before:.1} ms");
    // a debug build of the whole workspace's tests at once leaves room; one sync is 300 ms
    assert!(after < 120.0, "a command waited for the drive: {after:.1} ms");
}

/// (b) Everything flagged and rated while slow saves are in flight is on disk once they land
/// (a flush), after a quit with no close (the process ends normally: the log's thread finishes),
/// and after a clean quit (`close_library`).
#[test]
fn every_change_made_during_a_slow_save_is_on_disk() {
    for how in ["flush", "drop", "close"] {
        let dir = temp_dir(&format!("kept-{how}"));
        let mut s = open(&dir);
        slow_drive(&dir, Duration::from_millis(150), Arc::new(AtomicBool::new(false)));
        cull(&mut s, 3);
        let want = culled(&s);
        let expect = s.catalog.to_snapshot();
        match how {
            "flush" => {
                s.library.as_mut().unwrap().journal_mut().flush().unwrap();
                // a copy of the files taken now (the next crash) holds everything
                let copy = temp_dir("kept-copy");
                std::fs::create_dir_all(&copy).unwrap();
                for f in [lightcraft_catalog::journal::SNAPSHOT, lightcraft_catalog::journal::LOG] {
                    if dir.join(f).exists() {
                        std::fs::copy(dir.join(f), copy.join(f)).unwrap();
                    }
                }
                let (_, c, _) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::store::FsStore::open(&copy).unwrap())).unwrap();
                assert_eq!(c.to_snapshot(), expect, "{how}");
                let _ = std::fs::remove_dir_all(&copy);
            }
            "close" => s.close_library().unwrap(),
            _ => {}
        }
        assert!(s.unsaved().is_none(), "{how}: {:?}", s.unsaved());
        drop(s);
        fault::remove(&dir);
        let again = open(&dir);
        assert_eq!(culled(&again), want, "{how}");
        assert_eq!(again.catalog.to_snapshot(), expect, "{how}");
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Wait (bounded) until `done` holds, calling the frame loop's save meanwhile.
fn frames_until(s: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    let t = Instant::now();
    while !done(s) {
        assert!(t.elapsed() < Duration::from_secs(20), "{what}: {:?}", s.unsaved());
        s.persist_if_dirty();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// (c) A drive that fails: the flags handed over before the failure are held and retried, never
/// dropped; the failure is reported (`unsaved`, the quit prompt's source) and later flags fail
/// with `NotSaved` and stay queued; once the drive is back, all of it is written, in order, with
/// no gap in the log.
#[test]
fn a_failed_write_is_reported_and_retried_never_dropped() {
    let dir = temp_dir("fail");
    let mut s = open(&dir);
    let fail = Arc::new(AtomicBool::new(false));
    slow_drive(&dir, Duration::from_millis(5), fail.clone());
    let all = ids(&s);
    fail.store(true, Ordering::SeqCst);
    // handed over before the thread found out: accepted, then held
    s.execute("photo.flag", &json!({"flag": "pick", "ids": [all[0].0]})).unwrap();
    frames_until(&mut s, "the failure is reported", |s| s.unsaved().is_some());
    let (n, why) = s.unsaved().map(|(n, w)| (n, w.to_string())).unwrap();
    assert!(n >= 1 && why.contains("the drive is gone"), "{n} {why}");
    // later flags: refused with the reason, kept in memory and queued
    let r = s.execute("photo.flag", &json!({"flag": "reject", "ids": [all[1].0]}));
    assert!(matches!(r, Err(crate::EngineError::NotSaved(_))), "{r:?}");
    assert_eq!(s.catalog.photo(all[1]).unwrap().flag, Flag::Reject, "applied in memory");
    let _ = s.execute("photo.rate", &json!({"rating": 4, "ids": [all[2].0]}));
    assert!(s.unsaved().is_some_and(|(n, _)| n >= 3), "{:?}", s.unsaved());
    // the drive is back: the held record and the queued ones are written by the frame loop
    fail.store(false, Ordering::SeqCst);
    frames_until(&mut s, "written once the drive is back", |s| s.unsaved().is_none() && s.pending_log.is_empty());
    let want = culled(&s);
    let expect = s.catalog.to_snapshot();
    // a crash now: the files alone hold everything, and the log has no gap (no damage on load)
    s.library.as_mut().unwrap().journal_mut().flush().unwrap();
    let copy = temp_dir("fail-copy");
    std::fs::create_dir_all(&copy).unwrap();
    for f in [lightcraft_catalog::journal::SNAPSHOT, lightcraft_catalog::journal::LOG] {
        if dir.join(f).exists() {
            std::fs::copy(dir.join(f), copy.join(f)).unwrap();
        }
    }
    let (_, c, report) = lightcraft_catalog::Journal::open(Box::new(lightcraft_catalog::store::FsStore::open(&copy).unwrap())).unwrap();
    assert!(report.damaged.is_none(), "{report:?}");
    assert_eq!(c.to_snapshot(), expect);
    let _ = std::fs::remove_dir_all(&copy);
    fault::remove(&dir);
    drop(s);
    assert_eq!(culled(&open(&dir)), want);
    let _ = std::fs::remove_dir_all(&dir);
}

/// (c) The drive stays broken for the log until the quit: the quit prompt is told (unsaved), and
/// the closing snapshot (a fresh file) saves the held flags, so they are there on the next open.
#[test]
fn a_log_that_stays_broken_is_saved_by_the_closing_snapshot() {
    let dir = temp_dir("broken");
    let mut s = open(&dir);
    let all = ids(&s);
    let fail = Arc::new(AtomicBool::new(false));
    slow_drive(&dir, Duration::from_millis(5), fail.clone());
    fail.store(true, Ordering::SeqCst);
    s.execute("photo.flag", &json!({"flag": "reject", "ids": [all[0].0]})).unwrap();
    frames_until(&mut s, "the failure is reported", |s| s.unsaved().is_some());
    let _ = s.execute("photo.flag", &json!({"flag": "pick", "ids": [all[1].0]}));
    let want = culled(&s);
    // (the snapshot itself is written by the store, which the hook leaves alone; the log's reset
    // after it fails, so close reports it, but the snapshot holds everything)
    let _ = s.close_library();
    drop(s);
    fault::remove(&dir);
    let again = open(&dir);
    assert!(again.library.as_ref().unwrap().report.damaged.is_none());
    assert_eq!(culled(&again), want);
    drop(again);
    let _ = std::fs::remove_dir_all(&dir);
}
