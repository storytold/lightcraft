//! Map commands: the tile source (where map tiles come from, saved with the library preferences)
//! and the tile cache. See docs/map.md.

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
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

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "map.tileSource",
            "Map Tile Source",
            [],
            None,
            "{} → the map's tile source; set with {offline: true} (no network, no tiles), {id: \"osm\"} (OpenStreetMap, the default), or {urlTemplate: \"https://…/{z}/{x}/{y}.png\", attribution: text (required except for OpenStreetMap's template), maxZoom?: 0–24, id?: cache-folder name [a-z0-9._-]}; fields alone change the current source. Saved with the library preferences → {id, urlTemplate, attribution, maxZoom, userAgent, offline}",
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
