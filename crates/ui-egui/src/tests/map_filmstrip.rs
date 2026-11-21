//! The Map view's filmstrip (LRC-MAP-VIEW, LRC-MAP-GEOTAG): it is drawn under the map and
//! toggled like Detail's, the bottom bar offers the toggle instead of the grid's thumbnail size,
//! photos dragged from it onto the map get the drop point's position in one undo step, and the
//! selection shows both ways (selected pins and clusters on the map, a pin click selecting in
//! the strip). The map is never online in tests: no tiles are fetched.

use egui::{Pos2, Rect};
use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

/// The demo library in the Map view at 1300 × 1000, the map centred on `lat, lon` at `zoom`.
fn map_at(lat: f64, lon: f64, zoom: f64) -> Headless {
    map_sized(lat, lon, zoom, [1300.0, 1000.0])
}

fn map_sized(lat: f64, lon: f64, zoom: f64, size: [f32; 2]) -> Headless {
    let session = lightcraft_engine::Session::with_demo();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), size, 1.0);
    h.app.run("view.map", json!({})).unwrap();
    h.app.ui.map.center_lat = lat;
    h.app.ui.map.center_lon = lon;
    h.app.ui.map.zoom = zoom;
    frames(&mut h, 6);
    h
}

fn frames(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn widget(h: &Headless, id: &str) -> Option<Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn state(h: &mut Headless) -> Value {
    h.app.run("map.state", json!({})).unwrap()
}

fn canvas(s: &Value) -> Rect {
    let c = &s["canvas"];
    let f = |k: &str| c[k].as_f64().unwrap_or_else(|| panic!("canvas.{k}: {s}")) as f32;
    Rect::from_min_size(egui::pos2(f("x"), f("y")), egui::vec2(f("width"), f("height")))
}

fn gps(h: &Headless, id: u64) -> Option<(f64, f64)> {
    h.app.session.catalog.photo(PhotoId(id)).and_then(|p| p.meta.gps)
}

fn set_gps(h: &mut Headless, ids: &[u64], lat: f64, lon: f64) {
    h.app.run("photo.setGps", json!({"ids": ids, "lat": lat, "lon": lon})).unwrap();
}

fn press(h: &mut Headless, at: Pos2, pressed: bool) {
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    h.step();
}

fn glide(h: &mut Headless, from: Pos2, to: Pos2) {
    for i in 1..=10 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 10.0)));
        h.step();
    }
}

fn selected(h: &Headless) -> Vec<u64> {
    let mut v: Vec<u64> = h.app.session.selection.ids.iter().map(|p| p.0).collect();
    v.sort_unstable();
    v
}

/// The pin or cluster holding `id` in `map.state`.
fn pin_of(s: &Value, id: u64) -> Value {
    s["pins"]
        .as_array()
        .and_then(|pins| pins.iter().find(|p| p["ids"].as_array().is_some_and(|ids| ids.iter().any(|i| i.as_u64() == Some(id)))))
        .cloned()
        .unwrap_or_else(|| panic!("{id} has a pin: {s}"))
}

fn xy(p: &Value) -> Pos2 {
    egui::pos2(p["x"].as_f64().unwrap_or(0.0) as f32, p["y"].as_f64().unwrap_or(0.0) as f32)
}

#[test]
fn the_map_view_draws_the_filmstrip_and_view_filmstrip_toggles_it() {
    let mut h = map_at(30.0, 0.0, 3.0);
    assert!(h.app.ui.filmstrip, "the filmstrip is on by default");
    let s = state(&mut h);
    assert_eq!(s["filmstrip"], true, "{s}");
    let strip = widget(&h, "filmstrip").expect("the filmstrip is drawn in the Map view");
    let map = canvas(&s);
    assert!(map.bottom() <= strip.top() + 0.5, "the map ends above the strip: {map:?} {strip:?}");
    let first = h.app.session.visible_cloned()[0].0;
    assert!(widget(&h, &format!("film:{first}")).is_some(), "its thumbnails are there");
    // the bottom bar: the Filmstrip toggle, not the grid's thumbnail size
    assert!(widget(&h, "icon:filmstrip").is_some(), "the bottom bar has the Filmstrip toggle");
    assert!(widget(&h, "slider:thumbSize").is_none(), "no thumbnail-size slider on the map");

    h.app.run("view.filmstrip", json!({})).unwrap();
    frames(&mut h, 3);
    let s = state(&mut h);
    assert_eq!(s["filmstrip"], false);
    assert!(widget(&h, "filmstrip").is_none(), "hidden");
    assert!(canvas(&s).bottom() > map.bottom() + 100.0, "the map takes the room back");
    // the toggle in the bottom bar brings it back
    let toggle = widget(&h, "icon:filmstrip").expect("toggle").center();
    press(&mut h, toggle, true);
    press(&mut h, toggle, false);
    frames(&mut h, 3);
    assert!(h.app.ui.filmstrip && widget(&h, "filmstrip").is_some(), "shown again");

    // the grid keeps its thumbnail size
    h.app.run("view.photoGrid", json!({})).unwrap();
    frames(&mut h, 3);
    assert!(widget(&h, "slider:thumbSize").is_some());
    // and map.state no longer reports the map's last frame
    let s = state(&mut h);
    assert!(s["canvas"].is_null() && s["shown"] == false && s["filmstrip"] == false, "{s}");
}

#[test]
fn dragging_filmstrip_photos_onto_the_map_sets_their_gps_in_one_undo_step() {
    let mut h = map_at(30.0, 0.0, 3.0);
    let v = h.app.session.visible_cloned();
    let (a, b) = (v[0].0, v[1].0);
    // b already has a position elsewhere: a drop overwrites it, as Lightroom does
    set_gps(&mut h, &[b], -33.9, 151.2);
    h.app.run("library.select", json!({"ids": [a, b]})).unwrap();
    frames(&mut h, 4);
    let steps = h.app.session.undo.len();
    let from = widget(&h, &format!("film:{a}")).expect("a's thumbnail").center();
    let map = canvas(&state(&mut h));
    let to = map.center() + egui::vec2(120.0, -80.0);
    press(&mut h, from, true);
    glide(&mut h, from, to);
    // while they are over the map: a pin with their count at the pointer, nothing changed yet
    let s = state(&mut h);
    assert_eq!(s["drop"]["count"], 2, "{s}");
    assert!((s["drop"]["x"].as_f64().unwrap_or(0.0) - f64::from(to.x)).abs() < 0.5, "{s}");
    let (lat, lon) = (s["drop"]["lat"].as_f64().unwrap(), s["drop"]["lon"].as_f64().unwrap());
    assert!((-85.06..=85.06).contains(&lat) && (-180.0..=180.0).contains(&lon), "{lat} {lon}");
    assert_eq!((gps(&h, a), gps(&h, b)), (None, Some((-33.9, 151.2))), "nothing changes before the drop");
    press(&mut h, to, false);
    frames(&mut h, 2);
    for id in [a, b] {
        let g = gps(&h, id).expect("placed");
        assert!((g.0 - lat).abs() < 1e-6 && (g.1 - lon).abs() < 1e-6, "{id}: {g:?} vs {lat} {lon}");
    }
    assert!(h.app.ui.dragging_photos.is_none(), "the drag ended");
    assert!(state(&mut h)["drop"].is_null());
    assert_eq!(h.app.session.undo.len(), steps + 1, "one undo step for both");
    assert_eq!(h.app.session.undo.last().map(|u| u.label.clone()).as_deref(), Some("Set GPS Coordinates"));
    // they are now one pin, where they were dropped
    let pin = pin_of(&state(&mut h), a);
    assert_eq!(pin["count"], 2, "{pin}");
    assert!(xy(&pin).distance(to) < 40.0, "{pin} vs {to:?}");
    h.app.run("edit.undo", json!({})).unwrap();
    assert_eq!((gps(&h, a), gps(&h, b)), (None, Some((-33.9, 151.2))), "undo puts both back");
}

/// Released over the map's toolbar (not the map) or carrying photos that are gone: nothing
/// changes or an error is shown, never a crash.
#[test]
fn a_drop_outside_the_map_or_of_missing_photos_changes_nothing() {
    let mut h = map_at(30.0, 0.0, 3.0);
    let a = h.app.session.visible_cloned()[0].0;
    let from = widget(&h, &format!("film:{a}")).expect("a's thumbnail").center();
    let map = canvas(&state(&mut h));
    let toolbar = egui::pos2(map.center().x, map.bottom() - 10.0);
    let steps = h.app.session.undo.len();
    press(&mut h, from, true);
    glide(&mut h, from, toolbar);
    assert!(state(&mut h)["drop"].is_null(), "no drop over the toolbar");
    press(&mut h, toolbar, false);
    frames(&mut h, 2);
    assert_eq!(gps(&h, a), None);
    assert_eq!(h.app.session.undo.len(), steps);
    assert!(h.app.ui.dragging_photos.is_none());

    // photos deleted while carried: the drop reports it
    let at = map.center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.app.ui.dragging_photos = Some(vec![987_654_321]);
    h.step();
    h.app.ui.dragging_photos = Some(vec![987_654_321]);
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    frames(&mut h, 2);
    assert!(h.app.ui.dragging_photos.is_none());
    assert_eq!(h.app.session.undo.len(), steps);
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("no such photo")), "{:?}", h.app.ui.toast);
}

#[test]
fn selected_photos_are_marked_on_the_map_and_a_pin_click_selects_them_in_the_filmstrip() {
    // narrow, so the strip shows only the first few photos
    let mut h = map_sized(48.0, 2.0, 5.0, [900.0, 900.0]);
    let v = h.app.session.visible_cloned();
    let n = v.len();
    assert!(n >= 12, "the demo library is long enough to scroll the strip: {n}");
    // the last three photos (off screen) close together, one alone further away
    let group = [v[n - 3].0, v[n - 2].0, v[n - 1].0];
    let alone = v[3].0;
    set_gps(&mut h, &group[..1], 48.8584, 2.2945);
    set_gps(&mut h, &group[1..2], 48.8606, 2.3376);
    set_gps(&mut h, &group[2..], 48.8530, 2.3499);
    set_gps(&mut h, &[alone], 45.76, 4.84);
    h.app.run("library.select", json!({"ids": [group[1]]})).unwrap();
    frames(&mut h, 4);
    let s = state(&mut h);
    let cluster = pin_of(&s, group[0]);
    assert_eq!((cluster["count"].clone(), cluster["selected"].clone()), (json!(3), json!(1)), "the cluster shows 1 of 3 selected: {s}");
    assert_eq!(pin_of(&s, alone)["selected"], 0, "{s}");
    let strip = widget(&h, "filmstrip").expect("strip");
    let off = |h: &Headless, id: u64| widget(h, &format!("film:{id}")).is_none_or(|r| !strip.contains(r.center()));
    // selecting in the strip marks the pin
    h.app.run("library.select", json!({"ids": [alone]})).unwrap();
    frames(&mut h, 4);
    assert!(off(&h, group[0]), "the group's thumbnails start off screen");
    let s = state(&mut h);
    assert_eq!(pin_of(&s, alone)["selected"], 1, "{s}");
    assert_eq!(pin_of(&s, group[0])["selected"], 0, "{s}");

    // a click on the cluster selects its photos; the strip scrolls to the active one
    let at = xy(&pin_of(&s, group[0]));
    press(&mut h, at, true);
    press(&mut h, at, false);
    frames(&mut h, 8);
    let mut want = group.to_vec();
    want.sort_unstable();
    assert_eq!(selected(&h), want, "the cluster's photos are selected");
    let active = h.app.session.selection.active.map(|p| p.0).expect("an active photo");
    assert!(group.contains(&active));
    assert!(!off(&h, active), "the filmstrip scrolled to the active photo");
    assert_eq!(pin_of(&state(&mut h), group[0])["selected"], 3);
    // a single pin's click selects just that photo
    let at = xy(&pin_of(&state(&mut h), alone));
    frames(&mut h, 30);
    press(&mut h, at, true);
    press(&mut h, at, false);
    frames(&mut h, 3);
    assert_eq!(selected(&h), vec![alone]);
}
