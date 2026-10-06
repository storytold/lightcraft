//! The global tone map: scene luminance → display-linear luminance.
//!
//! Built in the log domain around middle grey (0.18): contrast scales log-exposure about grey,
//! whites move the shoulder (white point), blacks move the toe. The shoulder is an extended
//! Reinhard curve so highlights roll off smoothly instead of clipping.
//!
//! Rendered (display-referred) sources such as JPEGs use [`ToneMap::display`] instead: identity at
//! neutral settings (an unedited JPEG renders exactly as the file), with contrast/whites/blacks as
//! S-curve adjustments in a gamma-2.2 perceptual domain and a short shoulder above 0.95.
//!
//! A DNG `ProfileToneCurve` ([`CameraTone::per_channel`]) is applied to RGB instead of luminance:
//! in linear ProPhoto RGB, the curve maps the largest and smallest channel, and the middle
//! channel keeps its relative position between them, so the hue holds. Contrast curves then add
//! saturation, as the file's maker intends. On Apple ProRAW this brings the default render much
//! closer to Apple's own embedded render than a luminance-only curve does (docs/parity.md,
//! LR-PROF-CAMERACOLOR).

use std::sync::LazyLock;

use lightcraft_color::{D50, D65, Mat3, PROPHOTO, REC2020, bradford};

pub const GREY: f32 = 0.18;
/// The tone LUT spans `LUT_MIN_EV..LUT_MAX_EV` around grey in `LUT_N` steps.
pub const LUT_MIN_EV: f32 = -14.0;
pub const LUT_MAX_EV: f32 = 10.0;
pub const LUT_N: usize = 4096;

/// A file-local camera look, fitted independently of the scene-linear colour transform.
/// Knots are scene/display-linear luminance pairs. Keeping this in the finish stage preserves
/// RAW exposure and highlight headroom; it is never baked into the decoded sensor pixels.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct CameraTone {
    knots: [[f32; 2]; 32],
    /// Applied per channel (hue-preserving, linear ProPhoto RGB) instead of on luminance.
    rgb: bool,
}

impl<'de> serde::Deserialize<'de> for CameraTone {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Wire {
            knots: [[f32; 2]; 32],
            #[serde(default)]
            rgb: bool,
        }
        let w = Wire::deserialize(d)?;
        let tone = Self::new(w.knots).ok_or_else(|| serde::de::Error::custom("invalid camera tone curve"))?;
        Ok(if w.rgb { tone.per_channel() } else { tone })
    }
}

impl CameraTone {
    pub fn new(knots: [[f32; 2]; 32]) -> Option<Self> {
        let mut previous = [0.0, 0.0];
        for p in knots {
            if !p.iter().all(|v| v.is_finite()) || p[0] <= previous[0] || p[1] < previous[1] || p[1] >= 1.0 {
                return None;
            }
            previous = p;
        }
        Some(Self { knots, rgb: false })
    }

    /// The same curve applied to RGB (hue-preserving, linear ProPhoto) instead of luminance; used
    /// for a DNG `ProfileToneCurve`.
    pub fn per_channel(self) -> Self {
        Self { rgb: true, ..self }
    }

    pub fn apply(&self, y: f32) -> f32 {
        if !y.is_finite() || y <= 0.0 {
            return 0.0;
        }
        let mut previous = [0.0, 0.0];
        for p in self.knots {
            if y <= p[0] {
                let t = (y - previous[0]) / (p[0] - previous[0]);
                return previous[1] + t * (p[1] - previous[1]);
            }
            previous = p;
        }
        let a = self.knots[30];
        let b = self.knots[31];
        // Extend beyond observed (unclipped) highlights with a continuous, bounded shoulder.
        let slope = ((b[1] - a[1]) / (b[0] - a[0])).clamp(0.1, 16.0);
        1.0 - (1.0 - b[1]) * (-(y - b[0]) * slope / (1.0 - b[1]).max(0.01)).exp()
    }
}

#[derive(Clone, Debug)]
pub struct ToneMap {
    lut: Vec<f32>,
    /// Rec.2020 → ProPhoto and back when applied per channel ([`CameraTone::per_channel`]).
    rgb: Option<([[f32; 3]; 3], [[f32; 3]; 3])>,
}

/// Linear Rec.2020 D65 → linear ProPhoto D50, and back (for [`ToneMap::apply_rgb`]).
static PROPHOTO_MATRICES: LazyLock<([[f32; 3]; 3], [[f32; 3]; 3])> = LazyLock::new(|| {
    let to: Mat3 = PROPHOTO.from_xyz().mul(&bradford(D65, D50)).mul(&REC2020.to_xyz());
    (to.to_f32(), to.inverse().unwrap_or(Mat3::IDENTITY).to_f32())
});

/// The matrices [`ToneMap::apply_rgb`] uses: linear Rec.2020 D65 → linear ProPhoto D50, and back.
pub fn prophoto_matrices() -> ([[f32; 3]; 3], [[f32; 3]; 3]) {
    *PROPHOTO_MATRICES
}

#[inline]
fn mul3(m: &[[f32; 3]; 3], c: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2])
}

impl ToneMap {
    pub fn camera(curve: &CameraTone, contrast: f64, whites: f64, blacks: f64) -> ToneMap {
        let adjustment = Self::display(contrast, whites, blacks);
        let neutral = contrast == 0.0 && whites == 0.0 && blacks == 0.0;
        let lut = (0..LUT_N)
            .map(|i| {
                let ev = LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32;
                let y = curve.apply(GREY * 2f32.powf(ev));
                if neutral { y } else { adjustment.apply(y) }
            })
            .collect();
        ToneMap { lut, rgb: curve.rgb.then(prophoto_matrices) }
    }
    /// `contrast`, `whites`, `blacks` in −100..100 (Lightroom slider units).
    pub fn new(contrast: f64, whites: f64, blacks: f64) -> ToneMap {
        let c = (contrast / 100.0) as f32;
        let slope = if c >= 0.0 { 1.0 + 0.55 * c } else { 1.0 + 0.4 * c };
        // White point: scene luminance (after contrast) that maps to display 1.0.
        let white_ev = 2.9 - 1.6 * (whites as f32 / 100.0);
        let wl = GREY * 2f32.powf(white_ev);
        let pre = 1.0 + GREY / wl; // keep grey near grey
        let b = (blacks / 100.0) as f32;
        let lut = (0..LUT_N)
            .map(|i| {
                let ev = LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32;
                let y = GREY * 2f32.powf(ev * slope) * pre;
                // extended Reinhard with white point wl: y(1 + y/wl²)/(1 + y)
                let mut o = y * (1.0 + y / (wl * wl)) / (1.0 + y);
                o = o.min(1.0);
                // Toe: blacks < 0 crushes, > 0 lifts.
                if b < 0.0 {
                    // Smooth max(0, o − k) (a soft knee), renormalized so 1 stays 1.
                    let k = -b * 0.035;
                    let e = 0.004;
                    let soft = |v: f32| ((v - k) + ((v - k) * (v - k) + e * e).sqrt()) * 0.5;
                    o = (soft(o) - soft(0.0)) / (soft(1.0) - soft(0.0));
                } else if b > 0.0 {
                    let k = b * 0.03;
                    o = k + (1.0 - k) * o;
                }
                o.clamp(0.0, 1.0)
            })
            .collect();
        ToneMap { lut, rgb: None }
    }

    /// Tone map for display-referred sources: identity at neutral settings.
    pub fn display(contrast: f64, whites: f64, blacks: f64) -> ToneMap {
        let c = (contrast / 100.0) as f32;
        let w = (whites / 100.0) as f32;
        let b = (blacks / 100.0) as f32;
        let m = GREY.powf(1.0 / 2.2);
        let lut = (0..LUT_N)
            .map(|i| {
                let ev = LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32;
                let y = GREY * 2f32.powf(ev);
                let mut p = y.powf(1.0 / 2.2);
                if p <= 1.0 {
                    // S-curve anchored at 0, grey and 1
                    p += c * 0.35 * (p - m) * (1.0 - (2.0 * p - 1.0).powi(2));
                    // whites: lift/lower the upper tones; blacks: the lower tones
                    let up = smooth(0.45, 1.0, p);
                    p += w * 0.12 * up * (1.0 - p * 0.5);
                    let lo = 1.0 - smooth(0.0, 0.45, p);
                    p += b * 0.07 * lo * (p * 2.0).min(1.0);
                } else {
                    p += w * 0.12 * 0.5;
                }
                let mut o = p.max(0.0).powf(2.2);
                // short shoulder: slope 1 at 0.95, reaching 1.0 at 1.05
                if o > 0.95 {
                    let d = (o - 0.95).min(0.1);
                    o = 0.95 + d - d * d / 0.2;
                }
                o.clamp(0.0, 1.0)
            })
            .collect();
        ToneMap { lut, rgb: None }
    }

    /// The table (`LUT_N` entries, see [`ToneMap::apply`]).
    pub fn lut(&self) -> &[f32] {
        &self.lut
    }

    /// Whether the curve is applied per channel ([`ToneMap::apply_rgb`]) rather than to luminance.
    pub fn per_channel(&self) -> bool {
        self.rgb.is_some()
    }

    /// Scene luminance → display-linear luminance.
    #[inline]
    pub fn apply(&self, y: f32) -> f32 {
        if y <= 0.0 {
            return 0.0;
        }
        let ev = (y / GREY).log2();
        let f = ((ev - LUT_MIN_EV) / (LUT_MAX_EV - LUT_MIN_EV)).clamp(0.0, 1.0) * (LUT_N - 1) as f32;
        let i = (f as usize).min(LUT_N - 2);
        let t = f - i as f32;
        let v = self.lut[i] + (self.lut[i + 1] - self.lut[i]) * t;
        if ev < LUT_MIN_EV { v * (y / (GREY * 2f32.powf(LUT_MIN_EV))) } else { v }
    }

    /// Scene-linear Rec.2020 → display-linear Rec.2020, per channel and hue-preserving: in linear
    /// ProPhoto RGB the largest and smallest channel go through the curve and the middle one keeps
    /// its relative position between them. A map built without [`CameraTone::per_channel`] has no
    /// matrices and applies the curve to each Rec.2020 channel the same way.
    #[inline]
    pub fn apply_rgb(&self, c: [f32; 3]) -> [f32; 3] {
        let p = match &self.rgb {
            Some((to, _)) => mul3(to, c),
            None => c,
        };
        let (hi, lo) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
        let (th, tl) = (self.apply(hi), self.apply(lo));
        let q = if hi - lo > 1e-9 { p.map(|v| tl + (th - tl) * (v - lo) / (hi - lo)) } else { [th; 3] };
        match &self.rgb {
            Some((_, from)) => mul3(from, q),
            None => q,
        }
    }
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_curve_preserves_black_and_extends_headroom() {
        let knots = std::array::from_fn(|i| {
            let x = 0.005 * 1.15f32.powi(i as i32);
            [x, 1.0 - (-3.0 * x).exp()]
        });
        let curve = CameraTone::new(knots).unwrap();
        let base = ToneMap::camera(&curve, 0.0, 0.0, 0.0);
        assert_eq!(base.apply(0.0), 0.0);
        assert!(base.apply(0.1) < base.apply(0.2));
        let mut previous = 0.0;
        for i in 0..2000 {
            let y = 1e-6 * 1.01f32.powi(i);
            let v = base.apply(y);
            assert!((0.0..=1.0).contains(&v));
            assert!(v >= previous - 1e-6);
            previous = v;
        }
        assert!((base.apply(0.1) - curve.apply(0.1)).abs() < 0.001);
        assert!(ToneMap::camera(&curve, 50.0, 0.0, 0.0).apply(0.3) > base.apply(0.3));
        let mut invalid = knots;
        invalid[1][0] = invalid[0][0];
        assert!(CameraTone::new(invalid).is_none());
        assert!(serde_json::from_value::<CameraTone>(serde_json::json!({"knots": invalid})).is_err());
    }

    /// A DNG profile tone curve applied per channel keeps greys on the curve, keeps the hue (in
    /// ProPhoto, where it is applied), adds the saturation a contrast curve gives, and survives
    /// serialisation (smart previews carry it); older serialised curves stay luminance curves.
    #[test]
    fn per_channel_camera_tone_keeps_hue_and_greys() {
        let knots = std::array::from_fn(|i| {
            let x = 2f32.powf(-12.0 + 12.0 * i as f32 / 31.0);
            // an S-curve in linear light (darker shadows, brighter mids)
            [x, (x * x * (3.0 - 2.0 * x)).min(0.9995)]
        });
        let luma = CameraTone::new(knots).unwrap();
        let rgb = luma.per_channel();
        let t = ToneMap::camera(&rgb, 0.0, 0.0, 0.0);
        assert!(t.per_channel() && !ToneMap::camera(&luma, 0.0, 0.0, 0.0).per_channel());
        for g in [0.01f32, 0.18, 0.5, 0.9] {
            let out = t.apply_rgb([g; 3]);
            assert!(out.iter().all(|v| (v - t.apply(g)).abs() < 1e-4), "{g}: {out:?}");
        }
        let (to, _) = prophoto_matrices();
        let hue = |c: [f32; 3]| {
            let p = mul3(&to, c);
            let (hi, lo) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
            let mid = p[0] + p[1] + p[2] - hi - lo;
            ((mid - lo) / (hi - lo), p.iter().position(|v| *v == hi), p.iter().position(|v| *v == lo))
        };
        let sat = |c: [f32; 3]| {
            let p = mul3(&to, c);
            let hi = p[0].max(p[1]).max(p[2]);
            (hi - p[0].min(p[1]).min(p[2])) / hi
        };
        let luminance_tone = ToneMap::camera(&luma, 0.0, 0.0, 0.0);
        for c in [[0.4f32, 0.2, 0.1], [0.05, 0.3, 0.12], [0.2, 0.25, 0.6]] {
            let out = t.apply_rgb(c);
            let (h0, h1) = (hue(c), hue(out));
            assert!((h0.0 - h1.0).abs() < 1e-3 && h0.1 == h1.1 && h0.2 == h1.2, "{c:?} → {out:?}");
            // an S-curve saturates where the luminance curve keeps the ratios
            let y = lightcraft_color::luminance_2020(c);
            let by_luma = c.map(|v| v * luminance_tone.apply(y) / y);
            assert!(sat(out) > sat(by_luma) + 0.01, "{c:?}: {} vs {}", sat(out), sat(by_luma));
        }
        let json = serde_json::to_value(rgb).unwrap();
        assert_eq!(serde_json::from_value::<CameraTone>(json).unwrap(), rgb);
        assert_eq!(serde_json::from_value::<CameraTone>(serde_json::json!({ "knots": knots })).unwrap(), luma);
    }

    #[test]
    fn monotone_and_bounded() {
        for (c, w, b) in [(0.0, 0.0, 0.0), (100.0, 100.0, -100.0), (-100.0, -100.0, 100.0), (50.0, -30.0, -40.0)] {
            let t = ToneMap::new(c, w, b);
            let mut prev = -1.0;
            for i in 0..2000 {
                let y = 1e-5 * 1.012f32.powi(i);
                let o = t.apply(y);
                assert!((0.0..=1.0).contains(&o));
                assert!(o >= prev - 1e-6, "{c} {w} {b} at {y}: {o} < {prev}");
                prev = o;
            }
        }
    }

    #[test]
    fn display_identity_and_monotone() {
        let t = ToneMap::display(0.0, 0.0, 0.0);
        for i in 1..=95 {
            let y = i as f32 / 100.0;
            assert!((t.apply(y) - y).abs() < 2e-3, "{y} -> {}", t.apply(y));
        }
        for (c, w, b) in [(100.0, 100.0, -100.0), (-100.0, -100.0, 100.0), (60.0, -40.0, 30.0)] {
            let t = ToneMap::display(c, w, b);
            let mut prev = -1.0;
            for i in 0..1000 {
                let o = t.apply(i as f32 / 500.0);
                assert!(o >= prev - 1e-5, "{c} {w} {b}");
                prev = o;
            }
        }
        let c = ToneMap::display(60.0, 0.0, 0.0);
        assert!(c.apply(0.05) < 0.05 && c.apply(0.7) > 0.7);
    }

    #[test]
    fn grey_stays_near_grey_and_highlights_roll_off() {
        let t = ToneMap::new(0.0, 0.0, 0.0);
        let g = t.apply(0.18);
        assert!((0.15..0.24).contains(&g), "{g}");
        assert!(t.apply(1.0) < 0.95 && t.apply(1.0) > 0.6);
        assert!(t.apply(8.0) > 0.97);
    }

    #[test]
    fn sliders_move_the_right_way() {
        let base = ToneMap::new(0.0, 0.0, 0.0);
        let contrast = ToneMap::new(60.0, 0.0, 0.0);
        assert!(contrast.apply(0.05) < base.apply(0.05));
        assert!(contrast.apply(0.8) > base.apply(0.8));
        assert!(ToneMap::new(0.0, 60.0, 0.0).apply(0.8) > base.apply(0.8));
        assert!(ToneMap::new(0.0, 0.0, -60.0).apply(0.01) < base.apply(0.01));
        assert!(ToneMap::new(0.0, 0.0, 60.0).apply(0.01) > base.apply(0.01));
    }
}
