//! The Modern layout's menu (fork-owned): the ☰ button in the top left opens the whole menu bar
//! across the top, with Shortcuts (Import, Export) open first and then every menu Classic has (File,
//! Edit, Library, Photo, Metadata, View, Window, Help), built from the same model
//! ([`crate::menubar::menu_bar`]) and run the same way. A tap outside, Back or Esc closes it. On a
//! narrow window the bar scrolls sideways.
//!
//! Own design.

use egui::{Align2, Color32, Id, Rect, Sense, Stroke, pos2, vec2};
use serde_json::{Value, json};

use crate::LccApp;
use crate::modern_dock::glass;
use crate::theme::Tokens;
use crate::widgets::register;

/// The first menu, open when the bar opens: the actions people reach for most.
pub const SHORTCUTS: &str = "Shortcuts";
/// Shortcuts' items: (key, label).
const SHORTCUT_ITEMS: [(&str, &str); 2] = [("import", "Import…"), ("export", "Export…")];

/// A shortcut's command: Export is the save-and-share sheet where the host keeps a gallery
/// (Android), else the Export dialog.
fn shortcut_command(app: &LccApp, key: &str) -> (&'static str, Value) {
    match key {
        "export" if app.services.write_shared.is_some() => ("modern.exportSheet", json!({"on": true})),
        "export" => ("dialog.export", json!({})),
        "histogram" => ("view.histogram", json!({})),
        _ => ("file.addPhotos", json!({})),
    }
}

const BAR_H: f32 = 40.0;

/// Opens the menu bar on Shortcuts, or closes it.
pub(crate) fn toggle(app: &mut LccApp) {
    app.ui.modern.menu = if app.ui.modern.menu.is_some() { None } else { Some(SHORTCUTS.to_string()) };
}

/// Draws the open menu bar from `left` (right of the ☰ button) across the top of `area`.
pub(crate) fn show(app: &mut LccApp, ui: &egui::Ui, area: Rect, left: f32, top: f32) {
    let Some(open) = app.ui.modern.menu.clone() else { return };
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let bar = menu_titles(app);
    let rect = Rect::from_min_max(pos2(left, top), pos2(area.right() - 12.0, top + BAR_H));
    let mut pick: Option<String> = None;
    let mut title_rect: Option<Rect> = None;
    egui::Area::new(Id::new("modern-menubar")).fixed_pos(rect.min).order(egui::Order::Foreground).show(&ctx, |ui| {
        let (bg, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
        register(ui.ctx(), "modern:menubar", bg);
        glass(ui.painter(), bg, BAR_H / 2.0, &t);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bg.shrink2(vec2(6.0, 4.0))));
        egui::ScrollArea::horizontal()
            .id_salt("modern-menubar-scroll")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(&mut child, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for title in &bar {
                        let on = *title == open;
                        let name = crate::i18n::tr(title);
                        let galley = ui.painter().layout_no_wrap(name.to_string(), t.semibold(13.0), t.text);
                        let (r, resp) = ui.allocate_exact_size(vec2(galley.size().x + 24.0, BAR_H - 8.0), Sense::click());
                        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, name));
                        register(ui.ctx(), format!("modern:menu:{title}"), r);
                        let shortcuts = title == SHORTCUTS;
                        let p = ui.painter();
                        if on {
                            p.rect_filled(r, r.height() / 2.0, if shortcuts { t.accent } else { Color32::from_white_alpha(40) });
                            title_rect = Some(r);
                        } else if resp.hovered() {
                            p.rect_filled(r, r.height() / 2.0, Color32::from_white_alpha(16));
                        }
                        let c = if on {
                            if shortcuts { Color32::WHITE } else { t.text }
                        } else if shortcuts {
                            Color32::from_rgb(246, 192, 111)
                        } else {
                            t.text_label
                        };
                        p.galley_with_override_text_color(r.center() - galley.size() / 2.0, galley, c);
                        if on {
                            resp.scroll_to_me(None);
                        }
                        if resp.clicked() {
                            pick = Some(title.clone());
                        }
                    }
                });
            });
    });
    if let Some(p) = pick {
        app.ui.modern.menu = Some(p);
        return;
    }
    // the open menu's items, under its title
    let under = title_rect.map_or(rect.left_bottom(), |r| pos2(r.left(), rect.bottom() + 6.0));
    let x = under.x.min(area.right() - 12.0 - 260.0).max(area.left() + 12.0);
    let mut clicked: Option<(String, Value)> = None;
    let mac = ctx.os() == egui::os::OperatingSystem::Mac;
    let drop = egui::Area::new(Id::new("modern-menu-drop")).fixed_pos(pos2(x, rect.bottom() + 6.0)).order(egui::Order::Foreground).show(&ctx, |ui| {
        // as a menu: rows without button frames, on one line each
        ui.style_mut().visuals.button_frame = false;
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        // its submenus (popups of their own) take the same glass
        let v = &mut ui.style_mut().visuals;
        v.window_fill = t.chrome.gamma_multiply(0.94);
        v.window_stroke = Stroke::new(1.0, Color32::from_white_alpha(28));
        v.window_corner_radius = egui::CornerRadius::same(12);
        v.window_shadow = egui::epaint::Shadow::NONE;
        egui::Frame::NONE
            .fill(t.chrome.gamma_multiply(0.94))
            .stroke(Stroke::new(1.0, Color32::from_white_alpha(28)))
            .corner_radius(14.0)
            .inner_margin(egui::Margin::same(6))
            .show(ui, |ui| {
                // (scrolls, with arrows, when it doesn't fit under the bar)
                crate::menu_level::level(ui, 1, Some(rect.bottom() + 6.0), |ui| {
                    if open == SHORTCUTS {
                        shortcuts(app, ui, &t, &mut clicked);
                    } else if let Some((_, nodes)) = crate::menubar::menu_bar(app).into_iter().find(|(title, _)| *title == open) {
                        crate::menubar::nodes_ui(ui, &nodes, mac, &mut clicked, 1, Some(rect.bottom() + 6.0));
                    }
                });
            });
    });
    register(&ctx, "modern:menuItems", drop.response.rect);
    if let Some((id, params)) = clicked {
        app.ui.modern.menu = None;
        if let Err(e) = crate::menubar::run_item(app, &id, params) {
            app.toast_error(&ctx, e);
        }
        return;
    }
    // a tap anywhere else closes it (a submenu's popup counts as inside)
    let tapped_away = ctx.input(|i| i.pointer.any_click())
        && !egui::Popup::is_any_open(&ctx)
        && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| !rect.contains(p) && !drop.response.rect.contains(p) && !over_button(app, p));
    if tapped_away {
        app.ui.modern.menu = None;
    }
}

fn over_button(app: &LccApp, p: egui::Pos2) -> bool {
    app.widgets.iter().any(|(w, r)| w == "modern:menu" && r.contains(p))
}

/// Shortcuts first, then the menu bar's titles.
pub(crate) fn menu_titles(app: &LccApp) -> Vec<String> {
    std::iter::once(SHORTCUTS.to_string()).chain(crate::menubar::menu_bar(app).into_iter().map(|(title, _)| title)).collect()
}

fn shortcuts(app: &LccApp, ui: &mut egui::Ui, t: &Tokens, clicked: &mut Option<(String, Value)>) {
    // while the histogram is hidden in Develop, the way back is here too
    let hidden = app.ui.module != crate::module::ModuleId::Library && !crate::modern::histogram_shown(app);
    let extra = hidden.then_some(("histogram", "Show Histogram"));
    for (key, label) in SHORTCUT_ITEMS.into_iter().chain(extra) {
        let (command, params) = shortcut_command(app, key);
        let enabled = command.starts_with("modern.") || crate::menus::ui_enabled(app, command);
        let (r, resp) = ui.allocate_exact_size(vec2(240.0, 44.0), if enabled { Sense::click() } else { Sense::hover() });
        let name = crate::i18n::tr(label);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name));
        register(ui.ctx(), format!("modern:shortcut:{key}"), r);
        if enabled && (resp.hovered() || resp.is_pointer_button_down_on()) {
            ui.painter().rect_filled(r, 10.0, Color32::from_white_alpha(22));
        }
        ui.painter().text(
            pos2(r.left() + 14.0, r.center().y),
            Align2::LEFT_CENTER,
            name,
            t.semibold(15.0),
            if enabled { t.text } else { t.text_disabled },
        );
        if resp.clicked() {
            *clicked = Some((command.to_string(), params));
        }
    }
}
