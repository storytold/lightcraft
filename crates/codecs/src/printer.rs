//! Printing through a printer/paper ICC profile ("Color Management ▸ Profile" in the Print
//! module): a 3D table from sRGB-encoded photo colour to the printer's device RGB, built through
//! the CMS with the chosen rendering intent and, for relative colorimetric, optional black point
//! compensation.
//!
//! - The table is sampled on a `PRINT_GRID³` lattice of sRGB-encoded input (the photos the print
//!   module receives are sRGB renders) and applied with trilinear interpolation, at 8 or 16 bits.
//! - **Black point compensation** (relative colorimetric only; perceptual tables already map
//!   black): the device's darkest neutral is found by taking device black back through the
//!   profile, and the source is scaled linearly in light so that source black lands on it
//!   (`c' = Yb + (1 − Yb)·c` per linear Rec.2020 channel, white unchanged). Shadow detail then
//!   compresses into the printable range instead of clipping.
//! - Only RGB (and grey) printer profiles are printable: photo printer drivers take RGB. A CMYK
//!   profile is an error that says so (it can still be soft-proofed).
//!
//! Sources: ICC.1:2022 (rendering intents); the published description of black point
//! compensation (Adobe, "Adobe Systems' Implementation of Black Point Compensation", 2006: a linear
//! scaling in XYZ mapping source black to destination black), applied here as a neutral scaling in
//! linear RGB; the transforms are moxcms's. Own design otherwise.

use lcc_color::{REC2020, SRGB};
use moxcms::{ColorProfile, DataColorSpace, Layout, RenderingIntent, TransformOptions};

use crate::display::MAX_PROFILE_BYTES;
pub use crate::proof::ProofIntent as PrintIntent;

/// Lattice size of [`PrinterTransform`]s.
pub const PRINT_GRID: usize = 33;

/// sRGB-encoded RGB → printer device RGB (both 0..1).
#[derive(Clone, Debug, PartialEq)]
pub struct PrinterTransform {
    n: usize,
    data: Vec<[f32; 3]>,
    /// The profile's description.
    pub description: Option<String>,
    /// The device black (linear Rec.2020 luminance) black point compensation mapped to; 0 when off.
    pub black_point: f32,
}

fn guarded<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok().flatten()
}

impl PrinterTransform {
    /// The transform into the printer profile `bytes`. Errors say what's wrong with the file.
    pub fn new(bytes: &[u8], intent: PrintIntent, black_point_compensation: bool) -> Result<PrinterTransform, String> {
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err("the profile is too large".into());
        }
        let dev = guarded(|| ColorProfile::new_from_slice(bytes).ok()).ok_or("not a valid ICC profile")?;
        let (layout, ch) = match dev.color_space {
            DataColorSpace::Rgb => (Layout::Rgb, 3),
            DataColorSpace::Gray => (Layout::Gray, 1),
            DataColorSpace::Cmyk => return Err("a CMYK profile can be soft-proofed but not printed with: choose the printer's RGB profile".into()),
            _ => return Err("only RGB and grey printer profiles can be printed with".into()),
        };
        let lin = crate::icc::linear_rec2020_profile();
        let opts = |i| TransformOptions { rendering_intent: i, ..TransformOptions::default() };
        let ri = match intent {
            PrintIntent::Perceptual => RenderingIntent::Perceptual,
            PrintIntent::Relative => RenderingIntent::RelativeColorimetric,
        };
        let fwd = guarded(|| lin.create_transform_f32(Layout::Rgb, &dev, layout, opts(ri)).ok()).ok_or("can't convert to this profile")?;
        let black_point = if black_point_compensation && intent == PrintIntent::Relative {
            let back = guarded(|| dev.create_transform_f32(layout, &lin, Layout::Rgb, opts(RenderingIntent::RelativeColorimetric)).ok())
                .ok_or("can't convert from this profile")?;
            let src = vec![0f32; ch];
            let mut out = [0f32; 3];
            guarded(|| back.transform(&src, &mut out).ok()).ok_or("the profile's tables can't be applied")?;
            let l = REC2020.luma();
            let y = (l[0] as f32 * out[0] + l[1] as f32 * out[1] + l[2] as f32 * out[2]).clamp(0.0, 0.5);
            if y.is_finite() { y } else { 0.0 }
        } else {
            0.0
        };
        let to2020 = SRGB.to_space(&REC2020).to_f32();
        let n = PRINT_GRID;
        let grid: Vec<f32> = (0..n * n * n)
            .flat_map(|i| {
                let s = [i / (n * n), (i / n) % n, i % n].map(|k| lcc_color::transfer::srgb_to_linear(k as f32 / (n - 1) as f32));
                let r: [f32; 3] = std::array::from_fn(|j| (0..3).map(|k| to2020[j][k] * s[k]).sum::<f32>().max(0.0));
                r.map(|v| black_point + (1.0 - black_point) * v)
            })
            .collect();
        let mut device = vec![0f32; n * n * n * ch];
        guarded(|| fwd.transform(&grid, &mut device).ok()).ok_or("the profile's tables can't be applied")?;
        let data = (0..n * n * n)
            .map(|i| {
                let c = |k: usize| device.get(i * ch + k).copied().filter(|x| x.is_finite()).unwrap_or(0.0).clamp(0.0, 1.0);
                if ch == 1 { [c(0); 3] } else { [c(0), c(1), c(2)] }
            })
            .collect();
        let description = crate::icc::parse(bytes).and_then(|i| i.description);
        Ok(PrinterTransform { n, data, description, black_point })
    }

    /// Device RGB (0..1) of sRGB-encoded `rgb` (0..1).
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let m = (self.n - 1) as f32;
        let f = rgb.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) * m } else { 0.0 });
        let i0 = f.map(|v| (v.floor() as usize).min(self.n - 2));
        let t = [f[0] - i0[0] as f32, f[1] - i0[1] as f32, f[2] - i0[2] as f32];
        let mut acc = [0.0f32; 3];
        for corner in 0..8usize {
            let b = [corner >> 2 & 1, corner >> 1 & 1, corner & 1];
            let w: f32 = (0..3).map(|k| if b[k] == 1 { t[k] } else { 1.0 - t[k] }).product();
            let idx = ((i0[0] + b[0]) * self.n + i0[1] + b[1]) * self.n + i0[2] + b[2];
            if let Some(e) = self.data.get(idx) {
                for k in 0..3 {
                    acc[k] += e[k] * w;
                }
            }
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::NamedSpace;

    #[test]
    fn srgb_printer_profile_is_close_to_identity() {
        let t = PrinterTransform::new(&crate::icc::write_named(NamedSpace::Srgb), PrintIntent::Relative, false).unwrap();
        for v in [[0.0, 0.0, 0.0], [0.5, 0.5, 0.5], [1.0, 1.0, 1.0], [0.8, 0.2, 0.4]] {
            let o = t.apply(v);
            assert!((0..3).all(|k| (o[k] - v[k]).abs() < 0.02), "{v:?} → {o:?}");
        }
    }

    #[test]
    fn adobe_rgb_profile_moves_saturated_colours() {
        let t = PrinterTransform::new(&crate::icc::write_named(NamedSpace::AdobeRgb), PrintIntent::Perceptual, true).unwrap();
        let g = t.apply([0.0, 1.0, 0.0]);
        assert!(g[0] > 0.4 && g[2] > 0.1, "sRGB green is less saturated in Adobe RGB: {g:?}");
        assert_eq!(t.black_point, 0.0, "perceptual ignores black point compensation");
    }

    #[test]
    fn black_point_compensation_lifts_black_for_a_printer_with_dull_black() {
        // a matrix profile whose black is not zero: a grey TRC that starts above 0
        let mut p = ColorProfile::new_from_slice(&crate::icc::write_named(NamedSpace::Srgb)).unwrap();
        let curve = moxcms::ToneReprCurve::Lut((0..=255).map(|i| (3000 + i * (65535 - 3000) / 255) as u16).collect());
        p.red_trc = Some(curve.clone());
        p.green_trc = Some(curve.clone());
        p.blue_trc = Some(curve);
        let bytes = p.encode().unwrap();
        let off = PrinterTransform::new(&bytes, PrintIntent::Relative, false).unwrap();
        let on = PrinterTransform::new(&bytes, PrintIntent::Relative, true).unwrap();
        assert!(on.black_point > 0.0, "{}", on.black_point);
        // a deep shadow: clipped to device 0 without BPC, kept above it with
        let s = [0.03, 0.03, 0.03];
        assert!(on.apply(s)[1] >= off.apply(s)[1], "{:?} vs {:?}", on.apply(s), off.apply(s));
        assert!(on.apply([0.0; 3])[1] < 0.02);
    }

    #[test]
    fn cmyk_and_hostile_profiles_are_errors() {
        assert!(PrinterTransform::new(&[], PrintIntent::Perceptual, false).is_err());
        assert!(PrinterTransform::new(&[0u8; 400], PrintIntent::Relative, true).is_err());
        let good = crate::icc::write_named(NamedSpace::DisplayP3);
        for i in (0..good.len()).step_by(13) {
            let mut b = good.clone();
            b[i] ^= 0x5a;
            let _ = PrinterTransform::new(&b, PrintIntent::Relative, true);
        }
        let t = PrinterTransform::new(&good, PrintIntent::Relative, true).unwrap();
        let o = t.apply([f32::NAN, 2.0, -1.0]);
        assert!(o.iter().all(|v| v.is_finite()));
    }
}
