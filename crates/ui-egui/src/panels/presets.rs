//! The Presets column (opens to the left of the Edit panel): grouped presets with hover preview
//! (the loupe shows the photo with the preset while the pointer rests on it; nothing is
//! committed), optional live thumbnails, amount slider, create/favourite, import/export of preset
//! files.

use std::collections::BTreeMap;
use std::sync::Arc;

use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _};
use craft::ui::display::Divider;
use craft::ui::menu::{Menu, MenuItem};

use egui::{Align2, Rect, Sense, pos2, vec2};
use lightcraft_develop::{ControlSpec, Preset, Section, Track};
use lightcraft_engine::presets::PresetList;
use serde_json::json;

use crate::icons::Icon;
use crate::theme::Tokens;
use crate::widgets::{divider, icon_button, slider};
use crate::{HoverPreview, LightcraftApp};

mod sdk;

#[cfg(test)]
#[path = "presets/preset_options_external_20261119.rs"]
mod preset_options_external_20261119;

/// Long edge of preset thumbnails (px).
const THUMB_EDGE: usize = 128;

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
    let measuring = craft::ui::runtime::measure::active(ui.ctx());
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
            && !measuring
        {
            app.ui.dialog = Some(crate::state::Dialog::create_preset());
        }
        let more = icon_button(&mut hdr, "presetMore", Icon::More, vec2(28.0, 28.0), false, true, crate::i18n::tr("More preset options"));
        if !measuring && !ui.is_sizing_pass() {
            let pass = ui.ctx().cumulative_pass_nr();
            let open_id = more.response.id.with("preset-options-open");
            let mut open = ui.data(|data| data.get_temp::<(u64, bool)>(open_id))
                .filter(|(last, _)| *last == pass || last.saturating_add(1) == pass)
                .is_some_and(|(_, open)| open);
            // Use the compact SDK button's activation, including keyboard and long release.
            // Manual avoids a second toggle from the popup's external pointer-press trigger.
            if more.clicked() {
                open = !open;
            }
            // Escape closes it from the button or a row even when the menu leaves it untaken (`opener_menu_escape`).
            let more_id = more.response.id;
            let layer_id = open_id.with("layer");
            let menu_layer = ui.data(|d| d.get_temp::<egui::LayerId>(layer_id));
            Menu::new(crate::i18n::tr("More preset options").to_owned())
                .id_salt("preset-options-menu")
                .opened_by(&more.response, craft::ui::contract::Trigger::Manual)
                .open(Bind::bound(&mut open))
                .show(ui, |ui, _| {
                    // The SDK measures popup bodies separately; the outer panel guard is not enough.
                    let measuring = ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx());
                    // the rows' layer, so a later Escape on a row is known to be this menu's
                    if !measuring {
                        ui.data_mut(|d| d.insert_temp(layer_id, ui.layer_id()));
                    }
                    let thumbs = MenuItem::new(crate::i18n::tr("Show Thumbnails"))
                        .id_salt("preset-options-thumbnails")
                        .check(app.ui.preset_thumbs)
                        .keep_open(true)
                        .show(ui);
                    if !measuring {
                        if crate::widgets::on_live_layer(&thumbs.response) {
                            crate::widgets::register_response("presetMenu:thumbnails", thumbs.response.rect, &thumbs.response);
                        }
                        if thumbs.ran {
                            app.ui.preset_thumbs = !app.ui.preset_thumbs;
                        }
                    }
                    Divider::horizontal().show(ui);
                    let import = MenuItem::new(crate::i18n::tr("Import Presets…")).id_salt("preset-options-import").show(ui);
                    if !measuring {
                        if crate::widgets::on_live_layer(&import.response) {
                            crate::widgets::register_response("presetMenu:import", import.response.rect, &import.response);
                        }
                        if import.ran {
                            let _ = app.run("file.importPresets", json!({}));
                        }
                    }
                    let any_user = app.session.presets.iter().any(|p| !p.builtin);
                    let export = MenuItem::new(crate::i18n::tr("Export User Presets…"))
                        .id_salt("preset-options-export")
                        .enabled(any_user)
                        .show(ui);
                    if !measuring {
                        if crate::widgets::on_live_layer(&export.response) {
                            crate::widgets::register_response("presetMenu:export", export.response.rect, &export.response);
                        }
                        if export.ran {
                            let _ = app.run("file.exportPresets", json!({}));
                        }
                    }
                });
            // after the menu, which takes the Escape it handles itself: one left untaken closes it (next pass)
            if open && crate::shortcuts::opener_menu_escape(ui.ctx(), more_id, menu_layer) {
                open = false;
            }
            ui.data_mut(|data| {
                if open {
                    data.insert_temp(open_id, (pass, true));
                } else {
                    data.remove::<(u64, bool)>(open_id);
                }
            });
        }
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
                && !measuring
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
        } else if !measuring && last.is_some_and(|last| last.is_stale(&app.session)) {
            // kept across a switch to another photo and back (the slider returns with it); gone once
            // a later edit, undo or redo owns the step
            ui.data_mut(|d| d.remove::<AppliedPreset>(amt_id));
        }
        let active = app.session.active();
        let marked = AppliedByPhoto::of(ui.ctx(), &app.session, active);
        let thumbs = app.ui.preset_thumbs && active.is_some();
        let row_h = if thumbs { 54.0 } else { 26.0 };
        let groups = grouped(ui.ctx(), &app.session.presets);
        let scroll = egui::ScrollArea::vertical().id_salt("presets-scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            // Only the rows in view are laid out, drawn and given thumbnails: a Lightroom collection
            // imported whole is 10 000+ presets, and building every row each frame took seconds.
            let visible = ui.clip_rect().expand(row_h);
            for grp in groups.iter() {
                let (g, builtin, favorites) = (grp.name.clone(), grp.builtin, grp.favorites);
                // the Favorites section opens and closes apart from a user group named "Favorites"
                let open_id = egui::Id::unique(("preset-group", favorites, &g));
                let open: bool = ui.data(|d| d.get_temp(open_id)).unwrap_or(true);
                let (_, r) = ui.allocate_space(vec2(ui.available_width(), 32.0));
                let shown = sdk::group_row(ui, r, favorites, &g, crate::i18n::builtin_label(&g, builtin).as_ref(), open);
                let resp = shown.response;
                // outside the `presetGroup:` ids, which a user group of any name can take
                if !measuring {
                    crate::widgets::register_response(if favorites { "presetFavorites".to_string() } else { format!("presetGroup:{g}") }, r, &resp);
                }
                if !measuring {
                    if let Some(change) = shown.open {
                        ui.data_mut(|d| d.insert_temp(open_id, change.open));
                    } else if craft::ui::contract::clicked(&resp) {
                        ui.data_mut(|d| d.insert_temp(open_id, !open));
                    }
                }
                if !favorites && !measuring {
                    sdk::context_menu(ui, &resp, &g, |ui| {
                        if MenuItem::new(crate::i18n::tr("Export Group…")).show(ui).ran {
                            let _ = app.run("file.exportPresets", json!({"group": g}));
                        }
                    });
                }
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
                    let (_, r) = ui.allocate_space(vec2(ui.available_width(), row_h));
                    let is_last = marked.as_deref() == Some(pid.as_str());
                    // The thumbnail and hover use the engine composition, never a UI-only approximation.
                    let mut look = if thumbs && !measuring { active.and_then(|id| app.session.preset_applied(id, &pr, 1.0)) } else { None };
                    let texture = if thumbs && !measuring {
                        active.zip(look.as_ref()).and_then(|(id, settings)| {
                            let job = app.session.variant_job(id, settings, THUMB_EDGE)?;
                            app.renderer.variant(job).map(|texture| (texture.id(), texture.size))
                        })
                    } else {
                        None
                    };
                    let resp =
                        sdk::PresetRow { favorites, preset: &pid, label: &name, selected: is_last, favorite: fav, thumbnails: thumbs, texture }
                            .show_at(ui, r);
                    // Keep the actual SDK hit owner and egui's exact clipped interaction rect.
                    if !measuring {
                        crate::widgets::register_response(
                            if favorites { format!("presetFavorite:{pid}") } else { format!("preset:{pid}") },
                            r,
                            &resp,
                        );
                        if is_last {
                            crate::widgets::register_response("presetApplied", r, &resp);
                        }
                    }
                    if resp.hovered() && !measuring && look.is_none() {
                        look = active.and_then(|id| app.session.preset_applied(id, &pr, 1.0));
                    }
                    // resting on a preset previews it in the loupe (no history entry)
                    if !measuring
                        && resp.hovered()
                        && let Some(s) = look
                    {
                        let label = crate::i18n::tr_format!("Preset: {name}", name = name);
                        app.hover_preview = Some(HoverPreview { label: label.clone(), settings: s });
                        if let Some(id) = active {
                            ui.data_mut(|d| d.insert_temp(egui::Id::unique("last-preset-hover"), (id, pr.clone(), label)));
                        }
                    }
                    // the release that ends carrying a keyword or photos is their drop, not a pick of the preset under it
                    let carrying = app.ui.dragging_keyword.is_some() || app.ui.dragging_photos.is_some() || crate::widgets::carrying(ui.ctx());
                    if !measuring
                        && !carrying
                        && craft::ui::contract::clicked(&resp)
                        && app.run("preset.apply", json!({"id": pid, "amount": 100})).is_ok()
                    {
                        AppliedByPhoto::remember(ui.ctx(), &app.session, &pid);
                        ui.data_mut(|d| d.insert_temp(amt_id, AppliedPreset::new(&app.session, pid.clone(), 100.0)));
                        ui.data_mut(|d| d.remove::<(lightcraft_catalog::PhotoId, Preset, String)>(egui::Id::unique("last-preset-hover")));
                        app.toast(ui.ctx(), crate::i18n::tr_format!("Preset: {name}", name = name));
                    }
                    let (builtin, group) = (pr.builtin, pr.group.clone());
                    if !measuring {
                        sdk::context_menu(ui, &resp, &name, |ui| {
                            if MenuItem::new(crate::i18n::tr(if fav { "Remove from Favorites" } else { "Add to Favorites" })).show(ui).ran {
                                let _ = app.run("preset.favorite", json!({"id": pid}));
                            }
                            if builtin {
                                return;
                            }
                            Divider::horizontal().show(ui);
                            if MenuItem::new(crate::i18n::tr("Update with Current Settings")).enabled(app.session.active().is_some()).show(ui).ran {
                                match app.run("preset.update", json!({"id": pid})) {
                                    Ok(_) => app.toast(ui.ctx(), crate::i18n::tr_format!("Updated “{name}”", name = name)),
                                    Err(e) => app.toast(ui.ctx(), e),
                                }
                            }
                            if MenuItem::new(crate::i18n::tr("Rename…")).show(ui).ran {
                                crate::panels::dialogs::prompt(
                                    app,
                                    "Rename Preset",
                                    "Preset name",
                                    &name,
                                    "preset.rename",
                                    json!({"id": pid}),
                                    "name",
                                );
                            }
                            Menu::submenu_row(ui, crate::i18n::tr("Move to Group"), |ui, _| {
                                for g in groups.iter().filter(|x| x.user && x.name != group).map(|x| &x.name) {
                                    if MenuItem::new(g.clone()).show(ui).ran {
                                        let _ = app.run("preset.move", json!({"id": pid, "group": g}));
                                    }
                                }
                                if MenuItem::new(crate::i18n::tr("New Group…")).show(ui).ran {
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
                            Divider::horizontal().show(ui);
                            if MenuItem::new(crate::i18n::tr("Delete Preset")).show(ui).ran {
                                let _ = app.run("preset.delete", json!({"id": pid}));
                            }
                        });
                    }
                }
                if end < grp.items.len() {
                    ui.allocate_space(vec2(ui.available_width(), (grp.items.len() - end) as f32 * row_h));
                }
            }
            ui.add_space(30.0);
        });
        let last_hover_id = egui::Id::unique("last-preset-hover");
        let pointer_in_scroll = ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|pos| scroll.inner_rect.contains(pos));
        if !measuring && pointer_in_scroll {
            if app.hover_preview.is_none()
                && let Some(id) = active
                && let Some((saved_id, saved_preset, saved_label)) =
                    ui.data(|d| d.get_temp::<(lightcraft_catalog::PhotoId, Preset, String)>(last_hover_id))
                && saved_id == id
                && let Some(s) = app.session.preset_applied(id, &saved_preset, 1.0)
            {
                app.hover_preview = Some(HoverPreview { label: saved_label, settings: s });
            }
        } else if !measuring {
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
    /// The Favorites section on top (issue #1999): the starred presets, each also in its own group.
    favorites: bool,
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
            items: Vec::new(),
        });
        g.builtin |= p.builtin;
        g.user |= !p.builtin;
        g.items.push(i);
    }
    // starred presets first, in their own section, as Profiles does
    let starred: Vec<usize> = presets.iter().enumerate().filter(|(_, p)| p.favorite).map(|(i, _)| i).collect();
    let favorites = (!starred.is_empty()).then(|| Group { name: "Favorites".into(), builtin: true, user: false, favorites: true, items: starred });
    let groups = Arc::new(favorites.into_iter().chain(by_name.into_values()).collect::<Vec<_>>());
    ctx.data_mut(|d| d.insert_temp(id, (presets.generation(), groups.clone())));
    groups
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

    use egui_kittest::{Harness, kittest::Queryable};

    fn panel_harness(height: f32) -> Harness<'static, LightcraftApp> {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.ui.preset_thumbs = false;
        // The production host installs the app's semibold/font families before its first pass.
        // Installing the SDK alone is not a replacement for LightCraft's font setup.
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut h = Harness::builder().with_context(ctx).with_size(vec2(320.0, height)).build_ui_state(
            |ui, app: &mut LightcraftApp| {
                crate::widgets::take_registries(ui.ctx());
                app.hover_preview = None;
                let measuring = ui.data(|d| d.get_temp::<bool>(egui::Id::unique("preset-test-measure"))).unwrap_or(false);
                if measuring {
                    craft::ui::runtime::measure::run(&ui.ctx().clone(), || super::show(app, ui));
                } else {
                    super::show(app, ui);
                }
                (app.widgets, app.widget_hits) = crate::widgets::take_registries(ui.ctx());
            },
            app,
        );
        h.run_steps(5);
        h
    }

    fn first_row(h: &Harness<'_, LightcraftApp>) -> String {
        h.state()
            .widgets
            .iter()
            .find(|(id, rect)| id.starts_with("preset:") && rect.top() > 60.0 && rect.bottom() < 500.0)
            .map(|(id, _)| id.clone())
            .expect("visible real preset row")
    }

    fn pointer(h: &mut Harness<'_, LightcraftApp>, widget: &str, button: egui::PointerButton) {
        let rect = h.state().widgets.iter().find(|(id, _)| id == widget).unwrap().1;
        let pos = rect.center();
        h.event(egui::Event::PointerMoved(pos));
        h.run_steps(2);
        h.event(egui::Event::PointerButton { pos, button, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos, button, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
    }

    #[test]
    fn sdk_preset_pointer_applies_one_command_and_undo_restores_photo() {
        let mut h = panel_harness(600.0);
        let photo = h.state().session.active().unwrap();
        let before = h.state().session.develop_of(photo).unwrap();
        let row = first_row(&h);
        let preset = h.state().session.presets.iter().find(|p| row == format!("preset:{}", p.id)).unwrap().clone();
        let expected = h.state().session.preset_applied(photo, &preset, 1.0).unwrap();
        let commits = h.state().session.commits();
        pointer(&mut h, &row, egui::PointerButton::Primary);
        assert_eq!(h.state().session.commits(), commits + 1);
        assert_eq!(*h.state().session.develop_of(photo).unwrap(), expected);
        assert!(h.state().widgets.iter().any(|(id, _)| id == "presetApplied"));
        h.state_mut().run("edit.undo", json!({})).unwrap();
        h.run_steps(3);
        assert_eq!(h.state().session.develop_of(photo).unwrap(), before);
        assert!(!h.state().widgets.iter().any(|(id, _)| id == "presetApplied"));
    }

    #[test]
    fn sdk_preset_keyboard_activation_is_one_apply_not_an_egui_double_dispatch() {
        for key in [egui::Key::Enter, egui::Key::Space] {
            let mut h = panel_harness(600.0);
            let row = first_row(&h);
            let hit = h.state().widget_hits.iter().find(|(id, _)| id == &row).unwrap().1;
            h.ctx.memory_mut(|memory| memory.request_focus(hit.id));
            h.run_steps(2);
            let commits = h.state().session.commits();
            h.key_press(key);
            h.run_steps(3);
            assert_eq!(h.state().session.commits(), commits + 1, "{key:?}");
        }
    }

    #[test]
    fn sdk_preset_right_click_favorites_and_escape_do_not_apply() {
        let mut h = panel_harness(600.0);
        let row = first_row(&h);
        let commits = h.state().session.commits();
        pointer(&mut h, &row, egui::PointerButton::Secondary);
        assert_eq!(h.state().session.commits(), commits);
        h.get_by_label("Add to Favorites").click();
        h.run_steps(3);
        let pid = row.strip_prefix("preset:").unwrap();
        assert!(h.state().session.presets.iter().find(|p| p.id == pid).unwrap().favorite);
        assert!(h.state().session.undo.is_empty(), "favorite does not develop a photo");
        pointer(&mut h, &row, egui::PointerButton::Secondary);
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert!(h.query_by_label("Remove from Favorites").is_none());
        assert!(h.state().session.undo.is_empty());
    }

    #[test]
    fn sdk_preset_no_selection_refuses_apply_but_preserves_context_menu() {
        let mut h = panel_harness(600.0);
        h.state_mut().session.selection = Default::default();
        h.run_steps(3);
        let row = first_row(&h);
        let commits = h.state().session.commits();
        pointer(&mut h, &row, egui::PointerButton::Primary);
        assert_eq!(h.state().session.commits(), commits);
        assert!(!h.state().widgets.iter().any(|(id, _)| id == "presetApplied"));
        pointer(&mut h, &row, egui::PointerButton::Secondary);
        assert!(h.query_by_label("Add to Favorites").is_some());
    }

    #[test]
    fn sdk_preset_active_outside_selection_is_not_edited_or_marked() {
        let mut h = panel_harness(600.0);
        let active = h.state().session.active().unwrap();
        let selected = h.state_mut().session.visible().iter().copied().find(|id| *id != active).unwrap();
        h.state_mut().session.selection.ids = vec![selected];
        let before = h.state().session.develop_of(active).unwrap();
        let row = first_row(&h);
        pointer(&mut h, &row, egui::PointerButton::Primary);
        assert_eq!(h.state().session.develop_of(active).unwrap(), before);
        assert!(!h.state().widgets.iter().any(|(id, _)| id == "presetApplied"));
        h.state_mut().session.selection.active = Some(selected);
        h.run_steps(3);
        assert!(h.state().widgets.iter().any(|(id, _)| id == "presetApplied"));
    }

    #[test]
    fn sdk_preset_hover_cancels_on_exit_without_undo_or_photo_edit() {
        let mut h = panel_harness(600.0);
        let active = h.state().session.active().unwrap();
        let before = h.state().session.develop_of(active).unwrap();
        let row = first_row(&h);
        let pos = h.state().widgets.iter().find(|(id, _)| id == &row).unwrap().1.center();
        h.event(egui::Event::PointerMoved(pos));
        h.run_steps(3);
        assert!(h.state().hover_preview.is_some());
        assert!(h.state().session.undo.is_empty());
        h.event(egui::Event::PointerGone);
        h.run_steps(3);
        assert!(h.state().hover_preview.is_none());
        assert_eq!(h.state().session.develop_of(active).unwrap(), before);
    }

    #[test]
    fn sdk_preset_measure_does_not_apply_preview_or_publish_row_targets() {
        let mut h = panel_harness(600.0);
        let row = first_row(&h);
        let pos = h.state().widgets.iter().find(|(id, _)| id == &row).unwrap().1.center();
        let commits = h.state().session.commits();
        h.ctx.data_mut(|data| data.insert_temp(egui::Id::unique("preset-test-measure"), true));
        h.event(egui::Event::PointerMoved(pos));
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
        assert_eq!(h.state().session.commits(), commits);
        assert!(h.state().hover_preview.is_none());
        assert!(!h.state().widgets.iter().any(|(id, _)| id.starts_with("preset:")));
        h.ctx.data_mut(|data| data.insert_temp(egui::Id::unique("preset-test-measure"), false));
        h.run_steps(3);
        pointer(&mut h, &row, egui::PointerButton::Primary);
        assert_eq!(h.state().session.commits(), commits + 1, "the real pass still owns the command");
    }

    #[test]
    fn sdk_preset_large_collection_keeps_virtualization_and_exact_clipping() {
        let mut h = panel_harness(240.0);
        h.state_mut().session.add_presets(
            (0..5000)
                .map(|i| Preset {
                    id: format!("user.sdk.{i}"),
                    name: format!("Preset {i}"),
                    group: "AAA scale".into(),
                    settings: json!({"light": {"exposure": 0.5}}),
                    favorite: false,
                    builtin: false,
                })
                .collect(),
        );
        h.run_steps(4);
        let rows: Vec<_> = h.state().widget_hits.iter().filter(|(id, _)| id.starts_with("preset:")).collect();
        assert!((2..20).contains(&rows.len()), "only visible rows plus one-row overscan: {}", rows.len());
        assert!(rows.iter().all(|(_, hit)| hit.interact_rect.top() >= 0.0 && hit.interact_rect.bottom() <= 240.0));
        assert!(rows.iter().any(|(id, _)| id.starts_with("preset:user.sdk.")));
    }

    #[test]
    fn sdk_preset_tab_reaches_named_row_then_enter_applies() {
        let mut h = panel_harness(600.0);
        let row = first_row(&h);
        let hit = h.state().widget_hits.iter().find(|(id, _)| id == &row).unwrap().1;
        let preset = h.state().session.presets.iter().find(|p| row == format!("preset:{}", p.id)).unwrap();
        let name = crate::i18n::builtin_label(&preset.name, preset.builtin).to_string();
        // Preset rows sit under their group's row, so they are tree items.
        assert!(h.query_by_role_and_label(egui::accesskit::Role::TreeItem, &name).is_some());
        for _ in 0..100 {
            if h.ctx.memory(|memory| memory.focused()) == Some(hit.id) {
                break;
            }
            h.key_press(egui::Key::Tab);
            h.run_steps(1);
        }
        assert_eq!(h.ctx.memory(|memory| memory.focused()), Some(hit.id), "a real Tab stop, not just a painted label");
        let commits = h.state().session.commits();
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert_eq!(h.state().session.commits(), commits + 1);
    }

    #[test]
    fn sdk_preset_amount_replaces_only_its_preset_undo_step() {
        let mut h = panel_harness(600.0);
        let photo = h.state().session.active().unwrap();
        let before = h.state().session.develop_of(photo).unwrap();
        let row = first_row(&h);
        pointer(&mut h, &row, egui::PointerButton::Primary);
        let rect = h.state().widgets.iter().find(|(id, _)| id == "slider:presetAmount").unwrap().1;
        let pos = egui::pos2(egui::lerp(rect.x_range(), 0.25), rect.center().y);
        h.event(egui::Event::PointerMoved(pos));
        h.run_steps(2);
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
        let applied: AppliedPreset = h.ctx.data(|data| data.get_temp(egui::Id::unique("preset-amount"))).unwrap();
        assert!(applied.amount < 100.0, "the actual Amount control changed the amount");
        assert_eq!(h.state().session.undo.len(), 1, "one undo still removes the whole preset");
        h.state_mut().run("edit.undo", json!({})).unwrap();
        h.run_steps(3);
        assert_eq!(h.state().session.develop_of(photo).unwrap(), before);
        // An unrelated edit invalidates the slider instead of granting it that edit's undo step.
        pointer(&mut h, &row, egui::PointerButton::Primary);
        h.state_mut().run("develop.set", json!({"values": {"light.exposure": 0.75}})).unwrap();
        h.run_steps(3);
        assert!(!h.state().widgets.iter().any(|(id, _)| id == "slider:presetAmount"));
        assert_eq!(h.state().session.develop_of(photo).unwrap().light.exposure, 0.75);
    }

    #[test]
    fn sdk_preset_carrying_blocks_context_menu() {
        let mut h = panel_harness(600.0);
        let row = first_row(&h);
        crate::widgets::set_carrying(&h.ctx, true);
        pointer(&mut h, &row, egui::PointerButton::Secondary);
        assert!(h.query_by_label("Add to Favorites").is_none());
        assert!(h.state().session.undo.is_empty());
        crate::widgets::set_carrying(&h.ctx, false);
        pointer(&mut h, &row, egui::PointerButton::Secondary);
        assert!(h.query_by_label("Add to Favorites").is_some());
    }

    #[test]
    fn sdk_preset_repeated_right_click_moves_menu_without_one_frame_flash() {
        let mut h = panel_harness(600.0);
        let row = first_row(&h);
        pointer(&mut h, &row, egui::PointerButton::Secondary);
        let old = h.get_by_label("Add to Favorites").rect();
        let rect = h.state().widgets.iter().find(|(id, _)| id == &row).unwrap().1;
        let pos = egui::pos2(rect.left() + 8.0, rect.center().y);
        h.event(egui::Event::PointerMoved(pos));
        h.run_steps(2);
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
        let moved = h.get_by_label("Add to Favorites").rect();
        assert_ne!(moved.min, old.min, "menu relocates to the new right-click");
        assert!(h.state().session.undo.is_empty());
    }

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
