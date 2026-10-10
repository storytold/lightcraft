//! The map canvas: tiles, tracks, saved-location circles, pins and clusters, attribution; pan,
//! zoom, pin hover / click / drag, and drops of photos dragged from the filmstrip or grid.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use dac_geo::LatLon;
use dac_geo::mercator::{TILE, project, unproject, visible_tiles, world_px};

use super::MapUi;
use crate::DacApp;
use crate::module::{Edge, edge_visible};
use crate::theme::Tokens;
use crate::widgets::register;

/// Width of the map's side panel.
pub const SIDE_W: f32 = 280.0;
/// Height of the filter / search bar.
const BAR_H: f32 = 34.0;

/// Screen position of a place (the copy of the world nearest the centre).
pub fn geo_to_screen(m: &MapUi, r: Rect, p: LatLon) -> Pos2 {
    let world = world_px(m.prefs.zoom);
    let (cx, cy) = project(m.centre());
    let (x, y) = project(p);
    let mut dx = x - cx;
    dx -= dx.round();
    pos2(r.center().x + (dx * world) as f32, r.center().y + ((y - cy) * world) as f32)
}

pub fn screen_to_geo(m: &MapUi, r: Rect, s: Pos2) -> LatLon {
    let world = world_px(m.prefs.zoom);
    let (cx, cy) = project(m.centre());
    unproject(cx + f64::from(s.x - r.center().x) / world, cy + f64::from(s.y - r.center().y) / world)
}

/// The Map module's centre: filter bar, map canvas, side panel; the filmstrip below.
pub fn center(ui: &mut egui::Ui, app: &mut DacApp) {
    app.map.start(app.headless_host);
    let t = Tokens::get(ui.ctx());
    let mut area = ui.available_rect_before_wrap();
    if edge_visible(app, Edge::Bottom) {
        let film = Rect::from_min_max(pos2(area.left(), area.bottom() - t.film_h), area.max);
        area.max.y = film.top();
        crate::panels::detail::filmstrip(app, ui, film);
    }
    app.canvas_rect = Some(area);
    register(ui.ctx(), "view:module:map", area);
    super::side::poll_jobs(app, ui.ctx());
    let side = Rect::from_min_max(pos2((area.right() - SIDE_W).max(area.left()), area.top()), area.max);
    let bar = Rect::from_min_max(area.min, pos2(side.left(), area.top() + BAR_H));
    let map = Rect::from_min_max(pos2(area.left(), bar.bottom()), pos2(side.left(), area.bottom()));
    super::side::bar(app, ui, bar);
    super::side::panel(app, ui, side);
    if map.width() > 10.0 && map.height() > 10.0 {
        canvas(app, ui, map);
    }
}

fn canvas(app: &mut DacApp, ui: &mut egui::Ui, r: Rect) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    app.map.canvas = Some(r);
    register(&ctx, "map:canvas", r);
    let resp = ui.interact(r, egui::Id::new("map-canvas"), Sense::click_and_drag());
    let painter = ui.painter_at(r);
    painter.rect_filled(r, 0.0, Color32::from_rgb(0xaa, 0xd3, 0xdf));

    // ---- input: pan, zoom (pointer-anchored), double-click zoom
    let hover = resp.hover_pos();
    if resp.dragged() && app.map.dragging_pin.is_none() {
        let d = resp.drag_delta();
        let world = world_px(app.map.prefs.zoom);
        let (cx, cy) = project(app.map.centre());
        let c = unproject(cx - f64::from(d.x) / world, cy - f64::from(d.y) / world);
        let z = app.map.prefs.zoom;
        app.map.set_view(c, z);
    }
    if resp.drag_stopped() {
        app.map.save();
    }
    let scroll = if hover.is_some() { ui.input(|i| i.smooth_scroll_delta.y + i.zoom_delta().log2() * 200.0) } else { 0.0 };
    if let Some(at) = hover
        && scroll.abs() > 0.0
    {
        zoom_at(app, r, at, f64::from(scroll) / 200.0);
    }
    if resp.double_clicked()
        && let Some(at) = hover
    {
        zoom_at(app, r, at, 1.0);
        app.map.save();
    }

    // ---- tiles: integer zoom level, scaled by the fraction
    let server = app.map.server();
    let zf = app.map.prefs.zoom;
    let z = zf.floor().clamp(0.0, f64::from(server.max_zoom)) as u8;
    let scale = 2f64.powf(zf - f64::from(z));
    let (w, h) = (f64::from(r.width()) / scale, f64::from(r.height()) / scale);
    let tiles = visible_tiles(app.map.centre(), z, w, h);
    let sync = app.headless_host;
    app.map.tiles.poll(&ctx);
    let size = (TILE * scale) as f32;
    for (k, ox, oy) in &tiles {
        let min = r.min + vec2((*ox * scale) as f32, (*oy * scale) as f32);
        let tr = Rect::from_min_size(min, vec2(size, size));
        app.map.tiles.want(&ctx, &server, *k, sync);
        if let Some(tex) = app.map.tiles.get(&server.id, *k) {
            painter.image(tex, tr.expand(0.25), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            continue;
        }
        // a coarser tile already loaded stands in while this one loads
        let mut drawn = false;
        let (mut pk, mut uv) = (*k, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
        for _ in 0..4 {
            let Some(parent) = pk.parent() else { break };
            let (qx, qy) = ((pk.x % 2) as f32 * 0.5, (pk.y % 2) as f32 * 0.5);
            uv = Rect::from_min_size(pos2(qx + uv.min.x * 0.5, qy + uv.min.y * 0.5), uv.size() * 0.5);
            pk = parent;
            if let Some(tex) = app.map.tiles.get(&server.id, pk) {
                painter.image(tex, tr.expand(0.25), uv, Color32::WHITE);
                drawn = true;
                break;
            }
        }
        if !drawn {
            painter.rect_filled(tr, 0.0, Color32::from_rgb(0xe8, 0xe4, 0xdc));
            painter.rect_stroke(tr, 0.0, Stroke::new(0.5, Color32::from_rgb(0xd0, 0xcc, 0xc4)), StrokeKind::Inside);
        }
    }
    if app.map.tiles.busy() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    // ---- tracks
    for tr in app.map.tracks.iter().filter(|t| t.shown) {
        for seg in &tr.track.segments {
            let step = (seg.len() / 4000).max(1);
            let pts: Vec<Pos2> = seg.iter().step_by(step).map(|p| geo_to_screen(&app.map, r, *p)).collect();
            if pts.len() >= 2 {
                painter.add(egui::Shape::line(pts.clone(), Stroke::new(4.0, Color32::from_rgba_unmultiplied(255, 255, 255, 170))));
                painter.add(egui::Shape::line(pts, Stroke::new(2.0, Color32::from_rgb(0xe0, 0x40, 0x30))));
            } else if let Some(p) = pts.first() {
                painter.circle_filled(*p, 3.0, Color32::from_rgb(0xe0, 0x40, 0x30));
            }
        }
    }

    // ---- saved locations
    let mpp = dac_geo::mercator::metres_per_px(app.map.prefs.lat, zf);
    for l in app.session.catalog.saved_locations() {
        let c = geo_to_screen(&app.map, r, l.centre());
        let rad = (l.radius / mpp.max(1e-9)).clamp(3.0, 20_000.0) as f32;
        let col = if l.private { Color32::from_rgb(0xd0, 0x40, 0x40) } else { Color32::from_rgb(0x30, 0x80, 0xe0) };
        painter.circle(c, rad, col.gamma_multiply(0.15), Stroke::new(1.5, col));
        painter.text(
            c + vec2(0.0, -rad - 8.0),
            Align2::CENTER_CENTER,
            if l.private { format!("🔒 {}", l.name) } else { l.name.clone() },
            t.semibold(11.0),
            col,
        );
    }

    // ---- pins
    let pins = app.map.pins(&mut app.session);
    let selected: std::collections::HashSet<u64> = app.session.selection.ids.iter().map(|p| p.0).collect();
    app.map.drawn.clear();
    let mut hovered: Option<(Pos2, Vec<u64>)> = None;
    for c in pins.iter() {
        let at = geo_to_screen(&app.map, r, c.centre);
        if !r.expand(30.0).contains(at) {
            continue;
        }
        let n = c.items.len();
        let sel = c.items.iter().any(|i| selected.contains(i));
        let id = egui::Id::new(("map-pin", c.items.first().copied().unwrap_or(0)));
        let rad = if n == 1 { 7.0 } else { 11.0 + (n as f32).log10() * 4.0 };
        let hit = Rect::from_center_size(at, vec2(rad * 2.0 + 6.0, rad * 2.0 + 6.0));
        let pr = ui.interact(hit, id, Sense::click_and_drag());
        register(&ctx, format!("map:pin:{}", c.items.first().copied().unwrap_or(0)), hit);
        let fill = if sel { t.accent } else { Color32::from_rgb(0x20, 0x20, 0x24) };
        painter.circle(at, rad, fill, Stroke::new(2.0, Color32::WHITE));
        if n > 1 {
            painter.text(at, Align2::CENTER_CENTER, n.to_string(), t.semibold(if n > 99 { 9.5 } else { 11.0 }), Color32::WHITE);
        }
        app.map.drawn.push((at, c.items.clone()));
        if pr.hovered() {
            hovered = Some((at, c.items.clone()));
        }
        if pr.clicked() {
            let mode = if ui.input(|i| i.modifiers.command) { "add" } else { "replace" };
            let _ = app.run("library.select", json!({"ids": c.items, "mode": mode}));
        }
        if pr.double_clicked() {
            zoom_at(app, r, at, 2.0);
        }
        if pr.drag_started() {
            app.map.dragging_pin = Some(c.items.clone());
        }
    }
    // a pin being moved: follow the pointer; drop = new position for its photos
    if let Some(ids) = app.map.dragging_pin.clone() {
        let (pos, released) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.any_released()));
        if let Some(p) = pos {
            painter.circle(p, 8.0, t.accent.gamma_multiply(0.7), Stroke::new(2.0, Color32::WHITE));
            if released {
                if r.contains(p) {
                    let at = screen_to_geo(&app.map, r, p);
                    match app.run("map.geotag", json!({"ids": ids, "lat": at.lat, "lon": at.lon})) {
                        Ok(_) => app.toast(&ctx, crate::i18n::tr("Moved the photos to the new location")),
                        Err(e) => app.toast_error(&ctx, e),
                    }
                }
                app.map.dragging_pin = None;
            }
        } else if released {
            app.map.dragging_pin = None;
        }
    }
    // photos dragged from the filmstrip or the grid: drop to geotag
    if let Some(ids) = app.ui.dragging_photos.clone() {
        let (pos, released) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.any_released()));
        if let Some(p) = pos.filter(|p| r.contains(*p)) {
            painter.rect_stroke(r.shrink(2.0), 0.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
            painter.circle_stroke(p, 10.0, Stroke::new(2.0, t.accent));
            if released {
                let at = screen_to_geo(&app.map, r, p);
                let n = ids.len();
                match app.run("map.geotag", json!({"ids": ids, "lat": at.lat, "lon": at.lon})) {
                    Ok(_) => app.toast(&ctx, format!("Geotagged {n} photo{}", if n == 1 { "" } else { "s" }, n = n)),
                    Err(e) => app.toast_error(&ctx, e),
                }
                app.ui.dragging_photos = None;
            }
        }
    }
    // hover preview: the first photo's thumbnail and the count
    if let Some((at, ids)) = hovered
        && app.map.dragging_pin.is_none()
    {
        hover_preview(app, ui, r, at, &ids);
    }

    // ---- context menu
    let click_geo = resp.interact_pointer_pos().or(hover).map(|p| screen_to_geo(&app.map, r, p));
    resp.context_menu(|ui| {
        let Some(at) = click_geo else { return };
        ui.label(egui::RichText::new(format!("{:.5}, {:.5}", at.lat, at.lon)).weak());
        let has_sel = !app.session.selection.ids.is_empty();
        if ui.add_enabled(has_sel, egui::Button::new(crate::i18n::tr("Geotag Selected Photos Here"))).clicked() {
            if let Err(e) = app.run("map.geotag", json!({"lat": at.lat, "lon": at.lon})) {
                app.toast_error(ui.ctx(), e);
            }
            ui.close();
        }
        if ui.button(crate::i18n::tr("Save Location Here")).clicked() {
            let name = if app.map.new_name.trim().is_empty() {
                format!("Location {}", app.session.catalog.saved_locations().count() + 1)
            } else {
                app.map.new_name.clone()
            };
            let r = app.run(
                "map.saveLocation",
                json!({"name": name, "lat": at.lat, "lon": at.lon, "radius": app.map.new_radius, "private": app.map.new_private}),
            );
            if let Err(e) = r {
                app.toast_error(ui.ctx(), e);
            }
            ui.close();
        }
        if ui.button(crate::i18n::tr("Copy Coordinates")).clicked() {
            ui.ctx().copy_text(format!("{:.6}, {:.6}", at.lat, at.lon));
            ui.close();
        }
    });

    // ---- attribution (always on screen) and zoom buttons
    let attribution = format!("{}{}", server.attribution, app.map.tiles.last_error.as_ref().map(|e| format!("  ·  {e}")).unwrap_or_default());
    let g = painter.layout_no_wrap(attribution, t.font(10.0), Color32::from_gray(40));
    let ar = Rect::from_min_size(pos2(r.right() - g.size().x - 10.0, r.bottom() - g.size().y - 6.0), g.size() + vec2(8.0, 4.0));
    painter.rect_filled(ar, CornerRadius::same(3), Color32::from_rgba_unmultiplied(255, 255, 255, 200));
    painter.galley(ar.min + vec2(4.0, 2.0), g, Color32::from_gray(40));
    register(&ctx, "map:attribution", ar);
    for (i, (label, dz)) in [("+", 1.0), ("−", -1.0)].into_iter().enumerate() {
        let br = Rect::from_min_size(pos2(r.left() + 10.0, r.top() + 10.0 + i as f32 * 30.0), vec2(26.0, 26.0));
        let b = ui.interact(br, egui::Id::new(("map-zoom", i)), Sense::click());
        register(&ctx, format!("map:zoom{}", if dz > 0.0 { "In" } else { "Out" }), br);
        painter.rect(
            br,
            CornerRadius::same(4),
            if b.hovered() { Color32::from_gray(235) } else { Color32::WHITE },
            Stroke::new(1.0, Color32::from_gray(160)),
            StrokeKind::Inside,
        );
        painter.text(br.center(), Align2::CENTER_CENTER, label, t.semibold(16.0), Color32::from_gray(40));
        if b.clicked() {
            let c = app.map.centre();
            app.map.set_view(c, app.map.prefs.zoom.round() + dz);
            app.map.save();
        }
    }
    // scale bar
    let metres = mpp * 100.0;
    let nice = [1.0, 2.0, 5.0].iter().flat_map(|m| (0..8).map(move |e| m * 10f64.powi(e))).filter(|v| *v <= metres).fold(1.0, f64::max);
    let px = (nice / mpp.max(1e-9)) as f32;
    let y = r.bottom() - 12.0;
    painter.line_segment([pos2(r.left() + 10.0, y), pos2(r.left() + 10.0 + px, y)], Stroke::new(2.0, Color32::from_gray(40)));
    let label = if nice >= 1000.0 { format!("{} km", nice / 1000.0) } else { format!("{nice} m") };
    painter.text(pos2(r.left() + 12.0, y - 3.0), Align2::LEFT_BOTTOM, label, t.font(10.0), Color32::from_gray(40));
}

fn zoom_at(app: &mut DacApp, r: Rect, at: Pos2, dz: f64) {
    let before = screen_to_geo(&app.map, r, at);
    let c = app.map.centre();
    let z = app.map.prefs.zoom + dz;
    app.map.set_view(c, z);
    // keep the place under the pointer where it was
    let after = screen_to_geo(&app.map, r, at);
    let (bx, by) = project(before);
    let (ax, ay) = project(after);
    let (cx, cy) = project(app.map.centre());
    let zz = app.map.prefs.zoom;
    app.map.set_view(unproject(cx + (bx - ax), cy + (by - ay)), zz);
}

fn hover_preview(app: &mut DacApp, ui: &mut egui::Ui, r: Rect, at: Pos2, ids: &[u64]) {
    let t = Tokens::get(ui.ctx());
    let Some(first) = ids.first().map(|i| dac_catalog::PhotoId(*i)) else { return };
    crate::panels::grid::request_thumb(app, first, 256, 4);
    let size = vec2(150.0, 120.0);
    let mut pos = at + vec2(14.0, -size.y - 10.0);
    pos.x = pos.x.min(r.right() - size.x - 4.0);
    pos.y = pos.y.max(r.top() + 4.0);
    let box_r = Rect::from_min_size(pos, size);
    let p = ui.painter_at(r);
    p.rect(box_r, CornerRadius::same(4), t.chrome, Stroke::new(1.0, t.divider), StrokeKind::Inside);
    let img = Rect::from_min_max(box_r.min + vec2(6.0, 6.0), box_r.max - vec2(6.0, 22.0));
    if let Some(tex) = app.renderer.thumb(first) {
        let [tw, th] = tex.size;
        let s = (img.width() / tw.max(1) as f32).min(img.height() / th.max(1) as f32);
        let fr = Rect::from_center_size(img.center(), vec2(tw as f32 * s, th as f32 * s));
        p.image(tex.tex.id(), fr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    let name = app.session.catalog.photo(first).map(|ph| ph.file_name.clone()).unwrap_or_default();
    let text = if ids.len() > 1 { format!("{n} photos", n = ids.len()) } else { name };
    p.text(pos2(box_r.center().x, box_r.bottom() - 11.0), Align2::CENTER_CENTER, text, t.font(11.0), t.text);
    register(ui.ctx(), "map:hover", box_r);
}
