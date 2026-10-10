//! The fork's UI commands, kept out of the shared `menus.rs` (U.5): the command tables of the
//! fork's modules, their dispatch and enabled state, and the fork's own UI command arms. A child
//! module of `menus`; `ui_commands`, `run_ui_command` and `ui_enabled` call in here once each.

use serde_json::{Value, json};

use super::UiCommand;
use crate::DacApp;
use crate::pick::{PickRequest, Picked};
use crate::state::{Dialog, ViewMode};

/// The fork's command tables, after upstream's.
pub(super) fn commands() -> impl Iterator<Item = &'static UiCommand> {
    crate::module::SHELL_COMMANDS
        .iter()
        .chain(crate::map::COMMANDS)
        .chain(crate::print_ui::COMMANDS)
        .chain(crate::creations_ui::COMMANDS)
        .chain(PLUGIN_COMMANDS)
}

/// P4.3: the Plug-in Manager (`crate::panels::plugins`); native only, so the web build lists none.
#[cfg(not(target_arch = "wasm32"))]
const PLUGIN_COMMANDS: &[UiCommand] = &[
    ("plugins.manager", "Plug-in Manager…", None, "File"),
    ("plugins.install", "Install Plug-in…", None, ""),
    ("plugins.runItem", "Run Plug-in Command", None, ""),
];
#[cfg(target_arch = "wasm32")]
const PLUGIN_COMMANDS: &[UiCommand] = &[];

/// The fork's modules' commands, then the fork's own UI command arms.
pub(super) fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if let Some(r) = crate::module::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::catalog_ui::run(app, id, p) {
        return Some(r);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(r) = crate::panels::plugins::run(app, id, p) {
        return Some(r);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(r) = crate::panels::connections::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::libtools::run(app, id, p) {
        return Some(r);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(r) = crate::panels::tether_bar::run(app, id, p) {
        return Some(r);
    }
    Some(match id {
        "view.zoomLevel" => {
            // {level: fit|fill|1:4|1:3|1:2|1:1|2:1|3:1|4:1|8:1|11:1} → {zoom}
            const LEVELS: &str = "fit, fill, 1:4, 1:3, 1:2, 1:1, 2:1, 3:1, 4:1, 8:1 or 11:1";
            let Some(level) = p.get("level").and_then(Value::as_str) else {
                return Some(Err(format!("view.zoomLevel: level is required ({LEVELS})")));
            };
            let Some(z) = crate::state::zoom_level(level) else {
                return Some(Err(format!("view.zoomLevel: unknown level `{level}` ({LEVELS})")));
            };
            app.ui.zoom = z;
            app.ui.zoom_anim = true;
            Ok(json!({"zoom": z}))
        }
        "dialog.viewOptions" => {
            app.ui.dialog = Some(Dialog::ViewOptions);
            Ok(Value::Null)
        }
        "view.cellCompact" | "view.cellExpanded" => {
            let style = if id == "view.cellCompact" { "compact" } else { "expanded" };
            let r = app.run("view.gridCellStyle", json!({"style": style}));
            if r.is_ok() && !matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid) {
                app.ui.view = ViewMode::SquareGrid;
            }
            r
        }
        "view.cellIndex" => {
            let on = !app.ui.lib.cell_index;
            app.run("view.gridCellStyle", json!({"index": on}))
        }
        "view.cellBadges" => {
            let on = !app.ui.lib.cell_badges;
            app.run("view.gridCellStyle", json!({"badges": on}))
        }
        "view.thumbLarger" | "view.thumbSmaller" => {
            let k = if id == "view.thumbLarger" { 1.15 } else { 1.0 / 1.15 };
            app.ui.thumb_size = (app.ui.thumb_size * k).clamp(90.0, 480.0);
            Ok(json!({"thumbSize": app.ui.thumb_size}))
        }
        "library.first" | "library.last" => {
            let vis = app.session.visible_cloned();
            let pick = if id == "library.first" { vis.first() } else { vis.last() };
            match pick {
                Some(pid) => app.run("library.select", json!({"ids": [pid.0]})).map(|_| json!({"active": pid.0})),
                None => Ok(json!({"active": null})),
            }
        }
        "spot.cycleMode" => {
            let cur = app.session.active_spot.and_then(|i| {
                let d = app.session.active().and_then(|a| app.session.develop_of(a))?;
                d.spots.get(i).map(|s| (i, s.mode))
            });
            match cur {
                None => Err("select a spot first".to_string()),
                Some((i, mode)) => {
                    let next = match mode {
                        dac_develop::SpotMode::Remove => "heal",
                        dac_develop::SpotMode::Heal => "clone",
                        dac_develop::SpotMode::Clone => "remove",
                    };
                    app.run("spot.update", json!({"index": i, "mode": next})).map(|_| json!({"mode": next}))
                }
            }
        }
        "app.keymapSet" => crate::shortcuts::choose_set(app, p),
        "app.keymapExport" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => x.to_string(),
                None => {
                    let name = format!("{} Keymap.json", dac_brand::DISPLAY_NAME);
                    let req = PickRequest::save(crate::i18n::tr("Export Keymap"), crate::i18n::tr("Keymap"), &["json"], name);
                    match crate::pick::ask(app, id, p, "path", req, |_| None) {
                        Picked::Now(v) => match v.into_iter().next() {
                            Some(x) => x,
                            None => return Some(Ok(Value::Null)),
                        },
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform: pass {path}".into())),
                    }
                }
            };
            crate::shortcuts::export_file(app, &path)
        }
        "app.keymapImport" => {
            let path = match p.get("path").and_then(Value::as_str) {
                Some(x) => x.to_string(),
                None => {
                    let req = PickRequest::file(crate::i18n::tr("Import Keymap"), crate::i18n::tr("Keymap"), &["json"]);
                    match crate::pick::ask(app, id, p, "path", req, |_| None) {
                        Picked::Now(v) => match v.into_iter().next() {
                            Some(x) => x,
                            None => return Some(Ok(Value::Null)),
                        },
                        Picked::Later => return Some(Ok(Value::Null)),
                        Picked::Unavailable => return Some(Err("no file dialog on this platform: pass {path}".into())),
                    }
                }
            };
            crate::shortcuts::import_file(app, &path)
        }
        _ => return None,
    })
}

/// Enabled state of the fork's modules' commands.
pub(super) fn enabled(app: &DacApp, id: &str) -> Option<bool> {
    crate::module::enabled(app, id).or_else(|| crate::catalog_ui::enabled(app, id))
}
