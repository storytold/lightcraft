//! Headless tests of the Map module against a fake tile server on 127.0.0.1 (no external
//! network): tiles load and draw with the attribution, pins appear for geotagged photos, a photo
//! dragged from the filmstrip onto the map is geotagged where it lands, a pin dragged moves its
//! photos, saved locations and the location filter work, and track logs load.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A tile server answering every request with a solid red 256² PNG (until the test ends).
fn fake_tiles() -> u16 {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(256, 256, image::Rgba([220, 30, 30, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let png = png.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\r\n", png.len()).as_bytes());
                let _ = s.write_all(&png);
            });
        }
    });
    port
}

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = DacApp::new(dac_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(SETTLE);
    h
}

fn run(h: &mut Headless, id: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    r["result"].clone()
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn map_on_fake_tiles() -> Headless {
    let port = fake_tiles();
    let mut h = demo();
    run(&mut h, "module.map", json!({}));
    run(
        &mut h,
        "map.addServer",
        json!({"name": "Fake", "url": format!("http://127.0.0.1:{port}/{{z}}/{{x}}/{{y}}.png"), "attribution": "© Fake Tiles"}),
    );
    run(&mut h, "map.view", json!({"lat": 48.85, "lon": 2.35, "zoom": 12}));
    h.settle(SETTLE);
    h
}

#[test]
fn tiles_draw_with_attribution_and_pins_cluster() {
    let mut h = map_on_fake_tiles();
    let img = h.snapshot(T);
    let canvas = widget(&h, "map:canvas").expect("the map canvas is on screen");
    assert!(widget(&h, "view:module:map").is_some() && widget(&h, "map:attribution").is_some() && widget(&h, "map:side").is_some());
    // a tile pixel (away from pins, buttons and the attribution) is the fake server's red
    let p = canvas.center() + egui::vec2(-150.0, 120.0);
    let c = img.pixels[p.y as usize * img.width() + p.x as usize];
    assert!(c.r() > 180 && c.g() < 80 && c.b() < 80, "tile pixel {c:?}");
    let v = run(&mut h, "map.view", json!({}));
    assert!(v["tiles"]["loaded"].as_u64().unwrap() > 0, "{v}");
    assert_eq!(v["attribution"], "© Fake Tiles");

    // two photos near each other and one far: two pins at zoom 4, three at zoom 16
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    run(&mut h, "map.geotag", json!({"ids": [ids[0], ids[1]], "lat": 48.8566, "lon": 2.3522}));
    run(&mut h, "map.geotag", json!({"ids": [ids[2]], "lat": 48.8600, "lon": 2.3400}));
    run(&mut h, "map.view", json!({"lat": 48.858, "lon": 2.346, "zoom": 15}));
    h.settle(SETTLE);
    let v = run(&mut h, "map.view", json!({}));
    assert_eq!(v["pins"].as_array().unwrap().len(), 2, "{v}");
    run(&mut h, "map.view", json!({"zoom": 6}));
    h.settle(SETTLE);
    let v = run(&mut h, "map.view", json!({}));
    let pins = v["pins"].as_array().unwrap();
    assert_eq!(pins.len(), 1, "{v}");
    assert_eq!(pins[0]["ids"].as_array().unwrap().len(), 3);
    // clicking the cluster selects its photos
    h.request("ui.clickWidget", json!({"id": format!("map:pin:{}", pins[0]["ids"][0])}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.session.selection.ids.len(), 3);
    // map.fit frames them
    let v = run(&mut h, "map.fit", json!({}));
    assert!(v["zoom"].as_f64().unwrap() > 10.0, "{v}");
}

#[test]
fn dragging_from_the_filmstrip_and_moving_pins_geotags() {
    let mut h = map_on_fake_tiles();
    let id = h.app.session.visible_cloned()[0].0;
    assert!(h.app.session.catalog.photo(dac_catalog::PhotoId(id)).unwrap().meta.gps.is_none());
    let canvas = widget(&h, "map:canvas").unwrap();
    assert!(widget(&h, &format!("film:{id}")).is_some(), "the filmstrip shows the photo");
    let to = canvas.center();
    let r = h.request("ui.dragWidget", json!({"id": format!("film:{id}"), "toX": to.x, "toY": to.y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let gps = h.app.session.catalog.photo(dac_catalog::PhotoId(id)).unwrap().meta.gps.expect("dropped on the map: geotagged");
    assert!((gps.0 - 48.85).abs() < 0.01 && (gps.1 - 2.35).abs() < 0.01, "{gps:?}");
    // drag its pin 100 px east: the photo moves with it
    let r = h.request("ui.dragWidget", json!({"id": format!("map:pin:{id}"), "dx": 100.0, "dy": 0.0, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let moved = h.app.session.catalog.photo(dac_catalog::PhotoId(id)).unwrap().meta.gps.unwrap();
    assert!(moved.1 > gps.1 + 0.005 && (moved.0 - gps.0).abs() < 0.005, "{gps:?} → {moved:?}");
    // undo puts it back
    run(&mut h, "edit.undo", json!({}));
    assert_eq!(h.app.session.catalog.photo(dac_catalog::PhotoId(id)).unwrap().meta.gps, Some(gps));
    // geotagAt (control channel) at a screen point
    run(&mut h, "map.geotagAt", json!({"ids": [id], "x": to.x, "y": to.y}));
}

#[test]
fn saved_locations_filter_and_tracks() {
    let mut h = map_on_fake_tiles();
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    run(&mut h, "map.geotag", json!({"ids": [ids[0]], "lat": 48.8566, "lon": 2.3522}));
    run(&mut h, "map.saveLocation", json!({"name": "Home", "lat": 48.8566, "lon": 2.3522, "radius": 300, "private": true}));
    run(&mut h, "map.filter", json!({"filter": "location:Home"}));
    h.settle(SETTLE);
    let v = run(&mut h, "map.view", json!({}));
    assert_eq!(v["filter"], "location:Home");
    assert_eq!(v["pins"].as_array().unwrap().len(), 1);
    let r = h.request("engine.execute", json!({"command": "map.filter", "params": {"filter": "location:Nowhere"}}), T);
    assert_eq!(r["ok"], false);
    run(&mut h, "map.filter", json!({"filter": "untagged"}));
    h.settle(SETTLE);
    assert!(run(&mut h, "map.view", json!({}))["pins"].as_array().unwrap().is_empty());
    run(&mut h, "map.filter", json!({"filter": "all"}));
    // search finds the saved location
    let r = run(&mut h, "map.search", json!({"query": "home"}));
    assert_eq!(r["found"], "savedLocation");
    // online search without consent is refused (nothing is sent)
    let r = h.request("engine.execute", json!({"command": "map.search", "params": {"query": "Paris"}}), T);
    assert_eq!(r["ok"], false, "{r}");
    // a GPX track loads, frames the view and draws
    let dir = std::env::temp_dir().join(format!("dac-map-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let gpx = dir.join("walk.gpx");
    std::fs::write(
        &gpx,
        r#"<gpx><trk><trkseg><trkpt lat="46.0" lon="7.0"><time>2026-05-01T10:00:00Z</time></trkpt>
        <trkpt lat="46.01" lon="7.02"><time>2026-05-01T10:10:00Z</time></trkpt></trkseg></trk></gpx>"#,
    )
    .unwrap();
    let r = run(&mut h, "map.loadTrack", json!({"path": gpx.to_string_lossy()}));
    assert_eq!((r["points"].as_u64(), r["timed"].as_bool()), (Some(2), Some(true)));
    let v = run(&mut h, "map.view", json!({}));
    assert!((v["lat"].as_f64().unwrap() - 46.005).abs() < 0.01, "{v}");
    h.settle(SETTLE);
    let _ = h.snapshot(T);
    // bad input: errors, not panics
    for (cmd, p) in [
        ("map.loadTrack", json!({"path": "/no/such.gpx"})),
        ("map.view", json!({"lat": 1000})),
        ("map.style", json!({"id": "nope"})),
        ("map.addServer", json!({"name": "x", "url": "ftp://x", "attribution": "a"})),
        ("map.geocoder", json!({"provider": "magic"})),
        ("map.lookup", json!({})),
    ] {
        let r = h.request("engine.execute", json!({"command": cmd, "params": p}), T);
        assert_eq!(r["ok"], false, "{cmd}: {r}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// P6.2: a damaged or hand-edited `map.json` (hostile numbers, bad tile servers, wrong types)
/// either fails to parse (defaults are used) or is sanitized into range; never a panic.
#[test]
fn hostile_map_json_is_sanitized() {
    use super::{MapPrefs, MapUi};
    let nums = ["0", "-0", "1e308", "-1e308", "90.0001", "-180.5", "1e-320", "22", "99999999999999999999", "-1"];
    let servers = [
        r#"[]"#,
        r#"[{"id":"x","name":"x","url":"file:///etc/passwd","attribution":"","maxZoom":19}]"#,
        r#"[{"id":"","name":"","url":"https://t/{z}/{x}/{y}.png","attribution":"","maxZoom":255}]"#,
        r#"[{"id":"osm","name":"dup","url":"https://t/{z}/{x}/{y}.png","attribution":"a","maxZoom":19,"kind":"satellite"}]"#,
        r#"{"not":"a list"}"#,
    ];
    let mut n = 0;
    for (i, lat) in nums.iter().enumerate() {
        for lon in nums.iter().step_by(3) {
            for zoom in nums.iter().skip(i % 3).step_by(2) {
                for (k, sv) in servers.iter().enumerate() {
                    let style = ["osm", "", "x", "\\u0000"][k % 4];
                    let text = format!(
                        r#"{{"lat":{lat},"lon":{lon},"zoom":{zoom},"style":"{style}","servers":{sv},"geocoder":"online","endpoint":"","onlineConsent":true,"cacheMb":{}}}"#,
                        nums[k]
                    );
                    let Ok(prefs) = serde_json::from_str::<MapPrefs>(&text) else { continue };
                    n += 1;
                    let mut m = MapUi { prefs, ..MapUi::default() };
                    m.sanitize();
                    let p = &m.prefs;
                    let max = f64::from(dac_geo::mercator::MAX_ZOOM);
                    assert!(dac_geo::LatLon::new(p.lat, p.lon).is_valid() && (1.0..=max).contains(&p.zoom), "{text}");
                    assert!(m.servers().iter().any(|s| s.id == p.style), "{text}");
                    assert!(p.servers.iter().all(|s| s.validate().is_ok()));
                    let _ = serde_json::to_string(p);
                }
            }
        }
    }
    assert!(n > 50, "only {n} variants parsed");
}
