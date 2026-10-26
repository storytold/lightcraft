//! Map commands: the tile source (where map tiles come from, saved with the library preferences),
//! the tile cache, and geotagging (`photo.setGps`, `photo.removeGps`: what dropping photos on the
//! Map view, moving a pin and Photo ▸ Remove GPS Coordinates run). See docs/map.md.

use lightcraft_catalog::{Op, PhotoId};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_selection, names_photos};
use crate::locations::{LocationChange, PositionIndex, SavedLocation};
use crate::{EngineError, Result, Session};
use lightcraft_map::source::{OFFLINE_ID, OSM_ID, OSM_TILE_URL};
use lightcraft_map::{TileSource, cache};

const ID: &str = "map.tileSource";

fn source_json(s: &TileSource) -> Value {
    json!({
        "id": s.id,
        "urlTemplate": s.url_template,
        "attribution": s.attribution,
        "maxZoom": s.max_zoom,
        "userAgent": s.user_agent,
        "offline": s.is_offline(),
    })
}

fn opt_str<'a>(p: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.as_str())),
        Some(other) => Err(bad(ID, format!("`{key}` must be text, not {other}"))),
    }
}

/// The source `{id?, urlTemplate?, attribution?, maxZoom?, offline?}` asks for, from `cur`.
fn requested(cur: &TileSource, p: &Value) -> Result<Option<TileSource>> {
    let id = opt_str(p, "id")?.map(str::trim).filter(|v| !v.is_empty());
    let template = opt_str(p, "urlTemplate")?.map(str::trim).filter(|v| !v.is_empty());
    let attribution = opt_str(p, "attribution")?;
    let max_zoom = match p.get("maxZoom") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().and_then(|z| u8::try_from(z).ok()).ok_or_else(|| bad(ID, "maxZoom must be a whole number 0–24"))?),
    };
    let offline = match p.get("offline") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool().ok_or_else(|| bad(ID, "offline must be true or false"))?),
    };
    if id.is_none() && template.is_none() && attribution.is_none() && max_zoom.is_none() && offline.is_none() {
        return Ok(None);
    }
    let check = |r: std::result::Result<TileSource, lightcraft_map::MapError>| r.map_err(|e| bad(ID, e.to_string()));
    if offline == Some(true) || (id == Some(OFFLINE_ID) && template.is_none()) {
        return Ok(Some(TileSource::offline()));
    }
    // OpenStreetMap by name (optionally with a lower max zoom)
    if matches!(id, Some(OSM_ID) | Some("openstreetmap")) && template.is_none() && attribution.is_none() {
        return check(TileSource::custom(None, OSM_TILE_URL, None, max_zoom)).map(Some);
    }
    if offline == Some(false) && id.is_none() && template.is_none() && cur.is_offline() {
        return Ok(Some(TileSource::openstreetmap()));
    }
    // a change to the current source, or a new one
    let changed_template = template.is_some_and(|t| t != cur.url_template);
    let base = if changed_template { None } else { Some(cur) };
    let Some(template) = template.or(base.map(|b| b.url_template.as_str())).filter(|t| !t.is_empty()) else {
        return Err(bad(ID, "give urlTemplate (https://…/{z}/{x}/{y}.png) for a tile server, or {offline: true}"));
    };
    let attribution = attribution.or(base.map(|b| b.attribution.as_str()));
    let max_zoom = max_zoom.or(base.map(|b| b.max_zoom));
    let id = id.or(base.map(|b| b.id.as_str()));
    check(TileSource::custom(id, template, attribution, max_zoom)).map(Some)
}

fn tile_source(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(next) = requested(s.map.source(), p)? {
        let old = s.map.source().clone();
        s.map.set_source(next);
        // a source that can't be saved isn't kept: an error means nothing changed
        if let Err(e) = s.save_prefs() {
            s.map.set_source(old);
            return Err(e);
        }
    }
    Ok(source_json(s.map.source()))
}

fn cache_info(s: &mut Session, _: &Value) -> Result<Value> {
    let root = s.map.cache_root.clone();
    let info = root.as_deref().map(cache::info).unwrap_or_default();
    Ok(json!({
        "bytes": info.bytes,
        "tiles": info.tiles,
        "capBytes": s.map.cache_bytes,
        "dir": root.map(|d| d.to_string_lossy().to_string()),
    }))
}

fn clear_cache(s: &mut Session, _: &Value) -> Result<Value> {
    let Some(root) = s.map.cache_root.clone() else {
        return Ok(json!({"bytes": 0, "tiles": 0, "dir": Value::Null}));
    };
    // the service's counters and in-memory tiles go too
    s.map.reset_service();
    let gone = cache::clear(&root).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(json!({"bytes": gone.bytes, "tiles": gone.tiles, "dir": root.to_string_lossy()}))
}

/// A coordinate parameter: a finite number (JSON has no NaN, but a huge exponent must not pass).
fn coord(id: &str, p: &Value, key: &str) -> Result<f64> {
    match p.get(key) {
        Some(v) => v.as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad(id, format!("`{key}` must be a number (degrees)"))),
        None => Err(bad(id, format!("missing `{key}` (degrees)"))),
    }
}

/// `lon` folded into −180…180 (190 is −170: the same meridian), `None` when not finite.
pub(crate) fn wrap_lon(lon: f64) -> Option<f64> {
    if !lon.is_finite() {
        return None;
    }
    if (-180.0..=180.0).contains(&lon) {
        return Some(lon);
    }
    Some((lon + 180.0).rem_euclid(360.0) - 180.0)
}

/// The photos a geotag command acts on: those named (`ids` / `id`) that exist, else the
/// selection. Naming only photos that don't exist is an error (nothing to do, and the caller
/// should know).
fn gps_targets(s: &Session, p: &Value, id: &str) -> Result<Vec<PhotoId>> {
    let named = names_photos(p);
    if named && !p.get("ids").is_none_or(|v| v.is_null() || v.is_array()) {
        return Err(bad(id, "`ids` is a list of photo ids"));
    }
    let mut seen = std::collections::HashSet::new();
    let t: Vec<PhotoId> = s.targets(p)?.into_iter().filter(|i| s.catalog.photo(*i).is_some() && seen.insert(*i)).collect();
    if t.is_empty() {
        return Err(bad(id, if named { "no such photo" } else { "no photos selected" }));
    }
    Ok(t)
}

/// Set (`Some`) or clear (`None`) the GPS position of `targets`, one undo step `label`.
fn commit_gps(s: &mut Session, targets: &[PhotoId], gps: Option<(f64, f64)>, label: &str) -> Result<usize> {
    let ops: Vec<Op> = targets
        .iter()
        .filter_map(|id| s.catalog.photo(*id))
        .filter(|ph| ph.meta.gps != gps)
        .map(|ph| {
            let mut m = ph.meta.clone();
            m.gps = gps;
            Op::SetMeta { id: ph.id, meta: Box::new(m) }
        })
        .collect();
    let n = ops.len();
    if n > 0 {
        s.commit(label, Op::Batch { ops })?;
        s.gps_edited.extend(targets.iter().copied());
    }
    Ok(n)
}

fn set_gps(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.setGps";
    let lat = coord(C, p, "lat")?;
    let lon = coord(C, p, "lon")?;
    if !(-90.0..=90.0).contains(&lat) {
        return Err(bad(C, format!("lat {lat} is outside −90…90")));
    }
    let lon = wrap_lon(lon).ok_or_else(|| bad(C, "`lon` must be a number (degrees)"))?;
    let targets = gps_targets(s, p, C)?;
    let changed = commit_gps(s, &targets, Some((lat, lon)), "Set GPS Coordinates")?;
    Ok(json!({"changed": changed, "lat": lat, "lon": lon}))
}

fn remove_gps(s: &mut Session, p: &Value) -> Result<Value> {
    let targets = gps_targets(s, p, "photo.removeGps")?;
    let changed = commit_gps(s, &targets, None, "Remove GPS Coordinates")?;
    Ok(json!({"changed": changed}))
}

// ---------------------------------------------------------------- saved locations

/// An optional number parameter (finite), `None` when absent or null.
fn opt_num(id: &str, p: &Value, key: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|v| v.is_finite()).map(Some).ok_or_else(|| bad(id, format!("`{key}` must be a number"))),
    }
}

fn opt_bool(id: &str, p: &Value, key: &str) -> Result<Option<bool>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| bad(id, format!("`{key}` is true or false"))),
    }
}

/// The saved location named by `id`.
fn location_id(cmd: &str, s: &Session, p: &Value) -> Result<u64> {
    let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| bad(cmd, "`id` is a saved location's id (map.locations)"))?;
    if s.map.locations.get(id).is_none() {
        return Err(bad(cmd, format!("no saved location {id}")));
    }
    Ok(id)
}

/// Every library photo's position (not the deleted ones), for counting.
fn library_index(s: &Session) -> PositionIndex {
    PositionIndex::new(s.catalog.photos().filter(|p| !p.deleted).map(|p| (p.id.0, p.meta.gps)))
}

/// The positions of the photos in view (source, filter and sort: the grid's; before the Map
/// view's location filter).
fn view_index(s: &mut Session) -> PositionIndex {
    let (_, ids) = s.visible_base_shared();
    PositionIndex::new(ids.iter().map(|id| (id.0, s.catalog.photo(*id).and_then(|p| p.meta.gps))))
}

fn with_counts(l: &SavedLocation, library: &PositionIndex, view: &PositionIndex) -> Value {
    let mut v = l.to_json();
    v["photos"] = json!(library.count(&l.circle()));
    v["inView"] = json!(view.count(&l.circle()));
    v
}

fn locations(s: &mut Session, _: &Value) -> Result<Value> {
    let t0 = web_time::Instant::now();
    let (library, view) = (library_index(s), view_index(s));
    let list: Vec<Value> = s.map.locations.list().iter().map(|l| with_counts(l, &library, &view)).collect();
    Ok(json!({"locations": list, "count": list.len(), "countMs": t0.elapsed().as_secs_f64() * 1000.0}))
}

/// Where a new location goes when made from photos: the middle of their positions, and a radius
/// that holds them all (10 % to spare, at least 100 m, at most 1000 km).
fn around_photos(s: &Session, ids: &[PhotoId]) -> Option<(f64, f64, f64)> {
    let pos: Vec<(f64, f64)> =
        ids.iter().filter_map(|id| s.catalog.photo(*id)).filter_map(|p| lightcraft_catalog::rules::valid_gps(p.meta.gps)).collect();
    let b = lightcraft_map::Bounds::of(pos.iter().copied())?;
    let c = b.center();
    let lon = wrap_lon(c.lon)?;
    let far = pos.iter().map(|(la, lo)| lightcraft_catalog::rules::distance_m(c.lat, lon, *la, *lo)).filter(|d| d.is_finite()).fold(0.0, f64::max);
    let radius = (far * 1.1).clamp(100.0, lightcraft_catalog::rules::MAX_RADIUS_M);
    Some((c.lat, lon, radius))
}

fn location_add(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.location.add";
    let name = match p.get("name") {
        Some(Value::String(n)) => n.clone(),
        _ => return Err(bad(C, "`name` is the location's name (text)")),
    };
    let private = opt_bool(C, p, "private")?.unwrap_or(false);
    let (lat, lon, radius) = (opt_num(C, p, "lat")?, opt_num(C, p, "lon")?, opt_num(C, p, "radius")?);
    let from = opt_bool(C, p, "fromSelection")?.unwrap_or(false);
    let (lat, lon, radius) = match (lat, lon) {
        (Some(lat), Some(lon)) => (lat, lon, radius.unwrap_or(1000.0)),
        (None, None) if from => {
            let ids = s.targets(p)?;
            let (la, lo, r) = around_photos(s, &ids).ok_or_else(|| bad(C, "none of the selected photos has a GPS position"))?;
            (la, lo, radius.unwrap_or(r))
        }
        _ => return Err(bad(C, "give `lat` and `lon` (degrees), or `fromSelection: true`")),
    };
    let l = s.map.locations.add(&name, lat, lon, radius, private).map_err(|e| bad(C, e))?;
    if let Err(e) = s.save_locations() {
        // it stays for this session; say why it wasn't saved
        log::error!("{C}: {e}");
        let mut v = l.to_json();
        v["notSaved"] = json!(e.to_string());
        return Ok(v);
    }
    let (library, view) = (library_index(s), view_index(s));
    Ok(with_counts(&l, &library, &view))
}

fn location_update(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.location.update";
    let id = location_id(C, s, p)?;
    let name = match p.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(n)) => Some(n.clone()),
        Some(_) => return Err(bad(C, "`name` is text")),
    };
    let change = LocationChange {
        name,
        lat: opt_num(C, p, "lat")?,
        lon: opt_num(C, p, "lon")?,
        radius: opt_num(C, p, "radius")?,
        private: opt_bool(C, p, "private")?,
    };
    let l = s.map.locations.update(id, &change).map_err(|e| bad(C, e))?;
    let saved = s.save_locations();
    let (library, view) = (library_index(s), view_index(s));
    let mut v = with_counts(&l, &library, &view);
    if let Err(e) = saved {
        v["notSaved"] = json!(e.to_string());
    }
    Ok(v)
}

fn location_remove(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.location.remove";
    let id = location_id(C, s, p)?;
    let gone = s.map.locations.remove(id).ok_or_else(|| bad(C, format!("no saved location {id}")))?;
    let mut v = json!({"removed": gone.id, "name": gone.name});
    if let Err(e) = s.save_locations() {
        v["notSaved"] = json!(e.to_string());
    }
    Ok(v)
}

fn location_photos(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.location.photos";
    let id = location_id(C, s, p)?;
    let in_view = opt_bool(C, p, "inView")?.unwrap_or(false);
    let circle = s.map.locations.get(id).map(SavedLocation::circle).ok_or_else(|| bad(C, format!("no saved location {id}")))?;
    let ids = if in_view {
        // in the order of the view
        let (_, view) = s.visible_base_shared();
        view.iter()
            .filter(|i| {
                s.catalog.photo(**i).is_some_and(|ph| lightcraft_catalog::rules::valid_gps(ph.meta.gps).is_some_and(|(a, b)| circle.contains(a, b)))
            })
            .map(|i| i.0)
            .collect()
    } else {
        let mut v = library_index(s).inside(&circle);
        v.sort_unstable();
        v
    };
    Ok(json!({"id": id, "ids": ids, "count": ids.len()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "photo.setGps",
                "Set GPS Coordinates",
                [],
                None,
                "{ids?, lat, lon} — set the GPS position of the photos (default: the selection) to lat (−90…90) / lon (degrees; wrapped into −180…180), one undo step; what dropping photos on the Map view or moving a pin does → {changed, lat, lon}",
                has_selection,
                set_gps
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "photo.removeGps",
                "Remove GPS Coordinates",
                ["Photo"],
                None,
                "{ids?} — clear the GPS position of the photos (default: the selection), one undo step → {changed}",
                has_selection,
                remove_gps
            )
        },
        cmd!(
            "map.tileSource",
            "Map Tile Source",
            [],
            None,
            "{} → the map's tile source; set with {offline: true} (no network, no tiles), {id: \"osm\"} (OpenStreetMap, the default), or {urlTemplate: \"https://…/{z}/{x}/{y}.png\", attribution: text (required except for OpenStreetMap's template), maxZoom?: 0–24, id?: cache-folder name [a-z0-9._-]}; fields alone change the current source. Saved with the library preferences → {id, urlTemplate, attribution, maxZoom, userAgent, offline}",
            always,
            tile_source
        ),
        cmd!(
            "map.trackLog",
            "Map Tracklog",
            [],
            None,
            "{path?: GPX file (at most 50 MB, 2 million timed points) | gpx?: GPX text, active?: track index | \"all\", offset?: camera clock's UTC offset (`+02:00`, hours, or null / \"\" for UTC), clear?: true (alone)} — the track log shown on the Map view (session only, never saved): load it (a bad file changes nothing), pick the track tagging uses, set the time zone offset, or clear it; {} reports → {loaded, path, name, points, untimed, tracks: [{index, name, points, segments, start, end, duration}], active (null: all), activePoints, start, end, duration (UTC, of the active track), offset, startLocal, endLocal (camera time), loadMs, generation}. photo.autoTagTracklog {loaded: true} tags from it",
            always,
            crate::tracklog::track_log
        ),
        cmd!(query "map.locations", "Saved Locations", [], None, "{} → the library's saved locations: {locations: [{id, name, lat, lon, radius (metres), private, photos (in the library inside it), inView (of the photos in view)}], count, countMs}", always, locations),
        cmd!(
            "map.location.add",
            "New Saved Location",
            [],
            None,
            "{name, lat, lon, radius?: metres 1–1000000 (default 1000), private?: bool} or {name, fromSelection: true, ids?, radius?, private?} (centred on the selected (or named) photos' positions, a radius that holds them) — save a location (saved at once in locations.json; not an undo step) → the location with its counts",
            always,
            location_add
        ),
        cmd!(
            "map.location.update",
            "Edit Saved Location",
            [],
            None,
            "{id, name?, lat?, lon?, radius?, private?} — change a saved location (checked as a whole: nothing changes when any part is refused; not an undo step) → the location with its counts",
            always,
            location_update
        ),
        cmd!(
            "map.location.remove",
            "Delete Saved Location",
            [],
            None,
            "{id} — delete a saved location (not an undo step; the photos keep their positions) → {removed, name}",
            always,
            location_remove
        ),
        cmd!(query "map.location.photos", "Photos in Saved Location", [], None, "{id, inView?: bool} → {id, ids, count}: the library's photos inside the location (inView: only the photos in view, in their order)", always, location_photos),
        cmd!(query "map.cacheInfo", "Map Cache Info", [], None, "{} → the map tile cache: {bytes, tiles, capBytes, dir (null = no disk cache)}", always, cache_info),
        cmd!(
            "map.clearCache",
            "Clear Map Cache",
            [],
            None,
            "{} — delete every cached map tile (all tile sources); tiles in view are fetched again → {bytes, tiles (deleted), dir}",
            always,
            clear_cache
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lightcraft-engine-map-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn tile_source_get_and_set() {
        let mut s = Session::new();
        let r = s.execute("map.tileSource", &json!({})).unwrap();
        assert_eq!(r["id"], "osm");
        assert_eq!(r["urlTemplate"], "https://tile.openstreetmap.org/{z}/{x}/{y}.png");
        assert_eq!(r["attribution"], "© OpenStreetMap contributors");
        assert_eq!(r["maxZoom"], 19);
        assert!(r["userAgent"].as_str().unwrap().starts_with("LightCraft/"));
        assert_eq!(r["offline"], false);

        let r = s.execute("map.tileSource", &json!({"offline": true})).unwrap();
        assert_eq!((r["id"].as_str(), r["offline"].as_bool()), (Some("offline"), Some(true)));
        assert!(s.map.source().is_offline());
        // a field alone needs a server when offline
        assert!(s.execute("map.tileSource", &json!({"maxZoom": 10})).is_err());
        let r = s.execute("map.tileSource", &json!({"offline": false})).unwrap();
        assert_eq!(r["id"], "osm");

        let r = s
            .execute(
                "map.tileSource",
                &json!({"urlTemplate": "https://tiles.example.org/{z}/{x}/{y}.png", "attribution": "© Example", "maxZoom": 17}),
            )
            .unwrap();
        assert!(r["id"].as_str().unwrap().starts_with("custom-"));
        assert_eq!(r["maxZoom"], 17);
        // change one field of the current source
        let r = s.execute("map.tileSource", &json!({"maxZoom": 15})).unwrap();
        assert_eq!((r["maxZoom"].as_u64(), r["attribution"].as_str()), (Some(15), Some("© Example")));
        // bad settings are refused and change nothing
        for bad in [
            json!({"urlTemplate": "http://tiles.example.org/{z}/{x}/{y}.png", "attribution": "x"}),
            json!({"urlTemplate": "https://tiles.example.org/{z}/{x}.png", "attribution": "x"}),
            json!({"urlTemplate": "https://other.example.org/{z}/{x}/{y}.png"}),
            json!({"maxZoom": 99}),
            json!({"maxZoom": -1}),
            json!({"urlTemplate": 5}),
            json!({"offline": "yes"}),
            json!({"id": "../../etc"}),
        ] {
            assert!(matches!(s.execute("map.tileSource", &bad), Err(EngineError::BadParams { .. })), "{bad}");
        }
        assert_eq!(s.map.source().max_zoom, 15);
        let r = s.execute("map.tileSource", &json!({"id": "osm"})).unwrap();
        assert_eq!(r["urlTemplate"], OSM_TILE_URL);
    }

    #[test]
    fn changing_the_source_restarts_the_service() {
        let mut s = Session::new();
        assert!(s.map.service().is_none(), "nothing starts until a map asks");
        assert_eq!(s.map_tiles().source().id, "osm");
        s.execute("map.tileSource", &json!({"offline": true})).unwrap();
        assert!(s.map.service().is_none());
        assert!(s.map_tiles().source().is_offline());
    }

    #[test]
    fn cache_info_and_clear() {
        let mut s = Session::new();
        let r = s.execute("map.cacheInfo", &json!({})).unwrap();
        assert_eq!((r["bytes"].as_u64(), r["tiles"].as_u64(), r["dir"].is_null()), (Some(0), Some(0), true));
        assert_eq!(s.execute("map.clearCache", &json!({})).unwrap()["tiles"], 0);
        let root = tmp("cache");
        s.map.cache_root = Some(root.clone());
        let c = lightcraft_map::TileCache::new(&root, "osm", 1 << 30).unwrap();
        for x in 0..3 {
            c.write(lightcraft_map::TileKey::new(4, x, 1).unwrap(), &Default::default(), b"tile bytes").unwrap();
        }
        let r = s.execute("map.cacheInfo", &json!({})).unwrap();
        assert_eq!(r["tiles"], 3);
        assert!(r["bytes"].as_u64().unwrap() > 30);
        assert_eq!(r["dir"], root.to_string_lossy().as_ref());
        let r = s.execute("map.clearCache", &json!({})).unwrap();
        assert_eq!(r["tiles"], 3);
        assert_eq!(s.execute("map.cacheInfo", &json!({})).unwrap()["tiles"], 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn gps(s: &Session, id: PhotoId) -> Option<(f64, f64)> {
        s.catalog.photo(id).and_then(|p| p.meta.gps)
    }

    #[test]
    fn set_gps_sets_every_photo_in_one_undo_step() {
        let mut s = Session::with_demo();
        let ids: Vec<PhotoId> = s.catalog.photos().take(3).map(|p| p.id).collect();
        s.execute("photo.setMeta", &json!({"ids": [ids[0].0], "gps": [10.0, 20.0]})).unwrap();
        let r = s.execute("photo.setGps", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>(), "lat": 48.85, "lon": 2.29})).unwrap();
        assert_eq!(r["changed"], 3, "{r}");
        for id in &ids {
            assert_eq!(gps(&s, *id), Some((48.85, 2.29)));
        }
        assert_eq!(s.undo.last().map(|u| u.label.as_str()), Some("Set GPS Coordinates"));
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(gps(&s, ids[0]), Some((10.0, 20.0)), "the old position comes back");
        assert_eq!((gps(&s, ids[1]), gps(&s, ids[2])), (None, None), "one step undoes them all");
        // the same position again changes nothing (no empty undo step)
        s.execute("edit.redo", &json!({})).unwrap();
        let steps = s.undo.len();
        assert_eq!(s.execute("photo.setGps", &json!({"ids": [ids[1].0], "lat": 48.85, "lon": 2.29})).unwrap()["changed"], 0);
        assert_eq!(s.undo.len(), steps);
        // longitude wraps around the world
        let r = s.execute("photo.setGps", &json!({"ids": [ids[1].0], "lat": -33.9, "lon": 190.0})).unwrap();
        assert_eq!(r["lon"].as_f64(), Some(-170.0));
        assert_eq!(gps(&s, ids[1]), Some((-33.9, -170.0)));
    }

    #[test]
    fn set_gps_refuses_hostile_values_and_changes_nothing() {
        let mut s = Session::with_demo();
        let id = s.catalog.photos().next().unwrap().id;
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        let steps = s.undo.len();
        for bad in [
            json!({"lat": 91.0, "lon": 0.0}),
            json!({"lat": -90.5, "lon": 0.0}),
            json!({"lat": "north", "lon": 0.0}),
            json!({"lat": null, "lon": 0.0}),
            json!({"lon": 0.0}),
            json!({"lat": 0.0}),
            json!({"lat": 0.0, "lon": [1]}),
            json!({"lat": 0.0, "lon": 0.0, "ids": [999_999_999]}),
            json!({"lat": 0.0, "lon": 0.0, "ids": "1"}),
            json!({"lat": 0.0, "lon": 0.0, "ids": [-1, "x"]}),
        ] {
            assert!(matches!(s.execute("photo.setGps", &bad), Err(EngineError::BadParams { .. })), "{bad}");
        }
        // a number too large for f64 isn't a coordinate either
        let huge: Value = serde_json::from_str(r#"{"lat": 0, "lon": 1e400}"#).unwrap_or(json!({"lat": 0, "lon": f64::MAX}));
        let _ = s.execute("photo.setGps", &huge);
        assert!(s.undo.len() <= steps + 1);
        assert!(gps(&s, id).is_none_or(|(la, lo)| la.is_finite() && lo.is_finite() && lo.abs() <= 180.0));
        assert_eq!(wrap_lon(f64::NAN), None);
        assert_eq!(wrap_lon(f64::INFINITY), None);
        assert_eq!(wrap_lon(540.0), Some(-180.0));
        assert_eq!(wrap_lon(-190.0), Some(170.0));
        assert_eq!(wrap_lon(180.0), Some(180.0));
    }

    #[test]
    fn remove_gps_clears_the_selection_in_one_undo_step() {
        let mut s = Session::with_demo();
        let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
        s.execute("photo.setGps", &json!({"ids": ids, "lat": 51.5, "lon": -0.12})).unwrap();
        s.execute("library.select", &json!({"ids": ids})).unwrap();
        let r = s.execute("photo.removeGps", &json!({})).unwrap();
        assert_eq!(r["changed"], 2);
        assert!(ids.iter().all(|i| gps(&s, PhotoId(*i)).is_none()));
        assert_eq!(s.undo.last().map(|u| u.label.as_str()), Some("Remove GPS Coordinates"));
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(ids.iter().all(|i| gps(&s, PhotoId(*i)) == Some((51.5, -0.12))));
        assert!(s.execute("photo.removeGps", &json!({"ids": [123_456_789]})).is_err());
        // it is in the Photo menu
        let spec = crate::cmd::find_command("photo.removeGps").unwrap();
        assert_eq!(spec.menu, &["Photo"]);
    }

    /// A library whose settings can't be written (a full disk, a read-only folder).
    #[derive(Default)]
    struct Unwritable(lightcraft_catalog::MemStore);

    impl lightcraft_catalog::Store for Unwritable {
        fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
            self.0.read(name)
        }
        fn write_atomic(&mut self, _: &str, _: &[u8]) -> std::io::Result<()> {
            Err(std::io::Error::other("disk full"))
        }
        fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
            self.0.append(name, data)
        }
        fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
            self.0.truncate(name, len)
        }
        fn describe(&self) -> String {
            "unwritable".into()
        }
    }

    #[test]
    fn a_tile_source_that_cant_be_saved_is_not_kept() {
        let mut s = Session::new();
        let stores = crate::library::LibraryStores {
            dir: "unwritable".into(),
            catalog: Box::new(lightcraft_catalog::MemStore::new()),
            files: Box::new(Unwritable::default()),
            on_disk: false,
        };
        s.open_library_in(stores, true).unwrap();
        assert!(s.execute("map.tileSource", &json!({"offline": true})).is_err());
        assert_eq!(s.map.source().id, "osm", "an error means nothing changed");
    }

    #[test]
    fn a_damaged_saved_tile_source_keeps_the_other_preferences() {
        let lib = tmp("damaged-source");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("prefs.json"), br#"{"cacheMb": 777, "mapSource": {"urlTemplate": 5, "maxZoom": "high"}}"#).unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        assert_eq!(s.cache_mb, 777, "the rest of prefs.json is read");
        assert_eq!(s.map.source().id, "osm", "the damaged source falls back to OpenStreetMap");
        drop(s);
        let _ = std::fs::remove_dir_all(&lib);
    }
}
