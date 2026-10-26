//! Residual vignetting, estimated from the overlaps.
//!
//! Frames arrive with the vignetting correction their files ask for (a DNG's lens-shading gain map,
//! a lens profile), but it rarely fits exactly: on DJI drone DNGs each frame came out a little
//! brighter towards its edges than at its centre. One photo hides that; in a panorama every seam
//! sits where two frames are both near their edges, and the sky shows a bright ridge at each.
//!
//! As PTGui and Hugin do (d'Angelo, "Radiometric alignment and vignetting calibration", 2007;
//! Goldman & Chen, "Vignette and exposure calibration", 2005), the falloff shared by all frames is
//! fitted from the pixels two frames both see. A frame records `I = gᵢ · V(r) · L` for scene
//! radiance `L` at normalised radius `r` from its centre (1 at the corners), so for a pixel seen at
//! radius `rₐ` in frame `i` and `r_b` in frame `j`:
//!
//! `ln Iₐ − ln I_b = (Gᵢ − Gⱼ) + v(rₐ) − v(r_b)`, with `G = ln g`, `v = ln V = c₁r² + c₂r⁴ + c₃r⁶`,
//!
//! linear in the unknowns: least squares over a sample of overlap pixels (neither clipped nor deep in
//! the shadows, where flare adds light rather than multiplying it), with weak priors, then again
//! without the samples more than three robust deviations off (a moving cloud, a misaligned edge).
//! Each frame is then divided by `V(r)`. A fit that would change a pixel by more than a factor of
//! [`MAX_FACTOR`], or rests on too few samples, is not used.

use rayon::prelude::*;

use super::Tile;
use crate::linalg;

/// The largest correction (either way) a fit may ask for anywhere in a frame.
pub(crate) const MAX_FACTOR: f64 = 1.6;
/// Overlap samples per frame pair (at most).
const PER_PAIR: usize = 6000;
/// Fewest samples a fit is made from.
const MIN_SAMPLES: usize = 600;

/// `ln V` at squared normalised radius `r2` for coefficients `c`.
pub(crate) fn log_v(c: &[f64; 3], r2: f64) -> f64 {
    r2 * (c[0] + r2 * (c[1] + r2 * c[2]))
}

/// The tile's squared normalised radius at pixel `k` (stored as 16 bits).
fn r2_of(t: &Tile, k: usize) -> f64 {
    t.r2.get(k).copied().unwrap_or(0) as f64 / u16::MAX as f64
}

/// A sample: frames `i`, `j`, their log luminance difference and their squared radii.
struct Sample {
    i: usize,
    j: usize,
    d: f64,
    ra: f64,
    rb: f64,
}

/// Fit the shared falloff from the overlaps; `clip[i]` is frame `i`'s saturation level. `None` when
/// the overlaps don't say enough, or the fit is implausible.
pub(crate) fn estimate(tiles: &[Option<Tile>], clip: &[f32]) -> Option<[f64; 3]> {
    let n = tiles.len();
    let lum = |p: [f32; 3]| 0.25 * p[0] as f64 + 0.6 * p[1] as f64 + 0.15 * p[2] as f64;
    let pairs: Vec<(usize, usize)> = (0..n).flat_map(|i| (i + 1..n).map(move |j| (i, j))).collect();
    let mut samples: Vec<Sample> = pairs
        .par_iter()
        .flat_map_iter(|&(i, j)| {
            let mut out = Vec::new();
            let (Some(a), Some(b)) = (tiles.get(i).and_then(Option::as_ref), tiles.get(j).and_then(Option::as_ref)) else { return out };
            let (x0, y0) = (a.x0.max(b.x0), a.y0.max(b.y0));
            let (x1, y1) = ((a.x0 + a.w).min(b.x0 + b.w), (a.y0 + a.h).min(b.y0 + b.h));
            if x1 <= x0 || y1 <= y0 {
                return out;
            }
            let area = (x1 - x0) * (y1 - y0);
            let step = ((area / PER_PAIR) as f64).sqrt().ceil().max(1.0) as usize;
            let (ca, cb) = (clip.get(i).copied().unwrap_or(f32::INFINITY), clip.get(j).copied().unwrap_or(f32::INFINITY));
            let mut raw = Vec::new();
            for y in (y0..y1).step_by(step) {
                for x in (x0..x1).step_by(step) {
                    let ka = (y - a.y0) * a.w + x - a.x0;
                    let kb = (y - b.y0) * b.w + x - b.x0;
                    let (Some(&wa), Some(&wb), Some(&pa), Some(&pb)) = (a.wgt.get(ka), b.wgt.get(kb), a.rgb.get(ka), b.rgb.get(kb)) else { continue };
                    if wa <= 0.0 || wb <= 0.0 || pa.iter().any(|v| *v >= ca * 0.95) || pb.iter().any(|v| *v >= cb * 0.95) {
                        continue;
                    }
                    let (la, lb) = (lum(pa), lum(pb));
                    if la > 0.0 && lb > 0.0 && la.is_finite() && lb.is_finite() {
                        raw.push((la, lb, r2_of(a, ka), r2_of(b, kb)));
                    }
                }
            }
            // away from the shadows, where flare adds light instead of multiplying it
            let mut ls: Vec<f64> = raw.iter().map(|s| s.0.min(s.1)).collect();
            ls.sort_by(|p, q| p.total_cmp(q));
            let floor = ls.get(ls.len() / 4).copied().unwrap_or(0.0);
            for (la, lb, ra, rb) in raw {
                if la.min(lb) >= floor {
                    out.push(Sample { i, j, d: la.ln() - lb.ln(), ra, rb });
                }
            }
            out
        })
        .collect();
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let mut fit = solve(&samples, n)?;
    // without the outliers
    let res: Vec<f64> = samples.iter().map(|s| residual(s, &fit, n)).collect();
    let mut abs: Vec<f64> = res.iter().map(|r| r.abs()).collect();
    abs.sort_by(|p, q| p.total_cmp(q));
    let mad = abs.get(abs.len() / 2).copied().unwrap_or(0.0) * 1.4826;
    let limit = (3.0 * mad).max(0.02);
    let mut k = 0;
    samples.retain(|_| {
        let keep = res.get(k).is_some_and(|r| r.abs() <= limit);
        k += 1;
        keep
    });
    if samples.len() >= MIN_SAMPLES {
        fit = solve(&samples, n)?;
    }
    let c = [fit[n], fit[n + 1], fit[n + 2]];
    let worst = (0..=20).map(|s| log_v(&c, s as f64 / 20.0).abs()).fold(0.0, f64::max);
    (c.iter().all(|v| v.is_finite()) && worst <= MAX_FACTOR.ln()).then_some(c)
}

fn residual(s: &Sample, x: &[f64], n: usize) -> f64 {
    let c = [x[n], x[n + 1], x[n + 2]];
    s.d - (x[s.i] - x[s.j]) - (log_v(&c, s.ra) - log_v(&c, s.rb))
}

/// Least squares over the samples; unknowns `G₀…Gₙ₋₁, c₁, c₂, c₃`.
fn solve(samples: &[Sample], n: usize) -> Option<Vec<f64>> {
    let dim = n + 3;
    let mut ata = vec![0f64; dim * dim];
    let mut atb = vec![0f64; dim];
    let mut add = |coef: &[(usize, f64)], rhs: f64, w: f64| {
        for &(r, cr) in coef {
            for &(c, cc) in coef {
                ata[r * dim + c] += w * cr * cc;
            }
            atb[r] += w * cr * rhs;
        }
    };
    for s in samples {
        let (pa, pb) = ([s.ra, s.ra * s.ra, s.ra * s.ra * s.ra], [s.rb, s.rb * s.rb, s.rb * s.rb * s.rb]);
        add(&[(s.i, 1.0), (s.j, -1.0), (n, pa[0] - pb[0]), (n + 1, pa[1] - pb[1]), (n + 2, pa[2] - pb[2])], s.d, 1.0);
    }
    // weak priors: gains towards 1 (only the overall level is free), the falloff towards none
    let w = samples.len() as f64 * 1e-4 + 1e-6;
    for i in 0..n {
        add(&[(i, 1.0)], 0.0, w);
    }
    for k in 0..3 {
        add(&[(n + k, 1.0)], 0.0, w);
    }
    linalg::solve(ata, atb, dim).filter(|x| x.iter().all(|v| v.is_finite()))
}

/// The factor [`correct`] multiplies a pixel by at a tile's stored squared radius `r` (× 65535).
pub(crate) fn factor(c: &[f64; 3], r: u16) -> f32 {
    (-log_v(c, r as f64 / u16::MAX as f64)).exp() as f32
}

/// Divide every tile by the falloff `V(r)` with coefficients `c`.
pub(crate) fn correct(tiles: &mut [Option<Tile>], c: &[f64; 3]) {
    for t in tiles.iter_mut().flatten() {
        let r2 = std::mem::take(&mut t.r2);
        t.rgb.par_iter_mut().zip(r2.par_iter()).for_each(|(p, &r)| {
            let f = factor(c, r);
            *p = p.map(|v| v * f);
        });
        t.r2 = r2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two frames of a smooth sky, each darkened by the same falloff, overlapping by a third: the fit
    /// finds the falloff, and dividing it out leaves the frames agreeing.
    #[test]
    fn finds_a_shared_falloff() {
        let truth = [-0.35, 0.1, -0.05];
        let (fw, fh) = (300usize, 200usize);
        let sky = |x: f64, y: f64| 0.3 * (1.0 + 0.3 * (x / 500.0)) * (1.0 + 0.1 * (y / 200.0));
        let frame = |x0: usize, g: f64| {
            let mut rgb = Vec::with_capacity(fw * fh);
            let mut r2 = Vec::with_capacity(fw * fh);
            for y in 0..fh {
                for x in 0..fw {
                    let (dx, dy) = ((x as f64 + 0.5) / fw as f64 * 2.0 - 1.0, (y as f64 + 0.5) / fh as f64 * 2.0 - 1.0);
                    let rr = (dx * dx * (fw * fw) as f64 + dy * dy * (fh * fh) as f64) / ((fw * fw + fh * fh) as f64);
                    let v = (g * sky((x0 + x) as f64, y as f64) * log_v(&truth, rr).exp()) as f32;
                    rgb.push([v; 3]);
                    r2.push((rr * u16::MAX as f64).round() as u16);
                }
            }
            Tile { x0, y0: 0, w: fw, h: fh, rgb, wgt: vec![1.0; fw * fh], r2 }
        };
        let mut tiles = vec![Some(frame(0, 1.0)), Some(frame(200, 1.25))];
        let c = estimate(&tiles, &[f32::INFINITY; 2]).expect("a fit");
        for s in [0.25, 0.5, 1.0] {
            assert!((log_v(&c, s) - log_v(&truth, s)).abs() < 0.02, "{c:?} at r² {s}");
        }
        correct(&mut tiles, &c);
        // in the overlap the frames now differ by their gain only
        let (a, b) = (tiles[0].as_ref().unwrap(), tiles[1].as_ref().unwrap());
        let ratio = |x: usize, y: usize| b.rgb[y * fw + x - 200][0] / a.rgb[y * fw + x][0];
        let (r1, r2) = (ratio(205, 10), ratio(295, 190));
        assert!((r1 - r2).abs() < 0.02, "{r1} {r2}");
    }

    #[test]
    fn no_fit_without_overlap() {
        let t = |x0| Tile { x0, y0: 0, w: 10, h: 10, rgb: vec![[0.5; 3]; 100], wgt: vec![1.0; 100], r2: vec![0; 100] };
        assert!(estimate(&[Some(t(0)), Some(t(50))], &[1.0; 2]).is_none());
        assert!(estimate(&[], &[]).is_none());
    }
}
