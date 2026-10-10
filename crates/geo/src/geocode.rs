//! Reverse geocoding: a position → Sublocation / City / State / Country / ISO country code.
//! Opt-in, two providers:
//!
//! - **Offline GeoNames** ([`GeoNames`]): the `cities15000` dump (CC-BY 4.0, attribution:
//!   "GeoNames, geonames.org") with `admin1CodesASCII.txt` (state names) and `countryInfo.txt`
//!   (country names), downloaded on request into the settings folder ([`download_geonames`]); then
//!   every lookup is local. Nearest populated place within [`OFFLINE_MAX_KM`].
//! - **Online, Nominatim-compatible** ([`Nominatim`]): one request per photo position, at most
//!   one per second (the public server's usage policy), only when the caller passes consent.
//!
//! Nothing ever leaves the machine without [`Consent::Granted`].

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use crate::GeoError;
use crate::mercator::{LatLon, distance_m};

/// Offline lookups farther than this from any place find nothing.
pub const OFFLINE_MAX_KM: f64 = 50.0;
/// The files [`download_geonames`] fetches.
pub const GEONAMES_FILES: [&str; 3] = ["cities15000.txt", "admin1CodesASCII.txt", "countryInfo.txt"];
pub const GEONAMES_BASE: &str = "https://download.geonames.org/export/dump/";
pub const GEONAMES_ATTRIBUTION: &str = "Place names: GeoNames (geonames.org), CC-BY 4.0";
pub const NOMINATIM_DEFAULT: &str = "https://nominatim.openstreetmap.org/reverse";
pub const NOMINATIM_ATTRIBUTION: &str = "Place names: © OpenStreetMap contributors (Nominatim)";
/// Places kept from a dataset (cities15000 has ~33 000; cities500 ~200 000).
const MAX_PLACES: usize = 2_000_000;

/// The user's answer to "may the app send photo positions to a server?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consent {
    Granted,
    Denied,
}

/// What a lookup fills in (empty strings: unknown).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Place {
    pub sublocation: String,
    pub city: String,
    pub state: String,
    pub country: String,
    /// ISO 3166-1 alpha-2, upper case.
    pub iso: String,
}

#[derive(Clone, Debug)]
struct City {
    name: String,
    at: LatLon,
    country: String,
    admin1: String,
}

/// An offline GeoNames index: cities bucketed by 1° cells.
#[derive(Clone, Debug, Default)]
pub struct GeoNames {
    cities: Vec<City>,
    cells: HashMap<(i32, i32), Vec<u32>>,
    admin1: HashMap<String, String>,
    countries: HashMap<String, String>,
}

fn cell(p: LatLon) -> (i32, i32) {
    (p.lat.floor() as i32, p.lon.floor() as i32)
}

impl GeoNames {
    /// From the dump's text: `cities` (tab-separated, the `cities*.txt` layout), optional
    /// `admin1CodesASCII.txt` and `countryInfo.txt`. Bad lines are skipped.
    pub fn parse(cities: &str, admin1: Option<&str>, countries: Option<&str>) -> GeoNames {
        let mut g = GeoNames::default();
        for line in cities.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            // 1 name, 4 latitude, 5 longitude, 8 country code, 10 admin1 code
            let (Some(name), Some(lat), Some(lon), Some(cc)) = (f.get(1), f.get(4), f.get(5), f.get(8)) else { continue };
            let (Ok(lat), Ok(lon)) = (lat.parse::<f64>(), lon.parse::<f64>()) else { continue };
            let at = LatLon::new(lat, lon);
            if !at.is_valid() || name.is_empty() || g.cities.len() >= MAX_PLACES {
                continue;
            }
            let i = g.cities.len() as u32;
            g.cells.entry(cell(at)).or_default().push(i);
            g.cities.push(City { name: name.to_string(), at, country: cc.to_ascii_uppercase(), admin1: f.get(10).unwrap_or(&"").to_string() });
        }
        for line in admin1.unwrap_or("").lines() {
            let mut f = line.split('\t');
            if let (Some(code), Some(name)) = (f.next(), f.next()) {
                g.admin1.insert(code.to_string(), name.to_string());
            }
        }
        for line in countries.unwrap_or("").lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = line.split('\t').collect();
            if let (Some(iso), Some(name)) = (f.first(), f.get(4)) {
                g.countries.insert(iso.to_ascii_uppercase(), name.to_string());
            }
        }
        g
    }

    /// Load from a folder holding [`GEONAMES_FILES`].
    pub fn load(dir: &Path) -> Result<GeoNames, GeoError> {
        let read = |n: &str| std::fs::read_to_string(dir.join(n));
        let cities = read(GEONAMES_FILES[0]).map_err(|e| GeoError::Io(format!("the offline place names aren't downloaded ({e})")))?;
        Ok(GeoNames::parse(&cities, read(GEONAMES_FILES[1]).ok().as_deref(), read(GEONAMES_FILES[2]).ok().as_deref()))
    }

    pub fn len(&self) -> usize {
        self.cities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cities.is_empty()
    }

    /// The nearest place within [`OFFLINE_MAX_KM`].
    pub fn reverse(&self, p: LatLon) -> Option<Place> {
        if !p.is_valid() {
            return None;
        }
        let (cy, cx) = cell(p);
        let mut best: Option<(f64, &City)> = None;
        // ±1° covers 50 km everywhere except near the poles (where there are no cities)
        for dy in -1..=1 {
            for dx in -1..=1 {
                let mut x = cx + dx;
                if x < -180 {
                    x += 360;
                } else if x >= 180 {
                    x -= 360;
                }
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
        (d <= OFFLINE_MAX_KM * 1000.0).then(|| Place {
            sublocation: String::new(),
            city: c.name.clone(),
            state: self.admin1.get(&format!("{}.{}", c.country, c.admin1)).cloned().unwrap_or_default(),
            country: self.countries.get(&c.country).cloned().unwrap_or_else(|| c.country.clone()),
            iso: c.country.clone(),
        })
    }
}

/// Parse a Nominatim `/reverse?format=jsonv2` answer.
pub fn parse_nominatim(v: &serde_json::Value) -> Option<Place> {
    let a = v.get("address")?;
    let s = |keys: &[&str]| keys.iter().find_map(|k| a.get(*k).and_then(serde_json::Value::as_str)).unwrap_or("").to_string();
    let p = Place {
        sublocation: s(&["suburb", "neighbourhood", "quarter", "hamlet", "road"]),
        city: s(&["city", "town", "village", "municipality"]),
        state: s(&["state", "region", "province"]),
        country: s(&["country"]),
        iso: s(&["country_code"]).to_ascii_uppercase(),
    };
    (p != Place::default()).then_some(p)
}

/// Parse a Nominatim `/search?format=jsonv2` answer: `(display name, position)`.
pub fn parse_search(v: &serde_json::Value) -> Vec<(String, LatLon)> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let f = |k: &str| r.get(k).and_then(|x| x.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| x.as_f64()));
            let p = LatLon::new(f("lat")?, f("lon")?);
            let name = r.get("display_name").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
            p.is_valid().then_some((name, p))
        })
        .take(5)
        .collect()
}

/// Percent-encode a query string value.
pub fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
pub use online::*;

#[cfg(not(target_arch = "wasm32"))]
mod online {
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use dac_net::{Client, ClientConfig};

    use super::*;

    /// A Nominatim-compatible endpoint (`…/reverse`), rate-limited to one request per `interval`.
    #[derive(Debug)]
    pub struct Nominatim {
        client: Client,
        pub endpoint: String,
        pub interval: Duration,
        /// `Accept-Language` (e.g. "en").
        pub language: String,
        last: Mutex<Option<Instant>>,
    }

    impl Nominatim {
        pub fn new(endpoint: &str) -> Result<Nominatim, GeoError> {
            if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
                return Err(GeoError::Invalid("the geocoding endpoint starts with https://".into()));
            }
            let client = Client::new(ClientConfig { max_body: 1024 * 1024, stall_timeout: Duration::from_secs(20), ..ClientConfig::default() })
                .map_err(|e| GeoError::Net(e.to_string()))?;
            Ok(Nominatim { client, endpoint: endpoint.to_string(), interval: Duration::from_secs(1), language: "en".into(), last: Mutex::new(None) })
        }

        /// Sleep until the rate limit allows the next request.
        fn wait_turn(&self) {
            let mut last = self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(t) = *last {
                let since = t.elapsed();
                if since < self.interval {
                    std::thread::sleep(self.interval.saturating_sub(since));
                }
            }
            *last = Some(Instant::now());
        }

        /// One lookup. Refuses without consent; waits to respect the rate limit.
        pub fn reverse(&self, consent: Consent, p: LatLon) -> Result<Option<Place>, GeoError> {
            if consent != Consent::Granted {
                return Err(GeoError::NoConsent);
            }
            if !p.is_valid() {
                return Ok(None);
            }
            self.wait_turn();
            let sep = if self.endpoint.contains('?') { '&' } else { '?' };
            let url = format!("{}{sep}format=jsonv2&zoom=14&addressdetails=1&lat={:.6}&lon={:.6}", self.endpoint, p.lat, p.lon);
            let v: serde_json::Value = self
                .client
                .get(&url)
                .header("Accept-Language", &self.language)
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.json())
                .map_err(|e| GeoError::Net(format!("reverse geocoding: {e}")))?;
            Ok(parse_nominatim(&v))
        }
    }

    impl Nominatim {
        /// Forward search (the map's search box): `query` → up to 5 `(display name, position)`.
        /// The endpoint's `/reverse` is swapped for `/search`. Consent and rate limit as
        /// [`Nominatim::reverse`].
        pub fn search(&self, consent: Consent, query: &str) -> Result<Vec<(String, LatLon)>, GeoError> {
            if consent != Consent::Granted {
                return Err(GeoError::NoConsent);
            }
            let q = query.trim();
            if q.is_empty() || q.len() > 300 {
                return Ok(Vec::new());
            }
            self.wait_turn();
            let base = self.endpoint.strip_suffix("/reverse").map(|b| format!("{b}/search")).unwrap_or_else(|| self.endpoint.clone());
            let sep = if base.contains('?') { '&' } else { '?' };
            let url = format!("{base}{sep}format=jsonv2&limit=5&q={}", encode_query(q));
            let v: serde_json::Value = self
                .client
                .get(&url)
                .header("Accept-Language", &self.language)
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.json())
                .map_err(|e| GeoError::Net(format!("place search: {e}")))?;
            Ok(parse_search(&v))
        }
    }

    /// Download the GeoNames files into `dir` (consent required: it contacts geonames.org).
    /// `base` is [`GEONAMES_BASE`] unless testing. Returns the number of places.
    pub fn download_geonames(consent: Consent, base: &str, dir: &Path) -> Result<usize, GeoError> {
        if consent != Consent::Granted {
            return Err(GeoError::NoConsent);
        }
        let client = Client::new(ClientConfig { max_body: 64 * 1024 * 1024, ..ClientConfig::default() }).map_err(|e| GeoError::Net(e.to_string()))?;
        let get = |name: &str| -> Result<Vec<u8>, GeoError> {
            client
                .get(&format!("{base}{name}"))
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.bytes())
                .map_err(|e| GeoError::Net(format!("downloading {name}: {e}")))
        };
        std::fs::create_dir_all(dir)?;
        let zip = get("cities15000.zip")?;
        let cities = crate::unzip::extract(&zip, GEONAMES_FILES[0], 256 * 1024 * 1024)?;
        let parsed = GeoNames::parse(&String::from_utf8_lossy(&cities), None, None);
        if parsed.is_empty() {
            return Err(GeoError::Invalid("the downloaded place list is empty".into()));
        }
        let write = |name: &str, data: &[u8]| -> Result<(), GeoError> {
            let tmp = dir.join(format!("{name}.part"));
            std::fs::write(&tmp, data)?;
            std::fs::rename(&tmp, dir.join(name))?;
            Ok(())
        };
        for name in &GEONAMES_FILES[1..] {
            // optional: without them states are blank and countries are ISO codes
            match get(name) {
                Ok(b) => write(name, &b)?,
                Err(e) => log::warn!("{e}"),
            }
        }
        write(GEONAMES_FILES[0], &cities)?;
        Ok(parsed.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CITIES: &str = "2988507\tParis\tParis\t\t48.85341\t2.3488\tP\tPPLC\tFR\t\t11\t75\t\t\t2138551\t\t42\tEurope/Paris\t2024-01-01\n\
                          2643743\tLondon\tLondon\t\t51.50853\t-0.12574\tP\tPPLC\tGB\t\tENG\t\t\t\t8961989\t\t25\tEurope/London\t2024-01-01\n\
                          bad line\n\
                          1\tNowhere\tx\t\tNaN\t0\tP\tP\tXX\n";

    #[test]
    fn offline_nearest_place() {
        let g = GeoNames::parse(CITIES, Some("FR.11\tÎle-de-France\nGB.ENG\tEngland\n"), Some("#ISO\tx\nFR\tFRA\t250\tFR\tFrance\n"));
        assert_eq!(g.len(), 2);
        let p = g.reverse(LatLon::new(48.86, 2.34)).unwrap();
        assert_eq!(
            p,
            Place { sublocation: String::new(), city: "Paris".into(), state: "Île-de-France".into(), country: "France".into(), iso: "FR".into() }
        );
        let l = g.reverse(LatLon::new(51.5, -0.1)).unwrap();
        assert_eq!((l.city.as_str(), l.state.as_str(), l.country.as_str()), ("London", "England", "GB"));
        assert!(g.reverse(LatLon::new(0.0, 0.0)).is_none());
        assert!(g.reverse(LatLon::new(f64::NAN, 0.0)).is_none());
        // across the antimeridian cell boundary
        let fiji = GeoNames::parse("1\tX\tX\t\t-17.0\t179.9\tP\tP\tFJ\t\t\n", None, None);
        assert!(fiji.reverse(LatLon::new(-17.0, -179.95)).is_some());
    }

    #[test]
    fn nominatim_answers() {
        let v = serde_json::json!({"address": {"suburb": "Le Marais", "city": "Paris", "state": "Île-de-France", "country": "France", "country_code": "fr"}});
        let p = parse_nominatim(&v).unwrap();
        assert_eq!((p.sublocation.as_str(), p.iso.as_str()), ("Le Marais", "FR"));
        assert!(parse_nominatim(&serde_json::json!({"error": "Unable to geocode"})).is_none());
        let r = parse_search(&serde_json::json!([{"lat": "48.85", "lon": "2.35", "display_name": "Paris"}, {"lat": "x"}]));
        assert_eq!(r, vec![("Paris".to_string(), LatLon::new(48.85, 2.35))]);
        assert_eq!(encode_query("São Paulo & co"), "S%C3%A3o+Paulo+%26+co");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn online_needs_consent_and_works_against_a_fake_server() {
        use std::io::{BufRead, BufReader, Write};
        let n = Nominatim::new(NOMINATIM_DEFAULT).unwrap();
        assert_eq!(n.reverse(Consent::Denied, LatLon::new(1.0, 1.0)), Err(GeoError::NoConsent));
        assert_eq!(n.search(Consent::Denied, "Paris"), Err(GeoError::NoConsent));
        assert!(Nominatim::new("ftp://x").is_err());
        let dir = std::env::temp_dir().join(format!("dac-geo-geonames-{}", std::process::id()));
        assert_eq!(download_geonames(Consent::Denied, "http://127.0.0.1:1/", &dir), Err(GeoError::NoConsent));

        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let zip = crate::unzip::make("cities15000.txt", CITIES.as_bytes());
        let h = std::thread::spawn(move || {
            for _ in 0..5 {
                let (mut s, _) = l.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                }
                let body: Vec<u8> = if line.contains("/reverse") {
                    assert!(line.contains("lat=48.860000") && line.contains("format=jsonv2"), "{line}");
                    br#"{"address":{"city":"Paris","country":"France","country_code":"fr"}}"#.to_vec()
                } else if line.contains("cities15000.zip") {
                    zip.clone()
                } else if line.contains("countryInfo") {
                    b"FR\tFRA\t250\tFR\tFrance\n".to_vec()
                } else {
                    b"FR.11\tIle-de-France\n".to_vec()
                };
                let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
                let _ = s.write_all(&body);
            }
        });
        let mut n = Nominatim::new(&format!("http://127.0.0.1:{port}/reverse")).unwrap();
        n.interval = std::time::Duration::from_millis(1);
        let p = n.reverse(Consent::Granted, LatLon::new(48.86, 2.34)).unwrap().unwrap();
        assert_eq!((p.city.as_str(), p.iso.as_str()), ("Paris", "FR"));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(download_geonames(Consent::Granted, &format!("http://127.0.0.1:{port}/"), &dir).unwrap(), 2);
        let g = GeoNames::load(&dir).unwrap();
        assert_eq!(g.reverse(LatLon::new(48.86, 2.34)).unwrap().state, "Ile-de-France");
        // one more connection for the thread to finish
        let _ = n.reverse(Consent::Granted, LatLon::new(48.86, 2.34));
        h.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
