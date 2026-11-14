//! Map module commands: tile provider registry, saved locations, location filtering, and GPS tagging.

use lightcraft_catalog::Op;
use lightcraft_geom::{GeoBounds, LatLon};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, f64_req, has_selection, str_param};
use crate::map::{LocationFilter, MapProvider, SavedLocation};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "map.providers", "Map Providers", [], None, "{} → [{id, label, maxZoom, attribution}] — lists supported map tile providers", always, |_, _| {
            let providers: Vec<Value> = MapProvider::ALL.iter().map(|p| json!({
                "id": p.id(),
                "label": p.label(),
                "maxZoom": p.max_zoom(),
                "attribution": p.attribution(),
            })).collect();
            Ok(json!(providers))
        }),
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
