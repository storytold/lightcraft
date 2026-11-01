//! Offline reverse geocoding: a GPS position → Sublocation / City / State / Country, from the
//! GeoNames dump (CC-BY 4.0, "GeoNames, geonames.org"). See README.md.
//!
//! Sources: GeoNames dump readme (prose); haversine (textbook); cell index own design.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod unzip;

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

/// Lookups farther than this from any place find nothing.
pub const MAX_KM: f64 = 50.0;
/// The files the engine downloads (the first comes zipped as `cities15000.zip`).
pub const GEONAMES_FILES: [&str; 3] = ["cities15000.txt", "admin1CodesASCII.txt", "countryInfo.txt"];
/// The zip holding [`GEONAMES_FILES`]`[0]`.
pub const GEONAMES_ZIP: &str = "cities15000.zip";
pub const GEONAMES_BASE: &str = "https://download.geonames.org/export/dump/";
pub const GEONAMES_ATTRIBUTION: &str = "Place names: GeoNames (geonames.org), CC-BY 4.0";
/// Places kept from a dataset (cities15000 has ~33 000; cities500 ~200 000).
const MAX_PLACES: usize = 2_000_000;
/// Largest name kept (GeoNames names are short; anything longer is junk).
const MAX_NAME: usize = 200;
const EARTH_RADIUS_M: f64 = 6_371_008.8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeocodeError {
    /// The place names aren't there (not downloaded yet) or can't be read.
    Missing(String),
    /// The data is not what it should be.
    Invalid(String),
}

impl std::fmt::Display for GeocodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeocodeError::Missing(e) | GeocodeError::Invalid(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for GeocodeError {}

/// A position in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    pub fn new(lat: f64, lon: f64) -> LatLon {
        LatLon { lat, lon }
    }

    /// Finite and on the globe.
    pub fn is_valid(self) -> bool {
        self.lat.is_finite() && self.lon.is_finite() && self.lat.abs() <= 90.0 && self.lon.abs() <= 180.0
    }
}

/// Great-circle distance in metres (haversine). NaN in, NaN out.
pub fn distance_m(a: LatLon, b: LatLon) -> f64 {
    let (p1, p2) = (a.lat.to_radians(), b.lat.to_radians());
    let dp = p2 - p1;
    let dl = (b.lon - a.lon).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.clamp(0.0, 1.0).sqrt().asin()
}

/// What a lookup fills in (empty strings: unknown).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Place {
    /// Below city level. GeoNames' city list has none, so offline lookups leave it empty.
    pub sublocation: String,
    pub city: String,
    pub state: String,
    pub country: String,
    /// ISO 3166-1 alpha-2, upper case.
    pub iso: String,
    /// Metres from the position to the place.
    pub distance: f64,
}

#[derive(Clone, Debug)]
struct City {
    name: String,
    at: LatLon,
    country: String,
    admin1: String,
}

/// An offline GeoNames index: places bucketed by 1° cells.
#[derive(Clone, Debug, Default)]
pub struct GeoNames {
    cities: Vec<City>,
    cells: HashMap<(i32, i32), Vec<u32>>,
    admin1: HashMap<String, String>,
    countries: HashMap<String, String>,
}

fn cell(p: LatLon) -> (i32, i32) {
    // valid positions only: |lat| ≤ 90, |lon| ≤ 180, so the casts are in range
    // longitude 180 is the same meridian as -180: key it as column -180, the wrap `reverse` probes
    (p.lat.floor() as i32, (p.lon.floor() as i32 + 180).rem_euclid(360) - 180)
}

fn short(s: &str) -> String {
    s.trim().chars().take(MAX_NAME).collect()
}

impl GeoNames {
    /// From the dump's text: `cities` (tab-separated, the `cities*.txt` layout), and optionally
    /// `admin1CodesASCII.txt` and `countryInfo.txt`. Bad lines are skipped.
    pub fn parse(cities: &str, admin1: Option<&str>, countries: Option<&str>) -> GeoNames {
        let mut g = GeoNames::default();
        for line in cities.lines() {
            if g.cities.len() >= MAX_PLACES {
                break;
            }
            let f: Vec<&str> = line.split('\t').collect();
            // columns: 1 name, 4 latitude, 5 longitude, 8 country code, 10 admin1 code
            let (Some(name), Some(lat), Some(lon), Some(cc)) = (f.get(1), f.get(4), f.get(5), f.get(8)) else { continue };
            let (Ok(lat), Ok(lon)) = (lat.trim().parse::<f64>(), lon.trim().parse::<f64>()) else { continue };
            let at = LatLon::new(lat, lon);
            let name = short(name);
            if !at.is_valid() || name.is_empty() {
                continue;
            }
            let Ok(i) = u32::try_from(g.cities.len()) else { break };
            g.cells.entry(cell(at)).or_default().push(i);
            g.cities.push(City { name, at, country: short(cc).to_ascii_uppercase(), admin1: f.get(10).map(|s| short(s)).unwrap_or_default() });
        }
        for line in admin1.unwrap_or("").lines() {
            let mut f = line.split('\t');
            if let (Some(code), Some(name)) = (f.next(), f.next()) {
                g.admin1.insert(short(code), short(name));
            }
        }
        for line in countries.unwrap_or("").lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = line.split('\t').collect();
            if let (Some(iso), Some(name)) = (f.first(), f.get(4)) {
                g.countries.insert(short(iso).to_ascii_uppercase(), short(name));
            }
        }
        g
    }

    /// Load from a folder holding [`GEONAMES_FILES`] (the last two are optional: without them
    /// states stay blank and countries are ISO codes).
    pub fn load(dir: &Path) -> Result<GeoNames, GeocodeError> {
        let read = |n: &str| std::fs::read(dir.join(n)).map(|b| String::from_utf8_lossy(&b).into_owned());
        let cities = read(GEONAMES_FILES[0]).map_err(|e| GeocodeError::Missing(format!("the place names aren't downloaded ({e})")))?;
        let g = GeoNames::parse(&cities, read(GEONAMES_FILES[1]).ok().as_deref(), read(GEONAMES_FILES[2]).ok().as_deref());
        if g.is_empty() {
            return Err(GeocodeError::Invalid("the place names file holds no places".into()));
        }
        Ok(g)
    }

    pub fn len(&self) -> usize {
        self.cities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cities.is_empty()
    }

    /// The nearest place within [`MAX_KM`]; `None` for an invalid position or open country.
    pub fn reverse(&self, p: LatLon) -> Option<Place> {
        if !p.is_valid() {
            return None;
        }
        let (cy, _) = cell(p);
        let mut best: Option<(f64, &City)> = None;
        // ±1° of latitude covers 50 km everywhere. Longitude degrees shrink with latitude: a point
        // 50 km away differs by at most asin(sin(50 km / R) / cos(lat)) degrees of longitude
        // (every longitude once the circle of latitude is that small).
        let reach = (MAX_KM * 1000.0 / EARTH_RADIUS_M).sin();
        let scale = p.lat.to_radians().cos();
        let (x0, x1) = if scale <= reach {
            (-180, 179)
        } else {
            let span = (reach / scale).asin().to_degrees();
            ((p.lon - span).floor() as i32, (p.lon + span).floor() as i32)
        };
        for dy in -1..=1 {
            for dx in x0..=x1 {
                let x = (dx + 180).rem_euclid(360) - 180;
                for &i in self.cells.get(&(cy + dy, x)).into_iter().flatten() {
                    let Some(c) = self.cities.get(i as usize) else { continue };
                    let d = distance_m(p, c.at);
                    if best.is_none_or(|(b, _)| d < b) {
                        best = Some((d, c));
                    }
                }
            }
        }
        let (d, c) = best?;
        (d <= MAX_KM * 1000.0).then(|| Place {
            sublocation: String::new(),
            city: c.name.clone(),
            state: self.admin1.get(&format!("{}.{}", c.country, c.admin1)).cloned().unwrap_or_default(),
            country: self.countries.get(&c.country).cloned().unwrap_or_else(|| c.country.clone()),
            iso: c.country.clone(),
            distance: d.round(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CITIES: &str = "2988507\tParis\tParis\t\t48.85341\t2.3488\tP\tPPLC\tFR\t\t11\t75\t\t\t2138551\t\t42\tEurope/Paris\t2024-01-01\n\
                          2643743\tLondon\tLondon\t\t51.50853\t-0.12574\tP\tPPLC\tGB\t\tENG\t\t\t\t8961989\t\t25\tEurope/London\t2024-01-01\n\
                          bad line\n\
                          1\tNowhere\tx\t\tNaN\t0\tP\tP\tXX\n\
                          2\tFar\tx\t\t500\t0\tP\tP\tXX\n\
                          3\tInf\tx\t\tinf\t-inf\tP\tP\tXX\n";

    #[test]
    fn nearest_place() {
        let g = GeoNames::parse(CITIES, Some("FR.11\tÎle-de-France\nGB.ENG\tEngland\n"), Some("#ISO\tx\nFR\tFRA\t250\tFR\tFrance\n"));
        assert_eq!(g.len(), 2);
        let p = g.reverse(LatLon::new(48.86, 2.34)).unwrap();
        assert_eq!((p.city.as_str(), p.state.as_str(), p.country.as_str(), p.iso.as_str()), ("Paris", "Île-de-France", "France", "FR"));
        assert!(p.distance > 0.0 && p.distance < 2000.0);
        // no countryInfo row: the ISO code stands in
        let l = g.reverse(LatLon::new(51.5, -0.1)).unwrap();
        assert_eq!((l.city.as_str(), l.state.as_str(), l.country.as_str()), ("London", "England", "GB"));
        // open sea
        assert!(g.reverse(LatLon::new(0.0, 0.0)).is_none());
    }

    #[test]
    fn hostile_positions_find_nothing() {
        let g = GeoNames::parse(CITIES, None, None);
        for (la, lo) in [(f64::NAN, 0.0), (0.0, f64::NAN), (f64::INFINITY, 0.0), (91.0, 0.0), (0.0, -180.5), (-1e300, 1e300)] {
            assert!(g.reverse(LatLon::new(la, lo)).is_none(), "{la}, {lo}");
        }
        // the poles and the antimeridian are fine
        assert!(g.reverse(LatLon::new(90.0, 180.0)).is_none());
        assert!(g.reverse(LatLon::new(-90.0, -180.0)).is_none());
        assert!(GeoNames::default().reverse(LatLon::new(48.86, 2.34)).is_none());
    }

    #[test]
    fn across_the_antimeridian() {
        let fiji = GeoNames::parse("1\tX\tX\t\t-17.0\t179.9\tP\tP\tFJ\t\t\n", None, None);
        assert_eq!(fiji.reverse(LatLon::new(-17.0, -179.95)).unwrap().city, "X");
    }

    #[test]
    fn city_on_the_antimeridian_is_found() {
        let g = GeoNames::parse("1\tTaveuni\tTaveuni\t\t0.0\t180.0\tP\tPPL\tFJ\t\t01\n", None, None);
        for lon in [179.9, -179.9, 180.0, -180.0] {
            assert_eq!(g.reverse(LatLon::new(0.0, lon)).map(|p| p.city).as_deref(), Some("Taveuni"), "from {lon}");
        }
    }

    #[test]
    fn high_latitude_cities_two_cells_away() {
        let city = |lat: f64, lon: f64| GeoNames::parse(&format!("1\tX\tX\t\t{lat}\t{lon}\tP\tP\tNO\t\t\n"), None, None);
        // 70°N: 1° of longitude is ~38 km, so 45.6 km is two cells east
        assert_eq!(city(70.0, 0.9).reverse(LatLon::new(70.0, 2.1)).unwrap().city, "X");
        assert_eq!(city(70.0, 2.1).reverse(LatLon::new(70.0, 0.9)).unwrap().city, "X");
        // 64°N: 1° is ~48.7 km, so 1.02° is just inside range yet two cells apart
        assert_eq!(city(64.0, 0.99).reverse(LatLon::new(64.0, 2.01)).unwrap().city, "X");
        // across the antimeridian, two cells apart
        assert_eq!(city(70.0, 178.9).reverse(LatLon::new(70.0, -179.9)).unwrap().city, "X");
        assert_eq!(city(70.0, -179.9).reverse(LatLon::new(70.0, 178.9)).unwrap().city, "X");
        // still out of range (~80 km)
        assert!(city(70.0, 0.0).reverse(LatLon::new(70.0, 2.1)).is_none());
        // very near the pole every longitude is close
        assert_eq!(city(89.9, 0.5).reverse(LatLon::new(89.9, 100.0)).unwrap().city, "X");
    }

    #[test]
    fn junk_text_parses_to_nothing() {
        let g = GeoNames::parse("\t\t\t\t\t\n\u{0}\t\u{feff}\n", Some("\n\t\n"), Some("x"));
        assert!(g.is_empty());
        let long = format!("1\t{}\tx\t\t1\t1\tP\tP\tXX\n", "a".repeat(10_000));
        assert_eq!(GeoNames::parse(&long, None, None).reverse(LatLon::new(1.0, 1.0)).unwrap().city.len(), MAX_NAME);
    }

    #[test]
    fn load_from_a_folder() {
        let dir = std::env::temp_dir().join(format!("lightcraft-geocode-load-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(GeoNames::load(&dir), Err(GeocodeError::Missing(_))));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(GEONAMES_FILES[0]), "junk").unwrap();
        assert!(matches!(GeoNames::load(&dir), Err(GeocodeError::Invalid(_))));
        std::fs::write(dir.join(GEONAMES_FILES[0]), CITIES).unwrap();
        std::fs::write(dir.join(GEONAMES_FILES[2]), "GB\tGBR\t826\tUK\tUnited Kingdom\n").unwrap();
        let g = GeoNames::load(&dir).unwrap();
        assert_eq!(g.reverse(LatLon::new(51.5, -0.1)).unwrap().country, "United Kingdom");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
