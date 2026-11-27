//! The Remove tool's strokes on the photo (issue #2359): when a brush stroke records a point, the outline of
//! a stroke (the boundary of the union of its discs, not one ring per point) and what counts as grabbing it.
//! Pure geometry in screen points, so it is tested without a window.

use std::collections::HashMap;
use std::f32::consts::TAU;

use egui::Pos2;

/// Fewest and most samples on one circle: scaled with the radius so a ring stays round at any zoom.
const SAMPLES: (f32, f32) = (24.0, 128.0);

/// Whether a pointer at `q` adds a point to a stroke whose last stored point is `last`, in screen points, for a brush
/// of radius `r`. A point comes every `r / 4` (at least 2 points), like the mask brush's dabs, so a held or slowly
/// dragged pointer stores nothing more. On release (`end`) the stroke only has to reach `q` within `r / 8`.
pub fn adds_point(last: Pos2, q: Pos2, r: f32, end: bool) -> bool {
    let d = last.distance(q);
    let min = if end { r / 8.0 } else { (r / 4.0).max(2.0) };
    d.is_finite() && d > 0.0 && d >= min
}

/// The distance from `q` to the nearest of `centres`, when it is within `r` (the stroke can be grabbed there).
pub fn grab_distance(centres: &[Pos2], q: Pos2, r: f32) -> Option<f32> {
    let d = centres.iter().map(|c| c.distance(q)).filter(|d| d.is_finite()).min_by(f32::total_cmp)?;
    (d < r).then_some(d)
}

/// The boundary of the union of the discs of radius `r` at `centres`, as polylines of the arcs no other disc covers.
/// One disc, or many at the same place, gives one closed polyline (first point repeated at the end). Where discs
/// overlap, each circle contributes the arcs outside the others, from crossing to crossing, so the pieces meet and
/// are chained. Cost grows with the number of centres within `2r` of each other, so centres closer than `r / 16` to the
/// previous kept one (a held pointer in a stroke saved before points were spaced) don't count.
pub fn union_outline(centres: &[Pos2], r: f32) -> Vec<Vec<Pos2>> {
    if !r.is_finite() || r <= 0.0 {
        return Vec::new();
    }
    let mut cs: Vec<Pos2> = Vec::new();
    for &c in centres {
        if c.x.is_finite() && c.y.is_finite() && cs.last().is_none_or(|l| l.distance(c) >= r / 16.0) {
            cs.push(c);
        }
    }
    // a hostile stroke (thousands of points) is drawn from a subset: the outline stays within r/16 · stride of it
    const MAX_CENTRES: usize = 1024;
    if cs.len() > MAX_CENTRES {
        let stride = cs.len().div_ceil(MAX_CENTRES);
        cs = cs.into_iter().step_by(stride).collect();
    }
    let same2 = (r * 0.01) * (r * 0.01);
    // samples round a full circle: an arc gets the same angular step
    let step = TAU / (r * 0.5).clamp(SAMPLES.0, SAMPLES.1).floor();
    let (mut out, mut arcs) = (Vec::new(), Vec::new());
    for (i, &c) in cs.iter().enumerate() {
        let d2 = |o: &Pos2| (o.x - c.x).powi(2) + (o.y - c.y).powi(2);
        // a circle on top of an earlier one adds nothing
        if cs[..i].iter().any(|o| d2(o) <= same2) {
            continue;
        }
        // the angles of this circle that another one covers: centred on the way to its centre, as wide as the
        // chord where they cross (equal radii: acos(d / 2r))
        let mut covered: Vec<(f32, f32)> = Vec::new();
        for o in cs.iter().filter(|o| d2(o) > same2 && d2(o) < 4.0 * r * r) {
            let (dx, dy) = (o.x - c.x, o.y - c.y);
            let half = (dx.hypot(dy) / (2.0 * r)).clamp(0.0, 1.0).acos();
            let mid = dy.atan2(dx).rem_euclid(TAU);
            let (lo, hi) = (mid - half, mid + half);
            if lo < 0.0 {
                covered.extend([(lo + TAU, TAU), (0.0, hi)]);
            } else if hi > TAU {
                covered.extend([(lo, TAU), (0.0, hi - TAU)]);
            } else {
                covered.push((lo, hi));
            }
        }
        covered.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut free: Vec<(f32, f32)> = Vec::new();
        let mut from = 0.0f32;
        for (lo, hi) in covered {
            if lo > from {
                free.push((from, lo));
            }
            from = from.max(hi);
        }
        if from < TAU {
            free.push((from, TAU));
        }
        // an arc that runs through angle 0 is one
        if let (true, Some(&(_, end))) = (free.len() > 1 && free[0].0 == 0.0, free.last())
            && end == TAU
        {
            let (_, head) = free.remove(0);
            if let Some(last) = free.last_mut() {
                last.1 = TAU + head;
            }
        }
        for (a, b) in free {
            let m = (((b - a) / step).ceil() as usize).max(1);
            let mut line: Vec<Pos2> = (0..=m)
                .map(|k| {
                    let t = a + (b - a) * k as f32 / m as f32;
                    Pos2::new(c.x + r * t.cos(), c.y + r * t.sin())
                })
                .collect();
            // a circle nothing covers is a ring, not an arc
            if b - a >= TAU {
                line[m] = line[0];
                out.push(line);
            } else if b - a > 1e-4 {
                arcs.push(line);
            }
        }
    }
    out.extend(chain(arcs, (r * 0.01).max(0.25)));
    out
}

/// Arcs joined end to start wherever they meet within `tol`, so the outline is a few long polylines (one closed one
/// for a stroke) and not a piece per circle, with proper joins instead of butt ends at every crossing.
fn chain(arcs: Vec<Vec<Pos2>>, tol: f32) -> Vec<Vec<Pos2>> {
    let cell = |p: Pos2| ((p.x / tol).floor() as i32, (p.y / tol).floor() as i32);
    let first = |i: usize| arcs[i][0];
    let last = |i: usize| arcs[i][arcs[i].len() - 1];
    let grid = |at: &dyn Fn(usize) -> Pos2| {
        let mut g: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for i in 0..arcs.len() {
            g.entry(cell(at(i))).or_default().push(i);
        }
        g
    };
    // an arc whose start is within `tol` of p, `ok` to take
    let find = |g: &HashMap<(i32, i32), Vec<usize>>, at: &dyn Fn(usize) -> Pos2, p: Pos2, ok: &dyn Fn(usize) -> bool| {
        let (cx, cy) = cell(p);
        (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dy| (cx.saturating_add(dx), cy.saturating_add(dy))))
            .find_map(|c| g.get(&c)?.iter().copied().find(|&j| ok(j) && at(j).distance(p) <= tol))
    };
    let (by_start, by_end) = (grid(&first), grid(&last));
    // chains begin where no arc ends; whatever is left over is a cycle
    let heads: Vec<usize> = (0..arcs.len()).filter(|&i| find(&by_end, &last, first(i), &|j| j != i).is_none()).collect();
    let mut used = vec![false; arcs.len()];
    let mut out = Vec::new();
    for i in heads.into_iter().chain(0..arcs.len()) {
        if std::mem::replace(&mut used[i], true) {
            continue;
        }
        let mut line = arcs[i].clone();
        while let Some(j) = line.last().and_then(|&end| find(&by_start, &first, end, &|j| !used[j])) {
            used[j] = true;
            line.extend(arcs[j].iter().skip(1));
        }
        // a chain that came back to its start is closed
        if let (Some(&a), Some(&b)) = (line.first(), line.last())
            && line.len() > 2
            && a.distance(b) <= tol
        {
            line.push(a);
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn length(lines: &[Vec<Pos2>]) -> f32 {
        lines.iter().map(|l| l.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>()).sum()
    }

    fn outside_all(lines: &[Vec<Pos2>], centres: &[Pos2], r: f32) -> bool {
        lines.iter().flatten().all(|p| centres.iter().all(|c| c.distance(*p) >= r * (1.0 - 1e-3)))
    }

    #[test]
    fn one_disc_is_one_closed_circle() {
        let (c, r) = (Pos2::new(50.0, 40.0), 30.0);
        let lines = union_outline(&[c], r);
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        assert_eq!(l.first(), l.last(), "closed");
        assert!(l.iter().all(|p| (p.distance(c) - r).abs() < 1e-3));
        assert!((length(&lines) - TAU * r).abs() < TAU * r * 0.01, "{}", length(&lines));
    }

    #[test]
    fn many_identical_points_are_one_circle() {
        let (c, r) = (Pos2::new(10.0, 10.0), 20.0);
        let mut pts = vec![c; 40];
        pts.extend([Pos2::new(10.0, 10.0); 5]);
        assert_eq!(union_outline(&pts, r), union_outline(&[c], r));
        // the same place reached again later in the stroke adds nothing either
        let back = [c, Pos2::new(60.0, 10.0), c];
        assert_eq!(union_outline(&back, r).len(), 2, "two separate circles");
    }

    #[test]
    fn two_overlapping_discs_leave_no_sample_inside_the_other() {
        let r = 40.0;
        let cs = [Pos2::new(0.0, 0.0), Pos2::new(r, 0.0)];
        let lines = union_outline(&cs, r);
        assert_eq!(lines.len(), 1, "one closed outline, its two arcs joined at the crossings");
        assert!(lines[0][0].distance(lines[0][lines[0].len() - 1]) < 1e-3, "closed");
        assert!(outside_all(&lines, &cs, r));
        // each circle keeps 240° of its 360° when the centres are r apart
        let want = 2.0 * TAU * r * (240.0 / 360.0);
        assert!((length(&lines) - want).abs() < want * 0.02, "{} vs {want}", length(&lines));
        // the crossings of the two circles are on the outline
        let h = r * 3f32.sqrt() / 2.0;
        for y in [-h, h] {
            assert!(lines[0].iter().any(|p| p.distance(Pos2::new(r / 2.0, y)) < 0.5), "crossing at y={y}");
        }
    }

    #[test]
    fn a_stroke_is_one_outline_around_its_path() {
        let r = 30.0;
        // a straight stroke at r/4 spacing
        let cs: Vec<Pos2> = (0..40).map(|i| Pos2::new(i as f32 * r / 4.0, 0.0)).collect();
        let lines = union_outline(&cs, r);
        assert!(outside_all(&lines, &cs, r));
        // a stadium: two straight sides of the stroke's length and the two caps
        let want = 2.0 * 39.0 * r / 4.0 + TAU * r;
        assert!((length(&lines) - want).abs() < want * 0.03, "{} vs {want}", length(&lines));
        assert_eq!(lines.len(), 1, "the arcs of all the circles are one closed line");
        // every outline point lies on a circle of the stroke
        assert!(lines.iter().flatten().all(|p| cs.iter().any(|c| (c.distance(*p) - r).abs() < 1e-2)));
    }

    /// A curved stroke's arcs are shorter than a sample step in places: the outline must still be one closed line.
    #[test]
    fn a_curved_stroke_is_one_closed_outline_without_gaps() {
        for r in [6.0, 31.0, 80.0, 400.0] {
            let cs: Vec<Pos2> = (0..70)
                .map(|i| {
                    let t = i as f32 * r / 4.0;
                    Pos2::new(t * 0.9, r * 1.5 * (t / (6.0 * r)).sin() + t * 0.1)
                })
                .collect();
            let lines = union_outline(&cs, r);
            assert_eq!(lines.len(), 1, "r={r}: {} pieces", lines.len());
            let l = &lines[0];
            assert!(l[0].distance(l[l.len() - 1]) < 1e-3, "r={r}: closed");
            assert!(outside_all(&lines, &cs, r), "r={r}");
            // no step along the line is longer than a sample step of its circle (plus one crossing's worth of slack)
            let step = TAU * r / (r * 0.5).clamp(SAMPLES.0, SAMPLES.1).floor();
            assert!(l.windows(2).all(|w| w[0].distance(w[1]) <= step * 1.01 + 0.5), "r={r}: gap in the outline");
        }
    }

    #[test]
    fn a_stationary_pointer_in_a_stroke_costs_nothing_extra() {
        let r = 30.0;
        let moving: Vec<Pos2> = (0..8).map(|i| Pos2::new(i as f32 * r / 4.0, 5.0)).collect();
        // every position held for 25 frames, as a stroke saved before points were spaced
        let held: Vec<Pos2> = moving.iter().flat_map(|p| std::iter::repeat_n(*p, 25)).collect();
        assert_eq!(union_outline(&held, r), union_outline(&moving, r));
    }

    #[test]
    fn hostile_input_gives_an_empty_or_finite_outline() {
        for r in [0.0, -3.0, f32::NAN, f32::INFINITY] {
            assert!(union_outline(&[Pos2::new(1.0, 1.0)], r).is_empty());
        }
        assert!(union_outline(&[], 10.0).is_empty());
        let lines = union_outline(&[Pos2::new(f32::NAN, 0.0), Pos2::new(5.0, 5.0), Pos2::new(f32::INFINITY, 1.0)], 10.0);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].iter().all(|p| p.x.is_finite() && p.y.is_finite()));
        let many: Vec<Pos2> = (0..50_000).map(|i| Pos2::new(i as f32 * 0.7, (i % 7) as f32)).collect();
        assert!(!union_outline(&many, 40.0).is_empty());
    }

    #[test]
    fn a_stroke_adds_points_a_quarter_radius_apart() {
        let (last, r) = (Pos2::new(100.0, 100.0), 40.0);
        let at = |dx: f32| Pos2::new(100.0 + dx, 100.0);
        assert!(!adds_point(last, last, r, false), "stationary");
        assert!(!adds_point(last, at(9.0), r, false), "closer than r/4");
        assert!(adds_point(last, at(10.0), r, false));
        // a small brush still needs 2 points of movement
        assert!(!adds_point(last, at(1.5), 4.0, false));
        assert!(adds_point(last, at(2.0), 4.0, false));
        // the release reaches where the pointer is once it moved r/8
        assert!(!adds_point(last, at(4.9), r, true));
        assert!(adds_point(last, at(5.0), r, true));
        assert!(!adds_point(last, last, r, true));
        assert!(!adds_point(last, Pos2::new(f32::NAN, 0.0), r, false));
    }

    #[test]
    fn a_stroke_is_grabbed_anywhere_within_a_radius_of_its_points() {
        let cs = [Pos2::new(0.0, 0.0), Pos2::new(20.0, 0.0), Pos2::new(40.0, 0.0)];
        assert_eq!(grab_distance(&cs, Pos2::new(40.0, 25.0), 30.0), Some(25.0));
        assert_eq!(grab_distance(&cs, Pos2::new(40.0, 30.0), 30.0), None);
        assert_eq!(grab_distance(&[], Pos2::new(0.0, 0.0), 30.0), None);
    }
}
