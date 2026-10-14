//! Headless tests of the Color Grading flyout (issue #737): the 3-Way / Shadows / Midtones /
//! Highlights tabs, the equal-size wheels, and each wheel's eye that hides its effect for viewing.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 2400.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    if !h.app.ui.section_open("color") {
        h.app.ui.toggle_section("color");
    }
    if !h.app.ui.flyout_open("grading") {
        h.app.ui.toggle_flyout("grading");
    }
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
fn three_way_is_the_default_with_three_equal_wheels() {
    let h = demo();
    assert_eq!(h.app.ui.grading_mode, "3way");
    let sizes: Vec<f32> = ["shadows", "midtones", "highlights"].iter().map(|k| widget(&h, &format!("wheel:{k}")).unwrap().width()).collect();
    assert!(sizes.iter().all(|w| (w - sizes[0]).abs() < 0.5), "{sizes:?}");
    for tab in ["3way", "shadows", "midtones", "highlights"] {
        assert!(widget(&h, &format!("icon:grading-{tab}")).is_some(), "tab {tab}");
    }
}

#[test]
fn a_range_tab_shows_one_bigger_wheel_and_its_hue_and_saturation() {
    let mut h = demo();
    let small = widget(&h, "wheel:shadows").unwrap().width();
    click(&mut h, "icon:grading-shadows");
    assert_eq!(h.app.ui.grading_mode, "shadows");
    assert!(widget(&h, "wheel:shadows").unwrap().width() > small);
    assert!(widget(&h, "wheel:midtones").is_none() && widget(&h, "wheel:highlights").is_none());
    for c in ["grading.shadows.hue", "grading.shadows.sat", "grading.shadows.lum", "grading.blending", "grading.balance"] {
        assert!(h.app.widgets.iter().any(|(w, _)| w.contains(c)), "slider {c}");
    }
    assert!(!h.app.widgets.iter().any(|(w, _)| w.contains("grading.midtones.lum")));
}

#[test]
fn the_eye_hides_a_wheel_for_viewing_only_and_resets_on_another_photo() {
    let mut h = demo();
    let id = h.app.session.active().unwrap();
    h.app.session.execute("develop.set", &json!({"values": {"grading.midtones.sat": 50}})).unwrap();
    h.step();
    let r = h.request("ui.hoverWidget", json!({"id": "wheel:midtones"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    click(&mut h, "wheelEye:midtones");
    assert_eq!(h.app.ui.grading_hidden_for(id), [false, true, false]);
    // not an edit
    assert_eq!(h.app.session.catalog.photo(id).unwrap().develop.grading.midtones.sat, 50.0);
    // the eye stays on screen while the wheel is hidden, and shows it again
    h.request("ui.move", json!({"x": 5.0, "y": 5.0}), T);
    h.step();
    assert!(widget(&h, "wheelEye:midtones").is_some());
    click(&mut h, "wheelEye:midtones");
    assert_eq!(h.app.ui.grading_hidden_for(id), [false; 3]);
    // hidden state belongs to one photo
    h.app.ui.set_grading_hidden(id, 0, true);
    let other = lightcraft_catalog::PhotoId(id.0 + 1);
    assert_eq!(h.app.ui.grading_hidden_for(other), [false; 3]);
}

#[test]
fn hovering_a_three_way_wheel_shows_its_hue_and_saturation() {
    let mut h = demo();
    h.app.session.execute("develop.set", &json!({"values": {"grading.shadows.hue": 220, "grading.shadows.sat": 45}})).unwrap();
    h.step();
    let readout = "H 220°  S 45";
    assert!(!h.painted_text().iter().any(|t| t == readout), "hidden until hovered");
    let r = h.request("ui.hoverWidget", json!({"id": "wheel:shadows"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.painted_text().iter().any(|t| t == readout), "{:?}", h.painted_text());
}

#[test]
fn wheel_pick_reads_the_angle_and_distance_and_honours_a_lock() {
    use crate::panels::edit::{WheelLock, wheel_pick};
    let close = |a: f32, b: f32| (a - b).abs() < 0.01;
    // free: hue from the angle (screen y points down), saturation from the distance
    let (h, s) = wheel_pick(egui::vec2(0.0, -20.0), 40.0, None);
    assert!(close(h, 90.0) && close(s, 50.0), "{h} {s}");
    // hue locked at 0°: a rough drag up and right keeps the hue; only the part along the spoke counts
    let (h, s) = wheel_pick(egui::vec2(30.0, -12.0), 40.0, Some(WheelLock::Hue(0.0)));
    assert!(close(h, 0.0) && close(s, 75.0), "{h} {s}");
    // past the centre or the rim: clamped to 0..100
    assert_eq!(wheel_pick(egui::vec2(-10.0, 0.0), 40.0, Some(WheelLock::Hue(0.0))).1, 0.0);
    assert_eq!(wheel_pick(egui::vec2(90.0, 0.0), 40.0, Some(WheelLock::Hue(0.0))).1, 100.0);
    assert_eq!(wheel_pick(egui::vec2(90.0, 0.0), 40.0, None).1, 100.0);
    // saturation locked: the hue follows the pointer's angle whatever its distance
    let (h, s) = wheel_pick(egui::vec2(0.0, -39.0), 40.0, Some(WheelLock::Sat(25.0)));
    assert!(close(h, 90.0) && close(s, 25.0), "{h} {s}");
    let (h, s) = wheel_pick(egui::vec2(-3.0, 0.0), 40.0, Some(WheelLock::Sat(25.0)));
    assert!(close(h, 180.0) && close(s, 25.0), "{h} {s}");
    // a degenerate radius doesn't divide by zero
    assert!(wheel_pick(egui::vec2(1.0, 1.0), 0.0, None).1.is_finite());
}

#[test]
fn wheel_pick_never_returns_nan_or_inf() {
    use crate::panels::edit::{WheelLock, wheel_pick};
    let bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
    let locks = [None, Some(WheelLock::Hue(f32::NAN)), Some(WheelLock::Hue(400.0)), Some(WheelLock::Sat(f32::NAN)), Some(WheelLock::Sat(250.0))];
    for lock in locks {
        for x in bad {
            for rad in [0.0, -5.0, f32::NAN, 40.0] {
                for v in [egui::vec2(x, 3.0), egui::vec2(3.0, x), egui::vec2(x, x), egui::vec2(0.0, 0.0)] {
                    let (h, s) = wheel_pick(v, rad, lock);
                    assert!(h.is_finite() && s.is_finite() && (0.0..=100.0).contains(&s), "{v:?} {rad} {lock:?} -> {h} {s}");
                }
            }
        }
    }
    // at the exact centre a saturation lock still answers
    assert_eq!(wheel_pick(egui::Vec2::ZERO, 40.0, Some(WheelLock::Sat(30.0))).1, 30.0);
}

#[test]
fn a_grab_keeps_the_pucks_own_hue_not_the_pointers_angle() {
    use crate::panels::edit::{WheelGrab, WheelLock, WheelMods};
    let shift = WheelMods { keep_hue: true, ..Default::default() };
    // the puck is at 100° / 60 %, the pointer grabs it at 10°: the locked hue is 100°
    let g = WheelGrab::new(shift, 100.0, 60.0, egui::vec2(30.0, -5.0), 40.0);
    assert_eq!(g.lock(), Some(WheelLock::Hue(100.0)));
    // a puck at the centre has no hue yet: the pointer's angle at the grab stands in for it
    let g = WheelGrab::new(shift, 100.0, 0.0, egui::vec2(0.0, -20.0), 40.0);
    match g.lock() {
        Some(WheelLock::Hue(h)) => assert!((h - 90.0).abs() < 0.01, "{h}"),
        other => panic!("{other:?}"),
    }
    // Cmd/Ctrl keeps the puck's saturation; Shift wins when both are down
    let m = crate::panels::edit::WheelMods::from_modifiers(egui::Modifiers { shift: true, command: true, ..Default::default() });
    assert!(m.keep_hue && !m.keep_sat);
    let m = crate::panels::edit::WheelMods::from_modifiers(egui::Modifiers { command: true, ..Default::default() });
    assert_eq!(WheelGrab::new(m, 100.0, 60.0, egui::Vec2::ZERO, 40.0).lock(), Some(WheelLock::Sat(60.0)));
    // nothing held: free
    assert_eq!(WheelGrab::new(WheelMods::default(), 100.0, 60.0, egui::Vec2::ZERO, 40.0).lock(), None);
}

#[test]
fn a_fine_grab_moves_the_puck_a_tenth_as_far_as_the_pointer() {
    use crate::panels::edit::{WheelGrab, WheelMods};
    let fine = WheelMods { fine: true, ..Default::default() };
    // the puck at 0° / 50 % on a wheel of radius 40 sits 20 right of the centre; the pointer
    // grabs it there and moves 10 up: the puck moves 1 up (hue ≈ 2.9°), not 10
    let g = WheelGrab::new(fine, 0.0, 50.0, egui::vec2(20.0, 0.0), 40.0);
    let (h, s) = g.pick(egui::vec2(20.0, -10.0), 40.0);
    assert!((h - (1.0f32 / 20.0).atan().to_degrees()).abs() < 0.01 && (s - 50.0 * (1.0 + 0.0025f32).sqrt()).abs() < 0.1, "{h} {s}");
    // with no travel the puck stays put, wherever the pointer grabbed it
    let g = WheelGrab::new(fine, 120.0, 30.0, egui::vec2(-5.0, 5.0), 40.0);
    let (h, s) = g.pick(egui::vec2(-5.0, 5.0), 40.0);
    assert!((h - 120.0).abs() < 0.01 && (s - 30.0).abs() < 0.01, "{h} {s}");
}

/// A wheel's size on screen: its radius.
fn wheel_radius(h: &Headless, key: &str) -> f32 {
    widget(h, &format!("wheel:{key}")).unwrap().width() / 2.0
}

/// Where a puck at `hue` / `sat` sits, from the centre of a wheel of radius `rad`.
fn puck(hue: f64, sat: f64, rad: f32) -> egui::Vec2 {
    let a = (hue as f32).to_radians();
    egui::vec2(a.cos(), -a.sin()) * rad * (sat as f32 / 100.0)
}

fn wheel_value(h: &Headless, key: &str) -> (f64, f64) {
    let id = h.app.session.active().unwrap();
    let g = h.app.session.catalog.photo(id).unwrap().develop.grading;
    let w = match key {
        "shadows" => g.shadows,
        "midtones" => g.midtones,
        _ => g.highlights,
    };
    (w.hue, w.sat)
}

fn set_wheel(h: &mut Headless, key: &str, hue: f64, sat: f64) {
    h.app.session.execute("develop.set", &json!({"values": {format!("grading.{key}.hue"): hue, format!("grading.{key}.sat"): sat}})).unwrap();
    h.step();
    h.step();
}

/// Drag the midtones wheel from `from` (an offset from its centre) by `by`, holding `mods`
/// (`shift`, `cmd`, `alt`); the puck's new hue and saturation, and how many undo steps it took.
fn drag_wheel(h: &mut Headless, from: egui::Vec2, by: egui::Vec2, mods: &[&str]) -> ((f64, f64), usize) {
    let w = widget(h, "wheel:midtones").unwrap().width();
    let mut req = json!({"id": "wheel:midtones", "fx": 0.5 + from.x / w, "fy": 0.5 + from.y / w, "dx": by.x, "dy": by.y, "steps": 12});
    for m in mods {
        req[*m] = json!(true);
    }
    let undo = h.app.session.undo.len();
    let r = h.request("ui.dragWidget", req, T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    (wheel_value(h, "midtones"), h.app.session.undo.len() - undo)
}

/// The distance a puck moved, in points.
fn travelled(rad: f32, a: (f64, f64), b: (f64, f64)) -> f32 {
    (puck(b.0, b.1, rad) - puck(a.0, a.1, rad)).length()
}

#[test]
fn shift_dragging_across_the_tangent_keeps_the_hue_and_moves_saturation() {
    let mut h = demo();
    let rad = wheel_radius(&h, "midtones");
    set_wheel(&mut h, "midtones", 30.0, 50.0);
    let a = 30f32.to_radians();
    let radial = egui::vec2(a.cos(), -a.sin());
    let tangent = egui::vec2(-a.sin(), -a.cos());
    // grab the puck and go 10 points outward and 15 round the wheel (rad 40: +25 % saturation)
    let by = (radial * 10.0 + tangent * 15.0) * (rad / 40.0);
    let ((hue, sat), undo) = drag_wheel(&mut h, puck(30.0, 50.0, rad), by, &["shift"]);
    assert!((hue - 30.0).abs() <= 1.0, "hue stays: {hue}");
    assert!((sat - 75.0).abs() <= 1.5, "saturation follows the radial part: {sat}");
    assert_eq!(undo, 1, "one undo step per drag");
    // the same drag unconstrained does swing the hue round, so the lock is what held it
    set_wheel(&mut h, "midtones", 30.0, 50.0);
    let ((hue, _), undo) = drag_wheel(&mut h, puck(30.0, 50.0, rad), by, &[]);
    assert!(hue > 50.0, "unlocked, the hue follows the pointer: {hue}");
    assert_eq!(undo, 1);
}

#[test]
fn shift_drag_keeps_the_pucks_hue_wherever_the_pointer_grabs() {
    let mut h = demo();
    let rad = wheel_radius(&h, "midtones");
    // the puck is at 30°, the pointer grabs the wheel far off that angle (at 120°, and at 250°):
    // the hue stays the puck's own, it doesn't take the angle under the pointer
    for grab_angle in [120.0f64, 250.0] {
        set_wheel(&mut h, "midtones", 30.0, 50.0);
        let from = puck(grab_angle, 60.0, rad);
        let ((hue, sat), undo) = drag_wheel(&mut h, from, egui::vec2(8.0, -6.0) * (rad / 40.0), &["shift"]);
        assert!((hue - 30.0).abs() <= 1.0, "grabbed at {grab_angle}°, the hue is {hue}");
        let a = 30f32.to_radians();
        let end = from + egui::vec2(8.0, -6.0) * (rad / 40.0);
        let along = (end.x * a.cos() - end.y * a.sin()) / rad * 100.0;
        assert!((sat as f32 - along.clamp(0.0, 100.0)).abs() <= 1.5, "saturation is the pointer's part along the spoke: {sat} vs {along}");
        assert_eq!(undo, 1);
    }
}

#[test]
fn shift_drag_from_the_neutral_centre_locks_the_hue_where_the_pointer_grabbed() {
    let mut h = demo();
    let rad = wheel_radius(&h, "midtones");
    set_wheel(&mut h, "midtones", 0.0, 0.0);
    // no hue yet: the angle of the grab (90°, straight up) stands in for it
    let ((hue, sat), undo) = drag_wheel(&mut h, puck(90.0, 50.0, rad), egui::vec2(6.0, -10.0) * (rad / 40.0), &["shift"]);
    assert!((hue - 90.0).abs() <= 1.0, "{hue}");
    assert!((sat - 75.0).abs() <= 1.5, "{sat}");
    assert_eq!(undo, 1);
}

#[test]
fn cmd_drag_keeps_the_saturation_and_changes_only_the_hue() {
    let mut h = demo();
    let rad = wheel_radius(&h, "midtones");
    set_wheel(&mut h, "midtones", 30.0, 50.0);
    // drag the puck out to 200° at 90 % of the radius: the hue follows, the saturation stays
    let from = puck(30.0, 50.0, rad);
    let to = puck(200.0, 90.0, rad);
    let ((hue, sat), undo) = drag_wheel(&mut h, from, to - from, &["cmd"]);
    assert!((hue - 200.0).abs() <= 1.0, "{hue}");
    assert_eq!(sat, 50.0, "the saturation is held");
    assert_eq!(undo, 1, "one undo step per drag");
    // and the same drag without Cmd takes the pointer's saturation too
    set_wheel(&mut h, "midtones", 30.0, 50.0);
    let ((_, sat), _) = drag_wheel(&mut h, from, to - from, &[]);
    assert!((sat - 90.0).abs() <= 1.5, "{sat}");
}

#[test]
fn alt_drag_fine_adjusts_to_a_tenth_of_the_pointers_travel() {
    let mut h = demo();
    let rad = wheel_radius(&h, "midtones");
    let start = (30.0, 50.0);
    let from = puck(start.0, start.1, rad);
    let by = egui::vec2(20.0, 0.0) * (rad / 40.0);
    set_wheel(&mut h, "midtones", start.0, start.1);
    let (free, undo_free) = drag_wheel(&mut h, from, by, &[]);
    set_wheel(&mut h, "midtones", start.0, start.1);
    let (fine, undo_fine) = drag_wheel(&mut h, from, by, &["alt"]);
    let (free_move, fine_move) = (travelled(rad, start, free), travelled(rad, start, fine));
    let expect = by.length() * 0.1;
    assert!(free_move > by.length() * 0.7, "unmodified: the puck follows the pointer ({free_move})");
    assert!((fine_move - expect).abs() <= 1.5 * (rad / 40.0), "Alt: a tenth of {} is {expect}, moved {fine_move}", by.length());
    assert!(fine_move < free_move / 5.0);
    assert_eq!((undo_free, undo_fine), (1, 1));
}

#[test]
fn alt_works_with_shift_and_cmd_constraints() {
    let mut h = demo();
    let rad = wheel_radius(&h, "midtones");
    let from = puck(30.0, 50.0, rad);
    let a = 30f32.to_radians();
    // 20 points outward along the spoke, then 20 round the wheel
    let radial = egui::vec2(a.cos(), -a.sin()) * 20.0 * (rad / 40.0);
    let tangent = egui::vec2(-a.sin(), -a.cos()) * 20.0 * (rad / 40.0);
    set_wheel(&mut h, "midtones", 30.0, 50.0);
    let ((hue, sat), undo) = drag_wheel(&mut h, from, radial, &["shift", "alt"]);
    assert!((hue - 30.0).abs() <= 1.0, "{hue}");
    assert!((sat - 55.0).abs() <= 1.5, "20 points is 50 %, a tenth of it is 5: {sat}");
    assert_eq!(undo, 1);
    set_wheel(&mut h, "midtones", 30.0, 50.0);
    let ((hue, sat), undo) = drag_wheel(&mut h, from, tangent, &["cmd", "alt"]);
    assert_eq!(sat, 50.0);
    // 2 points round a circle 20 points from the centre: 0.1 rad ≈ 5.7°
    assert!((hue - 35.7).abs() <= 1.5, "{hue}");
    assert_eq!(undo, 1);
}

#[test]
fn double_click_still_resets_the_wheel_with_a_modifier_held() {
    let mut h = demo();
    set_wheel(&mut h, "midtones", 30.0, 50.0);
    let r = h.request("ui.clickWidget", json!({"id": "wheel:midtones", "count": 2, "shift": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(wheel_value(&h, "midtones"), (0.0, 0.0));
}
