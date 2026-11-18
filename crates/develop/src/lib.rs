//! The non-destructive edit model of the app.
//!
//! [`DevelopSettings`] is the complete description of a photo's look: pure, serializable data.
//! The pipeline evaluates it; the catalog stores it; presets, copy/paste, sync, versions and history
//! are operations on it. Numeric sliders are addressed by id through [`controls`].
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod brush_model;
pub mod controls;
pub mod levels_curve;
mod levels_table;
pub mod presets;
pub mod recipe;
pub mod retouch;
pub mod segmask;
pub mod settings;
pub mod vignetting_calibration;

pub use controls::{CONTROLS, ControlSpec, Section, Shade, Track};
pub use presets::{Preset, SettingItem, SettingsGroup, SettingsPick, apply_partial, extract_groups};
pub use retouch::{MAX_RETOUCH_MASKS, PartRequest, Retouch, RetouchAnalysis, RetouchMask, RetouchPart, RetouchTeeth};
pub use segmask::SegMask;
pub use settings::*;
pub use vignetting_calibration::{
    DEFAULT_VIGNETTING_CALIBRATION_PERCENT, EmbeddedVignettingMap, EvCodes, VignettingMapId, VignettingMapOrigin, absorb_portable, embed_portable,
};

use serde_json::Value;

/// The default look (Craft Colour): adds nothing of its own, so over a camera profile with a look of its own (a
/// tone curve or look table) the photo develops with that look, "As profile".
pub const DEFAULT_LOOK: &str = "lc.color";

impl DevelopSettings {
    /// Whether the camera profile's own look (its tone curve and look table) applies: with the default look ("As
    /// profile", [`DEFAULT_LOOK`]). Any other look sits on the camera profile's calibration only, so two looks are
    /// never stacked (OneSlips, LR-PROF-LAYERS). Settings from before this rule keep their JSON: a photo on the
    /// default look renders as it did, one on another look loses the camera profile's look under it.
    pub fn keeps_camera_look(&self) -> bool {
        self.profile.id.is_empty() || self.profile.id == DEFAULT_LOOK
    }

    /// Settings for a freshly imported raw file (Lightroom applies capture sharpening and colour
    /// noise reduction by default to raw files).
    pub fn for_raw(as_shot_temp: f64, as_shot_tint: f64) -> DevelopSettings {
        let mut s = DevelopSettings::default();
        s.wb.temp = as_shot_temp;
        s.wb.tint = as_shot_tint;
        s.detail.sharpen_amount = 40.0;
        s.detail.nr_color = 25.0;
        s.detail.nr_outliers = true;
        s
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn from_json(v: &Value) -> Result<DevelopSettings, serde_json::Error> {
        serde_json::from_value(v.clone())
    }

    /// [`Self::from_json`] for settings from outside the app, such as a sidecar's `lc:settings`
    /// (issue #2555): kept in range as [`Self::merged`] keeps them.
    pub fn from_untrusted_json(v: &Value) -> Result<DevelopSettings, serde_json::Error> {
        let mut s = DevelopSettings::from_json(v)?;
        s.keep_in_range(&DevelopSettings::default());
        Ok(s)
    }

    /// Merge a partial JSON object into these settings (fields not mentioned are kept). A white balance that
    /// names no [`WbScale`] keeps the meaning it always had (`Legacy`), whatever scale these settings were on: a
    /// partial from before the scale existed, or one that never said, must not become Kelvin by landing on
    /// Kelvin settings. Out-of-range values are brought back into range ([`DevelopSettings::keep_in_range`]).
    /// A flat-field map (`optics.vignetting_calibration`, an id) named without `optics.vignetting_calibration_amount` comes at the default
    /// amount (100); `null` removes it and keeps the amount. A portable partial's map (`optics.vignetting_calibration_map`) only
    /// sets the id here: storing the map is the catalog's ([`vignetting_calibration::absorb_portable`]).
    pub fn merged(&self, partial: &Value) -> Result<DevelopSettings, serde_json::Error> {
        let partial = vignetting_calibration::complete_partial(partial);
        let mut owned;
        let mut partial = partial.as_ref();
        if partial.get("optics").is_some_and(|o| o.get(vignetting_calibration::PORTABLE_MAP_KEY).is_some()) {
            owned = partial.clone();
            vignetting_calibration::absorb_portable(&mut owned);
            partial = &owned;
        }
        let mut v = self.to_json();
        presets::deep_merge(&mut v, partial);
        let mut out = DevelopSettings::from_json(&v)?;
        if let Some(wb) = partial.get("wb").and_then(Value::as_object)
            && (wb.contains_key("temp") || wb.contains_key("tint"))
            && !wb.contains_key("scale")
        {
            out.wb.scale = WbScale::Legacy;
        }
        out.keep_in_range(self);
        Ok(out)
    }

    /// What crs XMP import does to the values it reads, for settings that arrive as JSON (presets, `develop.merge`,
    /// `render --settings`; issue #1946): every slider clamped through its control spec, point curves in 0..1, and a
    /// crop with no area left as it was in `before`. The angle is kept within ±45° (not finite: 0). Unrotated, the crop
    /// is clipped to the photo first; a turned crop may reach past 0..1 and is bounded where the photo's size is known
    /// (`CropGeometry::bounded`, applied by the pipeline).
    fn keep_in_range(&mut self, before: &DevelopSettings) {
        for c in controls::CONTROLS {
            if let Some(v) = controls::get(self, c.id) {
                controls::set(self, c.id, v);
            }
        }
        let c = &mut self.curve;
        for p in [&mut c.master, &mut c.red, &mut c.green, &mut c.blue].into_iter().flatten() {
            *p = lcc_geom::Point::new(p.x.clamp(0.0, 1.0), p.y.clamp(0.0, 1.0));
        }
        for ch in Levels::CHANNELS {
            if let Some(c) = self.levels.channel_mut(ch) {
                *c = c.sanitized();
            }
        }
        let hsl = &mut self.hsl_curves;
        for p in [&mut hsl.hue, &mut hsl.sat, &mut hsl.lum].into_iter().flatten() {
            *p = lcc_geom::Point::new(p.x.clamp(0.0, 1.0), p.y.clamp(-1.0, 1.0));
        }
        let g = &mut self.crop.geometry;
        g.angle = if g.angle.is_finite() { g.angle.clamp(-45.0, 45.0) } else { 0.0 };
        if g.angle == 0.0 {
            g.rect = g.rect.intersect(&lcc_geom::Rect::UNIT);
        }
        if g.rect.is_empty() {
            let old = before.crop.geometry.rect;
            g.rect = if old.is_empty() { lcc_geom::Rect::UNIT } else { old };
        }
    }

    /// Stable 64-bit hash of the settings (FNV-1a over canonical JSON), for preview cache keys.
    pub fn hash64(&self) -> u64 {
        let s = serde_json::to_string(self).unwrap_or_default();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in s.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    }

    /// True if nothing differs from a fresh default (ignoring white balance "as shot" values, and
    /// the rendering process: that is not an edit).
    pub fn is_unedited(&self) -> bool {
        let mut a = self.clone();
        let d = DevelopSettings::default();
        a.wb = d.wb;
        a.process = d.process;
        // (new photos start with Constrain Crop on: not an edit)
        a.geometry.constrain_crop = d.geometry.constrain_crop;
        a == d
    }

    /// The pixel-filter plug-ins that render, in order: enabled entries of `stage`, none while the
    /// `pluginFilters` section's eye is off.
    pub fn active_plugin_filters(&self, stage: FilterStage) -> impl Iterator<Item = &PluginFilter> {
        let on = self.section_enabled(PLUGIN_FILTERS_SECTION);
        self.plugin_filters.iter().filter(move |f| on && f.enabled && f.stage == stage)
    }

    pub fn section_enabled(&self, section: &str) -> bool {
        !self.disabled_sections.iter().any(|s| s == section)
    }

    /// The settings as rendered: an Edit-panel section whose eye is off contributes nothing (issue
    /// #316). Light (with its tone curve), Color (white balance, presence, the mixers, colour grading
    /// and point colour) and Detail are reset to their defaults here, and so are the fork's
    /// Lightroom Classic panels: Tone Curve (`curve`), Color Mixer (`mixer`: the mixers and point
    /// colour) and Color Grading (`grading`). Effects, Optics, Geometry and Calibration the pipeline
    /// switches off where it applies them. Borrowed when every section is on.
    pub fn effective(&self) -> std::borrow::Cow<'_, DevelopSettings> {
        let off = |section: &str| !self.section_enabled(section);
        if !(off("light")
            || off("color")
            || off("curve")
            || off("mixer")
            || off("grading")
            || off("primaries")
            || off("levels")
            || off("detail")
            || (off(PLUGIN_FILTERS_SECTION) && !self.plugin_filters.is_empty()))
        {
            return std::borrow::Cow::Borrowed(self);
        }
        let mut d = self.clone();
        if off("light") {
            d.reset_section(Section::Light);
            d.reset_section(Section::Curve);
        }
        if off("color") {
            for section in [Section::Color, Section::Mixer, Section::BwMix, Section::Grading, Section::PointColor] {
                d.reset_section(section);
            }
            d.point_colors.clear();
        }
        if off("curve") {
            d.reset_section(Section::Curve);
        }
        if off("mixer") {
            for section in [Section::Mixer, Section::BwMix, Section::PointColor] {
                d.reset_section(section);
            }
            d.point_colors.clear();
        }
        if off("grading") {
            d.reset_section(Section::Grading);
        }
        if off("primaries") {
            d.reset_section(Section::Primaries);
        }
        if off("levels") {
            d.reset_section(Section::Levels);
        }
        if off("detail") {
            d.reset_section(Section::Detail);
        }
        if off(PLUGIN_FILTERS_SECTION) {
            d.plugin_filters.clear();
        }
        std::borrow::Cow::Owned(d)
    }

    /// How much of the AI-denoised picture to mix in, 0 (none) to 1 (all of it): the Denoise amount, unless the Detail
    /// section is switched off or the value is not a number. The process decides the slider's response
    /// ([`settings::denoise_weight`]): a straight mix up to V5, eased from V6.
    pub fn denoise_amount(&self) -> f32 {
        let a = self.enhance.denoise;
        if a.is_finite() && self.enhance.denoise_enabled() && self.section_enabled("detail") {
            settings::denoise_weight(self.process.process(), (a / 100.0).clamp(0.0, 1.0) as f32)
        } else {
            0.0
        }
    }

    /// Whether retouch section `section` is on: its own eye and the Retouch panel's (`retouch`).
    pub fn retouch_section_on(&self, section: &str) -> bool {
        self.section_enabled("retouch") && self.section_enabled(section)
    }

    pub fn set_section_enabled(&mut self, section: &str, on: bool) {
        self.disabled_sections.retain(|s| s != section);
        if !on {
            self.disabled_sections.push(section.to_string());
        }
    }

    /// Reset every control in `section` to its default, and the settings of the section that are not
    /// controls (the creative profile, the curve, the white balance mode).
    pub fn reset_section(&mut self, section: Section) {
        for c in controls::in_section(section) {
            controls::set(self, c.id, c.default);
        }
        match section {
            Section::Light => self.light.contrast_mode = ContrastMode::default(),
            Section::Curve => {
                let d = ToneCurve::default();
                self.curve = d;
            }
            Section::Color => self.wb.mode = WbMode::AsShot,
            Section::Detail => self.enhance.denoise_on = None,
            // the Log variant is a switch, not a control: the whole group goes back to its default
            Section::Primaries => self.primaries = Primaries::default(),
            Section::Levels => self.levels = Levels::default(),
            // the creative profile is not a control: the chosen look goes back to the default one,
            // and so does the camera's own local tone mapping
            Section::Profile => {
                let d = Profile::default();
                self.profile.id = d.id;
                self.profile.camera_local_tone = d.camera_local_tone;
            }
            _ => {}
        }
    }

    /// Next free mask id: one past the highest, or, when that is `u32::MAX` (ids come from files,
    /// pastes and agents), the lowest one no mask uses.
    pub fn next_mask_id(&self) -> u32 {
        let used = |id: u32| self.masks.iter().any(|m| m.id == id);
        match self.masks.iter().map(|m| m.id).max() {
            None => 1,
            // fewer masks than ids: one of the first `masks.len() + 1` is free
            Some(max) => max.checked_add(1).or_else(|| (1..=u32::MAX).find(|&id| !used(id))).unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests_genfill;
#[cfg(test)]
mod tests_levels;
#[cfg(test)]
mod tests_levels_curve;
#[cfg(test)]
mod tests_levels_paths;
#[cfg(test)]
mod tests_process;
#[cfg(test)]
mod tests_skin;

#[cfg(test)]
mod tests {
    use crate::{LEGACY_LOCAL_HUE_DEG, LocalAdjustments};

    #[test]
    fn mask_hue_in_degrees_round_trips_and_old_values_convert() {
        for deg in [-180.0, -97.5, 0.0, 34.0, 179.0, 180.0] {
            let a = LocalAdjustments { hue: deg, exposure: 0.5, ..Default::default() };
            let back: LocalAdjustments = serde_json::from_value(serde_json::to_value(a).unwrap()).unwrap();
            assert_eq!(back, a, "{deg}");
        }
        // an old catalog / preset: the −100..100 slider value, no marker
        let old: LocalAdjustments = serde_json::from_value(json!({"hue": 100.0, "saturation": 20.0})).unwrap();
        assert!((old.hue - 100.0 * LEGACY_LOCAL_HUE_DEG).abs() < 1e-12 && (old.hue - 34.377).abs() < 1e-3);
        assert_eq!((old.saturation, old.amount), (20.0, 100.0), "the other fields and defaults are untouched");
        // hostile: wrong types are an error, never a panic
        assert!(serde_json::from_value::<LocalAdjustments>(json!({"hue": "x", "hue_deg": 3})).is_err());
    }

    #[test]
    fn resetting_the_profile_section_restores_the_default_profile() {
        // the chosen creative profile is not a control, so resetting the section must restore it too
        let mut s = super::DevelopSettings::default();
        s.profile.id = "lc.muted.matte-soft".into();
        s.profile.amount = 40.0;
        s.profile.camera_local_tone = !super::Profile::default().camera_local_tone;
        s.reset_section(super::Section::Profile);
        assert_eq!(s.profile.id, super::Profile::default().id, "the creative profile survives a Profile reset");
        assert_eq!(s.profile.amount, super::Profile::default().amount);
        assert_eq!(s.profile.camera_local_tone, super::Profile::default().camera_local_tone);
    }

    use super::*;
    use serde_json::json;

    /// Remove Hot Pixels (#1345) is on for a newly imported raw and off for everything else, and an edit saved
    /// before the box existed reads as off, so it renders as it did.
    #[test]
    fn hot_pixel_box_is_on_for_new_raws_only() {
        assert!(DevelopSettings::for_raw(5500.0, 10.0).detail.nr_outliers);
        assert!(!DevelopSettings::default().detail.nr_outliers);
        let mut saved = DevelopSettings::for_raw(5500.0, 10.0).to_json();
        let detail = saved.get_mut("detail").and_then(|d| d.as_object_mut()).unwrap();
        assert!(detail.remove("nr_outliers").is_some());
        let read = DevelopSettings::from_json(&saved).unwrap();
        assert!(!read.detail.nr_outliers);
        assert_eq!(read.detail.nr_color, 25.0, "the rest of the saved edit is kept");
        assert_ne!(read.hash64(), DevelopSettings::for_raw(5500.0, 10.0).hash64(), "cached renders are told apart");
    }

    /// The Camera colour option is left out of the JSON while it is Auto (settings from before it keep their hash, and
    /// every cached preview stays valid); Colorimetric is stored, read back, and changes the hash.
    #[test]
    fn camera_colour_is_stored_only_when_it_is_not_auto() {
        let auto = DevelopSettings::default();
        assert!(!auto.to_json().to_string().contains("camera_colour"));
        let mut colorimetric = auto.clone();
        colorimetric.profile.camera_colour = CameraColour::Colorimetric;
        let v = colorimetric.to_json();
        assert_eq!(v["profile"]["camera_colour"], json!("colorimetric"));
        assert_eq!(DevelopSettings::from_json(&v).unwrap().profile.camera_colour, CameraColour::Colorimetric);
        assert_ne!(colorimetric.hash64(), auto.hash64());
        // a merge can set it back by name
        let back: Profile = serde_json::from_value(json!({"camera_colour": "auto"})).unwrap();
        assert!(back.camera_colour.is_auto());
    }

    /// `Light::contrast_mode` is omitted while `Luminance` (so existing settings keep their JSON and
    /// hash), stored when `Rgb`, carried by `light.contrast` presets, and restored by a Light reset.
    #[test]
    fn contrast_mode_is_stored_only_when_it_is_rgb() {
        let lum = DevelopSettings::default();
        assert_eq!(lum.light.contrast_mode, ContrastMode::Luminance);
        assert!(!lum.to_json().to_string().contains("contrast_mode"));
        let mut rgb = lum.clone();
        rgb.light.contrast = 35.0;
        rgb.light.contrast_mode = ContrastMode::Rgb;
        let v = rgb.to_json();
        assert_eq!(v["light"]["contrast_mode"], json!("rgb"));
        assert_eq!(DevelopSettings::from_json(&v).unwrap().light.contrast_mode, ContrastMode::Rgb);
        assert_ne!(rgb.hash64(), lum.hash64());
        let back = rgb.merged(&json!({"light": {"contrast_mode": "luminance"}})).unwrap();
        assert_eq!(back.light.contrast_mode, ContrastMode::Luminance);
        let pick = SettingsPick::from_ids(&["light.contrast"]);
        let extracted = pick.extract(&rgb);
        assert_eq!(apply_partial(&lum, &extracted, 1.0).light.contrast_mode, ContrastMode::Rgb);
        let mut reset = rgb.clone();
        reset.reset_section(Section::Light);
        assert_eq!(reset.light.contrast_mode, ContrastMode::Luminance);
    }

    /// A preset or sync of the Profile group carries the Camera colour both ways: Colorimetric onto an Auto photo,
    /// and Auto back onto a Colorimetric one.
    #[test]
    fn the_profile_group_carries_the_camera_colour_both_ways() {
        let auto = DevelopSettings::default();
        let mut colorimetric = auto.clone();
        colorimetric.profile.camera_colour = CameraColour::Colorimetric;
        let on = extract_groups(&colorimetric, &[SettingsGroup::Profile]);
        let off = extract_groups(&auto, &[SettingsGroup::Profile]);
        assert_eq!(apply_partial(&auto, &on, 1.0).profile.camera_colour, CameraColour::Colorimetric);
        assert_eq!(apply_partial(&colorimetric, &off, 1.0).profile.camera_colour, CameraColour::Auto);
        // and a preset of the group still applies at a low amount (where discrete fields already take the preset's)
        let mut lighter = colorimetric.clone();
        lighter.light.exposure = 1.0;
        let both = extract_groups(&lighter, &[SettingsGroup::Light, SettingsGroup::Profile]);
        let weak = apply_partial(&auto, &both, 0.3);
        assert!((weak.light.exposure - 0.3).abs() < 1e-9, "the preset applied: exposure {}", weak.light.exposure);
        assert_eq!(weak.profile.camera_colour, CameraColour::Colorimetric, "a discrete field takes the preset's at any amount above 0");
        // other groups leave it alone
        let light = extract_groups(&auto, &[SettingsGroup::Light]);
        assert_eq!(apply_partial(&colorimetric, &light, 1.0).profile.camera_colour, CameraColour::Colorimetric);
    }

    /// Lists in settings JSON keep their first items up to their limits, so a sidecar, preset or
    /// `develop.merge` can't ask for thousands of full-size mask planes or per-pixel loops: 10,000
    /// masks, 10⁶ colour samples and 10⁶ empty strokes come back within limits (issue #2121).
    #[test]
    fn huge_lists_in_settings_json_are_cut_to_their_limits() {
        let linear = json!({"op": "add", "shape": {"kind": "linear", "start": {"x": 0, "y": 0}, "end": {"x": 1, "y": 1}}});
        let masks: Vec<Value> = (0..10_000).map(|i| json!({"id": i, "components": [linear.clone()]})).collect();
        let samples = vec![json!([0.5, 0.0, 0.0]); 1_000_000];
        let strokes = vec![json!({}); 1_000_000];
        let bomb = json!({
            "masks": masks,
            "spots": vec![json!({}); 5_000],
            "curve": {"master": vec![json!({"x": 0.5, "y": 0.5}); 10_000]},
        });
        let s = DevelopSettings::from_json(&bomb).unwrap();
        assert_eq!((s.masks.len(), s.spots.len(), s.curve.master.len()), (MAX_MASKS, MAX_SPOTS, MAX_CURVE_POINTS));
        let inner = json!({"masks": [{"components": [
            {"shape": {"kind": "colorRange", "samples": samples, "refine": 50}},
            {"shape": {"kind": "brush", "strokes": strokes}},
        ]}]});
        let s = DevelopSettings::default().merged(&inner).unwrap();
        let shapes: Vec<&MaskShape> = s.masks[0].components.iter().map(|c| &c.shape).collect();
        assert!(matches!(shapes[0], MaskShape::ColorRange { samples, .. } if samples.len() == MAX_COLOR_SAMPLES));
        assert!(matches!(shapes[1], MaskShape::Brush { strokes } if strokes.len() == MAX_STROKES));
        // within the limits, nothing changes
        let few = json!({"masks": [{"id": 3, "components": [linear.clone(), linear]}], "redEye": []});
        assert_eq!(DevelopSettings::from_json(&few).unwrap().masks[0].components.len(), 2);
    }

    /// `over_list_limits` and `cap_lists` cover the same lists: settings built in code past every
    /// limit are over, and cut back they aren't (each list at its limit).
    #[test]
    fn list_limits_are_checked_and_cut_alike() {
        let p = lcc_geom::Point::new(0.5, 0.5);
        let mut s = DevelopSettings::default();
        assert!(!s.over_list_limits());
        let stroke = BrushStroke { points: vec![p; MAX_STROKE_POINTS + 1], pressure: vec![1.0; MAX_STROKE_POINTS + 1], ..Default::default() };
        let shapes = [
            MaskShape::Brush { strokes: vec![stroke; 2] },
            MaskShape::ColorRange { samples: vec![[0.0; 3]; MAX_COLOR_SAMPLES + 1], refine: 0.0, areas: Vec::new() },
            MaskShape::Object { hint: vec![p; MAX_PROMPT_POINTS + 1], exclude: vec![p; MAX_PROMPT_POINTS + 1], seg: None, detail: vec![], edge: 0.0 },
            MaskShape::People { person: 0, parts: vec![String::new(); MAX_PEOPLE_PARTS + 1], who: None, seg: None },
        ];
        // one list over at a time: each is noticed, and cut
        for (k, shape) in shapes.into_iter().enumerate() {
            let mut t = DevelopSettings::default();
            t.masks.push(Mask {
                components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape }],
                ..Default::default()
            });
            assert!(t.over_list_limits(), "shape {k}");
            t.cap_lists();
            assert!(!t.over_list_limits(), "shape {k}");
        }
        s.masks = vec![Mask::default(); MAX_MASKS + 1];
        s.masks[0].components =
            vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Subject }; MAX_MASK_COMPONENTS + 1];
        s.spots = vec![Spot { points: vec![p; MAX_SPOT_POINTS + 1], ..Default::default() }; MAX_SPOTS + 1];
        s.red_eye = vec![RedEye::default(); MAX_RED_EYES + 1];
        s.point_colors = vec![PointColor::default(); MAX_POINT_COLORS + 1];
        s.curve.blue = vec![p; MAX_CURVE_POINTS + 1];
        s.geometry.guides = vec![(p, p); MAX_GUIDES + 1];
        s.disabled_sections = vec![String::new(); MAX_DISABLED_SECTIONS + 1];
        assert!(s.over_list_limits());
        s.cap_lists();
        assert!(!s.over_list_limits());
        assert_eq!(
            (s.masks.len(), s.masks[0].components.len(), s.spots.len(), s.spots[0].points.len(), s.red_eye.len(), s.curve.blue.len()),
            (MAX_MASKS, MAX_MASK_COMPONENTS, MAX_SPOTS, MAX_SPOT_POINTS, MAX_RED_EYES, MAX_CURVE_POINTS)
        );
    }

    #[test]
    fn serde_roundtrip_default_and_edited() {
        let mut s = DevelopSettings::default();
        controls::set(&mut s, "light.exposure", 1.25);
        s.masks.push(Mask {
            id: 1,
            components: vec![MaskComponent {
                anchor: None,
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Radial { center: lcc_geom::Point::new(0.5, 0.5), rx: 0.2, ry: 0.1, angle: 10.0, feather: 50.0, invert: false },
            }],
            ..Default::default()
        });
        let v = s.to_json();
        let back = DevelopSettings::from_json(&v).unwrap();
        assert_eq!(back, s);
        assert_ne!(back.hash64(), DevelopSettings::default().hash64());
    }

    #[test]
    fn a_masks_local_curve_is_written_only_when_it_has_points() {
        use lcc_geom::Point;
        let mut s = DevelopSettings::default();
        s.masks.push(Mask { id: 1, ..Default::default() });
        // no curve: the mask's JSON (and so `hash64`) is what it was before masks had one
        let plain = s.to_json();
        assert!(plain["masks"][0].get("curve").is_none());
        let old = json!({"masks": [{"id": 1, "name": "Mask 1", "visible": true, "invert": false, "components": [], "adjust": {}}]});
        assert_eq!(DevelopSettings::from_json(&old).unwrap().masks[0].curve, PointCurves::default());
        // a curve is kept channel by channel, and only the channels that have points are written
        s.masks[0].curve.master = vec![Point::new(0.0, 0.0), Point::new(0.5, 0.65), Point::new(1.0, 1.0)];
        s.masks[0].curve.blue = vec![Point::new(0.0, 0.1), Point::new(1.0, 1.0)];
        let v = s.to_json();
        assert_eq!(v["masks"][0]["curve"].as_object().map(|o| o.len()), Some(2));
        let back = DevelopSettings::from_json(&v).unwrap();
        assert_eq!(back, s);
        assert_ne!(back.hash64(), DevelopSettings::from_json(&plain).unwrap().hash64());
        // points on the diagonal are linear but still kept as the editor placed them
        let mut diag = PointCurves { red: vec![Point::new(0.0, 0.0), Point::new(0.4, 0.4), Point::new(1.0, 1.0)], ..Default::default() };
        assert!(diag.is_identity() && !diag.is_empty());
        diag.channel_mut("rgb").unwrap().push(Point::new(0.2, 0.3));
        assert!(!diag.is_identity());
        assert!(diag.channel("alpha").is_none());
    }

    #[test]
    fn white_balance_scale_reads_absent_as_legacy_and_writes_only_kelvin() {
        // settings from before the scale existed: no field, `Legacy`, and written back without it
        let old = json!({"wb": {"mode": "custom", "temp": 5800.0, "tint": 3.0}});
        let s = DevelopSettings::from_json(&old).unwrap();
        assert_eq!(s.wb.scale, WbScale::Legacy);
        assert_eq!(s.to_json()["wb"], old["wb"]);
        assert_eq!(DevelopSettings::default().wb.scale, WbScale::Legacy);
        assert!(DevelopSettings::default().to_json()["wb"].get("scale").is_none());
        // written on the Kelvin scale: said so, and read back
        let mut k = s.clone();
        k.wb.scale = WbScale::Kelvin;
        assert_eq!(k.to_json()["wb"]["scale"], json!("kelvin"));
        assert_eq!(DevelopSettings::from_json(&k.to_json()).unwrap(), k);
        assert_ne!(k.hash64(), s.hash64());
        // an unknown scale from a newer build is an error, not a panic
        assert!(DevelopSettings::from_json(&json!({"wb": {"scale": "mired"}})).is_err());
    }

    #[test]
    fn a_partial_white_balance_without_a_scale_is_legacy() {
        let mut base = DevelopSettings::default();
        base.wb.scale = WbScale::Kelvin;
        // an old preset's white balance lands on Kelvin settings: it keeps its relative meaning
        let out = apply_partial(&base, &json!({"wb": {"mode": "custom", "temp": 5800.0}}), 1.0);
        assert_eq!((out.wb.scale, out.wb.temp), (WbScale::Legacy, 5800.0));
        let out = apply_partial(&base, &json!({"wb": {"tint": 12.0}}), 1.0);
        assert_eq!(out.wb.scale, WbScale::Legacy);
        // one that says Kelvin stays Kelvin; a mode alone leaves the scale as it was
        let out = apply_partial(&base, &json!({"wb": {"mode": "custom", "temp": 5800.0, "scale": "kelvin"}}), 1.0);
        assert_eq!(out.wb.scale, WbScale::Kelvin);
        let out = apply_partial(&base, &json!({"wb": {"mode": "cloudy"}}), 1.0);
        assert_eq!(out.wb.scale, WbScale::Kelvin);
        let out = apply_partial(&base, &json!({"light": {"exposure": 1.0}}), 1.0);
        assert_eq!(out.wb.scale, WbScale::Kelvin);
        // the plain merge (`develop.merge`) follows the same rule
        assert_eq!(base.merged(&json!({"wb": {"temp": 5800.0}})).unwrap().wb.scale, WbScale::Legacy);
        assert_eq!(base.merged(&json!({"wb": {"temp": 5800.0, "scale": "kelvin"}})).unwrap().wb.scale, WbScale::Kelvin);
        assert_eq!(base.merged(&json!({"wb": {"mode": "auto"}})).unwrap().wb.scale, WbScale::Kelvin);
    }

    #[test]
    fn default_hdr_is_not_serialized_so_existing_hashes_stay() {
        let s = DevelopSettings::default();
        assert!(s.to_json().get("hdr").is_none(), "HDR off must not change the settings JSON or hash64");
        let mut on = s.clone();
        on.hdr.enabled = true;
        assert!(on.to_json().get("hdr").is_some());
        assert_ne!(on.hash64(), s.hash64());
        assert_eq!(DevelopSettings::from_json(&on.to_json()).unwrap(), on);
    }

    #[test]
    fn hdr_peak_is_finite_for_a_non_finite_headroom() {
        let mut h = Hdr { enabled: true, max_ev: f64::NAN, ..Hdr::default() };
        assert_eq!(h.peak(), (Hdr::DEFAULT_MAX_EV as f32).exp2());
        h.max_ev = f64::INFINITY;
        assert_eq!(h.peak(), (Hdr::DEFAULT_MAX_EV as f32).exp2());
        h.max_ev = -3.0;
        assert_eq!(h.peak(), 1.0);
    }

    #[test]
    fn sdr_blend_compresses_or_clips_the_sdr_rendition_and_round_trips() {
        let mut s = DevelopSettings::default();
        s.hdr.enabled = true;
        s.hdr.sdr_blend = 50.0;
        let r = s.sdr_rendition().into_owned();
        assert!(r.light.highlights < 0.0 && r.light.whites < 0.0, "positive blend fits more highlights in");
        assert_eq!(DevelopSettings::from_json(&s.to_json()).unwrap(), s, "lc:settings round trip");
        s.hdr.max_ev = 5.0;
        assert!(s.sdr_rendition().light.highlights < r.light.highlights, "more headroom, stronger compression");
        s.hdr.sdr_blend = -50.0;
        assert!(s.sdr_rendition().light.highlights > 0.0);
        s.hdr.sdr_blend = f64::NAN;
        assert_eq!(s.sdr_rendition().light.highlights, 0.0, "damaged blend is ignored");
        s.hdr.enabled = false;
        s.hdr.sdr_blend = 80.0;
        assert_eq!(s.sdr_rendition().light.highlights, 0.0, "HDR off: no SDR rendition");
    }

    #[test]
    fn missing_fields_take_defaults_and_unknown_ignored() {
        let s = DevelopSettings::from_json(&json!({"light": {"exposure": 0.5}, "futureThing": 3})).unwrap();
        assert_eq!(s.light.exposure, 0.5);
        assert_eq!(s.grading.blending, 50.0);
    }

    #[test]
    fn merge_partial() {
        let s = DevelopSettings::default();
        let m = s.merged(&json!({"light": {"contrast": 20}, "effects": {"clarity": 10}})).unwrap();
        assert_eq!(m.light.contrast, 20.0);
        assert_eq!(m.effects.clarity, 10.0);
        assert_eq!(m.light.exposure, 0.0);
    }

    #[test]
    fn merged_settings_are_kept_in_range() {
        // issue #1946: `develop.merge`, `render --settings` and presets stored whatever the JSON said
        let mut base = DevelopSettings::default();
        base.crop.geometry.rect = lcc_geom::Rect::new(0.1, 0.2, 0.8, 0.9);
        let m = base
            .merged(&json!({"light": {"exposure": 1e308}, "vignette": {"amount": -1e308}, "geometry": {"scale": 0}, "optics": {"distortion": 1e308}}))
            .unwrap();
        assert_eq!((m.light.exposure, m.vignette.amount, m.geometry.scale, m.optics.distortion), (5.0, -100.0, 50.0, 100.0));
        // point curves stay in 0..1, as an imported XMP curve does
        let m = base.merged(&json!({"curve": {"master": [{"x": -3, "y": 0}, {"x": 1, "y": 1e308}]}})).unwrap();
        assert_eq!(m.curve.master, vec![lcc_geom::Point::new(0.0, 0.0), lcc_geom::Point::new(1.0, 1.0)]);
        // a crop with no area is left out, as XMP import leaves out one whose edges are inverted
        for rect in
            [json!({"x0": 0.9, "x1": 0.1, "y0": 0, "y1": 1}), json!({"x0": 1e308, "x1": -1e308, "y0": 0, "y1": 1}), json!({"x0": 0.5, "x1": 0.5})]
        {
            let m = base.merged(&json!({"crop": {"geometry": {"rect": rect}}})).unwrap();
            assert_eq!(m.crop.geometry.rect, base.crop.geometry.rect, "{rect}");
        }
        // unrotated, the crop stays inside the photo
        let m = base.merged(&json!({"crop": {"geometry": {"rect": {"x0": -1e308, "y0": 0.25, "x1": 1e308, "y1": 0.75}}}})).unwrap();
        assert_eq!(m.crop.geometry.rect, lcc_geom::Rect::new(0.0, 0.25, 1.0, 0.75));
    }

    #[test]
    fn unedited_and_reset() {
        let mut s = DevelopSettings::default();
        s.wb.temp = 5000.0;
        assert!(s.is_unedited());
        s.light.exposure = 1.0;
        s.light.shadows = 30.0;
        assert!(!s.is_unedited());
        s.reset_section(Section::Light);
        assert!(s.is_unedited());
    }

    #[test]
    fn the_denoise_amount_is_a_slider_and_follows_the_detail_section() {
        // V5's straight mix (V6's eased response: `tests_process`)
        let mut s = DevelopSettings { process: Process::V5.version(), ..DevelopSettings::default() };
        assert_eq!(s.denoise_amount(), 0.0);
        assert!(controls::set(&mut s, "enhance.denoise", 60.0));
        assert_eq!(controls::get(&s, "enhance.denoise"), Some(60.0));
        assert!((s.denoise_amount() - 0.6).abs() < 1e-6);
        // the slider's range is 0 to 100; values outside it are brought in
        controls::set(&mut s, "enhance.denoise", 500.0);
        assert_eq!(s.denoise_amount(), 1.0);
        controls::set(&mut s, "enhance.denoise", -5.0);
        assert_eq!(s.denoise_amount(), 0.0);
        // switching the Detail section off switches the denoise off with it; a settings file with a bad number does nothing
        controls::set(&mut s, "enhance.denoise", 80.0);
        s.set_section_enabled("detail", false);
        assert_eq!(s.denoise_amount(), 0.0);
        s.set_section_enabled("detail", true);
        s.enhance.denoise = f64::NAN;
        assert_eq!(s.denoise_amount(), 0.0);
        // it is part of Detail: a reset of the section clears it
        controls::set(&mut s, "enhance.denoise", 40.0);
        s.reset_section(Section::Detail);
        assert_eq!(s.enhance.denoise, 0.0);
        // The later independent switch preserves Amount when off, and can be on at zero.
        controls::set(&mut s, "enhance.denoise", 75.0);
        s.enhance.denoise_on = Some(false);
        assert_eq!(s.denoise_amount(), 0.0);
        assert_eq!(s.enhance.denoise, 75.0);
        s.enhance.denoise_on = Some(true);
        assert_eq!(s.denoise_amount(), 0.75);
        controls::set(&mut s, "enhance.denoise", 0.0);
        assert!(s.enhance.denoise_enabled());
        s.reset_section(Section::Detail);
        assert_eq!(s.enhance.denoise_on, None);
    }

    #[test]
    fn next_mask_id_after_the_highest_id_is_unused() {
        // ids come from files, pastes and agents: one at the limit must not overflow (a panic in
        // debug builds) or wrap to an id another mask has
        let mut s = DevelopSettings::default();
        assert_eq!(s.next_mask_id(), 1);
        s.masks = vec![Mask { id: u32::MAX, ..Default::default() }, Mask { id: 1, ..Default::default() }];
        let next = s.next_mask_id();
        assert!(!s.masks.iter().any(|m| m.id == next), "{next}");
        assert_eq!(next, 2);
    }
}
