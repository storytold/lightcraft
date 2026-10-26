//! Content models (searching photos by what they show): the model folders on disk, and the background jobs that
//! download, check and test them.
//!
//! Models live in the folder the host provides (`Session::content_models_dir`, `<config>/models/content`): one
//! subfolder per model, named by its id, holding the model's files (`model.safetensors`, `tokenizer.json`,
//! `config.json`, `preprocessor_config.json`), `content-model.json` (its manifest, [`lightcraft_vision::Manifest`]) and
//! `installed.json` (when the user accepted its licence, and its last self-test), plus `settings.json` (`enabled`,
//! `model`) and the user's own `catalog.json` beside them.
//!
//! A model is *installed* when its manifest validates, names its folder, every file it lists is there with its exact
//! size, and `installed.json` records the user's acceptance. Every file was checked against its SHA-256 when it was
//! installed. A folder of a known model that has all the files at the right sizes but no manifest (the files were put
//! there by hand) is *found*: checking it (`content.models.download`, which then fetches nothing) verifies every
//! SHA-256 and installs it. Anything else is ignored or reported as damaged, never trusted.
//!
//! Downloads come from Hugging Face (the revision the built-in manifest pins) or from the user's mirrors
//! (`LIGHTCRAFT_VISION_MIRRORS`, `content-mirrors.txt` beside the content folder) and are checked by size and
//! SHA-256 ([`lightcraft_fetch`]). One job runs at a time; it shows in the activity stack, and its ✕ cancels it.
//! A cancelled download leaves nothing behind; a failed one keeps its partial files so Try Again resumes.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lightcraft_vision::manifest::{self, MAX_MANIFEST_BYTES, Manifest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::activity::{Activity, Cancel, TaskHandle, Unit};

/// A model's manifest, in its folder.
pub const MANIFEST_FILE: &str = "content-model.json";
/// What the user accepted (and the last self-test), in the model's folder.
pub const RECORD_FILE: &str = "installed.json";
/// `enabled` and the model in use, in the content folder.
pub const SETTINGS_FILE: &str = "settings.json";
/// The user's own list of models (`{"models": [manifest, …]}`), in the content folder.
pub const CATALOG_FILE: &str = "catalog.json";
/// The user's mirrors (one base URL per line), beside the content folder (`<config>/models/content-mirrors.txt`).
pub const MIRRORS_FILE: &str = "content-mirrors.txt";
/// Extra mirrors (base URLs separated by commas, spaces or newlines), tried before Hugging Face.
pub const MIRRORS_ENV: &str = "LIGHTCRAFT_VISION_MIRRORS";
/// Largest `catalog.json` read, and most models it may list.
pub const MAX_CATALOG_BYTES: usize = 256 * 1024;
pub const MAX_CATALOG_MODELS: usize = 32;
/// Largest `settings.json` / `installed.json` read.
const MAX_SMALL: usize = 64 * 1024;

/// `settings.json`: whether photos are indexed, the model in use and how hard indexing works.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub enabled: bool,
    pub model: Option<String>,
    /// How hard indexing works: `low`, `normal` (the default) or `fast`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pace: Option<String>,
    /// The keyword parent of accepted auto-tags (`None`: `Contents`; empty: none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_parent: Option<String>,
    /// The auto-tags' cut-off chosen by the user (`None`: the model's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_threshold: Option<f32>,
}

/// Read at most `max` bytes of a small file (`None` when it is missing, unreadable or larger).
pub fn read_capped(path: &Path, max: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(max as u64 + 1).read_to_end(&mut buf).ok()?;
    (buf.len() <= max).then_some(buf)
}

/// Write `bytes` to `path` through `<path>.part` and a rename, so a crash never leaves half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let part = path.with_extension("json.part");
    std::fs::write(&part, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    std::fs::rename(&part, path).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("could not write {}: {e}", path.display())
    })
}

pub fn read_settings(dir: &Path) -> Settings {
    read_capped(&dir.join(SETTINGS_FILE), MAX_SMALL).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn write_settings(dir: &Path, st: &Settings) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create the content models folder {}: {e}", dir.display()))?;
    write_atomic(&dir.join(SETTINGS_FILE), &serde_json::to_vec_pretty(st).map_err(|e| e.to_string())?)
}

/// The user's catalog: the models it adds, and what was wrong with the rest.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub models: Vec<Manifest>,
    pub errors: Vec<String>,
}

/// Read `catalog.json` in `dir` (missing = empty). Each entry is validated on its own; a bad one is reported and
/// skipped, an entry with a built-in model's id too.
pub fn read_catalog(dir: &Path) -> Catalog {
    let file = dir.join(CATALOG_FILE);
    let Some(bytes) = read_capped(&file, MAX_CATALOG_BYTES) else {
        return if file.is_file() {
            Catalog { models: Vec::new(), errors: vec![format!("{CATALOG_FILE} is larger than {} KB", MAX_CATALOG_BYTES / 1024)] }
        } else {
            Catalog::default()
        };
    };
    let raw: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return Catalog { models: Vec::new(), errors: vec![format!("{CATALOG_FILE}: {e}")] },
    };
    let Some(list) = raw.get("models").and_then(Value::as_array) else {
        return Catalog { models: Vec::new(), errors: vec![format!("{CATALOG_FILE} has no `models` list")] };
    };
    let mut out = Catalog::default();
    for (i, entry) in list.iter().enumerate() {
        if out.models.len() >= MAX_CATALOG_MODELS {
            out.errors.push(format!("only the first {MAX_CATALOG_MODELS} models are read"));
            break;
        }
        let parsed = serde_json::from_value::<Manifest>(entry.clone()).map_err(|e| e.to_string()).and_then(|m| {
            manifest::validate(&m).map_err(|e| e.to_string())?;
            Ok(m)
        });
        match parsed {
            Ok(m) if manifest::builtin().iter().any(|b| b.id == m.id) || out.models.iter().any(|o| o.id == m.id) => {
                out.errors.push(format!("model {}: `{}` is already listed", i + 1, m.id));
            }
            Ok(m) => out.models.push(m),
            Err(e) => out.errors.push(format!("model {}: {}", i + 1, e.chars().take(200).collect::<String>())),
        }
    }
    out
}

/// The models LightCraft knows: the built-in ones, then the catalog's.
pub fn known(catalog: &Catalog) -> Vec<Manifest> {
    let mut all = manifest::builtin();
    all.extend(catalog.models.iter().cloned());
    all
}

/// A known model's manifest as installed, with this version's built-in tuning: the search and tag cut-offs, a softmax
/// model's score offset and the model list's hint. So a calibration made after a model was installed reaches it; its
/// files (and so its index and tag) are untouched. Anything else is returned as is.
pub fn tuned(mut m: Manifest) -> Manifest {
    if let Some(b) = manifest::builtin().into_iter().find(|b| b.id == m.id && b.family == m.family) {
        m.tag_threshold = b.tag_threshold;
        m.match_threshold = b.match_threshold;
        m.score.offset = b.score.offset;
        m.hint = b.hint;
    }
    m
}

/// How a model's folder stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    /// Manifest, files at their sizes and the user's acceptance: usable.
    Installed,
    /// A known model's files at their sizes, not yet checked or accepted (put there by hand).
    Unverified,
    /// Not usable, and why.
    Damaged(String),
}

/// A model folder on disk.
#[derive(Clone, Debug)]
pub struct OnDisk {
    pub manifest: Manifest,
    pub state: Found,
    /// `installed.json` (null when there is none).
    pub record: Value,
}

/// What is wrong with `home`'s files for `m` (missing, or not their size), if anything.
fn files_problem(home: &Path, m: &Manifest) -> Option<String> {
    for f in &m.files {
        match std::fs::metadata(home.join(&f.name)) {
            Ok(meta) if meta.is_file() && meta.len() == f.size => {}
            Ok(meta) if meta.is_file() => return Some(format!("{} is {} bytes, not {}", f.name, meta.len(), f.size)),
            _ => return Some(format!("{} is missing", f.name)),
        }
    }
    None
}

/// The model folders in `dir`. A folder that is not a model (a name that is not an id, a manifest that does not
/// validate or names another folder) is reported in the second list and otherwise ignored; a known model's folder
/// that holds only part of its files (a download that stopped) is not listed.
pub fn on_disk(dir: &Path, known: &[Manifest]) -> (Vec<OnDisk>, Vec<String>) {
    let (mut out, mut errors) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(dir) else { return (out, errors) };
    for e in entries.flatten().take(1000) {
        let home = e.path();
        let Some(name) = home.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        if !manifest::valid_id(&name) || !home.is_dir() {
            continue;
        }
        let record = read_capped(&home.join(RECORD_FILE), MAX_SMALL).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
        let file = home.join(MANIFEST_FILE);
        let builtin = known.iter().find(|k| k.id == name);
        if file.exists() {
            let parsed = read_capped(&file, MAX_MANIFEST_BYTES)
                .ok_or_else(|| format!("{MANIFEST_FILE} cannot be read or is too large"))
                .and_then(|b| manifest::parse(&b).map_err(|e| e.to_string()));
            let m = match parsed {
                Ok(m) if m.id == name => tuned(m),
                Ok(m) => {
                    errors.push(format!("{name}: its {MANIFEST_FILE} is for `{}`", m.id));
                    continue;
                }
                Err(why) => {
                    match builtin {
                        Some(k) => out.push(OnDisk {
                            manifest: k.clone(),
                            state: Found::Damaged(format!("its {MANIFEST_FILE} is not valid ({why})")),
                            record,
                        }),
                        None => errors.push(format!("{name}: {why}")),
                    }
                    continue;
                }
            };
            let state = match files_problem(&home, &m) {
                Some(p) => Found::Damaged(p),
                None if record.get("acceptedAt").is_some() => Found::Installed,
                None => Found::Unverified,
            };
            out.push(OnDisk { manifest: m, state, record });
        } else if let Some(k) = builtin
            && files_problem(&home, k).is_none()
        {
            out.push(OnDisk { manifest: k.clone(), state: Found::Unverified, record });
        }
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    (out, errors)
}

/// The mirrors to try for model `id`, in order: the user's (`<mirror>/<id>/<file>`), then Hugging Face for a
/// built-in model (`<repository>/resolve/<revision>/<file>`).
#[cfg(not(target_arch = "wasm32"))]
pub fn mirrors_for(dir: &Path, id: &str) -> Vec<String> {
    let env = std::env::var(MIRRORS_ENV).ok();
    let file = dir.parent().map(|p| p.join(MIRRORS_FILE));
    let mut out: Vec<String> = lightcraft_fetch::mirrors(env.as_deref(), file.as_deref(), &[]).into_iter().map(|m| format!("{m}/{id}")).collect();
    out.extend(manifest::hugging_face(id).map(str::to_string));
    out
}

#[cfg(target_arch = "wasm32")]
pub fn mirrors_for(_: &Path, _: &str) -> Vec<String> {
    Vec::new()
}

/// `https://host/path` → `host`.
pub fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or_default().rsplit('@').next().unwrap_or_default().to_string()
}

/// What a job is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    /// Fetching the files (or only checking them, when they are all there).
    Download,
    Test,
}

/// Where a job stands.
#[derive(Clone, Debug, PartialEq)]
pub enum JobState {
    Running {
        bytes: u64,
        total: u64,
    },
    /// Finished: what it found (the record written, or the self-test).
    Done(Value),
    Failed(String),
    Cancelled,
}

struct Job {
    kind: JobKind,
    state: Mutex<JobState>,
    cancel: Arc<AtomicBool>,
    task: Option<TaskHandle>,
    /// The session has acted on its end (chosen the model it installed).
    settled: AtomicBool,
}

impl Job {
    fn new(kind: JobKind, total: u64) -> Self {
        Job {
            kind,
            state: Mutex::new(JobState::Running { bytes: 0, total }),
            cancel: Arc::new(AtomicBool::new(false)),
            task: None,
            settled: AtomicBool::new(false),
        }
    }

    fn set(&self, s: JobState) {
        if let (JobState::Running { bytes, total }, Some(task)) = (&s, &self.task) {
            task.progress(*bytes, *total);
        }
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = s;
    }

    fn get(&self) -> JobState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// What a download needs.
pub struct DownloadRequest {
    pub manifest: Manifest,
    /// The model's folder (`<content dir>/<id>`).
    pub home: PathBuf,
    pub mirrors: Vec<String>,
    /// `installed.json` to write once every file is in place and checked.
    pub record: Value,
}

/// The session's content-model jobs, by model id. One runs at a time.
#[derive(Default)]
pub struct Jobs {
    jobs: HashMap<String, Arc<Job>>,
}

impl Jobs {
    /// The running job, if any: (model id, kind).
    pub fn running(&self) -> Option<(String, JobKind)> {
        self.jobs.iter().find(|(_, j)| matches!(j.get(), JobState::Running { .. })).map(|(id, j)| (id.clone(), j.kind))
    }

    /// Model `id`'s latest job.
    pub fn get(&self, id: &str) -> Option<(JobKind, JobState)> {
        self.jobs.get(id).map(|j| (j.kind, j.get()))
    }

    /// Downloads that installed a model and have not been acted on yet (each is returned once).
    pub fn newly_installed(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .jobs
            .iter()
            .filter(|(_, j)| j.kind == JobKind::Download && matches!(j.get(), JobState::Done(_)) && !j.settled.swap(true, Ordering::SeqCst))
            .map(|(id, _)| id.clone())
            .collect();
        out.sort();
        out
    }

    /// Stop model `id`'s running job, or forget a finished one. `true` if there was one.
    pub fn cancel(&mut self, id: &str) -> bool {
        let Some(job) = self.jobs.get(id) else { return false };
        if matches!(job.get(), JobState::Running { .. }) {
            // the thread notices, cleans up and marks itself cancelled
            job.cancel.store(true, Ordering::SeqCst);
        } else {
            self.jobs.remove(id);
        }
        true
    }

    fn refuse_second(&self) -> Result<(), String> {
        match self.running() {
            Some((id, JobKind::Download)) => Err(format!("{id} is downloading: wait for it, or cancel it first")),
            Some((id, JobKind::Test)) => Err(format!("{id} is being tested: wait a moment")),
            None => Ok(()),
        }
    }

    /// Fetch (or check) `req`'s files on a background thread, shown in the activity stack as `name`.
    pub fn start_download(&mut self, req: DownloadRequest, activity: &Activity, name: &str) -> Result<(), String> {
        self.refuse_second()?;
        std::fs::create_dir_all(req.home.parent().unwrap_or(&req.home))
            .map_err(|e| format!("could not create the content models folder (is it writable?): {e}"))?;
        let total = manifest::total_size(&req.manifest);
        // every file already there at its size: only their checksums are checked, nothing is fetched
        let checking = files_problem(&req.home, &req.manifest).is_none();
        let mut job = Job::new(JobKind::Download, total);
        let task = activity.start("download", if checking { "Checking model files" } else { "Downloading model" }, Cancel::Flag(job.cancel.clone()));
        task.set_unit(Unit::Bytes);
        task.detail(name);
        task.progress(0, total);
        job.task = Some(task.handle());
        let job = Arc::new(job);
        let worker = job.clone();
        let id = req.manifest.id.clone();
        std::thread::Builder::new()
            .name("content-model-download".into())
            .spawn(move || {
                // the row goes when the thread ends, however it ends
                let _task = task;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_download(&req, &worker)));
                if outcome.is_err() {
                    worker.set(JobState::Failed("the download stopped unexpectedly".into()));
                }
            })
            .map_err(|e| format!("could not start the download: {e}"))?;
        self.jobs.insert(id, job);
        Ok(())
    }

    /// Run `m`'s self-test on a background thread (it loads the whole model), recording the outcome in its folder.
    pub fn start_test(&mut self, home: PathBuf, m: Manifest, activity: &Activity) -> Result<(), String> {
        self.refuse_second()?;
        let mut job = Job::new(JobKind::Test, 0);
        let task = activity.start("contentTest", "Testing search model", Cancel::No);
        task.detail(&m.name);
        job.task = Some(task.handle());
        let job = Arc::new(job);
        let worker = job.clone();
        let id = m.id.clone();
        std::thread::Builder::new()
            .name("content-model-test".into())
            .spawn(move || {
                let _task = task;
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| test_and_record(&home, &m)));
                worker.set(match outcome {
                    Ok(Ok(v)) => JobState::Done(v),
                    Ok(Err(e)) => JobState::Failed(e),
                    Err(_) => JobState::Failed("the self-test stopped unexpectedly".into()),
                });
            })
            .map_err(|e| format!("could not start the self-test: {e}"))?;
        self.jobs.insert(id, job);
        Ok(())
    }
}

impl Drop for Jobs {
    /// Quitting while a download runs stops it instead of leaving it behind.
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.cancel.store(true, Ordering::SeqCst);
        }
        let started = std::time::Instant::now();
        while self.running().is_some() && started.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Remove what a stopped download left: every `.part`, the files it fetched (not those that were there before), and
/// the folder when it made it and nothing else is in it.
fn clean_up(req: &DownloadRequest, existed: &[bool], made_home: bool) {
    for (f, was) in req.manifest.files.iter().zip(existed) {
        let _ = std::fs::remove_file(req.home.join(format!("{}.part", f.name)));
        if !was {
            let _ = std::fs::remove_file(req.home.join(&f.name));
        }
    }
    if made_home {
        // (fails, harmlessly, when something else is in it)
        let _ = std::fs::remove_dir(&req.home);
    }
}

/// What went wrong, in words the user can act on.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn explain(e: &lightcraft_fetch::DownloadError) -> String {
    use lightcraft_fetch::DownloadError;
    match e {
        DownloadError::AllMirrorsFailed { file, errors } => {
            let detail: String = errors.join("; ").chars().take(300).collect();
            format!("could not download {file} ({detail}): check the internet connection and try again")
        }
        DownloadError::Disk(why) => format!("could not save the model ({why}): is the disk full or the folder read-only?"),
        DownloadError::NoMirrors => "LightCraft has no download location for this model: add a mirror (see docs/content-search.md)".into(),
        other => other.to_string(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch(req: &DownloadRequest, job: &Job) -> Result<(), String> {
    let specs: Vec<lightcraft_fetch::FileSpec<'_>> = req
        .manifest
        .files
        .iter()
        .map(|f| lightcraft_fetch::FileSpec { name: &f.name, size: Some(f.size), sha256: Some(&f.sha256), max: f.size })
        .collect();
    let total = manifest::total_size(&req.manifest);
    let r = lightcraft_fetch::download(&specs, &req.mirrors, &req.home, &lightcraft_fetch::Options::default(), &job.cancel, &mut |p| {
        job.set(JobState::Running { bytes: p.done.min(total), total });
    });
    match r {
        Ok(()) => Ok(()),
        Err(lightcraft_fetch::DownloadError::Cancelled) => Err(String::new()),
        Err(e) => Err(explain(&e)),
    }
}

#[cfg(target_arch = "wasm32")]
fn fetch(_: &DownloadRequest, _: &Job) -> Result<(), String> {
    Err("the web build downloads no models".into())
}

/// The whole download: fetch and check, then write the manifest and the acceptance. Always ends in a final state.
fn run_download(req: &DownloadRequest, job: &Job) {
    let existed: Vec<bool> = req.manifest.files.iter().map(|f| req.home.join(&f.name).is_file()).collect();
    let made_home = !req.home.exists();
    if let Err(e) = std::fs::create_dir_all(&req.home) {
        job.set(JobState::Failed(format!("could not create the model's folder (is the disk full or read-only?): {e}")));
        return;
    }
    let fetched = fetch(req, job);
    if job.cancel.load(Ordering::SeqCst) || fetched.as_ref().is_err_and(String::is_empty) {
        clean_up(req, &existed, made_home);
        job.set(JobState::Cancelled);
        return;
    }
    if let Err(e) = fetched {
        // the partial files stay, so Try Again resumes (a damaged one was already deleted)
        job.set(JobState::Failed(e));
        return;
    }
    let written = serde_json::to_vec_pretty(&req.manifest)
        .map_err(|e| e.to_string())
        .and_then(|b| write_atomic(&req.home.join(MANIFEST_FILE), &b))
        .and_then(|()| serde_json::to_vec_pretty(&req.record).map_err(|e| e.to_string()))
        .and_then(|b| write_atomic(&req.home.join(RECORD_FILE), &b));
    job.set(match written {
        Ok(()) => JobState::Done(req.record.clone()),
        Err(e) => JobState::Failed(e),
    });
}

/// Run the self-test and keep its outcome (passed or not) in the model's `installed.json`.
pub fn test_and_record(home: &Path, m: &Manifest) -> Result<Value, String> {
    let result = self_test(home, m);
    let entry = match &result {
        Ok(v) => v.clone(),
        Err(e) => json!({"ok": false, "error": e}),
    };
    let file = home.join(RECORD_FILE);
    let mut record = read_capped(&file, MAX_SMALL).and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or_else(|| json!({}));
    if let Some(o) = record.as_object_mut() {
        o.insert("selfTest".into(), entry);
        write_atomic(&file, &serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?)?;
    }
    result
}

/// The procedural scenes the self-test embeds, with the text that should match each best.
const SELF_TEST: &[(lightcraft_scenes::Kind, &str)] =
    &[(lightcraft_scenes::Kind::Beach, "a beach"), (lightcraft_scenes::Kind::MistyForest, "a forest"), (lightcraft_scenes::Kind::Macro, "a flower")];

/// Load the model and check it understands photos: each of a few procedurally generated scenes must match its own
/// description better than the others. Measures the load and how long an image and a text take.
#[cfg(not(target_arch = "wasm32"))]
pub fn self_test(home: &Path, m: &Manifest) -> Result<Value, String> {
    use lightcraft_vision::{Rgb8, Towers};
    let started = std::time::Instant::now();
    let model = lightcraft_vision::load(home, m, Towers::BOTH).map_err(|e| format!("the model does not load: {e}"))?;
    let load_ms = started.elapsed().as_secs_f64() * 1e3;
    let scenes = lightcraft_scenes::demo_library();
    let mut images = Vec::new();
    for (kind, _) in SELF_TEST {
        let s = scenes.iter().find(|s| s.kind == *kind).ok_or("a test scene is missing")?;
        let rgba = s.render(384, 256).to_srgb8();
        images.push(Rgb8 { width: rgba.width, height: rgba.height, data: rgba.data.iter().map(|p| [p[0], p[1], p[2]]).collect() });
    }
    let texts: Vec<&str> = SELF_TEST.iter().map(|(_, t)| *t).collect();
    let t = std::time::Instant::now();
    let img = model.embed_images(&images).map_err(|e| e.to_string())?;
    let ms_per_image = t.elapsed().as_secs_f64() * 1e3 / images.len().max(1) as f64;
    let t = std::time::Instant::now();
    let txt = model.embed_texts(&texts).map_err(|e| e.to_string())?;
    let ms_per_text = t.elapsed().as_secs_f64() * 1e3 / texts.len().max(1) as f64;
    let mut checks = Vec::new();
    let mut ok = img.len() == texts.len() && txt.len() == texts.len();
    for (i, e) in img.iter().enumerate() {
        let scores: Vec<f32> = txt.iter().map(|t| model.score(e, t)).collect();
        let best = scores.iter().enumerate().fold((0usize, f32::NEG_INFINITY), |b, (j, s)| if *s > b.1 { (j, *s) } else { b });
        let right = best.0 == i && scores.iter().all(|s| s.is_finite());
        ok &= right;
        checks.push(json!({"text": texts.get(i), "best": texts.get(best.0), "score": scores.get(i), "ok": right}));
    }
    let cpu = lightcraft_vision::best_device().is_cpu();
    let round = |v: f64| (v * 10.0).round() / 10.0;
    let result = json!({
        "ok": ok,
        "device": if cpu { "cpu" } else { "gpu" },
        "loadMs": round(load_ms),
        "msPerImage": round(ms_per_image),
        "cpuMsPerImage": cpu.then(|| round(ms_per_image)),
        "msPerText": round(ms_per_text),
        "checks": checks,
    });
    if ok {
        Ok(result)
    } else {
        let wrong: Vec<String> = checks.iter().filter(|c| c["ok"] != true).filter_map(|c| c["text"].as_str().map(|t| format!("“{t}”"))).collect();
        Err(format!("the model failed its self-test: it did not match {} to its picture", wrong.join(", ")))
    }
}

#[cfg(target_arch = "wasm32")]
pub fn self_test(_: &Path, _: &Manifest) -> Result<Value, String> {
    Err("content models are not supported in this build".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_known_models_take_this_versions_tuning() {
        for b in manifest::builtin() {
            let mut old = b.clone();
            old.tag_threshold = 0.3;
            old.match_threshold = 0.4;
            old.hint = None;
            if old.family == manifest::Family::Clip {
                old.score.offset = Some(1.0);
            }
            let t = tuned(old.clone());
            assert_eq!((t.tag_threshold, t.match_threshold, t.score, t.hint), (b.tag_threshold, b.match_threshold, b.score, b.hint), "{}", b.id);
            assert_eq!(t.files, old.files, "the files are what was installed");
            // another model under a known id's name (a different family) and unknown ids are left alone
            let mut other = old.clone();
            other.family = if b.family == manifest::Family::Clip { manifest::Family::Siglip } else { manifest::Family::Clip };
            assert_eq!(tuned(other.clone()), other);
            other.id = "mine".into();
            assert_eq!(tuned(other.clone()), other);
        }
    }

    #[test]
    fn hosts_come_from_addresses() {
        assert_eq!(host("https://huggingface.co/google/x/resolve/abc"), "huggingface.co");
        assert_eq!(host("http://127.0.0.1:8080/m"), "127.0.0.1:8080");
        assert_eq!(host("https://user@example.org"), "example.org");
        assert_eq!(host(""), "");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_full_disk_is_explained() {
        let e = explain(&lightcraft_fetch::DownloadError::Disk("No space left on device".into()));
        assert!(e.contains("disk full"), "{e}");
    }
}
