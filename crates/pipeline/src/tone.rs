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
//! channel keeps its relative position between them, so the hue holds. Around it sits Lightroom
//! Classic's own global tone, measured (see the section below), so such files render as Lightroom
//! renders them (docs/parity.md, LR-PROF-CAMERACOLOR, LR-BEHAV-RENDER-FIDELITY).

use std::sync::LazyLock;

use lightcraft_color::{D50, D65, Mat3, PROPHOTO, REC2020, bradford};

pub const GREY: f32 = 0.18;
/// The tone LUT spans `LUT_MIN_EV..LUT_MAX_EV` around grey in `LUT_N` steps.
pub const LUT_MIN_EV: f32 = -14.0;
pub const LUT_MAX_EV: f32 = 10.0;
pub const LUT_N: usize = 4096;

// ---- Lightroom's global tone on sources with a DNG profile tone curve
//
// Measured on Lightroom Classic 15.6 renders of a chart that carries an Apple ProRAW's own colour
// tags (iPhone 12 Pro, CC0), with its `ProfileToneCurve` swapped for an identity curve so the
// renders show Lightroom's operators themselves ([`crate::lr_tables`]). Lightroom's chain, every
// stage hue-preserving in linear ProPhoto unless noted:
// 1. a base operator on the scene-linear value after BaselineExposure + Exposure (a small black
//    offset and a highlight shoulder, both depending on that total exposure: Exposure is not a
//    plain gain): [`lr_base`];
// 2. Highlights and Shadows, local ([`LrHs`]: measured on display values, carried back through
//    the profile curve when Whites / Blacks follow, see [`ToneMap::apply_rgb_hs`]);
// 3. Whites, then Blacks, as maps of the base output. Where they flatten tones (Whites < 0,
//    Blacks > 0) Lightroom keeps the channel ratios (a luminance gain) instead: [`lr_wb_ratio`];
// 4. the profile tone curve;
// 5. Contrast, as a map of display values after the curve (the same map at every exposure),
//    adapting to the image ([`lr_key`]).
// On the chart this reproduces Lightroom's default render within mean ΔE00 0.5 (grey ramp 0.08),
// and the reference presets' Contrast / Whites / Blacks / Exposure within 0.6–0.9. Whites /
// Blacks were measured at the file's own exposure; at other exposures Lightroom's maps shift
// somewhat (up to ~0.1 log2 on average at ±1 EV). The order of 2–5 comes from pairs of sliders on
// the photo: each pair's combined render follows from the single ones in this order only.

use crate::lr_tables::{
    LR_BASE, LR_BASE_EV, LR_BASE_KNOT0, LR_BLACKS, LR_BLACKS_AMT, LR_CONTRAST, LR_CONTRAST_ADAPT, LR_CONTRAST_AMT, LR_CONTRAST_KEY_REF,
    LR_CONTRAST_KEYS, LR_KEY_FLOOR, LR_KEY_PERCENTILE, LR_SLIDER_KNOT0, LR_TONE_STEP, LR_WHITES, LR_WHITES_AMT,
};

/// Number of knots of the Highlights / Shadows tables, from [`LR_KNOT0`] in steps of [`LR_KNOT_STEP`].
pub const LR_KNOTS: usize = 21;
pub const LR_KNOT0: f32 = -10.0;
pub const LR_KNOT_STEP: f32 = 0.5;
/// Reference amount the Highlights / Shadows tables were measured at.
pub const LR_REF_LOCAL: f32 = 60.0;
/// Gaussian σ of the Highlights / Shadows neighbourhood, as a fraction of the long edge.
pub const LR_CONTEXT_SIGMA: f32 = 0.064;

// Highlights and Shadows (measured like the rest, on the photo): log2 changes of display-linear
// luminance at knots of log2 display luminance −10, −9.5 … 0, a luminance gain after the tone map,
// scaled linearly with the amount. They act locally: their table is read at a blend of the
// pixel's and its neighbourhood's log luminance, `l + α·(ctx − l)`, ctx being the tone-mapped
// Gaussian blur of log luminance at [`LR_CONTEXT_SIGMA`] of the long edge (what explains
// Lightroom's change best: R² 0.89–0.96).

/// A local slider's table and its pixel / neighbourhood blend `alpha`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LrLocal {
    pub alpha: f32,
    pub tab: [f32; LR_KNOTS],
}

pub const LR_HIGHLIGHTS: [LrLocal; 2] = [
    LrLocal {
        alpha: 0.45,
        tab: [
            0.050, 0.058, 0.069, 0.078, 0.079, 0.076, 0.076, 0.072, 0.067, 0.065, 0.063, 0.062, 0.121, 0.323, 0.375, 0.441, 0.394, 0.356, 0.356,
            0.356, 0.356,
        ],
    },
    LrLocal {
        alpha: 0.45,
        tab: [
            -0.049, -0.057, -0.068, -0.078, -0.080, -0.077, -0.077, -0.073, -0.068, -0.066, -0.063, -0.062, -0.121, -0.336, -0.391, -0.462, -0.406,
            -0.357, -0.357, -0.357, -0.357,
        ],
    },
];
pub const LR_SHADOWS: [LrLocal; 2] = [
    LrLocal {
        alpha: 0.3,
        tab: [
            2.194, 2.171, 2.075, 1.900, 1.779, 1.600, 1.354, 1.106, 0.872, 0.671, 0.485, 0.316, 0.107, 0.043, 0.036, 0.028, 0.019, 0.012, 0.001,
            0.001, 0.001,
        ],
    },
    LrLocal {
        alpha: 0.5,
        tab: [
            -2.424, -2.178, -2.024, -1.979, -1.954, -1.707, -1.398, -1.259, -0.955, -0.671, -0.431, -0.193, -0.036, -0.034, -0.033, -0.026, -0.018,
            -0.012, -0.012, -0.012, -0.012,
        ],
    },
];

/// A tone table at log2 luminance `l` (linear between knots, the end values beyond them).
#[inline]
pub fn lr_knot(tab: &[f32; LR_KNOTS], l: f32) -> f32 {
    let f = ((l - LR_KNOT0) / LR_KNOT_STEP).clamp(0.0, (LR_KNOTS - 1) as f32);
    let i = (f as usize).min(LR_KNOTS - 2);
    let t = f - i as f32;
    tab[i] + (tab[i + 1] - tab[i]) * t
}

/// The table for a signed amount (`[positive, negative]`) and the amount relative to the reference.
#[inline]
fn signed<T>(pair: &[T; 2], v: f32, reference: f32) -> (&T, f32) {
    (if v >= 0.0 { &pair[0] } else { &pair[1] }, v.abs() / reference)
}

/// Rows of a [`crate::lr_tables`] table (one per sorted key: an exposure or a slider amount),
/// interpolated at key `k` (clamped to the measured keys) and read at log2 input `l`.
fn lr_rows<const N: usize>(keys: &[f32], rows: &[[f32; N]], k: f32, knot0: f32, l: f32) -> f32 {
    let n = keys.len().min(rows.len());
    let (Some(first), Some(last)) = (keys.first(), keys.get(n.wrapping_sub(1))) else {
        return 0.0;
    };
    let k = k.clamp(*first, *last);
    let i = keys.iter().take(n.saturating_sub(1)).rposition(|x| *x <= k).unwrap_or(0);
    let read = |j: usize| {
        rows.get(j).map_or(0.0, |r| {
            let f = ((l - knot0) / LR_TONE_STEP).clamp(0.0, (N.max(2) - 1) as f32);
            let m = (f as usize).min(N.saturating_sub(2));
            let (a, b) = (r.get(m).copied().unwrap_or(0.0), r.get(m + 1).copied().unwrap_or(0.0));
            a + (b - a) * (f - m as f32)
        })
    };
    match (keys.get(i), keys.get(i + 1)) {
        (Some(a), Some(b)) if b > a => {
            let t = (k - a) / (b - a);
            read(i) + (read(i + 1) - read(i)) * t
        }
        _ => read(i),
    }
}

/// Lightroom's base operator: scene-linear `x` (after all exposure) → its output at total exposure
/// `ev` (BaselineExposure + Exposure).
pub fn lr_base(x: f32, ev: f32) -> f32 {
    if x.is_nan() || x <= 0.0 || !x.is_finite() {
        return 0.0;
    }
    (x * lr_rows(&LR_BASE_EV, &LR_BASE, ev, LR_BASE_KNOT0, x.log2()).exp2()).min(1.0)
}

/// A Lightroom slider map (Blacks / Whites on the base output) at `amount` (−100..100) on value `v`.
fn lr_slider<const N: usize>(amounts: &[f32], rows: &[[f32; N]], amount: f32, v: f32) -> f32 {
    if amount == 0.0 || v.is_nan() || v <= 0.0 {
        return v.max(0.0);
    }
    (v * lr_rows(amounts, rows, amount, LR_SLIDER_KNOT0, v.log2()).exp2()).min(1.0)
}

/// Lightroom's Contrast (−100..100) on display value `v` after the profile curve. It adapts to the
/// image: with its luminance statistic `key` ([`lr_key`]) the map moves from the reference chart's
/// towards the rows measured at that statistic (the ±50 difference, scaled with the amount).
fn lr_contrast(amount: f32, key: Option<f32>, v: f32) -> f32 {
    if amount == 0.0 || v.is_nan() || v <= 0.0 {
        return v.max(0.0);
    }
    let l = v.log2();
    let mut t = lr_rows(&LR_CONTRAST_AMT, &LR_CONTRAST, amount, LR_SLIDER_KNOT0, l);
    if let Some(k) = key.filter(|k| k.is_finite()) {
        let rows = &LR_CONTRAST_ADAPT[usize::from(amount < 0.0)];
        let at = |k: f32| lr_rows(&LR_CONTRAST_KEYS, rows, k, LR_SLIDER_KNOT0, l);
        t += amount.abs() / 50.0 * (at(k) - at(LR_CONTRAST_KEY_REF));
    }
    (v * t.exp2()).min(1.0)
}

/// The image statistic Lightroom's Contrast adapts to: the [`LR_KEY_PERCENTILE`] percentile of
/// log2 luminance (floored at [`LR_KEY_FLOOR`]) of the image after the base operator at its own
/// exposure, from scene-linear Rec.2020 pixels that include the file's BaselineExposure
/// `baseline_ev`. `None` without pixels.
pub fn lr_key(pixels: &[[f32; 3]], baseline_ev: f32) -> Option<f32> {
    let table = tone_table(|x| lr_base(x, baseline_ev));
    let (to, _) = prophoto_matrices();
    let luma = crate::colorops::PROPHOTO_LUMA;
    let step = (pixels.len() / 250_000).max(1);
    let mut logs: Vec<f32> = pixels
        .iter()
        .step_by(step)
        .filter_map(|c| {
            let q = tone_rgb(&table, mul3(&to, *c));
            let y = luma[0] * q[0] + luma[1] * q[1] + luma[2] * q[2];
            y.is_finite().then(|| y.max(LR_KEY_FLOOR.exp2()).log2())
        })
        .collect();
    let at = ((logs.len().saturating_sub(1)) as f32 * LR_KEY_PERCENTILE) as usize;
    if logs.is_empty() {
        return None;
    }
    logs.select_nth_unstable_by(at, f32::total_cmp);
    logs.get(at).copied()
}

/// How much of Whites (`whites`) / Blacks moves colour by a luminance gain (channel ratios kept)
/// rather than hue-preserving like the curves: 1 where Lightroom keeps the ratios (Whites < 0,
/// most of Blacks > 0, growing with the amount), 0 where the channel spread follows the slope.
/// Fitted on the chart (Whites −25…−100 → 1.07, 0.99; Blacks +25 / +50 / +100 → 0.25 / 0.68 / 0.86).
pub fn lr_wb_ratio(whites: bool, amount: f32) -> f32 {
    match (whites, amount < 0.0) {
        (true, true) => 1.0,
        (false, false) => (amount / 100.0 * 1.4).clamp(0.0, 0.9),
        _ => 0.0,
    }
}

/// Lightroom's Highlights and Shadows for one render (tables and amounts relative to the
/// reference), `None` from [`LrHs::new`] when both are 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LrHs {
    pub hl: (LrLocal, f32),
    pub sh: (LrLocal, f32),
}

impl LrHs {
    /// From the sliders (−100..100).
    pub fn new(highlights: f64, shadows: f64) -> Option<LrHs> {
        if highlights == 0.0 && shadows == 0.0 {
            return None;
        }
        let (h, kh) = signed(&LR_HIGHLIGHTS, highlights as f32, LR_REF_LOCAL);
        let (s, ks) = signed(&LR_SHADOWS, shadows as f32, LR_REF_LOCAL);
        Some(LrHs { hl: (*h, kh), sh: (*s, ks) })
    }

    /// log2 gain for a pixel at display log2 luminance `l` (after the tone map) in a
    /// neighbourhood at `ctx`: each table read at `l + α·(ctx − l)`; the result stays at or below
    /// white.
    #[inline]
    pub fn gain(&self, l: f32, ctx: f32) -> f32 {
        let mut out = l;
        for (t, k) in [self.hl, self.sh] {
            if k != 0.0 {
                out += k * lr_knot(&t.tab, l + t.alpha * (ctx - l));
            }
        }
        out.min(l.max(0.0)) - l
    }
}

/// Most knots a [`CameraTone`] holds (a DNG `ProfileToneCurve` is sampled at this many).
pub const CAMERA_TONE_KNOTS: usize = 128;

/// A camera's tone curve: a file-local look fitted independently of the scene-linear colour
/// transform, or a DNG `ProfileToneCurve`. Knots are scene/display-linear luminance pairs. Keeping
/// this in the finish stage preserves RAW exposure and highlight headroom; it is never baked into
/// the decoded sensor pixels.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "CameraToneWire", into = "CameraToneWire")]
pub struct CameraTone {
    knots: [[f32; 2]; CAMERA_TONE_KNOTS],
    len: usize,
    /// Applied per channel (hue-preserving, linear ProPhoto RGB) instead of on luminance.
    rgb: bool,
    /// The file's BaselineExposure (EV), already in the decoded pixels: Lightroom's base tone
    /// depends on the total exposure ([`lr_base`]).
    baseline_ev: f32,
    /// The image statistic Lightroom's Contrast adapts to ([`lr_key`]), when known.
    key: Option<f32>,
}

/// [`CameraTone`]'s serialised form (smart previews carry it); curves saved before
/// `baseline_ev` / `key` existed read as 0 / unknown.
#[derive(serde::Serialize, serde::Deserialize)]
struct CameraToneWire {
    knots: Vec<[f32; 2]>,
    #[serde(default)]
    rgb: bool,
    #[serde(default)]
    baseline_ev: f32,
    #[serde(default)]
    key: Option<f32>,
}

impl TryFrom<CameraToneWire> for CameraTone {
    type Error = &'static str;
    fn try_from(w: CameraToneWire) -> Result<Self, Self::Error> {
        let tone = Self::new(&w.knots).ok_or("invalid camera tone curve")?;
        let tone = if w.rgb { tone.per_channel() } else { tone };
        Ok(tone.with_baseline_exposure(w.baseline_ev).with_key(w.key))
    }
}

impl From<CameraTone> for CameraToneWire {
    fn from(t: CameraTone) -> Self {
        CameraToneWire { knots: t.knots().to_vec(), rgb: t.rgb, baseline_ev: t.baseline_ev, key: t.key }
    }
}

impl CameraTone {
    /// 2 to [`CAMERA_TONE_KNOTS`] knots, increasing in input, non-decreasing and below 1 in output.
    pub fn new(knots: &[[f32; 2]]) -> Option<Self> {
        if !(2..=CAMERA_TONE_KNOTS).contains(&knots.len()) {
            return None;
        }
        let mut previous = [0.0, 0.0];
        for p in knots {
            if !p.iter().all(|v| v.is_finite()) || p[0] <= previous[0] || p[1] < previous[1] || p[1] >= 1.0 {
                return None;
            }
            previous = *p;
        }
        let mut all = [[0.0; 2]; CAMERA_TONE_KNOTS];
        all.get_mut(..knots.len())?.copy_from_slice(knots);
        Some(Self { knots: all, len: knots.len(), rgb: false, baseline_ev: 0.0, key: None })
    }

    fn knots(&self) -> &[[f32; 2]] {
        self.knots.get(..self.len).unwrap_or(&[])
    }

    /// The same curve applied to RGB (hue-preserving, linear ProPhoto) instead of luminance; used
    /// for a DNG `ProfileToneCurve`.
    pub fn per_channel(self) -> Self {
        Self { rgb: true, ..self }
    }

    /// Whether the curve is applied per channel (a DNG `ProfileToneCurve`).
    pub fn is_per_channel(&self) -> bool {
        self.rgb
    }

    /// The file's BaselineExposure (EV) for Lightroom's base tone (non-finite values read as 0).
    pub fn with_baseline_exposure(self, ev: f32) -> Self {
        Self { baseline_ev: if ev.is_finite() { ev.clamp(-10.0, 10.0) } else { 0.0 }, ..self }
    }

    /// The image statistic Lightroom's Contrast adapts to ([`lr_key`]; non-finite reads as unknown).
    pub fn with_key(self, key: Option<f32>) -> Self {
        Self { key: key.filter(|k| k.is_finite()).map(|k| k.clamp(-30.0, 10.0)), ..self }
    }

    /// The file's BaselineExposure (EV) this curve was built with.
    pub fn baseline_exposure(&self) -> f32 {
        self.baseline_ev
    }

    pub fn apply(&self, y: f32) -> f32 {
        if !y.is_finite() || y <= 0.0 {
            return 0.0;
        }
        let knots = self.knots();
        let mut previous = [0.0, 0.0];
        for p in knots {
            if y <= p[0] {
                let t = (y - previous[0]) / (p[0] - previous[0]);
                return previous[1] + t * (p[1] - previous[1]);
            }
            previous = *p;
        }
        let (Some(a), Some(b)) = (knots.get(knots.len().wrapping_sub(2)), knots.last()) else {
            return 0.0;
        };
        // Extend beyond observed (unclipped) highlights with a continuous, bounded shoulder.
        let slope = ((b[1] - a[1]) / (b[0] - a[0])).clamp(0.1, 16.0);
        1.0 - (1.0 - b[1]) * (-(y - b[0]) * slope / (1.0 - b[1]).max(0.01)).exp()
    }
}

/// Lightroom's tone in separate stages, needed when Whites or Blacks move colour by a luminance
/// gain (see the module docs): each table spans the same grid as [`ToneMap`]'s. Lightroom's order:
/// the base operator, Highlights / Shadows ([`ToneMap::apply_rgb_hs`]), Whites, Blacks, the
/// profile tone curve, Contrast (on its pairs of sliders, each order but this one misses
/// Lightroom's combined render by 0.02–0.06 log2 in the shadows).
#[derive(Clone, Debug)]
pub struct ToneStages {
    /// The base operator (hue-preserving).
    pub pre: Vec<f32>,
    /// Blacks and Whites: table and [`lr_wb_ratio`] (Whites runs first).
    pub blacks: Option<(Vec<f32>, f32)>,
    pub whites: Option<(Vec<f32>, f32)>,
    /// The profile tone curve, then Contrast unless it runs separately (hue-preserving).
    pub post: Vec<f32>,
    /// The profile tone curve alone and its inverse (Highlights / Shadows act on display values
    /// before Whites / Blacks), and the base operator with the curve (the neighbourhood they read).
    pub profile: Vec<f32>,
    pub profile_inv: Vec<f32>,
    pub context: Vec<f32>,
}

#[derive(Clone, Debug)]
pub struct ToneMap {
    /// The whole map on luminance (greys), and per channel when there are no [`ToneStages`].
    lut: Vec<f32>,
    /// Rec.2020 → ProPhoto and back when applied per channel ([`CameraTone::per_channel`]).
    rgb: Option<([[f32; 3]; 3], [[f32; 3]; 3])>,
    stages: Option<Box<ToneStages>>,
    /// Contrast on its own, on display values ([`ToneMap::apply_contrast_rgb`]): when Highlights /
    /// Shadows run between the profile tone curve and Contrast, as in Lightroom (`lut` and
    /// `stages` then end at the profile curve).
    contrast: Option<Vec<f32>>,
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

/// A tone table on the [`ToneMap`] grid (`LUT_N` entries over `LUT_MIN_EV..LUT_MAX_EV` around
/// grey), kept monotone.
fn tone_table(f: impl Fn(f32) -> f32) -> Vec<f32> {
    let mut floor = 0.0f32;
    (0..LUT_N)
        .map(|i| {
            let ev = LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32;
            floor = floor.max(f(GREY * ev.exp2()));
            floor
        })
        .collect()
}

/// The inverse of the increasing map `f` as a tone table (bisection in log2).
fn inverse_table(f: impl Fn(f32) -> f32) -> Vec<f32> {
    tone_table(|y| {
        let (mut a, mut b) = (LUT_MIN_EV - 10.0, LUT_MAX_EV);
        for _ in 0..40 {
            let m = 0.5 * (a + b);
            if f(GREY * m.exp2()) < y {
                a = m;
            } else {
                b = m;
            }
        }
        GREY * (0.5 * (a + b)).exp2()
    })
}

/// Table `lut` at `y` (linear below the grid's first entry, the last value above it).
#[inline]
fn tone_eval(lut: &[f32], y: f32) -> f32 {
    if y <= 0.0 || lut.len() < 2 {
        return 0.0;
    }
    let ev = (y / GREY).log2();
    let f = ((ev - LUT_MIN_EV) / (LUT_MAX_EV - LUT_MIN_EV)).clamp(0.0, 1.0) * (lut.len() - 1) as f32;
    let i = (f as usize).min(lut.len() - 2);
    let t = f - i as f32;
    let (a, b) = (lut.get(i).copied().unwrap_or(0.0), lut.get(i + 1).copied().unwrap_or(0.0));
    let v = a + (b - a) * t;
    if ev < LUT_MIN_EV { v * (y / (GREY * LUT_MIN_EV.exp2())) } else { v }
}

/// Table `lut` on linear ProPhoto `p`, hue-preserving: the largest and smallest channel go through
/// it, the middle one keeps its relative position between them.
#[inline]
fn tone_rgb(lut: &[f32], p: [f32; 3]) -> [f32; 3] {
    let (hi, lo) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
    let (th, tl) = (tone_eval(lut, hi), tone_eval(lut, lo));
    if hi - lo > 1e-9 { p.map(|v| tl + (th - tl) * (v - lo) / (hi - lo)) } else { [th; 3] }
}

/// Whites / Blacks on linear ProPhoto `p`: hue-preserving, moved towards a luminance gain (channel
/// ratios kept) by `ratio`.
#[inline]
fn tone_wb(lut: &[f32], ratio: f32, p: [f32; 3]) -> [f32; 3] {
    let curve = tone_rgb(lut, p);
    if ratio == 0.0 {
        return curve;
    }
    let luma = crate::colorops::PROPHOTO_LUMA;
    let y = luma[0] * p[0] + luma[1] * p[1] + luma[2] * p[2];
    let k = if y > 1e-9 { tone_eval(lut, y) / y } else { 0.0 };
    std::array::from_fn(|i| curve[i] + ratio * (p[i] * k - curve[i]))
}

impl ToneMap {
    /// The camera's tone curve with Contrast / Whites / Blacks. For a DNG profile tone curve
    /// ([`CameraTone::per_channel`]): Lightroom's chain around it (module docs; `exposure` is the
    /// Exposure slider, EV, already applied to the pixels). Else the slider adjustments of a
    /// rendered image ([`ToneMap::display`]) after the curve.
    pub fn camera(curve: &CameraTone, exposure: f64, contrast: f64, whites: f64, blacks: f64) -> ToneMap {
        Self::camera_split(curve, exposure, contrast, whites, blacks, false)
    }

    /// [`ToneMap::camera`] for a render where Highlights / Shadows run (`hs`): on a DNG profile tone
    /// curve Lightroom runs them after the base operator, before Whites / Blacks and Contrast (with
    /// Contrast ±35 / ±70 and Highlights −36 or Shadows +39, Contrast applied to their result
    /// predicts Lightroom's combined render within 0.02 log2, the other order misses by up to
    /// 0.23). So Contrast becomes a separate last stage ([`ToneMap::apply_contrast_rgb`]) and, with
    /// Whites / Blacks, Highlights / Shadows run inside the map ([`ToneMap::apply_rgb_hs`]).
    pub fn camera_split(curve: &CameraTone, exposure: f64, contrast: f64, whites: f64, blacks: f64, hs: bool) -> ToneMap {
        if !curve.rgb {
            let adjustment = Self::display(contrast, whites, blacks);
            let neutral = contrast == 0.0 && whites == 0.0 && blacks == 0.0;
            let lut = tone_table(|x| {
                let y = curve.apply(x);
                if neutral { y } else { adjustment.apply(y) }
            });
            return ToneMap { lut, rgb: None, stages: None, contrast: None };
        }
        let ev = curve.baseline_ev + exposure as f32;
        let (c, w, b) = (contrast as f32, whites as f32, blacks as f32);
        let split = hs && c != 0.0;
        let base = |x: f32| lr_base(x, ev);
        let bk = |v: f32| lr_slider(&LR_BLACKS_AMT, &LR_BLACKS, b, v);
        let wh = |v: f32| lr_slider(&LR_WHITES_AMT, &LR_WHITES, w, v);
        let con = |v: f32| lr_contrast(c, curve.key, v);
        let post = |v: f32| if split { curve.apply(v) } else { con(curve.apply(v)) };
        let lut = tone_table(|x| post(bk(wh(base(x)))));
        let stages = (b != 0.0 || w != 0.0).then(|| {
            // Highlights / Shadows' tables only when they run (between the base operator and
            // Whites / Blacks, see [`ToneMap::apply_rgb_hs`])
            let profile = |v: f32| curve.apply(v);
            Box::new(ToneStages {
                pre: tone_table(base),
                blacks: (b != 0.0).then(|| (tone_table(bk), lr_wb_ratio(false, b))),
                whites: (w != 0.0).then(|| (tone_table(wh), lr_wb_ratio(true, w))),
                post: tone_table(post),
                profile: if hs { tone_table(profile) } else { Vec::new() },
                profile_inv: if hs { inverse_table(profile) } else { Vec::new() },
                context: if hs { tone_table(|x| curve.apply(base(x))) } else { Vec::new() },
            })
        });
        ToneMap { lut, rgb: Some(prophoto_matrices()), stages, contrast: split.then(|| tone_table(con)) }
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
        ToneMap { lut, rgb: None, stages: None, contrast: None }
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
        ToneMap { lut, rgb: None, stages: None, contrast: None }
    }

    /// The table (`LUT_N` entries, see [`ToneMap::apply`]).
    pub fn lut(&self) -> &[f32] {
        &self.lut
    }

    /// The separate stages [`ToneMap::apply_rgb`] runs when Whites or Blacks are set on a DNG
    /// profile tone curve.
    pub fn stages(&self) -> Option<&ToneStages> {
        self.stages.as_deref()
    }

    /// Whether the curve is applied per channel ([`ToneMap::apply_rgb`]) rather than to luminance.
    pub fn per_channel(&self) -> bool {
        self.rgb.is_some()
    }

    /// Scene luminance → display-linear luminance.
    #[inline]
    pub fn apply(&self, y: f32) -> f32 {
        tone_eval(&self.lut, y)
    }

    /// Scene-linear Rec.2020 → display-linear Rec.2020, per channel and hue-preserving: in linear
    /// ProPhoto RGB the largest and smallest channel go through the curve and the middle one keeps
    /// its relative position between them (Lightroom's Whites / Blacks partly as a luminance gain,
    /// see [`ToneStages`]). A map built without [`CameraTone::per_channel`] has no matrices and
    /// applies the curve to each Rec.2020 channel the same way.
    #[inline]
    pub fn apply_rgb(&self, c: [f32; 3]) -> [f32; 3] {
        self.apply_rgb_hs(c, |_| 0.0)
    }

    /// Whether Highlights / Shadows run inside the map ([`ToneMap::apply_rgb_hs`]): Whites /
    /// Blacks on a DNG profile tone curve, built for Highlights / Shadows ([`ToneMap::camera_split`]).
    pub fn hs_inside(&self) -> bool {
        self.stages.as_ref().is_some_and(|s| !s.profile.is_empty())
    }

    /// The display luminance the neighbourhood of Highlights / Shadows is read at, for scene
    /// luminance `y`: before Whites / Blacks and Contrast when they run inside the map.
    #[inline]
    pub fn apply_context(&self, y: f32) -> f32 {
        match &self.stages {
            Some(s) if !s.context.is_empty() => tone_eval(&s.context, y),
            _ => tone_eval(&self.lut, y),
        }
    }

    /// [`ToneMap::apply_rgb`] with Highlights / Shadows in Lightroom's place when they run inside
    /// the map ([`ToneMap::hs_inside`]): `hs` gets the pixel's display luminance after the base
    /// operator and the profile curve, and returns a log2 gain on it, carried back through the
    /// profile curve onto the base operator's output before Whites and Blacks.
    #[inline]
    pub fn apply_rgb_hs(&self, c: [f32; 3], hs: impl Fn(f32) -> f32) -> [f32; 3] {
        let p = match &self.rgb {
            Some((to, _)) => mul3(to, c),
            None => c,
        };
        let q = match &self.stages {
            Some(s) => {
                let mut q = tone_rgb(&s.pre, p);
                if !s.profile.is_empty() {
                    let luma = crate::colorops::PROPHOTO_LUMA;
                    let shown = tone_rgb(&s.profile, q);
                    let yd = luma[0] * shown[0] + luma[1] * shown[1] + luma[2] * shown[2];
                    let k = if yd > 1e-9 { hs(yd) } else { 0.0 };
                    if k != 0.0 {
                        let at = tone_eval(&s.profile_inv, yd);
                        if at > 1e-12 {
                            let g = tone_eval(&s.profile_inv, yd * k.exp2()) / at;
                            q = q.map(|v| v * g);
                        }
                    }
                }
                for (lut, ratio) in [&s.whites, &s.blacks].into_iter().flatten() {
                    q = tone_wb(lut, *ratio, q);
                }
                tone_rgb(&s.post, q)
            }
            None => tone_rgb(&self.lut, p),
        };
        match &self.rgb {
            Some((_, from)) => mul3(from, q),
            None => q,
        }
    }

    /// The separate Contrast stage ([`ToneMap::camera_split`]) on display-linear Rec.2020 `d`,
    /// hue-preserving in linear ProPhoto like the rest of the map; `d` itself without one.
    #[inline]
    pub fn apply_contrast_rgb(&self, d: [f32; 3]) -> [f32; 3] {
        match (&self.contrast, &self.rgb) {
            (Some(t), Some((to, from))) => mul3(from, tone_rgb(t, mul3(to, d))),
            _ => d,
        }
    }

    /// The separate Contrast table ([`ToneMap::apply_contrast_rgb`]), if any.
    pub fn contrast_table(&self) -> Option<&[f32]> {
        self.contrast.as_deref()
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
        let knots: [[f32; 2]; 32] = std::array::from_fn(|i| {
            let x = 0.005 * 1.15f32.powi(i as i32);
            [x, 1.0 - (-3.0 * x).exp()]
        });
        let curve = CameraTone::new(&knots).unwrap();
        let base = ToneMap::camera(&curve, 0.0, 0.0, 0.0, 0.0);
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
        assert!(ToneMap::camera(&curve, 0.0, 50.0, 0.0, 0.0).apply(0.3) > base.apply(0.3));
        let mut invalid = knots;
        invalid[1][0] = invalid[0][0];
        assert!(CameraTone::new(&invalid).is_none());
        assert!(CameraTone::new(&[[0.5, 0.5]]).is_none(), "one knot");
        assert!(CameraTone::new(&[[0.1, 0.1]; CAMERA_TONE_KNOTS + 1]).is_none(), "too many knots");
        assert!(serde_json::from_value::<CameraTone>(serde_json::json!({"knots": invalid})).is_err());
    }

    /// A DNG profile tone curve applied per channel keeps greys on the curve, keeps the hue (in
    /// ProPhoto, where it is applied), adds the saturation a contrast curve gives, and survives
    /// serialisation (smart previews carry it); older serialised curves stay luminance curves.
    #[test]
    fn per_channel_camera_tone_keeps_hue_and_greys() {
        let knots: [[f32; 2]; 32] = std::array::from_fn(|i| {
            let x = 2f32.powf(-12.0 + 12.0 * i as f32 / 31.0);
            // an S-curve in linear light (darker shadows, brighter mids)
            [x, (x * x * (3.0 - 2.0 * x)).min(0.9995)]
        });
        let luma = CameraTone::new(&knots).unwrap();
        let rgb = luma.per_channel();
        let t = ToneMap::camera(&rgb, 0.0, 0.0, 0.0, 0.0);
        assert!(t.per_channel() && !ToneMap::camera(&luma, 0.0, 0.0, 0.0, 0.0).per_channel());
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
        let luminance_tone = ToneMap::camera(&luma, 0.0, 0.0, 0.0, 0.0);
        for c in [[0.4f32, 0.2, 0.1], [0.05, 0.3, 0.12], [0.2, 0.25, 0.6]] {
            let out = t.apply_rgb(c);
            let (h0, h1) = (hue(c), hue(out));
            assert!((h0.0 - h1.0).abs() < 1e-3 && h0.1 == h1.1 && h0.2 == h1.2, "{c:?} → {out:?}");
            // an S-curve saturates where the luminance curve keeps the ratios
            let y = lightcraft_color::luminance_2020(c);
            let by_luma = c.map(|v| v * luminance_tone.apply(y) / y);
            assert!(sat(out) > sat(by_luma) + 0.01, "{c:?}: {} vs {}", sat(out), sat(by_luma));
        }
        let json = serde_json::to_value(rgb.with_baseline_exposure(-0.3)).unwrap();
        assert_eq!(serde_json::from_value::<CameraTone>(json).unwrap(), rgb.with_baseline_exposure(-0.3));
        assert_eq!(serde_json::from_value::<CameraTone>(serde_json::json!({ "knots": knots })).unwrap(), luma);
    }

    /// Lightroom's chain on a DNG profile curve: monotone and bounded for any slider mix and
    /// exposure; greys take the same path whether staged (Whites / Blacks set) or not; Whites < 0
    /// keeps a colour's channel ratios (a luminance gain) where Whites > 0 spreads them like the
    /// curve; Exposure moves the base operator (not only a gain).
    #[test]
    fn lightroom_chain_on_a_profile_curve() {
        let knots: [[f32; 2]; 64] = std::array::from_fn(|i| {
            let x = 2f32.powf(-12.0 + 12.0 * i as f32 / 63.0);
            [x, (x.powf(0.8) * 0.98).min(0.9995)]
        });
        let curve = CameraTone::new(&knots).unwrap().per_channel().with_baseline_exposure(-0.3);
        for (e, c, w, b) in
            [(0.0, 0.0, 0.0, 0.0), (0.0, 43.0, -56.0, 50.0), (-0.05, -35.0, -28.0, 22.0), (2.5, 100.0, 100.0, -100.0), (-3.0, -100.0, -100.0, 100.0)]
        {
            let t = ToneMap::camera(&curve, e, c, w, b);
            assert_eq!(t.stages().is_some(), w != 0.0 || b != 0.0);
            let mut prev = -1.0;
            for i in 0..1500 {
                let y = 1e-6 * 1.012f32.powi(i);
                let o = t.apply(y);
                assert!((0.0..=1.0).contains(&o) && o >= prev - 1e-6, "{e} {c} {w} {b} at {y}: {o} after {prev}");
                prev = o;
                // (each stage's table interpolates on its own: up to ~1.5 % apart where Blacks −100
                // crushes the toe)
                let tol = if b.abs() >= 100.0 { 2e-2 } else { 2e-3 };
                let g = t.apply_rgb([y; 3]);
                assert!(g.iter().all(|v| (v - o).abs() < tol * o + 1e-5), "{e} {c} {w} {b} grey {y}: {g:?} vs {o}");
            }
        }
        let (to, _) = prophoto_matrices();
        let ratio = |c: [f32; 3]| {
            let p = mul3(&to, c);
            p[0] / p[2]
        };
        let colour = [0.3f32, 0.15, 0.08];
        let base = ToneMap::camera(&curve, 0.0, 0.0, 0.0, 0.0).apply_rgb(colour);
        let lower = ToneMap::camera(&curve, 0.0, 0.0, -60.0, 0.0).apply_rgb(colour);
        let higher = ToneMap::camera(&curve, 0.0, 0.0, 60.0, 0.0).apply_rgb(colour);
        let moved = |c: [f32; 3]| (ratio(c) / ratio(base) - 1.0).abs();
        assert!(moved(higher) > 0.02 && moved(lower) < moved(higher) / 2.0, "{base:?} → lower {lower:?}, higher {higher:?}");
        // the same scene value comes out darker at +1 EV: the base operator's shoulder grows with
        // the exposure (Exposure is not a plain gain)
        let (e0, e1) = (ToneMap::camera(&curve, 0.0, 0.0, 0.0, 0.0), ToneMap::camera(&curve, 1.0, 0.0, 0.0, 0.0));
        assert!(e1.apply(0.8) < e0.apply(0.8) - 0.005, "{} vs {}", e1.apply(0.8), e0.apply(0.8));
        // Contrast adapts to the image: its pivot sits lower on a dark image, so +50 lowers a
        // shadow value less there than on a bright one; the image's key comes from its pixels
        let (dark, bright) = (vec![[0.01f32; 3]; 64], vec![[0.5f32; 3]; 64]);
        let (kd, kb) = (lr_key(&dark, -0.3), lr_key(&bright, -0.3));
        assert!(kd.zip(kb).is_some_and(|(d, b)| d < b - 2.0), "{kd:?} {kb:?}");
        let shadow = |key| ToneMap::camera(&curve.with_key(key), 0.0, 50.0, 0.0, 0.0).apply(0.02);
        assert!(shadow(kd) > shadow(kb) * 1.05, "{} vs {}", shadow(kd), shadow(kb));
        assert_eq!(lr_key(&[], -0.3), None);
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
