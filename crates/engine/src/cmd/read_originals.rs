//! Originals read after their records are committed (issue #1303): a Lightroom catalog import adds
//! its photos from the catalog alone, each marked "original not yet read" (no content hash), so a
//! large archive on slow or external drives is browsable in minutes instead of days. The originals
//! are then read here, in the background and in batches, filling in only what needs the file: the
//! content hash, file size and dimensions, empty camera fields, embedded lens corrections and the
//! as-shot white balance — exactly what Reload from Disk would give the photo
//! ([`crate::cmd::convert::reload_ops`]). Ratings, keywords, develop settings and history stay as
//! the catalog gave them.
//!
//! The marker is the absence of a content hash, so the work resumes by itself: a photo whose file
//! was read has a hash and is not listed again; a cancelled, interrupted or crashed run leaves the
//! rest listed for the next one. A file that can't be read (an offline drive) changes nothing and
//! is tried again when the library next opens.
//!
//! The work is split like [`crate::cmd::recheck`]: [`unread`] lists photos (no file-system calls),
//! [`probe_cancellable_with_progress`] reads files without the session, and [`apply`] applies the
//! results as the app's own bookkeeping (journaled, not an undo step). Agents and the CLI use
//! `library.readOriginals`, which does all three at once.

use std::collections::HashSet;

use lcc_catalog::{Catalog, MediaKind, Op, Photo, PhotoId};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::media::{FileProbe, ProbeInfo};
use crate::{Result, Session};

/// A photo whose original is still to be read, and the file it named when it was listed (so a
/// result for a photo relinked meanwhile is dropped).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unread {
    pub id: PhotoId,
    pub path: String,
}

impl Unread {
    /// The photo is still unread and names the same file.
    fn current(&self, p: &Photo) -> bool {
        p.content_hash.is_none() && crate::cmd::missing::checked_path(p) == Some(self.path.as_str())
    }
}

/// Whether a library photo's original is still to be read: a file of its own (not a virtual copy,
/// which follows its master), no content hash, and a still photo (a video's probe need not hash it,
/// so it would be listed forever).
pub fn is_unread(p: &Photo) -> bool {
    p.copy_of.is_none() && p.content_hash.is_none() && p.kind != MediaKind::Video && p.preview_only.is_none()
}

/// Up to `limit` library photos whose originals are still to be read, leaving out `skip` (those
/// already tried this session), in id order (the order an import added them). No file-system calls.
pub fn unread(cat: &Catalog, skip: &HashSet<PhotoId>, limit: usize) -> Vec<Unread> {
    let mut out: Vec<Unread> = cat
        .photos()
        .filter(|p| is_unread(p) && !skip.contains(&p.id))
        .filter_map(|p| crate::cmd::missing::checked_path(p).map(|path| Unread { id: p.id, path: path.to_string() }))
        .collect();
    out.sort_by_key(|u| u.id.0);
    out.truncate(limit);
    out
}

/// How many library photos' originals are still to be read. No file-system calls.
pub fn count_unread(cat: &Catalog) -> usize {
    cat.photos().filter(|p| is_unread(p) && crate::cmd::missing::checked_path(p).is_some()).count()
}

/// The session's file probe; `None` when the session has no file access (a demo or browser
/// session), where nothing can be read.
pub fn file_probe(s: &Session) -> Option<FileProbe> {
    s.media.file_probe.clone()
}

/// Read each listed file, reporting completed files and stopping before starting more after
/// `cancel` is set. Files not reached keep an error result, which [`apply`] leaves unchanged.
pub fn probe_cancellable_with_progress(
    probe: &FileProbe,
    unread: Vec<Unread>,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &dyn Fn(usize),
) -> Vec<(Unread, std::result::Result<ProbeInfo, String>)> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let paths: Vec<String> = unread.iter().map(|u| u.path.clone()).collect();
    let done = AtomicUsize::new(0);
    #[cfg(not(target_arch = "wasm32"))]
    let probed = std::thread::scope(|scope| {
        let worker = scope.spawn(|| crate::import::probe_all(Some(probe), &paths, &done, cancel));
        while !worker.is_finished() {
            progress(done.load(Ordering::Relaxed));
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        worker.join().unwrap_or_else(|_| paths.iter().map(|path| Err(format!("{path}: could not be read"))).collect())
    });
    #[cfg(target_arch = "wasm32")]
    let probed = crate::import::probe_all(Some(probe), &paths, &done, cancel);
    progress(done.load(Ordering::Relaxed));
    unread.into_iter().zip(probed).collect()
}

/// What [`apply`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadOriginals {
    /// Photos brought up to date with their file (virtual copies included).
    pub updated: Vec<PhotoId>,
    /// Listed photos whose file could not be read; they stay unread.
    pub unreadable: usize,
    /// `(read, already)`: a photo whose file, once read, has the same content as another library
    /// photo (an ordinary import would have skipped it as a duplicate). Both records are kept, since
    /// the newly read one may carry the catalog's edits; the pair is reported so the user can choose.
    pub duplicates: Vec<(PhotoId, PhotoId)>,
}

/// Apply the probes: each photo gets the ops Reload would give it, all in one batch that is
/// journaled but isn't an undo step. A photo that changed since it was listed (relinked, read by
/// another run, removed) is skipped.
pub fn apply(s: &mut Session, results: Vec<(Unread, std::result::Result<ProbeInfo, String>)>) -> Result<ReadOriginals> {
    let mut ops = Vec::new();
    let mut out = ReadOriginals::default();
    let mut hashes: std::collections::HashMap<String, PhotoId> = s
        .catalog
        .photos()
        .filter(|p| p.copy_of.is_none())
        .filter_map(|p| p.content_hash.as_deref().map(|h| (crate::sync::base_hash(h).to_string(), p.id)))
        .collect();
    for (item, info) in results {
        let Ok(info) = info else {
            out.unreadable = out.unreadable.saturating_add(1);
            continue;
        };
        if !s.catalog.photo(item.id).is_some_and(|p| item.current(p)) {
            continue;
        }
        if let Some(h) = info.content_hash.as_deref() {
            let key = crate::sync::base_hash(h).to_string();
            match hashes.get(&key) {
                Some(other) if *other != item.id => out.duplicates.push((item.id, *other)),
                _ => {
                    hashes.insert(key, item.id);
                }
            }
        }
        crate::cmd::convert::reload_ops(&s.catalog, item.id, &info, &mut ops, &mut out.updated);
    }
    if !ops.is_empty() {
        s.apply_system(Op::Batch { ops })?;
    }
    for id in &out.updated {
        s.media.forget(*id);
    }
    Ok(out)
}

/// Batch size of `library.readOriginals`: one journaled batch per this many files, so an interrupted
/// run keeps what it read.
const BATCH: usize = 256;

/// Files `library.readOriginals` reads when no `limit` is given: the command runs on the session's
/// thread, so over `--connect` an unbounded run would hold the app for a large library. Callers
/// loop on `remaining`.
const DEFAULT_LIMIT: usize = 1024;

fn read_originals(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.readOriginals";
    // without file access every probe "succeeds" with nothing known
    let fp = file_probe(s).ok_or_else(|| bad(C, "this session has no file access"))?;
    let limit = match p.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(C, "`limit` must be a whole number ≥ 0"))?,
    };
    let mut tried = HashSet::new();
    let (mut updated, mut unreadable, mut checked, mut duplicates) = (Vec::new(), 0usize, 0usize, Vec::new());
    let cancel = std::sync::atomic::AtomicBool::new(false);
    while checked < limit {
        let batch = unread(&s.catalog, &tried, BATCH.min(limit - checked));
        if batch.is_empty() {
            break;
        }
        checked = checked.saturating_add(batch.len());
        tried.extend(batch.iter().map(|u| u.id));
        let r = apply(s, probe_cancellable_with_progress(&fp, batch, &cancel, &|_| {}))?;
        updated.extend(r.updated);
        unreadable = unreadable.saturating_add(r.unreadable);
        duplicates.extend(r.duplicates);
    }
    Ok(json!({
        "checked": checked,
        "updated": updated.iter().map(|i| i.0).collect::<Vec<_>>(),
        "unreadable": unreadable,
        "duplicates": duplicates.iter().map(|(a, b)| json!({"id": a.0, "sameAs": b.0})).collect::<Vec<_>>(),
        "remaining": count_unread(&s.catalog),
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "library.readOriginals",
        "Read Originals Not Yet Read",
        [],
        None,
        "{limit?: int}: read the originals of photos added without reading their file (a Lightroom catalog import adds its photos from the catalog first; the app then reads their originals in the background), at most `limit` of them (default 1024; call again while `remaining` > 0), and fill in what needs the file as Reload from Disk would: content hash, file size, dimensions, empty camera fields, embedded lens corrections, as-shot white balance; not an undo step, ratings, keywords, develop settings and history are kept; files that can't be read (an offline drive) are left for next time; a read file whose content matches another library photo is kept and reported in `duplicates` → {checked, updated: [ids], unreadable, duplicates: [{id, sameAs}], remaining}",
        always,
        read_originals
    )]
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use lcc_catalog::Source;

    fn unread_photo(id: u64, path: &str) -> Photo {
        let mut p = Photo::new(PhotoId(id), Source::File { path: path.into() }, "a.NEF", "NEF", 30, 20, "2026-01-01");
        p.kind = MediaKind::Raw;
        p.rating = 4;
        p
    }

    fn session_with(photos: Vec<Photo>, probe: FileProbe) -> Session {
        let mut s = Session::new();
        for p in photos {
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        s.media.file_probe = Some(probe);
        s
    }

    fn probe_hashing(calls: Arc<AtomicUsize>) -> FileProbe {
        Arc::new(move |path: &str| {
            calls.fetch_add(1, Ordering::Relaxed);
            if path.contains("offline") {
                return Err(format!("{path}: not found"));
            }
            Ok(ProbeInfo {
                width: 60,
                height: 40,
                file_size: 1234,
                format: "NEF".into(),
                content_hash: Some(format!("hash-{path}")),
                ..Default::default()
            })
        })
    }

    #[test]
    fn reading_fills_file_facts_and_keeps_catalog_data_then_lists_nothing_more() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut s = session_with(vec![unread_photo(1, "/x/a.NEF"), unread_photo(2, "/offline/b.NEF")], probe_hashing(calls.clone()));
        assert_eq!(count_unread(&s.catalog), 2);
        let r = s.execute("library.readOriginals", &json!({})).unwrap();
        assert_eq!(r["updated"], json!([1]), "{r}");
        assert_eq!(r["unreadable"], 1, "{r}");
        let p = s.catalog.photo(PhotoId(1)).unwrap();
        assert_eq!(p.content_hash.as_deref(), Some("hash-/x/a.NEF"));
        assert_eq!((p.width, p.height, p.file_size), (60, 40, 1234));
        assert_eq!(p.rating, 4, "catalog data is kept");
        assert_eq!(r["remaining"], 1, "the offline original stays unread for next time: {r}");
        assert!(s.undo.is_empty(), "reading originals is not an undo step");
        // a second run re-reads only what is still unread
        let before = calls.load(Ordering::Relaxed);
        s.execute("library.readOriginals", &json!({})).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed) - before, 1);
    }

    #[test]
    fn a_read_original_matching_another_photo_is_reported_as_a_duplicate_and_kept() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut have = unread_photo(1, "/x/a.NEF");
        have.content_hash = Some("hash-/x/a.NEF".into());
        // the same file under another spelling hashes the same
        let probe: FileProbe = Arc::new(move |_path: &str| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(ProbeInfo { width: 6, height: 4, format: "NEF".into(), content_hash: Some("hash-/x/a.NEF".into()), ..Default::default() })
        });
        let mut s = session_with(vec![have, unread_photo(2, "/y/a.NEF")], probe);
        let r = s.execute("library.readOriginals", &json!({})).unwrap();
        assert_eq!(r["duplicates"], json!([{"id": 2, "sameAs": 1}]), "{r}");
        assert_eq!(s.catalog.len(), 2, "both records are kept");
        assert_eq!(r["remaining"], 0, "the duplicate is read, not listed again: {r}");
    }

    #[test]
    fn without_a_limit_a_run_stops_at_the_default_and_reports_the_rest() {
        let calls = Arc::new(AtomicUsize::new(0));
        let n = DEFAULT_LIMIT as u64 + 3;
        let photos = (1..=n).map(|i| unread_photo(i, &format!("/x/{i}.NEF"))).collect();
        let mut s = session_with(photos, probe_hashing(calls));
        let r = s.execute("library.readOriginals", &json!({})).unwrap();
        assert_eq!(r["checked"], DEFAULT_LIMIT, "{}", r["checked"]);
        assert_eq!(r["remaining"], 3);
    }

    #[test]
    fn a_limit_reads_part_and_the_rest_resumes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let photos = (1..=5).map(|i| unread_photo(i, &format!("/x/{i}.NEF"))).collect();
        let mut s = session_with(photos, probe_hashing(calls));
        let r = s.execute("library.readOriginals", &json!({"limit": 2})).unwrap();
        assert_eq!(r["checked"], 2);
        assert_eq!(r["remaining"], 3);
        let r = s.execute("library.readOriginals", &json!({})).unwrap();
        assert_eq!(r["remaining"], 0, "{r}");
    }

    #[test]
    fn hostile_params_and_photos_are_refused_or_skipped_without_panicking() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut copy = unread_photo(2, "/x/a.NEF");
        copy.copy_of = Some(PhotoId(1));
        let mut video = unread_photo(3, "/x/v.mov");
        video.kind = MediaKind::Video;
        let mut s = session_with(vec![unread_photo(1, "/x/a.NEF"), copy, video, unread_photo(4, "")], probe_hashing(calls));
        for limit in [json!(-1), json!(1.5), json!("x"), json!(u64::MAX)] {
            let r = s.execute("library.readOriginals", &json!({ "limit": limit }));
            assert!(r.is_ok() || r.is_err(), "no panic for {limit}");
        }
        assert!(s.execute("library.readOriginals", &json!({"limit": -1})).is_err());
        let listed = unread(&s.catalog, &HashSet::new(), usize::MAX);
        assert!(listed.iter().all(|u| u.id != PhotoId(2) && u.id != PhotoId(3)), "copies and videos are not listed: {listed:?}");
        // a session without file access refuses instead of marking everything read
        let mut bare = Session::new();
        assert!(bare.execute("library.readOriginals", &json!({})).is_err());
    }

    #[test]
    fn a_result_for_a_photo_relinked_meanwhile_is_dropped_and_cancel_reads_nothing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let probe = probe_hashing(calls.clone());
        let mut s = session_with(vec![unread_photo(1, "/x/a.NEF")], probe.clone());
        let listed = unread(&s.catalog, &HashSet::new(), 10);
        let cancelled = probe_cancellable_with_progress(&probe, listed.clone(), &AtomicBool::new(true), &|_| {});
        assert_eq!(calls.load(Ordering::Relaxed), 0, "a cancelled run reads no file");
        assert!(apply(&mut s, cancelled).unwrap().updated.is_empty());
        let results = probe_cancellable_with_progress(&probe, listed, &AtomicBool::new(false), &|_| {});
        s.catalog
            .apply(Op::Relink { id: PhotoId(1), file_name: "a.NEF".into(), source: Source::File { path: "/y/a.NEF".into() }, format: None })
            .unwrap();
        assert!(apply(&mut s, results).unwrap().updated.is_empty(), "the relinked photo is not given another file's facts");
        assert!(s.catalog.photo(PhotoId(1)).unwrap().content_hash.is_none());
    }
}
