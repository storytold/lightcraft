//! A hand-painted clickable area on the framework component pipeline: a section header, a flyout row, a
//! segment, a drop-down's opener. The caller keeps what it paints (it reserves the room with
//! `ui.allocate_space`, which senses nothing, and paints over the rectangle after showing the leaf); the framework
//! owns sensing, primary activation, cancellation, focus, keyboard activation and accessibility, so no egui
//! `interact` stays behind. One leaf per area keeps the native Tab order.

use std::ops::Deref;

use craft::behavior::Metrics;
use craft::ui::contract::{Access, Common, Component, Cx, NoValue, Outcome, Props as _, Role, Show as _};
use craft::ui::primitives::PaintSlot;
use craft::ui::style::{ComponentStyle, FocusLook, StyleCx};
use egui::{Color32, Rect, Response, Stroke, Ui};

use crate::theme::Tokens;

#[derive(Clone, Debug, PartialEq, ComponentStyle)]
struct AreaStyle {
    #[style(focus)]
    focus: FocusLook,
    #[style(metrics)]
    metrics: Metrics,
}

#[derive(Component)]
struct AreaLeaf {
    #[common]
    common: Common,
    #[style_key]
    accent: Color32,
    /// A disclosure: whether what it folds is open.
    expanded: Option<bool>,
    /// What it is to a screen reader when not a plain button.
    role: Option<Role>,
}

impl Component for AreaLeaf {
    const ROLE: Role = Role::BUTTON;
    // the roles an area may take instead: a disclosure, or a drop-down's combo box
    const ALT_ROLES: &'static [Role] = &[Role::Node(egui::accesskit::Role::DisclosureTriangle), Role::COMBO_BOX];
    const OVERLAYS: bool = false;
    const PARTS: bool = false;
    type Style = AreaStyle;
    type Output = HitAreaOutput;

    fn style(_: &StyleCx<'_>, key: AreaLeafStyleKey) -> AreaStyle {
        // Inside the area, so a narrow panel's clipping never hides the keyboard focus ring.
        AreaStyle { focus: FocusLook::new(Stroke::new(2.0, key.accent), -2.0), metrics: Metrics::new() }
    }

    fn access(&self, access: &mut Access, _: &Cx<'_, Self>) {
        // A disclosure announces AccessKit's `Expanded` state, not `Toggled`.
        if let Some(open) = self.expanded {
            access.role(Role::Node(egui::accesskit::Role::DisclosureTriangle));
            access.expanded(open);
        }
        if let Some(role) = self.role {
            access.role(role);
        }
    }

    fn finish(self, cx: Cx<'_, Self>) -> HitAreaOutput {
        HitAreaOutput { response: cx.response.clone(), ring: None }
    }
}

/// What a [`HitArea`] reports: egui's geometry and hover response, and the framework's activation.
///
/// `clicked()` follows the framework's primary-release rule (including key and accessibility activation), not egui's
/// independent pointer duration limit. Keep the output through tooltip chains.
#[derive(Clone, Debug)]
pub struct HitAreaOutput {
    pub response: Response,
    /// The places in the paint list before and after the leaf's own paint (its keyboard focus ring), for
    /// [`HitAreaOutput::face_under_ring`].
    ring: Option<(PaintSlot, PaintSlot)>,
}

impl HitAreaOutput {
    pub fn clicked(&self) -> bool {
        craft::ui::contract::clicked(&self.response)
    }

    /// Keeps the face the caller painted over the area under the leaf's keyboard focus ring.
    ///
    /// The face fills the same rectangle the leaf paints its inset ring in, so a face painted after the leaf hides the
    /// ring from keyboard users. Call this once the face is drawn and before anything else is painted on the layer:
    /// what was drawn since [`HitArea::show_at`] moves to the place under the ring, keeping its order.
    pub fn face_under_ring(&self) {
        let Some((under, after)) = self.ring else { return };
        let ctx = &self.response.ctx;
        under.extend(ctx, after.take_after(ctx));
    }

    pub fn on_hover_text(mut self, text: impl Into<egui::WidgetText>) -> Self {
        self.response = self.response.on_hover_text(text);
        self
    }
}

impl Deref for HitAreaOutput {
    type Target = Response;

    fn deref(&self) -> &Response {
        &self.response
    }
}

impl Outcome for HitAreaOutput {
    type Value = NoValue;

    fn response(&self) -> &Response {
        &self.response
    }
}

/// A clickable area, built and then shown with [`HitArea::show_at`] in a rectangle the caller reserved.
pub struct HitArea {
    widget: Option<String>,
    label: String,
    id: egui::Id,
    selected: Option<bool>,
    expanded: Option<bool>,
    role: Option<Role>,
    enabled: bool,
}

impl HitArea {
    /// An area named `label` for a screen reader, under the egui id `id`.
    pub fn new(label: impl Into<String>, id: egui::Id) -> Self {
        Self { widget: None, label: label.into(), id, selected: None, expanded: None, role: None, enabled: true }
    }

    /// Registered for automation (scripts, tests, `ui.clickWidget`) as `widget`.
    pub fn widget(mut self, widget: impl Into<String>) -> Self {
        self.widget = Some(widget.into());
        self
    }

    /// What the area is to a screen reader when it is not a plain button: `Role::COMBO_BOX` for a drop-down (a
    /// disclosure takes its role from [`HitArea::expanded`]).
    pub fn role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// The area is a chosen segment or a toggle that is on (or off).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }

    /// The area folds something: whether it is open now.
    pub fn expanded(mut self, open: bool) -> Self {
        self.expanded = Some(open);
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Show it in `rect`. A measuring run publishes no automation name.
    pub fn show_at(self, ui: &mut Ui, rect: Rect) -> HitAreaOutput {
        let t = Tokens::get(ui.ctx());
        let mut leaf = AreaLeaf { common: Common::new(), accent: t.accent, expanded: self.expanded, role: self.role }
            .id(self.id)
            .label(self.label)
            .enabled(self.enabled && ui.is_enabled());
        if let Some(selected) = self.selected {
            leaf = leaf.selected(selected);
        }
        // The caller paints the area's face over this rectangle, where the leaf paints its inset focus ring: a place
        // in the paint list before the leaf's own paint is where the face goes, so the ring stays above it (see
        // `face_under_ring`).
        let under = PaintSlot::reserve(ui.painter());
        let shown = leaf.show_at(ui, rect);
        let after = PaintSlot::reserve(ui.painter());
        if let Some(widget) = self.widget
            && !super::measuring(ui)
        {
            super::register_response(widget, rect, &shown.response);
        }
        HitAreaOutput { ring: Some((under, after)), ..shown }
    }
}

#[cfg(test)]
#[path = "tests_hit_area.rs"]
mod tests;
