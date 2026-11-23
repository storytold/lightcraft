//! Who owns the keyboard this frame. Keys reach commands two ways: the in-window shortcut handler
//! ([`crate::shortcuts::handle`]) and, on macOS, the native menu bar, which AppKit hands every ⌘ / ⌃ /
//! F-key accelerator before the window sees it. Both ask [`owner`], so a modal (a dialog, Settings,
//! the ⌘/ shortcuts overlay, another window that waits over the app: [`modal_open`]) keeps the
//! keys from the photos behind it whichever way they come.
//!
//! The owner is worked out from app state only: the menu bar and the shortcut handler run in
//! `logic`, before the dialogs and overlays are drawn. A new surface that takes the keyboard is
//! added here, in [`owner`] (and, for a modal, [`modal_open`]).

use crate::LccApp;

/// Who has the keyboard, highest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The keyboard shortcuts editor is recording a key: every key goes to it.
    Recorder,
    /// A dialog (Settings included), the shortcuts overlay or another window that waits over the
    /// app ([`modal_open`]): every key but [`PASS_THROUGH`] is its own, or nobody's.
    Modal,
    /// A widget with the keyboard focus keeps this frame's keys: a text field, a selected slider
    /// (↑ / ↓), a handle on the photo (arrows, Return, Space), a name being edited.
    Widget,
    /// The main window: the shortcuts run.
    Window,
}

/// The menu bar's commands a modal lets through: Quit (⌘Q, which can't be given to another
/// command). ⌘W passes too, as the system's own Close Window, which AppKit runs without asking the
/// app; a command the user binds to ⌘W waits like any other. The in-window shortcut handler lets
/// nothing through a modal.
pub const PASS_THROUGH: &[&str] = &["app.quit"];

/// A dialog (Settings is one), the ⌘/ shortcuts overlay, or another window that sits over the app
/// and waits for it to be closed is up.
pub fn modal_open(app: &LccApp) -> bool {
    // the full-screen preview draws none of the tool windows: they wait, and the keys stay with
    // the photo on show, until it ends
    answer_waiting(app) || !app.ui.fullscreen && tool_window_open(app)
}

/// A modal that isn't a tool window ([`tool_window_open`]): a dialog, a question, the ⌘/ key list,
/// a catalog window, the Export sheet, the Edit In windows, the Immich, sync and tethering ones.
pub(crate) fn answer_waiting(app: &LccApp) -> bool {
    app.ui.dialog.is_some()
        || app.help.open
        // the quit questions and the settings-file notice
        || app.quit_prompt.is_some()
        || !app.notices.is_empty()
        || catalog_dialog_modal(app)
        || app.onboarding.wizard.is_some()
        || library_problem_modal(app)
        // the Modern layout's Export sheet
        || app.modern_sheets.export.is_some()
        || crate::edit_in::modal_open()
        || native_question_open(app)
}

/// Edited Version Saved Again (Immich), Share the New Collection? (sync) and Tethered Capture
/// Settings, which the web app doesn't have.
#[cfg(not(target_arch = "wasm32"))]
fn native_question_open(app: &LccApp) -> bool {
    crate::panels::connections::resave_asked() || crate::panels::peer_sync::question_open(app) || crate::panels::tether_bar::settings_open()
}

#[cfg(target_arch = "wasm32")]
fn native_question_open(_: &LccApp) -> bool {
    false
}

/// Identity Plate Editor, Find Duplicates, Review Comments, Import Warnings, the camera-roll
/// question, the Plug-in Manager, the Plug-in Marketplace and the plug-ins' own windows.
fn tool_window_open(app: &LccApp) -> bool {
    app.ui.identity_plate.setup.is_some()
        || app.duplicates.as_ref().is_some_and(|f| f.open)
        || app.import_report.as_ref().is_some_and(|r| r.open)
        || crate::camera_roll::prompt_due(app)
        || native_window_open(app)
}

/// New Catalog, Catalog Settings, Export as Catalog, Import from Another Catalog, the XMP conflict,
/// Refine Photos. Not Welcome: no library is open behind it, and its way to Settings, About and
/// Check for Updates is the menu bar (issue #929).
#[cfg(not(target_arch = "wasm32"))]
fn catalog_dialog_modal(app: &LccApp) -> bool {
    app.catalog_ui.dialog.as_ref().is_some_and(|d| !matches!(d, crate::catalog_ui::CatalogDialog::Welcome { .. }))
}

/// (the web app has no catalog dialogs)
#[cfg(target_arch = "wasm32")]
fn catalog_dialog_modal(_: &LccApp) -> bool {
    false
}

/// The catalog opening or upgrading, or the catalog chooser over the library open now
/// (`catalog.chooser`). The other windows of a catalog that won't open come at launch with no
/// library behind them, and send the user to the File menu: they leave the menu bar alone.
fn library_problem_modal(app: &LccApp) -> bool {
    // (the order `library_problem::show` draws them in)
    app.library_problem.as_ref().is_some_and(|p| p.opening || p.upgrading || (!p.dismissed && p.chooser.as_ref().is_some_and(|c| c.over.is_some())))
}

/// Review Comments and the plug-in windows, which the web app doesn't have.
#[cfg(not(target_arch = "wasm32"))]
fn native_window_open(app: &LccApp) -> bool {
    app.review_comments.open || plugin_window_open(app)
}

#[cfg(target_arch = "wasm32")]
fn native_window_open(_: &LccApp) -> bool {
    false
}

/// The Plug-in Manager or one of the plug-ins' windows ([`crate::panels::plugins::modal_open`]), or
/// the Plug-in Marketplace ([`crate::panels::marketplace::modal_open`]).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn plugin_window_open(app: &LccApp) -> bool {
    crate::panels::plugins::modal_open(app) || crate::panels::marketplace::modal_open()
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn plugin_window_open(_: &LccApp) -> bool {
    false
}

/// Closes the tool window Back closes as it closes a dialog, when nothing else waits over it:
/// the Identity Plate Editor (cancelled), Import Warnings, Review Comments, Find Duplicates, each
/// as its own Cancel or ✕ closes it. The questions that need an answer, the plug-ins' windows and
/// the Plug-in Marketplace keep their own Esc; in the full-screen preview the tool windows wait
/// unseen, and stay. Returns what it closed.
pub(crate) fn close_tool_window(app: &mut LccApp) -> Option<&'static str> {
    if app.ui.fullscreen || answer_waiting(app) || plugin_window_open(app) || crate::camera_roll::prompt_due(app) {
        return None;
    }
    if app.ui.identity_plate.setup.is_some() && app.run("dialog.identityPlateSetup", serde_json::json!({"close": "cancel"})).is_ok() {
        return Some("identityPlate");
    }
    if let Some(r) = app.import_report.as_mut().filter(|r| r.open) {
        r.open = false;
        return Some("importWarnings");
    }
    #[cfg(not(target_arch = "wasm32"))]
    if app.review_comments.open {
        app.review_comments.open = false;
        return Some("reviewComments");
    }
    if let Some(f) = app.duplicates.as_mut().filter(|f| f.open) {
        f.open = false;
        // (its thumbnails go, as when its ✕ closes it)
        app.renderer.forget_duplicates();
        return Some("findDuplicates");
    }
    None
}

/// Who has the keyboard now, from app state (and, for [`Owner::Widget`], this frame's keys).
pub fn owner(app: &LccApp, ctx: &egui::Context) -> Owner {
    if app.recording_shortcut.is_some() {
        Owner::Recorder
    } else if modal_open(app) {
        Owner::Modal
    } else if app.ui.name_edit.is_some() || crate::shortcuts::keyboard_owned(app, ctx) {
        Owner::Widget
    } else {
        Owner::Window
    }
}

/// What the native menu bar does with an item chosen while `owner` has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuChoice {
    /// Run the item's command.
    Run,
    /// Nothing: the keyboard's owner keeps the key, and a click on the item does nothing either.
    Drop,
    /// A text-editing shortcut (⌘A, ⌘C…) edits the focused text field instead (issue #281).
    TextField,
}

/// `command` is the item's command id, `shortcut` its key (none for an item without one, e.g.
/// About); `text_focus` whether a text field has the keyboard focus.
pub fn menu_choice(owner: Owner, command: &str, shortcut: Option<&str>, text_focus: bool) -> MenuChoice {
    if text_focus && shortcut.is_some_and(|sc| crate::menubar::TEXT_EDIT_SHORTCUTS.contains(&sc)) {
        return MenuChoice::TextField;
    }
    match owner {
        Owner::Recorder | Owner::Modal if !PASS_THROUGH.contains(&command) => MenuChoice::Drop,
        _ => MenuChoice::Run,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modal_drops_menu_shortcuts_but_quit_and_text_editing() {
        for owner in [Owner::Modal, Owner::Recorder] {
            let choice = |command: &str, sc: Option<&str>, focus: bool| menu_choice(owner, command, sc, focus);
            for (command, sc) in [
                ("view.zoomIn", "Cmd+="),
                ("view.zoomOut", "Cmd+-"),
                ("library.deleteRejected", "Cmd+Delete"),
                ("app.settings", "Cmd+,"),
                ("view.toggleLeft", "F7"),
                ("view.grid", "G"),
            ] {
                assert_eq!(choice(command, Some(sc), false), MenuChoice::Drop, "{owner:?} keeps {sc} from the photos behind it");
            }
            assert_eq!(choice("app.about", None, false), MenuChoice::Drop, "{owner:?}: an item without a shortcut (About) waits too");
            assert_eq!(choice("app.quit", Some("Cmd+Q"), false), MenuChoice::Run, "{owner:?}: Quit works over a modal");
            assert_eq!(choice("view.zoomIn", Some("Cmd+W"), false), MenuChoice::Drop, "{owner:?}: a command the user put on ⌘W waits too");
            assert_eq!(choice("edit.copy", Some("Cmd+C"), true), MenuChoice::TextField, "{owner:?}: ⌘C copies from the modal's text field");
            assert_eq!(choice("edit.copy", Some("Cmd+C"), false), MenuChoice::Drop, "{owner:?}: with no field focused, ⌘C waits");
            assert_eq!(choice("view.zoomIn", Some("Cmd+="), true), MenuChoice::Drop, "{owner:?}: a field doesn't let other shortcuts out");
        }
    }

    #[test]
    fn the_window_runs_menu_shortcuts_and_a_text_field_keeps_its_editing_keys() {
        for owner in [Owner::Window, Owner::Widget] {
            let choice = |command: &str, sc: Option<&str>, focus: bool| menu_choice(owner, command, sc, focus);
            for (command, sc) in [
                ("view.zoomIn", "Cmd+="),
                ("view.zoomOut", "Cmd+-"),
                ("library.deleteRejected", "Cmd+Delete"),
                ("app.settings", "Cmd+,"),
                ("app.quit", "Cmd+Q"),
                ("view.toggleLeft", "F7"),
                ("view.grid", "G"),
            ] {
                assert_eq!(choice(command, Some(sc), false), MenuChoice::Run, "{owner:?} runs {sc}");
                assert_eq!(choice(command, Some(sc), true), MenuChoice::Run, "{owner:?} runs {sc} while typing");
            }
            assert_eq!(choice("app.about", None, false), MenuChoice::Run, "{owner:?}: About opens");
            for sc in crate::menubar::TEXT_EDIT_SHORTCUTS {
                assert_eq!(choice("edit.copy", Some(sc), true), MenuChoice::TextField, "{owner:?}: {sc} edits the field");
                assert_eq!(choice("edit.copy", Some(sc), false), MenuChoice::Run, "{owner:?}: {sc} runs its command with no field focused");
            }
        }
    }
}
