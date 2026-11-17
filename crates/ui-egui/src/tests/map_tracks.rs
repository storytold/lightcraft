//! Track logs on the Map view (LRC-MAP-TRACKLOG): loading one draws it and fits the map, the
//! Tracklog menu picks the active track and shows its time range, the selected photos are marked
//! where Auto-Tag would put them, Auto-Tag tags them in one undo step, and bad files, hostile tracks
//! and tiny windows never crash. The map is never online in tests.

use lightcraft_catalog::{Op, PhotoId};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

/// Two tracks: "Walk" (10:00–10:02 and 12:00–12:01 UTC, two segments) and "Ride" (14:00–14:10).
const TWO_TRACKS: &str = r#"<?xml version="1.0"?>
<gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1">
<trk><name>Walk</name>
  <trkseg>
    <trkpt lat="46.0000" lon="7.0000"><time>2026-05-01T10:00:00Z</time></trkpt>
    <trkpt lat="46.0010" lon="7.0020"><time>2026-05-01T10:01:00Z</time></trkpt>
    <trkpt lat="46.0020" lon="7.0040"><time>2026-05-01T10:02:00Z</time></trkpt>
  </trkseg>
  <trkseg>
    <trkpt lat="46.1000" lon="7.1000"><time>2026-05-01T12:00:00Z</time></trkpt>
    <trkpt lat="46.1010" lon="7.1010"><time>2026-05-01T12:01:00Z</time></trkpt>
  </trkseg>
</trk>
<trk><name>Ride</name><trkseg>
    <trkpt lat="47.0000" lon="8.0000"><time>2026-05-01T14:00:00Z</time></trkpt>
    <trkpt lat="47.1000" lon="8.1000"><time>2026-05-01T14:10:00Z</time></trkpt>
</trkseg></trk>
</gpx>"#;

fn temp_gpx(tag: &str, text: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-ui-tracks-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("walk.gpx");
    std::fs::write(&p, text).unwrap();
    p
}

fn demo() -> Headless {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1300.0, 1000.0],
        1.0,
    );
    h.app.run("view.map", json!({})).unwrap();
    frames(&mut h, 3);
    h
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

/// Frames until the track log being read on a worker thread has landed (or was refused).
fn wait_loaded(h: &mut Headless) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while state(h)["trackLog"]["loading"].is_string() {
        assert!(std::time::Instant::now() < end, "the track log never landed");
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.step();
    }
    frames(h, 2);
}

fn click(h: &mut Headless, id: &str) {
    let at = widget(h, id).unwrap_or_else(|| panic!("{id} on screen")).center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    frames(h, 2);
}

#[test]
fn a_loaded_track_is_drawn_and_the_map_fits_it_and_the_menu_picks_a_track() {
    let mut h = demo();
    let path = temp_gpx("load", TWO_TRACKS);
    h.app.run("map.loadTrackLog", json!({"path": path.to_string_lossy()})).unwrap();
    wait_loaded(&mut h);
    let s = state(&mut h);
    let t = &s["trackLog"];
    assert_eq!(t["loaded"], true, "{s}");
    assert_eq!(t["tracks"].as_array().unwrap().len(), 2);
    // the map shows the whole log (Switzerland), and the track is drawn
    let (lat, lon) = (s["lat"].as_f64().unwrap(), s["lon"].as_f64().unwrap());
    assert!((45.5..47.6).contains(&lat) && (6.5..8.6).contains(&lon), "{lat} {lon}");
    assert!(t["drawn"]["points"].as_u64().unwrap() >= 2, "{t}");
    // the Tracklog menu: the two tracks, the time range; picking Ride makes it the active one
    click(&mut h, "map:tracklog");
    assert!(widget(&h, "map:tracklog:info").is_some(), "the menu shows the time range");
    assert!(widget(&h, "map:tracklog:track:1").is_some());
    click(&mut h, "map:tracklog:track:1");
    let t = state(&mut h)["trackLog"].clone();
    assert_eq!(t["active"], 1, "{t}");
    // Clear from the menu
    click(&mut h, "map:tracklog");
    click(&mut h, "map:tracklog:clear");
    assert_eq!(state(&mut h)["trackLog"]["loaded"], false);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn the_selected_photos_are_marked_on_the_track_and_auto_tag_tags_them_in_one_step() {
    let mut h = demo();
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    // two photos in the Walk's time (camera clock UTC), one between the tracks
    for (id, t) in ids.iter().zip(["2026-05-01T10:00:30", "2026-05-01T12:00:30", "2026-05-01T13:00:00"]) {
        h.app.session.commit("t", Op::SetCaptured { id: PhotoId(*id), captured: Some(t.into()) }).unwrap();
    }
    h.app.run("photo.removeGps", json!({"ids": ids})).unwrap();
    h.app.run("map.trackLog", json!({"gpx": TWO_TRACKS, "offset": "+00:00"})).unwrap();
    h.app.run("map.trackLog.fit", json!({})).unwrap();
    h.app.run("library.select", json!({"ids": ids})).unwrap();
    frames(&mut h, 4);
    let t = state(&mut h)["trackLog"].clone();
    assert_eq!(t["onTrack"], 2, "two of the three fall on the track: {t}");
    let steps = h.app.session.undo.len();
    let r = h.app.run("map.trackLog.autoTag", json!({})).unwrap();
    assert_eq!(r["tagged"], 2, "{r}");
    assert_eq!(h.app.session.undo.len(), steps + 1, "one undo step");
    let gps = |h: &Headless, id: u64| h.app.session.catalog.photo(PhotoId(id)).and_then(|p| p.meta.gps);
    let a = gps(&h, ids[0]).expect("tagged");
    assert!((a.0 - 46.0).abs() < 0.01 && (a.1 - 7.0).abs() < 0.01, "{a:?}");
    assert!(gps(&h, ids[2]).is_none(), "outside the track's time");
    h.app.run("edit.undo", json!({})).unwrap();
    assert!(gps(&h, ids[0]).is_none() && gps(&h, ids[1]).is_none());
}

#[test]
fn bad_files_hostile_tracks_and_tiny_windows_never_crash() {
    let mut h = demo();
    h.app.run("map.trackLog", json!({"gpx": TWO_TRACKS})).unwrap();
    let before = h.app.session.map.track_generation;
    // a file that isn't GPX, a missing one: refused with a message, and the loaded log stays
    let junk = temp_gpx("junk", "<html>not a track</html>");
    for p in [junk.to_string_lossy().to_string(), "Z:/no/such/file.gpx".to_string()] {
        h.app.ui.toast = None;
        h.app.run("map.loadTrackLog", json!({"path": p})).unwrap();
        wait_loaded(&mut h);
        assert!(h.app.ui.toast.is_some(), "{p}: the refusal is shown");
    }
    assert_eq!(h.app.session.map.track_generation, before);
    // a track across the antimeridian and one at the pole draw without a crash
    let hostile = r#"<gpx><trk><trkseg>
        <trkpt lat="10" lon="179.9"><time>2026-05-01T10:00:00Z</time></trkpt>
        <trkpt lat="10.1" lon="-179.9"><time>2026-05-01T10:01:00Z</time></trkpt>
        <trkpt lat="89.99" lon="0"><time>2026-05-01T10:02:00Z</time></trkpt>
        <trkpt lat="-89.99" lon="180"><time>2026-05-01T10:03:00Z</time></trkpt>
    </trkseg></trk></gpx>"#;
    h.app.run("map.trackLog", json!({"gpx": hostile})).unwrap();
    for z in [0.0, 1.0, 5.0, 12.0] {
        h.app.run("map.setView", json!({"lat": 10, "lon": 180, "zoom": z})).unwrap();
        frames(&mut h, 2);
    }
    // a tiny window
    h.request("ui.resize", json!({"width": 60, "height": 40}), std::time::Duration::from_secs(20));
    frames(&mut h, 3);
    h.request("ui.resize", json!({"width": 1300, "height": 1000}), std::time::Duration::from_secs(20));
    frames(&mut h, 2);
    assert_eq!(state(&mut h)["trackLog"]["loaded"], true);
    let _ = std::fs::remove_dir_all(junk.parent().unwrap());
}

/// Load Tracklog… reads the file on a worker thread with a row on the activity stack, and the UI
/// keeps drawing meanwhile.
#[test]
fn loading_runs_off_the_ui_thread_with_an_activity_row() {
    let mut h = demo();
    // 60 000 points: long enough to see the row
    let mut gpx = String::from("<gpx><trk><trkseg>");
    for i in 0..60_000u32 {
        let (lat, lon) = (45.0 + f64::from(i) * 1e-5, 6.0 + f64::from(i) * 1e-5);
        gpx.push_str(&format!("<trkpt lat=\"{lat:.6}\" lon=\"{lon:.6}\"><time>2026-05-01T10:{:02}:{:02}Z</time></trkpt>", (i / 60) % 60, i % 60));
    }
    gpx.push_str("</trkseg></trk></gpx>");
    let path = temp_gpx("big", &gpx);
    h.app.run("map.loadTrackLog", json!({"path": path.to_string_lossy()})).unwrap();
    let rows: Vec<String> = h.app.session.activity.list().iter().map(|t| t.label.clone()).collect();
    assert!(rows.iter().any(|l| l.contains("walk.gpx")), "the activity stack shows the load: {rows:?}");
    h.step();
    wait_loaded(&mut h);
    assert_eq!(state(&mut h)["trackLog"]["points"], 60_000);
    assert!(h.app.session.activity.list().iter().all(|t| !t.label.contains("walk.gpx")), "the row goes with the load");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
