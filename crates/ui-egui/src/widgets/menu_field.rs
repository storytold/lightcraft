//! A field that opens a menu: the closed box of a dropdown (the framework's dropdown well, the shown value at the
//! reading start and the chevron at the end), for value menus whose rows keep automation names of their own. The
//! framework's `Select` owns its list rows; a host that builds its menu rows itself shows this face and opens its
//! `Menu` from the face's response.

use craft::behavior::{Metrics, Sense};
use craft::theme::{Stateful, States};
use craft::ui::contract::{Access, Common, Component, Cx, MeasureCx, PaintUi, Props as _, Role, Show as _, Styled as _};
use craft::ui::controls::disclosure::DisclosureGlyph;
use craft::ui::primitives::TextRun;
use craft::ui::style::{ComponentStyle, FocusLook, Ink, StyleColor, StyleCx, SurfaceLook, TypeRole};
use egui::{Rect, Response, Ui, Vec2, pos2};

/// Padding at each end of the box, and the chevron's side.
const PAD: f32 = 8.0;
const CHEVRON: f32 = 12.0;

#[derive(Clone, Debug, PartialEq, ComponentStyle)]
struct MenuFieldStyle {
    #[style(surface)]
    surface: SurfaceLook,
    #[style(focus)]
    focus: FocusLook,
    #[style(metrics)]
    metrics: Metrics,
    content: Stateful<StyleColor>,
}

#[derive(craft::ui::contract::Component)]
struct MenuFieldFace {
    #[common]
    common: Common,
    shown: String,
}

impl Component for MenuFieldFace {
    const ROLE: Role = Role::BUTTON;
    const OVERLAYS: bool = false;
    const PARTS: bool = false;
    type Style = MenuFieldStyle;
    type Output = Response;

    fn style(cx: &StyleCx<'_>, _: MenuFieldFaceStyleKey) -> MenuFieldStyle {
        let ink = cx.ink(Ink::OnPanel);
        let off = cx.color(cx.tokens.color.text.disabled);
        MenuFieldStyle {
            surface: cx.dropdown(),
            focus: cx.focus_look(),
            metrics: Metrics::new(),
            content: Stateful::new(ink).off(States::new(ink, ink, ink, off)),
        }
    }

    fn measure(&mut self, _: &MeasureCx<'_, Self>) -> Vec2 {
        // always shown at the rect its host lays out
        Vec2::new(120.0, 24.0)
    }

    fn sense(&self) -> Sense {
        Sense::CLICK | Sense::FOCUS
    }

    fn access(&self, access: &mut Access, _: &Cx<'_, Self>) {
        access.value(self.shown.clone());
    }

    fn paint(&mut self, ui: &mut PaintUi<'_>, cx: &Cx<'_, Self>) {
        let content = cx.resolve(&cx.style.content);
        let rect = cx.rect;
        let chevron = Rect::from_center_size(pos2(rect.right() - PAD - CHEVRON / 2.0, rect.center().y), Vec2::splat(CHEVRON));
        let text = Rect::from_min_max(pos2(rect.left() + PAD, rect.top()), pos2((chevron.left() - 4.0).max(rect.left() + PAD), rect.bottom()));
        let draw = ui.draw();
        draw.text(text, &TextRun::new(self.shown.as_str()).role(TypeRole::Body).color(content).elide());
        draw.icon(chevron, &DisclosureGlyph::dropdown().color(content));
    }

    fn finish(self, cx: Cx<'_, Self>) -> Response {
        cx.response.clone()
    }
}

/// The box at `rect`, named `name` (its accessible name and tooltip-free label), showing `shown`. Open the menu from
/// the response (`Menu::opened_by(&response, Trigger::Click)`); `craft::ui::contract::clicked` tells an activation.
pub(crate) fn menu_field(ui: &mut Ui, rect: Rect, salt: impl std::hash::Hash + std::fmt::Debug, name: &str, shown: &str) -> Response {
    let id = ui.scope_id().with(("lc-menu-field", salt));
    menu_field_with_id(ui, rect, id, name, shown)
}

/// [`menu_field`] under a given id (one that must stay what an older face used).
pub(crate) fn menu_field_with_id(ui: &mut Ui, rect: Rect, id: egui::Id, name: &str, shown: &str) -> Response {
    MenuFieldFace { common: Common::new(), shown: shown.to_owned() }.id(id).label(name.to_owned()).enabled(ui.is_enabled()).show_at(ui, rect)
}
