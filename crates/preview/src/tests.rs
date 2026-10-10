use std::sync::Arc;

use lightcraft_raster::Rgba8;

use super::*;

#[test]
fn cleared_generation_cannot_read_or_repopulate_memory_or_disk() {
    let dir = std::env::temp_dir().join(format!("lc-thumbnail-generation-{}-{}", std::process::id(), next_tick()));
    let cache = Arc::new(PreviewCache::with_disk(1 << 20, &dir, 1 << 20));
    let key = hash_bytes(b"same-key");
    let old = cache.generation();
    let wrong = Arc::new(gradient(8, 8, 255));
    let correct = Arc::new(gradient(8, 8, 0));
    cache.put_at(old, key, wrong.clone());
    assert!(cache.get_at(old, key).is_some());
    cache.clear();
    assert!(cache.get(key).is_none());
    assert!(cache.get_at(old, key).is_none());
    cache.put_at(old, key, wrong.clone());
    cache.put_deferred_at(old, key, wrong);
    assert!(cache.get(key).is_none());
    assert!(cache.disk().unwrap().get(key).is_none());
    cache.put_at(cache.generation(), key, correct.clone());
    assert_eq!(cache.get(key).unwrap().as_bytes(), correct.as_bytes());
    cache.clear();
}

#[test]
fn retired_cache_keeps_its_files_but_refuses_every_read_and_write() {
    let dir = temp_dir("retire");
    let old = Arc::new(PreviewCache::with_disk(1 << 20, &dir, 1 << 20));
    let (kept, late) = (hash_bytes(b"kept"), hash_bytes(b"late"));
    let img = Arc::new(gradient(8, 8, 7));
    let generation = old.generation();
    old.put_at(generation, kept, img.clone());
    old.retire();
    assert_ne!(old.generation(), generation);
    assert!(old.get_at(generation, kept).is_none());
    assert!(old.get(kept).is_none());
    old.put_at(generation, late, img.clone());
    old.put_deferred_at(generation, late, img.clone());
    old.put(late, img.clone());
    old.put_deferred(late, img.clone());
    old.clear();
    // a replacement for the same directory still finds the valid file, and only that
    let new = PreviewCache::with_disk(1 << 20, &dir, 1 << 20);
    assert_eq!(new.get(kept).map(|i| (i.width, i.height)), Some((8, 8)), "the disk file stays (JPEG: lossy)");
    assert!(new.get(late).is_none());
    new.clear();
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-preview-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn gradient(w: usize, h: usize, seed: u8) -> Rgba8 {
    let data = (0..w * h).map(|i| [((i % w) * 255 / w) as u8, ((i / w) * 255 / h) as u8, seed, 255]).collect();
    Rgba8 { width: w, height: h, data }
}

#[test]
fn hashes_are_stable_and_distinct() {
    let a = hash_bytes(b"hello");
    assert_eq!(a, hash_bytes(b"hello"));
    assert_ne!(a, hash_bytes(b"hellp"));
    assert_eq!(Hash128::parse(&a.to_string()), Some(a));
    assert_ne!(Hasher128::new().str("ab").str("c").finish(), Hasher128::new().str("a").str("bc").finish());
    // streaming == one shot
    assert_eq!(Hasher128::new().update(b"hel").update(b"lo").finish(), a);
}

#[test]
fn lru_evicts_least_recent_by_cost() {
    let mut l = Lru::new(10);
    l.insert(1, "a", 4);
    l.insert(2, "b", 4);
    assert_eq!(l.get(&1), Some(&"a")); // 1 is now most recent
    l.insert(3, "c", 4); // over budget: evicts 2
    assert!(l.contains(&1) && !l.contains(&2) && l.contains(&3));
    assert_eq!(l.cost(), 8);
    l.insert(4, "huge", 50); // alone over budget: kept, everything else evicted
    assert_eq!((l.len(), l.cost()), (1, 50));
    l.insert(4, "small", 1);
    assert_eq!((l.len(), l.cost()), (1, 1));
    l.retain(|k| *k != 4);
    assert!(l.is_empty());
}

#[test]
fn disk_cache_roundtrip_and_prune() {
    let dir = temp_dir("disk");
    let d = DiskCache::new(&dir, 40_000);
    let img = gradient(160, 100, 7);
    let k = hash_bytes(b"k1");
    assert!(d.get(k).is_none());
    d.put(k, &img);
    let back = d.get(k).expect("hit");
    assert_eq!((back.width, back.height), (160, 100));
    let err: i64 = img.data.iter().zip(&back.data).map(|(a, b)| (a[0] as i64 - b[0] as i64).abs() + (a[1] as i64 - b[1] as i64).abs()).max().unwrap();
    assert!(err < 24, "JPEG error {err}");
    assert!(back.data.iter().all(|p| p[3] == 255));
    // fill past the budget: the oldest entries go, the newest stay
    let keys: Vec<_> = (0..40u8).map(|i| hash_bytes(&[i, 1])).collect();
    for (i, k) in keys.iter().enumerate() {
        d.put(*k, &gradient(160, 100, i as u8));
    }
    assert!(d.size() <= 40_000, "{}", d.size());
    assert!(d.get(*keys.last().unwrap()).is_some());
    assert!(d.get(keys[0]).is_none());
    // a corrupt file is a miss and gets removed
    let k2 = hash_bytes(b"corrupt");
    d.put(k2, &img);
    let hex = k2.to_string();
    let p = dir.join(&hex[..2]).join(format!("{hex}.jpg"));
    std::fs::write(&p, b"not a jpeg").unwrap();
    assert!(d.get(k2).is_none());
    assert!(!p.exists());
    d.clear();
    assert_eq!(d.size(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preview_cache_memory_then_disk() {
    let dir = temp_dir("pc");
    let k = hash_bytes(b"x");
    {
        let c = PreviewCache::with_disk(1 << 20, &dir, 1 << 24);
        c.put(k, Arc::new(gradient(64, 48, 1)));
        assert_eq!(c.mem_usage().0, 1);
        assert!(c.get(k).is_some());
    }
    // a new process: memory is empty, disk serves it
    let c = PreviewCache::with_disk(1 << 20, &dir, 1 << 24);
    assert_eq!(c.mem_usage().0, 0);
    assert_eq!(c.get(k).map(|i| i.width), Some(64));
    assert_eq!(c.mem_usage().0, 1);
    assert_eq!(c.disk().unwrap().hits.load(std::sync::atomic::Ordering::Relaxed), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pool_priorities_dedupe_and_drop() {
    let mut p: JobPool<u32, u32> = JobPool::new(1);
    // One worker, held busy by a gate job while the others queue up.
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let g = gate.clone();
    p.submit(
        999,
        0,
        1000,
        Box::new(move || {
            let (m, cv) = &*g;
            let mut open = m.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
            999
        }),
    );
    // wait until the worker took the gate job
    while p.queued() > 0 {
        std::thread::yield_now();
    }
    p.submit(1, 1, 5, Box::new(|| 1)); // off-screen thumb
    p.submit(2, 1, 10, Box::new(|| 2)); // visible thumb
    p.submit(3, 1, 100, Box::new(|| 3)); // loupe
    p.submit(2, 2, 10, Box::new(|| 22)); // newer request for slot 2 replaces the queued one
    p.submit(4, 1, 5, Box::new(|| 4));
    assert_eq!(p.queued(), 4);
    let dropped = p.reprioritize(|s, pr| if *s == 4 { None } else { Some(pr) });
    assert_eq!(dropped, vec![4]);
    {
        let (m, cv) = &*gate;
        *m.lock().unwrap() = true;
        cv.notify_all();
    }
    let mut order = Vec::new();
    while order.len() < 4 {
        if let Some(d) = p.try_recv() {
            order.push((d.slot, d.key, d.result));
        } else {
            std::thread::yield_now();
        }
    }
    assert_eq!(order, vec![(999, 0, 999), (3, 1, 3), (2, 2, 22), (1, 1, 1)]);
}

#[test]
fn pool_runs_inline() {
    // No workers: run_inline drains the queue on the caller's thread (the wasm path).
    let mut p: JobPool<u8, u8> = JobPool::new(0);
    p.submit(1, 0, 1, Box::new(|| 1));
    p.submit(2, 0, 9, Box::new(|| 2));
    assert_eq!(p.run_inline(1), 1);
    assert_eq!(p.try_recv().map(|d| d.result), Some(2));
    assert_eq!(p.run_inline(10), 1);
    assert_eq!(p.try_recv().map(|d| d.result), Some(1));
}

/// A pool with one worker that is inside a job until the returned sender is used or dropped.
fn pool_with_a_running_job() -> (JobPool<u8, ()>, std::sync::mpsc::Sender<()>) {
    let mut p: JobPool<u8, ()> = JobPool::new(1);
    let (started, has_started) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    p.submit(
        1,
        0,
        1,
        Box::new(move || {
            let _ = started.send(());
            let _ = released.recv_timeout(std::time::Duration::from_secs(60));
        }),
    );
    has_started.recv().unwrap();
    (p, release)
}

#[test]
fn pool_drop_waits_for_running_job() {
    // issue #620: a job that outlives its pool can still be inside the GPU driver when the process exits
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut p: JobPool<u8, ()> = JobPool::new(1);
    let (started, has_started) = std::sync::mpsc::channel();
    let (finished, queued_ran) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let f = finished.clone();
    p.submit(
        1,
        0,
        9,
        Box::new(move || {
            let _ = started.send(());
            std::thread::sleep(std::time::Duration::from_millis(200));
            f.store(true, Ordering::SeqCst);
        }),
    );
    has_started.recv().unwrap();
    let q = queued_ran.clone();
    p.submit(2, 0, 1, Box::new(move || q.store(true, Ordering::SeqCst)));
    drop(p);
    assert!(finished.load(Ordering::SeqCst), "drop returned while the job was still running");
    assert!(!queued_ran.load(Ordering::SeqCst), "a job that had not started is discarded");
}

#[test]
fn pool_shutdown_reports_workers_and_drop_does_not_wait_again() {
    let (mut p, release) = pool_with_a_running_job();
    assert_eq!(p.live_workers(), 1);
    let t0 = std::time::Instant::now();
    assert!(!p.shutdown(std::time::Duration::from_millis(100)), "the job is still running");
    let waited = t0.elapsed();
    assert!(waited >= std::time::Duration::from_millis(100) && waited < std::time::Duration::from_secs(30), "{waited:?}");
    assert_eq!(p.live_workers(), 1);
    // stopped: nothing new is queued, and the job that ends is noticed
    p.submit(2, 0, 1, Box::new(|| ()));
    assert_eq!(p.queued(), 0);
    drop(release);
    assert!(p.shutdown(std::time::Duration::from_secs(30)));
    assert_eq!(p.live_workers(), 0);

    let (mut p, _release) = pool_with_a_running_job();
    assert!(!p.shutdown(std::time::Duration::ZERO));
    let t0 = std::time::Instant::now();
    drop(p);
    assert!(t0.elapsed() < std::time::Duration::from_secs(1), "dropped after a shutdown: no second wait");
}

#[test]
fn pool_drop_is_bounded_when_a_job_never_ends() {
    // a job blocked on something its owner holds must not hang the owner (it would hang quitting)
    let (p, _release) = pool_with_a_running_job();
    let t0 = std::time::Instant::now();
    drop(p);
    let waited = t0.elapsed();
    assert!(waited >= std::time::Duration::from_secs(1) && waited < std::time::Duration::from_secs(30), "{waited:?}");
}

#[test]
fn pool_stops_after_a_job_panicked_and_when_dropped_by_its_own_job() {
    // a worker that unwound is not waited for
    let mut p: JobPool<u8, ()> = JobPool::new(1);
    p.submit(1, 0, 1, Box::new(|| panic!("a job panics (expected in this test)")));
    while p.live_workers() > 0 {
        std::thread::yield_now();
    }
    assert!(p.shutdown(std::time::Duration::from_secs(30)));

    // the last owner of a pool is one of its jobs: the worker must not wait for (or join) itself
    let slot: Arc<std::sync::Mutex<Option<JobPool<u8, ()>>>> = Arc::new(std::sync::Mutex::new(None));
    let (dropped, has_dropped) = std::sync::mpsc::channel();
    let mut p: JobPool<u8, ()> = JobPool::new(1);
    let s = slot.clone();
    p.submit(
        1,
        0,
        1,
        Box::new(move || {
            let own = loop {
                if let Some(p) = s.lock().unwrap().take() {
                    break p;
                }
                std::thread::yield_now();
            };
            let t0 = std::time::Instant::now();
            drop(own);
            let _ = dropped.send(t0.elapsed());
        }),
    );
    *slot.lock().unwrap() = Some(p);
    let waited = has_dropped.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
    assert!(waited < std::time::Duration::from_secs(1), "{waited:?}");
}

#[test]
fn disk_cache_never_touches_foreign_files() {
    // issue #98: a library opened on a folder that already has a `thumbs/` folder
    let dir = temp_dir("foreign");
    let foreign = [
        dir.join("readme.txt"),
        dir.join("ab").join("holiday.jpg"),
        dir.join("ab").join("0123456789abcdef0123456789abcdef.jpg"), // wrong shard
        dir.join("small").join("0123456789abcdef0123456789abcdef.jpg"),
        dir.join("01").join("0123456789ABCDEF0123456789ABCDEF.jpg"), // upper case
        dir.join("01").join("0123456789abcdef0123456789abcdef.jpeg"),
        dir.join("01").join("0123456789abcdef0123456789abcdef.tmp"),
    ];
    for p in &foreign {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![7u8; 50_000]).unwrap();
    }
    // a stale temp file of our own is cleaned up
    let own_tmp = dir.join("01").join("0123456789abcdef0123456789abcdef.tmpThreadId9");
    std::fs::write(&own_tmp, b"partial").unwrap();
    let d = DiskCache::new(&dir, 40_000);
    assert_eq!(d.size(), 7, "only the cache's own temp file counts");
    // fill past the budget (forces prunes), then clear
    for i in 0..40u8 {
        d.put(hash_bytes(&[i, 2]), &gradient(160, 100, i));
    }
    assert!(d.size() <= 40_000);
    d.clear();
    assert_eq!(d.size(), 0);
    assert!(!own_tmp.exists());
    for p in &foreign {
        assert!(p.exists(), "foreign file deleted: {}", p.display());
    }
    assert!(disk::is_cache_file("01", "0123456789abcdef0123456789abcdef.jpg"));
    assert!(!disk::is_cache_file("01", "0123456789abcdef0123456789abcdef.jpg.bak"));
    let _ = std::fs::remove_dir_all(&dir);
}
