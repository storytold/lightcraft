//! Host-painted click targets and context menus on the SDK pipeline.
//!
//! The detail, compare, filmstrip and faces pages paint their own pictures (photo canvases, thumbnails, face
//! crops, badges). What was a raw `ui.interact(.., Sense::click())` over such a picture is a [`HostTarget`]: the host
//! still paints the face, and the SDK owns sensing, the press machine (double click), keyboard activation, the focus
//! ring and the accessibility node, as the keyword list's and the Profiles tiles' controls do.

use std::hash::Hash;
use std::ops::Deref;

use craft::behavior::{Metrics, PressPhase, Sense};
use craft::ui::contract::{Common, Component, Cx, NoValue, Outcome, PaintUi, Pointer as _, Props as _, Role, Show as _};
use craft::ui::runtime::access::Access;
use craft::ui::menu::Menu;
use craft::ui::contract::ShowContainer as _;
use craft::ui::style::{ComponentStyle, FocusLook, StyleCx};
use egui::{Color32, Rect, Response, Stroke, Ui};

use crate::theme::Tokens;

#[derive(Clone, Debug, PartialEq, ComponentStyle)]
struct TargetStyle {
    #[style(focus)]
    focus: FocusLook,
    #[style(metrics)]
    metrics: Metrics,
}

/// How a host-painted target takes part in input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// A control: it activates, takes the keyboard focus (a Tab stop) and shows the focus ring.
    Control,
    /// A pointer handle over a canvas (a minimap): it clicks and drags but leaves the keyboard focus where it is.
    Handle,
    /// A pointer-only control (a cell or a caption that resets on a double click): it activates from the pointer but is
    /// no Tab stop.
    Pointer,
    /// A label over a canvas that only explains itself on hover; it takes no press, so the canvas beneath keeps its clicks.
    Hover,
}

#[derive(craft::ui::contract::Component)]
struct HostTarget {
    #[common]
    common: Common,
    #[style_key]
    accent: Color32,
    drag: bool,
    mode: Mode,
}

impl Component for HostTarget {
    const ROLE: Role = Role::BUTTON;
    const PARTS: bool = false;
    const OVERLAYS: bool = false;
    type Style = TargetStyle;
    type Output = HostOutput;

    fn style(_: &StyleCx<'_>, key: HostTargetStyleKey) -> TargetStyle {
        TargetStyle { focus: FocusLook::new(Stroke::new(2.0, key.accent), -2.0), metrics: Metrics::new() }
    }

    const ALT_ROLES: &'static [Role] = &[Role::LABEL, Role::CANVAS];

    fn sense(&self) -> Sense {
        match self.mode {
            Mode::Pointer if self.drag => Sense::CLICK | Sense::DRAG,
            Mode::Pointer => Sense::CLICK,
            Mode::Control if self.drag => Sense::CLICK | Sense::DRAG | Sense::FOCUS,
            Mode::Control => Sense::CLICK | Sense::FOCUS,
            Mode::Handle => Sense::CLICK | Sense::DRAG | Sense::KEEP_FOCUS,
            Mode::Hover => Sense::HOVER,
        }
    }

    fn access(&self, access: &mut Access, _: &Cx<'_, Self>) {
        match self.mode {
            Mode::Control | Mode::Pointer => {}
            Mode::Handle => {
                access.role(Role::CANVAS);
            }
            Mode::Hover => {
                access.role(Role::LABEL);
            }
        }
    }

    /// The host paints the face.
    fn paint(&mut self, _: &mut PaintUi<'_>, _: &Cx<'_, Self>) {}

    fn finish(self, cx: Cx<'_, Self>) -> HostOutput {
        let clicks = if cx.activation().is_some() {
            match cx.press().phase {
                PressPhase::Release { start, click: true, .. } => start.clicks,
                _ => 1,
            }
        } else {
            0
        };
        let by_key = cx.activation().is_some_and(|activation| activation.by_key);
        // egui's drag_stopped is only set while the press is still down, so the pass of the release is read from the
        // press itself (as mask_controls' HitOutput does)
        let drag_ended = matches!(cx.press().phase, PressPhase::Release { click: false, .. } | PressPhase::Cancel { .. });
        HostOutput { response: cx.response.clone(), clicks, by_key, drag_ended }
    }
}

/// What a host-painted target reports: its response, whether it was activated (pointer or keyboard) and whether
/// that was a double click.
pub(crate) struct HostOutput {
    pub response: Response,
    clicks: u32,
    by_key: bool,
    drag_ended: bool,
}

impl HostOutput {
    /// Activated this pass, by pointer release or keyboard.
    pub fn clicked(&self) -> bool {
        craft::ui::contract::clicked(&self.response)
    }

    /// A secondary-button click landed on the target this pass. The SDK rewrites the response's own click flags
    /// (`Response::clicked` is the primary activation), so this reads egui's raw verdict for the same widget.
    pub fn secondary_clicked(&self) -> bool {
        self.response.secondary_clicked() || self.response.ctx.read_response(self.response.id).is_some_and(|raw| raw.enabled() && raw.secondary_clicked())
    }

    /// The activation was the second press of a double click.
    pub fn double_clicked(&self) -> bool {
        self.clicked() && self.clicks >= 2
    }

    /// A press that moved past the click slop ended this pass: released, or cancelled. This stands in for egui's
    /// `drag_stopped`, which the framework's response does not report on the pass of the release.
    pub fn drag_ended(&self) -> bool {
        self.drag_ended
    }
}

impl Deref for HostOutput {
    type Target = Response;

    fn deref(&self) -> &Response {
        &self.response
    }
}

impl Outcome for HostOutput {
    type Value = NoValue;

    fn response(&self) -> &Response {
        &self.response
    }
}

/// What a target needs besides where it is.
pub(crate) struct Target<'a> {
    /// The accessible name.
    pub label: String,
    /// A hover tooltip; empty for none.
    pub tooltip: String,
    /// Whether a press may become a drag (the host reads `drag_started` from the response).
    pub drag: bool,
    /// Whether the target reads as the selected one.
    pub selected: bool,
    /// How it takes part in input.
    pub mode: Mode,
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> Target<'a> {
    pub fn new(label: impl Into<String>) -> Self {
        Target { label: label.into(), tooltip: String::new(), drag: false, selected: false, mode: Mode::Control, _lifetime: std::marker::PhantomData }
    }

    pub fn tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = tooltip.into();
        self
    }

    pub fn drag(mut self) -> Self {
        self.drag = true;
        self
    }

    /// A pointer handle over a canvas: clicks and drags, never takes the keyboard focus.
    pub fn handle(mut self) -> Self {
        self.mode = Mode::Handle;
        self.drag = true;
        self
    }

    /// A label that only shows its tooltip on hover.
    pub fn hover_only(mut self) -> Self {
        self.mode = Mode::Hover;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Pointer-only: no Tab stop.
    pub fn pointer_only(mut self) -> Self {
        self.mode = Mode::Pointer;
        self
    }

}

/// Show a host-painted target over `rect`, named `key` for egui. The caller registers its automation name with
/// `register_response` when the pass is a live one.
pub(crate) fn target(ui: &mut Ui, key: impl Hash + std::fmt::Debug, rect: Rect, spec: Target<'_>) -> HostOutput {
    let mut control = HostTarget { common: Common::new(), accent: Tokens::get(ui.ctx()).accent, drag: spec.drag, mode: spec.mode }
        .id(egui::Id::unique(key))
        .label(spec.label)
        .selected(spec.selected)
        .enabled(ui.is_enabled());
    if !spec.tooltip.is_empty() {
        control = control.tooltip(spec.tooltip);
    }
    let out = control.show_at(ui, rect);
    // A pointer click acts and leaves no keyboard focus behind, as the raw click targets these replace did: a focused
    // control makes the shell treat the next Escape as a text field's, so Escape would not go back from the photo.
    // A key activation keeps its focus.
    if out.clicked() && !out.by_key {
        ui.ctx().memory_mut(|memory| memory.surrender_focus(out.response.id));
    }
    out
}

/// Whether this pass draws and acts: not an invisible sizing pass and not an SDK measuring run.
pub(crate) fn live(ui: &Ui) -> bool {
    !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx())
}

/// The SDK context menu of `response`: secondary click and Shift+F10 open it. While photos are carried, or in a
/// measuring pass, it shows nothing. The body shows `MenuItem`s (or submenu rows), whose run closes the menu.
pub(crate) fn context_menu(ui: &mut Ui, response: &Response, label: &str, mut body: impl FnMut(&mut Ui)) {
    if !live(ui) || crate::widgets::carrying(ui.ctx()) {
        return;
    }
    Menu::context(label.to_owned(), response).id_salt(("lc-context-menu", response.id)).show(ui, |ui, _| body(ui));
}

/// LightCraft's own vector icon as the glyph of a framework button (a leading or trailing icon, 16 points).
#[derive(craft::ui::contract::MemoProps)]
pub(crate) struct IconGlyph(pub crate::icons::Icon);

impl craft::ui::primitives::Glyph for IconGlyph {
    fn paint(&self, painter: &egui::Painter, _: &craft::ui::style::UiTokens, rect: Rect, color: Color32, _: bool) {
        crate::icons::paint(painter, Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0)), self.0, color);
    }
}
