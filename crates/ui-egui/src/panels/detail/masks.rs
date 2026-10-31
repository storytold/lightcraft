use super::geometry::CanvasMap;
use super::{
    Color32, DevelopSettings, Gesture, LightcraftApp, MaskShape, Point, Pos2, Rect, Sense, Stroke, StrokeKind, Tokens, ToolOverlay, json, pos2,
    register, vec2,
};

/// What the user did to a face box this frame.
pub(super) enum RegionEdit {
    /// The × was clicked.
    Remove(usize),
    /// A handle drag ended: the region's new box (normalized, upright frame).
    Resize(usize, lightcraft_geom::Rect),
    /// A name was typed or picked for the face.
    Name(usize, String),
    /// The name box's offer to set up face recognition was pressed.
    SetUp,
}

/// Handles of a box: (x, y) as fractions of its width and height, and the cursor they show.
const REGION_HANDLES: [(f32, f32, egui::CursorIcon); 8] = [
    (0.0, 0.0, egui::CursorIcon::ResizeNwSe),
    (0.5, 0.0, egui::CursorIcon::ResizeVertical),
    (1.0, 0.0, egui::CursorIcon::ResizeNeSw),
    (1.0, 0.5, egui::CursorIcon::ResizeHorizontal),
    (1.0, 1.0, egui::CursorIcon::ResizeNwSe),
    (0.5, 1.0, egui::CursorIcon::ResizeVertical),
    (0.0, 1.0, egui::CursorIcon::ResizeNeSw),
    (0.0, 0.5, egui::CursorIcon::ResizeHorizontal),
];

/// Face/pet/focus regions read from XMP (MWG-RS), drawn as boxes over the photo. Hovering a box shows
/// a × in its corner and eight resize handles. A drag previews the new box live and is reported once,
/// on release (one undo step); the × reports the region to remove. Both are catalog-only edits:
/// LightCraft doesn't write regions to XMP. Regions are stored on the upright (EXIF-oriented) photo;
/// `orient` is the user's Rotate / Flip on top of it, which the loupe's normalized frame includes.
/// What the face/pet/focus regions of a photo are drawn from.
pub(super) struct RegionContext<'a> {
    pub photo: &'a lightcraft_catalog::Photo,
    /// the user's Rotate / Flip on top of the upright photo
    pub orient: lightcraft_geom::Orientation,
    pub hints: Option<&'a crate::panels::faces::Hints>,
    /// the names to offer when a region is named
    pub people: &'a [String],
    pub setup: crate::panels::faces::Setup,
}

pub(super) fn region_overlay(
    ui: &egui::Ui,
    p: &egui::Painter,
    map: &CanvasMap,
    regions: &RegionContext,
    editing: &mut Option<crate::state::NameEdit>,
) -> Option<RegionEdit> {
    let RegionContext { photo, orient, hints, people, setup } = *regions;
    let t = Tokens::get(p.ctx());
    let clip = p.clip_rect();
    let pointer = ui.input(|i| i.pointer.hover_pos());
    // the box being dragged: (region index, its box so far)
    let drag_key = egui::Id::unique("region-drag");
    let live: Option<(usize, lightcraft_geom::Rect)> = ui.data(|d| d.get_temp(drag_key));
    let mut edit = None;
    for (index, r) in photo.meta.regions.iter().enumerate() {
        let dragging = live.filter(|(i, _)| *i == index);
        let norm = orient.map_norm_rect(dragging.map_or(r.rect, |(_, n)| n));
        let rect = Rect::from_two_pos(map.screen(Point::new(norm.x0, norm.y0)), map.screen(Point::new(norm.x1, norm.y1)));
        // white with a black keyline just outside it, so the box shows on any background
        p.rect_stroke(rect.expand(1.0), 0.0, Stroke::new(1.0, Color32::from_black_alpha(190)), StrokeKind::Outside);
        p.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Outside);
        if dragging.is_some() || (live.is_none() && pointer.is_some_and(|h| rect.expand(8.0).contains(h))) {
            for (h, (fx, fy, cursor)) in REGION_HANDLES.iter().enumerate() {
                let c = pos2(rect.left() + fx * rect.width(), rect.top() + fy * rect.height());
                let resp = ui.interact(Rect::from_center_size(c, vec2(14.0, 14.0)), egui::Id::unique(("region-handle", index, h)), Sense::drag());
                crate::widgets::announce(&resp, crate::i18n::tr("Resize region"));
                if resp.hovered() || resp.dragged() {
                    ui.ctx().set_cursor_icon(*cursor);
                }
                let sq = Rect::from_center_size(c, vec2(7.0, 7.0));
                p.rect_filled(sq, 0.0, if resp.hovered() || resp.dragged() { Color32::from_rgb(120, 190, 255) } else { Color32::WHITE });
                p.rect_stroke(sq, 0.0, Stroke::new(1.0, Color32::from_black_alpha(220)), StrokeKind::Outside);
                if resp.dragged()
                    && let Some(pp) = resp.interact_pointer_pos()
                {
                    // move the dragged edge(s) to the pointer, never closer than 12 px to the opposite one
                    let mut n = rect;
                    if *fx == 0.0 {
                        n.min.x = pp.x.min(n.max.x - 12.0);
                    } else if *fx == 1.0 {
                        n.max.x = pp.x.max(n.min.x + 12.0);
                    }
                    if *fy == 0.0 {
                        n.min.y = pp.y.min(n.max.y - 12.0);
                    } else if *fy == 1.0 {
                        n.max.y = pp.y.max(n.min.y + 12.0);
                    }
                    let (a, b) = (map.norm(n.min), map.norm(n.max));
                    let shown = lightcraft_geom::Rect {
                        x0: a.x.min(b.x).clamp(0.0, 1.0),
                        y0: a.y.min(b.y).clamp(0.0, 1.0),
                        x1: a.x.max(b.x).clamp(0.0, 1.0),
                        y1: a.y.max(b.y).clamp(0.0, 1.0),
                    };
                    // back to the upright frame the region is stored in
                    let new = orient.inverse().map_norm_rect(shown);
                    ui.data_mut(|d| d.insert_temp(drag_key, (index, new)));
                    ui.ctx().request_repaint();
                }
                if resp.drag_stopped() {
                    if let Some((i, new)) = ui.data(|d| d.get_temp::<(usize, lightcraft_geom::Rect)>(drag_key)) {
                        edit = Some(RegionEdit::Resize(i, new));
                    }
                    ui.data_mut(|d| d.remove_temp::<(usize, lightcraft_geom::Rect)>(drag_key));
                }
            }
        }
        if dragging.is_none() && live.is_none() && pointer.is_some_and(|h| rect.expand(3.0).contains(h)) {
            let xr = Rect::from_center_size(pos2(rect.right() - 13.0, rect.top() + 13.0), vec2(16.0, 16.0));
            let resp = ui.interact(xr, egui::Id::unique(("region-x", index)), Sense::click());
            crate::widgets::announce(&resp, crate::i18n::tr("Remove region"));
            register(ui, format!("regionRemove:{index}"), xr);
            let fill = Color32::from_black_alpha(if resp.hovered() { 235 } else { 190 });
            p.rect_filled(xr, 2.0, fill);
            p.rect_stroke(xr, 2.0, Stroke::new(1.0, Color32::from_white_alpha(120)), StrokeKind::Inside);
            let (c, m) = (xr.center(), 3.5);
            let cross = Stroke::new(1.4, if resp.hovered() { Color32::WHITE } else { Color32::from_gray(210) });
            p.line_segment([c - vec2(m, m), c + vec2(m, m)], cross);
            p.line_segment([c - vec2(m, -m), c + vec2(m, -m)], cross);
            if resp.on_hover_text(crate::i18n::tr("Remove this face box (undo with Edit ▸ Undo)")).clicked() {
                edit = Some(RegionEdit::Remove(index));
            }
        }
        // what the label says: the name, a guess to confirm ("Jane Doe?"), or on hover an invitation to name the face
        let is_face = r.kind == lightcraft_meta::RegionKind::Face;
        let hint = hints.and_then(|h| h.by_index.get(&index));
        let hovered = pointer.is_some_and(|h| rect.expand(3.0).contains(h));
        let editing_this = editing.as_ref().is_some_and(|e| e.photo == photo.id.0 && e.index == index);
        let (text, text_color, fill_alpha, invitation) = match (&r.name, hint.and_then(|h| h.suggestion.as_ref())) {
            (Some(n), _) => (n.clone(), Color32::from_gray(225), 235, false),
            (None, Some((n, _))) if is_face => (format!("{n}?"), Color32::from_gray(185), 200, false),
            (None, None) if is_face && live.is_none() => ("Add name".to_string(), Color32::from_gray(150), 190, true),
            _ => continue,
        };
        // the name in a dark label with a caret, centred above the box (below it when there is no room)
        let name = &text;
        let g = p.layout_no_wrap(name.clone(), t.font(13.0), text_color);
        let (pad, caret) = (vec2(14.0, 7.0), 5.0);
        let size = g.size() + pad * 2.0;
        let above = rect.top() - caret - size.y >= clip.top();
        let (top, tip, base) = if above {
            (rect.top() - caret - size.y, rect.top() - 1.0, rect.top() - caret - 1.0)
        } else {
            (rect.bottom() + caret, rect.bottom() + 1.0, rect.bottom() + caret + 1.0)
        };
        let left = (rect.center().x - size.x / 2.0).clamp(clip.left(), (clip.right() - size.x).max(clip.left()));
        let label = Rect::from_min_size(pos2(left, top), size);
        // the invitation shows while the pointer is on the box *or on the label* (and the gap between), so it can be reached
        if invitation && !(hovered || editing_this || pointer.is_some_and(|h| rect.union(label).expand(3.0).contains(h))) {
            continue;
        }
        let fill = Color32::from_rgba_unmultiplied(56, 56, 56, fill_alpha);
        p.rect_filled(label, 3.0, fill);
        p.rect_stroke(label, 3.0, Stroke::new(1.0, Color32::from_black_alpha(160)), StrokeKind::Inside);
        let cx = rect.center().x.clamp(label.left() + caret + 4.0, label.right() - caret - 4.0);
        p.add(egui::Shape::convex_polygon(vec![pos2(cx - caret, base), pos2(cx + caret, base), pos2(cx, tip)], fill, Stroke::NONE));
        p.galley(label.min + pad, g, text_color);
        if is_face && live.is_none() {
            // a click on the label opens the name box, filled with the name or the guess
            let resp = ui.interact(label, egui::Id::unique(("region-label", index)), Sense::click());
            crate::widgets::announce(&resp, crate::i18n::tr("Click to name this face"));
            register(ui, format!("regionLabel:{index}"), label);
            if resp.on_hover_text(crate::i18n::tr("Click to name this face")).clicked() {
                let start = r.name.clone().or_else(|| hint.and_then(|h| h.suggestion.as_ref().map(|(n, _)| n.clone()))).unwrap_or_default();
                *editing = Some(crate::state::NameEdit { photo: photo.id.0, index, text: start, fresh: true });
            }
        }
        if editing_this && let Some(e) = editing.as_mut() {
            let candidates = hint.map(|h| h.candidates.as_slice()).unwrap_or(&[]);
            match crate::panels::faces::name_editor(ui.ctx(), pos2(label.left(), rect.bottom() + 8.0), e, candidates, people, setup) {
                crate::panels::faces::Editor::Submit(name) => edit = Some(RegionEdit::Name(index, name)),
                crate::panels::faces::Editor::Setup => edit = Some(RegionEdit::SetUp),
                crate::panels::faces::Editor::Cancel => *editing = None,
                crate::panels::faces::Editor::Open => {}
            }
        }
    }
    edit
}

// ------------------------------------------------------------------------ masks

/// Where a mask component's pin sits (normalized), for shapes that have a place on the photo: an
/// Object at its first click, a Describe mask on what its segmentation found (issue #3049).
pub(crate) fn component_pin(shape: &MaskShape) -> Option<Point> {
    match shape {
        MaskShape::Radial { center, .. } => Some(*center),
        MaskShape::Linear { start, end } => Some(Point::new((start.x + end.x) / 2.0, (start.y + end.y) / 2.0)),
        MaskShape::Brush { strokes } => strokes.iter().find(|s| !s.erase).and_then(|s| s.points.first()).copied(),
        MaskShape::Object { hint, seg, .. } => hint.first().copied().or_else(|| seg.as_ref().and_then(seg_center)),
        MaskShape::Prompt { seg, .. } => seg.as_ref().and_then(seg_center),
        _ => None,
    }
}

/// The middle of what a segmentation selects (the mean of its cells inside the mask), else of the
/// part of the image it covers; `None` when it is damaged.
fn seg_center(seg: &lightcraft_develop::SegMask) -> Option<Point> {
    let [x0, y0, x1, y1] = seg.bounds()?;
    let logits = seg.logits()?;
    let side = seg.side as usize;
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0usize);
    for (i, _) in logits.iter().enumerate().filter(|(_, l)| **l > 0.0) {
        sx += (i % side) as f64 + 0.5;
        sy += (i / side) as f64 + 0.5;
        n += 1;
    }
    let cells = n as f64 * side as f64;
    let (u, v) = if n == 0 { (0.5, 0.5) } else { (sx / cells, sy / cells) };
    Some(Point::new(x0 + u * (x1 - x0), y0 + v * (y1 - y0)))
}

/// `shape` moved by `dn` (normalized); `handle` 1/2 = a linear gradient's start/end set to `at`.
fn moved_shape(shape: &MaskShape, handle: u8, dn: Point, at: Point, map: &CanvasMap, alt: bool) -> MaskShape {
    let mv = |p: Point| Point::new(p.x + dn.x, p.y + dn.y);
    match shape.clone() {
        MaskShape::Radial { center, rx, ry, angle, feather, invert } => match handle {
            1 | 3 => {
                let l = frame_long_norm(map);
                let dx = (at.x - center.x) / l.0;
                let dy = (at.y - center.y) / l.1;
                let (s, co) = angle.to_radians().sin_cos();
                let new_rx = (dx * co + dy * s).abs().max(0.01);
                let mut new_ry = ry;
                if alt {
                    new_ry = ry * (new_rx / rx.max(0.01));
                }
                if (new_rx - new_ry).abs() / new_rx.max(new_ry) < 0.05 {
                    new_ry = new_rx;
                }
                MaskShape::Radial { center, rx: new_rx, ry: new_ry, angle, feather, invert }
            }
            2 | 4 => {
                let l = frame_long_norm(map);
                let dx = (at.x - center.x) / l.0;
                let dy = (at.y - center.y) / l.1;
                let (s, co) = angle.to_radians().sin_cos();
                let new_ry = (dx * (-s) + dy * co).abs().max(0.01);
                let mut new_rx = rx;
                if alt {
                    new_rx = rx * (new_ry / ry.max(0.01));
                }
                if (new_rx - new_ry).abs() / new_rx.max(new_ry) < 0.05 {
                    new_rx = new_ry;
                }
                MaskShape::Radial { center, rx: new_rx, ry: new_ry, angle, feather, invert }
            }
            5 => {
                let l = frame_long_norm(map);
                let dx = (at.x - center.x) / l.0;
                let dy = (at.y - center.y) / l.1;
                let new_angle = dy.atan2(dx).to_degrees();
                MaskShape::Radial { center, rx, ry, angle: new_angle, feather, invert }
            }
            _ => MaskShape::Radial { center: mv(center), rx, ry, angle, feather, invert },
        },
        MaskShape::Linear { start, end } => match handle {
            1 => MaskShape::Linear { start: at, end },
            2 => MaskShape::Linear { start, end: at },
            _ => MaskShape::Linear { start: mv(start), end: mv(end) },
        },
        MaskShape::Brush { mut strokes } => {
            for st in &mut strokes {
                st.points.iter_mut().for_each(|p| *p = mv(*p));
            }
            MaskShape::Brush { strokes }
        }
        s => s,
    }
}

/// Hit the geometric ellipse, independently of feather/inversion. Radii use long-edge units,
/// so undo the same scaling and rotation used to draw its outline before testing containment.
fn radial_body_contains(shape: &MaskShape, at: Point, map: &CanvasMap) -> bool {
    let MaskShape::Radial { center, rx, ry, angle, .. } = shape else { return false };
    if !rx.is_finite() || !ry.is_finite() || *rx <= 0.0 || *ry <= 0.0 {
        return false;
    }
    let l = frame_long_norm(map);
    let dx = (at.x - center.x) / l.0;
    let dy = (at.y - center.y) / l.1;
    let (s, co) = angle.to_radians().sin_cos();
    let x = (dx * co + dy * s) / rx;
    let y = (-dx * s + dy * co) / ry;
    x * x + y * y <= 1.0
}

/// Colour Range eyedropper: a click samples the colour under it for the selected mask's
/// colour range, at normalized frame coordinates (crop, rotation and zoom aware);
/// ⇧-click adds a sample (up to 5).
pub(super) fn color_range_pick(app: &mut LightcraftApp, ui: &egui::Ui, resp: &egui::Response, map: &CanvasMap) {
    if resp.clicked()
        && let Some(q) = resp.interact_pointer_pos()
    {
        let n = map.norm(q);
        let add = ui.input(|i| i.modifiers.shift);
        if let Err(e) = app.run("mask.sampleColor", json!({"x": n.x, "y": n.y, "add": add})) {
            app.toast(ui.ctx(), e);
        }
    }
}

/// The Masking tool on the photo: outlines and handles of the selected mask, a pin per mask
/// component (click selects its mask, drag moves the component), and brush painting. The mask
/// itself shows as a rendered overlay ([`view_overlay`]).
pub(super) fn mask_overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
    let active = app.session.active_mask;
    let clip = app.canvas_rect.unwrap_or(map.rect);
    let p = &ui.painter_at(clip);
    // (mask, component, handle, screen position) of everything that can be grabbed
    let mut grips: Vec<(u32, usize, u8, Pos2)> = Vec::new();
    // pins follow Show Pins and the Tool Overlay mode (Shift+H); Auto hides them while brushing
    let (mode, live) = (app.ui.tool_overlay, pins_live(app, resp, map));
    let mut pins = p.clone();
    if mode == ToolOverlay::Auto && matches!(app.gesture, Some(Gesture::Brush { .. })) {
        pins.set_invisible();
    }
    for m in &d.masks {
        let sel = Some(m.id) == active;
        // Ctrl+Shift+O (`mask_overlay_all`) shows every visible mask's region; outline each one
        // too, not just the selected mask. Handles and the rotation grip stay on the selected
        // mask so nothing else becomes draggable. The component check keeps this in step with the
        // painted region (`view_overlay` at detail.rs:1354 only covers masks with components).
        let show_outline = sel || (app.ui.mask_overlay_all && m.visible && !m.components.is_empty());
        for (ci, c) in m.components.iter().enumerate() {
            if show_outline {
                component_outline(ui, p, map, (m.id, ci), &c.shape, sel, &mut grips);
            }
            if app.ui.mask_pins
                && mode.shows(sel, live)
                && let Some(at) = component_pin(&c.shape)
            {
                let q = map.screen(at);
                pin(&pins, q, sel);
                register(ui, format!("maskPin:{}:{ci}", m.id), Rect::from_center_size(q, vec2(14.0, 14.0)));
                grips.push((m.id, ci, 0, q));
            }
        }
    }
    mask_keys(app, ui, map, d, &grips);
    object_marks(app, p, map, d, active);
    // Colour Range "Pick": the Masking panel owns the loupe's clicks (general_interaction
    // never runs while it is open), so the eyedropper is handled here too.
    if app.ui.tool == "colorRange" {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        color_range_pick(app, ui, resp, map);
        return;
    }
    if app.ui.tool == "object" {
        object_tool(app, ui, resp, map, d, &grips);
        return;
    }
    if app.ui.tool == "brush" {
        brush_tool(app, ui, resp, map, p, clip, d);
        return;
    }
    grip_gestures(app, ui, resp, map, d, &grips);
}

/// The outline of one mask component, and (on the selected mask) its grips, registered and
/// collected into `grips`.
fn component_outline(
    ui: &egui::Ui,
    p: &egui::Painter,
    map: &CanvasMap,
    at: (u32, usize),
    shape: &MaskShape,
    sel: bool,
    grips: &mut Vec<(u32, usize, u8, Pos2)>,
) {
    let (mask, ci) = at;
    let stroke_a: u8 = if sel { 230 } else { 170 };
    match shape {
        MaskShape::Radial { center, rx, ry, angle, .. } => {
            let pts: Vec<Pos2> = (0..64)
                .map(|i| {
                    let a = i as f64 / 64.0 * std::f64::consts::TAU;
                    let (s, co) = angle.to_radians().sin_cos();
                    let (x, y) = (rx * a.cos(), ry * a.sin());
                    let l = frame_long_norm(map);
                    map.screen(Point::new(center.x + (x * co - y * s) * l.0, center.y + (x * s + y * co) * l.1))
                })
                .collect();
            p.add(egui::Shape::closed_line(pts, Stroke::new(1.5, Color32::from_white_alpha(stroke_a))));

            if sel {
                let l = frame_long_norm(map);
                for (i, a_deg) in [0.0_f64, 90.0_f64, 180.0_f64, 270.0_f64].into_iter().enumerate() {
                    let a_local = a_deg.to_radians();
                    let (s, co) = angle.to_radians().sin_cos();
                    let (x, y) = (rx * a_local.cos(), ry * a_local.sin());
                    let q = map.screen(Point::new(center.x + (x * co - y * s) * l.0, center.y + (x * s + y * co) * l.1));
                    p.circle_stroke(q, 5.0, Stroke::new(1.5, Color32::WHITE));
                    register(ui, format!("maskHandle:{}:{ci}:{}", mask, i + 1), Rect::from_center_size(q, vec2(14.0, 14.0)));
                    grips.push((mask, ci, i as u8 + 1, q));
                }

                let (s, co) = angle.to_radians().sin_cos();
                let rot_r = rx + 0.08;
                let rot_q = map.screen(Point::new(center.x + (rot_r * co) * l.0, center.y + (rot_r * s) * l.1));
                let h1_q = map.screen(Point::new(center.x + (rx * co) * l.0, center.y + (rx * s) * l.1));
                p.line_segment([h1_q, rot_q], Stroke::new(1.0, Color32::from_white_alpha(200)));
                p.circle_stroke(rot_q, 4.0, Stroke::new(1.5, Color32::WHITE));
                register(ui, format!("maskHandle:{}:{ci}:5", mask), Rect::from_center_size(rot_q, vec2(14.0, 14.0)));
                grips.push((mask, ci, 5, rot_q));
            }
        }
        MaskShape::Linear { start, end } => {
            let (a, b) = (map.screen(*start), map.screen(*end));
            let dir = (b - a).normalized();
            let perp = vec2(-dir.y, dir.x) * 3000.0;
            let (a1, a2): (u8, u8) = if sel { (230, 110) } else { (170, 80) };
            for (q, alpha) in [(a, a1), (b, a2)] {
                p.line_segment([q - perp, q + perp], Stroke::new(1.0, Color32::from_white_alpha(alpha)));
            }
            if sel {
                for (i, q) in [a, b].iter().enumerate() {
                    p.circle_stroke(*q, 5.0, Stroke::new(1.5, Color32::WHITE));
                    register(ui, format!("maskHandle:{}:{ci}:{i}", mask), Rect::from_center_size(*q, vec2(14.0, 14.0)));
                    grips.push((mask, ci, i as u8 + 1, *q));
                }
            }
        }
        _ => {}
    }
}

/// Object selections (SAM 3) of the selected mask: each click, green to include, red to exclude
/// (clicks still on their way to the model included).
fn object_marks(app: &LightcraftApp, p: &egui::Painter, map: &CanvasMap, d: &DevelopSettings, active: Option<u32>) {
    let pending = app.session.segmenter.pending_clicks().cloned();
    for m in d.masks.iter().filter(|m| Some(m.id) == active) {
        for (k, c) in m.components.iter().enumerate() {
            if let MaskShape::Object { hint, exclude, .. } = &c.shape {
                let (hint, exclude) = match pending.as_ref().filter(|q| (q.mask, q.comp) == (m.id, k)) {
                    Some(q) => (&q.hint, &q.exclude),
                    None => (hint, exclude),
                };
                for (pts, col) in [(hint, Color32::from_rgb(40, 200, 90)), (exclude, Color32::from_rgb(230, 60, 60))] {
                    for q in pts {
                        let q = map.screen(*q);
                        p.circle_filled(q, 5.0, col);
                        p.circle_stroke(q, 5.0, Stroke::new(1.5, Color32::WHITE));
                    }
                }
            }
        }
    }
}

/// The mask whose pin is under `q` (within the 10 px [`grip_gestures`] grabs at), not counting
/// the `active` mask's own pins.
fn other_pin_at(grips: &[(u32, usize, u8, Pos2)], active: Option<u32>, q: Pos2) -> Option<u32> {
    grips
        .iter()
        .filter(|g| Some(g.0) != active && g.2 == 0 && g.3.distance(q) < 10.0)
        .min_by(|a, b| a.3.distance(q).total_cmp(&b.3.distance(q)))
        .map(|g| g.0)
}

/// Whether `q` is on one of the `active` mask's own pins (within the same 10 px).
fn own_pin_at(grips: &[(u32, usize, u8, Pos2)], active: Option<u32>, q: Pos2) -> bool {
    active.is_some_and(|a| grips.iter().any(|g| g.0 == a && g.2 == 0 && g.3.distance(q) < 10.0))
}

/// Object tool: a click includes what's under it, ⌥-click leaves it out. A click on another
/// mask's pin selects that mask instead of adding a click to the selected one (issue #3049);
/// the tool then stays on only if the newly selected mask has an Object selection to refine. A
/// click on the selected mask's own pin does nothing: its first click is already there, and a
/// near-copy of it would use up one of the mask's limited Object clicks.
fn object_tool(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    resp: &egui::Response,
    map: &CanvasMap,
    d: &DevelopSettings,
    grips: &[(u32, usize, u8, Pos2)],
) {
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if resp.clicked()
        && let Some(q) = resp.interact_pointer_pos()
    {
        if let Some(mid) = other_pin_at(grips, app.session.active_mask, q) {
            let _ = app.run("mask.select", json!({"id": mid}));
            let has_object =
                d.masks.iter().find(|m| m.id == mid).is_some_and(|m| m.components.iter().any(|c| matches!(c.shape, MaskShape::Object { .. })));
            if !has_object {
                app.ui.tool.clear();
            }
            return;
        }
        if own_pin_at(grips, app.session.active_mask, q) {
            return;
        }
        let n = map.norm(q);
        let exclude = ui.input(|i| i.modifiers.alt);
        match app.run_typed("mask.objectPoint", json!({"x": n.x, "y": n.y, "exclude": exclude})) {
            Ok(_) => {
                if let Some(mid) = app.session.active_mask {
                    app.ui.detail_due = Some((app.tick_now() + 1.0, mid));
                }
            }
            Err(e) => app.ai_error(ui.ctx(), e, Some(("object", "new"))),
        }
    }
}

/// Brush tool: the cursor, the stroke being painted and its commit.
fn brush_tool(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    resp: &egui::Response,
    map: &CanvasMap,
    p: &egui::Painter,
    clip: Rect,
    d: &DevelopSettings,
) {
    let active = app.session.active_mask;
    let long = (map.rect.width().max(map.rect.height())) as f64;
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
            // a held, unmoving pointer records nothing: coverage builds by passes, not time (#518)
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
    // the stroke being painted (the render shows it once it's committed), with the committed
    // stroke's dab spacing, feather and flow (issue #517), tinted only when the overlay will
    // show it: with the overlay hidden (O, or a mask slider hid it) or a colour overlay at
    // opacity 0, the photo stays untinted while painting, so dodging and burning is judged on
    // the photo (issue #2549)
    use lightcraft_pipeline::MaskView;
    let colored = matches!(MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default(), MaskView::Color | MaskView::ColorOnBw);
    // the all-masks overlay ([`view_overlay`]) shows only visible masks, and gives way to the
    // selected mask's when none has anything in it (a stroke goes into the selected mask, or a new one)
    let visible = active.and_then(|id| d.masks.iter().find(|m| m.id == id)).is_none_or(|m| m.visible);
    let all = app.ui.mask_overlay_all && (visible || d.masks.iter().any(|m| m.visible && !m.components.is_empty()));
    let shown = if all { visible } else { app.ui.mask_overlay } && !(colored && app.ui.mask_overlay_opacity <= 0.0);
    if shown && let Some(Gesture::Brush { points, pressure }) = &app.gesture {
        let screen: Vec<Pos2> = points.iter().map(|q| map.screen(*q)).collect();
        // opacity 0 only gets here in the image views, which ignore it: a fixed tint there
        let opacity = if app.ui.mask_overlay_opacity > 0.0 { app.ui.mask_overlay_opacity / 100.0 } else { 110.0 / 255.0 };
        let params = crate::brush_live::LiveParams {
            clip,
            scale: ui.ctx().pixels_per_point(),
            r,
            hard: r * (1.0 - (app.ui.brush_feather / 100.0).clamp(0.0, 1.0)),
            flow: (app.ui.brush_flow / 100.0).clamp(0.0, 1.0),
            density: 1.0,
            color: app.ui.mask_overlay_color,
            opacity: opacity.clamp(0.0, 1.0),
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
        // holding ⌥ (Alt) paints with the other mode: Erase while adding, Add while erasing
        let (alt, shift) = resp.ctx.input(|i| (i.modifiers.alt, i.modifiers.shift));
        let erase = app.ui.brush_erase != alt;
        // a ⇧-click draws a straight line from where the last stroke ended, as in Lightroom
        let connect = shift && resp.clicked() && pts.len() == 1;
        let _ = app.run(
            "mask.brushStroke",
            json!({"points": pts, "size": app.ui.brush_size, "feather": app.ui.brush_feather, "flow": app.ui.brush_flow,
                   "erase": erase, "autoMask": app.ui.brush_auto_mask, "pressure": pressure, "connect": connect}),
        );
    }
}

/// Grabbing mask grips and component bodies: select, drag, release.
fn grip_gestures(
    app: &mut LightcraftApp,
    ui: &egui::Ui,
    resp: &egui::Response,
    map: &CanvasMap,
    d: &DevelopSettings,
    grips: &[(u32, usize, u8, Pos2)],
) {
    let active = app.session.active_mask;
    // the grip under the pointer: the selected mask's first, then the nearest
    let hit = |q: Pos2| {
        grips
            .iter()
            .filter(|g| g.3.distance(q) < 10.0)
            .min_by(|a, b| {
                (Some(a.0) != active, a.3.distance(q)).partial_cmp(&(Some(b.0) != active, b.3.distance(q))).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
    };
    if resp.clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && let Some((mid, ..)) = hit(q)
        && Some(mid) != active
    {
        let _ = app.run("mask.select", json!({"id": mid}));
    }
    if resp.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        let grip = hit(q).map(|(m, c, h, _)| (m, c, h)).or_else(|| {
            let m = d.masks.iter().find(|m| Some(m.id) == active)?;
            // Handles take precedence; otherwise grab the selected radial component under
            // the press, including components after the first in compound masks.
            let at = map.norm(q);
            if let Some(ci) = m.components.iter().position(|c| radial_body_contains(&c.shape, at, map)) {
                return Some((m.id, ci, 0));
            }
            // anywhere on the photo drags the selected linear gradient
            matches!(m.components.first()?.shape, MaskShape::Linear { .. }).then_some((m.id, 0, 0))
        });
        if let Some((mask, comp, handle)) = grip
            && let Some(original_shape) = d.masks.iter().find(|m| m.id == mask).and_then(|m| m.components.get(comp)).map(|c| c.shape.clone())
        {
            if Some(mask) != active {
                let _ = app.run("mask.select", json!({"id": mask}));
            }
            let _ = app.run("develop.beginInteraction", json!({"label": "Edit Mask"}));
            app.gesture = Some(Gesture::MaskHandle { mask, comp, handle, original_shape });
        }
    }
    if resp.dragged()
        && let (Some(Gesture::MaskHandle { mask, comp, handle, original_shape }), Some(q)) = (app.gesture.clone(), resp.interact_pointer_pos())
    {
        let n = map.norm(q);
        let press_origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(q);
        let n0 = map.norm(press_origin);
        let dn = Point::new(n.x - n0.x, n.y - n0.y);
        let alt = ui.input(|i| i.modifiers.alt);
        let new_shape = moved_shape(&original_shape, handle, dn, n, map, alt);
        let _ = app.run("mask.update", json!({"id": mask, "component": comp, "shape": new_shape}));
    }
    if resp.drag_stopped() && matches!(app.gesture, Some(Gesture::MaskHandle { .. })) {
        app.gesture = None;
        let _ = app.run("develop.endInteraction", json!({}));
    }
}

/// A mask handle's name for screen readers: `handle` 0 is the component's pin, 1..5 a radial
/// gradient's right / bottom / left / top edge and rotation, 1..2 a linear gradient's start and end.
fn mask_handle_name(mask: &str, shape: &MaskShape, handle: u8) -> String {
    let part = match (shape, handle) {
        (_, 0) => "pin",
        (MaskShape::Radial { .. }, 1) => "right edge",
        (MaskShape::Radial { .. }, 2) => "bottom edge",
        (MaskShape::Radial { .. }, 3) => "left edge",
        (MaskShape::Radial { .. }, 4) => "top edge",
        (MaskShape::Radial { .. }, _) => "rotation",
        (MaskShape::Linear { .. }, 1) => "start",
        (MaskShape::Linear { .. }, _) => "end",
        _ => "handle",
    };
    format!("{mask} {}", crate::i18n::tr(part))
}

/// Keyboard stand-ins for the mask handles and pins: an arrow press moves the focused one by a
/// step (one undo step), as dragging it would; Return on a pin selects its mask.
fn mask_keys(app: &mut LightcraftApp, ui: &mut egui::Ui, map: &CanvasMap, d: &DevelopSettings, grips: &[(u32, usize, u8, Pos2)]) {
    for &(mid, ci, handle, q) in grips {
        let Some(m) = d.masks.iter().find(|m| m.id == mid) else { continue };
        let Some(shape) = m.components.get(ci).map(|c| &c.shape) else { continue };
        let at = map.norm(q);
        let mask_name = if m.name.trim().is_empty() { format!("{} {mid}", crate::i18n::tr("Mask")) } else { m.name.clone() };
        let name = mask_handle_name(&mask_name, shape, handle);
        let keys = crate::canvas_keys::handle(ui, &format!("mask:{mid}:{ci}:{handle}"), &name, crate::canvas_keys::position_text(at.x, at.y), q);
        if keys.activated && app.session.active_mask != Some(mid) {
            let _ = app.run("mask.select", json!({"id": mid}));
        }
        let Some((dx, dy)) = keys.delta else { continue };
        if app.session.active_mask != Some(mid) {
            let _ = app.run("mask.select", json!({"id": mid}));
        }
        let to = Point::new(at.x + dx, at.y + dy);
        let new_shape = moved_shape(shape, handle, Point::new(dx, dy), to, map, false);
        let _ = app.run("develop.beginInteraction", json!({"label": "Edit Mask"}));
        let _ = app.run("mask.update", json!({"id": mid, "component": ci, "shape": new_shape}));
        let _ = app.run("develop.endInteraction", json!({}));
    }
}

/// Long-edge units → normalized units for x and y.
fn frame_long_norm(map: &CanvasMap) -> (f64, f64) {
    // derive from the mapping: 1 normalized unit in x and y in screen points
    let o = map.screen(Point::new(0.0, 0.0));
    let x = map.screen(Point::new(1.0, 0.0)).distance(o) as f64;
    let y = map.screen(Point::new(0.0, 1.0)).distance(o) as f64;
    let l = x.max(y);
    (l / x.max(1e-9), l / y.max(1e-9))
}

/// Auto's condition for the Tool Overlay (Shift+H): the pointer is over the photo, or a pin is
/// being dragged.
pub(super) fn pins_live(app: &LightcraftApp, resp: &egui::Response, map: &CanvasMap) -> bool {
    resp.hover_pos().is_some_and(|q| map.rect.contains(q))
        || matches!(app.gesture, Some(Gesture::SpotMove { .. } | Gesture::MaskHandle { .. }))
        // a handle moved with the keyboard stays shown while it has the focus
        || crate::canvas_keys::has_focus(&resp.ctx)
}

pub(super) fn pin(p: &egui::Painter, c: Pos2, sel: bool) {
    p.circle_filled(c, 6.0, if sel { Color32::from_rgb(1, 101, 221) } else { Color32::from_gray(200) });
    p.circle_stroke(c, 6.0, Stroke::new(1.5, Color32::WHITE));
}

/// The pen pressure (0..1) reported this frame: the force of the latest touch or pen event that carries one (winit
/// reports a pen's pressure as a touch force, on the platforms that give it), `None` for a mouse.
pub(crate) fn pen_force(i: &egui::InputState) -> Option<f32> {
    i.events.iter().rev().find_map(|e| match e {
        egui::Event::Touch { force: Some(f), .. } if f.is_finite() => Some(f.clamp(0.0, 1.0)),
        _ => None,
    })
}

/// Record the pressure of the stroke's point `index`: `force` when the pen reported one this frame, else the last
/// known one. Points before the first reported force take it too; a stroke without any keeps no pressure list.
pub(crate) fn record_pressure(pressure: &mut Vec<f32>, index: usize, force: Option<f32>) {
    match (force, pressure.last().copied()) {
        (Some(f), _) if pressure.is_empty() => pressure.extend(std::iter::repeat_n(f, index + 1)),
        (Some(f), _) => pressure.push(f),
        (None, Some(last)) => pressure.push(last),
        (None, None) => {}
    }
}

#[cfg(test)]
mod pen_tests {
    use super::*;

    #[test]
    fn pen_pressure_is_recorded_per_point_and_absent_for_a_mouse() {
        let mut p = Vec::new();
        record_pressure(&mut p, 0, None);
        record_pressure(&mut p, 1, None);
        assert!(p.is_empty(), "a mouse: no pressure list");
        // the first force covers the points before it, a frame without one repeats the last
        record_pressure(&mut p, 2, Some(0.4));
        record_pressure(&mut p, 3, None);
        record_pressure(&mut p, 4, Some(0.9));
        assert_eq!(p, vec![0.4, 0.4, 0.4, 0.4, 0.9]);

        let mut i = egui::InputState::default();
        assert_eq!(pen_force(&i), None);
        let touch = |force| egui::Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId(1),
            phase: egui::TouchPhase::Move,
            pos: egui::pos2(1.0, 1.0),
            force,
        };
        i.events = vec![touch(Some(0.3)), touch(Some(f32::NAN)), touch(None)];
        assert_eq!(pen_force(&i), Some(0.3), "the latest usable force");
        i.events = vec![touch(Some(4.0))];
        assert_eq!(pen_force(&i), Some(1.0));
    }
}
