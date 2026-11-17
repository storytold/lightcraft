//! Monitor (display) ICC profiles: showing photos correctly on wide-gamut and calibrated displays.
//!
//! The loupe renders straight into the display's own primaries: the develop pipeline targets the
//! display's linear RGB ([`DisplayProfile::to_rec2020`]) and encodes it with the sRGB curve, as it
//! does for every 8-bit output. That "proxy" encoding then goes to the display's real device
//! values with [`DisplayProfile::correct`]: its tone curves for a matrix/TRC profile (a per-channel
//! table), or its LUTs for a LUT-based profile (a CMS transform). Images made for sRGB (thumbnails,
//! cached previews, embedded camera JPEGs) go through [`DisplayProfile::from_srgb`] instead.
//!
//! All conversions are relative colorimetric: the display's white is the image's white.

use crate::icc;
use crate::space::Trc;
use craft_cms::{Builtin, ColorSpace, Intent, Pcs, Profile, Transform, TransformOptions};
use lightcraft_color::{D50, D65, Mat3, REC2020, bradford};
use lightcraft_raster::Rgba8;

/// Largest profile file accepted (monitor profiles are a few KB; LUT profiles up to a few MB).
pub const MAX_PROFILE_BYTES: usize = 32 << 20;

/// How a display profile describes the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayKind {
    /// Primaries + tone curves (exact).
    MatrixTrc,
    /// LUT-based (A2B/B2A tables): applied through the CMS; the primaries the pipeline targets are
    /// the profile's matrix tags when it has them, else measured through its tables.
    Lut,
}

impl DisplayKind {
    pub fn label(self) -> &'static str {
        match self {
            DisplayKind::MatrixTrc => "matrix",
            DisplayKind::Lut => "lut",
        }
    }
}

/// The proxy → device step of a display.
enum Correction {
    /// The proxy encoding is the display's (sRGB-curve matrix profile): nothing to do.
    None,
    /// Per-channel tables (matrix/TRC profile: same primaries, other curves).
    Curves(Box<[[u8; 256]; 3]>),
    /// A full CMS transform (LUT-based profile).
    Cms(Box<Transform>),
}

/// A parsed display profile with its transforms (built once, applied to every preview).
pub struct DisplayProfile {
    /// The profile's own description (or "Display profile").
    pub description: String,
    /// Identifies the profile (hash of its bytes): part of render keys.
    pub id: u64,
    pub kind: DisplayKind,
    /// Linear display RGB → linear Rec.2020 (D65; display white → (1, 1, 1)).
    pub to_rec2020: Mat3,
    correction: Correction,
    from_srgb: Transform,
    to_srgb: Transform,
}

impl std::fmt::Debug for DisplayProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplayProfile")
            .field("description", &self.description)
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("to_rec2020", &self.to_rec2020)
            .finish_non_exhaustive()
    }
}

fn opts() -> TransformOptions {
    TransformOptions { intent: Intent::RelativeColorimetric, bpc: false, precise_float: true }
}

/// Linear Rec.2020 → XYZ relative to D50 (the ICC PCS), Bradford-adapted.
fn rec2020_to_xyz_d50() -> Mat3 {
    bradford(D65, D50).mul(&REC2020.to_xyz())
}

/// Scale `m`'s columns so that (1, 1, 1) maps to (1, 1, 1): the display's white is the image's.
fn white_balanced(m: Mat3) -> Option<Mat3> {
    let s = m.inverse()?.apply([1.0; 3]);
    s.iter().all(|v| v.is_finite() && *v > 0.0).then(|| m.mul(&Mat3::diag(s[0], s[1], s[2])))
}

fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish() | 1
}

impl DisplayProfile {
    /// Parse a display profile (an RGB ICC profile, matrix/TRC or LUT-based) and build its
    /// transforms. Errors say what is wrong with the file; nothing here panics.
    pub fn from_icc(bytes: &[u8]) -> Result<DisplayProfile, String> {
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err(format!("the profile is too large ({} MB)", bytes.len() >> 20));
        }
        let p = Profile::parse(bytes).map_err(|_| "not a valid ICC profile")?;
        if p.color_space != ColorSpace::Rgb {
            return Err("not an RGB profile (a display profile describes an RGB device)".into());
        }
        if !matches!(p.pcs, Pcs::Xyz | Pcs::Lab) {
            return Err("unsupported profile connection space".into());
        }
        let info = icc::parse(bytes).ok_or("not a valid ICC profile")?;
        let description = info
            .description
            .as_deref()
            .map(|d| d.trim_start_matches('\u{feff}').trim())
            .filter(|d| !d.is_empty())
            .unwrap_or("Display profile")
            .to_string();
        let has_lut = p.a2b[0].is_some() || p.a2b[1].is_some() || p.b2a[0].is_some() || p.b2a[1].is_some();
        let srgb = Builtin::Srgb.profile().clone();

        // the primaries the pipeline renders into
        let (kind, to_xyz_d50, curves) = match &info.kind {
            icc::IccKind::MatrixTrc { to_xyz_d50, trc } if !has_lut => (DisplayKind::MatrixTrc, *to_xyz_d50, Some(trc.clone())),
            icc::IccKind::MatrixTrc { to_xyz_d50, .. } => (DisplayKind::Lut, *to_xyz_d50, None),
            _ => (DisplayKind::Lut, measured_to_xyz_d50(&p)?, None),
        };
        let to_rec2020 =
            rec2020_to_xyz_d50().inverse().map(|m| m.mul(&to_xyz_d50)).and_then(white_balanced).ok_or("the profile's primaries are degenerate")?;
        if to_rec2020.0.iter().flatten().any(|v| !v.is_finite()) {
            return Err("the profile's primaries are degenerate".into());
        }

        // the proxy: the display's primaries (as the pipeline targets them), sRGB curve
        let proxy_xyz = rec2020_to_xyz_d50().mul(&to_rec2020).0;
        let mut proxy = Builtin::Srgb.profile().clone();
        proxy.matrix = Some(proxy_xyz);

        let transform = |src: &Profile, dst: &Profile| Transform::with_options(src, dst, opts()).ok();
        let correction = match curves {
            Some(trc) if trc.iter().all(|t| t.approx_eq(&Trc::Srgb, 2e-4)) => Correction::None,
            Some(trc) => Correction::Curves(Box::new(curve_tables(&trc))),
            None => Correction::Cms(Box::new(transform(&proxy, &p).ok_or("the profile's tables can't be applied")?)),
        };
        let from_srgb = transform(&srgb, &p).ok_or("can't convert sRGB to this profile")?;
        let to_srgb = transform(&proxy, &srgb).ok_or("can't convert this profile to sRGB")?;
        Ok(DisplayProfile { description, id: hash(bytes), kind, to_rec2020, correction, from_srgb, to_srgb })
    }

    /// Does [`correct`](Self::correct) change anything (the display's curves aren't sRGB's)?
    pub fn needs_correction(&self) -> bool {
        !matches!(self.correction, Correction::None)
    }

    /// A render in the display's primaries with the sRGB curve (the pipeline's display target) →
    /// the display's device values, in place.
    pub fn correct(&self, img: &mut Rgba8) -> Result<(), String> {
        match &self.correction {
            Correction::None => Ok(()),
            Correction::Curves(t) => {
                let map = |p: &mut [u8; 4]| *p = [t[0][p[0] as usize], t[1][p[1] as usize], t[2][p[2] as usize], p[3]];
                #[cfg(feature = "parallel")]
                {
                    use rayon::prelude::*;
                    img.data.par_chunks_mut(1 << 14).for_each(|c| c.iter_mut().for_each(map));
                }
                #[cfg(not(feature = "parallel"))]
                img.data.iter_mut().for_each(map);
                Ok(())
            }
            Correction::Cms(x) => apply(x, img),
        }
    }

    /// An sRGB image → the display's device values, in place.
    pub fn from_srgb(&self, img: &mut Rgba8) -> Result<(), String> {
        apply(&self.from_srgb, img)
    }

    /// A render in the display's primaries with the sRGB curve → sRGB (colours outside sRGB are
    /// clipped): for histograms and cached previews, which stay sRGB.
    pub fn to_srgb(&self, img: &Rgba8) -> Result<Rgba8, String> {
        let mut out = img.clone();
        apply(&self.to_srgb, &mut out)?;
        Ok(out)
    }
}

/// Device RGB → PCS (XYZ D50) of the pure primaries, measured through the profile's tables
/// (LUT-only profiles have no colorant tags).
fn measured_to_xyz_d50(p: &Profile) -> Result<Mat3, String> {
    let lin = icc::linear_rec2020_profile();
    let x = Transform::with_options(p, &lin, opts()).map_err(|_| "the profile's tables can't be applied")?;
    let src = [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let mut dst = [0f32; 9];
    x.convert_f32(&src, 3, &mut dst, 3, false).map_err(|_| "the profile's tables can't be applied")?;
    let c = |i: usize| [dst[i] as f64, dst[i + 3] as f64, dst[i + 6] as f64];
    // columns: each primary in linear Rec.2020
    let m = Mat3([c(0), c(1), c(2)]);
    if m.0.iter().flatten().any(|v| !v.is_finite()) || m.determinant().abs() < 1e-6 {
        return Err("the profile's primaries are degenerate".into());
    }
    Ok(rec2020_to_xyz_d50().mul(&m))
}

/// sRGB-encoded 8-bit value → the display curve's code value, per channel.
fn curve_tables(trc: &[Trc; 3]) -> [[u8; 256]; 3] {
    let mut t = [[0u8; 256]; 3];
    for (k, tab) in t.iter_mut().enumerate() {
        for (i, v) in tab.iter_mut().enumerate() {
            let lin = lightcraft_color::transfer::srgb_to_linear(i as f32 / 255.0);
            *v = (from_linear(&trc[k], lin) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    }
    t
}

/// Inverse of [`Trc::to_linear`] on 0..1 (bisection: works for tables and every parametric form).
fn from_linear(t: &Trc, y: f32) -> f32 {
    let y = y.clamp(0.0, 1.0);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if t.to_linear(mid) < y {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

fn apply(x: &Transform, img: &mut Rgba8) -> Result<(), String> {
    const CHUNK: usize = 1 << 14; // pixels
    let src = img.data.clone();
    let run = |s: &[[u8; 4]], d: &mut [[u8; 4]]| x.convert_u8(s.as_flattened(), 4, d.as_flattened_mut(), 4, true).is_ok();
    #[cfg(feature = "parallel")]
    let ok = {
        use rayon::prelude::*;
        src.par_chunks(CHUNK).zip(img.data.par_chunks_mut(CHUNK)).all(|(s, d)| run(s, d))
    };
    #[cfg(not(feature = "parallel"))]
    let ok = src.chunks(CHUNK).zip(img.data.chunks_mut(CHUNK)).all(|(s, d)| run(s, d));
    if ok { Ok(()) } else { Err("display profile transform failed".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::NamedSpace;
    use lightcraft_color::{DISPLAY_P3, SRGB};

    fn img(px: &[[u8; 4]]) -> Rgba8 {
        Rgba8 { width: px.len(), height: 1, data: px.to_vec() }
    }

    fn close(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
        (0..4).all(|i| (a[i] as i32 - b[i] as i32).abs() <= tol)
    }

    #[test]
    fn srgb_display_changes_nothing() {
        let d = DisplayProfile::from_icc(&icc::write_named(NamedSpace::Srgb)).unwrap();
        assert_eq!(d.kind, DisplayKind::MatrixTrc);
        assert!(!d.needs_correction());
        for (i, row) in d.to_rec2020.0.iter().enumerate() {
            let want = SRGB.to_space(&REC2020).0[i];
            for k in 0..3 {
                assert!((row[k] - want[k]).abs() < 2e-3, "{:?}", d.to_rec2020);
            }
        }
        let px = [[0, 0, 0, 255], [255, 255, 255, 255], [200, 30, 90, 128], [17, 140, 251, 7]];
        let mut a = img(&px);
        d.correct(&mut a).unwrap();
        assert_eq!(a.data, px);
        let mut b = img(&px);
        d.from_srgb(&mut b).unwrap();
        for (x, y) in b.data.iter().zip(&px) {
            assert!(close(*x, *y, 1), "{x:?} {y:?}");
        }
    }

    /// On a Display P3 monitor, sRGB red is shown with less red drive than full; P3 green (out of
    /// sRGB) is reachable in the display's own primaries.
    #[test]
    fn p3_display() {
        let d = DisplayProfile::from_icc(&icc::write_named(NamedSpace::DisplayP3)).unwrap();
        assert!(!d.needs_correction(), "P3 uses the sRGB curve");
        let want = DISPLAY_P3.to_space(&REC2020).0;
        for i in 0..3 {
            for k in 0..3 {
                assert!((d.to_rec2020.0[i][k] - want[i][k]).abs() < 2e-3, "{:?}", d.to_rec2020);
            }
        }
        let mut red = img(&[[255, 0, 0, 255], [128, 128, 128, 255]]);
        d.from_srgb(&mut red).unwrap();
        assert!(red.data[0][0] < 245 && red.data[0][1] > 40, "{:?}", red.data[0]);
        assert!(close(red.data[1], [128, 128, 128, 255], 1), "grey stays grey: {:?}", red.data[1]);
        // the display's own pure green is outside sRGB: it clips there
        let srgb = d.to_srgb(&img(&[[0, 255, 0, 255]])).unwrap();
        assert!(srgb.data[0][0] < 10 && srgb.data[0][1] == 255, "{:?}", srgb.data[0]);
    }

    /// A gamma 2.2 display (Adobe RGB-like): the proxy's sRGB curve goes to the display's curve.
    #[test]
    fn gamma_display_uses_its_curve() {
        let bytes = icc::write_matrix_trc(&lightcraft_color::ADOBE_RGB, &Trc::Gamma(2.2));
        let d = DisplayProfile::from_icc(&bytes).unwrap();
        assert!(d.needs_correction());
        let mut a = img(&[[0, 0, 0, 255], [255, 255, 255, 255], [10, 128, 240, 9]]);
        d.correct(&mut a).unwrap();
        assert_eq!(a.data[0], [0, 0, 0, 255]);
        assert_eq!(a.data[1], [255, 255, 255, 255]);
        for (k, v) in [10u8, 128, 240].into_iter().enumerate() {
            let lin = lightcraft_color::transfer::srgb_to_linear(v as f32 / 255.0);
            let want = (lin.powf(1.0 / 2.2) * 255.0).round() as i32;
            assert!((a.data[2][k] as i32 - want).abs() <= 1, "{k}: {} vs {want}", a.data[2][k]);
        }
        assert_eq!(a.data[2][3], 9, "alpha kept");
    }

    #[test]
    fn rejects_bad_profiles() {
        assert!(DisplayProfile::from_icc(&[]).is_err());
        assert!(DisplayProfile::from_icc(&[0u8; 300]).is_err());
        let gray = Builtin::GrayGamma22.profile().to_bytes();
        assert!(DisplayProfile::from_icc(&gray).is_err());
        // truncated / corrupted real profiles never panic
        let good = icc::write_named(NamedSpace::DisplayP3);
        for n in [10, 128, 140, 200, good.len() - 1] {
            let _ = DisplayProfile::from_icc(&good[..n]);
        }
        for i in (0..good.len()).step_by(7) {
            let mut b = good.clone();
            b[i] ^= 0xa5;
            let _ = DisplayProfile::from_icc(&b);
        }
    }

    /// A LUT-based display profile with no colorant or TRC tags (its A2B/B2A tables describe a
    /// Display P3 panel with a gamma 2.2 response): the primaries are measured through the tables
    /// and every preview goes through the CMS.
    fn lut_only_p3_gamma22() -> Vec<u8> {
        use craft_cms::Curve;
        use craft_cms::pipeline::Stage;
        use craft_cms::profile::{Lut, LutKind};
        let m = crate::space::rgb_to_xyz_d50(&DISPLAY_P3).0;
        let inv = crate::space::rgb_to_xyz_d50(&DISPLAY_P3).inverse().unwrap().0;
        let flat = |m: [[f64; 3]; 3], scale: f64| -> Vec<f64> { m.iter().flatten().map(|&v| v * scale).collect() };
        let curves = |g: f64| vec![Curve::Gamma(g), Curve::Gamma(g), Curve::Gamma(g)];
        let ident = || vec![Curve::Identity, Curve::Identity, Curve::Identity];
        let a2b = Lut {
            kind: LutKind::Ab,
            inputs: 3,
            outputs: 3,
            stages: vec![
                Stage::Curves(curves(2.2)),
                Stage::Matrix { rows: 3, cols: 3, m: flat(m, 32768.0 / 65535.0), offset: vec![0.0; 3] },
                Stage::Curves(ident()),
            ],
        };
        let b2a = Lut {
            kind: LutKind::Ab,
            inputs: 3,
            outputs: 3,
            stages: vec![
                Stage::Curves(ident()),
                Stage::Matrix { rows: 3, cols: 3, m: flat(inv, 65535.0 / 32768.0), offset: vec![0.0; 3] },
                Stage::Curves(curves(1.0 / 2.2)),
            ],
        };
        let mut p = Builtin::Srgb.profile().clone();
        p.class = craft_cms::ProfileClass::Display;
        p.matrix = None;
        p.trc = None;
        p.a2b[0] = Some(a2b.clone());
        p.a2b[1] = Some(a2b);
        p.b2a[0] = Some(b2a.clone());
        p.b2a[1] = Some(b2a);
        p.with_encoded_bytes().to_bytes().to_vec()
    }

    #[test]
    fn lut_profile_goes_through_the_cms() {
        let bytes = lut_only_p3_gamma22();
        let parsed = Profile::parse(&bytes).unwrap();
        assert!(!parsed.is_matrix_shaper() && parsed.b2a[1].is_some(), "a LUT-only profile");
        let d = DisplayProfile::from_icc(&bytes).unwrap();
        assert_eq!(d.kind, DisplayKind::Lut);
        assert!(d.needs_correction());
        // the measured primaries are P3's
        let want = DISPLAY_P3.to_space(&REC2020).0;
        for i in 0..3 {
            for k in 0..3 {
                assert!((d.to_rec2020.0[i][k] - want[i][k]).abs() < 0.02, "{:?} vs {want:?}", d.to_rec2020);
            }
        }
        // the correction is the gamma 2.2 curve; grey stays grey
        let mut a = img(&[[0, 0, 0, 255], [255, 255, 255, 255], [128, 128, 128, 200]]);
        d.correct(&mut a).unwrap();
        assert!(close(a.data[0], [0, 0, 0, 255], 1) && close(a.data[1], [255, 255, 255, 255], 1), "{:?}", a.data);
        let lin = lightcraft_color::transfer::srgb_to_linear(128.0 / 255.0);
        let g = (lin.powf(1.0 / 2.2) * 255.0).round() as u8;
        assert!(close(a.data[2], [g, g, g, 200], 2), "{:?} vs {g}", a.data[2]);
        // sRGB red → less than full red drive on a P3 panel
        let mut red = img(&[[255, 0, 0, 255]]);
        d.from_srgb(&mut red).unwrap();
        assert!(red.data[0][0] < 245 && red.data[0][1] > 30, "{:?}", red.data[0]);
    }

    #[test]
    fn ids_identify_profiles() {
        let a = DisplayProfile::from_icc(&icc::write_named(NamedSpace::DisplayP3)).unwrap();
        let b = DisplayProfile::from_icc(&icc::write_named(NamedSpace::AdobeRgb)).unwrap();
        let a2 = DisplayProfile::from_icc(&icc::write_named(NamedSpace::DisplayP3)).unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(a.id, a2.id);
    }
}
