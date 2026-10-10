//! The global tone map: scene luminance → display-linear luminance.
//!
//! Built in the log domain around middle grey (0.18): contrast scales the log-exposure about grey
//! and whites move the shoulder. The shoulder is a logistic in the exposed luminance, so it rolls
//! off smoothly into white instead of clipping. Blacks is not part of that curve — it is a separate
//! per-channel curve over the result, see [`Blacks`].
//!
//! The base defaults are not the neutral log-slope of 1: [`BASE_SLOPE`] and [`BASE_WHITE_EV`] carry
//! the default response of the reference raw developer, so an unedited raw looks like the photo its
//! users see on opening one. Both are fitted from its observed output (see [`BASE_SLOPE`]); no
//! profile, matrix or preset of theirs is used.
//!
//! Rendered (display-referred) sources such as JPEGs use [`ToneMap::display`] instead: identity at
//! neutral settings (an unedited JPEG renders exactly as the file), with contrast/whites/blacks as
//! S-curve adjustments in a gamma-2.2 perceptual domain and a short shoulder above 0.95.

pub const GREY: f32 = 0.18;

/// Base slope of the default raw curve when contrast is 0, i.e. how much steeper than a neutral
/// log-slope of 1 the tones climb through grey. Fitted — with [`BASE_WHITE_EV`] — to the reference
/// developer's untouched default rendering of raw files.
///
/// The fit works on a scene-luminance axis recovered from exposure ladders on both sides: exposure
/// is a known power-of-two scale on scene luminance, so a pixel's rank within the frame is
/// invariant down a ladder and each rank yields one point of the curve. That anchors the axis
/// without assuming anything about either tool's internals, and the two constants then fall out of
/// a least-squares fit of this very formula — 0.045 stops RMS over the 7 stops measured.
///
/// A steeper-than-neutral base is what users of the reference tool see by default; setting these
/// to `1.0` and `2.9` restores the neutral, grey-preserving curve.
pub const BASE_SLOPE: f32 = 1.56;

/// Base shoulder position: scene EV above grey at which the default curve reaches half display.
pub const BASE_WHITE_EV: f32 = 1.30;

/// The display value Contrast pivots about (measured; the per-value fits agree to 0.05 over
/// twenty-four bodies, and it does not move with the slider).
const CONTRAST_PIVOT: f32 = 0.4934;

/// The Contrast slider's exponent is exponential in the slider: `g = exp(-this · contrast)`.
///
/// The law itself comes from the reference: fitting an odd-symmetric S about a pivot to its own
/// transfer at seven slider values over twenty-four bodies gives an exponent table that this
/// reproduces to within 1 %, landing on exactly `g = 1` (the identity) at neutral.
///
/// The coefficients, though, are *calibrated* rather than taken from that table, because the fit
/// compresses the exponent: applying the fitted value and measuring the result through the same
/// pipeline came back shallower than asked (g 0.692 where the reference reads 0.634 at +100). So
/// these are the fitted table divided by the measured compression — 0.832 on the positive arm and
/// 0.912 on the negative, which differ because an S and an inverse S do not fit alike. Checked by
/// re-measuring, not assumed.
///
/// This replaces a log-slope change about *grey*, which was wrong in two ways at once: ours came
/// out at g 0.760 with an effective pivot of 0.596 where the reference is 0.634 about 0.507 — too
/// weak, and pivoting 0.09 too high, so it moved the shadows more than it should and the
/// highlights less.
const CONTRAST_LOG_G_POS: f32 = 0.005453;
const CONTRAST_LOG_G_NEG: f32 = 0.004975;

/// Contrast's exponent: 1.0 at neutral, below 1 (an S) for positive, above 1 (an inverse S) for
/// negative.
fn contrast_g(contrast: f64) -> f32 {
    if !contrast.is_finite() {
        return 1.0;
    }
    let c = contrast.clamp(-100.0, 100.0) as f32;
    let k = if c >= 0.0 { CONTRAST_LOG_G_POS } else { CONTRAST_LOG_G_NEG };
    (-k * c).exp()
}

/// Apply Contrast to a display-*linear* value: an odd-symmetric power about [`CONTRAST_PIVOT`] in
/// the encoded domain, mirrored to both sides of it.
///
/// The pivot is in encoded units because that is where the reference's Contrast acts; taking the
/// value there and coming back is the same round trip [`whites_display`] makes, and for the same
/// reason — a display-domain operation cannot be applied to a scene-domain curve directly.
fn contrast_display(o: f32, g: f32) -> f32 {
    if g == 1.0 || !o.is_finite() {
        return o;
    }
    let x = lightcraft_color::transfer::linear_to_srgb(o.clamp(0.0, 1.0));
    let l = CONTRAST_PIVOT.max(1.0 - CONTRAST_PIVOT);
    let t = ((x - CONTRAST_PIVOT) / l).clamp(-1.0, 1.0);
    let y = CONTRAST_PIVOT + l * t.abs().powf(g) * t.signum();
    lightcraft_color::transfer::srgb_to_linear(y.clamp(0.0, 1.0))
}

/// EV the white point moves for each Whites notch, at −100, −75 … +100 (see [`whites_ev`]).
///
/// The reference's Whites does not only move the shoulder, which is what this curve used to do
/// through a single pair of coefficients (`WHITES_UP` / `WHITES_DOWN`): it moves the shoulder
/// *and* the log-slope. Fitting both to its renders beats the shoulder alone by 2–3x at every
/// notch — 0.062 vs 0.019 in encoded sRGB at +100 — and the two move together, so the slope is
/// `WHITES_SLOPE` times this. The same control that validates [`Blacks`] applies here: fitting
/// this shoulder to LightCraft's own Whites recovers the old coefficients to 0.02 EV with a
/// residual of 0.004, so the family is identified, and the two parameters are not — the tied fit
/// is as good as the free one to four decimals.
///
/// Asymmetric and convex, like the reference's other sliders: per unit it is 0.0042 EV at −100
/// against 0.0175 at +100. One parameter serves the whole range, but not one *coefficient*.
const WHITES_KNOTS: [f32; 9] = [-0.420, -0.357, -0.267, -0.147, 0.0, 0.115, 0.388, 0.904, 1.753];

/// The log-slope move that comes with a white-point move of [`whites_ev`] (measured, tied).
const WHITES_SLOPE: f32 = 0.425;

/// Slope of [`whites_scale`] against the frame's highlight percentile (measured, normalised).
const WHITES_SCALE_SLOPE: f32 = 3.9025;
/// The highlight percentile at which the per-photo multiplier is exactly 1 (the corpus mean).
const WHITES_SCALE_MID: f32 = 0.860010;
/// Rails on the multiplier, so an extreme frame cannot run away with the curve. The measured
/// spread over ten bodies is 0.23..1.69, so these only ever catch input the corpus never showed.
const WHITES_SCALE_MIN: f32 = 0.20;
const WHITES_SCALE_MAX: f32 = 2.00;

/// The per-photo scale on positive Whites travel, from the frame's own highlight level.
///
/// Positive Whites is not one curve. Measured across ten camera bodies — binning the reference's
/// own render at Whites = V by its render at 0, per channel, so the slider is isolated from
/// everything the two engines disagree about — the white-point move fitted per *photo* spans
/// 0.66..4.15 EV at +100 where [`WHITES_KNOTS`] holds one number, and the fixed value is off by
/// more than 0.35 EV on nine of the ten. That is the residual this scale removes; no better
/// constant can, because the spread is 6x and no value sits near the middle of it.
///
/// The reference stretches toward a white point the photo's own highlights set, so a frame that
/// already has bright highlights needs a *smaller* move. One statistic carries it: the 99th
/// percentile of the sRGB-*encoded* display luma of a small proxy render of the frame at Whites =
/// 0 (see `local::frame_highlight`), cross-validated by holding out a whole camera body. Against
/// the per-photo amount the reference asks for, that statistic correlates −0.41 at +25, −0.65 at
/// +50, −0.79 at +75 and **−0.84 at +100** — weaker than the −0.98 the relation was first fitted
/// against, because that used a percentile of the full-resolution render while this proxy skips
/// the camera colour transform. The strongest agreement is where the term matters most.
///
/// The percentile must be taken in the encoded domain: a percentile does not commute with the
/// transfer curve, and taking it in display-linear shifts every reading by ~0.2 and drops the
/// +100 correlation from −0.84 to −0.66.
///
/// Negative Whites is the opposite — its per-body spread is 0.04..0.08 EV, one curve — so
/// [`whites_ev_scaled`] applies the scale to positive travel only and the curve below zero is
/// untouched. The relation is normalised so the average photo keeps [`WHITES_KNOTS`] exactly
/// (mean multiplier 1.0 over the corpus), rather than also taking the ~36 % stronger mean the
/// same fit would give, which is as likely to be this engine's own profile error on unusual
/// bodies as the reference's true level.
///
/// This is the one slider where a per-photo statistic survived measurement: across the rest of
/// the Basic panel no usable one was found (Highlights and Shadows fit well but their parameters
/// are not identifiable; Contrast, Exposure, Vibrance and Saturation are fixed or resist).
pub fn whites_scale(highlight_p99: f32) -> f32 {
    if !highlight_p99.is_finite() {
        return 1.0;
    }
    let m = 1.0 - WHITES_SCALE_SLOPE * (highlight_p99 - WHITES_SCALE_MID);
    m.clamp(WHITES_SCALE_MIN, WHITES_SCALE_MAX)
}

/// EV the white point moves at `whites`, with the per-photo [`whites_scale`] on positive travel.
fn whites_ev_scaled(whites: f64, scale: f32) -> f32 {
    let dew = whites_ev(whites);
    if dew > 0.0 && scale.is_finite() { dew * scale.clamp(WHITES_SCALE_MIN, WHITES_SCALE_MAX) } else { dew }
}

/// EV the white point moves at `whites` (−100..=100, clamped), interpolating [`WHITES_KNOTS`].
fn whites_ev(whites: f64) -> f32 {
    if !whites.is_finite() {
        return 0.0;
    }
    let x = ((whites.clamp(-100.0, 100.0) as f32) + 100.0) / 25.0;
    let i = (x as usize).min(WHITES_KNOTS.len() - 2);
    let u = x - i as f32;
    WHITES_KNOTS[i] + (WHITES_KNOTS[i + 1] - WHITES_KNOTS[i]) * u
}

/// [`whites_ev`]'s move as a display→display map, for a curve this module did not build.
///
/// The move belongs to the slider, not to the curve under it: measured on files whose tone comes
/// from the camera's own look, the reference's Whites transfer is described by the same family as
/// on the default curve, and at the same residual (0.013–0.033 against 0.019 over the corpus). So
/// rather than re-deriving it for every camera look — or, as this used to do, approximating it
/// with a display-domain lift that left Whites nearly inert on such files — invert the neutral
/// curve to scene EV, apply the move there, and map forward again.
///
/// This is not a derivation of what the reference does under a camera curve; it is the
/// composition that reproduces the transfer that was measured.
fn whites_display(o: f32, dew: f32) -> f32 {
    if dew == 0.0 || !o.is_finite() || o <= 0.0 || o >= 1.0 {
        return o;
    }
    let wl0 = GREY * 2f32.powf(BASE_WHITE_EV);
    // Invert the neutral curve: o = y/(y + wl0) with y = GREY·2^(ev·BASE_SLOPE)·pre0.
    let d = o.min(1.0 - 1e-6);
    let ev = ((wl0 * d / (1.0 - d)) / (GREY * (1.0 + GREY / wl0))).log2() / BASE_SLOPE;
    let slope = BASE_SLOPE + WHITES_SLOPE * dew;
    let wl = GREY * 2f32.powf(BASE_WHITE_EV - dew);
    let y = GREY * 2f32.powf(ev * slope) * (1.0 + GREY / wl);
    (y / (y + wl)).clamp(0.0, 1.0)
}

/// The tone LUT spans `LUT_MIN_EV..LUT_MAX_EV` around grey in `LUT_N` steps.
pub const LUT_MIN_EV: f32 = -14.0;
pub const LUT_MAX_EV: f32 = 10.0;
pub const LUT_N: usize = 4096;
/// Nodes of a camera chroma curve, evenly spaced over display luminance 0..=1.
pub const CHROMA_N: usize = 8;
const NO_CHROMA: [f32; CHROMA_N] = [1.0; CHROMA_N];

/// Colourfulness of the default raw rendering relative to a purely luminance-preserving tone map,
/// by display luminance (0, 1/7 … 1).
///
/// [`ToneMap::new`] curves luminance and rescales the colour with it, which preserves saturation
/// exactly. The reference developer's default rendering does not: its baseline curve acts on the
/// channels, which saturates the shadows and bleaches the highlights toward white. Left out, an
/// otherwise-correct raw render came out ~19 % under the reference in chroma (mean ΔE 6.3 against
/// 3.4), flat rather than punchy.
///
/// Multiplying the colour back about the mapped luminance by these factors restores that behaviour
/// without the hue shifts and channel clipping a per-channel curve brings. Derived by measurement:
/// for every pixel of five Adobe Standard raws, the colourfulness the per-channel curve gives
/// against the colourfulness the luminance curve gives, averaged into these eight bins (the five
/// images agree to within a few percent of each other, so this is a property of the curve, not of a
/// photo). They are why [`ToneMap::new`] is punchy in the shadows and clean in the highlights.
///
/// A camera's own [`CameraTone`] curve carries its own chroma curve, fitted alongside it. A DNG
/// `ProfileToneCurve` has no such fit — it *is* the reference's curve — so it takes these factors
/// too, for the same reason: on its own the luminance curve renders the file flat.
pub const DEFAULT_CHROMA: [f32; CHROMA_N] = [1.431, 1.308, 1.164, 1.040, 0.955, 0.778, 0.593, 0.400];

/// A file-local camera look, fitted independently of the scene-linear colour transform.
/// Knots are scene/display-linear luminance pairs. Keeping this in the finish stage preserves
/// RAW exposure and highlight headroom; it is never baked into the decoded sensor pixels.
/// `chroma` scales colourfulness by display luminance after the curve (a camera's per-channel
/// curve saturates shadows and bleaches highlights toward white, which a luminance curve can't).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct CameraTone {
    knots: [[f32; 2]; 32],
    chroma: [f32; CHROMA_N],
}

impl<'de> serde::Deserialize<'de> for CameraTone {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Wire {
            knots: [[f32; 2]; 32],
            // absent in smart previews written before the chroma curve existed
            #[serde(default)]
            chroma: Option<[f32; CHROMA_N]>,
        }
        let w = Wire::deserialize(d)?;
        let tone = Self::new(w.knots).ok_or_else(|| serde::de::Error::custom("invalid camera tone curve"))?;
        match w.chroma {
            Some(c) => tone.with_chroma(c).ok_or_else(|| serde::de::Error::custom("invalid camera chroma curve")),
            None => Ok(tone),
        }
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
        Some(Self { knots, chroma: NO_CHROMA })
    }

    /// The curve with chroma scales at display luminance 0, 1/7 … 1 (each finite, 0..=4).
    pub fn with_chroma(self, chroma: [f32; CHROMA_N]) -> Option<Self> {
        chroma.iter().all(|k| k.is_finite() && (0.0..=4.0).contains(k)).then_some(Self { chroma, ..self })
    }

    pub fn chroma(&self) -> &[f32; CHROMA_N] {
        &self.chroma
    }

    /// The (scene, display) luminance knots, in increasing order.
    pub fn knots(&self) -> &[[f32; 2]; 32] {
        &self.knots
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

/// Scene EV over grey where [`ToneMap::hdr`] starts lifting highlights above the SDR curve.
const HDR_FROM_EV: f32 = 1.5;
/// The Blacks slider's own curve, in the display domain — which is where the reference applies it.
///
/// It is not a constant added to the shadow end of the tone curve. It is a second curve run over
/// the tone-mapped value, `out = ((x − t)/(1 − t))^p`, where `t` is a black point that only exists
/// on the negative side (a positive Blacks is anchored at 0, since a lift that raised black would
/// fog the shadows) and `p` is the curvature. The reference runs it over the channels, not over
/// luminance — though, as `a` below records, not uniformly so.
///
/// Nothing below is a coefficient of anyone's code: [`BLACKS_KNOTS`] was measured off the
/// reference's own renders — five values either side of zero, on three cameras (a Canon 5D Mark
/// III, a Pixel 4a and a Fujifilm X100S) — by binning each channel of the render at Blacks = V by
/// that channel's own value in the render at Blacks = 0, and fitting this formula. The three
/// cameras agree to within ~2 %, so this is a property of the slider and not of a photo.
///
/// The old model — a constant lift added in display-linear — is wrong in kind, not just in size:
/// at +100 it lifted a level rendering at 0.05 up to 0.20 where the reference gives 0.11, and at
/// −100 it left a level at 0.25 at 0.13 where the reference crushes it to 0.03. Mean colour error
/// against the reference over the slider's range falls from 1.6–11.0 to 0.7–2.1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blacks {
    t: f32,
    p: f32,
    a: f32,
}

/// Measured `(t, p, a)` at −100, −75, −50, −25, 0, +25, +50, +75, +100 (see [`Blacks`]).
///
/// `a` is how per-channel the reference's result is, as a share of the per-channel curve against
/// the same curve applied to luminance with the colour scaled to match — which is what [`ToneMap`]
/// does, so it is the fraction of the two this stage blends. Each was fitted to minimise the colour
/// error against the reference's renders, and reproduces the fractions reported independently in
/// review (per-channel at +25, ~0.4 at +100). `t` is zero above −25: the reference puts no black
/// point on a positive Blacks.
const BLACKS_KNOTS: [(f32, f32, f32); 9] = [
    (0.0516, 1.2308, 0.88),
    (0.0232, 1.1412, 0.98),
    (0.0100, 1.0805, 0.99),
    (0.0038, 1.0349, 0.99),
    (0.0, 1.0, 1.0),
    (0.0, 0.9735, 1.0),
    (0.0, 0.9070, 0.72),
    (0.0, 0.8478, 0.52),
    (0.0, 0.7914, 0.40),
];

impl Blacks {
    /// The curve at neutral settings: exactly the identity.
    pub const IDENTITY: Blacks = Blacks { t: 0.0, p: 1.0, a: 1.0 };

    /// The curve for `blacks` in −100..=100 (Lightroom slider units; clamped, non-finite is 0).
    pub fn new(blacks: f64) -> Blacks {
        if !blacks.is_finite() {
            return Self::IDENTITY;
        }
        let x = ((blacks.clamp(-100.0, 100.0) as f32) + 100.0) / 25.0;
        let i = (x as usize).min(BLACKS_KNOTS.len() - 2);
        let u = x - i as f32;
        let (lo, hi) = (BLACKS_KNOTS[i], BLACKS_KNOTS[i + 1]);
        Blacks { t: lo.0 + (hi.0 - lo.0) * u, p: lo.1 + (hi.1 - lo.1) * u, a: lo.2 + (hi.2 - lo.2) * u }
    }

    /// Whether this curve is the identity, so a caller can skip it.
    #[inline]
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// The black point (`0.0` unless the slider is negative).
    #[inline]
    pub fn black_point(&self) -> f32 {
        self.t
    }

    /// The curvature.
    #[inline]
    pub fn power(&self) -> f32 {
        self.p
    }

    /// The share of the result that is per-channel (1 = entirely).
    #[inline]
    pub fn share(&self) -> f32 {
        self.a
    }

    /// The luminance blend factor, or `None` when the result is purely per-channel.
    #[inline]
    pub fn blend(&self) -> Option<f32> {
        (self.a < 1.0).then_some(self.a)
    }

    /// The curve on one channel, `x` in display-linear 0..1 (anything above 1 is treated as 1).
    /// Mirrors `blacks_apply` in `finish.wgsl` exactly, NaN included.
    #[inline]
    pub fn apply(&self, x: f32) -> f32 {
        if x <= 0.0 || x.is_nan() {
            return 0.0;
        }
        let u = ((x - self.t) / (1.0 - self.t)).clamp(0.0, 1.0);
        if self.p == 1.0 { u } else { u.powf(self.p) }
    }
}

#[derive(Clone, Debug)]
pub struct ToneMap {
    lut: Vec<f32>,
    chroma: [f32; CHROMA_N],
}

impl ToneMap {
    pub fn camera(curve: &CameraTone, contrast: f64, whites: f64) -> ToneMap {
        Self::camera_scaled(curve, contrast, whites, 1.0)
    }

    /// [`ToneMap::camera`] with the per-photo positive-Whites scale ([`whites_scale`]) applied.
    pub fn camera_scaled(curve: &CameraTone, contrast: f64, whites: f64, whites_scale: f32) -> ToneMap {
        // Whites is the measured scene-domain move of the white point and the log-slope (see
        // [`WHITES_KNOTS`]), applied here over the camera curve's output as the display→display map
        // that move is equivalent to — see [`whites_display`]. This used to route Whites through
        // [`ToneMap::display`] alongside Contrast, a display-domain lift whose reach is a fraction
        // of a stop: on a file carrying a camera look that left Whites almost inert (measured dew
        // +0.26 where the reference moves +2.60 at +100), which is most of the error there.
        let g = contrast_g(contrast);
        let dew = whites_ev_scaled(whites, whites_scale);
        let neutral = g == 1.0 && dew == 0.0;
        let lut = (0..LUT_N)
            .map(|i| {
                let ev = LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32;
                let y = curve.apply(GREY * 2f32.powf(ev));
                let y = if neutral { y } else { contrast_display(y, g) };
                whites_display(y, dew)
            })
            .collect();
        ToneMap { lut, chroma: curve.chroma }
    }
    /// `contrast`, `whites` in −100..100 (Lightroom slider units). Blacks is [`Blacks`], applied
    /// separately — it is a per-channel curve over this map's output, not part of it.
    pub fn new(contrast: f64, whites: f64) -> ToneMap {
        ToneMap::hdr(contrast, whites, 1.0)
    }

    /// [`ToneMap::new`] for an HDR render reaching `peak` (linear, SDR white = 1; 1 = SDR exactly).
    /// Below about half of SDR white the curve is the SDR one; above, the highlights gain up to
    /// `peak − 1` more along a smoothstep in scene EV, from 1.5 EV over grey to one stop past the
    /// white point moved up by the headroom, so the result stays monotone, never exceeds `peak`,
    /// and reaches it there (within the table's range).
    pub fn hdr(contrast: f64, whites: f64, peak: f32) -> ToneMap {
        Self::hdr_scaled(contrast, whites, peak, 1.0)
    }

    /// [`ToneMap::hdr`] with the per-photo positive-Whites scale ([`whites_scale`]) applied.
    pub fn hdr_scaled(contrast: f64, whites: f64, peak: f32, whites_scale: f32) -> ToneMap {
        let peak = if peak.is_finite() { peak.max(1.0) } else { 1.0 };
        // Whites moves the white point and the log-slope together (see [`WHITES_KNOTS`]); Contrast
        // is not a slope at all but a display-domain S about its own pivot, applied further down.
        let g = contrast_g(contrast);
        let dew = whites_ev_scaled(whites, whites_scale);
        let slope = BASE_SLOPE + WHITES_SLOPE * dew;
        // Shoulder: scene luminance (after contrast) that maps to half display.
        let wl = GREY * 2f32.powf(BASE_WHITE_EV - dew);
        let pre = 1.0 + GREY / wl; // keep grey near grey
        let lut = (0..LUT_N)
            .map(|i| {
                let ev = LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32;
                let y = GREY * 2f32.powf(ev * slope) * pre;
                // logistic shoulder: y/(y + wl) is 0 at black and rises to 1 without ever
                // reaching it, so highlights compress smoothly instead of clipping
                let mut o = contrast_display((y / (y + wl)).clamp(0.0, 1.0), g);
                if peak > 1.0 {
                    // HDR: lift highlights above the SDR curve, up to `peak` (see [`ToneMap::hdr`])
                    let top = (BASE_WHITE_EV - dew + peak.log2() + 1.0).max(HDR_FROM_EV + 0.5);
                    o += (peak - 1.0) * smooth(HDR_FROM_EV, top, ev * slope);
                }
                o.clamp(0.0, peak)
            })
            .collect();
        ToneMap { lut, chroma: DEFAULT_CHROMA }
    }

    /// Tone map for display-referred sources: identity at neutral settings.
    pub fn display(contrast: f64, whites: f64, blacks: f64) -> ToneMap {
        ToneMap::display_hdr(contrast, whites, blacks, 1.0)
    }

    /// [`ToneMap::display`] for an HDR render reaching `peak`: values above SDR white (an HDR
    /// source, e.g. a decoded gain-map image) pass through and roll off softly towards `peak`
    /// instead of clipping at 1; SDR sources (≤ 1) render as with [`ToneMap::display`].
    pub fn display_hdr(contrast: f64, whites: f64, blacks: f64, peak: f32) -> ToneMap {
        let peak = if peak.is_finite() { peak.max(1.0) } else { 1.0 };
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
                if peak > 1.0 && o > 0.95 {
                    // exponential shoulder: slope 1 at 0.95, asymptote at `peak`
                    let r = peak - 0.95;
                    o = 0.95 + r * (1.0 - (-(o - 0.95) / r).exp());
                } else if o > 0.95 {
                    let d = (o - 0.95).min(0.1);
                    o = 0.95 + d - d * d / 0.2;
                }
                o.clamp(0.0, peak)
            })
            .collect();
        ToneMap { lut, chroma: NO_CHROMA }
    }

    /// The table (`LUT_N` entries, see [`ToneMap::apply`]).
    pub fn lut(&self) -> &[f32] {
        &self.lut
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
        let map = ToneMap::camera(&plain, 0.0, 0.0);
        assert!([0.0, 0.3, 0.77, 1.0, 2.0].iter().all(|o| map.chroma_scale(*o) == 1.0));
        // smart previews written before the chroma curve existed still load (identity)
        let old = serde_json::json!({ "knots": knots() });
        assert_eq!(serde_json::from_value::<CameraTone>(old).unwrap(), plain);
        let tone = plain.with_chroma([1.4, 1.3, 1.1, 1.0, 0.7, 0.4, 0.25, 0.2]).unwrap();
        let back: CameraTone = serde_json::from_value(serde_json::to_value(tone).unwrap()).unwrap();
        assert_eq!(back, tone);
        let map = ToneMap::camera(&tone, 0.0, 0.0);
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
    fn default_raw_curve_carries_the_fitted_chroma_curve() {
        let map = ToneMap::new(0.0, 0.0);
        // The reference's default rendering is punchy in the shadows and bleaches its highlights
        // toward white, where a purely luminance-preserving curve would sit at 1.0 throughout.
        assert!(map.chroma_scale(0.0) > 1.05, "shadows: {}", map.chroma_scale(0.0));
        assert!(map.chroma_scale(1.0) < 0.5, "highlights: {}", map.chroma_scale(1.0));
        let mut last = map.chroma_scale(0.0);
        for i in 1..=100 {
            let k = map.chroma_scale(i as f32 / 100.0);
            assert!(k <= last + 1e-6, "falls with luminance (at {i})");
            last = k;
        }
        // A display-referred source still renders exactly as the file.
        assert!([0.0, 0.5, 1.0].iter().all(|o| ToneMap::display(0.0, 0.0, 0.0).chroma_scale(*o) == 1.0));
    }

    #[test]
    fn camera_curve_preserves_black_and_extends_headroom() {
        let knots = std::array::from_fn(|i| {
            let x = 0.005 * 1.15f32.powi(i as i32);
            [x, 1.0 - (-3.0 * x).exp()]
        });
        let curve = CameraTone::new(knots).unwrap();
        let base = ToneMap::camera(&curve, 0.0, 0.0);
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
        assert!(ToneMap::camera(&curve, 50.0, 0.0).apply(0.3) > base.apply(0.3));
        let mut invalid = knots;
        invalid[1][0] = invalid[0][0];
        assert!(CameraTone::new(invalid).is_none());
        assert!(serde_json::from_value::<CameraTone>(serde_json::json!({"knots": invalid})).is_err());
    }

    #[test]
    fn monotone_and_bounded() {
        for (c, w) in [(0.0, 0.0), (100.0, 100.0), (-100.0, -100.0), (50.0, -30.0)] {
            let t = ToneMap::new(c, w);
            let mut prev = -1.0;
            for i in 0..2000 {
                let y = 1e-5 * 1.012f32.powi(i);
                let o = t.apply(y);
                assert!((0.0..=1.0).contains(&o));
                assert!(o >= prev - 1e-6, "{c} {w} at {y}: {o} < {prev}");
                prev = o;
            }
        }
        // Blacks is its own curve now, so it gets its own sweep -- over the whole slider, and
        // over both sides of the point where it hands over to the luminance blend.
        for b in (-100..=100).step_by(5) {
            let blacks = Blacks::new(b as f64);
            let mut prev = -1.0;
            for i in 0..1200 {
                let x = 1e-6 * 1.02f32.powi(i);
                let o = blacks.apply(x);
                assert!((0.0..=1.0).contains(&o), "blacks {b} at {x}: {o}");
                assert!(o >= prev - 1e-6, "blacks {b} at {x}: {o} < {prev}");
                prev = o;
            }
        }
    }

    #[test]
    fn hdr_is_sdr_at_peak_one_and_reaches_peak() {
        for (c, w) in [(0.0, 0.0), (100.0, 100.0), (-100.0, -100.0), (50.0, -30.0)] {
            assert_eq!(ToneMap::hdr(c, w, 1.0).lut(), ToneMap::new(c, w).lut());
            // Where the HDR lift has not started, the curve must be the SDR one exactly. The
            // boundary is stated in the curve's own terms rather than as a fixed scene level,
            // because the measured slope moves with Contrast and Whites.
            let slope = BASE_SLOPE + WHITES_SLOPE * whites_ev(w);
            for peak in [2.0f32, 8.0, 32.0] {
                let sdr = ToneMap::new(c, w);
                let t = ToneMap::hdr(c, w, peak);
                let mut prev = -1.0;
                for i in 0..2400 {
                    let y = 1e-5 * 1.01f32.powi(i);
                    let o = t.apply(y);
                    assert!((0.0..=peak).contains(&o), "{c} {w} {peak} at {y}: {o}");
                    assert!(o >= prev - 1e-6, "{c} {w} {peak} at {y}: {o} < {prev}");
                    prev = o;
                    // The margin is wider than a LUT step in this axis (0.006 EV), so the upper
                    // bracketing node is below the lift too and the two tables agree exactly.
                    if (y / GREY).log2() * slope <= HDR_FROM_EV - 0.05 {
                        assert!((o - sdr.apply(y)).abs() < 1e-6, "SDR range differs at {y}");
                    }
                }
                assert!(t.apply(4.0) > sdr.apply(4.0));
                if (c, w) == (0.0, 0.0) {
                    assert!(t.apply(GREY * 2f32.powf(LUT_MAX_EV)) > peak * 0.99, "{peak}");
                }
            }
        }
        // a broken peak falls back to SDR
        assert_eq!(ToneMap::hdr(0.0, 0.0, f32::NAN).lut(), ToneMap::new(0.0, 0.0).lut());
    }

    #[test]
    fn display_hdr_passes_hdr_sources_and_keeps_sdr() {
        assert_eq!(ToneMap::display_hdr(20.0, 10.0, -5.0, 1.0).lut(), ToneMap::display(20.0, 10.0, -5.0).lut());
        let t = ToneMap::display_hdr(0.0, 0.0, 0.0, 8.0);
        for i in 1..=90 {
            let y = i as f32 / 100.0;
            assert!((t.apply(y) - y).abs() < 2e-3, "{y} -> {}", t.apply(y));
        }
        let mut prev = 0.0;
        for i in 1..2000 {
            let o = t.apply(i as f32 / 100.0);
            assert!(o >= prev - 1e-6 && o <= 8.0);
            prev = o;
        }
        assert!(t.apply(3.0) > 2.0 && t.apply(3.0) < 3.0);
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
    fn default_curve_matches_the_reference_developer() {
        let t = ToneMap::new(0.0, 0.0);
        // Point the constants at measured scenes rather than at themselves: these are the
        // reference developer's untouched default response, read off exposure ladders on the same
        // files (see `BASE_SLOPE`). Loose bounds -- the tolerance is the fit, not this test.
        for (scene, want) in [
            (2f32.powf(-4.0) * GREY, 23.0 / 255.0), // deep shadow, lifted rather than crushed
            (0.18, 163.0 / 255.0),                  // 18 % grey is far brighter than neutral
            (2f32.powf(1.0) * GREY, 210.0 / 255.0), // upper mids
            (2f32.powf(3.0) * GREY, 249.0 / 255.0), // shoulder
        ] {
            let got = lin_to_srgb_test(t.apply(scene));
            assert!((got - want).abs() < 0.035, "scene {scene}: {got} vs {want}");
        }
    }

    /// The curve's own sRGB encode, for reading the assertions above in 0..1 display units.
    fn lin_to_srgb_test(x: f32) -> f32 {
        if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
    }

    #[test]
    fn default_curve_is_monotone_bounded_and_off_black() {
        let t = ToneMap::new(0.0, 0.0);
        assert_eq!(t.apply(0.0), 0.0);
        let mut prev = -1.0;
        for i in 0..=8000 {
            let y = 1e-7 * 1.012f32.powi(i);
            let o = t.apply(y);
            assert!((0.0..1.0).contains(&o) && o >= prev - 1e-6, "at {y}: {o} < {prev}");
            prev = o;
        }
        // the logistic approaches white without reaching it, and never overshoots
        assert!(t.apply(1e4) < 1.0 && t.apply(1e4) > 0.999);
    }

    #[test]
    fn sliders_move_the_right_way() {
        let base = ToneMap::new(0.0, 0.0);
        let contrast = ToneMap::new(60.0, 0.0);
        assert!(contrast.apply(0.05) < base.apply(0.05));
        assert!(contrast.apply(0.8) > base.apply(0.8));
        assert!(ToneMap::new(0.0, 60.0).apply(0.8) > base.apply(0.8));
        // Blacks no longer moves the tone map at all; it has its own curve.
        assert!(Blacks::new(-60.0).apply(0.01) < Blacks::IDENTITY.apply(0.01));
        assert!(Blacks::new(60.0).apply(0.01) > Blacks::IDENTITY.apply(0.01));
    }

    /// What the reference's Blacks slider does, in encoded sRGB, read off its own renders: each
    /// row is one channel of one of three cameras (Canon 5D Mark III, Pixel 4a, Fujifilm X100S),
    /// at the given slider value, grouped by the channel's own value in the render at Blacks = 0.
    /// The tolerance is the fit, not this test -- the old constant lift missed these rows by up to
    /// 0.10 in encoded sRGB, which is what the model exists to fix.
    #[test]
    fn blacks_curve_matches_the_reference_transfer() {
        const TOL: f32 = 0.035;
        let rows: [(f64, f32, f32); 20] = [
            (-100.0, 0.05, 0.0003),
            (-100.0, 0.10, 0.0005),
            (-100.0, 0.25, 0.0264),
            (-100.0, 0.50, 0.3654),
            (-100.0, 0.75, 0.6954),
            (-50.0, 0.05, 0.0008),
            (-50.0, 0.10, 0.0171),
            (-50.0, 0.25, 0.1876),
            (-50.0, 0.50, 0.4643),
            (-50.0, 0.75, 0.7361),
            (50.0, 0.05, 0.0852),
            (50.0, 0.10, 0.1397),
            (50.0, 0.25, 0.2921),
            (50.0, 0.50, 0.5348),
            (50.0, 0.75, 0.7643),
            (100.0, 0.05, 0.1165),
            (100.0, 0.10, 0.1846),
            (100.0, 0.25, 0.3458),
            (100.0, 0.50, 0.5792),
            (100.0, 0.75, 0.7808),
        ];
        for (blacks, x, want) in rows {
            let curve = Blacks::new(blacks);
            let got = lin_to_srgb_test(curve.apply(srgb_to_lin_test(x)));
            assert!((got - want).abs() < TOL, "Blacks {blacks} at {x}: {got} vs {want}");
        }
        // Negative Blacks puts a black point below the shadows; a positive one is anchored at 0,
        // so it can lift the shadows without fogging them.
        assert!(Blacks::new(-100.0).black_point() > 0.04);
        assert_eq!(Blacks::new(100.0).black_point(), 0.0);
        assert_eq!(Blacks::new(100.0).apply(0.0), 0.0);
        // the reference stops being purely per-channel above +25
        assert!(Blacks::new(-100.0).blend().is_some());
        assert!(Blacks::new(25.0).blend().is_none());
        assert_eq!(Blacks::new(100.0).blend(), Some(0.4));
        // neutral is the identity, exactly; hostile input is not a curve
        assert!(Blacks::new(0.0).is_identity());
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Blacks::new(bad).is_identity(), "{bad}");
        }
        assert!(Blacks::new(-1e9).black_point() >= Blacks::new(-100.0).black_point());
        assert_eq!(Blacks::new(1e9), Blacks::new(100.0));
        assert_eq!(Blacks::IDENTITY.apply(f32::NAN), 0.0);
        assert_eq!(Blacks::IDENTITY.apply(-1.0), 0.0);
        assert_eq!(Blacks::new(-50.0).apply(2.0), 1.0);
    }

    /// The encode these landmark rows are quoted in.
    fn srgb_to_lin_test(x: f32) -> f32 {
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    }

    /// What the reference's Whites slider does, read off its own renders: a scene luminance that
    /// renders at a given encoded level with Whites at 0, and the encoded level it renders at with
    /// the slider at `whites`. Same three cameras as [`Self::blacks_curve_matches_the_reference_transfer`].
    /// The tolerance is the fit — the old shoulder-only coefficients missed these rows by up to
    /// 0.071, and in the other direction: they darkened the shadows on a negative Whites.
    #[test]
    fn whites_curve_matches_the_reference_transfer() {
        const TOL: f32 = 0.035;
        // (whites, scene luminance, measured encoded out) -- the encoded baseline level is in the
        // comment, 0.25 / 0.50 / 0.75 / 0.90 top to bottom of each block.
        let rows: [(f64, f32, f32); 16] = [
            (-100.0, 0.039_50, 0.2400),
            (-100.0, 0.111_98, 0.4643),
            (-100.0, 0.273_11, 0.6693),
            (-100.0, 0.596_72, 0.8094),
            (-50.0, 0.039_50, 0.2415),
            (-50.0, 0.111_98, 0.4721),
            (-50.0, 0.273_11, 0.7003),
            (-50.0, 0.596_72, 0.8573),
            (50.0, 0.039_50, 0.2598),
            (50.0, 0.111_98, 0.5450),
            (50.0, 0.273_11, 0.8248),
            (50.0, 0.596_72, 0.9543),
            (100.0, 0.039_50, 0.3395),
            (100.0, 0.111_98, 0.7357),
            (100.0, 0.273_11, 0.9457),
            (100.0, 0.596_72, 0.9920),
        ];
        for (whites, scene, want) in rows {
            let got = lin_to_srgb_test(ToneMap::new(0.0, whites).apply(scene));
            assert!((got - want).abs() < TOL, "Whites {whites} at {scene}: {got} vs {want}");
        }
        // One parameter, and it is asymmetric: +100 moves the white point 4x as far as the
        // reference used to be said to, and the slope follows it.
        assert_eq!(whites_ev(0.0), 0.0);
        assert!((whites_ev(100.0) - 1.753).abs() < 1e-6);
        assert!((whites_ev(-100.0) + 0.420).abs() < 1e-6);
        // Not the linear pair it replaced (1.00 up, 0.42 down): convex both ways, and asymmetric.
        assert!(whites_ev(50.0) < 0.5 * whites_ev(100.0), "convex: {}", whites_ev(50.0));
        assert!(-whites_ev(-50.0) > 0.5 * -whites_ev(-100.0), "convex: {}", whites_ev(-50.0));
        assert!(whites_ev(100.0) > 4.0 * -whites_ev(-100.0), "up beats down");
        let mut previous = f32::NEG_INFINITY;
        for w in (-100..=100).step_by(5) {
            let d = whites_ev(w as f64);
            assert!(d > previous, "Whites {w}");
            previous = d;
        }
        // clamping, and hostile input is neutral rather than a curve
        assert_eq!(whites_ev(1e9), whites_ev(100.0));
        assert_eq!(whites_ev(-1e9), whites_ev(-100.0));
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(whites_ev(bad), 0.0, "{bad}");
        }
        // a negative Whites darkens the top; a positive one lifts everything
        let base = ToneMap::new(0.0, 0.0);
        assert!(ToneMap::new(0.0, -100.0).apply(0.6) < base.apply(0.6));
        assert!(ToneMap::new(0.0, 100.0).apply(0.1) > base.apply(0.1));
    }

    /// The per-photo scale on positive Whites, against the ten-body fit it came from.
    #[test]
    fn whites_scale_is_the_measured_highlight_relation() {
        // Exactly 1 at the corpus mean, and falling as the frame's own highlights get brighter.
        assert!((whites_scale(WHITES_SCALE_MID) - 1.0).abs() < 1e-6);
        let mut previous = f32::INFINITY;
        for i in 0..=12 {
            let p = 0.70 + i as f32 * 0.02;
            let m = whites_scale(p);
            assert!(m < previous, "p99 {p}: {m} !< {previous}");
            previous = m;
        }
        // The ends of the measured corpus land where they were measured (0.9992 -> 0.46 for the
        // brightest-highlight body, 0.7334 -> 1.49 for the dimmest).
        assert!((whites_scale(0.9992) - 0.46).abs() < 0.02, "{}", whites_scale(0.9992));
        assert!((whites_scale(0.7334) - 1.49).abs() < 0.02, "{}", whites_scale(0.7334));
        // The rails hold, and hostile input is neutral rather than a curve.
        assert_eq!(whites_scale(1.5), WHITES_SCALE_MIN);
        assert_eq!(whites_scale(0.0), WHITES_SCALE_MAX);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(whites_scale(bad), 1.0, "{bad}");
        }
    }

    /// The scale moves positive Whites only: at and below zero the curve cannot tell it is there.
    #[test]
    fn whites_scale_touches_positive_whites_only() {
        for w in [-100.0, -50.0, -1.0, 0.0] {
            for s in [WHITES_SCALE_MIN, 0.5, 1.0, WHITES_SCALE_MAX] {
                assert_eq!(ToneMap::hdr(0.0, w, 1.0).lut(), ToneMap::hdr_scaled(0.0, w, 1.0, s).lut(), "hdr Whites {w} moved with scale {s}");
            }
        }
        // Above zero it moves it, the way the scale says.
        let fixed = ToneMap::hdr(0.0, 100.0, 1.0).apply(0.6);
        assert!(ToneMap::hdr_scaled(0.0, 100.0, 1.0, 1.5).apply(0.6) > fixed);
        assert!(ToneMap::hdr_scaled(0.0, 100.0, 1.0, 0.5).apply(0.6) < fixed);
        // A hostile scale falls back to the fixed curve rather than poisoning the table.
        for bad in [f32::NAN, f32::INFINITY] {
            assert_eq!(ToneMap::hdr(0.0, 100.0, 1.0).lut(), ToneMap::hdr_scaled(0.0, 100.0, 1.0, bad).lut(), "{bad}");
        }
    }

    /// Whites on a file whose tone comes from a camera look is the measured scene-domain move, not
    /// the display-domain lift it used to be -- which left it nearly inert there (measured dew
    /// +0.26 where the reference moves +2.60 at +100), and was most of the error on such files.
    #[test]
    fn camera_path_whites_makes_the_measured_move() {
        let knots = std::array::from_fn(|i| {
            let x = 0.005 * 1.15f32.powi(i as i32);
            [x, 1.0 - (-3.0 * x).exp()]
        });
        let curve = CameraTone::new(knots).unwrap();
        let neutral = ToneMap::camera(&curve, 0.0, 0.0);
        let up = ToneMap::camera(&curve, 0.0, 100.0);
        let down = ToneMap::camera(&curve, 0.0, -100.0);
        let moved = up.apply(0.5) - neutral.apply(0.5);
        assert!(moved > 0.10, "a positive Whites must move the upper tones: {moved}");
        assert!(down.apply(0.5) < neutral.apply(0.5));
        // Monotone and bounded across the whole range, at every scale.
        for scale in [WHITES_SCALE_MIN, 0.5, 1.0, 1.5, WHITES_SCALE_MAX] {
            let t = ToneMap::camera_scaled(&curve, 0.0, 100.0, scale);
            let mut previous = -1.0;
            for i in 0..2400 {
                let y = 1e-6 * 1.01f32.powi(i);
                let v = t.apply(y);
                assert!((0.0..=1.0).contains(&v), "scale {scale} at {y}: {v}");
                assert!(v >= previous - 1e-6, "scale {scale} at {y}: {v} < {previous}");
                previous = v;
            }
        }
        assert!(ToneMap::camera_scaled(&curve, 0.0, 100.0, 1.5).apply(0.5) > up.apply(0.5));
        for w in [-100.0, 0.0] {
            assert_eq!(ToneMap::camera(&curve, 0.0, w).lut(), ToneMap::camera_scaled(&curve, 0.0, w, 1.5).lut(), "Whites {w}");
        }
    }

    /// Contrast is the reference's measured S about its own pivot, not a slope about grey.
    #[test]
    fn contrast_is_the_measured_s_curve() {
        // Identity at neutral, exactly, so an untouched render is unchanged.
        assert_eq!(contrast_g(0.0), 1.0);
        // Monotone in the slider, and the sign flips at zero: positive deepens, negative flattens.
        let mut previous = 0.0f32;
        for v in 1..=100 {
            let l = contrast_g(v as f64).ln().abs();
            assert!(l > previous, "contrast {v}: {l} !> {previous}");
            previous = l;
        }
        for v in [10.0, 25.0, 50.0, 100.0] {
            assert!(contrast_g(v) < 1.0 && contrast_g(-v) > 1.0, "{v}");
        }
        // Hostile input is neutral, and the slider clamps.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(contrast_g(bad), 1.0, "{bad}");
        }
        assert_eq!(contrast_g(1e9), contrast_g(100.0));
        assert_eq!(contrast_g(-1e9), contrast_g(-100.0));
        // The pivot is the fixed point, whatever the strength, and the map stays in range.
        let pivot = lightcraft_color::transfer::srgb_to_linear(CONTRAST_PIVOT);
        for g in [0.6f32, 0.8, 1.0, 1.2, 1.6] {
            // Loose on purpose: for g < 1 the S has infinite slope at the pivot, so `|t|^g`
            // magnifies the encode/decode round trip rather than the map being wrong.
            assert!((contrast_display(pivot, g) - pivot).abs() < 1e-3, "g {g}");
            let mut previous = -1.0;
            for i in 0..=1000 {
                let o = i as f32 / 1000.0;
                let v = contrast_display(o, g);
                assert!((0.0..=1.0).contains(&v), "g {g} at {o}: {v}");
                assert!(v >= previous - 1e-6, "g {g} at {o}: {v} < {previous}");
                previous = v;
            }
        }
        // A positive Contrast lifts above the pivot and drops below it; a negative one the reverse.
        let base = ToneMap::new(0.0, 0.0);
        let deep = ToneMap::new(100.0, 0.0);
        let flat = ToneMap::new(-100.0, 0.0);
        for y in [0.02f32, 0.05, 0.1] {
            assert!(deep.apply(y) < base.apply(y), "shadow {y}");
            assert!(flat.apply(y) > base.apply(y), "shadow {y}");
        }
        for y in [0.6f32, 1.0, 2.0] {
            assert!(deep.apply(y) > base.apply(y), "highlight {y}");
            assert!(flat.apply(y) < base.apply(y), "highlight {y}");
        }
    }
}
