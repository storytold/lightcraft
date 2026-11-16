//! The SAM 3 worker thread: it owns the model and the last encoded photo, and runs every
//! request (load, render the model input, encode, clicks, descriptions, detail passes) in
//! turn. The session only sends jobs and receives outcomes over channels, so no thread ever
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
//! the next job uses it again. `<PREFIX>_GPU=0` or `<PREFIX>_GPU_BACKEND=off` keep the model on
//! the CPU from the start.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::Duration;

use lcc_develop::SegMask;
use lcc_segment::{
    Encoded, Owl, OwlEncoded, Sam2, Sam2Encoded, Sam3,
    remote::{self, Operation, Request, text_logits},
};

use super::{Input, Prompt, Tag};
use crate::media::RenderJob;

/// Unload the model after this long without a request.
pub const IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);

/// What to compute.
#[derive(Clone)]
pub enum Kind {
    /// Load the model and encode the photo (so the first click is fast).
    Prepare,
    Clicks(Vec<lcc_segment::Click>),
    Text(String),
    /// A zoomed-in pass over `region` (normalized) of the photo, rendered by the job's `render`.
    Detail {
        prompt: Prompt,
        region: [f64; 4],
    },
    /// Learn an example of a remembered object from what `mask` selects on the photo
    /// (`concepts.teach`; SAM 2.1). The outcome's `extra` is the example.
    Learn {
        mask: Box<lcc_develop::Mask>,
    },
    /// Find a remembered object taught by `examples` on the photo (SAM 2.1). The outcome's
    /// `extra` is `{seeds: [[x, y], …], score}`, the clicks the selection grew from.
    Find {
        examples: Vec<lcc_segment::concept::Example>,
    },
    /// The photo's faces with their appearance signatures (MediaPipe faces in `face`, its people
    /// model beside it when installed, SAM 2.1's features from `sam2` when given). The outcome's
    /// `extra` is `[{rect: [x0, y0, x1, y1] normalized, signature}]`, left to right.
    Faces {
        face: PathBuf,
        sam2: Option<PathBuf>,
    },
    /// A People selection (`person` 0: everyone, n: the n-th from the left; `parts` Lightroom's
    /// names, none: the entire person) from MediaPipe's people model (Selfie Multiclass, beside
    /// `face`) and its faces model in `face`. The photo's analysis is kept for the next part.
    People {
        face: PathBuf,
        person: u32,
        parts: Vec<String>,
        /// SAM 2.1, when installed: one person of several follows their own silhouette where
        /// people overlap (an arm round someone stays its owner's).
        sam2: Option<PathBuf>,
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
    /// Describe runs on the OWLv2 object finder (this folder) and SAM 2.1 (`dir`), not SAM 3.
    pub detector: Option<PathBuf>,
    /// Which photo and look the request is about (the encode cache's key).
    pub key: u64,
    /// Renders the model input when the cache doesn't hold `key` (always for a detail pass).
    pub render: Option<RenderJob>,
    /// The photo's file, when it is missing on disk (for a clearer error if rendering fails).
    pub missing: Option<String>,
    pub kind: Kind,
    pub tag: Tag,
    pub reply: mpsc::Sender<Outcome>,
    /// Set when the job's batch was cancelled (Recompute AI Masks): skipped, not run.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Background work (finishing an import): the worker takes every other request first, between one of these and
    /// the next (OneSlips 9bb601286, test notes 2026-10-25 note 6: an AI mask asked for on the photos in front of the
    /// user runs first).
    pub low: bool,
}

/// A finished request: the segmentation (`None`: nothing selected / nothing to report).
pub struct Outcome {
    pub tag: Tag,
    pub result: Result<Option<SegMask>, String>,
    /// A newer request for the same selection came in; this one was skipped.
    pub superseded: bool,
    /// Something the user should know besides the result ([`GPU_FAILED`]).
    pub notice: Option<String>,
    /// What a job gives besides a selection (a learned example, the clicks a found object grew
    /// from).
    pub extra: Option<serde_json::Value>,
}

/// The notice of the job after which the model runs on the CPU.
pub const GPU_FAILED: &str = "The GPU couldn't run the AI mask model, so it runs on the CPU until the app restarts (slower). Details are in the log.";

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
        if !lcc_segment::GPU_BUILD {
            return Placement::Cpu;
        }
        if let Some(why) = lcc_gpu::env_off_reason() {
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

impl From<lcc_segment::Error> for Failure {
    fn from(e: lcc_segment::Error) -> Self {
        if matches!(e, lcc_segment::Error::Candle(_)) { Failure::Model(e.to_string()) } else { Failure::Other(e.to_string()) }
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
    /// Requests running or queued as background work ([`Job::low`]).
    pub low_pending: AtomicUsize,
    /// The request running now ([`running_code`]: 0 none, a Recompute AI Masks batch's code, or [`RUN_OTHER`]) and
    /// whether it is background work: what a batch waiting behind it says (OneSlips 9bb601286).
    pub running: std::sync::atomic::AtomicU64,
    pub running_low: AtomicBool,
    /// Makes decrementing the busy counter and sending the reply indivisible to the session poll
    /// (adapted from upstream #4221).
    pub(super) delivery: Mutex<()>,
}

/// [`Shared::running`] for a request that isn't part of a Recompute AI Masks batch.
pub const RUN_OTHER: u64 = u64::MAX;

/// [`Shared::running`] for a request tagged `tag`: its Recompute AI Masks batch's code, or [`RUN_OTHER`].
pub fn running_code(tag: &Tag) -> u64 {
    match tag {
        Tag::Recompute { run, .. } => run_code(*run),
        _ => RUN_OTHER,
    }
}

/// [`Shared::running`] while a job of Recompute AI Masks batch `run` runs (never 0 or [`RUN_OTHER`]).
pub fn run_code(run: u64) -> u64 {
    run.saturating_add(1).min(RUN_OTHER - 1)
}

/// Says which request runs while alive ([`Shared::running`]).
struct Running<'a>(&'a Shared);

impl<'a> Running<'a> {
    fn start(shared: &'a Shared, job: &Job) -> Self {
        shared.running.store(running_code(&job.tag), Ordering::SeqCst);
        shared.running_low.store(job.low, Ordering::SeqCst);
        Running(shared)
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.running.store(0, Ordering::SeqCst);
        self.0.running_low.store(false, Ordering::SeqCst);
    }
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

    /// Observe idleness and collect replies without racing a reply being delivered or blocking the UI:
    /// `None` while the worker is delivering one (the next poll takes it).
    pub(super) fn completed(&self, replies: &mpsc::Receiver<Outcome>) -> Option<(bool, Vec<Outcome>)> {
        let _delivery = match self.shared.delivery.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        Some((self.pending() == 0, replies.try_iter().collect()))
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
        if job.low {
            self.shared.low_pending.fetch_add(1, Ordering::SeqCst);
        }
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
                if job.low {
                    let _ = self.shared.low_pending.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| Some(n.saturating_sub(1)));
                }
                Err(format!("could not start the AI mask worker: {e}"))
            }
        }
    }
}

/// The model and the last encoded photo.
#[derive(Default)]
struct State {
    model: Option<(PathBuf, Sam3)>,
    cache: Option<(u64, Encoded)>,
    /// SAM 2.1, when the user chose it for Object clicks.
    sam2: Option<(PathBuf, Sam2)>,
    sam2_cache: Option<(u64, Sam2Encoded)>,
    /// The model input `sam2_cache` was encoded from (remembered objects read its colours).
    sam2_input: Option<Input>,
    /// The OWLv2 object finder, for Describe without SAM 3 (with SAM 2.1).
    owl: Option<(PathBuf, Owl)>,
    /// OWLv2's boxes and class embeddings for a photo (each description of it reuses them).
    owl_cache: Option<(u64, OwlEncoded)>,
    remote: Option<remote::Client>,
    /// The running job's [`Outcome::extra`].
    extra: Option<serde_json::Value>,
    /// MediaPipe's faces and people models (their folder), for appearance signatures.
    faces: Option<(PathBuf, lcc_segment::people::FaceModel, Option<lcc_segment::people::PeopleModel>)>,
    /// The last photo's people analysis (its look key): every part of it reuses it.
    people_seen: Option<(u64, PeopleSeen)>,
}

/// One photo's people analysis: the model input, the class map and the faces (left to right).
struct PeopleSeen {
    input: Input,
    map: Option<lcc_segment::people::PeopleMap>,
    faces: Vec<lcc_segment::people::Face>,
    /// Everyone's SAM 2.1 silhouette, once asked for (`Some(None)`: SAM 2.1 couldn't give them).
    instances: Option<Option<lcc_segment::people::Instances>>,
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

fn run(rx: &mpsc::Receiver<Job>, shared: &Shared, idle: Duration, mut placement: Placement) {
    let mut state = State::default();
    let mut queue: VecDeque<Job> = VecDeque::new();
    loop {
        if queue.is_empty() {
            match rx.recv_timeout(idle) {
                Ok(j) => queue.push_back(j),
                Err(RecvTimeoutError::Timeout) => {
                    if state.model.is_some() || state.sam2.is_some() || state.remote.is_some() {
                        state = State::default();
                        shared.loaded.store(false, Ordering::SeqCst);
                        log::info!("SAM 3 unloaded after {} minutes without use", idle.as_secs() / 60);
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        while let Ok(j) = rx.try_recv() {
            queue.push_back(j);
        }
        // the user's own requests before background work, each in the order asked
        let at = queue.iter().position(|j| !j.low).unwrap_or(0);
        let Some(mut job) = queue.remove(at) else { continue };
        let _low = job.low.then(|| Done(&shared.low_pending));
        let is_detail = matches!(job.kind, Kind::Detail { .. });
        // counts the job as pending until just before its reply goes out: a caller woken by the
        // reply must not still see the worker busy with it
        let done = Done(if is_detail { &shared.detail } else { &shared.pending });
        let cancelled = job.cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst));
        if cancelled || queue.iter().any(|later| supersedes(later, &job)) {
            let _delivery = shared.delivery.lock().unwrap_or_else(PoisonError::into_inner);
            drop(done);
            let _ = job.reply.send(Outcome { tag: job.tag, result: Ok(None), superseded: true, notice: None, extra: None });
            continue;
        }
        let running = Running::start(shared, &job);
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
        let extra = state.extra.take();
        if result.is_err() && !is_detail {
            state.cache = None;
            state.sam2_cache = None;
            state.owl_cache = None;
        }
        shared.loaded.store(state.model.is_some() || state.sam2.is_some() || state.owl.is_some() || state.remote.is_some(), Ordering::SeqCst);
        let _delivery = shared.delivery.lock().unwrap_or_else(PoisonError::into_inner);
        drop(done);
        drop(running);
        let _ = job.reply.send(Outcome { tag: job.tag, result, superseded: false, notice, extra });
    }
}

/// Run `job` with the model on `placement`. A panic is a [`Failure::Model`] and drops what the
/// model kept.
fn attempt(state: &mut State, shared: &Shared, job: &mut Job, placement: Placement) -> Result<Option<SegMask>, Failure> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| run_job(state, shared, job, placement))) {
        Ok(r) => r,
        Err(p) => {
            let why = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
            log::error!("SAM 3 panicked: {why}");
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
        // one model at a time: SAM 2.1 (and the OWLv2 finder Describe pairs with it) is freed
        (state.sam2, state.sam2_cache) = (None, None);
        (state.owl, state.owl_cache) = (None, None);
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

/// SAM 2.1 for `dir`, loading it on `placement` when needed (SAM 3 is freed: one model at a time).
fn sam2_model<'a>(state: &'a mut State, shared: &Shared, dir: &std::path::Path, placement: Placement) -> Result<&'a mut Sam2, Failure> {
    if state.sam2.as_ref().is_none_or(|(d, _)| d != dir) {
        (state.model, state.cache, state.sam2, state.sam2_cache) = (None, None, None, None);
        let _busy = Flag::raise(&shared.encoding);
        let t = web_time::Instant::now();
        let m = match placement {
            Placement::Gpu => Sam2::load(dir)?,
            Placement::Cpu => Sam2::load_cpu(dir)?,
        };
        log::info!("SAM 2.1 loaded in {:?}", t.elapsed());
        state.sam2 = Some((dir.to_path_buf(), m));
        shared.loaded.store(true, Ordering::SeqCst);
    }
    state.sam2.as_mut().map(|(_, m)| m).ok_or_else(|| "the model did not load".into())
}

/// The OWLv2 object finder for `dir`, loading it when needed (it runs on the CPU).
fn owl_model(state: &mut State, shared: &Shared, dir: &std::path::Path) -> Result<(), Failure> {
    if state.owl.as_ref().is_none_or(|(d, _)| d != dir) {
        (state.owl, state.owl_cache) = (None, None);
        let _busy = Flag::raise(&shared.encoding);
        let t = web_time::Instant::now();
        let m = Owl::load(dir)?;
        log::info!("OWLv2 loaded in {:?}", t.elapsed());
        state.owl = Some((dir.to_path_buf(), m));
        shared.loaded.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// The folder of the OWLv2 finder a Describe job names.
fn detector_of(job: &Job) -> Result<PathBuf, Failure> {
    job.detector.clone().ok_or_else(|| "Describe needs the OWLv2 object finder or the SAM 3 model".into())
}

/// Describe on the open models: OWLv2's boxes for `text` on `job`'s photo, each segmented by
/// SAM 2.1 (`job.dir`), merged; both analyses of the photo are kept for its next description.
/// `None` when nothing matches.
fn describe(state: &mut State, shared: &Shared, job: &mut Job, text: &str, placement: Placement) -> Result<Option<SegMask>, Failure> {
    let detector = detector_of(job)?;
    sam2_model(state, shared, &job.dir, placement)?;
    owl_model(state, shared, &detector)?;
    if state.sam2_cache.as_ref().is_none_or(|(k, _)| *k != job.key) {
        (state.sam2_cache, state.sam2_input, state.owl_cache) = (None, None, None);
        let input = render(job)?;
        let _busy = Flag::raise(&shared.encoding);
        let m = state.sam2.as_ref().map(|(_, m)| m).ok_or("the model did not load")?;
        let t = web_time::Instant::now();
        state.sam2_cache = Some((job.key, m.encode(&input.rgb, input.w, input.h)?));
        log::info!("SAM 2.1 encoded {}×{} in {:?}", input.w, input.h, t.elapsed());
        state.sam2_input = Some(input);
    }
    if state.owl_cache.as_ref().is_none_or(|(k, _)| *k != job.key) {
        state.owl_cache = None;
        let input = state.sam2_input.as_ref().ok_or("the photo is not available")?;
        let owl = state.owl.as_ref().map(|(_, m)| m).ok_or("the object finder did not load")?;
        let _busy = Flag::raise(&shared.encoding);
        let t = web_time::Instant::now();
        let enc = owl.encode(&input.rgb, input.w, input.h)?;
        log::info!("OWLv2 analysed {}×{} in {:?}", input.w, input.h, t.elapsed());
        state.owl_cache = Some((job.key, enc));
    }
    let (Some((_, owl)), Some((_, owl_enc)), Some((_, sam)), Some((_, sam_enc))) = (&state.owl, &state.owl_cache, &state.sam2, &state.sam2_cache)
    else {
        return Err("the Describe models are not ready".into());
    };
    let t = web_time::Instant::now();
    let p = lcc_segment::describe::describe(owl, owl_enc, sam, sam_enc, text, &Default::default())?;
    log::info!("Describe {text:?} in {:?}", t.elapsed());
    Ok(p.map(|p| SegMask::from_logits(lcc_segment::SAM2_MASK_SIDE, &p.logits)))
}

/// A Describe detail pass on the open models: `region` of the photo rendered again and both
/// analysed afresh (nothing kept).
fn describe_detail(
    state: &mut State,
    shared: &Shared,
    job: &mut Job,
    text: &str,
    region: [f64; 4],
    placement: Placement,
) -> Result<Option<SegMask>, Failure> {
    let detector = detector_of(job)?;
    let (rw, rh) = (region[2] - region[0], region[3] - region[1]);
    if !(rw > 0.0 && rh > 0.0) {
        return Ok(None);
    }
    let input = render(job)?;
    let t = web_time::Instant::now();
    sam2_model(state, shared, &job.dir, placement)?;
    owl_model(state, shared, &detector)?;
    let (Some((_, owl)), Some((_, sam))) = (&state.owl, &state.sam2) else { return Err("the Describe models did not load".into()) };
    let (owl_enc, sam_enc) = {
        let _busy = Flag::raise(&shared.encoding);
        (owl.encode(&input.rgb, input.w, input.h)?, sam.encode(&input.rgb, input.w, input.h)?)
    };
    let p = lcc_segment::describe::describe(owl, &owl_enc, sam, &sam_enc, text, &Default::default())?;
    log::info!("Describe detail pass ({}×{}) in {:?}", input.w, input.h, t.elapsed());
    Ok(p.map(|p| SegMask::from_logits_in(lcc_segment::SAM2_MASK_SIDE, &p.logits, region)))
}

/// Object clicks (and their detail passes) with SAM 2.1; Describe on the OWLv2 finder and SAM 2.1.
fn sam2_job(state: &mut State, shared: &Shared, job: &mut Job, placement: Placement) -> Result<Option<SegMask>, Failure> {
    let side = lcc_segment::SAM2_MASK_SIDE;
    let encode = |m: &Sam2, input: &Input| -> Result<Sam2Encoded, Failure> {
        let t = web_time::Instant::now();
        let enc = m.encode(&input.rgb, input.w, input.h)?;
        log::info!("SAM 2.1 encoded {}×{} in {:?}", input.w, input.h, t.elapsed());
        Ok(enc)
    };
    let kind = std::mem::replace(&mut job.kind, Kind::Prepare);
    if let Kind::Detail { prompt, region } = kind {
        let clicks = match prompt {
            Prompt::Clicks(clicks) => clicks,
            Prompt::Text(text) => return describe_detail(state, shared, job, &text, region, placement),
        };
        let (rw, rh) = (region[2] - region[0], region[3] - region[1]);
        if !(rw > 0.0 && rh > 0.0) {
            return Ok(None);
        }
        let inside: Vec<lcc_segment::Click> = clicks
            .iter()
            .map(|c| ((c.at.x - region[0]) / rw, (c.at.y - region[1]) / rh, c.include))
            .filter(|(x, y, _)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
            .map(|(x, y, positive)| lcc_segment::Click { x: x as f32, y: y as f32, positive })
            .collect();
        if !inside.iter().any(|c| c.positive) {
            return Ok(None);
        }
        let input = render(job)?;
        let m = sam2_model(state, shared, &job.dir, placement)?;
        let enc = {
            let _busy = Flag::raise(&shared.encoding);
            encode(m, &input)?
        };
        let p = m.segment_clicks(&enc, &inside)?;
        return Ok(Some(SegMask::from_logits_in(side, &p.logits, region)));
    }
    if let Kind::Text(text) = kind {
        return describe(state, shared, job, &text, placement);
    }
    sam2_model(state, shared, &job.dir, placement)?;
    if state.sam2_cache.as_ref().is_none_or(|(k, _)| *k != job.key) {
        (state.sam2_cache, state.sam2_input) = (None, None);
        let input = render(job)?;
        let _busy = Flag::raise(&shared.encoding);
        let m = state.sam2.as_ref().map(|(_, m)| m).ok_or("the model did not load")?;
        state.sam2_cache = Some((job.key, encode(m, &input)?));
        state.sam2_input = Some(input);
    }
    let (Some((_, m)), Some((_, enc))) = (state.sam2.as_ref(), state.sam2_cache.as_ref()) else { return Err("the model is not ready".into()) };
    let input = state.sam2_input.as_ref().ok_or("the photo is not available")?;
    match kind {
        Kind::Clicks(clicks) => Ok(Some(SegMask::from_logits(side, &m.segment_clicks(enc, &clicks)?.logits))),
        Kind::Learn { mask } => {
            let alpha = mask_alpha(&mask, input, 128);
            let (emb, es, dim) = enc.embedding()?;
            let (aw, ah) = (alpha.width.max(1), alpha.height.max(1));
            let at = |u: f32, v: f32| {
                let x = ((u * aw as f32) as usize).min(aw - 1);
                let y = ((v * ah as f32) as usize).min(ah - 1);
                alpha.data.get(y * aw + x).copied().unwrap_or(0.0)
            };
            let ex = lcc_segment::concept::learn(&emb, es, dim, at, &input.rgb, input.w, input.h);
            state.extra = ex.and_then(|e| serde_json::to_value(e).ok());
            Ok(None)
        }
        Kind::Find { examples } => {
            let Some(found) = lcc_segment::concept::find(m, enc, &examples, &input.rgb, input.w, input.h)? else { return Ok(None) };
            let seeds: Vec<[f32; 2]> = found.seeds.iter().map(|(x, y)| [*x, *y]).collect();
            state.extra = Some(serde_json::json!({"seeds": seeds, "score": found.score}));
            Ok(Some(SegMask::from_logits(found.mask_side, &found.logits)))
        }
        _ => Ok(None),
    }
}

/// Side of the class map a group's crops are pasted into.
const CROWD_SIDE: usize = 1024;

/// The class map `whole` of `input`, with a group's small people segmented again on their own
/// crops ([`lcc_segment::people::PeopleModel::segment_people`]): in the whole photo's 256-cell
/// map someone in a group of 29 is a few cells tall, too few for their clothes, hair or body, and
/// for one person's mask. Unchanged when everyone is large enough, or (with a log line) when the
/// crops fail.
fn crowd_map(
    model: Option<&lcc_segment::people::PeopleModel>,
    input: &Input,
    whole: Option<lcc_segment::people::PeopleMap>,
    faces: &[lcc_segment::people::Face],
) -> Option<lcc_segment::people::PeopleMap> {
    let (Some(model), Some(map)) = (model, whole.as_ref()) else { return whole };
    let boxes = lcc_segment::people::person_boxes(faces, input.w, input.h);
    if boxes.is_empty() {
        return whole;
    }
    let t = web_time::Instant::now();
    match model.segment_people(&input.rgb, input.w, input.h, map, &boxes, CROWD_SIDE) {
        Ok(m) => {
            log::info!("people: {} small people cropped from {}×{} in {:?}", boxes.len(), input.w, input.h, t.elapsed());
            Some(m)
        }
        Err(e) => {
            log::warn!("people: the group's crops failed ({e}); using the whole photo's map");
            whole
        }
    }
}

/// Every face on the photo with its appearance signature, left to right
/// ([`lcc_segment::identity`]).
fn faces_job(
    state: &mut State,
    shared: &Shared,
    job: &mut Job,
    face: &std::path::Path,
    sam2: Option<PathBuf>,
    placement: Placement,
) -> Result<serde_json::Value, Failure> {
    ensure_faces(state, shared, face)?;
    let input = render(job)?;
    let (_, fm, pm) = state.faces.as_ref().ok_or("the faces model did not load")?;
    let map = match pm {
        Some(p) => Some(p.segment(&input.rgb, input.w, input.h)?),
        None => None,
    };
    let mut faces = fm.faces_confirmed(&input.rgb, input.w, input.h, map.as_ref())?;
    faces.sort_by(|a, b| (a.detection.x0 + a.detection.x1).total_cmp(&(b.detection.x0 + b.detection.x1)));
    faces.truncate(64);
    let map = crowd_map(pm.as_ref(), &input, map, &faces);
    // SAM 2.1's features of each person's body, when it is installed (appearance beyond colour)
    let emb = match sam2.filter(|_| !faces.is_empty()) {
        Some(dir) => {
            let m = sam2_model(state, shared, &dir, placement)?;
            let enc = m.encode(&input.rgb, input.w, input.h)?;
            enc.embedding().ok()
        }
        None => None,
    };
    let (w, h) = (input.w.max(1) as f32, input.h.max(1) as f32);
    let out: Vec<serde_json::Value> = (0..faces.len())
        .filter_map(|i| {
            let mut sig = lcc_segment::identity::signature(&faces, i, map.as_ref(), &input.rgb, input.w, input.h)?;
            if let Some((e, side, dim)) = &emb {
                (sig.body, sig.face) = lcc_segment::identity::features(e, *side, *dim, &faces, i, map.as_ref(), input.w, input.h);
            }
            let d = &faces.get(i)?.detection;
            let rect = [d.x0 / w, d.y0 / h, d.x1 / w, d.y1 / h].map(|v| f64::from(v.clamp(0.0, 1.0)));
            Some(serde_json::json!({"rect": rect, "signature": sig}))
        })
        .collect();
    Ok(serde_json::Value::Array(out))
}

/// The alpha of `mask` over the model input, on a grid of about `edge` cells on the long side
/// (the mask's coordinates are the uncropped photo's, which the input is).
pub(crate) fn mask_alpha(mask: &lcc_develop::Mask, input: &Input, edge: usize) -> lcc_raster::Plane {
    let (w, h) = (input.w.max(1), input.h.max(1));
    let s = edge.max(8) as f64 / w.max(h) as f64;
    let (gw, gh) = (((w as f64 * s).round() as usize).max(1), ((h as f64 * s).round() as usize).max(1));
    let lin = |v: u8| {
        let c = f32::from(v) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let px = |x: usize, y: usize| -> [f32; 3] {
        let (sx, sy) = ((x * w / gw).min(w - 1), (y * h / gh).min(h - 1));
        let i = (sy * w + sx) * 3;
        match input.rgb.get(i..i + 3) {
            Some([r, g, b]) => [lin(*r), lin(*g), lin(*b)],
            _ => [0.0; 3],
        }
    };
    let img = lcc_raster::Rgb32f::from_fn(gw, gh, px);
    let log_l = lcc_raster::Plane::from_fn(gw, gh, |x, y| {
        let p = img.data.get(y * gw + x).copied().unwrap_or_default();
        (0.2627 * p[0] + 0.678 * p[1] + 0.0593 * p[2]).max(1e-6).log2()
    });
    let frame = lcc_pipeline::geometry::Frame::new(gw, gh, &lcc_develop::DevelopSettings::default(), false);
    let mut m = mask.clone();
    m.visible = true;
    m.adjust.amount = 100.0;
    lcc_pipeline::masks::evaluate_one(&m, &frame, gw, gh, &img, &log_l, 0.0, None)
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
        Kind::Learn { .. } | Kind::Find { .. } | Kind::Faces { .. } | Kind::People { .. } => {
            return Err("remembered objects, appearance and people run locally".into());
        }
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
        Some(r) => SegMask::from_logits_in(lcc_segment::MASK_SIDE, &l, r),
        None => SegMask::from_logits(lcc_segment::MASK_SIDE, &l),
    }))
}

/// Load MediaPipe's faces model in `face` (and the people model beside it) unless loaded.
fn ensure_faces(state: &mut State, shared: &Shared, face: &std::path::Path) -> Result<(), Failure> {
    if state.faces.as_ref().is_none_or(|(d, ..)| d != face) {
        let _busy = Flag::raise(&shared.encoding);
        let m = lcc_segment::people::FaceModel::load(face)?;
        state.faces = Some((face.to_path_buf(), m, crate::eyes::people_beside(face)));
        state.people_seen = None;
    }
    Ok(())
}

/// A [`Kind::People`] request: the selection as a stored segmentation (`None`: nobody, or none
/// of the parts, is there).
#[allow(clippy::too_many_arguments)]
fn people_job(
    state: &mut State,
    shared: &Shared,
    job: &mut Job,
    face: &std::path::Path,
    person: u32,
    parts: &[String],
    sam2: Option<&std::path::Path>,
    placement: Placement,
) -> Result<Option<SegMask>, Failure> {
    let req = lcc_segment::people::PeopleRequest::parse(person, parts).ok_or_else(|| Failure::Other(format!("unknown People part in {parts:?}")))?;
    ensure_faces(state, shared, face)?;
    if state.people_seen.as_ref().is_none_or(|(k, _)| *k != job.key || job.key == 0) {
        let input = render(job)?;
        let _busy = Flag::raise(&shared.encoding);
        let (_, fm, pm) = state.faces.as_ref().ok_or("the faces model did not load")?;
        let map = match pm {
            Some(p) => Some(p.segment(&input.rgb, input.w, input.h)?),
            None => None,
        };
        let mut faces = fm.faces_confirmed(&input.rgb, input.w, input.h, map.as_ref())?;
        faces.sort_by(|a, b| (a.detection.x0 + a.detection.x1).total_cmp(&(b.detection.x0 + b.detection.x1)));
        faces.truncate(64);
        let map = crowd_map(pm.as_ref(), &input, map, &faces);
        state.people_seen = Some((job.key, PeopleSeen { input, map, faces, instances: None }));
    }
    // one person of several: everyone's silhouette decides whose an overlapping pixel is
    let crowd = person > 0 && state.people_seen.as_ref().is_some_and(|(_, p)| p.faces.len() > 1 && p.instances.is_none());
    if let Some(dir) = sam2.filter(|_| crowd) {
        let found = silhouettes(state, shared, dir, placement);
        if let Some((_, p)) = state.people_seen.as_mut() {
            p.instances = Some(found);
        }
    }
    let (_, seen) = state.people_seen.as_ref().ok_or("the photo is not available")?;
    let instances = seen.instances.as_ref().and_then(Option::as_ref).filter(|_| person > 0);
    let g = lcc_segment::people::people_mask_with(&req, &seen.input.rgb, seen.input.w, seen.input.h, seen.map.as_ref(), &seen.faces, instances, None);
    // nobody found (the models ran): an empty selection, not the saliency guess a mask without
    // one falls back to
    Ok(Some(match g {
        Some(g) => match g.rect {
            Some(r) => SegMask::from_logits_in(g.side, &g.logits, r),
            None => SegMask::from_logits(g.side, &g.logits),
        },
        None => SegMask::from_logits(1, &[-16.0]),
    }))
}

/// Everyone's silhouette on the analysed photo (SAM 2.1; for each face: clicks on it and down
/// its body, the other faces left out); `None` (with a log line) when SAM 2.1 can't give them.
fn silhouettes(state: &mut State, shared: &Shared, dir: &std::path::Path, placement: Placement) -> Option<lcc_segment::people::Instances> {
    let seen = state.people_seen.take();
    let out = (|| -> Result<lcc_segment::people::Instances, Failure> {
        let (_, p) = seen.as_ref().ok_or("the photo is not available")?;
        let m = sam2_model(state, shared, dir, placement)?;
        Ok(person_silhouettes(m, &p.input.rgb, p.input.w, p.input.h, &p.faces)?)
    })();
    state.people_seen = seen;
    match out {
        Ok(i) => Some(i),
        Err(e) => {
            log::warn!("people: no silhouettes from SAM 2.1 ({})", e.message());
            None
        }
    }
}

/// Everyone's silhouette on an 8-bit RGB picture `w × h` with SAM 2.1 `m`: for each face, clicks
/// on it and down its body, the other faces left out. People masks and Retouch share it, so one
/// person of several is the same pixels in both.
pub(crate) fn person_silhouettes(
    m: &Sam2,
    rgb: &[u8],
    w: usize,
    h: usize,
    faces: &[lcc_segment::people::Face],
) -> Result<lcc_segment::people::Instances, lcc_segment::Error> {
    let (wf, hf) = (w.max(1) as f32, h.max(1) as f32);
    let centre = |f: &lcc_segment::people::Face| {
        let d = &f.detection;
        (((d.x0 + d.x1) / 2.0 / wf).clamp(0.0, 1.0), ((d.y0 + d.y1) / 2.0 / hf).clamp(0.0, 1.0), ((d.y1 - d.y0) / hf).max(0.0))
    };
    let enc = m.encode(rgb, w, h)?;
    let mut people = Vec::with_capacity(faces.len());
    let mut seeds = Vec::with_capacity(faces.len());
    for (k, f) in faces.iter().enumerate() {
        let (cx, cy, fh) = centre(f);
        seeds.push([cx, cy]);
        // their face, chest and middle (below the frame: left out); the others' faces out
        let mut clicks: Vec<lcc_segment::Click> =
            [0.0, 1.6, 3.2].iter().map(|d| cy + d * fh).filter(|y| *y < 0.98).map(|y| lcc_segment::Click { x: cx, y, positive: true }).collect();
        for (j, o) in faces.iter().enumerate() {
            if j != k {
                let (ox, oy, _) = centre(o);
                clicks.push(lcc_segment::Click { x: ox, y: oy, positive: false });
            }
        }
        people.push(m.segment_clicks_with(&enc, &clicks, false)?.logits);
    }
    Ok(lcc_segment::people::Instances::new(lcc_segment::SAM2_MASK_SIDE, people, &seeds))
}

fn run_job(state: &mut State, shared: &Shared, job: &mut Job, placement: Placement) -> Result<Option<SegMask>, Failure> {
    if let Kind::People { face, person, parts, sam2 } = &job.kind {
        let (face, person, parts, sam2) = (face.clone(), *person, parts.clone(), sam2.clone());
        return people_job(state, shared, job, &face, person, &parts, sam2.as_deref(), placement);
    }
    if let Kind::Faces { face, sam2 } = &job.kind {
        let (face, sam2) = (face.clone(), sam2.clone());
        state.extra = Some(faces_job(state, shared, job, &face, sam2, placement)?);
        return Ok(None);
    }
    // SAM 2.1 runs here even with a remote SAM 3 worker (which serves SAM 3 only)
    if lcc_segment::is_sam2_dir(&job.dir) {
        return sam2_job(state, shared, job, placement);
    }
    if let Some(endpoint) = remote::configured() {
        let result = remote_job(state, shared, job, &endpoint);
        if result.is_err() {
            state.remote = None;
        }
        return result.map_err(|e| Failure::Other(format!("Remote SAM 3 failed: {e}. Check the worker and SSH tunnel, then retry.")));
    }
    let side = lcc_segment::MASK_SIDE;
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
        Kind::Learn { .. } | Kind::Find { .. } | Kind::Faces { .. } | Kind::People { .. } => {
            Err("remembered objects need SAM 2.1 (Preferences ▸ AI Models ▸ Object model)".into())
        }
        Kind::Text(text) => {
            let (m, enc) = encoded(state, shared, job, placement)?;
            Ok(text_logits(m, enc, &text)?.map(|l| SegMask::from_logits(side, &l)))
        }
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
                    let inside: Vec<lcc_segment::Click> = clicks
                        .iter()
                        .map(|c| ((c.at.x - region[0]) / rw, (c.at.y - region[1]) / rh, c.include))
                        .filter(|(x, y, _)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
                        .map(|(x, y, positive)| lcc_segment::Click { x: x as f32, y: y as f32, positive })
                        .collect();
                    if !inside.iter().any(|c| c.positive) {
                        return Ok(None);
                    }
                    Some(m.segment_clicks(&mut enc, &inside)?.logits)
                }
                Prompt::Text(text) => text_logits(m, &mut enc, &text)?,
            };
            log::info!("SAM 3 detail pass ({}×{}) in {:?}", input.w, input.h, t.elapsed());
            Ok(logits.map(|l| SegMask::from_logits_in(side, &l, region)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(tag: Tag, reply: &mpsc::Sender<Outcome>) -> Job {
        // an empty folder: loading fails with an error (no model files)
        let dir = std::env::temp_dir().join(format!("lc-sam3-worker-none-{}", std::process::id()));
        Job { dir, detector: None, key: 1, render: None, missing: None, kind: Kind::Prepare, tag, reply: reply.clone(), cancel: None, low: false }
    }

    /// OneSlips 9bb601286 (test notes 2026-10-25 note 6): a request the user makes is taken before the background work
    /// queued ahead of it (finishing an import), which goes on after it.
    #[test]
    fn the_users_requests_go_before_background_work() {
        let shared = Shared::default();
        let (tx, rx) = mpsc::channel();
        let (jtx, jrx) = mpsc::channel();
        for _ in 0..3 {
            let mut j = job(Tag::Wait, &tx);
            j.low = true;
            jtx.send(j).unwrap();
        }
        // the user's: told apart by how it fails
        let mut mine = job(Tag::Wait, &tx);
        mine.kind = Kind::Panic;
        jtx.send(mine).unwrap();
        shared.pending.store(4, Ordering::SeqCst);
        shared.low_pending.store(3, Ordering::SeqCst);
        drop(jtx);
        run(&jrx, &shared, Duration::from_secs(60), Placement::Cpu);
        let order: Vec<bool> = rx.try_iter().map(|o| o.result.unwrap_err().contains("failed unexpectedly")).collect();
        assert_eq!(order, [true, false, false, false], "the user's first, then the background's");
        assert_eq!(shared.low_pending.load(Ordering::SeqCst), 0);
        assert_eq!(shared.pending.load(Ordering::SeqCst), 0);
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
        // no GPU in this build, or <PREFIX>_GPU=0 / <PREFIX>_GPU_BACKEND=off
        let out = run_kinds(Placement::Cpu, vec![Kind::FailOnGpu { panic: true }]);
        assert!(matches!(out[0].result, Ok(None)));
        assert_eq!(out[0].notice, None);
        if !lcc_segment::GPU_BUILD {
            assert_eq!(Placement::initial(), Placement::Cpu);
        }
    }

    #[test]
    fn missing_model_files_are_not_retried_on_the_cpu() {
        let out = run_kinds(Placement::Gpu, vec![Kind::Prepare]);
        assert!(out[0].result.as_ref().unwrap_err().contains("not found"));
        assert_eq!(out[0].notice, None);
        let missing = lcc_segment::Error::Missing(PathBuf::from("models"));
        assert!(matches!(Failure::from(missing), Failure::Other(_)));
    }

    #[test]
    fn a_damaged_sam2_model_is_an_error_and_describe_asks_for_sam3() {
        let dir = std::env::temp_dir().join(format!("lcc-sam2-damaged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), br#"{"model_type": "sam2_video"}"#).unwrap();
        std::fs::write(dir.join("model.safetensors"), b"\x10\x00\x00\x00\x00\x00\x00\x00{not json at all}").unwrap();
        let (tx, rx) = mpsc::channel();
        let (jtx, jrx) = mpsc::channel();
        let shared = Shared::default();
        for kind in [Kind::Clicks(vec![lcc_segment::Click { x: 0.5, y: 0.5, positive: true }]), Kind::Text("sky".into()), Kind::Prepare] {
            let mut j = job(Tag::Wait, &tx);
            (j.kind, j.dir) = (kind, dir.clone());
            jtx.send(j).unwrap();
        }
        shared.pending.store(3, Ordering::SeqCst);
        drop(jtx);
        run(&jrx, &shared, Duration::from_secs(60), Placement::Cpu);
        let out: Vec<Outcome> = rx.try_iter().collect();
        assert_eq!(out.len(), 3);
        assert!(out[0].result.is_err() && out[2].result.is_err(), "damaged weights are errors");
        assert!(out[1].result.as_ref().unwrap_err().contains("SAM 3"));
        assert_eq!(shared.pending.load(Ordering::SeqCst), 0);
        assert!(!shared.loaded.load(Ordering::SeqCst));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_request_for_the_same_selection_supersedes_a_queued_one() {
        let (tx, rx) = mpsc::channel();
        let (jtx, jrx) = mpsc::channel();
        let clicks = |n: usize| Tag::Clicks {
            photo: lcc_catalog::PhotoId(1),
            mask: 1,
            comp: 0,
            hint: vec![lcc_geom::Point::new(0.5, 0.5); n],
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
