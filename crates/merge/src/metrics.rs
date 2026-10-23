//! How sharp a merge is: measures for tests, benchmarks and tuning.
//!
//! - [`detail`]: local high-pass energy of a picture's √luminance (√ so shadows count, as the eye
//!   sees them): the squared difference from a σ = 1 px Gaussian blur, averaged over σ = 2 px.
//! - [`detail_ratio`]: a result's detail against a reference's (an HDR merge against its
//!   reference frame or the scene's truth) over chosen pixels. Below 1 the merge lost detail.
//! - [`detail_gain`]: the result's fine detail projected onto a known truth's: blur lowers it, noise
//!   doesn't count.
//! - [`residual`]: how far two aligned frames still are apart, per tile, in pixels (patches
//!   matched as in [`crate::refine`]).

use lightcraft_geom::Homography;
use lightcraft_raster::blur::gaussian;
use lightcraft_raster::{Plane, Rgb32f};
use rayon::prelude::*;

use crate::align::luma;
use crate::refine;

/// `img`'s √luminance minus its σ = 1 px blur.
fn high_pass(img: &Rgb32f) -> Plane {
    let y = img.map(|c| luma(c).max(0.0).sqrt());
    let b = gaussian(&y, 1.0);
    let mut d = y;
    d.data.par_iter_mut().zip(b.data.par_iter()).for_each(|(v, b)| *v -= b);
    d
}

/// Σ `f(a, b)` (two sums) over the pixels of `a` and `b` (same size) that `keep` accepts (`x`, `y`)
/// and where both are finite.
fn masked_sums(a: &Plane, b: &Plane, keep: impl Fn(usize, usize) -> bool + Sync, f: impl Fn(f64, f64) -> (f64, f64) + Sync) -> (f64, f64) {
    let w = a.width.max(1);
    a.data
        .par_chunks(w)
        .zip(b.data.par_chunks(w))
        .enumerate()
        .map(|(y, (ra, rb))| {
            let mut s = (0f64, 0f64);
            for (x, (&u, &v)) in ra.iter().zip(rb).enumerate() {
                if keep(x, y) && u.is_finite() && v.is_finite() {
                    let (p, q) = f(u as f64, v as f64);
                    s.0 += p;
                    s.1 += q;
                }
            }
            s
        })
        .reduce(|| (0.0, 0.0), |p, q| (p.0 + q.0, p.1 + q.1))
}

/// Local high-pass energy of `img`'s √luminance (see the module docs).
pub fn detail(img: &Rgb32f) -> Plane {
    let mut e = high_pass(img);
    e.data.par_iter_mut().for_each(|v| *v = if v.is_finite() { *v * *v } else { 0.0 });
    gaussian(&e, 2.0)
}

/// Σ detail(result) / Σ detail(reference) over the pixels `keep` accepts (`x`, `y`); `None`
/// when the sizes differ or the reference has no detail there.
pub fn detail_ratio(result: &Plane, reference: &Plane, keep: impl Fn(usize, usize) -> bool + Sync) -> Option<f64> {
    if result.width != reference.width || result.height != reference.height || result.width == 0 {
        return None;
    }
    let (a, b) = masked_sums(result, reference, keep, |a, b| (a, b));
    (b > 0.0).then(|| a / b)
}

/// How much of `truth`'s fine detail `result` carries: the result's high-pass (√luminance minus a
/// σ = 1 px blur) projected onto the truth's, Σ hp(result)·hp(truth) / Σ hp(truth)², over the pixels
/// `keep` accepts. 1 keeps the texture at full contrast; noise, uncorrelated with the truth, doesn't
/// count (unlike [`detail_ratio`]). `None` when the sizes differ or the truth has no detail there.
pub fn detail_gain(result: &Rgb32f, truth: &Rgb32f, keep: impl Fn(usize, usize) -> bool + Sync) -> Option<f64> {
    if result.width != truth.width || result.height != truth.height || result.width == 0 {
        return None;
    }
    let (rt, tt) = masked_sums(&high_pass(result), &high_pass(truth), keep, |r, t| (r * t, t * t));
    (tt > 0.0).then(|| rt / tt)
}

/// How far apart two aligned frames still are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Residual {
    /// Median and 95th percentile of the tiles' shifts (pixels).
    pub median: f64,
    pub p95: f64,
    /// Tiles that could be matched.
    pub tiles: usize,
}

/// The shift between `reference` and `frame` (same geometry; planes from [`refine::luma`]) at each
/// textured tile, within ±3 px; `None` when no tile matches.
pub fn residual(reference: &Plane, frame: &Plane) -> Option<Residual> {
    let sites = refine::sites(reference, 5);
    let mut d: Vec<f64> = sites
        .par_iter()
        .filter_map(|&s| refine::match_patch(reference, s, frame, &Homography::IDENTITY, 3))
        .map(|m| m.offset.x.hypot(m.offset.y))
        .collect();
    if d.is_empty() {
        return None;
    }
    d.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f64| d.get(((d.len() - 1) as f64 * q).round() as usize).copied().unwrap_or(0.0);
    Some(Residual { median: at(0.5), p95: at(0.95), tiles: d.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tex(w: usize, h: usize, blur: f32) -> Rgb32f {
        let img = Rgb32f::from_fn(w, h, |x, y| {
            let (x, y) = (x as f32, y as f32);
            [0.2 + 0.1 * (x * 0.9).sin() * (y * 0.7).cos() + 0.05 * (x * 0.31 + y * 0.43).sin(); 3]
        });
        if blur > 0.0 { gaussian(&img, blur) } else { img }
    }

    #[test]
    fn blur_lowers_the_detail_ratio() {
        let sharp = tex(200, 150, 0.0);
        let soft = tex(200, 150, 1.0);
        let r = detail_ratio(&detail(&soft), &detail(&sharp), |_, _| true).unwrap();
        assert!(r < 0.5, "{r}");
        assert!((detail_ratio(&detail(&sharp), &detail(&sharp), |_, _| true).unwrap() - 1.0).abs() < 1e-9);
        assert!(detail_ratio(&detail(&sharp), &Plane::new(3, 3), |_, _| true).is_none());
    }

    #[test]
    fn detail_gain_ignores_noise_and_sees_blur() {
        let sharp = tex(200, 150, 0.0);
        let mut noisy = sharp.clone();
        let mut rng = crate::ransac::Rng::new(7);
        for p in noisy.data.iter_mut() {
            let n = (rng.unit() as f32 - 0.5) * 0.05;
            *p = p.map(|v| v + n);
        }
        let g = detail_gain(&noisy, &sharp, |_, _| true).unwrap();
        assert!((g - 1.0).abs() < 0.05, "noise: {g}");
        assert!(detail_gain(&tex(200, 150, 1.0), &sharp, |_, _| true).unwrap() < 0.7, "an amplitude: blur that halves the energy leaves √½");
        assert!(detail_gain(&sharp, &Rgb32f::new(2, 2), |_, _| true).is_none());
    }

    #[test]
    fn residual_finds_a_known_shift() {
        let shifted = |dx: f32| {
            refine::luma(
                &Rgb32f::from_fn(600, 400, |x, y| {
                    let x = x as f32 + dx;
                    [0.1 + 0.05 * ((x * 0.37).sin() + (y as f32 * 0.23 + x * 0.05).cos()); 3]
                }),
                1.0,
                1.0,
            )
        };
        let (a, b) = (shifted(0.0), shifted(1.25));
        let r = residual(&a, &b).unwrap();
        assert!((r.median - 1.25).abs() < 0.1, "{r:?}");
        assert!(residual(&a, &a).unwrap().p95 < 0.05);
        assert!(residual(&Plane::new(10, 10), &Plane::new(10, 10)).is_none());
    }
}
