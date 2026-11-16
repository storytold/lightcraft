//! The histogram's tonal zones, as in Lightroom Classic: dragging one sideways moves its Basic
//! control (one undo step for the whole drag), the zone locks at the press, a double-click resets
//! that control, and hovering highlights the zone and names it in the readout under the plot.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The zones' controls, left to right (`panels::histogram::ZONES`).
const CONTROLS: [&str; 5] = ["light.blacks", "light.shadows", "light.exposure", "light.highlights", "light.whites"];

/// The demo library in Detail with the Edit panel (the histogram sits first) on a selected photo.
fn edit_panel() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let id = h.app.session.visible_cloned()[0].0;
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    widget(h, id).unwrap_or_else(|| panic!("no widget {id} on screen"))
}

fn value(h: &Headless, control: &str) -> f64 {
    let id = h.app.session.active().unwrap_or_else(|| panic!("no active photo"));
    let spec = lightcraft_develop::controls::find(control).unwrap_or_else(|| panic!("no control {control}"));
    lightcraft_develop::controls::get(&h.app.session.develop_of(id).unwrap_or_default(), control).unwrap_or(spec.default)
}

/// Every zone's control, left to right.
fn values(h: &Headless) -> Vec<f64> {
    CONTROLS.iter().map(|c| value(h, c)).collect()
}

/// Set a control through the engine, as a slider would.
fn set(h: &mut Headless, control: &str, v: f64) {
    h.app.run("develop.set", json!({"control": control, "value": v})).unwrap_or_else(|e| panic!("develop.set {control}: {e}"));
}

/// Let the harness clock pass `seconds`, running frames. egui closes its double-click window when
/// the next click comes more than 0.3 s later *and* more than 6 points away; its triple-click
/// window is twice that, and two clicks in another zone right after two in one are close enough to
/// the second click to count as a triple click of it — which no longer reports `double_clicked`.
fn after(h: &mut Headless, seconds: f64) {
    let until = h.time() + seconds;
    while h.time() < until {
        h.step();
    }
}

/// Drag from a fraction `fx` across the histogram: `fy` 0.5 is inside the plot, `dx` in points.
fn drag(h: &mut Headless, fx: f32, dx: f32, shift: bool) {
    let p = json!({"id": "histogram", "fx": fx, "fy": 0.5, "dx": dx, "steps": 8, "shift": shift});
    let r = h.request("ui.dragWidget", p.clone(), T);
    assert_eq!(r["ok"], true, "{p}: {r}");
    h.settle(SETTLE);
}

/// A double-click on the histogram at `fx` (the plot's own rows, `fy` 0.5).
fn double_click(h: &mut Headless, fx: f32) {
    let r = h.request("ui.clickWidget", json!({"id": "histogram", "fx": fx, "fy": 0.5, "count": 2}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

/// Hover the histogram at `fx` and let the frame settle.
fn hover(h: &mut Headless, fx: f32) {
    let r = h.request("ui.hoverWidget", json!({"id": "histogram", "fx": fx, "fy": 0.5}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

/// Dragging one zone moves that control: a sideways travel of the plot's width sweeps its whole
/// range, and the whole drag is one undo step.
#[test]
fn dragging_the_exposure_zone_moves_the_exposure_in_one_undo_step() {
    let mut h = edit_panel();
    let w = rect(&h, "histogram").width();
    let undo = h.app.session.undo.len();
    drag(&mut h, 0.5, w * 0.05, false);
    // a twentieth of the plot: a twentieth of ±5 EV
    let exposure = value(&h, "light.exposure");
    assert!((exposure - 0.5).abs() <= 0.02, "a twentieth of the plot is a twentieth of the range: {exposure}");
    assert_eq!(h.app.session.undo.len() - undo, 1, "one undo step for the whole drag");
    assert!(h.app.session.interaction.is_none(), "the interaction is closed on the release");
    assert!(h.app.ui.dragging_control.is_none(), "no control is left dragged");
    let mut others = values(&h);
    others.remove(2);
    assert_eq!(others, [0.0, 0.0, 0.0, 0.0], "the zones the drag crossed are untouched");
    // one undo brings the exposure back
    let r = h.request("engine.execute", json!({"command": "edit.undo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(value(&h, "light.exposure"), 0.0);
}

/// The zone a press landed in stays locked for the whole drag: dragging from Exposure across the
/// Shadows and Blacks zones still only moves the Exposure.
#[test]
fn the_zone_locks_at_the_press_however_far_the_drag_travels() {
    let mut h = edit_panel();
    let w = rect(&h, "histogram").width();
    let undo = h.app.session.undo.len();
    drag(&mut h, 0.5, -w * 0.4, false);
    let exposure = value(&h, "light.exposure");
    assert!((exposure + 4.0).abs() <= 0.02, "four tenths of the plot to the left is −4 EV: {exposure}");
    assert_eq!((value(&h, "light.shadows"), value(&h, "light.blacks")), (0.0, 0.0), "the zones crossed stay untouched");
    assert_eq!(h.app.session.undo.len() - undo, 1, "still one undo step");
}

/// Dragging left lowers the zone's control and dragging right raises it; a drag past either end
/// clamps at that control's range (the zones' own specs, ±100 for the four, ±5 EV for Exposure).
#[test]
fn a_drag_lowers_and_clamps_at_the_controls_range() {
    let mut h = edit_panel();
    let w = rect(&h, "histogram").width();
    drag(&mut h, 0.5, -w * 0.2, false);
    assert!((value(&h, "light.exposure") + 2.0).abs() <= 0.02, "{}", value(&h, "light.exposure"));
    // the Blacks zone, dragged right across the whole plot and beyond: clamped at +100
    drag(&mut h, 0.08, w * 0.9, false);
    assert_eq!(value(&h, "light.blacks"), 100.0);
    drag(&mut h, 0.92, -w * 2.0, false);
    assert_eq!(value(&h, "light.whites"), -100.0);
}

/// Shift is a tenth of the travel, as on the sliders.
#[test]
fn shift_drags_a_tenth_as_far() {
    let mut h = edit_panel();
    let w = rect(&h, "histogram").width();
    drag(&mut h, 0.5, w * 0.1, false);
    let plain = value(&h, "light.exposure");
    assert!((plain - 1.0).abs() <= 0.02, "{plain}");
    set(&mut h, "light.exposure", 0.0);
    drag(&mut h, 0.5, w * 0.1, true);
    let fine = value(&h, "light.exposure");
    assert!((fine - 0.1).abs() <= 0.02, "a tenth of {plain}: {fine}");
}

/// A double-click in a zone puts that zone's control back to its default in one undo step, leaving
/// the other zones' controls alone.
#[test]
fn double_clicking_a_zone_resets_that_control_in_one_undo_step() {
    let mut h = edit_panel();
    set(&mut h, "light.exposure", 1.25);
    set(&mut h, "light.shadows", 40.0);
    let undo = h.app.session.undo.len();
    double_click(&mut h, 0.5);
    assert_eq!(value(&h, "light.exposure"), 0.0, "the Exposure zone is reset");
    assert_eq!(value(&h, "light.shadows"), 40.0, "only the zone that was clicked");
    assert_eq!(h.app.session.undo.len() - undo, 1, "one undo step");
    // the Shadows zone (0.15–0.35) resets its own control, once the double-click window has passed
    after(&mut h, 0.7);
    double_click(&mut h, 0.25);
    assert_eq!(value(&h, "light.shadows"), 0.0);
    // a click on the histogram toggles the clipping warnings (as before this feature): the two
    // clicks of a double-click toggle them twice, so the state is where it started
    assert!(!h.app.ui.show_clipping);
}

/// The strip under the plot — the readout and the EXIF line — is not a zone: a drag there leaves
/// every control alone.
#[test]
fn a_drag_below_the_plot_leaves_the_tones_alone() {
    let mut h = edit_panel();
    let w = rect(&h, "histogram").width();
    let undo = h.app.session.undo.len();
    let p = json!({"id": "histogram", "fx": 0.5, "fy": 0.97, "dx": w * 0.2, "steps": 6});
    let r = h.request("ui.dragWidget", p.clone(), T);
    assert_eq!(r["ok"], true, "{p}: {r}");
    h.settle(SETTLE);
    assert_eq!(values(&h), [0.0; 5], "no zone was dragged");
    assert_eq!(h.app.session.undo.len(), undo, "and nothing was recorded");
    assert!(h.app.session.interaction.is_none(), "no interaction was opened");
}

/// A drag that starts on a clipping chevron (shadows or highlights) changes no develop tone: the
/// chevrons are clipping controls, not tonal zones, even though they sit inside the plot.
#[test]
fn a_drag_starting_on_a_clipping_chevron_changes_no_develop_value() {
    let mut h = edit_panel();
    let undo = h.app.session.undo.len();
    // Drag starting on the shadow clipping indicator (which sits over the Blacks zone)
    let p = json!({"id": "histogram:shadows", "dx": 40.0, "steps": 8});
    let r = h.request("ui.dragWidget", p.clone(), T);
    assert_eq!(r["ok"], true, "{p}: {r}");
    h.settle(SETTLE);
    assert_eq!(values(&h), [0.0; 5], "dragging from the shadow chevron moves no tone");
    assert_eq!(h.app.session.undo.len(), undo, "and leaves undo alone");
    assert!(h.app.session.interaction.is_none(), "no interaction was opened");

    // Drag starting on the highlight clipping indicator (which sits over the Whites zone)
    let p = json!({"id": "histogram:highlights", "dx": -40.0, "steps": 8});
    let r = h.request("ui.dragWidget", p.clone(), T);
    assert_eq!(r["ok"], true, "{p}: {r}");
    h.settle(SETTLE);
    assert_eq!(values(&h), [0.0; 5], "dragging from the highlight chevron moves no tone");
    assert_eq!(h.app.session.undo.len(), undo, "undo remains untouched");
    assert!(h.app.session.interaction.is_none(), "no interaction was opened");
}

/// Hovering a zone highlights it (the band covers that zone's share of the plot) and names it with
/// its value in the readout; off the histogram both are gone.
#[test]
fn hovering_a_zone_highlights_it_and_shows_its_name_and_value() {
    let mut h = edit_panel();
    let hist = rect(&h, "histogram");
    set(&mut h, "light.shadows", 25.0);
    hover(&mut h, 0.25);
    let band = rect(&h, "histogram:zone");
    let share = |x: f32| hist.left() + hist.width() * x;
    assert!(band.left() >= share(0.14) && band.right() <= share(0.36), "the Shadows band: {band:?} in {hist:?}");
    assert!(h.painted_text().iter().any(|t| t == "Shadows: +25"), "{:?}", h.painted_text());
    // the Exposure zone, with its EV unit
    hover(&mut h, 0.5);
    let band = rect(&h, "histogram:zone");
    assert!(band.left() >= share(0.34) && band.right() <= share(0.66), "the Exposure band: {band:?}");
    assert!(h.painted_text().iter().any(|t| t == "Exposure: 0.00 EV"), "{:?}", h.painted_text());
    // the pointer off the histogram: no band, no zone readout (the EXIF line is back)
    let r = h.request("ui.move", json!({"x": 600.0, "y": 500.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(widget(&h, "histogram:zone").is_none(), "the highlight is gone");
    assert!(!h.painted_text().iter().any(|t| t.starts_with("Shadows: ")), "{:?}", h.painted_text());
}

/// The pointer over the histogram says the zones can be dragged sideways; elsewhere the canvas'
/// own cursor is back.
#[test]
fn the_cursor_over_the_histogram_offers_a_sideways_drag() {
    let mut h = edit_panel();
    hover(&mut h, 0.5);
    assert_eq!(h.last_cursor, egui::CursorIcon::ResizeHorizontal);
    let r = h.request("ui.move", json!({"x": 600.0, "y": 500.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_ne!(h.last_cursor, egui::CursorIcon::ResizeHorizontal);
}

/// A drag dispatches as it goes, with the interaction held open, so the draft render follows it
/// live; nothing is committed until the release, which lands the whole drag as one undo step.
#[test]
fn a_drag_dispatches_live_and_commits_on_the_release() {
    let mut h = edit_panel();
    let before = h.app.session.undo.len();
    let hist = rect(&h, "histogram");
    let from = egui::pos2(hist.left() + hist.width() * 0.5, hist.top() + hist.height() * 0.5);
    let to = from + egui::vec2(hist.width() * 0.1, 0.0);
    // a press and its moves with no release: the drag is still going when the frames below run
    h.app.synthetic.push(egui::Event::PointerMoved(from));
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    for i in 1..=6 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
    }
    for _ in 0..12 {
        h.step();
    }
    let exposure = value(&h, "light.exposure");
    assert!(exposure > 0.4, "the exposure followed the drag while it was still going: {exposure}");
    assert!(h.app.session.interaction.is_some(), "the drag holds the interaction open");
    assert_eq!(h.app.ui.dragging_control.as_deref(), Some("light.exposure"), "the drag names the control it moves, as the sliders do");
    assert_eq!(h.app.session.undo.len(), before, "nothing is committed mid-drag");
    // the release: one undo step for the whole drag, and the interaction is closed
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.settle(SETTLE);
    assert!(h.app.session.interaction.is_none(), "the release commits the interaction");
    assert!(h.app.ui.dragging_control.is_none(), "and no control is left dragged");
    assert_eq!(h.app.session.undo.len() - before, 1, "the whole drag is one undo step");
}
