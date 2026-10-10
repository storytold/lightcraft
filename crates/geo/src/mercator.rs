//! Web-Mercator (EPSG:3857) as slippy maps use it: the world is a square of `256·2^zoom` pixels,
//! x east from 180°W, y south from ~85.05°N; tile `(z, x, y)` covers pixels `[256x, 256x+256)`.

use serde::{Deserialize, Serialize};

/// The tile edge in pixels.
pub const TILE: f64 = 256.0;
/// Highest zoom the app asks for (servers stop at 19–20).
pub const MAX_ZOOM: u8 = 20;
/// Latitude limit of the projection.
pub const MAX_LAT: f64 = 85.051_128_779_806_59;
/// Mean Earth radius (m).
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// A position in degrees.
#[derive(Clone, Copy, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    pub fn new(lat: f64, lon: f64) -> LatLon {
        LatLon { lat, lon }
    }

    /// Finite and on Earth.
    pub fn is_valid(self) -> bool {
        self.lat.is_finite() && self.lon.is_finite() && self.lat.abs() <= 90.0 && self.lon.abs() <= 180.0
    }
}

/// One tile of a zoom level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileKey {
    /// `None` when `x`/`y` are outside the zoom level's grid.
    pub fn new(z: u8, x: u32, y: u32) -> Option<TileKey> {
        let n = tiles_across(z)?;
        (z <= MAX_ZOOM && x < n && y < n).then_some(TileKey { z, x, y })
    }

    /// The parent tile one level up.
    pub fn parent(self) -> Option<TileKey> {
        (self.z > 0).then(|| TileKey { z: self.z - 1, x: self.x / 2, y: self.y / 2 })
    }
}

/// Tiles across a zoom level (`2^z`), `None` past [`MAX_ZOOM`].
pub fn tiles_across(z: u8) -> Option<u32> {
    (z <= MAX_ZOOM).then(|| 1u32 << z)
}

/// The world's size in pixels at a (fractional) zoom.
pub fn world_px(zoom: f64) -> f64 {
    TILE * zoom.clamp(0.0, MAX_ZOOM as f64).exp2()
}

/// Position → normalized world coordinates in `[0, 1]²` (x east, y south).
pub fn project(p: LatLon) -> (f64, f64) {
    let lat = p.lat.clamp(-MAX_LAT, MAX_LAT).to_radians();
    let x = (p.lon.clamp(-180.0, 180.0) + 180.0) / 360.0;
    let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0;
    (x, y.clamp(0.0, 1.0))
}

/// Normalized world coordinates → position (x wraps, y is clamped).
pub fn unproject(x: f64, y: f64) -> LatLon {
    let x = if x.is_finite() { x.rem_euclid(1.0) } else { 0.5 };
    let y = if y.is_finite() { y.clamp(0.0, 1.0) } else { 0.5 };
    let n = std::f64::consts::PI * (1.0 - 2.0 * y);
    LatLon { lat: n.sinh().atan().to_degrees(), lon: x * 360.0 - 180.0 }
}

/// The tiles covering a view `width × height` pixels centred on `centre` at integer `zoom`:
/// x wraps around the antimeridian, y stops at the poles. Capped (a hostile size can't ask
/// for millions).
pub fn visible_tiles(centre: LatLon, zoom: u8, width: f64, height: f64) -> Vec<(TileKey, f64, f64)> {
    let Some(n) = tiles_across(zoom) else { return Vec::new() };
    let world = TILE * n as f64;
    let (cx, cy) = project(centre);
    let (cx, cy) = (cx * world, cy * world);
    let (w, h) = (width.clamp(0.0, 16384.0), height.clamp(0.0, 16384.0));
    let (left, top) = (cx - w / 2.0, cy - h / 2.0);
    let tx0 = (left / TILE).floor() as i64;
    let tx1 = ((left + w) / TILE).floor() as i64;
    let ty0 = ((top / TILE).floor() as i64).max(0);
    let ty1 = (((top + h) / TILE).floor() as i64).min(n as i64 - 1);
    let mut out = Vec::new();
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let x = tx.rem_euclid(n as i64) as u32;
            if let Some(k) = TileKey::new(zoom, x, ty as u32) {
                // screen offset of the tile's top-left from the view's top-left
                out.push((k, tx as f64 * TILE - left, ty as f64 * TILE - top));
            }
            if out.len() >= 1024 {
                return out;
            }
        }
    }
    out
}

/// Great-circle distance in metres (haversine).
pub fn distance_m(a: LatLon, b: LatLon) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dp = p2 - p1;
    let dl = (b.lon - a.lon).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.clamp(0.0, 1.0).sqrt().asin()
}

/// Ground metres per screen pixel at `lat` and `zoom`.
pub fn metres_per_px(lat: f64, zoom: f64) -> f64 {
    2.0 * std::f64::consts::PI * EARTH_RADIUS_M * lat.clamp(-MAX_LAT, MAX_LAT).to_radians().cos() / world_px(zoom)
}

/// The centre and zoom that show every point in a `width × height` view (with a margin);
/// `None` without points.
pub fn fit(points: &[LatLon], width: f64, height: f64) -> Option<(LatLon, f64)> {
    let pts: Vec<(f64, f64)> = points.iter().filter(|p| p.is_valid()).map(|p| project(*p)).collect();
    let first = pts.first()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.0, first.1, first.0, first.1);
    for (x, y) in &pts {
        x0 = x0.min(*x);
        x1 = x1.max(*x);
        y0 = y0.min(*y);
        y1 = y1.max(*y);
    }
    let centre = unproject((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let span = |d: f64, px: f64| if d <= 1e-12 { 16.0 } else { ((px.max(64.0) * 0.8) / (TILE * d)).log2() };
    let zoom = span(x1 - x0, width).min(span(y1 - y0, height)).clamp(1.0, 16.0);
    Some((centre, zoom))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_round_trips() {
        for p in [LatLon::new(0.0, 0.0), LatLon::new(51.5, -0.12), LatLon::new(-33.9, 151.2), LatLon::new(80.0, 179.0)] {
            let (x, y) = project(p);
            let q = unproject(x, y);
            assert!((p.lat - q.lat).abs() < 1e-9 && (p.lon - q.lon).abs() < 1e-9, "{p:?} {q:?}");
        }
        assert_eq!(project(LatLon::new(0.0, 0.0)), (0.5, 0.5));
    }

    #[test]
    fn tile_of_london_at_zoom_10() {
        // the OSM wiki's worked example: z10 tile containing London is (511, 340)
        let (x, y) = project(LatLon::new(51.5074, -0.1278));
        assert_eq!(((x * 1024.0) as u32, (y * 1024.0) as u32), (511, 340));
    }

    #[test]
    fn visible_tiles_wrap_and_stop_at_poles() {
        let t = visible_tiles(LatLon::new(0.0, 179.9), 1, 1024.0, 1024.0);
        assert!(t.iter().all(|(k, _, _)| k.x < 2 && k.y < 2));
        assert!(!t.is_empty());
        assert!(visible_tiles(LatLon::new(0.0, 0.0), 2, f64::NAN, 1e12).len() <= 1024);
        assert!(TileKey::new(3, 8, 0).is_none());
        assert!(TileKey::new(30, 0, 0).is_none());
    }

    #[test]
    fn distances() {
        let paris = LatLon::new(48.8566, 2.3522);
        let london = LatLon::new(51.5074, -0.1278);
        let d = distance_m(paris, london);
        assert!((d - 343_500.0).abs() < 2_000.0, "{d}");
        assert_eq!(distance_m(paris, paris), 0.0);
    }

    #[test]
    fn fit_covers_points() {
        let (c, z) = fit(&[LatLon::new(48.0, 2.0), LatLon::new(52.0, 0.0)], 800.0, 600.0).unwrap_or_default();
        assert!(c.lat > 48.0 && c.lat < 52.0 && z > 3.0 && z < 9.0, "{c:?} {z}");
        assert!(fit(&[], 10.0, 10.0).is_none());
        assert!(fit(&[LatLon::new(f64::NAN, 0.0)], 10.0, 10.0).is_none());
    }
}
