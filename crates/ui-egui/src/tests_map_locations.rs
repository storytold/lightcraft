//! Headless tests of saved locations on the Map view (LRC-MAP-LOCATIONS): the list in the
//! Library panel with its counts, a click that shows a location, the checkbox that selects its
//! photos, dragging a location's centre and radius (and Lock Markers), New Location from the
//! selection, a location deleted mid-drag, the Info panel's map hidden for a private location, and
//! circles across the antimeridian, around a pole, by the thousand and on a tiny map. Tiles come
//! from the fake server of `tests_map` or are off (never the network).

use lightcraft_geom::Point;
use lightcraft_map::{LatLon, Viewport};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::tests_map::{PLACES, SETTLE, T, demo, demo_sized, fake, frames, run, state, widget};

fn location(h: &Headless, id: u64) -> Option<lightcraft_engine::locations::SavedLocation> {
    h.app.session.map.locations.get(id).cloned()
}

fn drawn(s: &Value, id: u64) -> Value {
    s["locations"].as_array().and_then(|a| a.iter().find(|l| l["id"] == id).cloned()).unwrap_or_else(|| panic!("location {id} is drawn: {s}"))
}

fn xy(v: &Value) -> egui::Pos2 {
    egui::pos2(v["x"].as_f64().unwrap() as f32, v["y"].as_f64().unwrap() as f32)
}

/// The position the map shows at screen point `p`.
fn place_at(h: &mut Headless, p: egui::Pos2) -> LatLon {
    let s = state(h);
    let canvas = h.app.map.rect.expect("the map is drawn");
    let f = |k: &str| s[k].as_f64().unwrap();
    let vp = Viewport::new(LatLon { lat: f("lat"), lon: f("lon") }, f("zoom"), f("width"), f("height"));
    vp.to_lat_lon(Point::new(f64::from(p.x - canvas.min.x), f64::from(p.y - canvas.min.y)))
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    frames(h, 3);
}

fn drag(h: &mut Headless, a: egui::Pos2, b: egui::Pos2) {
    let r = h.request("ui.drag", json!({"x": a.x, "y": a.y, "toX": b.x, "toY": b.y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    frames(h, 12);
}

fn selection(h: &Headless) -> Vec<u64> {
    let mut v: Vec<u64> = h.app.session.selection.ids.iter().map(|p| p.0).collect();
    v.sort();
    v
}

/// Paris (public, the three Paris photos) and London (private, the two London photos).
fn two_locations(h: &mut Headless) -> (u64, u64) {
    let paris = run(h, "map.location.add", json!({"name": "Paris", "lat": 48.8566, "lon": 2.3222, "radius": 5000}));
    let london = run(h, "map.location.add", json!({"name": "London", "lat": 51.505, "lon": -0.10, "radius": 5000, "private": true}));
    (paris["id"].as_u64().unwrap(), london["id"].as_u64().unwrap())
}

#[test]
fn the_list_counts_each_locations_photos_and_the_map_draws_them() {
    let (mut h, ids) = demo(Some(fake(false)));
    let (paris, london) = two_locations(&mut h);
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 50.2, "lon": 1.1, "zoom": 6}));
    frames(&mut h, 4);
    // the list is over the map's top-left corner, with each location's photos in view
    let panel = widget(&h, "map:locations").expect("the Saved Locations list");
    let canvas = widget(&h, "map:canvas").expect("the map");
    assert!(canvas.contains_rect(panel), "{panel:?} in {canvas:?}");
    for id in [paris, london] {
        let row = widget(&h, &format!("map:location:{id}")).expect("a row per location");
        assert!(panel.contains_rect(row));
        assert!(row.contains_rect(widget(&h, &format!("map:location:{id}:count")).expect("its count")));
    }
    let text = h.painted_text().join("\n");
    assert!(text.contains("Saved Locations (2)") && text.contains("Paris") && text.contains("London · private"), "{text}");
    let s = state(&mut h);
    let (p, l) = (drawn(&s, paris), drawn(&s, london));
    assert_eq!((p["inView"].as_u64(), l["inView"].as_u64()), (Some(3), Some(2)), "{s}");
    assert_eq!((p["private"].as_bool(), l["private"].as_bool()), (Some(false), Some(true)));
    // both circles are on the map, at their place, a few points across at this zoom
    let canvas = h.app.map.rect.unwrap();
    for v in [&p, &l] {
        assert!(canvas.contains(xy(v)), "{v}");
        let r = v["radiusPx"].as_f64().unwrap();
        assert!((2.0..20.0).contains(&r), "{v}");
    }
    let at = place_at(&mut h, xy(&p));
    assert!((at.lat - 48.8566).abs() < 0.01 && (at.lon - 2.3222).abs() < 0.01, "{at:?}");
    // the counts are of the photos in view: a filter that hides London's photos takes them away
    let others: Vec<u64> = ids[..3].to_vec();
    run(
        &mut h,
        "library.filter",
        json!({"ruleSet": {"rules": [{"field": "gpsArea", "op": "inside", "value": {"south": 48.0, "west": 2.0, "north": 49.0, "east": 3.0}}]}}),
    );
    frames(&mut h, 3);
    let s = state(&mut h);
    assert_eq!((drawn(&s, paris)["inView"].as_u64(), drawn(&s, london)["inView"].as_u64()), (Some(3), Some(0)), "{others:?}");
    // while the library keeps them all (map.locations)
    let all = run(&mut h, "map.locations", json!({}));
    assert_eq!(all["locations"][1]["photos"], 2, "{all}");
}

#[test]
fn a_click_shows_the_location_and_the_checkbox_selects_its_photos() {
    let (mut h, ids) = demo(Some(fake(false)));
    let (paris, london) = two_locations(&mut h);
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 0.0, "lon": 100.0, "zoom": 3}));
    frames(&mut h, 3);
    click(&mut h, &format!("map:location:{london}"));
    let s = state(&mut h);
    assert!((s["lat"].as_f64().unwrap() - 51.505).abs() < 1e-6 && (s["lon"].as_f64().unwrap() + 0.10).abs() < 1e-6, "centred on it: {s}");
    assert_eq!(s["selectedLocation"], london);
    let z = s["zoom"].as_f64().unwrap();
    assert!((9.0..13.0).contains(&z), "the 5 km circle fills the map: {z}");
    // the whole circle is on the map, with its handles
    let canvas = h.app.map.rect.unwrap();
    let l = drawn(&s, london);
    let r = l["radiusPx"].as_f64().unwrap() as f32;
    assert!(r > 60.0 && r < canvas.width().min(canvas.height()) / 2.0, "{l}");
    assert!(widget(&h, &format!("map:location:{london}:radius")).is_some());
    // the checkbox selects the photos inside it (London's two), and again clears them
    click(&mut h, &format!("map:location:{paris}:check"));
    assert_eq!(selection(&h), {
        let mut v = ids[..3].to_vec();
        v.sort();
        v
    });
    let text = h.painted_text().join("\n");
    assert!(text.contains("Paris"), "{text}");
    click(&mut h, &format!("map:location:{paris}:check"));
    assert!(selection(&h).iter().all(|i| !ids[..3].contains(i)), "unticked: {:?}", selection(&h));
    // a click on the map away from it drops the location's selection; one on its centre takes it again
    let empty = canvas.min + egui::vec2(20.0, canvas.height() - 60.0);
    h.request("ui.click", json!({"x": empty.x, "y": empty.y}), T);
    frames(&mut h, 3);
    assert!(state(&mut h)["selectedLocation"].is_null());
    let centre = xy(&drawn(&state(&mut h), london));
    h.request("ui.click", json!({"x": centre.x, "y": centre.y}), T);
    frames(&mut h, 3);
    assert_eq!(state(&mut h)["selectedLocation"], london);
}

#[test]
fn dragging_the_centre_and_the_radius_changes_the_location_unless_markers_are_locked() {
    let (mut h, _) = demo(Some(fake(false)));
    let (paris, _) = two_locations(&mut h);
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 2);
    run(&mut h, "map.location.show", json!({"id": paris}));
    frames(&mut h, 3);
    // the centre
    let l = drawn(&state(&mut h), paris);
    let from = xy(&l);
    let to = from + egui::vec2(70.0, 45.0);
    let want = place_at(&mut h, to);
    drag(&mut h, from, to);
    let got = location(&h, paris).unwrap();
    assert!((got.lat - want.lat).abs() < 1e-4 && (got.lon - want.lon).abs() < 1e-4, "{got:?} vs {want:?}");
    assert_eq!(got.radius, 5000.0, "the radius stays");
    assert!(state(&mut h)["movingLocation"].is_null());
    // the radius: its handle, dragged outwards
    frames(&mut h, 2);
    let l = drawn(&state(&mut h), paris);
    let handle = egui::pos2(l["handle"]["x"].as_f64().unwrap() as f32, l["handle"]["y"].as_f64().unwrap() as f32);
    let to = handle + egui::vec2(60.0, 0.0);
    let edge = place_at(&mut h, to);
    let want = lightcraft_catalog::rules::distance_m(got.lat, got.lon, edge.lat, edge.lon);
    drag(&mut h, handle, to);
    let r = location(&h, paris).unwrap().radius;
    assert!((r - want).abs() / want < 0.02 && r > 5000.0, "{r} vs {want}");
    assert_eq!(location(&h, paris).unwrap().lat, got.lat, "the centre stays");
    // saved at once (a library setting, not an undo step)
    assert!(h.app.session.undo.iter().all(|u| !u.label.contains("Location")));
    // Lock Markers: the handles don't move, the drag pans the map
    run(&mut h, "map.lockMarkers", json!({"on": true}));
    frames(&mut h, 2);
    let before = location(&h, paris).unwrap();
    let centre_before = state(&mut h)["lon"].as_f64().unwrap();
    let l = drawn(&state(&mut h), paris);
    let handle = egui::pos2(l["handle"]["x"].as_f64().unwrap() as f32, l["handle"]["y"].as_f64().unwrap() as f32);
    drag(&mut h, handle, handle + egui::vec2(80.0, 0.0));
    assert_eq!(location(&h, paris).unwrap(), before);
    assert!(state(&mut h)["lon"].as_f64().unwrap() < centre_before, "the map panned");
}

#[test]
fn new_location_from_the_selection_holds_the_selected_photos() {
    let (mut h, ids) = demo(Some(fake(false)));
    run(&mut h, "view.map", json!({}));
    run(&mut h, "library.select", json!({"ids": [ids[0], ids[1], ids[2]]}));
    frames(&mut h, 3);
    click(&mut h, "icon:locationNew");
    assert!(widget(&h, "map:locationNew:selection").is_some(), "the + menu offers it");
    click(&mut h, "map:locationNew:selection");
    let mut dlg = h.app.ui.dialog.take().expect("a name prompt opens");
    match &mut dlg {
        crate::state::Dialog::TextPrompt { command, value, .. } => {
            assert_eq!(command, "map.location.add");
            *value = "  Paris trip ".into();
        }
        other => panic!("a name prompt opens, not {other:?}"),
    }
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg);
    let r = r.unwrap();
    assert_eq!((r["name"].as_str(), r["photos"].as_u64()), (Some("Paris trip"), Some(3)), "{r}");
    frames(&mut h, 3);
    assert!(widget(&h, &format!("map:location:{}", r["id"])).is_some(), "listed at once");
    // at the map's centre: the place shown, a radius that suits the zoom
    run(&mut h, "map.setView", json!({"lat": PLACES[5].0, "lon": PLACES[5].1, "zoom": 12}));
    frames(&mut h, 2);
    click(&mut h, "icon:locationNew");
    click(&mut h, "map:locationNew:centre");
    let mut dlg = h.app.ui.dialog.take().expect("a name prompt opens");
    if let crate::state::Dialog::TextPrompt { value, .. } = &mut dlg {
        *value = "Midtown".into();
    }
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).unwrap();
    assert!((r["lat"].as_f64().unwrap() - PLACES[5].0).abs() < 1e-6, "{r}");
    let radius = r["radius"].as_f64().unwrap();
    assert!((300.0..5000.0).contains(&radius), "{r}");
    assert_eq!(r["photos"], 1);
    // an empty name is refused, nothing made
    let n = h.app.session.map.locations.len();
    click(&mut h, "icon:locationNew");
    click(&mut h, "map:locationNew:centre");
    let dlg = h.app.ui.dialog.take().expect("a name prompt opens");
    assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_err());
    assert_eq!(h.app.session.map.locations.len(), n);
}

#[test]
fn deleting_a_location_while_it_is_dragged_ends_the_drag() {
    let (mut h, ids) = demo(None);
    let (paris, _) = two_locations(&mut h);
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 2);
    run(&mut h, "map.location.show", json!({"id": paris}));
    frames(&mut h, 3);
    let from = xy(&drawn(&state(&mut h), paris));
    let to = from + egui::vec2(50.0, 30.0);
    let mut events = vec![json!({"kind": "down", "x": from.x, "y": from.y})];
    for i in 1..=8 {
        let p = from + (to - from) * (i as f32 / 8.0);
        events.push(json!({"kind": "drag", "x": p.x, "y": p.y}));
    }
    assert_eq!(h.request("ui.pointer", json!({"screen": true, "events": events}), T)["ok"], true);
    frames(&mut h, 10);
    assert_eq!(state(&mut h)["movingLocation"]["id"], paris, "the drag is on");
    // deleted (by an agent, or the list's menu) mid-drag
    let gps_before: Vec<_> = ids.iter().map(|i| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(*i)).unwrap().meta.gps).collect();
    run(&mut h, "map.location.remove", json!({"id": paris}));
    frames(&mut h, 3);
    let s = state(&mut h);
    assert!(s["movingLocation"].is_null() && s["selectedLocation"].is_null(), "{s}");
    assert!(s["locations"].as_array().unwrap().iter().all(|l| l["id"] != paris));
    h.request("ui.pointer", json!({"screen": true, "events": [{"kind": "up", "x": to.x, "y": to.y}]}), T);
    frames(&mut h, 4);
    assert!(location(&h, paris).is_none(), "it doesn't come back on release");
    let gps_after: Vec<_> = ids.iter().map(|i| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(*i)).unwrap().meta.gps).collect();
    assert_eq!(gps_before, gps_after, "no photo moved");
    // the list's menu deletes after asking
    let (_, london) = (0, run(&mut h, "map.locations", json!({}))["locations"][0]["id"].as_u64().unwrap());
    frames(&mut h, 2);
    h.app.ui.dialog = Some(crate::state::Dialog::DeleteLocation { id: london, name: "London".into() });
    frames(&mut h, 2);
    assert!(h.painted_text().join("\n").contains("This can't be undone"));
    let dlg = h.app.ui.dialog.take().unwrap();
    crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).unwrap();
    assert!(h.app.session.map.locations.is_empty());
}

#[test]
fn the_info_panel_hides_the_map_of_a_photo_in_a_private_location_until_asked() {
    let (mut h, ids) = demo_sized(Some(fake(false)), [1400.0, 1700.0]);
    run(&mut h, "library.select", json!({"ids": [ids[3]]}));
    h.app.ui.right = crate::state::RightPanel::Info;
    let (_, london) = two_locations(&mut h);
    frames(&mut h, 4);
    let s = state(&mut h);
    assert!(s["miniMap"].is_null(), "London is private: {s}");
    assert_eq!(s["privateMiniMap"], json!({"photo": ids[3], "location": london}));
    assert!(widget(&h, "info:miniMap").is_none());
    assert!(h.painted_text().join("\n").contains("In the private location “London”"));
    // asked for: shown
    click(&mut h, "button:showPrivateMap");
    frames(&mut h, 2);
    assert_eq!(state(&mut h)["miniMap"]["photo"], ids[3]);
    // a photo in a public location (Paris) has its map as before
    run(&mut h, "library.select", json!({"ids": [ids[0]]}));
    frames(&mut h, 3);
    let s = state(&mut h);
    assert_eq!(s["miniMap"]["photo"], ids[0], "{s}");
    assert!(s["privateMiniMap"].is_null());
    // the other London photo is hidden again (the reveal was for one photo)
    run(&mut h, "library.select", json!({"ids": [ids[4]]}));
    frames(&mut h, 3);
    assert!(state(&mut h)["miniMap"].is_null());
    // and the first one too, once another photo was shown: Show Map is asked for again
    run(&mut h, "library.select", json!({"ids": [ids[3]]}));
    frames(&mut h, 3);
    let s = state(&mut h);
    assert!(s["miniMap"].is_null(), "{s}");
    assert_eq!(s["privateMiniMap"]["photo"], ids[3]);
}

#[test]
fn circles_across_the_antimeridian_around_a_pole_by_the_thousand_and_on_a_tiny_map_never_crash() {
    let (mut h, _) = demo(None);
    let fiji = run(&mut h, "map.location.add", json!({"name": "Fiji", "lat": -17.0, "lon": 180.0, "radius": 1000000}))["id"].as_u64().unwrap();
    let pole =
        run(&mut h, "map.location.add", json!({"name": "Pole", "lat": 89.0, "lon": 30.0, "radius": 900000, "private": true}))["id"].as_u64().unwrap();
    run(&mut h, "map.location.add", json!({"name": "South", "lat": -90.0, "lon": 0.0, "radius": 1}));
    run(&mut h, "map.location.add", json!({"name": "Tiny", "lat": 48.8584, "lon": 2.2945, "radius": 1}));
    for i in 0..1500u32 {
        let (lat, lon) = (f64::from(i % 170) - 85.0, f64::from((i * 7) % 360) - 180.0);
        run(&mut h, "map.location.add", json!({"name": format!("L{i}"), "lat": lat, "lon": lon, "radius": 1000.0 + f64::from(i) * 300.0}));
    }
    run(&mut h, "view.map", json!({}));
    for (lat, lon, zoom) in
        [(0.0, 0.0, 0.0), (-17.0, 179.0, 4.0), (-17.0, -179.0, 6.0), (84.0, 30.0, 3.0), (-85.0, 0.0, 1.0), (48.8584, 2.2945, 19.0)]
    {
        run(&mut h, "map.setView", json!({"lat": lat, "lon": lon, "zoom": zoom}));
        frames(&mut h, 2);
        let s = state(&mut h);
        assert!(s["locations"].as_array().is_some(), "{s}");
    }
    // Fiji across ±180°: drawn whole from either side
    run(&mut h, "map.setView", json!({"lat": -17.0, "lon": -179.0, "zoom": 4}));
    frames(&mut h, 2);
    let f = drawn(&state(&mut h), fiji);
    let canvas = h.app.map.rect.unwrap();
    assert!(canvas.contains(xy(&f)), "{f}");
    // the polar circle is a band along the map's top edge, listed and selectable
    run(&mut h, "map.setView", json!({"lat": 70.0, "lon": 30.0, "zoom": 2}));
    run(&mut h, "map.location.show", json!({"id": pole}));
    frames(&mut h, 3);
    assert_eq!(drawn(&state(&mut h), pole)["private"], true);
    // a window too small for much of a map
    h.request("ui.resize", json!({"width": 60, "height": 40}), T);
    frames(&mut h, 4);
    let s = state(&mut h);
    assert!(s["width"].as_f64().unwrap() >= 0.0, "{s}");
    let r = run(&mut h, "map.locations", json!({}));
    assert_eq!(r["count"], 1504);
}

/// The screenshot for the PR (`LIGHTCRAFT_TEST_SHOTS=<dir>`): two saved locations, one private,
/// with the list and its counts, over the fake tiles.
#[test]
fn screenshot_of_saved_locations() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") else { return };
    let png = |img: &lightcraft_raster::Rgba8| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let services = crate::Services {
        png: Some(Box::new(png)),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        ..Default::default()
    };
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.session.map.set_service(lightcraft_map::TileService::with_transport(
        lightcraft_map::TileSource::openstreetmap(),
        None,
        Some(fake(false) as std::sync::Arc<dyn lightcraft_map::Transport>),
    ));
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.settle(SETTLE);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(PLACES.len()).map(|p| p.0).collect();
    for (id, (lat, lon)) in ids.iter().zip(PLACES) {
        h.app.run("photo.setMeta", json!({"ids": [id], "gps": [lat, lon]})).unwrap();
    }
    let (paris, _) = two_locations(&mut h);
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 50.15, "lon": 1.1, "zoom": 7}));
    run(&mut h, "map.location.select", json!({"id": paris}));
    run(&mut h, "map.location.update", json!({"id": paris, "radius": 40000}));
    run(&mut h, "map.location.update", json!({"id": 2, "radius": 30000}));
    h.step_until(T, |h| h.app.session.map.service().is_some_and(|s| s.is_idle()));
    frames(&mut h, 6);
    let path = std::path::Path::new(&dir).join("map-locations.png");
    let r = h.request("ui.screenshot", json!({"path": path.to_string_lossy(), "headless": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    // circles around the north pole and across the antimeridian, zoomed out
    run(&mut h, "map.location.add", json!({"name": "Arctic", "lat": 88.0, "lon": 0.0, "radius": 900000, "private": true}));
    run(&mut h, "map.location.add", json!({"name": "Fiji", "lat": -17.0, "lon": 179.5, "radius": 800000}));
    run(&mut h, "map.setView", json!({"lat": 30.0, "lon": 170.0, "zoom": 2}));
    h.step_until(T, |h| h.app.session.map.service().is_some_and(|s| s.is_idle()));
    frames(&mut h, 6);
    let path = std::path::Path::new(&dir).join("map-locations-world.png");
    let r = h.request("ui.screenshot", json!({"path": path.to_string_lossy(), "headless": true}), T);
    assert_eq!(r["ok"], true, "{r}");
}
