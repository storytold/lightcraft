//! Denoise commands (`denoise.*`): the models (list, install, download, test, remove, choose), the settings, and the
//! work (queue, cancel, status, pump, clear). The machinery they drive is [`crate::denoise`].
//!
//! Models live in the folder the host provides (`Session::denoise.models_dir`): one subfolder per model with
//! `model.onnx`, `denoise-model.json` (its manifest) and `installed.json` (what the user accepted and the file's hash),
//! plus `settings.json` beside them. Model files, archives and manifests are hostile input: sizes are capped, ids are
//! validated before they become folder names, and nothing outside a model's own folder is ever touched.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, mpsc};

use lightcraft_catalog::PhotoId;
use lightcraft_denoise::archive;
use lightcraft_denoise::hash::sha256_file;
use lightcraft_denoise::known::{self, Known};
use lightcraft_denoise::manifest::{self, DenoiserManifest, MAX_MANIFEST_BYTES, MAX_MODEL_BYTES};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::denoise::{Pace, RunOn, Settings, forget_gpu_set_ups, installed_models, read_capped, read_settings, write_atomic, write_settings};
use crate::model_download::State;
use crate::{EngineError, Result, Session};

fn fail(what: &str, e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("{what}: {e}"))
}

fn models_dir(s: &Session, cmd: &str) -> Result<PathBuf> {
    s.denoise.models_dir.clone().ok_or_else(|| bad(cmd, "this build has nowhere to keep denoise models (the desktop app does)"))
}

/// What a command says about a model.
fn row(m: &DenoiserManifest, download_host: Option<String>, installed: bool, selected: bool, accepted: &Value) -> Value {
    json!({
        "id": m.id,
        "name": m.name,
        "version": m.version,
        "licence": m.licence,
        "provenance": m.provenance,
        "source": m.source,
        "sizeBytes": m.size_bytes,
        "sha256": m.sha256,
        "tile": m.tile,
        "known": known::find(&m.id).is_some(),
        // a pinned address LightCraft can fetch it from (the user presses Download), and which site that is
        "downloadHost": download_host,
        "installed": installed,
        "selected": selected,
        "accepted": accepted,
    })
}

fn host_of(k: &Known) -> Option<String> {
    k.download.as_ref().map(|d| lightcraft_denoise::known::host(&d.url).to_string())
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    s.denoise_refresh_active(true);
    let dir = s.denoise.models_dir.clone();
    let st = dir.as_deref().map(read_settings).unwrap_or_default();
    let installed = dir.as_deref().map(installed_models).unwrap_or_default();
    let on_disk = |id: &str| installed.iter().find(|i| i.manifest.id == id);
    let chosen = |id: &str| st.model.as_deref() == Some(id);
    let mut models: Vec<Value> = Vec::new();
    for k in known::all() {
        let found = on_disk(&k.manifest.id);
        models.push(row(&k.manifest, host_of(&k), found.is_some(), chosen(&k.manifest.id), found.map_or(&Value::Null, |f| &f.accepted)));
    }
    for i in installed.iter().filter(|i| known::find(&i.manifest.id).is_none()) {
        models.push(row(&i.manifest, None, true, chosen(&i.manifest.id), &i.accepted));
    }
    Ok(json!({
        "dir": dir.as_ref().map(|d| d.display().to_string()),
        "productsDir": s.denoise_products_dir().map(|d| d.display().to_string()),
        "model": st.model,
        // whether this build can run denoise models (the `denoise` feature: our pure-Rust CPU/GPU runners)
        "runtime": cfg!(feature = "denoise"),
        "auto": st.auto(),
        "cacheGb": st.cache_bytes() >> 30,
        "threads": st.threads,
        "runOn": st.run_on().name(),
        "models": models,
    }))
}

/// What a file the user pointed at is, as far as installing it goes.
struct Source {
    manifest: DenoiserManifest,
    /// The `.onnx` inside the archive; `None` when the file is the `.onnx`.
    entry: Option<&'static str>,
    file_name: String,
}

fn identify(path: &Path, c: &str, sha_known: Option<&str>) -> Result<Source> {
    let meta = std::fs::metadata(path).map_err(|e| bad(c, format!("cannot read `{}`: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(bad(c, format!("`{}` is not a file", path.display())));
    }
    if meta.len() == 0 || meta.len() > MAX_MODEL_BYTES {
        return Err(bad(c, "a model file must be between 1 byte and 2 GiB"));
    }
    let sha256 = match sha_known {
        Some(h) => h.to_string(),
        None => sha256_file(path).map_err(|e| fail(&format!("could not read {}", path.display()), e))?,
    };
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // an archive LightCraft knows (the file darktable publishes the model in)
    if let Some(k) = known::all().into_iter().find(|k| k.download.as_ref().is_some_and(|d| d.sha256 == sha256)) {
        return Ok(Source { manifest: k.manifest, entry: k.entry, file_name });
    }
    // the `.onnx` of a known model
    if let Some(m) = known::by_sha256(&sha256) {
        return Ok(Source { manifest: m, entry: None, file_name });
    }
    // an `.onnx` with its manifest beside it
    let sibling = path.with_file_name("denoise-model.json");
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("onnx"))
        && let Some(bytes) = read_capped(&sibling, MAX_MANIFEST_BYTES)
    {
        let m = manifest::parse(&bytes).map_err(|e| bad(c, format!("{}: {e}", sibling.display())))?;
        return Ok(Source { manifest: m, entry: None, file_name });
    }
    Err(bad(
        c,
        "LightCraft does not know this file. A denoise model is an .onnx file with a denoise-model.json beside it that says how to use it \
         (see docs/denoise.md), or an archive LightCraft can fetch itself (`denoise.models.download`)",
    ))
}

/// Install the model in `src`. A model that passes its self-test becomes the one in use (`activate`, the default): installing
/// a model is how someone says they want it. `verified` is for a download whose hash was already checked.
fn install_file(
    dir: &Path,
    loader: &crate::denoise::Loader,
    run_on: RunOn,
    accepted_at: String,
    c: &str,
    src: &Path,
    acknowledged: bool,
    verified: bool,
) -> Result<(Value, Arc<dyn crate::denoise::Model>)> {
    let ident = identify(src, c, None)?;
    let m = ident.manifest.clone();
    manifest::validate(&m).map_err(|e| bad(c, e.to_string()))?;
    if !acknowledged {
        return Err(bad(
            c,
            "the licence has not been accepted: show the user its terms (see `denoise.models.list`) and pass `acknowledged: true` once they agree",
        ));
    }
    let home = dir.join(&m.id);
    if home.join("model.onnx").exists() {
        return Err(bad(c, "that model is already installed; remove it before installing a replacement"));
    }
    std::fs::create_dir_all(&home).map_err(|e| fail("could not create the model's folder", e))?;
    let mut cleanup = InstallCleanup { home: home.clone(), armed: true };
    let (part, final_path) = (home.join("model.onnx.part"), home.join("model.onnx"));
    let _ = std::fs::remove_file(&part);
    let placed: Result<()> = match ident.entry {
        Some(entry) => {
            archive::extract(src, entry, &part, MAX_MODEL_BYTES).map(|_| ()).map_err(|e| fail("could not take the model out of the archive", e))
        }
        None if verified && std::fs::rename(src, &part).is_ok() => Ok(()),
        None => std::fs::copy(src, &part).map(|_| ()).map_err(|e| fail("could not copy the model (is the disk full?)", e)),
    };
    let sha256 = placed.and_then(|_| sha256_file(&part).map_err(|e| fail("could not check the model", e))).and_then(|h| match &m.sha256 {
        Some(want) if *want != h => Err(EngineError::Other("the model file is not the one its description names (its SHA-256 differs)".into())),
        _ => Ok(h),
    });
    let sha256 = match sha256 {
        Ok(h) => h,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    // Test the staged file first. A failed replacement preserves the installed model.
    let model = loader(&part, &m).map_err(|e| bad(c, e))?;
    let test = match model.self_test(run_on) {
        Ok(t) => t,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(bad(c, e));
        }
    };
    // An installed id is immutable: installing again must never destroy a usable model.
    if final_path.exists() {
        let _ = std::fs::remove_file(&part);
        return Err(bad(c, "that model is already installed; remove it before installing a replacement"));
    }
    std::fs::rename(&part, &final_path).map_err(|e| fail("could not finish the copy", e))?;
    write_atomic(&home.join("denoise-model.json"), &serde_json::to_vec_pretty(&m).map_err(|e| fail("manifest", e))?).map_err(EngineError::Other)?;
    let accepted = json!({
        "acceptedAt": accepted_at,
        "licence": m.licence.name,
        "commercial": m.licence.commercial,
        "fileName": ident.file_name,
        "sha256": sha256,
        "selfTest": test,
    });
    write_atomic(&home.join("installed.json"), &serde_json::to_vec_pretty(&accepted).map_err(|e| fail("record", e))?).map_err(EngineError::Other)?;
    cleanup.armed = false;
    Ok((json!({"installed": row(&m, known::find(&m.id).and_then(|k| host_of(&k)), true, false, &accepted), "model": m.id}), model))
}

/// Removes only files created for this new installation, including on a worker panic.
struct InstallCleanup {
    home: PathBuf,
    armed: bool,
}
impl Drop for InstallCleanup {
    fn drop(&mut self) {
        if self.armed {
            for name in ["model.onnx.part", "model.onnx", "denoise-model.json", "installed.json"] {
                let _ = std::fs::remove_file(self.home.join(name));
            }
            let _ = std::fs::remove_dir(&self.home);
        }
    }
}

pub(crate) struct InstallJob {
    id: String,
    activate: bool,
    result: mpsc::Receiver<std::result::Result<(Value, Arc<dyn crate::denoise::Model>), String>>,
}

fn activate_install(s: &mut Session, result: &Value, model: Arc<dyn crate::denoise::Model>, activate: bool) -> Result<Value> {
    let dir = models_dir(s, "denoise.models.install")?;
    let id = result["installed"]["id"].as_str().ok_or_else(|| bad("denoise.models.install", "missing installed id"))?;
    let mut st = read_settings(&dir);
    if activate {
        st.model = Some(id.to_string());
        write_settings(&dir, &st).map_err(EngineError::Other)?;
    }
    s.denoise.touch();
    s.denoise_refresh_active(true);
    if let Some(a) = &s.denoise.active
        && a.id == id
    {
        *a.model.lock().unwrap_or_else(PoisonError::into_inner) = Some(model);
    }
    let mut v = result.clone();
    v["installed"]["selected"] = json!(st.model.as_deref() == Some(id));
    v["model"] = json!(st.model);
    Ok(v)
}

fn start_install(s: &mut Session, src: PathBuf, id: String, activate: bool, verified: bool) -> Result<Value> {
    const C: &str = "denoise.models.install";
    if s.denoise.install.is_some() {
        return Err(bad(C, "another model is being installed"));
    }
    let dir = models_dir(s, C)?;
    let loader = s.denoise.loader.clone();
    let run_on = s.denoise.settings.run_on();
    let at = (s.clock)();
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("denoise-model-install".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _held = crate::memory::work_gate().acquire(512 * 1024 * 1024);
                install_file(&dir, &loader, run_on, at, C, &src, true, verified).map_err(|e| plain(&e))
            }))
            .unwrap_or_else(|_| Err("the model installation stopped unexpectedly".into()));
            if verified {
                let _ = std::fs::remove_file(&src);
            }
            let _ = tx.send(outcome);
        })
        .map_err(|e| bad(C, format!("could not start installation: {e}")))?;
    s.denoise.install = Some(InstallJob { id: id.clone(), activate, result: rx });
    s.denoise.downloads.set_outcome(&id, State::Installing);
    Ok(json!({"started": id}))
}

fn install(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "denoise.models.install";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path`"))?;
    let acknowledged = bool_or(p, "acknowledged", false);
    let activate = bool_or(p, "activate", true);
    if !acknowledged {
        return Err(bad(C, "the licence has not been accepted: show its terms first"));
    }
    if bool_or(p, "background", false) {
        return start_install(s, PathBuf::from(path), "local-install".into(), activate, false);
    }
    if s.denoise.install.is_some() {
        return Err(bad(C, "another model is being installed"));
    }
    let (result, model) =
        install_file(&models_dir(s, C)?, &s.denoise.loader, s.denoise.settings.run_on(), (s.clock)(), C, Path::new(path), acknowledged, false)?;
    activate_install(s, &result, model, activate)
}

/// Read only the small adjacent manifest; hashing and execution happen in the install worker.
fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "denoise.models.inspect";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path`"))?;
    let src = Path::new(path);
    if !src.extension().is_some_and(|e| e.eq_ignore_ascii_case("onnx")) {
        return Err(bad(C, "choose an .onnx file with denoise-model.json beside it"));
    }
    let bytes = read_capped(&src.with_file_name("denoise-model.json"), MAX_MANIFEST_BYTES)
        .ok_or_else(|| bad(C, "denoise-model.json is missing or too large"))?;
    let m = manifest::parse(&bytes).map_err(|e| bad(C, e.to_string()))?;
    let dir = models_dir(s, C)?;
    Ok(json!({"kind": "model", "domain": "denoise", "path": path, "fileName": src.file_name().map(|n| n.to_string_lossy()), "model": m,
        "sizeBytes": std::fs::metadata(src).ok().map(|m| m.len()), "alreadyInstalled": dir.join(&m.id).join("model.onnx").is_file()}))
}

/// What an error says, without the command it came from (for a line shown to the user).
fn plain(e: &EngineError) -> String {
    match e {
        EngineError::BadParams { msg, .. } => msg.clone(),
        other => other.to_string(),
    }
}

/// Install every download that has arrived: each was accepted when it was started, so it is installed and chosen without
/// another question. Called by `denoise.models.downloads` and by every frame's `denoise.pump`.
pub(crate) fn finish_downloads(s: &mut Session) {
    if let Some(job) = &s.denoise.install {
        let outcome = match job.result.try_recv() {
            Ok(v) => Some(v),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("the installation worker stopped".into())),
        };
        if let Some(outcome) = outcome
            && let Some(job) = s.denoise.install.take()
        {
            let state = match outcome {
                Ok((v, model)) => match activate_install(s, &v, model, job.activate) {
                    Ok(_) => State::Installed,
                    Err(e) => State::Failed(plain(&e)),
                },
                Err(e) => State::Failed(e),
            };
            s.denoise.downloads.set_outcome(&job.id, state);
        }
    }
    if s.denoise.install.is_none()
        && let Some((id, path, _sha)) = s.denoise.downloads.finished().into_iter().next()
        && let Err(e) = start_install(s, path, id.clone(), true, true)
    {
        s.denoise.downloads.set_outcome(&id, State::Failed(plain(&e)));
    }
}

fn download(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "denoise.models.download";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let dir = models_dir(s, C)?;
    let k = known::find(id).ok_or_else(|| bad(C, "LightCraft has no download for that model: get the file from its page, then add it"))?;
    let spec = k.download.clone().ok_or_else(|| bad(C, "LightCraft has no download for that model: get the file from its page, then add it"))?;
    if !cfg!(feature = "denoise") {
        return Err(bad(C, "this build cannot run denoise models, so there is nothing to download for it"));
    }
    if !bool_or(p, "acknowledged", false) {
        return Err(bad(
            C,
            "the licence has not been accepted: show the user the model's terms (see `denoise.models.list`) and pass `acknowledged: true` once they agree",
        ));
    }
    if installed_models(&dir).iter().any(|i| i.manifest.id == spec.id) {
        return Err(bad(C, "that model is already installed"));
    }
    let host = lightcraft_denoise::known::host(&spec.url).to_string();
    s.denoise.downloads.start(spec, &dir, &s.activity, &k.manifest.name).map_err(|e| bad(C, e))?;
    Ok(json!({"started": id, "from": host}))
}

fn download_row(id: &str, state: &State) -> Value {
    let from = known::find(id).and_then(|k| host_of(&k));
    match state {
        State::Running { bytes, total } => json!({"id": id, "state": "running", "bytes": bytes, "total": total, "from": from}),
        State::Done { path, .. } => json!({"id": id, "state": "done", "path": path.display().to_string(), "from": from}),
        State::Installing => json!({"id": id, "state": "installing", "from": from}),
        State::Installed => json!({"id": id, "state": "installed", "from": from}),
        State::Failed(why) => json!({"id": id, "state": "failed", "error": why, "from": from}),
        State::Cancelled => json!({"id": id, "state": "cancelled", "from": from}),
    }
}

fn downloads(s: &mut Session, _: &Value) -> Result<Value> {
    finish_downloads(s);
    let all: Vec<Value> = s.denoise.downloads.snapshot().iter().map(|(id, st)| download_row(id, st)).collect();
    Ok(json!({"running": s.denoise.downloads.running(), "downloads": all}))
}

fn download_cancel(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "denoise.models.downloadCancel";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    Ok(json!({"discarded": s.denoise.downloads.discard(id)}))
}

fn test(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "denoise.models.test";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let dir = models_dir(s, C)?;
    let found = installed_models(&dir).into_iter().find(|i| i.manifest.id == id).ok_or_else(|| bad(C, "that model is not installed"))?;
    let run_on = s.denoise.settings.run_on();
    let result = (s.denoise.loader)(&found.onnx, &found.manifest).and_then(|m| m.self_test(run_on));
    let (ok, detail) = match &result {
        Ok(v) => (true, v.clone()),
        Err(e) => (false, json!(e)),
    };
    // keep the latest outcome with the model's record
    let mut record = found.accepted;
    if let Some(o) = record.as_object_mut() {
        o.insert("selfTest".into(), if ok { detail.clone() } else { json!({"ok": false, "error": detail}) });
        write_atomic(&found.onnx.with_file_name("installed.json"), &serde_json::to_vec_pretty(&record).map_err(|e| fail("record", e))?)
            .map_err(EngineError::Other)?;
    }
    Ok(json!({"id": id, "ok": ok, "result": detail}))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "denoise.models.remove";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let dir = models_dir(s, C)?;
    if !lightcraft_denoise::licence::valid_id(id) {
        return Err(bad(C, "not a model id"));
    }
    let home = dir.join(id);
    // only a folder that is a model (it has a manifest) is ever deleted
    if !home.join("denoise-model.json").is_file() {
        return Err(bad(C, "that model is not installed"));
    }
    std::fs::remove_dir_all(&home).map_err(|e| fail("could not remove the model", e))?;
    let mut st = read_settings(&dir);
    if st.model.as_deref() == Some(id) {
        st.model = installed_models(&dir).into_iter().map(|i| i.manifest.id).find(|m| m != id);
        write_settings(&dir, &st).map_err(EngineError::Other)?;
    }
    s.denoise.touch();
    s.denoise_refresh_active(true);
    Ok(json!({"removed": id, "model": st.model}))
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "denoise.models.select";
    let dir = models_dir(s, C)?;
    let id = match p.get("id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().ok_or_else(|| bad(C, "`id` must be a model id or null"))?.to_string()),
    };
    if let Some(id) = &id
        && !installed_models(&dir).iter().any(|i| &i.manifest.id == id)
    {
        return Err(bad(C, "that denoise model is not installed"));
    }
    let mut st = read_settings(&dir);
    st.model = id;
    write_settings(&dir, &st).map_err(EngineError::Other)?;
    s.denoise.touch();
    s.denoise_refresh_active(true);
    Ok(json!({"model": st.model}))
}

fn settings(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "denoise.settings";
    let dir = models_dir(s, C)?;
    let mut st: Settings = read_settings(&dir);
    if let Some(v) = p.get("auto") {
        st.auto = Some(v.as_bool().ok_or_else(|| bad(C, "`auto` must be true or false"))?);
    }
    if let Some(v) = p.get("cacheGb") {
        let n =
            v.as_u64().filter(|n| (1..=10_000).contains(n)).ok_or_else(|| bad(C, "`cacheGb` must be a whole number of gigabytes from 1 to 10000"))?;
        st.cache_gb = Some(n as u32);
    }
    if let Some(v) = p.get("threads") {
        st.threads = match v {
            Value::Null => None,
            v => Some(v.as_u64().filter(|n| (1..=64).contains(n)).ok_or_else(|| bad(C, "`threads` must be from 1 to 64, or null"))? as u32),
        };
    }
    let run_on = match p.get("runOn") {
        None => None,
        Some(v) => Some(v.as_str().and_then(RunOn::parse).ok_or_else(|| bad(C, "`runOn` must be auto, gpu or cpu"))?),
    };
    if let Some(r) = run_on {
        st.run_on = Some(r.name().to_string());
        st.gpu = None;
    }
    write_settings(&dir, &st).map_err(EngineError::Other)?;
    if run_on.is_some() {
        // a choice of where denoise runs is also a go-ahead to try the card again: a set-up that crashed or gave up
        // before is forgotten, and the loaded model sets the card up afresh for the next photo
        forget_gpu_set_ups(&dir);
        if let Some(a) = &s.denoise.active {
            *a.model.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }
    s.denoise.touch();
    s.denoise_refresh_active(true);
    // a smaller limit takes effect now
    s.denoise_evict(None);
    Ok(json!({"auto": st.auto(), "cacheGb": st.cache_bytes() >> 30, "threads": st.threads, "runOn": st.run_on().name()}))
}

fn ids_param(p: &Value) -> Option<Vec<PhotoId>> {
    p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(PhotoId).collect())
}

fn status_json(s: &Session, photo: Option<PhotoId>) -> Value {
    let (files, bytes) = s.denoise_cache_usage();
    let st = &s.denoise.settings;
    let model = s.denoise.active.as_ref().map(|a| json!({"id": a.id, "name": a.manifest.name}));
    let mut v = json!({
        "enabled": s.denoise.active.is_some(),
        "model": model,
        "runtime": cfg!(feature = "denoise"),
        "productsDir": s.denoise_products_dir().map(|d| d.display().to_string()),
        "queued": s.denoise.queued(),
        "running": s.denoise_running_json(),
        "ready": s.media.denoise.len(),
        "made": s.denoise_made(),
        "auto": st.auto(),
        "runOn": st.run_on().name(),
        "device": s.denoise_device(),
        "cache": {"files": files, "bytes": bytes, "limitBytes": st.cache_bytes()},
        "generation": s.denoise.generation,
    });
    if let (Some(id), Some(o)) = (photo, v.as_object_mut()) {
        o.insert("photo".into(), serde_json::to_value(s.denoise_photo_state(id)).unwrap_or(Value::Null));
    }
    v
}

/// A per-photo switch, independent of the selection and Auto Sync. Turning on starts at 50%.
fn toggle(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "denoise.toggle";
    let id = match p.get("id") {
        Some(v) => PhotoId(v.as_u64().ok_or_else(|| bad(C, "`id` must be a photo id"))?),
        None => s.active().ok_or_else(|| bad(C, "select a raw photo"))?,
    };
    let photo = s.catalog.photo(id).ok_or_else(|| bad(C, "no such photo"))?;
    if !crate::denoise::eligible(photo) {
        return Err(bad(C, "AI Denoise applies to raw photos developed from their sensor data"));
    }
    let mut d = (*s.develop_of(id).ok_or_else(|| bad(C, "no such photo"))?).clone();
    let on = match p.get("enabled") {
        Some(v) => v.as_bool().ok_or_else(|| bad(C, "`enabled` must be true or false"))?,
        None => !d.enhance.denoise_enabled(),
    };
    if on {
        if !d.enhance.denoise.is_finite() {
            return Err(bad(C, "the photo has an invalid Denoise amount"));
        }
        s.denoise_refresh_active(true);
        if s.denoise.active.is_none() {
            return Err(bad(C, "AI Denoise needs a model: install one in Settings > AI Denoise"));
        }
    }
    // The switch is its own setting: the Amount stays where it is (0 included) when it is flipped, so flipping compares
    // with and without. A photo that never had an Amount starts at 50 the first time.
    let amount = if on && d.enhance.denoise_on.is_none() && d.enhance.denoise <= 0.0 { 50.0 } else { d.enhance.denoise };
    if d.enhance.denoise != amount || d.enhance.denoise_on != Some(on) {
        s.end_interaction()?;
        d.enhance.denoise = amount;
        d.enhance.denoise_on = Some(on);
        let op = s.develop_op(id, d, "AI Denoise").ok_or_else(|| bad(C, "no such photo"))?;
        s.commit("AI Denoise", op)?;
    }
    if on {
        s.denoise_enqueue(&[id], true, false);
    } else {
        s.denoise_cancel(Some(id), false);
    }
    Ok(json!({"id": id.0, "enabled": on, "amount": amount}))
}

fn queue(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "denoise.queue";
    s.denoise_refresh_active(true);
    if s.denoise.active.is_none() {
        return Err(bad(C, "no denoise model is chosen: install one first (`denoise.models.download` or `denoise.models.install`)"));
    }
    if s.denoise_products_dir().is_none() {
        return Err(bad(C, "denoise keeps its pictures in the library's folder: open a library on disk first"));
    }
    let ids = match ids_param(p) {
        Some(ids) => ids,
        None => match str_param(p, "scope").unwrap_or("selected") {
            "selected" => s.selection.active.iter().chain(s.selection.ids.iter()).copied().collect(),
            "visible" => s.visible_cloned().to_vec(),
            "withAmount" => s.catalog.photos().filter(|ph| ph.develop.denoise_amount() > 0.0).map(|ph| ph.id).collect(),
            other => return Err(bad(C, format!("unknown scope `{other}`: use selected, visible or withAmount"))),
        },
    };
    let queued = s.denoise_enqueue(&ids, false, bool_or(p, "retry", false));
    let mut v = status_json(s, None);
    if let Some(o) = v.as_object_mut() {
        o.insert("added".into(), json!(queued));
    }
    Ok(v)
}

fn cancel(s: &mut Session, p: &Value) -> Result<Value> {
    let photo = p.get("id").and_then(Value::as_u64).map(PhotoId);
    let n = s.denoise_cancel(photo, bool_or(p, "all", photo.is_none()));
    Ok(json!({"cancelled": n}))
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    s.denoise_refresh_active(false);
    Ok(status_json(s, p.get("id").and_then(Value::as_u64).map(PhotoId)))
}

/// `denoise.pump {pace?}`: cheap to call every frame. Takes in what the background job has finished and keeps one going.
fn pump(s: &mut Session, p: &Value) -> Result<Value> {
    s.denoise_pump(Pace::parse(p.get("pace").and_then(Value::as_str)));
    s.denoise_track();
    Ok(json!({
        "active": s.denoise.active.is_some(),
        "running": s.denoise_running_json(),
        "queued": s.denoise.queued(),
        "ready": s.media.denoise.len(),
        "generation": s.denoise.generation,
        "downloading": s.denoise.downloads.running(),
    }))
}

fn clear(s: &mut Session, _: &Value) -> Result<Value> {
    let (files, bytes) = s.denoise_clear();
    Ok(json!({"deleted": files, "bytes": bytes}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "denoise.models.list", "Denoise Models", [], None, "{} → {dir, productsDir, model, runtime, auto, cacheGb, threads, runOn, models: [{id, name, version, licence{name, commercial, url, notice}, provenance, source, sizeBytes, sha256, tile, known, downloadHost, installed, selected, accepted}]}", always, list),
        cmd!(query "denoise.models.inspect", "Inspect Denoise Model", [], None, "{path} → model terms from an adjacent manifest; never executes the file", always, inspect),
        cmd!(query "denoise.models.install", "Install Denoise Model", [], None, "{path, acknowledged: true, activate?: true, background?: false} → {installed, model} or {started} — install a denoise model from a file: an .onnx with a denoise-model.json beside it, or an archive LightCraft knows (the darktable `.dtmodel`). `acknowledged` must be true: the user has been shown the model's licence and accepted it. Unless `activate` is false it becomes the model in use", always, install),
        cmd!(query "denoise.models.download", "Download Denoise Model", [], None, "{id, acknowledged: true} → {started, from} — fetch a model LightCraft has a pinned address for (see `downloadHost` in the list) in the background over HTTPS with lightcraft-fetch. `acknowledged` must be true: the user has been shown the model's terms and accepted them. It is checked against its size and SHA-256, installed and chosen by itself; `denoise.models.downloads` shows how far it is", always, download),
        cmd!(query "denoise.models.downloads", "Denoise Model Downloads", [], None, "{} → {running, downloads: [{id, state: running | done | installed | failed | cancelled, bytes, total, error, from}]} — also installs any download that has arrived", always, downloads),
        cmd!(query "denoise.models.downloadCancel", "Cancel Denoise Model Download", [], None, "{id} → {discarded} — stop a download, or delete a finished one that was not installed", always, download_cancel),
        cmd!(query "denoise.models.test", "Test Denoise Model", [], None, "{id} → {ok, result} — load an installed model and check it gives sensible pictures; needs the denoise runtime", always, test),
        cmd!(query "denoise.models.remove", "Remove Denoise Model", [], None, "{id} → {removed, model} — delete an installed model (its cached pictures stay until they are evicted)", always, remove),
        cmd!(query "denoise.models.select", "Choose Denoise Model", [], None, "{id: installed model | null} → {model} — null switches denoise off", always, select),
        cmd!(query "denoise.settings", "Denoise Settings", [], None, "{auto?: bool, cacheGb?: 1..10000, threads?: 1..64 | null, runOn?: auto | gpu | cpu} → the settings — `auto`: make the picture of the photos being looked at that have a Denoise amount; `cacheGb`: how much the cached pictures may take; `threads`: most tiles run at once; `runOn`: where the model runs — `auto` (the default) the graphics card where it is faster than the processor (both timed on this computer when the card is set up), `gpu` the card whenever it can run the model, `cpu` the processor. Setting `runOn` also lets the card be tried again after a set-up that failed or closed LightCraft", always, settings),
        cmd!(
            "denoise.toggle",
            "AI Denoise",
            [],
            None,
            "{id?: photoId, enabled?: bool} → {id, enabled, amount} — turn AI Denoise on (50% initially) or off for one raw photo, independent of selection and Auto Sync; needs an installed model to turn on",
            always,
            toggle
        ),
        cmd!(query "denoise.queue", "Denoise Photos", [], None, "{ids?: [photoId], scope?: selected | visible | withAmount, retry?: false} → {added, …status} — make the denoised pictures of raw photos that have none, in the background. `retry` tries photos that failed before", always, queue),
        cmd!(query "denoise.cancel", "Cancel Denoise", [], None, "{id?, all?} → {cancelled} — stop the photo being made and/or forget the ones waiting (all of them when `id` is omitted)", always, cancel),
        cmd!(query "denoise.status", "Denoise Status", [], None, "{id?} → {enabled, model, runtime, productsDir, queued, running: {photo, done, total, seconds} | null, ready, made, auto, runOn, device: {kind: gpu | cpu | pending | none, adapter?, reason?, cardMs?, cpuMs?, fellBack?}, cache: {files, bytes, limitBytes}, generation, photo?: {state: notApplicable | noModel | ready | queued | running | failed | idle, …}}", always, status),
        cmd!(query "denoise.pump", "Denoise Pump", [], None, "{pace?: pause | light | normal | full} → {active, running, queued, ready, generation, downloading} — called every frame by the app: takes in finished work and keeps one photo going", always, pump),
        cmd!(query "denoise.clear", "Clear Denoise Cache", [], None, "{} → {deleted, bytes} — delete every cached denoised picture of this library (they are made again when wanted)", always, clear),
    ]
}
