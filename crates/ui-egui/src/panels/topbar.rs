//! The top bar: sidebar toggle, back/forward, search, filter, and the right-hand icons.

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

/// The bar doubles as the window's title bar: dragging its empty space moves the window and a
/// double-click acts as the system setting says. Made before the bar's widgets so they win the click.
fn window_handle(ui: &mut egui::Ui, bar: Rect) {
    register(ui, "region:titlebar", bar);
    let resp = craft_window::title_bar::empty_area(ui, bar);
    crate::widgets::announce(&resp, crate::i18n::tr("Title bar"));
}

/// Draw the top bar: the titlebar drag area, the in-window menus when there is no native menu
/// bar, sidebar and back/forward buttons, the centred search field, the filter button with its
/// active count, and the right-side action icons.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // room for the traffic lights; in full screen they are gone, and the sidebar button sits as far
    // from the left edge as the Discord button from the right (12 margin + 4)
    let left = match (app.integrated_titlebar, app.window_is_fullscreen) {
        (true, false) => 78,
        (true, true) => 16,
        (false, _) => 10,
    };
    // A dialog fades the disabled bar's controls, and its fill with them. A sheet leaves the title bar's
    // color alone (the window buttons the OS draws over it stay lit), so the fill is painted unfaded
    // beneath, in a slot reserved before the bar's own shapes.
    let sheet = app.integrated_titlebar && !app.window_is_fullscreen && app.ui.dialog.is_some();
    let slot = sheet.then(|| ui.ctx().layer_painter(ui.layer_id()).add(egui::Shape::Noop));
    let fill = if sheet { egui::Color32::TRANSPARENT } else { t.chrome };
    let panel = egui::Panel::top("top_bar")
        .exact_size(t.top_bar_h)
        .frame(egui::Frame::NONE.fill(fill).inner_margin(egui::Margin { left, right: 12, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            crate::widgets::announce_region(ui, egui::accesskit::Role::Toolbar, crate::i18n::tr("Top bar"));
            let full = ui.max_rect();
            // The whole bar, its margins included. The search field is centred on this and not on
            // `full`, which is only what the margins leave: on macOS the left margin holds the
            // traffic lights' room, and full screen gives that room back. Centring on `full` moved
            // the field, and changed its width, every time that room changed (issue #2365).
            let bar = Rect::from_min_max(pos2(full.left() - left as f32, full.top()), pos2(full.right() + 12.0, full.bottom()));
            // The field stops short of the filter icon and the seven icons on the right; centred,
            // it is twice the room between the window's centre and them.
            let mut sw = 640.0f32.min(2.0 * (full.right() - 310.0 - bar.center().x)).max(190.0);
            if !app.native_menu {
                // leave room for the in-window menus left of the (centred) search field
                let menus_right = full.left() + 140.0 + crate::menubar::bar_width(ui) + 24.0;
                sw = sw.min(2.0 * (bar.center().x - menus_right)).max(190.0);
            }
            let search_left = bar.center().x - sw / 2.0;
            let sr = Rect::from_center_size(pos2(bar.center().x, full.center().y), vec2(sw, 28.0));
            if app.integrated_titlebar {
                window_handle(ui, bar);
            }
            nav_buttons(app, ui, search_left);
            search_field(app, ui, &t, sr);
            filter_button(app, ui, &t, sr);
            let x = right_icons(app, ui, &t, full);
            appearance_button(app, ui, &t, full, x);
        });
    if let Some(slot) = slot {
        ui.ctx().layer_painter(ui.layer_id()).set(slot, egui::Shape::rect_filled(panel.response.rect, 0.0, t.chrome));
    }
}

/// Sidebar toggle, back/forward and, without a native menu bar, the in-window menus.
fn nav_buttons(app: &mut LightcraftApp, ui: &mut egui::Ui, search_left: f32) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if icon_button(ui, "sidebar", Icon::Sidebar, vec2(30.0, 30.0), app.ui.left_panel, true, crate::i18n::tr("Show/hide My Photos panel"))
            .clicked()
        {
            let _ = app.run("view.leftPanel", json!({}));
        }
        ui.add_space(8.0);
        if icon_button(ui, "back", Icon::Back, vec2(30.0, 30.0), false, true, crate::i18n::tr("Back")).clicked() {
            let _ = app.run("view.back", json!({}));
        }
        icon_button(ui, "forward", Icon::Forward, vec2(30.0, 30.0), false, false, crate::i18n::tr("Forward"));
        if !app.native_menu {
            // no native menu bar (web, Windows, Linux): menus in the top bar
            ui.add_space(10.0);
            let room = search_left - ui.cursor().left() - 16.0;
            crate::menubar::show_in_window(app, ui, room);
        }
    });
}

/// The centred search field with its placeholder and focus ring.
fn search_field(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, sr: Rect) {
    // search field (centred on the window)
    let id = crate::text_field::id("field:search");
    if std::mem::take(&mut app.ui.focus_search) {
        ui.memory_mut(|m| m.request_focus(id));
    }
    let focused = ui.memory(|m| m.has_focus(id));
    ui.painter().rect(
        sr,
        4.0,
        if focused { t.canvas } else { t.field },
        Stroke::new(1.0, if focused { t.accent } else { t.field_border }),
        StrokeKind::Inside,
    );
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(sr.shrink2(vec2(10.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let empty = app.ui.search.is_empty();
    if empty && !focused {
        let g = child.painter().layout_no_wrap(crate::i18n::tr("Search Photos").into(), t.font(13.5), t.text_dim);
        let w = g.size().x + 24.0;
        let x0 = sr.center().x - w / 2.0;
        paint(child.painter(), Rect::from_min_size(pos2(x0, sr.center().y - 8.0), vec2(16.0, 16.0)), Icon::Search, t.text_dim);
        child.painter().galley(pos2(x0 + 24.0, sr.center().y - g.size().y / 2.0), g, t.text_dim);
    }
    let resp = crate::text_field::TextField::singleline("field:search", &mut app.ui.search)
        .label(crate::i18n::tr("Search Photos"))
        .frame(false)
        .width(sr.width() - 20.0)
        .font(t.font(13.5))
        .text_color(t.text)
        .show(&mut child)
        .response;
    if resp.changed() {
        let q = app.ui.search.clone();
        let _ = app.run("library.filter", json!({"text": q}));
    }
}

/// The filter icon beside the search field, with the active-filter badge.
fn filter_button(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, sr: Rect) {
    // filter icon right of the search field
    let fr = Rect::from_center_size(pos2(sr.right() + 22.0, sr.center().y), vec2(28.0, 28.0));
    let fresp = ui.interact(fr, egui::Id::unique("filter-btn"), Sense::click());
    crate::widgets::announce(&fresp, crate::i18n::tr("Filters"));
    register(ui, "icon:filter", fr);
    let filtering = app.session.filter != Default::default() || app.ui.filter_bar;
    paint(
        ui.painter(),
        fr.shrink(6.0),
        Icon::Filter,
        if filtering {
            t.accent
        } else if fresp.hovered() {
            t.text
        } else {
            t.icon
        },
    );
    // badge: how many filters are on, even with the filter bar closed
    let active = lightcraft_engine::filter_chips(&app.session.filter, &app.session.catalog).len();
    if active > 0 {
        let c = fr.right_top() + vec2(-3.0, 8.0);
        ui.painter().circle_filled(c, 7.0, t.accent);
        ui.painter().text(c, Align2::CENTER_CENTER, active.to_string(), t.semibold(9.5), t.canvas);
    }
    let fresp = fresp.on_hover_text(if active > 0 {
        crate::i18n::tr_format!("Filter bar — {active} active filter{}", if active == 1 { "" } else { "s" }, active = active)
    } else {
        crate::i18n::tr("Filter bar").into()
    });
    if fresp.clicked() {
        let _ = app.run("view.filterBar", json!({}));
    }
}

/// The right-side action icons; returns the x where the next icon would go.
fn right_icons(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, full: Rect) -> f32 {
    // right icons
    let mut x = full.right() - 18.0;
    // saving is failing: the cloud icon turns into a warning until a save succeeds
    let unsaved = app.session.unsaved().map(|(n, e)| unsaved_tip(n, e));
    let cloud_tip = unsaved.as_deref().unwrap_or("Local library — no cloud account needed");
    // Import Photos… beside Export, always one click away, with the key it is bound to now
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    let import_tip = match crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "file.addPhotos") {
        Some(sc) => format!("{} ({})", crate::i18n::tr("Import Photos…"), crate::menubar::shortcut_text(sc, mac)),
        None => crate::i18n::tr("Import Photos…").to_string(),
    };
    for (id, icon, tip, cmd) in [
        ("discord", Icon::Chat, "Join the ArtCraft community on Discord", "app.discord"),
        ("cloud", Icon::Cloud, cloud_tip, ""),
        ("help", Icon::Help, "Keyboard shortcuts", "app.shortcuts"),
        ("share", Icon::Share, "Export", "dialog.export"),
        ("import", Icon::Plus, import_tip.as_str(), "file.addPhotos"),
        ("bell", Icon::Bell, "Activity", "panel.activity"),
    ] {
        let r = Rect::from_center_size(pos2(x, full.center().y), vec2(28.0, 28.0));
        let tip = crate::i18n::tr(tip);
        let resp = ui.interact(r, egui::Id::unique(("top", id)), Sense::click());
        let name = crate::widgets::accessible_label(&resp, tip);
        let resp = resp.on_hover_text(tip);
        crate::widgets::announce(&resp, name);
        register(ui, format!("icon:{id}"), r);
        let warn = id == "cloud" && unsaved.is_some();
        let colour = if warn {
            t.caution
        } else if resp.hovered() {
            t.text
        } else {
            t.icon
        };
        paint(ui.painter(), r.shrink(5.0), icon, colour);
        if warn {
            let c = r.right_top() + vec2(-5.0, 6.0);
            ui.painter().circle_filled(c, 6.0, t.reject);
            ui.painter().text(c, Align2::CENTER_CENTER, "!", t.semibold(9.5), t.canvas);
            register(ui, "indicator:unsaved", r);
        }
        // a refusal says why (Import while a Lightroom catalog is coming in)
        if resp.clicked()
            && !cmd.is_empty()
            && let Err(e) = app.run(cmd, json!({}))
        {
            app.toast_error(ui.ctx(), e);
        }
        x -= 40.0;
    }
    x
}

/// The appearance icon at `x`, with its theme-favourites menu.
fn appearance_button(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, full: Rect, x: f32) {
    // Appearance Mode: a click steps Auto → Light → Dark → Auto; the icon shows the mode
    let (icon, mode) = appearance_icon(app.ui.settings.appearance_mode);
    let r = Rect::from_center_size(pos2(x, full.center().y), vec2(28.0, 28.0));
    let tip = format!("{}: {}", crate::i18n::tr("Appearance Mode"), crate::i18n::tr(mode));
    let resp = ui.interact(r, egui::Id::unique(("top", "appearance")), Sense::click()).on_hover_text(tip.clone());
    crate::widgets::announce(&resp, tip.clone());
    register(ui, "icon:appearance", r);
    paint(ui.painter(), r.shrink(5.0), icon, if resp.hovered() { t.text } else { t.icon });
    if resp.clicked() {
        let _ = app.run("view.appearance", json!({}));
    }
    crate::widgets::context_menu(&resp, |ui| {
        let settings = ui.button(crate::i18n::tr("Appearance Settings…"));
        register(ui, "menu:appearanceSettings", settings.rect);
        if settings.clicked() {
            let _ = app.run("app.settings", json!({"tab": "interface"}));
            ui.close();
        }
        let favorites = app.ui.settings.favorite_themes.clone();
        for dark in [false, true] {
            let group: Vec<_> =
                crate::theme::ThemeKind::ALL.iter().filter(|k| k.is_dark() == dark && favorites.iter().any(|id| id == k.id())).collect();
            if !group.is_empty() {
                ui.separator();
            }
            for kind in group {
                let r = ui.selectable_label(app.ui.settings.theme(app.system_theme(ui.ctx())) == *kind, crate::i18n::tr(kind.label()));
                register(ui, format!("menu:themeFavorite-{}", kind.id()), r.rect);
                if r.clicked() {
                    let _ = app.run("view.theme", json!({"theme": kind.id()}));
                    ui.close();
                }
            }
        }
    });
}

/// The appearance button's icon and the mode's name for its tooltip.
pub fn appearance_icon(mode: crate::state::AppearanceMode) -> (Icon, &'static str) {
    use crate::state::AppearanceMode;
    match mode {
        AppearanceMode::Auto => (Icon::Monitor, "Sync with System"),
        AppearanceMode::Light => (Icon::Sun, "Light Mode"),
        AppearanceMode::Dark => (Icon::Moon, "Dark Mode"),
    }
}

/// Tooltip for the warning cloud icon: `n` changes held in memory because saving failed with `e`.
fn unsaved_tip(n: usize, e: &str) -> String {
    let s = crate::i18n::plural_s(n);
    let it = if n == 1 { "it" } else { "them" };
    crate::i18n::tr_format!(
        "{n} change{} saved in memory but not written to disk: {e}\nLightCraft retries automatically; quitting now would lose {}.",
        s,
        it,
        e = e,
        n = n
    )
}
