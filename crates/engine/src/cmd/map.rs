//! Map module commands: tile sources and the tile cache, saved locations, location filtering, and
//! GPS tagging (`photo.setGps`, `photo.removeGps`: what dropping photos on the Map view, moving a
//! pin and Photo ▸ Remove GPS Coordinates run). The tile-source commands are ported from PR #797,
//! the geotag commands from PR #810 (Jason Baldwin).

use lightcraft_catalog::Op;
use lightcraft_geom::{GeoBounds, LatLon};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, f64_req, has_selection};
use crate::locations::{LocationChange, PositionIndex, SavedLocation};
use crate::map::{LocationFilter, TileSource, cache};
use crate::{EngineError, Result, Session};

const SOURCE: &str = "map.tileSource";

/// `photo.setGps`: every target photo gets the position (lat −90…90; the longitude wrapped into
/// −180…180, so a drop past the antimeridian lands on the same meridian), as one undo step.
/// Photos already there are left alone (no empty undo step). A named photo the library doesn't
/// have is an error and nothing changes: the caller (a drag of photos deleted meanwhile, an agent)
/// learns of it instead of a silent no-op.
fn set_gps(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "photo.setGps";
    let lat = f64_req(p, "lat", C)?;
    let lon = f64_req(p, "lon", C)?;
    let Some(at) = LatLon::wrapped(lat, lon) else {
        return Err(bad(C, format!("lat must be −90…90 and lon a finite number (got {lat}, {lon})")));
    };
    let pos = ((at.lat * 1e7).round() / 1e7, (at.lon * 1e7).round() / 1e7);
    let targets = gps_targets(s, p, C)?;
    let count = commit_gps(s, &targets, Some(pos), "Set GPS Coordinates")?;
    Ok(json!({ "updated": count, "lat": pos.0, "lon": pos.1 }))
}

/// The photos a geotag command acts on: those named (`ids` / `id`), else the selection, each once.
/// A named photo the library doesn't have is an error and nothing changes (a drag of photos deleted
/// meanwhile, an agent): the caller learns of it instead of a silent no-op.
fn gps_targets(s: &mut Session, p: &Value, c: &str) -> Result<Vec<lightcraft_catalog::PhotoId>> {
    let mut seen = std::collections::HashSet::new();
    let targets: Vec<_> = s
        .targets(p)
        .map_err(|e| match e {
            EngineError::BadParams { msg, .. } => bad(c, msg),
            other => other,
        })?
        .into_iter()
        .filter(|id| seen.insert(*id))
        .collect();
    if let Some(gone) = targets.iter().find(|id| s.catalog.photo(**id).is_none()) {
        return Err(bad(c, format!("no such photo {}", gone.0)));
    }
    if targets.is_empty() {
        return Err(bad(c, "no photos selected"));
    }
    Ok(targets)
}

/// Set (`Some`) or clear (`None`) the GPS position of `targets` as one undo step `label`; photos
/// already so are left alone (no empty undo step). The photos become LightCraft's for their
/// sidecar GPS ([`Session::gps_edited`]), so clearing (or undoing) removes it from the sidecar.
fn commit_gps(s: &mut Session, targets: &[lightcraft_catalog::PhotoId], gps: Option<(f64, f64)>, label: &str) -> Result<usize> {
    let ops: Vec<Op> = targets
        .iter()
        .filter_map(|id| s.catalog.photo(*id))
        .filter(|ph| ph.meta.gps != gps)
        .map(|ph| {
            let mut meta = ph.meta.clone();
            meta.gps = gps;
            Op::SetMeta { id: ph.id, meta: Box::new(meta) }
        })
        .collect();
    let count = ops.len();
    if count > 0 {
        // before the commit: the sidecar written when it lands already follows LightCraft's position
        let before = s.gps_edited.len();
        s.gps_edited.extend(targets.iter().copied());
        s.gps_owned_grew(before);
        s.commit(label, Op::Batch { ops })?;
    }
    Ok(count)
}

/// `photo.removeGps` (and its older name `photo.clearGps`): clear the photos' position, one undo
/// step "Remove GPS Coordinates".
fn remove_gps(s: &mut Session, p: &Value) -> Result<Value> {
    let targets = gps_targets(s, p, "photo.removeGps")?;
    let count = commit_gps(s, &targets, None, "Remove GPS Coordinates")?;
    Ok(json!({ "changed": count, "cleared": count }))
}

fn source_json(s: &Session) -> Value {
    let chosen = s.map.source();
    let used = s.map.effective_source();
    json!({
        "id": used.id(),
        "label": used.label(),
        "urlTemplate": used.url_template(),
        "attribution": s.map.attribution(),
        "maxZoom": used.max_zoom(),
        "userAgent": used.user_agent(),
        "offline": used.is_offline(),
        "custom": matches!(used, TileSource::Custom { .. }),
        // what prefs.json keeps, when the map can't use it (an Esri layer without a key)
        "saved": (*chosen != used).then(|| chosen.id().to_string()),
        "notice": s.map.fallback_notice(),
        // whether an ArcGIS API key is set; the key itself is never reported
        "esriKeySet": s.map.esri_key_set(),
    })
}

/// `map.setEsriKey {key}`: set the user's ArcGIS API key (`""` or null clears it). Checked for
/// shape only, no network call. Not journaled (a query spec), so the key never lands in the
/// journal; the reply and every error only say whether a key is set.
fn set_esri_key(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.setEsriKey";
    let key = match p.get("key") {
        None => return Err(bad(C, "give {key: \"<your ArcGIS API key>\"} (\"\" clears it)")),
        Some(Value::Null) => None,
        Some(Value::String(k)) if k.trim().is_empty() => None,
        Some(Value::String(k)) => Some(crate::map::ApiKey::parse(k).map_err(|e| bad(C, e))?),
        Some(_) => return Err(bad(C, "`key` must be text")),
    };
    s.map.set_esri_key(key).map_err(|e| EngineError::Other(format!("{C}: {e}")))?;
    Ok(json!({"esriKeySet": s.map.esri_key_set(), "notice": s.map.fallback_notice()}))
}

fn opt_str<'a>(p: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v.as_str())),
        Some(other) => Err(bad(SOURCE, format!("`{key}` must be text, not {other}"))),
    }
}

/// The source `{id?, urlTemplate?, attribution?, maxZoom?, offline?}` asks for, from `cur`
/// (`None`: nothing asked, report only).
fn requested(cur: &TileSource, p: &Value) -> Result<Option<TileSource>> {
    let id = opt_str(p, "id")?.map(str::trim).filter(|v| !v.is_empty());
    let template = opt_str(p, "urlTemplate")?.map(str::trim).filter(|v| !v.is_empty());
    let attribution = opt_str(p, "attribution")?;
    let max_zoom = match p.get("maxZoom") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().and_then(|z| u8::try_from(z).ok()).ok_or_else(|| bad(SOURCE, "maxZoom must be a whole number 0–24"))?),
    };
    let offline = match p.get("offline") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool().ok_or_else(|| bad(SOURCE, "offline must be true or false"))?),
    };
    if id.is_none() && template.is_none() && attribution.is_none() && max_zoom.is_none() && offline.is_none() {
        return Ok(None);
    }
    if offline == Some(true) {
        return Ok(Some(TileSource::Offline));
    }
    if offline == Some(false) && id.is_none() && template.is_none() && cur.is_offline() {
        return Ok(Some(TileSource::Osm));
    }
    let check = |r: std::result::Result<TileSource, crate::map::MapError>| r.map_err(|e| bad(SOURCE, e.to_string()));
    // a built-in source by name
    if template.is_none()
        && attribution.is_none()
        && let Some(built_in) = id.and_then(TileSource::built_in)
    {
        if max_zoom.is_some_and(|z| z != built_in.max_zoom()) {
            return Err(bad(SOURCE, format!("{} has a fixed max zoom of {}", built_in.label(), built_in.max_zoom())));
        }
        return Ok(Some(built_in));
    }
    // a change to the current custom source, or a new one
    let base = match cur {
        TileSource::Custom { url_template, .. } if template.is_none_or(|t| t == url_template) => Some(cur),
        _ => None,
    };
    let Some(template) = template.or(base.and_then(TileSource::url_template)) else {
        let known = TileSource::BUILT_IN.iter().map(TileSource::id).collect::<Vec<_>>().join(", ");
        return Err(bad(
            SOURCE,
            format!("give id (one of {known}), urlTemplate (https://…/{{z}}/{{x}}/{{y}}.png) for a tile server, or {{offline: true}}"),
        ));
    };
    let attribution = attribution.or(base.map(TileSource::attribution));
    let max_zoom = max_zoom.or(base.map(TileSource::max_zoom));
    let id = id.or(base.map(TileSource::id));
    check(TileSource::custom(id, template, attribution, max_zoom)).map(Some)
}

fn tile_source(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(next) = requested(s.map.source(), p)? {
        let old = s.map.source().clone();
        s.map.choose(next).map_err(|e| bad(SOURCE, e))?;
        // a source that can't be saved isn't kept: an error means nothing changed
        if let Err(e) = s.save_prefs() {
            s.map.restore(old);
            return Err(e);
        }
    }
    Ok(source_json(s))
}

fn providers(s: &mut Session, _: &Value) -> Result<Value> {
    let current = s.map.effective_source();
    let list: Vec<Value> = s
        .map
        .offered()
        .iter()
        .map(|p| {
            json!({
                "id": p.id(),
                "label": p.label(),
                "maxZoom": p.max_zoom(),
                "attribution": p.attribution(),
                "offline": p.is_offline(),
                "current": *p == current,
            })
        })
        .collect();
    Ok(json!(list))
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

// ---------------------------------------------------------------- saved locations
// (ported from PR #906, Jason Baldwin)

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
fn around_photos(s: &Session, ids: &[lightcraft_catalog::PhotoId]) -> Option<(f64, f64, f64)> {
    let pos: Vec<(f64, f64)> =
        ids.iter().filter_map(|id| s.catalog.photo(*id)).filter_map(|p| lightcraft_catalog::rules::valid_gps(p.meta.gps)).collect();
    let b = crate::map::Bounds::of(pos.iter().copied())?;
    let c = b.center();
    let lon = crate::map::mercator::wrap_lon(c.lon);
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
    let private = opt_bool(C, p, "private")?.or(opt_bool(C, p, "isPrivate")?).unwrap_or(false);
    let (lat, lon, radius) = (opt_num(C, p, "lat")?, opt_num(C, p, "lon")?, opt_num(C, p, "radius")?);
    let from = opt_bool(C, p, "fromSelection")?.unwrap_or(false);
    let (lat, lon, radius) = match (lat, lon) {
        (Some(lat), Some(lon)) => (lat, lon, radius.unwrap_or(1000.0)),
        (None, None) if from => {
            let ids = s.targets(p).map_err(|e| match e {
                EngineError::BadParams { msg, .. } => bad(C, msg),
                other => other,
            })?;
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
    let mut v = json!({"removed": gone.id, "name": gone.name, "deleted": true});
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
    let ids: Vec<u64> = if in_view {
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
        cmd!(query "map.providers", "Map Providers", [], None, "{} → [{id, label, maxZoom, attribution, offline, current}] — the tile sources the map offers now (the Esri layers only once an ArcGIS API key is set with map.setEsriKey: see docs/map.md); set one with map.tileSource", always, providers),
        cmd!(
            "map.tileSource",
            "Map Tile Source",
            [],
            None,
            "{} → the map's tile source; set with {id: \"osm\" | \"opentopomap\" | \"esri-streets\" | \"esri-topo\" | \"offline\"}, {offline: true|false}, or {urlTemplate: \"https://…/{z}/{x}/{y}.png\", attribution: text, maxZoom?: 0–24, id?: cache-folder name [a-z0-9._-]}; fields alone change the current custom source. The Esri layers need an ArcGIS API key (map.setEsriKey) and are refused without one. Saved with the library preferences → {id, label, urlTemplate, attribution, maxZoom, userAgent, offline, custom, saved, notice, esriKeySet}",
            always,
            tile_source
        ),
        // a query spec on purpose: not journaled, so the key never reaches the journal
        cmd!(
            query "map.setEsriKey",
            "Set ArcGIS API Key",
            [],
            None,
            "{key: text | \"\" | null} → {esriKeySet, notice} — the ArcGIS Location Platform API key for the Esri map layers (free at https://location.arcgis.com); \"\" or null clears it. Shape-checked only (no network call); kept in the config folder's map-keys.json; never reported back",
            always,
            set_esri_key
        ),
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
        cmd!(query "map.savedLocations.list", "List Saved Locations", [], None, "{} → the older name of map.locations (same answer)", always, locations),
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
            "map.savedLocations.create",
            "Create Saved Location",
            [],
            None,
            "the older name of map.location.add (same parameters; isPrivate is taken for private)",
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
            "{id} — delete a saved location (not an undo step; the photos keep their positions) → {removed, name, deleted}",
            always,
            location_remove
        ),
        cmd!("map.savedLocations.delete", "Remove Saved Location", [], None, "{id} — the older name of map.location.remove", always, location_remove),
        cmd!(query "map.location.photos", "Photos in Saved Location", [], None, "{id, inView?: bool} → {id, ids, count}: the library's photos inside the location (inView: only the photos in view, in their order)", always, location_photos),
        cmd!(query "map.filterPhotos", "Filter Photos for Map", [], None, "{bounds?: {minLat, maxLat, minLon, maxLon}, mode?: \"visible\"|\"tagged\"|\"untagged\"|\"none\"} → {photos: [{id, lat, lon, selected}]} — filters photos for map view", always, |s, p| {
            let mode = p.get("mode").and_then(Value::as_str).and_then(LocationFilter::parse).unwrap_or(LocationFilter::None);
            let bounds: Option<GeoBounds> = p.get("bounds").and_then(|b| {
                let min_lat = b.get("minLat")?.as_f64()?;
                let max_lat = b.get("maxLat")?.as_f64()?;
                let min_lon = b.get("minLon")?.as_f64()?;
                let max_lon = b.get("maxLon")?.as_f64()?;
                Some(GeoBounds::new(min_lat, max_lat, min_lon, max_lon))
            });

            let visible_ids = s.visible().to_vec();
            let mut out = Vec::new();
            for id in visible_ids {
                let Some(ph) = s.catalog.photo(id) else { continue };
                let is_selected = s.selection.contains(id);

                match (mode, ph.meta.gps) {
                    (LocationFilter::Untagged, None) => {
                        out.push(json!({ "id": id.0, "selected": is_selected }));
                    }
                    (LocationFilter::Tagged, Some((lat, lon))) => {
                        out.push(json!({ "id": id.0, "lat": lat, "lon": lon, "selected": is_selected }));
                    }
                    (LocationFilter::VisibleOnMap, Some((lat, lon))) => {
                        if let Some(bounds) = bounds {
                            if let Some(coord) = LatLon::new(lat, lon) && bounds.contains(coord) {
                                out.push(json!({ "id": id.0, "lat": lat, "lon": lon, "selected": is_selected }));
                            }
                        } else {
                            out.push(json!({ "id": id.0, "lat": lat, "lon": lon, "selected": is_selected }));
                        }
                    }
                    (LocationFilter::None, Some((lat, lon))) => {
                        out.push(json!({ "id": id.0, "lat": lat, "lon": lon, "selected": is_selected }));
                    }
                    (LocationFilter::None, None) => {
                        out.push(json!({ "id": id.0, "selected": is_selected }));
                    }
                    _ => {}
                }
            }
            Ok(json!({ "photos": out }))
        }),
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "photo.setGps",
                "Set GPS Coordinates",
                [],
                None,
                "{ids?: [PhotoId], lat: −90…90, lon: degrees (wrapped into −180…180)} → {updated, lat, lon} — sets the GPS position of the photos (default: the selection), one undo step \"Set GPS Coordinates\"; what dropping photos on the Map view does. Unknown ids are refused",
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
                "{ids?: [PhotoId]} → {changed, cleared} — clears the GPS position of the photos (default: the selection), one undo step \"Remove GPS Coordinates\"; Photo ▸ Remove GPS Coordinates and a map pin's right-click menu. Unknown ids are refused. Positions LightCraft set or cleared are removed from the photos' XMP sidecars too",
                has_selection,
                remove_gps
            )
        },
        CommandSpec {
            explicit_targets: true,
            ..cmd!(
                "photo.clearGps",
                "Clear Photo GPS Coordinates",
                [],
                None,
                "{ids?: [PhotoId]} → {changed, cleared} — the older name of photo.removeGps (same command)",
                has_selection,
                remove_gps
            )
        },
    ]
}
