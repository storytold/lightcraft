//! Never-crash harnesses (P6.2) for every untrusted input this crate parses: GPX / KML / GeoJSON
//! tracks, the GeoNames dump (zip + TSV), Nominatim answers and map tiles. Seeded mutation loops
//! from `dac-fuzzkit`, so they run in plain `cargo test`.

use crate::geocode::{GeoNames, parse_nominatim, parse_search};
use crate::mercator::LatLon;
use crate::{tiles, tracks, unzip};

const GPX: &str = r#"<?xml version="1.0"?><gpx version="1.1"><trk><name>x</name><trkseg>
<trkpt lat="48.1" lon="11.5"><ele>500</ele><time>2024-05-01T10:00:00Z</time></trkpt>
<trkpt lat="48.2" lon="11.6"><time>2024-05-01T10:10:00+02:00</time></trkpt></trkseg></trk>
<wpt lat="1" lon="2"/><rte><rtept lat="3" lon="4"/></rte></gpx>"#;
const KML: &str = "<kml><Placemark><LineString><coordinates>11.5,48.1,0 11.6,48.2 bad 200,0</coordinates></LineString></Placemark>\
<gx:Track><when>2024-05-01T10:00:00Z</when><gx:coord>11.7 48.3 5</gx:coord></gx:Track></kml>";
const GEOJSON: &str = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"LineString","coordinates":[[11.5,48.1],[11.6,48.2]]}},
{"type":"Feature","geometry":{"type":"MultiLineString","coordinates":[[[1,2],[3,4]]]}},{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]}]}]}"#;
const CITIES: &str = "2988507\tParis\tParis\t\t48.85341\t2.3488\tP\tPPLC\tFR\t\t11\t75\t\t\t2138551\t\t42\tEurope/Paris\t2024-01-01\n\
2643743\tLondon\tLondon\t\t51.50853\t-0.12574\tP\tPPLC\tGB\t\tENG\t\t\t\t8961989\t\t25\tEurope/London\t2024-01-01\n";

#[test]
fn tracks_never_panic() {
    dac_fuzzkit::run_str("geo.tracks", &[GPX, KML, GEOJSON], 3000, |s| {
        for name in ["t.gpx", "t.kml", "t.geojson", "t"] {
            if let Ok(t) = tracks::parse(name, s) {
                let _ = t.points();
            }
        }
    });
}

#[test]
fn geonames_never_panic() {
    let admin = "FR.11\tÎle-de-France\nGB.ENG\tEngland\n";
    let countries = "#ISO\tx\nFR\tFRA\t250\tFR\tFrance\n";
    dac_fuzzkit::run_str("geo.geonames", &[CITIES, admin, countries], 2000, |s| {
        for g in [GeoNames::parse(s, Some(s), Some(s)), GeoNames::parse(CITIES, Some(s), Some(s))] {
            for p in [LatLon::new(48.86, 2.34), LatLon::new(f64::NAN, 1.0), LatLon::new(90.0, 180.0), LatLon::new(-1e300, 1e300)] {
                let _ = g.reverse(p);
            }
        }
    });
}

#[test]
fn geonames_zip_never_panics() {
    let zip = unzip::make("cities15000.txt", CITIES.as_bytes());
    dac_fuzzkit::run("geo.unzip", &[&zip], 3000, |b| {
        let _ = unzip::extract(b, "cities15000.txt", 1 << 20);
        let _ = unzip::extract(b, "", 16);
    });
}

#[test]
fn nominatim_answers_never_panic() {
    let rev = r#"{"address":{"suburb":"Le Marais","city":"Paris","state":"Île-de-France","country":"France","country_code":"fr"}}"#;
    let search = r#"[{"lat":"48.85","lon":"2.35","display_name":"Paris"},{"lat":"x"},{"lat":1e999,"lon":-1}]"#;
    dac_fuzzkit::run_str("geo.nominatim", &[rev, search], 2000, |s| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
            let _ = parse_nominatim(&v);
            let _ = parse_search(&v);
        }
    });
}

#[test]
fn tiles_never_panic() {
    let mut png = Vec::new();
    let ok = image::RgbaImage::from_pixel(16, 16, image::Rgba([1, 2, 3, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .is_ok();
    assert!(ok);
    dac_fuzzkit::run("geo.tile", &[&png], 1500, |b| {
        let _ = tiles::decode_tile(b);
    });
}
