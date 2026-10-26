//! The Map view: the photos in view (current source, filter and sort: the grid's photos) placed by
//! their GPS position on a slippy map, with the filmstrip below.
//!
//! - **Tiles** come from the session's tile service (`lightcraft-map`, see docs/map.md): every
//!   frame the view tells it what it shows ([`TileService::show`], cheap), draws the tiles it has in
//!   memory as egui textures, and polls it for arrivals (a repaint). Each tile is uploaded once and
//!   kept by [`TileKey`] ([`MapView::uploads`] counts uploads); the least recently drawn are dropped
//!   past [`MAX_TEXTURES`]. A tile not loaded yet is drawn from a lower zoom's texture when one is
//!   at hand, else as a grey square. The source's attribution is always painted bottom right.
//! - **Pins**: each photo with a valid GPS position is projected once per change of the photo list
//!   ([`MapPoint`]); clusters are recomputed only when the view moves ([`lightcraft_map::cluster`],
//!   world-anchored cells of [`CLUSTER_CELL`] px). A click selects a pin's photos (⌘ / ⇧ adds),
//!   a double-click opens the first in Detail, hovering shows its thumbnail.
//! - **Moving**: drag to pan, mouse wheel / pinch / ⌘-scroll to zoom around the pointer,
//!   two-finger scroll to pan, double-click the map to zoom in; + / − and Fit to Photos at the top
//!   right. Commands: `view.map`, `map.fit`, `map.setView`, `map.zoomIn`, `map.zoomOut`, `map.state`.
//!
//! The view (centre and zoom) is kept for the session; the first time the map is shown it fits the
//! photos with GPS.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_geom::Point;
use lightcraft_map::{Bounds, CLUSTER_CELL, Cluster, LatLon, MapPoint, TileEvent, TileKey, Viewport, fit_bounds};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::icons::Icon;
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

/// The view's centre and zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub center: LatLon,
    pub zoom: f64,
}

struct TileTex {
    tex: egui::TextureHandle,
    /// Frame it was last drawn in.
    used: u64,
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
    frame: u64,
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
}

/// Deepest zoom the view goes to: the tile source's, or 19 offline.
fn max_zoom(app: &LightcraftApp) -> f64 {
    let s = app.session.map.source();
    if s.is_offline() { OFFLINE_MAX_ZOOM } else { f64::from(s.max_zoom) }
}

/// Re-project the photos when the photo list changed (catalog revision, source, filter, sort).
fn refresh_points(app: &mut LightcraftApp) {
    let (generation, ids) = app.session.visible_shared();
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
    let canvas = Rect::from_min_max(full.min, pos2(full.right(), (full.bottom() - film_h).max(full.top())));
    app.canvas_rect = Some(canvas);
    if film {
        super::detail::filmstrip(app, ui, Rect::from_min_max(pos2(full.left(), canvas.bottom()), full.max));
    }
    map_canvas(app, ui, canvas);
}

fn map_canvas(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    app.map.frame += 1;
    app.map.rect = Some(canvas);
    refresh_points(app);
    let (w, h) = (f64::from(canvas.width()), f64::from(canvas.height()));
    if app.map.camera.is_none() || std::mem::take(&mut app.map.fit_pending) {
        app.map.camera = Some(fit_camera(app, w, h));
    }
    let resp = ui.interact(canvas, egui::Id::unique("map-canvas"), Sense::click_and_drag());
    register(ui, "map:canvas", canvas);
    let local = |p: egui::Pos2| Point::new(f64::from(p.x - canvas.min.x), f64::from(p.y - canvas.min.y));

    // ---- moving the map
    let mut vp = viewport(app);
    if resp.dragged() {
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

    // ---- tiles
    let painter = ui.painter_at(canvas);
    painter.rect_filled(canvas, 0.0, OUTSIDE);
    let offline = app.session.map.source().is_offline();
    let source_max = app.session.map.source().max_zoom;
    let mut attribution = String::new();
    if app.map.textures_source != app.session.map.source().id {
        app.map.textures.clear();
        app.map.textures_source = app.session.map.source().id.clone();
    }
    if !offline {
        let svc = app.session.map_tiles();
        svc.show(&vp);
        let events = if app.map.shadow_pass { Vec::new() } else { svc.poll() };
        let idle = svc.is_idle();
        attribution = svc.source().attribution.clone();
        for e in &events {
            // a fresh copy replaces the texture (uploaded again when next drawn)
            if let TileEvent::Ready { key, .. } = e {
                app.map.textures.remove(key);
            }
        }
        if !events.is_empty() {
            ctx.request_repaint();
        } else if !idle {
            // the service doesn't wake the UI: look again soon while tiles are on their way
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    let frame = app.map.frame;
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
            && !app.map.shadow_pass
            && !app.map.textures.contains_key(&tile.key)
            && let Some(img) = app.session.map_tiles().get(tile.key)
            && let Some(tex) = upload(&ctx, tile.key, &img)
        {
            app.map.textures.insert(tile.key, TileTex { tex, used: frame });
            app.map.uploads += 1;
        }
        if let Some(tt) = app.map.textures.get_mut(&tile.key) {
            tt.used = frame;
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
                tt.used = frame;
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
    app.map.tiles_drawn = drawn;
    app.map.tiles_missing = missing;
    evict(&mut app.map, frame);

    // ---- pins
    let key = (
        app.map.points.generation.unwrap_or(0),
        [vp.center.lat.to_bits(), vp.center.lon.to_bits(), vp.zoom.to_bits(), (vp.width.to_bits() ^ vp.height.to_bits().rotate_left(32))],
    );
    if app.map.clusters_for != Some(key) {
        let t0 = crate::now_ms();
        app.map.clusters = lightcraft_map::cluster(&app.map.points.points, &vp, CLUSTER_CELL);
        app.map.cluster_ms = crate::now_ms() - t0;
        app.map.clusters_for = Some(key);
    }
    let selected: HashSet<u64> = app.session.selection.ids.iter().map(|p| p.0).collect();
    let active = app.session.selection.active.map(|a| a.0);
    let to_screen = |c: &Cluster| pos2(canvas.min.x + c.center.x as f32, canvas.min.y + c.center.y as f32);
    let hover = resp.hover_pos().filter(|_| !resp.dragged()).map(local);
    let hovered = hover.and_then(|p| lightcraft_map::cluster::hit(&app.map.clusters, p, |c| f64::from(radius(c.len())) + 3.0));
    let mut listed = HashSet::new();
    for (i, c) in app.map.clusters.iter().enumerate() {
        let at = to_screen(c);
        let n = c.len();
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
    if hovered.is_some() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
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
            None if resp.double_clicked() => {
                let v = viewport(app);
                set_viewport(app, v.zoomed_around(p, (v.zoom + 1.0).min(max_zoom(app)).floor()));
                ctx.request_repaint();
            }
            None => {}
        }
    }

    // ---- hover thumbnail
    if let Some(c) = hovered_cluster
        && let Some(&first) = c.ids.first()
    {
        thumbnail(app, ui, &painter, to_screen(&c), radius(c.len()), PhotoId(first), c.len());
    }

    // ---- overlays: notes, controls, attribution
    let n_gps = app.map.points.points.len();
    let without = app.map.points.without_gps;
    if n_gps == 0 {
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
    if !attribution.is_empty() {
        // always visible, never behind a toggle (the tile provider's terms)
        let g = painter.layout_no_wrap(attribution, t.font(11.0), Color32::from_gray(0x20));
        let r = Rect::from_min_size(pos2(canvas.right() - g.size().x - 12.0, canvas.bottom() - g.size().y - 6.0), g.size() + vec2(8.0, 4.0));
        painter.rect_filled(r, 2.0, Color32::from_white_alpha(200));
        painter.galley(r.min + vec2(4.0, 2.0), g, Color32::from_gray(0x20));
        register(ui, "map:attribution", r);
    }
    controls(app, ui, canvas);
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

/// + / − and Fit to Photos, top right.
fn controls(app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect) {
    let t = Tokens::get(ui.ctx());
    let fit_label = crate::i18n::tr("Fit to Photos").to_string();
    let fit_w = ui.painter().layout_no_wrap(fit_label.clone(), t.font(12.5), t.text).size().x + 20.0;
    let bar = Rect::from_min_size(pos2(canvas.right() - 12.0 - (fit_w + 6.0 + 64.0), canvas.top() + 12.0), vec2(fit_w + 6.0 + 64.0, 30.0));
    if bar.left() < canvas.left() || bar.bottom() > canvas.bottom() {
        return;
    }
    let p = ui.painter();
    let zoom_r = Rect::from_min_size(pos2(bar.right() - 64.0, bar.top()), vec2(64.0, 30.0));
    p.rect_filled(zoom_r, 4.0, t.chrome.gamma_multiply(0.92));
    let fit_r = Rect::from_min_size(bar.min, vec2(fit_w, 30.0));
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
    let r = match id {
        "view.map" => {
            app.ui.view = crate::state::ViewMode::Map;
            refresh_points(app);
            Ok(json!({"withGps": app.map.points.points.len(), "withoutGps": app.map.points.without_gps}))
        }
        "map.fit" => {
            refresh_points(app);
            match app.map.rect {
                Some(r) => app.map.camera = Some(fit_camera(app, f64::from(r.width()), f64::from(r.height()))),
                None => app.map.fit_pending = true,
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
            Ok(state(app))
        }
        "map.zoomIn" | "map.zoomOut" => {
            let v = viewport(app);
            let step = if id == "map.zoomIn" { 1.0 } else { -1.0 };
            let z = if step > 0.0 { (v.zoom + 1.0).floor() } else { (v.zoom - 1.0).ceil() };
            app.map.camera = Some(Camera { center: v.center, zoom: z.clamp(0.0, max_zoom(app)) });
            app.map.fit_pending = false;
            Ok(state(app))
        }
        "map.state" => {
            refresh_points(app);
            Ok(state(app))
        }
        _ => return None,
    };
    Some(r)
}

/// What the map shows: `map.state`.
pub fn state(app: &mut LightcraftApp) -> Value {
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
        "tiles": {"drawn": app.map.tiles_drawn, "missing": app.map.tiles_missing, "textures": app.map.textures.len(), "uploads": app.map.uploads},
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
            m.textures.insert(key(i), TileTex { tex: tex(), used: 7 });
        }
        evict(&mut m, 7);
        assert_eq!(m.textures.len(), MAX_TEXTURES + 20, "all drawn this frame: none dropped");
        for i in 0..10 {
            m.textures.insert(key(10_000 + i), TileTex { tex: tex(), used: 3 });
        }
        evict(&mut m, 7);
        assert_eq!(m.textures.len(), MAX_TEXTURES + 20, "only the older ones go");
        assert!(m.textures.values().all(|t| t.used == 7));
    }
}
