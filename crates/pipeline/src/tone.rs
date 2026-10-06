//! The global tone map: scene luminance → display-linear luminance.
//!
//! Built in the log domain around middle grey (0.18): contrast scales log-exposure about grey,
//! whites move the shoulder (white point), blacks move the toe. The shoulder is an extended
//! Reinhard curve so highlights roll off smoothly instead of clipping.
//!
//! Rendered (display-referred) sources such as JPEGs use [`ToneMap::display`] instead: identity at
//! neutral settings (an unedited JPEG renders exactly as the file), with contrast/whites/blacks as
//! S-curve adjustments in a gamma-2.2 perceptual domain and a short shoulder above 0.95.

pub const GREY: f32 = 0.18;
/// The tone LUT spans `LUT_MIN_EV..LUT_MAX_EV` around grey in `LUT_N` steps.
pub const LUT_MIN_EV: f32 = -14.0;
pub const LUT_MAX_EV: f32 = 10.0;
pub const LUT_N: usize = 4096;

/// A bounded scene-linear Rec.2020 → display-linear Rec.2020 response sampled from
/// the calibrated decoder with synthetic inputs, independently of the photographed scene.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct CameraResponse {
    data: Vec<[f32; 3]>,
}

impl<'de> serde::Deserialize<'de> for CameraResponse {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Wire {
            data: Vec<[f32; 3]>,
        }
        Self::new(Wire::deserialize(d)?.data).ok_or_else(|| serde::de::Error::custom("invalid camera response"))
    }
}

impl CameraResponse {
    pub const SIZE: usize = 33;
    pub const SCALE: f32 = 0.001;
    pub const MAX: f32 = 16.0;
    pub fn new(data: Vec<[f32; 3]>) -> Option<Self> {
        if data.len() != Self::SIZE.pow(3) || data.iter().flatten().any(|x| !x.is_finite() || !(-0.5..=2.0).contains(x)) {
            return None;
        }
        if data.first()?.iter().any(|v| v.abs() > 1e-4) || lightcraft_color::luminance_2020(*data.last()?) < 0.9 {
            return None;
        }
        let mut previous = 0.0;
        for i in 0..Self::SIZE {
            let y = lightcraft_color::luminance_2020(*data.get(i + Self::SIZE * (i + Self::SIZE * i))?);
            if y + 0.002 < previous {
                return None;
            }
            previous = y;
        }
        Some(Self { data })
    }
    pub fn data(&self) -> &[[f32; 3]] {
        &self.data
    }
    pub fn input(i: usize) -> f32 {
        Self::SCALE * (((1.0 + Self::MAX / Self::SCALE).ln() * i as f32 / (Self::SIZE - 1) as f32).exp() - 1.0)
    }
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let x = c.map(|v| {
            let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
            ((1.0 + v / Self::SCALE).ln() / (1.0 + Self::MAX / Self::SCALE).ln()).clamp(0.0, 1.0) * (Self::SIZE - 1) as f32
        });
        let i = x.map(|v| (v as usize).min(Self::SIZE - 2));
        let t = std::array::from_fn::<_, 3, _>(|k| x[k] - i[k] as f32);
        let mut out = [0.0; 3];
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    let weight = [r, g, b].iter().enumerate().map(|(k, d)| if *d == 0 { 1.0 - t[k] } else { t[k] }).product::<f32>();
                    let index = i[0] + r + Self::SIZE * (i[1] + g + Self::SIZE * (i[2] + b));
                    if let Some(p) = self.data.get(index) {
                        for k in 0..3 {
                            out[k] += p[k] * weight;
                        }
                    }
                }
            }
        }
        out
    }
    /// Grey-axis approximation used only for metering/Auto Tone, never for rendering colour.
    pub fn grey_tone(&self) -> CameraTone {
        let mut previous = 0.0f32;
        let knots = std::array::from_fn(|i| {
            let x = Self::input(i + 1);
            let y = lightcraft_color::luminance_2020(self.apply([x; 3])).clamp(0.0, 1.0 - f32::EPSILON).max(previous);
            previous = y;
            [x, y]
        });
        CameraTone::new(knots).unwrap_or_else(CameraTone::neutral)
    }
}

/// A file-local camera look, fitted independently of the scene-linear colour transform.
/// Knots are scene/display-linear luminance pairs. Keeping this in the finish stage preserves
/// RAW exposure and highlight headroom; it is never baked into the decoded sensor pixels.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct CameraTone {
    knots: [[f32; 2]; 32],
}

impl<'de> serde::Deserialize<'de> for CameraTone {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Wire {
            knots: [[f32; 2]; 32],
        }
        let w = Wire::deserialize(d)?;
        Self::new(w.knots).ok_or_else(|| serde::de::Error::custom("invalid camera tone curve"))
    }
}

impl CameraTone {
    /// A restrained display response for a calibrated source whose optional starting-look fit
    /// was rejected. Midtones remain linear; only display highlights receive a soft shoulder.
    /// This curve never changes the source calibration or lifts shadows to match a preview.
    pub fn neutral() -> Self {
        let knots = std::array::from_fn(|i| {
            let x = 2f32.powf(-12.0 + i as f32 * 14.0 / 31.0);
            let y = if x <= 0.8 { x } else { 0.8 + 0.2 * (1.0 - (-(x - 0.8) / 0.2).exp()) };
            [x, y.min(1.0 - f32::EPSILON)]
        });
        Self { knots }
    }

    pub fn new(knots: [[f32; 2]; 32]) -> Option<Self> {
        let mut previous = [0.0, 0.0];
        for p in knots {
            if !p.iter().all(|v| v.is_finite()) || p[0] <= previous[0] || p[1] < previous[1] || p[1] >= 1.0 {
                return None;
            }
            previous = p;
        }
        Some(Self { knots })
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
        ToneMap { lut }
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
        ToneMap { lut }
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
        ToneMap { lut }
    }

    /// The table (`LUT_N` entries, see [`ToneMap::apply`]).
    pub fn lut(&self) -> &[f32] {
        &self.lut
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
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    fn synthetic_response() -> CameraResponse {
        let mut data = Vec::new();
        for b in 0..33 {
            for g in 0..33 {
                for r in 0..33 {
                    let c = [r, g, b].map(CameraResponse::input).map(|v| v / (1.0 + v));
                    data.push([0.8 * c[0] + 0.2 * c[1], c[1], 0.9 * c[2] + 0.1 * c[0]]);
                }
            }
        }
        CameraResponse::new(data).unwrap()
    }
    #[test]
    fn response_preserves_cross_channel_colour_and_is_bounded() {
        let response = synthetic_response();
        assert_eq!(response.apply([0.0; 3]), [0.0; 3]);
        let c = [CameraResponse::input(18), CameraResponse::input(13), CameraResponse::input(10)];
        let v = c.map(|x| x / (1.0 + x));
        let expected = [0.8 * v[0] + 0.2 * v[1], v[1], 0.9 * v[2] + 0.1 * v[0]];
        for (a, b) in response.apply(c).iter().zip(expected) {
            assert!((a - b).abs() < 1e-6);
        }
        assert!(response.apply([f32::INFINITY, -1.0, f32::NAN]).iter().all(|v| v.is_finite()));
        assert!(CameraResponse::new(vec![[0.0; 3]; 33usize.pow(3)]).is_none());
        assert!(CameraResponse::new(vec![[1.0; 3]; 33usize.pow(3)]).is_none());
        assert!(CameraResponse::new(vec![[0.0; 3]; 8]).is_none());
        let mut invalid = response.data.clone();
        invalid[123][0] = f32::NAN;
        assert!(CameraResponse::new(invalid).is_none());
        let encoded = serde_json::to_string(&response).unwrap();
        assert_eq!(serde_json::from_str::<CameraResponse>(&encoded).unwrap(), response);
    }

    #[test]
    fn ordinary_initial_render_uses_response_once_and_keeps_linear_source() {
        let response = std::sync::Arc::new(synthetic_response());
        let c = [CameraResponse::input(18), CameraResponse::input(13), CameraResponse::input(10)];
        let mut src = lightcraft_raster::Rgb32f::new(8, 8);
        src.data.fill(c);
        let info = crate::SourceInfo {
            raw: true,
            relative_wb: true,
            camera_response: Some(response),
            camera_tone: Some(CameraTone::neutral()),
            ..Default::default()
        };
        let settings = lightcraft_develop::DevelopSettings::default();
        let actual = crate::render(&src, &info, &settings, &crate::RenderRequest::fit(8, 8));
        let v = c.map(|x| x / (1.0 + x));
        let expected = [0.8 * v[0] + 0.2 * v[1], v[1], 0.9 * v[2] + 0.1 * v[0]];
        let out = lightcraft_color::REC2020.to_space(&lightcraft_color::SRGB).apply(expected.map(f64::from)).map(|v| v as f32);
        let (out, _) = crate::finish::gamut_map(out, [0.2126, 0.7152, 0.0722]);
        let expected = out.map(|v| (lightcraft_color::transfer::linear_to_srgb(v) * 255.0).round() as u8);
        for (a, b) in actual.image.data[0][..3].iter().zip(expected) {
            assert!(a.abs_diff(b) <= 1);
        }
        assert_eq!(src.data[0], c);
        let mut edited = settings;
        edited.light.exposure = -1.0;
        let darker = crate::render(&src, &info, &edited, &crate::RenderRequest::fit(8, 8));
        assert!(darker.image.data[0][0] < actual.image.data[0][0]);
    }

    #[test]
    fn neutral_camera_response_preserves_midtones_and_bounds_highlights() {
        let curve = CameraTone::neutral();
        assert!(CameraTone::new(curve.knots).is_some());
        for x in [0.0, 0.001, 0.02, 0.18, 0.4, 0.6] {
            assert!((curve.apply(x) - x).abs() < 1e-5);
        }
        let mut previous = 0.0;
        for i in 0..1000 {
            let v = curve.apply(i as f32 / 100.0);
            assert!((0.0..=1.0).contains(&v) && v >= previous);
            previous = v;
        }
        assert!(curve.apply(1.0) < 1.0);
        assert!(curve.apply(2.0) > curve.apply(1.0));
    }

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
