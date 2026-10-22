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

/// The preset amount slider may replace only the undo step it created. One entry per photo
/// (#2392): switching photos must not forget which preset a photo received, so returning to
/// it restores the highlight (and the slider) while a photo that never received one shows none.
#[derive(Clone)]
struct AppliedPreset {
    id: String,
    amount: f64,
    selected: Vec<lightcraft_catalog::PhotoId>,
    active: Option<lightcraft_catalog::PhotoId>,
    commits: u64,
    /// Hash of the photo's develop settings right after the apply: the highlight follows the
    /// photo while its edits are unchanged, and clears on an undo, a redo or any new edit.
    hash: u64,
}

impl AppliedPreset {
    fn new(session: &lightcraft_engine::Session, id: String, amount: f64, hash: u64) -> Self {
        Self { id, amount, selected: session.selection.ids.clone(), active: session.active(), commits: session.commits(), hash }
    }

    fn is_current(&self, session: &lightcraft_engine::Session) -> bool {
        self.selected == session.selection.ids
            && self.active == session.active()
            && self.commits == session.commits()
            && session.interaction.is_none()
            && session.redo.is_empty()
            && session
                .undo
                .last()
                .is_some_and(|entry| session.presets.iter().any(|preset| preset.id == self.id && entry.label == format!("Preset: {}", preset.name)))
    }
}

/// Presets applied this session, by photo.
type AppliedPresets = BTreeMap<lightcraft_catalog::PhotoId, AppliedPreset>;

fn applied_id() -> egui::Id {
    egui::Id::new("preset-applied")
}

/// Remember the preset just applied to every photo that received it (the whole selection).
fn record(ui: &egui::Ui, session: &lightcraft_engine::Session, id: &str, amount: f64) {
    let mut applied: AppliedPresets = ui.data(|d| d.get_temp(applied_id())).unwrap_or_default();
    for photo in session.selection.ids.iter() {
        if let Some(d) = session.develop_of(*photo) {
            applied.insert(*photo, AppliedPreset::new(session, id.to_string(), amount, d.hash64()));
        }
    }
    ui.data_mut(|d| d.insert_temp(applied_id(), applied));
}

/// The preset the panel highlights for `photo`: the one recorded for it, while its edits are
/// unchanged since the apply (during an interaction the settings hold drafts, so the last
/// state keeps showing).
fn highlight_for<'a>(
    session: &lightcraft_engine::Session,
    applied: &'a AppliedPresets,
    photo: lightcraft_catalog::PhotoId,
) -> Option<&'a AppliedPreset> {
    applied.get(&photo).filter(|h| session.interaction.is_some() || session.develop_of(photo).is_some_and(|d| d.hash64() == h.hash))
}

/// The preset id the panel would highlight for `photo` (test hook).
#[cfg(test)]
pub(crate) fn highlight_for_test(ctx: &egui::Context, session: &lightcraft_engine::Session, photo: lightcraft_catalog::PhotoId) -> Option<String> {
    let applied: AppliedPresets = ctx.data(|d| d.get_temp(applied_id())).unwrap_or_default();
    highlight_for(session, &applied, photo).map(|h| h.id.clone())
}

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
            register(ui.ctx(), "presetMenu:thumbnails", r.rect);
            ui.separator();
            // every group at once (#2467): with dozens of imported groups open, finding one meant a
            // long scroll or closing the others one by one
            let groups = grouped(ui.ctx(), &app.session.presets);
            let any = !groups.is_empty();
            let expand = ui.add_enabled(any, egui::Button::new(crate::i18n::tr("Expand All Groups")));
            register(ui.ctx(), "presetMenu:expandAll", expand.rect);
            if expand.clicked() {
                for grp in groups.iter() {
                    ui.data_mut(|d| d.insert_temp(egui::Id::new(("preset-group", &grp.name)), true));
                }
            }
            let collapse = ui.add_enabled(any, egui::Button::new(crate::i18n::tr("Collapse All Groups")));
            register(ui.ctx(), "presetMenu:collapseAll", collapse.rect);
            if collapse.clicked() {
                for grp in groups.iter() {
                    ui.data_mut(|d| d.insert_temp(egui::Id::new(("preset-group", &grp.name)), false));
                }
            }
            ui.separator();
            if ui.button(crate::i18n::tr("Import Presets…")).clicked() {
                let _ = app.run("file.importPresets", json!({}));
            }
            let any_user = app.session.presets.iter().any(|p| !p.builtin);
            if ui.add_enabled(any_user, egui::Button::new(crate::i18n::tr("Export User Presets…"))).clicked() {
                let _ = app.run("file.exportPresets", json!({}));
            }
        });
        divider(ui);
        // amount slider (applies to the last applied preset)
        let applied: AppliedPresets = ui.data(|d| d.get_temp(applied_id())).unwrap_or_default();
        let last = app.session.active().and_then(|id| applied.get(&id).cloned()).filter(|last| last.is_current(&app.session));
        if let Some(last) = last {
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
                    record(ui, &app.session, &last.id, v);
                } else {
                    let _ = app.run("edit.redo", json!({}));
                    if let Some(id) = app.session.active() {
                        ui.data_mut(|d| {
                            let mut map: AppliedPresets = d.get_temp(applied_id()).unwrap_or_default();
                            map.remove(&id);
                            d.insert_temp(applied_id(), map);
                        });
                    }
                }
            }
            divider(ui);
        }
        let active = app.session.active();
        let current = active.and_then(|id| app.session.develop_of(id)).map(|d| (*d).clone());
        // the preset this photo received, while its edits are unchanged (#2392)
        let highlight = active.and_then(|id| highlight_for(&app.session, &applied, id));
        let thumbs = app.ui.preset_thumbs && active.is_some();
        let row_h = if thumbs { 54.0 } else { 26.0 };
        let groups = grouped(ui.ctx(), &app.session.presets);
        let scroll = egui::ScrollArea::vertical().id_salt("presets-scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            // Only the rows in view are laid out, drawn and given thumbnails: a Lightroom collection
            // imported whole is 10 000+ presets, and building every row each frame took seconds.
            let visible = ui.clip_rect().expand(row_h);
            for grp in groups.iter() {
                let (g, builtin) = (grp.name.clone(), grp.builtin);
                let open_id = egui::Id::new(("preset-group", &g));
                let open: bool = ui.data(|d| d.get_temp(open_id)).unwrap_or(true);
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                crate::widgets::announce(&resp, g.to_string());
                register(ui.ctx(), format!("presetGroup:{g}"), r);
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
                if resp.clicked() {
                    ui.data_mut(|d| d.insert_temp(open_id, !open));
                }
                resp.context_menu(|ui| {
                    if ui.button(crate::i18n::tr("Export Group…")).clicked() {
                        let _ = app.run("file.exportPresets", json!({"group": g}));
                    }
                });
                if !open {
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
                    register(ui.ctx(), format!("preset:{pid}"), r);
                    let is_last = highlight.as_ref().is_some_and(|h| h.id == pid);
                    if is_last {
                        ui.painter().rect_filled(r, 0.0, t.tool_active);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.7));
                    }
                    let look = if thumbs || resp.hovered() { current.as_ref().map(|d| pr.apply(d, 1.0)) } else { None };
                    let mut text_x = r.left() + 40.0;
                    if thumbs && let (Some(id), Some(s)) = (active, look.as_ref()) {
                        let tr = Rect::from_min_size(pos2(r.left() + 36.0, r.top() + 5.0), vec2(66.0, row_h - 10.0));
                        ui.painter().rect_filled(tr, 2.0, t.canvas);
                        if let Some(job) = app.session.variant_job(id, s, THUMB_EDGE)
                            && let Some(tex) = app.renderer.variant(job)
                        {
                            ui.painter().image(tex.tex.id(), tr, cover_uv(tr, tex.size), Color32::WHITE);
                        }
                        text_x = tr.right() + 10.0;
                    }
                    ui.painter().text(pos2(text_x, r.center().y), Align2::LEFT_CENTER, &name, t.font(13.0), t.text_label);
                    if fav {
                        paint(ui.painter(), Rect::from_center_size(pos2(r.right() - 20.0, r.center().y), vec2(16.0, 16.0)), Icon::StarFilled, t.star);
                    }
                    // resting on a preset previews it in the loupe (no history entry)
                    if resp.hovered()
                        && let Some(s) = look
                    {
                        let label = crate::i18n::tr_format!("Preset: {name}", name = name);
                        app.hover_preview = Some(HoverPreview { label: label.clone(), settings: s });
                        if let Some(id) = active {
                            ui.data_mut(|d| d.insert_temp(egui::Id::new("last-preset-hover"), (id, pr.clone(), label)));
                        }
                    }
                    if resp.clicked() && app.run("preset.apply", json!({"id": pid, "amount": 100})).is_ok() {
                        record(ui, &app.session, &pid, 100.0);
                        ui.data_mut(|d| d.remove::<(lightcraft_catalog::PhotoId, Preset, String)>(egui::Id::new("last-preset-hover")));
                        app.toast(ui.ctx(), crate::i18n::tr_format!("Preset: {name}", name = name));
                    }
                    let (builtin, group) = (pr.builtin, pr.group.clone());
                    resp.context_menu(|ui| {
                        if ui.button(crate::i18n::tr(if fav { "Remove from Favorites" } else { "Add to Favorites" })).clicked() {
                            let _ = app.run("preset.favorite", json!({"id": pid}));
                        }
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
            ui.add_space(30.0);
        });
        let last_hover_id = egui::Id::new("last-preset-hover");
        let pointer_in_scroll = ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|pos| scroll.inner_rect.contains(pos));
        if pointer_in_scroll {
            if app.hover_preview.is_none()
                && let (Some(id), Some(curr)) = (active, current.as_ref())
                && let Some((saved_id, saved_preset, saved_label)) =
                    ui.data(|d| d.get_temp::<(lightcraft_catalog::PhotoId, Preset, String)>(last_hover_id))
                && saved_id == id
            {
                app.hover_preview = Some(HoverPreview { label: saved_label, settings: saved_preset.apply(curr, 1.0) });
            }
        } else {
            ui.data_mut(|d| d.remove::<(lightcraft_catalog::PhotoId, Preset, String)>(last_hover_id));
        }
    });
}

/// A preset group as the panel lists it: indices into the session's presets.
struct Group {
    name: String,
    /// Holds a stock preset: a stock group (shown in the UI language), even when user presets
    /// share its name; a group of user presets keeps its name.
    builtin: bool,
    /// Holds a user preset (a Move to Group target).
    user: bool,
    items: Vec<usize>,
}

/// The presets by group (groups sorted by name, presets in list order), rebuilt only when the
/// list changed (its generation).
fn grouped(ctx: &egui::Context, presets: &PresetList) -> Arc<Vec<Group>> {
    let id = egui::Id::new("presets-grouped");
    if let Some((generation, groups)) = ctx.data(|d| d.get_temp::<(u64, Arc<Vec<Group>>)>(id))
        && generation == presets.generation()
    {
        return groups;
    }
    let mut by_name: BTreeMap<&str, Group> = BTreeMap::new();
    for (i, p) in presets.iter().enumerate() {
        let g = by_name.entry(&p.group).or_insert_with(|| Group { name: p.group.clone(), builtin: false, user: false, items: Vec::new() });
        g.builtin |= p.builtin;
        g.user |= !p.builtin;
        g.items.push(i);
    }
    let groups = Arc::new(by_name.into_values().collect::<Vec<_>>());
    ctx.data_mut(|d| d.insert_temp(id, (presets.generation(), groups.clone())));
    groups
}

/// The presets as the rows of a picker (a combo box's list), in list order with a header row
/// wherever the group changes. Only the rows in view are built (10 000+ presets made an open
/// picker take seconds a frame). Returns the id of the preset clicked.
pub fn picker_rows(ui: &mut egui::Ui, salt: &str, presets: &PresetList, selected: Option<&str>) -> Option<String> {
    let id = egui::Id::new("preset-picker-rows");
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

    fn applied_hash(session: &lightcraft_engine::Session) -> u64 {
        session.develop_of(session.active().expect("a photo")).expect("settings").hash64()
    }

    #[test]
    fn amount_slider_does_not_undo_a_later_edit() {
        let mut session = lightcraft_engine::Session::with_demo();
        let pid = "lc.golden-hour";
        session.execute("preset.apply", &json!({"id": pid})).unwrap();
        let state = AppliedPreset::new(&session, pid.into(), 100.0, applied_hash(&session));
        assert!(state.is_current(&session));

        session.execute("develop.set", &json!({"values": {"light.exposure": 0.75}})).unwrap();
        assert!(!state.is_current(&session), "a newer edit owns the undo step");
    }

    #[test]
    fn amount_slider_does_not_undo_on_another_photo() {
        let mut session = lightcraft_engine::Session::with_demo();
        let pid = "lc.golden-hour";
        session.execute("preset.apply", &json!({"id": pid})).unwrap();
        let state = AppliedPreset::new(&session, pid.into(), 100.0, applied_hash(&session));
        let active = session.active();
        let other = session.visible().iter().copied().find(|id| Some(*id) != active).unwrap();
        session.execute("library.select", &json!({"ids": [other.0]})).unwrap();
        assert!(!state.is_current(&session), "the preset belongs to the earlier selection");
    }

    /// The highlight follows the photo (#2392): a selection change keeps the memory, a photo
    /// that never received the preset shows none, and an edit (undone or not) clears it until
    /// the photo's edits are the preset's again.
    #[test]
    fn highlight_follows_the_photo_not_the_selection() {
        let mut session = lightcraft_engine::Session::with_demo();
        let pid = "lc.golden-hour";
        session.execute("preset.apply", &json!({"id": pid})).unwrap();
        let active = session.active().expect("a photo");
        let mut applied: AppliedPresets = BTreeMap::new();
        applied.insert(active, AppliedPreset::new(&session, pid.into(), 100.0, applied_hash(&session)));
        let other = session.visible().iter().copied().find(|id| *id != active).unwrap();
        assert!(highlight_for(&session, &applied, other).is_none(), "a photo that never received one shows none");
        session.execute("library.select", &json!({"ids": [other.0]})).unwrap();
        assert!(highlight_for(&session, &applied, active).is_some(), "the memory outlives the selection change");
        session.execute("library.select", &json!({"ids": [active.0]})).unwrap();
        assert!(highlight_for(&session, &applied, active).is_some());
        session.execute("develop.set", &json!({"values": {"light.exposure": 0.75}})).unwrap();
        assert!(highlight_for(&session, &applied, active).is_none(), "the edits changed");
        session.execute("edit.undo", &json!({})).unwrap();
        assert!(highlight_for(&session, &applied, active).is_some(), "undo restored the preset's edits");
    }
}
