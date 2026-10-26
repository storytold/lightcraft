//! iPad-only behaviour of the Detail view (press-and-hold Before, direct mask drags, multi-touch cancel, filmstrip
//! tick circles). `detail.rs` calls these; each returns at once unless `theme::touch_ui` is on, so desktop is untouched.

use egui::{Color32, Pos2, Rect, Stroke, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::MaskShape;
use serde_json::json;

use super::detail::{CanvasMap, Gesture, frame_long_norm};
use crate::LightcraftApp;
use crate::theme::{Tokens, touch_ui};
use crate::widgets::register;

/// Holding one finger still on the photo for this long shows Before.
const HOLD_SECS: f64 = 0.35;
/// Moving further than this (points) turns the hold into a pan.
const HOLD_SLOP: f32 = 18.0;

/// Single-finger press-and-hold on the canvas compares Before/After; resting a mouse button there is not a request to.
pub(super) fn canvas_hold(app: &mut LightcraftApp, ui: &egui::Ui, resp: &egui::Response) {
    if !touch_ui(ui.ctx()) {
        return;
    }
    let now = ui.input(|i| i.time);
    let is_down = ui.input(|i| i.pointer.primary_down());
    let pos = ui.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()));
    let pinching = ui.input(|i| i.multi_touch().is_some());
    let armed = resp.contains_pointer() && is_down && !pinching && app.gesture.is_none() && app.ui.tool.is_empty() && !app.ui.right.is_edit_tool();
    if armed {
        let Some(pos) = pos else { return };
        match app.ui.canvas_press_start {
            Some((start_t, start_pos)) => {
                if (pos - start_pos).length() > HOLD_SLOP {
                    // panning: stay disarmed until release, or a pause mid-pan would show Before
                    app.ui.canvas_press_start = Some((f64::INFINITY, pos));
                    app.ui.canvas_hold_before = false;
                } else if now - start_t >= HOLD_SECS {
                    app.ui.canvas_hold_before = true;
                    app.ui.canvas_suppress_click_zoom = true;
                    ui.ctx().request_repaint();
                }
            }
            None => {
                app.ui.canvas_press_start = Some((now, pos));
                app.ui.canvas_suppress_click_zoom = false;
            }
        }
    } else if (!is_down || pinching) && app.ui.canvas_press_start.is_some() {
        app.ui.canvas_press_start = None;
        app.ui.canvas_hold_before = false;
    }
}

/// A hold that showed Before must not also zoom when the finger lifts: the flag lives until that click is seen.
pub(super) fn suppress_click_zoom(app: &mut LightcraftApp, resp: &egui::Response) -> bool {
    if resp.clicked() || resp.double_clicked() { std::mem::take(&mut app.ui.canvas_suppress_click_zoom) } else { app.ui.canvas_suppress_click_zoom }
}

/// A second finger means pinch/pan: drop any half-made stroke or handle drag instead of letting the first keep painting.
/// Returns true when the caller should stop handling the mask overlay.
pub(super) fn cancel_on_multitouch(app: &mut LightcraftApp, ui: &egui::Ui) -> bool {
    if !touch_ui(ui.ctx()) || ui.input(|i| i.multi_touch().is_none()) {
        return false;
    }
    // a linear / radial mask being dragged out is dropped with the first finger's drag: one undo step, tool cleared
    if std::mem::take(&mut app.ui.mask_create_drag) {
        let _ = app.run("develop.endInteraction", json!({}));
        app.ui.tool.clear();
    }
    match app.gesture.take() {
        Some(Gesture::MaskHandle { .. }) => {
            let _ = app.run("develop.endInteraction", json!({}));
        }
        Some(Gesture::Brush { .. }) => app.brush_live = None,
        other => app.gesture = other,
    }
    true
}

/// Direct linear / radial creation by dragging on the photo. Returns true when it handled the frame.
pub(super) fn mask_create_drag(app: &mut LightcraftApp, ui: &egui::Ui, resp: &egui::Response, map: &CanvasMap, active: Option<u32>) -> bool {
    if !touch_ui(ui.ctx()) || !(app.ui.tool == "linear" || app.ui.tool == "radial") {
        return false;
    }
    let Some(mid) = active else { return false };
    let linear = app.ui.tool == "linear";
    if resp.drag_started()
        && let Some(q0) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        let n0 = map.norm(q0);
        let _ = app.run("develop.beginInteraction", json!({"label": "Add Mask"}));
        app.ui.mask_create_drag = true;
        let shape = if linear {
            MaskShape::Linear { start: n0, end: n0 }
        } else {
            MaskShape::Radial { center: n0, rx: 0.02, ry: 0.02, angle: 0.0, feather: 50.0, invert: false }
        };
        let _ = app.run("mask.update", json!({"id": mid, "component": 0, "shape": shape}));
    }
    if resp.dragged()
        && let (Some(q0), Some(q)) = (ui.input(|i| i.pointer.press_origin()), resp.interact_pointer_pos())
    {
        let (n0, n) = (map.norm(q0), map.norm(q));
        let shape = if linear {
            MaskShape::Linear { start: n0, end: n }
        } else {
            let l = frame_long_norm(map);
            let rx = ((n.x - n0.x).abs() / l.0).max(0.02);
            let ry = ((n.y - n0.y).abs() / l.1).max(0.02);
            MaskShape::Radial { center: n0, rx, ry, angle: 0.0, feather: 50.0, invert: false }
        };
        let _ = app.run("mask.update", json!({"id": mid, "component": 0, "shape": shape}));
    }
    if resp.drag_stopped() || resp.clicked() {
        if std::mem::take(&mut app.ui.mask_create_drag) {
            let _ = app.run("develop.endInteraction", json!({}));
        }
        app.ui.tool.clear();
    }
    true
}

/// Selection tick on a filmstrip cell (multi-select mode, or more than one photo selected).
pub(super) fn film_select_circle(app: &LightcraftApp, ui: &egui::Ui, p: &egui::Painter, t: &Tokens, cell: Rect, id: PhotoId, selected: bool) {
    if !touch_ui(ui.ctx()) || !(app.ui.multi_select || selected || app.session.selection.ids.len() > 1) {
        return;
    }
    let center: Pos2 = cell.right_top() + vec2(-13.0, 13.0);
    register(ui, format!("film_select_circle:{}", id.0), Rect::from_center_size(center, vec2(20.0, 20.0)));
    if selected {
        p.circle_filled(center, 9.0, t.accent);
        p.circle_stroke(center, 9.0, Stroke::new(1.0, Color32::WHITE));
        p.line_segment([center + vec2(-3.5, 0.0), center + vec2(-1.0, 2.8)], Stroke::new(2.0, Color32::WHITE));
        p.line_segment([center + vec2(-1.0, 2.8), center + vec2(3.8, -2.8)], Stroke::new(2.0, Color32::WHITE));
    } else {
        p.circle_filled(center, 9.0, Color32::from_rgba_unmultiplied(0, 0, 0, 100));
        p.circle_stroke(center, 9.0, Stroke::new(1.3, Color32::from_rgba_unmultiplied(255, 255, 255, 180)));
    }
}
