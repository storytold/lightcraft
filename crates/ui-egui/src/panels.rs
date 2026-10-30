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

/// The space between two toasts on screen.
const TOAST_GAP: f32 = 8.0;

/// The HUD toasts at the bottom centre of the canvas: the newest where a toast always was, the older ones above it,
/// each fading out at its own time (widgets `toast:0` for the newest, `toast:1` above it…).
pub fn toast(app: &mut LightcraftApp, ctx: &egui::Context) {
    let now = app.tick_now();
    app.ui.expire_toasts(now);
    if app.ui.toasts.is_empty() {
        return;
    }
    let Some(canvas) = app.canvas_rect else { return };
    let t = Tokens::get(ctx);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("toast")));
    // long messages (a save warning, the web build's notices) wrap within the canvas
    let wrap = (canvas.width() - 80.0).clamp(200.0, 720.0);
    // a slideshow's playback controls sit at the bottom: the toasts go to the top there, clear of them, the newest
    // on top; elsewhere the newest is at the bottom and the older ones stack above it
    let top = app.ui.slideshow.is_some();
    // the edge the next one goes against; the newest has none: it is centred where the single toast always was
    let mut edge: Option<f32> = None;
    let x = canvas.center().x;
    // the widget ids count the toasts drawn, so `toast:1` is always the one above `toast:0` even when one between is skipped
    let mut shown = 0;
    for (i, (text, until, label, _)) in app.ui.toasts.iter().rev().enumerate() {
        let fade = ((until - now) / 0.25).clamp(0.0, 1.0) as f32;
        let (background, foreground) = label.map(crate::theme::label_toast_colors).unwrap_or((egui::Color32::from_black_alpha(200), t.text));
        let foreground = foreground.gamma_multiply(fade);
        let galley = painter.layout(text.clone(), t.font(14.0), foreground, wrap);
        let size = vec2(galley.size().x + 40.0, (galley.size().y + 22.0).max(40.0));
        let r = match (edge, top) {
            (None, true) => Rect::from_center_size(pos2(x, canvas.top() + 60.0), size),
            (None, false) => Rect::from_center_size(pos2(x, canvas.bottom() - 60.0), size),
            (Some(y), true) => Rect::from_min_size(pos2(x - size.x / 2.0, y), size),
            (Some(y), false) => Rect::from_min_size(pos2(x - size.x / 2.0, y - size.y), size),
        };
        // the older ones never go past the canvas: a burst of long messages keeps those that fit (the newest is drawn
        // as the single toast always was, whatever room there is); one too tall is skipped, and a shorter older one may still fit
        if i > 0 && !canvas.contains_rect(r) {
            continue;
        }
        edge = Some(if top { r.bottom() + TOAST_GAP } else { r.top() - TOAST_GAP });
        painter.rect_filled(r, 6.0, background.gamma_multiply(fade));
        painter.galley(r.center() - galley.size() / 2.0, galley, foreground);
        crate::widgets::register_geometry(ctx, format!("toast:{shown}"), r);
        shown += 1;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(30));
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
