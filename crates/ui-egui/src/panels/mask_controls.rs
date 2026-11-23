//! Actual Masking controls on the SDK component pipeline: the hand-drawn tiles, rows, eye toggles
//! and small hit areas of the Masking panel, the Effect menu and the face-box overlay. The caller
//! keeps what it paints (icons, names, the pin of a handle); the framework owns sensing, primary
//! activation and its click count, cancellation, focus, keyboard activation and accessibility, so
//! no egui `interact` stays behind. One leaf per control keeps the native Tab order.
use std::ops::Deref;

use craft::behavior::{Metrics, PressPhase, Sense};
use craft::ui::contract::{Common, Component, Cx, NoValue, Outcome, PaintUi, Pointer as _, Props as _, Role, Show as _};
use craft::ui::style::{ComponentStyle, Flags, FocusLook, StyleCx};
use egui::{Color32, Rect, Response, Shape, Stroke, Ui};

use crate::theme::Tokens;

/// The plate under a control: `rest` while idle and `hover` under the pointer, rounded by `radius`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(in crate::panels) struct Fill {
    pub rest: Option<Color32>,
    pub hover: Option<Color32>,
    pub radius: f32,
}

impl Fill {
    /// No plate: the caller paints everything.
    pub const NONE: Fill = Fill { rest: None, hover: None, radius: 0.0 };

    pub fn new(rest: Color32, hover: Color32, radius: f32) -> Self {
        Self { rest: Some(rest), hover: Some(hover), radius }
    }

    /// Only a hover plate.
    pub fn on_hover(hover: Color32, radius: f32) -> Self {
        Self { rest: None, hover: Some(hover), radius }
    }
}

#[derive(Clone, Debug, PartialEq, ComponentStyle)]
struct HitStyle {
    #[style(focus)]
    focus: FocusLook,
    #[style(metrics)]
    metrics: Metrics,
}

#[derive(Component)]
struct MaskHit {
    #[common]
    common: Common,
    #[style_key]
    accent: Color32,
    fill: Fill,
    selected: bool,
    drag: bool,
    opacity: f32,
}

impl Component for MaskHit {
    const ROLE: Role = Role::BUTTON;
    const PARTS: bool = false;
    type Style = HitStyle;
    type Output = HitOutput;

    fn style(_: &StyleCx<'_>, key: MaskHitStyleKey) -> HitStyle {
        // Inside the control, so a narrow panel's clipping never hides the keyboard focus ring.
        HitStyle { focus: FocusLook::new(Stroke::new(1.5, key.accent), -2.0), metrics: Metrics::new() }
    }

    fn sense(&self) -> Sense {
        let sense = Sense::CLICK | Sense::FOCUS;
        if self.drag { sense | Sense::DRAG } else { sense }
    }

    fn flags(&self) -> Flags {
        if self.selected { Flags::SELECTED } else { Flags::empty() }
    }

    fn paint(&mut self, ui: &mut PaintUi<'_>, cx: &Cx<'_, Self>) {
        let plate = if cx.response.hovered() { self.fill.hover.or(self.fill.rest) } else { self.fill.rest };
        let shapes: Vec<Shape> = plate
            .map(|color| Shape::rect_filled(cx.rect, self.fill.radius, color.gamma_multiply(self.opacity)))
            .into_iter()
            .collect();
        ui.reserve_slot(cx.visible).extend(&cx.response.ctx, shapes);
    }

    fn finish(self, cx: Cx<'_, Self>) -> HitOutput {
        let clicks = if cx.activation().is_some() {
            match cx.press().phase {
                PressPhase::Release { start, click: true, .. } => start.clicks,
                _ => 1,
            }
        } else {
            0
        };
        let phase = cx.press().phase;
        HitOutput {
            response: cx.response.clone(),
            clicks,
            cancelled: matches!(phase, PressPhase::Cancel { .. }),
            // egui's drag_stopped is only set while the press is still down, so the pass of the release is read from the press itself
            drag_ended: matches!(phase, PressPhase::Release { click: false, .. } | PressPhase::Cancel { .. }),
        }
    }
}

/// What a control reports: egui's geometry and hover response, and the SDK's activation.
///
/// `clicked()` follows the SDK's primary-release rule (including key and accessibility
/// activation); `double_clicked()` is such a click that was the second press of a double click.
/// Keep the output through tooltip chains.
pub(in crate::panels) struct HitOutput {
    pub response: Response,
    /// The captured press was cancelled (Escape, focus loss or removal of the control).
    pub cancelled: bool,
    clicks: u32,
    drag_ended: bool,
}

impl HitOutput {
    pub fn clicked(&self) -> bool {
        craft::ui::contract::clicked(&self.response)
    }

    /// A press that moved past the click slop ended this pass: released, or cancelled. This stands in for
    /// egui's `drag_stopped`, which the framework's response does not report on the pass of the release.
    pub fn drag_ended(&self) -> bool {
        self.drag_ended
    }

    pub fn double_clicked(&self) -> bool {
        self.clicked() && self.clicks >= 2
    }

    pub fn on_hover_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.response = self.response.on_hover_text(text);
        self
    }
}

impl Deref for HitOutput {
    type Target = Response;

    fn deref(&self) -> &Response {
        &self.response
    }
}

impl Outcome for HitOutput {
    type Value = NoValue;

    fn response(&self) -> &Response {
        &self.response
    }
}

/// Whether `ui` is only measuring: an invisible pass publishes no automation target and acts on nothing.
pub(in crate::panels) fn measuring(ui: &Ui) -> bool {
    ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx())
}

/// A control, built and then shown with [`Hit::show_at`] in a rectangle the caller reserved
/// (`ui.allocate_space`, which senses nothing, so this leaf is the one owner of the pointer).
pub(in crate::panels) struct Hit<'a> {
    widget: &'a str,
    label: String,
    fill: Fill,
    selected: bool,
    drag: bool,
    enabled: bool,
    visual: Option<Rect>,
    id: Option<egui::Id>,
}

impl<'a> Hit<'a> {
    /// A control registered for automation as `widget`, named `label` for a screen reader.
    pub fn new(widget: &'a str, label: impl Into<String>) -> Self {
        Self { widget, label: label.into(), fill: Fill::NONE, selected: false, drag: false, enabled: true, visual: None, id: None }
    }

    pub fn fill(mut self, fill: Fill) -> Self {
        self.fill = fill;
        self
    }

    /// The control is a chosen row, or a toggle that is on.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// A press on it may become a drag.
    pub fn drag(mut self, drag: bool) -> Self {
        self.drag = drag;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// The egui id to use in place of the one the automation name gives: for a control whose press
    /// state must not outlive the identity of the thing it belongs to.
    pub fn id(mut self, id: egui::Id) -> Self {
        self.id = Some(id);
        self
    }

    /// The rectangle published for automation, when it is smaller than the hit area.
    pub fn published(mut self, rect: Rect) -> Self {
        self.visual = Some(rect);
        self
    }

    pub fn show_at(self, ui: &mut Ui, rect: Rect) -> HitOutput {
        let t = Tokens::get(ui.ctx());
        let hit = MaskHit { common: Common::new(), accent: t.accent, fill: self.fill, selected: self.selected, drag: self.drag, opacity: ui.opacity() };
        let enabled = self.enabled && ui.is_enabled();
        let shown = hit.label(self.label).id(self.id.unwrap_or_else(|| egui::Id::unique(self.widget))).enabled(enabled).show_at(ui, rect);
        if !measuring(ui) {
            crate::widgets::register_response(self.widget, self.visual.unwrap_or(rect), &shown.response);
        }
        shown
    }
}
