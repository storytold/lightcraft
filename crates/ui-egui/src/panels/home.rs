//! The Home screen (`view.home`): a welcome, the ways into a library (Import Photos…, Open Library…,
//! All Photos), the recent libraries and the help and community links.

use egui::{Align2, CornerRadius, Rect, Sense, Ui, pos2, vec2};
use lightcraft_engine::LibrarySource;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::button::Button;
use crate::icons::{Icon, paint};
use crate::state::ViewMode;
use crate::theme::Tokens;

/// The column's widest; a narrower window takes what it has.
const COLUMN: f32 = 640.0;
/// How many of File ▸ Open Recent Library the screen lists.
const RECENT_SHOWN: usize = 6;
const ROW_H: f32 = 28.0;

/// One click on the screen, run after its widgets are drawn.
enum Action {
    /// Run a command where it is: its result shows on screen.
    Run(&'static str),
    /// The grid, where what the command brings in shows up, then the command.
    GridThen(&'static str),
    Grid,
    OpenLibrary(String),
}

pub fn show(app: &mut LightcraftApp, ui: &mut Ui) {
    follow_the_sidebar(app, ui);
    let mut action = None;
    egui::ScrollArea::vertical().id_salt("home-scroll").auto_shrink([false, false]).show(ui, |ui| {
        let side = ((ui.available_width() - COLUMN) / 2.0).max(24.0);
        let width = (ui.available_width() - 2.0 * side).max(0.0);
        ui.add_space(56.0);
        ui.horizontal_top(|ui| {
            ui.add_space(side);
            ui.allocate_ui_with_layout(vec2(width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(width);
                action = content(app, ui);
            });
        });
        ui.add_space(56.0);
    });
    if let Some(action) = action
        && let Err(e) = run(app, action)
    {
        app.toast_error(ui.ctx(), e);
    }
}

fn run(app: &mut LightcraftApp, action: Action) -> Result<Value, String> {
    match action {
        Action::Run(command) => app.run(command, json!({})),
        Action::GridThen(command) => {
            app.ui.view = ViewMode::PhotoGrid;
            app.run(command, json!({}))
        }
        Action::Grid => {
            app.ui.view = ViewMode::PhotoGrid;
            Ok(Value::Null)
        }
        Action::OpenLibrary(path) => {
            let opened = app.run("app.openLibrary", json!({ "path": path }))?;
            app.ui.view = ViewMode::PhotoGrid;
            Ok(opened)
        }
    }
}

/// A source picked in the sidebar (an album, a date, a folder) is there to be looked at: the grid
/// shows it. The source this screen last saw lives in the context, so coming back to the screen
/// with another source already picked stays here.
fn follow_the_sidebar(app: &mut LightcraftApp, ui: &Ui) {
    let id = egui::Id::unique("home-source");
    let now = app.session.source;
    let pass = ui.ctx().cumulative_frame_nr();
    let seen: Option<(LibrarySource, u64)> = ui.data(|d| d.get_temp(id));
    ui.data_mut(|d| d.insert_temp(id, (now, pass)));
    if seen.is_some_and(|(source, last_pass)| source != now && last_pass + 1 == pass) {
        app.ui.view = ViewMode::PhotoGrid;
        ui.ctx().request_repaint();
    }
}

fn content(app: &LightcraftApp, ui: &mut Ui) -> Option<Action> {
    let t = Tokens::get(ui.ctx());
    let mut action = None;
    ui.label(egui::RichText::new(crate::i18n::tr("Welcome to LightCraft")).font(t.semibold(26.0)).color(t.text));
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(crate::i18n::tr("Your photo library and raw developer: local, open and scriptable."))
            .font(t.font(14.0))
            .color(t.text_dim),
    );
    ui.add_space(22.0);
    ui.horizontal_wrapped(|ui| {
        if Button::new("Import Photos…").primary().widget("button:homeImportPhotos").show(ui).clicked() {
            action = Some(Action::GridThen("file.addPhotos"));
        }
        // the browser build has one library, in the browser's own storage
        if !cfg!(target_arch = "wasm32") && Button::new("Open Library…").widget("button:homeOpenLibrary").show(ui).clicked() {
            action = Some(Action::Run("app.openLibrary"));
        }
        if Button::new("All Photos").widget("button:homeAllPhotos").show(ui).clicked() {
            action = Some(Action::Grid);
        }
    });
    if let Some(picked) = recent(app, ui) {
        action = Some(picked);
    }
    ui.add_space(28.0);
    heading(ui, &t, "Help");
    ui.horizontal_wrapped(|ui| {
        if Button::new("LightCraft Help").widget("button:homeHelp").show(ui).clicked() {
            action = Some(Action::Run("app.help"));
        }
        if Button::new("Join the ArtCraft Discord…").widget("button:homeDiscord").show(ui).clicked() {
            action = Some(Action::Run("app.discord"));
        }
    });
    action
}

fn heading(ui: &mut Ui, t: &Tokens, text: &str) {
    ui.label(egui::RichText::new(crate::i18n::tr(text)).font(t.semibold(14.0)).color(t.text));
    ui.add_space(8.0);
}

/// The first of File ▸ Open Recent Library, nothing when there are none. A library a check found
/// missing, and every other one while the library can't be switched, is greyed out as in the menu;
/// the open one is ticked and leads to its photos.
fn recent(app: &LightcraftApp, ui: &mut Ui) -> Option<Action> {
    let settings = &app.ui.settings;
    if settings.recent_libraries.is_empty() {
        return None;
    }
    let t = Tokens::get(ui.ctx());
    ui.add_space(28.0);
    heading(ui, &t, "Recent Libraries");
    let can = crate::panels::settings::can_switch_library(app).is_ok();
    let open = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
    let mut action = None;
    for (i, path) in settings.recent_libraries.iter().take(RECENT_SHOWN).enumerate() {
        let dir = std::path::Path::new(path);
        let is_open = open.as_deref() == Some(dir);
        let enabled = is_open || (can && app.recent_checks.exists(path) != Some(false));
        if recent_row(ui, &t, i, dir, is_open, enabled).clicked() {
            action = Some(if is_open { Action::Grid } else { Action::OpenLibrary(path.clone()) });
        }
    }
    action
}

/// One row: a tick on the open library, its name, and the folder it is in (dim, cut to fit). The
/// tooltip is the full path.
fn recent_row(ui: &mut Ui, t: &Tokens, index: usize, dir: &std::path::Path, is_open: bool, enabled: bool) -> egui::Response {
    const PAD: f32 = 6.0;
    const ICON: f32 = 16.0;
    let name = crate::panels::settings::library_name(dir);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), sense);
    crate::widgets::register(ui, format!("home:recent:{index}"), rect);
    if ui.is_rect_visible(rect) {
        if enabled && resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(4), t.hover);
        }
        if is_open {
            let tick = Rect::from_center_size(pos2(rect.left() + PAD + ICON / 2.0, rect.center().y), vec2(ICON, ICON));
            paint(ui.painter(), tick, Icon::Check, t.text);
        }
        let x = rect.left() + PAD + ICON + 6.0;
        let name_color = if enabled { t.text } else { t.text_dim };
        let shown = ui.painter().text(pos2(x, rect.center().y), Align2::LEFT_CENTER, &name, t.font(13.0), name_color);
        let folder = dir.parent().map(|p| p.display().to_string()).unwrap_or_default();
        let folder_x = shown.right() + 10.0;
        let room = rect.right() - PAD - folder_x;
        if room > 0.0 && !folder.is_empty() {
            let mut job = egui::text::LayoutJob::simple_singleline(folder, t.font(12.0), t.text_dim);
            job.wrap = egui::text::TextWrapping { max_width: room, max_rows: 1, break_anywhere: true, ..Default::default() };
            let galley = ui.painter().layout_job(job);
            ui.painter().galley(pos2(folder_x, rect.center().y - galley.size().y / 2.0), galley, t.text_dim);
        }
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &name));
    resp.on_hover_text(dir.display().to_string())
}
