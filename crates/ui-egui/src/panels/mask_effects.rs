//! Mask effect presets in the UI: the Effect menu above a mask's sliders (grouped, searchable,
//! keyboard-driven, previews the effect on the photo while the pointer rests on it), the
//! New Effect Preset dialog (which sliders, Amount, brush settings) and the Manage Effect Presets
//! window (search, rename, update, duplicate, stars, groups, delete). Every change runs a
//! `mask.*Effect` command.

use craft::ui::contract::{Props as _, Show as _, ShowContainer as _};
use craft::ui::display::Divider;
use craft::ui::menu::{Menu, MenuItem};
use egui::{Sense, vec2};
use lightcraft_develop::Mask;
use lightcraft_engine::cmd::mask_effects::{self as fx, FIELDS, MaskEffect};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::Dialog;
use crate::theme::Tokens;
use crate::widgets::register_response;

use super::masking::controls::{Fill, Hit, measuring};

/// A slider's label in the effect lists (the colour tint's hue and saturation are one "Color").
pub fn field_label(k: &str) -> &'static str {
    match k {
        "color_hue" | "color_sat" => "Color",
        _ => super::masking::LOCAL.iter().find(|s| s.id == k).map_or("", |s| s.label),
    }
}

/// The choices of the New Effect Preset checklist: each slider once (Color = hue + saturation).
fn checklist() -> Vec<&'static str> {
    FIELDS.iter().copied().filter(|k| *k != "color_hue").collect()
}

/// "Exposure +0.50 · Clarity −20" — what an effect sets, for the lists.
pub fn summary(e: &MaskEffect) -> String {
    let fields = e.field_list();
    let mut parts: Vec<String> = fx::changed_fields(&e.adjust)
        .iter()
        .filter(|k| fields.contains(&k.as_str()) && k.as_str() != "color_hue")
        .map(|k| {
            let v = fx::get(&e.adjust, k);
            match k.as_str() {
                "exposure" => format!("{} {:+.2}", crate::i18n::tr(field_label(k)), v),
                "color_sat" => format!("{} {:.0}°/{:.0}", crate::i18n::tr("Color"), e.adjust.color_hue, v),
                _ => format!("{} {:+.0}", crate::i18n::tr(field_label(k)), v),
            }
        })
        .collect();
    if e.with_amount {
        parts.push(format!("{} {:.0}", crate::i18n::tr("Amount"), e.adjust.amount));
    }
    if parts.is_empty() {
        parts.push(crate::i18n::tr("No change").to_string());
    }
    parts.join(" · ").replace('-', "−")
}

/// An effect's name as shown: runs of spaces collapsed (Lightroom users pad names to line up
/// parts of them in its menu; the stored name keeps them).
pub fn display_name(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The effect the selected mask's tool last applied (what Update / Rename in the menu act on).
fn last_applied(app: &LightcraftApp, mask: &Mask) -> Option<String> {
    let tool = fx::mask_tool(mask)?;
    let name = app.session.mask_tool_effects.get(&tool)?;
    app.session.mask_effects.iter().find(|e| e.name.eq_ignore_ascii_case(name)).map(|e| e.name.clone())
}

/// Apply an effect to `mask` (and take the brush settings it carries).
pub fn apply(app: &mut LightcraftApp, ctx: &egui::Context, name: &str, params: Value) {
    let mut p = params;
    p["name"] = json!(name);
    match app.run("mask.applyEffect", p) {
        Ok(v) => take_brush(app, &v["brush"]),
        Err(e) => app.toast_error(ctx, e),
    }
}

fn take_brush(app: &mut LightcraftApp, b: &Value) {
    let Some(b) = b.as_object() else { return };
    let f = |k: &str| b.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
    if let Some(v) = f("size") {
        app.ui.brush_size = (v as f32).clamp(0.002, 0.5);
    }
    if let Some(v) = f("feather") {
        app.ui.brush_feather = (v as f32).clamp(0.0, 100.0);
    }
    if let Some(v) = f("flow") {
        app.ui.brush_flow = (v as f32).clamp(1.0, 100.0);
    }
    if let Some(v) = b.get("auto_mask").and_then(Value::as_bool) {
        app.ui.brush_auto_mask = v;
    }
}

/// The brush settings in use, as an effect carries them.
fn brush_now(app: &LightcraftApp) -> Value {
    json!({"size": app.ui.brush_size, "feather": app.ui.brush_feather, "flow": app.ui.brush_flow, "auto_mask": app.ui.brush_auto_mask})
}

/// Open the New Effect Preset dialog for the selected mask.
pub fn open_save(app: &mut LightcraftApp) {
    let brush = app.ui.tool == "brush";
    app.ui.dialog = Some(Dialog::SaveEffect {
        name: String::new(),
        group: String::new(),
        fields: checklist().iter().map(|k| k.to_string()).collect(),
        amount: false,
        brush,
    });
}

/// Open the Manage Effect Presets window.
pub fn open_manage(app: &mut LightcraftApp) {
    app.ui.dialog =
        Some(Dialog::ManageEffects { search: String::new(), selected: Vec::new(), rename: None, confirm_delete: false, new_group: String::new() });
}

/// The menu's search text and keyboard highlight.
#[derive(Clone, Default)]
struct MenuState {
    search: String,
    highlight: Option<usize>,
    focused: bool,
}

fn state_id() -> egui::Id {
    egui::Id::unique("maskEffectMenuState")
}

/// A framework button of the Effect menu and the manager: what it reports is whether it was
/// clicked (the pointer or the keyboard) and its response, which the popups hang from. Measuring
/// publishes no automation name and acts on nothing.
struct Action {
    clicked: bool,
    response: egui::Response,
}

fn action(ui: &mut egui::Ui, widget: &str, text: &str, enabled: bool, flat: bool) -> Action {
    use craft::ui::buttons::Button;
    let enabled = enabled && ui.is_enabled();
    let mut button = Button::new(crate::i18n::tr(text).to_owned()).id(egui::Id::unique(widget)).enabled(enabled);
    if flat {
        button = button.variant(craft::theme::Variant::GHOST);
    }
    let shown = button.show(ui);
    if !measuring(ui) {
        register_response(widget, shown.response.rect, &shown.response);
    }
    Action { clicked: enabled && !measuring(ui) && shown.clicked(), response: shown.response }
}

/// A framework popover that `opener` toggles when `clicked` (the pointer, a long press or the keyboard
/// alike) and Escape or a click away closes. `body` returns whether to close it, after a choice.
/// The open state is kept under `key` between passes, and a popover that was not drawn on the
/// pass before comes back closed.
fn popover(ui: &mut egui::Ui, key: egui::Id, label: &str, opener: &egui::Response, clicked: bool, mut body: impl FnMut(&mut egui::Ui) -> bool) {
    use craft::ui::contract::Bind;
    use craft::ui::popover::Popover;
    use craft::ui::runtime::focus::Initial;

    let stamp = key.with("shown-pass");
    let pass = ui.ctx().cumulative_pass_nr();
    let real = !measuring(ui);
    let (kept, previous) = ui.data(|data| (data.get_temp::<bool>(key).unwrap_or(false), data.get_temp::<u64>(stamp)));
    let mut open = kept && previous.is_some_and(|last| last == pass || last.checked_add(1) == Some(pass));
    if real && clicked {
        open = !open;
    }
    let mut close = false;
    Popover::new(crate::i18n::tr(label).to_owned(), Bind::bound(&mut open))
        .id_salt(key)
        .opened_by(opener)
        .initial_focus(Initial::Keep)
        .overlay(|mut overlay| {
            // The search field and the rows take the focus in turn: Tab crosses the popover into its parent.
            overlay.focus = overlay.focus.trap(false);
            overlay.dismiss = overlay.dismiss.focus_left(false);
            overlay
        })
        .show(ui, |ui, _| close |= body(ui));
    if real {
        if close {
            open = false;
        }
        ui.data_mut(|data| {
            data.insert_temp(key, open);
            data.insert_temp(stamp, pass);
        });
    }
}

/// The effects the menu lists for `query`: (section heading, effect name, its row widget id).
fn menu_rows(effects: &[MaskEffect], query: &str) -> Vec<(String, String, String)> {
    let q = query.trim().to_lowercase();
    let hit = |e: &MaskEffect| q.is_empty() || e.name.to_lowercase().contains(&q) || e.group.to_lowercase().contains(&q);
    let mut rows = Vec::new();
    if q.is_empty() {
        let mut favs: Vec<&MaskEffect> = effects.iter().filter(|e| e.favorite).collect();
        favs.sort_by(|a, b| fx::natural_cmp(&a.name, &b.name));
        rows.extend(favs.into_iter().map(|e| ("★ Favorites".to_string(), e.name.clone(), format!("maskEffectFav:{}", e.name))));
    }
    for (group, ix) in fx::menu_order(effects) {
        for e in ix.iter().filter_map(|i| effects.get(*i)).filter(|e| hit(e)) {
            rows.push((group.clone(), e.name.clone(), format!("maskEffect:{}", e.name)));
        }
    }
    rows
}

/// The Effect menu above the mask's sliders.
pub fn effect_row(app: &mut LightcraftApp, ui: &mut egui::Ui, mask: &Mask) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr("Effect")).font(t.font(12.0)).color(t.text_label)).selectable(false));
            let current = fx::matching_effect(&app.session, &mask.adjust);
            let label = current.as_deref().map(display_name).unwrap_or_else(|| crate::i18n::tr("Custom").to_string());
            let r = crate::widgets::dropdown(ui, "maskEffect", &label, t.font(12.0), t.text);
            let clicked = r.clicked() && !measuring(ui);
            popover(ui, egui::Id::unique(("mask-effect-menu", mask.id)), "Effect", &r, clicked, |ui| {
                if clicked {
                    // each opening starts with an empty search
                    ui.data_mut(|d| d.insert_temp(state_id(), MenuState::default()));
                }
                menu(app, ui, mask, current.as_deref())
            });
        });
    });
}

/// The menu's body; whether a choice was made and the menu is to close.
fn menu(app: &mut LightcraftApp, ui: &mut egui::Ui, mask: &Mask, current: Option<&str>) -> bool {
    let t = Tokens::get(ui.ctx());
    ui.set_min_width(260.0);
    ui.set_max_width(320.0);
    let mut st: MenuState = ui.data(|d| d.get_temp(state_id())).unwrap_or_default();
    let before = st.search.clone();
    let r = crate::text_field::TextField::singleline("maskEffectSearch", &mut st.search)
        .hint(crate::i18n::tr("Search effects"))
        .width(f32::INFINITY)
        .show(ui);
    if !st.focused {
        crate::text_field::focus_id(ui.ctx(), r.response.id);
        st.focused = true;
    }
    if st.search != before {
        st.highlight = None;
    }
    let rows = menu_rows(&app.session.mask_effects, &st.search);
    // ↑ ↓ move through the list, Return applies
    let (down, up, enter) = ui.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.key_pressed(egui::Key::Enter) && !rows.is_empty(),
        )
    });
    let moved = down || up;
    if !rows.is_empty() {
        let last = rows.len() - 1;
        st.highlight = match (st.highlight, down, up) {
            (None, true, _) => Some(0),
            (None, _, true) => Some(last),
            (Some(h), true, _) => Some((h + 1).min(last)),
            (Some(h), _, true) => Some(h.saturating_sub(1)),
            (h, _, _) => h.filter(|h| *h <= last),
        };
    }
    let mut chosen: Option<String> = None;
    if enter {
        chosen = rows.get(st.highlight.unwrap_or(0)).map(|(_, n, _)| n.clone());
    }
    let mut hovered: Option<String> = None;
    if app.session.mask_effects.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::tr("No effect presets yet")).color(t.text_dim));
    } else if rows.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::tr("No effect matches")).color(t.text_dim));
    }
    egui::ScrollArea::vertical().id_salt("maskEffectList").max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
        let mut section: Option<&str> = None;
        for (i, (group, name, widget)) in rows.iter().enumerate() {
            if section != Some(group.as_str()) {
                section = Some(group.as_str());
                if !group.is_empty() {
                    ui.add_space(4.0);
                    let heading = if group == "★ Favorites" { crate::i18n::tr("★ Favorites").to_string() } else { group.clone() };
                    ui.label(egui::RichText::new(heading).font(t.semibold(11.5)).color(t.text_dim));
                }
            }
            let fav = app.session.mask_effects.iter().any(|e| e.favorite && &e.name == name);
            let (_, rect) = ui.allocate_space(vec2(ui.available_width(), 22.0));
            if st.highlight == Some(i) {
                ui.painter().rect_filled(rect, 3.0, t.hover);
            }
            // (rows scrolled out of view can't be clicked)
            let hit = ui
                .is_rect_visible(rect)
                .then(|| Hit::new(widget, display_name(name)).fill(Fill::on_hover(t.hover, 3.0)).show_at(ui, rect));
            if current == Some(name.as_str()) {
                ui.painter().text(rect.left_center() + vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, "✓", t.font(12.0), t.accent);
            }
            let room = rect.width() - 22.0 - if fav { 16.0 } else { 0.0 };
            let g = crate::widgets::one_line(ui.painter(), &display_name(name), t.font(12.5), t.text, room);
            let elided = g.elided;
            ui.painter().galley(rect.left_center() + vec2(18.0, -g.size().y / 2.0), g, t.text);
            if fav {
                crate::rating::paint_star(
                    ui.painter(),
                    egui::Rect::from_center_size(rect.right_center() - vec2(10.0, 0.0), vec2(11.0, 11.0)),
                    true,
                    t.text_dim,
                );
            }
            if moved && st.highlight == Some(i) {
                ui.scroll_to_rect(rect, None);
            }
            let Some(hit) = hit else { continue };
            let hit = if elided { hit.on_hover_text(display_name(name)) } else { hit };
            if hit.hovered() || (st.highlight == Some(i) && moved) {
                hovered = Some(name.clone());
            }
            if hit.clicked() {
                chosen = Some(name.clone());
            }
            if !measuring(ui) && !crate::widgets::carrying(ui.ctx()) {
                Menu::context(display_name(name), &hit.response)
                    .id_salt(("mask-effect-row-menu", widget))
                    .show(ui, |ui, _| row_menu(app, ui, mask, name, fav));
            }
        }
    });
    // resting on an effect previews it on the photo (no history entry)
    if let Some(name) = hovered.or_else(|| st.highlight.and_then(|h| rows.get(h)).map(|(_, n, _)| n.clone())) {
        preview(app, mask, &name);
    }
    ui.data_mut(|d| d.insert_temp(state_id(), st));
    if let Some(name) = chosen {
        apply(app, ui.ctx(), &name, json!({"id": mask.id}));
        return true;
    }
    Divider::horizontal().show(ui);
    let mut close = false;
    if action(ui, "maskEffectMenu:reset", "Reset Sliders", true, true).clicked {
        let zero: serde_json::Map<String, Value> = FIELDS.iter().map(|k| (k.to_string(), json!(0.0))).collect();
        let _ = app.run("mask.adjust", json!({"id": mask.id, "values": zero}));
        if let Some(tool) = fx::mask_tool(mask) {
            let _ = app.run("mask.toolEffect", json!({"tool": tool}));
        }
        close = true;
    }
    if action(ui, "maskEffectMenu:save", "Save Current Settings as New Preset…", true, true).clicked {
        open_save(app);
        close = true;
    }
    if let Some(last) = last_applied(app, mask) {
        let text = crate::i18n::tr_format!("Update “{name}” with Current Settings", name = last);
        // (the sentence names the preset, so it is not looked up as an English string)
        if action(ui, "maskEffectMenu:update", &text, current != Some(last.as_str()), true).clicked {
            let _ = app.run("mask.updateEffect", json!({"name": last, "id": mask.id}));
            close = true;
        }
    }
    if action(ui, "maskEffectMenu:manage", "Manage Presets…", true, true).clicked {
        open_manage(app);
        close = true;
    }
    if action(ui, "maskEffectMenu:import", "Import Presets…", app.services.pick_preset_files.is_some(), true).clicked {
        let _ = app.run("file.importPresets", json!({}));
        close = true;
    }
    let any = !app.session.mask_effects.is_empty();
    if action(ui, "maskEffectMenu:export", "Export Presets…", any && app.services.save_preset_file.is_some(), true).clicked {
        let _ = app.run("file.exportMaskEffects", json!({}));
        close = true;
    }
    close
}

/// The photo with `name` applied to `mask`, shown while the pointer rests on it.
fn preview(app: &mut LightcraftApp, mask: &Mask, name: &str) {
    let Some(id) = app.session.active() else { return };
    let Some(e) = app.session.mask_effects.iter().find(|e| e.name == name) else { return };
    let Some(d) = app.session.develop_of(id) else { return };
    let mut d = (*d).clone();
    let Some(m) = d.masks.iter_mut().find(|m| m.id == mask.id) else { return };
    m.adjust = e.apply(&m.adjust);
    app.hover_preview = Some(crate::HoverPreview { label: crate::i18n::tr_format!("Effect: {name}", name = display_name(name)), settings: d });
}

/// One row of a framework menu: whether it ran this pass (a click, Enter or Space). Measuring
/// runs nothing. `key` names the row for automation as `maskEffectRow:{key}`.
fn menu_run(ui: &mut egui::Ui, scope: &str, key: &str, text: &str, enabled: bool, tip: Option<String>) -> bool {
    let enabled = enabled && ui.is_enabled();
    let mut item = MenuItem::new(text.to_owned()).id_salt(("mask-effect-menu-row", scope, key)).enabled(enabled);
    if let Some(tip) = tip {
        item = item.tooltip(tip);
    }
    let shown = item.show(ui);
    if measuring(ui) {
        return false;
    }
    register_response(format!("maskEffectRow:{key}"), shown.response.rect, &shown.response);
    enabled && shown.ran
}

/// The right-click menu of an effect in the Effect menu.
fn row_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, mask: &Mask, name: &str, fav: bool) {
    let masks = app.session.active().and_then(|id| app.session.develop_of(id)).map_or(0, |d| d.masks.len());
    if menu_run(ui, name, "Apply to All Masks", crate::i18n::tr("Apply to All Masks"), masks > 1, None) {
        apply(app, ui.ctx(), name, json!({"all": true}));
    }
    if app.session.selection.ids.len() > 1 {
        let text = crate::i18n::tr_format!("Apply to “{mask}” on Selected Photos", mask = mask.name);
        let tip = crate::i18n::tr("Also the masks of the same name on the other selected photos").to_owned();
        if menu_run(ui, name, "Apply to Selected Photos", &text, true, Some(tip)) {
            apply(app, ui.ctx(), name, json!({"id": mask.id, "photos": "selected"}));
        }
    }
    Divider::horizontal().show(ui);
    let run = |app: &mut LightcraftApp, ui: &mut egui::Ui, text: &str, cmd: &str, p: Value| {
        if menu_run(ui, name, text, crate::i18n::tr(text), true, None)
            && let Err(e) = app.run(cmd, p)
        {
            app.toast_error(ui.ctx(), e);
        }
    };
    run(app, ui, "Update with Current Settings", "mask.updateEffect", json!({"name": name, "id": mask.id}));
    if menu_run(ui, name, "Rename…", crate::i18n::tr("Rename…"), true, None) {
        crate::panels::dialogs::prompt(app, "Rename Effect Preset", "Preset name", name, "mask.renameEffect", json!({"name": name}), "to");
    }
    run(app, ui, "Duplicate", "mask.duplicateEffect", json!({"name": name}));
    run(app, ui, if fav { "Remove from Favorites" } else { "Add to Favorites" }, "mask.favoriteEffect", json!({"name": name, "favorite": !fav}));
    Menu::submenu_row_enabled(ui, crate::i18n::tr("Move to Group"), ui.is_enabled(), |ui, _| group_menu(app, ui, &[name.to_string()]));
    Divider::horizontal().show(ui);
    if menu_run(ui, name, "Delete…", crate::i18n::tr("Delete…"), true, None) {
        app.ui.dialog = Some(Dialog::DeleteEffects { names: vec![name.to_string()] });
    }
}

/// The groups to move effects `names` to, None, and New Group….
fn group_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, names: &[String]) {
    let groups: Vec<String> = fx::menu_order(&app.session.mask_effects).into_iter().map(|(g, _)| g).filter(|g| !g.is_empty()).collect();
    for g in groups.iter().map(String::as_str).chain(std::iter::once("")) {
        let label = if g.is_empty() { crate::i18n::tr("None").to_string() } else { g.to_string() };
        let scope = names.join("|");
        if menu_run(ui, &scope, &format!("group:{g}"), &label, true, None) {
            let _ = app.run("mask.setEffectGroup", json!({"names": names, "group": g}));
        }
    }
    Divider::horizontal().show(ui);
    if menu_run(ui, &names.join("|"), "New Group…", crate::i18n::tr("New Group…"), true, None) {
        let names = json!({"names": names});
        crate::panels::dialogs::prompt(app, "New Effect Group", "Group name", "", "mask.setEffectGroup", names, "group");
    }
}

/// Save owns these two naming leaves; the manager deliberately keeps its native focus path.
fn save_naming_field(ui: &mut egui::Ui, widget: &str, value: &mut String, hint: &str, width: f32, autofocus: bool) -> bool {
    use craft::ui::contract::{Bind, Props as _, Show as _};
    use craft::ui::runtime::focus::{self, FocusTarget};
    let width = width.min(ui.available_width()).max(1.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, ui.spacing().interact_size.y.max(24.0)), Sense::hover());
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let eligible = real && ui.is_enabled() && ui.is_visible() && ui.clip_rect().intersects(rect);
    let shown = craft::ui::text::TextField::new(crate::i18n::tr(hint).to_owned(), Bind::bound(value))
        .id_salt(widget)
        .placeholder(crate::i18n::tr(hint).to_owned())
        .enabled(ui.is_enabled())
        .read_only(!ui.is_enabled())
        .limit(None)
        .submits()
        .show_at(ui, rect);
    if real {
        register_response(widget, shown.response.rect, &shown.response);
    }
    // The actual native single-line controls end their edit on Return. Group does not
    // confirm the parent; Save's caller alone decides whether this was the Name field.
    if eligible
        && shown.submitted
        && let Some(focused) = focus::focused_rec(ui.ctx()).filter(|record| record.stop.id == shown.response.id)
    {
        ui.memory_mut(|memory| memory.surrender_focus(focused.stop.root));
    }
    if eligible && autofocus && value.is_empty() && ui.memory(|memory| memory.focused().is_none()) {
        focus::request(ui.ctx(), FocusTarget::Id(shown.response.id));
    }
    eligible && shown.submitted
}

/// How wide the button that picks an existing group is.
const PICKER_WIDTH: f32 = 28.0;

/// The New Effect Preset dialog's body.
pub fn save_body(
    app: &LightcraftApp,
    ui: &mut egui::Ui,
    name: &mut String,
    group: &mut String,
    fields: &mut Vec<String>,
    amount: &mut bool,
    brush: &mut bool,
) -> bool {
    let t = Tokens::get(ui.ctx());
    let enter = save_naming_field(ui, "field:effectName", name, "Preset name", f32::INFINITY, true);
    use craft::ui::buttons::Button;
    use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, clicked};
    use craft::ui::popover::Popover;
    use craft::ui::runtime::focus::Initial;

    let key = egui::Id::unique("lightcraft.save-effect.group-popup");
    let stamp = key.with("shown-pass");
    let pass = ui.ctx().cumulative_pass_nr();
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let (kept, previous) = ui.data(|data| (data.get_temp::<bool>(key).unwrap_or(false), data.get_temp::<u64>(stamp)));
    // This app-only bit is the Popover's bound open state, not a focus owner.
    // A newly shown Save never inherits a popup from a vanished parent.
    let mut popup = kept && previous.is_some_and(|last| last == pass || last.checked_add(1) == Some(pass));
    let mut picked = None;
    #[cfg(test)]
    let mut popup_probe_buttons: Vec<(String, egui::Response)> = Vec::new();
    ui.horizontal(|ui| {
        // Group Return ends its field but must never use Name's Save path.
        save_naming_field(ui, "field:effectGroup", group, "Group (optional)", ui.available_width() - PICKER_WIDTH - ui.spacing().item_spacing.x, false);
        let (rect, _) = ui.allocate_exact_size(vec2(PICKER_WIDTH, ui.spacing().interact_size.y.max(24.0)), Sense::hover());
        // (LightCraft's own dots: a text "…" sits at the foot of a button this small)
        let dots = crate::panels::detail::host::IconGlyph(crate::icons::Icon::Dots);
        let picker = craft::ui::buttons::IconButton::new(craft::ui::primitives::IconSpec::glyph(&dots))
            .id_salt("button:effectGroupPick")
            .label(crate::i18n::tr("Choose a group").to_owned())
            .tooltip(crate::i18n::tr("Choose a group").to_owned())
            .show_at(ui, rect);
        if real {
            register_response("button:effectGroupPick", picker.rect, &picker);
            if clicked(&picker) {
                popup = !popup;
            }
        }
        let _popup_result = Popover::new(crate::i18n::tr("Choose a group").to_owned(), Bind::bound(&mut popup))
            .id_salt("save-effect-group-choices")
            .opened_by(&picker)
            .initial_focus(Initial::Keep)
            .overlay(|mut overlay| {
                // Keep the real opener focused on open, then let ordinary Tab
                // cross this instance's option Buttons into its parent. Keep is
                // not the nontrapping KeepAnchor dropdown contract. SDK owns keys.
                overlay.focus = overlay.focus.trap(false);
                overlay.dismiss = overlay.dismiss.focus_left(false).opener_escape(craft::ui::contract::overlay::OpenerEscape::CloseAndBlur);
                overlay
            })
            .show(ui, |ui, _| {
                for (value, _) in fx::menu_order(&app.session.mask_effects) {
                    let label = if value.is_empty() { crate::i18n::tr("None").to_owned() } else { value.clone() };
                    let option = Button::new(label).id_salt(("save-effect-group-option", value.as_str())).show(ui);
                    #[cfg(test)]
                    if real {
                        popup_probe_buttons.push((if value.is_empty() { "None".to_owned() } else { value.clone() }, option.response.clone()));
                    }
                    if real {
                        register_response(format!("popup:{}", if value.is_empty() { "None" } else { value.as_str() }), option.rect, &option);
                        if clicked(&option) {
                            picked = Some(value);
                        }
                    }
                }
            });
        #[cfg(test)]
        if real {
            let family: Vec<_> = craft::ui::runtime::overlay::family(ui.ctx(), egui::Id::unique("lightcraft.save-effect"))
                .into_iter().filter_map(|id|craft::ui::runtime::overlay::entry(ui.ctx(), id))
                .map(|entry|serde_json::json!({"id":format!("{:?}",entry.id),"rank":format!("{:?}",entry.rank),"exiting":entry.exiting})).collect();
            let buttons: Vec<_> = popup_probe_buttons.iter().map(|(label,response)| {
                let point = response.rect.center();
                serde_json::json!({"label":label,"id":format!("{:?}",response.id),"enabledWhenDrawn":response.enabled(),
                    "senseWhenDrawn":format!("{:?}",response.sense),"rect":[response.rect.min.x,response.rect.min.y,response.rect.max.x,response.rect.max.y],
                    "layerWhenDrawn":format!("{:?}",response.layer_id),"cachedLayerAtInterior":format!("{:?}",ui.ctx().layer_id_at(point)),
                    "lastPassShapesAtInterior":format!("{:?}",craft::ui::runtime::pointer::shapes_at(ui.ctx(),point))})
            }).collect();
            let receipt = serde_json::json!({"frameAtShow":ui.ctx().cumulative_frame_nr(),"passAtShow":ui.ctx().cumulative_pass_nr(),
                "phase":"immediately-after-Popover-show-within-draw","popoverId":format!("{:?}",_popup_result.output.response.id),
                "outputOpen":_popup_result.output.open,"outputClosed":format!("{:?}",_popup_result.output.closed),"bindingAfterShow":popup,
                "bodyRan":_popup_result.inner.is_some(),"familyAtShow":family,"actualOptionResponses":buttons});
            if _popup_result.output.closed.is_some() {
                println!("SAVE_EFFECT_POPOVER_CLOSE_OUTPUT {}",receipt);
            }
            ui.ctx().data_mut(|data|data.insert_temp(egui::Id::unique("save-effect-popup-frame-provenance"),receipt));
        }
    });
    if real {
        if let Some(value) = picked {
            *group = value;
            popup = false;
        }
        ui.data_mut(|data| {
            data.insert_temp(key, popup);
            data.insert_temp(stamp, pass);
        });
    }
    ui.label(egui::RichText::new(crate::i18n::tr("Settings to include")).color(t.text_dim));
    let adjust = app
        .session
        .active()
        .and_then(|id| app.session.develop_of(id))
        .and_then(|d| app.session.active_mask.and_then(|mid| d.masks.iter().find(|m| m.id == mid).map(|m| m.adjust)));
    let changed = adjust.as_ref().map(fx::changed_fields).unwrap_or_default();
    let list = checklist();
    use craft::ui::choice::Checkbox;
    use craft::ui::contract::{FieldLayout, LabelKind, SideLabel};
    use craft::ui::display::Text;
    use craft::ui::fonts::FontRef;
    use craft::ui::layout_components::Stack;
    use craft::ui::primitives::DocRun;
    use craft::ui::style::DataColor;

    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    Stack::new().id_salt("save-effect-include-block").show(ui, |ui, cx| {
        // This body also runs in the framework's invisible measuring and sizing passes: only the real one publishes or acts.
        let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
        ui.columns(2, |cols| {
            for (i, k) in list.iter().enumerate() {
                let mut on = fields.iter().any(|field| field == k);
                let was = on;
                let changed_here = changed.iter().any(|field| field == k);
                let words = crate::i18n::tr(field_label(k)).to_owned();
                // Preserve the original normal/strong and dim color distinction
                // as public static text inside the SDK-owned checkbox label.
                // labelled_with keeps the whole label in the real checkbox hit.
                let font = egui::TextStyle::Body.resolve(cols[i % 2].style());
                let face = match &font.family {
                    egui::FontFamily::Proportional => FontRef::proportional(),
                    egui::FontFamily::Monospace => FontRef::monospace(),
                    egui::FontFamily::Name(name) => FontRef::named(name),
                };
                let run = DocRun::new(words.clone(), face, font.size, DataColor::new(if changed_here { t.text } else { t.text_label }));
                let run = if changed_here { run.bold() } else { run };
                let shown = cx.labelled_with(
                    &mut cols[i % 2],
                    ("effectInclude", *k),
                    words.into(),
                    SideLabel::new().layout(FieldLayout::After).kind(LabelKind::Hidden),
                    Checkbox::new(Bind::bound(&mut on)),
                    Text::spans(&[run]),
                );
                if real {
                    register_response(format!("effectInclude:{k}"), shown.response.rect, &shown.response);
                    if on != was {
                        fields.retain(|field| field != k);
                        if on {
                            fields.push(k.to_string());
                        }
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            for (id, words) in [("all", "All"), ("none", "None"), ("changed", "Changed Only")] {
                let shown = Button::new(crate::i18n::tr(words).to_owned()).id_salt(("effectInclude", id)).show(ui);
                if real {
                    register_response(format!("effectInclude:{id}"), shown.rect, &shown);
                    if clicked(&shown) {
                        *fields = match id {
                            "all" => list.iter().map(|field| field.to_string()).collect(),
                            "none" => Vec::new(),
                            _ => changed.iter().filter(|field| field.as_str() != "color_hue").cloned().collect(),
                        };
                    }
                }
            }
        });
        ui.label(
            egui::RichText::new(crate::i18n::tr(
                "Highlighted: sliders this mask changes. Sliders left out stay as they are when the preset is applied.",
            ))
            .color(t.text_dim)
            .size(11.0),
        );
        let shown =
            Checkbox::new(Bind::bound(&mut *amount)).label(crate::i18n::tr("Include Amount").to_owned()).id_salt("check:effectAmount").show(ui);
        if real {
            register_response("check:effectAmount", shown.response.rect, &shown.response);
        }
        let shown = Checkbox::new(Bind::bound(&mut *brush))
            .label(crate::i18n::tr("Include brush size, feather and flow").to_owned())
            .id_salt("check:effectBrush")
            .show(ui);
        if real {
            register_response("check:effectBrush", shown.response.rect, &shown.response);
        }
    });
    enter
}

/// Save from the New Effect Preset dialog.
pub fn save_confirm(app: &mut LightcraftApp, name: &str, group: &str, fields: &[String], amount: bool, brush: bool) -> Result<Value, String> {
    if name.trim().is_empty() {
        return Err(crate::i18n::tr("Name the preset").to_string());
    }
    if fields.is_empty() {
        return Err(crate::i18n::tr("Choose at least one setting to include").to_string());
    }
    let mut keep: Vec<String> = fields.to_vec();
    if keep.iter().any(|f| f == "color_sat") {
        keep.push("color_hue".into());
    }
    let all = checklist().iter().all(|k| keep.iter().any(|f| f == k));
    let mut p = json!({"name": name.trim(), "group": group.trim(), "amount": amount});
    p["fields"] = if all { json!("all") } else { json!(keep) };
    if brush {
        p["brush"] = brush_now(app);
    }
    app.run("mask.saveEffect", p)
}

/// The Manage Effect Presets window's body.
pub fn manage_body(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    search: &mut String,
    selected: &mut Vec<String>,
    rename: &mut Option<(String, String)>,
    confirm_delete: &mut bool,
    new_group: &mut String,
) {
    let t = Tokens::get(ui.ctx());
    ui.set_min_width(500.0);
    // effects that went away (deleted, renamed) leave the selection
    selected.retain(|n| app.session.mask_effects.iter().any(|e| &e.name == n));
    ui.horizontal(|ui| {
        let field = crate::text_field::TextField::singleline("field:effectSearch", search)
            .hint(crate::i18n::tr("Search effects"))
            .width(ui.available_width() - 120.0)
            .show(ui);
        // Escape in the search field closes the window, as it did before the field took the key
        if field.cancelled() {
            crate::dialogs::field_escape_closes(ui);
        }
        let n = app.session.mask_effects.len();
        ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} preset{}", if n == 1 { "" } else { "s" }, n = n)).color(t.text_dim));
    });
    let q = search.trim().to_lowercase();
    let effects = app.session.mask_effects.clone();
    let mut toggle_star: Option<(String, bool)> = None;
    let mut commit_rename: Option<(String, String)> = None;
    egui::ScrollArea::vertical().id_salt("manageEffects").max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
        for (group, ix) in fx::menu_order(&effects) {
            let list: Vec<&MaskEffect> = ix
                .iter()
                .filter_map(|i| effects.get(*i))
                .filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q) || e.group.to_lowercase().contains(&q))
                .collect();
            if list.is_empty() {
                continue;
            }
            ui.add_space(4.0);
            let heading = if group.is_empty() { crate::i18n::tr("Ungrouped").to_string() } else { group.clone() };
            let caption = format!("{heading}  ({})", list.len());
            let galley = ui.painter().layout_no_wrap(caption.clone(), t.semibold(12.0), t.text_label);
            let (_, heading_rect) = ui.allocate_space(galley.size());
            let h = Hit::new(&format!("effectGroup:{group}"), caption).show_at(ui, heading_rect);
            ui.painter().galley(heading_rect.min, galley, t.text_label);
            if h.clicked() {
                // a click on the heading selects the group
                *selected = list.iter().map(|e| e.name.clone()).collect();
            }
            for e in list {
                let sel = selected.contains(&e.name);
                ui.horizontal(|ui| {
                    let star = crate::rating::favorite_button(ui, format!("effectStar:{}", e.name), e.favorite, crate::i18n::tr("Favorite"));
                    if star.clicked() {
                        toggle_star = Some((e.name.clone(), !e.favorite));
                    }
                    if let Some((old, text)) = rename.as_mut().filter(|(old, _)| *old == e.name) {
                        let r = crate::text_field::TextField::singleline("field:effectRename", text).width(220.0).select_on_focus(true).show(ui);
                        if !r.editing && r.ending.is_none() {
                            crate::text_field::focus_id(ui.ctx(), r.response.id);
                        }
                        if r.cancelled() {
                            *rename = None;
                        } else if r.committed() {
                            commit_rename = Some((old.clone(), text.trim().to_string()));
                        }
                        return;
                    }
                    let (_, rect) = ui.allocate_space(vec2(240.0, 22.0));
                    let plate = if sel { Fill::new(t.tool_active, t.tool_active, 3.0) } else { Fill::on_hover(t.hover, 3.0) };
                    // (rows scrolled out of view can't be clicked)
                    let row = ui
                        .is_rect_visible(rect)
                        .then(|| Hit::new(&format!("effectRow:{}", e.name), display_name(&e.name)).fill(plate).selected(sel).show_at(ui, rect));
                    let g = crate::widgets::one_line(ui.painter(), &display_name(&e.name), t.font(12.5), t.text, rect.width() - 8.0);
                    let elided = g.elided;
                    ui.painter().galley(rect.left_center() + vec2(4.0, -g.size().y / 2.0), g, t.text);
                    let row = match row {
                        Some(r) if elided => Some(r.on_hover_text(display_name(&e.name))),
                        other => other,
                    };
                    if let Some(r) = row {
                        if r.clicked() {
                            let add = ui.input(|i| i.modifiers.command || i.modifiers.shift);
                            if add {
                                if sel {
                                    selected.retain(|n| n != &e.name);
                                } else {
                                    selected.push(e.name.clone());
                                }
                            } else {
                                *selected = vec![e.name.clone()];
                            }
                        }
                        if r.double_clicked() {
                            *rename = Some((e.name.clone(), e.name.clone()));
                        }
                    }
                    let mut info = summary(e);
                    if !e.extra.is_empty() {
                        info += &crate::i18n::tr_format!(" · kept, not applied: {}", e.extra.keys().cloned().collect::<Vec<_>>().join(", "));
                    }
                    ui.add(egui::Label::new(egui::RichText::new(info).color(t.text_dim).size(11.0)).truncate());
                });

            }
        }
    });
    if let Some((name, on)) = toggle_star {
        let _ = app.run("mask.favoriteEffect", json!({"name": name, "favorite": on}));
    }
    if let Some((old, to)) = commit_rename {
        *rename = None;
        if !to.is_empty() && to != old {
            match app.run("mask.renameEffect", json!({"name": old, "to": to})) {
                Ok(_) => *selected = vec![to],
                Err(e) => app.toast_error(ui.ctx(), e),
            }
        }
    }
    Divider::horizontal().show(ui);
    let n = selected.len();
    let one = (n == 1).then(|| selected.first().cloned()).flatten();
    let mask = app.session.active_mask.filter(|_| app.session.active().is_some());
    if *confirm_delete && n > 0 {
        ui.label(
            egui::RichText::new(crate::i18n::tr_format!("Delete {n} effect preset{}? This can't be undone.", if n == 1 { "" } else { "s" }, n = n))
                .color(t.text),
        );
        ui.horizontal(|ui| {
            if action(ui, "button:effectDeleteConfirm", "Delete", true, false).clicked {
                let _ = app.run("mask.deleteEffect", json!({"names": selected.clone()}));
                selected.clear();
                *confirm_delete = false;
            }
            if action(ui, "button:effectDeleteCancel", "Cancel", true, false).clicked {
                *confirm_delete = false;
            }
        });
        return;
    }
    ui.horizontal_wrapped(|ui| {
        let button = |ui: &mut egui::Ui, id: &str, text: &str, enabled: bool| action(ui, &format!("button:effect{id}"), text, enabled, false).clicked;
        if button(ui, "Apply", "Apply", one.is_some() && mask.is_some())
            && let (Some(name), Some(mid)) = (&one, mask)
        {
            apply(app, ui.ctx(), name, json!({"id": mid}));
        }
        if button(ui, "Update", "Update with Mask", one.is_some() && mask.is_some())
            && let (Some(name), Some(mid)) = (&one, mask)
        {
            let _ = app.run("mask.updateEffect", json!({"name": name, "id": mid}));
        }
        if button(ui, "Rename", "Rename", one.is_some())
            && let Some(name) = &one
        {
            *rename = Some((name.clone(), name.clone()));
        }
        if button(ui, "Duplicate", "Duplicate", one.is_some())
            && let Some(name) = &one
            && let Ok(v) = app.run("mask.duplicateEffect", json!({"name": name}))
            && let Some(copy) = v["name"].as_str()
        {
            *selected = vec![copy.to_string()];
        }
        if button(ui, "Favorite", "★ Favorite", n > 0) {
            let all_fav = selected.iter().all(|s| effects.iter().any(|e| &e.name == s && e.favorite));
            let _ = app.run("mask.favoriteEffect", json!({"names": selected.clone(), "favorite": !all_fav}));
        }
        let g = action(ui, "button:effectGroup", "Move to Group…", n > 0, false);
        popover(ui, egui::Id::unique("mask-effect-move-to-group"), "Move to Group…", &g.response, g.clicked, |ui| {
            let names = selected.clone();
            let mut close = false;
            for (grp, _) in fx::menu_order(&app.session.mask_effects) {
                let label = if grp.is_empty() { crate::i18n::tr("None").to_string() } else { grp.clone() };
                if action(ui, &format!("popup:effectGroup:{label}"), &label, true, true).clicked {
                    let _ = app.run("mask.setEffectGroup", json!({"names": names, "group": grp}));
                    close = true;
                }
            }
            Divider::horizontal().show(ui);
            let r = crate::text_field::TextField::singleline("field:effectNewGroup", new_group)
                .hint(crate::i18n::tr("New group — Return"))
                .width(180.0)
                .show(ui);
            if r.ending == Some(crate::text_field::Ending::Return) && !new_group.trim().is_empty() {
                let _ = app.run("mask.setEffectGroup", json!({"names": names, "group": new_group.trim()}));
                new_group.clear();
                close = true;
            }
            close
        });
        if button(ui, "Export", "Export…", n > 0 && app.services.save_preset_file.is_some()) {
            let _ = app.run("file.exportMaskEffects", json!({"names": selected.clone()}));
        }
        if button(ui, "Delete", "Delete…", n > 0) {
            *confirm_delete = true;
        }
    });
    if mask.is_none() {
        ui.label(egui::RichText::new(crate::i18n::tr("Select a mask to apply or update presets.")).color(t.text_dim).size(11.0));
    }
}

/// The distinct values, sorted.
fn distinct<'a>(values: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut v: Vec<&str> = values.collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The toast after an import (`preset.import` / `mask.importEffects` result): how many effects,
/// in how many groups, and the settings they carry that LightCraft can't apply.
pub fn import_toast(v: &Value) -> String {
    let rows: Vec<&Value> = v["effects"].as_array().or_else(|| v["imported"].as_array()).into_iter().flatten().collect();
    let mut msg = imported_effects_message(rows.len());
    let groups = distinct(rows.iter().filter_map(|r| r["group"].as_str()).filter(|g| !g.is_empty()));
    if !groups.is_empty() {
        msg += &in_groups_message(groups.len());
    }
    let skipped = distinct(rows.iter().flat_map(|r| r["unmapped"].as_array().into_iter().flatten()).filter_map(Value::as_str));
    if !skipped.is_empty() {
        msg += &not_applied_message(&skipped);
    }
    msg
}

fn imported_effects_message(n: usize) -> String {
    crate::i18n::tr_format!("Imported {e} mask effect{} (Masking ▸ Effect)", crate::i18n::plural_s(n), e = n)
}

fn in_groups_message(groups: usize) -> String {
    crate::i18n::tr_format!(" in {g} group{}", crate::i18n::plural_s(groups), g = groups)
}

fn not_applied_message(settings: &[&str]) -> String {
    crate::i18n::tr_format!(" — kept but not applied: {}", settings.join(", "))
}

#[cfg(test)]
#[path = "save_effect_flow_tests.rs"]
mod save_effect_flow_tests;
