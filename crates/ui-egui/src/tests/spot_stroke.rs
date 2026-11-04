//! Headless tests of the Remove tool's brush stroke (issue #2359): points are recorded at a spacing, not one per
//! frame, and a stroke can be grabbed anywhere along it.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn remove_tool() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for (req, params) in [("ui.set", json!({"view": "detail"})), ("engine.execute", json!({"command": "panel.remove"}))] {
        let r = h.request(req, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    // spots show and can be grabbed whatever the pointer does
    let r = h.request("engine.execute", json!({"command": "view.toolOverlay", "params": {"mode": "always"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.remove_size = 0.04;
    h
}

fn pointer(h: &mut Headless, kind: &str, x: f64, y: f64) {
    let r = h.request("ui.pointer", json!({"events": [{"kind": kind, "x": x, "y": y}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

fn spots(h: &Headless) -> Vec<lightcraft_develop::Spot> {
    h.app.session.develop_of(h.app.session.active().expect("active photo")).unwrap_or_default().spots.clone()
}

#[test]
fn a_brushed_stroke_stores_points_at_a_spacing_and_ignores_a_held_pointer() {
    let mut h = remove_tool();
    pointer(&mut h, "down", 0.3, 0.5);
    // 40 frames of 0.002 of the photo's width each: about 1.4 px at this size, a point every r/4 = 8 px
    for i in 1..=40 {
        pointer(&mut h, "drag", 0.3 + 0.002 * f64::from(i), 0.5);
    }
    // held still for a while: no point per frame
    for _ in 0..15 {
        pointer(&mut h, "drag", 0.38, 0.5);
    }
    pointer(&mut h, "up", 0.38, 0.5);
    let s = spots(&h);
    assert_eq!(s.len(), 1, "one spot");
    let p = &s[0].points;
    assert!((3..=12).contains(&p.len()), "a stroke of ~64 px with a point every ~8 px is a handful of points, got {}", p.len());
    assert!((p[0].x - 0.3).abs() < 0.02, "starts where the button went down: {:?}", p[0]);
    let end = p[p.len() - 1];
    assert!((end.x - 0.38).abs() < 0.003 && (end.y - 0.5).abs() < 0.003, "reaches where the pointer was released: {end:?}");
}

#[test]
fn a_click_is_still_a_single_point_spot() {
    let mut h = remove_tool();
    pointer(&mut h, "down", 0.6, 0.4);
    pointer(&mut h, "up", 0.6, 0.4);
    let s = spots(&h);
    assert_eq!((s.len(), s[0].points.len()), (1, 1));
}

#[test]
fn a_stroke_is_grabbed_anywhere_along_it() {
    let mut h = remove_tool();
    let r = h.request(
        "engine.execute",
        json!({"command": "spot.add", "params": {"mode": "heal", "points": [[0.3, 0.5], [0.34, 0.5], [0.38, 0.5], [0.42, 0.5]], "size": 0.04, "source": [0.0, 0.2]}}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(spots(&h)[0].points.len(), 4);
    // the third point is 0.08 of the width (about 60 px) from the pin: grabbing there moves the spot, adds none
    pointer(&mut h, "move", 0.38, 0.5);
    pointer(&mut h, "down", 0.38, 0.5);
    pointer(&mut h, "drag", 0.38, 0.55);
    pointer(&mut h, "drag", 0.38, 0.6);
    pointer(&mut h, "up", 0.38, 0.6);
    let s = spots(&h);
    assert_eq!(s.len(), 1, "grabbed the stroke instead of painting a new spot");
    assert!((s[0].points[0].y - 0.6).abs() < 0.03 && (s[0].points[0].x - 0.3).abs() < 0.01, "{:?}", s[0].points);
    assert_eq!(s[0].points.len(), 4);
}

fn wheel(h: &mut Headless, dy: f32, modifiers: egui::Modifiers) {
    h.events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, dy),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers,
    });
    h.step();
}

#[test]
fn the_wheel_over_the_photo_sizes_and_feathers_the_brush_instead_of_zooming() {
    let mut h = remove_tool();
    let c = h.app.image_rect.expect("photo drawn").center();
    h.request("ui.move", json!({"x": c.x, "y": c.y}), T);
    h.step();
    let (zoom, size, feather) = (h.app.ui.zoom, h.app.ui.remove_size, h.app.ui.remove_feather);
    wheel(&mut h, 2.0, egui::Modifiers::NONE);
    for _ in 0..30 {
        h.step();
    }
    assert!((h.app.ui.remove_size / size - 1.21).abs() < 0.01, "two notches up: ×1.1², got {} from {size}", h.app.ui.remove_size);
    assert_eq!(h.app.ui.zoom, zoom, "the wheel doesn't zoom while the Remove tool is open");
    wheel(&mut h, -1.0, egui::Modifiers::SHIFT);
    assert_eq!(h.app.ui.remove_feather, feather - 5.0, "Shift+wheel feathers");
}

#[test]
fn a_smooth_scrolling_mouse_sizes_the_brush_too() {
    // macOS reports a Magic Mouse, a smooth-scrolling wheel and a trackpad in points, not lines
    let mut h = remove_tool();
    let c = h.app.image_rect.expect("photo drawn").center();
    h.request("ui.move", json!({"x": c.x, "y": c.y}), T);
    h.step();
    let (zoom, pan, size, feather) = (h.app.ui.zoom, h.app.ui.pan, h.app.ui.remove_size, h.app.ui.remove_feather);
    let line = h.view.ctx.options(|o| o.input_options.line_scroll_speed);
    for (y, modifiers) in [(line, egui::Modifiers::NONE), (line, egui::Modifiers::NONE), (-line, egui::Modifiers::SHIFT)] {
        h.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, y),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers,
        });
        h.step();
    }
    for _ in 0..30 {
        h.step();
    }
    assert!((h.app.ui.remove_size / size - 1.21).abs() < 0.01, "two lines' worth of points: ×1.1², got {} from {size}", h.app.ui.remove_size);
    assert_eq!(h.app.ui.remove_feather, feather - 5.0, "Shift+scroll feathers");
    assert_eq!((h.app.ui.zoom, h.app.ui.pan), (zoom, pan), "nor does it zoom or pan");
}

#[test]
fn the_wheel_sizes_the_spot_under_the_pointer_and_elsewhere_the_next_stroke() {
    let mut h = remove_tool();
    let r = h.request(
        "engine.execute",
        json!({"command": "spot.add", "params": {"mode": "heal", "points": [[0.3, 0.5]], "size": 0.04, "feather": 40.0, "source": [0.0, 0.2]}}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    // away from the spot just added (still selected): the brush changes, the spot doesn't
    pointer(&mut h, "move", 0.7, 0.5);
    let brush = h.app.ui.remove_size;
    wheel(&mut h, 1.0, egui::Modifiers::NONE);
    assert!((h.app.ui.remove_size / brush - 1.1).abs() < 0.01, "the next stroke's brush grows");
    assert_eq!(spots(&h)[0].size, 0.04, "the selected spot keeps its size");
    assert_eq!(h.app.session.active_spot, None, "and is let go, so the brush no longer shows its settings");
    h.step();
    assert!((h.app.ui.remove_size / brush - 1.1).abs() < 0.01, "the brush keeps its new size");
    // over the spot: that spot changes (and is selected again)
    pointer(&mut h, "move", 0.3, 0.5);
    wheel(&mut h, 1.0, egui::Modifiers::NONE);
    wheel(&mut h, -1.0, egui::Modifiers::SHIFT);
    let s = spots(&h);
    assert!((s[0].size / 0.04 - 1.1).abs() < 0.01, "the spot under the pointer grows, got {}", s[0].size);
    assert_eq!(s[0].feather, 35.0, "and Shift+wheel feathers it");
    assert_eq!(h.app.session.active_spot, Some(0));
}

#[test]
fn notches_on_consecutive_frames_all_count_and_undo_as_one_step() {
    let mut h = remove_tool();
    let r = h.request(
        "engine.execute",
        json!({"command": "spot.add", "params": {"mode": "heal", "points": [[0.3, 0.5]], "size": 0.04, "feather": 40.0, "source": [0.0, 0.2]}}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    pointer(&mut h, "move", 0.3, 0.5);
    // a spun wheel: a notch on each of four frames
    for _ in 0..4 {
        wheel(&mut h, 1.0, egui::Modifiers::NONE);
    }
    let grown = spots(&h)[0].size;
    assert!((grown / 0.04 - 1.1f64.powi(4)).abs() < 0.01, "four notches: ×1.1⁴, got {grown}");
    // the gesture ends off the spot; one undo takes back all of it
    pointer(&mut h, "move", 0.7, 0.5);
    assert!(h.app.session.interaction.is_none(), "the wheel gesture is committed");
    let r = h.request("engine.execute", json!({"command": "edit.undo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(spots(&h)[0].size, 0.04, "one undo step for the whole gesture");
}

#[test]
fn leaving_the_loupe_ends_a_wheel_gesture() {
    let mut h = remove_tool();
    let r = h.request(
        "engine.execute",
        json!({"command": "spot.add", "params": {"mode": "heal", "points": [[0.3, 0.5]], "size": 0.04, "source": [0.0, 0.2]}}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    pointer(&mut h, "move", 0.3, 0.5);
    wheel(&mut h, 1.0, egui::Modifiers::NONE);
    assert!(h.app.session.interaction.is_some(), "the wheel gesture is open");
    // straight to the grid, where the loupe (and its wheel handling) no longer draws
    let r = h.request("ui.set", json!({"view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.interaction.is_none(), "the wheel gesture is committed, so no later edit joins it");
    let r = h.request("engine.execute", json!({"command": "edit.undo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(spots(&h)[0].size, 0.04, "and is one undo step");
}

#[test]
fn the_wheel_sizes_the_spot_the_pointer_has_just_moved_onto() {
    let mut h = remove_tool();
    for x in [0.3, 0.7] {
        let r = h.request(
            "engine.execute",
            json!({"command": "spot.add", "params": {"mode": "heal", "points": [[x, 0.5]], "size": 0.04, "source": [0.0, 0.2]}}),
            T,
        );
        assert_eq!(r["ok"], true, "{r}");
    }
    pointer(&mut h, "move", 0.3, 0.5);
    // in one frame the pointer moves onto the other spot and the wheel turns
    let img = h.app.image_rect.expect("photo drawn");
    let to = egui::pos2(img.min.x + 0.7 * img.width(), img.min.y + 0.5 * img.height());
    h.events.push(egui::Event::PointerMoved(to));
    wheel(&mut h, 1.0, egui::Modifiers::NONE);
    let s = spots(&h);
    assert_eq!(s[0].size, 0.04, "the spot the pointer left keeps its size");
    assert!((s[1].size / 0.04 - 1.1).abs() < 0.01, "the spot under the pointer grows, got {}", s[1].size);
}

#[test]
fn a_spot_the_wheel_has_grown_is_grabbed_out_to_its_new_edge() {
    let mut h = remove_tool();
    let r = h.request(
        "engine.execute",
        json!({"command": "spot.add", "params": {"mode": "heal", "points": [[0.5, 0.5]], "size": 0.02, "feather": 0.0, "source": [0.0, 0.2]}}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    pointer(&mut h, "move", 0.5, 0.5);
    // grow it to twice its size, then, in the next frame, move to where only the grown spot reaches and turn on
    for _ in 0..8 {
        wheel(&mut h, 1.0, egui::Modifiers::NONE);
    }
    let grown = spots(&h)[0].size;
    assert!(grown > 0.04, "{grown}");
    let img = h.app.image_rect.expect("photo drawn");
    let long = img.width().max(img.height());
    // between the outline drawn before the last notch and the grown one (3 pt outside the radius, no feather)
    let between = (grown / 1.1 + grown) as f32 / 2.0 * long + 3.0;
    let to = egui::pos2(img.center().x + between, img.center().y);
    h.events.push(egui::Event::PointerMoved(to));
    wheel(&mut h, 1.0, egui::Modifiers::NONE);
    assert!((spots(&h)[0].size / grown - 1.1).abs() < 0.01, "the grown spot is still under the pointer: {}", spots(&h)[0].size);
    assert_eq!(h.app.session.active_spot, Some(0), "and stays selected (the brush isn't resized instead)");
}
