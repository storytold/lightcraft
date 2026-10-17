//! The histogram's tonal zones *and* its plot, as in Lightroom Classic: hovering a zone names it
//! and shows its value, dragging one sideways moves its Basic control, and a double-click puts that
//! control back to its default.
//!
//! The zone a press landed in stays locked for the whole drag, so the pointer may cross the others;
//! every step is a `develop.set` between `develop.beginInteraction` and `develop.endInteraction`, the
//! path the panel's sliders take: the draft render follows the drag live and the whole drag lands as
//! one undo step (`end_interaction`).
//!
//! The plot itself is drawn in the mode `view.histogram.mode` selected and at the scale
//! `view.histogram.scale` selected (see [`paint_plot`]).

use egui::epaint::{Mesh, Vertex};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Response, Shape, Stroke, pos2, vec2};
use lcc_catalog::PhotoId;
use lcc_develop::{ControlSpec, controls};
use lcc_raster::{Histogram, HistogramMode, HistogramScale, robust_peak, scope_rgb, scope_uv};
use serde_json::json;

use crate::LccApp;
use crate::panels::detail::Gesture;
use crate::theme::Tokens;
use crate::widgets::{register, shown_value};

/// One tonal zone across the histogram's plot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zone {
    /// The develop control the zone adjusts: an id of [`lcc_develop::controls`].
    pub control: &'static str,
    /// The zone's span of the plot's width, normalized 0 (left) to 1 (right).
    pub from: f32,
    pub to: f32,
}

/// Lightroom Classic's five histogram regions, left to right: Blacks, Shadows, Exposure, Highlights,
/// Whites. Each names the Basic panel's own control, so the zone has that control's range, default
/// and steps.
pub const ZONES: [Zone; 5] = [
    Zone { control: "light.blacks", from: 0.00, to: 0.15 },
    Zone { control: "light.shadows", from: 0.15, to: 0.35 },
    Zone { control: "light.exposure", from: 0.35, to: 0.65 },
    Zone { control: "light.highlights", from: 0.65, to: 0.85 },
    Zone { control: "light.whites", from: 0.85, to: 1.00 },
];

/// How far a sideways drag moves a control: crossing the whole plot sweeps the control's whole range,
/// as dragging a slider's thumb across its track does. Shift is a tenth of that, as on the sliders.
const SENSITIVITY: f64 = 1.0;
const FINE: f64 = 0.1;

impl Zone {
    /// The zone's control spec; `None` for an id the control table no longer has.
    pub fn spec(self) -> Option<&'static ControlSpec> {
        controls::find(self.control)
    }

    /// The zone's band in `plot`.
    pub fn rect(self, plot: Rect) -> Rect {
        let x = |f: f32| plot.left() + plot.width() * f;
        Rect::from_min_max(pos2(x(self.from), plot.top()), pos2(x(self.to), plot.bottom()))
    }
}

/// The zone covering `fx`, a position across the plot normalized to 0..1. `None` off the plot (NaN
/// too); 1.0 is the last zone's, whose right edge is the plot's.
pub fn zone_at(fx: f32) -> Option<Zone> {
    if !(0.0..=1.0).contains(&fx) {
        return None;
    }
    ZONES.iter().copied().find(|zone| fx >= zone.from && fx < zone.to).or_else(|| ZONES.last().copied())
}

/// The value `base` moved by a sideways drag of `dx` points over a plot `width` points wide. The
/// result is clamped to the control's range; a drag that can't be measured (no width, a hostile
/// delta) or a non-finite `base` leaves `base` alone.
pub fn dragged(spec: &ControlSpec, base: f64, dx: f32, width: f32, fine: bool) -> f64 {
    if !width.is_finite() || width <= 0.0 {
        return base;
    }
    let travel = f64::from(dx) / f64::from(width) * if fine { FINE } else { SENSITIVITY };
    let v = base + travel * (spec.max - spec.min);
    if v.is_finite() { v.clamp(spec.min, spec.max) } else { base }
}

/// `v` on the control's own steps, as the panel's sliders round and dispatch it. A non-finite `v`
/// gives the control's default, as [`ControlSpec::clamp`] does.
pub fn quantized(spec: &ControlSpec, v: f64) -> f64 {
    if !v.is_finite() {
        return spec.default;
    }
    let step = spec.step.max(1e-9);
    ((v / step).round() * step).clamp(spec.min, spec.max)
}

/// What the histogram is acting on this frame.
#[derive(Clone, Copy, Debug)]
pub struct Active {
    /// The zone under the pointer, or the one a drag locked.
    pub zone: Zone,
    /// The zone's control's live value.
    pub value: f64,
    /// A drag of the zone is in progress: its zone stays locked until the release.
    pub dragging: bool,
}

impl Active {
    /// The readout under the histogram: "Exposure: +0.45 EV", "Shadows: +25".
    pub fn readout(&self) -> Option<String> {
        let spec = self.zone.spec()?;
        let shown = shown_value(spec, self.value);
        let value = if spec.id == "light.exposure" { format!("{shown} EV") } else { shown };
        Some(format!("{}: {value}", crate::i18n::tr(spec.label)))
    }
}

/// The histogram's pointer input for this frame: the press locks its zone and opens the interaction,
/// the drag moves that control, a double-click resets it. Returns what the highlight and the readout
/// should show, `None` when the pointer is nowhere near a zone.
///
/// `plot` is the plot's rect: the strip under it (the readout and the EXIF line) is not a zone, and
/// neither are the clipping buttons in the plot's top corners, which take their own presses.
pub fn interact(app: &mut LccApp, ui: &egui::Ui, resp: &Response, plot: Rect, id: PhotoId) -> Option<Active> {
    if mode_of(app).is_scope() {
        if matches!(app.gesture, Some(Gesture::HistogramDrag { .. })) {
            finish(app);
        }
        return None;
    }
    let width = plot.width();
    let in_plot = |q: Pos2| plot.contains(q);
    let zone_of = |q: Pos2| in_plot(q).then(|| (q.x - plot.left()) / width).and_then(zone_at);
    // the zone the pointer is over, and the one a press (or a double-click) landed in: the press's
    // origin, so a drag is judged by where it started
    let hovered = resp.hover_pos().and_then(zone_of);
    let pressed = ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos()).and_then(zone_of);
    let was_dragging = matches!(app.gesture, Some(Gesture::HistogramDrag { .. }));
    // a drag cut short — the release went elsewhere, the panel closed — leaves the interaction open:
    // with no button held the gesture is over, as the crop tool's gestures decide too
    if was_dragging && !resp.drag_stopped() && !ui.input(|i| i.pointer.any_down()) {
        finish(app);
    }
    // 1. the press locks its zone: one interaction holds the whole drag as one undo step
    if resp.drag_started()
        && !was_dragging
        && let Some(zone) = pressed
        && let Some(spec) = zone.spec()
        && app.run("develop.beginInteraction", json!({"label": spec.label})).is_ok()
    {
        app.ui.dragging_control = Some(spec.id.to_string());
        app.gesture = Some(Gesture::HistogramDrag { control: zone.control, value: value_of(app, id, spec) });
    }
    // 2. the drag moves the locked zone's control, wherever the pointer has gone since
    if let Some(Gesture::HistogramDrag { control, value }) = app.gesture.clone()
        && let Some(spec) = controls::find(control)
    {
        if resp.dragged() || resp.drag_stopped() {
            // The frame a drag is first reported, take the whole travel from the press: egui only
            // starts reporting a drag once the pointer has left its click threshold, and those first
            // points are the user's movement too. Later frames add their own delta (a one-frame flick
            // reports the drag as started and stopped together: then this is its delta), so Shift —
            // a tenth — scales the travel it was held for, as the sliders' fine drag does. The
            // unrounded value is kept from frame to frame, so a slow drag adds up instead of rounding
            // back to the step it already has.
            let dx = if resp.drag_started() {
                ui.input(|i| i.pointer.total_drag_delta()).map_or(resp.drag_delta().x, |d| d.x)
            } else {
                resp.drag_delta().x
            };
            let next = dragged(spec, value, dx, width, ui.input(|i| i.modifiers.shift));
            app.gesture = Some(Gesture::HistogramDrag { control, value: next });
            let v = quantized(spec, next);
            if value_of(app, id, spec) != v {
                let _ = app.run("develop.set", json!({"control": control, "value": v}));
            }
        }
        if resp.drag_stopped() {
            finish(app);
        }
    }
    // 3. a double-click in a zone puts that control back to its default, in one undo step
    if resp.double_clicked()
        && let Some(zone) = pressed
        && let Some(spec) = zone.spec()
    {
        let _ = app.run("develop.beginInteraction", json!({"label": spec.label}));
        app.ui.dragging_control = Some(spec.id.to_string());
        let _ = app.run("develop.set", json!({"control": spec.id, "value": spec.default}));
        finish(app);
    }
    // the highlight and the readout follow the drag's locked zone, else the pointer's
    let zone = match app.gesture {
        Some(Gesture::HistogramDrag { control, .. }) => ZONES.iter().copied().find(|zone| zone.control == control),
        _ => hovered,
    }?;
    let spec = zone.spec()?;
    let dragging = matches!(app.gesture, Some(Gesture::HistogramDrag { .. }));
    if dragging || hovered.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    register(ui.ctx(), "histogram:zone", zone.rect(plot));
    Some(Active { zone, value: value_of(app, id, spec), dragging })
}

/// End the histogram drag: the interaction commits the whole drag as one undo step.
fn finish(app: &mut LccApp) {
    app.gesture = None;
    app.ui.dragging_control = None;
    let _ = app.run("develop.endInteraction", json!({}));
}

/// A control's value on the photo, or its default when the photo has no settings yet.
fn value_of(app: &LccApp, id: PhotoId, spec: &ControlSpec) -> f64 {
    controls::get(&app.session.develop_of(id).unwrap_or_default(), spec.id).unwrap_or(spec.default)
}

/// The band the active zone draws over the plot: a subtle translucent highlight, a little stronger
/// while the zone is being dragged. Painted behind the histogram's curves.
pub fn paint_zone(painter: &Painter, plot: Rect, active: &Active, tokens: &Tokens) {
    let tint = if active.dragging { tokens.pressed } else { tokens.hover };
    painter.rect_filled(active.zone.rect(plot), 0.0, tint.gamma_multiply(if active.dragging { 0.75 } else { 0.45 }));
}

// ------------------------------------------------------------------ the distribution

/// The histogram mode the settings name (as `view.histogram.mode` writes it). A value written by
/// another build — or hand-edited — reads as the default rather than failing.
pub fn mode_of(app: &LccApp) -> HistogramMode {
    HistogramMode::parse(&app.ui.histogram_mode).unwrap_or_default()
}

/// The histogram scale the settings name (as `view.histogram.scale` writes it).
pub fn scale_of(app: &LccApp) -> HistogramScale {
    HistogramScale::parse(&app.ui.histogram_scale).unwrap_or_default()
}

/// The red, green and blue a channel is drawn in.
pub const CHANNEL_COLORS: [Color32; 3] = [Color32::from_rgb(223, 57, 57), Color32::from_rgb(68, 176, 114), Color32::from_rgb(59, 111, 224)];

/// The five-bin box smoothing the panel draws its curves with: raw 256-bin histograms of a photo
/// are spiky, and a single-bin spike would draw as a one-pixel needle.
pub fn smooth(bins: &[u32]) -> Vec<f32> {
    (0..Histogram::BINS)
        .map(|i| {
            let from = i.saturating_sub(2);
            let to = (i + 3).min(Histogram::BINS);
            let sum: u32 = bins.get(from..to).map_or(0, |w| w.iter().copied().fold(0u32, u32::saturating_add));
            let n = to.saturating_sub(from).max(1);
            sum as f32 / n as f32
        })
        .collect()
}

/// The height every drawn bin is normalized against: the 99th percentile of the bins' counts, so a
/// single spike (a blown sky, a crushed black) does not flatten the rest of the curve.
fn peak(all: &[&[f32]]) -> f32 {
    let mut v: Vec<f32> = Vec::with_capacity(all.len() * Histogram::BINS);
    for b in all {
        v.extend_from_slice(b);
    }
    robust_peak(&v, 0.99)
}

/// Bin `i` of `bins` as a height in 0..1 against `peak`.
fn height(scale: HistogramScale, bins: &[f32], i: usize, peak: f32) -> f32 {
    scale.height(bins.get(i).copied().unwrap_or(0.0), peak)
}

/// The point the curve of bin `i` reaches in `track` (0 = the track's bottom, 1 = its top).
fn point(track: Rect, i: usize, v: f32) -> Pos2 {
    let x = track.left() + track.width() * i as f32 / (Histogram::BINS - 1) as f32;
    pos2(x, track.bottom() - track.height() * if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })
}

/// The horizontal span of bin `i` of a `width`-point-wide plot, a hair wider than its own column so
/// neighbouring columns meet without a seam.
fn column(left: f32, width: f32, i: usize) -> (f32, f32) {
    let w = width / Histogram::BINS as f32;
    let x0 = left + w * i as f32;
    let x1 = if i + 1 < Histogram::BINS { x0 + w + 0.4 } else { left + width };
    (x0, x1)
}

/// Add one filled rectangle to `mesh` (four vertices and two triangles: one draw call per plot).
fn push_quad(mesh: &mut Mesh, rect: Rect, color: Color32) {
    let k = u32::try_from(mesh.vertices.len()).unwrap_or(0);
    for pos in [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()] {
        mesh.vertices.push(Vertex { pos, uv: Pos2::ZERO, color });
    }
    mesh.indices.extend([k, k.saturating_add(1), k.saturating_add(2), k, k.saturating_add(2), k.saturating_add(3)]);
}

/// The three vertical bands of one column of the RGB-overlap histogram, bottom up: with the three
/// channel heights `h` the lowest band is where every channel reaches, the middle where the two
/// taller ones do and the top where only the tallest does. Returns the bands and how many there are
/// (0..=3; a column no channel reaches has none). `NaN` heights count as zero.
pub fn overlap_bands(h: [f32; 3]) -> ([(f32, f32, [bool; 3]); 3], usize) {
    // a height that isn't a number counts as zero, before the ordering, so a `NaN` channel cannot
    // claim a band
    let hs: [f32; 3] = h.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
    let mut order = [0usize, 1, 2];
    order.sort_by(|a, b| hs[*a].total_cmp(&hs[*b]));
    // the same heights, shortest first, with `order` saying which channel each came from
    let sorted: [f32; 3] = order.map(|k| hs[k]);
    let mut out = [(0.0f32, 0.0f32, [false; 3]); 3];
    let mut n = 0;
    for (k, (from, to)) in [(0usize, (0.0, sorted[0])), (1, (sorted[0], sorted[1])), (2, (sorted[1], sorted[2]))] {
        if to <= from {
            continue;
        }
        let mut parts = [false; 3];
        for j in order.iter().skip(k) {
            if let Some(slot) = parts.get_mut(*j) {
                *slot = true;
            }
        }
        if let Some(slot) = out.get_mut(n) {
            *slot = (from, to, parts);
            n += 1;
        }
    }
    (out, n)
}

/// The colour of an overlap band: neutral light grey where all three channels reach, the secondary
/// colours where two do, a channel's own colour where only that one does (so the plot adds up the
/// way the channels do).
pub fn blend_color(parts: [bool; 3]) -> Color32 {
    match parts {
        [true, true, true] => Color32::from_gray(235),
        [true, true, false] => Color32::from_rgb(226, 210, 74),
        [false, true, true] => Color32::from_rgb(74, 208, 214),
        [true, false, true] => Color32::from_rgb(214, 74, 206),
        [true, false, false] => CHANNEL_COLORS[0],
        [false, true, false] => CHANNEL_COLORS[1],
        [false, false, true] => CHANNEL_COLORS[2],
        [false, false, false] => Color32::TRANSPARENT,
    }
}

/// Add the trapezoid under one step of a curve to `mesh`: `a` and `b` are consecutive curve points
/// and `bottom` the floor they stand on.
fn push_step(mesh: &mut Mesh, a: Pos2, b: Pos2, bottom: f32, color: Color32) {
    if a.y >= bottom && b.y >= bottom {
        return;
    }
    let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
    if x1 <= x0 {
        return;
    }
    let k = u32::try_from(mesh.vertices.len()).unwrap_or(0);
    for pos in [pos2(x0, a.y), pos2(x1, b.y), pos2(x1, bottom), pos2(x0, bottom)] {
        mesh.vertices.push(Vertex { pos, uv: Pos2::ZERO, color });
    }
    mesh.indices.extend([k, k.saturating_add(1), k.saturating_add(2), k, k.saturating_add(2), k.saturating_add(3)]);
}

/// Fill the area under `bins` with `color`, inside `track`.
fn fill_curve(p: &Painter, track: Rect, bins: &[f32], scale: HistogramScale, peak: f32, color: Color32) {
    let mut mesh = Mesh::default();
    let mut prev: Option<Pos2> = None;
    for i in 0..Histogram::BINS {
        let top = point(track, i, height(scale, bins, i, peak));
        if let Some(a) = prev {
            push_step(&mut mesh, a, top, track.bottom(), color);
        }
        prev = Some(top);
    }
    p.add(mesh);
}

/// The hairline of `bins` itself, over its fill.
fn stroke_curve(p: &Painter, track: Rect, bins: &[f32], scale: HistogramScale, peak: f32, stroke: Stroke) {
    let pts: Vec<Pos2> = (0..Histogram::BINS).map(|i| point(track, i, height(scale, bins, i, peak))).collect();
    p.add(Shape::line(pts, stroke));
}

/// The six primary and secondary colour targets around the vectorscope graticule: label, pure
/// display-encoded RGB (which determines the target's angle and radius in `scope_uv`), and the
/// colour its ring and label are drawn in.
pub const SCOPE_TARGETS: [(&str, [u8; 3], Color32); 6] = [
    ("R", [255, 0, 0], CHANNEL_COLORS[0]),
    ("Mg", [255, 0, 255], Color32::from_rgb(214, 74, 206)),
    ("B", [0, 0, 255], CHANNEL_COLORS[2]),
    ("Cy", [0, 255, 255], Color32::from_rgb(74, 208, 214)),
    ("G", [0, 255, 0], CHANNEL_COLORS[1]),
    ("Yl", [255, 255, 0], Color32::from_rgb(226, 210, 74)),
];

/// The skin-tone ("I") line angle on the vectorscope graticule: 123° counter-clockwise from `+Cb`
/// (33° left of straight-up `+Cr`, between Red and Yellow).
pub const SKIN_TONE_ANGLE_RAD: f32 = 123.0 * std::f32::consts::PI / 180.0;

/// Centre and radius of the vectorscope disc inside `plot`.
pub fn scope_disc(plot: Rect) -> (Pos2, f32) {
    let radius = (plot.width().min(plot.height()) * 0.5 - 3.0).max(1.0);
    (plot.center(), radius)
}

/// Screen position in `plot`'s vectorscope disc for normalized chrominance `(u, v)` (`+u` right,
/// `+v` up).
pub fn scope_pos(center: Pos2, radius: f32, u: f32, v: f32) -> Pos2 {
    pos2(center.x + u * radius, center.y - v * radius)
}

/// Draw the polar chrominance vectorscope of `h` inside `plot` at `scale`.
fn paint_vectorscope(p: &Painter, plot: Rect, h: &Histogram, scale: HistogramScale) {
    let (center, radius) = scope_disc(plot);
    // 1. Graticule backing, concentric saturation rings (25%, 50%, 75%), crosshairs and skin-tone line
    p.circle_filled(center, radius, Color32::from_black_alpha(65));
    let ring_stroke = Stroke::new(1.0, Color32::from_gray(84).gamma_multiply(0.55));
    for r in [0.25, 0.50, 0.75] {
        p.circle_stroke(center, radius * r, ring_stroke);
    }
    let axis_stroke = Stroke::new(1.0, Color32::from_gray(92).gamma_multiply(0.65));
    p.line_segment([pos2(center.x - radius, center.y), pos2(center.x + radius, center.y)], axis_stroke);
    p.line_segment([pos2(center.x, center.y - radius), pos2(center.x, center.y + radius)], axis_stroke);
    let (su, sv) = (SKIN_TONE_ANGLE_RAD.cos(), SKIN_TONE_ANGLE_RAD.sin());
    p.line_segment([center, scope_pos(center, radius, su, sv)], Stroke::new(1.0, Color32::from_rgb(218, 162, 106).gamma_multiply(0.75)));

    // 2. 2D chrominance cloud: one quad per occupied cell, normalized against the 98th percentile
    // of non-empty cells so a neutral background spike does not hide subtle colour casts
    let non_zero: Vec<f32> = h.scope.iter().copied().filter(|&c| c > 0).map(|c| c as f32).collect();
    let peak = robust_peak(&non_zero, 0.98);
    let mid = Histogram::SCOPE_MID as f32;
    let half = (radius / mid).max(0.85) * 0.62;
    let mut mesh = Mesh::default();
    for by in 0..Histogram::SCOPE_BINS {
        for bx in 0..Histogram::SCOPE_BINS {
            let count = h.scope_at(bx, by);
            if count == 0 {
                continue;
            }
            let u = (bx as f32 - mid) / mid;
            let v = (mid - by as f32) / mid;
            if u * u + v * v > 1.02 {
                continue;
            }
            let d = scale.height(count as f32, peak);
            let [cr, cg, cb] = scope_rgb(u, v);
            let color = Color32::from_rgb(cr, cg, cb).gamma_multiply(0.28 + 0.72 * d);
            let pt = scope_pos(center, radius, u, v);
            push_quad(&mut mesh, Rect::from_center_size(pt, vec2(half * 2.0, half * 2.0)), color);
        }
    }
    p.add(mesh);

    // 3. Outer 100% ring and the six 75%-saturation primary/secondary target boxes and labels
    p.circle_stroke(center, radius, Stroke::new(1.0, Color32::from_gray(124)));
    for (label, rgb, color) in SCOPE_TARGETS {
        let (u, v) = scope_uv(rgb[0], rgb[1], rgb[2]);
        let target = scope_pos(center, radius, u * 0.75, v * 0.75);
        p.circle_stroke(target, 2.8, Stroke::new(1.0, color.gamma_multiply(0.85)));
        if radius >= 36.0 {
            let label_pos = scope_pos(center, radius, u * 0.91, v * 0.91);
            p.text(label_pos, Align2::CENTER_CENTER, label, FontId::proportional(9.5), color.gamma_multiply(0.9));
        }
    }
}

/// Draw `h`'s distribution in `plot`, in `mode` and at `scale`.
pub fn paint_plot(p: &Painter, plot: Rect, h: &Histogram, mode: HistogramMode, scale: HistogramScale) {
    if !(plot.width() > 1.0 && plot.height() > 1.0) {
        return;
    }
    if mode == HistogramMode::Vectorscope {
        paint_vectorscope(p, plot, h, scale);
        return;
    }
    let (r, g, b) = (smooth(&h.r), smooth(&h.g), smooth(&h.b));
    let (luma, chroma) = (smooth(&h.luma), smooth(&h.chroma));
    match mode {
        HistogramMode::RgbOverlap => {
            let peak = peak(&[&r, &g, &b]);
            let mut mesh = Mesh::default();
            for i in 0..Histogram::BINS {
                let hs = [height(scale, &r, i, peak), height(scale, &g, i, peak), height(scale, &b, i, peak)];
                let (bands, n) = overlap_bands(hs);
                let (x0, x1) = column(plot.left(), plot.width(), i);
                for (from, to, parts) in bands.iter().take(n) {
                    let rect = Rect::from_min_max(pos2(x0, plot.bottom() - plot.height() * *to), pos2(x1, plot.bottom() - plot.height() * *from));
                    push_quad(&mut mesh, rect, blend_color(*parts));
                }
            }
            p.add(mesh);
            // the perceptual luminance as a hairline over the three channels
            stroke_curve(p, plot, &luma, scale, peak, Stroke::new(1.0, Color32::from_gray(235).gamma_multiply(0.8)));
        }
        HistogramMode::Luminance => {
            let peak = peak(&[&luma]);
            fill_curve(p, plot, &luma, scale, peak, Color32::from_gray(118));
            stroke_curve(p, plot, &luma, scale, peak, Stroke::new(1.5, Color32::from_gray(226)));
        }
        HistogramMode::Chroma => {
            let peak = peak(&[&chroma]);
            fill_curve(p, plot, &chroma, scale, peak, Color32::from_rgb(92, 100, 146));
            stroke_curve(p, plot, &chroma, scale, peak, Stroke::new(1.5, Color32::from_rgb(190, 200, 244)));
        }
        HistogramMode::ChannelsSeparated => {
            // one scale for all three tracks, so their peaks can be compared at a glance
            let peak = peak(&[&r, &g, &b]);
            let gap = 3.0;
            let track_h = ((plot.height() - gap * 2.0) / 3.0).max(1.0);
            for (k, bins) in [&r, &g, &b].into_iter().enumerate() {
                let top = plot.top() + (track_h + gap) * k as f32;
                let track = Rect::from_min_max(pos2(plot.left(), top), pos2(plot.right(), (top + track_h).min(plot.bottom())));
                let color = CHANNEL_COLORS.get(k).copied().unwrap_or(Color32::GRAY);
                fill_curve(p, track, bins, scale, peak, color.gamma_multiply(0.45));
                stroke_curve(p, track, bins, scale, peak, Stroke::new(1.5, color));
            }
        }
        HistogramMode::Vectorscope => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_of(control: &str) -> &'static ControlSpec {
        controls::find(control).unwrap_or_else(|| panic!("{control} is not a control"))
    }

    /// The five zones tile the plot left to right, each naming a real Basic tone control of the
    /// range this app documents (the panel's own specs, not a copy of them).
    #[test]
    fn the_zones_tile_the_plot_and_name_real_tone_controls() {
        assert_eq!(ZONES.len(), 5);
        assert_eq!(ZONES.first().map(|z| z.from), Some(0.0));
        assert_eq!(ZONES.last().map(|z| z.to), Some(1.0));
        for pair in ZONES.windows(2) {
            assert_eq!(pair[0].to, pair[1].from, "{:?} does not meet {:?}", pair[0].control, pair[1].control);
        }
        for zone in ZONES {
            let spec = spec_of(zone.control);
            assert_eq!(spec.section, lcc_develop::Section::Light, "{}", spec.id);
            assert_eq!(spec.default, 0.0, "{}", spec.id);
            let (min, max) = if spec.id == "light.exposure" { (-5.0, 5.0) } else { (-100.0, 100.0) };
            assert_eq!((spec.min, spec.max), (min, max), "{}", spec.id);
        }
    }

    /// A position across the plot picks its zone, at the boundaries too; anything off the plot —
    /// including NaN — picks none.
    #[test]
    fn a_position_across_the_plot_picks_its_zone() {
        for (fx, control) in [
            (0.0, "light.blacks"),
            (0.14, "light.blacks"),
            (0.15, "light.shadows"),
            (0.34, "light.shadows"),
            (0.35, "light.exposure"),
            (0.64, "light.exposure"),
            (0.65, "light.highlights"),
            (0.84, "light.highlights"),
            (0.85, "light.whites"),
            (0.99, "light.whites"),
            (1.0, "light.whites"),
        ] {
            assert_eq!(zone_at(fx).map(|z| z.control), Some(control), "{fx}");
        }
        for off in [-0.01, -1.0, 1.01, f32::NAN, f32::INFINITY] {
            assert!(zone_at(off).is_none(), "{off}");
        }
    }

    /// The zones' bands are their share of the plot's width, in order.
    #[test]
    fn a_zones_band_is_its_share_of_the_plot() {
        let plot = Rect::from_min_max(pos2(100.0, 20.0), pos2(400.0, 200.0));
        let bands: Vec<Rect> = ZONES.iter().map(|z| z.rect(plot)).collect();
        assert_eq!(bands.first().map(|b| b.left()), Some(plot.left()));
        assert_eq!(bands.last().map(|b| b.right()), Some(plot.right()));
        for band in &bands {
            assert_eq!((band.top(), band.bottom()), (plot.top(), plot.bottom()));
        }
        for pair in bands.windows(2) {
            assert_eq!(pair[0].right(), pair[1].left());
        }
        let exposure = bands.get(2).copied().unwrap_or(plot);
        assert!((exposure.width() - plot.width() * 0.30).abs() < 1e-3, "{exposure:?}");
    }

    /// A drag across the whole plot sweeps the whole range; Shift is a tenth; the ends clamp; the
    /// unrounded value a slow drag accumulates keeps adding up.
    #[test]
    fn a_drag_sweeps_the_control_and_clamps_at_its_ends() {
        let exposure = spec_of("light.exposure");
        let blacks = spec_of("light.blacks");
        assert!((dragged(exposure, 0.0, 20.0, 200.0, false) - 1.0).abs() < 1e-9, "a tenth of the plot is a tenth of ±5 EV");
        assert!((dragged(blacks, 0.0, 20.0, 200.0, false) - 20.0).abs() < 1e-9);
        assert!((dragged(blacks, 0.0, 200.0, 200.0, false) - 100.0).abs() < 1e-9);
        assert_eq!(dragged(blacks, 0.0, 400.0, 200.0, false), 100.0, "past the right end it stays");
        assert_eq!(dragged(blacks, 0.0, -400.0, 200.0, false), -100.0, "and past the left end");
        assert!((dragged(blacks, 0.0, 20.0, 200.0, true) - 2.0).abs() < 1e-9, "Shift is a tenth of the travel");
        assert!((dragged(blacks, 0.0, -20.0, 200.0, false) + 20.0).abs() < 1e-9, "left lowers");
        // a drag whose frames are each smaller than one step still adds up over the whole drag (the
        // deltas are f32 points, so the sum carries their rounding)
        let mut acc = 0.0;
        for _ in 0..10 {
            acc = dragged(blacks, acc, 0.4, 200.0, false);
        }
        assert!((acc - 4.0).abs() < 1e-6, "{acc}");
        // hostile input: no width, a hostile delta, a base that isn't a number
        assert_eq!(dragged(blacks, 5.0, 20.0, 0.0, false), 5.0);
        assert_eq!(dragged(blacks, 5.0, 20.0, -10.0, false), 5.0);
        assert_eq!(dragged(blacks, 5.0, f32::NAN, 200.0, false), 5.0);
        assert_eq!(dragged(blacks, 5.0, f32::INFINITY, 200.0, false), 5.0);
        assert!(dragged(blacks, f64::NAN, 20.0, 200.0, false).is_nan(), "a base that isn't a number stays as it is");
    }

    /// Dispatched values land on the control's steps (as the sliders dispatch them) and inside its
    /// range; a value that isn't a number gives the default rather than a NaN.
    #[test]
    fn a_dispatched_value_lands_on_the_controls_steps() {
        let blacks = spec_of("light.blacks");
        let exposure = spec_of("light.exposure");
        assert_eq!(quantized(blacks, 20.4), 20.0);
        assert_eq!(quantized(blacks, 20.6), 21.0);
        assert_eq!(quantized(blacks, 1e9), 100.0);
        assert_eq!(quantized(blacks, -1e9), -100.0);
        assert_eq!(quantized(blacks, f64::NAN), 0.0);
        assert!((quantized(exposure, 0.454) - 0.45).abs() < 1e-9, "{}", quantized(exposure, 0.454));
        for v in [-1e9, -3.7, 0.0, 2.2, 1e9] {
            let q = quantized(exposure, v);
            assert!((exposure.min..=exposure.max).contains(&q), "{v} → {q}");
        }
    }

    /// The readout names the zone's control and shows its value: EV for the exposure, the plain
    /// signed number otherwise, exactly as the sliders show them.
    #[test]
    fn the_readout_names_the_zone_and_its_value() {
        let active = |control: &str, value: f64, dragging: bool| Active {
            zone: ZONES.iter().copied().find(|z| z.control == control).unwrap_or(ZONES[0]),
            value,
            dragging,
        };
        assert_eq!(active("light.exposure", 0.45, false).readout().as_deref(), Some("Exposure: +0.45 EV"));
        assert_eq!(active("light.exposure", -2.0, true).readout().as_deref(), Some("Exposure: -2.00 EV"));
        assert_eq!(active("light.shadows", 25.0, true).readout().as_deref(), Some("Shadows: +25"));
        assert_eq!(active("light.blacks", 0.0, false).readout().as_deref(), Some("Blacks: 0"));
        assert_eq!(active("light.highlights", -100.0, false).readout().as_deref(), Some("Highlights: -100"));
        assert_eq!(active("light.whites", 13.0, false).readout().as_deref(), Some("Whites: +13"));
    }

    /// The overlap bands add the channels up: the bottom band is where every channel reaches, the
    /// middle where the two taller ones do, the top where only the tallest does.
    #[test]
    fn the_overlap_bands_stack_by_channel_height() {
        // red 0.2, green 0.6, blue 0.9
        let (bands, n) = overlap_bands([0.2, 0.6, 0.9]);
        assert_eq!(n, 3);
        assert_eq!(bands[0], (0.0, 0.2, [true, true, true]), "all three");
        assert_eq!(bands[1], (0.2, 0.6, [false, true, true]), "green + blue = cyan");
        assert_eq!(bands[2], (0.6, 0.9, [false, false, true]), "blue alone");
        // the order the channels are given in does not matter
        assert_eq!(overlap_bands([0.9, 0.2, 0.6]).0[1].2, [true, false, true], "red + blue = magenta");
        // equal heights make one band, empty ones make none
        let (bands, n) = overlap_bands([0.5, 0.5, 0.5]);
        assert_eq!((n, bands[0]), (1, (0.0, 0.5, [true; 3])));
        assert_eq!(overlap_bands([0.0, 0.0, 0.0]).1, 0);
        assert_eq!(overlap_bands([0.0, 0.0, 0.4]).1, 1, "only blue reaches anything");
        // hostile heights stay inside the plot and never claim a channel that isn't there
        let (bands, n) = overlap_bands([1.5, -1.0, f32::NAN]);
        assert_eq!((n, bands[0]), (1, (0.0, 1.0, [true, false, false])), "the NaN and negative channels are absent");
        for h in [[f32::NAN; 3], [f32::INFINITY, -1.0, 2.0], [0.0, 0.0, 0.0]] {
            let (bands, n) = overlap_bands(h);
            assert!(n <= 3);
            for (from, to, _) in bands.iter().take(n) {
                assert!((0.0..=1.0).contains(from) && to > from && *to <= 1.0, "{h:?} → ({from}, {to})");
            }
        }
    }

    /// An overlap band's colour is the sum of its channels: neutral where all three reach, the
    /// secondary colours where a pair does, a channel's own colour where it is alone.
    #[test]
    fn an_overlap_band_takes_the_colour_of_its_channels() {
        assert_eq!(blend_color([true, true, true]), Color32::from_gray(235), "neutral light grey");
        assert_eq!(blend_color([true, true, false]), Color32::from_rgb(226, 210, 74), "yellow = R + G");
        assert_eq!(blend_color([false, true, true]), Color32::from_rgb(74, 208, 214), "cyan = G + B");
        assert_eq!(blend_color([true, false, true]), Color32::from_rgb(214, 74, 206), "magenta = R + B");
        assert_eq!(blend_color([true, false, false]), CHANNEL_COLORS[0]);
        assert_eq!(blend_color([false, true, false]), CHANNEL_COLORS[1]);
        assert_eq!(blend_color([false, false, true]), CHANNEL_COLORS[2]);
        assert_eq!(blend_color([false, false, false]), Color32::TRANSPARENT);
        // the three primaries are distinct, and none of them is neutral
        for (i, c) in CHANNEL_COLORS.iter().enumerate() {
            assert!(c.r() != c.g() || c.g() != c.b(), "channel {i} is drawn in a colour");
            assert_eq!(CHANNEL_COLORS.iter().filter(|o| *o == c).count(), 1);
        }
    }

    /// The curves are smoothed (a one-bin spike spreads over five bins), short or hostile bin lists
    /// still give one value per bin, and the peak ignores a spike.
    #[test]
    fn the_curve_smoothing_keeps_a_spike_from_drawing_as_a_needle() {
        let mut spike = vec![0u32; Histogram::BINS];
        spike[128] = 1000;
        let s = smooth(&spike);
        assert_eq!(s.len(), Histogram::BINS);
        assert!(s[128] > 0.0 && s[128] < 1000.0, "{}", s[128]);
        assert!(s[126] > 0.0 && s[130] > 0.0, "the spike reaches its neighbours");
        assert_eq!(s[100], 0.0);
        assert!(s.iter().all(|v| v.is_finite()));
        assert_eq!(smooth(&[]).len(), Histogram::BINS, "no bins draws flat");
        assert!(smooth(&[1, 2, 3]).iter().all(|v| v.is_finite()));
        // the peak is the 99th percentile of the bins, so the spike does not flatten the rest
        let spiked = peak(&[&s]);
        assert!(spiked > 0.0 && spiked <= s[128], "the peak sits below the spike itself: {spiked} of {}", s[128]);
        let flat = vec![4.0f32; Histogram::BINS];
        assert_eq!(peak(&[&flat, &flat]), 4.0);
        assert!(peak(&[]).is_finite() && peak(&[]) > 0.0, "an empty plot still has a scale");
    }

    /// The vectorscope fits inside the plot, puts neutral at its centre, and places the six 75 %
    /// primary and secondary targets inside the disc in opposite pairs.
    #[test]
    fn the_vectorscope_fits_the_plot_and_places_its_six_targets_around_the_disc() {
        let plot = Rect::from_min_max(pos2(20.0, 10.0), pos2(280.0, 166.0));
        let (center, radius) = scope_disc(plot);
        assert_eq!(center, plot.center());
        assert!(radius > 40.0 && radius * 2.0 <= plot.height() && radius * 2.0 <= plot.width());
        assert_eq!(scope_pos(center, radius, 0.0, 0.0), center, "neutral is the centre of the disc");
        assert_eq!(scope_pos(center, radius, 1.0, 0.0), pos2(center.x + radius, center.y), "+Cb is right");
        assert_eq!(scope_pos(center, radius, 0.0, 1.0), pos2(center.x, center.y - radius), "+Cr is up");
        assert_eq!(SCOPE_TARGETS.len(), 6);
        for &(label, rgb, _) in &SCOPE_TARGETS {
            let (u, v) = scope_uv(rgb[0], rgb[1], rgb[2]);
            let p = scope_pos(center, radius, u * 0.75, v * 0.75);
            let dist = (p - center).length();
            assert!(dist > radius * 0.35 && dist <= radius * 0.85, "{label} sits on the 75% ring: {dist} of {radius}");
            assert!(plot.contains(p), "{label} stays inside the plot: {p:?}");
        }
        // opposite pairs (R/Cy, G/Mg, B/Yl) balance across the centre
        for (a, b) in [(0, 3), (4, 1), (2, 5)] {
            let (ua, va) = scope_uv(SCOPE_TARGETS[a].1[0], SCOPE_TARGETS[a].1[1], SCOPE_TARGETS[a].1[2]);
            let (ub, vb) = scope_uv(SCOPE_TARGETS[b].1[0], SCOPE_TARGETS[b].1[1], SCOPE_TARGETS[b].1[2]);
            assert!((ua + ub).abs() < 0.02 && (va + vb).abs() < 0.02, "{} vs {}: ({ua},{va}) + ({ub},{vb})", SCOPE_TARGETS[a].0, SCOPE_TARGETS[b].0);
        }
    }
}
