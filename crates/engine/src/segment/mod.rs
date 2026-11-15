//! AI masks: Object (clicks) and Describe (text) selections computed with SAM 3, or with the
//! openly licensed models when it isn't installed (Object: SAM 2.1; Describe: the OWLv2 object
//! finder's boxes segmented by SAM 2.1) (`lcc-segment`, cargo feature `sam`; the desktop app
//! enables it).
//!
//! **Nothing requires the model.** It is not part of the app (SAM License); the user
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

pub mod depth;
#[cfg(feature = "sam")]
mod download;
pub mod models;
mod parts;
pub mod scene;
mod skin;
pub mod subject;
#[cfg(feature = "sam")]
mod worker;

#[cfg(feature = "sam")]
pub(crate) use skin::face_model;
pub use skin::{FoundFace, SkinFound};
pub(crate) use skin::{SkinKey, SkinPurpose};

use std::path::PathBuf;

use lcc_catalog::PhotoId;
use lcc_develop::{MaskShape, SegMask};
use lcc_geom::Point;

use crate::Session;

/// Long edge of the image the model sees.
pub const INPUT_EDGE: usize = 1008;
/// Download size of the model (`model.safetensors`; the tokenizer files add ~2 MB).
pub const MODEL_BYTES: u64 = 3_439_938_512;
/// The model's licence (not the app's): shown before downloading.
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
    /// Load the model and encode the photo ahead of the first click.
    Prepare,
    /// Object clicks on component `comp` of mask `mask`.
    Clicks { photo: PhotoId, mask: u32, comp: usize, hint: Vec<Point>, exclude: Vec<Point> },
    /// A Describe selection: a new mask (`mask: None`) or a component combined by `op`.
    NewPrompt { photo: PhotoId, mask: Option<u32>, op: String, name: Option<String>, text: String },
    /// A zoomed-in pass for a component, computed from `shape` (without its detail).
    Detail { photo: PhotoId, mask: u32, comp: usize, shape: MaskShape },
    /// Recompute AI Masks: the selection of component `comp` of mask `mask`, computed from
    /// `shape` (the recipe, without a segmentation), for run `run`.
    Recompute { run: u64, photo: PhotoId, mask: u32, comp: usize, shape: MaskShape },
    /// `concepts.teach`: an example of remembered object `name` learned on `photo`.
    Learn { photo: PhotoId, name: String },
    /// A remembered object found on `photo`: a new mask (`mask: None`) or a component combined by
    /// `op`, named after it.
    Concept { photo: PhotoId, mask: Option<u32>, op: String, name: String },
    /// The faces of `photo` (look `key`) with their appearance signatures; `learn`: remember the
    /// named ones in `people.json`.
    Faces { photo: PhotoId, key: u64, learn: bool },
}

/// Where the models for appearance signatures are ([`crate::identity`]).
#[cfg_attr(not(feature = "sam"), allow(dead_code))]
pub(crate) struct FacesKind {
    pub face: PathBuf,
    pub sam2: Option<PathBuf>,
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
    /// Requests sent now are background work ([`worker::Job::low`]: the user's go first). Set while work the user
    /// didn't ask for right now sends its photos' masks, skin and retouch parts (an import's finishing queue).
    pub low_priority: bool,
    /// The Subject / Background model (BiRefNet).
    pub subject: subject::Subject,
    /// Where the openly licensed models (SAM 2.1, Depth Anything) live; `None`: next to SAM 3's folder.
    pub models_root: Option<PathBuf>,
    /// Which model computes Object clicks ([`models::ObjectModel`]).
    object_model: models::ObjectModel,
    /// Which model computes Sky, Landscape and People ([`models::SceneModel`]).
    scene_model: models::SceneModel,
    /// The scene model as installed in the pipeline.
    pub scene: scene::SceneMasks,
    /// The depth model as installed in the pipeline (Depth Range masks).
    pub depth: depth::DepthMasks,
    #[cfg(feature = "sam")]
    model_downloads: std::collections::HashMap<&'static str, download::Downloader>,
    pending: Option<PendingClicks>,
    /// Remove ▸ Detect Objects, background mode: the Object mask `(photo, mask, spot mode)` whose
    /// clicks are queued becomes a Remove spot when its selection arrives (`spot.detectObject`).
    pub(crate) then_remove: Option<(PhotoId, u32, String)>,
    messages: Vec<String>,
    /// Smooth Skin and Find Blemishes requests ([`skin`]).
    pub(crate) skin: skin::SkinJobs,
    /// Retouch ▸ Teeth requests ([`parts`]).
    pub(crate) parts: parts::RetouchJobs,
    /// Photos whose Object / Describe selections should be computed at the next poll (a preset,
    /// paste or redo brought them without a segmentation).
    pub(crate) wanted: Vec<PhotoId>,
    /// The running Recompute AI Masks batch (one activity row).
    recompute: Option<RecomputeRun>,
    #[cfg_attr(not(feature = "sam"), allow(dead_code))]
    run_seq: u64,
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
            low_priority: false,
            subject: subject::Subject::default(),
            models_root: None,
            object_model: models::ObjectModel::default(),
            scene_model: models::SceneModel::default(),
            scene: scene::SceneMasks::default(),
            depth: depth::DepthMasks::default(),
            #[cfg(feature = "sam")]
            model_downloads: std::collections::HashMap::new(),
            pending: None,
            then_remove: None,
            messages: Vec::new(),
            skin: skin::SkinJobs::default(),
            parts: parts::RetouchJobs::default(),
            wanted: Vec::new(),
            recompute: None,
            run_seq: 0,
            #[cfg(feature = "sam")]
            worker: worker::Worker::default(),
            #[cfg(feature = "sam")]
            download: download::Downloader::default(),
            #[cfg(feature = "sam")]
            results: std::sync::mpsc::channel(),
        }
    }
}

/// A Recompute AI Masks batch on the worker: its activity row (✕ sets `cancel`, which the worker
/// checks before each job) and the selections still on their way.
#[cfg_attr(not(feature = "sam"), allow(dead_code))]
struct RecomputeRun {
    id: u64,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    guard: crate::activity::TaskGuard,
    photos: Vec<PhotoId>,
    left: usize,
    total: usize,
    /// The selections each photo still has on their way: the row counts photos, "1 of 3" (OneSlips 9bb601286).
    per_photo: std::collections::HashMap<PhotoId, usize>,
}

/// An AI-mask batch row's progress and detail (OneSlips 9bb601286, test notes 2026-10-25 note 6): `done` of `total`
/// photos have their masks. While one of its photos is at work it counts that one ("1 of 3", never "0 of 3"); before
/// its first photo starts, an indeterminate bar (`0, 0`) and what it waits for: the import's background work, or other
/// AI masks. → (done, total, detail).
#[cfg_attr(not(feature = "sam"), allow(dead_code))]
pub(crate) fn batch_row(done: usize, total: usize, mine_running: bool, other_running: bool, other_is_background: bool) -> (u64, u64, &'static str) {
    let n = |v: usize| u64::try_from(v).unwrap_or(u64::MAX);
    if mine_running {
        return (n(done.saturating_add(1).min(total)), n(total), "");
    }
    if done == 0 && total > 0 {
        let what = match (other_running, other_is_background) {
            (true, true) => WAITING_FOR_PREP,
            (true, false) => WAITING_FOR_MASKS,
            (false, _) => "",
        };
        return (0, 0, what);
    }
    (n(done.min(total)), n(total), "")
}

/// What an AI-mask batch row says while the worker finishes the import's background work first.
pub const WAITING_FOR_PREP: &str = "Waiting for Getting photos ready";
/// What an AI-mask batch row says while other AI masks run first.
pub const WAITING_FOR_MASKS: &str = "Waiting for other AI masks";

impl Segmenter {
    /// Whether a Recompute AI Masks batch is computing.
    pub fn recomputing(&self) -> bool {
        self.recompute.is_some()
    }

    /// Stop the Recompute AI Masks batch (all of it, or only when it works on `photo`): queued
    /// selections are skipped and late results dropped.
    pub(crate) fn cancel_recompute(&mut self, photo: Option<PhotoId>) {
        if self.recompute.as_ref().is_some_and(|r| photo.is_none_or(|p| r.photos.contains(&p)))
            && let Some(r) = self.recompute.take()
        {
            r.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        self.wanted.retain(|w| photo.is_some_and(|p| p != *w));
    }

    /// Whether this build can compute AI masks at all.
    pub const AVAILABLE: bool = cfg!(feature = "sam");

    pub fn remote_endpoint(&self) -> Option<String> {
        #[cfg(feature = "sam")]
        return lcc_segment::remote::configured();
        #[cfg(not(feature = "sam"))]
        None
    }

    /// Whether the local model files are in place.
    pub fn installed(&self) -> bool {
        #[cfg(feature = "sam")]
        if let Some(dir) = &self.dir {
            return lcc_segment::is_model_dir(dir);
        }
        false
    }

    /// Whether requests are queued or running (loading, analyzing the photo, a click…).
    pub fn busy(&self) -> bool {
        #[cfg(feature = "sam")]
        return self.worker.pending() > 0;
        #[cfg(not(feature = "sam"))]
        false
    }

    /// Whether the slow part runs now: loading the model or analyzing a photo.
    pub fn analyzing(&self) -> bool {
        #[cfg(feature = "sam")]
        return self.worker.encoding();
        #[cfg(not(feature = "sam"))]
        false
    }

    /// Whether a zoomed-in detail pass is queued or running.
    pub fn detail_busy(&self) -> bool {
        #[cfg(feature = "sam")]
        return self.worker.detail() > 0;
        #[cfg(not(feature = "sam"))]
        false
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
            let env = lcc_brand::env(lcc_segment::fetch::MIRRORS_ENV);
            lcc_segment::fetch::mirrors(env.as_deref(), self.mirrors_file.as_deref())
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
        if lcc_segment::remote::configured().is_some() {
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
                "{NOT_INSTALLED}. Download it (about {:.1} GB, {LICENSE_NAME}) when the app offers it, with `segment.model.download {{\"acknowledged\": true}}`, or put model.safetensors, vocab.json and merges.txt in {}.",
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
                return Err(lcc_segment::fetch::no_mirrors_message());
            }
            self.download.start(lcc_segment::fetch::SAM3_FILES, Vec::new(), mirrors, dir, lcc_segment::fetch::options(), activity, "SAM 3")
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
    if let MaskShape::Object { detail, edge, .. } | MaskShape::Prompt { detail, edge, .. } = &mut shape {
        detail.clear();
        *edge = 0.0;
    }
    shape
}

fn clicks_of(include: &[Point], exclude: &[Point]) -> Vec<Click> {
    include.iter().map(|p| Click { at: *p, include: true }).chain(exclude.iter().map(|p| Click { at: *p, include: false })).collect()
}

impl Session {
    /// The key and settings of photo `id` as the model sees it: its look without masks, uncropped.
    #[cfg_attr(not(feature = "sam"), allow(dead_code))]
    fn segment_key(&self, id: PhotoId) -> Option<(u64, lcc_develop::DevelopSettings)> {
        let p = self.catalog.photo(id)?;
        let mut d = (*p.develop).clone();
        d.masks.clear();
        // masks live on the photo before lens corrections, Upright, Transform and the flips
        // (`Mask::on_photo`): the model sees it so, and its selections stay on the content
        d.crop.flip_h = false;
        d.crop.flip_v = false;
        d.geometry = Default::default();
        d.optics.lens_profile = false;
        d.optics.distortion = 0.0;
        d.optics.remove_ca = false;
        d.optics.ca_red = 0.0;
        d.optics.ca_blue = 0.0;
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
    fn model_clicks(clicks: &[Click]) -> Vec<lcc_segment::Click> {
        clicks.iter().map(|c| lcc_segment::Click { x: c.at.x as f32, y: c.at.y as f32, positive: c.include }).collect()
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
        // Describe (text) runs on SAM 3, else on the OWLv2 finder with SAM 2.1; Object clicks use the model the user
        // chose
        let text = matches!(&kind, worker::Kind::Text(_) | worker::Kind::Detail { prompt: Prompt::Text(_), .. });
        let concept = matches!(&kind, worker::Kind::Learn { .. } | worker::Kind::Find { .. });
        let mut detector = None;
        let dir = if let worker::Kind::Faces { face, .. } | worker::Kind::People { face, .. } = &kind {
            face.clone()
        } else if concept {
            self.segmenter.require_sam2().map_err(|e| e.to_string())?
        } else if text {
            match self.segmenter.describe_dir()? {
                models::DescribeModel::Sam3(dir) => dir,
                models::DescribeModel::Open { detector: d, sam2 } => {
                    detector = Some(d);
                    sam2
                }
            }
        } else {
            self.segmenter.object_dir()?
        };
        // old masks go onto the photo first: the selection that comes back is in its coordinates
        if let Some(cur) = self.develop_of(id).filter(|d| d.masks.iter().any(|m| !m.on_photo)) {
            let mut on = (*cur).clone();
            self.attach_masks(id, &cur, &mut on);
            let op = lcc_catalog::Op::SetDevelop { id, settings: std::sync::Arc::new(on), label: "Masks".into(), edited: None };
            self.apply_unrecorded(op).map_err(|e| e.to_string())?;
        }
        let (key, mut d) = self.segment_key(id).ok_or("no such photo")?;
        let p = self.catalog.photo(id).ok_or("no such photo")?.clone();
        let missing = match &p.source {
            lcc_catalog::Source::File { path } if !std::path::Path::new(path).exists() => Some(path.clone()),
            _ => None,
        };
        let render = match region {
            None => self.preview_job(id, INPUT_EDGE, INPUT_EDGE, false, &d).ok_or("no such photo")?,
            Some(region) => {
                d.crop.geometry = lcc_geom::CropGeometry { rect: lcc_geom::Rect::new(region[0], region[1], region[2], region[3]), angle: 0.0 };
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
        Ok(worker::Job {
            dir,
            detector,
            key: if region.is_some() { 0 } else { key },
            render: Some(render),
            missing,
            kind,
            tag,
            reply,
            cancel: None,
            low: self.segmenter.low_priority,
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

    /// The look key of photo `id` (what [`Session::segment_key`] gives).
    pub(crate) fn segment_key_of(&self, id: PhotoId) -> Option<u64> {
        self.segment_key(id).map(|(k, _)| k)
    }

    /// The faces of photo `id` with their signatures: waited for (→ the worker's list), or in
    /// background mode queued (→ `None`; [`Session::segment_poll`] keeps them).
    #[cfg(feature = "sam")]
    pub(crate) fn segment_faces(&mut self, id: PhotoId, kind: FacesKind, learn: bool) -> Result<Option<serde_json::Value>, String> {
        let key = self.segment_key_of(id).ok_or("no such photo")?;
        let tag = Tag::Faces { photo: id, key, learn };
        let kind = worker::Kind::Faces { face: kind.face, sam2: kind.sam2 };
        if self.segmenter.background {
            self.segment_queue(id, kind, tag, None)?;
            return Ok(None);
        }
        let o = self.segment_wait_outcome(id, kind, tag)?;
        o.result?;
        Ok(Some(o.extra.unwrap_or_else(|| serde_json::json!([]))))
    }

    /// [`Session::segment_wait`] with the whole outcome (a learned example, a found object's seeds).
    #[cfg(feature = "sam")]
    fn segment_wait_outcome(&mut self, id: PhotoId, kind: worker::Kind, tag: Tag) -> Result<worker::Outcome, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = self.segment_job(id, kind, tag, tx, None)?;
        self.segmenter.worker.submit(job)?;
        match rx.recv_timeout(WAIT) {
            Ok(o) => {
                self.segmenter.messages.extend(o.notice.clone());
                Ok(o)
            }
            Err(_) => Err("the AI model took too long; try again".into()),
        }
    }

    /// Learn an example of remembered object `name` from `mask` on photo `id` (SAM 2.1). In
    /// background mode → `None` and [`Session::segment_poll`] stores it; otherwise → how many
    /// examples the object has now.
    pub fn concept_teach(&mut self, id: PhotoId, mask: lcc_develop::Mask, name: &str) -> Result<Option<usize>, String> {
        self.segmenter.require_sam2().map_err(|e| e.to_string())?;
        #[cfg(feature = "sam")]
        {
            let kind = worker::Kind::Learn { mask: Box::new(mask) };
            let tag = Tag::Learn { photo: id, name: name.to_string() };
            if self.segmenter.background {
                self.segment_queue(id, kind, tag, None)?;
                return Ok(None);
            }
            let o = self.segment_wait_outcome(id, kind, tag)?;
            o.result?;
            let ex = o.extra.ok_or("the mask selects too little to learn from")?;
            self.store_concept_example(name, id, ex).map(Some)
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (id, mask, name);
            Err("remembered objects need the AI models, which this build doesn't have".into())
        }
    }

    /// Find remembered object `name` on photo `id`: a new mask (`mask: None`) or a component of
    /// `mask` combined by `op`, an Object selection grown from the clicks the object's look
    /// picked (editable like any). In background mode → `None` (applied by
    /// [`Session::segment_poll`]); otherwise the command's result.
    pub fn concept_find(&mut self, id: PhotoId, name: &str, mask: Option<u32>, op: &str) -> Result<Option<serde_json::Value>, String> {
        let examples: Vec<serde_json::Value> =
            self.concept_store().get(name).map(|c| c.examples.clone()).ok_or_else(|| format!("no remembered object called “{name}”"))?;
        self.segmenter.require_sam2().map_err(|e| e.to_string())?;
        #[cfg(feature = "sam")]
        {
            let examples: Vec<lcc_segment::concept::Example> = examples.into_iter().filter_map(|e| serde_json::from_value(e).ok()).collect();
            if examples.is_empty() {
                return Err(format!("“{name}” has no examples yet: teach it on a photo first"));
            }
            let name = self.concept_store().get(name).map(|c| c.name.clone()).unwrap_or_else(|| name.to_string());
            let kind = worker::Kind::Find { examples };
            let tag = Tag::Concept { photo: id, mask, op: op.to_string(), name: name.clone() };
            if self.segmenter.background {
                self.segment_queue(id, kind, tag, None)?;
                return Ok(None);
            }
            let o = self.segment_wait_outcome(id, kind, tag)?;
            let seg = o.result?.ok_or_else(|| format!("“{name}” wasn't found on this photo"))?;
            self.add_found_concept(id, mask, op, &name, seg, o.extra.as_ref()).map(Some)
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (id, examples, mask, op);
            Err("remembered objects need the AI models, which this build doesn't have".into())
        }
    }

    /// The examples of remembered object `name`, for the worker.
    #[cfg(feature = "sam")]
    fn concept_examples(&mut self, name: &str) -> Result<Vec<lcc_segment::concept::Example>, String> {
        let examples: Vec<serde_json::Value> =
            self.concept_store().get(name).map(|c| c.examples.clone()).ok_or_else(|| format!("no remembered object called “{name}”"))?;
        let examples: Vec<lcc_segment::concept::Example> = examples.into_iter().filter_map(|e| serde_json::from_value(e).ok()).collect();
        if examples.is_empty() {
            return Err(format!("“{name}” has no examples yet: teach it on a photo first"));
        }
        Ok(examples)
    }

    /// Remembered object `name` on photo `id` (a `MaskShape::Concept` recipe), waiting for SAM 2.1;
    /// `Ok(None)` when it isn't found there.
    pub fn segment_concept(&mut self, id: PhotoId, name: &str) -> Result<Option<SegMask>, String> {
        self.segmenter.require_sam2().map_err(|e| e.to_string())?;
        #[cfg(feature = "sam")]
        {
            let examples = self.concept_examples(name)?;
            self.segment_wait(id, worker::Kind::Find { examples }, None)
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (id, name);
            Err("remembered objects need the AI models, which this build doesn't have".into())
        }
    }

    /// A found remembered object becomes an Object selection (its seeds are the clicks), as a new
    /// mask named after it or a component of `mask`.
    #[cfg(feature = "sam")]
    fn add_found_concept(
        &mut self,
        id: PhotoId,
        mask: Option<u32>,
        op: &str,
        name: &str,
        seg: SegMask,
        extra: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        if self.active() != Some(id) {
            return Err(format!("The photo changed before “{name}” was found; try again."));
        }
        let seeds = extra.and_then(|e| e.get("seeds")).cloned().unwrap_or_else(|| serde_json::json!([]));
        let seg = serde_json::to_value(seg).map_err(|e| e.to_string())?;
        let mut p = serde_json::json!({"kind": "object", "points": seeds, "seg": seg, "name": name});
        let r = match mask {
            None => self.execute("mask.add", &p),
            Some(m) => {
                if !self.develop_of(id).is_some_and(|d| d.masks.iter().any(|x| x.id == m)) {
                    return Err("The mask was removed before the object was found.".into());
                }
                self.active_mask = Some(m);
                p["op"] = serde_json::json!(op);
                self.execute("mask.addComponent", &p)
            }
        };
        r.map_err(|e| e.to_string())
    }

    /// Send a request whose result [`Session::segment_poll`] applies.
    #[cfg(feature = "sam")]
    fn segment_queue(&mut self, id: PhotoId, kind: worker::Kind, tag: Tag, region: Option<[f64; 4]>) -> Result<(), String> {
        let reply = self.segmenter.results.0.clone();
        let job = self.segment_job(id, kind, tag, reply, region)?;
        self.segmenter.worker.submit(job)
    }

    /// Start preparing photo `id` for AI masks (load the model, encode the image), so the first
    /// click is fast. In background mode it returns at once.
    pub fn segment_prepare(&mut self, id: PhotoId) -> Result<(), String> {
        self.segmenter.object_dir()?;
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
        self.segmenter.object_dir()?;
        if !clicks.iter().any(|c| c.include) {
            return Err("click the object to include it first".into());
        }
        if clicks.len() > MAX_CLICKS {
            return Err(format!("an Object selection takes at most {MAX_CLICKS} clicks"));
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
        self.segmenter.describe_dir()?;
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

    /// The People selection `person` / `parts` on photo `id` from MediaPipe's people models,
    /// waiting for them; `Ok(None)` when they aren't installed (the pipeline's heuristic shows) or
    /// nobody, or none of the parts, is there.
    pub fn segment_people(&mut self, id: PhotoId, person: u32, parts: &[String]) -> Result<Option<SegMask>, String> {
        let Some(face) = self.segmenter.people_models() else { return Ok(None) };
        #[cfg(feature = "sam")]
        {
            let sam2 = self.segmenter.require_sam2().ok();
            self.segment_wait(id, worker::Kind::People { face, person, parts: parts.to_vec(), sam2 }, None)
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (id, face, person, parts);
            Ok(None)
        }
    }

    /// Background mode: queue Object clicks for component `comp` of mask `mask`; the result
    /// (clicks and segmentation, one undo step) is applied by [`Session::segment_poll`].
    pub(crate) fn segment_clicks_later(&mut self, id: PhotoId, mask: u32, comp: usize, hint: Vec<Point>, exclude: Vec<Point>) -> Result<(), String> {
        self.segmenter.object_dir()?;
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

    /// Remove the Object mask a Detect Objects stroke made when nothing came of it.
    #[cfg(feature = "sam")]
    fn drop_detect_mask(&mut self, photo: PhotoId, mask: u32) {
        if let Some(d) = self.develop_of(photo) {
            let mut d = (*d).clone();
            d.masks.retain(|m| m.id != mask);
            let _ = self.set_develop(photo, d, "Remove");
        }
    }

    /// Background mode: queue a Describe selection; the mask (or component, combined by `op`
    /// into mask `mask`) is created by [`Session::segment_poll`] when something matches.
    pub(crate) fn segment_text_later(&mut self, id: PhotoId, mask: Option<u32>, op: &str, name: Option<String>, text: &str) -> Result<(), String> {
        self.segmenter.describe_dir()?;
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

    /// Start a zoomed-in pass for component `comp` of mask `mask` on photo `id` (an Object or
    /// Describe selection): the photo around the selection is rendered at a higher resolution
    /// and segmented again. In background mode [`Session::segment_poll`] applies the result;
    /// otherwise it is applied before this returns. `Ok(false)` when there is nothing to
    /// refine (no selection yet, or it is most of the photo) or a pass is running already.
    pub fn segment_detail(&mut self, id: PhotoId, mask: u32, comp: usize) -> Result<bool, String> {
        if self.segmenter.object_dir().is_err() {
            self.segmenter.describe_dir()?;
        }
        let d = self.develop_of(id).ok_or("no such photo")?;
        let shape = d.masks.iter().find(|m| m.id == mask).and_then(|m| m.components.get(comp)).map(|c| c.shape.clone()).ok_or("no such mask")?;
        let (seg, prompt) = match &shape {
            // (a document with absurdly many clicks isn't run through the model)
            MaskShape::Object { hint, exclude, seg: Some(seg), .. } if !hint.is_empty() && hint.len() + exclude.len() <= MAX_CLICKS => {
                (seg.clone(), Prompt::Clicks(clicks_of(hint, exclude)))
            }
            MaskShape::Prompt { text, seg: Some(seg), .. } => (seg.clone(), Prompt::Text(text.clone())),
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
                let outcome = worker::Outcome { tag, result: Ok(patch), superseded: false, notice: None, extra: None };
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

    /// Queue the Object and Describe selections of photo `id` that have no segmentation, as part of
    /// the Recompute AI Masks batch (started when none runs). → how many were queued.
    pub(crate) fn segment_missing_later(&mut self, id: PhotoId) -> Result<usize, String> {
        let Some(d) = self.develop_of(id) else { return Ok(0) };
        let todo: Vec<(u32, usize, MaskShape)> = d
            .masks
            .iter()
            .flat_map(|m| m.components.iter().enumerate().map(move |(ci, c)| (m.id, ci, c.shape.clone())))
            .filter(|(_, _, sh)| sh.stores_seg() && !sh.has_seg())
            .collect();
        // People selections run on MediaPipe's people and faces models; without them the
        // pipeline's heuristic shows (nothing to queue)
        let people_face = self.segmenter.people_models();
        let todo: Vec<(u32, usize, MaskShape)> =
            todo.into_iter().filter(|(_, _, sh)| people_face.is_some() || !matches!(sh, MaskShape::People { .. })).collect();
        if todo.is_empty() {
            return Ok(0);
        }
        // each kind needs its own models: Describe SAM 3 or the open pair, Object the model the user chose
        if todo.iter().any(|(_, _, sh)| matches!(sh, MaskShape::Prompt { .. })) {
            self.segmenter.describe_dir()?;
        }
        if todo.iter().any(|(_, _, sh)| !matches!(sh, MaskShape::People { .. } | MaskShape::Prompt { .. })) {
            self.segmenter.model_dir()?;
        }
        #[cfg(feature = "sam")]
        {
            if self.segmenter.recompute.as_ref().is_some_and(|r| r.cancel.load(std::sync::atomic::Ordering::SeqCst)) {
                self.segmenter.recompute = None;
            }
            if self.segmenter.recompute.is_none() {
                self.segmenter.run_seq = self.segmenter.run_seq.wrapping_add(1);
                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let guard = self.activity.start_batch("aiMasks", "Computing AI masks", crate::activity::Cancel::Flag(cancel.clone()));
                self.segmenter.recompute = Some(RecomputeRun {
                    id: self.segmenter.run_seq,
                    cancel,
                    guard,
                    photos: Vec::new(),
                    left: 0,
                    total: 0,
                    per_photo: Default::default(),
                });
            }
            let (run, cancel) = match &self.segmenter.recompute {
                Some(r) => (r.id, r.cancel.clone()),
                None => return Ok(0),
            };
            let mut queued = 0;
            for (mask, comp, shape) in todo {
                let kind = match &shape {
                    MaskShape::Object { hint, exclude, .. } if !hint.is_empty() && hint.len() + exclude.len() <= MAX_CLICKS => {
                        worker::Kind::Clicks(Self::model_clicks(&clicks_of(hint, exclude)))
                    }
                    MaskShape::Prompt { text, .. } if !text.trim().is_empty() => worker::Kind::Text(text.trim().to_string()),
                    MaskShape::Concept { name, .. } => match self.concept_examples(name) {
                        Ok(examples) if self.segmenter.require_sam2().is_ok() => worker::Kind::Find { examples },
                        _ => continue,
                    },
                    MaskShape::People { person, parts, .. } => match &people_face {
                        Some(face) => worker::Kind::People {
                            face: face.clone(),
                            person: *person,
                            parts: parts.clone(),
                            sam2: self.segmenter.require_sam2().ok(),
                        },
                        None => continue,
                    },
                    _ => continue,
                };
                let tag = Tag::Recompute { run, photo: id, mask, comp, shape };
                let reply = self.segmenter.results.0.clone();
                let mut job = self.segment_job(id, kind, tag, reply, None)?;
                job.cancel = Some(cancel.clone());
                self.segmenter.worker.submit(job)?;
                queued += 1;
            }
            if let Some(r) = &mut self.segmenter.recompute {
                r.left += queued;
                r.total += queued;
                if queued > 0 {
                    if !r.photos.contains(&id) {
                        r.photos.push(id);
                    }
                    *r.per_photo.entry(id).or_default() += queued;
                }
                if r.left == 0 {
                    self.segmenter.recompute = None;
                }
            }
            self.update_recompute_row();
            Ok(queued)
        }
        #[cfg(not(feature = "sam"))]
        Ok(0)
    }

    /// Apply finished background requests (to masks that haven't changed since) and collect
    /// their messages. Cheap when nothing is pending: call it every frame.
    pub fn segment_poll(&mut self) -> Polled {
        #[cfg_attr(not(feature = "sam"), allow(unused_mut))]
        let mut polled = Polled { messages: std::mem::take(&mut self.segmenter.messages), ..Default::default() };
        self.skin_poll(&mut polled);
        // a finished EoMT download is put to use (only for those who chose it: two file checks)
        if self.segmenter.scene_model() == models::SceneModel::Eomt {
            let activity = self.activity.clone();
            self.segmenter.scene_sync(&activity);
        }
        // presets, pastes and redos that brought selections without a segmentation
        let wanted = std::mem::take(&mut self.segmenter.wanted);
        for id in wanted.into_iter().take(10_000) {
            if let Err(e) = self.segment_missing_later(id)
                && !polled.messages.contains(&e)
            {
                polled.messages.push(e);
            }
        }
        #[cfg(feature = "sam")]
        {
            // ✕ on the row: the worker skips the rest; the row goes now
            if self.segmenter.recompute.as_ref().is_some_and(|r| r.cancel.load(std::sync::atomic::Ordering::SeqCst)) {
                self.segmenter.recompute = None;
            }
            // Observe idleness and drain replies together: a finishing worker can't be between
            // decrementing its counter and sending its reply while this runs (upstream #4221).
            // While it is delivering one, the next poll takes it.
            if let Some((idle, done)) = self.segmenter.worker.completed(&self.segmenter.results.1) {
                // A lost click (the worker restarted) isn't pending forever; a reply to it in this
                // very poll isn't lost. Cleared before outcomes apply: one may queue a new click.
                let lost_clicks = idle
                    && self.segmenter.pending.as_ref().is_some_and(|pending| {
                        !done.iter().any(|o| {
                            !o.superseded
                                && matches!(&o.tag, Tag::Clicks { photo, mask, comp, hint, exclude }
                                    if *photo == pending.photo && *mask == pending.mask && *comp == pending.comp
                                        && *hint == pending.hint && *exclude == pending.exclude)
                        })
                    });
                if lost_clicks {
                    self.segmenter.pending = None;
                }
                // (nor is a batch whose replies were lost; one that replied now finishes itself)
                if idle && !done.iter().any(|o| matches!(o.tag, Tag::Recompute { .. })) {
                    self.segmenter.recompute = None;
                }
                for o in done {
                    self.apply_outcome(o, &mut polled);
                }
            }
            // what the batch's row says follows the worker: its photo at work, or what it waits for
            self.update_recompute_row();
        }
        polled
    }

    #[cfg(feature = "sam")]
    fn apply_outcome(&mut self, o: worker::Outcome, polled: &mut Polled) {
        if let Tag::Recompute { run, photo, mask, comp, shape } = o.tag {
            return self.apply_recomputed(run, photo, mask, comp, &shape, o.superseded, o.result, o.notice, polled);
        }
        if o.superseded {
            return;
        }
        polled.messages.extend(o.notice);
        match o.tag {
            Tag::Wait => {}
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
                // Remove ▸ Detect Objects: this selection is for a spot, not a mask of its own
                let removal = match &self.segmenter.then_remove {
                    Some((p, m, _)) if (*p, *m) == (photo, mask) => self.segmenter.then_remove.take().map(|(_, _, mode)| mode),
                    _ => None,
                };
                let seg = match o.result {
                    Ok(Some(seg)) => seg,
                    Ok(None) | Err(_) if removal.is_some() => {
                        self.drop_detect_mask(photo, mask);
                        let why = o.result.err().unwrap_or_else(|| "No object was found where you painted.".to_string());
                        polled.changed = true;
                        return polled.messages.push(why);
                    }
                    Ok(None) => return,
                    Err(e) => return polled.messages.push(e),
                };
                let Some(d) = self.develop_of(photo) else { return };
                let mut d = (*d).clone();
                let Some(c) = d.masks.iter_mut().find(|m| m.id == mask).and_then(|m| m.components.get_mut(comp)) else { return };
                let MaskShape::Object { edge, .. } = c.shape else { return };
                c.shape = MaskShape::Object { hint: latest.hint, exclude: latest.exclude, seg: Some(seg), detail: vec![], edge };
                match self.set_develop(photo, d, "Object Mask") {
                    Ok(()) if removal.is_some() => {
                        polled.changed = true;
                        let mode = removal.unwrap_or_default();
                        if let Err(e) = self.execute("spot.fromMask", &serde_json::json!({"mask": mask, "mode": mode, "deleteMask": true})) {
                            self.drop_detect_mask(photo, mask);
                            polled.messages.push(e.to_string());
                        }
                    }
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
            Tag::Recompute { .. } => {}
            Tag::Faces { photo, key, learn } => {
                if let Err(e) = o.result {
                    return polled.messages.push(e);
                }
                let faces = self.keep_appearance(key, &o.extra.unwrap_or_default());
                if learn && let Err(e) = self.learn_named_faces(photo, &faces) {
                    polled.messages.push(e);
                }
                // the looks are here: People masks by name or role are matched again (and
                // computed for whoever they now find)
                let by_who = self
                    .develop_of(photo)
                    .is_some_and(|d| d.masks.iter().flat_map(|m| &m.components).any(|c| matches!(c.shape, MaskShape::People { who: Some(_), .. })));
                if by_who {
                    // (the app's own doing: no undo step of its own)
                    match self.compute_masks_later_as(photo, false, false) {
                        Ok((done, _)) => {
                            polled.changed |= done.changed > 0;
                        }
                        Err(e) => polled.messages.push(e.to_string()),
                    }
                }
            }
            Tag::Learn { photo, name } => {
                if let Err(e) = o.result {
                    return polled.messages.push(e);
                }
                let Some(ex) = o.extra else { return polled.messages.push("The mask selects too little to learn from.".into()) };
                match self.store_concept_example(&name, photo, ex) {
                    Ok(n) => polled.messages.push(format!("Remembered “{name}” ({n} example{}).", if n == 1 { "" } else { "s" })),
                    Err(e) => polled.messages.push(e),
                }
            }
            Tag::Concept { photo, mask, op, name } => {
                let seg = match o.result {
                    Ok(Some(seg)) => seg,
                    Ok(None) => return polled.messages.push(format!("“{name}” wasn't found on this photo.")),
                    Err(e) => return polled.messages.push(e),
                };
                match self.add_found_concept(photo, mask, &op, &name, seg, o.extra.as_ref()) {
                    Ok(_) => {
                        polled.changed = true;
                        polled.refine = self.active_mask;
                    }
                    Err(e) => polled.messages.push(e),
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
                    MaskShape::Object { detail, .. } | MaskShape::Prompt { detail, .. } => *detail = vec![patch],
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

impl Session {
    /// A Recompute AI Masks result: stored in the component when it still is the selection it was
    /// computed for (an undo or an edit since drops it), without an undo step of its own (undoing
    /// the preset or paste that brought the mask takes it away with the mask).
    #[cfg(feature = "sam")]
    #[allow(clippy::too_many_arguments)]
    fn apply_recomputed(
        &mut self,
        run: u64,
        photo: PhotoId,
        mask: u32,
        comp: usize,
        shape: &MaskShape,
        skipped: bool,
        result: Result<Option<SegMask>, String>,
        notice: Option<String>,
        polled: &mut Polled,
    ) {
        let Some(r) = self.segmenter.recompute.as_mut().filter(|r| r.id == run) else { return };
        r.left = r.left.saturating_sub(1);
        // the row counts photos: this one is done once its last selection is
        if let Some(n) = r.per_photo.get_mut(&photo) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                r.per_photo.remove(&photo);
            }
        }
        let name = self.develop_of(photo).and_then(|d| d.masks.iter().find(|m| m.id == mask).map(|m| m.name.clone())).unwrap_or_default();
        let finished = self.segmenter.recompute.as_ref().is_some_and(|r| r.left == 0);
        let report = |s: &mut Session, why: Option<&str>| {
            if let (Some(r), Some(w)) = (&s.segmenter.recompute, why) {
                r.guard.count_failed(w);
            }
            s.update_recompute_row();
        };
        polled.messages.extend(notice);
        match (skipped, result) {
            (true, _) => {}
            (false, Ok(Some(seg))) => {
                // merged into the photo's settings as they are now (an edit made while it ran stays, OneSlips
                // 1c785da00 note 18), and into its undo / redo / History steps: only into the selection it was
                // computed for (undone or edited meanwhile, it takes none)
                let fill = |d: &mut lcc_develop::DevelopSettings| {
                    let Some(c) = d.masks.iter_mut().find(|m| m.id == mask).and_then(|m| m.components.get_mut(comp)) else { return false };
                    if c.shape != *shape {
                        return false;
                    }
                    match &mut c.shape {
                        MaskShape::Object { seg: s, .. }
                        | MaskShape::Prompt { seg: s, .. }
                        | MaskShape::People { seg: s, .. }
                        | MaskShape::Concept { seg: s, .. } => *s = Some(seg.clone()),
                        _ => return false,
                    }
                    true
                };
                match self.merge_background(photo, "AI Masks", true, &fill) {
                    Ok(true) => polled.changed = true,
                    Ok(false) => {}
                    Err(e) => polled.messages.push(e.to_string()),
                }
                report(self, None);
            }
            (false, Ok(None)) => report(self, Some(&format!("{name}: nothing matched"))),
            (false, Err(e)) => {
                report(self, Some(&format!("{name}: {e}")));
                if !polled.messages.contains(&e) {
                    polled.messages.push(e);
                }
            }
        }
        if finished {
            self.segmenter.recompute = None;
        } else {
            self.update_recompute_row();
        }
    }

    /// The Recompute AI Masks row (OneSlips 9bb601286): the photos done of its photos, the one at work counted, and
    /// before its first photo starts what it waits for ([`batch_row`]). Cheap: called as jobs are queued and finish,
    /// and on every poll while the batch runs.
    #[cfg(feature = "sam")]
    fn update_recompute_row(&self) {
        use std::sync::atomic::Ordering;
        let Some(r) = &self.segmenter.recompute else { return };
        let shared = &self.segmenter.worker.shared;
        let running = shared.running.load(Ordering::SeqCst);
        let low = shared.running_low.load(Ordering::SeqCst);
        let total = r.photos.len();
        let done = total.saturating_sub(r.per_photo.len());
        let mine = running == worker::run_code(r.id);
        let (d, t, what) = batch_row(done, total, mine, running != 0 && !mine, low);
        r.guard.progress(d, t);
        r.guard.detail(what);
    }

    #[cfg(not(feature = "sam"))]
    #[allow(dead_code)]
    fn update_recompute_row(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OneSlips 9bb601286 (test notes 2026-10-25 note 6): an AI-mask batch row counts photos, "1 of 3" while its
    /// first is at work (never "0 of 3"); before its first photo starts it shows an indeterminate bar and says what it
    /// waits for; hostile counts stay in range.
    #[test]
    fn a_batch_row_counts_photos_and_says_what_it_waits_for() {
        assert_eq!(batch_row(0, 3, false, true, true), (0, 0, WAITING_FOR_PREP), "behind the import's background work");
        assert_eq!(batch_row(0, 3, false, true, false), (0, 0, WAITING_FOR_MASKS), "behind other AI masks");
        assert_eq!(batch_row(0, 3, false, false, false), (0, 0, ""), "about to start");
        assert_eq!(batch_row(0, 3, true, false, false), (1, 3, ""), "its first photo at work: 1 of 3");
        assert_eq!(batch_row(2, 3, true, false, false), (3, 3, ""));
        assert_eq!(batch_row(1, 3, false, true, false), (1, 3, ""), "once a photo is done it counts, waiting or not");
        assert_eq!(batch_row(3, 3, true, false, false), (3, 3, ""), "never past the total");
        assert_eq!(batch_row(usize::MAX, 2, true, true, true), (2, 2, ""));
        assert_eq!(batch_row(0, 0, false, true, true), (0, 0, ""));
    }

    /// The Recompute AI Masks row follows the worker (OneSlips 9bb601286): while another request runs it waits and
    /// says for what; while one of its own runs it counts that photo; photos count once all their selections are done.
    #[cfg(feature = "sam")]
    #[test]
    fn the_recompute_row_follows_the_worker() {
        use std::sync::atomic::Ordering;
        let mut s = Session::with_demo();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = s.activity.start_batch("aiMasks", "Computing AI masks", crate::activity::Cancel::Flag(cancel.clone()));
        let photos: Vec<PhotoId> = (1..=3).map(PhotoId).collect();
        let per_photo = [(photos[0], 2), (photos[1], 1), (photos[2], 1)].into_iter().collect();
        s.segmenter.recompute = Some(RecomputeRun { id: 7, cancel, guard, photos: photos.clone(), left: 4, total: 4, per_photo });
        let row = |s: &Session| s.activity.list().into_iter().find(|t| t.kind == "aiMasks").map(|t| (t.done, t.total, t.detail));
        let shared = s.segmenter.worker.shared.clone();
        shared.running.store(worker::RUN_OTHER, Ordering::SeqCst);
        shared.running_low.store(true, Ordering::SeqCst);
        s.update_recompute_row();
        assert_eq!(row(&s), Some((0, 0, WAITING_FOR_PREP.to_string())));
        shared.running_low.store(false, Ordering::SeqCst);
        s.update_recompute_row();
        assert_eq!(row(&s), Some((0, 0, WAITING_FOR_MASKS.to_string())));
        shared.running.store(worker::run_code(7), Ordering::SeqCst);
        s.update_recompute_row();
        assert_eq!(row(&s), Some((1, 3, String::new())), "its first photo at work");
        // the first photo's two selections done: 1 photo done, the next at work
        if let Some(r) = &mut s.segmenter.recompute {
            r.per_photo.remove(&photos[0]);
        }
        s.update_recompute_row();
        assert_eq!(row(&s).map(|r| (r.0, r.1)), Some((2, 3)));
        shared.running.store(0, Ordering::SeqCst);
        s.update_recompute_row();
        assert_eq!(row(&s).map(|r| (r.0, r.1)), Some((1, 3)), "between jobs it shows the photos done");
    }

    #[cfg(feature = "sam")]
    #[test]
    fn a_worker_notice_reaches_the_user_with_the_result() {
        // the job after which the model runs on the CPU (the GPU failed) says so
        let mut s = Session::with_demo();
        let notice = Some(worker::GPU_FAILED.to_string());
        s.segmenter.results.0.send(worker::Outcome { tag: Tag::Prepare, result: Ok(None), superseded: false, notice, extra: None }).unwrap();
        let polled = s.segment_poll();
        assert_eq!(polled.messages, vec![worker::GPU_FAILED.to_string()]);
        assert!(s.segment_poll().messages.is_empty(), "once");
    }

    // A reply racing the session poll must not be mistaken for a lost click (upstream #4221).
    #[cfg(feature = "sam")]
    #[test]
    fn idle_before_click_reply_must_not_discard_the_outcome() {
        use std::{sync::mpsc, time::Duration};
        let (setup_tx, setup_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();
        let (first_tx, first_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let session = std::thread::spawn(move || {
            let mut s = Session::with_demo();
            let photo = s.active().unwrap();
            let latest = PendingClicks { photo, mask: 1, comp: 0, hint: vec![], exclude: vec![] };
            s.segmenter.pending = Some(latest.clone());
            let tag = Tag::Clicks { photo, mask: 1, comp: 0, hint: latest.hint, exclude: latest.exclude };
            setup_tx.send((s.segmenter.worker.shared.clone(), s.segmenter.results.0.clone(), tag)).unwrap();
            go_rx.recv().unwrap();
            let first = s.segment_poll();
            first_tx.send((first.messages.is_empty(), s.segmenter.pending.is_some())).unwrap();
            resume_rx.recv().unwrap();
            let second = s.segment_poll();
            (second.messages, s.segmenter.pending.is_none())
        });
        let (shared, replies, tag) = setup_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // hold the delivery point while polling: a blocking poll fails within the timeout, never hangs
        let delivery = shared.delivery.lock().unwrap();
        go_tx.send(()).unwrap();
        let first = first_rx.recv_timeout(Duration::from_secs(5));
        replies.send(worker::Outcome { tag, result: Err("reply arrived".into()), superseded: false, notice: None, extra: None }).unwrap();
        drop(delivery);
        resume_tx.send(()).unwrap();
        let (messages, cleared) = session.join().unwrap();
        assert_eq!(first.expect("poll blocked while a reply was being delivered"), (true, true));
        assert_eq!(messages, vec!["reply arrived"]);
        assert!(cleared);
    }

    #[cfg(feature = "sam")]
    #[test]
    fn an_idle_worker_with_no_reply_clears_lost_clicks() {
        let mut s = Session::with_demo();
        s.segmenter.pending = Some(PendingClicks { photo: s.active().unwrap(), mask: 1, comp: 0, hint: vec![], exclude: vec![] });
        assert!(!s.segmenter.busy());
        s.segment_poll();
        assert!(s.segmenter.pending.is_none());
    }

    #[cfg(feature = "sam")]
    #[test]
    fn a_superseded_reply_does_not_hide_the_latest_click_reply() {
        let mut s = Session::with_demo();
        let photo = s.active().unwrap();
        s.segmenter.pending = Some(PendingClicks { photo, mask: 1, comp: 0, hint: vec![], exclude: vec![] });
        for (superseded, message) in [(true, "old reply"), (false, "latest reply")] {
            let tag = Tag::Clicks { photo, mask: 1, comp: 0, hint: vec![], exclude: vec![] };
            s.segmenter.results.0.send(worker::Outcome { tag, result: Err(message.into()), superseded, notice: None, extra: None }).unwrap();
        }
        assert_eq!(s.segment_poll().messages, vec!["latest reply"]);
        assert!(s.segmenter.pending.is_none());
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
