//! Lens Blur's tools on the photo: moving and resizing its fixed focus shape, and painting its
//! Focus / Blur refinement brushes.

use super::geometry::CanvasMap;
use super::masks::{frame_long_norm, pen_force, pin, record_pressure};
use super::{Color32, DevelopSettings, Gesture, LightcraftApp, Point, Pos2, Rect, Stroke, json, register, vec2};

/// Lens Blur's fixed focus shape on the photo (Edit Shape): its outline, a centre pin that moves
/// it (or drag anywhere inside), and handles on its right and bottom edges that stretch it
/// sideways or up and down. One drag is one undo step.
pub(super) fn lens_blur_shape_overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
    let Some(area) = d.lens_blur.focus_area(d.crop_center()) else { return };
    let clip = app.canvas_rect.unwrap_or(map.rect);
    let p = &ui.painter_at(clip);
    let l = frame_long_norm(map);
    let at = |dx: f64, dy: f64| map.screen(Point::new(area.center.x + dx * l.0, area.center.y + dy * l.1));
    let outline: Vec<Pos2> = (0..72)
        .map(|i| {
            let a = i as f64 / 72.0 * std::f64::consts::TAU;
            at(area.rx * a.cos(), area.ry * a.sin())
        })
        .collect();
    // a dark keyline under a light line: visible over bright and dark photos alike
    p.add(egui::Shape::closed_line(outline.clone(), Stroke::new(3.0, Color32::from_black_alpha(140))));
    p.add(egui::Shape::closed_line(outline, Stroke::new(1.5, Color32::from_white_alpha(235))));
    let (c, hx, hy) = (at(0.0, 0.0), at(area.rx, 0.0), at(0.0, area.ry));
    pin(p, c, true);
    for h in [hx, hy] {
        p.circle_filled(h, 5.5, Color32::WHITE);
        p.circle_stroke(h, 5.5, Stroke::new(1.5, Color32::from_black_alpha(160)));
    }
    for (name, q) in [("center", c), ("width", hx), ("height", hy)] {
        register(ui, format!("lensBlurArea:{name}"), Rect::from_center_size(q, vec2(14.0, 14.0)));
    }
    // which part a point grabs: a handle, else the inside
    let hit = |q: Pos2| -> Option<u8> {
        if q.distance(hx) < 10.0 {
            return Some(1);
        }
        if q.distance(hy) < 10.0 {
            return Some(2);
        }
        let n = map.norm(q);
        let (ex, ey) = ((n.x - area.center.x) / (area.rx * l.0).max(1e-9), (n.y - area.center.y) / (area.ry * l.1).max(1e-9));
        (q.distance(c) < 10.0 || ex * ex + ey * ey <= 1.0).then_some(0)
    };
    if let Some(q) = resp.hover_pos() {
        let icon = match hit(q) {
            Some(1) => Some(egui::CursorIcon::ResizeHorizontal),
            Some(2) => Some(egui::CursorIcon::ResizeVertical),
            Some(_) => Some(egui::CursorIcon::Move),
            None => None,
        };
        if let Some(icon) = icon {
            ui.ctx().set_cursor_icon(icon);
        }
    }
    if resp.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
        && let Some(handle) = hit(q)
    {
        let _ = app.run("develop.beginInteraction", json!({"label": "Lens Blur Shape"}));
        app.gesture = Some(Gesture::LensBlurArea { handle, original: area });
    }
    if resp.dragged()
        && let (Some(Gesture::LensBlurArea { handle, original }), Some(q)) = (app.gesture.clone(), resp.interact_pointer_pos())
    {
        let n = map.norm(q);
        let n0 = map.norm(ui.input(|i| i.pointer.press_origin()).unwrap_or(q));
        let moved = match handle {
            0 => lightcraft_develop::FocusArea { center: Point::new(original.center.x + n.x - n0.x, original.center.y + n.y - n0.y), ..original },
            1 => lightcraft_develop::FocusArea { rx: (n.x - original.center.x).abs() / l.0.max(1e-9), ..original },
            _ => lightcraft_develop::FocusArea { ry: (n.y - original.center.y).abs() / l.1.max(1e-9), ..original },
        }
        .sanitized();
        let _ = app.run("develop.merge", json!({"settings": {"lens_blur": {"area": moved}}, "label": "Lens Blur Shape"}));
    }
    if resp.drag_stopped() && matches!(app.gesture, Some(Gesture::LensBlurArea { .. })) {
        app.gesture = None;
        let _ = app.run("develop.endInteraction", json!({}));
    }
}

/// Lens Blur Brush Refinement on the photo: the brush circle (size and feather), the stroke being
/// painted (green: Focus, red: Blur), committed as `lensBlur.brushStroke` when the drag ends.
/// ⌥ (Alt) erases what the brush painted.
pub(super) fn lens_blur_brush_overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap) {
    let long = (map.rect.width().max(map.rect.height())) as f64;
    let clip = app.canvas_rect.unwrap_or(map.rect);
    let p = &ui.painter_at(clip);
    let focus = app.ui.tool == "lensBlurFocus";
    let r = (app.ui.brush_size as f64 * long) as f32;
    if let Some(h) = resp.hover_pos() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        p.circle_stroke(h, r, Stroke::new(1.0, Color32::WHITE));
        p.circle_stroke(h, r * (1.0 - app.ui.brush_feather / 100.0).max(0.05), Stroke::new(1.0, Color32::from_white_alpha(120)));
    }
    if let Some(q) = resp.interact_pointer_pos()
        && (resp.drag_started() || resp.dragged() || resp.clicked())
    {
        let n = map.norm(q);
        let force = if app.ui.brush_pressure { ui.input(pen_force) } else { None };
        match &mut app.gesture {
            Some(Gesture::Brush { points, .. }) if points.last() == Some(&n) => {}
            Some(Gesture::Brush { points, pressure }) => {
                record_pressure(pressure, points.len(), force);
                points.push(n);
            }
            _ => {
                let mut pressure = Vec::new();
                record_pressure(&mut pressure, 0, force);
                app.gesture = Some(Gesture::Brush { points: vec![n], pressure });
            }
        }
    }
    if let Some(Gesture::Brush { points, pressure }) = &app.gesture {
        let screen: Vec<Pos2> = points.iter().map(|q| map.screen(*q)).collect();
        let params = crate::brush_live::LiveParams {
            clip,
            scale: ui.ctx().pixels_per_point(),
            r,
            hard: r * (1.0 - (app.ui.brush_feather / 100.0).clamp(0.0, 1.0)),
            flow: (app.ui.brush_flow / 100.0).clamp(0.0, 1.0),
            density: (app.ui.lens_blur_brush_amount / 100.0).clamp(0.0, 1.0),
            color: if focus { [70, 200, 120] } else { [230, 70, 70] },
            opacity: 0.45,
        };
        crate::brush_live::paint(&mut app.brush_live, ui.ctx(), p, params, &screen, pressure);
    } else {
        app.brush_live = None;
    }
    if (resp.drag_stopped() || resp.clicked())
        && let Some(Gesture::Brush { points, pressure }) = app.gesture.take()
    {
        app.brush_live = None;
        let pts: Vec<[f64; 2]> = points.iter().map(|q| [q.x, q.y]).collect();
        let erase = resp.ctx.input(|i| i.modifiers.alt);
        let r = app.run(
            "lensBlur.brushStroke",
            json!({"mode": if focus { "focus" } else { "blur" }, "points": pts, "size": app.ui.brush_size, "feather": app.ui.brush_feather,
                   "flow": app.ui.brush_flow, "amount": app.ui.lens_blur_brush_amount, "erase": erase, "autoMask": app.ui.brush_auto_mask,
                   "pressure": pressure}),
        );
        if let Err(e) = r {
            app.toast(ui.ctx(), e);
        }
    }
}
