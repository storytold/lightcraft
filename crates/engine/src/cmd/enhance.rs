//! Enhance commands: `enhance.superResolution` and `enhance.aiSharpen` (see [`crate::enhance`]).
//!
//! Run on the spot here (CLI, MCP, control channel); the desktop UI plans the same job with
//! [`Session::plan_enhance`] and runs it on a worker thread.

use serde_json::Value;

use super::{CommandSpec, bad, cmd};
use crate::{Result, Session};

/// Super Resolution's factor when none is asked for (Lightroom's).
const DEFAULT_SCALE: u64 = 2;

fn can_enhance(s: &Session) -> std::result::Result<(), String> {
    if cfg!(target_arch = "wasm32") && s.media.file_bytes.is_none() {
        return Err("Enhance needs access to the photo files".into());
    }
    if s.selected().is_empty() { Err("select a photo".into()) } else { Ok(()) }
}

/// The one photo the command acts on (`id`, else the selection).
fn target(s: &Session, c: &str, p: &Value) -> Result<lightcraft_catalog::PhotoId> {
    match s.targets(p)?.as_slice() {
        [id] => Ok(*id),
        [] => Err(bad(c, "select a photo")),
        _ => Err(bad(c, "one photo at a time: select a single photo or pass `id`")),
    }
}

fn super_resolution(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "enhance.superResolution";
    let scale = match p.get("scale") {
        None | Some(Value::Null) => DEFAULT_SCALE,
        Some(v) => v.as_u64().filter(|f| [2, 4].contains(f)).ok_or_else(|| bad(C, "`scale` must be 2 or 4"))?,
    };
    let id = target(s, C, p)?;
    s.enhance_now(id, scale as usize)
}

fn ai_sharpen(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "enhance.aiSharpen";
    if p.get("scale").is_some_and(|v| !v.is_null()) {
        return Err(bad(C, "AI Sharpen keeps the photo's size: `scale` is for `enhance.superResolution`"));
    }
    let id = target(s, C, p)?;
    s.enhance_now(id, 1)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "enhance.superResolution",
                "Super Resolution",
                [],
                None,
                "{id?, scale=2|4}: the photo with its import defaults (none of its edits), enlarged `scale` times across by the installed upscaler (`denoise.models.select {kind: \"upscale\"}`), written as a 16-bit TIFF `<name>-Enhanced-SR.tif` next to it, imported, stacked with the original with the new photo on top, and selected → {id, path, bytes, width, height, factor}",
                can_enhance,
                super_resolution
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "enhance.aiSharpen",
                "AI Sharpen",
                [],
                None,
                "{id?} (no `scale`): like `enhance.superResolution`, but the upscaler's answer is averaged back down to the photo's own size: finer detail at the same pixel dimensions → {id, path, bytes, width, height, factor: 1}",
                can_enhance,
                ai_sharpen
            )
        },
    ]
}
