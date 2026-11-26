//! The negative's characteristic curve, straight-line approximation.
//!
//! Density `D = D_base + γ · log10(E)` for scene exposure `E` relative to the exposure that just
//! starts to darken the film; what the scanner sees is the transmittance `T = 10^−D`. So the
//! relative exposure that produced a scan value `t` with film base `t_base` is
//! `E = (t_base / t)^(1/γ)`: 1 at the base (scene black), growing as the negative gets denser.

use crate::{NEGATIVE_GAMMA, color::Encoding};

/// Transmittance below which a sample reads as fully dense (avoids division by zero).
pub const MIN_TRANSMITTANCE: f32 = 1e-5;
/// The sRGB-encoded value of [`MIN_TRANSMITTANCE`] (in the curve's linear part).
const MIN_TRANSMITTANCE_SRGB: f32 = MIN_TRANSMITTANCE * 12.92;

/// Relative scene exposure (≥ 1) behind a linear scan value `t` of a negative with film base
/// `t_base` (both linear light, 0..1). Values brighter than the base read as the base.
pub fn exposure(t: f32, t_base: f32) -> f32 {
    let t_base = if t_base.is_finite() { t_base.max(MIN_TRANSMITTANCE) } else { MIN_TRANSMITTANCE };
    let t = if t.is_finite() { t.clamp(MIN_TRANSMITTANCE, t_base) } else { t_base };
    libm::powf(t_base / t, 1.0 / NEGATIVE_GAMMA)
}

/// `t_base^(1/γ)`: the factor that turns `t^(−1/γ)` into [`exposure`].
pub fn base_factor(t_base: f32) -> f32 {
    let t_base = if t_base.is_finite() { t_base.max(MIN_TRANSMITTANCE) } else { MIN_TRANSMITTANCE };
    libm::powf(t_base, 1.0 / NEGATIVE_GAMMA)
}

/// `factor · t^(−exponent)` for a scan value `v` in its host encoding: one `powf` per sample.
/// With `exponent = 1/γ` and `factor` the base's [`base_factor`] this is [`exposure`]; a
/// different exponent per channel normalises channels of unequal density range
/// ([`crate::Converter`]). Results below 1 read as 1 (brighter than the base); non-finite
/// input reads as the base.
pub fn exposure_encoded(v: f32, enc: Encoding, factor: f32, exponent: f32) -> f32 {
    exposure_encoded_raw(v, enc, factor, exponent).max(1.0)
}

/// [`exposure_encoded`] before its floor at 1: it keeps going below 1 past the base, so that a
/// table of it interpolates straight through the black point instead of across a corner.
pub fn exposure_encoded_raw(v: f32, enc: Encoding, factor: f32, exponent: f32) -> f32 {
    if !v.is_finite() {
        return 1.0;
    }
    let e = match enc {
        Encoding::Srgb => factor * color::srgb_decode_pow(v.max(MIN_TRANSMITTANCE_SRGB), -exponent),
        Encoding::Linear => factor * libm::powf(v.max(MIN_TRANSMITTANCE), -exponent),
    };
    if e.is_finite() { e } else { 1.0 }
}

use crate::color;

/// The inverse: the linear scan value a relative exposure `e` (≥ 1) leaves on a negative with
/// film base `t_base`. Used to synthesise negatives in tests and to reason about levels.
pub fn transmittance(e: f32, t_base: f32) -> f32 {
    transmittance_at(e, t_base, NEGATIVE_GAMMA)
}

/// [`transmittance`] for a channel whose density grows by `gamma` per log-exposure: a scan of
/// colour film, whose channels spread apart ([`crate::SCAN_SPREAD`]).
pub fn transmittance_at(e: f32, t_base: f32, gamma: f32) -> f32 {
    t_base * libm::powf(e.max(1.0), -gamma)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_is_unit_exposure_and_density_grows_with_exposure() {
        assert_eq!(exposure(0.5, 0.5), 1.0);
        assert_eq!(exposure(0.9, 0.5), 1.0, "brighter than the base clamps");
        let e1 = exposure(0.25, 0.5);
        let e2 = exposure(0.125, 0.5);
        assert!(e1 > 1.0 && e2 > e1);
        // Halving the transmittance (0.3 density) is 0.5 log-exposure at γ 0.6: ×2^(1/0.6) ≈ 3.17.
        assert!((e2 / e1 - 3.175).abs() < 0.01, "{e1} {e2}");
    }

    #[test]
    fn transmittance_inverts_exposure() {
        for e in [1.0, 2.0, 10.0, 64.0] {
            let t = transmittance(e, 0.6);
            assert!((exposure(t, 0.6) - e).abs() / e < 1e-4, "{e}");
        }
        assert!(exposure(0.0, 0.6).is_finite());
        assert!(exposure(f32::NAN, f32::NAN).is_finite());
    }

    #[test]
    fn encoded_form_matches_the_linear_one() {
        let base = 0.6;
        let k = base_factor(base);
        for t in [0.6, 0.3, 0.1, 0.02, 0.001, 0.0] {
            let want = exposure(t, base);
            let lin = exposure_encoded(t, Encoding::Linear, k, 1.0 / NEGATIVE_GAMMA);
            let srgb = exposure_encoded(color::srgb_encode(t), Encoding::Srgb, k, 1.0 / NEGATIVE_GAMMA);
            assert!((lin - want).abs() / want < 1e-3, "linear {t}: {lin} vs {want}");
            assert!((srgb - want).abs() / want < 2e-3, "srgb {t}: {srgb} vs {want}");
        }
        assert_eq!(exposure_encoded(0.9, Encoding::Linear, k, 1.0 / NEGATIVE_GAMMA), 1.0, "brighter than the base");
        assert_eq!(exposure_encoded(f32::NAN, Encoding::Srgb, k, 1.0 / NEGATIVE_GAMMA), 1.0);
        assert!(exposure_encoded(0.0, Encoding::Srgb, f32::INFINITY, 1.0 / NEGATIVE_GAMMA).is_finite());
    }
}
