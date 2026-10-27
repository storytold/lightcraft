//! The compact layout: phones, and any window narrower than [`COMPACT_WIDTH`] (iPad Split View, a
//! phone browser) or shorter than [`COMPACT_HEIGHT`] and narrower than [`COMPACT_SHORT_WIDTH`]
//! (a phone held sideways). The desktop
//! layout's side columns and filmstrip leave such a window no room for the photo, so here:
//!
//! - the top bar keeps the sidebar toggle, Back, the menus (one "Menu" button), a search field
//!   that takes the free width, Filter and Export;
//! - the tool strip goes; its tools sit in a row atop a **bottom sheet** under the photo, which
//!   holds the right panel (Edit, Crop, Masking …) or the Presets column at full width; in a
//!   window wider than tall ([`beside`]) the sheet is a column at the right of the photo instead;
//! - My Photos (the left panel) opens as a full-width drawer (a column at the left in a wide
//!   window), closed again by picking a source;
//! - the bottom bar keeps the view modes, Sort and an Edit toggle.
//!
//! The panels themselves are the desktop ones: [`super::resizable_side`] and the Presets column
//! fill the region they're given while [`is_compact`] holds.

use egui::{Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::state::{RightPanel, ViewMode};
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

/// Content width (points, inside the safe area) below which the compact layout is used.
pub const COMPACT_WIDTH: f32 = 600.0;

/// Content height (points, inside the safe area) below which the compact layout is used: a phone
/// in landscape (an iPhone is about 400 pt tall, an Android phone about 410 pt less its status
/// bar). The smallest iPad in landscape is 744 pt tall.
pub const COMPACT_HEIGHT: f32 = 500.0;

/// A short window is compact only when it is also narrower than this: phones in landscape are at
/// most about 960 pt wide, while a short but wide desktop window keeps the desktop layout and its
/// menu bar.
pub const COMPACT_SHORT_WIDTH: f32 = 1000.0;

/// Width of the sheet and the My Photos drawer when they sit beside the photo ([`beside`]).
const SIDE_MIN: f32 = 300.0;
const SIDE_MAX: f32 = 420.0;

/// Side margin of the compact top bar, bottom bar and the sheet's tool row, so their icon
/// columns line up.
const BAR_MARGIN: i8 = 6;

/// Touch-friendly button size in the compact bars.
const BUTTON: egui::Vec2 = egui::vec2(40.0, 40.0);

fn flag_id() -> egui::Id {
    egui::Id::unique("compact-layout")
}

/// Whether this frame uses the compact layout.
pub fn is_compact(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(flag_id())).unwrap_or(false)
}

/// Decide the layout for this frame from the content area (inside the safe area). Entering the
/// compact layout closes My Photos, which would otherwise cover the photo as a full-width
/// drawer. A content area that got shorter (iOS: the soft keyboard came up) starts a few frames
/// of [`keep_focus_visible`].
pub(crate) fn update(app: &mut LightcraftApp, ctx: &egui::Context, content: Rect) {
    let (bottom, frames) = ctx.data(|d| d.get_temp::<(f32, u8)>(shrink_id())).unwrap_or((content.bottom(), 0));
    let frames = if content.bottom() < bottom - 0.5 { 4 } else { frames.saturating_sub(1) };
    ctx.data_mut(|d| d.insert_temp(shrink_id(), (content.bottom(), frames)));
    let compact = content.width() < COMPACT_WIDTH || (content.height() < COMPACT_HEIGHT && content.width() < COMPACT_SHORT_WIDTH);
    if compact && !is_compact(ctx) {
        app.ui.left_panel = false;
    }
    ctx.data_mut(|d| d.insert_temp(flag_id(), compact));
    // a phone: controls a finger can hit
    crate::theme::finger_sizes(ctx, compact && crate::widgets::touch_screen(ctx));
}

/// In the compact layout, whether the window is wider than tall (a phone in landscape): the
/// sheet and the My Photos drawer then sit beside the photo as columns, not over or under it.
pub fn beside(ctx: &egui::Context) -> bool {
    let r = ctx.content_rect();
    is_compact(ctx) && r.width() > r.height()
}

/// The width of a column beside the photo ([`beside`]) in a region `avail` wide.
pub fn side_width(avail: f32) -> f32 {
    (avail * 0.45).clamp(SIDE_MIN, SIDE_MAX).min(avail)
}

/// The bottom sheet is up: Detail view with a right panel or Presets open, or a grid with a
/// panel that isn't an editing tool (Info, Keywords, Versions, History), as the desktop's grid
/// has. (Editing tools and Presets switch to Detail.)
pub fn sheet_open(app: &LightcraftApp) -> bool {
    match app.ui.view {
        ViewMode::Detail => app.ui.right != RightPanel::None || app.ui.presets,
        ViewMode::PhotoGrid | ViewMode::SquareGrid => {
            matches!(app.ui.right, RightPanel::Info | RightPanel::Keywords | RightPanel::Versions | RightPanel::Activity)
        }
        _ => false,
    }
}

/// The window's chrome in the compact layout; the caller fills the central area as usual.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    topbar(app, ui);
    super::library_problem::banner(app, ui);
    bottombar(app, ui);
    if app.ui.left_panel {
        // picking a source (album, folder, date …) closes the drawer onto its photos
        let source = app.session.source;
        // the drawer: the whole region, or a column beside the photos in a wide window
        let avail = ui.available_rect_before_wrap();
        let w = if beside(ui.ctx()) { side_width(avail.width()) } else { avail.width() };
        let r = egui::Panel::left("left_panel").frame(egui::Frame::NONE).resizable(false).exact_size(w).show(ui, |ui| super::left::show(app, ui));
        register(ui, "panel:left_panel", r.response.rect);
        if app.session.source != source {
            app.ui.left_panel = false;
        }
    } else if sheet_open(app) {
        sheet(app, ui);
    }
}

fn topbar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("top_bar")
        .exact_size(t.top_bar_h + 4.0)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: BAR_MARGIN, right: BAR_MARGIN, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if icon_button(ui, "sidebar", Icon::Sidebar, BUTTON, app.ui.left_panel, true, "Show/hide My Photos panel").clicked() {
                    let _ = app.run("view.leftPanel", json!({}));
                }
                if icon_button(ui, "back", Icon::Back, BUTTON, false, true, "Back").clicked() {
                    let _ = app.run("view.back", json!({}));
                }
                if !app.native_menu {
                    ui.add_space(4.0);
                    crate::menubar::show_in_window(app, ui, 0.0);
                    ui.add_space(8.0);
                }
                // right-to-left from the edge: Export, Filter, then the search field takes the rest
                let rest = ui.available_rect_before_wrap();
                let mut right = ui.new_child(egui::UiBuilder::new().max_rect(rest).layout(egui::Layout::right_to_left(egui::Align::Center)));
                right.spacing_mut().item_spacing.x = 0.0;
                if icon_button(&mut right, "share", Icon::Share, BUTTON, false, true, "Export").clicked() {
                    let _ = app.run("dialog.export", json!({}));
                }
                let filtering = app.session.filter != Default::default() || app.ui.filter_bar;
                if icon_button(&mut right, "filter", Icon::Filter, BUTTON, filtering, true, "Filter bar").clicked() {
                    let _ = app.run("view.filterBar", json!({}));
                }
                // a failing save keeps its warning visible (the desktop's cloud icon)
                if let Some((n, e)) = app.session.unsaved() {
                    let (r, resp) = right.allocate_exact_size(BUTTON, Sense::hover());
                    paint(right.painter(), r.shrink(10.0), Icon::Cloud, t.caution);
                    register(&right, "indicator:unsaved", r);
                    resp.on_hover_text(format!("{n} unsaved: {e}"));
                }
                right.add_space(4.0);
                let field = right.available_rect_before_wrap();
                if field.width() >= 80.0 {
                    search(app, &mut right, Rect::from_center_size(field.center(), vec2(field.width(), 32.0)));
                }
            });
        });
}

/// The search field in `r` (same id and command as the desktop top bar's).
fn search(app: &mut LightcraftApp, ui: &mut egui::Ui, r: Rect) {
    let t = Tokens::get(ui.ctx());
    let id = crate::text_field::id("field:search");
    if std::mem::take(&mut app.ui.focus_search) {
        ui.memory_mut(|m| m.request_focus(id));
    }
    let focused = ui.memory(|m| m.has_focus(id));
    ui.painter().rect(
        r,
        6.0,
        if focused { t.canvas } else { t.field },
        Stroke::new(1.0, if focused { t.accent } else { t.field_border }),
        StrokeKind::Inside,
    );
    let inner = r.shrink2(vec2(10.0, 4.0));
    if app.ui.search.is_empty() && !focused {
        paint(ui.painter(), Rect::from_min_size(pos2(inner.left(), r.center().y - 8.0), vec2(16.0, 16.0)), Icon::Search, t.text_dim);
    }
    let text = Rect::from_min_max(pos2(inner.left() + 22.0, inner.top()), inner.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text).layout(egui::Layout::left_to_right(egui::Align::Center)));
    // (registered as `field:search` by the field itself, like the desktop's)
    let resp = crate::text_field::TextField::singleline("field:search", &mut app.ui.search)
        .frame(false)
        .hint(crate::i18n::tr("Search"))
        .width(text.width())
        .font(t.font(14.0))
        .text_color(t.text)
        .show(&mut child)
        .response;
    if resp.changed() {
        let q = app.ui.search.clone();
        let _ = app.run("library.filter", json!({"text": q}));
    }
}

fn bottombar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::bottom("bottom_bar")
        .exact_size(t.bottom_bar_h + 4.0)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: BAR_MARGIN, right: BAR_MARGIN, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            let full = ui.max_rect();
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (id, icon, mode, tip) in [
                    ("photoGrid", Icon::GridPhoto, ViewMode::PhotoGrid, "Photo Grid (G)"),
                    ("squareGrid", Icon::GridSquare, ViewMode::SquareGrid, "Square Grid (G toggles)"),
                    ("detail", Icon::Single, ViewMode::Detail, "Detail (D)"),
                ] {
                    if icon_button(ui, id, icon, BUTTON, app.ui.view == mode, true, tip).clicked() {
                        let _ = app.run(&format!("view.{id}"), json!({}));
                    }
                }
                let sort = icon_button(ui, "sort", Icon::Sort, BUTTON, false, true, "Sort");
                egui::Popup::menu(&sort).show(|ui| super::bottombar::sort_menu(app, ui));
                super::bottombar::select_mode_button(app, ui);
            });
            let mut right = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_max(pos2(full.center().x, full.top()), full.max))
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            right.spacing_mut().item_spacing.x = 2.0;
            let has_photo = app.session.active().is_some();
            // Edit: open the sheet on the Edit panel (from the grid too), or put it away
            if icon_button(&mut right, "edit", Icon::Sliders, BUTTON, sheet_open(app), has_photo, "Edit (E)").clicked() {
                if sheet_open(app) {
                    close_sheet(app);
                } else {
                    if app.ui.view != ViewMode::Detail {
                        let _ = app.run("view.detail", json!({}));
                    }
                    if app.ui.right == RightPanel::None && !app.ui.presets {
                        let _ = app.run("panel.edit", json!({}));
                    }
                }
            }
            if app.ui.view == ViewMode::Detail
                && icon_button(&mut right, "filmstrip", Icon::Filmstrip, BUTTON, app.ui.filmstrip, true, "Filmstrip").clicked()
            {
                app.ui.filmstrip = !app.ui.filmstrip;
            }
        });
}

fn close_sheet(app: &mut LightcraftApp) {
    app.ui.right = RightPanel::None;
    app.ui.presets = false;
}

/// The bottom sheet: a row of tools, then the right panel or Presets at full width. In a wide
/// window ([`beside`]) it is a column at the right of the photo.
fn sheet(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let avail = ui.available_rect_before_wrap();
    let panel = if beside(ui.ctx()) {
        egui::Panel::right("compact_sheet_side").exact_size(side_width(avail.width()))
    } else {
        // the photo keeps the upper part; the sheet gets the rest, within reason
        let height = (avail.height() * 0.5).clamp(220.0, (avail.height() - 160.0).max(220.0));
        egui::Panel::bottom("compact_sheet").exact_size(height)
    };
    panel.resizable(false).frame(egui::Frame::NONE.fill(t.chrome).stroke(Stroke::new(1.0, t.divider))).show(ui, |ui| {
        register(ui, "panel:sheet", ui.max_rect());
        egui::Panel::top("compact_sheet_tools")
            .exact_size(BUTTON.y + 4.0)
            // (inside the sheet's 1 pt border: its icons line up with the bars')
            .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: BAR_MARGIN - 1, right: BAR_MARGIN - 1, top: 2, bottom: 2 }))
            .show(ui, |ui| tools(app, ui));
        if app.ui.presets {
            super::presets::show(app, ui);
        } else {
            register(ui, "panel:right_panel", ui.available_rect_before_wrap());
            super::right::show(app, ui);
        }
    });
}

fn tools(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let has_photo = app.session.active().is_some();
    let full = ui.max_rect();
    egui::ScrollArea::horizontal().id_salt("compact-tools").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if icon_button(ui, "presets", Icon::Presets, BUTTON, app.ui.presets, has_photo, "Presets (Shift+P)").clicked() {
                let _ = app.run("panel.presets", json!({}));
            }
            for (id, icon, panel, tip) in [
                ("edit", Icon::Sliders, RightPanel::Edit, "Edit (E)"),
                ("crop", Icon::Crop, RightPanel::Crop, "Crop & Rotate (C)"),
                ("remove", Icon::Eraser, RightPanel::Remove, "Remove (H)"),
                ("masking", Icon::Mask, RightPanel::Masking, "Masking (M)"),
                ("redeye", Icon::Eye, RightPanel::RedEye, "Red Eye"),
                ("info", Icon::Info, RightPanel::Info, "Info (I)"),
                ("keywords", Icon::Tag, RightPanel::Keywords, "Keywords (K)"),
                ("versions", Icon::Versions, RightPanel::Versions, "Versions (Shift+V)"),
                ("activity", Icon::Activity, RightPanel::Activity, "History & Activity (Y)"),
            ] {
                let on = !app.ui.presets && (app.ui.right == panel || (panel == RightPanel::Edit && app.ui.right == RightPanel::Profiles));
                if icon_button(ui, id, icon, BUTTON, on, has_photo || panel == RightPanel::Activity, tip).clicked() {
                    // one thing at a time in the sheet: a tool replaces Presets instead of toggling off
                    app.ui.presets = false;
                    if !on {
                        let _ = app.run(&format!("panel.{id}"), json!({}));
                    }
                }
            }
            // room to scroll the last tool out from under the Hide button
            ui.add_space(BUTTON.x);
        });
    });
    // put the sheet away (pinned to the row's right end, over the scrolling tools)
    let r = Rect::from_min_size(pos2(full.right() - BUTTON.x, full.center().y - BUTTON.y / 2.0), BUTTON);
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.chrome);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
    if icon_button(&mut child, "sheetClose", Icon::ChevronDown, BUTTON, false, true, "Hide").clicked() {
        close_sheet(app);
    }
}

fn shrink_id() -> egui::Id {
    egui::Id::unique("content-shrink")
}

/// Call at the end of a scroll area's contents: for a few frames after the content area got
/// shorter, the focused widget (a text field, typically, whose keyboard just came up) is scrolled
/// back into view if it's in this scroll area and now hidden below the content area.
pub fn keep_focus_visible(ui: &egui::Ui) {
    let ctx = ui.ctx();
    // not on the frame the area shrank: the focused widget's rectangle (from the previous frame)
    // is still where it was laid out before
    if ctx.data(|d| d.get_temp::<(f32, u8)>(shrink_id())).is_none_or(|(_, frames)| frames == 0 || frames == 4) {
        return;
    }
    let Some(focused) = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id)) else { return };
    let visible = ui.clip_rect().intersect(ctx.content_rect());
    if ui.min_rect().contains_rect(focused.rect) && !visible.contains_rect(focused.rect) {
        // at once: the layout is still settling, and an animation would restart every frame
        ui.scroll_to_rect_animation(focused.rect, Some(egui::Align::Center), egui::style::ScrollAnimation::none());
    }
}

/// Space kept between a compact-layout dialog and the safe area's edges.
const DIALOG_INSET: f32 = 8.0;

/// The widest a compact-layout dialog gets (outer size).
const DIALOG_MAX_W: f32 = 560.0;

/// A dialog's content width: `desktop`, or in the compact layout what fits across the screen
/// inside a frame with `margin_x` of inner margin on each side.
pub fn dialog_width(ctx: &egui::Context, desktop: f32, margin_x: f32) -> f32 {
    if !is_compact(ctx) {
        return desktop;
    }
    desktop.min((ctx.content_rect().width().min(DIALOG_MAX_W + 2.0 * DIALOG_INSET) - 2.0 * (DIALOG_INSET + margin_x)).max(120.0))
}

/// In the compact layout a dialog window spans the screen (inside the safe area) at a fixed width
/// and scrolls when it is taller than the screen; elsewhere `win` is returned unchanged (desktop
/// dialogs fit the window on their own, #781).
pub fn dialog<'a>(ctx: &egui::Context, win: egui::Window<'a>) -> egui::Window<'a> {
    if !is_compact(ctx) {
        return win;
    }
    let area = ctx.content_rect().shrink(DIALOG_INSET);
    // egui's window sizes are its outer size (frame margins included); a phone in landscape gets
    // a dialog of readable width, not one stretched across the screen
    let w = area.width().min(DIALOG_MAX_W);
    // auto-sized: as tall as its contents up to the screen, then it scrolls
    win.auto_sized().min_width(w).max_width(w).max_height(area.height()).vscroll(true).constrain_to(area)
}

/// A labelled row for dialogs: in the compact layout the label sits above its controls, which
/// wrap; elsewhere `add` comes back for the caller's usual fixed label column.
pub fn stacked_row<R, F: FnOnce(&mut egui::Ui) -> R>(ui: &mut egui::Ui, label: egui::RichText, add: F) -> Result<R, F> {
    if !is_compact(ui.ctx()) {
        return Err(add);
    }
    Ok(ui
        .vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            // (no blank line for a row without a label)
            if !label.text().is_empty() {
                ui.label(label);
            }
            ui.horizontal_wrapped(add).inner
        })
        .inner)
}
