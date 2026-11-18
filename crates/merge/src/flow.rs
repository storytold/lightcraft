//! Local alignment: what one motion model for the whole frame leaves.
//!
//! A homography is exact only for a camera that turned about its lens. Handheld, the camera also
//! moves, and things at different distances shift differently (parallax): on a real bracket (Sony,
//! 33 MP, gravel a few metres away and a building a kilometre off) the fit, dominated by the
//! foreground's texture, left the building 2.3 px off, and the merge doubled its inscription. Lens
//! distortion and rolling shutter leave local shifts too.
//!
//! After the global fit ([`crate::align`], [`crate::refine`]) the frame is compared with the
//! reference tile by tile:
//!
//! 1. **Measure.** On a grid of about [`GRID`] tiles along the long edge, the reference's patch at
//!    the tile's centre is found in the frame, through the global fit, within [`RADIUS`] pixels
//!    ([`crate::refine::match_patch`]: masked correlation, sub-pixel peak, ambiguous and textureless
//!    patches dropped).
//! 2. **Clean.** A shift more than [`OUTLIER`] px from the median of its measured neighbours is
//!    dropped (a person walking, a mismatch), as is one with no measured neighbour to check it by. Tiles without a measurement take the mean of their
//!    measured neighbours, spreading outwards until every tile has one.
//! 3. **Smooth** with two 3 × 3 box passes, so the field bends gently.
//! 4. **Warp.** Each output pixel `p` samples the frame at `h(p + flow(p))` (the flow interpolated
//!    bilinearly between tile centres): one Lanczos-3 resampling for both corrections.

use lightcraft_geom::{Homography, Point};
use lightcraft_raster::{Plane, Rgb32f};
use rayon::prelude::*;

use crate::refine::{self, PATCH};

/// Tiles along the long edge.
pub const GRID: usize = 96;
/// Largest local shift measured (pixels); farther ones are filled from neighbours.
pub const RADIUS: usize = 5;
/// A tile this far (pixels) from the median of its neighbours is an outlier.
pub const OUTLIER: f32 = 0.75;

/// A smooth field of local shifts in reference pixels: one value per tile, tile `(i, j)`'s at
/// `((i + ½)·step, (j + ½)·step)`, so pixel centres of `dx` / `dy` line up with the tiles'.
#[derive(Clone, Debug, PartialEq)]
pub struct Flow {
    pub step: usize,
    pub dx: Plane,
    pub dy: Plane,
    /// Tiles that were measured (before filling).
    pub measured: usize,
}

impl Default for Flow {
    /// No shift anywhere.
    fn default() -> Self {
        Flow { step: 0, dx: Plane::new(0, 0), dy: Plane::new(0, 0), measured: 0 }
    }
}

impl Flow {
    /// The shift at reference coordinates (pixel centres at +0.5), bilinear between tile centres;
    /// zero for an empty flow.
    pub fn at(&self, x: f64, y: f64) -> (f64, f64) {
        let (fx, fy) = ((x / self.step as f64) as f32, (y / self.step as f64) as f32);
        if self.dx.data.is_empty() || !fx.is_finite() || !fy.is_finite() {
            return (0.0, 0.0);
        }
        (crate::features::bilinear(&self.dx, fx, fy) as f64, crate::features::bilinear(&self.dy, fx, fy) as f64)
    }

    /// The largest shift in the field (pixels).
    pub fn max(&self) -> f32 {
        self.dx.data.iter().zip(&self.dy.data).map(|(x, y)| x.hypot(*y)).fold(0.0, f32::max)
    }
}

/// The local shifts between `reference` and `frame` (both from [`refine::luma`]) once `h` (reference
/// → frame pixels) is applied, within `radius` pixels.
pub fn estimate(reference: &Plane, frame: &Plane, h: &Homography, radius: usize) -> Flow {
    let (w, ht) = (reference.width, reference.height);
    // an even step, so a patch centred on a tile has a whole-pixel corner
    let step = ((w.max(ht) / GRID).max(PATCH) + 1) & !1;
    let (gw, gh) = (w / step, ht / step);
    if gw == 0 || gh == 0 || frame.data.is_empty() {
        return Flow::default();
    }
    let r = radius.max(1);
    let raw: Vec<Option<[f32; 2]>> = (0..gw * gh)
        .into_par_iter()
        .map(|t| {
            let (i, j) = (t % gw, t / gw);
            let (sx, sy) = (i * step + (step - PATCH) / 2, j * step + (step - PATCH) / 2);
            if sx < r + 1 || sy < r + 1 || sx + PATCH + r + 1 > w || sy + PATCH + r + 1 > ht {
                return None;
            }
            refine::match_patch(reference, (sx, sy), frame, h, r).map(|m| [m.offset.x as f32, m.offset.y as f32])
        })
        .collect();
    let measured = raw.iter().filter(|v| v.is_some()).count();
    let at = |d: &[Option<[f32; 2]>], k: usize| d.get(k).copied().flatten();
    // 2. outliers against the median of their measured neighbours
    let mut d: Vec<Option<[f32; 2]>> = (0..gw * gh)
        .map(|t| {
            let v = at(&raw, t)?;
            let n = neighbours(|k| at(&raw, k), gw, gh, t);
            let med = [median(n.iter().map(|v| v[0]).collect()), median(n.iter().map(|v| v[1]).collect())];
            // a tile with no measured neighbour can't be checked, and the fill would spread it: dropped
            (!n.is_empty() && (v[0] - med[0]).hypot(v[1] - med[1]) <= OUTLIER).then_some(v)
        })
        .collect();
    //    fill outwards from measured tiles
    for _ in 0..gw + gh {
        if d.iter().all(Option::is_some) || d.iter().all(Option::is_none) {
            break;
        }
        let prev = d.clone();
        for (t, slot) in d.iter_mut().enumerate().filter(|(_, s)| s.is_none()) {
            *slot = mean(&neighbours(|k| at(&prev, k), gw, gh, t));
        }
    }
    let mut f: Vec<[f32; 2]> = d.into_iter().map(|v| v.unwrap_or([0.0; 2])).collect();
    // 3. smooth (each tile with its neighbours)
    for _ in 0..2 {
        let prev = f.clone();
        for (t, v) in f.iter_mut().enumerate() {
            let mut n = neighbours(|k| prev.get(k).copied(), gw, gh, t);
            n.push(*v);
            *v = mean(&n).unwrap_or(*v);
        }
    }
    let plane = |c: usize| Plane::from_fn(gw, gh, |i, j| f.get(j * gw + i).map_or(0.0, |v| v[c]));
    Flow { step, dx: plane(0), dy: plane(1), measured }
}

/// The values `get` has among the 8 neighbours of tile `t`.
fn neighbours(get: impl Fn(usize) -> Option<[f32; 2]>, gw: usize, gh: usize, t: usize) -> Vec<[f32; 2]> {
    let (i, j) = (t % gw, t / gw);
    let mut out = Vec::with_capacity(8);
    for b in j.saturating_sub(1)..=(j + 1).min(gh - 1) {
        for a in i.saturating_sub(1)..=(i + 1).min(gw - 1) {
            if (a, b) != (i, j)
                && let Some(v) = get(b * gw + a)
            {
                out.push(v);
            }
        }
    }
    out
}

fn mean(n: &[[f32; 2]]) -> Option<[f32; 2]> {
    let k = n.len() as f32;
    (!n.is_empty()).then(|| [n.iter().map(|v| v[0]).sum::<f32>() / k, n.iter().map(|v| v[1]).sum::<f32>() / k])
}

fn median(mut v: Vec<f32>) -> f32 {
    v.sort_by(|a, b| a.total_cmp(b));
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// `img` resampled into the reference's geometry through `h` after the local shifts:
/// `out(p) = img(h(p + flow(p)))` (Lanczos-3), and where that falls inside `img`.
pub fn warp(img: &Rgb32f, h: &Homography, flow: &Flow) -> (Rgb32f, Vec<bool>) {
    let (w, ht) = (img.width, img.height);
    let mut out = Rgb32f::new(w, ht);
    let mut valid = vec![false; w * ht];
    if w == 0 || ht == 0 {
        return (out, valid);
    }
    out.data.par_chunks_mut(w).zip(valid.par_chunks_mut(w)).enumerate().for_each(|(y, (row, vrow))| {
        for x in 0..w {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let (dx, dy) = flow.at(px, py);
            let q = h.apply(Point::new(px + dx, py + dy));
            let (sx, sy) = (q.x as f32, q.y as f32);
            if sx >= 0.0 && sy >= 0.0 && sx <= w as f32 && sy <= ht as f32 {
                row[x] = img.sample_lanczos3(sx, sy);
                vrow[x] = true;
            }
        }
    });
    (out, valid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refine::tests::tex;

    /// Parallax: the top of the frame (far) shifted 2.2 px against the bottom (near), with a
    /// smooth band between. The flow finds both, and the warp lines the frame up everywhere.
    #[test]
    fn finds_and_undoes_a_local_shift() {
        let (w, h) = (1200usize, 800usize);
        let shift = |y: f64| {
            let t = ((y - 300.0) / 200.0).clamp(0.0, 1.0);
            let s = t * t * (3.0 - 2.0 * t);
            ((1.0 - s) * 2.2, (1.0 - s) * -1.1)
        };
        let reference = Plane::from_fn(w, h, |x, y| tex(x as f64 + 0.5, y as f64 + 0.5));
        // the frame's pixel q shows the scene at q + shift(q.y)
        let frame = Rgb32f::from_fn(w, h, |x, y| {
            let (sx, sy) = shift(y as f64 + 0.5);
            [tex(x as f64 + 0.5 + sx, y as f64 + 0.5 + sy); 3]
        });
        let fl = refine::luma(&frame, 1.0, 10.0);
        let flow = estimate(&reference, &fl, &Homography::IDENTITY, RADIUS);
        let tiles = flow.dx.width * flow.dx.height;
        assert!(flow.measured > tiles / 2, "{} of {tiles}", flow.measured);
        // reference p matches frame p + d with d = −shift
        let (tx, ty) = flow.at(600.0, 100.0);
        assert!((tx + 2.2).abs() < 0.15 && (ty - 1.1).abs() < 0.15, "top {tx} {ty}");
        let (bx, by) = flow.at(600.0, 700.0);
        assert!(bx.abs() < 0.15 && by.abs() < 0.15, "bottom {bx} {by}");
        let (out, valid) = warp(&frame, &Homography::IDENTITY, &flow);
        let error = |img: &Rgb32f| {
            let (mut e, mut n) = (0f64, 0usize);
            for y in (40..h - 40).step_by(3) {
                for x in (40..w - 40).step_by(3) {
                    if valid[y * w + x] {
                        e += (img.get(x, y)[0] - reference.get(x, y)).abs() as f64;
                        n += 1;
                    }
                }
            }
            e / n as f64
        };
        let (before, after) = (error(&frame), error(&out));
        assert!(after < before / 6.0, "mean error {before} unwarped, {after} warped");
    }

    #[test]
    fn empty_and_flat_inputs_give_no_shift() {
        let flat = Plane::from_fn(400, 300, |_, _| 0.5);
        let f = estimate(&flat, &flat, &Homography::IDENTITY, RADIUS);
        assert_eq!(f.measured, 0);
        assert_eq!(f.max(), 0.0);
        assert_eq!(estimate(&Plane::new(0, 0), &Plane::new(0, 0), &Homography::IDENTITY, 3), Flow::default());
        assert_eq!(estimate(&flat, &Plane::new(0, 0), &Homography::IDENTITY, 3), Flow::default());
        assert_eq!(Flow::default().at(5.0, f64::NAN), (0.0, 0.0));
        assert_eq!(f.at(f64::INFINITY, 3.0), (0.0, 0.0));
        let (img, v) = warp(&Rgb32f::new(0, 0), &Homography::IDENTITY, &Flow::default());
        assert!(img.data.is_empty() && v.is_empty());
    }
}
