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
//! the next job uses it again. `LIGHTCRAFT_GPU=0` or `LIGHTCRAFT_GPU_BACKEND=off` keep the model on
//! the CPU from the start.
//!
//! Where the model runs is one pure decision ([`Placement::from_policy`]) behind one loader seam
//! ([`Placement::load_with`]), and an [`Executor`] seam lets tests drive this loop's queue, skip and
//! retry rules with a fake instead of a model. Nothing here picks the CPU for the user: #2533's
//! *Run on: CPU* setting supplies [`configured_run_on`], the third input of [`Placement::from_policy`];
//! that value is inert until the setting is routed here, and an explicit CPU
//! choice stays sticky (the GPU to CPU retry only ever follows a GPU attempt, and an idle unload never
//! sees the placement).

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use lightcraft_develop::SegMask;
use lightcraft_segment::{
    Encoded, Sam3,
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
    Clicks(Vec<lightcraft_segment::Click>),
    Text(String),
    /// The photo's subject among its people and animals (`Sam3::segment_subject`); `focus`: where
    /// the camera focused (normalized), when the file records it. No result: the subject is
    /// something else.
    Subject {
        focus: Option<(f32, f32)>,
    },
    /// A zoomed-in pass over `region` (normalized) of the photo, rendered by the job's `render`.
    Detail {
        prompt: Prompt,
        region: [f64; 4],
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
    /// Which photo and look the request is about (the encode cache's key).
    pub key: u64,
    /// Renders the model input when the cache doesn't hold `key` (always for a detail pass).
    pub render: Option<RenderJob>,
    /// The photo's file, when it is missing on disk (for a clearer error if rendering fails).
    pub missing: Option<String>,
    pub kind: Kind,
    pub tag: Tag,
    pub reply: mpsc::Sender<Outcome>,
}

/// A finished request: the segmentation (`None`: nothing selected / nothing to report).
pub struct Outcome {
    pub tag: Tag,
    pub result: Result<Option<SegMask>, String>,
    /// A newer request for the same selection came in; this one was skipped.
    pub superseded: bool,
    /// Something the user should know besides the result ([`GPU_FAILED`]).
    pub notice: Option<String>,
}

/// The notice of the job after which the model runs on the CPU.
pub const GPU_FAILED: &str =
    "The GPU couldn't run the AI mask model, so it runs on the CPU until LightCraft restarts (slower). Details are in the log.";

/// Where the user asked the model to run: PR #2533's *Run on* setting (`Automatic` or `CPU`).
///
/// This is the adapter between that setting and this worker. The setting's storage, its Settings
/// entry and the reason shown to the user are #2533's; this module only needs the value.
/// Nothing here selects the CPU for anyone: [`configured_run_on`] answers
/// [`RunOn::Automatic`] until #2533 routes the setting in, so behaviour is exactly as before and no
/// user-facing mitigation is claimed.
#[allow(dead_code)] // `Cpu` is constructed by the tests here and, at integration, by #2533's setting
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunOn {
    /// The GPU when the build and the environment allow it, the CPU otherwise.
    Automatic,
    /// The CPU always: slower, but it keeps the model off the GPU (the point of the setting).
    Cpu,
}

/// The user's preference, as this worker sees it: the single point where it enters.
///
/// Replaced by #2533's settings value when it lands, at the one line below. Read once per worker
/// start, so the decision stays in one place. A preference changed while the worker is alive
/// applies at the next start, since the worker is respawned on demand.
fn configured_run_on() -> RunOn {
    RunOn::Automatic
}

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
    fn initial(run_on: RunOn) -> Self {
        if !lightcraft_segment::GPU_BUILD {
            return Self::from_policy(false, false, run_on);
        }
        let disabled = lightcraft_gpu::env_off_reason();
        if let Some(why) = disabled {
            log::info!("SAM 3 runs on the CPU ({why})");
        }
        Self::from_policy(true, disabled.is_some(), run_on)
    }

    /// The one placement decision, as a pure function of the build's GPU path and the environment
    /// switch, so every combination is testable without a device or an environment change
    /// (`std::env::set_var` is process-wide and unsafe to mutate while other tests run).
    ///
    /// [`RunOn::Cpu`] wins over both the build and the environment switch, and it is sticky: no path
    /// in this worker moves a CPU placement to the GPU. The retry in [`run`] only ever goes GPU to
    /// CPU, [`unload`] never sees the placement, and a restart asks [`configured_run_on`] again, so an
    /// explicit CPU choice survives a model load, an encode-cache invalidation, an idle unload and a
    /// respawn. The user-facing setting that supplies this value is tracked in #2533.
    fn from_policy(gpu_build: bool, gpu_disabled: bool, run_on: RunOn) -> Self {
        match run_on {
            RunOn::Cpu => Self::Cpu,
            RunOn::Automatic if gpu_build && !gpu_disabled => Self::Gpu,
            RunOn::Automatic => Self::Cpu,
        }
    }

    /// Call `gpu` or `cpu` for this placement and nothing else: the loader seam. Tests pass fake
    /// closures, so "the CPU route never touches the GPU loader" is provable with no candle device,
    /// no model files and no download.
    fn load_with<T>(self, gpu: impl FnOnce() -> T, cpu: impl FnOnce() -> T) -> T {
        match self {
            Self::Gpu => gpu(),
            Self::Cpu => cpu(),
        }
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
        match std::thread::Builder::new()
            .name("sam3".into())
            .spawn(move || run(&rx, &shared, IDLE_UNLOAD, Placement::initial(configured_run_on()), &Model))
        {
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

/// The model and the last encoded photo.
#[derive(Default)]
struct State {
    model: Option<(PathBuf, Sam3)>,
    cache: Option<(u64, Encoded)>,
    remote: Option<remote::Client>,
}

/// Why a queued request can be skipped: the session drops its result anyway, so running it would
/// only queue more GPU work for the same visible outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Redundant {
    /// A later [`Kind::Prepare`] for the same photo. Its outcome is `Ok(None)` either way, and the
    /// later request encodes the photo again.
    Prepare,
    /// A later [`Tag::Clicks`] for the same selection. The session keeps only the newest clicks
    /// (`Session::segmenter.pending`) and returns early on a result whose clicks are not those
    /// (`crates/engine/src/segment.rs`, `apply_outcome`), and the newest request's hint is a superset
    /// because the session rebuilds it from the pending clicks.
    Clicks,
    /// A later [`Tag::Detail`] pass for the same selection. `apply_outcome` skips a patch whose
    /// recorded shape is no longer the component's shape, and two passes from one shape write the
    /// same `detail`. (Reachable only if `Session::segment_detail`'s `detail_busy` gate that refuses
    /// a second queued pass is loosened; if it ever is, this rule must compare shapes too, because an
    /// undo back to an older shape makes the *older* pass the applicable one.)
    Detail,
}

/// Why `job` is pointless now that `later` is queued, or `None` when it still has to run.
///
/// Only requests whose **result the session discards** are redundant. A [`Tag::NewPrompt`] never is:
/// `mask.add` creates one mask per request and `mask.addComponent` adds one component per request
/// (the same text and op twice included), so two identical Describe requests are two intentional
/// selections and no tag comparison can prove one of them pointless. Dropping one here would also be
/// silent: a skipped job's outcome is `superseded`, which `apply_outcome` returns on before it would
/// tell the user anything, whereas a Describe result for a photo the user has left is reported ("The
/// photo changed before … was found"). A queued Describe is not droppable for an inactive photo
/// either: the session checks the active photo when the result *arrives*, not when the request was
/// queued, so a request can still be applied if the user comes back to that photo. Bounding how many
/// Describe requests are queued is an admission decision in `Session::segment_text_later` (a visible
/// error to the caller), not a silent drop here; see DESIGN.md.
fn redundant_with(later: &Job, job: &Job) -> Option<Redundant> {
    match (&later.tag, &job.tag) {
        (Tag::Prepare, Tag::Prepare) => (later.key == job.key).then_some(Redundant::Prepare),
        (Tag::Clicks { photo: a, mask: am, comp: ac, .. }, Tag::Clicks { photo: b, mask: bm, comp: bc, .. }) => {
            ((a, am, ac) == (b, bm, bc)).then_some(Redundant::Clicks)
        }
        (Tag::Detail { photo: a, mask: am, comp: ac, .. }, Tag::Detail { photo: b, mask: bm, comp: bc, .. }) => {
            ((a, am, ac) == (b, bm, bc)).then_some(Redundant::Detail)
        }
        _ => None,
    }
}

/// Whether a failed attempt on `placement` may be retried on the CPU. Only a GPU attempt is retried
/// (an attempt on the CPU has nothing left to try), and never while a remote worker is configured:
/// the remote does the work whatever this machine's placement says.
fn may_retry_on_cpu(placement: Placement, remote: bool) -> bool {
    placement == Placement::Gpu && !remote
}

fn run(rx: &mpsc::Receiver<Job>, shared: &Shared, idle: Duration, mut placement: Placement, exec: &dyn Executor) {
    let mut state = State::default();
    let mut queue: VecDeque<Job> = VecDeque::new();
    loop {
        if queue.is_empty() {
            match rx.recv_timeout(idle) {
                Ok(j) => queue.push_back(j),
                Err(RecvTimeoutError::Timeout) => {
                    unload(&mut state, shared, idle);
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
        if let Some(why) = queue.iter().find_map(|later| redundant_with(later, &job)) {
            log::debug!("SAM 3 skipping a queued request ({why:?})");
            drop(done);
            let _ = job.reply.send(Outcome { tag: job.tag, result: Ok(None), superseded: true, notice: None });
            continue;
        }
        // what a retry on the CPU needs (running the job takes them)
        let retry = may_retry_on_cpu(placement, exec.remote_configured()).then(|| (job.kind.clone(), job.render.clone()));
        let mut result = exec.attempt(&mut state, shared, &mut job, placement);
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
            match exec.attempt(&mut state, shared, &mut job, Placement::Cpu) {
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
            state.cache = None;
        }
        shared.loaded.store(state.model.is_some() || state.remote.is_some(), Ordering::SeqCst);
        drop(done);
        let _ = job.reply.send(Outcome { tag: job.tag, result, superseded: false, notice });
    }
}

/// Runs one job on a placement: [`attempt`] for real work, or a fake in tests (no model, no candle
/// device, no GPU work, no download).
trait Executor {
    fn attempt(&self, state: &mut State, shared: &Shared, job: &mut Job, placement: Placement) -> Result<Option<SegMask>, Failure>;

    /// Whether a remote SAM worker is configured, in which case a retry on this machine's CPU is
    /// pointless: the remote does the work. The real executor asks the environment (as before, once
    /// per job); a test fake answers for itself, so the placement and retry tests cannot be bypassed
    /// by this process's environment and no test has to mutate it.
    fn remote_configured(&self) -> bool;
}

/// The real executor: [`attempt`], which loads the model and runs the job on it.
struct Model;

impl Executor for Model {
    fn attempt(&self, state: &mut State, shared: &Shared, job: &mut Job, placement: Placement) -> Result<Option<SegMask>, Failure> {
        attempt(state, shared, job, placement)
    }

    fn remote_configured(&self) -> bool {
        remote::configured().is_some()
    }
}

/// Drop what the model holds after `idle` without requests. `placement` is deliberately **not** a
/// parameter: an idle unload cannot change where the model runs, which is what keeps an explicit CPU
/// choice (or a placement the GPU already failed on) across the unload. The next load asks
/// [`Placement::load_with`] again, on the same placement.
fn unload(state: &mut State, shared: &Shared, idle: Duration) {
    if state.model.is_some() || state.remote.is_some() {
        *state = State::default();
        shared.loaded.store(false, Ordering::SeqCst);
        log::info!("SAM 3 unloaded after {} minutes without use", idle.as_secs() / 60);
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
        let _busy = Flag::raise(&shared.encoding);
        let t = web_time::Instant::now();
        let m = placement.load_with(|| Sam3::load(dir), || Sam3::load_cpu(dir))?;
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
        Kind::Subject { focus } => Operation::Subject(focus.map(|(x, y)| [x, y])),
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
        Kind::Subject { focus } => {
            let (m, enc) = encoded(state, shared, job, placement)?;
            // (the reference score threshold, as Describe uses)
            Ok(m.segment_subject(enc, focus, 0.5)?.map(|p| SegMask::from_logits(side, &p.logits)))
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
                    let inside: Vec<lightcraft_segment::Click> = clicks
                        .iter()
                        .map(|c| ((c.at.x - region[0]) / rw, (c.at.y - region[1]) / rh, c.include))
                        .filter(|(x, y, _)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y))
                        .map(|(x, y, positive)| lightcraft_segment::Click { x: x as f32, y: y as f32, positive })
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
    use std::cell::RefCell;

    #[test]
    fn disabled_gpu_policy_always_chooses_cpu_without_creating_a_device() {
        assert_eq!(Placement::from_policy(true, true, RunOn::Automatic), Placement::Cpu);
        assert_eq!(Placement::from_policy(false, true, RunOn::Automatic), Placement::Cpu);
        assert_eq!(Placement::from_policy(false, false, RunOn::Automatic), Placement::Cpu);
    }

    #[test]
    fn automatic_gpu_policy_is_preserved_when_allowed() {
        assert_eq!(Placement::from_policy(true, false, RunOn::Automatic), Placement::Gpu);
    }

    #[test]
    fn cpu_placement_calls_only_the_cpu_factory() {
        let gpu_calls = std::cell::Cell::new(0);
        let cpu_calls = std::cell::Cell::new(0);
        let result = Placement::from_policy(true, true, RunOn::Automatic).load_with(
            || {
                gpu_calls.set(gpu_calls.get() + 1);
                "GPU"
            },
            || {
                cpu_calls.set(cpu_calls.get() + 1);
                "CPU"
            },
        );
        assert_eq!(result, "CPU");
        assert_eq!((gpu_calls.get(), cpu_calls.get()), (0, 1));
    }

    #[test]
    fn cpu_factory_failure_does_not_initialize_gpu() {
        let gpu_calls = std::cell::Cell::new(0);
        let result: Result<(), &str> = Placement::Cpu.load_with(
            || {
                gpu_calls.set(gpu_calls.get() + 1);
                Ok(())
            },
            || Err("CPU-only failure"),
        );
        assert_eq!(result, Err("CPU-only failure"));
        assert_eq!(gpu_calls.get(), 0);
    }

    #[test]
    fn gpu_placement_calls_only_the_gpu_factory_before_worker_retry_policy() {
        let cpu_calls = std::cell::Cell::new(0);
        let result: Result<(), &str> = Placement::Gpu.load_with(
            || Err("GPU failure"),
            || {
                cpu_calls.set(cpu_calls.get() + 1);
                Ok(())
            },
        );
        assert_eq!(result, Err("GPU failure"));
        assert_eq!(cpu_calls.get(), 0);
    }

    fn job(tag: Tag, reply: &mpsc::Sender<Outcome>) -> Job {
        // an empty folder: loading fails with an error (no model files)
        let dir = std::env::temp_dir().join(format!("lc-sam3-worker-none-{}", std::process::id()));
        Job { dir, key: 1, render: None, missing: None, kind: Kind::Prepare, tag, reply: reply.clone() }
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
        run(&jrx, &shared, Duration::from_secs(60), Placement::Cpu, &Model);
        let first = rx.recv().unwrap();
        assert!(first.result.unwrap_err().contains("failed unexpectedly"));
        let second = rx.recv().unwrap();
        assert!(second.result.is_err(), "the next job still runs");
        assert_eq!(shared.pending.load(Ordering::SeqCst), 0, "counters reset");
        assert!(!shared.encoding.load(Ordering::SeqCst));
    }

    /// Run `kinds` in turn on a worker that starts on `placement`; their outcomes.
    fn run_kinds(placement: Placement, kinds: Vec<Kind>) -> Vec<Outcome> {
        // Tag::Wait: these requests are independent of each other, so only the placement can matter
        run_tagged(placement, kinds.into_iter().map(|kind| (kind, Tag::Wait)).collect(), &Model)
    }

    /// Run `jobs` (kind and tag) through the real worker loop with `exec`: the outcomes in order.
    fn run_tagged(placement: Placement, jobs: Vec<(Kind, Tag)>, exec: &dyn Executor) -> Vec<Outcome> {
        let (tx, rx) = mpsc::channel();
        let (jtx, jrx) = mpsc::channel();
        let shared = Shared::default();
        let details = jobs.iter().filter(|(kind, _)| matches!(kind, Kind::Detail { .. })).count();
        shared.detail.store(details, Ordering::SeqCst);
        shared.pending.store(jobs.len() - details, Ordering::SeqCst);
        for (kind, tag) in jobs {
            let mut j = job(tag, &tx);
            j.kind = kind;
            jtx.send(j).unwrap();
        }
        drop(jtx);
        run(&jrx, &shared, Duration::from_secs(60), placement, exec);
        assert_eq!((shared.pending.load(Ordering::SeqCst), shared.detail.load(Ordering::SeqCst)), (0, 0), "counters reset");
        assert!(!shared.encoding.load(Ordering::SeqCst));
        rx.try_iter().collect()
    }

    /// A stand-in for the model: records the placement of every attempt and answers from `answers`
    /// (then `Ok(None)`), so the loop's queue, skip, retry and placement decisions are testable with
    /// no candle, no Metal device, no model files and no download.
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<Placement>>,
        answers: RefCell<VecDeque<Result<Option<SegMask>, Failure>>>,
        /// What the fake answers [`Executor::remote_configured`]; never read from the environment.
        remote: bool,
    }

    impl Fake {
        fn new(answers: Vec<Result<Option<SegMask>, Failure>>) -> Self {
            Self { calls: RefCell::new(Vec::new()), answers: RefCell::new(answers.into()), remote: false }
        }

        /// A fake for a machine with a remote SAM worker configured (so no CPU retry may happen).
        fn remote(answers: Vec<Result<Option<SegMask>, Failure>>) -> Self {
            Self { remote: true, ..Self::new(answers) }
        }

        /// The placement of every attempt, in order.
        fn calls(&self) -> Vec<Placement> {
            self.calls.borrow().clone()
        }
    }

    impl Executor for Fake {
        fn attempt(&self, _state: &mut State, _shared: &Shared, _job: &mut Job, placement: Placement) -> Result<Option<SegMask>, Failure> {
            self.calls.borrow_mut().push(placement);
            self.answers.borrow_mut().pop_front().unwrap_or(Ok(None))
        }

        fn remote_configured(&self) -> bool {
            self.remote
        }
    }

    /// A tiny segmentation for the fake (no model involved).
    ///
    /// Quantizes `side * side` logits, so a small side needs no model, device or file.
    fn seg() -> SegMask {
        SegMask::from_logits(8, &[0.0_f32; 64])
    }

    /// The component shape the session records on a detail pass's tag.
    fn object_shape() -> lightcraft_develop::MaskShape {
        lightcraft_develop::MaskShape::Object { hint: Vec::new(), exclude: Vec::new(), seg: None, detail: Vec::new(), edge: 0.0 }
    }

    fn clicks_tag(photo: lightcraft_catalog::PhotoId, mask: u32, comp: usize) -> Tag {
        Tag::Clicks { photo, mask, comp, hint: vec![], exclude: vec![] }
    }

    fn detail_tag(photo: lightcraft_catalog::PhotoId, mask: u32, comp: usize) -> Tag {
        Tag::Detail { photo, mask, comp, shape: object_shape() }
    }

    fn detail_kind() -> Kind {
        Kind::Detail { prompt: Prompt::Text("hair".into()), region: [0.1, 0.2, 0.6, 0.7] }
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
            assert_eq!(Placement::initial(RunOn::Automatic), Placement::Cpu);
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
        run(&jrx, &shared, Duration::from_secs(60), Placement::Cpu, &Model);
        let out: Vec<Outcome> = rx.try_iter().collect();
        assert_eq!(out.len(), 3);
        assert!(out[0].superseded && out[1].superseded && !out[2].superseded);
        assert_eq!(shared.pending.load(Ordering::SeqCst), 0);
    }

    // ---- placement, loader and queue-policy invariants through the real worker loop ----

    #[test]
    fn only_a_gpu_attempt_without_a_remote_worker_may_be_retried_on_the_cpu() {
        assert!(may_retry_on_cpu(Placement::Gpu, false));
        assert!(!may_retry_on_cpu(Placement::Gpu, true), "a remote worker does the work");
        assert!(!may_retry_on_cpu(Placement::Cpu, false), "an attempt on the CPU has nothing left to try");
        assert!(!may_retry_on_cpu(Placement::Cpu, true));
    }

    #[test]
    fn a_cpu_preference_wins_over_the_build_and_the_environment_switch() {
        // #2533's Run on: CPU. The preference is an input to the one placement decision, so it wins on
        // a GPU-capable build with no environment switch, and it does not depend on the environment.
        assert_eq!(Placement::from_policy(true, false, RunOn::Cpu), Placement::Cpu, "a GPU-capable build");
        assert_eq!(Placement::from_policy(true, true, RunOn::Cpu), Placement::Cpu, "the switch is irrelevant");
        assert_eq!(Placement::from_policy(false, false, RunOn::Cpu), Placement::Cpu, "no GPU path");
        assert_eq!(Placement::from_policy(false, true, RunOn::Cpu), Placement::Cpu);
    }

    #[test]
    fn a_cpu_preference_never_consults_the_gpu_loader_on_a_gpu_build() {
        // positive control: the CPU loader is the one that runs; negative control: the GPU loader is
        // never called, so no Metal device and no model initialisation can happen on this path
        let gpu_calls = std::cell::Cell::new(0);
        let cpu_calls = std::cell::Cell::new(0);
        let result = Placement::from_policy(true, false, RunOn::Cpu).load_with(
            || {
                gpu_calls.set(gpu_calls.get() + 1);
                "GPU"
            },
            || {
                cpu_calls.set(cpu_calls.get() + 1);
                "CPU"
            },
        );
        assert_eq!(result, "CPU");
        assert_eq!((gpu_calls.get(), cpu_calls.get()), (0, 1), "the GPU factory is never entered");
    }

    #[test]
    fn a_cpu_preference_survives_a_cache_invalidating_error_and_stays_on_the_cpu() {
        // a non-detail error clears the encode cache (`state.cache`) and the model state; both resets
        // leave the placement alone, so the next request is still attempted on the CPU only
        let placement = Placement::from_policy(true, false, RunOn::Cpu);
        assert_eq!(placement, Placement::Cpu, "derived from the preference, not from the build");
        let fake = Fake::new(vec![Err(Failure::Other("the CPU failed this one".into())), Ok(None)]);
        let out = run_tagged(placement, vec![(Kind::Prepare, Tag::Wait), (Kind::Prepare, Tag::Wait)], &fake);
        assert_eq!(fake.calls(), vec![Placement::Cpu, Placement::Cpu], "the GPU is never tried");
        assert!(out[0].result.as_ref().unwrap_err().contains("the CPU failed this one"));
        assert_eq!(out[0].notice, None, "no fallback happened, so nothing is announced");
        assert!(out[1].result.is_ok(), "the next request runs");
    }

    #[test]
    fn an_idle_unload_cannot_move_the_placement() {
        // structural invariant: `unload` takes no placement and returns none, so the idle branch in
        // `run` cannot change where the model runs. An explicit CPU choice, or a placement the GPU
        // already failed on, therefore survives an idle unload; a restart asks `configured_run_on()`
        // again and derives the same answer (see the preference tests above).
        //
        // What this cannot show: the branch's own effect, because a `State` that holds a model cannot
        // be built without a model. Verifying the idle branch's effect requires a model-backed
        // integration test, not a no-model unit test.
        let shared = Shared::default();
        let mut state = State::default();
        unload(&mut state, &shared, IDLE_UNLOAD);
        assert!(!shared.loaded.load(Ordering::SeqCst), "nothing is loaded, so nothing is claimed");
        assert!(!shared.encoding.load(Ordering::SeqCst));
        let before = Placement::from_policy(true, false, RunOn::Cpu);
        assert_eq!(before, Placement::Cpu, "the preference decides, not the build");
        unload(&mut state, &shared, IDLE_UNLOAD);
        assert_eq!(Placement::from_policy(true, false, RunOn::Cpu), before, "the same preference still decides");
    }

    #[test]
    fn the_adapter_defaults_to_automatic_so_no_mitigation_is_claimed_yet() {
        // the negative control for the whole delta: until #2533 routes its setting here, the worker
        // asks for `Automatic`, which is exactly the pre-existing behaviour
        assert_eq!(configured_run_on(), RunOn::Automatic);
        assert_eq!(Placement::from_policy(true, false, configured_run_on()), Placement::Gpu, "unchanged default");
        assert_eq!(Placement::from_policy(false, false, configured_run_on()), Placement::Cpu);
    }

    #[test]
    fn a_cpu_placement_is_never_retried_on_the_gpu() {
        // an explicit CPU placement (no GPU path in this build, the environment switch, or #2533's
        // Run on: CPU) has no GPU fallback anywhere in the worker
        let fake = Fake::new(vec![Err(Failure::Model("a Metal kernel failed".into()))]);
        let out = run_tagged(Placement::Cpu, vec![(Kind::Prepare, Tag::Wait)], &fake);
        assert_eq!(fake.calls(), vec![Placement::Cpu], "one attempt, and it is the CPU");
        assert!(out[0].result.as_ref().unwrap_err().contains("a Metal kernel failed"));
        assert_eq!(out[0].notice, None, "no CPU to GPU fallback");
    }

    #[test]
    fn a_model_failure_on_the_gpu_is_retried_once_on_the_cpu_and_its_result_is_kept() {
        let fake = Fake::new(vec![Err(Failure::Model("a Metal kernel failed".into())), Ok(Some(seg()))]);
        let out = run_tagged(Placement::Gpu, vec![(Kind::Prepare, Tag::Wait)], &fake);
        assert_eq!(fake.calls(), vec![Placement::Gpu, Placement::Cpu], "one attempt on each");
        assert!(matches!(&out[0].result, Ok(Some(_))), "the CPU attempt's result is the job's: {:?}", out[0].result);
        assert_eq!(out[0].notice.as_deref(), Some(GPU_FAILED));
        assert!(!out[0].superseded);
    }

    #[test]
    fn the_cpu_stays_the_placement_for_the_next_job_after_a_successful_retry() {
        let fake =
            Fake::new(vec![Err(Failure::Model("a Metal kernel failed".into())), Ok(None), Err(Failure::Other("the model is not ready".into()))]);
        let out = run_tagged(Placement::Gpu, vec![(Kind::Prepare, Tag::Wait), (Kind::Prepare, Tag::Wait)], &fake);
        assert_eq!(fake.calls(), vec![Placement::Gpu, Placement::Cpu, Placement::Cpu], "the next job starts on the CPU");
        assert_eq!(out[0].notice.as_deref(), Some(GPU_FAILED), "said once");
        assert_eq!(out[1].notice, None, "and not again");
        assert!(out[1].result.as_ref().unwrap_err().contains("not ready"), "a later error is still reported");
    }

    #[test]
    fn a_cpu_failure_after_the_gpu_failure_keeps_the_original_gpu_error_and_the_next_job() {
        let fake = Fake::new(vec![Err(Failure::Model("a Metal kernel failed".into())), Err(Failure::Other("the CPU failed too".into())), Ok(None)]);
        let out = run_tagged(Placement::Gpu, vec![(Kind::Prepare, Tag::Wait), (Kind::Prepare, Tag::Wait)], &fake);
        assert_eq!(fake.calls(), vec![Placement::Gpu, Placement::Cpu, Placement::Gpu], "the GPU is tried again");
        assert_eq!(out[0].notice, None, "the GPU was not the cause, so nothing is said");
        // current behaviour, pinned as it is: the CPU attempt's error is only logged
        // (`log::warn!("SAM 3 failed on the CPU too: …")`); the job reports the ORIGINAL GPU error
        assert!(
            out[0].result.as_ref().unwrap_err().contains("a Metal kernel failed"),
            "the original GPU error is reported, not the CPU's: {:?}",
            out[0].result
        );
    }

    #[test]
    fn a_gpu_model_failure_with_a_remote_worker_configured_is_not_retried() {
        // the remote does the work, so this machine's placement is irrelevant and nothing is retried;
        // the fake answers for itself, so this process's environment cannot bypass the test
        let fake = Fake::remote(vec![Err(Failure::Model("the remote call failed".into()))]);
        let out = run_tagged(Placement::Gpu, vec![(Kind::Prepare, Tag::Wait)], &fake);
        assert_eq!(fake.calls(), vec![Placement::Gpu], "no CPU attempt");
        assert!(out[0].result.as_ref().unwrap_err().contains("the remote call failed"));
        assert_eq!(out[0].notice, None, "no CPU fallback notice");
    }

    #[test]
    fn a_failure_no_other_device_can_fix_is_not_retried_on_the_cpu() {
        let fake = Fake::new(vec![Err(Failure::Other("The SAM 3 model is not installed in /models".into()))]);
        let out = run_tagged(Placement::Gpu, vec![(Kind::Prepare, Tag::Wait)], &fake);
        assert_eq!(fake.calls(), vec![Placement::Gpu], "no second attempt");
        assert!(out[0].result.as_ref().unwrap_err().contains("not installed"));
        assert_eq!(out[0].notice, None);
    }

    #[test]
    fn a_later_click_request_skips_a_queued_one_before_any_work_starts() {
        // the session keeps only the newest clicks and its hint is a superset (it rebuilds it from
        // the pending clicks), and it returns early on a result whose clicks are not the newest
        let photo = lightcraft_catalog::PhotoId(1);
        let fake = Fake::new(vec![]);
        let jobs = vec![(Kind::Clicks(vec![]), clicks_tag(photo, 1, 0)), (Kind::Clicks(vec![]), clicks_tag(photo, 1, 0))];
        let out = run_tagged(Placement::Cpu, jobs, &fake);
        assert_eq!(fake.calls(), vec![Placement::Cpu], "only the newest click request reaches the model");
        assert_eq!(out.len(), 2, "both requests are answered");
        assert!(out[0].superseded && matches!(out[0].result, Ok(None)), "the queued one is skipped");
        assert!(!out[1].superseded);
    }

    #[test]
    fn a_click_request_for_another_selection_is_not_skipped() {
        // another component is another selection: its result is applied to that component
        let photo = lightcraft_catalog::PhotoId(1);
        let fake = Fake::new(vec![]);
        let jobs = vec![(Kind::Clicks(vec![]), clicks_tag(photo, 1, 0)), (Kind::Clicks(vec![]), clicks_tag(photo, 1, 1))];
        let out = run_tagged(Placement::Cpu, jobs, &fake);
        assert_eq!(fake.calls().len(), 2);
        assert!(out.iter().all(|o| !o.superseded));
    }

    #[test]
    fn a_later_detail_pass_skips_a_queued_pass_for_the_same_selection() {
        let photo = lightcraft_catalog::PhotoId(1);
        let fake = Fake::new(vec![]);
        let jobs = vec![(detail_kind(), detail_tag(photo, 2, 0)), (detail_kind(), detail_tag(photo, 2, 0))];
        let out = run_tagged(Placement::Cpu, jobs, &fake);
        assert_eq!(fake.calls().len(), 1, "the queued pass is skipped");
        assert!(out[0].superseded && !out[1].superseded);
    }

    #[test]
    fn a_detail_pass_for_another_component_is_not_skipped() {
        let photo = lightcraft_catalog::PhotoId(1);
        let fake = Fake::new(vec![]);
        let jobs = vec![(detail_kind(), detail_tag(photo, 2, 0)), (detail_kind(), detail_tag(photo, 2, 1))];
        let out = run_tagged(Placement::Cpu, jobs, &fake);
        assert_eq!(fake.calls().len(), 2);
        assert!(out.iter().all(|o| !o.superseded));
    }

    #[test]
    fn a_queued_prepare_for_the_same_photo_is_skipped_and_one_for_another_photo_is_not() {
        let (tx, rx) = mpsc::channel();
        let (jtx, jrx) = mpsc::channel();
        let shared = Shared::default();
        shared.pending.store(3, Ordering::SeqCst);
        for key in [1, 1, 2] {
            let mut j = job(Tag::Prepare, &tx);
            j.key = key;
            jtx.send(j).unwrap();
        }
        drop(jtx);
        let fake = Fake::default();
        run(&jrx, &shared, Duration::from_secs(60), Placement::Cpu, &fake);
        let out: Vec<Outcome> = rx.try_iter().collect();
        assert_eq!(out.len(), 3);
        assert!(out[0].superseded, "a later Prepare for the same photo covers it");
        assert!(!out[1].superseded && !out[2].superseded, "another photo is another request");
        assert_eq!(fake.calls().len(), 2);
        assert_eq!(shared.pending.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn two_identical_describe_requests_both_reach_the_model() {
        // mask.add creates one mask per request and mask.addComponent adds one component per
        // request (the same text and op twice included): two identical Describe requests are two
        // intentional selections, so neither may be skipped
        let prompt = || Tag::NewPrompt { photo: lightcraft_catalog::PhotoId(7), mask: Some(3), op: "add".into(), name: None, text: "hair".into() };
        let fake = Fake::new(vec![Ok(None), Ok(None)]);
        let jobs = vec![(Kind::Text("hair".into()), prompt()), (Kind::Text("hair".into()), prompt())];
        let out = run_tagged(Placement::Cpu, jobs, &fake);
        assert_eq!(fake.calls().len(), 2, "both requests run");
        assert!(out.iter().all(|o| !o.superseded));
    }

    #[test]
    fn a_queued_describe_request_is_not_skipped_by_a_later_click_or_detail_request() {
        // an inactive photo is not a reason to drop a queued request either: the session checks the
        // active photo when the result arrives, not when the request was queued
        let photo = lightcraft_catalog::PhotoId(7);
        let prompt = Tag::NewPrompt { photo, mask: None, op: "add".into(), name: None, text: "dog".into() };
        let fake = Fake::new(vec![Ok(None), Ok(None), Ok(None)]);
        let jobs =
            vec![(Kind::Text("dog".into()), prompt), (Kind::Clicks(vec![]), clicks_tag(photo, 1, 0)), (detail_kind(), detail_tag(photo, 1, 0))];
        let out = run_tagged(Placement::Cpu, jobs, &fake);
        assert_eq!(fake.calls().len(), 3, "a Describe is never redundant with a click or detail request");
        assert!(out.iter().all(|o| !o.superseded));
    }

    #[test]
    fn redundant_with_classifies_only_results_the_session_discards() {
        let (tx, _rx) = mpsc::channel();
        let mut first = job(Tag::Prepare, &tx);
        let same = job(Tag::Prepare, &tx);
        let mut other = job(Tag::Prepare, &tx);
        other.key = 2;
        assert_eq!(redundant_with(&same, &first), Some(Redundant::Prepare), "same photo");
        assert_eq!(redundant_with(&other, &first), None, "another photo");
        let photo = lightcraft_catalog::PhotoId(1);
        // a clicks request for that photo, mask 1, component 0: the tag is what the rule compares
        first.kind = Kind::Clicks(vec![]);
        first.tag = clicks_tag(photo, 1, 0);
        let mut later = job(clicks_tag(photo, 1, 0), &tx);
        assert_eq!(redundant_with(&later, &first), Some(Redundant::Clicks), "the same selection");
        later.tag = detail_tag(photo, 1, 0);
        assert_eq!(redundant_with(&later, &first), None, "a detail pass does not supersede a click request");
        let prompt = job(Tag::NewPrompt { photo, mask: None, op: "add".into(), name: None, text: "dog".into() }, &tx);
        assert_eq!(redundant_with(&prompt, &first), None, "a Describe is never superseded");
        assert_eq!(redundant_with(&prompt, &prompt), None, "not even by an identical one");
    }
}
