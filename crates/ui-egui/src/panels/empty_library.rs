//! A library with no photos yet: what the grid and the Detail view show instead of a dead end,
//! with Import Photos… and Import Lightroom Catalog… as buttons, and the view it opens on.

use egui::Rect;
use serde_json::json;

use super::EmptyAction;
use crate::LightcraftApp;
use crate::shortcuts::Keymap;
use crate::state::ViewMode;

/// The library holds no photos: none, or only ones in Recently Deleted or browsed under Local.
pub fn is_empty(session: &lightcraft_engine::Session) -> bool {
    !session.catalog.photos().any(|p| p.in_library())
}

/// What is shown is the empty library: not a folder browsed under Local nor Recently Deleted,
/// whose photos aren't in the library but are on screen.
pub fn shown(session: &lightcraft_engine::Session) -> bool {
    use lightcraft_engine::LibrarySource;
    !matches!(session.source, LibrarySource::Folder | LibrarySource::RecentlyDeleted) && is_empty(session)
}

/// At launch, an empty library opens on a grid with the sidebar, where the way in is (Detail had
/// only "Choose a photo in the grid" to say). Square Grid stays as it is; a library with photos
/// opens as left. The desktop host calls this before it notes what its settings file holds, so
/// the switch isn't saved for its own sake.
pub fn open_on_empty_library(app: &mut LightcraftApp) {
    if is_empty(&app.session) {
        if app.ui.view != ViewMode::SquareGrid {
            app.ui.view = ViewMode::PhotoGrid;
        }
        app.ui.left_panel = true;
    }
}

/// The line under the buttons: dropping works anywhere in the window, and the shortcut is the
/// one Import Photos… is bound to now, written as menus write it.
pub fn hint(keymap: &Keymap, mac: bool) -> String {
    // the browser build reads dropped files, not folders
    let drop = if cfg!(target_arch = "wasm32") {
        crate::i18n::tr("or drop photos into this window")
    } else {
        crate::i18n::tr("or drop photos and folders into this window")
    };
    match crate::shortcuts::shortcut_of(keymap, "file.addPhotos") {
        Some(sc) => format!("{drop} · {}", crate::menubar::shortcut_text(sc, mac)),
        None => drop.to_string(),
    }
}

const IMPORT_PHOTOS: EmptyAction = EmptyAction { command: "file.addPhotos", label: "Import Photos…", widget: "button:emptyImportPhotos" };
const IMPORT_LIGHTROOM: EmptyAction =
    EmptyAction { command: "file.importLightroom", label: "Import Lightroom Catalog…", widget: "button:emptyImportLightroom" };

/// The empty library's message and buttons, centred in `rect`. A click runs the command as its
/// menu item does; a refusal shows as a toast.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, rect: Rect) {
    // the browser build can't read a Lightroom catalog from disk: only offer what works there
    let actions: &[EmptyAction] = if cfg!(target_arch = "wasm32") { &[IMPORT_PHOTOS] } else { &[IMPORT_PHOTOS, IMPORT_LIGHTROOM] };
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    let hint = hint(&app.ui.settings.keymap, mac);
    let clicked = super::empty_state(
        ui,
        rect,
        "Your library is empty",
        "Photos you import show up here. Nothing is copied or moved unless you choose to.",
        actions,
        Some(&hint),
    );
    if let Some(command) = clicked
        && let Err(e) = app.run(command, json!({}))
    {
        app.toast_error(ui.ctx(), e);
    }
}
