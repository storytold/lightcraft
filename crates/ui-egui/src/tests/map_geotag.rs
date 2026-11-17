//! The Map view's pins (LRC-MAP-GEOTAG and the map's Phase 2): moving a single photo's pin, a
//! cluster's drag panning, Lock Markers (toolbar, `map.lockMarkers`, saved with the window state),
//! Remove GPS Coordinates from a pin's menu and the Photo menu, world-anchored clusters that keep
//! their groups while panning and split when zooming in, the view commands (`map.fit`,
//! `map.setView`, `map.zoomIn`, `map.zoomOut`), "N photos without GPS" and the hover thumbnail.
//! The map is never online in tests.

use egui::{Pos2, Rect};
use lightcraft_catalog::PhotoId;
use lightcraft_engine::map::Viewport;
use lightcraft_geom::{LatLon, Point};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

/// Paris ×3 (a few hundred metres apart), London ×2, New York ×1.
const PLACES: [(f64, f64); 6] =
    [(48.8584, 2.2945), (48.8606, 2.3376), (48.8530, 2.3499), (51.5007, -0.1246), (51.5081, -0.0759), (40.7484, -73.9857)];

/// The demo library in the Map view at 1300 × 1000, the first six photos at [`PLACES`].
fn demo() -> (Headless, Vec<u64>) {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1300.0, 1000.0],
        1.0,
    );
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(6).map(|p| p.0).collect();
    for (id, (lat, lon)) in ids.iter().zip(PLACES) {
        h.app.run("photo.setGps", json!({"ids": [id], "lat": lat, "lon": lon})).unwrap();
    }
    h.app.run("view.map", json!({})).unwrap();
    frames(&mut h, 4);
    (h, ids)
}

/// The photos of the list, in order.
fn ids_all(h: &mut Headless) -> Vec<u64> {
    h.app.session.visible_cloned().iter().map(|p| p.0).collect()
}

fn frames(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn view(h: &mut Headless, lat: f64, lon: f64, zoom: f64) {
    h.app.run("map.setView", json!({"lat": lat, "lon": lon, "zoom": zoom})).unwrap();
    frames(h, 3);
}

fn state(h: &mut Headless) -> Value {
    h.app.run("map.state", json!({})).unwrap()
}

fn widget(h: &Headless, id: &str) -> Option<Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn gps(h: &Headless, id: u64) -> Option<(f64, f64)> {
    h.app.session.catalog.photo(PhotoId(id)).and_then(|p| p.meta.gps)
}

fn button(h: &mut Headless, at: Pos2, b: egui::PointerButton, pressed: bool) {
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: b, pressed, modifiers: egui::Modifiers::NONE });
    h.step();
}

fn press(h: &mut Headless, at: Pos2, pressed: bool) {
    button(h, at, egui::PointerButton::Primary, pressed);
}

fn glide(h: &mut Headless, from: Pos2, to: Pos2) {
    for i in 1..=10 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 10.0)));
        h.step();
    }
}

/// A full drag from `a` to `b`, released there.
fn drag(h: &mut Headless, a: Pos2, b: Pos2) {
    press(h, a, true);
    glide(h, a, b);
    press(h, b, false);
    frames(h, 3);
}

fn pins(s: &Value) -> Vec<Value> {
    s["pins"].as_array().cloned().unwrap_or_default()
}

fn ids_of(p: &Value) -> Vec<u64> {
    let mut v: Vec<u64> = p["ids"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    v.sort_unstable();
    v
}

fn pin_of(s: &Value, id: u64) -> Value {
    pins(s).into_iter().find(|p| ids_of(p).contains(&id)).unwrap_or_else(|| panic!("{id} has a pin: {s}"))
}

fn xy(p: &Value) -> Pos2 {
    egui::pos2(p["x"].as_f64().unwrap_or(0.0) as f32, p["y"].as_f64().unwrap_or(0.0) as f32)
}

fn canvas(s: &Value) -> Rect {
    let c = &s["canvas"];
    let f = |k: &str| c[k].as_f64().unwrap_or_else(|| panic!("canvas.{k}: {s}")) as f32;
    Rect::from_min_size(egui::pos2(f("x"), f("y")), egui::vec2(f("width"), f("height")))
}

/// The position the map shows at screen point `at`.
fn place_at(s: &Value, at: Pos2) -> LatLon {
    let c = canvas(s);
    let f = |k: &str| s[k].as_f64().unwrap();
    let vp = Viewport::new(LatLon { lat: f("lat"), lon: f("lon") }, f("zoom"), f64::from(c.width()), f64::from(c.height()));
    vp.to_lat_lon(Point::new(f64::from(at.x - c.min.x), f64::from(at.y - c.min.y)))
}

fn close(a: (f64, f64), b: LatLon) -> bool {
    (a.0 - b.lat).abs() < 1e-4 && (a.1 - b.lon).abs() < 1e-4
}

#[test]
fn a_single_pin_moves_in_one_undo_step_a_cluster_pans_and_locked_markers_stay() {
    let (mut h, ids) = demo();
    // Paris at street zoom: one pin per photo
    view(&mut h, 48.857, 2.32, 14.0);
    let s = state(&mut h);
    let pin = pin_of(&s, ids[0]);
    assert_eq!(pin["count"], 1, "{s}");
    let from = xy(&pin);
    let to = from + egui::vec2(90.0, 50.0);
    let want = place_at(&s, to);
    let (others, before_view) = ((gps(&h, ids[1]), gps(&h, ids[2])), (s["lat"].clone(), s["lon"].clone()));
    let steps = h.app.session.undo.len();
    press(&mut h, from, true);
    glide(&mut h, from, to);
    let s = state(&mut h);
    assert_eq!(s["movingPin"]["id"], ids[0], "{s}");
    assert_eq!((s["lat"].clone(), s["lon"].clone()), before_view, "moving a pin doesn't pan");
    assert_eq!(gps(&h, ids[0]), Some(PLACES[0]), "nothing changes before the release");
    press(&mut h, to, false);
    frames(&mut h, 2);
    let got = gps(&h, ids[0]).expect("still placed");
    assert!(close(got, want), "{got:?} vs {want:?}");
    assert_eq!((gps(&h, ids[1]), gps(&h, ids[2])), others, "only that photo moved");
    assert_eq!(h.app.session.undo.len(), steps + 1, "one undo step");
    assert_eq!(h.app.session.undo.last().map(|u| u.label.clone()).as_deref(), Some("Set GPS Coordinates"));
    let s = state(&mut h);
    assert!(s["movingPin"].is_null());
    assert!(xy(&pin_of(&s, ids[0])).distance(to) < 2.0, "the pin is where it was let go");
    h.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(gps(&h, ids[0]), Some(PLACES[0]));

    // a pin let go outside the map (over the filmstrip) stays where it was
    let s = state(&mut h);
    let from = xy(&pin_of(&s, ids[1]));
    drag(&mut h, from, egui::pos2(from.x, canvas(&s).bottom() + 40.0));
    assert_eq!(gps(&h, ids[1]), Some(PLACES[1]));

    // a cluster (Paris's three photos at zoom 6) pans the map instead
    view(&mut h, 50.0, 1.0, 6.0);
    let s = state(&mut h);
    let paris = pins(&s).into_iter().find(|p| p["count"] == 3).unwrap_or_else(|| panic!("{s}"));
    let before: Vec<_> = ids[0..3].iter().map(|i| gps(&h, *i)).collect();
    drag(&mut h, xy(&paris), xy(&paris) + egui::vec2(150.0, 0.0));
    assert_eq!(ids[0..3].iter().map(|i| gps(&h, *i)).collect::<Vec<_>>(), before, "a cluster's photos stay");
    assert!(state(&mut h)["lon"].as_f64().unwrap() < 0.0, "the map panned west");

    // Lock Markers: a single pin's drag pans too
    let r = h.app.run("map.lockMarkers", json!({"on": true})).unwrap();
    assert_eq!(r["on"], true);
    assert_eq!(crate::menubar::checked(&h.app, "map.lockMarkers"), Some(true));
    view(&mut h, 48.857, 2.32, 14.0);
    let s = state(&mut h);
    assert_eq!(s["lockMarkers"], true);
    let from = xy(&pin_of(&s, ids[0]));
    drag(&mut h, from, from + egui::vec2(90.0, 50.0));
    assert_eq!(gps(&h, ids[0]), Some(PLACES[0]), "a locked pin stays");
    assert!(state(&mut h)["lon"].as_f64().unwrap() < 2.32, "the drag panned");
    // saved with the window state (ui.json)
    assert_eq!(serde_json::to_value(&h.app.ui).unwrap()["map"]["locked"], true);
    // the toolbar's Lock Markers toggles it back
    let b = widget(&h, "map:lockMarkers").expect("the toolbar has Lock Markers").center();
    // nothing covers it (the tile credit sits above the toolbar, not in it)
    assert!(!widget(&h, "map:attribution").unwrap().intersects(widget(&h, "map:lockMarkers").unwrap()));
    press(&mut h, b, true);
    press(&mut h, b, false);
    frames(&mut h, 2);
    assert!(!h.app.ui.map.locked, "the button toggles it");
    for bad in [json!({"on": "yes"}), json!({"on": 1})] {
        assert!(h.app.run("map.lockMarkers", bad.clone()).is_err(), "{bad}");
    }
    assert!(!h.app.ui.map.locked);
}

#[test]
fn remove_gps_from_a_pins_menu_and_the_photo_menu_in_one_undo_step() {
    let (mut h, ids) = demo();
    view(&mut h, 40.7484, -73.9857, 10.0);
    let ny = xy(&pin_of(&state(&mut h), ids[5]));
    button(&mut h, ny, egui::PointerButton::Secondary, true);
    button(&mut h, ny, egui::PointerButton::Secondary, false);
    frames(&mut h, 3);
    let item = widget(&h, "map:pinMenu:removeGps").expect("the pin's menu offers Remove GPS Coordinates").center();
    press(&mut h, item, true);
    press(&mut h, item, false);
    frames(&mut h, 3);
    assert_eq!(gps(&h, ids[5]), None);
    assert_eq!(h.app.session.undo.last().map(|u| u.label.clone()).as_deref(), Some("Remove GPS Coordinates"));
    h.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(gps(&h, ids[5]), Some(PLACES[5]));
    // Photo ▸ Remove GPS Coordinates acts on the selection
    let bar = crate::menubar::menu_bar(&h.app);
    let photo = bar.iter().find(|(title, _)| title == "Photo").map(|(_, items)| format!("{items:?}")).unwrap_or_default();
    assert!(photo.contains("photo.removeGps"), "in the Photo menu");
    h.app.run("library.select", json!({"ids": [ids[3], ids[4]]})).unwrap();
    h.app.run("photo.removeGps", json!({})).unwrap();
    assert_eq!((gps(&h, ids[3]), gps(&h, ids[4])), (None, None));
    // a right-click on the empty map opens no menu
    frames(&mut h, 30);
    let empty = canvas(&state(&mut h)).center() + egui::vec2(-300.0, 200.0);
    button(&mut h, empty, egui::PointerButton::Secondary, true);
    button(&mut h, empty, egui::PointerButton::Secondary, false);
    frames(&mut h, 3);
    assert!(widget(&h, "map:pinMenu:removeGps").is_none());
}

/// Clusters are world-anchored: panning moves them without regrouping; zooming in splits them.
#[test]
fn panning_keeps_the_groups_and_zooming_in_splits_them() {
    let (mut h, _) = demo();
    view(&mut h, 50.0, 1.0, 6.3);
    let a = state(&mut h);
    let groups = |s: &Value| {
        let mut g: Vec<Vec<u64>> = pins(s).iter().map(ids_of).collect();
        g.sort();
        g
    };
    let c = canvas(&a).center();
    drag(&mut h, c, c + egui::vec2(37.0, -21.0));
    let b = state(&mut h);
    assert_eq!(groups(&a), groups(&b), "panning never regroups the pins");
    // every pin moved by the same amount, the way the drag went
    let shifts: Vec<egui::Vec2> = pins(&a).iter().map(|p| xy(&pins(&b).into_iter().find(|q| ids_of(q) == ids_of(p)).unwrap()) - xy(p)).collect();
    let first = shifts[0];
    assert!(first.x > 10.0 && first.y < -5.0, "{first:?}");
    assert!(shifts.iter().all(|d| (*d - first).length() < 1.0), "{shifts:?}");
    // zooming in only splits: no fewer groups for the Paris photos at each whole zoom
    let mut last = 0;
    for z in [3.0, 6.0, 9.0, 12.0, 14.0] {
        view(&mut h, 48.857, 2.32, z);
        let n =
            pins(&state(&mut h)).iter().filter(|p| ids_of(p).iter().any(|i| h.app.session.visible_cloned()[..3].iter().any(|v| v.0 == *i))).count();
        assert!(n >= last, "zoom {z}: {n} < {last}");
        last = n;
    }
    assert_eq!(last, 3, "street level: one pin per Paris photo");
    let s = state(&mut h);
    assert!(s["clusterMs"].as_f64().is_some_and(|ms| ms >= 0.0), "{s}");
}

#[test]
fn view_commands_fit_set_and_zoom_and_refuse_hostile_values() {
    let (mut h, _) = demo();
    view(&mut h, 0.0, 100.0, 9.0);
    assert!(pins(&state(&mut h)).is_empty(), "nothing in the Indian Ocean");
    h.app.run("map.fit", json!({})).unwrap();
    frames(&mut h, 3);
    let s = state(&mut h);
    let c = canvas(&s);
    let all: Vec<u64> = pins(&s).iter().flat_map(ids_of).collect();
    assert_eq!(all.len(), 6, "Fit to Photos shows every photo with a position: {s}");
    // inside the map, clear of the filter bar at the top and the toolbar at the bottom
    let free = Rect::from_min_max(egui::pos2(c.left(), c.top() + 50.0), egui::pos2(c.right(), c.bottom() - 36.0));
    assert!(pins(&s).iter().all(|p| free.contains(xy(p))), "{s}");
    let z = s["zoom"].as_f64().unwrap();
    h.app.run("map.zoomIn", json!({})).unwrap();
    assert_eq!(state(&mut h)["zoom"].as_f64(), Some(z.round() + 1.0));
    h.app.run("map.zoomOut", json!({})).unwrap();
    h.app.run("map.zoomOut", json!({})).unwrap();
    assert_eq!(state(&mut h)["zoom"].as_f64(), Some(z.round() - 1.0));
    for _ in 0..30 {
        h.app.run("map.zoomOut", json!({})).unwrap();
    }
    assert_eq!(state(&mut h)["zoom"].as_f64(), Some(1.0), "never below zoom 1");
    // the longitude wraps, the latitude stays on the map
    h.app.run("map.setView", json!({"lat": 89.0, "lon": 190.0, "zoom": 4})).unwrap();
    let s = state(&mut h);
    assert!((s["lon"].as_f64().unwrap() + 170.0).abs() < 1e-9 && s["lat"].as_f64().unwrap() <= 85.06, "{s}");
    for bad in [json!({"lat": 91}), json!({"lat": "north"}), json!({"zoom": 99}), json!({"zoom": -1}), json!({"lon": [1]})] {
        assert!(h.app.run("map.setView", bad.clone()).is_err(), "{bad}");
    }
    assert_eq!(state(&mut h)["zoom"].as_f64(), Some(4.0), "a refusal changes nothing");
    // photos spread north to south: Fit keeps them clear of the filter bar and the toolbar
    let all = ids_all(&mut h);
    let (north, south) = (all[0], all[1]);
    h.app.run("photo.removeGps", json!({"ids": all[2..6].to_vec()})).unwrap();
    h.app.run("photo.setGps", json!({"ids": [north], "lat": 60.0, "lon": 10.0})).unwrap();
    h.app.run("photo.setGps", json!({"ids": [south], "lat": 40.0, "lon": 10.0})).unwrap();
    h.app.run("map.fit", json!({})).unwrap();
    frames(&mut h, 3);
    let s = state(&mut h);
    let c = canvas(&s);
    let free = Rect::from_min_max(egui::pos2(c.left(), c.top() + 50.0), egui::pos2(c.right(), c.bottom() - 36.0));
    assert_eq!(pins(&s).len(), 2, "{s}");
    for p in pins(&s) {
        // the pin's head is drawn up to 20 points above its point
        assert!(free.contains(xy(&p)) && free.contains(xy(&p) - egui::vec2(0.0, 20.0)), "{p} not in {free:?}");
    }
    for (id, (lat, lon)) in all[..6].iter().zip(PLACES) {
        h.app.run("photo.setGps", json!({"ids": [id], "lat": lat, "lon": lon})).unwrap();
    }
    // the toolbar's Fit to Photos does the same as map.fit
    let fit = widget(&h, "map:fit").expect("Fit to Photos").center();
    press(&mut h, fit, true);
    press(&mut h, fit, false);
    frames(&mut h, 3);
    assert_eq!(pins(&state(&mut h)).iter().flat_map(ids_of).count(), 6);
}

#[test]
fn the_first_showing_fits_the_photos_and_counts_those_without_gps() {
    let (mut h, ids) = demo();
    let s = state(&mut h);
    let all: Vec<u64> = pins(&s).iter().flat_map(ids_of).collect();
    assert_eq!(all.len(), ids.len(), "the first showing fits every photo with a position: {s}");
    let total = h.app.session.visible_cloned().len();
    assert_eq!((s["withGps"].as_u64(), s["withoutGps"].as_u64()), (Some(6), Some(total as u64 - 6)), "{s}");
    assert!(widget(&h, "map:withoutGps").is_some(), "the note is on the map");
    // hovering a pin shows its photo's thumbnail
    let p = xy(&pins(&s)[0]);
    h.app.synthetic.push(egui::Event::PointerMoved(p));
    frames(&mut h, 3);
    assert!(widget(&h, "map:hoverThumb").is_some(), "a thumbnail over the hovered pin");
    assert!(state(&mut h)["hovered"].is_array());
    // an empty library: no pins, no note, nothing breaks
    let mut e = Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), Services::default()), [900.0, 700.0], 1.0);
    e.app.run("view.map", json!({})).unwrap();
    frames(&mut e, 3);
    let s = state(&mut e);
    assert_eq!((s["withGps"].as_u64(), s["withoutGps"].as_u64(), pins(&s).len()), (Some(0), Some(0), 0));
    assert!(widget(&e, "map:withoutGps").is_none());
    e.app.run("map.fit", json!({})).unwrap();
}
