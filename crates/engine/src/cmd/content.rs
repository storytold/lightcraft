//! Content search commands (`content.*`): the models that let photos be searched by what they show (which exist,
//! downloading, adding, checking, choosing, testing and removing them), indexing (on/off, pace, the status, the scan's
//! pump, re-indexing and clearing the index) and the searches (`content.search`, `content.similar`).
//!
//! The folders and jobs are [`crate::content_models`]; the index and the scan are [`super::content_scan`]; this module
//! is the commands over them. Model folders and
//! manifests are hostile input: ids are validated before they become folder names, every file is checked against its
//! size and SHA-256 before a model is used, and nothing outside a model's own folder is ever touched.

use std::path::{Path, PathBuf};

use lightcraft_faces::hash::sha256_file;
use lightcraft_vision::manifest::{self, DEFAULT_MODEL, MAX_MANIFEST_BYTES, Manifest};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::content_models::{
    self as cm, DownloadRequest, Found, JobKind, JobState, MANIFEST_FILE, OnDisk, RECORD_FILE, Settings, read_capped, read_settings, write_atomic,
    write_settings,
};
use crate::{EngineError, Result, Session};

/// Whether this build can run content models (the web build cannot).
const RUNTIME: bool = cfg!(not(target_arch = "wasm32"));

fn fail(what: &str, e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("{what}: {e}"))
}

fn models_dir(s: &Session, c: &str) -> Result<PathBuf> {
    s.content_models_dir.clone().ok_or_else(|| bad(c, "this build has nowhere to keep content models (the desktop app has)"))
}

/// What is on disk and known, read afresh (a few small files).
struct View {
    dir: PathBuf,
    settings: Settings,
    catalog: cm::Catalog,
    known: Vec<Manifest>,
    disk: Vec<OnDisk>,
    errors: Vec<String>,
}

fn view(dir: &Path) -> View {
    let catalog = cm::read_catalog(dir);
    let known = cm::known(&catalog);
    let (disk, errors) = cm::on_disk(dir, &known);
    View { dir: dir.to_path_buf(), settings: read_settings(dir), catalog, known, disk, errors }
}

impl View {
    fn installed(&self, id: &str) -> Option<&OnDisk> {
        self.disk.iter().find(|d| d.manifest.id == id && d.state == Found::Installed)
    }

    /// The model in use: the chosen one when it is installed.
    fn model(&self) -> Option<&OnDisk> {
        self.settings.model.as_deref().and_then(|id| self.installed(id))
    }
}

/// Act on downloads that have finished: a model that arrived is chosen when none is (or the chosen one is gone).
pub(crate) fn settle(s: &mut Session) {
    let Some(dir) = s.content_models_dir.clone() else { return };
    let done = s.content_jobs.newly_installed();
    if done.is_empty() {
        return;
    }
    let v = view(&dir);
    if v.model().is_none()
        && let Some(id) = done.iter().find(|id| v.installed(id).is_some())
    {
        let mut st = v.settings.clone();
        st.model = Some(id.clone());
        if let Err(e) = write_settings(&dir, &st) {
            log::warn!("content models: {e}");
        }
    }
}

fn job_row(s: &Session, id: &str) -> Value {
    let Some((kind, state)) = s.content_jobs.get(id) else { return Value::Null };
    let mut v = json!({"kind": kind});
    match state {
        JobState::Running { bytes, total } => {
            v["state"] = json!("running");
            v["bytes"] = json!(bytes);
            v["total"] = json!(total);
        }
        JobState::Done(r) => {
            v["state"] = json!("done");
            v["result"] = r;
        }
        JobState::Failed(e) => {
            v["state"] = json!("failed");
            v["error"] = json!(e);
        }
        JobState::Cancelled => v["state"] = json!("cancelled"),
    }
    v
}

/// The open library's index file for model `m`: `{photos, bytes}` (photos counted from its header and size, superseded
/// records included), or null when there is none.
#[cfg(not(target_arch = "wasm32"))]
fn library_index(s: &Session, m: &Manifest) -> Value {
    let Some(lib) = s.library.as_ref().filter(|l| l.on_disk) else { return Value::Null };
    let path = crate::content_index::file_path(&lib.dir, &m.id);
    match (crate::content_index::records_in(&path), std::fs::metadata(&path)) {
        (Some(n), Ok(meta)) => json!({"photos": n, "bytes": meta.len()}),
        _ => Value::Null,
    }
}

/// The web build has no content index.
#[cfg(target_arch = "wasm32")]
fn library_index(_: &Session, _: &Manifest) -> Value {
    Value::Null
}

fn row(s: &Session, v: &View, m: &Manifest, disk: Option<&OnDisk>) -> Value {
    let builtin = manifest::builtin().iter().any(|b| b.id == m.id);
    let from_catalog = v.catalog.models.iter().any(|c| c.id == m.id);
    let (state, problem) = match disk.map(|d| &d.state) {
        Some(Found::Installed) => ("installed", None),
        Some(Found::Unverified) => ("found", None),
        Some(Found::Damaged(why)) => ("damaged", Some(why.clone())),
        None => ("available", None),
    };
    let record = disk.map_or(Value::Null, |d| d.record.clone());
    let mirrors = if builtin || from_catalog { cm::mirrors_for(&v.dir, &m.id) } else { Vec::new() };
    json!({
        "id": m.id,
        "name": m.name,
        "family": m.family,
        "version": m.version,
        "licence": m.licence,
        "publisher": builtin.then_some(match m.family {
            manifest::Family::Siglip => "Google",
            manifest::Family::Clip => "OpenAI",
        }),
        "source": m.source,
        "sizeBytes": manifest::total_size(m),
        "embeddingDim": m.embedding.dim,
        "imageSize": m.image.size,
        "default": m.id == DEFAULT_MODEL,
        "known": builtin || from_catalog,
        "fromCatalog": from_catalog,
        // this build has the towers (both families run natively)
        "supported": RUNTIME,
        // quality tier, time per photo relative to the default model, English only
        "hint": m.hint,
        // the open library's index for this model (each model keeps its own; switching back needs no new scan)
        "libraryIndex": library_index(s, m),
        "state": state,
        "problem": problem,
        "installed": state == "installed",
        "selected": state == "installed" && v.settings.model.as_deref() == Some(m.id.as_str()),
        "accepted": record,
        "selfTest": record.get("selfTest").cloned().unwrap_or(Value::Null),
        "speed": {"cpuMsPerImage": record.pointer("/selfTest/cpuMsPerImage").cloned().unwrap_or(Value::Null)},
        // where Download fetches it from (null: nowhere, add it by hand)
        "downloadHost": mirrors.first().map(|u| cm::host(u)),
        "job": job_row(s, &m.id),
    })
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    settle(s);
    let Some(dir) = s.content_models_dir.clone() else {
        return Ok(
            json!({"dir": null, "enabled": false, "model": null, "runtime": RUNTIME, "models": [], "catalog": {"path": null, "models": 0, "errors": []}}),
        );
    };
    let v = view(&dir);
    let mut models: Vec<Value> = v.known.iter().map(|m| row(s, &v, m, v.disk.iter().find(|d| d.manifest.id == m.id))).collect();
    for d in v.disk.iter().filter(|d| !v.known.iter().any(|k| k.id == d.manifest.id)) {
        models.push(row(s, &v, &d.manifest, Some(d)));
    }
    let mut errors = v.catalog.errors.clone();
    errors.extend(v.errors.iter().cloned());
    Ok(json!({
        "dir": dir.display().to_string(),
        "enabled": v.settings.enabled && v.model().is_some(),
        "model": v.model().map(|d| d.manifest.id.clone()),
        "runtime": RUNTIME,
        "running": s.content_jobs.running().map(|(id, kind)| json!({"id": id, "kind": kind})),
        "models": models,
        "catalog": {"path": dir.join(cm::CATALOG_FILE).display().to_string(), "models": v.catalog.models.len(), "errors": errors},
    }))
}

fn acknowledged(p: &Value) -> bool {
    p.get("acknowledged").and_then(Value::as_bool) == Some(true)
}

const NOT_ACCEPTED: &str =
    "the licence has not been accepted: show the user the model's terms (see `content.models.list`) and pass `acknowledged: true` once they agree";

fn record(s: &Session, m: &Manifest) -> Value {
    json!({"acceptedAt": (s.clock)(), "licence": m.licence.name, "commercial": m.licence.commercial, "source": m.source})
}

/// `content.models.download {id, acknowledged: true}`: fetch a known model in the background (or, when its files are
/// already in its folder, only check them), then install it.
fn download(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.models.download";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let dir = models_dir(s, C)?;
    if !RUNTIME {
        return Err(bad(C, "the web build downloads no models"));
    }
    let v = view(&dir);
    let m =
        v.known.iter().find(|m| m.id == id).cloned().ok_or_else(|| bad(C, "LightCraft knows no model with that id (see `content.models.list`)"))?;
    if !acknowledged(p) {
        return Err(bad(C, NOT_ACCEPTED));
    }
    if v.installed(id).is_some() {
        return Err(bad(C, "that model is already installed"));
    }
    let mirrors = cm::mirrors_for(&dir, id);
    if mirrors.is_empty() {
        return Err(bad(C, format!("LightCraft has no download location for {id}: add a mirror ({}) or add the folder yourself", cm::MIRRORS_ENV)));
    }
    let from = cm::host(mirrors.first().map_or("", String::as_str));
    let check_only = v.disk.iter().any(|d| d.manifest.id == id && d.state == Found::Unverified);
    let req = DownloadRequest { home: dir.join(id), mirrors, record: record(s, &m), manifest: m.clone() };
    s.content_jobs.start_download(req, &s.activity, &m.name).map_err(|e| bad(C, e))?;
    Ok(json!({"started": id, "from": from, "checkOnly": check_only}))
}

/// `content.models.downloadCancel {id}`: stop a download (it leaves nothing behind), or clear a finished one.
fn download_cancel(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.models.downloadCancel";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let running = s.content_jobs.get(id).is_some_and(|(k, st)| k == JobKind::Download && matches!(st, JobState::Running { .. }));
    let failed = s.content_jobs.get(id).is_some_and(|(k, st)| k == JobKind::Download && matches!(st, JobState::Failed(_)));
    let had = s.content_jobs.cancel(id);
    // a failed download kept its partial files for Try Again: clearing it throws them away
    if failed && let (Some(dir), true) = (s.content_models_dir.clone(), manifest::valid_id(id)) {
        let v = view(&dir);
        if let Some(m) = v.known.iter().find(|m| m.id == id) {
            for f in &m.files {
                let _ = std::fs::remove_file(dir.join(id).join(format!("{}.part", f.name)));
            }
        }
    }
    Ok(json!({"cancelled": running, "discarded": had}))
}

/// The manifest for a folder being added: its `content-model.json`, else the known model whose files are all there at
/// their sizes.
fn manifest_for(src: &Path, known: &[Manifest], c: &str) -> Result<Manifest> {
    let file = src.join(MANIFEST_FILE);
    if file.exists() {
        let bytes = read_capped(&file, MAX_MANIFEST_BYTES).ok_or_else(|| bad(c, format!("{MANIFEST_FILE} cannot be read or is too large")))?;
        return manifest::parse(&bytes).map_err(|e| bad(c, format!("{MANIFEST_FILE}: {e}")));
    }
    let fits = |m: &&Manifest| m.files.iter().all(|f| std::fs::metadata(src.join(&f.name)).is_ok_and(|md| md.is_file() && md.len() == f.size));
    known.iter().find(fits).cloned().ok_or_else(|| {
        bad(c, format!("the folder has no {MANIFEST_FILE} and is not a model LightCraft knows (its files are missing or not the expected sizes)"))
    })
}

/// Copy (or, in place, only check) every file of `m` from `src` into `home`, each checked against its size and SHA-256.
/// On failure nothing it made is left.
fn place_files(src: &Path, home: &Path, m: &Manifest, c: &str) -> Result<()> {
    let in_place = src.canonicalize().ok().is_some_and(|a| home.canonicalize().ok().is_some_and(|b| a == b));
    let made_home = !home.exists();
    if !in_place {
        std::fs::create_dir_all(home).map_err(|e| fail("could not create the model's folder (is the disk full or read-only?)", e))?;
    }
    // every file is copied to `<name>.part` and checked before any is renamed into place: a damaged
    // file in a second folder of an installed model leaves the installed files as they were
    let mut parts: Vec<PathBuf> = Vec::new();
    let undo = |paths: &[PathBuf]| {
        for p in paths {
            let _ = std::fs::remove_file(p);
        }
        if made_home {
            let _ = std::fs::remove_dir(home);
        }
    };
    for f in &m.files {
        let from = src.join(&f.name);
        let checked = if in_place {
            from.clone()
        } else {
            let part = home.join(format!("{}.part", f.name));
            if let Err(e) = std::fs::copy(&from, &part) {
                let _ = std::fs::remove_file(&part);
                undo(&parts);
                return Err(fail(&format!("could not copy {} (is the disk full?)", f.name), e));
            }
            part
        };
        let size = std::fs::metadata(&checked).map(|md| md.len()).unwrap_or(0);
        let sha = if size == f.size { sha256_file(&checked).ok() } else { None };
        if sha.as_deref() != Some(f.sha256.as_str()) {
            if !in_place {
                let _ = std::fs::remove_file(&checked);
            }
            undo(&parts);
            return Err(bad(c, format!("{} is damaged or not the model's file (its size or SHA-256 does not match)", f.name)));
        }
        if !in_place {
            parts.push(checked);
        }
    }
    let mut placed: Vec<PathBuf> = Vec::new();
    for (i, (part, f)) in parts.iter().zip(&m.files).enumerate() {
        let target = home.join(&f.name);
        if let Err(e) = std::fs::rename(part, &target) {
            undo(parts.get(i..).unwrap_or_default());
            undo(&placed);
            return Err(fail("could not finish the copy", e));
        }
        placed.push(target);
    }
    Ok(())
}

/// `content.models.install {path, acknowledged: true, activate?: true}`: add a model folder.
fn install(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.models.install";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path`"))?;
    let dir = models_dir(s, C)?;
    let src = PathBuf::from(path);
    if !src.is_dir() {
        return Err(bad(C, format!("`{path}` is not a folder")));
    }
    let v = view(&dir);
    let m = manifest_for(&src, &v.known, C)?;
    // a manifest under a known model's id must describe that model's files
    if let Some(k) = v.known.iter().find(|k| k.id == m.id)
        && k.files != m.files
    {
        return Err(bad(C, format!("this folder's {MANIFEST_FILE} uses the id `{}` of another model", m.id)));
    }
    if !acknowledged(p) {
        return Err(bad(C, NOT_ACCEPTED));
    }
    if s.content_jobs.running().is_some_and(|(id, _)| id == m.id) {
        return Err(bad(C, "that model is downloading or being tested: wait for it, or cancel it first"));
    }
    let home = dir.join(&m.id);
    place_files(&src, &home, &m, C)?;
    write_atomic(&home.join(MANIFEST_FILE), &serde_json::to_vec_pretty(&m).map_err(|e| fail("manifest", e))?).map_err(|e| bad(C, e))?;
    let rec = record(s, &m);
    write_atomic(&home.join(RECORD_FILE), &serde_json::to_vec_pretty(&rec).map_err(|e| fail("record", e))?).map_err(|e| bad(C, e))?;
    let mut st = read_settings(&dir);
    if p.get("activate").and_then(Value::as_bool).unwrap_or(true) || view(&dir).model().is_none() {
        st.model = Some(m.id.clone());
        write_settings(&dir, &st).map_err(|e| bad(C, e))?;
    }
    #[cfg(not(target_arch = "wasm32"))]
    super::content_scan::refresh(s, true);
    let v = view(&dir);
    let installed = row(s, &v, &m, v.disk.iter().find(|d| d.manifest.id == m.id));
    Ok(json!({"installed": installed, "model": v.model().map(|d| d.manifest.id.clone())}))
}

/// `content.models.select {id | null}`.
fn select(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.models.select";
    let dir = models_dir(s, C)?;
    let id = match p.get("id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().ok_or_else(|| bad(C, "`id` must be a model id or null"))?.to_string()),
    };
    let v = view(&dir);
    if let Some(id) = &id
        && v.installed(id).is_none()
    {
        return Err(bad(C, "that model is not installed"));
    }
    let mut st = v.settings;
    st.enabled &= id.is_some();
    st.model = id;
    write_settings(&dir, &st).map_err(|e| bad(C, e))?;
    // a scan of the old model stops now, and indexing goes on with the new one's index
    #[cfg(not(target_arch = "wasm32"))]
    super::content_scan::refresh(s, true);
    Ok(json!({"model": st.model, "enabled": st.enabled}))
}

/// `content.models.remove {id}`: delete a model's folder.
fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.models.remove";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let dir = models_dir(s, C)?;
    if !manifest::valid_id(id) {
        return Err(bad(C, "not a model id"));
    }
    if s.content_jobs.running().is_some_and(|(r, _)| r == id) {
        return Err(bad(C, "that model is downloading or being tested: cancel it or wait first"));
    }
    let v = view(&dir);
    // only a folder that is a model (listed as one) is ever deleted
    if !v.disk.iter().any(|d| d.manifest.id == id) {
        return Err(bad(C, "that model is not installed"));
    }
    std::fs::remove_dir_all(dir.join(id)).map_err(|e| fail("could not remove the model", e))?;
    let mut st = v.settings;
    if st.model.as_deref() == Some(id) {
        // another installed model takes over; with none left nothing is chosen and indexing is off
        let now = view(&dir);
        st.model = now.disk.iter().find(|d| d.state == Found::Installed).map(|d| d.manifest.id.clone());
        st.enabled &= st.model.is_some();
        write_settings(&dir, &st).map_err(|e| bad(C, e))?;
    }
    s.content_jobs.cancel(id);
    #[cfg(not(target_arch = "wasm32"))]
    super::content_scan::refresh(s, true);
    Ok(json!({"removed": id, "model": st.model, "enabled": st.enabled}))
}

/// `content.models.test {id, wait?}`: load an installed model and check it matches pictures to words.
fn test(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.models.test";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let dir = models_dir(s, C)?;
    if !RUNTIME {
        return Err(bad(C, "this build cannot run content models"));
    }
    let v = view(&dir);
    let found = v.installed(id).ok_or_else(|| bad(C, "that model is not installed"))?;
    let (home, m) = (dir.join(id), found.manifest.clone());
    if p.get("wait").and_then(Value::as_bool) == Some(true) {
        if s.content_jobs.running().is_some() {
            return Err(bad(C, "another model job is running: wait for it"));
        }
        let _row = s.activity.start("contentTest", "Testing search model", crate::activity::Cancel::No);
        return Ok(match cm::test_and_record(&home, &m) {
            Ok(r) => json!({"id": id, "ok": true, "result": r}),
            Err(e) => json!({"id": id, "ok": false, "error": e}),
        });
    }
    s.content_jobs.start_test(home, m, &s.activity).map_err(|e| bad(C, e))?;
    Ok(json!({"id": id, "started": true}))
}

/// `content.enable {on}` (or `enabled`; toggles when omitted): whether photos are indexed for search.
fn enable(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.enable";
    let dir = models_dir(s, C)?;
    let v = view(&dir);
    let on = p.get("on").or_else(|| p.get("enabled")).and_then(Value::as_bool).unwrap_or(!v.settings.enabled);
    if on && v.model().is_none() {
        return Err(bad(C, "choose a content model first (Settings ▸ Search ▸ Use)"));
    }
    let mut st = v.settings;
    st.enabled = on;
    write_settings(&dir, &st).map_err(|e| bad(C, e))?;
    // switched on: a scan the user stopped (or one a model error stopped) starts again; off: it stops at once
    #[cfg(not(target_arch = "wasm32"))]
    {
        if on {
            super::content_scan::resume(s);
        }
        super::content_scan::refresh(s, true);
        super::content_scan::step(s, false);
    }
    Ok(json!({"enabled": st.enabled}))
}

/// `content.pace {pace: low | normal | fast}`: how hard indexing works.
fn pace(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "content.pace";
    let dir = models_dir(s, C)?;
    let name = str_param(p, "pace").ok_or_else(|| bad(C, "missing `pace` (low, normal or fast)"))?;
    if !matches!(name, "low" | "normal" | "fast") {
        return Err(bad(C, "`pace` is low, normal or fast"));
    }
    let mut st = read_settings(&dir);
    st.pace = (name != "normal").then(|| name.to_string());
    write_settings(&dir, &st).map_err(|e| bad(C, e))?;
    #[cfg(not(target_arch = "wasm32"))]
    super::content_scan::refresh(s, true);
    Ok(json!({"pace": name}))
}

/// `content.status`: the model in use, whether indexing is on, and how far the index has got.
fn status(s: &mut Session, _: &Value) -> Result<Value> {
    settle(s);
    let v = s.content_models_dir.as_deref().map(view);
    let model = v.as_ref().and_then(View::model);
    let mut out = json!({
        "runtime": RUNTIME,
        "dir": v.as_ref().map(|v| v.dir.display().to_string()),
        "model": model.map(|d| d.manifest.id.clone()),
        "modelName": model.map(|d| d.manifest.name.clone()),
        "installed": v.as_ref().map_or(0, |v| v.disk.iter().filter(|d| d.state == Found::Installed).count()),
        "enabled": v.as_ref().is_some_and(|v| v.settings.enabled) && model.is_some(),
        "ready": RUNTIME && model.is_some(),
        "job": s.content_jobs.running().map(|(id, kind)| json!({"id": id, "kind": kind})),
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        super::content_scan::step(s, false);
        let index = super::content_scan::report(s);
        if let (Some(o), Value::Object(more)) = (out.as_object_mut(), index) {
            o.extend(more);
        }
    }
    #[cfg(target_arch = "wasm32")]
    if let Some(o) = out.as_object_mut() {
        for k in ["indexed", "total", "stale", "etaSeconds", "msPerPhoto", "device", "textDevice", "gpuFallback", "index", "error"] {
            o.insert(k.into(), Value::Null);
        }
        o.insert("indexing".into(), json!(false));
    }
    Ok(out)
}

#[cfg(not(target_arch = "wasm32"))]
mod scan {
    use serde_json::Value;

    use super::super::content_scan as cs;
    use super::super::{bad, str_param};
    use crate::{Result, Session};

    fn limit(p: &Value, default: usize) -> usize {
        p.get("limit").and_then(Value::as_u64).map_or(default, |n| usize::try_from(n).unwrap_or(cs::MAX_RESULTS)).clamp(1, cs::MAX_RESULTS)
    }

    fn min(p: &Value, c: &str) -> Result<f32> {
        match p.get("min") {
            None | Some(Value::Null) => Ok(f32::NEG_INFINITY),
            Some(v) => v.as_f64().filter(|m| m.is_finite()).map(|m| m as f32).ok_or_else(|| bad(c, "`min` must be a number")),
        }
    }

    pub fn search(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "content.search";
        let text = str_param(p, "text").map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(C, "missing `text`"))?;
        if text.chars().count() > 500 {
            return Err(bad(C, "`text` is at most 500 characters"));
        }
        let (limit, min) = (limit(p, 100), min(p, C)?);
        cs::search(s, text, limit, min)
    }

    pub fn similar(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "content.similar";
        let id = p.get("id").and_then(Value::as_u64).or_else(|| s.active().map(|a| a.0)).ok_or_else(|| bad(C, "missing `id`"))?;
        let (limit, min) = (limit(p, 50), min(p, C)?);
        let mut out = cs::similar(s, id, limit, min)?;
        if p.get("show").and_then(Value::as_bool) == Some(true) {
            // the view shows them, ranked, until the chip is closed
            s.clear_content_query();
            s.content_query.similar = Some(lightcraft_catalog::PhotoId(id));
            out["count"] = serde_json::json!(s.visible().len());
            out["contents"] = s.content_view().map(|v| v.to_json()).unwrap_or_default();
        }
        Ok(out)
    }

    pub fn prepare(s: &mut Session, _: &Value) -> Result<Value> {
        Ok(cs::prepare_search(s))
    }

    pub fn pump(s: &mut Session, p: &Value) -> Result<Value> {
        cs::pump(s, p)
    }

    pub fn reindex(s: &mut Session, p: &Value) -> Result<Value> {
        const C: &str = "content.reindex";
        let ids = match p.get("ids") {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) => {
                Some(a.iter().map(|v| v.as_u64().ok_or_else(|| bad(C, "`ids` must be photo ids"))).collect::<Result<Vec<u64>>>()?)
            }
            Some(_) => return Err(bad(C, "`ids` must be a list of photo ids")),
        };
        let n = cs::reindex(s, ids);
        cs::step(s, false);
        Ok(serde_json::json!({"queued": n}))
    }

    pub fn clear(s: &mut Session, _: &Value) -> Result<Value> {
        let r = cs::clear(s)?;
        cs::step(s, false);
        Ok(r)
    }

    pub(crate) use super::super::content_tags::{
        accept as accept_tags, counts as tag_counts, dismiss as dismiss_tags, options as tag_options, set_vocabulary, tags,
        vocabulary_cmd as vocabulary,
    };
}

#[cfg(target_arch = "wasm32")]
mod scan {
    use serde_json::Value;

    use super::super::bad;
    use crate::{Result, Session};

    fn unavailable(c: &str) -> Result<Value> {
        Err(bad(c, "content search is not available in the web build"))
    }

    pub fn search(_: &mut Session, _: &Value) -> Result<Value> {
        unavailable("content.search")
    }

    pub fn similar(_: &mut Session, _: &Value) -> Result<Value> {
        unavailable("content.similar")
    }

    pub fn prepare(_: &mut Session, _: &Value) -> Result<Value> {
        Ok(serde_json::json!({"state": "unavailable"}))
    }

    pub fn pump(_: &mut Session, _: &Value) -> Result<Value> {
        Ok(serde_json::json!({"active": false, "indexing": false, "pending": 0}))
    }

    pub fn reindex(_: &mut Session, _: &Value) -> Result<Value> {
        unavailable("content.reindex")
    }

    pub fn clear(_: &mut Session, _: &Value) -> Result<Value> {
        unavailable("content.clearIndex")
    }

    pub fn tags(_: &mut Session, _: &Value) -> Result<Value> {
        Ok(serde_json::json!({"state": "unavailable", "photos": []}))
    }

    pub fn tag_counts(_: &mut Session, _: &Value) -> Result<Value> {
        Ok(serde_json::json!({"state": "unavailable", "tags": []}))
    }

    pub fn accept_tags(_: &mut Session, _: &Value) -> Result<Value> {
        unavailable("content.acceptTags")
    }

    pub fn dismiss_tags(_: &mut Session, _: &Value) -> Result<Value> {
        unavailable("content.dismissTags")
    }

    pub fn vocabulary(_: &mut Session, _: &Value) -> Result<Value> {
        unavailable("content.vocabulary")
    }

    pub fn set_vocabulary(_: &mut Session, _: &Value) -> Result<Value> {
        unavailable("content.setVocabulary")
    }

    pub fn tag_options(_: &mut Session, _: &Value) -> Result<Value> {
        unavailable("content.tagOptions")
    }
}

/// `content.more` / `content.fewer`: widen or narrow the library view's search by contents by one step.
fn widen(s: &mut Session, by: i32, c: &str) -> Result<Value> {
    let view = s.content_view().ok_or_else(|| bad(c, "no search by contents is in view"))?;
    let can = if by > 0 { view.can_more } else { view.can_fewer };
    if can {
        let q = &mut s.content_query;
        q.step = (q.step + by).clamp(crate::content_query::MIN_STEP, crate::content_query::MAX_STEP);
    }
    let count = s.visible().len();
    let view = s.content_view().map(|v| v.to_json()).unwrap_or_default();
    Ok(json!({"count": count, "changed": can, "contents": view}))
}

fn more(s: &mut Session, _: &Value) -> Result<Value> {
    widen(s, 1, "content.more")
}

fn fewer(s: &mut Session, _: &Value) -> Result<Value> {
    widen(s, -1, "content.fewer")
}

fn clear_query(s: &mut Session, _: &Value) -> Result<Value> {
    s.clear_content_query();
    Ok(json!({"count": s.visible().len()}))
}

fn query_status(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(s.content_view().map(|v| v.to_json()).unwrap_or(Value::Null))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "content.models.list", "Content Models", [], None, "{} → {dir, enabled, model, runtime, running, models: [{id, name, family, licence{name, commercial, url}, publisher, source, sizeBytes, embeddingDim, imageSize, default, known, supported, state: available | found | installed | damaged, problem, installed, selected, accepted, selfTest, speed{cpuMsPerImage}, downloadHost, job{kind, state, bytes, total, error}}], catalog{path, models, errors}} — `found`: the files are in the model's folder but not yet checked (download checks them without fetching)", always, list),
        cmd!(query "content.models.download", "Download Content Model", [], None, "{id, acknowledged: true} → {started, from, checkOnly} — fetch a known content model in the background from Hugging Face (or the mirrors in LIGHTCRAFT_VISION_MIRRORS / content-mirrors.txt), checked by size and SHA-256, then install it; files already in its folder are only checked. `acknowledged` must be true: the user has been shown the model's licence and accepted it. One model job runs at a time; progress is in `content.models.list` (`job`) and the activity stack", always, download),
        cmd!(query "content.models.downloadCancel", "Cancel Content Model Download", [], None, "{id} → {cancelled, discarded} — stop a download (it leaves no files behind), or clear a finished or failed one", always, download_cancel),
        cmd!(query "content.models.install", "Add Content Model Folder", [], None, "{path, acknowledged: true, activate?: true} → {installed, model} — add a folder holding a content model (its content-model.json, or the files of a model LightCraft knows); every file is checked against its size and SHA-256 and copied into the models folder (checked in place when it is already there). Unless `activate` is false it becomes the model in use", always, install),
        cmd!(query "content.models.select", "Choose Content Model", [], None, "{id: installed model | null} → {model, enabled}", always, select),
        cmd!(query "content.models.remove", "Remove Content Model", [], None, "{id} → {removed, model, enabled} — delete a model's folder; another installed model takes over", always, remove),
        cmd!(query "content.models.test", "Test Content Model", [], None, "{id, wait?: false} → {started} | (wait) {ok, result: {device, loadMs, msPerImage, cpuMsPerImage, msPerText, checks}, error} — load an installed model and check it matches procedurally generated scenes to their descriptions; the result (and the speed) is kept in its installed.json", always, test),
        cmd!(query "content.enable", "Index Photo Contents On/Off", [], None, "{on?: bool} (toggles when omitted) → {enabled} — needs a chosen, installed content model. On, the library is indexed in the background (an activity row, `content` kind, that can be cancelled); turning it on again resumes a cancelled scan", always, enable),
        cmd!(query "content.status", "Content Search Status", [], None, "{} → {runtime, dir, model, modelName, installed, enabled, ready, job, indexed, total, stale, failed, offline, pending, indexing, stopped, pace: low|normal|fast, msPerPhoto, device, textDevice, gpuFallback, etaSeconds, embeddedThisSession, error, index{file, bytes, memoryBytes, records, deadRecords, note, saveError}} — `indexed` of `total` photos (videos are not indexed) are in the index of the model in use; `stale`: photos whose original changed since (queued again); `failed`: unreadable this session; `offline`: their file is missing (tried again later); `stopped`: the user cancelled the scan (content.enable on resumes it); `device`: what runs the image tower once loaded (\"GPU: <adapter>\" or \"CPU\"; `textDevice` the text tower's), `gpuFallback`: why the GPU stopped running the model (the CPU took over)", always, status),
        cmd!(query "content.pace", "Indexing Pace", [], None, "{pace: low | normal | fast} → {pace} — how much of the computer indexing may use: low (a few threads), normal (half), fast (most)", always, pace),
        cmd!(query "content.pump", "Run Content Indexing", [], None, "{pause?: bool, wait?: bool, timeoutMs?: 600000} → {active, indexing, pending, inFlight, indexed, total, stopped} — one step of the background scan (the app calls it a few times a second; `pause`: the user is busy, start no new batch). With `wait`, runs the scan to its end (or the timeout) and saves the index: what a headless session (CLI, MCP) uses to index a library", always, scan::pump),
        cmd!(query "content.search", "Search Photo Contents", [], None, "{text, limit?: 100, min?} → {text, results: [{id, score}], model, indexed, total, complete, indexing, ms} — photos ranked by how well they show `text` (any language the model knows), best first; `score` is the model's match probability (SigLIP: sigmoid, so a threshold means something; around 0.01 and up is a match for the base model). Works on a partial index: `indexed` of `total` photos were searched. The text model is loaded on the first search (about half a second) and kept while searches come", always, scan::search),
        cmd!(query "content.similar", "Find Similar Contents", [], None, "{id?: active photo, limit?: 50, min?, show?: bool} → {id, results: [{id, score}], model, indexed, total, complete, count?, contents?} — the photos whose contents are most like this photo's (cosine similarity of their embeddings, 1 = the same), the photo itself left out; it must be indexed. `show`: the library view shows them (the photo first), ranked, with a chip \"Similar to …\" (content.clearQuery ends it)", always, scan::similar),
        cmd!(query "content.prepare", "Prepare Search by Contents", [], None, "{} → {state: ready | loading | noModel | error, error?, model, indexed, total, complete, indexing} — load the search model of the model in use in the background (the search bar's Contents mode does this when chosen, so the first search does not wait)", always, scan::prepare),
        cmd!(query "content.more", "Show More Matches", [], None, "{} → {count, changed, contents} — widen the library view's search by contents one step (half the cut-off, or twice as many best matches)", always, more),
        cmd!(query "content.fewer", "Show Fewer Matches", [], None, "{} → {count, changed, contents} — narrow the library view's search by contents one step", always, fewer),
        cmd!(query "content.clearQuery", "Clear Search by Contents", [], None, "{} → {count} — end the library view's search by contents (the Contents text, `contents:` words, Find Similar Contents); the sort goes back to what it was", always, clear_query),
        cmd!(query "content.query", "Search by Contents in View", [], None, "{} → null | {kind: text | similar, text, id, label, state: ready | preparing | problem, problem, matches, bestMatches, step, canMore, canFewer, min, indexed, total, hint: null | {kind: turnOnIndexing | partial, percent}} — the library view's search by contents, as its chip shows it", always, query_status),
        cmd!(query "content.reindex", "Index Photo Contents Again", [], None, "{ids?: [photo ids]} → {queued} — embed these photos (or every photo) again, failed ones included; also restarts a stopped scan. Indexing must be on for it to run", always, scan::reindex),
        cmd!(query "content.clearIndex", "Clear Content Index", [], None, "{} → {cleared, file} — forget the index of the model in use for this library and delete its file; with indexing on it starts over", always, scan::clear),
        cmd!(query "content.tags", "Content Tags", [], None, "{id? | ids?: [photo ids] (default: the selection), wait?: bool, min?: score} → {state: ready | preparing | noIndex | noModel | stopped | error | unavailable, error, threshold, photos: [{id, tags: [{tag, label, score, accepted}]}]} — the auto-tags of these photos, best first: the vocabulary's tags (`animal|bird|owl`) whose score reaches the cut-off (at most 8 per photo; dismissed ones left out). `accepted`: the photo already has a keyword of that name. Tags are worked out in the background from the content index (`preparing` while the vocabulary is embedded or new photos are scored; `wait` waits for them). `min`: every kept score at or above it instead (at most 16 per photo, whatever the cut-off and dismissals), to choose a cut-off", always, scan::tags),
        cmd!(query "content.tagCounts", "Content Tag Counts", [], None, "{} → {state, error, threshold, tags: [{tag, label, count, depth, inVocabulary}]} — every auto-tag of the library with its photo count (a level counts the photos of the tags below it: `animal` includes `animal|bird|owl`), in path order: the Library Filter's Contents column. Pick one with library.filter {contentTag}", always, scan::tag_counts),
        cmd!(
            "content.acceptTags",
            "Accept Content Tags",
            [],
            None,
            "{ids?: [photo ids] (default: the selection), tags: [tag | name | synonym], all?: false} → {changed, keywords} — make auto-tags keywords: each tag becomes the keyword `<parent>|<its name>` (parent `Contents` unless changed with content.tagOptions; written to XMP and exported like any keyword) on the photos it is suggested for (`all`: on every photo given), as one undo step",
            always,
            scan::accept_tags
        ),
        cmd!(query "content.dismissTags", "Dismiss Content Tags", [], None, "{ids?: [photo ids] (default: the selection), tags: [tag | name | synonym]} → {dismissed} — never suggest these tags for these photos again (kept in content-dismissed.bin beside the catalog)", always, scan::dismiss_tags),
        cmd!(query "content.vocabulary", "Content Tag Vocabulary", [], None, "{text?: bool} → {custom, count, hash, notes, state, error, embedded, cacheNote, file, templates, terms: [{tag, label, prompts, threshold}], text?} — the auto-tag vocabulary in use (the built-in English list, or the user's own `vocabulary.txt` in the content models folder); `embedded`: tags whose text embedding is ready for the model in use. `text`: the vocabulary as editable text", always, scan::vocabulary),
        cmd!(query "content.setVocabulary", "Set Content Tag Vocabulary", [], None, "{text? | terms?: [lines] | path? | reset?: true} → {count, custom, notes, hash} — replace the user's auto-tag vocabulary, one tag per line: `animal|bird|owl` (described by its last level), `animal|dog = dog, puppy` (synonyms: one tag, their embeddings averaged; typing any finds it), `… @0.05` (its own cut-off). At most 5000 tags of at most 6 levels; lines that can't be read are skipped with a note. `reset` restores the built-in list. New tags are embedded in the background (an activity row), then the library's tags are worked out again", always, scan::set_vocabulary),
        cmd!(query "content.tagOptions", "Content Tag Options", [], None, "{parent?: keyword | \"\" | null, threshold?: number | null} → {parent, threshold, modelThreshold, custom} — the keyword parent accepted tags go under (default `Contents`; \"\" for none) and the cut-off a tag's score must reach (0–1; null: the model's own)", always, scan::tag_options),
    ]
}
