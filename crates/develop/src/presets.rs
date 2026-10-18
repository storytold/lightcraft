//! Presets and copy/paste subsets.
//!
//! A preset is a *partial* settings object (JSON) — only the groups it includes. Each group a
//! preset covers is computed from the photo's *original* state (its import settings) plus the
//! preset's values, and that result replaces the photo's own group: presets stack on the original
//! image, not on the visible edits, so a second preset restores whatever an earlier one changed in
//! the groups it styles (issue #2159). `amount` (0..200 %) interpolates numeric values between the
//! original settings and the preset's. Copy/paste/sync use the same group selection
//! ([`SettingsGroup`]) but merge into the current settings as before.
//!
//! The rendering process ([`crate::ProcessVersion`]) is in no group: copy, paste, sync and presets
//! made from a photo never carry it, so the photos they change keep their own. A partial that names
//! `process` itself (Apply Settings JSON) sets it like any other field.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::borrow::Cow;

use crate::{DevelopSettings, WbScale};

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

/// Interpolate numbers in `patch` towards/away from `base` by `t` (1 = patch as-is); non-numeric
/// values are taken from the patch when `t >= 0.5`.
fn scale_patch(base: &Value, patch: &Value, t: f64) -> Value {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            let mut out = Map::new();
            for (k, pv) in p {
                let bv = b.get(k).unwrap_or(&Value::Null);
                out.insert(k.clone(), scale_patch(bv, pv, t));
            }
            Value::Object(out)
        }
        // Integer fields (the grain seed, ids) are discrete like strings: an interpolated seed would
        // not deserialize as an integer, and the whole preset would silently not apply.
        (Value::Number(b), Value::Number(p)) if b.is_f64() || p.is_f64() => {
            let (b, p) = (b.as_f64().unwrap_or(0.0), p.as_f64().unwrap_or(0.0));
            serde_json::Number::from_f64(b + (p - b) * t).map(Value::Number).unwrap_or(Value::Null)
        }
        (b, p) => {
            if t >= 0.5 {
                p.clone()
            } else {
                b.clone()
            }
        }
    }
}

/// Apply a partial settings object with an amount (1.0 = 100 %). Out-of-range values are clamped
/// through the control specs, as every merge does ([`DevelopSettings::merged`]).
pub fn apply_partial(s: &DevelopSettings, partial: &Value, amount: f64) -> DevelopSettings {
    let patch = if (amount - 1.0).abs() < 1e-9 { Cow::Borrowed(partial) } else { Cow::Owned(scale_patch(&s.to_json(), partial, amount)) };
    // (a white balance without a scale keeps its old meaning: `DevelopSettings::merged`)
    s.merged(&patch).unwrap_or_else(|_| s.clone())
}

/// Top-level settings keys that hold per-photo pixel repairs: a preset patches them into the
/// photo's own values as before (masks are added by [`Preset::apply_on`]) instead of restoring the
/// original's, and a preset without them leaves the photo's alone.
const REPAIR_KEYS: [&str; 3] = ["masks", "spots", "red_eye"];

/// The groups `settings` covers, computed on `original` (the photo's import state) with `amount`:
/// each covered group is `original`'s deep-merged with the preset's (numbers amount-scaled against
/// `original`), so the result is the complete look of the covered groups and can replace the
/// photo's own — applying a preset never inherits what an earlier preset left there (issue #2159).
pub fn compose_on_original(original: &DevelopSettings, settings: &Value, amount: f64) -> Value {
    let orig = original.to_json();
    let patch = if (amount - 1.0).abs() < 1e-9 { Cow::Borrowed(settings) } else { Cow::Owned(scale_patch(&orig, settings, amount)) };
    let mut out = Map::new();
    if let (Some(base), Some(p)) = (orig.as_object(), patch.as_object()) {
        for (k, v) in p {
            let mut group = base.get(k).cloned().unwrap_or_else(|| Value::Object(Map::new()));
            deep_merge(&mut group, v);
            remove_nulls(&mut group);
            out.insert(k.clone(), group);
        }
    }
    Value::Object(out)
}

/// Drop `null` entries, recursively: `scale_patch` writes them where the base lacks the key and
/// `amount` is below 50 %, and a single null would make the whole settings fail to deserialize —
/// which silently no-ops the preset.
fn remove_nulls(v: &mut Value) {
    if let Value::Object(o) = v {
        o.retain(|_, x| !x.is_null());
        for x in o.values_mut() {
            remove_nulls(x);
        }
    }
}

/// Merge `patch` into `s`, replacing each top-level group the patch names with the patch's (it is
/// that group's complete look, composed on the photo's original state); the pixel-repair keys
/// ([`REPAIR_KEYS`]) merge into the photo's own values instead, as they always have.
fn merge_groups(s: &DevelopSettings, patch: &Value) -> DevelopSettings {
    let mut v = s.to_json();
    if let (Some(obj), Some(p)) = (v.as_object_mut(), patch.as_object()) {
        for (k, group) in p {
            if REPAIR_KEYS.contains(&k.as_str())
                && let Some(cur) = obj.get_mut(k)
            {
                deep_merge(cur, group);
            } else {
                obj.insert(k.clone(), group.clone());
            }
        }
    }
    let mut out = DevelopSettings::from_json(&v).unwrap_or_else(|_| s.clone());
    // (a white balance without a scale keeps its old meaning: `DevelopSettings::merged`)
    if let Some(wb) = patch.get("wb").and_then(Value::as_object)
        && (wb.contains_key("temp") || wb.contains_key("tint"))
        && !wb.contains_key("scale")
    {
        out.wb.scale = WbScale::Legacy;
    }
    for c in crate::controls::CONTROLS {
        if let Some(v) = crate::controls::get(&out, c.id) {
            crate::controls::set(&mut out, c.id, v);
        }
    }
    out
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
    /// Apply at `amount` (1.0 = 100 %), treating `s` as the photo's original state (see
    /// [`Preset::apply_on`]): right for the import default preset and for scratch bases. Masks in
    /// the preset are added to the photo's own (ones it already has are skipped), their effect
    /// scaled by the amount.
    pub fn apply(&self, s: &DevelopSettings, amount: f64) -> DevelopSettings {
        self.apply_on(s, s, amount)
    }
    /// Apply at `amount` (1.0 = 100 %), composing on `original` (the photo's import state): each
    /// group the preset covers is computed from `original` plus the preset's values and replaces
    /// the group in `current`, so presets stack on the original image instead of the visible edits
    /// and a second preset restores what an earlier one changed in the groups it styles (issue
    /// #2159). Groups the preset doesn't name, and the photo's pixel repairs (masks, spots, red
    /// eye), keep `current`'s values; masks in the preset are added to the photo's own (ones it
    /// already has are skipped), their effect scaled by the amount.
    pub fn apply_on(&self, current: &DevelopSettings, original: &DevelopSettings, amount: f64) -> DevelopSettings {
        // A profile reference that didn't resolve (`crs:Look` kept as `profile.look` without an id;
        // the engine's profile_refs) means "the photo keeps its own profile": the preset doesn't
        // cover the profile group at all.
        let settings = match self.settings.get("profile").and_then(Value::as_object) {
            Some(p) if !p.contains_key("id") => {
                let mut rest = self.settings.clone();
                if let Some(o) = rest.as_object_mut() {
                    o.remove("profile");
                }
                Cow::Owned(rest)
            }
            _ => Cow::Borrowed(&self.settings),
        };
        let Some(masks) = settings.get("masks").and_then(Value::as_array).filter(|m| !m.is_empty()).map(|m| m.to_vec()) else {
            return merge_groups(current, &compose_on_original(original, &settings, amount));
        };
        let mut rest = settings.into_owned();
        if let Some(o) = rest.as_object_mut() {
            o.remove("masks");
        }
        let mut out = merge_groups(current, &compose_on_original(original, &rest, amount));
        add_masks(&mut out, &masks, amount);
        out
    }
}

/// The preset's masks, added to the photo's own (ones it already has are skipped), their effect
/// scaled by the amount.
fn add_masks(out: &mut DevelopSettings, masks: &[Value], amount: f64) {
    let mut next = out.masks.iter().map(|m| m.id).max().unwrap_or(0);
    for m in masks {
        let Ok(mut m) = serde_json::from_value::<crate::Mask>(m.clone()) else { continue };
        if out.masks.iter().any(|q| q.name == m.name && q.components == m.components) {
            continue;
        }
        next += 1;
        m.id = next;
        m.adjust.amount = (m.adjust.amount * amount).clamp(0.0, 200.0);
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

    /// Issue #2159: each group a preset covers is computed on the photo's original state and
    /// replaces the photo's own, so a preset cannot inherit what the visible edits (or an earlier
    /// preset) put there; groups it doesn't name keep the photo's values.
    #[test]
    fn apply_on_composes_each_group_on_the_original() {
        let mut current = DevelopSettings::default();
        current.light.exposure = 2.0;
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
        assert_eq!(out.light.exposure, 0.0, "the covered group comes from the original, not the visible edits");
        assert_eq!(out.light.contrast, 12.0);
        assert_eq!(out.effects.clarity, 30.0, "a group the preset doesn't name is untouched");
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
