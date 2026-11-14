//! Geographic coordinates, Web Mercator projection, and slippy map tile mathematics.
//!
//! Conventions: coordinates are in degrees (WGS84). Latitude in `[-90.0, 90.0]`,
//! longitude in `[-180.0, 180.0]`. Web Mercator bounds latitude to `~[-85.05112878, 85.05112878]`.

use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Maximum latitude representable in Web Mercator projection (approx 85.05112878 degrees).
pub const MAX_WEB_MERCATOR_LAT: f64 = 85.0511287798066;

/// Earth radius in meters (mean radius for WGS84).
pub const EARTH_RADIUS_METERS: f64 = 6_371_000.0;

/// WGS84 Geographic Coordinate (Latitude, Longitude in decimal degrees).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    /// Create a new `LatLon` if coordinates are finite numbers.
    /// Values outside valid ranges `[-90, 90]` or `[-180, 180]` are clamped.
    pub fn new(lat: f64, lon: f64) -> Option<Self> {
        if lat.is_finite() && lon.is_finite() { Some(Self { lat: lat.clamp(-90.0, 90.0), lon: Self::normalize_lon(lon) }) } else { None }
    }

    /// Normalizes longitude to `[-180.0, 180.0)`.
    pub fn normalize_lon(lon: f64) -> f64 {
        if !lon.is_finite() {
            return 0.0;
        }
        let mut l = lon % 360.0;
        if l < -180.0 {
            l += 360.0;
        } else if l >= 180.0 {
            l -= 360.0;
        }
        l
    }

    /// Convert `LatLon` to continuous Web Mercator tile coordinates `(x, y)` at fractional zoom `zoom`.
    pub fn to_tile_coords(self, zoom: f64) -> (f64, f64) {
        let z = if zoom.is_finite() { zoom.clamp(0.0, 24.0) } else { 0.0 };
        let n = 2.0_f64.powf(z);
        let lon = Self::normalize_lon(self.lon);
        let x = ((lon + 180.0) / 360.0 * n).clamp(0.0, n);

        let lat_clamped = self.lat.clamp(-MAX_WEB_MERCATOR_LAT, MAX_WEB_MERCATOR_LAT);
        let lat_rad = lat_clamped.to_radians();
        // y = (1 - ln(tan(lat) + sec(lat)) / PI) / 2 * n
        let sin_lat = lat_rad.sin();
        let y = ((1.0 - ((1.0 + sin_lat) / (1.0 - sin_lat)).ln() / (2.0 * PI)) / 2.0 * n).clamp(0.0, n);
        (x, y)
    }

    /// Convert continuous tile coordinates back to `LatLon` at integer zoom `zoom`.
    pub fn from_tile_coords(x: f64, y: f64, zoom: u32) -> Self {
        let z = zoom.min(24);
        let n = 2.0_f64.powi(z as i32);
        let lon = if n > 0.0 { (x / n * 360.0 - 180.0).clamp(-180.0, 180.0) } else { 0.0 };
        let lat_rad = (PI * (1.0 - 2.0 * y / n)).sinh().atan();
        let lat = lat_rad.to_degrees().clamp(-90.0, 90.0);
        Self { lat, lon }
    }

    /// Great-circle distance between two coordinates in meters using the Haversine formula.
    pub fn distance_meters(self, other: LatLon) -> f64 {
        let lat1 = self.lat.to_radians();
        let lat2 = other.lat.to_radians();
        let delta_lat = (other.lat - self.lat).to_radians();
        let delta_lon = (other.lon - self.lon).to_radians();

        let a = (delta_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (delta_lon / 2.0).sin().powi(2);
        let c = 2.0 * a.sqrt().clamp(0.0, 1.0).asin();
        EARTH_RADIUS_METERS * c
    }
}

/// Geographic Point with optional altitude and camera direction heading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GeoPoint {
    pub lat_lon: LatLon,
    pub altitude: Option<f64>,
    pub direction: Option<f32>,
}

impl GeoPoint {
    pub fn new(lat: f64, lon: f64) -> Option<Self> {
        LatLon::new(lat, lon).map(|lat_lon| Self { lat_lon, altitude: None, direction: None })
    }
}

/// Geographic Bounding Box in WGS84 degrees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GeoBounds {
    pub min_lat: f64,
    pub max_lat: f64,
    pub min_lon: f64,
    pub max_lon: f64,
}

impl GeoBounds {
    pub fn new(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> Self {
        Self {
            min_lat: min_lat.min(max_lat).clamp(-90.0, 90.0),
            max_lat: min_lat.max(max_lat).clamp(-90.0, 90.0),
            min_lon: min_lon.min(max_lon).clamp(-180.0, 180.0),
            max_lon: min_lon.max(max_lon).clamp(-180.0, 180.0),
        }
    }

    /// Whether this bounding box contains `point`.
    pub fn contains(self, point: LatLon) -> bool {
        point.lat >= self.min_lat && point.lat <= self.max_lat && point.lon >= self.min_lon && point.lon <= self.max_lon
    }

    /// Center coordinate of the bounding box.
    pub fn center(self) -> LatLon {
        LatLon { lat: (self.min_lat + self.max_lat) / 2.0, lon: (self.min_lon + self.max_lon) / 2.0 }
    }

    /// Calculate bounding box covering a circle defined by center point and radius in meters.
    pub fn from_center_and_radius(center: LatLon, radius_meters: f64) -> Self {
        let r = radius_meters.max(0.0);
        let lat_delta = (r / EARTH_RADIUS_METERS).to_degrees();
        let cos_lat = center.lat.to_radians().cos().abs().max(0.0001);
        let lon_delta = ((r / EARTH_RADIUS_METERS) / cos_lat).to_degrees();

        Self::new(center.lat - lat_delta, center.lat + lat_delta, center.lon - lon_delta, center.lon + lon_delta)
    }
}

/// Discrete Slippy Map Tile Coordinate at integer zoom level `z`.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TileCoord {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileCoord {
    pub const fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }

    /// Maximum valid coordinate index along either axis for zoom level `z`.
    pub fn max_coord(z: u8) -> u32 {
        if z >= 31 { u32::MAX } else { (1 << z) - 1 }
    }

    /// Whether this tile index is valid at its zoom level.
    pub fn is_valid(self) -> bool {
        let max = Self::max_coord(self.z);
        self.x <= max && self.y <= max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lat_lon_clamping_and_normalization() {
        let p = LatLon::new(100.0, 190.0);
        assert!(p.is_some());
        if let Some(p) = p {
            assert_eq!(p.lat, 90.0);
            assert_eq!(p.lon, -170.0);
        }

        assert!(LatLon::new(f64::NAN, 0.0).is_none());
        assert!(LatLon::new(0.0, f64::INFINITY).is_none());
    }

    #[test]
    fn test_web_mercator_roundtrip() {
        let points = [
            LatLon { lat: 0.0, lon: 0.0 },
            LatLon { lat: 51.5074, lon: -0.1278 },   // London
            LatLon { lat: 45.57, lon: -73.2 },       // Beloeil area
            LatLon { lat: -33.8688, lon: 151.2093 }, // Sydney
        ];

        for p in points {
            for zoom in 0..=18 {
                let (x, y) = p.to_tile_coords(zoom as f64);
                let roundtrip = LatLon::from_tile_coords(x, y, zoom);
                assert!((p.lat - roundtrip.lat).abs() < 1e-4, "lat mismatch at z={zoom}: {} vs {}", p.lat, roundtrip.lat);
                assert!((p.lon - roundtrip.lon).abs() < 1e-4, "lon mismatch at z={zoom}: {} vs {}", p.lon, roundtrip.lon);
            }
        }
    }

    #[test]
    fn test_haversine_distance() {
        let london = LatLon { lat: 51.5074, lon: -0.1278 };
        let paris = LatLon { lat: 48.8566, lon: 2.3522 };
        let d = london.distance_meters(paris);
        // Distance London to Paris is ~343 km
        assert!(d > 340_000.0 && d < 350_000.0, "unexpected distance: {d}");
    }

    #[test]
    fn test_geo_bounds() {
        let bounds = GeoBounds::new(10.0, 20.0, 30.0, 40.0);
        assert!(bounds.contains(LatLon { lat: 15.0, lon: 35.0 }));
        assert!(!bounds.contains(LatLon { lat: 25.0, lon: 35.0 }));
        assert_eq!(bounds.center(), LatLon { lat: 15.0, lon: 35.0 });
    }

    #[test]
    fn test_tile_coord_validity() {
        assert!(TileCoord::new(0, 0, 0).is_valid());
        assert!(!TileCoord::new(0, 1, 0).is_valid());
        assert!(TileCoord::new(1, 1, 1).is_valid());
        assert!(!TileCoord::new(1, 2, 0).is_valid());
    }
}
