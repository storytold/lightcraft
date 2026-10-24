//! Interactive crop editing: what dragging a crop handle does.
//!
//! The crop rectangle lives in the straightened (rotated) frame and must stay inside the rotated
//! image. A drag is a *domain operation* on the crop, not a UI detail: the handle being dragged
//! decides which edges are anchored, an optional aspect lock decides the shape, and the image
//! bounds decide how far the drag may go. [`drag_crop`] answers all three in one place so the app,
//! CLI, control channel and MCP agree.

use crate::{Affine, CropGeometry, Point, Rect, Vec2};

/// Smallest crop side, as a fraction of the image's shorter side.
pub const MIN_CROP_FRACTION: f64 = 0.02;

/// Most elongated locked crop shape (width:height or its inverse) the drag supports.
pub const MAX_RATIO: f64 = 20.0;

/// A grab point on the crop frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropHandle {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
    Top,
    Right,
    Bottom,
    Left,
    /// Drag the whole frame.
    Move,
}

impl CropHandle {
    /// The overlay's numbering: 0..3 corners (clockwise from top-left), 4..7 edges (top, right,
    /// bottom, left), 8 = move.
    pub fn from_index(i: u8) -> Option<CropHandle> {
        Some(match i {
            0 => CropHandle::TopLeft,
            1 => CropHandle::TopRight,
            2 => CropHandle::BottomRight,
            3 => CropHandle::BottomLeft,
            4 => CropHandle::Top,
            5 => CropHandle::Right,
            6 => CropHandle::Bottom,
            7 => CropHandle::Left,
            8 => CropHandle::Move,
            _ => return None,
        })
    }

    /// Parse a command-parameter name (`"topLeft"`, `"top"`, `"move"`, …).
    pub fn from_name(n: &str) -> Option<CropHandle> {
        Some(match n {
            "topLeft" => CropHandle::TopLeft,
            "topRight" => CropHandle::TopRight,
            "bottomRight" => CropHandle::BottomRight,
            "bottomLeft" => CropHandle::BottomLeft,
            "top" => CropHandle::Top,
            "right" => CropHandle::Right,
            "bottom" => CropHandle::Bottom,
            "left" => CropHandle::Left,
            "move" => CropHandle::Move,
            _ => return None,
        })
    }
}

/// Drag `handle` of `start` from `grab` to `pointer` (both normalized, in the straightened frame)
/// on a `w × h` image. `aspect` is the locked width/height ratio in pixels, if any.
///
/// Guarantees (for any input, including hostile numbers):
/// - the result lies inside the rotated image;
/// - the edges opposite the handle stay where they were (a move keeps the size);
/// - with `aspect`, the frame keeps that shape on every handle (when `start` had it);
/// - the drag stops at the image edge instead of pushing the frame around, and slides along it.
pub fn drag_crop(start: CropGeometry, handle: CropHandle, grab: Point, pointer: Point, w: f64, h: f64, aspect: Option<f64>) -> CropGeometry {
    let sane = |v: f64| v.is_finite();
    let r0 = start.rect;
    if ![w, h, grab.x, grab.y, pointer.x, pointer.y, start.angle, r0.x0, r0.y0, r0.x1, r0.y1].iter().copied().all(sane)
        || w <= 0.0
        || h <= 0.0
        || r0.is_empty()
        || aspect.is_some_and(|a| !(a.is_finite() && a > 0.0))
    {
        return start;
    }
    // Start from a valid frame, then work in pixels of the straightened frame.
    let mut base = start.constrained(w, h);
    if base.angle == 0.0 {
        // absorb the 1e-6 tolerance of `is_within_image` so the exact test below accepts the start
        let r = base.rect;
        base.rect = Rect::new(r.x0.clamp(0.0, 1.0), r.y0.clamp(0.0, 1.0), r.x1.clamp(0.0, 1.0), r.y1.clamp(0.0, 1.0));
    }
    let sr = base.rect_px(w, h);
    // Moving further than twice the image never matters; clamping keeps the searches well-conditioned.
    let (dx, dy) = (((pointer.x - grab.x) * w).clamp(-2.0 * w, 2.0 * w), ((pointer.y - grab.y) * h).clamp(-2.0 * h, 2.0 * h));
    // Ratios beyond 20:1 make even the minimum frame larger than the image; treat them as 20:1.
    let aspect = aspect.map(|a| a.clamp(1.0 / MAX_RATIO, MAX_RATIO));
    // Unrotated, the frame must be exactly inside the image (`is_within_image` tolerates 1e-6, which
    // would leak out as a sliver past the edge and, after any clamp, a shifted ratio).
    let feasible = |r: Rect| {
        if base.angle == 0.0 {
            r.x0 >= 0.0 && r.y0 >= 0.0 && r.x1 <= w && r.y1 <= h
        } else {
            CropGeometry { rect: r.scale(1.0 / w, 1.0 / h), angle: base.angle }.is_within_image(w, h)
        }
    };
    let min_side = MIN_CROP_FRACTION * w.min(h);

    let out = match (handle, aspect) {
        (CropHandle::Move, _) => {
            let slide = |cur: Rect, ax: f64, ay: f64| {
                let t = bisect(0.0, 1.0, |t| feasible(cur.translate(Vec2::new(ax * t, ay * t))));
                cur.translate(Vec2::new(ax * t, ay * t))
            };
            // straightened, go to the nearest place that fits, so the frame slides along tilted edges (#2299)
            let nearest = if base.angle != 0.0 {
                let from = |r: Rect| {
                    let corners = r.corners().map(|c| (c, Vec2::new(1.0, 1.0)));
                    nearest_offset(&corners, &[], base.angle, w, h, Vec2::new(dx, dy)).map(|v| r.translate(v)).filter(|r| feasible(*r))
                };
                // a frame at the tolerance's edge on opposite sides moves only once it is inside it
                from(sr).or_else(|| shrunk_inside(sr, base.angle, w, h).and_then(from))
            } else {
                None
            };
            match nearest {
                Some(r) => r,
                None => slide(slide(sr, dx, 0.0), 0.0, dy),
            }
        }
        (hd, None) => {
            let (mx0, mx1, my0, my1) = sides(hd);
            let grow = |cur: Rect, tx: f64, ty: f64| {
                // `tx`/`ty`: how far the moving edges would go; interpolate towards it
                let target = Rect::new(
                    if mx0 { (cur.x0 + tx).min(cur.x1 - min_side) } else { cur.x0 },
                    if my0 { (cur.y0 + ty).min(cur.y1 - min_side) } else { cur.y0 },
                    if mx1 { (cur.x1 + tx).max(cur.x0 + min_side) } else { cur.x1 },
                    if my1 { (cur.y1 + ty).max(cur.y0 + min_side) } else { cur.y1 },
                );
                let at = |t: f64| lerp_rect(cur, target, t);
                at(bisect(0.0, 1.0, |t| feasible(at(t))))
            };
            // straightened, a free corner goes to the nearest place that fits, growing both sides (#2308)
            let corner = (mx0 || mx1) && (my0 || my1);
            let nearest =
                if base.angle != 0.0 && corner { nearest_corner(sr, mx0, my0, base.angle, (w, h), min_side, Vec2::new(dx, dy)) } else { None };
            match nearest.filter(|r| feasible(*r)) {
                Some(r) => r,
                None => grow(grow(sr, dx, 0.0), 0.0, dy),
            }
        }
        (hd, Some(a)) => {
            let (mx0, _, my0, _) = sides(hd);
            let (min_w, min_h) = (min_side.max(min_side * a), min_side.max(min_side / a));
            let c = sr.center();
            // Anchor: the opposite corner / edge midpoint. `build(n, o)` is the frame of width `n` that
            // keeps it, shifted by `o` along the anchored edge (edge handles only).
            let (target_w, build): (f64, Box<dyn Fn(f64, f64) -> Rect>) = match hd {
                CropHandle::Top | CropHandle::Bottom => {
                    let (ay, sy, edge) = if my0 { (sr.y1, -1.0, sr.y0) } else { (sr.y0, 1.0, sr.y1) };
                    let nh = (sy * (edge + dy - ay)).max(min_h);
                    (nh * a, Box::new(move |nw, o| vband(c.x + o, ay, sy, nw, a)))
                }
                CropHandle::Left | CropHandle::Right => {
                    let (ax, sx, edge) = if mx0 { (sr.x1, -1.0, sr.x0) } else { (sr.x0, 1.0, sr.x1) };
                    let nw = (sx * (edge + dx - ax)).max(min_w);
                    (nw, Box::new(move |nw, o| hband(ax, sx, c.y + o, nw, a)))
                }
                _ => {
                    let (ax, sx, ex) = if mx0 { (sr.x1, -1.0, sr.x0) } else { (sr.x0, 1.0, sr.x1) };
                    let (ay, sy, ey) = if my0 { (sr.y1, -1.0, sr.y0) } else { (sr.y0, 1.0, sr.y1) };
                    let (wx, wy) = (sx * (ex + dx - ax), sy * (ey + dy - ay));
                    let nw = ((wx + wy * a) / 2.0).max(min_w);
                    (nw, Box::new(move |nw, _| corner_rect(ax, sx, ay, sy, nw, a)))
                }
            };
            let target_w = target_w.min(2.0 * w.max(h * a));
            let slides = matches!(hd, CropHandle::Top | CropHandle::Bottom | CropHandle::Left | CropHandle::Right);
            let extent = if matches!(hd, CropHandle::Top | CropHandle::Bottom) { w } else { h };
            // Frame of width `n`, centred on the anchored edge if it fits, else slid the least distance.
            let place = |n: f64| -> Option<Rect> {
                let at = |o: f64| build(n, o);
                if feasible(at(0.0)) {
                    return Some(at(0.0));
                }
                if !slides {
                    return None;
                }
                const STEPS: u32 = 128;
                let step = extent / STEPS as f64;
                for k in 1..=STEPS {
                    for sign in [1.0, -1.0] {
                        let hi = sign * k as f64 * step;
                        if feasible(at(hi)) {
                            // closest feasible offset: bisect between the last infeasible and this one
                            let (mut bad, mut good) = (sign * (k - 1) as f64 * step, hi);
                            for _ in 0..40 {
                                let mid = (bad + good) / 2.0;
                                if feasible(at(mid)) {
                                    good = mid;
                                } else {
                                    bad = mid;
                                }
                            }
                            return Some(at(good));
                        }
                    }
                }
                None
            };
            if place(min_w).is_some() {
                let n = bisect_range(min_w, target_w.max(min_w), |n| place(n).is_some());
                place(n).unwrap_or(sr)
            } else {
                sr
            }
        }
    };
    let rect = out.scale(1.0 / w, 1.0 / h);
    if [rect.x0, rect.y0, rect.x1, rect.y1].iter().all(|v| v.is_finite()) { CropGeometry { rect, angle: base.angle } } else { start }
}

/// The offset `v` (pixels) nearest `want` that keeps each `p + m ⊙ v` inside a `w × h` image turned by `angle`
/// degrees and meets the `extra` half-planes `n · v <= b`.
fn nearest_offset(points: &[(Point, Vec2)], extra: &[(Vec2, f64)], angle: f64, w: f64, h: f64, want: Vec2) -> Option<Vec2> {
    let back = Affine::rotate_about(-angle.to_radians(), Point::new(w / 2.0, h / 2.0));
    let (ex, ey) = (back.apply_vec(Vec2::new(1.0, 0.0)), back.apply_vec(Vec2::new(0.0, 1.0)));
    // never tighter than where the frame is, plus a slack so a zero-width strip survives rounding
    let (sx, sy) = (1e-9 * w, 1e-9 * h);
    let mut planes = Vec::with_capacity(4 * points.len() + extra.len());
    for (p, m) in points {
        let q = back.apply(*p);
        let (nx, ny) = (Vec2::new(ex.x * m.x, ey.x * m.y), Vec2::new(ex.y * m.x, ey.y * m.y));
        for (n, b, s) in [(nx, w - q.x, sx), (-nx, q.x, sx), (ny, h - q.y, sy), (-ny, q.y, sy)] {
            planes.push((n, b.max(0.0) + s));
        }
    }
    planes.extend_from_slice(extra);
    if planes.iter().all(|(n, b)| n.dot(want) <= *b) {
        return Some(want);
    }
    let lim = 4.0 * w.max(h);
    let mut poly = vec![Vec2::new(-lim, -lim), Vec2::new(lim, -lim), Vec2::new(lim, lim), Vec2::new(-lim, lim)];
    for (n, b) in &planes {
        poly = clip_half_plane(&poly, *n, *b);
    }
    poly.iter()
        .zip(poly.iter().cycle().skip(1))
        .map(|(a, b)| {
            let ab = *b - *a;
            let l2 = ab.dot(ab);
            let t = if l2 > 0.0 { ((want - *a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
            *a + ab * t
        })
        .filter(|v| v.x.is_finite() && v.y.is_finite())
        .min_by(|a, b| (*a - want).len().total_cmp(&(*b - want).len()))
}

/// `r` scaled about its centre by the largest factor down to `1 - 1e-5` that puts it exactly inside a `w × h` image
/// turned by `angle` degrees.
fn shrunk_inside(r: Rect, angle: f64, w: f64, h: f64) -> Option<Rect> {
    let back = Affine::rotate_about(-angle.to_radians(), Point::new(w / 2.0, h / 2.0));
    let scaled = |k: f64| Rect::from_center(r.center(), r.width() * k, r.height() * k);
    let inside = |k: f64| {
        scaled(k).corners().iter().all(|c| {
            let q = back.apply(*c);
            q.x >= 0.0 && q.y >= 0.0 && q.x <= w && q.y <= h
        })
    };
    let lo = 1.0 - 1e-5;
    inside(lo).then(|| scaled(bisect_range(lo, 1.0, inside)))
}

/// The frame `r` (pixels) with a corner dragged by `want`, the opposite one held, at the nearest place that fits.
fn nearest_corner(r: Rect, left: bool, top: bool, angle: f64, (w, h): (f64, f64), min_side: f64, want: Vec2) -> Option<Rect> {
    let anchor = Point::new(if left { r.x1 } else { r.x0 }, if top { r.y1 } else { r.y0 });
    let corner = Point::new(if left { r.x0 } else { r.x1 }, if top { r.y0 } else { r.y1 });
    let points =
        [(corner, Vec2::new(1.0, 1.0)), (Point::new(corner.x, anchor.y), Vec2::new(1.0, 0.0)), (Point::new(anchor.x, corner.y), Vec2::new(0.0, 1.0))];
    let (sx, sy) = (if left { -1.0 } else { 1.0 }, if top { -1.0 } else { 1.0 });
    // at least `min_side`, and per axis only between where it is and where the pointer asks
    let extra = [
        (Vec2::new(-sx, 0.0), sx * (corner.x - anchor.x) - min_side),
        (Vec2::new(0.0, -sy), sy * (corner.y - anchor.y) - min_side),
        (Vec2::new(1.0, 0.0), want.x.max(0.0)),
        (Vec2::new(-1.0, 0.0), (-want.x).max(0.0)),
        (Vec2::new(0.0, 1.0), want.y.max(0.0)),
        (Vec2::new(0.0, -1.0), (-want.y).max(0.0)),
    ];
    let v = nearest_offset(&points, &extra, angle, w, h, want)?;
    Some(Rect::from_points(anchor, corner + v))
}

/// The part of the convex polygon `poly` where `n · v <= b`.
fn clip_half_plane(poly: &[Vec2], n: Vec2, b: f64) -> Vec<Vec2> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for (p, q) in poly.iter().zip(poly.iter().cycle().skip(1)) {
        let (dp, dq) = (n.dot(*p) - b, n.dot(*q) - b);
        if dp <= 0.0 {
            out.push(*p);
        }
        if (dp <= 0.0) != (dq <= 0.0) {
            out.push(*p + (*q - *p) * (dp / (dp - dq)));
        }
    }
    out
}

/// Which sides a handle moves: (left, right, top, bottom).
fn sides(h: CropHandle) -> (bool, bool, bool, bool) {
    use CropHandle::*;
    (
        matches!(h, TopLeft | BottomLeft | Left),
        matches!(h, TopRight | BottomRight | Right),
        matches!(h, TopLeft | TopRight | Top),
        matches!(h, BottomLeft | BottomRight | Bottom),
    )
}

fn lerp_rect(a: Rect, b: Rect, t: f64) -> Rect {
    let l = |p: f64, q: f64| p + (q - p) * t;
    Rect::new(l(a.x0, b.x0), l(a.y0, b.y0), l(a.x1, b.x1), l(a.y1, b.y1))
}

/// Frame of width `nw` and ratio `a` extending from corner (`ax`, `ay`) in directions `sx`, `sy`.
fn corner_rect(ax: f64, sx: f64, ay: f64, sy: f64, nw: f64, a: f64) -> Rect {
    let nh = nw / a;
    Rect::from_points(Point::new(ax, ay), Point::new(ax + sx * nw, ay + sy * nh))
}

/// Frame whose top/bottom edge (at `ay`, growing in direction `sy`) is anchored, centred on `cx`.
fn vband(cx: f64, ay: f64, sy: f64, nw: f64, a: f64) -> Rect {
    let nh = nw / a;
    Rect::from_points(Point::new(cx - nw / 2.0, ay), Point::new(cx + nw / 2.0, ay + sy * nh))
}

/// Frame whose left/right edge (at `ax`, growing in direction `sx`) is anchored, centred on `cy`.
fn hband(ax: f64, sx: f64, cy: f64, nw: f64, a: f64) -> Rect {
    let nh = nw / a;
    Rect::from_points(Point::new(ax, cy - nh / 2.0), Point::new(ax + sx * nw, cy + nh / 2.0))
}

/// Largest `t` in `[lo, hi]` for which `ok` holds, given that it holds at `lo` and the set of good
/// `t` is an interval (the feasible region is convex).
fn bisect_range(lo: f64, hi: f64, ok: impl Fn(f64) -> bool) -> f64 {
    if ok(hi) {
        return hi;
    }
    let (mut good, mut bad) = (lo, hi);
    for _ in 0..48 {
        let mid = (good + bad) / 2.0;
        if ok(mid) {
            good = mid;
        } else {
            bad = mid;
        }
    }
    good
}

fn bisect(lo: f64, hi: f64, ok: impl Fn(f64) -> bool) -> f64 {
    bisect_range(lo, hi, ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use CropHandle::*;

    const W: f64 = 600.0;
    const H: f64 = 400.0;
    const EPS: f64 = 1e-6;

    fn geo(x0: f64, y0: f64, x1: f64, y1: f64) -> CropGeometry {
        CropGeometry { rect: Rect::new(x0, y0, x1, y1), angle: 0.0 }
    }
    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }
    fn drag(g: CropGeometry, hd: CropHandle, from: Point, to: Point, aspect: Option<f64>) -> CropGeometry {
        drag_crop(g, hd, from, to, W, H, aspect)
    }
    fn px_aspect(g: &CropGeometry) -> f64 {
        g.rect.width() * W / (g.rect.height() * H)
    }
    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // Feature: dragging a crop handle
    //   Scenario: a free corner drag inside the image
    #[test]
    fn given_free_crop_when_corner_dragged_inside_then_corner_follows_and_opposite_corner_stays() {
        let g = drag(geo(0.2, 0.2, 0.8, 0.8), BottomRight, p(0.8, 0.8), p(0.9, 0.7), None);
        assert!(near(g.rect.x0, 0.2) && near(g.rect.y0, 0.2));
        assert!(near(g.rect.x1, 0.9) && near(g.rect.y1, 0.7));
    }

    //   Scenario: dragging past the right edge (the weird behaviour of issue 295)
    #[test]
    fn given_corner_dragged_past_image_edge_then_it_stops_at_the_edge_and_anchor_stays() {
        let g = drag(geo(0.2, 0.2, 0.8, 0.8), BottomRight, p(0.8, 0.8), p(1.3, 0.7), None);
        assert!(near(g.rect.x0, 0.2) && near(g.rect.y0, 0.2), "anchor moved: {:?}", g.rect);
        assert!(near(g.rect.x1, 1.0), "stops at the edge: {:?}", g.rect);
        assert!(near(g.rect.y1, 0.7), "other axis still follows the pointer: {:?}", g.rect);
    }

    #[test]
    fn given_locked_aspect_when_corner_dragged_past_edge_then_ratio_and_anchor_hold() {
        let a = 1.5;
        let g = drag(geo(0.2, 0.2, 0.8, 0.2 + 0.6 * W / H / a), BottomRight, p(0.8, 0.6), p(1.4, 0.9), Some(a));
        assert!(near(g.rect.x0, 0.2) && near(g.rect.y0, 0.2), "{:?}", g.rect);
        assert!(near(px_aspect(&g), a), "{}", px_aspect(&g));
        assert!(g.rect.x1 <= 1.0 + EPS && g.rect.y1 <= 1.0 + EPS);
        assert!(g.rect.x1 > 0.95 || g.rect.y1 > 0.95, "grows until it touches an edge: {:?}", g.rect);
    }

    #[test]
    fn given_locked_aspect_when_edge_dragged_then_ratio_holds_and_frame_grows_about_its_midline() {
        let a = 2.0;
        let start = geo(0.3, 0.3, 0.7, 0.3 + 0.4 * W / H / a);
        let g = drag(start, Bottom, p(0.5, start.rect.y1), p(0.5, start.rect.y1 + 0.1), Some(a));
        assert!(near(g.rect.y0, start.rect.y0), "opposite edge stays");
        assert!(near(g.rect.center().x, start.rect.center().x), "grows symmetrically sideways");
        assert!(near(px_aspect(&g), a));
        assert!(g.rect.height() > start.rect.height());
    }

    #[test]
    fn given_free_crop_when_edge_dragged_then_only_that_edge_moves() {
        let g = drag(geo(0.2, 0.2, 0.8, 0.8), Left, p(0.2, 0.5), p(0.1, 0.9), None);
        assert!(near(g.rect.x0, 0.1) && near(g.rect.y0, 0.2) && near(g.rect.x1, 0.8) && near(g.rect.y1, 0.8));
    }

    #[test]
    fn given_move_past_edge_then_size_is_kept_and_frame_slides_along_the_edge() {
        let start = geo(0.5, 0.25, 0.9, 0.65);
        let g = drag(start, Move, p(0.7, 0.45), p(1.3, 0.55), None);
        assert!(near(g.rect.width(), 0.4) && near(g.rect.height(), 0.4));
        assert!(near(g.rect.x1, 1.0), "flush with the right edge: {:?}", g.rect);
        assert!(near(g.rect.y0, 0.35), "still follows the pointer vertically: {:?}", g.rect);
    }

    #[test]
    fn given_edge_dragged_across_the_opposite_edge_then_crop_keeps_a_minimum_size() {
        let g = drag(geo(0.2, 0.2, 0.8, 0.8), Right, p(0.8, 0.5), p(-0.5, 0.5), None);
        assert!(g.rect.width() * W >= MIN_CROP_FRACTION * H - EPS && g.rect.x1 > g.rect.x0);
        assert!(near(g.rect.x0, 0.2));
    }

    #[test]
    fn given_straightened_image_when_dragged_outside_then_crop_stays_inside_the_rotated_image() {
        let start = crate::crop_fit_angle(W, H, 12.0, Some(1.5));
        let c = start.rect.center();
        for hd in [TopLeft, TopRight, BottomRight, BottomLeft, Top, Right, Bottom, Left, Move] {
            for aspect in [None, Some(1.5)] {
                let g = drag(start, hd, c, p(3.0, -3.0), aspect);
                assert!(g.is_within_image(W, H), "{hd:?} {aspect:?}: {:?}", g.rect);
                assert_eq!(g.angle, 12.0);
            }
        }
    }

    #[test]
    fn given_hostile_numbers_then_the_start_crop_is_returned_not_a_panic() {
        let s = geo(0.2, 0.2, 0.8, 0.8);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(drag(s, BottomRight, p(0.8, 0.8), p(bad, 0.5), None), s);
        }
        assert_eq!(drag_crop(s, BottomRight, p(0.8, 0.8), p(0.9, 0.9), 0.0, 400.0, None), s);
        assert_eq!(drag(s, BottomRight, p(0.8, 0.8), p(0.9, 0.9), Some(f64::NAN)), s);
        assert_eq!(drag(s, BottomRight, p(0.8, 0.8), p(0.9, 0.9), Some(-1.0)), s);
    }

    //   Scenario: a locked edge drag when the frame already touches the image edge (review finding)
    #[test]
    fn given_locked_frame_flush_with_the_top_when_right_edge_dragged_out_then_it_grows_sliding_down() {
        let a = 1.5;
        let start = geo(0.2, 0.0, 0.5, 0.3 * W / H * 0.0 + 0.3 * W / (a * H));
        let g = drag(start, Right, p(0.5, 0.2), p(0.9, 0.2), Some(a));
        assert!(near(g.rect.x0, 0.2), "anchor edge stays: {:?}", g.rect);
        assert!(g.rect.width() > start.rect.width() + 0.05, "it must grow: {:?}", g.rect);
        assert!(near(px_aspect(&g), a));
        assert!(g.is_within_image(W, H));
    }

    #[test]
    fn given_full_fit_straightened_crop_when_locked_edge_dragged_then_it_still_respects_the_image() {
        let start = crate::crop_fit_angle(W, H, 20.0, Some(1.5));
        let g = drag(start, Right, p(0.9, 0.5), p(1.5, 0.5), Some(1.5));
        assert!(g.is_within_image(W, H) && near(px_aspect(&g), 1.5));
    }

    //   Scenario: extreme or hostile magnitudes
    #[test]
    fn given_an_extreme_locked_ratio_then_the_frame_can_still_be_moved() {
        let start = geo(0.1, 0.4, 0.9, 0.4 + 0.8 * W / (60.0 * H));
        let g = drag(start, Move, p(0.5, 0.4), p(0.5, 0.7), Some(60.0));
        assert!(g.rect.y0 > 0.5, "moved: {:?}", g.rect);
    }

    #[test]
    fn given_a_huge_finite_pointer_then_a_locked_corner_still_grows_to_the_edge() {
        let a = 1.5;
        let start = geo(0.2, 0.2, 0.5, 0.2 + 0.3 * W / (a * H));
        let g = drag(start, BottomRight, p(0.5, 0.4), p(1e300, 1e300), Some(a));
        assert!(g.rect.x1 > 0.95 || g.rect.y1 > 0.95, "{:?}", g.rect);
        let g = drag(start, BottomRight, p(0.5, 0.4), p(1e300, 1e300), None);
        assert!(near(g.rect.x1, 1.0) && near(g.rect.y1, 1.0), "{:?}", g.rect);
    }

    #[test]
    fn given_an_unrotated_image_then_results_never_leave_the_unit_square() {
        let g = drag(geo(0.2, 0.2, 0.8, 0.8), BottomRight, p(0.8, 0.8), p(5.0, 5.0), None);
        assert!(g.rect.x1 <= 1.0 && g.rect.y1 <= 1.0 && g.rect.x0 >= 0.0 && g.rect.y0 >= 0.0, "{:?}", g.rect);
    }

    #[test]
    fn handle_numbering_matches_the_overlay() {
        assert_eq!(CropHandle::from_index(0), Some(TopLeft));
        assert_eq!(CropHandle::from_index(6), Some(Bottom));
        assert_eq!(CropHandle::from_index(8), Some(Move));
        assert_eq!(CropHandle::from_index(9), None);
        assert_eq!(CropHandle::from_name("bottomLeft"), Some(BottomLeft));
        assert_eq!(CropHandle::from_name("nope"), None);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn any_handle() -> impl Strategy<Value = CropHandle> {
            (0u8..9).prop_map(|i| CropHandle::from_index(i).unwrap_or(Move))
        }

        proptest! {
            /// Whatever the pointer does, the crop stays valid; the anchor and the ratio hold.
            #[test]
            fn drag_is_always_inside_anchored_and_shaped(
                angle in -45.0f64..45.0,
                ratio in prop::option::of(0.3f64..4.0),
                hd in any_handle(),
                tx in -4.0f64..5.0, ty in -4.0f64..5.0,
                shrink in 0.2f64..1.0,
            ) {
                let base = crate::crop_fit_angle(W, H, angle, ratio);
                // a smaller start crop so there is room to move
                let c = base.rect.center();
                let start = CropGeometry { rect: Rect::from_center(c, base.rect.width() * shrink, base.rect.height() * shrink), angle };
                prop_assume!(start.is_within_image(W, H));
                let g = drag_crop(start, hd, c, p(tx, ty), W, H, ratio);
                prop_assert!(g.is_within_image(W, H), "{:?} -> {:?}", start.rect, g.rect);
                prop_assert!(g.rect.width() > 0.0 && g.rect.height() > 0.0);
                let min = MIN_CROP_FRACTION * W.min(H) - 1e-6;
                prop_assert!(g.rect.width() * W >= min.min(start.rect.width() * W) && g.rect.height() * H >= min.min(start.rect.height() * H), "min size {:?}", g.rect);
                if let Some(a) = ratio {
                    prop_assert!((px_aspect(&g) - a).abs() < 1e-6 * a.max(1.0), "ratio {a}: {}", px_aspect(&g));
                }
                // anchors: edges opposite the handle do not move
                let (s, r) = (start.rect, g.rect);
                let keep_l = matches!(hd, TopRight | BottomRight | Right);
                let keep_r = matches!(hd, TopLeft | BottomLeft | Left);
                let keep_t = matches!(hd, BottomLeft | BottomRight | Bottom);
                let keep_b = matches!(hd, TopLeft | TopRight | Top);
                {
                    if keep_l { prop_assert!((r.x0 - s.x0).abs() < 1e-6, "x0"); }
                    if keep_r { prop_assert!((r.x1 - s.x1).abs() < 1e-6, "x1"); }
                    if keep_t { prop_assert!((r.y0 - s.y0).abs() < 1e-6, "y0"); }
                    if keep_b { prop_assert!((r.y1 - s.y1).abs() < 1e-6, "y1"); }
                }
                if hd == Move {
                    prop_assert!((r.width() - s.width()).abs() < 1e-9 && (r.height() - s.height()).abs() < 1e-9);
                }
            }

            #[test]
            fn drag_never_panics_on_arbitrary_numbers(
                x0 in -2.0f64..3.0, y0 in -2.0f64..3.0, x1 in -2.0f64..3.0, y1 in -2.0f64..3.0,
                angle in -90.0f64..90.0, tx in -1e9f64..1e9, ty in -1e9f64..1e9,
                ratio in prop::option::of(-1.0f64..10.0), hd in any_handle(),
            ) {
                let g = drag_crop(CropGeometry { rect: Rect::new(x0, y0, x1, y1), angle }, hd, p(0.5, 0.5), p(tx, ty), W, H, ratio);
                prop_assert!(g.rect.x0.is_finite() && g.rect.y1.is_finite());
            }
        }
    }

    //   Scenario: moving a frame that fills a straightened photo's height (issue #2299)
    #[test]
    fn given_largest_locked_frame_on_a_straightened_photo_when_moved_sideways_then_it_slides_along_the_tilt() {
        for angle in [3.0, -10.0] {
            let a = 2.0 / 3.0;
            let start = crate::crop_fit_angle(W, H, angle, Some(a));
            let c = start.rect.center();
            let g = drag(start, Move, c, p(c.x - 0.5, c.y), Some(a));
            assert!(g.is_within_image(W, H), "{angle}: {:?}", g.rect);
            assert!(near(g.rect.width(), start.rect.width()) && near(g.rect.height(), start.rect.height()), "a move keeps the size");
            assert!(g.rect.x0 < start.rect.x0 - 0.2, "{angle}: stuck at {:?} (from {:?})", g.rect, start.rect);
            let gc = g.rect.center();
            let further = drag(g, Move, gc, p(gc.x - 0.5, gc.y), Some(a));
            assert!((further.rect.x0 - g.rect.x0).abs() < 1e-4, "{angle}: not at the left side: {:?} → {:?}", g.rect, further.rect);
            let back = drag(g, Move, c, p(c.x + 0.5, c.y), Some(a));
            assert!(back.rect.x1 > start.rect.x1 + 0.2, "{angle}: {:?}", back.rect);
        }
    }

    //   Scenario: a straightened move that fits goes exactly where the pointer says
    #[test]
    fn given_straightened_photo_when_a_small_frame_is_moved_inside_then_it_follows_the_pointer() {
        let start = CropGeometry { rect: Rect::new(0.4, 0.4, 0.5, 0.5), angle: 8.0 };
        let g = drag(start, Move, p(0.45, 0.45), p(0.5, 0.42), None);
        assert!(near(g.rect.x0, 0.45) && near(g.rect.y0, 0.37), "{:?}", g.rect);
    }

    //   Scenario: a frame a handle drag left within the tolerance past a tilted edge (review of #2299)
    #[test]
    fn given_a_frame_just_past_a_tilted_edge_when_moved_then_it_still_slides_and_a_click_changes_nothing() {
        let a = 2.0 / 3.0;
        let fit = crate::crop_fit_angle(W, H, 3.0, Some(a));
        let k = 1.0 + 5e-7 / fit.rect.height();
        let start = CropGeometry { rect: Rect::from_center(fit.rect.center(), fit.rect.width() * k, fit.rect.height() * k), angle: 3.0 };
        assert!(start.is_within_image(W, H));
        let c = start.rect.center();
        let z = drag(start, Move, c, c, Some(a)).rect;
        let s = start.rect;
        assert!(near(z.x0, s.x0) && near(z.y0, s.y0) && near(z.x1, s.x1) && near(z.y1, s.y1), "a click inside the frame moves nothing: {z:?}");
        let g = drag(start, Move, c, p(c.x - 0.5, c.y), Some(a));
        assert!(g.rect.x0 < start.rect.x0 - 0.2, "stuck at {:?}", g.rect);
        assert!(g.is_within_image(W, H));
    }

    //   Scenario: a frame a handle drag enlarged to a tilted edge (review of #2299)
    #[test]
    fn given_a_frame_enlarged_to_a_tilted_edge_by_a_handle_when_moved_then_it_still_slides() {
        let a = 2.0 / 3.0;
        for angle in [3.0, 6.0, -10.0, 12.0] {
            let fit = crate::crop_fit_angle(W, H, angle, Some(a));
            let small = CropGeometry { rect: Rect::from_center(fit.rect.center(), fit.rect.width() * 0.6, fit.rect.height() * 0.6), angle };
            // the second fills the photo's height, at the tolerance's edge top and bottom
            let shrunk = drag(fit, BottomRight, p(0.5, 0.5), p(0.35, 0.35), Some(a));
            for start in [small, shrunk] {
                let big = drag(start, BottomRight, p(0.5, 0.5), p(1.5, 1.5), Some(a));
                let c = big.rect.center();
                let g = drag(big, Move, c, p(c.x - 0.5, c.y), Some(a));
                assert!(g.rect.x0 < big.rect.x0 - 0.2, "{angle}: stuck at {:?}", g.rect);
                assert!(
                    g.is_within_image(W, H)
                        && (g.rect.width() / big.rect.width() - 1.0).abs() < 1e-5
                        && (g.rect.height() / big.rect.height() - 1.0).abs() < 1e-5,
                    "{angle}: {:?}",
                    g.rect
                );
            }
        }
    }

    //   Scenario: a smaller frame pushed sideways past a tilted edge slides along it (#2299)
    #[test]
    fn given_a_small_frame_pushed_past_a_tilted_edge_then_it_stops_on_the_edge_and_slides_along_it() {
        let start = CropGeometry { rect: Rect::new(0.35, 0.35, 0.55, 0.65), angle: 6.0 };
        let c = start.rect.center();
        let g = drag(start, Move, c, p(c.x - 0.2, c.y), None);
        assert!(near(g.rect.x0, 0.15) && near(g.rect.y0, 0.35), "{:?}", g.rect);
        let (a, b) = (drag(start, Move, c, p(c.x - 0.6, c.y), None), drag(start, Move, c, p(c.x - 1.0, c.y), None));
        for g in [a, b] {
            assert!(g.is_within_image(W, H) && g.rect.x0 < 0.01, "{:?}", g.rect);
        }
        assert!(b.rect.y0 > a.rect.y0 + 0.01 && a.rect.y0 > start.rect.y0, "{:?} then {:?}", a.rect, b.rect);
    }

    //   Scenario: a free corner dragged diagonally out on a straightened photo (issue #2308)
    #[test]
    fn given_straightened_photo_when_a_free_corner_is_dragged_out_diagonally_then_both_sides_grow() {
        for angle in [6.0, 12.0] {
            let start = CropGeometry { rect: Rect::new(0.3, 0.3, 0.6, 0.6), angle };
            let g = drag(start, BottomRight, p(0.6, 0.6), p(1.2, 1.2), None);
            assert!(g.is_within_image(W, H), "{angle}: {:?}", g.rect);
            assert!(near(g.rect.x0, 0.3) && near(g.rect.y0, 0.3), "{angle}: the anchor corner stays: {:?}", g.rect);
            // both sides grow (main stops at y1 0.600)
            assert!(g.rect.x1 > 0.9 && g.rect.y1 > 0.9, "{angle}: one side only: {:?}", g.rect);
            let further = drag(g, BottomRight, p(g.rect.x1, g.rect.y1), p(1.2, 1.2), None);
            assert!((further.rect.x1 - g.rect.x1).abs() < 1e-4 && (further.rect.y1 - g.rect.y1).abs() < 1e-4, "{angle}: {:?}", further.rect);
        }
        let start = CropGeometry { rect: Rect::new(0.3, 0.3, 0.6, 0.6), angle: 8.0 };
        for (hd, from, to) in [(TopLeft, p(0.3, 0.3), p(-0.5, -0.5)), (TopRight, p(0.6, 0.3), p(1.5, -0.5)), (BottomLeft, p(0.3, 0.6), p(-0.5, 1.5))]
        {
            let g = drag(start, hd, from, to, None);
            assert!(g.is_within_image(W, H), "{hd:?}: {:?}", g.rect);
            assert!(g.rect.width() > 0.5 && g.rect.height() > 0.45, "{hd:?}: one side only: {:?}", g.rect);
            let (ax, ay) = (if from.x < 0.5 { g.rect.x1 } else { g.rect.x0 }, if from.y < 0.5 { g.rect.y1 } else { g.rect.y0 });
            assert!(near(ax, if from.x < 0.5 { 0.6 } else { 0.3 }) && near(ay, if from.y < 0.5 { 0.6 } else { 0.3 }), "{hd:?}: {:?}", g.rect);
        }
    }

    //   Scenario: a free corner pushed along one axis past a tilted edge on a straightened photo
    #[test]
    fn given_straightened_photo_when_a_free_corner_is_pushed_sideways_past_an_edge_then_the_other_side_keeps_its_size() {
        for angle in [6.0, 12.0, -8.0] {
            let start = CropGeometry { rect: Rect::new(0.3, 0.3, 0.6, 0.6), angle };
            let g = drag(start, BottomRight, p(0.6, 0.6), p(1.3, 0.6), None);
            assert!(g.is_within_image(W, H), "{angle}: {:?}", g.rect);
            assert!(near(g.rect.y1, 0.6) && near(g.rect.x0, 0.3) && near(g.rect.y0, 0.3), "{angle}: the height changed: {:?}", g.rect);
            assert!(g.rect.x1 > 0.85, "{angle}: it stopped short of the edge: {:?}", g.rect);
            let g = drag(start, BottomRight, p(0.6, 0.6), p(0.6, 1.3), None);
            assert!(near(g.rect.x1, 0.6) && g.rect.y1 > 0.8, "{angle}: the width changed: {:?}", g.rect);
        }
    }

    //   Scenario: a free corner of a crop under the minimum size on a straightened photo
    #[test]
    fn given_straightened_photo_when_a_corner_of_a_too_small_crop_is_dragged_then_it_reaches_the_minimum() {
        let start = CropGeometry { rect: Rect::new(0.3, 0.3, 0.3 + 2.0 / W, 0.6), angle: 6.0 };
        let g = drag(start, BottomRight, p(start.rect.x1, 0.6), p(start.rect.x1 + 2.0 / W, 0.62), None);
        assert!(g.rect.width() * W >= MIN_CROP_FRACTION * H - EPS, "{:?}", g.rect);
        assert!(g.is_within_image(W, H) && near(g.rect.x0, 0.3) && near(g.rect.y0, 0.3), "{:?}", g.rect);
    }

    //   Scenario: a free corner moved inside, or across its anchor, on a straightened photo
    #[test]
    fn given_straightened_photo_when_a_free_corner_moves_inside_then_it_follows_and_keeps_a_minimum_size() {
        let start = CropGeometry { rect: Rect::new(0.3, 0.3, 0.6, 0.6), angle: 8.0 };
        let g = drag(start, BottomRight, p(0.6, 0.6), p(0.7, 0.55), None);
        assert!(near(g.rect.x1, 0.7) && near(g.rect.y1, 0.55) && near(g.rect.x0, 0.3) && near(g.rect.y0, 0.3), "{:?}", g.rect);
        let z = drag(start, BottomRight, p(0.6, 0.6), p(0.6, 0.6), None).rect;
        assert!(near(z.x1, 0.6) && near(z.y1, 0.6), "a click moves nothing: {z:?}");
        let crossed = drag(start, BottomRight, p(0.6, 0.6), p(0.0, 0.0), None);
        assert!(near(crossed.rect.x0, 0.3) && near(crossed.rect.y0, 0.3), "{:?}", crossed.rect);
        assert!(
            crossed.rect.width() * W >= MIN_CROP_FRACTION * H - EPS && crossed.rect.height() * H >= MIN_CROP_FRACTION * H - EPS,
            "{:?}",
            crossed.rect
        );
    }
}
