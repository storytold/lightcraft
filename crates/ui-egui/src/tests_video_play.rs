//! Playing a video in the loupe (LR-VID-PLAY, KEY-VIDEO-PLAY): Space plays and pauses a clip
//! instead of toggling the zoom, the frame shows upright in place of the poster, the transport
//! bar's controls run the `video.*` commands, and leaving the clip stops it. The clips come from a
//! fake opener (no decoder needed; the engine's tests cover real decoding).

use std::time::Duration;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_engine::video_play::{FrameSource, Next, SourceInfo, VideoFrame};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_secs(60);
const CLIP: PhotoId = PhotoId(1);
const STILL: PhotoId = PhotoId(2);

/// 100 frames at 25 fps of 64 × 36 pixels, stored landscape, shown a quarter turn clockwise
/// (a phone held upright); with an audio track nothing plays.
struct Fake {
    info: SourceInfo,
    next: usize,
}

impl FrameSource for Fake {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.next = ((seconds / 0.04 + 1e-6).floor().max(0.0) as usize).min(99);
        Ok(())
    }
    fn next_frame(&mut self, stop: &mut dyn FnMut() -> bool, mut buffer: Vec<u8>) -> Result<Next, String> {
        if stop() {
            return Ok(Next::Stopped);
        }
        if self.next >= 100 {
            return Ok(Next::End);
        }
        let i = self.next;
        self.next += 1;
        buffer.clear();
        // red, a little brighter every frame
        for _ in 0..64 * 36 {
            buffer.extend_from_slice(&[100 + i as u8, 20, 20, 255]);
        }
        Ok(Next::Frame(VideoFrame { pts: i as f64 * 0.04, width: 64, height: 36, rgba: buffer, draft: false }))
    }
    fn set_draft(&mut self, _: bool) {}
}

fn opener(_path: &str) -> Result<Box<dyn FrameSource>, String> {
    let info = SourceInfo {
        duration: 4.0,
        width: 64,
        height: 36,
        frame_duration: 0.04,
        orientation: lightcraft_geom::Orientation::Rotate90,
        codec: "H.264".into(),
        audio: Some("AAC, 48 kHz, stereo".into()),
    };
    Ok(Box::new(Fake { info, next: 0 }))
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn state(h: &mut Headless) -> Value {
    exec(h, "video.state", json!({}))
}

/// A clip and a still, the clip active in the loupe.
fn app() -> Headless {
    let mut session = lightcraft_engine::Session::new();
    session.video.opener = opener;
    session.video.decodes = || true;
    for (id, name, kind) in [(CLIP, "clip.mp4", MediaKind::Video), (STILL, "still.jpg", MediaKind::Image)] {
        let mut p = Photo::new(id, Source::File { path: format!("/lightcraft-video-test/{name}") }, name, "MP4", 64, 36, "2026-05-04T10:00:00");
        p.kind = kind;
        p.duration = (kind == MediaKind::Video).then_some(4.0);
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    session.selection.active = Some(CLIP);
    let app = LightcraftApp::new(session, Services::default());
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.view = ViewMode::Detail;
    h.settle(SETTLE);
    h
}

#[test]
fn space_plays_and_pauses_a_video_instead_of_zooming() {
    let mut h = app();
    assert!(has(&h, "video:transport") && has(&h, "icon:video:playPause") && has(&h, "video:scrub"));
    let zoom = h.app.ui.zoom;
    let r = h.request("ui.key", json!({"key": "space"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step_until(T, |h| h.app.session.video.player.as_ref().is_some_and(|p| p.playing()));
    assert_eq!(h.app.ui.zoom, zoom, "Space toggled the zoom of a video");
    h.request("ui.key", json!({"key": "space"}), T);
    h.step_until(T, |h| h.app.session.video.player.as_ref().is_some_and(|p| !p.playing()));
}

#[test]
fn the_frame_shows_upright_in_place_of_the_poster() {
    let mut h = app();
    exec(&mut h, "video.seek", json!({"time": 1.0}));
    assert!(h.step_until(T, |h| h.app.loupe_shown == Some((CLIP, "videoFrame"))), "{:?}", h.app.loupe_shown);
    // stored 64 × 36, turned a quarter: drawn portrait
    let r = widget(&h, "canvas:videoFrame");
    assert!(r.height() > r.width(), "{r:?}");
    assert!((r.width() / r.height() - 36.0 / 64.0).abs() < 0.01, "{r:?}");
    let v = state(&mut h);
    assert!((v["frame"]["pts"].as_f64().unwrap() - 1.0).abs() < 1e-9, "{v}");
}

#[test]
fn transport_controls_run_the_video_commands() {
    let mut h = app();
    let r = h.request("ui.clickWidget", json!({"id": "icon:video:playPause"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.session.video.player.as_ref().is_some_and(|p| p.playing())));
    h.request("ui.clickWidget", json!({"id": "icon:video:stepForward"}), T);
    assert!(h.step_until(T, |h| h.app.session.video.player.as_ref().is_some_and(|p| !p.playing())));
    // a click three quarters along the scrub bar's track seeks there
    let s = widget(&h, "video:scrub");
    let fx = (4.0 + (s.width() - 8.0) * 0.75) / s.width();
    let r = h.request("ui.clickWidget", json!({"id": "video:scrub", "fx": fx, "fy": 0.5}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| h.app.session.video.player.as_ref().is_some_and(|p| (p.position() - 3.0).abs() < 0.1)));
    h.request("ui.clickWidget", json!({"id": "icon:video:stepBack"}), T);
    assert!(h.step_until(T, |h| h.app.session.video.player.as_ref().and_then(|p| p.frame()).is_some_and(|(f, _)| f.pts < 2.97)));
}

#[test]
fn the_bar_says_quietly_that_audio_is_not_played() {
    let mut h = app();
    // known once the clip is open
    exec(&mut h, "video.seek", json!({"time": 0.0}));
    assert!(h.step_until(T, |h| has(h, "video:audioNote")));
    assert_eq!(state(&mut h)["audio"], json!({"track": "AAC, 48 kHz, stereo", "played": false}));
    assert!(!has(&h, "video:problem"));
}

#[test]
fn leaving_the_clip_or_the_loupe_stops_playback() {
    let mut h = app();
    exec(&mut h, "video.play", json!({}));
    assert!(h.app.session.video.player.is_some());
    exec(&mut h, "library.select", json!({"ids": [STILL.0]}));
    assert!(h.step_until(T, |h| h.app.session.video.player.is_none()));
    assert!(!has(&h, "video:transport"));
    exec(&mut h, "library.select", json!({"ids": [CLIP.0]}));
    h.settle(SETTLE);
    exec(&mut h, "video.play", json!({}));
    h.app.ui.view = ViewMode::PhotoGrid;
    assert!(h.step_until(T, |h| h.app.session.video.player.is_none()));
    assert_eq!(state(&mut h)["open"], false);
}

#[test]
fn a_still_keeps_space_for_the_zoom() {
    let mut h = app();
    exec(&mut h, "library.select", json!({"ids": [STILL.0]}));
    h.settle(SETTLE);
    let zoom = h.app.ui.zoom;
    h.request("ui.key", json!({"key": "space"}), T);
    h.settle(SETTLE);
    assert_ne!(h.app.ui.zoom, zoom);
    assert!(h.app.session.video.player.is_none());
}

#[test]
fn a_clip_that_cannot_play_says_why_above_the_bar() {
    let mut h = app();
    h.app.session.video.opener = |_| Err("this build has no video decoding (feature video-frames)".into());
    h.request("ui.key", json!({"key": "space"}), T);
    assert!(h.step_until(T, |h| has(h, "video:problem")));
    let v = state(&mut h);
    assert_eq!(v["playing"], false);
    assert!(v["error"].as_str().is_some_and(|e| e.contains("no video decoding")), "{v}");
}

#[test]
fn a_build_without_video_decoding_offers_no_play_and_says_why() {
    let mut h = app();
    h.app.session.video.decodes = || false;
    assert!(h.step_until(T, |h| has(h, "video:problem")));
    // Space and the Play button start no decoder
    h.request("ui.key", json!({"key": "space"}), T);
    h.settle(SETTLE);
    let _ = h.request("ui.clickWidget", json!({"id": "icon:video:playPause"}), T);
    h.settle(SETTLE);
    assert!(h.app.session.video.player.is_none());
    let v = state(&mut h);
    assert_eq!(v["available"], false, "{v}");
}
