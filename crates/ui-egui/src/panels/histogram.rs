//! The histogram's tonal zones, as in Lightroom Classic: hovering one names it and shows its value,
//! dragging one sideways moves its Basic control, and a double-click puts that control back to its
//! default.
//!
//! The zone a press landed in stays locked for the whole drag, so the pointer may cross the others;
//! every step is a `develop.set` between `develop.beginInteraction` and `develop.endInteraction`, the
//! path the panel's sliders take: the draft render follows the drag live and the whole drag lands as
//! one undo step (`end_interaction`).

use egui::{Painter, Pos2, Rect, Response, pos2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::{ControlSpec, controls};
use serde_json::json;

use crate::LightcraftApp;
use crate::panels::detail::Gesture;
use crate::theme::Tokens;
use crate::widgets::{register, shown_value};

/// One tonal zone across the histogram's plot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zone {
    /// The develop control the zone adjusts: an id of [`lightcraft_develop::controls`].
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

/// The rectangles of the clipping buttons in the plot's top corners (shadows left, highlights right).
pub fn clipping_rects(ui: &egui::Ui, plot: Rect) -> [Rect; 2] {
    let scroll = ui.spacing().scroll;
    clipping_rects_in(plot, scroll.bar_width, scroll.bar_outer_margin)
}

/// The clipping indicator rectangles for a given `plot` and scroll-bar geometry.
pub fn clipping_rects_in(plot: Rect, scroll_bar_width: f32, scroll_bar_outer_margin: f32) -> [Rect; 2] {
    let inset = 20.0 + scroll_bar_width + scroll_bar_outer_margin;
    [
        Rect::from_center_size(pos2(plot.left() + inset, plot.top() + 14.0), egui::vec2(24.0, 22.0)),
        Rect::from_center_size(pos2(plot.right() - inset, plot.top() + 14.0), egui::vec2(24.0, 22.0)),
    ]
}

/// The histogram's pointer input for this frame: the press locks its zone and opens the interaction,
/// the drag moves that control, a double-click resets it. Returns what the highlight and the readout
/// should show, `None` when the pointer is nowhere near a zone.
///
/// `plot` is the plot's rect: the strip under it (the readout and the EXIF line) is not a zone, and
/// neither are the clipping buttons in the plot's top corners, which take their own presses.
pub fn interact(app: &mut LightcraftApp, ui: &egui::Ui, resp: &Response, plot: Rect, id: PhotoId) -> Option<Active> {
    let width = plot.width();
    let clips = clipping_rects(ui, plot);
    let in_plot = |q: Pos2| plot.contains(q) && !clips.iter().any(|c| c.contains(q));
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
    register(ui, "histogram:zone", zone.rect(plot));
    Some(Active { zone, value: value_of(app, id, spec), dragging })
}

/// End the histogram drag: the interaction commits the whole drag as one undo step.
fn finish(app: &mut LightcraftApp) {
    app.gesture = None;
    app.ui.dragging_control = None;
    let _ = app.run("develop.endInteraction", json!({}));
}

/// A control's value on the photo, or its default when the photo has no settings yet.
fn value_of(app: &LightcraftApp, id: PhotoId, spec: &ControlSpec) -> f64 {
    controls::get(&app.session.develop_of(id).unwrap_or_default(), spec.id).unwrap_or(spec.default)
}

/// The band the active zone draws over the plot: a subtle translucent highlight, a little stronger
/// while the zone is being dragged. Painted behind the histogram's curves.
pub fn paint_zone(painter: &Painter, plot: Rect, active: &Active, tokens: &Tokens) {
    let tint = if active.dragging { tokens.pressed } else { tokens.hover };
    painter.rect_filled(active.zone.rect(plot), 0.0, tint.gamma_multiply(if active.dragging { 0.75 } else { 0.45 }));
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
            assert_eq!(spec.section, lightcraft_develop::Section::Light, "{}", spec.id);
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

    /// The clipping rectangles sit in the plot's top corners, symmetric from both edges.
    #[test]
    fn clipping_rects_sit_in_the_plots_top_corners() {
        let plot = Rect::from_min_max(pos2(100.0, 20.0), pos2(400.0, 200.0));
        let [low, high] = clipping_rects_in(plot, 10.0, 4.0);
        assert!(plot.contains(low.center()));
        assert!(plot.contains(high.center()));
        assert_eq!(low.top(), plot.top() + 3.0);
        assert_eq!(high.top(), plot.top() + 3.0);
        assert_eq!(low.size(), egui::vec2(24.0, 22.0));
        assert_eq!(high.size(), egui::vec2(24.0, 22.0));
        assert!((low.left() - plot.left() - (plot.right() - high.right())).abs() < 1e-4);
    }
}
