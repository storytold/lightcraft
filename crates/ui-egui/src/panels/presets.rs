//! The Presets column (opens to the left of the Edit panel): grouped presets with hover preview
//! (the loupe shows the photo with the preset while the pointer rests on it; nothing is
//! committed), optional live thumbnails, amount slider, create/favourite, import/export of preset
//! files.

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use lcc_develop::{ControlSpec, Preset, Section, Track};
use lcc_engine::presets::PresetList;
use serde_json::json;

use crate::icons::{Icon, paint};
use crate::module::PanelId;
use crate::theme::Tokens;
use crate::widgets::{divider, icon_button, register, slider};
use crate::{HoverPreview, LccApp};

/// Long edge of preset thumbnails (px).
const THUMB_EDGE: usize = 128;

/// Height of the panel's Classic header ([`crate::panels::classic::header`]): folded, it is all
/// the panel takes in the left column.
pub const HEADER_H: f32 = 28.0;

/// The state the Presets panel of Develop's left column folds under (its own key, kept from when
/// the panel was also drawn elsewhere, so saved fold states still apply).
pub const IN_COLUMN_KEY: &str = "panel:presets-inline";

/// The preset amount slider may replace only the undo step it created.
#[derive(Clone)]
struct AppliedPreset {
    id: String,
    amount: f64,
    selected: Vec<lcc_catalog::PhotoId>,
    active: Option<lcc_catalog::PhotoId>,
    commits: u64,
}

impl AppliedPreset {
    fn new(session: &lcc_engine::Session, id: String, amount: f64) -> Self {
        Self { id, amount, selected: session.selection.ids.clone(), active: session.active(), commits: session.commits() }
    }

    fn is_current(&self, session: &lcc_engine::Session) -> bool {
        self.selected == session.selection.ids && self.active == session.active() && !self.is_stale(session)
    }

    /// A later edit, undo or redo (on any photo) owns the undo step: the slider may never replace it.
    fn is_stale(&self, session: &lcc_engine::Session) -> bool {
        !(self.commits == session.commits()
            && session.interaction.is_none()
            && session.redo.is_empty()
            && session
                .undo
                .last()
                .and_then(|entry| entry.label.strip_prefix("Preset: "))
                .is_some_and(|name| session.presets.iter().any(|preset| preset.id == self.id && preset.name == name)))
    }
}

/// The preset each photo was given, with its settings right after: the panel marks it while the
/// photo still has exactly those settings, so it is still marked on coming back to the photo
/// (issue #2392) and drops once the photo is edited further.
#[derive(Clone, Default)]
struct AppliedByPhoto(std::collections::HashMap<lcc_catalog::PhotoId, (String, Arc<lcc_develop::DevelopSettings>)>);

impl AppliedByPhoto {
    const ID: &'static str = "preset-applied-by-photo";

    /// Remember `preset` on the photos `preset.apply` just changed (the selection, else the active
    /// photo; an active photo outside the selection wasn't changed and isn't marked).
    fn remember(ctx: &egui::Context, session: &lcc_engine::Session, preset: &str) {
        let id = egui::Id::new(Self::ID);
        let mut map: Self = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
        for photo in session.selected() {
            if let Some(d) = session.develop_of(photo) {
                map.0.insert(photo, (preset.to_string(), d));
            }
        }
        ctx.data_mut(|d| d.insert_temp(id, map));
    }

    /// The preset to mark for `photo`: the one it was given, while its settings are unchanged since.
    fn of(ctx: &egui::Context, session: &lcc_engine::Session, photo: Option<lcc_catalog::PhotoId>) -> Option<String> {
        let photo = photo?;
        let map: Self = ctx.data(|d| d.get_temp(egui::Id::new(Self::ID)))?;
        let (preset, then) = map.0.get(&photo)?;
        let now = session.develop_of(photo)?;
        (Arc::ptr_eq(then, &now) || **then == *now).then(|| preset.clone())
    }
}

/// Whether the presets are on screen: Develop's left column is shown with its Presets panel
/// unfolded (the Modern layout: its Presets card is open). What Window ▸ Presets ticks.
pub fn shown(app: &LccApp) -> bool {
    if crate::modern::on(app) {
        return app.ui.modern.group == Some(crate::modern_dock::Group::Presets);
    }
    app.ui.module == crate::module::ModuleId::Develop
        && crate::module::edge_visible(app, crate::module::Edge::Left)
        && !app.ui.hidden_panels.contains(&PanelId::Presets)
        && super::classic::is_open_with_key(app, IN_COLUMN_KEY)
}

/// Bring the presets on screen where they live: Develop, its left column shown, the Presets panel
/// in it and unfolded. They are never a column of their own on the right — upstream's layout put
/// them in the Edit panels' place, which left Develop without its sliders. Fails when Develop has
/// no photo to open.
pub fn reveal(app: &mut LccApp) -> Result<(), String> {
    if crate::modern::on(app) {
        if !shown(app) {
            crate::modern_dock::open(app, crate::modern_dock::Group::Presets)?;
        }
        return Ok(());
    }
    crate::module::switch(app, crate::module::ModuleId::Develop)?;
    app.ui.left_panel = true;
    app.ui.hidden_panels.retain(|p| *p != PanelId::Presets);
    set_shown(app, true);
    Ok(())
}

/// Fold or unfold the Presets panel of Develop's left column (the Modern layout: close its card).
pub fn set_shown(app: &mut LccApp, open: bool) {
    if crate::modern::on(app) {
        if !open && shown(app) {
            crate::modern_dock::close(app);
        }
        return;
    }
    super::classic::set_open_with_key(app, PanelId::Presets, IN_COLUMN_KEY, open);
}

/// `in_column`: drawn as the Presets panel of Develop's left column (which gives it a share of the
/// column and lets it fold); otherwise whole, with a plain header (docking).
pub fn show(app: &mut LccApp, ui: &mut egui::Ui, side: &[PanelId], in_column: bool) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider)).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // The panel's own Classic header in the left column (issue #75): the chevron folds the
        // list to give the panels below the room, folding under `IN_COLUMN_KEY`. The header keeps
        // room for the Create and More buttons at the right (`header_inset`). Outside the left
        // column (docking), the panel is drawn whole and does not fold, so it carries no collapse
        // triangle (issue #238).
        let (hr, unfolded) = if in_column {
            super::classic::header_inset(app, ui, PanelId::Presets, side, IN_COLUMN_KEY, 62.0)
        } else {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), Sense::hover());
            register(ui.ctx(), "panelHeader:presets", r);
            let painter = ui.painter();
            painter.line_segment([pos2(r.left(), r.top()), pos2(r.right(), r.top())], egui::Stroke::new(1.0, t.divider));
            let name = crate::i18n::tr(super::classic::title(PanelId::Presets));
            painter.text(pos2(r.left() + 14.0, r.center().y), Align2::LEFT_CENTER, name, t.semibold(12.5), t.text);
            (r, true)
        };
        let mut hdr = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(pos2(hr.right() - 80.0, hr.top()), hr.right_bottom()))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let plus = icon_button(&mut hdr, "presetCreate", Icon::Plus, vec2(26.0, 22.0), false, true, crate::i18n::tr("Create Preset or Folder"));
        register(ui.ctx(), "icon:presetCreate", plus.rect);
        egui::Popup::menu(&plus).show(|ui| {
            if ui.add_enabled(app.session.active().is_some(), egui::Button::new(crate::i18n::tr("Create Preset…"))).clicked() {
                app.ui.dialog = Some(crate::state::Dialog::create_preset(&app.session));
            }
            if ui.button(crate::i18n::tr("Create Folder…")).clicked() {
                app.ui.dialog = Some(crate::state::Dialog::create_preset_folder(None));
            }
        });
        let more = icon_button(&mut hdr, "presetMore", Icon::More, vec2(26.0, 22.0), false, true, crate::i18n::tr("More preset options"));
        register(ui.ctx(), "icon:presetMore", more.rect);
        egui::Popup::menu(&more).show(|ui| {
            let r = ui.checkbox(&mut app.ui.preset_thumbs, crate::i18n::tr("Show Thumbnails"));
            register(ui.ctx(), "presetMenu:thumbnails", r.rect);
            ui.separator();
            if ui.button(crate::i18n::tr("Import Presets…")).clicked() {
                let _ = app.run("file.importPresets", json!({}));
            }
            let any_user = app.session.presets.iter().any(|p| !p.builtin);
            if ui.add_enabled(any_user, egui::Button::new(crate::i18n::tr("Export User Presets…"))).clicked() {
                let _ = app.run("file.exportPresets", json!({}));
            }
        });
        // folded: the header is the whole panel, and the hairline under it closes it off from the
        // panels below
        if !unfolded {
            divider(ui);
            return;
        }
        divider(ui);
        // amount slider (applies to the last applied preset)
        let amt_id = egui::Id::new("preset-amount");
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
                shade: lcc_develop::Shade::Plain,
            };
            let out = slider(ui, &spec, last.amount, true, None);
            if let Some(v) = out.value
                && app.run("edit.undo", json!({})).is_ok()
            {
                // Only replace the most recent preset; never undo intervening edits.
                if app.run("preset.apply", json!({"id": &last.id, "amount": v, "followAutoSync": true})).is_ok() {
                    AppliedByPhoto::remember(ui.ctx(), &app.session, &last.id);
                    ui.data_mut(|d| d.insert_temp(amt_id, AppliedPreset::new(&app.session, last.id, v)));
                } else {
                    let _ = app.run("edit.redo", json!({}));
                    ui.data_mut(|d| d.remove::<AppliedPreset>(amt_id));
                }
            }
            divider(ui);
        } else if last.as_ref().is_some_and(|last| last.is_stale(&app.session)) {
            // kept across a switch to another photo and back (the slider returns with it); gone once
            // a later edit, undo or redo owns the step
            ui.data_mut(|d| d.remove::<AppliedPreset>(amt_id));
        }
        let active = app.session.active();
        let marked = AppliedByPhoto::of(ui.ctx(), &app.session, active);
        let thumbs = app.ui.preset_thumbs && active.is_some();
        let row_h = if thumbs { 54.0 } else { 26.0 };
        let groups = grouped(ui.ctx(), &app.session, app.ui.settings.user_presets_first);
        let mut all_user_folders = Vec::new();
        collect_all_user_folders(&groups, &mut all_user_folders);
        all_user_folders.sort();

        let scroll = egui::ScrollArea::vertical().id_salt("presets-scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let visible = ui.clip_rect().expand(row_h);
            for node in groups.iter() {
                show_folder_node(app, ui, node, &all_user_folders, row_h, visible, &last, &marked, thumbs, amt_id);
            }
            let (space_r, space_resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
            // (the empty space under the folders: a drop target and the create menu)
            crate::widgets::announce(&space_resp, crate::i18n::tr("Presets").to_string());
            if let Some(dragged) = app.ui.dragging_preset_group.as_ref()
                && space_resp.hovered()
            {
                ui.painter().rect_stroke(space_r.shrink(1.0), 2.0, egui::Stroke::new(1.5, t.tool_active), egui::StrokeKind::Inside);
                if ui.input(|i| i.pointer.any_released()) {
                    let _ = app.run("preset.moveGroup", json!({"group": dragged, "parent": null}));
                    app.ui.dragging_preset_group = None;
                }
            }
            crate::widgets::context_menu(&space_resp, |ui| {
                if ui.button(crate::i18n::tr("Create Preset…")).clicked() {
                    app.ui.dialog = Some(crate::state::Dialog::create_preset(&app.session));
                }
                if ui.button(crate::i18n::tr("Create Folder…")).clicked() {
                    app.ui.dialog = Some(crate::state::Dialog::create_preset_folder(None));
                }
            });
            ui.add_space(30.0);
        });

        // Drop cancelled or ended anywhere
        if ui.input(|i| i.pointer.any_released()) {
            app.ui.dragging_preset = None;
            app.ui.dragging_preset_group = None;
        }

        // Drag feedback tooltip
        if let Some(pos) = ui.ctx().input(|i| i.pointer.hover_pos()) {
            if let Some(pid) = app.ui.dragging_preset.as_ref() {
                if let Some(pr) = app.session.presets.iter().find(|p| &p.id == pid) {
                    egui::Area::new(egui::Id::new("drag-preset"))
                        .order(egui::Order::Tooltip)
                        .interactable(false)
                        .fixed_pos(pos + vec2(14.0, 10.0))
                        .show(ui.ctx(), |ui| {
                            let t = Tokens::get(ui.ctx());
                            egui::Frame::popup(ui.style()).fill(t.chrome).show(ui, |ui| {
                                ui.label(&pr.name);
                            });
                        });
                }
            } else if let Some(grp) = app.ui.dragging_preset_group.as_ref() {
                egui::Area::new(egui::Id::new("drag-preset-group"))
                    .order(egui::Order::Tooltip)
                    .interactable(false)
                    .fixed_pos(pos + vec2(14.0, 10.0))
                    .show(ui.ctx(), |ui| {
                        let t = Tokens::get(ui.ctx());
                        egui::Frame::popup(ui.style()).fill(t.chrome).show(ui, |ui| {
                            ui.label(grp.rsplit_once('/').map(|(_, l)| l).unwrap_or(grp));
                        });
                    });
            }
        }
        let last_hover_id = egui::Id::new("last-preset-hover");
        let pointer_in_scroll = ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|pos| scroll.inner_rect.contains(pos));
        if pointer_in_scroll {
            if app.hover_preview.is_none()
                && let Some(id) = active
                && let Some((saved_id, saved_preset, saved_label)) = ui.data(|d| d.get_temp::<(lcc_catalog::PhotoId, Preset, String)>(last_hover_id))
                && saved_id == id
                && let Some(settings) = app.session.preset_applied(id, &saved_preset, 1.0)
            {
                app.hover_preview = Some(HoverPreview { label: saved_label, settings });
            }
        } else {
            ui.data_mut(|d| d.remove::<(lcc_catalog::PhotoId, Preset, String)>(last_hover_id));
        }
    });
}

/// A preset folder / group node in the hierarchy.
#[derive(Clone, Debug)]
pub struct FolderNode {
    pub name: String,
    pub path: String,
    pub builtin: bool,
    pub user: bool,
    pub favorites: bool,
    pub depth: usize,
    pub items: Vec<usize>,
    pub children: Vec<FolderNode>,
}

fn holds_current_preset(node: &FolderNode, app: &LccApp, last_id: Option<&str>, marked_id: Option<&str>) -> bool {
    let matches = |id: &str| last_id == Some(id) || marked_id == Some(id);
    let has_item = node.items.iter().any(|&i| app.session.presets.get(i).is_some_and(|p| matches(&p.id)));
    has_item || node.children.iter().any(|c| holds_current_preset(c, app, last_id, marked_id))
}

fn collect_all_user_folders(nodes: &[FolderNode], out: &mut Vec<String>) {
    for n in nodes {
        if n.user && !n.builtin && !n.favorites && !out.contains(&n.path) {
            out.push(n.path.clone());
        }
        collect_all_user_folders(&n.children, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn show_folder_node(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    node: &FolderNode,
    all_user_folders: &[String],
    row_h: f32,
    visible: Rect,
    last: &Option<AppliedPreset>,
    marked: &Option<String>,
    thumbs: bool,
    amt_id: egui::Id,
) {
    let t = Tokens::get(ui.ctx());
    let (path, name, builtin, favorites) = (node.path.clone(), node.name.clone(), node.builtin, node.favorites);
    let at_import = favorites && path == AT_IMPORT;
    let key = if at_import {
        AT_IMPORT.to_string()
    } else if favorites {
        "presetFavorites".to_string()
    } else {
        format!("preset:{path}")
    };
    let holds_current = holds_current_preset(node, app, last.as_ref().map(|l| l.id.as_str()), marked.as_deref());
    let open = app.ui.group_is_open(&key, holds_current || favorites);
    let indent = node.depth as f32 * 14.0;

    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click_and_drag());
    let label = if at_import {
        crate::i18n::tr("At Import")
    } else if favorites {
        crate::i18n::tr("Favorites")
    } else {
        crate::i18n::builtin_label(&name, builtin && node.depth == 0)
    };
    crate::widgets::announce(&resp, label.to_string());
    register(ui.ctx(), if favorites { key.clone() } else { format!("presetGroup:{path}") }, r);

    let is_drop_target = (app.ui.dragging_preset.is_some()
        || app.ui.dragging_preset_group.as_ref().is_some_and(|g| g != &path && !path.starts_with(&format!("{g}/"))))
        && resp.hovered();

    if is_drop_target {
        ui.painter().rect_stroke(r.shrink(1.0), 2.0, egui::Stroke::new(1.5, t.tool_active), egui::StrokeKind::Inside);
        if ui.input(|i| i.pointer.any_released()) {
            if let Some(dragged_pid) = app.ui.dragging_preset.take() {
                let _ = app.run("preset.move", json!({"id": dragged_pid, "group": path}));
            } else if let Some(dragged_grp) = app.ui.dragging_preset_group.take() {
                let _ = app.run("preset.moveGroup", json!({"group": dragged_grp, "parent": path}));
            }
        }
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.4));
    }

    if resp.drag_started() && node.user && !builtin && !favorites {
        app.ui.dragging_preset_group = Some(path.clone());
    }

    paint(
        ui.painter(),
        Rect::from_center_size(pos2(r.left() + 22.0 + indent, r.center().y), vec2(16.0, 16.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        t.text_label,
    );
    ui.painter().text(pos2(r.left() + 36.0 + indent, r.center().y), Align2::LEFT_CENTER, label, t.semibold(13.0), t.text_label);

    if resp.clicked() {
        app.ui.group_open.insert(key, !open);
    }

    if !favorites {
        crate::widgets::context_menu(&resp, |ui| {
            if ui.button(crate::i18n::tr("Create Preset…")).clicked() {
                app.ui.dialog = Some(crate::state::Dialog::create_preset_in(&app.session, Some(&path)));
            }
            if ui.button(crate::i18n::tr("Create Subfolder…")).clicked() {
                app.ui.dialog = Some(crate::state::Dialog::create_preset_folder(Some(path.clone())));
            }
            if node.user && !builtin {
                ui.separator();
                if ui.button(crate::i18n::tr("Rename…")).clicked() {
                    crate::panels::dialogs::prompt(app, "Rename Folder", "Folder name", &name, "preset.renameGroup", json!({"group": &path}), "name");
                }
                crate::widgets::menu_button(ui, crate::i18n::tr("Move to"), |ui| {
                    if node.depth > 0 && ui.button(crate::i18n::tr("Top Level")).clicked() {
                        let _ = app.run("preset.moveGroup", json!({"group": &path, "parent": null}));
                    }
                    for g in all_user_folders {
                        if g != &path && !g.starts_with(&format!("{path}/")) && ui.button(g).clicked() {
                            let _ = app.run("preset.moveGroup", json!({"group": &path, "parent": g}));
                        }
                    }
                });
                if ui.button(crate::i18n::tr("Delete Folder")).clicked() {
                    let _ = app.run("preset.deleteGroup", json!({"group": &path}));
                }
            }
            ui.separator();
            if ui.button(crate::i18n::tr("Export Folder…")).clicked() {
                let _ = app.run("file.exportPresets", json!({"group": path}));
            }
        });
    }

    if !open {
        return;
    }

    for child in &node.children {
        show_folder_node(app, ui, child, all_user_folders, row_h, visible, last, marked, thumbs, amt_id);
    }

    let active = app.session.active();
    let (first, end) = rows_in_view(ui.cursor().top(), row_h, node.items.len(), visible);
    if first > 0 {
        ui.allocate_space(vec2(ui.available_width(), first as f32 * row_h));
    }
    for &i in node.items.get(first..end).unwrap_or_default() {
        let Some(pr) = app.session.presets.get(i).cloned() else { continue };
        let (pid, pname, fav) = (pr.id.clone(), crate::i18n::builtin_label(&pr.name, pr.builtin).to_string(), pr.favorite);
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
        crate::widgets::announce(&resp, pname.clone());
        register(
            ui.ctx(),
            if node.path == AT_IMPORT {
                format!("presetAtImport:{pid}")
            } else if favorites {
                format!("presetFav:{pid}")
            } else {
                format!("preset:{pid}")
            },
            r,
        );
        let is_last = marked.as_deref() == Some(pid.as_str());

        if resp.drag_started() && !favorites {
            app.ui.dragging_preset = Some(pid.clone());
        }

        if is_last {
            register(ui.ctx(), "presetApplied", r);
            ui.painter().rect_filled(r, 0.0, t.tool_active);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.7));
        }
        // the preview is what a click commits: the same composition as `preset.apply` (`Session::preset_applied`)
        let look = if thumbs || resp.hovered() { active.and_then(|id| app.session.preset_applied(id, &pr, 1.0)) } else { None };
        let mut text_x = r.left() + 40.0 + indent;
        if thumbs && let (Some(id), Some(s)) = (active, look.as_ref()) {
            let tr = Rect::from_min_size(pos2(r.left() + 36.0 + indent, r.top() + 5.0), vec2(66.0, row_h - 10.0));
            ui.painter().rect_filled(tr, 2.0, t.canvas);
            if let Some(job) = app.session.variant_job(id, s, THUMB_EDGE)
                && let Some(tex) = app.renderer.variant(job)
            {
                ui.painter().image(tex.id(), tr, cover_uv(tr, tex.size), Color32::WHITE);
            }
            text_x = tr.right() + 10.0;
        }
        ui.painter().text(pos2(text_x, r.center().y), Align2::LEFT_CENTER, &pname, t.font(13.0), t.text_label);
        if fav {
            paint(ui.painter(), Rect::from_center_size(pos2(r.right() - 20.0, r.center().y), vec2(16.0, 16.0)), Icon::StarFilled, t.star);
        }
        if resp.hovered()
            && let Some(s) = look
        {
            let label = crate::i18n::tr_format!("Preset: {name}", name = pname);
            app.hover_preview = Some(HoverPreview { label: label.clone(), settings: s });
            if let Some(id) = active {
                ui.data_mut(|d| d.insert_temp(egui::Id::new("last-preset-hover"), (id, pr.clone(), label)));
            }
        }
        // a click follows Auto Sync, as a slider does; the whole selection on purpose: the menu
        if resp.clicked() && app.run("preset.apply", json!({"id": pid, "amount": 100, "followAutoSync": true})).is_ok() {
            AppliedByPhoto::remember(ui.ctx(), &app.session, &pid);
            ui.data_mut(|d| d.insert_temp(amt_id, AppliedPreset::new(&app.session, pid.clone(), 100.0)));
            ui.data_mut(|d| d.remove::<(lcc_catalog::PhotoId, Preset, String)>(egui::Id::new("last-preset-hover")));
            app.toast(ui.ctx(), crate::i18n::tr_format!("Preset: {name}", name = pname));
        }
        let (p_builtin, p_group) = (pr.builtin, pr.group.clone());
        crate::widgets::context_menu(&resp, |ui| {
            let selected = app.session.selection.ids.len();
            if selected > 1 && !app.session.auto_sync {
                if ui.button(crate::i18n::tr_format!("Apply to {n} Selected Photos", n = selected)).clicked() {
                    match app.run("preset.apply", json!({"id": pid, "amount": 100})) {
                        Ok(_) => app.toast(ui.ctx(), crate::i18n::tr_format!("Preset {name} on {n} photos", name = pname, n = selected)),
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
                ui.separator();
            }
            if ui.button(crate::i18n::tr(if fav { "Remove from Favorites" } else { "Add to Favorites" })).clicked() {
                let _ = app.run("preset.favorite", json!({"id": pid}));
            }
            let marked = app.session.preset_at_import.contains(&pid);
            if ui.button(crate::i18n::tr(if marked { "Remove from At Import" } else { "Add to At Import" })).clicked() {
                let _ = app.run("preset.forImport", json!({"id": pid}));
            }
            if ui.button(crate::i18n::tr("Hide")).clicked() {
                let _ = app.run("preset.hide", json!({"id": pid, "hidden": true}));
                app.toast(ui.ctx(), crate::i18n::tr("Hidden: Manage Presets… shows it again"));
            }
            if ui.button(crate::i18n::tr("Manage Presets…")).clicked() {
                let _ = app.run("dialog.presetsManager", json!({}));
            }
            if p_builtin {
                return;
            }
            ui.separator();
            if ui.add_enabled(app.session.active().is_some(), egui::Button::new(crate::i18n::tr("Update with Current Settings…"))).clicked()
                && let Err(e) = app.run("dialog.updatePreset", json!({"id": pid}))
            {
                app.toast(ui.ctx(), e);
            }
            if app.session.preset_undo.as_ref().is_some_and(|(id, _)| *id == pid) && ui.button(crate::i18n::tr("Undo Update")).clicked() {
                match app.run("preset.undoUpdate", json!({})) {
                    Ok(_) => app.toast(ui.ctx(), crate::i18n::tr_format!("“{name}” has its old settings again", name = pname)),
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
            if ui.button(crate::i18n::tr("Rename…")).clicked() {
                crate::panels::dialogs::prompt(app, "Rename Preset", "Preset name", &pname, "preset.rename", json!({"id": pid}), "name");
            }
            crate::widgets::menu_button(ui, crate::i18n::tr("Move to Group"), |ui| {
                for g in all_user_folders {
                    if g != &p_group && ui.button(g).clicked() {
                        let _ = app.run("preset.move", json!({"id": pid, "group": g}));
                    }
                }
                if ui.button(crate::i18n::tr("New Folder…")).clicked() {
                    crate::panels::dialogs::prompt(app, "Move Preset to New Folder", "Folder name", "", "preset.move", json!({"id": pid}), "group");
                }
            });
            ui.separator();
            if ui.button(crate::i18n::tr("Delete Preset")).clicked() {
                let _ = app.run("preset.delete", json!({"id": pid}));
            }
        });
    }
    if end < node.items.len() {
        ui.allocate_space(vec2(ui.available_width(), (node.items.len() - end) as f32 * row_h));
    }
}

/// The path of the At Import group (a list like Favorites, not a folder).
const AT_IMPORT: &str = "presetAtImport";

/// The group new presets are saved into by default.
const USER_PRESETS: &str = "User Presets";

/// The presets by folder (hierarchical tree) after a Favorites group of the starred ones, if any;
/// rebuilt only when the preset list changed (its generation). With `user_first` (Settings ▸
/// Interface, issue #476; off by default, as in Lightroom Classic) the user's own groups come right
/// after Favorites, "User Presets" first (LR-PRE-MANAGE); the built-in ones follow in their order.
fn grouped(ctx: &egui::Context, session: &lcc_engine::Session, user_first: bool) -> Arc<Vec<FolderNode>> {
    let (presets, custom_folders) = (&session.presets, &session.preset_folders);
    let hidden = |id: &str| session.preset_hidden.iter().any(|h| h == id);
    let id = egui::Id::new("presets-grouped");
    if let Some((generation, first, groups)) = ctx.data(|d| d.get_temp::<(u64, bool, Arc<Vec<FolderNode>>)>(id))
        && generation == presets.generation()
        && first == user_first
    {
        return groups;
    }

    let mut folder_data: BTreeMap<String, (bool, bool, Vec<usize>)> = BTreeMap::new();

    for (i, p) in presets.iter().enumerate() {
        // hidden in the Presets Manager: out of the panel (LR-PRE-MANAGE)
        if hidden(&p.id) {
            continue;
        }
        let (b, u, items) = folder_data.entry(p.group.clone()).or_insert_with(|| (false, false, Vec::new()));
        *b |= p.builtin;
        *u |= !p.builtin;
        items.push(i);

        let mut cur = p.group.as_str();
        while let Some((par, _)) = cur.rsplit_once('/') {
            let (pb, pu, _) = folder_data.entry(par.to_string()).or_insert_with(|| (false, false, Vec::new()));
            *pb |= p.builtin;
            *pu |= !p.builtin;
            cur = par;
        }
    }

    for f in custom_folders {
        let trimmed = f.trim();
        if !trimmed.is_empty() {
            folder_data.entry(trimmed.to_string()).or_insert_with(|| (false, true, Vec::new()));
            let mut cur = trimmed;
            while let Some((par, _)) = cur.rsplit_once('/') {
                folder_data.entry(par.to_string()).or_insert_with(|| (false, true, Vec::new()));
                cur = par;
            }
        }
    }

    fn build_nodes(parent_path: Option<&str>, depth: usize, folder_data: &BTreeMap<String, (bool, bool, Vec<usize>)>) -> Vec<FolderNode> {
        let mut direct_children: Vec<(String, String)> = Vec::new();
        for path in folder_data.keys() {
            match parent_path {
                None => {
                    if !path.contains('/') {
                        direct_children.push((path.clone(), path.clone()));
                    }
                }
                Some(parent) => {
                    if let Some(rest) = path.strip_prefix(parent)
                        && let Some(sub) = rest.strip_prefix('/')
                        && !sub.contains('/')
                    {
                        direct_children.push((sub.to_string(), path.clone()));
                    }
                }
            }
        }

        direct_children.sort_by_key(|a| a.0.to_lowercase());

        let mut nodes = Vec::new();
        for (name, full_path) in direct_children {
            let (builtin, user, items) = folder_data.get(&full_path).cloned().unwrap_or((false, false, Vec::new()));
            let children = build_nodes(Some(&full_path), depth + 1, folder_data);
            let any_builtin = builtin || children.iter().any(|c| c.builtin);
            let any_user = user || children.iter().any(|c| c.user);
            nodes.push(FolderNode { name, path: full_path, builtin: any_builtin, user: any_user, favorites: false, depth, items, children });
        }
        nodes
    }

    let mut roots = build_nodes(None, 0, &folder_data);
    // the user's own groups first ("User Presets" leading them), the built-in ones after
    if user_first {
        roots.sort_by_key(|n| (n.builtin || !n.user, n.path != USER_PRESETS));
    }

    fn collect_starred(nodes: &[FolderNode], presets: &PresetList) -> Vec<usize> {
        let mut starred = Vec::new();
        for node in nodes {
            for &i in &node.items {
                if presets.get(i).is_some_and(|p| p.favorite) && !starred.contains(&i) {
                    starred.push(i);
                }
            }
            starred.extend(collect_starred(&node.children, presets));
        }
        starred
    }

    // the presets marked At Import, after Favorites (LR-PRE-ONIMPORT)
    let at_import: Vec<usize> =
        session.preset_at_import.iter().filter(|id| !hidden(id)).filter_map(|id| presets.iter().position(|p| p.id == *id)).collect();
    if !at_import.is_empty() {
        roots.insert(
            0,
            FolderNode {
                name: "At Import".into(),
                path: AT_IMPORT.into(),
                builtin: false,
                user: false,
                favorites: true,
                depth: 0,
                items: at_import,
                children: Vec::new(),
            },
        );
    }
    let starred = collect_starred(&roots, presets);
    if !starred.is_empty() {
        roots.insert(
            0,
            FolderNode {
                name: "Favorites".into(),
                path: "presetFavorites".into(),
                builtin: false,
                user: false,
                favorites: true,
                depth: 0,
                items: starred,
                children: Vec::new(),
            },
        );
    }

    let groups = Arc::new(roots);
    ctx.data_mut(|d| d.insert_temp(id, (presets.generation(), user_first, groups.clone())));
    groups
}

/// The presets as the rows of a picker (a combo box's list): a search over names and groups;
/// Favorites and At Import first, then the user's own groups, then the built-in ones, each a
/// header that folds (Favorites, At Import and the group of `selected` start open, the rest
/// folded; a search opens what it finds). Each preset's star toggles it as a favourite in place,
/// and resting on one shows a small render of the active photo with it beside the list
/// ([`picker_frame`]: a variant thumbnail rendered in the background, never waited for). Only
/// the rows in view are built (10 000+ presets made an open picker take seconds a frame).
/// Returns the id of the preset clicked.
pub fn picker_rows(ui: &mut egui::Ui, salt: &str, session: &lcc_engine::Session, selected: Option<&str>) -> Option<String> {
    let presets = &session.presets;
    let t = Tokens::get(ui.ctx());
    // a search over names and groups (LR-PRESET-PICKER), kept per picker while it is open
    let qid = egui::Id::new(("preset-picker-query", salt));
    let mut query = ui.ctx().data(|d| d.get_temp::<String>(qid)).unwrap_or_default();
    if presets.len() > 12 {
        crate::text_field::TextField::singleline(&format!("presetPickerSearch-{salt}"), &mut query)
            .hint(crate::i18n::tr("Search presets"))
            .width(ui.available_width().max(160.0))
            .show(ui);
        ui.ctx().data_mut(|d| d.insert_temp(qid, query.clone()));
    }
    let needle = query.trim().to_lowercase();
    // which groups are open, by key (the picker's own, kept while the app runs)
    let fid = egui::Id::new(("preset-picker-open", salt));
    let mut folds: PickerFolds = ui.ctx().data(|d| d.get_temp(fid)).unwrap_or_default();
    let selected_group = selected.and_then(|id| presets.iter().find(|p| p.id == id)).map(|p| p.group.clone());
    let id = egui::Id::new(("preset-picker-rows", salt));
    let key = (presets.generation(), needle.clone(), folds.generation, selected_group.clone());
    let rows = match ui.ctx().data(|d| d.get_temp::<(PickKey, Arc<Vec<PickRow>>)>(id)) {
        Some((k, rows)) if k == key => rows,
        _ => {
            let rows = Arc::new(pick_rows(session, &needle, &folds, selected_group.as_deref()));
            ui.ctx().data_mut(|d| d.insert_temp(id, (key, rows.clone())));
            rows
        }
    };
    let row_h = ui.spacing().interact_size.y;
    // wide enough for the longest name it can show (a narrow combo cut 10 000 names to "01 Ava Ce…"),
    // within the screen
    let want = picker_width(ui, session, row_h);
    ui.set_min_width(want.min((ui.ctx().content_rect().width() - 2.0 * PICKER_MARGIN).max(160.0)));
    let mut out = None;
    let mut fold: Option<(String, bool)> = None;
    egui::ScrollArea::vertical().id_salt(("preset-picker", salt)).max_height(300.0).auto_shrink([false, true]).show_rows(
        ui,
        row_h,
        rows.len(),
        |ui, range| {
            for row in rows.get(range).unwrap_or_default() {
                let size = vec2(ui.available_width(), row_h);
                match row {
                    PickRow::Header { label, key, count, open } => {
                        let (r, resp) = ui.allocate_exact_size(size, Sense::click());
                        register(ui.ctx(), format!("presetPickerGroup:{salt}:{key}"), r);
                        crate::widgets::announce(&resp, label.clone());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, t.radius(3.0), t.hover.gamma_multiply(0.5));
                        }
                        let chevron = Rect::from_center_size(pos2(r.left() + 8.0, r.center().y), vec2(9.0, 9.0));
                        paint(ui.painter(), chevron, if *open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_dim);
                        let text = format!("{label}  ({count})");
                        let g = crate::widgets::one_line(ui.painter(), &text, t.semibold(11.0), t.text_dim, r.width() - 22.0);
                        ui.painter().galley(pos2(r.left() + 18.0, r.center().y - g.size().y / 2.0), g, t.text_dim);
                        if resp.clicked() && needle.is_empty() {
                            fold = Some((key.clone(), !open));
                        }
                    }
                    PickRow::Preset(i) => {
                        let Some(p) = presets.get(*i) else { continue };
                        let label = crate::i18n::builtin_label(&p.name, p.builtin);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let w = (ui.available_width() - row_h - 2.0).max(40.0);
                            let resp = ui.add_sized(
                                vec2(w, row_h),
                                egui::Button::selectable(selected == Some(p.id.as_str()), label).truncate().right_text(""),
                            );
                            register(ui.ctx(), format!("presetPick:{salt}:{}", p.id), resp.rect);
                            if resp.clicked() {
                                out = Some(p.id.clone());
                                // (pickers keep open for clicks inside: a choice closes them)
                                ui.close();
                            }
                            if resp.hovered() {
                                let frame = ui.ctx().cumulative_frame_nr();
                                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(PICKER_HOVER), (frame, p.id.clone(), resp.rect)));
                            }
                            let star = crate::rating::favorite_button(
                                ui,
                                format!("presetPickStar:{salt}:{}", p.id),
                                p.favorite,
                                crate::i18n::tr("Favorite"),
                            );
                            if star.clicked() {
                                ui.ctx().data_mut(|d| {
                                    d.get_temp_mut_or_default::<Vec<(String, bool)>>(egui::Id::new(PICKER_STARS)).push((p.id.clone(), !p.favorite))
                                });
                            }
                        });
                    }
                }
            }
        },
    );
    if let Some((key, open)) = fold {
        folds.open.insert(key, open);
        folds.generation = folds.generation.wrapping_add(1);
        ui.ctx().data_mut(|d| d.insert_temp(fid, folds));
    }
    out
}

/// The room a picker keeps from the screen's edges.
const PICKER_MARGIN: f32 = 16.0;
/// How many of the longest names (by characters) a picker measures to find its width.
const PICKER_MEASURED: usize = 48;

/// The width a picker needs for its longest shown name (a preset with its star, or a group with
/// its count), measured once per change of the presets: the [`PICKER_MEASURED`] longest names by
/// characters are laid out, not all 10 000.
pub(crate) fn picker_width(ui: &egui::Ui, session: &lcc_engine::Session, row_h: f32) -> f32 {
    let key = (session.presets.generation(), session.preset_hidden.len(), row_h.to_bits());
    let id = egui::Id::new("preset-picker-width");
    if let Some((k, w)) = ui.ctx().data(|d| d.get_temp::<((u64, usize, u32), f32)>(id))
        && k == key
    {
        return w;
    }
    let t = Tokens::get(ui.ctx());
    let hidden: std::collections::HashSet<&str> = session.preset_hidden.iter().map(String::as_str).collect();
    let shown = || session.presets.iter().filter(|p| !hidden.contains(p.id.as_str()));
    let mut names: Vec<String> = shown().map(|p| crate::i18n::builtin_label(&p.name, p.builtin).to_string()).collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.chars().count()));
    names.truncate(PICKER_MEASURED);
    let mut groups: Vec<&str> = shown().map(|p| p.group.as_str()).collect();
    groups.sort_unstable();
    groups.dedup();
    groups.sort_by_key(|g| std::cmp::Reverse(g.chars().count()));
    groups.truncate(PICKER_MEASURED);
    let body = egui::TextStyle::Button.resolve(ui.style());
    let pad = ui.spacing().button_padding.x * 2.0;
    let measure = |text: String, font: egui::FontId| ui.painter().layout_no_wrap(text, font, t.text).size().x;
    let name_w = names.into_iter().map(|n| measure(n, body.clone())).fold(0.0_f32, f32::max) + pad + row_h + 2.0;
    // (a header: chevron, name and "  (12345)")
    let group_w = groups.into_iter().map(|g| measure(format!("{g}  (00000)"), t.semibold(11.0))).fold(0.0_f32, f32::max) + 22.0;
    // and the search field's least width
    let w = name_w.max(group_w).max(160.0) + ui.spacing().scroll.bar_width + 8.0;
    ui.ctx().data_mut(|d| d.insert_temp(id, (key, w)));
    w
}

/// Where a picker leaves the preset the pointer rests on (frame, id, row) and the stars clicked
/// this frame, for [`picker_frame`].
const PICKER_HOVER: &str = "preset-picker-hover";
const PICKER_STARS: &str = "preset-picker-stars";
/// Long edge of the picker's hover render.
const PICKER_PREVIEW_EDGE: usize = 192;

/// After the frame's pickers: the stars clicked in them toggle (`preset.favorite`), and the preset
/// the pointer rests on is shown on the active photo beside the list. The render is a variant
/// thumbnail at the thumbnail level, made in the background ([`crate::render`]); until it is ready
/// the box says so and the UI never waits.
pub fn picker_frame(app: &mut LccApp, ctx: &egui::Context) {
    let stars: Vec<(String, bool)> = ctx.data_mut(|d| d.remove_temp(egui::Id::new(PICKER_STARS))).unwrap_or_default();
    for (id, on) in stars {
        if let Err(e) = app.run("preset.favorite", json!({"id": id, "favorite": on})) {
            app.toast_error(ctx, e);
        }
    }
    let Some((frame, pid, row)) = ctx.data(|d| d.get_temp::<(u64, String, Rect)>(egui::Id::new(PICKER_HOVER))) else { return };
    if frame != ctx.cumulative_frame_nr() {
        return;
    }
    let Some(active) = app.session.active() else { return };
    // what a click commits (`Session::preset_applied`, as `preset.apply`)
    let Some(look) = app.session.presets.iter().find(|p| p.id == pid).and_then(|preset| app.session.preset_applied(active, preset, 1.0)) else {
        return;
    };
    let tex = app.session.variant_job(active, &look, PICKER_PREVIEW_EDGE).and_then(|job| app.renderer.variant(job)).map(|t| (t.id(), t.size));
    let t = Tokens::get(ctx);
    let side = 150.0;
    let screen = ctx.content_rect();
    // beside the list, on the side with room
    let x = if row.right() + 12.0 + side <= screen.right() { row.right() + 12.0 } else { (row.left() - 12.0 - side).max(screen.left()) };
    let y = row.top().clamp(screen.top(), (screen.bottom() - side).max(screen.top()));
    egui::Area::new(egui::Id::new("preset-picker-preview")).order(egui::Order::Tooltip).interactable(false).fixed_pos(pos2(x, y)).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).fill(t.chrome).show(ui, |ui| {
            let (r, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
            register(ui.ctx(), "presetPickerPreview", r);
            ui.painter().rect_filled(r, 2.0, t.canvas);
            match tex {
                Some((id, size)) => {
                    let fit = fit_rect(r, size);
                    ui.painter().image(id, fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, crate::i18n::tr("Rendering…"), t.font(11.0), t.text_dim);
                    ctx.request_repaint_after(std::time::Duration::from_millis(60));
                }
            }
        });
    });
}

/// `size` fitted inside `r`, centred.
fn fit_rect(r: Rect, size: [usize; 2]) -> Rect {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let k = (r.width() / w).min(r.height() / h);
    Rect::from_center_size(r.center(), vec2(w * k, h * k))
}

/// A picker's open and folded groups.
#[derive(Clone, Default)]
struct PickerFolds {
    open: std::collections::HashMap<String, bool>,
    generation: u64,
}

type PickKey = (u64, String, u64, Option<String>);

/// The picker's rows ([`picker_rows`]); hidden presets are left out.
fn pick_rows(session: &lcc_engine::Session, needle: &str, folds: &PickerFolds, selected_group: Option<&str>) -> Vec<PickRow> {
    let presets = &session.presets;
    let hidden: std::collections::HashSet<&str> = session.preset_hidden.iter().map(String::as_str).collect();
    let mut rows = Vec::with_capacity(presets.len() + 16);
    let block = |rows: &mut Vec<PickRow>, key: String, label: String, items: Vec<usize>, default_open: bool| {
        if items.is_empty() {
            return;
        }
        let open = !needle.is_empty() || folds.open.get(&key).copied().unwrap_or(default_open);
        rows.push(PickRow::Header { label, key, count: items.len(), open });
        if open {
            rows.extend(items.into_iter().map(PickRow::Preset));
        }
    };
    if needle.is_empty() {
        let favorites: Vec<usize> =
            presets.iter().enumerate().filter(|(_, p)| p.favorite && !hidden.contains(p.id.as_str())).map(|(i, _)| i).collect();
        block(&mut rows, "presetFavorites".into(), crate::i18n::tr("Favorites").to_string(), favorites, true);
        let at_import: Vec<usize> = session
            .preset_at_import
            .iter()
            .filter(|id| !hidden.contains(id.as_str()))
            .filter_map(|id| presets.iter().position(|p| p.id == *id))
            .collect();
        block(&mut rows, AT_IMPORT.into(), crate::i18n::tr("At Import").to_string(), at_import, true);
    }
    // groups in list order, the user's own first
    let mut groups: Vec<(&str, bool, bool, Vec<usize>)> = Vec::new();
    for (i, p) in presets.iter().enumerate() {
        if hidden.contains(p.id.as_str()) || !(needle.is_empty() || p.name.to_lowercase().contains(needle) || p.group.to_lowercase().contains(needle))
        {
            continue;
        }
        match groups.iter_mut().find(|g| g.0 == p.group) {
            Some(g) => {
                g.1 |= p.builtin;
                g.2 |= !p.builtin;
                g.3.push(i);
            }
            None => groups.push((&p.group, p.builtin, !p.builtin, vec![i])),
        }
    }
    groups.sort_by_key(|g| !g.2);
    for (g, builtin, user, items) in groups {
        let label = crate::i18n::builtin_label(g, builtin && !user).to_string();
        block(&mut rows, format!("preset:{g}"), label, items, selected_group == Some(g));
    }
    rows
}

#[derive(Clone)]
enum PickRow {
    /// A group (its name in the UI language for stock groups), its fold key, how many presets
    /// it shows and whether it is open.
    Header { label: String, key: String, count: usize, open: bool },
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
        let mut session = lcc_engine::Session::with_demo();
        let pid = "lc.golden-hour";
        session.execute("preset.apply", &json!({"id": pid})).unwrap();
        let state = AppliedPreset::new(&session, pid.into(), 100.0);
        assert!(state.is_current(&session));

        session.execute("develop.set", &json!({"values": {"light.exposure": 0.75}})).unwrap();
        assert!(!state.is_current(&session), "a newer edit owns the undo step");
    }

    /// Issue #1999: starred presets are listed first, in a Favorites group, in the order the panel
    /// shows them below, and stay in their own groups; a user group named "Favorites" is not it.
    #[test]
    fn favorites_group_comes_first_in_panel_order() {
        let mut session = lcc_engine::Session::with_demo();
        let base = session.presets.iter().find(|p| p.id == "lc.warm-glow").unwrap().clone();
        for (id, group, favorite) in [("user.mine", "Mine", true), ("user.plain", "Favorites", false)] {
            session.presets.push(Preset { id: id.into(), name: id.into(), group: group.into(), favorite, builtin: false, ..base.clone() });
        }
        for id in ["lc.warm-glow", "lc.bw-selenium"] {
            session.execute("preset.favorite", &json!({"id": id})).unwrap();
        }
        let index = |id: &str| session.presets.iter().position(|p| p.id == id).unwrap();
        let (warm, bw, mine, plain) = (index("lc.warm-glow"), index("lc.bw-selenium"), index("user.mine"), index("user.plain"));
        assert!(warm < bw, "list order differs from panel order");

        let ctx = egui::Context::default();
        let groups = grouped(&ctx, &session, false);
        assert_eq!(groups[0].name, "Favorites");
        assert!(!groups[0].user && !groups[0].builtin, "not a Move to Group target");
        // B&W, then Color, then the user groups
        assert_eq!(groups[0].items, vec![bw, warm, mine]);
        for i in [bw, warm, mine] {
            assert!(groups[1..].iter().any(|g| g.items.contains(&i)), "a favourite stays in its own group");
        }
        let named: Vec<&FolderNode> = groups[1..].iter().filter(|g| g.name == "Favorites").collect();
        assert_eq!(named.len(), 1);
        assert!(named[0].user);
        assert_eq!(named[0].items, vec![plain]);

        for id in ["lc.warm-glow", "lc.bw-selenium", "user.mine"] {
            session.execute("preset.favorite", &json!({"id": id})).unwrap();
        }
        let groups = grouped(&ctx, &session, false);
        assert_eq!(groups[0].name, "B&W", "no Favorites group without favourites");
    }

    #[test]
    fn amount_slider_does_not_undo_on_another_photo() {
        let mut session = lcc_engine::Session::with_demo();
        let pid = "lc.golden-hour";
        session.execute("preset.apply", &json!({"id": pid})).unwrap();
        let state = AppliedPreset::new(&session, pid.into(), 100.0);
        let active = session.active();
        let other = session.visible().iter().copied().find(|id| Some(*id) != active).unwrap();
        session.execute("library.select", &json!({"ids": [other.0]})).unwrap();
        assert!(!state.is_current(&session), "the preset belongs to the earlier selection");
    }

    /// Issue #476: the User Presets group can be listed first (after Favorites); the default keeps
    /// Lightroom Classic's name order, and built-in groups keep their order either way.
    #[test]
    fn user_presets_group_first_only_when_asked() {
        let mut session = lcc_engine::Session::with_demo();
        let base = session.presets.iter().find(|p| p.id == "lc.warm-glow").unwrap().clone();
        session.presets.push(Preset { id: "user.mine".into(), name: "Mine".into(), group: "User Presets".into(), builtin: false, ..base.clone() });
        session.presets.push(Preset { id: "user.a".into(), name: "A".into(), group: "Aaa Mine".into(), builtin: false, ..base });
        session.execute("preset.favorite", &json!({"id": "lc.warm-glow"})).unwrap();
        let ctx = egui::Context::default();
        let names = |first| grouped(&ctx, &session, first).iter().map(|g| g.name.clone()).collect::<Vec<_>>();
        let off = names(false);
        assert_ne!(off[1], "User Presets", "default order unchanged");
        let on = names(true);
        assert_eq!(on[0], "Favorites");
        assert_eq!(on[1], "User Presets");
        let rest_off: Vec<_> = off.iter().filter(|n| *n != "User Presets").collect();
        let rest_on: Vec<_> = on.iter().filter(|n| *n != "User Presets").collect();
        assert_eq!(rest_off, rest_on, "every other group keeps its place");
    }

    /// Issue #468: Preset folders and nested subfolders.
    #[test]
    fn nested_preset_folders_structure_and_hierarchy() {
        let mut session = lcc_engine::Session::with_demo();
        let base = session.presets.iter().find(|p| p.id == "lc.warm-glow").unwrap().clone();
        session.presets.push(Preset {
            id: "user.mastin-direct".into(),
            name: "Direct Mastin".into(),
            group: "Mastin Labs".into(),
            builtin: false,
            ..base.clone()
        });
        session.presets.push(Preset {
            id: "user.mastin-portra".into(),
            name: "Portra 400".into(),
            group: "Mastin Labs/Portra Original".into(),
            builtin: false,
            ..base.clone()
        });
        session.presets.push(Preset {
            id: "user.mastin-adv".into(),
            name: "Adventure 01".into(),
            group: "Mastin Labs/Adventure Everyday".into(),
            builtin: false,
            ..base.clone()
        });

        let ctx = egui::Context::default();
        let groups = grouped(&ctx, &session, false);

        let mastin = groups.iter().find(|g| g.name == "Mastin Labs").expect("Mastin Labs folder at root");
        assert_eq!(mastin.depth, 0);
        assert_eq!(mastin.path, "Mastin Labs");
        assert_eq!(mastin.items.len(), 1, "direct preset in Mastin Labs");
        assert_eq!(mastin.children.len(), 2, "two subfolders under Mastin Labs");
        assert_eq!(mastin.children[0].name, "Adventure Everyday");
        assert_eq!(mastin.children[0].path, "Mastin Labs/Adventure Everyday");
        assert_eq!(mastin.children[0].depth, 1);
        assert_eq!(mastin.children[1].name, "Portra Original");
        assert_eq!(mastin.children[1].path, "Mastin Labs/Portra Original");
        assert_eq!(mastin.children[1].depth, 1);
    }

    /// Issue #468: Create, rename, move and delete preset folders.
    #[test]
    fn preset_folder_commands_crud() {
        let mut session = lcc_engine::Session::with_demo();
        let base = session.presets.iter().find(|p| p.id == "lc.warm-glow").unwrap().clone();

        // 1. Create folders (including empty parent and child)
        session.execute("preset.createGroup", &json!({"name": "Mastin Labs"})).unwrap();
        session.execute("preset.createGroup", &json!({"name": "Portra Original", "parent": "Mastin Labs"})).unwrap();
        assert!(session.preset_folders.contains(&"Mastin Labs".to_string()));
        assert!(session.preset_folders.contains(&"Mastin Labs/Portra Original".to_string()));

        // Add a preset into the folder
        session.presets.push(Preset {
            id: "user.p400".into(),
            name: "Portra 400".into(),
            group: "Mastin Labs/Portra Original".into(),
            builtin: false,
            ..base
        });

        // 2. Rename folder
        session.execute("preset.renameGroup", &json!({"group": "Mastin Labs/Portra Original", "name": "Portra Pack"})).unwrap();
        assert!(session.preset_folders.contains(&"Mastin Labs/Portra Pack".to_string()));
        let p = session.presets.iter().find(|p| p.id == "user.p400").unwrap();
        assert_eq!(p.group, "Mastin Labs/Portra Pack");

        // 3. Move folder to top level
        session.execute("preset.moveGroup", &json!({"group": "Mastin Labs/Portra Pack", "parent": null})).unwrap();
        let p = session.presets.iter().find(|p| p.id == "user.p400").unwrap();
        assert_eq!(p.group, "Portra Pack");

        // 4. Move folder back into Mastin Labs
        session.execute("preset.moveGroup", &json!({"group": "Portra Pack", "parent": "Mastin Labs"})).unwrap();
        let p = session.presets.iter().find(|p| p.id == "user.p400").unwrap();
        assert_eq!(p.group, "Mastin Labs/Portra Pack");

        // 5. Delete folder deletes presets inside it
        session.execute("preset.deleteGroup", &json!({"group": "Mastin Labs"})).unwrap();
        assert!(!session.presets.iter().any(|p| p.id == "user.p400"));
        assert!(!session.preset_folders.iter().any(|f| f.starts_with("Mastin Labs")));
    }
}
