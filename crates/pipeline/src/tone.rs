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
//! Apple ProRAW's DNG `ProfileToneCurve` ([`CameraTone::per_channel`]) is applied to RGB instead
//! of luminance: in linear ProPhoto RGB, the curve maps the largest and smallest channel, and the
//! middle channel keeps its relative position between them, so the hue holds. Lightroom Classic's
//! own global tone sits around it as analytic operators fitted to its renders (see the section
//! below), so such files render as Lightroom renders them (docs/parity.md,
//! LR-BEHAV-RENDER-FIDELITY). Other DNGs' profile curves, measured on no camera, stay luminance
//! curves with the sliders after them, like a fitted camera look.

use std::sync::LazyLock;

use lightcraft_color::{D50, D65, Mat3, PROPHOTO, REC2020, bradford};

pub const GREY: f32 = 0.18;
/// The tone LUT spans `LUT_MIN_EV..LUT_MAX_EV` around grey in `LUT_N` steps.
pub const LUT_MIN_EV: f32 = -14.0;
pub const LUT_MAX_EV: f32 = 10.0;
pub const LUT_N: usize = 4096;
/// Nodes of a camera chroma curve, evenly spaced over display luminance 0..=1.
pub const CHROMA_N: usize = 8;
const NO_CHROMA: [f32; CHROMA_N] = [1.0; CHROMA_N];

// ---- Lightroom's global tone on Apple ProRAW
//
// Closed-form operators with a few coefficients each, fitted to Lightroom Classic 15.6 renders of a
// synthetic chart carrying a CC0 iPhone 12 Pro ProRAW's colour tags (its `ProfileToneCurve` swapped
// for an identity curve, so the renders show Lightroom's operators themselves: a 46-patch grey
// ramp per setting), and Highlights / Shadows to renders of the photo itself. Residuals are log2
// RMS over the measured patches; "ΔE" is how far the photo's mean CIEDE2000 to Lightroom's render
// moves from that of the measured responses themselves, sampled on a grid (over its 91 reference
// renders, 1.176 → 1.178). Lightroom's chain, every stage hue-preserving in linear ProPhoto unless
// noted:
// 1. a base operator on the scene-linear value after BaselineExposure + Exposure (a toe and a
//    highlight shoulder, both depending on that total exposure: Exposure is not a plain gain):
//    [`lr_base`];
// 2. Highlights and Shadows, local ([`LrHs`]: measured on display values, carried back through
//    the profile curve when Whites / Blacks follow, see [`ToneMap::apply_rgb_hs`]);
// 3. Whites ([`lr_whites`]), then Blacks ([`lr_blacks`]), as maps of the base output. Where they
//    flatten tones (Whites < 0, Blacks > 0) Lightroom keeps the channel ratios (a luminance gain)
//    instead: [`lr_wb_ratio`];
// 4. the profile tone curve;
// 5. Contrast ([`lr_contrast`]), as a map of display values after the curve (the same map at
//    every exposure), adapting to the image ([`lr_key`]).
// Whites / Blacks were measured at the file's own exposure; at other exposures Lightroom's maps
// shift somewhat (up to ~0.1 log2 on average at ±1 EV). The order of 2–5 comes from pairs of
// sliders on the photo: each pair's combined render follows from the single ones in this order
// only. Only Apple ProRAW was measured, so only it takes this chain ([`crate::finish::lr_tone`]).

/// Knots of the evaluation grid [`LrLocal`] samples Highlights / Shadows on, from [`LR_KNOT0`] in
/// steps of [`LR_KNOT_STEP`] (log2 display luminance); the GPU reads the same grid.
pub const LR_KNOTS: usize = 105;
pub const LR_KNOT0: f32 = -12.0;
pub const LR_KNOT_STEP: f32 = 0.125;
/// Reference amount of the Highlights / Shadows curves (the photo was rendered at ±60; the change
/// scales linearly with the amount).
pub const LR_REF_LOCAL: f32 = 60.0;
/// Gaussian σ of the Highlights / Shadows neighbourhood, as a fraction of the long edge.
pub const LR_CONTEXT_SIGMA: f32 = 0.064;
/// Floor (log2) of the luminance statistic Contrast adapts to ([`lr_key`]).
pub const LR_KEY_FLOOR: f32 = -8.0;
/// The percentile of log2 luminance Contrast adapts to ([`lr_key`]): the photo's Contrast ±50 at
/// Exposure −1 / 0 / +1 matches at keys −3.9…−4.5 (mean: −3.42, 33rd percentile: −4.08).
pub const LR_KEY_PERCENTILE: f32 = 0.33;
/// The keys Contrast was measured at (charts on backgrounds from near black to near white); its
/// pivot holds its end values beyond them.
const LR_KEY_RANGE: (f32, f32) = (LR_KEY_FLOOR, -0.47);
/// The total exposures (EV) the base operator was measured at; its coefficients hold their end
/// values beyond them (the exposure itself still applies).
const LR_EV_RANGE: (f32, f32) = (-2.302, 1.698);

/// ln(1 + eˣ), without overflow.
#[inline]
fn softplus(x: f32) -> f32 {
    x.max(0.0) + (-x.abs()).exp().ln_1p()
}

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Lightroom's base operator: scene-linear `x` (after all exposure) → its output at total exposure
/// `ev` (BaselineExposure + Exposure).
///
/// With l = log2 x and e = `ev` (clamped to the measured −2.3…+1.7 EV):
/// - a shoulder above the knee K(e): s = l − a(e)·d², d = min(max(l − K, 0), 1/(2a)) (past its flat
///   top the curve rises again as a gain, so it stays increasing up to white), with K a cubic in e
///   and a(e) = (a₀ + a₁e + a₂e²)·smoothstep((e + 1.0) / 0.5): no shoulder below −1 EV, a growing
///   one above;
/// - a toe subtracting a power of the result: y = 2ˢ − 2^(t(e) + g(e)·(s + 6)), t quadratic in e,
///   g(e) = g₀ − g₁·¼·softplus(4(e − e_g)) (a constant offset at high exposure, a softer toe below).
///
/// 14 coefficients, fitted to the grey ramp at 12 total exposures from −2.3 to +1.7 EV: residual
/// 0.003 log2 RMS at the file's own −0.3 EV, 0.011 within −1.3…+1 EV, 0.015 over all (worst at
/// −2.3 and +1.5 EV, where Lightroom's shoulder changes shape abruptly). ΔE −0.004 on the default
/// render, +0.021 / +0.035 at Exposure +1 / −1.
pub fn lr_base(x: f32, ev: f32) -> f32 {
    const T: [f32; 3] = [-8.51604, 0.548665, -0.0192987];
    const G: [f32; 3] = [0.197681, 0.176065, 0.210152];
    const A: [f32; 4] = [0.0852097, 0.0375456, -0.000335723, -0.998057];
    const K: [f32; 4] = [-2.00526, -0.607198, 0.774938, -0.193873];
    if x.is_nan() || x <= 0.0 || !x.is_finite() {
        return 0.0;
    }
    let e = if ev.is_finite() { ev.clamp(LR_EV_RANGE.0, LR_EV_RANGE.1) } else { 0.0 };
    let toe = T[0] + e * (T[1] + e * T[2]);
    let g = G[0] - G[1] * 0.25 * softplus((e - G[2]) / 0.25);
    let on = ((e - A[3]) / 0.5).clamp(0.0, 1.0);
    let a = (A[0] + e * (A[1] + e * A[2])) * on * on * (3.0 - 2.0 * on);
    let knee = K[0] + e * (K[1] + e * (K[2] + e * K[3]));
    let l = x.log2();
    let mut d = (l - knee).max(0.0);
    if a > 0.0 {
        d = d.min(0.5 / a);
    }
    let s = l - a * d * d;
    (s.exp2() - (toe + g * (s + 6.0)).exp2()).clamp(0.0, 1.0)
}

/// Lightroom's Whites (−100..100) on the base output `v`, x = |amount| / 100:
/// - Whites < 0 lowers the highlights towards a gain G = −0.79x (log2) with a power-law onset:
///   log2(y / v) = G·(1 + 2^(−(l − c)/σ))^(−q·σ), l = log2 v, c and q linear in x;
/// - Whites > 0 raises them by a power of the value: y = v·(1 + c(x)·v^n(x)), c cubic (0 at 0),
///   n quadratic in x.
///
/// 12 coefficients; residual 0.005 log2 RMS within ±50, 0.014 within ±100 (0.036 at +100, where
/// most of the ramp clips). ΔE −0.015 / +0.018 at +50 / −50.
pub fn lr_whites(amount: f32, v: f32) -> f32 {
    const NEG: [f32; 6] = [-0.785028, -1.32562, 0.309281, 0.506906, 0.481801, 0.122073];
    const POS: [f32; 6] = [4.76257, -18.3376, 30.3492, 1.29896, -1.36983, 0.915893];
    if amount == 0.0 || v.is_nan() || v <= 0.0 {
        return v.max(0.0);
    }
    let x = (amount.abs() / 100.0).min(1.0);
    let y = if amount < 0.0 {
        let [g, c0, c1, q0, q1, s] = NEG;
        let onset = (1.0 + (-(v.log2() - (c0 + c1 * x)) / s).exp2()).powf(-(q0 + q1 * x) * s);
        v * (g * x * onset).exp2()
    } else {
        let [c1, c2, c3, n0, n1, n2] = POS;
        v * (1.0 + x * (c1 + x * (c2 + x * c3)) * v.powf(n0 + x * (n1 + x * n2)))
    };
    y.min(1.0)
}

/// Lightroom's Blacks (−100..100) on value `v` (after Whites), x = |amount| / 100, in the power
/// domain t = v^γ (γ quadratic in x):
/// - Blacks < 0 moves the black point to b (a cubic in x, 0 at 0): t' = (t − b^γ) / (1 − b^γ),
///   clipped at 0;
/// - Blacks > 0 lifts the shadows, fading towards white: t' = t + (1 − t)³·(d₀ + d₁t), d₀ and d₁
///   cubics in x (0 at 0); below the darkest value measured (2^−9.7) its gain holds, so black
///   stays black.
///
/// 14 coefficients; residual 0.009 log2 RMS within ±50, 0.012 within ±100 (most of it just above
/// the crushed blacks; 0.025 mean ΔL* on the ramp). ΔE +0.023 / +0.004 at +50 / −50.
pub fn lr_blacks(amount: f32, v: f32) -> f32 {
    const NEG: [f32; 6] = [1.0192, -0.334792, 0.0982776, 0.0216434, -0.0207681, 0.0293091];
    const POS: [f32; 8] = [1.00124, -0.660126, 0.210879, 0.0144894, 0.0099107, -0.19123, 1.25087, -0.704197];
    if amount == 0.0 || v.is_nan() || v <= 0.0 {
        return v.max(0.0);
    }
    let x = (amount.abs() / 100.0).min(1.0);
    let v = v.min(1.0);
    let y = if amount < 0.0 {
        let [i0, i1, i2, b1, b2, b3] = NEG;
        let gamma = i0 + x * (i1 + x * i2);
        let tb = (x * (b1 + x * (b2 + x * b3))).powf(gamma);
        ((v.powf(gamma) - tb) / (1.0 - tb)).max(0.0).powf(gamma.recip())
    } else {
        let [i0, i1, i2, a1, a3, b1, b2, b3] = POS;
        let gamma = i0 + x * (i1 + x * i2);
        let lift = |v: f32| {
            let t = v.powf(gamma);
            let fade = (1.0 - t) * (1.0 - t) * (1.0 - t);
            (t + fade * (x * (a1 + x * x * a3) + x * (b1 + x * (b2 + x * b3)) * t)).max(0.0).powf(gamma.recip())
        };
        const LOW: f32 = 0.00121;
        if v < LOW { v * lift(LOW) / LOW } else { lift(v) }
    };
    y.min(1.0)
}

/// Lightroom's Contrast (−100..100) on display value `v` after the profile curve: a Möbius S-curve
/// about a pivot p, in the power domain t = v^γ with γ = −1 / log2 p (so the pivot sits at t = ½).
/// Each half maps by u ↦ u·k / (1 − u + u·k) (u the position within the half; k on the lower
/// half, 1/k on the upper one), so 0, the pivot and white stay put.
/// - Contrast < 0: k = 2^x (x = |amount| / 100): exact on the measurements;
/// - Contrast > 0: k = 2^−(k₁x + k₂x²) and γ scaled by (1 − g·x).
///
/// It adapts to the image: log2 p = −0.683 + 0.345·key, `key` the image's statistic ([`lr_key`],
/// the reference chart's −2.50 without one; clamped to the measured −8…−0.47).
///
/// 5 coefficients; residual 0.012 log2 RMS within ±50 (0.003 below 0, 0.024 at +50), 0.040 within
/// ±100 (mostly the deepest shadows at +75 / +100), 0.012 over charts on 8 backgrounds (keys −8 to
/// −0.47) at ±50. ΔE +0.010 / +0.005 at +50 / −50, −0.010 / +0.053 at +35 / +70, 0.000 / +0.014
/// at −35 / −70.
pub fn lr_contrast(amount: f32, key: Option<f32>, v: f32) -> f32 {
    const GX: f32 = 0.662675;
    const K: [f32; 2] = [1.09321, -0.190232];
    const PIVOT: [f32; 2] = [-0.682744, 0.345154];
    const KEY_REF: f32 = -2.50154;
    if amount == 0.0 || v.is_nan() || v <= 0.0 {
        return v.max(0.0);
    }
    let x = (amount.abs() / 100.0).min(1.0);
    let key = key.filter(|k| k.is_finite()).unwrap_or(KEY_REF).clamp(LR_KEY_RANGE.0, LR_KEY_RANGE.1);
    let lp = PIVOT[0] + PIVOT[1] * key;
    let mut gamma = -lp.recip();
    let k = if amount < 0.0 {
        x.exp2()
    } else {
        gamma *= 1.0 - GX * x;
        (-(K[0] * x + K[1] * x * x)).exp2()
    };
    let tp = (lp * gamma).exp2();
    let t = v.min(1.0).powf(gamma);
    let mobius = |u: f32, k: f32| u * k / (1.0 - u + u * k);
    let out = if t < tp { tp * mobius(t / tp, k) } else { tp + (1.0 - tp) * mobius(((t - tp) / (1.0 - tp)).clamp(0.0, 1.0), k.recip()) };
    out.max(0.0).powf(gamma.recip()).min(1.0)
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

// Highlights and Shadows (fitted on the photo at ±60): log2 changes of display-linear luminance,
// a luminance gain after the tone map, scaled linearly with the amount. They act locally: their
// curve is read at a blend of the pixel's and its neighbourhood's log luminance,
// m = l + α·(ctx − l), ctx being the tone-mapped Gaussian blur of log luminance at
// [`LR_CONTEXT_SIGMA`] of the long edge (what explains Lightroom's change best).

/// Lightroom's Highlights at ±[`LR_REF_LOCAL`] (`positive`: the sign) at blended log2 luminance
/// `m`: a low plateau that steps up around the highlights and tapers above them,
/// h(m) = a₀ + (a₁ + a₂(m − c))·σ((m − c)/s), negated (scaled by 1.04) for negative amounts.
/// 6 coefficients; the photo's per-pixel R² 0.90 / 0.89 (+60 / −60), as the measured curve itself
/// (0.91 / 0.89). ΔE +0.022 / −0.003 at +60 / −60.
pub fn lr_highlights(positive: bool, m: f32) -> f32 {
    const H: [f32; 6] = [0.0662053, 0.462386, -0.0884733, -3.46858, 0.325487, 1.03695];
    let h = H[0] + (H[1] + H[2] * (m - H[3])) * sigmoid((m - H[3]) / H[4]);
    if positive { h } else { -H[5] * h }
}

/// Lightroom's Shadows at ±[`LR_REF_LOCAL`] at blended log2 luminance `m`: a ramp of slope k between
/// c₀ and c₁ (rounded over w) plus a small step at c₂,
/// s(m) = τ·σ((c₂ − m)/s₂) + k·w·(softplus((c₁ − m)/w) − softplus((c₀ − m)/w)).
/// 7 coefficients per sign; the photo's per-pixel R² 0.96 / 0.93 (+60 / −60), as the measured
/// curve itself (0.96 / 0.93). ΔE −0.011 / +0.054 at +60 / −60.
pub fn lr_shadows(positive: bool, m: f32) -> f32 {
    const POS: [f32; 7] = [0.140006, -4.31535, 0.142819, 0.562307, -4.79569, -8.52322, 0.715742];
    const NEG: [f32; 7] = [-0.812942, -5.7573, 0.719523, -0.286009, -4.27656, -13.3291, 0.1];
    let [tau, c2, s2, k, c1, c0, w] = if positive { POS } else { NEG };
    tau * sigmoid((c2 - m) / s2) + k * w * (softplus((c1 - m) / w) - softplus((c0 - m) / w))
}

/// A local slider's curve on the [`LR_KNOTS`] evaluation grid (the CPU and the GPU interpolate the
/// same values) and its pixel / neighbourhood blend `alpha`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LrLocal {
    pub alpha: f32,
    pub tab: [f32; LR_KNOTS],
}

impl LrLocal {
    fn sample(alpha: f32, curve: impl Fn(f32) -> f32) -> LrLocal {
        LrLocal { alpha, tab: std::array::from_fn(|i| curve(LR_KNOT0 + LR_KNOT_STEP * i as f32)) }
    }
}

/// A curve on the [`LR_KNOTS`] grid at log2 luminance `l` (linear between knots, the end values
/// beyond them).
#[inline]
pub fn lr_knot(tab: &[f32; LR_KNOTS], l: f32) -> f32 {
    let f = ((l - LR_KNOT0) / LR_KNOT_STEP).clamp(0.0, (LR_KNOTS - 1) as f32);
    let i = (f as usize).min(LR_KNOTS - 2);
    let t = f - i as f32;
    tab[i] + (tab[i + 1] - tab[i]) * t
}

/// Lightroom's Highlights and Shadows for one render (curves and amounts relative to
/// [`LR_REF_LOCAL`]), `None` from [`LrHs::new`] when both are 0.
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
        let (h, s) = (highlights as f32, shadows as f32);
        let hl = LrLocal::sample(0.45, |m| lr_highlights(h >= 0.0, m));
        let sh = LrLocal::sample(if s >= 0.0 { 0.3 } else { 0.5 }, |m| lr_shadows(s >= 0.0, m));
        Some(LrHs { hl: (hl, h.abs() / LR_REF_LOCAL), sh: (sh, s.abs() / LR_REF_LOCAL) })
    }

    /// log2 gain for a pixel at display log2 luminance `l` (after the tone map) in a
    /// neighbourhood at `ctx`: each curve read at `l + α·(ctx − l)`; the result stays at or below
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

/// Most knots a [`CameraTone`] holds (Apple ProRAW's `ProfileToneCurve` is sampled at this many).
pub const CAMERA_TONE_KNOTS: usize = 128;

/// A camera's tone curve: a file-local look fitted independently of the scene-linear colour
/// transform, or a DNG `ProfileToneCurve`. Knots are scene/display-linear luminance pairs. Keeping
/// this in the finish stage preserves RAW exposure and highlight headroom; it is never baked into
/// the decoded sensor pixels. `chroma` scales colourfulness by display luminance after a
/// luminance curve (a camera's per-channel curve saturates shadows and bleaches highlights toward
/// white, which a luminance curve can't).
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
    chroma: [f32; CHROMA_N],
}

/// [`CameraTone`]'s serialised form (smart previews carry it); curves saved before
/// `baseline_ev` / `key` / `chroma` existed read as 0 / unknown / identity.
#[derive(serde::Serialize, serde::Deserialize)]
struct CameraToneWire {
    knots: Vec<[f32; 2]>,
    #[serde(default)]
    rgb: bool,
    #[serde(default)]
    baseline_ev: f32,
    #[serde(default)]
    key: Option<f32>,
    #[serde(default)]
    chroma: Option<[f32; CHROMA_N]>,
}

impl TryFrom<CameraToneWire> for CameraTone {
    type Error = &'static str;
    fn try_from(w: CameraToneWire) -> Result<Self, Self::Error> {
        let tone = Self::from_knots(&w.knots).ok_or("invalid camera tone curve")?;
        let tone = if w.rgb { tone.per_channel() } else { tone };
        let tone = match w.chroma {
            Some(c) => tone.with_chroma(c).ok_or("invalid camera chroma curve")?,
            None => tone,
        };
        Ok(tone.with_baseline_exposure(w.baseline_ev).with_key(w.key))
    }
}

impl From<CameraTone> for CameraToneWire {
    fn from(t: CameraTone) -> Self {
        let chroma = (t.chroma != NO_CHROMA).then_some(t.chroma);
        CameraToneWire { knots: t.knots().to_vec(), rgb: t.rgb, baseline_ev: t.baseline_ev, key: t.key, chroma }
    }
}

impl CameraTone {
    /// 32 knots (a fitted camera look), see [`CameraTone::from_knots`].
    pub fn new(knots: [[f32; 2]; 32]) -> Option<Self> {
        Self::from_knots(&knots)
    }

    /// 2 to [`CAMERA_TONE_KNOTS`] knots, increasing in input, non-decreasing and below 1 in output.
    pub fn from_knots(knots: &[[f32; 2]]) -> Option<Self> {
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
        Some(Self { knots: all, len: knots.len(), rgb: false, baseline_ev: 0.0, key: None, chroma: NO_CHROMA })
    }

    fn knots(&self) -> &[[f32; 2]] {
        self.knots.get(..self.len).unwrap_or(&[])
    }

    /// The same curve applied to RGB (hue-preserving, linear ProPhoto) instead of luminance, inside
    /// Lightroom's tone chain; used for Apple ProRAW's `ProfileToneCurve`.
    pub fn per_channel(self) -> Self {
        Self { rgb: true, ..self }
    }

    /// Whether the curve is applied per channel (Apple ProRAW's `ProfileToneCurve`).
    pub fn is_per_channel(&self) -> bool {
        self.rgb
    }

    /// The same curve applied to luminance, as a camera's curve outside Lightroom's measured camera
    /// renders ([`crate::finish::lr_tone`]).
    pub fn on_luminance(self) -> Self {
        Self { rgb: false, ..self }
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

    /// The curve with chroma scales at display luminance 0, 1/7 … 1 (each finite, 0..=4).
    pub fn with_chroma(self, chroma: [f32; CHROMA_N]) -> Option<Self> {
        chroma.iter().all(|k| k.is_finite() && (0.0..=4.0).contains(k)).then_some(Self { chroma, ..self })
    }

    pub fn chroma(&self) -> &[f32; CHROMA_N] {
        &self.chroma
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
    /// The camera's chroma curve ([`ToneMap::chroma_scale`]).
    chroma: [f32; CHROMA_N],
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
    if ev < LUT_MIN_EV { v * (y / (GREY * 2f32.powf(LUT_MIN_EV))) } else { v }
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
    /// The camera's tone curve with Contrast / Whites / Blacks, at Exposure 0 (see
    /// [`ToneMap::camera_split`]).
    pub fn camera(curve: &CameraTone, contrast: f64, whites: f64, blacks: f64) -> ToneMap {
        Self::camera_split(curve, 0.0, contrast, whites, blacks, false)
    }

    /// The camera's tone curve with Contrast / Whites / Blacks. On Apple ProRAW's profile tone
    /// curve ([`CameraTone::per_channel`]): Lightroom's chain around it (module docs; `exposure` is
    /// the Exposure slider, EV, already applied to the pixels). Else the slider adjustments of a
    /// rendered image ([`ToneMap::display`]) after the curve.
    ///
    /// `hs`: Highlights / Shadows run in this render. On the profile curve Lightroom runs them
    /// after the base operator, before Whites / Blacks and Contrast (with Contrast ±35 / ±70 and
    /// Highlights −36 or Shadows +39, Contrast applied to their result predicts Lightroom's
    /// combined render within 0.02 log2, the other order misses by up to 0.23). So Contrast
    /// becomes a separate last stage ([`ToneMap::apply_contrast_rgb`]) and, with Whites / Blacks,
    /// Highlights / Shadows run inside the map ([`ToneMap::apply_rgb_hs`]).
    pub fn camera_split(curve: &CameraTone, exposure: f64, contrast: f64, whites: f64, blacks: f64, hs: bool) -> ToneMap {
        if !curve.rgb {
            let adjustment = Self::display(contrast, whites, blacks);
            let neutral = contrast == 0.0 && whites == 0.0 && blacks == 0.0;
            let lut = (0..LUT_N)
                .map(|i| {
                    let ev = LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32;
                    let y = curve.apply(GREY * 2f32.powf(ev));
                    if neutral { y } else { adjustment.apply(y) }
                })
                .collect();
            return ToneMap { lut, rgb: None, stages: None, contrast: None, chroma: curve.chroma };
        }
        let ev = curve.baseline_ev + exposure as f32;
        let (c, w, b) = (contrast as f32, whites as f32, blacks as f32);
        let split = hs && c != 0.0;
        let base = |x: f32| lr_base(x, ev);
        let bk = |v: f32| lr_blacks(b, v);
        let wh = |v: f32| lr_whites(w, v);
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
        ToneMap { lut, rgb: Some(prophoto_matrices()), stages, contrast: split.then(|| tone_table(con)), chroma: curve.chroma }
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
        ToneMap { lut, rgb: None, stages: None, contrast: None, chroma: NO_CHROMA }
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
        ToneMap { lut, rgb: None, stages: None, contrast: None, chroma: NO_CHROMA }
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

    /// The chroma curve (`CHROMA_N` entries, see [`ToneMap::chroma_scale`]).
    pub fn chroma_lut(&self) -> &[f32] {
        &self.chroma
    }

    /// Chroma scale at display luminance `o` (exactly 1 everywhere unless a camera look sets it).
    #[inline]
    pub fn chroma_scale(&self, o: f32) -> f32 {
        if !o.is_finite() {
            return 1.0;
        }
        let f = o.clamp(0.0, 1.0) * (CHROMA_N - 1) as f32;
        let i = (f as usize).min(CHROMA_N - 2);
        let t = f - i as f32;
        let (a, b) = (self.chroma.get(i).copied().unwrap_or(1.0), self.chroma.get(i + 1).copied().unwrap_or(1.0));
        if a == b { a } else { a + (b - a) * t }
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

    fn knots() -> [[f32; 2]; 32] {
        std::array::from_fn(|i| {
            let x = 0.004 * 1.18f32.powi(i as i32);
            [x, 1.0 - (-2.0 * x).exp()]
        })
    }

    #[test]
    fn chroma_curve_is_identity_unless_set_and_survives_serde() {
        let plain = CameraTone::new(knots()).unwrap();
        let map = ToneMap::camera(&plain, 0.0, 0.0, 0.0);
        assert!([0.0, 0.3, 0.77, 1.0, 2.0].iter().all(|o| map.chroma_scale(*o) == 1.0));
        assert!([0.0, 0.5, 1.0].iter().all(|o| ToneMap::new(0.0, 0.0, 0.0).chroma_scale(*o) == 1.0));
        // smart previews written before the chroma curve existed still load (identity)
        let old = serde_json::json!({ "knots": knots() });
        assert_eq!(serde_json::from_value::<CameraTone>(old).unwrap(), plain);
        let tone = plain.with_chroma([1.4, 1.3, 1.1, 1.0, 0.7, 0.4, 0.25, 0.2]).unwrap();
        let back: CameraTone = serde_json::from_value(serde_json::to_value(tone).unwrap()).unwrap();
        assert_eq!(back, tone);
        let map = ToneMap::camera(&tone, 0.0, 0.0, 0.0);
        assert!((map.chroma_scale(0.0) - 1.4).abs() < 1e-6 && (map.chroma_scale(1.0) - 0.2).abs() < 1e-6);
        assert!((map.chroma_scale(0.5 / 7.0) - 1.35).abs() < 1e-5, "interpolates between nodes");
        assert_eq!(map.chroma_scale(f32::NAN), 1.0);
        // hostile values are rejected, also when deserialized
        assert!(plain.with_chroma([f32::NAN; CHROMA_N]).is_none());
        assert!(plain.with_chroma([-1.0; CHROMA_N]).is_none());
        let bad = serde_json::json!({ "knots": knots(), "chroma": [9.0, 1, 1, 1, 1, 1, 1, 1] });
        assert!(serde_json::from_value::<CameraTone>(bad).is_err());
    }

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
        assert!(CameraTone::from_knots(&[[0.5, 0.5]]).is_none(), "one knot");
        assert!(CameraTone::from_knots(&[[0.1, 0.1]; CAMERA_TONE_KNOTS + 1]).is_none(), "too many knots");
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
        let curve = CameraTone::from_knots(&knots).unwrap().per_channel().with_baseline_exposure(-0.3);
        for (e, c, w, b) in
            [(0.0, 0.0, 0.0, 0.0), (0.0, 43.0, -56.0, 50.0), (-0.05, -35.0, -28.0, 22.0), (2.5, 100.0, 100.0, -100.0), (-3.0, -100.0, -100.0, 100.0)]
        {
            let t = ToneMap::camera_split(&curve, e, c, w, b, false);
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
        let base = ToneMap::camera(&curve, 0.0, 0.0, 0.0).apply_rgb(colour);
        let lower = ToneMap::camera(&curve, 0.0, -60.0, 0.0).apply_rgb(colour);
        let higher = ToneMap::camera(&curve, 0.0, 60.0, 0.0).apply_rgb(colour);
        let moved = |c: [f32; 3]| (ratio(c) / ratio(base) - 1.0).abs();
        assert!(moved(higher) > 0.02 && moved(lower) < moved(higher) / 2.0, "{base:?} → lower {lower:?}, higher {higher:?}");
        // the same scene value comes out darker at +1 EV: the base operator's shoulder grows with
        // the exposure (Exposure is not a plain gain)
        let (e0, e1) = (ToneMap::camera(&curve, 0.0, 0.0, 0.0), ToneMap::camera_split(&curve, 1.0, 0.0, 0.0, 0.0, false));
        assert!(e1.apply(0.8) < e0.apply(0.8) - 0.005, "{} vs {}", e1.apply(0.8), e0.apply(0.8));
        // Contrast adapts to the image: its pivot sits lower on a dark image, so +50 lowers a
        // shadow value less there than on a bright one; the image's key comes from its pixels
        let (dark, bright) = (vec![[0.01f32; 3]; 64], vec![[0.5f32; 3]; 64]);
        let (kd, kb) = (lr_key(&dark, -0.3), lr_key(&bright, -0.3));
        assert!(kd.zip(kb).is_some_and(|(d, b)| d < b - 2.0), "{kd:?} {kb:?}");
        let shadow = |key| ToneMap::camera(&curve.with_key(key), 50.0, 0.0, 0.0).apply(0.02);
        assert!(shadow(kd) > shadow(kb) * 1.05, "{} vs {}", shadow(kd), shadow(kb));
        assert_eq!(lr_key(&[], -0.3), None);
    }

    /// The analytic Lightroom operators keep their fitted shape (values from the fit's own
    /// evaluation), leave values alone at amount 0, change continuously from it, and stay
    /// monotone and within 0..=1 for every amount, exposure and key.
    #[test]
    fn lightroom_operators_keep_their_fit() {
        let close = |got: f32, want: f32, what: &str| assert!((got - want).abs() <= 2e-3 * want.abs().max(0.05), "{what}: {got} vs {want}");
        for (x, ev, want) in
            [(0.05, -0.302, 0.046958), (0.5, -0.302, 0.481253), (0.9, 1.0, 0.662047), (0.02, -1.302, 0.0182911), (2.0, 1.698, 0.916261)]
        {
            close(lr_base(x, ev), want, &format!("base {x} at {ev} EV"));
        }
        assert_eq!(lr_base(0.001, -0.302), 0.0, "the toe crushes the deepest values");
        for (a, v, want) in [(-56.0, 0.3, 0.240347), (50.0, 0.2, 0.281909), (-28.0, 0.6, 0.515553)] {
            close(lr_whites(a, v), want, &format!("whites {a} at {v}"));
        }
        for (a, v, want) in [(50.0, 0.01, 0.0148142), (-50.0, 0.05, 0.0378851), (22.0, 0.2, 0.202382)] {
            close(lr_blacks(a, v), want, &format!("blacks {a} at {v}"));
        }
        for (a, v, key, want) in [(43.0, 0.1, -4.0, 0.077615), (-35.0, 0.05, -4.0, 0.0642664), (70.0, 0.6, -2.0, 0.669651)] {
            close(lr_contrast(a, Some(key), v), want, &format!("contrast {a} at {v}, key {key}"));
        }
        close(lr_highlights(true, -3.0), 0.406483, "highlights +");
        close(lr_highlights(false, -6.0), -0.06895, "highlights −");
        close(lr_shadows(true, -6.0), 0.874136, "shadows +");
        close(lr_shadows(false, -5.0), -0.417276, "shadows −");
        let ramp = |i: i32| 1e-5 * 1.013f32.powi(i);
        for v in (0..900).map(ramp).filter(|v| *v <= 1.0) {
            assert_eq!((lr_whites(0.0, v), lr_blacks(0.0, v), lr_contrast(0.0, None, v)), (v, v, v));
            for small in [-0.05f32, 0.05] {
                // (Blacks −0.05 moves the black point to ~1e-5)
                let near = |f: f32, what: &str| assert!((f - v).abs() <= 0.01 * v + 2e-5, "{what} {small} at {v}: {f}");
                near(lr_whites(small, v), "whites");
                near(lr_blacks(small, v), "blacks");
                near(lr_contrast(small, None, v), "contrast");
            }
        }
        let monotone = |f: &dyn Fn(f32) -> f32, what: &str| {
            let mut prev = 0.0f32;
            for v in (0..1200).map(ramp) {
                let o = f(v);
                assert!((0.0..=1.0).contains(&o) && o >= prev - 1e-6, "{what} at {v}: {o} after {prev}");
                prev = o;
            }
        };
        for ev in [-4.0f32, -2.302, -1.0, -0.302, 0.4, 1.0, 1.698, 3.0, f32::NAN] {
            monotone(&|x| lr_base(x, ev), &format!("base at {ev} EV"));
        }
        for a in [-100.0f32, -75.0, -50.0, -10.0, 10.0, 25.0, 60.0, 100.0] {
            monotone(&|v| lr_whites(a, v), &format!("whites {a}"));
            monotone(&|v| lr_blacks(a, v), &format!("blacks {a}"));
            for key in [None, Some(-12.0), Some(-4.0), Some(-0.2), Some(f32::NAN)] {
                monotone(&|v| lr_contrast(a, key, v), &format!("contrast {a}, key {key:?}"));
            }
        }
        assert!([f32::NAN, f32::INFINITY, -1.0, 0.0].iter().all(|x| lr_base(*x, 0.0) == 0.0));
        assert!(lr_whites(30.0, f32::NAN) == 0.0 && lr_blacks(-30.0, f32::NAN) == 0.0 && lr_contrast(30.0, None, f32::NAN) == 0.0);
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
