//! The Detail (loupe) view: the developed photo, before/after, zoom & pan, the filmstrip, and the
//! on-canvas tools (crop, brush/gradient masks, remove spots, white-balance picker).

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lcc_catalog::PhotoId;
use lcc_develop::{DevelopSettings, MaskShape};
use lcc_geom::{Affine, Point};
use lcc_pipeline::geometry::Frame;
use serde_json::json;

use crate::LccApp;
use crate::render::Slot;
use crate::state::{BeforeAfter, RightPanel, ToolOverlay, Zoom};
use crate::theme::Tokens;
use crate::widgets::register;

/// A clip's whole decoded frame drawn into `r` through the loupe's [`Frame`]: the crop, its
/// straighten angle and the flips pick the part of the texture that shows (the uncropped frame
/// while the Crop tool is open).
fn video_frame_mesh(tex: egui::TextureId, r: Rect, frame: &Frame) -> egui::Shape {
    let (w, h) = ((r.width().round() as usize).max(1), (r.height().round() as usize).max(1));
    let to_uv = frame.out_to_norm(w, h);
    let mut mesh = egui::epaint::Mesh::with_texture(tex);
    for (x, y) in [(0.0, 0.0), (w as f64, 0.0), (w as f64, h as f64), (0.0, h as f64)] {
        let uv = to_uv.apply(Point::new(x, y));
        let pos = pos2(r.left() + (x / w as f64) as f32 * r.width(), r.top() + (y / h as f64) as f32 * r.height());
        mesh.vertices.push(egui::epaint::Vertex { pos, uv: pos2(uv.x as f32, uv.y as f32), color: Color32::WHITE });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    egui::Shape::mesh(mesh)
}

/// An in-progress on-canvas gesture.
#[derive(Clone, Debug)]
pub enum Gesture {
    Brush {
        points: Vec<Point>,
        /// Pen pressure (0..1) at each point; empty when no pen pressure was reported (a mouse) or the
        /// brush's Pen Pressure switch is off.
        pressure: Vec<f32>,
    },
    Spot {
        points: Vec<Point>,
    },
    /// Remove tool: dragging spot `spot`'s target (or its source).
    SpotMove {
        spot: usize,
        source: bool,
    },
    /// Crop tool: resizing by handle `handle` (0..7), or moving the photo under the box (8).
    CropHandle {
        handle: u8,
        start: lcc_geom::Rect,
        angle: f64,
        /// The view as the drag started: pointer positions mean photo points through it.
        map: CanvasMap,
    },
    /// Straighten tool: a line drawn along something that should be level (or plumb).
    StraightenLine {
        a: Pos2,
    },
    CropRotate {
        start_angle: f64,
        a0: f32,
    },
    Pan {
        start: (f32, f32),
        at: Pos2,
    },
    /// Dragging component `comp` of `mask`: its pin (`handle` 0) or a linear gradient's start (1)
    /// or end (2).
    MaskHandle {
        mask: u32,
        comp: usize,
        handle: u8,
        original_shape: MaskShape,
    },
    /// Guided Upright: a guide being drawn from `a` (normalized transformed coordinates).
    Guide {
        a: Point,
    },
    /// Red Eye: an ellipse being dragged from corner `a` (normalized).
    Eye {
        a: Point,
    },
    /// Targeted adjustment drag from `at` (normalized); `acc` = vertical travel not yet applied.
    Targeted {
        at: Point,
        acc: f32,
    },
    /// A drag of one of the histogram's tonal zones (see [`super::histogram`]): the control the
    /// press locked and the unrounded value its sideways travel has reached. It holds no canvas
    /// geometry: the zone stays locked wherever the pointer goes.
    HistogramDrag {
        control: &'static str,
        value: f64,
    },
}

/// Screen ↔ normalized-image mapping for the displayed frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasMap {
    rect: Rect,
    to_norm: Affine,
    from_norm: Affine,
    k: f32,
}

impl CanvasMap {
    fn new(frame: &Frame, rect: Rect) -> CanvasMap {
        let k = 8.0f32;
        let (w, h) = ((rect.width() * k).round().max(1.0) as usize, (rect.height() * k).round().max(1.0) as usize);
        let to_norm = frame.out_to_norm(w, h);
        CanvasMap { rect, to_norm, from_norm: to_norm.inverse().unwrap_or(Affine::IDENTITY), k }
    }
    pub fn norm(&self, p: Pos2) -> Point {
        self.to_norm.apply(Point::new(((p.x - self.rect.left()) * self.k) as f64, ((p.y - self.rect.top()) * self.k) as f64))
    }
    pub fn screen(&self, n: Point) -> Pos2 {
        let q = self.from_norm.apply(n);
        pos2(self.rect.left() + q.x as f32 / self.k, self.rect.top() + q.y as f32 / self.k)
    }
}

/// How the crop tool holds its view still while the crop changes (issue #60). As in Lightroom the
/// crop box stays upright on screen and the photo turns and moves beneath it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CropLayout {
    /// Straightening, rotating or moving: the box keeps its centre on screen (`centre`) and the
    /// scale (`k` points per pixel of the oriented photo); the photo turns and slides under it.
    Pinned { centre: Pos2, k: f32 },
    /// Resizing: the photo stays where it is and the box follows the handle.
    Frozen(CanvasMap),
}

/// The crop tool's view of the photo: the crop box upright, the whole photo around it, turned by
/// the straighten angle.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CropView {
    /// Normalized photo ↔ screen, through the crop box (upright on screen).
    pub map: CanvasMap,
    /// The photo's corners on screen (top-left, top-right, bottom-right, bottom-left of the
    /// uncropped render).
    pub photo: [Pos2; 4],
    /// The crop box's centre on screen, and screen points per pixel of the oriented photo.
    pub centre: Pos2,
    pub k: f32,
}

impl CropView {
    /// The photo's bounding box on screen.
    pub fn bounds(&self) -> Rect {
        Rect::from_points(&self.photo)
    }
}

/// Lay the crop tool's view out in `area`: held as `layout` says while the crop is being changed,
/// otherwise the turned photo fitted into `area`. `frame` is the uncropped frame the loupe renders.
pub(crate) fn crop_view(frame: &Frame, crop: lcc_geom::CropGeometry, area: Rect, layout: Option<CropLayout>) -> CropView {
    let cf = Frame { crop, ..frame.clone() };
    let (nw, nh) = cf.native_size();
    let (nw, nh) = ((nw as f32).max(1e-3), (nh as f32).max(1e-3));
    // the uncropped render's corners, as normalized photo points (its flips included)
    let uv_to_norm = frame.out_to_norm(1, 1);
    let corners = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(u, v)| uv_to_norm.apply(Point::new(u, v)));
    let finite = |k: f32| k.is_finite() && k > 0.0;
    let map = match layout {
        Some(CropLayout::Frozen(m)) => m,
        _ => {
            let (centre, k) = match layout {
                Some(CropLayout::Pinned { centre, k }) if centre.is_finite() && finite(k) => (centre, k),
                _ => {
                    // the turned photo at one point per pixel around a box centred on the origin,
                    // scaled and placed to fit the area
                    let unit = CanvasMap::new(&cf, Rect::from_center_size(Pos2::ZERO, vec2(nw, nh)));
                    let b = Rect::from_points(&corners.map(|c| unit.screen(c)));
                    let k = (area.width() / b.width()).min(area.height() / b.height());
                    let k = if finite(k) { k } else { 1.0 };
                    (area.center() - b.center().to_vec2() * k, k)
                }
            };
            CanvasMap::new(&cf, Rect::from_center_size(centre, vec2(nw * k, nh * k)))
        }
    };
    let photo = corners.map(|c| map.screen(c));
    let quad = cf.crop_quad_norm().map(|c| map.screen(c));
    let centre = pos2(quad.iter().map(|q| q.x).sum::<f32>() / 4.0, quad.iter().map(|q| q.y).sum::<f32>() / 4.0);
    let k = quad[0].distance(quad[1]) / nw;
    CropView { map, photo, centre, k: if finite(k) { k } else { 1.0 } }
}

/// The uncropped render drawn turned and placed as the crop tool's view lays it out.
fn crop_view_mesh(tex: egui::TextureId, view: &CropView) -> egui::Shape {
    let mut mesh = egui::epaint::Mesh::with_texture(tex);
    for (pos, uv) in view.photo.iter().zip([(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]) {
        mesh.vertices.push(egui::epaint::Vertex { pos: *pos, uv: pos2(uv.0, uv.1), color: Color32::WHITE });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    egui::Shape::mesh(mesh)
}

pub(crate) fn fit_rect(area: Rect, aspect: f32, zoom: Zoom, img_px: [usize; 2], ppp: f32, pan: (f32, f32)) -> Rect {
    let zoom = match zoom {
        Zoom::Percent(p) if !p.is_finite() || p <= 0.0 => Zoom::Fit,
        _ => zoom,
    };
    let (aw, ah) = (area.width(), area.height());
    let (w, h) = match zoom {
        Zoom::Fit => {
            if aw / ah > aspect {
                (ah * aspect, ah)
            } else {
                (aw, aw / aspect)
            }
        }
        Zoom::Fill => {
            if aw / ah > aspect {
                (aw, aw / aspect)
            } else {
                (ah * aspect, ah)
            }
        }
        Zoom::Percent(p) => {
            // the photo's own pixels at p% (its width as shown: cropped and rotated)
            let w = img_px[0] as f32 * p / 100.0 / ppp;
            (w, w / aspect)
        }
    };
    let pan = bounded_pan(area, vec2(w, h), pan);
    let c = pos2(area.center().x - (pan.0 - 0.5) * w, area.center().y - (pan.1 - 0.5) * h);
    let rect = Rect::from_center_size(c, vec2(w, h));
    // At 100 % the texture maps texel for texel to screen pixels, so its corner must sit on a whole
    // physical pixel: half a pixel off and the GPU's bilinear filter blurs the view (an odd-width
    // canvas on a 2x display does this). Other zooms resample anyway, and a pinch must not jump.
    let one_to_one = ppp.is_finite() && ppp > 0.0 && (w * ppp - img_px[0] as f32).abs() < 0.5;
    if matches!(zoom, Zoom::Percent(_)) && one_to_one {
        let snap = |v: f32| (v * ppp).round() / ppp;
        return Rect::from_min_size(pos2(snap(rect.min.x), snap(rect.min.y)), rect.size());
    }
    rect
}

/// Fit a decoded image inside the frame calculated from catalog metadata. Unsupported RAWs can
/// only show their embedded camera JPEG, whose crop and aspect ratio can differ from that metadata.
pub(crate) fn fit_texture_rect(area: Rect, size: [usize; 2]) -> Rect {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let scale = (area.width() / w).min(area.height() / h);
    Rect::from_center_size(area.center(), vec2(w * scale, h * scale))
}

/// Keep the viewport inside the photo; axes smaller than the viewport stay centred.
fn bounded_pan(area: Rect, size: egui::Vec2, pan: (f32, f32)) -> (f32, f32) {
    let bound = |visible: f32, image: f32, centre: f32| {
        let centre = if centre.is_finite() { centre } else { 0.5 };
        if image > visible.max(0.0) {
            let half = visible.max(0.0) / image / 2.0;
            centre.clamp(half, 1.0 - half)
        } else {
            0.5
        }
    };
    (bound(area.width(), size.x, pan.0), bound(area.height(), size.y, pan.1))
}

pub(crate) fn pan_image(app: &mut LccApp, area: Rect, img: Rect, delta: egui::Vec2) {
    let centre = area.center() - img.min - delta;
    let pan = bounded_pan(area, img.size(), (centre.x / img.width().max(1.0), centre.y / img.height().max(1.0)));
    let _ = app.run("view.navigate", json!({"pan": pan}));
}

/// Net vertical notches (up is positive) of unmodified line/page-unit wheel events: a real mouse wheel, not a trackpad.
fn wheel_notches(events: &[egui::Event]) -> f32 {
    events
        .iter()
        .filter_map(|e| match e {
            egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page, delta, modifiers, .. }
                if *modifiers == egui::Modifiers::NONE && delta.y.is_finite() =>
            {
                Some(delta.y)
            }
            _ => None,
        })
        .sum()
}

/// Native pinch (also modifier-wheel zoom) and two-finger scroll, scoped to this image view.
pub(crate) fn navigate_gesture(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, area: Rect, img: Rect, native: [usize; 2]) -> bool {
    if !resp.hovered() {
        return false;
    }
    let (mut factor, mut delta, anchor) =
        ui.input(|i| (i.zoom_delta(), i.translation_delta(), i.multi_touch().map(|t| t.center_pos).or(i.pointer.hover_pos())));

    let notches = ui.input(|i| wheel_notches(&i.events));
    // With the Masking brush active, a plain wheel notch resizes the brush (like `[` / `]`) instead of zooming
    // (Preferences → Interface); Ctrl/Cmd+wheel still zooms.
    if app.ui.settings.wheel_resizes_brush && notches != 0.0 && app.ui.tool == "brush" {
        crate::menus::adjust_brush(app, 1.0 + (notches * 0.1).clamp(-0.5, 0.5), 0.0);
        ui.ctx().request_repaint();
        return true;
    }

    // A notched mouse wheel (line/page units, no modifier) zooms around the pointer, through egui's smoothed wheel
    // delta like Ctrl+wheel (so it glides instead of jumping); trackpads report points and still pan.
    let wheel_id = egui::Id::new("loupe-wheel-zoom");
    if notches != 0.0 {
        ui.data_mut(|d| d.insert_temp(wheel_id, true));
    }
    if ui.data(|d| d.get_temp::<bool>(wheel_id)).unwrap_or(false) {
        let smooth = ui.input(|i| i.smooth_scroll_delta.y);
        let speed = ui.ctx().options(|o| o.input_options.scroll_zoom_speed);
        if smooth == 0.0 {
            ui.data_mut(|d| d.remove_temp::<bool>(wheel_id));
        } else {
            factor *= (speed * smooth).exp();
            delta = egui::Vec2::ZERO;
        }
    }
    if !factor.is_finite() || factor <= 0.0 || !delta.is_finite() || (factor == 1.0 && delta == egui::Vec2::ZERO) {
        return false;
    }
    let Some(anchor) = anchor.filter(|p| area.contains(*p)) else { return false };
    let ppp = ui.ctx().pixels_per_point();
    let aspect = img.width() / img.height().max(1.0);
    let fit = fit_rect(area, aspect, Zoom::Fit, native, ppp, (0.5, 0.5));
    let fit_pct = fit.width() * ppp * 100.0 / native[0].max(1) as f32;
    let mut zoom = app.ui.zoom;
    let size = if factor != 1.0 {
        // Start at the displayed scale (including Fit/Fill and an interrupted click animation).
        let pct = (img.width() * ppp * 100.0 / native[0].max(1) as f32 * factor).clamp(fit_pct.min(crate::state::MAX_ZOOM), crate::state::MAX_ZOOM);
        zoom = if pct <= fit_pct { Zoom::Fit } else { Zoom::Percent(pct) };
        fit_rect(area, aspect, zoom, native, ppp, (0.5, 0.5)).size()
    } else {
        img.size()
    };
    // The image point under the pointer remains there as its size changes, then follows the pan.
    let point = (anchor - img.min) / img.size();
    let offset = area.center() - anchor - delta;
    let pan = bounded_pan(area, size, (point.x + offset.x / size.x.max(1.0), point.y + offset.y / size.y.max(1.0)));
    let _ = app.run("view.navigate", json!({"zoom": zoom, "pan": pan}));
    ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    ui.ctx().request_repaint();
    true
}

/// The largest texture the GPU behind `ctx` takes (egui reports it; 2048 until it does, the
/// smallest limit WebGL devices have).
///
/// The value egui keeps, not the frame's raw input: a native host (egui-winit) reports the limit
/// in the first frame's input only, so the raw value is `None` from the second frame on
/// (issue #652: every window render was cut to 2048 px, with blurry strips beside it).
pub(crate) fn texture_side(ctx: &egui::Context) -> usize {
    ctx.input(|i| i.max_texture_side)
}

/// The photo's own pixels as it is shown: its size after the crop and the user's rotation (what
/// 100 % zoom and the render size limit are measured against), at least 1 × 1.
pub(crate) fn output_px(frame: &Frame) -> [usize; 2] {
    let (w, h) = frame.native_size();
    let px = |v: f64| if v.is_finite() { v.round().clamp(1.0, 1e9) as usize } else { 1 };
    [px(w), px(h)]
}

/// The camera's focus point of `photo` in the displayed frame (its crop, rotation and flips), as the
/// normalized box over the drawn image: `None` when the photo has none or it is cropped away.
pub(crate) fn focus_box(photo: &lcc_catalog::Photo) -> Option<lcc_geom::Rect> {
    let f = photo.meta.focus.as_ref()?;
    let d = &photo.develop;
    let frame = Frame::with_lens(photo.width.max(1) as usize, photo.height.max(1) as usize, d, true, photo.embedded_lens.as_deref());
    let [w, h] = output_px(&frame);
    let to_out = frame.out_to_norm(w, h).inverse()?;
    // stored on the upright photo: the user's Rotate / Flip on top, then the crop
    let r = d.orientation.map_norm_rect(f.rect);
    let corners = [Point::new(r.x0, r.y0), Point::new(r.x1, r.y0), Point::new(r.x0, r.y1), Point::new(r.x1, r.y1)].map(|c| {
        let q = to_out.apply(c);
        (q.x / w as f64, q.y / h as f64)
    });
    let xs = corners.map(|c| c.0);
    let ys = corners.map(|c| c.1);
    let out = lcc_geom::Rect {
        x0: xs.iter().copied().fold(f64::INFINITY, f64::min),
        y0: ys.iter().copied().fold(f64::INFINITY, f64::min),
        x1: xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        y1: ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    };
    if ![out.x0, out.y0, out.x1, out.y1].iter().all(|v| v.is_finite()) {
        return None;
    }
    // its centre must be in the frame; the box may reach past the edge (it is clipped when drawn)
    let (cx, cy) = ((out.x0 + out.x1) / 2.0, (out.y0 + out.y1) / 2.0);
    ((0.0..=1.0).contains(&cx) && (0.0..=1.0).contains(&cy)).then_some(out)
}

/// Where Toggle Zoom centres the view on the photo's focus point (`app.ui.pan`), if it has one in frame.
pub(crate) fn focus_pan(app: &LccApp, id: PhotoId) -> Option<(f32, f32)> {
    let r = focus_box(app.session.catalog.photo(id)?)?;
    Some((((r.x0 + r.x1) / 2.0) as f32, ((r.y0 + r.y1) / 2.0) as f32))
}

/// Draw the camera's focus box over a photo drawn in `img` (View ▸ Show Focus Point): green corner
/// brackets with a dark keyline, the way cameras show their AF frame, never smaller than 14 pt.
/// `key` names it for tests and the control channel (`canvas:focusPoint`, `compare:focusPoint:<id>`).
pub(crate) fn draw_focus_point(p: &egui::Painter, img: Rect, photo: &lcc_catalog::Photo, key: &str) {
    let Some(r) = focus_box(photo) else { return };
    let at = |x: f64, y: f64| pos2(img.left() + x as f32 * img.width(), img.top() + y as f32 * img.height());
    let b = Rect::from_two_pos(at(r.x0, r.y0), at(r.x1, r.y1));
    let b = Rect::from_center_size(b.center(), b.size().max(vec2(14.0, 14.0)));
    let arm = (b.width().min(b.height()) * 0.3).max(5.0);
    let green = Color32::from_rgb(90, 230, 110);
    for (c, dx, dy) in [(b.left_top(), 1.0, 1.0), (b.right_top(), -1.0, 1.0), (b.left_bottom(), 1.0, -1.0), (b.right_bottom(), -1.0, -1.0)] {
        let pts = vec![c + vec2(dx * arm, 0.0), c, c + vec2(0.0, dy * arm)];
        p.add(egui::Shape::line(pts.clone(), Stroke::new(4.0, Color32::from_black_alpha(170))));
        p.add(egui::Shape::line(pts, Stroke::new(2.0, green)));
    }
    let c = b.center();
    p.circle_filled(c, 2.5, Color32::from_black_alpha(170));
    p.circle_filled(c, 1.5, green);
    register(p.ctx(), key, b);
}

/// Ease the loupe rect toward `target` while a click-zoom animation runs; otherwise follow it exactly.
fn animated_rect(ctx: &egui::Context, anim: &mut bool, target: Rect) -> Rect {
    let t = if *anim { 0.22 } else { 0.0 };
    let id = egui::Id::new("loupe_anim");
    let v = |k: &str, x: f32| ctx.animate_value_with_time(id.with(k), x, t);
    let (c, s) = (target.center(), target.size());
    let r = Rect::from_center_size(pos2(v("cx", c.x), v("cy", c.y)), vec2(v("w", s.x), v("h", s.y)));
    if *anim && (r.center() - c).abs().max_elem() < 0.5 && (r.size() - s).abs().max_elem() < 0.5 {
        *anim = false;
    }
    r
}

/// What the tiles of a zoomed view (the loupe, a Compare / Reference photo) need to know about
/// the view they belong to.
pub(crate) struct WindowCtx {
    pub(crate) id: PhotoId,
    /// Long edge of the frame the tiles are cut from (`None`: no tiles).
    pub(crate) frame_edge: Option<usize>,
    pub(crate) aspect: f32,
    pub(crate) texture_side: usize,
    pub(crate) crop_tool: bool,
    pub(crate) interacting: bool,
    /// Hash of the photo's develop settings.
    pub(crate) look: u64,
    /// A pinch or two-finger scroll is running: keep the tiles there are.
    pub(crate) holding: bool,
    /// What the whole-frame render shows over the photo (a mask, a range…): the tiles show it too.
    pub(crate) overlay: lcc_pipeline::Overlay,
    pub(crate) proof: Option<lcc_pipeline::Proof>,
    /// The photo's 1:1 preview, when held: tiles are cut from it until they are rendered.
    pub(crate) full_preview: Option<std::sync::Arc<lcc_raster::Rgba8>>,
}

/// One pane of a zoomed view that gets tiles.
#[derive(Clone, Copy)]
pub(crate) struct WindowView {
    /// The pane ([`crate::render::PANE_AFTER`]…): its tiles and drag window are its own.
    pub(crate) pane: u8,
    /// The Before side (the photo without its edits).
    pub(crate) before: bool,
    /// The rect the photo is drawn in.
    pub(crate) img: Rect,
    /// The rect it is requested for (the drawn rect without a click-zoom animation).
    pub(crate) target: Rect,
    /// The area whose pixels are on screen.
    pub(crate) visible: Rect,
}

/// Priority of on-screen tiles (just below the whole-frame render) and of the ring around them.
const TILE_PRIORITY: u32 = 99;
const TILE_RING_PRIORITY: u32 = 70;

/// What of a `full` frame drawn in `rect` (points) is inside `visible`, in frame pixels.
fn visible_px(full: (usize, usize), rect: Rect, visible: Rect) -> (f32, f32, f32, f32) {
    let sx = full.0 as f32 / rect.width().max(1e-3);
    let sy = full.1 as f32 / rect.height().max(1e-3);
    (
        sx * (visible.left() - rect.left()),
        sy * (visible.top() - rect.top()),
        sx * (visible.right() - rect.left()),
        sy * (visible.bottom() - rect.top()),
    )
}

/// Ask for the tiles of `v` that are on screen (and a ring around them), nearest the centre first;
/// put a 1:1 preview's part in place of each until it is rendered. `None`: no tiles are wanted (or
/// possible) for this view. `wanted` collects the slots asked for; `prev` is the view of the
/// last frame (kept while a pinch or scroll runs).
///
/// While a slider drags, the tiles are not drafted one by one (each its own job with no stage
/// cache: every tick redid every stage once per tile, 10-15x slower than one window): the window
/// on screen is drafted as one job in the pane's [`Slot::Window`], whose stage cache the drag
/// reuses, and is drawn over the tiles until the tiles of the final look arrive.
pub(crate) fn request_window(
    app: &mut LccApp,
    c: &WindowCtx,
    v: &WindowView,
    prev: Option<crate::region::RegionView>,
    wanted: &mut Vec<Slot>,
    ctx: &egui::Context,
) -> Option<crate::region::RegionView> {
    let frame_edge = c.frame_edge?;
    // while a pinch or scroll runs the tiles there are keep being drawn, magnified: none asked for
    // per frame (zooming out would ask for ever more)
    if c.holding {
        return prev.filter(|w| w.photo == c.id);
    }
    let (fw, fh) =
        if c.aspect >= 1.0 { (frame_edge as f32, frame_edge as f32 / c.aspect) } else { (frame_edge as f32 * c.aspect, frame_edge as f32) };
    let full = (fw.round().max(1.0) as usize, fh.round().max(1.0) as usize);
    let side = crate::region::tile_side(c.texture_side);
    // (a ring of tiles around the view is prepared for a pan, but not drafted in a slider drag)
    let tiles = crate::region::tiles_for(full, side, visible_px(full, v.target, v.visible), if c.interacting { 0 } else { 1 });
    if !tiles.iter().any(|t| t.1) {
        return None;
    }
    // the look every tile of this view shares: the key of a job for a fixed window (photo, side,
    // frame size, settings, overlay, proof and quality all go into it)
    let job_for = |app: &mut LccApp, win: lcc_engine::pipeline::PixelWindow| {
        let job = if v.before {
            app.session.region_job_before(c.id, full.0, full.1, win, !c.crop_tool)
        } else {
            app.session.region_job(c.id, full.0, full.1, win, !c.crop_tool)
        }?;
        let job = if c.interacting { job.draft() } else { job };
        let job = if v.before { job } else { job.with_hidden_grading(app.ui.grading_hidden_for(c.id)) };
        let (overlay, proof) = if v.before { (lcc_pipeline::Overlay::None, None) } else { (c.overlay, c.proof) };
        Some(job.with_overlay(overlay).with_proof(proof))
    };
    let probe = lcc_engine::pipeline::PixelWindow { x: 0, y: 0, w: 1, h: 1 };
    let Some(look) = job_for(app, probe).map(|j| j.key) else {
        app.window_refused = Some((c.id, c.look, frame_edge));
        return None;
    };
    if c.interacting {
        let window = crate::region::window_for(full.0, full.1, visible_px(full, v.target, v.visible), crate::region::max_span(c.texture_side))?;
        let Some(job) = job_for(app, window) else {
            app.window_refused = Some((c.id, c.look, frame_edge));
            return None;
        };
        let geom = crate::render::WindowGeom { photo: c.id, full, window, look: c.look };
        app.renderer.request_window(v.pane, job, geom, TILE_PRIORITY);
        wanted.push(Slot::Window(v.pane));
        return Some(crate::region::RegionView { photo: c.id, before: v.before, key: look, full, window, settings: look, tile: side });
    }
    // the original is decoded once: until it is held, one tile at a time asks for it (each worker
    // would decode its own copy), the After side first
    let original_held = app.session.media.has_source(c.id, lcc_engine::SourceLevel::for_size(full.0.max(full.1)));
    let mut budget = if original_held {
        usize::MAX
    } else if app.renderer.tiles_pending() > 0 || (v.before && app.region_view.is_some_and(|w| w.photo == c.id)) {
        0
    } else {
        1
    };
    let placeholder =
        c.full_preview.as_ref().filter(|p| !v.before && c.overlay == lcc_pipeline::Overlay::None && c.proof.is_none() && (p.width, p.height) == full);
    let mut cover: Option<(usize, usize, usize, usize)> = None;
    for &(tile, on_screen) in &tiles {
        let Some(rect) = crate::region::tile_rect(full, side, tile) else { continue };
        let key = crate::region::TileKey { photo: c.id, pane: v.pane, full, tile, look };
        if on_screen {
            let (x1, y1) = (rect.x + rect.w, rect.y + rect.h);
            cover = Some(cover.map_or((rect.x, rect.y, x1, y1), |(a, b, cc, d)| (a.min(rect.x), b.min(rect.y), cc.max(x1), d.max(y1))));
            if let Some(p) = placeholder
                && !c.interacting
                && let Some(part) = crate::region::crop_tile(p, lcc_engine::pipeline::PixelWindow { x: 0, y: 0, w: p.width, h: p.height }, rect)
            {
                app.renderer.tile_placeholder(ctx, key, &part);
            }
        }
        if app.renderer.tiles.has_render(&key) {
            continue;
        }
        let slot = Slot::Tile { pane: v.pane, col: tile.col, row: tile.row };
        if budget == 0 {
            continue;
        }
        let window = crate::region::tile_render_window(full, rect);
        // refused: what the tile reads (a spot's source, an Auto Mask stroke) is too far for one
        // render, so the whole frame is rendered at the drawn size instead (see `region::plan`)
        let Some(job) = job_for(app, window) else {
            app.window_refused = Some((c.id, c.look, frame_edge));
            return None;
        };
        budget -= 1;
        wanted.push(slot);
        app.renderer.request_tile(key, job, window, rect, if on_screen { TILE_PRIORITY } else { TILE_RING_PRIORITY });
    }
    let (x0, y0, x1, y1) = cover?;
    let window = lcc_engine::pipeline::PixelWindow { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
    Some(crate::region::RegionView { photo: c.id, before: v.before, key: look, full, window, settings: look, tile: side })
}

/// Draw the tiles of `view` that are on screen over the whole-frame render: each its own render,
/// or (in a drag) the newest of its place, or its part of the 1:1 preview. The pane's drag window
/// goes under them while it shows the current settings, and alone while a slider drags.
pub(crate) fn draw_window(p: &egui::Painter, app: &mut LccApp, c: &WindowCtx, v: &WindowView, view: &crate::region::RegionView) {
    let full = view.full;
    let (fw, fh) = (full.0 as f32, full.1 as f32);
    let place = |w: lcc_engine::pipeline::PixelWindow| {
        let at = v.img.min + vec2(w.x as f32 / fw * v.img.width(), w.y as f32 / fh * v.img.height());
        Rect::from_min_size(at, vec2(w.w as f32 / fw * v.img.width(), w.h as f32 / fh * v.img.height()))
    };
    let mut drag_drawn = false;
    if let Some((tex, g)) = app.renderer.window_texture(v.pane)
        && g.photo == c.id
        && g.full == full
        && (c.interacting || g.look == c.look)
    {
        p.image(tex.tex.id(), place(g.window), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        drag_drawn = true;
    }
    if drag_drawn && c.interacting {
        // the tiles there are show an earlier value of the slider
        return;
    }
    for (tile, _) in crate::region::tiles_for(full, view.tile, visible_px(full, v.img, v.visible), 0) {
        let Some(rect) = crate::region::tile_rect(full, view.tile, tile) else { continue };
        let key = crate::region::TileKey { photo: c.id, pane: v.pane, full, tile, look: view.settings };
        let Some(tex) = app.renderer.tiles.draw(&key, c.interacting || c.holding) else { continue };
        let dst = place(rect);
        p.image(tex.tex.id(), dst, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
}

pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let full = ui.max_rect();
    let fullscreen = app.ui.fullscreen;
    let show_film = crate::module::edge_visible(app, crate::module::Edge::Bottom) && !fullscreen;
    let film_h = if show_film { t.film_h } else { 0.0 };
    let mut canvas = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - film_h));
    // a video: its control bar along the bottom of the canvas (photos keep the whole canvas)
    let mut video_state = None;
    if let Some(vid) = app.session.active().filter(|id| crate::video_ui::photo_is_video(app, *id)) {
        let bar = Rect::from_min_max(pos2(canvas.left(), canvas.bottom() - crate::video_ui::BAR_H), canvas.max);
        canvas.max.y = bar.top();
        video_state = crate::video_ui::show_bar(app, ui, bar, vid).map(|s| (vid, s));
    } else {
        crate::video_view::stop(app);
    }
    app.canvas_rect = Some(canvas);
    if show_film {
        // under the canvas and the video bar (the bar doesn't take the filmstrip's room)
        filmstrip(app, ui, Rect::from_min_max(pos2(full.left(), full.bottom() - film_h), full.max));
    }
    let Some(id) = app.session.active() else {
        if super::empty_library::shown(&app.session) {
            super::empty_library::show(app, ui, canvas);
        } else {
            super::empty_message(ui, canvas, "No photo selected", "Choose a photo in the grid or filmstrip");
        }
        return;
    };
    let Some(photo) = app.session.catalog.photo(id).cloned() else { return };
    let d = (*photo.develop).clone();
    // the full-screen preview shows the photo only: no tool overlays
    let right = if fullscreen { RightPanel::None } else { app.ui.right };
    let crop_tool = right == RightPanel::Crop;
    let frame = Frame::with_lens(photo.width.max(1) as usize, photo.height.max(1) as usize, &d, !crop_tool, photo.embedded_lens.as_deref());
    let aspect = frame.aspect() as f32;
    let ppp = ui.ctx().pixels_per_point();
    let area = canvas.shrink(if fullscreen {
        0.0
    } else if crop_tool {
        48.0
    } else {
        24.0
    });
    let native = output_px(&frame);
    let texture_side = texture_side(ui.ctx());
    // two views (before, after): side by side or stacked
    let split = matches!(app.ui.before_after, BeforeAfter::SideBySide | BeforeAfter::TopBottom);
    let split_view = matches!(app.ui.before_after, BeforeAfter::Split | BeforeAfter::SplitTopBottom);
    let areas: Vec<Rect> = match app.ui.before_after {
        BeforeAfter::SideBySide => {
            let half = area.width() / 2.0 - 6.0;
            vec![
                Rect::from_min_size(area.min, vec2(half, area.height())),
                Rect::from_min_size(pos2(area.center().x + 6.0, area.top()), vec2(half, area.height())),
            ]
        }
        BeforeAfter::TopBottom => {
            let half = area.height() / 2.0 - 14.0;
            vec![
                Rect::from_min_size(area.min, vec2(area.width(), half)),
                Rect::from_min_size(pos2(area.left(), area.center().y + 14.0), vec2(area.width(), half)),
            ]
        }
        _ => vec![area],
    };
    let main_area = *areas.last().unwrap_or(&area);
    let visible_texture_aspect = app
        .renderer
        .textures
        .get(&Slot::Main)
        .filter(|t| t.photo == id)
        .or_else(|| app.renderer.textures.get(&Slot::Preview).filter(|t| t.photo == id))
        .or_else(|| app.renderer.thumb(id))
        .map(|tex| tex.size[0].max(1) as f32 / tex.size[1].max(1) as f32);
    // Follow the texture only where it really differs (an unsupported raw's embedded JPEG with
    // another crop): a texture's whole-pixel size is a little off the exact aspect, and the crop
    // tool's overlays need the frame's own geometry.
    let display_aspect = visible_texture_aspect.filter(|a| !crop_tool && (a / aspect - 1.0).abs() > 0.02).unwrap_or(aspect);
    let resp = ui.interact(canvas, egui::Id::new("loupe"), Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr("Photo"));
    let mut target_rect = fit_rect(main_area, display_aspect, app.ui.zoom, native, ppp, app.ui.pan);
    let mut img_rect = animated_rect(ui.ctx(), &mut app.ui.zoom_anim, target_rect);
    let in_pane = |pane: Rect, image: Rect| image.translate(pane.center() - main_area.center());
    let mut navigating = false;
    for &view_area in &areas {
        let view_img = in_pane(view_area, img_rect);
        if navigate_gesture(app, ui, &resp, view_area, view_img, native) {
            navigating = true;
            target_rect = fit_rect(main_area, display_aspect, app.ui.zoom, native, ppp, app.ui.pan);
            img_rect = target_rect;
            break;
        }
    }
    // the crop tool: the crop box upright and the photo turned around it, held still while the crop
    // changes so the photo moves under the box (issue #60)
    if !crop_tool || app.session.interaction.is_none() {
        app.crop_layout = None;
    }
    let crop_view = crop_tool.then(|| crop_view(&frame, d.crop.geometry, main_area, app.crop_layout));
    if let Some(v) = &crop_view {
        if app.session.interaction.is_some() && app.crop_layout.is_none() {
            app.crop_layout = Some(CropLayout::Pinned { centre: v.centre, k: v.k });
        }
        let b = v.bounds();
        // (the size of the render: the uncropped photo at the view's scale)
        target_rect = Rect::from_center_size(b.center(), vec2(frame.ow as f32 * v.k, frame.oh as f32 * v.k));
        img_rect = b;
    }
    let before_img = in_pane(areas[0], img_rect);
    let before_target = in_pane(areas[0], target_rect);
    app.image_rect = Some(img_rect);
    // request renders: the loupe at display resolution (drafts during drags)
    let interacting = app.session.interaction.is_some();
    let overlay = view_overlay(app, &d, ui.input(|i| i.modifiers.alt));
    // a drag drafts at full size where the GPU draws the view (by the GPU's own rule, `handles`;
    // `ready` first: `available` would wait for a device still being created)
    let gpu = interacting
        && lcc_engine::gpu::handles(lcc_pipeline::OutputDepth::U8, app.ui.soft_proof, overlay)
        && lcc_engine::gpu::ready()
        && lcc_engine::gpu::available();
    let shown = app.renderer.textures.get(&Slot::Main).filter(|t| t.photo == id && t.quick.is_none()).map(|t| (t.key, t.ms));
    let drag = app.session.interaction.as_ref().map(|i| (i.photo, i.label.as_str()));
    let draft = app.drag_scale.scale(drag, gpu, shown.map(|(key, _)| key));
    // render at the final size: a click-zoom animation only changes how the result is drawn
    let native_long = native[0].max(native[1]);
    let drawn_long = target_rect.width().max(target_rect.height()) * ppp;
    // the whole frame at about canvas size; the zoomed part of it is a window render (below)
    let look = d.hash64();
    // Diagnostic views that need whole-frame context, soft proofing, or reads too far for one
    // window still render the whole frame at the size it is drawn. A mask overlay is window-safe.
    let sizes = crate::region::ViewSizes {
        drawn_long,
        canvas_long: canvas.width().max(canvas.height()) * ppp,
        native_long,
        texture_side,
        draft,
        // (the crop tool shows the whole photo, turned: no zoomed windows)
        windows: !crop_tool,
    };
    let mut plan = crate::region::plan(&app.ui.settings, sizes);
    if plan.window_edge.is_some_and(|edge| app.window_refused == Some((id, look, edge))) {
        plan = crate::region::plan(&app.ui.settings, crate::region::ViewSizes { windows: false, ..sizes });
    }
    // time the full-size drafts where they are what shows: under a window the whole-frame draft
    // is small anyway, and the frame's time is the window's upload
    if let Some((key, ms)) = shown.filter(|_| plan.window_edge.is_none()) {
        app.drag_scale.observe(key, ms, app.perf.host_frame_ms);
    }
    // a pinch changes the zoom every frame: keep the sizes of before it until it is quiet
    let now = ui.input(|i| i.time);
    let plan = app.size_hold.apply(id, now, navigating, plan);
    if app.size_hold.holding(now) {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(crate::region::HOLD_SECS));
    }
    let want = plan.main_edge;
    let (rw, rh) = if aspect >= 1.0 { (want, (want as f32 / aspect) as usize) } else { ((want as f32 * aspect) as usize, want) };
    // hover and before renders are stand-ins: the preview size is plenty and keeps them cheap
    let se = app.ui.settings.stand_in_edge(want, texture_side);
    let (sw, sh) = if aspect >= 1.0 { (se, (se as f32 / aspect) as usize) } else { ((se as f32 * aspect) as usize, se) };
    if let Some(job) = app.session.loupe_job(id, rw.max(8), rh.max(8), !crop_tool) {
        let job = if interacting { job.draft() } else { job };
        let job = job.with_overlay(overlay).with_proof(app.ui.soft_proof.then_some(app.ui.proof));
        let job = job.with_hidden_grading(app.ui.grading_hidden_for(id));
        app.renderer.request(Slot::Main, job, 100);
        // a drag's draft that renders in a few ms (GPU) is drawn in this frame, not the next (zoomed
        // in, what changes on screen is the window's draft)
        #[cfg(not(target_arch = "wasm32"))]
        if interacting && plan.window_edge.is_none() {
            let frame = ui.input(|i| i.predicted_dt).clamp(1.0 / 240.0, 1.0 / 30.0);
            // (a frame time that is not a number waits for nothing)
            let budget = std::time::Duration::try_from_secs_f32(frame * 0.4).unwrap_or_default();
            app.renderer.wait_for_draft(ui.ctx(), &mut app.session, budget);
        }
    }
    // an HDR display: an HDR edit's whole frame also as linear float with its highlights above
    // SDR white, drawn in place of the 8-bit render once it is there (not while dragging: the
    // drafts show; nor with an overlay or soft proof, which are 8-bit views)
    let hdr_key = (app.hdr_headroom.is_some() && d.hdr.enabled && !interacting && overlay == lcc_pipeline::Overlay::None && !app.ui.soft_proof)
        .then(|| app.session.render_job(id, rw.max(8), rh.max(8), false, !crop_tool))
        .flatten()
        .map(|job| {
            let job = hdr_view_job(job.with_hidden_grading(app.ui.grading_hidden_for(id)));
            let key = job.key;
            if app.renderer.hdr_view.is_none_or(|v| v.key != key) {
                app.renderer.request(Slot::HdrView, job, 98);
            }
            key
        });
    upload_hdr_view(app);
    // hovering a preset or profile: the photo with that look, shown instead of the loupe render
    // once it is ready (nothing is committed)
    let hover_key = match app.hover_preview.clone() {
        Some(h) if !interacting => app.session.preview_job(id, sw.max(8), sh.max(8), !crop_tool, &h.settings).map(|job| {
            let key = job.key;
            app.renderer.request(Slot::Hover, job, 105);
            (key, h.label)
        }),
        _ => None,
    };
    // once this photo is on screen: prepare its neighbours in filmstrip order (source decoded and
    // kept, view render cached) so stepping to them is instant
    if !interacting && !app.renderer.is_pending(Slot::Main) && app.renderer.textures.get(&Slot::Main).is_some_and(|t| t.photo == id) {
        let ids = app.session.visible_cloned();
        if let Some(i) = ids.iter().position(|p| *p == id) {
            let next = ids.get(i + 1).copied();
            let prev = i.checked_sub(1).and_then(|j| ids.get(j)).copied();
            for (n, nid) in [next, prev].into_iter().enumerate() {
                let Some(nid) = nid else { continue };
                let Some(np) = app.session.catalog.photo(nid).cloned() else { continue };
                let nf = Frame::with_lens(np.width.max(1) as usize, np.height.max(1) as usize, &np.develop, !crop_tool, np.embedded_lens.as_deref());
                let na = nf.aspect() as f32;
                let nr = fit_rect(main_area, na, app.ui.zoom, output_px(&nf), ppp, app.ui.pan);
                let nw =
                    app.ui.settings.prefetch_edge(nr.width().max(nr.height()) * ppp, output_px(&nf).into_iter().max().unwrap_or(1), texture_side);
                let (w, h) = if na >= 1.0 { (nw, (nw as f32 / na) as usize) } else { ((nw as f32 * na) as usize, nw) };
                if let Some(job) = app.session.loupe_job(nid, w.max(8), h.max(8), !crop_tool) {
                    app.renderer.prefetch(Slot::Prefetch(n as u8), job, PREFETCH_PRIORITY);
                }
            }
        }
    }
    // until the loupe has this photo: show its cached render / embedded preview / a thumbnail
    if app.renderer.textures.get(&Slot::Main).is_none_or(|t| t.photo != id)
        && let Some(q) = app.session.quick_view_job(id, want.max(8), !crop_tool)
    {
        app.renderer.request_quick(Slot::Preview, q, 110);
    }
    let show_before = app.ui.before_after == BeforeAfter::Original || ui.input(|i| i.key_down(egui::Key::Backslash));
    if (split || show_before || split_view)
        && let Some(job) = app.session.render_job(id, sw.max(8), sh.max(8), true, !crop_tool)
    {
        app.renderer.request(Slot::Before, job, 90);
    }
    // zoomed past what the whole-frame render holds: also render the tiles on screen, at no more
    // than 100 % (the GPU magnifies beyond that), for each side that is shown, with the overlay
    // or soft proof the whole frame has. Not while hovering a look (that draws another image),
    // nor where the shown picture isn't the frame's own (an unsupported raw's embedded JPEG with
    // another crop: tile coordinates are the frame's).
    let plain_view = hover_key.is_none() && crate::region::same_aspect(display_aspect, aspect);
    let frame_edge = plan.window_edge.filter(|_| plain_view);
    // the 1:1 preview, when it is the zoomed frame (Build 1:1 Previews), stands in for the tiles
    // (one held only on disk is read in the background, once per photo and look)
    let full_preview = frame_edge.filter(|e| *e >= native_long).and_then(|_| {
        let load = app.full_preview_loaded != Some((id, look));
        app.full_preview_loaded = Some((id, look));
        app.session.full_preview_in_memory(id, load)
    });
    let window_ctx = WindowCtx {
        id,
        frame_edge,
        aspect,
        texture_side,
        crop_tool,
        interacting,
        look,
        holding: navigating || app.size_hold.holding(now),
        overlay,
        proof: app.ui.soft_proof.then_some(app.ui.proof),
        full_preview,
    };
    // (slot, before side?, rect to draw in, rect it is requested for, area whose pixels show)
    let mut window_views: Vec<WindowView> = Vec::new();
    if window_ctx.frame_edge.is_some() {
        let after = WindowView { pane: crate::render::PANE_AFTER, before: false, img: img_rect, target: target_rect, visible: main_area };
        if split {
            window_views.push(WindowView {
                pane: crate::render::PANE_BEFORE,
                before: true,
                img: before_img,
                target: before_target,
                visible: areas[0],
            });
            window_views.push(after);
        } else if show_before {
            window_views.push(WindowView { pane: crate::render::PANE_BEFORE, before: true, img: img_rect, target: target_rect, visible: canvas });
        } else if split_view {
            window_views.push(WindowView { pane: crate::render::PANE_BEFORE, before: true, img: img_rect, target: target_rect, visible: canvas });
            window_views.push(WindowView { visible: canvas, ..after });
        } else {
            window_views.push(WindowView { visible: canvas, ..after });
        }
    }
    let mut windows: Vec<(WindowView, crate::region::RegionView)> = Vec::new();
    // (the After first: until the original is decoded, it is the one that asks for it)
    window_views.sort_by_key(|v| v.before);
    let mut wanted_tiles = Vec::new();
    for v in window_views {
        let prev = if v.before { app.region_before_view } else { app.region_view };
        if let Some(view) = request_window(app, &window_ctx, &v, prev, &mut wanted_tiles, ui.ctx()) {
            windows.push((v, view));
        }
    }
    // tiles scrolled away before their turn came are not rendered
    if !window_ctx.holding {
        app.renderer.cancel_tiles(|s| wanted_tiles.contains(&s));
    }
    for before in [false, true] {
        let shown = windows.iter().find(|(v, _)| v.before == before).map(|(_, view)| *view);
        if before {
            app.region_before_view = shown;
        } else {
            app.region_view = shown;
        }
        // a side no longer shown zoomed frees its tiles
        if shown.is_none() {
            let pane = if before { crate::render::PANE_BEFORE } else { crate::render::PANE_AFTER };
            app.renderer.tiles.retain(|k| k.pane != pane);
            app.renderer.release(Slot::Window(pane));
        }
    }
    // a clip: the frame to show at the playhead (playing or paused)
    let video_tex = video_state.as_ref().filter(|(v, _)| *v == id).and_then(|(vid, vs)| {
        let side = (target_rect.width().max(target_rect.height()) * ppp) as u32;
        crate::video_view::update(app, ui.ctx(), *vid, vs, side).map(|(t, s)| (t, s, vs.playing))
    });
    let p = ui.painter_at(canvas);
    // what a view slot shows: its own render of this photo, else the stand-ins (no blank frame
    // between photos, and the full render replaces them in place)
    let hdr_view = app.renderer.hdr_view.filter(|v| v.photo == id && Some(v.key) == hdr_key);
    let draw = |slot: Slot, r: Rect, clip: Rect| -> &'static str {
        if slot == Slot::Main
            && let Some(v) = hdr_view
        {
            let image_rect = fit_texture_rect(r, v.size);
            p.with_clip_rect(clip).image(v.tex, image_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            return "hdr";
        }
        let mine = |s: Slot| app.renderer.textures.get(&s).filter(|t| t.photo == id);
        let (tex, what) = if let Some(t) = mine(slot) {
            (t, "render")
        } else if let Some(t) = mine(Slot::Preview) {
            (t, t.quick.map(quick_name).unwrap_or("preview"))
        } else if let Some(t) = app.renderer.thumb(id) {
            (t, "thumb")
        } else {
            return "none";
        };
        match &crop_view {
            Some(v) if r == img_rect => {
                p.with_clip_rect(clip).add(crop_view_mesh(tex.tex.id(), v));
            }
            _ => {
                let image_rect = fit_texture_rect(r, tex.size);
                p.with_clip_rect(clip).image(tex.tex.id(), image_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        }
        what
    };
    // soft proofing: a paper-white surround and the proof's name, as Lightroom shows it
    if app.ui.soft_proof && !fullscreen {
        p.rect_filled(canvas, 0.0, Color32::from_gray(238));
        let label = crate::i18n::tr_format!("Proof Preview · {}", app.ui.proof.space.label());
        p.text(pos2(canvas.right() - 16.0, canvas.top() + 14.0), Align2::RIGHT_CENTER, label, t.font(12.5), Color32::from_gray(60));
    }
    let shown;
    if split {
        // each view stays in its pane: zoomed in, a photo reaches past it (and would cover the
        // other view's margins with its whole-frame render, magnified and blurred)
        let br = before_img;
        draw(Slot::Before, br, areas[0]);
        shown = draw(Slot::Main, img_rect, main_area);
        // the labels under each photo, or under its pane when the photo reaches past it
        let label_at = |r: Rect, pane: Rect| pos2(r.left().max(pane.left()), r.bottom().min(pane.bottom()) + 14.0);
        p.text(label_at(br, areas[0]), Align2::LEFT_CENTER, crate::i18n::tr("Before"), t.font(12.0), t.text_dim);
        p.text(label_at(img_rect, main_area), Align2::LEFT_CENTER, crate::i18n::tr("After"), t.font(12.0), t.text_dim);
    } else if show_before {
        shown = draw(Slot::Before, img_rect, canvas);
        p.text(pos2(img_rect.left() + 8.0, img_rect.top() + 14.0), Align2::LEFT_CENTER, crate::i18n::tr("Before"), t.font(12.0), t.text);
    } else if let Some((_key, label)) = &hover_key
        && let Some(tex) = app.renderer.textures.get(&Slot::Hover).filter(|t| t.photo == id)
    {
        let hover_rect = fit_texture_rect(img_rect, tex.size);
        p.image(tex.tex.id(), hover_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let g = p.layout_no_wrap(label.clone(), t.font(12.0), Color32::WHITE);
        let bg = Rect::from_min_size(hover_rect.left_top() + vec2(8.0, 8.0), g.size() + vec2(16.0, 8.0));
        p.rect_filled(bg, 4.0, Color32::from_black_alpha(160));
        p.galley(bg.min + vec2(8.0, 4.0), g, Color32::WHITE);
        shown = "hover";
    } else if let Some((tex, _, playing)) = video_tex {
        // a clip: its current frame (playing or paused) over the poster render
        let _ = draw(Slot::Main, img_rect, canvas);
        p.add(video_frame_mesh(tex, img_rect, &frame));
        shown = if playing { "video" } else { "video-still" };
    } else {
        shown = draw(Slot::Main, img_rect, canvas);
        if shown == "none" {
            match app.renderer.failure_of(Slot::Main, id) {
                Some(e) => {
                    p.text(
                        canvas.center() - vec2(0.0, 10.0),
                        Align2::CENTER_CENTER,
                        crate::i18n::tr("This photo can't be opened"),
                        t.semibold(14.0),
                        t.text,
                    );
                    p.text(canvas.center() + vec2(0.0, 12.0), Align2::CENTER_CENTER, e, t.font(12.0), t.text_dim);
                }
                None => {
                    p.text(canvas.center(), Align2::CENTER_CENTER, crate::i18n::tr("Rendering…"), t.font(13.0), t.text_dim);
                }
            }
        }
    }
    app.loupe_shown = Some((id, shown));
    // the colour under the pointer, for the readout under the histogram (the After render only)
    if shown == "render"
        && !split
        && !show_before
        && crop_view.is_none()
        && let Some(q) = resp.hover_pos().filter(|q| canvas.contains(*q))
        && let Some(tex) = app.renderer.textures.get(&Slot::Main).filter(|t| t.photo == id)
        && let Some(px) = tex.pixels.as_deref()
    {
        let r = fit_texture_rect(img_rect, tex.size);
        let (u, v) = ((q.x - r.left()) / r.width().max(1.0), (q.y - r.top()) / r.height().max(1.0));
        if let Some(rgb) = crate::color_readout::srgb_at(px, u, v, app.renderer.display().map(|d| d.as_ref())) {
            app.ui.hover_color = Some((ui.ctx().cumulative_pass_nr(), rgb));
        }
    }
    // a wipe shows the Before whole-frame render on its side of the line…
    let line = crate::region::wipe_line(img_rect, canvas);
    if app.ui.before_after == BeforeAfter::Split
        && let Some(tex) = app.renderer.textures.get(&Slot::Before).filter(|t| t.photo == id)
    {
        let before_rect = fit_texture_rect(img_rect, tex.size);
        let u = ((line.x - before_rect.left()) / before_rect.width().max(1.0)).clamp(0.0, 1.0);
        let left = Rect::from_min_max(before_rect.min, pos2(before_rect.left() + u * before_rect.width(), before_rect.bottom()));
        p.image(tex.tex.id(), left, Rect::from_min_max(pos2(0.0, 0.0), pos2(u, 1.0)), Color32::WHITE);
    }
    if app.ui.before_after == BeforeAfter::SplitTopBottom
        && let Some(tex) = app.renderer.textures.get(&Slot::Before).filter(|t| t.photo == id)
    {
        let before_rect = fit_texture_rect(img_rect, tex.size);
        let v = ((line.y - before_rect.top()) / before_rect.height().max(1.0)).clamp(0.0, 1.0);
        let top = Rect::from_min_max(before_rect.min, pos2(before_rect.right(), before_rect.top() + v * before_rect.height()));
        p.image(tex.tex.id(), top, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, v)), Color32::WHITE);
    }
    // …and the window renders go over the whole-frame ones, all of them, where they belong in the
    // frame (clipped to their pane, or their side of the line)
    for (v, view) in &windows {
        // (holding `\` in a wipe shows the Before everywhere: it is the Before view alone then)
        let mode = if show_before { BeforeAfter::Original } else { app.ui.before_after };
        let clip = crate::region::window_clip(mode, v.before, v.visible, canvas, img_rect);
        draw_window(&p.with_clip_rect(clip), app, &window_ctx, v, view);
    }
    // the line of a wipe over both
    if app.ui.before_after == BeforeAfter::Split {
        p.line_segment([pos2(line.x, img_rect.top()), pos2(line.x, img_rect.bottom())], Stroke::new(1.5, Color32::WHITE));
    }
    if app.ui.before_after == BeforeAfter::SplitTopBottom {
        p.line_segment([pos2(img_rect.left(), line.y), pos2(img_rect.right(), line.y)], Stroke::new(1.5, Color32::WHITE));
    }
    // View ▸ Loupe Overlay ▸ Layout Image, over the photo (the After view in a before/after)
    super::layout_overlay::draw(app, &p, img_rect);
    register(ui.ctx(), "canvas:image", img_rect);
    let map = crop_view.map_or_else(|| CanvasMap::new(&frame, img_rect), |v| v.map);
    info_overlay(app, &p, canvas, &photo);
    if !fullscreen {
        filter_pill(app, ui, canvas);
    }
    if app.ui.face_boxes {
        // a name box left open on another photo is dropped
        if app.ui.name_edit.as_ref().is_some_and(|e| e.photo != id.0) {
            app.ui.name_edit = None;
        }
        let hints = super::faces::hints_for(app, id.0);
        let people = if app.ui.name_edit.is_some() { app.caches.person_names(&app.session.catalog) } else { Default::default() };
        // while the name box is open and recognition is not set up, the box offers the next step
        let setup = if app.ui.name_edit.is_some() { super::faces::setup(app, ui.ctx()) } else { super::faces::Setup::Running };
        match region_overlay(ui, &p, &map, &photo, d.orientation, hints.as_deref(), &people, &mut app.ui.name_edit, setup) {
            Some(RegionEdit::SetUp) => {
                app.ui.name_edit = None;
                super::faces::take_step(app, ui.ctx(), setup);
            }
            Some(RegionEdit::Remove(index)) => {
                let _ = app.run("photo.removeRegion", json!({"id": id.0, "index": index}));
            }
            Some(RegionEdit::Name(index, name)) => {
                app.ui.name_edit = None;
                let _ = app.run("faces.setName", json!({"id": id.0, "index": index, "name": name}));
            }
            Some(RegionEdit::Resize(index, r)) => {
                let _ = app.run("photo.setRegion", json!({"id": id.0, "index": index, "rect": {"x0": r.x0, "y0": r.y0, "x1": r.x1, "y1": r.y1}}));
            }
            None => {}
        }
    }
    if app.ui.settings.focus_point && !crop_tool {
        draw_focus_point(&p.with_clip_rect(img_rect.intersect(canvas)), img_rect, &photo, "canvas:focusPoint");
    }
    // a fine grid while a transform (geometry) slider is dragged, to judge verticals
    if app.ui.dragging_control.as_deref().is_some_and(|c| c.starts_with("geometry.")) {
        let n = 12;
        let stroke = Stroke::new(1.0, Color32::from_white_alpha(70));
        for i in 1..n {
            let fx = img_rect.left() + img_rect.width() * i as f32 / n as f32;
            p.line_segment([pos2(fx, img_rect.top()), pos2(fx, img_rect.bottom())], stroke);
        }
        let rows = ((n as f32) * img_rect.height() / img_rect.width()).round().max(2.0) as usize;
        for i in 1..rows {
            let fy = img_rect.top() + img_rect.height() * i as f32 / rows as f32;
            p.line_segment([pos2(img_rect.left(), fy), pos2(img_rect.right(), fy)], stroke);
        }
    }
    // Cursor selection must also run on gesture frames, when tool input is suppressed.
    if !matches!(right, RightPanel::Crop | RightPanel::Masking | RightPanel::Remove | RightPanel::RedEye) {
        general_cursor(app, ui, &resp, img_rect, main_area);
    }
    if !navigating {
        match right {
            RightPanel::Crop => crop_overlay(app, ui, &resp, &map, &frame, &d, id),
            RightPanel::Masking => mask_overlay(app, ui, &resp, &map, &d),
            RightPanel::Remove => remove_overlay(app, ui, &resp, &map, &d),
            RightPanel::RedEye => eye_overlay(app, ui, &resp, &map, &d),
            _ => {
                // side by side / stacked: Before shows the same frame, so a click in its view (white
                // balance, colour samples, targeted adjustment) means the same point of the photo.
                // Zoomed in, a view's image reaches past its pane: what counts is the pane clicked.
                // A click-zoom there centres both views on the point clicked (#1742).
                let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
                let (map, img, pane) = match (split, areas.first(), pointer) {
                    (true, Some(before), Some(q)) if before.contains(q) => (CanvasMap::new(&frame, before_img), before_img, *before),
                    _ => (map, img_rect, main_area),
                };
                general_interaction(app, ui, &resp, &map, img, pane, native, aspect)
            }
        }
    }
    // Space held over a tool: the view's hand takes the pointer from it (#1895)
    crate::space_hand::interact(app, ui, canvas, (main_area, img_rect), split.then(|| (areas[0], before_img)));
    // drawn and hit-tested above the loupe and its tools: clicks on it pan
    let nav = navigator(app, ui, canvas, img_rect, id);
    // what is on screen isn't the render on its way (a stand-in, the last render after an edit,
    // a magnified frame under its window): say so once that has lasted a moment
    let whole: &[Slot] = if show_before && !split {
        &[Slot::Before]
    } else if split || split_view {
        &[Slot::Main, Slot::Before]
    } else {
        &[Slot::Main]
    };
    let coming = !matches!(shown, "none" | "hover")
        && whole
            .iter()
            .copied()
            .chain(windows.iter().map(|(v, _)| Slot::Window(v.pane)))
            .any(|s| app.renderer.is_pending(s) && !app.renderer.retrying(s));
    let keys = [Slot::Main, Slot::Before, Slot::Window(crate::render::PANE_AFTER), Slot::Window(crate::render::PANE_BEFORE)]
        .map(|s| app.renderer.textures.get(&s).filter(|t| t.photo == id).map(|t| t.key));
    let rendering = ui.ctx().data_mut(|d| rendering_overdue(d.get_temp_mut_or(egui::Id::new("loupe-rendering"), None), keys, coming, now));
    // the render failed while a stand-in is up: what is on screen is all there will be (also while
    // a library change retries it)
    let failed = (!rendering
        && (!show_before || split)
        && !matches!(shown, "render" | "none" | "hover")
        && (!app.renderer.is_pending(Slot::Main) || app.renderer.retrying(Slot::Main)))
    .then(|| app.renderer.failure_of(Slot::Main, id))
    .flatten();
    if rendering || failed.is_some() {
        // over the photo (the After in two views); in the crop tool's wide margin, under it
        let over = if crop_tool { canvas } else { img_rect.intersect(if split { main_area } else { canvas }) };
        let over = if over.is_positive() { over } else { canvas };
        match failed {
            Some(e) => failed_pill(ui, app, &photo, canvas, over, nav, "loupe:renderFailed", e),
            None => {
                loupe_pill(ui, canvas, over, nav, "loupe:rendering", crate::i18n::tr("Rendering…"), false);
            }
        }
    }
    if let Some(why) = photo.preview_only.as_deref().filter(|_| !fullscreen) {
        preview_only_pill(ui, canvas, why);
    }
    resp.context_menu(|ui| {
        crate::widgets::menu_button(ui, crate::i18n::tr("Zoom"), |ui| {
            for (label, cmd) in [("Fit", "view.zoomFit"), ("100%", "view.zoom100"), ("Zoom In", "view.zoomIn"), ("Zoom Out", "view.zoomOut")] {
                if ui.button(crate::i18n::tr(label)).clicked() {
                    let _ = app.run(cmd, json!({}));
                }
            }
        });
        ui.separator();
        super::grid::context_menu(app, ui, id);
    });
}

/// The info overlay at the canvas' top left (`view.infoOverlay`): file name with the capture date
/// and size, or with the camera and exposure.
fn info_overlay(app: &LccApp, p: &egui::Painter, canvas: Rect, photo: &lcc_catalog::Photo) {
    use crate::state::InfoOverlay;
    let lines: Vec<String> = match app.ui.info_overlay {
        InfoOverlay::Off => return,
        InfoOverlay::Basic => {
            let date = photo.captured.as_deref().unwrap_or(&photo.imported);
            vec![photo.file_name.clone(), format!("{} · {} × {}", pretty_date(date), photo.width, photo.height)]
        }
        InfoOverlay::Exposure => {
            let m = &photo.meta;
            let mut exp = Vec::new();
            if !m.shutter.is_empty() {
                // "1/250 s", "0.4 s"; a whole second's own ″ needs no second unit
                exp.push(if m.shutter.ends_with('″') { m.shutter.clone() } else { format!("{} s", m.shutter) });
            }
            if let Some(a) = m.aperture {
                exp.push(format!("f/{a:.1}"));
            }
            if let Some(i) = m.iso {
                exp.push(format!("ISO {i}"));
            }
            if let Some(f) = m.focal_mm {
                exp.push(format!("{f:.0} mm"));
            }
            let camera = [m.camera.as_str(), m.lens.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
            let mut v = vec![photo.file_name.clone()];
            v.push(if exp.is_empty() { "No exposure information".into() } else { exp.join("  ") });
            if !camera.is_empty() {
                v.push(camera);
            }
            v
        }
    };
    let t = Tokens::get(p.ctx());
    let mut y = canvas.top() + 12.0;
    for (i, l) in lines.iter().enumerate() {
        let font = if i == 0 { t.semibold(15.0) } else { t.font(12.5) };
        // laid out uncoloured: `galley` only fills placeholder text with its colour, so a white
        // layout made the shadow a second white copy (#1690)
        let g = p.layout_no_wrap(l.clone(), font, Color32::PLACEHOLDER);
        let at = pos2(canvas.left() + 14.0, y);
        // a soft shadow keeps the text readable on bright photos
        p.galley(at + vec2(1.0, 1.0), g.clone(), Color32::from_black_alpha(200));
        p.galley(at, g.clone(), Color32::WHITE);
        y += g.size().y + 3.0;
    }
    register(p.ctx(), "canvas:infoOverlay", Rect::from_min_max(canvas.min, pos2(canvas.left() + 320.0, y)));
}

/// What the user did to a face box this frame.
enum RegionEdit {
    /// The × was clicked.
    Remove(usize),
    /// A handle drag ended: the region's new box (normalized, upright frame).
    Resize(usize, lcc_geom::Rect),
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
/// The app doesn't write regions to XMP. Regions are stored on the upright (EXIF-oriented) photo;
/// `orient` is the user's Rotate / Flip on top of it, which the loupe's normalized frame includes.
fn region_overlay(
    ui: &egui::Ui,
    p: &egui::Painter,
    map: &CanvasMap,
    photo: &lcc_catalog::Photo,
    orient: lcc_geom::Orientation,
    hints: Option<&super::faces::Hints>,
    people: &[String],
    editing: &mut Option<crate::state::NameEdit>,
    setup: super::faces::Setup,
) -> Option<RegionEdit> {
    let t = Tokens::get(p.ctx());
    let clip = p.clip_rect();
    let pointer = ui.input(|i| i.pointer.hover_pos());
    // the box being dragged: (region index, its box so far)
    let drag_key = egui::Id::new("region-drag");
    let live: Option<(usize, lcc_geom::Rect)> = ui.data(|d| d.get_temp(drag_key));
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
                let resp = ui.interact(Rect::from_center_size(c, vec2(14.0, 14.0)), egui::Id::new(("region-handle", index, h)), Sense::drag());
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
                    let shown = lcc_geom::Rect {
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
                    if let Some((i, new)) = ui.data(|d| d.get_temp::<(usize, lcc_geom::Rect)>(drag_key)) {
                        edit = Some(RegionEdit::Resize(i, new));
                    }
                    ui.data_mut(|d| d.remove_temp::<(usize, lcc_geom::Rect)>(drag_key));
                }
            }
        }
        if dragging.is_none() && live.is_none() && pointer.is_some_and(|h| rect.expand(3.0).contains(h)) {
            let xr = Rect::from_center_size(pos2(rect.right() - 13.0, rect.top() + 13.0), vec2(16.0, 16.0));
            let resp = ui.interact(xr, egui::Id::new(("region-x", index)), Sense::click());
            crate::widgets::announce(&resp, crate::i18n::tr("Remove region"));
            register(ui.ctx(), format!("regionRemove:{index}"), xr);
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
        let is_face = r.kind == lcc_meta::RegionKind::Face;
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
            let resp = ui.interact(label, egui::Id::new(("region-label", index)), Sense::click());
            crate::widgets::announce(&resp, crate::i18n::tr("Click to name this face"));
            register(ui.ctx(), format!("regionLabel:{index}"), label);
            if resp.on_hover_text(crate::i18n::tr("Click to name this face")).clicked() {
                let start = r.name.clone().or_else(|| hint.and_then(|h| h.suggestion.as_ref().map(|(n, _)| n.clone()))).unwrap_or_default();
                *editing = Some(crate::state::NameEdit { photo: photo.id.0, index, text: start, fresh: true });
            }
        }
        if editing_this && let Some(e) = editing.as_mut() {
            let candidates = hint.map(|h| h.candidates.as_slice()).unwrap_or(&[]);
            match super::faces::name_editor(ui.ctx(), pos2(label.left(), rect.bottom() + 8.0), e, candidates, people, setup) {
                super::faces::Editor::Submit(name) => edit = Some(RegionEdit::Name(index, name)),
                super::faces::Editor::Setup => edit = Some(RegionEdit::SetUp),
                super::faces::Editor::Cancel => *editing = None,
                super::faces::Editor::Open => {}
            }
        }
    }
    edit
}

/// A small pill at the loupe's top left naming the active filters: the filmstrip and Next / Previous
/// follow them, so a filter must never be invisible here. A click clears them all. It sits in the
/// canvas margin, so the photo does not move.
fn filter_pill(app: &mut LccApp, ui: &mut egui::Ui, canvas: Rect) {
    let chips = lcc_engine::filter_chips(&app.session.filter, &app.session.catalog);
    if chips.is_empty() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let label =
        chips.iter().map(|c| crate::panels::chips::display_label(c, &app.session.filter, &app.session.catalog)).collect::<Vec<_>>().join("  ·  ");
    let g = p.layout_no_wrap(crate::i18n::tr_format!("Filtered: {label}", label = label), t.font(12.0), Color32::from_gray(225));
    // the same inset from the left as from the top, so the pill sits in the corner (#1645)
    let inset = 3.0;
    let w = (g.size().x + 40.0).min((canvas.width() - 2.0 * inset).max(60.0));
    let r = Rect::from_min_size(canvas.min + vec2(inset, inset), vec2(w, 20.0));
    let resp = ui.interact(r, egui::Id::new("loupe-filter-pill"), Sense::click());
    crate::widgets::announce(&resp, crate::i18n::tr("Filters"));
    register(ui.ctx(), "loupe:filters", r);
    p.rect_filled(r, 10.0, Color32::from_black_alpha(if resp.hovered() { 225 } else { 185 }));
    let text_clip = Rect::from_min_max(r.min, pos2(r.right() - 24.0, r.max.y));
    p.with_clip_rect(text_clip).galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, Color32::from_gray(225));
    let (c, m) = (pos2(r.right() - 13.0, r.center().y), 3.0);
    let cross = Stroke::new(1.3, if resp.hovered() { Color32::WHITE } else { Color32::from_gray(190) });
    p.line_segment([c - vec2(m, m), c + vec2(m, m)], cross);
    p.line_segment([c - vec2(m, -m), c + vec2(m, -m)], cross);
    if resp.on_hover_text(crate::i18n::tr("Click to clear these filters")).clicked() {
        let _ = app.run("library.clearFilter", json!({}));
    }
}

/// A raw shown from its embedded JPEG (issue #10): a pill at the canvas' top centre saying so;
/// hovering it explains (registered as `notice:previewOnly:loupe`).
fn preview_only_pill(ui: &mut egui::Ui, canvas: Rect, reason: &str) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let text = crate::i18n::tr_format!("Preview only — editing the camera's embedded JPEG ({})", crate::widgets::preview_only_variant(reason));
    let g = p.layout_no_wrap(text, t.font(12.0), Color32::WHITE);
    let size = vec2(g.size().x + 40.0, 26.0);
    let r = Rect::from_min_size(pos2(canvas.center().x - size.x / 2.0, canvas.top() + 12.0), size);
    p.rect_filled(r, 13.0, Color32::from_black_alpha(185));
    crate::icons::paint(&p, Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(15.0, 15.0)), crate::icons::Icon::Info, t.caution);
    p.galley(pos2(r.left() + 29.0, r.center().y - g.size().y / 2.0), g, Color32::WHITE);
    register(ui.ctx(), "notice:previewOnly:loupe", r);
    ui.interact(r, egui::Id::new("preview-only-pill"), Sense::hover()).on_hover_text(crate::widgets::preview_only_explanation(reason));
}

/// `2026-09-30T12:00:00` → `2026-09-30 12:00`.
fn pretty_date(iso: &str) -> String {
    let d = iso.replacen('T', " ", 1);
    d.get(..16).map(str::to_string).unwrap_or(d)
}

/// Size of the Navigator mini map (points).
const NAV_W: f32 = 180.0;

/// The Navigator: while zoomed in, a mini map of the photo at the canvas' bottom right with the
/// visible region outlined; click or drag on it to pan. Returns the area it covers.
fn navigator(app: &mut LccApp, ui: &mut egui::Ui, canvas: Rect, img: Rect, id: PhotoId) -> Option<Rect> {
    let zoomed = img.width() > canvas.width() + 1.0 || img.height() > canvas.height() + 1.0;
    // the Library's Navigator panel outlines the same part
    if img.width() > 0.0 && img.height() > 0.0 {
        let vis = canvas.intersect(img);
        let n = |q: Pos2| pos2((q.x - img.left()) / img.width(), (q.y - img.top()) / img.height());
        super::navigator::note_visible(ui.ctx(), Rect::from_min_max(n(vis.min), n(vis.max)));
    }
    if !app.ui.navigator || !zoomed || app.ui.fullscreen {
        return None;
    }
    let tex = app.renderer.thumb(id).or_else(|| app.renderer.textures.get(&Slot::Main).filter(|t| t.photo == id));
    let aspect = img.width() / img.height().max(1.0);
    let (w, h) = if aspect >= 1.0 { (NAV_W, NAV_W / aspect) } else { (NAV_W * aspect, NAV_W) };
    let frame = Rect::from_min_size(pos2(canvas.right() - w - 16.0, canvas.bottom() - h - 16.0), vec2(w, h));
    let p = ui.painter_at(canvas);
    p.rect_filled(frame.expand(5.0), 4.0, Color32::from_black_alpha(190));
    match tex {
        Some(t) => p.image(t.tex.id(), frame, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE),
        None => p.rect_filled(frame, 0.0, Color32::from_gray(40)),
    };
    // the visible part of the photo, in normalized image coordinates
    let vis = canvas.intersect(img);
    let n = |q: Pos2| pos2((q.x - img.left()) / img.width(), (q.y - img.top()) / img.height());
    let (a, b) = (n(vis.min), n(vis.max));
    let to_nav = |q: Pos2| pos2(frame.left() + q.x * frame.width(), frame.top() + q.y * frame.height());
    let view = Rect::from_min_max(to_nav(a), to_nav(b));
    p.rect_stroke(view, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
    p.rect_stroke(view.expand(1.5), 0.0, Stroke::new(1.0, Color32::from_black_alpha(160)), StrokeKind::Outside);
    register(ui.ctx(), "canvas:navigator", frame);
    let resp = ui.interact(frame.expand(5.0), egui::Id::new("navigator"), Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr("Navigator"));
    if (resp.clicked() || resp.dragged())
        && let Some(q) = resp.interact_pointer_pos()
    {
        // centre the view on the point under the pointer
        let u = ((q.x - frame.left()) / frame.width()).clamp(0.0, 1.0);
        let v = ((q.y - frame.top()) / frame.height()).clamp(0.0, 1.0);
        app.ui.pan = (u, v);
    }
    Some(frame.expand(5.0))
}

/// Neighbour prefetch: below on-screen thumbnails, above background thumbnail refreshes.
const PREFETCH_PRIORITY: u32 = 4;

/// How long a render may be on its way, with nothing new on screen, before the loupe says
/// "Rendering…" (s): as long as the activity stack waits before it shows a task.
const RENDERING_AFTER_SECS: f64 = 0.5;

/// What [`rendering_overdue`] keeps between frames while a render is coming: the texture keys on
/// screen, since when, and whether the pill is up.
type RenderingWait = Option<([Option<u64>; 4], f64, bool)>;

/// Whether the loupe says "Rendering…": a render has been `coming` for [`RENDERING_AFTER_SECS`] with
/// no new texture `keys` on screen. The wait starts when the render starts coming (an idle app draws
/// no frames, so the last one may be long ago), and once up the pill stays until nothing is coming:
/// drafts slower than the wait don't make it blink.
pub(crate) fn rendering_overdue(wait: &mut RenderingWait, keys: [Option<u64>; 4], coming: bool, now: f64) -> bool {
    if !coming {
        *wait = None;
        return false;
    }
    let (seen, since, up) = wait.get_or_insert((keys, now, false));
    if !*up && *seen != keys {
        (*seen, *since) = (keys, now);
    }
    *up |= now - *since > RENDERING_AFTER_SECS;
    *up
}

/// A pill over the photo, 12 pt above the bottom of `over` or just above the Navigator when that
/// is in the way, registered as `id`; `caution` adds the preview-only pill's caution icon. The
/// loupe's "Rendering…" says the pixels on screen aren't final, so it isn't an activity row (see
/// docs/background-tasks.md); "This photo can't be opened" says the stand-in on screen is all
/// there will be.
pub(crate) fn loupe_pill(ui: &egui::Ui, canvas: Rect, over: Rect, navigator: Option<Rect>, id: &str, text: &str, caution: bool) -> Rect {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let g = p.layout_no_wrap(text.to_string(), t.font(12.0), Color32::from_gray(225));
    let inset = if caution { 29.0 } else { 10.0 };
    let size = vec2(g.size().x + inset + 10.0, 20.0);
    let mut r = Rect::from_center_size(pos2(over.center().x, over.bottom() - 12.0 - size.y / 2.0), size);
    if let Some(nav) = navigator.filter(|n| n.expand(8.0).intersects(r)) {
        r = Rect::from_min_size(pos2(nav.right() - size.x, nav.top() - 8.0 - size.y), size);
    }
    p.rect_filled(r, 10.0, Color32::from_black_alpha(185));
    if caution {
        crate::icons::paint(&p, Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(13.0, 13.0)), crate::icons::Icon::Info, t.caution);
    }
    p.galley(pos2(r.left() + inset, r.center().y - g.size().y / 2.0), g, Color32::from_gray(225));
    register(ui.ctx(), id, r);
    r
}

/// The caution pill saying a render failed behind the stand-in on screen (see [`loupe_pill`]):
/// "Original offline · no smart preview" when that is why, else "This photo can't be opened", with
/// the error on hover and for screen readers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn failed_pill(
    ui: &egui::Ui,
    app: &LccApp,
    photo: &lcc_catalog::Photo,
    canvas: Rect,
    over: Rect,
    navigator: Option<Rect>,
    id: &str,
    error: &str,
) {
    let offline = matches!(&photo.source, lcc_catalog::Source::File { path } if app.session.media.availability.is_offline(path));
    let text = crate::i18n::tr(if offline { "Original offline · no smart preview" } else { "This photo can't be opened" });
    let r = loupe_pill(ui, canvas, over, navigator, id, text, true);
    let resp = ui.interact(r, egui::Id::new(("render-failed", id)), Sense::hover()).on_hover_text(error);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{text}: {error}")));
}

fn quick_name(q: lcc_engine::media::QuickSource) -> &'static str {
    use lcc_engine::media::QuickSource;
    match q {
        QuickSource::Cached => "cached",
        QuickSource::Embedded => "embedded",
        QuickSource::Small => "small",
    }
}

/// Targeted adjustment tool: dragging up/down on the photo raises/lowers the tone-curve region or
/// the colour-mixer bands under the press point (`develop.targeted`, one call per whole step, all
/// in one interaction = one undo step).
fn targeted_drag(app: &mut LccApp, resp: &egui::Response, map: &CanvasMap, target: &str) {
    if resp.drag_started()
        && let Some(q) = resp.interact_pointer_pos()
    {
        let _ = app.run("develop.beginInteraction", json!({"label": "Targeted Adjustment"}));
        app.gesture = Some(Gesture::Targeted { at: map.norm(q), acc: 0.0 });
    }
    if resp.dragged()
        && let Some(Gesture::Targeted { at, acc }) = &mut app.gesture
    {
        *acc -= resp.drag_delta().y * 0.5;
        let step = acc.trunc();
        if step != 0.0 {
            *acc -= step;
            let at = *at;
            let mut p = json!({"target": target, "x": at.x, "y": at.y, "delta": step});
            if target == "curve" && app.ui.curve_channel != "parametric" {
                p["channel"] = json!(app.ui.curve_channel);
            }
            let _ = app.run("develop.targeted", p);
        }
    }
    if resp.drag_stopped() && matches!(app.gesture, Some(Gesture::Targeted { .. })) {
        app.gesture = None;
        let _ = app.run("develop.endInteraction", json!({}));
    }
}

/// The loupe's HDR render for an HDR display: `job` (a whole-frame render of the open photo, no
/// caches) as linear float with the HDR range ([`Slot::HdrView`]). Its own result key.
pub(crate) fn hdr_view_job(mut job: lcc_engine::media::RenderJob) -> lcc_engine::media::RenderJob {
    job.request.depth = lcc_pipeline::OutputDepth::F32Hdr;
    job.request.space = lcc_pipeline::OutputSpace::Srgb;
    job.cache = None;
    job.view_cache = None;
    job.stages = None;
    job.display = None;
    job.request.display = None;
    job.key ^= 0x4844_525f_5649_4557;
    job
}

/// Hand a finished [`Slot::HdrView`] render to the window's HDR output as the loupe's texture.
fn upload_hdr_view(app: &mut LccApp) {
    let Some((photo, key, deep)) = app.renderer.hdr_view_result.take() else { return };
    let Some(out) = app.services.hdr_output.as_mut() else { return };
    let lcc_pipeline::DeepSamples::F32(rgb) = &deep.samples else { return };
    match out.upload(deep.width, deep.height, rgb) {
        Ok(tex) => app.renderer.hdr_view = Some(crate::render::HdrView { photo, key, tex, size: [deep.width, deep.height] }),
        Err(e) => log::warn!("HDR loupe texture: {e}"),
    }
}

/// The diagnostic overlay the loupe shows (the selected mask, Point Color's visualized range,
/// Visualize Spots, Visualize HDR, and the clipping overlay: J's per-pixel red/blue tint, or the
/// black/white clipping mask while ⌥/Alt-dragging a global tone slider).
pub(crate) fn view_overlay(app: &LccApp, d: &DevelopSettings, alt_held: bool) -> lcc_pipeline::Overlay {
    use lcc_pipeline::{MaskView, Overlay};
    if app.ui.fullscreen {
        return Overlay::None;
    }
    // Show every visible mask's region at once (Ctrl+Shift+O / Cmd+Shift+O), ahead of the selected-mask overlay
    // so the shortcut is not shadowed by the always-on single-mask overlay.
    if app.ui.right == RightPanel::Masking && app.ui.mask_overlay_all && d.masks.iter().any(|m| m.visible && !m.components.is_empty()) {
        return Overlay::AllMasks {
            view: MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default(),
            color: app.ui.mask_overlay_color,
            opacity: app.ui.mask_overlay_opacity.clamp(0.0, 100.0).round() as u8,
        };
    }
    if app.ui.right == RightPanel::Masking
        && app.ui.mask_overlay
        && let Some(m) = app.session.active_mask.and_then(|id| d.masks.iter().find(|m| m.id == id))
        && !m.components.is_empty()
    {
        return Overlay::Mask {
            id: m.id.min(u16::MAX as u32) as u16,
            view: MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default(),
            color: app.ui.mask_overlay_color,
            opacity: app.ui.mask_overlay_opacity.clamp(0.0, 100.0).round() as u8,
        };
    }
    if app.ui.right == RightPanel::Remove && app.ui.visualize_spots {
        return Overlay::Spots(app.ui.spots_threshold.clamp(0.0, 100.0).round() as u8);
    }
    let edit = app.ui.right == RightPanel::Edit;
    if edit && app.ui.point_color_visualize && app.ui.flyout_open("pointColor") && app.ui.point_color < d.point_colors.len() {
        return Overlay::PointColorRange(app.ui.point_color as u8);
    }
    if edit && app.ui.hdr_visualize && d.hdr.enabled {
        return Overlay::HdrRange;
    }

    if app.ui.right != RightPanel::Crop {
        // Keyed by the dragged control's id, not the interaction label: a mask's "Exposure" slider
        // must not bring up the global clipping mask.
        if alt_held && app.session.interaction.is_some() {
            match app.ui.dragging_control.as_deref() {
                Some("light.exposure" | "light.highlights" | "light.whites") => return Overlay::Clipping(Some(false)),
                Some("light.shadows" | "light.blacks") => return Overlay::Clipping(Some(true)),
                _ => {}
            }
        }

        // the persistent clipping overlay (J), or the one a hovered histogram indicator previews
        if app.ui.show_clipping || app.clipping_preview {
            return Overlay::Clipping(None);
        }
    }

    Overlay::None
}

fn general_cursor(app: &LccApp, ui: &egui::Ui, resp: &egui::Response, img: Rect, canvas: Rect) {
    // A panel or overlay under the pointer keeps its own cursor.
    if !resp.dragged() && !resp.hovered() {
        return;
    }
    let cursor = match app.ui.tool.as_str() {
        "wbPicker" | "colorRange" | "pointColor" => egui::CursorIcon::Crosshair,
        tool if tool.starts_with("tat:") => egui::CursorIcon::ResizeVertical,
        _ if img.width() > canvas.width() + 1.0 || img.height() > canvas.height() + 1.0 => {
            if resp.dragged() {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            }
        }
        _ => return,
    };
    ui.ctx().set_cursor_icon(cursor);
}

fn general_interaction(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    resp: &egui::Response,
    map: &CanvasMap,
    img: Rect,
    canvas: Rect,
    native: [usize; 2],
    aspect: f32,
) {
    if app.ui.tool == "wbPicker" {
        if resp.clicked()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let n = map.norm(q);
            let _ = app.run("develop.wbPick", json!({"x": n.x, "y": n.y}));
            app.ui.tool.clear();
        }
        return;
    }
    if let Some(target) = app.ui.tool.strip_prefix("tat:").map(str::to_string) {
        targeted_drag(app, resp, map, &target);
        return;
    }
    if app.ui.tool == "colorRange" {
        color_range_pick(app, ui, resp, map);
        return;
    }
    if app.ui.tool == "pointColor" {
        if resp.clicked()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let n = map.norm(q);
            if let Ok(r) = app.run("pointColor.pick", json!({"x": n.x, "y": n.y})) {
                app.ui.point_color = r["index"].as_u64().unwrap_or(0) as usize;
            }
            app.ui.tool.clear();
        }
        return;
    }
    click_zoom_and_pan(app, resp, img, canvas);
    let _ = (native, aspect);
}

/// The photo `img` reaches past its pane `canvas`: a drag pans it.
pub(crate) fn zoomed(img: Rect, canvas: Rect) -> bool {
    img.width() > canvas.width() + 1.0 || img.height() > canvas.height() + 1.0
}

/// The loupe's own click and drag (also Space's hand over a tool, `space_hand`): a click toggles
/// Fit ↔ the chosen click-zoom ratio at the clicked point, a drag pans when zoomed.
pub(crate) fn click_zoom_and_pan(app: &mut LccApp, resp: &egui::Response, img: Rect, canvas: Rect) {
    let zoomed = zoomed(img, canvas);
    if resp.double_clicked() || (resp.clicked() && !zoomed) {
        if let Some(q) = resp.interact_pointer_pos() {
            let u = ((q.x - img.left()) / img.width()).clamp(0.0, 1.0);
            let v = ((q.y - img.top()) / img.height()).clamp(0.0, 1.0);
            app.ui.pan = (u, v);
        }
        app.ui.zoom = if matches!(app.ui.zoom, Zoom::Fit) { Zoom::Percent(app.ui.click_zoom as f32) } else { Zoom::Fit };
        app.ui.zoom_anim = true;
    } else if resp.clicked() && zoomed {
        app.ui.zoom = Zoom::Fit;
        app.ui.zoom_anim = true;
    }
    if zoomed && resp.dragged() {
        pan_image(app, canvas, img, resp.drag_delta());
    }
}

// ------------------------------------------------------------------------ crop

fn crop_overlay(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, frame: &Frame, d: &DevelopSettings, id: PhotoId) {
    if app.ui.tool == "guidedUpright" {
        guided_overlay(app, ui, resp, map, frame, d);
        return;
    }
    if app.ui.tool == "straighten" {
        straighten_overlay(app, ui, resp, false, d.crop.geometry.angle);
        return;
    }
    // hold ⌘ and drag: draw a straighten line without leaving the crop tool
    let cmd_drag = resp.drag_started() && ui.input(|i| i.modifiers.command);
    if cmd_drag || matches!(app.gesture, Some(Gesture::StraightenLine { .. })) {
        straighten_overlay(app, ui, resp, true, d.crop.geometry.angle);
        return;
    }
    if resp.hovered() && ui.input(|i| i.modifiers.command) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let quad = frame_crop_quad(d, frame);
    let pts: Vec<Pos2> = quad.iter().map(|q| map.screen(*q)).collect();
    let p = ui.painter();
    // darken outside the crop (the box's corners in screen order: a flipped photo shows its
    // top-left corner elsewhere)
    let quad = flipped_corners(&pts, frame.flip_h, frame.flip_v);
    if pts.len() == 4 {
        let uv_to_norm = frame.out_to_norm(1, 1);
        let photo = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(u, v)| map.screen(uv_to_norm.apply(Point::new(u, v))));
        p.add(outside_crop_mesh(&photo, &quad, Color32::from_black_alpha(150)));
    }
    // the box's centre: what a rotation turns the photo about (it stays put on screen)
    let box_centre = pos2(pts.iter().map(|q| q.x).sum::<f32>() / 4.0, pts.iter().map(|q| q.y).sum::<f32>() / 4.0);
    p.add(egui::Shape::closed_line(pts.clone(), Stroke::new(1.0, Color32::WHITE)));
    // overlay guides
    let lerp = |a: Pos2, b: Pos2, t: f32| a + (b - a) * t;
    let guide = Stroke::new(0.8, Color32::from_white_alpha(150));
    // (u, v) across / down the (rotated) crop frame
    let at = |(u, v): (f32, f32)| pts[0] + (pts[1] - pts[0]) * u + (pts[3] - pts[0]) * v;
    let (cw, ch) = (pts[0].distance(pts[1]), pts[0].distance(pts[3]));
    for line in super::crop_overlay::paths(app.ui.crop_overlay, app.ui.crop_overlay_orient, cw, ch) {
        p.add(egui::Shape::line(line.into_iter().map(at).collect(), guide));
    }
    // handles: 0..3 corners, 4..7 edges (top, right, bottom, left)
    let handles: Vec<Pos2> = (0..8).map(|i| if i < 4 { pts[i] } else { lerp(pts[i - 4], pts[(i - 3) % 4], 0.5) }).collect();
    for (i, h) in handles.iter().enumerate() {
        register(ui.ctx(), format!("cropHandle:{i}"), Rect::from_center_size(*h, vec2(14.0, 14.0)));
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
    if resp.drag_started()
        && let Some(q) = resp.interact_pointer_pos()
    {
        if let Err(e) = app.run("develop.beginInteraction", json!({"label": "Crop"})).and_then(|_| app.begin_crop().map(|_| serde_json::Value::Null))
        {
            app.ui.status = e;
            let _ = app.session.cancel_interaction();
            return;
        }
        let d = app.session.develop_of(id).unwrap_or_default();
        let (start, angle, map) = (d.crop.geometry.rect, d.crop.geometry.angle, *map);
        // screen points per pixel of the photo: the box's width on screen over its width in pixels
        let native_w = Frame { crop: d.crop.geometry, ..frame.clone() }.native_size().0 as f32;
        let k = pts[0].distance(pts[1]) / native_w.max(1e-3);
        let pinned = Some(CropLayout::Pinned { centre: box_centre, k });
        let (gesture, layout) = match handles.iter().position(|h| h.distance(q) < 12.0) {
            Some(h) => (Gesture::CropHandle { handle: h as u8, start, angle, map }, Some(CropLayout::Frozen(map))),
            None if inside(q) => (Gesture::CropHandle { handle: 8, start, angle, map }, pinned),
            None => (Gesture::CropRotate { start_angle: angle, a0: (q - box_centre).angle() }, pinned),
        };
        app.gesture = Some(gesture);
        app.crop_layout = layout;
    }
    if resp.dragged()
        && let Some(q) = resp.interact_pointer_pos()
    {
        match app.gesture.clone() {
            Some(Gesture::CropHandle { handle, start, angle, map }) => {
                // The crop model decides what the drag does (anchor, aspect lock, image bounds): see
                // `lcc_geom::drag_crop`; the UI only reports where the pointer started and is.
                let n = to_straight(map.norm(q), angle, frame);
                let orig = ui.input(|i| i.pointer.press_origin()).map(|q0| to_straight(map.norm(q0), angle, frame)).unwrap_or(n);
                // moving: the photo follows the pointer under the box, so the box moves the other way
                // over the photo
                let (orig, n) = if handle == 8 { (n, orig) } else { (orig, n) };
                // Shift keeps the current proportions for this drag only; the Lock toggle is untouched.
                let keep_ratio = ui.input(|i| i.modifiers.shift);
                let _ = app.run(
                    "crop.drag",
                    json!({"handle": handle, "from": [orig.x, orig.y], "to": [n.x, n.y], "start": [start.x0, start.y0, start.x1, start.y1], "keepRatio": keep_ratio}),
                );
            }
            Some(Gesture::CropRotate { start_angle, a0 }) => {
                let a = (q - box_centre).angle();
                let ang = (start_angle + (a - a0).to_degrees() as f64).clamp(-45.0, 45.0);
                shown_angle = (ang * 100.0).round() / 100.0;
                let _ = app.run("crop.straighten", json!({"angle": shown_angle}));
            }
            _ => {}
        }
    }
    if resp.drag_stopped() {
        app.gesture = None;
        app.crop_layout = None;
        let _ = app.run("develop.endInteraction", json!({}));
    }
    if let Some(at) = readout_at {
        // the canvas (the loupe's own rect), not the photo: rotating happens in the margin around it
        angle_readout(ui, at, shown_angle, resp.rect);
    }
    let _ = id;
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
fn crop_keys(app: &mut LccApp, ui: &mut egui::Ui, map: &CanvasMap, handles: &[Pos2], pts: &[Pos2], d: &DevelopSettings, frame: &Frame) -> bool {
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
    let shown = lcc_develop::controls::find("crop.angle").map_or_else(|| format!("{angle:.2}"), |spec| crate::widgets::shown_value(spec, angle));
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
    ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new(name)))
}

/// The rotation pointer: a curved double arrow on a dark disc, so it reads over any photo,
/// turned `turn` radians clockwise ([`rotate_cursor_turn`]).
fn rotate_cursor(ui: &egui::Ui, at: Pos2, turn: f32) {
    let p = top_painter(ui, "crop-rotate-cursor");
    let r = Rect::from_center_size(at, vec2(22.0, 22.0));
    p.circle_filled(at, 12.0, Color32::from_black_alpha(150));
    crate::icons::paint_turned(&p, r.shrink(2.0), crate::icons::Icon::RotateDrag, Color32::WHITE, turn);
    register(ui.ctx(), "cropRotateCursor", r);
}

/// How far to turn the rotation pointer (radians, clockwise) so its arc, drawn bulging up, bulges
/// away from the crop box `pts` (its four corners on screen, maybe rotated): up above the top edge,
/// right beside the right edge, down below it, diagonally off a corner. Measured from the nearest
/// point of the box's outline; `inside` (a rotate drag that has come back over the box) faces it
/// the other way, still away from that edge.
fn rotate_cursor_turn(pts: &[Pos2], q: Pos2, inside: bool) -> f32 {
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
    register(ui.ctx(), "cropAngleReadout", rect);
}

/// Guided Upright: draw up to four guides along lines that should be vertical or horizontal. Guides are
/// stored in lens-corrected (pre-perspective) coordinates, so they stay attached to the image as it warps.
fn guided_overlay(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, frame: &Frame, d: &DevelopSettings) {
    let p = ui.painter_at(map.rect.expand(8.0));
    let col = Color32::from_rgb(255, 196, 40);
    let draw = |a: Pos2, b: Pos2| {
        p.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(120)));
        p.line_segment([a, b], Stroke::new(1.5, col));
        for q in [a, b] {
            p.circle_filled(q, 4.0, col);
            p.circle_stroke(q, 4.0, Stroke::new(1.0, Color32::BLACK));
        }
    };
    for (i, (a, b)) in d.geometry.guides.iter().enumerate() {
        let (sa, sb) = (map.screen(frame.corrected_to_transformed(*a)), map.screen(frame.corrected_to_transformed(*b)));
        register(ui.ctx(), format!("uprightGuide:{i}"), Rect::from_two_pos(sa, sb));
        draw(sa, sb);
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    if resp.drag_started()
        && let Some(q) = resp.interact_pointer_pos()
    {
        app.gesture = Some(Gesture::Guide { a: map.norm(q) });
    }
    if let (Some(Gesture::Guide { a }), Some(q)) = (&app.gesture, resp.interact_pointer_pos()) {
        draw(map.screen(*a), q);
    }
    if resp.drag_stopped()
        && let (Some(Gesture::Guide { a }), Some(q)) = (app.gesture.clone(), resp.interact_pointer_pos())
    {
        app.gesture = None;
        let b = map.norm(q);
        if a.dist(b) > 0.02 {
            let (ca, cb) = (frame.transformed_to_corrected(a), frame.transformed_to_corrected(b));
            let _ = app.run("geometry.guides", json!({"guides": [[ca.x, ca.y, cb.x, cb.y]], "add": true}));
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

/// A crop box's corners (top-left, top-right, bottom-right, bottom-left of the unflipped photo)
/// in the order they appear on screen when the photo is shown flipped.
fn flipped_corners(pts: &[Pos2], flip_h: bool, flip_v: bool) -> [Pos2; 4] {
    let at = |i: usize| pts.get(i).copied().unwrap_or(Pos2::ZERO);
    let order = match (flip_h, flip_v) {
        (false, false) => [0, 1, 2, 3],
        (true, false) => [1, 0, 3, 2],
        (false, true) => [3, 2, 1, 0],
        (true, true) => [2, 3, 0, 1],
    };
    order.map(at)
}

/// The dimming outside the crop box: the photo (a convex quad, turned in the crop tool) minus the crop quad, as one mesh of
/// non-overlapping convex pieces. The quad is a rotated rectangle, so its opposite sides are
/// parallel and the plane outside it splits into four disjoint half-strips: outside edge `i`
/// and inside edge `i + 1`. Each is clipped to the photo and fanned into triangles.
///
/// Issue #731: joining photo corner `i` to crop corner `i` with a quad per side only works while
/// the two rectangles are aligned. Rotated, those quads cross themselves and each other, which
/// showed as dimmed wedges inside the crop and doubly dark ones outside, growing with the angle.
/// A degenerate quad (a point, a line, a non-finite corner) dims nothing.
fn outside_crop_mesh(photo: &[Pos2; 4], quad: &[Pos2; 4], color: Color32) -> egui::epaint::Mesh {
    let mut mesh = egui::epaint::Mesh::default();
    if !quad.iter().chain(photo).all(|q| q.x.is_finite() && q.y.is_finite()) {
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
    for i in 0..4 {
        let outside = |q: Pos2| side(i, q).map(|d| -d);
        let inside_next = |q: Pos2| side(i + 1, q);
        let Some(strip) = clip(photo, &outside).and_then(|poly| clip(&poly, &inside_next)) else { return egui::epaint::Mesh::default() };
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

// ------------------------------------------------------------------------ masks

/// Where a mask component's pin sits (normalized), for shapes that have a place on the photo.
pub(crate) fn component_pin(shape: &MaskShape) -> Option<Point> {
    match shape {
        MaskShape::Radial { center, .. } => Some(*center),
        MaskShape::Linear { start, end } => Some(Point::new((start.x + end.x) / 2.0, (start.y + end.y) / 2.0)),
        MaskShape::Brush { strokes } => strokes.iter().find(|s| !s.erase).and_then(|s| s.points.first()).copied(),
        _ => None,
    }
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
fn color_range_pick(app: &mut LccApp, ui: &egui::Ui, resp: &egui::Response, map: &CanvasMap) {
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
fn mask_overlay(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
    let long = (map.rect.width().max(map.rect.height())) as f64;
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
        for (ci, c) in m.components.iter().enumerate() {
            match &c.shape {
                MaskShape::Radial { center, rx, ry, angle, .. } if sel => {
                    let pts: Vec<Pos2> = (0..64)
                        .map(|i| {
                            let a = i as f64 / 64.0 * std::f64::consts::TAU;
                            let (s, co) = angle.to_radians().sin_cos();
                            let (x, y) = (rx * a.cos(), ry * a.sin());
                            let l = frame_long_norm(map);
                            map.screen(Point::new(center.x + (x * co - y * s) * l.0, center.y + (x * s + y * co) * l.1))
                        })
                        .collect();
                    p.add(egui::Shape::closed_line(pts, Stroke::new(1.5, Color32::from_white_alpha(230))));

                    let l = frame_long_norm(map);
                    for (i, a_deg) in [0.0_f64, 90.0_f64, 180.0_f64, 270.0_f64].into_iter().enumerate() {
                        let a_local = a_deg.to_radians();
                        let (s, co) = angle.to_radians().sin_cos();
                        let (x, y) = (rx * a_local.cos(), ry * a_local.sin());
                        let q = map.screen(Point::new(center.x + (x * co - y * s) * l.0, center.y + (x * s + y * co) * l.1));
                        p.circle_stroke(q, 5.0, Stroke::new(1.5, Color32::WHITE));
                        register(ui.ctx(), format!("maskHandle:{}:{ci}:{}", m.id, i + 1), Rect::from_center_size(q, vec2(14.0, 14.0)));
                        grips.push((m.id, ci, i as u8 + 1, q));
                    }

                    let (s, co) = angle.to_radians().sin_cos();
                    let rot_r = rx + 0.08;
                    let rot_q = map.screen(Point::new(center.x + (rot_r * co) * l.0, center.y + (rot_r * s) * l.1));
                    let h1_q = map.screen(Point::new(center.x + (rx * co) * l.0, center.y + (rx * s) * l.1));
                    p.line_segment([h1_q, rot_q], Stroke::new(1.0, Color32::from_white_alpha(200)));
                    p.circle_stroke(rot_q, 4.0, Stroke::new(1.5, Color32::WHITE));
                    register(ui.ctx(), format!("maskHandle:{}:{ci}:5", m.id), Rect::from_center_size(rot_q, vec2(14.0, 14.0)));
                    grips.push((m.id, ci, 5, rot_q));
                }
                MaskShape::Linear { start, end } if sel => {
                    let (a, b) = (map.screen(*start), map.screen(*end));
                    let dir = (b - a).normalized();
                    let perp = vec2(-dir.y, dir.x) * 3000.0;
                    for (q, alpha) in [(a, 230), (b, 110)] {
                        p.line_segment([q - perp, q + perp], Stroke::new(1.0, Color32::from_white_alpha(alpha)));
                    }
                    for (i, q) in [a, b].iter().enumerate() {
                        p.circle_stroke(*q, 5.0, Stroke::new(1.5, Color32::WHITE));
                        register(ui.ctx(), format!("maskHandle:{}:{ci}:{i}", m.id), Rect::from_center_size(*q, vec2(14.0, 14.0)));
                        grips.push((m.id, ci, i as u8 + 1, *q));
                    }
                }
                _ => {}
            }
            if app.ui.mask_pins
                && mode.shows(sel, live)
                && let Some(at) = component_pin(&c.shape)
            {
                let q = map.screen(at);
                pin(&pins, q, sel);
                register(ui.ctx(), format!("maskPin:{}:{ci}", m.id), Rect::from_center_size(q, vec2(14.0, 14.0)));
                grips.push((m.id, ci, 0, q));
            }
        }
    }
    mask_keys(app, ui, map, d, &grips);
    // Object selections (SAM 3) of the selected mask: each click, green to include, red to exclude
    // (clicks still on their way to the model included)
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
    // Colour Range "Pick": the Masking panel owns the loupe's clicks (general_interaction
    // never runs while it is open), so the eyedropper is handled here too.
    if app.ui.tool == "colorRange" {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        color_range_pick(app, ui, resp, map);
        return;
    }
    // Object tool: a click includes what's under it, ⌥-click leaves it out
    if app.ui.tool == "object" {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.clicked()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let n = map.norm(q);
            let exclude = ui.input(|i| i.modifiers.alt);
            match app.run_typed("mask.objectPoint", json!({"x": n.x, "y": n.y, "exclude": exclude})) {
                Ok(_) => {
                    if let Some(mid) = app.session.active_mask {
                        app.ui.detail_due = Some((ui.input(|i| i.time) + 1.0, mid));
                    }
                }
                Err(e) => app.ai_error(ui.ctx(), e, Some(("object", "new"))),
            }
        }
        return;
    }
    // brush tool
    if app.ui.tool == "brush" {
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
        // stroke's dab spacing, feather and flow (issue #517)
        if let Some(Gesture::Brush { points, pressure }) = &app.gesture {
            let screen: Vec<Pos2> = points.iter().map(|q| map.screen(*q)).collect();
            let opacity = if app.ui.mask_overlay && app.ui.mask_overlay_opacity > 0.0 { app.ui.mask_overlay_opacity / 100.0 } else { 110.0 / 255.0 };
            let params = crate::brush_live::LiveParams {
                clip,
                scale: ui.ctx().pixels_per_point(),
                r,
                hard: r * (1.0 - (app.ui.brush_feather / 100.0).clamp(0.0, 1.0)),
                flow: (app.ui.brush_flow / 100.0).clamp(0.0, 1.0),
                density: (app.ui.brush_density / 100.0).clamp(0.0, 1.0),
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
                       "density": app.ui.brush_density, "erase": erase, "autoMask": app.ui.brush_auto_mask, "pressure": pressure, "connect": connect}),
            );
        }
        return;
    }
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
fn mask_keys(app: &mut LccApp, ui: &mut egui::Ui, map: &CanvasMap, d: &DevelopSettings, grips: &[(u32, usize, u8, Pos2)]) {
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
fn pins_live(app: &LccApp, resp: &egui::Response, map: &CanvasMap) -> bool {
    resp.hover_pos().is_some_and(|q| map.rect.contains(q))
        || matches!(app.gesture, Some(Gesture::SpotMove { .. } | Gesture::MaskHandle { .. }))
        // a handle moved with the keyboard stays shown while it has the focus
        || crate::canvas_keys::has_focus(&resp.ctx)
}

fn pin(p: &egui::Painter, c: Pos2, sel: bool) {
    p.circle_filled(c, 6.0, if sel { Color32::from_rgb(1, 101, 221) } else { Color32::from_gray(200) });
    p.circle_stroke(c, 6.0, Stroke::new(1.5, Color32::WHITE));
}

// ------------------------------------------------------------------------ remove

/// The Remove tool on the photo: every spot's outline and pin (the selected one with its source),
/// click a pin to select its spot, drag a target or source to move it, paint elsewhere to add one.
/// The Tool Overlay mode (Shift+H) says which spots show; a hidden spot can't be grabbed, so a
/// click there adds a new one.
fn remove_overlay(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
    let long = (map.rect.width().max(map.rect.height())) as f64;
    let active = app.session.active_spot.filter(|i| *i < d.spots.len());
    let (mode, live) = (app.ui.tool_overlay, pins_live(app, resp, map));
    // Auto also hides them while a new spot is painted
    let painting = mode == ToolOverlay::Auto && matches!(app.gesture, Some(Gesture::Spot { .. }));
    let p = ui.painter_at(app.canvas_rect.unwrap_or(map.rect));
    let mut pins = p.clone();
    if painting {
        pins.set_invisible();
    }
    // (spot, is source, screen centre, radius) of everything that can be grabbed
    let mut grips: Vec<(usize, bool, Pos2, f32)> = Vec::new();
    for (i, s) in d.spots.iter().enumerate() {
        let r = (s.size * long) as f32;
        let sel = Some(i) == active;
        if !mode.shows(sel, live) {
            continue;
        }
        let p = &pins;
        let col = Color32::from_white_alpha(if sel { 230 } else { 150 });
        for q in &s.points {
            p.circle_stroke(map.screen(*q), r, Stroke::new(if sel { 1.5 } else { 1.0 }, col));
        }
        let Some(&t) = s.points.first() else { continue };
        let tq = map.screen(t);
        if let Some(o) = s.source_offset.filter(|_| sel) {
            let sq = map.screen(Point::new(t.x + o.x, t.y + o.y));
            for q in &s.points {
                p.circle_stroke(map.screen(Point::new(q.x + o.x, q.y + o.y)), r, Stroke::new(1.0, Color32::from_white_alpha(170)));
            }
            let dir = (tq - sq).normalized();
            p.arrow(sq + dir * r, (tq - sq) - dir * (2.0 * r).min((tq - sq).length()), Stroke::new(1.0, Color32::from_white_alpha(200)));
            register(ui.ctx(), format!("spotSource:{i}"), Rect::from_center_size(sq, vec2(14.0, 14.0)));
            grips.push((i, true, sq, r));
        }
        pin(p, tq, sel);
        register(ui.ctx(), format!("spotPin:{i}"), Rect::from_center_size(tq, vec2(14.0, 14.0)));
        grips.push((i, false, tq, r));
    }
    if spot_keys(app, ui, map, d, &grips) {
        return;
    }
    let r = (app.ui.remove_size as f64 * long) as f32;
    // a target or source under `q`: the selected spot's first, then the nearest pin
    let hit = |q: Pos2| {
        grips
            .iter()
            .filter(|g| g.2.distance(q) < g.3.max(8.0))
            .min_by(|a, b| {
                (Some(a.0) != active, a.2.distance(q)).partial_cmp(&(Some(b.0) != active, b.2.distance(q))).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
    };
    let hover = resp.hover_pos().and_then(hit);
    if let Some(h) = resp.hover_pos() {
        if hover.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        } else {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            p.circle_stroke(h, r, Stroke::new(1.0, Color32::WHITE));
            p.circle_stroke(h, r * (1.0 - app.ui.remove_feather / 100.0).max(0.05), Stroke::new(1.0, Color32::from_white_alpha(110)));
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
        && (resp.drag_started() || resp.dragged() || resp.clicked())
    {
        let n = map.norm(q);
        match &mut app.gesture {
            Some(Gesture::Spot { points }) => points.push(n),
            _ => app.gesture = Some(Gesture::Spot { points: vec![n] }),
        }
    }
    if let Some(Gesture::Spot { points }) = &app.gesture {
        for q in points {
            p.circle_filled(map.screen(*q), r, Color32::from_white_alpha(60));
        }
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

/// Keyboard stand-ins for the Remove tool: each spot's target and source (an arrow press moves
/// it a step, one undo step; Return selects the spot), and a placement point that the arrows move
/// and Return turns into a new spot of the brush's size there. True when a spot was added.
fn spot_keys(app: &mut LccApp, ui: &mut egui::Ui, map: &CanvasMap, d: &DevelopSettings, grips: &[(usize, bool, Pos2, f32)]) -> bool {
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
    let key = egui::Id::new("lc-spot-placement");
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
fn eye_overlay(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
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

// ------------------------------------------------------------------------ filmstrip

/// A filmstrip cell's file-name label: names longer than 14 characters are cut to 13 and an
/// ellipsis. Counts characters, not bytes, so a CJK name is never cut inside a character (#266).
fn film_label(name: &str) -> String {
    if name.chars().nth(14).is_some() { format!("{}…", name.chars().take(13).collect::<String>()) } else { name.to_string() }
}

pub(crate) fn filmstrip(app: &mut LccApp, ui: &mut egui::Ui, r: Rect) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.canvas);
    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width(), 4.0)), 0.0, Color32::from_gray(0x20));
    let ids = app.session.visible_cloned();
    let active = app.session.selection.active;
    let cell_w = 120.0;
    let ppp = ui.ctx().pixels_per_point();
    if ids.is_empty() {
        let why = if app.session.filter != Default::default() { "No photos match the filters (View → Clear Filters)" } else { "No photos" };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, why, t.font(12.5), t.text_dim);
        return;
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(0.0, 4.0))));
    // a vertical mouse wheel scrolls the strip sideways (horizontal trackpad scrolls still work)
    child.style_mut().always_scroll_the_only_direction = true;
    // only a new active photo (or the strip appearing) scrolls it; see `grid::follow_active`
    let follow = super::grid::follow_active(child.ctx(), egui::Id::new("film-follow-active"), active, app.session.source);
    // a wheel notch scrolls a third of its width; see `grid::wheel_step`
    register(ui.ctx(), "filmstrip", child.max_rect());
    let wheel = super::grid::wheel_step(
        &child,
        egui::Id::new("film-wheel"),
        child.max_rect(),
        child.max_rect().width(),
        true,
        app.ui.settings.wheel_scroll_speed,
    );
    egui::ScrollArea::horizontal().id_salt("filmstrip").auto_shrink([false, false]).show_viewport(&mut child, |ui, vp| {
        if wheel != egui::Vec2::ZERO {
            ui.scroll_with_delta_animation(wheel, egui::style::ScrollAnimation::none());
        }
        let (area, _) = ui.allocate_exact_size(vec2(ids.len() as f32 * cell_w, r.height() - 16.0), Sense::hover());
        app.film_scroll = Some(vp.left());
        for (i, id) in ids.iter().enumerate() {
            let cr = Rect::from_min_size(pos2(area.left() + i as f32 * cell_w, area.top()), vec2(cell_w, area.height()));
            let local = Rect::from_min_size(pos2(i as f32 * cell_w, 0.0), cr.size());
            // centred when it is (partly) off screen; a click on a visible thumbnail leaves the strip alone
            if follow && Some(*id) == active && !(local.left() >= vp.left() && local.right() <= vp.right()) {
                ui.scroll_to_rect(cr, Some(egui::Align::Center));
            }
            if !local.intersects(vp.expand2(vec2(cell_w * 4.0, 0.0))) {
                continue;
            }
            // in the Map module photos are dragged from the strip onto the map to geotag them
            let map = app.ui.module == crate::module::ModuleId::Map;
            // in a regular album they are dragged along the strip to put them in Custom Order
            let reorder = !map && super::grid::custom_order_album(app).is_some();
            let resp = ui.interact(cr, egui::Id::new(("film", id.0)), if map || reorder { Sense::click_and_drag() } else { Sense::click() });
            crate::widgets::announce(&resp, app.session.catalog.photo(*id).map(|p| p.file_name.clone()).unwrap_or_default());
            if (map || reorder) && resp.drag_started() {
                if !app.session.selection.contains(*id) {
                    let _ = app.run("library.select", json!({"ids": [id.0]}));
                }
                app.ui.dragging_photos = Some(app.session.selection.ids.iter().map(|p| p.0).collect());
            } else if reorder && let Some(dragged) = app.ui.dragging_photos.clone() {
                super::grid::reorder_drop(app, ui, *id, cr, &dragged);
            }
            register(ui.ctx(), format!("film:{}", id.0), cr);
            let state = app.session.selection.state_of(*id);
            let sel = state == lcc_engine::SelectionState::Active;
            let p = ui.painter();
            let label = app.session.catalog.photo(*id).and_then(|ph| ph.label);
            let selected = state != lcc_engine::SelectionState::NotSelected;
            let base = if selected {
                t.cell_selected
            } else if resp.hovered() {
                t.cell_selected.gamma_multiply(0.6)
            } else {
                t.canvas
            };
            p.rect_filled(cr, 0.0, crate::theme::label_background(base, label, selected));
            let names = app.ui.settings.film_names;
            if let Some(ph) = app.session.catalog.photo(*id).filter(|_| names) {
                let name = ph.file_name.rsplit_once('.').map(|(n, _)| n).unwrap_or(&ph.file_name);
                p.text(pos2(cr.left() + 8.0, cr.top() + 10.0), Align2::LEFT_CENTER, film_label(name), t.font(10.0), t.text_dim);
                p.text(pos2(cr.right() - 8.0, cr.top() + 10.0), Align2::RIGHT_CENTER, &ph.format, t.semibold(8.5), t.text_dim);
            }
            let img_area = Rect::from_min_max(cr.min + vec2(10.0, 22.0), cr.max - vec2(10.0, 8.0));
            super::grid::request_thumb(app, *id, (256.0 * ppp.min(2.0) / 2.0) as usize * 2, 8);
            if let Some(tex) = app.renderer.thumb(*id) {
                let [tw, th] = tex.size;
                let s = (img_area.width() / tw as f32).min(img_area.height() / th as f32);
                let fr = Rect::from_center_size(img_area.center(), vec2(tw as f32 * s, th as f32 * s));
                p.image(tex.tex.id(), fr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                if sel {
                    p.rect_stroke(fr, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Outside);
                } else if state == lcc_engine::SelectionState::Selected {
                    p.rect_stroke(fr, 0.0, Stroke::new(1.5, Color32::from_gray(170)), StrokeKind::Outside);
                }
                if app.ui.settings.film_badges
                    && let Some(ph) = app.session.catalog.photo(*id)
                {
                    film_badges(p, &t, fr, ph);
                }
                if let Some(st) = app.session.catalog.stack_of(*id) {
                    let text =
                        if st.collapsed { st.photos.len().to_string() } else { format!("{}/{}", st.position(*id).unwrap_or(0) + 1, st.photos.len()) };
                    let g = p.layout_no_wrap(text, t.semibold(9.5), Color32::WHITE);
                    let br = Rect::from_min_size(fr.min + vec2(3.0, 3.0), vec2(g.size().x + 22.0, 15.0));
                    p.rect_filled(br, 7.5, Color32::from_black_alpha(170));
                    crate::icons::paint(p, Rect::from_min_size(br.min + vec2(4.0, 2.0), vec2(11.0, 11.0)), crate::icons::Icon::Stack, Color32::WHITE);
                    p.galley(pos2(br.min.x + 17.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
                }
            }
            if resp.clicked() {
                let m = ui.input(|i| i.modifiers);
                let compare = app.ui.view == crate::state::ViewMode::Compare;
                let r = if compare && !m.command && !m.shift {
                    // Compare: the clicked photo replaces the active side (or swaps with the other one)
                    app.run("compare.set", json!({"id": id.0}))
                } else if compare {
                    // ⌘/Ctrl- and Shift-clicks change the selection; the pair on screen stays (#187)
                    super::compare::select_with_modifier(app, *id, !m.command)
                } else {
                    let mode = if m.command {
                        "toggle"
                    } else if m.shift {
                        "range"
                    } else {
                        "replace"
                    };
                    app.run("library.select", json!({"ids": [id.0], "mode": mode}))
                };
                if compare && let Err(e) = r {
                    app.toast(ui.ctx(), e);
                }
            }
            // the same photo actions as the grid and loupe (Restore / Delete Permanently in Recently Deleted)
            resp.context_menu(|ui| super::grid::context_menu(app, ui, *id));
        }
    });
}

/// Rating, flag and edited badges along a filmstrip thumbnail's bottom edge (Settings → Interface).
fn film_badges(p: &egui::Painter, t: &Tokens, fr: Rect, ph: &lcc_catalog::Photo) {
    use crate::icons::{Icon, paint};
    use lcc_catalog::Flag;
    let edited = ph.is_edited();
    if ph.rating == 0 && ph.flag == Flag::None && !edited {
        return;
    }
    let bar = Rect::from_min_max(pos2(fr.left(), fr.bottom() - 15.0), fr.right_bottom());
    p.rect_filled(bar, 0.0, Color32::from_black_alpha(130));
    let y = bar.center().y;
    let mut x = bar.left() + 3.0;
    for i in 0..ph.rating {
        paint(p, Rect::from_min_size(pos2(x + i as f32 * 9.0, y - 4.0), vec2(8.0, 8.0)), Icon::StarFilled, t.star);
    }
    x += ph.rating as f32 * 9.0 + 2.0;
    match ph.flag {
        Flag::Pick => paint(p, Rect::from_min_size(pos2(x, y - 5.0), vec2(10.0, 10.0)), Icon::FlagPick, t.pick),
        Flag::Reject => paint(p, Rect::from_min_size(pos2(x, y - 5.0), vec2(10.0, 10.0)), Icon::FlagReject, t.reject),
        Flag::None => {}
    }
    if edited {
        paint(p, Rect::from_min_size(pos2(bar.right() - 13.0, y - 5.0), vec2(10.0, 10.0)), Icon::Sliders, t.text_label);
    }
}

#[cfg(test)]
mod preview_geometry_tests {
    use super::{fit_rect, fit_texture_rect};
    use crate::state::Zoom;
    use egui::{Rect, pos2, vec2};

    #[test]
    fn portrait_preview_keeps_its_ratio_inside_a_landscape_frame() {
        let frame = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0));
        let image = fit_texture_rect(frame, [100, 200]);

        assert!((image.width() / image.height() - 0.5).abs() < 1e-6);
        assert_eq!(image.center(), frame.center());
        assert!(frame.contains_rect(image));
    }

    /// A 100 % view must put the photo's corner on a whole physical pixel, or the GPU's bilinear
    /// filter softens it (a 1279.5 pt canvas on a 2x display centred the photo half a pixel off).
    #[test]
    fn percent_zoom_lands_on_whole_physical_pixels() {
        let area = Rect::from_min_size(pos2(0.0, 42.0), vec2(1279.5, 726.0));
        for ppp in [1.0, 1.25, 2.0, 3.0] {
            for pan in [(0.5, 0.5), (0.3, 0.7), (0.4999, 0.5001)] {
                let image = fit_rect(area, 5152.0 / 7728.0, Zoom::Percent(100.0), [5152, 7728], ppp, pan);
                for v in [image.min.x, image.min.y] {
                    assert!((v * ppp - (v * ppp).round()).abs() < 1e-3, "{v} pt is not a whole pixel at {ppp}x, pan {pan:?}");
                }
                assert!((image.width() * ppp - 5152.0).abs() < 1e-3, "size stays the photo's own pixels");
            }
        }
    }

    #[test]
    fn portrait_preview_aspect_fits_detail_area_without_stretching() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0));
        let image = fit_rect(area, 0.5, Zoom::Fit, [4000, 6000], 1.0, (0.5, 0.5));

        assert!((image.width() / image.height() - 0.5).abs() < 1e-6);
        assert_eq!(image.size(), vec2(100.0, 200.0));
        assert_eq!(image.center(), area.center());
    }
}

/// Straighten tool: drag along a horizon (or a vertical) to set the crop angle; double-click = Auto.
/// The image is shown unrotated in the crop view, so the line's on-screen angle is its image angle.
/// `held`: drawn with ⌘ held in the crop tool, which stays active afterwards.
/// The Straighten tool's level line. The crop tool shows the photo already turned by `angle`: a
/// line drawn on screen at some slope lies at that slope minus `angle` in the photo.
fn straighten_overlay(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, held: bool, angle: f64) {
    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    if !held && resp.double_clicked() {
        let _ = app.run("crop.autoStraighten", json!({}));
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
            // the line's slope in the photo, folded to the nearest axis: a line closer to vertical
            // straightens plumb lines
            let fold = |mut deg: f64| {
                while deg > 45.0 {
                    deg -= 90.0;
                }
                while deg < -45.0 {
                    deg += 90.0;
                }
                deg
            };
            let deg = v.y.atan2(v.x).to_degrees() as f64 - angle;
            if deg.is_finite() {
                let deg = fold(deg);
                let _ = app.run("crop.straighten", json!({"angle": (-deg * 100.0).round() / 100.0}));
            }
        }
        if !held {
            app.ui.tool.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    /// The rotation pointer's arc bulges away from the side of the crop box it is on, and follows
    /// a box that is itself rotated.
    #[test]
    fn rotate_cursor_faces_away_from_the_crop_box() {
        use egui::{Pos2, pos2};
        use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
        let near = |a: f32, b: f32| ((a - b + PI).rem_euclid(2.0 * PI) - PI).abs() < 1e-3;
        let pts = [pos2(100.0, 100.0), pos2(300.0, 100.0), pos2(300.0, 200.0), pos2(100.0, 200.0)];
        let turn = |x: f32, y: f32| super::rotate_cursor_turn(&pts, pos2(x, y), false);
        assert!(near(turn(200.0, 80.0), 0.0), "above: as drawn");
        assert!(near(turn(320.0, 150.0), FRAC_PI_2), "right");
        assert!(near(turn(200.0, 230.0), PI), "below");
        assert!(near(turn(70.0, 150.0), -FRAC_PI_2), "left");
        assert!(near(turn(290.0, 60.0), 0.0), "above the top edge near its end: still up");
        // off each corner: diagonally out from it
        assert!(near(turn(80.0, 80.0), -FRAC_PI_4), "top-left corner");
        assert!(near(turn(320.0, 80.0), FRAC_PI_4), "top-right corner");
        assert!(near(turn(320.0, 220.0), 3.0 * FRAC_PI_4), "bottom-right corner");
        assert!(near(turn(80.0, 220.0), -3.0 * FRAC_PI_4), "bottom-left corner");
        assert!(near(super::rotate_cursor_turn(&pts, pos2(200.0, 110.0), true), 0.0), "dragged back over the top edge: still up");
        // a box turned 90°: its top edge is now on the right
        let turned = [pts[3], pts[0], pts[1], pts[2]].map(|p| pos2(400.0 - p.y, p.x));
        assert!(near(super::rotate_cursor_turn(&turned, pos2(420.0, 200.0), false), FRAC_PI_2));
        // a box turned 30°: straight out from its top edge is 30° clockwise of up
        let c = pos2(200.0, 150.0);
        let rot = |p: Pos2| {
            let (s, co) = (30f32.to_radians().sin(), 30f32.to_radians().cos());
            let d = p - c;
            pos2(c.x + d.x * co - d.y * s, c.y + d.x * s + d.y * co)
        };
        let tilted = pts.map(rot);
        assert!(near(super::rotate_cursor_turn(&tilted, rot(pos2(200.0, 60.0)), false), 30f32.to_radians()));
    }

    use super::{film_label, flipped_corners, outside_crop_mesh, texture_side};
    use egui::{Color32, Pos2, Rect, pos2};

    /// A rotated `w` × `h` box centred on `centre`.
    fn corners(r: Rect) -> [Pos2; 4] {
        [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]
    }

    fn box_at(centre: Pos2, w: f32, h: f32, angle: f32) -> [Pos2; 4] {
        let (s, c) = angle.to_radians().sin_cos();
        [(-w, -h), (w, -h), (w, h), (-w, h)].map(|(x, y)| pos2(centre.x + (x * c - y * s) / 2.0, centre.y + (x * s + y * c) / 2.0))
    }

    /// Signed distances from a convex polygon's edges, positive inwards (its winding either way).
    fn sides(poly: &[Pos2], p: Pos2) -> Vec<f32> {
        let n = poly.len() as f32;
        let centre = pos2(poly.iter().map(|q| q.x).sum::<f32>() / n, poly.iter().map(|q| q.y).sum::<f32>() / n);
        (0..poly.len())
            .map(|i| {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                let nrm = egui::vec2(a.y - b.y, b.x - a.x).normalized();
                nrm.dot(p - a) * nrm.dot(centre - a).signum()
            })
            .collect()
    }

    /// Issue #731: the dimming is the photo minus the crop box at any angle. Every point of the
    /// photo outside the box lies in exactly one of the mesh's triangles; no point inside the box
    /// or outside the photo lies in any. (Four quads joining photo and box corners crossed and
    /// overlapped once the box was rotated.)
    #[test]
    fn the_dimming_outside_a_rotated_crop_is_exactly_the_photo_minus_the_box() {
        let img = Rect::from_min_max(pos2(100.0, 50.0), pos2(500.0, 650.0));
        // boxes inside the photo, poking out of it, and bigger than it; the photo upright (the box
        // turned) or turned under an upright box, as the crop tool shows it (issue #60)
        for (w, h) in [(240.0, 360.0), (420.0, 300.0), (700.0, 900.0)] {
            for angle in [0.0f32, 5.0, 15.0, 30.0, -45.0, 90.0, 137.0, 200.0] {
                let quad = box_at(img.center() + egui::vec2(30.0, -20.0), w, h, angle);
                assert_dims_photo_minus_box(&corners(img), &quad, w > img.width(), &format!("{w}×{h} box at {angle}°"));
                let turned = box_at(img.center(), img.width(), img.height(), -angle);
                let upright = box_at(img.center() + egui::vec2(30.0, -20.0), w, h, 0.0);
                assert_dims_photo_minus_box(&turned, &upright, w > img.width(), &format!("{w}×{h} box on a photo at {angle}°"));
            }
        }
    }

    /// Every sampled point of `photo` outside `quad` lies in exactly one of the dimming mesh's
    /// triangles; no point inside the box or outside the photo lies in any.
    fn assert_dims_photo_minus_box(photo: &[Pos2; 4], quad: &[Pos2; 4], swallowed: bool, what: &str) {
        let mesh = outside_crop_mesh(photo, quad, Color32::BLACK);
        let tris: Vec<[Pos2; 3]> = mesh
            .indices
            .chunks(3)
            .map(|t| [mesh.vertices[t[0] as usize].pos, mesh.vertices[t[1] as usize].pos, mesh.vertices[t[2] as usize].pos])
            .collect();
        let bounds = Rect::from_points(photo);
        let (mut inside, mut outside) = (0, 0);
        let mut y = bounds.top() - 20.0;
        while y < bounds.bottom() + 20.0 {
            let mut x = bounds.left() - 20.0;
            while x < bounds.right() + 20.0 {
                let p = pos2(x, y);
                let in_box = sides(quad, p);
                let in_img = sides(photo, p);
                // a pixel on an edge may fall either way: skip the boundaries themselves
                if in_box.iter().chain(&in_img).any(|d| d.abs() < 1.0) {
                    x += 7.0;
                    continue;
                }
                let expected = if in_img.iter().all(|d| *d > 0.0) && !in_box.iter().all(|d| *d > 0.0) { 1 } else { 0 };
                // a sample on the seam between two of a strip's fan triangles is in both or in
                // neither, depending on rounding: count it both ways
                let covered = |eps: f32| tris.iter().filter(|t| sides(*t, p).iter().all(|d| *d > eps)).count();
                let (at_least, at_most) = (covered(0.01), covered(-0.01));
                assert!(at_least <= expected && expected <= at_most, "{what}: ({x}, {y}) lies in {at_least}..={at_most} triangle(s), not {expected}");
                if expected == 1 {
                    outside += 1
                } else if in_img.iter().all(|d| *d > 0.0) {
                    inside += 1
                }
                x += 7.0;
            }
            y += 7.0;
        }
        // (a box bigger than the photo swallows it whole at some angles)
        assert!(inside > 50 && (outside > 50 || swallowed), "{what}: {outside} dimmed, {inside} clear samples");
    }

    /// A flat or broken box dims nothing rather than something arbitrary (or panicking).
    #[test]
    fn a_degenerate_crop_box_dims_nothing() {
        let img = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 300.0));
        let point = [pos2(10.0, 10.0); 4];
        let line = [pos2(10.0, 10.0), pos2(90.0, 10.0), pos2(90.0, 10.0), pos2(10.0, 10.0)];
        let nan = [pos2(f32::NAN, 10.0), pos2(90.0, 10.0), pos2(90.0, 50.0), pos2(10.0, 50.0)];
        for quad in [point, line, nan] {
            assert!(outside_crop_mesh(&corners(img), &quad, Color32::BLACK).indices.is_empty(), "{quad:?}");
        }
        assert!(outside_crop_mesh(&corners(Rect::NAN), &box_at(pos2(50.0, 50.0), 20.0, 20.0, 10.0), Color32::BLACK).indices.is_empty());
    }

    /// A raw's embedded JPEG shown while the raw decodes, or the last render while an edit renders,
    /// looked like the finished photo: the loupe says "Rendering…" once a render has been coming for
    /// a moment with nothing new on screen, and doesn't blink while slow drafts arrive.
    #[test]
    fn the_loupe_says_rendering_when_the_screen_falls_behind() {
        use super::{RENDERING_AFTER_SECS as WAIT, rendering_overdue};
        let key = |k| [Some(k), None, None, None];
        let mut wait = None;
        assert!(!rendering_overdue(&mut wait, key(1), false, 0.0), "nothing coming");
        // an edit after an idle spell: the wait starts now, not at the last frame drawn
        assert!(!rendering_overdue(&mut wait, key(1), true, 30.0), "a render just asked for");
        assert!(!rendering_overdue(&mut wait, key(1), true, 30.0 + WAIT * 0.8), "a quick render never shows it");
        assert!(rendering_overdue(&mut wait, key(1), true, 30.0 + WAIT * 1.2), "nothing new for the wait");
        assert!(rendering_overdue(&mut wait, key(2), true, 31.0 + WAIT), "once up, a slow draft doesn't blink it");
        assert!(!rendering_overdue(&mut wait, key(3), false, 32.0), "done");
        // a drag whose drafts keep up: always something coming, a new draft every 0.1 s
        for i in 1..30u32 {
            assert!(!rendering_overdue(&mut wait, key(u64::from(i)), true, 40.0 + f64::from(i) * 0.1), "draft {i}");
        }
    }

    /// Issue #652: a native host reports the GPU's texture limit in the first frame's input only.
    /// The loupe must still know it in every later frame (it read the raw input, found nothing and
    /// cut every window render to 2048 px).
    #[test]
    fn the_texture_limit_outlives_the_frame_it_was_reported_in() {
        let ctx = egui::Context::default();
        assert_eq!(texture_side(&ctx), 2048, "until the host says: the smallest limit there is");
        let mut seen = Vec::new();
        for reported in [Some(8192), None, None] {
            let raw = egui::RawInput { max_texture_side: reported, ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| seen.push(texture_side(ui.ctx())));
            out.textures_delta.clear();
        }
        assert_eq!(seen, [8192, 8192, 8192]);
    }

    #[test]
    fn film_labels_cut_on_characters_not_bytes() {
        // the name from #266: byte 13 falls inside '限'
        assert_eq!(film_label("202407層三限定訂閱圖(4)"), "202407層三限定訂閱圖…");
        assert_eq!(film_label("写真"), "写真");
        // exactly 14 characters (42 bytes) is shown whole
        assert_eq!(film_label("一二三四五六七八九十一二三四"), "一二三四五六七八九十一二三四");
        assert_eq!(film_label("IMG_20240712_153012"), "IMG_20240712_…");
        assert_eq!(film_label("DSC_0001"), "DSC_0001");
        assert_eq!(film_label(""), "");
    }

    /// With the photo flipped, the crop box's corners reach the screen mirrored; the dimming
    /// outside the box pairs each photo corner with the box corner on the same side of the screen
    /// (it used to pair them in data order, so a flipped photo was dimmed inside the box).
    #[test]
    fn flipped_crop_box_corners_come_in_screen_order() {
        let screen = [pos2(10.0, 10.0), pos2(90.0, 10.0), pos2(90.0, 50.0), pos2(10.0, 50.0)];
        // data order (top-left, top-right, bottom-right, bottom-left) as each flip shows it
        let h = [screen[1], screen[0], screen[3], screen[2]];
        let v = [screen[3], screen[2], screen[1], screen[0]];
        let hv = [screen[2], screen[3], screen[0], screen[1]];
        assert_eq!(flipped_corners(&screen, false, false), screen);
        assert_eq!(flipped_corners(&h, true, false), screen);
        assert_eq!(flipped_corners(&v, false, true), screen);
        assert_eq!(flipped_corners(&hv, true, true), screen);
    }
}

#[cfg(test)]
mod wheel_zoom_tests {
    use super::*;

    fn wheel(unit: egui::MouseWheelUnit, y: f32, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::MouseWheel { unit, delta: egui::vec2(0.0, y), phase: egui::TouchPhase::Move, modifiers }
    }

    #[test]
    fn only_notched_unmodified_wheels_count() {
        let m = egui::Modifiers::NONE;
        assert_eq!(wheel_notches(&[wheel(egui::MouseWheelUnit::Line, 1.0, m), wheel(egui::MouseWheelUnit::Line, 2.0, m)]), 3.0);
        assert_eq!(wheel_notches(&[wheel(egui::MouseWheelUnit::Point, 40.0, m)]), 0.0);
        assert_eq!(wheel_notches(&[wheel(egui::MouseWheelUnit::Line, 1.0, egui::Modifiers::CTRL)]), 0.0);
        assert_eq!(wheel_notches(&[wheel(egui::MouseWheelUnit::Line, f32::NAN, m)]), 0.0);
    }
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

#[cfg(test)]
mod info_overlay_tests {
    use super::*;

    /// #1690: the info overlay's shadow is a dark copy under the text. Drawn as a second white
    /// copy one point off, each line read doubled.
    #[test]
    fn the_info_overlay_shadow_is_dark() {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.ui.info_overlay = crate::state::InfoOverlay::Basic;
        let photo = app.session.catalog.photos().next().map(|p| (**p).clone()).expect("the demo catalog has photos");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let canvas = Rect::from_min_size(pos2(52.0, 82.0), vec2(800.0, 500.0));
        let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| info_overlay(&app, ui.painter(), canvas, &photo));
        out.textures_delta.clear();
        // the colour a text shape paints its glyphs in, as epaint resolves it
        let ink = |s: &egui::epaint::TextShape| {
            let c = s.galley.job.sections.first().map_or(Color32::PLACEHOLDER, |sec| sec.format.color);
            s.override_text_color.unwrap_or(if c == Color32::PLACEHOLDER { s.fallback_color } else { c })
        };
        fn texts(shape: &egui::epaint::Shape, out: &mut Vec<egui::epaint::TextShape>) {
            match shape {
                egui::epaint::Shape::Text(t) => out.push(t.clone()),
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        out.shapes.iter().for_each(|c| texts(&c.shape, &mut all));
        let copies: Vec<Color32> = all.iter().filter(|t| t.galley.job.text == photo.file_name).map(ink).collect();
        assert_eq!(copies.len(), 2, "the file name and its shadow: {copies:?}");
        assert_eq!(copies[1], Color32::WHITE, "the text on top");
        assert!(copies[0].r() < 64 && copies[0].a() > 0, "the shadow under it is dark: {:?}", copies[0]);
    }
}
