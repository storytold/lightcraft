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
/// Embedded previews read at once: each reads its raw file (a 50 MP raw is ~60 MB) and decodes the preview.
pub(crate) const MAX_PREVIEW_READS: usize = 4;
/// Photos rendered from their file at once (no usable preview: a JPEG, a TIFF, a raw without one). A render can hold
/// the whole file and a full-size decode, so one at a time: a few big files at once were the 2–5 GB peaks of a scan.
pub(crate) const MAX_RENDERS: usize = 1;

/// A counting gate: at most `max` holders at once (the others wait), the most there ever were, and how often one
/// had to wait.
pub(crate) struct Gate {
    max: usize,
    /// Holders now, the most at once, and the times one had to wait.
    state: Mutex<(usize, usize, usize)>,
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
        Gate { max, state: Mutex::new((0, 0, 0)), freed: Condvar::new() }
    }

    /// Wait for a place, and hold it until the pass is dropped.
    pub fn enter(&self) -> Pass<'_> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.0 >= self.max.max(1) {
            st.2 += 1;
        }
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

    /// How often a holder had to wait for a place.
    #[cfg(test)]
    pub fn waits(&self) -> usize {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).2
    }
}

/// How many pictures are read at once, by kind (shared by every scan of the process).
pub(crate) struct Gates {
    pub previews: Gate,
    pub renders: Gate,
}

pub(crate) static GATES: Gates = Gates { previews: Gate::new(MAX_PREVIEW_READS), renders: Gate::new(MAX_RENDERS) };

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
    let i = match pace {
        Pace::Low => 0,
        Pace::Normal => 1,
        Pace::Fast => 2,
    };
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

/// The real loader.
pub(crate) fn default_loader() -> Loader {
    Arc::new(|dir: &Path, m: &Manifest, towers: Towers| lightcraft_vision::load(dir, m, towers).map_err(|e| e.to_string()))
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

/// A render job as a [`Render`].
pub(crate) fn render(job: RenderJob) -> Render {
    Box::new(move || match job.run().rendered {
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
}

fn to_rgb(img: &lightcraft_raster::Rgba8) -> Rgb8 {
    Rgb8 { width: img.width, height: img.height, data: img.data.iter().map(|p| [p[0], p[1], p[2]]).collect() }
}

/// One photo on its way to a picture: read from its preview, or still to be rendered.
enum Step {
    Read(PhotoId, Hash, Result<Rgb8, Outcome>),
    Render(PhotoId, Hash, Option<Render>, Option<String>),
}

/// Why a photo has no picture: offline when its original is gone, else what went wrong.
fn outcome(why: String, path: Option<String>) -> Outcome {
    match path {
        Some(p) if !Path::new(&p).exists() => Outcome::Offline,
        _ => Outcome::Failed(why.chars().take(200).collect()),
    }
}

/// A photo's embedded preview, when it is usable; else the photo is to be rendered. At most [`MAX_PREVIEW_READS`] run
/// at once (`gates`).
fn read_preview(item: Item, gates: &Gates) -> Step {
    let Item { id, hash, preview, job, path } = item;
    let read = preview.and_then(|(p, load)| {
        let _pass = gates.previews.enter();
        catch_unwind(AssertUnwindSafe(|| load(&p, EDGE))).ok().flatten()
    });
    match read.filter(|img| img.width.max(img.height) >= MIN_PREVIEW_EDGE) {
        Some(img) => Step::Read(id, hash, Ok(to_rgb(&img))),
        None => Step::Render(id, hash, job, path),
    }
}

/// A photo rendered from its file. At most [`MAX_RENDERS`] run at once (`gates`).
fn render_picture(id: PhotoId, hash: Hash, job: Option<Render>, path: Option<String>, gates: &Gates) -> (PhotoId, Hash, Result<Rgb8, Outcome>) {
    let Some(job) = job else { return (id, hash, Err(outcome("nothing to render it from".into(), path))) };
    let _pass = gates.renders.enter();
    let r = match catch_unwind(AssertUnwindSafe(job)) {
        Ok(Ok(img)) => Ok(to_rgb(&img)),
        Ok(Err(why)) => Err(outcome(why, path)),
        Err(_) => Err(outcome("reading it stopped unexpectedly".into(), path)),
    };
    (id, hash, r)
}

/// `f` of every item, on at most `lanes` tasks of the current pool, each taking the next item when it is done.
///
/// The lanes are what bound how many run at once, not waiting on a gate inside the pool: a render's decode runs nested
/// work on the same pool, and a thread waiting for it can pick up another photo of the batch; had that photo waited
/// on a gate the thread itself holds, indexing would hang. With as many lanes as the gate has places, a lane never
/// waits on it.
fn in_lanes<T: Send, U: Send>(items: Vec<T>, lanes: usize, f: impl Fn(T) -> U + Sync) -> Vec<U> {
    use rayon::prelude::*;
    let n = items.len();
    let queue = Mutex::new(items.into_iter());
    let out = Mutex::new(Vec::with_capacity(n));
    (0..lanes.clamp(1, n.max(1))).into_par_iter().for_each(|_| {
        loop {
            let next = queue.lock().unwrap_or_else(PoisonError::into_inner).next();
            let Some(item) = next else { break };
            let r = f(item);
            out.lock().unwrap_or_else(PoisonError::into_inner).push(r);
        }
    });
    out.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// The pictures of a batch, read in parallel (within the gates): the previews first, then the photos without a
/// usable one rendered.
fn pictures(items: Vec<Item>, gates: &Gates) -> Vec<(PhotoId, Hash, Result<Rgb8, Outcome>)> {
    let mut out = Vec::with_capacity(items.len());
    let mut to_render = Vec::new();
    for step in in_lanes(items, gates.previews.max, |i| read_preview(i, gates)) {
        match step {
            Step::Read(id, hash, r) => out.push((id, hash, r)),
            Step::Render(id, hash, job, path) => to_render.push((id, hash, job, path)),
        }
    }
    out.extend(in_lanes(to_render, gates.renders.max, |(id, hash, job, path)| render_picture(id, hash, job, path, gates)));
    out
}

/// Embed one batch with `model` (already on the pace's pool).
fn embed(model: &dyn ContentModel, items: Vec<Item>) -> Vec<(PhotoId, Hash, Outcome)> {
    let pictures = pictures(items, &GATES);
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
                        let _ = done_tx.send(Done { results: skip(batch.items), ms: 0.0, embedded: 0, model_error: None });
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
                        let _ = done_tx.send(Done { results: skip(batch.items), ms: 0.0, embedded: 0, model_error: model_error.clone() });
                        continue;
                    };
                    let work = || embed(m, batch.items);
                    let results = match pool(batch.pace) {
                        Some(p) => p.install(work),
                        None => work(),
                    };
                    let embedded = results.iter().filter(|r| matches!(r.2, Outcome::Embedded(_))).count();
                    let ms = started.elapsed().as_secs_f64() * 1e3;
                    if done_tx.send(Done { results, ms, embedded, model_error: None }).is_err() {
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
        // previews only: at most MAX_PREVIEW_READS at once
        let gates = Gates { previews: Gate::new(MAX_PREVIEW_READS), renders: Gate::new(MAX_RENDERS) };
        let (live, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let out = pool.install(|| pictures(items(32, true, &live, &most), &gates));
        assert_eq!(out.len(), 32);
        assert!(out.iter().all(|(_, _, r)| r.as_ref().is_ok_and(|img| img.width == 300)));
        assert_eq!(gates.previews.peak(), MAX_PREVIEW_READS);
        // the lanes keep to the gate: no reader ever waits on it inside the pool (where waiting could hang the batch)
        assert_eq!(gates.previews.waits(), 0);
        assert!(most.load(Ordering::SeqCst) <= MAX_PREVIEW_READS);
        assert_eq!(gates.renders.peak(), 0);
        // no usable preview: every photo is rendered, one at a time
        let gates = Gates { previews: Gate::new(MAX_PREVIEW_READS), renders: Gate::new(MAX_RENDERS) };
        let (live, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let out = pool.install(|| pictures(items(12, false, &live, &most), &gates));
        assert!(out.iter().all(|(_, _, r)| r.as_ref().is_ok_and(|img| img.width == 320)));
        assert_eq!(gates.renders.peak(), MAX_RENDERS);
        assert_eq!(gates.renders.waits(), 0);
        // (a preview read and a render may overlap, never more than the two caps together)
        assert!(most.load(Ordering::SeqCst) <= MAX_PREVIEW_READS + MAX_RENDERS);
    }

    /// A render whose decode runs nested work on the pool (as a raw decode does) must not hang the batch: a thread
    /// waiting for that work may pick up another photo of the batch, which must not then wait on the render gate the
    /// thread itself holds.
    #[test]
    fn renders_with_nested_parallel_work_never_hang_the_batch() {
        use rayon::prelude::*;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build().unwrap();
            for _ in 0..2 {
                let gates = Gates { previews: Gate::new(MAX_PREVIEW_READS), renders: Gate::new(MAX_RENDERS) };
                // a few renders first, then many quick previews: the threads done with theirs take up the nested work
                let items: Vec<Item> = (0..128u64)
                    .map(|i| {
                        let job: Render = Box::new(|| {
                            (0..256).into_par_iter().for_each(|_| std::thread::sleep(std::time::Duration::from_micros(200)));
                            Ok(lightcraft_raster::Rgba8::new(320, 240))
                        });
                        let load: PreviewLoader = Arc::new(|_: &str, _: usize| Some(lightcraft_raster::Rgba8::new(300, 200)));
                        let preview = (i >= 8).then(|| ("x.arw".to_string(), load));
                        Item { id: PhotoId(i), hash: [0; 16], preview, job: Some(job), path: None }
                    })
                    .collect();
                let out = pool.install(|| pictures(items, &gates));
                let _ = tx.send((out.len(), gates.renders.peak()));
            }
        });
        for _ in 0..2 {
            let (n, peak) = rx.recv_timeout(std::time::Duration::from_secs(60)).expect("the batch hung");
            assert_eq!((n, peak), (128, MAX_RENDERS));
        }
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
