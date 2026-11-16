//! Map module view: interactive slippy map canvas, photo pins and clusters, geotagging, loupe
//! popover and saved locations. Tiles come from the engine's tile service (OpenStreetMap by
//! default, under its tile usage policy) and are drawn by [`tiles`].
//!
//! - **Pins**: the photos of the current list (source, filter and sort, as the grid) with a valid
//!   position, projected once per change of the list ([`Points`]), grouped every frame into
//!   world-anchored cells of about 60 px ([`lightcraft_engine::map::cluster`]): panning never
//!   regroups them and zooming in only splits them. A click selects a pin's photos (⌘ adds), a
//!   double-click opens a single photo in Detail or zooms in on a cluster, hovering shows the first
//!   photo's thumbnail; "N photos without GPS" counts the rest.
//! - **Moving**: drag to pan, wheel to zoom around the pointer, double-click to zoom in; Fit to
//!   Photos (also the first time the map is shown). Commands: `view.map`, `map.fit`, `map.setView`,
//!   `map.zoomIn`, `map.zoomOut`, `map.state`.
//! - **Geotagging**: photos dragged from the filmstrip and dropped on the map get the drop point's
//!   position; a drag that starts on a single photo's pin moves it (a cluster's drag pans, as in
//!   Lightroom Classic) unless Lock Markers is on (`map.lockMarkers`, ⌘K / Ctrl+K, saved in
//!   `ui.json`); a pin's right-click menu removes its photos' position. Each is one undo step
//!   (`photo.setGps`, `photo.removeGps`). Pin moves, the pin menu and the lock are ported from PR
//!   #810, the clustering from PR #797 (Jason Baldwin).

pub mod tiles;

use std::collections::HashSet;

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_engine::map::{Bounds, CLUSTER_CELL, Cluster, LocationFilter, MapPoint, Viewport, fit_bounds};
use lightcraft_geom::{LatLon, Point};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::ViewMode;
use crate::theme::Tokens;

/// Number of screen pixels per tile at standard resolution.
const TILE_SIZE: f32 = 256.0;

/// Height of the map's bottom toolbar (Map Style, zoom, Lock Markers, credit).
const TOOLBAR_H: f32 = 36.0;

/// Height of the location filter bar over the top of the map, with its margin.
const FILTER_BAR_H: f32 = 50.0;

/// How far from a pin's point a click or a hover still finds it (points).
const PIN_HIT: f64 = 16.0;

/// The deepest zoom Fit to Photos goes (a single photo isn't shown at street level).
const FIT_MAX_ZOOM: f64 = 16.0;

/// The lowest zoom the map shows.
const MIN_ZOOM: f64 = 1.0;

/// The photos of the current list that have a valid position, projected once per change of the
/// list (its generation from [`lightcraft_engine::Session::visible_shared`]).
#[derive(Default)]
pub struct Points {
    generation: Option<(u64, LocationFilter)>,
    pub points: Vec<MapPoint>,
    /// Photos of the list without a valid position.
    pub without: usize,
}

/// The current list's points (recomputed only when the list or the location filter changed).
fn refresh_points(app: &mut LightcraftApp) {
    let filter = app.ui.map.filter;
    let (generation, list) = app.session.visible_shared();
    if app.map_points.generation == Some((generation, filter)) {
        return;
    }
    let mut points = Vec::new();
    let mut without = 0;
    for id in list.iter() {
        match app.session.catalog.photo(*id).and_then(|p| lightcraft_engine::map::from_gps(p.meta.gps)) {
            Some(pos) => points.push(MapPoint::new(id.0, pos)),
            None => without += 1,
        }
    }
    // Untagged shows the photos without a position: no pins
    if filter == LocationFilter::Untagged {
        points.clear();
    }
    app.map_points = Points { generation: Some((generation, filter)), points, without };
}

/// A single photo's pin being dragged to a new place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PinDrag {
    pub id: u64,
    /// The pin's point minus the pointer (so the pin doesn't jump under it).
    pub grab: egui::Vec2,
    /// Where the pin is drawn now (screen points).
    pub at: Pos2,
}

/// What the Map view drew in its last frame (`map.state`; tests and agents read it).
#[derive(Clone, Debug, Default)]
pub struct MapDrawn {
    /// The map canvas (screen points), `None` when the Map view wasn't drawn.
    pub canvas: Option<Rect>,
    /// Whether the filmstrip was drawn under the map.
    pub filmstrip: bool,
    /// Each pin or cluster: its photos, how many of them are selected, and where it is.
    pub pins: Vec<DrawnPin>,
    /// Photos carried over the map: how many, the pointer, and where they would land.
    pub drop: Option<(usize, Pos2, LatLon)>,
    /// A pin being moved: its photo and where it is drawn.
    pub moving: Option<(u64, Pos2)>,
    /// The photos of the pin under the pointer (its thumbnail is shown).
    pub hovered: Option<Vec<u64>>,
    /// Photos with and without a position in the current list.
    pub with_gps: usize,
    pub without_gps: usize,
    /// Time the clustering took this frame (ms).
    pub cluster_ms: f64,
}

/// A pin or cluster as drawn: its photos, how many of them are selected, its screen point and
/// position.
#[derive(Clone, Debug)]
pub struct DrawnPin {
    pub ids: Vec<u64>,
    pub selected: usize,
    pub at: Pos2,
    pub pos: LatLon,
}

/// `map.state`: the view, the pins drawn last frame (each with `selected`, how many of its photos
/// are selected), a drop or a pin move in progress, and the counts.
pub fn state(app: &LightcraftApp) -> Value {
    let d = &app.map_drawn;
    let source = app.session.map.effective_source();
    let pins: Vec<Value> = d
        .pins
        .iter()
        .take(500)
        .map(|p| json!({"ids": p.ids, "count": p.ids.len(), "selected": p.selected, "x": p.at.x, "y": p.at.y, "lat": p.pos.lat, "lon": p.pos.lon}))
        .collect();
    json!({
        "lat": app.ui.map.center_lat,
        "lon": app.ui.map.center_lon,
        "zoom": app.ui.map.zoom,
        "maxZoom": source.max_zoom(),
        "source": source.id(),
        "offline": source.is_offline(),
        "attribution": app.session.map.attribution(),
        "shown": app.ui.view == ViewMode::Map && d.canvas.is_some(),
        "canvas": d.canvas.map(|r| json!({"x": r.left(), "y": r.top(), "width": r.width(), "height": r.height()})),
        "filmstrip": d.filmstrip,
        "pins": pins,
        "clusterCount": d.pins.len(),
        "clusterMs": d.cluster_ms,
        "withGps": d.with_gps,
        "withoutGps": d.without_gps,
        "lockMarkers": app.ui.map.locked,
        "movingPin": d.moving.map(|(id, at)| json!({"id": id, "x": at.x, "y": at.y})),
        "hovered": d.hovered,
        "drop": d.drop.map(|(n, at, to)| json!({"count": n, "x": at.x, "y": at.y, "lat": to.lat, "lon": to.lon})),
        // the Info panel's map, when it was drawn in the last frame
        "miniMap": app.mini_map.as_ref().map(|m| json!({
            "photo": m.photo,
            "lat": m.center.lat,
            "lon": m.center.lon,
            "zoom": m.zoom,
            "rect": {"x": m.rect.left(), "y": m.rect.top(), "width": m.rect.width(), "height": m.rect.height()},
            "pin": {"x": m.pin.x, "y": m.pin.y},
            "tiles": {"drawn": m.tiles.tiles, "fromParent": m.tiles.from_parent, "missing": m.tiles.missing, "offline": m.tiles.offline},
        })),
    })
}

/// The map's UI commands (`map.fit`, `map.setView`, `map.zoomIn`, `map.zoomOut`,
/// `map.lockMarkers`); `None` when `id` is none of them.
pub fn command(app: &mut LightcraftApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    let max_zoom = f64::from(app.session.map.effective_source().max_zoom());
    let r = match id {
        "map.fit" => {
            fit(app);
            Ok(state(app))
        }
        "map.setView" => set_view(app, p, max_zoom).map(|()| state(app)),
        "map.zoomIn" | "map.zoomOut" => {
            let step = if id == "map.zoomIn" { 1.0 } else { -1.0 };
            let m = &mut app.ui.map;
            m.zoom = (m.zoom.round() + step).clamp(MIN_ZOOM, max_zoom);
            m.fitted = true;
            Ok(state(app))
        }
        // {on?}: without it, toggles (Cmd/Ctrl+K in the Map view, the toolbar's Lock Markers)
        "map.lockMarkers" | "map.toggleLock" | "map.lock" => {
            let on = match p.get("on") {
                None | Some(Value::Null) => !app.ui.map.locked,
                Some(v) => match v.as_bool() {
                    Some(b) => b,
                    None => return Some(Err(format!("{id}: `on` is true or false"))),
                },
            };
            app.ui.map.locked = on;
            Ok(json!({"on": on, "locked": on}))
        }
        _ => return None,
    };
    Some(r)
}

/// `map.setView {lat?, lon?, zoom?}`: what is given must be a finite number (lat −90…90, shown
/// within the map's ±85.05°; lon wrapped; zoom 1…the source's max), else nothing changes.
fn set_view(app: &mut LightcraftApp, p: &Value, max_zoom: f64) -> Result<(), String> {
    let num = |k: &str| -> Result<Option<f64>, String> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_f64().filter(|v| v.is_finite()).map(Some).ok_or_else(|| format!("map.setView: `{k}` must be a number")),
        }
    };
    let (lat, lon, zoom) = (num("lat")?, num("lon")?, num("zoom")?);
    if lat.is_some_and(|l| !(-90.0..=90.0).contains(&l)) {
        return Err("map.setView: `lat` must be −90…90".into());
    }
    if zoom.is_some_and(|z| !(0.0..=24.0).contains(&z)) {
        return Err(format!("map.setView: `zoom` must be {MIN_ZOOM}…{max_zoom}"));
    }
    let m = &mut app.ui.map;
    if let Some(l) = lat {
        m.center_lat = l.clamp(-lightcraft_engine::map::MAX_LATITUDE, lightcraft_engine::map::MAX_LATITUDE);
    }
    if let Some(l) = lon {
        m.center_lon = lightcraft_engine::map::mercator::wrap_lon(l);
    }
    if let Some(z) = zoom {
        m.zoom = z.clamp(MIN_ZOOM, max_zoom);
    }
    m.fitted = true;
    Ok(())
}

/// Fit to Photos: the view that shows every pin of the current list (the canvas last drawn, or a
/// typical one before the map was shown). No photo with a position: the view stays.
fn fit(app: &mut LightcraftApp) {
    refresh_points(app);
    let max_zoom = f64::from(app.session.map.effective_source().max_zoom()).min(FIT_MAX_ZOOM);
    let size = app.ui.map.last_canvas.map_or((1000.0, 700.0), |r| (f64::from(r.width()), f64::from(r.height())));
    app.ui.map.fitted = true;
    let Some(b) = Bounds::of(app.map_points.points.iter().map(|p| (p.pos.lat, p.pos.lon))) else { return };
    // fit into the part of the map the filter bar and the toolbar leave free, then move the centre
    // so that part's middle shows the photos' middle
    let top = if app.ui.map.show_filter_bar { f64::from(FILTER_BAR_H) } else { 0.0 };
    let free = (size.1 - top - f64::from(TOOLBAR_H)).max(1.0);
    let v = fit_bounds(&b, size.0, free, 48.0, max_zoom);
    let target = top + free / 2.0;
    let full = Viewport::new(v.center, v.zoom, size.0, size.1);
    let center = full.to_lat_lon(Point::new(size.0 / 2.0, size.1 - target));
    let m = &mut app.ui.map;
    m.center_lat = center.lat;
    m.center_lon = center.lon;
    m.zoom = v.zoom.max(MIN_ZOOM);
}

/// Draw the Map view: the map, and the shared filmstrip under it when it is on (View ▸
/// Filmstrip, `/`), as in Detail. Photos dragged from the filmstrip onto the map get
/// the drop point's position (`photo.setGps`, one undo step).
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let full = ui.available_rect_before_wrap();
    app.map_drawn = MapDrawn::default();
    if full.width() <= 10.0 || full.height() <= 10.0 {
        return;
    }
    // the filmstrip takes the bottom, unless the window is too short to leave a map above it
    let show_film = app.ui.filmstrip && !app.ui.fullscreen && full.height() - t.film_h >= 2.0 * TOOLBAR_H;
    let film_h = if show_film { t.film_h } else { 0.0 };
    let rect = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - film_h));
    if show_film {
        crate::panels::detail::filmstrip(app, ui, Rect::from_min_max(pos2(full.left(), rect.bottom()), full.max));
    }
    app.map_drawn.filmstrip = show_film;

    let source = app.session.map.effective_source();
    let max_zoom = f64::from(source.max_zoom());
    let (left_w, navigator_h) = if app.ui.map.navigator_open { (220.0_f32, 160.0_f32) } else { (0.0, 0.0) };
    let canvas_rect = Rect::from_min_max(pos2(rect.left() + left_w, rect.top()), rect.max);
    app.ui.map.last_canvas = Some(canvas_rect);
    refresh_points(app);
    // the first time the map is shown (this session) it fits the photos
    if !app.ui.map.fitted {
        fit(app);
    }

    let mut map_ui = app.ui.map.clone();
    let mut center = LatLon::new(map_ui.center_lat, map_ui.center_lon).unwrap_or(LatLon { lat: 45.57, lon: -73.2 });
    let mut zoom = if map_ui.zoom.is_finite() { map_ui.zoom.clamp(MIN_ZOOM, max_zoom) } else { MIN_ZOOM };

    // 1. Background Map Canvas & Interaction
    let canvas_response = ui.allocate_rect(canvas_rect, Sense::click_and_drag());
    crate::widgets::register(ui, "map:canvas", canvas_rect);
    let painter = ui.painter_at(canvas_rect);
    let (w, h) = (f64::from(canvas_rect.width()), f64::from(canvas_rect.height()));
    let local = |p: Pos2| Point::new(f64::from(p.x - canvas_rect.min.x), f64::from(p.y - canvas_rect.min.y));
    let screen = |p: Point| pos2(canvas_rect.min.x + p.x as f32, canvas_rect.min.y + p.y as f32);
    let zone = drop_zone(canvas_rect, map_ui.show_filter_bar);
    let vp = navigate(app, ui, &canvas_response, Viewport::new(center, zoom, w, h), (canvas_rect, zone, max_zoom), &mut map_ui);
    center = vp.center;
    zoom = vp.zoom;
    let pointer = ui.input(|i| i.pointer.latest_pos());
    let carrying = app.ui.dragging_photos.is_some();

    // Tiles (the service is asked for the view plus one ring)
    tiles::draw(&mut app.session, &painter, canvas_rect, center, zoom, 1);
    app.map_drawn.canvas = Some(canvas_rect);

    // Photos carried from the filmstrip: a pin with their count follows the pointer over the map;
    // a release there sets their position (overwriting one they had).
    let drop = drop_photos(app, ui, zone, |p| vp.to_lat_lon(local(p)));
    refresh_points(app);

    // 2. Cluster the pins of the view (world-anchored cells: panning never regroups them)
    let started = web_time::Instant::now();
    let clusters = lightcraft_engine::map::cluster::cluster(&app.map_points.points, &vp, CLUSTER_CELL);
    app.map_drawn.cluster_ms = started.elapsed().as_secs_f64() * 1e3;
    app.map_drawn.with_gps = app.map_points.points.len();
    app.map_drawn.without_gps = app.map_points.without;

    // 3. Render Clusters and Markers
    let PinInput { clicked, double_clicked_photo, zoom_target, hovered } = draw_pins(app, ui, &painter, &clusters, (canvas_rect, pointer), &map_ui);
    if let Some((target, next_z)) = zoom_target {
        center = target;
        zoom = next_z.min(max_zoom);
    }
    if let Some((ids, m)) = clicked {
        select_pin(app, ui, &ids, m, &mut map_ui);
    }
    if let Some(id) = double_clicked_photo {
        let _ = app.run("library.select", json!({"ids": [id.0]}));
        app.ui.view = ViewMode::Detail;
    }
    pin_menu(app, &canvas_response, &clusters, local, &mut map_ui);
    if let Some(d) = map_ui.pin_drag {
        app.map_drawn.moving = Some((d.id, d.at));
        render_single_pin(&painter, d.at, app.session.selection.contains(PhotoId(d.id)), true);
    }

    // 4. Render Active Pin Loupe Popover Card
    if let Some(active_id) = map_ui.active_pin_photo {
        let pin_coord = app.session.catalog.photo(active_id).and_then(|ph| lightcraft_engine::map::from_gps(ph.meta.gps));
        if let Some(coord) = pin_coord {
            let pin_pos = screen(vp.to_screen(coord));
            if canvas_rect.contains(pin_pos) && hovered.is_none() && map_ui.pin_drag.is_none() {
                render_loupe_popover(app, ui, pin_pos, active_id, &mut map_ui);
            }
        }
    }

    // 4.5. Render active / matching saved location boundary circle
    for loc in &app.session.saved_locations {
        if (loc.lat - center.lat).abs() < 0.0001 && (loc.lon - center.lon).abs() < 0.0001 {
            let screen_pos = screen(vp.to_screen(LatLon { lat: loc.lat, lon: loc.lon }));
            let meters_per_px = (40075016.686 * (loc.lat.to_radians().cos().abs())) / (256.0 * 2.0_f64.powf(zoom));
            if meters_per_px > 0.0 {
                let radius_px = (loc.radius / meters_per_px) as f32;
                if radius_px > 4.0 && radius_px < 8000.0 {
                    painter.circle_filled(screen_pos, radius_px, Color32::from_rgba_unmultiplied(255, 204, 0, 20));
                    painter.circle_stroke(screen_pos, radius_px, Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 204, 0, 160)));
                }
            }
        }
    }

    // the hovered pin's thumbnail (not while something is carried)
    if let Some(ids) = hovered.as_ref().filter(|_| !carrying && map_ui.pin_drag.is_none())
        && let Some(at) = app.map_drawn.pins.iter().find(|p| &p.ids == ids).map(|p| p.at)
    {
        hover_thumbnail(app, ui, &t, at, ids);
    }
    app.map_drawn.hovered = hovered;

    if let Some((n, at, _)) = drop {
        drop_indicator(&painter, &t, at, n);
    }
    app.map_drawn.drop = drop;

    // "N photos without GPS", bottom left over the map
    if app.map_points.without > 0 {
        let n = app.map_points.without;
        let text = if n == 1 { crate::i18n::tr("1 photo without GPS").to_string() } else { crate::i18n::tr_format!("{n} photos without GPS", n = n) };
        let at = pos2(canvas_rect.left() + 12.0, canvas_rect.bottom() - TOOLBAR_H - 10.0);
        let galley = ui.painter().layout_no_wrap(text, FontId::proportional(11.5), Color32::WHITE);
        let r = Rect::from_min_size(pos2(at.x, at.y - galley.size().y - 8.0), galley.size() + vec2(14.0, 8.0));
        ui.painter().rect_filled(r, 4.0, Color32::from_rgba_unmultiplied(20, 24, 28, 210));
        ui.painter().galley(r.min + vec2(7.0, 4.0), galley, Color32::WHITE);
        crate::widgets::register(ui, "map:withoutGps", r);
    }

    // 5. Top Location Filter Bar & Breadcrumb
    if map_ui.show_filter_bar {
        render_top_filter_bar(app, ui, canvas_rect, &mut map_ui, center);
    }

    // 6. Bottom Map Toolbar (Style dropdown, Fit, Zoom slider, Lock button, Scale & Attribution)
    let fit_now = render_bottom_toolbar(app, ui, canvas_rect, &mut map_ui, &mut zoom);

    // 7. Map Legend Overlay
    if map_ui.legend_open {
        render_legend_overlay(ui, canvas_rect, &mut map_ui);
    }

    // 8. Left Navigator Mini-Map and Saved Locations Panel
    if map_ui.navigator_open {
        render_left_navigator(app, ui, (rect, canvas_rect), (left_w, navigator_h), &mut center, &mut zoom, &mut map_ui);
    }

    // Update persistent state
    map_ui.center_lat = center.lat;
    map_ui.center_lon = center.lon;
    map_ui.zoom = zoom;
    app.ui.map = map_ui;
    if fit_now {
        fit(app);
    }
}

/// The pointer on the map canvas: a drag that starts on a single photo's pin moves it (unless
/// markers are locked) and lands it where it is let go (outside `zone`: nothing changes); any
/// other drag (the empty map, a cluster) pans; the wheel zooms around the pointer and a
/// double-click on the empty map zooms in. Returns the view after them.
fn navigate(
    app: &mut LightcraftApp,
    ui: &egui::Ui,
    canvas_response: &egui::Response,
    mut vp: Viewport,
    (canvas_rect, zone, max_zoom): (Rect, Rect, f64),
    map_ui: &mut crate::state::MapUi,
) -> Viewport {
    let (w, h) = (vp.width, vp.height);
    let local = |p: Pos2| Point::new(f64::from(p.x - canvas_rect.min.x), f64::from(p.y - canvas_rect.min.y));
    let screen = |p: Point| pos2(canvas_rect.min.x + p.x as f32, canvas_rect.min.y + p.y as f32);
    let carrying = app.ui.dragging_photos.is_some();
    // a drag that starts on a single photo's pin moves it (unless markers are locked); any other
    // drag (the empty map, a cluster) pans
    let before = lightcraft_engine::map::cluster::cluster(&app.map_points.points, &vp, CLUSTER_CELL);
    if canvas_response.drag_started() {
        map_ui.pin_drag = None;
        if !map_ui.locked
            && !carrying
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
            && let Some(c) = pin_at(&before, local(origin))
            && let [id] = c.ids[..]
        {
            let at = screen(c.center);
            map_ui.pin_drag = Some(PinDrag { id, grab: at - origin, at });
        }
    } else if !canvas_response.dragged() && !canvas_response.drag_stopped() {
        map_ui.pin_drag = None;
    }
    let pointer = ui.input(|i| i.pointer.latest_pos());
    if let Some(d) = map_ui.pin_drag.as_mut()
        && let Some(p) = pointer
    {
        d.at = p + d.grab;
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }

    // Pan with mouse drag
    if canvas_response.dragged() && !canvas_response.drag_started() && map_ui.pin_drag.is_none() {
        let d = canvas_response.drag_delta();
        vp = vp.panned(f64::from(d.x), f64::from(d.y));
    }

    // Scroll wheel zoom anchored to cursor; a double-click on the empty map zooms in
    let hovered_before = pointer.filter(|p| canvas_rect.contains(*p)).and_then(|p| pin_at(&before, local(p)));
    if canvas_response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let delta_zoom = if canvas_response.double_clicked() && hovered_before.is_none() {
            1.0
        } else if scroll.abs() > 0.5 {
            f64::from(scroll) * 0.005
        } else {
            0.0
        };
        if delta_zoom != 0.0 {
            let new_zoom = (vp.zoom + delta_zoom).clamp(MIN_ZOOM, max_zoom);
            let anchor = pointer.filter(|p| canvas_rect.contains(*p)).map_or(Point::new(w / 2.0, h / 2.0), local);
            vp = vp.zoomed_around(anchor, new_zoom);
        }
    }

    // a moved pin lands where it is let go (outside the map: nothing changes)
    if canvas_response.drag_stopped()
        && let Some(d) = map_ui.pin_drag.take()
        && pointer.is_some_and(|p| zone.contains(p))
    {
        let to = vp.to_lat_lon(local(d.at));
        if let Err(e) = app.run("photo.setGps", json!({"ids": [d.id], "lat": to.lat, "lon": to.lon})) {
            app.toast(ui.ctx(), e);
        }
        refresh_points(app);
    }

    vp
}

/// Height of the Info panel's map of a photo's place.
pub const MINI_HEIGHT: f32 = 160.0;
/// Zoom of the Info panel's map (or the source's max when lower).
pub const MINI_ZOOM: f64 = 14.0;

/// What the Info panel's map drew this frame (`map.state` → `miniMap`).
#[derive(Clone, Debug)]
pub struct MiniShown {
    pub photo: u64,
    pub center: LatLon,
    pub zoom: f64,
    pub rect: Rect,
    pub pin: Pos2,
    pub tiles: tiles::Drawn,
}

/// The Info panel's map of `photo`'s place `pos`: `width` × [`MINI_HEIGHT`], zoom 14 (or the
/// source's max), the photo's pin and the tile credit, from the same tile service and textures as
/// the Map view. It asks for only the tiles it shows (no ring), and only while it is on screen:
/// scrolled out of view, or with the Info panel closed, nothing is fetched for it. It doesn't pan;
/// a click opens the Map view centred there. Offline it stays grey with the pin and "Map tiles are
/// off". Ported in spirit from PR #804 (Jason Baldwin).
pub fn mini_map(app: &mut LightcraftApp, ui: &mut egui::Ui, photo: PhotoId, pos: LatLon, width: f32) {
    let t = Tokens::get(ui.ctx());
    let w = if width.is_finite() { width.clamp(0.0, ui.available_width().max(0.0)) } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, MINI_HEIGHT), Sense::click());
    if w < 1.0 || !ui.is_rect_visible(rect) {
        return;
    }
    crate::widgets::register(ui, "info:miniMap", rect);
    let zoom = MINI_ZOOM.min(f64::from(app.session.map.effective_source().max_zoom()));
    let vp = Viewport::new(pos, zoom, f64::from(w), f64::from(MINI_HEIGHT));
    let painter = ui.painter_at(rect);
    let drawn = tiles::draw(&mut app.session, &painter, rect, vp.center, zoom, 0);
    // the pin (near the poles the map stops short of the photo's place: keep it on the map)
    let p = vp.to_screen(pos);
    let pin = keep_on_map(pos2(rect.min.x + p.x as f32, rect.min.y + p.y as f32), rect);
    render_single_pin(&painter, pin, true, false);
    // the tile provider's credit, always visible (tile licences require it)
    let credit = app.session.map.attribution();
    if !credit.is_empty() {
        let g = painter.layout(credit, FontId::proportional(9.5), Color32::from_gray(40), (w - 8.0).max(20.0));
        let r = Rect::from_min_size(pos2(rect.right() - g.size().x - 6.0, rect.bottom() - g.size().y - 4.0), g.size() + vec2(6.0, 4.0));
        painter.rect_filled(r, 2.0, Color32::from_white_alpha(200));
        painter.galley(r.min + vec2(3.0, 2.0), g, Color32::from_gray(40));
        crate::widgets::register(ui, "info:miniMapAttribution", r);
    }
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, t.button_border), StrokeKind::Inside);
    app.mini_map = Some(MiniShown { photo: photo.0, center: vp.center, zoom, rect, pin, tiles: drawn });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.on_hover_text(crate::i18n::tr("Show in the Map view")).clicked() {
        let shown = app.run("view.map", json!({}));
        let centred = app.run("map.setView", json!({"lat": pos.lat, "lon": pos.lon, "zoom": zoom}));
        if let Err(e) = shown.and(centred) {
            app.toast(ui.ctx(), e);
        }
    }
}

/// `pin` kept 8 points inside `rect` (the centre of a map narrower than that, or of a non-finite
/// point). From PR #804's review fix: `f32::clamp` panics when min > max.
pub(crate) fn keep_on_map(pin: Pos2, rect: Rect) -> Pos2 {
    if !(pin.x.is_finite() && pin.y.is_finite()) {
        return rect.center();
    }
    let lo = (rect.min + vec2(8.0, 8.0)).min(rect.center());
    let hi = (rect.max - vec2(8.0, 8.0)).max(rect.center());
    pin.clamp(lo, hi)
}

/// Where photos or a pin may be dropped: the map, less its filter bar and bottom toolbar.
fn drop_zone(canvas: Rect, filter_bar: bool) -> Rect {
    let top = if filter_bar { canvas.top() + FILTER_BAR_H } else { canvas.top() };
    Rect::from_min_max(pos2(canvas.left(), top), pos2(canvas.right(), (canvas.bottom() - TOOLBAR_H).max(top)))
}

/// The pin or cluster under canvas point `p` (nearest first), as a click finds it.
fn pin_at(clusters: &[Cluster], p: Point) -> Option<&Cluster> {
    lightcraft_engine::map::cluster::hit(clusters, p, |_| PIN_HIT).and_then(|i| clusters.get(i))
}

/// What the pointer did to the pins this frame.
struct PinInput {
    /// A pin's photos (a cluster: all of them) and the click's modifiers.
    clicked: Option<(Vec<PhotoId>, egui::Modifiers)>,
    double_clicked_photo: Option<PhotoId>,
    /// A double-clicked cluster: zoom in on it.
    zoom_target: Option<(LatLon, f64)>,
    /// The photos of the pin under the pointer.
    hovered: Option<Vec<u64>>,
}

/// Draw the pins and clusters (selected ones highlighted, a cluster with its selected count),
/// record them for `map.state` and read their clicks.
fn draw_pins(
    app: &mut LightcraftApp,
    ui: &egui::Ui,
    painter: &Painter,
    clusters: &[Cluster],
    (canvas, pointer): (Rect, Option<Pos2>),
    map_ui: &crate::state::MapUi,
) -> PinInput {
    let mut input = PinInput { clicked: None, double_clicked_photo: None, zoom_target: None, hovered: None };
    // while photos are carried over the map, or a pin is moved, pins are not hovered or clicked
    let busy = app.ui.dragging_photos.is_some() || map_ui.pin_drag.is_some();
    let selected: HashSet<u64> = app.session.selection.ids.iter().map(|p| p.0).collect();
    let moving = map_ui.pin_drag.map(|d| d.id);
    let to_screen = |c: &Cluster| pos2(canvas.min.x + c.center.x as f32, canvas.min.y + c.center.y as f32);
    let local = pointer.filter(|p| canvas.contains(*p)).map(|p| Point::new(f64::from(p.x - canvas.min.x), f64::from(p.y - canvas.min.y)));
    let hovered = if busy { None } else { local.and_then(|p| lightcraft_engine::map::cluster::hit(clusters, p, |_| PIN_HIT)) };
    for (i, c) in clusters.iter().enumerate() {
        let at = to_screen(c);
        let n_selected = c.ids.iter().filter(|id| selected.contains(id)).count();
        app.map_drawn.pins.push(DrawnPin { ids: c.ids.clone(), selected: n_selected, at, pos: c.pos });
        if let Some(first) = c.ids.first() {
            crate::widgets::register(ui, format!("map:pin:{first}"), Rect::from_center_size(at, vec2(32.0, 32.0)));
        }
        let is_hovered = hovered == Some(i);
        if let [id] = c.ids[..] {
            if moving == Some(id) {
                // the pin being moved: a faint mark where it was
                painter.circle_stroke(pos2(at.x, at.y - 12.0), 7.0, Stroke::new(1.5, Color32::from_white_alpha(120)));
                continue;
            }
            render_single_pin(painter, at, n_selected > 0, is_hovered);
        } else {
            render_cluster_badge(painter, at, c.len(), n_selected, is_hovered);
        }
        if !is_hovered {
            continue;
        }
        input.hovered = Some(c.ids.clone());
        let movable = c.len() == 1 && !map_ui.locked;
        ui.ctx().set_cursor_icon(if movable { egui::CursorIcon::Grab } else { egui::CursorIcon::PointingHand });
        if ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
            match c.ids[..] {
                [id] => input.double_clicked_photo = Some(PhotoId(id)),
                _ => input.zoom_target = Some((c.pos, app.ui.map.zoom.round() + 2.0)),
            }
        }
        if ui.input(|i| i.pointer.primary_clicked()) {
            input.clicked = Some((c.ids.iter().map(|id| PhotoId(*id)).collect(), ui.input(|i| i.modifiers)));
        }
    }
    input
}

/// A right-click on a pin opens its photos' menu: Remove GPS Coordinates (`photo.removeGps`).
/// A right-click on the empty map opens nothing.
fn pin_menu(app: &mut LightcraftApp, resp: &egui::Response, clusters: &[Cluster], local: impl Fn(Pos2) -> Point, map_ui: &mut crate::state::MapUi) {
    if resp.secondary_clicked() {
        map_ui.menu_for = resp.interact_pointer_pos().map(&local).and_then(|p| pin_at(clusters, p)).map(|c| c.ids.clone()).unwrap_or_default();
    }
    if map_ui.menu_for.is_empty() {
        return;
    }
    let ids = map_ui.menu_for.clone();
    let mut done = false;
    let open = resp.context_menu(|ui| {
        let label = if ids.len() == 1 { "Remove GPS Coordinates" } else { "Remove GPS Coordinates of These Photos" };
        let b = ui.button(crate::i18n::tr(label));
        crate::widgets::register(ui, "map:pinMenu:removeGps", b.rect);
        if b.clicked() {
            if let Err(e) = app.run("photo.removeGps", json!({"ids": ids})) {
                app.toast(ui.ctx(), e);
            }
            done = true;
            ui.close();
        }
    });
    if done || open.is_none() {
        map_ui.menu_for.clear();
    }
}

/// The hovered pin's first photo as a thumbnail card above it (with the pin's photo count).
fn hover_thumbnail(app: &mut LightcraftApp, ui: &egui::Ui, t: &Tokens, at: Pos2, ids: &[u64]) {
    let Some(&first) = ids.first() else { return };
    crate::panels::grid::request_thumb(app, PhotoId(first), 256, 6);
    let card = Rect::from_center_size(pos2(at.x, at.y - 34.0 - 60.0), vec2(124.0, 96.0));
    let p = ui.painter();
    p.rect_filled(card, 6.0, Color32::from_rgba_unmultiplied(24, 28, 34, 240));
    p.rect_stroke(card, 6.0, Stroke::new(1.0, Color32::from_white_alpha(80)), StrokeKind::Inside);
    let area = card.shrink(6.0);
    match app.renderer.thumb(PhotoId(first)) {
        Some(tex) => {
            let [tw, th] = tex.size;
            let s = (area.width() / tw.max(1) as f32).min(area.height() / th.max(1) as f32);
            let r = Rect::from_center_size(area.center(), vec2(tw as f32 * s, th as f32 * s));
            p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            p.rect_filled(area, 2.0, t.skeleton);
        }
    }
    if ids.len() > 1 {
        let text = ids.len().to_string();
        let b = Rect::from_min_size(pos2(card.right() - 30.0, card.top() + 4.0), vec2(26.0, 16.0));
        p.rect_filled(b, 8.0, SELECTED_BUBBLE);
        p.text(b.center(), Align2::CENTER_CENTER, text, FontId::proportional(10.0), Color32::WHITE);
    }
    crate::widgets::register(ui, "map:hoverThumb", card);
}

/// A click on a pin selects its photos (a cluster: all of them, the first active), so the
/// filmstrip shows them selected and scrolls to the active one; Cmd/Ctrl adds them instead.
fn select_pin(app: &mut LightcraftApp, ui: &egui::Ui, ids: &[PhotoId], m: egui::Modifiers, map_ui: &mut crate::state::MapUi) {
    let Some(&first) = ids.first() else { return };
    map_ui.active_pin_photo = Some(first);
    let mode = if m.command { "add" } else { "replace" };
    let raw: Vec<u64> = ids.iter().map(|p| p.0).collect();
    if let Err(e) = app.run("library.select", json!({"ids": raw, "active": first.0, "mode": mode})) {
        app.toast(ui.ctx(), e);
    }
}

fn render_single_pin(p: &Painter, pos: Pos2, selected: bool, hovered: bool) {
    let pin_color = if selected {
        Color32::from_rgb(255, 204, 0) // Yellow for selected
    } else {
        Color32::from_rgb(255, 115, 0) // Orange for unselected
    };
    let border_color = if hovered { Color32::WHITE } else { Color32::from_black_alpha(180) };

    // Pin head circle
    let head_center = pos2(pos.x, pos.y - 12.0);
    p.circle_filled(head_center, 7.0, pin_color);
    p.circle_stroke(head_center, 7.0, Stroke::new(1.5, border_color));

    // Center dot
    p.circle_filled(head_center, 2.5, Color32::WHITE);

    // Pin point
    p.line_segment([pos2(pos.x, pos.y - 5.0), pos], Stroke::new(2.0, pin_color));
}

/// A cluster's badge: its photo count, yellow when some of them are selected, and then a small
/// bubble at its top right with how many are selected.
fn render_cluster_badge(p: &Painter, pos: Pos2, count: usize, selected_count: usize, hovered: bool) {
    let selected = selected_count > 0;
    let badge_color = if selected { Color32::from_rgb(255, 204, 0) } else { Color32::from_rgb(240, 100, 20) };
    let text_color = if selected { Color32::BLACK } else { Color32::WHITE };
    let border_stroke = Stroke::new(if hovered { 2.0 } else { 1.2 }, if hovered { Color32::WHITE } else { Color32::from_black_alpha(160) });

    let label = count.to_string();
    let badge_w = (label.len() as f32 * 8.0 + 16.0).max(28.0);
    let badge_rect = Rect::from_center_size(pos, vec2(badge_w, 22.0));

    p.rect_filled(badge_rect, 11.0, badge_color);
    p.rect_stroke(badge_rect, 11.0, border_stroke, StrokeKind::Inside);
    p.text(pos, Align2::CENTER_CENTER, label, FontId::proportional(12.0), text_color);
    if selected {
        let text = selected_count.to_string();
        let w = (text.len() as f32 * 6.5 + 8.0).max(16.0);
        let bubble = Rect::from_center_size(pos2(badge_rect.right() - 2.0, badge_rect.top() - 2.0), vec2(w, 16.0));
        p.rect_filled(bubble, 8.0, SELECTED_BUBBLE);
        p.rect_stroke(bubble, 8.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
        p.text(bubble.center(), Align2::CENTER_CENTER, text, FontId::proportional(10.0), Color32::WHITE);
    }
}

/// The selected-count bubble on a cluster (dark, so it reads on the yellow badge and the map).
const SELECTED_BUBBLE: Color32 = Color32::from_rgb(30, 110, 230);

/// Photos carried from the filmstrip over the map (`zone`: not over its filter bar or toolbar):
/// returns how many, the pointer and where they would land (`place`: the latitude stays inside
/// Web Mercator's ±85.05°, the longitude wraps), and on the release frame sets their position
/// there (`photo.setGps`, one undo step, overwriting a position they had).
fn drop_photos(app: &mut LightcraftApp, ui: &egui::Ui, zone: Rect, place: impl Fn(Pos2) -> LatLon) -> Option<(usize, Pos2, LatLon)> {
    let ids = app.ui.dragging_photos.clone()?;
    let at = ui.input(|i| i.pointer.latest_pos()).filter(|p| zone.contains(*p))?;
    let to = place(at);
    if ui.input(|i| i.pointer.primary_released()) {
        app.ui.dragging_photos = None;
        if let Err(e) = app.run("photo.setGps", json!({"ids": ids, "lat": to.lat, "lon": to.lon})) {
            app.toast(ui.ctx(), e);
        }
        return None;
    }
    let n = ids.len();
    let hint = if n == 1 {
        crate::i18n::tr("Set the location of this photo").to_string()
    } else {
        crate::i18n::tr_format!("Set the location of {n} photos", n = n)
    };
    crate::panels::grid::drag_hint(ui.ctx(), hint);
    Some((n, at, to))
}

/// Where carried photos would land: a pin at the point with the photo count, and a ring.
fn drop_indicator(p: &Painter, t: &Tokens, at: Pos2, n: usize) {
    p.circle_stroke(at, 20.0, Stroke::new(2.0, t.accent.gamma_multiply(0.8)));
    render_single_pin(p, at, true, true);
    if n > 1 {
        let text = if n > 9999 { format!("{}k", n / 1000) } else { n.to_string() };
        let w = (text.len() as f32 * 6.5 + 10.0).max(18.0);
        let bubble = Rect::from_center_size(pos2(at.x + 12.0, at.y - 22.0), vec2(w, 16.0));
        p.rect_filled(bubble, 8.0, t.accent);
        p.text(bubble.center(), Align2::CENTER_CENTER, text, FontId::proportional(10.5), Color32::WHITE);
    }
}

fn render_top_filter_bar(_app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, map_ui: &mut crate::state::MapUi, center: LatLon) {
    let t = Tokens::get(ui.ctx());
    let bar_rect = Rect::from_min_size(pos2(canvas.left() + 16.0, canvas.top() + 14.0), vec2(canvas.width() - 32.0, 36.0));

    let bar_bg = Color32::from_rgba_unmultiplied(20, 24, 28, 220);
    ui.painter().rect_filled(bar_rect, 6.0, bar_bg);
    ui.painter().rect_stroke(bar_rect, 6.0, Stroke::new(1.0, t.button_border), StrokeKind::Inside);

    let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect));
    bar_ui.horizontal_centered(|ui| {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Location Filter:")).strong().color(Color32::WHITE));
        ui.add_space(8.0);

        let filters = [
            (LocationFilter::VisibleOnMap, "Visible On Map"),
            (LocationFilter::Tagged, "Tagged"),
            (LocationFilter::Untagged, "Untagged"),
            (LocationFilter::None, "None"),
        ];

        for (f, name) in filters {
            let active = map_ui.filter == f;
            let btn = ui.selectable_label(active, crate::i18n::tr(name));
            if btn.clicked() {
                map_ui.filter = f;
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            ui.label(egui::RichText::new(format!("{:.4}°, {:.4}°", center.lat, center.lon)).small().color(Color32::LIGHT_GRAY));
        });
    });
}

/// The bottom toolbar; returns whether Fit to Photos was clicked.
fn render_bottom_toolbar(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, map_ui: &mut crate::state::MapUi, zoom: &mut f64) -> bool {
    let mut fit_clicked = false;
    let t = Tokens::get(ui.ctx());
    let bar_h = 36.0;
    let bar_rect = Rect::from_min_max(pos2(canvas.left(), canvas.bottom() - bar_h), canvas.max);

    ui.painter().rect_filled(bar_rect, 0.0, Color32::from_rgba_unmultiplied(16, 20, 24, 235));
    ui.painter().line_segment([pos2(bar_rect.left(), bar_rect.top()), pos2(bar_rect.right(), bar_rect.top())], Stroke::new(1.0, t.button_border));

    let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect));
    bar_ui.horizontal_centered(|ui| {
        ui.add_space(12.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Map Style:")).small().color(Color32::LIGHT_GRAY));

        let current = app.session.map.effective_source();
        let max_zoom = f64::from(current.max_zoom());
        let mut chosen = None;
        crate::widgets::combo_box("map_style_combo").selected_text(crate::i18n::tr(current.label())).show_ui(ui, |ui| {
            // only the sources usable now: the Esri layers are not offered (docs/map.md)
            for p in app.session.map.offered() {
                if ui.selectable_label(p == current, crate::i18n::tr(p.label())).clicked() {
                    chosen = Some(p.id().to_string());
                }
            }
        });
        if let Some(id) = chosen
            && let Err(e) = app.run("map.tileSource", json!({"id": id}))
        {
            app.ui.toast = Some((e, app.tick_now() + 6.0, None));
        }

        ui.add_space(12.0);
        let fit = ui.button(crate::i18n::tr("Fit to Photos"));
        crate::widgets::register(ui, "map:fit", fit.rect);
        fit_clicked = fit.clicked();

        ui.add_space(12.0);
        if ui.button("-").clicked() {
            *zoom = (*zoom - 1.0).max(1.0);
        }
        ui.add(egui::Slider::new(zoom, 1.0..=max_zoom).show_value(false));
        if ui.button("+").clicked() {
            *zoom = (*zoom + 1.0).min(max_zoom);
        }

        ui.add_space(12.0);
        let lock_btn = ui.selectable_label(map_ui.locked, crate::i18n::tr("Lock Markers"));
        crate::widgets::register(ui, "map:lockMarkers", lock_btn.rect);
        if lock_btn.on_hover_text(crate::i18n::tr("Pins can't be dragged to move photos")).clicked() {
            map_ui.locked = !map_ui.locked;
        }

        // the tile provider's credit, always visible at the bottom right (tile licences require it)
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(12.0);
            // the source's credit; an Esri layer's comes from its metadata once fetched
            let credit = ui.label(egui::RichText::new(app.session.map.attribution()).small().color(Color32::from_white_alpha(170)));
            crate::widgets::register(ui, "map:attribution", credit.rect);
        });
    });
    fit_clicked
}

fn render_legend_overlay(ui: &mut egui::Ui, canvas: Rect, _map_ui: &mut crate::state::MapUi) {
    let legend_rect = Rect::from_min_size(pos2(canvas.right() - 210.0, canvas.bottom() - 170.0), vec2(196.0, 124.0));
    let p = ui.painter();

    p.rect_filled(legend_rect, 6.0, Color32::from_rgba_unmultiplied(20, 24, 28, 230));
    p.rect_stroke(legend_rect, 6.0, Stroke::new(1.0, Color32::from_white_alpha(40)), StrokeKind::Inside);

    let items = [
        (Color32::from_rgb(255, 115, 0), "Unselected photo"),
        (Color32::from_rgb(255, 204, 0), "Selected photo"),
        (Color32::from_rgb(240, 100, 20), "Group / cluster"),
    ];

    for (i, (color, text)) in items.iter().enumerate() {
        let y = legend_rect.top() + 20.0 + i as f32 * 28.0;
        p.circle_filled(pos2(legend_rect.left() + 20.0, y), 5.0, *color);
        p.text(pos2(legend_rect.left() + 34.0, y), Align2::LEFT_CENTER, crate::i18n::tr(text), FontId::proportional(11.0), Color32::WHITE);
    }
}

fn render_loupe_popover(app: &mut LightcraftApp, ui: &mut egui::Ui, pin_pos: Pos2, photo_id: PhotoId, map_ui: &mut crate::state::MapUi) {
    let card_w = 230.0;
    let card_h = 100.0;
    let card_rect = Rect::from_min_size(pos2(pin_pos.x - card_w / 2.0, pin_pos.y - card_h - 18.0), vec2(card_w, card_h));

    let p = ui.painter();
    p.rect_filled(card_rect, 6.0, Color32::from_rgba_unmultiplied(24, 28, 34, 245));
    p.rect_stroke(card_rect, 6.0, Stroke::new(1.2, Color32::from_white_alpha(70)), StrokeKind::Inside);

    let Some(ph) = app.session.catalog.photo(photo_id) else { return };
    let mut card_ui = ui.new_child(egui::UiBuilder::new().max_rect(card_rect));

    card_ui.vertical(|ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(&ph.file_name).strong().color(Color32::WHITE));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.button("X").clicked() {
                    map_ui.active_pin_photo = None;
                }
            });
        });

        ui.add_space(4.0);
        if let Some(ref cap) = ph.captured {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(cap).small().color(Color32::LIGHT_GRAY));
            });
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button(crate::i18n::tr("Open in Detail")).clicked() {
                app.ui.view = ViewMode::Detail;
            }
        });
    });
}

fn zoom_from_radius(radius_meters: f64) -> f64 {
    if radius_meters <= 300.0 {
        16.5
    } else if radius_meters <= 800.0 {
        15.5
    } else if radius_meters <= 2000.0 {
        14.0
    } else if radius_meters <= 5000.0 {
        12.5
    } else if radius_meters <= 15000.0 {
        11.0
    } else if radius_meters <= 50000.0 {
        9.5
    } else {
        8.0
    }
}

fn render_left_navigator(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    rects: (Rect, Rect),
    nav_dims: (f32, f32),
    center: &mut LatLon,
    zoom: &mut f64,
    map_ui: &mut crate::state::MapUi,
) {
    let (rect, canvas_rect) = rects;
    let (left_w, nav_h) = nav_dims;
    let t = Tokens::get(ui.ctx());
    let left_rect = Rect::from_min_size(rect.min, vec2(left_w, rect.height()));

    ui.painter().rect_filled(left_rect, 0.0, Color32::from_rgb(26, 30, 36));
    ui.painter()
        .line_segment([pos2(left_rect.right(), left_rect.top()), pos2(left_rect.right(), left_rect.bottom())], Stroke::new(1.0, t.button_border));

    let mut panel_ui = ui.new_child(egui::UiBuilder::new().max_rect(left_rect));
    panel_ui.vertical(|ui| {
        // Navigator mini-map header
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Navigator")).strong().color(Color32::WHITE));
        });

        ui.add_space(4.0);
        let mini_w = left_w - 16.0;
        let mini_rect = Rect::from_min_size(pos2(left_rect.left() + 8.0, ui.cursor().top()), vec2(mini_w, nav_h));
        let nav_resp = ui.allocate_rect(mini_rect, Sense::click_and_drag());

        let nav_max = f64::from(app.session.map.effective_source().max_zoom().saturating_sub(2).max(1));
        let nav_zoom = map_ui.navigator_zoom.clamp(1.0, nav_max);

        if nav_resp.dragged() {
            let delta = nav_resp.drag_delta();
            let delta_tx = delta.x as f64 / TILE_SIZE as f64;
            let delta_ty = delta.y as f64 / TILE_SIZE as f64;
            let n = 2.0_f64.powf(nav_zoom.clamp(0.0, 24.0));
            let (cur_tx, cur_ty) = center.to_tile_coords(nav_zoom);
            let new_tx = (cur_tx + delta_tx).rem_euclid(n);
            let new_ty = (cur_ty + delta_ty).clamp(0.0, n);
            *center = LatLon::from_tile_coords(new_tx, new_ty, nav_zoom);
            ui.ctx().request_repaint();
        } else if nav_resp.clicked()
            && let Some(pos) = nav_resp.interact_pointer_pos()
        {
            let offset_x = pos.x - mini_rect.center().x;
            let offset_y = pos.y - mini_rect.center().y;
            let delta_tx = offset_x as f64 / TILE_SIZE as f64;
            let delta_ty = offset_y as f64 / TILE_SIZE as f64;
            let n = 2.0_f64.powf(nav_zoom.clamp(0.0, 24.0));
            let (cur_tx, cur_ty) = center.to_tile_coords(nav_zoom);
            let new_tx = (cur_tx + delta_tx).rem_euclid(n);
            let new_ty = (cur_ty + delta_ty).clamp(0.0, n);
            *center = LatLon::from_tile_coords(new_tx, new_ty, nav_zoom);
            ui.ctx().request_repaint();
        }

        if nav_resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.5 {
                map_ui.navigator_zoom = (map_ui.navigator_zoom + (scroll as f64 * 0.005)).clamp(1.0, nav_max);
                ui.ctx().request_repaint();
            }
        }

        let nav_painter = ui.painter().with_clip_rect(mini_rect);
        nav_painter.rect_filled(mini_rect, 4.0, Color32::from_rgb(18, 22, 26));

        // Render overview map tiles (only what it shows: no ring)
        tiles::draw(&mut app.session, &nav_painter, mini_rect, *center, nav_zoom, 0);

        // Viewport rectangle showing the visible area on the main map
        let scale_diff = 2.0_f64.powf(nav_zoom - *zoom);
        let box_w = (canvas_rect.width() as f64 * scale_diff) as f32;
        let box_h = (canvas_rect.height() as f64 * scale_diff) as f32;
        let box_w_clamped = box_w.clamp(6.0, mini_rect.width() * 1.5);
        let box_h_clamped = box_h.clamp(6.0, mini_rect.height() * 1.5);
        let view_rect = Rect::from_center_size(mini_rect.center(), vec2(box_w_clamped, box_h_clamped));

        nav_painter.rect_filled(view_rect, 1.0, Color32::from_white_alpha(35));
        nav_painter.rect_stroke(view_rect, 1.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Middle);
        nav_painter.circle_filled(mini_rect.center(), 2.0, Color32::from_rgb(255, 204, 0));

        // Frame
        ui.painter().rect_stroke(mini_rect, 4.0, Stroke::new(1.0, t.button_border), StrokeKind::Inside);

        // Saved Locations header
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Saved Locations")).strong().color(Color32::WHITE));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.button("+").on_hover_text(crate::i18n::tr("Save current map location")).clicked() {
                    let _ = app.run(
                        "map.savedLocations.create",
                        json!({
                            "name": format!("Location {}", app.session.saved_locations.len() + 1),
                            "lat": center.lat,
                            "lon": center.lon,
                            "radius": 1500.0,
                        }),
                    );
                }
            });
        });

        ui.add_space(8.0);
        let mut delete_id = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            if app.session.saved_locations.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new(crate::i18n::tr("No saved locations. Click + to save.")).small().italics().color(Color32::GRAY));
                });
            } else {
                for loc in &app.session.saved_locations {
                    let is_active = (loc.lat - center.lat).abs() < 0.0001 && (loc.lon - center.lon).abs() < 0.0001;
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        let resp = ui.selectable_label(is_active, &loc.name);
                        if resp.clicked()
                            && let Some(target) = LatLon::new(loc.lat, loc.lon)
                        {
                            *center = target;
                            *zoom = zoom_from_radius(loc.radius);
                            ui.ctx().request_repaint();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(8.0);
                            if ui.small_button("✕").on_hover_text(crate::i18n::tr("Delete location")).clicked() {
                                delete_id = Some(loc.id.clone());
                            }
                        });
                    });
                }
            }
        });
        if let Some(id) = delete_id {
            let _ = app.run("map.savedLocations.delete", json!({"id": id}));
            ui.ctx().request_repaint();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use crate::headless::HeadlessView;

    #[test]
    fn headless_map_view_renders_cleanly() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;

        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert_eq!(app.ui.view, ViewMode::Map);
        assert_eq!(app.session.map.effective_source(), lightcraft_engine::map::TileSource::Osm);
    }

    #[test]
    fn continuous_zoom_and_pan_preserves_coordinates() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;
        app.ui.map.center_lat = 45.57;
        app.ui.map.center_lon = -73.2;
        app.ui.map.zoom = 10.35;

        let mut view = HeadlessView::new();
        let mut raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        raw.events.push(egui::Event::PointerMoved(pos2(500.0, 350.0)));
        raw.events.push(egui::Event::PointerButton {
            pos: pos2(500.0, 350.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        raw.events.push(egui::Event::PointerMoved(pos2(550.0, 370.0)));

        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert!(app.ui.map.center_lat.is_finite());
        assert!(app.ui.map.center_lon.is_finite());
        assert!((app.ui.map.center_lat - 45.57).abs() < 1.0);
        assert!((app.ui.map.center_lon - (-73.2)).abs() < 1.0);
    }

    #[test]
    fn saved_location_selection_updates_map_center() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;
        app.ui.map.center_lat = 0.0;
        app.ui.map.center_lon = 0.0;
        app.ui.map.zoom = 5.0;

        let _ = app.run(
            "map.savedLocations.create",
            json!({
                "name": "Beloeil",
                "lat": 45.57,
                "lon": -73.2,
                "radius": 1500.0,
            }),
        );

        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert_eq!(app.session.saved_locations.len(), 1);
        let loc = &app.session.saved_locations[0];
        app.ui.map.center_lat = loc.lat;
        app.ui.map.center_lon = loc.lon;
        app.ui.map.zoom = zoom_from_radius(loc.radius);

        assert_eq!(app.ui.map.center_lat, 45.57);
        assert_eq!(app.ui.map.center_lon, -73.2);
        assert_eq!(app.ui.map.zoom, 14.0);
    }

    #[test]
    fn navigator_zoom_is_independent_from_map_zoom() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;
        app.ui.map.zoom = 14.0;
        app.ui.map.navigator_zoom = 3.0;

        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert_eq!(app.ui.map.zoom, 14.0);
        assert_eq!(app.ui.map.navigator_zoom, 3.0);
    }

    /// A transport that serves a flat 256 × 256 PNG for every tile and records the URLs (never
    /// the internet).
    struct FakeTiles {
        urls: std::sync::Mutex<Vec<String>>,
        png: Vec<u8>,
    }

    impl lightcraft_engine::map::Transport for FakeTiles {
        fn get(
            &self,
            url: &str,
            _headers: &[(&'static str, String)],
            _max: usize,
            _cancel: &std::sync::atomic::AtomicBool,
        ) -> Result<lightcraft_engine::map::HttpResponse, String> {
            self.urls.lock().unwrap().push(url.to_string());
            Ok(lightcraft_engine::map::HttpResponse { status: 200, headers: vec![], body: self.png.clone() })
        }
    }

    fn fake_tiles() -> std::sync::Arc<FakeTiles> {
        use lightcraft_codecs::{EncodeImage, EncodeMeta, Samples, encode_png};
        let data: Vec<u8> = (0..256 * 256).flat_map(|_| [10u8, 120, 200, 255]).collect();
        let png = encode_png(&EncodeImage::new(256, 256, 4, Samples::U8(&data)), &EncodeMeta::default()).unwrap();
        std::sync::Arc::new(FakeTiles { urls: std::sync::Mutex::new(Vec::new()), png })
    }

    /// A session whose map uses `id` (a custom source, so tests don't share textures) served by
    /// the fake transport.
    fn session_with_fake_tiles(id: &str, fake: std::sync::Arc<FakeTiles>) -> lightcraft_engine::Session {
        use lightcraft_engine::map::{TileService, TileSource};
        let mut session = lightcraft_engine::Session::new();
        let source = TileSource::custom(Some(id), "https://tiles.test/{z}/{x}/{y}.png", Some("© Test tiles"), Some(19)).unwrap();
        session
            .execute(
                "map.tileSource",
                &json!({"id": id, "urlTemplate": "https://tiles.test/{z}/{x}/{y}.png", "attribution": "© Test tiles", "maxZoom": 19}),
            )
            .unwrap();
        session.map.set_service(TileService::with_transport(source, None, Some(fake)).with_activity(Some(session.activity.clone())));
        session
    }

    /// Draws `center` at `zoom` in a 512 × 512 map once and returns what was drawn.
    fn draw_once(session: &mut lightcraft_engine::Session, center: LatLon, zoom: f64) -> tiles::Drawn {
        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(600.0, 600.0), 1.0, 0.0, vec![]);
        let mut drawn = tiles::Drawn::default();
        view.run(raw, |ui| {
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(512.0, 512.0));
            drawn = tiles::draw(session, &ui.painter_at(rect), rect, center, zoom, 1);
        });
        drawn
    }

    fn wait_idle(session: &lightcraft_engine::Session) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !session.map.service().is_some_and(|s| s.is_idle()) {
            assert!(std::time::Instant::now() < end, "the tile service never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Tiles come from the engine's tile service; one not loaded yet is drawn from a loaded
    /// ancestor (scaled) instead of a blank square.
    #[test]
    fn tiles_are_drawn_from_the_service_and_missing_ones_from_an_ancestor() {
        let fake = fake_tiles();
        let mut session = session_with_fake_tiles("ui-parent", fake.clone());
        let center = LatLon::new(48.8566, 2.3522).unwrap();
        let first = draw_once(&mut session, center, 10.0);
        assert!(first.missing > 0 && first.tiles == 0, "{first:?}");
        wait_idle(&session);
        let loaded = draw_once(&mut session, center, 10.0);
        assert!(loaded.tiles > 0 && loaded.missing == 0, "{loaded:?}");
        // two zooms in: none of those tiles is loaded yet, but their zoom-10 ancestors are
        let deeper = draw_once(&mut session, center, 12.0);
        assert!(deeper.from_parent > 0 && deeper.tiles == 0, "{deeper:?}");
        assert!(fake.urls.lock().unwrap().iter().all(|u| u.starts_with("https://tiles.test/")));
    }

    /// The offline source draws a grey map and starts no tile service (no threads, no network).
    #[test]
    fn the_offline_source_starts_no_tile_service() {
        let mut session = lightcraft_engine::Session::new();
        session.execute("map.tileSource", &json!({"offline": true})).unwrap();
        let drawn = draw_once(&mut session, LatLon::new(0.0, 0.0).unwrap(), 3.0);
        assert!(drawn.offline);
        assert!(session.map.service().is_none());
        assert!(session.activity.list().is_empty());
    }

    /// A session the host didn't put online never fetches: the Map view in tests (and any
    /// session without `with_default_map_cache`) stays off the network.
    #[test]
    fn a_session_not_put_online_never_fetches() {
        let mut session = lightcraft_engine::Session::new();
        let drawn = draw_once(&mut session, LatLon::new(51.5, -0.12).unwrap(), 6.0);
        assert!(!drawn.offline && drawn.tiles == 0);
        assert!(session.map.service().is_some_and(|s| s.is_idle() && s.stats().requests == 0));
    }

    /// The Esri layers are not in the Map Style list without an ArcGIS API key (docs/map.md).
    #[test]
    fn the_map_style_list_has_no_esri_layer_without_a_key() {
        let session = lightcraft_engine::Session::new();
        let offered: Vec<String> = session.map.offered().iter().map(|s| s.label().to_string()).collect();
        assert_eq!(offered, vec!["OpenStreetMap", "OpenTopoMap", "Offline (no tiles)"]);
    }
}
