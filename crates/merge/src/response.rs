//! Camera response recovery for display-referred brackets (JPEG, and 8- or 16-bit TIFF and PNG).
//!
//! A camera that writes a JPEG applies its own tone curve before the file's transfer function, so the decoded values
//! are not proportional to the light even once the sRGB (or ICC) curve is undone. Merged as if they were, the frames
//! disagree about the same scene point: mid-tones of the brighter frames count as brighter than they are, and the
//! weighted mean bands and bends gradients.
//!
//! The curve is recovered from the brackets themselves, after P. E. Debevec and J. Malik, "Recovering High Dynamic
//! Range Radiance Maps from Photographs", SIGGRAPH 1997, §2.1. For pixel values `Z` in `L` levels and exposure times
//! `Δt`, it finds `g = ln f⁻¹` and the log radiances `ln E` of the sampled pixels that minimise
//!
//! ```text
//! Σᵢ Σⱼ [w(Zᵢⱼ) (g(Zᵢⱼ) − ln Eᵢ − ln Δtⱼ)]²  +  λ² Σ_z [w(z) g″(z)]²
//! ```
//!
//! with the hat weight `w` and `g` fixed at the middle level, for each channel on its own. Written from the paper; no
//! other implementation was consulted. Where this differs from it:
//!
//! - `Z` is the frame's value re-encoded with the sRGB curve, in 256 levels. For an sRGB JPEG that is the file's own
//!   code value; for another transfer function the fitted curve absorbs the difference, as it only has to be monotone.
//! - Each `ln Eᵢ` appears only in its own pixel's rows, so it is eliminated from the normal equations (the Schur
//!   complement of their diagonal block). That leaves an `L × L` system whatever the number of samples, so thousands
//!   of samples are used instead of the paper's hand-picked fifty, taken on a grid where every frame is flat in a 3 × 3
//!   neighbourhood (an edge sampled a pixel off would disagree with itself).
//! - `λ` is scaled by the data's weight per level, so the same value holds for any number of samples.
//! - The curve is made non-decreasing, level 0 means no light, and it is scaled so the top level is 1.0 (the frames
//!   keep "1.0 = the white level").
//! - No curve is fitted when the samples cover too few distinct levels (a bracket of flat patches): a smooth curve
//!   through a few levels fits them almost exactly and says nothing about the levels between.
//! - The curve is used only when it explains the samples clearly better than the linear assumption does
//!   ([`MIN_GAIN`]), so brackets that are already linear (a linear 16-bit TIFF, an sRGB PNG rendered from linear light)
//!   are left as they were.

use lightcraft_color::transfer::{linear_to_srgb, srgb_to_linear};
use rayon::prelude::*;

use crate::frame::Frame;
use crate::linalg;

/// Levels of `Z`.
pub const LEVELS: usize = 256;
/// The level `g` is fixed at (0 there).
const MID: usize = LEVELS / 2;
/// Samples the grid aims for.
const TARGET_SAMPLES: usize = 20_000;
/// Fewer flat samples than this and no curve is fitted.
const MIN_SAMPLES: usize = 1_000;
/// Fewer distinct levels than this, in any channel, among the samples that tie two or more frames together, and no
/// curve is fitted: samples that repeat a few levels (flat patches) fit a smooth curve through them almost exactly, and
/// that curve says nothing about the levels between them. A quarter of the levels leaves the smoothness term short gaps
/// to bridge, not most of the range.
const MIN_DISTINCT_LEVELS: usize = LEVELS / 4;
/// Widest spread of levels in a 3 × 3 neighbourhood, in every frame and channel, for a sample.
const FLAT_LEVELS: usize = 6;
/// Smoothness against data: the weight of `g″` at a level relative to the data weight an average level gets.
const LAMBDA: f64 = 30.0;
/// The fitted curve must leave at most this share of the linear assumption's squared residual.
pub const MIN_GAIN: f64 = 0.5;

/// The response recovered for a bracket: for each channel, the linear value of each level of `Z`.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseCurve {
    /// `levels[c][z]`: non-decreasing, `0.0` at level 0 and `1.0` at the top.
    pub levels: [Vec<f32>; 3],
    /// Squared residual of the samples under the fitted curve over that under the linear assumption (below
    /// [`MIN_GAIN`]; smaller is a bigger improvement).
    pub residual_ratio: f64,
}

impl ResponseCurve {
    /// The linear value of a frame value `v` of channel `c` (interpolated between levels).
    pub fn apply(&self, c: usize, v: f32) -> f32 {
        let Some(lv) = self.levels.get(c) else { return v };
        let t = level(v) * (LEVELS - 1) as f32;
        let i = (t.floor() as usize).min(LEVELS - 2);
        let f = t - i as f32;
        match (lv.get(i), lv.get(i + 1)) {
            (Some(a), Some(b)) => a + (b - a) * f,
            _ => v,
        }
    }

    /// Replace a frame's values with linear ones, and move its clip level with them (to the lowest channel's, so
    /// saturation is never seen later than before).
    pub fn linearise(&self, frame: &mut Frame) {
        frame.image.data.par_iter_mut().for_each(|p| *p = [0, 1, 2].map(|c| self.apply(c, p[c])));
        frame.clip = (0..3).map(|c| self.apply(c, frame.clip)).fold(f32::INFINITY, f32::min);
    }
}

/// A frame value re-encoded with the sRGB curve, 0..1.
fn level(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { linear_to_srgb(v.clamp(0.0, 1.0)).clamp(0.0, 1.0) }
}

fn quantise(v: f32) -> usize {
    ((level(v) * (LEVELS - 1) as f32).round() as usize).min(LEVELS - 1)
}

/// The paper's hat weight: rises from 0 at the bottom level to the middle and falls to 0 at the top.
fn hat(z: usize) -> f64 {
    if z <= (LEVELS - 1) / 2 { z as f64 } else { (LEVELS - 1).saturating_sub(z) as f64 }
}

/// Recover the response of `frames` (one bracket, aligned, same size) from their log exposures `ln_dt` (natural log,
/// any common offset). `valid[i]`, when given, marks the pixels frame `i` has after alignment. `None` when the
/// brackets don't constrain a curve (too few flat samples, too few distinct levels among them, a degenerate fit) or
/// the curve is not clearly better than the linear assumption.
pub fn recover(frames: &[Frame], valid: &[Option<Vec<bool>>], ln_dt: &[f64]) -> Option<ResponseCurve> {
    let n = frames.len();
    if n < 2 || ln_dt.len() != n || valid.len() != n || ln_dt.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let samples = sample(frames, valid);
    if samples.len() < MIN_SAMPLES || (0..3).any(|c| distinct_levels(&samples, c) < MIN_DISTINCT_LEVELS) {
        return None;
    }
    let mut levels: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let (mut fitted, mut linear) = (0.0, 0.0);
    let lin_g: Vec<f64> = (0..LEVELS).map(|z| f64::from(srgb_to_linear(z as f32 / (LEVELS - 1) as f32)).ln()).collect();
    for (c, out) in levels.iter_mut().enumerate() {
        let g = fit_channel(&samples, c, ln_dt)?;
        fitted += residual(&samples, c, ln_dt, &g);
        linear += residual(&samples, c, ln_dt, &lin_g);
        let top = *g.last()?;
        *out = g.iter().enumerate().map(|(z, v)| if z == 0 { 0.0 } else { (v - top).exp() as f32 }).collect();
    }
    let residual_ratio = fitted / linear;
    (residual_ratio.is_finite() && residual_ratio < MIN_GAIN).then_some(ResponseCurve { levels, residual_ratio })
}

/// The distinct levels of channel `c` that constrain `g`: those with weight in a sample where at least two frames have
/// weight (a sample seen in one frame only fixes its own `ln E` and nothing of the curve).
fn distinct_levels(samples: &[Vec<[u8; 3]>], c: usize) -> usize {
    let mut seen = [false; LEVELS];
    for zs in samples {
        let weighted: Vec<usize> = zs.iter().map(|z| usize::from(z[c])).filter(|&z| hat(z) > 0.0).collect();
        if weighted.len() >= 2 {
            for z in weighted {
                if let Some(s) = seen.get_mut(z) {
                    *s = true;
                }
            }
        }
    }
    seen.iter().filter(|s| **s).count()
}

/// The levels of each frame at flat, valid pixels of a grid: `samples[s][j][c]`.
fn sample(frames: &[Frame], valid: &[Option<Vec<bool>>]) -> Vec<Vec<[u8; 3]>> {
    let Some(first) = frames.first() else { return Vec::new() };
    let (w, h) = (first.width(), first.height());
    if w < 3 || h < 3 || frames.iter().any(|f| f.width() != w || f.height() != h || f.image.data.len() != w * h) {
        return Vec::new();
    }
    let step = ((w * h) as f64 / TARGET_SAMPLES as f64).sqrt().max(1.0) as usize;
    let rows: Vec<usize> = (1..h - 1).step_by(step).collect();
    rows.par_iter()
        .flat_map_iter(|&y| {
            (1..w - 1).step_by(step).filter_map(move |x| {
                let i = y * w + x;
                if valid.iter().any(|v| v.as_ref().is_some_and(|v| !v.get(i).copied().unwrap_or(false))) {
                    return None;
                }
                let mut zs = Vec::with_capacity(frames.len());
                for f in frames {
                    let mut lo = [usize::MAX; 3];
                    let mut hi = [0usize; 3];
                    for yy in y - 1..=y + 1 {
                        for xx in x - 1..=x + 1 {
                            let p = f.image.data.get(yy * w + xx)?;
                            for c in 0..3 {
                                let z = quantise(p[c]);
                                lo[c] = lo[c].min(z);
                                hi[c] = hi[c].max(z);
                            }
                        }
                    }
                    if (0..3).any(|c| hi[c] - lo[c] > FLAT_LEVELS) {
                        return None;
                    }
                    let p = f.image.data.get(i)?;
                    zs.push(p.map(|v| quantise(v) as u8));
                }
                Some(zs)
            })
        })
        .collect()
}

/// `g` for channel `c`: the normal equations of the paper's least squares, with the `ln E` eliminated.
fn fit_channel(samples: &[Vec<[u8; 3]>], c: usize, ln_dt: &[f64]) -> Option<Vec<f64>> {
    let l = LEVELS;
    let mut a = vec![0f64; l * l];
    let mut b = vec![0f64; l];
    let mut total = 0f64;
    for zs in samples {
        // squared row weights of this pixel's rows
        let ws: Vec<(usize, f64, f64)> = zs.iter().zip(ln_dt).map(|(z, t)| (usize::from(z[c]), hat(usize::from(z[c])).powi(2), *t)).collect();
        let d: f64 = ws.iter().map(|(_, w, _)| w).sum();
        if d <= 0.0 {
            continue;
        }
        total += d;
        // b_E of this pixel's ln E (its rows read  w·g(z) − w·ln E = w·ln Δt)
        let b_e: f64 = -ws.iter().map(|(_, w, t)| w * t).sum::<f64>();
        for &(zj, wj, tj) in &ws {
            *a.get_mut(zj * l + zj)? += wj;
            *b.get_mut(zj)? += wj * tj + wj * b_e / d;
            for &(zk, wk, _) in &ws {
                *a.get_mut(zj * l + zk)? -= wj * wk / d;
            }
        }
    }
    if total <= 0.0 {
        return None;
    }
    let per_level = total / (0..l).map(|z| hat(z).powi(2)).sum::<f64>();
    for z in 1..l - 1 {
        let s = LAMBDA * LAMBDA * per_level * hat(z).powi(2);
        let taps = [(z - 1, 1.0), (z, -2.0), (z + 1, 1.0)];
        for &(p, cp) in &taps {
            for &(q, cq) in &taps {
                *a.get_mut(p * l + q)? += s * cp * cq;
            }
        }
    }
    // g(MID) = 0
    *a.get_mut(MID * l + MID)? += per_level * hat(MID).powi(2);
    let mut g = linalg::solve(a, b, l)?;
    // a response never falls: a level that came out below the one under it takes that one's value
    for z in 1..l {
        let below = *g.get(z - 1)?;
        let v = g.get_mut(z)?;
        *v = v.max(below);
    }
    let (mid, top) = (*g.get(MID)?, *g.last()?);
    (g.iter().all(|v| v.is_finite()) && top > mid).then_some(g)
}

/// The paper's data term for channel `c` under the curve `g`, each pixel at its best `ln E`.
fn residual(samples: &[Vec<[u8; 3]>], c: usize, ln_dt: &[f64], g: &[f64]) -> f64 {
    samples
        .iter()
        .map(|zs| {
            let rows: Vec<(f64, f64)> = zs
                .iter()
                .zip(ln_dt)
                .filter_map(|(z, t)| {
                    let z = usize::from(z[c]);
                    let w = hat(z).powi(2);
                    (w > 0.0).then(|| g.get(z).map(|gz| (w, gz - t))).flatten()
                })
                .collect();
            let d: f64 = rows.iter().map(|(w, _)| w).sum();
            if d <= 0.0 {
                return 0.0;
            }
            let ln_e = rows.iter().map(|(w, v)| w * v).sum::<f64>() / d;
            rows.iter().map(|(w, v)| w * (v - ln_e).powi(2)).sum::<f64>()
        })
        .sum()
}
