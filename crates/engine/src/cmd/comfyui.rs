use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_active};
use crate::{Result, Session, comfyui};

fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    let settings = serde_json::from_value(p.clone()).map_err(|e| bad("mask.generate", e.to_string()))?;
    let mask = p
        .get("maskId")
        .map(|v| v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| bad("mask.generate", "maskId must be a mask id")))
        .transpose()?;
    comfyui::start(s, settings, mask).map_err(|e| bad("mask.generate", e))
}

#[cfg(not(target_arch = "wasm32"))]
fn accept(s: &mut Session, _: &Value) -> Result<Value> {
    const C: &str = "mask.acceptGeneration";
    let run = s.generation.clone().ok_or_else(|| bad(C, "no generated replacement"))?;
    if !run.belongs_to(s) {
        return Err(bad(C, "the original library or photo is no longer open; discard this result"));
    }
    let (png, provenance) = run.files().map_err(|e| bad(C, e))?;
    // Unique, create-new writes: never replace a photo, sidecar or another generated result.
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let path = run.directory.join(format!("{}-Generated-{stamp}-{}.png", run.stem, run.id));
    let info = path.with_extension("generation.json");
    let path = path.to_string_lossy().into_owned();
    let info = info.to_string_lossy().into_owned();
    s.check_write_target(&path).map_err(|e| bad(C, e))?;
    s.check_write_target(&info).map_err(|e| bad(C, e))?;
    crate::export::write_file_new(&info, &provenance).map_err(|e| bad(C, e))?;
    if let Err(e) = crate::export::write_file_new(&path, &png) {
        let _ = std::fs::remove_file(&info);
        return Err(bad(C, e));
    }
    // This PNG already contains the developed look. Do not bake the user's rendered-file
    // import preset onto it again. execute catches panics; restore the preference on errors too.
    let preset = s.import_defaults.other_preset.take();
    let imported = s.execute("library.import", &json!({"paths": [path]}));
    s.import_defaults.other_preset = preset;
    let imported = imported?;
    let id = imported
        .get("imported")
        .and_then(|v| v.get(0))
        .and_then(Value::as_u64)
        .ok_or_else(|| bad(C, format!("generated image saved to {path}, but could not be imported")))?;
    s.execute("stack.group", &json!({"ids": [id, run.photo.0], "top": id, "collapsed": false}))?;
    s.selection = crate::Selection { active: Some(lightcraft_catalog::PhotoId(id)), ids: vec![lightcraft_catalog::PhotoId(id)] };
    s.generation = None;
    Ok(json!({"id": id, "original": run.photo.0, "path": path, "provenance": info}))
}

#[cfg(target_arch = "wasm32")]
fn accept(_: &mut Session, _: &Value) -> Result<Value> {
    Err(bad("mask.acceptGeneration", "ComfyUI generation is available in the desktop app only"))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "mask.generate", "Generate replacement", [], None,
            "{endpoint?, model?, prompt, negative?, seed?, maskId?} — send the active developed photo's selected mask region to the user-configured ComfyUI server; starts a cancellable background task; the result is a preview until explicitly accepted",
            has_active, generate),
        cmd!(query "mask.generationStatus", "Generation status", [], None,
            "{} → {running, ready, cancelled, error, id, photo}; no request is made to ComfyUI",
            always, |s, _| Ok(s.generation.as_ref().map_or(json!({"running": false, "ready": false}), |g| g.status()))),
        cmd!(
            "mask.acceptGeneration",
            "Accept replacement",
            [],
            None,
            "{} — save the generated composite as a new full-size sRGB PNG and generation.json recipe, import it and stack it above the original; never overwrite the original",
            always,
            accept
        ),
        cmd!(query "mask.discardGeneration", "Discard replacement", [], None,
        "{} — cancel/discard this generation; pending jobs are removed by id; an already running ComfyUI job may finish (other clients' jobs are never interrupted)",
        always, |s, _| {
            if let Some(g) = &s.generation {
                g.cancel();
                // Keep the handle while it stops, so starting again cannot create orphan workers.
                if !g.running() { s.generation = None; }
            }
            Ok(json!({"discarded": true}))
        }),
    ]
}
