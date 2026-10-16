//! The non-destructive edit model of LightCraft.
//!
//! [`DevelopSettings`] is the complete description of a photo's look: pure, serializable data.
//! The pipeline evaluates it; the catalog stores it; presets, copy/paste, sync, versions and history
//! are operations on it. Numeric sliders are addressed by id through [`controls`].
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod controls;
pub mod point_curve;
pub mod presets;
pub mod segmask;
pub mod settings;
pub mod unsupported_edits;

pub use controls::{CONTROLS, ControlSpec, Section, Track};
pub use point_curve::{FunctionCurve, MAX_POINT_CURVE_POINTS, POINT_CURVE_LUT_SIZE, PointCurve, PointCurveError};
pub use presets::{Preset, PresetError, SettingsGroup, apply_partial, extract_groups, try_apply_partial};
pub use segmask::SegMask;
pub use settings::*;

use serde_json::Value;

impl DevelopSettings {
    /// Settings for a freshly imported raw file (Lightroom applies capture sharpening and colour
    /// noise reduction by default to raw files).
    pub fn for_raw(as_shot_temp: f64, as_shot_tint: f64) -> DevelopSettings {
        let mut s = DevelopSettings::default();
        s.wb.temp = as_shot_temp;
        s.wb.tint = as_shot_tint;
        s.detail.sharpen_amount = 40.0;
        s.detail.nr_color = 25.0;
        s
    }

    /// Minimum wire schema needed by the authored features; legacy markers remain unchanged.
    pub fn required_schema_version(&self) -> u32 {
        if self.wb.scale == WbScale::Kelvin {
            WB_SCALE_SCHEMA_VERSION
        } else if self.profile.imported.as_ref().is_some_and(|look| look.look_table.is_some()) {
            LOOK_TABLE_SCHEMA_VERSION
        } else if self.profile.imported.as_ref().is_some_and(|look| look.convert_to_grayscale) {
            PROFILE_TREATMENT_SCHEMA_VERSION
        } else if self.profile.imported.is_some() {
            IMPORTED_LOOK_SCHEMA_VERSION
        } else if [&self.curve.master, &self.curve.red, &self.curve.green, &self.curve.blue].iter().any(|c| c.is_function()) {
            POINT_CURVE_SCHEMA_VERSION
        } else {
            0
        }
    }

    /// Promote only settings that need a newer feature schema; never downgrade saved markers.
    pub fn normalize_schema(&mut self) {
        self.version = self.version.max(self.required_schema_version());
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn from_json(v: &Value) -> Result<DevelopSettings, serde_json::Error> {
        let mut settings: DevelopSettings = serde_json::from_value(v.clone())?;
        settings.normalize_schema();
        Ok(settings)
    }

    /// Merge a partial JSON object into these settings (fields not mentioned are kept). A white balance that
    /// names no [`WbScale`] keeps the meaning it always had (`Legacy`), whatever scale these settings were on: a
    /// partial from before the scale existed, or one that never said, must not become Kelvin by landing on
    /// Kelvin settings.
    pub fn merged(&self, partial: &Value) -> Result<DevelopSettings, serde_json::Error> {
        let mut v = serde_json::to_value(self)?;
        presets::deep_merge(&mut v, partial);
        let mut out = DevelopSettings::from_json(&v)?;
        if let Some(wb) = partial.get("wb").and_then(Value::as_object)
            && (wb.contains_key("temp") || wb.contains_key("tint"))
            && !wb.contains_key("scale")
        {
            out.wb.scale = WbScale::Legacy;
        }
        Ok(out)
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

    /// Compare edit values independently of the supported wire schema marker.
    pub fn same_values(&self, other: &Self) -> bool {
        let mut current = self.clone();
        current.version = other.version;
        current == *other
    }

    /// True if nothing differs from a fresh default (ignoring white balance "as shot" values, and
    /// the rendering process: that is not an edit).
    pub fn is_unedited(&self) -> bool {
        let mut a = self.clone();
        let d = DevelopSettings::default();
        a.wb = d.wb;
        a.process = d.process;
        a.version = d.version;
        a == d
    }

    pub fn section_enabled(&self, section: &str) -> bool {
        !self.disabled_sections.iter().any(|s| s == section)
    }

    /// Effective treatment, including a profile's independent monochrome conversion.
    /// Amount zero does not turn a monochrome profile into a color profile.
    pub fn is_bw(&self) -> bool {
        self.treatment == Treatment::Bw
            || self.profile.id == "lc.mono"
            || self.profile.id.starts_with("lc.bw.")
            || self.profile.imported_look().is_some_and(|look| look.convert_to_grayscale)
    }

    /// The settings as rendered: an Edit-panel section whose eye is off contributes nothing (issue
    /// #316). Light (with its tone curve), Color (white balance, presence, the mixers, colour grading
    /// and point colour) and Detail are reset to their defaults here; Effects, Optics, Geometry and
    /// Calibration the pipeline switches off where it applies them. Borrowed when every section is on.
    pub fn effective(&self) -> std::borrow::Cow<'_, DevelopSettings> {
        let off = |section: &str| !self.section_enabled(section);
        if !(off("light") || off("color") || off("detail")) {
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
        if off("detail") {
            d.reset_section(Section::Detail);
        }
        std::borrow::Cow::Owned(d)
    }

    /// How much of the AI-denoised picture to mix in, 0 (none) to 1 (all of it): the Denoise amount, unless the Detail
    /// section is switched off or the value is not a number.
    pub fn denoise_amount(&self) -> f32 {
        let a = self.enhance.denoise;
        if a.is_finite() && self.enhance.denoise_enabled() && self.section_enabled("detail") { (a / 100.0).clamp(0.0, 1.0) as f32 } else { 0.0 }
    }

    pub fn set_section_enabled(&mut self, section: &str, on: bool) {
        self.disabled_sections.retain(|s| s != section);
        if !on {
            self.disabled_sections.push(section.to_string());
        }
    }

    /// Reset every control in `section` to its default.
    pub fn reset_section(&mut self, section: Section) {
        for c in controls::in_section(section) {
            controls::set(self, c.id, c.default);
        }
        match section {
            Section::Curve => {
                let d = ToneCurve::default();
                self.curve = d;
            }
            Section::Color => self.wb.mode = WbMode::AsShot,
            Section::Detail => self.enhance.denoise_on = None,
            _ => {}
        }
    }

    /// Next free mask id.
    pub fn next_mask_id(&self) -> u32 {
        self.masks.iter().map(|m| m.id).max().map_or(1, |m| m + 1)
    }
}

#[cfg(test)]
mod tests_process;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serde_roundtrip_default_and_edited() {
        let mut s = DevelopSettings::default();
        controls::set(&mut s, "light.exposure", 1.25);
        s.masks.push(Mask {
            id: 1,
            components: vec![MaskComponent {
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Radial {
                    center: lightcraft_geom::Point::new(0.5, 0.5),
                    rx: 0.2,
                    ry: 0.1,
                    angle: 10.0,
                    feather: 50.0,
                    invert: false,
                },
            }],
            ..Default::default()
        });
        let v = s.to_json();
        let back = DevelopSettings::from_json(&v).unwrap();
        assert_eq!(back, s);
        assert_ne!(back.hash64(), DevelopSettings::default().hash64());

        s.profile.id = IMPORTED_LOOK_ID.into();
        s.profile.imported = Some(ImportedLook { contrast: 25.0, shadows: -15.0, ..Default::default() });
        // Catalogs serialize the struct directly; stale markers still cannot hide new features.
        let direct = serde_json::to_value(&s).unwrap();
        assert_eq!(direct["version"], IMPORTED_LOOK_SCHEMA_VERSION);
        let loaded: DevelopSettings = serde_json::from_value(direct).unwrap();
        s.normalize_schema();
        assert_eq!(loaded, s);
        assert_eq!(loaded.light.exposure, 1.25);
        assert_eq!(loaded.profile.imported_look(), s.profile.imported.as_ref());
        assert!(loaded.to_json()["profile"]["imported"].get("convert_to_grayscale").is_none());
        let legacy_wire = serde_json::to_string(&loaded).unwrap();
        let legacy: DevelopSettings = serde_json::from_str(&legacy_wire).unwrap();
        assert_eq!(serde_json::to_string(&legacy).unwrap(), legacy_wire);
        s.profile.imported.as_mut().unwrap().convert_to_grayscale = true;
        let direct = serde_json::to_value(&s).unwrap();
        assert_eq!(direct["version"], PROFILE_TREATMENT_SCHEMA_VERSION);
        assert_eq!(direct["profile"]["imported"]["convert_to_grayscale"], true);
        assert!(DevelopSettings::from_json(&direct).unwrap().is_bw());
        s.profile.imported = None;
        assert!(serde_json::to_value(&s).is_err());
        assert!(s.merged(&json!({"light":{"exposure":2.0}})).is_err());
    }

    #[test]
    fn white_balance_scale_reads_absent_as_legacy_and_writes_only_kelvin() {
        // settings from before the scale existed: no field, `Legacy`, and written back without it
        let old = json!({"wb": {"mode": "custom", "temp": 5800.0, "tint": 3.0}});
        let s = DevelopSettings::from_json(&old).unwrap();
        assert_eq!(s.wb.scale, WbScale::Legacy);
        assert_eq!(s.to_json()["wb"], old["wb"]);
        assert_eq!(s.to_json()["version"], 1);
        assert_eq!(DevelopSettings::default().wb.scale, WbScale::Legacy);
        assert!(DevelopSettings::default().to_json()["wb"].get("scale").is_none());
        // written on the Kelvin scale: said so, and read back
        let mut k = s.clone();
        k.wb.scale = WbScale::Kelvin;
        assert_eq!(k.to_json()["wb"]["scale"], json!("kelvin"));
        assert_eq!(k.to_json()["version"], WB_SCALE_SCHEMA_VERSION);
        k.normalize_schema();
        assert_eq!(k.version, WB_SCALE_SCHEMA_VERSION);
        assert_eq!(DevelopSettings::from_json(&k.to_json()).unwrap(), k);
        assert_ne!(k.hash64(), s.hash64());
        // an unknown scale from a newer build is an error, not a panic
        assert!(DevelopSettings::from_json(&json!({"wb": {"scale": "mired"}})).is_err());
    }

    #[test]
    fn a_partial_white_balance_without_a_scale_is_legacy() {
        let mut base = DevelopSettings::default();
        base.wb.scale = WbScale::Kelvin;
        base.wb.temp = 5100.0;
        base.wb.tint = -4.0;
        // an old preset's white balance lands on Kelvin settings: it keeps its relative meaning
        let out = apply_partial(&base, &json!({"wb": {"mode": "custom", "temp": 5800.0}}), 1.0);
        assert_eq!((out.wb.scale, out.wb.temp), (WbScale::Legacy, 5800.0));
        assert_eq!(out.wb.tint, base.wb.tint, "the omitted axis is preserved");
        assert_eq!(out.version, WB_SCALE_SCHEMA_VERSION, "merging never downgrades the saved schema");
        assert!(out.to_json()["wb"].get("scale").is_none());
        let out = apply_partial(&base, &json!({"wb": {"tint": 12.0}}), 1.0);
        assert_eq!(out.wb.scale, WbScale::Legacy);
        assert_eq!((out.wb.temp, out.wb.tint), (base.wb.temp, 12.0));
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
    fn missing_fields_take_defaults_and_unknown_ignored() {
        let s = DevelopSettings::from_json(&json!({"light": {"exposure": 0.5}, "futureThing": 3})).unwrap();
        assert_eq!(s.light.exposure, 0.5);
        assert_eq!(s.grading.blending, 50.0);
        assert_eq!(serde_json::to_value(Profile::default()).unwrap(), json!({"id":"lc.color","amount":100.0}));
        assert!(ImportedLook::default().validate().is_ok());
        for bad in [
            json!({"name":"x".repeat(MAX_IMPORTED_LOOK_TEXT_BYTES + 1)}),
            json!({"source_id":"bad\nidentifier"}),
            json!({"highlights":101.0}),
            json!({"clarity":null}),
            json!({"master":[{"x":0.0,"y":-0.1},{"x":1.0,"y":1.0}]}),
            json!({"unknown_table":[]}),
        ] {
            assert!(serde_json::from_value::<ImportedLook>(bad).is_err());
        }
        assert!(serde_json::from_value::<Profile>(json!({"id":IMPORTED_LOOK_ID})).is_err());
        assert!(serde_json::from_value::<Profile>(json!({"id":IMPORTED_LOOK_ID,"imported":null})).is_err());
        let invalid = ImportedLook { contrast: f64::NAN, ..Default::default() };
        assert!(serde_json::to_value(&invalid).is_err());
    }

    #[test]
    fn future_schema_is_rejected_by_direct_and_value_deserialization() {
        let future = json!({"version": SCHEMA_VERSION + 1, "light": {"exposure": 0.5}});
        assert!(DevelopSettings::from_json(&future).is_err());
        assert!(serde_json::from_value::<DevelopSettings>(future.clone()).is_err());
        assert!(serde_json::from_str::<DevelopSettings>(&future.to_string()).is_err());
        for legacy in [json!({}), json!({"version": 0}), json!({"version": SCHEMA_VERSION})] {
            assert!(serde_json::from_value::<DevelopSettings>(legacy).is_ok());
        }
    }

    #[test]
    fn merge_partial() {
        let s = DevelopSettings::default();
        let m = s.merged(&json!({"light": {"contrast": 20}, "effects": {"clarity": 10}})).unwrap();
        assert_eq!(m.light.contrast, 20.0);
        assert_eq!(m.effects.clarity, 10.0);
        assert_eq!(m.light.exposure, 0.0);

        let imported = s.merged(&json!({"profile":{"id":IMPORTED_LOOK_ID,"imported":{"name":"First","contrast":20.0}}})).unwrap();
        assert_eq!(imported.profile.imported_look().unwrap().contrast, 20.0);
        let amount = imported.merged(&json!({"profile":{"amount":75.0}})).unwrap();
        assert_eq!(amount.profile.imported, imported.profile.imported);
        let replaced = imported.merged(&json!({"profile":{"imported":{"name":"Second","clarity":10.0}}})).unwrap();
        assert_eq!(replaced.profile.imported_look().unwrap().contrast, 0.0);
        assert_eq!(replaced.profile.imported_look().unwrap().clarity, 10.0);
        for patch in [json!({"profile":{"id":"lc.color"}}), json!({"profile":{"id":"lc.color","imported":null}})] {
            let builtin = imported.merged(&patch).unwrap();
            assert_eq!(builtin.profile, Profile::default());
            assert!(builtin.profile.imported_look().is_none());
        }
        assert!(imported.merged(&json!({"profile":{"imported":null}})).is_err());
        assert_eq!(imported.profile.imported_look().unwrap().contrast, 20.0);
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
    fn section_toggles() {
        let mut s = DevelopSettings::default();
        assert!(s.section_enabled("effects"));
        s.set_section_enabled("effects", false);
        s.set_section_enabled("effects", false);
        assert!(!s.section_enabled("effects"));
        assert_eq!(s.disabled_sections.len(), 1);
        s.set_section_enabled("effects", true);
        assert!(s.section_enabled("effects"));
    }

    #[test]
    fn the_denoise_amount_is_a_slider_and_follows_the_detail_section() {
        let mut s = DevelopSettings::default();
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
    fn point_curve_identity_requires_full_domain_and_finite_knots() {
        use lightcraft_geom::Point;
        let points = |p: &[(f64, f64)]| p.iter().map(|&(x, y)| Point::new(x, y)).collect::<Vec<_>>();
        assert!(ToneCurve::point_curve_is_identity(&[]));
        assert!(ToneCurve::point_curve_is_identity(&points(&[(1.0, 1.0), (0.25, 0.25), (0.0, 0.0)])));
        assert!(!ToneCurve::point_curve_is_identity(&points(&[(0.25, 0.25), (0.75, 0.75)])));
        assert!(!ToneCurve::point_curve_is_identity(&points(&[(0.0, 0.0), (0.75, 0.75)])));
        assert!(!ToneCurve::point_curve_is_identity(&points(&[(0.25, 0.25), (1.0, 1.0)])));
        assert!(!ToneCurve::point_curve_is_identity(&points(&[(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)])));
        assert!(!ToneCurve::point_curve_is_identity(&points(&[(0.0, 0.0), (f64::NAN, f64::NAN), (1.0, 1.0)])));
    }
}
