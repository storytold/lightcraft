//! Quick Develop (Library): settings applied to every selected photo at once, each from its own
//! state, as one undo step. Relative steps are `develop.quickAdjust`; this adds the absolute
//! choices of the panel (treatment, white balance, crop ratio, auto tone) by running the
//! single-photo command on each target in turn.

use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, has_selection};
use crate::{Result, Session};

/// Run `command` with `params` on each of `ids` as the active photo, then fold the undo steps
/// into one labelled `label`. The selection is put back afterwards; a photo the command refuses
/// is skipped and reported.
fn for_each(s: &mut Session, ids: &[dac_catalog::PhotoId], command: &str, params: &Value, label: &str) -> Result<Value> {
    let saved = s.selection.clone();
    let before = s.undo.len();
    let mut changed = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for id in ids {
        s.selection.active = Some(*id);
        match s.execute(command, params) {
            Ok(_) => changed += 1,
            Err(e) => {
                if errors.len() < 5 {
                    errors.push(format!("{}: {e}", id.0));
                }
            }
        }
    }
    s.selection = saved;
    let steps = s.undo.len().saturating_sub(before);
    s.merge_undo(steps, label);
    Ok(json!({"changed": changed, "errors": errors}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "develop.quickSet",
        "Quick Develop Settings",
        [],
        None,
        "{ids?, treatment?: color|bw, wb?: asShot|auto|daylight|cloudy|shade|tungsten|fluorescent|flash, aspect?: as crop.aspect, autoTone?: true, preset?: preset id} — one choice of the Quick Develop panel applied to every target photo (each from its own settings) in one undo step → {changed, errors}",
        has_selection,
        |s, p| {
            let c = "develop.quickSet";
            let ids = s.targets(p);
            if ids.is_empty() {
                return Err(bad(c, "no photo selected"));
            }
            // cap: a scripted call with a huge id list must not run for minutes
            if ids.len() > 100_000 {
                return Err(bad(c, "too many photos (at most 100000)"));
            }
            if let Some(t) = p.get("treatment").and_then(Value::as_str) {
                let bw = match t {
                    "bw" | "blackAndWhite" => true,
                    "color" => false,
                    _ => return Err(bad(c, "treatment: color or bw")),
                };
                return for_each(s, &ids, "develop.treatment", &json!({"bw": bw}), "Quick Develop: Treatment");
            }
            if let Some(m) = p.get("wb").and_then(Value::as_str) {
                if m == "custom" {
                    return Err(bad(c, "wb: pick a preset (custom is set with temperature steps)"));
                }
                return for_each(s, &ids, "develop.wb", &json!({"mode": m}), "Quick Develop: White Balance");
            }
            if let Some(a) = p.get("aspect") {
                return for_each(s, &ids, "crop.aspect", &json!({"aspect": a}), "Quick Develop: Crop Ratio");
            }
            if p.get("autoTone").and_then(Value::as_bool) == Some(true) {
                return for_each(s, &ids, "develop.auto", &json!({}), "Quick Develop: Auto Tone");
            }
            if let Some(id) = p.get("preset").and_then(Value::as_str) {
                let list: Vec<u64> = ids.iter().map(|i| i.0).collect();
                s.execute("preset.apply", &json!({"id": id, "ids": list}))?;
                return Ok(json!({"changed": ids.len(), "errors": []}));
            }
            Err(bad(c, "give one of treatment, wb, aspect, autoTone, preset"))
        }
    )]
}
