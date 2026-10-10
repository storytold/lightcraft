//! The Classic-style module help (P6.3): ⌘/ in the Classic keymap lays a translucent sheet over the
//! window listing the current module's shortcuts, as Lightroom Classic does per module. Every
//! module binds the key in its [`crate::module::Module::keymap`] (module keys win over the global
//! keymap in the Classic set), and the sheet lists that module's own keys first, then the global
//! shortcuts that matter in it ([`crate::panels::keymap::in_module`]). Any click or key closes it;
//! "All Shortcuts…" opens the editable Help ▸ Keyboard Shortcuts dialog (`app.shortcuts`), which
//! stays on ⌘/ in the Alternative set.
//!
//! `module.help {"show": bool?}` toggles (or sets) it and reports what it lists.

use egui::{Align2, Color32, RichText, Sense, vec2};
use serde_json::{Value, json};

use crate::DacApp;
use crate::i18n::tr;
use crate::module::{ModuleId, ModuleKey};
use crate::theme::Tokens;
use crate::widgets::register;

/// The command that shows the sheet.
pub const COMMAND: &str = "module.help";
/// Its key in every module (Classic set).
pub const KEY: ModuleKey = ("Cmd+/", COMMAND, "{}");

/// Whether the sheet is up.
#[derive(Debug, Default)]
pub struct HelpOverlay {
    pub open: bool,
    /// Opened this frame: the key or click that opened it doesn't close it.
    fresh: bool,
}

/// A shortcut line: (keys as the menus show them, what they do).
pub type Row = (String, String);

/// The module's own keys (without the help key itself), then the global shortcuts in effect there.
pub fn rows(app: &DacApp, module: ModuleId, mac: bool) -> (Vec<Row>, Vec<Row>) {
    let label = |id: &str| -> String {
        let source = match id {
            "develop.wb" => "Auto White Balance",
            "develop.auto" => "Auto Tone",
            _ => crate::shortcuts::find_bindable(id).map_or(id, |b| b.label),
        };
        tr(source).to_string()
    };
    let own: Vec<Row> = crate::module::get(module)
        .keymap()
        .iter()
        .filter(|(_, id, _)| *id != COMMAND)
        .map(|(sc, id, _)| (crate::menubar::shortcut_text(sc, mac), label(id)))
        .collect();
    let taken: Vec<String> = crate::module::get(module).keymap().iter().map(|(sc, _, _)| crate::menubar::shortcut_text(sc, mac)).collect();
    let mut global: Vec<Row> = crate::shortcuts::bindable()
        .iter()
        .filter(|b| b.id != COMMAND && crate::panels::keymap::in_module(module, b.id))
        .filter_map(|b| {
            crate::shortcuts::binding(&app.ui.settings.keymap, b.id, b.default())
                .map(|sc| (crate::menubar::shortcut_text(sc, mac), tr(b.label).to_string()))
        })
        // a module key wins over the global binding of the same key
        .filter(|(sc, _)| !taken.contains(sc))
        .collect();
    global.sort_by_cached_key(|r| r.1.to_lowercase());
    global.dedup();
    (own, global)
}

/// `module.help`.
pub fn run(app: &mut DacApp, p: &Value) -> Result<Value, String> {
    let show = p.get("show").and_then(Value::as_bool).unwrap_or(!app.help.open);
    app.help.open = show;
    app.help.fresh = show;
    let (own, global) = rows(app, app.ui.module, cfg!(target_os = "macos"));
    Ok(json!({"open": show, "module": app.ui.module.key(), "moduleKeys": own.len(), "globalKeys": global.len()}))
}

/// Draw the sheet when it is up; a click or key press (after the frame that opened it) closes it.
pub fn show(app: &mut DacApp, ctx: &egui::Context) {
    if !app.help.open {
        return;
    }
    let fresh = std::mem::take(&mut app.help.fresh);
    let t = Tokens::get(ctx);
    let mac = ctx.os() == egui::os::OperatingSystem::Mac;
    let module = app.ui.module;
    let (own, global) = rows(app, module, mac);
    let screen = ctx.content_rect();
    let mut all_clicked = false;
    let mut dismissed = false;
    egui::Area::new(egui::Id::new("module-help")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let veil = ui.interact(screen, egui::Id::new("module-help-veil"), Sense::click());
        crate::access::button(&veil, "Close");
        register(ui.ctx(), "overlay:moduleHelp", screen);
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(238));
        let sheet = screen.shrink2(vec2((screen.width() * 0.08).max(16.0), (screen.height() * 0.08).max(16.0)));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(sheet).layout(egui::Layout::top_down(egui::Align::Min)));
        let ui = &mut child;
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr(module.label())).font(t.semibold(24.0)).color(Color32::WHITE));
            ui.label(RichText::new(tr("Shortcuts")).font(t.font(24.0)).color(Color32::from_gray(200)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.button(tr("All Shortcuts…"));
                register(ui.ctx(), "button:moduleHelp.all", r.rect);
                all_clicked = r.clicked();
            });
        });
        ui.add_space(14.0);
        let section = |ui: &mut egui::Ui, title: &str, rows: &[Row]| {
            ui.label(RichText::new(tr(title)).font(t.semibold(14.0)).color(Color32::from_gray(230)));
            ui.add_space(4.0);
            for (keys, what) in rows {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(150.0, 16.0), Sense::hover());
                    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, keys, t.font(12.5), Color32::WHITE);
                    ui.label(RichText::new(what).size(12.5).color(Color32::from_gray(190)));
                });
            }
            ui.add_space(10.0);
        };
        egui::ScrollArea::vertical().id_salt("module-help-scroll").auto_shrink([false, false]).max_height(ui.available_height()).show(ui, |ui| {
            let n = 3usize;
            let per = global.len().div_ceil(n).max(1);
            ui.columns(n, |cols| {
                if let Some(c) = cols.first_mut() {
                    if own.is_empty() {
                        c.label(RichText::new(tr("This module has no keys of its own.")).size(12.0).color(Color32::from_gray(160)));
                        c.add_space(10.0);
                    } else {
                        section(c, "This Module", &own);
                    }
                }
                for (i, chunk) in global.chunks(per).enumerate() {
                    if let Some(c) = cols.get_mut(i) {
                        section(c, if i == 0 { "Everywhere" } else { " " }, chunk);
                    }
                }
            });
        });
        if !fresh {
            let key = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })));
            dismissed = veil.clicked() || key;
        }
    });
    if all_clicked {
        app.help.open = false;
        let _ = app.run("app.shortcuts", json!({}));
    } else if dismissed {
        app.help.open = false;
    }
}
