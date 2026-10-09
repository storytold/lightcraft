//! Offline map primitives for the GPS photo view.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoPoint { pub latitude: f64, pub longitude: f64 }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapBounds { pub min_latitude: f64, pub max_latitude: f64, pub min_longitude: f64, pub max_longitude: f64 }

impl MapBounds {
    pub fn from_points(points: impl IntoIterator<Item = GeoPoint>) -> Option<Self> {
        let mut it = points.into_iter().filter(|p| p.latitude.is_finite() && p.longitude.is_finite());
        let first = it.next()?;
        let mut b = Self { min_latitude: first.latitude, max_latitude: first.latitude, min_longitude: first.longitude, max_longitude: first.longitude };
        for p in it { b.min_latitude = b.min_latitude.min(p.latitude); b.max_latitude = b.max_latitude.max(p.latitude); b.min_longitude = b.min_longitude.min(p.longitude); b.max_longitude = b.max_longitude.max(p.longitude); }
        Some(b)
    }

    /// Project into a normalized rectangle, with north at the top.
    pub fn project(self, point: GeoPoint) -> [f32; 2] {
        let lon_span = (self.max_longitude - self.min_longitude).max(1e-9);
        let lat_span = (self.max_latitude - self.min_latitude).max(1e-9);
        [((point.longitude - self.min_longitude) / lon_span).clamp(0.0, 1.0) as f32, (1.0 - (point.latitude - self.min_latitude) / lat_span).clamp(0.0, 1.0) as f32]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_project_northwest_and_southeast() {
        let b = MapBounds::from_points([GeoPoint { latitude: -34.0, longitude: 151.0 }, GeoPoint { latitude: -33.0, longitude: 152.0 }]).expect("points");
        assert_eq!(b.project(GeoPoint { latitude: -33.0, longitude: 151.0 }), [0.0, 0.0]);
        assert_eq!(b.project(GeoPoint { latitude: -34.0, longitude: 152.0 }), [1.0, 1.0]);
        assert!(MapBounds::from_points([GeoPoint { latitude: f64::NAN, longitude: 0.0 }]).is_none());
    }
}
