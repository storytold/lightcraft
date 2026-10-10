//! Help ▸ Keyboard Shortcuts (⌘/): every command's shortcut, editable.
//!
//! Click a shortcut, then press the new keys (Esc cancels); × removes it, ↺ restores the declared
//! one. A key another command had moves to the new command. Changes go through
//! [`crate::shortcuts::assign`] into [`crate::state::AppSettings::keymap`] (saved in `ui.json`),
//! the same path as the `app.setShortcut` command.

use egui::RichText;

use crate::DacApp;
use crate::shortcuts::{self, Bindable};
use crate::theme::Tokens;
use crate::widgets::register;

const SEARCH: &str = "shortcuts-search";
const SHOW_ALL: &str = "shortcuts-show-all";
const MODULE_ONLY: &str = "shortcuts-module-only";

pub fn body(app: &mut DacApp, ui: &mut egui::Ui, t: &Tokens) {
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    capture(app, ui.ctx());
    ui.set_min_width(560.0);
    let (search_id, all_id) = (egui::Id::new(SEARCH), egui::Id::new(SHOW_ALL));
    let mut search: String = ui.data(|d| d.get_temp(search_id)).unwrap_or_default();
    let mut show_all: bool = ui.data(|d| d.get_temp(all_id)).unwrap_or(false);
    let module_id = egui::Id::new(MODULE_ONLY);
    let mut module_only: bool = ui.data(|d| d.get_temp(module_id)).unwrap_or(true);
    let module = app.ui.module;
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut search).hint_text(crate::i18n::tr("Search commands or keys")).desired_width(220.0));
        register(ui.ctx(), "field:shortcutsSearch", r.rect);
        let r = ui.checkbox(&mut show_all, crate::i18n::tr("Show commands without a shortcut"));
        register(ui.ctx(), "check:shortcuts.showAll", r.rect);
        let r = ui.checkbox(&mut module_only, crate::i18n::tr_format!("Only {} keys", crate::i18n::tr(module.label())));
        register(ui.ctx(), "check:shortcuts.moduleOnly", r.rect);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui.add_enabled(!app.ui.settings.keymap.is_empty(), egui::Button::new(crate::i18n::tr("Reset All")));
            register(ui.ctx(), "button:shortcutsResetAll", r.rect);
            if r.clicked() {
                let _ = app.run("app.resetShortcuts", serde_json::json!({}));
                app.recording_shortcut = None;
            }
        });
    });
    ui.data_mut(|d| {
        d.insert_temp(search_id, search.clone());
        d.insert_temp(all_id, show_all);
        d.insert_temp(module_id, module_only);
    });
    // a search looks through every command; otherwise (by default) only the current module's
    let module_only = module_only && search.trim().is_empty();
    ui.label(RichText::new(crate::i18n::tr("Click a shortcut, then press the new keys (Esc cancels).")).size(11.0).color(t.text_dim));
    ui.add_space(4.0);

    let needle = search.trim().to_lowercase();
    let mut rows: Vec<(&'static Bindable, Option<String>)> = shortcuts::bindable()
        .iter()
        .map(|b| (b, shortcuts::binding(&app.ui.settings.keymap, b.id, b.default()).map(str::to_string)))
        .filter(|(b, sc)| show_all || sc.is_some() || app.recording_shortcut.as_deref() == Some(b.id))
        .filter(|(b, _)| !module_only || in_module(module, b.id))
        .filter(|(b, sc)| {
            needle.is_empty()
                || crate::i18n::tr(b.label).to_lowercase().contains(&needle)
                || b.id.to_lowercase().contains(&needle)
                || sc.as_deref().is_some_and(|sc| sc.to_lowercase().contains(&needle) || menu_text(sc, mac).to_lowercase().contains(&needle))
        })
        .collect();
    rows.sort_by_cached_key(|(b, _)| crate::i18n::tr(b.label).to_lowercase());

    // a fixed height, so the dialog doesn't jump around while the search narrows the list
    egui::ScrollArea::vertical().max_height(400.0).min_scrolled_height(400.0).auto_shrink([false, false]).id_salt("shortcuts-list").show(ui, |ui| {
        module_keys(app, ui, t, mac, &needle);
        egui::Grid::new("shortcuts").striped(true).num_columns(4).spacing([12.0, 4.0]).show(ui, |ui| {
            for (b, sc) in &rows {
                row(app, ui, t, b, sc.as_deref(), mac);
            }
            if rows.is_empty() {
                ui.label(RichText::new(crate::i18n::tr("No matching commands")).color(t.text_dim));
                ui.end_row();
            }
        });
        fixed_keys(app, ui, t, mac, &needle);
    });
}

/// Command-id families that matter in each module (the ⌘/ list shows only these by default).
pub(crate) fn in_module(module: crate::module::ModuleId, id: &str) -> bool {
    use crate::module::ModuleId;
    const SHELL: &[&str] = &[
        "app.",
        "module.",
        "panel.sides",
        "panel.all",
        "panel.top",
        "panel.bottom",
        "panel.left",
        "panel.right",
        "panel.toolbar",
        "file.",
        "catalog.",
        "edit.",
        "undo",
        "redo",
        "second.",
        "dialog.export",
        "library.next",
        "library.previous",
        "library.first",
        "library.last",
        "library.select",
        "view.lightsOut",
        "view.screenMode",
        "view.enterFullScreen",
        "view.filmstrip",
        "view.secondWindow",
    ];
    const LIBRARY: &[&str] = &[
        "view.",
        "library.",
        "photo.",
        "album.",
        "stack.",
        "keyword.",
        "metadata.",
        "dialog.",
        "panel.keywords",
        "panel.info",
        "compare.",
        "develop.quick",
        "develop.auto",
        "develop.copy",
        "develop.paste",
        "develop.sync",
    ];
    const DEVELOP: &[&str] = &[
        "develop.",
        "panel.",
        "tool.",
        "brush.",
        "section.",
        "crop.",
        "spot.",
        "mask.",
        "wb.",
        "light.",
        "version.",
        "view.beforeAfter",
        "view.showOriginal",
        "view.clipping",
        "view.softProof",
        "view.zoom",
        "view.maskOverlay",
        "view.cropOverlay",
        "view.visualizeSpots",
        "view.reference",
        "view.loupe",
        "view.gridToggle",
        "view.histogram",
        "view.infoOverlay",
        "photo.",
    ];
    let has = |list: &[&str]| list.iter().any(|p| id.starts_with(p));
    has(SHELL)
        || match module {
            ModuleId::Library => has(LIBRARY),
            ModuleId::Develop => has(DEVELOP),
            other => has(crate::module::get(other).command_prefixes()),
        }
}

/// The current module's own keys (Classic set), above the editable list.
fn module_keys(app: &DacApp, ui: &mut egui::Ui, t: &Tokens, mac: bool, needle: &str) {
    if shortcuts::active_set() != shortcuts::KeymapSet::Classic {
        return;
    }
    let module = app.ui.module;
    let keys: Vec<(&str, &str)> = crate::module::get(module)
        .keymap()
        .iter()
        .map(|(sc, id, params)| {
            let label = match (*id, *params) {
                ("develop.wb", _) => "Auto White Balance",
                ("develop.auto", _) => "Auto Tone",
                _ => shortcuts::find_bindable(id).map_or(*id, |b| b.label),
            };
            (*sc, label)
        })
        .filter(|(sc, label)| {
            needle.is_empty() || crate::i18n::tr(label).to_lowercase().contains(needle) || menu_text(sc, mac).to_lowercase().contains(needle)
        })
        .collect();
    if keys.is_empty() {
        return;
    }
    ui.label(RichText::new(crate::i18n::tr_format!("{} keys", crate::i18n::tr(module.label()))).font(t.semibold(12.5)).color(t.text));
    egui::Grid::new("shortcuts-module").striped(true).num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
        for (sc, label) in keys {
            ui.label(RichText::new(crate::i18n::tr(label)).color(t.text_label));
            ui.label(RichText::new(menu_text(sc, mac)).color(t.text));
            ui.end_row();
        }
    });
    ui.add_space(8.0);
}

/// `Cmd+Shift+Z` as menus show it (`⌘⇧Z` on macOS).
fn menu_text(sc: &str, mac: bool) -> String {
    crate::menubar::shortcut_text(sc, mac)
}

fn row(app: &mut DacApp, ui: &mut egui::Ui, t: &Tokens, b: &Bindable, sc: Option<&str>, mac: bool) {
    let changed = app.ui.settings.keymap.contains_key(b.id);
    let mut name = RichText::new(crate::i18n::tr(b.label)).color(t.text);
    if changed {
        name = name.strong();
    }
    // keys whose command changes in the Library grids (see `shortcuts::handle`)
    let hover = match b.id {
        "panel.presets" => format!("{} — {}", b.id, crate::i18n::tr("In Photo Grid and Square Grid: Flag as Pick and advance")),
        "view.softProof" => format!("{} — {}", b.id, crate::i18n::tr("In Photo Grid and Square Grid: expand or collapse the stack")),
        _ => b.id.to_string(),
    };
    ui.label(name).on_hover_text(hover);
    let recording = app.recording_shortcut.as_deref() == Some(b.id);
    let text = if recording {
        RichText::new(crate::i18n::tr("Press keys…")).color(egui::Color32::WHITE)
    } else {
        match sc {
            Some(sc) => RichText::new(menu_text(sc, mac)).color(t.text),
            None => RichText::new("—").color(t.text_dim),
        }
    };
    let r = ui.add(egui::Button::new(text).min_size(egui::vec2(110.0, 0.0)).selected(recording));
    register(ui.ctx(), format!("button:shortcut-{}", b.id), r.rect);
    if r.clicked() {
        app.recording_shortcut = if recording { None } else { Some(b.id.to_string()) };
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let r = ui.add_enabled(sc.is_some(), egui::Button::new("×").small()).on_hover_text(crate::i18n::tr("Remove shortcut"));
        register(ui.ctx(), format!("button:shortcutClear-{}", b.id), r.rect);
        if r.clicked() {
            apply(app, ui.ctx(), serde_json::json!({"id": b.id, "shortcut": null}));
        }
        let r = ui.add_enabled(changed, egui::Button::new("↺").small()).on_hover_text(match b.default() {
            Some(d) => crate::i18n::tr_format!("Restore {}", menu_text(d, mac)),
            None => crate::i18n::tr("Restore (no shortcut)").to_string(),
        });
        register(ui.ctx(), format!("button:shortcutReset-{}", b.id), r.rect);
        if r.clicked() {
            apply(app, ui.ctx(), serde_json::json!({"id": b.id, "reset": true}));
        }
    });
    ui.label(RichText::new(b.id).size(10.5).color(t.text_dim));
    ui.end_row();
}

/// Keys that aren't a command's own shortcut (ratings, labels, secondary keys): listed, not editable.
/// One the user gave to a command shows struck through.
fn fixed_keys(app: &DacApp, ui: &mut egui::Ui, t: &Tokens, mac: bool, needle: &str) {
    let mut fixed: Vec<(String, &str)> = Vec::new();
    for (sc, id, _) in shortcuts::ALIASES {
        let label = shortcuts::find_bindable(id).map_or(*id, |b| b.label);
        fixed.push((sc.to_string(), label));
    }
    for n in 0..=5 {
        fixed.push((n.to_string(), ["Clear rating", "Rate ★", "Rate ★★", "Rate ★★★", "Rate ★★★★", "Rate ★★★★★"][n]));
        fixed.push((
            format!("Shift+{n}"),
            [
                "Clear rating and advance",
                "Rate ★ and advance",
                "Rate ★★ and advance",
                "Rate ★★★ and advance",
                "Rate ★★★★ and advance",
                "Rate ★★★★★ and advance",
            ][n],
        ));
    }
    for (sc, label) in [("6", "Red label"), ("7", "Yellow label"), ("8", "Green label"), ("9", "Blue label")] {
        fixed.push((sc.to_string(), label));
    }
    let fixed: Vec<_> = fixed
        .into_iter()
        .filter(|(sc, label)| {
            needle.is_empty() || crate::i18n::tr(label).to_lowercase().contains(needle) || menu_text(sc, mac).to_lowercase().contains(needle)
        })
        .collect();
    if fixed.is_empty() {
        return;
    }
    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::tr("Fixed keys")).font(t.semibold(12.5)).color(t.text));
    let taken: Vec<_> = app.ui.settings.keymap.values().filter_map(|s| shortcuts::parse(s)).collect();
    egui::Grid::new("shortcuts-fixed").striped(true).num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
        for (sc, label) in fixed {
            ui.label(RichText::new(crate::i18n::tr(label)).color(t.text_label));
            let mut text = RichText::new(menu_text(&sc, mac)).color(t.text_dim);
            if shortcuts::parse(&sc).is_some_and(|k| taken.contains(&k)) {
                text = text.strikethrough();
            }
            ui.label(text);
            ui.end_row();
        }
    });
}

/// Run `app.setShortcut` and say what changed (a key taken from another command, or why not).
fn apply(app: &mut DacApp, ctx: &egui::Context, params: serde_json::Value) {
    match app.run("app.setShortcut", params) {
        Ok(r) => {
            let lost: Vec<String> = r["removedFrom"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str())
                .map(|id| shortcuts::find_bindable(id).map_or(id, |b| crate::i18n::tr(b.label)).to_string())
                .collect();
            if !lost.is_empty() {
                app.toast_for(ctx, crate::i18n::tr_format!("Removed from {}", lost.join(", ")), 3.0);
            }
        }
        Err(e) => app.toast_for(ctx, e, 3.0),
    }
}

/// While a row is recording: the first key press becomes its shortcut, Esc cancels.
fn capture(app: &mut DacApp, ctx: &egui::Context) {
    let Some(id) = app.recording_shortcut.clone() else { return };
    let pressed = ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            // ⌘, ⇧, ⌥ and ⌃ on their own arrive as keys too: keep waiting for the key they modify
            egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } if !shortcuts::is_modifier(*key) => Some((*key, *modifiers)),
            // the windowing layer turns ⌘C / ⌘X / ⌘V (with or without other modifiers) into these
            egui::Event::Copy => Some((egui::Key::C, i.modifiers)),
            egui::Event::Cut => Some((egui::Key::X, i.modifiers)),
            egui::Event::Paste(_) => Some((egui::Key::V, i.modifiers)),
            _ => None,
        })
    });
    let Some((key, modifiers)) = pressed else { return };
    // the key press is ours: nothing else (a focused button's Space, Tab navigation) acts on it
    ctx.input_mut(|i| {
        i.events
            .retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)));
    });
    app.recording_shortcut = None;
    if key == egui::Key::Escape && modifiers.is_none() {
        return;
    }
    match shortcuts::format(modifiers, key) {
        Some(sc) => apply(app, ctx, serde_json::json!({"id": id, "shortcut": sc})),
        None => app.toast_for(ctx, crate::i18n::tr("That key can't be used as a shortcut"), 3.0),
    }
}
