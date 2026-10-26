//! Headless tests of the Map view: switching to it, fitting the photos with GPS, clusters and
//! selection, moving the view by commands and gestures, tile textures, the attribution, offline
//! mode and hostile positions. Tiles come from an in-process fake server (never the network).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use lightcraft_map::{HttpResponse, TileService, TileSource, Transport};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A tile server in the test: every tile is the same 256 px PNG (or garbage for `bad` columns).
struct FakeTiles {
    png: Vec<u8>,
    requests: AtomicUsize,
    /// Tiles whose x is odd come back as bytes that aren't an image.
    garbage_odd: bool,
}

impl Transport for FakeTiles {
    fn get(&self, url: &str, _: &[(&'static str, String)], _: usize, _: &AtomicBool) -> Result<HttpResponse, String> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let x: u32 = url.rsplit('/').nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        let body = if self.garbage_odd && x % 2 == 1 { b"<html>not a tile</html>".to_vec() } else { self.png.clone() };
        Ok(HttpResponse { status: 200, headers: vec![("cache-control".into(), "max-age=604800".into())], body })
    }
}

fn tile_png() -> Vec<u8> {
    let mut px = Vec::with_capacity(256 * 256 * 4);
    for y in 0..256u32 {
        for x in 0..256u32 {
            let line = x % 64 == 0 || y % 64 == 0;
            let v = if line { 150 } else { 205 };
            px.extend_from_slice(&[v, v + 10, v - 20, 255]);
        }
    }
    let img = lightcraft_raster::Rgba8::from_bytes(256, 256, &px).unwrap();
    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap()
}

fn fake(garbage_odd: bool) -> Arc<FakeTiles> {
    Arc::new(FakeTiles { png: tile_png(), requests: AtomicUsize::new(0), garbage_odd })
}

/// Paris ×3, London ×2, New York ×1.
const PLACES: [(f64, f64); 6] =
    [(48.8584, 2.2945), (48.8606, 2.3376), (48.8530, 2.3499), (51.5007, -0.1246), (51.5081, -0.0759), (40.7484, -73.9857)];

/// The demo library at 1400 × 900 with the fake tile server; the first six photos in view get
/// [`PLACES`] (returned in that order).
fn demo(tiles: Option<Arc<FakeTiles>>) -> (Headless, Vec<u64>) {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    if let Some(f) = tiles {
        app.session.map.set_service(TileService::with_transport(TileSource::openstreetmap(), None, Some(f as Arc<dyn Transport>)));
    } else {
        app.session.map.set_source(TileSource::offline());
    }
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(SETTLE);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(PLACES.len()).map(|p| p.0).collect();
    assert_eq!(ids.len(), PLACES.len(), "the demo library has enough photos");
    for (id, (lat, lon)) in ids.iter().zip(PLACES) {
        h.app.run("photo.setMeta", json!({"ids": [id], "gps": [lat, lon]})).unwrap();
    }
    (h, ids)
}

fn run(h: &mut Headless, id: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    r["result"].clone()
}

fn state(h: &mut Headless) -> Value {
    run(h, "map.state", json!({}))
}

fn frames(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn clusters(s: &Value) -> Vec<Value> {
    s["clusters"].as_array().cloned().unwrap_or_default()
}

fn ids_of(c: &Value) -> Vec<u64> {
    let mut v: Vec<u64> = c["ids"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
    v.sort();
    v
}

#[test]
fn the_map_view_fits_the_photos_with_gps_and_credits_the_tiles() {
    let (mut h, ids) = demo(Some(fake(false)));
    let total = h.app.session.visible_cloned().len();
    let r = run(&mut h, "view.map", json!({}));
    assert_eq!((r["withGps"].as_u64(), r["withoutGps"].as_u64()), (Some(6), Some(total as u64 - 6)));
    frames(&mut h, 2);
    assert_eq!(h.app.ui.view, ViewMode::Map);
    let s = state(&mut h);
    // the first open fits every photo with GPS: all of them on the canvas
    let canvas = h.app.map.rect.unwrap();
    let cs = clusters(&s);
    let mut all: Vec<u64> = cs.iter().flat_map(ids_of).collect();
    all.sort();
    all.dedup();
    let mut want = ids.clone();
    want.sort();
    assert_eq!(all, want, "{s}");
    for c in &cs {
        let (x, y) = (c["x"].as_f64().unwrap() as f32, c["y"].as_f64().unwrap() as f32);
        if ids_of(c).contains(&ids[5]) || ids_of(c).contains(&ids[0]) {
            assert!(canvas.contains(egui::pos2(x, y)), "{c} outside {canvas:?}");
        }
    }
    assert!(s["zoom"].as_f64().unwrap() >= 2.0 && s["zoom"].as_f64().unwrap() <= 5.0, "{s}");
    assert_eq!(s["zoom"].as_f64().unwrap().fract(), 0.0, "fits at a whole zoom (sharp tiles)");
    // the tile provider is credited on the map, visibly, and the photos without GPS are counted
    let text = h.painted_text().join("\n");
    assert!(text.contains("© OpenStreetMap contributors"), "{text}");
    assert!(text.contains(&format!("{} photos without GPS", total - 6)), "{text}");
    let w = h.request("ui.widgets", json!({}), T).to_string();
    for id in ["map:canvas", "map:attribution", "map:fit", "icon:mapZoomIn", "icon:mapZoomOut", "map:withoutGps"] {
        assert!(w.contains(&format!("\"{id}\"")), "{id}: {w}");
    }
    // the menu and the bottom bar know the view
    assert_eq!(crate::menubar::checked(&h.app, "view.map"), Some(true));
    assert!(w.contains("\"icon:map\""), "the bottom bar has the Map button: {w}");
    // the map draws in the docking canvas, beside (not under) the docked Tools panel
    let tools = h.app.widgets.iter().find(|(id, _)| id == "panel:right_panel").map(|(_, r)| *r).expect("Tools is docked by default");
    assert!(!canvas.intersect(tools).is_positive(), "map {canvas:?} overlaps Tools {tools:?}");
    // the view is kept for the session: leaving and coming back doesn't refit
    run(&mut h, "map.setView", json!({"lat": 10.0, "lon": 20.0, "zoom": 7}));
    run(&mut h, "view.photoGrid", json!({}));
    frames(&mut h, 2);
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 2);
    let s = state(&mut h);
    assert_eq!((s["lat"].as_f64(), s["lon"].as_f64(), s["zoom"].as_f64()), (Some(10.0), Some(20.0), Some(7.0)), "{s}");
}

#[test]
fn clicking_a_cluster_selects_exactly_its_photos_and_double_click_opens_detail() {
    let (mut h, ids) = demo(None);
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 50.0, "lon": 1.0, "zoom": 6}));
    frames(&mut h, 2);
    let s = state(&mut h);
    let cs = clusters(&s);
    let mut counts: Vec<u64> = cs.iter().map(|c| c["count"].as_u64().unwrap()).collect();
    counts.sort();
    assert_eq!(counts, vec![2, 3], "Paris and London apart, New York off screen: {s}");
    let paris = cs.iter().find(|c| c["count"] == 3).unwrap();
    let r = h.request("ui.click", json!({"x": paris["x"], "y": paris["y"]}), T);
    assert_eq!(r["ok"], true, "{r}");
    frames(&mut h, 2);
    let mut sel: Vec<u64> = h.app.session.selection.ids.iter().map(|p| p.0).collect();
    sel.sort();
    let mut want = ids[0..3].to_vec();
    want.sort();
    assert_eq!(sel, want, "exactly the cluster's photos");
    assert_eq!(h.app.ui.view, ViewMode::Map, "a click stays on the map");
    // the selected cluster is reported (and drawn) as selected
    let s = state(&mut h);
    assert!(clusters(&s).iter().any(|c| c["count"] == 3 && c["selected"] == true), "{s}");
    // clicking the map elsewhere leaves the selection alone
    h.request("ui.click", json!({"x": 300.0, "y": 600.0}), T);
    frames(&mut h, 2);
    assert_eq!(h.app.session.selection.ids.len(), 3);
    // a double-click on London selects its two photos and opens the first in Detail
    let london = clusters(&s).into_iter().find(|c| c["count"] == 2).unwrap();
    frames(&mut h, 40); // not a triple click with the clicks above
    h.request("ui.click", json!({"x": london["x"], "y": london["y"], "count": 2}), T);
    frames(&mut h, 6);
    let mut sel: Vec<u64> = h.app.session.selection.ids.iter().map(|p| p.0).collect();
    sel.sort();
    let mut want = ids[3..5].to_vec();
    want.sort();
    assert_eq!(sel, want);
    assert_eq!(h.app.ui.view, ViewMode::Detail);
    assert_eq!(h.app.session.active().map(|a| a.0), Some(ids[3]));
}

#[test]
fn zooming_splits_clusters_and_commands_and_gestures_move_the_view() {
    let (mut h, ids) = demo(None);
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 48.857, "lon": 2.32, "zoom": 9}));
    frames(&mut h, 2);
    let paris = |s: &Value| clusters(s).iter().filter(|c| ids_of(c).iter().any(|i| ids[0..3].contains(i))).count();
    assert_eq!(paris(&state(&mut h)), 1);
    let s = run(&mut h, "map.zoomIn", json!({}));
    assert_eq!(s["zoom"].as_f64(), Some(10.0));
    run(&mut h, "map.setView", json!({"zoom": 14}));
    frames(&mut h, 2);
    assert_eq!(paris(&state(&mut h)), 3, "zoomed in: one pin per photo");
    let s = run(&mut h, "map.zoomOut", json!({}));
    assert_eq!(s["zoom"].as_f64(), Some(13.0));
    // ⌘= / ⌘- / ⌘0 act on the map in this view
    let s = run(&mut h, "view.zoomIn", json!({}));
    assert_eq!(s["zoom"].as_f64(), Some(14.0));
    // bad parameters are refused and change nothing
    for bad in [json!({"lat": 95}), json!({"lon": -181}), json!({"zoom": 99}), json!({"zoom": -1}), json!({"lat": "north"})] {
        let r = h.request("engine.execute", json!({"command": "map.setView", "params": bad}), T);
        assert_eq!(r["ok"], false, "{bad}: {r}");
    }
    assert_eq!(state(&mut h)["zoom"].as_f64(), Some(14.0));
    // a drag pans: the content follows the pointer
    let before = state(&mut h);
    let canvas = h.app.map.rect.unwrap();
    let (cx, cy) = (canvas.center().x, canvas.center().y);
    h.request("ui.drag", json!({"x": cx, "y": cy, "toX": cx + 200.0, "toY": cy, "steps": 8}), T);
    frames(&mut h, 2);
    let after = state(&mut h);
    assert!(after["lon"].as_f64().unwrap() < before["lon"].as_f64().unwrap(), "dragging right shows what lies west: {before} → {after}");
    assert!((after["zoom"].as_f64().unwrap() - 14.0).abs() < 1e-9);
    // a pinch zooms around the pointer
    h.request("ui.move", json!({"x": cx, "y": cy}), T);
    h.step();
    h.request("ui.zoom", json!({"factor": 0.5}), T);
    frames(&mut h, 2);
    assert!((state(&mut h)["zoom"].as_f64().unwrap() - 13.0).abs() < 1e-6);
    // fit comes back to every photo
    let s = run(&mut h, "map.fit", json!({}));
    assert!(s["zoom"].as_f64().unwrap() <= 5.0, "{s}");
}

#[test]
fn tile_textures_are_uploaded_once_not_every_frame() {
    let tiles = fake(false);
    let (mut h, _) = demo(Some(tiles.clone()));
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 48.857, "lon": 2.32, "zoom": 5}));
    let idle = |h: &Headless| h.app.session.map.service().is_some_and(|s| s.is_idle());
    assert!(h.step_until(T, |h| h.app.map.tiles_drawn > 0 && h.app.map.tiles_missing == 0 && idle(h)), "every tile arrives from the fake server");
    frames(&mut h, 2);
    let uploads = h.app.map.uploads;
    assert!(uploads > 0 && uploads as usize <= h.app.map.textures() + 8, "{uploads}");
    let requests = tiles.requests.load(Ordering::SeqCst);
    frames(&mut h, 30);
    assert_eq!(h.app.map.uploads, uploads, "nothing is uploaded again while the view stands still");
    assert_eq!(tiles.requests.load(Ordering::SeqCst), requests, "nor fetched again");
    // the tiles are painted: the fake tile's colour is on screen
    let img = h.snapshot(SETTLE);
    let canvas = h.app.map.rect.unwrap();
    let px = img.pixels[(canvas.top() as usize + 200) * img.size[0] + canvas.left() as usize + 200];
    assert!(px.r() > 140 && px.g() > 150, "{px:?}");
    // a headless screenshot (its own egui context) paints the window's tile textures too, and
    // uploads nothing into its context (texture ids are per context)
    let main = h.view.ctx.clone();
    let shot = h.app.headless_screenshot(&main, true).expect("a capture");
    let scale = shot.size[0] as f32 / img.size[0] as f32;
    let at = |x: f32, y: f32| shot.pixels[(y * scale) as usize * shot.size[0] + (x * scale) as usize];
    let px = at(canvas.left() + 200.0, canvas.top() + 200.0);
    // the fake tile's colour, not a missing texture's white
    let want = img.pixels[(canvas.top() as usize + 200) * img.size[0] + canvas.left() as usize + 200];
    let close = |a: u8, b: u8| a.abs_diff(b) <= 12;
    assert!(close(px.r(), want.r()) && close(px.g(), want.g()) && close(px.b(), want.b()), "the capture shows the tiles: {px:?} vs {want:?}");
    assert!(px.b() < 230, "{px:?}");
    assert_eq!(h.app.map.uploads, uploads, "the capture uploaded no tiles of its own");
    frames(&mut h, 3);
    assert_eq!(h.app.map.uploads, uploads);
    // panning a little reuses what is there and only uploads the newly shown tiles
    run(&mut h, "map.setView", json!({"lon": 4.0}));
    assert!(h.step_until(T, |h| h.app.map.tiles_missing == 0 && idle(h)));
    assert!(h.app.map.uploads - uploads <= 12, "{} new uploads for a small pan", h.app.map.uploads - uploads);
    assert!(h.app.map.textures() <= crate::panels::map::MAX_TEXTURES);
    // another tile source: the old source's pictures go
    run(&mut h, "map.tileSource", json!({"offline": true}));
    frames(&mut h, 2);
    assert_eq!((h.app.map.textures(), h.app.map.tiles_drawn), (0, 0));
}

#[test]
fn tiles_that_fail_to_decode_leave_placeholders() {
    let (mut h, _) = demo(Some(fake(true)));
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 48.857, "lon": 2.32, "zoom": 6}));
    assert!(h.step_until(T, |h| h.app.map.tiles_drawn > 0 && h.app.session.map.service().is_some_and(|s| s.is_idle())));
    frames(&mut h, 3);
    assert!(h.app.map.tiles_missing > 0, "the odd columns stay grey");
    assert!(h.painted_text().join("\n").contains("© OpenStreetMap contributors"));
}

#[test]
fn offline_shows_a_note_and_never_starts_the_tile_service() {
    let (mut h, _) = demo(None);
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 3);
    let text = h.painted_text().join("\n");
    assert!(text.contains("Map tiles are off (Settings / map.tileSource)"), "{text}");
    assert!(!text.contains("OpenStreetMap contributors"));
    assert!(h.app.session.map.service().is_none(), "no tile service, so no network");
    assert_eq!(state(&mut h)["offline"], true);
}

#[test]
fn hovering_a_pin_shows_its_thumbnail() {
    let (mut h, _) = demo(None);
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 40.7484, "lon": -73.9857, "zoom": 12}));
    frames(&mut h, 2);
    let s = state(&mut h);
    let ny = clusters(&s).into_iter().find(|c| c["count"] == 1).unwrap();
    h.request("ui.move", json!({"x": ny["x"], "y": ny["y"]}), T);
    let shown = h.step_until(SETTLE, |h| h.app.widgets.iter().any(|(w, _)| w == "map:hoverThumb"));
    assert!(shown, "the thumbnail appears once rendered");
}

#[test]
fn hostile_positions_and_sizes_never_crash() {
    let (mut h, ids) = demo(None);
    // poles, the antimeridian, and a photo with no position at all
    for (id, gps) in ids.iter().zip([json!([90.0, 180.0]), json!([-90.0, -180.0]), json!([0.0, 180.0]), json!([0.0, -180.0]), json!(null)]) {
        h.app.run("photo.setMeta", json!({"ids": [id], "gps": gps})).unwrap();
    }
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 2);
    let s = run(&mut h, "map.fit", json!({}));
    frames(&mut h, 2);
    assert!(s["lat"].as_f64().unwrap().is_finite() && s["zoom"].as_f64().unwrap().is_finite(), "{s}");
    for z in [0.0, 0.5, 19.0] {
        run(&mut h, "map.setView", json!({"lat": 85.0, "lon": 180.0, "zoom": z}));
        frames(&mut h, 2);
        for c in clusters(&state(&mut h)) {
            assert!(c["x"].as_f64().unwrap().is_finite() && c["y"].as_f64().unwrap().is_finite(), "{c}");
        }
    }
    // a tiny window, then a usual one
    h.request("ui.resize", json!({"width": 120, "height": 90}), T);
    frames(&mut h, 3);
    h.request("ui.resize", json!({"width": 1400, "height": 900}), T);
    frames(&mut h, 2);
    // no photo with a position at all
    for id in &ids {
        h.app.run("photo.setMeta", json!({"ids": [id], "gps": null})).unwrap();
    }
    frames(&mut h, 2);
    let s = run(&mut h, "map.fit", json!({}));
    assert_eq!(s["withGps"], 0);
    frames(&mut h, 2);
    assert!(h.painted_text().join("\n").contains("No photos with a location"));
}
