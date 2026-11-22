//! Who owns the keyboard this frame. Keys reach commands two ways: the in-window shortcut handler
//! ([`crate::shortcuts::handle`]) and, on macOS, the native menu bar, which AppKit hands every ⌘ / ⌃ /
//! F-key accelerator before the window sees it. Both ask [`owner`], so a modal (a dialog, Settings,
//! the ⌘/ shortcuts overlay) keeps the keys from the photos behind it whichever way they come.
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
    /// A dialog (Settings included) or the shortcuts overlay: every key but [`PASS_THROUGH`] is
    /// its own, or nobody's.
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

/// A dialog (Settings is one) or the ⌘/ shortcuts overlay is up.
pub fn modal_open(app: &LccApp) -> bool {
    app.ui.dialog.is_some() || app.help.open
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
