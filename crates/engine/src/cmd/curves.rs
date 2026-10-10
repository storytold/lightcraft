//! Tone-curve commands: reset a channel or the whole curve; point-curve presets (built-in Linear /
//! Medium / Strong Contrast plus the user's, saved with the library preferences, imported and
//! exported as small JSON files).

use lightcraft_develop::{PointCurve, ToneCurve};
use lightcraft_geom::Point;
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
            c.master.reset();
            c.red.reset();
            c.green.reset();
            c.blue.reset();
        }
        "parametric" => {
            (c.highlights, c.lights, c.darks, c.shadows) = (d.highlights, d.lights, d.darks, d.shadows);
            (c.split_shadows, c.split_mid, c.split_highlights) = (d.split_shadows, d.split_mid, d.split_highlights);
        }
        "master" | "rgb" | "luma" => c.master.reset(),
        "red" => c.red.reset(),
        "green" => c.green.reset(),
        "blue" => c.blue.reset(),
        other => return Err(format!("unknown channel `{other}` (one of {})", RESET_CHANNELS.join(", "))),
    }
    Ok(())
}

fn reset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "curve.reset";
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let channel = str_param(p, "channel").unwrap_or("all");
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    reset_channel(&mut d.curve, channel).map_err(|e| bad(C, e))?;
    let label = if channel == "all" { "Reset Tone Curve".to_string() } else { format!("Reset Tone Curve ({channel})") };
    s.set_develop(id, d, &label)?;
    Ok(json!({"channel": channel}))
}

/// The point-curve channel; aliases always report the canonical master name.
pub(crate) fn channel_mut<'a>(curve: &'a mut ToneCurve, channel: &str) -> std::result::Result<(&'static str, &'a mut PointCurve), String> {
    match channel {
        "master" | "rgb" | "luma" => Ok(("master", &mut curve.master)),
        "red" => Ok(("red", &mut curve.red)),
        "green" => Ok(("green", &mut curve.green)),
        "blue" => Ok(("blue", &mut curve.blue)),
        other => Err(format!("unknown point-curve channel `{other}` (master, red, green or blue)")),
    }
}

fn edit_point(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "develop.curvePoint";
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let mut settings = (*s.develop_of(id).unwrap_or_default()).clone();
    let (channel, curve) = channel_mut(&mut settings.curve, str_param(p, "channel").unwrap_or("master")).map_err(|e| bad(C, e))?;
    let index =
        || p.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(C, "index must be a nonnegative integer"));
    let point = || {
        let number = |key| {
            p.get(key)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or_else(|| bad(C, format!("{key} must be a finite number in 0..1")))
        };
        Ok::<_, crate::EngineError>(Point::new(number("x")?, number("y")?))
    };
    let selected = match str_param(p, "action") {
        Some("add") => Some(curve.try_add_point(point()?).map_err(|e| bad(C, e.to_string()))?),
        Some("move") => Some(curve.try_move_point(index()?, point()?).map_err(|e| bad(C, e.to_string()))?),
        Some("remove") => {
            curve.try_remove_point(index()?).map_err(|e| bad(C, e.to_string()))?;
            None
        }
        _ => return Err(bad(C, "action must be add, move or remove")),
    };
    let result = json!({"channel":channel, "index":selected, "curve":curve});
    s.set_develop(id, settings, "Tone Curve")?;
    Ok(result)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "develop.curvePoint",
            "Edit Point Curve Handle",
            [],
            None,
            "{channel?: master|red|green|blue, action: add|move|remove, index?: nonnegative integer, x?, y?} — add/move use x,y in 0..1; move/remove use the editable handle index; preserves the curve's base function → {channel, index, curve}",
            has_active,
            edit_point
        ),
        cmd!(
            "curve.reset",
            "Reset Tone Curve",
            [],
            None,
            "{channel?: all|point|parametric|master|red|green|blue (default all)} — the active photo's tone curve (or one channel) back to linear",
            has_active,
            reset
        ),
        cmd!(
            query "curve.presets",
            "Point Curve Presets",
            [],
            None,
            "{} → {presets: [{name, master, red, green, blue: point pairs or tagged function, builtin?}], current: name of the preset the active photo's point curves match, or null}",
            always,
            |s, _| Ok(list(s))
        ),
        cmd!(
            "curve.applyPreset",
            "Apply Point Curve Preset",
            [],
            None,
            "{name} — sets the active photo's point curves (all four channels; the parametric curve stays)",
            has_active,
            apply_preset
        ),
        cmd!(
            "curve.savePreset",
            "Save Point Curve Preset",
            [],
            None,
            "{name} — the active photo's point curves under a name (replaces a user preset of that name; built-in names are reserved), saved with the library",
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
            "{path | paths: [file]} — .lccurve JSON files (v2 curves or legacy v1 presets envelope, a bare array, or one preset); same-named user presets are replaced → {imported: [name], failed: [[file, error]]}",
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
const FILE_FORMAT: &str = "lightcraft.curvePresets";

/// A named set of point curves (master + red/green/blue, points in 0..1; empty = linear).
/// Built-ins ship with the app; the user's are saved with the library preferences.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CurvePreset {
    pub name: String,
    #[serde(with = "preset_channel")]
    pub master: PointCurve,
    #[serde(with = "preset_channel")]
    pub red: PointCurve,
    #[serde(with = "preset_channel")]
    pub green: PointCurve,
    #[serde(with = "preset_channel")]
    pub blue: PointCurve,
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
    let points: Vec<Point> = out.iter().map(|p| Point::new(p[0], p[1])).collect();
    if out.len() < 2 || ToneCurve::point_curve_is_identity(&points) {
        out.clear();
    }
    out
}

fn from_points(v: &[Point]) -> Vec<[f64; 2]> {
    normalize(&v.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>())
}

/// Curve-preset files historically use pair arrays, while DevelopSettings uses Point objects.
/// Preserve that sparse wire shape; a function curve keeps its complete tagged model.
mod preset_channel {
    use super::*;

    pub fn serialize<S: serde::Serializer>(curve: &PointCurve, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match curve.sparse_points() {
            Some(points) => from_points(points).serialize(serializer),
            None => curve.serialize(serializer),
        }
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<PointCurve, D::Error> {
        struct Channel;
        impl<'de> serde::de::Visitor<'de> for Channel {
            type Value = PointCurve;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array of point pairs or a tagged curve function")
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error> {
                let mut pairs = Vec::new();
                while pairs.len() < lightcraft_develop::MAX_POINT_CURVE_POINTS {
                    match seq.next_element::<[f64; 2]>()? {
                        Some(pair) => pairs.push(pair),
                        None => return Ok(normalize(&pairs).into_iter().map(|p| Point::new(p[0], p[1])).collect::<Vec<_>>().into()),
                    }
                }
                struct Excess;
                impl<'de> Deserialize<'de> for Excess {
                    fn deserialize<D: serde::Deserializer<'de>>(_: D) -> std::result::Result<Self, D::Error> {
                        Err(serde::de::Error::custom("too many point-curve handles"))
                    }
                }
                let _ = seq.next_element::<Excess>()?;
                Ok(normalize(&pairs).into_iter().map(|p| Point::new(p[0], p[1])).collect::<Vec<_>>().into())
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> std::result::Result<Self::Value, A::Error> {
                PointCurve::deserialize(serde::de::value::MapAccessDeserializer::new(map))
            }
        }
        deserializer.deserialize_any(Channel)
    }
}

impl CurvePreset {
    /// The point curves of `c` under `name`.
    pub fn from_curve(name: &str, c: &ToneCurve) -> CurvePreset {
        CurvePreset { name: name.into(), master: c.master.clone(), red: c.red.clone(), green: c.green.clone(), blue: c.blue.clone(), builtin: false }
    }
    /// Set the four point curves of `c` (the parametric curve and Refine Saturation stay).
    pub fn apply(&self, c: &mut ToneCurve) {
        c.master = self.master.clone();
        c.red = self.red.clone();
        c.green = self.green.clone();
        c.blue = self.blue.clone();
    }
    /// Does `c` carry exactly this preset's point curves?
    pub fn matches(&self, c: &ToneCurve) -> bool {
        let same = |a: &PointCurve, b: &PointCurve| match (a.sparse_points(), b.sparse_points()) {
            (Some(a), Some(b)) => {
                let (a, b) = (from_points(a), from_points(b));
                a.len() == b.len() && a.iter().zip(&b).all(|(p, q)| (p[0] - q[0]).abs() < 1e-6 && (p[1] - q[1]).abs() < 1e-6)
            }
            _ => a == b,
        };
        same(&self.master, &c.master) && same(&self.red, &c.red) && same(&self.green, &c.green) && same(&self.blue, &c.blue)
    }
    fn is_linear(&self) -> bool {
        [&self.master, &self.red, &self.green, &self.blue].iter().all(|v| v.is_identity())
    }
}

/// The presets that ship with the app: a linear curve and two symmetric S-curves (our own
/// values: the quarter-tones pulled 0.05 / 0.11 towards the ends, midpoint fixed).
pub fn builtin_presets() -> Vec<CurvePreset> {
    let s = |name: &str, master: Vec<[f64; 2]>| CurvePreset {
        name: name.into(),
        master: master.into_iter().map(|p| Point::new(p[0], p[1])).collect::<Vec<_>>().into(),
        builtin: true,
        ..Default::default()
    };
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
    all_presets(s).into_iter().find(|p| p.matches(c)).map(|p| p.name)
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

fn apply_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "curve.applyPreset";
    let name = name_param(p, C)?;
    let preset =
        all_presets(s).into_iter().find(|x| x.name.eq_ignore_ascii_case(&name)).ok_or_else(|| bad(C, format!("no curve preset `{name}`")))?;
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    preset.apply(&mut d.curve);
    s.set_develop(id, d, &format!("Point Curve: {}", preset.name))?;
    Ok(json!({"name": preset.name}))
}

fn save_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "curve.savePreset";
    let name = name_param(p, C)?;
    if is_builtin_name(&name) {
        return Err(bad(C, format!("`{name}` is a built-in preset: choose another name")));
    }
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let preset = CurvePreset::from_curve(&name, &s.develop_of(id).unwrap_or_default().curve);
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

/// Presets in a curve-preset file: a v2 `curves` envelope, legacy v1 `presets`, a bare array or preset.
pub fn parse_file(bytes: &[u8]) -> std::result::Result<Vec<CurvePreset>, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Legacy {
        format: String,
        version: u32,
        presets: Vec<CurvePreset>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Current {
        format: String,
        version: u32,
        curves: Vec<CurvePreset>,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum File {
        Legacy(Legacy),
        Current(Current),
        Bare(Box<CurvePreset>),
        List(Vec<CurvePreset>),
    }
    let list = match serde_json::from_slice::<File>(bytes).map_err(|e| format!("not a curve preset file: {e}"))? {
        File::Legacy(file) => {
            if file.format != FILE_FORMAT || file.version != 1 {
                return Err("unsupported legacy curve-preset format or version".into());
            }
            file.presets
        }
        File::Current(file) => {
            if file.format != FILE_FORMAT || file.version != 2 {
                return Err("unsupported curve-preset format or version".into());
            }
            file.curves
        }
        File::Bare(preset) => vec![*preset],
        File::List(presets) => presets,
    };
    let list: Vec<CurvePreset> = list.into_iter().filter(|p| !p.name.trim().is_empty()).collect();
    if list.is_empty() {
        return Err("no curve presets in the file".into());
    }
    Ok(list)
}

/// The JSON of a curve-preset file holding `presets`.
pub fn to_file(presets: &[CurvePreset]) -> String {
    let presets: Vec<CurvePreset> = presets.iter().map(|p| CurvePreset { builtin: false, ..p.clone() }).collect();
    serde_json::to_string_pretty(&json!({"format": FILE_FORMAT, "version": 2, "curves": presets})).unwrap_or_default()
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

#[cfg(test)]
#[path = "curves_tests.rs"]
mod tests;
