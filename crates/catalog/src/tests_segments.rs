//! The snapshot in segments (catalog format 11): photos are saved in segment files listed by
//! `catalog.snap`, and a compaction rewrites only the segments whose photos changed.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::journal::{LOG, SEGMENT_PREFIX, SNAPSHOT, VERSION, segment_file};
use crate::*;

fn photo(id: u64) -> Photo {
    Photo::new(
        PhotoId(id),
        Source::File { path: format!("/pictures/2019/trip/IMG_{id:06}.jpg") },
        &format!("IMG_{id:06}.jpg"),
        "JPEG",
        4032,
        3024,
        "2026-10-10T12:00:00",
    )
}

/// Apply and log.
fn run(j: &mut Journal, c: &mut Catalog, ops: Vec<Op>) {
    for op in &ops {
        c.apply(op.clone()).unwrap();
    }
    j.append(&ops).unwrap();
}

fn add(ids: impl IntoIterator<Item = u64>) -> Vec<Op> {
    ids.into_iter().map(|id| Op::AddPhoto { photo: Box::new(photo(id)) }).collect()
}

/// A library of photos `1..=n` in segments of `per` photos, saved and compacted.
fn library(n: u64, per: usize) -> (MemStore, Journal, Catalog) {
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    j.policy.segment_photos = per;
    run(&mut j, &mut c, add(1..=n));
    let album = c.alloc_album_id();
    run(&mut j, &mut c, vec![Op::AddAlbum { album: Album::new(album, "Keepers") }]);
    j.snapshot(&c).unwrap();
    (m, j, c)
}

fn segment_files(m: &MemStore) -> BTreeSet<String> {
    // (also what is left of one being written: `<name>.tmp`)
    m.files.lock().unwrap().keys().filter(|k| k.starts_with(SEGMENT_PREFIX) && (k.ends_with(".json") || k.ends_with(".json.tmp"))).cloned().collect()
}

fn manifest(m: &MemStore) -> Value {
    serde_json::from_slice(&m.get(SNAPSHOT).unwrap()).unwrap()
}

fn listed(m: &MemStore) -> BTreeSet<String> {
    manifest(m)["segments"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect()
}

/// The catalog as opening the files gives it, compared as public JSON too.
fn reopen(m: &MemStore) -> Catalog {
    let (_, c, _) = Journal::open(Box::new(m.clone())).unwrap();
    c
}

fn assert_same(a: &Catalog, b: &Catalog) {
    assert_eq!(a.to_snapshot(), b.to_snapshot());
    let public = |c: &Catalog| c.photos().map(|p| serde_json::to_string(p.as_ref()).unwrap()).collect::<Vec<_>>();
    assert_eq!(public(a), public(b));
}

/// A copy of the files as a crash at this moment would leave them.
fn crash_copy(m: &MemStore) -> MemStore {
    let copy = MemStore::new();
    *copy.files.lock().unwrap() = m.files.lock().unwrap().clone();
    copy
}

/// Given a library, when it is saved, then its photos are in segment files that `catalog.snap`
/// lists (and holds none itself), and opening it gives the same catalog.
#[test]
fn photos_are_saved_in_segments_listed_by_the_snapshot() {
    let (m, j, c) = library(10, 4);
    let head = manifest(&m);
    assert_eq!(head["version"].as_u64(), Some(u64::from(VERSION)));
    assert_eq!(head["catalog"]["photos"], serde_json::json!({}));
    assert_eq!(head["catalog"]["albums"].as_object().unwrap().len(), 1);
    let segments = head["segments"].as_array().unwrap();
    assert_eq!(segments.iter().map(|s| s["photos"].as_u64().unwrap()).collect::<Vec<_>>(), [4, 4, 2]);
    assert_eq!(segments.iter().map(|s| s["first"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 5, 9]);
    assert_eq!(listed(&m), segment_files(&m));
    for s in segments {
        let bytes = m.get(s["name"].as_str().unwrap()).unwrap();
        assert_eq!(bytes.len() as u64, s["bytes"].as_u64().unwrap());
        assert_eq!(u64::from(crc32fast::hash(&bytes)), s["crc"].as_u64().unwrap());
        // a segment is JSON a person can read: its photos by id
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["photos"].as_object().unwrap().len() as u64, s["photos"].as_u64().unwrap());
    }
    assert_eq!(j.snapshot_bytes(), m.get(SNAPSHOT).unwrap().len() as u64 + segments.iter().map(|s| s["bytes"].as_u64().unwrap()).sum::<u64>());
    assert_same(&reopen(&m), &c);
}

/// Given a saved library, when one photo changes and the log is compacted, then only that
/// photo's segment is rewritten: the other segment files are the same files.
#[test]
fn a_compaction_rewrites_only_the_segments_that_changed() {
    let (m, mut j, mut c) = library(12, 4);
    let before = listed(&m);
    let files_before = m.files.lock().unwrap().clone();
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(6), rating: 5 }]);
    j.snapshot(&c).unwrap();
    let after = listed(&m);
    assert_eq!(before.intersection(&after).count(), 2, "{before:?} -> {after:?}");
    for name in before.intersection(&after) {
        assert_eq!(m.get(name), files_before.get(name).cloned());
    }
    assert_eq!(j.stats().last_snapshot.segments, 1);
    // the replaced segment's old file is gone
    assert_eq!(segment_files(&m), after);
    assert_same(&reopen(&m), &c);
}

/// Given a saved library, when nothing about its photos changed (an album was renamed), then
/// a compaction writes no segment at all.
#[test]
fn a_change_that_touches_no_photo_rewrites_no_segment() {
    let (m, mut j, mut c) = library(12, 4);
    let before = listed(&m);
    let album = c.albums().next().unwrap().id;
    run(&mut j, &mut c, vec![Op::RenameAlbum { id: album, name: "Best".into() }]);
    j.snapshot(&c).unwrap();
    assert_eq!(listed(&m), before);
    assert_eq!(j.stats().last_snapshot.segments, 0);
    assert_same(&reopen(&m), &c);
}

/// Given a saved library, when photos are imported, then they go into new segments at the end
/// and the segments of the photos that were there are left alone.
#[test]
fn an_import_adds_segments_and_leaves_the_others() {
    let (m, mut j, mut c) = library(8, 4);
    let before = listed(&m);
    run(&mut j, &mut c, add(9..=20));
    j.snapshot(&c).unwrap();
    let after = listed(&m);
    assert!(before.is_subset(&after), "{before:?} -> {after:?}");
    let counts: Vec<u64> = manifest(&m)["segments"].as_array().unwrap().iter().map(|s| s["photos"].as_u64().unwrap()).collect();
    assert_eq!(counts.iter().sum::<u64>(), 20);
    assert!(counts.iter().all(|n| *n <= 8), "{counts:?}");
    assert_same(&reopen(&m), &c);
}

/// A segment that grew past twice its size is split; one that lost all its photos is dropped.
#[test]
fn segments_split_when_they_grow_and_go_when_they_empty() {
    let (m, mut j, mut c) = library(4, 4);
    // ids between the existing ones' segment boundaries can't happen with counted ids, but a
    // segment also grows when it is the last one: 4 photos -> 13
    run(&mut j, &mut c, add(5..=13));
    j.snapshot(&c).unwrap();
    let counts: Vec<u64> = manifest(&m)["segments"].as_array().unwrap().iter().map(|s| s["photos"].as_u64().unwrap()).collect();
    assert_eq!(counts, [4, 4, 4, 1]);
    assert_same(&reopen(&m), &c);

    run(&mut j, &mut c, (5..=8).map(|id| Op::RemovePhoto { id: PhotoId(id) }).collect());
    j.snapshot(&c).unwrap();
    let firsts: Vec<u64> = manifest(&m)["segments"].as_array().unwrap().iter().map(|s| s["first"].as_u64().unwrap()).collect();
    assert_eq!(firsts, [1, 9, 13]);
    assert_eq!(segment_files(&m), listed(&m));
    let back = reopen(&m);
    assert_same(&back, &c);
    assert!(back.photo(PhotoId(6)).is_none());

    // everything removed: no segments, and the library still opens
    run(&mut j, &mut c, [1, 2, 3, 4, 9, 10, 11, 12, 13].map(|id| Op::RemovePhoto { id: PhotoId(id) }).to_vec());
    j.snapshot(&c).unwrap();
    assert!(segment_files(&m).is_empty() && listed(&m).is_empty());
    assert_eq!(reopen(&m).len(), 0);
}

/// Photo ids far apart (random ids) are grouped by how many there are, not by their value.
#[test]
fn widely_spaced_ids_fill_segments_like_any_others() {
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    j.policy.segment_photos = 4;
    let ids: Vec<u64> = (1..=10u64).map(|k| k.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 11).collect();
    run(&mut j, &mut c, add(ids.clone()));
    j.snapshot(&c).unwrap();
    assert_eq!(listed(&m).len(), 3);
    // one more, in the middle of the id range: its segment only
    let before = listed(&m);
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    run(&mut j, &mut c, add([sorted[5] + 1]));
    j.snapshot(&c).unwrap();
    assert_eq!(before.intersection(&listed(&m)).count(), 2);
    assert_same(&reopen(&m), &c);
}

/// A store whose every change is preceded by a copy of its files: what a crash just before
/// that change would leave.
#[derive(Clone, Default)]
struct Filmed {
    files: MemStore,
    frames: Arc<Mutex<Vec<MemStore>>>,
    filming: Arc<Mutex<bool>>,
}

impl Filmed {
    fn frame(&self) {
        if *self.filming.lock().unwrap() {
            self.frames.lock().unwrap().push(crash_copy(&self.files));
        }
    }
}

impl Store for Filmed {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.frame();
        self.files.write_atomic(name, data)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.frame();
        self.files.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.frame();
        self.files.truncate(name, len)
    }
    fn remove(&mut self, name: &str) -> std::io::Result<()> {
        self.frame();
        self.files.remove(name)
    }
    fn list(&mut self) -> std::io::Result<Vec<String>> {
        self.files.list()
    }
    fn segmented(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "filmed".into()
    }
}

/// Given a compaction that rewrites, adds and drops segments, when the program crashes before
/// any one of its writes, then opening the library gives every change made so far, and leaves
/// no segment file behind that the snapshot doesn't list.
#[test]
fn a_crash_at_any_point_of_a_compaction_loses_nothing() {
    let store = Filmed::default();
    let (mut j, mut c, _) = Journal::open(Box::new(store.clone())).unwrap();
    j.policy.segment_photos = 4;
    run(&mut j, &mut c, add(1..=12));
    j.snapshot(&c).unwrap();
    run(
        &mut j,
        &mut c,
        vec![
            Op::SetRating { id: PhotoId(2), rating: 3 },
            Op::RemovePhoto { id: PhotoId(5) },
            Op::RemovePhoto { id: PhotoId(6) },
            Op::RemovePhoto { id: PhotoId(7) },
            Op::RemovePhoto { id: PhotoId(8) },
        ],
    );
    run(&mut j, &mut c, add(13..=24));
    let expected = c.clone();

    *store.filming.lock().unwrap() = true;
    j.snapshot(&c).unwrap();
    *store.filming.lock().unwrap() = false;
    let mut frames = std::mem::take(&mut *store.frames.lock().unwrap());
    frames.push(crash_copy(&store.files));
    assert!(frames.len() >= 6, "{} writes", frames.len());
    for (k, frame) in frames.iter().enumerate() {
        let (mut j2, back, report) = Journal::open(Box::new(frame.clone())).unwrap();
        assert_eq!((report.failed, report.damaged.clone()), (0, None), "crash before write {k}");
        assert_eq!(back.to_snapshot(), expected.to_snapshot(), "crash before write {k}");
        j2.remove_unlisted_segments();
        assert_eq!(segment_files(frame), listed(frame), "crash before write {k}: a segment file nothing lists");
        // and the library goes on from there
        drop(j2);
        let (mut j3, mut c3, _) = Journal::open(Box::new(frame.clone())).unwrap();
        run(&mut j3, &mut c3, vec![Op::SetRating { id: PhotoId(1), rating: 1 }]);
        j3.snapshot(&c3).unwrap();
        assert_eq!(reopen(frame).to_snapshot(), c3.to_snapshot(), "crash before write {k}");
    }
}

/// The same while the snapshot is written by the worker thread and changes go on.
#[test]
fn a_background_compaction_writes_segments_and_keeps_later_changes() {
    let m = MemStore { background: true, ..Default::default() };
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    j.policy.segment_photos = 4;
    run(&mut j, &mut c, add(1..=12));
    j.snapshot(&c).unwrap();
    let before = listed(&m);
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(10), rating: 4 }]);
    j.snapshot_in_background(&c).unwrap();
    // changes while it is being written: to a segment it rewrites, and to one it doesn't
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(11), rating: 2 }, Op::SetRating { id: PhotoId(1), rating: 2 }]);
    assert_same(&reopen(&crash_copy(&m)), &c);
    j.wait().unwrap();
    assert_eq!(before.intersection(&listed(&m)).count(), 2);
    assert_same(&reopen(&m), &c);
    // the next compaction rewrites the segments of what changed meanwhile
    j.snapshot(&c).unwrap();
    assert_eq!(j.stats().last_snapshot.segments, 2);
    assert_eq!(segment_files(&m), listed(&m));
    assert_same(&reopen(&m), &c);
}

/// Segments are read on several threads when the store allows it; the result is the same.
#[test]
fn segments_load_the_same_in_parallel() {
    let (m, _j, c) = library(200, 8);
    assert!(listed(&m).len() >= 20);
    let parallel = MemStore { background: true, ..crash_copy(&m) };
    assert_same(&reopen(&parallel), &c);
    assert_same(&reopen(&m), &c);
}

/// Given a segment file that is damaged, cut short or gone, when the library is opened, then
/// it says which file, and changes nothing.
#[test]
fn a_damaged_segment_is_reported_and_nothing_is_changed() {
    let (m, mut j, mut c) = library(12, 4);
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(1), rating: 1 }]);
    drop(j);
    let name = listed(&m).into_iter().nth(1).unwrap();
    let damage: [(&str, fn(Vec<u8>) -> Option<Vec<u8>>); 5] = [
        // still JSON, the same length and the same photos: only the checksum can tell
        ("one digit changed", |mut b| {
            let at = b.windows(4).position(|w| w == b"4032").unwrap();
            b[at] = b'5';
            Some(b)
        }),
        ("flipped", |mut b| {
            let at = b.len() / 2;
            b[at] ^= 0x20;
            Some(b)
        }),
        ("cut short", |mut b| {
            b.truncate(b.len() / 2);
            Some(b)
        }),
        ("empty", |_| Some(Vec::new())),
        ("gone", |_| None),
    ];
    for (what, f) in damage {
        let bad = crash_copy(&m);
        match f(bad.get(&name).unwrap()) {
            Some(b) => bad.set(&name, b),
            None => {
                bad.files.lock().unwrap().remove(&name);
            }
        }
        let files = bad.files.lock().unwrap().clone();
        let e = Journal::open(Box::new(bad.clone())).map(|_| ()).unwrap_err();
        assert!(matches!(&e, CatalogError::Corrupt(msg) if msg.contains(&name)), "{what}: {e}");
        assert_eq!(*bad.files.lock().unwrap(), files, "{what}: files were changed");
    }
}

/// A snapshot that lists segments it shouldn't (a name outside the library, the same file
/// twice, photos outside a segment's range, a wrong count) is refused, not followed.
#[test]
fn a_snapshot_with_a_bad_segment_list_is_refused() {
    let (m, j, _c) = library(12, 4);
    drop(j);
    let edits: [(&str, fn(&mut Value)); 6] = [
        ("a path", |v| v["segments"][0]["name"] = "../catalog.seg-1-1".into()),
        ("another file", |v| v["segments"][0]["name"] = "prefs.json".into()),
        ("twice", |v| v["segments"][1] = v["segments"][0].clone()),
        ("out of order", |v| v["segments"].as_array_mut().unwrap().swap(0, 2)),
        ("wrong range", |v| v["segments"][1]["first"] = 6.into()),
        ("wrong count", |v| v["segments"][0]["photos"] = 3.into()),
    ];
    for (what, edit) in edits {
        let bad = crash_copy(&m);
        let mut v = manifest(&bad);
        edit(&mut v);
        bad.set(SNAPSHOT, serde_json::to_vec(&v).unwrap());
        let files = bad.files.lock().unwrap().clone();
        let e = Journal::open(Box::new(bad.clone())).map(|_| ()).unwrap_err();
        assert!(matches!(e, CatalogError::Corrupt(_)), "{what}: {e}");
        assert_eq!(*bad.files.lock().unwrap(), files, "{what}: files were changed");
    }
    // absurd numbers are an error too, not an allocation
    let bad = crash_copy(&m);
    let mut v = manifest(&bad);
    v["segments"][0]["photos"] = u64::MAX.into();
    v["segments"][0]["bytes"] = u64::MAX.into();
    bad.set(SNAPSHOT, serde_json::to_vec(&v).unwrap());
    assert!(Journal::open(Box::new(bad)).is_err());
}

/// Segment files that the snapshot doesn't list (left by a crash) are removed when the opener,
/// holding the library to itself, asks for it; opening alone removes nothing (to a second
/// program, the segments the first is writing for its next snapshot look the same). Other files
/// in the folder are never touched.
#[test]
fn unlisted_segment_files_are_removed_on_request_only() {
    let (m, j, c) = library(8, 4);
    drop(j);
    m.set(&segment_file("99-99"), b"left by a crash".to_vec());
    m.set(&format!("{}.tmp", segment_file("99-100")), b"half written".to_vec());
    m.set("presets.json", b"[]".to_vec());
    m.set("segments/notes.txt", b"mine".to_vec());
    let (mut j, back, _) = Journal::open(Box::new(m.clone())).unwrap();
    assert_same(&back, &c);
    assert_eq!(segment_files(&m).len(), listed(&m).len() + 2, "opening removed files");
    j.remove_unlisted_segments();
    assert_eq!(segment_files(&m), listed(&m));
    assert_eq!(m.get("presets.json").as_deref(), Some(&b"[]"[..]));
    assert_eq!(m.get("segments/notes.txt").as_deref(), Some(&b"mine"[..]));
}

/// Given segment files without the snapshot that lists them (a folder copied or synced in part,
/// a `catalog.snap` that was removed), when the folder is opened, then it is refused and nothing
/// is touched: the photos are in those files, and a new empty library must not replace them.
#[test]
fn segments_without_their_snapshot_are_not_a_new_library() {
    let (m, j, _c) = library(12, 4);
    drop(j);
    for with_log in [true, false] {
        let lost = crash_copy(&m);
        lost.files.lock().unwrap().remove(SNAPSHOT);
        if !with_log {
            lost.files.lock().unwrap().remove(LOG);
        }
        let files = lost.files.lock().unwrap().clone();
        let e = Journal::open(Box::new(lost.clone())).map(|_| ()).unwrap_err();
        assert!(matches!(&e, CatalogError::Corrupt(msg) if msg.contains(SNAPSHOT) && msg.contains("3 segment")), "{e}");
        assert_eq!(*lost.files.lock().unwrap(), files);
    }
}

/// Given a library that so far is only a log (it never had a snapshot), when its first snapshot
/// gets as far as writing segments and no further (the write of `catalog.snap` fails, or the
/// program stops there), then nothing of it is lost and it opens once the snapshot can be
/// written: the log holds every change from the first, so the segment files are not "photos
/// without their snapshot".
#[test]
fn a_log_only_library_survives_a_first_snapshot_that_did_not_finish() {
    let ops = add(1..=5);
    let log: String = ops.iter().enumerate().map(|(k, op)| crate::journal::encode_record(k as u64 + 1, op) + "\n").collect();
    let mut expected = Catalog::new();
    for op in ops {
        expected.apply(op).unwrap();
    }
    // the write of `catalog.snap` fails: the library is not worked in as it is (a log states no
    // defaults for what would be added to it), and is left as it was, with no segment
    let store = Watched::default();
    store.files.set(LOG, log.clone().into_bytes());
    *store.fail.lock().unwrap() = Some(SNAPSHOT.into());
    let e = Journal::open(Box::new(store.clone())).err().expect("refused");
    assert!(matches!(&e, CatalogError::Io(why) if why.contains("can't be upgraded")), "{e}");
    assert!(segment_files(&store.files).is_empty() && store.files.get(SNAPSHOT).is_none());
    assert_eq!(store.files.get(LOG), Some(log.clone().into_bytes()));
    *store.fail.lock().unwrap() = None;
    let (_, c, r) = Journal::open(Box::new(store.clone())).unwrap();
    assert_eq!((c.to_snapshot(), r.replayed, r.upgraded_from), (expected.to_snapshot(), 5, Some(0)));

    // the program stopped between the segments and `catalog.snap`: the log and a segment file
    let m = MemStore::new();
    m.set(LOG, log.into_bytes());
    m.set(&segment_file("1-5"), b"written, never listed".to_vec());
    let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!((c.to_snapshot(), r.upgraded_from), (expected.to_snapshot(), Some(0)));
    assert!(!listed(&m).contains(&segment_file("1-5")), "written over a file that was there");
    assert_same(&reopen(&m), &expected);

    // a log that doesn't start at the first change is no such library: refused
    let (m, mut j, mut c) = library(12, 4);
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(1), rating: 1 }]);
    drop(j);
    m.files.lock().unwrap().remove(SNAPSHOT);
    assert!(matches!(Journal::open(Box::new(m)).map(|_| ()), Err(CatalogError::Corrupt(_))));
}

/// Given a snapshot whose `catalog.snap` was replaced although the store reported an error (so
/// the journal still takes the old segments to be the listed ones), when unlisted segments are
/// removed, then the ones the file on disk lists stay, and the library opens.
#[test]
fn cleaning_up_goes_by_the_snapshot_on_disk() {
    let (m, j, _c) = library(12, 4);
    drop(j);
    let store = Lying { files: m.clone(), lie: Arc::new(Mutex::new(false)) };
    let (mut j, mut c, _) = Journal::open(Box::new(store.clone())).unwrap();
    j.policy.segment_photos = 4;
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(3), rating: 5 }]);
    *store.lie.lock().unwrap() = true;
    assert!(j.snapshot(&c).is_err());
    *store.lie.lock().unwrap() = false;
    assert_same(&reopen(&crash_copy(&m)), &c);
    j.remove_unlisted_segments();
    assert_same(&reopen(&crash_copy(&m)), &c);
    j.snapshot(&c).unwrap();
    assert_same(&reopen(&m), &c);
}

/// A store whose replace of `catalog.snap` takes effect and then reports an error.
#[derive(Clone)]
struct Lying {
    files: MemStore,
    lie: Arc<Mutex<bool>>,
}

impl Store for Lying {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.files.write_atomic(name, data)?;
        if name == SNAPSHOT && *self.lie.lock().unwrap() {
            return Err(std::io::Error::other("the connection dropped"));
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
    fn list(&mut self) -> std::io::Result<Vec<String>> {
        self.files.list()
    }
    fn segmented(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "lying".into()
    }
}

/// Given an older one-file `catalog.snap` put back over a folder that has segments (a restored
/// backup), when it is opened, then the one file is what opens, and the segment files, which may
/// be all there is of the newer catalog, are left alone in that session: also when the opener
/// asks for unlisted ones to be removed, and though the one file is split into segments of its
/// own. (They go when the library, by then in segments, is next opened and cleaned.)
#[test]
fn segments_beside_a_one_file_snapshot_are_left_alone() {
    let (m, j, _c) = library(12, 4);
    drop(j);
    let newer = segment_files(&m);
    let newer_bytes: Vec<Vec<u8>> = newer.iter().map(|n| m.get(n).unwrap()).collect();
    let old = include_str!("fixtures/v10-catalog.snap.json");
    m.set(SNAPSHOT, old.as_bytes().to_vec());
    m.set(LOG, Vec::new());
    let (mut j, back, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!((back.len(), r.upgraded_from), (3, Some(10)));
    j.remove_unlisted_segments();
    for (name, bytes) in newer.iter().zip(&newer_bytes) {
        assert_eq!(m.get(name).as_ref(), Some(bytes), "{name}");
    }
    // the split wrote segments of its own, under names none of those files has
    assert!(listed(&m).is_disjoint(&newer));
    drop(j);
    let (mut j, back, _) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(back.len(), 3);
    j.remove_unlisted_segments();
    assert_eq!(segment_files(&m), listed(&m));
}

/// A snapshot whose list of segments doesn't add up to the photos it states (an entry removed)
/// is refused: opening it would be a library that silently lost photos.
#[test]
fn a_segment_list_that_does_not_hold_every_photo_is_refused() {
    let (m, j, _c) = library(12, 4);
    drop(j);
    assert_eq!(manifest(&m)["photos"].as_u64(), Some(12));
    let mut v = manifest(&m);
    v["segments"].as_array_mut().unwrap().pop();
    m.set(SNAPSHOT, serde_json::to_vec(&v).unwrap());
    let files = m.files.lock().unwrap().clone();
    let e = Journal::open(Box::new(m.clone())).map(|_| ()).unwrap_err();
    assert!(matches!(&e, CatalogError::Corrupt(msg) if msg.contains("8 photos") && msg.contains("12")), "{e}");
    assert_eq!(*m.files.lock().unwrap(), files);

    // and a snapshot that holds photos itself and lists segments too
    let (m, j, _c) = library(12, 4);
    drop(j);
    let mut v = manifest(&m);
    v["catalog"]["photos"] = serde_json::json!({"99": serde_json::to_value(photo(99)).unwrap()});
    v["photos"] = 13.into();
    m.set(SNAPSHOT, serde_json::to_vec(&v).unwrap());
    assert!(matches!(Journal::open(Box::new(m)).map(|_| ()), Err(CatalogError::Corrupt(_))));
}

/// Given a library that was closed and opened again, when one photo changes and the log is
/// compacted, then only that photo's segment is rewritten: what is on disk is known from
/// opening it, not only from having written it.
#[test]
fn a_compaction_after_reopening_rewrites_only_what_changed() {
    let (m, j, _c) = library(12, 4);
    drop(j);
    let before = listed(&m);
    let files_before = m.files.lock().unwrap().clone();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    j.policy.segment_photos = 4;
    assert_eq!(j.snapshot_bytes(), m.get(SNAPSHOT).unwrap().len() as u64 + j.segments().iter().map(|s| s.bytes).sum::<u64>());
    assert_eq!(j.stats().segments, 3);
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(10), rating: 5 }]);
    j.snapshot(&c).unwrap();
    assert_eq!(j.stats().last_snapshot.segments, 1);
    let after = listed(&m);
    assert_eq!(before.intersection(&after).count(), 2);
    for name in before.intersection(&after) {
        assert_eq!(m.get(name), files_before.get(name).cloned());
    }
    let rewritten = after.difference(&before).next().unwrap();
    assert_eq!(j.stats().last_snapshot.bytes, (m.get(SNAPSHOT).unwrap().len() + m.get(rewritten).unwrap().len()) as u64);
    assert_same(&reopen(&m), &c);
}

/// A log replayed when the library is opened counts towards the next compaction like one
/// written in this session: 2000 changes within one segment, then a restart, and it is due.
#[test]
fn a_replayed_log_is_due_for_compaction_like_any_other() {
    let (m, mut j, mut c) = library(20_000, 500);
    let ops: Vec<Op> = (0..2000u64).map(|k| Op::SetRating { id: PhotoId(1 + k % 400), rating: 1 + (k % 5) as u8 }).collect();
    run(&mut j, &mut c, ops);
    assert!(j.wants_snapshot());
    drop(j);
    let (j, _, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(r.replayed, 2000);
    assert!(j.wants_snapshot());
}

/// Where photos go when segments change: a last segment that isn't full takes the photos added
/// after it; a segment is split only past twice its size; a segment keeps its place in the id
/// range when its first photo is removed; and a segment is never written over a file that is
/// there.
#[test]
fn segments_are_laid_out_as_documented() {
    let firsts = |m: &MemStore| manifest(m)["segments"].as_array().unwrap().iter().map(|s| s["first"].as_u64().unwrap()).collect::<Vec<_>>();
    let counts = |m: &MemStore| manifest(m)["segments"].as_array().unwrap().iter().map(|s| s["photos"].as_u64().unwrap()).collect::<Vec<_>>();
    // a last segment of 2 (of 4) takes the next photos
    let (m, mut j, mut c) = library(10, 4);
    run(&mut j, &mut c, add(11..=12));
    j.snapshot(&c).unwrap();
    assert_eq!((firsts(&m), counts(&m)), (vec![1, 5, 9], vec![4, 4, 4]));
    // now full: the next ones start a segment
    run(&mut j, &mut c, add(13..=13));
    j.snapshot(&c).unwrap();
    assert_eq!((firsts(&m), counts(&m)), (vec![1, 5, 9, 13], vec![4, 4, 4, 1]));

    // a segment grows to twice its size before it is split (ids with room between them)
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    j.policy.segment_photos = 4;
    run(&mut j, &mut c, add((1..=10).map(|k| k * 10)));
    j.snapshot(&c).unwrap();
    assert_eq!((firsts(&m), counts(&m)), (vec![10, 50, 90], vec![4, 4, 2]));
    run(&mut j, &mut c, add(51..=54));
    j.snapshot(&c).unwrap();
    assert_eq!((firsts(&m), counts(&m)), (vec![10, 50, 90], vec![4, 8, 2]));
    run(&mut j, &mut c, add(55..=55));
    j.snapshot(&c).unwrap();
    assert_eq!((firsts(&m), counts(&m)), (vec![10, 50, 54, 80, 90], vec![4, 4, 4, 1, 2]));
    assert_same(&reopen(&m), &c);

    // the first photo of a segment removed: the segment still starts where it did
    let (m, mut j, mut c) = library(12, 4);
    run(&mut j, &mut c, vec![Op::RemovePhoto { id: PhotoId(5) }]);
    j.snapshot(&c).unwrap();
    assert_eq!((firsts(&m), counts(&m)), (vec![1, 5, 9], vec![4, 3, 4]));

    // two snapshots at the same op (the second of a catalog that differs): the segment written
    // second is a new file, not the first one written over
    let (m, mut j, c) = library(12, 4);
    let first = listed(&m);
    let mut other = c.clone();
    other.apply(Op::SetRating { id: PhotoId(2), rating: 5 }).unwrap();
    j.snapshot(&other).unwrap();
    let second = listed(&m);
    assert_eq!(first.intersection(&second).count(), 2);
    assert!(second.difference(&first).all(|n| !first.contains(n)));
    assert_same(&reopen(&m), &other);
}

/// A store that records the names it is asked to read or remove, fails writing the segments
/// whose name it is told to, and gives further handles (so segments are written and read on
/// several threads).
#[derive(Clone, Default)]
struct Watched {
    files: MemStore,
    asked: Arc<Mutex<BTreeSet<String>>>,
    fail: Arc<Mutex<Option<String>>>,
}

impl Store for Watched {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.asked.lock().unwrap().insert(name.to_string());
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if self.fail.lock().unwrap().as_deref().is_some_and(|f| name.starts_with(f)) {
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
        self.asked.lock().unwrap().insert(name.to_string());
        self.files.remove(name)
    }
    fn list(&mut self) -> std::io::Result<Vec<String>> {
        self.files.list()
    }
    fn segmented(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "watched".into()
    }
    fn background_writer(&self) -> Option<Box<dyn Store>> {
        Some(Box::new(self.clone()))
    }
}

/// Many segments are written on several threads; the result is the same, and when one of them
/// can't be written the snapshot fails as a whole: nothing of it stays, and the library is what
/// it was, with its log.
#[test]
fn segments_are_written_on_several_threads_or_not_at_all() {
    let store = Watched::default();
    let (mut j, mut c, _) = Journal::open(Box::new(store.clone())).unwrap();
    j.policy.segment_photos = 4;
    run(&mut j, &mut c, add(1..=80));
    j.snapshot(&c).unwrap();
    assert_eq!(j.stats().last_snapshot.segments, 20);
    assert_eq!(
        manifest(&store.files)["segments"].as_array().unwrap().iter().map(|s| s["first"].as_u64().unwrap()).collect::<Vec<_>>(),
        (0..20).map(|k| 1 + 4 * k).collect::<Vec<_>>()
    );
    assert_same(&reopen(&store.files), &c);

    // every segment changes; the one starting at photo 41 can't be written
    let before = store.files.files.lock().unwrap().clone();
    run(&mut j, &mut c, (1..=80).map(|id| Op::SetRating { id: PhotoId(id), rating: 3 }).collect());
    let logged = store.files.get(LOG).unwrap();
    *store.fail.lock().unwrap() = Some(format!("{SEGMENT_PREFIX}41-"));
    assert!(j.snapshot(&c).is_err());
    let mut after = store.files.files.lock().unwrap().clone();
    assert_eq!(after.remove(LOG), Some(logged));
    let mut expected = before.clone();
    expected.remove(LOG);
    assert_eq!(after, expected, "a failed snapshot left or changed files");
    assert_same(&reopen(&store.files), &c);
    // and it works again once the disk does
    *store.fail.lock().unwrap() = None;
    j.snapshot(&c).unwrap();
    assert_eq!(j.stats().last_snapshot.segments, 20);
    assert_same(&reopen(&store.files), &c);
}

/// A snapshot naming files that aren't segments is refused before any of them is read, and
/// nothing is ever removed under such a name.
#[test]
fn only_segment_files_are_ever_read_or_removed() {
    for name in [
        "../segments/1-1.json",
        "segments/../../x.json",
        "segments/1-1.json/../../x",
        "segments/sub/1-1.json",
        "prefs.json",
        "catalog.log",
        "segments/.json",
        "segments/1-1",
        "segments/1-1.json.tmp",
        "segments/١-1.json",
        "catalog.seg-1-1",
    ] {
        let store = Watched::default();
        let (mut j, mut c, _) = Journal::open(Box::new(store.clone())).unwrap();
        j.policy.segment_photos = 4;
        run(&mut j, &mut c, add(1..=12));
        j.snapshot(&c).unwrap();
        drop(j);
        let mut v = manifest(&store.files);
        let real = v["segments"][0]["name"].as_str().unwrap().to_string();
        v["segments"][0]["name"] = name.into();
        store.files.set(SNAPSHOT, serde_json::to_vec(&v).unwrap());
        // (what the name points at exists: the refusal is not for want of a file)
        store.files.set(name, store.files.get(&real).unwrap());
        store.asked.lock().unwrap().clear();
        let e = Journal::open(Box::new(store.clone())).map(|_| ()).unwrap_err();
        assert!(matches!(&e, CatalogError::Corrupt(msg) if msg.contains("not a segment file")), "{name}: {e}");
        let asked = store.asked.lock().unwrap().clone();
        assert_eq!(asked.into_iter().collect::<Vec<_>>(), [LOG, SNAPSHOT], "{name}");
    }
    // the same file twice, and segments out of order, are refused as such
    for (what, edit) in [
        ("twice", (|v| v["segments"][1] = v["segments"][0].clone()) as fn(&mut Value)),
        ("out of order", |v| v["segments"].as_array_mut().unwrap().swap(0, 2)),
    ] {
        let (m, j, _c) = library(12, 4);
        drop(j);
        let mut v = manifest(&m);
        edit(&mut v);
        m.set(SNAPSHOT, serde_json::to_vec(&v).unwrap());
        let e = Journal::open(Box::new(m)).map(|_| ()).unwrap_err();
        assert!(matches!(&e, CatalogError::Corrupt(msg) if msg.contains(what)), "{what}: {e}");
    }
}

/// Given a library saved in one file (format 10: `fixtures/v10-catalog.*`, written by that build,
/// with a log of later changes), when it is opened, then it reads as it was and is rewritten in
/// segments.
#[test]
fn a_v9_library_loads_and_is_split_into_segments() {
    let old = include_str!("fixtures/v10-catalog.snap.json");
    let log = include_str!("fixtures/v10-catalog.log.jsonl");
    assert!(old.starts_with(r#"{"format":"lightcraft-catalog","version":10,"#));
    // what the format 10 build reads from these files: the single-file loader, unchanged
    let v: Value = serde_json::from_str(old).unwrap();
    let mut expected = Catalog::from_snapshot(&v["catalog"].to_string()).unwrap();
    expected.replay(&log.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["op"].to_string() + "\n").collect::<String>()).unwrap();
    assert_eq!(expected.len(), 3);

    let m = MemStore::new();
    m.set(SNAPSHOT, old.as_bytes().to_vec());
    m.set(LOG, log.as_bytes().to_vec());
    let (j, loaded, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!((r.replayed, r.failed, r.upgraded_from), (log.lines().count(), 0, Some(10)));
    assert_same(&loaded, &expected);
    assert_eq!(manifest(&m)["version"].as_u64(), Some(u64::from(VERSION)));
    assert_eq!(manifest(&m)["catalog"]["photos"], serde_json::json!({}));
    assert_eq!(listed(&m).len(), 1);
    assert_eq!(segment_files(&m), listed(&m));
    assert!(m.get(LOG).unwrap().is_empty());
    drop(j);
    let (_, again, r) = Journal::open(Box::new(m.clone())).unwrap();
    assert_eq!(r.upgraded_from, None);
    assert_same(&again, &expected);
}

/// A store whose segment files come back different from what was written.
#[derive(Clone, Default)]
struct Garbling {
    files: MemStore,
    garble: Arc<Mutex<bool>>,
}

impl Store for Garbling {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        let mut b = self.files.read(name)?;
        if *self.garble.lock().unwrap()
            && name.starts_with(SEGMENT_PREFIX)
            && let Some(b) = b.as_mut()
            && let Some(at) = b.iter().position(|x| *x == b'4')
        {
            // a digit of a number: still JSON, still the same length
            b[at] = b'5';
        }
        Ok(b)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
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
    fn list(&mut self) -> std::io::Result<Vec<String>> {
        self.files.list()
    }
    fn segmented(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "garbling".into()
    }
}

/// Given a one-file library, when the segments written for it don't read back as the photos
/// that were loaded, then the one file is kept as it is: the library opens from it as before,
/// nothing of the failed attempt is left, and the split is tried again at the next open.
#[test]
fn a_split_that_does_not_read_back_keeps_the_one_file() {
    let old = include_str!("fixtures/v10-catalog.snap.json");
    let log = include_str!("fixtures/v10-catalog.log.jsonl");
    let store = Garbling::default();
    store.files.set(SNAPSHOT, old.as_bytes().to_vec());
    store.files.set(LOG, log.as_bytes().to_vec());
    *store.garble.lock().unwrap() = true;
    let (mut j, mut loaded, r) = Journal::open(Box::new(store.clone())).unwrap();
    assert_eq!((r.replayed, r.upgraded_from), (log.lines().count(), None), "not upgraded");
    assert_eq!(store.files.get(SNAPSHOT).unwrap(), old.as_bytes());
    assert_eq!(store.files.get(LOG).unwrap(), log.as_bytes());
    assert!(segment_files(&store.files).is_empty());
    // the library is usable meanwhile, and its changes are saved
    run(&mut j, &mut loaded, vec![Op::SetRating { id: PhotoId(8), rating: 2 }]);
    drop(j);
    *store.garble.lock().unwrap() = false;
    let (_, again, r) = Journal::open(Box::new(store.clone())).unwrap();
    assert_eq!(r.upgraded_from, Some(10));
    assert_same(&again, &loaded);
    assert_eq!(segment_files(&store.files), listed(&store.files));
}

/// A store without segments (the browser's): the photos stay in `catalog.snap`, and the
/// library moves to segments when it is opened where they are possible, and back.
#[test]
fn a_store_without_segments_keeps_one_file() {
    let m = MemStore { single_file: true, ..Default::default() };
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    j.policy.segment_photos = 4;
    run(&mut j, &mut c, add(1..=10));
    j.snapshot(&c).unwrap();
    assert_eq!(manifest(&m)["catalog"]["photos"].as_object().unwrap().len(), 10);
    assert!(manifest(&m)["segments"].as_array().is_none_or(|s| s.is_empty()) && segment_files(&m).is_empty());
    assert_same(&reopen(&m), &c);
    drop(j);

    // the same files where segments are possible
    let desktop = MemStore { single_file: false, ..crash_copy(&m) };
    let (mut j, mut c2, _) = Journal::open(Box::new(desktop.clone())).unwrap();
    assert_same(&c2, &c);
    j.policy.segment_photos = 4;
    run(&mut j, &mut c2, vec![Op::SetRating { id: PhotoId(3), rating: 3 }]);
    j.snapshot(&c2).unwrap();
    assert_eq!(manifest(&desktop)["catalog"]["photos"], serde_json::json!({}));
    assert_eq!(listed(&desktop).len(), 3);
    assert_same(&reopen(&desktop), &c2);
    drop(j);

    // and back: a store without segments that holds the segment files reads them and returns
    // to one file
    let back = MemStore { single_file: true, ..crash_copy(&desktop) };
    let (mut j, mut c3, _) = Journal::open(Box::new(back.clone())).unwrap();
    assert_same(&c3, &c2);
    run(&mut j, &mut c3, vec![Op::SetRating { id: PhotoId(4), rating: 4 }]);
    j.snapshot(&c3).unwrap();
    assert_eq!(manifest(&back)["catalog"]["photos"].as_object().unwrap().len(), 10);
    assert_same(&reopen(&back), &c3);
}

/// When to compact goes by what a compaction would rewrite: changes within one segment are
/// compacted after 2000 of them, however large the library; changes spread over the whole
/// library wait until the log is a quarter of the segments they touch.
#[test]
fn compaction_is_due_by_what_it_would_rewrite() {
    let (_m, mut j, mut c) = library(20_000, 500);
    assert!(j.snapshot_bytes() > 2_000_000);
    // 2000 changes to photos of one segment
    for k in 0..2000u64 {
        assert!(!j.wants_snapshot(), "after {k} changes");
        run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(1 + k % 400), rating: (k % 6) as u8 }]);
    }
    assert!(j.wants_snapshot());
    j.snapshot(&c).unwrap();
    assert_eq!(j.stats().last_snapshot.segments, 1);

    // 2000 changes spread over every segment: not yet
    let ops: Vec<Op> = (0..2000u64).map(|k| Op::SetRating { id: PhotoId(1 + k * 10), rating: 1 + (k % 5) as u8 }).collect();
    run(&mut j, &mut c, ops);
    assert!(!j.wants_snapshot(), "{} records, {} bytes of log for {} bytes of snapshot", j.log_records(), j.log_bytes(), j.snapshot_bytes());
    // closing a library compacts a long log all the same (it isn't left long between sessions)
    assert!(j.wants_snapshot_on_close());
    while !j.wants_snapshot() {
        let ops: Vec<Op> = (0..2000u64).map(|k| Op::SetRating { id: PhotoId(1 + k * 10), rating: (k % 6) as u8 }).collect();
        run(&mut j, &mut c, ops);
    }
    assert!(j.log_bytes() >= j.snapshot_bytes() / 5, "{} of {}", j.log_bytes(), j.snapshot_bytes());
}

/// An import of many photos writes, in all its compactions together, little more than the
/// final catalog.
#[test]
fn an_import_writes_little_more_than_the_catalog() {
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    j.policy.segment_photos = 1000;
    let mut written = 0;
    let mut next = 1;
    for _ in 0..80 {
        run(&mut j, &mut c, add(next..next + 500));
        next += 500;
        if j.wants_snapshot() {
            j.snapshot(&c).unwrap();
            written += j.stats().last_snapshot.bytes;
        }
    }
    j.snapshot(&c).unwrap();
    written += j.stats().last_snapshot.bytes;
    assert!(written <= j.snapshot_bytes() * 5 / 2, "{written} bytes written for a catalog of {}", j.snapshot_bytes());
    assert_eq!(reopen(&m).len(), 40_000);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(96))]

    /// Whatever is done to a library (photos added at the end, in gaps and below the first
    /// one, removed, changed; snapshots in the foreground and by the worker; closing and
    /// opening), opening its files gives the catalog as it is, and a snapshot taken right
    /// after another has nothing to write: what the journal takes to be on disk is on disk.
    #[test]
    fn segments_hold_the_catalog_whatever_is_done(
        per in 1usize..6,
        steps in proptest::collection::vec((0u8..10, 0u64..40, 1u64..6), 1..60),
    ) {
        let m = MemStore { background: true, ..Default::default() };
        let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
        j.policy.segment_photos = per;
        let mut next = 1000u64;
        for (what, pick, n) in steps {
            let ids: Vec<PhotoId> = c.photos().map(|p| p.id).collect();
            let some = |k: u64| ids.get((k as usize) % ids.len().max(1)).copied();
            match what {
                // an import at the end
                0 | 1 => {
                    run(&mut j, &mut c, add(next..next + n));
                    next += n + pick % 3;
                }
                // photos between and below the ones there are
                2 => {
                    let id = 1 + pick * 23 % 990;
                    if c.photo(PhotoId(id)).is_none() {
                        run(&mut j, &mut c, add([id]));
                    }
                }
                3 => {
                    if let Some(id) = some(pick) {
                        run(&mut j, &mut c, vec![Op::RemovePhoto { id }]);
                    }
                }
                // a run of photos removed
                4 => {
                    let gone: Vec<Op> = ids.iter().skip(pick as usize % ids.len().max(1)).take(n as usize * 2).map(|id| Op::RemovePhoto { id: *id }).collect();
                    run(&mut j, &mut c, gone);
                }
                5 | 6 => {
                    if let Some(id) = some(pick) {
                        run(&mut j, &mut c, vec![Op::SetRating { id, rating: (n % 6) as u8 }]);
                    }
                }
                7 => {
                    j.snapshot(&c).unwrap();
                    proptest::prop_assert_eq!(reopen(&m).to_snapshot(), c.to_snapshot());
                    j.snapshot(&c).unwrap();
                    proptest::prop_assert_eq!(j.stats().last_snapshot.segments, 0);
                }
                8 => {
                    j.snapshot_in_background(&c).unwrap();
                    if pick % 2 == 0
                        && let Some(id) = some(pick)
                    {
                        run(&mut j, &mut c, vec![Op::SetRating { id, rating: (n % 6) as u8 }]);
                    }
                    j.wait().unwrap();
                }
                _ => {
                    drop(j);
                    let (j2, c2, _) = Journal::open(Box::new(m.clone())).unwrap();
                    proptest::prop_assert_eq!(c2.to_snapshot(), c.to_snapshot());
                    j = j2;
                    c = c2;
                    j.policy.segment_photos = per;
                    j.remove_unlisted_segments();
                }
            }
            proptest::prop_assert_eq!(reopen(&crash_copy(&m)).to_snapshot(), c.to_snapshot());
        }
        j.wait().unwrap();
        j.snapshot(&c).unwrap();
        proptest::prop_assert_eq!(reopen(&m).to_snapshot(), c.to_snapshot());
        j.snapshot(&c).unwrap();
        proptest::prop_assert_eq!(j.stats().last_snapshot.segments, 0);
        proptest::prop_assert_eq!(segment_files(&m), listed(&m));
        // every photo is in the one segment whose stretch of ids holds it
        let firsts: Vec<u64> = j.segments().iter().map(|s| s.first.0).collect();
        proptest::prop_assert!(firsts.windows(2).all(|w| w[0] < w[1]));
        proptest::prop_assert_eq!(j.segments().iter().map(|s| s.photos).sum::<u64>(), c.len() as u64);
    }
}

/// Segments are sized by what they hold, not only by how many photos: where records are large
/// (long captions here; edited photos with their history in a real library), a segment holds
/// fewer of them, so that a change to one photo rewrites about as much either way.
#[test]
fn large_records_go_into_segments_of_fewer_photos() {
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    j.policy.segment_photos = 2000;
    j.policy.segment_bytes = 256 << 10;
    let heavy: Vec<Op> = (1..=1000u64)
        .map(|id| {
            let mut p = photo(id);
            p.meta.caption = "a caption ".repeat(200);
            Op::AddPhoto { photo: Box::new(p) }
        })
        .collect();
    run(&mut j, &mut c, heavy);
    // a first snapshot, of photos whose size isn't known yet: by count
    j.snapshot(&c).unwrap();
    assert_eq!(j.segments().len(), 1);
    // once it is, a segment that changes is cut to size, and so are photos added later
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(1), rating: 1 }]);
    let more: Vec<Op> = (1001..=1400u64)
        .map(|id| {
            let mut p = photo(id);
            p.meta.caption = "a caption ".repeat(200);
            Op::AddPhoto { photo: Box::new(p) }
        })
        .collect();
    run(&mut j, &mut c, more);
    j.snapshot(&c).unwrap();
    let sizes: Vec<(u64, u64)> = j.segments().iter().map(|s| (s.photos, s.bytes)).collect();
    assert!(sizes.len() >= 8, "{sizes:?}");
    assert!(sizes.iter().all(|(photos, bytes)| *photos <= 260 && *bytes <= 600 << 10), "{sizes:?}");
    assert_same(&reopen(&m), &c);

    // small records: by count, as before
    let (_m, j, _c) = library(1000, 300);
    assert_eq!(j.segments().iter().map(|s| s.photos).collect::<Vec<_>>(), [300, 300, 300, 100]);
}

/// Given a library in segments that states other defaults for develop settings than this build
/// has (an Exposure of +1), when it is opened, on one thread or several, then every photo has
/// that Exposure, not this build's; and every segment is rewritten with this build's defaults,
/// stating the Exposure, before anything is added: none of the old ones is kept, and the library
/// opens the same again.
#[test]
fn a_library_in_segments_with_other_defaults_reads_as_it_was_written() {
    let (m, j, _) = library(200, 8);
    drop(j);
    let before = listed(&m);
    assert!(before.len() >= 20);
    // as a build with other defaults wrote it: the same records, other stated defaults
    let mut theirs = stored::base().1.clone();
    theirs["light"]["exposure"] = 1.0.into();
    let snap = String::from_utf8(m.get(SNAPSHOT).unwrap()).unwrap();
    let stated = format!("\"defaults\":{},", stored::base().0);
    assert!(snap.contains(&stated));
    m.set(SNAPSHOT, snap.replace(&stated, &format!("\"defaults\":{theirs},")).into_bytes());

    for background in [false, true] {
        let m = MemStore { background, ..crash_copy(&m) };
        let (_, c, r) = Journal::open(Box::new(m.clone())).unwrap();
        assert!(r.other_defaults && r.upgraded_from.is_none(), "{r:?}");
        assert_eq!(c.len(), 200);
        assert!(c.photos().all(|p| p.develop.light.exposure == 1.0), "background: {background}");

        let after = listed(&m);
        assert!(after.is_disjoint(&before), "every segment rewritten");
        assert_eq!(segment_files(&m), after, "the old ones are gone");
        assert_eq!(manifest(&m)["defaults"], stored::base().1);
        let segment = String::from_utf8(m.get(after.iter().next().unwrap()).unwrap()).unwrap();
        // (stated first; a new photo's settings also state the rendering process it is on)
        assert!(segment.contains(r#""develop":{"light":{"exposure":1.0}"#), "{}", &segment[..segment.len().min(400)]);
        let (_, again, r) = Journal::open(Box::new(m)).unwrap();
        assert!(!r.other_defaults);
        assert_same(&again, &c);
    }
}

/// The sizes a library gets unless told otherwise: segments of about 2 MB, and of at most 4096
/// photos where records are small. (Measured: `docs/catalog-persistence.md`, "Segment size".
/// Larger ones rewrite more for the same edits; much smaller ones are many files to make
/// durable when everything changed.)
#[test]
fn a_segment_is_about_two_megabytes_and_at_most_4096_photos() {
    // small records: by count
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    run(&mut j, &mut c, add(1..=10_000));
    j.snapshot(&c).unwrap();
    assert_eq!(j.segments().iter().map(|s| s.photos).collect::<Vec<_>>(), [4096, 4096, 1808]);
    assert!(j.segments().iter().all(|s| s.bytes <= 2 << 20), "{:?}", j.segments().iter().map(|s| s.bytes).collect::<Vec<_>>());
    assert_same(&reopen(&m), &c);

    // large records: by size, once it is known
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    let heavy: Vec<Op> = (1..=3000u64)
        .map(|id| {
            let mut p = photo(id);
            p.meta.caption = "a caption ".repeat(200);
            Op::AddPhoto { photo: Box::new(p) }
        })
        .collect();
    run(&mut j, &mut c, heavy);
    j.snapshot(&c).unwrap();
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(1), rating: 1 }]);
    j.snapshot(&c).unwrap();
    let sizes: Vec<(u64, u64)> = j.segments().iter().map(|s| (s.photos, s.bytes)).collect();
    assert!(sizes.len() >= 3, "{sizes:?}");
    assert!(sizes.iter().all(|(photos, bytes)| *photos < 4096 && *bytes <= 5 << 19), "{sizes:?}");
    assert_same(&reopen(&m), &c);
}

/// In a library folder the segments are files `segments/<first id>-<seq>.json`: a folder of
/// their own beside `catalog.snap`, made with the first of them; what is left of one being
/// written, and files nothing lists, are cleared out of it on request; other files are not.
#[test]
fn segments_are_json_files_in_a_folder_of_their_own() {
    let dir = std::env::temp_dir().join(format!("lc-segments-folder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let names = |d: &std::path::Path| {
        let mut v: Vec<String> = std::fs::read_dir(d).map(|r| r.map(|e| e.unwrap().file_name().into_string().unwrap()).collect()).unwrap_or_default();
        v.sort();
        v
    };
    let (mut j, mut c, _) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    assert!(!dir.join("segments").exists(), "an empty library has none");
    j.policy.segment_photos = 4;
    run(&mut j, &mut c, add(1..=12));
    j.snapshot(&c).unwrap();
    assert_eq!(names(&dir), ["catalog.log", "catalog.snap", "segments"]);
    assert_eq!(names(&dir.join("segments")), ["1-12.json", "5-12.json", "9-12.json"]);
    assert_eq!(j.segments().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["segments/1-12.json", "segments/5-12.json", "segments/9-12.json"]);
    let text = std::fs::read_to_string(dir.join("segments/5-12.json")).unwrap();
    assert!(serde_json::from_str::<Value>(&text).unwrap()["photos"]["6"]["file_name"].is_string());
    run(&mut j, &mut c, vec![Op::SetRating { id: PhotoId(6), rating: 5 }]);
    j.snapshot(&c).unwrap();
    assert_eq!(names(&dir.join("segments")), ["1-12.json", "5-13.json", "9-12.json"]);
    drop(j);
    // left by a crash, and a file that is somebody else's
    for (name, bytes) in [("77-1.json", "x"), ("77-2.json.tmp", "x"), ("notes.txt", "mine")] {
        std::fs::write(dir.join("segments").join(name), bytes).unwrap();
    }
    let (mut j, back, _) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    assert_same(&back, &c);
    j.remove_unlisted_segments();
    assert_eq!(names(&dir.join("segments")), ["1-12.json", "5-13.json", "9-12.json", "notes.txt"]);
    drop(j);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The segments folder has to be a folder of the library's own. A file of that name, or a link
/// to a folder elsewhere, is an error that names it (and nothing in the folder linked to is
/// listed, read or removed); a folder that can't be read is an error too, never "no segments".
#[test]
fn a_segments_folder_that_is_not_one_is_an_error_that_says_so() {
    let temp = |name: &str| {
        let dir = std::env::temp_dir().join(format!("lc-segments-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    // a file named `segments`: the snapshot fails and says why; the changes stay in the log
    let dir = temp("file");
    std::fs::write(dir.join("segments"), b"not a folder").unwrap();
    // (no library is started in a folder like that: it says what is in the way)
    let e = Journal::open(Box::new(FsStore::open(&dir).unwrap())).map(|_| ()).unwrap_err();
    assert!(e.to_string().contains("segments") && e.to_string().contains("not a folder"), "{e}");
    std::fs::remove_file(dir.join("segments")).unwrap();
    drop(Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap());
    std::fs::write(dir.join("segments"), b"not a folder").unwrap();
    let (mut j, mut c, _) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    run(&mut j, &mut c, add(1..=12));
    let e = j.snapshot(&c).unwrap_err();
    assert!(e.to_string().contains("segments") && e.to_string().contains("not a folder"), "{e}");
    drop(j);
    assert_eq!(std::fs::read(dir.join("segments")).unwrap(), b"not a folder");
    assert_eq!(Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap().1.len(), 12);
    let _ = std::fs::remove_dir_all(&dir);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // a link to a folder elsewhere: what is in there is not the library's
        let (dir, elsewhere) = (temp("link"), temp("elsewhere"));
        std::fs::write(elsewhere.join("2024-10.json"), b"somebody's").unwrap();
        drop(Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap());
        std::os::unix::fs::symlink(&elsewhere, dir.join("segments")).unwrap();
        let (mut j, mut c, _) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
        run(&mut j, &mut c, add(1..=12));
        assert!(j.snapshot(&c).unwrap_err().to_string().contains("not a folder"));
        j.remove_unlisted_segments();
        drop(j);
        assert_eq!(std::fs::read(elsewhere.join("2024-10.json")).unwrap(), b"somebody's");
        for d in [dir, elsewhere] {
            let _ = std::fs::remove_dir_all(d);
        }

        // segments that can't be listed, and no snapshot: refused, not opened as a new library
        let dir = temp("unreadable");
        let (mut j, mut c, _) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
        j.policy.segment_photos = 4;
        run(&mut j, &mut c, add(1..=12));
        j.snapshot(&c).unwrap();
        drop(j);
        std::fs::remove_file(dir.join(SNAPSHOT)).unwrap();
        std::fs::remove_file(dir.join(LOG)).unwrap();
        std::fs::set_permissions(dir.join("segments"), std::fs::Permissions::from_mode(0o300)).unwrap();
        let refused = Journal::open(Box::new(FsStore::open(&dir).unwrap())).map(|_| ());
        std::fs::set_permissions(dir.join("segments"), std::fs::Permissions::from_mode(0o700)).unwrap();
        // (a process that may read anything, as root in a container, lists it: then it is refused for the segments it holds)
        assert!(refused.is_err(), "opened as a new library");
        assert!(!dir.join(SNAPSHOT).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The plain write of the store (a whole file given at once) puts a segment in its folder too.
#[test]
fn every_way_of_writing_a_segment_makes_its_folder() {
    let dir = std::env::temp_dir().join(format!("lc-segments-plain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = FsStore::open(&dir).unwrap();
    store.write_atomic(&segment_file("1-1"), b"{}").unwrap();
    assert_eq!(std::fs::read(dir.join("segments/1-1.json")).unwrap(), b"{}");
    assert_eq!(store.list().unwrap(), ["segments/1-1.json"]);
    store.remove(&segment_file("1-1")).unwrap();
    assert!(store.list().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
