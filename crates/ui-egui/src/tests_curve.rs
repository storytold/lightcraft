//! Headless tests of the point-curve editor: dragging points, resetting, presets.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// Detail view with the Edit panel's Curve flyout open on `channel`.
fn curve_open(channel: &str) -> Headless {
    curve_open_sized(channel, [1200.0, 1400.0])
}

fn curve_open_sized(channel: &str, size: [f32; 2]) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = crate::state::RightPanel::Edit;
    if !h.app.ui.section_open("light") {
        h.app.ui.toggle_section("light");
    }
    if !h.app.ui.flyout_open("curve") {
        h.app.ui.toggle_flyout("curve");
    }
    h.app.ui.curve_channel = channel.into();
    h.step();
    h.step();
    // a short window: scroll the Edit panel until the whole graph is on screen
    for _ in 0..40 {
        let r = curve_rect(&mut h);
        if r.bottom() <= size[1] - 8.0 {
            break;
        }
        h.input([egui::Event::PointerMoved(egui::pos2(r.center().x, size[1] / 2.0))]);
        let s = h.request("ui.scroll", json!({"dx": 0.0, "dy": -60.0}), T);
        assert_eq!(s["ok"], true, "{s}");
        h.step();
    }
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn develop(h: &Headless) -> lightcraft_develop::DevelopSettings {
    let id = h.app.session.active().expect("active photo");
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn pts(v: &[lightcraft_geom::Point]) -> Vec<(f64, f64)> {
    v.iter().map(|p| (p.x, p.y)).collect()
}

/// Side of the (square) curve graph in screen points.
fn curve_side(h: &mut Headless) -> f64 {
    let r = h.request("ui.widgets", json!({"filter": "curve"}), T);
    let w = r["result"].as_array().unwrap().iter().find(|w| w["id"] == "curve").cloned().unwrap_or_else(|| panic!("curve widget on screen: {r}"));
    w["rect"][2].as_f64().unwrap()
}

#[test]
fn dragging_a_point_moves_it_in_both_axes_with_one_undo_step() {
    let mut h = curve_open("master");
    exec(&mut h, "develop.curve", json!({"channel": "master", "points": [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]}));
    h.step();
    let side = curve_side(&mut h);
    let undo_before = h.app.session.undo.len();
    // grab the middle point and drag it up-left: x 0.5 → 0.4, y 0.5 → 0.7
    let r = h.request("ui.dragWidget", json!({"id": "curve", "fx": 0.5, "fy": 0.5, "dx": -0.1 * side, "dy": -0.2 * side, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let c = pts(&develop(&h).curve.master);
    assert_eq!(c.len(), 3, "dragging moves the point, it doesn't add one: {c:?}");
    assert!((c[1].0 - 0.4).abs() < 0.03, "x follows the pointer: {c:?}");
    assert!((c[1].1 - 0.7).abs() < 0.03, "y follows the pointer: {c:?}");
    assert_eq!(h.app.session.undo.len(), undo_before + 1, "one undo step per drag");
}

#[test]
fn alt_dragging_a_point_moves_it_a_tenth_as_far() {
    let mut h = curve_open("master");
    exec(&mut h, "develop.curve", json!({"channel": "master", "points": [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]}));
    h.step();
    let side = curve_side(&mut h);
    // the same drag as above with ⌥ held: x 0.5 → ~0.49, y 0.5 → ~0.52
    let r =
        h.request("ui.dragWidget", json!({"id": "curve", "fx": 0.5, "fy": 0.5, "dx": -0.1 * side, "dy": -0.2 * side, "steps": 12, "alt": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let c = pts(&develop(&h).curve.master);
    assert_eq!(c.len(), 3, "{c:?}");
    assert!(c[1].0 < 0.5 && c[1].0 > 0.485, "x moves a tenth of the pointer: {c:?}");
    assert!(c[1].1 > 0.51 && c[1].1 < 0.525, "y moves a tenth of the pointer: {c:?}");
}

fn click_widget(h: &mut Headless, id: &str, count: u64) {
    let r = h.request("ui.clickWidget", json!({"id": id, "count": count}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
}

const S_CURVE: [[f64; 2]; 4] = [[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]];

#[test]
fn double_clicking_a_channel_resets_only_that_channel() {
    let mut h = curve_open("master");
    exec(&mut h, "develop.curve", json!({"channel": "master", "points": S_CURVE}));
    exec(&mut h, "develop.curve", json!({"channel": "green", "points": S_CURVE}));
    h.step();
    click_widget(&mut h, "curveChannel:green", 2);
    let c = develop(&h).curve;
    assert!(c.green.is_empty(), "green resets: {:?}", c.green);
    assert_eq!(c.master.len(), 4, "the other channels stay");
    assert_eq!(h.app.ui.curve_channel, "green");
}

#[test]
fn reset_button_resets_every_curve() {
    let mut h = curve_open("red");
    for ch in ["master", "red", "blue"] {
        exec(&mut h, "develop.curve", json!({"channel": ch, "points": S_CURVE}));
    }
    exec(&mut h, "develop.set", json!({"control": "curve.highlights", "value": -40}));
    h.step();
    click_widget(&mut h, "button:curveReset", 1);
    let c = develop(&h).curve;
    assert_eq!(c, lightcraft_develop::ToneCurve::default());
}

#[test]
fn point_curve_preset_dropdown_applies_and_saves() {
    let mut h = curve_open("master");
    click_widget(&mut h, "dropdown:curvePreset", 1);
    click_widget(&mut h, "curvePreset:Medium Contrast", 1);
    let c = develop(&h).curve;
    assert_eq!(c.master.len(), 5, "the preset's S-curve: {:?}", c.master);
    assert!(c.master[1].y < 0.25 && c.master[3].y > 0.75);
    // shape it further, then save it from the dropdown (name prompt)
    exec(&mut h, "develop.curve", json!({"channel": "blue", "points": [[0.0, 0.08], [1.0, 1.0]]}));
    h.step();
    click_widget(&mut h, "dropdown:curvePreset", 1);
    click_widget(&mut h, "curvePresetMenu:save", 1);
    let mut dlg = h.app.ui.dialog.take().expect("a name prompt opens");
    match &mut dlg {
        crate::state::Dialog::TextPrompt { command, value, .. } => {
            assert_eq!(command, "curve.savePreset");
            *value = "Cool Shadows".into();
        }
        _ => panic!("a name prompt opens"),
    }
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg);
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(h.app.session.curve_presets.len(), 1);
    // Linear from the dropdown flattens every channel
    click_widget(&mut h, "dropdown:curvePreset", 1);
    click_widget(&mut h, "curvePreset:Linear", 1);
    let c = develop(&h).curve;
    assert!(c.master.is_empty() && c.blue.is_empty());
    click_widget(&mut h, "dropdown:curvePreset", 1);
    click_widget(&mut h, "curvePreset:Cool Shadows", 1);
    assert_eq!(develop(&h).curve.blue.len(), 2);
}

#[test]
fn dragging_a_point_past_its_neighbour_is_clamped() {
    let mut h = curve_open("red");
    exec(&mut h, "develop.curve", json!({"channel": "red", "points": [[0.0, 0.0], [0.3, 0.3], [0.6, 0.6], [1.0, 1.0]]}));
    h.step();
    let side = curve_side(&mut h);
    // drag the point at 0.3 far to the right (past 0.6)
    let r = h.request("ui.dragWidget", json!({"id": "curve", "fx": 0.3, "fy": 0.7, "dx": 0.5 * side, "dy": 0.0, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let c = pts(&develop(&h).curve.red);
    assert_eq!(c.len(), 4, "{c:?}");
    assert!(c[1].0 < c[2].0 && c[1].0 > 0.55, "clamped just left of its neighbour: {c:?}");
    assert_eq!(c[2], (0.6, 0.6), "the neighbour stays: {c:?}");
}

/// One finger from `from` to `to` in `steps` moves, then lifted, as egui-winit reports a touch.
fn touch_drag(h: &mut Headless, from: egui::Pos2, to: egui::Pos2, steps: usize) {
    let touch = |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: Some(0.5) };
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    h.input([touch(egui::TouchPhase::Start, from), egui::Event::PointerMoved(from), button(from, true)]);
    h.step();
    for i in 1..=steps {
        let p = from + (to - from) * (i as f32 / steps as f32);
        h.input([touch(egui::TouchPhase::Move, p), egui::Event::PointerMoved(p)]);
        h.step();
    }
    h.input([touch(egui::TouchPhase::End, to), button(to, false), egui::Event::PointerGone]);
    h.step();
    h.step();
}

fn curve_rect(h: &mut Headless) -> egui::Rect {
    let r = h.request("ui.widgets", json!({"filter": "curve"}), T);
    let w = r["result"].as_array().unwrap().iter().find(|w| w["id"] == "curve").cloned().unwrap();
    let v: Vec<f32> = (0..4).map(|i| w["rect"][i].as_f64().unwrap() as f32).collect();
    egui::Rect::from_min_size(egui::pos2(v[0], v[1]), egui::vec2(v[2], v[3]))
}

#[test]
fn touch_dragging_a_point_moves_it() {
    let mut h = curve_open("master");
    exec(&mut h, "develop.curve", json!({"channel": "master", "points": [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]}));
    h.step();
    let r = curve_rect(&mut h);
    let from = r.center();
    let to = from + egui::vec2(-0.1 * r.width(), -0.2 * r.height());
    touch_drag(&mut h, from, to, 12);
    let c = pts(&develop(&h).curve.master);
    assert_eq!(c.len(), 3, "{c:?}");
    assert!((c[1].0 - 0.4).abs() < 0.03 && (c[1].1 - 0.7).abs() < 0.03, "the point follows the finger: {c:?}");
}

#[test]
fn touch_dragging_a_point_moves_it_in_a_scrolling_panel() {
    // iPad 11" landscape: the Edit panel is taller than the window, so it scrolls by touch
    let mut h = curve_open_sized("master", [1194.0, 834.0]);
    exec(&mut h, "develop.curve", json!({"channel": "master", "points": [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]}));
    h.step();
    let r = curve_rect(&mut h);
    let from = r.center();
    let to = from + egui::vec2(-0.1 * r.width(), -0.2 * r.height());
    touch_drag(&mut h, from, to, 12);
    let c = pts(&develop(&h).curve.master);
    assert_eq!(c.len(), 3, "{c:?}");
    assert!((c[1].0 - 0.4).abs() < 0.03 && (c[1].1 - 0.7).abs() < 0.03, "the point follows the finger: {c:?}");
}

fn widget_rect(h: &mut Headless, id: &str) -> egui::Rect {
    let r = h.request("ui.widgets", json!({"filter": id}), T);
    let w = r["result"].as_array().unwrap().iter().find(|w| w["id"] == id).cloned().unwrap_or_else(|| panic!("{id} on screen: {r}"));
    let v: Vec<f32> = (0..4).map(|i| w["rect"][i].as_f64().unwrap() as f32).collect();
    egui::Rect::from_min_size(egui::pos2(v[0], v[1]), egui::vec2(v[2], v[3]))
}

/// A point at fractions (`fx` across, `fy` up from the bottom) of the curve graph.
fn at(r: egui::Rect, fx: f32, fy: f32) -> egui::Pos2 {
    egui::pos2(r.left() + fx * r.width(), r.bottom() - fy * r.height())
}

#[test]
fn touch_dragging_the_parametric_graph_adjusts_the_region_under_the_finger() {
    let mut h = curve_open_sized("parametric", [1194.0, 834.0]);
    let r = curve_rect(&mut h);
    let undo_before = h.app.session.undo.len();
    // the right end (highlights) dragged up by a quarter of the graph: +50
    touch_drag(&mut h, at(r, 0.9, 0.5), at(r, 0.9, 0.75), 10);
    let c = develop(&h).curve;
    assert!((c.highlights - 50.0).abs() <= 2.0, "highlights follow the drag: {c:?}");
    assert_eq!((c.lights, c.darks, c.shadows), (0.0, 0.0, 0.0), "only that region: {c:?}");
    assert_eq!(h.app.session.undo.len(), undo_before + 1, "one undo step per drag");
    // the left end (shadows) dragged down, past the bottom: clamped at −100
    touch_drag(&mut h, at(r, 0.1, 0.5), at(r, 0.1, -0.4), 10);
    assert_eq!(develop(&h).curve.shadows, -100.0);
    // darks (between the shadows and midtones splits) by mouse
    let r2 = h.request("ui.dragWidget", json!({"id": "curve", "fx": 0.4, "fy": 0.5, "dx": 0.0, "dy": -0.1 * r.height(), "steps": 8}), T);
    assert_eq!(r2["ok"], true, "{r2}");
    h.step();
    assert!((develop(&h).curve.darks - 20.0).abs() <= 2.0, "{:?}", develop(&h).curve);
}

#[test]
fn dragging_a_split_handle_moves_the_split_between_its_neighbours() {
    let mut h = curve_open("parametric");
    let r = curve_rect(&mut h);
    let mid = widget_rect(&mut h, "curveSplit:mid");
    // midtones split 50 → 60
    touch_drag(&mut h, mid.center(), mid.center() + egui::vec2(0.1 * r.width(), 0.0), 8);
    assert!((develop(&h).curve.split_mid - 60.0).abs() <= 1.0, "{:?}", develop(&h).curve);
    // the shadows split can't pass the midtones split
    let sh = widget_rect(&mut h, "curveSplit:shadows");
    touch_drag(&mut h, sh.center(), sh.center() + egui::vec2(0.6 * r.width(), 0.0), 8);
    let c = develop(&h).curve;
    assert!(c.split_shadows < c.split_mid, "{c:?}");
    // double-tap resets
    click_widget(&mut h, "curveSplit:mid", 2);
    assert_eq!(develop(&h).curve.split_mid, 50.0);
}

/// A targeted-adjustment tool takes over clicks and drags on the photo, so turning it on says so
/// (and how to leave), and Esc leaves it: one left on by mistake looked like a frozen canvas.
#[test]
fn targeted_adjustment_announces_itself_and_esc_turns_it_off() {
    let mut h = curve_open("master");
    let r = h.request("ui.clickWidget", json!({"id": "icon:tat-curve"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.tool, "tat:curve");
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("Esc")), "{:?}", h.app.ui.toast);
    let r = h.request("ui.key", json!({"key": "esc"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.ui.tool.is_empty(), "Esc turns the tool off: {:?}", h.app.ui.tool);
    assert!(matches!(h.app.ui.view, crate::state::ViewMode::Detail), "the first Esc only leaves the tool");
}

/// Dragging a point whose neighbours are closer than the editor's 0.02 spacing (two clicks side
/// by side, or a curve from a sidecar or an agent) clamped it between bounds that had crossed,
/// which panics: the app went down mid-drag. The point now stays between its neighbours.
#[test]
fn dragging_a_point_between_close_neighbours_does_not_panic() {
    let mut h = curve_open("master");
    let close = [[0.0, 0.0], [0.5, 0.5], [0.508, 0.6], [0.515, 0.3], [1.0, 1.0]];
    exec(&mut h, "develop.curve", json!({"channel": "master", "points": close}));
    h.step();
    let side = curve_side(&mut h);
    let r = h.request("ui.dragWidget", json!({"id": "curve", "fx": 0.508, "fy": 0.4, "dx": 0.05 * side, "dy": -0.1 * side, "steps": 8}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let c = pts(&develop(&h).curve.master);
    assert_eq!(c.len(), 5, "{c:?}");
    assert!(c.windows(2).all(|w| w[0].0 <= w[1].0), "the points stay in order: {c:?}");
    assert!(c[2].0 > 0.5 && c[2].0 < 0.515, "the dragged point stays between its neighbours: {c:?}");
}

/// Points an agent sends out of order are stored in the order the curve runs, so the editor's
/// neighbours are the curve's.
#[test]
fn curve_points_from_an_agent_are_stored_in_order() {
    let mut h = curve_open("green");
    exec(&mut h, "develop.curve", json!({"channel": "green", "points": [[0.0, 0.0], [0.7, 0.8], [0.3, 0.2], [1.0, 1.0]]}));
    h.step();
    assert_eq!(pts(&develop(&h).curve.green), vec![(0.0, 0.0), (0.3, 0.2), (0.7, 0.8), (1.0, 1.0)]);
    let side = curve_side(&mut h);
    let r = h.request("ui.dragWidget", json!({"id": "curve", "fx": 0.3, "fy": 0.8, "dx": 0.6 * side, "dy": 0.0, "steps": 8}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let c = pts(&develop(&h).curve.green);
    assert!(c[1].0 < c[2].0, "dragged up to its neighbour, not past it: {c:?}");
}
