//! The non-destructive edit model of LightCraft.
//!
//! [`DevelopSettings`] is the complete description of a photo's look: pure, serializable data.
//! The pipeline evaluates it; the catalog stores it; presets, copy/paste, sync, versions and history
//! are operations on it. Numeric sliders are addressed by id through [`controls`].
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod controls;
pub mod presets;
pub mod segmask;
pub mod settings;

pub use controls::{CONTROLS, ControlSpec, Section, Track};
pub use presets::{Preset, SettingsGroup, apply_partial, extract_groups};
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

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn from_json(v: &Value) -> Result<DevelopSettings, serde_json::Error> {
        serde_json::from_value(v.clone())
    }

    /// Merge a partial JSON object into these settings (fields not mentioned are kept).
    pub fn merged(&self, partial: &Value) -> Result<DevelopSettings, serde_json::Error> {
        let mut v = self.to_json();
        presets::deep_merge(&mut v, partial);
        DevelopSettings::from_json(&v)
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
        a == d
    }

    pub fn section_enabled(&self, section: &str) -> bool {
        !self.disabled_sections.iter().any(|s| s == section)
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
}
