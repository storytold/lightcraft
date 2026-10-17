//! Library preferences (Settings → Import / Performance): develop defaults applied on import and
//! the thumbnail cache budget. Saved in the library's `prefs.json` (see [`crate::library`]).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_active, str_param};
use crate::import::{CameraDefault, ImportDefaults};
use crate::{Result, Session};
use lcc_develop::{Preset, SettingsGroup};

const ID: &str = "library.preferences";
const DEFAULT_ID: &str = "library.setImportDefault";

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

/// Save the active photo's settings — the groups asked for, Optics by default — as a user preset and
/// make it the default for photos imported from now on: the raw default, the other-images default, or
/// this camera's when per-camera defaults are on (issue #148: the Optics checkboxes reset on every
/// photo, so the state a person wants for a batch belongs in a default). One preset per name, updated
/// in place, so pressing the button again doesn't pile presets up. It carries only the groups asked
/// for, never the photo's whole edit, and photos already in the library keep their settings.
fn set_import_default(s: &mut Session, p: &Value) -> Result<Value> {
    let c = DEFAULT_ID;
    let id = s.active().ok_or_else(|| bad(c, "select a photo first"))?;
    let d = (*s.develop_of(id).unwrap_or_default()).clone();
    let groups = super::develop::groups_param(p, c)?.unwrap_or_else(|| vec![SettingsGroup::Optics]);
    let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(crate::import::NEW_PHOTO_DEFAULT).to_string();
    let photo_camera = s.catalog.photo(id).map(|p| p.meta.camera.trim().to_string()).unwrap_or_default();
    let scope = match str_param(p, "scope") {
        Some("raw") => "raw",
        Some("other") => "other",
        Some("camera") => "camera",
        Some(x) => return Err(bad(c, format!("scope must be raw, other or camera, not `{x}`"))),
        // per-camera defaults on and the photo names its camera: that camera's, as Settings shows
        None if s.import_defaults.per_camera && !photo_camera.is_empty() => "camera",
        None => "raw",
    };
    let camera = match scope {
        "camera" => str_param(p, "camera").map(str::trim).filter(|s| !s.is_empty()).unwrap_or(photo_camera.as_str()).to_string(),
        _ => String::new(),
    };
    if scope == "camera" && camera.is_empty() {
        return Err(bad(c, "this photo names no camera: pass `camera` or leave the default for raw photos"));
    }
    let existing = s.presets.iter().position(|x| !x.builtin && x.name.eq_ignore_ascii_case(&name));
    let pid = match existing {
        Some(i) => {
            let (pid, group) = match s.presets.get(i) {
                Some(x) => (x.id.clone(), x.group.clone()),
                None => return Err(bad(c, "the preset vanished while it was being updated")),
            };
            let fresh = Preset::from_settings(&pid, &name, &group, &d, &groups);
            match s.presets.get_mut(i) {
                Some(x) => x.settings = fresh.settings,
                None => return Err(bad(c, "the preset vanished while it was being updated")),
            }
            pid
        }
        None => {
            let pid = format!("user.{}.{}", s.presets.len(), name.to_lowercase().replace(' ', "-"));
            s.presets.push(Preset::from_settings(&pid, &name, "User Presets", &d, &groups));
            pid
        }
    };
    match scope {
        "raw" => s.import_defaults.raw_preset = Some(pid.clone()),
        "other" => s.import_defaults.other_preset = Some(pid.clone()),
        _ => {
            s.import_defaults.cameras.retain(|x| !x.camera.eq_ignore_ascii_case(&camera));
            s.import_defaults.cameras.push(CameraDefault { camera: camera.clone(), preset: Some(pid.clone()) });
        }
    }
    s.save_prefs()?;
    Ok(json!({
        "id": pid,
        "name": name,
        "scope": scope,
        "camera": if camera.is_empty() { Value::Null } else { json!(camera) },
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "library.preferences",
            "Library Preferences",
            [],
            None,
            "{import?: {rawPreset?: presetId|\"default\", otherPreset?: presetId|\"default\", perCamera?: bool, cameras?: [{camera: \"Make Model\", preset: presetId|null}], copyright?: text, creator?: text (given to imported photos without one), showOnCard?: bool (open the import review when a camera or card is connected)}, camera?: {camera, preset?, remove?: bool}, cacheMb?: n (0 = default), forgetLocalDays?: n (forget untouched Local photos of folders not browsed for n days; 0 = never)} — develop defaults applied on import (raws / other images / per camera) and the thumbnail cache size, saved with the library → {xmp, import, cacheMb, forgetLocalDays, persistent}",
            always,
            prefs
        ),
        cmd!(
            "library.setImportDefault",
            "Set Default for New Photos",
            [],
            None,
            "{groups?: [settingsGroup] (default [\"optics\"]), name?: text (default \"Optics for new photos\"), scope?: \"raw\"|\"other\"|\"camera\" (default: this camera's when per-camera defaults are on and the active photo names a camera, else raw), camera?: \"Make Model\" (with scope \"camera\")} — save the active photo's settings (only the groups asked for) as a user preset and make it the default for photos imported from now on; the same name updates that one preset → {id, name, scope, camera}",
            has_active,
            set_import_default
        ),
    ]
}
