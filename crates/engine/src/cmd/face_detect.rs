//! `faces.detect`: find faces with the open MediaPipe face model (Face Landmarker: BlazeFace and FaceMesh-V2,
//! Apache-2.0, `ai.models.download {"id": "face"}`), the one Retouch uses. It replaced YuNet, whose weights were
//! trained on WIDER FACE (CC BY-NC-ND); a YuNet installed by an older version is left where it is and not used.
//!
//! The photo is rendered upright and uncropped with default settings (so edits, crops and rotations you have
//! made do not matter), the detector runs on it, and the faces come back as fractions of the photo's width
//! and height, the same frame face regions from XMP use. Unless `apply` is false, they are added to the photo as
//! unnamed face regions in one undoable step; earlier detections are replaced, and regions that came from XMP (or
//! that you drew or named) are never touched. Like every region edit it is catalog-only: no sidecar is written.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use lcc_catalog::Op;
use lcc_develop::DevelopSettings;
use lcc_faces::yunet::Face;
use lcc_meta::{Region, RegionKind};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_selection};
use crate::{AiModel, EngineError, Result, Session};

const C: &str = "faces.detect";
/// Regions made by this command say so in their description; only those are replaced on a new run.
pub(crate) const MARK: &str = "Detected by ";
pub(crate) const LABEL: &str = "MediaPipe Face Landmarker";
/// Long edge of the picture the detector looks at.
const LOOK_EDGE: usize = 1280;
/// How much a detection must overlap a region the photo already has to count as the same face.
const EXISTING_IOU: f64 = 0.3;
/// The default score a face must reach (BlazeFace's own floor is 0.65).
const DEFAULT_SCORE: f32 = 0.65;
/// Most faces one photo gives.
const DEFAULT_MAX_FACES: usize = 200;

/// Why [`detector`] has no detector to give.
#[derive(Debug)]
pub(crate) enum NoDetector {
    /// Not downloaded yet: a frontend offers the download ([`crate::ErrorKind::ModelMissing`]).
    #[cfg_attr(not(feature = "sam"), allow(dead_code))]
    NotInstalled(String),
    /// Not in this build, or there but unusable: says what to do.
    Unusable(String),
}

impl NoDetector {
    /// Command `cmd`'s error for it (an invalid-parameters message, as before kinds existed).
    pub(crate) fn into_error(self, cmd: &str) -> EngineError {
        match self {
            Self::NotInstalled(why) => EngineError::ModelMissing { model: AiModel::FaceDetector, msg: bad(cmd, why).to_string() },
            Self::Unusable(why) => bad(cmd, why),
        }
    }
}

/// The open face model, loaded.
pub(crate) struct Finder {
    #[cfg(feature = "sam")]
    model: Arc<lcc_segment::people::FaceModel>,
}

impl Finder {
    /// The faces in an 8-bit RGB picture (`w × h`), left to right: boxes and five landmarks as fractions of the
    /// picture (the eyes, the nose tip and the mouth corners, from FaceMesh where it placed its landmarks). `fine`
    /// also looks in overlapping tiles of half the picture, which finds smaller faces (group photos) at about four
    /// times the cost.
    pub(crate) fn detect(&self, rgb: &[u8], w: usize, h: usize, fine: bool) -> std::result::Result<Vec<Face>, String> {
        #[cfg(feature = "sam")]
        {
            use lcc_tflite::mediapipe;
            if w == 0 || h == 0 || rgb.len() < w.saturating_mul(h).saturating_mul(3) {
                return Ok(Vec::new());
            }
            let mut dets = self.model.detect(rgb, w, h).map_err(|e| e.to_string())?;
            if fine && w.min(h) >= 512 {
                // half-size tiles, overlapping by half: each is searched like a whole photo
                let (tw, th) = (w / 2, h / 2);
                let tiles = [0, w / 4, w - tw].into_iter().flat_map(|x| [0, h / 4, h - th].into_iter().map(move |y| (x, y)));
                for (x0, y0) in tiles {
                    let mut tile = Vec::with_capacity(tw * th * 3);
                    for y in y0..y0 + th {
                        tile.extend_from_slice(rgb.get((y * w + x0) * 3..(y * w + x0 + tw) * 3).unwrap_or(&[]));
                    }
                    if tile.len() != tw * th * 3 {
                        continue;
                    }
                    for d in self.model.detect(&tile, tw, th).map_err(|e| e.to_string())? {
                        dets.push(d.from_window(x0 as f32, y0 as f32, 1.0));
                    }
                }
                dets = mediapipe::merge_windows(dets, 0.3);
                dets.sort_by(|a, b| a.center()[0].total_cmp(&b.center()[0]));
            }
            let mut out = Vec::new();
            for d in dets {
                let landmarks = self.model.landmarks(rgb, w, h, &d).map_err(|e| e.to_string())?;
                // FaceMesh not seeing a face there, and the detector only fairly sure: a pattern, a paw, a circuit
                // board (the detector alone takes those for faces, mostly in the tiles of `fine`)
                if landmarks.is_none() && d.score < lcc_segment::people::FACE_THRESHOLD_NO_MESH {
                    continue;
                }
                let found = crate::segment::FoundFace::from_pixels(&lcc_segment::people::Face { detection: d, landmarks }, w as f32, h as f32);
                let [x0, y0, x1, y1] = found.rect;
                out.push(Face { x0, y0, x1, y1, score: found.score, landmarks: found.five() });
            }
            Ok(out)
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (rgb, w, h, fine);
            Err(NOT_AVAILABLE.into())
        }
    }
}

#[cfg(not(feature = "sam"))]
const NOT_AVAILABLE: &str = "finding faces needs the open AI models, which this build does not include";

/// The installed face model, loaded once and kept (a new one is loaded when its folder changes).
/// (also used by the background scan, which aligns faces by the landmarks and finds faces in photos that have none)
pub(crate) fn detector(s: &Session) -> std::result::Result<Arc<Finder>, NoDetector> {
    #[cfg(feature = "sam")]
    {
        let dir = match s.segmenter.require_open_model("face", AiModel::FaceDetector) {
            Ok(d) => d,
            Err(EngineError::ModelMissing { msg, .. }) => return Err(NoDetector::NotInstalled(msg)),
            Err(e) => return Err(NoDetector::Unusable(e.to_string())),
        };
        let model = crate::segment::face_model(&dir).map_err(NoDetector::Unusable)?;
        #[cfg(not(target_arch = "wasm32"))]
        offer_detection_gpu(s, &model);
        Ok(Arc::new(Finder { model }))
    }
    #[cfg(not(feature = "sam"))]
    {
        let _ = s;
        Err(NoDetector::Unusable(NOT_AVAILABLE.into()))
    }
}

/// The face model last offered to the GPU (its address), so each loaded model is checked once.
#[cfg(all(feature = "sam", not(target_arch = "wasm32")))]
static DETECTION_OFFERED: Mutex<usize> = Mutex::new(0);

/// Offer a newly loaded face model to the GPU ([`offer_gpu`]: in the background, checked against the CPU, kept only
/// when faster), or move it off a card that stopped working ([`move_off_lost_card`]).
#[cfg(all(feature = "sam", not(target_arch = "wasm32")))]
fn offer_detection_gpu(s: &Session, model: &Arc<lcc_segment::people::FaceModel>) {
    let attach = |m: Arc<lcc_segment::people::FaceModel>| {
        move |gpu: Arc<dyn lcc_faces::net::Accelerator>| -> std::result::Result<(), String> {
            let what = m.accelerate(gpu, true)?;
            log::info!("faces: {what} run on the GPU");
            Ok(())
        }
    };
    let at = Arc::as_ptr(model) as usize;
    let fresh = {
        let mut offered = DETECTION_OFFERED.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::replace(&mut *offered, at) != at
    };
    if fresh {
        offer_gpu(s, "detection", attach(model.clone()));
    } else {
        move_off_lost_card(s, "detection", model.accelerated(), attach(model.clone()));
    }
}

/// Keep face detection on the CPU (`cpu`), or let the next run offer it to the GPU again.
fn set_detection_cpu_only(s: &Session, cpu: bool) {
    #[cfg(all(feature = "sam", not(target_arch = "wasm32")))]
    {
        *DETECTION_OFFERED.lock().unwrap_or_else(PoisonError::into_inner) = 0;
        if cpu
            && s.segmenter.open_model_installed("face")
            && let Ok(dir) = s.segmenter.require_open_model("face", AiModel::FaceDetector)
            && let Ok(model) = crate::segment::face_model(&dir)
        {
            model.detach();
        }
    }
    let _ = s;
    let st = if cpu { GpuState { why: Some("the CPU is chosen in Settings".into()), ..GpuState::default() } } else { GpuState::default() };
    set_gpu_state("detection", st);
}

/// Where a face model runs, for Settings ▸ Faces and `faces.gpu`.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GpuState {
    /// The background check is still running.
    pub checking: bool,
    /// The GPU it runs on; `None` when it runs on the CPU.
    pub device: Option<String>,
    /// Which card that is (`second`, the second graphics card, or `editing`); `None` on the CPU.
    pub card: Option<&'static str>,
    /// Why it runs on the CPU.
    pub why: Option<String>,
}

/// Each model's [`GpuState`], by what it does ("detection", "recognition").
static GPU_STATE: Mutex<Vec<(&'static str, GpuState)>> = Mutex::new(Vec::new());

pub(crate) fn gpu_state(what: &str) -> Option<GpuState> {
    GPU_STATE.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(w, _)| *w == what).map(|(_, st)| st.clone())
}

fn set_gpu_state(what: &'static str, st: GpuState) {
    let mut all = GPU_STATE.lock().unwrap_or_else(PoisonError::into_inner);
    match all.iter_mut().find(|(w, _)| *w == what) {
        Some((_, old)) => *old = st,
        None => all.push((what, st)),
    }
}

/// The user chose the CPU for face models (Settings ▸ Faces).
pub(crate) fn cpu_only(s: &Session) -> bool {
    s.face_models_dir.as_deref().is_some_and(|d| super::face_models::read_settings(d).cpu_only)
}

/// Check in the background whether the GPU runs a face model faster than the CPU and gives its results; the model
/// runs on the CPU meanwhile, and stays there when the GPU is off, missing, wrong or slower, or the user chose the CPU.
/// The check shows in the activity stack while it runs.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn offer_gpu(
    s: &Session,
    what: &'static str,
    attach: impl FnOnce(Arc<dyn lcc_faces::net::Accelerator>) -> std::result::Result<(), String> + Send + 'static,
) {
    if cpu_only(s) {
        set_gpu_state(what, GpuState { why: Some("the CPU is chosen in Settings".into()), ..GpuState::default() });
        return;
    }
    let task = s.activity.start("facesGpu", "Checking the GPU for faces", crate::activity::Cancel::No);
    task.detail(what);
    set_gpu_state(what, GpuState { checking: true, ..GpuState::default() });
    let spawned = std::thread::Builder::new().name(format!("faces-gpu-{what}")).spawn(move || {
        let _task = task;
        let gpu = match lcc_gpu::faces::accelerator() {
            Ok(gpu) => gpu,
            Err(why) => {
                log::info!("faces: {what} runs on the CPU ({why})");
                return set_gpu_state(what, GpuState { why: Some(why), ..GpuState::default() });
            }
        };
        let device = lcc_faces::net::Accelerator::device(&*gpu);
        let card = gpu.card().id();
        match attach(gpu) {
            Ok(()) => {
                log::info!("faces: {what} runs on {device} (the {card} card)");
                set_gpu_state(what, GpuState { device: Some(device), card: Some(card), ..GpuState::default() });
            }
            Err(why) => {
                log::info!("faces: {what} stays on the CPU: {why}");
                set_gpu_state(what, GpuState { why: Some(why), ..GpuState::default() });
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("faces: could not check the GPU for {what}: {e}");
        set_gpu_state(what, GpuState { why: Some(format!("could not check the GPU: {e}")), ..GpuState::default() });
    }
}

/// The card a model that was on the GPU (`st`) has to leave: its net dropped the accelerator (`attached` false) because
/// that card stopped working. A model the GPU merely failed once (out of memory, a wrong answer) stays on the CPU, as
/// before there were two cards.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn lost_card(st: &GpuState, attached: bool) -> Option<lcc_gpu::faces::Card> {
    if st.checking || attached || st.device.is_none() {
        return None;
    }
    let card = lcc_gpu::ai::CARDS.into_iter().find(|c| st.card == Some(c.id()))?;
    lcc_gpu::faces::broken_reason_on(card).is_some().then_some(card)
}

/// When the card model `what` ran on stopped working ([`lost_card`]), offer the model to the GPU again: it goes to the
/// other card when that works ([`lcc_gpu::faces::accelerator`], checked against the CPU there as at first), else it
/// stays on the CPU and says why. Cheap to call every frame; each card is given up on once, so this ends.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn move_off_lost_card(
    s: &Session,
    what: &'static str,
    attached: bool,
    attach: impl FnOnce(Arc<dyn lcc_faces::net::Accelerator>) -> std::result::Result<(), String> + Send + 'static,
) -> bool {
    let Some(card) = gpu_state(what).and_then(|st| lost_card(&st, attached)) else { return false };
    log::info!("faces: the {} card stopped working; {what} tries the other card", card.id());
    offer_gpu(s, what, attach);
    true
}

/// `faces.gpu`: where the face models run, and the switch that keeps them on the CPU.
fn gpu(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.gpu";
    if let Some(cpu) = p.get("cpuOnly") {
        let cpu = cpu.as_bool().ok_or_else(|| bad(C, "`cpuOnly` is true or false"))?;
        let dir = s.face_models_dir.clone().ok_or_else(|| bad(C, "this build has nowhere to keep face models (the desktop app does)"))?;
        super::face_models::set_cpu_only(&dir, cpu)?;
        #[cfg(not(target_arch = "wasm32"))]
        let embedder = s.faces.embedder.as_ref().map(|(_, e)| e.clone());
        #[cfg(target_arch = "wasm32")]
        let embedder: Option<Arc<lcc_faces::runtime::Embedder>> = None;
        set_detection_cpu_only(s, cpu);
        if cpu {
            if let Some(e) = &embedder {
                e.net().detach();
            }
            set_gpu_state("recognition", GpuState { why: Some("the CPU is chosen in Settings".into()), ..GpuState::default() });
        } else {
            #[cfg(not(target_arch = "wasm32"))]
            {
                lcc_gpu::faces::reset();
                if let Some(e) = embedder {
                    offer_gpu(s, "recognition", move |gpu| e.accelerate(gpu, true).map_err(|e| e.to_string()));
                } else {
                    set_gpu_state("recognition", GpuState::default());
                }
            }
        }
    }
    Ok(gpu_json(s))
}

/// Where face detection runs, for Settings ▸ Faces and `faces.gpu`.
pub(crate) fn detection_gpu_state(s: &Session) -> GpuState {
    if !s.segmenter.open_model_installed("face") {
        return GpuState { why: Some("Face detector model not installed".into()), ..GpuState::default() };
    }
    if cpu_only(s) {
        return GpuState { why: Some("the CPU is chosen in Settings".into()), ..GpuState::default() };
    }
    if let Some(st) = gpu_state("detection")
        && (st.device.is_some() || st.checking || st.why.is_some())
    {
        return st;
    }
    // Load the installed detector now, as recognition is, so it is offered to the GPU (in the background) before
    // the first scan instead of the status saying it is not loaded (#978). The model is cached once loaded.
    #[cfg(all(feature = "sam", not(target_arch = "wasm32")))]
    if detector(s).is_ok()
        && let Some(st) = gpu_state("detection")
        && (st.device.is_some() || st.checking || st.why.is_some())
    {
        return st;
    }
    GpuState {
        why: Some("Face Landmarker runs multithreaded on the CPU until faces are first looked for and the GPU is checked".into()),
        ..GpuState::default()
    }
}

/// Where face recognition runs, for Settings ▸ Faces and `faces.gpu`.
pub(crate) fn recognition_gpu_state(s: &Session) -> GpuState {
    if let Some(st) = gpu_state("recognition")
        && (st.device.is_some() || st.checking || st.why.is_some())
    {
        return st;
    }
    if cpu_only(s) {
        return GpuState { why: Some("the CPU is chosen in Settings".into()), ..GpuState::default() };
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((_, e)) = &s.faces.embedder
        && let Some(a) = e.net().accelerator()
    {
        return GpuState { device: Some(a.device()), ..GpuState::default() };
    }
    let dir = s.face_models_dir.as_deref();
    let has_embedder = dir.is_some_and(|d| super::face_models::read_settings(d).embedder.is_some());
    if !has_embedder {
        return GpuState { why: Some("No recognition model chosen".into()), ..GpuState::default() };
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        match lcc_gpu::faces::accelerator() {
            Ok(gpu) => GpuState { device: Some(lcc_faces::net::Accelerator::device(&*gpu)), card: Some(gpu.card().id()), ..GpuState::default() },
            Err(why) => GpuState { why: Some(why), ..GpuState::default() },
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        GpuState { why: Some("GPU acceleration is not supported on WebAssembly".into()), ..GpuState::default() }
    }
}

/// What `faces.gpu` and `faces.models.list` say about the GPU.
pub(crate) fn gpu_json(s: &Session) -> Value {
    json!({
        "cpuOnly": cpu_only(s),
        "detection": detection_gpu_state(s),
        "recognition": recognition_gpu_state(s),
    })
}

fn face_json(f: &Face) -> Value {
    json!({
        "rect": {"x0": f.x0, "y0": f.y0, "x1": f.x1, "y1": f.y1},
        "score": f.score,
        "landmarks": f.landmarks.iter().map(|(x, y)| json!([x, y])).collect::<Vec<_>>(),
    })
}

/// A detected face that lies on a region the photo already has (from XMP, drawn or named) is that region, not a new face.
fn overlaps_existing(f: &Face, regions: &[Region]) -> bool {
    regions.iter().any(|r| {
        let (w, h) = (
            (r.rect.x1.min(f64::from(f.x1)) - r.rect.x0.max(f64::from(f.x0))).max(0.0),
            (r.rect.y1.min(f64::from(f.y1)) - r.rect.y0.max(f64::from(f.y0))).max(0.0),
        );
        let inter = w * h;
        let union = (r.rect.x1 - r.rect.x0) * (r.rect.y1 - r.rect.y0) + f64::from((f.x1 - f.x0) * (f.y1 - f.y0)) - inter;
        union > 0.0 && inter / union >= EXISTING_IOU
    })
}

/// A region Detect Faces may replace: one it found earlier and the user has not ignored (an ignored face is kept,
/// and its box stops the same face being found again).
pub(crate) fn is_detected(r: &Region) -> bool {
    r.kind != RegionKind::IgnoredFace && r.description.as_deref().is_some_and(|d| d.starts_with(MARK))
}

/// What `faces.detect` was asked for.
#[derive(Clone, Copy)]
struct Options {
    apply: bool,
    score: f32,
    max_faces: usize,
    fine: bool,
}

fn options(p: &Value) -> Options {
    Options {
        apply: p.get("apply").and_then(Value::as_bool).unwrap_or(true),
        score: p.get("score").and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(DEFAULT_SCORE, |v| v as f32),
        max_faces: p.get("maxFaces").and_then(Value::as_u64).map_or(DEFAULT_MAX_FACES, |v| usize::try_from(v).unwrap_or(DEFAULT_MAX_FACES)),
        fine: p.get("fine").and_then(Value::as_bool).unwrap_or(true),
    }
}

/// The photo `job` renders, as 8-bit RGB with its size.
fn rgb_of(job: crate::media::RenderJob) -> std::result::Result<(Vec<u8>, usize, usize), String> {
    let image = job.run().rendered?.image;
    let rgb: Vec<u8> = image.data.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
    Ok((rgb, image.width, image.height))
}

/// The faces in one photo that pass `o`.
fn find(detector: &Finder, job: crate::media::RenderJob, o: Options) -> std::result::Result<Vec<Face>, String> {
    let (rgb, width, height) = rgb_of(job)?;
    let mut f = detector.detect(&rgb, width, height, o.fine)?;
    f.retain(|f| f.score >= o.score);
    f.truncate(o.max_faces);
    Ok(f)
}

/// The job that renders `id` as the faces see it: upright, uncropped, default settings, its long edge at most [`LOOK_EDGE`].
fn look_job(s: &mut Session, id: lcc_catalog::PhotoId) -> Option<crate::media::RenderJob> {
    s.preview_job(id, LOOK_EDGE, LOOK_EDGE, false, &DevelopSettings::default())
}

/// Add the faces found to their photos (earlier detections replaced) in one undo step, and say what was done.
fn finish(s: &mut Session, o: Options, found: Vec<(lcc_catalog::PhotoId, std::result::Result<Vec<Face>, String>)>, ms: f64) -> Result<Value> {
    let (mut results, mut ops) = (Vec::new(), Vec::new());
    for (id, r) in found {
        let faces = match r {
            Ok(f) => f,
            Err(e) => {
                results.push(json!({"id": id.0, "error": e}));
                continue;
            }
        };
        if o.apply
            && let Some(photo) = s.catalog.photo(id)
        {
            let mut meta = photo.meta.clone();
            meta.regions.retain(|r| !is_detected(r));
            let fresh: Vec<&Face> = faces.iter().filter(|f| !overlaps_existing(f, &meta.regions)).collect();
            for f in fresh {
                meta.regions.push(Region {
                    rect: lcc_geom::Rect { x0: f64::from(f.x0), y0: f64::from(f.y0), x1: f64::from(f.x1), y1: f64::from(f.y1) },
                    kind: RegionKind::Face,
                    name: None,
                    description: Some(format!("{MARK}{LABEL}")),
                });
            }
            ops.push(Op::SetMeta { id, meta: Box::new(meta) });
        }
        results.push(json!({"id": id.0, "faces": faces.iter().map(face_json).collect::<Vec<_>>()}));
    }
    let applied = !ops.is_empty();
    if applied {
        let t = Instant::now();
        let n = ops.len();
        s.commit("Detect Faces", Op::Batch { ops })?;
        log::info!("faces: detected faces added to {n} photos in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
        // regions are not written to XMP, so a sidecar must not be rewritten for this
        s.skip_auto_write = true;
    }
    Ok(json!({"detector": LABEL, "applied": applied, "photos": results, "ms": ms}))
}

/// A background Detect Faces run: its results arrive on `rx`; `cancel` stops it between photos.
pub(crate) struct DetectRun {
    rx: Mutex<std::sync::mpsc::Receiver<Done>>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    options: Options,
    total: usize,
    started: Instant,
}

enum Done {
    Photo(lcc_catalog::PhotoId, std::result::Result<Vec<Face>, String>),
    End { cancelled: bool },
}

/// `faces.detect`. Without `wait` it runs in the background (a row in the activity stack, cancellable) and the faces
/// land in one undo step when it is done (`faces.detectPump`, called every frame by the app).
fn detect(s: &mut Session, p: &Value) -> Result<Value> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let o = options(p);
    let detector = detector(s).map_err(|e| e.into_error(C))?;
    let t0 = Instant::now();
    let ids = s.targets(p)?;
    if p.get("wait").and_then(Value::as_bool).unwrap_or(false) {
        let started = Instant::now();
        let found = ids
            .into_iter()
            .map(|id| {
                let r = look_job(s, id).ok_or_else(|| "no such photo".to_string()).and_then(|job| find(&detector, job, o));
                (id, r)
            })
            .collect();
        return finish(s, o, found, started.elapsed().as_secs_f64() * 1000.0);
    }
    if s.face_detect.as_ref().is_some_and(|r| !r.cancel.load(Ordering::Relaxed)) {
        return Err(bad(C, "a run is in progress already"));
    }
    let jobs: Vec<(lcc_catalog::PhotoId, Option<crate::media::RenderJob>)> = ids.into_iter().map(|id| (id, look_job(s, id))).collect();
    let total = jobs.len();
    let cancel = Arc::new(AtomicBool::new(false));
    let task = s.activity.start("faces", "Detecting faces", crate::activity::Cancel::Flag(cancel.clone()));
    task.progress(0, total as u64);
    let (tx, rx) = std::sync::mpsc::channel();
    let flag = cancel.clone();
    std::thread::Builder::new()
        .name("faces-detect".into())
        .spawn(move || {
            let mut done = 0u64;
            for (id, job) in jobs {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let r = match job {
                    None => Err("no such photo".to_string()),
                    Some(job) => crate::guard::catch("finding faces", || find(&detector, job, o)).and_then(|r| r),
                };
                done += 1;
                task.progress(done, total as u64);
                let _ = tx.send(Done::Photo(id, r));
            }
            let _ = tx.send(Done::End { cancelled: flag.load(Ordering::Relaxed) });
            drop(task);
        })
        .map_err(|e| bad(C, format!("could not start ({e})")))?;
    log::info!("faces: {total} photos queued in {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
    s.face_detect = Some(Arc::new(DetectRun { rx: Mutex::new(rx), cancel, options: o, total, started: Instant::now() }));
    s.face_detect_results.clear();
    Ok(json!({"done": false, "photos": total}))
}

/// `faces.detectPump`: cheap every frame. Takes in finished photos; when the run is over, adds the faces (one undo
/// step) and returns the summary once (`done: true`).
fn pump(s: &mut Session, _: &Value) -> Result<Value> {
    let Some(run) = s.face_detect.clone() else { return Ok(json!({"active": false})) };
    let mut end = None;
    {
        let rx = run.rx.lock().unwrap_or_else(PoisonError::into_inner);
        for d in rx.try_iter() {
            match d {
                Done::Photo(id, r) => s.face_detect_results.push((id, r)),
                Done::End { cancelled } => end = Some(cancelled),
            }
        }
    }
    let Some(cancelled) = end else {
        return Ok(json!({"active": true, "done": false, "looked": s.face_detect_results.len(), "photos": run.total}));
    };
    s.face_detect = None;
    let found = std::mem::take(&mut s.face_detect_results);
    log::info!("faces: detection of {} photos ran {:.1} s", found.len(), run.started.elapsed().as_secs_f64());
    let t = Instant::now();
    let mut out = finish(s, run.options, found, run.started.elapsed().as_secs_f64() * 1000.0)?;
    log::info!("faces: results taken in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    out["active"] = json!(false);
    out["done"] = json!(true);
    out["cancelled"] = json!(cancelled);
    Ok(out)
}

/// `faces.clearAndRescan`: forget what the scan learned about these photos' faces and detect them again. Faces you
/// named, drew or ignored are kept; the ones an earlier detection made are replaced.
fn clear_and_rescan(s: &mut Session, p: &Value) -> Result<Value> {
    let ids: Vec<lcc_catalog::PhotoId> = if p.get("ids").is_some() || s.selection.ids.len() > 1 { s.targets(p)? } else { s.visible_cloned() };
    let mut params = p.as_object().cloned().unwrap_or_default();
    params.insert("ids".into(), json!(ids.iter().map(|i| i.0).collect::<Vec<_>>()));
    let t = Instant::now();
    let started = detect(s, &Value::Object(params))?;
    log::info!("faces: rescan started in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let t = Instant::now();
        for id in ids {
            s.faces.content_replaced(id);
        }
        log::info!("faces: scan state cleared in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
        s.faces.epoch = s.faces.epoch.wrapping_add(1);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = ids;
    Ok(started)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "faces.detect",
            "Detect Faces",
            ["Library"],
            None,
            "{ids?, wait?: bool (false: runs in the background, an activity row, and `faces.detectPump` takes the results), apply?: bool (default true), score?: 0.65, maxFaces?: 200, fine?: bool (true: also looks in tiles, for the small faces of group photos)} → {photos: [{id, faces: [{rect, score, landmarks}]}]}. Finds faces with the open MediaPipe face model (Apache-2.0; download it first: `ai.models.download {id: \"face\", acknowledged: true}`) and, unless `apply` is false, adds them as unnamed face regions (one undo step), replacing earlier detections but never regions from XMP or ones you named; the sidecar is not touched",
            has_selection,
            detect
        ),
        cmd!(
            "faces.detectPump",
            "Detect Faces: Take Results",
            [],
            None,
            "{}: cheap every frame: when a Detect Faces run has finished, add its faces (one undo step) and return its summary once → {active, done, photos, …}",
            always,
            pump
        ),
        cmd!(
            "faces.clearAndRescan",
            "Clear Faces and Rescan",
            ["Library"],
            None,
            "{ids?}: the selected photos (else all in view): forget what the scan learned about their faces and detect them again in the background (faces you named, drew or ignored are kept) → {done: false, photos}",
            always,
            clear_and_rescan
        ),
        cmd!(
            query "faces.gpu",
            "Face Models on the GPU",
            [],
            None,
            "{cpuOnly?: bool} → {cpuOnly, detection, recognition}: where the face models run, each as {checking, device, why} (null until it is loaded). The GPU is used only after a background check shows it gives the CPU's results faster; `cpuOnly: true` keeps every face model on the CPU (Settings ▸ Faces), `false` checks the GPU again",
            always,
            gpu
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x0: f32, y0: f32, x1: f32, y1: f32) -> Face {
        Face { x0, y0, x1, y1, score: 0.9, landmarks: [(0.0, 0.0); 5] }
    }
    fn region(x0: f64, y0: f64, x1: f64, y1: f64) -> Region {
        Region { rect: lcc_geom::Rect { x0, y0, x1, y1 }, kind: RegionKind::Face, name: Some("Ann".into()), description: None }
    }

    #[test]
    fn a_detection_on_an_existing_region_is_not_a_new_face() {
        let existing = [region(0.40, 0.20, 0.60, 0.50), region(0.10, 0.10, 0.20, 0.20)];
        // the same face, a bit larger or shifted (a Lightroom box is not the detector's box)
        assert!(overlaps_existing(&face(0.38, 0.18, 0.64, 0.54), &existing));
        assert!(overlaps_existing(&face(0.42, 0.22, 0.58, 0.48), &existing));
        // another face, a sliver of overlap, and no regions at all
        assert!(!overlaps_existing(&face(0.70, 0.20, 0.90, 0.50), &existing));
        assert!(!overlaps_existing(&face(0.59, 0.49, 0.80, 0.80), &existing));
        assert!(!overlaps_existing(&face(0.0, 0.0, 0.0, 0.0), &existing), "an empty box overlaps nothing");
        assert!(!overlaps_existing(&face(0.4, 0.2, 0.6, 0.5), &[]));
    }

    /// A model whose card stopped working leaves it (for the other card, else the CPU); one the GPU failed once, still
    /// attached or still being checked stays where it is.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_model_leaves_a_card_that_stopped_working() {
        use lcc_gpu::faces::Card;
        // (only the second card is given up on: the other tests here may run on the card for editing)
        lcc_gpu::faces::give_up_on(Card::Second, "device lost (test)");
        let on = |card: &'static str| GpuState { device: Some("a card".into()), card: Some(card), ..GpuState::default() };
        assert_eq!(lost_card(&on("second"), false), Some(Card::Second), "dropped by a card that broke");
        assert_eq!(lost_card(&on("second"), true), None, "still attached");
        assert_eq!(lost_card(&GpuState { checking: true, ..on("second") }, false), None, "being checked");
        assert_eq!(lost_card(&on("editing"), false), None, "dropped by a card that still works (out of memory, say)");
        assert_eq!(lost_card(&GpuState::default(), false), None, "on the CPU");
        assert_eq!(lost_card(&GpuState { why: Some("slower".into()), ..GpuState::default() }, false), None);
    }
}
