//! Upright: automatic perspective correction.
//!
//! 1. **Line segments** — our own gradient-based detector: pixels with a strong luminance gradient are grown into
//!    regions of consistent level-line orientation (seeded from the strongest gradients), and each elongated region
//!    is fitted with a segment (weighted principal axis).
//! 2. **Vanishing points** — near-vertical and near-horizontal segments are grouped; for each family a RANSAC over
//!    segment pairs finds the point (possibly at infinity) most segments point at, refined by weighted least squares
//!    (smallest eigenvector of `Σ w·l·lᵀ` over the inlier lines).
//! 3. **Correction** — a virtual camera rotation `K·R·K⁻¹` that sends the vertical vanishing direction to the image
//!    y axis (and, for Full, the horizontal one to the x axis, with the focal length estimated from their
//!    orthogonality when possible); Level is a pure rotation.
//!
//! Guided Upright solves the same problem from up to four user-drawn guides.
//!
//! Coordinates: centred, `(p − centre) / (long edge / 2)` of the lens-corrected image, so results are independent
//! of the analysis resolution. Results map lens-corrected → transformed coordinates.

use std::borrow::Cow;
use std::sync::Mutex;

use lightcraft_develop::{Crop, DevelopSettings, Geometry, Upright};
use lightcraft_geom::{Homography, Point};
use lightcraft_raster::{Plane, Rgb32f};

use crate::SourceInfo;

use crate::transform::{DEFAULT_FOCAL, Mat3, recentre, rotation_homography};

/// A detected (or user-drawn) line segment in centred coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub a: Point,
    pub b: Point,
}

impl Segment {
    pub fn len(&self) -> f64 {
        self.a.dist(self.b)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0.0
    }
    pub(crate) fn mid(&self) -> Point {
        self.a.lerp(self.b, 0.5)
    }
    /// Homogeneous line through the endpoints, normalized so (a, b) is a unit normal.
    fn line(&self) -> [f64; 3] {
        let l = cross([self.a.x, self.a.y, 1.0], [self.b.x, self.b.y, 1.0]);
        let n = l[0].hypot(l[1]).max(1e-12);
        [l[0] / n, l[1] / n, l[2] / n]
    }
    /// Angle from vertical in degrees (0 = vertical), folded to 0..90.
    pub(crate) fn tilt_from_vertical(&self) -> f64 {
        let (dx, dy) = (self.b.x - self.a.x, self.b.y - self.a.y);
        dx.abs().atan2(dy.abs()).to_degrees()
    }
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f64; 3]) -> [f64; 3] {
    let n = dot(a, a).sqrt().max(1e-300);
    [a[0] / n, a[1] / n, a[2] / n]
}

// ------------------------------------------------------------------------------------------ segments

/// Detect line segments in a luminance plane (any scale; values roughly 0..1, perceptual). Returns segments in
/// centred coordinates of the plane.
pub fn detect_segments(lum: &Plane) -> Vec<Segment> {
    let (w, h) = (lum.width, lum.height);
    if w < 16 || h < 16 {
        return Vec::new();
    }
    let img = lightcraft_raster::blur::gaussian(lum, 0.8);
    let at = |x: usize, y: usize| img.data[y * w + x];
    let n = w * h;
    let mut mag = vec![0.0f32; n];
    let mut ang = vec![0.0f32; n]; // level-line angle (gradient + 90°), radians, mod π
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let gx = (at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x - 1, y) + at(x - 1, y + 1));
            let gy = (at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x, y - 1) + at(x + 1, y - 1));
            let m = gx.hypot(gy) / 8.0;
            mag[y * w + x] = m;
            ang[y * w + x] = (gy.atan2(gx) + std::f32::consts::FRAC_PI_2).rem_euclid(std::f32::consts::PI);
        }
    }
    // adaptive threshold: strong edges relative to the image's own contrast
    let mut sorted: Vec<f32> = mag.iter().copied().filter(|m| *m > 1e-4).collect();
    if sorted.len() < 32 {
        return Vec::new();
    }
    sorted.sort_by(f32::total_cmp);
    let p90 = sorted[sorted.len() * 9 / 10];
    // a quarter of the 90th percentile: a post's edge against foliage stays in
    let thr = (p90 * 0.25).max(0.008);
    let mut order: Vec<u32> = (0..n as u32).filter(|&i| mag[i as usize] > thr).collect();
    order.sort_by(|&a, &b| mag[b as usize].total_cmp(&mag[a as usize]));
    let mut used = vec![false; n];
    let tol = 22.5f32.to_radians();
    let min_len = 0.04 * w.max(h) as f64;
    let half_long = w.max(h) as f64 / 2.0;
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let mut out = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut region: Vec<usize> = Vec::new();
    for &seed in &order {
        let seed = seed as usize;
        if used[seed] {
            continue;
        }
        used[seed] = true;
        region.clear();
        region.push(seed);
        stack.clear();
        stack.push(seed);
        // region angle via doubled-angle mean
        let (mut sc, mut ss) = ((2.0 * ang[seed]).cos(), (2.0 * ang[seed]).sin());
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            let ra = ss.atan2(sc) / 2.0;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 1 || ny < 1 || nx >= w as i32 - 1 || ny >= h as i32 - 1 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if used[j] || mag[j] <= thr {
                        continue;
                    }
                    let mut d = (ang[j] - ra).abs() % std::f32::consts::PI;
                    if d > std::f32::consts::FRAC_PI_2 {
                        d = std::f32::consts::PI - d;
                    }
                    if d > tol {
                        continue;
                    }
                    used[j] = true;
                    region.push(j);
                    stack.push(j);
                    sc += (2.0 * ang[j]).cos();
                    ss += (2.0 * ang[j]).sin();
                }
            }
        }
        if region.len() < 12 {
            continue;
        }
        // weighted principal axis
        let (mut sw, mut mx, mut my) = (0.0f64, 0.0f64, 0.0f64);
        for &i in &region {
            let wt = mag[i] as f64;
            sw += wt;
            mx += wt * (i % w) as f64;
            my += wt * (i / w) as f64;
        }
        mx /= sw;
        my /= sw;
        let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
        for &i in &region {
            let wt = mag[i] as f64;
            let (dx, dy) = ((i % w) as f64 - mx, (i / w) as f64 - my);
            sxx += wt * dx * dx;
            syy += wt * dy * dy;
            sxy += wt * dx * dy;
        }
        let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
        let (ux, uy) = (theta.cos(), theta.sin());
        let (mut lo, mut hi, mut wlo, mut whi) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for &i in &region {
            let (dx, dy) = ((i % w) as f64 - mx, (i / w) as f64 - my);
            let t = dx * ux + dy * uy;
            let s = -dx * uy + dy * ux;
            lo = lo.min(t);
            hi = hi.max(t);
            wlo = wlo.min(s);
            whi = whi.max(s);
        }
        let (len, width) = (hi - lo, (whi - wlo).max(1.0));
        if len < min_len || len / width < 5.0 {
            continue;
        }
        let a = Point::new((mx + 0.5 + ux * lo - cx) / half_long, (my + 0.5 + uy * lo - cy) / half_long);
        let b = Point::new((mx + 0.5 + ux * hi - cx) / half_long, (my + 0.5 + uy * hi - cy) / half_long);
        out.push(Segment { a, b });
    }
    out
}

// ------------------------------------------------------------------------------------------ vanishing points

/// Angular error (radians) between a segment and the direction from its midpoint to the (homogeneous) point `v`.
pub(crate) fn vp_error(s: &Segment, v: [f64; 3]) -> f64 {
    let m = s.mid();
    let (dx, dy) = (v[0] - m.x * v[2], v[1] - m.y * v[2]);
    let (sx, sy) = (s.b.x - s.a.x, s.b.y - s.a.y);
    let d = dx.hypot(dy);
    if d < 1e-12 {
        return 0.0;
    }
    let c = ((dx * sx + dy * sy) / (d * sx.hypot(sy).max(1e-12))).abs().min(1.0);
    c.acos()
}

/// Smallest eigenvector of a symmetric 3×3 matrix (Jacobi rotations).
fn smallest_eigvec(m: Mat3) -> [f64; 3] {
    let mut a = m;
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..50 {
        let (mut p, mut q, mut big) = (0, 1, 0.0);
        for i in 0..3 {
            for j in i + 1..3 {
                if a[i][j].abs() > big {
                    big = a[i][j].abs();
                    p = i;
                    q = j;
                }
            }
        }
        if big < 1e-15 {
            break;
        }
        let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
        let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
        let t = if theta == 0.0 { 1.0 } else { t };
        let c = 1.0 / (t * t + 1.0).sqrt();
        let s = t * c;
        for k in 0..3 {
            let (akp, akq) = (a[k][p], a[k][q]);
            a[k][p] = c * akp - s * akq;
            a[k][q] = s * akp + c * akq;
        }
        for k in 0..3 {
            let (apk, aqk) = (a[p][k], a[q][k]);
            a[p][k] = c * apk - s * aqk;
            a[q][k] = s * apk + c * aqk;
        }
        for row in v.iter_mut() {
            let (vp, vq) = (row[p], row[q]);
            row[p] = c * vp - s * vq;
            row[q] = s * vp + c * vq;
        }
    }
    let i = (0..3).min_by(|&i, &j| a[i][i].total_cmp(&a[j][j])).unwrap_or(0);
    norm([v[0][i], v[1][i], v[2][i]])
}

/// Least-squares vanishing point of `segs` (weights = lengths).
fn refine_vp(segs: &[&Segment]) -> [f64; 3] {
    let mut m = [[0.0; 3]; 3];
    for s in segs {
        let l = s.line();
        let w = s.len();
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += w * l[i] * l[j];
            }
        }
    }
    smallest_eigvec(m)
}

/// The dominant vanishing point of a segment family: RANSAC over pairs, then least squares on the inliers.
/// Returns the point and the total inlier length, or None with fewer than two consistent segments.
pub fn vanishing_point(segs: &[Segment]) -> Option<([f64; 3], f64)> {
    vanishing_point_where(segs, |_| true)
}

/// [`vanishing_point`] among the candidate points `ok` accepts.
fn vanishing_point_where(segs: &[Segment], ok: impl Fn([f64; 3]) -> bool) -> Option<([f64; 3], f64)> {
    if segs.len() < 2 {
        return None;
    }
    let thr = 1.5f64.to_radians();
    let score = |v: [f64; 3]| -> f64 { segs.iter().map(|s| if vp_error(s, v) < thr { s.len() } else { 0.0 }).sum() };
    // deterministic pair sampling, longest segments first
    let mut idx: Vec<usize> = (0..segs.len()).collect();
    idx.sort_by(|&a, &b| segs[b].len().total_cmp(&segs[a].len()));
    idx.truncate(60);
    let mut best: Option<([f64; 3], f64)> = None;
    for (k, &i) in idx.iter().enumerate() {
        for &j in &idx[k + 1..] {
            let v = cross(segs[i].line(), segs[j].line());
            if dot(v, v) < 1e-24 {
                continue;
            }
            let v = norm(v);
            if !ok(v) {
                continue;
            }
            let sc = score(v);
            if best.is_none_or(|b| sc > b.1) {
                best = Some((v, sc));
            }
        }
    }
    let (v, _) = best?;
    let inl: Vec<&Segment> = segs.iter().filter(|s| vp_error(s, v) < thr * 2.0).collect();
    if inl.len() < 2 {
        return None;
    }
    // refined, the point must still be one `ok` accepts (else the candidate it came from stands)
    let v = Some(refine_vp(&inl)).filter(|r| ok(*r)).unwrap_or(v);
    let total: f64 = segs.iter().filter(|s| vp_error(s, v) < thr * 2.0).map(|s| s.len()).sum();
    let count = segs.iter().filter(|s| vp_error(s, v) < thr * 2.0).count();
    (count >= 2).then_some((v, total))
}

// ------------------------------------------------------------------------------------------ solving

/// Direction (camera frame) of a vanishing point for focal length `f`.
fn direction(v: [f64; 3], f: f64) -> [f64; 3] {
    norm([v[0] / f, v[1] / f, v[2]])
}

/// The rotation (rows) taking unit vector `a` to unit vector `b` (Rodrigues; minimal angle).
fn rotation_between(a: [f64; 3], b: [f64; 3]) -> Mat3 {
    let axis = cross(a, b);
    let s = dot(axis, axis).sqrt();
    let c = dot(a, b);
    if s < 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let k = [axis[0] / s, axis[1] / s, axis[2] / s];
    let kx = [[0.0, -k[2], k[1]], [k[2], 0.0, -k[0]], [-k[1], k[0], 0.0]];
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let kk: f64 = (0..3).map(|m| kx[i][m] * kx[m][j]).sum();
            r[i][j] = if i == j { 1.0 } else { 0.0 } + s * kx[i][j] + (1.0 - c) * kk;
        }
    }
    r
}

fn rotation_angle(r: &Mat3) -> f64 {
    ((r[0][0] + r[1][1] + r[2][2] - 1.0) / 2.0).clamp(-1.0, 1.0).acos()
}

/// Vertical correction: send the vertical vanishing direction to the image y axis.
fn solve_vertical(v: [f64; 3], f: f64) -> Mat3 {
    let mut d = direction(v, f);
    if d[1] < 0.0 {
        d = [-d[0], -d[1], -d[2]];
    }
    rotation_between(d, [0.0, 1.0, 0.0])
}

/// Full correction: vertical direction → y, horizontal direction → x (Gram–Schmidt).
fn solve_full(v: [f64; 3], hz: [f64; 3], f: f64) -> Mat3 {
    let mut dv = direction(v, f);
    if dv[1] < 0.0 {
        dv = [-dv[0], -dv[1], -dv[2]];
    }
    let mut dh = direction(hz, f);
    if dh[0] < 0.0 {
        dh = [-dh[0], -dh[1], -dh[2]];
    }
    let p = dot(dh, dv);
    let r1 = norm([dh[0] - p * dv[0], dh[1] - p * dv[1], dh[2] - p * dv[2]]);
    let r3 = cross(r1, dv);
    [r1, dv, r3]
}

/// Focal length (centred units) that makes two finite vanishing points orthogonal, when plausible.
fn focal_from_vps(v: [f64; 3], h: [f64; 3]) -> Option<f64> {
    if v[2].abs() < 1e-9 || h[2].abs() < 1e-9 {
        return None;
    }
    let (vx, vy, hx, hy) = (v[0] / v[2], v[1] / v[2], h[0] / h[2], h[1] / h[2]);
    let f2 = -(vx * hx + vy * hy);
    (f2 > 0.0).then(|| f2.sqrt()).filter(|f| (0.5..=6.0).contains(f))
}

/// Pure rotation (degrees, clockwise +) that levels the dominant lines.
fn level_angle(vert: &[Segment], horiz: &[Segment]) -> Option<f64> {
    level_angle_within(vert, horiz, 90.0, 1)
}

/// [`level_angle`] that only answers for at most `max` degrees, backed by at least `min_lines`
/// agreeing segments.
pub(crate) fn level_angle_within(vert: &[Segment], horiz: &[Segment], max: f64, min_lines: usize) -> Option<f64> {
    let dev = |s: &Segment, vertical: bool| {
        let (dx, dy) = (s.b.x - s.a.x, s.b.y - s.a.y);
        // signed deviation in degrees of the segment from the axis
        let a = if vertical { dx.atan2(dy) } else { dy.atan2(dx) };
        let a = a.to_degrees();
        let a = if a > 90.0 {
            a - 180.0
        } else if a < -90.0 {
            a + 180.0
        } else {
            a
        };
        if vertical { -a } else { a }
    };
    let lh: f64 = horiz.iter().map(Segment::len).sum();
    let lv: f64 = vert.iter().map(Segment::len).sum();
    let (set, vertical) = if lh >= lv * 0.5 && lh > 0.0 { (horiz, false) } else { (vert, true) };
    if set.is_empty() {
        return None;
    }
    // length-weighted median
    let mut v: Vec<(f64, f64)> = set.iter().map(|s| (dev(s, vertical), s.len())).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = v.iter().map(|x| x.1).sum();
    let mut acc = 0.0;
    let median = v.iter().find(|(_, l)| {
        acc += l;
        acc >= total / 2.0
    })?;
    // consensus: most of the line length must agree with the median within 2°, otherwise the "lines" are scene
    // texture (tree flanks, ridges) rather than a horizon or plumb lines
    let agreeing: Vec<_> = v.iter().filter(|(d, _)| (d - median.0).abs() <= 2.0).collect();
    let agree: f64 = agreeing.iter().map(|x| x.1).sum();
    (agree >= 0.5 * total && agreeing.len() >= min_lines && median.0.abs() <= max).then_some(-median.0)
}

fn rot_z_h(deg: f64) -> Homography {
    let (s, c) = deg.to_radians().sin_cos();
    Homography([c, -s, 0.0, s, c, 0.0, 0.0, 0.0, 1.0])
}

/// What an Upright analysis found.
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub vertical: Vec<Segment>,
    pub horizontal: Vec<Segment>,
    pub vertical_vp: Option<[f64; 3]>,
    pub horizontal_vp: Option<[f64; 3]>,
    /// Why there is no vertical vanishing point (when there is none).
    pub vertical_why: Option<Reject>,
    /// Why there is no horizontal vanishing point (when there is none).
    pub horizontal_why: Option<Reject>,
}

/// Why a family of lines gives no perspective to correct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// Fewer than two long lines of the family.
    TooFew,
    /// Too little of them: short lines only.
    TooShort,
    /// All on one object: not spread across the photo.
    OneObject,
    /// They point in different directions (trees, people, folds), not at one vanishing point.
    Disagree,
    /// They meet inside or near the photo: a scene feature (a road, rays), not a tilted camera.
    MeetInside,
    /// Correcting them would turn the camera too far to be a tilt (Auto leaves it to Vertical).
    TooSteep,
    /// The lines it would level by (horizontal, else vertical) are tilted more than Auto levels (5°): usually
    /// intentional, or scene lines (Level fixes it anyway).
    TooTilted,
}

impl Reject {
    /// In words for the user: "No vertical correction: …".
    pub fn message(self) -> &'static str {
        match self {
            Reject::TooFew => "too few straight lines",
            Reject::TooShort => "the straight lines are too short",
            Reject::OneObject => "the straight lines are all on one object",
            Reject::Disagree => "the long lines lean different ways",
            Reject::MeetInside => "the lines meet inside the photo (a floor or a road, not a tilted camera)",
            Reject::TooSteep => "the lines lean too far to be a tilted camera (Vertical corrects them anyway)",
            Reject::TooTilted => "the lines are tilted too far to be a levelling fix (Level corrects them anyway)",
        }
    }
}

/// How far (degrees) the camera turns to send the vertical vanishing point `v` to the image's y axis.
fn tilt_degrees(v: [f64; 3]) -> f64 {
    let d = direction(v, DEFAULT_FOCAL);
    d[1].abs().clamp(0.0, 1.0).acos().to_degrees()
}

/// Whether a (homogeneous) vanishing point lies inside or near the photo (within twice the half long edge of
/// its centre): a scene feature rather than a camera tilt.
fn inside(v: [f64; 3]) -> bool {
    v[2].abs() > 1e-9 && (v[0] / v[2]).hypot(v[1] / v[2]) < 2.0
}

/// A long line (in half long edges: 7.5 % of the long edge), and how far apart (across the family) two of them
/// must be to vouch for a vanishing point. Tuned on a photographer's own Upright corrections.
const LONG_LINE: f64 = 0.15;
const LONG_APART: f64 = 0.25;
/// Camera turns (degrees) above which a vertical correction needs three long lines and half the long lines'
/// length agreeing, and above which it needs most of it.
const BIG_TILT: f64 = 10.0;
const HUGE_TILT: f64 = 20.0;

/// Whether a family's vanishing point is backed by real structure rather than one object: at least three
/// inlier segments, a total inlier length of a quarter of the long edge, and inliers spread across at least a
/// fifth of the long edge perpendicular to the family, and most of the family's long lines agreeing (the
/// mirrored flanks of conical trees or peaks converge somewhere but say nothing about the camera), or at least
/// the lines that disagree not gathering at a rival point (two posts flanking a crowd).
fn reliable(segs: &[Segment], v: [f64; 3], vertical: bool) -> Result<(), Reject> {
    let thr = 3f64.to_radians();
    let inl: Vec<&Segment> = segs.iter().filter(|s| vp_error(s, v) < thr).collect();
    let total: f64 = inl.iter().map(|s| s.len()).sum();
    let across = |s: &&Segment| if vertical { s.mid().x } else { s.mid().y };
    let (lo, hi) = inl.iter().map(across).fold((f64::MAX, f64::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)));
    // a camera-tilt vanishing point lies far outside the frame; one inside or near it is a scene feature
    // (sun rays, a road to the horizon), not perspective to correct
    if inside(v) {
        return Err(Reject::MeetInside);
    }
    if inl.len() < 3 {
        return Err(Reject::TooFew);
    }
    if total < 0.5 {
        return Err(Reject::TooShort);
    }
    if hi - lo < 0.4 {
        return Err(Reject::OneObject);
    }
    // real structure has long straight edges: at least two long lines, apart, agree (folds of a dress, hair
    // and foliage are short pieces that agree by chance)
    let long: Vec<f64> = inl.iter().filter(|s| s.len() >= LONG_LINE).map(across).collect();
    let spread = long.iter().fold((f64::MAX, f64::MIN), |(lo, hi), x| (lo.min(*x), hi.max(*x)));
    if long.len() < 2 || spread.1 - spread.0 < LONG_APART {
        return Err(Reject::TooShort);
    }
    // a big correction needs more than a few lines: three long lines and half the long lines'
    // length agreeing (the folds of a gown spreading to the floor point at a far point too)
    let tilt = if vertical { tilt_degrees(v) } else { 0.0 };
    // consensus: real structure agrees; mirrored flanks (trees, peaks) split the family in two halves that each
    // converge somewhere. Most of the family agreeing settles it; else the lines that disagree must not gather at
    // a rival point of their own nearly as strongly (people, folds and foliage point every which way, so a few
    // long posts among them still decide)
    let mut family: f64 = segs.iter().map(Segment::len).sum();
    // (judged on the long lines: a gown's many short folds don't outvote the posts around it)
    let mut long_family: f64 = segs.iter().filter(|s| s.len() >= LONG_LINE).map(Segment::len).sum();
    let long_total: f64 = inl.iter().filter(|s| s.len() >= LONG_LINE).map(|s| s.len()).sum();
    let share = if tilt > HUGE_TILT { 0.6 } else { 0.5 };
    let big = tilt > BIG_TILT;
    // Lines that meet inside or near the photo are scene features (floorboards running to the horizon, the
    // apex of a drape, rays), never a camera tilt: they neither vote against the posts nor make a rival
    // (the posts of a gazebo were outvoted by its deck and its curtains), and the big-tilt test below
    // doesn't count them either.
    let mut rival = 0.0;
    if big || total < 0.6 * family {
        let mut rest: Vec<Segment> = segs.iter().copied().filter(|s| vp_error(s, v) >= thr).collect();
        for _ in 0..4 {
            let Some((r, support)) = vanishing_point(&rest) else { break };
            if !inside(r) {
                rival = support;
                break;
            }
            let (scene, other): (Vec<Segment>, Vec<Segment>) = rest.iter().partition(|s| vp_error(s, r) < thr);
            if scene.is_empty() {
                break;
            }
            family -= scene.iter().map(Segment::len).sum::<f64>();
            long_family -= scene.iter().filter(|s| s.len() >= LONG_LINE).map(Segment::len).sum::<f64>();
            rest = other;
        }
    }
    if big && (long.len() < 3 || long_total < share * long_family) {
        return Err(Reject::Disagree);
    }
    if total >= 0.6 * family {
        return Ok(());
    }
    if total >= 0.3 * family && total >= 2.0 * rival {
        return Ok(());
    }
    // most of the long lines agreeing settles it too (posts round a gown and its folds)
    if long_total >= 0.6 * long_family {
        return Ok(());
    }
    Err(Reject::Disagree)
}

/// Join the pieces of one straight edge (a post's side broken by a person in front of it, a branch, a
/// highlight) into one line, so a long structural edge counts as long: pieces on the same line (within 1.5°
/// and about 3 px at the analysis size) with a gap of at most a quarter of the half long edge between them.
fn merge_collinear(mut segs: Vec<Segment>) -> Vec<Segment> {
    segs.sort_by(|a, b| b.len().total_cmp(&a.len()));
    // each line: its first (longest) piece as the reference, and every piece joined to it
    let mut lines: Vec<(Segment, Vec<Segment>)> = Vec::with_capacity(segs.len());
    for s in segs {
        let joined = lines.iter_mut().find(|(l, _)| collinear(l, &s));
        match joined {
            Some((l, pieces)) => {
                pieces.push(s);
                *l = fit_line(pieces).unwrap_or(*l);
            }
            None => lines.push((s, vec![s])),
        }
    }
    lines.into_iter().map(|(l, _)| l).collect()
}

/// Whether piece `s` lies on line `l` (within 1.5° and about 3 px at the analysis size) with a gap of at most a
/// quarter of the half long edge between them.
fn collinear(l: &Segment, s: &Segment) -> bool {
    let (dx, dy) = (l.b.x - l.a.x, l.b.y - l.a.y);
    let n = dx.hypot(dy);
    if n < 1e-9 {
        return false;
    }
    let (ux, uy) = (dx / n, dy / n);
    let (sx, sy) = (s.b.x - s.a.x, s.b.y - s.a.y);
    let sn = sx.hypot(sy).max(1e-12);
    if ((ux * sx + uy * sy) / sn).abs() < 1.5f64.to_radians().cos() {
        return false;
    }
    let off = |p: Point| (p.x - l.a.x) * -uy + (p.y - l.a.y) * ux;
    if off(s.a).abs() > 0.006 || off(s.b).abs() > 0.006 {
        return false;
    }
    let t = |p: Point| (p.x - l.a.x) * ux + (p.y - l.a.y) * uy;
    let (t0, t1) = (t(s.a).min(t(s.b)), t(s.a).max(t(s.b)));
    (t0 - n).max(-t1).max(0.0) <= 0.25
}

/// The line through pieces (their endpoints weighted by the piece's length, principal axis), from end to end.
fn fit_line(pieces: &[Segment]) -> Option<Segment> {
    let (mut sw, mut mx, mut my) = (0.0, 0.0, 0.0);
    for s in pieces {
        let w = s.len();
        for p in [s.a, s.b] {
            sw += w;
            mx += w * p.x;
            my += w * p.y;
        }
    }
    if sw <= 0.0 {
        return None;
    }
    let (mx, my) = (mx / sw, my / sw);
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for s in pieces {
        let w = s.len();
        for p in [s.a, s.b] {
            let (dx, dy) = (p.x - mx, p.y - my);
            sxx += w * dx * dx;
            syy += w * dy * dy;
            sxy += w * dx * dy;
        }
    }
    let th = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (ux, uy) = (th.cos(), th.sin());
    let t = |p: Point| (p.x - mx) * ux + (p.y - my) * uy;
    let (lo, hi) = pieces.iter().flat_map(|s| [t(s.a), t(s.b)]).fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    Some(Segment { a: Point::new(mx + ux * lo, my + uy * lo), b: Point::new(mx + ux * hi, my + uy * hi) })
}

/// One family's vanishing point, if [`reliable`], else why not.
fn family_vp(segs: &[Segment], vertical: bool) -> (Option<[f64; 3]>, Option<Reject>) {
    // a camera tilt puts the point outside the photo: look there first, so lines meeting inside it (a deck
    // running to the horizon) can't take the place of the posts
    let found = vanishing_point_where(segs, |v| !inside(v)).or_else(|| vanishing_point(segs));
    match found {
        None => (None, Some(Reject::TooFew)),
        Some((v, _)) => match reliable(segs, v, vertical) {
            Ok(()) => (Some(v), None),
            Err(why) => (None, Some(why)),
        },
    }
}

/// Group segments into near-vertical / near-horizontal families and find their vanishing points (only reliable
/// ones; see [`reliable`]).
pub fn analyze(segs: &[Segment]) -> Analysis {
    let vertical = merge_collinear(segs.iter().copied().filter(|s| s.tilt_from_vertical() < 30.0).collect());
    let horizontal = merge_collinear(segs.iter().copied().filter(|s| s.tilt_from_vertical() > 60.0).collect());
    let (vertical_vp, vertical_why) = family_vp(&vertical, true);
    let (horizontal_vp, horizontal_why) = family_vp(&horizontal, false);
    Analysis { vertical, horizontal, vertical_vp, horizontal_vp, vertical_why, horizontal_why }
}

/// Why `mode` corrects less than it asks for on this analysis: `None` when it does what it says (or it is Off,
/// Guided or Auto with nothing to fix). In words for the user, after "No vertical correction: ".
pub fn shortfall(mode: Upright, a: &Analysis) -> Option<&'static str> {
    match mode {
        Upright::Vertical | Upright::Full if a.vertical_vp.is_none() => Some(a.vertical_why.unwrap_or(Reject::TooFew).message()),
        Upright::Full if a.horizontal_vp.is_none() => Some(a.horizontal_why.unwrap_or(Reject::TooFew).message()),
        Upright::Level if level_angle(&a.vertical, &a.horizontal).is_none() => {
            let why = if a.vertical.is_empty() && a.horizontal.is_empty() { Reject::TooFew } else { Reject::Disagree };
            Some(why.message())
        }
        Upright::Auto => auto(a).err().map(Reject::message),
        _ => None,
    }
}

/// What to draw in Guided when `mode` falls short ("Try Guided: draw …"). Guided works from the guides alone, not
/// from lines the mode found, and one guide of a kind only levels ([`solve_guided`]): Full needs two of each
/// family, Vertical two vertical ones, and levelling one line. Auto falls short either for its verticals (two
/// vertical guides) or for a horizon tilted more than it levels ([`Reject::TooTilted`]: one level guide), so it names
/// both.
pub fn guided_hint(mode: Upright) -> &'static str {
    match mode {
        Upright::Full => "two lines along what should be vertical and two along what should be horizontal",
        Upright::Level => "a line along what should be level or upright",
        Upright::Auto => "two lines along what should be vertical, or one line along what should be level",
        _ => "two lines along what should be vertical",
    }
}

/// The Upright correction for `mode` from an analysis (lens-corrected → transformed, centred coordinates).
pub fn solve(mode: Upright, a: &Analysis) -> Homography {
    let level = || level_angle(&a.vertical, &a.horizontal).map(rot_z_h).unwrap_or(Homography::IDENTITY);
    let vertical = || a.vertical_vp.map(|v| solve_vertical(v, DEFAULT_FOCAL));
    let full = || {
        let (v, h) = (a.vertical_vp?, a.horizontal_vp?);
        let f = focal_from_vps(v, h).unwrap_or(DEFAULT_FOCAL);
        Some((solve_full(v, h, f), f))
    };
    match mode {
        Upright::Off | Upright::Guided => Homography::IDENTITY,
        Upright::Level => level(),
        Upright::Vertical => vertical().map(|r| recentre(&rotation_homography(&r, DEFAULT_FOCAL))).unwrap_or_else(level),
        Upright::Full => match full() {
            Some((r, f)) => recentre(&rotation_homography(&r, f)),
            None => vertical().map(|r| recentre(&rotation_homography(&r, DEFAULT_FOCAL))).unwrap_or_else(level),
        },
        Upright::Auto => auto(a).unwrap_or(Homography::IDENTITY),
    }
}

/// What Auto does with an analysis: the correction it applies, or why it applies none. [`solve`] and
/// [`shortfall`] both ask it, so the note always matches the correction.
fn auto(a: &Analysis) -> Result<Homography, Reject> {
    // A vertical family that implies a camera roll must be confirmed by the horizontals (a real roll tilts
    // both); otherwise it is scene structure (e.g. the same-side flanks of many conical trees).
    if let Some(v) = a.vertical_vp {
        let (x, y) = if v[1] < 0.0 { (-v[0], -v[1]) } else { (v[0], v[1]) };
        let roll = x.atan2(y).to_degrees();
        let confirmed = roll.abs() <= 3.0 || level_angle(&[], &a.horizontal).is_some_and(|h| (h - roll).abs() <= 3.0);
        if !confirmed {
            let weaker = Analysis { vertical_vp: None, horizontal_vp: None, vertical_why: Some(Reject::Disagree), ..a.clone() };
            return auto(&weaker);
        }
    }
    // balanced: full correction only when it is moderate, else vertical, else level
    if let Some((v, h)) = a.vertical_vp.zip(a.horizontal_vp) {
        let f = focal_from_vps(v, h).unwrap_or(DEFAULT_FOCAL);
        let r = solve_full(v, h, f);
        if rotation_angle(&r) < 20f64.to_radians() {
            return Ok(recentre(&rotation_homography(&r, f)));
        }
    }
    let vertical = a.vertical_vp.map(|v| solve_vertical(v, DEFAULT_FOCAL));
    if let Some(r) = vertical.filter(|r| rotation_angle(r) < 25f64.to_radians()) {
        return Ok(recentre(&rotation_homography(&r, DEFAULT_FOCAL)));
    }
    // conservative: only small horizon fixes; bigger tilts are usually intentional or scene lines
    match level_angle(&a.vertical, &a.horizontal) {
        Some(d) if d.abs() <= 5.0 => Ok(rot_z_h(d)),
        _ if vertical.is_some() => Err(Reject::TooSteep),
        Some(_) => Err(Reject::TooTilted),
        _ => Err(a.vertical_why.unwrap_or(Reject::TooFew)),
    }
}

/// Guided Upright from user guides (centred coordinates of the lens-corrected image). Guides closer to vertical
/// define the vertical vanishing point, the others the horizontal one; one guide of a kind only levels.
pub fn solve_guided(guides: &[Segment]) -> Homography {
    let guides: Vec<Segment> = guides.iter().copied().filter(|g| g.len() > 1e-6).take(4).collect();
    let vert: Vec<Segment> = guides.iter().copied().filter(|g| g.tilt_from_vertical() <= 45.0).collect();
    let horiz: Vec<Segment> = guides.iter().copied().filter(|g| g.tilt_from_vertical() > 45.0).collect();
    let vp = |s: &[Segment]| -> Option<[f64; 3]> {
        if s.len() < 2 {
            return None;
        }
        let refs: Vec<&Segment> = s.iter().collect();
        Some(refine_vp(&refs))
    };
    let (v, h) = (vp(&vert), vp(&horiz));
    match (v, h) {
        (Some(v), Some(h)) => {
            let f = focal_from_vps(v, h).unwrap_or(DEFAULT_FOCAL);
            recentre(&rotation_homography(&solve_full(v, h, f), f))
        }
        (Some(v), None) => recentre(&rotation_homography(&solve_vertical(v, DEFAULT_FOCAL), DEFAULT_FOCAL)),
        (None, Some(_)) => {
            // two horizontal guides: make them parallel and horizontal (rotate the problem by 90°)
            let swap = |p: Point| Point::new(p.y, -p.x);
            let hs: Vec<Segment> = horiz.iter().map(|s| Segment { a: swap(s.a), b: swap(s.b) }).collect();
            let refs: Vec<&Segment> = hs.iter().collect();
            let r = solve_vertical(refine_vp(&refs), DEFAULT_FOCAL);
            let inner = rotation_homography(&r, DEFAULT_FOCAL);
            let to = Homography([0.0, 1.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0]); // (x, y) → (y, −x)
            let back = to.inverse().unwrap_or(Homography::IDENTITY);
            recentre(&back.mul(&inner).mul(&to))
        }
        (None, None) => level_angle(&vert, &horiz).map(rot_z_h).unwrap_or(Homography::IDENTITY),
    }
}

// ------------------------------------------------------------------------------------------ sources

/// Long edge of the analysis image.
const ANALYSIS_LONG: usize = 1024;

/// Analyse a source: the lens-corrected (optics applied, no perspective/crop) image at [`ANALYSIS_LONG`] px,
/// segment detection and vanishing points. Cached per source + optics settings.
pub fn analyze_source(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Analysis {
    static CACHE: Mutex<Vec<(u64, Analysis)>> = Mutex::new(Vec::new());
    let key = analysis_key(src, info, s);
    if let Ok(c) = CACHE.lock()
        && let Some((_, a)) = c.iter().find(|(k, _)| *k == key)
    {
        return a.clone();
    }
    let a = analyze(&detect_segments(&analysis_plane(src, info, s)));
    if let Ok(mut c) = CACHE.lock() {
        c.push((key, a.clone()));
        if c.len() > 8 {
            c.remove(0);
        }
    }
    a
}

/// [`analysis_plane`] at `long` px on its long edge (the evaluation examples read a larger one).
pub fn analysis_luminance(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, long: usize) -> Plane {
    let mut base = s.clone();
    base.geometry = Geometry::default();
    base.crop = Crop::default();
    let frame = crate::frame_for(src, info, &base, false);
    let (w, h) = frame.fit(long, long);
    let img = frame.sample(src, w, h);
    let lum = img.luminance();
    let mut v: Vec<f32> = lum.data.iter().copied().filter(|x| x.is_finite()).collect();
    v.sort_by(f32::total_cmp);
    let white = v.get(v.len() * 99 / 100).copied().unwrap_or(1.0).max(1e-4);
    lum.map(|x| (x.max(0.0) / white).min(1.5).powf(1.0 / 2.4))
}

/// Analyse an [`analysis_luminance`] plane of any size (brought to the analysis size first).
pub fn analyze_plane(lum: &Plane) -> Analysis {
    analyze_plane_at(lum, ANALYSIS_LONG)
}

/// [`analyze_plane`] at `size` px on the long edge (at most the plane's own).
pub fn analyze_plane_at(lum: &Plane, size: usize) -> Analysis {
    let long = lum.width.max(lum.height);
    if long > size {
        let fit = lightcraft_raster::resample::fit(lum, size, size, lightcraft_raster::resample::Filter::Box);
        return analyze(&detect_segments(&fit));
    }
    analyze(&detect_segments(lum))
}

/// What an analysis depends on: the source (by its sampled fingerprint), the optics and
/// orientation settings, whether the optics section is on, and the lens the frame is corrected for
/// (`info.lens`, which [`analysis_plane`] reads through the frame).
pub(crate) fn analysis_key(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut more = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", info.lens).hash(&mut more);
    s.section_enabled("optics").hash(&mut more);
    crate::optics::fingerprint(src)
        ^ lightcraft_develop::DevelopSettings { optics: s.optics, orientation: s.orientation, ..Default::default() }.hash64().rotate_left(17)
        ^ more.finish().rotate_left(29)
}

/// The image an analysis reads: the lens-corrected source (optics applied, no perspective or crop)
/// at [`ANALYSIS_LONG`] px, as perceptual lightness normalized to its bright end.
pub(crate) fn analysis_plane(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Plane {
    analysis_luminance(src, info, s, ANALYSIS_LONG)
}

/// The crop angle (degrees, clockwise +) Auto straighten levels `src` by, if it finds a horizon or
/// plumb lines it can trust ([`crate::straighten`]). Used by the crop panel's Auto straighten. It
/// reads the horizon and the segments from the same analysis image, built here (not through
/// [`analyze_source`]'s cache: it runs once per click).
pub fn level_degrees(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<f64> {
    crate::straighten::level_plane(&analysis_plane(src, info, s))
}

/// The automatic Upright correction for `mode` on `src` (identity for Off and Guided).
pub fn auto_transform(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, mode: Upright) -> Homography {
    if matches!(mode, Upright::Off | Upright::Guided) {
        return Homography::IDENTITY;
    }
    solve(mode, &analyze_source(src, info, s))
}

/// Fill in a missing automatic Upright transform (settings from XMP/CLI that name a mode but carry no analysis).
pub fn resolve<'a>(src: &Rgb32f, info: &SourceInfo, s: &'a DevelopSettings) -> Cow<'a, DevelopSettings> {
    let g = &s.geometry;
    if matches!(g.upright, Upright::Off | Upright::Guided) || g.upright_transform.is_some() || !s.section_enabled("geometry") {
        return Cow::Borrowed(s);
    }
    let mut e = s.clone();
    e.geometry.upright_transform = Some(auto_transform(src, info, s, g.upright));
    Cow::Owned(e)
}

/// The Upright homography in effect for settings (stored automatic transform, or solved from guides).
/// `ow × oh` is the oriented image size (guides are normalized to it).
pub fn homography(g: &Geometry, ow: f64, oh: f64) -> Homography {
    match g.upright {
        Upright::Off => Homography::IDENTITY,
        Upright::Guided => {
            let l = ow.max(oh) / 2.0;
            let c = |p: Point| Point::new((p.x * ow - ow / 2.0) / l, (p.y * oh - oh / 2.0) / l);
            let segs: Vec<Segment> = g.guides.iter().map(|(a, b)| Segment { a: c(*a), b: c(*b) }).collect();
            solve_guided(&segs)
        }
        _ => g.upright_transform.unwrap_or(Homography::IDENTITY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point refined from a candidate's lines is still held to the same test as the candidate: one that test
    /// turns down leaves the candidate itself (CodeRabbit on #2966).
    #[test]
    fn a_refined_vanishing_point_the_test_refuses_falls_back_to_its_candidate() {
        // three near-vertical lines that nearly, not exactly, meet far above the photo
        let segs = [
            Segment { a: Point::new(-0.5, -0.8), b: Point::new(-0.45, 0.8) },
            Segment { a: Point::new(0.5, -0.8), b: Point::new(0.45, 0.8) },
            Segment { a: Point::new(0.0, -0.8), b: Point::new(0.0003, 0.8) },
        ];
        // only the candidates made from two of the lines pass; the least-squares point of all three does not
        let candidates: Vec<[f64; 3]> = (0..3)
            .flat_map(|i| (i + 1..3).map(move |j| (i, j)))
            .flat_map(|(i, j)| [norm(cross(segs[i].line(), segs[j].line())), norm(cross(segs[j].line(), segs[i].line()))])
            .collect();
        let ok = |v: [f64; 3]| candidates.contains(&v);
        let (v, _) = vanishing_point_where(&segs, ok).expect("a point");
        assert!(ok(v), "{v:?} is not one of the candidates");
    }

    /// Long lines meeting inside the photo (floorboards, a drape) are scene features: they don't make a big
    /// tilt's posts look outvoted (CodeRabbit on #2966).
    #[test]
    fn scene_lines_inside_the_photo_do_not_outvote_a_big_tilts_posts() {
        // three posts leaning to a point far above the photo: a camera tilt of about 15 degrees
        let v = norm([0.0, 5.6, 1.0]);
        assert!(tilt_degrees(v) > BIG_TILT && tilt_degrees(v) < HUGE_TILT, "{}", tilt_degrees(v));
        let toward = |x: f64, (px, py): (f64, f64), y1: f64| {
            let t = (y1 + 0.8) / (py + 0.8);
            Segment { a: Point::new(x, -0.8), b: Point::new(x + (px - x) * t, y1) }
        };
        let mut segs: Vec<Segment> = [-0.8, 0.0, 0.8].iter().map(|&x| toward(x, (0.0, 5.6), 0.8)).collect();
        // six longer floor lines running to a point inside the photo, more length than the posts
        segs.extend([-0.5, -0.35, -0.2, 0.2, 0.35, 0.5].iter().map(|&x| toward(x, (0.0, 0.9), 0.6)));
        let posts: f64 = segs[..3].iter().map(Segment::len).sum();
        let floor: f64 = segs[3..].iter().map(Segment::len).sum();
        assert!(posts < 0.5 * (posts + floor), "the floor lines must outweigh the posts: {posts} vs {floor}");
        assert_eq!(reliable(&segs, v, true), Ok(()));
    }
}
