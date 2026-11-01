//! UI-level commands (views, panels, zoom, tools, dialogs) and the menu model shared by the native
//! menu bar, the shortcut handler and the control channel.

use serde::Serialize;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::pick::{PickRequest, Picked};
use crate::state::{BeforeAfter, Dialog, RightPanel, ViewMode, Zoom};

mod files;
mod run;
mod ui_commands;

pub use files::{confirm_delete, delete_in_album, reveal_label, show_in_folders_enabled};
pub use run::{parse_rgb, run_ui_command};
pub use ui_commands::{LANGUAGE_COMMANDS, UI_COMMANDS, language_from_command, ui_commands};

pub mod menu_level;
pub mod menubar;

/// (id, label, shortcut, menu path)
pub type UiCommand = (&'static str, &'static str, Option<&'static str>, &'static str);

/// Views whose bottom bar has the thumbnail-size slider and that draw tiles of that size.
fn thumb_size_view(app: &LightcraftApp) -> bool {
    matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid | ViewMode::People)
}

pub fn ui_enabled(app: &LightcraftApp, id: &str) -> bool {
    match id {
        "library.selectOnlyActive" => app.session.active().is_some(),
        "view.thumbSizeUp" | "view.thumbSizeDown" => thumb_size_view(app),
        "dialog.contactSheet" | "app.contactSheet" | "dialog.print" | "app.print" => {
            !cfg!(target_arch = "wasm32") && app.session.active().is_some() && app.export.is_none()
        }
        "dialog.slideshowExport" | "app.slideshowExport" => {
            !cfg!(target_arch = "wasm32") && !app.session.slideshow_export.as_ref().is_some_and(|r| r.running())
        }
        s if s.starts_with("panel.") || s.starts_with("tool.") || s.starts_with("section.") => app.session.active().is_some() || s == "panel.close",
        "app.export" | "dialog.export" | "dialog.createPreset" | "dialog.rename" | "dialog.captureTime" | "dialog.copySettings" => {
            app.session.active().is_some()
        }
        "photo.tagFromTracklog" => app.session.active().is_some() && app.services.pick_tracklog.is_some(),
        "photo.editInPhotoCraft" | "photo.editInExternal" => app.session.active().is_some() && app.services.open_with.is_some(),
        "app.exportPrevious" => app.session.active().is_some() && app.session.last_export.is_some(),
        "dialog.pasteSettings" => app.session.active().is_some() && app.session.clipboard.is_some(),
        "photo.openOriginalInExternal" => {
            // only a file type LightCraft reads can be handed over (#2135); the extension alone
            // decides, so no disk or share is touched while a menu is drawn
            app.services.open_with.is_some()
                && app.session.active().and_then(|id| app.session.catalog.photo(id)).is_some_and(|p| match &p.source {
                    lightcraft_engine::catalog::Source::File { path } => lightcraft_engine::open_path::is_media(std::path::Path::new(path)),
                    _ => false,
                })
        }
        "app.showInFinder" => {
            app.services.reveal.is_some()
                && app
                    .session
                    .active()
                    .and_then(|id| app.session.catalog.photo(id))
                    .is_some_and(|p| matches!(p.source, lightcraft_engine::catalog::Source::File { .. }))
        }
        "app.showInFolders" => show_in_folders_enabled(app, None),
        "app.openSamModelFolder" => app.session.segmenter.dir.is_some() && app.services.open_with.is_some(),
        "file.exportPresets" => app.session.presets.iter().any(|p| !p.builtin),
        "file.exportMaskEffects" => !app.session.mask_effects.is_empty(),
        "dialog.saveMaskEffect" => app.session.active().is_some() && app.session.active_mask.is_some(),
        "file.exportCurvePresets" => !app.session.curve_presets.is_empty(),
        "view.compare" => app.session.catalog.len() > 1,
        "view.fullScreenPreview" | "view.infoOverlay" | "view.navigator" => app.session.active().is_some() || app.ui.fullscreen,
        "app.openLibrary" | "app.newLibrary" => app.services.pick_folder.is_some() && crate::panels::settings::can_switch_library(app).is_ok(),
        "file.addFolder" => app.services.pick_folder.is_some() && !crate::lightroom_import::is_running(app),
        "file.backupLibrary" => app.services.backup_library.is_some(),
        "app.openLogFolder" => app.services.reveal.is_some() && app.services.log_file.is_some(),
        "file.restoreLibrary" => app.services.restore_library.is_some(),
        "app.clearRecentLibraries" => !app.ui.settings.recent_libraries.is_empty(),
        "compare.swap" | "compare.makeSelect" | "compare.set" => app.ui.view == ViewMode::Compare,
        "slideshow.togglePause" | "slideshow.next" | "slideshow.previous" | "slideshow.end" => app.ui.slideshow.is_some(),
        s if s.starts_with("dialog.merge") || (s.starts_with("merge.") && s.ends_with("Last")) => {
            app.session.selected().len() >= 2 && app.merge.final_task.is_none()
        }
        _ => true,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MenuEntry {
    pub id: String,
    pub label: String,
    pub menu: Vec<String>,
    pub shortcut: Option<String>,
    pub enabled: bool,
}

/// The flattened menu model (UI commands + engine commands with menu paths).
pub fn menu_entries(app: &LightcraftApp) -> Vec<MenuEntry> {
    let mut v: Vec<MenuEntry> = ui_commands()
        .filter(|c| !c.3.is_empty())
        .map(|(id, label, sc, m)| MenuEntry {
            id: id.to_string(),
            label: label.to_string(),
            menu: m.split('>').map(str::to_string).collect(),
            shortcut: crate::shortcuts::binding(&app.ui.settings.keymap, id, *sc).map(str::to_string),
            enabled: ui_enabled(app, id),
        })
        .collect();
    for c in app.session.commands() {
        if !c.menu.is_empty() {
            v.push(MenuEntry {
                id: c.id.into(),
                label: c.label.into(),
                menu: c.menu.iter().map(|s| s.to_string()).collect(),
                shortcut: crate::shortcuts::shortcut_of(&app.ui.settings.keymap, c.id).map(str::to_string),
                enabled: c.enabled,
            });
        }
    }
    v
}
