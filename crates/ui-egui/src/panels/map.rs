//! The Map view: the photos in view (current source, filter and sort: the grid's photos) placed by
//! their GPS position on a slippy map, with the filmstrip below.
//!
//! - **Tiles** come from the session's tile service (`lightcraft-map`, see docs/map.md): every
//!   frame each map on screen ([`Surface`]: this view, the Info panel's [`mini_map`]) tells it what
//!   it shows ([`TileService::want`], cheap; the maps drawn in this frame or the last one together,
//!   so neither cancels the other's tiles, and nothing once no map is drawn: [`end_pass`]), draws
//!   the tiles it has in memory as egui textures ([`draw_tiles`]), and polls it for arrivals (a
//!   repaint). Each tile is uploaded once into one cache shared by both maps, kept by [`TileKey`]
//!   ([`MapView::uploads`] counts uploads); the least recently drawn are dropped past
//!   [`MAX_TEXTURES`]. A tile not loaded yet is drawn from a lower zoom's texture when one is at
//!   hand, else as a grey square. The source's attribution is always painted bottom right
//!   ([`draw_attribution`]).
//! - **Pins**: each photo with a valid GPS position is projected once per change of the photo list
//!   ([`MapPoint`]); clusters are recomputed only when the view moves ([`lightcraft_map::cluster`],
//!   world-anchored cells of [`CLUSTER_CELL`] px). A click selects a pin's photos (⌘ / ⇧ adds),
//!   a double-click opens the first in Detail, hovering shows its thumbnail.
//! - **Moving**: drag to pan, mouse wheel / pinch / ⌘-scroll to zoom around the pointer,
//!   two-finger scroll to pan, double-click the map to zoom in; + / − and Fit to Photos at the top
//!   right. Commands: `view.map`, `map.fit`, `map.setView`, `map.zoomIn`, `map.zoomOut`, `map.state`.
//! - **Geotagging**: photos dragged from the filmstrip and dropped on the map get the drop point's
//!   position (a pin follows the pointer while they are over it); a drag that starts on a single
//!   photo's pin moves it (a cluster's drag pans, as Lightroom Classic does), unless Lock Markers
//!   is on; right-click a pin to remove its photos' GPS. Each is one undo step (`photo.setGps`,
//!   `photo.removeGps`); `map.lockMarkers` toggles the lock (a UI preference).
//! - **Location filter** (the bar above the map, as Lightroom Classic's): Visible on Map, Tagged,
//!   Untagged or None, with counts ([`LocationFilter`], `map.locationFilter`). It narrows the
//!   photos in view through [`lightcraft_engine::Session::narrow`] (rules Has GPS / GPS Area, the
//!   ones `library.filter` and smart albums take), so the filmstrip, selection and keys follow it
//!   while the library's own filter stays as it is; leaving the Map view drops it
//!   ([`sync_location_filter`]). Visible on Map follows the view once it rests for
//!   [`AREA_SETTLE_MS`] (commands apply at once); pins come from the photos before it (all those
//!   with a position; none for Untagged), so pins and filmstrip agree.
//!
//! The view (centre and zoom) is kept for the session; the first time the map is shown it fits the
//! photos with GPS.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_geom::Point;
use lightcraft_map::{Area, Bounds, CLUSTER_CELL, Cluster, LatLon, MapPoint, TileEvent, TileKey, Viewport, fit_bounds};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::icons::Icon;

#[path = "map_locations.rs"]
pub mod locations;
#[path = "map_tracks.rs"]
pub mod tracks;
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

/// Tile textures kept (256 × 256 RGBA: 256 KB each).
pub const MAX_TEXTURES: usize = 256;
/// Deepest zoom a fit goes to (a single photo, or photos close together).
const FIT_MAX_ZOOM: f64 = 16.0;
/// Room left around the photos by a fit (points).
const FIT_MARGIN: f64 = 60.0;
/// Deepest zoom without tiles (offline).
const OFFLINE_MAX_ZOOM: f64 = 19.0;
/// Zoom per mouse-wheel line.
const WHEEL_ZOOM: f64 = 0.5;
/// Clusters reported by `map.state` and registered as widgets (the view draws them all).
const MAX_LISTED: usize = 500;

/// How long the view must rest before Visible on Map narrows the filmstrip to it (ms).
pub const AREA_SETTLE_MS: f64 = 250.0;
/// Height of the location filter bar above the map (points).
const FILTER_BAR_H: f32 = 30.0;

/// The Map view's location filter (Lightroom Classic's Location Filter bar).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocationFilter {
    /// No location filter: the photos in view, as the grid.
    #[default]
    None,
    /// The photos whose position is inside the map's view.
    Visible,
    /// The photos with a GPS position.
    Tagged,
    /// The photos without one.
    Untagged,
}

impl LocationFilter {
    pub const ALL: [LocationFilter; 4] = [LocationFilter::Visible, LocationFilter::Tagged, LocationFilter::Untagged, LocationFilter::None];

    /// The command's id for it (`map.locationFilter {mode}`).
    pub fn id(self) -> &'static str {
        match self {
            LocationFilter::None => "none",
            LocationFilter::Visible => "visible",
            LocationFilter::Tagged => "tagged",
            LocationFilter::Untagged => "untagged",
        }
    }

    pub fn parse(s: &str) -> Option<LocationFilter> {
        LocationFilter::ALL.into_iter().find(|m| m.id().eq_ignore_ascii_case(s.trim()))
    }

    /// The bar's label (a message for translation).
    pub fn label(self) -> &'static str {
        match self {
            LocationFilter::None => "None",
            LocationFilter::Visible => "Visible on Map",
            LocationFilter::Tagged => "Tagged",
            LocationFilter::Untagged => "Untagged",
        }
    }
}

/// Zoom of the Info panel's mini-map (streets and their names), or the source's deepest if less.
pub const MINI_ZOOM: f64 = 14.0;
/// Height of the mini-map (points); it takes the panel's width.
pub const MINI_HEIGHT: f32 = 160.0;

/// The Info panel's mini-map as drawn (`map.state` → `miniMap`).
#[derive(Clone, Debug)]
pub struct MiniShown {
    pub photo: PhotoId,
    pub center: LatLon,
    pub zoom: f64,
    /// The map and its pin (screen points).
    pub rect: Rect,
    pub pin: egui::Pos2,
    pub tiles_drawn: usize,
    pub tiles_missing: usize,
    /// The egui pass it was drawn in.
    pass: u64,
}

/// The view's centre and zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub center: LatLon,
    pub zoom: f64,
}

struct TileTex {
    tex: egui::TextureHandle,
    /// The egui pass it was last drawn in.
    used: u64,
    /// The decoded tile it was made from (to tell a fresh copy from the same arrival).
    from: std::sync::Weak<lightcraft_map::TileImage>,
}

/// A single photo's pin being dragged to a new place.
#[derive(Clone, Copy, Debug)]
struct PinDrag {
    id: u64,
    /// The pin's centre minus the pointer (so the pin doesn't jump under it).
    grab: egui::Vec2,
    /// Where the pin is drawn now (screen points).
    at: egui::Pos2,
}

/// The photos of the current list that have a position, projected once.
#[derive(Default)]
struct Points {
    generation: Option<u64>,
    points: Arc<[MapPoint]>,
    without_gps: usize,
    bounds: Option<Bounds>,
}

/// The Map view's state for the session (not saved).
#[derive(Default)]
pub struct MapView {
    /// `None` until the map is first shown (then it fits the photos).
    pub camera: Option<Camera>,
    /// Fit the photos on the next frame (`map.fit` before the canvas size is known).
    fit_pending: bool,
    /// The canvas as drawn last (screen points).
    pub rect: Option<Rect>,
    textures: HashMap<TileKey, TileTex>,
    /// The tile source the textures came from (another source's tiles are dropped).
    textures_source: String,
    /// Tile textures uploaded so far (each arrival once).
    pub uploads: u64,
    /// The tiles each map on screen (by [`Surface`]) last asked for, and the egui pass it drew in.
    wanted: [(u64, Vec<TileKey>); 2],
    /// The tile service was asked for tiles that no map may show any more.
    wanting: bool,
    /// The Info panel's mini-map as drawn in the last pass.
    pub mini: Option<MiniShown>,
    points: Points,
    /// The clusters drawn last, in canvas coordinates, and what they were computed for.
    clusters: Vec<Cluster>,
    clusters_for: Option<(u64, [u64; 4])>,
    /// Tiles drawn last frame from their own texture, and those missing (placeholder or a lower
    /// zoom's picture).
    pub tiles_drawn: usize,
    pub tiles_missing: usize,
    /// Time the last clustering took (ms).
    pub cluster_ms: f64,
    /// A pin being moved.
    pin_drag: Option<PinDrag>,
    /// Photos dragged over the map (from the filmstrip): how many and where they would land.
    drop_at: Option<(usize, egui::Pos2)>,
    /// The photos of the pin whose context menu is open.
    menu_for: Vec<u64>,
    /// The location filter bar's choice (kept for the session).
    pub location: LocationFilter,
    /// The area Visible on Map narrows the photos to (the view's, once it rested), and the
    /// view's area last seen with when it was first seen (to wait for the view to rest).
    applied_area: Option<Area>,
    seen_area: Option<(Area, f64)>,
    /// Photos with a position inside the view, and what it was counted for (list, area).
    in_view: usize,
    in_view_for: Option<(u64, [u64; 4])>,
    /// Time the last count of the photos in view took (ms).
    pub count_ms: f64,
    /// Saved locations: the one selected, a handle being dragged, counts.
    pub loc: locations::LocState,
    /// The track log drawn on the map.
    pub trk: tracks::TrackState,
    /// The window is being drawn into the headless screenshot's own context: the tile textures
    /// belong to the window's context, so this pass draws with them (the capture paints them from
    /// [`cpu_textures`]) and neither uploads nor takes the service's arrivals.
    pub(crate) shadow_pass: bool,
}

impl MapView {
    /// Tile textures held now.
    pub fn textures(&self) -> usize {
        self.textures.len()
    }

    /// The tiles that have a texture now.
    pub fn texture_keys(&self) -> HashSet<TileKey> {
        self.textures.keys().copied().collect()
    }
}

/// A map drawn from the shared tile textures: each tells the tile service what it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The Info panel's mini-map (drawn first in a frame: its few tiles go first).
    Mini = 0,
    /// The Map view.
    View = 1,
}

/// What [`draw_tiles`] drew.
pub struct TilesDrawn {
    /// Tiles drawn from their own texture, and those missing (a lower zoom's picture or grey).
    pub drawn: usize,
    pub missing: usize,
    /// The tile source's credit to paint on the map ([`draw_attribution`]); empty offline.
    pub attribution: String,
}

/// Deepest zoom the view goes to: the tile source's, or 19 offline.
fn max_zoom(app: &LightcraftApp) -> f64 {
    let s = app.session.map.source();
    if s.is_offline() { OFFLINE_MAX_ZOOM } else { f64::from(s.max_zoom) }
}

/// Re-project the photos when the photo list changed (catalog revision, source, filter, sort).
fn refresh_points(app: &mut LightcraftApp) {
    // the photos before the location filter: every pin, and the counts of the filter bar
    let (generation, ids) = app.session.visible_base_shared();
    if app.map.points.generation == Some(generation) {
        return;
    }
    let mut points = Vec::new();
    let mut without = 0;
    for id in ids.iter() {
        match app.session.catalog.photo(*id).and_then(|p| LatLon::from_gps(p.meta.gps)) {
            Some(pos) => points.push(MapPoint::new(id.0, pos)),
            None => without += 1,
        }
    }
    let bounds = Bounds::of(points.iter().map(|p| (p.pos.lat, p.pos.lon)));
    app.map.points = Points { generation: Some(generation), points: points.into(), without_gps: without, bounds };
    app.map.clusters_for = None;
    app.map.in_view_for = None;
}

fn area_bits(a: &Area) -> [u64; 4] {
    [a.south.to_bits(), a.west.to_bits(), a.north.to_bits(), a.east.to_bits()]
}

/// The photos with a position inside the map's view now (counted once per list and view).
fn count_in_view(app: &mut LightcraftApp) -> usize {
    let area = viewport(app).area();
    let key = (app.map.points.generation.unwrap_or(0), area_bits(&area));
    if app.map.in_view_for != Some(key) {
        let t0 = crate::now_ms();
        app.map.in_view = app.map.points.points.iter().filter(|p| area.contains(p.pos)).count();
        app.map.count_ms = crate::now_ms() - t0;
        app.map.in_view_for = Some(key);
    }
    app.map.in_view
}

/// The rules the location filter narrows the photos in view with (`None`: no filter). Visible on
/// Map takes the area it was last applied to (the view's when none was yet).
fn location_rules(app: &mut LightcraftApp) -> Option<lightcraft_catalog::RuleSet> {
    let rule = |field: &str, op: &str, value: Value| lightcraft_catalog::RuleSet {
        mode: lightcraft_catalog::Match::All,
        rules: vec![lightcraft_catalog::Rule::Field { field: field.into(), op: op.into(), value }],
    };
    match app.map.location {
        LocationFilter::None => None,
        LocationFilter::Tagged => Some(rule("hasGps", "is", json!(true))),
        LocationFilter::Untagged => Some(rule("hasGps", "is", json!(false))),
        LocationFilter::Visible => {
            let a = match app.map.applied_area {
                Some(a) => a,
                None => {
                    // before the map is first drawn its size is unknown: the first frame applies the view's
                    let a = viewport(app).area();
                    if app.map.rect.is_some() {
                        app.map.applied_area = Some(a);
                    }
                    a
                }
            };
            let value = lightcraft_catalog::rules::GpsArea { south: a.south, west: a.west, north: a.north, east: a.east }.to_value();
            Some(rule("gpsArea", "inside", value))
        }
    }
}

/// Narrow the photos in view by the location filter while the Map view is shown, and drop it
/// once it isn't (the library's own filter is never touched, so leaving the view brings back
/// what the grid showed). Called at the start of every frame and after every command.
pub fn sync_location_filter(app: &mut LightcraftApp) {
    let want = if app.ui.view == crate::state::ViewMode::Map { location_rules(app) } else { None };
    if app.session.narrow != want {
        app.session.narrow = want;
    }
}

/// Visible on Map follows the view: apply the view's area now (after a command moved the map).
fn apply_view_area(app: &mut LightcraftApp) {
    if app.map.location == LocationFilter::Visible {
        let a = viewport(app).area();
        app.map.applied_area = Some(a);
        app.map.seen_area = Some((a, crate::now_ms()));
    }
}

/// The counts of the filter bar: `{mode, visible, tagged, untagged, all, area}` (`area`: what
/// Visible on Map narrows to, null for the other modes).
fn location_state(app: &mut LightcraftApp) -> Value {
    let visible = count_in_view(app);
    let (tagged, untagged) = (app.map.points.points.len(), app.map.points.without_gps);
    let area = app.map.applied_area.filter(|_| app.map.location == LocationFilter::Visible);
    json!({
        "mode": app.map.location.id(),
        "visible": visible,
        "tagged": tagged,
        "untagged": untagged,
        "all": tagged + untagged,
        "area": area.map(|a| json!({"south": a.south, "west": a.west, "north": a.north, "east": a.east})),
        "countMs": app.map.count_ms,
    })
}

/// The view that fits the photos with GPS into `w × h` points (the world when there are none).
fn fit_camera(app: &LightcraftApp, w: f64, h: f64) -> Camera {
    match &app.map.points.bounds {
        Some(b) => {
            let v = fit_bounds(b, w, h, FIT_MARGIN, FIT_MAX_ZOOM.min(max_zoom(app)));
            Camera { center: v.center, zoom: v.zoom.floor().max(0.0) }
        }
        None => {
            let zoom = if w > 0.0 { (w / 256.0).log2().floor().clamp(1.0, 3.0) } else { 2.0 };
            Camera { center: LatLon { lat: 25.0, lon: 0.0 }, zoom }
        }
    }
}

fn viewport(app: &LightcraftApp) -> Viewport {
    let size = app.map.rect.map(|r| (f64::from(r.width()), f64::from(r.height()))).unwrap_or((0.0, 0.0));
    let cam = app.map.camera.unwrap_or(Camera { center: LatLon { lat: 25.0, lon: 0.0 }, zoom: 2.0 });
    Viewport::new(cam.center, cam.zoom.clamp(0.0, max_zoom(app)), size.0, size.1)
}

fn set_viewport(app: &mut LightcraftApp, v: Viewport) {
    let zoom = v.zoom.clamp(0.0, max_zoom(app));
    app.map.camera = Some(Camera { center: v.center, zoom });
}

/// Pixel-aligned screen rect of a tile (so neighbours share edges exactly, without seams).
fn snap(r: Rect, ppp: f32) -> Rect {
    let s = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(pos2(s(r.min.x), s(r.min.y)), pos2(s(r.max.x), s(r.max.y)))
}

/// Radius of a cluster's badge (points).
fn radius(n: usize) -> f32 {
    match n {
        0 | 1 => 7.0,
        2..=9 => 11.0,
        10..=99 => 13.0,
        100..=999 => 15.5,
        _ => 18.0,
    }
}

const PIN: Color32 = Color32::from_rgb(0xf2, 0xa3, 0x1b);
const PIN_TEXT: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
const PLACEHOLDER: Color32 = Color32::from_gray(0x3a);
const PLACEHOLDER_LINE: Color32 = Color32::from_gray(0x44);
const OUTSIDE: Color32 = Color32::from_gray(0x2a);

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let full = ui.max_rect();
    // the filmstrip below, when the window leaves room for the map above it
    let film = app.ui.filmstrip && full.height() >= t.film_h + 80.0;
    let film_h = if film { t.film_h } else { 0.0 };
    let above = Rect::from_min_max(full.min, pos2(full.right(), (full.bottom() - film_h).max(full.top())));
    // the location filter bar on top, when the map keeps some room
    let bar_h = if above.height() >= FILTER_BAR_H + 80.0 { FILTER_BAR_H } else { 0.0 };
    let canvas = Rect::from_min_max(pos2(above.left(), above.top() + bar_h), above.max);
    app.canvas_rect = Some(canvas);
    if film {
        super::detail::filmstrip(app, ui, Rect::from_min_max(pos2(full.left(), canvas.bottom()), full.max));
    }
    map_canvas(app, ui, canvas);
    if bar_h > 0.0 {
        filter_bar(app, ui, Rect::from_min_max(above.min, pos2(above.right(), canvas.top())));
    }
}

/// The location filter bar: Visible on Map, Tagged, Untagged, None, each with its count; the
/// choice is highlighted.
fn filter_bar(app: &mut LightcraftApp, ui: &mut egui::Ui, bar: Rect) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(bar, 0.0, t.chrome);
    ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, t.divider));
    register(ui, "map:filterBar", bar);
    let s = location_state(app);
    let count = |k: &str| s[k].as_u64().unwrap_or(0);
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(12.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    child.spacing_mut().item_spacing.x = 4.0;
    child.label(egui::RichText::new(crate::i18n::tr("Location Filter:")).font(t.font(12.0)).color(t.text_label));
    child.add_space(6.0);
    for mode in LocationFilter::ALL {
        let n = match mode {
            LocationFilter::Visible => count("visible"),
            LocationFilter::Tagged => count("tagged"),
            LocationFilter::Untagged => count("untagged"),
            LocationFilter::None => count("all"),
        };
        let on = app.map.location == mode;
        let text = format!("{} ({n})", crate::i18n::tr(mode.label()));
        let rich = egui::RichText::new(text).font(if on { t.semibold(12.0) } else { t.font(12.0) }).color(if on { t.text } else { t.text_label });
        let b = child.add(egui::Button::new(rich).selected(on).fill(if on { t.tool_active } else { egui::Color32::TRANSPARENT }));
        register(&child, format!("map:filter:{}", mode.id()), b.rect);
        if b.clicked() && !on {
            let _ = app.run("map.locationFilter", json!({"mode": mode.id()}));
        }
    }
}

fn map_canvas(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    app.map.rect = Some(canvas);
    refresh_points(app);
    let (w, h) = (f64::from(canvas.width()), f64::from(canvas.height()));
    if app.map.camera.is_none() || std::mem::take(&mut app.map.fit_pending) {
        app.map.camera = Some(fit_camera(app, w, h));
    }
    // a track log loaded before the map had a size
    if app.map.trk.fit_pending {
        tracks::fit(app);
    }
    let resp = ui.interact(canvas, egui::Id::unique("map-canvas"), Sense::click_and_drag());
    register(ui, "map:canvas", canvas);
    let local = |p: egui::Pos2| Point::new(f64::from(p.x - canvas.min.x), f64::from(p.y - canvas.min.y));

    // ---- a drag that starts on a single photo's pin moves it (unless markers are locked); any
    // other drag (empty map, a cluster) pans
    if resp.drag_started() {
        app.map.pin_drag = None;
        let origin = ui.input(|i| i.pointer.press_origin());
        // the selected saved location's handles come first (they sit on top of the pins)
        let on_location = origin.is_some_and(|o| locations::drag_start(app, o));
        if !on_location
            && !app.ui.map_lock_markers
            && let Some(origin) = origin
            && let Some(c) = pin_at(&app.map.clusters, local(origin))
            && c.ids.len() == 1
            && let Some(&id) = c.ids.first()
        {
            let center = pos2(canvas.min.x + c.center.x as f32, canvas.min.y + c.center.y as f32);
            app.map.pin_drag = Some(PinDrag { id, grab: center - origin, at: center });
        }
    } else if !resp.dragged() && !resp.drag_stopped() {
        app.map.pin_drag = None;
        locations::drag_end(app, &ctx, false);
    }
    let pointer = ui.input(|i| i.pointer.latest_pos());
    if let Some(d) = app.map.pin_drag.as_mut()
        && let Some(p) = pointer
    {
        d.at = p + d.grab;
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    }

    // ---- moving the map
    let mut vp = viewport(app);
    if resp.dragged() && app.map.pin_drag.is_none() && !locations::dragging(app) {
        let d = resp.drag_delta();
        vp = vp.panned(f64::from(d.x), f64::from(d.y));
    }
    if resp.hovered() || resp.dragged() {
        let (factor, wheel, anchor) = ui.input(|i| {
            let mut zoom = 0.0f64;
            let mut pan = egui::Vec2::ZERO;
            for e in &i.events {
                if let egui::Event::MouseWheel { unit, delta, modifiers, .. } = e
                    && !modifiers.ctrl
                    && !modifiers.command
                    && delta.is_finite()
                {
                    match unit {
                        // a mouse wheel zooms, as on any web map
                        egui::MouseWheelUnit::Line => zoom += f64::from(delta.y) * WHEEL_ZOOM,
                        egui::MouseWheelUnit::Page => zoom += f64::from(delta.y.signum()),
                        // a trackpad's two-finger scroll pans
                        egui::MouseWheelUnit::Point => pan += *delta,
                    }
                }
            }
            (i.zoom_delta(), (zoom, pan), i.multi_touch().map(|t| t.center_pos).or(i.pointer.hover_pos()))
        });
        let anchor = anchor.filter(|p| canvas.contains(*p)).map(local).unwrap_or(Point::new(w / 2.0, h / 2.0));
        let mut dz = wheel.0;
        if factor.is_finite() && factor > 0.0 && factor != 1.0 {
            dz += f64::from(factor).log2();
        }
        if dz != 0.0 && dz.is_finite() {
            vp = vp.zoomed_around(anchor, (vp.zoom + dz.clamp(-3.0, 3.0)).clamp(0.0, max_zoom(app)));
        }
        if wheel.1 != egui::Vec2::ZERO {
            vp = vp.panned(f64::from(wheel.1.x), f64::from(wheel.1.y));
        }
        if resp.hovered() {
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }
    let vp = Viewport::new(vp.center, vp.zoom, w, h);
    set_viewport(app, vp);
    // a saved location's centre or radius follows the pointer
    if let Some(p) = pointer.filter(|_| locations::dragging(app)) {
        locations::drag_update(app, canvas, &vp, p);
        ctx.request_repaint();
    }
    // ---- Visible on Map follows the view once it rests (a pan or zoom doesn't refilter the
    // filmstrip every frame); the first view applies at once
    if app.map.location == LocationFilter::Visible {
        let area = vp.area();
        let now = crate::now_ms();
        let since = match app.map.seen_area {
            Some((a, at)) if a == area => at,
            _ => {
                app.map.seen_area = Some((area, now));
                now
            }
        };
        if app.map.applied_area != Some(area) {
            let wait = AREA_SETTLE_MS - (now - since);
            if app.map.applied_area.is_none() || wait <= 0.0 {
                app.map.applied_area = Some(area);
                sync_location_filter(app);
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(wait.clamp(1.0, AREA_SETTLE_MS) as u64));
            }
        }
    }

    // ---- a moved saved location lands (outside the map: nothing changes)
    if resp.drag_stopped() {
        locations::drag_end(app, &ctx, pointer.is_some_and(|p| canvas.contains(p)));
    }
    // ---- a moved pin lands (outside the map: nothing changes)
    if resp.drag_stopped()
        && let Some(d) = app.map.pin_drag.take()
        && pointer.is_some_and(|p| canvas.contains(p))
    {
        let to = vp.to_lat_lon(local(d.at));
        if let Err(e) = app.run("photo.setGps", json!({"ids": [d.id], "lat": to.lat, "lon": to.lon})) {
            app.toast(&ctx, e);
        }
    }
    // ---- photos dragged from the filmstrip: a pin at the pointer; a release sets their position
    // (not under a floating panel, like the catalog's drop targets)
    app.map.drop_at = None;
    if let Some(ids) = app.ui.dragging_photos.clone()
        && let Some(p) = pointer.filter(|p| crate::docking::drop_target(ui, canvas, pointer) && !app.map.loc.overlay.is_some_and(|r| r.contains(*p)))
    {
        if ui.input(|i| i.pointer.any_released()) {
            app.ui.dragging_photos = None;
            let to = vp.to_lat_lon(local(p));
            if let Err(e) = app.run("photo.setGps", json!({"ids": ids, "lat": to.lat, "lon": to.lon})) {
                app.toast(&ctx, e);
            }
        } else {
            app.map.drop_at = Some((ids.len(), p));
        }
    }

    // ---- tiles
    let painter = ui.painter_at(canvas);
    painter.rect_filled(canvas, 0.0, OUTSIDE);
    let offline = app.session.map.source().is_offline();
    let tiles = draw_tiles(app, &ctx, &painter, canvas, &vp, Surface::View);
    app.map.tiles_drawn = tiles.drawn;
    app.map.tiles_missing = tiles.missing;

    // ---- saved locations and the track log, under the pins
    locations::draw(app, ui, &painter, &t, canvas, &vp);
    tracks::draw(app, &painter, &t, canvas, &vp);

    // ---- pins
    let key = (
        app.map.points.generation.unwrap_or(0) ^ u64::from(app.map.location == LocationFilter::Untagged).rotate_left(63),
        [vp.center.lat.to_bits(), vp.center.lon.to_bits(), vp.zoom.to_bits(), (vp.width.to_bits() ^ vp.height.to_bits().rotate_left(32))],
    );
    if app.map.clusters_for != Some(key) {
        let t0 = crate::now_ms();
        // Untagged: the filmstrip holds the photos without a position, so no pins
        let shown: &[MapPoint] = if app.map.location == LocationFilter::Untagged { &[] } else { &app.map.points.points };
        app.map.clusters = lightcraft_map::cluster(shown, &vp, CLUSTER_CELL);
        app.map.cluster_ms = crate::now_ms() - t0;
        app.map.clusters_for = Some(key);
    }
    let selected: HashSet<u64> = app.session.selection.ids.iter().map(|p| p.0).collect();
    let active = app.session.selection.active.map(|a| a.0);
    let to_screen = |c: &Cluster| pos2(canvas.min.x + c.center.x as f32, canvas.min.y + c.center.y as f32);
    let hover = resp.hover_pos().filter(|_| !resp.dragged() && app.ui.dragging_photos.is_none()).map(local);
    let hovered = hover.and_then(|p| lightcraft_map::cluster::hit(&app.map.clusters, p, |c| f64::from(radius(c.len())) + 3.0));
    let mut listed = HashSet::new();
    let moving = app.map.pin_drag.map(|d| d.id);
    for (i, c) in app.map.clusters.iter().enumerate() {
        let at = to_screen(c);
        let n = c.len();
        if n == 1 && c.ids.first().copied() == moving {
            // the pin being moved: a faint mark where it was
            painter.circle(at, radius(1), PIN.gamma_multiply(0.35), Stroke::new(1.5, Color32::from_white_alpha(120)));
            continue;
        }
        let sel = c.ids.iter().any(|id| selected.contains(id));
        let is_active = active.is_some_and(|a| c.ids.contains(&a));
        let r = radius(n) + if hovered == Some(i) { 1.5 } else { 0.0 };
        let fill = if sel { t.accent } else { PIN };
        painter.circle_filled(at + vec2(0.0, 1.5), r + 1.0, Color32::from_black_alpha(90));
        painter.circle(at, r, fill, Stroke::new(if is_active { 3.0 } else { 2.0 }, Color32::WHITE));
        if n > 1 {
            let text = if n > 9999 { format!("{}k", n / 1000) } else { n.to_string() };
            painter.text(at, Align2::CENTER_CENTER, text, t.semibold(11.0), if sel { Color32::WHITE } else { PIN_TEXT });
        }
        if listed.len() < MAX_LISTED && listed.insert(c.ids.first().copied().unwrap_or(0)) {
            register(ui, format!("map:pin:{}", c.ids.first().copied().unwrap_or(0)), Rect::from_center_size(at, vec2(r * 2.0, r * 2.0)));
        }
    }
    if let Some(d) = app.map.pin_drag {
        let sel = selected.contains(&d.id);
        draw_pin(&painter, d.at, 1, if sel { t.accent } else { PIN }, &t, active == Some(d.id));
    }
    if let Some((n, p)) = app.map.drop_at {
        drop_indicator(&painter, &t, p, n);
    }
    if let Some(cursor) = resp.hover_pos().and_then(|p| locations::hover_cursor(app, p)) {
        ctx.set_cursor_icon(cursor);
    } else if let Some(c) = hovered.and_then(|i| app.map.clusters.get(i)).filter(|_| app.map.pin_drag.is_none()) {
        // a single photo's pin can be picked up (unless markers are locked); a cluster is clicked
        let movable = c.len() == 1 && !app.ui.map_lock_markers;
        ctx.set_cursor_icon(if movable { egui::CursorIcon::Grab } else { egui::CursorIcon::PointingHand });
    }
    let hovered_cluster = hovered.and_then(|i| app.map.clusters.get(i).cloned());

    // ---- clicks
    if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.clicked() || resp.double_clicked()).map(local) {
        let hit =
            lightcraft_map::cluster::hit(&app.map.clusters, p, |c| f64::from(radius(c.len())) + 3.0).and_then(|i| app.map.clusters.get(i).cloned());
        match hit {
            Some(c) => {
                let m = ui.input(|i| i.modifiers);
                let mode = if (m.command || m.shift) && !resp.double_clicked() { "add" } else { "replace" };
                let first = c.ids.first().copied();
                let _ = app.run("library.select", json!({"ids": c.ids, "mode": mode, "active": first}));
                if resp.double_clicked() {
                    let _ = app.run("view.detail", json!({}));
                }
            }
            // a saved location's centre or name selects it (anywhere else drops the selection)
            None if resp.clicked() && !resp.double_clicked() => {
                if let Some(at) = resp.interact_pointer_pos() {
                    locations::click(app, at);
                }
            }
            None if resp.double_clicked() => {
                let v = viewport(app);
                set_viewport(app, v.zoomed_around(p, (v.zoom + 1.0).min(max_zoom(app)).floor()));
                ctx.request_repaint();
            }
            None => {}
        }
    }

    // ---- right-click on a pin: its photos' menu
    if resp.secondary_clicked() {
        app.map.menu_for =
            resp.interact_pointer_pos().map(local).and_then(|p| pin_at(&app.map.clusters, p)).map(|c| c.ids.clone()).unwrap_or_default();
    }
    if !app.map.menu_for.is_empty() {
        let ids = app.map.menu_for.clone();
        resp.context_menu(|ui| {
            let label = if ids.len() == 1 { "Remove GPS Coordinates" } else { "Remove GPS Coordinates of These Photos" };
            let b = ui.button(crate::i18n::tr(label));
            register(ui, "map:pinMenu:removeGps", b.rect);
            if b.clicked() {
                if let Err(e) = app.run("photo.removeGps", json!({"ids": ids})) {
                    app.toast(ui.ctx(), e);
                }
                app.map.menu_for.clear();
                ui.close();
            }
        });
    }

    // ---- hover thumbnail
    if let Some(c) = hovered_cluster.filter(|_| app.map.pin_drag.is_none())
        && let Some(&first) = c.ids.first()
    {
        thumbnail(app, ui, &painter, to_screen(&c), radius(c.len()), PhotoId(first), c.len());
    }

    // ---- overlays: notes, controls, attribution
    let n_gps = app.map.points.points.len();
    let without = app.map.points.without_gps;
    if app.map.location == LocationFilter::Untagged && without > 0 {
        let text = crate::i18n::tr("Untagged: the filmstrip shows the photos without a location. Drag them onto the map to place them.").to_string();
        // below the toolbar (and the offline note)
        note_box(&painter, &t, pos2(canvas.center().x, canvas.top() + 84.0 + if offline { 40.0 } else { 0.0 }), &[(text, false)]);
    } else if n_gps == 0 {
        let (title, body) = if without == 0 {
            ("No photos", "Photos with a GPS position appear here as pins")
        } else {
            ("No photos with a location", "Photos with a GPS position appear here as pins (Info panel ▸ GPS, or Photo ▸ Auto-Tag from Tracklog…)")
        };
        note_box(&painter, &t, canvas.center(), &[(crate::i18n::tr(title).to_string(), true), (crate::i18n::tr(body).to_string(), false)]);
    }
    if offline {
        let text = crate::i18n::tr("Map tiles are off (Settings / map.tileSource)").to_string();
        note_box(&painter, &t, pos2(canvas.center().x, canvas.top() + 28.0), &[(text, false)]);
    }
    if without > 0 {
        let text = format!("{without} {}", crate::i18n::tr(if without == 1 { "photo without GPS" } else { "photos without GPS" }));
        let g = painter.layout_no_wrap(text, t.font(11.5), t.text_label);
        let r = Rect::from_min_size(pos2(canvas.left() + 8.0, canvas.bottom() - g.size().y - 14.0), g.size() + vec2(12.0, 6.0));
        painter.rect_filled(r, 4.0, Color32::from_black_alpha(150));
        painter.galley(r.min + vec2(6.0, 3.0), g, t.text_label);
        register(ui, "map:withoutGps", r);
    }
    draw_attribution(ui, &painter, &t, canvas, tiles.attribution, 11.0, "map:attribution");
    controls(app, ui, canvas);
    locations::overlay(app, ui, canvas);
}

/// The pin (or cluster) under canvas point `p`, as a click finds it.
fn pin_at(clusters: &[Cluster], p: Point) -> Option<&Cluster> {
    lightcraft_map::cluster::hit(clusters, p, |c| f64::from(radius(c.len())) + 3.0).and_then(|i| clusters.get(i))
}

/// A pin of `n` photos at `at` (lifted, with its shadow below: it is being carried).
fn draw_pin(painter: &egui::Painter, at: egui::Pos2, n: usize, fill: Color32, t: &Tokens, thick: bool) {
    let r = radius(n);
    painter.circle_filled(at + vec2(0.0, 6.0), r * 0.8, Color32::from_black_alpha(70));
    painter.circle(at, r + 1.0, fill, Stroke::new(if thick { 3.0 } else { 2.0 }, Color32::WHITE));
    if n > 1 {
        let text = if n > 9999 { format!("{}k", n / 1000) } else { n.to_string() };
        painter.text(at, Align2::CENTER_CENTER, text, t.semibold(11.0), Color32::WHITE);
    }
}

/// Where photos dragged over the map would land: a pin at the point, a ring around it and the
/// count.
fn drop_indicator(painter: &egui::Painter, t: &Tokens, at: egui::Pos2, n: usize) {
    painter.circle_stroke(at, 22.0, Stroke::new(2.0, t.accent.gamma_multiply(0.8)));
    painter.line_segment([at - vec2(30.0, 0.0), at - vec2(24.0, 0.0)], Stroke::new(1.5, Color32::WHITE));
    painter.line_segment([at + vec2(24.0, 0.0), at + vec2(30.0, 0.0)], Stroke::new(1.5, Color32::WHITE));
    painter.line_segment([at - vec2(0.0, 30.0), at - vec2(0.0, 24.0)], Stroke::new(1.5, Color32::WHITE));
    painter.line_segment([at + vec2(0.0, 24.0), at + vec2(0.0, 30.0)], Stroke::new(1.5, Color32::WHITE));
    draw_pin(painter, at, n, t.accent, t, true);
}

/// Draw the tiles of `vp` on `canvas` from the shared texture cache, telling the tile service what
/// this map shows (nothing offline). A tile not loaded yet is drawn from a lower zoom's texture
/// when one is at hand, else as a grey square. Used by the Map view and the Info panel's mini-map:
/// a tile one of them uploaded is drawn by the other without uploading it again.
pub fn draw_tiles(
    app: &mut LightcraftApp,
    ctx: &egui::Context,
    painter: &egui::Painter,
    canvas: Rect,
    vp: &Viewport,
    surface: Surface,
) -> TilesDrawn {
    let ppp = ctx.pixels_per_point();
    let pass = ctx.cumulative_pass_nr();
    let offline = app.session.map.source().is_offline();
    let source_max = app.session.map.source().max_zoom;
    let mut attribution = String::new();
    if app.map.textures_source != app.session.map.source().id {
        app.map.textures.clear();
        app.map.textures_source = app.session.map.source().id.clone();
    }
    // a headless screenshot's pass (its own context and pass numbers) draws what the window has
    // and changes nothing: no tiles asked for, no arrivals taken, no uploads
    let shadow = app.map.shadow_pass;
    if !offline {
        // the Map view loads a ring of tiles around what it shows (it pans); the mini-map doesn't
        let margin = if surface == Surface::Mini { 0 } else { 1 };
        if !shadow && let Some(w) = app.map.wanted.get_mut(surface as usize) {
            *w = (pass, vp.tile_keys(source_max, margin));
        }
        // every map drawn in this pass or the last one: one doesn't cancel the other's tiles
        let keys: Vec<TileKey> = app.map.wanted.iter().filter(|(p, _)| p.saturating_add(1) >= pass).flat_map(|(_, k)| k.iter().copied()).collect();
        app.map.wanting |= !shadow;
        let svc = app.session.map_tiles();
        if !shadow {
            svc.want(&keys);
        }
        let events = if shadow { Vec::new() } else { svc.poll() };
        let idle = svc.is_idle();
        attribution = svc.source().attribution.clone();
        for e in &events {
            // a fresh copy replaces the texture (uploaded again when next drawn); a texture already
            // made from this very arrival (it landed between a poll and a draw) stays
            if let TileEvent::Ready { key, .. } = e {
                let same = app.map.textures.get(key).and_then(|tt| tt.from.upgrade()).zip(svc.get(*key)).is_some_and(|(a, b)| Arc::ptr_eq(&a, &b));
                if !same {
                    app.map.textures.remove(key);
                }
            }
        }
        if !events.is_empty() {
            ctx.request_repaint();
        } else if !idle {
            // the service doesn't wake the UI: look again soon while tiles are on their way
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    let (mut drawn, mut missing) = (0, 0);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    for tile in vp.tiles(source_max) {
        let r = &tile.rect;
        let rect = snap(
            Rect::from_min_max(
                pos2(canvas.min.x + r.x0 as f32, canvas.min.y + r.y0 as f32),
                pos2(canvas.min.x + r.x1 as f32, canvas.min.y + r.y1 as f32),
            ),
            ppp,
        );
        if !offline
            && !shadow
            && !app.map.textures.contains_key(&tile.key)
            && let Some(img) = app.session.map_tiles().get(tile.key)
            && let Some(tex) = upload(ctx, tile.key, &img)
        {
            app.map.textures.insert(tile.key, TileTex { tex, used: pass, from: Arc::downgrade(&img) });
            app.map.uploads += 1;
        }
        if let Some(tt) = app.map.textures.get_mut(&tile.key) {
            if !shadow {
                tt.used = pass;
            }
            painter.image(tt.tex.id(), rect, uv, Color32::WHITE);
            drawn += 1;
            continue;
        }
        missing += 1;
        // a lower zoom's picture of the place, magnified, until the tile arrives
        let mut stand_in = false;
        for d in 1..=4u8.min(tile.key.z) {
            let parent = TileKey { z: tile.key.z - d, x: tile.key.x >> d, y: tile.key.y >> d };
            if let Some(tt) = app.map.textures.get_mut(&parent) {
                if !shadow {
                    tt.used = pass;
                }
                let n = f32::from(1u16 << d);
                let (ox, oy) = ((tile.key.x & ((1 << d) - 1)) as f32 / n, (tile.key.y & ((1 << d) - 1)) as f32 / n);
                painter.image(tt.tex.id(), rect, Rect::from_min_size(pos2(ox, oy), vec2(1.0 / n, 1.0 / n)), Color32::WHITE);
                stand_in = true;
                break;
            }
        }
        if !stand_in {
            painter.rect_filled(rect, 0.0, PLACEHOLDER);
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, PLACEHOLDER_LINE), StrokeKind::Inside);
        }
    }
    if !shadow {
        evict(&mut app.map, pass);
    }
    TilesDrawn { drawn, missing, attribution }
}

/// Paint the tile source's credit at the bottom right of `canvas`, always visible (the tile
/// provider's terms: never behind a toggle or off screen; wrapped on a map narrower than it), and
/// register it as `widget`. Nothing for an empty credit (offline).
pub fn draw_attribution(ui: &egui::Ui, painter: &egui::Painter, t: &Tokens, canvas: Rect, text: String, size: f32, widget: &str) {
    if text.is_empty() {
        return;
    }
    let ink = Color32::from_gray(0x20);
    let g = painter.layout(text, t.font(size), ink, (canvas.width() - 16.0).max(40.0));
    let r = Rect::from_min_size(
        pos2((canvas.right() - g.size().x - 12.0).max(canvas.left()), canvas.bottom() - g.size().y - 6.0),
        g.size() + vec2(8.0, 4.0),
    );
    painter.rect_filled(r, 2.0, Color32::from_white_alpha(200));
    painter.galley(r.min + vec2(4.0, 2.0), g, ink);
    register(ui, widget, r);
}

/// At the end of a frame: when no map was drawn in it, stop loading the tiles the last one asked
/// for (only the maps on screen fetch), and forget the mini-map.
pub fn end_pass(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.map.shadow_pass {
        return;
    }
    let pass = ctx.cumulative_pass_nr();
    if app.map.mini.as_ref().is_some_and(|m| m.pass != pass) {
        app.map.mini = None;
    }
    if app.map.loc.private_mini.is_some_and(|(_, _, p)| p != pass) {
        app.map.loc.private_mini = None;
    }
    if app.map.wanting && !app.map.wanted.iter().any(|(p, _)| *p == pass) {
        app.map.wanting = false;
        for w in &mut app.map.wanted {
            w.1.clear();
        }
        if let Some(svc) = app.session.map.service() {
            svc.want(&[]);
        }
    }
}

/// A centred box of lines (`(text, bold)`) on the map.
fn note_box(painter: &egui::Painter, t: &Tokens, center: egui::Pos2, lines: &[(String, bool)]) {
    let galleys: Vec<_> = lines
        .iter()
        .map(|(s, bold)| {
            painter.layout(s.clone(), if *bold { t.semibold(15.0) } else { t.font(12.5) }, if *bold { t.text } else { t.text_label }, 460.0)
        })
        .collect();
    let w = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max) + 28.0;
    let h = galleys.iter().map(|g| g.size().y + 4.0).sum::<f32>() + 16.0;
    let r = Rect::from_center_size(center, vec2(w, h));
    painter.rect_filled(r, 6.0, Color32::from_black_alpha(170));
    let mut y = r.top() + 8.0;
    for g in galleys {
        let x = r.center().x - g.size().x / 2.0;
        let gh = g.size().y;
        painter.galley(pos2(x, y), g, t.text);
        y += gh + 4.0;
    }
}

/// The hovered pin's first photo above it, with the number of photos when there are several.
#[allow(clippy::too_many_arguments)]
fn thumbnail(app: &mut LightcraftApp, ui: &egui::Ui, painter: &egui::Painter, at: egui::Pos2, r: f32, id: PhotoId, n: usize) {
    let t = Tokens::get(ui.ctx());
    let ppp = ui.ctx().pixels_per_point();
    let edge = 150.0;
    super::grid::request_thumb(app, id, (edge * ppp).round() as usize, 10);
    let Some(tex) = app.renderer.thumb(id) else { return };
    let [tw, th] = tex.size;
    let (tw, th) = (tw.max(1) as f32, th.max(1) as f32);
    let scale = edge / tw.max(th);
    let size = vec2(tw * scale, th * scale);
    let caption = if n > 1 { Some(format!("{n} {}", crate::i18n::tr("photos"))) } else { None };
    let cap_h = if caption.is_some() { 18.0 } else { 0.0 };
    let frame = Rect::from_min_size(pos2(at.x - size.x / 2.0 - 4.0, at.y - r - 10.0 - size.y - 8.0 - cap_h), size + vec2(8.0, 8.0 + cap_h));
    // keep it on the map
    let clip = painter.clip_rect();
    let dx = (clip.left() + 4.0 - frame.left()).max(0.0) + (clip.right() - 4.0 - frame.right()).min(0.0);
    let dy = (clip.top() + 4.0 - frame.top()).max(0.0);
    let frame = frame.translate(vec2(dx, dy));
    painter.rect_filled(frame.expand(1.0), 4.0, Color32::from_black_alpha(120));
    painter.rect_filled(frame, 4.0, t.chrome);
    let img = Rect::from_min_size(frame.min + vec2(4.0, 4.0), size);
    painter.image(tex.tex.id(), img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    if let Some(c) = caption {
        painter.text(pos2(frame.center().x, img.bottom() + 10.0), Align2::CENTER_CENTER, c, t.font(11.5), t.text_label);
    }
    register(ui, "map:hoverThumb", frame);
}

/// The Info panel's map of the active photo's position `pos`: `width` wide (the panel's fields),
/// centred on it at [`MINI_ZOOM`], with its pin and the tile source's credit. It doesn't pan (no
/// stray drags in a scrolling panel); a click opens the Map view centred there. It shares the Map view's tiles and
/// textures, and asks for tiles only while it is on screen (scrolled into view). Offline it is
/// grey, with the pin and a one-line note: the panel keeps its shape and the click still works.
pub fn mini_map(app: &mut LightcraftApp, ui: &mut egui::Ui, photo: PhotoId, pos: LatLon, width: f32) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let w = if width.is_finite() { width.clamp(0.0, ui.available_width().max(0.0)) } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, MINI_HEIGHT), Sense::click());
    if w < 1.0 || !ui.is_rect_visible(rect) {
        return;
    }
    register(ui, "info:miniMap", rect);
    let zoom = MINI_ZOOM.min(max_zoom(app));
    let vp = Viewport::new(pos, zoom, f64::from(w), f64::from(MINI_HEIGHT));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, OUTSIDE);
    let tiles = draw_tiles(app, &ctx, &painter, rect, &vp, Surface::Mini);
    // the pin (where the photo is; near the poles the map stops short of it, so keep it on the map)
    let p = vp.to_screen(pos);
    let pin = pos2(rect.min.x + p.x as f32, rect.min.y + p.y as f32);
    let pin = keep_on_map(pin, rect);
    painter.circle_filled(pin + vec2(0.0, 1.5), radius(1) + 1.0, Color32::from_black_alpha(90));
    // drawn as the Map view draws the active photo's pin
    painter.circle(pin, radius(1), t.accent, Stroke::new(3.0, Color32::WHITE));
    if app.session.map.source().is_offline() {
        let g = painter.layout(crate::i18n::tr("Map tiles are off").to_string(), t.font(11.5), t.text_label, (w - 16.0).max(40.0));
        let r = Rect::from_center_size(pos2(rect.center().x, rect.top() + 8.0 + g.size().y / 2.0 + 3.0), g.size() + vec2(12.0, 6.0));
        painter.rect_filled(r, 4.0, Color32::from_black_alpha(150));
        painter.galley(r.min + vec2(6.0, 3.0), g, t.text_label);
    }
    draw_attribution(ui, &painter, &t, rect, tiles.attribution, 10.0, "info:miniMapAttribution");
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, t.divider), StrokeKind::Inside);
    if !app.map.shadow_pass {
        app.map.mini = Some(MiniShown {
            photo,
            center: vp.center,
            zoom,
            rect,
            pin,
            tiles_drawn: tiles.drawn,
            tiles_missing: tiles.missing,
            pass: ctx.cumulative_pass_nr(),
        });
    }
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.on_hover_text(crate::i18n::tr("Show in the Map view")).clicked() {
        let shown = app.run("view.map", json!({}));
        let centred = app.run("map.setView", json!({"lat": pos.lat, "lon": pos.lon, "zoom": zoom}));
        if let Err(e) = shown.and(centred) {
            app.toast(&ctx, e);
        }
    }
}

/// `pin` kept 8 points inside `rect` (the centre of a map narrower than that; a non-finite one too).
pub(crate) fn keep_on_map(pin: egui::Pos2, rect: Rect) -> egui::Pos2 {
    if !(pin.x.is_finite() && pin.y.is_finite()) {
        return rect.center();
    }
    // an ordered range however small the map (f32::clamp panics on min > max)
    let lo = (rect.min + vec2(8.0, 8.0)).min(rect.center());
    let hi = (rect.max - vec2(8.0, 8.0)).max(rect.center());
    pin.clamp(lo, hi)
}

/// + / − and Fit to Photos, top right.
fn controls(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect) {
    let t = Tokens::get(ui.ctx());
    let fit_label = crate::i18n::tr("Fit to Photos").to_string();
    let fit_w = ui.painter().layout_no_wrap(fit_label.clone(), t.font(12.5), t.text).size().x + 20.0;
    let lock_label = crate::i18n::tr("Lock Markers").to_string();
    let lock_w = ui.painter().layout_no_wrap(lock_label.clone(), t.font(12.5), t.text).size().x + 20.0;
    let track_w = ui.painter().layout_no_wrap(crate::i18n::tr("Tracklog").to_string(), t.font(12.5), t.text).size().x + 20.0;
    let bar_w = track_w + 6.0 + lock_w + 6.0 + fit_w + 6.0 + 64.0;
    let bar = Rect::from_min_size(pos2(canvas.right() - 12.0 - bar_w, canvas.top() + 12.0), vec2(bar_w, 30.0));
    if bar.left() < canvas.left() || bar.bottom() > canvas.bottom() {
        return;
    }
    tracks::button(app, ui, Rect::from_min_size(bar.min, vec2(track_w, 30.0)));
    let locked = app.ui.map_lock_markers;
    let lock_r = Rect::from_min_size(pos2(bar.left() + track_w + 6.0, bar.top()), vec2(lock_w, 30.0));
    let fill = if locked { t.tool_active } else { t.chrome.gamma_multiply(0.92) };
    let b = ui.put(lock_r, egui::Button::new(egui::RichText::new(lock_label).font(t.font(12.5))).fill(fill).selected(locked));
    register(ui, "map:lockMarkers", b.rect);
    if b.on_hover_text(crate::i18n::tr("Pins can't be dragged to move photos")).clicked() {
        let _ = app.run("map.lockMarkers", json!({}));
    }
    let p = ui.painter();
    let zoom_r = Rect::from_min_size(pos2(bar.right() - 64.0, bar.top()), vec2(64.0, 30.0));
    p.rect_filled(zoom_r, 4.0, t.chrome.gamma_multiply(0.92));
    let fit_r = Rect::from_min_size(pos2(lock_r.right() + 6.0, bar.top()), vec2(fit_w, 30.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(zoom_r).layout(egui::Layout::left_to_right(egui::Align::Center)));
    child.spacing_mut().item_spacing.x = 0.0;
    let (zmax, z) = (max_zoom(app), viewport(app).zoom);
    if icon_button(&mut child, "mapZoomOut", Icon::Minus, vec2(32.0, 30.0), false, z > 0.0, "Zoom Out").clicked() {
        let _ = app.run("map.zoomOut", json!({}));
    }
    if icon_button(&mut child, "mapZoomIn", Icon::Plus, vec2(32.0, 30.0), false, z < zmax, "Zoom In").clicked() {
        let _ = app.run("map.zoomIn", json!({}));
    }
    let b = ui.put(fit_r, egui::Button::new(egui::RichText::new(fit_label).font(t.font(12.5))).fill(t.chrome.gamma_multiply(0.92)));
    register(ui, "map:fit", b.rect);
    if b.on_hover_text(crate::i18n::tr("Show every photo with a location")).clicked() {
        let _ = app.run("map.fit", json!({}));
    }
}

/// Upload a decoded tile as a texture (`None` for a tile whose pixels don't match its size).
fn upload(ctx: &egui::Context, key: TileKey, img: &lightcraft_map::TileImage) -> Option<egui::TextureHandle> {
    let (w, h) = (img.width as usize, img.height as usize);
    if w == 0 || h == 0 || w.checked_mul(h)?.checked_mul(4)? != img.rgba.len() {
        return None;
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &img.rgba);
    Some(ctx.load_texture(format!("map-tile-{}-{}-{}", key.z, key.x, key.y), image, egui::TextureOptions::LINEAR))
}

/// The tile textures as CPU images (by texture id), for a headless screenshot to paint them: the
/// textures live in the window's context, not the capture's. Tiles no longer in memory are left
/// out (drawn as missing).
pub(crate) fn cpu_textures(app: &LightcraftApp) -> HashMap<egui::TextureId, crate::softpaint::CpuTexture> {
    let Some(svc) = app.session.map.service() else { return HashMap::new() };
    app.map
        .textures
        .iter()
        .filter_map(|(key, tt)| {
            let img = svc.get(*key)?;
            let (w, h) = (img.width as usize, img.height as usize);
            if w == 0 || h == 0 || w.checked_mul(h)?.checked_mul(4)? != img.rgba.len() {
                return None;
            }
            let image = Arc::new(egui::ColorImage::from_rgba_unmultiplied([w, h], &img.rgba));
            Some((tt.tex.id(), crate::softpaint::CpuTexture::linear(image)))
        })
        .collect()
}

/// Drop the least recently drawn textures past [`MAX_TEXTURES`] (down to three quarters of it),
/// never one drawn in frame `now` (it would only be uploaded again on the next frame).
fn evict(m: &mut MapView, now: u64) {
    if m.textures.len() <= MAX_TEXTURES {
        return;
    }
    let mut by_use: Vec<(u64, TileKey)> = m.textures.iter().filter(|(_, v)| v.used != now).map(|(k, v)| (v.used, *k)).collect();
    by_use.sort();
    let drop = m.textures.len() - MAX_TEXTURES * 3 / 4;
    for (_, k) in by_use.into_iter().take(drop) {
        m.textures.remove(&k);
    }
}

fn num(p: &Value, key: &str) -> Result<Option<f64>, String> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().filter(|v| v.is_finite()).map(Some).ok_or_else(|| format!("map.setView: `{key}` must be a number")),
    }
}

/// The map's UI commands (`None`: not one of them).
pub fn command(app: &mut LightcraftApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    // in the Map view the zoom keys (⌘= / ⌘- / ⌘0) zoom the map
    let id = match id {
        "view.zoomIn" | "view.zoomOut" | "view.zoomFit" if app.ui.view == crate::state::ViewMode::Map => match id {
            "view.zoomIn" => "map.zoomIn",
            "view.zoomOut" => "map.zoomOut",
            _ => "map.fit",
        },
        other => other,
    };
    if let Some(r) = locations::command(app, id, p) {
        return Some(r);
    }
    if let Some(r) = tracks::command(app, id, p) {
        return Some(r);
    }
    let r = match id {
        "view.map" => {
            app.ui.view = crate::state::ViewMode::Map;
            sync_location_filter(app);
            refresh_points(app);
            Ok(json!({"withGps": app.map.points.points.len(), "withoutGps": app.map.points.without_gps}))
        }
        "map.fit" => {
            refresh_points(app);
            match app.map.rect {
                Some(r) => {
                    app.map.camera = Some(fit_camera(app, f64::from(r.width()), f64::from(r.height())));
                    apply_view_area(app);
                }
                None => {
                    app.map.fit_pending = true;
                    app.map.applied_area = None;
                }
            }
            Ok(state(app))
        }
        "map.setView" => {
            // {lat?, lon?, zoom?}: what is left out stays
            let (lat, lon, zoom) = match (num(p, "lat"), num(p, "lon"), num(p, "zoom")) {
                (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return Some(Err(e)),
            };
            if lat.is_some_and(|v| !(-90.0..=90.0).contains(&v)) || lon.is_some_and(|v| !(-180.0..=180.0).contains(&v)) {
                return Some(Err("map.setView: lat is −90…90 and lon −180…180 (degrees)".into()));
            }
            let zmax = max_zoom(app);
            if zoom.is_some_and(|z| !(0.0..=zmax).contains(&z)) {
                return Some(Err(format!("map.setView: zoom is 0…{zmax}")));
            }
            let cur = viewport(app);
            let center = LatLon { lat: lat.unwrap_or(cur.center.lat), lon: lon.unwrap_or(cur.center.lon) };
            app.map.camera = Some(Camera { center, zoom: zoom.unwrap_or(cur.zoom) });
            app.map.fit_pending = false;
            apply_view_area(app);
            Ok(state(app))
        }
        "map.zoomIn" | "map.zoomOut" => {
            let v = viewport(app);
            let step = if id == "map.zoomIn" { 1.0 } else { -1.0 };
            let z = if step > 0.0 { (v.zoom + 1.0).floor() } else { (v.zoom - 1.0).ceil() };
            app.map.camera = Some(Camera { center: v.center, zoom: z.clamp(0.0, max_zoom(app)) });
            app.map.fit_pending = false;
            apply_view_area(app);
            Ok(state(app))
        }
        "map.state" => {
            refresh_points(app);
            Ok(state(app))
        }
        "map.lockMarkers" => {
            // {on?}: without it, toggles
            let on = match p.get("on") {
                None | Some(Value::Null) => !app.ui.map_lock_markers,
                Some(v) => match v.as_bool() {
                    Some(b) => b,
                    None => return Some(Err("map.lockMarkers: `on` is true or false".into())),
                },
            };
            app.ui.map_lock_markers = on;
            Ok(json!({"on": on}))
        }
        "map.locationFilter" => {
            // {mode?: visible|tagged|untagged|none}: without it, reports
            match p.get("mode") {
                None | Some(Value::Null) => {}
                Some(v) => match v.as_str().and_then(LocationFilter::parse) {
                    Some(mode) => {
                        app.map.location = mode;
                        // Visible on Map starts from the view as it is now
                        app.map.applied_area = None;
                        app.map.seen_area = None;
                        if mode == LocationFilter::Visible && app.map.rect.is_some() {
                            apply_view_area(app);
                        }
                        app.map.clusters_for = None;
                        sync_location_filter(app);
                    }
                    None => return Some(Err("map.locationFilter: mode is visible, tagged, untagged or none".into())),
                },
            }
            refresh_points(app);
            let mut s = location_state(app);
            s["shown"] = json!(app.session.visible().len());
            Ok(s)
        }
        _ => return None,
    };
    Some(r)
}

/// What the map shows: `map.state`.
pub fn state(app: &mut LightcraftApp) -> Value {
    let mut s = state_core(app);
    locations::state(app, &mut s);
    tracks::state(app, &mut s);
    s
}

fn state_core(app: &mut LightcraftApp) -> Value {
    let v = viewport(app);
    let origin = app.map.rect.map(|r| r.min).unwrap_or(egui::Pos2::ZERO);
    let selected: HashSet<u64> = app.session.selection.ids.iter().map(|p| p.0).collect();
    let source = app.session.map.source();
    let clusters: Vec<Value> = app
        .map
        .clusters
        .iter()
        .take(MAX_LISTED)
        .map(|c| {
            json!({
                "ids": c.ids,
                "count": c.len(),
                "x": f64::from(origin.x) + c.center.x,
                "y": f64::from(origin.y) + c.center.y,
                "lat": c.pos.lat,
                "lon": c.pos.lon,
                "selected": c.ids.iter().any(|id| selected.contains(id)),
            })
        })
        .collect();
    json!({
        "lat": v.center.lat,
        "lon": v.center.lon,
        "zoom": v.zoom,
        "maxZoom": max_zoom(app),
        "width": v.width,
        "height": v.height,
        "tileZoom": v.tile_zoom(source.max_zoom),
        "source": source.id,
        "offline": source.is_offline(),
        "attribution": source.attribution,
        "withGps": app.map.points.points.len(),
        "withoutGps": app.map.points.without_gps,
        "clusters": clusters,
        "clusterCount": app.map.clusters.len(),
        "clusterMs": app.map.cluster_ms,
        "lockMarkers": app.ui.map_lock_markers,
        "locationFilter": location_state(app),
        "movingPin": app.map.pin_drag.map(|d| json!({"id": d.id, "x": d.at.x, "y": d.at.y})),
        "drop": app.map.drop_at.map(|(n, at)| {
            let p = v.to_lat_lon(Point::new(f64::from(at.x - origin.x), f64::from(at.y - origin.y)));
            json!({"count": n, "x": at.x, "y": at.y, "lat": p.lat, "lon": p.lon})
        }),
        "tiles": {"drawn": app.map.tiles_drawn, "missing": app.map.tiles_missing, "textures": app.map.textures.len(), "uploads": app.map.uploads},
        "miniMap": app.map.mini.as_ref().map(|m| json!({
            "photo": m.photo.0,
            "lat": m.center.lat,
            "lon": m.center.lon,
            "zoom": m.zoom,
            "x": m.rect.min.x,
            "y": m.rect.min.y,
            "width": m.rect.width(),
            "height": m.rect.height(),
            "pin": {"x": m.pin.x, "y": m.pin.y},
            "tiles": {"drawn": m.tiles_drawn, "missing": m.tiles_missing},
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eviction keeps every texture drawn in the current frame, however many there are (a big
    /// window's tiles and their stand-ins), and drops the oldest of the rest.
    #[test]
    fn eviction_never_drops_what_this_frame_drew() {
        let ctx = egui::Context::default();
        let mut m = MapView::default();
        let tex = || ctx.load_texture("t", egui::ColorImage::new([1, 1], vec![Color32::WHITE]), egui::TextureOptions::LINEAR);
        let key = |i: usize| TileKey { z: 12, x: i as u32, y: 0 };
        for i in 0..MAX_TEXTURES + 20 {
            m.textures.insert(key(i), TileTex { tex: tex(), used: 7, from: std::sync::Weak::new() });
        }
        evict(&mut m, 7);
        assert_eq!(m.textures.len(), MAX_TEXTURES + 20, "all drawn this frame: none dropped");
        for i in 0..10 {
            m.textures.insert(key(10_000 + i), TileTex { tex: tex(), used: 3, from: std::sync::Weak::new() });
        }
        evict(&mut m, 7);
        assert_eq!(m.textures.len(), MAX_TEXTURES + 20, "only the older ones go");
        assert!(m.textures.values().all(|t| t.used == 7));
    }
}
