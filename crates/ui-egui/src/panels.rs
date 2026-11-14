//! Window regions and panels.

pub mod activity;
pub mod bottombar;
pub mod chips;
pub mod compare;
pub mod crop_overlay;
pub mod denoise;
pub mod detail;
pub mod edit;
pub mod empty_library;
pub mod filterbar;
pub mod grid;
pub mod histogram;
pub mod keyword_list;
pub mod keywording;
pub mod layout_overlay;
pub mod left;
pub mod library_problem;
pub mod mask_effects;
pub mod masking;
pub mod notices;
pub mod people;
pub mod person;
pub mod presets;
pub mod profiles;
pub mod right;
pub mod second;
pub mod slideshow;
pub mod strip;
pub mod topbar;
pub mod unnamed;

use egui::{Align2, Rect, pos2, vec2};

use crate::LightcraftApp;
pub use crate::dialogs;
pub use crate::dialogs::faces;
pub use crate::dialogs::keymap;
pub use crate::dialogs::rules_editor;
pub use crate::dialogs::rules_editor::rules_move;
pub use crate::dialogs::settings;
pub use crate::dialogs::settings::slide_layout;
pub use crate::dialogs::settings::slide_overlays;
pub use crate::dialogs::settings::slide_templates;
pub use crate::dialogs::settings::slide_titles;
use crate::theme::Tokens;

/// A side panel the user resizes: which side, how it is drawn and how wide it may be.
pub struct SidePanel<'a> {
    /// the left side (else the right)
    pub left: bool,
    pub id: &'a str,
    pub frame: egui::Frame,
    /// the width the user chose
    pub width: f32,
    pub limits: crate::state::PanelWidth,
    /// room to leave for what is beside the photo
    pub reserve: f32,
}

/// Show a side panel the user resizes by dragging its inner edge. `width` (in [`crate::UiState`],
/// so it survives navigation and restarts) is the source of truth: egui's own remembered size is
/// dropped every frame, and a drag of the edge returns the new width to store. The panel never
/// takes more than `available − reserve − MIN_PHOTO_WIDTH` (its minimum permitting), so the photo
/// stays usable; a narrower window shrinks it without forgetting the chosen width.
pub fn resizable_side(ui: &mut egui::Ui, side: SidePanel, add: impl FnOnce(&mut egui::Ui)) -> Option<f32> {
    let SidePanel { left, id, frame, width, limits, reserve } = side;
    // `Panel::new` stores `IdSalt::new(id)`; the panel's persisted key is
    // `parent_ui.scope_id().with_salt(that_salt)`, which is exactly `scope_id().with(id)`.
    // (Passing an already-built `Id` through `Panel::left` would salt it a second time.)
    let pid = ui.scope_id().with(id);
    ui.ctx().data_mut(|d| d.remove::<egui::containers::panel::PanelState>(pid));
    let avail = ui.available_rect_before_wrap();
    let max = (avail.width() - reserve - crate::state::MIN_PHOTO_WIDTH).clamp(limits.min, limits.max);
    let panel = if left { egui::Panel::left(id) } else { egui::Panel::right(id) };
    let r = panel.frame(frame).resizable(true).size_range(limits.min..=max).default_size(limits.clamp(width).min(max)).show(ui, add);
    crate::widgets::register(ui, format!("panel:{id}"), r.response.rect);
    // egui names the edge's drag widget `<panel id>.with("__resize")`; the width is measured from
    // the panel's fixed edge to the pointer (contents wider than the panel don't count)
    if !ui.ctx().is_being_dragged(pid.with("__resize")) {
        return None;
    }
    let x = ui.ctx().pointer_interact_pos()?.x;
    let w = if left { x - avail.left() } else { avail.right() - x };
    Some(w.round().clamp(limits.min, max))
}

/// The toast's geometry: how its text wraps within the canvas, its padding, and how far from
/// the canvas edge it sits (from the top in a slideshow, clear of the playback controls).
const TOAST_WRAP_MARGIN: f32 = 80.0;
const TOAST_WRAP_MIN: f32 = 200.0;
const TOAST_WRAP_MAX: f32 = 720.0;
const TOAST_PAD_X: f32 = 40.0;
const TOAST_PAD_Y: f32 = 22.0;
const TOAST_MIN_HEIGHT: f32 = 40.0;
const TOAST_EDGE_OFFSET: f32 = 60.0;
/// The toast's text size, background opacity, corner radius, the fade-out ramp before it
/// expires, and how soon the next frame is asked for while it is shown.
const TOAST_FONT: f32 = 14.0;
const TOAST_BACKGROUND_ALPHA: u8 = 200;
const TOAST_CORNER: f32 = 6.0;
const TOAST_FADE_SECONDS: f64 = 0.25;
const TOAST_REPAINT_MS: u64 = 30;

/// The HUD toast at the bottom centre of the canvas. A press on it dismisses it (its rectangle
/// is registered as `toast` for automation); the press itself passes through to whatever the
/// canvas holds underneath, as it always has (#3050).
pub fn toast(app: &mut LightcraftApp, ctx: &egui::Context) {
    let now = app.tick_now();
    let Some((text, until, label)) = app.ui.toast.clone() else { return };
    if now > until {
        app.ui.toast = None;
        return;
    }
    let Some(canvas) = app.canvas_rect else { return };
    // a slideshow's playback controls sit at the bottom: the toast goes to the top there, clear of them
    let y = if app.ui.slideshow.is_some() { canvas.top() + TOAST_EDGE_OFFSET } else { canvas.bottom() - TOAST_EDGE_OFFSET };
    let fade = ((until - now) / TOAST_FADE_SECONDS).clamp(0.0, 1.0) as f32;
    let (galley, rect, background, foreground) = toast_layout(ctx, text, label, fade, canvas, y);
    toast_paint(ctx, &galley, rect, background, foreground);
    crate::widgets::register_geometry(ctx, "toast", rect);
    // A press inside the rectangle dismisses the toast. The event is not consumed: the press
    // still reaches what is under it (a dialog control may share the rectangle), so nothing the
    // input does changes — only the toast disappears (#3050).
    let pressed_on_toast =
        ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary) && i.pointer.latest_pos().is_some_and(|p| rect.contains(p)));
    if pressed_on_toast {
        app.ui.toast = None;
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(TOAST_REPAINT_MS));
}

/// The toast's text wrapped and faded for this frame, and the rectangle it fills on `canvas`,
/// centred at `y`: the galley to paint, the rectangle, and the colours behind and in it.
fn toast_layout(
    ctx: &egui::Context,
    text: String,
    label: Option<lightcraft_catalog::ColorLabel>,
    fade: f32,
    canvas: Rect,
    y: f32,
) -> (std::sync::Arc<egui::epaint::Galley>, Rect, egui::Color32, egui::Color32) {
    let t = Tokens::get(ctx);
    // long messages (a save warning, the web build's notices) wrap within the canvas
    let wrap = (canvas.width() - TOAST_WRAP_MARGIN).clamp(TOAST_WRAP_MIN, TOAST_WRAP_MAX);
    let (background, foreground) =
        label.map(crate::theme::label_toast_colors).unwrap_or((egui::Color32::from_black_alpha(TOAST_BACKGROUND_ALPHA), t.text));
    let foreground = foreground.gamma_multiply(fade);
    let background = background.gamma_multiply(fade);
    let galley = ctx.fonts_mut(|f| f.layout(text, t.font(TOAST_FONT), foreground, wrap));
    let size = vec2(galley.size().x + TOAST_PAD_X, (galley.size().y + TOAST_PAD_Y).max(TOAST_MIN_HEIGHT));
    (galley, Rect::from_center_size(pos2(canvas.center().x, y), size), background, foreground)
}

/// The toast's laid-out text painted at `rect` on the Foreground layer.
fn toast_paint(ctx: &egui::Context, galley: &std::sync::Arc<egui::epaint::Galley>, rect: Rect, background: egui::Color32, foreground: egui::Color32) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("toast")));
    painter.rect_filled(rect, TOAST_CORNER, background);
    painter.galley(rect.center() - galley.size() / 2.0, galley.clone(), foreground);
}

/// Paint a centred, dimmed message (empty states).
pub fn empty_message(ui: &egui::Ui, rect: Rect, title: &str, body: &str) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter();
    p.text(rect.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, crate::i18n::tr(title), t.semibold(18.0), t.text_label);
    p.text(rect.center() + vec2(0.0, 14.0), Align2::CENTER_CENTER, crate::i18n::tr(body), t.font(13.0), t.text_dim);
}

/// A button under an empty state's message: the command it runs, its label, and the id it is
/// registered under (`button:…`) for scripts and tests.
pub struct EmptyAction {
    pub command: &'static str,
    pub label: &'static str,
    pub widget: &'static str,
}

/// [`empty_message`] with the next step under it: a row of buttons (the first is the primary
/// one, filled with the accent) and an optional dimmed hint line below them. The block is
/// centred in `rect` as one. Returns the command of the button clicked this frame.
pub fn empty_state(ui: &mut egui::Ui, rect: Rect, title: &str, body: &str, actions: &[EmptyAction], hint: Option<&str>) -> Option<&'static str> {
    let t = Tokens::get(ui.ctx());
    let wrap = (rect.width() - 48.0).max(80.0);
    let centred = |ui: &egui::Ui, text: &str, font: egui::FontId, color: egui::Color32| {
        let mut job = egui::text::LayoutJob::simple(text.to_string(), font, color, wrap);
        job.halign = egui::Align::Center;
        ui.painter().layout_job(job)
    };
    let title = centred(ui, crate::i18n::tr(title), t.semibold(18.0), t.text_label);
    let body = centred(ui, crate::i18n::tr(body), t.font(13.0), t.text_dim);
    let hint = hint.map(|h| centred(ui, h, t.font(12.0), t.text_dim));
    // the buttons' widths, and whether they fit side by side (else one above the other)
    const BUTTON_H: f32 = 30.0;
    const GAP: f32 = 8.0;
    let widths: Vec<f32> =
        actions.iter().map(|a| ui.painter().layout_no_wrap(crate::i18n::tr(a.label).to_string(), t.semibold(13.0), t.text).size().x + 32.0).collect();
    let row_w = widths.iter().sum::<f32>() + GAP * widths.len().saturating_sub(1) as f32;
    let side_by_side = row_w <= wrap;
    let buttons_h = match (actions.len(), side_by_side) {
        (0, _) => 0.0,
        (_, true) => BUTTON_H,
        (n, false) => n as f32 * BUTTON_H + (n - 1) as f32 * GAP,
    };
    let gap_above_buttons = if actions.is_empty() { 0.0 } else { 20.0 };
    let hint_h = hint.as_ref().map_or(0.0, |g| 12.0 + g.size().y);
    let block = title.size().y + 6.0 + body.size().y + gap_above_buttons + buttons_h + hint_h;
    let top = rect.center().y - block / 2.0;
    let p = ui.painter().clone();
    let body_top = top + title.size().y + 6.0;
    let buttons_top = body_top + body.size().y + gap_above_buttons;
    p.galley(pos2(rect.center().x, top), title, t.text_label);
    p.galley(pos2(rect.center().x, body_top), body, t.text_dim);
    let mut clicked = None;
    let mut x = rect.center().x - row_w / 2.0;
    for (i, (a, w)) in actions.iter().zip(&widths).enumerate() {
        let r = if side_by_side {
            let r = Rect::from_min_size(pos2(x, buttons_top), vec2(*w, BUTTON_H));
            x += w + GAP;
            r
        } else {
            let w = widths.iter().copied().fold(0.0, f32::max).min(wrap);
            Rect::from_min_size(pos2(rect.center().x - w / 2.0, buttons_top + i as f32 * (BUTTON_H + GAP)), vec2(w, BUTTON_H))
        };
        let label = egui::RichText::new(crate::i18n::tr(a.label)).font(t.semibold(13.0));
        let button =
            if i == 0 { egui::Button::new(label.color(egui::Color32::WHITE)).fill(t.accent) } else { egui::Button::new(label.color(t.text)) };
        let resp = ui.put(r, button);
        crate::widgets::register(ui, a.widget, resp.rect);
        if resp.clicked() {
            clicked = Some(a.command);
        }
    }
    if let Some(g) = hint {
        p.galley(pos2(rect.center().x, buttons_top + buttons_h + 12.0), g, t.text_dim);
    }
    clicked
}
