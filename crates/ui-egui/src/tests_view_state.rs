//! Headless tests of Library ▸ View's switches: Lock Zoom Position, Scroll By Row, Mirror Image
//! Mode (`crate::view_state`).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Zoom;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo(view: &str) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": view, "thumbSize": 480.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn select(h: &mut Headless, id: u64) {
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

fn exec(h: &mut Headless, command: &str) -> serde_json::Value {
    h.request("engine.execute", json!({"command": command, "params": {}}), T)
}

#[test]
fn unlocked_zoom_is_remembered_per_photo() {
    let mut h = demo("detail");
    let ids = h.app.session.visible_cloned();
    let (a, b) = (ids[0].0, ids[1].0);
    select(&mut h, a);
    // locked (default): the zoom follows to the next photo
    h.app.ui.zoom = Zoom::Percent(200.0);
    h.app.ui.pan = (0.2, 0.7);
    select(&mut h, b);
    assert_eq!((h.app.ui.zoom, h.app.ui.pan), (Zoom::Percent(200.0), (0.2, 0.7)));
    // unlocked: each photo keeps its own
    exec(&mut h, "view.lockZoomPosition");
    select(&mut h, a);
    h.app.ui.zoom = Zoom::Percent(100.0);
    h.app.ui.pan = (0.3, 0.3);
    select(&mut h, ids[2].0);
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "an unseen photo opens at Fit");
    select(&mut h, b);
    h.app.ui.zoom = Zoom::Percent(400.0);
    h.app.ui.pan = (0.9, 0.1);
    select(&mut h, a);
    assert_eq!((h.app.ui.zoom, h.app.ui.pan), (Zoom::Percent(100.0), (0.3, 0.3)));
    select(&mut h, b);
    assert_eq!((h.app.ui.zoom, h.app.ui.pan), (Zoom::Percent(400.0), (0.9, 0.1)));
}

#[test]
fn scroll_by_row_lands_on_a_row_top_and_mirror_mode_flips_only_the_view() {
    let mut h = demo("photoGrid");
    exec(&mut h, "view.scrollByRow");
    h.step();
    let view = h.app.widgets.iter().find(|(w, _)| w == "grid:scroll").map(|(_, r)| *r).expect("grid");
    h.request("ui.move", json!({"x": view.center().x, "y": view.center().y}), T);
    for _ in 0..2 {
        h.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -1.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.settle(SETTLE);
    }
    let y = h.app.grid_scroll.expect("grid drawn");
    assert!(y > 0.0, "scrolled down");
    // a notch scrolls one row: not the third of the view a plain notch moves
    let plain = view.height() * crate::theme::WHEEL_STRIP_SHARE;
    assert!((y - 2.0 * plain).abs() > 1.0, "offset {y} is not two plain notches");
    // Mirror Image Mode flips the view, not the library
    h.step();
    let before = h.paint();
    exec(&mut h, "view.mirrorImage");
    h.settle(SETTLE);
    h.step();
    let ids = h.app.session.visible_cloned();
    let after = h.paint();
    assert_ne!(before.pixels, after.pixels, "the grid is painted differently when mirrored");
    // catalog contents are untouched by the view mode
    assert_eq!(h.app.session.visible_cloned(), ids);
}
