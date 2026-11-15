//! The fork's UI commands, kept out of the shared `menus.rs` (U.5): the command tables of the
//! fork's modules, their dispatch and enabled state, and the fork's own UI command arms. A child
//! module of `menus`; `ui_commands`, `run_ui_command` and `ui_enabled` call in here once each.

use serde_json::{Value, json};

use super::UiCommand;
use crate::LccApp;
use crate::pick::{PickRequest, Picked};
use crate::state::{Dialog, ViewMode};

/// The fork's command tables, after upstream's.
pub(super) fn commands() -> impl Iterator<Item = &'static UiCommand> {
    crate::module::SHELL_COMMANDS
        .iter()
        .chain(EDIT_SECTIONS)
        .chain(crate::map::COMMANDS)
        .chain(crate::print_ui::COMMANDS)
        .chain(crate::creations_ui::COMMANDS)
        .chain(PLUGIN_COMMANDS)
        .chain(crate::timelapse_ui::COMMANDS)
        .chain(crate::email_photo::COMMANDS)
        .chain(crate::panels::filterbar::COMMANDS)
        .chain(crate::import_report::COMMANDS)
        .chain(crate::duplicates_ui::COMMANDS)
        .chain(crate::comments_ui::COMMANDS)
        .chain(crate::face_menu::COMMANDS)
        .chain(crate::modern::COMMANDS)
        .chain(crate::modern_library::COMMANDS)
        .chain(crate::modern_sheets::COMMANDS)
        .chain(GRADING_COMMANDS)
        .chain(crate::tool_profiles::COMMANDS)
        .chain(crate::quick_bar::COMMANDS)
        .chain(crate::help_link::COMMANDS)
        .chain(crate::lrc_menus::SOON_COMMANDS.iter())
}

/// The Edit panel's grading modes: Eboda, the Lightroom Classic panels (the default), and
/// Witchcraft, the DaVinci Resolve style colour-grading palettes. `view.gradingMode` cycles, or
/// takes `{mode: eboda|witchcraft}`; the Edit panel's header shows the same choice.
const GRADING_COMMANDS: &[UiCommand] = &[("view.gradingMode", "Grading Mode", Some("Cmd+Shift+G"), "Window>Grading Mode")];

/// The Edit panel's sections the fork adds: Transform (Upright and the transform sliders, upstream's Geometry in the
/// Crop tool), as Lightroom Classic's Transform panel, and Plug-in Filters (the photo's pixel-filter stack). Opened and
/// closed like the others (`section.*`).
const EDIT_SECTIONS: &[UiCommand] = &[
    ("section.curve", "Tone Curve", Some("Cmd+2"), "Window>Edit Sections"),
    ("section.mixer", "Color Mixer", Some("Cmd+3"), "Window>Edit Sections"),
    ("section.grading", "Color Grading", Some("Cmd+4"), "Window>Edit Sections"),
    ("section.primaries", "Primaries", None, "Window>Edit Sections"),
    ("section.hsl", "HSL Curves", None, "Window>Edit Sections"),
    ("section.geometry", "Transform", Some("Cmd+7"), "Window>Edit Sections"),
    ("section.pluginFilters", "Plug-in Filters", None, "Window>Edit Sections"),
];

/// P4.3: the Plug-in Manager (`crate::panels::plugins`); native only, so the web build lists none.
#[cfg(not(target_arch = "wasm32"))]
const PLUGIN_COMMANDS: &[UiCommand] = &[
    // Lightroom Classic's key for its Plug-in Manager (Ctrl+Alt+Shift+, on Windows and Linux)
    ("plugins.manager", "Plug-in Manager…", Some("Cmd+Alt+Shift+,"), "File>Plug-ins"),
    ("plugins.install", "Install Plug-in…", None, ""),
    ("plugins.runItem", "Run Plug-in Command", None, ""),
    ("plugins.page", "Plug-in Settings", None, ""),
    ("plugins.showFolder", "Show Plug-ins Folder", None, ""),
    ("plugins.openLink", "Open Plug-in Link", None, ""),
];
#[cfg(target_arch = "wasm32")]
const PLUGIN_COMMANDS: &[UiCommand] = &[];

/// The fork's modules' commands, then the fork's own UI command arms.
pub(super) fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    // a greyed "Coming soon" menu item never runs anything
    if let Some(c) = crate::lrc_menus::coming_soon(id) {
        return Some(Err(format!("{} is coming soon", c.label)));
    }
    if let Some(r) = crate::module::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::catalog_ui::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::tool_profiles::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::modern::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::modern_library::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::modern_sheets::run(app, id, p) {
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
    if let Some(r) = crate::timelapse_ui::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::email_photo::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::panels::filterbar::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::face_menu::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::import_report::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::duplicates_ui::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::comments_ui::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::libtools::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::quick_bar::run(app, id, p) {
        return Some(r);
    }
    if let Some(r) = crate::help_link::run(app, id, p) {
        return Some(r);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(r) = crate::panels::tether_bar::run(app, id, p) {
        return Some(r);
    }
    Some(match id {
        // the fork's Edit sections, opened and closed as upstream's are (⌥-click: just this one)
        "section.curve"
        | "section.mixer"
        | "section.grading"
        | "section.primaries"
        | "section.hsl"
        | "section.geometry"
        | "section.pluginFilters" => {
            let sec = &id["section.".len()..];
            app.ui.right = crate::state::RightPanel::Edit;
            if p.get("solo").and_then(Value::as_bool) == Some(true) {
                app.ui.solo_section(sec);
            } else {
                app.ui.toggle_section(sec);
            }
            Ok(Value::Null)
        }
        "view.gradingMode" => {
            // {mode?: eboda|witchcraft}; without one, cycles Eboda → Witchcraft → Eboda
            let mode = match p.get("mode") {
                None | Some(Value::Null) => app.ui.settings.grading_mode.next(),
                Some(v) => match v.as_str().and_then(crate::state::GradingMode::parse) {
                    Some(m) => m,
                    None => return Some(Err(format!("view.gradingMode: unknown mode {v} (eboda|witchcraft)"))),
                },
            };
            app.ui.settings.grading_mode = mode;
            Ok(json!({"mode": mode.id()}))
        }
        "view.colorReadout" | "view.colorReadoutRgb" | "view.colorReadoutHsl" | "view.colorReadoutLab" => {
            // {format?: rgb|hsl|lab}; `view.colorReadout` without one cycles RGB → HSL → Lab
            use crate::color_readout::ColorReadout;
            let format = match (id, p.get("format")) {
                ("view.colorReadoutRgb", _) => ColorReadout::Rgb,
                ("view.colorReadoutHsl", _) => ColorReadout::Hsl,
                ("view.colorReadoutLab", _) => ColorReadout::Lab,
                (_, None | Some(Value::Null)) => app.ui.settings.color_readout.next(),
                (_, Some(v)) => match v.as_str().and_then(ColorReadout::parse) {
                    Some(f) => f,
                    None => return Some(Err(format!("view.colorReadout: unknown format {v} (rgb|hsl|lab)"))),
                },
            };
            app.ui.settings.color_readout = format;
            Ok(json!({"format": format.id()}))
        }
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
                app.ui.grid_style = ViewMode::SquareGrid;
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
                        lcc_develop::SpotMode::Remove => "heal",
                        lcc_develop::SpotMode::Heal => "clone",
                        lcc_develop::SpotMode::Clone => "fill",
                        lcc_develop::SpotMode::Fill | lcc_develop::SpotMode::Generate | lcc_develop::SpotMode::Flat => "remove",
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
                    let name = format!("{} Keymap.json", lcc_brand::DISPLAY_NAME);
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
pub(super) fn enabled(app: &LccApp, id: &str) -> Option<bool> {
    if crate::lrc_menus::is_coming_soon(id) {
        return Some(false);
    }
    crate::module::enabled(app, id)
        .or_else(|| crate::catalog_ui::enabled(app, id))
        .or_else(|| crate::timelapse_ui::enabled(app, id))
        .or_else(|| crate::email_photo::enabled(app, id))
        .or_else(|| crate::import_report::enabled(app, id))
        .or_else(|| crate::duplicates_ui::enabled(app, id))
        .or_else(|| crate::comments_ui::enabled(app, id))
        .or_else(|| crate::face_menu::enabled(app, id))
}
