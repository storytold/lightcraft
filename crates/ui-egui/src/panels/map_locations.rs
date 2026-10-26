//! Saved locations on the Map view (LRC-MAP-LOCATIONS, as Lightroom Classic's Saved Locations):
//! each library location ([`lightcraft_engine::locations`], commands `map.location.*`) is drawn
//! on the map as a circle with its name ([`draw`]); the selected one has a handle at its centre
//! and one on its edge, which drag the location and its radius ([`drag_start`], [`drag_end`]:
//! one `map.location.update` on release; Lock Markers stops them as it stops pins). The list
//! sits over the map's top-left corner ([`overlay`]; it folds to its header): each location with
//! the number of photos in view inside it, a checkbox that selects those photos, a click that
//! shows it on the map, a menu to rename it, make it private or delete it, and + for a new one (at
//! the map's centre or around the selected photos). It lives on the map rather than in a docked
//! panel because the docked panels are shared by every view (and the Library panel is closed by
//! default): on the canvas, which can't be closed, it is there whenever the map is.
//!
//! Circles are measured on the Earth (`GpsCircle`), not on the screen: drawn from points around
//! the centre, a circle across the antimeridian stays whole and one around a pole fills the band
//! up to the map's edge. Locations far off screen or smaller than a few points cost one
//! projection each.

use std::collections::HashMap;

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_catalog::rules::{EARTH_RADIUS_M, GpsCircle, MAX_RADIUS_M, MIN_RADIUS_M, destination, distance_m};
use lightcraft_engine::locations::{PositionIndex, SavedLocation};
use lightcraft_geom::Point;
use lightcraft_map::mercator::{to_world, world_size};
use lightcraft_map::{LatLon, Viewport};
use serde_json::{Value, json};

use super::{LightcraftApp, MAX_LISTED, viewport};
use crate::theme::Tokens;
use crate::widgets::register;

/// Points around a circle's edge when it is drawn.
const RING: usize = 72;
/// Grab distance of a handle (points).
const HANDLE_GRAB: f32 = 10.0;
const LOCATION: Color32 = Color32::from_rgb(0x3d, 0xa5, 0xd9);
const PRIVATE: Color32 = Color32::from_rgb(0xc8, 0x5c, 0xd8);

/// A saved location's centre or radius being dragged: what it will be on release.
#[derive(Clone, Copy, Debug)]
struct LocDrag {
    id: u64,
    radius: bool,
    /// The handle's centre minus the pointer at the start (so it doesn't jump).
    grab: egui::Vec2,
    lat: f64,
    lon: f64,
    meters: f64,
}

/// A location as drawn last (screen points), for clicks, drags and `map.state`.
#[derive(Clone, Copy, Debug)]
struct Shown {
    id: u64,
    center: Pos2,
    /// The radius handle (on the edge, east of the centre).
    handle: Pos2,
    r_px: f32,
    label: Option<Rect>,
}

/// The Map view's saved-location state (for the session).
#[derive(Default)]
pub struct LocState {
    /// The location shown with its handles.
    pub selected: Option<u64>,
    drag: Option<LocDrag>,
    /// Photos in view inside each location, and what they were counted for (photo list, locations).
    counts: HashMap<u64, usize>,
    counts_for: Option<(u64, u64)>,
    /// The photos in view with a position, for the counts and the checkboxes.
    index: PositionIndex,
    /// The list over the map (screen points; `None` when it isn't drawn), and whether it is folded.
    pub overlay: Option<Rect>,
    pub list_folded: bool,
    shown: Vec<Shown>,
    /// The Info panel shows the map of this photo although it is in a private location (asked for).
    pub reveal_private: Option<PhotoId>,
    /// The Info panel hid its map for (photo, location) in pass …
    pub private_mini: Option<(PhotoId, u64, u64)>,
}

/// Photos in view inside each location (counted once per photo list and change of locations).
pub fn counts(app: &mut LightcraftApp) -> &HashMap<u64, usize> {
    super::refresh_points(app);
    let key = (app.map.points.generation.unwrap_or(0), app.session.map.locations.revision());
    if app.map.loc.counts_for != Some(key) {
        let idx = PositionIndex::new(app.map.points.points.iter().map(|p| (p.id, Some((p.pos.lat, p.pos.lon)))));
        app.map.loc.counts = app.session.map.locations.list().iter().map(|l| (l.id, idx.count(&l.circle()))).collect();
        app.map.loc.index = idx;
        app.map.loc.counts_for = Some(key);
    }
    &app.map.loc.counts
}

/// The photos in view inside location `id`, in view order.
fn photos_in_view(app: &mut LightcraftApp, id: u64) -> Vec<u64> {
    let Some(c) = app.session.map.locations.get(id).map(SavedLocation::circle) else { return vec![] };
    let (_, ids) = app.session.visible_base_shared();
    ids.iter()
        .filter(|i| {
            app.session.catalog.photo(**i).and_then(|p| lightcraft_catalog::rules::valid_gps(p.meta.gps)).is_some_and(|(a, b)| c.contains(a, b))
        })
        .map(|i| i.0)
        .collect()
}

/// The location as it is drawn now: its saved place, or where a drag has it.
fn current(app: &LightcraftApp, l: &SavedLocation) -> (f64, f64, f64) {
    match app.map.loc.drag {
        Some(d) if d.id == l.id => (d.lat, d.lon, d.meters),
        _ => (l.lat, l.lon, l.radius),
    }
}

/// Screen point of a position given as its offset from `centre` (lon unwrapped), so a ring
/// around a centre stays continuous across ±180°.
fn project_from(vp: &Viewport, centre: (f64, f64), at: Pos2, p: (f64, f64)) -> Pos2 {
    let s = world_size(vp.zoom);
    let cy = to_world(LatLon { lat: centre.0, lon: 0.0 }, vp.zoom).y;
    let py = to_world(LatLon { lat: p.0, lon: 0.0 }, vp.zoom).y;
    let dx = (p.1 - centre.1) / 360.0 * s;
    pos2(at.x + dx as f32, at.y + (py - cy) as f32)
}

/// Metres per screen point at latitude `lat` in `vp`.
fn meters_per_point(vp: &Viewport, lat: f64) -> f64 {
    let s = world_size(vp.zoom);
    if s <= 0.0 {
        return f64::INFINITY;
    }
    2.0 * std::f64::consts::PI * EARTH_RADIUS_M * lat.to_radians().cos().abs().max(1e-6) / s
}

/// Draw every saved location on the map (before the pins, which stay on top).
pub fn draw(app: &mut LightcraftApp, ui: &egui::Ui, painter: &egui::Painter, t: &Tokens, canvas: Rect, vp: &Viewport) {
    // a location deleted mid-drag (a command, another window): the drag ends with it
    if app.map.loc.drag.is_some_and(|d| app.session.map.locations.get(d.id).is_none()) {
        app.map.loc.drag = None;
    }
    if app.map.loc.selected.is_some_and(|id| app.session.map.locations.get(id).is_none()) {
        app.map.loc.selected = None;
    }
    let mut shown = Vec::new();
    let list: Vec<SavedLocation> = app.session.map.locations.list().to_vec();
    let world = world_size(vp.zoom) as f32;
    let reach = canvas.expand(canvas.width().max(canvas.height()));
    for l in &list {
        let (lat, lon, meters) = current(app, l);
        let Ok(circle) = GpsCircle::new(lat, lon, meters) else { continue };
        let local = vp.to_screen(LatLon { lat, lon });
        let at = pos2(canvas.min.x + local.x as f32, canvas.min.y + local.y as f32);
        if !(at.x.is_finite() && at.y.is_finite()) {
            continue;
        }
        let r_px = (meters / meters_per_point(vp, lat)) as f32;
        let r_px = if r_px.is_finite() { r_px.min(1e6) } else { 0.0 };
        let pole = circle.pole();
        // off screen (and not a band across the map): nothing to draw
        if pole.is_none() && !reach.expand(r_px).contains(at) {
            continue;
        }
        let selected = app.map.loc.selected == Some(l.id);
        let ink = if l.private { PRIVATE } else { LOCATION };
        let stroke = Stroke::new(if selected { 2.5 } else { 1.5 }, ink);
        let fill = ink.gamma_multiply(if selected { 0.22 } else { 0.12 });
        let east = destination(lat, lon, 90.0, meters);
        let handle = project_from(vp, (lat, lon), at, east);
        if r_px < 3.0 && pole.is_none() {
            painter.circle(at, 3.0, fill, stroke);
        } else {
            let ring: Vec<(f64, f64)> = (0..RING).map(|i| destination(lat, lon, i as f64 * 360.0 / RING as f64, meters)).collect();
            match pole {
                None => {
                    let pts: Vec<Pos2> = ring.iter().map(|p| project_from(vp, (lat, lon), at, *p)).collect();
                    fan(painter, at, &pts, fill);
                    if l.private {
                        let mut closed = pts.clone();
                        closed.extend(pts.first().copied());
                        painter.extend(egui::Shape::dashed_line(&closed, stroke, 6.0, 4.0));
                    } else {
                        painter.add(egui::Shape::closed_line(pts, stroke));
                    }
                }
                Some(north) => band(painter, vp, canvas, (lat, lon), at, &ring, north, world, fill, stroke),
            }
        }
        // the name above the circle (clear of the pins inside it), or above the centre for a
        // circle around a pole (whose top is the map's edge)
        // (circles a few points across, zoomed far out, go unnamed unless selected: the list names them)
        let label = if (list.len() <= MAX_LISTED && (r_px >= 8.0 || pole.is_some())) || selected {
            let g = painter.layout_no_wrap(l.name.clone(), t.semibold(11.5), Color32::WHITE);
            let y = if pole.is_none() {
                let top = project_from(vp, (lat, lon), at, destination(lat, lon, 0.0, meters)).y;
                if top.is_finite() { top.min(at.y - 8.0) - 12.0 } else { at.y - 20.0 }
            } else {
                // the centre is past the map's edge: the name goes just inside it
                (at.y + 16.0).clamp(canvas.top() + 14.0, (canvas.bottom() - 14.0).max(canvas.top() + 14.0))
            };
            let r = Rect::from_center_size(pos2(at.x, y), g.size() + vec2(10.0, 4.0));
            painter.rect_filled(r, 3.0, Color32::from_black_alpha(if selected { 200 } else { 150 }));
            painter.galley(r.min + vec2(5.0, 2.0), g, Color32::WHITE);
            Some(r)
        } else {
            None
        };
        painter.circle(at, if selected { 5.0 } else { 3.5 }, if selected { Color32::WHITE } else { ink }, Stroke::new(1.5, ink));
        if selected {
            painter.circle(handle, 5.0, Color32::WHITE, Stroke::new(2.0, ink));
        }
        if shown.len() < MAX_LISTED || selected {
            shown.push(Shown { id: l.id, center: at, handle, r_px, label });
        }
    }
    for s in &shown {
        if shown.len() <= MAX_LISTED {
            register(ui, format!("map:location:{}:centre", s.id), Rect::from_center_size(s.center, vec2(12.0, 12.0)));
        }
        if app.map.loc.selected == Some(s.id) {
            register(ui, format!("map:location:{}:radius", s.id), Rect::from_center_size(s.handle, vec2(12.0, 12.0)));
        }
    }
    app.map.loc.shown = shown;
}

/// A filled ring of points around `c` (a projected circle is star-shaped from its centre).
fn fan(painter: &egui::Painter, c: Pos2, pts: &[Pos2], fill: Color32) {
    if pts.len() < 3 || pts.iter().any(|p| !(p.x.is_finite() && p.y.is_finite())) {
        return;
    }
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(c, fill);
    for p in pts {
        mesh.colored_vertex(*p, fill);
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// A circle around a pole: on the map, the band between its edge and the map's top (bottom)
/// edge, repeated across the world's copies on screen.
#[allow(clippy::too_many_arguments)]
fn band(
    painter: &egui::Painter,
    vp: &Viewport,
    canvas: Rect,
    centre: (f64, f64),
    at: Pos2,
    ring: &[(f64, f64)],
    north: bool,
    world: f32,
    fill: Color32,
    stroke: Stroke,
) {
    // the edge as one sweep around the world: each step the shorter way round
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(ring.len() + 1);
    let mut prev: Option<f64> = None;
    let mut ordered: Vec<(f64, f64)> = ring.to_vec();
    ordered.sort_by(|a, b| a.1.total_cmp(&b.1));
    for (la, lo) in ordered {
        let lo = match prev {
            Some(p) => p + ((lo - p + 540.0).rem_euclid(360.0) - 180.0),
            None => lo,
        };
        prev = Some(lo);
        pts.push((la, lo));
    }
    if let Some(first) = pts.first().copied() {
        pts.push((first.0, first.1 + 360.0));
    }
    let edge_lat = if north { 90.0 } else { -90.0 };
    let screen: Vec<Pos2> = pts.iter().map(|p| project_from(vp, centre, at, *p)).collect();
    let edge_y = project_from(vp, centre, at, (edge_lat, centre.1)).y;
    if !(world.is_finite() && world > 0.0) || screen.iter().any(|p| !(p.x.is_finite() && p.y.is_finite())) {
        return;
    }
    // copies of the world to the left and right of the one drawn, as many as the screen needs
    let copies = ((canvas.width() / world).ceil() as i32 + 1).clamp(1, 64);
    for k in -copies..=copies {
        let shift = vec2(k as f32 * world, 0.0);
        let mut mesh = egui::Mesh::default();
        for pair in screen.windows(2) {
            if let [a, b] = pair {
                let (a, b) = (*a + shift, *b + shift);
                let base = mesh.vertices.len() as u32;
                mesh.colored_vertex(a, fill);
                mesh.colored_vertex(b, fill);
                mesh.colored_vertex(pos2(b.x, edge_y), fill);
                mesh.colored_vertex(pos2(a.x, edge_y), fill);
                mesh.add_triangle(base, base + 1, base + 2);
                mesh.add_triangle(base, base + 2, base + 3);
            }
        }
        painter.add(egui::Shape::mesh(mesh));
        painter.add(egui::Shape::line(screen.iter().map(|p| *p + shift).collect(), stroke));
    }
}

/// A drag starting at `origin`: on the selected location's radius or centre handle (unless
/// markers are locked), it moves that; `true` when it did.
pub fn drag_start(app: &mut LightcraftApp, origin: Pos2) -> bool {
    app.map.loc.drag = None;
    if app.ui.map_lock_markers {
        return false;
    }
    let Some(id) = app.map.loc.selected else { return false };
    let Some(s) = app.map.loc.shown.iter().find(|s| s.id == id).copied() else { return false };
    let Some(l) = app.session.map.locations.get(id).cloned() else { return false };
    let on_radius = s.handle.distance(origin) <= HANDLE_GRAB;
    let on_centre = s.center.distance(origin) <= HANDLE_GRAB;
    if !(on_radius || on_centre) {
        return false;
    }
    let target = if on_radius { s.handle } else { s.center };
    app.map.loc.drag = Some(LocDrag { id, radius: on_radius, grab: target - origin, lat: l.lat, lon: l.lon, meters: l.radius });
    true
}

/// The drag follows the pointer (`at`, screen points) over the map at `canvas` in `vp`.
pub fn drag_update(app: &mut LightcraftApp, canvas: Rect, vp: &Viewport, at: Pos2) {
    let Some(mut d) = app.map.loc.drag else { return };
    let p = at + d.grab;
    let ll = vp.to_lat_lon(Point::new(f64::from(p.x - canvas.min.x), f64::from(p.y - canvas.min.y)));
    if !(ll.lat.is_finite() && ll.lon.is_finite()) {
        return;
    }
    if d.radius {
        let m = distance_m(d.lat, d.lon, ll.lat, ll.lon);
        if m.is_finite() {
            d.meters = m.clamp(MIN_RADIUS_M, MAX_RADIUS_M);
        }
    } else {
        d.lat = ll.lat.clamp(-90.0, 90.0);
        d.lon = ll.lon.clamp(-180.0, 180.0);
    }
    app.map.loc.drag = Some(d);
}

/// Whether a location drag is on.
pub fn dragging(app: &LightcraftApp) -> bool {
    app.map.loc.drag.is_some()
}

/// The drag ends: over the map, the location takes its new place or radius (one
/// `map.location.update`); off the map, or once the location is gone, nothing changes.
pub fn drag_end(app: &mut LightcraftApp, ctx: &egui::Context, over_map: bool) {
    let Some(d) = app.map.loc.drag.take() else { return };
    if !over_map || app.session.map.locations.get(d.id).is_none() {
        return;
    }
    let p = if d.radius { json!({"id": d.id, "radius": d.meters}) } else { json!({"id": d.id, "lat": d.lat, "lon": d.lon}) };
    if let Err(e) = app.run("map.location.update", p) {
        app.toast(ctx, e);
    }
}

/// A click at `p` (screen points) not on a pin: on a location's centre or name it selects that
/// location (`true`); elsewhere it drops the selection.
pub fn click(app: &mut LightcraftApp, p: Pos2) -> bool {
    let hit = app.map.loc.shown.iter().rev().find(|s| s.center.distance(p) <= HANDLE_GRAB || s.label.is_some_and(|r| r.contains(p))).map(|s| s.id);
    app.map.loc.selected = hit;
    hit.is_some()
}

/// The cursor over a location's handles (while one can be dragged).
pub fn hover_cursor(app: &LightcraftApp, p: Pos2) -> Option<egui::CursorIcon> {
    if let Some(d) = app.map.loc.drag {
        return Some(if d.radius { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::Grabbing });
    }
    let s = app.map.loc.shown.iter().find(|s| Some(s.id) == app.map.loc.selected)?;
    if app.ui.map_lock_markers {
        return None;
    }
    if s.handle.distance(p) <= HANDLE_GRAB {
        Some(egui::CursorIcon::ResizeHorizontal)
    } else if s.center.distance(p) <= HANDLE_GRAB {
        Some(egui::CursorIcon::Grab)
    } else {
        None
    }
}

/// `map.state` → `locations`, `selectedLocation`, `movingLocation`.
pub fn state(app: &mut LightcraftApp, s: &mut Value) {
    let counts = counts(app).clone();
    let list: Vec<Value> = app
        .map
        .loc
        .shown
        .iter()
        .filter_map(|sh| {
            let l = app.session.map.locations.get(sh.id)?;
            Some(json!({
                "id": l.id,
                "name": l.name,
                "private": l.private,
                "x": sh.center.x,
                "y": sh.center.y,
                "radiusPx": sh.r_px,
                "handle": {"x": sh.handle.x, "y": sh.handle.y},
                "inView": counts.get(&l.id).copied().unwrap_or(0),
                "selected": app.map.loc.selected == Some(l.id),
            }))
        })
        .collect();
    s["locations"] = json!(list);
    s["selectedLocation"] = json!(app.map.loc.selected);
    s["movingLocation"] = json!(
        app.map
            .loc
            .drag
            .map(|d| json!({"id": d.id, "lat": d.lat, "lon": d.lon, "radius": d.meters, "part": if d.radius { "radius" } else { "centre" }}))
    );
    s["privateMiniMap"] = json!(app.map.loc.private_mini.map(|(photo, loc, _)| json!({"photo": photo.0, "location": loc})));
}

/// The view that shows location `l` whole in a `w × h` map.
fn fit(l: &SavedLocation, w: f64, h: f64, max_zoom: f64) -> super::Camera {
    let pts: Vec<(f64, f64)> = [0.0, 90.0, 180.0, 270.0]
        .iter()
        .map(|b| destination(l.lat, l.lon, *b, l.radius))
        .map(|(a, b)| (a, lightcraft_map::mercator::wrap_lon(b)))
        .collect();
    match lightcraft_map::Bounds::of(pts) {
        Some(b) if w > 0.0 && h > 0.0 => {
            let v = lightcraft_map::fit_bounds(&b, w, h, 40.0, max_zoom);
            super::Camera { center: LatLon { lat: l.lat, lon: l.lon }, zoom: v.zoom.clamp(0.0, max_zoom) }
        }
        _ => super::Camera { center: LatLon { lat: l.lat, lon: l.lon }, zoom: 12.0f64.min(max_zoom) },
    }
}

/// Show location `id`: selected, and the map centred on it at a zoom that holds it.
fn show_location(app: &mut LightcraftApp, id: u64) -> Result<(), String> {
    let l = app.session.map.locations.get(id).cloned().ok_or_else(|| format!("no saved location {id}"))?;
    app.map.loc.selected = Some(id);
    let (w, h) = app.map.rect.map(|r| (f64::from(r.width()), f64::from(r.height()))).unwrap_or((0.0, 0.0));
    app.map.camera = Some(fit(&l, w, h, super::max_zoom(app)));
    app.map.fit_pending = false;
    super::apply_view_area(app);
    Ok(())
}

/// The radius of a new location at the map's centre: a sixth of the map's shorter side.
fn default_radius(app: &LightcraftApp) -> f64 {
    let vp = viewport(app);
    let side = vp.width.min(vp.height);
    let m = if side > 0.0 { side / 6.0 * meters_per_point(&vp, vp.center.lat) } else { 1000.0 };
    if m.is_finite() { m.clamp(MIN_RADIUS_M, MAX_RADIUS_M).round().max(MIN_RADIUS_M) } else { 1000.0 }
}

/// New Location: ask for its name, then save it at the map's centre (`from_selection`: around
/// the selected photos' positions).
pub fn new_location(app: &mut LightcraftApp, from_selection: bool) {
    let params = if from_selection {
        json!({"fromSelection": true})
    } else {
        let vp = viewport(app);
        json!({"lat": vp.center.lat, "lon": vp.center.lon, "radius": default_radius(app)})
    };
    super::super::dialogs::prompt(app, "New Location", "Location name", "", "map.location.add", params, "name");
}

/// The map's saved-location UI commands (`None`: not one of them).
pub fn command(app: &mut LightcraftApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let r = match id {
        "map.location.show" => {
            let Some(loc) = p.get("id").and_then(Value::as_u64) else {
                return Some(Err("map.location.show: `id` is a saved location's id".into()));
            };
            show_location(app, loc).map(|()| super::state(app))
        }
        "map.location.select" => match p.get("id") {
            None | Some(Value::Null) => {
                app.map.loc.selected = None;
                Ok(json!({"selected": Value::Null}))
            }
            Some(v) => match v.as_u64().filter(|i| app.session.map.locations.get(*i).is_some()) {
                Some(i) => {
                    app.map.loc.selected = Some(i);
                    Ok(json!({"selected": i}))
                }
                None => Err(format!("map.location.select: no saved location {v}")),
            },
        },
        "map.location.selectPhotos" => {
            let Some(loc) = p.get("id").and_then(Value::as_u64).filter(|i| app.session.map.locations.get(*i).is_some()) else {
                return Some(Err("map.location.selectPhotos: `id` is a saved location's id".into()));
            };
            let ids = photos_in_view(app, loc);
            let first = ids.first().copied();
            app.run("library.select", json!({"ids": ids, "active": first})).map(|_| json!({"selected": ids.len()}))
        }
        _ => return None,
    };
    Some(r)
}

/// Width of the Saved Locations list over the map (points).
const LIST_W: f32 = 250.0;
const ROW_H: f32 = 27.0;
const HEADER_H: f32 = 30.0;

/// The Saved Locations list, over the map's top-left corner: a header (its title, how many, a
/// plus button for a new one, a chevron that folds it) and a row per location with its count of
/// photos in view. Drawn after the map, so it takes clicks and scrolls before the map does.
pub fn overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect) {
    app.map.loc.overlay = None;
    // a map too small for it: the list waits (the circles and the commands still work)
    if canvas.width() < LIST_W + 220.0 || canvas.height() < 200.0 {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let list: Vec<SavedLocation> = app.session.map.locations.list().to_vec();
    let open = !app.map.loc.list_folded;
    let rows_h = if !open {
        0.0
    } else if list.is_empty() {
        44.0
    } else {
        (list.len() as f32 * ROW_H + 6.0).min((canvas.height() * 0.5 - HEADER_H).max(ROW_H * 2.0))
    };
    let frame = Rect::from_min_size(canvas.min + vec2(12.0, 12.0), vec2(LIST_W, HEADER_H + rows_h));
    app.map.loc.overlay = Some(frame);
    register(ui, "map:locations", frame);
    // the list's own room: clicks, drags and scrolls on it never reach the map below
    let _ = ui.interact(frame, egui::Id::unique("map-locations-bg"), Sense::click_and_drag());
    ui.painter().rect_filled(frame.expand(1.0), 6.0, Color32::from_black_alpha(90));
    ui.painter().rect_filled(frame, 6.0, t.chrome.gamma_multiply(0.96));
    // the header: a click folds or unfolds the list
    let header = Rect::from_min_size(frame.min, vec2(LIST_W, HEADER_H));
    let hresp = ui.interact(header.with_max_x(header.right() - 34.0), egui::Id::unique("map-locations-header"), Sense::click());
    register(ui, "map:locationsHeader", header);
    let title = format!("{} ({})", crate::i18n::tr("Saved Locations"), list.len());
    let g = ui.painter().text(pos2(header.left() + 12.0, header.center().y), Align2::LEFT_CENTER, title, t.semibold(12.5), t.text);
    let c = pos2(g.right() + 10.0, header.center().y);
    let col = if hresp.hovered() { t.text } else { t.text_dim };
    let pts = if open {
        vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)]
    } else {
        vec![c + vec2(-2.0, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.0, 4.0)]
    };
    ui.painter().add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
    if hresp.clicked() {
        app.map.loc.list_folded = open;
    }
    let mut hdr = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(header.right() - 34.0, header.top()), pos2(header.right() - 4.0, header.bottom())))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let plus = crate::widgets::icon_button(&mut hdr, "locationNew", crate::icons::Icon::Plus, vec2(26.0, 26.0), false, true, "New Location");
    egui::Popup::menu(&plus).show(|ui| {
        let b = ui.button(crate::i18n::tr("New Location at Map Centre…"));
        register(ui, "map:locationNew:centre", b.rect);
        if b.clicked() {
            new_location(app, false);
        }
        let any =
            app.session.targets(&json!({})).unwrap_or_default().iter().any(|id| app.session.catalog.photo(*id).is_some_and(|p| p.meta.gps.is_some()));
        let b = ui.add_enabled(any, egui::Button::new(crate::i18n::tr("New Location from Selection…")));
        register(ui, "map:locationNew:selection", b.rect);
        if b.clicked() {
            new_location(app, true);
        }
    });
    if !open {
        return;
    }
    let body = Rect::from_min_max(pos2(frame.left(), header.bottom()), frame.max);
    ui.painter().line_segment([body.left_top() + vec2(8.0, 0.0), body.right_top() - vec2(8.0, 0.0)], Stroke::new(1.0, t.divider));
    if list.is_empty() {
        let g = ui.painter().layout(
            crate::i18n::tr("No saved locations. Use + to save the place the map shows.").to_string(),
            t.font(11.5),
            t.text_dim,
            LIST_W - 24.0,
        );
        ui.painter().galley(pos2(body.left() + 12.0, body.top() + 6.0), g, t.text_dim);
        return;
    }
    let counts = counts(app).clone();
    let selected_photos: std::collections::HashSet<u64> = app.session.selection.ids.iter().map(|p| p.0).collect();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body.shrink2(vec2(0.0, 3.0))).layout(egui::Layout::top_down(egui::Align::Min)));
    child.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical().id_salt("map-locations-list").auto_shrink([false, false]).show_rows(&mut child, ROW_H, list.len(), |ui, range| {
        for l in list.get(range).unwrap_or(&[]) {
            location_row(app, ui, &t, l, counts.get(&l.id).copied().unwrap_or(0), &selected_photos);
        }
    });
}

/// One row: [checkbox] name (private) … count. The checkbox selects the photos in view inside it
/// (ticked when they all are); a click on the row shows the location; right-click for its menu.
fn location_row(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    t: &Tokens,
    l: &SavedLocation,
    count: usize,
    selected_photos: &std::collections::HashSet<u64>,
) {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    register(ui, format!("map:location:{}", l.id), r);
    let shown = app.map.loc.selected == Some(l.id);
    let edge = r.right();
    let inner = Rect::from_min_max(r.min + vec2(4.0, 0.0), pos2((edge - 4.0).max(r.left() + 4.0), r.bottom()));
    if shown {
        ui.painter().rect_filled(inner, 4.0, t.canvas);
    } else if resp.hovered() {
        ui.painter().rect_filled(inner, 4.0, t.hover.gamma_multiply(0.6));
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, shown, &l.name));
    // the checkbox: are this location's photos (in view) the ones selected?
    let check = Rect::from_center_size(pos2(r.left() + 18.0, r.center().y), vec2(13.0, 13.0));
    register(ui, format!("map:location:{}:check", l.id), check);
    let ticked = count > 0 && count <= selected_photos.len() && app.map.loc.index.inside(&l.circle()).iter().all(|i| selected_photos.contains(i));
    let p = ui.painter();
    p.rect_stroke(check, 2.0, Stroke::new(1.2, t.text_label), egui::StrokeKind::Inside);
    if ticked {
        p.rect_filled(check.shrink(1.0), 2.0, t.accent);
        p.line(
            vec![check.left_center() + vec2(3.0, 0.0), check.center_bottom() + vec2(-1.0, -3.0), check.right_top() + vec2(-3.0, 3.0)],
            Stroke::new(1.6, Color32::WHITE),
        );
    }
    let ink = if l.private { PRIVATE } else { LOCATION };
    p.circle_filled(pos2(check.right() + 12.0, r.center().y), 4.0, ink);
    let count_g = p.layout_no_wrap(count.to_string(), t.font(12.5), t.text_dim);
    let count_left = edge - 12.0 - count_g.size().x;
    let name_left = check.right() + 22.0;
    let suffix = if l.private { format!(" · {}", crate::i18n::tr("private")) } else { String::new() };
    let label = format!("{}{suffix}", l.name);
    let room = (count_left - 8.0 - name_left).max(0.0);
    let measure = |s: &str| p.layout_no_wrap(s.to_string(), t.font(13.0), t.text_label).size().x;
    let text = if measure(&label) <= room { label.clone() } else { crate::widgets::elide_head(&label, room, measure) };
    p.text(pos2(name_left, r.center().y), Align2::LEFT_CENTER, &text, t.font(13.0), if shown { t.text } else { t.text_label });
    let count_rect = Rect::from_min_size(pos2(count_left, r.center().y - count_g.size().y / 2.0), count_g.size());
    register(ui, format!("map:location:{}:count", l.id), count_rect);
    p.galley(count_rect.min, count_g, t.text_dim);
    let resp = if text != label { resp.on_hover_text(&label) } else { resp };
    if resp.clicked() {
        let at = resp.interact_pointer_pos();
        let result = if at.is_some_and(|a| check.expand(4.0).contains(a)) {
            if ticked { app.run("library.select", json!({"ids": []})) } else { app.run("map.location.selectPhotos", json!({"id": l.id})) }
        } else {
            app.run("map.location.show", json!({"id": l.id}))
        };
        if let Err(e) = result {
            app.toast(ui.ctx(), e);
        }
    }
    let id = l.id;
    let (name, private) = (l.name.clone(), l.private);
    resp.context_menu(|ui| {
        let b = ui.button(crate::i18n::tr("Go to Location"));
        register(ui, "map:locationMenu:show", b.rect);
        if b.clicked() {
            let _ = app.run("map.location.show", json!({"id": id}));
            ui.close();
        }
        let b = ui.button(crate::i18n::tr("Select Photos in Location"));
        register(ui, "map:locationMenu:selectPhotos", b.rect);
        if b.clicked() {
            let _ = app.run("map.location.selectPhotos", json!({"id": id}));
            ui.close();
        }
        ui.separator();
        let b = ui.button(crate::i18n::tr("Rename…"));
        register(ui, "map:locationMenu:rename", b.rect);
        if b.clicked() {
            super::super::dialogs::prompt(app, "Rename Location", "Location name", &name, "map.location.update", json!({"id": id}), "name");
            ui.close();
        }
        let mut on = private;
        let b = ui.checkbox(&mut on, crate::i18n::tr("Private"));
        register(ui, "map:locationMenu:private", b.rect);
        let b = b.on_hover_text(crate::i18n::tr("Photos inside are exported without their location, and the Info panel hides their map"));
        if b.changed() {
            if let Err(e) = app.run("map.location.update", json!({"id": id, "private": on})) {
                app.toast(ui.ctx(), e);
            }
            ui.close();
        }
        ui.separator();
        let b = ui.button(crate::i18n::tr("Delete Location…"));
        register(ui, "map:locationMenu:delete", b.rect);
        if b.clicked() {
            app.ui.dialog = Some(crate::state::Dialog::DeleteLocation { id, name: name.clone() });
            ui.close();
        }
    });
}

/// The private location the Info panel's map would show photo `id` in (unless the user asked to
/// see it), noted for `map.state`.
pub fn hides_mini_map(app: &mut LightcraftApp, id: PhotoId, gps: Option<(f64, f64)>, pass: u64) -> Option<SavedLocation> {
    // Show Map holds for one photo while it is shown: another one ends it
    if app.map.loc.reveal_private.is_some_and(|r| r != id) {
        app.map.loc.reveal_private = None;
    }
    if app.map.loc.reveal_private == Some(id) {
        return None;
    }
    let l = app.session.map.locations.private_at(gps).cloned()?;
    app.map.loc.private_mini = Some((id, l.id, pass));
    Some(l)
}
