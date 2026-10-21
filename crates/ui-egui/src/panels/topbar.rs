//! The top bar: sidebar toggle, back/forward, search, filter, and the right-hand icons.

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::state::SearchMode;
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

/// The bar doubles as the window's title bar: dragging its empty space moves the window and a
/// double-click zooms or restores it. Registered before the bar's widgets so they win the click.
fn window_handle(app: &LightcraftApp, ui: &mut egui::Ui, content: Rect, margin_left: f32, margin_right: f32, search_rect: Rect) {
    use crate::titlebar::{Gesture, WindowState, command_for};
    let bar = Rect::from_min_max(pos2(content.left() - margin_left, content.top()), pos2(content.right() + margin_right, content.bottom()));
    register(ui.ctx(), "region:titlebar", bar);
    let handle = egui::Id::new("titlebar-handle");
    let resp = ui.interact(bar, handle, Sense::click_and_drag());
    crate::widgets::announce(&resp, crate::i18n::tr("Title bar"));
    // a press that began on a button or the search field is theirs: dragging off it must not move the window
    // (while pressing, egui hovers every widget under the pointer; once the drag starts, only the dragged one)
    let ctx = ui.ctx().clone();
    let memo = egui::Id::new("titlebar-press-on-widget");
    if resp.is_pointer_button_down_on() && ctx.input(|i| i.pointer.primary_pressed()) {
        let hovered = ctx.interaction_snapshot(|s| s.hovered.clone());
        let on_widget =
            hovered.iter().any(|id| *id != handle && ctx.read_response(*id).is_some_and(|r| r.sense.senses_click() || r.sense.senses_drag()));
        ctx.data_mut(|d| d.insert_temp(memo, on_widget));
    }
    let on_widget = ctx.data(|d| d.get_temp::<bool>(memo)).unwrap_or(false);
    // The search field is drawn after this handle. Its hit-test response may not exist
    // yet when the press is recorded; the press origin still identifies text selection.
    let on_search = ui.input(|i| i.pointer.press_origin().is_some_and(|p| search_rect.contains(p)));
    let gesture = if resp.double_clicked_by(egui::PointerButton::Primary) && !on_search {
        Some(Gesture::DoubleClicked)
    } else if resp.drag_started_by(egui::PointerButton::Primary) && !on_widget && !on_search {
        Some(Gesture::DragStarted)
    } else {
        None
    };
    if let Some(g) = gesture {
        let window = WindowState { maximized: ui.input(|i| i.viewport().maximized).unwrap_or(false), fullscreen: app.window_is_fullscreen };
        if let Some(cmd) = command_for(g, window) {
            ui.ctx().send_viewport_cmd(cmd);
        }
    }
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // room for the traffic lights; in full screen they are gone, and the sidebar button sits as far
    // from the left edge as the Discord button from the right (12 margin + 4)
    let left = match (app.integrated_titlebar, app.window_is_fullscreen) {
        (true, false) => 78,
        (true, true) => 16,
        (false, _) => 10,
    };
    egui::Panel::top("top_bar")
        .exact_size(t.top_bar_h)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left, right: 12, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            let full = ui.max_rect();
            // room for the filter icon and the seven icons on the right
            // (the field is centred, so each icon on the right takes its width from both sides)
            let mut sw = 640.0f32.min(full.width() - 620.0).max(190.0);
            if !app.native_menu {
                // leave room for the in-window menus left of the (centred) search field
                let menus_right = full.left() + 140.0 + crate::menubar::bar_width(ui) + 24.0;
                sw = sw.min(2.0 * (full.center().x - menus_right)).max(190.0);
            }
            let search_left = full.center().x - sw / 2.0;
            let sr = Rect::from_center_size(pos2(full.center().x, full.center().y), vec2(sw, 28.0));
            if app.integrated_titlebar {
                window_handle(app, ui, full, left as f32, 12.0, sr);
            }
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
            let contents_mode = app.ui.search_mode == SearchMode::Contents;
            // the magnifier at the field's left picks what is searched: Text or Contents
            let mr = Rect::from_min_size(pos2(sr.left() + 4.0, sr.center().y - 10.0), vec2(26.0, 20.0));
            let mresp = ui.interact(mr, egui::Id::new("search-mode"), Sense::click());
            register(ui.ctx(), "search:mode", mr);
            let mcol = if contents_mode {
                t.accent
            } else if mresp.hovered() {
                t.text
            } else {
                t.text_dim
            };
            paint(ui.painter(), Rect::from_min_size(pos2(mr.left() + 2.0, mr.center().y - 7.0), vec2(14.0, 14.0)), Icon::Search, mcol);
            // the caret says there is a menu
            let c = pos2(mr.right() - 5.0, mr.center().y + 1.0);
            ui.painter().add(egui::Shape::convex_polygon(vec![c + vec2(-3.0, -2.0), c + vec2(3.0, -2.0), c + vec2(0.0, 2.0)], mcol, Stroke::NONE));
            let mresp = mresp.on_hover_text(crate::i18n::tr("Search Text or Contents"));
            egui::Popup::menu(&mresp).show(|ui| {
                for (mode, label, id) in
                    [(SearchMode::Text, "Search Text", "menu:searchText"), (SearchMode::Contents, "Search Contents", "menu:searchContents")]
                {
                    let r = ui.selectable_label(app.ui.search_mode == mode, crate::i18n::tr(label));
                    register(ui.ctx(), id, r.rect);
                    if r.clicked() {
                        set_search_mode(app, mode);
                        ui.close();
                    }
                }
            });
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_max(pos2(mr.right() + 2.0, sr.top() + 4.0), pos2(sr.right() - 10.0, sr.bottom() - 4.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            let empty = app.ui.search.is_empty();
            if empty && !focused {
                let hint = if contents_mode { "Search photo contents (e.g. beach at sunset)" } else { "Search Photos" };
                let g = child.painter().layout_no_wrap(crate::i18n::tr(hint).into(), t.font(13.5), t.text_dim);
                let x0 = if contents_mode { mr.right() + 6.0 } else { sr.center().x - g.size().x / 2.0 };
                child.painter().galley(pos2(x0, sr.center().y - g.size().y / 2.0), g, t.text_dim);
            }
            let field = crate::text_field::TextField::singleline("field:search", &mut app.ui.search)
                .label(crate::i18n::tr("Search Photos"))
                .frame(false)
                .width(sr.right() - mr.right() - 14.0)
                .font(t.font(13.5))
                .text_color(t.text)
                .show(&mut child);
            let now = ui.input(|i| i.time);
            if field.response.changed() {
                let q = app.ui.search.clone();
                if contents_mode {
                    // a search by contents waits for a pause in the typing
                    app.ui.search_due = Some(now + SEARCH_DEBOUNCE);
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(SEARCH_DEBOUNCE));
                } else {
                    let _ = app.run("library.filter", json!({"text": q, "wait": false}));
                }
            }
            if contents_mode && (field.committed() || app.ui.search_due.is_some_and(|due| now >= due)) {
                app.ui.search_due = None;
                let q = app.ui.search.clone();
                let _ = app.run("library.filter", json!({"contents": q, "wait": false}));
            } else if let Some(due) = app.ui.search_due {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((due - now).max(0.01)));
            }
            // filter icon right of the search field
            let fr = Rect::from_center_size(pos2(sr.right() + 22.0, sr.center().y), vec2(28.0, 28.0));
            let fresp = ui.interact(fr, egui::Id::new("filter-btn"), Sense::click());
            crate::widgets::announce(&fresp, crate::i18n::tr("Filters"));
            register(ui.ctx(), "icon:filter", fr);
            let filtering = app.session.filter != Default::default() || app.ui.filter_bar || app.session.content_ask().is_some();
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
            // right icons
            let mut x = full.right() - 18.0;
            // saving is failing: the cloud icon turns into a warning until a save succeeds
            let unsaved = app.session.unsaved().map(|(n, e)| {
                crate::i18n::tr_format!(
                    "{n} change{} saved in memory but not written to disk: {e}\nLightCraft retries automatically; quitting now would lose {}.",
                    if n == 1 { "" } else { "s" },
                    if n == 1 { "it" } else { "them" },
                    e = e,
                    n = n
                )
            });
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
                let resp = ui.interact(r, egui::Id::new(("top", id)), Sense::click()).on_hover_text(crate::i18n::tr(tip));
                crate::widgets::announce(&resp, crate::i18n::tr(tip));
                register(ui.ctx(), format!("icon:{id}"), r);
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
                    register(ui.ctx(), "indicator:unsaved", r);
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
            // Appearance Mode: a click steps Auto → Light → Dark → Auto; the icon shows the mode
            let (icon, mode) = appearance_icon(app.ui.settings.appearance_mode);
            let r = Rect::from_center_size(pos2(x, full.center().y), vec2(28.0, 28.0));
            let tip = format!("{}: {}", crate::i18n::tr("Appearance Mode"), crate::i18n::tr(mode));
            let resp = ui.interact(r, egui::Id::new(("top", "appearance")), Sense::click()).on_hover_text(tip.clone());
            crate::widgets::announce(&resp, tip.clone());
            register(ui.ctx(), "icon:appearance", r);
            paint(ui.painter(), r.shrink(5.0), icon, if resp.hovered() { t.text } else { t.icon });
            if resp.clicked() {
                let _ = app.run("view.appearance", json!({}));
            }
            resp.context_menu(|ui| {
                let settings = ui.button(crate::i18n::tr("Appearance Settings…"));
                register(ui.ctx(), "menu:appearanceSettings", settings.rect);
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
                        register(ui.ctx(), format!("menu:themeFavorite-{}", kind.id()), r.rect);
                        if r.clicked() {
                            let _ = app.run("view.theme", json!({"theme": kind.id()}));
                            ui.close();
                        }
                    }
                }
            });
            let _ = Align2::CENTER_CENTER;
        });
}

/// Seconds a search by contents waits after the last key typed.
pub const SEARCH_DEBOUNCE: f64 = 0.25;

/// Switch the search field between Text and Contents, taking what is typed along: the text search becomes a search by
/// contents or the other way round. Choosing Contents starts loading the search model, so the first search does not
/// wait for it.
pub fn set_search_mode(app: &mut LightcraftApp, mode: SearchMode) {
    if app.ui.search_mode == mode {
        return;
    }
    app.ui.search_mode = mode;
    app.ui.search_due = None;
    let q = app.ui.search.trim().to_string();
    match mode {
        SearchMode::Contents => {
            let _ = app.run("content.prepare", json!({}));
            if !q.is_empty() {
                let _ = app.run("library.filter", json!({"text": "", "contents": q, "wait": false}));
            }
        }
        SearchMode::Text => {
            let _ = app.run("library.filter", json!({"contents": null, "text": q, "wait": false}));
        }
    }
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
