//! The Info panel's map of the active photo's place (LR-MAP-INFO): its pin and credit, the active
//! photo only with several selected, none without a position, a click opening the Map view there
//! from the same tiles (nothing fetched twice), no fetching once it is off screen, offline, a place
//! near the pole, and the photo changing. Tiles come from an in-process fake server, never the
//! network.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

/// Serves one flat 256 px PNG for every tile and records the URLs asked for.
struct FakeTiles {
    urls: Mutex<Vec<String>>,
    png: Vec<u8>,
}

impl lightcraft_engine::map::Transport for FakeTiles {
    fn get(
        &self,
        url: &str,
        _headers: &[(&'static str, String)],
        _max: usize,
        _cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<lightcraft_engine::map::HttpResponse, String> {
        self.urls.lock().unwrap().push(url.to_string());
        Ok(lightcraft_engine::map::HttpResponse { status: 200, headers: vec![], body: self.png.clone() })
    }
}

fn fake() -> Arc<FakeTiles> {
    use lightcraft_codecs::{EncodeImage, EncodeMeta, Samples, encode_png};
    let data: Vec<u8> = (0..256 * 256).flat_map(|_| [40u8, 160, 90, 255]).collect();
    let png = encode_png(&EncodeImage::new(256, 256, 4, Samples::U8(&data)), &EncodeMeta::default()).unwrap();
    Arc::new(FakeTiles { urls: Mutex::new(Vec::new()), png })
}

/// The demo library (in a window tall enough to show the Info panel down to its map) with its first photos in Paris, London and near the North Pole, the third
/// without a position, the Info panel open on the first, tiles from `tiles` (a source of its own,
/// so tests don't share textures).
fn demo(id: &str, tiles: Arc<FakeTiles>) -> (Headless, Vec<u64>) {
    use lightcraft_engine::map::{TileService, TileSource};
    let mut session = lightcraft_engine::Session::with_demo();
    let template = format!("https://{id}.tiles.test/{{z}}/{{x}}/{{y}}.png");
    session.execute("map.tileSource", &json!({"id": id, "urlTemplate": template, "attribution": "© Test tiles", "maxZoom": 19})).unwrap();
    let source = TileSource::custom(Some(id), &template, Some("© Test tiles"), Some(19)).unwrap();
    session.map.set_service(TileService::with_transport(source, None, Some(tiles)).with_activity(Some(session.activity.clone())));
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1400.0, 1800.0], 1.0);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(4).map(|p| p.0).collect();
    for (id, (lat, lon)) in [(ids[0], (48.8584, 2.2945)), (ids[1], (51.5007, -0.1246)), (ids[3], (89.5, 10.0))] {
        h.app.run("photo.setGps", json!({"ids": [id], "lat": lat, "lon": lon})).unwrap();
    }
    h.app.run("library.select", json!({"ids": [ids[0]]})).unwrap();
    h.app.run("panel.info", json!({})).unwrap();
    frames(&mut h, 4);
    (h, ids)
}

fn frames(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn mini(h: &mut Headless) -> Value {
    h.app.run("map.state", json!({})).unwrap()["miniMap"].clone()
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// Draw frames until the tile service has nothing left to do.
fn settle_tiles(h: &mut Headless) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        frames(h, 2);
        if h.app.session.map.service().is_some_and(|s| s.is_idle()) && mini(h)["tiles"]["missing"] == 0 {
            return;
        }
        assert!(std::time::Instant::now() < end, "the tiles never arrived: {}", mini(h));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn the_info_panel_shows_the_active_photos_place_with_its_pin_and_credit() {
    let (mut h, ids) = demo("info-pin", fake());
    let m = mini(&mut h);
    assert_eq!(m["photo"], ids[0], "{m}");
    assert!((m["lat"].as_f64().unwrap() - 48.8584).abs() < 1e-6 && (m["lon"].as_f64().unwrap() - 2.2945).abs() < 1e-6, "{m}");
    assert_eq!(m["zoom"], 14.0);
    let rect = widget(&h, "info:miniMap").expect("the map is in the Info panel");
    let pin = egui::pos2(m["pin"]["x"].as_f64().unwrap() as f32, m["pin"]["y"].as_f64().unwrap() as f32);
    assert!(pin.distance(rect.center()) < 1.0, "the photo's place is the map's centre: {pin:?} {rect:?}");
    assert!(widget(&h, "info:miniMapAttribution").is_some_and(|c| rect.contains(c.center())), "the credit is on the map");
    assert!(widget(&h, "button:showOnMap").is_some(), "Open in Browser keeps its widget id");
    settle_tiles(&mut h);
    assert!(mini(&mut h)["tiles"]["drawn"].as_u64().unwrap() > 0);

    // several selected: the active photo's place
    h.app.run("library.select", json!({"ids": [ids[0], ids[1]], "active": ids[1]})).unwrap();
    frames(&mut h, 3);
    assert_eq!(mini(&mut h)["photo"], ids[1]);
    // a photo without a position has no map
    h.app.run("library.select", json!({"ids": [ids[2]]})).unwrap();
    frames(&mut h, 3);
    assert!(mini(&mut h).is_null() && widget(&h, "info:miniMap").is_none());
    // near the pole the map stops short of the place: the pin stays on the map
    h.app.run("library.select", json!({"ids": [ids[3]]})).unwrap();
    frames(&mut h, 3);
    let m = mini(&mut h);
    let rect = widget(&h, "info:miniMap").expect("a map for the polar photo");
    let pin = egui::pos2(m["pin"]["x"].as_f64().unwrap() as f32, m["pin"]["y"].as_f64().unwrap() as f32);
    assert!(rect.shrink(7.0).contains(pin), "{pin:?} in {rect:?}");
}

/// A click opens the Map view on the place, from the tiles the small map already loaded: no tile
/// is asked for twice.
#[test]
fn a_click_opens_the_map_view_there_from_the_same_tiles() {
    let tiles = fake();
    let (mut h, ids) = demo("info-click", tiles.clone());
    settle_tiles(&mut h);
    let at = widget(&h, "info:miniMap").unwrap().center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    frames(&mut h, 3);
    assert_eq!(h.app.ui.view, crate::state::ViewMode::Map);
    let s = h.app.run("map.state", json!({})).unwrap();
    assert!((s["lat"].as_f64().unwrap() - 48.8584).abs() < 1e-6 && (s["lon"].as_f64().unwrap() - 2.2945).abs() < 1e-6, "{s}");
    assert_eq!(s["zoom"], 14.0);
    assert!(s["pins"].as_array().unwrap().iter().any(|p| p["ids"].as_array().unwrap().contains(&json!(ids[0]))), "{s}");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !h.app.session.map.service().is_some_and(|s| s.is_idle()) {
        assert!(std::time::Instant::now() < end);
        frames(&mut h, 2);
    }
    let urls = tiles.urls.lock().unwrap().clone();
    let mut unique = urls.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), urls.len(), "a tile the small map loaded isn't fetched again: {urls:?}");
}

/// Off screen (the Info panel closed, or the map below its fold) the small map asks for nothing, so
/// nothing is fetched for it.
#[test]
fn nothing_is_fetched_for_it_once_it_is_off_screen_and_offline_it_stays_grey() {
    let tiles = fake();
    let (mut h, _) = demo("info-off", tiles.clone());
    settle_tiles(&mut h);
    assert!(h.app.session.map.service().unwrap().stats().wanted > 0);
    h.app.run("panel.edit", json!({})).unwrap();
    frames(&mut h, 4);
    assert!(mini(&mut h).is_null());
    assert_eq!(h.app.session.map.service().unwrap().stats().wanted, 0, "no map on screen: nothing wanted");
    // in a shorter window the map is below the panel's fold: not drawn, nothing wanted
    h.app.run("panel.info", json!({})).unwrap();
    h.request("ui.resize", json!({"width": 1400, "height": 900}), std::time::Duration::from_secs(20));
    frames(&mut h, 4);
    assert!(mini(&mut h).is_null(), "scrolled out of view");
    assert_eq!(h.app.session.map.service().unwrap().stats().wanted, 0);
    h.request("ui.resize", json!({"width": 1400, "height": 1800}), std::time::Duration::from_secs(20));
    // offline: the grey map, the pin, and no service at all
    h.app.run("map.tileSource", json!({"offline": true})).unwrap();
    frames(&mut h, 3);
    let m = mini(&mut h);
    assert_eq!(m["tiles"]["offline"], true, "{m}");
    assert!(m["pin"]["x"].is_number());
    assert!(h.app.session.map.service().is_none());
}

#[test]
fn the_pin_stays_inside_maps_of_any_size() {
    let r = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(4.0, 3.0));
    for p in [egui::pos2(-1e9, 1e9), egui::pos2(f32::NAN, 0.0), egui::pos2(f32::INFINITY, 5.0), egui::pos2(12.0, 11.0)] {
        let q = crate::panels::map::keep_on_map(p, r);
        assert!(r.contains(q), "{p:?} → {q:?}");
    }
}
