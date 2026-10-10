//! AI denoise, the session's side (`docs/denoise.md`): which photos have a denoised picture, making the missing ones
//! in the background, and keeping the cache within its size.
//!
//! The denoised picture of a raw photo is *cached data*, never a file in the library: it is made from the untouched
//! original by the model the user chose, kept in `<library>/denoise/<key>.lcdn`, and thrown away whenever space is
//! needed (it is made again when it is next wanted). Its name is a hash of the model, the algorithm and the photo's
//! content, so a changed file or another model simply does not find it. The Denoise *amount* is an ordinary develop
//! setting (`enhance.denoise`): a render mixes the plain and the denoised picture by it ([`crate::media`]).
//!
//! What runs where: the session thread only decides (which photos, which model) and keeps an index of what is ready
//! ([`crate::media::DenoiseIndex`], memory only). One background thread at a time decodes a photo, runs the model on
//! its mosaic tile by tile and writes the product ([`make_product`]); an export that finds its photo's product missing
//! makes it itself, on its own thread, so it never comes out without the denoise it was asked for.
//!
//! Failure is a message, never a panic: a file that is not a Bayer raw, a model that does not load, a full disk, a
//! panic inside the runtime and a cancelled job all end as an [`Outcome`] the pump records.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};

use dac_catalog::{Photo, PhotoId, Source};
use dac_denoise::bayer::Layout;
use dac_denoise::manifest::DenoiserManifest;
use dac_denoise::run::{Control, Params, TileRunner, denoise_bayer};
use dac_denoise::{Error as RunError, product};
use dac_develop::DevelopSettings;
use dac_preview::Hasher128;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::media::{DenoiseSpec, MakeProduct, PairLoader};
use crate::merge::ByteReader;
use crate::model_download::Downloads;
use crate::{Selection, Session};

/// Extension of a product file.
pub const EXT: &str = "lcdn";
/// Where the products of a library are kept, inside the library's folder (safe to delete, like `thumbs/`).
pub const DIR: &str = "denoise";
/// What the cache may take up unless the user says otherwise (GB). A product is about as large as the raw file's
/// decoded samples, compressed: ~100 MB for 24 megapixels.
pub const DEFAULT_CACHE_GB: u32 = 20;
/// Samples at or above this (white = 1) are clipped: the model leaves them as the camera recorded them.
const CLIP: f32 = 0.99;
/// Most photos waiting in the queue.
const MAX_QUEUE: usize = 5000;
/// Most selected photos the automatic queue looks at in one pump.
const AUTO_LOOK: usize = 64;
/// How long a look at the model settings is trusted.
const SETTINGS_TTL: Duration = Duration::from_secs(2);
/// Stack of the threads that run the model (the runtime recurses through its graph).
const STACK: usize = 16 << 20;

// ---------------------------------------------------------------------------------------------------------------------
// the model

/// Where the model runs (Settings ▸ AI Denoise ▸ Speed; `denoise.settings {runOn}`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunOn {
    /// The graphics card where it is faster than the processor at the pace the work goes at (both are timed on this
    /// computer when the card is set up), else the processor.
    #[default]
    Auto,
    /// The graphics card whenever it can run the model, even where the processor would be faster.
    Gpu,
    /// Always the processor.
    Cpu,
}

impl RunOn {
    /// `auto`, `gpu` or `cpu`.
    pub fn parse(name: &str) -> Option<RunOn> {
        match name {
            "auto" => Some(RunOn::Auto),
            "gpu" => Some(RunOn::Gpu),
            "cpu" => Some(RunOn::Cpu),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            RunOn::Auto => "auto",
            RunOn::Gpu => "gpu",
            RunOn::Cpu => "cpu",
        }
    }
}

/// A loaded denoise model: what runs tiles, and a self-check.
pub(crate) trait Model: Send + Sync {
    /// What runs a photo's tiles, and how many to run at once, when `threads` may be used and the setting is `run_on`
    /// (a graphics card is kept busy by a few). The first time the card may be used it is set up and checked here.
    fn runner(&self, run_on: RunOn, threads: usize) -> (&dyn TileRunner, usize);
    /// Check it does something sensible: the test's result, or why not. Unless `run_on` is the processor the graphics
    /// card is set up and checked too, and the result says where the work will run.
    fn self_test(&self, run_on: RunOn) -> Result<Value, String>;
    /// Conservative scratch bound for an unknown runner.
    fn bytes_per_tile(&self) -> usize {
        512 * 1024 * 1024
    }
    /// Where the tiles run: `{kind: "gpu", adapter}`, `{kind: "cpu", reason?}` or `{kind: "pending"}` (the card is not
    /// set up yet), with the card's and the processor's time for the check tile (`cardMs`, `cpuMs`) once both were
    /// timed. Never starts anything.
    fn device(&self, _run_on: RunOn) -> Value {
        json!({"kind": "cpu"})
    }
}

/// Loads the model at a path as its manifest describes it.
pub(crate) type Loader = Arc<dyn Fn(&Path, &DenoiserManifest) -> Result<Arc<dyn Model>, String> + Send + Sync>;

/// Tiles run at once on a graphics card: enough to keep it fed while others are packed and blended.
#[cfg(feature = "denoise")]
const GPU_PARALLEL: usize = 4;
/// How far the GPU's answer on the check tile may be from the CPU's (of the answer's largest value) before it is not used.
#[cfg(feature = "denoise")]
const GPU_AGREE: f32 = 1e-3;
/// Longest the card may take to set up (build its kernels, run the check tile, time it) before the processor does the
/// work instead.
#[cfg(feature = "denoise")]
const GPU_SETUP_LIMIT: Duration = Duration::from_secs(30);
/// Tiles the card may get wrong (an error, numbers that are not finite) before it stops being used for the model.
#[cfg(feature = "denoise")]
const GPU_MAX_FALLBACKS: usize = 3;
/// The file beside a model while the graphics card is set up for it (see [`set_up_card`]).
pub(crate) const GPU_SETUP_MARKER: &str = "gpu-setup.marker";
/// Why the card is not tried when a set-up marker is found.
#[cfg(feature = "denoise")]
const GPU_CRASHED: &str = "The app closed while it was setting up the graphics card for AI Denoise last time";
/// Why the card is not used when its set-up ran out of time (the start of it).
#[cfg(feature = "denoise")]
const GPU_SLOW: &str = "setting up the graphics card took longer than";
/// Why the card is not used while a set-up that ran out of time is still going.
#[cfg(feature = "denoise")]
const GPU_STUCK: &str = "the graphics card is still setting up from before (it took too long)";

/// A set-up that may work another time (a crash, the time limit), unlike one the card or the model cannot do.
#[cfg(feature = "denoise")]
fn worth_retrying(why: &str) -> bool {
    why == GPU_CRASHED || why == GPU_STUCK || why.starts_with(GPU_SLOW)
}

/// The model on our pure-Rust CPU runner, and on the graphics card when there is one that can run it: the card is set up for the
/// first photo that may use it, checked against the CPU's answer on a check tile and timed against it, and used while
/// it keeps working; a tile it gets wrong is run on the CPU, so a photo is always finished.
#[cfg(feature = "denoise")]
struct Network {
    cpu: dac_denoise::runtime::CpuRunner,
    path: PathBuf,
    tile: usize,
    gpu: OnceLock<Result<GpuSide, String>>,
    /// Tiles the card got wrong (an error, numbers that are not finite) and the CPU ran.
    fallbacks: AtomicUsize,
    /// Tiles at once of the photo started last (0: none yet), to say where the work runs.
    last_threads: AtomicUsize,
}

#[cfg(feature = "denoise")]
struct GpuSide {
    runner: Box<dyn TileRunner + Send>,
    adapter: String,
    /// The check tile on the card, the fastest of a few runs (ms).
    card_ms: f64,
    /// The check tile on the CPU, on one thread (ms).
    cpu_ms: f64,
}

/// Whether the card (`card_ms` a tile) is faster than the CPU running `threads` tiles at once, from the CPU's time for
/// one tile alone (`cpu_ms`). Tiles at once share the memory and the cores' second threads, so each one more counts as
/// half a tile's speed: on a 16-core laptop one tile alone took 1.4 s and sixteen at once finished one every 0.2 s.
#[cfg(feature = "denoise")]
fn card_is_faster(card_ms: f64, cpu_ms: f64, threads: usize) -> bool {
    let at_once = 1.0 + 0.5 * threads.saturating_sub(1) as f64;
    card_ms.is_finite() && card_ms <= cpu_ms / at_once
}

/// Card set-ups of this process that have not finished, by marker (their markers are not a crash's).
#[cfg(feature = "denoise")]
static SETTING_UP: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Removes a set-up's marker when the set-up ends, however it ends — but not when the process dies inside the driver.
#[cfg(feature = "denoise")]
struct SetUpEnded(PathBuf);

#[cfg(feature = "denoise")]
impl Drop for SetUpEnded {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        SETTING_UP.lock().unwrap_or_else(PoisonError::into_inner).retain(|m| m != &self.0);
    }
}

/// Set the graphics card up with `card` on a thread of its own while `meanwhile` runs here, and give up on the card when
/// it takes longer than `limit` (a driver building kernels for minutes, or stuck). The `marker` file exists while `card`
/// runs: one found before it starts was left by a the app that closed during a set-up (a driver crash takes the app
/// with it), so the card is not tried again until the user chooses where denoise runs (`denoise.settings {runOn}`).
#[cfg(feature = "denoise")]
fn set_up_card<T: Send + 'static, R>(
    marker: &Path,
    limit: Duration,
    card: impl FnOnce() -> Result<T, String> + Send + 'static,
    meanwhile: impl FnOnce() -> R,
) -> Result<(T, R), String> {
    let ended = {
        let mut running = SETTING_UP.lock().unwrap_or_else(PoisonError::into_inner);
        if running.iter().any(|m| m == marker) {
            return Err(GPU_STUCK.into());
        }
        if marker.exists() {
            return Err(GPU_CRASHED.into());
        }
        let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        // a marker that cannot be written only loses the crash guard
        let _ = std::fs::write(marker, format!("setting up the graphics card for AI Denoise (unix time {secs})\n"));
        running.push(marker.to_path_buf());
        SetUpEnded(marker.to_path_buf())
    };
    let started = web_time::Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("denoise-gpu-setup".into())
        .stack_size(STACK)
        .spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(card)).unwrap_or_else(|_| Err("the GPU runner gave up".into()));
            drop(ended);
            let _ = tx.send(r);
        })
        .map_err(|e| format!("could not start setting up the graphics card: {e}"))?;
    let here = meanwhile();
    match rx.recv_timeout(limit.saturating_sub(started.elapsed())) {
        Ok(r) => r.map(|t| (t, here)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!("{GPU_SLOW} {} s, so it is not used", limit.as_secs_f64())),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("the GPU runner gave up".into()),
    }
}

/// The card's time for one tile once it is warm: the fastest of up to three runs, stopping after about two seconds (a
/// slow card is slow enough to tell at once).
#[cfg(feature = "denoise")]
fn card_tile_ms(runner: &dyn TileRunner, input: &[f32]) -> f64 {
    if runner.run(input).is_err() {
        return f64::INFINITY;
    }
    let began = web_time::Instant::now();
    let mut best = f64::INFINITY;
    for _ in 0..3 {
        let one = web_time::Instant::now();
        if runner.run(input).is_err() {
            break;
        }
        best = best.min(one.elapsed().as_secs_f64() * 1000.0);
        if began.elapsed() > Duration::from_secs(2) {
            break;
        }
    }
    best
}

/// Set the card up for the model at `path`: kernels built, the check tile run on both and compared, both timed.
#[cfg(feature = "denoise")]
fn make_gpu(cpu: &dac_denoise::runtime::CpuRunner, path: &Path, tile: usize) -> Result<GpuSide, String> {
    let input = Arc::new(dac_denoise::runtime::check_tile(tile));
    let (on_card, net) = (input.clone(), cpu.net().clone());
    let card = move || -> Result<_, String> {
        let runner = dac_gpu::nn::runner(&net, tile)?;
        let got = runner.run(&on_card).map_err(|e| format!("the GPU could not run the check tile: {e}"))?;
        let card_ms = card_tile_ms(&runner, &on_card);
        Ok((runner, got, card_ms))
    };
    let on_cpu = || {
        let started = web_time::Instant::now();
        (cpu.run(&input), started.elapsed().as_secs_f64() * 1000.0)
    };
    let ((runner, got, card_ms), (want, cpu_ms)) = set_up_card(&path.with_file_name(GPU_SETUP_MARKER), GPU_SETUP_LIMIT, card, on_cpu)?;
    let want = want.map_err(|e| format!("the CPU could not check the GPU: {e}"))?;
    let scale = want.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-12);
    let worst = want.iter().zip(&got).fold(0f32, |m, (a, b)| m.max((a - b).abs())) / scale;
    if got.len() != want.len() || !got.iter().all(|v| v.is_finite()) || worst > GPU_AGREE {
        return Err(format!("its answer on the check tile is not the CPU's (off by {worst:.1e} of the largest value): not used"));
    }
    Ok(GpuSide { adapter: runner.adapter(), runner: Box::new(runner), card_ms, cpu_ms })
}

#[cfg(feature = "denoise")]
impl Network {
    /// The card when it can be used now, else why not. `start` sets it up when that has not been tried yet; without it
    /// nothing is started.
    fn card(&self, start: bool) -> Result<&GpuSide, String> {
        let side = if start {
            Some(self.gpu.get_or_init(|| {
                let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make_gpu(&self.cpu, &self.path, self.tile)))
                    .unwrap_or_else(|_| Err("the GPU runner gave up".into()));
                if let Err(why) = &made {
                    log::info!("denoise: running on the CPU ({why})");
                }
                made
            }))
        } else {
            self.gpu.get()
        };
        let g = match side {
            None => return Err("the graphics card is not set up yet".into()),
            Some(Err(why)) => return Err(why.clone()),
            Some(Ok(g)) => g,
        };
        if let Some(why) = dac_gpu::nn::broken_reason() {
            return Err(format!("the graphics card stopped working ({why})"));
        }
        if self.fallbacks.load(Ordering::Relaxed) >= GPU_MAX_FALLBACKS {
            return Err(format!("{} got {GPU_MAX_FALLBACKS} tiles wrong", g.adapter));
        }
        Ok(g)
    }

    /// The card, when a photo made `threads` tiles at a time under `run_on` runs on it; else why the CPU does.
    fn choose(&self, run_on: RunOn, threads: usize, start: bool) -> Result<&GpuSide, String> {
        if run_on == RunOn::Cpu {
            return Err("the processor is chosen in Settings".into());
        }
        let g = self.card(start)?;
        if run_on == RunOn::Auto && !card_is_faster(g.card_ms, g.cpu_ms, threads) {
            return Err(format!("it is faster than {} on this computer", g.adapter));
        }
        Ok(g)
    }

    fn fell_back(&self, why: &str) {
        let n = self.fallbacks.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            log::warn!("denoise: a tile came back wrong from the GPU ({why}); the CPU runs it");
        } else if n == GPU_MAX_FALLBACKS {
            log::warn!("denoise: the GPU got {n} tiles wrong; the CPU does the rest");
        }
    }
}

/// The card, with the CPU running any tile it gets wrong. Handed out only for photos that run on the card.
#[cfg(feature = "denoise")]
impl TileRunner for Network {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, RunError> {
        if let Ok(g) = self.card(false) {
            match g.runner.run(input) {
                Ok(out) if out.iter().all(|v| v.is_finite()) => return Ok(out),
                Ok(_) => self.fell_back("numbers that are not finite"),
                Err(e) => self.fell_back(&e.to_string()),
            }
        }
        self.cpu.run(input)
    }
}

#[cfg(feature = "denoise")]
impl Model for Network {
    fn bytes_per_tile(&self) -> usize {
        self.cpu.bytes_per_tile()
    }
    fn runner(&self, run_on: RunOn, threads: usize) -> (&dyn TileRunner, usize) {
        let threads = threads.max(1);
        self.last_threads.store(threads, Ordering::Relaxed);
        match self.choose(run_on, threads, true) {
            Ok(_) => (self, threads.min(GPU_PARALLEL)),
            Err(_) => (&self.cpu, threads),
        }
    }

    fn self_test(&self, run_on: RunOn) -> Result<Value, String> {
        let t = self.cpu.self_test();
        if !t.ok {
            let failed: Vec<&str> = t.checks.iter().filter(|(_, ok)| !*ok).map(|(c, _)| c.as_str()).collect();
            return Err(format!("the model failed its self-test: it does not {}", failed.join(", and does not ")));
        }
        let mut v = serde_json::to_value(&t).map_err(|e| e.to_string())?;
        // the card is set up and checked here, so the test says where the work will run
        if run_on != RunOn::Cpu {
            let _ = self.card(true);
        }
        if let Some(o) = v.as_object_mut() {
            o.insert("device".into(), self.device(run_on));
        }
        Ok(v)
    }

    fn device(&self, run_on: RunOn) -> Value {
        if run_on != RunOn::Cpu && self.gpu.get().is_none() {
            return json!({"kind": "pending"});
        }
        let threads = match self.last_threads.load(Ordering::Relaxed) {
            0 => Pace::Full.parallel(std::thread::available_parallelism().map_or(1, |n| n.get()), None),
            n => n,
        };
        let mut v = match self.choose(run_on, threads, false) {
            Ok(g) => json!({"kind": "gpu", "adapter": g.adapter}),
            // a set-up that crashed or ran out of time can be tried again (`denoise.settings {runOn}`)
            Err(why) => json!({"kind": "cpu", "retry": worth_retrying(&why), "reason": why}),
        };
        if let (Some(Ok(g)), Some(o)) = (self.gpu.get(), v.as_object_mut()) {
            // measured on this computer, on the check tile: the card's best run, and one tile alone on the CPU
            o.insert("cardMs".into(), json!(g.card_ms));
            o.insert("cpuMs".into(), json!(g.cpu_ms));
            o.insert("fellBack".into(), json!(self.fallbacks.load(Ordering::Relaxed)));
        }
        v
    }
}

/// The loader of this build: our pure-Rust executor with the `denoise` feature, else one that says why nothing can run.
pub(crate) fn default_loader() -> Loader {
    #[cfg(feature = "denoise")]
    {
        Arc::new(|path, manifest| {
            let cpu = dac_denoise::runtime::CpuRunner::load(path, manifest).map_err(|e| e.to_string())?;
            Ok(Arc::new(Network {
                cpu,
                path: path.to_path_buf(),
                tile: manifest.tile as usize,
                gpu: OnceLock::new(),
                fallbacks: AtomicUsize::new(0),
                last_threads: AtomicUsize::new(0),
            }) as Arc<dyn Model>)
        })
    }
    #[cfg(not(feature = "denoise"))]
    {
        Arc::new(|_, _| Err("this build cannot run denoise models".to_string()))
    }
}

/// A model that is loaded on first use and kept, shared by the background job and the exports.
pub(crate) type ModelSlot = Arc<Mutex<Option<Arc<dyn Model>>>>;

// ---------------------------------------------------------------------------------------------------------------------
// settings and installed models

/// `settings.json` in the models folder.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Settings {
    /// The installed model in use; `None`: denoise is off.
    pub model: Option<String>,
    /// Photos with a Denoise amount above 0 that are being looked at (active, selected) get their picture made without
    /// being asked (`None`: yes).
    pub auto: Option<bool>,
    /// Cache size limit (GB; `None`: [`DEFAULT_CACHE_GB`]).
    pub cache_gb: Option<u32>,
    /// Most tiles run at once (`None`: by how hard the pump says to work).
    pub threads: Option<u32>,
    /// Where the model runs: `auto`, `gpu` or `cpu` (`None` or anything else: `auto`; see [`RunOn`]).
    pub run_on: Option<String>,
    /// What came before `runOn`: `false` was the processor only.
    pub gpu: Option<bool>,
}

impl Settings {
    pub fn auto(&self) -> bool {
        self.auto.unwrap_or(true)
    }

    pub fn run_on(&self) -> RunOn {
        match self.run_on.as_deref().and_then(RunOn::parse) {
            Some(r) => r,
            None if self.gpu == Some(false) => RunOn::Cpu,
            None => RunOn::Auto,
        }
    }

    pub fn cache_bytes(&self) -> u64 {
        u64::from(self.cache_gb.unwrap_or(DEFAULT_CACHE_GB).clamp(1, 10_000)) << 30
    }
}

pub(crate) fn read_capped(path: &Path, max: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(max as u64 + 1).read_to_end(&mut buf).ok()?;
    (buf.len() <= max).then_some(buf)
}

pub(crate) fn read_settings(dir: &Path) -> Settings {
    read_capped(&dir.join("settings.json"), 4096).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Write a file next to its final name and rename it into place.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let part = path.with_extension("part");
    std::fs::write(&part, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    std::fs::rename(&part, path).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("could not write {}: {e}", path.display())
    })
}

pub(crate) fn write_settings(dir: &Path, st: &Settings) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create the denoise models folder: {e}"))?;
    write_atomic(&dir.join("settings.json"), &serde_json::to_vec_pretty(st).map_err(|e| e.to_string())?)
}

/// An installed model folder: `<id>/model.onnx`, `denoise-model.json`, `installed.json`.
pub(crate) struct Installed {
    pub manifest: DenoiserManifest,
    pub onnx: PathBuf,
    /// What was recorded when it was installed (terms accepted, hash of the file, self-test).
    pub accepted: Value,
}

impl Installed {
    /// Names the model file the cache is made with: a hash of the file as installed, so another file with the same id
    /// never finds this one's products.
    pub fn fingerprint(&self) -> String {
        let sha = self.accepted.get("sha256").and_then(Value::as_str).or(self.manifest.sha256.as_deref()).unwrap_or("");
        let len = std::fs::metadata(&self.onnx).map(|m| m.len()).unwrap_or(0);
        format!("{}@{}:{len}", self.manifest.id, sha)
    }
}

/// The models in the folder. A folder that is not a valid model (wrong name, no model file, a manifest that does not
/// validate or does not match its folder) is ignored, never trusted.
pub(crate) fn installed_models(dir: &Path) -> Vec<Installed> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        let onnx = p.join("model.onnx");
        if !dac_denoise::licence::valid_id(&name) || !p.is_dir() || !onnx.is_file() {
            continue;
        }
        let Some(m) =
            read_capped(&p.join("denoise-model.json"), dac_denoise::manifest::MAX_MANIFEST_BYTES).and_then(|b| dac_denoise::manifest::parse(&b).ok())
        else {
            continue;
        };
        if m.id != name {
            continue;
        }
        let accepted = read_capped(&p.join("installed.json"), 8192).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
        out.push(Installed { manifest: m, onnx, accepted });
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    out
}

/// Forget that setting the graphics card up for a model closed the app before (the markers [`set_up_card`] leaves):
/// the user chose where denoise runs, so the card may be tried again.
pub(crate) fn forget_gpu_set_ups(dir: &Path) {
    for i in installed_models(dir) {
        let _ = std::fs::remove_file(i.onnx.with_file_name(GPU_SETUP_MARKER));
    }
}

/// The model in use.
pub(crate) struct Active {
    pub id: String,
    pub manifest: DenoiserManifest,
    pub onnx: PathBuf,
    pub fingerprint: String,
    pub model: ModelSlot,
}

// ---------------------------------------------------------------------------------------------------------------------
// products

/// The photos whose files can be denoised at all: raw files developed from their sensor data and read from disk.
pub(crate) fn eligible(p: &Photo) -> bool {
    p.develops_raw() && matches!(p.source, Source::File { .. }) && !p.deleted
}

/// A product's name: what it was made from and with.
pub(crate) fn product_key(fingerprint: &str, p: &Photo) -> String {
    let mut h = Hasher128::new();
    h.str("lcdn").u64(u64::from(product::ALGORITHM)).str(fingerprint).str(&crate::media::content_key(p));
    h.finish().to_string()
}

pub(crate) fn product_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{EXT}"))
}

/// The `.lcdn` files in `dir`: path, size, last change.
fn products_in(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == EXT))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (e.path(), m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .collect()
}

/// Make sure only one thread writes a product at a time: a second one waits, then finds it made.
fn making() -> &'static (Mutex<HashSet<PathBuf>>, Condvar) {
    static MAKING: OnceLock<(Mutex<HashSet<PathBuf>>, Condvar)> = OnceLock::new();
    MAKING.get_or_init(|| (Mutex::new(HashSet::new()), Condvar::new()))
}

struct Slot(PathBuf);

impl Slot {
    fn enter(path: &Path) -> Slot {
        let (set, cv) = making();
        let mut g = set.lock().unwrap_or_else(PoisonError::into_inner);
        while g.contains(path) {
            g = cv.wait(g).unwrap_or_else(PoisonError::into_inner);
        }
        g.insert(path.to_path_buf());
        Slot(path.to_path_buf())
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let (set, cv) = making();
        set.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.0);
        cv.notify_all();
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// making a product

/// Everything a thread needs to make one product, detached from the session.
#[derive(Clone)]
pub(crate) struct JobSpec {
    /// The original file.
    pub path: String,
    pub read: Option<ByteReader>,
    pub product: PathBuf,
    pub key: String,
    pub manifest: DenoiserManifest,
    pub onnx: PathBuf,
    pub model: ModelSlot,
    pub loader: Loader,
    /// Tiles run at once.
    pub parallel: usize,
    /// Where the model runs (the setting).
    pub run_on: RunOn,
}

/// Why a product was not made.
#[derive(Debug, PartialEq)]
pub(crate) enum MakeError {
    /// Denoise does not apply to this file (not a Bayer raw): nothing is wrong.
    Unsupported(String),
    Failed(String),
    Cancelled,
}

impl From<RunError> for MakeError {
    fn from(e: RunError) -> Self {
        match e {
            RunError::Cancelled => MakeError::Cancelled,
            other => MakeError::Failed(other.to_string()),
        }
    }
}

/// Progress and cancel of a running job.
#[derive(Default)]
pub(crate) struct Progress {
    done: AtomicUsize,
    total: AtomicUsize,
    cancel: AtomicBool,
}

impl Progress {
    /// Ask the job to stop at the next tile.
    pub(crate) fn cancel_now(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The loaded model, loading it first when it is not (kept for the next photo).
fn loaded(spec: &JobSpec) -> Result<Arc<dyn Model>, MakeError> {
    if let Some(m) = spec.model.lock().unwrap_or_else(PoisonError::into_inner).clone() {
        return Ok(m);
    }
    let m = (spec.loader)(&spec.onnx, &spec.manifest).map_err(|e| MakeError::Failed(format!("the denoise model could not be loaded: {e}")))?;
    *spec.model.lock().unwrap_or_else(PoisonError::into_inner) = Some(m.clone());
    Ok(m)
}

/// The cell `[(0,0), (1,0), (0,1), (1,1)]` colours of a Bayer mosaic and so its layout.
fn layout_of(cfa: &dac_raw::Cfa) -> Option<Layout> {
    Layout::from_cell([cfa.color_at(0, 0), cfa.color_at(1, 0), cfa.color_at(0, 1), cfa.color_at(1, 1)])
}

/// Make the product of `spec` unless it is there: decode the photo, run the model over its mosaic, write the result.
/// `Ok(true)` when it was made now, `Ok(false)` when it already existed.
pub(crate) fn make_product(spec: &JobSpec, progress: Option<&Progress>) -> Result<bool, MakeError> {
    let _slot = Slot::enter(&spec.product);
    if product::is_current(&spec.product, &spec.key) {
        return Ok(false);
    }
    let began = web_time::Instant::now();
    let cancelled = || progress.is_some_and(|p| p.cancel.load(Ordering::Relaxed));
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let bytes = match &spec.read {
        Some(r) => r(&spec.path).map_err(MakeError::Failed)?,
        None => std::fs::read(&spec.path).map_err(|e| MakeError::Failed(format!("{}: {e}", spec.path)))?,
    };
    // the decode and the normalised mosaic wait their turn at the process-wide memory gate, as every decode does
    let (mosaic, layout) = {
        let _held = crate::memory::work_gate().acquire(bytes.len().saturating_mul(4));
        if dac_raw::probe(&bytes).is_none() {
            return Err(MakeError::Unsupported("not a raw file".into()));
        }
        let raw = match dac_raw::decode(&bytes) {
            Ok(r) => r,
            Err(dac_raw::RawError::Unsupported(why)) => return Err(MakeError::Unsupported(why)),
            Err(e) => return Err(MakeError::Failed(e.to_string())),
        };
        drop(bytes);
        if raw.cpp != 1 || !raw.cfa.as_ref().is_some_and(dac_raw::Cfa::is_bayer) {
            return Err(MakeError::Unsupported(
                "only Bayer raw files can be denoised (this one has another sensor layout or is already demosaiced)".into(),
            ));
        }
        let n = raw.normalized().map_err(|e| MakeError::Failed(e.to_string()))?;
        drop(raw);
        let layout =
            n.cfa.as_ref().and_then(layout_of).ok_or_else(|| MakeError::Unsupported("the colour filter layout is not a Bayer pattern".into()))?;
        (n, layout)
    };
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let decoded = began.elapsed();
    let model = loaded(spec)?;
    // Packed input, returned RGB and the preceding group's RGB overlap while tiles run.
    let scratch =
        model.bytes_per_tile().saturating_add(128usize.saturating_mul(spec.manifest.tile as usize).saturating_mul(spec.manifest.tile as usize));
    let (_, budget) = crate::memory::work_gate().usage();
    let parallel = spec.parallel.clamp(1, 4).min((budget / scratch.max(1)).max(1));
    let _held = crate::memory::work_gate().acquire(mosaic.data.len().saturating_mul(20).saturating_add(scratch.saturating_mul(parallel)));
    let (runner, parallel) = model.runner(spec.run_on, parallel);
    let params = Params {
        tile: spec.manifest.tile as usize,
        overlap: spec.manifest.overlap as usize,
        gain: spec.manifest.gain,
        clip: Some(CLIP),
        parallel: parallel.max(1),
    };
    let report = |done: usize, total: usize| {
        if let Some(p) = progress {
            p.total.store(total, Ordering::Relaxed);
            p.done.store(done, Ordering::Relaxed);
        }
    };
    let ctl = Control { cancel: progress.map(|p| &p.cancel), progress: Some(&report) };
    // the tiles run on threads of their own, so a busy window's renders keep theirs
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(params.parallel)
        .stack_size(STACK)
        .thread_name(|i| format!("denoise-{i}"))
        .build()
        .map_err(|e| MakeError::Failed(format!("could not start the worker threads: {e}")))?;
    let rgb = pool.install(|| denoise_bayer(&mosaic.data, mosaic.width, mosaic.height, layout, runner, &params, &ctl))?;
    drop(mosaic);
    if cancelled() {
        return Err(MakeError::Cancelled);
    }
    let ran = began.elapsed();
    product::write(&spec.product, &rgb, &spec.key).map_err(|e| MakeError::Failed(format!("could not save the denoised picture: {e}")))?;
    if dac_brand::env_os("PROFILE").is_some() {
        eprintln!(
            "[denoise] read + decode {:.0} ms, model (set-up, tiles, blend) {:.0} ms, write {:.0} ms",
            decoded.as_secs_f64() * 1e3,
            (ran - decoded).as_secs_f64() * 1e3,
            (began.elapsed() - ran).as_secs_f64() * 1e3
        );
    }
    Ok(true)
}

// ---------------------------------------------------------------------------------------------------------------------
// the session's state

/// How hard the background work may go (the app picks it from what the user is doing, like the face scan).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    /// Start nothing new.
    Pause,
    /// Two tiles at once.
    Light,
    /// Half the machine.
    Normal,
    /// Most of the machine: the user is watching the progress.
    Full,
}

impl Pace {
    /// `pause`, `light`, `normal` or `full`; anything else is `normal`.
    pub fn parse(name: Option<&str>) -> Pace {
        match name {
            Some("pause") => Pace::Pause,
            Some("light") => Pace::Light,
            Some("full") => Pace::Full,
            _ => Pace::Normal,
        }
    }

    /// Tiles to run at once on a machine with `cores` threads (`cap`: the user's own limit).
    pub fn parallel(self, cores: usize, cap: Option<u32>) -> usize {
        let n = match self {
            Pace::Pause | Pace::Light => 2,
            Pace::Normal => cores / 2,
            Pace::Full => cores * 4 / 5,
        }
        .clamp(1, 16);
        cap.map_or(n, |c| n.min(c.max(1) as usize))
    }
}

/// How a photo stands, for the interface.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum PhotoState {
    /// Not a photo denoise applies to (not a raw, offline, a demo).
    NotApplicable,
    /// No model is chosen: nothing can be made.
    NoModel,
    /// Its picture is made: the Amount slider works.
    Ready,
    Queued {
        /// Photos ahead of it (0 = next).
        ahead: usize,
    },
    Running {
        done: usize,
        total: usize,
    },
    /// Making it failed (`why`), or denoise does not apply to this file (`unsupported`).
    Failed {
        why: String,
        unsupported: bool,
    },
    /// Nothing has asked for it yet.
    Idle,
}

#[derive(Clone)]
struct Failure {
    key: String,
    why: String,
    unsupported: bool,
}

enum Outcome {
    Done,
    Failed(String, bool),
    Cancelled,
}

struct Running {
    photo: PhotoId,
    key: String,
    product: PathBuf,
    progress: Arc<Progress>,
    outcome: Arc<Mutex<Option<Outcome>>>,
    started: web_time::Instant,
}

pub(crate) struct State {
    /// Where the models are kept (`None` where there is no file system).
    pub models_dir: Option<PathBuf>,
    /// Models being downloaded at the user's request.
    pub downloads: Downloads,
    pub(crate) install: Option<crate::cmd::denoise::InstallJob>,
    /// How a model is loaded (tests put another in).
    pub loader: Loader,
    pub(crate) active: Option<Active>,
    active_seen: Option<web_time::Instant>,
    /// `settings.json` as last read (with the model choice).
    pub(crate) settings: Settings,
    /// The index must be made again (the library, the model or the cache changed).
    dirty: bool,
    /// The catalog revision and time of the last index, to look again when photos changed.
    reindexed: Option<(u64, web_time::Instant)>,
    queue: VecDeque<PhotoId>,
    running: Option<Running>,
    failed: HashMap<PhotoId, Failure>,
    /// Counts every change of what is ready, so the interface knows to draw again.
    pub generation: u64,
    /// Photos made since the session started.
    made: u64,
    /// AI Denoise's row in the activity stack, while a photo is being made or waits its turn.
    task: Option<crate::activity::TaskGuard>,
}

impl Default for State {
    fn default() -> Self {
        State {
            models_dir: None,
            downloads: Downloads::default(),
            install: None,
            loader: default_loader(),
            active: None,
            active_seen: None,
            settings: Settings::default(),
            dirty: true,
            reindexed: None,
            queue: VecDeque::new(),
            running: None,
            failed: HashMap::new(),
            generation: 0,
            made: 0,
            task: None,
        }
    }
}

impl Drop for State {
    /// Quitting stops the job instead of leaving it to finish a photo nobody will see.
    fn drop(&mut self) {
        if let Some(r) = &self.running {
            r.progress.cancel_now();
        }
    }
}

impl State {
    /// The library was replaced: nothing carries over (photo ids are per library).
    pub fn library_changed(&mut self) {
        if let Some(r) = self.running.take() {
            r.progress.cancel_now();
        }
        self.queue.clear();
        self.failed.clear();
        self.dirty = true;
        self.generation += 1;
    }

    /// The model choice or the cache changed on disk: look again at the next pump.
    pub fn touch(&mut self) {
        self.active_seen = None;
        self.dirty = true;
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }
}

impl Session {
    /// Where the active model's tiles run (see [`Model::device`]); `{kind: "none"}` while no model is loaded.
    pub(crate) fn denoise_device(&self) -> Value {
        let loaded = self.denoise.active.as_ref().and_then(|a| a.model.lock().unwrap_or_else(PoisonError::into_inner).clone());
        match loaded {
            Some(m) => m.device(self.denoise.settings.run_on()),
            None => json!({"kind": "none"}),
        }
    }

    /// The folder the products of the open library are kept in; `None` without a library on disk.
    pub(crate) fn denoise_products_dir(&self) -> Option<PathBuf> {
        self.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join(DIR))
    }

    /// Read which model is in use (at most every couple of seconds): the manifest and file of the model the settings
    /// name, if it is installed.
    pub(crate) fn denoise_refresh_active(&mut self, force: bool) {
        let fresh = self.denoise.active_seen.is_some_and(|t| t.elapsed() < SETTINGS_TTL);
        if fresh && !force {
            return;
        }
        self.denoise.active_seen = Some(web_time::Instant::now());
        self.denoise.settings = self.denoise.models_dir.as_deref().map(read_settings).unwrap_or_default();
        let chosen = self.denoise.models_dir.as_deref().and_then(|dir| {
            let id = self.denoise.settings.model.clone()?;
            installed_models(dir).into_iter().find(|i| i.manifest.id == id)
        });
        let found = chosen.map(|i| (i.fingerprint(), i));
        let same = match (&self.denoise.active, &found) {
            (None, None) => true,
            (Some(a), Some((fp, _))) => a.fingerprint == *fp,
            _ => false,
        };
        if same {
            return;
        }
        // another model: what was made with the old one is not this one's, and what failed may work now
        self.denoise.active = found.map(|(fingerprint, i)| Active {
            id: i.manifest.id.clone(),
            manifest: i.manifest,
            onnx: i.onnx,
            fingerprint,
            model: Arc::new(Mutex::new(None)),
        });
        if let Some(r) = self.denoise.running.take() {
            r.progress.cancel_now();
        }
        self.denoise.queue.clear();
        self.denoise.failed.clear();
        self.denoise.dirty = true;
    }

    fn denoise_spec(&self, p: &Photo) -> Option<(DenoiseSpec, String)> {
        let (dir, active) = (self.denoise_products_dir()?, self.denoise.active.as_ref()?);
        let key = product_key(&active.fingerprint, p);
        Some((DenoiseSpec { product: product_path(&dir, &key), key: key.clone(), make: None }, key))
    }

    /// Make the index of ready photos from the products on disk (a directory listing and a hash per photo: no file is
    /// opened). Pictures that appeared or vanished drop the decoded sources they affect.
    pub(crate) fn denoise_reindex(&mut self) {
        self.denoise.dirty = false;
        self.denoise.reindexed = Some((self.catalog.revision, web_time::Instant::now()));
        let mut now: HashMap<PhotoId, DenoiseSpec> = HashMap::new();
        if let (Some(dir), Some(active)) = (self.denoise_products_dir(), self.denoise.active.as_ref()) {
            let names: HashSet<String> =
                products_in(&dir).into_iter().filter_map(|(p, _, _)| p.file_stem().and_then(|s| s.to_str()).map(str::to_string)).collect();
            if !names.is_empty() {
                for p in self.catalog.photos().filter(|p| eligible(p)) {
                    let key = product_key(&active.fingerprint, p);
                    if names.contains(&key) {
                        now.insert(p.id, DenoiseSpec { product: product_path(&dir, &key), key, make: None });
                    }
                }
            }
        }
        let mut changed = false;
        for id in self.media.denoise.ids() {
            if !now.contains_key(&id) {
                self.media.denoise.remove(id);
                self.media.forget(id);
                changed = true;
            }
        }
        for (id, spec) in now {
            if self.media.denoise.spec(id).is_none_or(|old| old.key != spec.key) {
                self.media.denoise.set(id, spec);
                self.media.forget(id);
                changed = true;
            }
        }
        if changed {
            self.denoise.generation += 1;
        }
    }

    /// A photo whose file changed (its content key is not the one its index entry was made for) has no picture.
    fn denoise_check_photo(&mut self, id: PhotoId) {
        let Some(p) = self.catalog.photo(id).cloned() else { return };
        let Some((spec, _)) = self.denoise_spec(&p) else { return };
        match self.media.denoise.spec(id) {
            Some(have) if have.key != spec.key => {
                self.media.denoise.remove(id);
                self.media.forget(id);
                self.denoise.generation += 1;
            }
            None if spec.product.is_file() && product::is_current(&spec.product, &spec.key) => {
                // made by an export, or by another session
                self.media.denoise.set(id, spec);
                self.media.forget(id);
                self.denoise.generation += 1;
            }
            _ => {}
        }
    }

    /// How photo `id` stands.
    pub fn denoise_photo_state(&self, id: PhotoId) -> PhotoState {
        let Some(p) = self.catalog.photo(id) else { return PhotoState::NotApplicable };
        if !eligible(p) {
            return PhotoState::NotApplicable;
        }
        if self.denoise.active.is_none() {
            // the model folder has not been looked at yet (just started): not "no model" until it has been
            return if self.denoise.active_seen.is_none() { PhotoState::Idle } else { PhotoState::NoModel };
        }
        if self.denoise_products_dir().is_none() {
            return PhotoState::Failed {
                why: "AI Denoise keeps its pictures in the library folder, and this session has none.".into(),
                unsupported: true,
            };
        }
        if self.media.denoise.spec(id).is_some() {
            return PhotoState::Ready;
        }
        if let Some(r) = self.denoise.running.as_ref().filter(|r| r.photo == id) {
            return PhotoState::Running { done: r.progress.done.load(Ordering::Relaxed), total: r.progress.total.load(Ordering::Relaxed) };
        }
        if let Some(ahead) = self.denoise.queue.iter().position(|q| *q == id) {
            return PhotoState::Queued { ahead: ahead + usize::from(self.denoise.running.is_some()) };
        }
        let key = self.denoise_spec(p).map(|s| s.1);
        match self.denoise.failed.get(&id) {
            Some(f) if Some(&f.key) == key.as_ref() => PhotoState::Failed { why: f.why.clone(), unsupported: f.unsupported },
            _ => PhotoState::Idle,
        }
    }

    /// Ask for photos' pictures to be made (the ones that are not made, queued or known to be impossible). With `retry`
    /// a photo that failed before is tried again. A photo that is not raw is skipped. Returns how many were queued.
    pub(crate) fn denoise_enqueue(&mut self, ids: &[PhotoId], front: bool, retry: bool) -> usize {
        self.denoise_refresh_active(false);
        if self.denoise.active.is_none() || self.denoise_products_dir().is_none() {
            return 0;
        }
        let mut added = 0;
        for id in ids {
            let Some(p) = self.catalog.photo(*id).cloned() else { continue };
            if !eligible(&p) || self.media.denoise.spec(*id).is_some() || self.denoise.queue.contains(id) {
                continue;
            }
            if self.denoise.running.as_ref().is_some_and(|r| r.photo == *id) || self.denoise.queue.len() >= MAX_QUEUE {
                continue;
            }
            let Some((_, key)) = self.denoise_spec(&p) else { continue };
            if retry {
                self.denoise.failed.remove(id);
            } else if self.denoise.failed.get(id).is_some_and(|f| f.key == key) {
                continue;
            }
            if front {
                self.denoise.queue.push_front(*id);
            } else {
                self.denoise.queue.push_back(*id);
            }
            added += 1;
        }
        added
    }

    /// Stop the job that is running, and with `all` forget everything that waits.
    pub(crate) fn denoise_cancel(&mut self, photo: Option<PhotoId>, all: bool) -> usize {
        let mut n = 0;
        if let Some(r) = &self.denoise.running
            && photo.is_none_or(|p| p == r.photo)
        {
            r.progress.cancel_now();
            n += 1;
        }
        let before = self.denoise.queue.len();
        match photo {
            Some(p) => self.denoise.queue.retain(|q| *q != p),
            None if all => self.denoise.queue.clear(),
            None => {}
        }
        n + before - self.denoise.queue.len()
    }

    /// The photos the automatic queue looks at: the one open and the ones selected, those with a Denoise amount.
    fn denoise_wanted(&self) -> Vec<PhotoId> {
        let Selection { active, ids, .. } = &self.selection;
        active
            .iter()
            .chain(ids.iter().take(AUTO_LOOK))
            .copied()
            .filter(|id| self.catalog.photo(*id).is_some_and(|p| p.develop.denoise_amount() > 0.0))
            .collect()
    }

    fn denoise_job_spec(&self, p: &Photo, parallel: usize) -> Option<JobSpec> {
        let Source::File { path } = &p.source else { return None };
        let (spec, _) = self.denoise_spec(p)?;
        let active = self.denoise.active.as_ref()?;
        Some(JobSpec {
            path: path.clone(),
            read: self.media.file_bytes.clone(),
            product: spec.product,
            key: spec.key,
            manifest: active.manifest.clone(),
            onnx: active.onnx.clone(),
            model: active.model.clone(),
            loader: self.denoise.loader.clone(),
            parallel,
            run_on: self.denoise.settings.run_on(),
        })
    }

    /// Start the next queued photo on its own thread.
    fn denoise_start_next(&mut self, parallel: usize) {
        while let Some(id) = self.denoise.queue.pop_front() {
            let Some(p) = self.catalog.photo(id).cloned() else { continue };
            if !eligible(&p) || self.media.denoise.spec(id).is_some() {
                continue;
            }
            let Some(spec) = self.denoise_job_spec(&p, parallel) else { continue };
            // another session or an export may have made it since it was queued
            if product::is_current(&spec.product, &spec.key) {
                self.denoise_check_photo(id);
                continue;
            }
            let progress = Arc::new(Progress::default());
            let outcome: Arc<Mutex<Option<Outcome>>> = Arc::new(Mutex::new(None));
            let (key, product) = (spec.key.clone(), spec.product.clone());
            let (worker_progress, worker_outcome) = (progress.clone(), outcome.clone());
            let started = std::thread::Builder::new().name("denoise".into()).stack_size(STACK).spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make_product(&spec, Some(&worker_progress))));
                let o = match result {
                    Ok(Ok(_)) => Outcome::Done,
                    Ok(Err(MakeError::Cancelled)) => Outcome::Cancelled,
                    Ok(Err(MakeError::Unsupported(why))) => Outcome::Failed(why, true),
                    Ok(Err(MakeError::Failed(why))) => Outcome::Failed(why, false),
                    Err(_) => Outcome::Failed("the denoiser stopped unexpectedly".into(), false),
                };
                *worker_outcome.lock().unwrap_or_else(PoisonError::into_inner) = Some(o);
            });
            match started {
                Ok(_) => {
                    self.denoise.running = Some(Running { photo: id, key, product, progress, outcome, started: web_time::Instant::now() });
                }
                Err(e) => {
                    self.denoise.failed.insert(id, Failure { key, why: format!("could not start the denoiser: {e}"), unsupported: false });
                    continue;
                }
            }
            return;
        }
    }

    /// Take in the running job's result.
    fn denoise_poll(&mut self) {
        let Some(r) = &self.denoise.running else { return };
        let Some(outcome) = r.outcome.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };
        let Some(r) = self.denoise.running.take() else { return };
        match outcome {
            Outcome::Done => {
                self.denoise.made += 1;
                self.denoise_check_photo(r.photo);
                self.denoise_evict(Some(&r.product));
            }
            Outcome::Failed(why, unsupported) => {
                self.denoise.failed.insert(r.photo, Failure { key: r.key, why, unsupported });
            }
            Outcome::Cancelled => {}
        }
        // the picture's state changed: the interface draws it again
        self.denoise.generation += 1;
    }

    /// Keep the cache within its limit: the pictures nobody asks for go first, the oldest first; `keep` (the one just
    /// made) never goes. The index forgets what was deleted.
    pub(crate) fn denoise_evict(&mut self, keep: Option<&Path>) {
        let Some(models) = self.denoise.models_dir.clone() else { return };
        self.denoise_evict_to(read_settings(&models).cache_bytes(), keep);
    }

    /// [`Self::denoise_evict`] for a limit of `limit` bytes.
    pub(crate) fn denoise_evict_to(&mut self, limit: u64, keep: Option<&Path>) {
        let Some(dir) = self.denoise_products_dir() else { return };
        let mut files = products_in(&dir);
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        if total <= limit {
            return;
        }
        // the pictures photos use now (a Denoise amount above 0) are the last to go
        let wanted: HashSet<PathBuf> = self
            .media
            .denoise
            .ids()
            .into_iter()
            .filter(|id| self.catalog.photo(*id).is_some_and(|p| p.develop.denoise_amount() > 0.0))
            .filter_map(|id| self.media.denoise.spec(id).map(|s| s.product.clone()))
            .collect();
        files.sort_by_key(|(path, _, modified)| (wanted.contains(path), *modified));
        let mut gone: HashSet<PathBuf> = HashSet::new();
        for (path, size, _) in files {
            if total <= limit {
                break;
            }
            if keep == Some(path.as_path()) {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
                gone.insert(path);
            }
        }
        if gone.is_empty() {
            return;
        }
        for id in self.media.denoise.ids() {
            if self.media.denoise.spec(id).is_some_and(|s| gone.contains(&s.product)) {
                self.media.denoise.remove(id);
                self.media.forget(id);
            }
        }
        self.denoise.generation += 1;
    }

    /// Delete every product of this library (they are made again when wanted).
    pub(crate) fn denoise_clear(&mut self) -> (usize, u64) {
        self.denoise_cancel(None, true);
        let (mut files, mut bytes) = (0, 0);
        if let Some(dir) = self.denoise_products_dir() {
            for (path, size, _) in products_in(&dir) {
                if std::fs::remove_file(&path).is_ok() {
                    files += 1;
                    bytes += size;
                }
            }
        }
        self.denoise.failed.clear();
        self.denoise.touch();
        (files, bytes)
    }

    /// Per frame: take in what finished, look at what the user is looking at, and keep one job going. Cheap when
    /// nothing is happening (no file is opened unless a photo with a Denoise amount lacks its picture).
    pub(crate) fn denoise_pump(&mut self, pace: Pace) {
        crate::cmd::denoise::finish_downloads(self);
        self.denoise_refresh_active(false);
        // photos that changed (a replaced file has another key) must not keep the picture of what they were
        let stale = !self.media.denoise.is_empty()
            && self.denoise.reindexed.is_none_or(|(rev, at)| rev != self.catalog.revision && at.elapsed() > SETTINGS_TTL);
        if self.denoise.dirty || stale {
            self.denoise_reindex();
        }
        self.denoise_poll();
        if self.denoise.active.is_none() || self.denoise_products_dir().is_none() {
            return;
        }
        let (cap, auto) = (self.denoise.settings.threads, self.denoise.settings.auto());
        let wanted = self.denoise_wanted();
        for id in &wanted {
            self.denoise_check_photo(*id);
        }
        if auto {
            // the photo being edited goes first
            let (open, others): (Vec<PhotoId>, Vec<PhotoId>) = wanted.into_iter().partition(|id| self.selection.active == Some(*id));
            self.denoise_enqueue(&others, false, false);
            self.denoise_enqueue(&open, true, false);
        }
        if self.denoise.running.is_none() && pace != Pace::Pause {
            let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
            self.denoise_start_next(pace.parallel(cores, cap));
        }
    }

    /// The statistics `denoise.status` reports besides the queue (reads the cache folder).
    pub(crate) fn denoise_cache_usage(&self) -> (usize, u64) {
        match self.denoise_products_dir() {
            Some(dir) => {
                let files = products_in(&dir);
                (files.len(), files.iter().map(|f| f.1).sum())
            }
            None => (0, 0),
        }
    }

    /// Are pictures made without being asked for photos being looked at (Settings ▸ AI Denoise)?
    pub fn denoise_auto(&self) -> bool {
        self.denoise.settings.auto()
    }

    /// Is a picture being made, or waiting for its turn with a model to make it? (Headless runs wait for this.)
    pub fn denoise_busy(&self) -> bool {
        self.denoise.running.is_some() || (!self.denoise.queue.is_empty() && self.denoise.active.is_some())
    }

    /// Keep AI Denoise's row in the activity stack up to date (each `denoise.pump`): there while a photo is being made or
    /// waits its turn, with the running photo's tiles and its file name; gone when the queue is idle. It has no ✕: what is
    /// made follows the photos' Denoise amounts and Settings ▸ AI Denoise.
    pub(crate) fn denoise_track(&mut self) {
        if !self.denoise_busy() {
            self.denoise.task = None;
            return;
        }
        let task = self.denoise.task.get_or_insert_with(|| {
            let t = self.activity.start("denoise", "AI Denoise", crate::activity::Cancel::No);
            t.set_unit(crate::activity::Unit::Percent);
            t
        });
        match &self.denoise.running {
            Some(r) => {
                task.progress(r.progress.done.load(Ordering::Relaxed) as u64, r.progress.total.load(Ordering::Relaxed) as u64);
                task.detail(self.catalog.photo(r.photo).map_or("", |p| p.file_name.as_str()));
            }
            None => {
                task.progress(0, 0);
                task.detail("");
            }
        }
    }

    /// Seconds the running job has been going, and its progress.
    pub(crate) fn denoise_running_json(&self) -> Value {
        match &self.denoise.running {
            Some(r) => json!({
                "photo": r.photo.0,
                "done": r.progress.done.load(Ordering::Relaxed),
                "total": r.progress.total.load(Ordering::Relaxed),
                "seconds": r.started.elapsed().as_secs_f64(),
            }),
            None => Value::Null,
        }
    }

    pub(crate) fn denoise_made(&self) -> u64 {
        self.denoise.made
    }

    /// What an export of photo `id` with `settings` needs for its Denoise amount: its denoised picture, made now when the
    /// queue has not got to it. `Ok(None)` when denoise does not apply (amount 0, not a Bayer raw), `Err` when it should
    /// have and cannot (no model chosen, nowhere to keep the picture, the model failing).
    pub(crate) fn denoise_for_export(&mut self, id: PhotoId, settings: &DevelopSettings) -> Result<Option<(DenoiseSpec, PairLoader)>, String> {
        if settings.denoise_amount() <= 0.0 {
            return Ok(None);
        }
        let Some(p) = self.catalog.photo(id).cloned() else { return Ok(None) };
        let Some(loader) = self.media.denoise.loader.clone() else { return Ok(None) };
        if !eligible(&p) {
            return Ok(None);
        }
        self.denoise_refresh_active(false);
        let name = &p.file_name;
        if self.denoise.active.is_none() {
            return Err(format!("{name}: Denoise is on for this photo but no denoise model is chosen; choose one in Settings, or set Denoise to 0"));
        }
        if self.denoise_products_dir().is_none() {
            return Err(format!("{name}: Denoise needs a library on disk to keep its pictures in"));
        }
        self.denoise_check_photo(id);
        let Some(job) = self.denoise_job_spec(&p, Pace::Full.parallel(std::thread::available_parallelism().map_or(1, |n| n.get()), None)) else {
            return Ok(None);
        };
        let label = name.clone();
        let spec = job.clone();
        let make: MakeProduct = Arc::new(move || match make_product(&spec, None) {
            Ok(_) => Ok(true),
            Err(MakeError::Unsupported(_)) => Ok(false),
            Err(MakeError::Failed(why)) => Err(format!("{label}: could not denoise: {why}")),
            Err(MakeError::Cancelled) => Err(format!("{label}: denoise was cancelled")),
        });
        Ok(Some((DenoiseSpec { product: job.product, key: job.key, make: Some(make) }, loader)))
    }
}

#[cfg(all(test, feature = "denoise"))]
mod gpu_tests {
    use super::*;
    use dac_denoise::manifest::{Domain, Gain};

    /// A folder of the test's own: the card's set-up leaves its marker beside the model.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-engine-gpu-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The model file is read again when the card is set up (for the first photo), so it stays until the test ends.
    fn model(name: &str, tile: u64) -> (Arc<dyn Model>, dac_denoise::runtime::CpuRunner, PathBuf) {
        let path = folder(name).join("model.onnx");
        std::fs::write(&path, dac_denoise::synthetic::unet_onnx(tile, 8, 2, 7)).unwrap();
        let manifest = DenoiserManifest {
            id: "synthetic".into(),
            name: "Synthetic".into(),
            version: "1".into(),
            licence: Default::default(),
            source: None,
            sha256: None,
            size_bytes: None,
            provenance: String::new(),
            domain: Domain::BayerToRgb,
            tile: tile as u32,
            overlap: 16,
            gain: Gain::MatchMean { nominal: 1.0, max_deviation: 0.05 },
        };
        let loaded = default_loader()(&path, &manifest).unwrap();
        let cpu = dac_denoise::runtime::CpuRunner::load(&path, &manifest).unwrap();
        (loaded, cpu, path)
    }

    fn worst(want: &[f32], got: &[f32]) -> f32 {
        assert_eq!(want.len(), got.len());
        let scale = want.iter().fold(1e-9f32, |m, v| m.max(v.abs()));
        want.iter().zip(got).fold(0f32, |m, (a, b)| m.max((a - b).abs())) / scale
    }

    #[test]
    fn a_tile_on_the_card_is_the_cpus_tile() {
        let (model, cpu, path) = model("same", 64);
        let (input, _) = dac_denoise::runtime::test_tile(64);
        let want = cpu.run(&input).unwrap();
        let (runner, at_once) = model.runner(RunOn::Gpu, 16);
        assert!(worst(&want, &runner.run(&input).unwrap()) < 1e-3);
        let device = model.device(RunOn::Gpu);
        eprintln!("device: {device}");
        match device["kind"].as_str() {
            Some("gpu") => {
                assert!(device["adapter"].as_str().is_some_and(|a| !a.is_empty()));
                assert!(at_once <= GPU_PARALLEL, "a card is fed by a few tiles at once");
                assert!(device["cardMs"].as_f64().is_some_and(|ms| ms > 0.0) && device["cpuMs"].as_f64().is_some_and(|ms| ms > 0.0), "{device}");
            }
            Some("cpu") => {
                assert!(device["reason"].as_str().is_some_and(|r| !r.is_empty()), "the CPU is only used for a reason: {device}");
                assert_eq!(at_once, 16);
            }
            other => panic!("the first photo decides where the work runs, not {other:?}"),
        }
        assert!(!path.with_file_name(GPU_SETUP_MARKER).exists(), "a set-up that ended leaves no marker");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn with_the_processor_chosen_everything_runs_on_the_cpu() {
        let (model, cpu, path) = model("cpu", 64);
        let (input, _) = dac_denoise::runtime::test_tile(64);
        let (runner, at_once) = model.runner(RunOn::Cpu, 16);
        assert_eq!(runner.run(&input).unwrap(), cpu.run(&input).unwrap(), "the CPU path is the CPU runner, bit for bit");
        assert_eq!(at_once, 16, "the threads asked for are the threads used");
        assert_eq!(model.device(RunOn::Cpu)["kind"], "cpu");
        assert_eq!(model.device(RunOn::Auto)["kind"], "pending", "the card was not even set up");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_card_is_used_where_it_is_faster_than_the_processor() {
        // measured: a tile alone on one CPU thread 1.4 s; an RTX 4090 Laptop 13 ms; a Radeon 610M 490 ms
        for threads in [1, 2, 8, 16, 64] {
            assert!(card_is_faster(13.0, 1400.0, threads), "a big card wins at {threads}");
        }
        assert!(card_is_faster(490.0, 1400.0, 2), "a small card beats the two threads background work gets");
        assert!(!card_is_faster(490.0, 1400.0, 16), "and loses to sixteen");
        assert!(!card_is_faster(f64::INFINITY, 1400.0, 1) && !card_is_faster(f64::NAN, 1400.0, 1), "a card that never ran is never faster");
        assert!(!card_is_faster(10.0, 0.0, 1));
    }

    /// A card that gives numbers that are not finite.
    struct Garbage(AtomicUsize);

    impl TileRunner for Garbage {
        fn run(&self, input: &[f32]) -> Result<Vec<f32>, RunError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            let n = input.len() / 4 * 12;
            Ok(vec![f32::NAN; n])
        }
    }

    #[test]
    fn a_tile_the_card_gets_wrong_is_run_on_the_processor_and_three_stop_the_card() {
        let (_, cpu, path) = model("garbage", 64);
        let card = GpuSide { runner: Box::new(Garbage(AtomicUsize::new(0))), adapter: "Test card".into(), card_ms: 1.0, cpu_ms: 1000.0 };
        let model = Network {
            cpu: cpu.clone(),
            path: path.clone(),
            tile: 32,
            gpu: OnceLock::from(Ok(card)),
            fallbacks: AtomicUsize::new(0),
            last_threads: AtomicUsize::new(0),
        };
        let (input, _) = dac_denoise::runtime::test_tile(64);
        let want = cpu.run(&input).unwrap();
        let (runner, at_once) = model.runner(RunOn::Auto, 8);
        assert_eq!(at_once, GPU_PARALLEL, "the (fast) card is chosen");
        for _ in 0..GPU_MAX_FALLBACKS {
            assert_eq!(runner.run(&input).unwrap(), want, "a wrong tile is the CPU's tile");
        }
        let device = model.device(RunOn::Auto);
        assert_eq!(device["kind"], "cpu", "{device}");
        assert!(device["reason"].as_str().unwrap().contains("tiles wrong"), "{device}");
        assert_eq!(device["fellBack"], GPU_MAX_FALLBACKS);
        let (_, at_once) = model.runner(RunOn::Gpu, 8);
        assert_eq!(at_once, 8, "the next photo runs on the CPU, even with the card chosen");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_slow_card_is_left_out_unless_it_is_chosen() {
        let (_, cpu, path) = model("slow", 64);
        let slow = GpuSide { runner: Box::new(cpu.clone()), adapter: "Slow card".into(), card_ms: 490.0, cpu_ms: 1400.0 };
        let model = Network {
            cpu,
            path: path.clone(),
            tile: 32,
            gpu: OnceLock::from(Ok(slow)),
            fallbacks: AtomicUsize::new(0),
            last_threads: AtomicUsize::new(0),
        };
        assert_eq!(model.runner(RunOn::Auto, 16).1, 16, "sixteen threads beat it");
        let device = model.device(RunOn::Auto);
        assert_eq!((device["kind"].as_str(), device["reason"].as_str()), (Some("cpu"), Some("it is faster than Slow card on this computer")));
        assert_eq!(device["cardMs"], 490.0, "the times measured are reported");
        assert_eq!(device["retry"], false, "trying again would not make it faster");
        assert_eq!(model.runner(RunOn::Auto, 2).1, GPU_PARALLEL.min(2), "two threads do not");
        assert_eq!(model.device(RunOn::Auto)["kind"], "gpu");
        assert_eq!(model.runner(RunOn::Gpu, 16).1, GPU_PARALLEL, "chosen, it is used anyway");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_set_up_marker_left_behind_keeps_the_card_off() {
        let (model, _, path) = model("crashed", 64);
        let marker = path.with_file_name(GPU_SETUP_MARKER);
        std::fs::write(&marker, "setting up the graphics card for AI Denoise (unix time 0)").unwrap();
        let (_, at_once) = model.runner(RunOn::Gpu, 4);
        assert_eq!(at_once, 4, "the CPU does the work");
        let device = model.device(RunOn::Gpu);
        assert_eq!((device["kind"].as_str(), device["reason"].as_str()), (Some("cpu"), Some(GPU_CRASHED)), "{device}");
        assert_eq!(device["retry"], true, "the interface offers to try again");
        assert!(marker.exists(), "it stays until the user chooses where denoise runs");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_set_up_runs_beside_the_cpu_and_removes_its_marker() {
        let dir = folder("setup");
        let marker = dir.join(GPU_SETUP_MARKER);
        let seen = Arc::new(AtomicBool::new(false));
        let (m, s) = (marker.clone(), seen.clone());
        let r = set_up_card(
            &marker,
            Duration::from_secs(10),
            move || {
                s.store(m.exists(), Ordering::Relaxed);
                Ok(7)
            },
            || "cpu",
        );
        assert_eq!(r, Ok((7, "cpu")));
        assert!(seen.load(Ordering::Relaxed), "the marker is there while the card is set up");
        assert!(!marker.exists(), "and gone after");
        // a set-up that fails or panics removes it too
        assert_eq!(set_up_card(&marker, Duration::from_secs(10), || Err::<(), _>("no card".into()), || ()), Err("no card".into()));
        assert!(!marker.exists());
        let r = set_up_card(&marker, Duration::from_secs(10), || -> Result<(), String> { panic!("driver") }, || ());
        assert!(r.is_err() && !marker.exists(), "{r:?}");
        // a marker found before the set-up starts means the last one took the app down: the card is not touched
        std::fs::write(&marker, "left behind").unwrap();
        let touched = Arc::new(AtomicBool::new(false));
        let t = touched.clone();
        let r = set_up_card(
            &marker,
            Duration::from_secs(10),
            move || {
                t.store(true, Ordering::Relaxed);
                Ok(())
            },
            || (),
        );
        assert_eq!(r, Err(GPU_CRASHED.to_string()));
        assert!(!touched.load(Ordering::Relaxed) && marker.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_set_up_that_takes_too_long_is_given_up_on() {
        let dir = folder("slowsetup");
        let marker = dir.join(GPU_SETUP_MARKER);
        let started = web_time::Instant::now();
        let slow = || -> Result<(), String> {
            std::thread::sleep(Duration::from_millis(600));
            Ok(())
        };
        let r = set_up_card(&marker, Duration::from_millis(50), slow, || ());
        assert!(r.as_ref().is_err_and(|e| e.contains("took longer") && worth_retrying(e)), "{r:?}");
        assert!(started.elapsed() < Duration::from_millis(500), "the caller does not wait for the card");
        // while the first is still stuck, another set-up of the same model does not start a second one
        let again = set_up_card(&marker, Duration::from_secs(5), || Ok(()), || ());
        assert!(again.is_err_and(|e| e.contains("still setting up") && worth_retrying(&e)));
        // once it ends its marker goes, and the card can be set up again
        while marker.exists() {
            assert!(started.elapsed() < Duration::from_secs(10), "the stuck set-up ended");
            std::thread::sleep(Duration::from_millis(20));
        }
        while SETTING_UP.lock().unwrap().iter().any(|m| m == &marker) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(set_up_card(&marker, Duration::from_secs(5), || Ok(1), || 2), Ok((1, 2)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
