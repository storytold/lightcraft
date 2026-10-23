//! The snapshot's photos in segment files (catalog format 11).
//!
//! `catalog.snap` holds everything about the catalog but its photos, and lists the **segments**
//! that hold those: files `segments/<first id>-<seq>.json` in the library folder, each a run of
//! photos in id order,
//!
//! ```text
//! {"format":"lightcraft-catalog-segment","version":11,"photos":{"<id>":{…},…}}
//! ```
//!
//! with its size and CRC-32 in the list. A compaction writes only the segments whose photos
//! changed since the last one, so its cost follows what changed, not the size of the library;
//! and opening a library reads its segments on several threads.
//!
//! **Which segments changed** is not worked out from the ops. The journal keeps the photos as the
//! segments on disk hold them (the *baseline*: shared pointers, not copies); a photo is the same
//! as on disk while the catalog still points to the same one. Changing a photo copies it (it is
//! shared with the baseline), so a segment is rewritten exactly when one of its photos was
//! changed, added or removed, whatever did it.
//!
//! **Where photos go.** Segment `k` holds the ids from its `first` up to the next segment's.
//! A segment whose photos changed is written again under a new name, in pieces of
//! [`crate::SnapshotPolicy::segment_photos`] once it holds more than twice that; one left without
//! photos is dropped. Photos added after a full last segment start new ones (an import leaves the
//! existing segments alone). Ids need not be dense: segments go by how many photos there are.
//!
//! **Crash safety.** Segments are written under names no snapshot lists yet, each fsynced; then
//! `catalog.snap` is replaced atomically, which is the commit; then the files it no longer lists
//! are removed. A crash before the commit leaves the old snapshot with all its segments; after
//! it, the new one with all of its. Segment files nothing lists are removed when the library is
//! opened.
//!
//! **A damaged segment** (wrong size or CRC, unreadable, photos it shouldn't hold) fails the open
//! with an error naming the file, and nothing is modified.
//!
//! **Stores without segments** ([`Store::segmented`] is false: the browser's) keep the photos in
//! `catalog.snap` itself, as formats up to 10 did. Either layout is read wherever it is found.

use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::store::Store;
use crate::{Photo, PhotoId};

/// The folder in the library that holds the segment files.
pub const SEGMENT_DIR: &str = "segments";
/// Segment files are named `segments/<first id>-<seq>.json`: every one starts with this, and
/// ends with [`SEGMENT_SUFFIX`].
pub const SEGMENT_PREFIX: &str = "segments/";
/// Segments are JSON, and say so.
pub const SEGMENT_SUFFIX: &str = ".json";

/// The name of the segment file `<stem>.json` in the segments folder (`stem`: `<first id>-<seq>`).
pub fn segment_file(stem: &str) -> String {
    format!("{SEGMENT_PREFIX}{stem}{SEGMENT_SUFFIX}")
}
const SEGMENT_FORMAT: &str = "lightcraft-catalog-segment";
/// Segments read at once when a library is opened.
const LOAD_THREADS: usize = 8;

pub(crate) type Photos = BTreeMap<PhotoId, Arc<Photo>>;

/// One segment, as `catalog.snap` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// The file's name in the library folder.
    pub name: String,
    /// The segment holds the ids from this one up to the next segment's `first`.
    pub first: PhotoId,
    /// How many photos it holds.
    pub photos: u64,
    /// The file's size and CRC-32.
    pub bytes: u64,
    pub crc: u32,
}

fn corrupt(what: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what)
}

/// The ids of segment `k` of `segments`: from its `first` (the first segment: from the lowest
/// id) up to the next segment's.
fn range(segments: &[Segment], k: usize) -> (Bound<PhotoId>, Bound<PhotoId>) {
    let lo = match (k, segments.get(k)) {
        (0, _) | (_, None) => Bound::Unbounded,
        (_, Some(s)) => Bound::Included(s.first),
    };
    let hi = segments.get(k + 1).map_or(Bound::Unbounded, |next| Bound::Excluded(next.first));
    (lo, hi)
}

/// A run of photos to write as one segment.
struct Piece {
    first: PhotoId,
    /// The last id it holds.
    last: PhotoId,
    photos: u64,
}

/// What a snapshot does with one stretch of the catalog.
enum Step {
    /// The segment's photos are what its file holds.
    Keep(Segment),
    Write(Piece),
    /// The ids of a stretch whose photos are not what the segments held (it is written in
    /// pieces, or holds no photo any more).
    Changed(Bound<PhotoId>, Bound<PhotoId>),
}

/// How many photos go into a segment.
#[derive(Clone, Copy)]
pub(crate) struct Size {
    /// At most this many ([`crate::SnapshotPolicy::segment_photos`]).
    pub per: usize,
    /// And about this many bytes ([`crate::SnapshotPolicy::segment_bytes`]; 0: any).
    pub bytes: u64,
    /// What a photo's record takes on average, as far as known (0: not known), for photos that
    /// are in no segment yet.
    pub record_bytes: u64,
}

impl Size {
    /// Photos per segment where a record takes `record_bytes`: the fewer the larger the records
    /// (edited photos with their history), down to 64, so that a segment is about
    /// [`Size::bytes`] whatever it holds.
    fn photos(&self, record_bytes: u64) -> usize {
        let per = self.per.max(1);
        match self.bytes.checked_div(record_bytes) {
            Some(n) if self.bytes > 0 => usize::try_from(n).unwrap_or(per).max(64).min(per),
            _ => per,
        }
    }
}

/// `now` cut into pieces of `per` photos (one piece while there are at most twice that). The
/// first piece starts at `first` when that is lower than its first photo.
fn pieces<'a>(now: impl Iterator<Item = &'a PhotoId>, first: Option<PhotoId>, per: usize, out: &mut Vec<Step>) {
    let ids: Vec<PhotoId> = now.copied().collect();
    let per = per.max(1);
    let size = if ids.len() > per.saturating_mul(2) { per } else { ids.len().max(1) };
    for (k, chunk) in ids.chunks(size).enumerate() {
        let (Some(lo), Some(hi)) = (chunk.first(), chunk.last()) else { continue };
        let start = match first {
            Some(f) if k == 0 => f.min(*lo),
            _ => *lo,
        };
        out.push(Step::Write(Piece { first: start, last: *hi, photos: chunk.len() as u64 }));
    }
}

/// What a snapshot of `photos` writes, given the `segments` on disk and the photos they hold
/// (`baseline`).
fn plan(segments: &[Segment], baseline: &Photos, photos: &Photos, size: &Size) -> Vec<Step> {
    let mut steps = Vec::new();
    if segments.is_empty() {
        steps.push(Step::Changed(Bound::Unbounded, Bound::Unbounded));
        pieces(photos.keys(), None, size.photos(size.record_bytes), &mut steps);
        return steps;
    }
    for (k, segment) in segments.iter().enumerate() {
        // by the size of this segment's own records
        let per = size.photos(segment.bytes.checked_div(segment.photos).unwrap_or(0));
        let (lo, mut hi) = range(segments, k);
        // photos added after a full last segment start segments of their own
        let mut after = None;
        if k + 1 == segments.len()
            && segment.photos >= per as u64
            && let Some((last, _)) = span(baseline, lo, hi).next_back()
        {
            hi = Bound::Included(*last);
            after = Some(*last);
        }
        let (mut was, mut now) = (span(baseline, lo, hi), span(photos, lo, hi));
        let same = loop {
            match (was.next(), now.next()) {
                (None, None) => break true,
                (Some((a, p)), Some((b, q))) if a == b && Arc::ptr_eq(p, q) => {}
                _ => break false,
            }
        };
        // (a segment the baseline has no photos for is not known to be on disk as it should be)
        if same && span(baseline, lo, hi).next().is_some() {
            steps.push(Step::Keep(segment.clone()));
        } else {
            steps.push(Step::Changed(lo, hi));
            pieces(span(photos, lo, hi).map(|(id, _)| id), Some(segment.first), per, &mut steps);
        }
        if let Some(last) = after
            && span(photos, Bound::Excluded(last), Bound::Unbounded).next().is_some()
        {
            steps.push(Step::Changed(Bound::Excluded(last), Bound::Unbounded));
            pieces(span(photos, Bound::Excluded(last), Bound::Unbounded).map(|(id, _)| id), None, per, &mut steps);
        }
    }
    steps
}

/// Counts and checksums what is written through it.
struct Summed<W> {
    inner: W,
    crc: crc32fast::Hasher,
}

impl<W: std::io::Write> std::io::Write for Summed<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        if let Some(written) = buf.get(..n) {
            self.crc.update(written);
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// A name for a new segment that no file in `taken` has.
fn fresh_name(first: PhotoId, seq: u64, taken: &std::collections::BTreeSet<String>) -> String {
    let mut name = segment_file(&format!("{}-{seq}", first.0));
    let mut n = 1u32;
    while taken.contains(&name) {
        name = segment_file(&format!("{}-{seq}-{n}", first.0));
        n = n.saturating_add(1);
    }
    name
}

/// Only names this module gives: nothing outside the library folder, no other file of it.
pub(crate) fn valid_name(name: &str) -> bool {
    name.strip_prefix(SEGMENT_PREFIX)
        .and_then(|rest| rest.strip_suffix(SEGMENT_SUFFIX))
        .is_some_and(|stem| !stem.is_empty() && stem.len() <= 64 && stem.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
}

/// The photos with ids from `lo` to `hi` (none when the bounds are the wrong way round, which
/// `BTreeMap::range` would not take).
fn span(photos: &Photos, lo: Bound<PhotoId>, hi: Bound<PhotoId>) -> std::collections::btree_map::Range<'_, PhotoId, Arc<Photo>> {
    let ordered = match (lo, hi) {
        (Bound::Included(a), Bound::Included(b)) => a <= b,
        (Bound::Included(a) | Bound::Excluded(a), Bound::Excluded(b)) | (Bound::Excluded(a), Bound::Included(b)) => a < b,
        (Bound::Unbounded, _) | (_, Bound::Unbounded) => true,
    };
    if ordered { photos.range((lo, hi)) } else { photos.range(PhotoId(0)..PhotoId(0)) }
}

fn write_segment(store: &mut dyn Store, name: &str, version: u32, piece: &Piece, photos: &Photos) -> std::io::Result<Segment> {
    let mut crc = 0;
    let bytes = store.write_new_with(name, &mut |w| {
        use std::io::Write;
        let mut b = Summed { inner: std::io::BufWriter::with_capacity(1 << 20, w), crc: crc32fast::Hasher::new() };
        write!(b, "{{\"format\":\"{SEGMENT_FORMAT}\",\"version\":{version},\"photos\":")?;
        let run = span(photos, Bound::Included(piece.first), Bound::Included(piece.last));
        serde_json::to_writer(&mut b, &crate::stored::PhotoRange(run)).map_err(std::io::Error::other)?;
        b.write_all(b"}\n")?;
        b.flush()?;
        crc = b.crc.clone().finalize();
        Ok(())
    })?;
    Ok(Segment { name: name.to_string(), first: piece.first, photos: piece.photos, bytes, crc })
}

/// Write `pieces` (each with its place in the list and its file name), on several threads when
/// the store gives further handles (see [`readers`]): a segment is its own file, and most of
/// writing one is waiting for it to reach the disk. One result per piece, in order.
fn write_pieces(store: &mut dyn Store, version: u32, pieces: &[(usize, String, Piece)], photos: &Photos) -> Vec<std::io::Result<Segment>> {
    let handles = readers(store, pieces.len());
    if handles.len() < 2 {
        return pieces.iter().map(|(_, name, piece)| write_segment(store, name, version, piece, photos)).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done: std::sync::Mutex<Vec<(usize, std::io::Result<Segment>)>> = std::sync::Mutex::new(Vec::new());
    let work = |store: &mut dyn Store| {
        loop {
            let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some((_, name, piece)) = pieces.get(k) else { break };
            let written = write_segment(store, name, version, piece, photos);
            done.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((k, written));
        }
    };
    together(handles, store, &work);
    let mut results: Vec<Option<std::io::Result<Segment>>> = Vec::new();
    results.resize_with(pieces.len(), || None);
    for (k, written) in done.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner) {
        if let Some(slot) = results.get_mut(k) {
            *slot = Some(written);
        }
    }
    results
        .into_iter()
        .zip(pieces)
        .map(|(r, (_, name, _))| r.unwrap_or_else(|| Err(std::io::Error::other(format!("{name} could not be written")))))
        .collect()
}

/// Run `work` on a thread per handle and on this one (with `store`), until all return. A thread
/// that can't be started just isn't one of them, and one that panics leaves what it was doing
/// undone (the callers find that out from what is missing): neither is a panic here.
fn together(handles: Vec<Box<dyn Store>>, store: &mut dyn Store, work: &(dyn Fn(&mut dyn Store) + Sync)) {
    std::thread::scope(|scope| {
        let threads: Vec<_> = handles
            .into_iter()
            .filter_map(|mut handle| {
                std::thread::Builder::new().name("catalog-segments".into()).spawn_scoped(scope, move || work(handle.as_mut())).ok()
            })
            .collect();
        work(store);
        for thread in threads {
            let _ = thread.join();
        }
    });
}

/// The segments of a snapshot, written and kept.
pub(crate) struct Written {
    /// The snapshot's segments, in order.
    pub segments: Vec<Segment>,
    /// How many of them were written now, and their bytes.
    pub written: u64,
    pub bytes: u64,
    /// Files the snapshot before this one listed and this one doesn't: to remove once this one
    /// is committed.
    pub obsolete: Vec<String>,
    /// Files written now: to remove if this snapshot is not committed after all.
    pub new: Vec<String>,
    /// The stretches of ids whose photos changed (see [`refresh`]).
    pub changed: Vec<(Bound<PhotoId>, Bound<PhotoId>)>,
}

/// Bring `baseline` (the photos the segments held) up to a snapshot of `photos` that was
/// committed: in the stretches that changed, it holds what `photos` holds. Costs what changed,
/// not a copy of every photo's pointer.
pub(crate) fn refresh(baseline: &mut Photos, photos: &Photos, changed: &[(Bound<PhotoId>, Bound<PhotoId>)]) {
    for (lo, hi) in changed {
        let gone: Vec<PhotoId> = span(baseline, *lo, *hi).map(|(id, _)| *id).collect();
        for id in gone {
            baseline.remove(&id);
        }
        baseline.extend(span(photos, *lo, *hi).map(|(id, p)| (*id, p.clone())));
    }
}

/// What a snapshot's segments are written against.
#[derive(Clone, Copy)]
pub(crate) struct OnDisk<'a> {
    /// The catalog format and the op the snapshot is of (they name the files).
    pub version: u32,
    pub seq: u64,
    /// The segments the current snapshot lists, and the photos they hold.
    pub segments: &'a [Segment],
    pub baseline: &'a Photos,
    /// How large segments are to be.
    pub size: Size,
    /// Read every segment written back and compare it with the photos before returning (when a
    /// one-file library is split: its one file is replaced only by segments known to hold the
    /// same).
    pub verify: bool,
}

/// Write the segments of `photos` that differ from what `segments` hold (`baseline`), under
/// names no file has. Nothing is listed anywhere yet: the caller commits by writing
/// `catalog.snap` with [`Written::segments`], or removes [`Written::new`].
pub(crate) fn write(store: &mut dyn Store, photos: &Photos, on_disk: &OnDisk<'_>) -> std::io::Result<Written> {
    let OnDisk { version, seq, segments, baseline, size, verify } = *on_disk;
    // names no file has: not the listed segments', and not those of files nothing lists (left by
    // a snapshot that was not committed, or whose outcome this journal never learned)
    let mut taken: std::collections::BTreeSet<String> = segments.iter().map(|s| s.name.clone()).collect();
    // (a folder that can't be listed is a snapshot that can't be written, not an empty folder)
    let held = store.list()?;
    // a store that lists nothing though it holds the catalog can't say which names are free:
    // then what is written here is not removed on failure either (it may not be new)
    let knows_its_files = !held.is_empty();
    taken.extend(held.into_iter().filter(|n| valid_name(n)));
    let mut out = Written { segments: Vec::new(), written: 0, bytes: 0, obsolete: Vec::new(), new: Vec::new(), changed: Vec::new() };
    // the snapshot's segments in order: the ones kept, and a place for each one to write
    let mut listed: Vec<Option<Segment>> = Vec::new();
    let mut pieces: Vec<(usize, String, Piece)> = Vec::new();
    for step in plan(segments, baseline, photos, &size) {
        match step {
            Step::Keep(segment) => listed.push(Some(segment)),
            Step::Changed(lo, hi) => out.changed.push((lo, hi)),
            Step::Write(piece) => {
                let name = fresh_name(piece.first, seq, &taken);
                taken.insert(name.clone());
                out.new.push(name.clone());
                pieces.push((listed.len(), name, piece));
                listed.push(None);
            }
        }
    }
    let result = (|| {
        for ((slot, _, _), segment) in pieces.iter().zip(write_pieces(store, version, &pieces, photos)) {
            let segment = segment?;
            out.written += 1;
            out.bytes += segment.bytes;
            if let Some(place) = listed.get_mut(*slot) {
                *place = Some(segment);
            }
        }
        out.segments = listed.into_iter().flatten().collect();
        // the new files are in the folder for good before anything lists them
        store.sync()?;
        check_list(&out.segments).map_err(corrupt)?;
        if out.segments.iter().map(|s| s.photos).sum::<u64>() != photos.len() as u64 {
            return Err(corrupt("the segments don't hold every photo".into()));
        }
        if verify {
            // one segment at a time (not a second copy of every photo in memory at once)
            for (k, segment) in out.segments.iter().enumerate() {
                let read = read_segment(store, &out.segments, k)?;
                let (lo, hi) = range(&out.segments, k);
                let mut held = span(photos, lo, hi);
                let same = read.iter().all(|(a, p)| {
                    held.next()
                        .is_some_and(|(b, q)| a == b && p == q && serde_json::to_string(p.as_ref()).ok() == serde_json::to_string(q.as_ref()).ok())
                }) && held.next().is_none();
                if !same {
                    return Err(corrupt(format!("{} doesn't read back as the catalog's photos", segment.name)));
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        if knows_its_files {
            discard(store, &out.new);
        }
        return Err(e);
    }
    let kept: std::collections::BTreeSet<&str> = out.segments.iter().map(|s| s.name.as_str()).collect();
    out.obsolete = segments.iter().filter(|s| !kept.contains(s.name.as_str())).map(|s| s.name.clone()).collect();
    Ok(out)
}

/// Remove segment files (ones that are not, or no longer, listed). Failing to is not an error:
/// what stays is removed when the library is next opened.
pub(crate) fn discard(store: &mut dyn Store, names: &[String]) {
    for name in names {
        if valid_name(name.strip_suffix(".tmp").unwrap_or(name))
            && let Err(e) = store.remove(name)
        {
            if e.kind() == std::io::ErrorKind::Unsupported {
                // said once: such a store keeps every segment file it was ever given
                static SAID: std::sync::Once = std::sync::Once::new();
                SAID.call_once(|| log::warn!("catalog: this store can't remove files; segment files that are no longer used stay in it"));
            } else {
                log::warn!("catalog: can't remove {name}: {e}");
            }
        }
    }
}

/// How many segment files the store holds. A store that can't be listed is an error, not "none"
/// (it may hold a catalog's photos).
pub(crate) fn files(store: &mut dyn Store) -> std::io::Result<usize> {
    Ok(store.list()?.iter().filter(|n| valid_name(n)).count())
}

/// Whether the files `names` are all in the store (true where it can't say: it lists nothing).
pub(crate) fn present(store: &mut dyn Store, names: &[String]) -> bool {
    match store.list() {
        Ok(held) if !held.is_empty() => {
            let held: std::collections::BTreeSet<&str> = held.iter().map(String::as_str).collect();
            names.iter().all(|n| held.contains(n.as_str()))
        }
        _ => true,
    }
}

/// Remove the segment files in the library folder that `segments` doesn't list.
pub(crate) fn discard_unlisted(store: &mut dyn Store, segments: &[Segment]) {
    let listed: std::collections::BTreeSet<&str> = segments.iter().map(|s| s.name.as_str()).collect();
    let unlisted: Vec<String> = match store.list() {
        // (and what a crash left of one being written: `<name>.tmp`)
        Ok(names) => names.into_iter().filter(|n| valid_name(n.strip_suffix(".tmp").unwrap_or(n)) && !listed.contains(n.as_str())).collect(),
        Err(e) => {
            log::warn!("catalog: can't list the library's files: {e}");
            return;
        }
    };
    discard(store, &unlisted);
}

/// A segment list a snapshot may state: this module's file names, each once, in id order.
pub(crate) fn check_list(segments: &[Segment]) -> Result<(), String> {
    let mut names = std::collections::BTreeSet::new();
    let mut last: Option<PhotoId> = None;
    for s in segments {
        if !valid_name(&s.name) {
            return Err(format!("{:?} is not a segment file", s.name));
        }
        if !names.insert(s.name.as_str()) {
            return Err(format!("{} is listed twice", s.name));
        }
        if last.is_some_and(|l| s.first <= l) {
            return Err(format!("{} is out of order", s.name));
        }
        last = Some(s.first);
    }
    Ok(())
}

#[derive(Deserialize)]
struct SegmentFile {
    format: String,
    photos: Photos,
}

/// Read and check one segment.
fn read_segment(store: &mut dyn Store, segments: &[Segment], k: usize) -> std::io::Result<Photos> {
    let Some(segment) = segments.get(k) else { return Ok(Photos::new()) };
    let name = &segment.name;
    let bytes =
        store.read(name).map_err(|e| std::io::Error::new(e.kind(), format!("{name}: {e}")))?.ok_or_else(|| corrupt(format!("{name} is missing")))?;
    if bytes.len() as u64 != segment.bytes || crc32fast::hash(&bytes) != segment.crc {
        return Err(corrupt(format!("{name} is damaged ({} bytes where the snapshot lists {})", bytes.len(), segment.bytes)));
    }
    let file: SegmentFile = serde_json::from_slice(&bytes).map_err(|e| corrupt(format!("{name}: {e}")))?;
    if file.format != SEGMENT_FORMAT {
        return Err(corrupt(format!("{name} is not a catalog segment (format {:?})", file.format)));
    }
    if file.photos.len() as u64 != segment.photos {
        return Err(corrupt(format!("{name} holds {} photos where the snapshot lists {}", file.photos.len(), segment.photos)));
    }
    let next = segments.get(k + 1).map(|s| s.first);
    if file.photos.keys().any(|id| *id < segment.first || next.is_some_and(|n| *id >= n)) {
        return Err(corrupt(format!("{name} holds photos of another segment")));
    }
    Ok(file.photos)
}

/// Further handles on `store` to read `count` segments with at once (none when it gives none:
/// see [`Store::background_writer`]).
pub(crate) fn readers(store: &dyn Store, count: usize) -> Vec<Box<dyn Store>> {
    let wanted = std::thread::available_parallelism().map_or(1, |n| n.get()).min(LOAD_THREADS).min(count / 2);
    let mut handles = Vec::new();
    while handles.len() < wanted {
        let Some(h) = store.background_writer() else { break };
        handles.push(h);
    }
    handles
}

/// Read the photos of `segments`: on the threads of `handles` (see [`readers`]) when there are
/// two or more, else one after the other. Any segment that can't be read as listed is an error
/// naming it.
pub(crate) fn load(store: &mut dyn Store, handles: Vec<Box<dyn Store>>, segments: &[Segment]) -> std::io::Result<Photos> {
    check_list(segments).map_err(corrupt)?;
    let mut parts: Vec<Option<std::io::Result<Photos>>> = Vec::new();
    parts.resize_with(segments.len(), || None);
    if handles.len() < 2 {
        for (k, part) in parts.iter_mut().enumerate() {
            *part = Some(read_segment(store, segments, k));
        }
    } else {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let done: std::sync::Mutex<Vec<(usize, std::io::Result<Photos>)>> = std::sync::Mutex::new(Vec::new());
        // (the library's own defaults, where its records are read over them: on every thread)
        let over = crate::stored::reading_now();
        let work = |store: &mut dyn Store| {
            let _over = crate::stored::reading_over(over.clone());
            loop {
                let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if k >= segments.len() {
                    break;
                }
                let part = read_segment(store, segments, k);
                done.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((k, part));
            }
        };
        together(handles, store, &work);
        for (k, part) in done.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner) {
            if let Some(slot) = parts.get_mut(k) {
                *slot = Some(part);
            }
        }
    }
    let mut photos = Photos::new();
    for (segment, part) in segments.iter().zip(parts) {
        // (a thread that died leaves its segment unread)
        let mut part = part.unwrap_or_else(|| Err(corrupt(format!("{} could not be read", segment.name))))?;
        photos.append(&mut part);
    }
    Ok(photos)
}

/// The index in `segments` of the segment that holds (or would hold) photo `id`.
pub(crate) fn segment_of(segments: &[Segment], id: PhotoId) -> Option<usize> {
    if segments.is_empty() {
        return None;
    }
    Some(segments.partition_point(|s| s.first <= id).saturating_sub(1))
}
