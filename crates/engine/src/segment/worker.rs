//! The AI mask worker thread: it owns the models (SAM 3, SAM 2.1, BiRefNet, EoMT, each loaded
//! when first needed) and the last encoded photo, and runs every request (load, render the model
//! input, encode, clicks, descriptions, automatic selections, detail passes) in turn. The session only sends jobs and receives outcomes over channels, so no thread ever
//! waits on a lock the model holds, and the UI thread never runs the model.
//!
//! Every job runs under `catch_unwind`: a panic inside candle becomes an error outcome, drops
//! the (possibly inconsistent) model and leaves the worker ready for the next job. The busy
//! counters are decremented by drop guards, so they can't stay stuck. The model is unloaded
//! after [`IDLE_UNLOAD`] without requests (it holds several GB).
//!
//! Where the model can run on the GPU (Metal on macOS), a job that fails there (a panic or an
//! error from candle, e.g. a kernel the GPU's compiler can't build, issue #1974) frees the
//! model and runs once more on the CPU. When that works, the model stays on the CPU for the rest
//! of the session and the job's outcome says so; when it fails too, the GPU wasn't the cause and
//! the next job uses it again. `LIGHTCRAFT_GPU=0` or `LIGHTCRAFT_GPU_BACKEND=off` keep the model on
//! the CPU from the start.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use lightcraft_develop::SegMask;
use lightcraft_segment::{
    Encoded, SAM2_MASK_SIDE, Sam2, Sam2Encoded, Sam3,
    birefnet::{BIREFNET_SIDE, BiRefNet},
    people::{Face, FaceModel, PeopleMap, PeopleModel, PeopleRequest, people_mask},
    remote::{self, Operation, Request, text_logits},
    scene::{Scene, SceneModel},
};

use super::{AutoKind, AutoModel, Input, Prompt, Tag};
use crate::media::RenderJob;

/// Unload the model after this long without a request.
pub const IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);

/// What to compute.
#[derive(Clone)]
pub enum Kind {
    /// Load the model and encode the photo (so the first click is fast).
    Prepare,
    Clicks(Vec<lightcraft_segment::Click>),
    Text(String),
    /// A zoomed-in pass over `region` (normalized) of the photo, rendered by the job's `render`.
    Detail {
        prompt: Prompt,
        region: [f64; 4],
    },
    /// An automatic selection (Subject, Sky, …) computed by `model`.
    Auto {
        what: AutoKind,
        model: AutoModel,
    },
    /// Panics (tests of the worker's recovery).
    #[cfg(test)]
    Panic,
    /// Fails like the model on a GPU that can't run it (a panic or a candle error), and
    /// succeeds on the CPU (tests of the CPU fallback).
    #[cfg(test)]
    FailOnGpu {
        panic: bool,
    },
}

/// One request.
pub struct Job {
    /// The model folder.
    pub dir: PathBuf,
    /// Object clicks (and their detail passes) run on SAM 2.1 (`dir` is its folder), not SAM 3.
    pub sam2: bool,
    /// Which photo and look the request is about (the encode cache's key).
    pub key: u64,
    /// Renders the model input when the cache doesn't hold `key` (always for a detail pass).
    pub render: Option<RenderJob>,
    /// The photo's file, when it is missing on disk (for a clearer error if rendering fails).
    pub missing: Option<String>,
    pub kind: Kind,
    pub tag: Tag,
    pub reply: mpsc::Sender<Outcome>,
    /// Where the slow steps (loading a model, analyzing a photo) show as activity rows.
    pub activity: Option<crate::activity::Activity>,
}

/// A finished request: the segmentation (`None`: nothing selected / nothing to report).
pub struct Outcome {
    pub tag: Tag,
    pub result: Result<Option<SegMask>, String>,
    /// A newer request for the same selection came in; this one was skipped.
    pub superseded: bool,
    /// Something the user should know about the result (e.g. face features it couldn't find).
    pub note: Option<String>,
    /// Something the user should know besides the result ([`GPU_FAILED`]).
    pub notice: Option<String>,
}

/// The notice of the job after which the model runs on the CPU.
pub const GPU_FAILED: &str =
    "The GPU couldn't run the AI mask model, so it runs on the CPU until LightCraft restarts (slower). Details are in the log.";

/// Where the model is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// On the GPU when there is one (Metal on macOS); a job that fails there is retried on the CPU.
    Gpu,
    /// On the CPU: this build has no GPU path, the environment switches the GPU off, or the model
    /// failed on it.
    Cpu,
}

impl Placement {
    fn initial() -> Self {
        if !lightcraft_segment::GPU_BUILD {
            return Placement::Cpu;
        }
        if let Some(why) = lightcraft_gpu::env_off_reason() {
            log::info!("SAM 3 runs on the CPU ({why})");
            return Placement::Cpu;
        }
        Placement::Gpu
    }
}

/// Why a job failed.
#[derive(Debug)]
enum Failure {
    /// The model failed (a candle error, or a panic): on the GPU, the CPU may run it.
    Model(String),
    /// Anything else (no model files, the photo, a remote worker): another device won't help.
    Other(String),
}

impl Failure {
    fn message(self) -> String {
        match self {
            Failure::Model(m) | Failure::Other(m) => m,
        }
    }
}

impl From<String> for Failure {
    fn from(m: String) -> Self {
        Failure::Other(m)
    }
}

impl From<&str> for Failure {
    fn from(m: &str) -> Self {
        Failure::Other(m.into())
    }
}

impl From<lightcraft_segment::Error> for Failure {
    fn from(e: lightcraft_segment::Error) -> Self {
        if matches!(e, lightcraft_segment::Error::Candle(_)) { Failure::Model(e.to_string()) } else { Failure::Other(e.to_string()) }
    }
}

/// Counters the session reads without locking.
#[derive(Default)]
pub struct Shared {
    /// Requests other than detail passes, queued or running.
    pub pending: AtomicUsize,
    /// Detail passes queued or running.
    pub detail: AtomicUsize,
    /// The model is in memory.
    pub loaded: AtomicBool,
    /// Loading the model or encoding a photo right now (the slow parts).
    pub encoding: AtomicBool,
}

/// Decrements a counter when dropped (also when unwinding).
struct Done<'a>(&'a AtomicUsize);

impl Drop for Done<'_> {
    fn drop(&mut self) {
        let _ = self.0.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| Some(n.saturating_sub(1)));
    }
}

/// Sets a flag while alive.
struct Flag<'a>(&'a AtomicBool);

impl<'a> Flag<'a> {
    fn raise(f: &'a AtomicBool) -> Self {
        f.store(true, Ordering::SeqCst);
        Flag(f)
    }
}

impl Drop for Flag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The session's handle on the worker thread (started on the first request, restarted if it
/// ever ends).
#[derive(Default)]
pub struct Worker {
    tx: Option<mpsc::Sender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub shared: Arc<Shared>,
}

impl Worker {
    fn counter(&self, job: &Job) -> &AtomicUsize {
        if matches!(job.kind, Kind::Detail { .. }) { &self.shared.detail } else { &self.shared.pending }
    }

    /// Whether the thread has ended (it never should); its counters are then meaningless.
    fn dead(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| t.is_finished())
    }

    pub fn pending(&self) -> usize {
        if self.dead() { 0 } else { self.shared.pending.load(Ordering::SeqCst) }
    }

    pub fn detail(&self) -> usize {
        if self.dead() { 0 } else { self.shared.detail.load(Ordering::SeqCst) }
    }

    pub fn encoding(&self) -> bool {
        !self.dead() && self.shared.encoding.load(Ordering::SeqCst)
    }

    pub fn loaded(&self) -> bool {
        !self.dead() && self.shared.loaded.load(Ordering::SeqCst)
    }

    /// Queue `job`; returns at once.
    pub fn submit(&mut self, job: Job) -> Result<(), String> {
        if self.dead() {
            // a thread that ended took its counters' meaning with it
            self.tx = None;
            self.thread = None;
            self.shared = Arc::new(Shared::default());
        }
        self.counter(&job).fetch_add(1, Ordering::SeqCst);
        let job = match &self.tx {
            Some(tx) => match tx.send(job) {
                Ok(()) => return Ok(()),
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        // (re)start the thread
        let (tx, rx) = mpsc::channel();
        let shared = self.shared.clone();
        match std::thread::Builder::new().name("sam3".into()).spawn(move || run(&rx, &shared, IDLE_UNLOAD, Placement::initial())) {
            Ok(t) => {
                self.thread = Some(t);
                self.tx = Some(tx.clone());
                tx.send(job).map_err(|_| "the AI mask worker stopped".to_string())
            }
            Err(e) => {
                let _ = self.counter(&job).try_update(Ordering::SeqCst, Ordering::SeqCst, |n| Some(n.saturating_sub(1)));
                Err(format!("could not start the AI mask worker: {e}"))
            }
        }
    }
}

/// The models and what they last computed for a photo.
#[derive(Default)]
struct State {
    model: Option<(PathBuf, Sam3)>,
    cache: Option<(u64, Encoded)>,
    remote: Option<remote::Client>,
    sam2: Option<(PathBuf, Sam2)>,
    sam2_cache: Option<(u64, Sam2Encoded)>,
    subject: Option<(PathBuf, BiRefNet)>,
    /// BiRefNet's logits for a photo (Subject and Background share them).
    subject_cache: Option<(u64, Vec<f32>)>,
    scene: Option<(PathBuf, SceneModel)>,
    /// EoMT's scene for a photo (Sky, People, Landscape and Subject share it).
    scene_cache: Option<(u64, Scene)>,
    people: Option<(PathBuf, PeopleModel)>,
    /// Selfie Multiclass's classes for a photo.
    people_cache: Option<(u64, PeopleMap)>,
    face: Option<(PathBuf, FaceModel)>,
    /// The faces (with landmarks) of a photo.
    faces_cache: Option<(u64, Vec<Face>)>,
    /// The last rendered input of the openly licensed models (several of them may run on one photo).
    input_cache: Option<(u64, Input)>,
    /// A note about the job just run, for its outcome.
    note: Option<String>,
}

impl State {
    fn any_loaded(&self) -> bool {
        self.model.is_some()
            || self.remote.is_some()
            || self.sam2.is_some()
            || self.subject.is_some()
            || self.scene.is_some()
            || self.people.is_some()
            || self.face.is_some()
    }

    /// Forget what was computed for photos (a failed request may have left it half done).
    fn forget_photos(&mut self) {
        self.cache = None;
        self.sam2_cache = None;
        self.subject_cache = None;
        self.scene_cache = None;
        self.people_cache = None;
        self.faces_cache = None;
        self.input_cache = None;
    }
}

/// Whether `later` makes `job` pointless (it asks for the same selection again).
fn supersedes(later: &Job, job: &Job) -> bool {
    match (&later.tag, &job.tag) {
        (Tag::Prepare, Tag::Prepare) => later.key == job.key,
        (Tag::Clicks { photo: a, mask: am, comp: ac, .. }, Tag::Clicks { photo: b, mask: bm, comp: bc, .. }) => (a, am, ac) == (b, bm, bc),
        (Tag::Detail { photo: a, mask: am, comp: ac, .. }, Tag::Detail { photo: b, mask: bm, comp: bc, .. }) => (a, am, ac) == (b, bm, bc),
        _ => false,
    }
}

/// Large matrix products of the models on the GPU (`lightcraft_gpu::gemm`); declines (the CPU
/// does them) whenever the GPU is unavailable, switched off or fails.
struct GpuGemm;

impl lightcraft_segment::accel::Gemm for GpuGemm {
    fn gemm(&self, a: &[f32], rows: usize, k: usize, n: usize, key: u64, weights: &dyn Fn() -> Option<Vec<f32>>) -> Option<Vec<f32>> {
        lightcraft_gpu::gemm::matmul(a, rows, k, n, key, weights)
    }
}

/// Give the models the GPU for their large matrix products when `LIGHTCRAFT_AI_GPU=1`. Off by
/// default: measured on an RTX 5070 (DX12) it saves ~20 % of a warm BiRefNet pass but its first
/// pass costs ~20 s more (kernel compilation, uploading 885 MB of weights), and the rest of the
/// pass (window attention, deformable sampling) stays on the CPU.
fn install_gpu() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let on = std::env::var("LIGHTCRAFT_AI_GPU").is_ok_and(|v| matches!(v.trim(), "1" | "on" | "true" | "yes"));
        if on {
            lightcraft_segment::accel::set_gemm(Some(Arc::new(GpuGemm)));
        }
    });
}

fn run(rx: &mpsc::Receiver<Job>, shared: &Shared, idle: Duration, mut placement: Placement) {
    install_gpu();
    let mut state = State::default();
    let mut queue: VecDeque<Job> = VecDeque::new();
    loop {
        if queue.is_empty() {
            match rx.recv_timeout(idle) {
                Ok(j) => queue.push_back(j),
                Err(RecvTimeoutError::Timeout) => {
                    if state.any_loaded() {
                        state = State::default();
                        shared.loaded.store(false, Ordering::SeqCst);
                        log::info!("AI mask models unloaded after {} minutes without use", idle.as_secs() / 60);
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        while let Ok(j) = rx.try_recv() {
            queue.push_back(j);
        }
        let Some(mut job) = queue.pop_front() else { continue };
        let is_detail = matches!(job.kind, Kind::Detail { .. });
        // counts the job as pending until just before its reply goes out: a caller woken by the
        // reply must not still see the worker busy with it
        let done = Done(if is_detail { &shared.detail } else { &shared.pending });
        if queue.iter().any(|later| supersedes(later, &job)) {
            drop(done);
            let _ = job.reply.send(Outcome { tag: job.tag, result: Ok(None), superseded: true, note: None, notice: None });
            continue;
        }
        // what a retry on the CPU needs (running the job takes them)
        let retry = (placement == Placement::Gpu && remote::configured().is_none()).then(|| (job.kind.clone(), job.render.clone()));
        let mut result = attempt(&mut state, shared, &mut job, placement);
        let mut notice = None;
        let gpu_failed = match &result {
            Err(Failure::Model(why)) => Some(why.clone()),
            _ => None,
        };
        if let (Some(why), Some((kind, render))) = (gpu_failed, retry) {
            // the GPU may be the cause (e.g. a kernel its compiler can't build, issue #1974): free
            // the model there and try once on the CPU
            log::warn!("SAM 3 failed on the GPU ({why}); trying the CPU");
            state = State::default();
            (job.kind, job.render) = (kind, render);
            match attempt(&mut state, shared, &mut job, Placement::Cpu) {
                Ok(r) => {
                    log::warn!("SAM 3 runs on the CPU from now on: it failed on the GPU and works on the CPU");
                    placement = Placement::Cpu;
                    notice = Some(GPU_FAILED.to_string());
                    result = Ok(r);
                }
                Err(e) => {
                    // the CPU fails too, so the GPU isn't the cause: the next job uses it again
                    log::warn!("SAM 3 failed on the CPU too: {}", e.message());
                    state = State::default();
                }
            }
        }
        let result = result.map_err(Failure::message);
        if result.is_err() && !is_detail {
            state.forget_photos();
        }
        shared.loaded.store(state.any_loaded(), Ordering::SeqCst);
        drop(done);
        let note = state.note.take();
        if let Some(n) = &note {
            log::info!("AI masks: {n}");
        }
        let _ = job.reply.send(Outcome { tag: job.tag, result, superseded: false, note, notice });
    }
}

/// Run `job` with the model on `placement`. A panic is a [`Failure::Model`] and drops what the
/// model kept.
fn attempt(state: &mut State, shared: &Shared, job: &mut Job, placement: Placement) -> Result<Option<SegMask>, Failure> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| run_job(state, shared, job, placement))) {
        Ok(r) => r,
        Err(p) => {
            let why = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
            log::error!("AI mask model panicked: {why}");
            // don't trust what the model kept: load it again next time
            *state = State::default();
            Err(Failure::Model("the AI model failed unexpectedly (it will be reloaded on the next try)".into()))
        }
    }
}

/// The model for `dir`, loading it on `placement` when needed.
fn model<'a>(state: &'a mut State, shared: &Shared, dir: &std::path::Path, placement: Placement) -> Result<&'a mut Sam3, Failure> {
    if state.model.as_ref().is_none_or(|(d, _)| d != dir) {
        state.model = None;
        state.cache = None;
        let _busy = Flag::raise(&shared.encoding);
        let t = web_time::Instant::now();
        let m = match placement {
            Placement::Gpu => Sam3::load(dir)?,
            Placement::Cpu => Sam3::load_cpu(dir)?,
        };
        log::info!("SAM 3 loaded in {:?} ({})", t.elapsed(), if m.device().is_cpu() { "CPU" } else { "GPU" });
        state.model = Some((dir.to_path_buf(), m));
        shared.loaded.store(true, Ordering::SeqCst);
    }
    state.model.as_mut().map(|(_, m)| m).ok_or_else(|| "the model did not load".into())
}

/// Load the model in `slot` from `dir` when it isn't loaded from there (dropping the photos
/// computed with another copy).
fn load<'a, M>(
    slot: &'a mut Option<(PathBuf, M)>,
    shared: &Shared,
    dir: &std::path::Path,
    name: &str,
    open: impl FnOnce(&std::path::Path) -> lightcraft_segment::Result<M>,
) -> Result<&'a M, String> {
    if slot.as_ref().is_none_or(|(d, _)| d != dir) {
        *slot = None;
        let _busy = Flag::raise(&shared.encoding);
        let t = web_time::Instant::now();
        let m = open(dir).map_err(|e| format!("{name}: {e}"))?;
        log::info!("{name} loaded in {:?}", t.elapsed());
        *slot = Some((dir.to_path_buf(), m));
        shared.loaded.store(true, Ordering::SeqCst);
    }
    slot.as_ref().map(|(_, m)| m).ok_or_else(|| format!("{name} did not load"))
}

/// SAM 2.1 and the photo `job.key` encoded by it (rendering and encoding it when needed).
fn sam2_encoded<'a>(state: &'a mut State, shared: &Shared, job: &mut Job) -> Result<(&'a Sam2, &'a Sam2Encoded), String> {
    if state.sam2.as_ref().is_none_or(|(d, _)| *d != job.dir) {
        state.sam2_cache = None;
    }
    load(&mut state.sam2, shared, &job.dir, "SAM 2.1", Sam2::load)?;
    if state.sam2_cache.as_ref().is_none_or(|(k, _)| *k != job.key) {
        state.sam2_cache = None;
        let input = render(job)?;
        let _busy = Flag::raise(&shared.encoding);
        let _row = row(job, "SAM 2.1");
        let m = state.sam2.as_ref().map(|(_, m)| m).ok_or("SAM 2.1 did not load")?;
        let t = web_time::Instant::now();
        let enc = m.encode(&input.rgb, input.w, input.h).map_err(|e| e.to_string())?;
        log::info!("SAM 2.1 encoded {}×{} in {:?}", input.w, input.h, t.elapsed());
        state.sam2_cache = Some((job.key, enc));
    }
    match (&state.sam2, &state.sam2_cache) {
        (Some((_, m)), Some((_, e))) => Ok((m, e)),
        _ => Err("SAM 2.1 is not ready".into()),
    }
}

/// Clicks inside `region` (normalized) of the photo, in the region's own coordinates; `None` when
/// no including click is inside.
fn clicks_in(clicks: &[super::Click], region: [f64; 4]) -> Option<Vec<lightcraft_segment::Click>> {
    let (rw, rh) = (region[2] - region[0], region[3] - region[1]);
    if !(rw > 0.0 && rh > 0.0) {
        return None;
    }
    let inside: Vec<lightcraft_segment::Click> = clicks
        .iter()
        .map(|c| ((c.at.x - region[0]) / rw, (c.at.y - region[1]) / rh, c.include))
        .filter(|(x, y, _)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
        .map(|(x, y, positive)| lightcraft_segment::Click { x: x as f32, y: y as f32, positive })
        .collect();
    inside.iter().any(|c| c.positive).then_some(inside)
}

/// Side of the grid an EoMT selection is stored at (its output is 160 px on the long edge).
const SCENE_SIDE: usize = 256;

/// The COCO panoptic classes (by name) each landscape class covers.
fn landscape_classes(class: &str) -> &'static [&'static str] {
    match class {
        "sky" => &["sky-other-merged"],
        "water" => &["sea", "river", "water-other"],
        "vegetation" => &["tree-merged", "grass-merged", "flower", "potted plant"],
        "mountains" => &["mountain-merged", "rock-merged"],
        "architecture" => &[
            "building-other-merged",
            "house",
            "roof",
            "bridge",
            "tent",
            "wall-brick",
            "wall-stone",
            "wall-tile",
            "wall-wood",
            "wall-other-merged",
            "fence-merged",
            "door-stuff",
            "window-other",
            "window-blind",
            "stairs",
        ],
        "naturalGround" => &["sand", "dirt-merged", "gravel", "snow", "playingfield"],
        "artificialGround" => &["road", "pavement-merged", "railroad", "platform", "floor-wood", "floor-other-merged", "rug-merged"],
        _ => &[],
    }
}

/// An activity row for a slow step of `job` (analyzing a photo with `name`).
fn row(job: &Job, name: &str) -> Option<crate::activity::TaskGuard> {
    job.activity.as_ref().map(|a| {
        let t = a.start("ai", "Analyzing photo for AI masks", crate::activity::Cancel::No);
        t.detail(name);
        t
    })
}

/// The openly licensed models' input for `job`'s photo, rendered once per photo.
fn photo<'a>(cache: &'a mut Option<(u64, Input)>, job: &mut Job) -> Result<&'a Input, String> {
    if cache.as_ref().is_none_or(|(k, _)| *k != job.key || job.key == 0) {
        *cache = None;
        *cache = Some((job.key, render(job)?));
    }
    cache.as_ref().map(|(_, i)| i).ok_or_else(|| "the photo is not available".to_string())
}

/// Run `f` on the model in `slot` and the photo, unless `cache` holds its result for the photo.
fn cached<'a, M, T>(
    slot: &Option<(PathBuf, M)>,
    cache: &'a mut Option<(u64, T)>,
    input: &mut Option<(u64, Input)>,
    shared: &Shared,
    job: &mut Job,
    name: &str,
    f: impl FnOnce(&M, &Input) -> lightcraft_segment::Result<T>,
) -> Result<&'a T, String> {
    if cache.as_ref().is_none_or(|(k, _)| *k != job.key) {
        *cache = None;
        let m = slot.as_ref().map(|(_, m)| m).ok_or_else(|| format!("{name} did not load"))?;
        let img = photo(input, job)?;
        let _busy = Flag::raise(&shared.encoding);
        let _row = row(job, name);
        let t = web_time::Instant::now();
        let out = f(m, img).map_err(|e| format!("{name}: {e}"))?;
        log::info!("{name}: {}×{} in {:?}", img.w, img.h, t.elapsed());
        *cache = Some((job.key, out));
    }
    cache.as_ref().map(|(_, v)| v).ok_or_else(|| format!("{name} is not ready"))
}

/// Load the model in `slot` from `dir` (forgetting what it computed when it moves).
fn ensure<M, T>(
    slot: &mut Option<(PathBuf, M)>,
    cache: &mut Option<(u64, T)>,
    shared: &Shared,
    dir: &std::path::Path,
    name: &str,
    open: impl FnOnce(&std::path::Path) -> lightcraft_segment::Result<M>,
) -> Result<(), String> {
    if slot.as_ref().is_none_or(|(d, _)| d != dir) {
        *cache = None;
    }
    load(slot, shared, dir, name, open).map(|_| ())
}

/// An automatic selection; `None` when nothing of the kind was found.
fn auto(state: &mut State, shared: &Shared, job: &mut Job, what: &AutoKind, model: AutoModel) -> Result<Option<SegMask>, String> {
    let invert = |l: Vec<f32>| l.into_iter().map(|v| -v).collect::<Vec<f32>>();
    let background = *what == AutoKind::Background;
    match model {
        AutoModel::Subject(dir) => {
            ensure(&mut state.subject, &mut state.subject_cache, shared, &dir, "BiRefNet", BiRefNet::load)?;
            let logits = cached(&state.subject, &mut state.subject_cache, &mut state.input_cache, shared, job, "BiRefNet", |m, i| {
                m.segment(&i.rgb, i.w, i.h)
            })?
            .clone();
            let logits = if background { invert(logits) } else { logits };
            Ok(Some(SegMask::from_logits(BIREFNET_SIDE, &logits)))
        }
        AutoModel::Scene(dir) => {
            ensure(&mut state.scene, &mut state.scene_cache, shared, &dir, "EoMT", SceneModel::load)?;
            let scene =
                cached(&state.scene, &mut state.scene_cache, &mut state.input_cache, shared, job, "EoMT", |m, i| m.segment(&i.rgb, i.w, i.h))?;
            let Some(probs) = scene_probs(scene, what) else { return Ok(None) };
            let logits = lightcraft_segment::grid::to_square_logits(&probs, scene.width, scene.height, SCENE_SIDE);
            let logits = if background { invert(logits) } else { logits };
            Ok(Some(SegMask::from_logits(SCENE_SIDE, &logits)))
        }
        AutoModel::People { people, face, scene } => {
            let map = match &people {
                Some(dir) => {
                    ensure(&mut state.people, &mut state.people_cache, shared, dir, "MediaPipe people", PeopleModel::load)?;
                    Some(cached(&state.people, &mut state.people_cache, &mut state.input_cache, shared, job, "MediaPipe people", |m, i| {
                        m.segment(&i.rgb, i.w, i.h)
                    })?)
                }
                None => None,
            };
            if matches!(what, AutoKind::Subject | AutoKind::Background) {
                // a person is the subject when there is one; else EoMT's most confident thing
                let person_there = map.as_ref().is_some_and(|m| m.person_share() > 0.005);
                if !person_there && let Some(scene) = scene {
                    return auto(state, shared, job, what, AutoModel::Scene(scene));
                }
                let Some(map) = map else { return Ok(None) };
                let req = PeopleRequest::parse(0, &[]).ok_or("people request")?;
                let input = photo(&mut state.input_cache, job)?;
                return Ok(people_mask(&req, &input.rgb, input.w, input.h, Some(map), &[]).map(|g| {
                    let logits = if background { invert(g.logits) } else { g.logits };
                    SegMask::from_logits(g.side, &logits)
                }));
            }
            let AutoKind::People { person, parts } = what else { return Err("not a People selection".into()) };
            let req = PeopleRequest::parse(*person, parts).ok_or("unknown People part")?;
            let faces: Vec<Face> = match &face {
                Some(dir) => {
                    ensure(&mut state.face, &mut state.faces_cache, shared, dir, "MediaPipe faces", FaceModel::load)?;
                    cached(&state.face, &mut state.faces_cache, &mut state.input_cache, shared, job, "MediaPipe faces", |m, i| {
                        m.faces(&i.rgb, i.w, i.h)
                    })?
                    .clone()
                }
                None => Vec::new(),
            };
            let map = match &people {
                Some(_) => state.people_cache.as_ref().filter(|(k, _)| *k == job.key).map(|(_, m)| m),
                None => None,
            };
            // (the input cache is shared by every model: another photo's mask may have replaced it
            // while the people and faces caches still hold this one, so render it again if so)
            let input = photo(&mut state.input_cache, job)?;
            let skin = req.classes.iter().any(|c| matches!(c, 2 | 3));
            let Some(g) = people_mask(&req, &input.rgb, input.w, input.h, map, &faces) else { return Ok(None) };
            if skin && face.is_none() {
                state.note = Some("Skin includes eyes, brows and mouth: the MediaPipe faces model, which finds them, isn't installed.".into());
            } else if skin && g.faces_without_features > 0 {
                state.note = Some(format!(
                    "Face skin: eyes and mouth couldn't be located on {} face(s) (seen in profile), so there only hair is left out.",
                    g.faces_without_features
                ));
            }
            Ok(Some(match g.rect {
                Some(r) => SegMask::from_logits_in(g.side, &g.logits, r),
                None => SegMask::from_logits(g.side, &g.logits),
            }))
        }
    }
}

/// The probability of `what` at each cell of `scene`'s grid; `None` when it isn't there.
fn scene_probs(scene: &Scene, what: &AutoKind) -> Option<Vec<f32>> {
    use lightcraft_segment::scene::class_ids;
    let person = class_ids(|n| n == "person");
    let found = |p: &[f32]| p.iter().any(|v| *v > 0.5);
    match what {
        AutoKind::Sky => Some(scene.class_probability(&class_ids(|n| n.starts_with("sky")))),
        AutoKind::Landscape { class } => {
            let names = landscape_classes(class);
            Some(scene.class_probability(&class_ids(|n| names.contains(&n))))
        }
        AutoKind::People { person: 0, .. } => Some(scene.class_probability(&person)).filter(|p| found(p)),
        AutoKind::People { person: n, .. } => {
            // people counted from the left (by the centre of their mask)
            let mut people: Vec<(f32, &Vec<f32>)> = scene
                .instances
                .iter()
                .filter(|i| person.contains(&i.class))
                .map(|i| {
                    let (mut sx, mut sw) = (0.0f32, 0.0f32);
                    for (k, v) in i.mask.iter().enumerate() {
                        sx += v * (k % scene.width.max(1)) as f32;
                        sw += v;
                    }
                    (if sw > 0.0 { sx / sw } else { 0.0 }, &i.mask)
                })
                .collect();
            people.sort_by(|a, b| a.0.total_cmp(&b.0));
            people.get((*n as usize).saturating_sub(1)).map(|(_, m)| (*m).clone())
        }
        // a subject without BiRefNet: the people, else the most confident thing
        AutoKind::Subject | AutoKind::Background => {
            let p = scene.class_probability(&person);
            if found(&p) {
                return Some(p);
            }
            let things = class_ids(|n| !n.contains('-') && !matches!(n, "sand" | "sea" | "river" | "road" | "snow" | "gravel" | "house" | "roof"));
            scene.instances.iter().filter(|i| things.contains(&i.class)).max_by(|a, b| a.score.total_cmp(&b.score)).map(|i| i.mask.clone())
        }
    }
}

/// Render the model input.
fn render(job: &mut Job) -> Result<Input, String> {
    let r = job.render.take().ok_or("the photo is not available")?.run();
    let img = match r.rendered {
        Ok(r) => r.image,
        Err(e) => {
            return Err(match &job.missing {
                Some(path) => format!(
                    "This photo's file is missing (moved or deleted), so it can't be analyzed for AI masks: {path}. Library ▸ Find Missing Photos can relink it."
                ),
                None => format!("couldn't render the photo for AI masks: {e}"),
            });
        }
    };
    let rgb = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    Ok(Input { rgb, w: img.width, h: img.height })
}

/// The model and the encoded photo `job.key` (rendering and encoding it when needed).
fn encoded<'a>(state: &'a mut State, shared: &Shared, job: &mut Job, placement: Placement) -> Result<(&'a mut Sam3, &'a mut Encoded), Failure> {
    model(state, shared, &job.dir, placement)?;
    if state.cache.as_ref().is_none_or(|(k, _)| *k != job.key) {
        state.cache = None;
        let input = render(job)?;
        let _busy = Flag::raise(&shared.encoding);
        let _row = row(job, "SAM 3");
        let m = state.model.as_mut().map(|(_, m)| m).ok_or("the model did not load")?;
        let t = web_time::Instant::now();
        let mut enc = m.encode(&input.rgb, input.w, input.h)?;
        m.prepare_clicks(&mut enc)?;
        log::info!("SAM 3 encoded {}×{} in {:?}", input.w, input.h, t.elapsed());
        state.cache = Some((job.key, enc));
    }
    let State { model, cache, .. } = state;
    match (model.as_mut(), cache.as_mut()) {
        (Some((_, m)), Some((_, e))) => Ok((m, e)),
        _ => Err("the model is not ready".into()),
    }
}

fn remote_job(state: &mut State, shared: &Shared, job: &mut Job, endpoint: &str) -> Result<Option<SegMask>, String> {
    let _busy = Flag::raise(&shared.encoding);
    if state.remote.is_none() {
        state.remote = Some(remote::Client::connect(endpoint)?);
    }
    let client = state.remote.as_mut().ok_or("remote SAM unavailable")?;
    let mut region = None;
    let operation = match std::mem::replace(&mut job.kind, Kind::Prepare) {
        Kind::Prepare => Operation::Prepare,
        Kind::Text(text) => Operation::Text(text),
        Kind::Clicks(clicks) => Operation::Clicks(clicks.iter().map(|c| [c.x, c.y, if c.positive { 1.0 } else { 0.0 }]).collect()),
        Kind::Detail { prompt, region: r } => {
            region = Some(r);
            let (rw, rh) = (r[2] - r[0], r[3] - r[1]);
            if !(rw > 0.0 && rh > 0.0) {
                return Ok(None);
            }
            match prompt {
                Prompt::Text(text) => Operation::Text(text),
                Prompt::Clicks(clicks) => {
                    let inside: Vec<_> = clicks
                        .iter()
                        .map(|c| [((c.at.x - r[0]) / rw) as f32, ((c.at.y - r[1]) / rh) as f32, if c.include { 1.0 } else { 0.0 }])
                        .filter(|p| (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1]))
                        .collect();
                    if !inside.iter().any(|p| p[2] == 1.0) {
                        return Ok(None);
                    }
                    Operation::Clicks(inside)
                }
            }
        }
        Kind::Auto { .. } => return Err("automatic selections run locally".into()),
        #[cfg(test)]
        Kind::Panic | Kind::FailOnGpu { .. } => return Err("test panic request".into()),
    };
    let input = if region.is_some() || client.key != Some(job.key) { render(job)? } else { Input { rgb: Vec::new(), w: 0, h: 0 } };
    let request = Request { width: input.w, height: input.h, temporary: region.is_some(), operation };
    let logits = client.request(&request, &input.rgb)?;
    if region.is_none() {
        client.key = Some(job.key);
    }
    Ok(logits.map(|l| match region {
        Some(r) => SegMask::from_logits_in(lightcraft_segment::MASK_SIDE, &l, r),
        None => SegMask::from_logits(lightcraft_segment::MASK_SIDE, &l),
    }))
}

fn run_job(state: &mut State, shared: &Shared, job: &mut Job, placement: Placement) -> Result<Option<SegMask>, Failure> {
    if let Kind::Auto { what, model } = &job.kind {
        let (what, model) = (what.clone(), model.clone());
        return Ok(auto(state, shared, job, &what, model)?);
    }
    if job.sam2 {
        return Ok(sam2_job(state, shared, job)?);
    }
    if let Some(endpoint) = remote::configured() {
        let result = remote_job(state, shared, job, &endpoint);
        if result.is_err() {
            state.remote = None;
        }
        return result.map_err(|e| Failure::Other(format!("Remote SAM 3 failed: {e}. Check the worker and SSH tunnel, then retry.")));
    }
    let side = lightcraft_segment::MASK_SIDE;
    match std::mem::replace(&mut job.kind, Kind::Prepare) {
        Kind::Prepare => {
            encoded(state, shared, job, placement)?;
            Ok(None)
        }
        Kind::Clicks(clicks) => {
            let (m, enc) = encoded(state, shared, job, placement)?;
            let p = m.segment_clicks(enc, &clicks)?;
            Ok(Some(SegMask::from_logits(side, &p.logits)))
        }
        Kind::Text(text) => {
            let (m, enc) = encoded(state, shared, job, placement)?;
            Ok(text_logits(m, enc, &text)?.map(|l| SegMask::from_logits(side, &l)))
        }
        Kind::Auto { .. } => Err("an automatic selection reached the SAM 3 path".into()),
        #[cfg(test)]
        #[allow(clippy::panic)]
        Kind::Panic => panic!("a test panic"),
        #[cfg(test)]
        #[allow(clippy::panic)]
        Kind::FailOnGpu { panic } => match placement {
            Placement::Gpu if panic => panic!("a test panic on the GPU"),
            Placement::Gpu => Err(Failure::Model("a test error on the GPU".into())),
            Placement::Cpu => Ok(None),
        },
        Kind::Detail { prompt, region } => {
            let input = render(job)?;
            let m = model(state, shared, &job.dir, placement)?;
            let t = web_time::Instant::now();
            let mut enc = m.encode(&input.rgb, input.w, input.h)?;
            let (rw, rh) = (region[2] - region[0], region[3] - region[1]);
            if !(rw > 0.0 && rh > 0.0) {
                return Ok(None);
            }
            let logits = match prompt {
                Prompt::Clicks(clicks) => {
                    let Some(inside) = clicks_in(&clicks, region) else { return Ok(None) };
                    Some(m.segment_clicks(&mut enc, &inside)?.logits)
                }
                Prompt::Text(text) => text_logits(m, &mut enc, &text)?,
            };
            log::info!("SAM 3 detail pass ({}×{}) in {:?}", input.w, input.h, t.elapsed());
            Ok(logits.map(|l| SegMask::from_logits_in(side, &l, region)))
        }
    }
}

/// Object clicks, their preparation and detail passes on SAM 2.1.
fn sam2_job(state: &mut State, shared: &Shared, job: &mut Job) -> Result<Option<SegMask>, String> {
    let side = SAM2_MASK_SIDE;
    match std::mem::replace(&mut job.kind, Kind::Prepare) {
        Kind::Prepare => {
            sam2_encoded(state, shared, job)?;
            Ok(None)
        }
        Kind::Clicks(clicks) => {
            let (m, enc) = sam2_encoded(state, shared, job)?;
            let p = m.segment_clicks(enc, &clicks).map_err(|e| e.to_string())?;
            Ok(Some(SegMask::from_logits(side, &p.logits)))
        }
        Kind::Detail { prompt: Prompt::Clicks(clicks), region } => {
            let Some(inside) = clicks_in(&clicks, region) else { return Ok(None) };
            let input = render(job)?;
            let m = load(&mut state.sam2, shared, &job.dir, "SAM 2.1", Sam2::load)?;
            let t = web_time::Instant::now();
            let enc = m.encode(&input.rgb, input.w, input.h).map_err(|e| e.to_string())?;
            let p = m.segment_clicks(&enc, &inside).map_err(|e| e.to_string())?;
            log::info!("SAM 2.1 detail pass ({}×{}) in {:?}", input.w, input.h, t.elapsed());
            Ok(Some(SegMask::from_logits_in(side, &p.logits, region)))
        }
        Kind::Text(_) | Kind::Detail { prompt: Prompt::Text(_), .. } => Err("Describe needs the SAM 3 model".into()),
        Kind::Auto { .. } => Err("an automatic selection reached the SAM 2.1 path".into()),
        #[cfg(test)]
        #[allow(clippy::panic)]
        Kind::Panic => panic!("a test panic"),
        #[cfg(test)]
        Kind::FailOnGpu { .. } => Err("a GPU fallback test reached the SAM 2.1 path".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(tag: Tag, reply: &mpsc::Sender<Outcome>) -> Job {
        // an empty folder: loading fails with an error (no model files)
        let dir = std::env::temp_dir().join(format!("lc-sam3-worker-none-{}", std::process::id()));
        Job { dir, sam2: false, key: 1, render: None, missing: None, kind: Kind::Prepare, tag, reply: reply.clone(), activity: None }
    }

    #[test]
    fn errors_come_back_and_the_counters_return_to_zero() {
        let mut w = Worker::default();
        let (tx, rx) = mpsc::channel();
        for _ in 0..3 {
            w.submit(job(Tag::Wait, &tx)).unwrap();
        }
        for _ in 0..3 {
            let o = rx.recv_timeout(Duration::from_secs(30)).unwrap();
            assert!(o.result.unwrap_err().contains("not found"));
        }
        let t = web_time::Instant::now();
        while w.pending() > 0 && t.elapsed() < Duration::from_secs(5) {
            std::thread::yield_now();
        }
        assert_eq!(w.pending(), 0);
        assert!(!w.encoding() && !w.loaded());
    }

    #[test]
    fn a_panic_in_a_job_is_an_error_and_the_worker_carries_on() {
        let shared = Shared::default();
        let (tx, rx) = mpsc::channel();
        let (jtx, jrx) = mpsc::channel();
        let mut j = job(Tag::Wait, &tx);
        j.kind = Kind::Panic;
        jtx.send(j).unwrap();
        jtx.send(job(Tag::Wait, &tx)).unwrap();
        shared.pending.store(2, Ordering::SeqCst);
        drop(jtx);
        run(&jrx, &shared, Duration::from_secs(60), Placement::Cpu);
        let first = rx.recv().unwrap();
        assert!(first.result.unwrap_err().contains("failed unexpectedly"));
        let second = rx.recv().unwrap();
        assert!(second.result.is_err(), "the next job still runs");
        assert_eq!(shared.pending.load(Ordering::SeqCst), 0, "counters reset");
        assert!(!shared.encoding.load(Ordering::SeqCst));
    }

    /// Run `kinds` in turn on a worker that starts on `placement`; their outcomes.
    fn run_kinds(placement: Placement, kinds: Vec<Kind>) -> Vec<Outcome> {
        let (tx, rx) = mpsc::channel();
        let (jtx, jrx) = mpsc::channel();
        let shared = Shared::default();
        shared.pending.store(kinds.len(), Ordering::SeqCst);
        for kind in kinds {
            let mut j = job(Tag::Wait, &tx);
            j.kind = kind;
            jtx.send(j).unwrap();
        }
        drop(jtx);
        run(&jrx, &shared, Duration::from_secs(60), placement);
        assert_eq!(shared.pending.load(Ordering::SeqCst), 0, "counters reset");
        assert!(!shared.encoding.load(Ordering::SeqCst));
        rx.try_iter().collect()
    }

    #[test]
    fn a_job_that_panics_on_the_gpu_runs_on_the_cpu_and_so_do_the_next_ones() {
        // issue #1974: candle panics compiling a Metal kernel the GPU's compiler can't build
        let out = run_kinds(Placement::Gpu, vec![Kind::FailOnGpu { panic: true }, Kind::FailOnGpu { panic: true }]);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0].result, Ok(None)), "retried on the CPU: {:?}", out[0].result.as_ref().err());
        assert_eq!(out[0].notice.as_deref(), Some(GPU_FAILED));
        assert!(matches!(out[1].result, Ok(None)), "the next job starts on the CPU");
        assert_eq!(out[1].notice, None, "said once");
    }

    #[test]
    fn a_model_error_on_the_gpu_is_retried_on_the_cpu_too() {
        let out = run_kinds(Placement::Gpu, vec![Kind::FailOnGpu { panic: false }]);
        assert!(matches!(out[0].result, Ok(None)));
        assert_eq!(out[0].notice.as_deref(), Some(GPU_FAILED));
    }

    #[test]
    fn a_job_that_fails_on_the_cpu_too_keeps_the_gpu() {
        let out = run_kinds(Placement::Gpu, vec![Kind::Panic, Kind::FailOnGpu { panic: true }]);
        assert!(out[0].result.as_ref().unwrap_err().contains("failed unexpectedly"));
        assert_eq!(out[0].notice, None);
        // not the GPU's fault: the next job tries the GPU first again
        assert!(matches!(out[1].result, Ok(None)));
        assert_eq!(out[1].notice.as_deref(), Some(GPU_FAILED));
    }

    #[test]
    fn a_worker_on_the_cpu_never_tries_the_gpu() {
        // no GPU in this build, or LIGHTCRAFT_GPU=0 / LIGHTCRAFT_GPU_BACKEND=off
        let out = run_kinds(Placement::Cpu, vec![Kind::FailOnGpu { panic: true }]);
        assert!(matches!(out[0].result, Ok(None)));
        assert_eq!(out[0].notice, None);
        if !lightcraft_segment::GPU_BUILD {
            assert_eq!(Placement::initial(), Placement::Cpu);
        }
    }

    #[test]
    fn missing_model_files_are_not_retried_on_the_cpu() {
        let out = run_kinds(Placement::Gpu, vec![Kind::Prepare]);
        assert!(out[0].result.as_ref().unwrap_err().contains("not found"));
        assert_eq!(out[0].notice, None);
        let missing = lightcraft_segment::Error::Missing(PathBuf::from("models"));
        assert!(matches!(Failure::from(missing), Failure::Other(_)));
    }

    #[test]
    fn a_newer_request_for_the_same_selection_supersedes_a_queued_one() {
        let (tx, rx) = mpsc::channel();
        let (jtx, jrx) = mpsc::channel();
        let clicks = |n: usize| Tag::Clicks {
            photo: lightcraft_catalog::PhotoId(1),
            mask: 1,
            comp: 0,
            hint: vec![lightcraft_geom::Point::new(0.5, 0.5); n],
            exclude: vec![],
        };
        for n in 1..=3 {
            jtx.send(job(clicks(n), &tx)).unwrap();
        }
        drop(jtx);
        let shared = Shared::default();
        shared.pending.store(3, Ordering::SeqCst);
        run(&jrx, &shared, Duration::from_secs(60), Placement::Cpu);
        let out: Vec<Outcome> = rx.try_iter().collect();
        assert_eq!(out.len(), 3);
        assert!(out[0].superseded && out[1].superseded && !out[2].superseded);
        assert_eq!(shared.pending.load(Ordering::SeqCst), 0);
    }
}
