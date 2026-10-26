use egui::{Event, TouchDeviceId, TouchId, TouchPhase, pos2};
use serde_json::json;
use std::time::Duration;

use crate::headless::Headless;
use crate::state::Zoom;
use crate::touch::{TouchAction, TouchGestureTracker};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 2400.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

#[test]
fn touch_tracker_two_finger_tap_undo() {
    let mut tracker = TouchGestureTracker::new();
    let dev = TouchDeviceId(1);

    // Frame 1: Finger 1 touches down
    let t0 = 10.0;
    let events = vec![Event::Touch { device_id: dev, id: TouchId(1), phase: TouchPhase::Start, pos: pos2(100.0, 100.0), force: None }];
    assert_eq!(tracker.process_events(t0, &events), None);

    // Frame 2: Finger 2 touches down
    let events = vec![Event::Touch { device_id: dev, id: TouchId(2), phase: TouchPhase::Start, pos: pos2(180.0, 100.0), force: None }];
    assert_eq!(tracker.process_events(t0 + 0.05, &events), None);

    // Frame 3: Both fingers lift within 0.15s without moving
    let events = vec![
        Event::Touch { device_id: dev, id: TouchId(1), phase: TouchPhase::End, pos: pos2(101.0, 100.0), force: None },
        Event::Touch { device_id: dev, id: TouchId(2), phase: TouchPhase::End, pos: pos2(180.0, 101.0), force: None },
    ];
    let action = tracker.process_events(t0 + 0.15, &events);
    assert_eq!(action, Some(TouchAction::Undo));
}

#[test]
fn touch_tracker_three_finger_tap_redo() {
    let mut tracker = TouchGestureTracker::new();
    let dev = TouchDeviceId(1);

    let t0 = 20.0;
    // Frame 1: Three fingers touch down
    let events = vec![
        Event::Touch { device_id: dev, id: TouchId(10), phase: TouchPhase::Start, pos: pos2(100.0, 100.0), force: None },
        Event::Touch { device_id: dev, id: TouchId(11), phase: TouchPhase::Start, pos: pos2(150.0, 100.0), force: None },
        Event::Touch { device_id: dev, id: TouchId(12), phase: TouchPhase::Start, pos: pos2(200.0, 100.0), force: None },
    ];
    assert_eq!(tracker.process_events(t0, &events), None);

    // Frame 2: Three fingers lift
    let events = vec![
        Event::Touch { device_id: dev, id: TouchId(10), phase: TouchPhase::End, pos: pos2(100.0, 102.0), force: None },
        Event::Touch { device_id: dev, id: TouchId(11), phase: TouchPhase::End, pos: pos2(151.0, 100.0), force: None },
        Event::Touch { device_id: dev, id: TouchId(12), phase: TouchPhase::End, pos: pos2(200.0, 101.0), force: None },
    ];
    let action = tracker.process_events(t0 + 0.18, &events);
    assert_eq!(action, Some(TouchAction::Redo));
}

#[test]
fn touch_tracker_drag_movement_disqualifies_tap() {
    let mut tracker = TouchGestureTracker::new();
    let dev = TouchDeviceId(1);

    let t0 = 30.0;
    // Two fingers touch down
    let events = vec![
        Event::Touch { device_id: dev, id: TouchId(1), phase: TouchPhase::Start, pos: pos2(100.0, 100.0), force: None },
        Event::Touch { device_id: dev, id: TouchId(2), phase: TouchPhase::Start, pos: pos2(180.0, 100.0), force: None },
    ];
    assert_eq!(tracker.process_events(t0, &events), None);

    // Fingers drag (e.g. pinch zoom or two-finger pan) > 30 pt
    let events = vec![
        Event::Touch { device_id: dev, id: TouchId(1), phase: TouchPhase::Move, pos: pos2(180.0, 100.0), force: None },
        Event::Touch { device_id: dev, id: TouchId(2), phase: TouchPhase::Move, pos: pos2(260.0, 100.0), force: None },
    ];
    assert_eq!(tracker.process_events(t0 + 0.1, &events), None);

    // Fingers lift
    let events = vec![
        Event::Touch { device_id: dev, id: TouchId(1), phase: TouchPhase::End, pos: pos2(180.0, 100.0), force: None },
        Event::Touch { device_id: dev, id: TouchId(2), phase: TouchPhase::End, pos: pos2(260.0, 100.0), force: None },
    ];
    assert_eq!(tracker.process_events(t0 + 0.2, &events), None);
}

#[test]
fn input_two_finger_and_three_finger_commands_execute() {
    let mut h = demo();

    let id = h.app.session.active().unwrap();

    // 1. Make an edit
    h.app.run("develop.set", json!({"control": "light.exposure", "value": 1.5})).unwrap();
    assert_eq!(h.app.session.catalog.photo(id).unwrap().develop.light.exposure, 1.5);

    // 2. Execute input.twoFingerTap -> should Undo
    h.app.run("input.twoFingerTap", json!({})).unwrap();
    assert_eq!(h.app.session.catalog.photo(id).unwrap().develop.light.exposure, 0.0);

    // 3. Execute input.threeFingerTap -> should Redo
    h.app.run("input.threeFingerTap", json!({})).unwrap();
    assert_eq!(h.app.session.catalog.photo(id).unwrap().develop.light.exposure, 1.5);
}

#[test]
fn apple_pencil_double_tap_toggles_zoom() {
    let mut h = demo();

    assert_eq!(h.app.ui.zoom, Zoom::Fit);

    // Double-tap Apple Pencil -> switches to 100% zoom
    h.app.run("input.pencilDoubleTap", json!({})).unwrap();
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));
    assert!(h.app.ui.zoom_anim);

    // Double-tap Apple Pencil again -> switches back to Fit
    h.app.run("input.pencilDoubleTap", json!({})).unwrap();
    assert_eq!(h.app.ui.zoom, Zoom::Fit);
}

#[test]
fn canvas_press_and_hold_state_and_badge() {
    let mut h = demo();

    // Set hold state
    h.app.ui.canvas_hold_before = true;
    h.step();

    // Verify beforeHold badge is rendered on canvas
    assert!(h.app.widgets.iter().any(|(id, _)| id == "loupe:beforeHold"), "loupe:beforeHold badge should be present during hold");

    // Release hold
    h.app.ui.canvas_hold_before = false;
    h.step();
    assert!(!h.app.widgets.iter().any(|(id, _)| id == "loupe:beforeHold"), "loupe:beforeHold badge should disappear on release");
}
