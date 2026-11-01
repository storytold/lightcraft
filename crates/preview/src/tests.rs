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
    let p = dir.join(&hex[..2]).join(format!("{hex}.lcthumb"));
    std::fs::write(&p, b"not a jpeg").unwrap();
    assert!(d.get(k2).is_none());
    assert!(!p.exists());
    d.clear();
    assert_eq!(d.size(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn disk_cache_overwrite_accounts_for_replaced_jpeg_only_once() {
    let dir = temp_dir("overwrite");
    let cache = DiskCache::new(&dir, 1 << 20);
    let (a, b) = (hash_bytes(b"overwrite-a"), hash_bytes(b"overwrite-b"));
    let first = gradient(80, 60, 4);
    cache.put(a, &first);
    cache.put(b, &gradient(80, 60, 7));
    let initial = cache.size();

    // Re-rendering the same key used to double-count bytes although the
    // replacement occupied exactly the same pathname on disk.
    cache.put(a, &first);
    assert_eq!(cache.size(), initial);
    assert!(cache.get(b).is_some());

    // A replacement with a different encoded size must also update the total.
    cache.put(a, &gradient(120, 90, 9));
    let path = |key: Hash128| {
        let hex = key.to_string();
        dir.join(&hex[..2]).join(format!("{hex}.lcthumb"))
    };
    let actual = std::fs::metadata(path(a)).unwrap().len() + std::fs::metadata(path(b)).unwrap().len();
    assert_eq!(cache.size(), actual);
    cache.clear();
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
    assert!(disk::is_cache_file("01", "0123456789abcdef0123456789abcdef.lcthumb"));
    // (what the cache wrote before it stopped naming its files as pictures is still its own)
    assert!(disk::is_cache_file("01", "0123456789abcdef0123456789abcdef.jpg"));
    assert!(!disk::is_cache_file("01", "0123456789abcdef0123456789abcdef.jpg.bak"));
    assert!(!disk::is_cache_file("01", "0123456789abcdef0123456789abcdef.lcthumb.bak"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the cache is nobody's pictures (issues #610, #1066)

/// Every file under `dir`, by its path from there.
fn files_under(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                todo.push(e.path());
            } else {
                out.push(e.path().strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

/// The endings by which a photo gallery, a backup of "my pictures" or a media scanner takes a
/// file for a picture.
const PICTURE_ENDINGS: [&str; 12] = ["jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "avif"];

fn is_named_as_a_picture(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ending)| PICTURE_ENDINGS.contains(&ending.to_ascii_lowercase().as_str()))
}

/// Given a library in the Pictures folder, when thumbnails are cached, then no file the cache
/// writes is named as a picture: a photo gallery that looks through Pictures does not list the
/// cache as the person's photos, however many thumbnails there are.
#[test]
fn nothing_the_cache_writes_is_named_as_a_picture() {
    let dir = temp_dir("not-pictures");
    let d = DiskCache::new(&dir, 10 << 20);
    for i in 0..12u8 {
        d.put(hash_bytes(&[i, 9]), &gradient(160, 100, i));
    }
    let written = files_under(&dir);
    assert!(written.len() >= 12, "{written:?}");
    let pictures: Vec<&String> = written.iter().filter(|f| is_named_as_a_picture(f)).collect();
    assert!(pictures.is_empty(), "named as pictures: {pictures:?}");
    // and they are found again under those names
    assert!((0..12u8).all(|i| d.get(hash_bytes(&[i, 9])).is_some()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A shard directory that is a symlink to a folder outside the cache is neither listed, cleared,
/// pruned, read nor written (issue #1504).
#[cfg(unix)]
#[test]
fn disk_cache_ignores_symlinked_shard_directories() {
    let dir = temp_dir("symlink-shard");
    let outside = temp_dir("symlink-outside");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let key = hash_bytes(&[9, 9]);
    let hex = key.to_string();
    let sentinel = outside.join(format!("{hex}.jpg"));
    std::fs::write(&sentinel, vec![7u8; 50_000]).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join(&hex[..2])).unwrap();
    let d = DiskCache::new(&dir, 10);
    assert_eq!(d.size(), 0, "files behind the link are not the cache's");
    d.prune();
    assert!(sentinel.exists(), "prune followed the link");
    assert!(d.get(key).is_none(), "get read through the link");
    d.put(key, &gradient(16, 16, 1));
    assert_eq!(std::fs::metadata(&sentinel).unwrap().len(), 50_000, "put wrote through the link");
    d.clear();
    assert!(sentinel.exists(), "clear followed the link");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

/// A folder whose shard is a symlink to somewhere else is not only the cache's: tidying it
/// neither follows the link nor tags the folder as a cache (issue #1504).
#[cfg(unix)]
#[test]
fn tidying_does_not_declare_a_folder_with_a_symlinked_shard_a_cache() {
    let dir = temp_dir("symlink-tidy");
    let outside = temp_dir("symlink-tidy-outside");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let hex = hash_bytes(&[9, 8]).to_string();
    // (named as a thumbnail is now: only the link says it is not the cache's)
    let sentinel = outside.join(format!("{hex}.lcthumb"));
    std::fs::write(&sentinel, b"somebody's file").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join(&hex[..2])).unwrap();
    let d = DiskCache::new(&dir, 100 << 20);
    assert!(d.needs_tidying());
    assert_eq!(d.take_over_old(&|| false, &|_, _| {}), 0, "nothing behind the link is taken over");
    assert!(sentinel.is_file(), "the file behind the link is where it was");
    assert!(!dir.join("CACHEDIR.TAG").exists(), "a folder with a link in it is not declared a cache");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

/// The bytes of every file under `dir` that is the cache's own.
fn cache_bytes_on_disk(dir: &std::path::Path) -> u64 {
    files_under(dir)
        .iter()
        .filter(|f| f.split_once('/').is_some_and(|(shard, name)| disk::is_cache_file(shard, name)))
        .map(|f| std::fs::metadata(dir.join(f)).unwrap().len())
        .sum()
}

/// Given a cache from before, whose thumbnails are `.jpg` files, when one is asked for, then it
/// is found (nothing is rendered again for the change of name) and is no longer a `.jpg`. When
/// the cache is tidied, none of its old files is a `.jpg` any more, each thumbnail is there
/// once and counted once, and the folder (which holds nothing but the cache) is tagged. Asking
/// how big the cache is renames nothing: that is asked from the interface.
#[test]
fn thumbnails_cached_as_jpg_files_are_taken_over_under_the_new_name() {
    let dir = temp_dir("taken-over");
    let old_path = |k: Hash128| {
        let hex = k.to_string();
        dir.join(&hex[..2]).join(format!("{hex}.jpg"))
    };
    // (thousands in one folder: renaming while a folder is listed lists some of them twice)
    let keys: Vec<Hash128> = (0..3000u128).map(|i| Hash128((0xab << 120) | i)).collect();
    let jpeg = disk::encode_jpeg(&gradient(32, 20, 3)).unwrap();
    for k in &keys {
        std::fs::create_dir_all(old_path(*k).parent().unwrap()).unwrap();
        std::fs::write(old_path(*k), &jpeg).unwrap();
    }
    // one that is there under both names (an older build ran after a newer one)
    std::fs::write(old_path(keys[1]).with_extension("lcthumb"), &jpeg).unwrap();

    let d = DiskCache::new(&dir, 100 << 20);
    let back = d.get(keys[0]).expect("found under its old name");
    assert_eq!((back.width, back.height), (32, 20));
    assert!(!old_path(keys[0]).exists(), "and no longer a .jpg");
    assert_eq!(d.writes.load(std::sync::atomic::Ordering::Relaxed), 0, "nothing was rendered or written anew");
    let before = d.size();
    assert_eq!(before, cache_bytes_on_disk(&dir));
    assert!(old_path(keys[2]).exists(), "asking for the size renames nothing");

    let seen = std::sync::atomic::AtomicUsize::new(0);
    let taken = d.take_over_old(&|| false, &|done, of| {
        assert_eq!(of, 2999, "the old files, less the one already asked for");
        seen.store(done, std::sync::atomic::Ordering::Relaxed);
    });
    assert_eq!(taken, 2998, "all but the one already asked for and the one that was there twice");
    assert_eq!(seen.load(std::sync::atomic::Ordering::Relaxed), 2999, "and its progress came to the end");
    let left: Vec<String> = files_under(&dir).into_iter().filter(|f| is_named_as_a_picture(f)).collect();
    assert_eq!(left, Vec::<String>::new(), "nothing of the cache's is named as a picture");
    assert_eq!(d.size(), cache_bytes_on_disk(&dir), "each thumbnail counted once");
    assert_eq!(d.size(), jpeg.len() as u64 * 3000);
    assert!(keys.iter().all(|k| d.get(*k).is_some()));
    assert!(dir.join("CACHEDIR.TAG").is_file(), "a folder that holds nothing but the cache is tagged");
    // tidied once: there is nothing more to do, and it says so
    assert!(!d.needs_tidying());
    assert_eq!(d.take_over_old(&|| false, &|_, _| {}), 0);
    // a thumbnail written while the size was not known is counted with the rest, not alone
    let fresh = DiskCache::new(&dir, 100 << 20);
    fresh.put(hash_bytes(b"one more"), &gradient(32, 20, 9));
    assert_eq!(fresh.size(), cache_bytes_on_disk(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a `thumbs` folder that also holds somebody else's files (a library opened on a photo
/// folder, issue #98), when the cache is tidied, then their files are as they were, whatever
/// they are named, and the folder is not declared a cache. A thing that is no file under a
/// thumbnail's new name keeps the old one from being touched. And tidying can be stopped.
#[test]
fn tidying_leaves_what_is_somebodys_and_can_be_stopped() {
    let dir = temp_dir("tidy-shared");
    let jpeg = disk::encode_jpeg(&gradient(32, 20, 3)).unwrap();
    let key = hash_bytes(b"ours");
    let hex = key.to_string();
    let ours = dir.join(&hex[..2]).join(format!("{hex}.jpg"));
    std::fs::create_dir_all(ours.parent().unwrap()).unwrap();
    std::fs::write(&ours, &jpeg).unwrap();
    let theirs = [dir.join("holiday.jpg"), dir.join(&hex[..2]).join("holiday.jpg"), dir.join("album").join("one.jpg")];
    for p in &theirs {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"somebody's picture").unwrap();
    }
    // something that is no file has another thumbnail's new name
    let blocked = hash_bytes(b"blocked").to_string();
    let blocked_old = dir.join(&blocked[..2]).join(format!("{blocked}.jpg"));
    std::fs::create_dir_all(blocked_old.with_extension("lcthumb")).unwrap();
    std::fs::write(&blocked_old, &jpeg).unwrap();

    let d = DiskCache::new(&dir, 100 << 20);
    assert!(d.needs_tidying());
    assert_eq!(d.take_over_old(&|| true, &|_, _| {}), 0, "stopped before anything");
    assert!(ours.exists() && d.needs_tidying(), "and to be gone on with");
    assert_eq!(d.take_over_old(&|| false, &|_, _| {}), 1);
    assert!(!ours.exists() && ours.with_extension("lcthumb").is_file());
    assert!(blocked_old.is_file(), "left where it is");
    for p in &theirs {
        assert_eq!(std::fs::read(p).unwrap(), b"somebody's picture", "{}", p.display());
    }
    assert!(!dir.join("CACHEDIR.TAG").exists(), "a folder with somebody's files in it is not declared a cache");
    assert!(d.needs_tidying(), "one could not be taken over: it is tried again");
    // with that out of the way it is done, once, though the folder is still shared
    std::fs::remove_dir(blocked_old.with_extension("lcthumb")).unwrap();
    assert_eq!(d.take_over_old(&|| false, &|_, _| {}), 1);
    assert!(!d.needs_tidying() && !dir.join("CACHEDIR.TAG").exists());

    // what a file manager leaves behind in a folder it showed does not make it somebody's
    let own = temp_dir("tidy-droppings");
    let k = hash_bytes(b"k").to_string();
    std::fs::create_dir_all(own.join(&k[..2])).unwrap();
    std::fs::write(own.join(&k[..2]).join(format!("{k}.jpg")), &jpeg).unwrap();
    for left in [own.join(".DS_Store"), own.join("desktop.ini"), own.join(&k[..2]).join("Thumbs.db"), own.join("._thumbs")] {
        std::fs::write(left, b"x").unwrap();
    }
    let d = DiskCache::new(&own, 100 << 20);
    assert_eq!(d.take_over_old(&|| false, &|_, _| {}), 1);
    assert!(own.join("CACHEDIR.TAG").is_file() && !d.needs_tidying());
    let _ = std::fs::remove_dir_all(&own);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a full cache, when it has to let thumbnails go, then the ones read in this session
/// stay over ones that were written later and never looked at: what is in use is not the first
/// to go, though reading it wrote nothing.
#[test]
fn a_full_cache_keeps_what_was_read_this_session() {
    let dir = temp_dir("read-kept");
    let d = DiskCache::new(&dir, 60_000);
    let keys: Vec<Hash128> = (0..8u8).map(|i| hash_bytes(&[i, 7])).collect();
    for (i, k) in keys.iter().enumerate() {
        d.put(*k, &gradient(160, 100, i as u8));
    }
    // the first two written are the ones in use
    assert!(d.get(keys[0]).is_some() && d.get(keys[1]).is_some());
    for i in 0..40u8 {
        d.put(hash_bytes(&[i, 8]), &gradient(160, 100, i));
    }
    assert!(d.size() <= 60_000, "{}", d.size());
    assert!(d.get(keys[0]).is_some() && d.get(keys[1]).is_some(), "what was read stayed");
    assert!(d.get(keys[5]).is_none(), "what was written after it and never read went");
    let _ = std::fs::remove_dir_all(&dir);
}

fn modified(p: &std::path::Path) -> std::time::SystemTime {
    std::fs::metadata(p).unwrap().modified().unwrap()
}

/// Given cached thumbnails, when they are read (as at every launch, for every photo in view),
/// then nothing is written: a file read today is as it was, date and all. Only one that was not
/// used for a long while has its date brought forward, so that what is still in use is not the
/// first to go when the cache is full.
#[test]
fn reading_a_thumbnail_writes_nothing() {
    let dir = temp_dir("read-only");
    let d = DiskCache::new(&dir, 10 << 20);
    let (fresh, stale) = (hash_bytes(b"fresh"), hash_bytes(b"stale"));
    let path = |k: Hash128| {
        let hex = k.to_string();
        dir.join(&hex[..2]).join(format!("{hex}.lcthumb"))
    };
    d.put(fresh, &gradient(160, 100, 1));
    d.put(stale, &gradient(160, 100, 2));
    let day = std::time::Duration::from_secs(24 * 3600);
    let now = std::time::SystemTime::now();
    let set = |k: Hash128, at: std::time::SystemTime| std::fs::File::options().write(true).open(path(k)).unwrap().set_modified(at).unwrap();
    set(fresh, now - day);
    set(stale, now - day * 60);
    let before = (modified(&path(fresh)), std::fs::read(path(fresh)).unwrap());
    for _ in 0..3 {
        assert!(d.get(fresh).is_some());
    }
    assert_eq!((modified(&path(fresh)), std::fs::read(path(fresh)).unwrap()), before, "read, and untouched");
    assert!(d.get(stale).is_some());
    assert!(modified(&path(stale)) > now - day, "one not used for two months is in use again");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A folder the cache makes for itself says that it is a cache (`CACHEDIR.TAG`, which backup
/// programs go by), and the tag is not one of its thumbnails: clearing the cache leaves it. A
/// folder that was there before (it may hold somebody's files, issue #98) is not tagged.
#[test]
fn a_folder_the_cache_makes_is_tagged_as_a_cache_and_one_that_was_there_is_not() {
    let (made, theirs) = (temp_dir("tagged"), temp_dir("untagged"));
    let own = made.join("thumbs");
    let d = DiskCache::new(&own, 10 << 20);
    assert!(!own.exists(), "nothing is made before there is something to keep");
    d.put(hash_bytes(b"a"), &gradient(160, 100, 1));
    let tag = std::fs::read_to_string(own.join("CACHEDIR.TAG")).expect("tagged");
    assert!(tag.starts_with("Signature: 8a477f597d28d172789f06886806bc55"), "{tag}");
    d.clear();
    assert_eq!(d.size(), 0);
    assert!(own.join("CACHEDIR.TAG").is_file());

    std::fs::create_dir_all(&theirs).unwrap();
    std::fs::write(theirs.join("holiday.jpg"), b"somebody's picture").unwrap();
    let d = DiskCache::new(&theirs, 10 << 20);
    d.put(hash_bytes(b"a"), &gradient(160, 100, 1));
    assert!(d.get(hash_bytes(b"a")).is_some());
    assert!(!theirs.join("CACHEDIR.TAG").exists(), "a folder with somebody's files in it is not declared a cache");
    for dir in [made, theirs] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// [`JobPool::wait`] returns once a job finishes, which [`JobPool::try_recv`] then hands out, or
/// after the timeout.
#[test]
fn pool_wait_keeps_the_finished_job_for_try_recv() {
    let mut p: JobPool<u8, u8> = JobPool::new(1);
    assert!(!p.wait(std::time::Duration::from_millis(10)), "nothing runs");
    p.submit(1, 0, 1, Box::new(|| 7));
    assert!(p.wait(std::time::Duration::from_secs(10)));
    assert!(p.wait(std::time::Duration::ZERO), "kept until taken");
    assert_eq!(p.try_recv().map(|d| d.result), Some(7));
    assert!(p.try_recv().is_none());
}

/// Wait up to `secs` for `f` to hold.
fn eventually(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_secs(secs) {
        if f() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

/// A slider drag submits for a view faster than its renders finish. A newer job for a slot used
/// to start beside the running one on another worker: one view's renders piled up on the GPU and
/// most were thrown away. Now a slot runs one job at a time: the newer one waits (replacing an
/// older one still queued), other slots keep going, and it starts when the slot's job ends.
#[test]
fn a_slot_runs_one_job_at_a_time() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut p: JobPool<u8, u32> = JobPool::new(4);
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (started, has_started) = std::sync::mpsc::channel();
    let running = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let job = |n: u32, wait: Option<std::sync::mpsc::Receiver<()>>| -> crate::pool::Job<u32> {
        let (running, most, started) = (running.clone(), most.clone(), started.clone());
        Box::new(move || {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            let _ = started.send(n);
            if let Some(w) = wait {
                let _ = w.recv_timeout(std::time::Duration::from_secs(30));
            }
            running.fetch_sub(1, Ordering::SeqCst);
            n
        })
    };
    p.submit(1, 1, 9, job(1, Some(released)));
    assert_eq!(has_started.recv().unwrap(), 1);
    // two newer requests for the busy slot: the second replaces the first, neither starts
    p.submit(1, 2, 9, job(2, None));
    p.submit(1, 3, 9, job(3, None));
    // another slot is not held up
    p.submit(2, 9, 1, Box::new(|| 9));
    let mut done = Vec::new();
    assert!(eventually(10, || {
        done.extend(std::iter::from_fn(|| p.try_recv()).map(|d| d.result));
        done.contains(&9)
    }));
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(has_started.try_recv().is_err(), "a second job of slot 1 started beside the first");
    assert!(p.is_queued(1));
    drop(release);
    assert!(eventually(10, || {
        done.extend(std::iter::from_fn(|| p.try_recv()).map(|d| d.result));
        done.contains(&3)
    }));
    assert!(!done.contains(&2), "the replaced job never runs");
    assert_eq!(most.load(Ordering::SeqCst), 1, "slot 1 ran two jobs at once");
}

/// A job that panics still frees its slot: the slot's next job runs (on another worker).
#[test]
fn a_panicking_job_frees_its_slot() {
    let mut p: JobPool<u8, u32> = JobPool::new(2);
    p.submit(1, 1, 9, Box::new(|| panic!("simulated render panic")));
    assert!(eventually(10, || !p.is_queued(1) && p.live_workers() == 1));
    p.submit(1, 2, 9, Box::new(|| 2));
    let mut got = None;
    assert!(eventually(10, || {
        got = got.or_else(|| p.try_recv().map(|d| d.result));
        got.is_some()
    }));
    assert_eq!(got, Some(2));
}

/// Deferred disk writes go through one writer with a byte budget: a burst of big renders can't queue more than the
/// budget (each used to start a thread of its own, holding its image; issue #1522), and what was queued reaches disk.
#[test]
fn deferred_writes_are_bounded_and_drain_to_disk() {
    let dir = temp_dir("deferred-budget");
    let cache = Arc::new(PreviewCache::with_disk(1 << 30, &dir, 1 << 31));
    let big = Arc::new(gradient(4096, 2048, 3)); // 32 MiB each
    let keys: Vec<Hash128> = (0..24u32).map(|i| hash_bytes(&i.to_le_bytes())).collect();
    for key in &keys {
        cache.put_deferred(*key, big.clone());
        assert!(cache.queued_bytes() <= MAX_QUEUED_BYTES, "{} bytes queued", cache.queued_bytes());
    }
    let t = std::time::Instant::now();
    while cache.queued_bytes() > 0 && t.elapsed() < std::time::Duration::from_secs(120) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(cache.queued_bytes(), 0, "the queue drains");
    assert!(cache.disk().unwrap().get(keys[0]).is_some(), "the first write reached disk");
    // every image is still in memory, written to disk or not
    assert!(keys.iter().all(|k| cache.get(*k).is_some()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The files in the cache's shard folders under `dir`.
fn cached_files(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |shards| shards.flatten().filter_map(|s| std::fs::read_dir(s.path()).ok()).map(|f| f.count()).sum())
}

/// A clear removed its disk files while holding the generation lock, which the app reads every frame: 100,000
/// thumbnails took 6.7 s, the whole time the UI waited (found in the review of #1328, whose source refreshes clear
/// like this branch's conversions, discards and look refreshes). The new generation now shows at once, while the
/// files go; meanwhile none of them is served, and once the clear returns none is left.
#[test]
fn clearing_a_large_disk_cache_never_holds_up_generation_reads() {
    let dir = temp_dir("clear-unlocked");
    let cache = Arc::new(PreviewCache::with_disk(1 << 20, &dir, 1 << 40));
    let old = cache.generation();
    let key = hash_bytes(b"an old thumbnail");
    cache.put_at(old, key, Arc::new(gradient(8, 8, 3)));
    // many more, named as the cache names them (a clear does not read them)
    for i in 0..20_000u64 {
        let hex = hash_bytes(&i.to_le_bytes()).to_string();
        let shard = dir.join(&hex[..2]);
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(shard.join(format!("{hex}.jpg")), b"x").unwrap();
    }
    let clearing = {
        let cache = cache.clone();
        std::thread::spawn(move || cache.clear())
    };
    while cache.generation() == old {
        std::thread::yield_now();
    }
    let left = cached_files(&dir);
    let served = cache.get(key).is_some();
    clearing.join().unwrap();
    assert!(left > 0, "the new generation could be read only once every file was gone");
    assert!(!served, "an old thumbnail was served while the files were being removed");
    assert_eq!(cached_files(&dir), 0);
    assert!(cache.get(key).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

type ClearHook = Arc<dyn Fn(&DiskCache) + Send + Sync>;
static DURING_CLEAR: std::sync::Mutex<Vec<(std::path::PathBuf, ClearHook)>> = std::sync::Mutex::new(Vec::new());

/// Called by [`DiskCache::clear`] after it lists the files and before it removes them.
pub(crate) fn during_clear(disk: &DiskCache) {
    let hook = DURING_CLEAR.lock().unwrap().iter().find(|(dir, _)| dir == disk.dir()).map(|(_, hook)| hook.clone());
    if let Some(hook) = hook {
        hook(disk);
    }
}

/// A clear lets current thumbnails be written while it removes the files. One written after its
/// listing outlives it, so the cache's size must count it: a total of zero would let the folder
/// grow past its budget until something scanned it again.
#[test]
fn a_thumbnail_written_while_a_clear_removes_files_keeps_its_size() {
    let dir = temp_dir("clear-accounting");
    let disk = DiskCache::new(&dir, 1 << 30);
    for i in 0..4u8 {
        disk.put(hash_bytes(&[i]), &gradient(32, 32, i));
    }
    assert!(disk.size() > 0);
    DURING_CLEAR.lock().unwrap().push((dir.clone(), Arc::new(|disk: &DiskCache| disk.put(hash_bytes(b"during the clear"), &gradient(32, 32, 9)))));
    disk.clear();
    let on_disk: u64 = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter_map(|shard| std::fs::read_dir(shard.path()).ok())
        .flat_map(|files| files.flatten())
        .map(|f| f.metadata().unwrap().len())
        .sum();
    assert!(on_disk > 0, "the thumbnail written during the clear outlives it");
    assert_eq!(disk.size(), on_disk, "the cache's size counts what is on disk");
    let _ = std::fs::remove_dir_all(&dir);
}
