//! The legacy dense text-button face on the SDK component pipeline.
//!
//! This is not `buttons::Button`: its private style cannot express our exact semibold 11.5 pt
//! label, +18/min-37 width and 24 pt height. Keep that face/measurement here; the framework
//! owns sensing, primary activation, cancellation, focus, keyboard and accessibility.

use std::ops::Deref;
use std::sync::Arc;

use craft::behavior::Metrics;
use craft::theme::{Stateful, States};
use craft::ui::contract::{Common, Component, Cx, MeasureCx, NoValue, Outcome, PaintUi, Props as _, Role, Show as _};
use craft::ui::style::{ComponentStyle, FocusLook, StyleCx, SurfaceLook};
use egui::{Color32, CornerRadius, Galley, Response, Sense, Stroke, Ui, Vec2, vec2};

use crate::theme::Tokens;

/// All face colours participate in the style key, including the currently installed app theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Palette {
    button: Color32,
    hover: Color32,
    pressed: Color32,
    border: Color32,
    text: Color32,
    focus: Color32,
}

impl From<&Tokens> for Palette {
    fn from(t: &Tokens) -> Self {
        Self { button: t.button, hover: t.hover, pressed: t.pressed, border: t.button_border, text: t.text, focus: t.accent }
    }
}

#[derive(Clone, Debug, PartialEq, ComponentStyle)]
struct DenseStyle {
    #[style(surface)]
    surface: SurfaceLook,
    #[style(focus)]
    focus: FocusLook,
    #[style(metrics)]
    metrics: Metrics,
}

#[derive(Component)]
struct DenseTextButton {
    #[common]
    common: Common,
    #[style_key]
    palette: Palette,
    #[style_key]
    active: bool,
    galley: Arc<Galley>,
    opacity: f32,
}

impl DenseTextButton {
    fn new(ui: &Ui, label: &str, active: bool) -> Self {
        let t = Tokens::get(ui.ctx());
        let galley = ui.painter().layout_no_wrap(label.to_owned(), t.semibold(11.5), t.text);
        Self { common: Common::new(), palette: Palette::from(&t), active, galley, opacity: ui.opacity() }.label(label.to_owned()).selected(active)
    }

    fn desired_size(&self) -> Vec2 {
        vec2(super::text_button_width(self.galley.size().x), 24.0)
    }
}

impl Component for DenseTextButton {
    const ROLE: Role = Role::BUTTON;
    const OVERLAYS: bool = false;
    const PARTS: bool = false;
    type Style = DenseStyle;
    type Output = TextButtonOutput;

    fn style(cx: &StyleCx<'_>, key: DenseTextButtonStyleKey) -> DenseStyle {
        let p = key.palette;
        let color = |value: Color32| cx.color(craft::theme::Rgba::from(value));
        // The original selected face stays pressed even when hovered. A disabled scope already
        // fades its painter; do not fade its colours twice or change the selected surface.
        let fill = if key.active {
            Stateful::new(color(p.pressed))
        } else {
            Stateful::new(color(p.button)).off(States::new(color(p.button), color(p.hover), color(p.hover), color(p.button)))
        };
        DenseStyle {
            surface: SurfaceLook::new(fill).radius(CornerRadius::same(4)).border(Stateful::new(color(p.border)), 1.0),
            // Inside the control so narrow/mobile clipping never hides the keyboard focus ring.
            focus: FocusLook::new(Stroke::new(2.0, p.focus), -2.0),
            metrics: Metrics::new(),
        }
    }

    fn measure(&mut self, _: &MeasureCx<'_, Self>) -> Vec2 {
        self.desired_size()
    }

    fn paint(&mut self, ui: &mut PaintUi<'_>, cx: &Cx<'_, Self>) {
        // A reserved SDK paint slot keeps the exact existing galley, under the pipeline's clip
        // and focus ring. It creates no widget, id, focus stop or second interaction owner.
        let shape = egui::epaint::TextShape::new(cx.rect.center() - self.galley.size() / 2.0, Arc::clone(&self.galley), self.palette.text)
            .with_opacity_factor(self.opacity);
        ui.reserve_slot(cx.visible).extend(&cx.response.ctx, vec![shape.into()]);
    }

    fn finish(self, cx: Cx<'_, Self>) -> TextButtonOutput {
        TextButtonOutput { response: cx.response.clone(), native: false }
    }
}

/// A dense button's SDK activation and its egui geometry/hover response.
///
/// `clicked()` follows the SDK's primary-release rule (including key/AT activation), not
/// egui's independent pointer duration limit. Keep the output through tooltip chains; use
/// `menu()` instead of `egui::Popup::menu(&output)`, which would call egui's click predicate.
/// No response flags or pointer/keyboard events are synthesized or rewritten by this adapter.
#[derive(Clone, Debug)]
pub struct TextButtonOutput {
    pub response: Response,
    native: bool,
}

impl TextButtonOutput {
    pub fn clicked(&self) -> bool {
        if self.native { self.response.clicked() } else { craft::ui::contract::clicked(&self.response) }
    }

    pub fn on_hover_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.response = self.response.on_hover_text(text);
        self
    }

    /// The existing egui popup appearance/placement, with the SDK result as its explicit toggle.
    pub fn menu(&self) -> egui::Popup<'_> {
        egui::Popup::menu(&self.response).open_memory(self.clicked().then_some(egui::SetOpenCommand::Toggle))
    }
}

impl Deref for TextButtonOutput {
    type Target = Response;

    fn deref(&self) -> &Response {
        &self.response
    }
}

impl Outcome for TextButtonOutput {
    type Value = NoValue;

    fn response(&self) -> &Response {
        &self.response
    }
}

pub(super) fn show(ui: &mut Ui, id: &str, label: &str, active: bool) -> TextButtonOutput {
    let label = if id.starts_with("labelSet-") { label } else { crate::i18n::tr(label) };
    let button = DenseTextButton::new(ui, label, active);
    // Use the existing egui allocation, not SDK flow allocation, to retain horizontal_wrapped
    // rows and RTL placement. The allocation senses hover only; only the SDK leaf owns clicks.
    let (rect, _) = ui.allocate_exact_size(button.desired_size(), Sense::hover());
    let response = button.id_salt(("lightcraft-text-button", id)).show_at(ui, rect);
    // Framework Pages/layout can run this closure to measure before the real draw. Publishing
    // that invisible run would produce a duplicate automation registration with a stale rect.
    if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        super::register_response(format!("button:{id}"), rect, &response);
    }
    response
}

#[cfg(test)]
#[path = "tests_text_button.rs"]
mod tests;

// Retained native Settings boundary until mixed traversal is supported by the SDK.
fn native(ui: &mut Ui, id: &str, label: &str, active: bool) -> TextButtonOutput {
    let label = if id.starts_with("labelSet-") { label } else { crate::i18n::tr(label) };
    let t = Tokens::get(ui.ctx());
    let font = t.semibold(11.5);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, t.text);
    let size = vec2(super::text_button_width(galley.size().x), 24.0);
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, active, label));
    super::register(ui, format!("button:{id}"), r);
    let fill = if active {
        t.pressed
    } else if resp.hovered() {
        t.hover
    } else {
        t.button
    };
    let p = ui.painter();
    p.rect(r, CornerRadius::same(4), fill, Stroke::new(1.0, t.button_border), egui::StrokeKind::Inside);
    p.galley(r.center() - galley.size() / 2.0, galley, t.text);
    TextButtonOutput { response: resp, native: true }
}
