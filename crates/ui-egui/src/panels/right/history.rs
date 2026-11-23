use craft::ui::contract::{Props as _, Show as _, ShowContainer as _};
use craft::ui::display::Divider;
use crate::panels::detail::host;
use super::{Align2, Icon, LightcraftApp, PhotoId, Rect, Tokens, header, json, padded, paint, pos2, text_button, vec2};

pub(super) fn versions(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let Some(p) = app.session.catalog.photo(id).cloned() else { return };
    let measuring = ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx());
    // These commands resolve the active photo; a stale, disabled or speculative panel must not edit another target.
    let interactive = ui.is_enabled() && !measuring && app.session.active() == Some(id);
    header(ui, "Versions");
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        if !interactive {
            ui.disable();
        }
        if text_button(ui, "versionCreate", crate::i18n::tr("Create Version"), false).clicked() {
            let _ = app.run("version.create", json!({}));
        }
        ui.add_space(8.0);
        // Named (made by you) and Auto (made by LightCraft) versions
        let tab_id = egui::Id::unique("versions-tab");
        let mut auto: bool = ui.data(|d| d.get_temp(tab_id)).unwrap_or(false);
        let named_n = p.versions.iter().filter(|v| !v.auto).count();
        let items = [("Named", "named"), ("Auto", "auto")];
        let labels =
            [crate::i18n::tr_format!("Named ({named_n})", named_n = named_n), crate::i18n::tr_format!("Auto ({})", p.versions.len() - named_n)];
        let items: Vec<(&str, &str)> = items.iter().zip(&labels).map(|((_, k), l)| (l.as_str(), *k)).collect();
        if let Some(i) = crate::widgets::segmented(ui, "versionsTab", &items, Some(auto as usize), 2) {
            auto = i == 1;
            ui.data_mut(|d| d.insert_temp(tab_id, auto));
        }
        ui.add_space(6.0);
        let shown: Vec<(usize, &lightcraft_catalog::Version)> = p.versions.iter().enumerate().filter(|(_, v)| v.auto == auto).collect();
        if shown.is_empty() {
            let msg = if auto { "No automatic versions." } else { "No versions yet. Create one to keep this look." };
            ui.label(egui::RichText::new(crate::i18n::tr(msg)).color(t.text_dim));
        }
        // newest first
        for (i, v) in shown.into_iter().rev() {
            ui.push_id(("version-row", id.0, i), |ui| {
                let (_, r) = ui.allocate_space(vec2(ui.available_width(), 58.0));
                let current = *v.settings == *p.develop;
                let resp = host::target(
                    ui,
                    ("version-row", id.0, i),
                    r,
                    host::Target::new(format!("{} {}", crate::i18n::tr("Version"), i + 1)).tooltip(crate::i18n::tr(if current {
                        "The photo has these settings"
                    } else {
                        "Click to restore · double-click to rename"
                    })),
                );
                if !measuring {
                    crate::widgets::register_response(format!("version:{i}"), r, &resp);
                }
                if resp.hovered() {
                    ui.painter().rect_filled(r, 4.0, t.hover.gamma_multiply(0.7));
                }
                // thumbnail of the version's look
                let tr = Rect::from_min_size(r.min + vec2(4.0, 5.0), vec2(72.0, 48.0));
                ui.painter().rect_filled(tr, 2.0, t.canvas);
                if !measuring
                    && let Some(job) = app.session.variant_job(id, &v.settings, 160)
                    && let Some(tex) = app.renderer.variant(job)
                {
                    ui.painter().image(tex.id(), tr, crate::panels::presets::cover_uv(tr, tex.size), egui::Color32::WHITE);
                }
                let x = tr.right() + 10.0;
                ui.painter().with_clip_rect(r.with_max_x(r.right() - 24.0)).text(
                    pos2(x, r.top() + 18.0),
                    Align2::LEFT_CENTER,
                    &v.name,
                    t.semibold(13.0),
                    t.text,
                );
                let painter = ui.painter().with_clip_rect(r);
                painter.text(pos2(x, r.top() + 38.0), Align2::LEFT_CENTER, short_time(&v.created), t.font(11.0), t.text_dim);
                if current {
                    paint(ui.painter(), Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), vec2(14.0, 14.0)), Icon::Check, t.accent);
                }
                // resting on a version shows it in the loupe (nothing is changed)
                if interactive && resp.hovered() && !current {
                    app.hover_preview =
                        Some(crate::HoverPreview { label: crate::i18n::tr_format!("Version: {}", v.name), settings: (*v.settings).clone() });
                }
                if resp.double_clicked() {
                    crate::panels::dialogs::prompt(app, "Rename Version", "Version name", &v.name, "version.rename", json!({"index": i}), "name");
                } else if resp.clicked() && !current {
                    let _ = app.run("version.restore", json!({"index": i}));
                }
                // Native egui popups route Tab away from SDK stops. The SDK menu must own this layer.
                if !measuring && !(resp.secondary_clicked() && crate::widgets::carrying(ui.ctx())) {
                    let mut targets = Vec::new();
                    let mut layer = None;
                    let mut action = None;
                    craft::ui::menu::Menu::context(crate::i18n::tr("Version"), &resp)
                        .id_salt(("history-menu", "version", id.0, i))
                        .show(ui, |ui, menu| {
                            layer = Some(ui.layer_id());
                            if !interactive {
                                ui.disable();
                            }
                            if history_menu_action(ui, ("version", id.0, i, "restore"), "Restore", &mut targets) {
                                action = Some(VersionMenuAction::Restore);
                                menu.request_close();
                            }
                            if history_menu_action(ui, ("version", id.0, i, "update"), "Update with Current Settings", &mut targets) {
                                action = Some(VersionMenuAction::Update);
                                menu.request_close();
                            }
                            if history_menu_action(ui, ("version", id.0, i, "rename"), "Rename…", &mut targets) {
                                action = Some(VersionMenuAction::Rename);
                                menu.request_close();
                            }
                            if history_menu_action(ui, ("version", id.0, i, "before"), "Set as Before", &mut targets) {
                                action = Some(VersionMenuAction::Before);
                                menu.request_close();
                            }
                            Divider::horizontal().show(ui);
                            if history_menu_action(ui, ("version", id.0, i, "delete"), "Delete", &mut targets) {
                                action = Some(VersionMenuAction::Delete);
                                menu.request_close();
                            }
                        });
                    let eligible = publish_history_menu(ui.ctx(), layer, targets);
                    if interactive && eligible {
                        match action {
                            Some(VersionMenuAction::Restore) => { let _ = app.run("version.restore", json!({"index": i})); }
                            Some(VersionMenuAction::Update) => { let _ = app.run("version.update", json!({"index": i})); }
                            Some(VersionMenuAction::Rename) => { crate::panels::dialogs::prompt(app, "Rename Version", "Version name", &v.name, "version.rename", json!({"index": i}), "name"); }
                            Some(VersionMenuAction::Before) => { let _ = app.run("beforeAfter.setBefore", json!({"source": "version", "name": v.name})); }
                            Some(VersionMenuAction::Delete) => { let _ = app.run("version.delete", json!({"index": i})); }
                            None => {}
                        }
                    }
                }
            });
        }
    });
}

/// "Sep 30, 2026, 12:00 PM" from an ISO time.
pub(super) fn short_time(iso: &str) -> String {
    // Other languages use their own date patterns (`i18n::display_time`).
    if crate::i18n::language() != crate::i18n::Locale::En {
        return crate::i18n::display_time(iso);
    }
    let long = lightcraft_catalog::dates::display_time(iso);
    // "September 30, 2026 at 12:00:00 PM" → month abbreviated, seconds dropped
    let (date, time) = long.split_once(" at ").map_or((long.as_str(), None), |(d, t)| (d, Some(t)));
    let date = match date.split_once(' ') {
        Some((m, rest)) => format!("{} {rest}", m.chars().take(3).collect::<String>()),
        None => date.to_string(),
    };
    match time.and_then(|t| t.rsplit_once(' ')) {
        Some((hms, ampm)) => format!("{date}, {} {ampm}", hms.rsplit_once(':').map_or(hms, |x| x.0)),
        None => date,
    }
}

/// How a History row is drawn: the step the settings are at is highlighted, the steps after it
/// (still there to return to until a new edit) are greyed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryRow {
    Done,
    Current,
    Undone,
}

pub(super) fn history_row(i: usize, cursor: Option<usize>) -> HistoryRow {
    match cursor {
        Some(c) if i == c => HistoryRow::Current,
        Some(c) if i > c => HistoryRow::Undone,
        _ => HistoryRow::Done,
    }
}

pub(super) fn activity(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let Some(p) = app.session.catalog.photo(id).cloned() else { return };
    let measuring = ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx());
    // These commands resolve the active photo; a stale, disabled or speculative panel must not edit another target.
    let interactive = ui.is_enabled() && !measuring && app.session.active() == Some(id);
    header(ui, "History");
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        if !interactive {
            ui.disable();
        }
        if p.history.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr("No edits yet.")).color(t.text_dim));
        }
        let cursor = p.history_index();
        for (i, h) in p.history.iter().enumerate().rev() {
            ui.push_id(("history-row", id.0, i), |ui| {
                let (_, r) = ui.allocate_space(vec2(ui.available_width(), 24.0));
                let resp = host::target(ui, ("history-row", id.0, i), r, host::Target::new(crate::i18n::tr(&h.label)));
                if !measuring {
                    crate::widgets::register_response(format!("history:{i}"), r, &resp);
                }
                let row = history_row(i, cursor);
                if row == HistoryRow::Current {
                    ui.painter().rect_filled(r, 3.0, t.pressed);
                } else if resp.hovered() {
                    ui.painter().rect_filled(r, 3.0, t.hover);
                }
                let (icon, text) = if row == HistoryRow::Undone { (t.text_disabled, t.text_disabled) } else { (t.icon, t.text_label) };
                paint(ui.painter(), Rect::from_min_size(r.min + vec2(0.0, 4.0), vec2(16.0, 16.0)), Icon::Clock, icon);
                ui.painter().text(
                    pos2(r.left() + 24.0, r.center().y),
                    Align2::LEFT_CENTER,
                    crate::i18n::history_label(&h.label, &app.session.presets),
                    t.font(12.5),
                    text,
                );
                if resp.clicked() {
                    let _ = app.run("history.restore", json!({"index": i}));
                }
                // Native egui popups route Tab away from SDK stops. The SDK menu must own this layer.
                if !measuring && !(resp.secondary_clicked() && crate::widgets::carrying(ui.ctx())) {
                    let mut targets = Vec::new();
                    let mut layer = None;
                    let mut action = None;
                    craft::ui::menu::Menu::context(crate::i18n::tr("History"), &resp)
                        .id_salt(("history-menu", "history", id.0, i))
                        .show(ui, |ui, menu| {
                            layer = Some(ui.layer_id());
                            if !interactive {
                                ui.disable();
                            }
                            if history_menu_action(ui, ("history", id.0, i, "before"), "Copy History Step Settings to Before", &mut targets) {
                                action = Some(HistoryMenuAction::Before);
                                menu.request_close();
                            }
                            if history_menu_action(ui, ("history", id.0, i, "createVersion"), "Create Version from Step", &mut targets) {
                                action = Some(HistoryMenuAction::CreateVersion);
                                menu.request_close();
                            }
                            Divider::horizontal().show(ui);
                            if history_menu_action(ui, ("history", id.0, i, "clear"), "Clear History", &mut targets) {
                                action = Some(HistoryMenuAction::Clear);
                                menu.request_close();
                            }
                        });
                    let eligible = publish_history_menu(ui.ctx(), layer, targets);
                    if interactive && eligible {
                        match action {
                            Some(HistoryMenuAction::Before) => { let _ = app.run("beforeAfter.setBefore", json!({"source": "history", "index": i})); }
                            Some(HistoryMenuAction::CreateVersion) => { let _ = app.run("history.restore", json!({"index": i}));
                                let _ = app.run("version.create", json!({"name": h.label})); }
                            Some(HistoryMenuAction::Clear) => { let _ = app.run("history.clear", json!({})); }
                            None => {}
                        }
                    }
                }
            });
        }
    });
}

enum VersionMenuAction { Restore, Update, Rename, Before, Delete }
enum HistoryMenuAction { Before, CreateVersion, Clear }

/// Draw real SDK actions, but do not publish or run app effects from a fading exit body.
fn history_menu_action(
    ui: &mut egui::Ui,
    key: (&str, u64, usize, &str),
    label: &'static str,
    targets: &mut Vec<(String, egui::Response)>,
) -> bool {
    let shown = craft::ui::menu::MenuItem::new(crate::i18n::tr(label))
        .id_salt(("history-action", key))
        .enabled(ui.is_enabled())
        .show(ui);
    let measuring = ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx());
    if measuring {
        return false;
    }
    let ran = ui.is_enabled() && shown.ran;
    targets.push((format!("{}Menu:{}:{}", key.0, key.2, key.3), shown.response));
    ran
}

/// The Area stores its CURRENT interactability after its body returns. Querying it inside
/// the closure would use the previous pass and break same-target reopen during the exit.
fn publish_history_menu(
    ctx: &egui::Context,
    layer: Option<egui::LayerId>,
    targets: Vec<(String, egui::Response)>,
) -> bool {
    let eligible = layer.and_then(|layer| egui::AreaState::load(ctx, layer.id)).is_some_and(|state| state.interactable);
    if eligible {
        for (name, response) in targets {
            crate::widgets::register_response(name, response.rect, &response);
        }
    }
    eligible
}

// The inherited direct-control disabled/measurement checks have no popup layer.
#[cfg(test)]
fn history_action(ui: &mut egui::Ui, key: (&str, u64, usize, &str), label: &'static str) -> bool {
    let mut targets = Vec::new();
    let ran = history_menu_action(ui, key, label, &mut targets);
    for (name, response) in targets {
        crate::widgets::register_response(name, response.rect, &response);
    }
    ran
}

#[cfg(test)]
#[path = "history_sdk_action_tests.rs"]
mod history_sdk_action_tests;
