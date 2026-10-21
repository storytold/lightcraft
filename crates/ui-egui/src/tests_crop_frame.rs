//! Crop & Straighten on a large photo (issue #1643): the frame stays put on screen while the photo
//! rotates and moves beneath it, and a drag doesn't re-render the uncropped photo every frame.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(60);

/// The demo library in the crop tool, cropped to the middle of the photo.
fn cropped() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for (method, params) in [
        ("ui.set", json!({"view": "detail"})),
        ("engine.execute", json!({"command": "panel.crop"})),
        ("engine.execute", json!({"command": "crop.set", "params": {"rect": [0.25, 0.25, 0.75, 0.75]}})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(T);
    h
}

fn crop(h: &Headless) -> lightcraft_geom::CropGeometry {
    let id = h.app.session.active().expect("a photo");
    h.app.session.catalog.photo(id).expect("the photo").develop.crop.geometry
}

/// The crop box's corners on screen (its corner handles).
fn box_corners(h: &Headless) -> [egui::Pos2; 4] {
    [0, 1, 2, 3].map(|i| {
        let id = format!("cropHandle:{i}");
        h.app.widgets.iter().find(|(w, _)| *w == id).map(|(_, r)| r.center()).expect("the crop box's handles")
    })
}

fn near(a: egui::Pos2, b: egui::Pos2) -> bool {
    a.distance(b) < 0.75
}

fn press(h: &mut Headless, at: egui::Pos2) {
    h.events.push(egui::Event::PointerMoved(at));
    h.step();
    h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.step();
}

fn drag_to(h: &mut Headless, at: egui::Pos2) {
    h.events.push(egui::Event::PointerMoved(at));
    h.step();
    h.step();
}

fn release(h: &mut Headless, at: egui::Pos2) {
    h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.settle(T);
}

fn upright(c: &[egui::Pos2; 4]) -> bool {
    (c[0].y - c[1].y).abs() < 0.75 && (c[0].x - c[3].x).abs() < 0.75
}

/// Rotating outside the box turns the photo under it: the box stays where it is on screen and
/// upright, the photo still covers it, and the drag draws the render it has (no new one per step).
#[test]
fn rotating_turns_the_photo_under_a_box_that_stays_put() {
    let mut h = cropped();
    let start = box_corners(&h);
    assert!(upright(&start));
    let img = h.app.image_rect.expect("the photo on screen");
    let at = egui::pos2(img.left() - 20.0, img.center().y);
    press(&mut h, at);
    drag_to(&mut h, at + egui::vec2(0.0, -10.0));
    // (a drag draws a draft of the photo, once)
    let main = h.app.renderer.wanted(crate::render::Slot::Main);
    for dy in [20.0, 40.0, 60.0] {
        drag_to(&mut h, at + egui::vec2(0.0, -dy));
        let now = box_corners(&h);
        assert!(crop(&h).angle != 0.0, "the drag rotates");
        assert!(upright(&now), "the box stays upright: {now:?}");
        let (c0, c1) = (start[0] + (start[2] - start[0]) / 2.0, now[0] + (now[2] - now[0]) / 2.0);
        assert!(near(c0, c1), "the box keeps its place on screen: {c0:?} → {c1:?}");
        assert_eq!(h.app.renderer.wanted(crate::render::Slot::Main), main, "no new render of the photo while it turns");
    }
    // dragged up on the left of the box: clockwise, and the photo turns with the pointer
    assert!(crop(&h).angle > 0.0, "{}", crop(&h).angle);
    release(&mut h, at + egui::vec2(0.0, -60.0));
    let id = h.app.session.active().unwrap();
    let p = h.app.session.catalog.photo(id).unwrap();
    assert!(crop(&h).is_within_image(p.width as f64, p.height as f64), "the photo covers the box");
    assert!(upright(&box_corners(&h)));
}

/// The Straighten slider turns the photo too: the box keeps its place while it is dragged.
#[test]
fn straightening_keeps_the_box_in_place() {
    let mut h = cropped();
    let start = box_corners(&h);
    h.app.run("develop.beginInteraction", json!({"label": "Straighten"})).unwrap();
    h.step();
    for a in [2.0, 5.0, 9.0] {
        h.app.run("crop.straighten", json!({"angle": a})).unwrap();
        h.step();
        let now = box_corners(&h);
        assert!(upright(&now));
        let (c0, c1) = (start[0] + (start[2] - start[0]) / 2.0, now[0] + (now[2] - now[0]) / 2.0);
        assert!(near(c0, c1), "{c0:?} → {c1:?}");
    }
    h.app.run("develop.endInteraction", json!({})).unwrap();
    h.settle(T);
}

/// Zooming in the crop tool magnifies the photo (and the box with it), as the loupe does, so a
/// crop can be placed precisely: the requested zoom and pan are not overwritten by the fit.
#[test]
fn the_crop_tool_follows_the_requested_zoom() {
    let mut h = cropped();
    let fitted = box_corners(&h);
    let width = |c: &[egui::Pos2; 4]| c[0].distance(c[1]);
    let r = h.request("ui.set", json!({"zoom": {"percent": 400}, "pan": [0.5, 0.5]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    let zoomed = box_corners(&h);
    assert!(upright(&zoomed));
    assert!(width(&zoomed) > width(&fitted) * 1.5, "the box grew with the zoom: {} -> {}", width(&fitted), width(&zoomed));
    let r = h.request("ui.set", json!({"zoom": "fit", "pan": [0.5, 0.5]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    assert!(near(box_corners(&h)[0], fitted[0]), "Fit brings it back");
}

/// Dragging inside the box moves the photo under it: the box stays put and the photo follows the
/// pointer (the crop moves the other way over the photo).
#[test]
fn dragging_inside_the_box_moves_the_photo() {
    let mut h = cropped();
    let start = box_corners(&h);
    let x0 = crop(&h).rect.x0;
    let at = start[0] + (start[2] - start[0]) / 2.0;
    press(&mut h, at);
    drag_to(&mut h, at + egui::vec2(10.0, 0.0));
    drag_to(&mut h, at + egui::vec2(40.0, 0.0));
    assert!(crop(&h).rect.x0 < x0, "the photo moved right, so the crop is further left on it");
    let now = box_corners(&h);
    assert!(start.iter().zip(&now).all(|(a, b)| near(*a, *b)), "the box stays put: {start:?} → {now:?}");
    release(&mut h, at + egui::vec2(40.0, 0.0));
}

fn big_photo(tag: &str, w: usize, h: usize) -> (Headless, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-crop-frame-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut data = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let v = ((x * 7 + y * 3) % 256) as u8;
            data.push([v, ((x / 40 + y / 40) % 2 * 200) as u8, (y * 255 / h) as u8, 255]);
        }
    }
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let jpg = lightcraft_codecs::encode_jpeg(&lightcraft_codecs::EncodeImage::rgba8(&img), 90, Default::default(), &Default::default()).unwrap();
    let path = dir.join("big.jpg");
    std::fs::write(&path, jpg).unwrap();
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services { png: None, ..Default::default() });
    app.run("library.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    for (method, params) in [("ui.set", json!({"view": "detail"})), ("engine.execute", json!({"command": "panel.crop"}))] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(T);
    (h, dir)
}

/// Measurement (run by hand): per drag tick, the frame time, the time until the screen is current
/// again, and how many ticks asked for a new render of the photo.
fn measure(h: &mut Headless, what: &str, tick: impl Fn(&mut Headless, usize)) {
    h.app.run("develop.beginInteraction", json!({"label": "Crop"})).unwrap();
    let (mut frame_ms, mut settle_ms, mut renders) = (Vec::new(), Vec::new(), 0);
    let mut wanted = h.app.renderer.wanted(crate::render::Slot::Main);
    for i in 1..=20 {
        tick(h, i);
        let t0 = Instant::now();
        h.step();
        frame_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        let now = h.app.renderer.wanted(crate::render::Slot::Main);
        if now != wanted {
            renders += 1;
            wanted = now;
        }
        // until the photo on screen is the current render (the draft has arrived)
        let t1 = Instant::now();
        h.step_until(T, |h| h.app.renderer.in_flight() == 0 && !h.app.renderer.is_pending(crate::render::Slot::Main));
        settle_ms.push(t1.elapsed().as_secs_f64() * 1000.0 + frame_ms.last().copied().unwrap_or(0.0));
        h.settle(T);
    }
    h.app.run("develop.endInteraction", json!({})).unwrap();
    h.settle(T);
    let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let max = |v: &[f64]| v.iter().cloned().fold(0.0, f64::max);
    eprintln!(
        "MEASURE {what}: frame avg {:.1} ms max {:.1} ms; until current avg {:.1} ms max {:.1} ms; new renders {renders}/20",
        avg(&frame_ms),
        max(&frame_ms),
        avg(&settle_ms),
        max(&settle_ms)
    );
}

#[test]
#[ignore]
fn measure_crop_drags() {
    let (mut h, dir) = big_photo("measure", 6000, 4000);
    let id = h.app.session.active().unwrap();
    eprintln!("MEASURE photo {}x{}", h.app.session.catalog.photo(id).unwrap().width, h.app.session.catalog.photo(id).unwrap().height);
    measure(&mut h, "straighten", |h, i| {
        h.app.run("crop.straighten", json!({"angle": i as f64 * 0.5})).unwrap();
    });
    h.app.run("crop.straighten", json!({"angle": 0.0})).unwrap();
    h.app.run("crop.set", json!({"rect": [0.2, 0.2, 0.7, 0.7]})).ok();
    h.settle(T);
    measure(&mut h, "move", |h, i| {
        let x = 0.2 + i as f64 * 0.005;
        h.app.run("crop.drag", json!({"handle": 8, "from": [0.45, 0.45], "to": [x + 0.25, 0.45], "start": [0.2, 0.2, 0.7, 0.7]})).unwrap();
    });
    let _ = std::fs::remove_dir_all(&dir);
}
