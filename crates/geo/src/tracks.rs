//! Tracks to draw on the map: GPX (parsed by `dac_meta::parse_gpx`, which also drives
//! Auto-Tag), KML (`<coordinates>` and `<gx:coord>`) and GeoJSON (LineString, MultiLineString,
//! Point, in Features and FeatureCollections).

use serde::Serialize;
use serde_json::Value;

use crate::GeoError;
use crate::mercator::LatLon;

/// Points kept per track (a display limit; hostile input stops here).
const MAX_POINTS: usize = 2_000_000;
const MAX_DEPTH: usize = 32;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Track {
    pub name: String,
    /// Connected runs of points.
    pub segments: Vec<Vec<LatLon>>,
    /// UTC seconds of the first and last timed point (GPX only).
    pub span: Option<(f64, f64)>,
    /// Can be used for Auto-Tag (GPX with times).
    pub timed: bool,
}

impl Track {
    pub fn points(&self) -> usize {
        self.segments.iter().map(Vec::len).sum()
    }

    pub fn all_points(&self) -> impl Iterator<Item = LatLon> + '_ {
        self.segments.iter().flatten().copied()
    }
}

/// Parse by content (GPX, KML or GeoJSON); `name` is shown in the track list.
pub fn parse(name: &str, text: &str) -> Result<Track, GeoError> {
    let head = text.trim_start();
    let t = if head.starts_with('{') {
        parse_geojson(text)?
    } else if text.contains("<gpx") {
        parse_gpx(text)?
    } else if text.contains("<kml") {
        parse_kml(text)?
    } else {
        return Err(GeoError::Invalid("not a GPX, KML or GeoJSON track".into()));
    };
    if t.points() == 0 {
        return Err(GeoError::Invalid("the file has no track points".into()));
    }
    Ok(Track { name: name.to_string(), ..t })
}

fn parse_gpx(text: &str) -> Result<Track, GeoError> {
    let log = dac_meta::parse_gpx(text).map_err(|e| GeoError::Invalid(e.to_string()))?;
    let mut segments: Vec<Vec<LatLon>> = Vec::new();
    let mut last = None;
    for p in &log.points {
        if last != Some(p.segment) || segments.is_empty() {
            segments.push(Vec::new());
            last = Some(p.segment);
        }
        if let Some(s) = segments.last_mut() {
            s.push(LatLon::new(p.latitude, p.longitude));
        }
    }
    Ok(Track { name: String::new(), segments, span: log.span(), timed: !log.points.is_empty() })
}

/// `lon,lat[,alt]` tuples separated by whitespace.
fn kml_coords(s: &str, out: &mut Vec<LatLon>) {
    for tuple in s.split_whitespace() {
        let mut it = tuple.split(',').map(|v| v.trim().parse::<f64>());
        if let (Some(Ok(lon)), Some(Ok(lat))) = (it.next(), it.next()) {
            let p = LatLon::new(lat, lon);
            if p.is_valid() && out.len() < MAX_POINTS {
                out.push(p);
            }
        }
    }
}

fn parse_kml(text: &str) -> Result<Track, GeoError> {
    let mut segments = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("<coordinates>") {
        let after = rest.get(i + 13..).unwrap_or("");
        let Some(j) = after.find("</coordinates>") else { break };
        let mut seg = Vec::new();
        kml_coords(after.get(..j).unwrap_or(""), &mut seg);
        if !seg.is_empty() {
            segments.push(seg);
        }
        rest = after.get(j..).unwrap_or("");
    }
    // gx:Track: <gx:coord>lon lat alt</gx:coord>
    let mut gx = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("<gx:coord>") {
        let after = rest.get(i + 10..).unwrap_or("");
        let Some(j) = after.find("</gx:coord>") else { break };
        let v: Vec<f64> = after.get(..j).unwrap_or("").split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if let (Some(lon), Some(lat)) = (v.first(), v.get(1)) {
            let p = LatLon::new(*lat, *lon);
            if p.is_valid() && gx.len() < MAX_POINTS {
                gx.push(p);
            }
        }
        rest = after.get(j..).unwrap_or("");
    }
    if !gx.is_empty() {
        segments.push(gx);
    }
    Ok(Track { segments, ..Track::default() })
}

fn geo_point(v: &Value) -> Option<LatLon> {
    let a = v.as_array()?;
    let p = LatLon::new(a.get(1)?.as_f64()?, a.first()?.as_f64()?);
    p.is_valid().then_some(p)
}

fn geo_line(v: &Value) -> Vec<LatLon> {
    v.as_array().map(|a| a.iter().filter_map(geo_point).take(MAX_POINTS).collect()).unwrap_or_default()
}

fn walk_geojson(v: &Value, depth: usize, out: &mut Vec<Vec<LatLon>>) {
    if depth > MAX_DEPTH {
        return;
    }
    let coords = v.get("coordinates");
    match v.get("type").and_then(Value::as_str) {
        Some("FeatureCollection") => {
            for f in v.get("features").and_then(Value::as_array).into_iter().flatten() {
                walk_geojson(f, depth + 1, out);
            }
        }
        Some("Feature") => {
            if let Some(g) = v.get("geometry") {
                walk_geojson(g, depth + 1, out);
            }
        }
        Some("GeometryCollection") => {
            for g in v.get("geometries").and_then(Value::as_array).into_iter().flatten() {
                walk_geojson(g, depth + 1, out);
            }
        }
        Some("LineString") | Some("MultiPoint") => {
            if let Some(c) = coords {
                out.push(geo_line(c));
            }
        }
        Some("MultiLineString") | Some("Polygon") => {
            for l in coords.and_then(Value::as_array).into_iter().flatten() {
                out.push(geo_line(l));
            }
        }
        Some("Point") => {
            if let Some(p) = coords.and_then(geo_point) {
                out.push(vec![p]);
            }
        }
        _ => {}
    }
}

fn parse_geojson(text: &str) -> Result<Track, GeoError> {
    let v: Value = serde_json::from_str(text).map_err(|e| GeoError::Invalid(format!("GeoJSON: {e}")))?;
    let mut segments = Vec::new();
    walk_geojson(&v, 0, &mut segments);
    segments.retain(|s| !s.is_empty());
    Ok(Track { segments, ..Track::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpx_kml_geojson() {
        let gpx = r#"<?xml version="1.0"?><gpx version="1.1"><trk><trkseg>
            <trkpt lat="48.1" lon="11.5"><time>2024-05-01T10:00:00Z</time></trkpt>
            <trkpt lat="48.2" lon="11.6"><time>2024-05-01T10:10:00Z</time></trkpt></trkseg></trk></gpx>"#;
        let t = parse("walk.gpx", gpx).unwrap();
        assert_eq!((t.points(), t.timed, t.name.as_str()), (2, true, "walk.gpx"));
        assert!(t.span.is_some());

        let kml = "<kml><Placemark><LineString><coordinates>11.5,48.1,0 11.6,48.2 bad 200,0</coordinates></LineString></Placemark>\
                   <gx:Track><gx:coord>11.7 48.3 5</gx:coord></gx:Track></kml>";
        let t = parse("a.kml", kml).unwrap();
        assert_eq!(t.segments.len(), 2);
        assert_eq!(t.segments[0], vec![LatLon::new(48.1, 11.5), LatLon::new(48.2, 11.6)]);

        let gj = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"LineString","coordinates":[[11.5,48.1],[11.6,48.2]]}},
                    {"type":"Feature","geometry":{"type":"MultiLineString","coordinates":[[[1,2],[3,4]]]}}]}"#;
        let t = parse("b.geojson", gj).unwrap();
        assert_eq!((t.segments.len(), t.points()), (2, 4));
    }

    #[test]
    fn junk_is_an_error() {
        assert!(parse("x", "hello").is_err());
        assert!(parse("x", "{").is_err());
        assert!(parse("x", r#"{"type":"Feature"}"#).is_err());
        assert!(parse("x", "<kml><coordinates>").is_err());
        // deep nesting stops
        let deep = format!("{}{}", r#"{"type":"GeometryCollection","geometries":["#.repeat(60), "]}".repeat(60));
        assert!(parse("x", &deep).is_err());
    }
}
