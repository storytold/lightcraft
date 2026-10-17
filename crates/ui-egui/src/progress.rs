//! Work in progress, shown one way everywhere: a [`Progress`] bar (a known fraction, or a gentle
//! sweep when nothing is known yet) and a [`spinner`] for short waits beside text or in a busy
//! [`crate::button::Button`]. It replaces `egui::ProgressBar` and `ui.spinner()`.
//!
//! ```ignore
//! Progress::new(Some(done as f32 / total as f32)).label(text).widget("denoise:progress").show(ui);
//! Progress::new(None).label(tr("Starting…")).show(ui);
//! ui.horizontal(|ui| { spinner(ui, 14.0); ui.label(tr("Selecting…")); });
//! ```
//!
//! A bar announces itself to screen readers as a progress indicator with its percentage. Motion
//! asks for repaints only while it is on screen. There is no reduced-motion setting in the app,
//! so the motion is kept slow and small: one gentle sweep, one slow turning arc.

use std::f32::consts::TAU;

use egui::{Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, pos2, vec2};

use crate::theme::Tokens;

/// A bar in a model row: it leaves the row's Cancel button its place.
pub const ROW_WIDTH: f32 = 260.0;
/// The narrowest a bar that fills the available width is drawn.
const MIN_WIDTH: f32 = 96.0;
/// Height of a bar that carries text.
const HEIGHT_WITH_TEXT: f32 = 18.0;
/// Height of a bar without text.
const HEIGHT_PLAIN: f32 = 6.0;
/// Seconds an indeterminate bar takes to sweep across and back.
const SWEEP_SECONDS: f64 = 1.8;
/// Seconds a spinner takes for one turn.
const TURN_SECONDS: f64 = 1.2;
/// How often moving things redraw.
const FRAME: std::time::Duration = std::time::Duration::from_millis(33);

/// `fraction` as a number from 0 to 1: NaN counts as nothing done, and out-of-range values stop at
/// the ends.
pub fn clamp_fraction(fraction: f32) -> f32 {
    if fraction.is_nan() { 0.0 } else { fraction.clamp(0.0, 1.0) }
}

/// A progress bar, built and then shown with [`Progress::show`].
#[derive(Clone, Debug)]
pub struct Progress {
    fraction: Option<f32>,
    label: Option<String>,
    widget: Option<String>,
    width: Option<f32>,
    dimmed: bool,
}

impl Progress {
    /// `Some(fraction)` of the work done (clamped to 0..=1, NaN as 0), or `None` while it is not
    /// known how much there is.
    pub fn new(fraction: Option<f32>) -> Self {
        Progress { fraction: fraction.map(clamp_fraction), label: None, widget: None, width: None, dimmed: false }
    }

    /// The fraction drawn, once clamped (`None` for an indeterminate bar).
    pub fn fraction(&self) -> Option<f32> {
        self.fraction
    }

    /// Text on the bar (a percentage, a count, a file's name), already translated.
    pub fn label(mut self, text: impl Into<String>) -> Self {
        self.label = Some(text.into());
        self
    }

    /// Registered under this id for scripts, tests and `ui.clickWidget` (e.g. `progress:samDownload`).
    pub fn widget(mut self, id: impl Into<String>) -> Self {
        self.widget = Some(id.into());
        self
    }

    /// Drawn in the quiet text colour instead of the accent (work that is being stopped).
    pub fn dimmed(mut self, dimmed: bool) -> Self {
        self.dimmed = dimmed;
        self
    }

    /// This wide; by default the bar fills the width the layout has left.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Show it where the layout puts it.
    pub fn show(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let label = self.label.as_deref().filter(|s| !s.is_empty());
        let width = self.width.unwrap_or_else(|| ui.available_size_before_wrap().x.max(MIN_WIDTH));
        let height = if label.is_some() { HEIGHT_WITH_TEXT } else { HEIGHT_PLAIN };
        let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        resp.widget_info(|| {
            let mut info = match label {
                Some(l) => egui::WidgetInfo::labeled(egui::WidgetType::ProgressIndicator, ui.is_enabled(), l),
                None => egui::WidgetInfo::new(egui::WidgetType::ProgressIndicator),
            };
            info.value = self.fraction.map(|f| f64::from(f * 100.0).floor());
            info
        });
        if let Some(w) = &self.widget {
            crate::widgets::register(ui.ctx(), w.clone(), rect);
        }
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let p = ui.painter_at(rect);
        let radius = height / 2.0;
        p.rect_filled(rect, radius, t.inset);
        p.rect_stroke(rect, radius, Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
        let inner = rect.shrink(1.0);
        let fill = match self.fraction {
            Some(f) => Rect::from_min_size(inner.min, vec2(inner.width() * f, inner.height())),
            None => {
                // a short bar sweeping across and back, eased so it slows at the ends
                let phase = (ui.input(|i| i.time) / SWEEP_SECONDS).fract() as f32;
                let eased = 0.5 - 0.5 * (phase * TAU).cos();
                let len = inner.width() * 0.3;
                let x = inner.left() + (inner.width() - len) * eased;
                ui.ctx().request_repaint_after(FRAME);
                Rect::from_min_size(pos2(x, inner.top()), vec2(len, inner.height()))
            }
        };
        if fill.width() > 0.0 {
            p.with_clip_rect(inner).rect_filled(fill, radius - 1.0, if self.dimmed { t.text_dim } else { t.accent });
        }
        if let Some(l) = label {
            // the text reads on the track in the theme's colour, and in white where the fill is
            let font = t.font(11.5);
            let on_track = p.layout_no_wrap(l.to_string(), font.clone(), t.text);
            let on_fill = p.layout_no_wrap(l.to_string(), font, Color32::WHITE);
            let at = pos2(rect.center().x - on_track.size().x / 2.0, rect.center().y - on_track.size().y / 2.0);
            let clipped = ui.painter_at(rect);
            clipped.galley(at, on_track, t.text);
            clipped.with_clip_rect(rect.intersect(fill)).galley(at, on_fill, Color32::WHITE);
        }
        resp
    }
}

/// The arc a spinner draws at `time` seconds: points on a circle of `radius` around `center`,
/// covering three quarters of the turn and rotating once per [`TURN_SECONDS`].
fn arc_points(center: Pos2, radius: f32, time: f64) -> Vec<Pos2> {
    let start = ((time / TURN_SECONDS).fract() as f32) * TAU;
    let steps = 24;
    (0..=steps)
        .map(|i| {
            let a = start + TAU * 0.75 * (i as f32 / steps as f32);
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect()
}

/// Draw a spinner of `size` points centred in `rect`, in `color`, at `time` seconds. Callers that
/// draw by hand (a busy button) use this; it asks for no repaint itself.
pub fn paint_spinner(p: &egui::Painter, rect: Rect, color: Color32, time: f64) {
    let size = rect.width().min(rect.height());
    let width = (size / 8.0).max(1.5);
    let radius = ((size - width) / 2.0).max(0.5);
    p.add(Shape::line(arc_points(rect.center(), radius, time), Stroke::new(width, color)));
}

/// A small turning arc beside text that says what is being waited for. It is drawn as a vector
/// arc, not a glyph, and asks for repaints only while it is on screen.
pub fn spinner(ui: &mut Ui, size: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::new(egui::WidgetType::ProgressIndicator));
    if ui.is_rect_visible(rect) {
        paint_spinner(ui.painter(), rect, t.text_dim, ui.input(|i| i.time));
        ui.ctx().request_repaint_after(FRAME);
    }
    resp
}
