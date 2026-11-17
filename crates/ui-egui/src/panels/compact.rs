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
///
/// Laid out like a phone photo editor: the library has a slim top bar (My Photos, Import,
/// search, Filter, Export, and "…" for every menu) over the grid and the bottom bar; the editor
/// (Detail) has Back, Show Original, Undo, Redo, Export and "…" on top, the photo, and a row of
/// labelled tools along the bottom (down the right in landscape). A tool opens its sheet: one
/// Edit section (Light, Color …), the whole Edit panel, Crop, Masking, Presets and the rest.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    // an Edit section chosen in the tool row applies only while the Edit panel is the one open
    if app.ui.right != RightPanel::Edit {
        set_section(ui.ctx(), None);
    }
    let detail = app.ui.view == ViewMode::Detail;
    if detail {
        editor_bar(app, ui);
    } else {
        library_bar(app, ui);
    }
    super::library_problem::banner(app, ui);
    if detail {
        if app.session.active().is_some() {
            tool_dock(app, ui);
        }
    } else {
        bottombar(app, ui);
    }
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
    } else if sheet_open(app) && !(detail && beside(ui.ctx())) {
        // (in landscape the editor's sheet is part of the tool column, drawn by `tool_dock`)
        sheet(app, ui);
    }
}

/// A bar along the top with the safe-area and margins of the compact layout.
fn top_bar(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::top("top_bar")
        .exact_size(t.top_bar_h + 4.0)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: BAR_MARGIN, right: BAR_MARGIN, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                add(ui);
            });
        });
}

/// The right end of a top bar, filled right to left: "…" ([`more_menu`]), then `add`.
fn bar_right(app: &mut LightcraftApp, ui: &mut egui::Ui, editor: bool, add: impl FnOnce(&mut LightcraftApp, &mut egui::Ui)) {
    let rest = ui.available_rect_before_wrap();
    let mut right = ui.new_child(egui::UiBuilder::new().max_rect(rest).layout(egui::Layout::right_to_left(egui::Align::Center)));
    right.spacing_mut().item_spacing.x = 0.0;
    let more = icon_button(&mut right, "more", Icon::Dots, BUTTON, false, true, "More");
    more_menu(app, &more, editor);
    add(app, &mut right);
}

/// What "…" offers in the editor: the photo's settings and the photo itself.
const EDITOR_MORE: &[(&str, &str)] = &[
    ("develop.copy", "Copy Settings"),
    ("develop.paste", "Paste Settings"),
    ("develop.reset", "Reset Edits"),
    ("photo.rotateLeft", "Rotate Left"),
    ("photo.rotateRight", "Rotate Right"),
    ("photo.virtualCopy", "Create Virtual Copy"),
    ("dialog.rename", "Rename…"),
    ("photo.delete", "Delete Photo"),
];

/// What "…" offers in the library.
const LIBRARY_MORE: &[(&str, &str)] = &[
    ("file.addFolder", "Import from Files…"),
    ("library.selectAll", "Select All"),
    ("app.settings", "Settings…"),
    ("app.about", "About LightCraft"),
];

/// "…": a short list of what this screen needs, as a phone photo editor has it, then All Menus
/// (the desktop's menus, for everything else).
fn more_menu(app: &mut LightcraftApp, more: &egui::Response, editor: bool) {
    let has_photo = app.session.active().is_some();
    let mut picked: Option<&str> = None;
    egui::Popup::menu(more).show(|ui| {
        ui.set_min_width(210.0);
        for (id, label) in if editor { EDITOR_MORE } else { LIBRARY_MORE } {
            let enabled = !editor || has_photo;
            let r = ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr(label)).frame(false));
            register(ui, format!("more:{id}"), r.rect);
            if r.clicked() {
                picked = Some(id);
            }
            // the destructive one apart from the rest
            if *id == "dialog.rename" {
                ui.separator();
            }
        }
        if !app.native_menu {
            ui.separator();
            let bottom = Some(ui.max_rect().bottom());
            let r = ui.menu_button(crate::i18n::tr("All Menus"), |ui| crate::menubar::menus_in(app, ui, bottom));
            register(ui, "more:allMenus", r.response.rect);
        }
    });
    if let Some(id) = picked
        && let Err(e) = crate::menubar::run_item(app, id, json!({}))
    {
        app.toast(&more.ctx, e);
    }
}

fn unsaved_indicator(app: &LightcraftApp, ui: &mut egui::Ui) {
    // a failing save keeps its warning visible (the desktop's cloud icon)
    if let Some((n, e)) = app.session.unsaved() {
        let t = Tokens::get(ui.ctx());
        let (r, resp) = ui.allocate_exact_size(BUTTON, Sense::hover());
        paint(ui.painter(), r.shrink(10.0), Icon::Cloud, t.caution);
        register(ui, "indicator:unsaved", r);
        resp.on_hover_text(format!("{n} unsaved: {e}"));
    }
}

/// The library's bar: My Photos, then (from the right) "…", Export, Filter, Import and the
/// search field in what's left.
fn library_bar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    top_bar(ui, |ui| {
        if icon_button(ui, "sidebar", Icon::Sidebar, BUTTON, app.ui.left_panel, true, "Show/hide My Photos panel").clicked() {
            let _ = app.run("view.leftPanel", json!({}));
        }
        if app.ui.view != ViewMode::PhotoGrid
            && app.ui.view != ViewMode::SquareGrid
            && icon_button(ui, "back", Icon::Back, BUTTON, false, true, "Back").clicked()
        {
            let _ = app.run("view.back", json!({}));
        }
        bar_right(app, ui, false, |app, right| {
            if icon_button(right, "share", Icon::Share, BUTTON, false, app.session.active().is_some(), "Export").clicked() {
                let _ = app.run("dialog.export", json!({}));
            }
            let filtering = app.session.filter != Default::default() || app.ui.filter_bar;
            if icon_button(right, "filter", Icon::Filter, BUTTON, filtering, true, "Filter bar").clicked() {
                let _ = app.run("view.filterBar", json!({}));
            }
            // the system photo picker (or the Files app's) on a phone; the file dialog elsewhere
            if icon_button(right, "import", Icon::Plus, BUTTON, false, true, "Import Photos…").clicked()
                && let Err(e) = crate::menubar::run_item(app, "file.addPhotos", json!({}))
            {
                app.toast(right.ctx(), e);
            }
            unsaved_indicator(app, right);
            right.add_space(4.0);
            let field = right.available_rect_before_wrap();
            if field.width() >= 80.0 {
                search(app, right, Rect::from_center_size(field.center(), vec2(field.width(), 32.0)));
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

/// The editor's bar: Back to the photos, then (from the right) "…", Export, Redo, Undo and
/// Show Original (held down: the photo as shot, let go: the edit).
fn editor_bar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    top_bar(ui, |ui| {
        if icon_button(ui, "back", Icon::ChevronLeft, BUTTON, false, true, "Back").clicked() {
            let _ = app.run("view.back", json!({}));
        }
        bar_right(app, ui, true, |app, right| {
            if icon_button(right, "share", Icon::Share, BUTTON, false, app.session.active().is_some(), "Export").clicked() {
                let _ = app.run("dialog.export", json!({}));
            }
            if undo_button(right, "redo", "Redo", true).clicked() {
                let _ = app.run("edit.redo", json!({}));
            }
            if undo_button(right, "undo", "Undo", false).clicked() {
                let _ = app.run("edit.undo", json!({}));
            }
            let r = icon_button(right, "showOriginal", Icon::BeforeAfter, BUTTON, false, true, "Show Original (hold)");
            hold_original(app, &r);
            unsaved_indicator(app, right);
        });
    });
}

/// Show Original held down: the press shows the photo as shot, and the release brings back the
/// view it replaced. Only the press and release change the view, so View > Show Original and the
/// Before / After layouts work as usual otherwise. The hold is followed until the pointer lifts,
/// not by the button's response: egui turns a finger held past `max_click_duration` into a long
/// press and the button then no longer reports the pointer as down on it.
fn hold_original(app: &mut LightcraftApp, r: &egui::Response) {
    let key = egui::Id::unique("compact-hold-original");
    let was: Option<crate::state::BeforeAfter> = r.ctx.data(|d| d.get_temp(key));
    match was {
        None if r.is_pointer_button_down_on() => {
            r.ctx.data_mut(|d| d.insert_temp(key, app.ui.before_after));
            app.ui.before_after = crate::state::BeforeAfter::Original;
        }
        Some(before) if !r.ctx.input(|i| i.pointer.any_down() || i.any_touches()) => {
            r.ctx.data_mut(|d| d.remove::<crate::state::BeforeAfter>(key));
            app.ui.before_after = before;
        }
        _ => {}
    }
}

/// Undo (a hooked arrow turning back to the left) or Redo (the same, mirrored).
fn undo_button(ui: &mut egui::Ui, id: &str, tip: &str, redo: bool) -> egui::Response {
    let r = icon_button(ui, id, Icon::Circle, BUTTON, false, true, tip);
    // paint over the placeholder glyph: there is no undo icon in the set
    let t = Tokens::get(ui.ctx());
    let c = r.rect.center();
    ui.painter().rect_filled(Rect::from_center_size(c, vec2(26.0, 26.0)), 4.0, if r.hovered() { t.hover } else { t.chrome });
    let sx = if redo { -1.0 } else { 1.0 };
    let p = |x: f32, y: f32| pos2(c.x + sx * x, c.y + y);
    let stroke = Stroke::new(1.6, t.icon);
    // the hook: from the arrow head, across, round and back down
    let mut pts = vec![p(-7.0, -3.0)];
    for i in 0..=8 {
        let a = std::f32::consts::FRAC_PI_2 * (f32::from(i as u8) / 8.0) * 2.0 - std::f32::consts::FRAC_PI_2;
        pts.push(p(1.5 + 5.0 * a.cos(), 2.0 + 5.0 * a.sin()));
    }
    pts.push(p(-1.0, 7.0));
    ui.painter().add(egui::Shape::line(pts, stroke));
    ui.painter().line_segment([p(-7.0, -3.0), p(-3.5, -6.5)], stroke);
    ui.painter().line_segment([p(-7.0, -3.0), p(-3.5, 0.5)], stroke);
    r
}

/// The library's bottom bar: the grid sizes and Detail, Sort and Select (a tap on a photo opens
/// the editor, so there is no Edit button). In Select mode by finger it is the selection's bar instead
/// ([`select_bar`]).
fn bottombar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    if app.ui.select_mode && crate::widgets::touch_screen(ui.ctx()) {
        egui::Panel::bottom("bottom_bar")
            .exact_size(t.bottom_bar_h + 4.0)
            .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: BAR_MARGIN + 6, right: BAR_MARGIN, top: 0, bottom: 0 }))
            .show(ui, |ui| select_bar(app, ui));
        return;
    }
    egui::Panel::bottom("bottom_bar")
        .exact_size(t.bottom_bar_h + 4.0)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: BAR_MARGIN, right: BAR_MARGIN, top: 0, bottom: 0 }))
        .show(ui, |ui| {
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
        });
}

/// Select mode's bar: how many photos are selected, then Share (export), Album, Move to Folder,
/// Delete, and Done (leaves Select mode).
fn select_bar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let n = app.session.selection.ids.len();
    let any = n > 0;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let count = if any { crate::i18n::tr_format!("{n} selected", n = n) } else { crate::i18n::tr("Select Photos").to_string() };
        ui.label(egui::RichText::new(count).font(t.semibold(14.0)).color(t.text));
        let full = ui.max_rect();
        let mut right = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(pos2(full.center().x - 60.0, full.top()), full.max))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        right.spacing_mut().item_spacing.x = 2.0;
        let done = right.add(egui::Button::new(egui::RichText::new(crate::i18n::tr("Done")).font(t.semibold(14.0)).color(t.accent)).frame(false));
        register(&right, "button:selectDone", done.rect);
        if done.clicked() {
            let _ = app.run("view.selectMode", json!({"on": false}));
        }
        if icon_button(&mut right, "selectDelete", Icon::Trash, BUTTON, false, any, "Delete").clicked() {
            let _ = crate::menubar::run_item(app, "photo.delete", json!({}));
        }
        if crate::panels::grid::can_move_to_folder(app)
            && icon_button(&mut right, "selectMove", Icon::Folder, BUTTON, false, any, "Move to Folder…").clicked()
        {
            crate::panels::grid::move_to_folder(app, right.ctx());
        }
        let album = icon_button(&mut right, "selectAlbum", Icon::Album, BUTTON, false, any, "Add to Album");
        if let Some(id) = app.session.selection.active.or_else(|| app.session.selection.ids.first().copied()) {
            egui::Popup::menu(&album).show(|ui| crate::panels::grid::context_menu_albums(app, ui, id));
        }
        if icon_button(&mut right, "selectShare", Icon::Share, BUTTON, false, any, "Export").clicked() {
            let _ = app.run("dialog.export", json!({}));
        }
    });
}

fn close_sheet(app: &mut LightcraftApp) {
    app.ui.right = RightPanel::None;
    app.ui.presets = false;
}

/// What a tab of the editor's tool row opens.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Presets,
    /// An action, not a sheet: Auto settings.
    Auto,
    /// A right panel (Edit shows all of its sections).
    Panel(RightPanel),
    /// One section of the Edit panel on its own.
    Section(&'static str),
}

/// The editor's tool row, in a phone photo editor's order: (widget id, label, icon, tab).
const TABS: &[(&str, &str, Icon, Tab)] = &[
    ("presets", "Presets", Icon::Presets, Tab::Presets),
    ("auto", "Auto", Icon::Target, Tab::Auto),
    ("crop", "Crop", Icon::Crop, Tab::Panel(RightPanel::Crop)),
    ("light", "Light", Icon::Sliders, Tab::Section("light")),
    ("color", "Color", Icon::Radial, Tab::Section("color")),
    ("effects", "Effects", Icon::Curve, Tab::Section("effects")),
    ("detailSection", "Detail", Icon::Picker, Tab::Section("detail")),
    ("optics", "Optics", Icon::Circle, Tab::Section("optics")),
    ("masking", "Masking", Icon::Mask, Tab::Panel(RightPanel::Masking)),
    ("remove", "Remove", Icon::Eraser, Tab::Panel(RightPanel::Remove)),
    ("edit", "All", Icon::Pencil, Tab::Panel(RightPanel::Edit)),
    ("calibration", "Calibration", Icon::ProfileGrid, Tab::Section("calibration")),
    ("redeye", "Red Eye", Icon::Eye, Tab::Panel(RightPanel::RedEye)),
    ("info", "Info", Icon::Info, Tab::Panel(RightPanel::Info)),
    ("keywords", "Keywords", Icon::Tag, Tab::Panel(RightPanel::Keywords)),
    ("versions", "Versions", Icon::Versions, Tab::Panel(RightPanel::Versions)),
    ("activity", "History", Icon::Activity, Tab::Panel(RightPanel::Activity)),
];

/// A tab's touch target.
const TAB: egui::Vec2 = egui::vec2(62.0, 54.0);

fn section_id() -> egui::Id {
    egui::Id::unique("compact-edit-section")
}

/// The Edit section the sheet shows on its own (`None`: the whole Edit panel).
fn section(ctx: &egui::Context) -> Option<&'static str> {
    ctx.data(|d| d.get_temp::<Option<&'static str>>(section_id())).flatten()
}

fn set_section(ctx: &egui::Context, s: Option<&'static str>) {
    ctx.data_mut(|d| d.insert_temp(section_id(), s));
}

fn tab_on(app: &LightcraftApp, ctx: &egui::Context, tab: Tab) -> bool {
    match tab {
        Tab::Presets => app.ui.presets,
        Tab::Auto => false,
        Tab::Panel(RightPanel::Edit) => !app.ui.presets && matches!(app.ui.right, RightPanel::Edit | RightPanel::Profiles) && section(ctx).is_none(),
        Tab::Panel(p) => !app.ui.presets && app.ui.right == p,
        Tab::Section(k) => !app.ui.presets && app.ui.right == RightPanel::Edit && section(ctx) == Some(k),
    }
}

/// A tab pressed: its sheet opens (one thing at a time), or, pressed again, the sheet goes away.
fn press(app: &mut LightcraftApp, ctx: &egui::Context, tab: Tab) {
    if tab == Tab::Auto {
        let _ = app.run("develop.auto", json!({}));
        app.toast(ctx, crate::i18n::tr("Auto settings applied"));
        return;
    }
    if tab_on(app, ctx, tab) {
        close_sheet(app);
        return;
    }
    match tab {
        Tab::Presets => {
            if !app.ui.presets {
                let _ = app.run("panel.presets", json!({}));
            }
        }
        Tab::Panel(p) => {
            app.ui.presets = false;
            set_section(ctx, None);
            if app.ui.right != p {
                let id = TABS.iter().find(|(_, _, _, t)| *t == tab).map_or("edit", |(id, ..)| *id);
                let _ = app.run(&format!("panel.{id}"), json!({}));
            }
        }
        Tab::Section(k) => {
            app.ui.presets = false;
            if app.ui.right != RightPanel::Edit {
                let _ = app.run("panel.edit", json!({}));
            }
            set_section(ctx, Some(k));
        }
        Tab::Auto => {}
    }
}

/// The tool row (or, `vertical`, column): each tab an icon over its name, the open one lit.
fn tool_tabs(app: &mut LightcraftApp, ui: &mut egui::Ui, vertical: bool) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let mut row = |ui: &mut egui::Ui| {
        for (id, label, icon, tab) in TABS {
            let on = tab_on(app, &ctx, *tab);
            let (r, resp) = ui.allocate_exact_size(TAB, Sense::click());
            register(ui, format!("icon:{id}"), r);
            resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, crate::i18n::tr(label)));
            let color = if on { t.accent } else { t.icon };
            if on {
                ui.painter().rect_filled(r.shrink(2.0), 8.0, t.tool_active);
            }
            paint(ui.painter(), Rect::from_center_size(pos2(r.center().x, r.top() + 19.0), vec2(20.0, 20.0)), *icon, color);
            ui.painter().text(pos2(r.center().x, r.bottom() - 11.0), egui::Align2::CENTER_CENTER, crate::i18n::tr(label), t.font(10.5), color);
            if resp.clicked() {
                press(app, &ctx, *tab);
            }
        }
    };
    let drag = egui::scroll_area::ScrollSource { drag: egui::scroll_area::DragScroll::Always, ..Default::default() };
    if vertical {
        egui::ScrollArea::vertical().id_salt("compact-tools").scroll_source(drag).auto_shrink([false, false]).show(ui, |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                row(ui);
            });
        });
    } else {
        egui::ScrollArea::horizontal().id_salt("compact-tools").scroll_source(drag).auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                row(ui);
            });
        });
    }
}

/// The editor's tools: a row along the bottom, or in landscape a column at the right edge (with
/// the open sheet beside it, as one column).
fn tool_dock(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    if beside(ui.ctx()) {
        let avail = ui.available_rect_before_wrap();
        if sheet_open(app) && !app.ui.left_panel {
            let r = egui::Panel::right("compact_sheet_side")
                .exact_size(side_width(avail.width()))
                .resizable(false)
                .frame(egui::Frame::NONE.fill(t.chrome).stroke(Stroke::new(1.0, t.divider)))
                .show(ui, |ui| {
                    register(ui, "panel:sheet", ui.max_rect());
                    egui::Panel::bottom("compact_tools")
                        .exact_size(TAB.y + 6.0)
                        .resizable(false)
                        .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: BAR_MARGIN, right: BAR_MARGIN, top: 2, bottom: 2 }))
                        .show(ui, |ui| tool_tabs(app, ui, false));
                    sheet_body(app, ui);
                });
            register(ui, "panel:tools", r.response.rect);
        } else {
            let r = egui::Panel::right("compact_tools_side")
                .exact_size(TAB.x + 8.0)
                .resizable(false)
                .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: 4, right: 4, top: 0, bottom: 0 }))
                .show(ui, |ui| tool_tabs(app, ui, true));
            register(ui, "panel:tools", r.response.rect);
        }
    } else {
        let r = egui::Panel::bottom("compact_tools")
            .exact_size(TAB.y + 8.0)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: BAR_MARGIN, right: BAR_MARGIN, top: 3, bottom: 3 }))
            .show(ui, |ui| tool_tabs(app, ui, false));
        register(ui, "panel:tools", r.response.rect);
    }
}

/// The bottom sheet above the tool row (in landscape, the editor's sheet is part of the tool
/// column instead; the grid's Info, Keywords … sheet is a column at the right there).
fn sheet(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let avail = ui.available_rect_before_wrap();
    let panel = if beside(ui.ctx()) {
        egui::Panel::right("compact_sheet_side").exact_size(side_width(avail.width()))
    } else {
        // the photo keeps the upper part; the sheet gets the rest, within reason
        let height = (avail.height() * 0.5).clamp(200.0, (avail.height() - 160.0).max(200.0));
        egui::Panel::bottom("compact_sheet").exact_size(height)
    };
    panel.resizable(false).frame(egui::Frame::NONE.fill(t.chrome).stroke(Stroke::new(1.0, t.divider))).show(ui, |ui| {
        register(ui, "panel:sheet", ui.max_rect());
        sheet_body(app, ui);
    });
}

/// The sheet's title and Hide, then what the open tool shows.
fn sheet_body(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let title = TABS.iter().find(|(.., tab)| tab_on(app, &ctx, *tab)).map_or("Edit", |(_, label, ..)| *label);
    // one Edit section: its eye (on / off), as on the section's header in the full panel
    let eye = section(&ctx)
        .filter(|_| app.ui.right == RightPanel::Edit && !app.ui.presets)
        .zip(app.session.active())
        .map(|(key, id)| (key, super::edit::shown_settings(app, id).0.section_enabled(key)));
    egui::Panel::top("compact_sheet_head")
        .exact_size(36.0)
        .resizable(false)
        .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: 16, right: BAR_MARGIN - 1, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(egui::RichText::new(crate::i18n::tr(title)).font(t.semibold(14.0)).color(t.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, "sheetClose", Icon::ChevronDown, vec2(36.0, 36.0), false, true, "Hide").clicked() {
                        close_sheet(app);
                    }
                    if let Some((key, on)) = eye {
                        section_eye(app, ui, key, on);
                    }
                });
            });
        });
    if app.ui.presets {
        super::presets::show(app, ui);
        return;
    }
    let one = section(&ctx).filter(|_| app.ui.right == RightPanel::Edit);
    match (one, app.session.active()) {
        (Some(key), Some(id)) => {
            register(ui, "panel:right_panel", ui.available_rect_before_wrap());
            let drag = egui::scroll_area::ScrollSource { drag: egui::scroll_area::DragScroll::Always, ..Default::default() };
            egui::ScrollArea::vertical().id_salt("compact-section").scroll_source(drag).auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(6.0);
                super::edit::notices(app, ui, id);
                let (d, raw) = super::edit::shown_settings(app, id);
                super::edit::section_body(app, ui, &d, id, raw, key);
                ui.add_space(24.0);
                keep_focus_visible(ui);
            });
        }
        _ => {
            register(ui, "panel:right_panel", ui.available_rect_before_wrap());
            super::right::show(app, ui);
        }
    }
}

/// The sheet's eye for its Edit section: off, the section contributes nothing to the photo.
fn section_eye(app: &mut LightcraftApp, ui: &mut egui::Ui, key: &str, on: bool) {
    let t = Tokens::get(ui.ctx());
    let tip = if on { "Hide this section's edits" } else { "Show this section's edits" };
    let r = icon_button(ui, "sheetEye", Icon::Eye, vec2(36.0, 36.0), false, true, tip);
    crate::widgets::announce_selected(&r, crate::i18n::tr("Show / hide"), on);
    if !on {
        let e = Rect::from_center_size(r.rect.center(), vec2(18.0, 18.0));
        ui.painter().line_segment([e.left_bottom(), e.right_top()], Stroke::new(1.5, t.text_disabled));
    }
    if r.clicked() {
        let _ = app.run("develop.sectionEnabled", json!({"section": key, "enabled": !on}));
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
