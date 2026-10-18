//! Reading the camera's focus point for photos already in the library (issue #1480).
//!
//! Import reads a photo's focus point ([`lightcraft_meta::focus`]); photos imported before that have none. This job
//! fills them in, cheaply enough to run over a whole library on a spinning disk: it opens only the photos from a body
//! whose focus data is decoded (by the camera the catalog stores, so nothing else is opened), and reads only the few
//! kilobytes of each file that hold the focus data ([`lightcraft_meta::focus::read_header`]: about 32 KB of a 75 MB α1
//! ARW), one file at a time on one worker thread.
//!
//! - **Commits** go in batches of [`COMMIT_EVERY`] photos (or every [`COMMIT_AFTER`]), as LightCraft's own bookkeeping
//!   ([`Session::apply_system`]): no undo step, no sidecar write (the focus point is never written to XMP). Only
//!   `meta.focus` changes, and only on a photo that still has none, still in the library at the same path.
//! - **"Looked, found nothing"** (manual focus, a centre-only Multi report) is kept beside the library in
//!   `focus-scanned.bin` (photo ids, appended to, headed by the decoder's tag): it is cheap to append, never touches the
//!   catalog, and a decoder that learns more bodies has another tag, so its record starts afresh and those photos are
//!   looked at again. A photo whose file can't be read (offline drive, file gone or replaced mid-read) is not recorded:
//!   it is tried again the next time the library opens.
//! - **Runs** off the UI thread with a row in the activity stack ("Reading focus points", n of N, ✕ stops it). The UI's
//!   per-frame `focus.pump` gives it the pace: nothing new while the user drags, types or scrolls, a short rest between
//!   files while they are around.
//! - **Starts** by itself once per library opening (from `focus.pump`) when photos are left to look at, and on demand
//!   with `photo.readFocusPoints`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, PhotoId, Source};
use lightcraft_meta::FocusPoint;
use lightcraft_meta::focus::HeaderRead;
use serde_json::{Value, json};

use crate::activity::{Cancel, TaskGuard};
use crate::faces_index::Scanned;
use crate::{Result, Session};

/// The record of photos looked at with nothing found, beside the library.
pub(crate) const RECORD_FILE: &str = "focus-scanned.bin";
const RECORD_MAGIC: [u8; 4] = *b"LCFP";
/// Photos committed at once.
pub(crate) const COMMIT_EVERY: usize = 200;
/// What has been read is committed at least this often, so points appear while a long run goes on.
const COMMIT_AFTER: Duration = Duration::from_secs(3);
/// The rest between files while the user is around (`light` pace): their own reads of the same disk go first.
const LIGHT_REST: Duration = Duration::from_millis(40);
/// How often a paused worker looks at its pace again.
const PAUSE_POLL: Duration = Duration::from_millis(50);
/// Results taken in per pump (the rest wait for the next frame).
const MAX_DRAIN: usize = 5000;

/// The file extensions read (Sony bodies also shoot HEIF, which carries no maker note this decoder reads).
const EXTENSIONS: [&str; 3] = ["arw", "jpg", "jpeg"];

/// How hard the worker may work, from what the user is doing (`focus.pump`'s `pace`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Pace {
    Pause = 0,
    Light = 1,
    Normal = 2,
}

impl Pace {
    pub fn parse(name: Option<&str>) -> Pace {
        match name {
            Some("pause") => Pace::Pause,
            Some("light") => Pace::Light,
            _ => Pace::Normal,
        }
    }
}

/// The focus point in the file at `path`, read from its header only.
pub fn read_focus(path: &str) -> std::result::Result<HeaderRead, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let len = file.metadata().map_err(|e| format!("{path}: {e}"))?.len();
    lightcraft_meta::focus::read_header(len, &mut |off, buf| {
        file.seek(SeekFrom::Start(off))?;
        file.read_exact(buf)
    })
    .map_err(|e| format!("{path}: {e}"))
}

/// What the worker says about one photo.
enum Outcome {
    /// The header was read: the point it holds, or none.
    Read { id: PhotoId, focus: Option<FocusPoint>, bytes: u64 },
    /// The file couldn't be read (offline, gone, changed under the read): tried again another time.
    Unavailable,
}

/// What a run did (the last one is kept for `focus.status` and `photo.readFocusPoints {wait}`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Summary {
    pub total: u64,
    pub done: u64,
    pub found: u64,
    pub none: u64,
    pub unavailable: u64,
    pub bytes: u64,
    pub seconds: f64,
    pub cancelled: bool,
}

impl Summary {
    fn json(&self) -> Value {
        json!({
            "total": self.total,
            "done": self.done,
            "found": self.found,
            "none": self.none,
            "unavailable": self.unavailable,
            "bytesRead": self.bytes,
            "seconds": self.seconds,
            "cancelled": self.cancelled,
        })
    }
}

struct Job {
    identity: Arc<()>,
    cancel: Arc<AtomicBool>,
    pace: Arc<AtomicU8>,
    rx: mpsc::Receiver<(Outcome, String)>,
    /// Its row in the activity stack (dropping the job removes it).
    guard: TaskGuard,
    /// The path each photo was read at: a photo moved or relinked since is left for another run.
    paths: HashMap<PhotoId, String>,
    pending: Vec<(PhotoId, Option<FocusPoint>)>,
    committed_at: Instant,
    started: Instant,
    summary: Summary,
}

impl Drop for Job {
    fn drop(&mut self) {
        // the worker stops before its next file (and when it can't hand a result over)
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The backfill's state in the session.
pub(crate) struct FocusScan {
    record: Scanned,
    /// The library opening the record and the automatic start belong to.
    library: Option<Arc<()>>,
    auto_checked: bool,
    job: Option<Job>,
    last: Option<Summary>,
    /// The pace the UI last gave (a job started between two pumps starts at it).
    pace: Pace,
}

impl Default for FocusScan {
    fn default() -> Self {
        FocusScan {
            record: Scanned::new(RECORD_MAGIC, &lightcraft_meta::focus::decoder_tag()),
            library: None,
            auto_checked: false,
            job: None,
            last: None,
            pace: Pace::Normal,
        }
    }
}

/// Follow the session's library: another one (or none) drops a running job (its results belong to the old library and
/// are read again there next time) and loads that library's record.
fn follow_library(s: &mut Session) {
    if s.focus_scan.library.as_ref().is_some_and(|l| Arc::ptr_eq(l, &s.library_identity)) {
        return;
    }
    s.focus_scan.job = None;
    s.focus_scan.auto_checked = false;
    let path = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join(RECORD_FILE));
    s.focus_scan.record.reset(path);
    s.focus_scan.library = Some(Arc::clone(&s.library_identity));
}

/// The photos to read: in the library, no focus point yet, from a body whose focus data is decoded, a raw or JPEG file;
/// with `ids`, those of them; without, every one not already looked at. In path order (neighbours on disk together).
fn candidates(s: &Session, ids: Option<&[PhotoId]>) -> Vec<(PhotoId, String)> {
    let wanted = |ph: &lightcraft_catalog::Photo| -> Option<(PhotoId, String)> {
        if !ph.in_library() || ph.meta.focus.is_some() || !lightcraft_meta::focus::camera_supported(&ph.meta.camera) {
            return None;
        }
        let Source::File { path } = &ph.source else { return None };
        let ext = std::path::Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
        EXTENSIONS.contains(&ext.as_str()).then(|| (ph.id, path.clone()))
    };
    let mut out: Vec<(PhotoId, String)> = match ids {
        Some(ids) => ids.iter().filter_map(|id| s.catalog.photo(*id)).filter_map(|ph| wanted(ph)).collect(),
        None => s.catalog.photos().filter(|ph| !s.focus_scan.record.contains(ph.id.0)).filter_map(|ph| wanted(ph)).collect(),
    };
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out.dedup_by_key(|c| c.0);
    out
}

/// Start reading `list` on a worker thread.
fn start(s: &mut Session, list: Vec<(PhotoId, String)>, pace: Pace) -> std::result::Result<(), String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let pace = Arc::new(AtomicU8::new(pace as u8));
    let (tx, rx) = mpsc::channel();
    let paths: HashMap<PhotoId, String> = list.iter().cloned().collect();
    let total = list.len() as u64;
    let (worker_cancel, worker_pace) = (cancel.clone(), pace.clone());
    std::thread::Builder::new()
        .name("focus-points".into())
        .spawn(move || {
            for (id, path) in list {
                loop {
                    if worker_cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    if worker_pace.load(Ordering::Relaxed) != Pace::Pause as u8 {
                        break;
                    }
                    std::thread::sleep(PAUSE_POLL);
                }
                if worker_pace.load(Ordering::Relaxed) == Pace::Light as u8 {
                    std::thread::sleep(LIGHT_REST);
                }
                // a decoder bug in one file is that file's problem, not the job's end
                let outcome = match crate::guard::catch("reading a focus point", || read_focus(&path)) {
                    Ok(Ok(r)) => Outcome::Read { id, focus: r.focus, bytes: r.bytes_read },
                    Ok(Err(e)) => {
                        log::debug!("focus point: {e}");
                        Outcome::Unavailable
                    }
                    Err(_) => Outcome::Unavailable,
                };
                let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if tx.send((outcome, name)).is_err() {
                    return;
                }
            }
        })
        .map_err(|e| format!("could not start reading focus points: {e}"))?;
    let guard = s.activity.start("focus", "Reading focus points", Cancel::Flag(cancel.clone()));
    guard.progress(0, total);
    let now = Instant::now();
    s.focus_scan.job = Some(Job {
        identity: Arc::clone(&s.library_identity),
        cancel,
        pace,
        rx,
        guard,
        paths,
        pending: Vec::new(),
        committed_at: now,
        started: now,
        summary: Summary { total, ..Default::default() },
    });
    Ok(())
}

/// Commit what has been read: the points found (one system op, no undo step, no sidecar write) and the photos with none
/// (to the record).
fn commit(s: &mut Session) {
    let Some(job) = s.focus_scan.job.as_mut() else { return };
    let pending = std::mem::take(&mut job.pending);
    job.committed_at = Instant::now();
    if !Arc::ptr_eq(&job.identity, &s.library_identity) {
        return;
    }
    let mut ops = Vec::new();
    let mut none = Vec::new();
    for (id, focus) in pending {
        let Some(ph) = s.catalog.photo(id) else { continue };
        let same_file = matches!((&ph.source, job.paths.get(&id)), (Source::File { path }, Some(read)) if path == read);
        // deleted, moved, relinked or given a point meanwhile: leave it
        if !same_file || !ph.in_library() || ph.meta.focus.is_some() {
            continue;
        }
        match focus {
            Some(f) => {
                let mut meta = ph.meta.clone();
                meta.focus = Some(f);
                ops.push(Op::SetMeta { id, meta: Box::new(meta) });
            }
            None => none.push(id.0),
        }
    }
    for id in none {
        s.focus_scan.record.insert(id);
    }
    if let Err(e) = s.focus_scan.record.save() {
        log::warn!("could not save the list of photos read for focus points: {e}");
    }
    if ops.is_empty() {
        return;
    }
    match s.apply_system(Op::Batch { ops }) {
        // the focus point is never written to XMP: no sidecar is rewritten for it
        Ok(()) => s.skip_auto_write = true,
        Err(e) => log::warn!("could not store focus points: {e}"),
    }
}

/// Take in what the worker has read, commit when a batch is due, and end the job when it is done or cancelled.
/// Returns whether a job is still running.
fn step(s: &mut Session, pace: Pace) -> bool {
    let Some(job) = s.focus_scan.job.as_mut() else { return false };
    job.pace.store(pace as u8, Ordering::Relaxed);
    let mut finished = job.cancel.load(Ordering::Relaxed);
    let mut last_name = None;
    for _ in 0..MAX_DRAIN {
        match job.rx.try_recv() {
            Ok((outcome, name)) => {
                job.summary.done += 1;
                last_name = Some(name);
                match outcome {
                    Outcome::Read { id, focus, bytes } => {
                        job.summary.bytes = job.summary.bytes.saturating_add(bytes);
                        if focus.is_some() {
                            job.summary.found += 1;
                        } else {
                            job.summary.none += 1;
                        }
                        job.pending.push((id, focus));
                    }
                    Outcome::Unavailable => job.summary.unavailable += 1,
                }
            }
            Err(mpsc::TryRecvError::Empty) => break,
            // the worker ended (all read, cancelled, or it could not go on)
            Err(mpsc::TryRecvError::Disconnected) => {
                finished = true;
                break;
            }
        }
    }
    finished |= job.summary.done >= job.summary.total;
    job.guard.progress(job.summary.done, job.summary.total);
    if let Some(name) = last_name {
        job.guard.detail(&name);
    }
    if finished || job.pending.len() >= COMMIT_EVERY || (!job.pending.is_empty() && job.committed_at.elapsed() >= COMMIT_AFTER) {
        commit(s);
    }
    if finished && let Some(job) = s.focus_scan.job.take() {
        let mut summary = job.summary.clone();
        summary.cancelled = job.cancel.load(Ordering::Relaxed) && summary.done < summary.total;
        summary.seconds = job.started.elapsed().as_secs_f64();
        log::info!(
            "focus points: read {} of {} photos ({} found, {} without, {} unavailable, {} KB) in {:.1} s",
            summary.done,
            summary.total,
            summary.found,
            summary.none,
            summary.unavailable,
            summary.bytes / 1024,
            summary.seconds
        );
        s.focus_scan.last = Some(summary);
        return false;
    }
    true
}

fn status(s: &Session) -> Value {
    let running = s.focus_scan.job.as_ref().map(|j| json!({"done": j.summary.done, "total": j.summary.total, "found": j.summary.found}));
    json!({"active": running.is_some(), "running": running, "last": s.focus_scan.last.as_ref().map(Summary::json), "lookedAt": s.focus_scan.record.len()})
}

/// `focus.pump {pace?}`: cheap to call every frame.
pub(crate) fn pump(s: &mut Session, p: &Value) -> Result<Value> {
    follow_library(s);
    let pace = Pace::parse(p.get("pace").and_then(Value::as_str));
    s.focus_scan.pace = pace;
    if !step(s, pace) && !s.focus_scan.auto_checked {
        // once per library opening: start by itself when photos are left to look at (a few KB each)
        s.focus_scan.auto_checked = true;
        let list = candidates(s, None);
        if !list.is_empty()
            && let Err(e) = start(s, list, pace)
        {
            log::warn!("{e}");
        }
    }
    Ok(status(s))
}

/// `photo.readFocusPoints {ids?, wait?}`.
pub(crate) fn read_points(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.readFocusPoints";
    follow_library(s);
    if s.focus_scan.job.is_some() {
        return Ok(json!({"queued": 0, "running": true, "status": status(s)}));
    }
    let ids: Option<Vec<PhotoId>> = match p.get("ids") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_array()
                .ok_or_else(|| crate::cmd::bad(C, "`ids` must be a list of photo ids"))?
                .iter()
                .map(|v| v.as_u64().map(PhotoId).ok_or_else(|| crate::cmd::bad(C, "`ids` must be a list of photo ids")))
                .collect::<Result<Vec<_>>>()?,
        ),
    };
    let list = candidates(s, ids.as_deref());
    let queued = list.len();
    if queued == 0 {
        return Ok(json!({"queued": 0, "running": false}));
    }
    let wait = p.get("wait").and_then(Value::as_bool) == Some(true);
    start(s, list, if wait { Pace::Normal } else { s.focus_scan.pace }).map_err(|e| crate::cmd::bad(C, e))?;
    // a run asked for counts as this library opening's automatic one
    s.focus_scan.auto_checked = true;
    if !wait {
        return Ok(json!({"queued": queued, "running": true}));
    }
    // a script's call: read them all now (the batches still commit as they come)
    while step(s, Pace::Normal) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let last = s.focus_scan.last.as_ref().map(Summary::json).unwrap_or(Value::Null);
    Ok(json!({"queued": queued, "running": false, "last": last}))
}

#[cfg(test)]
pub(crate) fn record_len(s: &Session) -> usize {
    s.focus_scan.record.len()
}

#[cfg(test)]
pub(crate) fn record_contains(s: &Session, id: PhotoId) -> bool {
    s.focus_scan.record.contains(id.0)
}
