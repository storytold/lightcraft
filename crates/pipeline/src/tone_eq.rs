//! Tone Equalizer (beyond Lightroom): exposure per luminance band on the scene-linear image.
//!
//! Translated from darktable's `src/iop/toneequal.c` (GPL-3.0-or-later, Aurélien Pierre and the
//! darktable developers): nine bands one EV apart from -8 EV to middle grey, joined by a sum of
//! Gaussian radial basis functions whose weights are solved so the curve passes through the bands
//! (darktable's `compute_correction` and `pseudo_solve`), evaluated on a luminance mask that a guided
//! filter smooths so the change follows regions, not pixels, and local contrast inside a band stays.
//! Differences from darktable: the mask is the Rec. 2020 luminance (darktable offers several norms),
//! the guided filter runs on log2 luminance (shift-equivariant, so it can run before Exposure and stay
//! exact), its window is Gaussian and relative to the long edge, and the linear system is solved by a
//! ridge-regularized Gaussian elimination instead of a pseudo-inverse.
//!
//! Runs after noise reduction, Smooth Skin and the retouch of people's parts, before Exposure is
//! applied; Exposure is added to the mask, so a band keeps meaning "this many EV below grey" as shown.
//! Both renderers run this code (the GPU renderer reads the image back for it), so they match.

use lcc_develop::tone_eq::{BANDS, FIRST_BAND_EV, MAX_BAND_EV};
use lcc_develop::{DevelopSettings, ToneEq};
use lcc_raster::{Plane, Rgb32f};

use crate::for_rows;

/// Rec. 2020 luminance weights (the pipeline's working space).
const LUMA: [f32; 3] = [0.2627, 0.6780, 0.0593];
/// Middle grey, scene-linear.
const GREY: f32 = crate::tone::GREY;
/// Darkest luminance the mask reads (16 EV below grey): no log of zero.
const FLOOR: f32 = GREY / 65536.0;
/// Entries of the gain table across the bands' span.
const LUT_N: usize = 1025;

/// Whether the Tone Equalizer changes a render of `s`.
pub fn active(s: &DevelopSettings) -> bool {
    s.tone_eq.active()
}

/// What its result depends on (`None` when it does nothing): a stage-cache key. Exposure is in it, as
/// the mask is read after Exposure.
pub fn key(s: &DevelopSettings) -> Option<u64> {
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    if !active(s) {
        return None;
    }
    let t = s.tone_eq.sanitized();
    let bits = (t.bands().map(f64::to_bits), [t.smoothing, t.detail, t.size, t.edges, s.light.exposure].map(f64::to_bits));
    Some(BuildHasherDefault::<DefaultHasher>::default().hash_one(("toneEq", bits)))
}

/// The mask smoothing's Gaussian sigma in output pixels (`ppl`: output pixels per unit of the long
/// edge); 0 when the mask is not smoothed.
fn sigma_px(t: &ToneEq, ppl: f64) -> f64 {
    if t.detail <= 0.0 || !ppl.is_finite() || ppl <= 0.0 {
        return 0.0;
    }
    // Mask Size 100: a sigma of 5 % of the long edge
    (t.size / 100.0 * 0.05 * ppl).clamp(0.0, 1e6)
}

/// How far (output pixels) the Tone Equalizer reads around a pixel it changes: a windowed render must
/// hold this much more around itself to match the whole render.
pub fn reach(s: &DevelopSettings, ppl: f64) -> f64 {
    if !active(s) {
        return 0.0;
    }
    (3.0 * sigma_px(&s.tone_eq.sanitized(), ppl)).ceil().min(1e6)
}

/// The guided filter's regularization in EV squared: higher Edge Refinement keeps edges (smaller epsilon).
fn epsilon(t: &ToneEq) -> f32 {
    10f64.powf(-(t.edges / 25.0)).clamp(1e-5, 1.0) as f32
}

/// The correction curve: band exposures through Gaussian radial basis functions.
#[derive(Clone, Debug)]
pub struct Curve {
    weights: [f64; BANDS],
    /// `1 / (2 sigma^2)` of the basis functions.
    denom: f64,
}

impl Curve {
    pub fn new(t: &ToneEq) -> Curve {
        let t = t.sanitized();
        // darktable: sigma = sqrt(2)^(1 + smoothing)
        let sigma = std::f64::consts::SQRT_2.powf(1.0 + t.smoothing);
        let denom = 1.0 / (2.0 * sigma * sigma);
        let centre = |i: usize| FIRST_BAND_EV + i as f64;
        let mut a = [[0.0; BANDS]; BANDS];
        for (i, row) in a.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                let d = centre(i) - centre(j);
                *v = (-d * d * denom).exp();
            }
        }
        let weights = solve(a, t.bands()).unwrap_or([0.0; BANDS]);
        Curve { weights, denom }
    }

    /// The exposure change (EV) at mask value `ev` (EV relative to grey): bounded to +-2, and constant
    /// outside the bands' span, as in darktable.
    pub fn at(&self, ev: f64) -> f64 {
        let x = if ev.is_finite() { ev.clamp(FIRST_BAND_EV, 0.0) } else { FIRST_BAND_EV };
        let v: f64 = self
            .weights
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let d = x - (FIRST_BAND_EV + i as f64);
                w * (-d * d * self.denom).exp()
            })
            .sum();
        if v.is_finite() { v.clamp(-MAX_BAND_EV, MAX_BAND_EV) } else { 0.0 }
    }

    /// Linear gains over `FIRST_BAND_EV..=0` in `LUT_N` steps.
    fn gains(&self) -> Vec<f32> {
        (0..LUT_N)
            .map(|i| {
                let ev = FIRST_BAND_EV - FIRST_BAND_EV * i as f64 / (LUT_N - 1) as f64;
                self.at(ev).exp2() as f32
            })
            .collect()
    }
}

/// `(A + ridge I) w = b` by Gaussian elimination with partial pivoting; the small ridge keeps the nearly
/// singular systems of smooth curves solvable (the curve then passes very close to, not exactly
/// through, the bands). `None` when it can't be solved.
fn solve(mut a: [[f64; BANDS]; BANDS], mut b: [f64; BANDS]) -> Option<[f64; BANDS]> {
    const RIDGE: f64 = 1e-6;
    for (i, row) in a.iter_mut().enumerate() {
        if let Some(v) = row.get_mut(i) {
            *v += RIDGE;
        }
    }
    for col in 0..BANDS {
        let pivot = (col..BANDS).max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for r in col + 1..BANDS {
            let f = a[r][col] / a[col][col];
            for c in col..BANDS {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0.0; BANDS];
    for r in (0..BANDS).rev() {
        let s: f64 = (r + 1..BANDS).map(|c| a[r][c] * x[c]).sum();
        x[r] = (b[r] - s) / a[r][r];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// The mask: log2 luminance relative to grey (before Exposure), smoothed by the guided filter as
/// Preserve Details asks.
pub fn mask(img: &Rgb32f, t: &ToneEq, ppl: f64) -> Plane {
    let mut l = Plane::new(img.width, img.height);
    let w = img.width.max(1);
    for_rows(&mut l.data, w, |y, row| {
        let start = y.saturating_mul(w);
        let src = img.data.get(start..start.saturating_add(row.len())).unwrap_or(&[]);
        for (v, p) in row.iter_mut().zip(src) {
            let y = LUMA[0] * p[0] + LUMA[1] * p[1] + LUMA[2] * p[2];
            *v = if y.is_finite() { (y.max(FLOOR) / GREY).log2() } else { 0.0 };
        }
    });
    let t = t.sanitized();
    let sigma = sigma_px(&t, ppl) as f32;
    if sigma < 0.5 || l.data.is_empty() {
        return l;
    }
    let smooth = crate::local::guided_fast(&l, sigma, epsilon(&t));
    if smooth.data.len() != l.data.len() {
        return l;
    }
    let k = (t.detail / 100.0).clamp(0.0, 1.0) as f32;
    for (v, s) in l.data.iter_mut().zip(&smooth.data) {
        *v += (s - *v) * k;
    }
    l
}

/// Applies the Tone Equalizer to the scene-linear (not yet exposed) image in place.
pub fn apply(img: &mut Rgb32f, s: &DevelopSettings, ppl: f64) {
    if !active(s) || img.data.is_empty() {
        return;
    }
    let t = s.tone_eq.sanitized();
    let ev = if s.light.exposure.is_finite() { s.light.exposure.clamp(-100.0, 100.0) as f32 } else { 0.0 };
    let m = mask(img, &t, ppl);
    let gains = Curve::new(&t).gains();
    let top = (LUT_N - 1) as f32;
    let step = top / -(FIRST_BAND_EV as f32);
    let w = img.width.max(1);
    for_rows(&mut img.data, w, |y, row| {
        let start = y.saturating_mul(w);
        let ms = m.data.get(start..start.saturating_add(row.len())).unwrap_or(&[]);
        for (p, mv) in row.iter_mut().zip(ms) {
            let x = (mv + ev - FIRST_BAND_EV as f32) * step;
            let x = if x.is_finite() { x.clamp(0.0, top) } else { 0.0 };
            let i = x as usize;
            let f = x - i as f32;
            let g0 = gains.get(i).copied().unwrap_or(1.0);
            let g1 = gains.get(i + 1).copied().unwrap_or(g0);
            let g = g0 + (g1 - g0) * f;
            for c in p.iter_mut() {
                *c *= g;
            }
        }
    });
}
