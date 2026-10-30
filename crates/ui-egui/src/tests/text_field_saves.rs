//! An edit in a text box ends exactly once, and every way of leaving it except Esc saves, including
//! the box no longer being drawn: a tab clicked, a command that opens another tab or dialog, a panel
//! hidden by a click, the window closing or Quit.
//!
//! Before: a box reported the end of an edit only from inside its own `show()`, and egui reports a
//! lost focus only to a widget that is drawn, so any route that stopped drawing the box mid-edit
//! dropped the text.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::{Dialog, RightPanel};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1200.0, 900.0], 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn steps(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}

/// The command `app.settings` for `tab`, as a menu item or the control channel runs it.
fn show(h: &mut Headless, tab: &str) {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": tab}}), T);
    assert_eq!(r["ok"], true, "app.settings {tab}: {r}");
    h.settle(T);
    steps(h);
}

fn click(h: &mut Headless, id: &str, button: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id, "button": button}), T);
    assert_eq!(r["ok"], true, "click {id}: {r}");
    steps(h);
}

fn type_into(h: &mut Headless, id: &str, text: &str) {
    click(h, id, "left");
    let r = h.request("ui.text", json!({"text": text}), T);
    assert_eq!(r["ok"], true, "type into {id}: {r}");
    steps(h);
}

fn key(h: &mut Headless, key: egui::Key) {
    for pressed in [true, false] {
        h.events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE });
    }
    steps(h);
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Press the primary button at `at`, step a frame, release it, step a frame: a mouse's click, whose
/// press and release reach the app in separate frames (the control channel's `ui.click` sends both
/// in one).
fn press_and_release(h: &mut Headless, at: egui::Pos2) {
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.extend([egui::Event::PointerMoved(at), button(true)]);
    h.step();
    h.events.push(button(false));
    h.step();
    steps(h);
}

fn settings_open(h: &Headless, tab: &str) -> bool {
    h.app.ui.dialog == Some(Dialog::Settings { tab: tab.into() })
}

fn says(h: &Headless, part: &str) -> bool {
    h.painted_text().iter().any(|text| text.contains(part))
}

/// What Settings ▸ Import holds for `key` (`copyright`, `creator`).
fn import(h: &mut Headless, key: &str) -> Value {
    h.app.run("library.preferences", json!({})).unwrap()["import"][key].clone()
}

/// Given Settings open on Import with `typed` typed into Copyright and not yet ended.
fn typing_in_copyright(typed: &str) -> Headless {
    let mut h = demo();
    show(&mut h, "import");
    type_into(&mut h, "field:settings.copyright", typed);
    assert!(says(&h, typed), "the typing shows in the box");
    assert_eq!(import(&mut h, "copyright"), json!(""), "nothing is saved while the edit is under way");
    h
}

/// A click on another Settings tab (press and release in separate frames, with no box menu open)
/// saves what was typed, and the clicked tab's page is the one drawn.
#[test]
fn a_click_on_another_settings_tab_saves_the_text_typed_in_a_box() {
    let mut h = typing_in_copyright("Example Name");
    let tab = rect(&h, "button:settingsTab-general");
    press_and_release(&mut h, tab.center());
    assert!(settings_open(&h, "general"), "{:?}", h.app.ui.dialog);
    assert!(!has(&h, "field:settings.copyright"), "General's page is drawn, not Import's");
    assert_eq!(import(&mut h, "copyright"), json!("Example Name"), "the text typed in Copyright was dropped when the tab was clicked");
}

/// The menu-open case: the box's own menu is open when the tab is pressed and released.
#[test]
fn a_click_on_a_tab_with_the_boxs_menu_open_saves_the_text() {
    let mut h = typing_in_copyright("Example Name");
    click(&mut h, "field:settings.copyright", "right");
    assert!(egui::Popup::is_any_open(&h.view.ctx), "the box's menu is open");
    let tab = rect(&h, "button:settingsTab-general");
    press_and_release(&mut h, tab.center());
    assert!(settings_open(&h, "general"), "{:?}", h.app.ui.dialog);
    assert_eq!(import(&mut h, "copyright"), json!("Example Name"), "the text was dropped: the menu was open when the tab was clicked");
}

/// A command that opens another tab (a menu item, a shortcut, `app.settings` on the control
/// channel) is applied before the frame is drawn: the page with the box is never drawn again.
#[test]
fn a_command_that_opens_another_settings_tab_saves_the_text_typed_in_a_box() {
    let mut h = typing_in_copyright("Example Name");
    show(&mut h, "general");
    assert!(settings_open(&h, "general"), "{:?}", h.app.ui.dialog);
    assert!(!says(&h, "Copyright") && says(&h, "Language"), "General's page is drawn: {:?}", h.painted_text());
    assert_eq!(import(&mut h, "copyright"), json!("Example Name"), "the text typed in Copyright was dropped when a command opened another tab");
}

#[test]
fn creator_is_saved_when_a_command_opens_another_tab() {
    let mut h = demo();
    show(&mut h, "import");
    type_into(&mut h, "field:settings.creator", "Example Creator");
    show(&mut h, "faces");
    assert!(settings_open(&h, "faces"), "{:?}", h.app.ui.dialog);
    assert_eq!(import(&mut h, "creator"), json!("Example Creator"), "the text typed in Creator was dropped");
}

#[test]
fn slideshow_options_chosen_from_a_menu_saves_the_text_typed_in_a_box() {
    let mut h = typing_in_copyright("Example Name");
    let r = h.request("engine.execute", json!({"command": "view.slideshowOptions", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    steps(&mut h);
    assert!(settings_open(&h, "slideshow"), "{:?}", h.app.ui.dialog);
    assert_eq!(import(&mut h, "copyright"), json!("Example Name"), "the text typed in Copyright was dropped");
}

/// The gap in a fix that listed what a box with the focus takes in: the box's own menu is open, so
/// the focus is on the menu, when the command arrives.
#[test]
fn a_command_with_the_boxs_menu_open_saves_the_text() {
    let mut h = typing_in_copyright("Example Name");
    click(&mut h, "field:settings.copyright", "right");
    assert!(egui::Popup::is_any_open(&h.view.ctx), "the box's menu is open");
    show(&mut h, "general");
    assert!(settings_open(&h, "general"), "{:?}", h.app.ui.dialog);
    assert_eq!(import(&mut h, "copyright"), json!("Example Name"), "the text was dropped: the menu was open when the command came");
}

/// A command for the tab that is open ends no edit.
#[test]
fn a_command_for_the_tab_already_open_ends_no_edit() {
    let mut h = typing_in_copyright("Example Name");
    show(&mut h, "import");
    assert!(says(&h, "Example Name"), "the typing stays in the box");
    assert_eq!(import(&mut h, "copyright"), json!(""), "an unfinished edit is not saved");
}

/// A command that replaces the dialog (View ▸ Slideshow Options from Settings is one; so is any
/// dialog's command) takes the box away too.
#[test]
fn a_command_that_replaces_the_dialog_saves_the_text_typed_in_a_box() {
    let mut h = typing_in_copyright("Example Name");
    let r = h.request("engine.execute", json!({"command": "dialog.newAlbum", "params": {}}), T);
    steps(&mut h);
    assert!(
        h.app.ui.dialog.as_ref().is_some_and(|d| !matches!(d, Dialog::Settings { .. })),
        "another dialog replaced Settings: {r} {:?}",
        h.app.ui.dialog
    );
    assert_eq!(import(&mut h, "copyright"), json!("Example Name"), "the text was dropped when the dialog was replaced");
}

/// Closing the window with an edit in progress saves it, before the question about unsaved changes.
#[test]
fn closing_the_window_saves_the_text_typed_in_a_box() {
    let mut h = typing_in_copyright("Example Name");
    h.request_close();
    h.step();
    assert_eq!(import(&mut h, "copyright"), json!("Example Name"), "the text was dropped when the window was closed");
}

/// Quit (the host's ⌘Q, which asks `may_close` itself rather than closing the viewport) saves it too.
#[test]
fn quitting_saves_the_text_typed_in_a_box() {
    let mut h = typing_in_copyright("Example Name");
    let ctx = h.view.ctx.clone();
    assert!(h.app.may_close_after_saving_edits(&ctx), "nothing is left to ask about once the text is saved");
    assert_eq!(import(&mut h, "copyright"), json!("Example Name"), "the text was dropped when Quit was chosen");
}

/// The Info panel of a selected photo, its Title box typed in and not yet ended.
fn typing_in_title(typed: &str) -> (Headless, u64) {
    let mut h = demo();
    let first = h.app.session.visible_cloned()[0].0;
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = RightPanel::Info;
    h.settle(T);
    steps(&mut h);
    // (the demo photo has a title already: start from none, so typing gives exactly `typed`)
    h.app.run("photo.setMeta", json!({"ids": [first], "title": ""})).unwrap();
    h.settle(T);
    steps(&mut h);
    assert_eq!(title(&h, first), "", "no title yet");
    type_into(&mut h, "field:title", typed);
    assert_eq!(title(&h, first), "", "nothing is saved while the edit is under way");
    (h, first)
}

fn title(h: &Headless, photo: u64) -> String {
    h.app.session.catalog.photo(lightcraft_catalog::PhotoId(photo)).unwrap().meta.title.clone()
}

/// A click that hides the Info panel (its button in the tool strip, drawn before the panel) saves
/// the text typed in one of its boxes.
#[test]
fn hiding_the_info_panel_saves_the_text_typed_in_one_of_its_boxes() {
    let (mut h, photo) = typing_in_title("Rome");
    let button = rect(&h, "icon:info");
    press_and_release(&mut h, button.center());
    assert_eq!(h.app.ui.right, RightPanel::None, "the Info panel is hidden");
    assert!(!has(&h, "field:title"), "the box is not drawn");
    assert_eq!(title(&h, photo), "Rome", "the text typed in the Info panel was dropped when the panel was hidden");
}

/// A command that switches the view (here the control channel's, applied before layout) does.
#[test]
fn a_command_that_changes_the_view_saves_the_text_typed_in_an_info_box() {
    let (mut h, photo) = typing_in_title("Rome");
    let r = h.request("engine.execute", json!({"command": "panel.info", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h);
    assert!(!has(&h, "field:title"), "the box is not drawn");
    assert_eq!(title(&h, photo), "Rome", "the text typed in the Info panel was dropped by the command");
}

/// A change that leaves no active photo (Select by ▸ Rejects with none rejected, from the menu bar
/// or the control channel) hides the Info panel before the sweep saves: the save goes to the photo
/// the edit was for, whatever the selection the change left.
#[test]
fn a_change_that_leaves_no_active_photo_still_saves_the_text_typed_in_an_info_box() {
    let (mut h, photo) = typing_in_title("Rome");
    // (nothing rejected, so Select by ▸ Rejects selects nothing; the demo library rejects some)
    let all: Vec<u64> = h.app.session.visible_cloned().iter().map(|id| id.0).collect();
    h.app.run("photo.unflag", json!({"ids": all})).unwrap();
    steps(&mut h);
    assert!(has(&h, "field:title"), "the box is still drawn, its edit under way");
    let r = h.request("engine.execute", json!({"command": "library.selectBy", "params": {"flag": "reject"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h);
    assert!(h.app.session.selected().is_empty(), "the change left nothing selected and no active photo");
    assert!(!has(&h, "field:title"), "the Info panel is not drawn");
    assert_eq!(title(&h, photo), "Rome", "the text typed in the Info panel was lost: the save was refused with no photos selected");
}

#[test]
fn closing_the_window_saves_the_text_typed_in_an_info_box() {
    let (mut h, photo) = typing_in_title("Rome");
    h.request_close();
    h.step();
    assert_eq!(title(&h, photo), "Rome", "the text typed in the Info panel was dropped when the window was closed");
}

/// A flush whose window then stays open (the library switch that fails, a Quit that is not
/// followed through) leaves the box drawn and focused: its edit is over, as the save ran. Esc has
/// nothing to discard, and clicking away or Return save nothing a second time (that would add an
/// undo step that changes nothing, so the first ⌘Z seems to do nothing).
#[test]
fn a_flush_ends_the_edit_whose_box_stays_drawn() {
    for ending in ["click away", "return", "esc"] {
        let (mut h, photo) = typing_in_title("Rome");
        let undo = h.app.session.undo.len();
        let ctx = h.view.ctx.clone();
        h.app.save_unfinished_edits(&ctx).unwrap();
        assert_eq!(title(&h, photo), "Rome", "{ending}: the flush saved the text");
        for _ in 0..3 {
            h.step();
        }
        assert!(has(&h, "field:title"), "{ending}: the box is still drawn");
        assert!(!crate::text_field::any_unfinished(&ctx), "{ending}: the saved edit is not recorded again");
        match ending {
            "esc" => key(&mut h, egui::Key::Escape),
            "return" => key(&mut h, egui::Key::Enter),
            _ => {
                let at = egui::pos2(h.size.x - 20.0, h.size.y - 60.0);
                press_and_release(&mut h, at);
            }
        }
        for _ in 0..5 {
            h.step();
        }
        assert_eq!(title(&h, photo), "Rome", "{ending}: the text stays");
        assert_eq!(h.app.session.undo.len(), undo + 1, "{ending}: and is saved once");
    }
}

/// The active photo changes under a box that stays drawn (Select by, an agent's `library.select`):
/// the text typed is saved to the photo it was typed for, and the box shows the new photo's value,
/// not the old photo's draft, which Return would save to the old photo.
#[test]
fn a_change_of_the_active_photo_mid_edit_saves_to_the_photo_typed_for_and_shows_the_new_ones_text() {
    let (mut h, a) = typing_in_title("Rome");
    let b = h.app.session.visible_cloned()[1].0;
    h.app.run("photo.setMeta", json!({"ids": [b], "title": "Paris"})).unwrap();
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [b]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h);
    assert!(has(&h, "field:title"), "the box stays drawn, for the new photo");
    assert_eq!(title(&h, a), "Rome", "the text typed for the first photo was saved to it when the photo changed");
    assert_eq!(title(&h, b), "Paris");
    key(&mut h, egui::Key::Enter);
    assert_eq!(title(&h, b), "Paris", "Return saves nothing for the new photo: the box showed its own text");
    assert_eq!(title(&h, a), "Rome");
}

/// Esc still gives the edit up: the text is back and nothing is saved.
#[test]
fn esc_still_discards_the_edit() {
    let mut h = typing_in_copyright("Example Name");
    key(&mut h, egui::Key::Escape);
    assert_eq!(import(&mut h, "copyright"), json!(""), "Esc gave the edit up: nothing is saved");
    show(&mut h, "general");
    assert_eq!(import(&mut h, "copyright"), json!(""), "and the tab's end finds nothing to save");
}

/// Esc that arrives with the command that takes the box away is still an Esc.
#[test]
fn esc_with_the_command_that_hides_the_box_still_discards() {
    let (mut h, photo) = typing_in_title("Rome");
    for pressed in [true, false] {
        h.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE });
    }
    let r = h.request("engine.execute", json!({"command": "panel.info", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h);
    assert!(!has(&h, "field:title"), "the box is not drawn");
    assert_eq!(title(&h, photo), "", "Esc gave the edit up");
}

/// Return ends the edit and saves; a box that stays drawn saves once, however long it stays.
#[test]
fn a_box_that_stays_drawn_saves_exactly_once() {
    for ending in ["return", "click away"] {
        let (mut h, photo) = typing_in_title("Rome");
        let undo = h.app.session.undo.len();
        if ending == "return" {
            key(&mut h, egui::Key::Enter);
        } else {
            // (the empty room at the bottom of the Info panel: no control there)
            let at = egui::pos2(h.size.x - 20.0, h.size.y - 60.0);
            press_and_release(&mut h, at);
        }
        assert_eq!(title(&h, photo), "Rome", "{ending}: saved");
        for _ in 0..10 {
            h.step();
        }
        assert_eq!(h.app.session.undo.len(), undo + 1, "{ending}: one save, however many frames follow");
        h.request_close();
        h.step();
        assert_eq!(h.app.session.undo.len(), undo + 1, "{ending}: and the window closing finds nothing left to save");
    }
}

/// A dialog's own box, where Return means OK and Cancel must not save, doesn't opt in: Cancel
/// leaves nothing behind.
#[test]
fn cancel_does_not_save_the_text_typed_in_a_dialog_box() {
    let mut h = demo();
    h.app.ui.dialog = Some(Dialog::NewAlbum { name: String::new(), folder: false, parent: None, include_selected: false });
    steps(&mut h);
    let albums = h.app.session.catalog.albums().count();
    type_into(&mut h, "field:albumName", "Never Made");
    click(&mut h, "button:dialogCancel", "left");
    assert!(h.app.ui.dialog.is_none(), "the dialog is closed");
    h.request_close();
    h.step();
    assert_eq!(h.app.session.catalog.albums().count(), albums, "Cancel made no album");
}

/// Another library opened (a menu item, the control channel) with an Info box mid-edit: the text
/// is saved to the library it was typed in, and photo ids being local to a library, never run
/// against the one that was opened.
#[test]
fn a_library_switch_mid_edit_saves_to_the_old_library_and_writes_nothing_to_the_new() {
    let root = std::env::temp_dir().join(format!("lc-tf-switch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (a, b) = (root.join("a"), root.join("b"));
    // (a second library with photos of its own, whose ids are the first's)
    let mut other = lightcraft_engine::Session::new();
    other.open_library(&b, true).unwrap();
    other.close_library().unwrap();
    drop(other);
    let mut session = lightcraft_engine::Session::new();
    session.open_library(&a, true).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1200.0, 900.0], 1.0);
    h.settle(T);
    let first = h.app.session.visible_cloned()[0].0;
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.run("photo.setMeta", json!({"ids": [first], "title": ""})).unwrap();
    h.app.ui.right = RightPanel::Info;
    h.settle(T);
    steps(&mut h);
    type_into(&mut h, "field:title", "Rome");
    assert_eq!(title(&h, first), "", "nothing is saved while the edit is under way");
    h.app.run("app.openLibrary", json!({"path": b.to_string_lossy()})).unwrap();
    h.settle(T);
    steps(&mut h);
    h.request_close();
    h.step();
    let titles: Vec<String> =
        h.app.session.visible_cloned().iter().filter_map(|id| h.app.session.catalog.photo(*id)).map(|p| p.meta.title.clone()).collect();
    assert!(!titles.is_empty() && titles.iter().all(|t| t != "Rome"), "the text typed in the old library was written into the new one: {titles:?}");
    h.app.run("app.openLibrary", json!({"path": a.to_string_lossy()})).unwrap();
    h.settle(T);
    assert_eq!(title(&h, first), "Rome", "the text typed in the old library was dropped when it was closed");
    let _ = h.app.session.close_library();
    let _ = std::fs::remove_dir_all(&root);
}

/// An Info box with text the engine refuses (a place that is no coordinates), typed and not ended.
fn typing_refused_gps() -> Headless {
    let (mut h, _) = typing_in_title("Rome");
    // (tall enough that the GPS box is on screen without scrolling the panel)
    let r = h.request("ui.resize", json!({"width": 1200, "height": 2400}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h);
    type_into(&mut h, "field:gps", "not a place");
    h
}

/// A save the engine refuses (invalid GPS text) does not let the window close: the box keeps the
/// text and the error is shown. Closing again, after the error was shown, gives the text up.
#[test]
fn a_refused_save_stops_the_window_closing_and_a_second_close_discards() {
    let mut h = typing_refused_gps();
    h.request_close();
    h.step();
    assert!(h.window_commands.contains(&egui::ViewportCommand::CancelClose), "the close was not canceled: the refused text would be lost");
    assert!(h.app.ui.toast.is_some(), "the error is shown");
    assert!(h.view.ctx.memory(|m| m.has_focus(crate::text_field::id("field:gps"))), "the box still holds the text");
    h.window_commands.clear();
    h.request_close();
    h.step();
    assert!(!h.window_commands.contains(&egui::ViewportCommand::CancelClose), "closing again after the error was shown gives the text up");
}

/// The shared close path (`may_close_after_saving_edits`, which the close button and the macOS Quit
/// both call; `main.rs` can't run headless): a refused save keeps the app open with the error
/// shown, and the second try gives the text up.
#[test]
fn a_refused_save_stops_quit_and_a_second_quit_discards() {
    let mut h = typing_refused_gps();
    let ctx = h.view.ctx.clone();
    assert!(!h.app.may_close_after_saving_edits(&ctx), "Quit went ahead with refused text typed");
    assert!(h.app.ui.toast.is_some(), "the error is shown");
    steps(&mut h);
    assert!(h.app.may_close_after_saving_edits(&ctx), "Quit again, after the error was shown, gives the text up");
}

/// The same for Open Library: it refuses, saying why, and the library stays; the second try goes ahead.
#[test]
fn a_refused_save_stops_a_library_switch_and_a_second_try_discards() {
    let mut h = typing_refused_gps();
    let dir = std::env::temp_dir().join(format!("lc-tf-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let generation = h.app.session.library_generation();
    let first = h.app.run("app.openLibrary", json!({"path": dir.to_string_lossy()}));
    assert!(first.is_err(), "the switch went ahead with refused text typed: {first:?}");
    assert_eq!(h.app.session.library_generation(), generation, "the library stays open");
    steps(&mut h);
    assert!(h.view.ctx.memory(|m| m.has_focus(crate::text_field::id("field:gps"))), "the box still holds the text");
    let second = h.app.run("app.openLibrary", json!({"path": dir.to_string_lossy()}));
    assert!(second.is_ok(), "the second try gives the text up: {second:?}");
    assert_ne!(h.app.session.library_generation(), generation, "and the library was switched");
    let _ = h.app.session.close_library();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Text put right after a refusal is a new edit: closing saves it and lets the window close in one step.
#[test]
fn text_put_right_after_a_refusal_is_saved_and_the_window_closes() {
    let mut h = typing_refused_gps();
    h.request_close();
    h.step();
    assert!(h.window_commands.contains(&egui::ViewportCommand::CancelClose), "refused: the window stays");
    h.window_commands.clear();
    for pressed in [true, false] {
        h.events.push(egui::Event::Key { key: egui::Key::A, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::COMMAND });
    }
    steps(&mut h);
    let r = h.request("ui.text", json!({"text": "46.0, 7.0"}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h);
    h.request_close();
    h.step();
    assert!(!h.window_commands.contains(&egui::ViewportCommand::CancelClose), "a save that succeeds lets the window close");
    let photo = h.app.session.visible_cloned()[0];
    assert!(h.app.session.catalog.photo(photo).unwrap().meta.gps.is_some(), "the corrected place was saved");
}

/// Open Library… from its folder dialog is answered in the middle of a pass, after the box was
/// drawn (`pick::poll` runs the command): the second try must still give the refused text up.
#[test]
fn a_refused_save_is_given_up_when_the_switch_comes_from_inside_a_pass() {
    let mut h = typing_refused_gps();
    let dir = std::env::temp_dir().join(format!("lc-tf-inpass-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let senders = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = senders.clone();
    h.app.services.picker = Some(Box::new(move |_| {
        let (tx, rx) = std::sync::mpsc::channel();
        kept.lock().unwrap().push(tx);
        Ok(rx)
    }));
    let generation = h.app.session.library_generation();
    let choose = |h: &mut Headless| {
        h.app.run("app.openLibrary", json!({})).unwrap();
        let tx = senders.lock().unwrap().pop().expect("the folder dialog was asked");
        tx.send(vec![dir.to_string_lossy().into_owned()]).unwrap();
        steps(h);
    };
    choose(&mut h);
    assert_eq!(h.app.session.library_generation(), generation, "refused the first time: the library stays");
    assert!(h.view.ctx.memory(|m| m.has_focus(crate::text_field::id("field:gps"))), "the box still holds the text");
    choose(&mut h);
    assert_ne!(h.app.session.library_generation(), generation, "given up the second time: the library switched");
    let _ = h.app.session.close_library();
    let _ = std::fs::remove_dir_all(&dir);
}
