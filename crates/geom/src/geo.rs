//! Geographic coordinates in degrees (WGS84): latitude in `[-90.0, 90.0]`, longitude in
//! `[-180.0, 180.0]`. The Web Mercator projection lives in `lightcraft_engine::map::mercator`.

use serde::{Deserialize, Serialize};

/// WGS84 Geographic Coordinate (Latitude, Longitude in decimal degrees).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    /// The point, or `None` for NaN, infinities or values out of range: latitude in `[-90, 90]`,
    /// longitude in `[-180, 180]`. Never clamped: a latitude of 95° is a broken record, not the
    /// North Pole. A longitude of exactly 180 is kept as given (it is the same meridian as -180).
    pub fn new(lat: f64, lon: f64) -> Option<Self> {
        let ok = lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon);
        ok.then_some(Self { lat, lon })
    }

    /// The point with its longitude wrapped into `[-180, 180)`: `None` for NaN, infinities or a
    /// latitude out of `[-90, 90]` (commands that accept any longitude, such as `photo.setGps`).
    pub fn wrapped(lat: f64, lon: f64) -> Option<Self> {
        if !lon.is_finite() {
            return None;
        }
        Self::new(lat, Self::normalize_lon(lon))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_positions_are_rejected_not_clamped() {
        // a latitude of 100° used to become the North Pole, so commands that validate with
        // `LatLon::new` accepted it
        assert_eq!(LatLon::new(100.0, 10.0), None);
        assert_eq!(LatLon::new(-90.5, 10.0), None);
        assert_eq!(LatLon::new(10.0, 190.0), None);
        assert_eq!(LatLon::new(10.0, -180.1), None);
        assert!(LatLon::new(f64::NAN, 0.0).is_none());
        assert!(LatLon::new(0.0, f64::INFINITY).is_none());
        assert_eq!(LatLon::new(90.0, 180.0), Some(LatLon { lat: 90.0, lon: 180.0 }));
        assert_eq!(LatLon::new(-90.0, -180.0), Some(LatLon { lat: -90.0, lon: -180.0 }));
        // wrapped: any finite longitude, latitude still checked
        assert_eq!(LatLon::wrapped(10.0, 190.0), Some(LatLon { lat: 10.0, lon: -170.0 }));
        assert_eq!(LatLon::wrapped(10.0, -540.0), Some(LatLon { lat: 10.0, lon: -180.0 }));
        assert_eq!(LatLon::wrapped(91.0, 0.0), None);
        assert_eq!(LatLon::wrapped(0.0, f64::NAN), None);
    }
}
