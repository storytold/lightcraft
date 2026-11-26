//! Presets and copy/paste subsets.
//!
//! A preset is a *partial* settings object (JSON) — only the settings it names. Applying it changes
//! exactly the settings it names (a leaf: a number, a flag, a name; an array is one leaf): each is
//! computed from the photo's *original* state (its import settings, when known) and the preset's
//! value, and replaces the photo's value; every other setting keeps the photo's value (issue
//! #2159). `amount` (0..200 %) moves each named number between the original and the preset's
//! value; a point curve (a tone curve) moves between the two curves as rendered. Switches and choices
//! (a profile or LUT id, a style, a flag) take the original's value below half amount and the
//! preset's from half amount up, and the strength of a choice the preset changes fades in from 0
//! there; white balance on another scale is taken whole (see [`interpolate_object`]).
//! Copy/paste/sync use the same group selection ([`SettingsGroup`]) and merge into the current
//! settings as before.
//!
//! The rendering process ([`crate::ProcessVersion`]) is in no group: copy, paste, sync and presets
//! made from a photo never carry it, so the photos they change keep their own. A partial that names
//! `process` itself (Apply Settings JSON) sets it like any other field.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::borrow::Cow;

use crate::DevelopSettings;

/// Groups shown in "Choose settings to copy" / "Create preset" (Lightroom's checklist).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsGroup {
    Profile,
    Treatment,
    WhiteBalance,
    Light,
    ToneCurve,
    Color,
    ColorMixer,
    ColorGrading,
    Effects,
    Vignette,
    Grain,
    Detail,
    Optics,
    Geometry,
    Crop,
    Masks,
    Spots,
    RedEye,
    LensBlur,
    Calibration,
}

impl SettingsGroup {
    pub const ALL: [SettingsGroup; 20] = [
        SettingsGroup::Profile,
        SettingsGroup::Treatment,
        SettingsGroup::WhiteBalance,
        SettingsGroup::Light,
        SettingsGroup::ToneCurve,
        SettingsGroup::Color,
        SettingsGroup::ColorMixer,
        SettingsGroup::ColorGrading,
        SettingsGroup::Effects,
        SettingsGroup::Vignette,
        SettingsGroup::Grain,
        SettingsGroup::Detail,
        SettingsGroup::Optics,
        SettingsGroup::Geometry,
        SettingsGroup::Crop,
        SettingsGroup::Masks,
        SettingsGroup::Spots,
        SettingsGroup::RedEye,
        SettingsGroup::LensBlur,
        SettingsGroup::Calibration,
    ];

    /// Groups included by default when copying (Lightroom excludes crop, masks and spots by default).
    pub fn default_copy() -> Vec<SettingsGroup> {
        SettingsGroup::ALL
            .into_iter()
            .filter(|g| !matches!(g, SettingsGroup::Crop | SettingsGroup::Masks | SettingsGroup::Spots | SettingsGroup::RedEye))
            .collect()
    }

    pub fn label(self) -> &'static str {
        match self {
            SettingsGroup::Profile => "Profile",
            SettingsGroup::Treatment => "Treatment",
            SettingsGroup::WhiteBalance => "White Balance",
            SettingsGroup::Light => "Light",
            SettingsGroup::ToneCurve => "Tone Curve",
            SettingsGroup::Color => "Color",
            SettingsGroup::ColorMixer => "Color Mixer",
            SettingsGroup::ColorGrading => "Color Grading",
            SettingsGroup::Effects => "Effects",
            SettingsGroup::Vignette => "Vignette",
            SettingsGroup::Grain => "Grain",
            SettingsGroup::Detail => "Detail",
            SettingsGroup::Optics => "Optics",
            SettingsGroup::Geometry => "Geometry",
            SettingsGroup::Crop => "Crop",
            SettingsGroup::Masks => "Masking",
            SettingsGroup::Spots => "Remove",
            SettingsGroup::RedEye => "Red Eye",
            SettingsGroup::LensBlur => "Lens Blur",
            SettingsGroup::Calibration => "Calibration",
        }
    }

    /// Top-level JSON keys of `DevelopSettings` that belong to the group.
    pub fn keys(self) -> &'static [&'static str] {
        match self {
            SettingsGroup::Profile => &["profile"],
            SettingsGroup::Treatment => &["treatment"],
            SettingsGroup::WhiteBalance => &["wb"],
            SettingsGroup::Light => &["light", "hdr"],
            SettingsGroup::ToneCurve => &["curve"],
            SettingsGroup::Color => &["color"],
            SettingsGroup::ColorMixer => &["mixer", "bw_mix", "point_colors"],
            SettingsGroup::ColorGrading => &["grading"],
            SettingsGroup::Effects => &["effects"],
            SettingsGroup::Vignette => &["vignette"],
            SettingsGroup::Grain => &["grain"],
            SettingsGroup::Detail => &["detail", "enhance"],
            SettingsGroup::Optics => &["optics"],
            SettingsGroup::Geometry => &["geometry"],
            SettingsGroup::Crop => &["crop", "orientation"],
            SettingsGroup::Masks => &["masks"],
            SettingsGroup::Spots => &["spots"],
            SettingsGroup::RedEye => &["red_eye"],
            SettingsGroup::LensBlur => &["lens_blur"],
            SettingsGroup::Calibration => &["calibration"],
        }
    }
}

/// The subset of `s` covering `groups`, as a partial JSON object.
pub fn extract_groups(s: &DevelopSettings, groups: &[SettingsGroup]) -> Value {
    let full = s.to_json_for_merge();
    let mut out = Map::new();
    for g in groups {
        for k in g.keys() {
            if let Some(v) = full.get(*k) {
                out.insert((*k).to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

/// Recursively merge `patch` into `base` (objects merge; everything else replaces).
pub fn deep_merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(bv) if bv.is_object() && v.is_object() => deep_merge(bv, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// Apply a partial settings object with an amount (1.0 = 100 %): each setting it names is blended
/// from `s`'s value by the same rules as a preset ([`interpolate_object`]). Out-of-range values are
/// clamped through the control specs, as every merge does ([`DevelopSettings::merged`]). At 0 %
/// (or less) nothing changes; at 100 % the partial is merged as it is.
pub fn apply_partial(s: &DevelopSettings, partial: &Value, amount: f64) -> DevelopSettings {
    if amount.is_nan() || amount <= 0.0 {
        return s.clone();
    }
    let patch = if (amount - 1.0).abs() < 1e-9 {
        Cow::Borrowed(partial)
    } else {
        let base = blend_base(s);
        Cow::Owned(Value::Object(partial.as_object().map(|p| interpolate_object(base.as_object(), p, amount)).unwrap_or_default()))
    };
    // (a white balance without a scale keeps its old meaning: `DevelopSettings::merged`)
    s.merged(&patch).unwrap_or_else(|_| s.clone())
}

/// `s` as the base an amount blends from: every group written out, also the ones serialization
/// leaves out while at their defaults (HDR), so their numbers blend from `s`'s values too.
fn blend_base(s: &DevelopSettings) -> Value {
    let mut v = s.to_json_for_merge();
    if let (Some(o), Ok(hdr)) = (v.as_object_mut(), serde_json::to_value(s.hdr)) {
        o.entry("hdr").or_insert(hdr);
    }
    v
}

/// Top-level settings keys that hold per-photo pixel repairs: a preset's named repairs compose on
/// the photo's own values instead of the original's (masks are added by [`Preset::apply_on`]), and
/// a preset without them leaves the photo's alone.
const REPAIR_KEYS: [&str; 3] = ["masks", "spots", "red_eye"];

/// The settings `settings` (a partial JSON object) names, applied to `current` at `amount`: each
/// named setting is computed from `original` (the photo's import state) and the preset's value,
/// and replaces the photo's value (issue #2159). Every setting the preset does not name keeps the
/// photo's current value, edits included. Pixel repairs compose on the photo's own values, as they
/// always have.
fn compose_named(current: &DevelopSettings, original: &DevelopSettings, settings: &Value, amount: f64) -> DevelopSettings {
    // At 100 % the preset's own values apply as they are, and `original` is not read. The presets
    // panel calls this for every visible row on every frame, so the scaling base is built below only.
    if (amount - 1.0).abs() < 1e-9 {
        return current.merged(settings).unwrap_or_else(|_| current.clone());
    }
    let cur = current.to_json();
    let mut base = blend_base(original);
    if let (Some(b), Some(c)) = (base.as_object_mut(), cur.as_object()) {
        for k in REPAIR_KEYS {
            if let Some(v) = c.get(k) {
                b.insert(k.to_string(), v.clone());
            }
        }
    }
    let composed = match settings.as_object() {
        Some(patch) => Value::Object(interpolate_object(base.as_object(), patch, amount)),
        None => Value::Object(Map::new()),
    };
    // `merged` deep-merges: the named settings replace, everything else stays as `current` has it
    current.merged(&composed).unwrap_or_else(|_| current.clone())
}

/// The leaves of `patch` that apply at `t` (below 100 %), each computed from the same place in
/// `base` ([`interpolate_leaf`]). A leaf the amount does not reach is left out, so the photo keeps
/// its own value there. Two rules look at an object as a whole (issue #2669):
///
/// - **The `amount` beside a choice.** When the preset changes a choice here (a string such as the
///   profile or LUT id or a style, or the object's `enabled` switch), its `amount` is the strength of
///   that choice, not of the base's: from half amount up, where the preset's choice applies, the
///   amount fades in from 0 (a new profile at 50 % has half the preset's strength, and a profile at
///   0 % is no look at all); below half amount the base's choice keeps its own amount.
/// - **White balance on another scale.** `temp` / `tint` on a different [`crate::WbScale`] than the
///   base's (Kelvin against the older relative scale; a partial that names no scale is on the
///   relative one, as [`DevelopSettings::merged`] reads it) are in other units and are never
///   blended: below half amount the base's numbers and scale apply, from half amount up the
///   preset's, as for any other choice.
fn interpolate_object(base: Option<&Map<String, Value>>, patch: &Map<String, Value>, t: f64) -> Map<String, Value> {
    let get = |k: &str| base.and_then(|b| b.get(k));
    let choice_changed = patch.iter().any(|(k, pv)| (pv.is_string() || (k == "enabled" && pv.is_boolean())) && get(k) != Some(pv));
    let scale_of = |o: Option<&Map<String, Value>>| o.and_then(|o| o.get("scale")).and_then(Value::as_str).unwrap_or("legacy").to_string();
    let other_scale = (patch.contains_key("temp") || patch.contains_key("tint")) && scale_of(Some(patch)) != scale_of(base);
    let mut out = Map::new();
    for (k, pv) in patch {
        if other_scale && matches!(k.as_str(), "temp" | "tint" | "scale") {
            continue;
        }
        let bv = get(k);
        let v = match pv {
            Value::Number(_) if choice_changed && k == "amount" => {
                if t >= 0.5 {
                    interpolate_leaf(Some(&Value::from(0.0)), pv, t)
                } else {
                    bv.cloned()
                }
            }
            Value::Object(p) => Some(Value::Object(interpolate_object(bv.and_then(Value::as_object), p, t))),
            _ => interpolate_leaf(bv, pv, t),
        };
        if let Some(v) = v {
            out.insert(k.clone(), v);
        }
    }
    if other_scale {
        // the numbers and their scale come together from one side, never a mix of the two
        let side = if t >= 0.5 { Some(patch) } else { base };
        for k in ["temp", "tint"] {
            if let Some(v) = patch.get(k).and(side.and_then(|o| o.get(k))) {
                out.insert(k.to_string(), v.clone());
            }
        }
        out.insert("scale".to_string(), Value::from(scale_of(side)));
    }
    out
}

/// One leaf at `t`. Numbers move from `base` towards `patch`. Point curves (lists of `{x, y}`
/// points, the tone curves; empty is the identity) move as curves: both are evaluated as the
/// renderer does (monotone cubic) at every control point of either, and the points blend like
/// numbers, `y` kept in `0..1`. Other values switch to the preset's at half amount and up, and
/// below it take the base's value. A null in the base is the baseline value the import state
/// recorded (a cleared option, such as a free crop aspect): below half amount it applies and
/// clears the photo's value, as the amount contract promises. A null the preset writes is a value
/// like any other, so it applies at half amount and up (a preset can clear an option field such as
/// a crop aspect).
fn interpolate_leaf(base: Option<&Value>, patch: &Value, t: f64) -> Option<Value> {
    if t > 0.0
        && let (Some(bc), Some(pc)) = (base.and_then(point_curve), point_curve(patch))
        && !(bc.is_empty() && pc.is_empty())
    {
        return Some(blend_curves(&bc, &pc, t));
    }
    match (base, patch) {
        // integer fields (the grain seed, ids) are discrete like strings: an interpolated seed would
        // not deserialize as an integer, and the whole preset would silently not apply
        (Some(Value::Number(b)), Value::Number(p)) if b.is_f64() || p.is_f64() => {
            let (b, p) = (b.as_f64().unwrap_or(0.0), p.as_f64().unwrap_or(0.0));
            serde_json::Number::from_f64(b + (p - b) * t).map(Value::Number)
        }
        _ if t >= 0.5 => Some(patch.clone()),
        _ => base.cloned(),
    }
}

/// `v` as a point curve: a list whose items all are `{x, y}` numbers (empty: the identity).
fn point_curve(v: &Value) -> Option<Vec<(f64, f64)>> {
    v.as_array()?
        .iter()
        .map(|q| {
            let o = q.as_object().filter(|o| o.len() == 2)?;
            Some((o.get("x")?.as_f64()?, o.get("y")?.as_f64()?))
        })
        .collect()
}

/// Two point curves blended by `t` at the control points of both (see [`interpolate_leaf`]).
fn blend_curves(base: &[(f64, f64)], patch: &[(f64, f64)], t: f64) -> Value {
    use lightcraft_color::spline::MonotoneCurve;
    let (bc, pc) = (MonotoneCurve::new(base), MonotoneCurve::new(patch));
    let mut xs: Vec<f64> = bc.points().into_iter().chain(pc.points()).map(|(x, _)| x.clamp(0.0, 1.0)).collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    let points = xs
        .into_iter()
        .map(|x| {
            let (yb, yp) = (bc.eval(x), pc.eval(x));
            serde_json::json!({"x": x, "y": (yb + (yp - yb) * t).clamp(0.0, 1.0)})
        })
        .collect();
    Value::Array(points)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub group: String,
    /// Partial settings.
    pub settings: Value,
    #[serde(default)]
    pub favorite: bool,
    /// Built-in presets ship with the app and can't be deleted.
    #[serde(default)]
    pub builtin: bool,
}

impl Preset {
    pub fn from_settings(id: &str, name: &str, group: &str, s: &DevelopSettings, groups: &[SettingsGroup]) -> Preset {
        Preset { id: id.into(), name: name.into(), group: group.into(), settings: extract_groups(s, groups), favorite: false, builtin: false }
    }
    /// Apply at `amount` (1.0 = 100 %), with `s` as both the photo's current and its original
    /// settings (see [`Preset::apply_on`]): right for the import default preset and for scratch
    /// bases. Masks in the preset are added to the photo's own (ones it already has are skipped),
    /// their effect scaled by the amount.
    ///
    /// This knows nothing about the photo's source. A preset that names only one white balance
    /// number needs the photo prepared against its as-shot white first, and the result put back on
    /// the scale the photo is edited on: callers on a real photo go through the engine's
    /// `apply_preset_to_photo`, which does that.
    pub fn apply(&self, s: &DevelopSettings, amount: f64) -> DevelopSettings {
        self.apply_on(s, s, amount)
    }
    /// Apply at `amount` (1.0 = 100 %), composing on `original` (the photo's import state, or the
    /// photo's current settings when that is not known): each setting the preset names is computed
    /// from `original` and the preset's value, `amount` moving it between the two, and replaces the
    /// photo's value. Every setting the preset does not name keeps `current`'s value, and so do
    /// the photo's pixel repairs (masks, spots, red eye), which the preset patches; masks in the
    /// preset are added to the photo's own (ones it already has are skipped), their effect scaled
    /// by the amount. A second preset therefore restores what the first one changed in the
    /// settings it names (issue #2159), and nothing it leaves out. At 0 % (or less) the settings it
    /// names take `original`'s values, so with `current` as the original nothing changes, and its
    /// masks are not added (a mask at no strength would only block applying the preset later); a
    /// NaN `amount` changes nothing.
    pub fn apply_on(&self, current: &DevelopSettings, original: &DevelopSettings, amount: f64) -> DevelopSettings {
        if amount.is_nan() {
            return current.clone();
        }
        let amount = amount.max(0.0);
        let Some(masks) = self.settings.get("masks").and_then(Value::as_array).filter(|m| !m.is_empty()) else {
            return compose_named(current, original, &self.settings, amount);
        };
        let mut rest = self.settings.clone();
        if let Some(o) = rest.as_object_mut() {
            o.remove("masks");
        }
        let mut out = compose_named(current, original, &rest, amount);
        if amount > 0.0 {
            add_masks(&mut out, masks, amount);
        }
        out
    }
}

/// The preset's masks, added to the photo's own (ones it already has are skipped), their effect
/// scaled by the amount.
fn add_masks(out: &mut DevelopSettings, masks: &[Value], amount: f64) {
    // the masks by name, kept up to date: a preset of many masks compares each with its
    // namesakes only, not with every mask added before it
    let mut by_name: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for (i, q) in out.masks.iter().enumerate() {
        by_name.entry(q.name.clone()).or_default().push(i);
    }
    let mut next_id = out.next_mask_id();
    let mut used: std::collections::HashSet<u32> = out.masks.iter().map(|q| q.id).collect();
    for m in masks {
        let Ok(mut m) = serde_json::from_value::<crate::Mask>(m.clone()) else { continue };
        if by_name.get(&m.name).is_some_and(|ix| ix.iter().any(|&i| out.masks.get(i).is_some_and(|q| q.components == m.components))) {
            continue;
        }
        // ids run on from the first free one, past any a mask already has (a scan for the
        // next free id per mask would be another pass over them)
        while used.contains(&next_id) {
            next_id = next_id.checked_add(1).unwrap_or(1);
        }
        m.id = next_id;
        used.insert(next_id);
        m.adjust.amount = (m.adjust.amount * amount).clamp(0.0, 200.0);
        by_name.entry(m.name.clone()).or_default().push(out.masks.len());
        out.masks.push(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_and_apply_roundtrip() {
        let mut a = DevelopSettings::default();
        a.light.exposure = 1.0;
        a.effects.clarity = 30.0;
        a.crop.geometry.angle = 5.0;
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        assert!(p.settings.get("crop").is_none());
        let b = p.apply(&DevelopSettings::default(), 1.0);
        assert_eq!(b.light.exposure, 1.0);
        assert_eq!(b.effects.clarity, 30.0);
        assert_eq!(b.crop.geometry.angle, 0.0);
    }

    #[test]
    fn amount_scales_numbers() {
        let base = DevelopSettings::default();
        let partial = json!({"light": {"exposure": 1.0, "contrast": 40}});
        let half = apply_partial(&base, &partial, 0.5);
        assert!((half.light.exposure - 0.5).abs() < 1e-9);
        assert!((half.light.contrast - 20.0).abs() < 1e-9);
        let more = apply_partial(&base, &partial, 2.0);
        assert!((more.light.contrast - 80.0).abs() < 1e-9);
        // clamped through specs
        let lots = apply_partial(&base, &json!({"light": {"exposure": 4.0}}), 2.0);
        assert_eq!(lots.light.exposure, 5.0);
    }

    /// A preset with the Grain group (its seed is an integer) used to do nothing at any amount
    /// other than 100 %: the interpolated seed failed to deserialize and the preset was dropped.
    #[test]
    fn amount_keeps_integer_fields_whole() {
        let mut a = DevelopSettings::default();
        a.light.exposure = 1.0;
        a.grain.amount = 40.0;
        a.grain.seed = 12345;
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        assert!(p.settings.pointer("/grain/seed").is_some_and(Value::is_u64), "the default copy groups include the grain seed");
        let base = DevelopSettings::default();
        for (amount, seed) in [(0.3, 0), (0.5, 12345), (1.5, 12345), (2.0, 12345)] {
            let out = p.apply(&base, amount);
            assert!((out.light.exposure - amount.min(5.0)).abs() < 1e-9, "{amount}: exposure {}", out.light.exposure);
            assert!((out.grain.amount - (40.0 * amount).min(100.0)).abs() < 1e-9, "{amount}: grain {}", out.grain.amount);
            assert_eq!(out.grain.seed, seed, "{amount}: seed");
        }
    }

    fn pts(v: &[(f64, f64)]) -> Vec<lightcraft_geom::Point> {
        v.iter().map(|&(x, y)| lightcraft_geom::Point::new(x, y)).collect()
    }

    fn eval(c: &[lightcraft_geom::Point], x: f64) -> f64 {
        lightcraft_color::spline::MonotoneCurve::new(&c.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()).eval(x)
    }

    fn preset(settings: Value) -> Preset {
        Preset { id: "p".into(), name: "P".into(), group: "G".into(), settings, favorite: false, builtin: false }
    }

    /// Issue #2669: a preset's tone curve did nothing below 50 % and all of it from 50 % up.
    #[test]
    fn amount_blends_point_curves() {
        let curve = [(0.0, 0.0), (0.25, 0.15), (0.75, 0.85), (1.0, 1.0)];
        let partial = json!({"curve": {"master": pts(&curve)}});
        let base = DevelopSettings::default();
        let mut last = 0.25;
        for i in 1..=10 {
            let t = f64::from(i) / 10.0;
            let out = apply_partial(&base, &partial, t);
            let y = eval(&out.curve.master, 0.25);
            assert!((y - (0.25 - 0.10 * t)).abs() < 1e-9, "{t}: y(0.25) = {y}");
            assert!(y < last + 1e-12, "{t}: the curve moves steadily towards the preset's");
            last = y;
        }
        assert!(apply_partial(&base, &partial, 0.0).curve.master.is_empty(), "0 % keeps the photo's curve");
        // 200 % goes twice as far, kept in 0..1
        let more = apply_partial(&base, &partial, 2.0);
        assert!((eval(&more.curve.master, 0.25) - 0.05).abs() < 1e-9);
        assert!(more.curve.master.iter().all(|p| (0.0..=1.0).contains(&p.y)));

        // a photo whose original has its own curves (other points): at every control point of either
        // curve the result is the blend of the original's curve and the preset's as rendered, whatever
        // the photo's current curve is
        let mut original = DevelopSettings::default();
        original.curve.master = pts(&[(0.0, 0.05), (0.5, 0.6), (1.0, 0.95)]);
        original.curve.red = pts(&[(0.0, 0.0), (0.4, 0.5), (1.0, 1.0)]);
        let mut current = DevelopSettings::default();
        current.curve.master = pts(&[(0.0, 0.2), (1.0, 0.8)]);
        let p = preset(json!({"curve": {"master": pts(&curve), "red": []}}));
        let out = p.apply_on(&current, &original, 0.4);
        for x in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let want = eval(&original.curve.master, x) * 0.6 + eval(&pts(&curve), x) * 0.4;
            assert!((eval(&out.curve.master, x) - want).abs() < 1e-9, "x {x}");
        }
        // towards the identity (an empty curve) too
        assert!((eval(&out.curve.red, 0.4) - (0.5 * 0.6 + 0.4 * 0.4)).abs() < 1e-9);
    }

    /// The profile (and LUT) choice steps to the preset's at half amount, as every choice does
    /// (#2207); from there its strength fades in from 0 %, so the look shows in proportion to the
    /// amount. The same profile blends its amount from the original's, not the photo's current one.
    #[test]
    fn amount_fades_in_a_new_profile() {
        let p = preset(json!({"profile": {"id": "lc.vivid", "amount": 150.0}}));
        let base = DevelopSettings::default();
        for (t, id, amount) in [(0.3, "lc.color", 100.0), (0.5, "lc.vivid", 75.0), (0.8, "lc.vivid", 120.0), (1.0, "lc.vivid", 150.0)] {
            let out = p.apply_on(&base, &base, t);
            assert_eq!(out.profile.id, id, "{t}");
            assert!((out.profile.amount - amount).abs() < 1e-9, "{t}: amount {}", out.profile.amount);
        }
        assert_eq!(p.apply_on(&base, &base, 0.0), base, "0 % changes nothing");
        // the same profile: its amount blends from the original's (60 → 150 at 50 % is 105), not
        // from the photo's current 20
        let mut original = DevelopSettings::default();
        original.profile.id = "lc.vivid".into();
        original.profile.amount = 60.0;
        let mut current = original.clone();
        current.profile.amount = 20.0;
        for t in [0.3, 0.5] {
            let out = p.apply_on(&current, &original, t);
            assert_eq!(out.profile.id, "lc.vivid");
            assert!((out.profile.amount - (60.0 + 90.0 * t)).abs() < 1e-9, "{t}: {}", out.profile.amount);
        }
    }

    /// Switches and choices keep the original's value below half amount and take the preset's from
    /// half amount up; an `enabled` switch or a style the preset changes brings its amount in from 0
    /// there, and below half amount leaves the original's amount as it is.
    #[test]
    fn amount_steps_switches_and_choices_at_half_and_fades_in_their_amount() {
        let mut original = DevelopSettings::default();
        original.lens_blur.amount = 50.0;
        original.optics.lens_profile = true;
        let style = serde_json::to_value(crate::VignetteStyle::ColorPriority).unwrap();
        let p = preset(json!({
            "lens_blur": {"enabled": true, "amount": 80.0},
            "treatment": "bw",
            "optics": {"lens_profile": false},
            "vignette": {"style": style, "amount": -40.0},
        }));
        let mut current = original.clone();
        current.light.exposure = 1.0;
        let low = p.apply_on(&current, &original, 0.25);
        assert!(!low.lens_blur.enabled);
        assert_eq!(low.lens_blur.amount, 50.0, "below half the original's own amount");
        assert_eq!(low.treatment, original.treatment);
        assert!(low.optics.lens_profile);
        assert_eq!((low.vignette.style, low.vignette.amount), (original.vignette.style, original.vignette.amount));
        assert_eq!(low.light.exposure, 1.0, "a setting the preset doesn't name keeps the photo's value");
        let half = p.apply_on(&current, &original, 0.5);
        assert!(half.lens_blur.enabled);
        assert!((half.lens_blur.amount - 40.0).abs() < 1e-9, "{}", half.lens_blur.amount);
        assert_eq!(half.treatment, crate::Treatment::Bw);
        assert!(!half.optics.lens_profile);
        assert_eq!(half.vignette.style, crate::VignetteStyle::ColorPriority);
        assert!((half.vignette.amount + 20.0).abs() < 1e-9, "{}", half.vignette.amount);
        assert_eq!(p.apply_on(&current, &original, 0.0), current, "0 % changes nothing");
    }

    /// HDR is left out of the JSON while at its defaults; its numbers still blend from the original's.
    #[test]
    fn amount_blends_groups_left_out_at_their_defaults() {
        let p = preset(json!({"hdr": {"enabled": true, "sdr_contrast": 40.0}}));
        let base = DevelopSettings::default();
        let out = p.apply_on(&base, &base, 0.5);
        assert!(out.hdr.enabled);
        assert!((out.hdr.sdr_contrast - 20.0).abs() < 1e-9, "{}", out.hdr.sdr_contrast);
        let out = apply_partial(&base, &p.settings, 0.3);
        assert!(!out.hdr.enabled);
        assert!((out.hdr.sdr_contrast - 12.0).abs() < 1e-9, "{}", out.hdr.sdr_contrast);
    }

    /// White balance on another scale (Kelvin against the older relative one) is in other units:
    /// the original's below half amount and the preset's from half amount up, each whole with its
    /// scale, never a blend of the two.
    #[test]
    fn amount_takes_white_balance_on_another_scale_whole() {
        let kelvin = preset(json!({"wb": {"mode": "custom", "temp": 4200.0, "tint": 8.0, "scale": "kelvin"}}));
        let photo = DevelopSettings::default(); // relative scale, 6500 / 0 = as shot
        for t in [0.01, 0.3, 0.49] {
            assert_eq!(kelvin.apply_on(&photo, &photo, t).wb, photo.wb, "{t}");
        }
        for t in [0.5, 0.7, 1.5] {
            let out = kelvin.apply_on(&photo, &photo, t);
            assert_eq!((out.wb.scale, out.wb.temp, out.wb.tint), (crate::WbScale::Kelvin, 4200.0, 8.0), "{t}");
        }
        let wb = crate::WhiteBalance { mode: crate::WbMode::Custom, temp: 5000.0, tint: 0.0, scale: crate::WbScale::Kelvin };
        let photo = DevelopSettings { wb, ..DevelopSettings::default() };
        // a partial without a scale is on the relative scale
        let relative = preset(json!({"wb": {"temp": 7000.0, "tint": 10.0}}));
        assert_eq!(relative.apply_on(&photo, &photo, 0.3).wb, photo.wb);
        let out = relative.apply_on(&photo, &photo, 0.6);
        assert_eq!((out.wb.scale, out.wb.temp, out.wb.tint), (crate::WbScale::Legacy, 7000.0, 10.0));
        // the same scale blends
        let out = kelvin.apply_on(&photo, &photo, 0.5);
        assert_eq!((out.wb.scale, out.wb.temp, out.wb.tint), (crate::WbScale::Kelvin, 4600.0, 4.0));
    }

    #[test]
    fn zero_amount_mask_preset_leaves_settings_unchanged_and_can_be_applied_later() {
        let photo = DevelopSettings { masks: vec![crate::Mask { id: 9, name: "Existing".into(), ..Default::default() }], ..Default::default() };
        let p = preset(json!({"light": {"exposure": 1.0}, "masks": [{"name": "New", "adjust": {"amount": 80.0}}]}));
        for amount in [0.0, -0.5, f64::NAN] {
            let unchanged = p.apply_on(&photo, &photo, amount);
            assert_eq!(unchanged, photo);
            let out = p.apply(&unchanged, 1.0);
            assert_eq!(out.light.exposure, 1.0);
            assert_eq!(out.masks.len(), 2);
            assert_eq!(out.masks[0], photo.masks[0]);
            assert_eq!(out.masks[1].name, "New");
            assert_eq!(out.masks[1].adjust.amount, 80.0);
        }
        // with an import state, 0 % is that state for the settings the preset names (#2207), and still
        // adds no mask
        let mut current = photo.clone();
        current.light.exposure = 2.0;
        current.curve.master = pts(&[(0.0, 0.1), (1.0, 0.9)]);
        let original = DevelopSettings { light: crate::Light { exposure: -0.5, ..Default::default() }, ..Default::default() };
        let q =
            preset(json!({"light": {"exposure": 1.0}, "curve": {"master": pts(&[(0.0, 0.0), (0.5, 0.7), (1.0, 1.0)])}, "masks": [{"name": "New"}]}));
        let out = q.apply_on(&current, &original, 0.0);
        assert_eq!(out.light.exposure, -0.5);
        assert!(out.curve.master.is_empty(), "the original's curve, point for point");
        assert_eq!(out.masks, current.masks);
    }

    /// Every leaf of `v` that is not a number (and every list's length), by path.
    fn non_numbers(v: &Value, path: &str, out: &mut Vec<(String, Value)>) {
        match v {
            Value::Object(o) => o.iter().for_each(|(k, v)| non_numbers(v, &format!("{path}/{k}"), out)),
            Value::Array(a) => {
                out.push((format!("{path}#len"), Value::from(a.len())));
                a.iter().enumerate().for_each(|(i, v)| non_numbers(v, &format!("{path}/{i}"), out));
            }
            Value::Number(_) => {}
            v => out.push((path.to_string(), v.clone())),
        }
    }

    /// Across the whole Amount range a preset has one step, at half amount (#2207): below it the
    /// switches, choices and lists stay as at 1 %, from it as at 50 %; the numbers and tone curves
    /// move in proportion to the amount throughout, and the strength of a changed choice fades in
    /// from 0 once the choice is the preset's.
    #[test]
    fn amount_has_one_step_at_half_and_blends_the_rest() {
        let mut a = DevelopSettings::default();
        a.profile.id = "lc.vivid".into();
        a.profile.amount = 140.0;
        a.treatment = crate::Treatment::Bw;
        a.light.exposure = 0.8;
        a.curve.master = pts(&[(0.0, 0.0), (0.3, 0.2), (0.7, 0.8), (1.0, 1.0)]);
        a.curve.green = pts(&[(0.0, 0.05), (1.0, 0.9)]);
        a.grain.amount = 30.0;
        a.grain.seed = 777;
        a.vignette.style = crate::VignetteStyle::ColorPriority;
        a.vignette.amount = -25.0;
        a.lens_blur = crate::LensBlur { enabled: true, amount: 60.0, focal_distance: 30.0, focal_range: 20.0, ..Default::default() };
        a.optics.remove_ca = true;
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        let mut original = DevelopSettings::default();
        original.curve.master = pts(&[(0.0, 0.1), (0.5, 0.45), (1.0, 1.0)]);
        original.light.exposure = -0.4;
        let mut current = original.clone();
        current.light.exposure = 2.0;
        let leaves = |t: f64| {
            let mut v = Vec::new();
            non_numbers(&p.apply_on(&current, &original, t).to_json(), "", &mut v);
            v
        };
        let (below, above) = (leaves(0.01), leaves(0.5));
        assert_ne!(below, above, "the choices step at half amount");
        let y0 = eval(&original.curve.master, 0.3);
        for i in 2..=200 {
            let t = f64::from(i) / 100.0;
            let out = p.apply_on(&current, &original, t);
            if i != 100 {
                // (at exactly 100 % the preset is merged as it is: its own curve points)
                assert_eq!(leaves(t), if t < 0.5 { below.clone() } else { above.clone() }, "{t}: a switch, choice or list moved off its side");
            }
            let want = (-0.4 + 1.2 * t).min(5.0);
            assert!((out.light.exposure - want).abs() < 1e-9, "{t}: exposure {}", out.light.exposure);
            let (profile, blur) =
                if t < 0.5 { (original.profile.amount, original.lens_blur.amount) } else { ((140.0 * t).min(200.0), (60.0 * t).min(100.0)) };
            assert!((out.profile.amount - profile).abs() < 1e-9, "{t}: profile amount {}", out.profile.amount);
            assert!((out.lens_blur.amount - blur).abs() < 1e-9, "{t}: lens blur amount {}", out.lens_blur.amount);
            let y = eval(&out.curve.master, 0.3);
            let want = (y0 + (0.2 - y0) * t).clamp(0.0, 1.0);
            assert!((y - want).abs() < 1e-9, "{t}: curve y(0.3) {y}, want {want}");
        }
    }

    /// A preset changes only the settings it names: a setting in a group it styles but does not
    /// name keeps the photo's value, and so does a whole group it leaves out.
    #[test]
    fn apply_on_changes_only_the_settings_it_names() {
        let mut current = DevelopSettings::default();
        current.light.exposure = 2.0;
        current.light.blacks = -30.0;
        current.effects.clarity = 30.0;
        let original = DevelopSettings::default();
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "G".into(),
            settings: json!({"light": {"contrast": 12.0}}),
            favorite: false,
            builtin: false,
        };
        let out = p.apply_on(&current, &original, 1.0);
        assert_eq!(out.light.contrast, 12.0);
        assert_eq!(out.light.exposure, 2.0, "a setting the preset doesn't name keeps the photo's value");
        assert_eq!(out.light.blacks, -30.0, "a setting in the same group, not named, keeps the photo's value");
        assert_eq!(out.effects.clarity, 30.0, "a group the preset doesn't name is untouched");
    }

    /// A partial preset that names one colour of the mixer leaves the other colours, and the
    /// unnamed settings of the colour it names, as the photo has them.
    #[test]
    fn apply_on_keeps_the_mixer_colours_it_does_not_name() {
        let mut current = DevelopSettings::default();
        current.mixer.red.sat = 20.0;
        current.mixer.blue.hue = 15.0;
        current.mixer.blue.sat = 5.0;
        let original = DevelopSettings::default();
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "G".into(),
            settings: json!({"mixer": {"blue": {"sat": 15.0, "lum": -25.0}}}),
            favorite: false,
            builtin: false,
        };
        let out = p.apply_on(&current, &original, 1.0);
        assert_eq!(out.mixer.red.sat, 20.0, "a colour the preset doesn't name");
        assert_eq!(out.mixer.blue.hue, 15.0, "a setting of a named colour that the preset doesn't name");
        assert_eq!((out.mixer.blue.sat, out.mixer.blue.lum), (15.0, -25.0));
    }

    /// The white balance is not one setting: a preset that names only the temperature keeps the
    /// photo's tint.
    #[test]
    fn apply_on_keeps_the_white_balance_fields_it_does_not_name() {
        let mut current = DevelopSettings::default();
        current.wb.temp = 6000.0;
        current.wb.tint = 12.0;
        let original = DevelopSettings::default();
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "G".into(),
            settings: json!({"wb": {"temp": 5000.0}}),
            favorite: false,
            builtin: false,
        };
        let out = p.apply_on(&current, &original, 1.0);
        assert_eq!((out.wb.temp, out.wb.tint), (5000.0, 12.0));
    }

    /// A preset's amount 0 changes nothing when the original is the photo's current settings (no
    /// import state is recorded for the photo), and any other amount moves the named settings
    /// from where the photo has them.
    #[test]
    fn apply_on_without_an_import_state_scales_from_the_current_settings() {
        let mut current = DevelopSettings::default();
        current.light.exposure = 2.0;
        current.light.blacks = -30.0;
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "G".into(),
            settings: json!({"light": {"exposure": 0.45, "contrast": 12.0}}),
            favorite: false,
            builtin: false,
        };
        assert_eq!(p.apply_on(&current, &current, 0.0), current, "amount 0 applies nothing");
        let half = p.apply_on(&current, &current, 0.5);
        assert!((half.light.exposure - 1.225).abs() < 1e-9, "{}", half.light.exposure);
        assert_eq!(half.light.blacks, -30.0);
    }

    /// An unresolved `crs:Look` reference (its profile object has no id; the importer leaves
    /// `look` in it) changes nothing: the photo keeps its own profile.
    #[test]
    fn an_unresolved_profile_reference_keeps_the_photos_profile() {
        let mut current = DevelopSettings::default();
        current.profile.id = "lc.vivid".into();
        current.profile.amount = 60.0;
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "G".into(),
            settings: json!({"profile": {"look": {"name": "Not imported", "amount": 100.0}}, "light": {"contrast": 12.0}}),
            favorite: false,
            builtin: false,
        };
        for amount in [1.0, 0.5] {
            let out = p.apply_on(&current, &current, amount);
            assert_eq!((out.profile.id.as_str(), out.profile.amount), ("lc.vivid", 60.0), "amount {amount}");
        }
    }

    /// The amount interpolates between the original and the preset, not between the visible
    /// values and the preset: a preset at 50 % looks the same however the photo was edited before.
    #[test]
    fn apply_on_scales_against_the_original() {
        let mut current = DevelopSettings::default();
        current.light.exposure = 2.0;
        let original = DevelopSettings::default();
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "G".into(),
            settings: json!({"light": {"exposure": 0.45}}),
            favorite: false,
            builtin: false,
        };
        let out = p.apply_on(&current, &original, 0.5);
        assert!((out.light.exposure - 0.225).abs() < 1e-9, "{}", out.light.exposure);
    }

    /// Pixel repairs are the photo's own: a preset's are patched in, and one without them never
    /// restores the original's.
    #[test]
    fn apply_on_patches_pixel_repairs_into_the_current_ones() {
        let current = DevelopSettings {
            spots: serde_json::from_value(json!([{"mode": "heal", "points": [[0.5, 0.5]], "size": 0.05, "feather": 50.0, "opacity": 100.0}]))
                .unwrap(),
            ..Default::default()
        };
        let original = DevelopSettings::default();
        let none = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "G".into(),
            settings: json!({"light": {"contrast": 10.0}}),
            favorite: false,
            builtin: false,
        };
        let out = none.apply_on(&current, &original, 1.0);
        assert_eq!(out.spots.len(), 1, "the photo's spot survives a preset without spots");
        let clearing = Preset {
            id: "q".into(),
            name: "Q".into(),
            group: "G".into(),
            settings: json!({"light": {"contrast": 10.0}, "spots": []}),
            favorite: false,
            builtin: false,
        };
        let out = clearing.apply_on(&current, &original, 1.0);
        assert_eq!(out.spots.len(), 0, "a preset that names spots replaces them, as before");
    }

    /// A null a preset writes into an option field (here a crop aspect: null is "free") is the
    /// preset's value, so it clears the photo's aspect from half amount up, as any value of the
    /// preset does; below half amount the import baseline applies, which here is null as well.
    #[test]
    fn a_preset_null_clears_an_option_field_from_half_amount() {
        let mut current = DevelopSettings::default();
        current.crop.aspect = Some((3, 2));
        let original = DevelopSettings::default(); // the import state has no aspect
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "G".into(),
            settings: json!({"crop": {"aspect": null}}),
            favorite: false,
            builtin: false,
        };
        for amount in [1.0, 0.7, 0.5] {
            assert_eq!(p.apply_on(&current, &original, amount).crop.aspect, None, "amount {amount}: the preset's null clears the aspect");
        }
        assert_eq!(p.apply_on(&current, &original, 0.3).crop.aspect, None, "below half amount the import baseline (no aspect) applies");
    }

    /// The import state has no aspect, and that null is the baseline below half amount (the amount
    /// contract: a named setting moves between the import state and the preset's value); from half
    /// amount the preset's aspect applies.
    #[test]
    fn a_null_in_the_original_is_the_baseline_below_half_amount() {
        let mut current = DevelopSettings::default();
        current.crop.aspect = Some((3, 2));
        let original = DevelopSettings::default();
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "G".into(),
            settings: json!({"crop": {"aspect": [16, 9]}}),
            favorite: false,
            builtin: false,
        };
        assert_eq!(p.apply_on(&current, &original, 0.4).crop.aspect, None, "40 %: the import baseline (no aspect) applies");
        assert_eq!(p.apply_on(&current, &original, 0.6).crop.aspect, Some((16, 9)), "60 %: the preset's aspect applies");
    }

    #[test]
    fn deep_merge_objects() {
        let mut a = json!({"x": {"a": 1, "b": 2}, "y": [1]});
        deep_merge(&mut a, &json!({"x": {"b": 3}, "y": [2, 3]}));
        assert_eq!(a, json!({"x": {"a": 1, "b": 3}, "y": [2, 3]}));
    }

    #[test]
    fn preset_masks_get_unused_ids_after_a_mask_id_at_the_limit() {
        let s = DevelopSettings { masks: vec![crate::Mask { id: u32::MAX, name: "Theirs".into(), ..Default::default() }], ..Default::default() };
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "User".into(),
            settings: json!({"masks": [{"id": 7, "name": "A"}, {"id": 7, "name": "B"}]}),
            favorite: false,
            builtin: false,
        };
        let out = p.apply(&s, 1.0);
        let mut ids: Vec<u32> = out.masks.iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), 3);
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 3, "{:?}", out.masks);
    }

    #[test]
    fn preset_masks_skip_ids_an_existing_mask_has_after_the_first_free_one() {
        // the first free id is 1, and 2 is taken: the second added mask must not get 2
        let theirs = |id| crate::Mask { id, name: format!("T{id}"), ..Default::default() };
        let s = DevelopSettings { masks: vec![theirs(u32::MAX), theirs(2)], ..Default::default() };
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "User".into(),
            settings: json!({"masks": [{"name": "A"}, {"name": "B"}]}),
            favorite: false,
            builtin: false,
        };
        let mut ids: Vec<u32> = p.apply(&s, 1.0).masks.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, [1, 2, 3, u32::MAX]);
    }
}
