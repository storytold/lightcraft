//! AI masks: Object (clicks) and Describe (text) selections computed with SAM 3
//! (`lightcraft-segment`, cargo feature `sam`; the desktop app enables it).
//!
//! **Nothing requires the model.** It is not part of LightCraft (SAM License); the user
//! downloads it when they first use an AI mask and agree to (`segment.model.download`), or
//! puts the files in the model folder themselves. Without it, AI mask requests fail with a
//! clear "not installed" error; renders, exports and every other feature never touch it
//! (stored segmentations are plain data in the develop settings).
//!
//! The model sees the photo as developed, uncropped and without masks, at 1008 px — the frame
//! mask coordinates live in — so a stored segmentation lines up with the photo at any size.
//! Everything that touches the model (loading it, rendering and encoding the photo, clicks,
//! descriptions, detail passes) runs on one worker thread ([`worker`]); the session talks to it
//! over channels and never waits on a lock. With [`Segmenter::background`] (the desktop app)
//! commands return at once and [`Session::segment_poll`] applies the results; without it (CLI,
//! MCP, tests) they wait for the worker.

#[cfg(feature = "sam")]
mod download;
#[cfg(feature = "sam")]
mod worker;

use std::path::PathBuf;

use lightcraft_catalog::PhotoId;
use lightcraft_develop::{MaskShape, SegMask};
use lightcraft_geom::Point;

use crate::Session;

/// Long edge of the image the model sees.
pub const INPUT_EDGE: usize = 1008;
/// Download size of the model (`model.safetensors`; the tokenizer files add ~2 MB).
pub const MODEL_BYTES: u64 = 3_439_938_512;
/// The model's licence (not LightCraft's): shown before downloading.
pub const LICENSE_NAME: &str = "SAM License (Meta)";
pub const LICENSE_URL: &str = "https://github.com/facebookresearch/sam3/blob/main/LICENSE";
/// How errors about a missing model start (frontends decide on [`crate::ErrorKind::ModelMissing`], not on this text).
pub const NOT_INSTALLED: &str = "The SAM 3 model is not installed";
/// Longest a waiting command (CLI, MCP) waits for the model (loading on a CPU can take minutes).
#[cfg_attr(not(feature = "sam"), allow(dead_code))]
const WAIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// Most clicks one Object selection takes (the model's prompt grows with each).
pub const MAX_CLICKS: usize = 64;

/// One click of an Object selection (normalized image coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Click {
    pub at: Point,
    pub include: bool,
}

/// What a request's result is for (how [`Session::segment_poll`] applies it).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "sam"), allow(dead_code))]
pub(crate) enum Tag {
    /// A command waiting for the result itself.
    Wait,
    /// Silent background model/photo warmup at startup or when switching photos.
    Warmup,
    /// Load the model and encode the photo ahead of the first click.
    Prepare,
    /// Object clicks on component `comp` of mask `mask`.
    Clicks { photo: PhotoId, mask: u32, comp: usize, hint: Vec<Point>, exclude: Vec<Point> },
    /// A Describe selection: a new mask (`mask: None`) or a component combined by `op`.
    NewPrompt { photo: PhotoId, mask: Option<u32>, op: String, name: Option<String>, text: String },
    /// An automatic AI concept selection (Subject, Sky, Background): updates `target: Some((mask, comp))`
    /// created immediately in background mode, or creates a new mask/component when `target: None`.
    NewConcept { photo: PhotoId, mask: Option<u32>, target: Option<(u32, usize)>, op: String, name: Option<String>, kind: String, invert: bool },
    /// A zoomed-in pass for a component, computed from `shape` (without its detail).
    Detail { photo: PhotoId, mask: u32, comp: usize, shape: MaskShape },
}

/// A background job currently queued or running on the SAM 3 worker thread.
#[cfg_attr(not(feature = "sam"), allow(dead_code))]
struct InFlight {
    id: u64,
    tag: Tag,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    detail: bool,
}

/// The model input: 8-bit RGB.
#[cfg_attr(not(feature = "sam"), allow(dead_code))]
pub(crate) struct Input {
    rgb: Vec<u8>,
    w: usize,
    h: usize,
}

/// What a detail pass re-runs.
#[derive(Clone)]
#[cfg_attr(not(feature = "sam"), allow(dead_code))]
pub(crate) enum Prompt {
    Clicks(Vec<Click>),
    Text(String),
}

/// Object clicks sent to the worker and not applied yet (background mode): further clicks add
/// to these, and the loupe shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingClicks {
    pub photo: PhotoId,
    pub mask: u32,
    pub comp: usize,
    pub hint: Vec<Point>,
    pub exclude: Vec<Point>,
}

/// What [`Session::segment_poll`] did.
#[derive(Debug, Default)]
pub struct Polled {
    /// A mask changed.
    pub changed: bool,
    /// Errors and notices from background work, for the user.
    pub messages: Vec<String>,
    /// A mask that got a new selection (worth a detail pass).
    pub refine: Option<u32>,
}

/// The model download (`segment.model.*`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct DownloadStatus {
    pub running: bool,
    pub done: u64,
    pub total: u64,
    pub file: String,
    pub error: Option<String>,
    /// The last download was cancelled (it resumes next time): decide on this, not on `error`'s text.
    pub cancelled: bool,
    pub finished: bool,
}

/// The model (on its worker thread), the download, and AI mask requests in flight.
pub struct Segmenter {
    /// Where the `facebook/sam3` files live (set by the app; `None`: no AI masks).
    pub dir: Option<PathBuf>,
    /// The user's list of download mirrors (one base URL per line), if any.
    pub mirrors_file: Option<PathBuf>,
    /// Run requests in the background and apply them in [`Session::segment_poll`] (the
    /// desktop app); otherwise commands wait for their result (CLI, MCP, tests).
    pub background: bool,
    /// Optional remote AI worker endpoint override (host:port).
    pub remote: Option<String>,
    pending: Option<PendingClicks>,
    messages: Vec<String>,
    #[cfg_attr(not(feature = "sam"), allow(dead_code))]
    next_job_id: u64,
    #[cfg_attr(not(feature = "sam"), allow(dead_code))]
    in_flight: Vec<InFlight>,
    #[cfg_attr(not(feature = "sam"), allow(dead_code))]
    warmup_cancel: Option<(u64, std::sync::Arc<std::sync::atomic::AtomicBool>)>,
    #[cfg(feature = "sam")]
    worker: worker::Worker,
    #[cfg(feature = "sam")]
    download: download::Downloader,
    #[cfg(feature = "sam")]
    results: (std::sync::mpsc::Sender<worker::Outcome>, std::sync::mpsc::Receiver<worker::Outcome>),
}

// (derivable only without the `sam` feature: the results channel has no Default)
#[allow(clippy::derivable_impls)]
impl Default for Segmenter {
    fn default() -> Self {
        Segmenter {
            dir: None,
            mirrors_file: None,
            background: false,
            remote: None,
            pending: None,
            messages: Vec::new(),
            next_job_id: 1,
            in_flight: Vec::new(),
            warmup_cancel: None,
            #[cfg(feature = "sam")]
            worker: worker::Worker::default(),
            #[cfg(feature = "sam")]
            download: download::Downloader::default(),
            #[cfg(feature = "sam")]
            results: std::sync::mpsc::channel(),
        }
    }
}

impl Segmenter {
    /// Whether this build can compute AI masks at all.
    pub const AVAILABLE: bool = cfg!(feature = "sam");

    /// The configured remote AI worker endpoint, if any.
    pub fn remote_endpoint(&self) -> Option<String> {
        if let Some(r) = &self.remote {
            return Some(r.clone());
        }
        #[cfg(feature = "sam")]
        return lightcraft_segment::remote::configured();
        #[cfg(not(feature = "sam"))]
        None
    }

    /// Whether the local model files are in place.
    pub fn installed(&self) -> bool {
        #[cfg(feature = "sam")]
        if let Some(dir) = &self.dir {
            return lightcraft_segment::is_model_dir(dir);
        }
        false
    }

    /// Whether the SAM 3 model is available to segment, either locally installed or through a configured remote worker.
    pub fn has_model(&self) -> bool {
        self.installed() || self.remote_endpoint().is_some()
    }

    /// Whether requests are queued or running (loading, analyzing the photo, a click…).
    pub fn busy(&self) -> bool {
        #[cfg(feature = "sam")]
        {
            if self.background {
                return self.in_flight.iter().any(|j| !j.detail) && self.worker.pending() > 0;
            }
            self.worker.pending() > 0
        }
        #[cfg(not(feature = "sam"))]
        false
    }

    /// Whether the slow part runs now: loading the model or analyzing a photo.
    pub fn analyzing(&self) -> bool {
        #[cfg(feature = "sam")]
        {
            if self.background && self.in_flight.is_empty() {
                return false;
            }
            self.worker.encoding()
        }
        #[cfg(not(feature = "sam"))]
        false
    }

    /// Whether a zoomed-in detail pass is queued or running.
    pub fn detail_busy(&self) -> bool {
        #[cfg(feature = "sam")]
        {
            if self.background {
                return self.in_flight.iter().any(|j| j.detail) && self.worker.detail() > 0;
            }
            self.worker.detail() > 0
        }
        #[cfg(not(feature = "sam"))]
        false
    }

    /// Immediately cancel all in-flight background AI requests (e.g. when the user deletes
    /// all masks, switches tools, or presses the Cancel button next to the spinner).
    pub fn cancel_all(&mut self) -> bool {
        let had = !self.in_flight.is_empty() || self.pending.is_some();
        for job in self.in_flight.drain(..) {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.pending = None;
        had
    }

    /// Immediately cancel any in-flight AI requests targeting `mask` on `photo`.
    pub fn cancel_mask(&mut self, photo: PhotoId, mask: u32) -> bool {
        let mut cancelled = false;
        self.in_flight.retain(|job| {
            let matches_mask = match &job.tag {
                Tag::Clicks { photo: p, mask: m, .. } | Tag::Detail { photo: p, mask: m, .. } => *p == photo && *m == mask,
                Tag::NewPrompt { photo: p, mask: Some(m), .. } => *p == photo && *m == mask,
                Tag::NewConcept { photo: p, mask: m, target, .. } => *p == photo && (*m == Some(mask) || target.is_some_and(|(tm, _)| tm == mask)),
                _ => false,
            };
            if matches_mask {
                job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                cancelled = true;
                false
            } else {
                true
            }
        });
        if self.pending.as_ref().is_some_and(|p| p.photo == photo && p.mask == mask) {
            self.pending = None;
            cancelled = true;
        }
        cancelled
    }

    /// Whether the model is in memory (it is unloaded after a while without use).
    pub fn loaded(&self) -> bool {
        #[cfg(feature = "sam")]
        return self.worker.loaded();
        #[cfg(not(feature = "sam"))]
        false
    }

    /// Clicks waiting for their segmentation.
    pub fn pending_clicks(&self) -> Option<&PendingClicks> {
        self.pending.as_ref()
    }

    /// The model download's state.
    pub fn download_status(&self) -> DownloadStatus {
        #[cfg(feature = "sam")]
        {
            let s = self.download.status();
            DownloadStatus {
                running: s.running,
                done: s.done,
                total: s.total,
                file: s.file,
                error: s.error,
                cancelled: s.cancelled,
                finished: s.finished,
            }
        }
        #[cfg(not(feature = "sam"))]
        DownloadStatus::default()
    }

    /// Where the model is downloaded from, in order (the user's mirrors first).
    pub fn mirrors(&self) -> Vec<String> {
        #[cfg(feature = "sam")]
        {
            let env = std::env::var(lightcraft_segment::fetch::MIRRORS_ENV).ok();
            lightcraft_segment::fetch::mirrors(env.as_deref(), self.mirrors_file.as_deref())
        }
        #[cfg(not(feature = "sam"))]
        Vec::new()
    }

    /// The model folder, if the model is there; otherwise an error that says what to do.
    pub fn model_dir(&self) -> Result<PathBuf, String> {
        self.require_model().map_err(|e| e.to_string())
    }

    /// [`Segmenter::model_dir`] for commands: a model that is not installed (or still downloading) is
    /// [`crate::EngineError::ModelMissing`], so a frontend offers the download without reading the message.
    pub fn require_model(&self) -> crate::Result<PathBuf> {
        let other = |why: &str| crate::EngineError::Other(why.into());
        if !Self::AVAILABLE {
            return Err(other("AI masks are not available in this build"));
        }
        #[cfg(feature = "sam")]
        if self.remote_endpoint().is_some() {
            return Ok(PathBuf::new());
        }
        let dir = self.dir.clone().ok_or_else(|| other("no folder is set for the SAM 3 model"))?;
        if self.installed() {
            return Ok(dir);
        }
        let d = self.download_status();
        let msg = if d.running {
            let pct = d.done.saturating_mul(100).checked_div(d.total).unwrap_or(0);
            format!("{NOT_INSTALLED} yet: it is downloading ({pct} %).")
        } else {
            format!(
                "{NOT_INSTALLED}. Download it (about {:.1} GB, {LICENSE_NAME}) when LightCraft offers it, with `segment.model.download {{\"acknowledged\": true}}`, or put model.safetensors, vocab.json and merges.txt in {}.",
                MODEL_BYTES as f64 / 1e9,
                dir.display()
            )
        };
        Err(crate::EngineError::ModelMissing { model: crate::AiModel::Sam3, msg })
    }

    /// Start downloading the model (on a background thread; see `segment.model.download`).
    /// `Ok(false)` when it is installed or downloading already.
    pub fn start_download(&self, activity: &crate::activity::Activity) -> Result<bool, String> {
        if !Self::AVAILABLE {
            return Err("AI masks are not available in this build".into());
        }
        let dir = self.dir.clone().ok_or("no folder is set for the SAM 3 model")?;
        if self.installed() {
            return Ok(false);
        }
        #[cfg(feature = "sam")]
        {
            let mirrors = self.mirrors();
            if mirrors.is_empty() {
                return Err(lightcraft_segment::fetch::no_mirrors_message());
            }
            self.download.start(lightcraft_segment::fetch::SAM3_FILES, mirrors, dir, lightcraft_segment::fetch::options(), activity, "SAM 3")
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (dir, activity);
            Ok(false)
        }
    }

    /// Stop a running download (its partial files stay, to resume). False when none runs.
    pub fn cancel_download(&self) -> bool {
        #[cfg(feature = "sam")]
        return self.download.cancel();
        #[cfg(not(feature = "sam"))]
        false
    }
}

/// Mix `v` into a 64-bit hash.
#[cfg_attr(not(feature = "sam"), allow(dead_code))]
fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x0100_0000_01b3).rotate_left(29)
}

/// Objects covering more than this much of the photo gain little from a zoomed pass.
const DETAIL_MAX_AREA: f64 = 0.4;

/// The part of the photo to zoom into for `seg`'s selection: its bounding box with a margin of
/// a quarter of its size, at least 12 % of the long edge, at most 2:1, inside the photo
/// (`aspect` = width / height). `None` when nothing is selected or it is most of the photo.
pub fn detail_region(seg: &SegMask, aspect: f64) -> Option<[f64; 4]> {
    let logits = seg.logits()?;
    let r = seg.bounds()?;
    let side = seg.side as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, l) in logits.iter().enumerate() {
        if *l > 0.0 {
            let (x, y) = (i % side, i / side);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
        }
    }
    if x0 == usize::MAX {
        return None;
    }
    let (rw, rh) = (r[2] - r[0], r[3] - r[1]);
    let nx = |c: usize| r[0] + c as f64 / side as f64 * rw;
    let ny = |c: usize| r[1] + c as f64 / side as f64 * rh;
    let (bx0, by0, bx1, by1) = (nx(x0), ny(y0), nx(x1), ny(y1));
    if (bx1 - bx0) * (by1 - by0) > DETAIL_MAX_AREA {
        return None;
    }
    // in units of the photo's height: x × aspect
    let a = if aspect.is_finite() { aspect.clamp(0.1, 10.0) } else { 1.0 };
    let (cx, cy) = ((bx0 + bx1) / 2.0 * a, (by0 + by1) / 2.0);
    let size = ((bx1 - bx0) * a).max(by1 - by0);
    let m = 0.25 * size;
    let mut w = ((bx1 - bx0) * a + 2.0 * m).max(0.12 * a.max(1.0));
    let mut h = ((by1 - by0) + 2.0 * m).max(0.12 * a.max(1.0));
    w = w.max(h / 2.0).min(a);
    h = h.max(w / 2.0).min(1.0);
    let x = (cx - w / 2.0).clamp(0.0, (a - w).max(0.0));
    let y = (cy - h / 2.0).clamp(0.0, (1.0 - h).max(0.0));
    let out = [x / a, y, (x + w) / a, y + h];
    // a region nearly the whole photo isn't worth a second pass
    ((out[2] - out[0]) * (out[3] - out[1]) < 0.8).then_some(out)
}

/// `shape` without its detail patches (what a detail pass was computed from)
#[cfg_attr(not(feature = "sam"), allow(dead_code))]
fn strip_detail(mut shape: MaskShape) -> MaskShape {
    // (nor its edge setting: moving the slider meanwhile keeps the pass)
    match &mut shape {
        MaskShape::Object { detail, edge, .. }
        | MaskShape::Prompt { detail, edge, .. }
        | MaskShape::Subject { detail, edge, .. }
        | MaskShape::Sky { detail, edge, .. }
        | MaskShape::Background { detail, edge, .. } => {
            detail.clear();
            *edge = 0.0;
        }
        _ => {}
    }
    shape
}

fn clicks_of(include: &[Point], exclude: &[Point]) -> Vec<Click> {
    include.iter().map(|p| Click { at: *p, include: true }).chain(exclude.iter().map(|p| Click { at: *p, include: false })).collect()
}

impl Session {
    /// The key and settings of photo `id` as the model sees it: its look without masks, uncropped.
    #[cfg_attr(not(feature = "sam"), allow(dead_code))]
    fn segment_key(&self, id: PhotoId) -> Option<(u64, lightcraft_develop::DevelopSettings)> {
        let p = self.catalog.photo(id)?;
        let mut d = (*p.develop).clone();
        d.masks.clear();
        let content = crate::media::content_key(p);
        let mut h = mix(0xcbf2_9ce4_8422_2325, d.hash64());
        h = mix(h, id.0);
        for b in content.bytes() {
            h = mix(h, u64::from(b));
        }
        Some((h, d))
    }

    /// Clicks (in normalized photo coordinates) for the model.
    #[cfg(feature = "sam")]
    fn model_clicks(clicks: &[Click]) -> Vec<lightcraft_segment::Click> {
        clicks.iter().map(|c| lightcraft_segment::Click { x: c.at.x as f32, y: c.at.y as f32, positive: c.include }).collect()
    }

    /// Cancel any in-flight AI requests whose target photo, mask, component, or spot no longer
    /// exists (for example after `mask.delete`, `spot.delete`, or `edit.undo`).
    pub fn reconcile_segmenter(&mut self) {
        let active = self.active();
        let active_dev = active.and_then(|id| self.develop_of(id));
        let mut keep_pending = true;
        if let Some(p) = &self.segmenter.pending {
            let exists = active == Some(p.photo)
                && active_dev.as_ref().is_some_and(|d| d.masks.iter().find(|m| m.id == p.mask).is_some_and(|m| p.comp < m.components.len()));
            if !exists {
                keep_pending = false;
            }
        }
        if !keep_pending {
            self.segmenter.pending = None;
        }
        let mut i = 0;
        while i < self.segmenter.in_flight.len() {
            let valid = match &self.segmenter.in_flight[i].tag {
                Tag::Wait | Tag::Warmup | Tag::Prepare => true,
                Tag::Clicks { photo, mask, comp, .. } | Tag::Detail { photo, mask, comp, .. } => {
                    active == Some(*photo)
                        && self.develop_of(*photo).is_some_and(|d| d.masks.iter().find(|m| m.id == *mask).is_some_and(|m| *comp < m.components.len()))
                }
                Tag::NewPrompt { photo, mask: Some(mask), .. } => {
                    active == Some(*photo) && self.develop_of(*photo).is_some_and(|d| d.masks.iter().any(|m| m.id == *mask))
                }
                Tag::NewPrompt { photo, mask: None, .. } => active == Some(*photo),
                Tag::NewConcept { photo, mask, target, kind, .. } => {
                    active == Some(*photo)
                        && match target {
                            Some((tm, tc)) => self.develop_of(*photo).is_some_and(|d| {
                                d.masks.iter().find(|m| m.id == *tm).and_then(|m| m.components.get(*tc)).is_some_and(|c| {
                                    matches!(
                                        (kind.as_str(), &c.shape),
                                        ("subject", MaskShape::Subject { seg: None, .. })
                                            | ("sky", MaskShape::Sky { seg: None, .. })
                                            | ("background", MaskShape::Background { seg: None, .. })
                                    )
                                })
                            }),
                            None => match mask {
                                Some(m) => self.develop_of(*photo).is_some_and(|d| d.masks.iter().any(|x| x.id == *m)),
                                None => true,
                            },
                        }
                }
            };
            if valid {
                i += 1;
            } else {
                let job = self.segmenter.in_flight.remove(i);
                job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    /// A worker request about photo `id` (the render job for its model input goes along; the
    /// worker runs it only when its cache doesn't hold the photo). `region` renders that part
    /// of the photo, from a source sharp enough for it (detail passes).
    #[cfg(feature = "sam")]
    fn segment_job(
        &mut self,
        id: PhotoId,
        kind: worker::Kind,
        tag: Tag,
        reply: std::sync::mpsc::Sender<worker::Outcome>,
        region: Option<[f64; 4]>,
    ) -> Result<worker::Job, String> {
        let dir = self.segmenter.model_dir()?;
        let (key, mut d) = self.segment_key(id).ok_or("no such photo")?;
        let p = self.catalog.photo(id).ok_or("no such photo")?.clone();
        let missing = match &p.source {
            lightcraft_catalog::Source::File { path } if !std::path::Path::new(path).exists() => Some(path.clone()),
            _ => None,
        };
        let render = match region {
            None => self.preview_job(id, INPUT_EDGE, INPUT_EDGE, false, &d).ok_or("no such photo")?,
            Some(region) => {
                d.crop.geometry =
                    lightcraft_geom::CropGeometry { rect: lightcraft_geom::Rect::new(region[0], region[1], region[2], region[3]), angle: 0.0 };
                d.crop.flip_h = false;
                d.crop.flip_v = false;
                d.geometry.constrain_crop = false;
                let mut job = self.preview_job(id, INPUT_EDGE, INPUT_EDGE, true, &d).ok_or("no such photo")?;
                let frac = (region[2] - region[0]).max(region[3] - region[1]).max(0.01);
                job.level = crate::media::SourceLevel::for_size((INPUT_EDGE as f64 / frac) as usize);
                job.source = self.media.source_ref(&p, job.level);
                job
            }
        };
        let job_id = self.segmenter.next_job_id;
        self.segmenter.next_job_id = self.segmenter.next_job_id.wrapping_add(1).max(1);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let remote = self.segmenter.remote_endpoint();
        Ok(worker::Job {
            id: job_id,
            cancel,
            dir,
            key: if region.is_some() { 0 } else { key },
            render: Some(render),
            missing,
            kind,
            tag,
            reply,
            remote,
        })
    }

    /// Send a request and wait for its result (CLI, MCP, tests; or when not in background mode).
    #[cfg(feature = "sam")]
    fn segment_wait(&mut self, id: PhotoId, kind: worker::Kind, region: Option<[f64; 4]>) -> Result<Option<SegMask>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = self.segment_job(id, kind, Tag::Wait, tx, region)?;
        self.segmenter.worker.submit(job)?;
        match rx.recv_timeout(WAIT) {
            Ok(o) => {
                // (shown by the next poll in an app; logged anyway)
                self.segmenter.messages.extend(o.notice);
                o.result
            }
            Err(_) => Err("the AI model took too long; try again".into()),
        }
    }

    /// Send a request whose result [`Session::segment_poll`] applies.
    #[cfg(feature = "sam")]
    fn segment_queue(&mut self, id: PhotoId, kind: worker::Kind, tag: Tag, region: Option<[f64; 4]>) -> Result<(), String> {
        let reply = self.segmenter.results.0.clone();
        let detail = matches!(kind, worker::Kind::Detail { .. });
        let job = self.segment_job(id, kind, tag.clone(), reply, region)?;
        if let Some((warm_key, cancel)) = &self.segmenter.warmup_cancel
            && *warm_key != job.key
        {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            self.segmenter.warmup_cancel = None;
        }
        let job_id = job.id;
        let cancel = job.cancel.clone();
        self.segmenter.worker.submit(job)?;
        self.segmenter.in_flight.push(InFlight { id: job_id, tag, cancel, detail });
        Ok(())
    }

    /// The cache key of the currently active photo as SAM 3 sees it, or `None` when no photo is active.
    pub fn active_segment_key(&self) -> Option<u64> {
        let id = self.active()?;
        Some(self.segment_key(id)?.0)
    }

    /// Whether the active photo's settings are already warmed up or warming up in SAM 3.
    pub fn is_segment_warmed(&self) -> bool {
        let key = self.active().and_then(|id| self.segment_key(id).map(|(k, _)| k)).unwrap_or(0);
        self.segmenter.warmup_cancel.as_ref().is_some_and(|(k, _)| *k == key && (key != 0 || self.segmenter.loaded()))
    }

    /// Pre-load the SAM 3 model on its background worker thread (and pre-encode the active photo if one
    /// is selected) so the model and image embeddings are ready in memory before the user's first AI
    /// mask or object-remove action. Does nothing if the model is not installed.
    pub fn segment_warmup(&mut self) {
        if !self.segmenter.has_model() {
            return;
        }
        #[cfg(feature = "sam")]
        {
            let Ok(dir) = self.segmenter.model_dir() else { return };
            let active_id = self.active();
            let key = active_id.and_then(|id| self.segment_key(id).map(|(k, _)| k)).unwrap_or(0);
            if let Some((prev_key, prev_cancel)) = &self.segmenter.warmup_cancel {
                if *prev_key == key && (key != 0 || self.segmenter.loaded()) {
                    return;
                }
                prev_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let (render, missing) = active_id
                .and_then(|id| {
                    let (_, d) = self.segment_key(id)?;
                    let p = self.catalog.photo(id)?;
                    let missing = match &p.source {
                        lightcraft_catalog::Source::File { path } if !std::path::Path::new(path).exists() => Some(path.clone()),
                        _ => None,
                    };
                    let render = self.preview_job(id, INPUT_EDGE, INPUT_EDGE, false, &d)?;
                    Some((Some(render), missing))
                })
                .unwrap_or_default();
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.segmenter.warmup_cancel = Some((key, cancel.clone()));
            let reply = self.segmenter.results.0.clone();
            let remote = self.segmenter.remote_endpoint();
            let job = worker::Job { id: 0, cancel, dir, key, render, missing, kind: worker::Kind::Warmup, tag: Tag::Warmup, reply, remote };
            let _ = self.segmenter.worker.submit(job);
        }
    }

    /// Start preparing photo `id` for AI masks (load the model, encode the image), so the first
    /// click is fast. In background mode it returns at once.
    pub fn segment_prepare(&mut self, id: PhotoId) -> Result<(), String> {
        self.segmenter.model_dir()?;
        #[cfg(feature = "sam")]
        {
            if self.segmenter.background {
                return self.segment_queue(id, worker::Kind::Prepare, Tag::Prepare, None);
            }
            self.segment_wait(id, worker::Kind::Prepare, None)?;
        }
        #[cfg(not(feature = "sam"))]
        let _ = id;
        Ok(())
    }

    /// Segment the object `clicks` pick on photo `id`, waiting for the model.
    pub fn segment_clicks(&mut self, id: PhotoId, clicks: &[Click]) -> Result<SegMask, String> {
        self.segmenter.model_dir()?;
        if !clicks.iter().any(|c| c.include) {
            return Err("click the object to include it first".into());
        }
        #[cfg(feature = "sam")]
        {
            self.segment_wait(id, worker::Kind::Clicks(Self::model_clicks(clicks)), None)?.ok_or_else(|| "nothing was selected".to_string())
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = id;
            Err("AI masks are not available in this build".into())
        }
    }

    /// Segment everything `text` describes on photo `id`, waiting for the model; `Ok(None)`
    /// when nothing matches.
    pub fn segment_text(&mut self, id: PhotoId, text: &str) -> Result<Option<SegMask>, String> {
        self.segmenter.model_dir()?;
        let text = text.trim();
        if text.is_empty() {
            return Err("describe what to select".into());
        }
        #[cfg(feature = "sam")]
        {
            self.segment_wait(id, worker::Kind::Text(text.to_string()), None)
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = id;
            Err("AI masks are not available in this build".into())
        }
    }

    /// Background mode: queue Object clicks for component `comp` of mask `mask`; the result
    /// (clicks and segmentation, one undo step) is applied by [`Session::segment_poll`].
    pub(crate) fn segment_clicks_later(&mut self, id: PhotoId, mask: u32, comp: usize, hint: Vec<Point>, exclude: Vec<Point>) -> Result<(), String> {
        self.segmenter.model_dir()?;
        if hint.is_empty() {
            return Err("click the object to include it first".into());
        }
        #[cfg(feature = "sam")]
        {
            let clicks = Self::model_clicks(&clicks_of(&hint, &exclude));
            let tag = Tag::Clicks { photo: id, mask, comp, hint: hint.clone(), exclude: exclude.clone() };
            self.segment_queue(id, worker::Kind::Clicks(clicks), tag, None)?;
            self.segmenter.pending = Some(PendingClicks { photo: id, mask, comp, hint, exclude });
            Ok(())
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (id, mask, comp, exclude);
            Err("AI masks are not available in this build".into())
        }
    }

    /// Background mode: queue a Describe selection; the mask (or component, combined by `op`
    /// into mask `mask`) is created by [`Session::segment_poll`] when something matches.
    pub(crate) fn segment_text_later(&mut self, id: PhotoId, mask: Option<u32>, op: &str, name: Option<String>, text: &str) -> Result<(), String> {
        self.segmenter.model_dir()?;
        let text = text.trim();
        if text.is_empty() {
            return Err("describe what to select".into());
        }
        #[cfg(feature = "sam")]
        {
            let tag = Tag::NewPrompt { photo: id, mask, op: op.to_string(), name, text: text.to_string() };
            self.segment_queue(id, worker::Kind::Text(text.to_string()), tag, None)
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (id, mask, op, name);
            Err("AI masks are not available in this build".into())
        }
    }

    /// Background mode: queue an automatic AI selection (Subject, Sky, Background); updates the
    /// immediately-created `target` mask/component (or creates one if `target` is `None`) when
    /// the model finishes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn segment_concept_later(
        &mut self,
        id: PhotoId,
        mask: Option<u32>,
        target: Option<(u32, usize)>,
        op: &str,
        name: Option<String>,
        kind: &str,
        text: &str,
        invert: bool,
    ) -> Result<(), String> {
        self.segmenter.model_dir()?;
        let text = text.trim();
        #[cfg(feature = "sam")]
        {
            let tag = Tag::NewConcept { photo: id, mask, target, op: op.to_string(), name, kind: kind.to_string(), invert };
            self.segment_queue(id, worker::Kind::Text(text.to_string()), tag, None)
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (id, mask, target, op, name, kind, text, invert);
            Err("AI masks are not available in this build".into())
        }
    }

    /// Start a zoomed-in pass for component `comp` of mask `mask` on photo `id` (an Object or
    /// Describe selection): the photo around the selection is rendered at a higher resolution
    /// and segmented again. In background mode [`Session::segment_poll`] applies the result;
    /// otherwise it is applied before this returns. `Ok(false)` when there is nothing to
    /// refine (no selection yet, or it is most of the photo) or a pass is running already.
    pub fn segment_detail(&mut self, id: PhotoId, mask: u32, comp: usize) -> Result<bool, String> {
        self.segmenter.model_dir()?;
        let d = self.develop_of(id).ok_or("no such photo")?;
        let shape = d.masks.iter().find(|m| m.id == mask).and_then(|m| m.components.get(comp)).map(|c| c.shape.clone()).ok_or("no such mask")?;
        let (seg, prompt) = match &shape {
            // (a document with absurdly many clicks isn't run through the model)
            MaskShape::Object { hint, exclude, seg: Some(seg), .. } if !hint.is_empty() && hint.len() + exclude.len() <= MAX_CLICKS => {
                (seg.clone(), Prompt::Clicks(clicks_of(hint, exclude)))
            }
            MaskShape::Prompt { text, seg: Some(seg), .. } => (seg.clone(), Prompt::Text(text.clone())),
            MaskShape::Subject { seg: Some(seg), .. } => (seg.clone(), Prompt::Text(crate::cmd::SUBJECT_CONCEPT.into())),
            MaskShape::Sky { seg: Some(seg), .. } => (seg.clone(), Prompt::Text("sky".into())),
            _ => return Ok(false),
        };
        let p = self.catalog.photo(id).ok_or("no such photo")?;
        let (pw, ph) = (f64::from(p.width.max(1)), f64::from(p.height.max(1)));
        let aspect = if d.orientation.swaps_axes() { ph / pw } else { pw / ph };
        let Some(region) = detail_region(&seg, aspect) else { return Ok(false) };
        #[cfg(feature = "sam")]
        {
            let kind = worker::Kind::Detail { prompt, region };
            let tag = Tag::Detail { photo: id, mask, comp, shape: strip_detail(shape) };
            if self.segmenter.background {
                if self.segmenter.detail_busy() {
                    return Ok(false);
                }
                self.segment_queue(id, kind, tag, Some(region))?;
            } else {
                let patch = self.segment_wait(id, kind, Some(region))?;
                let outcome = worker::Outcome { id: 0, tag, result: Ok(patch), superseded: false, notice: None };
                let mut polled = Polled::default();
                self.apply_outcome(outcome, &mut polled);
            }
            Ok(true)
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (prompt, region, mask);
            Ok(false)
        }
    }

    /// Apply finished background requests (to masks that haven't changed since) and collect
    /// their messages. Cheap when nothing is pending: call it every frame.
    pub fn segment_poll(&mut self) -> Polled {
        self.reconcile_segmenter();
        #[cfg_attr(not(feature = "sam"), allow(unused_mut))]
        let mut polled = Polled { messages: std::mem::take(&mut self.segmenter.messages), ..Default::default() };
        #[cfg(feature = "sam")]
        {
            // (before draining: a worker that is idle now has sent everything)
            let idle = !self.segmenter.busy();
            let done: Vec<worker::Outcome> = self.segmenter.results.1.try_iter().collect();
            for o in done {
                self.apply_outcome(o, &mut polled);
            }
            // clicks whose request was lost (the worker restarted) aren't pending forever
            if idle {
                self.segmenter.pending = None;
            }
        }
        polled
    }

    #[cfg(feature = "sam")]
    fn apply_outcome(&mut self, o: worker::Outcome, polled: &mut Polled) {
        if o.id != 0 {
            let was_tracked = self.segmenter.in_flight.iter().any(|j| j.id == o.id);
            self.segmenter.in_flight.retain(|j| j.id != o.id);
            if !was_tracked {
                // Job was explicitly cancelled before its outcome arrived; ignore silently.
                return;
            }
        }
        if o.superseded {
            return;
        }
        polled.messages.extend(o.notice);
        match o.tag {
            Tag::Wait | Tag::Warmup => {}
            Tag::Prepare => {
                if let Err(e) = o.result {
                    polled.messages.push(e);
                }
            }
            Tag::Clicks { photo, mask, comp, hint, exclude } => {
                let latest = PendingClicks { photo, mask, comp, hint, exclude };
                if self.segmenter.pending.as_ref() != Some(&latest) {
                    return; // newer clicks are on their way
                }
                self.segmenter.pending = None;
                let seg = match o.result {
                    Ok(Some(seg)) => seg,
                    Ok(None) => return,
                    Err(e) => return polled.messages.push(e),
                };
                let Some(d) = self.develop_of(photo) else { return };
                let mut d = (*d).clone();
                let Some(c) = d.masks.iter_mut().find(|m| m.id == mask).and_then(|m| m.components.get_mut(comp)) else { return };
                let MaskShape::Object { edge, .. } = c.shape else { return };
                c.shape = MaskShape::Object { hint: latest.hint, exclude: latest.exclude, seg: Some(seg), detail: vec![], edge };
                match self.set_develop(photo, d, "Object Mask") {
                    Ok(()) => {
                        polled.changed = true;
                        polled.refine = Some(mask);
                    }
                    Err(e) => polled.messages.push(e.to_string()),
                }
            }
            Tag::NewPrompt { photo, mask, op, name, text } => {
                let seg = match o.result {
                    Ok(Some(seg)) => seg,
                    Ok(None) => return polled.messages.push(format!("Nothing matching “{text}” was found in this photo.")),
                    Err(e) => return polled.messages.push(e),
                };
                if self.active() != Some(photo) {
                    return polled.messages.push(format!("The photo changed before “{text}” was found; try again."));
                }
                let Ok(seg) = serde_json::to_value(seg) else { return };
                let mut p = serde_json::json!({"kind": "prompt", "text": text, "seg": seg});
                if let Some(n) = name {
                    p["name"] = serde_json::json!(n);
                }
                let r = match mask {
                    None => self.execute("mask.add", &p),
                    Some(m) => {
                        if !self.develop_of(photo).is_some_and(|d| d.masks.iter().any(|x| x.id == m)) {
                            return polled.messages.push("The mask was removed before the selection was found.".into());
                        }
                        self.active_mask = Some(m);
                        p["op"] = serde_json::json!(op);
                        self.execute("mask.addComponent", &p)
                    }
                };
                match r {
                    Ok(_) => {
                        polled.changed = true;
                        polled.refine = self.active_mask;
                    }
                    Err(e) => polled.messages.push(e.to_string()),
                }
            }
            Tag::NewConcept { photo, mask, target, op, name, kind, invert } => {
                let seg = match o.result {
                    Ok(Some(mut seg)) => {
                        if invert {
                            let Some(inv) = seg.invert() else { return };
                            seg = inv;
                        }
                        seg
                    }
                    Ok(None) => return polled.messages.push(format!("No {kind} was found in this photo.")),
                    Err(e) => return polled.messages.push(e),
                };
                if self.active() != Some(photo) {
                    return polled.messages.push(format!("The photo changed before {kind} was found; try again."));
                }
                if let Some((tm, tc)) = target {
                    let Some(d) = self.develop_of(photo) else { return };
                    let mut d = (*d).clone();
                    let Some(c) = d.masks.iter_mut().find(|m| m.id == tm).and_then(|m| m.components.get_mut(tc)) else {
                        return;
                    };
                    match (&mut c.shape, kind.as_str()) {
                        (MaskShape::Subject { seg: s_dst, detail, .. }, "subject")
                        | (MaskShape::Sky { seg: s_dst, detail, .. }, "sky")
                        | (MaskShape::Background { seg: s_dst, detail, .. }, "background")
                            if s_dst.is_none() =>
                        {
                            *s_dst = Some(seg);
                            detail.clear();
                        }
                        _ => return,
                    }
                    match self.set_develop(photo, d, "AI Mask") {
                        Ok(()) => {
                            polled.changed = true;
                            polled.refine = Some(tm);
                        }
                        Err(e) => polled.messages.push(e.to_string()),
                    }
                    return;
                }
                let Ok(seg_val) = serde_json::to_value(seg) else { return };
                let mut p = serde_json::json!({"kind": kind, "seg": seg_val});
                if let Some(n) = name {
                    p["name"] = serde_json::json!(n);
                }
                let r = match mask {
                    None => self.execute("mask.add", &p),
                    Some(m) => {
                        if !self.develop_of(photo).is_some_and(|d| d.masks.iter().any(|x| x.id == m)) {
                            return polled.messages.push("The mask was removed before the selection was found.".into());
                        }
                        self.active_mask = Some(m);
                        p["op"] = serde_json::json!(op);
                        self.execute("mask.addComponent", &p)
                    }
                };
                match r {
                    Ok(_) => {
                        polled.changed = true;
                        polled.refine = self.active_mask;
                    }
                    Err(e) => polled.messages.push(e.to_string()),
                }
            }
            Tag::Detail { photo, mask, comp, shape } => {
                let patch = match o.result {
                    Ok(Some(p)) => p,
                    Ok(None) => return,
                    Err(e) => return log::warn!("SAM 3 detail pass: {e}"),
                };
                let Some(d) = self.develop_of(photo) else { return };
                let mut d = (*d).clone();
                let Some(c) = d.masks.iter_mut().find(|m| m.id == mask).and_then(|m| m.components.get_mut(comp)) else { return };
                if strip_detail(c.shape.clone()) != shape {
                    return; // clicked again meanwhile: a newer pass will follow
                }
                match &mut c.shape {
                    MaskShape::Object { detail, .. }
                    | MaskShape::Prompt { detail, .. }
                    | MaskShape::Subject { detail, .. }
                    | MaskShape::Sky { detail, .. }
                    | MaskShape::Background { detail, .. } => *detail = vec![patch],
                    _ => return,
                }
                match self.set_develop(photo, d, "AI Mask Detail") {
                    Ok(()) => polled.changed = true,
                    Err(e) => log::warn!("AI mask detail: {e}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "sam")]
    #[test]
    fn a_worker_notice_reaches_the_user_with_the_result() {
        // the job after which the model runs on the CPU (the GPU failed) says so
        let mut s = Session::with_demo();
        let notice = Some(worker::GPU_FAILED.to_string());
        s.segmenter.results.0.send(worker::Outcome { id: 0, tag: Tag::Prepare, result: Ok(None), superseded: false, notice }).unwrap();
        let polled = s.segment_poll();
        assert_eq!(polled.messages, vec![worker::GPU_FAILED.to_string()]);
        assert!(s.segment_poll().messages.is_empty(), "once");
    }

    #[cfg(feature = "sam")]
    #[test]
    fn concept_masks_use_sam_when_installed() {
        let mut s = Session::with_demo();
        s.segmenter.background = true;
        let dir = std::env::temp_dir().join(format!("lc-test-sam-concept-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.safetensors"), b"\x10\x00\x00\x00\x00\x00\x00\x00{}").unwrap();
        std::fs::write(dir.join("vocab.json"), b"{}").unwrap();
        std::fs::write(dir.join("merges.txt"), b"#version: 0.2\n").unwrap();
        s.segmenter.dir = Some(dir.clone());
        assert!(s.segmenter.installed());

        let r = s.execute("mask.add", &serde_json::json!({"kind": "subject"})).unwrap();
        assert_eq!(r["pending"], true, "subject mask should queue in background mode");
        let mid1 = r["id"].as_u64().unwrap() as u32;

        let r = s.execute("mask.add", &serde_json::json!({"kind": "sky"})).unwrap();
        assert_eq!(r["pending"], true, "sky mask should queue in background mode");

        let r = s.execute("mask.add", &serde_json::json!({"kind": "background"})).unwrap();
        assert_eq!(r["pending"], true, "background mask should queue in background mode");
        assert_eq!(s.segmenter.in_flight.len(), 3, "three jobs queued");

        let photo = s.active().unwrap();
        let seg = SegMask::from_logits(4, &[5.0; 16]);
        s.segmenter
            .results
            .0
            .send(worker::Outcome {
                id: 0,
                tag: Tag::NewConcept {
                    photo,
                    mask: None,
                    target: Some((mid1, 0)),
                    op: "add".into(),
                    name: None,
                    kind: "subject".into(),
                    invert: false,
                },
                result: Ok(Some(seg)),
                superseded: false,
                notice: None,
            })
            .unwrap();

        let polled = s.segment_poll();
        assert!(polled.changed);
        let d = s.develop_of(photo).unwrap();
        assert_eq!(d.masks[0].id, mid1);
        assert_eq!(d.masks[0].name, "Mask 1");
        let lightcraft_develop::MaskShape::Subject { seg: Some(ref s_seg), .. } = d.masks[0].components[0].shape else {
            panic!("expected Subject with Some(seg)");
        };
        assert_eq!(s_seg.side, 4);

        // Deleting a mask while AI is still running immediately cancels its in-flight job and clears busy()!
        s.execute("mask.deleteAll", &serde_json::json!({})).unwrap();
        assert!(s.segmenter.in_flight.is_empty(), "all in-flight jobs cancelled immediately");
        assert!(!s.segmenter.busy(), "segmenter is no longer busy after deleting masks");
        assert!(!s.segmenter.analyzing(), "segmenter is no longer analyzing after deleting masks");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "sam")]
    #[test]
    fn concept_masks_reconciliation_validates_identity_and_avoids_overwriting_mismatched_component() {
        let mut s = Session::with_demo();
        s.segmenter.background = true;
        let dir = std::env::temp_dir().join(format!("lc-test-sam-identity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.safetensors"), b"\x10\x00\x00\x00\x00\x00\x00\x00{}").unwrap();
        std::fs::write(dir.join("vocab.json"), b"{}").unwrap();
        std::fs::write(dir.join("merges.txt"), b"#version: 0.2\n").unwrap();
        s.segmenter.dir = Some(dir.clone());

        // Queue a Subject mask in background mode: target is (mid1, 0)
        let r = s.execute("mask.add", &serde_json::json!({"kind": "subject"})).unwrap();
        let mid1 = r["id"].as_u64().unwrap() as u32;
        assert_eq!(s.segmenter.in_flight.len(), 1);

        // Replace component 0 with a Sky component
        let photo = s.active().unwrap();
        let mut d = (*s.develop_of(photo).unwrap()).clone();
        d.masks[0].components[0].shape = MaskShape::sky();
        s.set_develop(photo, d, "change to sky").unwrap();

        // Reconcile should detect the identity change and cancel the in-flight job
        s.reconcile_segmenter();
        assert!(s.segmenter.in_flight.is_empty(), "in-flight job should be cancelled on identity mismatch");

        // Even if an outcome arrives for the original Subject job targeting (mid1, 0),
        // poll_segmenter must NOT overwrite the Sky component!
        let seg = SegMask::from_logits(4, &[5.0; 16]);
        s.segmenter
            .results
            .0
            .send(worker::Outcome {
                id: 0,
                tag: Tag::NewConcept {
                    photo,
                    mask: None,
                    target: Some((mid1, 0)),
                    op: "add".into(),
                    name: None,
                    kind: "subject".into(),
                    invert: false,
                },
                result: Ok(Some(seg)),
                superseded: false,
                notice: None,
            })
            .unwrap();

        let polled = s.segment_poll();
        assert!(!polled.changed, "mismatched component shape must not be modified");
        let d = s.develop_of(photo).unwrap();
        let lightcraft_develop::MaskShape::Sky { seg: sky_seg, .. } = &d.masks[0].components[0].shape else {
            panic!("component 0 should remain Sky");
        };
        assert!(sky_seg.is_none(), "sky component must not be overwritten by subject segmentation");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn seg_box(side: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> SegMask {
        let l: Vec<f32> =
            (0..side * side).map(|i| if (x0..x1).contains(&(i % side)) && (y0..y1).contains(&(i / side)) { 8.0 } else { -8.0 }).collect();
        SegMask::from_logits(side, &l)
    }

    #[test]
    fn detail_region_frames_the_selection_with_a_margin() {
        // a small object in the middle of a square photo
        let r = detail_region(&seg_box(100, 40, 45, 50, 55), 1.0).unwrap();
        assert!(r[0] < 0.4 && r[2] > 0.5 && r[1] < 0.45 && r[3] > 0.55, "contains it: {r:?}");
        assert!(r[2] - r[0] < 0.25 && r[3] - r[1] < 0.25, "zoomed in: {r:?}");
        assert!(r.iter().all(|v| (0.0..=1.0).contains(v)));
        // at the corner: shifted inside the photo, not cut
        let r = detail_region(&seg_box(100, 0, 0, 5, 5), 1.5).unwrap();
        assert!(r[0] >= 0.0 && r[1] >= 0.0 && r[0] < 1e-9 && r[1] < 1e-9);
        // a thin object stays at most 2:1 (in photo pixels, 1.5:1 photo)
        let r = detail_region(&seg_box(100, 10, 50, 90, 52), 1.5).unwrap();
        let (w, h) = ((r[2] - r[0]) * 1.5, r[3] - r[1]);
        assert!(w / h <= 2.0 + 1e-9, "{w} × {h}");
        // a hostile aspect ratio stays sane
        assert!(detail_region(&seg_box(100, 40, 45, 50, 55), f64::NAN).is_some_and(|r| r.iter().all(|v| v.is_finite())));
    }

    #[test]
    fn no_region_for_nothing_or_most_of_the_photo() {
        assert!(detail_region(&seg_box(50, 0, 0, 0, 0), 1.0).is_none());
        assert!(detail_region(&seg_box(50, 2, 2, 48, 48), 1.0).is_none());
        let damaged = SegMask { side: 50, data: "?".into(), rect: None };
        assert!(detail_region(&damaged, 1.0).is_none());
    }
}
