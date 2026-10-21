//! Full-resolution refinement of an alignment found on small versions of the frames.
//!
//! [`crate::align`] registers frames on versions of at most [`crate::align::WORK_EDGE`] pixels.
//! For a 24–60 MP photo that is 5–8× smaller, and a tenth of a work pixel off is most of a full
//! pixel off: merged frames that far apart blur the result. Here the motion is fitted again at
//! full size:
//!
//! 1. **Sites.** The reference is cut into a grid of cells (≈ [`CELLS`] along the long edge); in
//!    each, the [`PATCH`]-pixel square with the most two-dimensional texture (the smaller
//!    eigenvalue of its gradient structure tensor, so a straight edge, which only fixes one
//!    direction, doesn't count) is a site.
//! 2. **Matching.** Each site's patch is compared with the frame resampled through the current
//!    alignment, by normalised cross-correlation (immune to the frames' different brightness and
//!    contrast) over integer offsets within a search radius, counting only pixels usable in both
//!    (clipped ones aren't); the peak is refined to a fraction of a pixel by a parabola through its
//!    neighbours on each axis. Patches less than half usable, with too little texture, a weak peak, a peak on the search border or a second peak almost
//!    as strong (repeating texture) are dropped.
//! 3. **Fit.** The matches are fitted with RANSAC at [`THRESH`] pixels, choosing the simplest of
//!    translation / similarity / homography that explains (nearly) as many as the most general
//!    one, as [`crate::align::align_pair`] does. A second, narrower pass starts from that fit.
//!
//! When too few patches match, or the fit isn't a plausible handheld motion, the alignment found
//! on the small versions stays: refining never makes an alignment worse than not refining.

use lightcraft_geom::{Homography, Point};
use lightcraft_raster::{Plane, Rgb32f};
use rayon::prelude::*;

use crate::align::{Alignment, Models, choose_model, luma as lum};
use crate::ransac::Model;

/// Side of a matched patch (pixels).
pub const PATCH: usize = 48;
/// Cells along the long edge (sites are picked one per cell).
pub const CELLS: usize = 24;
/// RANSAC inlier threshold at full resolution (pixels).
pub const THRESH: f64 = 0.75;
/// Weakest correlation peak a match may have.
const MIN_SCORE: f32 = 0.8;
/// The best peak away from the best one must be at least this much weaker.
const UNIQUE: f32 = 0.03;
/// Least standard deviation (of the gamma-encoded luminance) a patch needs to be matched.
const MIN_STD: f32 = 0.01;
/// Fewest matches a fit is made from.
const MIN_MATCHES: usize = 12;
/// Share of the matches the fit must explain.
const MIN_SHARE: f64 = 0.4;

/// A frame's luminance for matching: `(luminance · gain)^(1/2.2)` at full size, NaN where the
/// frame is clipped (any channel at or above 95 % of `clip`) or `gain` takes it past 1, so such
/// pixels never take part. `gain` brings frames of a bracket to the same brightness.
pub fn luma(img: &Rgb32f, gain: f32, clip: f32) -> Plane {
    let g = if gain.is_finite() && gain > 0.0 { gain } else { 1.0 };
    img.map(|c| {
        if c[0].max(c[1]).max(c[2]) >= 0.95 * clip {
            return f32::NAN;
        }
        let y = lum(c) * g;
        if y.is_finite() && y < 1.0 { y.max(0.0).powf(1.0 / 2.2) } else { f32::NAN }
    })
}

/// The search radius (full-resolution pixels) for an alignment found at `scale` (work pixels per
/// full pixel): about two work pixels.
pub fn radius_for(scale: f64) -> usize {
    if scale.is_finite() && scale > 0.0 { ((2.0 / scale).ceil() as usize + 1).clamp(3, 16) } else { 3 }
}

/// A patch of the reference found in a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Match {
    /// Centre of the patch in the reference (pixel centres at +0.5).
    pub reference: Point,
    /// Where it is in the frame.
    pub frame: Point,
    /// The offset from where the alignment put it, in reference pixels.
    pub offset: Point,
    /// Correlation at the peak (−1..1).
    pub score: f32,
}

/// Top-left corners of the patches to match: one per cell, where the reference has the most
/// two-dimensional texture, `margin` pixels clear of the edges.
pub fn sites(reference: &Plane, margin: usize) -> Vec<(usize, usize)> {
    let (w, h) = (reference.width, reference.height);
    let need = PATCH + 2 * margin;
    if w < need || h < need {
        return Vec::new();
    }
    let long = w.max(h);
    let cell = long.div_ceil(CELLS).max(PATCH);
    let (nx, ny) = ((w - 2 * margin) / cell, (h - 2 * margin) / cell);
    let (nx, ny) = (nx.max(1), ny.max(1));
    let (cw, chh) = ((w - 2 * margin) / nx, (h - 2 * margin) / ny);
    (0..nx * ny)
        .into_par_iter()
        .filter_map(|c| {
            let (cx, cy) = (margin + (c % nx) * cw, margin + (c / nx) * chh);
            let span_x = cw.saturating_sub(PATCH);
            let span_y = chh.saturating_sub(PATCH);
            let mut best: Option<((usize, usize), f32)> = None;
            for j in 0..3 {
                for i in 0..3 {
                    let (x, y) = (cx + span_x * i / 2, cy + span_y * j / 2);
                    if x + PATCH > w - margin || y + PATCH > h - margin {
                        continue;
                    }
                    if let Some(t) = texture(reference, x, y)
                        && best.is_none_or(|(_, b)| t > b)
                    {
                        best = Some(((x, y), t));
                    }
                }
            }
            best.filter(|(_, t)| *t > 1e-6).map(|(p, _)| p)
        })
        .collect()
}

/// The smaller eigenvalue of the patch's mean gradient structure tensor, over its usable pixels;
/// `None` when fewer than half of them are.
fn texture(p: &Plane, x0: usize, y0: usize) -> Option<f32> {
    let w = p.width;
    let at = |x: usize, y: usize| p.data.get(y * w + x).copied().unwrap_or(f32::NAN);
    let (mut sxx, mut syy, mut sxy, mut n) = (0f64, 0f64, 0f64, 0usize);
    for y in y0 + 1..y0 + PATCH - 1 {
        for x in x0 + 1..x0 + PATCH - 1 {
            let gx = (at(x + 1, y) - at(x - 1, y)) as f64;
            let gy = (at(x, y + 1) - at(x, y - 1)) as f64;
            if gx.is_finite() && gy.is_finite() {
                sxx += gx * gx;
                syy += gy * gy;
                sxy += gx * gy;
                n += 1;
            }
        }
    }
    if n * 2 < (PATCH - 2) * (PATCH - 2) {
        return None;
    }
    let n = n as f64;
    let (a, b, c) = (sxx / n, syy / n, sxy / n);
    let tr = a + b;
    let det = a * b - c * c;
    Some((tr / 2.0 - ((tr * tr / 4.0 - det).max(0.0)).sqrt()) as f32)
}

/// Find the reference patch at `site` (top-left) in `frame`, which `h` maps reference pixels into,
/// searching offsets up to `radius` pixels.
pub fn match_patch(reference: &Plane, site: (usize, usize), frame: &Plane, h: &Homography, radius: usize) -> Option<Match> {
    let (sx, sy) = site;
    let (rw, rh) = (reference.width, reference.height);
    if sx + PATCH > rw || sy + PATCH > rh || frame.width == 0 || frame.height == 0 {
        return None;
    }
    let r = radius.max(1);
    let s = PATCH + 2 * r;
    let mut pr: Vec<f32> = Vec::with_capacity(PATCH * PATCH);
    for y in sy..sy + PATCH {
        for x in sx..sx + PATCH {
            pr.push(reference.data.get(y * rw + x).copied().unwrap_or(f32::NAN));
        }
    }
    // the frame around the patch, through `h`; NaN where it can't be used
    let (fw, fh) = (frame.width as f64, frame.height as f64);
    let mut win = vec![f32::NAN; s * s];
    for j in 0..s {
        for i in 0..s {
            let q = h.apply(Point::new((sx + i) as f64 - r as f64 + 0.5, (sy + j) as f64 - r as f64 + 0.5));
            if !(q.x >= 0.5 && q.y >= 0.5 && q.x <= fw - 0.5 && q.y <= fh - 0.5) {
                return None;
            }
            win[j * s + i] = crate::features::bilinear(frame, q.x as f32, q.y as f32);
        }
    }
    let d = 2 * r + 1;
    // offsets that can't be scored (too few usable pixels) stay at −∞
    let mut score = vec![f32::NEG_INFINITY; d * d];
    for dy in 0..d {
        for dx in 0..d {
            if let Some(c) = correlation(&pr, |u, v| win.get((v + dy) * s + dx + u).copied().unwrap_or(f32::NAN)) {
                score[dy * d + dx] = c;
            }
        }
    }
    let (bi, &best) = score.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let (bx, by) = (bi % d, bi / d);
    if best < MIN_SCORE || bx == 0 || by == 0 || bx == d - 1 || by == d - 1 {
        return None;
    }
    // a second peak nearly as strong: repeating texture, the match is ambiguous
    let second =
        score.iter().enumerate().filter(|(k, _)| (k % d).abs_diff(bx) > 1 || (k / d).abs_diff(by) > 1).map(|(_, v)| *v).fold(-1f32, f32::max);
    if second > best - UNIQUE {
        return None;
    }
    let at = |x: usize, y: usize| score[y * d + x];
    // the sub-pixel parabola needs the peak's four neighbours: one that couldn't be scored would pull it away
    if [at(bx - 1, by), at(bx + 1, by), at(bx, by - 1), at(bx, by + 1)].iter().any(|v| !v.is_finite()) {
        return None;
    }
    let sub = |m: f32, c: f32, p: f32| {
        let den = m - 2.0 * c + p;
        if den < -1e-9 { (0.5 * (m - p) / den).clamp(-0.5, 0.5) } else { 0.0 }
    };
    let ox = bx as f64 - r as f64 + sub(at(bx - 1, by), best, at(bx + 1, by)) as f64;
    let oy = by as f64 - r as f64 + sub(at(bx, by - 1), best, at(bx, by + 1)) as f64;
    let c = Point::new(sx as f64 + PATCH as f64 / 2.0, sy as f64 + PATCH as f64 / 2.0);
    Some(Match { reference: c, frame: h.apply(Point::new(c.x + ox, c.y + oy)), offset: Point::new(ox, oy), score: best })
}

/// Correlation of the reference patch `a` (row-major, [`PATCH`] wide) with `b(u, v)` over the
/// pixels usable in both; `None` with fewer than half of the patch usable, or too little texture in
/// either.
fn correlation(a: &[f32], b: impl Fn(usize, usize) -> f32) -> Option<f32> {
    let (mut n, mut sa, mut sb, mut saa, mut sbb, mut sab) = (0usize, 0f64, 0f64, 0f64, 0f64, 0f64);
    for v in 0..PATCH {
        for u in 0..PATCH {
            let (x, y) = (a.get(v * PATCH + u).copied().unwrap_or(f32::NAN), b(u, v));
            if x.is_finite() && y.is_finite() {
                let (x, y) = (x as f64, y as f64);
                n += 1;
                sa += x;
                sb += y;
                saa += x * x;
                sbb += y * y;
                sab += x * y;
            }
        }
    }
    if n * 2 < PATCH * PATCH {
        return None;
    }
    let nf = n as f64;
    let (ma, mb) = (sa / nf, sb / nf);
    let (va, vb) = (saa / nf - ma * ma, sbb / nf - mb * mb);
    let min = (MIN_STD as f64).powi(2);
    if !(va >= min && vb >= min * 0.25) {
        return None;
    }
    let c = (sab / nf - ma * mb) / (va * vb).sqrt();
    c.is_finite().then_some(c as f32)
}

/// Translation, similarity and homography: what a handheld frame may need.
pub const ALL_MODELS: [(Model, &str, usize); 3] =
    [(Model::Translation, "translation", 6), (Model::Similarity, "similarity", 8), (Model::Homography, "homography", 12)];

/// Similarity only (a focus stack: focusing magnifies, a tripod adds nothing else, and a homography fitted to the
/// sharp slice of a frame extrapolates badly).
pub const SIMILARITY: [(Model, &str, usize); 1] = [(Model::Similarity, "similarity", 8)];

/// Search radius (pixels) for a frame with no alignment to start from (the small versions found
/// none: a bright frame mostly clipped, too little texture left for features).
pub const RECOVER_RADIUS: usize = 16;

/// Fit `models` (reference → frame, full resolution) to patches of `reference` matched in `frame`
/// around `start` within `radius` pixels, then again within 2 px of that fit; both planes from
/// [`luma`]. `start` is an alignment found on small versions (`radius` from [`radius_for`]), or the
/// identity with [`RECOVER_RADIUS`]. `None` when too few patches match or no fit is a plausible
/// handheld motion: the caller keeps what it had.
pub fn fit(reference: &Plane, frame: &Plane, start: &Homography, radius: usize, seed: u64, models: &Models) -> Option<Alignment> {
    let (w, ht) = (reference.width as f64, reference.height as f64);
    let r0 = radius.max(2);
    // the sites for the widest search fit the narrower one too
    let s = sites(reference, r0 + 2);
    let mut cur: Option<Alignment> = None;
    for r in [r0, 2] {
        let h = cur.as_ref().map_or(*start, |a| a.h);
        let m: Vec<Match> = s.par_iter().filter_map(|&p| match_patch(reference, p, frame, &h, r)).collect();
        if m.len() < MIN_MATCHES {
            break;
        }
        let pa: Vec<Point> = m.iter().map(|m| m.reference).collect();
        let pb: Vec<Point> = m.iter().map(|m| m.frame).collect();
        let Some((name, f)) = choose_model(models, &pa, &pb, THRESH, MIN_SHARE, (w, ht), seed) else { break };
        cur = Some(Alignment { h: f.h, model: name, inliers: f.inliers.len(), rms: f.rms });
    }
    cur
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A smooth random texture, sampled anywhere.
    pub(crate) fn tex(x: f64, y: f64) -> f32 {
        let mut v = 0.45;
        for k in 0..9 {
            let a = k as f64 * 2.399;
            let f = 0.02 + 0.025 * k as f64;
            v += 0.05 * ((x * a.cos() + y * a.sin()) * f * std::f64::consts::TAU + k as f64).sin();
        }
        v as f32
    }

    fn plane(w: usize, h: usize, map: &Homography) -> Plane {
        Plane::from_fn(w, h, |x, y| {
            let p = map.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
            tex(p.x, p.y)
        })
    }

    fn corner_error(a: &Homography, b: &Homography, w: f64, h: f64) -> f64 {
        [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].iter().map(|&(x, y)| a.apply(Point::new(x, y)).dist(b.apply(Point::new(x, y)))).fold(0.0, f64::max)
    }

    /// A frame rotated 0.2° and moved 1.37 px, given an alignment 2 px off: refined to a tenth of
    /// a pixel at the corners.
    #[test]
    fn refines_a_rough_alignment_to_a_fraction_of_a_pixel() {
        let (w, h) = (900usize, 600usize);
        let reference = plane(w, h, &Homography::IDENTITY);
        // the frame's pixel q shows the scene at s(q); reference → frame is s⁻¹
        let (c, sn) = (0.2f64.to_radians().cos(), 0.2f64.to_radians().sin());
        let s = Homography([c, -sn, 1.37, sn, c, -0.83, 0.0, 0.0, 1.0]);
        let truth = s.inverse().unwrap();
        let frame = plane(w, h, &s);
        let rough = Homography([truth.0[0], truth.0[1], truth.0[2] + 1.6, truth.0[3], truth.0[4], truth.0[5] - 1.2, 0.0, 0.0, 1.0]);
        assert!(corner_error(&rough, &truth, w as f64, h as f64) > 1.5);
        let r = fit(&reference, &frame, &rough, 4, 1, &ALL_MODELS).unwrap();
        let e = corner_error(&r.h, &truth, w as f64, h as f64);
        assert!(e < 0.1, "corner error {e} px, model {}", r.model);
        assert!(r.inliers >= MIN_MATCHES);
    }

    /// A frame the small versions couldn't align, 5 px and 0.1° off: found from nothing.
    #[test]
    fn recovers_a_frame_with_no_alignment() {
        let (w, h) = (900usize, 600usize);
        let reference = plane(w, h, &Homography::IDENTITY);
        let (c, sn) = (0.1f64.to_radians().cos(), 0.1f64.to_radians().sin());
        let s = Homography([c, -sn, -4.6, sn, c, 2.1, 0.0, 0.0, 1.0]);
        let r = fit(&reference, &plane(w, h, &s), &Homography::IDENTITY, RECOVER_RADIUS, 1, &ALL_MODELS).unwrap();
        let e = corner_error(&r.h, &s.inverse().unwrap(), w as f64, h as f64);
        assert!(e < 0.1, "corner error {e} px, model {}", r.model);
        assert!(fit(&reference, &Plane::from_fn(w, h, |_, _| 0.5), &Homography::IDENTITY, RECOVER_RADIUS, 1, &ALL_MODELS).is_none());
    }

    /// Nothing to match (a flat frame, planes too small for a patch, empty ones): no fit.
    #[test]
    fn no_fit_when_nothing_matches() {
        let h = Homography([1.0, 0.0, 0.7, 0.0, 1.0, 0.2, 0.0, 0.0, 1.0]);
        let flat = Plane::from_fn(400, 300, |_, _| 0.5);
        assert!(fit(&flat, &flat, &h, 4, 1, &ALL_MODELS).is_none());
        let tiny = Plane::from_fn(20, 20, |x, _| x as f32);
        assert!(fit(&tiny, &tiny, &h, 4, 1, &ALL_MODELS).is_none());
        assert!(fit(&Plane::new(0, 0), &Plane::new(0, 0), &h, 4, 1, &ALL_MODELS).is_none());
    }

    #[test]
    fn clipped_pixels_and_odd_gains_are_left_out() {
        let img = Rgb32f::from_fn(4, 1, |x, _| [x as f32 * 0.33; 3]);
        let l = luma(&img, 1.0, 1.0);
        assert!(l.data[0] == 0.0 && l.data[3].is_nan(), "{:?}", l.data);
        let l = luma(&img, 4.0, 1.0);
        assert!(l.data[1].is_nan(), "0.33 × 4 is past 1: {:?}", l.data);
        assert!(luma(&img, f32::NAN, 1.0).data[1].is_finite());
        assert_eq!(radius_for(0.125), 16);
        assert_eq!(radius_for(1.0), 3);
        assert_eq!(radius_for(f64::NAN), 3);
    }
}
