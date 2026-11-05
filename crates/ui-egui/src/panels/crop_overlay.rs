//! Crop guide overlays as polylines in crop-relative coordinates (`(u, v)` in `0..=1`, `u` across
//! the crop's width, `v` down its height). The crop tool maps them onto the (rotated) frame.
//! Composition guides are classic geometry: thirds, a grid, golden-section lines, 45° diagonals
//! from the corners, the harmonic triangle and the golden spiral.

use crate::state::CropOverlay;

/// Guide lines for `kind` on a `w × h` (pixels, any unit) crop. `orient` (0..4) mirrors the
/// asymmetric guides (triangle, spiral): bit 0 across, bit 1 down.
pub fn paths(kind: CropOverlay, orient: u8, w: f32, h: f32) -> Vec<Vec<(f32, f32)>> {
    let (w, h) = (w.max(1e-3), h.max(1e-3));
    let lines = |fracs: &[f32]| fracs.iter().flat_map(|&f| [vec![(f, 0.0), (f, 1.0)], vec![(0.0, f), (1.0, f)]]).collect();
    let out: Vec<Vec<(f32, f32)>> = match kind {
        CropOverlay::None => vec![],
        CropOverlay::Thirds => lines(&[1.0 / 3.0, 2.0 / 3.0]),
        CropOverlay::Grid => lines(&(1..8).map(|i| i as f32 / 8.0).collect::<Vec<_>>()),
        CropOverlay::Golden => lines(&[0.382, 0.618]),
        // 45° (in pixels) from each corner until it meets the far side
        CropOverlay::Diagonal => {
            let (du, dv) = (w.min(h) / w, w.min(h) / h);
            vec![
                vec![(0.0, 0.0), (du, dv)],
                vec![(1.0, 0.0), (1.0 - du, dv)],
                vec![(0.0, 1.0), (du, 1.0 - dv)],
                vec![(1.0, 1.0), (1.0 - du, 1.0 - dv)],
            ]
        }
        // the main diagonal and the perpendiculars to it from the other two corners
        CropOverlay::Triangle => {
            let foot = |px: f32, py: f32| {
                let n = (w * w + h * h).sqrt();
                let (dx, dy) = (w / n, h / n);
                let t = px * dx + py * dy;
                (t * dx / w, t * dy / h)
            };
            vec![vec![(0.0, 0.0), (1.0, 1.0)], vec![(1.0, 0.0), foot(w, 0.0)], vec![(0.0, 1.0), foot(0.0, h)]]
        }
        CropOverlay::Spiral => spiral(w, h),
    };
    let (fu, fv) = (orient & 1 == 1, orient & 2 == 2);
    let flip = |(u, v): (f32, f32)| (if fu { 1.0 - u } else { u }, if fv { 1.0 - v } else { v });
    match kind {
        CropOverlay::Triangle | CropOverlay::Spiral => out.into_iter().map(|l| l.into_iter().map(flip).collect()).collect(),
        _ => out,
    }
}

/// The golden spiral: squares carved off a golden-ratio rectangle (left, top, right, bottom, …), a
/// quarter circle in each, joined into one curve; plus the square boundaries.
///
/// The squares come off a `PHI × 1` rectangle whatever the crop's shape is, and the normalisation
/// at the end stretches the result to fit. Carving them off the crop itself made the guide depend
/// on the crop: a square crop lost everything after the first arc, because the first subtraction
/// left no rectangle to carve, and other ratios gave different numbers of arcs (#2393). Only a
/// golden-ratio rectangle subdivides into squares forever, so it is the one shape that gives a
/// complete spiral; stretching it distorts the curve on a crop of another ratio, which is the
/// price of the guide staying complete and is what the report asks for.
fn spiral(w: f32, h: f32) -> Vec<Vec<(f32, f32)>> {
    // A `PHI × 1` rectangle less a `1 × 1` square is `1 × 1/PHI`, the same shape again.
    const PHI: f32 = std::f32::consts::GOLDEN_RATIO;
    // work landscape; transpose back for portrait crops
    let portrait = h > w;
    let (rw, rh) = (PHI, 1.0);
    let (mut x, mut y, mut cw, mut ch) = (0.0f32, 0.0f32, rw, rh);
    let mut curve = Vec::new();
    let mut squares = Vec::new();
    for k in 0..10 {
        let s = cw.min(ch);
        if s < rw.min(rh) * 0.01 {
            break;
        }
        // square corner origin, arc centre, start angle (degrees, y down)
        let (sx, sy, cx, cy, a0) = match k % 4 {
            0 => (x, y, x + s, y + s, 180.0f32),
            1 => (x, y, x, y + s, 270.0),
            2 => (x + cw - s, y, x + cw - s, y, 0.0),
            _ => (x, y + ch - s, x + s, y + ch - s, 90.0),
        };
        for i in 0..=12 {
            let a = (a0 + 90.0 * i as f32 / 12.0).to_radians();
            curve.push((cx + s * a.cos(), cy + s * a.sin()));
        }
        squares.push(vec![(sx, sy), (sx + s, sy), (sx + s, sy + s), (sx, sy + s), (sx, sy)]);
        match k % 4 {
            0 => {
                x += s;
                cw -= s;
            }
            1 => {
                y += s;
                ch -= s;
            }
            2 => cw -= s,
            _ => ch -= s,
        }
    }
    let norm = |(px, py): (f32, f32)| if portrait { (py / rh, px / rw) } else { (px / rw, py / rh) };
    squares.push(curve);
    squares.into_iter().map(|l| l.into_iter().map(norm).collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_unit(paths: &[Vec<(f32, f32)>]) -> bool {
        paths.iter().flatten().all(|&(u, v)| (-1e-4..=1.0001).contains(&u) && (-1e-4..=1.0001).contains(&v))
    }

    #[test]
    fn every_overlay_stays_inside_the_crop() {
        for kind in [CropOverlay::Thirds, CropOverlay::Grid, CropOverlay::Golden, CropOverlay::Diagonal, CropOverlay::Triangle, CropOverlay::Spiral] {
            for (w, h) in [(3.0, 2.0), (2.0, 3.0), (1.0, 1.0), (16.0, 9.0)] {
                for o in 0..4 {
                    let p = paths(kind, o, w, h);
                    assert!(!p.is_empty() && in_unit(&p), "{kind:?} {w}×{h} {o}");
                }
            }
        }
        assert!(paths(CropOverlay::None, 0, 3.0, 2.0).is_empty());
    }

    #[test]
    fn diagonals_are_45_degrees_and_triangle_feet_are_perpendicular() {
        let (w, h) = (3.0, 2.0);
        for l in paths(CropOverlay::Diagonal, 0, w, h) {
            let (a, b) = (l[0], l[1]);
            assert!((((b.0 - a.0) * w).abs() - ((b.1 - a.1) * h).abs()).abs() < 1e-4, "{l:?}");
        }
        let t = paths(CropOverlay::Triangle, 0, w, h);
        for l in &t[1..] {
            let (a, b) = (l[0], l[1]);
            // perpendicular to the main diagonal (w, h) in pixels
            let dot = (b.0 - a.0) * w * w + (b.1 - a.1) * h * h;
            assert!(dot.abs() < 1e-3, "{l:?}");
        }
        // orientation mirrors the triangle
        assert_eq!(paths(CropOverlay::Triangle, 1, w, h)[0], vec![(1.0, 0.0), (0.0, 1.0)]);
    }

    /// The spiral is the same complete guide at every crop shape. Carving the squares off the
    /// crop itself made it depend on the crop: a square kept one arc of ten, 3:2 kept three and
    /// 16:9 four, so changing the crop truncated the guide (#2393).
    #[test]
    fn the_spiral_is_complete_at_every_aspect_ratio() {
        let shapes = [(1.0f32, 1.0f32), (1.618, 1.0), (1.0, 1.618), (3.0, 2.0), (2.0, 3.0), (16.0, 9.0), (9.0, 16.0), (4.0, 5.0)];
        let (squares, points) = {
            let p = paths(CropOverlay::Spiral, 0, 1.618, 1.0);
            (p.len() - 1, p.last().unwrap().len())
        };
        assert!(squares >= 8, "the golden rectangle itself gives a long spiral: {squares}");
        for (w, h) in shapes {
            for o in 0..4 {
                let p = paths(CropOverlay::Spiral, o, w, h);
                assert_eq!(p.len() - 1, squares, "{w}x{h} orient {o}: {} squares, not {squares}", p.len() - 1);
                assert_eq!(p.last().unwrap().len(), points, "{w}x{h} orient {o}: a shorter curve");
                assert!(in_unit(&p), "{w}x{h} orient {o}");
            }
        }
    }

    #[test]
    fn spiral_is_one_continuous_curve() {
        for (w, h) in [(1.618, 1.0), (1.0, 1.618), (3.0, 2.0)] {
            let p = paths(CropOverlay::Spiral, 0, w, h);
            let curve = p.last().unwrap();
            for pair in curve.windows(2) {
                let d = ((pair[1].0 - pair[0].0) * w).hypot((pair[1].1 - pair[0].1) * h);
                assert!(d < 0.2 * w.max(h), "jump {d} in {w}×{h}");
            }
        }
    }
}
