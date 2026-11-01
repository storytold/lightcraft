//! Culling a folder of raws with a profile picked for every photo (a body's default profile) showed
//! "Rendering…" in the loupe and empty cells in the grid and the filmstrip, because a photo with a
//! profile counted as edited and so had no cheap picture: each waited for a full raw render. The
//! rule: never an empty placeholder; the cheapest picture first for the whole view, then the next
//! one, then anything bigger.
//!
//! Here the raws' embedded previews are instant and every raw decode is held until the test lets
//! it go (renders that take seconds), so a picture on screen can only be a stand-in.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// The raw decodes: held until `release` drops, and the moment the first one started.
struct Decodes {
    release: Option<std::sync::mpsc::Sender<()>>,
    first: Arc<Mutex<Option<Instant>>>,
}

/// A headless app culling `n` raws, each with a profile picked.
fn culling(n: u64) -> (Headless, Decodes) {
    let mut s = lightcraft_engine::Session::new();
    s.media.preview_loader = Some(Arc::new(|_: &str, edge: usize| {
        let w = edge.clamp(1, 96);
        let h = (w * 2 / 3).max(1);
        Some(lightcraft_raster::Rgba8 { width: w, height: h, data: vec![[180, 120, 90, 255]; w * h] })
    }));
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = Arc::new(Mutex::new(held));
    let first = Arc::new(Mutex::new(None));
    let f = first.clone();
    s.media.file_loader = Some(Arc::new(move |_, _| {
        f.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert_with(Instant::now);
        // a 45 MP raw: held (one decode at a time here; the others queue behind the lock)
        let _ = held.lock().map(|r| r.recv_timeout(Duration::from_secs(30)));
        Ok((lightcraft_raster::Rgb32f::new(8, 8), Default::default()))
    }));
    for id in 1..=n {
        let mut p = Photo::new(PhotoId(id), Source::File { path: format!("cull/r{id}.cr3") }, &format!("r{id}.cr3"), "CR3", 8192, 5464, "2026-10-22");
        p.kind = lightcraft_catalog::MediaKind::Raw;
        let mut d = p.camera_defaults();
        // a profile picked for every photo of the body (any profile other than the default)
        d.profile.id = "lc.vivid".into();
        p.develop = Arc::new(d);
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let app = LightcraftApp::new(s, Services { png: None, ..Default::default() });
    (Headless::new(app, [1400.0, 900.0], 1.0), Decodes { release: Some(release), first })
}

impl Drop for Decodes {
    fn drop(&mut self) {
        self.release.take();
    }
}

/// Step frames (about 60 a second) until `done`, at most `max`.
fn frames_until(h: &mut Headless, max: Duration, done: impl Fn(&Headless) -> bool) -> Option<Duration> {
    let t0 = Instant::now();
    while t0.elapsed() < max {
        h.step();
        if done(h) {
            return Some(t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    None
}

#[test]
fn every_cell_on_screen_has_a_picture_before_any_raw_is_rendered() {
    let (mut h, decodes) = culling(60);
    let r = h.request("engine.execute", json!({"command": "view.photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.thumb_size = 160.0;
    h.app.thumb_first.reset();
    let filled = frames_until(&mut h, Duration::from_secs(10), |h| {
        let s = h.app.thumb_first.summary();
        s.seen > 0 && s.empty_now == 0
    });
    let at = Instant::now();
    let s = h.app.thumb_first.summary();
    assert!(filled.is_some(), "cells still empty with every raw decode held: {s:?}");
    assert!(s.seen >= 12, "a screenful of cells: {s:?}");
    // the real thumbnails behind the stand-ins (a raw decode each) start only after the screenful
    let first = *decodes.first.lock().unwrap();
    assert!(first.is_none_or(|f| f >= at - Duration::from_millis(50)), "a raw was decoded before every cell had its picture");
    // and they come: background work starts once the screen is full
    let started = frames_until(&mut h, Duration::from_secs(5), |_| decodes.first.lock().unwrap().is_some());
    assert!(started.is_some(), "the real thumbnails are rendered in the background afterwards");
}

#[test]
fn the_loupe_shows_the_camera_preview_at_once_while_the_raw_renders() {
    let (mut h, decodes) = culling(30);
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [5]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for step in 0..4 {
        let id = h.app.session.active();
        let shown = frames_until(&mut h, Duration::from_secs(3), |h| {
            h.app.loupe_shown.is_some_and(|(p, s)| Some(p) == h.app.session.active() && crate::loupe_timing::is_full_size(s))
        });
        assert!(shown.is_some(), "step {step}: the loupe showed {:?} for {id:?}, not the camera preview", h.app.loupe_shown);
        assert_eq!(h.app.loupe_shown.map(|(_, s)| s), Some("embedded"), "the raw itself is still being decoded");
        // and the loupe says so (FredApps on #3080: the camera's JPEG differs from the developed
        // photo in colour and sharpness), with what that means for its accessible name
        h.step();
        let w = h.request("ui.widgets", json!({"filter": "loupe:cameraPreview"}), T);
        let badge = w["result"].as_array().and_then(|a| a.first()).cloned().unwrap_or_default();
        assert_eq!(badge["id"], "loupe:cameraPreview", "step {step}: no Camera preview badge over the camera JPEG: {w}");
        // the filmstrip under it has its pictures too
        let film = frames_until(&mut h, Duration::from_secs(3), |h| h.app.thumb_first.summary().empty_now == 0);
        assert!(film.is_some(), "step {step}: filmstrip cells empty: {:?}", h.app.thumb_first.summary());
        let r = h.request("engine.execute", json!({"command": "library.next"}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    // once the raw's own render is on screen, the badge goes
    drop(decodes);
    let rendered = frames_until(&mut h, Duration::from_secs(10), |h| h.app.loupe_shown.is_some_and(|(_, s)| matches!(s, "render" | "cached")));
    assert!(rendered.is_some(), "the render replaced the camera JPEG: {:?}", h.app.loupe_shown);
    h.step();
    let w = h.request("ui.widgets", json!({"filter": "loupe:cameraPreview"}), T);
    assert_eq!(w["result"].as_array().map(Vec::len), Some(0), "the badge stays only while the camera JPEG is shown: {w}");
}

/// The filmstrip counts only its cells on screen as seen (CodeRabbit on #3080): the cells it
/// draws ahead on either side are not waited for, so they neither skew `thumbFirst` nor hold
/// back the background work.
#[test]
fn the_filmstrip_counts_only_its_cells_on_screen() {
    let (mut h, _decodes) = culling(40);
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [20]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    frames_until(&mut h, Duration::from_millis(300), |_| false);
    h.app.thumb_first.reset();
    let filled = frames_until(&mut h, Duration::from_secs(3), |h| {
        let s = h.app.thumb_first.summary();
        s.seen > 0 && s.empty_now == 0
    });
    assert!(filled.is_some(), "filmstrip cells empty: {:?}", h.app.thumb_first.summary());
    // a 1400 px wide strip of 120 px cells shows 12 of them, 13 when one is cut at each end
    let s = h.app.thumb_first.summary();
    assert!(s.seen <= 13, "cells off screen were counted: {s:?}");
}
