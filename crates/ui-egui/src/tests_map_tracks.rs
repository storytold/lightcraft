//! Headless tests of track logs on the Map view (LRC-MAP-TRACKLOG): a loaded GPX file drawn as
//! lines (pixels along the track, none across a logging gap), the map fitted to it, the active
//! track picked from the Tracklog menu, the selected photos marked on the track, Auto-Tag from the
//! menu tagging them where the log says (one undo step), and hostile tracks (antimeridian, poles,
//! one point, a tiny map, a load in the middle of a drag) never crashing. Tiles come from the fake
//! server of `tests_map` (never the network).

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_geom::Point;
use lightcraft_map::{LatLon, Viewport};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::tests_map::{T, demo, demo_sized, fake, frames, run, state, widget};

/// Track 0 "Seine" across Paris (two segments: 10:00–10:10 and 11:00–11:05 UTC), track 1
/// "Thames" in London (14:00–14:10 UTC).
const GPX: &str = r#"<?xml version="1.0"?>
<gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1">
<trk><name>Seine</name>
<trkseg>
<trkpt lat="48.850" lon="2.250"><time>2026-05-01T10:00:00Z</time></trkpt>
<trkpt lat="48.852" lon="2.260"><time>2026-05-01T10:01:00Z</time></trkpt>
<trkpt lat="48.854" lon="2.270"><time>2026-05-01T10:02:00Z</time></trkpt>
<trkpt lat="48.856" lon="2.280"><time>2026-05-01T10:03:00Z</time></trkpt>
<trkpt lat="48.858" lon="2.290"><time>2026-05-01T10:04:00Z</time></trkpt>
<trkpt lat="48.860" lon="2.300"><time>2026-05-01T10:05:00Z</time></trkpt>
<trkpt lat="48.862" lon="2.310"><time>2026-05-01T10:06:00Z</time></trkpt>
<trkpt lat="48.864" lon="2.320"><time>2026-05-01T10:07:00Z</time></trkpt>
<trkpt lat="48.866" lon="2.330"><time>2026-05-01T10:08:00Z</time></trkpt>
<trkpt lat="48.868" lon="2.340"><time>2026-05-01T10:09:00Z</time></trkpt>
<trkpt lat="48.870" lon="2.350"><time>2026-05-01T10:10:00Z</time></trkpt>
</trkseg>
<trkseg>
<trkpt lat="48.840" lon="2.400"><time>2026-05-01T11:00:00Z</time></trkpt>
<trkpt lat="48.830" lon="2.425"><time>2026-05-01T11:02:30Z</time></trkpt>
<trkpt lat="48.820" lon="2.450"><time>2026-05-01T11:05:00Z</time></trkpt>
</trkseg>
</trk>
<trk><name>Thames</name><trkseg>
<trkpt lat="51.500" lon="-0.120"><time>2026-05-01T14:00:00Z</time></trkpt>
<trkpt lat="51.510" lon="-0.085"><time>2026-05-01T14:05:00Z</time></trkpt>
<trkpt lat="51.520" lon="-0.050"><time>2026-05-01T14:10:00Z</time></trkpt>
</trkseg></trk>
</gpx>"#;

fn viewport(s: &Value) -> Viewport {
    let f = |k: &str| s[k].as_f64().unwrap();
    Viewport::new(LatLon { lat: f("lat"), lon: f("lon") }, f("zoom"), f("width"), f("height"))
}

/// Where `(lat, lon)` is on screen now.
fn on_screen(h: &mut Headless, lat: f64, lon: f64) -> egui::Pos2 {
    let s = state(h);
    let p: Point = viewport(&s).to_screen(LatLon { lat, lon });
    let canvas = h.app.map.rect.expect("the map is drawn");
    egui::pos2(canvas.min.x + p.x as f32, canvas.min.y + p.y as f32)
}

fn is_track(c: egui::Color32) -> bool {
    c.r() > 180 && c.g() < 120 && c.b() > 80
}

/// The track's colour at or right next to `p` (lines are 3 px wide).
fn track_at(img: &egui::ColorImage, p: egui::Pos2) -> bool {
    let [w, h] = img.size;
    (-1..=1).any(|dy| {
        (-1..=1).any(|dx| {
            let (x, y) = (p.x.round() as i64 + dx, p.y.round() as i64 + dy);
            x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && is_track(img.pixels[y as usize * w + x as usize])
        })
    })
}

fn settle_tiles(h: &mut Headless) {
    h.step_until(T, |h| h.app.session.map.service().is_none_or(|s| s.is_idle()));
    frames(h, 4);
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    frames(h, 3);
}

#[test]
fn a_loaded_track_is_drawn_along_its_points_and_the_map_fits_it() {
    let (mut h, _) = demo(Some(fake(false)));
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 0.0, "lon": 100.0, "zoom": 3}));
    frames(&mut h, 3);
    let r = run(&mut h, "map.trackLog", json!({"gpx": GPX}));
    assert_eq!((r["loaded"].as_bool(), r["tracks"].as_array().map(Vec::len)), (Some(true), Some(2)), "{r}");
    frames(&mut h, 3);
    // the map fits both tracks (Paris to London)
    let s = state(&mut h);
    let lat = s["lat"].as_f64().unwrap();
    assert!((48.8..51.6).contains(&lat) && (-0.2..2.5).contains(&s["lon"].as_f64().unwrap()), "{s}");
    for (la, lo) in [(48.85, 2.25), (51.52, -0.05)] {
        let p = on_screen(&mut h, la, lo);
        assert!(h.app.map.rect.unwrap().contains(p), "({la}, {lo}) on the map: {p:?}");
    }
    // pick the Seine: the map fits it; lines along its points, nothing across its logging gap
    run(&mut h, "map.trackLog", json!({"active": 0}));
    settle_tiles(&mut h);
    let s = state(&mut h);
    let z = s["zoom"].as_f64().unwrap();
    assert!((11.0..=14.0).contains(&z), "zoomed to the walk: {s}");
    let tl = &s["trackLog"];
    assert_eq!(tl["active"], 0);
    assert_eq!(tl["drawn"]["lines"][0], 2, "two segments, two lines: {tl}");
    assert!(tl["drawn"]["points"].as_u64().unwrap() >= 4, "{tl}");
    let img = h.paint();
    // (away from the photo pins at 2.2945° and 2.3376°, drawn over it)
    for t in [0.15, 0.3, 0.65, 0.75] {
        // between the points of the first segment (it is straight: on the line at any t)
        let p = on_screen(&mut h, 48.850 + 0.020 * t, 2.250 + 0.100 * t);
        assert!(track_at(&img, p), "the track is drawn at t = {t} ({p:?})");
    }
    let p = on_screen(&mut h, 48.830, 2.425);
    assert!(track_at(&img, p), "the second segment too ({p:?})");
    // the gap between the segments (10:10 → 11:00) is not joined
    for t in [0.3, 0.5, 0.7] {
        let p = on_screen(&mut h, 48.870 - 0.030 * t, 2.350 + 0.050 * t);
        assert!(!track_at(&img, p), "no line across the gap at t = {t} ({p:?})");
    }
    // nowhere else: a point well off the track
    let p = on_screen(&mut h, 48.80, 2.28);
    assert!(!track_at(&img, p));
    // cleared: nothing drawn
    run(&mut h, "map.trackLog", json!({"clear": true}));
    frames(&mut h, 3);
    let img = h.paint();
    let p = on_screen(&mut h, 48.850 + 0.020 * 0.3, 2.250 + 0.100 * 0.3);
    assert!(!track_at(&img, p), "cleared");
    assert_eq!(state(&mut h)["trackLog"]["loaded"], false);
}

#[test]
fn the_tracklog_menu_picks_the_active_track_and_shows_its_time_range() {
    let (mut h, _) = demo(Some(fake(false)));
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 3);
    click(&mut h, "map:tracklog");
    assert!(widget(&h, "map:tracklog:load").is_some(), "nothing loaded: the menu offers to load one");
    h.request("ui.key", json!({"key": "Escape"}), T);
    frames(&mut h, 2);
    run(&mut h, "map.trackLog", json!({"gpx": GPX, "offset": "+02:00"}));
    frames(&mut h, 3);
    click(&mut h, "map:tracklog");
    for id in ["map:tracklog:track:all", "map:tracklog:track:0", "map:tracklog:track:1", "map:tracklog:info", "map:tracklog:autoTag"] {
        assert!(widget(&h, id).is_some(), "{id} in the menu");
    }
    // the whole log's range on the camera's clock (+02:00)
    let text = h.painted_text().join("\n");
    assert!(text.contains("Seine") && text.contains("Thames"), "{text}");
    assert!(text.contains("2026-05-01 12:00:00") && text.contains("2026-05-01 16:10:00") && text.contains("UTC+02:00"), "{text}");
    assert!(text.contains("4 h 10 min"), "{text}");
    click(&mut h, "map:tracklog:track:1");
    let s = state(&mut h);
    assert_eq!(s["trackLog"]["active"], 1, "{}", s["trackLog"]);
    assert!((s["lat"].as_f64().unwrap() - 51.51).abs() < 0.05, "fitted to the Thames: {s}");
    // the active track drawn bright, the other dimmed (off screen here: drawn but not counted)
    click(&mut h, "map:tracklog");
    let text = h.painted_text().join("\n");
    assert!(text.contains("2026-05-01 16:00:00") && text.contains("10 min 00 s"), "the Thames' range: {text}");
    click(&mut h, "map:tracklog:track:all");
    assert!(state(&mut h)["trackLog"]["active"].is_null());
}

/// Photos 0–2 lose their position and get capture times on a +02:00 camera clock: 10:03:30 UTC
/// (the Seine's first segment), 11:02:30 (its second) and 14:05 (the Thames).
fn timed_photos(h: &mut Headless, ids: &[u64]) {
    for (id, t) in ids.iter().zip(["2026-05-01T12:03:30", "2026-05-01T13:02:30", "2026-05-01T16:05:00"]) {
        h.app.session.commit("t", Op::SetCaptured { id: PhotoId(*id), captured: Some(t.into()) }).unwrap();
    }
    run(h, "photo.removeGps", json!({"ids": &ids[..3]}));
}

#[test]
fn auto_tag_from_the_map_tags_the_selected_photos_where_the_track_says_in_one_undo_step() {
    let (mut h, ids) = demo(Some(fake(false)));
    timed_photos(&mut h, &ids);
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.trackLog", json!({"gpx": GPX, "active": 0}));
    run(&mut h, "library.select", json!({"ids": &ids[..3]}));
    frames(&mut h, 3);
    // no offset yet: Auto-Tag asks for the camera's time zone first
    click(&mut h, "map:tracklog");
    click(&mut h, "map:tracklog:autoTag");
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::TextPrompt { .. })), "asks for the time zone");
    h.app.ui.dialog = None;
    assert!(ids[..3].iter().all(|i| h.app.session.catalog.photo(PhotoId(*i)).unwrap().meta.gps.is_none()), "nothing tagged yet");
    // with the offset set, the selected photos are marked on the track where they'll go
    run(&mut h, "map.trackLog.setOffset", json!({"offset": "+02:00"}));
    frames(&mut h, 3);
    let s = state(&mut h);
    let ticks = s["trackLog"]["ticks"].as_array().cloned().unwrap_or_default();
    assert_eq!(s["trackLog"]["onTrack"], 2, "two of the three fall on the Seine: {}", s["trackLog"]);
    let log = h.app.session.map.track.as_ref().unwrap().active_log.clone();
    let utc = |t: &str| lightcraft_meta::DateTime::parse_iso(t).unwrap().unix_seconds() as f64;
    let expect = [log.locate(utc("2026-05-01T10:03:30Z"), 600.0).unwrap().0, log.locate(utc("2026-05-01T11:02:30Z"), 600.0).unwrap().0];
    let img = h.paint();
    // the segments they fall on
    let segs = [((48.856, 2.280), (48.858, 2.290)), ((48.840, 2.400), (48.830, 2.425))];
    for ((id, g), (a, b)) in ids.iter().zip(&expect).zip(segs) {
        let tick = ticks.iter().find(|t| t["id"] == *id).unwrap_or_else(|| panic!("photo {id} marked: {ticks:?}"));
        let p = on_screen(&mut h, g.latitude, g.longitude);
        assert!((tick["x"].as_f64().unwrap() as f32 - p.x).abs() < 0.5 && (tick["y"].as_f64().unwrap() as f32 - p.y).abs() < 0.5, "{tick} at {p:?}");
        // the mark sits on the line: the track goes on either side of it
        let dir = (on_screen(&mut h, b.0, b.1) - on_screen(&mut h, a.0, a.1)).normalized();
        assert!(track_at(&img, p + dir * 9.0) && track_at(&img, p - dir * 9.0), "photo {id}: the line through its mark at {p:?}");
    }
    // Auto-Tag from the menu: the two on the track get the track's position, one undo step
    let depth = h.app.session.undo.len();
    click(&mut h, "map:tracklog");
    click(&mut h, "map:tracklog:autoTag");
    assert!(h.app.ui.dialog.is_none(), "the offset is known: no question");
    assert_eq!(h.app.session.undo.len(), depth + 1, "one undo step");
    let gps = |h: &Headless, id: u64| h.app.session.catalog.photo(PhotoId(id)).unwrap().meta.gps;
    for (id, g) in ids.iter().zip(&expect) {
        let (la, lo) = gps(&h, *id).expect("tagged");
        assert!((la - g.latitude).abs() < 1e-6 && (lo - g.longitude).abs() < 1e-6, "{id}: ({la}, {lo}) vs {g:?}");
    }
    assert_eq!(gps(&h, ids[2]), None, "outside the Seine's time range");
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("Tagged 2 photos") && toast.contains("1 outside"), "{toast}");
    // the pins are now on the track
    frames(&mut h, 3);
    let s = state(&mut h);
    assert!(
        crate::tests_map::clusters(&s).iter().any(|c| crate::tests_map::ids_of(c).contains(&ids[0])),
        "a pin for the tagged photo: {}",
        s["clusters"]
    );
    run(&mut h, "edit.undo", json!({}));
    assert!(ids[..3].iter().all(|i| gps(&h, *i).is_none()), "one step undoes both");
    // the command form with the offset in the call
    run(&mut h, "map.trackLog", json!({"offset": null}));
    let r = run(&mut h, "map.trackLog.autoTag", json!({"offset": "+02:00", "ids": [ids[1]]}));
    assert_eq!(r["tagged"], 1, "{r}");
}

#[test]
fn hostile_tracks_tiny_maps_and_a_load_mid_drag_never_crash() {
    let (mut h, _) = demo_sized(Some(fake(false)), [1000.0, 700.0]);
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 2);
    // across the antimeridian, with points at the poles and NaN
    let wild = r#"<gpx><trk><trkseg>
        <trkpt lat="-17.0" lon="179.5"><time>2026-01-01T00:00:00Z</time></trkpt>
        <trkpt lat="-17.2" lon="-179.5"><time>2026-01-01T00:10:00Z</time></trkpt>
        <trkpt lat="NaN" lon="NaN"><time>2026-01-01T00:15:00Z</time></trkpt>
        <trkpt lat="-17.4" lon="-179.0"><time>2026-01-01T00:20:00Z</time></trkpt>
      </trkseg><trkseg>
        <trkpt lat="90" lon="0"><time>2026-01-01T01:00:00Z</time></trkpt>
        <trkpt lat="-90" lon="180"><time>2026-01-01T00:59:00Z</time></trkpt>
      </trkseg></trk>
      <trk><trkseg><trkpt lat="10" lon="20"><time>2026-01-01T03:00:00Z</time></trkpt></trkseg></trk></gpx>"#;
    run(&mut h, "map.trackLog", json!({"gpx": wild, "active": 0}));
    run(&mut h, "map.setView", json!({"lat": -17.2, "lon": 180.0, "zoom": 8}));
    settle_tiles(&mut h);
    let s = state(&mut h);
    let tl = &s["trackLog"];
    let img = h.paint();
    // the crossing is drawn the short way: at the map's centre column, not across the world
    let p = on_screen(&mut h, -17.1, 180.0);
    assert!(track_at(&img, p), "the line crosses ±180° on screen ({p:?}): {tl}");
    // the poles, the whole world, one point, a deep zoom
    for (lat, lon, zoom) in [(0.0, 0.0, 0.0), (85.0, 0.0, 2.0), (10.0, 20.0, 19.0), (-17.2, -179.9, 19.0)] {
        run(&mut h, "map.setView", json!({"lat": lat, "lon": lon, "zoom": zoom}));
        frames(&mut h, 2);
    }
    run(&mut h, "map.trackLog", json!({"active": 1}));
    frames(&mut h, 2);
    assert_eq!(state(&mut h)["trackLog"]["active"], 1);
    // a load in the middle of a drag: the drag goes on from the new view
    let c = h.app.map.rect.unwrap().center();
    let ev = |kind: &str, x: f32, y: f32| json!({"kind": kind, "x": x, "y": y});
    h.request("ui.pointer", json!({"screen": true, "events": [ev("down", c.x, c.y)]}), T);
    frames(&mut h, 2);
    h.request("ui.pointer", json!({"screen": true, "events": [ev("move", c.x + 30.0, c.y + 10.0)]}), T);
    frames(&mut h, 2);
    run(&mut h, "map.trackLog", json!({"gpx": GPX}));
    h.request("ui.pointer", json!({"screen": true, "events": [ev("move", c.x + 60.0, c.y + 20.0)]}), T);
    frames(&mut h, 2);
    h.request("ui.pointer", json!({"screen": true, "events": [ev("up", c.x + 60.0, c.y + 20.0)]}), T);
    frames(&mut h, 3);
    assert_eq!(state(&mut h)["trackLog"]["tracks"].as_array().map(Vec::len), Some(2));
    // a tiny window and back
    h.request("ui.resize", json!({"width": 120, "height": 90}), T);
    frames(&mut h, 3);
    h.request("ui.resize", json!({"width": 1000, "height": 700}), T);
    frames(&mut h, 3);
    let _ = state(&mut h);
    // bad loads are errors (the loaded log stays), never panics
    for bad in [json!({"gpx": "<gpx/>"}), json!({"path": "/no/such/file.gpx"}), json!({"active": 9}), json!({"gpx": "not xml at all <<<"})] {
        let r = h.request("engine.execute", json!({"command": "map.trackLog", "params": bad}), T);
        assert_eq!(r["ok"], false, "{bad}: {r}");
    }
    assert_eq!(state(&mut h)["trackLog"]["tracks"].as_array().map(Vec::len), Some(2));
    // loading a file by path from the UI command (the picker's answer)
    let dir = std::env::temp_dir().join(format!("lc-ui-tracks-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("walk.gpx");
    std::fs::write(&path, GPX).unwrap();
    let r = run(&mut h, "map.loadTrackLog", json!({"path": path.to_string_lossy()}));
    assert_eq!(r["name"], "walk.gpx", "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The screenshot for the PR (`LIGHTCRAFT_TEST_SHOTS=<dir>`): a loaded track with the photos
/// tagged along it and the Tracklog menu open, over the fake tiles.
#[test]
fn screenshot_of_a_track_log() {
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
    h.settle(crate::tests_map::SETTLE);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(8).map(|p| p.0).collect();
    // eight photos along the Seine walk, a minute apart (camera on +02:00)
    for (i, id) in ids.iter().enumerate() {
        let t = format!("2026-05-01T12:0{}:{}0", i, if i % 2 == 0 { 2 } else { 4 });
        h.app.session.commit("t", Op::SetCaptured { id: PhotoId(*id), captured: Some(t) }).unwrap();
    }
    run(&mut h, "view.map", json!({}));
    let gpx = std::path::Path::new(&dir).join("seine-and-thames.gpx");
    std::fs::write(&gpx, GPX).unwrap();
    run(&mut h, "map.trackLog", json!({"path": gpx.to_string_lossy(), "offset": "+02:00"}));
    run(&mut h, "map.trackLog", json!({"active": 0}));
    // five tagged (pins on the track); three more selected and marked where they would go
    run(&mut h, "photo.removeGps", json!({"ids": ids}));
    run(&mut h, "map.trackLog.autoTag", json!({"ids": &ids[..5]}));
    run(&mut h, "library.select", json!({"ids": &ids[3..]}));
    settle_tiles(&mut h);
    click(&mut h, "map:tracklog");
    frames(&mut h, 3);
    let path = std::path::Path::new(&dir).join("map-tracklog.png");
    let r = h.request("ui.screenshot", json!({"path": path.to_string_lossy(), "headless": true}), T);
    assert_eq!(r["ok"], true, "{r}");
}
