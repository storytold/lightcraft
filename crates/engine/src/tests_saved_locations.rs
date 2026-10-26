//! Saved locations (LRC-MAP-LOCATIONS): the commands, their checks and counts (across the
//! antimeridian and around a pole), `locations.json` (saved, reloaded, a damaged or hostile file
//! set aside, never a panic), and private locations stripping the location from exports of the
//! photos inside them, and only those.

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use crate::export::{ExportOptions, export_photo};
use crate::tests_xmp::temp_dir;
use crate::{EngineError, Session};

fn ids(s: &Session, n: usize) -> Vec<PhotoId> {
    s.catalog.photos().take(n).map(|p| p.id).collect()
}

fn place(s: &mut Session, id: PhotoId, lat: f64, lon: f64) {
    s.execute("photo.setGps", &json!({"ids": [id.0], "lat": lat, "lon": lon})).unwrap();
}

fn add(s: &mut Session, p: Value) -> Value {
    s.execute("map.location.add", &p).unwrap()
}

fn listed(s: &mut Session) -> Vec<Value> {
    s.execute("map.locations", &json!({})).unwrap()["locations"].as_array().cloned().unwrap_or_default()
}

fn ids_in(s: &mut Session, id: &Value) -> Vec<u64> {
    s.execute("map.location.photos", &json!({"id": id})).unwrap()["ids"].as_array().unwrap().iter().filter_map(Value::as_u64).collect()
}

#[test]
fn locations_count_the_photos_inside_them_across_the_antimeridian_and_a_pole() {
    let mut s = Session::with_demo();
    let p = ids(&s, 7);
    // two near the Eiffel Tower, one in London; two either side of ±180° in Fiji; two near the north pole on opposite meridians
    for (id, (lat, lon)) in
        p.iter().zip([(48.8584, 2.2945), (48.8606, 2.2900), (51.5007, -0.1246), (-17.0, 179.95), (-17.0, -179.95), (89.5, 0.0), (89.5, 180.0)])
    {
        place(&mut s, *id, lat, lon);
    }
    let paris = add(&mut s, json!({"name": "Paris", "lat": 48.8584, "lon": 2.2945, "radius": 1000}));
    assert_eq!((paris["id"].as_u64(), paris["photos"].as_u64(), paris["private"].as_bool()), (Some(1), Some(2), Some(false)), "{paris}");
    let fiji = add(&mut s, json!({"name": "Fiji", "lat": -17.0, "lon": 180.0, "radius": 20000}));
    assert_eq!(fiji["photos"], 2, "both sides of the antimeridian: {fiji}");
    let pole = add(&mut s, json!({"name": "Pole", "lat": 90.0, "lon": 0.0, "radius": 100000, "private": true}));
    assert_eq!(pole["photos"], 2, "around the pole: {pole}");
    let all = listed(&mut s);
    assert_eq!(all.iter().map(|l| l["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Paris", "Fiji", "Pole"]);
    assert_eq!(all.iter().map(|l| l["photos"].as_u64().unwrap()).collect::<Vec<_>>(), [2, 2, 2]);
    // the demo shows every photo, so in view = in the library
    assert_eq!(all.iter().map(|l| l["inView"].as_u64().unwrap()).collect::<Vec<_>>(), [2, 2, 2]);
    let mut want = vec![p[3].0, p[4].0];
    want.sort();
    assert_eq!(ids_in(&mut s, &fiji["id"]), want);
    // the photos in view, in their order
    let r = s.execute("map.location.photos", &json!({"id": paris["id"], "inView": true})).unwrap();
    assert_eq!(r["count"], 2);
    // moving the circle changes what is inside
    let r = s.execute("map.location.update", &json!({"id": paris["id"], "lat": 51.5007, "lon": -0.1246, "name": "London"})).unwrap();
    assert_eq!((r["name"].as_str(), r["photos"].as_u64()), (Some("London"), Some(1)));
    let r = s.execute("map.location.update", &json!({"id": paris["id"], "radius": 400000})).unwrap();
    assert_eq!(r["photos"], 3, "London and Paris within 400 km: {r}");
    // deleting it leaves the photos alone
    let r = s.execute("map.location.remove", &json!({"id": paris["id"]})).unwrap();
    assert_eq!(r["name"], "London");
    assert_eq!(listed(&mut s).len(), 2);
    assert!(s.catalog.photo(p[0]).unwrap().meta.gps.is_some());
    // not undo steps: Edit ▸ Undo is still about the photos
    assert_eq!(s.undo.last().map(|u| u.label.as_str()), Some("Set GPS Coordinates"));
}

#[test]
fn location_commands_refuse_hostile_values_and_change_nothing() {
    let mut s = Session::with_demo();
    let home = add(&mut s, json!({"name": "Home", "lat": 10.0, "lon": 10.0, "radius": 50}));
    for bad in [
        json!({}),
        json!({"name": "", "lat": 0, "lon": 0}),
        json!({"name": "   ", "lat": 0, "lon": 0}),
        json!({"name": 5, "lat": 0, "lon": 0}),
        json!({"name": "x".repeat(201), "lat": 0, "lon": 0}),
        json!({"name": "x", "lat": 90.01, "lon": 0}),
        json!({"name": "x", "lat": 0, "lon": -180.5}),
        json!({"name": "x", "lat": "north", "lon": 0}),
        json!({"name": "x", "lat": 0}),
        json!({"name": "x", "lat": 0, "lon": 0, "radius": 0.5}),
        json!({"name": "x", "lat": 0, "lon": 0, "radius": 1000001}),
        json!({"name": "x", "lat": 0, "lon": 0, "radius": -10}),
        json!({"name": "x", "lat": 0, "lon": 0, "private": "yes"}),
        json!({"name": "x", "fromSelection": true, "ids": []}),
    ] {
        assert!(matches!(s.execute("map.location.add", &bad), Err(EngineError::BadParams { .. })), "{bad}");
    }
    // a number too large for f64 isn't a coordinate either
    if let Ok(huge) = serde_json::from_str::<Value>(r#"{"name": "x", "lat": 1e400, "lon": 0}"#) {
        assert!(s.execute("map.location.add", &huge).is_err());
    }
    for bad in [
        json!({"id": 999}),
        json!({"id": "1"}),
        json!({"id": home["id"], "radius": 0}),
        json!({"id": home["id"], "lat": 100}),
        json!({"id": home["id"], "name": ""}),
        json!({"id": home["id"], "name": "Ok", "radius": 2000000}),
    ] {
        assert!(matches!(s.execute("map.location.update", &bad), Err(EngineError::BadParams { .. })), "{bad}");
    }
    assert!(s.execute("map.location.remove", &json!({"id": 999})).is_err());
    assert!(s.execute("map.location.photos", &json!({})).is_err());
    let all = listed(&mut s);
    assert_eq!(all.len(), 1);
    assert_eq!((all[0]["name"].as_str(), all[0]["radius"].as_f64()), (Some("Home"), Some(50.0)), "nothing changed");
}

#[test]
fn a_location_made_from_the_selection_holds_its_photos() {
    let mut s = Session::with_demo();
    let p = ids(&s, 4);
    place(&mut s, p[0], 48.8584, 2.2945);
    place(&mut s, p[1], 48.8530, 2.3499);
    place(&mut s, p[2], 48.8606, 2.3376);
    s.execute("library.select", &json!({"ids": [p[0].0, p[1].0, p[2].0, p[3].0]})).unwrap();
    let l = add(&mut s, json!({"name": "Paris", "fromSelection": true}));
    assert_eq!(l["photos"], 3, "{l}");
    let r = l["radius"].as_f64().unwrap();
    assert!((2000.0..4000.0).contains(&r), "{l}");
    // a selection without positions has nothing to centre on
    s.execute("library.select", &json!({"ids": [p[3].0]})).unwrap();
    assert!(s.execute("map.location.add", &json!({"name": "Nowhere", "fromSelection": true})).is_err());
}

#[test]
fn locations_are_saved_with_the_library_and_reloaded() {
    let dir = temp_dir("locations-persist");
    {
        let mut s = Session::new().with_fs();
        s.open_library(&dir, false).unwrap();
        add(&mut s, json!({"name": "Home", "lat": 48.86, "lon": 2.33, "radius": 300, "private": true}));
        let park = add(&mut s, json!({"name": "Park", "lat": 48.85, "lon": 2.29, "radius": 2000}));
        add(&mut s, json!({"name": "Gone", "lat": 1.0, "lon": 1.0, "radius": 10}));
        s.execute("map.location.update", &json!({"id": park["id"], "name": "Big Park"})).unwrap();
        s.execute("map.location.remove", &json!({"id": 3})).unwrap();
        s.close_library().unwrap();
    }
    let text = std::fs::read_to_string(dir.join("locations.json")).unwrap();
    assert!(text.contains("\"Big Park\"") && !text.contains("Gone"), "{text}");
    assert!(!dir.join("locations.json.part").exists());
    let mut s = Session::new().with_fs();
    s.open_library(&dir, false).unwrap();
    assert!(s.take_library_warnings().is_empty());
    let all = listed(&mut s);
    assert_eq!(all.iter().map(|l| l["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Home", "Big Park"]);
    assert_eq!(all[0]["private"], true);
    // a new one gets a fresh id
    let l = add(&mut s, json!({"name": "New", "lat": 0.0, "lon": 0.0}));
    assert_eq!((l["id"].as_u64(), l["radius"].as_f64()), (Some(3), Some(1000.0)));
    drop(s);
    // another library has its own (none)
    let other = temp_dir("locations-other");
    let mut s = Session::new().with_fs();
    s.open_library(&other, false).unwrap();
    assert!(listed(&mut s).is_empty());
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
}

fn corrupt_copies(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("locations.json.corrupt-"))
        .collect()
}

#[test]
fn a_damaged_locations_file_is_set_aside_and_the_library_opens() {
    let dir = temp_dir("locations-damaged");
    let bad = br#"{"version": 1, "locations": [{"id": 1, "name": "Ho"#;
    std::fs::write(dir.join("locations.json"), bad).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&dir, true).unwrap();
    assert!(listed(&mut s).is_empty());
    let kept = corrupt_copies(&dir);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(std::fs::read(dir.join(&kept[0])).unwrap(), bad);
    let w = s.take_library_warnings();
    assert!(w.iter().any(|w| w.contains("locations.json is damaged")), "{w:?}");
    assert!(s.catalog.photos().next().is_some(), "the library itself is fine");
    // saving goes on
    add(&mut s, json!({"name": "Home", "lat": 1.0, "lon": 1.0}));
    assert!(std::fs::read_to_string(dir.join("locations.json")).unwrap().contains("Home"));
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_entries_are_left_out_and_the_file_kept_aside() {
    let dir = temp_dir("locations-hostile");
    let mut entries = vec![
        json!({"id": 1, "name": "Good", "lat": 48.0, "lon": 2.0, "radius": 100.0, "private": true}),
        json!({"id": 2, "name": "NaN-ish", "lat": "NaN", "lon": 2.0, "radius": 100.0}),
        json!({"id": 3, "name": "Huge", "lat": 1e308, "lon": 2.0, "radius": 100.0}),
        json!({"id": 4, "name": "", "lat": 1.0, "lon": 2.0, "radius": 100.0}),
        json!({"id": 5, "name": "Zero", "lat": 1.0, "lon": 2.0, "radius": 0}),
        json!({"id": 1, "name": "Same id", "lat": 1.0, "lon": 2.0, "radius": 100.0}),
        json!({"id": 18446744073709551615u64, "name": "Max id", "lat": 1.0, "lon": 2.0, "radius": 100.0}),
        json!([1, 2, 3]),
    ];
    // and thousands of good ones
    for i in 0..3000u64 {
        entries
            .push(json!({"id": 100 + i, "name": format!("L{i}"), "lat": (i % 170) as f64 - 85.0, "lon": (i % 359) as f64 - 179.0, "radius": 5000.0}));
    }
    let text = serde_json::to_string(&json!({"version": 99, "locations": entries})).unwrap();
    std::fs::write(dir.join("locations.json"), &text).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&dir, true).unwrap();
    let all = listed(&mut s);
    assert_eq!(all.len(), 3001);
    assert_eq!((all[0]["name"].as_str(), all[0]["private"].as_bool()), (Some("Good"), Some(true)));
    let w = s.take_library_warnings();
    assert!(w.iter().any(|w| w.contains("7 saved location(s)")), "{w:?}");
    let kept = corrupt_copies(&dir);
    assert_eq!(kept.len(), 1);
    assert_eq!(std::fs::read_to_string(dir.join(&kept[0])).unwrap(), text);
    // counting thousands of locations over the library stays quick
    let r = s.execute("map.locations", &json!({})).unwrap();
    eprintln!("3001 locations counted in {} ms", r["countMs"]);
    assert!(r["countMs"].as_f64().unwrap() < 2000.0, "{}", r["countMs"]);
    drop(s);
    // the good ones were written back: opening again finds nothing to set aside or warn about
    let mut s = Session::new().with_fs();
    s.open_library(&dir, true).unwrap();
    assert_eq!(listed(&mut s).len(), 3001);
    assert!(s.take_library_warnings().iter().all(|w| !w.contains("locations.json")));
    assert_eq!(corrupt_copies(&dir).len(), 1, "no second copy");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_locations_file_that_cant_be_read_is_never_overwritten() {
    let dir = temp_dir("locations-unreadable");
    // a folder where the file should be: reading fails
    std::fs::create_dir_all(dir.join("locations.json")).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&dir, false).unwrap();
    assert!(listed(&mut s).is_empty());
    // the change stays for the session and the command says why it wasn't saved
    let l = add(&mut s, json!({"name": "Home", "lat": 1.0, "lon": 1.0}));
    assert!(l["notSaved"].is_string(), "{l}");
    assert_eq!(listed(&mut s).len(), 1);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

fn exported_gps(s: &mut Session, id: PhotoId, o: &ExportOptions) -> (Option<lightcraft_meta::Gps>, Option<String>) {
    let e = export_photo(s, id, o, 1).unwrap();
    let m = lightcraft_meta::extract(&e.bytes);
    (m.gps, m.city)
}

#[test]
fn photos_in_a_private_location_are_exported_without_their_location() {
    let mut s = Session::with_demo();
    let p = ids(&s, 3);
    // home (private), the park (public, overlapping home's edge… not quite), elsewhere
    place(&mut s, p[0], 48.8600, 2.3300);
    place(&mut s, p[1], 48.8584, 2.2945);
    place(&mut s, p[2], 40.7484, -73.9857);
    for id in &p {
        s.execute("photo.setMeta", &json!({"ids": [id.0], "city": "Somewhere"})).unwrap();
    }
    let o = ExportOptions::from_json(&json!({"format": "jpeg", "width": 64, "height": 64}));
    // before: every photo keeps its location
    assert!(exported_gps(&mut s, p[0], &o).0.is_some());
    add(&mut s, json!({"name": "Home", "lat": 48.8601, "lon": 2.3302, "radius": 200, "private": true}));
    add(&mut s, json!({"name": "Park", "lat": 48.8584, "lon": 2.2945, "radius": 500}));
    let (gps, city) = exported_gps(&mut s, p[0], &o);
    assert!(gps.is_none() && city.is_none(), "inside the private location: no location at all ({gps:?}, {city:?})");
    let (gps, city) = exported_gps(&mut s, p[1], &o);
    let g = gps.expect("a public location changes nothing");
    assert!((g.latitude - 48.8584).abs() < 1e-4 && city.as_deref() == Some("Somewhere"));
    assert!(exported_gps(&mut s, p[2], &o).0.is_some(), "outside every location");
    // the photo keeps its position in the library
    assert!(s.catalog.photo(p[0]).unwrap().meta.gps.is_some());
    // made public again, the location is exported again
    s.execute("map.location.update", &json!({"id": 1, "private": false})).unwrap();
    assert!(exported_gps(&mut s, p[0], &o).0.is_some());
    s.execute("map.location.update", &json!({"id": 1, "private": true})).unwrap();
    assert!(exported_gps(&mut s, p[0], &o).0.is_none());
    s.execute("map.location.remove", &json!({"id": 1})).unwrap();
    assert!(exported_gps(&mut s, p[0], &o).0.is_some());
}
