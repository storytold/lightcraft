//! Headless tests of geotagging on the Map view (LRC-MAP-GEOTAG): dropping filmstrip photos on
//! the map, moving a pin, Lock Markers, removing GPS from a pin's menu, and hostile drops. The map
//! is offline or served by the fake tile server of `tests_map` (never the network).

use lightcraft_geom::Point;
use lightcraft_map::{LatLon, MAX_LATITUDE, Viewport};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::tests_map::{SETTLE, T, clusters, demo, fake, frames, ids_of, run, state, widget};

fn gps(h: &Headless, id: u64) -> Option<(f64, f64)> {
    h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).and_then(|p| p.meta.gps)
}

/// The position the map shows at screen point (`x`, `y`), from `map.state`.
fn place_at(h: &mut Headless, x: f32, y: f32) -> LatLon {
    let s = state(h);
    let canvas = h.app.map.rect.expect("the map is drawn");
    let f = |k: &str| s[k].as_f64().unwrap();
    let vp = Viewport::new(LatLon { lat: f("lat"), lon: f("lon") }, f("zoom"), f("width"), f("height"));
    vp.to_lat_lon(Point::new(f64::from(x - canvas.min.x), f64::from(y - canvas.min.y)))
}

fn close(a: (f64, f64), b: LatLon) -> bool {
    (a.0 - b.lat).abs() < 1e-4 && (a.1 - b.lon).abs() < 1e-4
}

/// The Map view at `lat, lon, zoom`.
fn map_at(h: &mut Headless, lat: f64, lon: f64, zoom: f64) {
    run(h, "view.map", json!({}));
    run(h, "map.setView", json!({"lat": lat, "lon": lon, "zoom": zoom}));
    frames(h, 3);
}

/// Centre of the filmstrip thumbnail of `id`.
fn film(h: &mut Headless, id: u64) -> egui::Pos2 {
    frames(h, 2);
    widget(h, &format!("film:{id}")).unwrap_or_else(|| panic!("film:{id} is on screen")).center()
}

/// A drag from `a` to `b` with the button released at `b`, and the frames it takes.
fn drag(h: &mut Headless, a: egui::Pos2, b: egui::Pos2) {
    let r = h.request("ui.drag", json!({"x": a.x, "y": a.y, "toX": b.x, "toY": b.y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    frames(h, 18);
}

/// Press at `a` and move to `b` in screen points, keeping the button down.
fn hold(h: &mut Headless, a: egui::Pos2, b: egui::Pos2) {
    let mut events = vec![json!({"kind": "down", "x": a.x, "y": a.y})];
    for i in 1..=10 {
        let p = a + (b - a) * (i as f32 / 10.0);
        events.push(json!({"kind": "drag", "x": p.x, "y": p.y}));
    }
    let r = h.request("ui.pointer", json!({"screen": true, "events": events}), T);
    assert_eq!(r["ok"], true, "{r}");
    frames(h, 15);
}

fn release(h: &mut Headless, at: egui::Pos2) {
    h.request("ui.pointer", json!({"screen": true, "events": [{"kind": "up", "x": at.x, "y": at.y}]}), T);
    frames(h, 4);
}

/// The cluster holding `id` in `map.state`.
fn pin_of(s: &Value, id: u64) -> Value {
    clusters(s).into_iter().find(|c| ids_of(c).contains(&id)).unwrap_or_else(|| panic!("{id} has a pin: {s}"))
}

fn xy(c: &Value) -> egui::Pos2 {
    egui::pos2(c["x"].as_f64().unwrap() as f32, c["y"].as_f64().unwrap() as f32)
}

#[test]
fn dragging_a_filmstrip_photo_onto_the_map_sets_its_gps_in_one_undo_step() {
    let (mut h, _) = demo(Some(fake(false)));
    map_at(&mut h, 30.0, 0.0, 3.0);
    // the seventh photo has no position yet
    let id = h.app.session.visible_cloned()[6].0;
    assert_eq!(gps(&h, id), None);
    let from = film(&mut h, id);
    let canvas = h.app.map.rect.unwrap();
    let to = canvas.center() + egui::vec2(140.0, -90.0);
    // while the photo is over the map: a pin at the pointer, reported by map.state
    hold(&mut h, from, to);
    let s = state(&mut h);
    assert_eq!(s["drop"]["count"], 1, "{s}");
    assert!((s["drop"]["x"].as_f64().unwrap() - f64::from(to.x)).abs() < 0.5, "{s}");
    assert_eq!(gps(&h, id), None, "nothing changes before the drop");
    let want = place_at(&mut h, to.x, to.y);
    release(&mut h, to);
    let got = gps(&h, id).expect("the drop set a position");
    assert!(close(got, want), "{got:?} vs {want:?}");
    assert!(h.app.ui.dragging_photos.is_none(), "the drag ended");
    assert!(state(&mut h)["drop"].is_null());
    // the photo is now a pin on the map, where it was dropped
    frames(&mut h, 2);
    let pin = xy(&pin_of(&state(&mut h), id));
    assert!(pin.distance(to) < 1.0, "{pin:?} vs {to:?}");
    // one undo step takes it off again
    run(&mut h, "edit.undo", json!({}));
    assert_eq!(gps(&h, id), None);
}

#[test]
fn dropping_several_photos_moves_them_all_including_ones_with_gps_in_one_step() {
    let (mut h, ids) = demo(None);
    map_at(&mut h, 20.0, 60.0, 4.0);
    let visible = h.app.session.visible_cloned();
    let (a, b) = (visible[6].0, visible[7].0);
    // Paris (ids[0]) already has a position: Lightroom overwrites it, so do we
    run(&mut h, "library.select", json!({"ids": [a, b, ids[0]]}));
    let before = gps(&h, ids[0]);
    let to = h.app.map.rect.unwrap().center() + egui::vec2(-60.0, 40.0);
    let from = film(&mut h, b);
    let want = place_at(&mut h, to.x, to.y);
    let steps = h.app.session.undo.len();
    drag(&mut h, from, to);
    for id in [a, b, ids[0]] {
        assert!(gps(&h, id).is_some_and(|g| close(g, want)), "{id}: {:?} vs {want:?}", gps(&h, id));
    }
    assert_eq!(h.app.session.undo.len(), steps + 1, "one undo step for the batch");
    assert_eq!(gps(&h, ids[1]), Some(super::tests_map::PLACES[1]), "photos not dragged stay");
    run(&mut h, "edit.undo", json!({}));
    assert_eq!((gps(&h, a), gps(&h, b), gps(&h, ids[0])), (None, None, before));
}

#[test]
fn a_single_pin_moves_a_cluster_pans_and_locked_markers_stay() {
    let (mut h, ids) = demo(None);
    // Paris at street zoom: one pin per photo
    map_at(&mut h, 48.857, 2.32, 14.0);
    let s = state(&mut h);
    let pin = pin_of(&s, ids[0]);
    assert_eq!(pin["count"], 1, "{s}");
    let from = xy(&pin);
    let to = from + egui::vec2(90.0, 50.0);
    let want = place_at(&mut h, to.x, to.y);
    let (others, view) = ((gps(&h, ids[1]), gps(&h, ids[2])), (s["lat"].clone(), s["lon"].clone()));
    hold(&mut h, from, to);
    let s = state(&mut h);
    assert_eq!(s["movingPin"]["id"], ids[0], "{s}");
    assert_eq!((s["lat"].clone(), s["lon"].clone()), view, "moving a pin doesn't pan");
    release(&mut h, to);
    let got = gps(&h, ids[0]).unwrap();
    assert!(close(got, want), "{got:?} vs {want:?}");
    assert_eq!((gps(&h, ids[1]), gps(&h, ids[2])), others, "only that photo moved");
    let s = state(&mut h);
    assert!(s["movingPin"].is_null());
    assert_eq!((s["lat"].clone(), s["lon"].clone()), view);
    run(&mut h, "edit.undo", json!({}));
    assert_eq!(gps(&h, ids[0]), Some(super::tests_map::PLACES[0]));

    // a cluster (Paris's three photos at zoom 6) pans the map instead
    map_at(&mut h, 50.0, 1.0, 6.0);
    let s = state(&mut h);
    let paris = clusters(&s).into_iter().find(|c| c["count"] == 3).unwrap_or_else(|| panic!("{s}"));
    let before: Vec<_> = ids[0..3].iter().map(|i| gps(&h, *i)).collect();
    drag(&mut h, xy(&paris), xy(&paris) + egui::vec2(150.0, 0.0));
    assert_eq!(ids[0..3].iter().map(|i| gps(&h, *i)).collect::<Vec<_>>(), before, "a cluster's photos stay");
    assert!(state(&mut h)["lon"].as_f64().unwrap() < 1.0, "the map panned west");

    // Lock Markers: a single pin's drag pans too; the toolbar button toggles it back
    let r = run(&mut h, "map.lockMarkers", json!({"on": true}));
    assert_eq!(r["on"], true);
    assert_eq!(crate::menubar::checked(&h.app, "map.lockMarkers"), Some(true));
    map_at(&mut h, 48.857, 2.32, 14.0);
    let s = state(&mut h);
    assert_eq!(s["lockMarkers"], true);
    let from = xy(&pin_of(&s, ids[0]));
    drag(&mut h, from, from + egui::vec2(90.0, 50.0));
    assert_eq!(gps(&h, ids[0]), Some(super::tests_map::PLACES[0]), "a locked pin stays");
    assert!(state(&mut h)["lon"].as_f64().unwrap() < 2.32, "the drag panned");
    // saved with the UI state (ui.json)
    assert_eq!(serde_json::to_value(&h.app.ui).unwrap()["mapLockMarkers"], true);
    let button = widget(&h, "map:lockMarkers").expect("the toolbar has Lock Markers").center();
    h.request("ui.click", json!({"x": button.x, "y": button.y}), T);
    frames(&mut h, 3);
    assert!(!h.app.ui.map_lock_markers, "the button toggles it");
    for bad in [json!({"on": "yes"}), json!({"on": 1})] {
        let r = h.request("engine.execute", json!({"command": "map.lockMarkers", "params": bad}), T);
        assert_eq!(r["ok"], false, "{r}");
    }
}

#[test]
fn remove_gps_from_a_pins_menu_and_the_photo_menu_and_undo() {
    let (mut h, ids) = demo(None);
    map_at(&mut h, 40.7484, -73.9857, 10.0);
    let ny = xy(&pin_of(&state(&mut h), ids[5]));
    h.request("ui.click", json!({"x": ny.x, "y": ny.y, "button": "right"}), T);
    frames(&mut h, 4);
    let item = widget(&h, "map:pinMenu:removeGps").expect("the pin's menu offers Remove GPS Coordinates").center();
    h.request("ui.click", json!({"x": item.x, "y": item.y}), T);
    frames(&mut h, 4);
    assert_eq!(gps(&h, ids[5]), None);
    assert_eq!(h.app.session.undo.last().map(|u| u.label.clone()).as_deref(), Some("Remove GPS Coordinates"));
    run(&mut h, "edit.undo", json!({}));
    assert_eq!(gps(&h, ids[5]), Some(super::tests_map::PLACES[5]));
    // Photo ▸ Remove GPS Coordinates acts on the selection
    run(&mut h, "library.select", json!({"ids": [ids[3], ids[4]]}));
    let r = h.request("ui.menu.invoke", json!({"command": "photo.removeGps"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!((gps(&h, ids[3]), gps(&h, ids[4])), (None, None));
    let tree = h.request("ui.menu.tree", json!({}), T).to_string();
    assert!(tree.contains("photo.removeGps"), "in the Photo menu");
    // a right-click on the empty map opens no menu
    frames(&mut h, 30);
    h.request("ui.click", json!({"x": 300.0, "y": 600.0, "button": "right"}), T);
    frames(&mut h, 4);
    assert!(widget(&h, "map:pinMenu:removeGps").is_none());
}

#[test]
fn hostile_drops_never_crash() {
    let (mut h, ids) = demo(Some(fake(false)));
    // the whole world at zoom 0: above the top of the map the latitude clamps to the projection's edge
    map_at(&mut h, 0.0, 0.0, 0.0);
    let canvas = h.app.map.rect.unwrap();
    let id = h.app.session.visible_cloned()[6].0;
    let from = film(&mut h, id);
    drag(&mut h, from, egui::pos2(canvas.center().x + 500.0, canvas.top() + 2.0));
    let (lat, lon) = gps(&h, id).expect("set");
    assert!(lat <= MAX_LATITUDE + 1e-9 && lat > 80.0 && (-180.0..=180.0).contains(&lon), "{lat} {lon}");
    // a pin dropped outside the map (on the filmstrip) changes nothing
    map_at(&mut h, 48.857, 2.32, 14.0);
    let from = xy(&pin_of(&state(&mut h), ids[1]));
    drag(&mut h, from, egui::pos2(from.x, canvas.bottom() + 40.0));
    assert_eq!(gps(&h, ids[1]), Some(super::tests_map::PLACES[1]));
    // photos deleted from the library while dragged: an error, not a crash
    let gone = 987_654_321u64;
    let at = canvas.center();
    h.app.ui.dragging_photos = Some(vec![gone]);
    hold(&mut h, at, at + egui::vec2(10.0, 10.0));
    h.app.ui.dragging_photos = Some(vec![gone]);
    release(&mut h, at + egui::vec2(10.0, 10.0));
    assert!(h.app.ui.dragging_photos.is_none());
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("no such photo")), "{:?}", h.app.ui.toast);
    // a tiny window: nothing to drop on, nothing breaks
    h.request("ui.resize", json!({"width": 60, "height": 40}), T);
    frames(&mut h, 3);
    h.app.ui.dragging_photos = Some(vec![ids[0]]);
    h.request("ui.drag", json!({"x": 10.0, "y": 10.0, "toX": 30.0, "toY": 20.0, "steps": 4}), T);
    frames(&mut h, 10);
    h.request("ui.resize", json!({"width": 1400, "height": 900}), T);
    frames(&mut h, 2);
    // the command refuses hostile values
    for bad in
        [json!({"ids": [ids[0]], "lat": 95.0, "lon": 0.0}), json!({"ids": [ids[0]], "lat": "x", "lon": 0.0}), json!({"ids": [ids[0]], "lon": 0.0})]
    {
        let r = h.request("engine.execute", json!({"command": "photo.setGps", "params": bad}), T);
        assert_eq!(r["ok"], false, "{r}");
    }
    assert!(h.settle(SETTLE));
}

#[test]
fn a_floating_panel_over_the_map_refuses_the_drop_like_catalog_targets() {
    let (mut h, _) = demo(None);
    map_at(&mut h, 30.0, 0.0, 3.0);
    let id = h.app.session.visible_cloned()[6].0;
    let canvas = h.app.map.rect.expect("the map is drawn in the docked canvas");
    let to = canvas.center();
    let steps = h.app.session.undo.len();
    // the Tools panel floats over the middle of the map: dropping there does nothing
    h.app.run("ui.dock", json!({"operation": "float", "panel": "tools", "rect": [to.x - 150.0, to.y - 200.0, 330, 400]})).unwrap();
    frames(&mut h, 4);
    let from = film(&mut h, id);
    hold(&mut h, from, to);
    assert!(h.app.ui.dragging_photos.is_some(), "the filmstrip drag is under way");
    assert!(state(&mut h)["drop"].is_null(), "no drop pin under the floating panel");
    release(&mut h, to);
    assert_eq!(gps(&h, id), None, "the covered map takes no drop");
    assert_eq!(h.app.session.undo.len(), steps);
    // moved off the map's middle, the same drop sets the position
    h.app
        .run("ui.dock", json!({"operation": "moveFloating", "panel": "tools", "rect": [canvas.left() + 5.0, canvas.top() + 5.0, 200, 200]}))
        .unwrap();
    frames(&mut h, 4);
    let want = place_at(&mut h, to.x, to.y);
    let from = film(&mut h, id);
    hold(&mut h, from, to);
    assert_eq!(state(&mut h)["drop"]["count"], 1, "the uncovered map shows the drop pin");
    release(&mut h, to);
    assert!(gps(&h, id).is_some_and(|g| close(g, want)), "{:?} vs {want:?}", gps(&h, id));
    assert_eq!(h.app.session.undo.len(), steps + 1);
}
