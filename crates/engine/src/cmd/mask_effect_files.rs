//! Mask effect preset files: Lightroom Classic local adjustment presets (`.lrtemplate`, Lua,
//! `type = "LocalizedAdjustmentPreset"`), Camera Raw local correction presets (`.xmp` with
//! `crs:Local*` fields, from its `LocalCorrections` folder), our `.lceffect` JSON, and `.zip`
//! bundles of these. Folders, and Lightroom users' "divider" presets (`10 ----- DETAIL ----- 10`,
//! empty presets that head a numbered section of the menu), become groups.
//!
//! Read from the files' public structure and black-box observation; settings we have no slider
//! for are reported and kept with the effect (an export carries them).

use lightcraft_develop::LocalAdjustments;
use serde_json::{Map, Value, json};

use super::mask_effects::{MaskEffect, upsert};
use super::{bad, str_param};
use crate::preset_import::{Lua, group_from_dir, parse_lua, read_zip};
use crate::{Result, Session};

/// File extension of exported effect presets (JSON).
pub const EFFECT_EXT: &str = "lceffect";
/// `format` tag of an exported effect-preset file.
const FILE_FORMAT: &str = "lightcraft.maskEffects";
/// The Lua `type` of a Lightroom local adjustment preset.
const LR_LOCAL_TYPE: &str = "LocalizedAdjustmentPreset";
/// Larger files aren't presets (or bundles of them).
const MAX_FILE: u64 = 64 << 20;
/// How deep nested Lua tables are kept as JSON.
const MAX_DEPTH: usize = 16;

/// An effect read from a file, with the settings it couldn't keep and where it came from.
#[derive(Clone, Debug)]
pub struct Found {
    pub effect: MaskEffect,
    pub unmapped: Vec<String>,
    pub file: String,
    /// Effects of one folder (or folder inside a bundle) share this key: divider presets group
    /// the effects next to them.
    pub dir: String,
    /// The group the folder gives (when the file names none).
    pub dir_group: Option<String>,
    /// The file named its group (`.lceffect`, `crs:Group`): kept.
    pub has_group: bool,
}

/// A readable name for a Lightroom local setting we can't carry over.
fn setting_label(key: &str) -> String {
    let k = key.trim_start_matches("Local");
    let lower = k.to_ascii_lowercase();
    match lower.as_str() {
        "grain" => "Grain".into(),
        "toningluminance" => "Color luminance".into(),
        "pointcolors" => "Point Color".into(),
        "refinesaturation" => "Refine Saturation".into(),
        "curverefinesaturation" => "Refine Saturation".into(),
        "maincurve" | "redcurve" | "greencurve" | "bluecurve" => "Curve".into(),
        _ => k.to_string(),
    }
}

/// A Lua value as JSON (tables nested at most [`MAX_DEPTH`] deep).
fn lua_json(v: &Lua, depth: usize) -> Value {
    match v {
        Lua::Nil => Value::Null,
        Lua::Bool(b) => json!(b),
        Lua::Num(n) => serde_json::Number::from_f64(*n).map(Value::Number).unwrap_or(Value::Null),
        Lua::Str(s) => json!(s),
        Lua::Table(..) if depth >= MAX_DEPTH => Value::Null,
        Lua::Table(items, fields) if fields.is_empty() => Value::Array(items.iter().map(|x| lua_json(x, depth + 1)).collect()),
        Lua::Table(_, fields) => Value::Object(fields.iter().map(|(k, x)| (k.clone(), lua_json(x, depth + 1))).collect()),
    }
}

/// Does a setting value do anything (a non-zero number, a non-empty table, …)?
fn meaningful(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x.abs() > 1e-9),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Is `text` a Lightroom local adjustment preset (`.lrtemplate`)?
pub fn is_lr_local_template(text: &str) -> bool {
    parse_lua(text.trim_start_matches('\u{feff}'))
        .ok()
        .and_then(|root| root.get("type").and_then(Lua::str).map(|t| t == LR_LOCAL_TYPE))
        .unwrap_or(false)
}

/// `$$$/Path/Key=Default` (a localisable string) → `Default`.
fn delocalize(t: &str) -> &str {
    t.strip_prefix("$$$/").and_then(|r| r.split_once('=')).map_or(t, |(_, d)| d)
}

/// A Lightroom local adjustment preset → an effect named after its title (else `stem`), and the
/// settings we have no equivalent for (e.g. `Grain`, `Point Color`), which the effect keeps in
/// `extra`.
pub fn parse_lr_local_template(text: &str, stem: &str) -> std::result::Result<(MaskEffect, Vec<String>), String> {
    let root = parse_lua(text.trim_start_matches('\u{feff}'))?;
    if root.get("type").and_then(Lua::str) != Some(LR_LOCAL_TYPE) {
        return Err("not a local adjustment preset".into());
    }
    let Some(Lua::Table(_, fields)) = root.get("value") else { return Err("no adjustments in this local preset".into()) };
    // `exposure2012` → `LocalExposure2012`: the template's keys are the XMP `Local*` names, lower camel case
    let crs = |k: &str| {
        let mut c = k.chars();
        c.next().map(|f| format!("Local{}{}", f.to_ascii_uppercase(), c.as_str())).unwrap_or_default()
    };
    let num = |k: &str| fields.iter().find(|(f, _)| crs(f) == k).and_then(|(_, v)| if let Lua::Num(n) = v { Some(*n) } else { None });
    let values = crate::crs_masks::local_values(num);
    let mut extra = Map::new();
    for (k, v) in fields {
        if crate::crs_masks::is_local_field(&crs(k)) {
            continue;
        }
        let j = lua_json(v, 0);
        if meaningful(&j) {
            extra.insert(k.clone(), j);
        }
    }
    let title = root.get("title").or_else(|| root.get("internalName")).and_then(Lua::str).map(str::trim).filter(|t| !t.is_empty());
    let name = title.map(|t| delocalize(t).trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| stem.to_string());
    build(name, values, extra)
}

fn build(name: String, values: Map<String, Value>, extra: Map<String, Value>) -> std::result::Result<(MaskEffect, Vec<String>), String> {
    let adjust: LocalAdjustments = serde_json::from_value(Value::Object(values)).map_err(|e| format!("unreadable local preset: {e}"))?;
    let mut unmapped: Vec<String> = extra.keys().map(|k| setting_label(k)).collect();
    unmapped.sort();
    unmapped.dedup();
    let effect = MaskEffect { name, adjust, extra, ..Default::default() }.normalized();
    Ok((effect, unmapped))
}

/// The local slider fields of a Camera Raw local correction preset: its top-level `crs:Local*`
/// properties, or (a preset typed `Local` without them) its first correction's.
fn xmp_local_fields(d: &lightcraft_meta::XmpData) -> Option<Map<String, Value>> {
    let mut out = Map::new();
    for (k, v) in &d.properties {
        if let Some(name) = k.strip_prefix("crs:").filter(|n| n.starts_with("Local") && !n.contains('/'))
            && let Some(x) = v.first()
        {
            out.insert(name.to_string(), json!(x.trim()));
        }
    }
    if !out.is_empty() {
        return Some(out);
    }
    let typed_local = d.properties.get("crs:PresetType").and_then(|v| v.first()).is_some_and(|t| t.trim().eq_ignore_ascii_case("local"));
    if !typed_local {
        return None;
    }
    let correction = crate::crs_masks::CONTAINERS.iter().find_map(|c| d.values.get(*c).and_then(|l| l.items().first()))?;
    let lightcraft_meta::XmpValue::Struct(m) = correction else { return None };
    for (k, v) in m {
        if let Some(name) = k.strip_prefix("crs:").filter(|n| n.starts_with("Local"))
            && let Some(x) = v.text()
        {
            out.insert(name.to_string(), json!(x));
        }
    }
    Some(out)
}

/// Is `text` a Camera Raw local correction preset (XMP)?
pub fn is_local_xmp(text: &str) -> bool {
    lightcraft_meta::parse_xmp(text.trim_start_matches('\u{feff}')).ok().and_then(|d| xmp_local_fields(&d)).is_some()
}

/// A Camera Raw local correction preset (XMP) → an effect named after `crs:Name` (else `stem`),
/// in `crs:Group` if it names one.
pub fn parse_local_xmp(text: &str, stem: &str) -> std::result::Result<(MaskEffect, Vec<String>), String> {
    let d = lightcraft_meta::parse_xmp(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    let fields = xmp_local_fields(&d).ok_or("not a local correction preset (no crs:Local… settings)")?;
    let num = |v: &Value| v.as_str().map(|s| s.strip_prefix('+').unwrap_or(s)).and_then(|s| s.parse::<f64>().ok()).filter(|x| x.is_finite());
    let values = crate::crs_masks::local_values(|k| fields.get(k).and_then(num));
    let mut extra = Map::new();
    for (k, v) in &fields {
        if crate::crs_masks::is_local_field(k) {
            continue;
        }
        let v = num(v).map_or_else(|| v.clone(), |x| json!(x));
        if meaningful(&v) {
            extra.insert(k.clone(), v);
        }
    }
    let first = |k: &str| d.properties.get(k).and_then(|v| v.first()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let name = first("crs:Name").unwrap_or_else(|| stem.to_string());
    let (mut e, unmapped) = build(name, values, extra)?;
    e.group = first("crs:Group").unwrap_or_default();
    Ok((e, unmapped))
}

/// Effects in an `.lceffect` file: `{format, version, effects: [...]}`, a bare array, or one effect.
pub fn parse_file(bytes: &[u8]) -> std::result::Result<Vec<MaskEffect>, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("not an effect preset file: {e}"))?;
    let items = match v {
        Value::Object(mut o) if o.contains_key("effects") => o.remove("effects").unwrap_or_default(),
        v @ Value::Array(_) => v,
        v @ Value::Object(_) => Value::Array(vec![v]),
        _ => return Err("not an effect preset file".into()),
    };
    let list: Vec<MaskEffect> = serde_json::from_value(items).map_err(|e| format!("not an effect preset file: {e}"))?;
    let list: Vec<MaskEffect> = list.into_iter().filter(|e| !e.name.trim().is_empty()).map(MaskEffect::normalized).collect();
    if list.is_empty() {
        return Err("no effect presets in the file".into());
    }
    Ok(list)
}

fn ext_of(name: &str) -> String {
    std::path::Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

fn stem_of(name: &str) -> String {
    std::path::Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "Effect".into())
}

type Parsed = std::result::Result<Vec<(MaskEffect, Vec<String>)>, String>;

/// The effects in file `name` (`bytes`) if it is a local preset (`.lrtemplate` of the local type,
/// a local correction `.xmp`, an `.lceffect`); `None` for any other file (a develop preset…).
pub fn parse_bytes(name: &str, bytes: &[u8]) -> Option<Parsed> {
    let stem = stem_of(name);
    match ext_of(name).as_str() {
        "lrtemplate" => {
            let text = String::from_utf8_lossy(bytes);
            is_lr_local_template(&text).then(|| parse_lr_local_template(&text, &stem).map(|e| vec![e]))
        }
        "xmp" => {
            let text = String::from_utf8_lossy(bytes);
            is_local_xmp(&text).then(|| parse_local_xmp(&text, &stem).map(|e| vec![e]))
        }
        EFFECT_EXT => Some(parse_file(bytes).map(|v| v.into_iter().map(|e| (e, Vec::new())).collect())),
        _ => None,
    }
}

/// The group effects in folder `dir` (relative to the imported folder) go to.
pub fn group_for_dir(dir: &str) -> Option<String> {
    effect_group_from_dir(dir)
}

/// Folder names that only say "local presets" (`Local Adjustment Presets`, `LocalCorrections`…)
/// give no group.
fn effect_group_from_dir(dir: &str) -> Option<String> {
    const FILLER: &[&str] = &[
        "local",
        "adjustment",
        "adjustments",
        "localcorrections",
        "corrections",
        "correction",
        "brush",
        "brushes",
        "mask",
        "masks",
        "effect",
        "effects",
    ];
    let generic = |part: &str| {
        let lower = part.to_ascii_lowercase();
        let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        words.iter().any(|w| FILLER.contains(w))
            && words.iter().all(|w| FILLER.contains(w) || matches!(*w, "presets" | "preset" | "lightroom" | "camera" | "raw" | "classic"))
    };
    let kept: Vec<&str> = dir.split(['/', '\\']).filter(|p| !p.is_empty() && !generic(p)).collect();
    group_from_dir(&kept.join("/"))
}

/// Effects found in `bytes` of file `file` (a `.zip` is opened: the local presets inside).
/// `dir_group`: the group the file's folder gives. Errors are `(file, message)`.
pub fn found_in(file: &str, bytes: &[u8], dir_group: Option<String>, out: &mut Vec<Found>, failed: &mut Vec<(String, String)>) -> bool {
    let dir = std::path::Path::new(file).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
    if ext_of(file) == "zip" {
        let Ok(entries) = read_zip(bytes) else { return false };
        let zip_group = effect_group_from_dir(&stem_of(file)).or(dir_group);
        let mut any = false;
        for (inner, data) in entries {
            let inner_dir = inner.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
            let group = effect_group_from_dir(&inner_dir).or_else(|| zip_group.clone());
            let label = format!("{file}/{inner}");
            match parse_bytes(&inner, &data) {
                Some(Ok(list)) => {
                    any = true;
                    push(out, list, &label, format!("{file}/{inner_dir}"), group, &inner);
                }
                Some(Err(e)) => {
                    any = true;
                    failed.push((label, e));
                }
                None => {}
            }
        }
        return any;
    }
    match parse_bytes(file, bytes) {
        Some(Ok(list)) => {
            push(out, list, file, dir, dir_group, file);
            true
        }
        Some(Err(e)) => {
            failed.push((file.to_string(), e));
            true
        }
        None => false,
    }
}

fn push(out: &mut Vec<Found>, list: Vec<(MaskEffect, Vec<String>)>, file: &str, dir: String, dir_group: Option<String>, name: &str) {
    let named = ext_of(name) == EFFECT_EXT;
    for (effect, unmapped) in list {
        let has_group = named || !effect.group.is_empty();
        out.push(Found { effect, unmapped, file: file.to_string(), dir: dir.clone(), dir_group: dir_group.clone(), has_group });
    }
}

/// The number a preset name starts with (`24a-Eye` → 24).
fn leading_number(name: &str) -> Option<u64> {
    let digits: String = name.trim().chars().take_while(char::is_ascii_digit).take(18).collect();
    digits.parse().ok()
}

/// The section title of a divider preset — an effect that sets nothing, named like
/// `10 ----- DETAIL ENHANCERS ----- 10` (a run of at least three `-`, `=`, `_`, `*`, `#`, `~`, `—`).
pub fn divider_title(e: &MaskEffect) -> Option<String> {
    if !e.is_empty() {
        return None;
    }
    let marks = ['-', '=', '_', '*', '#', '~', '—', '–', '•', '.'];
    let chars: Vec<char> = e.name.chars().collect();
    let has_run = chars.windows(3).any(|w| marks.contains(&w[0]) && w[0] == w[1] && w[1] == w[2]);
    if !has_run {
        return None;
    }
    let title: String = e.name.chars().map(|c| if marks.contains(&c) { ' ' } else { c }).collect();
    let title = title.trim().trim_start_matches(|c: char| c.is_ascii_digit() || c.is_whitespace());
    let title = title.trim_end_matches(|c: char| c.is_ascii_digit() || c.is_whitespace());
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if !title.chars().any(char::is_alphanumeric) {
        return None;
    }
    // "DETAIL ENHANCERS" → "Detail Enhancers"
    let shouting = title.chars().any(char::is_alphabetic) && !title.chars().any(char::is_lowercase);
    Some(if shouting {
        title
            .split(' ')
            .map(|w| {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect::<String>()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        title
    })
}

/// Give `found` their groups; returns the divider presets (which become groups, not effects) as
/// `(found, group)`. A numbered effect goes to the divider with the largest number not above its
/// own (`12 Sky` under `10 --- DETAIL --- 10`); others to their folder's group.
pub fn assign_groups(found: Vec<Found>, group: Option<&str>) -> (Vec<Found>, Vec<(Found, String)>) {
    let (dividers, mut effects): (Vec<(Found, Option<String>)>, Vec<(Found, Option<String>)>) =
        found.into_iter().map(|f| (divider_title(&f.effect).filter(|_| !f.has_group), f)).map(|(t, f)| (f, t)).partition(|(_, t)| t.is_some());
    let dividers: Vec<(Found, String)> = dividers.into_iter().filter_map(|(f, t)| t.map(|t| (f, t))).collect();
    for (f, _) in &mut effects {
        if let Some(g) = group {
            f.effect.group = g.to_string();
            continue;
        }
        if f.has_group {
            continue;
        }
        let section = leading_number(&f.effect.name).and_then(|n| {
            dividers
                .iter()
                .filter(|(d, _)| d.dir == f.dir)
                .filter_map(|(d, t)| leading_number(&d.effect.name).filter(|m| *m <= n).map(|m| (m, t)))
                .max_by_key(|(m, _)| *m)
                .map(|(_, t)| t.clone())
        });
        f.effect.group = section.or_else(|| f.dir_group.clone()).unwrap_or_default();
    }
    (effects.into_iter().map(|(f, _)| f).collect(), dividers)
}

/// Add the found effects (grouped; same-named ones replaced; effects left from an earlier import
/// of a divider dropped) and save → the report rows `(imported, dividers)`.
pub fn add_found(s: &mut Session, found: Vec<Found>, group: Option<&str>, dry: bool) -> Result<(Vec<Value>, Vec<Value>)> {
    let (effects, dividers) = assign_groups(found, group);
    let rows: Vec<Value> =
        effects.iter().map(|f| json!({"name": f.effect.name, "group": f.effect.group, "file": f.file, "unmapped": f.unmapped})).collect();
    let div_rows: Vec<Value> = dividers.iter().map(|(f, g)| json!({"name": f.effect.name, "group": g, "file": f.file})).collect();
    if dry || (effects.is_empty() && dividers.is_empty()) {
        return Ok((rows, div_rows));
    }
    s.mask_effects.retain(|e| !(e.is_empty() && dividers.iter().any(|(d, _)| d.effect.name.eq_ignore_ascii_case(&e.name))));
    for f in effects {
        upsert(s, f.effect);
    }
    s.save_prefs()?;
    Ok((rows, div_rows))
}

/// The files under `paths` an effect import reads (folders walked, at most 8 levels), each with
/// the group its folder gives and whether it was named (not found in a folder).
fn effect_files(paths: &[String]) -> Vec<(String, Option<String>, bool)> {
    fn walk(dir: &std::path::Path, base: &std::path::Path, depth: usize, out: &mut Vec<(String, Option<String>, bool)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                if depth < 8 {
                    walk(&p, base, depth + 1, out);
                }
            } else if p.extension().is_some_and(|e| ["lrtemplate", EFFECT_EXT, "xmp", "zip"].iter().any(|x| e.eq_ignore_ascii_case(x))) {
                let rel = p.parent().and_then(|d| d.strip_prefix(base).ok()).map(|d| d.to_string_lossy().to_string());
                out.push((p.to_string_lossy().to_string(), rel.and_then(|d| effect_group_from_dir(&d)), false));
            }
        }
    }
    let mut out = Vec::new();
    for x in paths {
        let p = std::path::Path::new(x);
        if p.is_dir() {
            walk(p, p.parent().unwrap_or(std::path::Path::new("")), 0, &mut out);
        } else {
            out.push((x.clone(), None, true));
        }
    }
    out
}

/// Read a file of at most [`MAX_FILE`] bytes.
pub fn read_capped(path: &str) -> std::result::Result<Vec<u8>, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_FILE {
        return Err(format!("too large for a preset file ({} MB)", len >> 20));
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

pub fn import_effects(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.importEffects";
    let mut paths: Vec<String> =
        p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    if let Some(x) = str_param(p, "path") {
        paths.push(x.to_string());
    }
    if paths.is_empty() {
        return Err(bad(C, "missing `path` / `paths`"));
    }
    let group = str_param(p, "group").map(str::trim).filter(|g| !g.is_empty());
    let dry = p.get("dryRun").and_then(Value::as_bool).unwrap_or(false);
    let (mut found, mut failed) = (Vec::new(), Vec::new());
    for (f, dir_group, named) in effect_files(&paths) {
        match read_capped(&f) {
            Ok(bytes) => {
                if !found_in(&f, &bytes, dir_group, &mut found, &mut failed) && named {
                    failed.push((f, "not a local adjustment preset".into()));
                }
            }
            Err(e) => failed.push((f, e)),
        }
    }
    let (imported, dividers) = add_found(s, found, group, dry)?;
    Ok(json!({"imported": imported, "dividers": dividers, "failed": failed.into_iter().map(|(f, e)| json!([f, e])).collect::<Vec<_>>()}))
}

pub fn export_effects(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "mask.exportEffects";
    let path = str_param(p, "path").filter(|x| !x.trim().is_empty()).ok_or_else(|| bad(C, "missing `path`"))?;
    let mut path = std::path::PathBuf::from(path);
    if path.extension().is_none() {
        path.set_extension(EFFECT_EXT);
    }
    let names: Option<Vec<String>> =
        p.get("names").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect());
    let group = str_param(p, "group").map(str::trim);
    let chosen: Vec<&MaskEffect> = s
        .mask_effects
        .iter()
        .filter(|e| names.as_ref().is_none_or(|n| n.iter().any(|x| x.eq_ignore_ascii_case(&e.name))))
        .filter(|e| group.is_none_or(|g| g.eq_ignore_ascii_case(&e.group)))
        .collect();
    if chosen.is_empty() {
        return Err(bad(C, "no effect presets to export"));
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| crate::EngineError::Other(format!("{}: {e}", dir.display())))?;
    }
    let text = serde_json::to_string_pretty(&json!({"format": FILE_FORMAT, "version": 2, "effects": chosen}))
        .map_err(|e| crate::EngineError::Other(e.to_string()))?;
    std::fs::write(&path, text).map_err(|e| crate::EngineError::Other(format!("{}: {e}", path.display())))?;
    Ok(json!({"path": path.display().to_string(), "count": chosen.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A synthetic template in the shape Lightroom writes (our own values).
    const TEMPLATE: &str = r#"s = {
	id = "00000000-0000-0000-0000-000000000001",
	internalName = "Soft Glow",
	title = "Soft Glow",
	type = "LocalizedAdjustmentPreset",
	value = {
		clarity = 0.5,
		clarity2012 = -0.25,
		exposure = 0,
		exposure2012 = 0.125,
		grain = 0.25,
		highlights2012 = -0.5,
		luminanceNoise = 0.1,
		refineSaturation = 0,
		temperature = 0.2,
		texture = -0.3,
		toningHue = 240,
		toningLuminance = 0.1,
		toningSaturation = 0,
		pointColors = {
			{
				HueRange = { LowerFull = 0.3, LowerNone = 0, UpperFull = 0.6, UpperNone = 1, },
				HueShift = -0.05,
				RangeAmount = 0.5,
				SrcHue = 0.8,
			},
		},
	},
	version = 0,
}"#;

    #[test]
    fn reads_a_lightroom_local_template() {
        assert!(is_lr_local_template(TEMPLATE));
        let (e, unmapped) = parse_lr_local_template(TEMPLATE, "file").unwrap();
        assert_eq!(e.name, "Soft Glow");
        assert!((e.adjust.exposure - 0.5).abs() < 1e-9, "exposure2012 is a quarter of the stops");
        assert!((e.adjust.clarity + 25.0).abs() < 1e-9, "the 2012 field wins over the old one");
        assert!((e.adjust.highlights + 50.0).abs() < 1e-9);
        assert!((e.adjust.temp - 20.0).abs() < 1e-9);
        assert!((e.adjust.texture + 30.0).abs() < 1e-9);
        assert!((e.adjust.noise - 10.0).abs() < 1e-9);
        assert!((e.adjust.amount - 100.0).abs() < 1e-9);
        assert_eq!(unmapped, vec!["Color luminance".to_string(), "Grain".into(), "Point Color".into()]);
        assert_eq!(e.extra.get("grain"), Some(&json!(0.25)), "kept for export");
        assert_eq!(e.extra["pointColors"][0]["HueRange"]["UpperFull"], json!(0.6));
        assert!(!e.extra.contains_key("refineSaturation"), "zero settings aren't kept");
    }

    #[test]
    fn develop_templates_are_not_local_ones() {
        let develop = r#"s = { title = "X", type = "Develop", value = { settings = { Exposure2012 = 1 } } }"#;
        assert!(!is_lr_local_template(develop));
        assert!(parse_lr_local_template(develop, "x").is_err());
        assert!(parse_bytes("x.lrtemplate", develop.as_bytes()).is_none(), "left to the develop preset import");
    }

    #[test]
    fn malformed_files_are_errors_not_panics() {
        for bad in [
            "garbage {",
            "",
            "s = {",
            r#"s = { type = "LocalizedAdjustmentPreset", value = 3 }"#,
            r#"s = { type = "LocalizedAdjustmentPreset" }"#,
            r#"s = { type = "LocalizedAdjustmentPreset", value = { exposure2012 = "x", grain = {{{{}}}} } }"#,
            "s = { type = \"LocalizedAdjustmentPreset\", value = { exposure2012 = 1e400, contrast2012 = -1e400 } }",
            "\u{feff}s = { type = \"LocalizedAdjustmentPreset\", title = \"\", value = {} }",
        ] {
            let _ = parse_lr_local_template(bad, "x");
            let _ = parse_bytes("x.lrtemplate", bad.as_bytes());
        }
        // deeply nested tables
        let deep = format!("s = {{ type = \"LocalizedAdjustmentPreset\", value = {{ pointColors = {}1{} }} }}", "{".repeat(200), "}".repeat(200));
        let _ = parse_lr_local_template(&deep, "x");
        let (e, _) = parse_lr_local_template("s = { type = \"LocalizedAdjustmentPreset\", value = { exposure2012 = 9 } }", "Stem").unwrap();
        assert_eq!((e.name.as_str(), e.adjust.exposure), ("Stem", 4.0), "values clamp, the file name names it");
        for bad in ["", "<x:xmpmeta", "<x:xmpmeta></x:xmpmeta>", "\u{0}\u{1}"] {
            assert!(parse_bytes("x.xmp", bad.as_bytes()).is_none_or(|r| r.is_err()));
        }
        for bad in ["", "3", "[]", "{\"effects\": 3}", "[{\"name\": 4}]", "{\"effects\": [{\"name\": \" \"}]}"] {
            assert!(parse_file(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(!found_in("x.zip", b"PK\x05\x06garbage", None, &mut Vec::new(), &mut Vec::new()));
    }

    /// A Camera Raw local correction preset (our own values).
    const ACR: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
  crs:PresetType="Local" crs:LocalExposure2012="+0.25" crs:LocalShadows2012="0.4" crs:LocalGrain="0.2" crs:LocalTexture="0">
 <crs:Name><rdf:Alt><rdf:li xml:lang="x-default">Lift Shadows</rdf:li></rdf:Alt></crs:Name>
 <crs:Group><rdf:Alt><rdf:li xml:lang="x-default">Faces</rdf:li></rdf:Alt></crs:Group>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;

    #[test]
    fn reads_a_camera_raw_local_preset() {
        assert!(is_local_xmp(ACR));
        let (e, unmapped) = parse_local_xmp(ACR, "file").unwrap();
        assert_eq!((e.name.as_str(), e.group.as_str()), ("Lift Shadows", "Faces"));
        assert_eq!((e.adjust.exposure, e.adjust.shadows), (1.0, 40.0));
        assert_eq!(unmapped, vec!["Grain".to_string()]);
        // a develop preset is not one
        let develop = ACR
            .replace(
                "crs:LocalExposure2012=\"+0.25\" crs:LocalShadows2012=\"0.4\" crs:LocalGrain=\"0.2\" crs:LocalTexture=\"0\"",
                "crs:Exposure2012=\"1\"",
            )
            .replace("PresetType=\"Local\"", "PresetType=\"Normal\"");
        assert!(!is_local_xmp(&develop));
        // a preset typed Local that keeps its sliders in a correction
        let nested = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:PresetType="Local">
 <crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:LocalClarity2012="-0.5"/></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let (e, _) = parse_local_xmp(nested, "Soft").unwrap();
        assert_eq!((e.name.as_str(), e.adjust.clarity), ("Soft", -50.0));
    }

    fn found(name: &str, exposure: f64, dir: &str) -> Found {
        Found {
            effect: MaskEffect { name: name.into(), adjust: LocalAdjustments { exposure, ..Default::default() }, ..Default::default() },
            unmapped: Vec::new(),
            file: format!("{dir}/{name}.lrtemplate"),
            dir: dir.into(),
            dir_group: None,
            has_group: false,
        }
    }

    #[test]
    fn dividers_become_groups() {
        let e = |n: &str| MaskEffect { name: n.into(), ..Default::default() };
        assert_eq!(divider_title(&e("10 ----- DETAIL ENHANCERS ----- 10")).as_deref(), Some("Detail Enhancers"));
        assert_eq!(divider_title(&e("50--------MISC-----50")).as_deref(), Some("Misc"));
        assert_eq!(divider_title(&e("=== Portraits ===")).as_deref(), Some("Portraits"));
        assert_eq!(divider_title(&e("11_General_-_All_Purpose")), None, "no run of marks");
        assert_eq!(divider_title(&e("-----")), None, "no title");
        let busy = MaskEffect { adjust: LocalAdjustments { exposure: 1.0, ..Default::default() }, ..e("20 --- X --- 20") };
        assert_eq!(divider_title(&busy), None, "a divider sets nothing");

        let list = vec![
            found("01 Burn", -1.0, "a"),
            found("10 ----- DETAIL ENHANCERS ----- 10", 0.0, "a"),
            found("12 Sky", 0.5, "a"),
            found("20 ----- RETOUCHING ----- 20", 0.0, "a"),
            found("24a-Eye-Cleaner", 0.1, "a"),
            found("99 Late", 0.1, "a"),
            found("Burn (Darken)", -0.5, "a"),
            Found { dir_group: Some("Other".into()), ..found("15 Elsewhere", 0.2, "b") },
        ];
        let (effects, dividers) = assign_groups(list.clone(), None);
        let groups: Vec<(&str, &str)> = effects.iter().map(|f| (f.effect.name.as_str(), f.effect.group.as_str())).collect();
        assert_eq!(
            groups,
            vec![
                ("01 Burn", ""),
                ("12 Sky", "Detail Enhancers"),
                ("24a-Eye-Cleaner", "Retouching"),
                ("99 Late", "Retouching"),
                ("Burn (Darken)", ""),
                ("15 Elsewhere", "Other"),
            ]
        );
        assert_eq!(dividers.len(), 2);
        let (effects, _) = assign_groups(list, Some("Mine"));
        assert!(effects.iter().all(|f| f.effect.group == "Mine"));
    }

    #[test]
    fn folder_names_that_only_say_local_presets_give_no_group() {
        assert_eq!(effect_group_from_dir("Local Adjustment Presets"), None);
        assert_eq!(effect_group_from_dir("LocalCorrections"), None);
        assert_eq!(effect_group_from_dir("Local Adjustment Presets/Portrait Brushes"), Some("Portrait Brushes".into()));
        assert_eq!(effect_group_from_dir("Skin Kit"), Some("Skin Kit".into()));
    }
}
