//! CIE 1976 L*a*b* relative to a D50 white, the reference white of ICC profile connection space
//! and of the Lab values photo editors show in their colour readouts.
//!
//! Sources: CIE 15:2004 (Colorimetry, §8.2.1: the L*a*b* equations with the ε = 216/24389 and
//! κ = 24389/27 constants of the exact rational form) and Bruce Lindbloom's published equations
//! for XYZ → Lab and Bradford adaptation. Own implementation.

use crate::{D50, RgbSpace, SRGB, bradford, transfer::srgb_to_linear};

/// ε of CIE 15:2004 in its exact rational form (216 / 24389).
const EPSILON: f64 = 216.0 / 24389.0;
/// κ of CIE 15:2004 in its exact rational form (24389 / 27).
const KAPPA: f64 = 24389.0 / 27.0;

fn f(t: f64) -> f64 {
    if t > EPSILON { t.cbrt() } else { (KAPPA * t + 16.0) / 116.0 }
}

/// XYZ (Y of the white = 1) relative to the D50 white → L*a*b* (L* 0..100 for colours inside
/// the white). Non-finite input gives black (0, 0, 0); out-of-range input (negative, above white)
/// is extended by the same formulas and never panics.
pub fn xyz_d50_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    if xyz.iter().any(|v| !v.is_finite()) {
        return [0.0; 3];
    }
    let w = D50.to_xyz();
    let (fx, fy, fz) = (f(xyz[0] / w[0]), f(xyz[1] / w[1]), f(xyz[2] / w[2]));
    let lab = [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)];
    if lab.iter().all(|v| v.is_finite()) { lab } else { [0.0; 3] }
}

/// Linear RGB in `space` → L*a*b* D50 (the space's white Bradford-adapted to D50, so the space's
/// white is L* 100, a* = b* = 0).
pub fn linear_to_lab(rgb: [f64; 3], space: &RgbSpace) -> [f64; 3] {
    let xyz = bradford(space.white, D50).apply(space.to_xyz().apply(rgb));
    xyz_d50_to_lab(xyz)
}

/// An 8-bit sRGB colour (sRGB curve) → L*a*b* D50.
pub fn srgb8_to_lab(rgb: [u8; 3]) -> [f64; 3] {
    let lin = rgb.map(|v| f64::from(srgb_to_linear(f32::from(v) / 255.0)));
    linear_to_lab(lin, &SRGB)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn white_and_black_are_the_ends_of_the_lightness_axis() {
        let w = srgb8_to_lab([255, 255, 255]);
        assert!(near(w, [100.0, 0.0, 0.0], 0.05), "{w:?}");
        let k = srgb8_to_lab([0, 0, 0]);
        assert!(near(k, [0.0, 0.0, 0.0], 1e-9), "{k:?}");
        // any neutral grey has no chroma
        let g = srgb8_to_lab([119, 119, 119]);
        assert!(g[1].abs() < 0.05 && g[2].abs() < 0.05 && (g[0] - 50.0).abs() < 1.0, "{g:?}");
    }

    #[test]
    fn srgb_primaries_match_published_d50_values() {
        // Lindbloom's calculator, sRGB, Bradford adaptation to D50
        for (rgb, lab) in [([255, 0, 0], [54.29, 80.80, 69.89]), ([0, 255, 0], [87.82, -79.29, 80.99]), ([0, 0, 255], [29.57, 68.29, -112.03])] {
            let got = srgb8_to_lab(rgb);
            assert!(near(got, lab, 0.6), "{rgb:?}: {got:?} vs {lab:?}");
        }
    }

    #[test]
    fn hostile_input_never_panics_and_stays_finite() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 1e300, -1e300, f64::MIN_POSITIVE] {
            let lab = linear_to_lab([v, 0.5, v], &SRGB);
            assert!(lab.iter().all(|x| x.is_finite()), "{v}: {lab:?}");
            let lab = xyz_d50_to_lab([v, v, v]);
            assert!(lab.iter().all(|x| x.is_finite()), "{v}: {lab:?}");
        }
    }
}
