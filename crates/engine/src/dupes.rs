//! Find Duplicate Photos (`library.findDuplicates`, `duplicates.pump`): photos that are the same picture — a file
//! imported from two places, an export kept beside its raw, a resized or re-saved copy — grouped, each group ranked
//! by quality so the best one is kept and the rest can go (Delete moves them to Recently Deleted, one undo step).
//!
//! - **Exact** duplicates share a content hash (the file's bytes) and need no decoding.
//! - **Visual** duplicates have the same aspect ratio and perceptual hashes within `distance` bits
//!   ([`cull::dct_hash`] and [`cull::diff_hash`], from the thumbnail-level source, the same picture the grid shows).
//!   The default distance is tight on purpose: a burst frame or the next shot of the scene is never a duplicate,
//!   only the same picture at another size, compression or with mild tone edits.
//! - **Quality**: a raw beats a rendered file, lossless beats lossy, more pixels, a larger file (less compression),
//!   then edits and rating, then the earlier import.
//! - **Runs** off the UI thread like the focus-point reader ([`crate::focus_scan`]): a worker decodes and hashes one
//!   photo at a time, paced by `duplicates.pump {pace}` (nothing new while the user drags, types or scrolls), with a
//!   row in the activity stack ("Finding duplicate photos", n of N, ✕ stops it). Hashes are kept by content key for
//!   the session, so a second run only decodes photos it hasn't seen. `wait: true` runs it to the end (scripts, MCP,
//!   tests).
//! - **Ends** by filtering the view to the groups (`only`, keepers first) and selecting every photo that isn't its
//!   group's keeper, so Delete removes them; Clear Filters brings the library back. No catalog change, no undo step.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use lightcraft_catalog::{MediaKind, Photo, PhotoId};
use lightcraft_pipeline::cull;
use serde_json::{Value, json};

use crate::activity::{Cancel, TaskGuard};
use crate::focus_scan::Pace;
use crate::media::{SourceLevel, SourceRef, content_key};
use crate::{Result, Session};

/// Bits two DCT hashes may differ in and still be the same picture (default of `distance`).
pub(crate) const DEFAULT_DISTANCE: u32 = 6;
/// The loosest `distance` accepted.
pub(crate) const MAX_DISTANCE: u32 = 16;
/// The difference hash may differ by this many bits more than `distance` (it is the looser of the two).
const DIFF_SLACK: u32 = 4;
/// Aspect ratios within this share of each other count as the same frame.
const ASPECT_TOLERANCE: f64 = 0.03;
/// Formats that keep every pixel: preferred over a lossy file of the same picture.
const LOSSLESS: [&str; 4] = ["DNG", "TIFF", "TIF", "PNG"];
/// The rest between photos while the user is around (`light` pace).
const LIGHT_REST: Duration = Duration::from_millis(40);
/// How often a paused worker looks at its pace again.
const PAUSE_POLL: Duration = Duration::from_millis(50);
/// Results taken in per pump (the rest wait for the next frame).
const MAX_DRAIN: usize = 5000;

/// A photo's perceptual signature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sig {
    pub dct: u64,
    pub diff: u64,
    pub aspect: f64,
}

/// The signature of a decoded picture.
pub(crate) fn sig_of(img: &lightcraft_raster::Rgb32f) -> Sig {
    Sig { dct: cull::dct_hash(img), diff: cull::diff_hash(img), aspect: img.width as f64 / img.height.max(1) as f64 }
}

#[derive(Clone, Copy, Debug)]
struct Options {
    distance: u32,
    filter: bool,
    select_extras: bool,
}

/// What the worker sends back for one photo: its content key, its signature (none when the file couldn't be read) and
/// its file name (the activity row's detail).
type Hashed = (String, Option<Sig>, String);

struct Job {
    identity: Arc<()>,
    cancel: Arc<AtomicBool>,
    pace: Arc<AtomicU8>,
    rx: mpsc::Receiver<Hashed>,
    /// Its row in the activity stack (dropping the job removes it).
    guard: TaskGuard,
    candidates: Vec<PhotoId>,
    options: Options,
    total: u64,
    done: u64,
    unreadable: u64,
    started: Instant,
}

impl Drop for Job {
    fn drop(&mut self) {
        // the worker stops before its next photo (and when it can't hand a result over)
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Find Duplicates' state in the session.
pub(crate) struct Dupes {
    /// Signatures by content key: a photo's hash never changes while its file doesn't.
    sigs: HashMap<String, Sig>,
    job: Option<Job>,
    /// The last run's result (`duplicates.pump` → `last`, `library.findDuplicates {wait}`).
    last: Option<Value>,
    /// The library opening the job belongs to.
    library: Option<Arc<()>>,
    /// The pace the UI last gave (a job started between two pumps starts at it).
    pace: Pace,
}

impl Default for Dupes {
    fn default() -> Self {
        Dupes { sigs: HashMap::new(), job: None, last: None, library: None, pace: Pace::Normal }
    }
}

/// Follow the session's library: another one (or none) drops a running job (its candidates belong to the old library)
/// and its result. The signatures stay: they are keyed by content, not by library.
fn follow_library(s: &mut Session) {
    if s.dupes.library.as_ref().is_some_and(|l| Arc::ptr_eq(l, &s.library_identity)) {
        return;
    }
    s.dupes.job = None;
    s.dupes.last = None;
    s.dupes.library = Some(Arc::clone(&s.library_identity));
}

/// The photos a run looks at: in the library, not a video, not a virtual copy (it shares its original's file); with
/// `ids`, those of them, else every one.
fn candidates(s: &Session, ids: Option<&[PhotoId]>) -> Vec<PhotoId> {
    let wanted = |p: &Photo| p.in_library() && !p.is_video() && p.copy_of.is_none();
    let mut out: Vec<PhotoId> = match ids {
        Some(ids) => ids.iter().filter_map(|id| s.catalog.photo(*id)).filter(|p| wanted(p)).map(|p| p.id).collect(),
        None => s.catalog.photos().filter(|p| wanted(p)).map(|p| p.id).collect(),
    };
    out.sort_unstable();
    out.dedup();
    out
}

fn options(p: &Value, c: &str) -> Result<Options> {
    let distance = match p.get("distance") {
        None | Some(Value::Null) => DEFAULT_DISTANCE,
        Some(v) => match v.as_u64() {
            Some(d) if d <= u64::from(MAX_DISTANCE) => d as u32,
            _ => return Err(crate::cmd::bad(c, format!("`distance` must be a whole number from 0 to {MAX_DISTANCE}"))),
        },
    };
    let flag = |key: &str, default: bool| -> Result<bool> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(crate::cmd::bad(c, format!("`{key}` must be true or false"))),
        }
    };
    Ok(Options { distance, filter: flag("filter", true)?, select_extras: flag("selectExtras", true)? })
}

/// Start hashing `todo` on a worker thread.
fn start(
    s: &mut Session,
    candidates: Vec<PhotoId>,
    todo: Vec<(String, SourceRef, String)>,
    options: Options,
    pace: Pace,
) -> std::result::Result<(), String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let pace = Arc::new(AtomicU8::new(pace as u8));
    let (tx, rx) = mpsc::channel();
    let total = todo.len() as u64;
    let (worker_cancel, worker_pace) = (cancel.clone(), pace.clone());
    std::thread::Builder::new()
        .name("find-duplicates".into())
        .spawn(move || {
            for (key, source, name) in todo {
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
                let sig = match crate::guard::catch("hashing a photo", || source.load()) {
                    Ok(Ok(img)) => Some(sig_of(&img)),
                    Ok(Err(e)) => {
                        log::debug!("find duplicates: {name}: {e}");
                        None
                    }
                    Err(_) => None,
                };
                if tx.send((key, sig, name)).is_err() {
                    return;
                }
            }
        })
        .map_err(|e| format!("could not start finding duplicates: {e}"))?;
    let guard = s.activity.start("duplicates", "Finding duplicate photos", Cancel::Flag(cancel.clone()));
    guard.progress(0, total);
    s.dupes.job = Some(Job {
        identity: Arc::clone(&s.library_identity),
        cancel,
        pace,
        rx,
        guard,
        candidates,
        options,
        total,
        done: 0,
        unreadable: 0,
        started: Instant::now(),
    });
    Ok(())
}

/// How a run went, for its result.
struct Run {
    decoded: u64,
    unreadable: u64,
    started: Instant,
    cancelled: bool,
}

/// Take in what the worker has hashed and, when it is done or stopped, group and show the result. Returns whether a
/// job is still running.
fn step(s: &mut Session, pace: Pace) -> bool {
    let Some(job) = s.dupes.job.as_mut() else { return false };
    job.pace.store(pace as u8, Ordering::Relaxed);
    let mut finished = job.cancel.load(Ordering::Relaxed);
    let mut last_name = None;
    for _ in 0..MAX_DRAIN {
        match job.rx.try_recv() {
            Ok((key, sig, name)) => {
                job.done += 1;
                last_name = Some(name);
                match sig {
                    Some(sig) => {
                        s.dupes.sigs.insert(key, sig);
                    }
                    None => job.unreadable += 1,
                }
            }
            Err(mpsc::TryRecvError::Empty) => break,
            // the worker ended (all hashed, cancelled, or it could not go on)
            Err(mpsc::TryRecvError::Disconnected) => {
                finished = true;
                break;
            }
        }
    }
    finished |= job.done >= job.total;
    job.guard.progress(job.done, job.total);
    if let Some(name) = last_name {
        job.guard.detail(&name);
    }
    if !finished {
        return true;
    }
    let Some(job) = s.dupes.job.take() else { return false };
    let cancelled = job.cancel.load(Ordering::Relaxed) && job.done < job.total;
    let run = Run { decoded: job.done, unreadable: job.unreadable, started: job.started, cancelled };
    // a job of another library (closed meanwhile) has nothing to show
    if Arc::ptr_eq(&job.identity, &s.library_identity) {
        let result = finish(s, &job.candidates, job.options, &run);
        s.dupes.last = Some(result);
    }
    false
}

/// Group `candidates`, show the groups (filter and selection) unless the run was stopped, and describe the result.
fn finish(s: &mut Session, candidates: &[PhotoId], o: Options, run: &Run) -> Value {
    let groups = group(s, candidates, o.distance);
    let mut only = Vec::new();
    let mut extras = Vec::new();
    for g in &groups {
        only.push(g.keep);
        only.extend(g.extras.iter().map(|e| e.0));
        extras.extend(g.extras.iter().map(|e| e.0));
    }
    if !run.cancelled {
        if o.filter {
            s.shelved_filter = None;
            s.filter.only.clone_from(&only);
            // an empty `only` means "no constraint": no duplicates is its own state that shows no photos
            s.filter.only_none = only.is_empty();
        }
        if o.select_extras && !extras.is_empty() {
            s.selection.ids.clone_from(&extras);
            s.selection.active = extras.first().copied();
        }
    }
    json!({
        "groups": groups.iter().map(Group::json).collect::<Vec<_>>(),
        "photos": candidates.len(),
        "decoded": run.decoded,
        "unreadable": run.unreadable,
        "duplicates": extras.len(),
        "seconds": run.started.elapsed().as_secs_f64(),
        "cancelled": run.cancelled,
    })
}

/// What ranks a photo within its group (the facts the catalog has; no file is opened).
struct Row {
    id: PhotoId,
    hash: Option<String>,
    sig: Option<Sig>,
    raw: bool,
    lossless: bool,
    pixels: u64,
    size: u64,
    edited: bool,
    rating: u8,
    format: String,
    width: u32,
    height: u32,
}

impl Row {
    fn of(p: &Photo, sig: Option<Sig>) -> Row {
        Row {
            id: p.id,
            hash: p.content_hash.clone(),
            sig,
            raw: p.kind == MediaKind::Raw,
            lossless: LOSSLESS.iter().any(|f| p.format.eq_ignore_ascii_case(f)),
            pixels: u64::from(p.width) * u64::from(p.height),
            size: p.file_size,
            edited: p.is_edited(),
            rating: p.rating,
            format: p.format.clone(),
            width: p.width,
            height: p.height,
        }
    }

    /// Higher is better; the earlier import wins a tie.
    fn quality(&self) -> (bool, bool, u64, u64, bool, u8, std::cmp::Reverse<u64>) {
        (self.raw, self.lossless, self.pixels, self.size, self.edited, self.rating, std::cmp::Reverse(self.id.0))
    }

    /// Why `self` is kept over `other` (a short English phrase for agents and the report).
    fn why_over(&self, other: &Row) -> String {
        if self.raw && !other.raw {
            format!("raw beats {}", other.format)
        } else if self.lossless && !other.lossless {
            format!("lossless beats {}", other.format)
        } else if self.pixels > other.pixels {
            format!("{}×{} beats {}×{}", self.width, self.height, other.width, other.height)
        } else if self.size > other.size {
            format!("{} bytes beats {} (less compression)", self.size, other.size)
        } else if self.edited && !other.edited {
            "has edits".into()
        } else if self.rating > other.rating {
            "higher rating".into()
        } else {
            "the same; the earlier import is kept".into()
        }
    }
}

struct Group {
    exact: bool,
    keep: PhotoId,
    /// The photos to remove, best first, each with why the keeper beats it.
    extras: Vec<(PhotoId, String)>,
    /// The most bits two members' DCT hashes differ in (0 for exact groups).
    max_distance: u32,
}

impl Group {
    fn json(&self) -> Value {
        let mut photos = vec![json!({"id": self.keep.0, "keep": true})];
        photos.extend(self.extras.iter().map(|(id, why)| json!({"id": id.0, "keep": false, "why": why})));
        json!({
            "exact": self.exact,
            "keep": self.keep.0,
            "delete": self.extras.iter().map(|e| e.0.0).collect::<Vec<_>>(),
            "maxDistance": self.max_distance,
            "photos": photos,
        })
    }
}

fn root(parent: &mut [usize], mut i: usize) -> usize {
    // bounded: every step moves to a smaller-or-equal index chain and the slice is finite
    for _ in 0..parent.len() {
        match parent.get(i) {
            Some(&p) if p != i => i = p,
            _ => return i,
        }
    }
    i
}

fn unite(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (root(parent, a), root(parent, b));
    if ra != rb
        && let Some(slot) = parent.get_mut(rb)
    {
        *slot = ra;
    }
}

/// Same picture: both hashes within reach and the same frame.
fn same_picture(a: &Sig, b: &Sig, distance: u32) -> bool {
    (a.aspect - b.aspect).abs() <= ASPECT_TOLERANCE * a.aspect.max(b.aspect).max(1e-6)
        && cull::hamming(a.dct, b.dct) <= distance
        && cull::hamming(a.diff, b.diff) <= distance.saturating_add(DIFF_SLACK)
}

/// Pairs within `distance` bits without comparing every pair: a DCT hash is cut into `distance + 1` chunks, and two
/// hashes that differ in at most `distance` bits agree in at least one chunk (pigeonhole), so only photos sharing a
/// chunk's value are compared in full.
fn unite_near(rows: &[Row], parent: &mut [usize], distance: u32) {
    let chunks = (distance as usize + 1).min(64);
    let width = 64 / chunks;
    for k in 0..chunks {
        let shift = k * width;
        let bits = if k + 1 == chunks { 64 - shift } else { width };
        let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
        let mut buckets: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, r) in rows.iter().enumerate() {
            if let Some(sig) = r.sig {
                buckets.entry((sig.dct >> shift) & mask).or_default().push(i);
            }
        }
        for idxs in buckets.values() {
            for (n, &i) in idxs.iter().enumerate() {
                let Some(a) = rows.get(i).and_then(|r| r.sig) else { continue };
                for &j in idxs.iter().skip(n + 1) {
                    let Some(b) = rows.get(j).and_then(|r| r.sig) else { continue };
                    if same_picture(&a, &b, distance) {
                        unite(parent, i, j);
                    }
                }
            }
        }
    }
}

/// The duplicate groups among `candidates` still in the library, largest first.
fn group(s: &Session, candidates: &[PhotoId], distance: u32) -> Vec<Group> {
    let rows: Vec<Row> = candidates
        .iter()
        .filter_map(|id| s.catalog.photo(*id))
        .filter(|p| p.in_library())
        .map(|p| Row::of(p, s.dupes.sigs.get(&content_key(p)).copied()))
        .collect();
    let mut parent: Vec<usize> = (0..rows.len()).collect();
    // exact: the same bytes
    let mut first_of: HashMap<&str, usize> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if let Some(h) = r.hash.as_deref() {
            match first_of.get(h) {
                Some(&f) => unite(&mut parent, f, i),
                None => {
                    first_of.insert(h, i);
                }
            }
        }
    }
    unite_near(&rows, &mut parent, distance);
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..rows.len() {
        members.entry(root(&mut parent, i)).or_default().push(i);
    }
    let mut groups: Vec<Group> = members
        .into_values()
        .filter(|m| m.len() > 1)
        .map(|mut m| {
            m.sort_by(|a, b| {
                let (qa, qb) = (rows.get(*a).map(Row::quality), rows.get(*b).map(Row::quality));
                qb.cmp(&qa)
            });
            let rs: Vec<&Row> = m.iter().filter_map(|i| rows.get(*i)).collect();
            let keeper = rs.first().copied();
            let exact = keeper.is_some_and(|k| k.hash.is_some() && rs.iter().all(|r| r.hash == k.hash));
            let mut max_distance = 0;
            for (n, a) in rs.iter().enumerate() {
                for b in rs.iter().skip(n + 1) {
                    if let (Some(sa), Some(sb)) = (a.sig, b.sig) {
                        max_distance = max_distance.max(cull::hamming(sa.dct, sb.dct));
                    }
                }
            }
            Group {
                exact,
                keep: keeper.map_or(PhotoId(0), |k| k.id),
                extras: keeper.map(|k| rs.iter().skip(1).map(|r| (r.id, k.why_over(r))).collect()).unwrap_or_default(),
                max_distance,
            }
        })
        .collect();
    groups.sort_by_key(|g| (std::cmp::Reverse(g.extras.len()), g.keep));
    groups
}

fn status(s: &Session) -> Value {
    let running = s.dupes.job.as_ref().map(|j| json!({"done": j.done, "total": j.total}));
    json!({
        "active": running.is_some(),
        "running": running,
        "last": s.dupes.last.clone().unwrap_or(Value::Null),
        "hashed": s.dupes.sigs.len(),
    })
}

/// `duplicates.pump {pace?}`: cheap to call every frame.
pub(crate) fn pump(s: &mut Session, p: &Value) -> Result<Value> {
    follow_library(s);
    let pace = Pace::parse(p.get("pace").and_then(Value::as_str));
    s.dupes.pace = pace;
    step(s, pace);
    Ok(status(s))
}

/// `library.findDuplicates {ids?, distance?, filter?, selectExtras?, wait?}`.
pub(crate) fn find(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.findDuplicates";
    follow_library(s);
    if s.dupes.job.is_some() {
        return Ok(json!({"queued": 0, "running": true, "status": status(s)}));
    }
    let ids = crate::cmd::photo_ids(p, C)?;
    let options = options(p, C)?;
    let wait = p.get("wait").and_then(Value::as_bool) == Some(true);
    let candidates = candidates(s, ids.as_deref());
    // what still needs decoding (the rest is known by content)
    let mut todo: Vec<(String, SourceRef, String)> = Vec::new();
    for id in &candidates {
        let Some(ph) = s.catalog.photo(*id).cloned() else { continue };
        let key = content_key(&ph);
        if s.dupes.sigs.contains_key(&key) {
            continue;
        }
        let source = s.media.source_ref(&ph, SourceLevel::Thumb);
        todo.push((key, source, ph.file_name.clone()));
    }
    let queued = todo.len();
    if todo.is_empty() {
        let run = Run { decoded: 0, unreadable: 0, started: Instant::now(), cancelled: false };
        let result = finish(s, &candidates, options, &run);
        s.dupes.last = Some(result.clone());
        return Ok(json!({"queued": 0, "running": false, "last": result}));
    }
    start(s, candidates, todo, options, if wait { Pace::Normal } else { s.dupes.pace }).map_err(|e| crate::cmd::bad(C, e))?;
    if !wait {
        return Ok(json!({"queued": queued, "running": true}));
    }
    // a script's call: hash them all now
    while step(s, Pace::Normal) {
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(json!({"queued": queued, "running": false, "last": s.dupes.last.clone().unwrap_or(Value::Null)}))
}
