use super::geometry::CanvasMap;
use super::{
    Affine, Color32, DevelopSettings, Frame, Gesture, LightcraftApp, PhotoId, Point, Pos2, Rect, Stroke, Tokens, json, pos2, register, vec2,
};

// ------------------------------------------------------------------------ crop

pub(super) fn crop_overlay(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    resp: &egui::Response,
    map: &CanvasMap,
    frame: &Frame,
    d: &DevelopSettings,
    id: PhotoId,
) {
    // the selected guide belongs to this photo and to the Guided Upright tool, and to a guide
    // that still exists (a Delete after the last one leaves the selection behind)
    if app.ui.tool != "guidedUpright" || app.ui.selected_guide.is_some_and(|(photo, i)| photo != id.0 || i >= d.geometry.guides.len()) {
        app.ui.selected_guide = None;
    }
    if app.ui.tool == "guidedUpright" {
        guided_overlay(app, ui, resp, map, frame, d, id);
        return;
    }
    if app.ui.tool == "straighten" {
        straighten_overlay(app, ui, resp, false);
        return;
    }
    // hold ⌘ and drag: draw a straighten line without leaving the crop tool
    let cmd_drag = resp.drag_started() && ui.input(|i| i.modifiers.command);
    if cmd_drag || matches!(app.gesture, Some(Gesture::StraightenLine { .. })) {
        straighten_overlay(app, ui, resp, true);
        return;
    }
    if resp.hovered() && ui.input(|i| i.modifiers.command) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    // an untouched photo shows the remembered ratio's box, which the first drag (or Return) applies
    let preview;
    let d = match app.crop_tool_preview() {
        Some(geometry) => {
            preview = DevelopSettings { crop: lightcraft_develop::Crop { geometry, ..d.crop }, ..d.clone() };
            &preview
        }
        None => d,
    };
    let quad = frame_crop_quad(d, frame);
    let pts: Vec<Pos2> = quad.iter().map(|q| map.screen(*q)).collect();
    let p = ui.painter();
    // darken outside the crop (the box's corners in screen order: a flipped photo shows its
    // top-left corner elsewhere)
    let quad = flipped_corners(&pts, frame.flip_h, frame.flip_v);
    if pts.len() == 4 {
        p.add(outside_crop_mesh(map.rect, &quad, Color32::from_black_alpha(150)));
    }
    p.add(egui::Shape::closed_line(pts.clone(), Stroke::new(1.0, Color32::WHITE)));
    // overlay guides
    let lerp = |a: Pos2, b: Pos2, t: f32| a + (b - a) * t;
    let guide = Stroke::new(0.8, Color32::from_white_alpha(150));
    // (u, v) across / down the (rotated) crop frame
    let at = |(u, v): (f32, f32)| pts[0] + (pts[1] - pts[0]) * u + (pts[3] - pts[0]) * v;
    let (cw, ch) = (pts[0].distance(pts[1]), pts[0].distance(pts[3]));
    for line in crate::panels::crop_overlay::paths(app.ui.crop_overlay, app.ui.crop_overlay_orient, cw, ch) {
        p.add(egui::Shape::line(line.into_iter().map(at).collect(), guide));
    }
    // handles: 0..3 corners, 4..7 edges (top, right, bottom, left)
    let handles: Vec<Pos2> = (0..8).map(|i| if i < 4 { pts[i] } else { lerp(pts[i - 4], pts[(i - 3) % 4], 0.5) }).collect();
    for (i, h) in handles.iter().enumerate() {
        register(ui, format!("cropHandle:{i}"), Rect::from_center_size(*h, vec2(14.0, 14.0)));
        let s = if i < 4 { 12.0 } else { 9.0 };
        p.rect_filled(Rect::from_center_size(*h, vec2(s, 3.0)), 0.0, Color32::WHITE);
        p.rect_filled(Rect::from_center_size(*h, vec2(3.0, s)), 0.0, Color32::WHITE);
    }
    // the keyboard: each handle (and the box itself) a stand-in Tab reaches and the arrows move
    if crop_keys(app, ui, map, &handles, &pts, d, frame) {
        return;
    }
    let inside = |q: Pos2| {
        let n = map.norm(q);
        let s = to_straight(n, d.crop.geometry.angle, frame);
        d.crop.geometry.rect.contains(Point::new(s.x.clamp(-1.0, 2.0), s.y))
    };
    // a crop drag cut short (the tool closed before the release) leaves its gesture behind: with no
    // button held it is over
    if matches!(app.gesture, Some(Gesture::CropRotate { .. } | Gesture::CropHandle { .. }))
        && !resp.drag_stopped()
        && !ui.input(|i| i.pointer.any_down())
    {
        app.gesture = None;
    }
    // while rotating, the pointer may leave the canvas: it still shows rotation and the angle
    let rotating = matches!(app.gesture, Some(Gesture::CropRotate { .. }));
    let pointer = resp.hover_pos().or_else(|| if rotating { ui.input(|i| i.pointer.latest_pos()) } else { None });
    // with ⌘ held a drag draws a level line (the straighten crosshair, set above), not a rotation
    let straightening = !rotating && ui.input(|i| i.modifiers.command);
    if let Some(hq) = pointer.filter(|_| !straightening) {
        // a move or resize drag keeps its pointer wherever it goes, even past the box
        let held = match app.gesture {
            Some(Gesture::CropHandle { handle, .. }) => Some(usize::from(handle)),
            _ => None,
        };
        let near = held.or_else(|| handles.iter().position(|h| h.distance(hq) < 12.0)).filter(|_| !rotating);
        let cursor = match near {
            Some(0 | 2) => egui::CursorIcon::ResizeNwSe,
            Some(1 | 3) => egui::CursorIcon::ResizeNeSw,
            Some(4 | 6) => egui::CursorIcon::ResizeVertical,
            Some(5 | 7) => egui::CursorIcon::ResizeHorizontal,
            // 8: the whole box
            Some(_) => egui::CursorIcon::Move,
            None if inside(hq) && !rotating => egui::CursorIcon::Move,
            // a drag here rotates: no system cursor shows that, so draw a curved double arrow
            None => {
                rotate_cursor(ui, hq, rotate_cursor_turn(&pts, hq, inside(hq)));
                egui::CursorIcon::None
            }
        };
        ui.ctx().set_cursor_icon(cursor);
    }
    // the angle next to the pointer, drawn once this frame's rotation is applied (below)
    let readout_at = pointer.filter(|_| rotating);
    let mut shown_angle = d.crop.geometry.angle;
    // double-click inside the crop box applies the crop (same as Return / Done)
    if resp.double_clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && inside(q)
        && !handles.iter().any(|h| h.distance(q) < 12.0)
    {
        let _ = app.run("tool.done", json!({}));
        return;
    }
    // egui reports a drag only once the pointer has moved a few px: the zone is where the button
    // went down, where the cursor showed it, not where the pointer is by then
    if resp.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos())
    {
        if let Err(e) = app.run("develop.beginInteraction", json!({"label": "Crop"})).and_then(|_| app.begin_crop().map(|_| serde_json::Value::Null))
        {
            app.ui.status = e;
            let _ = app.session.cancel_interaction();
            return;
        }
        let d = app.session.develop_of(id).unwrap_or_default();
        app.gesture = Some(match handles.iter().position(|h| h.distance(q) < 12.0) {
            Some(h) => Gesture::CropHandle { handle: h as u8, start: d.crop.geometry.rect, angle: d.crop.geometry.angle },
            None if inside(q) => Gesture::CropHandle { handle: 8, start: d.crop.geometry.rect, angle: d.crop.geometry.angle },
            None => {
                let c = map.screen(Point::new(0.5, 0.5));
                Gesture::CropRotate { start_angle: d.crop.geometry.angle, a0: (q - c).angle() }
            }
        });
    }
    if resp.dragged()
        && let Some(q) = resp.interact_pointer_pos()
    {
        match app.gesture.clone() {
            Some(Gesture::CropHandle { handle, start, angle }) => {
                // The crop model decides what the drag does (anchor, aspect lock, image bounds): see
                // `lightcraft_geom::drag_crop`; the UI only reports where the pointer started and is.
                let n = to_straight(map.norm(q), angle, frame);
                let orig = ui.input(|i| i.pointer.press_origin()).map(|q0| to_straight(map.norm(q0), angle, frame)).unwrap_or(n);
                // Shift keeps the current proportions for this drag only; the Lock toggle is untouched.
                let keep_ratio = ui.input(|i| i.modifiers.shift);
                let _ = app.run(
                    "crop.drag",
                    json!({"handle": handle, "from": [orig.x, orig.y], "to": [n.x, n.y], "start": [start.x0, start.y0, start.x1, start.y1], "keepRatio": keep_ratio}),
                );
            }
            Some(Gesture::CropRotate { start_angle, a0 }) => {
                let c = map.screen(Point::new(0.5, 0.5));
                let a = (q - c).angle();
                let ang = (start_angle + (a - a0).to_degrees() as f64).clamp(-45.0, 45.0);
                shown_angle = (ang * 100.0).round() / 100.0;
                let _ = app.run("crop.straighten", json!({"angle": shown_angle}));
            }
            _ => {}
        }
    }
    if resp.drag_stopped() {
        app.gesture = None;
        let _ = app.run("develop.endInteraction", json!({}));
    }
    if let Some(at) = readout_at {
        // the canvas (the loupe's own rect), not the photo: rotating happens in the margin around it
        angle_readout(ui, at, shown_angle, resp.rect);
    }
}

/// The crop handles' names for screen readers, in `crop.drag`'s numbering: 0..3 corners, 4..7
/// edges (top, right, bottom, left), 8 the whole box.
const CROP_HANDLE_NAMES: [&str; 9] = [
    "Crop top-left corner",
    "Crop top-right corner",
    "Crop bottom-right corner",
    "Crop bottom-left corner",
    "Crop top edge",
    "Crop right edge",
    "Crop bottom edge",
    "Crop left edge",
    "Crop box",
];

/// Keyboard stand-ins for the crop handles: an arrow press drags the focused one by a step (one
/// undo step each), as a pointer drag from its place would. Return applies the crop. True when the
/// frame is over (the crop was applied).
fn crop_keys(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    map: &CanvasMap,
    handles: &[Pos2],
    pts: &[Pos2],
    d: &DevelopSettings,
    frame: &Frame,
) -> bool {
    let r = d.crop.geometry.rect;
    let centre = if pts.len() == 4 { pts.iter().fold(Pos2::ZERO, |acc, q| acc + q.to_vec2() / 4.0) } else { return false };
    let pct = |v: f64| if v.is_finite() { (v * 100.0).round() as i64 } else { 0 };
    let value = format!("left {}%, top {}%, right {}%, bottom {}%", pct(r.x0), pct(r.y0), pct(r.x1), pct(r.y1));
    for (i, name) in CROP_HANDLE_NAMES.iter().enumerate() {
        let Some(at) = handles.get(i).copied().or_else(|| (i == 8).then_some(centre)) else { continue };
        let keys = crate::canvas_keys::handle(ui, &format!("crop:{i}"), crate::i18n::tr(name), value.clone(), at);
        if keys.activated {
            let _ = app.run("tool.done", json!({}));
            return true;
        }
        let Some((dx, dy)) = keys.delta else { continue };
        // the drag runs in the straightened crop frame, as a pointer drag from the handle would;
        // the arrows follow the screen
        let angle = d.crop.geometry.angle;
        let to_q = at + vec2(dx as f32 * map.rect.width(), dy as f32 * map.rect.height());
        let (from, to) = (to_straight(map.norm(at), angle, frame), to_straight(map.norm(to_q), angle, frame));
        let started = app.run("develop.beginInteraction", json!({"label": "Crop"})).and_then(|_| app.begin_crop().map(|_| serde_json::Value::Null));
        if let Err(e) = started {
            app.ui.status = e;
            let _ = app.session.cancel_interaction();
            return false;
        }
        let _ = app.run(
            "crop.drag",
            json!({"handle": i, "from": [from.x, from.y], "to": [to.x, to.y], "start": [r.x0, r.y0, r.x1, r.y1], "keepRatio": false}),
        );
        let _ = app.run("develop.endInteraction", json!({}));
    }
    false
}

/// The crop angle as the rotation readout shows it: like the Straighten value, in degrees.
pub(crate) fn crop_angle_label(angle: f64) -> String {
    let shown =
        lightcraft_develop::controls::find("crop.angle").map_or_else(|| format!("{angle:.2}"), |spec| crate::widgets::shown_value(spec, angle));
    // the slider's format turns a rounded −0.00 into "0": every zero reads "0.00", the angle's
    // usual two decimals
    let shown = if shown == "0" { "0.00".to_string() } else { shown };
    format!("{shown}°")
}

/// Where the angle readout of `size` goes for a pointer `at`: below right of it, flipped to the
/// left / above where that would leave `bounds`, then kept inside them.
pub(crate) fn readout_rect(at: Pos2, size: egui::Vec2, bounds: Rect) -> Rect {
    // without a real pointer or bounds (NaN, an empty rect) there is nothing to keep it in; and
    // f32::clamp panics on NaN, so only finite numbers get there
    if !at.is_finite() || !bounds.is_finite() || !bounds.is_positive() {
        let at = if at.is_finite() { at } else { bounds.min.max(Pos2::ZERO) };
        let at = if at.is_finite() { at } else { Pos2::ZERO };
        return Rect::from_min_size(at + vec2(18.0, 14.0), size);
    }
    let x = if at.x + 18.0 + size.x > bounds.right() { at.x - 18.0 - size.x } else { at.x + 18.0 };
    let y = if at.y + 14.0 + size.y > bounds.bottom() { at.y - 14.0 - size.y } else { at.y + 14.0 };
    let x = x.clamp(bounds.left(), (bounds.right() - size.x).max(bounds.left()));
    let y = y.clamp(bounds.top(), (bounds.bottom() - size.y).max(bounds.top()));
    Rect::from_min_size(pos2(x, y), size)
}

/// On top of everything (the tooltip layer), so neither the photo nor a panel covers it.
fn top_painter(ui: &egui::Ui, name: &'static str) -> egui::Painter {
    ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique(name)))
}

/// The rotation pointer: a curved double arrow on a dark disc, so it reads over any photo,
/// turned `turn` radians clockwise ([`rotate_cursor_turn`]).
fn rotate_cursor(ui: &egui::Ui, at: Pos2, turn: f32) {
    let p = top_painter(ui, "crop-rotate-cursor");
    let r = Rect::from_center_size(at, vec2(22.0, 22.0));
    p.circle_filled(at, 12.0, Color32::from_black_alpha(150));
    crate::icons::paint_turned(&p, r.shrink(2.0), crate::icons::Icon::RotateDrag, Color32::WHITE, turn);
    register(ui, "cropRotateCursor", r);
}

/// How far to turn the rotation pointer (radians, clockwise) so its arc, drawn bulging up, bulges
/// away from the crop box `pts` (its four corners on screen, maybe rotated): up above the top edge,
/// right beside the right edge, down below it, diagonally off a corner. Measured from the nearest
/// point of the box's outline; `inside` (a rotate drag that has come back over the box) faces it
/// the other way, still away from that edge.
pub(super) fn rotate_cursor_turn(pts: &[Pos2], q: Pos2, inside: bool) -> f32 {
    let nearest = (0..pts.len())
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            let ab = b - a;
            let t = if ab.length_sq() > 0.0 { ((q - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
            a + ab * t
        })
        .min_by(|a, b| a.distance_sq(q).total_cmp(&b.distance_sq(q)));
    let Some(nearest) = nearest else { return 0.0 };
    let away = if inside { nearest - q } else { q - nearest };
    if away.length() < 0.5 {
        return 0.0;
    }
    away.y.atan2(away.x) + std::f32::consts::FRAC_PI_2
}

/// While rotating: the angle, next to the pointer, kept inside `bounds` (the canvas): on the
/// pointer's other side when it would run past an edge.
fn angle_readout(ui: &egui::Ui, at: Pos2, angle: f64, bounds: Rect) {
    let t = Tokens::get(ui.ctx());
    let p = top_painter(ui, "crop-angle-readout");
    let galley = p.layout_no_wrap(crop_angle_label(angle), t.font(12.5), Color32::WHITE);
    let rect = readout_rect(at, galley.size() + vec2(12.0, 6.0), bounds);
    p.rect_filled(rect, 4.0, Color32::from_black_alpha(170));
    p.galley(rect.min + vec2(6.0, 3.0), galley, Color32::WHITE);
    register(ui, "cropAngleReadout", rect);
}

/// Guided Upright: draw up to four guides along lines that should be vertical or horizontal. Guides are
/// stored in lens-corrected (pre-perspective) coordinates, so they stay attached to the image as it warps.
fn guided_overlay(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    resp: &egui::Response,
    map: &CanvasMap,
    frame: &Frame,
    d: &DevelopSettings,
    id: PhotoId,
) {
    let p = ui.painter_at(map.rect.expand(8.0));
    let col = Color32::from_rgb(255, 196, 40);
    // the selected guide: brighter, so the Delete key has a visible target
    let selected = app.ui.selected_guide.filter(|(photo, _)| *photo == id.0).map(|(_, i)| i);
    let draw = |a: Pos2, b: Pos2, sel: bool| {
        let (edge, core, at) = if sel { (3.5, 2.5, 5.0) } else { (3.0, 1.5, 4.0) };
        p.line_segment([a, b], Stroke::new(edge, Color32::from_black_alpha(120)));
        p.line_segment([a, b], Stroke::new(core, if sel { Color32::WHITE } else { col }));
        for q in [a, b] {
            p.circle_filled(q, at, if sel { Color32::WHITE } else { col });
            p.circle_stroke(q, at, Stroke::new(1.0, Color32::BLACK));
        }
    };
    let on_screen = |guide: &(Point, Point)| {
        let (sa, sb) = (map.screen(frame.corrected_to_transformed(guide.0)), map.screen(frame.corrected_to_transformed(guide.1)));
        (sa, sb)
    };
    for (i, guide) in d.geometry.guides.iter().enumerate() {
        let (sa, sb) = on_screen(guide);
        register(ui, format!("uprightGuide:{i}"), Rect::from_two_pos(sa, sb));
        draw(sa, sb, selected == Some(i));
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    // A press starts what it means, right away (the pointer's press point, not where a drag
    // threshold would have been crossed): on an endpoint it drags that endpoint (the guide is
    // selected when it lands), on a line it selects the guide (a press on the selected one
    // deselects), anywhere else it starts a new guide from the press point. The primary button
    // only, never on the navigator — it pans the photo, it draws no guide — and only where the
    // loupe itself takes the press: `contains_pointer` is layer-aware over the current pointer
    // (the raw pointer is not), and `layer_id_at` asks the same question at the press point —
    // a menu can receive the press and the pointer can leave it within the same frame, so the
    // press itself must belong to the loupe's layer. Anything floating above the photo — a
    // context menu, a dropdown, a dialog — owns its own press; the guide under it stays put.
    // A slow frame can also swallow the whole press-move-release into one batch, and egui
    // clears `press_origin` on the release: then the press position is read back from the
    // frame's own events, so the gesture still starts where the pointer went down.
    let nav = app.widgets.iter().rev().find(|(w, _)| w == "canvas:navigator").map(|(_, r)| *r);
    if ui.input(|i| i.pointer.primary_pressed())
        && let Some(q) = ui
            .input(|i| {
                i.pointer.press_origin().or_else(|| {
                    i.events.iter().rev().find_map(|e| match e {
                        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } => Some(*pos),
                        _ => None,
                    })
                })
            })
            .or(resp.interact_pointer_pos())
        && resp.contains_pointer()
        && ui.ctx().layer_id_at(q) == Some(resp.layer_id)
        && resp.rect.contains(q)
        && !nav.is_some_and(|n| n.expand(5.0).contains(q))
    {
        let endpoint = d.geometry.guides.iter().enumerate().find_map(|(i, guide)| {
            let (a, b) = on_screen(guide);
            if a.distance(q) <= 7.0 {
                Some((i, 0))
            } else if b.distance(q) <= 7.0 {
                Some((i, 1))
            } else {
                None
            }
        });
        let line = d.geometry.guides.iter().position(|guide| {
            let (a, b) = on_screen(guide);
            point_segment_dist(q, a, b) <= 6.0
        });
        match (endpoint, line) {
            (Some((i, end)), _) => app.gesture = Some(Gesture::GuideEndpoint { i, end, from: q }),
            (None, Some(i)) => app.ui.selected_guide = if selected == Some(i) { None } else { Some((id.0, i)) },
            // drawing a new guide leaves nothing selected
            (None, None) => {
                app.ui.selected_guide = None;
                app.gesture = Some(Gesture::Guide { a: map.norm(q) });
            }
        }
    }
    let q = resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.latest_pos()));
    if let (Some(Gesture::Guide { a }), Some(q)) = (&app.gesture, q) {
        draw(map.screen(*a), q, false);
    }
    // the endpoint being dragged: the guide follows the pointer, bright
    if let (Some(Gesture::GuideEndpoint { i, end, .. }), Some(q)) = (&app.gesture, q)
        && let Some(guide) = d.geometry.guides.get(*i)
    {
        let fixed = if *end == 0 { guide.1 } else { guide.0 };
        draw(map.screen(frame.corrected_to_transformed(fixed)), q, true);
    }
    if ui.input(|i| i.pointer.any_released()) {
        match app.gesture.take() {
            // the endpoint lands: the guide stays (one undo step) and is selected. A press
            // without a real drag only selects — no no-op undo entry for a click.
            Some(Gesture::GuideEndpoint { i, end, from }) => {
                app.ui.selected_guide = Some((id.0, i));
                if let (Some(q), Some(guide)) = (q, d.geometry.guides.get(i)) {
                    let fixed_t = if end == 0 { guide.1 } else { guide.0 };
                    let new_t = frame.transformed_to_corrected(map.norm(q));
                    let (a, b) = if end == 0 { (new_t, fixed_t) } else { (fixed_t, new_t) };
                    let (na, nb) = (frame.corrected_to_transformed(a), frame.corrected_to_transformed(b));
                    // a press that only selected (it landed within the endpoint's grab radius
                    // and never moved) writes no guide: compare against where it went down
                    let moved = from.distance(q) > 2.0;
                    if moved && na.dist(nb) > 0.02 {
                        let all: Vec<[f64; 4]> = d
                            .geometry
                            .guides
                            .iter()
                            .enumerate()
                            .map(|(j, g)| {
                                let (x, y) = if j == i { (a, b) } else { *g };
                                [x.x, x.y, y.x, y.y]
                            })
                            .collect();
                        let _ = app.run("geometry.guides", json!({"guides": all}));
                    }
                }
            }
            Some(Gesture::Guide { a }) => {
                if let Some(q) = q
                    && map.norm(q).dist(a) > 0.02
                {
                    let b = map.norm(q);
                    let (ca, cb) = (frame.transformed_to_corrected(a), frame.transformed_to_corrected(b));
                    let _ = app.run("geometry.guides", json!({"guides": [[ca.x, ca.y, cb.x, cb.y]], "add": true}));
                }
            }
            other => app.gesture = other,
        }
    }
}

/// Normalized oriented coords → the straightened (rotated) frame the crop rect lives in.
fn to_straight(n: Point, angle: f64, frame: &Frame) -> Point {
    let (w, h) = (frame.ow, frame.oh);
    let px = Point::new(n.x * w, n.y * h);
    let r = Affine::rotate_about(angle.to_radians(), Point::new(w / 2.0, h / 2.0)).apply(px);
    Point::new(r.x / w, r.y / h)
}

/// How far `q` is from the segment `a`–`b` (screen pixels): the click that selects a guide.
fn point_segment_dist(q: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 { ((q - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
    (a + ab * t).distance(q)
}

/// A crop box's corners (top-left, top-right, bottom-right, bottom-left of the unflipped photo)
/// in the order they appear on screen when the photo is shown flipped.
pub(super) fn flipped_corners(pts: &[Pos2], flip_h: bool, flip_v: bool) -> [Pos2; 4] {
    let at = |i: usize| pts.get(i).copied().unwrap_or(Pos2::ZERO);
    let order = match (flip_h, flip_v) {
        (false, false) => [0, 1, 2, 3],
        (true, false) => [1, 0, 3, 2],
        (false, true) => [3, 2, 1, 0],
        (true, true) => [2, 3, 0, 1],
    };
    order.map(at)
}

/// The dimming outside the crop box: the photo's rectangle minus the crop quad, as one mesh of
/// non-overlapping convex pieces. The quad is a rotated rectangle, so its opposite sides are
/// parallel and the plane outside it splits into four disjoint half-strips: outside edge `i`
/// and inside edge `i + 1`. Each is clipped to the photo and fanned into triangles.
///
/// Issue #731: joining photo corner `i` to crop corner `i` with a quad per side only works while
/// the two rectangles are aligned. Rotated, those quads cross themselves and each other, which
/// showed as dimmed wedges inside the crop and doubly dark ones outside, growing with the angle.
/// A degenerate quad (a point, a line, a non-finite corner) dims nothing.
pub(super) fn outside_crop_mesh(img: Rect, quad: &[Pos2; 4], color: Color32) -> egui::epaint::Mesh {
    let mut mesh = egui::epaint::Mesh::default();
    if !quad.iter().all(|q| q.x.is_finite() && q.y.is_finite()) || !img.is_finite() {
        return mesh;
    }
    let centre = pos2((quad[0].x + quad[1].x + quad[2].x + quad[3].x) / 4.0, (quad[0].y + quad[1].y + quad[2].y + quad[3].y) / 4.0);
    // signed distance from edge i's line, positive towards the crop's centre
    let side = |i: usize, q: Pos2| -> Option<f32> {
        let (a, b) = (quad[i % 4], quad[(i + 1) % 4]);
        let n = vec2(a.y - b.y, b.x - a.x);
        let orient = n.dot(centre - a);
        // an edge the centre lies on: the box is flat
        (orient.abs() > 1e-6 * n.length().max(1.0)).then(|| n.dot(q - a) * orient.signum())
    };
    // Sutherland–Hodgman: the part of a convex polygon where `keep` is at or above zero
    let clip = |poly: &[Pos2], keep: &dyn Fn(Pos2) -> Option<f32>| -> Option<Vec<Pos2>> {
        let mut out = Vec::with_capacity(poly.len() + 2);
        for (k, &a) in poly.iter().enumerate() {
            let b = *poly.get((k + 1) % poly.len())?;
            let (da, db) = (keep(a)?, keep(b)?);
            if da >= 0.0 {
                out.push(a);
            }
            if (da < 0.0) != (db < 0.0) && (da - db).abs() > f32::EPSILON {
                let t = da / (da - db);
                out.push(a + (b - a) * t);
            }
        }
        Some(out)
    };
    let photo = [img.left_top(), img.right_top(), img.right_bottom(), img.left_bottom()];
    for i in 0..4 {
        let outside = |q: Pos2| side(i, q).map(|d| -d);
        let inside_next = |q: Pos2| side(i + 1, q);
        let Some(strip) = clip(&photo, &outside).and_then(|poly| clip(&poly, &inside_next)) else { return egui::epaint::Mesh::default() };
        if strip.len() < 3 {
            continue;
        }
        let base = mesh.vertices.len() as u32;
        mesh.vertices.extend(strip.iter().map(|&pos| egui::epaint::Vertex { pos, uv: Pos2::ZERO, color }));
        for k in 1..(strip.len() as u32).saturating_sub(1) {
            mesh.indices.extend([base, base + k, base + k + 1]);
        }
    }
    mesh
}

fn frame_crop_quad(d: &DevelopSettings, frame: &Frame) -> [Point; 4] {
    let f = Frame { crop: d.crop.geometry, ..frame.clone() };
    f.crop_quad_norm()
}

/// Straighten tool: drag along a horizon (or a vertical) to set the crop angle; double-click = Auto.
/// The image is shown unrotated in the crop view, so the line's on-screen angle is its image angle.
/// `held`: drawn with ⌘ held in the crop tool, which stays active afterwards.
fn straighten_overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, held: bool) {
    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    if !held && resp.double_clicked() {
        app.auto_straighten(ui.ctx());
        app.ui.tool.clear();
        app.gesture = None;
        return;
    }
    if resp.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        app.gesture = Some(Gesture::StraightenLine { a: q });
    }
    let Some(Gesture::StraightenLine { a }) = app.gesture.clone() else { return };
    let Some(b) = resp.interact_pointer_pos().or(resp.hover_pos()) else { return };
    let p = ui.painter();
    p.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(140)));
    p.line_segment([a, b], Stroke::new(1.5, Color32::WHITE));
    if resp.drag_stopped() {
        app.gesture = None;
        let v = b - a;
        if v.length() > 8.0 {
            let mut deg = v.y.atan2(v.x).to_degrees() as f64;
            // fold to the nearest axis: a line closer to vertical straightens plumb lines
            while deg > 45.0 {
                deg -= 90.0;
            }
            while deg < -45.0 {
                deg += 90.0;
            }
            let _ = app.run("crop.straighten", json!({"angle": (-deg * 100.0).round() / 100.0}));
        }
        if !held {
            app.ui.tool.clear();
        }
    }
}
