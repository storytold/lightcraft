//! GPS track logs drawn on the map: polylines projected once, simplified per zoom, clipped to the
//! view.
//!
//! - [`TrackShape::new`] takes a track's lines (one per continuous logging span; a line is never
//!   joined to the next) as `(latitude, longitude)` points. Invalid points (NaN, infinities, out of
//!   range) are skipped. Each line is projected once into the Web Mercator square (fractions of its
//!   edge; latitudes clamped to ±85.05°, so points at the poles sit on the map's edge) and cut where
//!   it crosses the antimeridian: a step of more than 180° of longitude between two points is taken
//!   the short way round, the line ends at ±180° and goes on from the other edge.
//! - Simplification is Douglas–Peucker, computed once for every tolerance: each point gets the
//!   *rank* at which it stops being needed (its distance from the chord of its range, capped by the
//!   rank of the point that split the range before it). Keeping the points ranked above a
//!   tolerance is then exactly the Douglas–Peucker result at that tolerance, so [`TrackShape::level`]
//!   is one pass over the points per zoom, the ends of every line are always kept, and no dropped
//!   point lies further than [`TOLERANCE_PX`] screen pixels from the line drawn. A long run of
//!   points that don't move (a logger lying still) is dropped in one step, and the work is capped
//!   (past the cap the rest of a line is kept whole), so no input makes it quadratic.
//! - A [`Level`] holds the kept points in chunks of [`CHUNK`] points with their bounding boxes, so
//!   [`Level::screen_lines`] skips what is off screen and clips the rest to the view (no point is
//!   handed to the painter thousands of pixels away).

use lightcraft_geom::Point;

use crate::mercator::{Bounds, LatLon, MAX_ZOOM, TILE_SIZE, Viewport, clamp_lat, to_world, world_size};

/// Largest distance, in screen pixels, between a dropped point and the line drawn.
pub const TOLERANCE_PX: f64 = 0.6;
/// Points per chunk of a [`Level`] (the unit of culling).
pub const CHUNK: usize = 256;
/// Below this distance (world fractions; about a millimetre on the ground) a point is on the
/// chord: the rest of its range is dropped without searching it further.
const FLAT: f64 = 1e-12;
/// Distance computations allowed per point when ranking (Douglas–Peucker is n log n on real
/// tracks; this only stops pathological input).
const WORK_PER_POINT: usize = 64;

/// One line cut at the antimeridian: points in the Mercator square (fractions: x from −180°
/// eastward in [0, 1], y from the north edge in [0, 1]) and their simplification ranks.
#[derive(Clone, Debug, Default)]
struct Piece {
    xy: Vec<Point>,
    rank: Vec<f64>,
}

/// A track ready to draw: see the module docs.
#[derive(Clone, Debug, Default)]
pub struct TrackShape {
    pieces: Vec<Piece>,
    bounds: Option<Bounds>,
    points: usize,
}

/// Position of a valid point in the Mercator square (longitude not wrapped: 180° is x = 1).
fn frac(p: LatLon) -> Point {
    let y = to_world(LatLon { lat: clamp_lat(p.lat), lon: 0.0 }, 0.0).y / f64::from(TILE_SIZE);
    Point::new((p.lon + 180.0) / 360.0, y)
}

/// Distance from `p` to the segment `a`–`b`.
fn seg_dist(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let (ex, ey) = (a.x + t * dx - p.x, a.y + t * dy - p.y);
    (ex * ex + ey * ey).sqrt()
}

/// The Douglas–Peucker rank of every point of `xy` (ends: infinite).
fn ranks(xy: &[Point]) -> Vec<f64> {
    let n = xy.len();
    let mut rank = vec![0.0; n];
    if let Some(r) = rank.first_mut() {
        *r = f64::INFINITY;
    }
    if let Some(r) = rank.last_mut() {
        *r = f64::INFINITY;
    }
    if n < 3 {
        return rank;
    }
    let mut budget = n.saturating_mul(WORK_PER_POINT);
    let mut stack = vec![(0usize, n - 1, f64::INFINITY)];
    while let Some((a, b, parent)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (Some(&pa), Some(&pb)) = (xy.get(a), xy.get(b)) else { continue };
        let inner = b - a - 1;
        if inner > budget {
            // out of work: keep the rest of this range whole (more points, never more error)
            for r in rank.get_mut(a + 1..b).unwrap_or(&mut []) {
                *r = parent;
            }
            continue;
        }
        budget -= inner;
        let (mut best, mut at) = (-1.0, a + 1);
        for (i, p) in xy.iter().enumerate().take(b).skip(a + 1) {
            let d = seg_dist(*p, pa, pb);
            if d > best {
                best = d;
                at = i;
            }
        }
        if best <= FLAT {
            // every point of the range is on the chord: all dropped (rank 0 is never kept)
            continue;
        }
        let r = best.min(parent);
        if let Some(slot) = rank.get_mut(at) {
            *slot = r;
        }
        stack.push((a, at, r));
        stack.push((at, b, r));
    }
    rank
}

impl TrackShape {
    /// The shape of a track made of `lines` (`(latitude, longitude)` points each).
    pub fn new<L, P>(lines: L) -> TrackShape
    where
        L: IntoIterator<Item = P>,
        P: IntoIterator<Item = (f64, f64)>,
    {
        let mut pieces = Vec::new();
        let mut all: Vec<(f64, f64)> = Vec::new();
        for line in lines {
            let mut cur: Vec<Point> = Vec::new();
            let mut prev: Option<LatLon> = None;
            for (lat, lon) in line {
                let Some(p) = LatLon::new(lat, lon) else { continue };
                all.push((p.lat, p.lon));
                if let Some(q) = prev {
                    let d = p.lon - q.lon;
                    if d.abs() > 180.0 {
                        // the short way round: leave by one edge, come back by the other
                        let (edge, other, unwrapped) = if d < 0.0 { (180.0, -180.0, p.lon + 360.0) } else { (-180.0, 180.0, p.lon - 360.0) };
                        let span = unwrapped - q.lon;
                        let t = if span != 0.0 { ((edge - q.lon) / span).clamp(0.0, 1.0) } else { 0.0 };
                        let lat_x = q.lat + (p.lat - q.lat) * t;
                        cur.push(frac(LatLon { lat: lat_x, lon: edge }));
                        let done = std::mem::take(&mut cur);
                        if done.len() >= 2 {
                            pieces.push(done);
                        }
                        cur.push(frac(LatLon { lat: lat_x, lon: other }));
                    }
                }
                cur.push(frac(p));
                prev = Some(p);
            }
            if !cur.is_empty() {
                pieces.push(cur);
            }
        }
        let points = all.len();
        let bounds = Bounds::of(all);
        let pieces = pieces
            .into_iter()
            .map(|xy| {
                let rank = ranks(&xy);
                Piece { xy, rank }
            })
            .collect();
        TrackShape { pieces, bounds, points }
    }

    /// The box holding every valid point (antimeridian aware), `None` for a track without one.
    pub fn bounds(&self) -> Option<Bounds> {
        self.bounds
    }

    /// Valid points of the track.
    pub fn points(&self) -> usize {
        self.points
    }

    /// Lines drawn (after the antimeridian cuts).
    pub fn pieces(&self) -> usize {
        self.pieces.len()
    }

    /// The points kept at tile zoom `zoom` ([`TOLERANCE_PX`] at that zoom), chunked for culling.
    pub fn level(&self, zoom: u8) -> Level {
        let zoom = zoom.min(MAX_ZOOM);
        let tol = TOLERANCE_PX / world_size(f64::from(zoom));
        let mut chunks = Vec::new();
        let mut kept = 0;
        for piece in &self.pieces {
            let pts: Vec<Point> = piece.xy.iter().zip(&piece.rank).filter(|(_, r)| **r > tol).map(|(p, _)| *p).collect();
            kept += pts.len();
            if pts.len() == 1 {
                // a single point (a one-point track): a dot
                chunks.push(Chunk::new(pts));
                continue;
            }
            // chunks share their end points, so the line stays whole
            let mut start = 0;
            while start + 1 < pts.len() {
                let end = (start + CHUNK).min(pts.len() - 1);
                chunks.push(Chunk::new(pts.get(start..=end).unwrap_or(&[]).to_vec()));
                start = end;
            }
        }
        Level { zoom, chunks, points: kept }
    }
}

#[derive(Clone, Debug)]
struct Chunk {
    pts: Vec<Point>,
    min: Point,
    max: Point,
}

impl Chunk {
    fn new(pts: Vec<Point>) -> Chunk {
        let mut min = Point::new(f64::INFINITY, f64::INFINITY);
        let mut max = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        for p in &pts {
            min = Point::new(min.x.min(p.x), min.y.min(p.y));
            max = Point::new(max.x.max(p.x), max.y.max(p.y));
        }
        Chunk { pts, min, max }
    }
}

/// A track simplified for one zoom ([`TrackShape::level`]).
#[derive(Clone, Debug, Default)]
pub struct Level {
    pub zoom: u8,
    chunks: Vec<Chunk>,
    /// Points kept.
    pub points: usize,
}

/// Clip the segment `a`–`b` to the box `[x0, x1] × [y0, y1]` (Liang–Barsky): the part inside,
/// and whether each end was moved.
fn clip(a: Point, b: Point, x0: f64, y0: f64, x1: f64, y1: f64) -> Option<(Point, Point, bool, bool)> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, a.x - x0), (dx, x1 - a.x), (-dy, a.y - y0), (dy, y1 - a.y)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    if t0 > t1 || !t0.is_finite() || !t1.is_finite() {
        return None;
    }
    let at = |t: f64| Point::new(a.x + t * dx, a.y + t * dy);
    Some((at(t0), at(t1), t0 > 0.0, t1 < 1.0))
}

impl Level {
    /// The lines to paint for view `vp`, in screen pixels from its top-left corner, clipped to the
    /// view grown by `margin` pixels. Each chunk is drawn on the copy of the world nearest the view's
    /// centre (as pins are). A one-point track gives a one-point line (a dot).
    pub fn screen_lines(&self, vp: &Viewport, margin: f64) -> Vec<Vec<Point>> {
        let v = Viewport::new(vp.center, vp.zoom, vp.width, vp.height);
        let mut out: Vec<Vec<Point>> = Vec::new();
        if v.width <= 0.0 || v.height <= 0.0 {
            return out;
        }
        let ws = world_size(v.zoom);
        let c = to_world(v.center, v.zoom);
        let (cx, cy) = (c.x / ws, c.y / ws);
        let m = if margin.is_finite() { margin.clamp(0.0, 1000.0) } else { 0.0 };
        let (x0, y0, x1, y1) = (-m, -m, v.width + m, v.height + m);
        for ch in &self.chunks {
            let shift = (cx - (ch.min.x + ch.max.x) / 2.0).round();
            let sx = |x: f64| (x + shift - cx) * ws + v.width / 2.0;
            let sy = |y: f64| (y - cy) * ws + v.height / 2.0;
            if sx(ch.max.x) < x0 || sx(ch.min.x) > x1 || sy(ch.max.y) < y0 || sy(ch.min.y) > y1 {
                continue;
            }
            let screen = |p: &Point| Point::new(sx(p.x), sy(p.y));
            if let [only] = ch.pts.as_slice() {
                out.push(vec![screen(only)]);
                continue;
            }
            for pair in ch.pts.windows(2) {
                let [a, b] = pair else { continue };
                if let Some((ca, cb, moved_a, _)) = clip(screen(a), screen(b), x0, y0, x1, y1) {
                    // a line goes on where the last one ended (the next segment, the next chunk)
                    if moved_a || out.last().and_then(|l| l.last()) != Some(&ca) {
                        out.push(vec![ca]);
                    }
                    if let Some(line) = out.last_mut() {
                        line.push(cb);
                    }
                }
            }
        }
        out
    }
}
