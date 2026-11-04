use super::geometry::CanvasMap;
use super::masks::{pin, pins_live};
use super::{Color32, DevelopSettings, Gesture, LightcraftApp, Point, Pos2, Rect, Stroke, ToolOverlay, json, register, vec2};
use crate::panels::spot_outline;
use lightcraft_develop::SpotMode;

// ------------------------------------------------------------------------ remove

/// What of each Remove spot can be grabbed, as last drawn: `(spot, is source, screen points of
/// its stroke, radius)`, the selected spot and the photo's long edge on screen (for [`grab_radius`]).
/// `brush_wheel` finds the spot under the pointer in it.
#[derive(Clone, Debug, Default)]
pub(super) struct SpotBodies {
    pub(super) bodies: Vec<(usize, bool, Vec<Pos2>, f32)>,
    pub(super) active: Option<usize>,
    pub(super) long: f64,
}

/// How far from its stroke a spot is drawn (its outline) and can be grabbed, on a photo whose long edge is `long` on screen.
pub(super) fn grab_radius(s: &lightcraft_develop::Spot, long: f64) -> f32 {
    feather_band(s.mode == SpotMode::Fill, (s.size * long) as f32, s.feather as f32).1 + OUTLINE_GAP
}
pub(super) const SPOT_BODIES: &str = "remove-spot-bodies";

/// The target or source among `bodies` under `q` (its pin or anywhere along its stroke) as `(spot, is
/// source, distance)`: the selected spot's first, then the nearest.
pub(super) fn spot_under(bodies: &[(usize, bool, Vec<Pos2>, f32)], active: Option<usize>, q: Pos2) -> Option<(usize, bool, f32)> {
    bodies
        .iter()
        .filter_map(|(spot, source, pts, r)| Some((*spot, *source, spot_outline::grab_distance(pts, q, r.max(8.0))?)))
        .min_by(|a, b| (Some(a.0) != active, a.2).partial_cmp(&(Some(b.0) != active, b.2)).unwrap_or(std::cmp::Ordering::Equal))
}

/// The Remove tool on the photo: every spot's outline and pin (the selected one with its source),
/// click a pin to select its spot, drag a target or source to move it, paint elsewhere to add one.
/// The Tool Overlay mode (Shift+H) says which spots show; a hidden spot can't be grabbed, so a
/// click there adds a new one.
pub(super) fn remove_overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
    let long = (map.rect.width().max(map.rect.height())) as f64;
    let active = app.session.active_spot.filter(|i| *i < d.spots.len());
    let (mode, live) = (app.ui.tool_overlay, pins_live(app, resp, map));
    // Auto also hides them while a new spot is painted
    let painting = mode == ToolOverlay::Auto && matches!(app.gesture, Some(Gesture::Spot { .. }));
    // while a spot's target or source is dragged, its target's outline is hidden, so the edge being matched shows
    let moving = match app.gesture {
        Some(Gesture::SpotMove { spot, .. }) => Some(spot),
        _ => None,
    };
    let p = ui.painter_at(app.canvas_rect.unwrap_or(map.rect));
    let mut pins = p.clone();
    if painting {
        pins.set_invisible();
    }
    // (spot, is source, screen centre, radius) of every pin: the keyboard handles
    let mut grips: Vec<(usize, bool, Pos2, f32)> = Vec::new();
    // (spot, is source, screen points of the stroke, radius) of everything that can be grabbed: the pin, and
    // anywhere within the radius of the stroke's points (the source's, shifted by its offset, for the selected spot)
    let mut bodies: Vec<(usize, bool, Vec<Pos2>, f32)> = Vec::new();
    let clip = p.clip_rect();
    for (i, s) in d.spots.iter().enumerate() {
        let band = feather_band(s.mode == SpotMode::Fill, (s.size * long) as f32, s.feather as f32);
        let r = grab_radius(s, long);
        let sel = Some(i) == active;
        if !mode.shows(sel, live) {
            continue;
        }
        let p = &pins;
        let target: Vec<Pos2> = s.points.iter().map(|q| map.screen(*q)).collect();
        let light = if sel { Stroke::new(1.25, Color32::from_white_alpha(230)) } else { Stroke::new(1.0, Color32::from_white_alpha(120)) };
        if touches(&target, r, clip) && moving != Some(i) {
            draw_spot(p, &target, band, light, false, sel);
        }
        let Some(&tq) = target.first() else { continue };
        if let Some(o) = s.source_offset.filter(|_| sel) {
            let source: Vec<Pos2> = s.points.iter().map(|q| map.screen(Point::new(q.x + o.x, q.y + o.y))).collect();
            if touches(&source, r, clip) {
                draw_spot(p, &source, band, Stroke::new(1.0, Color32::from_white_alpha(150)), true, false);
            }
            let sq = source[0];
            let dir = (tq - sq).normalized();
            let (from, by) = (sq + dir * r, (tq - sq) - dir * (2.0 * r).min((tq - sq).length()));
            p.arrow(from, by, Stroke::new(2.5, Color32::from_black_alpha(110)));
            p.arrow(from, by, Stroke::new(1.0, Color32::from_white_alpha(200)));
            register(ui, format!("spotSource:{i}"), Rect::from_center_size(sq, vec2(14.0, 14.0)));
            grips.push((i, true, sq, r));
            bodies.push((i, true, source, r));
        }
        pin(p, tq, sel);
        register(ui, format!("spotPin:{i}"), Rect::from_center_size(tq, vec2(14.0, 14.0)));
        grips.push((i, false, tq, r));
        bodies.push((i, false, target, r));
    }
    if spot_keys(app, ui, map, d, &grips) {
        return;
    }
    let r = (app.ui.remove_size as f64 * long) as f32;
    let band = feather_band(app.ui.tool == "fill", r, app.ui.remove_feather);
    let hit = |q: Pos2| spot_under(&bodies, active, q);
    let hover = resp.hover_pos().and_then(hit);
    // the mouse wheel resizes the spot under the pointer instead of the brush (`brush_wheel`, next frame)
    ui.data_mut(|m| m.insert_temp(egui::Id::unique(SPOT_BODIES), SpotBodies { bodies: bodies.clone(), active, long }));
    if let Some(h) = resp.hover_pos() {
        if hover.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        } else {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            brush_circles(&p, h, spot_band(band));
        }
    }
    let press = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
    if resp.drag_started()
        && let Some((spot, source, ..)) = press.and_then(hit)
    {
        if Some(spot) != active {
            let _ = app.run("spot.select", json!({"index": spot}));
        }
        let _ = app.run("develop.beginInteraction", json!({"label": "Edit Spot"}));
        app.gesture = Some(Gesture::SpotMove { spot, source });
    }
    if let Some(Gesture::SpotMove { spot, source }) = app.gesture.clone() {
        if resp.dragged()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let (n, n0) = (map.norm(q), map.norm(q - resp.drag_delta()));
            let dn = [n.x - n0.x, n.y - n0.y];
            if dn != [0.0, 0.0] {
                let _ = app.run("spot.update", json!({"index": spot, if source { "moveSource" } else { "move" }: dn}));
            }
        }
        if resp.drag_stopped() {
            app.gesture = None;
            let _ = app.run("develop.endInteraction", json!({}));
        }
        return;
    }
    if resp.clicked()
        && let Some((spot, ..)) = press.and_then(hit)
    {
        let _ = app.run("spot.select", json!({"index": spot}));
        return;
    }
    if let Some(q) = resp.interact_pointer_pos()
        // the release ends the stroke where it is, but only the left button's: a middle drag pans
        && (resp.drag_started() || resp.dragged() || resp.clicked() || resp.drag_stopped_by(egui::PointerButton::Primary))
    {
        let n = map.norm(q);
        match &mut app.gesture {
            // a point every r/4 of movement; a held pointer stores nothing, and the release adds where it ended
            Some(Gesture::Spot { points }) => {
                let last = points.last().map(|l| map.screen(*l));
                if last.is_none_or(|l| spot_outline::adds_point(l, q, r, resp.drag_stopped_by(egui::PointerButton::Primary))) {
                    points.push(n);
                }
            }
            _ => app.gesture = Some(Gesture::Spot { points: vec![n] }),
        }
    }
    if let Some(Gesture::Spot { points }) = &app.gesture {
        let screen: Vec<Pos2> = points.iter().map(|q| map.screen(*q)).collect();
        draw_spot(&p, &screen, band, Stroke::new(1.25, Color32::from_white_alpha(230)), false, true);
    }
    if (resp.drag_stopped() || resp.clicked())
        && let Some(Gesture::Spot { points }) = app.gesture.take()
    {
        let mode = match app.ui.tool.as_str() {
            "heal" => "heal",
            "clone" => "clone",
            "fill" => "fill",
            _ => "remove",
        };
        let pts: Vec<[f64; 2]> = points.iter().map(|q| [q.x, q.y]).collect();
        let _ = app.run(
            "spot.add",
            json!({"mode": mode, "points": pts, "size": app.ui.remove_size, "feather": app.ui.remove_feather, "opacity": app.ui.remove_opacity}),
        );
    }
}

/// Whether the discs of radius `r` at `pts` can show in `clip`.
fn touches(pts: &[Pos2], r: f32, clip: Rect) -> bool {
    pts.iter().any(|q| clip.expand(r).contains(*q))
}

/// Where a spot of radius `r` starts and stops blending into the photo, as `pipeline::spots` does it: Heal and
/// Clone feather inside the radius (over its outer 80 % at full feather), Fill replaces all of it and feathers
/// outside, up to 0.3 radii further.
pub(super) fn feather_band(outside: bool, r: f32, feather: f32) -> (f32, f32) {
    let f = (feather / 100.0).clamp(0.0, 1.0);
    if outside { (r, r * (1.0 + 0.3 * f)) } else { (r * (1.0 - 0.8 * f), r) }
}

/// How far outside the blend a spot's outline runs (screen points), so the line never covers the edge being matched.
const OUTLINE_GAP: f32 = 3.0;

/// A spot over `pts` blending across `band`: its outline just outside the blend and, with `feather`, a faint
/// dashed line where the blend is full, so the feather between the two shows.
fn draw_spot(p: &egui::Painter, pts: &[Pos2], band: (f32, f32), light: Stroke, dashed: bool, feather: bool) {
    draw_outline(p, &spot_outline::union_outline(pts, band.1 + OUTLINE_GAP), light, dashed);
    if feather && band.1 - band.0 >= 3.0 && band.0 >= 2.0 {
        for l in spot_outline::union_outline(pts, band.0) {
            p.extend(egui::Shape::dashed_line(&l, Stroke::new(1.0, Color32::from_white_alpha(110)), 3.0, 3.0));
        }
    }
}

/// A Remove brush's `band` as its cursor shows it: the outer circle where a spot's outline will run.
pub(super) fn spot_band(band: (f32, f32)) -> (f32, f32) {
    (band.0, band.1 + OUTLINE_GAP)
}

/// The brush cursor: a solid circle at the outer edge of `band`, a faint one at its inner edge (where the blend is full).
pub(super) fn brush_circles(p: &egui::Painter, at: Pos2, band: (f32, f32)) {
    for (rr, col) in [(band.1, Color32::WHITE), (band.0.max(1.0), Color32::from_white_alpha(140))] {
        p.circle_stroke(at, rr, Stroke::new(2.0, Color32::from_black_alpha(90)));
        p.circle_stroke(at, rr, Stroke::new(1.0, col));
    }
}

/// A spot's outline in `light`, over a dark under-stroke so a thin line stays visible on light and dark areas alike.
fn draw_outline(p: &egui::Painter, lines: &[Vec<Pos2>], light: Stroke, dashed: bool) {
    let dark = Stroke::new(light.width + 1.0, Color32::from_black_alpha(90));
    for stroke in [dark, light] {
        for l in lines {
            if dashed {
                p.extend(egui::Shape::dashed_line(l, stroke, 6.0, 4.0));
            } else {
                p.add(egui::Shape::line(l.clone(), stroke));
            }
        }
    }
}

/// Keyboard stand-ins for the Remove tool: each spot's target and source (an arrow press moves
/// it a step, one undo step; Return selects the spot), and a placement point that the arrows move
/// and Return turns into a new spot of the brush's size there. True when a spot was added.
fn spot_keys(app: &mut LightcraftApp, ui: &mut egui::Ui, map: &CanvasMap, d: &DevelopSettings, grips: &[(usize, bool, Pos2, f32)]) -> bool {
    for &(spot, source, q, _) in grips {
        let at = map.norm(q);
        let name =
            format!("{} {}{}", crate::i18n::tr("Spot"), spot + 1, if source { format!(" {}", crate::i18n::tr("source")) } else { String::new() });
        let size = d.spots.get(spot).map(|s| s.size).unwrap_or_default();
        let value = format!("{}, {} {:.0}%", crate::canvas_keys::position_text(at.x, at.y), crate::i18n::tr("size"), size * 100.0);
        let keys = crate::canvas_keys::handle(ui, &format!("spot:{spot}:{}", if source { "source" } else { "target" }), &name, value, q);
        if (keys.activated || keys.delta.is_some()) && app.session.active_spot != Some(spot) {
            let _ = app.run("spot.select", json!({"index": spot}));
        }
        if let Some((dx, dy)) = keys.delta {
            let _ = app.run("develop.beginInteraction", json!({"label": "Edit Spot"}));
            let _ = app.run("spot.update", json!({"index": spot, if source { "moveSource" } else { "move" }: [dx, dy]}));
            let _ = app.run("develop.endInteraction", json!({}));
        }
    }
    // where a new spot goes: the photo's centre at first, then wherever the arrows took it
    let key = egui::Id::unique("lc-spot-placement");
    let place: (f64, f64) = ui.data(|m| m.get_temp(key)).unwrap_or((0.5, 0.5));
    let q = map.screen(Point::new(place.0, place.1));
    let keys = crate::canvas_keys::handle(
        ui,
        "spot:new",
        crate::i18n::tr("New spot (Return adds it here)"),
        crate::canvas_keys::position_text(place.0, place.1),
        q,
    );
    if let Some((dx, dy)) = keys.delta {
        let moved = ((place.0 + dx).clamp(0.0, 1.0), (place.1 + dy).clamp(0.0, 1.0));
        ui.data_mut(|m| m.insert_temp(key, moved));
    }
    if crate::canvas_keys::has_focus(ui.ctx()) && ui.memory(|m| m.has_focus(crate::canvas_keys::id("spot:new"))) {
        let r = (app.ui.remove_size as f64 * (map.rect.width().max(map.rect.height())) as f64) as f32;
        ui.painter().circle_stroke(q, r, Stroke::new(1.0, Color32::WHITE));
    }
    if keys.activated {
        let mode = match app.ui.tool.as_str() {
            "heal" => "heal",
            "clone" => "clone",
            "fill" => "fill",
            _ => "remove",
        };
        let _ = app.run(
            "spot.add",
            json!({"mode": mode, "points": [[place.0, place.1]], "size": app.ui.remove_size, "feather": app.ui.remove_feather, "opacity": app.ui.remove_opacity}),
        );
        return true;
    }
    false
}

// ------------------------------------------------------------------------ red eye

/// Red Eye tool: drag an ellipse over an eye (a new correction), click one to select it.
pub(super) fn eye_overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
    // screen points per long-edge unit
    let o = map.screen(Point::new(0.0, 0.0));
    let (sx, sy) = (map.screen(Point::new(1.0, 0.0)).distance(o), map.screen(Point::new(0.0, 1.0)).distance(o));
    let per_long = sx.max(sy);
    let p = ui.painter();
    for (i, e) in d.red_eye.iter().enumerate() {
        let c = map.screen(e.center);
        let r = vec2(e.rx as f32 * per_long, e.ry as f32 * per_long);
        let sel = i == app.ui.eye;
        let col = if sel { Color32::WHITE } else { Color32::from_white_alpha(150) };
        p.add(egui::Shape::ellipse_stroke(c, r, Stroke::new(if sel { 1.5 } else { 1.0 }, col)));
        p.line_segment([c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)], Stroke::new(1.0, col));
        p.line_segment([c - vec2(0.0, 4.0), c + vec2(0.0, 4.0)], Stroke::new(1.0, col));
    }
    if resp.hover_pos().is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if resp.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        // from where the button went down (a drag is recognized only after some movement)
        app.gesture = Some(Gesture::Eye { a: map.norm(q) });
    }
    if let Some(Gesture::Eye { a }) = app.gesture
        && let Some(q) = resp.interact_pointer_pos()
    {
        let (a, b) = (map.screen(a), q);
        p.add(egui::Shape::ellipse_stroke(a.lerp(b, 0.5), ((b - a) / 2.0).abs(), Stroke::new(1.0, Color32::WHITE)));
    }
    if resp.drag_stopped()
        && let Some(Gesture::Eye { a }) = app.gesture.take()
        && let Some(q) = resp.interact_pointer_pos()
    {
        let (sa, sb) = (map.screen(a), q);
        let half = ((sb - sa) / 2.0).abs() / per_long;
        if half.x > 0.002 && half.y > 0.002 {
            let c = map.norm(sa.lerp(sb, 0.5));
            let r = app.run("redeye.add", json!({"center": [c.x, c.y], "rx": half.x, "ry": half.y, "pet": app.ui.eye_pet}));
            if let Ok(v) = r {
                app.ui.eye = v["index"].as_u64().unwrap_or(0) as usize;
            }
        }
    } else if resp.clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && let Some(i) = d.red_eye.iter().position(|e| {
            let c = map.screen(e.center);
            let (dx, dy) = ((q.x - c.x) / (e.rx as f32 * per_long).max(4.0), (q.y - c.y) / (e.ry as f32 * per_long).max(4.0));
            dx * dx + dy * dy <= 1.0
        })
    {
        app.ui.eye = i;
    }
}
