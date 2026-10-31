//! The files a put-off upgrade keeps ([`Journal::keep_before_upgrade`]): one pair, the library as
//! it was at open, kept once however many attempts fail, and told once the one that works is
//! written. A child of `journal` because it sets the private `behind` (a library whose upgrade
//! was put off at open) by hand, so each failure of the attempts can be made at will. The store
//! here is a one-file store (it doesn't take the segments of format 11).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::store::MemStore;
use crate::{Photo, Source};

/// A store whose write of `catalog.snap` fails while `fail` is set (also through the background
/// writer) and, with `fail_log`, whose write of `catalog.log` does.
#[derive(Clone, Default)]
struct Failing {
    files: MemStore,
    fail: Arc<AtomicBool>,
    fail_log: Arc<AtomicBool>,
    /// A write of `catalog.snap` takes effect and is then said to have failed.
    fail_after: Arc<AtomicBool>,
    /// A write of `catalog.snap` panics: before it takes effect, or (`panic_after`) after.
    panic_before: Arc<AtomicBool>,
    panic_after: Arc<AtomicBool>,
    /// The next read of `catalog.snap` fails (a disk that stumbles).
    blind: Arc<AtomicBool>,
    /// How many files were written under a kept name (`….v<format>-<seq>`).
    kept_writes: Arc<std::sync::atomic::AtomicUsize>,
}

impl Store for Failing {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        if name == SNAPSHOT && self.blind.swap(false, Ordering::SeqCst) {
            return Err(std::io::Error::other("unreadable"));
        }
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if name.contains(".v") {
            self.kept_writes.fetch_add(1, Ordering::SeqCst);
        }
        if (name == SNAPSHOT && self.fail.load(Ordering::SeqCst)) || (name == LOG && self.fail_log.load(Ordering::SeqCst)) {
            return Err(std::io::Error::other("disk full"));
        }
        if name == SNAPSHOT && self.panic_before.load(Ordering::SeqCst) {
            panic!("the snapshot worker died");
        }
        self.files.write_atomic(name, data)?;
        if name == SNAPSHOT && self.panic_after.load(Ordering::SeqCst) {
            panic!("the snapshot worker died");
        }
        if name == SNAPSHOT && self.fail_after.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("folder not synced"));
        }
        Ok(())
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
        "failing".into()
    }
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        self.files.background.then(|| Box::new(self.clone()) as Box<dyn Store>)
    }
}

/// A library with a log, whose snapshot on disk states an older format (`from`), and a journal
/// that is behind it, as an open that put the upgrade off leaves it. The snapshot and the log at
/// that point are returned: they are what the files kept for the upgrade must hold.
fn put_off(background: bool) -> (Failing, Journal, Catalog, u32, (Vec<u8>, Vec<u8>)) {
    let from = VERSION - 1;
    let files = MemStore { background, ..MemStore::new() };
    let store = Failing { files: files.clone(), ..Default::default() };
    let (mut j, mut c, _) = Journal::open(Box::new(store.clone())).unwrap();
    add_photo(&mut j, &mut c);
    let snap = String::from_utf8(files.get(SNAPSHOT).unwrap()).unwrap();
    files.set(SNAPSHOT, snap.replacen(&format!("\"version\":{VERSION},"), &format!("\"version\":{from},"), 1).into_bytes());
    j.behind = Some(from);
    j.behind_at = (j.seq, j.log_bytes);
    store.fail.store(true, Ordering::SeqCst);
    let at_open = (files.get(SNAPSHOT).unwrap(), files.get(LOG).unwrap());
    (store, j, c, from, at_open)
}

fn add_photo(j: &mut Journal, c: &mut Catalog) {
    let id = c.alloc_photo_id();
    let photo = Photo::new(id, Source::File { path: format!("/pictures/IMG_{:06}.jpg", id.0) }, "IMG.jpg", "JPEG", 4032, 3024, "2026-10-10T12:00:00");
    let op = Op::AddPhoto { photo: Box::new(photo) };
    c.apply(op.clone()).unwrap();
    j.append(&[op]).unwrap();
}

/// The files kept for the upgrade (`….v<format>-<seq>`), by name.
fn kept(m: &MemStore) -> Vec<String> {
    m.files.lock().unwrap().keys().filter(|k| k.contains(".v")).cloned().collect()
}

/// The names the open would have kept the library under.
fn kept_names(from: u32, seq: u64) -> Vec<String> {
    let mut names = vec![format!("{LOG}.v{from}-{seq}"), format!("{SNAPSHOT}.v{from}-{seq}")];
    names.sort();
    names
}

fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names
}

fn attempt(j: &mut Journal, c: &Catalog, background: bool) -> Result<()> {
    if background { j.snapshot_in_background(c).and_then(|()| j.wait()) } else { j.snapshot(c) }
}

/// Given a library whose upgrade was put off and a disk that can't take the snapshot, when
/// compaction is tried again and again with changes in between, then one pair of files is kept,
/// as the open would have named it, and written once: tries that fail don't pile copies up on a
/// disk that is already full, and none of them is told as written.
#[test]
fn failed_attempts_keep_one_pair() {
    for background in [false, true] {
        let (store, mut j, mut c, from, at_open) = put_off(background);
        let names = kept_names(from, j.behind_at.0);
        for _ in 0..3 {
            add_photo(&mut j, &mut c);
            assert!(attempt(&mut j, &c, background).is_err(), "background: {background}");
            assert_eq!(kept(&store.files), names, "background: {background}");
            assert!(j.take_kept().is_empty(), "nothing is written yet to tell");
        }
        assert_eq!(store.kept_writes.load(Ordering::SeqCst), 2, "the snapshot and the log, once");
        assert_eq!((store.files.get(&names[1]), j.behind), (Some(at_open.0), Some(from)), "background: {background}");
    }
}

/// Given a library put off and changed since it was opened, when a later attempt works, then the
/// files kept are the library as it was at open (not the log as it is now), it is upgraded, and
/// the files are told once.
#[test]
fn a_later_attempt_upgrades_and_tells_what_it_kept() {
    for background in [false, true] {
        let (store, mut j, mut c, from, at_open) = put_off(background);
        let names = kept_names(from, j.behind_at.0);
        add_photo(&mut j, &mut c);
        assert!(attempt(&mut j, &c, background).is_err());
        assert!(j.take_kept().is_empty());
        store.fail.store(false, Ordering::SeqCst);
        add_photo(&mut j, &mut c);
        assert!(store.files.get(LOG).unwrap().len() > at_open.1.len(), "the log has grown");
        attempt(&mut j, &c, background).unwrap();
        assert_eq!(kept(&store.files), names, "background: {background}");
        assert_eq!((store.files.get(&names[1]), store.files.get(&names[0])), (Some(at_open.0), Some(at_open.1)), "as it was at open");
        assert_eq!(sorted(j.take_kept()), names);
        assert!(j.take_kept().is_empty(), "once");
        assert_eq!((j.behind, snapshot_version_on(&store.files)), (None, u64::from(VERSION)), "background: {background}");
    }
}

fn snapshot_version_on(m: &MemStore) -> u64 {
    serde_json::from_slice::<serde_json::Value>(&m.get(SNAPSHOT).unwrap()).unwrap()["version"].as_u64().unwrap()
}

/// Given an attempt whose snapshot takes effect but is reported as failed (the folder could not
/// be synced) after its files were kept, when it is tried again, then it works, and the files told
/// are the old library, not the snapshot that landed. Also where the attempt was a background one
/// that nobody waited for (the journal is dropped): the kept files stay.
#[test]
fn an_attempt_that_took_effect_all_the_same_is_tried_again() {
    for how in ["sync", "background", "dropped"] {
        let (store, mut j, mut c, from, at_open) = put_off(how != "sync");
        store.fail.store(false, Ordering::SeqCst);
        store.fail_after.store(true, Ordering::SeqCst);
        add_photo(&mut j, &mut c);
        let names = kept_names(from, j.behind_at.0);
        match how {
            "sync" => assert!(j.snapshot(&c).is_err(), "{how}"),
            "background" => assert!(j.snapshot_in_background(&c).and_then(|()| j.wait()).is_err(), "{how}"),
            _ => j.snapshot_in_background(&c).unwrap(),
        }
        if how == "dropped" {
            let files = store.files.clone();
            drop(j);
            assert_eq!((kept(&files), files.get(&names[1])), (names, Some(at_open.0)), "{how}");
            continue;
        }
        assert_eq!(snapshot_version_on(&store.files), u64::from(VERSION), "{how}: it did land");
        store.fail_after.store(false, Ordering::SeqCst);
        j.snapshot(&c).unwrap();
        assert_eq!(sorted(j.take_kept()), names, "{how}");
        assert_eq!((store.files.get(&names[1]), j.behind), (Some(at_open.0), None), "{how}: the old library, not the snapshot that landed");
        assert_eq!(store.kept_writes.load(Ordering::SeqCst), 2, "{how}: not written again");
    }
}

/// Given a library put off whose snapshot is in this build's format already (a write that landed
/// and reported a failure before anything was kept), when snapshots are written, then each of
/// them works: the upgrade is done, with nothing to keep or tell.
#[test]
fn a_snapshot_that_landed_is_the_upgrade_done() {
    let (store, mut j, c, from, _) = put_off(false);
    store.fail.store(false, Ordering::SeqCst);
    let snap = String::from_utf8(store.files.get(SNAPSHOT).unwrap()).unwrap();
    store.files.set(SNAPSHOT, snap.replacen(&format!("\"version\":{from},"), &format!("\"version\":{VERSION},"), 1).into_bytes());
    j.snapshot(&c).unwrap();
    j.snapshot(&c).unwrap();
    assert_eq!((kept(&store.files), j.take_kept(), j.behind), (Vec::<String>::new(), Vec::<String>::new(), None));
}

/// Given a library put off whose snapshot is in some other format than the one it was put off
/// in or this build's, when a snapshot is written, then it is refused and nothing is kept: what
/// the files would keep is not the library it was.
#[test]
fn a_snapshot_in_another_format_is_not_kept() {
    let (store, mut j, c, from, _) = put_off(false);
    store.fail.store(false, Ordering::SeqCst);
    let snap = String::from_utf8(store.files.get(SNAPSHOT).unwrap()).unwrap();
    let other = snap.replacen(&format!("\"version\":{from},"), "\"version\":3,", 1);
    store.files.set(SNAPSHOT, other.clone().into_bytes());
    assert!(j.snapshot(&c).is_err());
    assert_eq!((kept(&store.files), store.files.get(SNAPSHOT), j.behind), (Vec::<String>::new(), Some(other.into_bytes()), Some(from)));
}

/// Given an attempt whose worker dies (a panic) before its snapshot takes effect, when the
/// journal is waited for and the next attempt works, then the files kept for the first one are
/// the ones told: they are not written again.
#[test]
fn a_worker_that_dies_keeps_the_files_for_the_next_attempt() {
    let (store, mut j, mut c, from, at_open) = put_off(true);
    let names = kept_names(from, j.behind_at.0);
    store.fail.store(false, Ordering::SeqCst);
    store.panic_before.store(true, Ordering::SeqCst);
    add_photo(&mut j, &mut c);
    j.snapshot_in_background(&c).unwrap();
    assert!(j.wait().is_err());
    assert_eq!((kept(&store.files), j.behind), (names.clone(), Some(from)));
    store.panic_before.store(false, Ordering::SeqCst);
    j.snapshot(&c).unwrap();
    assert_eq!((sorted(j.take_kept()), store.files.get(&names[1]), j.behind), (names, Some(at_open.0), None));
    assert_eq!(store.kept_writes.load(Ordering::SeqCst), 2, "written once");
}

/// Given a store that has no background writer (the compaction is written synchronously), when a
/// background compaction is asked for, then the old files are kept once, not once by the request
/// and again by the synchronous write it falls back to.
#[test]
fn a_background_request_without_a_writer_keeps_the_files_once() {
    let (store, mut j, mut c, _, _) = put_off(false);
    store.fail.store(false, Ordering::SeqCst);
    add_photo(&mut j, &mut c);
    j.snapshot_in_background(&c).unwrap();
    assert_eq!(store.kept_writes.load(Ordering::SeqCst), 2, "the snapshot and the log, once");
}

/// Given a put-off library whose snapshot is written but whose log then can't be emptied, when
/// that fails, then the upgrade is done all the same: the library is no longer behind, the files
/// kept stay and are told, and nothing keeps the new format over them.
#[test]
fn an_upgrade_whose_log_cannot_be_emptied_is_done_and_tells() {
    let (store, mut j, mut c, from, at_open) = put_off(false);
    let names = kept_names(from, j.behind_at.0);
    store.fail.store(false, Ordering::SeqCst);
    store.fail_log.store(true, Ordering::SeqCst);
    add_photo(&mut j, &mut c);
    assert!(j.snapshot(&c).is_err(), "the log could not be emptied");
    assert_eq!((snapshot_version_on(&store.files), j.behind), (u64::from(VERSION), None));
    assert_eq!((sorted(j.take_kept()), kept(&store.files), store.files.get(&names[1])), (names.clone(), names.clone(), Some(at_open.0)));
    store.fail_log.store(false, Ordering::SeqCst);
    j.snapshot(&c).unwrap();
    assert_eq!((kept(&store.files), store.kept_writes.load(Ordering::SeqCst)), (names, 2), "nothing kept again");
}

/// Given a worker that dies after its snapshot took effect, when the next snapshot comes, then
/// it works, and the files told are the old library, written once.
#[test]
fn a_worker_that_dies_after_its_commit_leaves_the_way_back_as_it_was() {
    let (store, mut j, mut c, from, at_open) = put_off(true);
    let names = kept_names(from, j.behind_at.0);
    store.fail.store(false, Ordering::SeqCst);
    store.panic_after.store(true, Ordering::SeqCst);
    add_photo(&mut j, &mut c);
    j.snapshot_in_background(&c).unwrap();
    assert!(j.wait().is_err());
    store.panic_after.store(false, Ordering::SeqCst);
    j.snapshot(&c).unwrap();
    assert_eq!((sorted(j.take_kept()), store.files.get(&names[1]), j.behind), (names, Some(at_open.0), None));
    assert_eq!(store.kept_writes.load(Ordering::SeqCst), 2, "written once");
}

/// Given a put-off library whose snapshot can't be read when the files are to be kept (a disk that
/// stumbles), when a snapshot is written, then it is refused and nothing is kept or changed; the
/// next one keeps the files and upgrades.
#[test]
fn a_disk_that_cannot_be_read_keeps_nothing_and_is_tried_again() {
    let (store, mut j, c, from, at_open) = put_off(false);
    let names = kept_names(from, j.behind_at.0);
    store.fail.store(false, Ordering::SeqCst);
    store.blind.store(true, Ordering::SeqCst);
    assert!(j.snapshot(&c).is_err(), "the read that fails");
    assert_eq!((kept(&store.files), store.files.get(SNAPSHOT), j.behind), (Vec::<String>::new(), Some(at_open.0.clone()), Some(from)));
    j.snapshot(&c).unwrap();
    assert_eq!((sorted(j.take_kept()), store.files.get(&names[1]), j.behind), (names, Some(at_open.0), None));
}
