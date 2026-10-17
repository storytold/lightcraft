//! Crop & Straighten on a large photo (issue #60): the frame stays put on screen while the photo
//! rotates and moves beneath it, and a drag doesn't re-render the uncropped photo every frame.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(60);

/// The demo library in the crop tool, cropped to the middle of the photo.
fn cropped() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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

fn crop(h: &Headless) -> lcc_geom::CropGeometry {
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
