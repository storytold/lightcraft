//! Find Duplicate Photos (`library.findDuplicates`, `duplicates.pump`): photos that are the same picture — a file
//! imported from two places, an export kept beside its raw, a resized or re-saved copy — grouped, each group ranked
//! by quality so the best one is kept and the rest can go (Delete moves them to Recently Deleted, one undo step).
//!
//! - **Exact** duplicates share a content hash (the file's bytes) and need no picture.
//! - **Visual** duplicates have the same aspect ratio and both perceptual hashes within `distance` bits
//!   ([`cull::dct_hash_srgb8`] and [`cull::diff_hash_srgb8`] of the photo's grid thumbnail: the cached one where the
//!   grid has rendered it, else rendered now and cached, so the grid gains it too). A photo joins a group only when it
//!   is within `distance` of *every* member (complete linkage): a burst of near-identical frames can't chain into one
//!   group through its neighbours. `sameMoment` also asks for the same capture time where both photos have one: a
//!   copy keeps the original's, the next frame of a burst has its own.
//! - **Quality**: a raw beats a rendered file, lossless beats lossy, more pixels, a larger file (less compression),
//!   then edits and rating, then the earlier import.
//! - **Runs** off the UI thread: the UI thread snapshots the catalog's facts and builds the thumbnail jobs, a
//!   coordinator thread runs them on up to [`MAX_WORKERS`] workers (one while the user is around, the rest only at
//!   `normal` pace, as `duplicates.pump {pace}` says) and then groups, with a row in the activity stack ("Finding
//!   duplicate photos", n of N, ✕ stops it). Hashes are kept by content key for the session, so a second run only
//!   renders photos it hasn't seen. `wait: true` runs it to the end (scripts, MCP, tests).
//! - **Ends** by filtering the view to the groups (`only`, keepers first) and selecting every photo that isn't its
//!   group's keeper, so Delete removes them (no groups: an empty view and nothing selected, so a Delete can't reach
//!   photos the view doesn't show); Clear Filters brings the library back. No catalog change, no undo step.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use lightcraft_catalog::{MediaKind, Photo, PhotoId};
use lightcraft_pipeline::cull;
use serde_json::{Value, json};

use crate::activity::{Cancel, TaskGuard};
use crate::focus_scan::Pace;
use crate::media::{RenderJob, content_key};
use crate::{Result, Session};

/// Bits each perceptual hash may differ in and still be the same picture (default of `distance`).
pub(crate) const DEFAULT_DISTANCE: u32 = 6;
/// The loosest `distance` accepted.
pub(crate) const MAX_DISTANCE: u32 = 16;
/// Aspect ratios within this share of each other count as the same frame.
const ASPECT_TOLERANCE: f64 = 0.03;
/// Formats that keep every pixel: preferred over a lossy file of the same picture.
const LOSSLESS: [&str; 4] = ["DNG", "TIFF", "TIF", "PNG"];
/// The long edge of the thumbnail hashed (a grid bucket, so the grid's cached thumbnails serve).
const HASH_EDGE: usize = 256;
/// Workers at most (each renders one thumbnail at a time; a raw's decode can take hundreds of MB).
const MAX_WORKERS: usize = 8;
/// The rest between photos while the user is around (`light` pace).
const LIGHT_REST: Duration = Duration::from_millis(40);
/// How often a waiting worker looks at its pace again.
const PAUSE_POLL: Duration = Duration::from_millis(50);
/// Results taken in per pump (the rest wait for the next frame).
const MAX_DRAIN: usize = 5000;
/// Pairs the grouping compares between two looks at the cancel flag.
const CANCEL_EVERY: usize = 4096;

/// A photo's perceptual signature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sig {
    pub dct: u64,
    pub diff: u64,
    pub aspect: f64,
}

/// The signature of a rendered thumbnail.
pub(crate) fn sig_of(img: &lightcraft_raster::Rgba8) -> Sig {
    Sig { dct: cull::dct_hash_srgb8(img), diff: cull::diff_hash_srgb8(img), aspect: img.width as f64 / img.height.max(1) as f64 }
}

/// What makes two photos the same picture.
#[derive(Clone, Copy, Debug)]
struct Matching {
    distance: u32,
    same_moment: bool,
}

/// What a run was asked for.
#[derive(Clone, Copy, Debug)]
struct Options {
    matching: Matching,
    filter: bool,
    select_extras: bool,
}

/// What the coordinator sends back: a photo hashed (its content key, its signature — none when the thumbnail couldn't
/// be rendered — and its file name for the activity row), then the groups once every photo is hashed.
enum Msg {
    Hashed(String, Option<Sig>, String),
    Grouped(Vec<Group>),
}

struct Job {
    identity: Arc<()>,
    cancel: Arc<AtomicBool>,
    pace: Arc<AtomicU8>,
    rx: mpsc::Receiver<Msg>,
    /// Its row in the activity stack (dropping the job removes it).
    guard: TaskGuard,
    candidates: usize,
    options: Options,
    total: u64,
    done: u64,
    unreadable: u64,
    groups: Option<Vec<Group>>,
    started: Instant,
}

impl Drop for Job {
    fn drop(&mut self) {
        // the workers stop before their next photo (and when they can't hand a result over)
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

/// The run's options from the command's parameters, refused before anything is started when they are out of range.
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
    Ok(Options {
        matching: Matching { distance, same_moment: flag("sameMoment", false)? },
        filter: flag("filter", true)?,
        select_extras: flag("selectExtras", true)?,
    })
}

/// What ranks a photo within its group (the facts the catalog has, snapshotted before the run; no file is opened).
struct Row {
    id: PhotoId,
    key: String,
    hash: Option<String>,
    sig: Option<Sig>,
    /// Capture time to the second (`sameMoment`).
    moment: Option<String>,
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
    /// The row of `p`, with the signature already known for its content, if any.
    fn of(p: &Photo, sig: Option<Sig>) -> Row {
        Row {
            id: p.id,
            key: content_key(p),
            hash: p.content_hash.clone(),
            sig,
            moment: p.captured.as_deref().map(|c| c.chars().take(19).collect()),
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
            format!("{} beats {} (less compression)", bytes(self.size), bytes(other.size))
        } else if self.edited && !other.edited {
            "has edits".into()
        } else if self.rating > other.rating {
            "higher rating".into()
        } else {
            "the same; the earlier import is kept".into()
        }
    }
}

/// "2,452,673 B": a byte count a person can compare with another at a glance.
fn bytes(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 2);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out.push_str(" B");
    out
}

/// One group of the same picture, as the coordinator found it.
struct Group {
    exact: bool,
    keep: PhotoId,
    /// The photos to remove, best first, each with why the keeper beats it.
    extras: Vec<(PhotoId, String)>,
    /// The most bits two members' DCT hashes differ in (0 for exact groups).
    max_distance: u32,
}

impl Group {
    /// The group for a command's answer.
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

/// A thumbnail to render and hash: which row, the job, the file name for the activity row.
type Work = (usize, RenderJob, String);

/// Workers for a run: half the machine at most, within [`MAX_WORKERS`].
fn worker_count() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get()).div_ceil(2).clamp(1, MAX_WORKERS)
}

/// Wait until this worker may go on: every worker stops at a cancel; the first follows pause and light, the others
/// only run at normal pace. `false` when cancelled.
fn may_go_on(worker: usize, cancel: &AtomicBool, pace: &AtomicU8) -> bool {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let pace = pace.load(Ordering::Relaxed);
        if pace == Pace::Normal as u8 || (worker == 0 && pace == Pace::Light as u8) {
            if pace == Pace::Light as u8 {
                std::thread::sleep(LIGHT_REST);
            }
            return true;
        }
        std::thread::sleep(PAUSE_POLL);
    }
}

/// Render and hash the queued thumbnails on `workers` threads, filling `rows` and reporting each to `tx`. `false` when
/// cancelled.
fn hash_all(rows: &mut [Row], todo: Vec<Work>, workers: usize, cancel: &AtomicBool, pace: &AtomicU8, tx: &mpsc::Sender<Msg>) -> bool {
    let queue = Mutex::new(todo);
    let (done_tx, done_rx) = mpsc::channel::<(usize, Option<Sig>, String)>();
    std::thread::scope(|scope| {
        for worker in 0..workers {
            let (queue, done_tx) = (&queue, done_tx.clone());
            scope.spawn(move || {
                loop {
                    if !may_go_on(worker, cancel, pace) {
                        return;
                    }
                    let Some((index, job, name)) = queue.lock().unwrap_or_else(PoisonError::into_inner).pop() else { return };
                    // (`run` guards against a decoder panic: that file is unreadable, the run goes on)
                    let sig = match job.run().rendered {
                        Ok(rendered) => Some(sig_of(&rendered.image)),
                        Err(e) => {
                            log::debug!("find duplicates: {name}: {e}");
                            None
                        }
                    };
                    if done_tx.send((index, sig, name)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(done_tx);
        for (index, sig, name) in done_rx {
            let Some(row) = rows.get_mut(index) else { continue };
            row.sig = sig;
            if tx.send(Msg::Hashed(row.key.clone(), sig, name)).is_err() {
                cancel.store(true, Ordering::Relaxed);
            }
        }
    });
    !cancel.load(Ordering::Relaxed)
}

/// Start hashing and grouping `rows` on a coordinator thread; `todo` says which rows still need their signature (the
/// others have one from an earlier run) and the thumbnail job that gives it.
fn start(s: &mut Session, mut rows: Vec<Row>, todo: Vec<Work>, options: Options, pace: Pace) -> std::result::Result<(), String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let pace = Arc::new(AtomicU8::new(pace as u8));
    let (tx, rx) = mpsc::channel();
    let total = todo.len() as u64;
    let candidates = rows.len();
    let workers = worker_count();
    let (worker_cancel, worker_pace) = (cancel.clone(), pace.clone());
    std::thread::Builder::new()
        .name("find-duplicates".into())
        .spawn(move || {
            if !hash_all(&mut rows, todo, workers, &worker_cancel, &worker_pace, &tx) {
                return;
            }
            // then the grouping, also off the UI thread and stoppable (a loose distance compares many pairs)
            let groups = match crate::guard::catch("grouping duplicates", || group(&rows, options.matching, &worker_cancel)) {
                Ok(Some(groups)) => groups,
                _ => return,
            };
            let _ = tx.send(Msg::Grouped(groups));
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
        groups: None,
        started: Instant::now(),
    });
    Ok(())
}

/// Take in what the workers have hashed and, once the coordinator has grouped (or stopped), show the result. Returns
/// whether a job is still running.
fn step(s: &mut Session, pace: Pace) -> bool {
    let Some(job) = s.dupes.job.as_mut() else { return false };
    job.pace.store(pace as u8, Ordering::Relaxed);
    let mut finished = job.cancel.load(Ordering::Relaxed);
    let mut last_name = None;
    for _ in 0..MAX_DRAIN {
        match job.rx.try_recv() {
            Ok(Msg::Hashed(key, sig, name)) => {
                job.done += 1;
                last_name = Some(name);
                match sig {
                    Some(sig) => {
                        s.dupes.sigs.insert(key, sig);
                    }
                    None => job.unreadable += 1,
                }
            }
            Ok(Msg::Grouped(groups)) => {
                job.groups = Some(groups);
                finished = true;
                break;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            // the coordinator ended without its groups: stopped, or it could not go on
            Err(mpsc::TryRecvError::Disconnected) => {
                finished = true;
                break;
            }
        }
    }
    job.guard.progress(job.done, job.total);
    if let Some(name) = last_name {
        job.guard.detail(&name);
    }
    if !finished {
        return true;
    }
    let Some(job) = s.dupes.job.take() else { return false };
    // a job of another library (closed meanwhile) has nothing to show
    if Arc::ptr_eq(&job.identity, &s.library_identity) {
        let result = finish(s, &job);
        s.dupes.last = Some(result);
    }
    false
}

/// Show a finished job's groups (filter and selection), unless it was stopped, and describe the result. A photo that
/// left the library while the job ran is left out of its group; a group that shrinks to one photo is dropped.
fn finish(s: &mut Session, job: &Job) -> Value {
    let cancelled = job.groups.is_none();
    let groups: Vec<Group> = job
        .groups
        .iter()
        .flatten()
        .filter(|g| s.catalog.photo(g.keep).is_some_and(|p| p.in_library()))
        .map(|g| Group {
            exact: g.exact,
            keep: g.keep,
            extras: g.extras.iter().filter(|(id, _)| s.catalog.photo(*id).is_some_and(|p| p.in_library())).cloned().collect(),
            max_distance: g.max_distance,
        })
        .filter(|g| !g.extras.is_empty())
        .collect();
    let mut only = Vec::new();
    let mut extras = Vec::new();
    for g in &groups {
        only.push(g.keep);
        only.extend(g.extras.iter().map(|e| e.0));
        extras.extend(g.extras.iter().map(|e| e.0));
    }
    let o = job.options;
    if !cancelled {
        if o.filter {
            s.shelved_filter = None;
            s.filter.only.clone_from(&only);
            // an empty `only` means "no constraint": no duplicates is its own state that shows no photos
            s.filter.only_none = only.is_empty();
        }
        // (also when there are none: a selection the empty view doesn't show must not be what Delete acts on)
        if o.select_extras {
            s.selection.ids.clone_from(&extras);
            s.selection.active = extras.first().copied();
        }
    }
    json!({
        "groups": groups.iter().map(Group::json).collect::<Vec<_>>(),
        "photos": job.candidates,
        "decoded": job.done,
        "unreadable": job.unreadable,
        "duplicates": extras.len(),
        "seconds": job.started.elapsed().as_secs_f64(),
        "cancelled": cancelled,
    })
}

/// Same picture: both hashes within `distance` bits and the same frame.
fn same_picture(a: &Sig, b: &Sig, distance: u32) -> bool {
    (a.aspect - b.aspect).abs() <= ASPECT_TOLERANCE * a.aspect.max(b.aspect).max(1e-6)
        && cull::hamming(a.dct, b.dct) <= distance
        && cull::hamming(a.diff, b.diff) <= distance
}

/// [`same_picture`] for the tests.
#[cfg(test)]
pub(crate) fn same_picture_for_tests(a: &Sig, b: &Sig, distance: u32) -> bool {
    same_picture(a, b, distance)
}

/// Whether two rows may be the same picture: their signatures agree, and, with `sameMoment`, so do their capture
/// times when both have one.
fn pair_matches(a: &Row, b: &Row, m: Matching) -> bool {
    let (Some(sa), Some(sb)) = (a.sig, b.sig) else { return false };
    if m.same_moment
        && let (Some(ta), Some(tb)) = (&a.moment, &b.moment)
        && ta != tb
    {
        return false;
    }
    same_picture(&sa, &sb, m.distance)
}

/// The matching pairs, closest first, without comparing every pair: a DCT hash is cut into `distance + 1` chunks, and
/// two hashes that differ in at most `distance` bits agree in at least one chunk (pigeonhole), so only photos sharing
/// a chunk's value are compared in full. `None` when stopped.
fn near_pairs(rows: &[Row], m: Matching, cancel: &AtomicBool) -> Option<Vec<(u32, usize, usize)>> {
    let chunks = (m.distance as usize + 1).min(64);
    let width = 64 / chunks;
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut pairs = Vec::new();
    let mut compared = 0usize;
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
                for &j in idxs.iter().skip(n + 1) {
                    compared += 1;
                    if compared.is_multiple_of(CANCEL_EVERY) && cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    let (Some(a), Some(b)) = (rows.get(i), rows.get(j)) else { continue };
                    if pair_matches(a, b, m) && seen.insert((i.min(j), i.max(j))) {
                        let d = match (a.sig, b.sig) {
                            (Some(sa), Some(sb)) => cull::hamming(sa.dct, sb.dct),
                            _ => 0,
                        };
                        pairs.push((d, i.min(j), i.max(j)));
                    }
                }
            }
        }
    }
    pairs.sort_unstable();
    Some(pairs)
}

/// Whether `i` is the same picture as every member of `members` (complete linkage), so a group never holds two photos
/// further apart than the distance.
fn fits_all(rows: &[Row], i: usize, members: &[usize], m: Matching) -> bool {
    let Some(a) = rows.get(i) else { return false };
    members.iter().all(|&j| j == i || rows.get(j).is_some_and(|b| b.hash.is_some() && b.hash == a.hash || pair_matches(a, b, m)))
}

/// The members of each duplicate group: byte-identical files first, then the matching pairs closest first, each
/// photo joining a group only when it fits every member (and two groups merging only when every cross pair fits).
fn members(rows: &[Row], pairs: &[(u32, usize, usize)], m: Matching) -> Vec<Vec<usize>> {
    let mut group_of: Vec<Option<usize>> = vec![None; rows.len()];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut by_hash: HashMap<&str, usize> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        let Some(h) = r.hash.as_deref() else { continue };
        match by_hash.get(h) {
            Some(&g) => {
                if let Some(members) = groups.get_mut(g) {
                    members.push(i);
                }
                group_of[i] = Some(g);
            }
            None => {
                by_hash.insert(h, groups.len());
                group_of[i] = Some(groups.len());
                groups.push(vec![i]);
            }
        }
    }
    for &(_, i, j) in pairs {
        match (group_of.get(i).copied().flatten(), group_of.get(j).copied().flatten()) {
            (None, None) => {
                group_of[i] = Some(groups.len());
                group_of[j] = Some(groups.len());
                groups.push(vec![i, j]);
            }
            (Some(g), None) | (None, Some(g)) => {
                let other = if group_of.get(i).copied().flatten() == Some(g) { j } else { i };
                if groups.get(g).is_some_and(|members| fits_all(rows, other, members, m)) {
                    if let Some(members) = groups.get_mut(g) {
                        members.push(other);
                    }
                    group_of[other] = Some(g);
                }
            }
            (Some(a), Some(b)) if a != b => {
                let fits = groups.get(b).is_some_and(|bs| bs.iter().all(|&x| groups.get(a).is_some_and(|members| fits_all(rows, x, members, m))));
                if fits {
                    let moved = groups.get_mut(b).map(std::mem::take).unwrap_or_default();
                    for &x in &moved {
                        group_of[x] = Some(a);
                    }
                    if let Some(members) = groups.get_mut(a) {
                        members.extend(moved);
                    }
                }
            }
            _ => {}
        }
    }
    groups.into_iter().filter(|g| g.len() > 1).collect()
}

/// The duplicate groups among `rows`, largest first; `None` when stopped.
fn group(rows: &[Row], m: Matching, cancel: &AtomicBool) -> Option<Vec<Group>> {
    let pairs = near_pairs(rows, m, cancel)?;
    let mut groups: Vec<Group> = members(rows, &pairs, m).into_iter().map(|g| group_of(rows, g)).collect();
    groups.sort_by_key(|g| (std::cmp::Reverse(g.extras.len()), g.keep));
    Some(groups)
}

/// One group from its members' rows: ranked, the best kept, the rest with why they lose.
fn group_of(rows: &[Row], mut members: Vec<usize>) -> Group {
    members.sort_by(|a, b| {
        let (qa, qb) = (rows.get(*a).map(Row::quality), rows.get(*b).map(Row::quality));
        qb.cmp(&qa)
    });
    let rs: Vec<&Row> = members.iter().filter_map(|i| rows.get(*i)).collect();
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
}

/// What `duplicates.pump` answers: whether a job runs, how far it is, the last result and the hashes known.
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

/// Run the job to its end (a script's `wait: true`).
fn wait_for_job(s: &mut Session) {
    while step(s, Pace::Normal) {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `library.findDuplicates {ids?, distance?, sameMoment?, filter?, selectExtras?, wait?}`.
pub(crate) fn find(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "library.findDuplicates";
    follow_library(s);
    let wait = p.get("wait").and_then(Value::as_bool) == Some(true);
    if s.dupes.job.is_some() {
        if !wait {
            return Ok(json!({"queued": 0, "running": true, "status": status(s)}));
        }
        // a script that waits gets the running job's result, whatever it was asked for
        wait_for_job(s);
        return Ok(json!({"queued": 0, "running": false, "last": s.dupes.last.clone().unwrap_or(Value::Null)}));
    }
    let ids = crate::cmd::photo_ids(p, C)?;
    let options = options(p, C)?;
    let candidates = candidates(s, ids.as_deref());
    // the catalog's facts now, and a thumbnail job for what still needs hashing (the rest is known by content)
    let mut rows: Vec<Row> = Vec::with_capacity(candidates.len());
    let mut todo: Vec<Work> = Vec::new();
    for id in &candidates {
        let Some(ph) = s.catalog.photo(*id).cloned() else { continue };
        let row = Row::of(&ph, s.dupes.sigs.get(&content_key(&ph)).copied());
        if row.sig.is_none()
            && let Some(job) = s.thumb_job(*id, HASH_EDGE)
        {
            todo.push((rows.len(), job, ph.file_name.clone()));
        }
        rows.push(row);
    }
    // (the queue is popped from its end: keep the catalog's order)
    todo.reverse();
    let queued = todo.len();
    start(s, rows, todo, options, if wait { Pace::Normal } else { s.dupes.pace }).map_err(|e| crate::cmd::bad(C, e))?;
    if !wait {
        return Ok(json!({"queued": queued, "running": true}));
    }
    wait_for_job(s);
    Ok(json!({"queued": queued, "running": false, "last": s.dupes.last.clone().unwrap_or(Value::Null)}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u64, dct: u64, moment: Option<&str>) -> Row {
        Row {
            id: PhotoId(id),
            key: format!("k{id}"),
            hash: None,
            sig: Some(Sig { dct, diff: 0, aspect: 1.5 }),
            moment: moment.map(str::to_string),
            raw: false,
            lossless: false,
            pixels: 12,
            size: 100,
            edited: false,
            rating: 0,
            format: "JPEG".into(),
            width: 4,
            height: 3,
        }
    }

    /// A chain a–b–c where a and c are too far apart never becomes one group: a group holds only photos that all
    /// match each other, so a burst can't gather through its neighbours.
    #[test]
    fn a_group_never_holds_two_photos_further_apart_than_the_distance() {
        let rows = vec![row(1, 0b0000, None), row(2, 0b0011, None), row(3, 0b1111, None)];
        let m = Matching { distance: 2, same_moment: false };
        let never = AtomicBool::new(false);
        let groups = group(&rows, m, &never).unwrap();
        assert_eq!(groups.len(), 1, "{:?}", groups.iter().map(|g| (g.keep, g.extras.len())).collect::<Vec<_>>());
        assert_eq!((groups[0].keep, groups[0].extras.len()), (PhotoId(1), 1));
        assert!(groups[0].max_distance <= 2);
        // loosened, all three fit each other
        let groups = group(&rows, Matching { distance: 4, same_moment: false }, &never).unwrap();
        assert_eq!((groups.len(), groups[0].extras.len()), (1, 2));
    }

    /// With `sameMoment`, two photos of different capture times are never the same picture; a photo without a
    /// capture time may still match.
    #[test]
    fn same_moment_keeps_frames_of_different_seconds_apart() {
        let rows = vec![row(1, 0, Some("2026-10-31T10:00:00")), row(2, 0, Some("2026-10-31T10:00:01")), row(3, 0, None)];
        let never = AtomicBool::new(false);
        let strict = Matching { distance: 6, same_moment: true };
        let groups = group(&rows, strict, &never).unwrap();
        assert_eq!(groups.len(), 1, "the two timed frames don't match each other");
        assert_eq!(groups[0].extras.len(), 1, "the untimed photo joins one of them");
        let loose = Matching { distance: 6, same_moment: false };
        assert_eq!(group(&rows, loose, &never).unwrap()[0].extras.len(), 2);
    }

    #[test]
    fn byte_counts_read_with_separators() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(2_452_673), "2,452,673 B");
    }
}
