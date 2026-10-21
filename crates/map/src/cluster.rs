//! Photo pins and their clusters.
//!
//! A map view projects each photo with a GPS position once ([`MapPoint::new`], when the set of
//! photos changes) and then, every frame, groups the points on screen into [`Cluster`]s with
//! [`cluster`]: the world (at the view's zoom) is cut into square cells of `cell` screen pixels and
//! the points in one cell become one cluster, drawn at their mean position. The cells are anchored
//! to the world, not the screen, so panning never regroups the pins; zooming in splits clusters as
//! the cells shrink on the ground. The cost is one hash lookup per point on screen (about a
//! millisecond for 50 000 points), so it runs on the UI thread.

use std::collections::HashMap;

use lightcraft_geom::Point;

use crate::mercator::{LatLon, Viewport, from_world, to_world, world_size};

/// Default cell edge in screen pixels: pins closer than this merge.
pub const CLUSTER_CELL: f64 = 60.0;

/// A photo's position, projected once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapPoint {
    /// The photo (a catalog photo id).
    pub id: u64,
    pub pos: LatLon,
    /// Position in the world square as fractions of its edge: x in [0, 1) from −180° eastward,
    /// y in [0, 1] from the north edge.
    fx: f64,
    fy: f64,
}

impl MapPoint {
    pub fn new(id: u64, pos: LatLon) -> MapPoint {
        let w = to_world(pos, 0.0);
        let s = world_size(0.0);
        MapPoint { id, pos, fx: (w.x / s).clamp(0.0, 1.0), fy: (w.y / s).clamp(0.0, 1.0) }
    }
}

/// Photos drawn as one pin (one photo) or one counted badge (several).
#[derive(Clone, Debug, PartialEq)]
pub struct Cluster {
    /// The photos, in the order the points were given.
    pub ids: Vec<u64>,
    /// Where to draw it, in screen pixels from the viewport's top-left corner.
    pub center: Point,
    /// The mean position of its photos.
    pub pos: LatLon,
}

impl Cluster {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

#[derive(Default)]
struct Cell {
    ids: Vec<u64>,
    sx: f64,
    sy: f64,
}

/// The clusters of `points` in `view`: points in the same world-anchored `cell` (screen pixels,
/// clamped to 1–4096; NaN → [`CLUSTER_CELL`]) merge. Only points within one cell of the view are
/// considered. When the world is narrower than the view (low zoom) a cluster appears once per copy
/// of the world on screen. Sorted top to bottom, then left to right. Empty for an empty view.
pub fn cluster(points: &[MapPoint], view: &Viewport, cell: f64) -> Vec<Cluster> {
    let v = Viewport::new(view.center, view.zoom, view.width, view.height);
    if v.width <= 0.0 || v.height <= 0.0 {
        return Vec::new();
    }
    let cell = if cell.is_nan() { CLUSTER_CELL } else { cell.clamp(1.0, 4096.0) };
    let s = world_size(v.zoom);
    let c = to_world(v.center, v.zoom);
    let (half_w, half_h) = (v.width / 2.0, v.height / 2.0);
    // the world repeats east–west: when it is wider than the view (plus a cell each side), only
    // the copy nearest the centre can be on screen
    let one_copy = s >= v.width + 2.0 * cell;
    let mut cells: HashMap<(i64, i64), Cell> = HashMap::new();
    for p in points {
        let (wx, wy) = (p.fx * s, p.fy * s);
        let sy = half_h + (wy - c.y);
        if sy < -cell || sy > v.height + cell {
            continue;
        }
        if one_copy {
            let dx = (wx - c.x + s / 2.0).rem_euclid(s) - s / 2.0;
            let sx = half_w + dx;
            if sx < -cell || sx > v.width + cell {
                continue;
            }
        }
        let key = ((wx / cell).floor() as i64, (wy / cell).floor() as i64);
        let e = cells.entry(key).or_default();
        e.ids.push(p.id);
        e.sx += wx;
        e.sy += wy;
    }
    let mut out = Vec::with_capacity(cells.len());
    for (_, e) in cells {
        let n = e.ids.len().max(1) as f64;
        let (mx, my) = (e.sx / n, e.sy / n);
        let pos = from_world(Point::new(mx, my), v.zoom);
        let y = half_h + (my - c.y);
        let dx = (mx - c.x + s / 2.0).rem_euclid(s) - s / 2.0;
        let nearest = half_w + dx;
        if one_copy {
            out.push(Cluster { ids: e.ids, center: Point::new(nearest, y), pos });
            continue;
        }
        // every copy of the world that puts it on screen (at most a few hundred at zoom 0 on a
        // 16k-pixel view; usually one to three)
        let first = ((-cell - nearest) / s).ceil() as i64;
        let last = ((v.width + cell - nearest) / s).floor() as i64;
        for k in first..=last {
            out.push(Cluster { ids: e.ids.clone(), center: Point::new(nearest + k as f64 * s, y), pos });
        }
    }
    out.sort_by(|a, b| a.center.y.total_cmp(&b.center.y).then(a.center.x.total_cmp(&b.center.x)));
    out
}

/// The cluster drawn under screen point `at` within `radius` pixels, nearest first (later
/// clusters are drawn on top, so ties go to them).
pub fn hit(clusters: &[Cluster], at: Point, radius: impl Fn(&Cluster) -> f64) -> Option<usize> {
    let mut best: Option<(f64, usize)> = None;
    for (i, c) in clusters.iter().enumerate() {
        let (dx, dy) = (c.center.x - at.x, c.center.y - at.y);
        let d = (dx * dx + dy * dy).sqrt();
        if d <= radius(c) && best.is_none_or(|(bd, _)| d <= bd) {
            best = Some((d, i));
        }
    }
    best.map(|(_, i)| i)
}
