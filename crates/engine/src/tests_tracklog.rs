//! The Map view's track log (`map.trackLog`): loading, tracks and the active one, the time zone
//! offset, clearing, refused files (the loaded log stays), and tagging from it
//! (`photo.autoTagTracklog {loaded: true}`) in one undo step.

use lightcraft_catalog::{Op, PhotoId};
use serde_json::json;

use crate::{EngineError, Session};

/// Two tracks: "Walk" (two segments, 10:00–10:02 and 12:00–12:01 UTC) and "Ride" (14:00–14:10).
pub(crate) const TWO_TRACKS: &str = r#"<?xml version="1.0"?>
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

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-tracklog-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn refused(r: crate::Result<serde_json::Value>) -> String {
    match r {
        Err(EngineError::BadParams { msg, .. }) => msg,
        other => panic!("refused, not {other:?}"),
    }
}

#[test]
fn load_report_pick_a_track_set_the_offset_and_clear() {
    let dir = temp_dir("load");
    let path = dir.join("trip.gpx");
    std::fs::write(&path, TWO_TRACKS).unwrap();
    let mut s = Session::new();
    assert_eq!(s.execute("map.trackLog", &json!({})).unwrap()["loaded"], false);
    let r = s.execute("map.trackLog", &json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!((r["loaded"].as_bool(), r["name"].as_str(), r["points"].as_u64()), (Some(true), Some("trip.gpx"), Some(7)), "{r}");
    let tracks = r["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 2);
    assert_eq!((tracks[0]["name"].as_str(), tracks[0]["points"].as_u64(), tracks[0]["segments"].as_u64()), (Some("Walk"), Some(5), Some(2)));
    assert_eq!((tracks[1]["start"].as_str(), tracks[1]["end"].as_str()), (Some("2026-05-01T14:00:00Z"), Some("2026-05-01T14:10:00Z")));
    assert_eq!(tracks[1]["duration"], 600.0);
    // all tracks are active at first: the whole file's time range, in UTC (no offset yet)
    assert!(r["active"].is_null());
    assert_eq!((r["start"].as_str(), r["end"].as_str()), (Some("2026-05-01T10:00:00Z"), Some("2026-05-01T14:10:00Z")));
    assert_eq!(r["startLocal"], "2026-05-01T10:00:00");
    assert_eq!(s.map.track.as_ref().unwrap().shapes.len(), 2, "each track ready to draw");
    let g0 = r["generation"].as_u64().unwrap();
    // pick the second track, and the camera's zone: the local times follow
    let r = s.execute("map.trackLog", &json!({"active": 1, "offset": "+02:00"})).unwrap();
    assert_eq!((r["active"].as_u64(), r["activePoints"].as_u64(), r["offset"].as_str()), (Some(1), Some(2), Some("+02:00")), "{r}");
    assert_eq!((r["startLocal"].as_str(), r["endLocal"].as_str()), (Some("2026-05-01T16:00:00"), Some("2026-05-01T16:10:00")));
    assert!(r["generation"].as_u64().unwrap() > g0);
    let r = s.execute("map.trackLog", &json!({"active": "all", "offset": -5.5})).unwrap();
    assert_eq!((r["active"].is_null(), r["offset"].as_str()), (true, Some("-05:30")));
    let r = s.execute("map.trackLog", &json!({"offset": null})).unwrap();
    assert!(r["offset"].is_null());
    // a report changes nothing
    let g = r["generation"].clone();
    assert_eq!(s.execute("map.trackLog", &json!({})).unwrap()["generation"], g);
    // clear
    let r = s.execute("map.trackLog", &json!({"clear": true})).unwrap();
    assert_eq!(r["loaded"], false);
    assert!(s.map.track.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_files_are_refused_and_the_loaded_log_stays() {
    let dir = temp_dir("bad");
    let mut s = Session::with_demo();
    let one = s.catalog.photos().next().unwrap().id.0;
    s.execute("map.trackLog", &json!({"gpx": TWO_TRACKS})).unwrap();
    let g = s.map.track_generation;
    let huge = dir.join("huge.gpx");
    let f = std::fs::File::create(&huge).unwrap();
    f.set_len(crate::tracklog::MAX_GPX_BYTES + 1).unwrap();
    drop(f);
    let binary = dir.join("bin.gpx");
    std::fs::write(&binary, [0xffu8, 0xfe, 0x00, 0x81]).unwrap();
    let cases = [
        (json!({"path": huge.to_string_lossy()}), "larger than 50 MB"),
        (json!({"path": dir.join("missing.gpx").to_string_lossy()}), "can't read"),
        (json!({"path": dir.to_string_lossy()}), "not a file"),
        (json!({"path": binary.to_string_lossy()}), "not a text"),
        (json!({"gpx": "<kml><Document/></kml>"}), "not a GPX file"),
        (json!({"gpx": "<gpx version=\"1.1\"/>"}), "no tracks"),
        (json!({"gpx": "<gpx><trk><trkseg/></trk></gpx>"}), "no track points"),
        (json!({"gpx": "<gpx><trk><trkseg><trkpt lat=\"1\" lon=\"2\"/></trkseg></trk></gpx>"}), "no timed track points (1 without a time)"),
        (
            json!({"gpx": "<gpx><trk><trkseg><trkpt lat=\"NaN\" lon=\"2\"><time>2026-01-01T00:00:00Z</time></trkpt></trkseg></trk></gpx>"}),
            "no track points",
        ),
        // an unclosed document is refused as malformed before its points are counted
        (json!({"gpx": "<gpx><trk>"}), "malformed"),
        (
            json!({"gpx": "<gpx><trk><trkseg><trkpt lat=\"1\" lon=\"2\"><time>2026-01-01T00:00:00Z</time></trkpt></trkseg></trk><!-- never closed"}),
            "malformed",
        ),
        (json!({"gpx": 5}), "`path` is a file path"),
        (json!({"active": 2}), "no track 2"),
        (json!({"active": -1}), "track's index"),
        (json!({"offset": "noon"}), "can't read offset"),
        (json!({"offset": 19}), "±18"),
        (json!({"offset": "+19:00"}), "offset"),
        (json!({"clear": true, "active": 0}), "alone"),
        (json!({"clear": "yes"}), "true or false"),
        (json!({"gpx": TWO_TRACKS, "active": 5}), "no track 5"),
    ];
    for (p, why) in cases {
        let msg = refused(s.execute("map.trackLog", &p));
        assert!(msg.contains(why), "{p}: {msg}");
        assert_eq!(s.map.track_generation, g, "{p} changed nothing");
        assert_eq!(s.map.track.as_ref().map(|t| t.tracks.len()), Some(2));
    }
    // nothing loaded: picking a track or an offset has nothing to act on
    s.execute("map.trackLog", &json!({"clear": true})).unwrap();
    assert!(refused(s.execute("map.trackLog", &json!({"active": 0}))).contains("no track log"));
    assert!(refused(s.execute("map.trackLog", &json!({"offset": 2}))).contains("no track log"));
    assert!(refused(s.execute("photo.autoTagTracklog", &json!({"loaded": true, "ids": [one]}))).contains("no track log"));
    // the existing command takes the same size bound
    assert!(refused(s.execute("photo.autoTagTracklog", &json!({"path": huge.to_string_lossy(), "ids": [one]}))).contains("50 MB"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn auto_tag_from_the_loaded_track_uses_its_active_track_and_offset_in_one_undo_step() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.visible_cloned().iter().take(3).map(|p| p.0).collect();
    let set = |s: &mut Session, id: u64, t: &str| {
        s.commit("t", Op::SetCaptured { id: PhotoId(id), captured: Some(t.into()) }).unwrap();
        s.execute("photo.removeGps", &json!({"ids": [id]})).unwrap();
    };
    set(&mut s, ids[0], "2026-05-01T12:00:30"); // +02:00 camera: 10:00:30 UTC, the walk
    set(&mut s, ids[1], "2026-05-01T16:05:00"); // 14:05 UTC, half-way along the ride
    set(&mut s, ids[2], "2026-05-01T14:00:30"); // 12:00:30 UTC, the walk's second segment
    s.execute("map.trackLog", &json!({"gpx": TWO_TRACKS, "offset": "+02:00", "active": 1})).unwrap();
    let gps = |s: &Session, id: u64| s.catalog.photo(PhotoId(id)).unwrap().meta.gps;
    // a dry run: only the ride's photo falls on the active track
    let r = s.execute("photo.autoTagTracklog", &json!({"loaded": true, "ids": ids, "dryRun": true})).unwrap();
    assert_eq!((r["tagged"].as_u64(), r["skipped"]["outside"].as_u64(), r["points"].as_u64()), (Some(1), Some(2), Some(2)), "{r}");
    // all tracks: the three, each where the log says it was (interpolated within its segment)
    s.execute("map.trackLog", &json!({"active": "all"})).unwrap();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    let depth = s.undo.len();
    let r = s.execute("photo.autoTagTracklog", &json!({"loaded": true})).unwrap();
    assert_eq!((r["tagged"].as_u64(), r["assumedUtc"].as_u64()), (Some(3), Some(0)), "the track's offset counts: {r}");
    assert_eq!(s.undo.len(), depth + 1, "one undo step");
    let log = s.map.track.as_ref().unwrap().log.clone();
    let t = |iso: &str| lightcraft_meta::DateTime::parse_iso(iso).unwrap().unix_seconds() as f64;
    for (id, utc) in ids.iter().zip(["2026-05-01T10:00:30Z", "2026-05-01T14:05:00Z", "2026-05-01T12:00:30Z"]) {
        let (want, _) = log.locate(t(utc), 600.0).unwrap();
        let (la, lo) = gps(&s, *id).unwrap();
        assert!((la - want.latitude).abs() < 1e-6 && (lo - want.longitude).abs() < 1e-6, "{id}: {la},{lo} vs {want:?}");
    }
    assert_eq!(gps(&s, ids[1]), Some((47.05, 8.05)));
    // an offset given to the command wins over the track's
    let r = s.execute("photo.autoTagTracklog", &json!({"loaded": true, "offset": 0, "replace": true, "dryRun": true})).unwrap();
    assert_eq!(r["tagged"], 2, "two hours later on the log's clock: {r}");
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(ids.iter().all(|i| gps(&s, *i).is_none()), "one step undoes them all");
}
