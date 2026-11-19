//! sRGB transfer functions and luminance, including the fused forms the conversion runs on
//! (one `powf` where the naive chain would need two: the host meters instructions per sample).

/// How a host's samples are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Display-referred, sRGB transfer (8- and 16-bit documents).
    Srgb,
    /// Scene-linear (32-bit float documents).
    Linear,
}

impl Encoding {
    /// The usual encoding of a document of `depth` bits per sample.
    pub fn for_depth(depth: u32) -> Self {
        if depth >= 32 { Self::Linear } else { Self::Srgb }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "srgb" => Some(Self::Srgb),
            "linear" => Some(Self::Linear),
            _ => None,
        }
    }

    /// A sample in this encoding as linear light.
    pub fn decode(self, v: f32) -> f32 {
        match self {
            Self::Srgb => srgb_decode(v),
            Self::Linear => v,
        }
    }

    /// Three linear values as samples in this encoding.
    pub fn encode3(self, rgb: [f32; 3]) -> [f32; 3] {
        [self.encode(rgb[0]), self.encode(rgb[1]), self.encode(rgb[2])]
    }

    /// Linear light as a sample in this encoding.
    pub fn encode(self, v: f32) -> f32 {
        match self {
            Self::Srgb => srgb_encode(v),
            Self::Linear => v,
        }
    }
}

/// Below this encoded value the sRGB curve is linear.
const SRGB_KNEE_ENCODED: f32 = 0.04045;
/// Below this linear value the sRGB curve is linear.
const SRGB_KNEE_LINEAR: f32 = 0.003_130_8;

/// sRGB-encoded 0..1 to linear light (non-finite input reads as 0).
pub fn srgb_decode(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    let v = v.clamp(0.0, 1.0);
    if v <= SRGB_KNEE_ENCODED { v / 12.92 } else { libm::powf((v + 0.055) / 1.055, 2.4) }
}

/// `srgb_decode(v)^e` in one `powf`, for `v` in 0..1 and finite `e`.
pub fn srgb_decode_pow(v: f32, e: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    let v = v.clamp(0.0, 1.0);
    if v <= SRGB_KNEE_ENCODED { libm::powf(v / 12.92, e) } else { libm::powf((v + 0.055) / 1.055, 2.4 * e) }
}

/// Linear light 0..1 to sRGB-encoded (clamped; non-finite input reads as 0).
pub fn srgb_encode(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    let v = v.clamp(0.0, 1.0);
    if v <= SRGB_KNEE_LINEAR { v * 12.92 } else { 1.055 * libm::powf(v, 1.0 / 2.4) - 0.055 }
}

/// `srgb_encode(v^e)` in one `powf`, for `v` in 0..1 and `e > 0`; `knee` is
/// [`encode_pow_knee`]`(e)`, the `v` below which `v^e` is in the curve's linear part.
pub fn srgb_encode_pow(v: f32, e: f32, knee: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    let v = v.clamp(0.0, 1.0);
    if v <= knee { 12.92 * libm::powf(v, e) } else { 1.055 * libm::powf(v, e / 2.4) - 0.055 }
}

/// The `v` below which `v^e` falls in the sRGB curve's linear part.
pub fn encode_pow_knee(e: f32) -> f32 {
    if e.is_finite() && e > 0.0 { libm::powf(SRGB_KNEE_LINEAR, 1.0 / e) } else { SRGB_KNEE_LINEAR }
}

/// Rec. 709 luminance of a linear RGB triple.
pub fn luminance(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trips_and_pins_known_points() {
        for v in [0.0, 0.001, 0.04045, 0.1, 0.5, 0.9, 1.0] {
            assert!((srgb_decode(srgb_encode(v)) - v).abs() < 1e-6, "{v}");
        }
        assert!((srgb_encode(0.18) - 0.4613).abs() < 1e-3);
        assert!((srgb_decode(0.5) - 0.2140).abs() < 1e-3);
        assert_eq!(srgb_encode(f32::NAN), 0.0);
        assert_eq!(srgb_decode(2.0), 1.0);
    }

    #[test]
    fn fused_forms_match_the_chains() {
        for v in [0.0f32, 0.001, 0.02, 0.04045, 0.1, 0.5, 0.9, 1.0] {
            for e in [0.5, 1.0, 1.7, -1.6] {
                // Negative exponents blow up at 0; callers clamp first, as the test does.
                let v = if e < 0.0 { v.max(0.001) } else { v };
                let want = libm::powf(srgb_decode(v), e);
                let got = srgb_decode_pow(v, e);
                assert!((got - want).abs() / want.max(1.0) < 1e-4, "decode {v}^{e}: {got} vs {want}");
            }
            for e in [0.5, 1.0, 2.0] {
                let want = srgb_encode(libm::powf(v, e));
                let got = srgb_encode_pow(v, e, encode_pow_knee(e));
                assert!((got - want).abs() < 1e-5, "encode {v}^{e}: {got} vs {want}");
            }
        }
        assert_eq!(srgb_decode_pow(f32::NAN, 2.0), 0.0);
        assert_eq!(srgb_encode_pow(f32::INFINITY, 1.0, encode_pow_knee(1.0)), 0.0);
        assert_eq!(encode_pow_knee(f32::NAN), SRGB_KNEE_LINEAR);
    }

    #[test]
    fn depth_picks_encoding() {
        assert_eq!(Encoding::for_depth(8), Encoding::Srgb);
        assert_eq!(Encoding::for_depth(16), Encoding::Srgb);
        assert_eq!(Encoding::for_depth(32), Encoding::Linear);
        assert_eq!(Encoding::parse("linear"), Some(Encoding::Linear));
        assert_eq!(Encoding::parse("auto"), None);
    }
}
