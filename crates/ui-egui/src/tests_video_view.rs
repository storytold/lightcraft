//! Headless playback in the loupe (M14.4) on a synthetic H.264 clip made in-test: play advances
//! the shown frame with the (simulated) player clock, pause shows a full-quality frame, a trim
//! handle drag sets the trim, seek moves the still, and playing stops at the trim end.

use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use filmcraft_h264enc::{Encoder, EncoderConfig, PacketFormat, RateControl, YuvFrame};
use filmcraft_isobmff::{AvcConfig, Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(30);

/// A `w × h` 30 fps clip of `frames` flat frames, frame k's luma `16 + 3k`.
fn write_clip(path: &Path, w: u32, h: u32, frames: usize) {
    let mut cfg = EncoderConfig::new(w, h, 30, 1);
    cfg.format = PacketFormat::LengthPrefixed;
    cfg.bframes = 0;
    cfg.keyint = 30;
    cfg.scenecut = false;
    cfg.rate = RateControl::Crf(12.0);
    cfg.threads = 1;
    let mut enc = Encoder::new(cfg).unwrap();
    let (wu, hu) = (w as usize, h as usize);
    let mut packets = Vec::new();
    for k in 0..frames {
        let (y, u, v) = (vec![(16 + 3 * k).min(235) as u8; wu * hu], vec![128u8; wu * hu / 4], vec![128u8; wu * hu / 4]);
        let f = YuvFrame { y: &y, u: &u, v: &v, y_stride: wu, uv_stride: wu / 2 };
        packets.extend(enc.encode(&f, k as i64).unwrap());
    }
    packets.extend(enc.flush());
    let avcc = AvcConfig::parse(&enc.avcc()).unwrap();
    let mut mux = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let t = mux.add_track(TrackConfig::new(SampleEntry::avc(avcc, w as u16, h as u16), 30)).unwrap();
    for (k, p) in packets.iter().enumerate() {
        let off = (p.pts - k as i64) as i32;
        mux.write_sample(t, WriteSample { data: &p.data, duration: 1, composition_offset: off, is_sync: p.keyframe }).unwrap();
    }
    std::fs::write(path, mux.finish().unwrap().into_inner()).unwrap();
}

fn exec(h: &mut Headless, cmd: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": cmd, "params": params}), T);
    assert_eq!(r["ok"], true, "{cmd}: {r}");
    r["result"].clone()
}

#[test]
fn loupe_plays_pauses_trims_seeks_and_stops_at_the_trim_end() {
    let dir = std::env::temp_dir().join(format!("lcc-video-view-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("clip.mp4");
    write_clip(&clip, 128, 72, 60);
    let clock = Arc::new(Mutex::new(0.0f64));
    let mut session = lcc_engine::Session::new().with_fs();
    let c = clock.clone();
    session.video.clock = Box::new(move || *c.lock().unwrap());
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
    let r = exec(&mut h, "library.import", json!({"paths": [clip.to_string_lossy()]}));
    let id = lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    exec(&mut h, "library.select", json!({"ids": [id.0], "active": id.0}));
    h.app.ui.view = ViewMode::Detail;
    // paused at the start: a full-quality frame
    assert!(h.step_until(T, |h| h.app.video_view.info(id).is_some_and(|i| i.full)), "a still at the start");
    assert_eq!(h.app.loupe_shown.map(|s| s.1), Some("video-still"));
    // play: the shown frame follows the clock
    exec(&mut h, "video.play", json!({}));
    let mut last = -1.0;
    for tick in 1..=5 {
        *clock.lock().unwrap() = tick as f64 * 0.2;
        let want = tick as f64 * 0.2 - 0.05;
        assert!(
            h.step_until(T, |h| h.app.video_view.info(id).is_some_and(|i| !i.full && i.time >= want)),
            "tick {tick}: {:?}",
            h.app.video_view.info(id)
        );
        let i = h.app.video_view.info(id).unwrap();
        assert!(i.playing && i.time > last && i.time <= tick as f64 * 0.2 + 0.02, "{i:?}");
        last = i.time;
    }
    // pause: a full-quality frame at the playhead
    let st = exec(&mut h, "video.pause", json!({}));
    let at = st["current"].as_f64().unwrap();
    assert!(h.step_until(T, |h| h.app.video_view.info(id).is_some_and(|i| i.full && (i.time - at).abs() < 0.04)), "{:?}", h.app.video_view.info(id));
    // a develop edit while paused re-renders the frame (brighter)
    let tex_before = h.app.video_view.texture_id(id);
    exec(&mut h, "develop.set", json!({"control": "light.exposure", "value": 1.0}));
    assert!(h.step_until(T, |h| h.app.video_view.info(id).is_some_and(|i| i.full) && h.app.video_view.texture_id(id) != tex_before));
    // seek: the still moves
    exec(&mut h, "video.seek", json!({"time": 1.5}));
    assert!(h.step_until(T, |h| h.app.video_view.info(id).is_some_and(|i| i.full && (i.time - 1.5).abs() < 0.04)));
    // drag the trim-in handle to the middle of the track: one setTrim on release
    h.step();
    let rect = |h: &Headless, k: &str| h.app.widgets.iter().find(|(n, _)| n == k).map(|(_, r)| *r).unwrap();
    let (hin, track) = (rect(&h, "video:trimIn"), rect(&h, "video:track"));
    let undo = h.app.session.undo.len();
    let from = hin.center();
    let to = egui::pos2(track.left() + track.width() * 0.25, from.y);
    h.app.synthetic.push(egui::Event::PointerMoved(from));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    h.step();
    for i in 1..=5 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 5.0)));
        h.step();
    }
    h.app.synthetic.push(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.step();
    h.step();
    let st = exec(&mut h, "video.get", json!({}));
    assert!((st["trimIn"].as_f64().unwrap() - 0.5).abs() < 0.1, "{st}");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step per drag");
    // end of the trim: playing stops at trim out
    exec(&mut h, "video.setTrim", json!({"in": 0.5, "out": 1.0}));
    exec(&mut h, "video.seek", json!({"time": 0.8}));
    exec(&mut h, "video.play", json!({}));
    let now = *clock.lock().unwrap();
    *clock.lock().unwrap() = now + 1.0;
    for _ in 0..3 {
        h.step();
    }
    let st = exec(&mut h, "video.get", json!({}));
    assert_eq!(st["playing"], false, "{st}");
    assert!((st["current"].as_f64().unwrap() - 1.0).abs() < 1e-6, "{st}");
    assert!(h.step_until(T, |h| h.app.video_view.info(id).is_some_and(|i| i.full && !i.playing)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stands in for the window GPU: answers a fixed texture id, or fails when told to.
struct FakeGpu {
    calls: Arc<Mutex<usize>>,
    fail: Arc<Mutex<bool>>,
}

impl crate::VideoGpu for FakeGpu {
    fn grade(&mut self, img: &lcc_raster::Rgb32f, _: u64, _: &lcc_pipeline::grade::GradeLut) -> Result<(egui::TextureId, [usize; 2]), String> {
        *self.calls.lock().unwrap() += 1;
        if *self.fail.lock().unwrap() {
            return Err("device lost".into());
        }
        Ok((egui::TextureId::User(77), [img.width, img.height]))
    }
}

#[test]
fn playing_frames_go_to_the_gpu_grade_and_fall_back_to_the_cpu() {
    let dir = std::env::temp_dir().join(format!("lcc-video-gpu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("clip.mp4");
    write_clip(&clip, 128, 72, 60);
    let clock = Arc::new(Mutex::new(0.0f64));
    let mut session = lcc_engine::Session::new().with_fs();
    let c = clock.clone();
    session.video.clock = Box::new(move || *c.lock().unwrap());
    let (calls, fail) = (Arc::new(Mutex::new(0)), Arc::new(Mutex::new(false)));
    let gpu = FakeGpu { calls: calls.clone(), fail: fail.clone() };
    let app = LccApp::new(session, Services { png: None, video_gpu: Some(Box::new(gpu)), ..Default::default() });
    let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
    h.app.ui.settings.gpu = true;
    let r = exec(&mut h, "library.import", json!({"paths": [clip.to_string_lossy()]}));
    let id = lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    exec(&mut h, "library.select", json!({"ids": [id.0], "active": id.0}));
    h.app.ui.view = ViewMode::Detail;
    assert!(h.step_until(T, |h| h.app.video_view.info(id).is_some_and(|i| i.full)), "a still at the start");
    assert_eq!(*calls.lock().unwrap(), 0, "paused stills stay on the CPU pipeline");
    exec(&mut h, "video.play", json!({}));
    *clock.lock().unwrap() = 0.3;
    assert!(h.step_until(T, |h| h.app.video_view.info(id).is_some_and(|i| !i.full && i.gpu)), "{:?}", h.app.video_view.info(id));
    assert_eq!(h.app.video_view.texture_id(id), Some(egui::TextureId::User(77)));
    // the GPU fails: that frame and the rest are graded on the CPU, and playing goes on
    *fail.lock().unwrap() = true;
    *clock.lock().unwrap() = 0.8;
    assert!(h.step_until(T, |h| h.app.video_view.info(id).is_some_and(|i| !i.full && !i.gpu && i.time > 0.6)), "{:?}", h.app.video_view.info(id));
    let n = *calls.lock().unwrap();
    *clock.lock().unwrap() = 1.4;
    assert!(h.step_until(T, |h| h.app.video_view.info(id).is_some_and(|i| !i.gpu && i.time > 1.2)));
    assert_eq!(*calls.lock().unwrap(), n, "not tried again after a failure");
    let _ = std::fs::remove_dir_all(&dir);
}
