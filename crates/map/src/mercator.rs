//! Web Mercator (EPSG:3857) with 256 px tiles, as used by OpenStreetMap and most slippy maps.
//!
//! At zoom `z` the world is a square of `256 · 2^z` *world pixels*: x grows east from longitude
//! −180°, y grows south from latitude +85.05112878° ([`MAX_LATITUDE`], where the square ends).
//! Tile `(x, y)` at zoom `z` covers world pixels `[256x, 256x + 256) × [256y, 256y + 256)`.
//! Zoom may be fractional: a view at zoom 10.4 draws zoom-10 tiles scaled by 2^0.4.
//!
//! Latitudes are clamped to ±[`MAX_LATITUDE`] when projected and longitudes wrap around (the world
//! repeats east and west). Points from photos are checked with [`LatLon::new`]: NaN, infinities and
//! out-of-range values are rejected, so they never turn into NaN screen positions.

use std::collections::HashSet;
use std::f64::consts::PI;

use lightcraft_geom::{Point, Rect};
use serde::{Deserialize, Serialize};

/// Tile edge in pixels.
pub const TILE_SIZE: u32 = 256;
const TILE: f64 = TILE_SIZE as f64;
/// The latitude where the Web Mercator square ends (north and south).
pub const MAX_LATITUDE: f64 = 85.051_128_78;
/// The deepest zoom any source may have (tile indices stay well inside `u32`).
pub const MAX_ZOOM: u8 = 24;
/// Largest viewport edge considered, in screen pixels (bounds the number of tiles).
pub const MAX_VIEW_PX: f64 = 16_384.0;

/// A checked position in degrees: latitude in [−90, 90], longitude in [−180, 180], both finite.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    /// The point, or `None` for NaN, infinities or values out of range (never clamped: a latitude
    /// of 95° is a broken record, not the North Pole).
    pub fn new(lat: f64, lon: f64) -> Option<LatLon> {
        let ok = lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon);
        ok.then_some(LatLon { lat, lon })
    }

    /// A photo's GPS position (`(latitude, longitude)`, as `Photo.gps` stores it), if valid.
    pub fn from_gps(gps: Option<(f64, f64)>) -> Option<LatLon> {
        gps.and_then(|(lat, lon)| LatLon::new(lat, lon))
    }
}

/// `lon` wrapped into [−180, 180) (0 for NaN or infinities).
pub fn wrap_lon(lon: f64) -> f64 {
    if !lon.is_finite() {
        return 0.0;
    }
    if (-180.0..180.0).contains(&lon) {
        return lon;
    }
    let w = (lon + 180.0).rem_euclid(360.0) - 180.0;
    // rem_euclid can round up to exactly 360 for tiny negative inputs
    if w >= 180.0 { w - 360.0 } else { w }
}

/// `lat` clamped to the Web Mercator square (0 for NaN).
pub fn clamp_lat(lat: f64) -> f64 {
    if lat.is_nan() { 0.0 } else { lat.clamp(-MAX_LATITUDE, MAX_LATITUDE) }
}

/// `zoom` clamped to [0, [`MAX_ZOOM`]] (0 for NaN).
pub fn clamp_zoom(zoom: f64) -> f64 {
    if zoom.is_nan() { 0.0 } else { zoom.clamp(0.0, f64::from(MAX_ZOOM)) }
}

/// Edge of the world square at `zoom`, in world pixels.
pub fn world_size(zoom: f64) -> f64 {
    TILE * clamp_zoom(zoom).exp2()
}

/// World x of longitude `lon` at zoom 0 as a fraction of the world (0..1, not wrapped).
fn x_frac(lon: f64) -> f64 {
    (lon + 180.0) / 360.0
}

/// World y of latitude `lat` as a fraction of the world (0 at the north edge, 1 at the south).
fn y_frac(lat: f64) -> f64 {
    let phi = clamp_lat(lat).to_radians();
    (1.0 - (PI / 4.0 + phi / 2.0).tan().ln() / PI) / 2.0
}

fn lat_of_y_frac(y: f64) -> f64 {
    let y = if y.is_nan() { 0.5 } else { y.clamp(0.0, 1.0) };
    (PI * (1.0 - 2.0 * y)).sinh().atan().to_degrees()
}

/// The world pixel of `p` at `zoom` (x in [0, world), longitude wrapped; y in [0, world]).
pub fn to_world(p: LatLon, zoom: f64) -> Point {
    let s = world_size(zoom);
    Point::new(x_frac(wrap_lon(p.lon)) * s, y_frac(p.lat) * s)
}

/// The position of world pixel `w` at `zoom` (x wraps around the world, y is clamped to it).
pub fn from_world(w: Point, zoom: f64) -> LatLon {
    let s = world_size(zoom);
    let x = if w.x.is_finite() { w.x / s } else { 0.5 };
    LatLon { lat: lat_of_y_frac(w.y / s), lon: wrap_lon(x * 360.0 - 180.0) }
}

/// One tile of the pyramid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileKey {
    /// The tile, or `None` when `z` exceeds [`MAX_ZOOM`] or `x`/`y` fall outside the zoom's grid.
    pub fn new(z: u8, x: u32, y: u32) -> Option<TileKey> {
        let n = 1u64 << z.min(MAX_ZOOM);
        (z <= MAX_ZOOM && u64::from(x) < n && u64::from(y) < n).then_some(TileKey { z, x, y })
    }

    /// The tile containing `p` at zoom `z` (clamped to [`MAX_ZOOM`]).
    pub fn containing(p: LatLon, z: u8) -> TileKey {
        let z = z.min(MAX_ZOOM);
        let n = 1u64 << z;
        let w = to_world(p, f64::from(z));
        let idx = |v: f64| ((v / TILE).floor().max(0.0) as u64).min(n - 1) as u32;
        TileKey { z, x: idx(w.x), y: idx(w.y) }
    }

    /// North-west and south-east corners of the tile. The east edge of the last column is +180°
    /// (not wrapped to −180°).
    pub fn corners(&self) -> (LatLon, LatLon) {
        let n = f64::from(self.z.min(MAX_ZOOM)).exp2();
        let lon = |x: u32| f64::from(x) / n * 360.0 - 180.0;
        let lat = |y: u32| lat_of_y_frac(f64::from(y) / n);
        (LatLon { lat: lat(self.y), lon: lon(self.x) }, LatLon { lat: lat(self.y.saturating_add(1)), lon: lon(self.x.saturating_add(1)) })
    }
}

/// A tile placed on screen by [`Viewport::tiles`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisibleTile {
    pub key: TileKey,
    /// Where to draw it, in screen pixels from the viewport's top-left corner (it may reach past
    /// the viewport's edges). At low zoom the world repeats east–west, so the same key can appear
    /// at several places.
    pub rect: Rect,
}

/// A latitude / longitude box in degrees ([`Viewport::area`]): `east < west` when it crosses the
/// antimeridian (±180°).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl Area {
    /// Whether `p` is inside, edges included (longitude ±180 is one meridian).
    pub fn contains(&self, p: LatLon) -> bool {
        if !(p.lat >= self.south && p.lat <= self.north) {
            return false;
        }
        let within = |lon: f64| if self.west <= self.east { lon >= self.west && lon <= self.east } else { lon >= self.west || lon <= self.east };
        within(p.lon) || (p.lon.abs() == 180.0 && within(-p.lon))
    }
}

/// What a map view shows: a centre, a (fractional) zoom and a size in screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    pub center: LatLon,
    pub zoom: f64,
    pub width: f64,
    pub height: f64,
}

impl Viewport {
    /// A viewport with its values made safe: latitude clamped to the Mercator square, longitude
    /// wrapped, zoom in [0, [`MAX_ZOOM`]], size in [0, [`MAX_VIEW_PX`]] (NaN → 0).
    pub fn new(center: LatLon, zoom: f64, width: f64, height: f64) -> Viewport {
        let size = |v: f64| if v.is_nan() { 0.0 } else { v.clamp(0.0, MAX_VIEW_PX) };
        Viewport {
            center: LatLon { lat: clamp_lat(center.lat), lon: wrap_lon(center.lon) },
            zoom: clamp_zoom(zoom),
            width: size(width),
            height: size(height),
        }
    }

    /// The same view, sanitized (fields are public, so a caller may have put anything there).
    fn safe(&self) -> Viewport {
        Viewport::new(self.center, self.zoom, self.width, self.height)
    }

    /// The tile zoom drawn for this view: the view's zoom rounded, within `[0, max_zoom]`.
    pub fn tile_zoom(&self, max_zoom: u8) -> u8 {
        let z = clamp_zoom(self.zoom).round() as u8;
        z.min(max_zoom.min(MAX_ZOOM))
    }

    /// Screen pixels per world pixel of zoom `z` (2^(zoom − z)).
    pub fn scale_for(&self, z: u8) -> f64 {
        (clamp_zoom(self.zoom) - f64::from(z)).exp2()
    }

    /// Screen position of `p` (pixels from the top-left corner), on the copy of the world nearest
    /// the centre. Positions outside the viewport are returned as they are (off screen).
    pub fn to_screen(&self, p: LatLon) -> Point {
        let v = self.safe();
        let s = world_size(v.zoom);
        let c = to_world(v.center, v.zoom);
        let w = to_world(p, v.zoom);
        // the shortest way around the world
        let dx = (w.x - c.x + s / 2.0).rem_euclid(s) - s / 2.0;
        Point::new(v.width / 2.0 + dx, v.height / 2.0 + (w.y - c.y))
    }

    /// The position under screen point `sp` (longitude wrapped, latitude clamped to the map).
    pub fn to_lat_lon(&self, sp: Point) -> LatLon {
        let v = self.safe();
        let c = to_world(v.center, v.zoom);
        let (dx, dy) = (if sp.x.is_finite() { sp.x - v.width / 2.0 } else { 0.0 }, if sp.y.is_finite() { sp.y - v.height / 2.0 } else { 0.0 });
        from_world(Point::new(c.x + dx, c.y + dy), v.zoom)
    }

    /// The part of the Earth this view shows, edges in degrees ([`Area`]). A view wider than
    /// the world spans every longitude (−180…180); one whose left and right edges sit either side
    /// of the antimeridian has `east < west`. A view reaching the top (bottom) edge of the
    /// Mercator square reaches the pole (±90°: positions beyond ±85.05° are drawn on that edge).
    /// An empty view (no size) is the point at its centre.
    pub fn area(&self) -> Area {
        let v = self.safe();
        let s = world_size(v.zoom);
        let c = to_world(v.center, v.zoom);
        let (top, bottom) = (c.y - v.height / 2.0, c.y + v.height / 2.0);
        let north = if top <= 0.0 { 90.0 } else { lat_of_y_frac(top / s) };
        let south = if bottom >= s { -90.0 } else { lat_of_y_frac(bottom / s) };
        let (west, east) = if v.width >= s {
            (-180.0, 180.0)
        } else {
            let span = v.width / s * 360.0;
            let west = wrap_lon(v.center.lon - span / 2.0);
            let east = west + span;
            (west, if east > 180.0 { east - 360.0 } else { east })
        };
        Area { south, west, north, east }
    }

    /// The view moved by `(dx, dy)` screen pixels (content follows a drag of that amount).
    pub fn panned(&self, dx: f64, dy: f64) -> Viewport {
        let v = self.safe();
        let c = Point::new(v.width / 2.0 - if dx.is_finite() { dx } else { 0.0 }, v.height / 2.0 - if dy.is_finite() { dy } else { 0.0 });
        Viewport::new(v.to_lat_lon(c), v.zoom, v.width, v.height)
    }

    /// The view at `zoom`, keeping the position under screen point `anchor` where it is (wheel
    /// zoom around the pointer).
    pub fn zoomed_around(&self, anchor: Point, zoom: f64) -> Viewport {
        let v = self.safe();
        let anchor = if anchor.x.is_finite() && anchor.y.is_finite() { anchor } else { Point::new(v.width / 2.0, v.height / 2.0) };
        let under = v.to_lat_lon(anchor);
        let z = clamp_zoom(zoom);
        let w = to_world(under, z);
        let c = Point::new(w.x - (anchor.x - v.width / 2.0), w.y - (anchor.y - v.height / 2.0));
        Viewport::new(from_world(c, z), z, v.width, v.height)
    }

    /// Top-left of the view in world pixels of tile zoom `z`, and the screen scale.
    fn origin(&self, z: u8) -> (Point, f64) {
        let scale = self.scale_for(z);
        let c = to_world(self.center, f64::from(z));
        (Point::new(c.x - self.width / 2.0 / scale, c.y - self.height / 2.0 / scale), scale)
    }

    /// Tile columns and rows (unwrapped column indices) covering the view at tile zoom `z`,
    /// grown by `margin` tiles on each side; rows are clamped to the grid.
    fn grid(&self, z: u8, margin: i64) -> (std::ops::RangeInclusive<i64>, std::ops::RangeInclusive<i64>, Point, f64) {
        let (o, scale) = self.origin(z);
        let n = 1i64 << z;
        let (w, h) = (self.width / scale, self.height / scale);
        let first = |v: f64| (v / TILE).floor() as i64 - margin;
        // a view edge exactly on a tile boundary doesn't pull in the next tile
        let last = |v: f64, len: f64| ((v + len) / TILE).ceil() as i64 - 1 + margin;
        let cols = first(o.x)..=last(o.x, w).max(first(o.x));
        let rows = first(o.y).max(0)..=last(o.y, h).min(n - 1);
        (cols, rows, o, scale)
    }

    /// The tiles to draw for this view at zoom `min(round(zoom), max_zoom)`, with their screen
    /// rectangles, nearest the centre first. Empty for an empty viewport.
    pub fn tiles(&self, max_zoom: u8) -> Vec<VisibleTile> {
        let v = self.safe();
        if v.width <= 0.0 || v.height <= 0.0 {
            return Vec::new();
        }
        let z = v.tile_zoom(max_zoom);
        let n = 1i64 << z;
        let (cols, rows, o, scale) = v.grid(z, 0);
        let mut out = Vec::new();
        for ty in rows {
            for tx in cols.clone() {
                let Some(key) = TileKey::new(z, tx.rem_euclid(n) as u32, ty as u32) else { continue };
                let x0 = (tx as f64 * TILE - o.x) * scale;
                let y0 = (ty as f64 * TILE - o.y) * scale;
                out.push(VisibleTile { key, rect: Rect::new(x0, y0, x0 + TILE * scale, y0 + TILE * scale) });
            }
        }
        let (cx, cy) = (v.width / 2.0, v.height / 2.0);
        let d = |t: &VisibleTile| {
            let (x, y) = ((t.rect.x0 + t.rect.x1) / 2.0 - cx, (t.rect.y0 + t.rect.y1) / 2.0 - cy);
            x * x + y * y
        };
        out.sort_by(|a, b| d(a).total_cmp(&d(b)));
        out
    }

    /// The distinct tiles to load for this view: those [`Viewport::tiles`] draws plus a ring of
    /// `margin` tiles around them (at most 1: the OpenStreetMap tile policy forbids fetching
    /// beyond what is shown), nearest the centre first. Only the drawn zoom, never others.
    pub fn tile_keys(&self, max_zoom: u8, margin: u32) -> Vec<TileKey> {
        let v = self.safe();
        if v.width <= 0.0 || v.height <= 0.0 {
            return Vec::new();
        }
        let z = v.tile_zoom(max_zoom);
        let n = 1i64 << z;
        let (cols, rows, o, scale) = v.grid(z, i64::from(margin.min(1)));
        let c = Point::new(o.x + v.width / 2.0 / scale, o.y + v.height / 2.0 / scale);
        let mut seen = HashSet::new();
        let mut keyed = Vec::new();
        for ty in rows {
            for tx in cols.clone() {
                let Some(key) = TileKey::new(z, tx.rem_euclid(n) as u32, ty as u32) else { continue };
                let (dx, dy) = ((tx as f64 + 0.5) * TILE - c.x, (ty as f64 + 0.5) * TILE - c.y);
                keyed.push((dx * dx + dy * dy, key));
            }
        }
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
        keyed.into_iter().filter(|(_, k)| seen.insert(*k)).map(|(_, k)| k).collect()
    }
}

/// A latitude/longitude box. `west > east` means it crosses the antimeridian (±180°): it spans
/// from `west` eastward through 180° to `east`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl Bounds {
    /// The smallest box holding every valid point (`(latitude, longitude)` pairs, as `Photo.gps`
    /// stores them); invalid points (NaN, out of range) are skipped. `None` when no point is valid.
    ///
    /// Longitudes are treated as a circle: the box is the complement of the widest gap between the
    /// points, so photos at 179° and −179° give a 2°-wide box crossing the antimeridian (west 179,
    /// east −179), not one spanning the whole world.
    pub fn of<I: IntoIterator<Item = (f64, f64)>>(points: I) -> Option<Bounds> {
        let mut lons = Vec::new();
        let (mut south, mut north) = (f64::INFINITY, f64::NEG_INFINITY);
        for p in points.into_iter().filter_map(|(lat, lon)| LatLon::new(lat, lon)) {
            south = south.min(p.lat);
            north = north.max(p.lat);
            lons.push(wrap_lon(p.lon));
        }
        if lons.is_empty() {
            return None;
        }
        lons.sort_by(f64::total_cmp);
        lons.dedup();
        let (first, last) = (*lons.first()?, *lons.last()?);
        // the gap from the easternmost point round to the westernmost
        let mut best = (first + 360.0 - last, first, last);
        for pair in lons.windows(2) {
            if let [a, b] = pair
                && b - a > best.0
            {
                best = (b - a, *b, *a);
            }
        }
        let (_, west, mut east) = best;
        if east == -180.0 && west > east {
            east = 180.0;
        }
        Some(Bounds { south, west, north, east })
    }

    /// The box crosses the antimeridian.
    pub fn crosses_antimeridian(&self) -> bool {
        self.west > self.east
    }

    /// Width in degrees of longitude (0..=360).
    pub fn lon_span(&self) -> f64 {
        if self.crosses_antimeridian() { self.east + 360.0 - self.west } else { self.east - self.west }
    }

    /// `p` lies inside (edges included).
    pub fn contains(&self, p: LatLon) -> bool {
        let lon = wrap_lon(p.lon);
        let lat_ok = (self.south..=self.north).contains(&p.lat);
        let lon_ok = if self.crosses_antimeridian() {
            lon >= self.west || lon <= self.east
        } else {
            (self.west..=self.east).contains(&lon) || (self.east == 180.0 && lon == -180.0)
        };
        lat_ok && lon_ok
    }

    /// The centre in Web Mercator terms (the middle of the projected box, longitude wrapped).
    pub fn center(&self) -> LatLon {
        let y = (y_frac(self.north) + y_frac(self.south)) / 2.0;
        LatLon { lat: lat_of_y_frac(y), lon: wrap_lon(self.west + self.lon_span() / 2.0) }
    }
}

/// The view of a `width × height` px map that shows all of `bounds` with `margin` px to spare on
/// every side, at the deepest zoom up to `max_zoom` (a single point, or a box too small to need
/// more, gets `max_zoom`). Zoom is fractional; callers wanting whole zooms floor it.
pub fn fit_bounds(bounds: &Bounds, width: f64, height: f64, margin: f64, max_zoom: f64) -> Viewport {
    let margin = if margin.is_finite() { margin.max(0.0) } else { 0.0 };
    let size = |v: f64| if v.is_finite() { v.clamp(0.0, MAX_VIEW_PX) } else { 0.0 };
    let (width, height) = (size(width), size(height));
    let max_zoom = clamp_zoom(max_zoom);
    let avail_w = (width - 2.0 * margin).max(1.0);
    let avail_h = (height - 2.0 * margin).max(1.0);
    let dx = bounds.lon_span().clamp(0.0, 360.0) / 360.0 * TILE;
    let dy = ((y_frac(bounds.south) - y_frac(bounds.north)) * TILE).abs();
    let fit = |avail: f64, d: f64| if d > 0.0 && d.is_finite() { (avail / d).log2() } else { f64::INFINITY };
    let zoom = fit(avail_w, dx).min(fit(avail_h, dy));
    let zoom = if zoom.is_finite() { zoom.clamp(0.0, max_zoom) } else { max_zoom };
    Viewport::new(bounds.center(), zoom, width, height)
}
