//! Colour tools on display-linear Rec.2020 values: the 8-band colour mixer (HSV of linear
//! ProPhoto, see [`MIX_CENTERS`]), then vibrance, saturation, B&W mix and 3-way colour grading in
//! OkLCh; plus the camera-calibration matrix (scene linear).

use std::f32::consts::{PI, TAU};
use std::sync::OnceLock;

use lightcraft_color::perceptual::{hsv_to_rgb, lab_to_lch, lch_to_lab, oklab_from_2020, oklab_to_2020, rgb_to_hsv};
use lightcraft_color::{Mat3, PROPHOTO, REC2020, SRGB};
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

/// Colour grading per wheel (shadows, midtones, highlights, global), fitted to Lightroom Classic
/// renders (Apple ProRAW chart; every wheel's hue, saturation and luminance, Blending and Balance
/// swept): each wheel scales the linear-ProPhoto channels by `exp(w·(K·sat·(c − m) + K_L·lum))`,
/// with `w` the wheel's weight at the pixel's log2 luminance and `c` the wheel hue's pure ProPhoto
/// colour. The log ratio between the hue's channels and the others grows linearly with
/// saturation, as in Lightroom. Midtones and global centre the tint (`m = ½`); shadows and
/// highlights measure it from the hue's luminance, `m = Y(c)` (a blue shadow tint raises blue and
/// leaves red and green), until the raised channels reach [`GRADE_CAP`]'s limit and the rest
/// lowers the others. Columns: K, K_L, weight centre (log2 Y), weight width. The shadows weight
/// falls, the highlights weight rises and the midtones weight peaks around its centre; the global
/// wheel fades out above diffuse white.
pub const GRADE_WHEEL: [[f32; 4]; 4] =
    [[1.969, 0.492, -4.711, 1.625], [0.847, 0.340, -2.860, 3.208], [0.893, 0.201, -2.449, 1.527], [1.091, 0.436, -0.927, 0.999]];
/// Shadows and highlights: the most the hue's channels rise and the others fall (ln gain at full
/// weight) before the other side takes over the tint.
pub const GRADE_CAP: [[f32; 2]; 2] = [[0.941, 0.740], [0.300, 3.990]];
/// Balance ±100 moves the shadows / midtones / highlights weight centres by this many stops.
pub const GRADE_BALANCE: f32 = -2.625;
/// Blending (0..100, `b` = blending/100) moves the shadows centre up and the highlights centre
/// down by `GRADE_SPREAD · (b − ½)` stops (more overlap) and scales the shadows / midtones /
/// highlights widths by `1 + GRADE_BLEND · (b − ½)`. Lightroom treats a missing Blending like 100
/// (the legacy split-toning look); the centres above are those at Blending 50.
pub const GRADE_SPREAD: f32 = 4.234;
pub const GRADE_BLEND: f32 = -0.828;

/// Colour grading resolved for a render (see [`GRADE_WHEEL`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradeK {
    /// Per wheel: the ln gain per channel at full weight, `K·sat·(c − m) + K_L·lum`.
    pub d: [[f32; 3]; 4],
    /// Per wheel: weight centre and width, in log2 luminance.
    pub mu: [f32; 4],
    pub sg: [f32; 4],
}

impl GradeK {
    pub fn new(g: &lightcraft_develop::ColorGrading) -> GradeK {
        let balance = ((g.balance / 100.0) as f32).clamp(-1.0, 1.0);
        let b = ((g.blending / 100.0) as f32).clamp(0.0, 1.0) - 0.5;
        let (width, spread) = (1.0 + GRADE_BLEND * b, GRADE_SPREAD * b);
        let wheels = [&g.shadows, &g.midtones, &g.highlights, &g.global];
        let mut k = GradeK { d: [[0.0; 3]; 4], mu: [0.0; 4], sg: [0.0; 4] };
        for (i, w) in wheels.iter().enumerate() {
            let [kc, kl, mu, sg] = GRADE_WHEEL[i];
            let t = kc * ((w.sat / 100.0) as f32).clamp(0.0, 1.0);
            let c = hsv_to_rgb(w.hue.rem_euclid(360.0) as f32, 1.0, 1.0);
            let m = match i {
                0 | 2 if t > 0.0 => {
                    let [up, down] = GRADE_CAP[i / 2];
                    let y = PROPHOTO_LUMA[0] * c[0] + PROPHOTO_LUMA[1] * c[1] + PROPHOTO_LUMA[2] * c[2];
                    let lo = 1.0 - up / t;
                    y.clamp(lo, (down / t).max(lo))
                }
                _ => 0.5,
            };
            let lum = kl * ((w.lum / 100.0) as f32).clamp(-1.0, 1.0);
            k.d[i] = c.map(|v| t * (v - m) + lum);
            let shift = GRADE_BALANCE * balance + [spread, 0.0, -spread, 0.0][i];
            let tonal = i < 3;
            k.mu[i] = if tonal { mu + shift } else { mu };
            k.sg[i] = if tonal { sg * width } else { sg };
        }
        k
    }

    /// Graded linear ProPhoto `p`.
    #[inline]
    pub fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let y = PROPHOTO_LUMA[0] * p[0] + PROPHOTO_LUMA[1] * p[1] + PROPHOTO_LUMA[2] * p[2];
        let x = y.max(1e-6).log2();
        let sig = |t: f32| 1.0 / (1.0 + (-t).exp());
        let m = (x - self.mu[1]) / self.sg[1];
        let w = [sig((self.mu[0] - x) / self.sg[0]), (-m * m).exp(), sig((x - self.mu[2]) / self.sg[2]), sig((self.mu[3] - x) / self.sg[3])];
        std::array::from_fn(|c| p[c] * (w[0] * self.d[0][c] + w[1] * self.d[1][c] + w[2] * self.d[2][c] + w[3] * self.d[3][c]).exp())
    }
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

// ---- Colour mixer, fitted to Lightroom Classic 15.6 renders of a synthetic chart carrying an
// Apple ProRAW's own colour tags (every band's Hue / Saturation / Luminance at ±50 / ±100): the
// bands act in HSV of display-linear ProPhoto RGB (a hue the profile tone curve leaves alone),
// with raised-cosine weights between neighbouring band centres. Hue ±100 moves a band centre most
// of the way to the next / previous centre; Saturation scales HSV saturation (−100 all but greys
// it); Luminance scales HSV value, less for weakly saturated colours. Mean ΔE00 per slider at
// ±100: hue 0.1–0.3, saturation 0.2–1.7, luminance 0.6–1.8.

/// Band centres (HSV hue of linear ProPhoto, degrees): red, orange, yellow, green, aqua, blue,
/// purple, magenta.
pub const MIX_CENTERS: [f32; 8] = [0.0, 29.5, 54.0, 95.8, 159.7, 230.6, 275.6, 331.0];
/// Hue shift (degrees) at +100 / −100, per band.
const MIX_HUE: [[f32; 8]; 2] = [[30.1, 30.0, 35.5, 58.0, 62.8, 40.1, 49.1, 30.0], [30.1, 29.9, 29.6, 35.7, 58.3, 62.4, 40.2, 49.5]];
/// ln of the saturation factor at +100. Saturation scales a colour's spread around its HSL
/// lightness `(max + min) / 2` in linear ProPhoto: Lightroom's mixer is an HSL one (on its renders
/// of the photo under each preset's Saturation sliders, its own per-pixel factor applied that way
/// is within mean ΔE00 0.04–0.10; around the HSV value, which brightens desaturated colours,
/// 0.7–2.3). Measured at band centres on the chart at +50 / +100.
const MIX_SAT: [f32; 8] = [0.258, 0.295, 0.261, 0.277, 0.284, 0.277, 0.271, 0.305];
/// Negative Saturation scales the spread linearly, by `1 + k·a` at a band's centre (−100 greys a
/// band): k per band, measured like [`MIX_SAT`] at −50 / −100.
const MIX_DESAT: [f32; 8] = [0.987, 0.983, 0.965, 0.967, 0.989, 0.985, 0.977, 0.991];
/// Luminance multiplies a colour (all channels) by `(1 + a)^k` at a band's centre (a = amount / 100,
/// so −100 darkens to black): k per band, measured on the chart at −50 / +50 / +100 (fully
/// saturated patches: ±50 within 0.05 log2 of that law, a preset's Blue −19 within 0.03).
const MIX_LUM: [f32; 8] = [1.717, 1.636, 1.662, 1.612, 1.565, 1.63, 1.633, 1.636];
/// Less colourful colours take part of the Luminance gain `g`: `(s·∛v / MIX_LUM_CHROMA)^MIX_LUM_POW`
/// (at most 1), with HSV saturation `s` and value `v` — a perceptual chroma, so dark colours of
/// any saturation move little. The gain scales a colour's HSL lightness `(max + min) / 2`, its
/// spread around it only by `g^MIX_LUM_SPREAD` (brighter blues come out paler, as in Lightroom).
/// Fitted on Lightroom's renders of the photo under each preset's Luminance sliders and Blue +100:
/// mean ΔE00 0.18–0.32 per preset, 0.74 for Blue +100 (a plain gain on HSV saturation alone:
/// 0.29–0.59, 2.37).
pub const MIX_LUM_CHROMA: f32 = 0.4568;
pub const MIX_LUM_POW: f32 = 0.9604;
pub const MIX_LUM_SPREAD: f32 = 0.5675;

/// Partition-of-unity weights of HSV hue `h` (degrees) over the mixer bands (raised cosine between
/// neighbouring [`MIX_CENTERS`]).
#[inline]
pub fn mix_weights(h: f32) -> [f32; 8] {
    let mut w = [0.0f32; 8];
    for i in 0..8 {
        let a = MIX_CENTERS[i];
        let span = (MIX_CENTERS[(i + 1) % 8] - a).rem_euclid(360.0);
        let d = (h - a).rem_euclid(360.0);
        if d <= span {
            let s = 0.5 - 0.5 * (d / span * PI).cos();
            w[i] += 1.0 - s;
            w[(i + 1) % 8] += s;
            break;
        }
    }
    w
}

/// The table entry for a slider at `v` (−100..100) scaled by its amount.
fn mix_amount(table: &[[f32; 8]; 2], i: usize, v: f64) -> f32 {
    let a = (v.clamp(-100.0, 100.0) / 100.0) as f32;
    a * if a >= 0.0 { table[0][i] } else { table[1][i] }
}

/// A band's Luminance at `v` (−100..100) as a log2 gain ([`MIX_LUM`]), at most 10 stops darker.
fn mix_lum(i: usize, v: f64) -> f32 {
    let a = (v.clamp(-100.0, 100.0) / 100.0) as f32;
    (MIX_LUM[i] * (1.0 + a).max(1e-6).log2()).max(-10.0)
}

// ---- Vibrance and Saturation, fitted to the same Lightroom renders (at ±25 / ±50 / ±100), on
// display-linear ProPhoto. Vibrance scales HSV saturation, more for weakly saturated colours,
// raising (or lowering) value a little, and positive amounts spare skin tones (mean ΔE00
// 0.4 / 0.8 / 1.5 at +25 / +50 / +100, 1.0 / 2.3 at −25 / −50). Saturation scales the chroma
// around ProPhoto luminance, −100 giving exactly that grey (1.1 / 0.9 at −25 / −100, 1.0 / 1.9 at
// +25 / +50).

/// Vibrance > 0: ln saturation gain at +100, exponent of (1 − S), log2 value gain, skin weight.
pub const VIBRANCE_POS: [f32; 4] = [0.751, 0.736, 0.15, 0.391];
/// Vibrance < 0: ln saturation gain at −100, exponent of (1 − S), log2 value gain, exponent of S
/// on the value gain.
pub const VIBRANCE_NEG: [f32; 4] = [1.288, 0.323, 0.608, 0.263];
/// Skin tones Vibrance spares: HSV hue of linear ProPhoto and half-width (degrees).
pub const SKIN_HUE: [f32; 2] = [20.0, 35.0];
/// Saturation: chroma scale per unit at +100 and its (1 − S) exponent; at −100 the chroma goes.
pub const SATURATION_POS: [f32; 2] = [0.927, 0.076];
/// Luminance weights of linear ProPhoto RGB (D50).
pub const PROPHOTO_LUMA: [f32; 3] = [0.288_040_2, 0.711_874_1, 0.000_085_7];

/// Vibrance (−1..1) on linear ProPhoto `p`.
#[inline]
pub fn vibrance(p: [f32; 3], a: f32) -> [f32; 3] {
    let [h, s, v] = rgb_to_hsv(p);
    if v <= 0.0 || s <= 0.0 {
        return p;
    }
    let rest = (1.0 - s).max(0.0);
    let (e, dv) = if a > 0.0 {
        let d = (h - SKIN_HUE[0] + 180.0).rem_euclid(360.0) - 180.0;
        let skin = 1.0 - VIBRANCE_POS[3] * (-(d / SKIN_HUE[1]).powi(2)).exp();
        (a * VIBRANCE_POS[0] * rest.powf(VIBRANCE_POS[1]) * skin, a * VIBRANCE_POS[2] * skin)
    } else {
        (a * VIBRANCE_NEG[0] * rest.powf(VIBRANCE_NEG[1]), a * VIBRANCE_NEG[2] * s.min(1.0).powf(VIBRANCE_NEG[3]))
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
    /// Colour mixer per band: hue shift (degrees), ln saturation factor (positive sliders),
    /// linear saturation change (negative sliders, see [`MIX_DESAT`]), log2 value factor.
    pub hue: [f32; 8],
    pub sat: [f32; 8],
    pub desat: [f32; 8],
    pub lum: [f32; 8],
    pub mixer: bool,
    /// Point Color samples that change something (applied after the mixer).
    pub points: Vec<PointK>,
    pub bw: Option<[f32; 8]>,
    /// Colour grading (applied last, in linear ProPhoto).
    pub grading: Option<GradeK>,
}

impl ColorOps {
    pub fn new(s: &DevelopSettings) -> ColorOps {
        let bands = s.mixer.bands();
        let g = &s.grading;
        ColorOps {
            vibrance: (s.color.vibrance / 100.0) as f32,
            saturation: (s.color.saturation / 100.0) as f32,
            hue: std::array::from_fn(|i| mix_amount(&MIX_HUE, i, bands[i].hue)),
            sat: std::array::from_fn(|i| (bands[i].sat.clamp(0.0, 100.0) / 100.0) as f32 * MIX_SAT[i]),
            desat: std::array::from_fn(|i| (bands[i].sat.clamp(-100.0, 0.0) / 100.0) as f32 * MIX_DESAT[i]),
            lum: std::array::from_fn(|i| mix_lum(i, bands[i].lum)),
            mixer: !s.mixer.is_neutral(),
            points: if crate::is_bw(s) {
                Vec::new()
            } else {
                s.point_colors.iter().take(lightcraft_develop::MAX_POINT_COLORS).filter(|p| !p.is_neutral()).map(PointK::new).collect()
            },
            bw: crate::is_bw(s).then(|| s.bw_mix.bands().map(|v| (v / 100.0) as f32)),
            grading: (!g.is_neutral()).then(|| GradeK::new(g)),
        }
    }

    /// Mixer, Vibrance and Saturation on display-linear Rec.2020 `rgb`, in linear ProPhoto (see
    /// [`MIX_CENTERS`] and [`VIBRANCE_POS`]).
    #[inline]
    pub fn prophoto(&self, rgb: [f32; 3]) -> [f32; 3] {
        let (to, from) = crate::tone::prophoto_matrices();
        let mut p: [f32; 3] = std::array::from_fn(|i| to[i][0] * rgb[0] + to[i][1] * rgb[1] + to[i][2] * rgb[2]);
        if self.mixer {
            p = self.mix(p);
        }
        if self.vibrance != 0.0 {
            p = vibrance(p, self.vibrance);
        }
        if self.saturation != 0.0 {
            p = saturation(p, self.saturation);
        }
        std::array::from_fn(|i| from[i][0] * p[0] + from[i][1] * p[1] + from[i][2] * p[2])
    }

    /// The colour mixer on linear ProPhoto `p` (see [`MIX_CENTERS`]): hue, then saturation around
    /// the HSL lightness ([`MIX_SAT`]), then luminance.
    #[inline]
    fn mix(&self, p: [f32; 3]) -> [f32; 3] {
        let [h, s, v] = rgb_to_hsv(p);
        if v <= 0.0 || s <= 0.0 {
            return p;
        }
        let w = mix_weights(h);
        let (mut dh, mut ds, mut dd, mut dv) = (0.0, 0.0, 0.0, 0.0);
        for i in 0..8 {
            dh += w[i] * self.hue[i];
            ds += w[i] * self.sat[i];
            dd += w[i] * self.desat[i];
            dv += w[i] * self.lum[i];
        }
        let q = hsv_to_rgb(h + dh, s, v);
        let (hi, lo) = (q[0].max(q[1]).max(q[2]), q[0].min(q[1]).min(q[2]));
        let l = (hi + lo) / 2.0;
        // the spread factor, at most what keeps the smallest channel at or above 0
        let f = ds.exp() * (1.0 + dd).max(0.0);
        let f = if lo >= 0.0 && l > lo { f.min(l / (l - lo)) } else { f };
        // less colourful colours take part of the gain (a linear blend: −100 doesn't black them out)
        let g = 1.0 + (s * v.cbrt() / MIX_LUM_CHROMA).min(1.0).powf(MIX_LUM_POW) * (dv.exp2() - 1.0);
        let fs = f * g.max(0.0).powf(MIX_LUM_SPREAD);
        q.map(|c| l * g + (c - l) * fs)
    }

    /// Whether [`ColorOps::apply`] leaves colours alone (grading is separate: [`ColorOps::grade`]).
    pub fn is_identity(&self) -> bool {
        self.vibrance == 0.0 && self.saturation == 0.0 && !self.mixer && self.points.is_empty() && self.bw.is_none()
    }

    /// The colour tools that run before the tone curves (mixer, Vibrance, Saturation, Point
    /// Color, B&W); `local_sat` (−1..1) and `local_hue` (radians) come from masks.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3], local_sat: f32, local_hue: f32) -> [f32; 3] {
        if self.is_identity() && local_sat == 0.0 && local_hue == 0.0 {
            return rgb;
        }
        let rgb = if self.mixer || self.vibrance != 0.0 || self.saturation != 0.0 { self.prophoto(rgb) } else { rgb };
        if !self.points.is_empty() || self.bw.is_some() || local_sat != 0.0 || local_hue != 0.0 {
            return self.oklch(rgb, local_sat, local_hue);
        }
        rgb
    }

    /// Colour grading on display-linear Rec.2020 `rgb`, after the tone curves as in Lightroom
    /// (its wheels' luminance ranges are those of the curved image).
    #[inline]
    pub fn grade(&self, rgb: [f32; 3]) -> [f32; 3] {
        let Some(g) = &self.grading else { return rgb };
        let (to, from) = crate::tone::prophoto_matrices();
        let p = g.apply(std::array::from_fn(|i| to[i][0] * rgb[0] + to[i][1] * rgb[1] + to[i][2] * rgb[2]));
        std::array::from_fn(|i| from[i][0] * p[0] + from[i][1] * p[1] + from[i][2] * p[2])
    }

    /// Point Color, a mask's saturation / hue and the B&W mix, in OkLCh.
    #[inline]
    fn oklch(&self, rgb: [f32; 3], local_sat: f32, local_hue: f32) -> [f32; 3] {
        let lab = oklab_from_2020(rgb);
        // only a chroma change (a mask's saturation): scale a, b directly — the same result as the
        // OkLCh round trip without its sin / cos / atan2
        if self.points.is_empty() && self.bw.is_none() && local_hue == 0.0 {
            let k = (1.0 + local_sat).max(0.0);
            return oklab_to_2020([lab[0], lab[1] * k, lab[2] * k]);
        }
        let [mut l, mut c, mut h] = lab_to_lch(lab);
        for p in &self.points {
            [l, c, h] = p.apply([l, c, h]);
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
        oklab_to_2020(lch_to_lab([l, c, h]))
    }
}

/// Calibration primaries, per primary (red, green, blue), in linear ProPhoto: how far Hue ±100
/// moves the primary towards the next (`[0]`) / previous (`[1]`) primary, how far Saturation
/// ±100 takes it away from the other two (`[2]`, `[3]`), and a quadratic Saturation term (`[4]`).
/// Fitted to Lightroom Classic 15.6 renders of a synthetic chart carrying an Apple ProRAW's own
/// colour tags (every primary slider at ±50 / ±100 within mean ΔE00 1.2; the three reference
/// presets, which move all primaries at once, 0.8–1.3).
const CALIB_COEF: [[f64; 5]; 3] = [[0.308, 0.343, 0.392, 0.409, 0.034], [0.335, 0.334, 0.402, 0.402, 0.055], [0.344, 0.307, 0.389, 0.381, 0.003]];

/// The calibration panel's primaries as a white-preserving 3×3 matrix on linear Rec.2020
/// (row-major), `None` when neutral. Built in linear ProPhoto (D50, Bradford): each primary's
/// column gains `±k·amount` in the neighbouring channels (hue towards the next primary,
/// saturation away from both), the primaries' offsets add up, and each row is completed to sum
/// to 1 so greys stay grey.
pub fn calibration_matrix(c: &Calibration) -> Option<[[f32; 3]; 3]> {
    let prim = c.primaries();
    if prim.iter().all(|(h, s)| *h == 0.0 && *s == 0.0) {
        return None;
    }
    let mut m = Mat3::IDENTITY.0;
    for (p, (hue, sat)) in prim.iter().enumerate() {
        let [an, ap, bn, bp, q] = CALIB_COEF[p];
        let h = hue.clamp(-100.0, 100.0) / 100.0;
        let s = sat.clamp(-100.0, 100.0) / 100.0;
        let s = s * (1.0 + q * s);
        m[(p + 1) % 3][p] += an * h - bn * s;
        m[(p + 2) % 3][p] += -ap * h - bp * s;
    }
    for (i, row) in m.iter_mut().enumerate() {
        let sum: f64 = row.iter().sum();
        row[i] += 1.0 - sum;
    }
    let to = REC2020.to_space(&PROPHOTO);
    Some(to.inverse()?.mul(&Mat3(m)).mul(&to).to_f32())
}

/// Shadows-tint strength at ±100: the cut (×(1 − k)) of green (+) or of red and blue (−) in
/// the deepest shadows, fading out by [`SHADOW_TINT_RANGE`].
pub const SHADOW_TINT: f32 = 0.148;
/// Shadows-tint weight: 1 below, 0 above this range of log2(scene luminance / 0.18).
pub const SHADOW_TINT_RANGE: [f32; 2] = [-2.83, 2.63];

/// Calibration in scene-linear light (before tone mapping): the primaries matrix, then the shadows
/// tint, which (as in Lightroom, fitted to Lightroom Classic 15.6 renders at ±50) takes light away:
/// positive cuts green (towards magenta), negative cuts red and blue (towards green), most in the
/// shadows.
#[inline]
pub fn calibrate(c: [f32; 3], m: Option<&[[f32; 3]; 3]>, shadow_tint: f32) -> [f32; 3] {
    let mut c = c;
    if let Some(m) = m {
        c = std::array::from_fn(|r| (m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2]).max(0.0));
    }
    if shadow_tint != 0.0 {
        let y = lightcraft_color::luminance_2020(c);
        let w = 1.0 - smooth(SHADOW_TINT_RANGE[0], SHADOW_TINT_RANGE[1], (y.max(1e-7) / 0.18).log2());
        let k = (1.0 - SHADOW_TINT * shadow_tint.abs() * w).max(0.0);
        if shadow_tint > 0.0 {
            c[1] *= k;
        } else {
            c[0] *= k;
            c[2] *= k;
        }
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
            let s: f32 = mix_weights(i as f32).iter().sum();
            assert!((s - 1.0).abs() < 1e-4, "{i}: {s}");
        }
        // at a band centre, that band dominates
        let w = band_weights(band_hues()[5]);
        assert!(w[5] > 0.99);
        assert!(mix_weights(MIX_CENTERS[5])[5] > 0.99);
    }

    #[test]
    fn neutral_is_identity() {
        let ops = ColorOps::new(&DevelopSettings::default());
        assert!(ops.is_identity());
        assert_eq!(ops.apply([0.2, 0.3, 0.4], 0.0, 0.0), [0.2, 0.3, 0.4]);
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
        let chroma = |c: [f32; 3]| lab_to_lch(oklab_from_2020(c))[1];
        assert!(chroma(ops.apply(blue, 0.0, 0.0)) < 0.6 * chroma(blue));
        assert!((chroma(ops.apply(red, 0.0, 0.0)) - chroma(red)).abs() < 0.01);
        // hue +100 moves a band's centre most of the way to the next band's
        s.mixer.blue.sat = 0.0;
        s.mixer.red.hue = 100.0;
        let (to, _) = crate::tone::prophoto_matrices();
        let hue = |c: [f32; 3]| rgb_to_hsv(std::array::from_fn(|i| to[i][0] * c[0] + to[i][1] * c[1] + to[i][2] * c[2]))[0];
        let h1 = hue(ColorOps::new(&s).apply(red, 0.0, 0.0));
        assert!((h1 - hue(red) - MIX_HUE[0][0] * mix_weights(hue(red))[0]).abs() < 0.5, "{} -> {h1}", hue(red));
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
        assert!(dark[1] < dark[0] * 0.9, "magenta shadows: {dark:?}");
        assert!(dark[0] == 0.01 && dark[2] == 0.01, "only green is cut: {dark:?}");
        assert!((bright[1] - bright[0]).abs() < 0.005 * bright[0], "{bright:?}");
        let green = calibrate([0.01, 0.01, 0.01], None, -1.0);
        assert!(green[1] == 0.01 && green[0] < 0.01 && green[2] < 0.01, "red and blue are cut: {green:?}");
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
        let dark = ops.grade([0.01, 0.01, 0.01]);
        let bright = ops.grade([0.8, 0.8, 0.8]);
        assert!(dark[2] > 1.5 * dark[0], "{dark:?}");
        assert!(bright[2] < 1.2 * bright[0], "{bright:?}");
        // Balance towards the highlights narrows the shadows
        s.grading.balance = 100.0;
        let dark2 = ColorOps::new(&s).grade([0.01, 0.01, 0.01]);
        assert!(dark2[2] / dark2[0] < dark[2] / dark[0], "{dark2:?}");
    }
}
