//! Saved locations on the Map view (LRC-MAP-LOCATIONS): every location in view is a circle with its
//! name; the active one (picked in the list) has a centre and a radius handle that move it (unless
//! Lock Markers is on), saved when let go (`map.location.update`). The list in the navigator panel
//! shows each location with the photos in view inside it, a checkbox selecting them, a private
//! mark, and a right-click menu (Go To, Select Photos, Rename…, Private, Delete… with a
//! confirmation). `+` makes one at the map's centre, or around the selected photos with a position.
//! Distances are along the Earth's surface (`lightcraft_catalog::rules::GpsCircle`), so circles
//! across the antimeridian and around a pole work. Ported in spirit from PR #906 (Jason Baldwin).

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, pos2, vec2};
use lightcraft_catalog::rules::{EARTH_RADIUS_M, distance_m};
use lightcraft_engine::locations::SavedLocation;
use lightcraft_engine::map::Viewport;
use lightcraft_geom::{LatLon, Point};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::MapUi;
use crate::theme::Tokens;

/// How far from a handle a press still takes it (points).
const HANDLE_HIT: f32 = 9.0;
/// The colour of saved locations' circles; private ones are drawn dashed-red.
const CIRCLE: Color32 = Color32::from_rgb(255, 204, 0);
const PRIVATE: Color32 = Color32::from_rgb(230, 80, 80);

/// A handle of the active location being dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    Centre,
    Radius,
}

/// The active location's handle being dragged: which location and handle, and where it is now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocationDrag {
    pub id: u64,
    pub handle: Handle,
    pub at: Pos2,
}

/// A location as drawn this frame (`map.state` → `locations`).
#[derive(Clone, Debug)]
pub struct DrawnLocation {
    pub id: u64,
    pub at: Pos2,
    pub radius_px: f32,
    pub active: bool,
}

/// Screen points per metre at latitude `lat` for view `vp` (0 when it can't be told).
fn px_per_metre(vp: &Viewport, lat: f64) -> f64 {
    let world = lightcraft_engine::map::mercator::world_size(vp.zoom);
    let metres_round = 2.0 * std::f64::consts::PI * EARTH_RADIUS_M * lat.to_radians().cos().abs();
    if metres_round > 1.0 { world / metres_round } else { 0.0 }
}

/// Where `l` is drawn on a canvas at `origin`: centre and radius in points (the radius capped so a
/// huge circle stays drawable).
fn placed(l: &SavedLocation, vp: &Viewport, origin: Pos2) -> Option<(Pos2, f32)> {
    let c = LatLon::new(l.lat, l.lon)?;
    let p = vp.to_screen(c);
    let r = (l.radius * px_per_metre(vp, l.lat)).min(1e6) as f32;
    let at = pos2(origin.x + p.x as f32, origin.y + p.y as f32);
    (at.x.is_finite() && at.y.is_finite() && r.is_finite()).then_some((at, r))
}

/// The location `id` with a handle drag applied (what is drawn while dragging, and saved on release).
fn dragged(l: &SavedLocation, d: &LocationDrag, vp: &Viewport, origin: Pos2) -> SavedLocation {
    let mut l = l.clone();
    let p = vp.to_lat_lon(Point::new(f64::from(d.at.x - origin.x), f64::from(d.at.y - origin.y)));
    match d.handle {
        Handle::Centre => {
            l.lat = p.lat;
            l.lon = p.lon;
        }
        Handle::Radius => {
            let r = distance_m(l.lat, l.lon, p.lat, p.lon);
            if r.is_finite() {
                l.radius = r.clamp(lightcraft_catalog::rules::MIN_RADIUS_M, lightcraft_catalog::rules::MAX_RADIUS_M);
            }
        }
    }
    l
}

/// A press at `at` on the active location's handle (unless markers are locked): the drag it starts.
pub fn grab(app: &LightcraftApp, m: &MapUi, vp: &Viewport, origin: Pos2, at: Pos2) -> Option<LocationDrag> {
    if m.locked {
        return None;
    }
    let id = m.active_location?;
    let l = app.session.map.locations.get(id)?;
    let (c, r) = placed(l, vp, origin)?;
    let radius_handle = pos2(c.x + r, c.y);
    if at.distance(radius_handle) <= HANDLE_HIT {
        Some(LocationDrag { id, handle: Handle::Radius, at: radius_handle })
    } else if at.distance(c) <= HANDLE_HIT {
        Some(LocationDrag { id, handle: Handle::Centre, at: c })
    } else {
        None
    }
}

/// Save a handle drag that ended over the map (`map.location.update`).
pub fn drop_drag(app: &mut LightcraftApp, ctx: &egui::Context, d: &LocationDrag, vp: &Viewport, origin: Pos2) {
    let Some(l) = app.session.map.locations.get(d.id).cloned() else { return };
    let moved = dragged(&l, d, vp, origin);
    let p = match d.handle {
        Handle::Centre => json!({"id": d.id, "lat": moved.lat, "lon": moved.lon}),
        Handle::Radius => json!({"id": d.id, "radius": moved.radius}),
    };
    if let Err(e) = app.run("map.location.update", p) {
        app.toast(ctx, e);
    }
}

/// Draw every saved location in view (the active one with its handles), and record them.
pub fn draw(app: &mut LightcraftApp, painter: &Painter, canvas: Rect, vp: &Viewport, m: &MapUi) {
    let origin = canvas.min;
    let list: Vec<SavedLocation> = app.session.map.locations.list().to_vec();
    for l in &list {
        let shown = match m.location_drag {
            Some(d) if d.id == l.id => dragged(l, &d, vp, origin),
            _ => l.clone(),
        };
        let Some((c, r)) = placed(&shown, vp, origin) else { continue };
        if !canvas.expand(r).contains(c) && !canvas.intersects(Rect::from_center_size(c, vec2(2.0 * r, 2.0 * r))) {
            continue;
        }
        let active = m.active_location == Some(l.id);
        let colour = if l.private { PRIVATE } else { CIRCLE };
        painter.circle_filled(c, r.max(3.0), colour.gamma_multiply(if active { 0.16 } else { 0.08 }));
        painter.circle_stroke(c, r.max(3.0), Stroke::new(if active { 2.0 } else { 1.5 }, colour.gamma_multiply(0.85)));
        painter.text(pos2(c.x, c.y - r.max(3.0) - 4.0), Align2::CENTER_BOTTOM, &l.name, FontId::proportional(11.5), Color32::from_gray(20));
        if active && !m.locked {
            for h in [c, pos2(c.x + r, c.y)] {
                painter.circle(h, 5.0, Color32::WHITE, Stroke::new(1.5, colour));
            }
        }
        app.map_drawn.locations.push(DrawnLocation { id: l.id, at: c, radius_px: r, active });
    }
}

/// Zoom that shows a circle of `radius` metres in a map of ~800 points.
pub fn zoom_for(radius: f64) -> f64 {
    let r = if radius.is_finite() { radius.max(1.0) } else { 1000.0 };
    (40_075_016.0 / (r * 2.0 * 3.0)).log2().clamp(2.0, 17.0)
}

/// The navigator panel's list: each location with the photos in view inside it, a checkbox
/// selecting them, a private mark, a right-click menu; `+` makes a new one.
pub fn list(app: &mut LightcraftApp, ui: &mut egui::Ui, m: &mut MapUi, center: &mut LatLon, zoom: &mut f64) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Saved Locations")).strong().color(Color32::WHITE));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            let b = ui.button("+").on_hover_text(crate::i18n::tr("New saved location: around the selected photos, or at the map's centre"));
            crate::widgets::register(ui, "map:location:new", b.rect);
            if b.clicked() {
                new_location(app, ui.ctx(), m, *center);
            }
        });
    });
    ui.add_space(6.0);
    let selected: std::collections::HashSet<u64> = app.session.selection.ids.iter().map(|p| p.0).collect();
    let rows: Vec<(SavedLocation, Vec<u64>)> = app
        .session
        .map
        .locations
        .list()
        .iter()
        .map(|l| {
            let circle = l.circle();
            let inside = app.map_points.points.iter().filter(|p| circle.contains(p.pos.lat, p.pos.lon)).map(|p| p.id).collect();
            (l.clone(), inside)
        })
        .collect();
    let mut action: Option<(u64, &'static str, Value)> = None;
    egui::ScrollArea::vertical().id_salt("map-locations").show(ui, |ui| {
        if rows.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new(crate::i18n::tr("No saved locations. Click + to save.")).small().italics().color(Color32::GRAY));
            });
        }
        for (l, inside) in &rows {
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                // the checkbox: all its photos in view selected
                let mut all = !inside.is_empty() && inside.iter().all(|id| selected.contains(id));
                let cb = ui.add_enabled(!inside.is_empty(), egui::Checkbox::without_text(&mut all));
                crate::widgets::register(ui, format!("map:location:check:{}", l.id), cb.rect);
                if cb.changed() {
                    action = Some((l.id, if all { "select" } else { "deselect" }, Value::Null));
                }
                if m.renaming.as_ref().is_some_and(|(id, _)| *id == l.id) {
                    if let Some((_, text)) = m.renaming.as_mut() {
                        let r = crate::text_field::TextField::singleline("field:locationName", text).width(120.0).show(ui);
                        if r.committed() {
                            action = Some((l.id, "rename", json!(text.clone())));
                        } else if r.cancelled() || !r.editing && !r.response.has_focus() && r.ending.is_some() {
                            m.renaming = None;
                        }
                        if !r.editing {
                            r.response.request_focus();
                        }
                    }
                } else {
                    let label = if l.private { format!("🔒 {}", l.name) } else { l.name.clone() };
                    let resp = ui.selectable_label(m.active_location == Some(l.id), label);
                    crate::widgets::register(ui, format!("map:location:{}", l.id), resp.rect);
                    if resp.clicked() {
                        action = Some((l.id, "goto", Value::Null));
                    }
                    resp.context_menu(|ui| {
                        for (key, label) in
                            [("goto", "Go To"), ("select", "Select Photos"), ("rename", "Rename…"), ("private", "Private"), ("delete", "Delete…")]
                        {
                            let b = if key == "private" {
                                let mut on = l.private;
                                let r = ui.checkbox(&mut on, crate::i18n::tr(label));
                                if r.clicked() {
                                    action = Some((l.id, "private", json!(on)));
                                    ui.close();
                                }
                                r
                            } else {
                                let r = ui.button(crate::i18n::tr(label));
                                if r.clicked() {
                                    action = Some((l.id, key, Value::Null));
                                    ui.close();
                                }
                                r
                            };
                            crate::widgets::register(ui, format!("map:location:menu:{key}"), b.rect);
                        }
                    });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(inside.len().to_string()).small().color(t.text_dim));
                });
            });
            // Delete… asks first
            if m.confirm_delete == Some(l.id) {
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Delete “{name}”?", name = l.name)).small().color(Color32::WHITE));
                    let d = ui.small_button(crate::i18n::tr("Delete"));
                    crate::widgets::register(ui, "map:location:confirmDelete", d.rect);
                    if d.clicked() {
                        action = Some((l.id, "remove", Value::Null));
                    }
                    if ui.small_button(crate::i18n::tr("Cancel")).clicked() {
                        m.confirm_delete = None;
                    }
                });
            }
        }
    });
    if let Some((id, what, v)) = action {
        act(app, ui.ctx(), m, center, zoom, id, what, v);
    }
}

/// `+`: a location around the selected photos that have a position, else at the map's centre.
fn new_location(app: &mut LightcraftApp, ctx: &egui::Context, m: &mut MapUi, center: LatLon) {
    let n = app.session.map.locations.len() + 1;
    let name = crate::i18n::tr_format!("Location {n}", n = n);
    let any_placed = app
        .session
        .selection
        .ids
        .iter()
        .any(|id| app.session.catalog.photo(*id).is_some_and(|p| lightcraft_engine::map::from_gps(p.meta.gps).is_some()));
    let p = if any_placed {
        json!({"name": name, "fromSelection": true})
    } else {
        json!({"name": name, "lat": center.lat, "lon": center.lon, "radius": 1000.0})
    };
    match app.run("map.location.add", p) {
        Ok(v) => {
            m.active_location = v["id"].as_u64();
            m.renaming = m.active_location.map(|id| (id, name));
        }
        Err(e) => app.toast(ctx, e),
    }
}

/// What a list row, its menu or the confirmation asked for.
#[allow(clippy::too_many_arguments)]
fn act(app: &mut LightcraftApp, ctx: &egui::Context, m: &mut MapUi, center: &mut LatLon, zoom: &mut f64, id: u64, what: &str, v: Value) {
    let r = match what {
        "goto" => {
            if let Some(l) = app.session.map.locations.get(id)
                && let Some(c) = LatLon::new(l.lat, l.lon)
            {
                *center = c;
                *zoom = zoom_for(l.radius);
                m.fitted = true;
            }
            m.active_location = Some(id);
            Ok(Value::Null)
        }
        "select" | "deselect" => {
            let inside: Vec<u64> = match app.session.execute("map.location.photos", &json!({"id": id, "inView": true})) {
                Ok(r) => r["ids"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default(),
                Err(e) => {
                    app.toast(ctx, e.to_string());
                    return;
                }
            };
            if what == "select" {
                app.run("library.select", json!({"ids": inside, "mode": "add"}))
            } else {
                // the selection without the location's photos (the active one kept when it stays)
                let rest: Vec<u64> = app.session.selection.ids.iter().map(|p| p.0).filter(|p| !inside.contains(p)).collect();
                let active = app.session.selection.active.map(|a| a.0).filter(|a| rest.contains(a));
                let mut q = json!({"ids": rest});
                if let Some(a) = active {
                    q["active"] = json!(a);
                }
                app.run("library.select", q)
            }
        }
        "rename" if v.is_null() => {
            m.renaming = app.session.map.locations.get(id).map(|l| (id, l.name.clone()));
            Ok(Value::Null)
        }
        "rename" => {
            m.renaming = None;
            app.run("map.location.update", json!({"id": id, "name": v}))
        }
        "private" => app.run("map.location.update", json!({"id": id, "private": v})),
        "delete" => {
            m.confirm_delete = Some(id);
            Ok(Value::Null)
        }
        "remove" => {
            m.confirm_delete = None;
            if m.active_location == Some(id) {
                m.active_location = None;
            }
            app.run("map.location.remove", json!({"id": id}))
        }
        _ => Ok(Value::Null),
    };
    if let Err(e) = r {
        app.toast(ctx, e);
    }
}

/// `map.state` → `locations`: each location drawn last frame `{id, x, y, radiusPx, active}`.
pub fn state(app: &LightcraftApp) -> Value {
    json!(
        app.map_drawn
            .locations
            .iter()
            .map(|l| json!({"id": l.id, "x": l.at.x, "y": l.at.y, "radiusPx": l.radius_px, "active": l.active}))
            .collect::<Vec<_>>()
    )
}
