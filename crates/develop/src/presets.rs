//! Presets and copy/paste subsets.
//!
//! A preset is a *partial* settings object (JSON) — only the groups it includes. Applying merges it
//! into the photo's settings; `amount` (0..200 %) interpolates numeric values between the current
//! settings and the preset's. Copy/paste/sync use the same group selection ([`SettingsGroup`]).
//!
//! The rendering process ([`crate::ProcessVersion`]) is in no group: copy, paste, sync and presets
//! made from a photo never carry it, so the photos they change keep their own. A partial that names
//! `process` itself (Apply Settings JSON) sets it like any other field.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{DevelopSettings, PointCurve, PointCurveError, Profile};
use std::fmt;

#[derive(Debug)]
pub enum PresetError {
    InvalidAmount,
    InvalidVersion,
    Settings(serde_json::Error),
    Curve(PointCurveError),
    MaskIdExhausted,
}
impl fmt::Display for PresetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAmount => f.write_str("preset amount must be finite"),
            Self::InvalidVersion => f.write_str("unsupported develop settings version in preset"),
            Self::Settings(e) => e.fmt(f),
            Self::Curve(e) => e.fmt(f),
            Self::MaskIdExhausted => f.write_str("the photo has no free mask identifiers"),
        }
    }
}
impl std::error::Error for PresetError {}
impl From<serde_json::Error> for PresetError {
    fn from(value: serde_json::Error) -> Self {
        Self::Settings(value)
    }
}
impl From<PointCurveError> for PresetError {
    fn from(value: PointCurveError) -> Self {
        Self::Curve(value)
    }
}
type Result<T> = std::result::Result<T, PresetError>;

fn function_wire(value: &Value) -> bool {
    value.get("kind").and_then(Value::as_str) == Some("function")
}

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
    let mut version = 0;
    if out.contains_key("wb") && s.wb.scale == crate::settings::WbScale::Kelvin {
        version = crate::settings::WB_SCALE_SCHEMA_VERSION;
    }
    if out.get("profile").and_then(|profile| profile.get("imported")).is_some_and(|look| !look.is_null()) {
        let profile_version = if s.profile.imported.as_ref().is_some_and(|look| look.look_table.is_some()) {
            crate::settings::LOOK_TABLE_SCHEMA_VERSION
        } else if s.profile.imported.as_ref().is_some_and(|look| look.convert_to_grayscale) {
            crate::settings::PROFILE_TREATMENT_SCHEMA_VERSION
        } else {
            crate::settings::IMPORTED_LOOK_SCHEMA_VERSION
        };
        version = version.max(profile_version);
    }
    if out.get("curve").is_some_and(|curve| ["master", "red", "green", "blue"].iter().any(|key| curve.get(*key).is_some_and(function_wire))) {
        version = version.max(crate::settings::POINT_CURVE_SCHEMA_VERSION);
    }
    if version > 0 {
        out.insert("version".into(), Value::from(version));
    }
    Value::Object(out)
}

/// Recursively merge `patch` into `base` (objects merge; everything else replaces).
pub fn deep_merge(base: &mut Value, patch: &Value) {
    deep_merge_inner(base, patch, true);
}

// A profile's imported definition is one checked value. A newly selected id cannot inherit the
// previous profile's definition, while changing only Amount keeps it verbatim.
fn merge_profile(base: &mut Value, patch: &Value) {
    if let (Some(b), Some(p)) = (base.as_object_mut(), patch.as_object()) {
        if let Some(id) = p.get("id")
            && !p.contains_key("imported")
            && (id.as_str() != Some(crate::IMPORTED_LOOK_ID) || b.get("id") != Some(id))
        {
            b.remove("imported");
        }
        for (key, value) in p {
            b.insert(key.clone(), value.clone());
        }
    } else {
        *base = patch.clone();
    }
}

fn deep_merge_inner(base: &mut Value, patch: &Value, root: bool) {
    // A function is one checked model, not a partial settings group. Inheriting omitted model
    // fields from an old function would silently accept an incomplete replacement.
    if function_wire(base) || function_wire(patch) {
        *base = patch.clone();
        return;
    }
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(bv) if root && k == "profile" => merge_profile(bv, v),
                    Some(bv) if bv.is_object() && v.is_object() => deep_merge_inner(bv, v, false),
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
    // Tone-curve channels are handled as typed functions in try_apply_partial. Never recursively
    // scale a tagged function should one occur in another partial payload.
    if function_wire(base) || function_wire(patch) {
        return if t >= 0.5 { patch.clone() } else { base.clone() };
    }
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            let mut out = Map::new();
            for (k, pv) in p {
                let bv = b.get(k).unwrap_or(&Value::Null);
                out.insert(k.clone(), if k == "version" { pv.clone() } else { scale_patch(bv, pv, t) });
            }
            Value::Object(out)
        }
        // Integer fields (grain seed, identifiers) remain discrete: fractional JSON would
        // fail their integer deserialization and reject the entire otherwise valid preset.
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
/// through the control specs.
pub fn apply_partial(s: &DevelopSettings, partial: &Value, amount: f64) -> DevelopSettings {
    try_apply_partial(s, partial, amount).unwrap_or_else(|_| s.clone())
}

/// Checked counterpart for command/import paths: invalid curve models remain an actionable error.
pub fn try_apply_partial(s: &DevelopSettings, partial: &Value, amount: f64) -> Result<DevelopSettings> {
    if !amount.is_finite() {
        return Err(PresetError::InvalidAmount);
    }
    if amount == 0.0 {
        return Ok(s.clone());
    }
    if partial.get("version").is_some_and(|v| v.as_u64().is_none_or(|v| v > u64::from(crate::settings::SCHEMA_VERSION))) {
        return Err(PresetError::InvalidVersion);
    }
    let base = serde_json::to_value(s)?;
    let mut rest = partial.clone();
    let profile = if let Some(patch) = rest.as_object_mut().and_then(|rest| rest.remove("profile")) {
        let mut target = serde_json::to_value(&s.profile)?;
        merge_profile(&mut target, &patch);
        // Validate even below the selection threshold, so a malformed definition is never
        // accepted merely because this particular Amount would leave it inactive.
        let target: Profile = serde_json::from_value(target)?;
        let mut chosen = if amount >= 0.5 { target.clone() } else { s.profile.clone() };
        if amount != 1.0 && patch.get("amount").is_some() {
            chosen.amount = s.profile.amount + (target.amount - s.profile.amount) * amount;
            if !chosen.amount.is_finite() {
                return Err(PresetError::InvalidAmount);
            }
        }
        Some(chosen)
    } else {
        None
    };
    let mut channels: [Option<PointCurve>; 4] = [None, None, None, None];
    if let Some(curve) = rest.get_mut("curve").and_then(Value::as_object_mut) {
        for ((name, source), slot) in [("master", &s.curve.master), ("red", &s.curve.red), ("green", &s.curve.green), ("blue", &s.curve.blue)]
            .into_iter()
            .zip(channels.iter_mut())
        {
            if let Some(wire) = curve.remove(name) {
                let target: PointCurve = serde_json::from_value(wire)?;
                *slot = Some(source.blend(&target, amount)?);
            }
        }
    }
    let patch = if amount == 1.0 { rest } else { scale_patch(&base, &rest, amount) };
    let mut out = s.merged(&patch)?;
    if let Some(profile) = profile {
        out.profile = profile;
    }
    for (destination, value) in [&mut out.curve.master, &mut out.curve.red, &mut out.curve.green, &mut out.curve.blue].into_iter().zip(channels) {
        if let Some(value) = value {
            *destination = value;
        }
    }
    for c in crate::controls::CONTROLS {
        if let Some(v) = crate::controls::get(&out, c.id) {
            crate::controls::set(&mut out, c.id, v);
        }
    }
    out.normalize_schema();
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub group: String,
    /// Partial settings.
    pub settings: Value,
    /// Names of saved settings that this preset could not carry over (never values/payloads).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "crate::unsupported_edits::deserialize_names",
        serialize_with = "crate::unsupported_edits::serialize_names"
    )]
    pub unsupported_edits: Vec<String>,
    #[serde(default)]
    pub favorite: bool,
    /// Built-in presets ship with the app and can't be deleted.
    #[serde(default)]
    pub builtin: bool,
}

impl Preset {
    pub fn from_settings(id: &str, name: &str, group: &str, s: &DevelopSettings, groups: &[SettingsGroup]) -> Preset {
        Preset {
            id: id.into(),
            name: name.into(),
            group: group.into(),
            settings: extract_groups(s, groups),
            unsupported_edits: Vec::new(),
            favorite: false,
            builtin: false,
        }
    }
    /// A preset is a partial edit: unsupported source settings remain relevant after applying it.
    pub fn merged_unsupported_edits(&self, existing: &[String]) -> Vec<String> {
        crate::unsupported_edits::bounded_names(existing.iter().chain(&self.unsupported_edits).map(String::as_str))
    }
    /// Apply at `amount` (1.0 = 100 %). Masks in the preset are added to the photo's own (ones it
    /// already has are skipped), their effect scaled by the amount.
    pub fn apply(&self, s: &DevelopSettings, amount: f64) -> DevelopSettings {
        self.try_apply(s, amount).unwrap_or_else(|_| s.clone())
    }
    /// Apply atomically, preserving errors from curve validation and malformed preset settings.
    pub fn try_apply(&self, s: &DevelopSettings, amount: f64) -> Result<DevelopSettings> {
        if amount == 0.0 {
            return Ok(s.clone());
        }
        let Some(masks) = self.settings.get("masks").and_then(Value::as_array).filter(|m| !m.is_empty()) else {
            return try_apply_partial(s, &self.settings, amount);
        };
        let mut rest = self.settings.clone();
        if let Some(o) = rest.as_object_mut() {
            o.remove("masks");
        }
        let mut out = try_apply_partial(s, &rest, amount)?;
        let mut next = out.masks.iter().map(|m| m.id).max().unwrap_or(0);
        for m in masks {
            let mut m = serde_json::from_value::<crate::Mask>(m.clone())?;
            if out.masks.iter().any(|q| q.name == m.name && q.components == m.components) {
                continue;
            }
            next = next.checked_add(1).ok_or(PresetError::MaskIdExhausted)?;
            m.id = next;
            m.adjust.amount = (m.adjust.amount * amount).clamp(0.0, 200.0);
            out.masks.push(m);
        }
        Ok(out)
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

        a.profile.id = crate::IMPORTED_LOOK_ID.into();
        a.profile.imported = Some(crate::ImportedLook {
            name: "Synthetic look".into(),
            source_id: "test-look".into(),
            contrast: 20.0,
            master: curve(&[(0.0, 0.1), (1.0, 0.9)]),
            ..Default::default()
        });
        let profile = extract_groups(&a, &[SettingsGroup::Profile]);
        assert_eq!(profile["version"], crate::IMPORTED_LOOK_SCHEMA_VERSION);
        assert!(profile.get("light").is_none());
        let applied = try_apply_partial(&b, &profile, 1.0).unwrap();
        assert_eq!(applied.profile, a.profile);
        assert_eq!(applied.light, b.light);
        assert_eq!(applied.curve, b.curve);
        assert_eq!(applied.version, crate::IMPORTED_LOOK_SCHEMA_VERSION);
        a.profile.imported.as_mut().unwrap().convert_to_grayscale = true;
        a.profile.amount = 0.0;
        let profile = extract_groups(&a, &[SettingsGroup::Profile]);
        assert_eq!(profile["version"], crate::PROFILE_TREATMENT_SCHEMA_VERSION);
        assert!(profile.get("treatment").is_none());
        let applied = try_apply_partial(&b, &profile, 1.0).unwrap();
        assert_eq!(applied.treatment, b.treatment);
        assert!(applied.is_bw(), "profile-only copy retains monochrome at Amount zero");
        let native = serde_json::to_string(&applied).unwrap();
        assert!(serde_json::from_str::<DevelopSettings>(&native).unwrap().is_bw());
        assert!(!native.contains("look_table"), "absent references keep the legacy wire shape");
        let reference: crate::LookTableRef = "e96cb8af6484f8402bb0e6ecfc8ad0fc".parse().unwrap();
        a.profile.imported.as_mut().unwrap().look_table = Some(reference.clone());
        let partial = extract_groups(&a, &[SettingsGroup::Profile]);
        assert_eq!(partial["version"], crate::LOOK_TABLE_SCHEMA_VERSION);
        let applied = try_apply_partial(&b, &partial, 1.0).unwrap();
        assert_eq!(applied.profile.imported_look().unwrap().look_table, Some(reference));
        assert_eq!(serde_json::from_value::<DevelopSettings>(applied.to_json()).unwrap(), applied);
        let amount = applied.merged(&json!({"profile":{"amount":150.0}})).unwrap();
        assert_eq!(amount.profile.imported, applied.profile.imported);
        assert!(applied.merged(&json!({"profile":{"id":"lc.color"}})).unwrap().profile.imported.is_none());
        for invalid in ["", "abc", "E96CB8AF6484F8402BB0E6ECFC8AD0F_"] {
            assert!(serde_json::from_value::<crate::LookTableRef>(json!(invalid)).is_err());
        }
        a.wb.scale = crate::WbScale::Kelvin;
        let wb = extract_groups(&a, &[SettingsGroup::WhiteBalance]);
        assert_eq!(wb["version"], crate::WB_SCALE_SCHEMA_VERSION);
        assert_eq!(wb["wb"]["scale"], "kelvin");
        assert_eq!(try_apply_partial(&b, &wb, 1.0).unwrap().wb, a.wb);
        let combined = extract_groups(&a, &[SettingsGroup::Profile, SettingsGroup::WhiteBalance]);
        assert_eq!(combined["version"], crate::WB_SCALE_SCHEMA_VERSION, "selected groups use their maximum required schema");
        assert_eq!(extract_groups(&a, &[SettingsGroup::Profile])["version"], crate::LOOK_TABLE_SCHEMA_VERSION);
        assert!(extract_groups(&a, &[SettingsGroup::Light]).get("version").is_none());
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

        let mut base = base;
        base.profile.id = crate::IMPORTED_LOOK_ID.into();
        base.profile.imported = Some(crate::ImportedLook { contrast: 20.0, name: "First".into(), ..Default::default() });
        let target = json!({"profile":{"id":crate::IMPORTED_LOOK_ID,"amount":160.0,"imported":{"name":"Second","clarity":30.0}}});
        for amount in [0.3, 0.5, 1.0, 2.0] {
            let out = try_apply_partial(&base, &target, amount).unwrap();
            let look = out.profile.imported_look().unwrap();
            assert_eq!(look.name, if amount < 0.5 { "First" } else { "Second" });
            assert_eq!(look.contrast, if amount < 0.5 { 20.0 } else { 0.0 });
            assert_eq!(look.clarity, if amount < 0.5 { 0.0 } else { 30.0 });
            assert_eq!(out.profile.amount, (100.0 + 60.0 * amount).min(200.0));
            assert_eq!(out.light, base.light);
        }
        let builtin = json!({"profile":{"id":"lc.vivid"}});
        assert_eq!(try_apply_partial(&base, &builtin, 0.3).unwrap().profile, base.profile);
        let switched = try_apply_partial(&base, &builtin, 0.5).unwrap();
        assert_eq!(switched.profile.id, "lc.vivid");
        assert!(switched.profile.imported.is_none());
        let amount_only = try_apply_partial(&base, &json!({"profile":{"amount":60.0}}), 0.5).unwrap();
        assert_eq!(amount_only.profile.imported, base.profile.imported);
        assert_eq!(amount_only.profile.amount, 80.0);
        for bad in [json!({"id":crate::IMPORTED_LOOK_ID}), json!({"id":crate::IMPORTED_LOOK_ID,"imported":{"clarity":101.0}})] {
            assert!(try_apply_partial(&DevelopSettings::default(), &json!({"profile":bad}), 0.3).is_err());
        }
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

    #[test]
    fn deep_merge_objects() {
        let mut a = json!({"x": {"a": 1, "b": 2}, "y": [1]});
        deep_merge(&mut a, &json!({"x": {"b": 3}, "y": [2, 3]}));
        assert_eq!(a, json!({"x": {"a": 1, "b": 3}, "y": [2, 3]}));
    }
    fn curve(points: &[(f64, f64)]) -> PointCurve {
        points.iter().map(|&(x, y)| lightcraft_geom::Point::new(x, y)).collect::<Vec<_>>().into()
    }

    #[test]
    fn all_curve_channels_blend_continuously_and_omitted_channels_stay_exact() {
        let mut base = DevelopSettings::default();
        let a = curve(&[(0.0, 0.0), (0.27, 0.1), (0.68, 0.85), (1.0, 1.0)]);
        let b = curve(&[(0.0, 0.12), (0.51, 0.74), (1.0, 0.95)]);
        base.curve.master = a.clone();
        base.curve.red = a.clone();
        base.curve.green = a.clone();
        base.curve.blue = a.clone();
        let mut target = base.clone();
        target.curve.master = b.clone();
        target.curve.red = b.clone();
        target.curve.green = b.clone();
        target.curve.blue = b.clone();
        target.curve.highlights = 40.0;
        let partial = extract_groups(&target, &[SettingsGroup::ToneCurve]);
        assert_eq!(try_apply_partial(&base, &partial, 0.0).unwrap(), base);
        assert_eq!(try_apply_partial(&base, &partial, 1.0).unwrap().curve, target.curve);
        for amount in [0.49999, 0.5, 0.50001, 0.9999, 1.5] {
            let result = try_apply_partial(&base, &partial, amount).unwrap();
            assert!((result.curve.highlights - 40.0 * amount).abs() < 1e-10);
            for channel in [&result.curve.master, &result.curve.red, &result.curve.green, &result.curve.blue] {
                assert!(channel.is_function());
                for i in 0..=128 {
                    let x = i as f64 / 128.0;
                    let expected = a.eval(x).unwrap() + amount * (b.eval(x).unwrap() - a.eval(x).unwrap());
                    assert!((channel.eval(x).unwrap() - expected).abs() < 1e-11);
                }
            }
        }
        let only_master = json!({"curve":{"master":serde_json::to_value(&b).unwrap()}});
        let changed = try_apply_partial(&base, &only_master, 0.5).unwrap();
        assert_eq!(changed.curve.red, base.curve.red);
        assert_eq!(changed.curve.green, base.curve.green);
        assert_eq!(changed.curve.blue, base.curve.blue);
        let reset = json!({"curve":{"master":[]}});
        assert!(try_apply_partial(&base, &reset, 1.0).unwrap().curve.master.is_identity());
    }

    #[test]
    fn function_presets_are_typed_and_version_markers_are_not_interpolated() {
        let mut base = DevelopSettings::default();
        let a = curve(&[(0.0, 0.0), (0.3, 0.2), (1.0, 1.0)]);
        let b = curve(&[(0.0, 0.1), (0.7, 0.9), (1.0, 0.97)]);
        base.curve.master = a.blend(&b, 0.3).unwrap();
        let mut target = base.clone();
        target.curve.master = a.blend(&b, 0.8).unwrap();
        let partial = extract_groups(&target, &[SettingsGroup::ToneCurve]);
        assert_eq!(partial["version"], crate::settings::POINT_CURVE_SCHEMA_VERSION);
        assert!(extract_groups(&target, &[SettingsGroup::Light]).get("version").is_none());
        target.wb.scale = crate::WbScale::Kelvin;
        assert_eq!(extract_groups(&target, &[SettingsGroup::ToneCurve])["version"], crate::POINT_CURVE_SCHEMA_VERSION);
        let combined = extract_groups(&target, &[SettingsGroup::ToneCurve, SettingsGroup::WhiteBalance]);
        assert_eq!(combined["version"], crate::WB_SCALE_SCHEMA_VERSION);
        let result = try_apply_partial(&base, &partial, 0.4).unwrap();
        assert_eq!(result.version, crate::settings::POINT_CURVE_SCHEMA_VERSION);
        for i in 0..=128 {
            let x = i as f64 / 128.0;
            let av = base.curve.master.eval(x).unwrap();
            let bv = target.curve.master.eval(x).unwrap();
            assert!((result.curve.master.eval(x).unwrap() - (av + 0.4 * (bv - av))).abs() < 1e-11);
        }
        let roundtrip = DevelopSettings::from_json(&result.to_json()).unwrap();
        assert_eq!(roundtrip, result);
        assert_eq!(roundtrip.hash64(), result.hash64());
        assert_eq!(scale_patch(&json!({"version":1}), &json!({"version":2}), 0.3), json!({"version":2}));
        let before = base.clone();
        assert!(try_apply_partial(&base, &json!({"version":999}), 0.5).is_err());
        assert!(try_apply_partial(&base, &partial, f64::NAN).is_err());
        assert_eq!(base, before);
    }

    #[test]
    fn malformed_function_replacements_and_masks_return_errors_without_partial_edits() {
        let mut base = DevelopSettings::default();
        base.curve.master = PointCurve::default().blend(&curve(&[(0.0, 0.1), (1.0, 0.9)]), 0.5).unwrap();
        let malformed = json!({"light":{"exposure":2.0},"curve":{"master":{"kind":"function","version":1}}});
        let before = base.clone();
        assert!(try_apply_partial(&base, &malformed, 0.3).is_err());
        assert!(base.merged(&malformed).is_err());
        assert_eq!(try_apply_partial(&base, &malformed, 0.0).unwrap(), base);
        assert_eq!(base, before);
        base.masks.push(crate::Mask { id: u32::MAX, name: "existing".into(), ..Default::default() });
        let mut preset = Preset::from_settings("p", "P", "User", &DevelopSettings::default(), &[]);
        preset.settings = json!({"masks":[{"name":"new"}]});
        assert!(matches!(preset.try_apply(&base, 1.0), Err(PresetError::MaskIdExhausted)));
        assert_eq!(base.masks.len(), 1);
    }
}
