//! The top bar: sidebar toggle, back/forward, search, filter, and the right-hand icons.

use egui::{Align2, Rect, pos2, vec2};
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

/// The centred search field: the framework's search field (glyph, placeholder, clear part, focus ring).
/// The library filters as the text changes.
fn search_field(app: &mut LightcraftApp, ui: &mut egui::Ui, _t: &Tokens, sr: Rect) {
    use craft::ui::contract::{Bind, Props as _, Show as _};
    if std::mem::take(&mut app.ui.focus_search) {
        crate::text_field::focus(ui.ctx(), "field:search");
    }
    let before = app.ui.search.clone();
    let carrying = app.ui.dragging_photos.is_some() && ui.input(|i| i.pointer.primary_down());
    let (shown, record) = craft::ui::runtime::inspect::record(ui, crate::text_field::id("field:search").with("inspect"), |ui| {
        let mut field = craft::ui::text::TextField::search(crate::i18n::tr("Search Photos"), Bind::bound(&mut app.ui.search))
            .id(crate::text_field::id("field:search"))
            .placeholder(crate::i18n::tr("Search Photos"))
            .limit(None);
        // A right-click while photos are carried (the primary button held) opens no Cut / Copy / Paste menu: the
        // field's own menu is asked for and none is given.
        if carrying {
            field = field.own_context_menu();
        }
        field.show_at(ui, sr)
    });
    crate::text_field::register_field(ui, "field:search", &shown.response, record.as_deref());
    // Escape (which clears the text) and Return leave the field, so the next key is a shortcut again. An Escape in the
    // empty field is one the field leaves untaken (it goes on to the host): it lets go of the field too, and does
    // nothing else.
    let field = crate::text_field::id("field:search");
    let left_escape = !shown.escaped && crate::shortcuts::take_focused_cancel(ui.ctx(), |rec| rec.stop.id == field || rec.stop.root == field);
    if shown.escaped || shown.submitted || left_escape {
        crate::text_field::blur(ui.ctx(), "field:search");
    }
    if app.ui.search != before {
        let q = app.ui.search.clone();
        let _ = app.run("library.filter", json!({"text": q}));
    }
}

/// The filter icon beside the search field, with the active-filter badge.
fn filter_button(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, sr: Rect) {
    // filter icon right of the search field
    let fr = Rect::from_center_size(pos2(sr.right() + 22.0, sr.center().y), vec2(28.0, 28.0));
    let filtering = app.session.filter != Default::default() || app.ui.filter_bar;
    // badge: how many filters are on, even with the filter bar closed
    let active = lightcraft_engine::filter_chips(&app.session.filter, &app.session.catalog).len();
    let tip = if active > 0 {
        crate::i18n::tr_format!("Filter bar — {active} active filter{}", if active == 1 { "" } else { "s" }, active = active)
    } else {
        crate::i18n::tr("Filter bar").into()
    };
    let accent = if filtering { Some(t.accent) } else { None };
    let (fresp, live) = bar_icon(ui, fr, ("top", "filter"), "icon:filter", Icon::Filter, 16, accent.unwrap_or(t.icon), accent.unwrap_or(t.text), &tip);
    if live && active > 0 {
        let c = fr.right_top() + vec2(-3.0, 8.0);
        ui.painter().circle_filled(c, 7.0, t.accent);
        ui.painter().text(c, Align2::CENTER_CENTER, active.to_string(), t.semibold(9.5), t.canvas);
    }
    if fresp.is_some_and(|r| craft::ui::contract::clicked(&r)) {
        let _ = app.run("view.filterBar", json!({}));
    }
}

/// A top-bar icon on the framework's icon button: LightCraft's own vector artwork inside the SDK's face, so sensing,
/// keyboard activation, focus ring and accessibility come from the framework. `rest` and `hover` are the ink colours.
/// Returns the response (`None` while only measuring) and whether the pass is a live one.
#[allow(clippy::too_many_arguments)]
fn bar_icon(
    ui: &mut egui::Ui,
    rect: Rect,
    id: (&str, &str),
    automation: &str,
    icon: Icon,
    side: u8,
    rest: egui::Color32,
    hover: egui::Color32,
    tip: &str,
) -> (Option<egui::Response>, bool) {
    use craft::ui::buttons::IconButton;
    use craft::ui::contract::{Props as _, Show as _};
    use craft::ui::primitives::IconSpec;

    let id = egui::Id::unique(id);
    let tip = crate::i18n::tr(tip);
    let (label, hint) = split_key_hint(tip);
    // egui resolves hover from the prior pass's geometry before drawing this pass.
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let glyph = BarGlyph { icon, ink: if hovered { hover } else { rest }, side };
    let mut button = IconButton::new(IconSpec::glyph(&glyph)).id(id).label(label.to_owned());
    if !tip.is_empty() {
        button = button.tooltip(tip.to_owned());
    }
    if let Some(hint) = hint {
        button = button.description(hint.to_owned());
    }
    let shown = button.show_at(ui, rect);
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return (None, false);
    }
    crate::widgets::register_response(automation, rect, &shown.response);
    (Some(shown.response), true)
}

/// A tooltip that ends in its key hint ("Import Photos… (⌘⇧I)") is a label and a hint; the label is the accessible
/// name and the hint its description. The Japanese and Traditional Chinese catalogs use full-width brackets.
fn split_key_hint(tip: &str) -> (&str, Option<&str>) {
    let split =
        tip.strip_suffix(')').and_then(|body| body.rsplit_once(" (")).or_else(|| tip.strip_suffix('）').and_then(|body| body.rsplit_once('（')));
    let Some((label, hint)) = split else { return (tip, None) };
    let label = label.trim_end();
    if label.is_empty() || hint.is_empty() { (tip, None) } else { (label, Some(hint)) }
}

/// The right-side action icons; returns the x where the next icon would go.
fn right_icons(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, full: Rect) -> f32 {
    // right icons
    let mut x = full.right() - 18.0;
    // saving is failing: a warning shows until a save succeeds (nothing is shown otherwise)
    let unsaved = app.session.unsaved().map(|(n, e)| unsaved_tip(n, e));
    let cloud_tip = unsaved.as_deref().unwrap_or_default();
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
        let warn = id == "cloud" && unsaved.is_some();
        if id == "cloud" && !warn {
            continue;
        }
        let r = Rect::from_center_size(pos2(x, full.center().y), vec2(28.0, 28.0));
        let (rest, hover) = if warn { (t.caution, t.caution) } else { (t.icon, t.text) };
        let (resp, live) = bar_icon(ui, r, ("top", id), &format!("icon:{id}"), icon, 18, rest, hover, tip);
        if live && warn {
            let c = r.right_top() + vec2(-5.0, 6.0);
            ui.painter().circle_filled(c, 6.0, t.reject);
            ui.painter().text(c, Align2::CENTER_CENTER, "!", t.semibold(9.5), t.canvas);
            register(ui, "indicator:unsaved", r);
        }
        // a refusal says why (Import while a Lightroom catalog is coming in)
        if resp.is_some_and(|resp| craft::ui::contract::clicked(&resp))
            && !cmd.is_empty()
            && let Err(e) = app.run(cmd, json!({}))
        {
            app.toast_error(ui.ctx(), e);
        }
        x -= 40.0;
    }
    x
}

/// LightCraft's original vector artwork at its own point size; the SDK owns the button and its focus ring.
#[derive(craft::ui::contract::MemoProps)]
struct BarGlyph {
    icon: Icon,
    ink: egui::Color32,
    side: u8,
}

impl craft::ui::primitives::Glyph for BarGlyph {
    fn paint(&self, painter: &egui::Painter, _: &craft::ui::style::UiTokens, rect: Rect, _: egui::Color32, _: bool) {
        paint(painter, Rect::from_center_size(rect.center(), vec2(f32::from(self.side), f32::from(self.side))), self.icon, self.ink);
    }
}

/// The appearance icon at `x`, with its theme-favourites menu.
fn appearance_button(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, full: Rect, x: f32) {
    use craft::ui::buttons::IconButton;
    use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _};
    use craft::ui::menu::{Menu, MenuItem};
    use craft::ui::primitives::IconSpec;

    let (icon, mode) = appearance_icon(app.ui.settings.appearance_mode);
    let r = Rect::from_center_size(pos2(x, full.center().y), vec2(28.0, 28.0));
    let id = egui::Id::unique(("top", "appearance"));
    let tip = format!("{}: {}", crate::i18n::tr("Appearance Mode"), crate::i18n::tr(mode));
    // egui resolves hover from the prior pass's geometry before drawing this pass.
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let glyph = BarGlyph { icon, ink: if hovered { t.text } else { t.icon }, side: 18 };
    let anchor = IconButton::new(IconSpec::glyph(&glyph)).id(id).label(tip.clone()).tooltip(tip).show_at(ui, r);
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return;
    }
    let resp = &anchor.response;
    crate::widgets::register_response("icon:appearance", r, resp);
    // SDK primary activation includes keyboard and accessibility. Its rewritten CLICKED flag
    // is not egui's secondary-click flag. Read the *same* registered widget for that verdict;
    // this does not create another hit target or consume/replay an event.
    let secondary = ui.ctx().read_response(id).is_some_and(|raw| raw.enabled() && raw.secondary_clicked());
    let primary = anchor.clicked();
    if primary {
        let _ = app.run("view.appearance", json!({}));
    }
    let enabled = ui.is_enabled();
    let key = resp.id.with("appearance-menu-open");
    let mut open = ui.ctx().data(|data| data.get_temp::<bool>(key)).unwrap_or(false);
    // Only a click won by the icon may open the menu, not an arbitrary release over it.
    // The menu and its opener now share the SDK focus registry.
    if secondary && crate::widgets::carrying(ui.ctx()) {
        return;
    }
    if secondary && enabled {
        open = true;
    } else if primary {
        open = false;
    }
    let shown = Menu::context(crate::i18n::tr("Appearance Mode").to_owned(), resp).id_salt("appearance-context").open(Bind::bound(&mut open)).show(
        ui,
        |ui, _| {
            let live = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
            let row = MenuItem::new(crate::i18n::tr("Appearance Settings…").to_owned()).id_salt("menu:appearanceSettings").enabled(enabled).show(ui);
            if live {
                crate::widgets::register_response("menu:appearanceSettings", row.response.rect, &row.response);
                if row.ran {
                    let _ = app.run("app.settings", json!({"tab": "interface"}));
                }
            }
            let favorites = &app.ui.settings.favorite_themes;
            let selected = app.ui.settings.theme(app.system_theme(ui.ctx()));
            let mut choice = None;
            for dark in [false, true] {
                let mut first = true;
                for kind in crate::theme::ThemeKind::ALL.iter().filter(|kind| kind.is_dark() == dark && favorites.iter().any(|id| id == kind.id())) {
                    if first {
                        craft::ui::display::Divider::horizontal().show(ui);
                        first = false;
                    }
                    let id = format!("menu:themeFavorite-{}", kind.id());
                    let row =
                        MenuItem::new(crate::i18n::tr(kind.label()).to_owned()).id_salt(kind.id()).check(selected == *kind).enabled(enabled).show(ui);
                    if live {
                        crate::widgets::register_response(id, row.response.rect, &row.response);
                        if row.ran {
                            choice = Some(kind.id());
                        }
                    }
                }
            }
            if let Some(theme) = choice {
                let _ = app.run("view.theme", json!({"theme": theme}));
            }
        },
    );
    ui.ctx().data_mut(|data| {
        if open && shown.output.closed.is_none() {
            data.insert_temp(key, true);
        } else {
            data.remove::<bool>(key);
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

#[cfg(test)]
#[path = "appearance_settings_menu_port_tests.rs"]
mod appearance_settings_menu_port_tests;
