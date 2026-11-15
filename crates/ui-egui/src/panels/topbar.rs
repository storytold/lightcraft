//! The top bar: sidebar toggle, back/forward, search, filter, and the right-hand icons.

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

/// The bar doubles as the window's title bar: dragging its empty space moves the window and a
/// double-click zooms or restores it. Registered before the bar's widgets so they win the click.
fn window_handle(app: &LccApp, ui: &mut egui::Ui, bar: Rect, search_rect: Rect) {
    use crate::titlebar::{Gesture, WindowState, command_for};
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

pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
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
            // The whole bar, its margins included. The search field is centred on this and not on
            // `full`, which is only what the margins leave: on macOS the left margin holds the
            // traffic lights' room, and full screen gives that room back. Centring on `full` moved
            // the field, and changed its width, every time that room changed (issue #2365).
            let bar = Rect::from_min_max(pos2(full.left() - left as f32, full.top()), pos2(full.right() + 12.0, full.bottom()));
            // The field stops short of the filter icon and the seven icons on the right; centred,
            // it is twice the room between the window's centre and them.
            let mut sw = 640.0f32.min(2.0 * (full.right() - 310.0 - bar.center().x)).max(190.0);
            let mut cx = bar.center().x;
            if !app.native_menu {
                // leave room for the in-window menus left of the (centred) search field
                let menus_right = full.left() + 140.0 + crate::menubar::bar_width(ui, app.ui.module) + 24.0;
                sw = sw.min(2.0 * (cx - menus_right)).max(190.0);
                // the Import | Library tabs leave the menus no room beside a centred field: the
                // field moves right of them, while it still fits before the icons
                if cx - sw / 2.0 < menus_right && menus_right + sw <= full.right() - 310.0 {
                    cx = menus_right + sw / 2.0;
                }
            }
            let search_left = cx - sw / 2.0;
            let sr = Rect::from_center_size(pos2(cx, full.center().y), vec2(sw, 28.0));
            if app.integrated_titlebar {
                window_handle(app, ui, bar, sr);
            }
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if icon_button(ui, "sidebar", Icon::Sidebar, vec2(30.0, 30.0), app.ui.left_panel, true, crate::i18n::tr("Show/hide My Photos panel"))
                    .clicked()
                {
                    let _ = app.run("view.leftPanel", json!({}));
                }
                if !app.native_menu {
                    // no native menu bar (web, Windows, Linux): menus in the top bar
                    ui.add_space(10.0);
                    let room = search_left - ui.cursor().left() - 16.0;
                    crate::menubar::show_in_window(app, ui, room);
                }
            });
            // search field (centred on the window); with semantic search switched off (Settings ▸ AI Models) it has no AI
            // button and searches the text
            let semantic_on = app.session.semantic_search_on();
            if !semantic_on {
                app.ui.semantic_search = false;
            }
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
            let ai_r = Rect::from_min_size(pos2(sr.right() - 32.0, sr.center().y - 10.0), vec2(26.0, 20.0));
            let text_sr = Rect::from_min_max(sr.min + vec2(10.0, 4.0), pos2(ai_r.left() - 4.0, sr.max.y - 4.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_sr).layout(egui::Layout::left_to_right(egui::Align::Center)));
            let empty = app.ui.search.is_empty();
            let placeholder = if app.ui.semantic_search { crate::i18n::tr("Describe photos…") } else { crate::i18n::tr("Search Photos") };
            if empty && !focused {
                let g = child.painter().layout_no_wrap(placeholder.into(), t.font(13.5), t.text_dim);
                let w = g.size().x + 24.0;
                let x0 = (sr.center().x - w / 2.0).max(text_sr.left());
                paint(child.painter(), Rect::from_min_size(pos2(x0, sr.center().y - 8.0), vec2(16.0, 16.0)), Icon::Search, t.text_dim);
                child.painter().galley(pos2(x0 + 24.0, sr.center().y - g.size().y / 2.0), g, t.text_dim);
            }
            let resp = crate::text_field::TextField::singleline("field:search", &mut app.ui.search)
                .label(placeholder)
                .frame(false)
                .width(text_sr.width().max(20.0))
                .font(t.font(13.5))
                .text_color(t.text)
                .show(&mut child)
                .response;
            if resp.changed() {
                let q = app.ui.search.clone();
                let p = if app.ui.semantic_search { json!({"semantic": q}) } else { json!({"text": q}) };
                if let Err(e) = app.run("library.filter", p) {
                    app.toast_error(ui.ctx(), e);
                } else if app.ui.semantic_search && !q.trim().is_empty() && app.session.selected_ml_model().is_none() {
                    app.toast_with_action(
                        ui.ctx(),
                        crate::i18n::tr("Download the semantic search model to search by description."),
                        crate::i18n::tr("AI Models…"),
                        "dialog.aiModels",
                        json!({}),
                        8.0,
                    );
                }
            }
            if semantic_on {
                let ai_resp =
                    ui.interact(ai_r, egui::Id::new("search-ai-btn"), Sense::click()).on_hover_text(crate::i18n::tr("Natural-language search (AI)"));
                ai_resp.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Button,
                        true,
                        app.ui.semantic_search,
                        crate::i18n::tr("Natural-language search (AI)"),
                    )
                });
                register(ui.ctx(), "search:ai", ai_r);
                ui.painter().rect_filled(
                    ai_r,
                    3.0,
                    if app.ui.semantic_search {
                        t.accent
                    } else if ai_resp.hovered() {
                        t.hover
                    } else {
                        egui::Color32::TRANSPARENT
                    },
                );
                ui.painter().text(
                    ai_r.center(),
                    Align2::CENTER_CENTER,
                    crate::i18n::tr("AI"),
                    t.semibold(10.5),
                    if app.ui.semantic_search {
                        t.canvas
                    } else if ai_resp.hovered() {
                        t.text
                    } else {
                        t.text_dim
                    },
                );
                if ai_resp.clicked() {
                    app.ui.semantic_search = !app.ui.semantic_search;
                    let q = app.ui.search.clone();
                    if !q.trim().is_empty() {
                        let p = if app.ui.semantic_search {
                            json!({"text": "", "semantic": q})
                        } else {
                            json!({"semantic": serde_json::Value::Null, "text": q})
                        };
                        if let Err(e) = app.run("library.filter", p) {
                            app.toast_error(ui.ctx(), e);
                        } else if app.ui.semantic_search && app.session.selected_ml_model().is_none() {
                            app.toast_with_action(
                                ui.ctx(),
                                crate::i18n::tr("Download the semantic search model to search by description."),
                                crate::i18n::tr("AI Models…"),
                                "dialog.aiModels",
                                json!({}),
                                8.0,
                            );
                        }
                    }
                }
            }
            // filter icon right of the search field
            let fr = Rect::from_center_size(pos2(sr.right() + 22.0, sr.center().y), vec2(28.0, 28.0));
            let fresp = ui.interact(fr, egui::Id::new("filter-btn"), Sense::click());
            crate::widgets::announce(&fresp, crate::i18n::tr("Filters"));
            register(ui.ctx(), "icon:filter", fr);
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
            let active = lcc_engine::filter_chips(&app.session.filter, &app.session.catalog).len();
            if active > 0 {
                let c = fr.right_top() + vec2(-3.0, 8.0);
                ui.painter().circle_filled(c, 7.0, t.accent);
                ui.painter().text(c, Align2::CENTER_CENTER, active.to_string(), t.semibold(9.5), t.canvas);
            }
            let fresp = fresp.on_hover_text(if active > 0 {
                crate::i18n::tr_format!("Filter bar: {active} active filter{}", if active == 1 { "" } else { "s" }, active = active)
            } else {
                crate::i18n::tr("Filter bar").into()
            });
            if fresp.clicked() {
                let _ = app.run("view.filterBar", json!({}));
            }
            // right icons; rightmost, mirroring the sidebar button: show/hide the module bar
            let mut x = full.right() - 18.0;
            let r = Rect::from_center_size(pos2(x, full.center().y), vec2(30.0, 30.0));
            let mut top = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
            if icon_button(&mut top, "moduleBar", Icon::SidebarTop, vec2(30.0, 30.0), app.ui.module_bar, true, crate::i18n::tr("Show/hide top panel"))
                .clicked()
            {
                let _ = app.run("panel.top", json!({}));
            }
            x -= 38.0;
            // show/hide the right panel (it used to head the tool strip at the far right)
            let r = Rect::from_center_size(pos2(x, full.center().y), vec2(30.0, 30.0));
            let mut right = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
            let develop = app.ui.module == crate::module::ModuleId::Develop;
            let closed = develop && app.ui.right == crate::state::RightPanel::None;
            let open = crate::module::module_edge(app, crate::module::Edge::Right) && !closed;
            if icon_button(&mut right, "collapseRight", Icon::SidebarRight, vec2(30.0, 30.0), open, true, crate::i18n::tr("Show/hide right panel"))
                .clicked()
            {
                let _ = app.run(if closed { "panel.edit" } else { "panel.collapseRight" }, json!({}));
            }
            x -= 42.0;
            // saving is failing: the cloud icon turns into a warning until a save succeeds
            let unsaved = app.session.unsaved().map(|(n, e)| {
                crate::i18n::tr_format!(
                    "{n} change{} saved in memory but not written to disk: {e}\n{app} retries automatically; quitting now would lose {}.",
                    if n == 1 { "" } else { "s" },
                    if n == 1 { "it" } else { "them" },
                    e = e,
                    n = n
                )
            });
            let cloud_tip = unsaved.as_deref().unwrap_or("Sync: Devices or Immich");
            #[cfg(not(target_arch = "wasm32"))]
            let sync_waiting = super::peer_sync::waiting(app);
            #[cfg(target_arch = "wasm32")]
            let sync_waiting = 0usize;
            // Import Photos… beside Export, always one click away, with the key it is bound to now
            let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
            let import_tip = match crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "dialog.import")
                .or_else(|| crate::shortcuts::shortcut_of(&app.ui.settings.keymap, "file.addPhotos"))
            {
                Some(sc) => format!("{} ({})", crate::i18n::tr("Import…"), crate::menubar::shortcut_text(sc, mac)),
                None => crate::i18n::tr("Import…").to_string(),
            };
            for (id, icon, tip, cmd) in [
                ("cloud", Icon::Cloud, cloud_tip, ""),
                ("help", Icon::Help, "Keyboard shortcuts", "app.shortcuts"),
                ("share", Icon::Share, "Export", "file.export"),
                ("import", Icon::Plus, import_tip.as_str(), "dialog.import"),
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
                } else if id == "cloud" && sync_waiting > 0 {
                    // an invitation or a new collection waits for an answer
                    let c = r.right_top() + vec2(-5.0, 6.0);
                    ui.painter().circle_filled(c, 6.0, t.accent);
                    ui.painter().text(c, Align2::CENTER_CENTER, sync_waiting.min(9).to_string(), t.semibold(9.5), t.canvas);
                    register(ui.ctx(), "indicator:syncWaiting", r);
                }
                // a refusal says why (Import while a Lightroom catalog is coming in)
                if resp.clicked()
                    && !cmd.is_empty()
                    && let Err(e) = app.run(cmd, json!({}))
                {
                    app.toast_error(ui.ctx(), e);
                }
                // the cloud opens a menu of the sync services, each to its Settings page
                #[cfg(not(target_arch = "wasm32"))]
                if id == "cloud" {
                    egui::Popup::menu(&resp).show(|ui| {
                        for (key, label, tab) in [("peerSync", "Device Sync…", "sync"), ("immich", "Immich…", "connections")] {
                            let b = ui.button(crate::i18n::tr(label));
                            register(ui.ctx(), format!("menu:cloud-{key}"), b.rect);
                            if b.clicked() {
                                let _ = app.run("app.settings", json!({"tab": tab}));
                                ui.close();
                            }
                        }
                    });
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
            crate::widgets::context_menu(&resp, |ui| {
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

/// The appearance button's icon and the mode's name for its tooltip.
pub fn appearance_icon(mode: crate::state::AppearanceMode) -> (Icon, &'static str) {
    use crate::state::AppearanceMode;
    match mode {
        AppearanceMode::Auto => (Icon::Monitor, "Sync with System"),
        AppearanceMode::Light => (Icon::Sun, "Light Mode"),
        AppearanceMode::Dark => (Icon::Moon, "Dark Mode"),
    }
}
