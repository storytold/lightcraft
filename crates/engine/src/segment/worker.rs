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

/// The model and the last encoded photo.
#[derive(Default)]
struct State {
    model: Option<(PathBuf, Sam3)>,
    cache: Option<(u64, Encoded)>,
    remote: Option<remote::Client>,
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
                    if state.model.is_some() || state.remote.is_some() {
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
        let Some(mut job) = queue.pop_front() else { continue };
        let is_detail = matches!(job.kind, Kind::Detail { .. });
        // counts the job as pending until just before its reply goes out: a caller woken by the
        // reply must not still see the worker busy with it
        let done = Done(if is_detail { &shared.detail } else { &shared.pending });
        if queue.iter().any(|later| supersedes(later, &job)) {
            drop(done);
            let _ = job.reply.send(Outcome { tag: job.tag, result: Ok(None), superseded: true, notice: None });
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
            state.cache = None;
        }
        shared.loaded.store(state.model.is_some() || state.remote.is_some(), Ordering::SeqCst);
        drop(done);
        let _ = job.reply.send(Outcome { tag: job.tag, result, superseded: false, notice });
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
