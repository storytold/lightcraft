//! LightCraft's compact vector buttons on the supported SDK component lifecycle.
//!
//! The stock square IconButton cannot express a 32 pt selected plate inside a 40 pt strip
//! target, or the existing rectangular 20×20 through 40×32 targets. Only that face is local;
//! the SDK owns sensing, cancellation, focus, keyboard activation and accessibility.

use std::ops::Deref;

use craft::behavior::Metrics;
use craft::theme::{Stateful, States};
use craft::ui::contract::{Common, Component, Cx, MeasureCx, NoValue, Outcome, PaintUi, Props as _, Role, Show as _, Styled as _};
use craft::ui::primitives::{Glyph, IconSpec};
use craft::ui::style::{ComponentStyle, FocusLook, StyleColor, StyleCx, SurfaceLook, UiTokens};
use egui::{Color32, CornerRadius, Painter, Rect, Response, Stroke, Ui, Vec2, vec2};

use crate::icons::Icon;
use crate::theme::Tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Palette {
    active: Color32,
    hover: Color32,
    icon: Color32,
    text: Color32,
    selected: Color32,
    disabled: Color32,
    focus: Color32,
}

impl From<&Tokens> for Palette {
    fn from(t: &Tokens) -> Self {
        Self {
            active: t.tool_active,
            hover: t.hover.gamma_multiply(0.7),
            icon: t.icon,
            text: t.text,
            selected: t.pick,
            disabled: t.text_disabled,
            focus: t.accent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, ComponentStyle)]
struct CompactStyle {
    // This plate is intentionally smaller than the interaction rectangle in the tool strip.
    plate: SurfaceLook,
    glyph: Stateful<StyleColor>,
    #[style(focus)]
    focus: FocusLook,
    #[style(metrics)]
    metrics: Metrics,
}

struct AppGlyph(Icon);

impl Glyph for AppGlyph {
    fn paint(&self, painter: &Painter, _: &UiTokens, rect: Rect, color: Color32, _: bool) {
        // These are LightCraft's original vector drawings, not a substituted SDK icon set.
        crate::icons::paint(painter, rect, self.0, color);
    }
}

#[derive(Component)]
struct CompactIconButton {
    #[common]
    common: Common,
    #[style_key]
    palette: Palette,
    #[style_key]
    active: bool,
    glyph: AppGlyph,
    face: Rect,
    size: Vec2,
}

impl Component for CompactIconButton {
    const ROLE: Role = Role::BUTTON;
    const OVERLAYS: bool = false;
    const PARTS: bool = false;
    const SHOWS_LABEL: bool = false;
    type Style = CompactStyle;
    type Output = IconButtonOutput;

    fn style(cx: &StyleCx<'_>, key: CompactIconButtonStyleKey) -> CompactStyle {
        let p = key.palette;
        let color = |value: Color32| cx.color(craft::theme::Rgba::from(value));
        let clear = color(Color32::TRANSPARENT);
        let plate = if key.active {
            Stateful::new(color(p.active))
        } else {
            // The old face has no separate pressed colour: a press keeps its hovered plate.
            Stateful::new(clear).off(States::new(clear, color(p.hover), color(p.hover), clear))
        };
        let rest = color(if key.active { p.selected } else { p.icon });
        let hover = color(if key.active { p.selected } else { p.text });
        CompactStyle {
            plate: SurfaceLook::new(plate).radius(CornerRadius::same(4)),
            glyph: Stateful::new(rest).off(States::new(rest, hover, hover, color(p.disabled))),
            focus: FocusLook::new(Stroke::new(2.0, p.focus), -2.0),
            metrics: Metrics::new(),
        }
    }

    fn measure(&mut self, _: &MeasureCx<'_, Self>) -> Vec2 {
        self.size
    }

    fn paint(&mut self, ui: &mut PaintUi<'_>, cx: &Cx<'_, Self>) {
        let plate = Rect::from_center_size(self.face.center(), vec2(self.size.x.min(32.0), self.size.y.min(32.0)));
        let side = (self.size.x.min(self.size.y) * 0.62).min(22.0);
        let draw = ui.draw();
        draw.surface_look(plate, &cx.style.plate, cx.state());
        draw.icon(Rect::from_center_size(self.face.center(), Vec2::splat(side)), &IconSpec::glyph(&self.glyph).color(cx.resolve(&cx.style.glyph)));
    }

    fn finish(self, cx: Cx<'_, Self>) -> IconButtonOutput {
        IconButtonOutput { response: cx.response.clone() }
    }
}

/// Preserve the SDK primary-release result through tooltip and popup chains.
/// `response` remains available for geometry and changed-state reporting;
/// it must not replace `clicked()` or `menu()` as the activation oracle.
#[derive(Clone, Debug)]
pub struct IconButtonOutput {
    pub response: Response,
}

impl IconButtonOutput {
    pub fn clicked(&self) -> bool {
        craft::ui::contract::clicked(&self.response)
    }

    pub fn on_hover_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.response = self.response.on_hover_text(text);
        self
    }

    /// Keep the caller's egui popup placement/appearance, toggled by SDK activation.
    pub fn menu(&self) -> egui::Popup<'_> {
        egui::Popup::menu(&self.response).open_memory(self.clicked().then_some(egui::SetOpenCommand::Toggle))
    }
}

impl Deref for IconButtonOutput {
    type Target = Response;

    fn deref(&self) -> &Response {
        &self.response
    }
}

impl Outcome for IconButtonOutput {
    type Value = NoValue;

    fn response(&self) -> &Response {
        &self.response
    }
}

pub(super) fn show(ui: &mut Ui, id: &str, icon: Icon, size: Vec2, active: bool, enabled: bool, tooltip: &str) -> IconButtonOutput {
    let tooltip = crate::i18n::tr(tooltip);
    let (label, hint) = super::split_key_hint(tooltip);
    // allocate_space is allocate_exact_size's allocation, without its egui interaction. Pass
    // that same auto ID and full justified hit rectangle to the SDK: no second click owner.
    let (widget_id, hit) = ui.allocate_space(size);
    let face = ui.layout().align_size_within_rect(size, hit);
    let mut button =
        CompactIconButton { common: Common::new(), palette: Palette::from(&Tokens::get(ui.ctx())), active, glyph: AppGlyph(icon), face, size }
            .id(widget_id)
            .label(label.to_owned())
            .selected(active)
            .enabled(enabled);
    if !tooltip.is_empty() {
        button = button.tooltip(tooltip.to_owned());
    }
    if let Some(hint) = hint {
        button = button.description(hint.to_owned());
    }
    let output = button.show_at(ui, hit);
    // Invisible SDK/egui measurement is not an automation target or a second registration.
    if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        super::register_response(format!("icon:{id}"), face, &output.response);
    }
    output
}

#[cfg(test)]
#[path = "icon_button_tests.rs"]
mod tests;
