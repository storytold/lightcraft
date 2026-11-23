//! Actual Keyword List controls on the SDK component pipeline, not native focus proxies.
//! Separate leaf controls retain native Tab order (row, disclosure, enabled tick, arrow).
use std::ops::Deref;

use craft::behavior::{Metrics, PressPhase, Sense};
use craft::ui::contract::{Common, Component, Cx, NoValue, Outcome, PaintUi, Pointer as _, Props as _, Role, Show as _};
use craft::ui::runtime::access::Access;
use craft::ui::style::{ComponentStyle, Flags, FocusLook, StyleCx};
use egui::{Color32, Rect, Response, Shape, Stroke, StrokeKind, Ui, vec2};

use super::Tick;
use crate::theme::Tokens;

#[derive(Clone, Copy)]
pub(super) enum Face {
    Row { selected: bool },
    Disclosure { open: bool, can_fold: bool },
    Check { state: Tick, inside: bool, on: bool },
    Arrow { revealed: bool },
}

#[derive(Clone, Debug, PartialEq, ComponentStyle)]
struct ControlStyle {
    #[style(focus)]
    focus: FocusLook,
    #[style(metrics)]
    metrics: Metrics,
}

#[derive(Component)]
struct KeywordControl {
    #[common]
    common: Common,
    #[style_key]
    accent: Color32,
    face: Face,
    tokens: Tokens,
    opacity: f32,
}

impl Component for KeywordControl {
    const ROLE: Role = Role::BUTTON;
    const ALT_ROLES: &'static [Role] = &[Role::LIST_ITEM, Role::CHECK_BOX];
    const PARTS: bool = false;
    type Style = ControlStyle;
    type Output = ControlOutput;

    fn style(_: &StyleCx<'_>, key: KeywordControlStyleKey) -> ControlStyle {
        ControlStyle { focus: FocusLook::new(Stroke::new(1.5, key.accent), -2.0), metrics: Metrics::new() }
    }

    fn sense(&self) -> Sense {
        let mut sense = Sense::CLICK | Sense::FOCUS;
        // A tick captures its own drag but never turns it into a keyword drag. A secondary
        // or middle row drag is supported by the SDK press machine too, as on the native row.
        if matches!(self.face, Face::Row { .. } | Face::Check { .. }) {
            sense |= Sense::DRAG;
        }
        sense
    }

    fn flags(&self) -> Flags {
        match self.face {
            Face::Row { selected: true } => Flags::SELECTED,
            Face::Check { state: Tick::All, .. } => Flags::CHECKED,
            Face::Check { state: Tick::Some, .. } => Flags::MIXED,
            _ => Flags::empty(),
        }
    }

    fn access(&self, access: &mut Access, _: &Cx<'_, Self>) {
        match self.face {
            Face::Row { .. } => {
                access.role(Role::LIST_ITEM);
            }
            Face::Check { .. } => {
                access.role(Role::CHECK_BOX);
            }
            Face::Disclosure { open, .. } => {
                access.expanded(open);
            }
            Face::Arrow { .. } => {}
        }
    }

    fn paint(&mut self, ui: &mut PaintUi<'_>, cx: &Cx<'_, Self>) {
        let t = self.tokens;
        let ink = |color: Color32| color.gamma_multiply(self.opacity);
        let r = cx.rect;
        let hover = cx.response.hovered();
        let mut shapes = Vec::with_capacity(3);
        match self.face {
            Face::Row { selected } => {
                let inner = Rect::from_min_max(r.min + vec2(16.0, 0.0), r.max - vec2(16.0, 0.0));
                if selected || hover {
                    shapes.push(Shape::rect_filled(inner, 4.0, if selected { ink(t.canvas) } else { ink(t.hover).gamma_multiply(0.6) }));
                }
            }
            Face::Disclosure { open, can_fold } => {
                shapes.push(chevron(r, open, if hover && can_fold { ink(t.text) } else { ink(t.text_dim) }));
            }
            Face::Check { state, inside, on } => {
                // The real hit rect includes the native 3-point slop around the 13-point box.
                let boxr = r.shrink(3.0);
                let edge = if on { ink(t.text_label) } else { ink(t.text_dim).gamma_multiply(0.5) };
                match state {
                    Tick::All => {
                        shapes.push(Shape::rect_filled(boxr, 2.0, ink(t.accent)));
                        shapes.push(Shape::line(
                            vec![boxr.left_center() + vec2(2.5, 0.0), boxr.center_bottom() + vec2(-1.0, -3.0), boxr.right_top() + vec2(-2.5, 3.0)],
                            Stroke::new(1.6, ink(Color32::WHITE)),
                        ));
                    }
                    Tick::Some => {
                        shapes.push(Shape::rect_stroke(boxr, 2.0, Stroke::new(1.0, edge), StrokeKind::Inside));
                        shapes.push(Shape::line_segment(
                            [boxr.left_center() + vec2(3.0, 0.0), boxr.right_center() - vec2(3.0, 0.0)],
                            Stroke::new(1.6, ink(t.text)),
                        ));
                    }
                    Tick::No => {
                        shapes.push(Shape::rect_stroke(boxr, 2.0, Stroke::new(1.0, edge), StrokeKind::Inside));
                        if inside {
                            shapes.push(Shape::circle_filled(boxr.center(), 2.5, ink(t.text)));
                        }
                    }
                }
            }
            Face::Arrow { revealed } => {
                if revealed || hover || cx.response.has_focus() {
                    shapes.push(chevron(r, false, if hover { ink(t.text) } else { ink(t.text_dim) }));
                }
            }
        }
        ui.reserve_slot(cx.visible).extend(&cx.response.ctx, shapes);
    }

    fn finish(self, cx: Cx<'_, Self>) -> ControlOutput {
        let clicks = if cx.activation().is_some() {
            match cx.press().phase {
                PressPhase::Release { start, click: true, .. } => start.clicks,
                _ => 1,
            }
        } else {
            0
        };
        ControlOutput { response: cx.response.clone(), clicks, cancelled: matches!(cx.press().phase, PressPhase::Cancel { .. }) }
    }
}

fn chevron(rect: Rect, down: bool, ink: Color32) -> Shape {
    // Same 20-unit artwork and 16-point cap as icons::{ChevronDown, ChevronRight}.
    let side = rect.width().min(rect.height()).min(16.0);
    let r = Rect::from_center_size(rect.center(), vec2(side, side));
    let k = side / 20.0;
    let points = if down { [(5.0, 7.5), (10.0, 12.5), (15.0, 7.5)] } else { [(7.5, 5.0), (12.5, 10.0), (7.5, 15.0)] };
    Shape::line(points.into_iter().map(|(x, y)| r.min + vec2(x * k, y * k)).collect(), Stroke::new((1.5 * k).max(1.0), ink))
}

pub(super) struct ControlOutput {
    pub response: Response,
    /// The actual control's captured press was cancelled (Escape, focus loss or removal).
    pub cancelled: bool,
    clicks: u32,
}

impl ControlOutput {
    pub fn clicked(&self) -> bool {
        craft::ui::contract::clicked(&self.response)
    }
    pub fn double_clicked(&self) -> bool {
        self.clicked() && self.clicks >= 2
    }
    pub fn on_hover_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.response = self.response.on_hover_text(text);
        self
    }
}

impl Deref for ControlOutput {
    type Target = Response;
    fn deref(&self) -> &Response {
        &self.response
    }
}

impl Outcome for ControlOutput {
    type Value = NoValue;
    fn response(&self) -> &Response {
        &self.response
    }
}

pub(super) fn show(ui: &mut Ui, widget: &str, rect: Rect, face: Face, label: String, enabled: bool) -> ControlOutput {
    let t = Tokens::get(ui.ctx());
    let control = KeywordControl { common: Common::new(), accent: t.accent, face, tokens: t, opacity: ui.opacity() };
    let shown = control.label(label).id(egui::Id::unique(widget)).enabled(enabled && ui.is_enabled()).show_at(ui, rect);
    if !super::measuring(ui) {
        let visual = if matches!(face, Face::Check { .. }) { rect.shrink(3.0) } else { rect };
        crate::widgets::register_response(widget, visual, &shown.response);
    }
    shown
}
