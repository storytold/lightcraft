//! AI Defringe commands (docs/ai-defringe.md): the per-photo switch, the installed model, and the corrections.

use std::path::Path;

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::aidefringe;
use crate::{Result, Session};

fn photo(s: &Session, p: &Value, c: &str) -> Result<PhotoId> {
    match p.get("id") {
        Some(v) => Ok(PhotoId(v.as_u64().ok_or_else(|| bad(c, "`id` must be a photo id"))?)),
        None => s.active().ok_or_else(|| bad(c, "select a photo")),
    }
}

fn model_json(m: &aidefringe::Manifest) -> Value {
    json!({"id": m.id, "name": m.name, "version": m.version, "licence": m.licence, "sha256": m.sha256, "notice": m.notice, "url": m.url})
}

/// A per-photo switch, independent of the selection and Auto Sync. Turning on starts at 100.
fn toggle(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "aiDefringe.toggle";
    let id = photo(s, p, C)?;
    let mut d = (*s.develop_of(id).ok_or_else(|| bad(C, "no such photo"))?).clone();
    let on = match p.get("enabled") {
        Some(v) => v.as_bool().ok_or_else(|| bad(C, "`enabled` must be true or false"))?,
        None => !d.optics.ai_defringe_enabled(),
    };
    if on && s.aidefringe_model().is_none() {
        return Err(bad(C, "AI Defringe needs a model: install one in Settings > AI Defringe"));
    }
    // the switch is its own setting: the Amount stays where it is when it is flipped, so flipping compares with and
    // without; a photo that never had an Amount starts at 100
    let amount = if on && d.optics.ai_defringe_on.is_none() && d.optics.ai_defringe <= 0.0 { 100.0 } else { d.optics.ai_defringe };
    if !amount.is_finite() {
        return Err(bad(C, "the photo has an invalid AI Defringe amount"));
    }
    if d.optics.ai_defringe != amount || d.optics.ai_defringe_on != Some(on) {
        s.end_interaction()?;
        d.optics.ai_defringe = amount;
        d.optics.ai_defringe_on = Some(on);
        let op = s.develop_op(id, d, "AI Defringe").ok_or_else(|| bad(C, "no such photo"))?;
        s.commit("AI Defringe", op)?;
    }
    Ok(json!({"id": id.0, "enabled": on, "amount": amount}))
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    let model = s.aidefringe_model();
    let mut v = json!({
        "runtime": aidefringe::runtime_available(),
        "model": model.as_ref().map(model_json),
        "dir": s.aidefringe_models_dir().map(|d| d.display().to_string()),
        "generation": s.aidefringe.generation,
    });
    if p.get("id").is_some() || s.active().is_some() {
        let id = photo(s, p, "aiDefringe.status")?;
        let state = s.aidefringe_state(id);
        if let Some(o) = v.as_object_mut() {
            o.insert("photo".into(), serde_json::to_value(state).unwrap_or(Value::Null));
        }
    }
    Ok(v)
}

/// Called every frame by the app: takes in a finished correction and keeps the active photo's coming.
fn pump(s: &mut Session, _: &Value) -> Result<Value> {
    let arrived = s.aidefringe_pump();
    Ok(json!({"arrived": arrived, "generation": s.aidefringe.generation}))
}

fn make(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "aiDefringe.make";
    let id = photo(s, p, C)?;
    let changed = s.aidefringe_make_now(id).map_err(|e| bad(C, e))?;
    Ok(json!({"id": id.0, "changed": changed}))
}

fn retry(s: &mut Session, p: &Value) -> Result<Value> {
    let id = photo(s, p, "aiDefringe.retry")?;
    s.aidefringe_retry(id);
    Ok(json!({"id": id.0, "photo": s.aidefringe_state(id)}))
}

fn inspect(_: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "aiDefringe.models.inspect";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "`path` is required"))?;
    let m = aidefringe::inspect(Path::new(path)).map_err(|e| bad(C, e))?;
    Ok(model_json(&m))
}

fn install(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "aiDefringe.models.install";
    let path = str_param(p, "path").ok_or_else(|| bad(C, "`path` is required"))?;
    if !bool_or(p, "acknowledged", false) {
        return Err(bad(C, "show the user the model's licence and notice first, then pass `acknowledged: true`"));
    }
    let dir = s.aidefringe_models_dir().ok_or_else(|| bad(C, "no folder for defringe models (set LIGHTCRAFT_DEFRINGE_MODELS)"))?;
    let m = aidefringe::install(Path::new(path), &dir).map_err(|e| bad(C, e))?;
    s.aidefringe_model_changed();
    Ok(json!({"installed": true, "model": model_json(&m)}))
}

fn remove(s: &mut Session, _: &Value) -> Result<Value> {
    let removed = match s.aidefringe_models_dir() {
        Some(dir) => aidefringe::remove(&dir).map_err(|e| bad("aiDefringe.models.remove", e))?,
        None => false,
    };
    s.aidefringe_model_changed();
    Ok(json!({"removed": removed}))
}

fn clear(s: &mut Session, _: &Value) -> Result<Value> {
    let (files, bytes) = s.aidefringe_clear();
    Ok(json!({"deleted": files, "bytes": bytes}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "aiDefringe.toggle",
            "AI Defringe",
            [],
            None,
            "{id?: photoId, enabled?: bool} → {id, enabled, amount} — turn AI Defringe on (Amount 100 the first time) or off for one photo, independent of selection and Auto Sync; needs an installed defringe model",
            always,
            toggle
        ),
        cmd!(query "aiDefringe.status", "AI Defringe Status", [], None, "{id?} → {runtime, model: {id, name, version, licence, sha256, notice, url} | null, dir, generation, photo?: {state: off | noModel | ready | running | failed | waiting, changed?, done?, total?, error?}}", always, status),
        cmd!(query "aiDefringe.pump", "AI Defringe Pump", [], None, "{} → {arrived, generation} — called every frame by the app: takes in a finished correction and starts the active photo's when AI Defringe is on and it has none", always, pump),
        cmd!(query "aiDefringe.make", "Make AI Defringe Correction", [], None, "{id?} → {id, changed} — make a photo's correction now and wait for it (for scripts); `changed` is how many pixels it changes", always, make),
        cmd!(query "aiDefringe.retry", "Retry AI Defringe", [], None, "{id?} → {id, photo} — try a photo whose correction failed again", always, retry),
        cmd!(query "aiDefringe.models.inspect", "Inspect Defringe Model", [], None, "{path} → {id, name, version, licence, sha256, notice, url} — the defringe-model.json beside an .onnx file, checked; never runs the file", always, inspect),
        cmd!(query "aiDefringe.models.install", "Install Defringe Model", [], None, "{path, acknowledged: true} → {installed, model} — install a defringe model from an .onnx file with a defringe-model.json beside it, after checking its SHA-256 and running it once; it replaces the installed one. `acknowledged` must be true: the user has been shown the model's licence and notice", always, install),
        cmd!(query "aiDefringe.models.remove", "Remove Defringe Model", [], None, "{} → {removed} — delete the installed defringe model (corrections already made stay until cleared)", always, remove),
        cmd!(query "aiDefringe.clear", "Clear AI Defringe Cache", [], None, "{} → {deleted, bytes} — delete every cached AI Defringe correction of this library (they are made again when wanted)", always, clear),
    ]
}

