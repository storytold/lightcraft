use crate::Session;
use serde_json::json;

#[test]
fn test_map_providers_command() {
    let mut s = Session::new();
    let res = s.execute("map.providers", &json!({})).unwrap();
    let arr = res.as_array().unwrap();
    assert_eq!(arr.len(), 5);
    let ids: Vec<&str> = arr.iter().filter_map(|v| v.get("id")?.as_str()).collect();
    assert!(ids.contains(&"osm"));
    assert!(ids.contains(&"esri-imagery"));
    assert!(ids.contains(&"esri-streets"));
    assert!(ids.contains(&"esri-topo"));
    assert!(ids.contains(&"esri-hybrid"));
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
