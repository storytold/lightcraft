use crate::Session;
use serde_json::json;

/// The Esri layers used to be offered (and fetched) by default; their terms don't allow it, so
/// they are neither listed nor accepted.
#[test]
fn test_map_providers_command() {
    let mut s = Session::new();
    let res = s.execute("map.providers", &json!({})).unwrap();
    let ids: Vec<&str> = res.as_array().unwrap().iter().filter_map(|v| v.get("id")?.as_str()).collect();
    assert_eq!(ids, vec!["osm", "opentopomap", "offline"]);
    assert_eq!(res[0]["current"], true);
    assert_eq!(res[0]["attribution"], "© OpenStreetMap contributors");
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
    assert_eq!((r["offline"].as_bool(), r["esriKeySet"].as_bool()), (Some(false), Some(false)));

    let r = s.execute("map.tileSource", &json!({"offline": true})).unwrap();
    assert_eq!((r["id"].as_str(), r["offline"].as_bool()), (Some("offline"), Some(true)));
    assert!(s.map_tiles().is_none(), "offline starts no tile service");
    // a field alone needs a server when offline
    assert!(s.execute("map.tileSource", &json!({"maxZoom": 10})).is_err());
    let r = s.execute("map.tileSource", &json!({"offline": false})).unwrap();
    assert_eq!(r["id"], "osm");
    let r = s.execute("map.tileSource", &json!({"id": "opentopomap"})).unwrap();
    assert_eq!((r["id"].as_str(), r["maxZoom"].as_u64()), (Some("opentopomap"), Some(17)));

    let r = s
        .execute("map.tileSource", &json!({"urlTemplate": "https://tiles.example.org/{z}/{x}/{y}.png", "attribution": "© Example", "maxZoom": 17}))
        .unwrap();
    assert!(r["id"].as_str().unwrap().starts_with("custom-"));
    assert_eq!((r["maxZoom"].as_u64(), r["custom"].as_bool()), (Some(17), Some(true)));
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
        json!({"id": "osm", "maxZoom": 12}),
    ] {
        assert!(matches!(s.execute("map.tileSource", &bad), Err(crate::EngineError::BadParams { .. })), "{bad}");
    }
    assert_eq!(s.map.source().max_zoom(), 15);
    let r = s.execute("map.tileSource", &json!({"id": "osm"})).unwrap();
    assert_eq!(r["urlTemplate"], crate::map::OSM_TILE_URL);
}

const KEY: &str = "AAPK0123456789abcdef_TEST-key.x";

/// The Esri layers need the user's own ArcGIS API key: refused (with where to get one) and not
/// listed without it, listed and accepted with it; the key is never reported back.
#[test]
fn esri_sources_need_a_key_and_the_key_is_never_reported() {
    let mut s = Session::new();
    for id in ["esri-streets", "esri-topo"] {
        let e = s.execute("map.tileSource", &json!({"id": id})).unwrap_err().to_string();
        assert!(e.contains("API key") && e.contains("location.arcgis.com"), "{e}");
    }
    assert!(s.execute("map.tileSource", &json!({"id": "esri-imagery"})).is_err(), "no keyless imagery layer");
    assert_eq!(*s.map.source(), crate::map::TileSource::Osm);
    // a key of the wrong shape is refused without being repeated
    for bad in [json!({"key": "has spaces in it 0123456789"}), json!({"key": 5}), json!({})] {
        let e = s.execute("map.setEsriKey", &bad).unwrap_err().to_string();
        assert!(!e.contains("0123456789"), "{e}");
    }
    let r = s.execute("map.setEsriKey", &json!({"key": KEY})).unwrap();
    assert_eq!(r["esriKeySet"], true);
    let r = s.execute("map.tileSource", &json!({"id": "esri-streets"})).unwrap();
    assert_eq!((r["id"].as_str(), r["esriKeySet"].as_bool()), (Some("esri-streets"), Some(true)));
    assert!(r["attribution"].as_str().unwrap().starts_with("Powered by Esri"));
    let providers = s.execute("map.providers", &json!({})).unwrap();
    let ids: Vec<&str> = providers.as_array().unwrap().iter().filter_map(|p| p["id"].as_str()).collect();
    assert_eq!(ids, vec!["osm", "opentopomap", "esri-streets", "esri-topo", "offline"]);
    // the key appears in no reply, no journal entry and not in what prefs.json stores
    let everything = format!(
        "{r} {providers} {} {} {:?} {}",
        s.execute("map.tileSource", &json!({})).unwrap(),
        s.execute("map.setEsriKey", &json!({"key": KEY})).unwrap(),
        s.journal,
        serde_json::to_string(s.map.source()).unwrap()
    );
    assert!(!everything.contains(KEY), "{everything}");
    assert_eq!(s.map_tiles().unwrap().source(), &crate::map::TileSource::EsriStreet);
    // cleared: the map falls back to OpenStreetMap and keeps the choice, with a notice
    let r = s.execute("map.setEsriKey", &json!({"key": ""})).unwrap();
    assert_eq!(r["esriKeySet"], false);
    assert!(r["notice"].as_str().unwrap().contains("OpenStreetMap"), "{r}");
    let r = s.execute("map.tileSource", &json!({})).unwrap();
    assert_eq!((r["id"].as_str(), r["saved"].as_str()), (Some("osm"), Some("esri-streets")));
    assert_eq!(s.map_tiles().unwrap().source(), &crate::map::TileSource::Osm, "Esri tiles are never fetched without a key");
    assert!(s.execute("map.setEsriKey", &json!({"key": null})).is_ok());
}

#[test]
fn changing_the_source_restarts_the_service() {
    let mut s = Session::new();
    assert!(s.map.service().is_none(), "nothing starts until a map asks");
    assert_eq!(s.map_tiles().unwrap().source().id(), "osm");
    s.execute("map.tileSource", &json!({"id": "opentopomap"})).unwrap();
    assert!(s.map.service().is_none());
    assert_eq!(s.map_tiles().unwrap().source().id(), "opentopomap");
}

#[test]
fn cache_info_and_clear() {
    let mut s = Session::new();
    let r = s.execute("map.cacheInfo", &json!({})).unwrap();
    assert_eq!((r["bytes"].as_u64(), r["tiles"].as_u64(), r["dir"].is_null()), (Some(0), Some(0), true));
    assert_eq!(s.execute("map.clearCache", &json!({})).unwrap()["tiles"], 0);
    let root = std::env::temp_dir().join(format!("lightcraft-engine-map-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    s.map.cache_root = Some(root.clone());
    let c = crate::map::TileCache::new(&root, "osm", 1 << 30).unwrap();
    for x in 0..3 {
        c.write(crate::map::TileKey::new(4, x, 1).unwrap(), &Default::default(), b"tile bytes").unwrap();
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

#[test]
fn test_photo_set_and_clear_gps_with_undo() {
    let mut s = Session::with_demo();
    let ids = s.visible().to_vec();
    assert!(!ids.is_empty());
    let id = ids[0];

    // Select first photo
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();

    // Set GPS
    let res = s.execute("photo.setGps", &json!({"ids": [id.0], "lat": 45.57, "lon": -73.2})).unwrap();
    assert_eq!(res.get("updated").and_then(|v| v.as_u64()), Some(1));

    let ph = s.catalog.photo(id).unwrap();
    assert_eq!(ph.meta.gps, Some((45.57, -73.2)));

    // Undo set GPS
    s.execute("edit.undo", &json!({})).unwrap();
    let ph = s.catalog.photo(id).unwrap();
    assert_ne!(ph.meta.gps, Some((45.57, -73.2)));

    // Redo set GPS
    s.execute("edit.redo", &json!({})).unwrap();
    let ph = s.catalog.photo(id).unwrap();
    assert_eq!(ph.meta.gps, Some((45.57, -73.2)));

    // Clear GPS
    s.execute("photo.clearGps", &json!({"ids": [id.0]})).unwrap();
    let ph = s.catalog.photo(id).unwrap();
    assert_eq!(ph.meta.gps, None);

    // Undo clear GPS
    s.execute("edit.undo", &json!({})).unwrap();
    let ph = s.catalog.photo(id).unwrap();
    assert_eq!(ph.meta.gps, Some((45.57, -73.2)));
}

#[test]
fn test_saved_locations_crud() {
    let mut s = Session::with_demo();
    let id0 = s.visible()[0];

    // Tag photo in Beloeil area
    s.execute("photo.setGps", &json!({"ids": [id0.0], "lat": 45.57, "lon": -73.2})).unwrap();

    // Create saved location covering Beloeil (2000m radius)
    let created = s
        .execute(
            "map.savedLocations.create",
            &json!({
                "name": "Beloeil Studio",
                "lat": 45.57,
                "lon": -73.2,
                "radius": 2000.0,
                "isPrivate": true,
            }),
        )
        .unwrap();

    let loc_id = created.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    // List saved locations
    let list = s.execute("map.savedLocations.list", &json!({})).unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].get("name").and_then(|v| v.as_str()), Some("Beloeil Studio"));
    assert_eq!(arr[0].get("photoCount").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(arr[0].get("isPrivate").and_then(|v| v.as_bool()), Some(true));

    // Delete saved location
    s.execute("map.savedLocations.delete", &json!({"id": loc_id})).unwrap();
    let list_after = s.execute("map.savedLocations.list", &json!({})).unwrap();
    assert_eq!(list_after.as_array().unwrap().len(), 0);
}

#[test]
fn test_map_filter_photos() {
    let mut s = Session::with_demo();
    let ids = s.visible().to_vec();
    assert!(ids.len() >= 2);

    // Set GPS on first photo only
    s.execute("photo.setGps", &json!({"ids": [ids[0].0], "lat": 45.57, "lon": -73.2})).unwrap();
    s.execute("photo.clearGps", &json!({"ids": [ids[1].0]})).unwrap();

    // Filter "tagged"
    let tagged = s.execute("map.filterPhotos", &json!({"mode": "tagged"})).unwrap();
    let tagged_photos = tagged.get("photos").and_then(|v| v.as_array()).unwrap();
    assert!(tagged_photos.iter().any(|p| p.get("id").and_then(|v| v.as_u64()) == Some(ids[0].0)));
    assert!(!tagged_photos.iter().any(|p| p.get("id").and_then(|v| v.as_u64()) == Some(ids[1].0)));

    // Filter "untagged"
    let untagged = s.execute("map.filterPhotos", &json!({"mode": "untagged"})).unwrap();
    let untagged_photos = untagged.get("photos").and_then(|v| v.as_array()).unwrap();
    assert!(!untagged_photos.iter().any(|p| p.get("id").and_then(|v| v.as_u64()) == Some(ids[0].0)));
    assert!(untagged_photos.iter().any(|p| p.get("id").and_then(|v| v.as_u64()) == Some(ids[1].0)));
}

fn temp_lib(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lightcraft-map-prefs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The tile source is a library preference; a stored one is checked again when read.
#[test]
fn the_map_tile_source_persists_and_is_rechecked() {
    let lib = temp_lib("persist");
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        assert_eq!(s.map.source().id(), "osm");
        s.execute(
            "map.tileSource",
            &json!({"urlTemplate": "https://tiles.example.org/{z}/{x}/{y}.png", "attribution": "© Example", "maxZoom": 16, "id": "example"}),
        )
        .unwrap();
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let src = s.map.source().clone();
    assert_eq!((src.id(), src.max_zoom(), src.attribution()), ("example", 16, "© Example"));
    s.execute("map.tileSource", &json!({"offline": true})).unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert!(s.map.source().is_offline());
    drop(s);
    // a hand-edited prefs.json pointing at plain http falls back to OpenStreetMap
    let mut prefs: serde_json::Value = serde_json::from_slice(&std::fs::read(lib.join("prefs.json")).unwrap()).unwrap();
    prefs["mapSource"] = json!({"kind": "custom", "id": "evil", "urlTemplate": "http://evil.example/{z}/{x}/{y}", "attribution": "x", "maxZoom": 19});
    std::fs::write(lib.join("prefs.json"), serde_json::to_vec(&prefs).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(*s.map.source(), crate::map::TileSource::Osm);
    drop(s);
    let _ = std::fs::remove_dir_all(&lib);
}

/// A library saved with an Esri layer (by an older build) opens on OpenStreetMap with a one-line
/// notice, and prefs.json keeps the Esri choice.
#[test]
fn a_saved_esri_source_falls_back_with_a_notice_and_is_kept() {
    let lib = temp_lib("esri");
    std::fs::write(lib.join("prefs.json"), br#"{"cacheMb": 321, "mapSource": {"kind": "esriTopo"}}"#).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.cache_mb, 321);
    assert_eq!(s.map.effective_source(), crate::map::TileSource::Osm);
    let warnings = s.take_library_warnings();
    assert_eq!(warnings.iter().filter(|w| w.contains("OpenStreetMap") && w.contains("Esri")).count(), 1, "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("location.arcgis.com")), "the notice says where to get a key: {warnings:?}");
    // saving other preferences keeps the Esri choice in the file
    s.save_prefs().unwrap();
    let prefs: serde_json::Value = serde_json::from_slice(&std::fs::read(lib.join("prefs.json")).unwrap()).unwrap();
    assert_eq!(prefs["mapSource"], json!({"kind": "esriTopo"}));
    drop(s);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn a_damaged_saved_tile_source_keeps_the_other_preferences() {
    let lib = temp_lib("damaged");
    std::fs::write(lib.join("prefs.json"), br#"{"cacheMb": 777, "mapSource": {"kind": "custom", "urlTemplate": 5, "maxZoom": "high"}}"#).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.cache_mb, 777, "the rest of prefs.json is read");
    assert_eq!(s.map.source().id(), "osm", "the damaged source falls back to OpenStreetMap");
    drop(s);
    let _ = std::fs::remove_dir_all(&lib);
}

/// What a drop of several photos on the Map view runs: all of them (one already placed
/// elsewhere) get the drop point in one undo step, and undo restores each one's old position.
#[test]
fn set_gps_moves_every_named_photo_in_one_undo_step() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.visible().iter().take(3).map(|p| p.0).collect();
    s.execute("photo.setGps", &json!({"ids": [ids[0]], "lat": 10.0, "lon": 20.0})).unwrap();
    let steps = s.undo.len();
    let r = s.execute("photo.setGps", &json!({"ids": ids, "lat": 48.85, "lon": 2.29})).unwrap();
    assert_eq!(r["updated"], 3, "{r}");
    assert_eq!(s.undo.len(), steps + 1, "one undo step");
    assert_eq!(s.undo.last().map(|u| u.label.as_str()), Some("Set GPS Coordinates"));
    let gps = |s: &Session, id: u64| s.catalog.photo(lightcraft_catalog::PhotoId(id)).and_then(|p| p.meta.gps);
    assert!(ids.iter().all(|i| gps(&s, *i) == Some((48.85, 2.29))));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(gps(&s, ids[0]), Some((10.0, 20.0)), "the old position comes back");
    assert_eq!((gps(&s, ids[1]), gps(&s, ids[2])), (None, None));
    // the same position again is no new undo step
    s.execute("edit.redo", &json!({})).unwrap();
    let steps = s.undo.len();
    assert_eq!(s.execute("photo.setGps", &json!({"ids": [ids[1]], "lat": 48.85, "lon": 2.29})).unwrap()["updated"], 0);
    assert_eq!(s.undo.len(), steps);
}

/// A longitude past the antimeridian (a drop on the world's repeat at low zoom) is the same
/// meridian, wrapped; a latitude out of range, a non-number or an id the library doesn't have is
/// refused and nothing changes.
#[test]
fn set_gps_wraps_longitude_and_refuses_hostile_values() {
    let mut s = Session::with_demo();
    let id = s.visible()[0].0;
    let r = s.execute("photo.setGps", &json!({"ids": [id], "lat": -33.9, "lon": 190.0})).unwrap();
    assert_eq!(r["lon"].as_f64(), Some(-170.0), "{r}");
    assert_eq!(s.catalog.photo(lightcraft_catalog::PhotoId(id)).and_then(|p| p.meta.gps), Some((-33.9, -170.0)));
    let steps = s.undo.len();
    for bad in [
        json!({"ids": [id], "lat": 91.0, "lon": 0.0}),
        json!({"ids": [id], "lat": -90.5, "lon": 0.0}),
        json!({"ids": [id], "lat": "north", "lon": 0.0}),
        json!({"ids": [id], "lon": 0.0}),
        json!({"ids": [id], "lat": 0.0}),
        json!({"ids": [id], "lat": 0.0, "lon": [1]}),
        json!({"ids": [id, 987_654_321u64], "lat": 1.0, "lon": 1.0}),
        json!({"ids": "1", "lat": 1.0, "lon": 1.0}),
        json!({"ids": [], "lat": 1.0, "lon": 1.0}),
    ] {
        assert!(s.execute("photo.setGps", &bad).is_err(), "{bad}");
    }
    assert_eq!(s.undo.len(), steps, "nothing changed");
    assert_eq!(s.catalog.photo(lightcraft_catalog::PhotoId(id)).and_then(|p| p.meta.gps), Some((-33.9, -170.0)));
}

/// `photo.removeGps` (Photo ▸ Remove GPS Coordinates, a pin's menu) clears the selection's position
/// in one undo step; ids the library doesn't have are refused; `photo.clearGps` is the same command.
#[test]
fn remove_gps_clears_in_one_undo_step_and_refuses_unknown_ids() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    s.execute("photo.setGps", &json!({"ids": ids, "lat": 51.5, "lon": -0.12})).unwrap();
    let gps = |s: &Session, id: u64| s.catalog.photo(lightcraft_catalog::PhotoId(id)).and_then(|p| p.meta.gps);
    s.execute("library.select", &json!({"ids": [ids[0], ids[1]]})).unwrap();
    let steps = s.undo.len();
    let r = s.execute("photo.removeGps", &json!({})).unwrap();
    assert_eq!(r["changed"], 2, "{r}");
    assert_eq!((gps(&s, ids[0]), gps(&s, ids[1]), gps(&s, ids[2])), (None, None, Some((51.5, -0.12))));
    assert_eq!(s.undo.len(), steps + 1);
    assert_eq!(s.undo.last().map(|u| u.label.as_str()), Some("Remove GPS Coordinates"));
    // nothing to clear: no empty undo step
    assert_eq!(s.execute("photo.removeGps", &json!({})).unwrap()["changed"], 0);
    assert_eq!(s.undo.len(), steps + 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!((gps(&s, ids[0]), gps(&s, ids[1])), (Some((51.5, -0.12)), Some((51.5, -0.12))));
    for bad in [json!({"ids": [987_654_321]}), json!({"ids": "1"})] {
        assert!(matches!(s.execute("photo.removeGps", &bad), Err(crate::EngineError::BadParams { .. })), "{bad}");
    }
    assert_eq!(gps(&s, ids[0]), Some((51.5, -0.12)), "a refusal changes nothing");
    let r = s.execute("photo.clearGps", &json!({"ids": [ids[2]]})).unwrap();
    assert_eq!((r["changed"].as_u64(), gps(&s, ids[2])), (Some(1), None));
    assert!(s.gps_edited.contains(&lightcraft_catalog::PhotoId(ids[2])), "its sidecar GPS is LightCraft's now");
}
