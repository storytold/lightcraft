//! Mask effect presets in the UI: the Effect menu above a mask's sliders (grouped, searchable,
//! keyboard-driven, previews the effect on the photo while the pointer rests on it), the
//! New Effect Preset dialog (which sliders, Amount, brush settings) and the Manage Effect Presets
//! window (search, rename, update, duplicate, stars, groups, delete). Every change runs a
//! `mask.*Effect` command.

use egui::{Sense, vec2};
use lightcraft_develop::Mask;
use lightcraft_engine::cmd::mask_effects::{self as fx, FIELDS, MaskEffect};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::Dialog;
use crate::theme::Tokens;
use crate::widgets::{register, register_response};

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
    egui::Frame::NONE.inner_margin(crate::widgets::pad_margin(ui, 2, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Effect")).font(t.font(12.0)).color(t.text_label));
            let current = fx::matching_effect(&app.session, &mask.adjust);
            let label = current.as_deref().map(display_name).unwrap_or_else(|| crate::i18n::tr("Custom").to_string());
            let r = crate::widgets::dropdown(ui, "maskEffect", &label, t.font(12.0), t.text);
            if r.clicked() {
                ui.data_mut(|d| d.insert_temp(state_id(), MenuState::default()));
            }
            egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| menu(app, ui, mask, current.as_deref()));
        });
    });
}

fn menu(app: &mut LightcraftApp, ui: &mut egui::Ui, mask: &Mask, current: Option<&str>) {
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
        r.response.request_focus();
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
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
            // (rows scrolled out of view can't be clicked)
            if ui.is_rect_visible(rect) {
                register(ui, widget.clone(), rect);
            }
            let lit = st.highlight == Some(i) || resp.hovered();
            if lit {
                ui.painter().rect_filled(rect, 3.0, t.hover);
            }
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
                resp.scroll_to_me(None);
            }
            let resp = if elided { resp.on_hover_text(display_name(name)) } else { resp };
            if resp.hovered() || (st.highlight == Some(i) && moved) {
                hovered = Some(name.clone());
            }
            if resp.clicked() {
                chosen = Some(name.clone());
            }
            crate::widgets::context_menu(&resp, |ui| row_menu(app, ui, mask, name, fav));
        }
    });
    // resting on an effect previews it on the photo (no history entry)
    if let Some(name) = hovered.or_else(|| st.highlight.and_then(|h| rows.get(h)).map(|(_, n, _)| n.clone())) {
        preview(app, mask, &name);
    }
    ui.data_mut(|d| d.insert_temp(state_id(), st));
    if let Some(name) = chosen {
        apply(app, ui.ctx(), &name, json!({"id": mask.id}));
        ui.close();
        return;
    }
    ui.separator();
    let item = |ui: &mut egui::Ui, id: &str, text: String, enabled: bool| {
        let r = ui.add_enabled(enabled, egui::Button::new(text).frame(false));
        register(ui, id.to_string(), r.rect);
        r.clicked()
    };
    if item(ui, "maskEffectMenu:reset", crate::i18n::tr("Reset Sliders").to_string(), true) {
        let zero: serde_json::Map<String, Value> = FIELDS.iter().map(|k| (k.to_string(), json!(0.0))).collect();
        let _ = app.run("mask.adjust", json!({"id": mask.id, "values": zero}));
        if let Some(tool) = fx::mask_tool(mask) {
            let _ = app.run("mask.toolEffect", json!({"tool": tool}));
        }
        ui.close();
    }
    if item(ui, "maskEffectMenu:save", crate::i18n::tr("Save Current Settings as New Preset…").to_string(), true) {
        open_save(app);
        ui.close();
    }
    if let Some(last) = last_applied(app, mask) {
        let text = crate::i18n::tr_format!("Update “{name}” with Current Settings", name = last);
        if item(ui, "maskEffectMenu:update", text, current != Some(last.as_str())) {
            let _ = app.run("mask.updateEffect", json!({"name": last, "id": mask.id}));
            ui.close();
        }
    }
    if item(ui, "maskEffectMenu:manage", crate::i18n::tr("Manage Presets…").to_string(), true) {
        open_manage(app);
        ui.close();
    }
    if item(ui, "maskEffectMenu:import", crate::i18n::tr("Import Presets…").to_string(), app.services.pick_preset_files.is_some()) {
        let _ = app.run("file.importPresets", json!({}));
        ui.close();
    }
    let any = !app.session.mask_effects.is_empty();
    if item(ui, "maskEffectMenu:export", crate::i18n::tr("Export Presets…").to_string(), any && app.services.save_preset_file.is_some()) {
        let _ = app.run("file.exportMaskEffects", json!({}));
        ui.close();
    }
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

/// The right-click menu of an effect in the Effect menu.
fn row_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, mask: &Mask, name: &str, fav: bool) {
    let run = |app: &mut LightcraftApp, ui: &mut egui::Ui, text: &str, cmd: &str, p: Value| {
        let r = ui.button(crate::i18n::tr(text));
        register(ui, format!("maskEffectRow:{text}"), r.rect);
        if r.clicked() {
            if let Err(e) = app.run(cmd, p) {
                app.toast_error(ui.ctx(), e);
            }
            ui.close();
        }
    };
    let masks = app.session.active().and_then(|id| app.session.develop_of(id)).map_or(0, |d| d.masks.len());
    if ui.add_enabled(masks > 1, egui::Button::new(crate::i18n::tr("Apply to All Masks"))).clicked() {
        apply(app, ui.ctx(), name, json!({"all": true}));
        ui.close();
    }
    if app.session.selection.ids.len() > 1 {
        let text = crate::i18n::tr_format!("Apply to “{mask}” on Selected Photos", mask = mask.name);
        if ui.button(text).on_hover_text(crate::i18n::tr("Also the masks of the same name on the other selected photos")).clicked() {
            apply(app, ui.ctx(), name, json!({"id": mask.id, "photos": "selected"}));
            ui.close();
        }
    }
    ui.separator();
    run(app, ui, "Update with Current Settings", "mask.updateEffect", json!({"name": name, "id": mask.id}));
    if ui.button(crate::i18n::tr("Rename…")).clicked() {
        crate::panels::dialogs::prompt(app, "Rename Effect Preset", "Preset name", name, "mask.renameEffect", json!({"name": name}), "to");
        ui.close();
    }
    run(app, ui, "Duplicate", "mask.duplicateEffect", json!({"name": name}));
    run(app, ui, if fav { "Remove from Favorites" } else { "Add to Favorites" }, "mask.favoriteEffect", json!({"name": name, "favorite": !fav}));
    ui.menu_button(crate::i18n::tr("Move to Group"), |ui| group_menu(app, ui, &[name.to_string()]));
    ui.separator();
    if ui.button(crate::i18n::tr("Delete…")).clicked() {
        app.ui.dialog = Some(Dialog::DeleteEffects { names: vec![name.to_string()] });
        ui.close();
    }
}

/// The groups to move effects `names` to, None, and New Group….
fn group_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, names: &[String]) {
    let groups: Vec<String> = fx::menu_order(&app.session.mask_effects).into_iter().map(|(g, _)| g).filter(|g| !g.is_empty()).collect();
    for g in groups.iter().map(String::as_str).chain(std::iter::once("")) {
        let label = if g.is_empty() { crate::i18n::tr("None").to_string() } else { g.to_string() };
        if ui.button(label).clicked() {
            let _ = app.run("mask.setEffectGroup", json!({"names": names, "group": g}));
            ui.close();
        }
    }
    ui.separator();
    if ui.button(crate::i18n::tr("New Group…")).clicked() {
        let names = json!({"names": names});
        crate::panels::dialogs::prompt(app, "New Effect Group", "Group name", "", "mask.setEffectGroup", names, "group");
        ui.close();
    }
}

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
    let r = crate::text_field::TextField::singleline("field:effectName", name).hint(crate::i18n::tr("Preset name")).width(f32::INFINITY).show(ui);
    if !r.editing && name.is_empty() && ui.memory(|m| m.focused().is_none()) {
        r.response.request_focus();
    }
    let enter = r.ending == Some(crate::text_field::Ending::Return);
    ui.horizontal(|ui| {
        crate::text_field::TextField::singleline("field:effectGroup", group)
            .hint(crate::i18n::tr("Group (optional)"))
            .width(ui.available_width() - 30.0)
            .show(ui);
        let pick = ui.small_button("…").on_hover_text(crate::i18n::tr("Choose a group"));
        register(ui, "button:effectGroupPick", pick.rect);
        egui::Popup::menu(&pick).show(|ui| {
            for (g, _) in fx::menu_order(&app.session.mask_effects) {
                if ui.button(if g.is_empty() { crate::i18n::tr("None").to_string() } else { g.clone() }).clicked() {
                    *group = g;
                }
            }
        });
    });
    ui.label(egui::RichText::new(crate::i18n::tr("Settings to include")).color(t.text_dim));
    let adjust = app
        .session
        .active()
        .and_then(|id| app.session.develop_of(id))
        .and_then(|d| app.session.active_mask.and_then(|mid| d.masks.iter().find(|m| m.id == mid).map(|m| m.adjust)));
    let changed = adjust.as_ref().map(fx::changed_fields).unwrap_or_default();
    let list = checklist();
    ui.columns(2, |cols| {
        for (i, k) in list.iter().enumerate() {
            let mut on = fields.iter().any(|f| f == k);
            let text = egui::RichText::new(crate::i18n::tr(field_label(k)));
            // the sliders this mask changes stand out; the others are dimmed
            let text = if changed.iter().any(|c| c == k) { text.color(t.text).strong() } else { text.color(t.text_dim) };
            let r = cols[i % 2].checkbox(&mut on, text);
            register_response(format!("effectInclude:{k}"), r.rect, &r);
            if r.changed() {
                fields.retain(|f| f != k);
                if on {
                    fields.push(k.to_string());
                }
            }
        }
    });
    ui.horizontal(|ui| {
        for (id, text) in [("all", "All"), ("none", "None"), ("changed", "Changed Only")] {
            let b = ui.small_button(crate::i18n::tr(text));
            register(ui, format!("effectInclude:{id}"), b.rect);
            if b.clicked() {
                *fields = match id {
                    "all" => list.iter().map(|k| k.to_string()).collect(),
                    "none" => Vec::new(),
                    _ => changed.iter().filter(|k| k.as_str() != "color_hue").cloned().collect(),
                };
            }
        }
    });
    ui.label(
        egui::RichText::new(crate::i18n::tr("Highlighted: sliders this mask changes. Sliders left out stay as they are when the preset is applied."))
            .color(t.text_dim)
            .size(11.0),
    );
    let r = ui.checkbox(amount, crate::i18n::tr("Include Amount"));
    register(ui, "check:effectAmount", r.rect);
    let r = ui.checkbox(brush, crate::i18n::tr("Include brush size, feather and flow"));
    register(ui, "check:effectBrush", r.rect);
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
        crate::text_field::TextField::singleline("field:effectSearch", search)
            .hint(crate::i18n::tr("Search effects"))
            .width(ui.available_width() - 120.0)
            .show(ui);
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
            let h = ui.add(
                egui::Label::new(egui::RichText::new(format!("{heading}  ({})", list.len())).font(t.semibold(12.0)).color(t.text_label))
                    .sense(Sense::click()),
            );
            register(ui, format!("effectGroup:{group}"), h.rect);
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
                            r.response.request_focus();
                        }
                        if r.cancelled() {
                            *rename = None;
                        } else if r.committed() {
                            commit_rename = Some((old.clone(), text.trim().to_string()));
                        }
                        return;
                    }
                    let (rect, r) = ui.allocate_exact_size(vec2(240.0, 22.0), Sense::click());
                    if ui.is_rect_visible(rect) {
                        register(ui, format!("effectRow:{}", e.name), rect);
                    }
                    if sel {
                        ui.painter().rect_filled(rect, 3.0, t.tool_active);
                    } else if r.hovered() {
                        ui.painter().rect_filled(rect, 3.0, t.hover);
                    }
                    let g = crate::widgets::one_line(ui.painter(), &display_name(&e.name), t.font(12.5), t.text, rect.width() - 8.0);
                    let elided = g.elided;
                    ui.painter().galley(rect.left_center() + vec2(4.0, -g.size().y / 2.0), g, t.text);
                    let r = if elided { r.on_hover_text(display_name(&e.name)) } else { r };
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
    ui.separator();
    let n = selected.len();
    let one = (n == 1).then(|| selected.first().cloned()).flatten();
    let mask = app.session.active_mask.filter(|_| app.session.active().is_some());
    if *confirm_delete && n > 0 {
        ui.label(
            egui::RichText::new(crate::i18n::tr_format!("Delete {n} effect preset{}? This can't be undone.", if n == 1 { "" } else { "s" }, n = n))
                .color(t.text),
        );
        ui.horizontal(|ui| {
            let d = ui.button(crate::i18n::tr("Delete"));
            register(ui, "button:effectDeleteConfirm", d.rect);
            if d.clicked() {
                let _ = app.run("mask.deleteEffect", json!({"names": selected.clone()}));
                selected.clear();
                *confirm_delete = false;
            }
            let c = ui.button(crate::i18n::tr("Cancel"));
            register(ui, "button:effectDeleteCancel", c.rect);
            if c.clicked() {
                *confirm_delete = false;
            }
        });
        return;
    }
    ui.horizontal_wrapped(|ui| {
        let button = |ui: &mut egui::Ui, id: &str, text: &str, enabled: bool| {
            let r = ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr(text)));
            register(ui, format!("button:effect{id}"), r.rect);
            r.clicked()
        };
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
        let g = ui.add_enabled(n > 0, egui::Button::new(crate::i18n::tr("Move to Group…")));
        register(ui, "button:effectGroup", g.rect);
        egui::Popup::menu(&g).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            let names = selected.clone();
            for (grp, _) in fx::menu_order(&app.session.mask_effects) {
                let label = if grp.is_empty() { crate::i18n::tr("None").to_string() } else { grp.clone() };
                if ui.button(label).clicked() {
                    let _ = app.run("mask.setEffectGroup", json!({"names": names, "group": grp}));
                    ui.close();
                }
            }
            ui.separator();
            let r = crate::text_field::TextField::singleline("field:effectNewGroup", new_group)
                .hint(crate::i18n::tr("New group — Return"))
                .width(180.0)
                .show(ui);
            if r.ending == Some(crate::text_field::Ending::Return) && !new_group.trim().is_empty() {
                let _ = app.run("mask.setEffectGroup", json!({"names": names, "group": new_group.trim()}));
                new_group.clear();
                ui.close();
            }
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

/// The toast after an import (`preset.import` / `mask.importEffects` result): how many effects,
/// in how many groups, and the settings they carry that LightCraft can't apply.
pub fn import_toast(v: &Value) -> String {
    let rows: Vec<&Value> = v["effects"].as_array().or_else(|| v["imported"].as_array()).into_iter().flatten().collect();
    let n = rows.len();
    let mut msg = crate::i18n::tr_format!("Imported {e} mask effect{} (Masking ▸ Effect)", if n == 1 { "" } else { "s" }, e = n);
    let mut groups: Vec<&str> = rows.iter().filter_map(|r| r["group"].as_str()).filter(|g| !g.is_empty()).collect();
    groups.sort_unstable();
    groups.dedup();
    if !groups.is_empty() {
        msg += &crate::i18n::tr_format!(" in {g} group{}", if groups.len() == 1 { "" } else { "s" }, g = groups.len());
    }
    let mut skipped: Vec<&str> = rows.iter().flat_map(|r| r["unmapped"].as_array().into_iter().flatten()).filter_map(Value::as_str).collect();
    skipped.sort_unstable();
    skipped.dedup();
    if !skipped.is_empty() {
        msg += &crate::i18n::tr_format!(" — kept but not applied: {}", skipped.join(", "));
    }
    msg
}
