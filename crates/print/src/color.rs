//! Print colour: Print Adjustment (brightness and contrast that change the print only, to make up
//! for prints that come out darker or flatter than the screen) and the conversion of the photos
//! into the printer profile ([`lcc_codecs::printer`]), at 8 or 16 bits per channel.
//!
//! Order: Print Adjustment on the display-encoded sRGB photo, then the printer profile. Both run
//! on the photos the engine rendered for the print, after print sharpening.
//!
//! Print Adjustment curve (on display-encoded 0..1 values, endpoints fixed, monotone):
//! brightness `b ∈ [−1, 1]` is a power curve `v^(2^(−b))`; contrast `k ∈ [−1, 1]` blends towards
//! (or away from) the S-curve `3v² − 2v³`: `v + k·(s(v) − v)`, whose slope `1 + k·(s′ − 1)` stays
//! positive for |k| ≤ 1.
//!
//! Sources: own design.

use std::sync::Arc;

use lcc_codecs::printer::PrinterTransform;
use lcc_layout::render::Rgb16;
use lcc_raster::Rgba8;

use crate::{PrintColor, PrintError, PrintIntent, Result};

/// Largest printer profile read (bytes).
pub const MAX_PROFILE_BYTES: u64 = lcc_codecs::display::MAX_PROFILE_BYTES as u64;

/// The Print Adjustment curve value of display-encoded `v` (0..1).
pub fn adjust_value(v: f32, brightness: f32, contrast: f32) -> f32 {
    let b = if brightness.is_finite() { (brightness / 100.0).clamp(-1.0, 1.0) } else { 0.0 };
    let k = if contrast.is_finite() { (contrast / 100.0).clamp(-1.0, 1.0) } else { 0.0 };
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let v = v.powf(2f32.powf(-b));
    let s = v * v * (3.0 - 2.0 * v);
    (v + k * (s - v)).clamp(0.0, 1.0)
}

/// Everything [`PrintColor`] does to a photo, ready to apply.
#[derive(Clone, Debug, Default)]
pub struct ColorJob {
    /// 256-entry 8-bit Print Adjustment table and its 16-bit form (4096 steps), when on.
    curve8: Option<Vec<u8>>,
    curve16: Option<Vec<f32>>,
    transform: Option<PrinterTransform>,
    /// The printer profile, to tag the photos with in the output.
    pub profile: Option<Arc<Vec<u8>>>,
    /// The printer profile's description.
    pub profile_name: Option<String>,
}

const CURVE16_STEPS: usize = 4096;

impl ColorJob {
    /// The job for `color`; `profile` is the printer profile's bytes when `color.profile` is set.
    pub fn new(color: &PrintColor, profile: Option<Vec<u8>>) -> Result<ColorJob> {
        let adjust = color.adjust && (color.brightness != 0.0 || color.contrast != 0.0);
        let (curve8, curve16) = if adjust {
            let c8 = (0..256).map(|i| (adjust_value(i as f32 / 255.0, color.brightness, color.contrast) * 255.0).round() as u8).collect();
            let c16 = (0..=CURVE16_STEPS).map(|i| adjust_value(i as f32 / CURVE16_STEPS as f32, color.brightness, color.contrast)).collect();
            (Some(c8), Some(c16))
        } else {
            (None, None)
        };
        let (transform, profile) = match profile {
            Some(bytes) => {
                let intent = match color.intent {
                    PrintIntent::Perceptual => lcc_codecs::printer::PrintIntent::Perceptual,
                    PrintIntent::Relative => lcc_codecs::printer::PrintIntent::Relative,
                };
                let t = PrinterTransform::new(&bytes, intent, color.black_point_compensation)
                    .map_err(|e| PrintError::Invalid(format!("printer profile: {e}")))?;
                (Some(t), Some(Arc::new(bytes)))
            }
            None => (None, None),
        };
        let profile_name = transform.as_ref().and_then(|t| t.description.clone());
        Ok(ColorJob { curve8, curve16, transform, profile, profile_name })
    }

    /// Reads the printer profile `color.profile` names (if any) and builds the job.
    pub fn load(color: &PrintColor) -> Result<ColorJob> {
        let bytes = match color.profile.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(path) => {
                let meta = std::fs::metadata(path).map_err(|e| PrintError::Invalid(format!("printer profile {path}: {e}")))?;
                if meta.len() > MAX_PROFILE_BYTES {
                    return Err(PrintError::Invalid(format!("printer profile {path}: too large for an ICC profile")));
                }
                Some(std::fs::read(path).map_err(|e| PrintError::Invalid(format!("printer profile {path}: {e}")))?)
            }
            None => None,
        };
        ColorJob::new(color, bytes)
    }

    /// `true` when applying changes nothing.
    pub fn is_identity(&self) -> bool {
        self.curve8.is_none() && self.transform.is_none()
    }

    /// Applies the adjustment and the profile to an 8-bit photo.
    pub fn apply8(&self, img: &mut Rgba8) {
        if self.is_identity() {
            return;
        }
        for p in &mut img.data {
            if let Some(c) = &self.curve8 {
                for v in &mut p[..3] {
                    *v = c.get(usize::from(*v)).copied().unwrap_or(*v);
                }
            }
            if let Some(t) = &self.transform {
                let o = t.apply([f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0]);
                for k in 0..3 {
                    p[k] = (o[k] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }

    /// Applies the adjustment and the profile to a 16-bit photo.
    pub fn apply16(&self, img: &mut Rgb16) {
        if self.is_identity() {
            return;
        }
        let curve = |v: f32| -> f32 {
            let Some(c) = &self.curve16 else { return v };
            let f = v.clamp(0.0, 1.0) * CURVE16_STEPS as f32;
            let i = (f.floor() as usize).min(CURVE16_STEPS - 1);
            let t = f - i as f32;
            match (c.get(i), c.get(i + 1)) {
                (Some(a), Some(b)) => a + (b - a) * t,
                _ => v,
            }
        };
        for px in img.data.as_chunks_mut::<3>().0 {
            let mut v = [0f32; 3];
            for k in 0..3 {
                v[k] = curve(f32::from(px[k]) / 65535.0);
            }
            if let Some(t) = &self.transform {
                v = t.apply(v);
            }
            for k in 0..3 {
                px[k] = (v[k] * 65535.0).round().clamp(0.0, 65535.0) as u16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjustment_curve_keeps_endpoints_and_is_monotone() {
        for (b, c) in [(0.0, 0.0), (50.0, 0.0), (-100.0, 0.0), (0.0, 100.0), (0.0, -100.0), (100.0, 100.0), (-100.0, -100.0)] {
            assert!(adjust_value(0.0, b, c) < 1e-6 && (adjust_value(1.0, b, c) - 1.0).abs() < 1e-6, "{b} {c}");
            let mut last = -1.0;
            for i in 0..=100 {
                let v = adjust_value(i as f32 / 100.0, b, c);
                assert!(v >= last - 1e-6, "{b} {c} at {i}");
                last = v;
            }
        }
        assert!(adjust_value(0.5, 50.0, 0.0) > 0.55, "brightness lifts midtones");
        assert!(adjust_value(0.25, 0.0, 50.0) < 0.25 && adjust_value(0.75, 0.0, 50.0) > 0.75, "contrast spreads");
        assert!(adjust_value(f32::NAN, f32::NAN, f32::INFINITY).is_finite());
    }

    #[test]
    fn job_adjusts_8_and_16_bit_alike_and_off_is_identity() {
        let off = ColorJob::new(&PrintColor { adjust: true, ..PrintColor::default() }, None).unwrap();
        assert!(off.is_identity());
        let c = PrintColor { adjust: true, brightness: 40.0, contrast: 20.0, ..PrintColor::default() };
        let job = ColorJob::new(&c, None).unwrap();
        let mut a = Rgba8::new(1, 1);
        a.data[0] = [100, 128, 200, 255];
        job.apply8(&mut a);
        let mut d = Rgb16 { width: 1, height: 1, data: vec![100 * 257, 128 * 257, 200 * 257] };
        job.apply16(&mut d);
        for k in 0..3 {
            assert!((i32::from(a.data[0][k]) - i32::from(d.data[k] / 257)).abs() <= 1, "{:?} vs {:?}", a.data[0], d.data);
        }
        assert!(a.data[0][1] > 128);
    }

    #[test]
    fn printer_profile_converts_and_bad_profiles_are_errors() {
        let p = lcc_codecs::icc::write_named(lcc_codecs::space::NamedSpace::AdobeRgb);
        let job = ColorJob::new(&PrintColor::default(), Some(p)).unwrap();
        assert!(job.profile.is_some());
        let mut a = Rgba8::new(1, 1);
        a.data[0] = [0, 255, 0, 255];
        job.apply8(&mut a);
        assert!(a.data[0][0] > 100 && a.data[0][2] > 30, "{:?}", a.data[0]);
        assert!(ColorJob::new(&PrintColor::default(), Some(b"nope".to_vec())).is_err());
        let missing = PrintColor { profile: Some("/nonexistent/printer.icc".into()), ..PrintColor::default() };
        assert!(ColorJob::load(&missing).is_err());
        assert!(ColorJob::load(&PrintColor::default()).unwrap().is_identity());
    }
}
