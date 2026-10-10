//! `map.*`: the Map module's library commands — pins and clusters, geotagging, saved locations
//! (with the private flag export honours), track logs, and opt-in reverse geocoding. The map view
//! itself (tiles, panning) lives in the UI; tile servers and geocoders in `dac-geo`.

use serde_json::{Value, json};

use dac_catalog::{Op, PhotoId};
use dac_geo::{LatLon, SavedLocation};

use super::{CommandSpec, always, bad, bool_or, cmd, f64_or, has_selection, str_param};
use crate::Result;

/// Which photos the map shows.
#[derive(Clone, Debug, PartialEq)]
pub enum LocationFilter {
    /// Every photo with a position.
    All,
    /// Only photos with a position (same pins as All; counts differ).
    Tagged,
    /// Photos without a position (no pins: the filmstrip shows them).
    Untagged,
    /// Photos inside a saved location.
    Location(String),
}

impl LocationFilter {
    pub fn parse(s: &str) -> Option<LocationFilter> {
        Some(match s {
            "" | "all" | "visible" => LocationFilter::All,
            "tagged" => LocationFilter::Tagged,
            "untagged" => LocationFilter::Untagged,
            _ => LocationFilter::Location(s.strip_prefix("location:")?.to_string()),
        })
    }

    pub fn key(&self) -> String {
        match self {
            LocationFilter::All => "all".into(),
            LocationFilter::Tagged => "tagged".into(),
            LocationFilter::Untagged => "untagged".into(),
            LocationFilter::Location(n) => format!("location:{n}"),
        }
    }
}

/// The photos in view (the current source and filter) that pass `filter`, with their positions
/// (`None` for untagged).
pub fn filtered(s: &mut crate::Session, filter: &LocationFilter) -> Vec<(PhotoId, Option<LatLon>)> {
    let ids = s.visible().to_vec();
    let place = match filter {
        LocationFilter::Location(n) => s.catalog.saved_location(n).cloned(),
        _ => None,
    };
    ids.into_iter()
        .filter_map(|id| {
            let gps = s.catalog.photo(id)?.meta.gps.map(|(a, b)| LatLon::new(a, b)).filter(|p| p.is_valid());
            let keep = match filter {
                LocationFilter::All => true,
                LocationFilter::Tagged => gps.is_some(),
                LocationFilter::Untagged => gps.is_none(),
                LocationFilter::Location(_) => place.as_ref().is_some_and(|l| gps.is_some_and(|g| l.contains(g))),
            };
            keep.then_some((id, gps))
        })
        .collect()
}

fn locations_json(s: &crate::Session) -> Value {
    let positions: Vec<LatLon> = s.catalog.photos().filter_map(|p| p.meta.gps.map(|(a, b)| LatLon::new(a, b))).collect();
    let list: Vec<Value> = s
        .catalog
        .saved_locations()
        .map(|l| {
            let n = positions.iter().filter(|p| l.contains(**p)).count();
            json!({"name": l.name, "lat": l.lat, "lon": l.lon, "radius": l.radius, "private": l.private, "photos": n})
        })
        .collect();
    json!({"locations": list})
}

fn round7(v: f64) -> f64 {
    (v * 1e7).round() / 1e7
}

fn geotag(s: &mut crate::Session, p: &Value) -> Result<Value> {
    const C: &str = "map.geotag";
    let ids = s.targets(p);
    if ids.is_empty() {
        return Err(bad(C, "no photos to geotag"));
    }
    let gps = if bool_or(p, "clear", false) {
        None
    } else {
        let (lat, lon) = (p.get("lat").and_then(Value::as_f64), p.get("lon").and_then(Value::as_f64));
        let (Some(lat), Some(lon)) = (lat, lon) else { return Err(bad(C, "give `lat` and `lon`, or `clear: true`")) };
        if !LatLon::new(lat, lon).is_valid() {
            return Err(bad(C, format!("{lat}, {lon} is not a position")));
        }
        Some((round7(lat), round7(lon)))
    };
    let mut ops = Vec::new();
    for id in &ids {
        let Some(ph) = s.catalog.photo(*id) else { continue };
        if ph.meta.gps != gps {
            let mut meta = ph.meta.clone();
            meta.gps = gps;
            ops.push(Op::SetMeta { id: *id, meta: Box::new(meta) });
        }
    }
    let changed = ops.len();
    if changed > 0 {
        s.commit(if gps.is_some() { "Geotag" } else { "Remove GPS" }, Op::Batch { ops })?;
    }
    Ok(json!({"photos": ids.len(), "changed": changed, "gps": gps.map(|g| [g.0, g.1])}))
}

fn save_location(s: &mut crate::Session, p: &Value) -> Result<Value> {
    const C: &str = "map.saveLocation";
    let name = str_param(p, "name").ok_or_else(|| bad(C, "missing `name`"))?;
    let old = str_param(p, "rename").map(str::to_string);
    let existing = s.catalog.saved_location(old.as_deref().unwrap_or(name)).cloned();
    let lat = p.get("lat").and_then(Value::as_f64).or(existing.as_ref().map(|l| l.lat)).ok_or_else(|| bad(C, "missing `lat`"))?;
    let lon = p.get("lon").and_then(Value::as_f64).or(existing.as_ref().map(|l| l.lon)).ok_or_else(|| bad(C, "missing `lon`"))?;
    let radius = f64_or(p, "radius", existing.as_ref().map_or(100.0, |l| l.radius));
    let private = bool_or(p, "private", existing.as_ref().is_some_and(|l| l.private));
    let loc = SavedLocation::new(name, lat, lon, radius, private).map_err(|e| bad(C, e.to_string()))?;
    let mut ops = Vec::new();
    if let Some(o) = old.filter(|o| !o.trim().eq_ignore_ascii_case(name.trim())) {
        if s.catalog.saved_location(&o).is_none() {
            return Err(bad(C, format!("no saved location `{o}`")));
        }
        ops.push(Op::SetSavedLocation { name: o, location: None });
    }
    ops.push(Op::SetSavedLocation { name: loc.name.clone(), location: Some(loc.clone()) });
    s.commit("Save Location", Op::Batch { ops })?;
    Ok(json!({"location": loc}))
}

fn delete_location(s: &mut crate::Session, p: &Value) -> Result<Value> {
    const C: &str = "map.deleteLocation";
    let name = str_param(p, "name").ok_or_else(|| bad(C, "missing `name`"))?;
    if s.catalog.saved_location(name).is_none() {
        return Err(bad(C, format!("no saved location `{name}`")));
    }
    s.commit("Delete Location", Op::SetSavedLocation { name: name.to_string(), location: None })?;
    Ok(json!({"deleted": name}))
}

fn pins(s: &mut crate::Session, p: &Value) -> Result<Value> {
    const C: &str = "map.pins";
    let filter =
        LocationFilter::parse(str_param(p, "filter").unwrap_or("all")).ok_or_else(|| bad(C, "filter is all|tagged|untagged|location:<name>"))?;
    let zoom = f64_or(p, "zoom", 3.0);
    if !zoom.is_finite() {
        return Err(bad(C, "zoom must be a number"));
    }
    let cell = f64_or(p, "cellPx", 48.0).clamp(1.0, 1024.0);
    let photos = filtered(s, &filter);
    let untagged = photos.iter().filter(|(_, g)| g.is_none()).count();
    let pts: Vec<(PhotoId, LatLon)> = photos.iter().filter_map(|(id, g)| g.map(|g| (*id, g))).collect();
    let clusters: Vec<Value> = dac_geo::cluster(&pts, zoom, cell)
        .into_iter()
        .map(|c| json!({"lat": c.centre.lat, "lon": c.centre.lon, "count": c.items.len(), "ids": c.items.iter().take(100).map(|i| i.0).collect::<Vec<_>>()}))
        .collect();
    let bounds = dac_geo::mercator::fit(&pts.iter().map(|p| p.1).collect::<Vec<_>>(), 1000.0, 700.0);
    Ok(json!({
        "filter": filter.key(), "tagged": pts.len(), "untagged": untagged, "clusters": clusters,
        "fit": bounds.map(|(c, z)| json!({"lat": c.lat, "lon": c.lon, "zoom": z})),
    }))
}

fn read_track(p: &Value, c: &str) -> Result<dac_geo::tracks::Track> {
    let (name, text) = match (str_param(p, "text"), str_param(p, "path")) {
        (Some(t), _) => ("track".to_string(), t.to_string()),
        (None, Some(path)) => {
            let meta = std::fs::metadata(path).map_err(|e| bad(c, format!("can't read {path}: {e}")))?;
            if meta.len() > 512 * 1024 * 1024 {
                return Err(bad(c, "the track file is larger than 512 MB"));
            }
            let t = std::fs::read_to_string(path).map_err(|e| bad(c, format!("can't read {path}: {e}")))?;
            let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (name, t)
        }
        (None, None) => return Err(bad(c, "missing `path` (a .gpx, .kml or .geojson file) or `text`")),
    };
    dac_geo::tracks::parse(&name, &text).map_err(|e| bad(c, e.to_string()))
}

fn track_info(_s: &mut crate::Session, p: &Value) -> Result<Value> {
    let t = read_track(p, "map.track")?;
    let pts: Vec<LatLon> = t.all_points().collect();
    let fit = dac_geo::mercator::fit(&pts, 1000.0, 700.0);
    let iso = |x: f64| format!("{}Z", dac_catalog::dates::civil(x.floor() as i64));
    Ok(json!({
        "name": t.name, "points": t.points(), "segments": t.segments.len(), "timed": t.timed,
        "start": t.span.map(|s| iso(s.0)), "end": t.span.map(|s| iso(s.1)),
        "fit": fit.map(|(c, z)| json!({"lat": c.lat, "lon": c.lon, "zoom": z})),
    }))
}

/// The camera clock's offset from a photo taken at a known point of the track: the track's time
/// at the point nearest `lat, lon` vs. the photo's capture time.
fn track_offset(s: &mut crate::Session, p: &Value) -> Result<Value> {
    const C: &str = "map.trackOffset";
    let text = match (str_param(p, "gpx"), str_param(p, "path")) {
        (Some(t), _) => t.to_string(),
        (None, Some(path)) => std::fs::read_to_string(path).map_err(|e| bad(C, format!("can't read {path}: {e}")))?,
        (None, None) => return Err(bad(C, "missing `path` (a .gpx file) or `gpx`")),
    };
    let log = dac_meta::parse_gpx(&text).map_err(|e| bad(C, e.to_string()))?;
    let id = p
        .get("id")
        .and_then(Value::as_u64)
        .map(PhotoId)
        .or_else(|| s.active())
        .ok_or_else(|| bad(C, "missing `id` (the photo taken at the known point)"))?;
    let ph = s.catalog.photo(id).ok_or_else(|| bad(C, format!("no photo {}", id.0)))?;
    let at = match (p.get("lat").and_then(Value::as_f64), p.get("lon").and_then(Value::as_f64)) {
        (Some(a), Some(b)) => LatLon::new(a, b),
        _ => ph.meta.gps.map(|(a, b)| LatLon::new(a, b)).ok_or_else(|| bad(C, "give `lat` and `lon` (or geotag the photo first)"))?,
    };
    let dt = ph.captured.as_deref().and_then(dac_meta::DateTime::parse_iso).ok_or_else(|| bad(C, "the photo has no capture time"))?;
    let nearest = log
        .points
        .iter()
        .min_by(|a, b| {
            let da = dac_geo::distance_m(at, LatLon::new(a.latitude, a.longitude));
            let db = dac_geo::distance_m(at, LatLon::new(b.latitude, b.longitude));
            da.total_cmp(&db)
        })
        .ok_or_else(|| bad(C, "the track log has no timed points"))?;
    let local = dt.unix_seconds() + dt.offset_minutes.map_or(0, i64::from) * 60;
    // capture time as written on the camera (wall clock) minus the true UTC time = the zone offset
    let minutes = ((local as f64 - nearest.time) / 60.0).round() as i64;
    if minutes.abs() > 18 * 60 {
        return Err(bad(C, "the photo's time and the track are more than 18 hours apart: is it the right track?"));
    }
    let sign = if minutes < 0 { '-' } else { '+' };
    let offset = format!("{sign}{:02}:{:02}", minutes.abs() / 60, minutes.abs() % 60);
    Ok(json!({"offset": offset, "minutes": minutes, "distance": dac_geo::distance_m(at, LatLon::new(nearest.latitude, nearest.longitude))}))
}

#[cfg(not(target_arch = "wasm32"))]
mod geocoding {
    use super::*;
    use dac_geo::geocode::{Consent, GEONAMES_ATTRIBUTION, GeoNames, NOMINATIM_ATTRIBUTION, NOMINATIM_DEFAULT, Nominatim, Place};

    pub fn geonames_dir(p: &Value) -> Option<std::path::PathBuf> {
        str_param(p, "dataset").map(std::path::PathBuf::from).or_else(|| crate::config::config_dir().map(|d| d.join("geonames")))
    }

    pub fn status(_s: &mut crate::Session, p: &Value) -> Result<Value> {
        let dir = geonames_dir(p);
        let file = dir.as_ref().map(|d| d.join(dac_geo::geocode::GEONAMES_FILES[0]));
        let size = file.as_ref().and_then(|f| std::fs::metadata(f).ok()).map(|m| m.len());
        Ok(json!({
            "offline": {"downloaded": size.is_some(), "bytes": size, "path": dir, "attribution": GEONAMES_ATTRIBUTION, "source": dac_geo::geocode::GEONAMES_BASE},
            "online": {"endpoint": NOMINATIM_DEFAULT, "attribution": NOMINATIM_ATTRIBUTION, "rateLimit": "1 request per second"},
        }))
    }

    pub fn download(_s: &mut crate::Session, p: &Value) -> Result<Value> {
        const C: &str = "map.geonamesDownload";
        let consent = if bool_or(p, "consent", false) { Consent::Granted } else { Consent::Denied };
        let dir = geonames_dir(p).ok_or_else(|| bad(C, "no settings folder: give `dataset`"))?;
        let base = str_param(p, "base").unwrap_or(dac_geo::geocode::GEONAMES_BASE);
        let n = dac_geo::geocode::download_geonames(consent, base, &dir).map_err(|e| bad(C, e.to_string()))?;
        Ok(json!({"places": n, "path": dir, "attribution": GEONAMES_ATTRIBUTION}))
    }

    pub fn reverse(s: &mut crate::Session, p: &Value) -> Result<Value> {
        const C: &str = "map.reverseGeocode";
        let provider = str_param(p, "provider").unwrap_or("offline");
        if provider == "given" {
            return apply_given(s, p);
        }
        let overwrite = bool_or(p, "overwrite", false);
        let ids = s.targets(p);
        let todo: Vec<(PhotoId, LatLon)> = ids
            .iter()
            .filter_map(|id| {
                let ph = s.catalog.photo(*id)?;
                let g = ph.meta.gps.map(|(a, b)| LatLon::new(a, b))?;
                let empty = ph.meta.city.is_empty() && ph.meta.state.is_empty() && ph.meta.country.is_empty() && ph.meta.location.is_empty();
                (overwrite || empty).then_some((*id, g))
            })
            .collect();
        let mut lookup: Box<dyn FnMut(LatLon) -> Result<Option<Place>>> = match provider {
            "offline" => {
                let dir = geonames_dir(p).ok_or_else(|| bad(C, "no settings folder: give `dataset`"))?;
                let g = GeoNames::load(&dir).map_err(|e| bad(C, format!("{e} — run map.geonamesDownload first")))?;
                Box::new(move |at| Ok(g.reverse(at)))
            }
            "online" => {
                let consent = if bool_or(p, "consent", false) { Consent::Granted } else { Consent::Denied };
                if consent != Consent::Granted {
                    return Err(bad(C, "online reverse geocoding sends photo positions to a server: pass `consent: true` (the user's opt-in)"));
                }
                if todo.len() > 500 {
                    return Err(bad(C, "online lookups are limited to 500 photos per call (one per second)"));
                }
                let n = Nominatim::new(str_param(p, "endpoint").unwrap_or(NOMINATIM_DEFAULT)).map_err(|e| bad(C, e.to_string()))?;
                Box::new(move |at| n.reverse(consent, at).map_err(|e| bad(C, e.to_string())))
            }
            other => return Err(bad(C, format!("provider is offline|online, not `{other}`"))),
        };
        let mut ops = Vec::new();
        let mut found = Vec::new();
        let mut cache: Vec<(LatLon, Option<Place>)> = Vec::new();
        for (id, at) in todo.iter().copied() {
            // photos a few metres apart share one lookup
            let place = match cache.iter().find(|(q, _)| dac_geo::distance_m(*q, at) < 50.0) {
                Some((_, pl)) => pl.clone(),
                None => {
                    let pl = lookup(at)?;
                    cache.push((at, pl.clone()));
                    pl
                }
            };
            let Some(pl) = place else { continue };
            let Some(ph) = s.catalog.photo(id) else { continue };
            let mut meta = ph.meta.clone();
            meta.location = pl.sublocation.clone();
            meta.city = pl.city.clone();
            meta.state = pl.state.clone();
            meta.country = pl.country.clone();
            if meta != ph.meta {
                ops.push(Op::SetMeta { id, meta: Box::new(meta) });
            }
            found.push(json!({"id": id.0, "place": pl}));
        }
        if !bool_or(p, "dryRun", false) && !ops.is_empty() {
            s.commit("Reverse Geocode", Op::Batch { ops })?;
        }
        let attribution = if provider == "online" { NOMINATIM_ATTRIBUTION } else { GEONAMES_ATTRIBUTION };
        Ok(json!({"looked": todo.len(), "found": found.len(), "photos": found, "attribution": attribution}))
    }
}

/// `provider: "given"`: places looked up elsewhere (the UI's background lookups), applied as one step.
#[cfg(not(target_arch = "wasm32"))]
fn apply_given(s: &mut crate::Session, p: &Value) -> Result<Value> {
    const C: &str = "map.reverseGeocode";
    let list = p
        .get("places")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(C, "provider `given` needs `places: [{id, sublocation, city, state, country}]`"))?;
    let mut ops = Vec::new();
    for e in list {
        let Some(id) = e.get("id").and_then(Value::as_u64).map(PhotoId) else { continue };
        let Some(ph) = s.catalog.photo(id) else { continue };
        let t = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").chars().take(256).collect::<String>();
        let mut meta = ph.meta.clone();
        meta.location = t("sublocation");
        meta.city = t("city");
        meta.state = t("state");
        meta.country = t("country");
        if meta != ph.meta {
            ops.push(Op::SetMeta { id, meta: Box::new(meta) });
        }
    }
    let n = ops.len();
    if n > 0 {
        s.commit("Reverse Geocode", Op::Batch { ops })?;
    }
    Ok(json!({"changed": n}))
}

pub fn specs() -> Vec<CommandSpec> {
    let mut v = vec![
        cmd!(query "map.pins", "Map Pins", [], None, "{zoom?: 0–20 (3), cellPx?: cluster cell in screen px (48), filter?: all|tagged|untagged|location:<name>} → {tagged, untagged, clusters: [{lat, lon, count, ids}], fit: {lat, lon, zoom}} — the photos in view (current source and filter) as map pins, grid-clustered for the zoom", always, pins),
        cmd!(
            "map.geotag",
            "Geotag",
            [],
            None,
            "{ids?, lat, lon} | {ids?, clear: true} — set (or remove) the GPS position of the photos; one undo step",
            has_selection,
            geotag
        ),
        cmd!(query "map.locations", "Saved Locations", [], None, "{} → {locations: [{name, lat, lon, radius (m), private, photos}]}", always, |s, _| Ok(locations_json(s))),
        cmd!(
            "map.saveLocation",
            "Save Location",
            [],
            None,
            "{name, lat, lon, radius?: metres (100), private?: bool — photos inside a private location are exported without GPS or location fields, rename?: old name} — create or change a saved location (undoable)",
            always,
            save_location
        ),
        cmd!("map.deleteLocation", "Delete Location", [], None, "{name}", always, delete_location),
        cmd!(query "map.track", "Track Info", [], None, "{path: .gpx|.kml|.geojson file | text} → {name, points, segments, timed, start, end, fit} — read a track log (Auto-Tag: photo.autoTagTracklog)", always, track_info),
        cmd!(query "map.trackOffset", "Track Time Offset", [], None, "{path | gpx, id?: photo (active), lat?, lon?: where it was taken (else its GPS)} → {offset: \"+HH:MM\", minutes, distance} — the camera clock's UTC offset from one photo at a known point of the track; pass it as photo.autoTagTracklog's offset", always, track_offset),
    ];
    #[cfg(not(target_arch = "wasm32"))]
    v.extend([
        cmd!(query "map.geocodeStatus", "Reverse Geocoding Status", [], None, "{dataset?: folder} → {offline: {downloaded, bytes, path, attribution}, online: {endpoint, attribution, rateLimit}}", always, geocoding::status),
        cmd!(query "map.geonamesDownload", "Download Place Names", [], None, "{consent: true — contacts download.geonames.org, dataset?: folder} → {places, path, attribution} — the offline GeoNames cities list (CC-BY 4.0) for reverse geocoding", always, geocoding::download),
        cmd!("map.reverseGeocode", "Reverse Geocode", [], None, "{ids?, provider?: offline (default; needs map.geonamesDownload) | online (Nominatim-compatible, needs consent: true) | given (with places: [{id, sublocation, city, state, country}]), endpoint?, overwrite?: bool (also photos that have location fields), dryRun?} → {looked, found, photos: [{id, place: {sublocation, city, state, country, iso}}], attribution} — fills Sublocation/City/State/Country from GPS; one undo step", has_selection, geocoding::reverse),
    ]);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    fn session_with(n: usize) -> (Session, Vec<PhotoId>) {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        assert!(ids.len() >= n);
        (s, ids)
    }

    #[test]
    fn geotag_pins_locations_and_undo() {
        let (mut s, ids) = session_with(4);
        let r = s.execute("map.geotag", &json!({"ids": [ids[0].0, ids[1].0], "lat": 48.8566, "lon": 2.3522})).unwrap();
        assert_eq!(r["changed"], 2);
        s.execute("map.geotag", &json!({"ids": [ids[2].0], "lat": 51.5, "lon": -0.12})).unwrap();
        let pins = s.execute("map.pins", &json!({"zoom": 4})).unwrap();
        assert_eq!(pins["tagged"], 3);
        assert_eq!(pins["clusters"].as_array().unwrap().len(), 2);
        let near = s.execute("map.pins", &json!({"zoom": 4, "filter": "untagged"})).unwrap();
        assert_eq!((near["tagged"].as_u64(), near["untagged"].as_u64()), (Some(0), Some(ids.len() as u64 - 3)));

        s.execute("map.saveLocation", &json!({"name": "Paris", "lat": 48.8566, "lon": 2.3522, "radius": 2000, "private": true})).unwrap();
        let l = s.execute("map.locations", &json!({})).unwrap();
        assert_eq!(l["locations"][0]["photos"], 2);
        let inside = s.execute("map.pins", &json!({"filter": "location:paris"})).unwrap();
        assert_eq!(inside["tagged"], 2);
        assert!(s.catalog.is_private_location((48.8566, 2.3522)));
        // rename keeps it one location
        s.execute("map.saveLocation", &json!({"name": "Home", "rename": "Paris"})).unwrap();
        let l = s.execute("map.locations", &json!({})).unwrap();
        assert_eq!((l["locations"].as_array().unwrap().len(), l["locations"][0]["name"].as_str()), (1, Some("Home")));
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.catalog.saved_location("Paris").is_some());
        s.execute("map.deleteLocation", &json!({"name": "paris"})).unwrap();
        assert!(s.execute("map.deleteLocation", &json!({"name": "paris"})).is_err());

        // bad input is an error, never a panic
        assert!(s.execute("map.geotag", &json!({"ids": [ids[0].0], "lat": 91, "lon": 0})).is_err());
        assert!(s.execute("map.geotag", &json!({"ids": [ids[0].0]})).is_err());
        assert!(s.execute("map.saveLocation", &json!({"name": "x", "lat": 0, "lon": 0, "radius": -1})).is_err());
        assert!(s.execute("map.pins", &json!({"filter": "bogus"})).is_err());
        s.execute("map.geotag", &json!({"ids": [ids[0].0], "clear": true})).unwrap();
        assert!(s.catalog.photo(ids[0]).unwrap().meta.gps.is_none());
    }

    #[test]
    fn track_offset_and_info() {
        let (mut s, ids) = session_with(1);
        let gpx = r#"<gpx><trk><trkseg><trkpt lat="48.0" lon="11.0"><time>2024-05-01T10:00:00Z</time></trkpt>
            <trkpt lat="48.1" lon="11.1"><time>2024-05-01T11:00:00Z</time></trkpt></trkseg></trk></gpx>"#;
        s.commit("t", Op::SetCaptured { id: ids[0], captured: Some("2024-05-01T13:00:00".into()) }).unwrap();
        let r = s.execute("map.trackOffset", &json!({"gpx": gpx, "id": ids[0].0, "lat": 48.1, "lon": 11.1})).unwrap();
        assert_eq!(r["offset"], "+02:00");
        let t = s.execute("map.track", &json!({"text": gpx})).unwrap();
        assert_eq!((t["points"].as_u64(), t["timed"].as_bool()), (Some(2), Some(true)));
        assert!(s.execute("map.track", &json!({"text": "nope"})).is_err());
    }

    #[test]
    fn reverse_geocode_offline_and_consent() {
        let (mut s, ids) = session_with(2);
        s.execute("map.geotag", &json!({"ids": [ids[0].0], "lat": 48.86, "lon": 2.34})).unwrap();
        let dir = std::env::temp_dir().join(format!("dac-engine-geonames-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cities15000.txt"), "1\tParis\tParis\t\t48.85341\t2.3488\tP\tPPLC\tFR\t\t11\n").unwrap();
        std::fs::write(dir.join("countryInfo.txt"), "FR\tFRA\t250\tFR\tFrance\n").unwrap();
        let d = dir.to_string_lossy().to_string();
        s.execute("map.geotag", &json!({"ids": [ids[1].0], "clear": true})).unwrap();
        let r = s.execute("map.reverseGeocode", &json!({"ids": [ids[0].0, ids[1].0], "dataset": d, "overwrite": true})).unwrap();
        assert_eq!(r["found"], 1);
        let m = &s.catalog.photo(ids[0]).unwrap().meta;
        assert_eq!((m.city.as_str(), m.country.as_str()), ("Paris", "France"));
        // online without consent: refused before anything is sent
        let e = s.execute("map.reverseGeocode", &json!({"ids": [ids[0].0], "provider": "online", "overwrite": true})).unwrap_err();
        assert!(e.to_string().contains("consent"), "{e}");
        assert!(s.execute("map.geonamesDownload", &json!({"dataset": d})).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A photo inside a private saved location is exported without GPS or location fields; one
    /// outside keeps them.
    #[test]
    fn private_locations_strip_gps_on_export() {
        use crate::export::{ExportOptions, export_metadata};
        use dac_catalog::{Catalog, Photo, Source};
        let mut c = Catalog::new();
        c.apply(Op::SetSavedLocation { name: "Home".into(), location: Some(SavedLocation::new("Home", 48.85, 2.35, 500.0, true).unwrap()) }).unwrap();
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-09-30T00:00:00");
        p.meta.gps = Some((48.851, 2.351));
        p.meta.city = "Paris".into();
        let m = export_metadata(&p, &c, &ExportOptions::default()).unwrap();
        assert!(m.gps.is_none() && m.city.is_none());
        p.meta.gps = Some((45.0, 2.35));
        let m = export_metadata(&p, &c, &ExportOptions::default()).unwrap();
        assert!(m.gps.is_some() && m.city.as_deref() == Some("Paris"));
    }
}
