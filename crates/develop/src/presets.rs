//! Presets and copy/paste subsets.
//!
//! A preset is a *partial* settings object (JSON) — only the settings it names. Applying it changes
//! exactly the settings it names; every other setting keeps the photo's value. `amount`
//! (0..200 %) blends each named setting from its baseline (the photo's import look when one is
//! recorded, else its current value; [`Preset::apply_on`]) towards the preset's: sliders and point
//! curves continuously, switches and choices by a fixed rule (the preset's at any amount above 0,
//! with the strength of a new choice fading in; see `scale_patch`). Copy/paste/sync use the same
//! group selection ([`SettingsGroup`]) and blend from the current settings ([`apply_partial`]).
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
    /// Levels (beyond Lightroom): black, midtone, white and output levels per channel.
    Levels,
    ToneCurve,
    Color,
    ColorMixer,
    ColorGrading,
    /// The colourist primaries (the Witchcraft grading mode's wheels, beyond Lightroom).
    Primaries,
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
    PluginFilters,
    /// Retouch: Smooth Skin's sliders and which skin it works on, and the other retouch sections
    /// (Teeth) with the person each is for; never the skin and parts found on the photo (each
    /// photo's own are found for it). (Its id is still `smoothSkin`.)
    SmoothSkin,
}

impl SettingsGroup {
    pub const ALL: [SettingsGroup; 24] = [
        SettingsGroup::Profile,
        SettingsGroup::Treatment,
        SettingsGroup::WhiteBalance,
        SettingsGroup::Light,
        SettingsGroup::Levels,
        SettingsGroup::ToneCurve,
        SettingsGroup::Color,
        SettingsGroup::ColorMixer,
        SettingsGroup::ColorGrading,
        SettingsGroup::Primaries,
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
        SettingsGroup::PluginFilters,
        SettingsGroup::SmoothSkin,
    ];

    /// Groups a new preset includes by default: the copy defaults without White Balance (each
    /// photo keeps its own unless the photographer ticks it; the user's decision).
    pub fn default_preset() -> Vec<SettingsGroup> {
        SettingsGroup::default_copy().into_iter().filter(|g| *g != SettingsGroup::WhiteBalance).collect()
    }

    /// Groups included by default when copying (Lightroom excludes crop, masks and spots by default).
    pub fn default_copy() -> Vec<SettingsGroup> {
        SettingsGroup::ALL
            .into_iter()
            .filter(|g| {
                !matches!(g, SettingsGroup::Crop | SettingsGroup::Masks | SettingsGroup::Spots | SettingsGroup::RedEye | SettingsGroup::SmoothSkin)
            })
            .collect()
    }

    pub fn label(self) -> &'static str {
        match self {
            SettingsGroup::Profile => "Profile",
            SettingsGroup::Treatment => "Treatment",
            SettingsGroup::WhiteBalance => "White Balance",
            SettingsGroup::Light => "Light",
            SettingsGroup::Levels => "Levels",
            SettingsGroup::ToneCurve => "Tone Curve",
            SettingsGroup::Color => "Color",
            SettingsGroup::ColorMixer => "Color Mixer",
            SettingsGroup::ColorGrading => "Color Grading",
            SettingsGroup::Primaries => "Primaries",
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
            SettingsGroup::PluginFilters => "Plug-in Filters",
            SettingsGroup::SmoothSkin => "Retouch",
        }
    }

    /// Top-level JSON keys of `DevelopSettings` that belong to the group.
    pub fn keys(self) -> &'static [&'static str] {
        match self {
            SettingsGroup::Profile => &["profile"],
            SettingsGroup::Treatment => &["treatment"],
            SettingsGroup::WhiteBalance => &["wb"],
            SettingsGroup::Light => &["light", "hdr"],
            SettingsGroup::Levels => &["levels"],
            SettingsGroup::ToneCurve => &["curve"],
            SettingsGroup::Color => &["color"],
            SettingsGroup::ColorMixer => &["mixer", "bw_mix", "point_colors", "hsl_curves"],
            SettingsGroup::ColorGrading => &["grading"],
            SettingsGroup::Primaries => &["primaries"],
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
            SettingsGroup::PluginFilters => &["plugin_filters"],
            SettingsGroup::SmoothSkin => &["skin", "retouch"],
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
            } else if *k == "plugin_filters" {
                // Not written while empty: an empty stack still replaces the target's on paste.
                out.insert((*k).to_string(), Value::Array(Vec::new()));
            }
        }
        // (neutral channels are not written either: a photo without Levels still resets the Levels it is pasted on)
        if *g == SettingsGroup::Levels {
            out.insert("levels".into(), s.levels.full_json());
        }
        // (not written while off: copying it off still switches it off where it is pasted)
        if *g == SettingsGroup::SmoothSkin && !out.contains_key("skin") {
            out.insert("skin".into(), serde_json::to_value(crate::SkinSmoothing::default()).unwrap_or(Value::Null));
        }
        // (the flat-field amount isn't written at 100 %: copying it still sets it where it is pasted)
        if *g == SettingsGroup::Optics {
            with_vignetting_calibration_amount(&mut out, s);
        }
        if *g == SettingsGroup::SmoothSkin && !out.contains_key("retouch") {
            out.insert("retouch".into(), serde_json::to_value(crate::Retouch::default()).unwrap_or(Value::Null));
        }
    }
    // the Camera colour is left out of the settings while it is Auto; a preset or a sync of the Profile group still
    // carries it, so that applying it turns a Colorimetric photo back to Auto
    if let Some(Value::Object(profile)) = out.get_mut("profile") {
        profile.entry("camera_colour").or_insert_with(|| serde_json::to_value(s.profile.camera_colour).unwrap_or(Value::Null));
    }
    Value::Object(out)
}

/// One setting inside a [`SettingsGroup`] that can be ticked on its own in the New Preset, Update
/// Preset and Copy Settings trees (Lightroom's per-setting checklist): Exposure inside Light, the
/// Sharpening sliders inside Detail… `id` is `<group id>.<name>`; `paths` are the dot paths of the
/// settings JSON it covers (a group's items together cover every key of the group).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingItem {
    pub id: &'static str,
    pub label: &'static str,
    pub paths: &'static [&'static str],
}

const fn item(id: &'static str, label: &'static str, paths: &'static [&'static str]) -> SettingItem {
    SettingItem { id, label, paths }
}

const LIGHT_ITEMS: &[SettingItem] = &[
    item("light.exposure", "Exposure", &["light.exposure"]),
    item("light.contrast", "Contrast", &["light.contrast", "light.contrast_mode"]),
    item("light.highlights", "Highlights", &["light.highlights"]),
    item("light.shadows", "Shadows", &["light.shadows"]),
    item("light.whites", "Whites", &["light.whites"]),
    item("light.blacks", "Blacks", &["light.blacks"]),
    item("light.hdr", "HDR Output", &["hdr"]),
];
const CURVE_ITEMS: &[SettingItem] = &[
    item(
        "toneCurve.parametric",
        "Parametric Curve",
        &["curve.highlights", "curve.lights", "curve.darks", "curve.shadows", "curve.split_shadows", "curve.split_mid", "curve.split_highlights"],
    ),
    item("toneCurve.point", "Point Curve", &["curve.master", "curve.red", "curve.green", "curve.blue"]),
    item("toneCurve.refineSaturation", "Refine Saturation", &["curve.refine_saturation"]),
];
const COLOR_ITEMS: &[SettingItem] =
    &[item("color.vibrance", "Vibrance", &["color.vibrance"]), item("color.saturation", "Saturation", &["color.saturation"])];
const MIXER_ITEMS: &[SettingItem] = &[
    item("colorMixer.mixer", "Color Mixer", &["mixer"]),
    item("colorMixer.bw", "B&W Mix", &["bw_mix"]),
    item("colorMixer.pointColor", "Point Color", &["point_colors"]),
];
const EFFECTS_ITEMS: &[SettingItem] = &[
    item("effects.texture", "Texture", &["effects.texture"]),
    item("effects.clarity", "Clarity", &["effects.clarity"]),
    item("effects.dehaze", "Dehaze", &["effects.dehaze"]),
];
const DETAIL_ITEMS: &[SettingItem] = &[
    item("detail.sharpening", "Sharpening", &["detail.sharpen_amount", "detail.sharpen_radius", "detail.sharpen_detail", "detail.sharpen_masking"]),
    item("detail.noise", "Noise Reduction", &["detail.nr_luminance", "detail.nr_detail", "detail.nr_contrast", "detail.nr_outliers"]),
    item("detail.colorNoise", "Color Noise Reduction", &["detail.nr_color", "detail.nr_color_detail", "detail.nr_color_smoothness"]),
    item("detail.enhance", "AI Denoise and Enhance", &["enhance"]),
];
const OPTICS_ITEMS: &[SettingItem] = &[
    item("optics.lensProfile", "Lens Profile Corrections", &["optics.lens_profile", "optics.profile_distortion", "optics.profile_vignetting"]),
    item("optics.chromatic", "Chromatic Aberration", &["optics.remove_ca", "optics.ca_red", "optics.ca_blue"]),
    item(
        "optics.defringe",
        "Defringe",
        &[
            "optics.defringe_purple_amount",
            "optics.defringe_purple_hue_lo",
            "optics.defringe_purple_hue_hi",
            "optics.defringe_green_amount",
            "optics.defringe_green_hue_lo",
            "optics.defringe_green_hue_hi",
        ],
    ),
    item("optics.distortion", "Manual Distortion", &["optics.distortion"]),
    item("optics.vignetting", "Manual Vignetting", &["optics.vignetting", "optics.vignetting_midpoint"]),
    item("optics.vignettingCalibration", "Vignetting Calibration", &["optics.vignetting_calibration", "optics.vignetting_calibration_amount"]),
];
const GEOMETRY_ITEMS: &[SettingItem] = &[
    item("geometry.upright", "Upright", &["geometry.upright", "geometry.guides", "geometry.upright_transform", "geometry.lrc_upright"]),
    item(
        "geometry.transform",
        "Transform Sliders",
        &[
            "geometry.vertical",
            "geometry.horizontal",
            "geometry.rotate",
            "geometry.aspect",
            "geometry.scale",
            "geometry.offset_x",
            "geometry.offset_y",
        ],
    ),
    item("geometry.constrainCrop", "Constrain Crop", &["geometry.constrain_crop"]),
];
const CROP_ITEMS: &[SettingItem] =
    &[item("crop.crop", "Crop and Straighten", &["crop"]), item("crop.orientation", "Rotation and Flip", &["orientation"])];

impl SettingsGroup {
    /// The settings that can be ticked one by one (empty: the group is one setting).
    pub fn items(self) -> &'static [SettingItem] {
        match self {
            SettingsGroup::Light => LIGHT_ITEMS,
            SettingsGroup::ToneCurve => CURVE_ITEMS,
            SettingsGroup::Color => COLOR_ITEMS,
            SettingsGroup::ColorMixer => MIXER_ITEMS,
            SettingsGroup::Effects => EFFECTS_ITEMS,
            SettingsGroup::Detail => DETAIL_ITEMS,
            SettingsGroup::Optics => OPTICS_ITEMS,
            SettingsGroup::Geometry => GEOMETRY_ITEMS,
            SettingsGroup::Crop => CROP_ITEMS,
            _ => &[],
        }
    }

    /// The group's id (`light`, `whiteBalance`…), as commands take it.
    pub fn id(self) -> &'static str {
        match self {
            SettingsGroup::Profile => "profile",
            SettingsGroup::Treatment => "treatment",
            SettingsGroup::WhiteBalance => "whiteBalance",
            SettingsGroup::Light => "light",
            SettingsGroup::Levels => "levels",
            SettingsGroup::ToneCurve => "toneCurve",
            SettingsGroup::Color => "color",
            SettingsGroup::ColorMixer => "colorMixer",
            SettingsGroup::ColorGrading => "colorGrading",
            SettingsGroup::Primaries => "primaries",
            SettingsGroup::Effects => "effects",
            SettingsGroup::Vignette => "vignette",
            SettingsGroup::Grain => "grain",
            SettingsGroup::Detail => "detail",
            SettingsGroup::Optics => "optics",
            SettingsGroup::Geometry => "geometry",
            SettingsGroup::Crop => "crop",
            SettingsGroup::Masks => "masks",
            SettingsGroup::Spots => "spots",
            SettingsGroup::RedEye => "redEye",
            SettingsGroup::LensBlur => "lensBlur",
            SettingsGroup::Calibration => "calibration",
            SettingsGroup::PluginFilters => "pluginFilters",
            SettingsGroup::SmoothSkin => "smoothSkin",
        }
    }

    /// The group with id `id`.
    pub fn from_id(id: &str) -> Option<SettingsGroup> {
        SettingsGroup::ALL.into_iter().find(|g| g.id() == id)
    }
}

/// The setting item with id `id` and its group.
pub fn find_item(id: &str) -> Option<(SettingsGroup, &'static SettingItem)> {
    SettingsGroup::ALL.into_iter().find_map(|g| g.items().iter().find(|i| i.id == id).map(|i| (g, i)))
}

/// What to copy, save in a preset or sync: whole groups, plus single settings of groups that are only
/// partly ticked ([`SettingItem`] ids). Remembered by the dialogs that choose it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsPick {
    pub groups: Vec<SettingsGroup>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<String>,
}

impl SettingsPick {
    /// Whole groups only.
    pub fn of_groups(groups: Vec<SettingsGroup>) -> SettingsPick {
        SettingsPick { groups, settings: Vec::new() }.normalized()
    }

    /// A list of group and setting ids (`light`, `detail.sharpening`) as the dialogs keep them: a group
    /// whose every setting is ticked becomes the whole group. Unknown ids are left out.
    pub fn from_ids<S: AsRef<str>>(ids: &[S]) -> SettingsPick {
        let mut pick = SettingsPick::default();
        for id in ids {
            let id = id.as_ref();
            if let Some(g) = SettingsGroup::from_id(id) {
                if !pick.groups.contains(&g) {
                    pick.groups.push(g);
                }
            } else if find_item(id).is_some() && !pick.settings.iter().any(|s| s == id) {
                pick.settings.push(id.to_string());
            }
        }
        pick.normalized()
    }

    /// The pick as ids, one per tickable box: a group with settings lists each of them (the tree's leaves).
    pub fn to_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        for g in SettingsGroup::ALL {
            if self.groups.contains(&g) {
                if g.items().is_empty() {
                    out.push(g.id().to_string());
                } else {
                    out.extend(g.items().iter().map(|i| i.id.to_string()));
                }
            } else {
                out.extend(g.items().iter().filter(|i| self.settings.iter().any(|s| s == i.id)).map(|i| i.id.to_string()));
            }
        }
        out
    }

    /// Single settings of a group that is whole anyway are dropped; a group whose settings are all
    /// there becomes whole; unknown setting ids are dropped.
    pub fn normalized(mut self) -> SettingsPick {
        let groups = self.groups.clone();
        self.settings.retain(|s| find_item(s).is_some_and(|(g, _)| !groups.contains(&g)));
        for g in SettingsGroup::ALL {
            let items = g.items();
            if !items.is_empty() && !self.groups.contains(&g) && items.iter().all(|i| self.settings.iter().any(|s| s == i.id)) {
                self.groups.push(g);
                self.settings.retain(|s| !items.iter().any(|i| i.id == s));
            }
        }
        // in the groups' own order, once each
        self.groups = SettingsGroup::ALL.into_iter().filter(|g| self.groups.contains(g)).collect();
        let mut seen = Vec::new();
        self.settings.retain(|s| {
            let new = !seen.contains(s);
            seen.push(s.clone());
            new
        });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.settings.is_empty()
    }

    /// The pick includes all or part of `g`.
    pub fn touches(&self, g: SettingsGroup) -> bool {
        self.groups.contains(&g) || g.items().iter().any(|i| self.settings.iter().any(|s| s == i.id))
    }

    /// The pick includes setting (or group) `id`.
    pub fn has(&self, id: &str) -> bool {
        if let Some(g) = SettingsGroup::from_id(id) {
            return self.groups.contains(&g);
        }
        self.settings.iter().any(|s| s == id) || find_item(id).is_some_and(|(g, _)| self.groups.contains(&g))
    }

    /// Only what both picks hold (Paste Selected: the ticked part of what was copied).
    pub fn intersect(&self, other: &SettingsPick) -> SettingsPick {
        let ids: Vec<String> = self.to_ids().into_iter().filter(|id| other.has(id)).collect();
        SettingsPick::from_ids(&ids)
    }

    /// The part of `s` the pick covers, as a partial JSON object.
    pub fn extract(&self, s: &DevelopSettings) -> Value {
        let mut out = extract_groups(s, &self.groups);
        if self.settings.is_empty() {
            return out;
        }
        let full = s.to_json();
        let Some(o) = out.as_object_mut() else { return out };
        for id in &self.settings {
            let Some((_, it)) = find_item(id) else { continue };
            for path in it.paths {
                copy_path(&full, o, path);
            }
            // (left out of the JSON while off, as the whole group knows: copying it off switches it off)
            if it.paths.contains(&"hdr") && !o.contains_key("hdr") {
                o.insert("hdr".into(), serde_json::to_value(crate::Hdr::default()).unwrap_or(Value::Null));
            }
            if it.paths.contains(&"optics.vignetting_calibration_amount") {
                with_vignetting_calibration_amount(o, s);
            }
        }
        out
    }

    /// What the partial settings `partial` (a preset's) hold, as a pick: a key of a group with several
    /// settings counts for the settings it has.
    pub fn of_partial(partial: &Value) -> SettingsPick {
        let Some(o) = partial.as_object() else { return SettingsPick::default() };
        let mut ids: Vec<String> = Vec::new();
        for g in SettingsGroup::ALL {
            if g.items().is_empty() {
                if g.keys().iter().any(|k| o.contains_key(*k)) {
                    ids.push(g.id().to_string());
                }
                continue;
            }
            for it in g.items() {
                if it.paths.iter().any(|p| path_in(partial, p)) {
                    ids.push(it.id.to_string());
                }
            }
        }
        SettingsPick::from_ids(&ids)
    }
}

/// Put `s`'s flat-field amount into the partial `out`'s `optics` when it isn't there (it isn't written at the
/// default, but a copy of it must still set it).
fn with_vignetting_calibration_amount(out: &mut Map<String, Value>, s: &DevelopSettings) {
    let optics = out.entry("optics").or_insert_with(|| Value::Object(Map::new()));
    if let Some(o) = optics.as_object_mut() {
        o.entry("vignetting_calibration_amount").or_insert_with(|| Value::from(s.optics.vignetting_calibration_amount));
    }
}

/// `path` (dot separated) is in `v`.
fn path_in(v: &Value, path: &str) -> bool {
    let mut cur = v;
    for k in path.split('.') {
        match cur.get(k) {
            Some(n) => cur = n,
            None => return false,
        }
    }
    true
}

/// Copy the value at `path` in `from` into `to` (creating the objects on the way).
fn copy_path(from: &Value, to: &mut Map<String, Value>, path: &str) {
    let mut parts = path.split('.').peekable();
    let mut src = from;
    let mut dst = to;
    while let Some(k) = parts.next() {
        let Some(v) = src.get(k) else { return };
        if parts.peek().is_none() {
            dst.insert(k.to_string(), v.clone());
            return;
        }
        src = v;
        let entry = dst.entry(k.to_string()).or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        let Some(next) = entry.as_object_mut() else { return };
        dst = next;
    }
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

/// Interpolate numbers in `patch` towards/away from `base` by `t` (1 = patch as-is, 0 = base as-is).
/// Point curves blend point by point. Discrete values (profile id, treatment, switches, seeds) take
/// the preset's at any amount above 0, so the look fades in from nothing instead of jumping when
/// they flip half-way (#37). When the preset changes a choice (a string) or an object's `enabled`
/// switch, that object's `amount` blends from 0, not from the photo's (which was the strength of
/// its own choice), so a new profile or LUT fades in; white balance `temp` / `tint` on another
/// scale than the photo's can't be blended and are taken whole (upstream #2669 / #2896).
fn scale_patch(base: &Value, patch: &Value, t: f64) -> Value {
    if t <= 0.0 {
        return if base.is_null() { patch.clone() } else { base.clone() };
    }
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            let choice_changed = p.iter().any(|(k, pv)| {
                let changed = b.get(k).is_none_or(|bv| bv != pv);
                changed && (pv.is_string() || (k == "enabled" && pv.is_boolean()))
            });
            let other_scale = (p.contains_key("temp") || p.contains_key("tint"))
                && p.get("scale").and_then(Value::as_str).unwrap_or("legacy") != b.get("scale").and_then(Value::as_str).unwrap_or("legacy");
            // a flat-field map (an id: a choice, taken whole) that is new fades in from no correction
            let new_map = p.get("vignetting_calibration").is_some_and(|pm| !pm.is_null() && b.get("vignetting_calibration") != Some(pm));
            let zero = Value::from(0.0);
            let mut out = Map::new();
            for (k, pv) in p {
                let bv = match b.get(k) {
                    Some(Value::Number(_)) if choice_changed && k == "amount" => &zero,
                    _ if new_map && k == "vignetting_calibration_amount" => &zero,
                    Some(bv) => bv,
                    None if choice_changed && k == "amount" && pv.is_number() => &zero,
                    None => &Value::Null,
                };
                // (a portable partial's map, `optics.vignetting_calibration_map`, is taken whole too: its numbers aren't settings)
                let whole = k == crate::vignetting_calibration::PORTABLE_MAP_KEY || (other_scale && t > 0.0 && (k == "temp" || k == "tint"));
                let v = if whole { pv.clone() } else { scale_patch(bv, pv, t) };
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
        (b, Value::Array(p)) => match (curve_points(b), curve_points(&Value::Array(p.clone()))) {
            (Some(bc), Some(pc)) => blend_curves(&bc, &pc, t),
            _ => match b {
                Value::Array(ba) if ba.len() == p.len() => Value::Array(ba.iter().zip(p).map(|(b, p)| scale_patch(b, p, t)).collect()),
                _ => Value::Array(p.clone()),
            },
        },
        (_, p) => p.clone(),
    }
}

/// Most points a blended curve keeps (input-derived lengths are capped).
const MAX_CURVE_POINTS: usize = 256;

/// A point curve (`[{x, y}, …]`, empty = identity) as sorted points, or `None` when `v` is not one.
fn curve_points(v: &Value) -> Option<Vec<(f64, f64)>> {
    let a = v.as_array()?;
    let mut pts = Vec::with_capacity(a.len().min(MAX_CURVE_POINTS));
    for p in a.iter().take(MAX_CURVE_POINTS) {
        let o = p.as_object()?;
        if o.len() != 2 {
            return None;
        }
        let (x, y) = (o.get("x")?.as_f64()?, o.get("y")?.as_f64()?);
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        pts.push((x, y));
    }
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(pts)
}

/// Piecewise-linear value of a curve at `x` (empty = identity).
fn curve_at(c: &[(f64, f64)], x: f64) -> f64 {
    let (Some(first), Some(last)) = (c.first(), c.last()) else { return x };
    if x <= first.0 {
        return first.1;
    }
    if x >= last.0 {
        return last.1;
    }
    for w in c.windows(2) {
        if let [(x0, y0), (x1, y1)] = *w
            && x <= x1
        {
            let d = x1 - x0;
            return if d > 1e-12 { y0 + (y1 - y0) * (x - x0) / d } else { y1 };
        }
    }
    last.1
}

/// Blend two point curves: the union of their x positions, each y moved from the base curve's
/// value towards the preset's by `t`.
fn blend_curves(b: &[(f64, f64)], p: &[(f64, f64)], t: f64) -> Value {
    if p.is_empty() && b.is_empty() {
        return Value::Array(Vec::new());
    }
    let mut xs: Vec<f64> = b.iter().chain(p).map(|q| q.0).collect();
    if b.is_empty() || p.is_empty() {
        xs.extend([0.0, 1.0]);
    }
    xs.sort_by(f64::total_cmp);
    xs.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    xs.truncate(MAX_CURVE_POINTS);
    let pts = xs
        .into_iter()
        .map(|x| {
            let (yb, yp) = (curve_at(b, x), curve_at(p, x));
            serde_json::json!({"x": x, "y": (yb + (yp - yb) * t).clamp(0.0, 1.0)})
        })
        .collect();
    Value::Array(pts)
}

/// Apply a partial settings object with an amount (1.0 = 100 %; blending rules: [`scale_patch`]).
/// Out-of-range values are clamped through the control specs, as every merge does
/// ([`DevelopSettings::merged`]).
/// At 0 % nothing changes; at 100 % the partial is merged as it is.
pub fn apply_partial(s: &DevelopSettings, partial: &Value, amount: f64) -> DevelopSettings {
    if amount <= 0.0 {
        return s.clone();
    }
    // (a map named without an amount comes at 100 %, which the strength then blends: `DevelopSettings::merged`)
    let partial = crate::vignetting_calibration::complete_partial(partial);
    let partial = partial.as_ref();
    let patch = if (amount - 1.0).abs() < 1e-9 { Cow::Borrowed(partial) } else { Cow::Owned(scale_patch(&blend_base(s), partial, amount)) };
    // (a white balance without a scale keeps its old meaning: `DevelopSettings::merged`)
    s.merged(&patch).unwrap_or_else(|_| s.clone())
}

/// `s` as the base a partial's numbers blend from: every group written out, also the ones
/// serialization leaves out while at their defaults (HDR), so their numbers blend from `s`'s
/// values too.
fn blend_base(s: &DevelopSettings) -> Value {
    let mut base = s.to_json();
    if let (Some(o), Ok(hdr)) = (base.as_object_mut(), serde_json::to_value(s.hdr)) {
        o.entry("hdr").or_insert(hdr);
    }
    // Levels leave a channel out while it is neutral: every channel blends from its own values (neutral ones too)
    if let Some(o) = base.as_object_mut() {
        o.insert("levels".into(), s.levels.full_json());
    }
    base
}

/// Top-level settings keys that hold the photo's own pixel repairs: a preset that names them blends
/// from the photo's values, never from the import look's (masks are added by [`Preset::apply_on`]).
const REPAIR_KEYS: [&str; 3] = ["masks", "spots", "red_eye"];

/// The settings `partial` names, applied to `current` at `amount`, each blended from `original`
/// (the photo's import look; `current` itself when none is recorded) towards the partial's value
/// ([`scale_patch`]); every setting `partial` does not name keeps `current`'s value, edits
/// included. At 100 % the partial's values apply as they are and `original` is not read; at 0 %
/// each named setting takes `original`'s value. With `original` the same as `current` this is
/// [`apply_partial`] (adapted from upstream #2207).
fn compose_named(current: &DevelopSettings, original: &DevelopSettings, partial: &Value, amount: f64) -> DevelopSettings {
    if (amount - 1.0).abs() < 1e-9 || std::ptr::eq(current, original) || current == original {
        return apply_partial(current, partial, amount);
    }
    let mut base = blend_base(original);
    let cur = current.to_json();
    if let Some(b) = base.as_object_mut() {
        for k in REPAIR_KEYS {
            match cur.get(k) {
                Some(v) => b.insert(k.to_string(), v.clone()),
                None => b.remove(k),
            };
        }
    }
    let patch = if amount <= 0.0 { named_values(&base, partial) } else { scale_patch(&base, partial, amount) };
    // `merged` deep-merges: the named settings replace, everything else stays as `current` has it
    current.merged(&patch).unwrap_or_else(|_| current.clone())
}

/// `base`'s values at the places `patch` names (an object's keys, recursively; anything else is
/// one setting). A setting `base` lacks is left out, so the photo keeps its own. A white balance
/// number taken this way carries `base`'s scale with it, so it keeps its meaning.
fn named_values(base: &Value, patch: &Value) -> Value {
    let (Some(b), Some(p)) = (base.as_object(), patch.as_object()) else { return base.clone() };
    let mut out = Map::new();
    for (k, pv) in p {
        let Some(bv) = b.get(k) else { continue };
        out.insert(k.clone(), if pv.is_object() && bv.is_object() { named_values(bv, pv) } else { bv.clone() });
    }
    if (out.contains_key("temp") || out.contains_key("tint"))
        && let Some(scale) = b.get("scale")
    {
        out.entry("scale").or_insert_with(|| scale.clone());
    }
    Value::Object(out)
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

    /// Apply at `amount` (1.0 = 100 %), as Lightroom Classic does: the preset's settings are set
    /// (only those it has; everything else stays) and its masks are **added** to the photo's own,
    /// their effect scaled by the amount. Masks are recipes: what a model computed for the photo
    /// the preset was made on (Object, Describe) is dropped and computed for each photo
    /// ([`crate::Mask::needs_compute`]).
    ///
    /// The masks the latest preset application added are provisional (tagged with that preset's
    /// id): applying the same preset again right away replaces them (no doubling); applying any
    /// other preset first makes them the photographer's own (untagged), so the next application
    /// of the first preset adds a fresh set. Editing a provisional mask makes it the
    /// photographer's own too ([`settle_provisional`]). A mask with a placeholder name ("Mask 2")
    /// is named after the preset, or after the preset and what it selects when it brings several.
    ///
    /// `s` is both the photo's current settings and the baseline the amount blends from (see
    /// [`Preset::apply_on`]): right for the import default preset and for scratch bases.
    pub fn apply(&self, s: &DevelopSettings, amount: f64) -> DevelopSettings {
        self.apply_on(s, s, amount)
    }

    /// [`Preset::apply`] on `current`, with each setting the preset names blended from `original`
    /// (the photo's import look, or `current` when the photo has none) towards the preset's value:
    /// a preset at 50 % looks the same however the photo was edited before, and at 0 % the named
    /// settings take `original`'s values. Every setting the preset does not name keeps `current`'s
    /// value, and the photo's pixel repairs (masks, spots, red eye) blend from its own. Adapted
    /// from upstream #2207.
    pub fn apply_on(&self, current: &DevelopSettings, original: &DevelopSettings, amount: f64) -> DevelopSettings {
        let masks: Vec<crate::Mask> = self
            .settings
            .get("masks")
            .and_then(Value::as_array)
            .map(|m| m.iter().filter_map(|m| serde_json::from_value::<crate::Mask>(m.clone()).ok()).collect())
            .unwrap_or_default();
        let mut rest = self.settings.clone();
        if let Some(o) = rest.as_object_mut() {
            o.remove("masks");
        }
        let mut out = compose_named(current, original, &rest, amount);
        if masks.is_empty() {
            return out;
        }
        // the same preset again: its own (still provisional) masks make room for the new ones
        out.masks.retain(|m| m.preset.as_deref() != Some(self.id.as_str()));
        // whatever an earlier application left is the photographer's now
        for m in &mut out.masks {
            m.preset = None;
        }
        let short = crate::recipe::short_preset_name(&self.name);
        let several = masks.len() > 1;
        for mut m in masks {
            if crate::recipe::generic_name(&m.name) {
                let own = crate::recipe::content_name(&m);
                m.name = match (short.is_empty(), several) {
                    (true, _) => own,
                    (false, true) => format!("{short} - {own}"),
                    (false, false) => short.clone(),
                };
            }
            // above every id the photo had, so a fresh mask never takes the id of the one it replaces
            let top = current.masks.iter().chain(&out.masks).map(|m| m.id).max().unwrap_or(0);
            m.id = top.checked_add(1).unwrap_or_else(|| out.next_mask_id());
            m.preset = Some(self.id.clone());
            m.strip_ai(crate::recipe::RECIPE_ASPECT);
            m.adjust.amount = (m.adjust.amount * amount).clamp(0.0, 200.0);
            out.masks.push(m);
        }
        out
    }

    /// Whether it carries masks with AI selections (applying it computes them on each photo).
    pub fn has_ai_masks(&self) -> bool {
        self.settings
            .get("masks")
            .and_then(Value::as_array)
            .is_some_and(|m| m.iter().filter_map(|m| serde_json::from_value::<crate::Mask>(m.clone()).ok()).any(|m| m.uses_ai()))
    }

    /// Whether it has masks and no other settings (a "mask-only" preset such as Bring Sky Down).
    pub fn mask_only(&self) -> bool {
        self.settings.as_object().is_some_and(|o| o.contains_key("masks") && o.len() == 1)
    }
}

/// `partial`'s masks as recipes: no segmentation computed for one photo, no Object clicks (they
/// point at that photo's pixels: the object is picked again on each photo), no provisional tag
/// (what presets, copy and paste carry).
pub fn to_recipes(partial: &mut Value) {
    let Some(masks) = partial.get_mut("masks").and_then(Value::as_array_mut) else { return };
    for m in masks.iter_mut() {
        if let Ok(mut mask) = serde_json::from_value::<crate::Mask>(m.clone()) {
            mask.for_other_photo(crate::recipe::RECIPE_ASPECT);
            mask.preset = None;
            if let Ok(v) = serde_json::to_value(mask) {
                *m = v;
            }
        }
    }
}

/// After an edit of `before` into `after`: when the photographer changed one of the masks the
/// latest preset application added, all of that application's masks become theirs (untagged), so
/// applying that preset again adds rather than replaces.
pub fn settle_provisional(before: &DevelopSettings, after: &mut DevelopSettings) {
    let touched = after.masks.iter().filter(|m| m.preset.is_some()).any(|m| {
        before.masks.iter().find(|b| b.id == m.id).is_some_and(|b| {
            let mut b = b.clone();
            b.preset.clone_from(&m.preset);
            b != *m
        })
    });
    if touched {
        for m in &mut after.masks {
            m.preset = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn presets_add_masks_and_the_same_preset_again_replaces_its_own() {
        let mask = json!({"name": "Mask 1", "components": [{"shape": {"kind": "sky"}}], "adjust": {"exposure": -0.5}});
        let p = Preset {
            id: "p".into(),
            name: "P3 Bring Sky Down".into(),
            group: String::new(),
            settings: json!({"masks": [mask]}),
            favorite: false,
            builtin: false,
        };
        let q = Preset { id: "q".into(), ..p.clone() };
        assert!(p.has_ai_masks() && p.mask_only());
        let once = p.apply(&DevelopSettings::default(), 1.0);
        assert_eq!(once.masks.len(), 1);
        assert_eq!(once.masks[0].name, "Bring Sky Down");
        assert_eq!(once.masks[0].preset.as_deref(), Some("p"));
        let again = p.apply(&once, 0.5);
        assert_eq!(again.masks.len(), 1, "the same preset again replaces its provisional masks");
        assert_ne!(again.masks[0].id, once.masks[0].id);
        // another preset in between makes the first one's masks the photographer's
        let other = q.apply(&once, 1.0);
        assert!(other.masks.iter().filter(|m| m.preset.is_none()).count() == 1);
        assert_eq!(p.apply(&other, 1.0).masks.len(), 3);
        // an edit of a provisional mask settles them all
        let mut edited = once.clone();
        edited.masks[0].adjust.exposure = 1.0;
        settle_provisional(&once, &mut edited);
        assert!(edited.masks[0].preset.is_none());
        let mut same = once.clone();
        settle_provisional(&once, &mut same);
        assert!(same.masks[0].preset.is_some());
    }

    fn preset(settings: Value) -> Preset {
        Preset { id: "p".into(), name: "P".into(), group: "G".into(), settings, favorite: false, builtin: false }
    }

    /// A preset changes only the settings it names: a setting in a group it styles but does not
    /// name keeps the photo's value, and so does a whole group it leaves out, at any amount.
    #[test]
    fn apply_on_changes_only_the_settings_it_names() {
        let mut current = DevelopSettings::default();
        current.light.exposure = 2.0;
        current.light.blacks = -30.0;
        current.effects.clarity = 30.0;
        let mut original = DevelopSettings::default();
        original.light.blacks = -10.0;
        original.effects.clarity = 5.0;
        let p = preset(json!({"light": {"contrast": 12.0}}));
        for amount in [1.0, 0.5, 0.0] {
            let out = p.apply_on(&current, &original, amount);
            assert!((out.light.contrast - 12.0 * amount).abs() < 1e-9, "amount {amount}: {}", out.light.contrast);
            assert_eq!(out.light.exposure, 2.0, "amount {amount}: a setting the preset doesn't name keeps the photo's value");
            assert_eq!(out.light.blacks, -30.0, "amount {amount}: a setting in the same group, not named, keeps the photo's value");
            assert_eq!(out.effects.clarity, 30.0, "amount {amount}: a group the preset doesn't name is untouched");
        }
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
        let p = preset(json!({"mixer": {"blue": {"sat": 15.0, "lum": -25.0}}}));
        for amount in [1.0, 0.5] {
            let out = p.apply_on(&current, &original, amount);
            assert_eq!(out.mixer.red.sat, 20.0, "amount {amount}: a colour the preset doesn't name");
            assert_eq!(out.mixer.blue.hue, 15.0, "amount {amount}: a setting of a named colour that the preset doesn't name");
            assert_eq!((out.mixer.blue.sat, out.mixer.blue.lum), (15.0 * amount, -25.0 * amount), "amount {amount}");
        }
    }

    /// The white balance is not one setting: a preset that names only the temperature keeps the
    /// photo's tint.
    #[test]
    fn apply_on_keeps_the_white_balance_fields_it_does_not_name() {
        let mut current = DevelopSettings::default();
        current.wb.temp = 6000.0;
        current.wb.tint = 12.0;
        let original = DevelopSettings::default();
        let p = preset(json!({"wb": {"temp": 5000.0}}));
        let out = p.apply_on(&current, &original, 1.0);
        assert_eq!((out.wb.temp, out.wb.tint), (5000.0, 12.0));
    }

    /// At 0 % a white balance number the preset names takes the original's value on the
    /// original's scale, not a Kelvin number read on the relative scale.
    #[test]
    fn amount_zero_takes_the_originals_white_balance_on_its_scale() {
        let mut original = DevelopSettings::for_raw(5000.0, 4.0);
        original.wb.scale = crate::WbScale::Kelvin;
        let mut current = original.clone();
        current.wb.temp = 6200.0;
        current.wb.tint = 10.0;
        let out = preset(json!({"wb": {"temp": 7000.0}})).apply_on(&current, &original, 0.0);
        assert_eq!((out.wb.temp, out.wb.tint, out.wb.scale), (5000.0, 10.0, crate::WbScale::Kelvin));
    }

    /// A preset's amount 0 changes nothing when the original is the photo's current settings (no
    /// import state is recorded for the photo), and any other amount moves the named settings
    /// from where the photo has them.
    #[test]
    fn apply_on_without_an_import_state_scales_from_the_current_settings() {
        let mut current = DevelopSettings::default();
        current.light.exposure = 2.0;
        current.light.blacks = -30.0;
        let p = preset(json!({"light": {"exposure": 0.45, "contrast": 12.0}}));
        assert_eq!(p.apply_on(&current, &current, 0.0), current, "amount 0 applies nothing");
        let half = p.apply_on(&current, &current.clone(), 0.5);
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
        let p = preset(json!({"profile": {"look": {"name": "Not imported", "amount": 100.0}}, "light": {"contrast": 12.0}}));
        for amount in [1.0, 0.5] {
            for original in [current.clone(), DevelopSettings::default()] {
                let out = p.apply_on(&current, &original, amount);
                assert_eq!((out.profile.id.as_str(), out.profile.amount), ("lc.vivid", 60.0), "amount {amount}");
            }
        }
    }

    /// The amount blends between the original and the preset, not between the visible values and
    /// the preset: a preset at 50 % looks the same however the photo was edited before.
    #[test]
    fn apply_on_scales_against_the_original() {
        let mut current = DevelopSettings::default();
        current.light.exposure = 2.0;
        let original = DevelopSettings::default();
        let p = preset(json!({"light": {"exposure": 0.45}}));
        let out = p.apply_on(&current, &original, 0.5);
        assert!((out.light.exposure - 0.225).abs() < 1e-9, "{}", out.light.exposure);
        assert_eq!(p.apply_on(&current, &original, 0.0).light.exposure, 0.0, "at 0 % the original's value");
    }

    /// Pixel repairs are the photo's own: a preset's are patched in, and one without them never
    /// restores the original's, at any amount.
    #[test]
    fn apply_on_patches_pixel_repairs_into_the_current_ones() {
        let spot = json!([{"mode": "heal", "points": [[0.5, 0.5]], "size": 0.05, "feather": 50.0, "opacity": 100.0}]);
        let current = DevelopSettings { spots: serde_json::from_value(spot).unwrap(), ..Default::default() };
        let original = DevelopSettings::default();
        let none = preset(json!({"light": {"contrast": 10.0}}));
        let clearing = preset(json!({"light": {"contrast": 10.0}, "spots": []}));
        for amount in [1.0, 0.5, 0.0] {
            assert_eq!(
                none.apply_on(&current, &original, amount).spots.len(),
                1,
                "amount {amount}: the photo's spot survives a preset without spots"
            );
        }
        assert_eq!(clearing.apply_on(&current, &original, 1.0).spots.len(), 0, "a preset that names spots replaces them, as before");
        assert_eq!(clearing.apply_on(&current, &original, 0.0).spots.len(), 1, "at 0 % the photo's own spots are the baseline");
    }

    /// A null a preset writes into an option field (here a crop aspect: null is "free") is the
    /// preset's value, so it clears the photo's aspect at any amount above 0, as any choice of the
    /// preset does.
    #[test]
    fn a_preset_null_clears_an_option_field() {
        let mut current = DevelopSettings::default();
        current.crop.aspect = Some((3, 2));
        let p = preset(json!({"crop": {"aspect": null}}));
        for original in [DevelopSettings::default(), current.clone()] {
            for amount in [1.0, 0.7, 0.3] {
                assert_eq!(p.apply_on(&current, &original, amount).crop.aspect, None, "amount {amount}: the preset's null clears the aspect");
            }
        }
    }

    /// A null in the original (the import state had a free aspect) is the baseline at 0 %; above
    /// it the preset's aspect applies (choices take the preset's at any amount above 0).
    #[test]
    fn a_null_in_the_original_is_the_baseline_at_amount_zero() {
        let mut current = DevelopSettings::default();
        current.crop.aspect = Some((3, 2));
        let original = DevelopSettings::default();
        let p = preset(json!({"crop": {"aspect": [16, 9]}}));
        assert_eq!(p.apply_on(&current, &original, 0.0).crop.aspect, None, "0 %: the import baseline (no aspect) applies");
        assert_eq!(p.apply_on(&current, &original, 0.4).crop.aspect, Some((16, 9)), "40 %: the preset's aspect applies");
    }

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
        for (amount, seed) in [(0.3, 12345), (0.5, 12345), (1.5, 12345), (2.0, 12345)] {
            let out = p.apply(&base, amount);
            assert!((out.light.exposure - amount.min(5.0)).abs() < 1e-9, "{amount}: exposure {}", out.light.exposure);
            assert!((out.grain.amount - (40.0 * amount).min(100.0)).abs() < 1e-9, "{amount}: grain {}", out.grain.amount);
            assert_eq!(out.grain.seed, seed, "{amount}: seed");
        }
    }

    /// #37: lowering the amount faded the preset until about 50 %, then jumped to the unedited
    /// image because the curve, profile and switches flipped back to the photo's own there.
    #[test]
    fn amount_fades_gradually_to_nothing() {
        let mut a = DevelopSettings::default();
        a.profile.id = "lc.vivid".into();
        a.light.contrast = 40.0;
        a.curve.master = vec![lcc_geom::Point { x: 0.0, y: 0.1 }, lcc_geom::Point { x: 0.5, y: 0.6 }, lcc_geom::Point { x: 1.0, y: 1.0 }];
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        let base = DevelopSettings::default();
        assert_eq!(p.apply(&base, 0.0), apply_partial(&base, &json!({}), 1.0), "0 % is the photo as it was");
        let mid = |s: &DevelopSettings| lcc_develop_curve_mid(&s.curve.master);
        let mut last = (0.0, 0.5);
        for step in 1..=20 {
            let t = f64::from(step) / 10.0;
            let out = p.apply(&base, t);
            assert_eq!(out.profile.id, "lc.vivid", "{t}: profile switches as soon as the preset is there");
            let now = (out.light.contrast, mid(&out));
            assert!(now.0 > last.0 && now.0 - last.0 < 4.0 + 1e-9, "{t}: contrast {now:?} after {last:?}");
            assert!(now.1 >= last.1 - 1e-9 && now.1 - last.1 < 0.011, "{t}: curve mid {now:?} after {last:?}");
            last = now;
        }
    }

    /// A preset that picks another profile fades its strength in from 0 %, not from the photo's own
    /// profile amount; white balance on another scale is taken whole.
    #[test]
    fn a_new_choice_fades_its_amount_in_from_zero() {
        let mut a = DevelopSettings::default();
        a.profile.id = "lc.vivid".into();
        a.profile.amount = 80.0;
        let p = Preset::from_settings("p", "P", "User", &a, &SettingsGroup::default_copy());
        let mut base = DevelopSettings::default();
        base.profile.amount = 100.0;
        let half = p.apply(&base, 0.5);
        assert_eq!(half.profile.id, "lc.vivid");
        assert!((half.profile.amount - 40.0).abs() < 1e-6, "{}", half.profile.amount);
        let out = scale_patch(&json!({"temp": 0.1, "tint": 0.0}), &json!({"temp": 5500.0, "tint": 3.0, "scale": "kelvin"}), 0.5);
        assert_eq!(out["temp"], json!(5500.0));
    }

    fn lcc_develop_curve_mid(c: &[lcc_geom::Point]) -> f64 {
        let pts: Vec<(f64, f64)> = c.iter().map(|p| (p.x, p.y)).collect();
        curve_at(&pts, 0.5)
    }

    /// Every group with single settings is covered by them: all ticked = the whole group.
    #[test]
    fn group_items_cover_every_key() {
        let mut s = DevelopSettings::default();
        s.hdr.enabled = true;
        // only written when an XMP carried Lightroom's Upright matrix
        s.geometry.lrc_upright = Some(lcc_geom::Homography::IDENTITY);
        // only written while the photo has one (and the amount only off its default)
        s.optics.vignetting_calibration = crate::EmbeddedVignettingMap::from_ev([4, 4], [2, 2], None, Default::default(), &[0.0, 0.5, 0.5, 1.0])
            .map(|m| crate::VignettingMapId::of(&m));
        s.optics.vignetting_calibration_amount = 80.0;
        let full = s.to_json();
        for g in SettingsGroup::ALL {
            if g.items().is_empty() {
                continue;
            }
            let whole = extract_groups(&s, &[g]);
            let pick = SettingsPick { groups: vec![], settings: g.items().iter().map(|i| i.id.to_string()).collect() };
            assert_eq!(pick.clone().normalized().groups, vec![g], "{g:?}");
            let by_items = SettingsPick { groups: vec![], settings: pick.settings }.extract(&s);
            assert_eq!(whole, by_items, "{g:?}: the settings miss a key of the group");
            for it in g.items() {
                assert!(it.id.starts_with(&format!("{}.", g.id())), "{}", it.id);
                for p in it.paths {
                    assert!(
                        path_in(&full, p) || ["camera_profile", "retouch", "light.contrast_mode"].contains(p),
                        "{}: {p} isn't in the settings",
                        it.id
                    );
                }
            }
        }
        for g in SettingsGroup::ALL {
            assert_eq!(serde_json::to_value(g).unwrap(), json!(g.id()));
            assert_eq!(SettingsGroup::from_id(g.id()), Some(g));
        }
    }

    /// Client note 8a: Light without Exposure leaves each photo's exposure alone.
    #[test]
    fn single_settings_travel_alone() {
        let mut a = DevelopSettings::default();
        a.light.exposure = 1.5;
        a.light.contrast = 30.0;
        a.detail.sharpen_amount = 80.0;
        a.detail.nr_luminance = 40.0;
        let pick = SettingsPick::from_ids(&["light.contrast", "detail.sharpening"]);
        assert_eq!(pick.settings, vec!["light.contrast".to_string(), "detail.sharpening".to_string()]);
        let p = Preset { settings: pick.extract(&a), ..Preset::from_settings("p", "P", "User", &a, &[]) };
        assert_eq!(
            p.settings,
            json!({"light": {"contrast": 30.0}, "detail": {"sharpen_amount": 80.0, "sharpen_radius": 1.0, "sharpen_detail": 25.0, "sharpen_masking": 0.0}})
        );
        let mut b = DevelopSettings::default();
        b.light.exposure = -0.7;
        b.detail.nr_luminance = 12.0;
        let out = p.apply(&b, 1.0);
        assert_eq!((out.light.exposure, out.light.contrast), (-0.7, 30.0));
        assert_eq!((out.detail.sharpen_amount, out.detail.nr_luminance), (80.0, 12.0));
        // what the preset holds, as ticks again
        assert_eq!(SettingsPick::of_partial(&p.settings), pick);
        assert_eq!(pick.to_ids(), vec!["light.contrast".to_string(), "detail.sharpening".to_string()]);
        // a whole group lists every setting as a tick
        let whole = SettingsPick::from_ids(&["effects", "whiteBalance", "nonsense", "light.nope"]);
        assert_eq!(whole.groups, vec![SettingsGroup::WhiteBalance, SettingsGroup::Effects]);
        assert!(whole.settings.is_empty());
        assert_eq!(whole.to_ids(), vec!["whiteBalance", "effects.texture", "effects.clarity", "effects.dehaze"]);
        assert!(whole.has("effects.clarity") && whole.has("whiteBalance") && !whole.has("light.exposure"));
        assert_eq!(whole.intersect(&SettingsPick::from_ids(&["effects.clarity", "light"])), SettingsPick::from_ids(&["effects.clarity"]));
    }

    /// Client note 8b: a preset saved without White Balance never touches a photo's own (as shot or
    /// set by hand), and New Preset leaves it out unless it is ticked.
    #[test]
    fn preset_without_white_balance_keeps_each_photos_own() {
        assert!(!SettingsGroup::default_preset().contains(&SettingsGroup::WhiteBalance));
        assert!(SettingsGroup::default_copy().contains(&SettingsGroup::WhiteBalance));
        let mut src = DevelopSettings::for_raw(3200.0, 12.0);
        src.wb.mode = crate::WbMode::Custom;
        src.wb.scale = crate::WbScale::Kelvin;
        src.light.exposure = 0.4;
        src.calibration.red_hue = 10.0;
        for pick in [SettingsPick::of_groups(SettingsGroup::default_preset()), SettingsPick::from_ids(&["light", "color", "calibration"])] {
            let p = Preset { settings: pick.extract(&src), ..Preset::from_settings("p", "P", "User", &src, &[]) };
            assert!(p.settings.get("wb").is_none(), "{:?}", p.settings);
            for (temp, tint, mode, scale) in [
                (5600.0, 4.0, crate::WbMode::AsShot, crate::WbScale::Kelvin),
                (6500.0, 0.0, crate::WbMode::AsShot, crate::WbScale::Legacy),
                (4100.0, -8.0, crate::WbMode::Custom, crate::WbScale::Kelvin),
            ] {
                let mut photo = DevelopSettings::for_raw(temp, tint);
                photo.wb.mode = mode;
                photo.wb.scale = scale;
                let out = p.apply(&photo, 1.0);
                assert_eq!(out.wb, photo.wb);
                assert_eq!(out.light.exposure, 0.4);
                let half = p.apply(&photo, 0.5);
                assert_eq!(half.wb, photo.wb);
            }
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
