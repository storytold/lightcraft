//! Library preferences (Settings → Import / Performance): develop defaults applied on import and
//! the thumbnail cache budget. Saved in the library's `prefs.json` (see [`crate::library`]).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::import::{CameraDefault, ImportDefaults};
use crate::{Result, Session};

const ID: &str = "library.preferences";

fn current(s: &Session) -> Value {
    json!({
        "xmp": s.xmp,
        "import": s.import_defaults,
        "cacheMb": if s.cache_mb == 0 { s.cache_bytes() >> 20 } else { u64::from(s.cache_mb) },
        "forgetLocalDays": s.forget_local_days,
        "persistent": s.library.is_some(),
    })
}

/// `"default"`, `""` or null = no preset; anything else must name an existing preset.
fn preset_ref(s: &Session, v: &Value) -> Result<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::String(x) if x.is_empty() || x == "default" => Ok(None),
        Value::String(x) if s.presets.iter().any(|p| p.id == *x) => Ok(Some(x.clone())),
        Value::String(x) => Err(bad(ID, format!("unknown preset `{x}` (see preset.list)"))),
        other => Err(bad(ID, format!("preset must be a preset id or \"default\", not {other}"))),
    }
}

fn prefs(s: &mut Session, p: &Value) -> Result<Value> {
    let mut d: ImportDefaults = s.import_defaults.clone();
    let mut changed = false;
    if let Some(i) = p.get("import") {
        if let Some(v) = i.get("rawPreset") {
            d.raw_preset = preset_ref(s, v)?;
        }
        if let Some(v) = i.get("otherPreset") {
            d.other_preset = preset_ref(s, v)?;
        }
        if let Some(v) = i.get("perCamera").and_then(Value::as_bool) {
            d.per_camera = v;
        }
        if let Some(v) = i.get("showOnCard").and_then(Value::as_bool) {
            d.show_on_card = v;
        }
        if let Some(v) = i.get("copyright").and_then(Value::as_str) {
            d.copyright = v.trim().to_string();
        }
        if let Some(v) = i.get("creator").and_then(Value::as_str) {
            d.creator = v.trim().to_string();
        }
        // (the kinds that are named: the others stay as they are)
        if let Some(read) = i.get("read").filter(|r| !r.is_null()) {
            let read = crate::import::ReadChoice::parse(read).map_err(|e| bad(ID, e))?.over(d.read);
            d.set_read(read);
        }
        // how a dialog's Read row was last used (the app says it when the dialog is confirmed)
        if let Some(last) = i.get("readLast").filter(|r| !r.is_null()) {
            let Some(last) = last.as_object() else { return Err(bad(ID, "`readLast` is an object: {import?, synchronize?}")) };
            for (name, read) in last {
                let Some(dialog) = crate::import::ReadDialog::ALL.into_iter().find(|d| d.name() == name) else {
                    return Err(bad(ID, format!("`readLast.{name}`: the dialogs are import and synchronize")));
                };
                let read = match read {
                    Value::Null => None,
                    // (every kind, the ones left out as the dialog would open now)
                    r => Some(crate::import::ReadChoice::parse(r).map_err(|e| bad(ID, e))?.over(d.read_in(dialog))),
                };
                d.remember_read(dialog, read);
            }
        }
        match i.get("metadataPreset") {
            Some(Value::String(n)) if n.is_empty() || n == "none" => d.metadata_preset = None,
            Some(Value::String(n)) => {
                if !s.metadata_presets.iter().any(|m| m.name.eq_ignore_ascii_case(n)) {
                    return Err(bad(ID, format!("unknown metadata preset `{n}` (see metadata.presets)")));
                }
                d.metadata_preset = Some(n.clone());
            }
            Some(Value::Null) => d.metadata_preset = None,
            _ => {}
        }
        if let Some(a) = i.get("cameras").and_then(Value::as_array) {
            d.cameras.clear();
            for c in a {
                let camera = c.get("camera").and_then(Value::as_str).unwrap_or_default().trim().to_string();
                if camera.is_empty() {
                    return Err(bad(ID, "each camera default needs `camera` (make + model)"));
                }
                let preset = preset_ref(s, c.get("preset").unwrap_or(&Value::Null))?;
                d.cameras.push(CameraDefault { camera, preset });
            }
        }
        changed = true;
    }
    // set or clear one camera's default: {camera: {camera: "Make Model", preset: id|null}}
    if let Some(c) = p.get("camera") {
        let camera = c.get("camera").and_then(Value::as_str).unwrap_or_default().trim().to_string();
        if camera.is_empty() {
            return Err(bad(ID, "`camera.camera` (make + model) is required"));
        }
        let preset = preset_ref(s, c.get("preset").unwrap_or(&Value::Null))?;
        d.cameras.retain(|x| !x.camera.eq_ignore_ascii_case(&camera));
        if c.get("remove").and_then(Value::as_bool) != Some(true) {
            d.cameras.push(CameraDefault { camera, preset });
        }
        changed = true;
    }
    s.import_defaults = d;
    if let Some(v) = p.get("forgetLocalDays") {
        let days = v.as_u64().ok_or_else(|| bad(ID, "forgetLocalDays must be a whole number of days (0 = never)"))?;
        s.forget_local_days = days.min(36_500) as u32;
        changed = true;
    }
    if let Some(mb) = p.get("cacheMb") {
        let mb = mb.as_u64().ok_or_else(|| bad(ID, "cacheMb must be a number of megabytes"))?;
        s.set_cache_mb(mb.clamp(0, 1 << 20) as u32)?;
    } else if changed {
        s.save_prefs()?;
    }
    Ok(current(s))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "library.preferences",
        "Library Preferences",
        [],
        None,
        "{import?: {read?: {keywords?, ratings?, descriptions?, location?, develop?: bool} (what imports read from the photos themselves unless one says otherwise, see `library.import`; the kinds named change, the others stay; changing it makes the dialogs follow it again, see `readLast`; a Lightroom catalog import and Read Metadata from File always read everything; the library's default creator and copyright are not given to a photo whose own were left in the file), readLast?: {import?, synchronize?: {keywords?, …}|null} (how the Read row of the Import Photos review / of Synchronize Folder was last used: each dialog opens with it; null, or ticks that are the library's own, forget it; nothing else goes by it — dropped files, Auto Import and agents' imports follow `read`), rawPreset?: presetId|\"default\", otherPreset?: presetId|\"default\", perCamera?: bool, cameras?: [{camera: \"Make Model\", preset: presetId|null}], copyright?: text, creator?: text (given to imported photos without one), showOnCard?: bool (open the import review when a camera or card is connected)}, camera?: {camera, preset?, remove?: bool}, cacheMb?: n (0 = default), forgetLocalDays?: n (forget untouched Local photos of folders not browsed for n days; 0 = never)} — develop defaults applied on import (raws / other images / per camera) and the thumbnail cache size, saved with the library → {xmp, import, cacheMb, forgetLocalDays, persistent}",
        always,
        prefs
    )]
}
