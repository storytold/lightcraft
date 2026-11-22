//! Brightness (beyond Lightroom, as Capture One's Brightness): lifts or darkens the midtones while black and white
//! stay fixed, so nothing clips.
//!
//! **Where it runs:** in [`crate::finish`], once, right after the encode and before the Levels / Tone Curve stage, on
//! the final in-gamut display values (the output primaries with the sRGB curve). The single call site is
//! `finish_with` (and the kernel's, in `finish.wgsl`). Capture One's operator was measured as it comes out of the
//! export (relative to the untouched export of the same 16-bit sRGB file), so it is only valid here, after the
//! output encoding: a base or output curve of its own, if one is ever added, goes before this stage, never after it.
//! Capture One applies it after Exposure, Contrast and Saturation and before Levels, which is the order of this stage.
//!
//! **The operator** (own fit to Capture One exports, aggregate numbers only; accurate to about 0.02 of an 8-bit level
//! on every grey and colour patch measured):
//!
//! - *Grey:* a shift in logit space of the encoded value, `logit(v) = logit(u) + phi`, that is
//!   `v = u·E / (1 + u·(E − 1))` with `E = e^phi` and `phi = PHI_PER_UNIT · c1`, where `c1 = cf / 2` (the slider runs
//!   -100..100, Capture One's -50..50 doubled). The curve fixes 0 and 1, is monotone, has slopes `E` at 0 and `1/E` at
//!   1, and `-c1` gives its exact inverse.
//! - *Colour:* the brightest channel `V` takes `f(V)` exactly and the colour keeps its HSV hue (each channel keeps its
//!   place between the darkest and the brightest). The saturation `S = 1 − m/V` becomes
//!   `S' = (1 − SAT_WEIGHT)·S + SAT_WEIGHT·S_pc`, with `S_pc = 1 − f(m)/f(V)` the saturation per-channel `f` would give
//!   (`m` the darkest channel); a quarter of the colour is left as it was, three quarters follow the per-channel curve.
//!
//! **Beyond SDR white (HDR renders)** the stage keeps everything at or below SDR white exactly as the SDR render has it
//! (an HDR edit is also rendered once more, with HDR off, as its SDR rendition: the two must agree below white). The
//! range from SDR white 1 up to the headroom `H` (the render's peak, encoded: `linear_to_srgb(peak)`) gets the same
//! curve rescaled onto it, `1 + (H − 1)·f((v − 1)/(H − 1))`: black, SDR white and the peak stay fixed, the direction of
//! the slider is the same on both sides of white, and value, order and monotony are kept. The slope has a kink at SDR
//! white (`1/E` below, `E` above; the ratio is `E²`). Above `H` (nothing is meant to be there) the value passes through.
//! An SDR render limits at 1; below 0 everything is limited to 0 (nothing below 0 reaches this stage).
//!
//! The curve is this one function ([`curve`]) and the two constants below; nothing else holds the maths. CPU and
//! GPU both evaluate it in closed form (the shader takes `E`, the headroom and the saturation weight from the parameter
//! block, so the constants live here only).

/// Logit shift of the encoded value per Capture One unit (fitted to the exports: 1.2476 at C1 +50, not 1.25).
pub const PHI_PER_UNIT: f64 = 0.02495;
/// Slider units per Capture One unit: the slider runs -100..100 where Capture One's runs -50..50.
pub const CF_PER_C1: f64 = 2.0;
/// How much of the colour's saturation follows the per-channel curve (the rest keeps the original saturation).
pub const SAT_WEIGHT: f32 = 0.75;
/// The slider's reach either way.
pub const MAX_CF: f64 = 100.0;
/// Inputs are limited to this (HDR values are far below it): keeps every sum and ratio finite for hostile input.
const MAX_IN: f32 = 1.0e4;

/// The Brightness of a render: `E = e^phi`, `phi` the logit shift, and the headroom `H` of an HDR render.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrightnessK {
    pub e: f32,
    /// The HDR render's peak in encoded units (`linear_to_srgb(peak)`), 1 for an SDR render.
    pub h: f32,
}

impl BrightnessK {
    /// The stage for slider value `cf` (limited to ±100) in an SDR render, `None` while it changes nothing (0, -0, not a number).
    pub fn new(cf: f64) -> Option<BrightnessK> {
        if !cf.is_finite() || cf == 0.0 {
            return None;
        }
        let phi = PHI_PER_UNIT * cf.clamp(-MAX_CF, MAX_CF) / CF_PER_C1;
        Some(BrightnessK { e: phi.exp() as f32, h: 1.0 })
    }

    /// The stage for a render whose peak (linear, relative to SDR white; 1 for SDR) is `peak`.
    pub fn for_peak(cf: f64, peak: f32) -> Option<BrightnessK> {
        let mut k = BrightnessK::new(cf)?;
        if peak.is_finite() && peak > 1.0 {
            k.h = lcc_color::transfer::linear_to_srgb(peak).max(1.0);
        }
        Some(k)
    }

    /// The saturation weight the GPU kernel is given.
    pub fn sat_weight(&self) -> f32 {
        SAT_WEIGHT
    }

    /// The headroom the curve works with: `None` for an SDR render.
    fn headroom(&self, hdr: bool) -> Option<f32> {
        hdr.then(|| self.h.max(1.0))
    }

    /// The curve on one value; `hdr` lets values above 1 through.
    #[inline]
    pub fn curve(&self, v: f32, hdr: bool) -> f32 {
        curve(v, self.e, self.headroom(hdr))
    }

    /// The encoded colour `c` through the stage (grey, hue-preserving saturation blend; see the module docs).
    #[inline]
    pub fn apply(&self, c: [f32; 3], hdr: bool) -> [f32; 3] {
        self.apply_with(c, self.headroom(hdr))
    }

    // (`max` then `min`, not `clamp`: a NaN comes out as 0, where `clamp` would pass it on)
    #[allow(clippy::manual_clamp)]
    fn apply_with(&self, c: [f32; 3], h: Option<f32>) -> [f32; 3] {
        let c = c.map(|v| v.max(0.0).min(MAX_IN));
        let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
        if mx <= 0.0 {
            return [0.0; 3];
        }
        let vp = curve(mx, self.e, h);
        let range = mx - mn;
        if range <= 0.0 {
            return [vp; 3];
        }
        // the darkest channel: (1 − w)·V'·(m/V) + w·f(m)
        let mp = (1.0 - SAT_WEIGHT) * vp * (mn / mx) + SAT_WEIGHT * curve(mn, self.e, h);
        c.map(|v| {
            if v >= mx {
                vp
            } else if v <= mn {
                mp
            } else {
                mp + (v - mn) / range * (vp - mp)
            }
        })
    }
}

/// The grey curve on 0..1: `v·E / (1 + v·(E − 1))`. Above 1 an SDR render (`h` = `None`) holds 1; an HDR render with the
/// headroom `h` (encoded peak, at least 1) applies the same curve rescaled onto `1..h` and passes values above `h`
/// through. Below 0 (and not a number) the result is 0. `E = e^phi` is 1 for no Brightness (then it is the identity).
// (`max` then `min`, not `clamp`: a NaN comes out as 0, where `clamp` would pass it on)
#[allow(clippy::manual_clamp)]
#[inline]
pub fn curve(v: f32, e: f32, h: Option<f32>) -> f32 {
    let v = v.max(0.0).min(MAX_IN);
    if v < 1.0 {
        return v * e / (1.0 + v * (e - 1.0));
    }
    match h {
        None => 1.0,
        Some(h) if v < h => {
            let r = h - 1.0;
            let x = (v - 1.0) / r;
            1.0 + r * (x * e / (1.0 + x * (e - 1.0)))
        }
        Some(_) => v,
    }
}

#[cfg(test)]
#[path = "tests_brightness.rs"]
mod tests;
