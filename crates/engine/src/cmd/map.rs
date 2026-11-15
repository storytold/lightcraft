//! Map module commands: tile sources and the tile cache, saved locations, location filtering, and
//! GPS tagging. The tile-source commands are ported from PR #797 (Jason Baldwin).

use lightcraft_catalog::Op;
use lightcraft_geom::{GeoBounds, LatLon};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, f64_req, has_selection, str_param};
use crate::map::{LocationFilter, SavedLocation, TileSource, cache};
use crate::{EngineError, Result, Session};

const SOURCE: &str = "map.tileSource";

fn source_json(s: &Session) -> Value {
    let chosen = s.map.source();
    let used = s.map.effective_source();
    json!({
        "id": used.id(),
        "label": used.label(),
        "urlTemplate": used.url_template(),
        "attribution": used.attribution(),
        "maxZoom": used.max_zoom(),
        "userAgent": used.user_agent(),
        "offline": used.is_offline(),
        "custom": matches!(used, TileSource::Custom { .. }),
        // what prefs.json keeps, when the map can't use it (an Esri layer while Esri is off)
        "saved": (*chosen != used).then(|| chosen.id().to_string()),
        "notice": s.map.fallback_notice(),
        "esriEnabled": s.map.esri_enabled(),
    })
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

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "map.providers", "Map Providers", [], None, "{} → [{id, label, maxZoom, attribution, offline, current}] — the tile sources the map offers now (the Esri layers are not offered: see docs/map.md); set one with map.tileSource", always, providers),
        cmd!(
            "map.tileSource",
            "Map Tile Source",
            [],
            None,
            "{} → the map's tile source; set with {id: \"osm\" | \"opentopomap\" | \"offline\"}, {offline: true|false}, or {urlTemplate: \"https://…/{z}/{x}/{y}.png\", attribution: text, maxZoom?: 0–24, id?: cache-folder name [a-z0-9._-]}; fields alone change the current custom source. The Esri layers are refused (their terms, docs/map.md). Saved with the library preferences → {id, label, urlTemplate, attribution, maxZoom, userAgent, offline, custom, saved, notice, esriEnabled}",
            always,
            tile_source
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
        cmd!(query "map.savedLocations.list", "List Saved Locations", [], None, "{} → [{id, name, lat, lon, radius, isPrivate, photoCount}] — lists user saved locations with count of photos within boundary", always, |s, _| {
            let visible_ids = s.visible().to_vec();
            let mut list = Vec::new();
            for loc in &s.saved_locations {
                let mut count = 0;
                for id in &visible_ids {
                    if let Some(ph) = s.catalog.photo(*id)
                        && let Some((lat, lon)) = ph.meta.gps
                        && let Some(p) = LatLon::new(lat, lon)
                        && loc.contains(p)
                    {
                        count += 1;
                    }
                }
                list.push(json!({
                    "id": loc.id,
                    "name": loc.name,
                    "lat": loc.lat,
                    "lon": loc.lon,
                    "radius": loc.radius,
                    "isPrivate": loc.is_private,
                    "photoCount": count,
                }));
            }
            Ok(json!(list))
        }),
        cmd!(
            "map.savedLocations.create",
            "Create Saved Location",
            [],
            None,
            "{name: str, lat: f64, lon: f64, radius?: f64 (meters, default 1000), isPrivate?: bool} → {id, name, ...} — creates a new saved location",
            always,
            |s, p| {
                const C: &str = "map.savedLocations.create";
                let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "missing `name`"))?.to_string();
                let lat = f64_req(p, "lat", C)?;
                let lon = f64_req(p, "lon", C)?;
                if LatLon::new(lat, lon).is_none() {
                    return Err(bad(C, "lat/lon out of valid range or non-finite"));
                }
                let radius = p.get("radius").and_then(Value::as_f64).unwrap_or(1000.0).max(1.0);
                let is_private = p.get("isPrivate").and_then(Value::as_bool).unwrap_or(false);

                let id = format!("loc-{}", s.saved_locations.len() + 1);
                let loc = SavedLocation::new(&id, &name, lat, lon, radius, is_private);
                s.saved_locations.push(loc.clone());
                let _ = s.save_prefs();
                Ok(json!(loc))
            }
        ),
        cmd!(
            "map.savedLocations.delete",
            "Delete Saved Location",
            [],
            None,
            "{id: str} → {deleted: true} — removes a saved location",
            always,
            |s, p| {
                const C: &str = "map.savedLocations.delete";
                let id = str_param(p, "id").ok_or_else(|| bad(C, "missing `id`"))?;
                let before_len = s.saved_locations.len();
                s.saved_locations.retain(|loc| loc.id != id);
                if s.saved_locations.len() == before_len {
                    return Err(bad(C, format!("saved location `{id}` not found")));
                }
                let _ = s.save_prefs();
                Ok(json!({ "deleted": true }))
            }
        ),
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
        cmd!(
            "photo.setGps",
            "Set Photo GPS Coordinates",
            [],
            None,
            "{ids?: [PhotoId], lat: f64, lon: f64} → {updated: count} — sets GPS coordinates for target photos (undoable)",
            has_selection,
            |s, p| {
                const C: &str = "photo.setGps";
                let lat = f64_req(p, "lat", C)?;
                let lon = f64_req(p, "lon", C)?;
                if LatLon::new(lat, lon).is_none() {
                    return Err(bad(C, "lat/lon out of range or non-finite"));
                }
                let pos = ((lat * 1e7).round() / 1e7, (lon * 1e7).round() / 1e7);
                let mut ops = Vec::new();
                for id in s.targets(p)? {
                    let Some(ph) = s.catalog.photo(id) else { continue };
                    if ph.meta.gps != Some(pos) {
                        let mut meta = ph.meta.clone();
                        meta.gps = Some(pos);
                        ops.push(Op::SetMeta { id, meta: Box::new(meta) });
                    }
                }
                let count = ops.len();
                if !ops.is_empty() {
                    s.commit("Set GPS Location", Op::Batch { ops })?;
                }
                Ok(json!({ "updated": count }))
            }
        ),
        cmd!(
            "photo.clearGps",
            "Clear Photo GPS Coordinates",
            [],
            None,
            "{ids?: [PhotoId]} → {cleared: count} — clears GPS coordinates for target photos (undoable)",
            has_selection,
            |s, p| {
                let mut ops = Vec::new();
                for id in s.targets(p)? {
                    let Some(ph) = s.catalog.photo(id) else { continue };
                    if ph.meta.gps.is_some() {
                        let mut meta = ph.meta.clone();
                        meta.gps = None;
                        ops.push(Op::SetMeta { id, meta: Box::new(meta) });
                    }
                }
                let count = ops.len();
                if !ops.is_empty() {
                    s.commit("Clear GPS Location", Op::Batch { ops })?;
                }
                Ok(json!({ "cleared": count }))
            }
        ),
    ]
}
