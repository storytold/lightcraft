//! Presets and copy/paste subsets.
//!
//! A preset is a *partial* settings object (JSON) — only the groups it includes. Applying merges it
//! into the photo's settings; `amount` (0..200 %) blends between the current settings and the
//! preset's: sliders and point curves continuously, switches and choices by a fixed rule (the
//! preset's at any amount above 0, with the strength of a new choice fading in; see
//! `scale_patch`). Copy/paste/sync use the same group selection ([`SettingsGroup`]).
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
    let full = s.to_json();
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

/// Blend `patch` towards/away from `base` by `t` (0 = `base`, 1 = `patch` as-is, up to 2 = 200 %).
/// The rules, by kind of value (issue #2669: before them everything that wasn't a plain number
/// jumped from the photo's value to the preset's at exactly 50 %):
///
/// - **Continuous numbers** (floats: every slider) blend linearly, `base + (patch − base)·t`.
/// - **Point curves** (lists of `{x, y}` points, the tone curves; empty is the identity) blend as
///   curves: both are evaluated (as the renderer does, monotone cubic) at every control point of
///   either, and the blended points go through `base + (patch − base)·t`, with `y` kept in `0..1`.
/// - **Objects** blend field by field, with these rules.
/// - **Discrete values** — switches (`true`/`false`), choices (strings: the profile or LUT id,
///   enum values such as the treatment or the vignette style), integers (the grain seed) and other
///   lists (point colours, spots) — have no in-between: the preset's value is taken at any amount
///   above 0, and the photo's kept at 0. What they switch on or choose then appears gradually,
///   because its strength is a continuous number blended by the amount; see the next rule.
/// - **The `amount` beside a choice.** When the preset changes a choice (a string field) or the
///   `enabled` switch of an object, that object's `amount` blends from 0 rather than from the
///   photo's value: the photo's amount was the strength of its own choice. So a preset's profile
///   or LUT fades in from 0 % to the preset's amount (a profile at 0 % is no look at all).
/// - **Numbers on another scale.** White balance `temp` / `tint` on a different [`crate::WbScale`]
///   than the photo's (Kelvin against the older relative scale) are in other units and can't be
///   blended: they are taken whole at any amount above 0, with the scale. (A partial with `temp` or
///   `tint` and no `scale` is on the relative scale, as [`DevelopSettings::merged`] reads it.)
fn scale_patch(base: &Value, patch: &Value, t: f64) -> Value {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            let choice_changed = p.iter().any(|(k, pv)| {
                let changed = b.get(k).is_none_or(|bv| bv != pv);
                changed && (pv.is_string() || (k == "enabled" && pv.is_boolean()))
            });
            let other_scale = (p.contains_key("temp") || p.contains_key("tint"))
                && p.get("scale").and_then(Value::as_str).unwrap_or("legacy") != b.get("scale").and_then(Value::as_str).unwrap_or("legacy");
            let zero = Value::from(0.0);
            let mut out = Map::new();
            for (k, pv) in p {
                let bv = match b.get(k) {
                    Some(Value::Number(_)) if choice_changed && k == "amount" => &zero,
                    Some(bv) => bv,
                    None if choice_changed && k == "amount" && pv.is_number() => &zero,
                    None => &Value::Null,
                };
                let v = if other_scale && t > 0.0 && (k == "temp" || k == "tint") { pv.clone() } else { scale_patch(bv, pv, t) };
                out.insert(k.clone(), v);
            }
            Value::Object(out)
        }
        // Integer fields (the grain seed, ids) are discrete like strings: an interpolated seed would
        // not deserialize as an integer, and the whole preset would silently not apply.
        (Value::Number(b), Value::Number(p)) if b.is_f64() || p.is_f64() => {
            let (b, p) = (b.as_f64().unwrap_or(0.0), p.as_f64().unwrap_or(0.0));
            serde_json::Number::from_f64(b + (p - b) * t).map(Value::Number).unwrap_or(Value::Null)
        }
        (b, p) => match (point_curve(b), point_curve(p)) {
            (Some(bc), Some(pc)) if !(bc.is_empty() && pc.is_empty()) => blend_curves(&bc, &pc, t),
            _ => {
                if t > 0.0 {
                    p.clone()
                } else {
                    b.clone()
                }
            }
        },
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

/// Two point curves blended by `t` at the control points of both (see [`scale_patch`]).
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

/// Apply a partial settings object with an amount (1.0 = 100 %; blending rules: [`scale_patch`]).
/// Out-of-range values are clamped through the control specs, as every merge does
/// ([`DevelopSettings::merged`]).
/// At 0 % nothing changes; at 100 % the partial is merged as it is.
pub fn apply_partial(s: &DevelopSettings, partial: &Value, amount: f64) -> DevelopSettings {
    if amount <= 0.0 {
        return s.clone();
    }
    let patch = if (amount - 1.0).abs() < 1e-9 {
        Cow::Borrowed(partial)
    } else {
        // the base with every group written out, also the ones serialization leaves out while at
        // their defaults (HDR), so their numbers blend from the photo's values too
        let mut base = s.to_json();
        if let (Some(o), Ok(hdr)) = (base.as_object_mut(), serde_json::to_value(s.hdr)) {
            o.entry("hdr").or_insert(hdr);
        }
        Cow::Owned(scale_patch(&base, partial, amount))
    };
    // (a white balance without a scale keeps its old meaning: `DevelopSettings::merged`)
    s.merged(&patch).unwrap_or_else(|_| s.clone())
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
    /// Apply at `amount` (1.0 = 100 %). Masks in the preset are added to the photo's own (ones it
    /// already has are skipped), their effect scaled by the amount.
    ///
    /// This knows nothing about the photo's source. A preset that names only one white balance number (or a
    /// scale the photo is not on) needs the photo prepared against its as-shot white first, and the result put back
    /// on the scale the photo is edited on: callers on a real photo go through the engine's
    /// `apply_preset_to_photo`, which does that. A zero (or negative) `amount` returns `s` unchanged.
    pub fn apply(&self, s: &DevelopSettings, amount: f64) -> DevelopSettings {
        if amount <= 0.0 {
            return s.clone();
        }
        let Some(masks) = self.settings.get("masks").and_then(Value::as_array).filter(|m| !m.is_empty()) else {
            return apply_partial(s, &self.settings, amount);
        };
        let mut rest = self.settings.clone();
        if let Some(o) = rest.as_object_mut() {
            o.remove("masks");
        }
        let mut out = apply_partial(s, &rest, amount);
        for m in masks {
            let Ok(mut m) = serde_json::from_value::<crate::Mask>(m.clone()) else { continue };
            if out.masks.iter().any(|q| q.name == m.name && q.components == m.components) {
                continue;
            }
            m.id = out.next_mask_id();
            m.adjust.amount = (m.adjust.amount * amount).clamp(0.0, 200.0);
            out.masks.push(m);
        }
        out
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
    /// The seed is discrete: the preset's at any amount above 0 (the grain itself fades in).
    #[test]
    fn amount_keeps_integer_fields_whole() {
        let mut a = DevelopSettings::default();
        a.light.exposure = 1.0;
        a.grain.amount = 40.0;
        a.grain.seed = 12345;
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        assert!(p.settings.pointer("/grain/seed").is_some_and(Value::is_u64), "the default copy groups include the grain seed");
        let base = DevelopSettings::default();
        for (amount, seed) in [(0.0, 0), (0.3, 12345), (0.5, 12345), (1.5, 12345), (2.0, 12345)] {
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

        // a photo with its own curve (other points): at every control point of either curve the
        // result is the blend of the two curves as rendered
        let mut photo = DevelopSettings::default();
        photo.curve.master = pts(&[(0.0, 0.05), (0.5, 0.6), (1.0, 0.95)]);
        photo.curve.red = pts(&[(0.0, 0.0), (0.4, 0.5), (1.0, 1.0)]);
        let partial = json!({"curve": {"master": pts(&curve), "red": []}});
        let out = apply_partial(&photo, &partial, 0.4);
        for x in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let want = eval(&photo.curve.master, x) * 0.6 + eval(&pts(&curve), x) * 0.4;
            assert!((eval(&out.curve.master, x) - want).abs() < 1e-9, "x {x}");
        }
        // towards the identity (an empty curve) too
        assert!((eval(&out.curve.red, 0.4) - (0.5 * 0.6 + 0.4 * 0.4)).abs() < 1e-9);
    }

    /// The profile (and LUT) choice is the preset's at any amount above 0; its strength fades in
    /// from 0 %, so the look shows in proportion to the amount.
    #[test]
    fn amount_fades_in_a_new_profile() {
        let partial = json!({"profile": {"id": "lc.vivid", "amount": 150.0}});
        let base = DevelopSettings::default();
        for (t, id, amount) in [(0.0, "lc.color", 100.0), (0.3, "lc.vivid", 45.0), (0.5, "lc.vivid", 75.0), (1.0, "lc.vivid", 150.0)] {
            let out = apply_partial(&base, &partial, t);
            assert_eq!(out.profile.id, id, "{t}");
            assert!((out.profile.amount - amount).abs() < 1e-9, "{t}: amount {}", out.profile.amount);
        }
        // the same profile: its amount blends from the photo's
        let mut photo = DevelopSettings::default();
        photo.profile.id = "lc.vivid".into();
        photo.profile.amount = 60.0;
        let out = apply_partial(&photo, &partial, 0.5);
        assert!((out.profile.amount - 105.0).abs() < 1e-9, "{}", out.profile.amount);
    }

    /// Switches and choices take the preset's value at any amount above 0; an `enabled` switch or
    /// a style the preset changes brings its amount in from 0.
    #[test]
    fn amount_applies_switches_and_choices_above_zero() {
        let mut photo = DevelopSettings::default();
        photo.lens_blur.amount = 50.0;
        photo.optics.lens_profile = true;
        let style = serde_json::to_value(crate::VignetteStyle::ColorPriority).unwrap();
        let partial = json!({
            "lens_blur": {"enabled": true, "amount": 80.0},
            "treatment": "bw",
            "optics": {"lens_profile": false},
            "vignette": {"style": style, "amount": -40.0},
        });
        let out = apply_partial(&photo, &partial, 0.25);
        assert!(out.lens_blur.enabled);
        assert!((out.lens_blur.amount - 20.0).abs() < 1e-9, "{}", out.lens_blur.amount);
        assert_eq!(out.treatment, crate::Treatment::Bw);
        assert!(!out.optics.lens_profile);
        assert_eq!(out.vignette.style, crate::VignetteStyle::ColorPriority);
        assert!((out.vignette.amount + 10.0).abs() < 1e-9, "{}", out.vignette.amount);
        assert_eq!(apply_partial(&photo, &partial, 0.0), photo, "0 % changes nothing");
    }

    /// HDR is left out of the JSON while at its defaults; its numbers still blend from the photo's.
    #[test]
    fn amount_blends_groups_left_out_at_their_defaults() {
        let partial = json!({"hdr": {"enabled": true, "sdr_contrast": 40.0}});
        let out = apply_partial(&DevelopSettings::default(), &partial, 0.5);
        assert!(out.hdr.enabled);
        assert!((out.hdr.sdr_contrast - 20.0).abs() < 1e-9, "{}", out.hdr.sdr_contrast);
    }

    /// White balance on another scale (Kelvin against the older relative one) is in other units:
    /// taken whole with its scale, never a blend of the two.
    #[test]
    fn amount_takes_white_balance_on_another_scale_whole() {
        let kelvin = json!({"wb": {"mode": "custom", "temp": 4200.0, "tint": 8.0, "scale": "kelvin"}});
        let photo = DevelopSettings::default(); // relative scale, 6500 / 0 = as shot
        for t in [0.01, 0.3, 0.7] {
            let out = apply_partial(&photo, &kelvin, t);
            assert_eq!((out.wb.scale, out.wb.temp, out.wb.tint), (crate::WbScale::Kelvin, 4200.0, 8.0), "{t}");
        }
        let wb = crate::WhiteBalance { mode: crate::WbMode::Custom, temp: 5000.0, tint: 0.0, scale: crate::WbScale::Kelvin };
        let photo = DevelopSettings { wb, ..DevelopSettings::default() };
        // a partial without a scale is on the relative scale
        let out = apply_partial(&photo, &json!({"wb": {"temp": 7000.0, "tint": 10.0}}), 0.3);
        assert_eq!((out.wb.scale, out.wb.temp, out.wb.tint), (crate::WbScale::Legacy, 7000.0, 10.0));
        // the same scale blends
        let out = apply_partial(&photo, &kelvin, 0.5);
        assert_eq!((out.wb.scale, out.wb.temp, out.wb.tint), (crate::WbScale::Kelvin, 4600.0, 4.0));
    }

    #[test]
    fn zero_amount_mask_preset_leaves_settings_unchanged_and_can_be_applied_later() {
        let photo = DevelopSettings { masks: vec![crate::Mask { id: 9, name: "Existing".into(), ..Default::default() }], ..Default::default() };
        let p = Preset {
            id: "p".into(),
            name: "P".into(),
            group: "User".into(),
            settings: json!({"light": {"exposure": 1.0}, "masks": [{"name": "New", "adjust": {"amount": 80.0}}]}),
            favorite: false,
            builtin: false,
        };
        for amount in [0.0, -0.5] {
            let unchanged = p.apply(&photo, amount);
            assert_eq!(unchanged, photo);
            let out = p.apply(&unchanged, 1.0);
            assert_eq!(out.light.exposure, 1.0);
            assert_eq!(out.masks.len(), 2);
            assert_eq!(out.masks[0], photo.masks[0]);
            assert_eq!(out.masks[1].name, "New");
            assert_eq!(out.masks[1].adjust.amount, 80.0);
        }
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

    /// No setting of any kind jumps between amounts above 0 (#2669: everything that wasn't a
    /// slider switched at 50 %): from 1 % to 200 % the switches, choices and lists stay as at 1 %,
    /// and the numbers move in proportion to the amount.
    #[test]
    fn amount_has_no_step_above_zero() {
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
        a.lens_blur = crate::LensBlur { enabled: true, amount: 60.0, focal_distance: 30.0, focal_range: 20.0 };
        a.optics.remove_ca = true;
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        let mut photo = DevelopSettings::default();
        photo.curve.master = pts(&[(0.0, 0.1), (0.5, 0.45), (1.0, 1.0)]);
        photo.light.exposure = -0.4;
        let mut first = Vec::new();
        non_numbers(&p.apply(&photo, 0.01).to_json(), "", &mut first);
        let y0 = eval(&photo.curve.master, 0.3);
        for i in 2..=200 {
            let t = f64::from(i) / 100.0;
            let out = p.apply(&photo, t);
            if i != 100 {
                // (at exactly 100 % the preset is merged as it is: its own curve points)
                let mut leaves = Vec::new();
                non_numbers(&out.to_json(), "", &mut leaves);
                assert_eq!(leaves, first, "{t}: a switch, choice or list differs from 1 %");
            }
            let want = (-0.4 + 1.2 * t).min(5.0);
            assert!((out.light.exposure - want).abs() < 1e-9, "{t}: exposure {}", out.light.exposure);
            assert!((out.profile.amount - (140.0 * t).min(200.0)).abs() < 1e-9, "{t}: profile amount");
            assert!((out.lens_blur.amount - 60.0 * t).abs() < 1e-9, "{t}: lens blur amount");
            let y = eval(&out.curve.master, 0.3);
            let want = (y0 + (0.2 - y0) * t).clamp(0.0, 1.0);
            assert!((y - want).abs() < 1e-9, "{t}: curve y(0.3) {y}, want {want}");
        }
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
}
