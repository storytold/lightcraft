//! `photo.qualityScan`: the Image Quality Scanner (issue #1313). It scores photos for technical
//! quality — blur, noise, exposure, compression artifacts ([`lcc_pipeline::quality`]) — over a
//! selection, the photos in view, a folder or the whole library, stores each score on the photo
//! (catalog format 20, [`lcc_catalog::QualityScan`]) so the Library can filter and sort by it
//! without scanning again, and maps scores to Pick Flags with two configurable cutoffs.
//!
//! A background scan is a row in the activity stack (cancellable), measures a batch of
//! thumbnail-level sources at a time on every core and writes the scores as one undo step when it
//! ends: [`Session::quality_scan_advance`] is its state machine, driven by `photo.qualityScanPump`
//! (which the app calls every frame) or run to the end by `wait: true` (the CLI, MCP, tests). A
//! cancelled run still stores what it finished.
//!
//! **Unscanned only** (the default) leaves photos that already carry a score alone; `rescan: true`
//! measures every target again and overwrites its score. Unless `applyFlags` is on, the scan only
//! says what flag a photo's score asks for (`verdict`); with it on, it writes that flag onto
//! photos the user has not flagged themselves, never touching a flag the user set.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use lcc_catalog::{Flag, Op, Photo, PhotoId, QualityScan, QualityVerdict, Source};
use lcc_pipeline::quality::{QualityFactors, QualityWeights};
use serde_json::{Value, json};

use crate::activity::{Cancel, TaskGuard, TaskHandle};
use crate::cmd::{CommandSpec, always, bad, cmd};
use crate::{Result, Session};

const C: &str = "photo.qualityScan";
/// Photos measured side by side: the sources of one batch are decoded and scored on this many
/// lanes (the same count assisted culling's signature pass uses).
const LANES: usize = 8;
/// The largest number of photos one run accepts: every photo of a large library is fine, a
/// runaway script asking for more is refused before anything is measured.
const MAX_TARGETS: usize = 2_000_000;

/// The scan's settings, as `photo.qualityScan` takes them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanOptions {
    /// The score at or above which a photo is Accepted (0..100, at or above `rejected`).
    pub accepted: f32,
    /// The score at or below which a photo is Rejected (0..100, at or below `accepted`).
    pub rejected: f32,
    /// Write the Pick / Reject flags the scores ask for (only onto photos the user hasn't flagged).
    pub apply_flags: bool,
    /// Measure photos that carry a score already, and overwrite their score.
    pub rescan: bool,
    /// The factors measured and their weights.
    pub weights: QualityWeights,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions { accepted: 70.0, rejected: 25.0, apply_flags: false, rescan: false, weights: QualityWeights::default() }
    }
}

impl ScanOptions {
    /// The options in `p`, refused before anything is measured when a number isn't one, the
    /// cutoffs cross, or every factor is switched off (a scan that scores nothing measures
    /// nothing).
    pub fn parse(p: &Value) -> Result<ScanOptions> {
        let d = ScanOptions::default();
        let accepted = cutoff(p, "accepted", d.accepted)?;
        let rejected = cutoff(p, "rejected", d.rejected)?;
        if accepted < rejected {
            return Err(bad(C, "`accepted` must be at or above `rejected`: the other way round, the two cutoffs overlap"));
        }
        let weights = QualityWeights {
            blur: weight(p, "blur", d.weights.blur)?,
            noise: weight(p, "noise", d.weights.noise)?,
            exposure: weight(p, "exposure", d.weights.exposure)?,
            compression: weight(p, "compression", d.weights.compression)?,
        };
        if weights.blur == 0.0 && weights.noise == 0.0 && weights.exposure == 0.0 && weights.compression == 0.0 {
            return Err(bad(C, "every factor's weight is 0: there is nothing to score"));
        }
        Ok(ScanOptions {
            accepted,
            rejected,
            apply_flags: p.get("applyFlags").and_then(Value::as_bool).unwrap_or(d.apply_flags),
            rescan: p.get("rescan").and_then(Value::as_bool).unwrap_or(d.rescan),
            weights,
        })
    }
}

/// A number in 0..=100 for one of the two cutoffs, or its default.
fn cutoff(p: &Value, key: &str, default: f32) -> Result<f32> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_f64() {
            Some(x) if x.is_finite() && (0.0..=100.0).contains(&x) => Ok(x as f32),
            _ => Err(bad(C, format!("`{key}` must be a number from 0 to 100"))),
        },
    }
}

/// The weight of one factor (`weights: {blur, noise, exposure, compression}`), 0..=10.
fn weight(p: &Value, key: &str, default: f32) -> Result<f32> {
    match p.get("weights").and_then(|w| w.get(key)) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_f64() {
            Some(x) if x.is_finite() && (0.0..=10.0).contains(&x) => Ok(x as f32),
            _ => Err(bad(C, format!("`weights.{key}` must be a number from 0 to 10"))),
        },
    }
}

/// Whether a photo's source is (or was made from) a JPEG: blockiness is measured on it. A raw's
/// own data was never JPEG encoded (its embedded preview was, but the raw data is what is
/// scored); a preview-only raw is scored on the camera's JPEG, so it is.
pub fn jpeg_encoded(photo: &Photo) -> bool {
    photo.preview_only.is_some()
        || (matches!(photo.kind, lcc_catalog::MediaKind::Image)
            && ["jpg", "jpeg", "jpe", "jfif"].iter().any(|e| photo.format.eq_ignore_ascii_case(e)))
}

/// Score one decoded source: the factors, their combined score, and the verdict the two cutoffs
/// read from it.
pub fn score(img: &lcc_raster::Rgb32f, options: &ScanOptions, jpeg: bool) -> QualityScan {
    let factors: QualityFactors = QualityFactors::measure(img, options.weights, jpeg);
    // (a scan with every factor at weight 0 is refused, so a measured photo has a score)
    let score = factors.score.unwrap_or(0.0);
    QualityScan {
        sharpness: factors.sharpness,
        noise: factors.noise,
        exposure: factors.exposure,
        compression: factors.compression,
        score,
        verdict: QualityVerdict::of(score, options.accepted, options.rejected),
        // (the flag record is for the run that stores the scan: a measurement writes no flag yet)
        flag_written: None,
    }
}

/// The photo's flag once a scan that writes flags (`apply_flags`) stores `scan`: the verdict's
/// flag when the photo is unflagged, or when its flag is the one an earlier scan wrote (a rescan
/// may change its own flag); a flag the user set stays, and a `Pending` verdict changes nothing.
/// `previous` is the scan the photo carried before this run, if any.
pub fn flag_after(photo: &Photo, previous: Option<QualityScan>, scan: &QualityScan, apply_flags: bool) -> Flag {
    let ours = photo.flag == Flag::None || previous.and_then(|q| q.flag_written) == Some(photo.flag);
    match scan.verdict.flag() {
        Some(flag) if apply_flags && ours => flag,
        _ => photo.flag,
    }
}

/// The photos a scan looks at:
/// - `scope: "selected"` (the default with `ids`): `ids`, else the selection, else the photos in
///   view (the dialog's "the selected photos, else the photos in view");
/// - `scope: "view"` (the default without `ids`): `ids`, else every photo in view;
/// - `scope: "folder"`: every library photo in the folder shown — the Folders panel's choice, or
///   the folder Browse shows (its subfolders when the view shows them) — or in `path` (and its
///   subfolders, unless `subfolders: false`);
/// - `scope: "catalog"`: every library photo.
fn targets(s: &mut Session, p: &Value) -> Result<Vec<PhotoId>> {
    let explicit_ids = p.get("ids").is_some_and(|v| !v.is_null());
    let scope = match p.get("scope").and_then(Value::as_str) {
        Some(scope) => scope,
        None if explicit_ids => "selected",
        None => "view",
    };
    let mut ids: Vec<PhotoId> = match scope {
        "selected" => {
            if explicit_ids {
                s.targets(p)?
            } else if s.selection.ids.is_empty() {
                // nothing selected (or one photo open with no selection): the view, so the dialog's
                // "the photos in view" is what happens
                s.visible_cloned()
            } else {
                s.selected()
            }
        }
        "view" => {
            if explicit_ids {
                s.targets(p)?
            } else {
                s.visible_cloned()
            }
        }
        "folder" => {
            let path = p.get("path").and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
            let (dir, deep) = match path {
                Some(path) => {
                    // (read as the catalog reads a folder's identity: either separator, `..` resolved)
                    let dir = lcc_catalog::query::folder_key(path);
                    if dir.is_empty() {
                        return Err(bad(C, format!("`path` must be a folder, not {path:?}")));
                    }
                    let deep = p.get("subfolders").and_then(Value::as_bool).unwrap_or(true);
                    (dir, deep)
                }
                None => {
                    // the folder the Library shows: the Folders panel's choice, or the folder
                    // Browse shows on disk (the photos of it the library has)
                    match s.library_folder.clone() {
                        Some(dir) => (dir, s.folder_subfolders),
                        None => match s.browse.as_ref() {
                            Some(browse) => {
                                let dir = lcc_catalog::query::folder_key(&browse.path);
                                if dir.is_empty() {
                                    return Err(bad(C, "`scope: \"folder\"` scans the folder shown: open one, or give `path`"));
                                }
                                (dir, browse.subfolders)
                            }
                            None => {
                                return Err(bad(C, "`scope: \"folder\"` scans the folder shown: open one, or give `path`"));
                            }
                        },
                    }
                }
            };
            s.catalog
                .photos()
                .filter(|ph| ph.in_library())
                .filter(|ph| match &ph.source {
                    Source::File { path } => {
                        // (a file's path is one name below its folder, so its own folder is `len == 1`)
                        lcc_catalog::query::folder_within(path, &dir)
                            && (deep || lcc_catalog::query::folder_rest(path, &dir).is_some_and(|rest| rest.len() == 1))
                    }
                    _ => false,
                })
                .map(|ph| ph.id)
                .collect()
        }
        "catalog" => s.catalog.photos().filter(|ph| ph.in_library()).map(|ph| ph.id).collect(),
        other => return Err(bad(C, format!("`scope` must be selected, view, folder or catalog, not {other:?}"))),
    };
    // one entry per photo, in a stable order
    ids.sort_unstable_by_key(|id| id.0);
    ids.dedup();
    ids.retain(|id| s.catalog.photo(*id).is_some());
    if ids.len() > MAX_TARGETS {
        return Err(bad(C, format!("at most {MAX_TARGETS} photos at a time")));
    }
    Ok(ids)
}

/// What a measurement thread sends back for one photo: the flag it had (so the scan can tell
/// whether it changes it), and its scores or why it has none.
type Measured = std::result::Result<QualityScan, String>;

/// One photo of a batch, as the thread measures it.
struct Frame {
    id: PhotoId,
    flag: Flag,
    jpeg: bool,
    source: crate::media::SourceRef,
}

/// A background scan: the photos still to do, the batch measuring now, and everything measured so
/// far (kept until the run ends and it is all written in one undo step).
pub struct QualityScanRun {
    queue: std::collections::VecDeque<PhotoId>,
    options: ScanOptions,
    cancel: Arc<AtomicBool>,
    /// The batch measuring now (its results, how many arrived), while it runs.
    inflight: Option<(Receiver<(PhotoId, Flag, Measured)>, usize, usize)>,
    /// Measured photos, waiting for the run's last batch.
    results: Vec<(PhotoId, Flag, Measured)>,
    measured: u64,
    failed: u64,
    cancelled: bool,
    /// Photos left alone because they carry a score already (unscanned only).
    skipped: u64,
    /// Clips skipped (a poster frame is not the clip's quality).
    clips: u64,
    /// How many photos the run was started for.
    total: u64,
    /// The activity row: dropped, the row goes.
    task: TaskGuard,
}

impl QualityScanRun {
    /// The run's state, as `photo.qualityScanPump` and `status` answer while it runs.
    fn progress(&self) -> Value {
        // (`done` never passes `total`: one result per frame, counted as it arrives)
        let inflight = self.inflight.as_ref().map_or(0, |(_, done, total)| total.saturating_sub(*done) as u64);
        json!({
            "active": true,
            "done": false,
            "photos": self.total,
            "measured": self.measured,
            "remaining": self.queue.len() as u64 + inflight,
            "cancelled": self.cancel.load(Ordering::Relaxed),
        })
    }
}

impl Session {
    /// Starts one batch: its sources are resolved here (a handle each, nothing decoded), then a
    /// thread of its own decodes and scores them in parallel. `None` when the batch had no photo
    /// left to measure; `Some(Err(n))` when a thread couldn't be made (those `n` are failures).
    #[allow(clippy::type_complexity)]
    fn quality_scan_batch(
        &mut self,
        ids: Vec<PhotoId>,
        options: ScanOptions,
        handle: TaskHandle,
    ) -> Option<std::result::Result<(Receiver<(PhotoId, Flag, Measured)>, usize, usize), usize>> {
        let mut frames = Vec::with_capacity(ids.len());
        for id in ids {
            // (cloned out: `source_ref` takes `&mut self`)
            let Some(photo) = self.catalog.photo(id).cloned() else { continue };
            let source = self.media.source_ref(&photo, crate::media::SourceLevel::Thumb);
            frames.push(Frame { id, flag: photo.flag, jpeg: jpeg_encoded(&photo), source });
        }
        if frames.is_empty() {
            return None;
        }
        let total = frames.len();
        let (tx, rx) = channel();
        // (the thread counts each photo in the activity row itself; the row goes when the run
        // ends, not when a batch's last lane does)
        let spawned = std::thread::Builder::new().name("quality-scan".into()).spawn(move || {
            use rayon::prelude::*;
            frames.into_par_iter().for_each_with(tx, |tx, frame| {
                let measured =
                    frame.source.load().map_err(|e| format!("can't read the {}: {e}", frame.id.0)).map(|img| score(&img, &options, frame.jpeg));
                match &measured {
                    Ok(_) => handle.item_done(),
                    Err(why) => handle.item_failed(why),
                }
                // (the receiver gone means the run was abandoned: drop the result, don't wait)
                let _ = tx.send((frame.id, frame.flag, measured));
            });
        });
        match spawned {
            Ok(_) => Some(Ok((rx, 0, total))),
            Err(_) => Some(Err(total)),
        }
    }

    /// One step of a background scan: takes in the batch that finished and starts the next, or —
    /// when nothing is left — writes every score in one undo step and answers the summary
    /// (`{active: false, done: true, …}`); `{active: false, done: false}` when no scan runs.
    /// Cheap: called every frame.
    pub fn quality_scan_advance(&mut self) -> Value {
        let Some(mut run) = self.quality_scan.take() else {
            return json!({"active": false, "done": false});
        };
        loop {
            // 1. take in the batch measuring now
            let mut batch = run.inflight.take();
            let mut finished = false;
            if let Some((rx, done, total)) = batch.as_mut() {
                let mut disconnected = false;
                loop {
                    match rx.try_recv() {
                        Ok((id, flag, measured)) => {
                            // (failures are counted where the results are written: one per photo)
                            if measured.is_ok() {
                                run.measured += 1;
                            }
                            run.results.push((id, flag, measured));
                            *done += 1;
                        }
                        // (a disconnected channel has handed over everything it held already)
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                        Err(TryRecvError::Empty) => break,
                    }
                }
                // every result is in, or the sender is gone (a lane panicked: what arrived counts)
                finished = *done >= *total || disconnected;
            }
            if let Some((rx, done, total)) = batch.take()
                && !finished
            {
                run.inflight = Some((rx, done, total));
                run.task.progress(run.measured, run.total);
                let progress = run.progress();
                self.quality_scan = Some(run);
                return progress;
            }
            // 2. cancelled, or nothing left: write what was measured and end
            if run.cancel.load(Ordering::Relaxed) {
                run.cancelled = true;
            }
            if run.cancelled || run.queue.is_empty() {
                return match self.finish_quality_scan(run) {
                    Ok(summary) => summary,
                    Err(e) => json!({"active": false, "done": true, "error": e.to_string()}),
                };
            }
            // 3. the next batch
            let next: Vec<PhotoId> = run.queue.drain(..LANES.min(run.queue.len())).collect();
            let handle = run.task.handle();
            match self.quality_scan_batch(next, run.options, handle) {
                Some(Ok(inflight)) => {
                    run.inflight = Some(inflight);
                    run.task.progress(run.measured, run.total);
                    let progress = run.progress();
                    self.quality_scan = Some(run);
                    return progress;
                }
                Some(Err(lost)) => {
                    // a thread couldn't be made: those photos count as failed, and the loop goes on
                    run.failed += lost as u64;
                    continue;
                }
                // nothing in the batch could be measured: the loop goes on with the one after it
                None => continue,
            }
        }
    }

    /// Writes a run's measured scores (one undo step) and answers its summary.
    fn finish_quality_scan(&mut self, mut run: QualityScanRun) -> Result<Value> {
        let results = std::mem::take(&mut run.results);
        let mut accepted = 0u64;
        let mut pending = 0u64;
        let mut rejected = 0u64;
        let mut flagged = 0u64;
        let mut failures: Vec<Value> = Vec::new();
        let mut photos = Vec::with_capacity(run.measured as usize);
        let mut ops: Vec<Op> = Vec::with_capacity(run.measured as usize * 2);
        for (id, flag_before, measured) in results {
            let scan = match measured {
                Ok(scan) => scan,
                Err(e) => {
                    // (counted here, not when the batch arrived: one failure, one entry)
                    run.failed += 1;
                    failures.push(json!([id.0, e]));
                    continue;
                }
            };
            let Some(photo) = self.catalog.photo(id) else { continue };
            let mut analysis = photo.analysis.unwrap_or_default();
            let previous = analysis.quality;
            analysis.quality = Some(scan);
            // the focus the scan measured is the assisted culling's own focus measure: keep the
            // two in one number, and the best-of-burst and other culling marks as they were
            analysis.sharpness = scan.sharpness;
            let flag = flag_after(photo, previous, &scan, run.options.apply_flags);
            // the flag a scan writes is its own, so a later scan may change it; a flag it leaves
            // alone is the user's. The record says which: the flag just written, the one an earlier
            // scan wrote and this one kept, or none (the photo's flag is not the scan's)
            if let Some(q) = analysis.quality.as_mut() {
                q.flag_written = if flag != flag_before { Some(flag) } else { previous.and_then(|old| old.flag_written).filter(|old| *old == flag) };
            }
            ops.push(Op::SetAnalysis { id, analysis: Some(analysis) });
            if flag != flag_before {
                ops.push(Op::SetFlag { id, flag });
                flagged += 1;
            }
            match scan.verdict {
                QualityVerdict::Accepted => accepted += 1,
                QualityVerdict::Pending => pending += 1,
                QualityVerdict::Rejected => rejected += 1,
            }
            photos.push(json!({
                "id": id.0,
                "score": round1(scan.score),
                "sharpness": round1(scan.sharpness),
                "noise": round1(scan.noise),
                "exposure": round1(scan.exposure),
                "compression": scan.compression.map(round1),
                "verdict": scan.verdict.as_str(),
                "flag": flag,
            }));
        }
        if !ops.is_empty() {
            self.commit("Image Quality Scan", Op::Batch { ops })?;
        }
        let failed = run.failed;
        drop(run.task);
        let summary = json!({
            "active": false,
            "done": true,
            "measured": photos.len() as u64,
            "skipped": run.skipped,
            "clips": run.clips,
            "accepted": accepted,
            "pending": pending,
            "rejected": rejected,
            "flagged": flagged,
            "failed": failed,
            "failures": failures,
            "cancelled": run.cancelled,
            "photos": photos,
        });
        Ok(summary)
    }
}

fn round1(v: f32) -> f64 {
    (f64::from(v) * 10.0).round() / 10.0
}

/// `photo.qualityScan`.
fn quality_scan(s: &mut Session, p: &Value) -> Result<Value> {
    if p.get("cancel").and_then(Value::as_bool).unwrap_or(false) {
        let cancelled = s.quality_scan.as_ref().is_some_and(|run| {
            run.cancel.store(true, Ordering::Relaxed);
            true
        });
        return Ok(json!({"cancelled": cancelled}));
    }
    if p.get("status").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(match &s.quality_scan {
            Some(run) => run.progress(),
            None => json!({"active": false, "done": false}),
        });
    }
    let options = ScanOptions::parse(p)?;
    let ids = targets(s, p)?;
    if s.quality_scan.as_ref().is_some_and(|run| !run.cancel.load(Ordering::Relaxed)) {
        return Err(bad(C, "a scan is in progress already (`{\"cancel\": true}` stops it)"));
    }
    // Unscanned only: photos that carry a score are left alone, and counted as skipped. A clip's
    // poster frame is not the clip's quality: clips are skipped too.
    let mut queue: Vec<PhotoId> = Vec::with_capacity(ids.len());
    let mut skipped = 0u64;
    let mut clips = 0u64;
    for id in ids {
        let Some(photo) = s.catalog.photo(id) else { continue };
        if photo.kind == lcc_catalog::MediaKind::Video {
            clips += 1;
            continue;
        }
        if !options.rescan && photo.analysis.and_then(|a| a.quality).is_some() {
            skipped += 1;
            continue;
        }
        queue.push(id);
    }
    if queue.is_empty() {
        // nothing to measure: say so plainly, without an empty activity row
        return Ok(json!({
            "active": false, "done": true, "measured": 0, "skipped": skipped, "clips": clips,
            "accepted": 0, "pending": 0, "rejected": 0, "flagged": 0,
            "failed": 0, "failures": [], "cancelled": false, "photos": [],
        }));
    }
    let total = queue.len() as u64;
    if p.get("wait").and_then(Value::as_bool).unwrap_or(false) {
        let task = s.activity.start_batch("qualityScan", "Scanning image quality", Cancel::No);
        s.quality_scan = Some(QualityScanRun {
            queue: queue.into(),
            options,
            cancel: Arc::new(AtomicBool::new(false)),
            inflight: None,
            results: Vec::new(),
            measured: 0,
            failed: 0,
            cancelled: false,
            skipped,
            clips,
            total,
            task,
        });
        // run the state machine to the end right here: the frame it ends on is the summary
        // (the state machine answers its summary; this is what it would say if it could not run)
        let mut summary = json!({"active": false, "done": true, "measured": 0, "skipped": skipped, "clips": clips, "photos": []});
        for _ in 0..MAX_TARGETS {
            let step = s.quality_scan_advance();
            if step["done"] == true {
                summary = step;
                break;
            }
            if step["active"] != true {
                break;
            }
        }
        return Ok(summary);
    }
    // a run already cancelled is over as far as a new scan is concerned: it is left for its own
    // pump to write (what it measured before the ✕ is kept — a stopped scan is not a lost one),
    // and the queue it still held goes with it
    if s.quality_scan.as_ref().is_some_and(|run| run.cancel.load(Ordering::Relaxed)) {
        s.quality_scan = None;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    // (the label is translated in the Activity stack; how far it is shows in its progress)
    let task = s.activity.start_batch("qualityScan", "Scanning image quality", Cancel::Flag(cancel.clone()));
    task.progress(0, total);
    s.quality_scan = Some(QualityScanRun {
        queue: queue.into(),
        options,
        cancel,
        inflight: None,
        results: Vec::new(),
        measured: 0,
        failed: 0,
        cancelled: false,
        skipped,
        clips,
        total,
        task,
    });
    Ok(json!({"started": true, "done": false, "photos": total, "skipped": skipped, "clips": clips}))
}

/// Cheap every frame: moves a background scan along and answers its progress; the frame it ends,
/// its summary (once).
pub fn pump(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(s.quality_scan_advance())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "photo.qualityScan",
            "Image Quality Scan",
            ["Library"],
            None,
            "{scope?: selected|view|folder|catalog (selected with `ids`, else the selection or the photo in view; view: the photos in view; folder: the folder shown (the Folders panel's or Browse's), or `path`; catalog: the whole library), ids?, path?, subfolders?: bool (true), accepted?: 0..100 (70), rejected?: 0..100 (25), applyFlags?: bool (false), rescan?: bool (false), weights?: {blur, noise, exposure, compression: 0..10 (1)}, wait?: bool, cancel?: bool, status?: bool}: score photos for technical quality — sharpness (blur), noise, exposure and JPEG compression artifacts — and store each score on the photo with a verdict from the two cutoffs; `applyFlags` also writes Pick / Reject onto photos the user has not flagged (a flag an earlier scan wrote is the scan's own, so a rescan may change it); `rescan` measures photos that carry a score already; runs in the background (an activity row, cancellable: cancelling keeps what it measured) and `wait` runs it now → the summary {active: false, done: true, measured, skipped, clips, accepted, pending, rejected, flagged, failed, failures, cancelled, photos} or, started in the background, {started: true, done: false, photos, skipped, clips}",
            always,
            quality_scan
        ),
        cmd!(
            query "photo.qualityScanPump",
            "Image Quality Scan: Take Results",
            [],
            None,
            "{}: cheap every frame: moves a background scan along and returns its progress {active, done, photos, measured, remaining, cancelled}; the frame it ends, its summary (one undo step) → {active, done, measured, skipped, clips, accepted, pending, rejected, flagged, failed, failures, cancelled, photos}",
            always,
            pump
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cutoffs are inclusive, the rejected one winning where they touch, and a score that
    /// isn't a number is Pending (never a flag).
    #[test]
    fn verdicts_follow_the_two_cutoffs() {
        assert_eq!(QualityVerdict::of(70.0, 70.0, 25.0), QualityVerdict::Accepted);
        assert_eq!(QualityVerdict::of(69.9, 70.0, 25.0), QualityVerdict::Pending);
        assert_eq!(QualityVerdict::of(25.0, 70.0, 25.0), QualityVerdict::Rejected);
        assert_eq!(QualityVerdict::of(25.1, 70.0, 25.0), QualityVerdict::Pending);
        assert_eq!(QualityVerdict::of(f32::NAN, 70.0, 25.0), QualityVerdict::Pending);
        // both cutoffs at one number: that score is a reject (nothing sits above it and below it)
        assert_eq!(QualityVerdict::of(50.0, 50.0, 50.0), QualityVerdict::Rejected);
    }

    /// Options: hostile numbers are refused with an actionable message, not measured around.
    #[test]
    fn options_refuse_what_they_cannot_use() {
        let parse = |v: Value| ScanOptions::parse(&v);
        assert!(parse(json!({})).is_ok());
        for bad in [json!({"accepted": 101}), json!({"rejected": -1}), json!({"accepted": "high"}), json!({"accepted": 20, "rejected": 40})] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
        let none = json!({"weights": {"blur": 0, "noise": 0, "exposure": 0, "compression": 0}});
        assert!(parse(none).is_err(), "nothing to score");
        let nan = json!({"weights": {"blur": null}});
        assert!(parse(nan).is_ok(), "an absent weight takes its default");
    }

    /// A JPEG photo and a preview-only raw are measured for blockiness; a raw is not.
    #[test]
    fn blockiness_is_measured_on_jpeg_sources_only() {
        use lcc_catalog::Photo;
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "JPEG", 4, 4, "2026-01-01");
        assert!(jpeg_encoded(&p));
        p.format = "CR3".into();
        p.kind = lcc_catalog::MediaKind::Raw;
        assert!(!jpeg_encoded(&p));
        p.preview_only = Some("unsupported variant".into());
        assert!(jpeg_encoded(&p), "a preview-only raw is scored on the camera's JPEG");
    }

    /// The scan writes a flag only onto an unflagged photo (or one whose flag an earlier scan
    /// wrote), and only when asked to.
    #[test]
    fn flags_follow_the_verdict_and_the_users_choice() {
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "JPEG", 4, 4, "2026-01-01");
        let accepted = QualityScan { score: 90.0, verdict: QualityVerdict::Accepted, ..QualityScan::default() };
        let rejected = QualityScan { score: 10.0, verdict: QualityVerdict::Rejected, ..QualityScan::default() };
        assert_eq!(flag_after(&p, None, &accepted, false), Flag::None, "not asked to flag");
        assert_eq!(flag_after(&p, None, &accepted, true), Flag::Pick);
        assert_eq!(flag_after(&p, None, &rejected, true), Flag::Reject);
        // a flag the user set stays
        p.flag = Flag::Pick;
        assert_eq!(flag_after(&p, None, &rejected, true), Flag::Pick, "the user's flag stays");
        // ... unless it is the one a scan wrote: a rescan may change its own
        let ours = QualityScan { flag_written: Some(Flag::Pick), ..accepted };
        assert_eq!(flag_after(&p, Some(ours), &rejected, true), Flag::Reject, "the scan's own flag may change");
        // and the flag of the scan before this one is not the user's either way
        p.flag = Flag::Reject;
        assert_eq!(flag_after(&p, Some(ours), &accepted, true), Flag::Reject, "the flag is not the scan's record");
        // a verdict between the cutoffs asks for nothing
        p.flag = Flag::None;
        let pending = QualityScan { score: 50.0, verdict: QualityVerdict::Pending, ..QualityScan::default() };
        assert_eq!(flag_after(&p, None, &pending, true), Flag::None);
    }
}
