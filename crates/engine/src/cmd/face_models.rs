//! Face model commands (`faces.*`): which models exist, inspecting a file the user dropped in, installing
//! it once its licence is accepted, removing it, and the faces on/off setting.
//!
//! Models live in the folder the host provides (`Session::face_models_dir`): one subfolder per model with
//! `model.onnx`, `face-model.json` (its manifest) and `installed.json` (what the user accepted), plus
//! `settings.json` beside them. Model files and manifests are hostile input: sizes are capped, ids are
//! validated before they become folder names, and nothing outside a model's own folder is ever touched.

use std::io::Read;
use std::path::{Path, PathBuf};

use lightcraft_faces::hash::sha256_file;
use lightcraft_faces::manifest::{self, MAX_MANIFEST_BYTES, MAX_MODEL_BYTES, ModelManifest, Role};
use lightcraft_faces::suggest::{Suggestion, suggest};
use lightcraft_faces::{known, onnx};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::face_download::{self, State};
use crate::{EngineError, Result, Session};

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct FaceSettings {
    pub(super) enabled: bool,
    /// The installed recogniser in use.
    pub(super) embedder: Option<String>,
}

fn fail(what: &str, e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("{what}: {e}"))
}

fn models_dir(s: &Session, cmd: &str) -> Result<PathBuf> {
    s.face_models_dir.clone().ok_or_else(|| bad(cmd, "this build has nowhere to keep face models (the desktop app does)"))
}

/// Read at most `max` bytes of a small file.
fn read_capped(path: &Path, max: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(max as u64 + 1).read_to_end(&mut buf).ok()?;
    (buf.len() <= max).then_some(buf)
}

pub(super) fn read_settings(dir: &Path) -> FaceSettings {
    read_capped(&dir.join("settings.json"), 4096).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let part = path.with_extension("part");
    std::fs::write(&part, bytes).map_err(|e| fail(&format!("could not write {}", path.display()), e))?;
    std::fs::rename(&part, path).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        fail(&format!("could not write {}", path.display()), e)
    })
}

fn write_settings(dir: &Path, st: &FaceSettings) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| fail("could not create the face models folder", e))?;
    write_atomic(&dir.join("settings.json"), &serde_json::to_vec_pretty(st).map_err(|e| fail("settings", e))?)
}

pub(super) struct Installed {
    pub(super) manifest: ModelManifest,
    pub(super) accepted: Value,
}

/// The models in the folder. A folder that is not a valid model (wrong name, no model file, a manifest that
/// does not validate or does not match its folder) is ignored, never trusted.
pub(super) fn installed_models(dir: &Path) -> Vec<Installed> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        if !manifest::valid_id(&name) || !p.is_dir() || !p.join("model.onnx").is_file() {
            continue;
        }
        let Some(m) = read_capped(&p.join("face-model.json"), MAX_MANIFEST_BYTES).and_then(|b| manifest::parse(&b).ok()) else { continue };
        if m.id != name {
            continue;
        }
        let accepted = read_capped(&p.join("installed.json"), 4096).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
        out.push(Installed { manifest: m, accepted });
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    out
}

fn row(m: &ModelManifest, installed: bool, selected: bool, accepted: &Value) -> Value {
    let bundled = known::BUNDLED.contains(&m.id.as_str());
    json!({
        "id": m.id,
        "name": m.name,
        "role": m.role,
        "licence": m.licence,
        "provenance": m.provenance,
        "source": m.source,
        "sizeBytes": m.size_bytes,
        "sha256": m.sha256,
        "known": known::all().iter().any(|k| k.id == m.id),
        "bundled": bundled,
        // a pinned address LightCraft can fetch it from (the user presses Download), and which site that is
        "downloadHost": known::download(&m.id).map(|d| known::host(&d.url).to_string()),
        "installed": installed || bundled,
        "selected": selected,
        "accepted": accepted,
    })
}

struct Inspection {
    sha256: String,
    size: u64,
    file_name: String,
    suggestion: Suggestion,
}

/// Look at a model file. `known_sha` is its SHA-256 when something already checked it (a finished download), so a
/// file of hundreds of megabytes is not read again just to be hashed.
fn inspect_file(path: &str, cmd: &str, known_sha: Option<&str>) -> Result<Inspection> {
    let p = Path::new(path);
    let meta = std::fs::metadata(p).map_err(|e| bad(cmd, format!("cannot read `{path}`: {e}")))?;
    if !meta.is_file() {
        return Err(bad(cmd, format!("`{path}` is not a file")));
    }
    if meta.len() == 0 || meta.len() > MAX_MODEL_BYTES {
        return Err(bad(cmd, "a model file must be between 1 byte and 4 GiB"));
    }
    let sha256 = match known_sha {
        Some(h) => h.to_string(),
        None => sha256_file(p).map_err(|e| fail(&format!("could not read {path}"), e))?,
    };
    let file_name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let suggestion = match onnx::probe_path(p) {
        Ok(info) => suggest(&info, &sha256, meta.len(), &file_name),
        Err(e) => Suggestion::Unsupported(e.to_string()),
    };
    Ok(Inspection { sha256, size: meta.len(), file_name, suggestion })
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let dir = s.face_models_dir.clone();
    let (settings, installed) = match &dir {
        Some(d) => (read_settings(d), installed_models(d)),
        None => (FaceSettings::default(), Vec::new()),
    };
    let on_disk = |id: &str| installed.iter().find(|i| i.manifest.id == id);
    let mut models: Vec<Value> = Vec::new();
    for k in known::all() {
        let found = on_disk(&k.id);
        models.push(row(&k, found.is_some(), settings.embedder.as_deref() == Some(k.id.as_str()), found.map_or(&Value::Null, |f| &f.accepted)));
    }
    for i in &installed {
        if !known::all().iter().any(|k| k.id == i.manifest.id) {
            models.push(row(&i.manifest, true, settings.embedder.as_deref() == Some(i.manifest.id.as_str()), &i.accepted));
        }
    }
    Ok(json!({
        "dir": dir.as_ref().map(|d| d.display().to_string()),
        "enabled": settings.enabled,
        "embedder": settings.embedder,
        // whether this build can run recognition models (the `recognition` feature: tract)
        "runtime": cfg!(feature = "recognition"),
        "models": models,
    }))
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.models.inspect";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path`"))?;
    let ins = inspect_file(path, C, None)?;
    let installed = s.face_models_dir.as_deref().map(installed_models).unwrap_or_default();
    let (kind, model, assumptions, reason) = match &ins.suggestion {
        Suggestion::Known(m) => ("known", Some(m), Vec::new(), None),
        Suggestion::Draft { manifest, assumptions } => ("draft", Some(manifest), assumptions.clone(), None),
        Suggestion::Unsupported(why) => ("unsupported", None, Vec::new(), Some(why.clone())),
    };
    let already = model.is_some_and(|m| installed.iter().any(|i| i.manifest.sha256 == m.sha256 && i.manifest.id == m.id));
    Ok(json!({
        "path": path,
        "fileName": ins.file_name,
        "sizeBytes": ins.size,
        "sha256": ins.sha256,
        "kind": kind,
        "model": model.map(|m| row(m, already, false, &Value::Null)),
        "assumptions": assumptions,
        "reason": reason,
        "alreadyInstalled": already,
    }))
}

fn install(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.models.install";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "missing `path`"))?;
    let acknowledged = p.get("acknowledged").and_then(Value::as_bool).unwrap_or(false);
    let activate = p.get("activate").and_then(Value::as_bool).unwrap_or(true);
    let ins = inspect_file(path, C, None)?;
    install_file(s, C, Path::new(path), ins, acknowledged, activate, false)
}

/// Install the inspected model file `src`. A recognition model that passes its self-test becomes the one in use and
/// recognition is switched on (`activate`, the default): installing a model is how someone says they want it.
/// `verified` is for a download that was already checked against its recorded hash: it is moved into place instead
/// of being copied and read again.
fn install_file(s: &mut Session, c: &str, src: &Path, ins: Inspection, acknowledged: bool, activate: bool, verified: bool) -> Result<Value> {
    let dir = models_dir(s, c)?;
    let m = match ins.suggestion {
        Suggestion::Known(m) | Suggestion::Draft { manifest: m, .. } => m,
        Suggestion::Unsupported(why) => return Err(bad(c, format!("this model cannot be used yet: {why}"))),
    };
    if known::BUNDLED.contains(&m.id.as_str()) {
        return Err(bad(c, "this model is already included with LightCraft"));
    }
    if m.role != Role::Embedder {
        return Err(bad(c, "only face recognition models can be installed for now"));
    }
    manifest::validate(&m).map_err(|e| bad(c, e.to_string()))?;
    if !acknowledged {
        return Err(bad(c, "the licence has not been accepted: show the user its terms and pass `acknowledged: true` once they agree"));
    }
    let home = dir.join(&m.id);
    std::fs::create_dir_all(&home).map_err(|e| fail("could not create the model's folder", e))?;
    let (part, final_path) = (home.join("model.onnx.part"), home.join("model.onnx"));
    let moved = verified && std::fs::rename(src, &part).is_ok();
    let placed = if moved {
        Ok(())
    } else {
        std::fs::copy(src, &part).map_err(|e| fail("could not copy the model (is the disk full?)", e)).and_then(|_| match sha256_file(&part) {
            Ok(h) if h == ins.sha256 => Ok(()),
            Ok(_) => Err(EngineError::Other("the copy does not match the original (the file changed while copying?)".into())),
            Err(e) => Err(fail("could not check the copy", e)),
        })
    };
    if let Err(e) = placed.and_then(|_| std::fs::rename(&part, &final_path).map_err(|e| fail("could not finish the copy", e))) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    // a model that does not work is never left installed
    let test = match self_test(&final_path, &m) {
        Ok(t) => t,
        Err(e) => {
            let _ = std::fs::remove_file(&final_path);
            let _ = std::fs::remove_file(home.join("face-model.json"));
            let _ = std::fs::remove_dir(&home);
            return Err(bad(c, e));
        }
    };
    write_atomic(&home.join("face-model.json"), &serde_json::to_vec_pretty(&m).map_err(|e| fail("manifest", e))?)?;
    let accepted = json!({"acceptedAt": (s.clock)(), "licence": m.licence.name, "commercial": m.licence.commercial, "fileName": ins.file_name, "selfTest": test});
    write_atomic(&home.join("installed.json"), &serde_json::to_vec_pretty(&accepted).map_err(|e| fail("record", e))?)?;
    // a file that was downloaded and waited for the user's acceptance has served: the installed copy is the model now
    if src.parent() == Some(dir.join(face_download::STAGING).as_path()) {
        let _ = std::fs::remove_file(src);
        s.face_downloads.staged_file_used(src);
    }
    // the newest model is the one in use (the earlier one stays installed, and `faces.models.select` switches back)
    if activate {
        let mut st = read_settings(&dir);
        st.embedder = Some(m.id.clone());
        st.enabled = true;
        write_settings(&dir, &st)?;
    }
    let now = read_settings(&dir);
    let selected = now.embedder.as_deref() == Some(m.id.as_str());
    Ok(json!({"installed": row(&m, true, selected, &accepted), "active": {"embedder": now.embedder, "enabled": now.enabled}}))
}

/// What an error says, without the command it came from (for a line shown to the user).
fn plain(e: &EngineError) -> String {
    match e {
        EngineError::BadParams { msg, .. } => msg.clone(),
        other => other.to_string(),
    }
}

/// Install every download that has arrived: each was accepted when it was started, so it is installed, chosen and
/// switched on without another question. Called by `faces.models.downloads` and by every frame's `faces.pump`.
pub(super) fn finish_downloads(s: &mut Session) {
    const C: &str = "faces.models.download";
    for (id, path, sha) in s.face_downloads.finished() {
        let outcome = inspect_file(&path.to_string_lossy(), C, Some(&sha)).and_then(|ins| install_file(s, C, &path, ins, true, true, true));
        match outcome {
            Ok(_) => s.face_downloads.set_outcome(&id, State::Installed),
            Err(e) => {
                // a file that cannot be installed is of no use to anyone: throw it away, say why
                let _ = std::fs::remove_file(&path);
                s.face_downloads.set_outcome(&id, State::Failed(plain(&e)));
            }
        }
    }
}

/// `faces.models.download {id, acknowledged: true}`: fetch a model LightCraft knows an address for, in the background;
/// when it has arrived and checked out it is installed, chosen and switched on.
fn download(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.models.download";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let dir = models_dir(s, C)?;
    let spec = known::download(id).ok_or_else(|| bad(C, "LightCraft has no download for that model: get the file from its page, then add it"))?;
    if !cfg!(feature = "recognition") {
        return Err(bad(C, "this build cannot run face recognition models, so there is nothing to download for it"));
    }
    if p.get("acknowledged").and_then(Value::as_bool) != Some(true) {
        return Err(bad(
            C,
            "the licence has not been accepted: show the user the model's terms (see `faces.models.list`) and pass `acknowledged: true` once they agree",
        ));
    }
    if installed_models(&dir).iter().any(|i| i.manifest.id == spec.id) {
        return Err(bad(C, "that model is already installed"));
    }
    let host = known::host(&spec.url).to_string();
    s.face_downloads.start(spec, &dir).map_err(|e| bad(C, e))?;
    Ok(json!({"started": id, "from": host}))
}

fn download_row(id: &str, state: &State) -> Value {
    let from = known::download(id).map(|d| known::host(&d.url).to_string());
    match state {
        State::Running { bytes, total } => json!({"id": id, "state": "running", "bytes": bytes, "total": total, "from": from}),
        State::Done { path, .. } => json!({"id": id, "state": "done", "path": path.display().to_string(), "from": from}),
        State::Installed => json!({"id": id, "state": "installed", "from": from}),
        State::Failed(why) => json!({"id": id, "state": "failed", "error": why, "from": from}),
        State::Cancelled => json!({"id": id, "state": "cancelled", "from": from}),
    }
}

/// `faces.models.downloads`: where each download stands (a finished one is installed first).
fn downloads(s: &mut Session, _: &Value) -> Result<Value> {
    finish_downloads(s);
    let all: Vec<Value> = s.face_downloads.snapshot().iter().map(|(id, st)| download_row(id, st)).collect();
    Ok(json!({"running": s.face_downloads.running(), "downloads": all}))
}

/// `faces.models.downloadCancel {id}`: stop a download, or throw away a finished one that was not installed.
fn download_cancel(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.models.downloadCancel";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    Ok(json!({"discarded": s.face_downloads.discard(id)}))
}

/// Load the model and run its self-test: `Ok(null)` in a build without the recognition runtime (nothing can
/// be checked), `Ok(result)` when it passes, `Err(reason)` when it does not load or fails a check.
#[cfg(feature = "recognition")]
fn self_test(path: &Path, m: &ModelManifest) -> std::result::Result<Value, String> {
    let embedder = lightcraft_faces::runtime::Embedder::load(path, m).map_err(|e| e.to_string())?;
    let t = embedder.self_test();
    if !t.ok {
        let failed: Vec<&str> = t.checks.iter().filter(|(_, ok)| !*ok).map(|(c, _)| c.as_str()).collect();
        return Err(format!("the model failed its self-test: it does not {}", failed.join(", and does not ")));
    }
    serde_json::to_value(&t).map_err(|e| e.to_string())
}

#[cfg(not(feature = "recognition"))]
fn self_test(_: &Path, _: &ModelManifest) -> std::result::Result<Value, String> {
    Ok(Value::Null)
}

/// `faces.models.test {id}`: run an installed model's self-test again.
fn test(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.models.test";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let dir = models_dir(s, C)?;
    if !cfg!(feature = "recognition") {
        return Err(bad(C, "this build cannot run face recognition models"));
    }
    let found = installed_models(&dir).into_iter().find(|i| i.manifest.id == id).ok_or_else(|| bad(C, "that model is not installed"))?;
    let result = self_test(&dir.join(id).join("model.onnx"), &found.manifest);
    let (ok, detail) = match &result {
        Ok(v) => (true, v.clone()),
        Err(e) => (false, json!(e)),
    };
    // keep the latest outcome with the model's record
    let mut record = found.accepted;
    if let Some(o) = record.as_object_mut() {
        o.insert("selfTest".into(), if ok { detail.clone() } else { json!({"ok": false, "error": detail}) });
        write_atomic(&dir.join(id).join("installed.json"), &serde_json::to_vec_pretty(&record).map_err(|e| fail("record", e))?)?;
    }
    Ok(json!({"id": id, "ok": ok, "result": detail}))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.models.remove";
    let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
    let dir = models_dir(s, C)?;
    if !manifest::valid_id(id) {
        return Err(bad(C, "not a model id"));
    }
    if known::BUNDLED.contains(&id) {
        return Err(bad(C, "this model is part of LightCraft and cannot be removed"));
    }
    let home = dir.join(id);
    // only a folder that is a model (it has a manifest) is ever deleted
    if !home.join("face-model.json").is_file() {
        return Err(bad(C, "that model is not installed"));
    }
    std::fs::remove_dir_all(&home).map_err(|e| fail("could not remove the model", e))?;
    let mut st = read_settings(&dir);
    if st.embedder.as_deref() == Some(id) {
        // another installed recogniser takes over, so recognition keeps working; with none left nothing is chosen
        st.embedder = installed_models(&dir).into_iter().find(|i| i.manifest.id != id && i.manifest.role == Role::Embedder).map(|i| i.manifest.id);
        write_settings(&dir, &st)?;
    }
    Ok(json!({"removed": id, "embedder": st.embedder}))
}

fn select(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.models.select";
    let dir = models_dir(s, C)?;
    let id = match p.get("id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().ok_or_else(|| bad(C, "`id` must be a model id or null"))?.to_string()),
    };
    if let Some(id) = &id
        && !installed_models(&dir).iter().any(|i| &i.manifest.id == id && i.manifest.role == Role::Embedder)
    {
        return Err(bad(C, "that recognition model is not installed"));
    }
    let mut st = read_settings(&dir);
    st.embedder = id;
    write_settings(&dir, &st)?;
    Ok(json!({"embedder": st.embedder}))
}

fn enable(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "faces.enable";
    let dir = models_dir(s, C)?;
    let mut st = read_settings(&dir);
    st.enabled = p.get("enabled").and_then(Value::as_bool).unwrap_or(!st.enabled);
    write_settings(&dir, &st)?;
    // the background scan notices at once, not within the second it trusts its last look
    #[cfg(feature = "recognition")]
    {
        s.faces.enabled_seen = None;
    }
    Ok(json!({"enabled": st.enabled}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "faces.models.list", "Face Models", [], None, "{} → {dir, enabled, embedder, runtime, models: [{id, name, role, licence{name, commercial, url, notice}, provenance, source, sizeBytes, known, bundled, installed, selected}]}", always, list),
        cmd!(query "faces.models.inspect", "Inspect Face Model File", [], None, "{path} → what a .onnx file is: {kind: known | draft | unsupported, model, assumptions, reason, alreadyInstalled}; installs nothing", always, inspect),
        cmd!(query "faces.models.install", "Install Face Model", [], None, "{path, acknowledged: true, activate?: true} → {installed, active: {embedder, enabled}} — copy a .onnx face recognition model into the models folder. `acknowledged` must be true: the user has been shown its licence (see inspect) and accepted it. Unless `activate` is false the model becomes the one in use and recognition is switched on; an earlier model stays installed", always, install),
        cmd!(query "faces.models.download", "Download Face Model", [], None, "{id, acknowledged: true} → {started, from} — fetch a recognition model LightCraft has a pinned address for (see `downloadHost` in the list), in the background with the system's curl. `acknowledged` must be true: the user has been shown the model's terms and accepted them. It is checked against its size and SHA-256, then installed, chosen and switched on by itself; `faces.models.downloads` shows how far it is", always, download),
        cmd!(query "faces.models.downloads", "Face Model Downloads", [], None, "{} → {running, downloads: [{id, state: running | done | installed | failed | cancelled, bytes, total, error, from}]} — also installs any download that has arrived; `installed` stays listed until `faces.models.downloadCancel` clears it", always, downloads),
        cmd!(query "faces.models.downloadCancel", "Cancel Face Model Download", [], None, "{id} → {discarded} — stop a download, or delete a finished one that was not installed", always, download_cancel),
        cmd!(query "faces.models.test", "Test Face Model", [], None, "{id} → {ok, result: {loadMs, embedMs, dimension, checks}} — load an installed recognition model and check it gives sensible faces; needs the recognition runtime", always, test),
        cmd!(query "faces.models.remove", "Remove Face Model", [], None, "{id} — delete an installed model", always, remove),
        cmd!(query "faces.models.select", "Choose Face Recognition Model", [], None, "{id: installed recogniser | null}", always, select),
        cmd!(query "faces.enable", "Face Recognition On/Off", [], None, "{enabled?: bool} (toggles when omitted)", always, enable),
    ]
}
