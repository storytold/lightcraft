//! The Presets column (opens to the left of the Edit panel): grouped presets with hover preview
//! (the loupe shows the photo with the preset while the pointer rests on it; nothing is
//! committed), optional live thumbnails, amount slider, create/favourite, import/export of preset
//! files.

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use lightcraft_develop::{ControlSpec, Preset, Section, Track};
use lightcraft_engine::presets::PresetList;
use serde_json::json;

use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::{divider, icon_button, register, slider};
use crate::{HoverPreview, LightcraftApp};

/// Long edge of preset thumbnails (px).
const THUMB_EDGE: usize = 128;
/// Height of the bar under the list while presets are selected (points).
const SELECTION_BAR_H: f32 = 34.0;
/// How many presets selected bring up the bar.
const MIN_FOR_BAR: usize = 2;

/// The preset amount slider may replace only the undo step it created.
#[derive(Clone)]
struct AppliedPreset {
    id: String,
    amount: f64,
    selected: Vec<lightcraft_catalog::PhotoId>,
    active: Option<lightcraft_catalog::PhotoId>,
    commits: u64,
}

impl AppliedPreset {
    fn new(session: &lightcraft_engine::Session, id: String, amount: f64) -> Self {
        Self { id, amount, selected: session.selection.ids.clone(), active: session.active(), commits: session.commits() }
    }

    fn is_current(&self, session: &lightcraft_engine::Session) -> bool {
        self.selected == session.selection.ids && self.active == session.active() && !self.is_stale(session)
    }

    /// A later edit, undo or redo (on any photo) owns the undo step: the slider may never replace it.
    fn is_stale(&self, session: &lightcraft_engine::Session) -> bool {
        !(self.commits == session.commits()
            && session.interaction.is_none()
            && session.redo.is_empty()
            && session
                .undo
                .last()
                .is_some_and(|entry| session.presets.iter().any(|preset| preset.id == self.id && entry.label == format!("Preset: {}", preset.name))))
    }
}

/// The preset each photo was given, with its settings right after: the panel marks it while the
/// photo still has exactly those settings, so it is still marked on coming back to the photo
/// (issue #2392) and drops once the photo is edited further.
#[derive(Clone, Default)]
struct AppliedByPhoto(std::collections::HashMap<lightcraft_catalog::PhotoId, (String, Arc<lightcraft_develop::DevelopSettings>)>);

impl AppliedByPhoto {
    const ID: &'static str = "preset-applied-by-photo";

    /// Remember `preset` on the photos `preset.apply` just changed (the selection, else the active
    /// photo; an active photo outside the selection wasn't changed and isn't marked).
    fn remember(ctx: &egui::Context, session: &lightcraft_engine::Session, preset: &str) {
        let id = egui::Id::unique(Self::ID);
        let mut map: Self = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
        for photo in session.selected() {
            if let Some(d) = session.develop_of(photo) {
                map.0.insert(photo, (preset.to_string(), d));
            }
        }
        ctx.data_mut(|d| d.insert_temp(id, map));
    }

    /// The preset to mark for `photo`: the one it was given, while its settings are unchanged since.
    fn of(ctx: &egui::Context, session: &lightcraft_engine::Session, photo: Option<lightcraft_catalog::PhotoId>) -> Option<String> {
        let photo = photo?;
        let map: Self = ctx.data(|d| d.get_temp(egui::Id::unique(Self::ID)))?;
        let (preset, then) = map.0.get(&photo)?;
        let now = session.develop_of(photo)?;
        (Arc::ptr_eq(then, &now) || **then == *now).then(|| preset.clone())
    }
}

/// The Presets panel: the Favorites section of starred presets on top (issue #1999), then the
/// groups of built-in and user presets, each row applying its preset on click.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
        ui.painter().text(pos2(hr.left() + 24.0, hr.center().y + 2.0), Align2::LEFT_CENTER, crate::i18n::tr("Presets"), t.semibold(15.0), t.text);
        let mut hdr = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(pos2(hr.right() - 80.0, hr.top()), hr.right_bottom()))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        if icon_button(
            &mut hdr,
            "presetCreate",
            Icon::Plus,
            vec2(28.0, 28.0),
            false,
            app.session.active().is_some(),
            crate::i18n::tr("Create Preset…"),
        )
        .clicked()
        {
            app.ui.dialog = Some(crate::state::Dialog::create_preset());
        }
        let more = icon_button(&mut hdr, "presetMore", Icon::More, vec2(28.0, 28.0), false, true, crate::i18n::tr("More preset options"));
        egui::Popup::menu(&more).show(|ui| {
            let r = ui.checkbox(&mut app.ui.preset_thumbs, crate::i18n::tr("Show Thumbnails"));
            register(ui, "presetMenu:thumbnails", r.rect);
            let r = ui.checkbox(&mut app.ui.preset_double_click, crate::i18n::tr("Double-Click to Apply"));
            register(ui, "presetMenu:doubleClick", r.rect);
            ui.separator();
            if ui.button(crate::i18n::tr("Import Presets…")).clicked() {
                let _ = app.run("file.importPresets", json!({}));
            }
            let any_user = app.session.presets.iter().any(|p| !p.builtin);
            if ui.add_enabled(any_user, egui::Button::new(crate::i18n::tr("Export User Presets…"))).clicked() {
                let _ = app.run("file.exportPresets", json!({}));
            }
            ui.separator();
            let r = ui.button(crate::i18n::tr("New Group…"));
            register(ui, "presetMenu:newGroup", r.rect);
            if r.clicked() {
                crate::panels::dialogs::prompt(app, "New Preset Group", "Group name", "", "preset.createGroup", json!({}), "name");
            }
            let r = ui.button(crate::i18n::tr("Select All"));
            register(ui, "presetMenu:selectAll", r.rect);
            if r.clicked() {
                app.ui.preset_selection = app.session.presets.iter().map(|p| p.id.clone()).collect();
            }
        });
        divider(ui);
        // amount slider (applies to the last applied preset)
        let amt_id = egui::Id::unique("preset-amount");
        let last: Option<AppliedPreset> = ui.data(|d| d.get_temp(amt_id));
        if let Some(last) = last.clone().filter(|last| last.is_current(&app.session)) {
            let spec = ControlSpec {
                id: "presetAmount",
                label: "Amount",
                section: Section::Profile,
                min: 0.0,
                max: 200.0,
                default: 100.0,
                step: 1.0,
                decimals: 0,
                track: Track::Plain,
            };
            let out = slider(ui, &spec, last.amount, true, None);
            if let Some(v) = out.value
                && app.run("edit.undo", json!({})).is_ok()
            {
                // Only replace the most recent preset; never undo intervening edits.
                if app.run("preset.apply", json!({"id": &last.id, "amount": v})).is_ok() {
                    AppliedByPhoto::remember(ui.ctx(), &app.session, &last.id);
                    ui.data_mut(|d| d.insert_temp(amt_id, AppliedPreset::new(&app.session, last.id, v)));
                } else {
                    let _ = app.run("edit.redo", json!({}));
                    ui.data_mut(|d| d.remove::<AppliedPreset>(amt_id));
                }
            }
            divider(ui);
        } else if last.is_some_and(|last| last.is_stale(&app.session)) {
            // kept across a switch to another photo and back (the slider returns with it); gone once
            // a later edit, undo or redo owns the step
            ui.data_mut(|d| d.remove::<AppliedPreset>(amt_id));
        }
        let active = app.session.active();
        let marked = AppliedByPhoto::of(ui.ctx(), &app.session, active);
        let thumbs = app.ui.preset_thumbs && active.is_some();
        let row_h = if thumbs { 54.0 } else { 26.0 };
        let groups = grouped(ui.ctx(), &app.session.presets);
        let (selected_total, selected_user) = selection_counts(&app.session.presets, &app.ui.preset_selection);
        // the bar goes under the list, in room kept here: above it, its coming and going would move
        // the rows between the two clicks of a double-click, onto another preset
        let bar_room = if selected_total >= MIN_FOR_BAR { SELECTION_BAR_H + 1.0 } else { 0.0 };
        let list_height = (ui.available_height() - bar_room).max(0.0);
        let scroll = egui::ScrollArea::vertical().id_salt("presets-scroll").auto_shrink([false, false]).max_height(list_height).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            // Only the rows in view are laid out, drawn and given thumbnails: a Lightroom collection
            // imported whole is 10 000+ presets, and building every row each frame took seconds.
            let visible = ui.clip_rect().expand(row_h);
            for grp in groups.iter() {
                let (g, builtin, favorites) = (grp.name.clone(), grp.builtin, grp.favorites);
                let open_id = group_open_id(favorites, &g);
                let open = group_open(ui.ctx(), grp);
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                crate::widgets::announce(&resp, g.to_string());
                // outside the `presetGroup:` ids, which a user group of any name can take
                register(ui, if favorites { "presetFavorites".to_string() } else { format!("presetGroup:{g}") }, r);
                paint(
                    ui.painter(),
                    Rect::from_center_size(pos2(r.left() + 22.0, r.center().y), vec2(16.0, 16.0)),
                    if open { Icon::ChevronDown } else { Icon::ChevronRight },
                    t.text_label,
                );
                ui.painter().text(
                    pos2(r.left() + 36.0, r.center().y),
                    Align2::LEFT_CENTER,
                    crate::i18n::builtin_label(&g, builtin),
                    t.semibold(13.0),
                    t.text_label,
                );
                if grp.pinned {
                    crate::rating::paint_star(
                        ui.painter(),
                        Rect::from_center_size(pos2(r.right() - 20.0, r.center().y), vec2(16.0, 16.0)),
                        true,
                        t.star,
                    );
                }
                if resp.clicked() {
                    ui.data_mut(|d| d.insert_temp(open_id, !open));
                }
                if !favorites {
                    crate::widgets::context_menu(&resp, |ui| group_menu(app, ui, grp));
                }
                if !open {
                    continue;
                }
                if grp.items.is_empty() {
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
                    ui.painter().text(
                        pos2(r.left() + 40.0, r.center().y),
                        Align2::LEFT_CENTER,
                        crate::i18n::tr("No presets in this group"),
                        t.font(13.0),
                        t.text_dim,
                    );
                    continue;
                }
                let (first, end) = rows_in_view(ui.cursor().top(), row_h, grp.items.len(), visible);
                if first > 0 {
                    ui.allocate_space(vec2(ui.available_width(), first as f32 * row_h));
                }
                for &i in grp.items.get(first..end).unwrap_or_default() {
                    let Some(pr) = app.session.presets.get(i).cloned() else { continue };
                    let (pid, name, fav) = (pr.id.clone(), crate::i18n::builtin_label(&pr.name, pr.builtin).to_string(), pr.favorite);
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    crate::widgets::announce(&resp, name.clone());
                    // a starred preset is listed twice: in Favorites and in its group
                    register(ui, if favorites { format!("presetFavorite:{pid}") } else { format!("preset:{pid}") }, r);
                    let is_last = marked.as_deref() == Some(pid.as_str());
                    if is_last {
                        register(ui, "presetApplied", r);
                        ui.painter().rect_filled(r, 0.0, t.tool_active);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.7));
                    }
                    let selected = app.ui.preset_selection.contains(&pid);
                    if selected {
                        register(ui, format!("presetSelected:{pid}"), r);
                        if !is_last && !resp.hovered() {
                            ui.painter().rect_filled(r, 0.0, t.hover);
                        }
                        ui.painter().rect_filled(Rect::from_min_size(r.left_top(), vec2(3.0, r.height())), 0.0, t.accent);
                    }
                    // the preview is what a click commits: the same composition as `preset.apply` (`Session::preset_applied`)
                    let look = if thumbs || resp.hovered() { active.and_then(|id| app.session.preset_applied(id, &pr, 1.0)) } else { None };
                    let mut text_x = r.left() + 40.0;
                    if thumbs && let (Some(id), Some(s)) = (active, look.as_ref()) {
                        let tr = Rect::from_min_size(pos2(r.left() + 36.0, r.top() + 5.0), vec2(66.0, row_h - 10.0));
                        ui.painter().rect_filled(tr, 2.0, t.canvas);
                        if let Some(job) = app.session.variant_job(id, s, THUMB_EDGE)
                            && let Some(tex) = app.renderer.variant(job)
                        {
                            ui.painter().image(tex.id(), tr, cover_uv(tr, tex.size), Color32::WHITE);
                        }
                        text_x = tr.right() + 10.0;
                    }
                    ui.painter().text(pos2(text_x, r.center().y), Align2::LEFT_CENTER, &name, t.font(13.0), t.text_label);
                    if fav {
                        crate::rating::paint_star(
                            ui.painter(),
                            Rect::from_center_size(pos2(r.right() - 20.0, r.center().y), vec2(16.0, 16.0)),
                            true,
                            t.star,
                        );
                    }
                    // resting on a preset previews it in the loupe (no history entry)
                    if resp.hovered()
                        && let Some(s) = look
                    {
                        let label = crate::i18n::tr_format!("Preset: {name}", name = name);
                        app.hover_preview = Some(HoverPreview { label: label.clone(), settings: s });
                        if let Some(id) = active {
                            ui.data_mut(|d| d.insert_temp(egui::Id::unique("last-preset-hover"), (id, pr.clone(), label)));
                        }
                    }
                    let modifiers = ui.input(|i| i.modifiers);
                    let selects = modifiers.command || modifiers.shift;
                    let double_click = app.ui.preset_double_click;
                    // while a selection is on show, a plain click ends it and applies nothing: a
                    // slip after selecting a hundred presets shouldn't change the photo
                    let ends_selection = !app.ui.preset_selection.is_empty();
                    if resp.clicked() && selects {
                        click_select(app, ui.ctx(), &groups, favorites, &pid, modifiers.shift);
                    } else if resp.clicked() {
                        app.ui.preset_selection.clear();
                        if double_click {
                            app.ui.preset_selection.insert(pid.clone());
                        }
                        app.ui.preset_anchor = Some(pid.clone());
                    }
                    let applies = !selects && if double_click { resp.double_clicked() } else { resp.clicked() && !ends_selection };
                    if applies && app.run("preset.apply", json!({"id": pid, "amount": 100})).is_ok() {
                        AppliedByPhoto::remember(ui.ctx(), &app.session, &pid);
                        ui.data_mut(|d| d.insert_temp(amt_id, AppliedPreset::new(&app.session, pid.clone(), 100.0)));
                        ui.data_mut(|d| d.remove::<(lightcraft_catalog::PhotoId, Preset, String)>(egui::Id::unique("last-preset-hover")));
                        app.toast(ui.ctx(), crate::i18n::tr_format!("Preset: {name}", name = name));
                    }
                    let (builtin, group) = (pr.builtin, pr.group.clone());
                    crate::widgets::context_menu(&resp, |ui| {
                        // a right-click inside a selection acts on all of it
                        if selected && selected_total > 1 {
                            bulk_menu(app, ui, &groups, selected_total, selected_user);
                            return;
                        }
                        if ui.button(crate::i18n::tr(if fav { "Remove from Favorites" } else { "Add to Favorites" })).clicked() {
                            let _ = app.run("preset.favorite", json!({"id": pid}));
                        }
                        crate::widgets::menu_button(ui, crate::i18n::tr("Copy to Group"), |ui| {
                            group_targets(app, ui, &groups, "preset.copy", std::slice::from_ref(&pid), "Copy Preset to New Group");
                        });
                        if builtin {
                            return;
                        }
                        ui.separator();
                        if ui
                            .add_enabled(app.session.active().is_some(), egui::Button::new(crate::i18n::tr("Update with Current Settings")))
                            .clicked()
                        {
                            match app.run("preset.update", json!({"id": pid})) {
                                Ok(_) => app.toast(ui.ctx(), crate::i18n::tr_format!("Updated “{name}”", name = name)),
                                Err(e) => app.toast(ui.ctx(), e),
                            }
                        }
                        if ui.button(crate::i18n::tr("Rename…")).clicked() {
                            crate::panels::dialogs::prompt(app, "Rename Preset", "Preset name", &name, "preset.rename", json!({"id": pid}), "name");
                        }
                        crate::widgets::menu_button(ui, crate::i18n::tr("Move to Group"), |ui| {
                            for g in groups.iter().filter(|x| x.user && x.name != group).map(|x| &x.name) {
                                if ui.button(g).clicked() {
                                    let _ = app.run("preset.move", json!({"id": pid, "group": g}));
                                }
                            }
                            if ui.button(crate::i18n::tr("New Group…")).clicked() {
                                crate::panels::dialogs::prompt(
                                    app,
                                    "Move Preset to New Group",
                                    "Group name",
                                    "",
                                    "preset.move",
                                    json!({"id": pid}),
                                    "group",
                                );
                            }
                        });
                        ui.separator();
                        if ui.button(crate::i18n::tr("Delete Preset")).clicked() {
                            let _ = app.run("preset.delete", json!({"id": pid}));
                        }
                    });
                }
                if end < grp.items.len() {
                    ui.allocate_space(vec2(ui.available_width(), (grp.items.len() - end) as f32 * row_h));
                }
            }
            // as much more room as the bar takes when it isn't there: at the very end of the list the
            // bar going away would otherwise bring the rows down, under the pointer
            ui.add_space(30.0 + SELECTION_BAR_H + 1.0 - bar_room);
        });
        selection_bar(app, ui);
        let last_hover_id = egui::Id::unique("last-preset-hover");
        let pointer_in_scroll = ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|pos| scroll.inner_rect.contains(pos));
        if pointer_in_scroll {
            if app.hover_preview.is_none()
                && let Some(id) = active
                && let Some((saved_id, saved_preset, saved_label)) =
                    ui.data(|d| d.get_temp::<(lightcraft_catalog::PhotoId, Preset, String)>(last_hover_id))
                && saved_id == id
                && let Some(s) = app.session.preset_applied(id, &saved_preset, 1.0)
            {
                app.hover_preview = Some(HoverPreview { label: saved_label, settings: s });
            }
        } else {
            ui.data_mut(|d| d.remove::<(lightcraft_catalog::PhotoId, Preset, String)>(last_hover_id));
        }
    });
}

/// The key of a group's open state. The Favorites section opens and closes apart from a user group
/// named "Favorites".
fn group_open_id(favorites: bool, name: &str) -> egui::Id {
    egui::Id::unique(("preset-group", favorites, name))
}

fn group_open(ctx: &egui::Context, group: &Group) -> bool {
    ctx.data(|d| d.get_temp::<bool>(group_open_id(group.favorites, &group.name))).unwrap_or(true)
}

/// How many presets are selected, and how many of them are user presets (a selection may name
/// presets that are gone). O(presets) only while something is selected.
fn selection_counts(presets: &PresetList, selection: &std::collections::BTreeSet<String>) -> (usize, usize) {
    if selection.is_empty() {
        return (0, 0);
    }
    let chosen = presets.iter().filter(|p| selection.contains(&p.id));
    chosen.fold((0, 0), |(total, user), p| (total + 1, user + usize::from(!p.builtin)))
}

/// A row clicked with ⌘/Ctrl or Shift: toggle it, or with Shift extend from the last row clicked
/// (with or without a modifier) across the open groups. A preset listed twice (Favorites and its group) extends within the
/// section clicked, so a range in the groups doesn't start up in Favorites.
fn click_select(app: &mut LightcraftApp, ctx: &egui::Context, groups: &[Group], favorites: bool, pid: &str, extend: bool) {
    if extend && let Some(anchor) = app.ui.preset_anchor.clone() {
        let rows = open_rows(ctx, &app.session.presets, groups, favorites);
        if let Some(range) = ids_between(&rows, &anchor, pid) {
            app.ui.preset_selection.extend(range.iter().cloned());
            return;
        }
    }
    if !app.ui.preset_selection.remove(pid) {
        app.ui.preset_selection.insert(pid.to_string());
    }
    app.ui.preset_anchor = Some(pid.to_string());
}

/// The ids of the rows on show, in panel order: the Favorites section, or the other groups.
fn open_rows(ctx: &egui::Context, presets: &PresetList, groups: &[Group], favorites: bool) -> Vec<String> {
    let open = groups.iter().filter(|g| g.favorites == favorites && group_open(ctx, g));
    open.flat_map(|g| g.items.iter()).filter_map(|&i| presets.get(i).map(|p| p.id.clone())).collect()
}

/// The rows from `a` to `b`, whichever comes first; `None` when either isn't on show.
fn ids_between<'a>(rows: &'a [String], a: &str, b: &str) -> Option<&'a [String]> {
    let i = rows.iter().position(|x| x == a)?;
    let j = rows.iter().position(|x| x == b)?;
    rows.get(i.min(j)..=i.max(j))
}

/// The bar shown while presets are selected: how many, what can be done with them, and a way out.
/// A plain click on a preset also ends the selection. One preset selected has its own menu.
fn selection_bar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let (total, user) = selection_counts(&app.session.presets, &app.ui.preset_selection);
    // presets gone (a group deleted, an agent): they shouldn't count, nor come back selected if imported again
    if total < app.ui.preset_selection.len() {
        let exists: std::collections::HashSet<&str> = app.session.presets.iter().map(|p| p.id.as_str()).collect();
        app.ui.preset_selection.retain(|id| exists.contains(id.as_str()));
    }
    if total < MIN_FOR_BAR {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let groups = grouped(ui.ctx(), &app.session.presets);
    divider(ui);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), SELECTION_BAR_H), Sense::hover());
    let text = crate::i18n::tr_format!("{n} selected", n = total);
    ui.painter().text(pos2(r.left() + 24.0, r.center().y), Align2::LEFT_CENTER, text, t.font(13.0), t.text_label);
    let mut row = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(8.0, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
    if icon_button(&mut row, "presetSelectionClear", Icon::Close, vec2(24.0, 24.0), false, true, crate::i18n::tr("Deselect All")).clicked() {
        app.ui.preset_selection.clear();
        app.ui.preset_anchor = None;
    }
    let menu = crate::widgets::menu_button(&mut row, crate::i18n::tr("Actions"), |ui| bulk_menu(app, ui, &groups, total, user));
    register(&row, "presetSelectionActions", menu.response.rect);
}

/// What can be done with the selection: built-in presets can be copied, not moved or deleted.
fn bulk_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, groups: &[Group], total: usize, user: usize) {
    let selection = &app.ui.preset_selection;
    let ids: Vec<String> = app.session.presets.iter().filter(|p| selection.contains(&p.id)).map(|p| p.id.clone()).collect();
    let r = crate::widgets::menu_button(ui, crate::i18n::tr("Copy to Group"), |ui| {
        group_targets(app, ui, groups, "preset.copy", &ids, "Copy Presets to New Group");
    });
    register(ui, "presetBulk:copy", r.response.rect);
    let r = ui.add_enabled_ui(user > 0, |ui| {
        crate::widgets::menu_button(ui, crate::i18n::tr("Move to Group"), |ui| {
            group_targets(app, ui, groups, "preset.moveMany", &ids, "Move Presets to New Group");
        })
    });
    register(ui, "presetBulk:move", r.inner.response.rect);
    ui.separator();
    let r = ui.add_enabled(user > 0, egui::Button::new(crate::i18n::tr("Delete…")));
    register(ui, "presetBulk:delete", r.rect);
    if r.clicked() {
        app.ui.dialog =
            Some(crate::state::Dialog::DeletePresets { ids, builtin: total.saturating_sub(user), library: app.session.library_generation() });
    }
}

/// The user groups as the targets of `command` (`{ids, group}`), and a new one.
fn group_targets(app: &mut LightcraftApp, ui: &mut egui::Ui, groups: &[Group], command: &str, ids: &[String], new_title: &str) {
    for g in groups.iter().filter(|x| x.user) {
        let r = ui.button(&g.name);
        register(ui, format!("presetTarget:{command}:{}", g.name), r.rect);
        if r.clicked() {
            let message = match app.run(command, json!({"ids": ids, "group": g.name})) {
                Ok(done) => outcome(command, &g.name, &done),
                Err(e) => e,
            };
            app.toast(ui.ctx(), message);
        }
    }
    if ui.button(crate::i18n::tr("New Group…")).clicked() {
        crate::panels::dialogs::prompt(app, new_title, "Group name", "", command, json!({"ids": ids}), "group");
    }
}

/// What a bulk copy or move into `group` did, for the toast.
fn outcome(command: &str, group: &str, done: &serde_json::Value) -> String {
    if command == "preset.copy" {
        let n = done["ids"].as_array().map_or(0, Vec::len);
        let skipped = done["skipped"].as_u64().unwrap_or(0);
        if skipped > 0 {
            return crate::i18n::tr_format!("Copied {n} to “{group}”, {skipped} already there", n = n, group = group, skipped = skipped);
        }
        return crate::i18n::tr_format!("Copied {n} to “{group}”", n = n, group = group);
    }
    let n = done["moved"].as_u64().unwrap_or(0);
    crate::i18n::tr_format!("Moved {n} to “{group}”", n = n, group = group)
}

/// A group header's menu.
fn group_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, group: &Group) {
    let empty = group.items.is_empty();
    let r = ui.add_enabled(!empty, egui::Button::new(crate::i18n::tr("Select All in Group")));
    register(ui, "presetGroupMenu:selectAll", r.rect);
    if r.clicked() {
        let ids: Vec<String> = group.items.iter().filter_map(|&i| app.session.presets.get(i).map(|p| p.id.clone())).collect();
        app.ui.preset_selection.extend(ids);
    }
    if ui.add_enabled(!empty, egui::Button::new(crate::i18n::tr("Export Group…"))).clicked() {
        let _ = app.run("file.exportPresets", json!({"group": group.name}));
    }
    let r = ui.button(crate::i18n::tr(if group.pinned { "Remove from Favorites" } else { "Add to Favorites" }));
    register(ui, "presetGroupMenu:favorite", r.rect);
    if r.clicked() {
        let _ = app.run("preset.favoriteGroup", json!({"name": group.name, "favorite": !group.pinned}));
    }
    if group.builtin {
        return;
    }
    ui.separator();
    let r = ui.button(crate::i18n::tr("Rename…"));
    register(ui, "presetGroupMenu:rename", r.rect);
    if r.clicked() {
        crate::panels::dialogs::prompt(
            app,
            "Rename Preset Group",
            "Group name",
            &group.name,
            "preset.renameGroup",
            json!({"name": group.name}),
            "to",
        );
    }
    let r = ui.button(crate::i18n::tr("Delete Group…"));
    register(ui, "presetGroupMenu:delete", r.rect);
    if r.clicked() {
        if empty {
            if let Err(e) = app.run("preset.deleteGroup", json!({"name": group.name})) {
                app.toast(ui.ctx(), e);
            }
        } else {
            app.ui.dialog = Some(crate::state::Dialog::DeletePresetGroup {
                name: group.name.clone(),
                count: group.items.len(),
                library: app.session.library_generation(),
            });
        }
    }
}

/// A preset group as the panel lists it: indices into the session's presets.
struct Group {
    name: String,
    /// Holds a stock preset: a stock group (shown in the UI language), even when user presets
    /// share its name; a group of user presets keeps its name.
    builtin: bool,
    /// Holds a user preset or was made by name (a Move and Copy to Group target).
    user: bool,
    /// The Favorites section on top (issue #1999): the starred presets, each also in its own group.
    favorites: bool,
    /// Starred as a group: listed right after that section, before the other groups.
    pinned: bool,
    items: Vec<usize>,
}

/// The presets by group (groups sorted by name, presets in list order), rebuilt only when the
/// list changed (its generation).
fn grouped(ctx: &egui::Context, presets: &PresetList) -> Arc<Vec<Group>> {
    let id = egui::Id::unique("presets-grouped");
    if let Some((generation, groups)) = ctx.data(|d| d.get_temp::<(u64, Arc<Vec<Group>>)>(id))
        && generation == presets.generation()
    {
        return groups;
    }
    let mut by_name: BTreeMap<&str, Group> = BTreeMap::new();
    for (i, p) in presets.iter().enumerate() {
        let g = by_name.entry(&p.group).or_insert_with(|| Group {
            name: p.group.clone(),
            builtin: false,
            user: false,
            favorites: false,
            pinned: presets.favorite_groups().contains(&p.group),
            items: Vec::new(),
        });
        g.builtin |= p.builtin;
        g.user |= !p.builtin;
        g.items.push(i);
    }
    // the groups made by name are listed even with no preset naming them
    for name in presets.created_groups() {
        let g = by_name.entry(name).or_insert_with(|| Group {
            name: name.clone(),
            builtin: false,
            user: true,
            favorites: false,
            pinned: presets.favorite_groups().contains(name),
            items: Vec::new(),
        });
        g.user = true;
    }
    // starred presets first, in their own section, as Profiles does
    let starred: Vec<usize> = presets.iter().enumerate().filter(|(_, p)| p.favorite).map(|(i, _)| i).collect();
    let favorites =
        (!starred.is_empty()).then(|| Group { name: "Favorites".into(), builtin: true, user: false, favorites: true, pinned: false, items: starred });
    let (pinned, others): (Vec<Group>, Vec<Group>) = by_name.into_values().partition(|g| g.pinned);
    let groups = Arc::new(favorites.into_iter().chain(pinned).chain(others).collect::<Vec<_>>());
    ctx.data_mut(|d| d.insert_temp(id, (presets.generation(), groups.clone())));
    groups
}

/// The presets as the rows of a picker (a combo box's list), in list order with a header row
/// wherever the group changes. Only the rows in view are built (10 000+ presets made an open
/// picker take seconds a frame). Returns the id of the preset clicked.
pub fn picker_rows(ui: &mut egui::Ui, salt: &str, presets: &PresetList, selected: Option<&str>) -> Option<String> {
    let id = egui::Id::unique("preset-picker-rows");
    let rows = match ui.ctx().data(|d| d.get_temp::<(u64, Arc<Vec<PickRow>>)>(id)) {
        Some((generation, rows)) if generation == presets.generation() => rows,
        _ => {
            let mut rows = Vec::with_capacity(presets.len() + 16);
            let mut group: Option<&str> = None;
            for (i, p) in presets.iter().enumerate() {
                if group != Some(p.group.as_str()) {
                    group = Some(&p.group);
                    rows.push(PickRow::Header(crate::i18n::builtin_label(&p.group, p.builtin).to_string()));
                }
                rows.push(PickRow::Preset(i));
            }
            let rows = Arc::new(rows);
            ui.ctx().data_mut(|d| d.insert_temp(id, (presets.generation(), rows.clone())));
            rows
        }
    };
    let row_h = ui.spacing().interact_size.y;
    let mut out = None;
    egui::ScrollArea::vertical().id_salt(("preset-picker", salt)).max_height(300.0).auto_shrink([false, true]).show_rows(
        ui,
        row_h,
        rows.len(),
        |ui, range| {
            for row in rows.get(range).unwrap_or_default() {
                let size = vec2(ui.available_width(), row_h);
                match row {
                    PickRow::Header(g) => {
                        ui.add_sized(size, egui::Label::new(egui::RichText::new(g).size(10.5).weak()));
                    }
                    PickRow::Preset(i) => {
                        let Some(p) = presets.get(*i) else { continue };
                        let label = crate::i18n::builtin_label(&p.name, p.builtin);
                        if ui.add_sized(size, egui::Button::selectable(selected == Some(p.id.as_str()), label)).clicked() {
                            out = Some(p.id.clone());
                        }
                    }
                }
            }
        },
    );
    out
}

#[derive(Clone)]
enum PickRow {
    /// A group's name (in the UI language for stock groups).
    Header(String),
    /// An index into the session's presets.
    Preset(usize),
}

/// The rows `first..end` of a block of `n` rows `row_h` high starting at `top` that meet
/// `visible`.
fn rows_in_view(top: f32, row_h: f32, n: usize, visible: Rect) -> (usize, usize) {
    if row_h <= 0.0 || !top.is_finite() {
        return (0, n);
    }
    // `as usize` saturates (negative and NaN give 0)
    let first = (((visible.top() - top) / row_h).floor() as usize).min(n);
    let end = (((visible.bottom() - top) / row_h).ceil() as usize).clamp(first, n);
    (first, end)
}

/// The part of a `size` image that covers `r` (centre crop), as texture coordinates.
pub fn cover_uv(r: Rect, size: [usize; 2]) -> Rect {
    let a = size[0] as f32 / size[1].max(1) as f32;
    let ra = r.width() / r.height().max(1.0);
    if a > ra {
        let f = ra / a;
        Rect::from_min_max(pos2((1.0 - f) / 2.0, 0.0), pos2((1.0 + f) / 2.0, 1.0))
    } else {
        let f = a / ra;
        Rect::from_min_max(pos2(0.0, (1.0 - f) / 2.0), pos2(1.0, (1.0 + f) / 2.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_slider_does_not_undo_a_later_edit() {
        let mut session = lightcraft_engine::Session::with_demo();
        let pid = "lc.golden-hour";
        session.execute("preset.apply", &json!({"id": pid})).unwrap();
        let state = AppliedPreset::new(&session, pid.into(), 100.0);
        assert!(state.is_current(&session));

        session.execute("develop.set", &json!({"values": {"light.exposure": 0.75}})).unwrap();
        assert!(!state.is_current(&session), "a newer edit owns the undo step");
    }

    #[test]
    fn amount_slider_does_not_undo_on_another_photo() {
        let mut session = lightcraft_engine::Session::with_demo();
        let pid = "lc.golden-hour";
        session.execute("preset.apply", &json!({"id": pid})).unwrap();
        let state = AppliedPreset::new(&session, pid.into(), 100.0);
        let active = session.active();
        let other = session.visible().iter().copied().find(|id| Some(*id) != active).unwrap();
        session.execute("library.select", &json!({"ids": [other.0]})).unwrap();
        assert!(!state.is_current(&session), "the preset belongs to the earlier selection");
    }
}
