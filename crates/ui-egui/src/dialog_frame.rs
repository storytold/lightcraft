//! One layout for every modal dialog: a fixed **header** (the title bar, plus tabs where the dialog
//! has them), a **body** that scrolls vertically, and a fixed **footer** with the action buttons.
//!
//! The whole window is capped to the viewport height minus [`SCREEN_MARGIN`] at the top and the
//! bottom, so the footer buttons stay on screen however tall the content is (a 1366×768 laptop, a
//! small window). When the content fits, the body takes just its own height: no scrollbar and no
//! empty space. The body keeps the full dialog width, so a scrollbar appearing doesn't reflow it.
//!
//! The parts are drawn in order inside the window's content closure, so each can borrow what it
//! needs mutably:
//!
//! ```ignore
//! let shown = egui::Window::new(title).id(id).show(ctx, |ui| {
//!     let mut layout = ModalLayout::begin(ui, id);
//!     tabs(ui);                               // header (optional)
//!     layout.body(ui, |ui| content(ui));     // scrolls when needed
//!     layout.footer(ui, |ui| buttons(ui));    // always visible
//! });
//! ModalLayout::finish(ctx, id, shown.as_ref().map(|w| w.response.rect));
//! ```
//!
//! The heights of the footer and of the window chrome (title bar, frame margins) are measured each
//! frame and used on the next, so the cap settles after the first frame.

use egui::{Id, Rect, Ui};

/// Room kept free above and below a dialog, in points.
pub const SCREEN_MARGIN: f32 = 32.0;
/// The body never gets shorter than this (a viewport smaller than any dialog still shows some).
const MIN_BODY: f32 = 60.0;
/// First-frame guesses, before anything was measured.
const GUESS_CHROME: f32 = 64.0;
const GUESS_FOOTER: f32 = 44.0;

/// The tallest a dialog window may be in this viewport.
pub fn max_window_height(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() - 2.0 * SCREEN_MARGIN).max(MIN_BODY * 2.0)
}

/// The header / scrolling body / footer layout of a modal dialog; see the module docs.
pub struct ModalLayout {
    id: Id,
    /// the most the content (header + body + footer) may take
    max_content: f32,
    top: f32,
    body_bottom: Option<f32>,
}

impl ModalLayout {
    /// Starts the layout at the top of a dialog window's content. `id` is the window's id.
    pub fn begin(ui: &Ui, id: Id) -> Self {
        Self::begin_with_max(ui, id, max_window_height(ui.ctx()))
    }

    /// [`begin`](Self::begin) for a window anchored `top` points below the viewport's top edge
    /// rather than centred: it may reach down to [`SCREEN_MARGIN`] above the bottom.
    pub fn begin_below(ui: &Ui, id: Id, top: f32) -> Self {
        let room = (ui.ctx().content_rect().height() - top.max(0.0) - SCREEN_MARGIN).max(MIN_BODY * 2.0);
        Self::begin_with_max(ui, id, room)
    }

    fn begin_with_max(ui: &Ui, id: Id, max_window: f32) -> Self {
        let chrome = ui.ctx().data(|d| d.get_temp::<f32>(id.with("dialog-chrome"))).unwrap_or(GUESS_CHROME);
        let max_content = max_window - chrome.clamp(0.0, 400.0);
        Self { id, max_content, top: ui.cursor().top(), body_bottom: None }
    }

    /// The height left for the body now that the header is drawn.
    pub fn body_max_height(&self, ui: &Ui) -> f32 {
        let footer = ui.ctx().data(|d| d.get_temp::<f32>(self.id.with("dialog-footer"))).unwrap_or(GUESS_FOOTER);
        let header = (ui.cursor().top() - self.top).max(0.0);
        (self.max_content - header - footer.clamp(0.0, 400.0)).max(MIN_BODY)
    }

    /// The middle of the dialog: scrolls vertically once it is taller than the room left.
    ///
    /// While the content fits (as measured on the previous frame) it is laid out directly, so the
    /// dialog has its exact height from the first frame (a scroll area sizes itself from the
    /// previous frame's content, which would make a just-opened dialog jump). Once it doesn't fit,
    /// a scroll area exactly the room left holds it.
    pub fn body<R>(&mut self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
        let max = self.body_max_height(ui);
        let key = self.id.with("dialog-body-height");
        let last = ui.ctx().data(|d| d.get_temp::<f32>(key));
        let (out, height) = if last.is_some_and(|h| h > max) {
            let out = egui::ScrollArea::vertical()
                .id_salt(self.id.with("dialog-body"))
                .max_height(max)
                .min_scrolled_height(max)
                // full width: no reflow when the scrollbar appears
                .auto_shrink([false, false])
                .show(ui, add);
            (out.inner, out.content_size.y)
        } else {
            let top = ui.cursor().top();
            let out = ui.scope(add).inner;
            (out, ui.min_rect().bottom() - top)
        };
        self.body_bottom = Some(ui.cursor().top());
        remember(ui.ctx(), key, height);
        out
    }

    /// The action buttons at the bottom; always on screen.
    pub fn footer<R>(&mut self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
        let start = self.body_bottom.unwrap_or_else(|| ui.cursor().top());
        let out = add(ui);
        let end = ui.min_rect().bottom();
        let (id, content) = (self.id, end - self.top);
        remember(ui.ctx(), id.with("dialog-footer"), (end - start).max(0.0));
        ui.ctx().data_mut(|d| d.insert_temp(id.with("dialog-content"), content.max(0.0)));
        out
    }

    /// Records the window chrome (title bar, margins) from the shown window, for the next frame.
    pub fn finish(ctx: &egui::Context, id: Id, window: Option<Rect>) {
        let Some(rect) = window else { return };
        let Some(content) = ctx.data(|d| d.get_temp::<f32>(id.with("dialog-content"))) else { return };
        let chrome = (rect.height() - content).max(0.0);
        if chrome.is_finite() {
            remember(ctx, id.with("dialog-chrome"), chrome);
        }
    }
}

/// A dialog body in two columns side by side (Export: its presets beside its settings), as tall as
/// the right column's content (measured on the previous frame) but at least `min_h` and at most
/// `max_h`; the right column scrolls when it is taller. Draw the left column into [`left`]
/// (`ui.scope_builder(UiBuilder::new().max_rect(cols.left), …)`) and the right one with
/// [`TwoColumns::right`]. Laid out in rectangles rather than a horizontal row, whose children get
/// one row's height.
///
/// [`left`]: TwoColumns::left
pub struct TwoColumns {
    /// The left column (empty when `left_w` is 0).
    pub left: Rect,
    right: Rect,
    id: Id,
}

impl TwoColumns {
    /// Allocates the body below the cursor across `ui`'s width: a `left_w` column, a `gap`, the rest.
    pub fn begin(ui: &mut Ui, id: Id, left_w: f32, gap: f32, min_h: f32, max_h: f32) -> Self {
        let content = ui.ctx().data(|d| d.get_temp::<f32>(id.with("two-columns-content"))).unwrap_or(max_h);
        let h = content.max(min_h).min(max_h).max(MIN_BODY);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), egui::Sense::hover());
        let left_w = left_w.clamp(0.0, rect.width());
        let left = Rect::from_min_size(rect.min, vec2(left_w, h));
        let right = Rect::from_min_max(egui::pos2((left.right() + if left_w > 0.0 { gap } else { 0.0 }).min(rect.right()), rect.top()), rect.max);
        Self { left, right, id }
    }

    /// Draws the right column, scrolling vertically when its content is taller than the body.
    pub fn right<R>(&self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
        let out = ui.scope_builder(egui::UiBuilder::new().max_rect(self.right).layout(Layout::top_down(Align::Min)), |ui| {
            egui::ScrollArea::vertical()
                .id_salt(self.id.with("two-columns-scroll"))
                .max_height(self.right.height())
                .auto_shrink([false, false])
                .show(ui, add)
        });
        let out = out.inner;
        remember(ui.ctx(), self.id.with("two-columns-content"), out.content_size.y.ceil());
        out.inner
    }
}

/// Stores a measured height; a change lays the frame out again (no window jumping a frame later).
fn remember(ctx: &egui::Context, key: Id, value: f32) {
    let value = (value * 2.0).round() / 2.0;
    if ctx.data(|d| d.get_temp::<f32>(key)) != Some(value) {
        ctx.data_mut(|d| d.insert_temp(key, value));
        ctx.request_discard("dialog layout measured");
    }
}

// ------------------------------------------------------------------------------- Form building
//
// The look every dialog shares (Settings is the reference): titled **groups** drawn as subtle inset
// panels, a two-column **form** inside them (labels right-aligned in a fixed column, controls to
// their right), checkboxes and **help** text aligned to the control column, and, for dialogs with
// categories, a **sidebar** list on the left.
//
// The Classic Settings window draws the same rows as a **plain** form ([`plain_form`]): sections
// are a bold header over a hairline (no panels), labels sit left-aligned in their column and wrap
// there, and checkboxes and help text start at the left edge, the help wrapping at a readable
// [`HELP_MAX_W`].

use egui::{Align, Layout, RichText, Shape, StrokeKind, layers::ShapeIdx, vec2};

use crate::theme::Tokens;

/// The label column of a form, in points (each form may set its own with [`form`]).
pub const LABEL_W: f32 = 150.0;
/// The gap between the label column and the control column.
pub const LABEL_GAP: f32 = 10.0;
/// How far a group's panel reaches past its content on each side.
pub const GROUP_PAD: f32 = 10.0;
/// The height of one form row.
const ROW_H: f32 = 24.0;
/// A form row's height in the Compact layout's dense look.
const DENSE_ROW_H: f32 = 20.0;
/// The longest line of help text in a [`plain_form`].
pub const HELP_MAX_W: f32 = 680.0;

/// A form row's height: [`ROW_H`], or [`DENSE_ROW_H`] in the Compact layout.
fn row_h(ui: &Ui) -> f32 {
    if Tokens::get(ui.ctx()).dense { DENSE_ROW_H } else { ROW_H }
}

/// The label column width the form in `ui` uses (set by [`form`], else [`LABEL_W`]).
pub fn label_width(ui: &Ui) -> f32 {
    ui.ctx().data(|d| d.get_temp::<f32>(Id::new("dialog-form-label-w"))).unwrap_or(LABEL_W)
}

fn set_label_width(ui: &Ui, width: f32) {
    let width = if width.is_finite() { width.clamp(40.0, 400.0) } else { LABEL_W };
    ui.ctx().data_mut(|d| d.insert_temp(Id::new("dialog-form-label-w"), width));
}

/// The x offset of the control column from the form's left edge.
pub fn control_indent(ui: &Ui) -> f32 {
    if stacked(ui) || plain(ui) {
        return 0.0;
    }
    label_width(ui) + LABEL_GAP
}

/// The form in `ui` puts each row's label above its controls (a narrow touch screen, see
/// [`stacked_form`]).
pub fn stacked(ui: &Ui) -> bool {
    ui.ctx().data(|d| d.get_temp::<bool>(Id::new("dialog-form-stacked"))).unwrap_or(false)
}

/// Controls drawn in `ui` are finger-sized (the touch Settings screen; see [`set_finger_sized`]).
pub fn finger_sized(ui: &Ui) -> bool {
    ui.ctx().data(|d| d.get_temp::<bool>(Id::new("dialog-finger-sized"))).unwrap_or(false)
}

/// Sets whether controls drawn from now on are finger-sized; returns the previous setting (put it
/// back when done).
pub fn set_finger_sized(ctx: &egui::Context, on: bool) -> bool {
    let key = Id::new("dialog-finger-sized");
    let was = ctx.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    ctx.data_mut(|d| d.insert_temp(key, on));
    was
}

/// A [`form`] for a narrow screen: each row's label sits above its controls, which take the
/// whole width, and checkboxes and help text start at the left edge. The same rows as a
/// two-column form, laid out for a phone (the touch Settings screen).
pub fn stacked_form<R>(ui: &mut Ui, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let key = Id::new("dialog-form-stacked");
    let outer = stacked(ui);
    ui.ctx().data_mut(|d| d.insert_temp(key, true));
    let out = form(ui, label_w, add);
    ui.ctx().data_mut(|d| d.insert_temp(key, outer));
    out
}

/// A [`form`] in the plain look of the Classic Settings window: no group panels, section headers
/// over a hairline, labels left-aligned (wrapping in their column, which gives up room in a narrow
/// window), and checkboxes and help text at the left edge.
pub fn plain_form<R>(ui: &mut Ui, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let key = Id::new("dialog-form-plain");
    let outer = plain(ui);
    ui.ctx().data_mut(|d| d.insert_temp(key, true));
    let out = form(ui, label_w, add);
    ui.ctx().data_mut(|d| d.insert_temp(key, outer));
    out
}

/// The form in `ui` is a [`plain_form`].
pub fn plain(ui: &Ui) -> bool {
    ui.ctx().data(|d| d.get_temp::<bool>(Id::new("dialog-form-plain"))).unwrap_or(false)
}

/// The label column of a [`plain_form`] row `room` wide: the form's own, until the window is so
/// narrow that the labels would take more than a third of it (never under 90 points).
pub fn plain_label_width(label_w: f32, room: f32) -> f32 {
    let third = if room.is_finite() { (room * 0.32).floor() } else { label_w };
    label_w.min(third).max(90.0)
}

/// One form row: `label` right-aligned in the label column, then the control(s) `add` draws.
pub fn form_row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let w = label_width(ui);
    let t = Tokens::get(ui.ctx());
    if plain(ui) {
        let label_w = plain_label_width(w, ui.available_width());
        // (the Compact layout's shorter rows too)
        let h = row_h(ui);
        return ui
            .horizontal_top(|ui| {
                ui.allocate_ui_with_layout(vec2(label_w, h), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(label_w);
                    if !label.is_empty() {
                        // (level with the text of the row-high controls beside it)
                        ui.add_space(4.0 + (h - ROW_H) / 2.0);
                        ui.add(egui::Label::new(RichText::new(crate::i18n::tr(label)).color(t.text_label)).wrap());
                    }
                });
                // a row of many choices wraps beside the label instead of widening the window
                let room = vec2(ui.available_width().max(1.0), h);
                ui.allocate_ui_with_layout(room, Layout::left_to_right(Align::Center).with_main_wrap(true), |ui| {
                    ui.set_min_height(h);
                    add(ui)
                })
                .inner
            })
            .inner;
    }
    if stacked(ui) {
        return ui
            .vertical(|ui| {
                if !label.is_empty() {
                    ui.add(egui::Label::new(RichText::new(crate::i18n::tr(label)).color(t.text_label)).wrap());
                }
                let room = vec2(ui.available_width().max(40.0), row_h(ui));
                ui.allocate_ui_with_layout(room, Layout::left_to_right(Align::Center).with_main_wrap(true), add).inner
            })
            .inner;
    }
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        ui.allocate_ui_with_layout(vec2(w, row_h(ui)), Layout::right_to_left(Align::Center), |ui| {
            ui.set_min_width(w);
            if !label.is_empty() {
                ui.add(egui::Label::new(RichText::new(crate::i18n::tr(label)).color(t.text_label)).truncate());
            }
        });
        ui.add_space((LABEL_GAP - gap).max(0.0));
        // a row of many choices wraps inside the control column instead of widening the dialog
        let room = vec2(ui.available_width().max(40.0), row_h(ui));
        ui.allocate_ui_with_layout(room, Layout::left_to_right(Align::Center).with_main_wrap(true), add).inner
    })
    .inner
}

/// Draws `add` in the control column (indented past the labels) when `ui` lays out top-down; inside
/// a row (left to right) it draws in place.
pub fn in_control_column<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    if !in_form(ui) || !ui.layout().main_dir().is_vertical() {
        return add(ui);
    }
    let indent = control_indent(ui);
    ui.horizontal_top(|ui| {
        ui.add_space(indent);
        ui.vertical(|ui| {
            // help text wraps at the panel's edge, not past it
            ui.set_max_width(ui.available_width());
            add(ui)
        })
        .inner
    })
    .inner
}

/// Help / description text: small and muted, in the control column.
pub fn help(ui: &mut Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    let text = RichText::new(crate::i18n::tr(text)).size(11.0).color(t.text_dim);
    let max = if plain(ui) { HELP_MAX_W } else { f32::INFINITY };
    in_control_column(ui, |ui| {
        ui.scope(|ui| {
            ui.set_max_width(ui.available_width().min(max));
            ui.add(egui::Label::new(text).wrap());
        });
    });
}

/// A checkbox in the control column, addressable as `check:{id}`; true when toggled.
pub fn form_check(ui: &mut Ui, id: &str, value: &mut bool, label: &str) -> bool {
    let label = crate::i18n::tr(label).to_string();
    in_control_column(ui, |ui| {
        let r = ui.checkbox(value, label);
        crate::widgets::register(ui.ctx(), format!("check:{id}"), r.rect);
        r.changed()
    })
}

/// `ui.checkbox`, in the control column in a form.
pub fn checkbox(ui: &mut Ui, value: &mut bool, text: impl Into<egui::WidgetText>) -> egui::Response {
    in_control_column(ui, |ui| ui.checkbox(value, text))
}

/// A note (`ui.label`), in the control column in a form.
pub fn note(ui: &mut Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    in_control_column(ui, |ui| ui.label(text))
}

/// A row of controls without a label of its own (buttons, links), starting at the control column
/// so it lines up with the rows above it; it wraps there instead of widening the form.
pub fn control_row<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    in_control_column(ui, |ui| ui.horizontal_wrapped(add).inner)
}

/// A link (translated `text`) in the control column; addressable as `link:{id}` when `id` is given.
pub fn link(ui: &mut Ui, id: Option<&str>, text: &str) -> egui::Response {
    let text = crate::i18n::tr(text).to_string();
    in_control_column(ui, |ui| {
        let r = ui.link(text);
        if let Some(id) = id {
            crate::widgets::register(ui.ctx(), format!("link:{id}"), r.rect);
        }
        r
    })
}

/// The open group of a form: the reserved panel shape, filled in when the group closes.
#[derive(Clone, Copy)]
struct OpenGroup {
    shape: ShapeIdx,
    top: f32,
    left: f32,
    right: f32,
}

fn group_key(ui: &Ui) -> Id {
    Id::new("dialog-open-group").with(ui.layer_id())
}

/// Starts a titled group: a heading, then an inset panel behind everything drawn until the next
/// [`group_begin`] or [`group_end`]. Lets a long, linear form be grouped without nesting closures
/// (Settings' tabs); use [`group`] where a closure is handy.
pub fn group_begin(ui: &mut Ui, title: Option<&str>) {
    group_begin_help(ui, title, None);
}

/// [`group_begin`] with a help link (`crate::help_link`, `help:{topic}`) just after the title.
pub fn group_begin_help(ui: &mut Ui, title: Option<&str>, help: Option<&str>) {
    group_end(ui);
    let t = Tokens::get(ui.ctx());
    if in_form(ui) && plain(ui) {
        // a header over a hairline across the page (no room above the page's first one)
        let first = ui.cursor().top() - ui.max_rect().top() <= 2.0;
        match title.filter(|s| !s.is_empty()) {
            Some(title) => {
                if !first {
                    ui.add_space(10.0);
                }
                let r = ui.label(RichText::new(crate::i18n::tr(title)).font(t.semibold(12.5)).color(t.text));
                if let Some(topic) = help {
                    crate::help_link::beside(ui, r.rect, topic);
                }
                let y = ui.cursor().top() - ui.spacing().item_spacing.y / 2.0;
                ui.painter().hline(ui.available_rect_before_wrap().x_range(), y, egui::Stroke::new(1.0, t.divider));
                ui.add_space(2.0);
            }
            None if !first => ui.add_space(6.0),
            None => {}
        }
        return;
    }
    if !in_form(ui) {
        if let Some(title) = title.filter(|s| !s.is_empty()) {
            let r = ui.label(RichText::new(crate::i18n::tr(title)).font(t.semibold(12.0)).color(t.text));
            if let Some(topic) = help {
                crate::help_link::beside(ui, r.rect, topic);
            }
        }
        return;
    }
    if t.dense {
        // Compact (LAYOUT-DENSE): a flat band across the form with a thin title, the rows under it
        // unboxed
        ui.add_space(2.0);
        if let Some(title) = title.filter(|s| !s.is_empty()) {
            let r = band(ui, title);
            if let Some(topic) = help {
                crate::help_link::beside(ui, r, topic);
            }
        }
    } else {
        ui.add_space(6.0);
        if let Some(title) = title.filter(|s| !s.is_empty()) {
            let r = heading(ui, title);
            if let Some(topic) = help {
                crate::help_link::beside(ui, r, topic);
            }
        }
    }
    let shape = ui.painter().add(Shape::Noop);
    let rect = ui.max_rect();
    let open = OpenGroup { shape, top: ui.cursor().top(), left: rect.left() - GROUP_PAD, right: rect.right() + GROUP_PAD };
    ui.ctx().data_mut(|d| d.insert_temp(group_key(ui), open));
    ui.add_space(4.0);
}

/// The card every dialog and tool window is drawn on: rounded, a faint light edge, a soft shadow
/// (the title goes inside it: [`card_header`]; the window has no title bar).
pub fn card_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::window(&ctx.global_style())
        .inner_margin(egui::Margin { left: 18, right: 18, top: 14, bottom: 14 })
        .corner_radius(Tokens::get(ctx).radius(16.0))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(26)))
        .shadow(egui::epaint::Shadow { offset: [0, 10], blur: 32, spread: 0, color: egui::Color32::from_black_alpha(110) })
}

/// A card's title row: the (translated) `title` left-aligned and, with `close` (a widget id), a ✕
/// on the right. True when the ✕ was clicked.
pub fn card_header(ui: &mut Ui, title: &str, close: Option<&str>) -> bool {
    let t = Tokens::get(ui.ctx());
    let mut closed = false;
    ui.horizontal(|ui| {
        let h = ui.label(RichText::new(title).font(t.semibold(16.5)).color(t.text));
        crate::widgets::register(ui.ctx(), "dialog:title", h.rect);
        if let Some(id) = close {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let b = crate::button::Button::icon_only(crate::icons::Icon::Close, "Close").kind(crate::button::Kind::Ghost).widget(id);
                closed = b.show(ui).clicked();
            });
        }
    });
    ui.add_space(4.0);
    closed
}

/// Esc for a tool window (Edit In Presets, Actions, Plug-ins): true, and the key taken, when it
/// should close: no popup is open, no text is being typed and no dialog sits over it.
pub fn esc_closes(app_dialog_open: bool, ctx: &egui::Context) -> bool {
    !app_dialog_open
        && !egui::Popup::is_any_open(ctx)
        && !ctx.egui_wants_keyboard_input()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
}

/// A group's header in the Compact layout: a flat band across the form (past the group padding),
/// a divider on top, the title at the left in small semibold text.
/// Returns the title's rect.
fn band(ui: &mut Ui, title: &str) -> Rect {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), egui::Sense::hover());
    let bar = Rect::from_min_max(egui::pos2(rect.left() - GROUP_PAD, row.top()), egui::pos2(rect.right() + GROUP_PAD, row.bottom()));
    let p = ui.painter();
    p.rect_filled(bar, 0.0, t.inset);
    p.hline(bar.x_range(), bar.top(), egui::Stroke::new(1.0, t.divider));
    let g = p.layout_no_wrap(crate::i18n::tr(title).to_string(), t.semibold(11.0), t.text_label);
    let at = egui::pos2(bar.left() + GROUP_PAD, bar.center().y - g.size().y / 2.0);
    let text = Rect::from_min_size(at, g.size());
    p.galley(at, g, t.text_label);
    crate::widgets::register(ui.ctx(), format!("formBand:{title}"), bar);
    text
}

/// A group's heading: small spaced capitals in the dim text colour, above its card. Returns its rect.
fn heading(ui: &mut Ui, title: &str) -> Rect {
    let t = Tokens::get(ui.ctx());
    let text = crate::i18n::tr(title).to_uppercase();
    let mut job = egui::text::LayoutJob::default();
    job.append(&text, 4.0, egui::TextFormat { font_id: t.semibold(10.5), color: t.text_dim, extra_letter_spacing: 1.0, ..Default::default() });
    ui.label(job).rect
}

/// Closes the open group (if any), painting its panel.
pub fn group_end(ui: &mut Ui) {
    let key = group_key(ui);
    let Some(open) = ui.ctx().data(|d| d.get_temp::<OpenGroup>(key)) else { return };
    ui.ctx().data_mut(|d| d.remove::<OpenGroup>(key));
    // nothing drawn in it (a category that starts with a titled group): no empty panel
    if ui.cursor().top() <= open.top + 4.0 + ui.spacing().item_spacing.y + 0.5 {
        return;
    }
    ui.add_space(4.0);
    let t = Tokens::get(ui.ctx());
    let bottom = ui.cursor().top() - ui.spacing().item_spacing.y + 2.0;
    let rect = Rect::from_min_max(egui::pos2(open.left, open.top), egui::pos2(open.right, bottom.max(open.top)));
    if t.dense {
        // Compact: the band above is the group's frame; its rows sit on the chrome
        ui.add_space(2.0);
        return;
    }
    // a rounded card with a faint edge
    let panel = egui::epaint::RectShape::new(rect, 12.0, t.inset, egui::Stroke::new(1.0, t.divider.gamma_multiply(0.7)), StrokeKind::Inside);
    ui.painter().set(open.shape, panel);
    ui.add_space(6.0);
}

/// A group that folds (Lightroom Classic's Export sections): a header bar across the form with a
/// triangle, `title`, and on the right a one-line `summary` of the settings inside (shown folded or
/// not), then, while `open`, an inset panel behind everything drawn until the next group. Clicking
/// the bar flips `open`; true when it did. Addressable as `section:{widget}`.
pub fn fold_group(ui: &mut Ui, widget: &str, title: &str, summary: &str, open: &mut bool) -> bool {
    group_end(ui);
    let t = Tokens::get(ui.ctx());
    let title = crate::i18n::tr(title).to_string();
    let rect = ui.max_rect();
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h(ui) + 2.0), egui::Sense::hover());
    let bar = Rect::from_min_max(egui::pos2(rect.left() - GROUP_PAD, row.top()), egui::pos2(rect.right() + GROUP_PAD, row.bottom()));
    let resp = ui.interact(bar, ui.id().with(("fold-group", widget)), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, *open, &title));
    crate::widgets::register(ui.ctx(), format!("section:{widget}"), bar);
    let toggled = resp.clicked();
    if toggled {
        *open = !*open;
    }
    let p = ui.painter();
    let fill = if resp.hovered() { t.hover } else { t.inset };
    p.rect(bar, t.radius(4.0), fill, egui::Stroke::new(1.0, t.divider), StrokeKind::Inside);
    // ▶ folded, ▼ open
    let c = egui::pos2(bar.left() + 14.0, bar.center().y);
    let pts = if *open {
        vec![c + vec2(-4.0, -2.5), c + vec2(4.0, -2.5), c + vec2(0.0, 3.0)]
    } else {
        vec![c + vec2(-2.5, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.5, 4.0)]
    };
    p.add(Shape::convex_polygon(pts, t.text_label, egui::Stroke::NONE));
    let title_g = p.layout_no_wrap(title, t.semibold(12.0), t.text);
    let title_x = bar.left() + 26.0;
    let title_w = title_g.size().x;
    p.galley(egui::pos2(title_x, bar.center().y - title_g.size().y / 2.0), title_g, t.text);
    // the summary takes the room left on the right, cut short with … rather than over the title
    let room = (bar.right() - 10.0 - (title_x + title_w + 16.0)).max(0.0);
    if !summary.is_empty() && room > 24.0 {
        let mut job = egui::text::LayoutJob::simple_singleline(summary.to_string(), egui::FontId::proportional(11.5), t.text_dim);
        job.wrap = egui::text::TextWrapping::truncate_at_width(room);
        let g = p.layout_job(job);
        p.galley(egui::pos2(bar.right() - 10.0 - g.size().x, bar.center().y - g.size().y / 2.0), g, t.text_dim);
    }
    if *open {
        let shape = ui.painter().add(Shape::Noop);
        let open_group = OpenGroup { shape, top: ui.cursor().top(), left: rect.left() - GROUP_PAD, right: rect.right() + GROUP_PAD };
        ui.ctx().data_mut(|d| d.insert_temp(group_key(ui), open_group));
        ui.add_space(4.0);
    } else {
        ui.add_space(2.0);
    }
    toggled
}

/// A titled group around `add` (see [`group_begin`]).
pub fn group<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    group_begin(ui, Some(title));
    let out = add(ui);
    group_end(ui);
    out
}

/// A form body: room on both sides for the group panels, and the label column width.
pub fn form<R>(ui: &mut Ui, label_w: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    set_label_width(ui, label_w);
    let active = Id::new("dialog-form-active");
    let outer = in_form(ui);
    ui.ctx().data_mut(|d| d.insert_temp(active, true));
    // (a plain form has no panels to make room for)
    let pad = if plain(ui) { 0 } else { GROUP_PAD as i8 + 1 };
    egui::Frame::NONE
        .inner_margin(egui::Margin { left: pad, right: pad, top: 0, bottom: 0 })
        .show(ui, |ui| {
            let out = add(ui);
            group_end(ui);
            ui.ctx().data_mut(|d| d.insert_temp(active, outer));
            out
        })
        .inner
}

/// Whether `ui` is drawing inside a [`form`] (its helpers then lay out as a form; elsewhere, e.g.
/// in a side panel, they keep to the panel's own flow).
pub fn in_form(ui: &Ui) -> bool {
    ui.ctx().data(|d| d.get_temp::<bool>(Id::new("dialog-form-active"))).unwrap_or(false)
}

/// The category list on the left of a dialog with sections (Settings): one full-width row per
/// `(id, label)`, the selected one highlighted, each addressable as `button:{prefix}-{id}`. True
/// when the selection changed.
pub fn sidebar(ui: &mut Ui, prefix: &str, items: &[(&str, &str)], selected: &mut String, width: f32) -> bool {
    let t = Tokens::get(ui.ctx());
    let mut changed = false;
    ui.vertical(|ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 1.0;
        for (id, label) in items {
            let label = crate::i18n::tr(label);
            let on = selected == id;
            let (rect, resp) = ui.allocate_exact_size(vec2(width, 26.0), egui::Sense::click());
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, label));
            crate::widgets::register(ui.ctx(), format!("button:{prefix}-{id}"), rect);
            let fill = if on {
                t.pressed
            } else if resp.hovered() {
                t.hover
            } else {
                egui::Color32::TRANSPARENT
            };
            let p = ui.painter();
            p.rect_filled(rect, 4.0, fill);
            if on {
                let bar = Rect::from_min_size(rect.left_top() + vec2(0.0, 5.0), vec2(3.0, rect.height() - 10.0));
                p.rect_filled(bar, 1.5, t.accent);
            }
            let font = if on { t.semibold(12.5) } else { egui::FontId::proportional(12.5) };
            let galley = p.layout_no_wrap(label.to_string(), font, if on { t.text } else { t.text_label });
            let pos = egui::pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0);
            p.with_clip_rect(rect.intersect(p.clip_rect())).galley(pos, galley, t.text);
            if resp.clicked() && !on {
                *selected = id.to_string();
                changed = true;
            }
        }
    });
    changed
}

/// A thin rule across the dialog above its footer buttons.
pub fn footer_rule(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width().max(ui.min_rect().width());
    let (rect, _) = ui.allocate_exact_size(vec2(w, 1.0), egui::Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, egui::Stroke::new(1.0, t.divider));
}

/// Whether `dialog` is shown as a bottom sheet: on a phone (Android with the window's short side
/// under 600 points, or any window under 600 points wide), every dialog but the full-window screens
/// (Settings, Import, Export) and Print. A short, wide desktop window keeps centred cards.
pub fn phone_sheet(ctx: &egui::Context, dialog: &crate::state::Dialog) -> bool {
    use crate::state::Dialog;
    let size = ctx.content_rect().size();
    let phone = size.x < 600.0 || (cfg!(target_os = "android") && size.x.min(size.y) < 600.0);
    phone && !matches!(dialog, Dialog::Settings { .. } | Dialog::Import { .. } | Dialog::Export { .. } | Dialog::Print)
}

/// A bottom sheet's grab handle across its top (`dialog:grab`): true when it was tapped or pulled
/// down, which closes the sheet (as Cancel).
pub fn grab_handle(ui: &mut Ui) -> bool {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), egui::Sense::click_and_drag());
    crate::widgets::register(ui.ctx(), "dialog:grab", r);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, crate::i18n::tr("Close")));
    ui.painter().rect_filled(Rect::from_center_size(r.center(), vec2(40.0, 5.0)), 2.5, t.text_dim.gamma_multiply(0.7));
    let pulled = resp.drag_stopped()
        && ui.input(|i| match (i.pointer.press_origin(), i.pointer.interact_pos()) {
            (Some(a), Some(b)) => b.y - a.y > 40.0,
            _ => false,
        });
    resp.clicked() || pulled
}
