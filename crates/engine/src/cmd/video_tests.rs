//! The `video.*` commands on a session whose clips come from a fake opener (no decoder needed):
//! what the Space key, the transport bar, the control channel and MCP see.

use std::time::Duration;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;
use crate::video_play::{FrameSource, Next, SourceInfo, VideoFrame};

/// 50 frames at 25 fps, 4 × 2 pixels; each frame's first byte is its index.
struct Fake {
    info: SourceInfo,
    next: usize,
}

impl FrameSource for Fake {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.next = ((seconds / 0.04 + 1e-6).floor().max(0.0) as usize).min(49);
        Ok(())
    }
    fn next_frame(&mut self, stop: &mut dyn FnMut() -> bool, mut buffer: Vec<u8>) -> Result<Next, String> {
        if stop() {
            return Ok(Next::Stopped);
        }
        if self.next >= 50 {
            return Ok(Next::End);
        }
        let i = self.next;
        self.next += 1;
        buffer.clear();
        buffer.resize(32, 0);
        buffer[0] = i as u8;
        Ok(Next::Frame(VideoFrame { pts: i as f64 * 0.04, width: 4, height: 2, rgba: buffer, draft: false }))
    }
    fn set_draft(&mut self, _: bool) {}
}

fn fake_opener(path: &str) -> Result<Box<dyn FrameSource>, String> {
    if path.contains("missing") {
        return Err("The clip could not be opened: no such file".into());
    }
    let info = SourceInfo {
        duration: 2.0,
        width: 4,
        height: 2,
        frame_duration: 0.04,
        orientation: lightcraft_geom::Orientation::Rotate90,
        codec: "H.264".into(),
        audio: Some("AAC, 48 kHz, stereo".into()),
    };
    Ok(Box::new(Fake { info, next: 0 }))
}

fn add(s: &mut Session, name: &str, kind: MediaKind) -> PhotoId {
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: format!("/clips/{name}") }, name, "MP4", 4, 2, "2026-01-01T10:00:00");
    p.kind = kind;
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn session() -> (Session, PhotoId, PhotoId) {
    let mut s = Session::new();
    s.video.opener = fake_opener;
    let clip = add(&mut s, "clip.mp4", MediaKind::Video);
    let still = add(&mut s, "still.jpg", MediaKind::Image);
    s.selection.active = Some(clip);
    (s, clip, still)
}

/// Run `video.state` until `done` holds (the decode thread runs on its own), at most 5 s.
fn state_until(s: &mut Session, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..5000 {
        let v = s.execute("video.state", &json!({})).unwrap();
        if done(&v) {
            return v;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out: {}", s.execute("video.state", &json!({})).unwrap());
}

fn frame_pts(v: &Value) -> Option<f64> {
    v["frame"]["pts"].as_f64()
}

#[test]
fn nothing_plays_until_asked() {
    let (mut s, _, _) = session();
    let v = s.execute("video.state", &json!({})).unwrap();
    assert_eq!(v["open"], false);
    assert_eq!(v["available"], crate::video_source::available());
    assert_eq!(s.execute("video.stop", &json!({})).unwrap()["stopped"], false);
    // pausing nothing is fine
    s.execute("video.pause", &json!({})).unwrap();
}

#[test]
fn play_pause_toggles_and_reports_the_clip() {
    let (mut s, clip, _) = session();
    let v = s.execute("video.playPause", &json!({})).unwrap();
    assert_eq!(v["open"], true);
    assert_eq!(v["photo"], clip.0);
    assert_eq!(v["playing"], true);
    let v = state_until(&mut s, |v| v["duration"].is_number());
    assert_eq!(v["duration"], 2.0);
    assert_eq!(v["codec"], "H.264");
    assert_eq!(v["orientation"], 6);
    assert_eq!(v["audio"], json!({"track": "AAC, 48 kHz, stereo", "played": false}));
    let v = s.execute("video.playPause", &json!({})).unwrap();
    assert_eq!(v["playing"], false);
}

#[test]
fn seek_and_step_land_on_frames() {
    let (mut s, _, _) = session();
    s.execute("video.seek", &json!({"time": 1.0})).unwrap();
    let v = state_until(&mut s, |v| frame_pts(v).is_some_and(|t| (t - 1.0).abs() < 1e-9));
    assert_eq!(v["playing"], false);
    s.execute("video.step", &json!({})).unwrap();
    state_until(&mut s, |v| frame_pts(v).is_some_and(|t| (t - 1.04).abs() < 1e-9));
    s.execute("video.step", &json!({"frames": -2})).unwrap();
    state_until(&mut s, |v| frame_pts(v).is_some_and(|t| (t - 0.96).abs() < 1e-9));
    s.execute("video.seek", &json!({"fraction": 0.5})).unwrap();
    state_until(&mut s, |v| frame_pts(v).is_some_and(|t| (t - 1.0).abs() < 1e-9));
    s.execute("video.play", &json!({"at": 0.2})).unwrap();
    let v = state_until(&mut s, |v| frame_pts(v).is_some_and(|t| t >= 0.2 - 1e-9));
    assert_eq!(v["playing"], true);
}

#[test]
fn bad_arguments_are_refused_with_a_reason() {
    let (mut s, _, _) = session();
    for (cmd, p) in [
        ("video.seek", json!({})),
        ("video.seek", json!({"time": -1})),
        ("video.seek", json!({"time": "soon"})),
        ("video.seek", json!({"fraction": 1.5})),
        ("video.step", json!({"frames": 0})),
        ("video.step", json!({"frames": 1e9})),
        ("video.step", json!({"frames": 0.5})),
        ("video.play", json!({"at": f64::MAX})),
    ] {
        let r = s.execute(cmd, &p);
        if cmd == "video.play" {
            // a huge start time is clamped to the clip, not an error
            assert!(r.is_ok(), "{cmd} {p}: {r:?}");
        } else {
            assert!(r.is_err(), "{cmd} {p} was accepted");
        }
    }
}

#[test]
fn a_still_or_nothing_selected_cannot_play() {
    let (mut s, _, still) = session();
    s.selection.active = Some(still);
    let e = s.execute("video.playPause", &json!({})).unwrap_err().to_string();
    assert!(e.contains("not a video"), "{e}");
    s.selection.active = None;
    assert!(s.execute("video.play", &json!({})).is_err());
}

#[test]
fn an_unreadable_clip_reports_why() {
    let (mut s, _, _) = session();
    let gone = add(&mut s, "missing.mp4", MediaKind::Video);
    s.selection.active = Some(gone);
    s.execute("video.playPause", &json!({})).unwrap();
    let v = state_until(&mut s, |v| v["error"].is_string());
    assert!(v["error"].as_str().unwrap().contains("no such file"), "{v}");
    assert_eq!(v["playing"], false);
}

#[test]
fn leaving_the_clip_stops_playback() {
    let (mut s, _, still) = session();
    s.execute("video.play", &json!({})).unwrap();
    assert!(s.video.player.is_some());
    s.selection.active = Some(still);
    assert_eq!(s.video_tick(), None);
    assert!(s.video.player.is_none());
    assert_eq!(s.execute("video.state", &json!({})).unwrap()["open"], false);
}

#[test]
fn playback_commands_stay_out_of_the_journal_and_undo() {
    let (mut s, _, _) = session();
    let before = s.undo.len();
    s.execute("video.play", &json!({})).unwrap();
    s.execute("video.seek", &json!({"time": 0.5})).unwrap();
    s.execute("video.pause", &json!({})).unwrap();
    assert_eq!(s.undo.len(), before);
    for id in ["video.playPause", "video.play", "video.pause", "video.seek", "video.step", "video.stop", "video.state"] {
        let spec = crate::cmd::find_command(id).unwrap();
        assert!(!spec.journal, "{id} is journalled");
    }
}
