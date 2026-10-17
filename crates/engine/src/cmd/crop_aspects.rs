//! The crop tool's aspect-ratio list (issue #1854): the built-in ratios, which of them the user
//! hid, and the ratios the user saved under a name (as many as they like, in their own order),
//! kept with the library preferences. Picking one is `crop.aspect` with its value.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::{Result, Session};

/// The built-in ratios of the aspect menu, in menu order: (label, `crop.aspect` value).
pub const BUILTIN_ASPECTS: [(&str, &str); 10] = [
    ("Free", "free"),
    ("Original", "original"),
    ("1 × 1", "1x1"),
    ("4 × 5 / 8 × 10", "4x5"),
    ("8.5 × 11", "8.5x11"),
    ("5 × 7", "5x7"),
    ("2 × 3 / 4 × 6", "2x3"),
    ("4 × 3", "4x3"),
    ("16 × 9", "16x9"),
    ("16 × 10", "16x10"),
];

/// The longest name a saved ratio may have, in characters.
pub const MAX_NAME_CHARS: usize = 64;

/// A ratio the user saved under a name (`[width, height]`, any positive numbers).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomAspect {
    pub name: String,
    pub aspect: [f64; 2],
}

/// The user's part of the aspect menu: saved ratios in menu order, hidden built-in keys.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CropAspects {
    pub custom: Vec<CustomAspect>,
    pub hidden: Vec<String>,
}

impl CropAspects {
    /// Only what the commands would have accepted: a hand-edited prefs.json can't add a ratio
    /// `crop.aspect` refuses, an empty, built-in or duplicate name, or hide Free / Original.
    pub fn sanitized(self) -> CropAspects {
        let mut custom: Vec<CustomAspect> = Vec::new();
        for a in self.custom {
            let name = a.name.trim();
            if check_name(name).is_ok() && valid_ratio(a.aspect[0], a.aspect[1]) && !custom.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
                custom.push(CustomAspect { name: name.to_string(), aspect: a.aspect });
            }
        }
        let mut hidden: Vec<String> = Vec::new();
        for key in self.hidden {
            if hideable(&key) && !hidden.contains(&key) {
                hidden.push(key);
            }
        }
        CropAspects { custom, hidden }
    }

    pub fn is_hidden(&self, key: &str) -> bool {
        self.hidden.iter().any(|k| k == key)
    }
}

/// A ratio `crop.aspect` takes as `[w, h]`: both positive and finite, at most 20:1 either way.
pub fn valid_ratio(w: f64, h: f64) -> bool {
    let r = w / h;
    w.is_finite()
        && h.is_finite()
        && w > 0.0
        && h > 0.0
        && w * 100.0 <= f64::from(u32::MAX)
        && h * 100.0 <= f64::from(u32::MAX)
        && (1.0 / lightcraft_geom::MAX_RATIO..=lightcraft_geom::MAX_RATIO).contains(&r)
}

/// A built-in ratio the menu may leave out (every one but Free and Original).
fn hideable(key: &str) -> bool {
    key != "free" && key != "original" && BUILTIN_ASPECTS.iter().any(|(_, k)| *k == key)
}

fn is_builtin_name(name: &str) -> bool {
    BUILTIN_ASPECTS.iter().any(|(label, key)| label.eq_ignore_ascii_case(name) || key.eq_ignore_ascii_case(name))
}

fn check_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("missing `name`".into());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("the name is longer than {MAX_NAME_CHARS} characters"));
    }
    if is_builtin_name(name) {
        return Err(format!("`{name}` is a built-in ratio: choose another name"));
    }
    Ok(())
}

fn name_param(p: &Value, c: &str) -> Result<String> {
    str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).ok_or_else(|| bad(c, "missing `name`"))
}

/// `[w, h]` or `"WxH"`.
fn ratio_param(p: &Value, c: &str) -> Result<[f64; 2]> {
    let (w, h) = match p.get("aspect") {
        Some(Value::Array(a)) if a.len() == 2 => match (a.first().and_then(Value::as_f64), a.get(1).and_then(Value::as_f64)) {
            (Some(w), Some(h)) => (w, h),
            _ => return Err(bad(c, "use [w, h] with two numbers")),
        },
        Some(Value::String(s)) => {
            let (w, h) = s.split_once(['x', 'X', ':']).ok_or_else(|| bad(c, "use [w, h] or \"WxH\""))?;
            match (w.trim().parse::<f64>(), h.trim().parse::<f64>()) {
                (Ok(w), Ok(h)) => (w, h),
                _ => return Err(bad(c, "use [w, h] or \"WxH\"")),
            }
        }
        Some(_) => return Err(bad(c, "use [w, h] or \"WxH\"")),
        None => return Err(bad(c, "missing `aspect`")),
    };
    if !valid_ratio(w, h) {
        return Err(bad(c, "aspect needs two positive numbers with a ratio between 1:20 and 20:1"));
    }
    Ok([w, h])
}

fn position(s: &Session, name: &str) -> Option<usize> {
    s.crop_aspects.custom.iter().position(|a| a.name.eq_ignore_ascii_case(name))
}

/// Built-ins first (each with `hidden`), then the saved ones, in menu order.
fn list(s: &Session) -> Value {
    let mut v: Vec<Value> = BUILTIN_ASPECTS
        .iter()
        .map(|(label, key)| json!({"name": label, "aspect": key, "builtin": true, "hidden": s.crop_aspects.is_hidden(key)}))
        .collect();
    v.extend(s.crop_aspects.custom.iter().map(|a| json!({"name": a.name, "aspect": a.aspect, "builtin": false, "hidden": false})));
    json!({"aspects": v})
}

fn save(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "crop.saveAspect";
    let name = name_param(p, C)?;
    check_name(&name).map_err(|e| bad(C, e))?;
    let aspect = ratio_param(p, C)?;
    let entry = CustomAspect { name, aspect };
    match position(s, &entry.name).and_then(|i| s.crop_aspects.custom.get_mut(i)) {
        Some(a) => *a = entry,
        None => s.crop_aspects.custom.push(entry),
    }
    s.save_prefs()?;
    Ok(list(s))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "crop.deleteAspect";
    let name = name_param(p, C)?;
    if is_builtin_name(&name) {
        return Err(bad(C, "built-in ratios can't be deleted (crop.hideAspect hides them)"));
    }
    let i = position(s, &name).ok_or_else(|| bad(C, format!("no saved ratio `{name}`")))?;
    s.crop_aspects.custom.remove(i);
    s.save_prefs()?;
    Ok(list(s))
}

fn move_to(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "crop.moveAspect";
    let name = name_param(p, C)?;
    let to = p.get("to").and_then(Value::as_u64).ok_or_else(|| bad(C, "missing `to` (a position from 0)"))?;
    let from = position(s, &name).ok_or_else(|| bad(C, format!("no saved ratio `{name}`")))?;
    let a = s.crop_aspects.custom.remove(from);
    let to = usize::try_from(to).unwrap_or(usize::MAX).min(s.crop_aspects.custom.len());
    s.crop_aspects.custom.insert(to, a);
    s.save_prefs()?;
    Ok(list(s))
}

fn hide(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "crop.hideAspect";
    let key = str_param(p, "aspect").ok_or_else(|| bad(C, "missing `aspect` (a built-in ratio's key, like \"4x5\")"))?;
    if !hideable(key) {
        return Err(bad(C, format!("`{key}` is not a built-in ratio that can be hidden (Free and Original always show)")));
    }
    let hidden = p.get("hidden").and_then(Value::as_bool).unwrap_or(true);
    s.crop_aspects.hidden.retain(|k| k != key);
    if hidden {
        s.crop_aspects.hidden.push(key.to_string());
    }
    s.save_prefs()?;
    Ok(list(s))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "crop.aspects",
            "Crop Aspect Ratios",
            [],
            None,
            "{} → {aspects: [{name, aspect: built-in key (\"4x5\") or [w, h], builtin, hidden}]}: the aspect menu, built-ins first, then the saved ratios in order",
            always,
            |s, _| Ok(list(s))
        ),
        cmd!(
            "crop.saveAspect",
            "Save Crop Aspect Ratio",
            [],
            None,
            "{name, aspect: [w, h] | \"WxH\"} — adds a ratio to the aspect menu under a name (any number of them; the same name replaces it in place), saved with the library",
            always,
            save
        ),
        cmd!("crop.deleteAspect", "Delete Crop Aspect Ratio", [], None, "{name} — removes a saved ratio from the aspect menu", always, delete),
        cmd!(
            "crop.moveAspect",
            "Move Crop Aspect Ratio",
            [],
            None,
            "{name, to} — moves a saved ratio to position `to` (0 = first) among the saved ones",
            always,
            move_to
        ),
        cmd!(
            "crop.hideAspect",
            "Hide Crop Aspect Ratio",
            [],
            None,
            "{aspect: built-in key (\"4x5\"), hidden?: bool (default true)} — leaves a built-in ratio out of the aspect menu, or shows it again (Free and Original always show)",
            always,
            hide
        ),
    ]
}
