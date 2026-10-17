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

/// Stores a measured height; a change lays the frame out again (no window jumping a frame later).
fn remember(ctx: &egui::Context, key: Id, value: f32) {
    let value = (value * 2.0).round() / 2.0;
    if ctx.data(|d| d.get_temp::<f32>(key)) != Some(value) {
        ctx.data_mut(|d| d.insert_temp(key, value));
        ctx.request_discard("dialog layout measured");
    }
}
