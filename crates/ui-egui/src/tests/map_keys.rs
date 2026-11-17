//! The Map view's keys, menus and settings (map phase 8): H hides the pins, O the saved locations
//! list, `=` / `-` and Cmd+= / Cmd+- / Cmd+0 zoom and fit the map, `\` and Cmd+K as before — in the
//! Map view only (outside it the keys keep Remove, the mask overlay, the grid's thumbnail size and
//! Add Keywords); hidden pins can't be clicked; View ▸ Map gathers the map's commands (greyed
//! elsewhere); Settings ▸ Map picks the tile source (a custom server only on Apply or Return; a
//! refused one changes nothing) and clears the cache. The map is never online in tests.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> (Headless, Vec<u64>) {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1300.0, 1000.0],
        1.0,
    );
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    for (id, (lat, lon)) in ids.iter().zip([(48.8584, 2.2945), (51.5007, -0.1246), (40.7484, -73.9857)]) {
        h.app.run("photo.setGps", json!({"ids": [id], "lat": lat, "lon": lon})).unwrap();
    }
    h.app.run("view.map", json!({})).unwrap();
    h.app.run("map.setView", json!({"lat": 48.8584, "lon": 2.2945, "zoom": 10})).unwrap();
    frames(&mut h, 3);
    (h, ids)
}

fn frames(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn key(h: &mut Headless, k: &str, cmd: bool) {
    let r = h.request("ui.key", json!({"key": k, "cmd": cmd}), T);
    assert_eq!(r["ok"], true, "{k}: {r}");
    frames(h, 2);
}

fn state(h: &mut Headless) -> Value {
    h.app.run("map.state", json!({})).unwrap()
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn click_at(h: &mut Headless, at: egui::Pos2) {
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    frames(h, 2);
}

fn zoom(h: &mut Headless) -> f64 {
    state(h)["zoom"].as_f64().unwrap()
}

#[test]
fn the_map_keys_work_in_the_map_view_only() {
    let (mut h, ids) = demo();
    // H hides the pins, and hidden pins can't be clicked
    let pin = state(&mut h)["pins"].as_array().unwrap().iter().find(|p| p["ids"].as_array().unwrap().contains(&json!(ids[0]))).cloned().unwrap();
    let at = egui::pos2(pin["x"].as_f64().unwrap() as f32, pin["y"].as_f64().unwrap() as f32);
    key(&mut h, "H", false);
    let s = state(&mut h);
    assert_eq!((s["showPins"].as_bool(), s["pins"].as_array().map(Vec::len)), (Some(false), Some(0)), "{s}");
    h.app.run("library.select", json!({"ids": [ids[2]]})).unwrap();
    click_at(&mut h, at);
    assert_eq!(h.app.session.selection.ids.iter().map(|p| p.0).collect::<Vec<_>>(), vec![ids[2]], "a hidden pin isn't clicked");
    assert_eq!(serde_json::to_value(&h.app.ui).unwrap()["map"]["showPins"], false, "saved with the window state");
    key(&mut h, "H", false);
    assert_eq!(state(&mut h)["showPins"], true);
    assert_ne!(h.app.ui.right, crate::state::RightPanel::Remove, "H doesn't open Remove in the Map view");
    // O shows / hides the saved locations list (the navigator panel)
    let (open, overlay) = (h.app.ui.map.navigator_open, h.app.ui.mask_overlay);
    key(&mut h, "O", false);
    assert_eq!(h.app.ui.map.navigator_open, !open);
    assert_eq!(h.app.ui.mask_overlay, overlay, "O doesn't touch the mask overlay in the Map view");
    key(&mut h, "O", false);
    // = / - and Cmd+= / Cmd+- zoom; Cmd+0 fits
    let z = zoom(&mut h);
    key(&mut h, "=", false);
    assert_eq!(zoom(&mut h), z + 1.0);
    key(&mut h, "-", false);
    key(&mut h, "-", false);
    assert_eq!(zoom(&mut h), z - 1.0);
    key(&mut h, "=", true);
    assert_eq!(zoom(&mut h), z);
    let thumbs = h.app.ui.thumb_size;
    assert_eq!(h.app.ui.thumb_size, thumbs, "the grid's thumbnail size didn't change");
    key(&mut h, "0", true);
    assert!(zoom(&mut h) < z, "Cmd+0 fits every photo with a position (Paris, London, New York)");
    // \ and Cmd+K as before
    let bar = h.app.ui.map.show_filter_bar;
    key(&mut h, "\\", false);
    assert_eq!(h.app.ui.map.show_filter_bar, !bar);
    key(&mut h, "K", true);
    assert!(h.app.ui.map.locked);

    // outside the Map view the keys keep their meaning
    h.app.run("view.photoGrid", json!({})).unwrap();
    frames(&mut h, 2);
    let pins = h.app.ui.map.show_pins;
    let thumbs = h.app.ui.thumb_size;
    key(&mut h, "=", false);
    assert!(h.app.ui.thumb_size > thumbs, "= sizes the grid's thumbnails");
    key(&mut h, "H", false);
    assert_eq!(h.app.ui.map.show_pins, pins, "H leaves the pins alone");
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Remove, "H opens Remove (in Detail, as ever)");
}

#[test]
fn view_map_gathers_the_map_commands_greyed_outside_the_map_view() {
    let (mut h, _) = demo();
    let find = |h: &Headless| -> Vec<(String, bool, Option<bool>)> {
        let bar = crate::menubar::menu_bar(&h.app);
        let view = bar.iter().find(|(t, _)| t == "View").map(|(_, i)| i.clone()).unwrap();
        let map = view
            .iter()
            .find_map(|n| match n {
                crate::menubar::MenuNode::Submenu { label, children } if label == "Map" => Some(children.clone()),
                _ => None,
            })
            .expect("View ▸ Map");
        map.iter()
            .filter_map(|n| match n {
                crate::menubar::MenuNode::Item { id, enabled, checked, .. } => Some((id.clone(), *enabled, *checked)),
                _ => None,
            })
            .collect()
    };
    let items = find(&h);
    for id in ["map.fit", "map.zoomIn", "map.zoomOut", "map.showPins", "map.lockMarkers", "map.filterBar", "map.settings"] {
        assert!(items.iter().any(|(i, e, _)| i == id && *e), "{id} enabled in the Map view: {items:?}");
    }
    assert_eq!(items.iter().find(|(i, ..)| i == "map.showPins").map(|(_, _, c)| *c), Some(Some(true)));
    // run from the menu, as the keys do
    h.app.run("map.showPins", json!({})).unwrap();
    assert_eq!(find(&h).iter().find(|(i, ..)| i == "map.showPins").map(|(_, _, c)| *c), Some(Some(false)));
    h.app.run("map.settings", json!({})).unwrap();
    assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "map"));
    h.app.ui.dialog = None;
    // greyed outside the Map view (but Map Tiles… and Load Tracklog… stay)
    h.app.run("view.photoGrid", json!({})).unwrap();
    let items = find(&h);
    assert!(items.iter().any(|(i, e, _)| i == "map.fit" && !*e));
    assert!(items.iter().any(|(i, e, _)| i == "map.settings" && *e));
    assert!(h.app.run("map.showPins", json!({"on": "yes"})).is_err());
}

#[test]
fn settings_map_picks_the_source_applies_a_custom_server_only_on_confirm_and_clears_the_cache() {
    let (mut h, _) = demo();
    h.app.run("app.settings", json!({"tab": "map"})).unwrap();
    frames(&mut h, 3);
    // a built-in source by its radio
    let r = h.request("ui.clickWidget", json!({"id": "settings:mapSource:opentopomap"}), T);
    assert_eq!(r["ok"], true, "{r}");
    frames(&mut h, 2);
    assert_eq!(h.app.session.map.source().id(), "opentopomap");
    assert!(widget(&h, "settings:mapSource:esri-streets").is_none(), "no Esri styles without a key");
    // a custom server: typing changes nothing; Return with a bad template shows why and changes nothing
    h.request("ui.clickWidget", json!({"id": "field:mapTemplate"}), T);
    frames(&mut h, 1);
    h.request("ui.text", json!({"text": "http://tiles.example.org/{z}/{x}/{y}.png"}), T);
    frames(&mut h, 2);
    assert_eq!(h.app.session.map.source().id(), "opentopomap", "not while typing");
    h.request("ui.key", json!({"key": "Enter"}), T);
    frames(&mut h, 2);
    assert!(widget(&h, "settings:mapError").is_some(), "the refusal is shown");
    assert_eq!(h.app.session.map.source().id(), "opentopomap", "a refused server changes nothing");
    // a good one, with its credit, on Apply
    h.request("ui.clickWidget", json!({"id": "field:mapTemplate"}), T);
    frames(&mut h, 1);
    h.request("ui.key", json!({"key": "A", "cmd": true}), T);
    h.request("ui.text", json!({"text": "https://tiles.example.org/{z}/{x}/{y}.png"}), T);
    frames(&mut h, 1);
    h.request("ui.clickWidget", json!({"id": "field:mapAttribution"}), T);
    frames(&mut h, 1);
    h.request("ui.text", json!({"text": "© Example"}), T);
    frames(&mut h, 1);
    h.request("ui.clickWidget", json!({"id": "button:mapCustomApply"}), T);
    frames(&mut h, 2);
    let s = h.app.session.execute("map.tileSource", &json!({})).unwrap();
    assert_eq!((s["custom"].as_bool(), s["attribution"].as_str()), (Some(true), Some("© Example")), "{s}");
    assert!(widget(&h, "settings:mapError").is_none());
    // the cache: its size, and Clear Cache when it holds tiles
    assert!(widget(&h, "settings:mapCacheSize").is_some());
    // the Esri styles appear once a key is set
    h.app.session.execute("map.setEsriKey", &json!({"key": "AAPK0123456789abcdef_TEST-key.x"})).unwrap();
    frames(&mut h, 2);
    assert!(widget(&h, "settings:mapSource:esri-streets").is_some());
}
