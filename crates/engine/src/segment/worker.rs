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
//! Where the model runs ([`Placement`]): on macOS candle runs all of it on the GPU through Metal.
//! Elsewhere candle runs on the CPU, and the backbone (most of the work) on the GPU with
//! `lightcraft-gpu`'s wgpu kernels, unless the GPU is switched off (`LIGHTCRAFT_GPU=0`,
//! `LIGHTCRAFT_GPU_BACKEND=off`, or Settings ▸ Performance ▸ Use the GPU for rendering, as for AI
//! Denoise and faces); a GPU that can't be used or fails on a photo leaves the backbone to the CPU.
//! Settings ▸ Performance ▸ AI masks ▸ CPU ([`Job::cpu`]) puts all of it on the CPU, on every
//! platform.
//! [`Backend`] tells the session where it ran and how long the last photo took.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lightcraft_develop::SegMask;
use lightcraft_segment::{
    BackboneAccel, Device, Encoded, Sam3, WeightReader,
    remote::{self, Operation, Request, text_logits},
};

use super::{Input, Prompt, Tag};
use crate::media::RenderJob;

/// Unload the model after this long without a request.
pub const IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);

/// What to compute.
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
}

/// One request.
pub struct Job {
    /// The model folder.
    pub dir: PathBuf,
    /// The user chose the CPU for SAM 3 (Settings ▸ Performance ▸ AI masks).
    pub cpu: bool,
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
    /// Where the model runs and how long the last photo took.
    pub backend: Mutex<Backend>,
}

/// Where the model runs, and how long the last photo took (`segment.model.status` → `backend`,
/// Settings ▸ Performance ▸ AI masks).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Backend {
    /// What the backbone (the 32-layer ViT, most of the work) runs on: a GPU's name ("NVIDIA
    /// GeForce RTX 5070 (Vulkan)"), "Metal GPU" or "CPU". `None` until the model first loads.
    pub device: Option<String>,
    /// What runs the backbone: "wgpu" (LightCraft's GPU kernels) or "candle".
    pub runtime: Option<String>,
    /// What runs the rest (the necks and the click and text heads): "CPU" or "Metal GPU", with the parts the GPU
    /// accelerator runs ("CPU; the necks, the DETR encoder and the mask decoder on the GPU").
    pub heads: Option<String>,
    /// Why the backbone runs on the CPU where it could use the GPU: the GPU is switched off, there
    /// is no usable one, or it failed on a photo.
    pub fallback: Option<String>,
    /// The last photo's backbone run, in milliseconds.
    pub encode_ms: Option<u64>,
    /// The last photo's whole analysis before the first click (backbone and click features), in
    /// milliseconds.
    pub analyze_ms: Option<u64>,
}

/// Where the model runs on this machine.
#[derive(Clone, Debug, PartialEq)]
enum Placement {
    /// All of it in candle on its best device (Metal on macOS).
    Best,
    /// The backbone on the GPU (wgpu), the rest in candle on the CPU.
    GpuBackbone,
    /// All of it in candle on the CPU: the CPU is chosen, or the GPU is switched off (why).
    Cpu(String),
}

/// Where the model runs, for candle's best device being the CPU (`best_is_cpu`), the GPU switched
/// off (`switched_off`: why) or not, and the CPU chosen in Settings (`cpu`) or not. The choice wins
/// on every platform; the GPU switches only govern the wgpu backbone (macOS keeps Metal).
fn placement(best_is_cpu: bool, switched_off: Option<String>, cpu: bool) -> Placement {
    match (cpu, best_is_cpu, switched_off) {
        (true, _, _) => Placement::Cpu(super::CPU_CHOSEN.into()),
        (false, false, _) => Placement::Best,
        (false, true, None) => Placement::GpuBackbone,
        (false, true, Some(why)) => Placement::Cpu(why),
    }
}

/// Whether a model loaded for `loaded` runs somewhere else than `wanted` (it must load again).
/// Two reasons for the CPU are the same place.
fn moves(loaded: &Placement, wanted: &Placement) -> bool {
    std::mem::discriminant(loaded) != std::mem::discriminant(wanted)
}

/// Builds the GPU backbone from the checkpoint (`lightcraft_gpu::sam3::backbone`; tests use fakes).
type MakeAccel = fn(&WeightReader) -> Result<Box<dyn BackboneAccel>, String>;

/// Load the model in `dir` as `placement` says, on `best` (candle's best device).
fn load(dir: &Path, best: Device, placement: &Placement, make: MakeAccel) -> lightcraft_segment::Result<Sam3> {
    match placement {
        Placement::Best => Sam3::load_on(dir, best),
        Placement::Cpu(_) => Sam3::load_on(dir, Device::Cpu),
        Placement::GpuBackbone => Sam3::load_accelerated(dir, Device::Cpu, make),
    }
}

/// Note where the model `m`, just loaded for `placement`, runs.
fn note_loaded(shared: &Shared, m: &Sam3, placement: &Placement) {
    let mut b = shared.backend.lock().unwrap_or_else(PoisonError::into_inner);
    *b = Backend {
        device: Some(m.backbone_device()),
        runtime: Some(if m.accelerated() { "wgpu" } else { "candle" }.into()),
        heads: Some(m.heads_device()),
        fallback: match placement {
            Placement::Cpu(why) => Some(why.clone()),
            _ => m.backbone_fallback(),
        },
        encode_ms: None,
        analyze_ms: None,
    };
}

/// Note where the backbone ran a photo (it may have moved to the CPU on the way) and, when it
/// succeeded, how long it took (`encode`).
fn note_run(shared: &Shared, m: &Sam3, encode: Option<Duration>) {
    let mut b = shared.backend.lock().unwrap_or_else(PoisonError::into_inner);
    b.device = Some(m.backbone_device());
    b.runtime = Some(if m.accelerated() { "wgpu" } else { "candle" }.into());
    b.heads = Some(m.heads_device());
    if let Some(why) = m.backbone_fallback() {
        b.fallback = Some(why);
    }
    if let Some(d) = encode {
        b.encode_ms = Some(millis(d));
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
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

    /// Where the model runs (ran, once unloaded) and how long the last photo took.
    pub fn backend(&self) -> Backend {
        self.shared.backend.lock().unwrap_or_else(PoisonError::into_inner).clone()
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
        match std::thread::Builder::new().name("sam3".into()).spawn(move || run(&rx, &shared, IDLE_UNLOAD)) {
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
    /// Where the model was loaded to run, and whether candle's best device was the CPU then.
    placement: Option<(Placement, bool)>,
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

fn run(rx: &mpsc::Receiver<Job>, shared: &Shared, idle: Duration) {
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
            let _ = job.reply.send(Outcome { tag: job.tag, result: Ok(None), superseded: true });
            continue;
        }
        let result = match std::panic::catch_unwind(AssertUnwindSafe(|| run_job(&mut state, shared, &mut job))) {
            Ok(r) => r,
            Err(p) => {
                let why = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
                log::error!("SAM 3 panicked: {why}");
                // don't trust what the model kept: load it again next time
                state = State::default();
                Err("the AI model failed unexpectedly (it will be reloaded on the next try)".to_string())
            }
        };
        if result.is_err() && !is_detail {
            state.cache = None;
        }
        shared.loaded.store(state.model.is_some() || state.remote.is_some(), Ordering::SeqCst);
        drop(done);
        let _ = job.reply.send(Outcome { tag: job.tag, result, superseded: false });
    }
}

/// The model for `dir`, loading it when needed (and again when the GPU was switched on or off, or
/// the CPU chosen or not: `cpu`).
fn model<'a>(state: &'a mut State, shared: &Shared, dir: &Path, cpu: bool) -> Result<&'a mut Sam3, String> {
    let off = lightcraft_gpu::switched_off();
    let stale = state.placement.as_ref().is_some_and(|(p, best_is_cpu)| moves(p, &placement(*best_is_cpu, off.clone(), cpu)));
    if stale || state.model.as_ref().is_none_or(|(d, _)| d != dir) {
        state.model = None;
        state.placement = None;
        state.cache = None;
        let _busy = Flag::raise(&shared.encoding);
        let t = web_time::Instant::now();
        // with the CPU chosen candle's best device isn't asked for (on macOS that sets up Metal): it
        // counts as Metal on macOS and the CPU elsewhere when the choice changes back
        let best = if cpu { Device::Cpu } else { lightcraft_segment::best_device() };
        let best_is_cpu = if cpu { !cfg!(target_os = "macos") } else { best.is_cpu() };
        let placement = placement(best_is_cpu, off, cpu);
        let m = load(dir, best, &placement, lightcraft_gpu::sam3::backbone).map_err(|e| e.to_string())?;
        log::info!("SAM 3 loaded in {:?}: the backbone on the {}, the rest on the {}", t.elapsed(), m.backbone_device(), m.heads_device());
        note_loaded(shared, &m, &placement);
        state.model = Some((dir.to_path_buf(), m));
        state.placement = Some((placement, best_is_cpu));
        shared.loaded.store(true, Ordering::SeqCst);
    }
    state.model.as_mut().map(|(_, m)| m).ok_or_else(|| "the model did not load".to_string())
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
fn encoded<'a>(state: &'a mut State, shared: &Shared, job: &mut Job) -> Result<(&'a mut Sam3, &'a mut Encoded), String> {
    model(state, shared, &job.dir, job.cpu)?;
    if state.cache.as_ref().is_none_or(|(k, _)| *k != job.key) {
        state.cache = None;
        let input = render(job)?;
        let _busy = Flag::raise(&shared.encoding);
        let m = state.model.as_mut().map(|(_, m)| m).ok_or("the model did not load")?;
        let t = web_time::Instant::now();
        let enc = m.encode(&input.rgb, input.w, input.h);
        let encode = t.elapsed();
        note_run(shared, m, enc.is_ok().then_some(encode));
        let mut enc = enc.map_err(|e| e.to_string())?;
        m.prepare_clicks(&mut enc).map_err(|e| e.to_string())?;
        let analyze = t.elapsed();
        shared.backend.lock().unwrap_or_else(PoisonError::into_inner).analyze_ms = Some(millis(analyze));
        log::info!(
            "SAM 3 analyzed {}×{} in {analyze:?}: backbone {encode:?} on the {}, click features {:?} on the {}",
            input.w,
            input.h,
            m.backbone_device(),
            analyze.saturating_sub(encode),
            m.heads_device()
        );
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
        Kind::Panic => return Err("test panic request".into()),
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

fn run_job(state: &mut State, shared: &Shared, job: &mut Job) -> Result<Option<SegMask>, String> {
    if let Some(endpoint) = remote::configured() {
        let result = remote_job(state, shared, job, &endpoint);
        if result.is_err() {
            state.remote = None;
        }
        return result.map_err(|e| format!("Remote SAM 3 failed: {e}. Check the worker and SSH tunnel, then retry."));
    }
    let side = lightcraft_segment::MASK_SIDE;
    match std::mem::replace(&mut job.kind, Kind::Prepare) {
        Kind::Prepare => {
            encoded(state, shared, job)?;
            Ok(None)
        }
        Kind::Clicks(clicks) => {
            let (m, enc) = encoded(state, shared, job)?;
            let p = m.segment_clicks(enc, &clicks).map_err(|e| e.to_string())?;
            Ok(Some(SegMask::from_logits(side, &p.logits)))
        }
        Kind::Text(text) => {
            let (m, enc) = encoded(state, shared, job)?;
            Ok(text_logits(m, enc, &text)?.map(|l| SegMask::from_logits(side, &l)))
        }
        #[cfg(test)]
        #[allow(clippy::panic)]
        Kind::Panic => panic!("a test panic"),
        Kind::Detail { prompt, region } => {
            let input = render(job)?;
            let m = model(state, shared, &job.dir, job.cpu)?;
            let t = web_time::Instant::now();
            let enc = m.encode(&input.rgb, input.w, input.h);
            note_run(shared, m, enc.is_ok().then(|| t.elapsed()));
            let mut enc = enc.map_err(|e| e.to_string())?;
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
                    Some(m.segment_clicks(&mut enc, &inside).map_err(|e| e.to_string())?.logits)
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
        Job { dir, cpu: false, key: 1, render: None, missing: None, kind: Kind::Prepare, tag, reply: reply.clone() }
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
        run(&jrx, &shared, Duration::from_secs(60));
        let first = rx.recv().unwrap();
        assert!(first.result.unwrap_err().contains("failed unexpectedly"));
        let second = rx.recv().unwrap();
        assert!(second.result.is_err(), "the next job still runs");
        assert_eq!(shared.pending.load(Ordering::SeqCst), 0, "counters reset");
        assert!(!shared.encoding.load(Ordering::SeqCst));
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
        run(&jrx, &shared, Duration::from_secs(60));
        let out: Vec<Outcome> = rx.try_iter().collect();
        assert_eq!(out.len(), 3);
        assert!(out[0].superseded && out[1].superseded && !out[2].superseded);
        assert_eq!(shared.pending.load(Ordering::SeqCst), 0);
    }

    /// A model folder whose checkpoint holds no tensors: it loads with an accelerator, never in candle.
    #[allow(clippy::unwrap_used)]
    fn empty_model(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-sam3-worker-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = 2u64.to_le_bytes().to_vec();
        st.extend_from_slice(b"{}");
        std::fs::write(dir.join(lightcraft_segment::WEIGHTS_FILE), st).unwrap();
        std::fs::write(dir.join("vocab.json"), "{}").unwrap();
        std::fs::write(dir.join("merges.txt"), "").unwrap();
        dir
    }

    /// A GPU backbone that answers with zeros, or fails on every photo.
    struct FakeGpu(bool);

    impl BackboneAccel for FakeGpu {
        fn forward(&self, _: &[f32]) -> Result<Vec<f32>, String> {
            if self.0 { Err("device lost".into()) } else { Ok(vec![0.0; 1024 * 72 * 72]) }
        }
        fn device(&self) -> String {
            "Fake GPU (Vulkan)".into()
        }
    }

    fn fake_gpu(_: &WeightReader) -> Result<Box<dyn BackboneAccel>, String> {
        Ok(Box::new(FakeGpu(false)))
    }

    fn failing_gpu(_: &WeightReader) -> Result<Box<dyn BackboneAccel>, String> {
        Ok(Box::new(FakeGpu(true)))
    }

    fn no_gpu(_: &WeightReader) -> Result<Box<dyn BackboneAccel>, String> {
        Err("no GPU adapter".into())
    }

    #[allow(clippy::panic)]
    fn never(_: &WeightReader) -> Result<Box<dyn BackboneAccel>, String> {
        panic!("the GPU backbone was built")
    }

    #[allow(clippy::unwrap_used)]
    fn backend(shared: &Shared) -> Backend {
        shared.backend.lock().unwrap().clone()
    }

    #[test]
    fn the_backbone_goes_to_the_gpu_only_where_candle_has_none_and_the_gpu_is_on() {
        // macOS: candle runs everything on Metal, as before (the GPU switches are wgpu's)
        assert_eq!(placement(false, None, false), Placement::Best);
        assert_eq!(placement(false, Some("disabled by LIGHTCRAFT_GPU=0".into()), false), Placement::Best);
        // elsewhere: the backbone on the GPU unless it is switched off
        assert_eq!(placement(true, None, false), Placement::GpuBackbone);
        let off = "disabled by the GPU rendering preference (app.gpu)";
        assert_eq!(placement(true, Some(off.into()), false), Placement::Cpu(off.into()));
    }

    #[test]
    fn the_cpu_chosen_in_settings_wins_on_every_platform() {
        let chosen = Placement::Cpu(super::super::CPU_CHOSEN.into());
        // macOS (Metal) and elsewhere, the GPU on or off
        for best_is_cpu in [false, true] {
            for off in [None, Some("disabled by LIGHTCRAFT_GPU=0".to_string())] {
                assert_eq!(placement(best_is_cpu, off.clone(), true), chosen, "{best_is_cpu} {off:?}");
            }
        }
        assert_eq!(super::super::CPU_CHOSEN, "the CPU is chosen in Settings (AI masks)");
    }

    #[test]
    fn switching_the_gpu_or_the_choice_reloads_only_a_model_it_moves() {
        let cpu = |why: &str| Placement::Cpu(why.into());
        let chosen = || placement(true, None, true);
        // the GPU switch: Metal stays; the wgpu backbone goes to the CPU and back
        assert!(!moves(&Placement::Best, &placement(false, Some("off".into()), false)));
        assert!(moves(&Placement::GpuBackbone, &placement(true, Some("off".into()), false)));
        assert!(moves(&cpu("off"), &placement(true, None, false)));
        // the choice: Metal (macOS) or the GPU backbone to the CPU and back
        assert!(moves(&Placement::Best, &placement(false, None, true)));
        assert!(moves(&placement(false, None, true), &placement(false, None, false)));
        assert!(moves(&Placement::GpuBackbone, &chosen()));
        // the CPU for one reason or the other: it stays loaded
        assert!(!moves(&cpu("disabled by LIGHTCRAFT_GPU=0"), &chosen()));
        assert!(!moves(&chosen(), &cpu("disabled by LIGHTCRAFT_GPU=0")));
        assert!(!moves(&Placement::GpuBackbone, &placement(true, None, false)));
    }

    #[test]
    fn with_the_cpu_chosen_the_gpu_backbone_is_never_built() {
        let dir = empty_model("chosen");
        // candle loads on the CPU (and fails: the checkpoint is empty), without touching the GPU
        assert!(load(&dir, Device::Cpu, &placement(true, None, true), never).is_err());
        assert!(load(&dir, Device::Cpu, &placement(false, None, true), never).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_gpu_backbone_loads_and_reports_where_it_runs() {
        let dir = empty_model("gpu");
        let m = load(&dir, Device::Cpu, &Placement::GpuBackbone, fake_gpu).unwrap();
        let shared = Shared::default();
        note_loaded(&shared, &m, &Placement::GpuBackbone);
        let b = backend(&shared);
        assert_eq!(
            (b.device.as_deref(), b.runtime.as_deref(), b.heads.as_deref(), b.fallback.as_deref()),
            (Some("Fake GPU (Vulkan)"), Some("wgpu"), Some("CPU"), None)
        );
        let enc = m.encode(&[0; 8 * 8 * 3], 8, 8);
        note_run(&shared, &m, enc.is_ok().then_some(Duration::from_millis(812)));
        assert_eq!(backend(&shared).encode_ms, Some(812));
        let json = serde_json::to_value(backend(&shared)).unwrap();
        assert_eq!(json["device"], "Fake GPU (Vulkan)");
        assert_eq!(json["encodeMs"], 812);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn with_the_gpu_switched_off_the_gpu_backbone_is_never_built() {
        let dir = empty_model("off");
        let off = Placement::Cpu("disabled by LIGHTCRAFT_GPU_BACKEND=off".into());
        // candle loads (and fails: the checkpoint is empty), without touching the GPU
        assert!(load(&dir, Device::Cpu, &off, never).is_err());
        assert!(load(&dir, Device::Cpu, &Placement::Best, never).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_gpu_that_cannot_be_used_leaves_the_backbone_to_the_cpu() {
        let dir = empty_model("nogpu");
        // without an adapter candle loads the backbone (failing here only for the empty checkpoint)
        let e = load(&dir, Device::Cpu, &Placement::GpuBackbone, no_gpu).err().unwrap();
        assert!(!e.to_string().contains("no GPU adapter"), "{e}");
        // a GPU that fails on a photo: the CPU takes over, and the status says why
        let m = load(&dir, Device::Cpu, &Placement::GpuBackbone, failing_gpu).unwrap();
        let shared = Shared::default();
        note_loaded(&shared, &m, &Placement::GpuBackbone);
        assert_eq!(backend(&shared).runtime.as_deref(), Some("wgpu"));
        let enc = m.encode(&[0; 8 * 8 * 3], 8, 8);
        note_run(&shared, &m, enc.is_ok().then_some(Duration::from_millis(1)));
        let b = backend(&shared);
        assert_eq!((b.device.as_deref(), b.runtime.as_deref()), (Some("CPU"), Some("candle")));
        assert!(b.fallback.as_deref().is_some_and(|f| f.contains("device lost")), "{b:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
