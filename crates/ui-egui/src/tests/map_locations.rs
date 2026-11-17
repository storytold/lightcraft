//! Saved locations on the Map view (LRC-MAP-LOCATIONS): the list with counts of the photos in
//! view, `+` at the map's centre or around the selected photos, Go To, the checkbox selecting a
//! location's photos, circles drawn on the map, the active one's centre and radius handles (and Lock
//! Markers keeping them), Delete… asking first, and the Info panel hiding the map of a photo inside a
//! private location until Show Map. The map is never online in tests.

use egui::Pos2;
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

/// Paris ×3, London ×2, New York ×1 on the demo library's first six photos.
const PLACES: [(f64, f64); 6] =
    [(48.8584, 2.2945), (48.8606, 2.3376), (48.8530, 2.3499), (51.5007, -0.1246), (51.5081, -0.0759), (40.7484, -73.9857)];

fn demo(size: [f32; 2]) -> (Headless, Vec<u64>) {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }), size, 1.0);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(6).map(|p| p.0).collect();
    for (id, (lat, lon)) in ids.iter().zip(PLACES) {
        h.app.run("photo.setGps", json!({"ids": [id], "lat": lat, "lon": lon})).unwrap();
    }
    h.app.run("view.map", json!({})).unwrap();
    h.app.run("map.setView", json!({"lat": 48.857, "lon": 2.32, "zoom": 12})).unwrap();
    frames(&mut h, 4);
    (h, ids)
}

fn frames(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn state(h: &mut Headless) -> Value {
    h.app.run("map.state", json!({})).unwrap()
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn click(h: &mut Headless, at: Pos2, button: egui::PointerButton) {
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    frames(h, 2);
}

fn drag(h: &mut Headless, a: Pos2, b: Pos2) {
    h.app.synthetic.push(egui::Event::PointerMoved(a));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: a,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    for i in 1..=10 {
        h.app.synthetic.push(egui::Event::PointerMoved(a + (b - a) * (i as f32 / 10.0)));
        h.step();
    }
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: b,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    frames(h, 2);
}

fn location(h: &mut Headless, id: u64) -> Value {
    h.app.session.execute("map.locations", &json!({})).unwrap()["locations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

fn drawn(s: &Value, id: u64) -> Value {
    s["locations"].as_array().unwrap().iter().find(|l| l["id"] == id).cloned().unwrap_or_else(|| panic!("location {id} drawn: {s}"))
}

#[test]
fn the_list_makes_shows_and_selects_locations() {
    let (mut h, ids) = demo([1300.0, 1000.0]);
    // + with nothing placed selected: at the map's centre, then renamed in place
    h.app.run("library.select", json!({"ids": []})).unwrap();
    let plus = widget(&h, "map:location:new").expect("+ in the list").center();
    click(&mut h, plus, egui::PointerButton::Primary);
    let first = h.app.session.map.locations.list().first().cloned().expect("a location");
    assert!((first.lat - 48.857).abs() < 0.01 && (first.lon - 2.32).abs() < 0.01, "{first:?}");
    assert!(h.app.ui.map.renaming.is_some(), "its name is offered for editing");
    h.request("ui.text", json!({"text": "Eiffel"}), std::time::Duration::from_secs(20));
    h.request("ui.key", json!({"key": "Enter"}), std::time::Duration::from_secs(20));
    frames(&mut h, 3);
    // (the text replaces the selected default name, or follows it)
    let name = location(&mut h, first.id)["name"].as_str().unwrap().to_string();
    assert!(name.ends_with("Eiffel"), "{name}");
    // a location around the selected photos (Paris's three)
    h.app.run("library.select", json!({"ids": &ids[..3]})).unwrap();
    let r = h.app.run("map.location.new", json!({"name": "Paris"})).unwrap();
    let paris = r["id"].as_u64().unwrap();
    assert_eq!(r["photos"], 3, "{r}");
    frames(&mut h, 3);
    // the list shows it with the photos in view inside it; circles are drawn
    assert!(widget(&h, &format!("map:location:{paris}")).is_some());
    let s = state(&mut h);
    assert_eq!(drawn(&s, paris)["active"], true, "the new one is the active one");
    // Go To from another view of the world
    h.app.run("map.setView", json!({"lat": 0, "lon": 100, "zoom": 6})).unwrap();
    frames(&mut h, 2);
    let at = widget(&h, &format!("map:location:{paris}")).unwrap().center();
    click(&mut h, at, egui::PointerButton::Primary);
    let s = state(&mut h);
    assert!((s["lat"].as_f64().unwrap() - 48.857).abs() < 0.05 && (s["lon"].as_f64().unwrap() - 2.33).abs() < 0.05, "{s}");
    // the checkbox selects the location's photos in view, and clears them
    h.app.run("library.select", json!({"ids": [ids[5]]})).unwrap();
    frames(&mut h, 2);
    let cb = widget(&h, &format!("map:location:check:{paris}")).unwrap().center();
    click(&mut h, cb, egui::PointerButton::Primary);
    let mut sel: Vec<u64> = h.app.session.selection.ids.iter().map(|p| p.0).collect();
    sel.sort_unstable();
    let mut want = vec![ids[0], ids[1], ids[2], ids[5]];
    want.sort_unstable();
    assert_eq!(sel, want, "added to the selection");
    click(&mut h, cb, egui::PointerButton::Primary);
    assert_eq!(h.app.session.selection.ids.iter().map(|p| p.0).collect::<Vec<_>>(), vec![ids[5]]);
    // the commands
    h.app.run("map.location.select", json!({"id": paris})).unwrap();
    assert_eq!(h.app.session.selection.ids.len(), 3);
    assert!(h.app.run("map.location.show", json!({"id": 999})).is_err());
}

#[test]
fn the_active_locations_handles_move_it_unless_markers_are_locked() {
    let (mut h, _) = demo([1300.0, 1000.0]);
    let r = h.app.run("map.location.new", json!({"name": "Here"})).unwrap();
    let id = r["id"].as_u64().unwrap();
    h.app.run("library.select", json!({"ids": []})).unwrap();
    h.app.run("map.location.show", json!({"id": id})).unwrap();
    frames(&mut h, 3);
    let before = location(&mut h, id);
    let d = drawn(&state(&mut h), id);
    let (c, r_px) = (egui::pos2(d["x"].as_f64().unwrap() as f32, d["y"].as_f64().unwrap() as f32), d["radiusPx"].as_f64().unwrap() as f32);
    // the radius handle: twice as far
    drag(&mut h, egui::pos2(c.x + r_px, c.y), egui::pos2(c.x + 2.0 * r_px, c.y));
    let after = location(&mut h, id);
    let ratio = after["radius"].as_f64().unwrap() / before["radius"].as_f64().unwrap();
    assert!((ratio - 2.0).abs() < 0.1, "{before} → {after}");
    // the centre handle: moves it
    drag(&mut h, c, c + egui::vec2(80.0, 40.0));
    let moved = location(&mut h, id);
    assert!(
        moved["lat"].as_f64().unwrap() < before["lat"].as_f64().unwrap() && moved["lon"].as_f64().unwrap() > before["lon"].as_f64().unwrap(),
        "{moved}"
    );
    // Lock Markers keeps it: the drag pans instead
    h.app.run("map.lockMarkers", json!({"on": true})).unwrap();
    frames(&mut h, 2);
    let d = drawn(&state(&mut h), id);
    let c = egui::pos2(d["x"].as_f64().unwrap() as f32, d["y"].as_f64().unwrap() as f32);
    drag(&mut h, c, c + egui::vec2(60.0, 0.0));
    assert_eq!(location(&mut h, id)["lat"], moved["lat"]);
    assert_eq!(location(&mut h, id)["lon"], moved["lon"]);
}

#[test]
fn delete_asks_first_and_private_hides_the_info_panel_map() {
    let (mut h, ids) = demo([1400.0, 1800.0]);
    // nothing selected: at the map's centre (48.857, 2.32), a kilometre round
    h.app.run("library.select", json!({"ids": []})).unwrap();
    let r = h.app.run("map.location.new", json!({"name": "Home"})).unwrap();
    let id = r["id"].as_u64().unwrap();
    frames(&mut h, 3);
    // Private from the row's menu
    let row = widget(&h, &format!("map:location:{id}")).unwrap().center();
    click(&mut h, row, egui::PointerButton::Secondary);
    let private = widget(&h, "map:location:menu:private").expect("the row's menu").center();
    click(&mut h, private, egui::PointerButton::Primary);
    assert_eq!(location(&mut h, id)["private"], true);
    // a photo inside it: the Info panel shows no map until Show Map
    h.app.run("photo.setGps", json!({"ids": [ids[0]], "lat": 48.857, "lon": 2.32})).unwrap();
    h.app.run("library.select", json!({"ids": [ids[0]]})).unwrap();
    h.app.run("panel.info", json!({})).unwrap();
    frames(&mut h, 4);
    assert!(widget(&h, "info:miniMap").is_none(), "hidden inside a private location");
    let show = widget(&h, "button:showPrivateMap").expect("Show Map").center();
    click(&mut h, show, egui::PointerButton::Primary);
    frames(&mut h, 2);
    assert!(widget(&h, "info:miniMap").is_some(), "shown when asked");
    // moved into another private location, it asks again; back in the first, it doesn't
    h.app.run("map.location.add", json!({"name": "Cabin", "lat": 45.0, "lon": 6.0, "radius": 1000.0, "private": true})).unwrap();
    h.app.run("photo.setGps", json!({"ids": [ids[0]], "lat": 45.0, "lon": 6.0})).unwrap();
    frames(&mut h, 3);
    assert!(widget(&h, "info:miniMap").is_none(), "another private location: hidden again");
    h.app.run("photo.setGps", json!({"ids": [ids[0]], "lat": 48.857, "lon": 2.32})).unwrap();
    frames(&mut h, 3);
    assert!(widget(&h, "info:miniMap").is_some(), "the location already shown");
    // Delete… asks first; Delete removes it
    let row = widget(&h, &format!("map:location:{id}")).unwrap().center();
    click(&mut h, row, egui::PointerButton::Secondary);
    let del = widget(&h, "map:location:menu:delete").expect("Delete… in the menu").center();
    click(&mut h, del, egui::PointerButton::Primary);
    assert!(!location(&mut h, id).is_null(), "nothing deleted before the confirmation");
    let yes = widget(&h, "map:location:confirmDelete").expect("the confirmation").center();
    click(&mut h, yes, egui::PointerButton::Primary);
    assert!(location(&mut h, id).is_null());
}
