//! Settings ▸ Develop (fork-owned, own design): the Develop tool profiles ([`crate::tool_profiles`]). Pick the
//! profile in use, make, duplicate, rename and delete the user's own; give one a Custom group of chosen sliders in a
//! chosen order; switch the panel's groups on and off and hide the sliders and options of each. The built-in
//! profiles (Default, Witchcraft) show what they hold but can't be changed. Every change runs
//! `view.toolProfile` / `view.toolProfileEdit`, so agents and the CLI can do the same.

use serde_json::{Value, json};

use super::settings::{check, heading, hint, row};
use crate::LccApp;
use crate::icons::Icon;
use crate::theme::Tokens;
use crate::tool_profiles::{GROUPS, options_of, sliders_of};
use crate::widgets::{register, text_button};

/// Runs a tool profile command; a refusal is said in a toast.
fn edit(app: &mut LccApp, ctx: &egui::Context, params: Value) {
    if let Err(e) = app.run("view.toolProfileEdit", params) {
        app.toast(ctx, e);
    }
}

pub fn settings_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    super::settings::heading_help(ui, t, "Tool Profiles", "toolProfiles");
    hint(
        ui,
        t,
        "A tool profile picks the groups and sliders the Develop panel shows. Default and Witchcraft are built in and can't be changed: make a new profile, or duplicate one, to make your own.",
    );
    let profile = app.ui.settings.tool_profiles.active().clone();
    let builtin = profile.builtin();
    let all: Vec<(String, String)> = app.ui.settings.tool_profiles.all().map(|p| (p.id.clone(), p.label())).collect();
    let ctx = ui.ctx().clone();
    row(ui, t, "Profile", |ui| {
        let mut chosen = profile.id.clone();
        let combo = crate::widgets::combo_box("toolProfileSettings").width(220.0).selected_text(profile.label()).show_ui(ui, |ui| {
            for (id, label) in &all {
                let r = ui.selectable_value(&mut chosen, id.clone(), label);
                register(ui.ctx(), format!("toolProfileSettings:{id}"), r.rect);
            }
        });
        register(ui.ctx(), "combo:toolProfileSettings", combo.response.rect);
        if chosen != profile.id
            && let Err(e) = app.run("view.toolProfile", json!({"id": chosen}))
        {
            app.toast(&ctx, e);
        }
    });
    crate::dialog_frame::control_row(ui, |ui| {
        if text_button(ui, "toolProfileNew", crate::i18n::tr("New"), false)
            .on_hover_text(crate::i18n::tr("A new profile showing every group"))
            .clicked()
        {
            edit(app, &ctx, json!({"op": "new"}));
        }
        if text_button(ui, "toolProfileDuplicate", crate::i18n::tr("Duplicate"), false)
            .on_hover_text(crate::i18n::tr("A copy of this profile, to change"))
            .clicked()
        {
            edit(app, &ctx, json!({"op": "duplicate", "id": profile.id}));
        }
        let r = ui.add_enabled_ui(!builtin, |ui| text_button(ui, "toolProfileDelete", crate::i18n::tr("Delete"), false)).inner;
        if r.clicked() {
            edit(app, &ctx, json!({"op": "delete", "id": profile.id}));
        }
    });
    if builtin {
        hint(ui, t, "Built-in profiles can't be changed. Duplicate this one to make your own.");
    } else {
        row(ui, t, "Name", |ui| {
            let key = egui::Id::new(("toolProfileName", profile.id.as_str()));
            let mut name = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| profile.name.clone());
            let r = crate::text_field::TextField::singleline("toolProfileName", &mut name).width(220.0).show(ui);
            if r.committed() {
                if name.trim() != profile.name
                    && let Err(e) = app.run("view.toolProfileEdit", json!({"op": "rename", "id": profile.id, "name": name}))
                {
                    app.toast(&ctx, e);
                }
                ui.data_mut(|d| d.remove::<String>(key));
            } else if r.cancelled() {
                ui.data_mut(|d| d.remove::<String>(key));
            } else {
                ui.data_mut(|d| d.insert_temp(key, name));
            }
        });
    }
    // everything below changes the profile: shown greyed for a built-in one
    ui.add_enabled_ui(!builtin, |ui| {
        custom_group(app, ui, t, &profile);
        groups(app, ui, t, &profile);
    });
}

/// The Custom group: its sliders in order, each with Move Up / Move Down / Remove, and a menu to add one.
fn custom_group(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, profile: &crate::tool_profiles::ToolProfile) {
    let ctx = ui.ctx().clone();
    heading(ui, t, "Custom");
    hint(ui, t, "The sliders added here show first in the Develop panel, in this order, and not again in their own group.");
    let id = profile.id.clone();
    for (i, c) in profile.custom.iter().enumerate() {
        let Some(spec) = lcc_develop::controls::find(c) else { continue };
        let group = crate::tool_profiles::group_of(c).and_then(|g| GROUPS.iter().find(|(k, _)| *k == g)).map_or("", |(_, l)| *l);
        crate::dialog_frame::control_row(ui, |ui| {
            ui.label(format!("{}. {} · {}", i + 1, crate::i18n::tr(spec.label), crate::i18n::tr(group)));
            let size = egui::vec2(20.0, 20.0);
            if crate::widgets::icon_button(ui, &format!("toolCustomUp:{c}"), Icon::ChevronUp, size, false, i > 0, crate::i18n::tr("Move Up"))
                .clicked()
            {
                edit(app, &ctx, json!({"op": "custom", "id": id, "up": c}));
            }
            let last = i + 1 == profile.custom.len();
            if crate::widgets::icon_button(ui, &format!("toolCustomDown:{c}"), Icon::ChevronDown, size, false, !last, crate::i18n::tr("Move Down"))
                .clicked()
            {
                edit(app, &ctx, json!({"op": "custom", "id": id, "down": c}));
            }
            if crate::widgets::icon_button(ui, &format!("toolCustomRemove:{c}"), Icon::Close, size, false, true, crate::i18n::tr("Remove")).clicked()
            {
                edit(app, &ctx, json!({"op": "custom", "id": id, "remove": c}));
            }
        });
    }
    row(ui, t, "Add a slider", |ui| {
        let mut add: Option<String> = None;
        let combo =
            crate::widgets::combo_box("toolCustomAdd").width(260.0).height(360.0).selected_text(crate::i18n::tr("Choose…")).show_ui(ui, |ui| {
                for (g, title) in GROUPS {
                    let sliders: Vec<_> = sliders_of(g).filter(|s| !profile.custom.iter().any(|c| c == s.id)).collect();
                    if sliders.is_empty() {
                        continue;
                    }
                    ui.label(egui::RichText::new(crate::i18n::tr(title)).small().weak());
                    for s in sliders {
                        let r = ui.selectable_label(false, crate::i18n::tr(s.label));
                        register(ui.ctx(), format!("toolCustomAdd:{}", s.id), r.rect);
                        if r.clicked() {
                            add = Some(s.id.to_string());
                        }
                    }
                }
            });
        register(ui.ctx(), "combo:toolCustomAdd", combo.response.rect);
        if let Some(s) = add {
            edit(app, &ctx, json!({"op": "custom", "id": id, "add": s}));
        }
    });
}

/// The panel's groups: each switched on or off, and when on, its sliders and options one by one.
fn groups(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, profile: &crate::tool_profiles::ToolProfile) {
    let ctx = ui.ctx().clone();
    heading(ui, t, "Groups");
    hint(ui, t, "Switch a group off to hide it, or open it to hide some of its sliders and options.");
    let id = profile.id.clone();
    let mut witchcraft = profile.witchcraft;
    if check(ui, "toolWitchcraft", &mut witchcraft, "Show Primaries and HSL Curves in either grading mode") {
        edit(app, &ctx, json!({"op": "witchcraft", "id": id, "on": witchcraft}));
    }
    for (g, title) in GROUPS {
        let mut on = profile.shows_group(g);
        if check(ui, &format!("toolGroup:{g}"), &mut on, title) {
            edit(app, &ctx, json!({"op": "group", "id": id, "group": g, "on": on}));
        }
        let items: Vec<(String, String)> = options_of(g)
            .map(|(o, l)| (o.to_string(), crate::i18n::tr(l).to_string()))
            .chain(sliders_of(g).map(|s| (s.id.to_string(), crate::i18n::tr(s.label).to_string())))
            .collect();
        if !on || items.is_empty() {
            continue;
        }
        crate::dialog_frame::control_row(ui, |ui| {
            ui.vertical(|ui| {
                let header = egui::CollapsingHeader::new(crate::i18n::tr("Sliders and options")).id_salt(("toolGroupItems", *g)).show(ui, |ui| {
                    for (item, label) in &items {
                        let in_custom = profile.custom.iter().any(|c| c == item);
                        let mut shown = profile.shows_item(item);
                        let r = ui.add_enabled(!in_custom, egui::Checkbox::new(&mut shown, label));
                        register(ui.ctx(), format!("check:toolItem:{item}"), r.rect);
                        let r = if in_custom { r.on_disabled_hover_text(crate::i18n::tr("In the Custom group")) } else { r };
                        if r.changed() {
                            edit(app, &ctx, json!({"op": "item", "id": id, "item": item, "on": shown}));
                        }
                    }
                });
                register(ui.ctx(), format!("toolGroupItems:{g}"), header.header_response.rect);
            });
        });
    }
}
