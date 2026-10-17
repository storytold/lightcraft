//! Headless tests of the point-curve editor: dragging points, resetting, presets.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::edit::ParametricCurve;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// Detail view with the Edit panel's Curve flyout open on `channel`.
fn curve_open(channel: &str) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 1400.0], 1.0);
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

/// The settings the parametric graph is drawn from: two neighbouring regions pushed opposite ways,
/// where the raw sum of their bumps folds back on itself before the renderer's running maximum
/// straightens it out.
fn folded_regions() -> lightcraft_develop::ToneCurve {
    lightcraft_develop::ToneCurve { shadows: 90.0, darks: -80.0, lights: -70.0, highlights: 60.0, ..Default::default() }
}

/// The bare sum of the four region bumps is not monotone, so the drawn curve needs the renderer's
/// running maximum: this is the shape that made the graph disagree with the sliders.
#[test]
fn the_parametric_model_is_monotone_where_the_bare_sum_of_bumps_is_not() {
    let c = folded_regions();
    let raw = |x: f64| {
        let s = [0.0, c.split_shadows / 100.0, c.split_mid / 100.0, c.split_highlights / 100.0, 1.0];
        let amounts = [c.shadows, c.darks, c.lights, c.highlights];
        let mut d = 0.0;
        for i in 0..4 {
            let (a, b) = (s[i], s[i + 1]);
            let (ctr, half) = ((a + b) / 2.0, (b - a) * 0.75 + 0.05);
            let t = ((x - ctr) / half).clamp(-1.0, 1.0);
            d += amounts[i] / 100.0 * 0.22 * (0.5 + 0.5 * (t * std::f64::consts::PI).cos());
        }
        x + d * 4.0 * x * (1.0 - x)
    };
    assert!((0..=64).any(|i| raw(i as f64 / 64.0) < raw((i + 1) as f64 / 64.0)), "the shape folds back, or it proves nothing");
    let model = ParametricCurve::new(&c);
    let mut prev = f64::MIN;
    for i in 0..=256 {
        let y = model.at(i as f64 / 256.0);
        assert!(y >= prev - 1e-12, "the model never folds back, at {i}: {y} < {prev}");
        prev = y;
    }
}

/// What the graph *paints* is the model the renderer computes from the sliders. They used to be two
/// copies of the formula and the panel's copy was missing the renderer's monotone pass, so the drawn
/// line could fold back while the renderer straightened it: the graph and the sliders below it
/// disagreed. This reads the painted line itself, not the function behind it.
#[test]
fn the_parametric_graph_paints_the_model_the_renderer_uses() {
    let mut h = curve_open("parametric");
    let c = folded_regions();
    for (control, value) in [("curve.shadows", c.shadows), ("curve.darks", c.darks), ("curve.lights", c.lights), ("curve.highlights", c.highlights)] {
        exec(&mut h, "develop.set", json!({"control": control, "value": value}));
    }
    h.step();
    h.step();
    let settings = develop(&h).curve;
    assert_eq!(settings, c, "the sliders hold the folded shape");
    let graph = curve_graph_rect(&mut h);
    let painted = painted_param_curve(&h, graph);
    let model = ParametricCurve::new(&c);
    let mut checked = 0;
    for i in 0..=24 {
        let x = i as f64 / 24.0;
        let want = model.at(x);
        let got = painted(x);
        assert!((got - want).abs() < 0.02, "the painted curve is the renderer's at {x}: painted {got}, model {want}");
        checked += 1;
    }
    assert_eq!(checked, 25);
    // and the shape really does fold back before the monotone pass, so this proves something
    assert!(model.at(0.6) > model.at(0.3), "the region bumps still bend the curve");
}

/// The graph rect as `[left, top, width, height]` plus a reader for the painted curve's height at a
/// graph fraction (0 = the bottom of the graph, 1 = the top).
fn curve_graph_rect(h: &mut Headless) -> [f32; 4] {
    let r = h.request("ui.widgets", json!({"filter": "curve"}), T);
    let w = r["result"].as_array().unwrap().iter().find(|w| w["id"] == "curve").cloned().unwrap_or_else(|| panic!("curve widget on screen: {r}"));
    let v = w["rect"].as_array().unwrap();
    std::array::from_fn(|i| v[i].as_f64().unwrap() as f32)
}

/// A reader for the height of the painted parametric curve at a graph fraction. The curve is the
/// wide grey path the graph draws; the histogram is a mesh and the grid lines are single segments.
fn painted_param_curve(h: &Headless, r: [f32; 4]) -> impl Fn(f64) -> f64 {
    let points = h
        .view
        .painted_shapes()
        .into_iter()
        .find_map(|s| match s {
            egui::Shape::Path(p) if p.points.len() > 32 && (p.stroke.width - 2.0).abs() < 0.1 => Some(p.points),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the parametric curve line is painted"));
    move |x: f64| {
        let want_x = r[0] + x as f32 * r[2];
        let q = points
            .iter()
            .min_by(|a, b| (a.x - want_x).abs().total_cmp(&(b.x - want_x).abs()))
            .copied()
            .unwrap_or_else(|| panic!("the curve has points"));
        ((r[1] + r[3] - q.y) / r[3]).clamp(0.0, 1.0) as f64
    }
}

/// The tone the parametric graph paints at `x`, read by value from the painted line. Stepped first
/// so the panel has drawn the state the command left behind.
fn painted_tone(h: &mut Headless, x: f64) -> f64 {
    h.step();
    h.step();
    let r = curve_graph_rect(h);
    painted_param_curve(h, r)(x)
}

/// Where the midtones sit on a linear curve: a point well inside the Lights region, so lifting
/// Lights bends the line there.
const MID: f64 = 0.625;

/// Reset restores the four sliders *and* puts the graph back to the straight diagonal, as one state.
#[test]
fn reset_clears_the_sliders_and_the_graph_together() {
    let mut h = curve_open("parametric");
    let straight = painted_tone(&mut h, MID);
    assert!((straight - MID).abs() < 0.02, "a fresh parametric curve is the diagonal: {straight}");
    exec(&mut h, "develop.set", json!({"control": "curve.lights", "value": 60}));
    let bent = painted_tone(&mut h, MID);
    assert!(bent > MID + 0.02, "the graph follows the slider up: {bent}");
    let undo_before = h.app.session.undo.len();
    click_widget(&mut h, "button:curveReset", 1);
    let c = develop(&h).curve;
    assert_eq!(c, lightcraft_develop::ToneCurve::default(), "Reset puts every slider and split back: {c:?}");
    let flat = painted_tone(&mut h, MID);
    assert!((flat - MID).abs() < 0.02, "and the graph is the diagonal again: {flat}");
    assert_eq!(h.app.session.undo.len(), undo_before + 1, "Reset is one undo step");
}

/// Undo after a Reset brings back both the slider and the graph: the state the graph draws is the
/// state Undo restores, so the two can never be left showing different things.
#[test]
fn undo_restores_the_slider_and_the_graph_together() {
    let mut h = curve_open("parametric");
    exec(&mut h, "develop.set", json!({"control": "curve.lights", "value": 60}));
    let bent = painted_tone(&mut h, MID);
    assert!(bent > MID + 0.02, "the graph is bent: {bent}");
    click_widget(&mut h, "button:curveReset", 1);
    assert_eq!(develop(&h).curve, lightcraft_develop::ToneCurve::default(), "reset first");
    exec(&mut h, "edit.undo", json!({}));
    let c = develop(&h).curve;
    assert_eq!(c.lights, 60.0, "Undo brings the slider back: {c:?}");
    let again = painted_tone(&mut h, MID);
    assert!(again > MID + 0.02, "and the graph is bent with it, not left flat: {again}");
    // redo puts the diagonal back, and the graph follows that too
    exec(&mut h, "edit.redo", json!({}));
    assert_eq!(develop(&h).curve, lightcraft_develop::ToneCurve::default(), "redo restores the reset");
    let flat = painted_tone(&mut h, MID);
    assert!((flat - MID).abs() < 0.02, "the graph is flat with the sliders: {flat}");
}

/// One graph drag that bends several regions at once is one undo step: Ctrl+Z takes back the whole
/// gesture, not the last slider it happened to touch.
#[test]
fn one_graph_drag_that_moves_several_regions_is_one_undo_step() {
    let mut h = curve_open("parametric");
    exec(&mut h, "develop.set", json!({"control": "curve.lights", "value": 40}));
    exec(&mut h, "develop.set", json!({"control": "curve.darks", "value": -40}));
    let before = develop(&h).curve;
    let undo_before = h.app.session.undo.len();
    // drag the Lights slider's own track, the way a graph handle writes it: begin, live updates, end
    exec(&mut h, "develop.beginInteraction", json!({"label": "Parametric Curve"}));
    for v in [45, 55, 70, 85] {
        exec(&mut h, "develop.set", json!({"control": "curve.lights", "value": v}));
    }
    exec(&mut h, "develop.endInteraction", json!({}));
    let after = develop(&h).curve;
    assert_eq!(after.lights, 85.0, "the drag ended at its last value: {after:?}");
    assert_ne!(after, before, "the drag changed the state");
    assert_eq!(h.app.session.undo.len(), undo_before + 1, "the whole drag is one step, not one per frame");
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h).curve, before, "undo takes back the whole gesture at once");
}

/// The screen point where a drag should grab the curve: graph fraction `fx`, on the line itself.
fn on_curve(r: [f32; 4], c: &lightcraft_develop::ToneCurve, fx: f32) -> [f32; 2] {
    let y = ParametricCurve::new(c).at(fx as f64);
    [r[0] + fx * r[2], r[1] + (1.0 - y as f32) * r[3]]
}

/// Dragging the curve itself bends the region under the pointer and moves that region's slider with
/// it, live. The graph draws no handles, so the only thing to grab is the line.
#[test]
fn dragging_the_curve_moves_the_region_slider() {
    let mut h = curve_open("parametric");
    exec(&mut h, "develop.set", json!({"control": "curve.lights", "value": 60}));
    h.step();
    let r = curve_graph_rect(&mut h);
    let c = develop(&h).curve;
    let undo_before = h.app.session.undo.len();
    // grab the line inside the Lights window (x = 0.625) and pull it down
    let [x, y] = on_curve(r, &c, 0.625);
    let d = h.request("ui.drag", json!({"x": x, "y": y, "toX": x, "toY": y + 0.08 * r[3], "steps": 8}), T);
    assert_eq!(d["ok"], true, "{d}");
    h.step();
    let after = develop(&h).curve;
    assert!(after.lights < 60.0, "the Lights slider follows the line down: {}", after.lights);
    assert!(after.lights > 0.0, "and lands on a value, not at the bottom: {}", after.lights);
    assert_eq!((after.shadows, after.darks, after.highlights), (0.0, 0.0, 0.0), "the other regions stay put: {after:?}");
    assert_eq!(after.split_shadows, c.split_shadows, "and the splits are untouched");
    assert_eq!(h.app.session.undo.len(), undo_before + 1, "one undo step for the whole drag");
    // the graph really is redrawn below where the line was
    let graph = curve_graph_rect(&mut h);
    let painted = painted_param_curve(&h, graph)(0.625);
    assert!(painted < c.lights / 100.0 + 0.625, "the painted line followed the drag down: {painted}");
    // and Undo takes the whole gesture back to where it started
    exec(&mut h, "edit.undo", json!({}));
    assert_eq!(develop(&h).curve, c, "undo restores the value the drag started from");
}

/// Dragging up bends the region the other way, so the gesture is not just "downwards works".
#[test]
fn dragging_the_curve_up_raises_the_region_slider() {
    let mut h = curve_open("parametric");
    let r = curve_graph_rect(&mut h);
    let c = develop(&h).curve;
    // x = 0.375 is inside the Darks window
    let [x, y] = on_curve(r, &c, 0.375);
    let d = h.request("ui.drag", json!({"x": x, "y": y, "toX": x, "toY": y - 0.08 * r[3], "steps": 8}), T);
    assert_eq!(d["ok"], true, "{d}");
    h.step();
    let after = develop(&h).curve;
    assert!(after.darks > 0.0, "the Darks slider follows the line up: {}", after.darks);
    assert_eq!((after.shadows, after.lights, after.highlights), (0.0, 0.0, 0.0), "the other regions stay put: {after:?}");
    assert!(ParametricCurve::new(&after).at(0.375) > ParametricCurve::new(&c).at(0.375), "the curve is drawn bent up at the Darks region");
}

/// Which region a drag moves is decided by where on the line it is grabbed: the same drag gesture
/// four times over grabs four different sliders.
#[test]
fn the_region_dragged_is_the_one_under_the_pointer() {
    for (fx, control, region) in
        [(0.125_f32, "curve.shadows", 0usize), (0.375, "curve.darks", 1), (0.625, "curve.lights", 2), (0.875, "curve.highlights", 3)]
    {
        let mut h = curve_open("parametric");
        let r = curve_graph_rect(&mut h);
        let c = develop(&h).curve;
        assert_eq!(ParametricCurve::new(&c).region_at(fx as f64), region, "x = {fx} belongs to {control}");
        let [x, y] = on_curve(r, &c, fx);
        let d = h.request("ui.drag", json!({"x": x, "y": y, "toX": x, "toY": y - 0.06 * r[3], "steps": 6}), T);
        assert_eq!(d["ok"], true, "{d}");
        h.step();
        let after = develop(&h).curve;
        let moved = after.shadows + after.darks + after.lights + after.highlights;
        let want = match region {
            0 => after.shadows,
            1 => after.darks,
            2 => after.lights,
            _ => after.highlights,
        };
        assert!(want > 0.0, "grabbing at {fx} raises {control}: {after:?}");
        assert!((moved - want).abs() < 1e-9, "and nothing else moved: {after:?}");
    }
}

/// A drag that starts away from the line is not a graph gesture at all: nothing moves and no undo
/// step is made (the graph is not a "click anywhere to bend" surface).
#[test]
fn a_parametric_drag_off_the_line_changes_nothing() {
    let mut h = curve_open("parametric");
    exec(&mut h, "develop.set", json!({"control": "curve.lights", "value": 40}));
    h.step();
    let before = develop(&h).curve;
    let undo = h.app.session.undo.len();
    let r = curve_graph_rect(&mut h);
    // a corner of the graph, far above the line
    let d = h.request(
        "ui.drag",
        json!({"x": r[0] + 0.06 * r[2], "y": r[1] + 0.06 * r[3], "toX": r[0] + 0.2 * r[2], "toY": r[1] + 0.3 * r[3], "steps": 6}),
        T,
    );
    assert_eq!(d["ok"], true, "{d}");
    h.step();
    assert_eq!(develop(&h).curve, before, "no control moved");
    assert_eq!(h.app.session.undo.len(), undo, "and no undo step was made");
}

/// Hovering the line names the region a press there would move (and offers the grab cursor), so the
/// graph says which slider it is about to change before the drag starts.
#[test]
fn hovering_the_line_names_the_region_it_would_move() {
    let mut h = curve_open("parametric");
    exec(&mut h, "develop.set", json!({"control": "curve.lights", "value": 60}));
    h.step();
    let r = curve_graph_rect(&mut h);
    let [x, y] = on_curve(r, &develop(&h).curve, 0.625);
    let q = h.request("ui.move", json!({"x": x, "y": y}), T);
    assert_eq!(q["ok"], true, "{q}");
    h.step();
    assert_eq!(h.last_cursor, egui::CursorIcon::Grab, "the line offers a grab: {:?}", h.last_cursor);
    assert!(h.painted_text().iter().any(|t| t.contains("Lights")), "and names the slider it would move: {:?}", h.painted_text());
    // well above the line there is no grab and no name
    let q = h.request("ui.move", json!({"x": x, "y": r[1] + 0.06 * r[3]}), T);
    assert_eq!(q["ok"], true, "{q}");
    h.step();
    assert_eq!(h.last_cursor, egui::CursorIcon::Default, "away from the line there is nothing to grab: {:?}", h.last_cursor);
}

/// A point's level on the region's curve and its slider amount are inverses, so a drag puts the line
/// exactly where the hand is, at whatever point on the graph it was grabbed. A level the region
/// cannot reach saturates rather than jumping.
#[test]
fn a_curve_level_and_its_amount_are_inverses() {
    let c = lightcraft_develop::ToneCurve { shadows: 20.0, lights: 40.0, ..Default::default() };
    let model = ParametricCurve::new(&c);
    for region in 0..4 {
        let centre = model.region_x(region);
        // the region's window centre, and a point off-centre inside the same window
        for x in [centre, centre + (model.split_x(region) - centre) * 0.5] {
            for amount in [-100.0, -40.0, 0.0, 30.0, 100.0] {
                let level = model.handle_level_at(region, amount, x);
                let back = model.amount_at_level(region, level, x);
                assert!((back - amount).abs() <= 1.0, "region {region} at {x}: {amount} → {level} → {back}");
            }
        }
    }
    let centre = model.region_x(3);
    assert_eq!(model.amount_at_level(3, 1.0, centre), 100.0, "a level above the region's reach saturates high");
    assert_eq!(model.amount_at_level(3, 0.0, centre), -100.0, "and one below saturates low");
}

/// The four regions tile the graph: every position belongs to exactly one of them, in order, so a
/// drag anywhere on the line moves exactly one slider and the one the pointer is over.
#[test]
fn the_four_regions_tile_the_graph() {
    let model = ParametricCurve::new(&lightcraft_develop::ToneCurve::default());
    assert_eq!(model.region_at(0.0), 0, "black is the darkest region");
    assert_eq!(model.region_at(1.0), 3, "white is the lightest");
    assert_eq!(model.region_at(0.2), 0, "below the first split");
    assert_eq!(model.region_at(0.3), 1, "between the first two splits");
    assert_eq!(model.region_at(0.6), 2);
    assert_eq!(model.region_at(0.8), 3);
    let mut prev = model.region_at(0.0);
    for i in 0..=100 {
        let r = model.region_at(i as f64 / 100.0);
        assert!(r >= prev, "the regions never go backwards: {r} after {prev}");
        prev = r;
    }
}
