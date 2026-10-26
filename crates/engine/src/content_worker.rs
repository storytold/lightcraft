//! The background part of indexing photo contents: one thread that owns the content model's image tower and embeds
//! the photos it is handed, a batch at a time, so the session thread (the window) never waits for the model.
//!
//! For each photo of a batch it takes the unedited picture, upright and uncropped: a raw's embedded JPEG when its long
//! edge is at least [`MIN_PREVIEW_EDGE`] (no raw decode), else the photo rendered with default settings at
//! [`EDGE`] pixels. The pictures are read in parallel, then go through the model in one forward pass. All of it runs on
//! a thread pool sized by the pace ([`Pace`]), so a Normal scan leaves the rest of the machine to the user.
//!
//! The session side ([`State`], driven by `content.pump`) prepares the batches (which needs the catalog), takes the
//! embeddings into the index ([`crate::content_index`]) and keeps the row in the activity stack.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use lightcraft_catalog::PhotoId;
use lightcraft_vision::{ContentModel, Manifest, Rgb8, Towers};

use crate::content_index::{Hash, Index};
use crate::media::{PreviewLoader, RenderJob};

/// Long edge of the picture a photo is embedded from (the model squashes it to 256 px; a little more keeps the resize
/// honest).
pub(crate) const EDGE: usize = 512;
/// A raw's embedded preview is used when its long edge is at least this.
pub(crate) const MIN_PREVIEW_EDGE: usize = 256;
/// Embedded previews read at once, by pace (Low, Normal, Fast): each reads its raw file (a 50 MP raw is ~60 MB) and
/// decodes the preview.
pub(crate) const PREVIEW_READS: [usize; 3] = [2, 4, 8];
/// Photos rendered from their file at once, by pace (no usable preview: a JPEG, a TIFF, a raw without one). A render
/// holds its file and a decode: a few big files at once were the 2–5 GB peaks of a scan before big TIFFs were decoded
/// reduced. One at a time left a model on the GPU waiting for them (352 mixed photos: 26 s with one, 10 s with four,
/// the same peak memory).
pub(crate) const RENDERS: [usize; 3] = [1, 3, 6];

/// A counting gate: at most `max` holders at once (the others wait), and the most there ever were.
pub(crate) struct Gate {
    max: usize,
    /// Holders now, and the most at once.
    state: Mutex<(usize, usize)>,
    freed: Condvar,
}

/// Held while inside a [`Gate`].
pub(crate) struct Pass<'a>(&'a Gate);

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        let mut st = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.0 = st.0.saturating_sub(1);
        self.0.freed.notify_one();
    }
}

impl Gate {
    pub const fn new(max: usize) -> Gate {
        Gate { max, state: Mutex::new((0, 0)), freed: Condvar::new() }
    }

    /// Wait for a place, and hold it until the pass is dropped.
    pub fn enter(&self) -> Pass<'_> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while st.0 >= self.max.max(1) {
            st = self.freed.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
        st.0 += 1;
        st.1 = st.1.max(st.0);
        Pass(self)
    }

    /// The most holders there were at once.
    #[cfg(test)]
    pub fn peak(&self) -> usize {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1
    }
}

/// How many pictures are read at once, by kind (shared by every scan of the process).
pub(crate) struct Gates {
    pub previews: Gate,
    pub renders: Gate,
}

static LOW_GATES: Gates = Gates { previews: Gate::new(PREVIEW_READS[0]), renders: Gate::new(RENDERS[0]) };
static NORMAL_GATES: Gates = Gates { previews: Gate::new(PREVIEW_READS[1]), renders: Gate::new(RENDERS[1]) };
static FAST_GATES: Gates = Gates { previews: Gate::new(PREVIEW_READS[2]), renders: Gate::new(RENDERS[2]) };

/// How hard indexing may work: the user's choice (Settings ▸ Search, `content.pace`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Pace {
    /// A few threads, small batches: barely noticeable.
    Low,
    /// Half the machine.
    #[default]
    Normal,
    /// Most of the machine.
    Fast,
}

impl Pace {
    /// `low`, `normal` or `fast`; anything else is `None`.
    pub fn parse(name: &str) -> Option<Pace> {
        match name {
            "low" => Some(Pace::Low),
            "normal" => Some(Pace::Normal),
            "fast" => Some(Pace::Fast),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Pace::Low => "low",
            Pace::Normal => "normal",
            Pace::Fast => "fast",
        }
    }

    /// Threads the model and the decodes use at this pace on a machine with `cores` threads.
    pub fn threads(self, cores: usize) -> usize {
        match self {
            Pace::Low => (cores / 4).clamp(1, 4),
            Pace::Normal => (cores / 2).max(1),
            Pace::Fast => (cores * 4 / 5).max(1),
        }
    }

    fn index(self) -> usize {
        match self {
            Pace::Low => 0,
            Pace::Normal => 1,
            Pace::Fast => 2,
        }
    }

    /// How many pictures a scan at this pace reads at once (shared by every scan of the process).
    pub(crate) fn gates(self) -> &'static Gates {
        match self {
            Pace::Low => &LOW_GATES,
            Pace::Normal => &NORMAL_GATES,
            Pace::Fast => &FAST_GATES,
        }
    }

    /// Photos per forward pass: larger batches are a little faster per photo, smaller ones stop sooner when cancelled.
    pub fn batch(self) -> usize {
        match self {
            Pace::Low => 8,
            Pace::Normal => 16,
            Pace::Fast => 32,
        }
    }
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// The thread pool for `pace`: made when first needed, its idle threads cost nothing. The model's matrix products and
/// the decodes run inside it (rayon's global pool serves the interactive work, which must not wait behind a scan).
pub(crate) fn pool(pace: Pace) -> Option<&'static rayon::ThreadPool> {
    static POOLS: [OnceLock<Option<rayon::ThreadPool>>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let i = pace.index();
    POOLS
        .get(i)?
        .get_or_init(|| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(pace.threads(cores()))
                .thread_name(move |t| format!("lightcraft-content-{i}-{t}"))
                .build()
                .ok()
        })
        .as_ref()
}

/// Loads a content model from its folder (the real one: [`lightcraft_vision::load`]; tests use a stand-in).
pub(crate) type Loader = Arc<dyn Fn(&Path, &Manifest, Towers) -> Result<Box<dyn ContentModel>, String> + Send + Sync>;

/// The real loader: each tower on the GPU when one can run it (wgpu compute: `lightcraft_gpu::transformer`), else on
/// the CPU; a GPU failure later moves the model to the CPU for good ([`ContentModel::gpu_fallback`]).
pub(crate) fn default_loader() -> Loader {
    Arc::new(|dir: &Path, m: &Manifest, towers: Towers| load_accelerated(dir, m, towers))
}

/// [`lightcraft_vision::load`] with the GPU runner offered.
pub(crate) fn load_accelerated(dir: &Path, m: &Manifest, towers: Towers) -> Result<Box<dyn ContentModel>, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let accel = lightcraft_gpu::transformer::accelerator();
        lightcraft_vision::load_with(dir, m, towers, Some(&*accel)).map_err(|e| e.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        lightcraft_vision::load(dir, m, towers).map_err(|e| e.to_string())
    }
}

/// One photo to embed.
pub(crate) struct Item {
    pub id: PhotoId,
    /// Its content hash when it was prepared.
    pub hash: Hash,
    /// A raw's embedded preview (tried first).
    pub preview: Option<(String, PreviewLoader)>,
    /// The photo rendered with default settings ([`render`] of its job; tests bring their own).
    pub job: Option<Render>,
    /// Its original, to tell an offline photo from an unreadable one.
    pub path: Option<String>,
}

/// Renders one photo's picture (display sRGB).
pub(crate) type Render = Box<dyn FnOnce() -> Result<lightcraft_raster::Rgba8, String> + Send>;

/// A render job as a [`Render`], run on the CPU ([`RenderJob::run_on_cpu`]: the scan leaves the GPU to the views).
pub(crate) fn render(job: RenderJob) -> Render {
    Box::new(move || match job.run_on_cpu().rendered {
        Ok(r) if r.image.width > 0 && r.image.height > 0 => Ok(r.image),
        Ok(_) => Err("it rendered empty".into()),
        Err(e) => Err(e),
    })
}

pub(crate) struct Batch {
    pub items: Vec<Item>,
    pub pace: Pace,
}

/// What became of one photo.
#[derive(Debug)]
pub(crate) enum Outcome {
    Embedded(Vec<f32>),
    /// Its file is not there (a drive that is not connected): tried again later.
    Offline,
    /// It could not be read or embedded: not tried again this session (unless its file changes).
    Failed(String),
    /// Not looked at (the scan was cancelled first).
    Skipped,
}

pub(crate) struct Done {
    pub results: Vec<(PhotoId, Hash, Outcome)>,
    /// Wall time of the batch, and how many photos it embedded.
    pub ms: f64,
    pub embedded: usize,
    /// The model could not be loaded: nothing will work until it changes.
    pub model_error: Option<String>,
    /// What ran the model ([`ContentModel::runs_on`]), and why the GPU stopped running it, if it did.
    pub device: Option<String>,
    pub gpu_fallback: Option<String>,
}

fn to_rgb(img: &lightcraft_raster::Rgba8) -> Rgb8 {
    Rgb8 { width: img.width, height: img.height, data: img.data.iter().map(|p| [p[0], p[1], p[2]]).collect() }
}

/// The picture of one photo, or why there is none. At most [`PREVIEW_READS`] previews and [`RENDERS`] renders
/// run at once (`gates`), whatever the size of the pool. Once `gone` is set (the session let go of the worker: the
/// model or the library changed, indexing stopped) nothing more is read: the photo comes back skipped.
/// The reads and renders run in `pool` (the gate is taken before, on the calling thread: see [`pictures`]).
fn picture(item: Item, gates: &Gates, gone: &AtomicBool, pool: Option<&rayon::ThreadPool>) -> (PhotoId, Hash, Result<Rgb8, Outcome>) {
    let Item { id, hash, preview, job, path } = item;
    let unwanted = || gone.load(Ordering::SeqCst);
    let inside = |f: &mut (dyn FnMut() -> Option<lightcraft_raster::Rgba8> + Send)| match pool {
        Some(p) => p.install(f),
        None => f(),
    };
    let run = catch_unwind(AssertUnwindSafe(|| {
        if unwanted() {
            return Ok(None);
        }
        let preview = preview.and_then(|(p, load)| {
            let _pass = gates.previews.enter();
            if unwanted() { None } else { inside(&mut || load(&p, EDGE)) }
        });
        if let Some(img) = preview.filter(|img| img.width.max(img.height) >= MIN_PREVIEW_EDGE) {
            return Ok(Some(to_rgb(&img)));
        }
        let Some(job) = job else { return Err("nothing to render it from".to_string()) };
        // (the gates are shared by every scan of the process: the wait for one can be long)
        let _pass = gates.renders.enter();
        if unwanted() {
            return Ok(None);
        }
        let mut job = Some(job);
        let mut rendered = None;
        inside(&mut || {
            rendered = job.take().map(|j| j());
            None
        });
        rendered.unwrap_or_else(|| Err("it was not rendered".into())).map(|img| Some(to_rgb(&img)))
    }));
    let r = match run {
        Ok(Ok(Some(img))) => Ok(img),
        Ok(Ok(None)) => return (id, hash, Err(Outcome::Skipped)),
        Ok(Err(why)) => Err(why),
        Err(_) => Err("reading it stopped unexpectedly".into()),
    };
    (
        id,
        hash,
        r.map_err(|why| match path {
            Some(p) if !Path::new(&p).exists() => Outcome::Offline,
            _ => Outcome::Failed(why.chars().take(200).collect()),
        }),
    )
}

/// The pictures of a batch, read a few at a time (the gates' sizes) by threads of their own, which wait for the gates;
/// the reads and renders themselves run in `pool`. A rayon worker must never wait for a gate: a render's parallel
/// loops let the worker running it take on another photo's task meanwhile, which would then wait for the gate that
/// very worker holds (the scan hung once its renders ran on the CPU).
fn pictures(items: Vec<Item>, gates: &Gates, gone: &AtomicBool, pool: Option<&rayon::ThreadPool>) -> Vec<(PhotoId, Hash, Result<Rgb8, Outcome>)> {
    let n = items.len();
    let queue = Mutex::new(items.into_iter().enumerate().collect::<std::collections::VecDeque<_>>());
    let done = Mutex::new(Vec::with_capacity(n));
    let work = || {
        loop {
            let next = queue.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
            let Some((i, item)) = next else { break };
            let r = picture(item, gates, gone, pool);
            done.lock().unwrap_or_else(PoisonError::into_inner).push((i, r));
        }
    };
    let threads = (gates.previews.max + gates.renders.max).clamp(1, n.max(1));
    std::thread::scope(|sc| {
        for t in 0..threads {
            // (a thread that cannot be started leaves its share to the others, or to this one below)
            let _ = std::thread::Builder::new().name(format!("lightcraft-content-read-{t}")).spawn_scoped(sc, work);
        }
    });
    work();
    let mut done = done.into_inner().unwrap_or_else(PoisonError::into_inner);
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// Embed one batch with `model`: the pictures read ([`pictures`]), then the model run in `pool` (the pace's). Once
/// `gone` is set the rest of the batch is skipped: a worker the session dropped (another model, another library) stops
/// after the photo it is reading, and never runs the model again.
fn embed(
    model: &dyn ContentModel,
    items: Vec<Item>,
    gates: &Gates,
    gone: &AtomicBool,
    pool: Option<&rayon::ThreadPool>,
) -> Vec<(PhotoId, Hash, Outcome)> {
    let pictures = pictures(items, gates, gone, pool);
    match pool {
        Some(p) => p.install(|| embed_pictures(model, pictures, gone)),
        None => embed_pictures(model, pictures, gone),
    }
}

fn embed_pictures(
    model: &dyn ContentModel,
    pictures: Vec<(PhotoId, Hash, Result<Rgb8, Outcome>)>,
    gone: &AtomicBool,
) -> Vec<(PhotoId, Hash, Outcome)> {
    let (mut keys, mut images, mut out) = (Vec::new(), Vec::new(), Vec::new());
    for (id, hash, r) in pictures {
        match r {
            Ok(img) => {
                keys.push((id, hash));
                images.push(img);
            }
            Err(o) => out.push((id, hash, o)),
        }
    }
    if gone.load(Ordering::SeqCst) {
        out.extend(keys.into_iter().map(|(id, hash)| (id, hash, Outcome::Skipped)));
        return out;
    }
    for (keys, images) in keys.chunks(lightcraft_vision::MAX_BATCH).zip(images.chunks(lightcraft_vision::MAX_BATCH)) {
        let run = catch_unwind(AssertUnwindSafe(|| model.embed_images(images)));
        match run {
            Ok(Ok(vectors)) if vectors.len() == keys.len() => {
                out.extend(keys.iter().zip(vectors).map(|((id, hash), v)| (*id, *hash, Outcome::Embedded(v))));
            }
            _ => {
                // one bad picture fails the whole pass: find it by doing them one at a time
                for ((id, hash), img) in keys.iter().zip(images) {
                    let one = catch_unwind(AssertUnwindSafe(|| model.embed_images(std::slice::from_ref(img))));
                    let outcome = match one {
                        Ok(Ok(v)) if v.len() == 1 => v.into_iter().next().map_or(Outcome::Failed("the model made nothing".into()), Outcome::Embedded),
                        Ok(Err(e)) => Outcome::Failed(e.to_string().chars().take(200).collect()),
                        _ => Outcome::Failed("the model could not embed it".into()),
                    };
                    out.push((*id, *hash, outcome));
                }
            }
        }
    }
    out
}

/// The thread that owns the model.
pub(crate) struct Worker {
    jobs: Sender<Batch>,
    done: Receiver<Done>,
    /// Batches handed over and not yet back.
    pub busy: usize,
    /// Set when the session lets go of the worker: batches still queued are not started.
    dropped: Arc<AtomicBool>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl Worker {
    /// Start the thread for the model in `home` described by `manifest`. The model is loaded on that thread, with the
    /// first batch. A batch handed over after `cancel` is set comes back untouched.
    pub fn start(home: PathBuf, manifest: Manifest, loader: Loader, cancel: Arc<AtomicBool>) -> Result<Worker, String> {
        let (jobs, job_rx) = channel::<Batch>();
        let (done_tx, done) = channel::<Done>();
        let dropped = Arc::new(AtomicBool::new(false));
        let gone = dropped.clone();
        std::thread::Builder::new()
            .name("lightcraft-content-index".into())
            .spawn(move || {
                let mut model: Option<Box<dyn ContentModel>> = None;
                let mut model_error: Option<String> = None;
                // ends when the session drops its sender (indexing stopped, the model or the library changed)
                while let Ok(batch) = job_rx.recv() {
                    // indexing stopped, or the model or the library changed: what is still queued is not wanted
                    if gone.load(Ordering::SeqCst) {
                        break;
                    }
                    let started = Instant::now();
                    let skip = |items: Vec<Item>| items.into_iter().map(|i| (i.id, i.hash, Outcome::Skipped)).collect::<Vec<_>>();
                    if cancel.load(Ordering::SeqCst) {
                        let _ = done_tx.send(Done {
                            results: skip(batch.items),
                            ms: 0.0,
                            embedded: 0,
                            model_error: None,
                            device: None,
                            gpu_fallback: None,
                        });
                        continue;
                    }
                    if model.is_none() && model_error.is_none() {
                        let loaded = catch_unwind(AssertUnwindSafe(|| loader(&home, &manifest, Towers { image: true, text: false })));
                        match loaded {
                            Ok(Ok(m)) => model = Some(m),
                            Ok(Err(e)) => model_error = Some(e),
                            Err(_) => model_error = Some("loading the model stopped unexpectedly".into()),
                        }
                    }
                    let Some(m) = model.as_deref() else {
                        let _ = done_tx.send(Done {
                            results: skip(batch.items),
                            ms: 0.0,
                            embedded: 0,
                            model_error: model_error.clone(),
                            device: None,
                            gpu_fallback: None,
                        });
                        continue;
                    };
                    // (on this thread: the reads wait for their gates outside the pool, see `pictures`)
                    let results = embed(m, batch.items, batch.pace.gates(), &gone, pool(batch.pace));
                    let embedded = results.iter().filter(|r| matches!(r.2, Outcome::Embedded(_))).count();
                    let ms = started.elapsed().as_secs_f64() * 1e3;
                    let (device, gpu_fallback) = (Some(m.runs_on()), m.gpu_fallback());
                    if done_tx.send(Done { results, ms, embedded, model_error: None, device, gpu_fallback }).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("could not start indexing: {e}"))?;
        Ok(Worker { jobs, done, busy: 0, dropped })
    }

    pub fn submit(&mut self, batch: Batch) -> bool {
        let ok = self.jobs.send(batch).is_ok();
        if ok {
            self.busy += 1;
        }
        ok
    }

    /// Batches finished since the last call.
    pub fn finished(&mut self) -> Vec<Done> {
        let out: Vec<Done> = self.done.try_iter().collect();
        self.busy = self.busy.saturating_sub(out.len());
        out
    }
}

/// The model in use, as the session last looked it up.
#[derive(Clone)]
pub(crate) struct Current {
    pub id: String,
    /// `<model id>@<sha256 of the weights>`.
    pub tag: String,
    pub home: PathBuf,
    pub manifest: Manifest,
}

/// What the text thread is asked.
enum TextJob {
    /// One search text.
    Query(String),
    /// Many texts (the auto-tag vocabulary's sentences), embedded [`VOCAB_CHUNK`] at a time with searches served in
    /// between, until done or `cancel` is set.
    Vocab { id: u64, texts: Vec<String>, cancel: Arc<AtomicBool> },
}

/// What the text thread sends back.
enum TextMsg {
    Loaded(Arc<dyn ContentModel>),
    LoadFailed(String),
    Embedded(String, Result<Vec<f32>, String>),
    /// Embeddings of a vocabulary job's texts `start..start + n`.
    VocabPart {
        id: u64,
        start: usize,
        vectors: Result<Vec<Vec<f32>>, String>,
    },
}

/// Recent search texts whose embeddings are kept.
const QUERY_CACHE: usize = 64;
/// Texts of a vocabulary job embedded in one forward pass (a search typed meanwhile waits at most one of these).
pub(crate) const VOCAB_CHUNK: usize = 16;

/// The text tower (for searches and the auto-tag vocabulary), loaded and run on a thread of its own so the window never
/// waits for it: loading takes about half a second, embedding a text a few tens of milliseconds. Searches typed while
/// it loads are embedded once it is ready; a search typed while the vocabulary is being embedded goes first.
pub(crate) struct TextWorker {
    /// `<model id>@<sha256>` of the model it loads.
    pub tag: String,
    jobs: Sender<TextJob>,
    done: Receiver<TextMsg>,
    /// The model, once loaded (scores are computed with it on the session thread: [`ContentModel::score_cosine`]).
    pub model: Option<Arc<dyn ContentModel>>,
    /// It could not be loaded.
    pub error: Option<String>,
    /// Texts handed over and not yet back.
    pending: HashSet<String>,
    /// Embeddings of recent texts, or why a text could not be embedded.
    queries: HashMap<String, Result<Vec<f32>, String>>,
    /// Parts of vocabulary jobs that came back: (job, first text, embeddings).
    pub vocab_parts: Vec<(u64, usize, Result<Vec<Vec<f32>>, String>)>,
    pub used: Instant,
}

/// Where a text's embedding is.
pub(crate) enum TextVector {
    Ready(Vec<f32>),
    /// Being loaded or embedded: ask again.
    Pending,
    Failed(String),
}

/// Embed `texts` with `model`, one way of failing.
fn embed_with(model: &Result<Arc<dyn ContentModel>, String>, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
    match model {
        Ok(m) => match catch_unwind(AssertUnwindSafe(|| m.embed_texts(texts))) {
            Ok(Ok(v)) if v.len() == texts.len() => Ok(v),
            Ok(Ok(_)) => Err("the model made no embedding for the text".to_string()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("the model stopped unexpectedly".to_string()),
        },
        Err(e) => Err(e.clone()),
    }
}

impl TextWorker {
    /// Start the thread for the model in `home`: it loads the text tower right away.
    pub fn start(tag: String, home: PathBuf, manifest: Manifest, loader: Loader) -> Result<TextWorker, String> {
        let (jobs, job_rx) = channel::<TextJob>();
        let (done_tx, done) = channel::<TextMsg>();
        std::thread::Builder::new()
            .name("lightcraft-content-text".into())
            .spawn(move || {
                let loaded = catch_unwind(AssertUnwindSafe(|| loader(&home, &manifest, Towers { image: false, text: true })));
                let model: Result<Arc<dyn ContentModel>, String> = match loaded {
                    Ok(Ok(m)) => Ok(Arc::from(m)),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err("loading the model stopped unexpectedly".into()),
                };
                let sent = match &model {
                    Ok(m) => done_tx.send(TextMsg::Loaded(m.clone())),
                    Err(e) => done_tx.send(TextMsg::LoadFailed(e.clone())),
                };
                if sent.is_err() {
                    return;
                }
                let mut queries: std::collections::VecDeque<String> = Default::default();
                // (job, texts, next text, cancel)
                let mut vocab: std::collections::VecDeque<(u64, Vec<String>, usize, Arc<AtomicBool>)> = Default::default();
                let take = |job: TextJob, queries: &mut std::collections::VecDeque<String>, vocab: &mut std::collections::VecDeque<_>| match job {
                    TextJob::Query(t) => queries.push_back(t),
                    TextJob::Vocab { id, texts, cancel } => vocab.push_back((id, texts, 0usize, cancel)),
                };
                // ends when the session drops its sender (the model changed, or nothing asked for a while)
                loop {
                    if queries.is_empty() && vocab.is_empty() {
                        match job_rx.recv() {
                            Ok(job) => take(job, &mut queries, &mut vocab),
                            Err(_) => break,
                        }
                    }
                    loop {
                        match job_rx.try_recv() {
                            Ok(job) => take(job, &mut queries, &mut vocab),
                            Err(std::sync::mpsc::TryRecvError::Empty) => break,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                        }
                    }
                    // searches first: someone is waiting for them
                    if let Some(text) = queries.pop_front() {
                        let v = embed_with(&model, &[text.as_str()]).and_then(|v| v.into_iter().next().ok_or_else(|| "no embedding".to_string()));
                        if done_tx.send(TextMsg::Embedded(text, v)).is_err() {
                            break;
                        }
                        continue;
                    }
                    let Some((id, texts, next, cancel)) = vocab.front_mut() else { continue };
                    let (id, start) = (*id, *next);
                    let chunk: Vec<&str> = texts.iter().skip(start).take(VOCAB_CHUNK).map(String::as_str).collect();
                    if cancel.load(Ordering::SeqCst) || chunk.is_empty() {
                        vocab.pop_front();
                        continue;
                    }
                    let n = chunk.len();
                    let vectors = embed_with(&model, &chunk);
                    let failed = vectors.is_err();
                    *next += n;
                    if failed || *next >= texts.len() {
                        vocab.pop_front();
                    }
                    if done_tx.send(TextMsg::VocabPart { id, start, vectors }).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("could not start the search model: {e}"))?;
        Ok(TextWorker {
            tag,
            jobs,
            done,
            model: None,
            error: None,
            pending: HashSet::new(),
            queries: HashMap::new(),
            vocab_parts: Vec::new(),
            used: Instant::now(),
        })
    }

    /// Take in what the thread finished.
    pub fn poll(&mut self) {
        for msg in self.done.try_iter() {
            match msg {
                TextMsg::Loaded(m) => self.model = Some(m),
                TextMsg::LoadFailed(e) => self.error = Some(e),
                TextMsg::Embedded(text, v) => {
                    self.pending.remove(&text);
                    if self.queries.len() >= QUERY_CACHE {
                        self.queries.clear();
                    }
                    self.queries.insert(text, v);
                }
                TextMsg::VocabPart { id, start, vectors } => self.vocab_parts.push((id, start, vectors)),
            }
        }
    }

    /// Embed `texts` (job `id`) in the background; the parts come back in [`Self::vocab_parts`]. `false` when the
    /// thread has stopped.
    pub fn embed_vocab(&mut self, id: u64, texts: Vec<String>, cancel: Arc<AtomicBool>) -> bool {
        self.used = Instant::now();
        self.jobs.send(TextJob::Vocab { id, texts, cancel }).is_ok()
    }

    /// The embedding of `text`, asking the thread for it when it is not known yet.
    pub fn vector(&mut self, text: &str) -> TextVector {
        self.poll();
        self.used = Instant::now();
        if let Some(e) = &self.error {
            return TextVector::Failed(format!("the content model could not be loaded: {e}"));
        }
        match self.queries.get(text) {
            Some(Ok(v)) => return TextVector::Ready(v.clone()),
            Some(Err(e)) => return TextVector::Failed(e.clone()),
            None => {}
        }
        if !self.pending.contains(text) {
            if self.jobs.send(TextJob::Query(text.to_string())).is_err() {
                return TextVector::Failed("the search model stopped".into());
            }
            self.pending.insert(text.to_string());
        }
        TextVector::Pending
    }

    /// [`Self::vector`], waiting for it (at most `timeout`): for commands that answer at once (`content.search`).
    pub fn wait(&mut self, text: &str, timeout: std::time::Duration) -> TextVector {
        let started = Instant::now();
        loop {
            match self.vector(text) {
                TextVector::Pending if started.elapsed() < timeout => std::thread::sleep(std::time::Duration::from_millis(2)),
                TextVector::Pending => return TextVector::Failed("the search model took too long".into()),
                done => return done,
            }
        }
    }
}

/// Counts over the library, made when the queue is.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Counts {
    /// Photos that can be indexed (in the library, not videos).
    pub total: usize,
    /// Of those, indexed at their current content.
    pub indexed: usize,
    /// Indexed at an older content (their original was replaced): queued again.
    pub stale: usize,
}

/// What content indexing keeps between calls (`Session::content`).
#[derive(Default)]
pub(crate) struct State {
    /// The selected model and whether indexing is on, and when that was last read from the settings.
    pub current: Option<Current>,
    pub enabled: bool,
    pub pace: Pace,
    pub checked: Option<Instant>,
    /// The index of the current model, for this library.
    pub index: Option<Index>,
    pub worker: Option<Worker>,
    /// Set by the activity row's ✕ (or `content.pump {cancel}`): the scan stops after the batch it is on.
    pub cancel: Arc<AtomicBool>,
    pub task: Option<crate::activity::TaskGuard>,
    /// Stopped by the user (cancelled): nothing new starts until indexing is started again.
    pub held: bool,
    /// Photos to do, the next last, and the catalog revision and time the list was made at.
    pub queue: Vec<PhotoId>,
    pub queue_stamp: Option<u64>,
    pub queue_at: Option<Instant>,
    pub in_flight: HashSet<PhotoId>,
    /// Photos that could not be read, with the content hash they failed at (tried again only when it changes).
    pub failed: HashMap<u64, (Hash, String)>,
    /// Photos whose file was missing, tried again a minute later.
    pub offline: HashSet<u64>,
    pub offline_at: Option<Instant>,
    /// Photos to embed again although their record is current (`content.reindex`).
    pub force: HashSet<u64>,
    /// The model could not be loaded (`tag`, why): indexing waits until the model or the setting changes.
    pub error: Option<(String, String)>,
    pub saved_at: Option<Instant>,
    /// Embedded this session, and the measured cost of one photo (moving average, milliseconds).
    pub embedded: u64,
    pub ms_per_photo: Option<f64>,
    /// What runs the image tower ("GPU: …" or "CPU"), once loaded, and why the GPU stopped running it, if it did.
    pub device: Option<String>,
    pub gpu_fallback: Option<String>,
    pub counts: Counts,
    /// The catalog revision and index size the counts were made at (a status with nothing changed does not count again).
    pub counts_stamp: Option<(u64, usize)>,
    pub text: Option<TextWorker>,
    /// Test hook: a stand-in for [`lightcraft_vision::load`].
    pub loader: Option<Loader>,
    /// Bumped when another library opens.
    pub epoch: u64,
    /// Auto-tags, and their settings (Settings ▸ Search; read with the others by the pump).
    pub tags: crate::cmd::content_tags::TagState,
    pub tag_parent: Option<String>,
    pub tag_threshold: Option<f32>,
}

impl State {
    /// Stop the scan: the worker ends after the batch it is on (its results are dropped), the row goes, the index is
    /// written.
    pub fn stop(&mut self) {
        self.worker = None;
        self.task = None;
        self.in_flight.clear();
        self.queue.clear();
        self.queue_stamp = None;
        self.save();
    }

    /// Write the index (a failure is kept in the index's `save_error` and logged).
    pub fn save(&mut self) {
        if let Some(i) = self.index.as_mut()
            && i.has_unsaved()
            && let Err(e) = i.save()
        {
            log::warn!("content index: {e}");
        }
        self.saved_at = Some(Instant::now());
    }

    /// Another library is being opened: the index of the old one is saved and dropped (photo ids belong to one library).
    pub fn library_changed(&mut self) {
        self.stop();
        self.index = None;
        self.failed.clear();
        self.offline.clear();
        self.force.clear();
        self.held = false;
        self.counts = Counts::default();
        self.counts_stamp = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.epoch = self.epoch.wrapping_add(1);
        self.tags.library_changed();
    }

    pub fn loader(&self) -> Loader {
        self.loader.clone().unwrap_or_else(default_loader)
    }
}

impl crate::Session {
    /// Load content models with `loader` instead of [`lightcraft_vision::load`] (tests of the frontends use a small
    /// stand-in; a host could bring its own runtime). It is called with the model's folder, its manifest and the towers
    /// wanted, on the indexing thread or (the text tower, for a search) on the session's.
    pub fn set_content_model_loader(&mut self, loader: Arc<dyn Fn(&Path, &Manifest, Towers) -> Result<Box<dyn ContentModel>, String> + Send + Sync>) {
        self.content.stop();
        self.content.text = None;
        self.content.loader = Some(loader);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A batch of photos with previews (`preview`: usable or not) and renders that take a while and count how many
    /// run at once.
    fn items(n: usize, preview: bool, live: &Arc<std::sync::atomic::AtomicUsize>, most: &Arc<std::sync::atomic::AtomicUsize>) -> Vec<Item> {
        use std::sync::atomic::AtomicUsize;
        let busy = |live: Arc<AtomicUsize>, most: Arc<AtomicUsize>| {
            move || {
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(15));
                live.fetch_sub(1, Ordering::SeqCst);
            }
        };
        (0..n)
            .map(|i| {
                let b = busy(live.clone(), most.clone());
                let load: PreviewLoader = Arc::new(move |_: &str, _: usize| {
                    b();
                    preview.then(|| lightcraft_raster::Rgba8::new(300, 200))
                });
                let r = busy(live.clone(), most.clone());
                let job: Render = Box::new(move || {
                    r();
                    Ok(lightcraft_raster::Rgba8::new(320, 240))
                });
                Item { id: PhotoId(i as u64), hash: [0; 16], preview: Some(("x.arw".into(), load)), job: Some(job), path: None }
            })
            .collect()
    }

    #[test]
    fn previews_and_renders_run_a_few_at_a_time_whatever_the_pool() {
        use std::sync::atomic::AtomicUsize;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(16).build().unwrap();
        let (reads, renders) = (PREVIEW_READS[1], RENDERS[1]);
        // previews only: at most `reads` at once
        let gates = Gates { previews: Gate::new(reads), renders: Gate::new(renders) };
        let (live, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let out = pictures(items(32, true, &live, &most), &gates, &AtomicBool::new(false), Some(&pool));
        assert_eq!(out.len(), 32);
        assert!(out.iter().all(|(_, _, r)| r.as_ref().is_ok_and(|img| img.width == 300)));
        assert_eq!(gates.previews.peak(), reads);
        assert!(most.load(Ordering::SeqCst) <= reads);
        assert_eq!(gates.renders.peak(), 0);
        // no usable preview: every photo is rendered, a few at a time
        let gates = Gates { previews: Gate::new(reads), renders: Gate::new(renders) };
        let (live, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let out = pictures(items(12, false, &live, &most), &gates, &AtomicBool::new(false), Some(&pool));
        assert!(out.iter().all(|(_, _, r)| r.as_ref().is_ok_and(|img| img.width == 320)));
        assert_eq!(gates.renders.peak(), renders);
        // (a preview read and a render may overlap, never more than the two caps together)
        assert!(most.load(Ordering::SeqCst) <= reads + renders);
        // Low reads less at once than Normal, Fast more
        assert!(PREVIEW_READS.is_sorted() && RENDERS.is_sorted() && RENDERS[0] == 1);
    }

    #[test]
    fn renders_that_run_on_another_pool_never_deadlock_on_the_gates() {
        // A render that hands work to another rayon pool (the demo scenes render on a pool of their own) keeps its
        // thread busy meanwhile: a pool thread then takes on other tasks of its own pool. With the gate waits on pool
        // threads, one of those was another photo waiting for the render gate that very thread held: the scan hung (once
        // its renders ran on the CPU).
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
        let other = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap());
        let gates = Gates { previews: Gate::new(2), renders: Gate::new(1) };
        let items: Vec<Item> = (0..16)
            .map(|i| {
                let other = other.clone();
                let job: Render = Box::new(move || {
                    other.install(|| std::thread::sleep(std::time::Duration::from_millis(5)));
                    Ok(lightcraft_raster::Rgba8::new(64, 48))
                });
                Item { id: PhotoId(i), hash: [0; 16], preview: None, job: Some(job), path: None }
            })
            .collect();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let out = pictures(items, &gates, &AtomicBool::new(false), Some(&pool));
            let _ = tx.send((out.len(), out.iter().all(|(_, _, r)| r.is_ok()), gates.renders.peak()));
        });
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(30)), Ok((16, true, 1)), "the reads deadlocked");
    }

    /// Counts the images it is asked to embed.
    struct Counting(Arc<std::sync::atomic::AtomicUsize>, Manifest);

    impl ContentModel for Counting {
        fn embed_images(&self, batch: &[Rgb8]) -> lightcraft_vision::Result<Vec<Vec<f32>>> {
            self.0.fetch_add(batch.len(), Ordering::SeqCst);
            Ok(batch.iter().map(|_| vec![1.0, 0.0]).collect())
        }
        fn embed_texts(&self, _: &[&str]) -> lightcraft_vision::Result<Vec<Vec<f32>>> {
            Ok(Vec::new())
        }
        fn score_cosine(&self, cosine: f32) -> f32 {
            cosine
        }
        fn manifest(&self) -> &Manifest {
            &self.1
        }
    }

    #[test]
    fn a_worker_the_session_let_go_of_reads_and_embeds_nothing_more() {
        use std::sync::atomic::AtomicUsize;
        let (live, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let calls = Arc::new(AtomicUsize::new(0));
        let model = Counting(calls.clone(), lightcraft_vision::manifest::builtin_siglip2_b16_256());
        // let go of before the batch: no picture is read, the model never runs, every photo comes back skipped
        let gone = AtomicBool::new(true);
        let out = embed(&model, items(6, false, &live, &most), Pace::Low.gates(), &gone, None);
        assert_eq!(out.len(), 6);
        assert!(out.iter().all(|(_, _, o)| matches!(o, Outcome::Skipped)));
        assert_eq!((most.load(Ordering::SeqCst), calls.load(Ordering::SeqCst)), (0, 0));
        // still wanted: read and embedded
        let out = embed(&model, items(3, true, &live, &most), Pace::Low.gates(), &AtomicBool::new(false), None);
        assert!(out.iter().all(|(_, _, o)| matches!(o, Outcome::Embedded(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        // let go of while its pictures were being read (waiting for the shared render gate): the model is not run
        let gone = Arc::new(AtomicBool::new(false));
        let flag = gone.clone();
        let mut batch = items(4, false, &live, &most);
        batch[0].job = Some(Box::new(move || {
            flag.store(true, Ordering::SeqCst);
            Ok(lightcraft_raster::Rgba8::new(320, 240))
        }));
        let out = embed(&model, batch, Pace::Low.gates(), &gone, None);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|(_, _, o)| matches!(o, Outcome::Skipped)), "nothing embedded");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "the model did not run again");
    }

    /// Two scans at once (two sessions: the tests of a frontend, or a library open in two windows) share the pace's
    /// pool and the render gate. A render that hands work to another rayon pool (the demo scenes render on a pool of
    /// their own) keeps its thread busy meanwhile, and a pool thread then takes on other tasks of its own pool: with the
    /// gate waits on pool threads, one of those was the other scan's photo waiting for the render gate that very thread
    /// held, and both scans hung.
    #[test]
    fn two_scans_sharing_the_pool_and_the_gates_never_deadlock() {
        let pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap());
        let other = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap());
        let gates = Arc::new(Gates { previews: Gate::new(2), renders: Gate::new(1) });
        let batch = |n: u64| -> Vec<Item> {
            (0..n)
                .map(|i| {
                    let other = other.clone();
                    let job: Render = Box::new(move || {
                        other.install(|| std::thread::sleep(std::time::Duration::from_millis(2)));
                        Ok(lightcraft_raster::Rgba8::new(64, 48))
                    });
                    Item { id: PhotoId(i), hash: [0; 16], preview: None, job: Some(job), path: None }
                })
                .collect()
        };
        let (tx, rx) = channel();
        for _ in 0..4 {
            let (pool, gates, tx, items) = (pool.clone(), gates.clone(), tx.clone(), batch(24));
            std::thread::spawn(move || {
                let out = pictures(items, &gates, &AtomicBool::new(false), Some(&pool));
                let _ = tx.send((out.len(), out.iter().all(|(_, _, r)| r.is_ok())));
            });
        }
        for _ in 0..4 {
            assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(30)), Ok((24, true)), "the scans deadlocked");
        }
        assert_eq!(gates.renders.peak(), 1);
    }

    #[test]
    fn the_pace_sets_threads_and_batches() {
        use Pace::*;
        assert_eq!([Low, Normal, Fast].map(|p| p.threads(32)), [4, 16, 25]);
        assert_eq!([Low, Normal, Fast].map(|p| p.threads(8)), [2, 4, 6]);
        assert_eq!([Low, Normal, Fast].map(|p| p.threads(1)), [1, 1, 1]);
        assert_eq!([Low, Normal, Fast].map(|p| p.threads(0)), [1, 1, 1]);
        assert_eq!([Low, Normal, Fast].map(Pace::batch), [8, 16, 32]);
        assert!([Low, Normal, Fast].iter().all(|p| p.batch() <= lightcraft_vision::MAX_BATCH));
        assert_eq!(["low", "normal", "fast", "turbo", ""].map(Pace::parse), [Some(Low), Some(Normal), Some(Fast), None, None]);
        assert!([Low, Normal, Fast].iter().all(|p| Pace::parse(p.name()) == Some(*p)));
        for p in [Low, Normal, Fast] {
            let pool = pool(p).expect("a pool");
            assert_eq!(pool.current_num_threads(), p.threads(cores()));
        }
    }
}
