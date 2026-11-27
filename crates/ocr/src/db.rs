//! From the detector's text-probability map to text-line boxes: PaddleOCR's DB post-processing (`DBPostProcess`, box
//! type "quad") and the pipeline's box filtering and reading order.
//!
//! The map is thresholded; each connected region of text pixels gets its minimum-area rectangle, kept when its mean
//! probability is high enough and grown outwards ("unclipped") by `area × ratio / perimeter`, since the model is trained
//! to mark a shrunk core of each line. Boxes are then scaled to the photo, checked for size and put in reading order.

/// A box's corners, clockwise from the top-left, in pixels.
pub type Quad = [[f32; 2]; 4];

/// The detection thresholds: PaddleOCR 3's OCR pipeline defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// A pixel is text above this probability.
    pub thresh: f32,
    /// A box is kept when its mean probability is at least this.
    pub box_thresh: f32,
    /// How far a box grows (relative to area / perimeter).
    pub unclip_ratio: f32,
    /// Most regions looked at per image.
    pub max_candidates: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self { thresh: 0.3, box_thresh: 0.6, unclip_ratio: 1.5, max_candidates: 3000 }
    }
}

/// Smallest box side kept, in map pixels (`min_size`).
const MIN_SIZE: f32 = 3.0;

/// The minimum-area rectangle around `points`: its corners (in [`order`]ed form) and its shorter side.
fn min_area_rect(points: &[[f32; 2]]) -> Option<(Quad, f32)> {
    let hull = convex_hull(points);
    if hull.len() < 3 {
        return None;
    }
    let mut best: Option<(f32, Quad, f32)> = None;
    for i in 0..hull.len() {
        let (a, b) = (hull[i], hull[(i + 1) % hull.len()]);
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let len = (ex * ex + ey * ey).sqrt();
        if len <= 0.0 {
            continue;
        }
        let (ux, uy) = (ex / len, ey / len);
        let (mut lo_u, mut hi_u, mut lo_v, mut hi_v) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in &hull {
            let (dx, dy) = (p[0] - a[0], p[1] - a[1]);
            let (u, v) = (dx * ux + dy * uy, -dx * uy + dy * ux);
            lo_u = lo_u.min(u);
            hi_u = hi_u.max(u);
            lo_v = lo_v.min(v);
            hi_v = hi_v.max(v);
        }
        let area = (hi_u - lo_u) * (hi_v - lo_v);
        if best.as_ref().is_none_or(|(b, _, _)| area < *b) {
            let corner = |u: f32, v: f32| [a[0] + u * ux - v * uy, a[1] + u * uy + v * ux];
            let quad = [corner(lo_u, lo_v), corner(hi_u, lo_v), corner(hi_u, hi_v), corner(lo_u, hi_v)];
            best = Some((area, quad, (hi_u - lo_u).min(hi_v - lo_v)));
        }
    }
    best.map(|(_, q, side)| (order_mini_box(q), side))
}

/// Andrew's monotone chain; the hull anticlockwise (in y-down pixels, clockwise on screen), without repeats.
fn convex_hull(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut p: Vec<[f32; 2]> = points.iter().copied().filter(|q| q[0].is_finite() && q[1].is_finite()).collect();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut hull: Vec<[f32; 2]> = Vec::with_capacity(p.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &[f32; 2]>> = if pass == 0 { Box::new(p.iter()) } else { Box::new(p.iter().rev()) };
        for &q in iter {
            while hull.len() >= start + 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], q) <= 0.0 {
                hull.pop();
            }
            hull.push(q);
        }
        hull.pop();
    }
    hull
}

/// PaddleOCR's `get_mini_boxes` corner order: by x, then the upper of the two left points first and the upper of the
/// two right ones second (top-left, top-right, bottom-right, bottom-left).
fn order_mini_box(q: Quad) -> Quad {
    let mut p = q;
    p.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let (a, d) = if p[1][1] > p[0][1] { (p[0], p[1]) } else { (p[1], p[0]) };
    let (b, c) = if p[3][1] > p[2][1] { (p[2], p[3]) } else { (p[3], p[2]) };
    [a, b, c, d]
}

/// The mean probability inside the box (`box_score_fast`: corners truncated to whole pixels, edges included).
fn box_score(prob: &[f32], w: usize, h: usize, quad: &Quad) -> f32 {
    let clampi = |v: f32, hi: usize| (v.max(0.0) as usize).min(hi.saturating_sub(1));
    let xs = quad.map(|p| p[0]);
    let ys = quad.map(|p| p[1]);
    let (xmin, xmax) =
        (clampi(xs.iter().copied().fold(f32::MAX, f32::min).floor(), w), clampi(xs.iter().copied().fold(f32::MIN, f32::max).ceil(), w));
    let (ymin, ymax) =
        (clampi(ys.iter().copied().fold(f32::MAX, f32::min).floor(), h), clampi(ys.iter().copied().fold(f32::MIN, f32::max).ceil(), h));
    let poly: Vec<[f32; 2]> = quad.iter().map(|p| [(p[0] - xmin as f32).trunc(), (p[1] - ymin as f32).trunc()]).collect();
    let (mut sum, mut n) = (0f64, 0u64);
    for y in ymin..=ymax {
        for x in xmin..=xmax {
            if inside(&poly, (x - xmin) as f32, (y - ymin) as f32)
                && let Some(v) = prob.get(y * w + x)
            {
                sum += f64::from(*v);
                n += 1;
            }
        }
    }
    if n == 0 { 0.0 } else { (sum / n as f64) as f32 }
}

/// Whether (x, y) is inside the convex polygon or on its edge.
fn inside(poly: &[[f32; 2]], x: f32, y: f32) -> bool {
    let (mut pos, mut neg) = (false, false);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let c = (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
        if c > 1e-6 {
            pos = true;
        } else if c < -1e-6 {
            neg = true;
        }
    }
    !(pos && neg)
}

/// The rectangle grown by `d` on every side (pyclipper's rounded offset of a rectangle has this minimum-area box).
fn unclip(q: &Quad, ratio: f32) -> Option<(Quad, f32)> {
    let d = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    let (w, h) = (d(q[0], q[1]), d(q[1], q[2]));
    let (area, perimeter) = (w * h, 2.0 * (w + h));
    if perimeter.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    let dist = area * ratio / perimeter;
    let unit = |a: [f32; 2], b: [f32; 2], len: f32| [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
    let (u, v) = (unit(q[0], q[1], w), unit(q[1], q[2], h));
    let grow = |p: [f32; 2], su: f32, sv: f32| [p[0] + dist * (su * u[0] + sv * v[0]), p[1] + dist * (su * u[1] + sv * v[1])];
    let g = [grow(q[0], -1.0, -1.0), grow(q[1], 1.0, -1.0), grow(q[2], 1.0, 1.0), grow(q[3], -1.0, 1.0)];
    Some((order_mini_box(g), (w + 2.0 * dist).min(h + 2.0 * dist)))
}

/// Connected regions (8-neighbours) of `bitmap`, each as its boundary pixels' centres, in scan order of their first pixel.
fn regions(bitmap: &[bool], w: usize, h: usize, max: usize) -> Vec<Vec<[f32; 2]>> {
    let mut seen = vec![false; bitmap.len()];
    let mut out = Vec::new();
    let on = |x: isize, y: isize| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && bitmap.get(y as usize * w + x as usize) == Some(&true);
    for start in 0..bitmap.len() {
        if out.len() >= max {
            break;
        }
        if bitmap.get(start) != Some(&true) || seen.get(start) != Some(&false) {
            continue;
        }
        let mut stack = vec![start];
        if let Some(s) = seen.get_mut(start) {
            *s = true;
        }
        let mut boundary = Vec::new();
        while let Some(i) = stack.pop() {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            if !(on(x - 1, y) && on(x + 1, y) && on(x, y - 1) && on(x, y + 1)) {
                boundary.push([x as f32, y as f32]);
            }
            for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let (nx, ny) = (x + dx, y + dy);
                if on(nx, ny) {
                    let j = ny as usize * w + nx as usize;
                    if let Some(s) = seen.get_mut(j)
                        && !*s
                    {
                        *s = true;
                        stack.push(j);
                    }
                }
            }
        }
        out.push(boundary);
    }
    out
}

/// Clockwise from the top-left by position (`order_points_clockwise`): the two leftmost points, upper first, are the
/// top-left and bottom-left; the two rightmost the top-right and bottom-right.
fn order_clockwise(q: Quad) -> Quad {
    let mut p = q;
    p.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let (mut left, mut right) = ([p[0], p[1]], [p[2], p[3]]);
    left.sort_by(|a, b| a[1].total_cmp(&b[1]));
    right.sort_by(|a, b| a[1].total_cmp(&b[1]));
    [left[0], right[0], right[1], left[1]]
}

/// The text-line boxes in a probability map of `w × h`, in the pixels of a photo of `dest_w × dest_h`, with their
/// scores, in reading order.
pub fn boxes(prob: &[f32], w: usize, h: usize, dest_w: usize, dest_h: usize, p: &Params) -> Vec<(Quad, f32)> {
    // the clamps below need a photo of at least one pixel each way, and `w * h` is a size from the caller
    if w == 0 || h == 0 || dest_w == 0 || dest_h == 0 || w.checked_mul(h) != Some(prob.len()) {
        return Vec::new();
    }
    let bitmap: Vec<bool> = prob.iter().map(|v| *v > p.thresh).collect();
    let (sx, sy) = (dest_w as f32 / w as f32, dest_h as f32 / h as f32);
    let mut out = Vec::new();
    for region in regions(&bitmap, w, h, p.max_candidates) {
        let Some((quad, side)) = min_area_rect(&region) else { continue };
        if side < MIN_SIZE {
            continue;
        }
        let score = box_score(prob, w, h, &quad);
        if p.box_thresh > score {
            continue;
        }
        let Some((grown, side)) = unclip(&quad, p.unclip_ratio) else { continue };
        if side < MIN_SIZE + 2.0 {
            continue;
        }
        let scaled = grown.map(|c| [(c[0] * sx).round().clamp(0.0, dest_w as f32), (c[1] * sy).round().clamp(0.0, dest_h as f32)]);
        // the detector's own filter: clockwise order, inside the photo, more than 3 pixels each way
        let q = order_clockwise(scaled).map(|c| [c[0].clamp(0.0, dest_w as f32 - 1.0).floor(), c[1].clamp(0.0, dest_h as f32 - 1.0).floor()]);
        let d = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
        if d(q[0], q[1]) as usize <= 3 || d(q[0], q[3]) as usize <= 3 {
            continue;
        }
        out.push((q, score));
    }
    reading_order(&mut out);
    out
}

/// Top to bottom, then left to right among boxes whose tops are within 10 pixels (`sorted_boxes`).
fn reading_order(b: &mut [(Quad, f32)]) {
    b.sort_by(|x, y| x.0[0][1].total_cmp(&y.0[0][1]).then(x.0[0][0].total_cmp(&y.0[0][0])));
    for i in 0..b.len().saturating_sub(1) {
        for j in (0..=i).rev() {
            if (b[j + 1].0[0][1] - b[j].0[0][1]).abs() < 10.0 && b[j + 1].0[0][0] < b[j].0[0][0] {
                b.swap(j, j + 1);
            } else {
                break;
            }
        }
    }
}

/// The minimum-area rectangle of a detected box, in [`order_mini_box`] order (the pipeline crops along it).
pub fn crop_box(q: &Quad) -> Option<Quad> {
    min_area_rect(q).map(|(r, _)| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rotated_rectangle_has_its_own_minimum_box() {
        // a square turned 45°: the box is the square itself, side √2·2
        let pts = [[2.0, 0.0], [4.0, 2.0], [2.0, 4.0], [0.0, 2.0], [2.0, 2.0]];
        let (q, side) = min_area_rect(&pts).unwrap();
        assert!((side - 8f32.sqrt()).abs() < 1e-4, "{side}");
        // first corner as get_mini_boxes orders it: of the two leftmost points, the upper one when the lower is second
        assert!((q[0][0] - 2.0).abs() < 1e-4 && q[0][1].abs() < 1e-4, "{:?}", q[0]);
        assert!(min_area_rect(&[[1.0, 1.0], [2.0, 2.0], [3.0, 3.0]]).is_none(), "a line has no area");
    }

    #[test]
    fn finds_two_lines_in_reading_order() {
        // a 40×20 map with two bars of probability 0.9: one at the right of row 2, one lower at the left
        let (w, h) = (40, 20);
        let mut prob = vec![0f32; w * h];
        for y in 3..7 {
            for x in 22..36 {
                prob[y * w + x] = 0.9;
            }
        }
        for y in 12..17 {
            for x in 2..20 {
                prob[y * w + x] = 0.9;
            }
        }
        let b = boxes(&prob, w, h, 80, 40, &Params::default());
        assert_eq!(b.len(), 2, "{b:?}");
        assert!(b[0].0[0][1] < b[1].0[0][1], "top line first");
        assert!((b[0].1 - 0.9).abs() < 1e-5);
        // grown outwards and scaled ×2: wider than the 14-pixel core × 2
        assert!(b[0].0[1][0] - b[0].0[0][0] > 28.0, "{:?}", b[0].0);
        // a box too thin to be text is dropped
        let mut thin = vec![0f32; w * h];
        for x in 5..30 {
            thin[5 * w + x] = 0.9;
        }
        assert!(boxes(&thin, w, h, w, h, &Params::default()).is_empty());
        // mismatched sizes are no boxes, not a panic
        assert!(boxes(&prob, w + 1, h, w, h, &Params::default()).is_empty());
    }

    #[test]
    fn a_zero_sized_photo_or_an_overflowing_map_size_is_no_boxes_not_a_panic() {
        let (w, h) = (40, 20);
        let mut prob = vec![0f32; w * h];
        for y in 3..7 {
            for x in 22..36 {
                prob[y * w + x] = 0.9;
            }
        }
        // a region that passes every filter, scaled to a photo with no width or no height
        assert!(boxes(&prob, w, h, 0, 40, &Params::default()).is_empty());
        assert!(boxes(&prob, w, h, 80, 0, &Params::default()).is_empty());
        // w * h overflows usize (and wraps to the empty map's length in a release build)
        assert!(boxes(&[], usize::MAX / 2 + 1, 2, 10, 10, &Params::default()).is_empty());
    }

    #[test]
    fn lines_on_one_row_read_left_to_right() {
        let q = |x: f32, y: f32| [[x, y], [x + 10.0, y], [x + 10.0, y + 5.0], [x, y + 5.0]];
        let mut b = vec![(q(50.0, 12.0), 1.0), (q(10.0, 15.0), 1.0), (q(0.0, 40.0), 1.0)];
        reading_order(&mut b);
        assert_eq!(b.iter().map(|x| x.0[0][0]).collect::<Vec<_>>(), vec![10.0, 50.0, 0.0]);
    }
}
