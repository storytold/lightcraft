//! The session's side of the shared AI backend (`lcc_ml`, issue #491): its one job queue, the GPU probe it uses (the
//! same switches as GPU rendering: `{ENV_PREFIX}_GPU`, `{ENV_PREFIX}_GPU_BACKEND`, `app.gpu`), its batch row in the
//! activity stack ("Indexing photos · 37 of 200", kind `mlIndex`; its ✕ cancels the batch), model installation and
//! downloads (`ml.models.*`), the per-library embedding store (`ml-embeddings-<id>.bin`), the background indexing pump
//! (`ml.pump`, `ml.index`), and semantic search (`search.semantic`).
//!
//! `{ENV_PREFIX}_ML_DEVICE=cpu` keeps the AI jobs on the CPU (`auto`, the default, takes the GPU when there is one);
//! `ml.status {preference}` changes it for the session. See `docs/ml-backend.md`.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lcc_catalog::{Photo, PhotoId};
use lcc_develop::DevelopSettings;
use lcc_ml::{
    CLIP_VIT_B32_ID, Config, Ctx, Device, Download, Encoder, Job, JobError, ModelManifest, Outcome, Preference, Priority, Probe, Queue, RgbImage,
    Role, Row, Rows, Status, Store, SubmitError, fingerprint, store_file_name, valid_id,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Session;
use crate::activity::{Activity, Cancel, TaskGuard, TaskHandle, Unit};
use crate::cmd::bad;

/// What a job hands back; its owner (known by the job's key) downcasts it.
pub(crate) type Output = Box<dyn Any + Send>;

/// The activity row's kind and label.
pub(crate) const ROW_KIND: &str = "mlIndex";
pub(crate) const ROW_LABEL: &str = "Indexing photos";

/// The command that pauses search indexing, the one that resumes it, and where its options are (Settings ▸ AI Models ▸
/// Semantic search): the indexing row's pause button and gear.
fn row_controls(paused: bool) -> crate::activity::Controls {
    crate::activity::Controls { paused, pause: "ml.pause", resume: "ml.resume", settings_tab: "ai", settings_section: Some("semanticSearch") }
}

/// Staging folder inside `ml_models_dir` for in-progress model downloads.
pub const STAGING_DIR: &str = ".downloads";

/// Settings file inside `ml_models_dir` recording the chosen models.
const SETTINGS_FILE: &str = "settings.json";

/// Long edge of the preview decoded for CLIP embedding (more than 2× the 224 px input).
const EMBED_PREVIEW_EDGE: usize = 512;

/// Minimum embedded raw preview edge accepted without running the develop pipeline.
const MIN_RAW_PREVIEW_EDGE: usize = 224;

/// How often dirty embeddings are flushed to disk while a batch is running.
const SAVE_EVERY: Duration = Duration::from_secs(3);

/// How long the cached model check is reused by per-frame `ml.pump`.
const MODEL_RECHECK: Duration = Duration::from_secs(1);

/// Rest between photos when `ml.pump` runs at `light` pace.
const LIGHT_REST: Duration = Duration::from_millis(25);

/// Poll interval when `ml.pump` pauses background work.
const PAUSE_POLL: Duration = Duration::from_millis(25);

/// Default `topK` when semantic search filters the grid (`library.filter {semantic}` or `search.semantic {filter: true}`).
pub const DEFAULT_FILTER_TOP_K: usize = 200;

/// Default cosine-similarity threshold when semantic search filters the grid (`library.filter {semantic}` or `search.semantic {filter: true}`).
pub const DEFAULT_FILTER_THRESHOLD: f32 = 0.25;

/// Cached text query embedding for an active semantic filter so `drain_ml_results` can refresh
/// `Session::filter.only` as new photo embeddings finish in the background.
#[derive(Clone, Debug)]
pub(crate) struct CachedQuery {
    pub text: String,
    pub tag: String,
    pub vec: Vec<f32>,
    pub top_k: usize,
    pub threshold: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Pace {
    Pause = 0,
    Light = 1,
    Normal = 2,
    Full = 3,
}

impl Pace {
    pub fn parse(name: Option<&str>) -> Self {
        match name {
            Some("pause") => Self::Pause,
            Some("light") => Self::Light,
            Some("full") => Self::Full,
            _ => Self::Normal,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Pause,
            1 => Self::Light,
            3 => Self::Full,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct MlSettings {
    embedder: Option<String>,
    device: Option<String>,
    /// Semantic search (and its background indexing) on or off (`ml.enable`); unset means on.
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    /// Background indexing paused by the user (`ml.pause`); unset means not paused.
    #[serde(skip_serializing_if = "Option::is_none")]
    paused: Option<bool>,
}

fn read_settings(dir: &Path) -> MlSettings {
    std::fs::read(dir.join(SETTINGS_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_settings(dir: &Path, settings: &MlSettings) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    let path = dir.join(SETTINGS_FILE);
    std::fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Output of a photo embedding job on [`Queue`].
struct EmbedOutput {
    epoch: u64,
    tag: String,
    photo_id: PhotoId,
    fingerprint: u64,
    embedding: Vec<f32>,
}

/// Output of a text query embedding job on [`Queue`].
struct QueryOutput(Vec<f32>);

/// State of an ML model download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadState {
    Running { bytes: u64, total: u64 },
    Installed,
    Failed(String),
    Cancelled,
}

struct DownloadShared {
    state: Mutex<DownloadState>,
    cancel: Arc<AtomicBool>,
    task: Option<TaskHandle>,
}

impl DownloadShared {
    fn set(&self, s: DownloadState) {
        if let (DownloadState::Running { bytes, total }, Some(task)) = (&s, &self.task) {
            task.progress(*bytes, *total);
        }
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = s;
    }

    fn get(&self) -> DownloadState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[derive(Default)]
pub struct Downloads {
    jobs: HashMap<String, Arc<DownloadShared>>,
}

impl Downloads {
    pub fn snapshot(&self) -> Vec<(String, DownloadState)> {
        let mut out: Vec<_> = self.jobs.iter().map(|(k, v)| (k.clone(), v.get())).filter(|(_, s)| *s != DownloadState::Cancelled).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn get(&self, id: &str) -> Option<DownloadState> {
        self.jobs.get(id).map(|j| j.get())
    }

    pub fn cancel(&mut self, id: &str) -> bool {
        let Some(job) = self.jobs.get(id) else {
            return false;
        };
        job.cancel.store(true, Ordering::Relaxed);
        if !matches!(job.get(), DownloadState::Running { .. }) {
            self.jobs.remove(id);
        }
        true
    }
}

impl Drop for Downloads {
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub(crate) struct State {
    /// Made on the first job (no thread, no GPU probe before).
    queue: Option<Queue<Output>>,
    /// Dedicated interactive queue for text queries (`rows: None`) so a search query never waits behind
    /// a running background photo embedding job on `queue` or flickers the `mlIndex` activity row.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    query_queue: Option<Queue<Output>>,
    /// Set by `ml.status {preference}` before the queue exists.
    preference: Option<Preference>,
    /// Chosen hardware device (`None`: auto, `Some("cpu")`: cpu, or adapter key), shared with the session's GPU probe.
    device_choice: Arc<Mutex<Option<String>>>,
    /// `ml_models_dir` for which saved `MlSettings.device` was already checked.
    device_adopted_dir: Option<PathBuf>,
    /// Cached `settings.json` for `ml_models_dir`, so `ml.status` does not read the disk on every frame.
    settings_cache: Option<(PathBuf, MlSettings)>,
    /// Cached installed manifests and active embedder id (`(dir, at, installed, active_id)`).
    models_cache: Option<(PathBuf, web_time::Instant, Vec<ModelManifest>, Option<String>)>,
    /// Whether background indexing is enabled (`ml.enable`); read it through `Session::ml_enabled`.
    pub enabled: bool,
    /// `ml_models_dir` whose saved `MlSettings.enabled` was already adopted.
    enabled_adopted_dir: Option<PathBuf>,
    /// Set when the user clicks ✕ on the `mlIndex` activity row; cleared on library change, import, or `ml.index`.
    pub paused_by_user: bool,
    /// Background indexing paused (`ml.pause`, the row's pause button), saved in the models folder so it stays paused
    /// across restarts until `ml.resume`. Adopted with `enabled` ([`Session::ml_enabled`]).
    pub paused: bool,
    /// The batch row the queue shows while it has work, to give it pause / resume (it is made on the worker's side).
    row: Arc<Mutex<Option<TaskHandle>>>,
    /// The row shown while indexing is paused with photos left and nothing queued (after a restart, say), so the
    /// row's resume button is there.
    paused_row: Option<TaskGuard>,
    /// Bumped whenever the library or active model changes so stale in-flight results are ignored.
    pub epoch: u64,
    /// Current pace set by `ml.pump`.
    pace: Arc<AtomicU8>,
    /// Active or completed model downloads.
    pub downloads: Downloads,
    /// Loaded encoder (`(tag, encoder)`).
    #[cfg(not(target_arch = "wasm32"))]
    encoder: Option<(String, Arc<dyn Encoder>)>,
    /// Active model's embedding store.
    pub store: Option<Store>,
    /// Path to `ml-embeddings-<id>.bin` when the library is on disk.
    pub store_path: Option<PathBuf>,
    /// Photos currently waiting or running in `queue`.
    pub in_flight: HashSet<PhotoId>,
    /// Photos `(id, fingerprint)` whose decode/inference failed this session.
    pub failed_photos: HashSet<(u64, u64)>,
    /// Last time `store` was saved to disk.
    last_saved: Option<web_time::Instant>,
    /// Last time `ml_models_dir` was checked by `ml.pump`.
    last_checked: Option<web_time::Instant>,
    /// Monotonic counter for interactive text query keys.
    query_seq: u64,
    /// Cached text query embedding for live semantic filter updates.
    pub(crate) last_query: Option<CachedQuery>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            queue: None,
            query_queue: None,
            preference: None,
            device_choice: Arc::new(Mutex::new(None)),
            device_adopted_dir: None,
            settings_cache: None,
            models_cache: None,
            enabled: true,
            enabled_adopted_dir: None,
            paused_by_user: false,
            paused: false,
            row: Arc::new(Mutex::new(None)),
            paused_row: None,
            epoch: 1,
            pace: Arc::new(AtomicU8::new(Pace::Normal as u8)),
            downloads: Downloads::default(),
            #[cfg(not(target_arch = "wasm32"))]
            encoder: None,
            store: None,
            store_path: None,
            in_flight: HashSet::new(),
            failed_photos: HashSet::new(),
            last_saved: None,
            last_checked: None,
            query_seq: 0,
            last_query: None,
        }
    }
}

impl State {
    pub(crate) fn chosen_device(&self) -> Option<String> {
        self.device_choice.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub(crate) fn set_chosen_device(&mut self, choice: Option<String>) {
        let mut guard = self.device_choice.lock().unwrap_or_else(PoisonError::into_inner);
        if *guard != choice {
            *guard = choice;
            drop(guard);
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.encoder = None;
            }
        }
    }

    fn cached_settings(&mut self, dir: &Path) -> MlSettings {
        if let Some((cached_dir, settings)) = &self.settings_cache
            && cached_dir == dir
        {
            return settings.clone();
        }
        let settings = read_settings(dir);
        self.settings_cache = Some((dir.to_path_buf(), settings.clone()));
        settings
    }

    fn save_settings(&mut self, dir: &Path, settings: MlSettings) -> Result<(), String> {
        write_settings(dir, &settings)?;
        self.settings_cache = Some((dir.to_path_buf(), settings));
        self.models_cache = None;
        Ok(())
    }

    fn cached_models(&mut self, dir: &Path) -> (Vec<ModelManifest>, Option<String>) {
        let now = web_time::Instant::now();
        if let Some((cached_dir, at, installed, active)) = &self.models_cache
            && cached_dir == dir
            && now.saturating_duration_since(*at) < MODEL_RECHECK
        {
            return (installed.clone(), active.clone());
        }
        let installed = installed_manifests(dir);
        let settings = self.cached_settings(dir);
        let active = resolve_active_embedder(&installed, &settings);
        self.models_cache = Some((dir.to_path_buf(), now, installed.clone(), active.clone()));
        (installed, active)
    }

    /// Flush dirty embeddings and reset per-library indexing state when opening or closing a library.
    pub fn library_changed(&mut self) {
        self.save_store_now();
        if let Some(q) = &self.queue {
            for id in self.in_flight.drain() {
                q.cancel(&format!("embed:{}", id.0));
            }
        } else {
            self.in_flight.clear();
        }
        self.failed_photos.clear();
        self.paused_by_user = false;
        self.paused_row = None;
        self.store = None;
        self.store_path = None;
        self.last_checked = None;
        self.last_query = None;
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Forget a photo's cached embedding when its file on disk was replaced (`Op::SetContent`).
    pub fn content_replaced(&mut self, id: PhotoId) {
        if let Some(store) = &mut self.store {
            store.remove(id.0);
        }
        if let Some(q) = &self.queue {
            q.cancel(&format!("embed:{}", id.0));
        }
        self.in_flight.remove(&id);
        self.failed_photos.retain(|&(pid, _)| pid != id.0);
        self.paused_by_user = false;
    }

    /// Wake background indexing when new photos are added to the library.
    pub fn photo_added(&mut self) {
        self.paused_by_user = false;
    }

    /// Save the embedding store immediately if it is dirty and backed by a file on disk.
    pub fn save_store_now(&mut self) {
        if let (Some(store), Some(path)) = (&mut self.store, &self.store_path)
            && store.is_dirty()
        {
            if let Err(e) = store.save_atomic(path) {
                log::warn!("cannot save ML embedding store {}: {e}", path.display());
            } else {
                self.last_saved = Some(web_time::Instant::now());
            }
        }
    }
}

/// The preference from `{ENV_PREFIX}_ML_DEVICE` (`auto` when unset or not understood).
fn env_preference() -> Preference {
    lcc_brand::env("ML_DEVICE").and_then(|v| Preference::parse(&v.to_lowercase())).unwrap_or_default()
}

/// Write a tiny deterministic synthetic CLIP model directory at `dir` (for tests across crates).
#[cfg(not(target_arch = "wasm32"))]
pub fn write_synthetic_clip_dir(dir: &std::path::Path, id: &str) -> crate::Result<()> {
    lcc_ml::Clip::write_synthetic_dir(dir, id).map(|_| ()).map_err(|e| crate::EngineError::Other(e.to_string()))
}

/// The GPU the AI jobs may use, or why not: the rendering GPU's switches apply.
fn gpu_probe_for(target_card: Option<String>) -> Result<String, String> {
    if let Some(why) = lcc_gpu::env_off_reason() {
        return Err(why);
    }
    if !lcc_gpu::enabled() {
        return Err("disabled by the GPU rendering preference (app.gpu)".into());
    }
    if let Some(ref pref) = target_card
        && pref.eq_ignore_ascii_case("cpu")
    {
        return Err("the AI device preference is the CPU".into());
    }
    // the device CLIP's wgpu kernels run on (Vulkan, DX12 or Metal: the same on every OS)
    #[cfg(not(target_arch = "wasm32"))]
    return lcc_gpu::clip::adapter_for(target_card.as_deref());
    #[cfg(target_arch = "wasm32")]
    lcc_gpu::adapter_name().ok_or_else(|| lcc_gpu::unavailable_reason().unwrap_or_else(|| "no usable GPU".into()))
}

fn make_gpu_probe(device_choice: Arc<Mutex<Option<String>>>) -> Probe {
    Box::new(move || {
        let target = device_choice.lock().unwrap_or_else(PoisonError::into_inner).clone();
        gpu_probe_for(target)
    })
}

/// CLIP's transformer blocks on the GPU through `lcc-gpu`'s wgpu kernels (`lcc_ml` checks them against the CPU first).
#[cfg(test)]
pub(crate) fn clip_accelerator() -> lcc_ml::AccelFactory {
    clip_accelerator_for(Arc::new(Mutex::new(None)))
}

/// CLIP's transformer blocks on the GPU through `lcc-gpu`'s wgpu kernels, bound to the session's `device_choice`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn clip_accelerator_for(device_choice: Arc<Mutex<Option<String>>>) -> lcc_ml::AccelFactory {
    struct Tower(lcc_gpu::clip::ClipTower);
    impl lcc_ml::Accelerator for Tower {
        fn device(&self) -> String {
            self.0.device()
        }
        fn run_blocks(&self, x: &[f32], batch: usize, seq: usize) -> Result<Vec<f32>, String> {
            self.0.run(x, batch, seq)
        }
    }
    Arc::new(move |w: &lcc_ml::TowerWeights| {
        let spec = lcc_gpu::clip::TowerSpec { hidden: w.hidden, intermediate: w.intermediate, heads: w.heads, causal: w.causal, eps: w.eps };
        let blocks: Vec<lcc_gpu::clip::BlockRef<'_>> = w
            .blocks
            .iter()
            .map(|b| lcc_gpu::clip::BlockRef {
                ln1_w: &b.ln1_w,
                ln1_b: &b.ln1_b,
                q_w: &b.q_w,
                q_b: &b.q_b,
                k_w: &b.k_w,
                k_b: &b.k_b,
                v_w: &b.v_w,
                v_b: &b.v_b,
                o_w: &b.o_w,
                o_b: &b.o_b,
                ln2_w: &b.ln2_w,
                ln2_b: &b.ln2_b,
                fc1_w: &b.fc1_w,
                fc1_b: &b.fc1_b,
                fc2_w: &b.fc2_w,
                fc2_b: &b.fc2_b,
            })
            .collect();
        let target = device_choice.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let tower = lcc_gpu::clip::tower_for(target.as_deref(), spec, &blocks)?;
        Ok(Arc::new(Tower(tower)) as Arc<dyn lcc_ml::Accelerator>)
    })
}

struct ActivityRows {
    activity: Activity,
    /// The row now shown, for `ml.pump` to keep its pause / resume up to date.
    current: Arc<Mutex<Option<TaskHandle>>>,
}

struct ActivityRow(TaskGuard);

impl Rows for ActivityRows {
    fn begin(&self, cancel: Arc<AtomicBool>) -> Box<dyn Row> {
        let guard = self.activity.start_batch(ROW_KIND, ROW_LABEL, Cancel::Flag(cancel));
        guard.set_controls(Some(row_controls(false)));
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Some(guard.handle());
        Box::new(ActivityRow(guard))
    }
}

impl Row for ActivityRow {
    fn progress(&self, done: u64, total: u64) {
        self.0.progress(done, total);
    }
    fn detail(&self, name: &str) {
        self.0.detail(name);
    }
    fn item_done(&self) {
        self.0.item_done();
    }
    fn item_failed(&self, why: &str) {
        self.0.item_failed(why);
    }
}

fn device_json(d: &Device) -> Value {
    json!({"kind": d.kind.as_str(), "name": d.name, "whyCpu": d.why_cpu})
}

pub(crate) fn status_json(st: &Status) -> Value {
    json!({
        "device": st.device.as_ref().map(device_json),
        "preference": st.preference.as_str(),
        "pending": st.pending,
        "running": st.running,
        "done": st.done,
        "failed": st.failed,
        "cancelled": st.cancelled,
        "threaded": st.threaded,
    })
}

fn photo_fingerprint(photo: &Photo) -> u64 {
    fingerprint(&crate::media::content_key(photo))
}

fn installed_manifests(models_dir: &Path) -> Vec<ModelManifest> {
    let Ok(rd) = std::fs::read_dir(models_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(dir_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !valid_id(dir_name) {
            continue;
        }
        if let Ok(m) = ModelManifest::read_dir(&path)
            && m.id == dir_name
            && m.files.iter().all(|f| path.join(&f.name).is_file())
        {
            out.push(m);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn resolve_active_embedder(installed: &[ModelManifest], settings: &MlSettings) -> Option<String> {
    if let Some(ref chosen) = settings.embedder
        && installed.iter().any(|m| &m.id == chosen && m.role == Role::Embedder)
    {
        return Some(chosen.clone());
    }
    if installed.iter().any(|m| m.id == CLIP_VIT_B32_ID && m.role == Role::Embedder) {
        return Some(CLIP_VIT_B32_ID.to_string());
    }
    installed.iter().find(|m| m.role == Role::Embedder).map(|m| m.id.clone())
}

fn active_embedder_id(models_dir: &Path) -> Option<String> {
    let installed = installed_manifests(models_dir);
    let settings = read_settings(models_dir);
    resolve_active_embedder(&installed, &settings)
}

fn manifest_json(m: &ModelManifest, installed: bool, selected: bool, dl: Option<&DownloadState>) -> Value {
    let dl_val = dl.map(|state| match state {
        DownloadState::Running { bytes, total } => {
            json!({"state": "running", "bytes": bytes, "total": total})
        }
        DownloadState::Installed => json!({"state": "installed"}),
        DownloadState::Failed(err) => json!({"state": "failed", "error": err}),
        DownloadState::Cancelled => json!({"state": "cancelled"}),
    });
    json!({
        "id": m.id,
        "name": m.name,
        "role": m.role.as_str(),
        "versionTag": m.version_tag,
        "embedDim": m.embed_dim,
        "sizeBytes": m.total_bytes(),
        "licence": {
            "spdx": m.licence.spdx,
            "commercial": m.licence.commercial.as_str(),
            "url": m.licence.url,
            "attribution": m.licence.attribution,
        },
        "notes": m.notes,
        "installed": installed,
        "selected": selected,
        "download": dl_val,
        "downloadHost": lcc_ml::known::download(&m.id).map(|_| "huggingface.co"),
    })
}

impl Session {
    /// Active installed semantic search model id, if any (`None` when none is installed).
    pub fn selected_ml_model(&self) -> Option<String> {
        let dir = self.ml_models_dir.as_deref()?;
        if let Some((cached_dir, at, _, active)) = &self.ml.models_cache
            && cached_dir == dir
            && web_time::Instant::now().saturating_duration_since(*at) < MODEL_RECHECK
        {
            return active.clone();
        }
        active_embedder_id(dir)
    }

    /// The queue, made on first use with the engine's GPU probe and activity row.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn ml_queue(&mut self) -> &Queue<Output> {
        let preference = self.ml.preference.unwrap_or_else(env_preference);
        let probe = make_gpu_probe(Arc::clone(&self.ml.device_choice));
        self.ml_queue_with(Config { preference, ..Config::default() }, probe)
    }

    /// The queue, made with `probe` and `config` if it doesn't exist yet (tests pick the GPU this way).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn ml_queue_with(&mut self, config: Config, probe: Probe) -> &Queue<Output> {
        let rows = ActivityRows { activity: self.activity.clone(), current: Arc::clone(&self.ml.row) };
        self.ml.queue.get_or_insert_with(|| Queue::new(config, probe, Some(Arc::new(rows) as Arc<dyn Rows>)))
    }

    /// Queue an AI job.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn ml_submit(&mut self, job: Job<Output>) -> Result<(), SubmitError> {
        self.ml_queue().submit(job)
    }

    /// Dedicated interactive queue for text queries (`rows: None`), made on first query.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn ml_query_queue(&mut self) -> &Queue<Output> {
        let preference = self.ml.preference.unwrap_or_else(env_preference);
        let probe = make_gpu_probe(Arc::clone(&self.ml.device_choice));
        self.ml.query_queue.get_or_insert_with(|| Queue::new(Config { preference, ..Config::default() }, probe, None))
    }

    /// Whether semantic search is switched on (Settings ▸ AI Models ▸ Semantic search, `ml.enable`): frontends hide
    /// their AI search button while it is off.
    pub fn semantic_search_on(&mut self) -> bool {
        self.ml_enabled()
    }

    /// Whether background indexing is on, adopting the choice saved in the models folder once per folder (#1133).
    pub(crate) fn ml_enabled(&mut self) -> bool {
        if let Some(dir) = self.ml_models_dir.clone()
            && self.ml.enabled_adopted_dir.as_deref() != Some(dir.as_path())
        {
            let saved = self.ml.cached_settings(&dir);
            if let Some(on) = saved.enabled {
                self.ml.enabled = on;
            }
            if let Some(paused) = saved.paused {
                self.ml.paused = paused;
            }
            self.ml.enabled_adopted_dir = Some(dir);
        }
        self.ml.enabled
    }

    /// `ml.status {preference?, device?}`.
    pub(crate) fn ml_status(&mut self, p: &Value) -> crate::Result<Value> {
        // Adopt saved device preference from MlSettings once per models dir if unset in memory and not pinned by environment.
        if let Some(models_dir) = self.ml_models_dir.clone()
            && self.ml.device_adopted_dir.as_deref() != Some(models_dir.as_path())
        {
            self.ml.device_adopted_dir = Some(models_dir.clone());
            if self.ml.chosen_device().is_none()
                && self.ml.preference.is_none()
                && env_preference() == Preference::Auto
                && let Some(saved) = self.ml.cached_settings(&models_dir).device
            {
                if saved.eq_ignore_ascii_case("cpu") {
                    self.ml.set_chosen_device(Some("cpu".to_string()));
                    self.ml.preference = Some(Preference::Cpu);
                    if let Some(q) = &self.ml.queue {
                        q.set_preference(Preference::Cpu);
                    }
                    if let Some(qq) = &self.ml.query_queue {
                        qq.set_preference(Preference::Cpu);
                    }
                }
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(parsed) = lcc_gpu::cards::AdapterKey::parse(&saved)
                    && lcc_gpu::adapters().iter().any(|a| a.usable && (a.key == parsed || a.key.same_model(&parsed)))
                {
                    self.ml.set_chosen_device(Some(saved));
                    self.ml.preference = Some(Preference::Auto);
                    if let Some(q) = &self.ml.queue {
                        q.set_preference(Preference::Auto);
                    }
                    if let Some(qq) = &self.ml.query_queue {
                        qq.set_preference(Preference::Auto);
                    }
                }
            }
        }

        if let Some(v) = p.get("device") {
            let dev_str = v.as_str().ok_or_else(|| bad("ml.status", "`device` must be a string"))?.trim();
            if dev_str.eq_ignore_ascii_case("cpu") {
                self.ml.set_chosen_device(Some("cpu".to_string()));
                self.ml.preference = Some(Preference::Cpu);
                if let Some(q) = &self.ml.queue {
                    q.set_preference(Preference::Cpu);
                }
                if let Some(qq) = &self.ml.query_queue {
                    qq.set_preference(Preference::Cpu);
                }
            } else if dev_str.is_empty() {
                // Empty string resets the saved device choice and restores the environment preference (`auto` when unset).
                let pref = env_preference();
                self.ml.set_chosen_device(None);
                self.ml.preference = None;
                if let Some(q) = &self.ml.queue {
                    q.set_preference(pref);
                }
                if let Some(qq) = &self.ml.query_queue {
                    qq.set_preference(pref);
                }
            } else if dev_str.eq_ignore_ascii_case("auto") {
                self.ml.set_chosen_device(None);
                self.ml.preference = Some(Preference::Auto);
                if let Some(q) = &self.ml.queue {
                    q.set_preference(Preference::Auto);
                }
                if let Some(qq) = &self.ml.query_queue {
                    qq.set_preference(Preference::Auto);
                }
            } else {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let parsed = lcc_gpu::cards::AdapterKey::parse(dev_str).ok_or_else(|| bad("ml.status", format!("unknown device `{dev_str}`")))?;
                    let adapters = lcc_gpu::adapters();
                    let entry = adapters
                        .iter()
                        .find(|a| a.key == parsed || a.key.same_model(&parsed))
                        .ok_or_else(|| bad("ml.status", format!("unknown graphics card `{dev_str}`")))?;
                    if !entry.usable {
                        let reason = entry.why_not.as_deref().unwrap_or("card is not usable");
                        return Err(bad("ml.status", format!("graphics card `{dev_str}` is not usable: {reason}")));
                    }
                    self.ml.set_chosen_device(Some(dev_str.to_string()));
                    self.ml.preference = Some(Preference::Auto);
                    if let Some(q) = &self.ml.queue {
                        q.set_preference(Preference::Auto);
                    }
                    if let Some(qq) = &self.ml.query_queue {
                        qq.set_preference(Preference::Auto);
                    }
                }
                #[cfg(target_arch = "wasm32")]
                {
                    return Err(bad("ml.status", "GPU devices are not supported on this platform"));
                }
            }

            // Write back to saved setting so the explicit `device` choice sticks across restarts.
            if let Some(models_dir) = self.ml_models_dir.clone() {
                let mut settings = self.ml.cached_settings(&models_dir);
                settings.device = if dev_str.is_empty() || dev_str.eq_ignore_ascii_case("auto") { None } else { Some(dev_str.to_string()) };
                let _ = self.ml.save_settings(&models_dir, settings);
            }
        } else if let Some(v) = p.get("preference") {
            // `preference` is session-only (does not write `settings.json`).
            let pref = v
                .as_str()
                .and_then(Preference::parse)
                .ok_or_else(|| bad("ml.status", "preference is `auto` (the GPU when there is one) or `cpu`"))?;
            self.ml.preference = Some(pref);
            if pref == Preference::Cpu {
                self.ml.set_chosen_device(Some("cpu".to_string()));
            } else if self.ml.chosen_device().as_deref() == Some("cpu") {
                self.ml.set_chosen_device(None);
            }
            if let Some(q) = &self.ml.queue {
                q.set_preference(pref);
            }
            if let Some(qq) = &self.ml.query_queue {
                qq.set_preference(pref);
            }
        }
        let mut base = match &self.ml.queue {
            Some(q) => status_json(&q.status()),
            None => status_json(&Status {
                device: None,
                preference: self.ml.preference.unwrap_or_else(env_preference),
                pending: 0,
                running: None,
                done: 0,
                failed: 0,
                cancelled: 0,
                threaded: false,
            }),
        };
        let model_id = self.ml_models_dir.clone().and_then(|d| self.ml.cached_models(&d).1);
        #[cfg(not(target_arch = "wasm32"))]
        if model_id.is_some() && self.ml.store.is_none() {
            let _ = self.ensure_ml_encoder(false);
        }
        let indexed = self.ml.store.as_ref().map_or(0, Store::len);
        let unindexed = self.unindexed_photo_count();
        #[cfg(not(target_arch = "wasm32"))]
        let gpu_devices: Vec<Value> = {
            let cards = lcc_gpu::adapters();
            cards
                .iter()
                .map(|a| {
                    let mut name = a.name.clone();
                    if cards.iter().filter(|c| c.key.same_model(&a.key)).count() > 1 {
                        name = format!("{name} #{}", a.key.index);
                    }
                    if a.drives_screen {
                        name = format!("{name} (screen)");
                    }
                    json!({
                        "id": a.key.to_string(),
                        "name": name,
                        "backend": a.backend,
                        "kind": "gpu",
                        "usable": a.usable,
                        "whyNot": a.why_not,
                    })
                })
                .collect()
        };
        #[cfg(target_arch = "wasm32")]
        let gpu_devices: Vec<Value> = Vec::new();

        let available_devices: Vec<Value> = std::iter::once(json!({
            "id": "auto",
            "name": "Automatic",
            "kind": "auto",
        }))
        .chain(std::iter::once(json!({
            "id": "cpu",
            "name": "CPU",
            "kind": "cpu",
        })))
        .chain(gpu_devices)
        .collect();

        let device_choice = self.ml.chosen_device();
        let progress = self.ml_progress()?;
        if let Some(obj) = base.as_object_mut() {
            obj.insert("enabled".into(), Value::Bool(self.ml_enabled()));
            obj.insert("paused".into(), progress["paused"].clone());
            obj.insert("state".into(), progress["state"].clone());
            obj.insert("modelId".into(), model_id.as_ref().map(|s| Value::String(s.clone())).unwrap_or(Value::Null));
            obj.insert("modelInstalled".into(), Value::Bool(model_id.is_some()));
            obj.insert("indexedPhotos".into(), json!(indexed));
            obj.insert("unindexedPhotos".into(), json!(unindexed));
            obj.insert("deviceChoice".into(), json!(device_choice.as_deref().unwrap_or("auto")));
            obj.insert("availableDevices".into(), Value::Array(available_devices));
        }
        Ok(base)
    }

    fn unindexed_photo_count(&self) -> usize {
        let Some(store) = &self.ml.store else {
            return self.catalog.photos().filter(|p| p.in_library()).count();
        };
        self.catalog.photos().filter(|p| p.in_library()).filter(|p| !store.has(p.id.0, photo_fingerprint(p))).count()
    }

    /// `ml.enable {enabled: bool}`.
    pub(crate) fn ml_enable(&mut self, p: &Value) -> crate::Result<Value> {
        const C: &str = "ml.enable";
        let enabled = p.get("enabled").and_then(Value::as_bool).ok_or_else(|| bad(C, "missing boolean `enabled`"))?;
        self.ml_enabled();
        self.ml.enabled = enabled;
        if let Some(dir) = self.ml_models_dir.clone() {
            let mut settings = self.ml.cached_settings(&dir);
            settings.enabled = Some(enabled);
            self.ml.save_settings(&dir, settings).map_err(|e| bad(C, e))?;
        }
        if enabled {
            self.ml.paused_by_user = false;
        } else {
            if let Some(q) = &self.ml.queue {
                for id in self.ml.in_flight.drain() {
                    q.cancel(&format!("embed:{}", id.0));
                }
            } else {
                self.ml.in_flight.clear();
            }
            self.ml.paused_row = None;
            // semantic search is off: a description filter on the grid goes with it
            if self.filter.semantic.is_some() {
                let _ = self.apply_semantic_filter(C, None, None, None);
                self.reconcile_view();
                self.save_view();
            }
        }
        Ok(json!({"enabled": self.ml.enabled}))
    }

    /// `ml.pause` / `ml.resume`: hold background indexing (photos being embedded wait; nothing new starts) or let it go
    /// on, remembered in the models folder so a paused index stays paused across restarts.
    pub(crate) fn ml_set_paused(&mut self, cmd: &'static str, paused: bool) -> crate::Result<Value> {
        self.ml_enabled();
        self.ml.paused = paused;
        if let Some(dir) = self.ml_models_dir.clone() {
            let mut settings = self.ml.cached_settings(&dir);
            settings.paused = Some(paused);
            self.ml.save_settings(&dir, settings).map_err(|e| bad(cmd, e))?;
        }
        if paused {
            self.ml.pace.store(Pace::Pause as u8, Ordering::Relaxed);
        } else {
            self.ml.paused_by_user = false;
            self.ml.paused_row = None;
        }
        if let Some(row) = self.ml.row.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            row.set_controls(Some(row_controls(paused)));
        }
        Ok(json!({"enabled": self.ml.enabled, "paused": self.ml.paused}))
    }

    /// Drop the library's search index: every photo's embedding (of every model), in memory and on disk. Photos being
    /// embedded finish, and what they bring back is dropped.
    fn ml_drop_index(&mut self) {
        self.ml.epoch = self.ml.epoch.wrapping_add(1);
        if let Some(store) = &mut self.ml.store {
            store.retain(|_| false);
        }
        self.ml.failed_photos.clear();
        self.ml.paused_row = None;
        if let Some(dir) = self.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone())
            && let Ok(entries) = std::fs::read_dir(&dir)
        {
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("ml-embeddings-") && name.ends_with(".bin") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        // the open model's file comes back empty
        self.ml.save_store_now();
        #[cfg(not(target_arch = "wasm32"))]
        self.refresh_semantic_filter_from_cache();
    }

    /// `ml.clearIndex`: drop the search index and pause indexing, so it does not start over at once.
    pub(crate) fn ml_clear_index(&mut self, _p: &Value) -> crate::Result<Value> {
        self.ml_set_paused("ml.clearIndex", true)?;
        self.ml_drop_index();
        self.ml_progress()
    }

    /// `ml.restart`: drop the search index and index the library again from the start.
    pub(crate) fn ml_restart(&mut self, _p: &Value) -> crate::Result<Value> {
        self.ml_set_paused("ml.restart", false)?;
        self.ml_drop_index();
        self.ml_progress()
    }

    /// Where search indexing stands: `off` (semantic search switched off), `paused`, `indexing` (photos left), `complete`
    /// (none left) or `idle` (no model installed, or not looked at yet), with `done` of `total` photos.
    pub(crate) fn ml_progress(&mut self) -> crate::Result<Value> {
        let enabled = self.ml_enabled();
        let indexed = self.ml.store.as_ref().map_or(0, Store::len);
        let unindexed = self.unindexed_photo_count();
        let ready = self.ml.store.is_some() && self.selected_ml_model().is_some();
        let state = if !enabled {
            "off"
        } else if self.ml.paused || self.ml.paused_by_user {
            "paused"
        } else if !ready {
            "idle"
        } else if unindexed > 0 {
            "indexing"
        } else {
            "complete"
        };
        Ok(json!({
            "enabled": enabled,
            "paused": self.ml.paused || self.ml.paused_by_user,
            "state": state,
            "done": indexed,
            "total": indexed + unindexed,
            "indexedPhotos": indexed,
            "unindexedPhotos": unindexed,
        }))
    }

    /// `ml.models.list {}`.
    pub(crate) fn ml_models_list(&mut self, _p: &Value) -> crate::Result<Value> {
        if self.ml.downloads.snapshot().iter().any(|(_, state)| *state == DownloadState::Installed) {
            self.ml.models_cache = None;
            self.ml.settings_cache = None;
        }
        let (installed, selected) = match self.ml_models_dir.clone() {
            Some(dir) => self.ml.cached_models(&dir),
            None => (Vec::new(), None),
        };
        let mut models = Vec::new();
        let mut seen = HashSet::new();

        for known in lcc_ml::known::all() {
            let is_installed = installed.iter().any(|m| m.id == known.id);
            let is_selected = selected.as_deref() == Some(known.id.as_str());
            let dl = self.ml.downloads.get(&known.id);
            models.push(manifest_json(&known, is_installed, is_selected, dl.as_ref()));
            seen.insert(known.id);
        }
        for inst in installed {
            if seen.insert(inst.id.clone()) {
                let is_selected = selected.as_deref() == Some(inst.id.as_str());
                let dl = self.ml.downloads.get(&inst.id);
                models.push(manifest_json(&inst, true, is_selected, dl.as_ref()));
            }
        }
        Ok(json!({
            "dir": self.ml_models_dir.as_ref().map(|d| d.display().to_string()),
            "enabled": self.ml_enabled(),
            "runtime": cfg!(not(target_arch = "wasm32")),
            "models": models,
            "selectedEmbedder": selected,
        }))
    }

    /// `ml.models.install {path, acknowledged: true, select?: true}`.
    pub(crate) fn ml_models_install(&mut self, p: &Value) -> crate::Result<Value> {
        const C: &str = "ml.models.install";
        let models_dir = self.ml_models_dir.clone().ok_or_else(|| bad(C, "this session has no ML models folder configured"))?;
        let src_str = p
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| bad(C, "missing `path` to model directory"))?;
        let acknowledged =
            p.get("acknowledged").or_else(|| p.get("acceptLicence")).or_else(|| p.get("acceptLicense")).and_then(Value::as_bool).unwrap_or(false);
        if !acknowledged {
            return Err(bad(C, "pass `acknowledged: true` to confirm the model's licence before installing"));
        }
        let select = p.get("select").and_then(Value::as_bool).unwrap_or(true);
        let src_dir = PathBuf::from(src_str);

        let manifest = ModelManifest::verify_dir(&src_dir).map_err(|e| bad(C, e.to_string()))?;
        let dst_dir = models_dir.join(&manifest.id);
        let same_dir = std::fs::canonicalize(&src_dir).ok().zip(std::fs::canonicalize(&dst_dir).ok()).is_some_and(|(a, b)| a == b);

        if !same_dir {
            let tmp_dir = models_dir.join(format!(".install-{}-{}", manifest.id, std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp_dir);
            std::fs::create_dir_all(&tmp_dir).map_err(|e| bad(C, format!("cannot create {}: {e}", tmp_dir.display())))?;
            for f in &manifest.files {
                std::fs::copy(src_dir.join(&f.name), tmp_dir.join(&f.name)).map_err(|e| {
                    let _ = std::fs::remove_dir_all(&tmp_dir);
                    bad(C, format!("cannot copy {}: {e}", f.name))
                })?;
            }
            let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| bad(C, e.to_string()))?;
            std::fs::write(tmp_dir.join(lcc_ml::MANIFEST_FILE), manifest_bytes).map_err(|e| {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                bad(C, format!("cannot write manifest.json: {e}"))
            })?;
            ModelManifest::verify_dir(&tmp_dir).map_err(|e| {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                bad(C, e.to_string())
            })?;
            let _ = std::fs::remove_dir_all(&dst_dir);
            std::fs::rename(&tmp_dir, &dst_dir).map_err(|e| {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                bad(C, format!("cannot install model to {}: {e}", dst_dir.display()))
            })?;
        }

        self.ml.models_cache = None;
        if select && manifest.role == Role::Embedder {
            let mut settings = self.ml.cached_settings(&models_dir);
            settings.embedder = Some(manifest.id.clone());
            self.ml.save_settings(&models_dir, settings).map_err(|e| bad(C, e))?;
            self.ml.last_checked = None;
            self.ml.paused_by_user = false;
        }
        let selected_id = self.ml.cached_models(&models_dir).1;
        let selected = selected_id.as_deref() == Some(manifest.id.as_str());
        let info = manifest_json(&manifest, true, selected, None);
        Ok(json!({
            "installed": info,
            "selected": selected_id,
        }))
    }

    /// `ml.models.select {id}`.
    pub(crate) fn ml_models_select(&mut self, p: &Value) -> crate::Result<Value> {
        const C: &str = "ml.models.select";
        let models_dir = self.ml_models_dir.clone().ok_or_else(|| bad(C, "this session has no ML models folder configured"))?;
        let id = match p.get("id") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => return Err(bad(C, "`id` must be a model id string or null")),
        };
        if let Some(ref chosen) = id {
            if !valid_id(chosen) {
                return Err(bad(C, format!("invalid model id {chosen:?}")));
            }
            let installed = installed_manifests(&models_dir);
            if !installed.iter().any(|m| &m.id == chosen && m.role == Role::Embedder) {
                return Err(bad(C, format!("embedder model {chosen:?} is not installed")));
            }
        }
        let mut settings = self.ml.cached_settings(&models_dir);
        settings.embedder = id.clone();
        self.ml.save_settings(&models_dir, settings).map_err(|e| bad(C, e))?;
        self.ml.last_checked = None;
        self.ml.paused_by_user = false;
        Ok(json!({"selectedEmbedder": id}))
    }

    /// `ml.models.remove {id}`.
    pub(crate) fn ml_models_remove(&mut self, p: &Value) -> crate::Result<Value> {
        const C: &str = "ml.models.remove";
        let models_dir = self.ml_models_dir.clone().ok_or_else(|| bad(C, "this session has no ML models folder configured"))?;
        let id = p.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| bad(C, "missing `id`"))?;
        if !valid_id(id) {
            return Err(bad(C, format!("invalid model id {id:?}")));
        }
        let target = models_dir.join(id);
        let removed = if target.is_dir() {
            std::fs::remove_dir_all(&target).map_err(|e| bad(C, format!("cannot remove {}: {e}", target.display())))?;
            true
        } else {
            false
        };
        self.ml.models_cache = None;
        let mut settings = self.ml.cached_settings(&models_dir);
        if settings.embedder.as_deref() == Some(id) {
            settings.embedder = None;
            let _ = self.ml.save_settings(&models_dir, settings);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.ml.encoder.as_ref().is_some_and(|(_, e)| e.manifest().id == id) {
            self.ml.save_store_now();
            self.ml.encoder = None;
            self.ml.store = None;
            self.ml.store_path = None;
            if let Some(q) = &self.ml.queue {
                for pid in self.ml.in_flight.drain() {
                    q.cancel(&format!("embed:{}", pid.0));
                }
            }
        }
        self.ml.last_checked = None;
        Ok(json!({"id": id, "removed": removed}))
    }

    /// `ml.models.download {id, acceptLicence?: true}`.
    pub(crate) fn ml_models_download(&mut self, p: &Value) -> crate::Result<Value> {
        const C: &str = "ml.models.download";
        let models_dir = self.ml_models_dir.clone().ok_or_else(|| bad(C, "this session has no ML models folder configured"))?;
        let id = p.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| bad(C, "missing `id`"))?;
        let spec = lcc_ml::known::download(id).ok_or_else(|| bad(C, format!("unknown downloadable ML model {id:?}")))?;
        let accepted =
            p.get("acceptLicence").or_else(|| p.get("acceptLicense")).or_else(|| p.get("acknowledged")).and_then(Value::as_bool).unwrap_or(false);
        if !accepted {
            return Err(bad(
                C,
                format!(
                    "pass `acceptLicence: true` to confirm downloading {} ({}, {} bytes)",
                    spec.manifest.name,
                    spec.manifest.licence.spdx,
                    spec.manifest.total_bytes()
                ),
            ));
        }
        if matches!(self.ml.downloads.get(id), Some(DownloadState::Running { .. })) {
            return Err(bad(C, format!("{id} is already downloading")));
        }
        start_model_download(&mut self.ml.downloads, spec.clone(), &models_dir, &self.activity).map_err(|e| bad(C, e))?;
        Ok(json!({
            "id": spec.manifest.id,
            "name": spec.manifest.name,
            "sizeBytes": spec.manifest.total_bytes(),
            "licence": spec.manifest.licence.spdx,
            "state": "running",
        }))
    }

    /// `ml.models.downloads {}`.
    pub(crate) fn ml_models_downloads(&mut self, _p: &Value) -> crate::Result<Value> {
        let snap = self.ml.downloads.snapshot();
        if snap.iter().any(|(_, state)| *state == DownloadState::Installed) {
            self.ml.models_cache = None;
            self.ml.settings_cache = None;
        }
        let list: Vec<Value> = snap
            .into_iter()
            .map(|(id, state)| match state {
                DownloadState::Running { bytes, total } => {
                    json!({"id": id, "state": "running", "bytes": bytes, "total": total})
                }
                DownloadState::Installed => json!({"id": id, "state": "installed"}),
                DownloadState::Failed(err) => json!({"id": id, "state": "failed", "error": err}),
                DownloadState::Cancelled => json!({"id": id, "state": "cancelled"}),
            })
            .collect();
        Ok(json!({"downloads": list}))
    }

    /// `ml.models.downloadCancel {id}`.
    pub(crate) fn ml_models_download_cancel(&mut self, p: &Value) -> crate::Result<Value> {
        const C: &str = "ml.models.downloadCancel";
        let id = p.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| bad(C, "missing `id`"))?;
        let cancelled = self.ml.downloads.cancel(id);
        Ok(json!({"id": id, "cancelled": cancelled}))
    }
}

fn start_model_download(downloads: &mut Downloads, spec: Download, models_dir: &Path, activity: &Activity) -> Result<(), String> {
    let staging_root = models_dir.join(STAGING_DIR).join(&spec.manifest.id);
    std::fs::create_dir_all(&staging_root).map_err(|e| format!("could not create download folder: {e}"))?;
    let total = spec.manifest.total_bytes();
    let cancel = Arc::new(AtomicBool::new(false));
    let task = activity.start("download", "Downloading AI model", Cancel::Flag(cancel.clone()));
    task.set_unit(Unit::Bytes);
    task.detail(&spec.manifest.name);
    task.progress(0, total);
    let shared = Arc::new(DownloadShared { state: Mutex::new(DownloadState::Running { bytes: 0, total }), cancel, task: Some(task.handle()) });
    let worker = Arc::clone(&shared);
    let id = spec.manifest.id.clone();
    let dst_models_dir = models_dir.to_path_buf();

    std::thread::Builder::new()
        .name("ml-model-download".into())
        .spawn(move || {
            let _task = task;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_model_download(&spec, &staging_root, &dst_models_dir, &worker);
            }));
            if outcome.is_err() {
                worker.set(DownloadState::Failed("the model download stopped unexpectedly".into()));
            }
        })
        .map_err(|e| format!("could not start download thread: {e}"))?;
    downloads.jobs.insert(id, shared);
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn run_model_download(spec: &Download, staging_dir: &Path, models_dir: &Path, shared: &DownloadShared) {
    let total = spec.manifest.total_bytes();
    let file_specs: Vec<lcc_fetch::FileSpec<'_>> = spec
        .files
        .iter()
        .map(|f| lcc_fetch::FileSpec { name: f.name, size: Some(f.size_bytes), sha256: Some(f.sha256), max: f.size_bytes })
        .collect();
    let base_mirror = lcc_ml::known::CLIP_VIT_B32_BASE_URL.to_string();
    let res = lcc_fetch::download(&file_specs, &[base_mirror], staging_dir, &lcc_fetch::Options::default(), &shared.cancel, &mut |p| {
        shared.set(DownloadState::Running { bytes: p.done.min(total), total });
    });
    match res {
        Ok(()) => {
            let manifest_bytes = match serde_json::to_vec_pretty(&spec.manifest) {
                Ok(b) => b,
                Err(e) => {
                    shared.set(DownloadState::Failed(e.to_string()));
                    return;
                }
            };
            if let Err(e) = std::fs::write(staging_dir.join(lcc_ml::MANIFEST_FILE), manifest_bytes) {
                shared.set(DownloadState::Failed(format!("cannot write manifest.json: {e}")));
                return;
            }
            if let Err(e) = ModelManifest::verify_dir(staging_dir) {
                shared.set(DownloadState::Failed(e.to_string()));
                return;
            }
            let final_dir = models_dir.join(&spec.manifest.id);
            let _ = std::fs::remove_dir_all(&final_dir);
            if let Err(e) = std::fs::rename(staging_dir, &final_dir) {
                shared.set(DownloadState::Failed(format!("cannot move verified model into place: {e}")));
                return;
            }
            let mut settings = read_settings(models_dir);
            if settings.embedder.is_none() {
                settings.embedder = Some(spec.manifest.id.clone());
                let _ = write_settings(models_dir, &settings);
            }
            shared.set(DownloadState::Installed);
        }
        Err(lcc_fetch::DownloadError::Cancelled) => {
            let _ = std::fs::remove_dir_all(staging_dir);
            shared.set(DownloadState::Cancelled);
        }
        Err(e) => {
            shared.set(DownloadState::Failed(e.to_string()));
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn run_model_download(_spec: &Download, _staging_dir: &Path, _models_dir: &Path, shared: &DownloadShared) {
    shared.set(DownloadState::Failed("the web build does not download ML models".into()));
}

#[cfg(not(target_arch = "wasm32"))]
impl Session {
    /// Ensure the active embedder model is loaded and its [`Store`] is attached for the current library.
    fn ensure_ml_encoder(&mut self, fresh: bool) -> Result<Arc<dyn Encoder>, String> {
        if !fresh
            && let Some((_, enc)) = &self.ml.encoder
            && self.ml.last_checked.is_some_and(|t| t.elapsed() < MODEL_RECHECK)
        {
            return Ok(Arc::clone(enc));
        }
        if fresh {
            self.ml.models_cache = None;
            self.ml.settings_cache = None;
        }
        let models_dir = self.ml_models_dir.clone().ok_or_else(|| "no ML models folder is configured".to_string())?;
        let (installed, id_opt) = self.ml.cached_models(&models_dir);
        let id = id_opt.ok_or_else(|| {
            "no AI embedding model is installed (install or download one with `ml.models.install` or `ml.models.download`)".to_string()
        })?;
        let model_dir = models_dir.join(&id);
        let manifest = match installed.into_iter().find(|m| m.id == id) {
            Some(m) => m,
            None => ModelManifest::read_dir(&model_dir).map_err(|e| e.to_string())?,
        };
        let tag = manifest.tag();
        self.ml.last_checked = Some(web_time::Instant::now());

        let expected_store_path = self.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join(store_file_name(&id)));

        if let Some((loaded_tag, enc)) = &self.ml.encoder
            && *loaded_tag == tag
        {
            let enc = Arc::clone(enc);
            self.ensure_ml_store(&tag, manifest.embed_dim, expected_store_path)?;
            return Ok(enc);
        }

        let clip = lcc_ml::Clip::load_lazy(&model_dir)
            .map_err(|e| e.to_string())?
            .with_accelerator(clip_accelerator_for(Arc::clone(&self.ml.device_choice)));
        let enc: Arc<dyn Encoder> = Arc::new(clip);
        self.ml.save_store_now();
        self.ml.encoder = Some((tag.clone(), Arc::clone(&enc)));
        self.ml.store = None;
        self.ml.store_path = None;
        self.ml.in_flight.clear();
        self.ml.failed_photos.clear();
        self.ml.epoch = self.ml.epoch.wrapping_add(1);
        self.ensure_ml_store(&tag, manifest.embed_dim, expected_store_path)?;
        let warm_enc = Arc::clone(&enc);
        let _ = self.ml_query_queue().submit(
            Job::new("prewarm:text", move |ctx: &Ctx| -> Result<Output, JobError> {
                let _ = warm_enc.encode_text("", ctx)?;
                Ok(Box::new(()))
            })
            .priority(Priority::Background),
        );
        Ok(enc)
    }

    fn ensure_ml_store(&mut self, tag: &str, dim: u32, expected_path: Option<PathBuf>) -> Result<(), String> {
        let needs_reload = match &self.ml.store {
            None => true,
            Some(st) => st.tag() != tag || st.dim() != dim || self.ml.store_path != expected_path,
        };
        if needs_reload {
            self.ml.save_store_now();
            let store = match &expected_path {
                Some(path) => Store::load_or_reset(path, tag, dim),
                None => Store::new(tag, dim).map_err(|e| e.to_string())?,
            };
            self.ml.store = Some(store);
            self.ml.store_path = expected_path;
        }
        Ok(())
    }

    /// Drain finished `embed:*` jobs from `ml.queue` into `ml.store`.
    pub(crate) fn drain_ml_results(&mut self) -> (usize, usize) {
        let Some(q) = &self.ml.queue else {
            return (0, 0);
        };
        let finished = q.drain();
        let busy = q.busy();
        let mut added = 0usize;
        let mut failed = 0usize;
        for f in finished {
            if let Some(rest) = f.key.strip_prefix("embed:")
                && let Ok(raw_id) = rest.parse::<u64>()
            {
                let pid = PhotoId(raw_id);
                self.ml.in_flight.remove(&pid);
                match f.outcome {
                    Outcome::Done(any) => {
                        if let Ok(out) = any.downcast::<EmbedOutput>()
                            && out.epoch == self.ml.epoch
                            && let Some(photo) = self.catalog.photo(out.photo_id)
                            && photo.in_library()
                            && photo_fingerprint(photo) == out.fingerprint
                            && let Some(store) = &mut self.ml.store
                            && store.tag() == out.tag
                            && store.insert(out.photo_id.0, out.fingerprint, out.embedding).is_ok()
                        {
                            added += 1;
                        }
                    }
                    Outcome::Failed(_) => {
                        failed += 1;
                        if let Some(photo) = self.catalog.photo(pid) {
                            self.ml.failed_photos.insert((pid.0, photo_fingerprint(photo)));
                        }
                    }
                    Outcome::Cancelled => {
                        self.ml.paused_by_user = true;
                        self.ml.in_flight.clear();
                    }
                }
            }
        }
        let should_save =
            self.ml.store.as_ref().is_some_and(Store::is_dirty) && (!busy || self.ml.last_saved.is_none_or(|t| t.elapsed() >= SAVE_EVERY));
        if should_save {
            self.ml.save_store_now();
        }
        if added > 0 {
            self.refresh_semantic_filter_from_cache();
        }
        (added, failed)
    }

    /// Re-run the cached query vector against `ml.store` when new photo embeddings finish while
    /// a semantic filter is active on `self.filter`.
    pub(crate) fn refresh_semantic_filter_from_cache(&mut self) {
        let Some(active_text) = self.filter.semantic.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string) else {
            return;
        };
        let needs_encode = match (&self.ml.last_query, &self.ml.store) {
            (Some(c), Some(st)) => c.text != active_text || c.tag != st.tag(),
            _ => true,
        };
        if needs_encode {
            let _ = self.apply_semantic_filter("ml.pump", Some(&active_text), None, None);
            return;
        }
        let (Some(cached), Some(store)) = (self.ml.last_query.as_ref(), self.ml.store.as_ref()) else {
            return;
        };
        let allowed: HashSet<u64> = self.catalog.photos().filter(|ph| ph.in_library()).map(|ph| ph.id.0).collect();
        if let Ok(hits) = store.search(&cached.vec, cached.top_k, cached.threshold, Some(&allowed)) {
            let ids: Vec<PhotoId> = hits.into_iter().map(|h| PhotoId(h.photo_id)).collect();
            self.filter.only_none = ids.is_empty();
            self.filter.only = ids;
        }
    }

    /// Submit unindexed photos to `ml_queue`.
    fn queue_ml_photos(&mut self, encoder: &Arc<dyn Encoder>, targets: Option<&[PhotoId]>, priority: Priority) -> usize {
        let Some(store) = &self.ml.store else {
            return 0;
        };
        let tag = store.tag().to_string();
        let epoch = self.ml.epoch;
        let candidate_ids: Vec<PhotoId> = match targets {
            Some(ids) => ids.to_vec(),
            None => self.catalog.photos().filter(|p| p.in_library()).map(|p| p.id).collect(),
        };

        let mut to_submit = Vec::new();
        for id in candidate_ids {
            if self.ml.in_flight.contains(&id) {
                continue;
            }
            let Some(photo) = self.catalog.photo(id) else {
                continue;
            };
            if !photo.in_library() {
                continue;
            }
            let fp = photo_fingerprint(photo);
            if self.ml.store.as_ref().is_some_and(|st| st.has(id.0, fp)) {
                continue;
            }
            if targets.is_none() && self.ml.failed_photos.contains(&(id.0, fp)) {
                continue;
            }
            to_submit.push((id, fp, photo.file_name.clone()));
        }

        let mut submitted = 0usize;
        for (id, fp, name) in to_submit {
            let Some(photo) = self.catalog.photo(id).cloned() else {
                continue;
            };
            let Some(render_job) = self.preview_job(id, EMBED_PREVIEW_EDGE, EMBED_PREVIEW_EDGE, false, &DevelopSettings::default()) else {
                continue;
            };
            let mut render_job = Some(render_job);
            let mut decoded: Option<RgbImage> = None;
            let preview = self.scan_preview_of(&photo);
            let enc = Arc::clone(encoder);
            let job_tag = tag.clone();
            let pace_ref = Arc::clone(&self.ml.pace);
            let key = format!("embed:{}", id.0);

            let job = Job::new(key, move |ctx: &Ctx| -> Result<Output, JobError> {
                ctx.check()?;
                if priority == Priority::Background {
                    while Pace::from_u8(pace_ref.load(Ordering::Relaxed)) == Pace::Pause {
                        ctx.check()?;
                        std::thread::sleep(PAUSE_POLL);
                    }
                    if Pace::from_u8(pace_ref.load(Ordering::Relaxed)) == Pace::Light {
                        std::thread::sleep(LIGHT_REST);
                        ctx.check()?;
                    }
                }
                let rgb_img = if let Some(img) = decoded.as_ref() {
                    img.clone()
                } else {
                    let rgba = preview
                        .as_ref()
                        .and_then(|(path, load)| load(path, EMBED_PREVIEW_EDGE))
                        .filter(|img| img.width.max(img.height) >= MIN_RAW_PREVIEW_EDGE)
                        .or_else(|| render_job.take().and_then(|j| j.run().rendered.ok().map(|r| r.image)))
                        .ok_or_else(|| JobError::Failed(format!("cannot decode preview for photo {}", id.0)))?;
                    ctx.check()?;

                    let mut rgb = Vec::with_capacity(rgba.data.len() * 3);
                    for px in &rgba.data {
                        rgb.extend_from_slice(&[px[0], px[1], px[2]]);
                    }
                    let img = RgbImage::new(rgba.width as u32, rgba.height as u32, rgb).map_err(JobError::Failed)?;
                    decoded = Some(img.clone());
                    img
                };
                let mut vecs = enc.encode_images(&[rgb_img], ctx)?;
                let embedding = vecs.pop().ok_or_else(|| JobError::Failed("empty embedding batch output".into()))?;
                Ok(Box::new(EmbedOutput { epoch, tag: job_tag.clone(), photo_id: id, fingerprint: fp, embedding }))
            })
            .name(name)
            .priority(priority);

            if self.ml_submit(job).is_ok() {
                self.ml.in_flight.insert(id);
                submitted += 1;
            } else {
                break;
            }
        }
        submitted
    }

    /// `ml.pump {pace?: pause | light | normal | full}`.
    pub(crate) fn ml_pump(&mut self, p: &Value) -> crate::Result<Value> {
        self.ml_enabled();
        // paused by the user (`ml.pause`): the photos being embedded wait, nothing new is queued
        let pace = if self.ml.paused { Pace::Pause } else { Pace::parse(p.get("pace").and_then(Value::as_str)) };
        self.ml.pace.store(pace as u8, Ordering::Relaxed);
        let (added, _) = self.drain_ml_results();

        let mut ready = false;
        if self.ml_enabled()
            && !self.ml.paused_by_user
            && let Ok(enc) = self.ensure_ml_encoder(false)
        {
            ready = true;
            if pace != Pace::Pause {
                let _ = self.queue_ml_photos(&enc, None, Priority::Background);
            }
        }
        let indexed = self.ml.store.as_ref().map_or(0, Store::len);
        let unindexed = self.unindexed_photo_count();
        // the queue's row (while it has work) says whether it is paused; with nothing queued, a paused index with photos
        // left keeps a row of its own, so resume is there
        let busy = self.ml.queue.as_ref().is_some_and(Queue::busy);
        if let Some(row) = self.ml.row.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            row.set_controls(Some(row_controls(self.ml.paused)));
            if self.ml.paused && busy {
                row.detail("Paused");
            }
        }
        if self.ml.paused && ready && unindexed > 0 && !busy {
            let row = self.ml.paused_row.get_or_insert_with(|| self.activity.start(ROW_KIND, ROW_LABEL, Cancel::No));
            row.progress(indexed as u64, (indexed + unindexed) as u64);
            row.detail("Paused");
            row.set_controls(Some(row_controls(true)));
        } else {
            self.ml.paused_row = None;
        }
        let in_flight = self.ml.in_flight.len();
        let (pending, running, done, failed) = match &self.ml.queue {
            Some(q) => {
                let st = q.status();
                (st.pending, st.running, st.done, st.failed)
            }
            None => (0, None, 0, 0),
        };
        let active = ready && self.ml.enabled && !self.ml.paused_by_user && !self.ml.paused;
        Ok(json!({
            "active": active,
            "ready": ready,
            "enabled": self.ml.enabled,
            "paused": self.ml.paused_by_user || self.ml.paused,
            "added": added,
            "indexedPhotos": indexed,
            "unindexedPhotos": unindexed,
            "pendingPhotos": unindexed,
            "inFlight": in_flight,
            "pending": pending,
            "running": running,
            "done": done,
            "failed": failed,
        }))
    }

    /// `ml.index {ids?, wait?: false, budgetMs?: 30000}`.
    pub(crate) fn ml_index(&mut self, p: &Value) -> crate::Result<Value> {
        const C: &str = "ml.index";
        let explicit_ids: Option<Vec<PhotoId>> = match p.get("ids") {
            None | Some(Value::Null) => None,
            Some(Value::Array(arr)) => {
                if let Some(why) = crate::bad_ids_entry(arr) {
                    return Err(bad(C, why));
                }
                Some(arr.iter().filter_map(|v| v.as_u64().map(PhotoId)).collect())
            }
            Some(_) => return Err(bad(C, "`ids` must be an array of photo ids")),
        };
        let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false) || p.get("budgetMs").is_some();
        let budget_ms = p.get("budgetMs").and_then(Value::as_u64).unwrap_or(30_000).clamp(1, 300_000);

        let enc = self.ensure_ml_encoder(true).map_err(|e| bad(C, e))?;
        self.ml.paused_by_user = false;
        self.ml.pace.store(Pace::Normal as u8, Ordering::Relaxed);
        match &explicit_ids {
            Some(ids) => {
                for id in ids {
                    self.ml.failed_photos.retain(|&(pid, _)| pid != id.0);
                }
            }
            None => self.ml.failed_photos.clear(),
        }
        self.drain_ml_results();
        let priority = if wait { Priority::Interactive } else { Priority::Background };
        let queued = self.queue_ml_photos(&enc, explicit_ids.as_deref(), priority);

        if wait {
            let started = web_time::Instant::now();
            let budget = Duration::from_millis(budget_ms);
            while started.elapsed() < budget {
                self.drain_ml_results();
                let done = match &explicit_ids {
                    Some(ids) => ids.iter().all(|id| !self.ml.in_flight.contains(id)),
                    None => self.ml.in_flight.is_empty() && self.ml.queue.as_ref().is_none_or(|q| !q.busy()),
                };
                if done {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            self.drain_ml_results();
            self.ml.save_store_now();
        }

        let indexed = self.ml.store.as_ref().map_or(0, Store::len);
        let unindexed = self.unindexed_photo_count();
        Ok(json!({
            "queued": queued,
            "indexedPhotos": indexed,
            "unindexedPhotos": unindexed,
            "inFlight": self.ml.in_flight.len(),
        }))
    }

    /// Encode `text` using the active ML embedder (reusing `ml.last_query` when the text and model tag match).
    fn encode_query_vec(&mut self, cmd_name: &'static str, text: &str) -> crate::Result<(Arc<dyn Encoder>, String, Vec<f32>)> {
        let enc = self.ensure_ml_encoder(false).map_err(|e| bad(cmd_name, e))?;
        let tag = enc.manifest().tag();
        self.drain_ml_results();

        if let Some(cached) = self.ml.last_query.as_ref()
            && cached.text == text
            && cached.tag == tag
        {
            return Ok((enc, tag, cached.vec.clone()));
        }

        self.ml.query_seq = self.ml.query_seq.wrapping_add(1);
        let qkey = format!("query:{}", self.ml.query_seq);
        let qtext = text.to_string();
        let qenc = Arc::clone(&enc);
        self.ml_query_queue()
            .submit(
                Job::new(qkey.clone(), move |ctx: &Ctx| -> Result<Output, JobError> {
                    let vec = qenc.encode_text(&qtext, ctx)?;
                    Ok(Box::new(QueryOutput(vec)))
                })
                .priority(Priority::Interactive),
            )
            .map_err(|e| bad(cmd_name, e.to_string()))?;

        let started = web_time::Instant::now();
        let mut query_vec: Option<Vec<f32>> = None;
        while started.elapsed() < Duration::from_secs(30) {
            let finished = self.ml.query_queue.as_ref().map(|q| q.drain()).unwrap_or_default();
            for f in finished {
                if f.key == qkey {
                    match f.outcome {
                        Outcome::Done(any) => {
                            if let Ok(qout) = any.downcast::<QueryOutput>() {
                                query_vec = Some(qout.0);
                            }
                        }
                        Outcome::Failed(why) => return Err(bad(cmd_name, why)),
                        Outcome::Cancelled => {
                            return Err(crate::EngineError::Cancelled("semantic search cancelled".into()));
                        }
                    }
                }
            }
            if query_vec.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        let qvec = query_vec.ok_or_else(|| bad(cmd_name, "semantic search timed out waiting for query embedding"))?;
        Ok((enc, tag, qvec))
    }

    /// Set or clear the library's natural-language semantic filter (`library.filter {semantic}` or `search.semantic {filter: true}`).
    pub(crate) fn apply_semantic_filter(
        &mut self,
        cmd_name: &'static str,
        query: Option<&str>,
        top_k: Option<usize>,
        threshold: Option<f32>,
    ) -> crate::Result<Vec<lcc_ml::Hit>> {
        let Some(text) = query.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string) else {
            self.filter.semantic = None;
            self.filter.only.clear();
            self.filter.only_none = false;
            self.ml.last_query = None;
            return Ok(Vec::new());
        };
        if !self.ml_enabled() {
            return Err(bad(cmd_name, "semantic search is off (Settings ▸ AI Models ▸ Semantic search)"));
        }
        let top_k = top_k.unwrap_or(DEFAULT_FILTER_TOP_K);
        let threshold = threshold.unwrap_or(DEFAULT_FILTER_THRESHOLD);
        if cmd_name != "search.semantic" && self.selected_ml_model().is_none() {
            self.shelved_filter = None;
            self.filter.semantic = Some(text);
            self.filter.only.clear();
            self.filter.only_none = true;
            self.ml.last_query = None;
            return Ok(Vec::new());
        }
        let (enc, tag, qvec) = self.encode_query_vec(cmd_name, &text)?;
        if self.ml_enabled() && !self.ml.paused_by_user {
            let _ = self.queue_ml_photos(&enc, None, Priority::Background);
        }
        let allowed: HashSet<u64> = self.catalog.photos().filter(|ph| ph.in_library()).map(|ph| ph.id.0).collect();
        let hits = self
            .ml
            .store
            .as_ref()
            .map(|st| st.search(&qvec, top_k, threshold, Some(&allowed)))
            .transpose()
            .map_err(|e| bad(cmd_name, e.to_string()))?
            .unwrap_or_default();
        self.shelved_filter = None;
        self.filter.semantic = Some(text.clone());
        self.filter.only = hits.iter().map(|h| PhotoId(h.photo_id)).collect();
        self.filter.only_none = self.filter.only.is_empty();
        self.ml.last_query = Some(CachedQuery { text, tag, vec: qvec, top_k, threshold });
        Ok(hits)
    }

    /// `search.semantic {text, topK?: 50, threshold?: -1.0, scope?: "all" | "filtered" | "selected", filter?: false, clear?: false}`.
    pub(crate) fn search_semantic(&mut self, p: &Value) -> crate::Result<Value> {
        const C: &str = "search.semantic";
        let filter_grid = p.get("filter").or_else(|| p.get("show")).and_then(Value::as_bool).unwrap_or(false);
        let clear = p.get("clear").and_then(Value::as_bool).unwrap_or(false);
        let raw_text = p.get("text").or_else(|| p.get("query")).and_then(Value::as_str).map(str::trim).unwrap_or("");
        if clear || (filter_grid && raw_text.is_empty()) {
            self.apply_semantic_filter(C, None, None, None)?;
            self.reconcile_view();
            self.save_view();
            let filtered = self.visible().len();
            return Ok(json!({
                "query": "",
                "cleared": true,
                "hits": [],
                "matches": [],
                "filtered": filtered,
            }));
        }
        if raw_text.is_empty() {
            return Err(bad(C, "missing non-empty `text` query"));
        }
        if !self.ml_enabled() {
            return Err(bad(C, "semantic search is off (Settings ▸ AI Models ▸ Semantic search)"));
        }
        let text = raw_text.to_string();
        let top_k = match p.get("topK").or_else(|| p.get("limit")) {
            None | Some(Value::Null) => {
                if filter_grid {
                    DEFAULT_FILTER_TOP_K
                } else {
                    50usize
                }
            }
            Some(v) => v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|&n| (1..=1000).contains(&n))
                .ok_or_else(|| bad(C, "`topK` must be an integer between 1 and 1000"))?,
        };
        let threshold = match p.get("threshold").or_else(|| p.get("minScore")) {
            None | Some(Value::Null) => {
                if filter_grid {
                    DEFAULT_FILTER_THRESHOLD
                } else {
                    -1.0f32
                }
            }
            Some(v) => v
                .as_f64()
                .map(|x| x as f32)
                .filter(|x| x.is_finite() && (-1.0..=1.0).contains(x))
                .ok_or_else(|| bad(C, "`threshold` must be a number between -1.0 and 1.0"))?,
        };
        let scope = p.get("scope").and_then(Value::as_str).unwrap_or("all");
        let allowed: HashSet<u64> = match scope {
            "all" => self.catalog.photos().filter(|ph| ph.in_library()).map(|ph| ph.id.0).collect(),
            "filtered" => self.visible().iter().map(|id| id.0).collect(),
            "selected" => self.selected().into_iter().map(|id| id.0).collect(),
            other => {
                return Err(bad(C, format!("`scope` must be `all`, `filtered` or `selected` (got {other:?})")));
            }
        };

        let (enc, tag, qvec) = self.encode_query_vec(C, &text)?;
        let model_id = enc.manifest().id.clone();
        if filter_grid && self.ml_enabled() && !self.ml.paused_by_user {
            let _ = self.queue_ml_photos(&enc, None, Priority::Background);
        }
        let hits = self
            .ml
            .store
            .as_ref()
            .map(|st| st.search(&qvec, top_k, threshold, Some(&allowed)))
            .transpose()
            .map_err(|e| bad(C, e.to_string()))?
            .unwrap_or_default();
        let indexed = self.ml.store.as_ref().map_or(0, Store::len);
        let pending = self.unindexed_photo_count();

        if filter_grid {
            self.shelved_filter = None;
            self.filter.semantic = Some(text.clone());
            self.filter.only = hits.iter().map(|h| PhotoId(h.photo_id)).collect();
            self.filter.only_none = self.filter.only.is_empty();
            self.ml.last_query = Some(CachedQuery { text: text.clone(), tag, vec: qvec, top_k, threshold });
            self.reconcile_view();
            self.save_view();
        }

        let rows: Vec<Value> = hits.iter().map(|h| json!({"id": h.photo_id, "score": h.score})).collect();
        let mut out = json!({
            "query": text,
            "modelId": model_id,
            "hits": rows,
            "matches": rows,
            "indexedPhotos": indexed,
            "pendingPhotos": pending,
        });
        if filter_grid && let Some(obj) = out.as_object_mut() {
            obj.insert("filtered".into(), json!(self.visible().len()));
        }
        Ok(out)
    }
}

#[cfg(target_arch = "wasm32")]
impl Session {
    pub(crate) fn apply_semantic_filter(
        &mut self,
        cmd_name: &'static str,
        query: Option<&str>,
        _top_k: Option<usize>,
        _threshold: Option<f32>,
    ) -> crate::Result<Vec<lcc_ml::Hit>> {
        if query.map(str::trim).filter(|s| !s.is_empty()).is_none() {
            self.filter.semantic = None;
            self.filter.only.clear();
            self.filter.only_none = false;
            self.ml.last_query = None;
            return Ok(Vec::new());
        }
        Err(bad(cmd_name, "semantic search is not available in the web build"))
    }
    pub(crate) fn ml_pump(&mut self, _p: &Value) -> crate::Result<Value> {
        Ok(json!({
            "active": false,
            "ready": false,
            "enabled": self.ml_enabled(),
            "paused": false,
            "added": 0,
            "indexedPhotos": 0,
            "unindexedPhotos": 0,
            "pendingPhotos": 0,
            "inFlight": 0,
            "pending": 0,
            "running": null,
            "done": 0,
            "failed": 0,
        }))
    }

    pub(crate) fn ml_index(&mut self, _p: &Value) -> crate::Result<Value> {
        Err(bad("ml.index", "ML indexing is not available in the web build"))
    }

    pub(crate) fn search_semantic(&mut self, _p: &Value) -> crate::Result<Value> {
        Err(bad("search.semantic", "semantic search is not available in the web build"))
    }
}
