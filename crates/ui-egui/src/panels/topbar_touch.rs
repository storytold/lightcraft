//! The top bar: sidebar toggle, back/forward, search, filter, and the right-hand icons.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

/// The bar doubles as the window's title bar (a no-op drag area on the iPad, which has no windows to move); made before
/// the bar's widgets so they win the click.
fn window_handle(_app: &LightcraftApp, ui: &mut egui::Ui, content: Rect, margin_left: f32, margin_right: f32) {
    let bar = Rect::from_min_max(pos2(content.left() - margin_left, content.top()), pos2(content.right() + margin_right, content.bottom()));
    register(ui, "region:titlebar", bar);
    let resp = craft_window::title_bar::empty_area(ui, bar);
    crate::widgets::announce(&resp, crate::i18n::tr("Title bar"));
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let touch = crate::theme::touch_ui(ui.ctx());
    let insets = crate::theme::safe_area_insets(ui.ctx());
    let safe_top = insets[0].max(if touch { 28.0 } else { 0.0 });
    let safe_left = insets[2];
    let safe_right = insets[3];

    let left: f32 = if touch {
        14.0f32 + safe_left
    } else if app.integrated_titlebar {
        78.0f32
    } else {
        10.0f32
    };
    let right: f32 = if touch { 14.0f32 + safe_right } else { 12.0f32 };
    let total_h = t.top_bar_h + safe_top;

    let glass = crate::glass::active(ui.ctx());
    let frame = if glass {
        // a floating glass shell below the status bar
        crate::glass::shell(
            egui::Margin { left: 12, right: 12, top: 0, bottom: 0 },
            egui::Margin { left: 10 + safe_left.round() as i8, right: 10 + safe_right.round() as i8, top: safe_top.round() as i8 + 4, bottom: 4 },
        )
    } else {
        egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin {
            left: left.round() as i8,
            right: right.round() as i8,
            top: safe_top.round() as i8,
            bottom: 0,
        })
    };
    egui::Panel::top("top_bar").exact_size(if glass { t.top_bar_h + safe_top + 8.0 } else { total_h }).frame(frame).show(ui, |ui| {
        let full = ui.max_rect();
        if app.integrated_titlebar && !touch {
            window_handle(app, ui, full, left, 12.0);
        }
        let btn_size = if touch { vec2(38.0, 38.0) } else { vec2(30.0, 30.0) };
        // Split View's smallest widths: only what cannot be reached elsewhere stays (the bottom bar has undo / redo, History has the bell)
        let compact = touch && full.width() < 620.0;
        let mut left_cluster_end = full.left();
        // desktop: the search field is centred on the window, with room for the in-window menus left of it
        let mut desk_sw = 640.0f32.min(full.width() - 540.0).max(200.0);
        if !app.native_menu && !touch {
            let menus_right = full.left() + 140.0 + crate::menubar::bar_width(ui) + 24.0;
            desk_sw = desk_sw.min(2.0 * (full.center().x - menus_right)).max(200.0);
        }
        let desk_search_left = full.center().x - desk_sw / 2.0;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = if touch { 8.0 } else { 4.0 };
            if icon_button(ui, "sidebar", Icon::Sidebar, btn_size, app.ui.left_panel, true, crate::i18n::tr("Show/hide My Photos panel")).clicked() {
                let _ = app.run("view.leftPanel", json!({}));
            }
            ui.add_space(if touch { 4.0 } else { 8.0 });
            if touch {
                ui.add_space(4.0);
            }
            // (view history is the first thing to go when the bar is tight: portrait, Split View)
            if !(touch && full.width() < 900.0) {
                if icon_button(ui, "back", Icon::Back, btn_size, false, true, crate::i18n::tr("Back")).clicked() {
                    let _ = app.run("view.back", json!({}));
                }
                icon_button(ui, "forward", Icon::Forward, btn_size, false, false, crate::i18n::tr("Forward"));
            }
            if touch && !compact {
                // undo / redo for edits, ratings and flags (the arrows above move between views)
                ui.add_space(6.0);
                let can_undo = !app.session.undo.is_empty();
                let can_redo = !app.session.redo.is_empty();
                if icon_button(ui, "undo", Icon::Undo, btn_size, false, can_undo, crate::i18n::tr("Undo")).clicked() {
                    let _ = app.run("edit.undo", json!({}));
                }
                if icon_button(ui, "redo", Icon::Redo, btn_size, false, can_redo, crate::i18n::tr("Redo")).clicked() {
                    let _ = app.run("edit.redo", json!({}));
                }
            }
            if !app.native_menu {
                ui.add_space(if touch { 6.0 } else { 10.0 });
                let room = if touch { 90.0 } else { desk_search_left - ui.cursor().left() - 16.0 };
                crate::menubar::show_in_window(app, ui, room);
            }
            left_cluster_end = ui.cursor().left();
        });

        // Dynamically calculate non-overlapping bounds for search and filter
        // Select mode adds Done, Deselect All and the count badge to the right-hand group
        // (always reserved on the iPad, so the search bar stays where it is when Select is turned on or off)
        // Wide windows reserve it always; narrow ones (portrait, Split View) make room only while Select is on.
        let narrow = touch && full.width() < 1000.0;
        let select_extra = if narrow {
            if app.ui.multi_select { 80.0 } else { 0.0 }
        } else if touch || app.ui.multi_select {
            240.0
        } else {
            0.0
        };
        let right_reserved = (if touch {
            if compact {
                190.0
            } else if narrow {
                302.0
            } else {
                210.0
            }
        } else {
            320.0
        }) + select_extra;
        let search_left_bound = left_cluster_end + if touch { 12.0 } else { 16.0 };
        let search_right_bound = (full.right() - right_reserved).max(search_left_bound + 100.0);
        let available_search_w = search_right_bound - search_left_bound;
        let sw = if touch { available_search_w.clamp(110.0, 360.0) } else { available_search_w.clamp(160.0, 560.0) };
        let search_center_x = (search_left_bound + search_right_bound) / 2.0;
        let search_h = if touch { 36.0 } else { 28.0 };
        // narrow: the field starts right after the buttons (never over them) and takes what is left; wide: centred in its space
        let sr = if !touch {
            Rect::from_center_size(pos2(full.center().x, full.center().y), vec2(desk_sw, search_h))
        } else if narrow {
            Rect::from_min_size(pos2(search_left_bound, full.center().y - search_h / 2.0), vec2(available_search_w.clamp(120.0, 360.0), search_h))
        } else {
            Rect::from_center_size(pos2(search_center_x, full.center().y), vec2(sw, search_h))
        };
        let id = crate::text_field::id("field:search");
        if std::mem::take(&mut app.ui.focus_search) {
            ui.memory_mut(|m| m.request_focus(id));
        }
        let focused = ui.memory(|m| m.has_focus(id));
        let search_corner = if touch { search_h / 2.0 } else { 4.0 };
        let dark = ui.visuals().dark_mode;
        let search_bg = if focused {
            t.canvas
        } else if touch {
            if dark { Color32::from_rgba_unmultiplied(255, 255, 255, 16) } else { Color32::from_rgba_unmultiplied(0, 0, 0, 14) }
        } else {
            t.field
        };
        let search_stroke = if focused {
            t.accent
        } else if touch {
            if dark { Color32::from_rgba_unmultiplied(255, 255, 255, 26) } else { Color32::from_rgba_unmultiplied(0, 0, 0, 20) }
        } else {
            t.field_border
        };
        ui.painter().rect(sr, search_corner, search_bg, Stroke::new(1.0, search_stroke), StrokeKind::Inside);
        let mut child =
            ui.new_child(egui::UiBuilder::new().max_rect(sr.shrink2(vec2(10.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let empty = app.ui.search.is_empty();
        let search_font = t.font(if touch { 14.0 } else { 13.5 });
        if empty && !focused {
            let g = child.painter().layout_no_wrap(crate::i18n::tr("Search Photos").into(), search_font.clone(), t.text_dim);
            let w = g.size().x + 24.0;
            let x0 = sr.center().x - w / 2.0;
            paint(child.painter(), Rect::from_min_size(pos2(x0, sr.center().y - 8.0), vec2(16.0, 16.0)), Icon::Search, t.text_dim);
            child.painter().galley(pos2(x0 + 24.0, sr.center().y - g.size().y / 2.0), g, t.text_dim);
        }
        let resp = crate::text_field::TextField::singleline("field:search", &mut app.ui.search)
            .label(crate::i18n::tr("Search Photos"))
            .frame(false)
            .width(sr.width() - 20.0)
            .font(search_font)
            .text_color(t.text)
            .show(&mut child)
            .response;
        if resp.changed() {
            let q = app.ui.search.clone();
            let _ = app.run("library.filter", json!({"text": q}));
        }
        // filter icon right of the search field, safely clamped
        let fr_size = if touch { vec2(34.0, 34.0) } else { vec2(28.0, 28.0) };
        let fr_x = if touch { (sr.right() + 20.0).min(full.right() - 170.0) } else { sr.right() + 22.0 };
        let fr = Rect::from_center_size(pos2(fr_x, sr.center().y), fr_size);
        let fresp = ui.interact(touch_hit(fr), egui::Id::unique("filter-btn"), Sense::click());
        crate::widgets::announce(&fresp, crate::i18n::tr("Filters"));
        register(ui, "icon:filter", fr);
        let filtering = app.session.filter != Default::default() || app.ui.filter_bar;
        paint(
            ui.painter(),
            fr.shrink(if touch { 7.0 } else { 6.0 }),
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
        let mut x = full.right() - if touch { 20.0 } else { 18.0 };
        let icon_size = if touch { vec2(36.0, 36.0) } else { vec2(28.0, 28.0) };
        let icon_step = if touch { 44.0 } else { 40.0 };
        let icon_shrink = if touch { 7.0 } else { 5.0 };

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
        let icons_list: &[(&str, Icon, &str, &str)] = if compact {
            &[("share", Icon::Share, "Export", "dialog.export")]
        } else if touch {
            &[
                ("cloud", Icon::Cloud, cloud_tip, ""),
                ("share", Icon::Share, "Export", "dialog.export"),
                ("bell", Icon::Bell, "Activity", "panel.activity"),
            ]
        } else {
            &[
                ("discord", Icon::Chat, "Join the ArtCraft community on Discord", "app.discord"),
                ("cloud", Icon::Cloud, cloud_tip, ""),
                ("help", Icon::Help, "Keyboard shortcuts", "app.shortcuts"),
                ("share", Icon::Share, "Export", "dialog.export"),
                ("bell", Icon::Bell, "Activity", "panel.activity"),
            ]
        };
        for &(id, icon, tip, cmd) in icons_list {
            let r = Rect::from_center_size(pos2(x, full.center().y), icon_size);
            let resp = ui.interact(touch_hit(r), egui::Id::unique(("top", id)), Sense::click()).on_hover_text(crate::i18n::tr(tip));
            crate::widgets::announce(&resp, crate::i18n::tr(tip));
            register(ui, format!("icon:{id}"), r);
            let warn = id == "cloud" && unsaved.is_some();
            let colour = if warn {
                t.caution
            } else if resp.hovered() {
                t.text
            } else {
                t.icon
            };
            paint(ui.painter(), r.shrink(icon_shrink), icon, colour);
            if warn {
                let c = r.right_top() + vec2(-5.0, 6.0);
                ui.painter().circle_filled(c, 6.0, t.reject);
                ui.painter().text(c, Align2::CENTER_CENTER, "!", t.semibold(9.5), t.canvas);
                register(ui, "indicator:unsaved", r);
            }
            if touch && id == "share" && app.session.selection.ids.len() > 1 {
                let sel_n = app.session.selection.ids.len();
                let badge_text = if sel_n > 99 { "99+".to_string() } else { sel_n.to_string() };
                let g = ui.painter().layout_no_wrap(badge_text, t.semibold(9.0), Color32::WHITE);
                let badge_r = Rect::from_center_size(r.right_top() + vec2(-4.0, 4.0), vec2(g.size().x + 8.0, 14.0));
                ui.painter().rect_filled(badge_r, 7.0, t.accent);
                ui.painter().galley(badge_r.center() - g.size() / 2.0, g, Color32::WHITE);
            }
            if resp.clicked() && !cmd.is_empty() {
                let _ = app.run(cmd, json!({}));
            }
            x -= icon_step;
        }
        // Multi-Select toggle & actions on touch or multi-select mode
        if touch || app.ui.multi_select {
            let sel_label = if app.ui.multi_select { "Done" } else { "Select" };
            let is_active = app.ui.multi_select;
            let btn_w = 58.0;
            let btn_h = if touch { 32.0 } else { 28.0 };
            let r = Rect::from_center_size(pos2(x - btn_w / 2.0 + 8.0, full.center().y), vec2(btn_w, btn_h));
            let resp = ui.interact(touch_hit(r), egui::Id::unique("topbar:select_mode"), Sense::click());
            crate::widgets::announce(&resp, crate::i18n::tr(sel_label));
            register(ui, "button:multiSelect", r);
            let (fill, stroke_rim, text_col) = if is_active {
                (t.accent, Stroke::NONE, Color32::WHITE)
            } else if resp.hovered() || resp.is_pointer_button_down_on() {
                (Color32::from_rgba_unmultiplied(255, 255, 255, 40), Stroke::NONE, t.text)
            } else {
                (Color32::from_rgba_unmultiplied(255, 255, 255, 22), Stroke::NONE, t.text)
            };
            ui.painter().rect(r, 16.0, fill, stroke_rim, StrokeKind::Inside);
            let g = ui.painter().layout_no_wrap(crate::i18n::tr(sel_label).into(), t.semibold(12.5), text_col);
            ui.painter().galley(r.center() - g.size() / 2.0, g, text_col);
            if resp.clicked() {
                let _ = app.run("view.multiSelect", json!({}));
            }
            x -= btn_w + 10.0;

            // (Select All / Deselect All lives in the bottom bar with Export: one place, not two; the count badge
            // goes when the bar is narrow, the grid header and Export button already say how many)
            if is_active && !narrow {
                let sel_count = app.session.selection.ids.len();
                let cnt_str = format!("\u{2713} {sel_count}");
                let cg = ui.painter().layout_no_wrap(cnt_str, t.semibold(11.5), t.accent);
                let cr_w = cg.size().x + 16.0;
                let cr = Rect::from_center_size(pos2(x - cr_w / 2.0 + 6.0, full.center().y), vec2(cr_w, btn_h));
                ui.painter().rect_filled(cr, 16.0, Color32::from_rgba_unmultiplied(10, 132, 255, 40));
                ui.painter().galley(cr.center() - cg.size() / 2.0, cg, t.accent);
                x -= cr_w + 10.0;
            }
        }

        if !compact {
            // Appearance Mode: a click steps Auto → Light → Dark → Auto; the icon shows the mode
            let (icon, mode) = appearance_icon(app.ui.settings.appearance_mode);
            let r = Rect::from_center_size(pos2(x, full.center().y), icon_size);
            let tip = format!("{}: {}", crate::i18n::tr("Appearance Mode"), crate::i18n::tr(mode));
            let resp = ui.interact(touch_hit(r), egui::Id::unique(("top", "appearance")), Sense::click()).on_hover_text(tip.clone());
            crate::widgets::announce(&resp, tip);
            register(ui, "icon:appearance", r);
            paint(ui.painter(), r.shrink(icon_shrink), icon, if resp.hovered() { t.text } else { t.icon });
            if resp.clicked() {
                let _ = app.run("view.appearance", json!({}));
            }
            resp.context_menu(|ui| {
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
        // Lightroom Classic Module Switcher: [ LIBRARY | DEVELOP ]
        let left_bound = fr.right() + 16.0;
        let right_bound = x - icon_size.x / 2.0 - 12.0;
        if touch && right_bound > left_bound + 150.0 {
            use crate::state::{RightPanel, ViewMode};
            let is_develop = app.ui.right.is_edit_tool() || (app.ui.view == ViewMode::Detail && app.ui.right == RightPanel::Edit);
            let mod_w = if touch { 82.0 } else { 70.0 };
            let mod_h = if touch { 36.0 } else { 26.0 };
            let center_x = (left_bound + right_bound) / 2.0;
            let mod_rect = Rect::from_center_size(pos2(center_x, full.center().y), vec2(mod_w * 2.0 + 6.0, mod_h));

            let dark = ui.visuals().dark_mode;
            let container_radius = if touch { mod_h / 2.0 } else { 6.0 };
            let container_bg = if touch {
                if dark { Color32::from_rgba_unmultiplied(120, 120, 128, 48) } else { Color32::from_rgba_unmultiplied(120, 120, 128, 30) }
            } else {
                t.canvas.gamma_multiply(0.5)
            };
            let container_stroke = if touch {
                if dark { Color32::from_rgba_unmultiplied(255, 255, 255, 20) } else { Color32::from_rgba_unmultiplied(0, 0, 0, 24) }
            } else {
                t.field_border
            };
            ui.painter().rect(mod_rect, container_radius, container_bg, Stroke::new(1.0, container_stroke), StrokeKind::Inside);

            let pad = 2.0;
            let pill_h = mod_h - pad * 2.0;
            let pill_radius = if touch { pill_h / 2.0 } else { 4.0 };
            let lib_rect = Rect::from_min_size(mod_rect.min + vec2(pad, pad), vec2(mod_w, pill_h));
            let dev_rect = Rect::from_min_size(pos2(lib_rect.right() + 2.0, lib_rect.top()), vec2(mod_w, pill_h));

            let lib_resp = ui.interact(touch_hit(lib_rect), egui::Id::unique("module-library"), Sense::click());
            let dev_resp = ui.interact(touch_hit(dev_rect), egui::Id::unique("module-develop"), Sense::click());
            crate::widgets::announce(&lib_resp, crate::i18n::tr("Library"));
            crate::widgets::announce(&dev_resp, crate::i18n::tr("Develop"));

            register(ui, "module:library", lib_rect);
            register(ui, "module:develop", dev_rect);

            let active_fill = if touch {
                if dark { Color32::from_rgba_unmultiplied(255, 255, 255, 48) } else { Color32::from_rgba_unmultiplied(255, 255, 255, 230) }
            } else {
                t.accent.gamma_multiply(0.25)
            };
            let active_stroke = if touch {
                if dark { Color32::from_rgba_unmultiplied(255, 255, 255, 35) } else { Color32::from_rgba_unmultiplied(0, 0, 0, 24) }
            } else {
                t.accent
            };
            let active_text = if touch { if dark { Color32::WHITE } else { Color32::from_rgb(20, 20, 20) } } else { t.text };
            let hover_fill = if dark { Color32::from_rgba_unmultiplied(255, 255, 255, 18) } else { Color32::from_rgba_unmultiplied(0, 0, 0, 14) };

            if !is_develop {
                ui.painter().rect(lib_rect, pill_radius, active_fill, Stroke::new(1.0, active_stroke), StrokeKind::Inside);
            } else if lib_resp.hovered() || lib_resp.is_pointer_button_down_on() {
                ui.painter().rect_filled(lib_rect, pill_radius, hover_fill);
            }
            ui.painter().text(
                lib_rect.center(),
                Align2::CENTER_CENTER,
                crate::i18n::tr("Library"),
                t.semibold(if touch { 13.0 } else { 12.0 }),
                if !is_develop {
                    active_text
                } else if lib_resp.hovered() {
                    t.text
                } else {
                    t.text_dim
                },
            );

            if is_develop {
                ui.painter().rect(dev_rect, pill_radius, active_fill, Stroke::new(1.0, active_stroke), StrokeKind::Inside);
            } else if dev_resp.hovered() || dev_resp.is_pointer_button_down_on() {
                ui.painter().rect_filled(dev_rect, pill_radius, hover_fill);
            }
            ui.painter().text(
                dev_rect.center(),
                Align2::CENTER_CENTER,
                crate::i18n::tr("Develop"),
                t.semibold(if touch { 13.0 } else { 12.0 }),
                if is_develop {
                    active_text
                } else if dev_resp.hovered() {
                    t.text
                } else {
                    t.text_dim
                },
            );

            if lib_resp.clicked() && is_develop {
                app.ui.right = RightPanel::QuickDevelop;
                if app.ui.view == ViewMode::Detail {
                    app.ui.view = ViewMode::PhotoGrid;
                }
            } else if dev_resp.clicked() && !is_develop {
                app.ui.view = ViewMode::Detail;
                app.ui.right = RightPanel::Edit;
            }
        }

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

/// A finger's target for a control painted at `r`: at least 44pt each way, centred on it (the painted size is unchanged).
fn touch_hit(r: Rect) -> Rect {
    Rect::from_center_size(r.center(), vec2(r.width().max(44.0), r.height().max(44.0)))
}
