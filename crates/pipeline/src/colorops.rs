//! Colour tools on display-linear Rec.2020 values: vibrance and saturation in linear ProPhoto (see
//! [`VIBRANCE_POS`]), the 8-band colour mixer, B&W mix and 3-way colour grading in OkLCh; plus the
//! camera-calibration matrix (scene linear).

use std::f32::consts::{PI, TAU};
use std::sync::OnceLock;

use lightcraft_color::perceptual::{hsv_to_rgb, lab_to_lch, lch_to_lab, oklab_from_2020, oklab_to_2020, rgb_to_hsv};
use lightcraft_color::{Mat3, REC2020, SRGB};
use lightcraft_develop::{Calibration, DevelopSettings, MIXER_HUES, PointColor};

/// OkLCh hue angle (radians) of a pure sRGB colour with HSV hue `deg`.
pub fn oklch_hue_of_srgb_hue(deg: f64) -> f32 {
    let c = hsv_to_rgb(deg as f32, 1.0, 1.0);
    let lin = c.map(lightcraft_color::transfer::srgb_to_linear);
    let m = SRGB.to_space(&REC2020).apply_f32(lin);
    lab_to_lch(oklab_from_2020(m))[2]
}

/// OkLCh hue (radians) of each colour-mixer band centre.
pub fn band_hues() -> &'static [f32; 8] {
    static H: OnceLock<[f32; 8]> = OnceLock::new();
    H.get_or_init(|| MIXER_HUES.map(oklch_hue_of_srgb_hue))
}

#[inline]
fn wrap(a: f32) -> f32 {
    (a + PI).rem_euclid(TAU) - PI
}

/// Luminance weights of linear ProPhoto RGB (D50).
pub const PROPHOTO_LUMA: [f32; 3] = [0.288_040_2, 0.711_874_1, 0.000_085_7];

/// Partition-of-unity weights of hue `h` over the 8 bands (raised cosine between neighbours).
#[inline]
pub fn band_weights(h: f32) -> [f32; 8] {
    let hues = band_hues();
    let mut w = [0.0f32; 8];
    for i in 0..8 {
        let a = hues[i];
        let b = hues[(i + 1) % 8];
        let span = wrap(b - a).rem_euclid(TAU);
        let d = wrap(h - a).rem_euclid(TAU);
        if d <= span {
            let t = d / span;
            let s = 0.5 - 0.5 * (t * PI).cos();
            w[i] += 1.0 - s;
            w[(i + 1) % 8] += s;
            break;
        }
    }
    w
}

/// A colour-grading wheel as an OkLab offset (a, b) and a lightness shift.
#[derive(Clone, Copy, Debug)]
pub struct WheelK {
    pub a: f32,
    pub b: f32,
    pub lum: f32,
}

/// Number of `f32`s per Point Color sample in [`PointK::words`] (the GPU parameter layout).
pub const POINT_WORDS: usize = 10;

/// A Point Color sample resolved for the per-pixel stage: the sample (OkLab lightness, chroma, hue
/// in radians), the adjustment (hue rotation in radians, chroma scale − 1, lightness shift,
/// variance factor − 1) and the half-widths of the range box (hue radians, chroma, lightness).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointK {
    pub l: f32,
    pub c: f32,
    pub h: f32,
    pub dh: f32,
    pub sat: f32,
    pub dl: f32,
    pub var: f32,
    pub wh: f32,
    pub wc: f32,
    pub wl: f32,
}

impl PointK {
    pub fn new(p: &PointColor) -> PointK {
        let k = (p.range.clamp(0.0, 100.0) / 50.0).max(0.05) as f32;
        let f = |v: f64| (v.clamp(0.0, 100.0) / 100.0) as f32;
        PointK {
            l: p.lum as f32,
            c: p.chroma.max(0.0) as f32,
            h: (p.hue as f32).to_radians(),
            dh: (p.hue_shift.clamp(-100.0, 100.0) / 100.0) as f32 * 0.5,
            sat: (p.sat_shift.clamp(-100.0, 100.0) / 100.0) as f32,
            dl: (p.lum_shift.clamp(-100.0, 100.0) / 100.0) as f32 * 0.2,
            var: (p.variance.clamp(-100.0, 100.0) / 100.0) as f32 * 0.8,
            wh: (0.1 + 0.6 * f(p.hue_range)) * k,
            wc: (0.02 + 0.12 * f(p.sat_range)) * k,
            wl: (0.05 + 0.4 * f(p.lum_range)) * k,
        }
    }

    pub fn words(&self) -> [f32; POINT_WORDS] {
        [self.l, self.c, self.h, self.dh, self.sat, self.dl, self.var, self.wh, self.wc, self.wl]
    }

    /// How much an OkLCh colour belongs to this sample's range (0..1, soft edges).
    #[inline]
    pub fn weight(&self, l: f32, c: f32, h: f32) -> f32 {
        // hue is meaningless for near-neutral colours: a chromatic sample ignores them, a
        // near-neutral sample selects by chroma and lightness only
        let wh = if self.c < 0.02 { 1.0 } else { (1.0 - smooth(0.5 * self.wh, self.wh, wrap(h - self.h).abs())) * smooth(0.005, 0.025, c) };
        if wh <= 0.0 {
            return 0.0;
        }
        let wc = 1.0 - smooth(0.5 * self.wc, self.wc, (c - self.c).abs());
        let wl = 1.0 - smooth(0.5 * self.wl, self.wl, (l - self.l).abs());
        wh * wc * wl
    }

    /// Apply this sample's adjustment to an OkLCh colour.
    #[inline]
    pub fn apply(&self, lch: [f32; 3]) -> [f32; 3] {
        let [mut l, mut c, mut h] = lch;
        let w = self.weight(l, c, h);
        if w <= 0.0 {
            return lch;
        }
        h += w * (self.var * wrap(h - self.h) + self.dh);
        c += w * self.var * (c - self.c);
        c = (c * (1.0 + w * self.sat)).max(0.0);
        l += w * (self.var * (l - self.l) + self.dl);
        [l, c, h]
    }
}

// ---- Vibrance and Saturation, fitted to Lightroom Classic 15.6 renders of a synthetic chart
// carrying an Apple ProRAW's own colour tags (at ±25 / ±50 / ±100), on display-linear ProPhoto.
// Vibrance scales HSV saturation, more for weakly saturated colours, raising (or lowering) value a
// little, and positive amounts spare skin tones (mean ΔE00 0.4 / 0.8 / 1.5 at +25 / +50 / +100,
// 1.0 / 2.3 at −25 / −50). Saturation scales the chroma around ProPhoto luminance, −100 giving
// exactly that grey (1.1 / 0.9 at −25 / −100, 1.0 / 1.9 at +25 / +50).

/// Vibrance > 0: ln saturation gain at +100, exponent of (1 − S), log2 value gain, skin weight.
pub const VIBRANCE_POS: [f32; 4] = [0.751, 0.736, 0.15, 0.391];
/// Vibrance < 0: ln saturation gain at −100, exponent of (1 − S), log2 value gain, exponent of S
/// on the value gain.
pub const VIBRANCE_NEG: [f32; 4] = [1.288, 0.323, 0.608, 0.263];
/// Below this HSV saturation Vibrance's value gain fades out linearly: Lightroom leaves greys'
/// brightness alone (chart grey ramp: log2 change ≤ 0.004 at ±100). Fitted on the chart's patches
/// under S 0.25, which the gains above leave out; on them mean ΔE00 0.77 / 1.49 / 2.61 → 0.62 /
/// 1.20 / 2.03 at +25 / +50 / +100 and 0.64 / 1.46 / 3.85 → 0.22 / 0.76 / 2.76 at −25 / −50 / −100,
/// greys 0.4–1.8 → under 0.14. Without it exact greys kept their value and near greys took the full
/// gain: a step at the neutral axis.
pub const VIBRANCE_FADE: f32 = 0.3;
/// Skin tones Vibrance spares: HSV hue of linear ProPhoto and half-width (degrees).
pub const SKIN_HUE: [f32; 2] = [20.0, 35.0];
/// Saturation: chroma scale per unit at +100 and its (1 − S) exponent; at −100 the chroma goes.
pub const SATURATION_POS: [f32; 2] = [0.927, 0.076];

/// Vibrance (−1..1) on linear ProPhoto `p`.
#[inline]
pub fn vibrance(p: [f32; 3], a: f32) -> [f32; 3] {
    let [h, s, v] = rgb_to_hsv(p);
    if v <= 0.0 || s <= 0.0 {
        return p;
    }
    let rest = (1.0 - s).max(0.0);
    let fade = (s / VIBRANCE_FADE).min(1.0);
    let (e, dv) = if a > 0.0 {
        let d = (h - SKIN_HUE[0] + 180.0).rem_euclid(360.0) - 180.0;
        let skin = 1.0 - VIBRANCE_POS[3] * (-(d / SKIN_HUE[1]).powi(2)).exp();
        (a * VIBRANCE_POS[0] * rest.powf(VIBRANCE_POS[1]) * skin, a * VIBRANCE_POS[2] * skin * fade)
    } else {
        (a * VIBRANCE_NEG[0] * rest.powf(VIBRANCE_NEG[1]), a * VIBRANCE_NEG[2] * s.min(1.0).powf(VIBRANCE_NEG[3]) * fade)
    };
    hsv_to_rgb(h, (s * e.exp()).min(s.max(1.0)), v * dv.exp2())
}

/// Saturation (−1..1) on linear ProPhoto `p`.
#[inline]
pub fn saturation(p: [f32; 3], a: f32) -> [f32; 3] {
    let y = PROPHOTO_LUMA[0] * p[0] + PROPHOTO_LUMA[1] * p[1] + PROPHOTO_LUMA[2] * p[2];
    let f = if a > 0.0 {
        let s = rgb_to_hsv(p)[1].clamp(0.0, 1.0);
        1.0 + a * SATURATION_POS[0] * (1.0 - s).powf(SATURATION_POS[1])
    } else {
        (1.0 + a).max(0.0)
    };
    p.map(|c| y + (c - y) * f)
}

/// The colour tools' parameters, resolved once per render (fields are read by the GPU kernel).
#[derive(Clone, Debug)]
pub struct ColorOps {
    pub vibrance: f32,
    pub saturation: f32,
    pub hue: [f32; 8],
    pub sat: [f32; 8],
    pub lum: [f32; 8],
    pub mixer: bool,
    /// Point Color samples that change something (applied after the mixer).
    pub points: Vec<PointK>,
    pub bw: Option<[f32; 8]>,
    /// Wheels (shadows, midtones, highlights, global), blending, balance.
    pub grading: Option<([WheelK; 4], f32, f32)>,
}

fn wheel(w: &lightcraft_develop::Wheel) -> WheelK {
    let h = oklch_hue_of_srgb_hue(w.hue);
    let s = (w.sat / 100.0) as f32 * 0.09;
    WheelK { a: s * h.cos(), b: s * h.sin(), lum: (w.lum / 100.0) as f32 * 0.12 }
}

impl ColorOps {
    pub fn new(s: &DevelopSettings) -> ColorOps {
        let bands = s.mixer.bands();
        let g = &s.grading;
        ColorOps {
            vibrance: (s.color.vibrance / 100.0) as f32,
            saturation: (s.color.saturation / 100.0) as f32,
            hue: bands.map(|b| (b.hue / 100.0) as f32 * 0.5),
            sat: bands.map(|b| (b.sat / 100.0) as f32),
            lum: bands.map(|b| (b.lum / 100.0) as f32 * 0.18),
            mixer: !s.mixer.is_neutral(),
            points: if crate::is_bw(s) {
                Vec::new()
            } else {
                s.point_colors.iter().take(lightcraft_develop::MAX_POINT_COLORS).filter(|p| !p.is_neutral()).map(PointK::new).collect()
            },
            bw: crate::is_bw(s).then(|| s.bw_mix.bands().map(|v| (v / 100.0) as f32)),
            grading: (!g.is_neutral()).then(|| {
                (
                    [wheel(&g.shadows), wheel(&g.midtones), wheel(&g.highlights), wheel(&g.global)],
                    (g.blending / 100.0) as f32,
                    (g.balance / 100.0) as f32,
                )
            }),
        }
    }

    pub fn is_identity(&self) -> bool {
        self.vibrance == 0.0 && self.saturation == 0.0 && !self.mixer && self.points.is_empty() && self.bw.is_none() && self.grading.is_none()
    }

    /// The colour tools in order: the mixer and Point Color (OkLCh), Vibrance and Saturation
    /// (linear ProPhoto, see [`VIBRANCE_POS`]), then a mask's saturation / hue, the B&W mix and
    /// colour grading (OkLCh). `local_sat` (−1..1) and `local_hue` (radians) come from masks.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3], local_sat: f32, local_hue: f32) -> [f32; 3] {
        if self.is_identity() && local_sat == 0.0 && local_hue == 0.0 {
            return rgb;
        }
        if self.vibrance == 0.0 && self.saturation == 0.0 {
            return self.oklch(rgb, true, true, local_sat, local_hue);
        }
        let rgb = self.oklch(rgb, true, false, 0.0, 0.0);
        let rgb = self.prophoto(rgb);
        self.oklch(rgb, false, true, local_sat, local_hue)
    }

    /// Vibrance and Saturation on display-linear Rec.2020 `rgb`, in linear ProPhoto.
    #[inline]
    fn prophoto(&self, rgb: [f32; 3]) -> [f32; 3] {
        let (to, from) = crate::tone::prophoto_matrices();
        let mut p: [f32; 3] = std::array::from_fn(|i| to[i][0] * rgb[0] + to[i][1] * rgb[1] + to[i][2] * rgb[2]);
        if self.vibrance != 0.0 {
            p = vibrance(p, self.vibrance);
        }
        if self.saturation != 0.0 {
            p = saturation(p, self.saturation);
        }
        std::array::from_fn(|i| from[i][0] * p[0] + from[i][1] * p[1] + from[i][2] * p[2])
    }

    /// The OkLCh tools: the mixer and Point Color when `pre`; a mask's saturation / hue, the B&W
    /// mix and colour grading when `post`. Returns `rgb` when none of them has work to do.
    #[inline]
    fn oklch(&self, rgb: [f32; 3], pre: bool, post: bool, local_sat: f32, local_hue: f32) -> [f32; 3] {
        let pre = pre && (self.mixer || !self.points.is_empty());
        let post = post && (self.bw.is_some() || self.grading.is_some() || local_sat != 0.0 || local_hue != 0.0);
        if !pre && !post {
            return rgb;
        }
        let lab = oklab_from_2020(rgb);
        // only a chroma change (a mask's saturation, maybe grading): scale a, b directly — the same
        // result as the OkLCh round trip without its sin / cos / atan2
        if !pre && self.bw.is_none() && local_hue == 0.0 {
            let k = (1.0 + local_sat).max(0.0);
            return self.grade([lab[0], lab[1] * k, lab[2] * k]);
        }
        let [mut l, mut c, mut h] = lab_to_lch(lab);
        if pre {
            if self.mixer {
                let w = band_weights(h);
                let (mut dh, mut ds, mut dl) = (0.0, 0.0, 0.0);
                for i in 0..8 {
                    dh += w[i] * self.hue[i];
                    ds += w[i] * self.sat[i];
                    dl += w[i] * self.lum[i];
                }
                let chroma_w = (c / 0.12).min(1.0);
                h += dh * chroma_w;
                c *= (1.0 + ds).max(0.0);
                l += dl * chroma_w * l.max(0.05).sqrt();
            }
            for p in &self.points {
                [l, c, h] = p.apply([l, c, h]);
            }
        }
        if !post {
            return oklab_to_2020(lch_to_lab([l, c, h]));
        }
        if local_sat != 0.0 {
            c *= (1.0 + local_sat).max(0.0);
        }
        h += local_hue;
        if let Some(bw) = &self.bw {
            let w = band_weights(h);
            let mix: f32 = (0..8).map(|i| w[i] * bw[i]).sum();
            l = (l + mix * (c / 0.2).min(1.0) * 0.25).max(0.0);
            c = 0.0;
        }
        self.grade(lch_to_lab([l, c, h]))
    }

    /// Colour grading on OkLab, then back to linear Rec.2020.
    #[inline]
    fn grade(&self, mut lab: [f32; 3]) -> [f32; 3] {
        if let Some((wheels, blending, balance)) = &self.grading {
            let m = 0.5 - balance * 0.25;
            let width = 0.15 + blending * 0.5;
            let ws = 1.0 - smooth(m - width, m + width * 0.25, lab[0]);
            let wh = smooth(m - width * 0.25, m + width, lab[0]);
            let wm = (1.0 - ws - wh).max(0.0);
            for (k, wt) in wheels.iter().zip([ws, wm, wh, 1.0]) {
                lab[1] += k.a * wt;
                lab[2] += k.b * wt;
                lab[0] += k.lum * wt;
            }
        }
        oklab_to_2020(lab)
    }
}

/// Hue rotation (OkLCh radians) of a calibration primary at ±100.
pub const CALIB_HUE: f32 = 0.5;
/// Chroma scale of a calibration primary at ±100 (`1 ± CALIB_SAT`).
pub const CALIB_SAT: f32 = 0.6;

/// The calibration panel's primaries as a white-preserving 3×3 matrix on linear Rec.2020 (row-major):
/// each primary is rotated in OkLCh hue and scaled in chroma at constant OkLab lightness, then the
/// columns are rescaled so that neutral (1, 1, 1) maps to itself. `None` when neutral.
pub fn calibration_matrix(c: &Calibration) -> Option<[[f32; 3]; 3]> {
    let prim = c.primaries();
    if prim.iter().all(|(h, s)| *h == 0.0 && *s == 0.0) {
        return None;
    }
    let mut p = [[0.0f64; 3]; 3];
    for (i, (hue, sat)) in prim.iter().enumerate() {
        let mut e = [0.0f32; 3];
        e[i] = 1.0;
        let [l, ch, h] = lab_to_lch(oklab_from_2020(e));
        let h2 = h + (hue.clamp(-100.0, 100.0) / 100.0) as f32 * CALIB_HUE;
        let c2 = ch * (1.0 + (sat.clamp(-100.0, 100.0) / 100.0) as f32 * CALIB_SAT);
        let q = oklab_to_2020(lch_to_lab([l, c2, h2]));
        for (r, row) in p.iter_mut().enumerate() {
            row[i] = q[r] as f64;
        }
    }
    let m = Mat3(p);
    let k = m.inverse()?.apply([1.0, 1.0, 1.0]);
    Some(std::array::from_fn(|r| std::array::from_fn(|col| (p[r][col] * k[col]) as f32)))
}

/// Shadows-tint strength at ±100: the green channel's relative change in deep shadows.
pub const SHADOW_TINT: f32 = 0.3;

/// Calibration in scene-linear light (before tone mapping): the primaries matrix, then the shadows
/// tint (green ↔ magenta, weighted towards dark tones, luminance kept).
#[inline]
pub fn calibrate(c: [f32; 3], m: Option<&[[f32; 3]; 3]>, shadow_tint: f32) -> [f32; 3] {
    let mut c = c;
    if let Some(m) = m {
        c = std::array::from_fn(|r| (m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2]).max(0.0));
    }
    if shadow_tint != 0.0 {
        let y0 = lightcraft_color::luminance_2020(c);
        let w = 1.0 - smooth(-5.0, -0.5, (y0.max(1e-7) / 0.18).log2());
        c[1] *= (1.0 - SHADOW_TINT * shadow_tint * w).max(0.0);
        let y1 = lightcraft_color::luminance_2020(c).max(1e-9);
        c = c.map(|v| v * y0 / y1);
    }
    c
}

#[inline]
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_color::perceptual::lab_to_lch;

    #[test]
    fn weights_partition_unity() {
        for i in 0..360 {
            let h = (i as f32).to_radians() - PI;
            let w = band_weights(h);
            let s: f32 = w.iter().sum();
            assert!((s - 1.0).abs() < 1e-4, "{i}: {s}");
        }
        // at a band centre, that band dominates
        let w = band_weights(band_hues()[5]);
        assert!(w[5] > 0.99);
    }

    #[test]
    fn neutral_is_identity() {
        let ops = ColorOps::new(&DevelopSettings::default());
        assert!(ops.is_identity());
        assert_eq!(ops.apply([0.2, 0.3, 0.4], 0.0, 0.0), [0.2, 0.3, 0.4]);
    }

    #[test]
    fn vibrance_is_continuous_at_the_neutral_axis() {
        // an exact grey and one a rounding error off it come out alike: the value gain fades out
        // towards neutral (without the fade it would apply in full to every colour but an exact grey)
        for a in [-1.0, -0.4, 0.4, 1.0] {
            for v in [0.05f32, 0.5, 1.0] {
                let grey = vibrance([v, v, v], a);
                assert_eq!(grey, [v, v, v]);
                for tint in [[1.0, 1.0, 1.0 + 1e-4], [1.0 + 1e-4, 1.0, 1.0], [1.0, 1.0 - 1e-4, 1.0]] {
                    let near = vibrance(std::array::from_fn(|i| v * tint[i]), a);
                    assert!((0..3).all(|c| (near[c] - grey[c]).abs() < 1e-3 * v), "a {a}, v {v}: {near:?} vs {grey:?}");
                }
            }
        }
        // saturated colours keep the full gain
        let [_, _, v0] = rgb_to_hsv([0.6, 0.2, 0.1]);
        let [_, _, v1] = rgb_to_hsv(vibrance([0.6, 0.2, 0.1], 1.0));
        assert!(v1 > v0 * 1.05, "{v0} -> {v1}");
    }

    #[test]
    fn vibrance_favours_muted_colours_and_spares_skin() {
        let sat = |p: [f32; 3]| rgb_to_hsv(p)[1];
        // a muted blue gains more saturation (relative) than a vivid one
        let (muted, vivid) = ([0.3, 0.35, 0.45], [0.05, 0.15, 0.6]);
        let gain = |p: [f32; 3]| sat(vibrance(p, 0.6)) / sat(p);
        assert!(gain(muted) > gain(vivid) * 1.1, "{} vs {}", gain(muted), gain(vivid));
        // positive Vibrance spares skin tones (HSV hue ~20° in linear ProPhoto)
        let skin = hsv_to_rgb(SKIN_HUE[0], 0.4, 0.5);
        let other = hsv_to_rgb(SKIN_HUE[0] + 180.0, 0.4, 0.5);
        assert!(gain(skin) < gain(other) - 0.05, "{} vs {}", gain(skin), gain(other));
        // negative Vibrance desaturates, without the skin protection
        let neg = |p: [f32; 3]| sat(vibrance(p, -0.6)) / sat(p);
        assert!(neg(skin) < 0.8 && (neg(skin) - neg(other)).abs() < 1e-4, "{} {}", neg(skin), neg(other));
    }

    #[test]
    fn saturation_keeps_prophoto_luminance() {
        let y = |p: [f32; 3]| PROPHOTO_LUMA[0] * p[0] + PROPHOTO_LUMA[1] * p[1] + PROPHOTO_LUMA[2] * p[2];
        let p = [0.5, 0.2, 0.1];
        for a in [-1.0, -0.5, 0.5, 1.0] {
            assert!((y(saturation(p, a)) - y(p)).abs() < 1e-6, "{a}");
        }
        // −100 gives exactly that grey; a grey stays put at any amount
        let g = saturation(p, -1.0);
        assert!(g.iter().all(|c| (c - y(p)).abs() < 1e-6), "{g:?}");
        assert!(saturation([0.3, 0.3, 0.3], 1.0).iter().all(|c| (c - 0.3).abs() < 1e-6));
        // positive amounts raise weakly saturated colours a little more
        let spread = |p: [f32; 3], a| (saturation(p, a)[0] - y(p)) / (p[0] - y(p));
        assert!(spread([0.3, 0.25, 0.25], 0.5) > spread([0.6, 0.05, 0.05], 0.5));
    }

    #[test]
    fn vibrance_and_saturation_leave_the_other_tools_in_order() {
        // the mixer still acts first (on the unmodified colour) and a mask's saturation last
        let mut s = DevelopSettings::default();
        s.mixer.blue.sat = -100.0;
        let blue = [0.02, 0.05, 0.4];
        let base = ColorOps::new(&s).apply(blue, 0.5, 0.0);
        s.color.saturation = 1.0;
        let tiny = ColorOps::new(&s).apply(blue, 0.5, 0.0);
        assert!((0..3).all(|i| (tiny[i] - base[i]).abs() < 2e-3), "{base:?} vs {tiny:?}");
        let c = lab_to_lch(oklab_from_2020(ColorOps::new(&s).apply(blue, -1.0, 0.0)))[1];
        assert!(c < 1e-3, "{c}");
    }

    #[test]
    fn saturation_and_bw() {
        let mut s = DevelopSettings::default();
        s.color.saturation = -100.0;
        let g = ColorOps::new(&s).apply([0.4, 0.1, 0.05], 0.0, 0.0);
        assert!((g[0] - g[1]).abs() < 1e-3 && (g[1] - g[2]).abs() < 1e-3, "{g:?}");
        let mut s = DevelopSettings { treatment: lightcraft_develop::Treatment::Bw, ..Default::default() };
        let ops = ColorOps::new(&s);
        let b = ops.apply([0.05, 0.1, 0.5], 0.0, 0.0);
        assert!((b[0] - b[2]).abs() < 1e-3);
        // raising blue in the B&W mix brightens blue things
        s.bw_mix.blue = 80.0;
        let b2 = ColorOps::new(&s).apply([0.05, 0.1, 0.5], 0.0, 0.0);
        assert!(b2[1] > b[1]);
    }

    #[test]
    fn mixer_targets_its_band() {
        let mut s = DevelopSettings::default();
        s.mixer.blue.sat = -100.0;
        let ops = ColorOps::new(&s);
        let blue = [0.02, 0.05, 0.4];
        let red = [0.4, 0.03, 0.02];
        let cb = lab_to_lch(oklab_from_2020(ops.apply(blue, 0.0, 0.0)))[1];
        let cr0 = lab_to_lch(oklab_from_2020(red))[1];
        let cr = lab_to_lch(oklab_from_2020(ops.apply(red, 0.0, 0.0)))[1];
        assert!(cb < 0.02, "{cb}");
        assert!((cr - cr0).abs() < 0.01);
    }

    #[test]
    fn calibration_keeps_white_and_moves_primaries() {
        assert!(calibration_matrix(&Calibration::default()).is_none());
        let cal = Calibration { red_hue: 60.0, blue_sat: -50.0, ..Default::default() };
        let m = calibration_matrix(&cal).unwrap();
        let grey = calibrate([0.3, 0.3, 0.3], Some(&m), 0.0);
        assert!(grey.iter().all(|v| (v - 0.3).abs() < 1e-4), "{grey:?}");
        // red rotates in hue, blue loses chroma
        let red = [0.4, 0.05, 0.03];
        let h0 = lab_to_lch(oklab_from_2020(red))[2];
        let h1 = lab_to_lch(oklab_from_2020(calibrate(red, Some(&m), 0.0)))[2];
        assert!(h1 - h0 > 0.05, "{h0} -> {h1}");
        let blue = [0.03, 0.05, 0.4];
        let c0 = lab_to_lch(oklab_from_2020(blue))[1];
        let c1 = lab_to_lch(oklab_from_2020(calibrate(blue, Some(&m), 0.0)))[1];
        assert!(c1 < c0 * 0.97, "{c0} -> {c1}");
    }

    #[test]
    fn shadow_tint_targets_shadows() {
        let dark = calibrate([0.01, 0.01, 0.01], None, 1.0);
        let bright = calibrate([0.8, 0.8, 0.8], None, 1.0);
        assert!(dark[1] < dark[0] * 0.85, "magenta shadows: {dark:?}");
        assert!((bright[1] - bright[0]).abs() < 1e-3, "{bright:?}");
        let y = |c: [f32; 3]| lightcraft_color::luminance_2020(c);
        assert!((y(dark) - 0.01).abs() < 1e-5);
        let green = calibrate([0.01, 0.01, 0.01], None, -1.0);
        assert!(green[1] > green[0]);
    }

    #[test]
    fn point_color_targets_its_range() {
        let skin = [0.35, 0.16, 0.09];
        let [l, c, h] = lab_to_lch(oklab_from_2020(skin));
        let sample =
            PointColor { lum: l as f64, chroma: c as f64, hue: (h as f64).to_degrees(), hue_shift: 60.0, sat_shift: -50.0, ..Default::default() };
        let mut s = DevelopSettings::default();
        s.point_colors.push(sample);
        let ops = ColorOps::new(&s);
        let out = lab_to_lch(oklab_from_2020(ops.apply(skin, 0.0, 0.0)));
        assert!(out[1] < c * 0.7, "chroma {c} -> {}", out[1]);
        assert!(wrap(out[2] - h) > 0.2, "hue {h} -> {}", out[2]);
        // far colours are untouched
        for far in [[0.02, 0.05, 0.4], [0.05, 0.3, 0.05], [0.3, 0.3, 0.3]] {
            let o = ops.apply(far, 0.0, 0.0);
            assert!(far.iter().zip(&o).all(|(a, b)| (a - b).abs() < 1e-4), "{far:?} -> {o:?}");
        }
        // weight: 1 at the sample, falling to 0 outside the range; a wider range reaches further
        let k = PointK::new(&sample);
        assert!((k.weight(l, c, h) - 1.0).abs() < 1e-6);
        assert_eq!(k.weight(l, c, h + 1.0), 0.0);
        let wide = PointK::new(&PointColor { range: 100.0, ..sample });
        assert!(wide.weight(l, c, h + 0.45) > k.weight(l, c, h + 0.45));
    }

    #[test]
    fn point_color_variance_compresses_towards_the_sample() {
        let p = PointColor { lum: 0.6, chroma: 0.1, hue: 40.0, variance: -100.0, range: 100.0, ..Default::default() };
        let k = PointK::new(&p);
        let (h0, near) = (40f32.to_radians(), 40f32.to_radians() + 0.1);
        let o = k.apply([0.62, 0.11, near]);
        assert!((o[2] - h0).abs() < 0.1 && (o[1] - 0.1).abs() < 0.01 && (o[0] - 0.6).abs() < 0.02, "{o:?}");
        let k = PointK::new(&PointColor { variance: 100.0, ..p });
        let o = k.apply([0.62, 0.11, near]);
        assert!(o[2] - h0 > 0.1, "{o:?}");
    }

    #[test]
    fn grading_tints_shadows_only() {
        let mut s = DevelopSettings::default();
        s.grading.shadows = lightcraft_develop::Wheel { hue: 220.0, sat: 60.0, lum: 0.0 };
        let ops = ColorOps::new(&s);
        let dark = ops.apply([0.01, 0.01, 0.01], 0.0, 0.0);
        let bright = ops.apply([0.8, 0.8, 0.8], 0.0, 0.0);
        assert!(dark[2] > dark[0]);
        assert!((bright[2] - bright[0]).abs() < 0.02);
    }
}
