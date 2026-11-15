//! Window regions and panels.

pub mod activity;
pub mod ai_models;
pub mod alternatives;
pub mod bottombar;
pub mod cells;
pub mod chips;
pub mod classic;
pub mod collections;
pub mod compare;
#[cfg(not(target_arch = "wasm32"))]
pub mod connections;
pub mod controls;
pub mod crop_overlay;
pub mod denoise;
pub mod detail;
pub mod dialogs;
pub mod edit;
pub mod empty_library;
pub mod face_preview;
pub mod faces;
pub mod filterbar;
pub mod folders;
pub mod genfill;
pub mod grid;
pub mod histogram;
pub mod index_controls;
pub mod keymap;
pub mod keyword_list;
pub mod keywording;
pub mod layout_overlay;
pub mod left;
pub mod levels;
pub mod library_problem;
pub mod manager_table;
pub mod mask_effects;
pub mod mask_pickers;
pub mod masking;
pub mod metadata;
pub mod ml_models;
pub mod navigator;
pub mod notices;
#[cfg(not(target_arch = "wasm32"))]
pub mod peer_sync;
pub mod people;
pub mod person;
pub mod plugin_filters;
#[cfg(not(target_arch = "wasm32"))]
pub mod plugin_icon;
#[cfg(not(target_arch = "wasm32"))]
pub mod plugins;
pub mod presets;
pub mod presets_manager;
pub mod profile_manager;
pub mod profiles;
#[cfg(not(target_arch = "wasm32"))]
pub mod publish;
pub mod quick_develop;
pub mod range_pick;
pub mod retouch;
pub mod right;
pub mod rules_editor;
pub mod rules_move;
pub mod second;
pub mod settings;
pub mod settings_develop;
pub mod settings_folders;
pub mod settings_profiles;
pub mod slide_layout;
pub mod slide_overlays;
pub mod slide_templates;
pub mod slide_titles;
pub mod slideshow;
pub mod strip;
pub mod surface_osd;
#[cfg(not(target_arch = "wasm32"))]
pub mod sync_pill;
#[cfg(not(target_arch = "wasm32"))]
pub mod tether_bar;
pub mod topbar;
pub mod unnamed;
#[cfg(not(target_arch = "wasm32"))]
pub mod update;
pub mod witchcraft;

use egui::{Align2, Rect, pos2, vec2};

use crate::LccApp;
use crate::theme::Tokens;

/// Show a side panel the user resizes by dragging its inner edge. `width` (in [`crate::UiState`],
/// so it survives navigation and restarts) is the source of truth: egui's own remembered size is
/// dropped every frame, and a drag of the edge returns the new width to store. The panel never
/// takes more than `available − reserve − MIN_PHOTO_WIDTH` (its minimum permitting), so the photo
/// stays usable; a narrower window shrinks it without forgetting the chosen width.
pub fn resizable_side(
    ui: &mut egui::Ui,
    left: bool,
    id: &str,
    frame: egui::Frame,
    width: f32,
    limits: crate::state::PanelWidth,
    reserve: f32,
    add: impl FnOnce(&mut egui::Ui),
) -> Option<f32> {
    let pid = egui::Id::new(id);
    ui.ctx().data_mut(|d| d.remove::<egui::containers::panel::PanelState>(pid));
    let avail = ui.available_rect_before_wrap();
    let max = (avail.width() - reserve - crate::state::MIN_PHOTO_WIDTH).clamp(limits.min, limits.max);
    let panel = if left { egui::Panel::left(pid) } else { egui::Panel::right(pid) };
    let r = panel.frame(frame).resizable(true).size_range(limits.min..=max).default_size(limits.clamp(width).min(max)).show(ui, add);
    crate::widgets::register(ui.ctx(), format!("panel:{id}"), r.response.rect);
    // the edge egui draws for resizing takes the pointer: screen readers get its name
    let edge = if left { "Resize the left panel" } else { "Resize the right panel" };
    crate::widgets::screen_reader_name(ui.ctx(), pid.with("__resize"), crate::i18n::tr(edge));
    // egui names the edge's drag widget `<panel id>.with("__resize")`; the width is measured from
    // the panel's fixed edge to the pointer (contents wider than the panel don't count)
    if !ui.ctx().is_being_dragged(pid.with("__resize")) {
        return None;
    }
    let x = ui.ctx().pointer_interact_pos()?.x;
    let w = if left { x - avail.left() } else { avail.right() - x };
    Some(w.round().clamp(limits.min, max))
}

/// Split a side column off `area` for a module that lays out its own sides (Map, Book, Print,
/// Web, the Import tab): the column takes `width` (clamped to `limits`, and leaving the photo area
/// [`crate::state::MIN_PHOTO_WIDTH`] where it can) from the left or right of `area`, which shrinks
/// to the rest. Its inner edge is dragged to resize it, as the Library and Develop panels are; a
/// drag writes the new width back to `width` (the shared [`crate::UiState`] width, so the column
/// keeps its size across modules and restarts). Returns the column.
pub fn split_side(ui: &mut egui::Ui, area: &mut Rect, left: bool, id: &str, width: &mut f32, limits: crate::state::PanelWidth) -> Rect {
    let room = area.width().max(0.0);
    let max = (room - crate::state::MIN_PHOTO_WIDTH).clamp(limits.min, limits.max).min(room * 0.5).max(0.0);
    let w = limits.clamp(*width).min(max);
    let col = if left {
        Rect::from_min_max(area.min, pos2(area.left() + w, area.bottom()))
    } else {
        Rect::from_min_max(pos2(area.right() - w, area.top()), area.max)
    };
    if left {
        area.min.x = col.right();
    } else {
        area.max.x = col.left();
    }
    let edge_x = if left { col.right() } else { col.left() };
    let handle = Rect::from_min_max(pos2(edge_x - 3.0, col.top()), pos2(edge_x + 3.0, col.bottom()));
    let pid = egui::Id::new(id).with("__resize");
    let r = ui.interact(handle, pid, egui::Sense::drag());
    crate::widgets::register(ui.ctx(), format!("panel:{id}"), col);
    let name = if left { "Resize the left panel" } else { "Resize the right panel" };
    crate::widgets::screen_reader_name(ui.ctx(), pid, crate::i18n::tr(name));
    if r.hovered() || r.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        let t = Tokens::get(ui.ctx());
        ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, pid)).vline(edge_x, col.y_range(), egui::Stroke::new(2.0, t.accent));
    }
    if r.dragged()
        && let Some(x) = ui.ctx().pointer_interact_pos()
    {
        let outer = if left { x.x - col.left() } else { col.right() - x.x };
        if outer.is_finite() {
            *width = outer.round().clamp(limits.min, max.max(limits.min));
        }
    }
    col
}

/// The HUD toasts at the bottom centre of the canvas: the newest where a single toast always was, the older ones
/// stacked above it (below it during a slideshow, whose toasts sit at the top), each fading out at its own time.
pub fn toast(app: &mut LccApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    app.ui.expire_toasts(now);
    let action_h = toast_action(app, ctx, now);
    if app.ui.toasts.is_empty() {
        return;
    }
    let Some(canvas) = app.canvas_rect else { return };
    let t = Tokens::get(ctx);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("toast")));
    // long messages (a save warning, the web build's notices) wrap within the canvas
    let wrap = (canvas.width() - 80.0).clamp(200.0, 720.0);
    // a slideshow's playback controls sit at the bottom: the toasts go to the top there, clear of them
    let top = app.ui.slideshow.is_some();
    let mut edge = if top { canvas.top() + 40.0 + action_h } else { canvas.bottom() - 40.0 - action_h };
    for (text, until, label) in app.ui.toasts.iter().rev() {
        let fade = ((until - now) / 0.25).clamp(0.0, 1.0) as f32;
        let (background, foreground) = label.map(crate::theme::label_toast_colors).unwrap_or((egui::Color32::from_black_alpha(200), t.text));
        let foreground = foreground.gamma_multiply(fade);
        let galley = painter.layout(text.clone(), t.font(14.0), foreground, wrap);
        let size = vec2(galley.size().x + 40.0, (galley.size().y + 22.0).max(40.0));
        let r = if top {
            Rect::from_min_size(pos2(canvas.center().x - size.x / 2.0, edge), size)
        } else {
            Rect::from_min_size(pos2(canvas.center().x - size.x / 2.0, edge - size.y), size)
        };
        // never past the view
        if r.top() < canvas.top() || r.bottom() > canvas.bottom() {
            break;
        }
        painter.rect_filled(r, 6.0, background.gamma_multiply(fade));
        painter.galley(r.center() - galley.size() / 2.0, galley, foreground);
        edge = if top { r.bottom() + 8.0 } else { r.top() - 8.0 };
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(30));
}

/// The toast with a button ([`crate::state::ToastAction`]), in the toasts' look, at the bottom
/// centre of the canvas (top during a slideshow) with the others stacked past it. A click runs its
/// command once. Returns the room it takes (0 without one).
fn toast_action(app: &mut LccApp, ctx: &egui::Context, now: f64) -> f32 {
    if app.ui.toast_action.as_ref().is_some_and(|a| a.until < now) {
        app.ui.toast_action = None;
    }
    let (Some(a), Some(canvas)) = (app.ui.toast_action.clone(), app.canvas_rect) else { return 0.0 };
    let t = Tokens::get(ctx);
    let fade = ((a.until - now) / 0.25).clamp(0.0, 1.0) as f32;
    let top = app.ui.slideshow.is_some();
    let (pivot, anchor) = if top {
        (egui::Align2::CENTER_TOP, pos2(canvas.center().x, canvas.top() + 40.0))
    } else {
        (egui::Align2::CENTER_BOTTOM, pos2(canvas.center().x, canvas.bottom() - 40.0))
    };
    let mut clicked = false;
    let shown = egui::Area::new(egui::Id::new("toast-action")).order(egui::Order::Foreground).pivot(pivot).fixed_pos(anchor).show(ctx, |ui| {
        egui::Frame::NONE
            .fill(egui::Color32::from_black_alpha(200).gamma_multiply(fade))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(20, 9))
            .show(ui, |ui| {
                ui.set_max_width((canvas.width() - 80.0).clamp(200.0, 720.0));
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&a.text).font(t.font(14.0)).color(t.text.gamma_multiply(fade)));
                    ui.add_space(8.0);
                    let b = ui.button(egui::RichText::new(&a.button).font(t.semibold(13.0)));
                    crate::widgets::register(ui.ctx(), "button:toastAction", b.rect);
                    clicked = b.clicked();
                });
            })
    });
    if clicked {
        app.ui.toast_action = None;
        match app.run(&a.command, a.params.clone()) {
            Ok(_) => {}
            Err(e) => app.toast_error(ctx, e),
        }
        return 0.0;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(30));
    shown.response.rect.height() + 8.0
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
    empty_state_sized(ui, rect, title, body, actions, hint, false)
}

/// [`empty_state`]; `touch` makes the buttons finger-sized and stacks them one per row, all of
/// one width (the Modern layout).
pub fn empty_state_sized(
    ui: &mut egui::Ui,
    rect: Rect,
    title: &str,
    body: &str,
    actions: &[EmptyAction],
    hint: Option<&str>,
    touch: bool,
) -> Option<&'static str> {
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
    let (button_h, gap, label_size, min_w): (f32, f32, f32, f32) = if touch { (52.0, 12.0, 16.0, 260.0) } else { (30.0, 8.0, 13.0, 0.0) };
    let widths: Vec<f32> = actions
        .iter()
        .map(|a| (ui.painter().layout_no_wrap(crate::i18n::tr(a.label).to_string(), t.semibold(label_size), t.text).size().x + 32.0).max(min_w))
        .collect();
    let row_w = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    let side_by_side = !touch && row_w <= wrap;
    let buttons_h = match (actions.len(), side_by_side) {
        (0, _) => 0.0,
        (_, true) => button_h,
        (n, false) => n as f32 * button_h + (n - 1) as f32 * gap,
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
            let r = Rect::from_min_size(pos2(x, buttons_top), vec2(*w, button_h));
            x += w + gap;
            r
        } else {
            let w = widths.iter().copied().fold(0.0, f32::max).min(wrap);
            Rect::from_min_size(pos2(rect.center().x - w / 2.0, buttons_top + i as f32 * (button_h + gap)), vec2(w, button_h))
        };
        let label = egui::RichText::new(crate::i18n::tr(a.label)).font(t.semibold(label_size));
        let button =
            if i == 0 { egui::Button::new(label.color(egui::Color32::WHITE)).fill(t.accent) } else { egui::Button::new(label.color(t.text)) };
        let resp = ui.put(r, button);
        crate::widgets::register(ui.ctx(), a.widget, resp.rect);
        if resp.clicked() {
            clicked = Some(a.command);
        }
    }
    if let Some(g) = hint {
        p.galley(pos2(rect.center().x, buttons_top + buttons_h + 12.0), g, t.text_dim);
    }
    clicked
}
