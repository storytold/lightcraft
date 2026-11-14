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

/// Whether the preview or thumbnail may stand in for `slot` until its own render arrives. They show
/// the photo's current settings, so for the Before view only while those are its before settings:
/// an edited photo would otherwise flash its edits before the Before render replaces them
/// (upstream #2248).
pub(crate) fn stand_in_shows(app: &LccApp, id: PhotoId, slot: Slot) -> bool {
    slot != Slot::Before || app.session.catalog.photo(id).is_some_and(|ph| *ph.develop == app.session.before_settings(ph))
}

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

/// How long (s) after a brush stroke the photographer still counts as painting ([`crate::LccApp::paint_hold_until`]):
/// long enough for the next stroke to start.
pub(crate) const PAINT_HOLD_SECS: f64 = 1.0;

/// An in-progress on-canvas gesture.
#[derive(Clone, Debug)]
pub enum Gesture {
    Brush {
        points: Vec<Point>,
        /// Pen pressure (0..1) at each point; empty when no pen pressure was reported (a mouse) or the
        /// brush's Pen Pressure switch is off.
        pressure: Vec<f32>,
        /// Whether each point was painted erasing (Erase picked, or ⌥ / Alt held then): the
        /// gesture goes to `mask.brushStroke` as `runs` of one mode each.
        erase: Vec<bool>,
        /// Strokes already sent live for this gesture (a mask with settings shows its effect while
        /// painting); the next send replaces them. 0: nothing sent yet.
        sent: usize,
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
    /// Guided Upright: dragging the endpoint `end` (0 = `guide.0`, 1 = `guide.1`) of guide `i`,
    /// from where the pointer went down (`from` — egui forgets the press origin at the release).
    GuideEndpoint {
        i: usize,
        end: usize,
        from: Pos2,
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
    /// Colour / Luminance Range: an area being dragged from `a` (normalized) to sample.
    RangeArea {
        a: Point,
    },
    /// Colour Range: pin `index` of component `comp` being dragged to another place.
    ColorPin {
        comp: usize,
        index: usize,
    },
}

/// Screen ↔ normalized-image mapping for the displayed frame.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasMap {
    rect: Rect,
    to_norm: Affine,
    from_norm: Affine,
    k: f32,
    /// The frame's lens / perspective warp (with the oriented size it works in), when it moves
    /// pixels: a mask on the photo (`Mask::on_photo`) has its coordinates before it.
    warp: Option<std::sync::Arc<(lcc_pipeline::optics::Warp, f64, f64)>>,
    /// Whether `norm` / `screen` speak a mask's coordinates on the photo ([`CanvasMap::for_mask`]).
    on_photo: bool,
}

impl CanvasMap {
    fn new(frame: &Frame, rect: Rect) -> CanvasMap {
        let k = 8.0f32;
        let (w, h) = ((rect.width() * k).round().max(1.0) as usize, (rect.height() * k).round().max(1.0) as usize);
        let to_norm = frame.out_to_norm(w, h);
        let warp = frame.warp.as_ref().filter(|w| w.moves_pixels()).map(|w| std::sync::Arc::new((w.clone(), frame.ow, frame.oh)));
        CanvasMap { rect, to_norm, from_norm: to_norm.inverse().unwrap_or(Affine::IDENTITY), k, warp, on_photo: false }
    }
    /// The mapping for a mask's coordinates: on the photo before lens corrections, Upright and
    /// Transform (`on_photo`: every mask made now), or an old mask's on the corrected frame.
    pub(crate) fn for_mask(&self, on_photo: bool) -> CanvasMap {
        CanvasMap { on_photo, ..self.clone() }
    }
    /// [`Self::for_mask`] for mask `id` of `d` (none, or a new mask: on the photo).
    pub(crate) fn for_mask_id(&self, d: &DevelopSettings, id: Option<u32>) -> CanvasMap {
        self.for_mask(id.and_then(|id| d.masks.iter().find(|m| m.id == id)).is_none_or(|m| m.on_photo))
    }
    pub fn norm(&self, p: Pos2) -> Point {
        let n = self.to_norm.apply(Point::new(((p.x - self.rect.left()) * self.k) as f64, ((p.y - self.rect.top()) * self.k) as f64));
        match self.warp.as_deref().filter(|_| self.on_photo) {
            Some((w, ow, oh)) => {
                let s = w.to_source(Point::new(n.x * ow, n.y * oh), 1);
                Point::new(s.x / ow, s.y / oh)
            }
            None => n,
        }
    }
    /// Whether `norm` / `screen` go through a lens or perspective warp (a mask's coordinates on a
    /// warped photo).
    pub(crate) fn warped(&self) -> bool {
        self.on_photo && self.warp.is_some()
    }
    pub fn screen(&self, n: Point) -> Pos2 {
        self.try_screen(n).unwrap_or_else(|| self.place(n))
    }
    /// [`Self::screen`], or `None` where the warp can't be undone at `n` (a point a strong
    /// warp folds away), for drawing that must not join such a point to its neighbours.
    pub(crate) fn try_screen(&self, n: Point) -> Option<Pos2> {
        let n = match self.warp.as_deref().filter(|_| self.on_photo) {
            Some((w, ow, oh)) => w.from_source(Point::new(n.x * ow, n.y * oh)).map(|t| Point::new(t.x / ow, t.y / oh))?,
            None => n,
        };
        Some(self.place(n))
    }
    /// Screen points per unit of the oriented photo's long edge: the unit of brush and spot sizes.
    /// The render measures them on the uncropped photo (`Frame::px_per_long`), so a cursor sized from
    /// the cropped frame on screen would draw smaller than the stroke it paints.
    pub(crate) fn per_long(&self) -> f32 {
        let o = self.place(Point::new(0.0, 0.0));
        let (sx, sy) = (self.place(Point::new(1.0, 0.0)).distance(o), self.place(Point::new(0.0, 1.0)).distance(o));
        sx.max(sy).max(1e-3)
    }
    /// A point on the displayed frame (normalized) on screen.
    fn place(&self, n: Point) -> Pos2 {
        let q = self.from_norm.apply(n);
        pos2(self.rect.left() + q.x as f32 / self.k, self.rect.top() + q.y as f32 / self.k)
    }
}

/// How the crop tool holds its view still while the crop changes (issue #60). As in Lightroom the
/// crop box stays upright on screen and the photo turns and moves beneath it.
#[derive(Clone, Debug, PartialEq)]
pub enum CropLayout {
    /// Straightening, rotating or moving: the box keeps its centre on screen (`centre`) and the
    /// scale (`k` points per pixel of the oriented photo); the photo turns and slides under it.
    Pinned { centre: Pos2, k: f32 },
    /// Resizing: the photo stays where it is and the box follows the handle.
    Frozen(CanvasMap),
}

/// The crop tool's view of the photo: the crop box upright, the whole photo around it, turned by
/// the straighten angle.
#[derive(Clone, Debug)]
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
/// otherwise the turned photo at `zoom` (fitted into `area` unless a percentage) and `pan`. `frame` is the uncropped frame the loupe renders.
pub(crate) fn crop_view(
    frame: &Frame,
    crop: lcc_geom::CropGeometry,
    area: Rect,
    layout: Option<CropLayout>,
    zoom: Zoom,
    ppp: f32,
    pan: (f32, f32),
) -> CropView {
    let cf = Frame { crop, ..frame.clone() };
    let (nw, nh) = cf.native_size();
    let (nw, nh) = ((nw as f32).max(1e-3), (nh as f32).max(1e-3));
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
                    // scaled to the zoom asked for (or to fit the area) and placed as `pan` says
                    let unit = CanvasMap::new(&cf, Rect::from_center_size(Pos2::ZERO, vec2(nw, nh)));
                    let b = Rect::from_points(&corners.map(|c| unit.screen(c)));
                    let k = match zoom {
                        Zoom::Percent(p) if finite(p) && finite(ppp) => p / 100.0 / ppp,
                        _ => (area.width() / b.width()).min(area.height() / b.height()),
                    };
                    let k = if finite(k) { k } else { 1.0 };
                    let size = b.size() * k;
                    let pan = bounded_pan(area, size, pan);
                    let shift = vec2((pan.0 - 0.5) * size.x, (pan.1 - 0.5) * size.y);
                    (area.center() - shift - b.center().to_vec2() * k, k)
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
    // Fit has one physical-pixel rectangle for every render quality and output mode.
    // Snapping only display-sized textures moved the image when a draft replaced them.
    if matches!(zoom, Zoom::Fit) {
        return snap_fit_rect(rect, ppp);
    }
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

/// A develop render uses the photo's bounds, not its texture's rounded aspect ratio.
/// A genuinely different crop still fits inside those bounds instead of stretching.
fn render_texture_rect(area: Rect, size: [usize; 2]) -> Rect {
    let aspect = size[0].max(1) as f32 / size[1].max(1) as f32;
    if crate::region::same_aspect(aspect, area.width() / area.height().max(f32::MIN_POSITIVE)) { area } else { fit_texture_rect(area, size) }
}

/// Align the fitted photo, independently of the texture used to display it. Round the
/// physical dimensions first, then its centred origin, so every quality has identical bounds.
pub(crate) fn snap_fit_rect(rect: Rect, ppp: f32) -> Rect {
    let ppp = if ppp.is_finite() && ppp > 0.0 { ppp } else { 1.0 };
    let size = vec2((rect.width() * ppp).round().max(1.0), (rect.height() * ppp).round().max(1.0));
    let origin = rect.center().to_vec2() * ppp - size * 0.5;
    Rect::from_min_size(pos2(origin.x.round() / ppp, origin.y.round() / ppp), size / ppp)
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

/// A drag with the middle button (the mouse wheel pressed) pans the zoomed photo whatever tool is
/// active, so the healing brush, mask brushes and crop never paint or move while you look around;
/// the hand cursor shows while it does.
fn middle_pan(app: &mut LccApp, ui: &egui::Ui, resp: &egui::Response, area: Rect, img: Rect) -> bool {
    if !resp.dragged_by(egui::PointerButton::Middle) {
        return false;
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    if img.width() > area.width() + 1.0 || img.height() > area.height() + 1.0 {
        pan_image(app, area, img, resp.drag_delta());
    }
    true
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

/// Net delta (up/right is positive) of unmodified point-unit wheel events: a trackpad scroll, not a notched mouse wheel.
fn trackpad_scroll_delta(events: &[egui::Event]) -> egui::Vec2 {
    events
        .iter()
        .filter_map(|e| match e {
            egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, modifiers, .. }
                if *modifiers == egui::Modifiers::NONE && delta.is_finite() =>
            {
                Some(*delta)
            }
            _ => None,
        })
        .fold(egui::Vec2::ZERO, |acc, d| acc + d)
}

/// Native pinch (also modifier-wheel zoom) and two-finger scroll, scoped to this image view.
/// Brush resize per wheel notch (or [`crate::theme::WHEEL_LINE_POINTS`] of trackpad scroll), as
/// a log-scale step: about +25 % per notch.
const BRUSH_PER_NOTCH: f32 = 0.22;

pub(crate) fn navigate_gesture(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, area: Rect, img: Rect, native: [usize; 2]) -> bool {
    // While the White Balance Selector is armed, a wheel notch steps its sample size instead of zooming
    // (OneSlips, test notes 2026-10-25 note 13); Ctrl/Cmd+wheel still zooms.
    if app.ui.tool == "wbPicker" && !ui.input(|i| i.modifiers.command) {
        let notches = ui.input(|i| wheel_notches(&i.events));
        if notches != 0.0 {
            let at = WB_SAMPLES.iter().position(|v| *v == app.ui.wb_sample).unwrap_or(2) as isize;
            let next = (at + notches.signum() as isize).clamp(0, WB_SAMPLES.len() as isize - 1) as usize;
            app.ui.wb_sample = WB_SAMPLES.get(next).copied().unwrap_or(5);
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            ui.ctx().request_repaint();
            return true;
        }
    }

    // While Space is held over a tool, `loupe:hand` sits above `loupe` and takes the hover from it;
    // wheel/scroll gestures over the canvas still belong to the view.
    if !resp.hovered() && !(app.space_hand.is_some() && resp.contains_pointer()) {
        return false;
    }
    let (mut factor, mut delta, anchor) =
        ui.input(|i| (i.zoom_delta(), i.translation_delta(), i.multi_touch().map(|t| t.center_pos).or(i.pointer.hover_pos())));

    let notches = ui.input(|i| wheel_notches(&i.events));
    let trackpad_delta = ui.input(|i| trackpad_scroll_delta(&i.events));
    if delta == egui::Vec2::ZERO && trackpad_delta != egui::Vec2::ZERO {
        delta = trackpad_delta;
    }
    let is_brush = app.ui.tool == "brush" || app.ui.right == RightPanel::Remove;
    let space_held = ui.input(|i| i.key_down(egui::Key::Space)) || app.space_hand.is_some();
    let cmd_or_ctrl = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);

    // With the Masking or Remove brush active, a plain wheel notch or two-finger trackpad scroll
    // resizes the brush (like `[` / `]`) instead of zooming or panning (Preferences → Interface).
    // Holding Space lends the hand tool back so the scroll gesture moves the frame around instead;
    // Ctrl/Cmd+wheel still zooms.
    if app.ui.settings.wheel_resizes_brush && is_brush && !cmd_or_ctrl && !space_held {
        if notches != 0.0 {
            crate::menus::adjust_brush(app, (notches * BRUSH_PER_NOTCH).exp().clamp(0.25, 4.0), 0.0);
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            ui.ctx().request_repaint();
            // Not a view change: the frame still draws the tool overlay, so the brush ring stays
            // shown instead of flickering to the pointer while it resizes (#867).
            return false;
        }
        if trackpad_delta.y != 0.0 {
            // Scales with the scroll distance, so a trackpad's many small deltas and a wheel's line
            // add up to the same growth per distance scrolled (#867).
            let k = (trackpad_delta.y * (BRUSH_PER_NOTCH / crate::theme::WHEEL_LINE_POINTS)).exp().clamp(0.25, 4.0);
            crate::menus::adjust_brush(app, k, 0.0);
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            ui.ctx().request_repaint();
            return false;
        }
        if factor == 1.0 && delta != egui::Vec2::ZERO {
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            delta = egui::Vec2::ZERO;
        }
    }

    if space_held && delta != egui::Vec2::ZERO {
        crate::space_hand::mark_used(app);
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
    let point = focus_box(app.session.catalog.photo(id)?).map(|r| (((r.x0 + r.x1) / 2.0) as f32, ((r.y0 + r.y1) / 2.0) as f32));
    // the main subject's sharpest open eye, else the largest face, once the eye finder has looked
    // at the photo (with the eye strip off too: `wants_eyes`)
    let eye = app.session.eye_job(id).and_then(|j| app.eye_strip.report(j.key)).and_then(|r| r.zoom_target()).map(|[x, y]| (x, y));
    space_target(app.ui.settings.space_zoom, point, eye)
}

/// Where Space zooms to from the camera's focus point and the main subject's eye (either may be
/// missing), as View ▸ Space Zooms To says; `None`: where the photo was zoomed last.
pub(crate) fn space_target(mode: crate::state::SpaceZoom, point: Option<(f32, f32)>, eye: Option<(f32, f32)>) -> Option<(f32, f32)> {
    match mode {
        crate::state::SpaceZoom::FocusPoint => point.or(eye),
        crate::state::SpaceZoom::Eye => eye.or(point),
    }
}

/// Whether Toggle Zoom on photo `id` may want its eyes found ([`focus_pan`]): Zoom to Focus Point
/// on, and the eyes asked for first or no focus point recorded. The eyes are then looked for in the
/// background while the photo is on screen, eye strip or not.
pub(crate) fn wants_eyes(app: &LccApp, id: PhotoId) -> bool {
    app.ui.settings.zoom_to_focus
        && (app.ui.settings.space_zoom == crate::state::SpaceZoom::Eye || app.session.catalog.photo(id).is_some_and(|p| focus_box(p).is_none()))
}

/// Draw the camera's focus box over a photo drawn in `img` (View ▸ Focus Point): green corner
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
    let t = if *anim && !crate::motion::reduced(ctx) { 0.22 } else { 0.0 };
    let id = egui::Id::new("loupe_anim");
    let v = |k: &str, x: f32| ctx.animate_value_with_time(id.with(k), x, t);
    let (c, s) = (target.center(), target.size());
    let r = Rect::from_center_size(pos2(v("cx", c.x), v("cy", c.y)), vec2(v("w", s.x), v("h", s.y)));
    if *anim && (r.center() - c).abs().max_elem() < 0.5 && (r.size() - s).abs().max_elem() < 0.5 {
        *anim = false;
    }
    r
}

/// View Options' Show Briefly When Photo Changes: note when the loupe changes to photo `id`, and
/// whether the brief info overlay shows this frame (until [`BRIEF_INFO_SECS`] have passed; a
/// repaint is asked for then, so it hides without input).
///
/// [`BRIEF_INFO_SECS`]: crate::state::BRIEF_INFO_SECS
fn brief_info(app: &mut LccApp, ctx: &egui::Context, id: PhotoId) {
    let now = ctx.input(|i| i.time);
    let brief = &mut app.ui.brief_info;
    if brief.photo != Some(id) {
        brief.photo = Some(id);
        brief.since = now;
    }
    let left = crate::state::BRIEF_INFO_SECS - (now - brief.since);
    brief.shown = app.ui.settings.loupe_info.briefly != crate::state::InfoOverlay::Off && left > 0.0;
    if brief.shown {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
    }
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

/// The long edge of the loupe's render at rest: no less than the preview source level, so the
/// source is copied 1:1 (no resample in the pipeline, whose prefilter and bilinear pass soften
/// detail near the output's limit) and shrunk once, by the Lanczos display fit. A user's explicit
/// Preview size below the preview level still caps it.
fn rest_edge(settings: &crate::state::AppSettings, main_edge: usize, native_long: usize, texture_side: usize) -> usize {
    let limit = if settings.preview_limit == 0 { usize::MAX } else { settings.preview_limit as usize };
    let floor = lcc_engine::SourceLevel::Preview.max_edge().min(limit).min(native_long);
    main_edge.max(floor).min(texture_side.max(crate::state::MIN_TEXTURE_SIDE))
}

/// The render box of long edge `edge` for a frame of `aspect`. The short side is rounded, not
/// truncated: a truncated box (2560 x 1706 for a 2560 x 1707 source) made the render a slightly
/// anisotropic resample of its source, a 2-tap blur along one axis only. A box that matches the
/// source to rounding is rendered at exactly its size.
fn render_box(edge: usize, aspect: f32) -> (usize, usize) {
    let short = |e: usize, a: f32| ((e as f32 / a).round() as usize).max(1);
    if aspect >= 1.0 { (edge, short(edge, aspect)) } else { (short(edge, 1.0 / aspect), edge) }
}

/// Photo `id`'s loupe render at rest: `size` (from [`rest_edge`] and [`render_box`]), shrunk to
/// exactly the `fit` it is drawn at with a Lanczos filter (on the GPU when it renders there), so
/// the window draws it texel for texel instead of shrinking it with 2-tap sampling. The loupe and
/// the prefetch of a neighbour build it alike, so the step to that photo finds it ready.
fn rest_job(
    app: &mut LccApp,
    id: PhotoId,
    size: (usize, usize),
    fit: [usize; 2],
    overlay: lcc_pipeline::Overlay,
    crop_tool: bool,
) -> Option<lcc_engine::media::RenderJob> {
    let job = app.session.loupe_job(id, size.0.max(8), size.1.max(8), !crop_tool)?.with_display_fit(fit[0], fit[1]);
    let job = job.with_overlay(overlay).with_proof(app.ui.soft_proof.then_some(app.ui.proof));
    Some(job.with_hidden_grading(app.ui.grading_hidden_for(id)))
}

/// Share of a frame the loupe waits for a slider drag's update to finish so it is drawn in that
/// frame (`Renderer::wait_for_draft`): a draft, and a full-quality update (high quality adjustment
/// previews), which takes longer but leaves the rest of the frame as cheap.
const DRAFT_WAIT_SHARE: f32 = 0.4;
const FULL_WAIT_SHARE: f32 = 0.6;

/// How long a frame of `frame` seconds (egui's predicted frame time) waits for a slider drag's
/// update: a draft's share, or a full-quality update's.
pub(crate) fn wait_budget(frame: f32, drafting: bool) -> std::time::Duration {
    let frame = frame.clamp(1.0 / 240.0, 1.0 / 30.0);
    let share = if drafting { DRAFT_WAIT_SHARE } else { FULL_WAIT_SHARE };
    // (a frame time that is not a number waits for nothing)
    std::time::Duration::try_from_secs_f32(frame * share).unwrap_or_default()
}
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
        let job = if c.interacting && adjust_drafts(app) { job.draft() } else { job };
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
        // (an HDR window drawn in its place, or on its way, needs no 8-bit draft as well)
        if matches!(hdr_window(app, c, v, full), HdrPane::Sdr) {
            let full_quality = job.request.quality == lcc_pipeline::Quality::Full;
            let geom = crate::render::WindowGeom { photo: c.id, full, window, look: c.look, full_quality };
            app.renderer.request_window(v.pane, job, geom, TILE_PRIORITY);
        }
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
    // the part of the frame on screen: the tiles that cover it
    let mut cover: Option<(usize, usize, usize, usize)> = None;
    for rect in tiles.iter().filter(|t| t.1).filter_map(|t| crate::region::tile_rect(full, side, t.0)) {
        let (x1, y1) = (rect.x + rect.w, rect.y + rect.h);
        cover = Some(cover.map_or((rect.x, rect.y, x1, y1), |(a, b, cc, d)| (a.min(rect.x), b.min(rect.y), cc.max(x1), d.max(y1))));
    }
    let (x0, y0, x1, y1) = cover?;
    let window = lcc_engine::pipeline::PixelWindow { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
    // an HDR edit shown in HDR: its HDR window (or the whole frame's HDR until it comes) is what
    // shows, so no 8-bit tiles are rendered beside it (they would only hold it up)
    let sdr = matches!(hdr_window(app, c, v, full), HdrPane::Sdr);
    if !sdr {
        budget = 0;
    }
    // After an edit here (a slider let go, a preset): the part on screen once more as one window
    // at full quality, in the pane's own slot, whose stage cache the drag kept warm, in place of a
    // render for each tile on screen, each from the original with no stage cache (a release took
    // 0.2-4.9 s). A high quality drag's last update is that window already. The ring of tiles
    // around it is still prepared for a pan, and a pan past the window tiles as before.
    let vis = visible_px(full, v.target, v.visible);
    let mut window_stands_in = false;
    if sdr && let Some((_, g)) = app.renderer.window_texture(v.pane).filter(|(_, g)| g.photo == c.id && g.full == full) {
        let covers = |w: lcc_engine::pipeline::PixelWindow| {
            w.x as f32 <= vis.0.max(0.0)
                && w.y as f32 <= vis.1.max(0.0)
                && (w.x + w.w) as f32 >= vis.2.min(full.0 as f32)
                && (w.y + w.h) as f32 >= vis.3.min(full.1 as f32)
        };
        if g.look == c.look && g.full_quality && covers(g.window) {
            window_stands_in = true;
        } else if (g.look != c.look || !g.full_quality)
            && let Some(window) = crate::region::window_for(full.0, full.1, vis, crate::region::max_span(c.texture_side))
            && let Some(job) = job_for(app, window)
        {
            let geom = crate::render::WindowGeom { photo: c.id, full, window, look: c.look, full_quality: true };
            app.renderer.request_window(v.pane, job, geom, TILE_PRIORITY + 1);
            // (the tiles on screen wait for it; if it fails, they render)
            window_stands_in = app.renderer.failure_of(Slot::Window(v.pane), c.id).is_none();
        }
        wanted.push(Slot::Window(v.pane));
    }
    let placeholder = c
        .full_preview
        .as_ref()
        .filter(|p| budget > 0 && !v.before && c.overlay == lcc_pipeline::Overlay::None && c.proof.is_none() && (p.width, p.height) == full);
    for &(tile, on_screen) in &tiles {
        let Some(rect) = crate::region::tile_rect(full, side, tile) else { continue };
        let key = crate::region::TileKey { photo: c.id, pane: v.pane, full, tile, look };
        if on_screen
            && let Some(p) = placeholder
            && !c.interacting
            && let Some(part) = crate::region::crop_tile(p, lcc_engine::pipeline::PixelWindow { x: 0, y: 0, w: p.width, h: p.height }, rect)
        {
            app.renderer.tile_placeholder(ctx, key, &part);
        }
        if app.renderer.tiles.has_render(&key) {
            continue;
        }
        let slot = Slot::Tile { pane: v.pane, col: tile.col, row: tile.row };
        if budget == 0 || (on_screen && window_stands_in) {
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
    Some(crate::region::RegionView { photo: c.id, before: v.before, key: look, full, window, settings: look, tile: side })
}

/// Draw the tiles of `view` that are on screen over the whole-frame render: each its own render,
/// or (in a drag) the newest of its place, or its part of the 1:1 preview. The pane's drag window
/// goes under them while it shows the current settings, and alone while a slider drags. An HDR
/// edit on an HDR display draws its HDR window ([`hdr_window`]) instead, and nothing while that
/// is on its way: the whole frame's HDR render under it shows, softer but just as bright.
pub(crate) fn draw_window(p: &egui::Painter, app: &mut LccApp, c: &WindowCtx, v: &WindowView, view: &crate::region::RegionView) {
    let full = view.full;
    let (fw, fh) = (full.0 as f32, full.1 as f32);
    let place = |w: lcc_engine::pipeline::PixelWindow| {
        let at = v.img.min + vec2(w.x as f32 / fw * v.img.width(), w.y as f32 / fh * v.img.height());
        Rect::from_min_size(at, vec2(w.w as f32 / fw * v.img.width(), w.h as f32 / fh * v.img.height()))
    };
    if view.photo == c.id {
        match hdr_window(app, c, v, full) {
            // (an older window, while the new one renders, where it belongs: after a pan only the
            // part newly on screen is soft)
            HdrPane::Frame(tex) => {
                p.image(tex.tex, place(tex.window.unwrap_or(view.window)), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                return;
            }
            HdrPane::Pending => return,
            HdrPane::Sdr => {}
        }
    }
    let mut drag_drawn = false;
    if let Some((tex, g)) = app.renderer.window_texture(v.pane)
        && g.photo == c.id
        && g.full == full
        && (c.interacting || g.look == c.look)
    {
        p.image(tex.id(), place(g.window), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
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
        p.image(tex.id(), dst, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
}

pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let full = ui.max_rect();
    let fullscreen = app.ui.fullscreen;
    let show_film = crate::module::edge_visible(app, crate::module::Edge::Bottom) && !fullscreen;
    let film_h = if show_film { app.ui.film_height() } else { 0.0 };
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
    brief_info(app, ui.ctx(), id);
    let d = (*photo.develop).clone();
    // the full-screen preview shows the photo only: no tool overlays; nor does the Import tab's loupe, which has no
    // Develop panel (a tool left open there drew its overlays and notes over a browsed photo)
    let browsing = crate::import_tab::showing(app);
    let right = if fullscreen || browsing { RightPanel::None } else { app.ui.right };
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
    // where the view's picture lies in the photo now: any picture of it that says where it lies is
    // drawn cut to that, at once (a synced or new crop shows with no render)
    let view_place = app.session.placement_of(id, !crop_tool);
    let placed = |t: &crate::render::Tex| t.placement.is_some_and(|p| lcc_engine::placement::fits_shape(&p, t.size[0], t.size[1]));
    let visible_texture_aspect = app
        .renderer
        .textures
        .get(&Slot::Main)
        .filter(|t| t.photo == id)
        .or_else(|| app.renderer.textures.get(&Slot::Preview).filter(|t| t.photo == id))
        .or_else(|| app.renderer.thumb(id))
        // (a picture that says where it lies is drawn in the photo's own shape)
        .filter(|t| !(view_place.is_some() && placed(t)))
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
        if navigate_gesture(app, ui, &resp, view_area, view_img, native) || middle_pan(app, ui, &resp, view_area, view_img) {
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
    let crop_view = crop_tool.then(|| crop_view(&frame, shown_crop(&d, &frame), main_area, app.crop_layout.clone(), app.ui.zoom, ppp, app.ui.pan));
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
    let now = ui.input(|i| i.time);
    // Quality and scheduling are separate: high quality adjustment previews still coalesce a
    // drag's updates and render one visible window, but at the settled render's quality, size and
    // display fit, until they are too slow; then the moving slider drafts and a pause renders the
    // settled quality (`region::HqDrag`)
    let hq = app.ui.settings.high_quality_adjustment_previews;
    let hq_drag = app.session.interaction.as_ref().filter(|_| hq).map(|i| (i.photo, i.label.clone()));
    let (adjust, settle_in) = app.hq_drag.frame(hq_drag.as_ref().map(|(p, l)| (*p, l.as_str())), d.hash64(), now);
    if let Some(wait) = settle_in.and_then(|s| std::time::Duration::try_from_secs_f64(s).ok()) {
        ui.ctx().request_repaint_after(wait);
    }
    // a still photo in an open interaction (a slider held, a control surface's gesture) refines
    // to the full render after a moment (`region::StillRefine`)
    let full_ms = app.renderer.last_full_main_ms;
    let changing = app.still_refine.drafting(interacting, id, d.hash64(), now, full_ms);
    if changing && let Some(left) = app.still_refine.refine_in(now, full_ms).and_then(|s| std::time::Duration::try_from_secs_f64(s).ok()) {
        ui.ctx().request_repaint_after(left);
    }
    let drafting = interacting && changing && (!hq || adjust.drafts());
    // (a drag that fell back is one drag for `DragScale` across its pauses, so its draft size holds)
    let draft_drag = interacting && (!hq || app.hq_drag.fell_back());
    let overlay = view_overlay(app, &d, ui.input(|i| i.modifiers.alt));
    // an overlay hidden while a mask slider worked comes back once it rests, input or not
    if let Some(left) = app.ui.mask_adjusting_until.map(|t| t.saturating_duration_since(std::time::Instant::now())).filter(|d| !d.is_zero()) {
        ui.ctx().request_repaint_after(left);
    }
    // a picked mask's peek overlay goes again when its moment is up (issue #1109)
    if let Some(left) = app.ui.mask_peek_until.map(|t| t.saturating_duration_since(std::time::Instant::now())).filter(|d| !d.is_zero()) {
        ui.ctx().request_repaint_after(left);
    }
    // a drag drafts at full size where the GPU draws the view (by the GPU's own rule, `handles`;
    // `ready` first: `available` would wait for a device still being created)
    let gpu = draft_drag
        && lcc_engine::gpu::handles(
            lcc_pipeline::OutputDepth::U8,
            app.ui.soft_proof && lcc_pipeline::proof_profile(app.ui.proof.profile).is_some(),
            overlay,
        )
        && lcc_engine::gpu::ready()
        && lcc_engine::gpu::available();
    let shown = app.renderer.textures.get(&Slot::Main).filter(|t| t.photo == id && t.quick.is_none()).map(|t| (t.key, t.ms));
    let drag = app.session.interaction.as_ref().filter(|_| draft_drag).map(|i| (i.photo, i.label.as_str()));
    let draft = app.drag_scale.scale(drag, gpu, shown.map(|(key, _)| key), now).filter(|_| drafting);
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
        if drafting {
            app.drag_scale.observe(key, ms, app.perf.host_frame_ms);
        } else {
            // (a settled pause's full-quality render is not a draft of the drag)
            app.drag_scale.ignore(key);
        }
    }
    // a high quality drag times what shows its updates: the zoomed window over the whole frame,
    // or the whole frame; on an HDR display, the HDR texture drawn over the 8-bit one (its float
    // render can be the slower of the two)
    if adjust == crate::region::AdjustQuality::Full {
        let hdr = |app: &LccApp, view: u8| {
            let shows = hdr_shown(app) && d.hdr.enabled && overlay == lcc_pipeline::Overlay::None && !app.ui.soft_proof;
            let v = app.renderer.hdr_view.get(&view).filter(|v| shows && v.photo == id)?;
            Some((v.key, app.renderer.hdr_view_ms.get(&view).copied().unwrap_or(0.0)))
        };
        let timed = if plan.window_edge.is_some() {
            hdr(app, crate::render::HDR_ZOOMED.saturating_add(crate::render::PANE_AFTER))
                .or_else(|| app.renderer.window_texture(crate::render::PANE_AFTER).filter(|(_, g)| g.photo == id).map(|(t, _)| (t.key, t.ms)))
        } else {
            hdr(app, crate::render::HDR_LOUPE).or(shown)
        };
        if let Some((key, ms)) = timed {
            app.hq_drag.observe(key, ms, app.perf.host_frame_ms);
        }
    }
    // a pinch changes the zoom every frame: keep the sizes of before it until it is quiet
    let plan = app.size_hold.apply(id, now, navigating, plan);
    // the drawn size in pixels, clamped first: a NaN or negative float must not become a size
    let drawn_px = |points: f32| (points * ppp).round().clamp(0.0, 65_536.0) as usize;
    let fit_px = app.size_hold.display_fit(now, navigating, [drawn_px(target_rect.width()), drawn_px(target_rect.height())]);
    if app.size_hold.holding(now) {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(crate::region::HOLD_SECS));
    }
    // At rest the whole frame renders at no less than the preview source level: the source is then
    // copied 1:1 (no resample in the pipeline, whose prefilter and bilinear pass soften detail near
    // the output's limit) and shrunk once, by the Lanczos display fit below.
    // (zoomed in under a window render, the whole frame is hidden: it stays canvas-sized)
    let want =
        if drafting || plan.window_edge.is_some() { plan.main_edge } else { rest_edge(&app.ui.settings, plan.main_edge, native_long, texture_side) };
    let (rw, rh) = render_box(want, aspect);
    // hover and before renders are stand-ins: the preview size is plenty and keeps them cheap
    let (sw, sh) = render_box(app.ui.settings.stand_in_edge(want, texture_side), aspect);
    // an HDR display: an HDR edit's whole frame also as linear float with its highlights above
    // SDR white, including interactive drafts. Overlays and soft proof remain 8-bit views.
    let hdr_key = (overlay == lcc_pipeline::Overlay::None && !app.ui.soft_proof)
        .then(|| app.session.render_job(id, rw.max(8), rh.max(8), false, !crop_tool))
        .flatten()
        .and_then(|job| request_hdr(app, crate::render::HDR_LOUPE, job.with_hidden_grading(app.ui.grading_hidden_for(id)), true, 98));
    upload_hdr_views(app);
    // (a drag that shows its HDR frames needs no 8-bit drafts as well: the final render follows
    // the release)
    let hdr_drafts = interacting && hdr_texture(app, crate::render::HDR_LOUPE, id, hdr_key).is_some();
    // stepping fast (an arrow held down): the render waits until the photo stays, the camera JPEG
    // or thumbnail shows meanwhile, and a render queued for a photo already left is dropped
    let settling = !interacting && app.loupe_timing.settling(id, now);
    if settling {
        if app.renderer.textures.get(&Slot::Main).is_none_or(|t| t.photo != id) {
            app.renderer.drop_queued(Slot::Main);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(crate::loupe_timing::SETTLE_SECS / 2.0));
    }
    // the background preparation is making this very photo's preview: the loupe waits for it (it
    // lands in the view cache, which the render below then finds) rather than decoding the same raw a
    // second time beside it, at most PREP_WAIT_MAX_S (a build held up elsewhere doesn't keep the photo
    // unrendered)
    let wait_for_prep = !settling
        && !interacting
        && !drafting
        && plan.window_edge.is_none()
        && app.renderer.textures.get(&Slot::Main).is_none_or(|t| t.photo != id)
        && app.loupe_timing.on_screen_secs(id, now) < PREP_WAIT_MAX_S
        && app.session.being_prepared(id);
    if wait_for_prep {
        app.renderer.drop_queued(Slot::Main);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }
    let main_job = if settling || wait_for_prep {
        None
    } else if drafting {
        app.session.loupe_job(id, rw.max(8), rh.max(8), !crop_tool).map(|job| {
            let job = job.draft().with_overlay(overlay).with_proof(app.ui.soft_proof.then_some(app.ui.proof));
            job.with_hidden_grading(app.ui.grading_hidden_for(id))
        })
    } else {
        // (a high quality drag's updates leave their drawn-size image in the view cache: reading the
        // full render back as well costs each update more than the fit saved; at rest it is kept)
        rest_job(app, id, (rw, rh), fit_px, overlay, crop_tool).map(|job| if interacting { job.with_drawn_view_cache() } else { job })
    };
    if !hdr_drafts && let Some(job) = main_job {
        app.renderer.request(Slot::Main, job, 100);
        // a drag's draft that renders in a few ms (GPU) is drawn in this frame, not the next (zoomed
        // in, what changes on screen is the window's draft)
        // (a high quality update the GPU renders as fast is drawn at once too; a slow one returns
        // at once: `wait_for_draft` waits only when the last render fits in the budget). A full-
        // quality update (its fit done on the GPU, ~5 ms) gets more of the frame than a draft:
        // the frame around it is cheap, and missing the frame cost every such update one more.
        #[cfg(not(target_arch = "wasm32"))]
        if interacting && plan.window_edge.is_none() {
            let budget = wait_budget(ui.input(|i| i.predicted_dt), drafting);
            app.renderer.wait_for_draft(ui.ctx(), &mut app.session, budget);
        }
        // the renderer polled before this request: a refine asked for when nothing else repaints
        // (a still photo in an open interaction) gets the frame that collects it
        app.renderer.keep_polling(ui.ctx());
    }
    // hovering a preset or profile: the photo with that look, shown instead of the loupe render
    // once it is ready (nothing is committed)
    let hover_key = match app.hover_preview.clone() {
        Some(h) if !interacting => app.session.preview_job(id, sw.max(8), sh.max(8), !crop_tool, &h.settings).map(|job| {
            // (the same display fit as the After view, drawn texel for texel like it)
            let job = job.with_display_fit(fit_px[0], fit_px[1]);
            let key = job.key;
            app.renderer.request(Slot::Hover, job, 105);
            (key, h.label)
        }),
        _ => None,
    };
    // the most the loupe shows a whole photo at (the canvas's long side; a photo of another shape
    // than this one may fill it): the background preparation builds previews at least this big
    if matches!(app.ui.zoom, Zoom::Fit) && !interacting && sizes.canvas_long.is_finite() && sizes.canvas_long >= 1.0 {
        app.session.loupe_edge = sizes.canvas_long.ceil() as usize;
    }
    // which way the user steps (the photos ahead are those in the direction of travel)
    let ids = app.session.visible_cloned();
    let here = ids.iter().position(|p| *p == id);
    if let Some(i) = here {
        if let Some((was, at)) = app.prep_at
            && was != id
        {
            app.prep_backward = i < at;
        }
        app.prep_at = Some((id, i));
    }
    // once this photo is on screen: prepare the photos around it, as many each way as Settings ▸
    // Performance ▸ Prepare photos in the background says (nearest first, the direction of travel
    // first at each distance): the nearest each way are rendered as the loupe will ask for them and
    // adopted at the step, those further rendered into the preview cache, which the loupe then
    // fits (no render). Editing: the nearest are decoded and kept, ready for their edits.
    // (nor while a cell on screen waits for its first picture: those come before anything bigger;
    // nor while the user's own work runs, an export above all)
    // (nor while the photographer paints: only the photo on screen renders, OneSlips NOTES-1028 note 29)
    if !interacting
        && !app.paint_hold
        && !app.renderer.is_pending(Slot::Main)
        && app.renderer.textures.get(&Slot::Main).is_some_and(|t| t.photo == id)
        && !app.pictures_busy(app.renderer.frame())
        && app.session.activity.user_work().is_none()
    {
        let mode = prep_mode(app);
        let prep = app.session.prep_settings();
        let ahead = prep.ahead(mode);
        let develop = mode == lcc_engine::prep::Mode::Develop;
        if let Some(i) = here.filter(|_| ahead > 0) {
            let mut targets = nearest_each_way(&ids, i, ahead, app.prep_backward);
            if prep.scope(mode) == lcc_engine::prep::Scope::Marked {
                targets.retain(|t| lcc_engine::prep::marked(&app.session, *t));
            }
            // a few at a time, nearest first (the pool keeps workers free for the photo the user goes to)
            let mut room = PREFETCH_AT_ONCE.saturating_sub(app.renderer.prefetches_pending());
            let mut nearest = [false; 2];
            for nid in targets {
                if room == 0 {
                    break;
                }
                let Some(at) = ids.iter().position(|p| *p == nid) else { continue };
                // (one the background preparation is making already is not decoded a second time)
                if !develop && app.session.being_prepared(nid) {
                    continue;
                }
                // the nearest one each way in slots 0 (ahead) and 1 (behind), kept to be adopted at
                // the step; the others by their place in the view, so a step keeps them where they are
                let side = usize::from((at < i) != app.prep_backward);
                let slot = if nearest.get(side).is_some_and(|taken| !taken) {
                    if let Some(taken) = nearest.get_mut(side) {
                        *taken = true;
                    }
                    Slot::Prefetch(side as u8)
                } else {
                    far_prefetch_slot(at)
                };
                let Some(np) = app.session.catalog.photo(nid).cloned() else { continue };
                let nf = Frame::with_lens(np.width.max(1) as usize, np.height.max(1) as usize, &np.develop, !crop_tool, np.embedded_lens.as_deref());
                let na = nf.aspect() as f32;
                let nnative = output_px(&nf);
                let nlong = nnative[0].max(nnative[1]);
                let nr = fit_rect(main_area, na, app.ui.zoom, nnative, ppp, app.ui.pan);
                let nplan = crate::region::plan(
                    &app.ui.settings,
                    crate::region::ViewSizes { drawn_long: nr.width().max(nr.height()) * ppp, native_long: nlong, draft: None, ..sizes },
                );
                // at the whole-frame view: the very render the loupe asks for once that photo is
                // shown, which it adopts at the step (`Renderer::prefetch`); zoomed in, its source
                let job = if nplan.window_edge.is_none() {
                    let fit = [drawn_px(nr.width()), drawn_px(nr.height())];
                    let size = render_box(rest_edge(&app.ui.settings, nplan.main_edge, nlong, texture_side), na);
                    rest_job(app, nid, size, fit, lcc_pipeline::Overlay::None, crop_tool)
                } else {
                    let (w, h) = render_box(app.ui.settings.prefetch_edge(nr.width().max(nr.height()) * ppp, nlong, texture_side), na);
                    app.session.loupe_job(nid, w.max(8), h.max(8), !crop_tool)
                };
                if let Some(mut job) = job {
                    // (editing: the nearest are decoded and kept, not taken from a cached render)
                    if develop && at.abs_diff(i) <= DECODED_AHEAD {
                        job.view_cache = None;
                    }
                    // (asked for already, in this slot: queued, running or made)
                    if app.renderer.prefetch_asked(slot, job.key) {
                        continue;
                    }
                    room -= 1;
                    app.renderer.prefetch(slot, job, PREFETCH_PRIORITY);
                }
            }
        }
    }
    // the photo on screen and its neighbours' renders hold back background work (the background
    // preparation's preview build, eye finding) while they are being made
    if app.renderer.is_pending(Slot::Main) || app.renderer.prefetches_pending() > 0 {
        lcc_engine::prep::foreground();
    }
    // After a short look at the whole photo, its full-resolution source, decoded in the background
    // below the neighbours when nothing else is waiting, so a zoom to 1:1 (to check focus) starts
    // from it instead of decoding first (0.2-2.7 s). The engine keeps one such source.
    let whole = !interacting && plan.window_edge.is_none() && app.renderer.textures.get(&Slot::Main).is_some_and(|t| t.photo == id);
    if !whole {
        app.whole_since = None;
    } else {
        let since = match app.whole_since {
            Some((p, t)) if p == id => t,
            _ => {
                app.whole_since = Some((id, now));
                now
            }
        };
        if now - since >= WARM_FULL_AFTER_S
            && native_long > lcc_engine::SourceLevel::Preview.max_edge()
            && !app.session.media.has_source(id, lcc_engine::SourceLevel::Full)
            && app.renderer.queued() == 0
            && let Some(job) =
                app.session.region_job(id, native[0], native[1], lcc_engine::pipeline::PixelWindow { x: 0, y: 0, w: 1, h: 1 }, !crop_tool)
        {
            app.renderer.prefetch(Slot::Prefetch(crate::render::FULL_SOURCE_PREFETCH), job, PREFETCH_PRIORITY - 1);
        } else if now - since < WARM_FULL_AFTER_S {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((WARM_FULL_AFTER_S - (now - since)).max(0.05)));
        }
    }
    // until the loupe has this photo: show its cached render / embedded preview / a thumbnail
    if app.renderer.textures.get(&Slot::Main).is_none_or(|t| t.photo != id)
        && let Some(q) = app.session.quick_view_job(id, want.max(8), !crop_tool)
    {
        let q = cheapest_first(app, id, q, want.max(8), !crop_tool, settling);
        app.renderer.request_quick(Slot::Preview, q, 110);
    }
    if !interacting && let Some(i) = here {
        look_ahead(app, &ids, i, want.max(8), !crop_tool);
    }
    let show_before = app.ui.before_after == BeforeAfter::Original || ui.input(|i| i.key_down(egui::Key::Backslash));
    let mut hdr_before_key = None;
    if (split || show_before || split_view)
        && let Some(job) = app.session.render_job(id, sw.max(8), sh.max(8), true, !crop_tool)
    {
        // (the same display fit as the After view, so both panes are the same texture size)
        let job = if drafting { job } else { job.with_display_fit(fit_px[0], fit_px[1]) };
        // (the Before is the photo without its edits: HDR only when those settings are HDR. A drag
        // doesn't change it: its HDR frame from before the drag stays, not rendered again at the
        // drag's size and once more at the release)
        let held = app.renderer.hdr_view.get(&crate::render::HDR_BEFORE).filter(|h| interacting && hdr_shown(app) && h.photo == id).map(|h| h.key);
        hdr_before_key = held.or_else(|| request_hdr(app, crate::render::HDR_BEFORE, job.clone(), false, 88));
        app.renderer.request(Slot::Before, job, 90);
    }
    upload_hdr_views(app);
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
        // a side no longer shown zoomed frees the tiles of other photos; the photo on screen keeps
        // its own (the cache's budget bounds them), so a return to 100 % with the same look draws
        // at once instead of rendering every tile again (about 5 s on a 45 MP raw)
        if shown.is_none() {
            let pane = if before { crate::render::PANE_BEFORE } else { crate::render::PANE_AFTER };
            app.renderer.tiles.retain(|k| k.pane != pane || k.photo == id);
            app.renderer.release(Slot::Window(pane));
        }
    }
    // a clip: the frame to show at the playhead (playing or paused)
    let video_tex = video_state.as_ref().filter(|(v, _)| *v == id).and_then(|(vid, vs)| {
        let side = (target_rect.width().max(target_rect.height()) * ppp) as u32;
        crate::video_view::update(app, ui.ctx(), *vid, vs, side).map(|(t, s)| (t, s, vs.playing))
    });
    // zoomed in on a raw as shot: its camera JPEG at full size under the window render, so a focus
    // check doesn't wait for the raw (Capture One's Cull view shows the embedded JPEG at 100 % too).
    // Released when the view is not zoomed any more (180 MB).
    let mut camera_full = !windows.is_empty() && hover_key.is_none() && !split && !show_before && !split_view && !interacting;
    if camera_full && let Some(q) = app.session.embedded_full_job(id, native_long.min(texture_side)) {
        app.renderer.request_quick(Slot::QuickFull, q, 98);
    } else {
        camera_full = false;
        if app.renderer.textures.contains_key(&Slot::QuickFull) || app.renderer.is_pending(Slot::QuickFull) {
            app.renderer.forget(Slot::QuickFull);
        }
    }
    let p = ui.painter_at(canvas);
    // what a view slot shows: its own render of this photo, else the stand-ins (no blank frame
    // between photos, and the full render replaces them in place)
    let hdr_view = hdr_texture(app, crate::render::HDR_LOUPE, id, hdr_key);
    let hdr_before = hdr_texture(app, crate::render::HDR_BEFORE, id, hdr_before_key);
    // an edited photo never shows the camera's JPEG (unedited, uncropped), not even one made while
    // the photo was still as shot
    let as_shot = lcc_engine::import::looks_as_shot(&photo);
    let truthful = |t: &&crate::render::Tex| as_shot || t.quick != Some(lcc_engine::media::QuickSource::Embedded);
    let drawn = std::cell::Cell::new(None);
    let draw = |slot: Slot, r: Rect, clip: Rect| -> &'static str {
        // (headless screenshots: the HDR texture is GPU-only, so they show the 8-bit render)
        let hdr = match slot {
            Slot::Main => hdr_view,
            Slot::Before => hdr_before,
            _ => None,
        };
        if let Some(v) = hdr {
            let image_rect = render_texture_rect(r, v.size);
            p.with_clip_rect(clip).image(v.tex, image_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            return "hdr";
        }
        let mine = |s: Slot| app.renderer.textures.get(&s).filter(|t| t.photo == id);
        // (the Before view takes no stand-in that shows the edits: upstream #2248)
        let stand_in = stand_in_shows(app, id, slot);
        let candidates = [
            mine(slot).map(|t| (t, "render")),
            mine(Slot::Preview).filter(truthful).filter(|_| stand_in).map(|t| (t, t.quick.map(quick_name).unwrap_or("preview"))),
            app.renderer.thumb(id).filter(truthful).filter(|_| stand_in).map(|t| (t, "thumb")),
        ];
        // each picture's corners on the view, when it says where it lies (`Some(None)`: drawn as it
        // is; `None`: it can't be drawn for this view)
        let quad_of = |t: &crate::render::Tex, own: bool| -> Option<Option<[Pos2; 4]>> {
            let (Some(have), Some(want)) = (t.placement.filter(|_| placed(t) && crop_view.is_none()), view_place) else {
                return Some(None);
            };
            // a picture with another look (the as-shot preview of a photo with a preset since)
            // never stands in; the view's own render is always shown while the next one is made
            // (an edit in progress: a brush stroke, a curve)
            if !own && !have.look.close_to(&want.look) {
                return None;
            }
            // another warp (an Optics drag) can't be mapped: the view's own render is drawn as it is
            // until the new one lands, never a black loupe; a stand-in is left out
            let Some(uv) = lcc_engine::placement::stand_in_uv(&have, &want) else {
                return own.then_some(None);
            };
            if lcc_engine::placement::is_identity(&uv, 1e-3) {
                return Some(None);
            }
            placed_quad(r, &uv).map(Some)
        };
        // the first picture that fills the view (as it is, or cut to the crop); else the first that
        // fills part of it, where it lies
        let view = r.intersect(clip);
        let pick = candidates
            .iter()
            .flatten()
            .find_map(|(t, w)| quad_of(t, *w == "render").filter(|q| q.is_none_or(|q| quad_covers(&q, view))).map(|q| (*t, *w, q)))
            .or_else(|| candidates.iter().flatten().find_map(|(t, w)| quad_of(t, *w == "render").map(|q| (*t, *w, q))));
        let Some((tex, what, quad)) = pick else { return "none" };
        if let Some(quad) = quad {
            p.with_clip_rect(view).add(placed_mesh(tex.id(), &quad));
            drawn.set(Some(LoupeDrawn { quad, rect: view, placed: true, known: true }));
            return what;
        }
        match &crop_view {
            Some(v) if r == img_rect => {
                p.with_clip_rect(clip).add(crop_view_mesh(tex.id(), v));
            }
            _ => {
                let image_rect = if tex.quick == Some(lcc_engine::media::QuickSource::Embedded) {
                    fit_texture_rect(r, tex.size)
                } else {
                    render_texture_rect(r, tex.size)
                };
                p.with_clip_rect(clip).image(tex.id(), image_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                let corners = [image_rect.left_top(), image_rect.right_top(), image_rect.right_bottom(), image_rect.left_bottom()];
                let known = tex.placement.is_some_and(|h| placed(tex) && view_place.is_some_and(|w| h.look.close_to(&w.look)));
                drawn.set(Some(LoupeDrawn { quad: corners, rect: image_rect.intersect(clip), placed: false, known }));
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
    // the Before label's pill, painted once the window renders are down (they would cover it)
    let mut before_badge: Option<(Rect, std::sync::Arc<egui::Galley>)> = None;
    // below what else lives in the canvas' top-left corner: the filter pill, the info overlay
    let hide_filter = app.ui.settings.hide_filter_in_develop && app.ui.module == crate::module::ModuleId::Develop;
    let pill_shown = !fullscreen && !hide_filter && filter_pill_shown(app);
    let pill_inset = if pill_shown { FILTER_PILL_INSET + FILTER_PILL_HEIGHT } else { 0.0 };
    // the info overlay starts below the filter pill when both show (#266)
    let info_extent = info_overlay_extent(app, &p, &photo);
    let badge_top_inset = if pill_shown && info_extent > 0.0 {
        pill_inset + INFO_OVERLAY_BELOW_PILL - INFO_OVERLAY_TOP + info_extent
    } else {
        pill_inset.max(info_extent)
    };
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
        // on the photo itself, so it gets the hover label's dark pill to stay readable over light
        // pictures; painted after the window renders below, which would cover it zoomed in
        let g = p.layout_no_wrap(crate::i18n::tr("Before").to_string(), t.font(12.0), Color32::WHITE);
        before_badge = Some((label_badge_rect(img_rect, canvas, g.size(), badge_top_inset), g));
    } else if let Some((_key, label)) = &hover_key
        && let Some(tex) = app.renderer.textures.get(&Slot::Hover).filter(|t| t.photo == id)
    {
        let hover_rect = fit_texture_rect(img_rect, tex.size);
        p.image(tex.id(), hover_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let g = p.layout_no_wrap(label.clone(), t.font(12.0), Color32::WHITE);
        let bg = label_badge_rect(hover_rect, canvas, g.size(), badge_top_inset);
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
                None if app.ui.settings.loupe_messages => {
                    p.text(canvas.center(), Align2::CENTER_CENTER, crate::i18n::tr("Rendering…"), t.font(13.0), t.text_dim);
                }
                None => {}
            }
        }
    }
    app.loupe_shown = Some((id, shown));
    app.loupe_drawn = drawn.get();
    app.loupe_timing.observe(id, shown, now);
    // the camera's own JPEG is on screen, not the app's rendering of the raw: say so, so colour and
    // tone are not judged on it
    if shown == "embedded" || (shown == "thumb" && app.session.embedded_thumb_is_final(&photo)) {
        camera_preview_badge(ui, canvas, img_rect.intersect(canvas));
    }
    // no picture of this photo yet, its first one on its way: background work waits (the photo on
    // screen comes first, at its cheapest first)
    if shown == "none" && (app.renderer.is_pending(Slot::Preview) || app.renderer.is_pending(Slot::Main)) {
        app.pictures_missing(app.renderer.frame());
    }
    // the colour under the pointer, for the readout under the histogram (the After render only)
    if shown == "render"
        && !split
        && !show_before
        && crop_view.is_none()
        && let Some(q) = resp.hover_pos().filter(|q| canvas.contains(*q))
        && let Some(tex) = app.renderer.textures.get(&Slot::Main).filter(|t| t.photo == id)
        && let Some(px) = tex.pixels.as_deref()
    {
        let r = render_texture_rect(img_rect, tex.size);
        let (u, v) = ((q.x - r.left()) / r.width().max(1.0), (q.y - r.top()) / r.height().max(1.0));
        if let Some(rgb) = crate::color_readout::srgb_at(px, u, v, app.renderer.display().map(|d| d.as_ref())) {
            app.ui.hover_color = Some((ui.ctx().cumulative_pass_nr(), rgb));
        }
    }
    // a wipe shows the Before whole-frame render on its side of the line…
    let line = crate::region::wipe_line(img_rect, canvas);
    // (the Before's HDR render when it is an HDR edit, else its 8-bit one)
    let before_tex =
        hdr_before.map(|v| (v.tex, v.size)).or_else(|| app.renderer.textures.get(&Slot::Before).filter(|t| t.photo == id).map(|t| (t.id(), t.size)));
    if app.ui.before_after == BeforeAfter::Split
        && let Some((tex, size)) = before_tex
    {
        let before_rect = render_texture_rect(img_rect, size);
        let u = ((line.x - before_rect.left()) / before_rect.width().max(1.0)).clamp(0.0, 1.0);
        let left = Rect::from_min_max(before_rect.min, pos2(before_rect.left() + u * before_rect.width(), before_rect.bottom()));
        p.image(tex, left, Rect::from_min_max(pos2(0.0, 0.0), pos2(u, 1.0)), Color32::WHITE);
    }
    if app.ui.before_after == BeforeAfter::SplitTopBottom
        && let Some((tex, size)) = before_tex
    {
        let before_rect = render_texture_rect(img_rect, size);
        let v = ((line.y - before_rect.top()) / before_rect.height().max(1.0)).clamp(0.0, 1.0);
        let top = Rect::from_min_max(before_rect.min, pos2(before_rect.right(), before_rect.top() + v * before_rect.height()));
        p.image(tex, top, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, v)), Color32::WHITE);
    }
    // the camera JPEG at full size, magnified like the whole-frame render, until the window is there
    let mut camera_full_drawn = false;
    if camera_full
        && let Some(tex) = app.renderer.textures.get(&Slot::QuickFull).filter(|t| t.photo == id)
        && (tex.size[0].max(1) as f32 / tex.size[1].max(1) as f32 / display_aspect - 1.0).abs() <= 0.02
    {
        p.image(tex.id(), img_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        camera_full_drawn = true;
    }
    // …and the window renders go over the whole-frame ones, all of them, where they belong in the
    // frame (clipped to their pane, or their side of the line)
    let window_drawn = app.region_view.is_some_and(|v| v.photo == id && app.renderer.tiles.has_render_of_look(id, v.settings))
        || app.renderer.window_texture(crate::render::PANE_AFTER).is_some_and(|(_, g)| g.photo == id);
    if camera_full_drawn && !window_drawn {
        app.loupe_shown = Some((id, "embedded"));
        camera_preview_badge(ui, canvas, img_rect.intersect(canvas));
    }
    for (v, view) in &windows {
        // (holding `\` in a wipe shows the Before everywhere: it is the Before view alone then)
        let mode = if show_before { BeforeAfter::Original } else { app.ui.before_after };
        let clip = crate::region::window_clip(mode, v.before, v.visible, canvas, img_rect);
        draw_window(&p.with_clip_rect(clip), app, &window_ctx, v, view);
    }
    // each person's sharper eye at 100 %, found for this photo and the next ones (never waited for)
    if app.eye_focus_on() {
        // the photos ahead in the direction of travel, as many as Settings ▸ Performance says
        let ids = app.session.visible_cloned();
        let eye_ahead = app.session.prep_settings().eye_ahead(prep_mode(app));
        let mut upcoming: Vec<PhotoId> =
            ids.iter().position(|p| *p == id).map(|i| ahead_only(&ids, i, eye_ahead, app.prep_backward)).unwrap_or_default();
        if app.session.prep_settings().scope(prep_mode(app)) == lcc_engine::prep::Scope::Marked {
            upcoming.retain(|t| lcc_engine::prep::marked(&app.session, *t));
        }
        crate::eye_strip::show(app, ui, canvas, id, &upcoming);
    } else if wants_eyes(app, id) {
        // Space zooms to the eyes without the strip: they are found quietly for this photo
        crate::eye_strip::find_quietly(app, id);
    }
    if let Some((bg, g)) = before_badge {
        p.rect_filled(bg, 4.0, Color32::from_black_alpha(160));
        p.galley(bg.min + vec2(8.0, 4.0), g, Color32::WHITE);
        register(ui.ctx(), "canvas:beforeLabel", bg);
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
    let hide_filter = app.ui.settings.hide_filter_in_develop && app.ui.module == crate::module::ModuleId::Develop;
    let pill = if !fullscreen && !hide_filter { filter_pill(app, ui, canvas) } else { None };
    if !fullscreen {
        all_masks_pill(app, ui, canvas, img_rect);
    }
    info_overlay(app, &p, canvas, &photo, pill);
    if app.ui.shot_info {
        // zoomed in, it sits over the photo's bottom edge: not while a tool works on the pixels
        let busy = super::surface_osd::on_photo_tool(app, ui.ctx());
        // the Modern layout's floating dock covers the photo's bottom edge, and an open group's
        // card grows above it: the strip keeps clear of the dock, and hides while a card is open
        let modern = crate::modern::on(app);
        let clear = if modern { crate::modern_dock::bottom_clearance(app) } else { 0.0 };
        if !(modern && app.ui.modern.group.is_some()) {
            shot_info_strip(ui, canvas, img_rect, &shot_info(&photo.meta, native), busy, clear);
        }
    }
    if super::faces::face_boxes_shown(app) {
        // a name box left open on another photo is dropped
        if app.ui.name_edit.as_ref().is_some_and(|e| e.photo != id.0) {
            app.ui.name_edit = None;
        }
        let hints = super::faces::hints_for(app, id.0);
        let people = if app.ui.name_edit.is_some() { app.caches.person_names(&app.session.catalog) } else { Default::default() };
        // while the name box is open and recognition is not set up, the box offers the next step
        let setup = if app.ui.name_edit.is_some() { super::faces::setup(app, ui.ctx()) } else { super::faces::Setup::Running };
        match region_overlay(ui, &p, &map, &photo, d.orientation, hints.as_deref(), &people, &mut app.ui.name_edit, setup) {
            Some(RegionEdit::Focus(index)) => app.ui.face_focus = Some((id.0, index)),
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
            RightPanel::Remove => {
                remove_overlay(app, ui, &resp, &map, &d);
                // a spot being generated: a spinner over it until the result arrives
                crate::panels::genfill::canvas(app, ui, &ui.painter_at(app.canvas_rect.unwrap_or(map.rect)), &|q| map.screen(q), &d);
            }
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
                if app.ui.draw_faces && !browsing {
                    draw_face_overlay(app, ui, &p, &resp, &map, d.orientation, id);
                } else if browsing {
                    // no Develop picker works on a browsed photo: a click only zooms, as in Library
                    click_zoom_and_pan(app, &resp, img, pane);
                } else {
                    general_interaction(app, ui, &resp, &map, img, pane, native, aspect)
                }
            }
        }
    }
    // Space held over a tool: the view's hand takes the pointer from it (#1895)
    crate::space_hand::interact(app, ui, canvas, (main_area, img_rect), split.then(|| (areas[0], before_img)));
    // Compact (OneSlips 0c58de7ad): the overlay switched on to stay (O) overrides showing it only when useful, so the
    // photo says so; it steps aside while a slider or dial adjusts (the overlay does too)
    if right == RightPanel::Masking && crate::theme::compact(ui.ctx()) && app.ui.mask_overlay && !adjusting(app) && !fullscreen {
        overlay_on_hint(ui, canvas, app.ui.mask_overlay_color);
    }
    // drawn and hit-tested above the loupe and its tools: clicks on it pan
    let nav = navigator(app, ui, canvas, img_rect, id);
    // an AI mask on its way: what is being found, on the photo, with Cancel (no stand-in overlay)
    if right == RightPanel::Masking
        && let Some(m) = app.session.active_mask.and_then(|mid| d.masks.iter().find(|m| m.id == mid))
        && let Some((comp, what)) = ai_computing(app, m)
    {
        let over = img_rect.intersect(canvas);
        if ai_progress(ui, if over.is_positive() { over } else { canvas }, &what) {
            let r = if m.components.len() <= 1 {
                app.run("mask.delete", json!({"id": m.id}))
            } else {
                app.run("mask.component", json!({"id": m.id, "component": comp, "action": "delete"}))
            };
            if let Err(e) = r {
                app.toast(ui.ctx(), e);
            }
        }
    }
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
    let wait = rendering_wait(app);
    let rendering =
        ui.ctx().data_mut(|d| rendering_overdue_for(d.get_temp_mut_or(egui::Id::new("loupe-rendering"), (id, None)), id, keys, coming, now, wait));
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
            // (View Options' Show message when loading or rendering photos sets how long the wait is)
            None => {
                loupe_pill(ui, canvas, over, nav, "loupe:rendering", crate::i18n::tr("Rendering…"), false);
            }
        }
    }
    if let Some(why) = photo.preview_only.as_deref().filter(|_| !fullscreen) {
        // (the Modern layout's buttons float over the canvas' top: below them)
        let top = if crate::modern::on(app) { crate::modern::chrome_top(app) } else { 12.0 };
        preview_only_pill(ui, canvas, why, top);
    }
    crate::widgets::context_menu(&resp, |ui| {
        // Masking: a pin right-clicked opens its mask's menu; elsewhere the selected AI mask can be
        // updated from here too
        if app.ui.right == RightPanel::Masking && super::masking::photo_mask_menu(app, ui) {
            return;
        }
        if app.ui.right == RightPanel::Remove
            && let Some(active_idx) = app.session.active_spot
            && let Some(d) = app.session.active().and_then(|id| app.session.develop_of(id))
            && active_idx < d.spots.len()
        {
            spot_context_menu(app, ui, active_idx);
            ui.separator();
        }
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

/// The dark pill behind a label drawn over the photo (`Before`, a hovered preset's name): at the
/// photo's top left, inset 8 points, with the `text` size plus padding. Zoomed in, the photo
/// reaches past its `pane` (the canvas): the pill then sits at the pane's top left, so the label
/// stays in view (issue #2267: at 100 % it left the screen with the photo's corner), below the
/// `top_inset` the filter pill takes in that corner, and never past the pane's right edge.
fn label_badge_rect(photo: Rect, pane: Rect, text: egui::Vec2, top_inset: f32) -> Rect {
    let size = text + vec2(16.0, 8.0);
    let left = photo.left().max(pane.left()).min(pane.right() - size.x - 16.0).max(pane.left());
    let corner = pos2(left, photo.top().max(pane.top() + top_inset));
    Rect::from_min_size(corner + vec2(8.0, 8.0), size)
}

/// The info overlay at the canvas' top left (`view.infoOverlay`): the lines View Options chose for
/// Info 1 or Info 2 (`view.viewOptions`). If an active filter pill sits in the top-left corner, the
/// info overlay begins cleanly below it to avoid collision (issue #266).
/// "Overlay on · O" at the canvas' bottom left, a swatch of the overlay's colour before it (OneSlips 0c58de7ad): the
/// mask overlay was switched on to stay (widget `canvas:overlayOn`).
fn overlay_on_hint(ui: &egui::Ui, canvas: Rect, color: [u8; 3]) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let g = p.layout_no_wrap(crate::i18n::tr("Overlay on · O").to_string(), t.font(12.0), Color32::WHITE);
    let bg = Rect::from_min_size(pos2(canvas.left() + 10.0, canvas.bottom() - 18.0 - g.size().y), g.size() + vec2(30.0, 8.0));
    p.rect_filled(bg, t.radius(bg.height() / 2.0), Color32::from_black_alpha(150));
    let [r, gg, b] = color;
    p.circle_filled(pos2(bg.left() + 12.0, bg.center().y), 4.5, Color32::from_rgb(r, gg, b));
    p.galley(pos2(bg.left() + 22.0, bg.top() + 4.0), g, Color32::WHITE);
    register(ui.ctx(), "canvas:overlayOn", bg);
}

fn info_overlay(app: &LccApp, p: &egui::Painter, canvas: Rect, photo: &lcc_catalog::Photo, pill: Option<Rect>) {
    let Some(lines) = info_overlay_lines(app, photo) else { return };
    let t = Tokens::get(p.ctx());
    let mut y = pill.map_or(canvas.top() + info_overlay_top(app, &t), |r| r.bottom() + INFO_OVERLAY_BELOW_PILL);
    let start_y = pill.map_or(canvas.top(), |r| r.bottom() + 2.0);
    for (i, l) in lines.iter().enumerate() {
        // laid out uncoloured: `galley` only fills placeholder text with its colour, so a white
        // layout made the shadow a second white copy (#1690)
        let g = p.layout_no_wrap(l.clone(), info_overlay_font(&t, i), Color32::PLACEHOLDER);
        let at = pos2(canvas.left() + 14.0, y);
        // a soft shadow keeps the text readable on bright photos
        p.galley(at + vec2(1.0, 1.0), g.clone(), Color32::from_black_alpha(200));
        p.galley(at, g.clone(), Color32::WHITE);
        y += g.size().y + INFO_OVERLAY_GAP;
    }
    y = overlay_badges(p, &t, pos2(canvas.left() + 14.0, y), photo);
    register(p.ctx(), "canvas:infoOverlay", Rect::from_min_max(pos2(canvas.left(), start_y), pos2(canvas.left() + 320.0, y)));
}

/// Where the info overlay's text starts below the canvas' top (without window controls over it),
/// and the gap after each line.
const INFO_OVERLAY_TOP: f32 = 12.0;
const INFO_OVERLAY_GAP: f32 = 3.0;
/// Where the info overlay's text starts below the filter pill, when the pill shows (#266).
const INFO_OVERLAY_BELOW_PILL: f32 = 6.0;

/// The info overlay's font for line `i` (the file name is the first, larger line).
fn info_overlay_font(t: &Tokens, i: usize) -> egui::FontId {
    if i == 0 { t.semibold(15.0) } else { t.font(12.5) }
}

/// How far below the canvas' top the info overlay's text and badges reach (0 when it is off), so a
/// label drawn in that corner can sit below it.
fn info_overlay_extent(app: &LccApp, p: &egui::Painter, photo: &lcc_catalog::Photo) -> f32 {
    let Some(lines) = info_overlay_lines(app, photo) else { return 0.0 };
    let t = Tokens::get(p.ctx());
    let text = lines.iter().enumerate().fold(info_overlay_top(app, &t), |y, (i, l)| {
        y + p.layout_no_wrap(l.clone(), info_overlay_font(&t, i), Color32::PLACEHOLDER).size().y + INFO_OVERLAY_GAP
    });
    text + if has_overlay_badges(photo) { OVERLAY_BADGES_H } else { 0.0 }
}

/// Where the info overlay's text starts below the canvas' top: the full-screen preview has no top
/// bar, so on macOS it starts below the window controls that sit over the photo there (#2460).
fn info_overlay_top(app: &LccApp, t: &Tokens) -> f32 {
    let controls = app.ui.fullscreen && app.integrated_titlebar && !app.window_is_fullscreen;
    if controls { t.top_bar_h } else { INFO_OVERLAY_TOP }
}

/// The info overlay's lines for `photo`, or `None` when it is off.
fn info_overlay_lines(app: &LccApp, photo: &lcc_catalog::Photo) -> Option<Vec<String>> {
    use crate::state::InfoOverlay;
    // with the overlay off, Show Briefly When Photo Changes shows its Info for a moment
    let brief = app.ui.info_overlay == InfoOverlay::Off && app.ui.brief_info.shown;
    let mode = if brief { app.ui.settings.loupe_info.briefly } else { app.ui.info_overlay };
    let fields = match mode {
        InfoOverlay::Off => return None,
        InfoOverlay::Basic => &app.ui.settings.loupe_info.info1,
        InfoOverlay::Exposure => &app.ui.settings.loupe_info.info2,
    };
    let lines: Vec<String> = fields.iter().filter_map(|f| info_line(*f, photo)).collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines)
}

/// The height of the info overlay's rating and flag row.
const OVERLAY_BADGES_H: f32 = 17.0;

fn has_overlay_badges(photo: &lcc_catalog::Photo) -> bool {
    photo.rating > 0 || photo.flag != lcc_catalog::Flag::None
}

/// The star rating and flag under the info overlay's lines, when the photo has either; returns
/// the y below them.
fn overlay_badges(p: &egui::Painter, t: &Tokens, at: Pos2, photo: &lcc_catalog::Photo) -> f32 {
    use crate::icons::{Icon, paint};
    use lcc_catalog::Flag;
    let flag = match photo.flag {
        Flag::Pick => Some((Icon::FlagPickFilled, Color32::WHITE)),
        Flag::Reject => Some((Icon::FlagReject, t.reject)),
        Flag::None => None,
    };
    if !has_overlay_badges(photo) {
        return at.y;
    }
    let stars = photo.rating.min(5);
    let mut icons: Vec<(Icon, Color32)> = vec![(Icon::StarFilled, Color32::WHITE); stars as usize];
    icons.extend(flag);
    let side = 12.0;
    let mut x = at.x;
    for (i, (icon, color)) in icons.into_iter().enumerate() {
        // the flag stands a little apart from the stars
        if i > 0 && i == stars as usize {
            x += 5.0;
        }
        let r = Rect::from_min_size(pos2(x, at.y + 1.0), vec2(side, side));
        paint(p, r.translate(vec2(1.0, 1.0)), icon, Color32::from_black_alpha(200));
        paint(p, r, icon, color);
        x += side + 2.0;
    }
    register(p.ctx(), "canvas:infoOverlayBadges", Rect::from_min_max(at, pos2(x, at.y + side + 2.0)));
    at.y + OVERLAY_BADGES_H
}

/// Photo > People > Draw Face Regions: a drag over the photo draws a box; on release it becomes an unnamed face
/// region (`photo.addRegion`, one undo step) and its name box opens. Esc, or the menu item again, leaves the mode.
fn draw_face_overlay(
    app: &mut LccApp,
    ui: &egui::Ui,
    p: &egui::Painter,
    resp: &egui::Response,
    map: &CanvasMap,
    orient: lcc_geom::Orientation,
    id: lcc_catalog::PhotoId,
) {
    let key = egui::Id::new("face-draw");
    if app.ui.name_edit.is_none() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.draw_faces = false;
        ui.data_mut(|d| d.remove_temp::<(Pos2, Pos2)>(key));
        return;
    }
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if resp.drag_started_by(egui::PointerButton::Primary)
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        ui.data_mut(|d| d.insert_temp(key, (q, q)));
    }
    let Some((start, _)) = ui.data(|d| d.get_temp::<(Pos2, Pos2)>(key)) else { return };
    let now = resp.interact_pointer_pos().or(resp.hover_pos()).unwrap_or(start);
    let (a, b) = (map.norm(start), map.norm(now));
    let shown = lcc_geom::Rect {
        x0: a.x.min(b.x).clamp(0.0, 1.0),
        y0: a.y.min(b.y).clamp(0.0, 1.0),
        x1: a.x.max(b.x).clamp(0.0, 1.0),
        y1: a.y.max(b.y).clamp(0.0, 1.0),
    };
    if resp.dragged() {
        ui.data_mut(|d| d.insert_temp(key, (start, now)));
        let rect = Rect::from_two_pos(map.screen(Point::new(shown.x0, shown.y0)), map.screen(Point::new(shown.x1, shown.y1)));
        p.rect_stroke(rect.expand(1.0), 0.0, Stroke::new(1.0, Color32::from_black_alpha(190)), StrokeKind::Outside);
        p.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Outside);
        ui.ctx().request_repaint();
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove_temp::<(Pos2, Pos2)>(key));
        // a click or a slip of the mouse is not a face
        let moved = now - start;
        if moved.x.abs().max(moved.y.abs()) < 8.0 {
            return;
        }
        let upright = orient.inverse().map_norm_rect(shown);
        if let Ok(v) =
            app.run("photo.addRegion", json!({"id": id.0, "rect": {"x0": upright.x0, "y0": upright.y0, "x1": upright.x1, "y1": upright.y1}}))
            && let Some(index) = v["index"].as_u64().and_then(|i| usize::try_from(i).ok())
        {
            app.ui.name_edit = Some(crate::state::NameEdit { photo: id.0, index, text: String::new(), fresh: true });
        }
    }
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
    /// The pointer rests on this box: the loupe's chosen face.
    Focus(usize),
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
    let mut focus = None;
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
        if hovered && is_face && live.is_none() {
            focus = Some(index);
        }
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
    edit.or(focus.map(RegionEdit::Focus))
}

/// The filter pill's inset from the canvas' top-left corner and its height (the same inset from
/// the left as from the top, so the pill sits in the corner, #1645).
const FILTER_PILL_INSET: f32 = 3.0;
const FILTER_PILL_HEIGHT: f32 = 20.0;

/// Whether [`filter_pill`] has something to show (a filter is active).
fn filter_pill_shown(app: &LccApp) -> bool {
    !lcc_engine::filter_chips(&app.session.filter, &app.session.catalog).is_empty()
}

/// A small pill at the loupe's top left naming the active filters: the filmstrip and Next / Previous
/// follow them, so a filter must never be invisible here. A click clears them all. It sits in the
/// canvas margin, so the photo does not move.
fn filter_pill(app: &mut LccApp, ui: &mut egui::Ui, canvas: Rect) -> Option<Rect> {
    let chips = lcc_engine::filter_chips(&app.session.filter, &app.session.catalog);
    if chips.is_empty() {
        return None;
    }
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let label =
        chips.iter().map(|c| crate::panels::chips::display_label(c, &app.session.filter, &app.session.catalog)).collect::<Vec<_>>().join("  ·  ");
    let g = p.layout_no_wrap(crate::i18n::tr_format!("Filtered: {label}", label = label), t.font(12.0), Color32::from_gray(225));
    let inset = FILTER_PILL_INSET;
    let w = (g.size().x + 40.0).min((canvas.width() - 2.0 * inset).max(60.0));
    let r = Rect::from_min_size(canvas.min + vec2(inset, inset), vec2(w, FILTER_PILL_HEIGHT));
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
    Some(r)
}

/// While All Mask Overlays is on (Ctrl+Shift+O), a pill at the top of the photo says so and how
/// to leave it; a click turns it off (widget `loupe:allMasks`). Styled as the filter pill.
fn all_masks_pill(app: &mut LccApp, ui: &mut egui::Ui, canvas: Rect, over: Rect) {
    if !app.ui.mask_overlay_all || app.ui.right != RightPanel::Masking {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let text = crate::i18n::tr("All masks shown · O to hide");
    let g = p.layout_no_wrap(text.to_string(), t.font(12.0), Color32::from_gray(225));
    let size = vec2(g.size().x + 20.0, FILTER_PILL_HEIGHT);
    let over = if over.is_positive() { over.intersect(canvas) } else { canvas };
    let r = Rect::from_min_size(pos2(over.center().x - size.x / 2.0, over.top() + FILTER_PILL_INSET), size);
    let resp = ui.interact(r, egui::Id::new("loupe-all-masks-pill"), Sense::click());
    crate::widgets::announce(&resp, text);
    register(ui.ctx(), "loupe:allMasks", r);
    p.rect_filled(r, 10.0, Color32::from_black_alpha(if resp.hovered() { 225 } else { 185 }));
    p.galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, Color32::from_gray(225));
    if resp.on_hover_text(crate::i18n::tr("Click to hide the overlay")).clicked() {
        let _ = app.run("view.maskOverlay", json!({"show": false}));
    }
}

/// A raw shown from its embedded JPEG (issue #10): a pill at the canvas' top centre saying so;
/// hovering it explains (registered as `notice:previewOnly:loupe`).
fn preview_only_pill(ui: &mut egui::Ui, canvas: Rect, reason: &str, top: f32) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let text = crate::i18n::tr_format!("Preview only: editing the camera's embedded JPEG ({})", crate::widgets::preview_only_variant(reason));
    let g = p.layout_no_wrap(text, t.font(12.0), Color32::WHITE);
    let size = vec2(g.size().x + 40.0, 26.0);
    let r = Rect::from_min_size(pos2(canvas.center().x - size.x / 2.0, canvas.top().max(ui.ctx().content_rect().top()) + top), size);
    p.rect_filled(r, 13.0, Color32::from_black_alpha(185));
    crate::icons::paint(&p, Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(15.0, 15.0)), crate::icons::Icon::Info, t.caution);
    p.galley(pos2(r.left() + 29.0, r.center().y - g.size().y / 2.0), g, Color32::WHITE);
    register(ui.ctx(), "notice:previewOnly:loupe", r);
    ui.interact(r, egui::Id::new("preview-only-pill"), Sense::hover()).on_hover_text(crate::widgets::preview_only_explanation(reason));
}

/// One line of the info overlay for `photo`; `None` when the field is None or the photo has
/// nothing to show for it.
fn info_line(field: crate::state::LoupeInfoField, photo: &lcc_catalog::Photo) -> Option<String> {
    use crate::state::LoupeInfoField as F;
    let m = &photo.meta;
    let text = match field {
        F::None => return None,
        F::FileName => photo.file_name.clone(),
        F::CaptureDate => pretty_date(photo.captured.as_deref().unwrap_or(&photo.imported)),
        F::Dimensions => format!("{} × {}", photo.width, photo.height),
        F::Exposure => {
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
            if exp.is_empty() { "No exposure information".into() } else { exp.join("  ") }
        }
        F::Camera => m.camera.clone(),
        F::Lens => m.lens.clone(),
        F::CameraAndLens => [m.camera.as_str(), m.lens.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · "),
    };
    (!text.is_empty()).then_some(text)
}

/// The capture date the info overlay shows, in the same format as the metadata panel and the grid
/// (`i18n::display_time`: "March 30, 2022 at 10:11:11 PM" in English; a year or a month alone, such
/// as "1983" / "April 1983", shows at its own precision) so every capture date reads alike (#743).
fn pretty_date(iso: &str) -> String {
    crate::i18n::display_time(iso)
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
        Some(t) => p.image(t.id(), frame, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE),
        None => p.rect_filled(frame, 0.0, Tokens::get(ui.ctx()).skeleton),
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

/// How many photos ahead (in the direction of travel) the loupe makes its stand-in for, and how
/// many behind (a step back is common when culling).
const AHEAD: usize = 5;
/// The thumbnails made ahead while a step key is held ([`crate::loupe_timing::LoupeTiming::holding`]):
/// about a second of a held key.
pub(crate) const AHEAD_WHILE_HOLDING: usize = 30;
const BEHIND: usize = 1;
/// The look-ahead's priority: below the loupe's own render (100), above everything else.
const AHEAD_PRIORITY: u32 = 96;
/// Thumbnails the loupe keeps while stepping (as many as the grid keeps), and how many photos on
/// either side of the open one stay when it trims them.
const MAX_LOUPE_THUMBS: usize = 600;
const KEEP_AROUND: usize = 64;
/// The thumbnail size the look-ahead asks for (the filmstrip's at 2x).
const AHEAD_THUMB_EDGE: usize = 256;

/// `ahead` photos in the direction of travel from position `i` of `ids`, nearest first, then
/// `behind` the other way.
pub(crate) fn ahead_and_behind(ids: &[PhotoId], i: usize, backward: bool, ahead: usize, behind: usize) -> Vec<PhotoId> {
    let dir: isize = if backward { -1 } else { 1 };
    let at = |k: isize| i.checked_add_signed(k).and_then(|j| ids.get(j)).copied();
    let cap = |n: usize| n.min(ids.len()) as isize;
    let ahead = (1..=cap(ahead)).filter_map(|k| at(dir * k));
    let behind = (1..=cap(behind)).filter_map(|k| at(-dir * k));
    ahead.chain(behind).collect()
}

/// The long edge of the loupe's first stand-in of a photo shown from its camera JPEG: the size of the
/// small JPEG most raws keep beside the full-size one (Canon CR3 1620 px, Sony ARW 1616 px), which
/// decodes in a few ms. The full-size JPEG of a 45 MP raw takes 0.3-1.2 s, and stepping at 0.4 s
/// showed nothing at all for half of the photos while it decoded (2026-11-10 culling sweep).
pub(crate) const FIRST_PICTURE_EDGE: usize = 1600;

/// Told apart from the photo's full-size stand-in (whose key doesn't depend on the size).
const FIRST_PICTURE_SALT: u64 = 0x5f1e_57ad_c0de_0001;

/// The photo's quick stand-in at [`FIRST_PICTURE_EDGE`] (`None`: none of that kind), keyed apart.
fn first_picture_job(app: &mut LccApp, id: PhotoId, crop: bool) -> Option<lcc_engine::media::QuickJob> {
    let mut q = app.session.quick_view_job(id, FIRST_PICTURE_EDGE, crop)?;
    q.embedded.as_ref()?;
    q.key ^= FIRST_PICTURE_SALT;
    Some(q)
}

/// The cheapest picture first, then the next (the user's rule, PERF-CULL-TIERS): while the loupe
/// shows nothing of this photo as big as [`FIRST_PICTURE_EDGE`], its camera JPEG at that size; once
/// that is up and the photo stays (not `settling`), `full` (the camera JPEG at the loupe's size, or a
/// cached render). A photo whose stand-in at full size was made ahead takes that one at once.
fn cheapest_first(
    app: &mut LccApp,
    id: PhotoId,
    full: lcc_engine::media::QuickJob,
    want: usize,
    crop: bool,
    settling: bool,
) -> lcc_engine::media::QuickJob {
    if want <= FIRST_PICTURE_EDGE || full.embedded.is_none() || app.renderer.made_ahead(id, full.key) {
        return full;
    }
    let shown = app.renderer.textures.get(&Slot::Preview).filter(|t| t.photo == id).map_or(0, |t| t.size[0].max(t.size[1]));
    if shown >= FIRST_PICTURE_EDGE * 9 / 10 && !settling {
        return full;
    }
    first_picture_job(app, id, crop).unwrap_or(full)
}

/// Culling: make the loupe's stand-ins (the camera JPEG of a raw as shot, or a cached render) of
/// the next photos in the direction of travel, and their thumbnails, so that a step shows the next
/// one at once instead of after reading and decoding it (see [`Slot::QuickAhead`]). Worked out
/// again only when the photo, the direction, the catalog or the size changes.
fn look_ahead(app: &mut LccApp, ids: &[PhotoId], i: usize, edge: usize, crop: bool) {
    let Some(&id) = ids.get(i) else { return };
    let want = (id, app.prep_backward, app.session.catalog.revision, edge);
    if app.look_ahead_made_for == Some(want) {
        return;
    }
    app.look_ahead_made_for = Some(want);
    let targets = ahead_and_behind(ids, i, app.prep_backward, AHEAD, BEHIND);
    // stepping through a whole wedding in the loupe would keep every thumbnail it ever drew: keep
    // the ones around the photo (what the filmstrip can show) once there are many
    if app.renderer.thumb_textures() > MAX_LOUPE_THUMBS {
        let keep = ids.iter().skip(i.saturating_sub(KEEP_AROUND)).take(2 * KEEP_AROUND + 1).copied().collect();
        app.renderer.evict_thumbs(&keep, MAX_LOUPE_THUMBS);
    }
    // their thumbnails first (a small embedded JPEG each, a few ms): something to show at once
    // even when a step outruns the full-size previews, and in full screen, where no filmstrip
    // asks for them. A held key steps about 30 photos a second and outran the five ahead within a
    // fifth of a second: a quarter of the steps showed nothing at all (2026-11-10 culling sweep);
    // while it is held, the thumbnails go further ahead.
    let thumbs_ahead = if app.loupe_timing.holding() { AHEAD_WHILE_HOLDING } else { AHEAD };
    for nid in ahead_and_behind(ids, i, app.prep_backward, thumbs_ahead, BEHIND) {
        super::grid::request_thumb(app, nid, AHEAD_THUMB_EDGE, AHEAD_PRIORITY + 2);
    }
    // (an edited photo's stand-in made ahead is its last render, if any: never a raw decode for a
    // thumbnail ahead, which would take the cores from the render the user waits for)
    // (a raw as shot: its camera JPEG at the first picture's size, which the step shows at once and
    // replaces with the full-size one once the photo stays; see `cheapest_first`)
    let jobs = targets
        .into_iter()
        .filter_map(|nid| {
            let q = app.session.quick_view_job(nid, edge, crop)?;
            if edge > FIRST_PICTURE_EDGE
                && q.embedded.is_some()
                && let Some(first) = first_picture_job(app, nid, crop)
            {
                return Some(first);
            }
            Some(q)
        })
        .map(|mut q| {
            if !q.latest.is_empty() {
                q.small = None;
            }
            q
        })
        .collect();
    app.renderer.look_ahead(jobs, AHEAD_PRIORITY);
}

/// The longest the loupe waits for the background preparation's preview of the photo on screen
/// (s) before rendering it itself. Short: the preparation runs at the lowest priority, and on a busy
/// machine (a build beside the app) its render crawled while the photo on screen waited for it
/// (4 s at first: 0 of 15 photos sharp at 1.5 s steps, 2026-11-10).
pub(crate) const PREP_WAIT_MAX_S: f64 = 1.0;

/// Neighbour prefetch: below on-screen thumbnails, above background thumbnail refreshes.
const PREFETCH_PRIORITY: u32 = 4;
/// Photos prepared ahead at once (each render spreads over the cores already): the others wait,
/// so the photo the user steps to next finds a worker free.
const PREFETCH_AT_ONCE: usize = 2;
/// Editing: the neighbours this near each way are decoded and kept, ready for their edits; those
/// further are rendered with their settings and cached.
const DECODED_AHEAD: usize = 2;
/// The slots of the photos prepared further than the nearest each way ([`Slot::Prefetch`] 3 and
/// up; 0 and 1 are the nearest, 2 the open photo's full-resolution source), by the photo's place
/// in the view: a step keeps the photos still ahead in the slots they have (nothing is asked for
/// twice), and a jump to another part of the shoot replaces what was waiting.
const FAR_PREFETCH_SLOTS: usize = 2 * lcc_engine::prep::MAX_AHEAD as usize + 2;

fn far_prefetch_slot(at: usize) -> Slot {
    Slot::Prefetch((FIRST_FAR_PREFETCH + at % FAR_PREFETCH_SLOTS) as u8)
}

/// The first [`Slot::Prefetch`] of the photos prepared further than the nearest each way.
pub(crate) const FIRST_FAR_PREFETCH: usize = 3;

/// `n` photos each way from position `i` of `ids`, nearest first, the direction of travel first
/// at each distance.
pub(crate) fn nearest_each_way(ids: &[PhotoId], i: usize, n: usize, backward: bool) -> Vec<PhotoId> {
    let dir: isize = if backward { -1 } else { 1 };
    let at = |k: isize| i.checked_add_signed(k).and_then(|j| ids.get(j)).copied();
    (1..=n.min(ids.len()) as isize).flat_map(|k| [at(dir * k), at(-dir * k)]).flatten().collect()
}

/// `n` photos from position `i` of `ids` in the direction of travel, nearest first.
pub(crate) fn ahead_only(ids: &[PhotoId], i: usize, n: usize, backward: bool) -> Vec<PhotoId> {
    let dir: isize = if backward { -1 } else { 1 };
    (1..=n.min(ids.len()) as isize).filter_map(|k| i.checked_add_signed(dir * k).and_then(|j| ids.get(j)).copied()).collect()
}

/// What the user is doing, for the background preparation: editing (a tool panel open), else
/// culling a folder or browsing the library.
pub fn prep_mode(app: &LccApp) -> lcc_engine::prep::Mode {
    if app.ui.right.is_edit_tool() { lcc_engine::prep::Mode::Develop } else { lcc_engine::prep::Mode::of(&app.session) }
}

/// How long (s) the whole photo is shown before its full-resolution source is decoded in the
/// background (a photographer stepping through photos doesn't decode each).
pub(crate) const WARM_FULL_AFTER_S: f64 = 1.5;

/// How long a render may be on its way, with nothing new on screen, before the loupe says
/// "Rendering…" (s): as long as the activity stack waits before it shows a task.
pub(crate) const RENDERING_AFTER_SECS: f64 = 0.5;

/// The same wait while View Options' Show message when loading or rendering photos is off: the
/// person asked not to be told about ordinary renders, but a photo that is still the old picture
/// after this long looks like a frozen app, so the loupe says so anyway (s).
pub(crate) const SLOW_RENDERING_AFTER_SECS: f64 = 3.0;

/// What [`rendering_overdue`] keeps between frames while a render is coming: the texture keys on
/// screen, since when, and whether the pill is up.
type RenderingWait = Option<([Option<u64>; 4], f64, bool)>;

/// Whether the loupe says "Rendering…": a render has been `coming` for `after` seconds (the
/// constants above) with no new texture `keys` on screen. The wait starts when the render starts
/// coming (an idle app draws no frames, so the last one may be long ago), and once up the pill stays
/// until nothing is coming: drafts slower than the wait don't make it blink.
pub(crate) fn rendering_overdue(wait: &mut RenderingWait, keys: [Option<u64>; 4], coming: bool, now: f64, after: f64) -> bool {
    if !coming {
        *wait = None;
        return false;
    }
    let (seen, since, up) = wait.get_or_insert((keys, now, false));
    if !*up && *seen != keys {
        (*seen, *since) = (keys, now);
    }
    *up |= now - *since > after;
    *up
}

/// [`rendering_overdue`] for a pane that shows one photo at a time: the wait belongs to the photo
/// in `state`, so one moved in starts its own (the pill of the last photo doesn't carry over).
pub(crate) fn rendering_overdue_for(
    state: &mut (PhotoId, RenderingWait),
    photo: PhotoId,
    keys: [Option<u64>; 4],
    coming: bool,
    now: f64,
    after: f64,
) -> bool {
    if state.0 != photo {
        *state = (photo, None);
    }
    rendering_overdue(&mut state.1, keys, coming, now, after)
}

/// How long a render may be on its way before the loupe says "Rendering…": View Options' Show message when loading or
/// rendering photos on, half a second; off, only a render that stays slow says so.
fn rendering_wait(app: &LccApp) -> f64 {
    if app.ui.settings.loupe_messages { RENDERING_AFTER_SECS } else { SLOW_RENDERING_AFTER_SECS }
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

/// The selected mask `m`'s Object or Describe selection still being computed by the model: the
/// component and what is being found. Meanwhile no overlay shows for the mask (it would look like
/// the result).
pub(crate) fn ai_computing(app: &LccApp, m: &lcc_develop::Mask) -> Option<(usize, String)> {
    if !app.session.segmenter.busy() {
        return None;
    }
    m.components.iter().enumerate().find_map(|(k, c)| {
        let what = match &c.shape {
            MaskShape::Prompt { text, seg: None, .. } => crate::i18n::tr_format!("Finding “{text}”…", text = text.trim()),
            MaskShape::Object { seg: None, hint, .. } if !hint.is_empty() => crate::i18n::tr("Selecting the object…").to_string(),
            _ => return None,
        };
        Some((k, what))
    })
}

/// The AI mask's progress in the middle of the photo: a spinner, what is being found, and Cancel
/// (widgets `canvas:aiProgress`, `canvas:aiCancel`). True when Cancel was clicked.
fn ai_progress(ui: &mut egui::Ui, over: Rect, what: &str) -> bool {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(over);
    let g = p.layout_no_wrap(what.to_string(), t.font(13.0), Color32::WHITE);
    let cancel_w = 64.0;
    let size = vec2(34.0 + g.size().x + 12.0 + cancel_w + 10.0, 36.0);
    let bg = Rect::from_center_size(over.center(), size);
    p.rect_filled(bg, 8.0, Color32::from_black_alpha(185));
    // the spinner: an arc going round
    let c = pos2(bg.left() + 18.0, bg.center().y);
    let a0 = ui.input(|i| i.time) as f32 * 5.0;
    let arc: Vec<Pos2> = (0..=20).map(|k| c + egui::Vec2::angled(a0 + k as f32 / 20.0 * 4.4) * 7.0).collect();
    p.add(egui::Shape::line(arc, Stroke::new(2.0, Color32::WHITE)));
    p.galley(pos2(bg.left() + 34.0, bg.center().y - g.size().y / 2.0), g, Color32::WHITE);
    register(ui.ctx(), "canvas:aiProgress", bg);
    let br = Rect::from_min_size(pos2(bg.right() - cancel_w - 8.0, bg.top() + 6.0), vec2(cancel_w, bg.height() - 12.0));
    let b = ui.interact(br, egui::Id::new("canvas-ai-cancel"), Sense::click());
    crate::widgets::announce(&b, crate::i18n::tr("Cancel"));
    register(ui.ctx(), "canvas:aiCancel", br);
    p.rect_filled(br, 4.0, if b.hovered() { Color32::from_white_alpha(60) } else { Color32::from_white_alpha(30) });
    p.text(br.center(), Align2::CENTER_CENTER, crate::i18n::tr("Cancel"), t.font(12.5), Color32::WHITE);
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
    b.clicked()
}

fn quick_name(q: lcc_engine::media::QuickSource) -> &'static str {
    use lcc_engine::media::QuickSource;
    match q {
        QuickSource::Cached => "cached",
        QuickSource::Embedded => "embedded",
        QuickSource::Small => "small",
        QuickSource::Last => "last",
        QuickSource::Draft => "draft",
        QuickSource::Kept => "kept",
    }
}

/// How the loupe drew its picture ([`crate::LccApp::loupe_drawn`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoupeDrawn {
    /// Where the picture's top-left, top-right, bottom-right and bottom-left corners went.
    pub quad: [Pos2; 4],
    /// The part of the screen it was drawn in (clipped to).
    pub rect: Rect,
    /// Cut to the photo's current crop from a picture made with another ([`placed_quad`]).
    pub placed: bool,
    /// The picture said where it lies and has the photo's look ([`lcc_engine::placement`]).
    pub known: bool,
}

/// Where a picture's corners go on screen when the view `r` shows the part of it whose corners
/// (top-left, top-right, bottom-right, bottom-left of `r`) are `uv` on it
/// ([`lcc_engine::placement::stand_in_uv`]). `None` for a degenerate mapping.
fn placed_quad(r: Rect, uv: &[[f64; 2]; 4]) -> Option<[Pos2; 4]> {
    // uv = u0 + sx (u1 - u0) + sy (u3 - u0), with (sx, sy) across `r` from 0 to 1
    let [u0, u1, _, u3] = *uv;
    let (ax, ay) = (u1[0] - u0[0], u1[1] - u0[1]);
    let (bx, by) = (u3[0] - u0[0], u3[1] - u0[1]);
    let det = ax * by - bx * ay;
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let at = |c: [f64; 2]| {
        let (dx, dy) = (c[0] - u0[0], c[1] - u0[1]);
        let sx = (dx * by - bx * dy) / det;
        let sy = (ax * dy - dx * ay) / det;
        pos2(r.left() + (sx as f32) * r.width(), r.top() + (sy as f32) * r.height())
    };
    let q = [at([0.0, 0.0]), at([1.0, 0.0]), at([1.0, 1.0]), at([0.0, 1.0])];
    q.iter().all(|p| p.x.is_finite() && p.y.is_finite()).then_some(q)
}

/// Whether the convex quad `q` holds every corner of `r` (half a pixel of slack).
pub(crate) fn quad_covers(q: &[Pos2; 4], r: Rect) -> bool {
    let inside = |p: Pos2| {
        let mut sign = 0.0f32;
        for i in 0..4 {
            let (Some(&a), Some(&b)) = (q.get(i), q.get((i + 1) % 4)) else { return false };
            let edge = b - a;
            let len = edge.length().max(1e-6);
            let c = (edge.x * (p.y - a.y) - edge.y * (p.x - a.x)) / len;
            if c.abs() <= 0.5 {
                continue;
            }
            if sign == 0.0 {
                sign = c.signum();
            } else if c.signum() != sign {
                return false;
            }
        }
        true
    };
    r.is_positive() && [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()].into_iter().all(inside)
}

/// `tex` with its corners at `quad` (a picture cut to the view by the clip around it).
fn placed_mesh(tex: egui::TextureId, quad: &[Pos2; 4]) -> egui::epaint::Mesh {
    let mut mesh = egui::epaint::Mesh::with_texture(tex);
    for (at, uv) in quad.iter().zip([pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)]) {
        mesh.vertices.push(egui::epaint::Vertex { pos: *at, uv, color: Color32::WHITE });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    mesh
}

/// A small "Camera preview" label at the top right of the photo while the loupe shows the camera's
/// embedded JPEG (widget `loupe:cameraPreview`), with what that means on hover.
fn camera_preview_badge(ui: &egui::Ui, canvas: Rect, over: Rect) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let text = crate::i18n::tr("Camera preview");
    let g = p.layout_no_wrap(text.to_string(), t.font(11.5), Color32::from_gray(225));
    let size = vec2(g.size().x + 16.0, 18.0);
    let over = if over.is_positive() { over } else { canvas };
    let r = Rect::from_min_size(pos2(over.right() - size.x - 8.0, over.top() + 8.0), size);
    p.rect_filled(r, 9.0, Color32::from_black_alpha(165));
    p.galley(pos2(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, Color32::from_gray(225));
    register(ui.ctx(), "loupe:cameraPreview", r);
    let tip = crate::i18n::tr("The camera's own JPEG, shown until {app} has rendered the raw. Judge colour and tone on the rendered photo.");
    let resp = ui.interact(r, egui::Id::new("camera-preview-badge"), Sense::hover()).on_hover_text(tip);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{text}: {tip}")));
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

/// An HDR display render as linear float with the HDR range ([`Slot::HdrView`]), without
/// the 8-bit preview caches. The renderer supplies a per-view stage cache. Its key includes the
/// headroom used for encoding so a headroom change re-uploads.
pub(crate) fn hdr_view_job(mut job: lcc_engine::media::RenderJob, headroom: Option<f32>) -> lcc_engine::media::RenderJob {
    job.request.depth = lcc_pipeline::OutputDepth::F32Hdr;
    job.request.space = lcc_pipeline::OutputSpace::Srgb;
    job.cache = None;
    job.view_cache = None;
    job.stages = None;
    job.display = None;
    job.request.display = None;
    job.key ^= 0x4844_525f_5649_4557;
    // Include headroom in the key so a headroom change re-uploads the texture (P2-4).
    if let Some(h) = headroom {
        job.key ^= h.to_bits() as u64;
    }
    job
}

/// Whether HDR edits show in HDR now: an HDR display and View ▸ HDR Preview on.
pub(crate) fn hdr_shown(app: &LccApp) -> bool {
    app.hdr_headroom.is_some() && app.ui.hdr_preview
}

/// Whether photo `id`'s develop settings are being edited now (a slider drag): the views that
/// show them draft, the others (its Before, other photos) keep their renders.
pub(crate) fn editing(app: &LccApp, id: PhotoId) -> bool {
    app.session.interaction.as_ref().is_some_and(|i| i.photo == id)
}

/// Whether a slider drag's renders are drafts now: always with high quality adjustment previews
/// off; with them on, only while a drag that was too slow at full quality is moving
/// ([`crate::region::HqDrag`], decided by the loupe each frame). `false` without a drag.
pub(crate) fn adjust_drafts(app: &LccApp) -> bool {
    let Some(i) = app.session.interaction.as_ref() else { return false };
    !app.ui.settings.high_quality_adjustment_previews || app.hq_drag.quality(Some((i.photo, i.label.as_str()))).drafts()
}

/// Ask for view `view`'s HDR render of `job` ([`hdr_view_job`]) when HDR edits show in HDR and
/// `job` is one, as a draft while `job` shows the settings being edited (`edited`: the job has
/// the photo's own settings, not its Before). Returns the HDR job's key, which [`hdr_texture`]
/// matches.
pub(crate) fn request_hdr(app: &mut LccApp, view: u8, job: lcc_engine::media::RenderJob, edited: bool, priority: u32) -> Option<u64> {
    if !hdr_shown(app) || !job.settings.hdr.enabled {
        return None;
    }
    let headroom = app.services.hdr_output.as_mut().and_then(|o| o.headroom());
    app.renderer.set_hdr_headroom(headroom);
    // (an extended-linear surface takes sRGB: no monitor profile conversion)
    if let Some(o) = app.services.hdr_output.as_ref() {
        app.renderer.set_surface_extended_linear(o.extended());
    }
    let job = if edited && editing(app, job.photo) && adjust_drafts(app) { job.draft() } else { job };
    let mut job = hdr_view_job(job, headroom);
    // (on the GPU its texture's texels are made there, for the headroom the output uploads with)
    job.hdr_texels = app.services.hdr_output.as_ref().and_then(|o| o.texel_headroom());
    app.renderer.prepare_hdr_view(view, &job);
    let key = job.key;
    if app.renderer.hdr_view.get(&view).is_some_and(|v| v.key != key) && app.renderer.reuse_hdr(view, key) {
        return Some(key);
    }
    if app.renderer.hdr_view.get(&view).is_none_or(|v| v.key != key) || app.renderer.wanted(Slot::HdrView(view)).is_some_and(|pending| pending != key)
    {
        app.renderer.request(Slot::HdrView(view), job, priority);
    }
    Some(key)
}

/// View `view`'s HDR texture of photo `photo` for the render `key` asked for, or the frame
/// before it while that renders ([`crate::render::Renderer::hdr_frame`]; none in headless
/// screenshots: the texture lives on the GPU only, so they show the 8-bit render).
pub(crate) fn hdr_texture(app: &LccApp, view: u8, photo: PhotoId, key: Option<u64>) -> Option<crate::render::HdrView> {
    let key = key?;
    if app.headless_screenshotting {
        return None;
    }
    app.renderer.hdr_frame(view, photo, key)
}

/// Hand the finished [`Slot::HdrView`] renders to the window's HDR output, each as its view's texture.
pub(crate) fn upload_hdr_views(app: &mut LccApp) {
    if app.renderer.hdr_view_result.is_empty() {
        return;
    }
    let results = std::mem::take(&mut app.renderer.hdr_view_result);
    let Some(out) = app.services.hdr_output.as_mut() else { return };
    for (view, crate::render::HdrResult { photo, key, size, deep, window, texels }) in results {
        let slot = app.renderer.hdr_upload_slot(view, window);
        // (texels the worker or the GPU made, when they are for the headroom the output uploads
        // with; else the floats)
        let ready = texels.and_then(|(t, h)| out.upload_texels(slot, size[0], size[1], &t, h));
        let uploaded = match (ready, deep.as_ref().map(|d| &d.samples)) {
            (Some(r), _) => r,
            (None, Some(lcc_pipeline::DeepSamples::F32(rgb))) => out.upload(slot, size[0], size[1], rgb),
            // (the GPU's texels, for a headroom the output no longer uploads with: the view keeps
            // the frame it has until its next render)
            _ => {
                log::debug!("HDR view {view}: texels for another headroom and no floats; kept the frame before");
                continue;
            }
        };
        match uploaded {
            Ok(tex) => {
                app.renderer.adopt_hdr(view, crate::render::HdrView { photo, key, tex, size, window, slot });
            }
            Err(e) => log::warn!("HDR view texture: {e}"),
        }
    }
}

/// What a zoomed pane of an HDR edit shows on an HDR display ([`hdr_window`]).
pub(crate) enum HdrPane {
    /// Its HDR window, or while that updates the one before it (drawn at its own place).
    Frame(crate::render::HdrView),
    /// Its HDR window is on its way: meanwhile the whole frame's HDR render under it shows
    /// (softer, never an 8-bit frame of another brightness).
    Pending,
    /// Not in HDR (an 8-bit view, HDR Preview off, an SDR edit, a pinch, a failed render, no HDR
    /// whole frame under it yet): its 8-bit tiles.
    Sdr,
}

/// A zoomed view of an HDR edit on an HDR display: the part of the `full` frame on screen (the
/// window a drag renders, [`crate::region::window_for`]: the same at rest and in a drag, and no
/// bigger, since each update is a texture the UI thread copies to the GPU) rendered once more as
/// linear float, drawn in place of its 8-bit tiles so highlights go above SDR white zoomed in
/// too, including during slider drags. A window it showed before comes back at once
/// ([`Renderer::reuse_hdr`]).
///
/// [`Renderer::reuse_hdr`]: crate::render::Renderer::reuse_hdr
pub(crate) fn hdr_window(app: &mut LccApp, c: &WindowCtx, v: &WindowView, full: (usize, usize)) -> HdrPane {
    if !hdr_shown(app) || c.holding || app.headless_screenshotting {
        return HdrPane::Sdr;
    }
    if !v.before && (c.overlay != lcc_pipeline::Overlay::None || c.proof.is_some()) {
        return HdrPane::Sdr;
    }
    let Some(win) = crate::region::window_for(full.0, full.1, visible_px(full, v.target, v.visible), crate::region::max_span(c.texture_side)) else {
        return HdrPane::Sdr;
    };
    // (one texture: no bigger than the GPU takes)
    if win.w == 0 || win.h == 0 || win.w.max(win.h) > c.texture_side {
        return HdrPane::Sdr;
    }
    let job = if v.before {
        app.session.region_job_before(c.id, full.0, full.1, win, !c.crop_tool)
    } else {
        app.session.region_job(c.id, full.0, full.1, win, !c.crop_tool).map(|j| j.with_hidden_grading(app.ui.grading_hidden_for(c.id)))
    };
    let Some(job) = job else { return HdrPane::Sdr };
    let hdr_view = crate::render::HDR_ZOOMED.saturating_add(v.pane);
    // (above the tiles: it is what shows)
    let Some(key) = request_hdr(app, hdr_view, job, !v.before, TILE_PRIORITY.saturating_add(1)) else { return HdrPane::Sdr };
    upload_hdr_views(app);
    if let Some(frame) = hdr_texture(app, hdr_view, c.id, Some(key)) {
        return HdrPane::Frame(frame);
    }
    let slot = Slot::HdrView(hdr_view);
    // the whole frame's HDR render this pane is drawn over
    let under = match v.pane {
        crate::render::PANE_AFTER => crate::render::HDR_LOUPE,
        crate::render::PANE_BEFORE => crate::render::HDR_BEFORE,
        pane => crate::render::HDR_COMPARE.saturating_add(pane.saturating_sub(crate::render::PANE_COMPARE)),
    };
    let underlay = app.renderer.hdr_view.get(&under).is_some_and(|h| h.photo == c.id);
    if underlay && app.renderer.is_pending(slot) && app.renderer.failed_key(slot) != Some(key) { HdrPane::Pending } else { HdrPane::Sdr }
}

/// The diagnostic overlay the loupe shows (the selected mask, Point Color's visualized range,
/// Visualize Spots, Visualize HDR, and the clipping overlay: J's per-pixel red/blue tint, or the
/// black/white clipping mask while ⌥/Alt-dragging a global tone slider).
/// Whether a mask has settings: an effect on the photo to judge. A mask without any shows its
/// overlay by itself, so what it selects can be seen (upstream PR 2920's overlay rule).
pub(crate) fn mask_has_settings(m: &lcc_develop::Mask) -> bool {
    m.adjust != lcc_develop::LocalAdjustments::default() || !m.curve.is_identity()
}

/// Whether a mask is being shaped on the photo right now: a handle, the gradient or the pin
/// dragged, a brush stroke, a colour or tone area or a colour pin on its way. It counts as
/// adjusting: hovering shows no overlay meanwhile, and a mask with settings shows its effect.
pub(crate) fn mask_gesture(app: &LccApp) -> bool {
    matches!(app.gesture, Some(Gesture::MaskHandle { .. } | Gesture::Brush { .. } | Gesture::RangeArea { .. } | Gesture::ColorPin { .. }))
}

/// Whether a slider or a dial is adjusting the photo right now (OneSlips 0c58de7ad, the user's standard): then the
/// mask overlay, the pins and the tool graphics (mask lines and handles, Remove spots, crop guides, red-eye circles)
/// hide so the effect itself can be judged, even an overlay switched on with O; they come back as they were when the
/// adjusting stops. Sliders that shape the tool's own graphics (a range's Refine, a radial's Feather, a spot's Size,
/// the crop angle, a transform with its fine grid) keep them.
pub(crate) fn adjusting(app: &LccApp) -> bool {
    let slider = app.ui.dragging_control.as_deref().is_some_and(|c| !shapes_graphics(c));
    let mask_slider = app.ui.right == RightPanel::Masking && app.ui.mask_adjusting_until.is_some_and(|t| std::time::Instant::now() < t);
    slider || mask_slider || app.surface.adjusting()
}

/// Slider ids that change what the tool graphics show rather than the photo's look.
fn shapes_graphics(id: &str) -> bool {
    matches!(id, "refine" | "smoothness" | "edge" | "feather" | "maskFeather" | "ui.removeSize" | "ui.removeFeather")
        || id.starts_with("geometry.")
        || id.starts_with("crop.")
}

pub(crate) fn view_overlay(app: &LccApp, d: &DevelopSettings, alt_held: bool) -> lcc_pipeline::Overlay {
    use lcc_pipeline::{MaskView, Overlay};
    if app.ui.fullscreen {
        return Overlay::None;
    }
    // A mask gesture (a brush stroke, a handle drag, …) on a mask with settings is adjusting too:
    // nothing covers the photo, not even O or All, so the effect can be judged while painting. A
    // mask without settings keeps showing what it selects (there is no effect to see yet).
    let gesture = app.ui.right == RightPanel::Masking && mask_gesture(app);
    if gesture && app.session.active_mask.and_then(|id| d.masks.iter().find(|m| m.id == id)).is_some_and(mask_has_settings) {
        return Overlay::None;
    }
    // While a slider or a dial is at work no mask overlay or spot view covers the photo, in any panel: its effect is
    // what is being judged (the ⌥ threshold view of a tone slider still shows: it is what that drag asks for).
    let shown = !adjusting(app);
    // Show every visible mask's region at once (Ctrl+Shift+O / Cmd+Shift+O), ahead of the selected-mask overlay
    // so the shortcut is not shadowed by the always-on single-mask overlay.
    if shown && app.ui.right == RightPanel::Masking && app.ui.mask_overlay_all && d.masks.iter().any(|m| m.visible && !m.components.is_empty()) {
        return Overlay::AllMasks {
            view: MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default(),
            color: app.ui.mask_overlay_color,
            opacity: app.ui.mask_overlay_opacity.clamp(0.0, 100.0).round() as u8,
        };
    }
    // the pointer on a mask's row in the list shows what that mask selects
    if shown
        && app.ui.right == RightPanel::Masking
        && !gesture
        && let Some(m) = app.ui.mask_hover.or(app.ui.mask_pin_hover).and_then(|id| d.masks.iter().find(|m| m.id == id))
        && !m.components.is_empty()
    {
        return Overlay::Mask {
            id: m.id.min(u16::MAX as u32) as u16,
            view: MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default(),
            color: app.ui.mask_overlay_color,
            opacity: app.ui.mask_overlay_opacity.clamp(0.0, 100.0).round() as u8,
        };
    }
    // a picked mask with settings peeks its overlay for a moment (issue #1109), so the user sees
    // which mask is on; the Show Overlay checkbox stays as the auto rule left it (not latched on)
    let peeking = app.ui.mask_peek_until.is_some_and(|t| std::time::Instant::now() < t);
    if shown
        && app.ui.right == RightPanel::Masking
        && (app.ui.mask_overlay || peeking)
        && let Some(m) = app.session.active_mask.and_then(|id| d.masks.iter().find(|m| m.id == id))
        && !m.components.is_empty()
        && ai_computing(app, m).is_none()
    {
        return Overlay::Mask {
            id: m.id.min(u16::MAX as u32) as u16,
            view: MaskView::parse(&app.ui.mask_overlay_mode).unwrap_or_default(),
            color: app.ui.mask_overlay_color,
            opacity: app.ui.mask_overlay_opacity.clamp(0.0, 100.0).round() as u8,
        };
    }
    if shown && app.ui.right == RightPanel::Remove && app.ui.visualize_spots {
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
        // the colour pickers: an eyedropper, its tip on the pixel picked
        "wbPicker" | "fringe" | "colorRange" | "pointColor" | "hslPick" => {
            if let Some(q) = resp.hover_pos().or_else(|| ui.input(|i| i.pointer.latest_pos())) {
                crate::eyedropper::show(ui.ctx(), q);
            }
            return;
        }
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
            match app.run("develop.wbPick", json!({"x": n.x, "y": n.y, "size": app.ui.wb_sample})) {
                // a clipped, near-white or near-black spot: nothing changes, the selector stays
                // armed and says why (OneSlips, test notes 2026-10-25 note 13)
                Ok(r) if r["applied"] == false => {
                    let message = r["message"].as_str().unwrap_or("This spot can't be read").to_string();
                    app.toast(ui.ctx(), crate::i18n::tr(&message));
                }
                _ => app.ui.tool.clear(),
            }
        } else if let Some(q) = resp.hover_pos() {
            wb_loupe(app, ui, q, img, canvas);
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
    if app.ui.tool == "fringe" {
        // the Fringe Color Selector: a click points Defringe at the colour under it; a spot with no
        // colour says so and the selector stays armed
        if resp.clicked()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let n = map.norm(q);
            match app.run("develop.fringePick", json!({"x": n.x, "y": n.y})) {
                Ok(r) if r["applied"] == false => {
                    let message = r["message"].as_str().unwrap_or("This spot can't be read").to_string();
                    app.toast(ui.ctx(), crate::i18n::tr(&message));
                }
                _ => app.ui.tool.clear(),
            }
        }
        return;
    }
    if app.ui.tool == "hslPick" {
        // the Witchcraft HSL curves' picker: the hue under the pointer becomes a point on the
        // shown curve
        if resp.clicked()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let n = map.norm(q);
            let _ = app.run("develop.hslPick", json!({"channel": app.ui.hsl_curve, "x": n.x, "y": n.y}));
            app.ui.tool.clear();
        }
        return;
    }
    click_zoom_and_pan(app, resp, img, canvas);
    let _ = (native, aspect);
}

/// The White Balance Selector's sample sizes (side of the averaged square, preview pixels).
pub(crate) const WB_SAMPLES: [u32; 5] = [1, 3, 5, 9, 15];

/// The sample size nearest `n` among [`WB_SAMPLES`].
pub(crate) fn wb_sample_step(n: f64) -> u32 {
    WB_SAMPLES.iter().copied().min_by(|a, b| (*a as f64 - n).abs().total_cmp(&(*b as f64 - n).abs())).unwrap_or(5)
}

/// The White Balance Selector's loupe (#644): beside the pointer, the photo under it magnified to
/// single pixels, with the square that a click averages outlined; the same square on the photo.
fn wb_loupe(app: &LccApp, ui: &egui::Ui, q: Pos2, img: Rect, canvas: Rect) {
    if !img.contains(q) {
        return;
    }
    let Some(active) = app.session.active() else { return };
    let Some(tex) = app.renderer.textures.get(&Slot::Main).or_else(|| app.renderer.textures.get(&Slot::Preview)).filter(|t| t.photo == active) else {
        return;
    };
    let r = render_texture_rect(img, tex.size);
    let (tw, th) = (tex.size[0].max(1) as f32, tex.size[1].max(1) as f32);
    let u = ((q.x - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0);
    let v = ((q.y - r.top()) / r.height().max(1.0)).clamp(0.0, 1.0);
    let sample = app.ui.wb_sample.clamp(1, 31) as f32;
    // texels across the loupe: odd, so the pointer's texel sits in the middle
    let across = ((sample * 2.0 + 7.0).max(15.0) as u32 | 1) as f32;
    let side = 168.0f32;
    let cell = side / across;
    // centred on the texel under the pointer
    let (px, py) = ((u * tw).floor().min(tw - 1.0), (v * th).floor().min(th - 1.0));
    let half = (across - 1.0) / 2.0;
    let uv = Rect::from_min_max(pos2((px - half) / tw, (py - half) / th), pos2((px + half + 1.0) / tw, (py + half + 1.0) / th));
    // up and right of the pointer, flipped inside the pane near its edges
    let gap = 24.0;
    let mut x = q.x + gap;
    if x + side > canvas.right() {
        x = q.x - gap - side;
    }
    let mut y = q.y - gap - side;
    if y < canvas.top() {
        y = q.y + gap;
    }
    let at = Rect::from_min_size(pos2(x, y), vec2(side, side));
    let p = ui.painter().with_clip_rect(canvas);
    // the sampled square on the photo itself, where it is bigger than a few points
    let on_screen = vec2(r.width() / tw, r.height() / th) * sample;
    if on_screen.x >= 4.0 {
        let sq = Rect::from_center_size(pos2(r.left() + (px + 0.5) / tw * r.width(), r.top() + (py + 0.5) / th * r.height()), on_screen);
        p.rect_stroke(sq, 0.0, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Outside);
    }
    p.rect_filled(at.expand(2.0), 3.0, Color32::from_black_alpha(200));
    p.rect_filled(at, 0.0, Color32::from_gray(40));
    // texels outside the photo stay dark grey (the texture clamps at its edges)
    let inside = Rect::from_min_max(pos2(uv.left().max(0.0), uv.top().max(0.0)), pos2(uv.right().min(1.0), uv.bottom().min(1.0)));
    if inside.width() > 0.0 && inside.height() > 0.0 {
        let to = |w: Pos2| pos2(at.left() + (w.x - uv.left()) / uv.width() * side, at.top() + (w.y - uv.top()) / uv.height() * side);
        p.image(tex.id(), Rect::from_min_max(to(inside.min), to(inside.max)), inside, Color32::WHITE);
    }
    // the pixel grid, then the averaged square
    let grid = Stroke::new(0.5, Color32::from_black_alpha(70));
    if cell >= 6.0 {
        for i in 1..across as u32 {
            let o = i as f32 * cell;
            p.line_segment([pos2(at.left() + o, at.top()), pos2(at.left() + o, at.bottom())], grid);
            p.line_segment([pos2(at.left(), at.top() + o), pos2(at.right(), at.top() + o)], grid);
        }
    }
    let sq = Rect::from_center_size(at.center(), vec2(cell * sample, cell * sample));
    p.rect_stroke(sq, 0.0, Stroke::new(2.0, Color32::BLACK), egui::StrokeKind::Outside);
    p.rect_stroke(sq, 0.0, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Outside);
    register(ui.ctx(), "canvas:wbLoupe", at);
    // the colour under the pointer right under the loupe, as under the histogram, so it can be read
    // without looking away from the photo (#1094); above the loupe when there's no room below
    if let Some(rgb) = tex.pixels.as_deref().and_then(|px| crate::color_readout::srgb_at(px, u, v, app.renderer.display().map(|d| d.as_ref()))) {
        let text = app.ui.settings.color_readout.format(rgb);
        let h = 20.0;
        let y = if at.bottom() + 2.0 + h <= canvas.bottom() { at.bottom() + 2.0 } else { at.top() - 2.0 - h };
        let band = Rect::from_min_size(pos2(at.left() - 2.0, y), vec2(side + 4.0, h));
        p.rect_filled(band, 3.0, Color32::from_black_alpha(200));
        p.text(band.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(12.0), Color32::from_gray(230));
        register(ui.ctx(), "canvas:wbLoupeReadout", band);
    }
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
    // the crop as it will be: shrunk as the render shrinks it (lens corrections; perspective with
    // Constrain Crop), so no empty wedges show here that vanish on leaving the tool
    let shown = shown_crop(d, frame);
    let d = &DevelopSettings { crop: lcc_develop::Crop { geometry: shown, ..d.crop }, ..d.clone() };
    let quad = frame_crop_quad(d, frame);
    let pts: Vec<Pos2> = quad.iter().map(|q| map.screen(*q)).collect();
    // zoomed in, the box reaches past the canvas: it is drawn only on it, never over the
    // filmstrip or the panels (#1156)
    let p = &ui.painter_at(app.canvas_rect.unwrap_or(map.rect));
    // darken outside the crop (the box's corners in screen order: a flipped photo shows its
    // top-left corner elsewhere)
    let quad = flipped_corners(&pts, frame.flip_h, frame.flip_v);
    if pts.len() == 4 {
        let uv_to_norm = frame.out_to_norm(1, 1);
        let photo = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|(u, v)| map.screen(uv_to_norm.apply(Point::new(u, v))));
        p.add(outside_crop_mesh(&photo, &quad, Color32::from_black_alpha(crate::state::crop_mask_alpha(app.ui.crop_mask_opacity))));
    }
    // the box's centre: what a rotation turns the photo about (it stays put on screen)
    let box_centre = pos2(pts.iter().map(|q| q.x).sum::<f32>() / 4.0, pts.iter().map(|q| q.y).sum::<f32>() / 4.0);
    p.add(egui::Shape::closed_line(pts.clone(), Stroke::new(1.0, Color32::WHITE)));
    // overlay guides and handles (hidden while a slider or dial adjusts the photo)
    let mut p = p.clone();
    if adjusting(app) {
        p.set_invisible();
    }
    let p = &p;
    // overlay guides
    let lerp = |a: Pos2, b: Pos2, t: f32| a + (b - a) * t;
    let guide = Stroke::new(0.8, Color32::from_white_alpha(150));
    // (u, v) across / down the (rotated) crop frame
    let at = |(u, v): (f32, f32)| pts[0] + (pts[1] - pts[0]) * u + (pts[3] - pts[0]) * v;
    let (cw, ch) = (pts[0].distance(pts[1]), pts[0].distance(pts[3]));
    for line in super::crop_overlay::paths(app.ui.crop_overlay, app.ui.crop_overlay_orient, cw, ch) {
        p.add(egui::Shape::line(line.into_iter().map(at).collect(), guide));
    }
    // turning the photo: a fine grid in the box, to line things up with
    if matches!(app.gesture, Some(Gesture::CropRotate { .. })) {
        let fine = Stroke::new(0.6, Color32::from_white_alpha(70));
        for k in 1..12 {
            let u = k as f32 / 12.0;
            p.line_segment([at((u, 0.0)), at((u, 1.0))], fine);
            p.line_segment([at((0.0, u)), at((1.0, u))], fine);
        }
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
        let near = held.or_else(|| crop_zone(&pts, hq, inside(hq)).filter(|&z| z < 8)).filter(|_| !rotating);
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
        && crop_zone(&pts, q, inside(q)) == Some(8)
    {
        let _ = app.run("tool.done", json!({}));
        return;
    }
    // egui reports a drag only once the pointer has moved a few px: the zone is where the button
    // went down, where the cursor showed it, not where the pointer is by then. It is taken at the
    // press itself, from the box as drawn then: the box can still move under a held button on the
    // tool's first frames (the view settling, a render arriving), and a corner grabbed then must
    // stay grabbed
    let zone_key = egui::Id::new("crop_press_zone");
    if resp.is_pointer_button_down_on()
        && ui.input(|i| i.pointer.primary_pressed())
        && let Some(q) = ui.input(|i| i.pointer.press_origin())
    {
        let zone = crop_zone(&pts, q, inside(q));
        ui.data_mut(|m| m.insert_temp(zone_key, zone));
    }
    if resp.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos())
    {
        if let Err(e) = app.run("develop.beginInteraction", json!({"label": "Crop"})).and_then(|_| app.begin_crop().map(|_| serde_json::Value::Null))
        {
            app.ui.status = e;
            let _ = app.session.cancel_interaction();
            return;
        }
        // (from the crop as shown: a shrunk box starts where it is drawn)
        let g = shown_crop(&app.session.develop_of(id).unwrap_or_default(), frame);
        let (start, angle, map) = (g.rect, g.angle, map.clone());
        // screen points per pixel of the photo: the box's width on screen over its width in pixels
        let native_w = Frame { crop: g, ..frame.clone() }.native_size().0 as f32;
        let k = pts[0].distance(pts[1]) / native_w.max(1e-3);
        let pinned = Some(CropLayout::Pinned { centre: box_centre, k });
        let zone = ui.data_mut(|m| m.remove_temp::<Option<usize>>(zone_key)).unwrap_or_else(|| crop_zone(&pts, q, inside(q)));
        let (gesture, layout) = match zone {
            Some(8) => (Gesture::CropHandle { handle: 8, start, angle, map }, pinned),
            Some(h) => {
                (Gesture::CropHandle { handle: u8::try_from(h.min(8)).unwrap_or(8), start, angle, map: map.clone() }, Some(CropLayout::Frozen(map)))
            }
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
                // (the turn since the press, taken the short way round: the pointer crossing the box's
                // left side flips the angle between -180 and 180 degrees)
                let turn = (a - a0 + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                let ang = (start_angle + turn.to_degrees() as f64).clamp(-45.0, 45.0);
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
}

/// How far (screen points) from a corner of the crop box a press grabs that corner.
const CROP_CORNER_REACH: f32 = 14.0;
/// How far (screen points) either side of an edge of the crop box a press grabs that edge, along
/// its whole length.
const CROP_EDGE_BAND: f32 = 10.0;

/// What a press at `q` takes hold of in the crop box `pts` (its corners on screen, in
/// `crop.drag`'s order): `Some(0..3)` a corner (resizes both sides), `Some(4..7)` an edge (top,
/// right, bottom, left: anywhere along it resizes that side), `Some(8)` the box (moves the photo
/// under it), `None` outside (rotates). `inside`: whether `q` is inside the box. An edge away
/// from its middle handle used to rotate.
fn crop_zone(pts: &[Pos2], q: Pos2, inside: bool) -> Option<usize> {
    if pts.len() != 4 || !q.is_finite() {
        return inside.then_some(8);
    }
    if let Some((i, _)) =
        pts.iter().enumerate().map(|(i, c)| (i, c.distance(q))).filter(|(_, d)| *d < CROP_CORNER_REACH).min_by(|a, b| a.1.total_cmp(&b.1))
    {
        return Some(i);
    }
    let edge = (0..4)
        .filter_map(|i| {
            let (a, b) = (*pts.get(i)?, *pts.get((i + 1) % 4)?);
            let ab = b - a;
            let len2 = ab.length_sq();
            if len2.is_nan() || len2 <= 0.0 {
                return None;
            }
            let t = (q - a).dot(ab) / len2;
            (0.0..=1.0).contains(&t).then(|| (i + 4, (a + ab * t).distance(q)))
        })
        .filter(|(_, d)| *d < CROP_EDGE_BAND)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((i, _)) = edge {
        return Some(i);
    }
    inside.then_some(8)
}

#[cfg(test)]
mod crop_zone_tests {
    use super::*;

    #[test]
    fn edges_grab_along_their_whole_length_and_hostile_points_never_panic() {
        let pts = [pos2(0.0, 0.0), pos2(200.0, 0.0), pos2(200.0, 100.0), pos2(0.0, 100.0)];
        assert_eq!(crop_zone(&pts, pos2(2.0, 3.0), true), Some(0), "a corner");
        assert_eq!(crop_zone(&pts, pos2(40.0, 4.0), true), Some(4), "the top edge, far from its middle");
        assert_eq!(crop_zone(&pts, pos2(160.0, 104.0), false), Some(6), "just outside the bottom edge");
        assert_eq!(crop_zone(&pts, pos2(100.0, 50.0), true), Some(8), "inside moves");
        assert_eq!(crop_zone(&pts, pos2(300.0, 50.0), false), None, "outside rotates");
        for q in [pos2(f32::NAN, 0.0), pos2(f32::INFINITY, 1.0)] {
            assert_eq!(crop_zone(&pts, q, false), None);
        }
        assert_eq!(crop_zone(&[pos2(0.0, 0.0); 4], pos2(50.0, 50.0), true), Some(8), "a box of no size");
        assert_eq!(crop_zone(&[], pos2(1.0, 1.0), false), None);
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
fn guided_overlay(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, frame: &Frame, d: &DevelopSettings, id: PhotoId) {
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
        register(ui.ctx(), format!("uprightGuide:{i}"), Rect::from_two_pos(sa, sb));
        draw(sa, sb, selected == Some(i));
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    // double-click on the photo finishes the tool, as in the crop tool (same as Return / Done); issue #297
    // A press starts a new guide right away (below), so the second click of a double-click has
    // already begun one: a guide that never left its press point is that click, not a drawn guide.
    let here = resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.latest_pos())).map(|q| map.norm(q));
    let drawing = match (&app.gesture, here) {
        (Some(Gesture::Guide { a }), Some(q)) => q.dist(*a) > 0.02,
        _ => false,
    };
    if resp.double_clicked() && !drawing {
        if matches!(app.gesture, Some(Gesture::Guide { .. })) {
            app.gesture = None;
        }
        let _ = app.run("tool.done", json!({}));
        return;
    }
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

/// The crop the photo gets: shrunk into the warped photo's valid area as the render does it (lens
/// corrections always, perspective with Constrain Crop).
fn shown_crop(d: &DevelopSettings, frame: &Frame) -> lcc_geom::CropGeometry {
    frame.constrained_crop(d.crop.geometry, d.geometry.constrain_crop)
}

fn frame_crop_quad(d: &DevelopSettings, frame: &Frame) -> [Point; 4] {
    let f = Frame { crop: d.crop.geometry, ..frame.clone() };
    f.crop_quad_norm()
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
fn seg_center(seg: &lcc_develop::SegMask) -> Option<Point> {
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

/// The mask whose pin is under `q` (within the 10 px the pin grips grab at), not counting
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

/// `shape` dragged from `at - dn` to `at` (normalized) by `handle`: 0 moves it; a radial
/// gradient's 1..4 resize it, 5 and [`ROTATE`] turn it, 6 sets the feather (its inner ring), 9
/// draws a new one; a linear gradient's 1 / 2 set its start / end, [`ROTATE`] turns it about its
/// middle, [`LINE_START`] / [`LINE_END`] slide the start / end line along it.
fn moved_shape(shape: &MaskShape, handle: u8, dn: Point, at: Point, map: &CanvasMap, alt: bool) -> MaskShape {
    let mv = |p: Point| Point::new(p.x + dn.x, p.y + dn.y);
    let press = Point::new(at.x - dn.x, at.y - dn.y);
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
            // turning: by as much as the pointer went round the centre (on screen)
            5 | ROTATE => {
                let turn = screen_angle(map, at, center) - screen_angle(map, press, center);
                let mut new_angle = angle + f64::from(turn).to_degrees();
                if !new_angle.is_finite() {
                    new_angle = angle;
                }
                MaskShape::Radial { center, rx, ry, angle: (new_angle + 180.0).rem_euclid(360.0) - 180.0, feather, invert }
            }
            // the feather ring: inside it the effect is full, so its size sets the feather
            6 => {
                let r = radial_r(shape, at, map).unwrap_or(1.0 - feather / 100.0);
                let feather = ((1.0 - r) * 100.0).clamp(0.0, 100.0).round();
                MaskShape::Radial { center, rx, ry, angle, feather, invert }
            }
            _ => MaskShape::Radial { center: mv(center), rx, ry, angle, feather, invert },
        },
        MaskShape::Linear { start, end } => match handle {
            1 => MaskShape::Linear { start: at, end },
            2 => MaskShape::Linear { start, end: at },
            ROTATE => {
                let mid = Point::new((start.x + end.x) / 2.0, (start.y + end.y) / 2.0);
                let turn = screen_angle(map, at, mid) - screen_angle(map, press, mid);
                if !turn.is_finite() {
                    return MaskShape::Linear { start, end };
                }
                MaskShape::Linear { start: turned(map, start, mid, turn), end: turned(map, end, mid, turn) }
            }
            LINE_START | LINE_END => {
                // only along the gradient: the line stays parallel, the fade gets wider or narrower
                let (a, b) = (map.screen(start), map.screen(end));
                let len = a.distance(b);
                if len.is_nan() || len < 1.0 {
                    return MaskShape::Linear { start, end };
                }
                let dir = (b - a) / len;
                let shift = (map.screen(at) - map.screen(press)).dot(dir);
                if handle == LINE_START {
                    MaskShape::Linear { start: map.norm(a + dir * shift), end }
                } else {
                    MaskShape::Linear { start, end: map.norm(b + dir * shift) }
                }
            }
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

/// Where the radial gradient's feather handle sits on its inner ring (degrees, in the ellipse's own
/// frame: up and to the right of the centre).
const FEATHER_HANDLE_DEG: f64 = -45.0;

/// The inner (feather) ring's size relative to the outer edge: the effect is full inside it and
/// fades to nothing at the edge (`feather` 0..100, as the pipeline draws it).
fn radial_inner(feather: f64) -> f64 {
    if feather.is_finite() { (1.0 - feather / 100.0).clamp(0.0, 1.0) } else { 0.5 }
}

/// The point at `deg` on the ellipse `scale` times the radial gradient's, on screen.
fn radial_point(map: &CanvasMap, center: Point, rx: f64, ry: f64, angle: f64, scale: f64, deg: f64) -> Pos2 {
    let l = frame_long_norm(map);
    let (s, co) = angle.to_radians().sin_cos();
    let a = deg.to_radians();
    let (x, y) = (scale * rx * a.cos(), scale * ry * a.sin());
    map.screen(Point::new(center.x + (x * co - y * s) * l.0, center.y + (x * s + y * co) * l.1))
}

/// The radial gradient's ellipse scaled by `scale`, as a closed line on screen.
fn radial_ring(map: &CanvasMap, center: Point, rx: f64, ry: f64, angle: f64, scale: f64) -> Vec<Pos2> {
    (0..72).map(|i| radial_point(map, center, rx, ry, angle, scale, i as f64 * 5.0)).collect()
}

/// How far `at` (normalized) is from the radial gradient's centre, 1 on its edge.
fn radial_r(shape: &MaskShape, at: Point, map: &CanvasMap) -> Option<f64> {
    let MaskShape::Radial { center, rx, ry, angle, .. } = shape else { return None };
    if !rx.is_finite() || !ry.is_finite() || *rx <= 0.0 || *ry <= 0.0 {
        return None;
    }
    let l = frame_long_norm(map);
    let dx = (at.x - center.x) / l.0;
    let dy = (at.y - center.y) / l.1;
    let (s, co) = angle.to_radians().sin_cos();
    let x = (dx * co + dy * s) / rx;
    let y = (-dx * s + dy * co) / ry;
    let r = (x * x + y * y).sqrt();
    r.is_finite().then_some(r)
}

/// `p` turned by `turn` radians about `c`, on screen (so the photo's aspect doesn't skew it).
fn turned(map: &CanvasMap, p: Point, c: Point, turn: f32) -> Point {
    let (pq, cq) = (map.screen(p), map.screen(c));
    let v = pq - cq;
    let (s, co) = turn.sin_cos();
    map.norm(cq + vec2(v.x * co - v.y * s, v.x * s + v.y * co))
}

/// The angle (radians, on screen) of `p` seen from `c`.
fn screen_angle(map: &CanvasMap, p: Point, c: Point) -> f32 {
    (map.screen(p) - map.screen(c)).angle()
}

/// The double-arrow cursor that resizes along `dir` (on screen).
fn resize_cursor(dir: egui::Vec2) -> egui::CursorIcon {
    let deg = dir.y.atan2(dir.x).to_degrees().rem_euclid(180.0);
    match deg {
        d if !(22.5..157.5).contains(&d) => egui::CursorIcon::ResizeHorizontal,
        d if d < 67.5 => egui::CursorIcon::ResizeNwSe,
        d if d < 112.5 => egui::CursorIcon::ResizeVertical,
        _ => egui::CursorIcon::ResizeNeSw,
    }
}

/// What a press at `q` on the selected gradient `shape` grabs when no handle is under it: a
/// handle number for [`moved_shape`] (0 moves it), or `None` (nothing: the press is elsewhere).
/// Radial: inside moves, just outside the edge rotates. Linear: the middle line rotates, the
/// start and end lines move that line only (the fade's width), anywhere else moves the gradient.
fn gradient_part(shape: &MaskShape, q: Pos2, map: &CanvasMap) -> Option<u8> {
    match shape {
        MaskShape::Radial { center, .. } => {
            let r = radial_r(shape, map.norm(q), map)?;
            if r <= 1.0 {
                return Some(0);
            }
            // a band outside the edge, about 16 points wide on screen
            let edge = map.screen(*center).distance(q) / r as f32;
            (r <= 1.0 + f64::from(16.0 / edge.max(1.0))).then_some(ROTATE)
        }
        MaskShape::Linear { start, end } => {
            let (a, b) = (map.screen(*start), map.screen(*end));
            let len = a.distance(b);
            if len.is_nan() || len < 1.0 {
                return Some(0);
            }
            let dir = (b - a) / len;
            // distance along the gradient from the start line, in points
            let t = (q - a).dot(dir);
            if (t - len / 2.0).abs() < 6.0 {
                Some(ROTATE)
            } else if t.abs() < 6.0 {
                Some(LINE_START)
            } else if (t - len).abs() < 6.0 {
                Some(LINE_END)
            } else {
                Some(0)
            }
        }
        _ => None,
    }
}

/// [`moved_shape`]'s handles beyond the ones drawn: turning a gradient about its centre, and
/// moving a linear gradient's start or end line along it (the width of the fade, not its angle).
const ROTATE: u8 = 7;
const LINE_START: u8 = 11;
const LINE_END: u8 = 12;

/// The selected mask `m`'s gradient part under `q` when no handle is: (component, handle). A
/// linear gradient's lines first, then a radial gradient's body or the band round it, then the
/// first linear gradient as a whole (anywhere on the photo moves it).
fn gradient_grab(m: &lcc_develop::Mask, q: Pos2, map: &CanvasMap, active: Option<usize>) -> Option<(usize, u8)> {
    let linear = |c: &&lcc_develop::MaskComponent| matches!(c.shape, MaskShape::Linear { .. });
    for (ci, c) in m.components.iter().enumerate().filter(|(_, c)| linear(c)) {
        if let Some(h) = gradient_part(&c.shape, q, map).filter(|h| *h != 0) {
            return Some((ci, h));
        }
    }
    for (ci, c) in m.components.iter().enumerate().filter(|(_, c)| matches!(c.shape, MaskShape::Radial { .. })) {
        if let Some(h) = gradient_part(&c.shape, q, map) {
            return Some((ci, h));
        }
    }
    // anywhere else drags the selected linear gradient (the active component, else the first)
    active.filter(|&ci| m.components.get(ci).is_some_and(|c| linear(&c))).or_else(|| m.components.iter().position(|c| linear(&c))).map(|ci| (ci, 0))
}

/// The system cursor for dragging `handle` of `shape` from `q`; `None` for a turn (no system
/// cursor shows that: the canvas draws its own).
fn gradient_cursor(shape: &MaskShape, handle: u8, q: Pos2, map: &CanvasMap) -> Option<egui::CursorIcon> {
    match (shape, handle) {
        (_, 0) => Some(egui::CursorIcon::Move),
        (MaskShape::Radial { .. }, 5 | ROTATE) | (MaskShape::Linear { .. }, ROTATE) => None,
        (MaskShape::Radial { center, rx, ry, angle, .. }, h) => {
            let deg = match h {
                1 | 3 => 0.0,
                2 | 4 => 90.0,
                _ => FEATHER_HANDLE_DEG,
            };
            let c = map.screen(*center);
            let dir = radial_point(map, *center, *rx, *ry, *angle, 1.0, deg) - c;
            let dir = if dir.length() > 0.5 { dir } else { q - c };
            Some(resize_cursor(dir))
        }
        (MaskShape::Linear { start, end }, _) => Some(resize_cursor(map.screen(*end) - map.screen(*start))),
        _ => Some(egui::CursorIcon::Move),
    }
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
    // the mask's Color Mixer targeted adjustment: a vertical drag moves the bands under the point
    if let Some(group) = app.ui.tool.strip_prefix("maskMixer:").map(str::to_string) {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if resp.dragged()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let dy = -resp.drag_delta().y;
            if dy != 0.0 {
                let n = map.norm(q - resp.drag_delta());
                let _ = app.run("mask.mixerTargeted", json!({"target": group, "x": n.x, "y": n.y, "delta": f64::from(dy) * 0.5}));
            }
        }
        return;
    }
    // the mask's Point Color eyedropper: one click samples into the active mask
    if app.ui.tool == "maskPointColor" {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.clicked()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let n = map.norm(q);
            if let Ok(r) = app.run("mask.pointColorPick", json!({"x": n.x, "y": n.y})) {
                app.ui.mask_point_color = r["index"].as_u64().unwrap_or(0) as usize;
            }
            app.ui.tool.clear();
        }
        return;
    }
    // sizes are in units of the uncropped photo's long edge, as the render reads them
    let long = f64::from(map.per_long());
    let active = app.session.active_mask;
    // no tool armed as this frame starts (a click that makes a mask puts its tool away: that click doesn't zoom)
    let idle = app.ui.tool.is_empty();
    let clip = app.canvas_rect.unwrap_or(map.rect);
    let mut painter = ui.painter_at(clip);
    // a slider or dial at work: no lines, handles or pins over the effect
    if adjusting(app) {
        painter.set_invisible();
    }
    let p = &painter;
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
        // (its coordinates on the photo, or an old mask's on the corrected frame)
        let map = &map.for_mask(m.on_photo);
        // Ctrl+Shift+O (`mask_overlay_all`) shows every visible mask's region; outline each one
        // too, not just the selected mask. Handles and the rotation grip stay on the selected
        // mask so nothing else becomes draggable. The component check keeps this in step with the
        // painted region (`view_overlay` at detail.rs:1354 only covers masks with components).
        let show_outline = sel || (app.ui.mask_overlay_all && m.visible && !m.components.is_empty());
        let stroke_a: u8 = if sel { 230 } else { 170 };
        for (ci, c) in m.components.iter().enumerate() {
            match &c.shape {
                MaskShape::Radial { center, rx, ry, angle, feather, .. } if show_outline => {
                    let pts: Vec<Pos2> = (0..64)
                        .map(|i| {
                            let a = i as f64 / 64.0 * std::f64::consts::TAU;
                            let (s, co) = angle.to_radians().sin_cos();
                            let (x, y) = (rx * a.cos(), ry * a.sin());
                            let l = frame_long_norm(map);
                            map.screen(Point::new(center.x + (x * co - y * s) * l.0, center.y + (x * s + y * co) * l.1))
                        })
                        .collect();
                    p.add(egui::Shape::closed_line(pts, Stroke::new(1.0, Color32::from_white_alpha(stroke_a))));

                    if sel {
                        // inside the edge, the feather ring (where the effect is full), with a
                        // handle that sets the feather
                        let inner = radial_inner(*feather);
                        p.add(egui::Shape::dashed_line(
                            &radial_ring(map, *center, *rx, *ry, *angle, inner),
                            Stroke::new(1.0, Color32::from_white_alpha(170)),
                            4.0,
                            3.0,
                        ));
                        let fq = radial_point(map, *center, *rx, *ry, *angle, inner, FEATHER_HANDLE_DEG);
                        p.circle_filled(fq, 4.0, Color32::from_black_alpha(90));
                        p.circle_stroke(fq, 4.0, Stroke::new(1.5, Color32::WHITE));
                        register(ui.ctx(), format!("maskHandle:{}:{ci}:6", m.id), Rect::from_center_size(fq, vec2(14.0, 14.0)));
                        grips.push((m.id, ci, 6, fq));
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
                }
                MaskShape::Linear { start, end } if show_outline => {
                    let (a, b) = (map.screen(*start), map.screen(*end));
                    let (a1, a2): (u8, u8) = if sel { (230, 110) } else { (170, 80) };
                    // start (full effect), the middle (50 %, with the pin) and the end (no effect)
                    let middle = Point::new((start.x + end.x) / 2.0, (start.y + end.y) / 2.0);
                    for (at, alpha) in [(*start, a1), (middle, a1.saturating_sub(70).max(80)), (*end, a2)] {
                        for run in linear_guide(map, *start, *end, at) {
                            p.add(egui::Shape::line(run, Stroke::new(1.0, Color32::from_white_alpha(alpha))));
                        }
                    }
                    if sel {
                        for (i, q) in [a, b].iter().enumerate() {
                            p.circle_stroke(*q, 5.0, Stroke::new(1.5, Color32::WHITE));
                            register(ui.ctx(), format!("maskHandle:{}:{ci}:{i}", m.id), Rect::from_center_size(*q, vec2(14.0, 14.0)));
                            grips.push((m.id, ci, i as u8 + 1, *q));
                        }
                    }
                }
                _ => {}
            }
            let pin_allowed = sel || app.ui.mask_unselected_pins;
            if app.ui.mask_pins
                && pin_allowed
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
    // the pointer resting on a mask's pin shows what that mask selects (as its row does)
    let pin_hover = resp
        .hover_pos()
        .and_then(|q| grips.iter().filter(|g| g.2 == 0 && g.3.distance(q) < 10.0).min_by(|a, b| a.3.distance(q).total_cmp(&b.3.distance(q))))
        .map(|g| g.0);
    if app.ui.mask_pin_hover != pin_hover {
        app.ui.mask_pin_hover = pin_hover;
        ui.ctx().request_repaint();
    }
    // a right-click on a pin: the photo's menu is that mask's (`masking::photo_mask_menu`)
    if resp.secondary_clicked() {
        let at = resp.interact_pointer_pos();
        app.ui.mask_menu_for =
            at.and_then(|q| grips.iter().filter(|g| g.3.distance(q) < 10.0).min_by(|a, b| a.3.distance(q).total_cmp(&b.3.distance(q)))).map(|g| g.0);
    }
    mask_keys(app, ui, map, d, &grips);
    // Object selections (SAM 3) of the selected mask: each click, green to include, red to exclude
    // (clicks still on their way to the model included)
    let pending = app.session.segmenter.pending_clicks().cloned();
    for m in d.masks.iter().filter(|m| Some(m.id) == active) {
        let map = &map.for_mask(m.on_photo);
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
    // Color Range and Object armed for a new mask: the first click (or drag) on the photo makes it,
    // and the tool then picks or selects there as usual
    if let Some((_, kind)) = app.ui.mask_new.clone().filter(|(_, k)| (k == "colorRange" || k == "object") && app.ui.tool == *k)
        && (resp.clicked() || resp.drag_started())
    {
        match app.run_typed("mask.add", json!({"kind": kind})) {
            Ok(_) => {
                app.ui.mask_new = None;
                app.ui.tool = kind;
                app.ui.active_mask_component = Some(0);
            }
            Err(e) => {
                app.ai_error(ui.ctx(), e, Some((kind.as_str(), "new")));
                return;
            }
        }
    }
    // Colour Range "Pick": the Masking panel owns the loupe's clicks (general_interaction
    // never runs while it is open), so the eyedropper is handled here too.
    if app.ui.tool == "colorRange" {
        if let Some(q) = resp.hover_pos() {
            crate::eyedropper::show(ui.ctx(), q);
        }
        super::range_pick::color_tool(app, ui, resp, map, d, p);
        return;
    }
    // the Luminance Range picker
    if app.ui.tool == "lumRange" {
        if let Some(q) = resp.hover_pos() {
            crate::eyedropper::show(ui.ctx(), q);
        }
        super::range_pick::luminance_tool(app, ui, resp, map, p);
        return;
    }
    // Object tool: a click includes what's under it, ⌥-click leaves it out. A click on another
    // mask's pin selects that mask instead of adding a click to the selected one (issue #3049);
    // the tool then stays on only if the newly selected mask has an Object selection to refine. A
    // click on the selected mask's own pin does nothing: its first click is already there, and a
    // near-copy of it would use up one of the mask's limited Object clicks.
    if app.ui.tool == "object" {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            // ⌥ / Alt: the click leaves out what's under it; a minus beside the pointer says so
            if ui.input(|i| i.modifiers.alt)
                && let Some(h) = resp.hover_pos()
            {
                brush_minus(p, h + vec2(14.0, 14.0));
                register(ui.ctx(), "objectCursor:exclude", Rect::from_center_size(h, vec2(12.0, 12.0)));
            }
        }
        if resp.clicked()
            && let Some(q) = resp.interact_pointer_pos()
        {
            if let Some(mid) = other_pin_at(&grips, app.session.active_mask, q) {
                let _ = app.run("mask.select", json!({"id": mid}));
                let has_object =
                    d.masks.iter().find(|m| m.id == mid).is_some_and(|m| m.components.iter().any(|c| matches!(c.shape, MaskShape::Object { .. })));
                if !has_object {
                    app.ui.tool.clear();
                }
                return;
            }
            if own_pin_at(&grips, app.session.active_mask, q) {
                return;
            }
            let n = map.for_mask_id(d, active).norm(q);
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
        // ⌥ / Alt switches painting and erasing, read every frame; the panel and the [ / ] keys
        // follow it to the eraser's own settings
        app.ui.brush_alt = ui.input(|i| i.modifiers.alt);
        let set = app.ui.brush_set(app.ui.brush_erasing());
        // Lightroom's brush: Size is the solid core, Feather adds a halo outside it, and Size is a
        // size on the screen, the same at every zoom
        let r0 = crate::state::brush_core_points(set.size);
        let r = r0 * lcc_develop::brush_model::outer_ratio(set.feather as f64) as f32;
        if let Some(h) = resp.hover_pos() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            // a bold circle round the core, a thin one where the halo ends
            p.circle_stroke(h, r0, Stroke::new(2.0, Color32::WHITE));
            if r > r0 + 0.5 {
                p.circle_stroke(h, r, Stroke::new(1.0, Color32::from_white_alpha(150)));
            }
            // the centre, where Auto Mask samples what to follow: a minus while the stroke would
            // erase (Erase on, or ⌥ / Alt held), a cross while it paints
            let erasing = app.ui.brush_erasing();
            if erasing {
                brush_minus(p, h);
            } else {
                brush_crosshair(p, h);
            }
            register(ui.ctx(), if erasing { "brushCursor:erase" } else { "brushCursor:paint" }, Rect::from_center_size(h, vec2(2.0 * r, 2.0 * r)));
        }
        if let Some(q) = resp.interact_pointer_pos()
            && (resp.drag_started() || resp.dragged() || resp.clicked())
        {
            // the stroke goes into the selected mask (or a new one): in its coordinates
            let n = map.for_mask_id(d, active).norm(q);
            let force = if app.ui.brush_pressure { ui.input(pen_force) } else { None };
            // ⌥ / Alt is read every frame of the stroke: each point keeps the mode it was painted in
            let erasing = app.ui.brush_erase != ui.input(|i| i.modifiers.alt);
            match &mut app.gesture {
                // a held, unmoving pointer records nothing: coverage builds by passes, not time (#518)
                Some(Gesture::Brush { points, .. }) if points.last() == Some(&n) => {}
                Some(Gesture::Brush { points, pressure, erase, .. }) => {
                    record_pressure(pressure, points.len(), force);
                    points.push(n);
                    erase.push(erasing);
                }
                // a stroke starts with a press, not part-way through a drag: one ended for picking
                // another mask or photo (`LccApp::cancel_brush_gesture`) paints nothing more until
                // the button is let go
                _ if !(resp.drag_started() || resp.clicked()) => {}
                _ => {
                    let mut pressure = Vec::new();
                    record_pressure(&mut pressure, 0, force);
                    app.gesture = Some(Gesture::Brush { points: vec![n], pressure, erase: vec![erasing], sent: 0 });
                }
            }
        }
        // the stroke being painted (the render shows it once it's committed), with the committed
        // stroke's dab spacing, feather and flow (issue #517)
        // a mask with settings and Auto Mask off shows the stroke's effect while it is painted (the
        // stroke goes to the mask live); the red stroke is for a mask without settings, or with the
        // overlay on
        let target = if app.ui.mask_new.is_some() { None } else { d.masks.iter().find(|m| Some(m.id) == active) };
        let live = !app.ui.brush_auto_mask && target.is_some_and(mask_has_settings);
        if live
            && resp.dragged()
            && let Some(Gesture::Brush { points, pressure, erase, sent }) = &app.gesture
            && points.len() > 1
        {
            let (points, pressure, erase, sent) = (points.clone(), pressure.clone(), erase.clone(), *sent);
            if sent == 0 {
                let _ = app.run("develop.beginInteraction", json!({"label": "Brush"}));
            }
            let params = brush_params(app, d, &points, &pressure, &erase, sent, false, long);
            let n = params["runs"].as_array().map_or(1, Vec::len);
            if app.run("mask.brushStroke", params).is_ok()
                && let Some(Gesture::Brush { sent, .. }) = &mut app.gesture
            {
                *sent = n;
            }
        }
        if let Some(Gesture::Brush { points, pressure, sent: 0, .. }) = &app.gesture {
            let screen: Vec<Pos2> = {
                let mmap = map.for_mask_id(d, active);
                points.iter().map(|q| mmap.screen(*q)).collect()
            };
            let opacity = if app.ui.mask_overlay && app.ui.mask_overlay_opacity > 0.0 { app.ui.mask_overlay_opacity / 100.0 } else { 110.0 / 255.0 };
            let params = crate::brush_live::LiveParams {
                clip,
                scale: ui.ctx().pixels_per_point(),
                r,
                hard: 0.0,
                lrc: Some({
                    let (a, pw) = lcc_develop::brush_model::profile(set.feather as f64);
                    (a as f32 * r0, pw as f32)
                }),
                flow: (set.flow / 100.0).clamp(0.0, 1.0),
                density: (set.density / 100.0).clamp(0.0, 1.0),
                color: app.ui.mask_overlay_color,
                opacity: opacity.clamp(0.0, 1.0),
            };
            crate::brush_live::paint(&mut app.brush_live, ui.ctx(), p, params, &screen, pressure);
        } else {
            app.brush_live = None;
        }
        if (resp.drag_stopped() || resp.clicked())
            && let Some(Gesture::Brush { points, pressure, erase, sent }) = app.gesture.take()
        {
            app.brush_live = None;
            // a ⇧-click draws a straight line from where the last stroke ended, as in Lightroom
            let shift = resp.ctx.input(|i| i.modifiers.shift);
            let connect = shift && resp.clicked() && points.len() == 1;
            let params = brush_params(app, d, &points, &pressure, &erase, sent, connect, long);
            let made = app.run("mask.brushStroke", params);
            if sent > 0 {
                let _ = app.run("develop.endInteraction", json!({}));
            }
            // the first stroke of a tool armed for a new mask made it: the brush paints on into it
            if made.is_ok() && app.ui.mask_new.take().is_some() {
                app.ui.active_mask_component = Some(0);
            }
        }
        return;
    }
    // a gradient tool armed for a new mask: a drag draws it (one undo step), a click places one
    if matches!(app.ui.tool.as_str(), "linear" | "radial") && (app.ui.mask_new.is_some() || app.ui.mask_drawing.is_some()) {
        draw_new_gradient(app, ui, resp, map, d);
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
    // what a press here grabs: a handle or pin, else a part of the selected mask's gradients
    let active_comp = app.ui.active_mask_component;
    let grab = |q: Pos2| {
        hit(q).map(|(m, c, h, _)| (m, c, h)).or_else(|| {
            let m = d.masks.iter().find(|m| Some(m.id) == active)?;
            gradient_grab(m, q, &map.for_mask(m.on_photo), active_comp).map(|(ci, h)| (m.id, ci, h))
        })
    };
    // the pointer says what a drag will do: move, resize (the edge, a line, the feather ring) or
    // turn the gradient
    let held = match &app.gesture {
        Some(Gesture::MaskHandle { mask, comp, handle, .. }) => Some((*mask, *comp, *handle)),
        _ => None,
    };
    if let Some(h) = resp.hover_pos().or_else(|| held.and_then(|_| ui.input(|i| i.pointer.latest_pos())))
        && let Some((mid, ci, handle)) = held.or_else(|| grab(h))
        && let Some(shape) = d.masks.iter().find(|m| m.id == mid).and_then(|m| m.components.get(ci)).map(|c| &c.shape)
        && !app.ui.mask_adjusting_until.is_some_and(|t| std::time::Instant::now() < t)
        && app.ui.mask_new.is_none()
    {
        let mmap = map.for_mask_id(d, Some(mid));
        let map = &mmap;
        match gradient_cursor(shape, handle, h, map) {
            Some(cursor) => ui.ctx().set_cursor_icon(cursor),
            None => {
                let c = match shape {
                    MaskShape::Radial { center, .. } => map.screen(*center),
                    MaskShape::Linear { start, end } => map.screen(*start).lerp(map.screen(*end), 0.5),
                    _ => h,
                };
                let away = h - c;
                let turn = if away.length() > 0.5 { away.y.atan2(away.x) + std::f32::consts::FRAC_PI_2 } else { 0.0 };
                rotate_cursor(ui, h, turn);
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            }
        }
    }
    let clicked_grip = resp.clicked().then(|| resp.interact_pointer_pos().and_then(hit)).flatten();
    if let Some((mid, ci, ..)) = clicked_grip {
        if Some(mid) != active {
            let _ = app.run("mask.select", json!({"id": mid}));
        }
        app.ui.active_mask_component = Some(ci);
    }
    // no tool armed and no pin or handle clicked: a click zooms, as in Edit (a drag pans below)
    if (resp.clicked() || resp.double_clicked())
        && clicked_grip.is_none()
        && idle
        && app.ui.tool.is_empty()
        && let Some(canvas) = app.canvas_rect
    {
        click_zoom_and_pan(app, resp, map.rect, canvas);
    }
    if resp.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        let grip = grab(q);
        if let Some((mask, comp, handle)) = grip
            && d.masks.iter().find(|m| m.id == mask).and_then(|m| m.components.get(comp)).is_some()
        {
            if Some(mask) != active {
                let _ = app.run("mask.select", json!({"id": mask}));
            }
            app.ui.active_mask_component = Some(comp);
            let _ = app.run("develop.beginInteraction", json!({"label": "Edit Mask"}));
            // (read after it begins: an old mask is on the photo from then on, in its coordinates)
            if let Some(original_shape) = current_shape(app, mask, comp)
                .or_else(|| d.masks.iter().find(|m| m.id == mask).and_then(|m| m.components.get(comp)).map(|c| c.shape.clone()))
            {
                app.gesture = Some(Gesture::MaskHandle { mask, comp, handle, original_shape });
            }
        } else if grip.is_none()
            && app.ui.tool.is_empty()
            && let (Some(img), Some(canvas)) = (app.image_rect, app.canvas_rect)
            && zoomed(img, canvas)
        {
            // no tool armed and nothing grabbed: the drag pans the zoomed photo, as in the loupe
            app.gesture = Some(Gesture::Pan { start: app.ui.pan, at: q });
            pan_image(app, canvas, img, resp.drag_delta());
        }
    } else if let Some(Gesture::Pan { .. }) = app.gesture {
        if resp.dragged()
            && let (Some(img), Some(canvas)) = (app.image_rect, app.canvas_rect)
        {
            pan_image(app, canvas, img, resp.drag_delta());
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        if resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            app.gesture = None;
        }
        return;
    }
    if resp.dragged()
        && let (Some(Gesture::MaskHandle { mask, comp, handle, original_shape }), Some(q)) = (app.gesture.clone(), resp.interact_pointer_pos())
    {
        // (the live flag: the drag's first frame began the interaction, which put an old mask on the photo)
        let map = &current_on_photo(app, mask).map_or_else(|| map.for_mask_id(d, Some(mask)), |on| map.for_mask(on));
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

/// The erasing brush's centre: a minus (dark under light, so it reads on any photo).
fn brush_minus(p: &egui::Painter, c: Pos2) {
    for (w, col) in [(3.5, Color32::from_black_alpha(140)), (1.5, Color32::WHITE)] {
        p.line_segment([c - vec2(6.0, 0.0), c + vec2(6.0, 0.0)], Stroke::new(w, col));
    }
}

/// The painting brush's centre: a small cross (dark under light, so it reads on any photo).
fn brush_crosshair(p: &egui::Painter, c: Pos2) {
    for (w, col) in [(3.0, Color32::from_black_alpha(140)), (1.0, Color32::WHITE)] {
        p.line_segment([c - vec2(5.0, 0.0), c + vec2(5.0, 0.0)], Stroke::new(w, col));
        p.line_segment([c - vec2(0.0, 5.0), c + vec2(0.0, 5.0)], Stroke::new(w, col));
    }
}

/// A linear or radial gradient drawn by a drag of a tool armed for a new mask: the press makes the
/// mask (the next-mask settings), the drag shapes it from the press (Linear: start to end; Radial:
/// centred on the press, out to the pointer), letting go ends it as one undo step. A click
/// places a gradient of the default size there.
fn draw_new_gradient(app: &mut LccApp, ui: &egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let kind = app.ui.tool.clone();
    let mmap = map.for_mask_id(d, None);
    if resp.clicked()
        && app.ui.mask_drawing.is_none()
        && let Some(q) = resp.interact_pointer_pos()
    {
        let n = mmap.norm(q);
        let params = if kind == "linear" {
            json!({"kind": kind, "start": [n.x, n.y - 0.15], "end": [n.x, n.y + 0.15]})
        } else {
            json!({"kind": kind, "center": [n.x, n.y]})
        };
        if app.run("mask.add", params).is_ok() {
            app.ui.mask_new = None;
            app.ui.active_mask_component = Some(0);
        }
        return;
    }
    if resp.drag_started()
        && app.ui.mask_drawing.is_none()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        let o = mmap.norm(q);
        let params = if kind == "linear" {
            json!({"kind": kind, "start": [o.x, o.y], "end": [o.x, o.y + 0.001]})
        } else {
            json!({"kind": kind, "center": [o.x, o.y], "rx": 0.005, "ry": 0.005})
        };
        let _ = app.run("develop.beginInteraction", json!({"label": "Add Mask"}));
        match app.run("mask.add", params) {
            Ok(_) => {
                if let Some(mid) = app.session.active_mask {
                    app.ui.mask_drawing = Some((mid, 0, o));
                }
                app.ui.mask_new = None;
                app.ui.active_mask_component = Some(0);
            }
            Err(_) => {
                let _ = app.run("develop.endInteraction", json!({}));
            }
        }
        return;
    }
    if resp.dragged()
        && let (Some((mid, comp, o)), Some(q)) = (app.ui.mask_drawing, resp.interact_pointer_pos())
        && let Some(shape) = d.masks.iter().find(|m| m.id == mid).and_then(|m| m.components.get(comp)).map(|c| c.shape.clone())
    {
        let n = map.for_mask_id(d, Some(mid)).norm(q);
        let shape = match shape {
            MaskShape::Linear { .. } => Some(MaskShape::Linear { start: o, end: n }),
            MaskShape::Radial { angle, feather, invert, .. } => {
                Some(MaskShape::Radial { center: o, rx: (n.x - o.x).abs().max(0.005), ry: (n.y - o.y).abs().max(0.005), angle, feather, invert })
            }
            _ => None,
        };
        if let Some(shape) = shape {
            let _ = app.run("mask.update", json!({"id": mid, "component": comp, "shape": shape}));
        }
    }
    if resp.drag_stopped() && app.ui.mask_drawing.take().is_some() {
        let _ = app.run("develop.endInteraction", json!({}));
    }
}

/// `mask.brushStroke`'s parameters for brush gesture `points` (with its `pressure` and each point's
/// `erase` mode): one stroke, or `runs` of one mode each joined without a gap; `sent` strokes
/// sent live before are replaced (`continue`). The stroke goes into the selected mask's active
/// Brush component, or makes a new mask while a tool is armed for one. Each run paints with its
/// mode's settings (the eraser keeps its own); Size, a size on the screen, goes as the core's
/// radius over the photo's on-screen long side `long` (points).
#[allow(clippy::too_many_arguments)]
pub(crate) fn brush_params(
    app: &LccApp,
    d: &DevelopSettings,
    points: &[Point],
    pressure: &[f32],
    erase: &[bool],
    sent: usize,
    connect: bool,
    long: f64,
) -> serde_json::Value {
    let pt = |q: &Point| [q.x, q.y];
    let mut runs: Vec<(bool, Vec<[f64; 2]>, Vec<f32>)> = Vec::new();
    for (i, q) in points.iter().enumerate() {
        let mode = erase.get(i).or(erase.last()).copied().unwrap_or(app.ui.brush_erase);
        let p = pressure.get(i).copied();
        match runs.last_mut() {
            Some((m, pts, pr)) if *m == mode => {
                pts.push(pt(q));
                pr.extend(p);
            }
            last => {
                // a new run starts where the last one ended: no gap between them
                let join = last.and_then(|(_, pts, pr)| Some((*pts.last()?, pr.last().copied())));
                let (mut pts, mut pr) = (Vec::new(), Vec::new());
                if let Some((at, p0)) = join {
                    pts.push(at);
                    pr.extend(p0);
                }
                pts.push(pt(q));
                pr.extend(p);
                runs.push((mode, pts, pr));
            }
        }
    }
    // the stroke's radius in photo units: the core's screen radius over the photo's screen size
    let values = |erase: bool| {
        let s = app.ui.brush_set(erase);
        let size = f64::from(crate::state::brush_core_points(s.size)) / long.max(1.0);
        json!({"size": size, "feather": s.feather, "flow": s.flow, "density": s.density})
    };
    let mut params = json!({"autoMask": app.ui.brush_auto_mask, "autoRange": app.ui.brush_auto_range, "connect": connect});
    let merge = |into: &mut serde_json::Value, from: serde_json::Value| {
        if let (Some(o), serde_json::Value::Object(f)) = (into.as_object_mut(), from) {
            o.extend(f);
        }
    };
    match runs.as_slice() {
        [(mode, pts, pr)] => {
            merge(&mut params, values(*mode));
            params["points"] = json!(pts);
            params["erase"] = json!(mode);
            params["pressure"] = json!(pr);
        }
        _ => {
            params["runs"] = runs
                .iter()
                .map(|(mode, pts, pr)| {
                    let mut r = json!({"points": pts, "erase": mode, "pressure": pr});
                    merge(&mut r, values(*mode));
                    r
                })
                .collect();
        }
    }
    if sent > 0 {
        params["continue"] = json!(true);
        params["replace"] = json!(sent);
    }
    if app.ui.mask_new.is_some() {
        return params;
    }
    let is_brush = app
        .ui
        .active_mask_component
        .and_then(|ci| d.masks.iter().find(|m| Some(m.id) == app.session.active_mask).and_then(|m| m.components.get(ci)))
        .is_some_and(|c| matches!(c.shape, MaskShape::Brush { .. }));
    if is_brush && let Some(ci) = app.ui.active_mask_component {
        params["component"] = json!(ci);
    }
    params
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
        (MaskShape::Radial { .. }, 6) => "feather",
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
        let map = &map.for_mask(m.on_photo);
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
        let _ = app.run("develop.beginInteraction", json!({"label": "Edit Mask"}));
        // (read after it begins: an old mask is on the photo from then on, in its coordinates)
        let (shape, map) = match (current_shape(app, mid, ci), current_on_photo(app, mid)) {
            (Some(s), Some(on)) => (s, map.for_mask(on)),
            _ => (shape.clone(), map.clone()),
        };
        let at = map.norm(q);
        let to = Point::new(at.x + dx, at.y + dy);
        let new_shape = moved_shape(&shape, handle, Point::new(dx, dy), to, &map, false);
        let _ = app.run("mask.update", json!({"id": mid, "component": ci, "shape": new_shape}));
        let _ = app.run("develop.endInteraction", json!({}));
    }
}

/// The shape of component `comp` of mask `mask` of the active photo as the session has it now.
fn current_shape(app: &LccApp, mask: u32, comp: usize) -> Option<MaskShape> {
    let d = app.session.develop_of(app.session.active()?)?;
    d.masks.iter().find(|m| m.id == mask)?.components.get(comp).map(|c| c.shape.clone())
}

/// Whether mask `mask` of the active photo is on the photo now (`Mask::on_photo`).
fn current_on_photo(app: &LccApp, mask: u32) -> Option<bool> {
    let d = app.session.develop_of(app.session.active()?)?;
    d.masks.iter().find(|m| m.id == mask).map(|m| m.on_photo)
}

/// A linear gradient's guide through `at` (its start, middle or end): the line at right angles to
/// it, as screen points. In a mask's coordinates on the photo, a lens or perspective warp bends
/// and turns it on screen, so it is drawn through the warp in steps; else it is a straight line.
fn linear_guide(map: &CanvasMap, start: Point, end: Point, at: Point) -> Vec<Vec<Pos2>> {
    let (a, b) = (map.screen(start), map.screen(end));
    if !map.warped() {
        let dir = (b - a).normalized();
        let perp = vec2(-dir.y, dir.x) * 3000.0;
        let q = map.screen(at);
        return vec![vec![q - perp, q + perp]];
    }
    // in long-edge units the perpendicular is a right angle; it spans the whole photo
    let l = frame_long_norm(map);
    let d = ((end.x - start.x) / l.0, (end.y - start.y) / l.1);
    let n = d.0.hypot(d.1);
    if !(n.is_finite() && n > 1e-12) {
        return Vec::new();
    }
    let perp = (-d.1 / n * l.0, d.0 / n * l.1);
    // from where the line enters the photo to where it leaves it (with a margin), measured from
    // `at`, so a guide whose end lies off the photo still reaches it
    let pp = perp.0 * perp.0 + perp.1 * perp.1;
    let ts = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].map(|(x, y)| ((x - at.x) * perp.0 + (y - at.y) * perp.1) / pp);
    let (lo, hi) = ts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), t| (lo.min(*t), hi.max(*t)));
    let pad = 0.1 * (hi - lo);
    let (lo, hi) = (lo - pad, hi + pad);
    if !(lo.is_finite() && hi.is_finite() && hi > lo) {
        return Vec::new();
    }
    // 48 steps across about a photo's width, at most 256
    let steps = ((hi - lo) * 16.0).ceil().clamp(48.0, 256.0) as usize;
    split_jumps(guide_runs((0..=steps).map(|i| {
        let t = lo + (hi - lo) * i as f64 / steps as f64;
        map.try_screen(Point::new(at.x + perp.0 * t, at.y + perp.1 * t))
    })))
}

/// `runs` cut again where a step is far longer than the run's usual one: a strong warp can fold
/// the line, and a chord across the fold isn't part of it.
fn split_jumps(runs: Vec<Vec<Pos2>>) -> Vec<Vec<Pos2>> {
    let mut out = Vec::new();
    for run in runs {
        let mut steps: Vec<f32> = run.windows(2).filter_map(|w| Some(w.first()?.distance(*w.get(1)?))).collect();
        steps.sort_by(f32::total_cmp);
        let usual = steps.get(steps.len() / 2).copied().unwrap_or(0.0);
        let limit = (8.0 * usual).max(2.0);
        let mut cur: Vec<Pos2> = Vec::new();
        for q in run {
            if cur.last().is_some_and(|p: &Pos2| p.distance(q) > limit) {
                if cur.len() >= 2 {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
            }
            cur.push(q);
        }
        if cur.len() >= 2 {
            out.push(cur);
        }
    }
    out
}

/// The unbroken runs (two points or more) of a sampled line whose missing samples (`None`) cut it.
fn guide_runs(samples: impl IntoIterator<Item = Option<Pos2>>) -> Vec<Vec<Pos2>> {
    let mut out: Vec<Vec<Pos2>> = Vec::new();
    let mut cur: Vec<Pos2> = Vec::new();
    for s in samples {
        match s {
            Some(q) => cur.push(q),
            None => {
                if cur.len() >= 2 {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
            }
        }
    }
    if cur.len() >= 2 {
        out.push(cur);
    }
    out
}

/// Long-edge units → normalized units for x and y.
fn frame_long_norm(map: &CanvasMap) -> (f64, f64) {
    // derive from the mapping: 1 normalized unit in x and y in screen points (of the frame's
    // proportions, which a lens or perspective warp doesn't change)
    let map = map.for_mask(false);
    let o = map.screen(Point::new(0.0, 0.0));
    let x = map.screen(Point::new(1.0, 0.0)).distance(o) as f64;
    let y = map.screen(Point::new(0.0, 1.0)).distance(o) as f64;
    let l = x.max(y);
    (l / x.max(1e-9), l / y.max(1e-9))
}

/// Auto's condition for the Tool Overlay (Shift+H): the pointer is over the photo, or a pin is
/// being dragged.
fn pins_live(app: &LccApp, resp: &egui::Response, map: &CanvasMap) -> bool {
    let hover_pos = resp.hover_pos().or_else(|| resp.ctx.input(|i| i.pointer.hover_pos()));
    hover_pos.is_some_and(|q| map.rect.contains(q))
        || (app.ui.right == RightPanel::Remove && hover_pos.is_some_and(|q| app.right_panel_rect.is_some_and(|r| r.contains(q))))
        || matches!(app.gesture, Some(Gesture::SpotMove { .. } | Gesture::MaskHandle { .. }))
        // a handle moved with the keyboard stays shown while it has the focus
        || crate::canvas_keys::has_focus(&resp.ctx)
}

/// The spot whose painted area or source is under `h` (the last drawn first, as it is on top).
fn spot_at(d: &DevelopSettings, map: &CanvasMap, long: f64, h: Pos2) -> Option<usize> {
    d.spots.iter().enumerate().rev().find_map(|(i, s)| {
        let r = ((s.size * long) as f32).max(4.0);
        let src = s.source_offset.filter(|_| s.mode != lcc_develop::SpotMode::Fill);
        let inside = s.points.iter().any(|q| map.screen(*q).distance(h) <= r)
            || src.is_some_and(|o| s.points.iter().any(|q| map.screen(Point::new(q.x + o.x, q.y + o.y)).distance(h) <= r));
        inside.then_some(i)
    })
}

fn pin(p: &egui::Painter, c: Pos2, sel: bool) {
    p.circle_filled(c, 6.0, if sel { Color32::from_rgb(1, 101, 221) } else { Color32::from_gray(200) });
    p.circle_stroke(c, 6.0, Stroke::new(1.5, Color32::WHITE));
}

/// A spot's pin; `tint` replaces its background (Remove Dust's spots).
fn spot_pin(p: &egui::Painter, c: Pos2, sel: bool, mode: lcc_develop::SpotMode, tint: Option<Color32>) {
    let pin_r = 9.0;
    let bg = tint.unwrap_or(if sel { Color32::from_rgb(1, 101, 221) } else { Color32::from_rgba_unmultiplied(35, 35, 38, 225) });
    p.circle_filled(c, pin_r, bg);
    p.circle_stroke(c, pin_r, Stroke::new(1.5, Color32::WHITE));
    let icon_rect = Rect::from_center_size(c, vec2(11.0, 11.0));
    let icon = match mode {
        lcc_develop::SpotMode::Heal | lcc_develop::SpotMode::Flat => crate::icons::Icon::Bandage,
        lcc_develop::SpotMode::Clone => crate::icons::Icon::Stamp,
        lcc_develop::SpotMode::Remove => crate::icons::Icon::Eraser,
        lcc_develop::SpotMode::Fill | lcc_develop::SpotMode::Generate => crate::icons::Icon::Sparkle,
    };
    crate::icons::paint(p, icon_rect, icon, Color32::WHITE);
}

pub(crate) fn spot_context_menu(app: &mut LccApp, ui: &mut egui::Ui, spot_idx: usize) {
    let cur_mode = app.session.active().and_then(|id| app.session.develop_of(id)).and_then(|d| d.spots.get(spot_idx).map(|s| s.mode));

    for (label, mode, mode_str) in [
        ("Remove", lcc_develop::SpotMode::Remove, "remove"),
        ("Content-Aware", lcc_develop::SpotMode::Fill, "fill"),
        ("Heal", lcc_develop::SpotMode::Heal, "heal"),
        ("Clone", lcc_develop::SpotMode::Clone, "clone"),
    ] {
        let is_cur = cur_mode == Some(mode);
        let text = if is_cur { format!("✓ {}", crate::i18n::tr(label)) } else { format!("   {}", crate::i18n::tr(label)) };
        if ui.button(text).clicked() {
            let _ = app.run("spot.update", json!({"index": spot_idx, "mode": mode_str}));
            ui.close();
        }
    }
    ui.separator();
    if ui.button(crate::i18n::tr("Refresh Source (/)")).clicked() {
        let _ = app.run("spot.refreshSource", json!({"index": spot_idx}));
        ui.close();
    }
    if ui.button(crate::i18n::tr("Delete (⌫)")).clicked() {
        let _ = app.run("spot.delete", json!({"index": spot_idx}));
        ui.close();
    }
    if ui.button(crate::i18n::tr("Reset")).clicked() {
        let d = lcc_develop::Spot::default();
        app.ui.remove_size = d.size as f32;
        app.ui.remove_feather = d.feather as f32;
        app.ui.remove_opacity = d.opacity as f32;
        let _ = app.run(
            "spot.update",
            json!({
                "index": spot_idx,
                "size": d.size,
                "feather": d.feather,
                "opacity": d.opacity,
            }),
        );
        ui.close();
    }
}

// ------------------------------------------------------------------------ remove

/// Distance from point `p` to line segment `(a, b)`.
fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let vx = b.x - a.x;
    let vy = b.y - a.y;
    let l2 = vx * vx + vy * vy;
    if l2 < 1e-4 {
        return p.distance(a);
    }
    let t = (((p.x - a.x) * vx + (p.y - a.y) * vy) / l2).clamp(0.0, 1.0);
    let q = Pos2::new(a.x + t * vx, a.y + t * vy);
    p.distance(q)
}

/// Smoothstep function matching pipeline/spots.rs.
#[inline]
fn smooth_step(e0: f32, e1: f32, x: f32) -> f32 {
    if e0 >= e1 {
        return if x < e1 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Extracts the 0.5 alpha isoline (unified outer perimeter) from an 8-bit alpha buffer
/// generated by rasterizing a brush stroke with the given radius and feather.
fn extract_stroke_contour(pts: &[Pos2], r: f32, feather: f64) -> Vec<Vec<Pos2>> {
    if pts.is_empty() || r <= 0.5 || !r.is_finite() {
        return Vec::new();
    }
    let feather = if feather.is_nan() { 0.5 } else { (feather as f32 / 100.0).clamp(0.0, 1.0) };
    let inner = r * (1.0 - feather * 0.8);
    let mid_r = (inner + r) * 0.5;

    // Single point: direct circle polygon
    if pts.len() == 1 {
        let Some(&c) = pts.first() else { return Vec::new() };
        if !c.x.is_finite() || !c.y.is_finite() {
            return Vec::new();
        }
        let n = 48;
        let mut loop_pts = Vec::with_capacity(n);
        for i in 0..n {
            let a = (i as f32 / n as f32) * std::f32::consts::TAU;
            loop_pts.push(Pos2::new(c.x + a.cos() * mid_r, c.y + a.sin() * mid_r));
        }
        return vec![loop_pts];
    }

    // Bounding box of the stroke with padding for the radius + margin
    let mut min_x = f32::MAX;
    let mut max_x = f32::MIN;
    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;
    for p in pts {
        if !p.x.is_finite() || !p.y.is_finite() {
            return Vec::new();
        }
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_y = min_y.min(p.y);
        max_y = max_y.max(p.y);
    }
    let pad = (r + 6.0).min(5000.0);
    min_x -= pad;
    max_x += pad;
    min_y -= pad;
    max_y += pad;

    let w = (max_x - min_x).max(1.0);
    let h = (max_y - min_y).max(1.0);
    if w > 10_000.0 || h > 10_000.0 || !w.is_finite() || !h.is_finite() {
        return Vec::new();
    }
    let step = (w.max(h) / 200.0).clamp(1.5, 4.0);
    let cols = (((max_x - min_x) / step).ceil() as usize + 1).min(350);
    let rows = (((max_y - min_y) / step).ceil() as usize + 1).min(350);
    if cols < 2 || rows < 2 {
        return Vec::new();
    }

    // Offscreen 8-bit alpha buffer
    let mut buf = vec![0u8; cols * rows];

    // Rasterize each line segment into the buffer (boolean union / max blend)
    for w in pts.windows(2) {
        let (Some(&a), Some(&b)) = (w.first(), w.get(1)) else { continue };
        let seg_min_x = a.x.min(b.x) - r;
        let seg_max_x = a.x.max(b.x) + r;
        let seg_min_y = a.y.min(b.y) - r;
        let seg_max_y = a.y.max(b.y) + r;

        let c0 = (((seg_min_x - min_x) / step).floor().max(0.0) as usize).min(cols);
        let c1 = (((seg_max_x - min_x) / step).ceil() as usize + 1).min(cols);
        let r0 = (((seg_min_y - min_y) / step).floor().max(0.0) as usize).min(rows);
        let r1 = (((seg_max_y - min_y) / step).ceil() as usize + 1).min(rows);

        for row in r0..r1 {
            let py = min_y + row as f32 * step;
            for col in c0..c1 {
                let px = min_x + col as f32 * step;
                let d = dist_to_segment(Pos2::new(px, py), a, b);
                if d < r {
                    let a_val = ((1.0 - smooth_step(inner, r, d)) * 255.0).clamp(0.0, 255.0) as u8;
                    let idx = row * cols + col;
                    if let Some(cell) = buf.get_mut(idx)
                        && a_val > *cell
                    {
                        *cell = a_val;
                    }
                }
            }
        }
    }

    // Marching squares to extract 0.5 alpha contour line (isoline at threshold 128)
    let thresh: u8 = 128;
    let grid_w = cols - 1;
    let grid_h = rows - 1;
    let mut grid_codes = vec![0u8; grid_w * grid_h];

    for row in 0..grid_h {
        let r_top = row * cols;
        let r_bot = (row + 1) * cols;
        for c in 0..grid_w {
            let v0 = buf.get(r_top + c).copied().unwrap_or(0) >= thresh;
            let v1 = buf.get(r_top + c + 1).copied().unwrap_or(0) >= thresh;
            let v2 = buf.get(r_bot + c + 1).copied().unwrap_or(0) >= thresh;
            let v3 = buf.get(r_bot + c).copied().unwrap_or(0) >= thresh;
            let code = ((v0 as u8) << 3) | ((v1 as u8) << 2) | ((v2 as u8) << 1) | (v3 as u8);
            if let Some(slot) = grid_codes.get_mut(row * grid_w + c) {
                *slot = code;
            }
        }
    }

    // Helper to compute sub-pixel edge crossing point
    let edge_point = |c: usize, row: usize, edge_idx: u8| -> Pos2 {
        let x0 = min_x + c as f32 * step;
        let y0 = min_y + row as f32 * step;
        let r_top = row * cols;
        let r_bot = (row + 1) * cols;
        let v0 = buf.get(r_top + c).copied().unwrap_or(0) as f32;
        let v1 = buf.get(r_top + c + 1).copied().unwrap_or(0) as f32;
        let v2 = buf.get(r_bot + c + 1).copied().unwrap_or(0) as f32;
        let v3 = buf.get(r_bot + c).copied().unwrap_or(0) as f32;
        let th = thresh as f32;

        match edge_idx {
            0 => {
                let t = if (v1 - v0).abs() > 1e-4 { (th - v0) / (v1 - v0) } else { 0.5 };
                Pos2::new(x0 + t.clamp(0.0, 1.0) * step, y0)
            }
            1 => {
                let t = if (v2 - v1).abs() > 1e-4 { (th - v1) / (v2 - v1) } else { 0.5 };
                Pos2::new(x0 + step, y0 + t.clamp(0.0, 1.0) * step)
            }
            2 => {
                let t = if (v2 - v3).abs() > 1e-4 { (th - v3) / (v2 - v3) } else { 0.5 };
                Pos2::new(x0 + t.clamp(0.0, 1.0) * step, y0 + step)
            }
            _ => {
                let t = if (v3 - v0).abs() > 1e-4 { (th - v0) / (v3 - v0) } else { 0.5 };
                Pos2::new(x0, y0 + t.clamp(0.0, 1.0) * step)
            }
        }
    };

    let get_segments = |code: u8| -> &[(u8, u8)] {
        match code {
            1 => &[(3, 2)],
            2 => &[(2, 1)],
            3 => &[(3, 1)],
            4 => &[(1, 0)],
            5 => &[(3, 0), (1, 2)],
            6 => &[(2, 0)],
            7 => &[(3, 0)],
            8 => &[(0, 3)],
            9 => &[(0, 2)],
            10 => &[(0, 3), (2, 1)],
            11 => &[(0, 1)],
            12 => &[(1, 3)],
            13 => &[(1, 2)],
            14 => &[(2, 3)],
            _ => &[],
        }
    };

    let total_cells = grid_w * grid_h;
    let mut visited = vec![false; total_cells.saturating_mul(2)];
    let mut loops = Vec::new();

    for row in 0..grid_h {
        for c in 0..grid_w {
            let code = grid_codes.get(row * grid_w + c).copied().unwrap_or(0);
            let segs = get_segments(code);
            for (seg_idx, &(in_e, out_e)) in segs.iter().enumerate() {
                let v_idx = (row * grid_w + c) * 2 + seg_idx;
                if visited.get(v_idx).copied().unwrap_or(false) {
                    continue;
                }
                if let Some(v) = visited.get_mut(v_idx) {
                    *v = true;
                }

                let mut loop_pts = vec![edge_point(c, row, in_e), edge_point(c, row, out_e)];
                let (mut cur_c, mut cur_r, mut cur_out) = (c, row, out_e);

                for _ in 0..5000 {
                    let (next_c, next_r, enter_e) = match cur_out {
                        0 if cur_r > 0 => (cur_c, cur_r - 1, 2),
                        1 if cur_c + 1 < grid_w => (cur_c + 1, cur_r, 3),
                        2 if cur_r + 1 < grid_h => (cur_c, cur_r + 1, 0),
                        3 if cur_c > 0 => (cur_c - 1, cur_r, 1),
                        _ => break,
                    };
                    let n_code = grid_codes.get(next_r * grid_w + next_c).copied().unwrap_or(0);
                    let n_segs = get_segments(n_code);
                    let found = n_segs.iter().enumerate().find(|(_, seg)| seg.0 == enter_e);
                    let Some((n_s_idx, seg)) = found else { break };
                    let n_out = seg.1;

                    let n_v_idx = (next_r * grid_w + next_c) * 2 + n_s_idx;
                    if visited.get(n_v_idx).copied().unwrap_or(false) {
                        break;
                    }
                    if let Some(v) = visited.get_mut(n_v_idx) {
                        *v = true;
                    }
                    loop_pts.push(edge_point(next_c, next_r, n_out));
                    cur_c = next_c;
                    cur_r = next_r;
                    cur_out = n_out;
                }

                if loop_pts.len() >= 3 {
                    loops.push(loop_pts);
                }
            }
        }
    }

    loops
}

/// Renders a unified outer perimeter with a 1px solid white outline and a 1px subtle dark drop-shadow/halo.
fn draw_unified_outline(p: &egui::Painter, loops: &[Vec<Pos2>], offset: egui::Vec2, sel: bool) {
    draw_tinted_outline(p, loops, offset, sel, None);
}

/// [`draw_unified_outline`] in `tint` instead of white (Remove Dust's spots), drawn a little wider.
fn draw_tinted_outline(p: &egui::Painter, loops: &[Vec<Pos2>], offset: egui::Vec2, sel: bool, tint: Option<Color32>) {
    let halo_stroke = Stroke::new(2.5, if sel { Color32::from_black_alpha(150) } else { Color32::from_black_alpha(80) });
    let outline_stroke = match tint {
        Some(c) => Stroke::new(if sel { 2.0 } else { 1.5 }, c),
        None => Stroke::new(1.0, if sel { Color32::WHITE } else { Color32::from_white_alpha(130) }),
    };

    for loop_pts in loops {
        if loop_pts.len() < 3 {
            continue;
        }
        let pts: Vec<Pos2> = if offset == egui::Vec2::ZERO { loop_pts.clone() } else { loop_pts.iter().map(|pt| *pt + offset).collect() };
        p.add(egui::Shape::closed_line(pts.clone(), halo_stroke));
        p.add(egui::Shape::closed_line(pts, outline_stroke));
    }
}

/// The Remove tool on the photo: every spot's outline and pin (the selected one with its source),
/// click a pin to select its spot, drag a target or source to move it, paint elsewhere to add one.
/// The Tool Overlay mode (Shift+H) says which spots show; a hidden spot can't be grabbed, so a
/// click there adds a new one.
fn remove_overlay(app: &mut LccApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
    // sizes are in units of the uncropped photo's long edge, as the render reads them
    let long = f64::from(map.per_long());
    let active = app.session.active_spot.filter(|i| *i < d.spots.len());
    let (mode, live) = (app.ui.tool_overlay, pins_live(app, resp, map));
    // Auto also hides them while a new spot is painted
    let painting = mode == ToolOverlay::Auto && matches!(app.gesture, Some(Gesture::Spot { .. }));
    let mut p = ui.painter_at(app.canvas_rect.unwrap_or(map.rect));
    // a slider or dial at work: the spots' outlines and pins hide over the result
    if adjusting(app) {
        p.set_invisible();
    }
    let mut pins = p.clone();
    if painting {
        pins.set_invisible();
    }
    // (spot, is source, screen centre, radius) of everything that can be grabbed
    let mut grips: Vec<(usize, bool, Pos2, f32)> = Vec::new();
    // the spot under the pointer (its painted area or its source) shows its outline and both pins
    let hot =
        resp.hover_pos().filter(|_| !painting && matches!(mode, ToolOverlay::Auto | ToolOverlay::Always)).and_then(|h| spot_at(d, map, long, h));
    for (i, s) in d.spots.iter().enumerate() {
        let r = (s.size * long) as f32;
        let sel = Some(i) == active;
        let hot = hot == Some(i);
        if !mode.shows(sel, live) && !hot {
            continue;
        }
        let p = &pins;
        if s.points.is_empty() {
            continue;
        }
        let (sum_x, sum_y) = s.points.iter().fold((0.0, 0.0), |acc, pt| (acc.0 + pt.x, acc.1 + pt.y));
        let n_pts = s.points.len() as f64;
        let target_centroid = Point::new(sum_x / n_pts, sum_y / n_pts);
        let tq = map.screen(target_centroid);

        let screen_pts: Vec<Pos2> = s.points.iter().map(|q| map.screen(*q)).collect();

        // Cache contour per spot to avoid per-frame marching squares when geometry has not changed
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for pt in &screen_pts {
            pt.x.to_bits().hash(&mut hasher);
            pt.y.to_bits().hash(&mut hasher);
        }
        r.to_bits().hash(&mut hasher);
        s.feather.to_bits().hash(&mut hasher);
        let key_hash = hasher.finish();

        let cache_id = egui::Id::new(("spot-contour-cache", i));
        let loops: Vec<Vec<Pos2>> = ui.data_mut(|d| {
            if let Some((cached_hash, cached_loops)) = d.get_temp::<(u64, Vec<Vec<Pos2>>)>(cache_id)
                && cached_hash == key_hash
            {
                return cached_loops;
            }
            let fresh = extract_stroke_contour(&screen_pts, r, s.feather);
            d.insert_temp(cache_id, (key_hash, fresh.clone()));
            fresh
        });

        // Remove Dust's spots: amber (the camera's known dust) or yellow (found in this photo alone); with Visualize
        // Spots on they stand out more
        let tint = s.auto.map(|a| {
            let alpha = if sel || app.ui.visualize_spots { 255 } else { 190 };
            if a.confirmed { Color32::from_rgba_unmultiplied(255, 166, 0, alpha) } else { Color32::from_rgba_unmultiplied(255, 228, 92, alpha) }
        });
        draw_tinted_outline(p, &loops, egui::Vec2::ZERO, sel, tint);

        if s.mode != lcc_develop::SpotMode::Fill
            && let Some(o) = s.source_offset.filter(|_| sel || hot)
        {
            let sq = map.screen(Point::new(target_centroid.x + o.x, target_centroid.y + o.y));
            let offset = sq - tq;
            draw_unified_outline(p, &loops, offset, true);

            let diff = tq - sq;
            let dist = diff.length();
            if dist > 20.0 {
                let dir = diff / dist;
                let start = sq + dir * 10.0;
                let end = tq - dir * 14.0;
                let vec = end - start;
                p.arrow(start, vec, Stroke::new(2.5, Color32::from_black_alpha(150)));
                p.arrow(start, vec, Stroke::new(1.0, Color32::WHITE));
            }
            pin(p, sq, true);
            register(ui.ctx(), format!("spotSource:{i}"), Rect::from_center_size(sq, vec2(14.0, 14.0)));
            grips.push((i, true, sq, r));
            let src_rect = Rect::from_center_size(sq, vec2(20.0, 20.0));
            let src_resp = ui.interact(src_rect, ui.id().with(("spotSourceGrip", i)), egui::Sense::click());
            src_resp.context_menu(|ui| {
                spot_context_menu(app, ui, i);
            });
        }
        // Remove Dust's spots: an amber (known dust) or yellow (this photo alone) pin
        let dust = s.auto.filter(|_| !sel).map(|a| if a.confirmed { Color32::from_rgb(255, 166, 0) } else { Color32::from_rgb(240, 214, 80) });
        spot_pin(p, tq, sel, s.mode, dust);
        register(ui.ctx(), format!("spotPin:{i}"), Rect::from_center_size(tq, vec2(18.0, 18.0)));
        grips.push((i, false, tq, r));
        let pin_rect = Rect::from_center_size(tq, vec2(20.0, 20.0));
        let pin_resp = ui.interact(pin_rect, ui.id().with(("spotPinGrip", i)), egui::Sense::click());
        if pin_resp.clicked() && Some(i) != active {
            let _ = app.run("spot.select", json!({"index": i}));
        }
        pin_resp.context_menu(|ui| {
            if Some(i) != app.session.active_spot {
                let _ = app.run("spot.select", json!({"index": i}));
            }
            spot_context_menu(app, ui, i);
        });
    }
    if spot_keys(app, ui, map, d, &grips) {
        return;
    }
    let r = (app.ui.remove_size as f64 * long) as f32;
    // a target or source under `q`: the selected spot's first, then the nearest pin or stroke
    let hit = |q: Pos2| {
        let pin_hit = grips
            .iter()
            .filter(|g| g.2.distance(q) < g.3.max(12.0))
            .min_by(|a, b| {
                (Some(a.0) != active, a.2.distance(q)).partial_cmp(&(Some(b.0) != active, b.2.distance(q))).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied();
        if pin_hit.is_some() {
            return pin_hit;
        }
        for (i, s) in d.spots.iter().enumerate() {
            let sel = Some(i) == active;
            if !mode.shows(sel, live) {
                continue;
            }
            let r = (s.size * long) as f32;
            if s.mode != lcc_develop::SpotMode::Fill
                && let Some(o) = s.source_offset.filter(|_| sel)
            {
                let hit_src = s.points.iter().any(|pt| {
                    let sp = Point::new(pt.x + o.x, pt.y + o.y);
                    map.screen(sp).distance(q) < r
                });
                if hit_src {
                    let (sum_x, sum_y) = s.points.iter().fold((0.0, 0.0), |acc, pt| (acc.0 + pt.x, acc.1 + pt.y));
                    let n = s.points.len() as f64;
                    let sc = Point::new(sum_x / n + o.x, sum_y / n + o.y);
                    return Some((i, true, map.screen(sc), r));
                }
            }
            let hit_tgt = s.points.iter().any(|pt| map.screen(*pt).distance(q) < r);
            if hit_tgt {
                let (sum_x, sum_y) = s.points.iter().fold((0.0, 0.0), |acc, pt| (acc.0 + pt.x, acc.1 + pt.y));
                let n = s.points.len() as f64;
                let tc = Point::new(sum_x / n, sum_y / n);
                return Some((i, false, map.screen(tc), r));
            }
        }
        None
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
    if resp.secondary_clicked()
        && let Some((spot, ..)) = press.and_then(hit)
        && Some(spot) != active
    {
        let _ = app.run("spot.select", json!({"index": spot}));
    }
    if resp.drag_started()
        && let Some((spot, source, ..)) = press.and_then(hit)
    {
        if Some(spot) != active {
            let _ = app.run("spot.select", json!({"index": spot}));
        }
        // a spot that shows a generated fill stays where the fill was made (a nudge hid it, test note 5)
        if d.spots.get(spot).is_some_and(|s| s.shown_gen().is_some()) {
            app.toast(ui.ctx(), crate::i18n::tr("A generated fill stays where it was made. Paint a new area to fill somewhere else."));
            return;
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
        // the spot being painted: its outline only, so what it covers stays visible
        let screen_pts: Vec<Pos2> = points.iter().map(|q| map.screen(*q)).collect();
        let loops = extract_stroke_contour(&screen_pts, r, app.ui.remove_feather as f64);
        draw_unified_outline(&p, &loops, egui::Vec2::ZERO, true);
    }
    if (resp.drag_stopped() || resp.clicked())
        && let Some(Gesture::Spot { points }) = app.gesture.take()
    {
        let mode = match app.ui.tool.as_str() {
            "heal" => "heal",
            "clone" => "clone",
            "fill" => "fill",
            "generate" => "generate",
            _ => "remove",
        };
        let pts: Vec<[f64; 2]> = points.iter().map(|q| [q.x, q.y]).collect();
        // Remove Options ▸ Detect Objects: the stroke picks the object under it instead of being the area
        if app.ui.remove_detect_objects && matches!(mode, "remove" | "generate") {
            let mode = if mode == "generate" { "generate" } else { "fill" };
            if let Err(e) = app.run_typed("spot.detectObject", json!({"points": pts, "mode": mode})) {
                app.ai_error(ui.ctx(), e, None);
            }
            return;
        }
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
        let r = app.ui.remove_size * map.per_long();
        ui.painter().circle_stroke(q, r, Stroke::new(1.0, Color32::WHITE));
    }
    if keys.activated {
        let mode = match app.ui.tool.as_str() {
            "heal" => "heal",
            "clone" => "clone",
            "fill" => "fill",
            "generate" => "generate",
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
    let mut p = ui.painter().clone();
    if adjusting(app) {
        p.set_invisible();
    }
    for (i, e) in d.red_eye.iter().enumerate() {
        let c = map.screen(e.center);
        let r = vec2(e.rx as f32 * per_long, e.ry as f32 * per_long);
        let sel = i == app.ui.eye;
        let col = if sel { Color32::WHITE } else { Color32::from_white_alpha(150) };
        p.add(egui::Shape::ellipse_stroke(c, r, Stroke::new(if sel { 1.5 } else { 1.0 }, col)));
        p.line_segment([c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)], Stroke::new(1.0, col));
        p.line_segment([c - vec2(0.0, 4.0), c + vec2(0.0, 4.0)], Stroke::new(1.0, col));
    }
    if eye_keys(app, ui, map, d, per_long) {
        return;
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

/// Keyboard stand-ins for the Red Eye tool: each correction's centre (the arrows move it), its
/// right edge (left / right change its width) and bottom edge (up / down its height), one undo
/// step a press; focusing one selects it, so Delete removes it. A placement point that the arrows
/// move turns into a new correction of the default size on Return. True when one was added.
fn eye_keys(app: &mut LccApp, ui: &mut egui::Ui, map: &CanvasMap, d: &DevelopSettings, per_long: f32) -> bool {
    for (i, e) in d.red_eye.iter().enumerate() {
        let c = map.screen(e.center);
        let name = format!("{} {}", crate::i18n::tr(if e.pet { "Pet Eye" } else { "Red Eye" }), i + 1);
        let value = format!(
            "{}, {} {:.0}%, {} {:.0}%",
            crate::canvas_keys::position_text(e.center.x, e.center.y),
            crate::i18n::tr("width"),
            e.rx * 200.0,
            crate::i18n::tr("height"),
            e.ry * 200.0
        );
        let parts = [
            ("", format!("eye:{i}"), c),
            ("right edge", format!("eye:{i}:width"), c + vec2(e.rx as f32 * per_long, 0.0)),
            ("bottom edge", format!("eye:{i}:height"), c + vec2(0.0, e.ry as f32 * per_long)),
        ];
        for (k, (part, key, at)) in parts.into_iter().enumerate() {
            let label = if part.is_empty() { name.clone() } else { format!("{name} {}", crate::i18n::tr(part)) };
            let keys = crate::canvas_keys::handle(ui, &key, &label, value.clone(), at);
            if ui.memory(|m| m.has_focus(crate::canvas_keys::id(&key))) {
                app.ui.eye = i;
            }
            let Some((dx, dy)) = keys.delta else { continue };
            let params = match k {
                0 => json!({"index": i, "move": [dx, dy]}),
                1 => json!({"index": i, "rx": e.rx + dx}),
                _ => json!({"index": i, "ry": e.ry + dy}),
            };
            let _ = app.run("develop.beginInteraction", json!({"label": "Edit Red Eye"}));
            let _ = app.run("redeye.update", params);
            let _ = app.run("develop.endInteraction", json!({}));
        }
    }
    // where a new correction goes: the photo's centre at first, then wherever the arrows took it
    let key = egui::Id::new("lc-eye-placement");
    let place: (f64, f64) = ui.data(|m| m.get_temp(key)).unwrap_or((0.5, 0.5));
    let q = map.screen(Point::new(place.0, place.1));
    let keys = crate::canvas_keys::handle(
        ui,
        "eye:new",
        crate::i18n::tr("New red eye correction (Return adds it here)"),
        crate::canvas_keys::position_text(place.0, place.1),
        q,
    );
    if let Some((dx, dy)) = keys.delta {
        let moved = ((place.0 + dx).clamp(0.0, 1.0), (place.1 + dy).clamp(0.0, 1.0));
        ui.data_mut(|m| m.insert_temp(key, moved));
    }
    if keys.activated {
        let r = app.run("redeye.add", json!({"center": [place.0, place.1], "pet": app.ui.eye_pet}));
        if let Ok(v) = r {
            app.ui.eye = v["index"].as_u64().unwrap_or(0) as usize;
        }
        return true;
    }
    false
}

// ------------------------------------------------------------------------ filmstrip

/// A filmstrip cell's file-name label: names longer than 14 characters are cut to 13 and an
/// ellipsis. Counts characters, not bytes, so a CJK name is never cut inside a character (#266).
/// Height of the filmstrip's source line (Lightroom Classic's "Folder : 2025  69 photos / 1 selected / name").
const FILM_HEAD_H: f32 = 16.0;

/// The source line's parts: the source, then the count and selection, then the active photo's file
/// name (shown a shade brighter), or `None` when no photo is active.
pub(crate) fn film_source_parts(source: &str, count: &str, selected: usize, name: Option<&str>) -> (String, String, Option<String>) {
    let counts = format!("{count} / {}", crate::i18n::tr_format!("{n} selected", n = selected));
    (source.to_string(), counts, name.filter(|n| !n.is_empty()).map(|n| format!(" / {n}")))
}

/// `text` cut at a character boundary (with "…") so `width` of it fits in `max`; empty when not even "…" fits.
pub(crate) fn elide_end(text: &str, max: f32, width: impl Fn(&str) -> f32) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |n: usize| chars.iter().take(n).collect::<String>() + "…";
    // the longest prefix that fits with the ellipsis
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width(&cut(mid)) <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let out = cut(lo);
    if width(&out) <= max { out } else { String::new() }
}

/// Draws the source line in `head`: the source gives way first, then the file name, then the counts.
fn film_source_line(app: &mut LccApp, ui: &egui::Ui, head: Rect, shown: usize, location_filter: bool, t: &Tokens) {
    if head.height() < 8.0 || head.width() < 40.0 {
        return;
    }
    let filtering = location_filter || app.session.filter != Default::default();
    let count = super::chips::count_text(shown, app.session.source_total(), filtering);
    let name = app.session.active().and_then(|id| app.session.catalog.photo(id)).map(|p| p.file_name.clone());
    let source = crate::i18n::source_title(&app.session);
    let (source, counts, name) = film_source_parts(&source, &count, app.session.selection.ids.len(), name.as_deref());
    let font = t.font(11.0);
    let p = ui.painter();
    let w = |s: &str| p.layout_no_wrap(s.to_string(), font.clone(), t.text_dim).size().x;
    let (left, right) = (head.left() + 10.0, head.right() - 10.0);
    let gap = 24.0;
    let name_w = name.as_deref().map_or(0.0, w);
    let source_room = right - left - w(&counts) - name_w - gap;
    let source = if source_room > 30.0 { crate::widgets::elide_head(&source, source_room, w) } else { String::new() };
    let y = head.center().y;
    let mut x = left;
    if !source.is_empty() {
        x = p.text(pos2(x, y), Align2::LEFT_CENTER, &source, font.clone(), t.text_dim).right() + gap;
    }
    let counts = elide_end(&counts, (right - x).max(0.0), w);
    let cr = p.text(pos2(x, y), Align2::LEFT_CENTER, &counts, font.clone(), t.text_dim);
    register(ui.ctx(), "film:count", cr);
    if let Some(name) = name {
        let name = elide_end(&name, (right - cr.right()).max(0.0), w);
        if !name.is_empty() {
            let nr = p.text(pos2(cr.right(), y), Align2::LEFT_CENTER, &name, font, t.text_label);
            register(ui.ctx(), "film:fileName", nr);
        }
    }
}

/// The shot info strip's parts, most important first (the strip drops them from the end when it
/// is narrow): aperture, shutter, ISO, focal length, the lens, the picture's size in pixels as
/// shown (after the crop, oriented). Only what the photo's metadata already holds: nothing is
/// read from the file.
pub(crate) fn shot_info(m: &lcc_catalog::Meta, size: [usize; 2]) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(a) = m.aperture.filter(|a| a.is_finite() && *a > 0.0) {
        v.push(if a >= 10.0 { format!("f/{a:.0}") } else { format!("f/{a:.1}") });
    }
    let shutter = m.shutter.trim();
    if !shutter.is_empty() {
        // the camera notation (1/250, 1.3″) or whole seconds, each with its unit once
        v.push(if shutter.ends_with(['s', '″']) { shutter.to_string() } else { format!("{shutter} s") });
    }
    if let Some(i) = m.iso {
        v.push(format!("ISO {i}"));
    }
    if let Some(f) = m.focal_mm.filter(|f| f.is_finite() && *f > 0.0) {
        v.push(format!("{f:.0} mm"));
    }
    let lens = m.lens.trim();
    // the lens's focal range (OneSlips PERF-CULL-7), in brackets after the focal length shot: added to a lens name
    // that doesn't say it ("Sigma Art" → "Sigma Art (24–70 mm)"), or alone when the file names no lens
    let range = m.lens_range.and_then(|r| lens_range_text(lens, r));
    match (lens.is_empty(), range) {
        (false, Some(r)) => v.push(format!("{lens} {r}")),
        (false, None) => v.push(lens.to_string()),
        (true, Some(r)) => v.push(r),
        (true, None) => {}
    }
    if size[0] > 1 && size[1] > 1 {
        v.push(format!("{} × {}", size[0], size[1]));
    }
    v
}

/// "(24–70 mm)" or, for a prime, "(85 mm)": a lens's focal range for the shot info, unless `lens` already names it
/// (its digits are in the name) or the range is unusable.
fn lens_range_text(lens: &str, (a, b): (f32, f32)) -> Option<String> {
    if !(a.is_finite() && b.is_finite() && a > 0.0 && b >= a && b < 100_000.0) {
        return None;
    }
    let (short, long) = (format!("{a:.0}"), format!("{b:.0}"));
    let named = |n: &str| {
        // the number standing on its own in the name (`24-70mm`, `FE 85mm`), not inside a longer one
        lens.match_indices(n).any(|(i, _)| {
            let before = lens.get(..i).and_then(|s| s.chars().last());
            let after = lens.get(i + n.len()..).and_then(|s| s.chars().next());
            !before.is_some_and(|c| c.is_ascii_digit()) && !after.is_some_and(|c| c.is_ascii_digit())
        })
    };
    if !lens.is_empty() && named(&short) && named(&long) {
        return None;
    }
    Some(if short == long { format!("({short} mm)") } else { format!("({short}–{long} mm)") })
}

/// The shot info strip: one line under the photo, or a slim pill over the bottom edge of the part
/// of the photo on screen when there is no room below it (full screen, zoomed in), its parts
/// dropped from the end when it doesn't fit (widget `loupe:shotInfo`). `hide_over`: an on-photo
/// tool is in use, so the pill over the photo is left out (under the photo it stays: it covers
/// nothing).
fn shot_info_strip(ui: &egui::Ui, canvas: Rect, img: Rect, parts: &[String], hide_over: bool, clear_bottom: f32) {
    // the part of the photo on screen (zoomed in, the photo is far bigger than the canvas)
    let img = img.intersect(canvas);
    if parts.is_empty() || !img.is_positive() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(canvas);
    let font = t.font(11.5);
    let h = 18.0;
    let below = img.bottom() + 3.0 + h <= canvas.bottom();
    if !below && hide_over {
        return;
    }
    // `clear_bottom` is the height something floating over the canvas' bottom edge takes (the
    // Modern layout's dock): the strip sits above that rather than under it
    let top = (if below { img.bottom() + 3.0 } else { img.bottom() - h - 6.0 }).min(canvas.bottom() - clear_bottom - h);
    // on the photo it needs its own backing; under it, the page shows through
    let on_photo = top < img.bottom() - 1.0;
    let colour = if on_photo { Color32::from_gray(230) } else { t.text_dim };
    let max_w = (img.width() - 16.0).max(40.0);
    let width = |s: &str| p.layout_no_wrap(s.to_string(), font.clone(), colour).size().x;
    let mut shown: Vec<String> = parts.to_vec();
    // least important last: the size, then the lens give way first; the last part left is cut
    // short with an ellipsis rather than dropped
    while shown.len() > 1 && strip_width(&shown.iter().map(|s| width(s)).collect::<Vec<_>>()) > max_w {
        shown.pop();
    }
    if shown.len() == 1
        && let Some(only) = shown.first_mut()
    {
        let mut chars: Vec<char> = only.chars().collect();
        while width(only) > max_w && chars.len() > 1 {
            chars.pop();
            *only = chars.iter().collect::<String>().trim_end().to_string() + "…";
        }
    }
    if shown.is_empty() {
        return;
    }
    let galleys: Vec<_> = shown.into_iter().map(|s| p.layout_no_wrap(s, font.clone(), colour)).collect();
    let total = strip_width(&galleys.iter().map(|g| g.size().x).collect::<Vec<_>>());
    let r = Rect::from_center_size(pos2(img.center().x, top + h / 2.0), vec2(total + 2.0 * STRIP_PAD, h));
    if on_photo {
        p.rect_filled(r, 9.0, Color32::from_black_alpha(140));
    }
    let mut x = r.left() + STRIP_PAD;
    let n = galleys.len();
    for (i, g) in galleys.into_iter().enumerate() {
        let w = g.size().x;
        p.galley(pos2(x, r.center().y - g.size().y / 2.0), g, colour);
        x += w;
        if i + 1 < n {
            // a faint dot between the items, in the middle of a wide gap
            p.circle_filled(pos2(x + STRIP_GAP / 2.0, r.center().y), 1.3, colour.gamma_multiply(0.55));
            x += STRIP_GAP;
        }
    }
    register(ui.ctx(), "loupe:shotInfo", r);
}

/// Space between two items of the shot info strip (a dot sits in its middle), and around them.
const STRIP_GAP: f32 = 22.0;
const STRIP_PAD: f32 = 10.0;

/// Width of the shot info strip's items (their widths) with the gaps between them.
pub(crate) fn strip_width(widths: &[f32]) -> f32 {
    widths.iter().sum::<f32>() + STRIP_GAP * widths.len().saturating_sub(1) as f32
}

/// Width of a filmstrip cell for a strip `height` points high (the shape of its original
/// 120 × 142 cells).
pub(crate) fn film_cell_width(height: f32) -> f32 {
    let h = if height.is_finite() { height } else { crate::state::FILM_HEIGHT.default };
    (h * 120.0 / 142.0).round().max(60.0)
}

/// The thumbnail size (px) a filmstrip cell `cell_w` points wide asks for: what it shows at the
/// screen's density, from the thumbnail sizes the cache keeps (at most 512).
pub(crate) fn film_thumb_px(cell_w: f32, ppp: f32) -> usize {
    let px = cell_w * if ppp.is_finite() { ppp.clamp(1.0, 3.0) } else { 1.0 } * 2.0;
    (px.round() as usize).clamp(128, 512)
}

/// The filmstrip's scroll offset after its cells change from `old_w` to `new_w` points wide: the
/// photo at index `at` (the active one) stays where it was on screen; with none, the first photo
/// in view does.
pub(crate) fn film_rescroll(old_off: f32, old_w: f32, new_w: f32, at: Option<usize>) -> f32 {
    if !(old_off.is_finite() && old_w.is_finite() && new_w.is_finite()) || old_w <= 0.0 {
        return 0.0;
    }
    let off = match at {
        Some(i) => {
            let centre = (i as f32 + 0.5) * old_w;
            (i as f32 + 0.5) * new_w - (centre - old_off)
        }
        None => old_off * new_w / old_w,
    };
    off.max(0.0)
}

/// The filmstrip's top edge: dragged up or down, it resizes the strip (and its thumbnails) within
/// [`crate::state::FILM_HEIGHT`]; a double click puts it back. The height is kept with the UI
/// settings (widget `filmstrip:resize`; `film:` ids are the photos).
fn film_resize(app: &mut LccApp, ui: &mut egui::Ui, r: Rect) {
    let edge = Rect::from_min_size(r.min - vec2(0.0, 2.0), vec2(r.width(), 7.0));
    let resp = ui.interact(edge, egui::Id::new("filmstrip-resize"), Sense::click_and_drag());
    // a screen reader names the edge after what it resizes
    crate::access::named(&resp, egui::WidgetType::Other, crate::i18n::tr("Filmstrip"));
    register(ui.ctx(), "filmstrip:resize", edge);
    // (screen readers get its name)
    crate::widgets::announce(&resp, crate::i18n::tr("Resize the filmstrip"));
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    if resp.dragged() {
        let dy = resp.drag_delta().y;
        if dy.is_finite() && dy != 0.0 {
            app.ui.film_height = crate::state::FILM_HEIGHT.clamp(app.ui.film_height() - dy);
        }
    }
    if resp.double_clicked() {
        app.ui.film_height = crate::state::FILM_HEIGHT.default;
    }
}

fn film_label(name: &str) -> String {
    if name.chars().nth(14).is_some() { format!("{}…", name.chars().take(13).collect::<String>()) } else { name.to_string() }
}

pub(crate) fn filmstrip(app: &mut LccApp, ui: &mut egui::Ui, r: Rect) {
    crate::view_state::around(app, ui, r, |app, ui| filmstrip_strip(app, ui, r));
}

fn filmstrip_strip(app: &mut LccApp, ui: &mut egui::Ui, r: Rect) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.canvas);
    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width(), 4.0)), 0.0, Color32::from_gray(0x20));
    film_resize(app, ui, r);
    // the cells keep the shape they had at the strip's original height (120 wide at 142 high)
    let cell_w = film_cell_width(r.height());
    // in the Map module the strip shows the photos passing its Location Filter (fork #783)
    let map_filter = (app.ui.module == crate::module::ModuleId::Map && app.map.filter != lcc_engine::cmd::map::LocationFilter::All)
        .then(|| app.map.filter.clone());
    let ids = if map_filter.is_some() { app.map.film_ids(&mut app.session) } else { app.session.visible_cloned() };
    let active = app.session.selection.active;
    // Lightroom Classic's source line above the strip: source, count, selection, active file name
    let head = Rect::from_min_max(pos2(r.left(), r.top() + 4.0), pos2(r.right(), (r.top() + 4.0 + FILM_HEAD_H).min(r.bottom())));
    film_source_line(app, ui, head, ids.len(), map_filter.is_some(), &t);
    let r = Rect::from_min_max(pos2(r.left(), head.bottom()), r.max);
    let ppp = ui.ctx().pixels_per_point();
    let thumb_px = film_thumb_px(cell_w, ppp);
    if ids.is_empty() {
        let why = if map_filter.is_some() {
            "No photos match the Location Filter"
        } else if app.session.filter != Default::default() {
            "No photos match the filters (View → Clear Filters)"
        } else {
            "No photos"
        };
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
    // a bar that can be seen and grabbed, as in the grid, its handle always there
    super::grid::visible_scroll_bar(&mut child);
    // a resized strip keeps the active photo where it was on screen (#1082)
    let width_id = egui::Id::new("film-cell-width");
    let last_w = child.data(|d| d.get_temp::<f32>(width_id));
    child.data_mut(|d| d.insert_temp(width_id, cell_w));
    let rescroll = match (last_w, app.film_scroll) {
        (Some(old_w), Some(old_off)) if (old_w - cell_w).abs() > 0.01 && old_w > 0.0 => {
            let at = active.and_then(|a| ids.iter().position(|id| *id == a));
            Some(film_rescroll(old_off, old_w, cell_w, at))
        }
        _ => None,
    };
    let mut area = egui::ScrollArea::horizontal();
    if let Some(off) = rescroll {
        area = area.horizontal_scroll_offset(off);
    }
    area.id_salt("filmstrip").auto_shrink([false, false]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible).show_viewport(
        &mut child,
        |ui, vp| {
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
                // cells drawn ahead of the scroll (off view, under the side panels) take no clicks or
                // keyboard focus: Tab never lands on a thumbnail nobody can see
                let sense = if !local.intersects(vp) {
                    Sense::hover()
                } else if map || reorder {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                };
                let resp = ui.interact(cr, egui::Id::new(("film", id.0)), sense);
                // asked for first: a failure left by an earlier request is not this tile's verdict once a new one is on its way
                super::grid::request_thumb(app, *id, thumb_px, 8);
                if local.intersects(vp) {
                    super::grid::note_cell(app, *id);
                }
                let failed = app.renderer.thumb(*id).is_none() && app.renderer.thumb_failure(*id).is_some();
                crate::widgets::announce(&resp, film_spoken(app.session.catalog.photo(*id).map_or("", |p| p.file_name.as_str()), failed));
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
                let p = ui.painter();
                let label = app.session.catalog.photo(*id).and_then(|ph| ph.label);
                let selected = state != lcc_engine::SelectionState::NotSelected;
                // Compact: the grid's cell levels, the flag's glow and the marks in a strip under the picture
                let compact = app.compact();
                let flag = app.session.catalog.photo(*id).map(|ph| ph.flag).unwrap_or_default();
                let base = if compact {
                    super::grid::cell_fill(&t, state, flag, resp.hovered(), t.canvas)
                } else if selected {
                    t.cell_selected
                } else if resp.hovered() {
                    t.cell_selected.gamma_multiply(0.6)
                } else {
                    t.canvas
                };
                p.rect_filled(cr, 0.0, crate::theme::label_background(base, label, selected));
                if compact {
                    super::grid::flag_glow(p, &t, cr, flag);
                }
                let names = app.ui.settings.film_names;
                if let Some(ph) = app.session.catalog.photo(*id).filter(|_| names) {
                    let name = ph.file_name.rsplit_once('.').map(|(n, _)| n).unwrap_or(&ph.file_name);
                    p.text(pos2(cr.left() + 8.0, cr.top() + 10.0), Align2::LEFT_CENTER, film_label(name), t.font(10.0), t.text_dim);
                    p.text(pos2(cr.right() - 8.0, cr.top() + 10.0), Align2::RIGHT_CENTER, &ph.format, t.semibold(8.5), t.text_dim);
                }
                let under = compact && app.ui.settings.film_badges;
                let strip = if under { (cr.height() * 0.11).clamp(12.0, 16.0) } else { 0.0 };
                let img_area = Rect::from_min_max(cr.min + vec2(10.0, 22.0), cr.max - vec2(10.0, 8.0 + strip));
                if under && let Some(ph) = app.session.catalog.photo(*id) {
                    let bar =
                        Rect::from_min_max(pos2(cr.left() + 8.0, img_area.bottom() + 3.0), pos2(cr.right() - 8.0, img_area.bottom() + 3.0 + strip));
                    register(ui.ctx(), format!("filmmarks:{}", id.0), bar);
                    super::grid::paint_badges(p, &t, bar, ph, false, app.session.catalog.is_vignetting_reference(ph.id));
                }
                let thumb = app.renderer.thumb(*id);
                // the picture: a click outside it (on the frame) selects this photo alone
                let mut picture = img_area;
                if thumb.is_none() {
                    p.rect_filled(img_area, 0.0, t.skeleton);
                    if failed {
                        p.text(img_area.center(), Align2::CENTER_CENTER, "!", t.semibold(14.0), t.text);
                    } else {
                        super::grid::paint_loading_cue(p, &t, img_area, img_area, "film:badge:loading:", id.0);
                    }
                }
                if let Some(tex) = thumb {
                    let [tw, th] = tex.size;
                    let s = (img_area.width() / tw as f32).min(img_area.height() / th as f32);
                    let fr = Rect::from_center_size(img_area.center(), vec2(tw as f32 * s, th as f32 * s));
                    picture = fr;
                    p.image(tex.id(), fr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    super::grid::selection_ring(p, &t, fr, state);
                    if app.ui.settings.film_badges
                        && !under
                        && let Some(ph) = app.session.catalog.photo(*id)
                    {
                        film_badges(p, &t, fr, ph);
                    }
                    if let Some(st) = app.session.catalog.stack_of(*id) {
                        let collapsed = st.collapsed;
                        let text = if collapsed {
                            st.photos.len().to_string()
                        } else {
                            format!("{}/{}", st.position(*id).unwrap_or(0) + 1, st.photos.len())
                        };
                        let g = p.layout_no_wrap(text, t.semibold(9.5), Color32::WHITE);
                        let br = Rect::from_min_size(fr.min + vec2(3.0, 3.0), vec2(g.size().x + 22.0, 15.0));
                        // like the grid's badge: a click expands / collapses the stack
                        let badge = ui.interact(br, egui::Id::new(("film-stack-badge", id.0)), Sense::click()).on_hover_text(if collapsed {
                            "Stack (click to expand)"
                        } else {
                            "Stack (click to collapse)"
                        });
                        crate::widgets::announce(&badge, crate::i18n::tr(if collapsed { "Expand Stack" } else { "Collapse Stack" }));
                        register(ui.ctx(), format!("filmstack:{}", id.0), br);
                        let p = ui.painter();
                        let fill = if badge.hovered() { Color32::from_black_alpha(230) } else { Color32::from_black_alpha(170) };
                        p.rect_filled(br, 7.5, fill);
                        crate::icons::paint(
                            p,
                            Rect::from_min_size(br.min + vec2(4.0, 2.0), vec2(11.0, 11.0)),
                            crate::icons::Icon::Stack,
                            Color32::WHITE,
                        );
                        p.galley(pos2(br.min.x + 17.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
                        if badge.clicked() {
                            super::grid::toggle_stack(app, ui.ctx(), *id);
                        }
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
                    } else if app.ui.view == crate::state::ViewMode::Survey && !m.command && !m.shift {
                        // Survey: keep the surveyed group; a thumbnail outside it joins, one inside only becomes active
                        app.run("library.select", json!({"ids": [id.0], "mode": "add"}))
                    } else {
                        // on the picture: the active photo changes, the set stays; on the frame: this photo alone
                        let on_picture = resp.interact_pointer_pos().is_none_or(|at| picture.contains(at));
                        app.run("library.select", json!({"ids": [id.0], "mode": super::grid::click_mode(m, on_picture)}))
                    };
                    if compare && let Err(e) = r {
                        app.toast(ui.ctx(), e);
                    }
                }
                // fork #741: a double-clicked thumbnail centres the map on its pin and zooms in,
                // even when the pin was already on screen
                if map && resp.double_clicked() {
                    let _ = app.run("map.reveal", json!({"force": true}));
                }
                // the same photo actions as the grid and loupe (Restore / Delete Permanently in Recently Deleted)
                super::compare::activate_on_right_click(app, &resp, *id);
                crate::widgets::context_menu(&resp, |ui| super::grid::context_menu(app, ui, *id));
            }
        },
    );
}

/// What a screen reader says for a filmstrip thumbnail: the file, and why there is no picture
/// when the thumbnail failed (the painted `!` has no name of its own). The name is empty when the
/// catalog no longer has the photo (or its file name is blank); the reason then stands alone.
pub(crate) fn film_spoken(file_name: &str, failed: bool) -> String {
    match (file_name.is_empty(), failed) {
        (_, false) => file_name.to_string(),
        (true, true) => crate::i18n::tr("This photo can't be opened").to_string(),
        (false, true) => format!("{file_name}, {}", crate::i18n::tr("This photo can't be opened")),
    }
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
    x += crate::rating::paint_rating(p, pos2(x, y), ph.rating, crate::rating::RatingSize::Small, t.star) + 2.0;
    match ph.flag {
        Flag::Pick => paint(p, Rect::from_min_size(pos2(x, y - 5.0), vec2(10.0, 10.0)), Icon::FlagPickFilled, t.pick),
        Flag::Reject => paint(p, Rect::from_min_size(pos2(x, y - 5.0), vec2(10.0, 10.0)), Icon::FlagReject, t.reject),
        Flag::None => {}
    }
    if edited {
        paint(p, Rect::from_min_size(pos2(bar.right() - 13.0, y - 5.0), vec2(10.0, 10.0)), Icon::Sliders, t.text_label);
    }
}

#[cfg(test)]
mod preview_geometry_tests {
    use super::{fit_rect, fit_texture_rect, label_badge_rect};
    use crate::state::Zoom;
    use egui::{Rect, pos2, vec2};

    /// The "Before" label sits on a dark pill that wraps the text with padding on every side.
    #[test]
    fn before_label_badge_wraps_the_text() {
        let text = vec2(40.0, 14.0);
        let pane = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let photo = Rect::from_min_size(pos2(20.0, 30.0), vec2(400.0, 300.0));
        let bg = label_badge_rect(photo, pane, text, 0.0);
        assert_eq!(bg.min, pos2(28.0, 38.0));
        assert_eq!(bg.size(), vec2(56.0, 22.0));
        assert!(photo.contains_rect(bg));
    }

    /// Zoomed in, the photo's corner is off the canvas: the pill moves to the canvas' corner
    /// instead of leaving with it (issue #2267), on whichever axes the photo reaches past.
    #[test]
    fn before_label_badge_stays_in_the_pane_when_the_photo_reaches_past_it() {
        let text = vec2(40.0, 14.0);
        let pane = Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0));
        let both = label_badge_rect(Rect::from_min_size(pos2(-900.0, -700.0), vec2(3000.0, 2000.0)), pane, text, 0.0);
        assert_eq!(both.min, pos2(108.0, 58.0));
        assert!(pane.contains_rect(both));
        let left_only = label_badge_rect(Rect::from_min_size(pos2(-900.0, 120.0), vec2(3000.0, 400.0)), pane, text, 0.0);
        assert_eq!(left_only.min, pos2(108.0, 128.0));
        assert!(pane.contains_rect(left_only));
    }

    /// With a filter active, the pill in the canvas' corner has that corner: the label sits below
    /// it, whether the photo is past the pane or just under the pill. A photo lower down is left
    /// alone. A wide label near the pane's right edge stays inside the pane.
    #[test]
    fn before_label_badge_keeps_clear_of_the_filter_pill_and_the_right_edge() {
        let text = vec2(40.0, 14.0);
        let pane = Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0));
        let inset = super::FILTER_PILL_INSET + super::FILTER_PILL_HEIGHT;
        let past = label_badge_rect(Rect::from_min_size(pos2(-900.0, -700.0), vec2(3000.0, 2000.0)), pane, text, inset);
        assert_eq!(past.min, pos2(108.0, 50.0 + inset + 8.0));
        let under = label_badge_rect(Rect::from_min_size(pos2(200.0, 60.0), vec2(400.0, 300.0)), pane, text, inset);
        assert_eq!(under.min.y, 50.0 + inset + 8.0, "a photo whose corner is under the pill moves down too");
        let lower = label_badge_rect(Rect::from_min_size(pos2(200.0, 200.0), vec2(400.0, 300.0)), pane, text, inset);
        assert_eq!(lower.min, pos2(208.0, 208.0));
        let wide = label_badge_rect(Rect::from_min_size(pos2(860.0, 200.0), vec2(400.0, 300.0)), pane, text, 0.0);
        assert!(pane.contains_rect(wide), "{wide:?}");
        assert_eq!(wide.max.x, pane.right() - 8.0, "8 points from the pane's right edge, like the left inset");
    }

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
    /// A mask on the photo (`Mask::on_photo`) is placed through the frame's warp: a point taken
    /// from the screen lands back where it was, and on the photo it is not the corrected-frame
    /// point (the warp moves it).
    #[test]
    fn mask_coordinates_go_through_the_warp() {
        use super::{CanvasMap, DevelopSettings, Frame, Rect, pos2};
        let mut s = DevelopSettings::default();
        s.geometry.vertical = 40.0;
        let frame = Frame::new(600, 400, &s, true);
        let map = CanvasMap::new(&frame, Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(600.0, 400.0)));
        let on = map.for_mask(true);
        let q = pos2(150.0, 120.0);
        let (n_on, n_old) = (on.norm(q), map.norm(q));
        assert!(on.screen(n_on).distance(q) < 0.5, "round trip: {:?}", on.screen(n_on));
        assert!((n_on.x - n_old.x).hypot(n_on.y - n_old.y) > 0.005, "the warp moves it: {n_on:?} vs {n_old:?}");
        // an old mask (and a frame without a warp) maps as before
        assert_eq!(map.for_mask(false), map);
        let flat = CanvasMap::new(&Frame::new(600, 400, &DevelopSettings::default(), true), map.rect);
        assert_eq!(flat.for_mask(true).norm(q), flat.norm(q));
    }

    #[test]
    fn contour_extraction_single_point_stroke_and_degenerate_inputs() {
        use egui::Pos2;
        // 1. Single point generates a circle
        let single = super::extract_stroke_contour(&[Pos2::new(100.0, 100.0)], 20.0, 50.0);
        assert_eq!(single.len(), 1, "single point produces one closed loop");
        assert_eq!(single[0].len(), 48, "single point circle has 48 vertices");
        for p in &single[0] {
            let dist = p.distance(Pos2::new(100.0, 100.0));
            assert!((dist - 16.0).abs() < 1.0, "radius should match mid_r: dist = {dist}");
        }

        // 2. Stroke with multiple points produces contour loops
        let stroke_pts =
            vec![Pos2::new(100.0, 100.0), Pos2::new(120.0, 100.0), Pos2::new(140.0, 100.0), Pos2::new(160.0, 100.0), Pos2::new(180.0, 100.0)];
        let stroke = super::extract_stroke_contour(&stroke_pts, 15.0, 50.0);
        assert!(!stroke.is_empty(), "stroke produces at least one contour loop");
        assert!(stroke[0].len() >= 4, "contour loop has multiple vertices");

        // 3. Degenerate and hostile inputs do not panic
        assert!(super::extract_stroke_contour(&[], 20.0, 50.0).is_empty(), "empty points");
        assert!(super::extract_stroke_contour(&[Pos2::new(100.0, 100.0)], 0.0, 50.0).is_empty(), "zero radius");
        assert!(super::extract_stroke_contour(&[Pos2::new(100.0, 100.0)], -5.0, 50.0).is_empty(), "negative radius");
        assert!(super::extract_stroke_contour(&[Pos2::new(100.0, 100.0)], f32::NAN, 50.0).is_empty(), "NaN radius");
        assert!(super::extract_stroke_contour(&[Pos2::new(100.0, 100.0)], f32::INFINITY, 50.0).is_empty(), "inf radius");
        assert!(super::extract_stroke_contour(&[Pos2::new(f32::NAN, 100.0)], 20.0, 50.0).is_empty(), "NaN coordinate");
        assert!(super::extract_stroke_contour(&[Pos2::new(100.0, f32::INFINITY)], 20.0, 50.0).is_empty(), "inf coordinate");

        // Huge coordinates / huge radius
        assert!(super::extract_stroke_contour(&[Pos2::new(1e25, 1e25), Pos2::new(1e25 + 10.0, 1e25)], 20.0, 50.0).is_empty());
        assert!(super::extract_stroke_contour(&[Pos2::new(100.0, 100.0), Pos2::new(200.0, 100.0)], 1e20, 50.0).is_empty());

        // Extreme feather values
        let nan_feather = super::extract_stroke_contour(&[Pos2::new(100.0, 100.0)], 20.0, f64::NAN);
        assert_eq!(nan_feather.len(), 1);
        let huge_feather = super::extract_stroke_contour(&[Pos2::new(100.0, 100.0)], 20.0, 1e9);
        assert_eq!(huge_feather.len(), 1);
    }

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
        use super::{RENDERING_AFTER_SECS as WAIT, SLOW_RENDERING_AFTER_SECS as SLOW, rendering_overdue};
        let key = |k| [Some(k), None, None, None];
        let mut wait = None;
        assert!(!rendering_overdue(&mut wait, key(1), false, 0.0, WAIT), "nothing coming");
        // an edit after an idle spell: the wait starts now, not at the last frame drawn
        assert!(!rendering_overdue(&mut wait, key(1), true, 30.0, WAIT), "a render just asked for");
        assert!(!rendering_overdue(&mut wait, key(1), true, 30.0 + WAIT * 0.8, WAIT), "a quick render never shows it");
        assert!(rendering_overdue(&mut wait, key(1), true, 30.0 + WAIT * 1.2, WAIT), "nothing new for the wait");
        assert!(rendering_overdue(&mut wait, key(2), true, 31.0 + WAIT, WAIT), "once up, a slow draft doesn't blink it");
        assert!(!rendering_overdue(&mut wait, key(3), false, 32.0, WAIT), "done");
        // a drag whose drafts keep up: always something coming, a new draft every 0.1 s
        for i in 1..30u32 {
            assert!(!rendering_overdue(&mut wait, key(u64::from(i)), true, 40.0 + f64::from(i) * 0.1, WAIT), "draft {i}");
        }
        // with the message off the same logic waits for the longer time
        let mut wait = None;
        assert!(!rendering_overdue(&mut wait, key(1), true, 50.0, SLOW));
        assert!(!rendering_overdue(&mut wait, key(1), true, 50.0 + SLOW * 0.8, SLOW), "a render that is only a little slow stays quiet");
        assert!(rendering_overdue(&mut wait, key(1), true, 50.0 + SLOW * 1.2, SLOW), "one that stays slow says so");
    }

    /// The wait belongs to the photo on screen: a slow render's pill doesn't stay up for the next
    /// photo, whose render may well come in under the wait.
    #[test]
    fn the_rendering_wait_starts_afresh_for_the_next_photo() {
        use super::{PhotoId, RENDERING_AFTER_SECS as WAIT, rendering_overdue_for};
        let key = |k| [Some(k), None, None, None];
        let mut state = (PhotoId(1), None);
        assert!(!rendering_overdue_for(&mut state, PhotoId(1), key(1), true, 10.0, WAIT));
        assert!(rendering_overdue_for(&mut state, PhotoId(1), key(1), true, 10.0 + WAIT * 1.2, WAIT), "a slow render of the first photo");
        // the next photo's render is pending at once: its own wait just began
        assert!(
            !rendering_overdue_for(&mut state, PhotoId(2), key(1), true, 10.0 + WAIT * 1.3, WAIT),
            "the pill of the last photo does not carry over"
        );
        assert!(rendering_overdue_for(&mut state, PhotoId(2), key(1), true, 10.0 + WAIT * 2.6, WAIT), "its own wait can still run out");
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
    fn film_source_line_names_the_active_file_and_elides_on_characters() {
        let (_, counts, name) = super::film_source_parts("Folder : 2025", "69 photos", 1, Some("9J0A0107.CR3"));
        assert!(counts.contains("69 photos"));
        assert_eq!(name.as_deref(), Some(" / 9J0A0107.CR3"));
        assert_eq!(super::film_source_parts("x", "0 photos", 0, None).2, None);
        assert_eq!(super::film_source_parts("x", "0 photos", 0, Some("")).2, None);
        // one point per character: cut on characters (multi-byte names too), never wider than asked
        let w = |s: &str| s.chars().count() as f32;
        for text in ["写真写真写真.JPG", "IMG_0001.CR3", ""] {
            for max in [0.0, 0.5, 1.0, 3.0, 5.0, 100.0] {
                let cut = super::elide_end(text, max, w);
                assert!(w(&cut) <= max, "{text:?} at {max}: {cut:?}");
            }
        }
        assert_eq!(super::elide_end("写真写真.JPG", 4.0, w), "写真写…");
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

    #[test]
    fn hdr_view_job_key_includes_headroom() {
        // A headroom change must produce a different render key so the texture is re-uploaded (P2-4).
        let base = lcc_engine::media::RenderJob {
            drawn_view_cache: false,
            on_screen: false,
            cache_generation: 0,
            request_id: 1,
            source_key: None,
            photo: lcc_catalog::PhotoId(42),
            level: lcc_engine::media::SourceLevel::Full,
            source: lcc_engine::media::SourceRef::File {
                path: "/tmp/test.jpg".to_string(),
                max_edge: 100,
                loader: None,
                fallback: None,
                denoise: None,
                options: Default::default(),
                reduced: None,
            },
            origin: lcc_catalog::Source::File { path: "/tmp/test.jpg".to_string() },
            info: Default::default(),
            display_fit: None,
            hdr_texels: None,
            superseded: None,
            latest: None,
            kept: None,
            placement: None,
            settings: Default::default(),
            request: lcc_pipeline::RenderRequest {
                max_w: 100,
                max_h: 100,
                exact_size: false,
                quality: Default::default(),
                apply_crop: false,
                overlay: Default::default(),
                space: Default::default(),
                depth: Default::default(),
                proof: None,
                window: None,
                display: None,
                downscale: Default::default(),
                alpha: false,
            },
            key: 0,
            cache: None,
            view_cache: None,
            stages: None,
            display: None,
            slide: None,
            preview_cut: None,
        };
        let k1 = super::hdr_view_job(base.clone(), Some(2.0)).key;
        let k2 = super::hdr_view_job(base.clone(), Some(4.0)).key;
        let k3 = super::hdr_view_job(base.clone(), None).key;
        assert_ne!(k1, k2, "different headroom values must produce different keys");
        assert_ne!(k1, k3, "Some(headroom) must differ from None");
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

    #[test]
    fn trackpad_scroll_delta_reads_point_events_only() {
        let m = egui::Modifiers::NONE;
        assert_eq!(
            trackpad_scroll_delta(&[wheel(egui::MouseWheelUnit::Point, 40.0, m), wheel(egui::MouseWheelUnit::Point, -15.0, m)]),
            egui::vec2(0.0, 25.0)
        );
        assert_eq!(trackpad_scroll_delta(&[wheel(egui::MouseWheelUnit::Line, 1.0, m)]), egui::Vec2::ZERO);
        assert_eq!(trackpad_scroll_delta(&[wheel(egui::MouseWheelUnit::Point, 40.0, egui::Modifiers::CTRL)]), egui::Vec2::ZERO);
        assert_eq!(trackpad_scroll_delta(&[wheel(egui::MouseWheelUnit::Point, f32::NAN, m)]), egui::Vec2::ZERO);
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
mod snap_tests {
    use super::*;

    #[test]
    fn fit_bounds_do_not_follow_draft_or_final_texture_dimensions() {
        for ppp in [1.0, 1.25, 1.5, 2.0] {
            for native in [[3264, 2448], [6000, 4000], [3265, 2449], [2449, 3265]] {
                let area = Rect::from_min_size(pos2(15.3, 110.0), vec2(1299.4, 676.0));
                let image = fit_rect(area, native[0] as f32 / native[1] as f32, Zoom::Fit, native, ppp, (0.5, 0.5));
                for v in [image.min.x, image.min.y, image.width(), image.height()] {
                    assert!((v * ppp - (v * ppp).round()).abs() < 1e-3, "{image:?} at {ppp}");
                }
                for edge in [576, 928, 1024, 1600, 2560] {
                    let size = if native[0] >= native[1] {
                        [edge, (edge as f64 * native[1] as f64 / native[0] as f64).round() as usize]
                    } else {
                        [(edge as f64 * native[0] as f64 / native[1] as f64).round() as usize, edge]
                    };
                    assert_eq!(render_texture_rect(image, size), image, "texture {size:?}, {ppp}x");
                }
            }
        }
    }

    #[test]
    fn embedded_previews_keep_even_a_small_difference_in_crop() {
        let frame = Rect::from_min_size(pos2(215.0, 110.0), vec2(901.0, 676.0));
        let preview = fit_texture_rect(frame, [1000, 740]);
        assert_eq!(preview.center(), frame.center());
        assert!((preview.width() / preview.height() - 1000.0 / 740.0).abs() < 1e-6);
        assert!(frame.contains_rect(preview));
        assert_ne!(preview, frame);
    }

    #[test]
    fn four_by_three_fit_keeps_the_settled_pixel_origin_for_drafts() {
        let area = Rect::from_min_max(pos2(15.0, 110.0), pos2(1315.0, 786.0));
        let image = fit_rect(area, 4.0 / 3.0, Zoom::Fit, [3264, 2448], 1.0, (0.5, 0.5));
        assert_eq!(image, Rect::from_min_size(pos2(215.0, 110.0), vec2(901.0, 676.0)));
        for size in [[901, 676], [928, 696], [576, 432], [2560, 1920]] {
            assert_eq!(render_texture_rect(image, size), image);
        }
        assert_eq!(snap_fit_rect(image, f32::NAN), image);
    }
}

#[cfg(test)]
mod info_overlay_tests {
    use super::*;

    /// The info overlay's capture date shows a year or a month alone at its own precision, and a
    /// full timestamp in the same format the metadata panel shows it (#743).
    #[test]
    fn the_info_overlay_shows_a_year_or_month_alone_as_such() {
        let mut photo =
            lcc_catalog::Photo::new(lcc_catalog::PhotoId(1), lcc_catalog::Source::Demo { scene: 0 }, "scan.jpg", "JPEG", 3, 2, "2026-10-01T00:00:00");
        for (captured, shown) in [("1983-04", "April 1983"), ("1983", "1983"), ("2024-01-16T12:34:56", "January 16, 2024 at 12:34:56 PM")] {
            photo.captured = Some(captured.into());
            assert_eq!(info_line(crate::state::LoupeInfoField::CaptureDate, &photo).as_deref(), Some(shown));
            assert_eq!(
                info_line(crate::state::LoupeInfoField::CaptureDate, &photo).as_deref(),
                Some(crate::i18n::display_time(captured).as_str()),
                "the overlay shows the capture date as the metadata panel does"
            );
        }
    }

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
        let mut out = ctx.run_ui(raw, |ui| info_overlay(&app, ui.painter(), canvas, &photo, None));
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

    /// View Options' Show Briefly When Photo Changes: with the overlay off, the chosen Info shows
    /// for `BRIEF_INFO_SECS` after the loupe changes photo, then hides; an overlay that is on stays.
    #[test]
    fn show_briefly_shows_the_info_after_the_photo_changes() {
        use crate::state::{BRIEF_INFO_SECS, InfoOverlay};
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let photos: Vec<lcc_catalog::Photo> = app.session.catalog.photos().take(2).map(|p| (**p).clone()).collect();
        assert_eq!(photos.len(), 2, "the demo catalog has two photos");
        let ctx = egui::Context::default();
        // the loupe shows photo `i` at `time`: the lines the overlay paints then
        let at = |app: &mut LccApp, i: usize, time: f64| {
            let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, time, vec![]);
            let mut out = ctx.run_ui(raw, |ui| brief_info(app, ui.ctx(), photos[i].id));
            out.textures_delta.clear();
            painted_lines(app, &photos[i])
        };
        let name = |i: usize| photos[i].file_name.clone();
        assert!(at(&mut app, 0, 1.0).is_empty(), "off by default");
        app.ui.settings.loupe_info.briefly = InfoOverlay::Exposure;
        let info2 = at(&mut app, 1, 2.0);
        assert_eq!(info2.first(), Some(&name(1)), "Info 2 shows when the photo changes: {info2:?}");
        assert!(!at(&mut app, 1, 2.0 + BRIEF_INFO_SECS - 0.5).is_empty(), "still up");
        assert!(at(&mut app, 1, 2.0 + BRIEF_INFO_SECS + 0.1).is_empty(), "then hidden");
        assert_eq!(at(&mut app, 0, 10.0).first(), Some(&name(0)), "and shown again for the next photo");
        app.ui.settings.loupe_info.briefly = InfoOverlay::Basic;
        let info1 = at(&mut app, 1, 20.0);
        assert_eq!(info1.len(), 3, "Info 1's three lines: {info1:?}");
        assert_ne!(info1, info2, "Info 1, not Info 2");
        app.ui.info_overlay = InfoOverlay::Exposure;
        assert_eq!(at(&mut app, 1, 30.0), info2, "an overlay that is on shows its own Info, and stays");
    }

    /// Issue #266: when filters are active in the loupe, the filter pill and the info overlay
    /// box do not collide/superimpose; the info overlay renders cleanly below the filter pill.
    #[test]
    fn info_overlay_renders_below_filter_pill_when_filters_active() {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.ui.info_overlay = crate::state::InfoOverlay::Basic;
        app.session.filter.text = "demo".into();
        let photo = app.session.catalog.photos().next().map(|p| (**p).clone()).expect("the demo catalog has photos");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let canvas = Rect::from_min_size(pos2(52.0, 82.0), vec2(800.0, 500.0));
        let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| {
            let pill = filter_pill(&mut app, ui, canvas);
            assert!(pill.is_some(), "filter pill is shown when filters are active");
            info_overlay(&app, ui.painter(), canvas, &photo, pill);
        });
        out.textures_delta.clear();
        let widgets = crate::widgets::take_registry(&ctx);
        let pill_rect = widgets.iter().find(|(w, _)| w == "loupe:filters").map(|(_, r)| *r).expect("loupe:filters registered");
        let info_rect = widgets.iter().find(|(w, _)| w == "canvas:infoOverlay").map(|(_, r)| *r).expect("canvas:infoOverlay registered");
        assert!(info_rect.top() >= pill_rect.bottom(), "info overlay {info_rect:?} is below filter pill {pill_rect:?}");
        assert!(!info_rect.intersects(pill_rect), "info overlay does not overlap filter pill");
    }

    /// When `hide_filter_in_develop` is enabled in Settings ▸ Interface, the filter pill is
    /// hidden in Develop mode, but remains visible in Library mode.
    #[test]
    fn hide_filter_in_develop_setting_hides_pill_in_develop_only() {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.session.filter.text = "demo".into();
        app.ui.settings.hide_filter_in_develop = true;

        // In Develop mode with the setting enabled, the filter pill is suppressed
        app.ui.module = crate::module::ModuleId::Develop;
        let hide_in_develop = app.ui.settings.hide_filter_in_develop && app.ui.module == crate::module::ModuleId::Develop;
        assert!(hide_in_develop);

        // In Library mode, the filter pill is not suppressed
        app.ui.module = crate::module::ModuleId::Library;
        let hide_in_library = app.ui.settings.hide_filter_in_develop && app.ui.module == crate::module::ModuleId::Develop;
        assert!(!hide_in_library);

        // When the setting is disabled, the filter pill is shown in both modes
        app.ui.settings.hide_filter_in_develop = false;
        app.ui.module = crate::module::ModuleId::Develop;
        assert!(!(app.ui.settings.hide_filter_in_develop && app.ui.module == crate::module::ModuleId::Develop));
    }

    /// The lines the overlay paints, top to bottom (each once: the shadow copies left out).
    fn painted_lines(app: &LccApp, photo: &lcc_catalog::Photo) -> Vec<String> {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let canvas = Rect::from_min_size(pos2(52.0, 82.0), vec2(800.0, 500.0));
        let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| info_overlay(app, ui.painter(), canvas, photo, None));
        out.textures_delta.clear();
        let mut lines = Vec::new();
        for c in &out.shapes {
            if let egui::epaint::Shape::Text(t) = &c.shape
                && t.override_text_color.unwrap_or(t.fallback_color) == Color32::WHITE
            {
                lines.push(t.galley.job.text.clone());
            }
        }
        lines
    }

    /// View ▸ View Options: Info 1 and Info 2 each show the three lines chosen for them, in that
    /// order, without the lines set to None.
    #[test]
    fn the_info_overlay_shows_the_lines_chosen_in_view_options() {
        use crate::state::{InfoOverlay, LoupeInfoField as F};
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let photo = app.session.catalog.photos().next().map(|p| (**p).clone()).expect("the demo catalog has photos");
        let size = format!("{} × {}", photo.width, photo.height);
        let date = pretty_date(photo.captured.as_deref().unwrap_or(&photo.imported));
        app.ui.settings.loupe_info.info1 = [F::Dimensions, F::None, F::FileName];
        app.ui.settings.loupe_info.info2 = [F::CaptureDate, F::FileName, F::None];
        app.ui.info_overlay = InfoOverlay::Basic;
        assert_eq!(painted_lines(&app, &photo), vec![size.clone(), photo.file_name.clone()], "Info 1");
        app.ui.info_overlay = InfoOverlay::Exposure;
        assert_eq!(painted_lines(&app, &photo), vec![date, photo.file_name.clone()], "Info 2");
        app.ui.settings.loupe_info.info1 = [F::None; 3];
        app.ui.info_overlay = InfoOverlay::Basic;
        assert_eq!(painted_lines(&app, &photo), Vec::<String>::new(), "nothing chosen, nothing shown");
    }

    fn demo_app() -> (LccApp, lcc_catalog::Photo) {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.ui.info_overlay = crate::state::InfoOverlay::Basic;
        let photo = app.session.catalog.photos().next().map(|p| (**p).clone()).expect("the demo catalog has photos");
        (app, photo)
    }

    /// The overlay's text shapes and the rects it registered, painted on a canvas at the window's corner.
    fn paint_overlay(app: &LccApp, photo: &lcc_catalog::Photo) -> (Vec<egui::epaint::TextShape>, Vec<(String, Rect)>) {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let canvas = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0));
        let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        let mut out = ctx.run_ui(raw, |ui| info_overlay(app, ui.painter(), canvas, photo, None));
        out.textures_delta.clear();
        fn texts(shape: &egui::epaint::Shape, out: &mut Vec<egui::epaint::TextShape>) {
            match shape {
                egui::epaint::Shape::Text(t) => out.push(t.clone()),
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        out.shapes.iter().for_each(|c| texts(&c.shape, &mut all));
        (all, crate::widgets::take_registry(&ctx))
    }

    fn name_top(app: &LccApp, photo: &lcc_catalog::Photo) -> f32 {
        let (texts, _) = paint_overlay(app, photo);
        texts.iter().filter(|t| t.galley.job.text == photo.file_name).map(|t| t.pos.y).fold(f32::INFINITY, f32::min)
    }

    /// #2460: in the full-screen preview on macOS the window controls stay at the top-left, so the
    /// file name starts below them; in macOS full screen they are gone and it keeps the corner.
    #[test]
    fn the_file_name_clears_the_macos_window_controls_in_the_full_screen_preview() {
        let (mut app, photo) = demo_app();
        app.ui.fullscreen = true;
        assert_eq!(name_top(&app, &photo), 12.0, "no window controls over the photo");
        app.integrated_titlebar = true;
        let bar = Tokens::get(&egui::Context::default()).top_bar_h;
        assert!(name_top(&app, &photo) >= bar, "below the window controls");
        app.window_is_fullscreen = true;
        assert_eq!(name_top(&app, &photo), 12.0, "macOS full screen hides the window controls");
        app.window_is_fullscreen = false;
        app.ui.fullscreen = false;
        assert_eq!(name_top(&app, &photo), 12.0, "the loupe's canvas starts below the top bar");
    }

    /// #2460: a rated or flagged photo shows its stars and flag under the date and size.
    #[test]
    fn the_info_overlay_shows_the_rating_and_flag_under_the_date() {
        let (_, mut photo) = demo_app();
        let (app, _) = demo_app();
        photo.rating = 0;
        photo.flag = lcc_catalog::Flag::None;
        let badges =
            |photo: &lcc_catalog::Photo| paint_overlay(&app, photo).1.into_iter().find(|(id, _)| id == "canvas:infoOverlayBadges").map(|(_, r)| r);
        assert_eq!(badges(&photo), None, "nothing to show");
        photo.rating = 3;
        let stars = badges(&photo).expect("three stars");
        photo.flag = lcc_catalog::Flag::Pick;
        let flagged = badges(&photo).expect("three stars and a flag");
        assert!(flagged.width() > stars.width(), "the flag follows the stars: {stars:?} {flagged:?}");
        let (texts, _) = paint_overlay(&app, &photo);
        let date_bottom = texts.iter().map(|t| t.pos.y + t.galley.size().y).fold(0.0, f32::max);
        assert!(flagged.top() >= date_bottom, "under the date and size: {flagged:?}, text ends at {date_bottom}");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let raw = crate::headless::HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        let mut extent = 0.0;
        ctx.run_ui(raw, |ui| extent = info_overlay_extent(&app, ui.painter(), &photo)).textures_delta.clear();
        assert!(extent >= flagged.bottom(), "a label below the overlay clears the badges: {extent} < {flagged:?}");
        photo.rating = 0;
        photo.flag = lcc_catalog::Flag::Reject;
        assert!(badges(&photo).is_some(), "a rejected photo shows its flag alone");
    }
}

#[cfg(test)]
#[path = "detail_tools_tests.rs"]
mod tools_tests;

#[cfg(test)]
mod mask_guide_tests {
    use super::*;
    use lcc_develop::DevelopSettings;

    fn warped_map() -> CanvasMap {
        let mut d = DevelopSettings::default();
        d.geometry.vertical = 35.0;
        d.optics.distortion = 50.0;
        let frame = Frame::new(400, 300, &d, true);
        CanvasMap::new(&frame, Rect::from_min_size(pos2(100.0, 50.0), vec2(400.0, 300.0))).for_mask(true)
    }

    /// Brush and spot sizes are measured on the uncropped photo's long edge: on a photo cropped to
    /// half its width the cursor is as large as the stroke the render paints (upstream #3958).
    #[test]
    fn a_brush_cursor_on_a_cropped_photo_is_the_size_the_render_paints() {
        let size = 0.05f32;
        let painted = |frame: &Frame, out_w: usize| size * frame.px_per_long(out_w) as f32 * 2.0;
        let mut d = DevelopSettings::default();
        d.crop.geometry.rect = lcc_geom::Rect::new(0.25, 0.0, 0.75, 1.0);
        let cropped = Frame::new(400, 300, &d, true);
        let m = CanvasMap::new(&cropped, Rect::from_min_size(pos2(10.0, 20.0), vec2(400.0, 600.0)));
        let want = painted(&cropped, 200);
        assert!((size * m.per_long() - want).abs() < 1e-3, "cursor {} vs painted {want}", size * m.per_long());
        let full = Frame::new(400, 300, &DevelopSettings::default(), true);
        let m = CanvasMap::new(&full, Rect::from_min_size(pos2(10.0, 20.0), vec2(800.0, 600.0)));
        assert!((size * m.per_long() - painted(&full, 400)).abs() < 1e-3);
    }

    /// A linear gradient's guides on a warped photo follow its bands through the warp: every
    /// point drawn is on the band through the gradient's start or end (review of upstream #3550).
    #[test]
    fn a_linear_gradients_guides_follow_the_warp() {
        let m = warped_map();
        assert!(m.warped());
        let (start, end) = (Point::new(0.3, 0.3), Point::new(0.5, 0.6));
        // (long-edge units: the frame is 400 x 300)
        let (sx, sy) = (1.0, 0.75);
        let dd = ((end.x - start.x) * sx, (end.y - start.y) * sy);
        let t = |q: Point| ((q.x - start.x) * sx * dd.0 + (q.y - start.y) * sy * dd.1) / (dd.0 * dd.0 + dd.1 * dd.1);
        for (at, want) in [(start, 0.0), (end, 1.0)] {
            let guide: Vec<Pos2> = linear_guide(&m, start, end, at).concat();
            assert!(guide.len() > 2, "drawn in steps");
            let inside: Vec<&Pos2> = guide.iter().filter(|g| m.rect.expand(-2.0).contains(**g)).collect();
            assert!(inside.len() > 2, "the guide crosses the canvas: {}", inside.len());
            for g in inside {
                let got = t(m.norm(*g));
                assert!((got - want).abs() < 1e-3, "{g:?}: band {got}, want {want}");
            }
        }
    }

    /// A guide is cut where a sample can't go through the warp, so no line joins a warped point
    /// to an unwarped one (review of upstream #3550).
    #[test]
    fn a_guide_breaks_where_the_warp_cant_be_undone() {
        let (a, b, c) = (pos2(1.0, 1.0), pos2(2.0, 2.0), pos2(3.0, 3.0));
        let runs = guide_runs([Some(a), Some(b), None, Some(c), None, None, Some(a), Some(b)]);
        assert_eq!(runs, vec![vec![a, b], vec![a, b]], "single points are dropped, the rest split");
        assert!(guide_runs([None, Some(a), None]).is_empty());
        // a chord far longer than the usual step (across a fold) cuts the run too
        let line: Vec<Pos2> = (0..10).map(|i| pos2(i as f32, 0.0)).chain((0..10).map(|i| pos2(200.0 + i as f32, 0.0))).collect();
        let cut = split_jumps(vec![line]);
        assert_eq!(cut.len(), 2, "{cut:?}");
        assert!(cut.iter().all(|r| r.len() == 10));
    }

    /// A guide through an end far off the photo still crosses the canvas (review of upstream #3550).
    #[test]
    fn a_guide_through_an_end_off_the_photo_still_crosses_it() {
        let m = warped_map();
        let (start, end) = (Point::new(3.5, 0.4), Point::new(3.5, 0.6));
        for at in [start, end] {
            let guide: Vec<Pos2> = linear_guide(&m, start, end, at).concat();
            assert!(guide.iter().filter(|g| m.rect.expand(-2.0).contains(**g)).count() > 2, "{at:?}: the guide crosses the canvas");
        }
    }
}
