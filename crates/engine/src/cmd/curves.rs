//! Tone-curve commands: reset a channel or the whole curve; point-curve presets (built-in Linear /
//! Medium / Strong Contrast plus the user's, saved with the library preferences, imported and
//! exported as small JSON files).
//!
//! With `mask: id` the point-curve commands (`develop.curve`, `curve.reset`, `curve.presets`,
//! `curve.applyPreset`, `curve.savePreset`) work on that mask's local Curve instead of the photo's
//! Tone Curve: the same channels without the parametric curve, and the same presets.

use lcc_develop::{DevelopSettings, PointCurves, ToneCurve};
use lcc_geom::Point;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_active, str_param};
use crate::{Result, Session};

/// Channels `curve.reset` accepts.
pub const RESET_CHANNELS: [&str; 7] = ["all", "point", "parametric", "master", "red", "green", "blue"];

/// `c` with `channel` back at its default: `all` (the whole Tone Curve, incl. Refine Saturation),
/// `point` (the four point curves), `parametric` (region sliders and splits) or one point channel.
pub fn reset_channel(c: &mut ToneCurve, channel: &str) -> std::result::Result<(), String> {
    let d = ToneCurve::default();
    match channel {
        "all" => *c = d,
        "point" => {
            c.master.clear();
            c.red.clear();
            c.green.clear();
            c.blue.clear();
        }
        "parametric" => {
            (c.highlights, c.lights, c.darks, c.shadows) = (d.highlights, d.lights, d.darks, d.shadows);
            (c.split_shadows, c.split_mid, c.split_highlights) = (d.split_shadows, d.split_mid, d.split_highlights);
        }
        "master" | "rgb" | "luma" => c.master.clear(),
        "red" => c.red.clear(),
        "green" => c.green.clear(),
        "blue" => c.blue.clear(),
        other => return Err(format!("unknown channel `{other}` (one of {})", RESET_CHANNELS.join(", "))),
    }
    Ok(())
}

/// Channels `curve.reset` accepts for a mask's local Curve.
pub const MASK_RESET_CHANNELS: [&str; 5] = ["all", "master", "red", "green", "blue"];

/// A mask's local Curve `c` with `channel` linear again: `all` (also `point`) or one channel.
pub fn reset_points(c: &mut PointCurves, channel: &str) -> std::result::Result<(), String> {
    if matches!(channel, "all" | "point") {
        *c = PointCurves::default();
        return Ok(());
    }
    c.channel_mut(channel)
        .map(Vec::clear)
        .ok_or_else(|| format!("unknown channel `{channel}` for a mask's curve (one of {})", MASK_RESET_CHANNELS.join(", ")))
}

/// The `mask` parameter of the point-curve commands: the id of the mask whose local Curve they work
/// on, `None` (the photo's Tone Curve) when absent or null.
pub(crate) fn mask_param(p: &Value, c: &str) -> Result<Option<u32>> {
    match p.get("mask") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().and_then(|v| u32::try_from(v).ok()).map(Some).ok_or_else(|| bad(c, "`mask` must be a mask id")),
    }
}

/// Mask `mask`'s local Curve in `d`.
pub(crate) fn mask_curve_mut<'a>(d: &'a mut DevelopSettings, mask: u32, c: &str) -> Result<&'a mut PointCurves> {
    d.masks.iter_mut().find(|m| m.id == mask).map(|m| &mut m.curve).ok_or_else(|| bad(c, format!("no mask {mask} on this photo")))
}

/// The point curves a command with `mask` reads: the mask's local Curve, or the photo's Tone Curve's.
fn points_of(d: &DevelopSettings, mask: Option<u32>, c: &str) -> Result<PointCurves> {
    match mask {
        Some(m) => d.masks.iter().find(|x| x.id == m).map(|x| x.curve.clone()).ok_or_else(|| bad(c, format!("no mask {m} on this photo"))),
        None => Ok(d.curve.points()),
    }
}

fn reset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "curve.reset";
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let channel = str_param(p, "channel").unwrap_or("all");
    let mask = mask_param(p, C)?;
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    let what = match mask {
        Some(m) => {
            reset_points(mask_curve_mut(&mut d, m, C)?, channel).map_err(|e| bad(C, e))?;
            "Mask Curve"
        }
        None => {
            reset_channel(&mut d.curve, channel).map_err(|e| bad(C, e))?;
            "Tone Curve"
        }
    };
    let label = if channel == "all" { format!("Reset {what}") } else { format!("Reset {what} ({channel})") };
    s.set_develop(id, d, &label)?;
    Ok(json!({"channel": channel}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "curve.reset",
            "Reset Tone Curve",
            [],
            None,
            "{channel?: all|point|parametric|master|red|green|blue (default all), mask?: id} — the active photo's tone curve (or one channel) back to linear; with `mask`, that mask's local Curve (no parametric part)",
            has_active,
            reset
        ),
        cmd!(
            query "curve.presets",
            "Point Curve Presets",
            [],
            None,
            "{mask?: id} → {presets: [{name, master, red, green, blue: [[x,y],...], builtin?}], current: name of the preset the active photo's point curves (or that mask's local Curve) match, or null}",
            always,
            list_cmd
        ),
        cmd!(
            "curve.applyPreset",
            "Apply Point Curve Preset",
            [],
            None,
            "{name, mask?: id} — sets the active photo's point curves (all four channels; the parametric curve stays), or with `mask` that mask's local Curve",
            has_active,
            apply_preset
        ),
        cmd!(
            "curve.savePreset",
            "Save Point Curve Preset",
            [],
            None,
            "{name, mask?: id} — the active photo's point curves (or that mask's local Curve) under a name (replaces a user preset of that name; built-in names are reserved), saved with the library",
            has_active,
            save_preset
        ),
        cmd!(
            "curve.deletePreset",
            "Delete Point Curve Preset",
            [],
            None,
            "{name} — a user preset (built-ins can't be deleted)",
            always,
            delete_preset
        ),
        cmd!(
            "curve.importPresets",
            "Import Point Curve Presets",
            [],
            None,
            "{path | paths: [file]} — .lccurve JSON files ({format, version, presets: [...]}, an array of presets, or one preset); same-named user presets are replaced → {imported: [name], failed: [[file, error]]}",
            always,
            import_presets
        ),
        cmd!(
            "curve.exportPresets",
            "Export Point Curve Presets",
            [],
            None,
            "{path, names?: [name] (default: every user preset)} — writes a .lccurve JSON file → {path, count}",
            always,
            export_presets
        ),
    ]
}
// ---------------------------------------------------------------- point-curve presets

/// File extension of exported curve presets (JSON).
pub const CURVE_PRESET_EXT: &str = "lccurve";
/// `format` tag of an exported curve-preset file.
const FILE_FORMAT: &str = crate::legacy::CURVE_PRESETS_FORMAT;

/// A named set of point curves (master + red/green/blue, points in 0..1; empty = linear).
/// Built-ins ship with the app; the user's are saved with the library preferences.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CurvePreset {
    pub name: String,
    pub master: Vec<[f64; 2]>,
    pub red: Vec<[f64; 2]>,
    pub green: Vec<[f64; 2]>,
    pub blue: Vec<[f64; 2]>,
    /// Built-in presets can't be deleted or overwritten (never stored).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub builtin: bool,
}

/// Points clamped to 0..1, sorted by x, duplicates in x dropped; a linear curve becomes empty.
fn normalize(v: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out: Vec<[f64; 2]> =
        v.iter().filter(|p| p[0].is_finite() && p[1].is_finite()).map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)]).collect();
    out.sort_by(|a, b| a[0].total_cmp(&b[0]));
    out.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6);
    if out.len() < 2 || out.iter().all(|p| (p[0] - p[1]).abs() < 1e-9) {
        out.clear();
    }
    out
}

fn from_points(v: &[Point]) -> Vec<[f64; 2]> {
    normalize(&v.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>())
}

fn to_points(v: &[[f64; 2]]) -> Vec<Point> {
    normalize(v).iter().map(|p| Point::new(p[0], p[1])).collect()
}

impl CurvePreset {
    /// The point curves of `c` under `name`.
    pub fn from_curve(name: &str, c: &ToneCurve) -> CurvePreset {
        CurvePreset::from_points(name, &c.points())
    }
    /// The point curves `c` (a mask's local Curve) under `name`.
    pub fn from_points(name: &str, c: &PointCurves) -> CurvePreset {
        CurvePreset {
            name: name.into(),
            master: from_points(&c.master),
            red: from_points(&c.red),
            green: from_points(&c.green),
            blue: from_points(&c.blue),
            builtin: false,
        }
    }
    /// Set the four point curves of `c` (the parametric curve and Refine Saturation stay).
    pub fn apply(&self, c: &mut ToneCurve) {
        let mut p = c.points();
        self.apply_points(&mut p);
        c.set_points(p);
    }
    /// Set the four point curves `c` (a mask's local Curve).
    pub fn apply_points(&self, c: &mut PointCurves) {
        c.master = to_points(&self.master);
        c.red = to_points(&self.red);
        c.green = to_points(&self.green);
        c.blue = to_points(&self.blue);
    }
    /// Does `c` carry exactly this preset's point curves?
    pub fn matches(&self, c: &ToneCurve) -> bool {
        self.matches_points(&c.points())
    }
    /// Do the point curves `c` (a mask's local Curve) match this preset's?
    pub fn matches_points(&self, c: &PointCurves) -> bool {
        let same = |a: &[[f64; 2]], b: &[Point]| {
            let (a, b) = (normalize(a), from_points(b));
            a.len() == b.len() && a.iter().zip(&b).all(|(p, q)| (p[0] - q[0]).abs() < 1e-6 && (p[1] - q[1]).abs() < 1e-6)
        };
        same(&self.master, &c.master) && same(&self.red, &c.red) && same(&self.green, &c.green) && same(&self.blue, &c.blue)
    }
    fn is_linear(&self) -> bool {
        [&self.master, &self.red, &self.green, &self.blue].iter().all(|v| normalize(v).is_empty())
    }
}

/// The presets that ship with the app: a linear curve and two symmetric S-curves (our own
/// values: the quarter-tones pulled 0.05 / 0.11 towards the ends, midpoint fixed).
pub fn builtin_presets() -> Vec<CurvePreset> {
    let s = |name: &str, master: Vec<[f64; 2]>| CurvePreset { name: name.into(), master, builtin: true, ..Default::default() };
    vec![
        s("Linear", Vec::new()),
        s("Medium Contrast", vec![[0.0, 0.0], [0.25, 0.2], [0.5, 0.5], [0.75, 0.8], [1.0, 1.0]]),
        s("Strong Contrast", vec![[0.0, 0.0], [0.25, 0.14], [0.5, 0.5], [0.75, 0.86], [1.0, 1.0]]),
    ]
}

/// Built-in presets, then the user's.
pub fn all_presets(s: &Session) -> Vec<CurvePreset> {
    let mut v = builtin_presets();
    v.extend(s.curve_presets.iter().cloned());
    v
}

/// Name of the preset whose point curves `c` carries (built-ins first), if any.
pub fn matching_preset(s: &Session, c: &ToneCurve) -> Option<String> {
    matching_points_preset(s, &c.points())
}

/// Name of the preset the point curves `c` (a mask's local Curve) match (built-ins first), if any.
pub fn matching_points_preset(s: &Session, c: &PointCurves) -> Option<String> {
    all_presets(s).into_iter().find(|p| p.matches_points(c)).map(|p| p.name)
}

fn is_builtin_name(name: &str) -> bool {
    builtin_presets().iter().any(|p| p.name.eq_ignore_ascii_case(name))
}

/// Add or replace (same name, ignoring case) a user preset.
fn upsert(s: &mut Session, p: CurvePreset) {
    match s.curve_presets.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&p.name)) {
        Some(x) => *x = p,
        None => s.curve_presets.push(p),
    }
}

fn name_param(p: &Value, c: &str) -> Result<String> {
    str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).ok_or_else(|| bad(c, "missing `name`"))
}

fn list(s: &Session) -> Value {
    let current = s.active().and_then(|id| s.develop_of(id)).and_then(|d| matching_preset(s, &d.curve));
    json!({"presets": all_presets(s), "current": current})
}

fn list_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "curve.presets";
    let Some(mask) = mask_param(p, C)? else { return Ok(list(s)) };
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let curve = points_of(&s.develop_of(id).unwrap_or_default(), Some(mask), C)?;
    Ok(json!({"presets": all_presets(s), "current": matching_points_preset(s, &curve)}))
}

fn apply_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "curve.applyPreset";
    let name = name_param(p, C)?;
    let mask = mask_param(p, C)?;
    let preset =
        all_presets(s).into_iter().find(|x| x.name.eq_ignore_ascii_case(&name)).ok_or_else(|| bad(C, format!("no curve preset `{name}`")))?;
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    let label = match mask {
        Some(m) => {
            preset.apply_points(mask_curve_mut(&mut d, m, C)?);
            format!("Mask Curve: {}", preset.name)
        }
        None => {
            preset.apply(&mut d.curve);
            format!("Point Curve: {}", preset.name)
        }
    };
    s.set_develop(id, d, &label)?;
    Ok(json!({"name": preset.name}))
}

fn save_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "curve.savePreset";
    let name = name_param(p, C)?;
    if is_builtin_name(&name) {
        return Err(bad(C, format!("`{name}` is a built-in preset: choose another name")));
    }
    let mask = mask_param(p, C)?;
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let preset = CurvePreset::from_points(&name, &points_of(&s.develop_of(id).unwrap_or_default(), mask, C)?);
    if preset.is_linear() {
        return Err(bad(C, "the point curve is linear: shape it first"));
    }
    upsert(s, preset);
    s.save_prefs()?;
    Ok(list(s))
}

fn delete_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "curve.deletePreset";
    let name = name_param(p, C)?;
    if is_builtin_name(&name) {
        return Err(bad(C, "built-in presets can't be deleted"));
    }
    let n = s.curve_presets.len();
    s.curve_presets.retain(|x| !x.name.eq_ignore_ascii_case(&name));
    if n == s.curve_presets.len() {
        return Err(bad(C, format!("no curve preset `{name}`")));
    }
    s.save_prefs()?;
    Ok(list(s))
}

/// Presets in a curve-preset file: `{format, version, presets: [...]}`, a bare array, or one preset.
pub fn parse_file(bytes: &[u8]) -> std::result::Result<Vec<CurvePreset>, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("not a curve preset file: {e}"))?;
    let items = match &v {
        Value::Object(o) if o.contains_key("presets") => o["presets"].clone(),
        Value::Array(_) => v.clone(),
        Value::Object(_) => Value::Array(vec![v.clone()]),
        _ => return Err("not a curve preset file".into()),
    };
    let list: Vec<CurvePreset> = serde_json::from_value(items).map_err(|e| format!("not a curve preset file: {e}"))?;
    let list: Vec<CurvePreset> = list.into_iter().filter(|p| !p.name.trim().is_empty()).collect();
    if list.is_empty() {
        return Err("no curve presets in the file".into());
    }
    Ok(list)
}

/// The JSON of a curve-preset file holding `presets`.
pub fn to_file(presets: &[CurvePreset]) -> String {
    let presets: Vec<CurvePreset> = presets.iter().map(|p| CurvePreset { builtin: false, ..p.clone() }).collect();
    serde_json::to_string_pretty(&json!({"format": FILE_FORMAT, "version": 1, "presets": presets})).unwrap_or_default()
}

fn import_presets(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "curve.importPresets";
    let mut paths: Vec<String> =
        p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    if let Some(x) = str_param(p, "path") {
        paths.push(x.to_string());
    }
    if paths.is_empty() {
        return Err(bad(C, "missing `path` / `paths`"));
    }
    let (mut imported, mut failed) = (Vec::new(), Vec::new());
    for f in &paths {
        match std::fs::read(f).map_err(|e| e.to_string()).and_then(|b| parse_file(&b)) {
            Ok(list) => {
                for mut preset in list {
                    preset.name = preset.name.trim().to_string();
                    preset.builtin = false;
                    if is_builtin_name(&preset.name) {
                        preset.name = format!("{} (imported)", preset.name);
                    }
                    imported.push(preset.name.clone());
                    upsert(s, preset);
                }
            }
            Err(e) => failed.push(json!([f, e])),
        }
    }
    if !imported.is_empty() {
        s.save_prefs()?;
    }
    Ok(json!({"imported": imported, "failed": failed}))
}

fn export_presets(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "curve.exportPresets";
    let path = str_param(p, "path").filter(|x| !x.trim().is_empty()).ok_or_else(|| bad(C, "missing `path`"))?;
    let mut path = std::path::PathBuf::from(path);
    if path.extension().is_none() {
        path.set_extension(CURVE_PRESET_EXT);
    }
    let names: Option<Vec<String>> =
        p.get("names").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect());
    let chosen: Vec<CurvePreset> = match &names {
        Some(n) => all_presets(s).into_iter().filter(|x| n.iter().any(|m| m.eq_ignore_ascii_case(&x.name))).collect(),
        None => s.curve_presets.clone(),
    };
    if chosen.is_empty() {
        return Err(bad(C, "no curve presets to export (save one first, or pass names)"));
    }
    let target = path.to_string_lossy();
    s.check_write_target(&target).map_err(|e| bad(C, e))?;
    crate::export::write_file_durable(&target, to_file(&chosen).as_bytes()).map_err(crate::EngineError::Other)?;
    Ok(json!({"path": path.display().to_string(), "count": chosen.len()}))
}
