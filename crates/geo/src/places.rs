//! Saved locations: a named circle on the map. A **private** location hides the position of every
//! photo inside it on export (GPS and the location fields are left out).

use serde::{Deserialize, Serialize};

use crate::GeoError;
use crate::mercator::{LatLon, distance_m};

/// Smallest and largest radius (m).
pub const MIN_RADIUS_M: f64 = 1.0;
pub const MAX_RADIUS_M: f64 = 1_000_000.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedLocation {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// Metres.
    pub radius: f64,
    #[serde(default)]
    pub private: bool,
}

impl SavedLocation {
    /// A checked location (name not empty, a position on Earth, a sane radius).
    pub fn new(name: &str, lat: f64, lon: f64, radius: f64, private: bool) -> Result<SavedLocation, GeoError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(GeoError::Invalid("a saved location needs a name (up to 200 characters)".into()));
        }
        if !LatLon::new(lat, lon).is_valid() {
            return Err(GeoError::Invalid(format!("{lat}, {lon} is not a position (latitude ±90, longitude ±180)")));
        }
        if !radius.is_finite() || !(MIN_RADIUS_M..=MAX_RADIUS_M).contains(&radius) {
            return Err(GeoError::Invalid(format!("the radius is {MIN_RADIUS_M}–{MAX_RADIUS_M} m")));
        }
        Ok(SavedLocation { name: name.to_string(), lat, lon, radius, private })
    }

    pub fn centre(&self) -> LatLon {
        LatLon::new(self.lat, self.lon)
    }

    /// Is the position inside the circle?
    pub fn contains(&self, p: LatLon) -> bool {
        p.is_valid() && distance_m(self.centre(), p) <= self.radius
    }
}

/// Is `p` inside any private location?
pub fn is_private(locations: &[SavedLocation], p: LatLon) -> bool {
    locations.iter().any(|l| l.private && l.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_and_privacy() {
        let home = SavedLocation::new(" Home ", 48.8566, 2.3522, 500.0, true).unwrap();
        assert_eq!(home.name, "Home");
        assert!(home.contains(LatLon::new(48.857, 2.353)));
        assert!(!home.contains(LatLon::new(48.87, 2.353)));
        assert!(!home.contains(LatLon::new(f64::NAN, 2.0)));
        let park = SavedLocation { private: false, ..home.clone() };
        assert!(is_private(std::slice::from_ref(&home), LatLon::new(48.857, 2.353)));
        assert!(!is_private(&[park], LatLon::new(48.857, 2.353)));
        assert!(SavedLocation::new("", 0.0, 0.0, 10.0, false).is_err());
        assert!(SavedLocation::new("x", 95.0, 0.0, 10.0, false).is_err());
        assert!(SavedLocation::new("x", 0.0, 0.0, f64::INFINITY, false).is_err());
        assert!(SavedLocation::new("x", 0.0, 0.0, 0.0, false).is_err());
    }
}
