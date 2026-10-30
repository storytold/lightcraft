//! Keyboard in Sidebar (`view.sidebarKeys`, Cmd+Alt+B) in the app's own left sidebar, through the control
//! channel (`tests_side_keys.rs` has the core on a stand-in sidebar).
//!
//! - Off (as always before): no cursor, and every key is what it was.
//! - On: a cursor (ringed) starts on the row of what is shown; Down / Up / Home / End move it,
//!   Enter chooses its row as a click does, Left / Right fold and unfold and step in and out,
//!   typing goes to the next row that starts so, F2 opens the row's rename dialog.
//! - It ends on Esc, on its shortcut again, on a click outside the sidebar, and when the sidebar is no
//!   longer drawn; keys without Cmd / Ctrl are not shortcuts meanwhile.

use std::time::Duration;

use lightcraft_engine::LibrarySource;
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(20);

fn app() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn key(h: &mut Headless, name: &str) {
    let r = h.request("ui.key", json!({"key": name}), T);
    assert_eq!(r["ok"], true, "{name}: {r}");
    h.step();
    h.step();
    h.step();
}

/// The mode's own shortcut (Cmd+Alt+B).
fn toggle(h: &mut Headless) {
    let r = h.request("ui.key", json!({"key": "B", "cmd": true, "alt": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    h.step();
}

fn text(h: &mut Headless, t: &str) {
    let r = h.request("ui.text", json!({"text": t}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.step();
    h.step();
    r["result"].clone()
}

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// The row the cursor's ring is drawn round, by its widget id.
fn cursor_row(h: &Headless) -> Option<String> {
    let ring = rect(h, "sidebarCursor")?;
    let on = |prefix: &str| {
        h.app
            .widgets
            .iter()
            .find(|(w, r)| w.starts_with(prefix) && !w.starts_with("sidebarSectionC") && r.contains_rect(ring.shrink(1.0)))
            .map(|(w, _)| w.clone())
    };
    on("source:").or_else(|| on("sidebarSection:"))
}

/// Given the keys were never asked into the sidebar, then no row is ringed and an arrow key is
/// the photos' as ever.
#[test]
fn off_there_is_no_cursor_and_keys_are_the_photos() {
    let mut h = app();
    let r = h.request("ui.clickWidget", json!({"id": "source:picks"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!h.app.ui.sidebar_keys, "a click on a row does not take the keys");
    let before = h.app.session.selection.active;
    key(&mut h, "right");
    assert_eq!(cursor_row(&h), None);
    assert_eq!(h.app.session.source, LibrarySource::Picks, "the arrow moved among the photos, not the rows");
    assert_ne!(h.app.session.selection.active, before, "the next photo is active");
}

/// Given its shortcut, then the cursor is on the row of what is shown and says so; Down and Up move it, and
/// Enter chooses its row as a click would.
#[test]
fn the_shortcut_puts_a_cursor_on_the_shown_row_and_enter_chooses_like_a_click() {
    let mut h = app();
    toggle(&mut h);
    assert!(h.app.ui.sidebar_keys);
    assert_eq!(cursor_row(&h).as_deref(), Some("source:all"), "it starts where the grid is");
    let said = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(said.contains("Esc"), "the mode says how it ends: {said:?}");
    key(&mut h, "down");
    assert_eq!(cursor_row(&h).as_deref(), Some("source:recentlyAdded"));
    assert_eq!(h.app.session.source, LibrarySource::All, "moving the cursor chooses nothing");
    key(&mut h, "down");
    key(&mut h, "enter");
    assert_eq!(h.app.session.source, LibrarySource::Picks, "Enter is a click on the row");
    key(&mut h, "up");
    key(&mut h, "space");
    assert_eq!(h.app.session.source, LibrarySource::RecentlyAdded, "so is Space");
    key(&mut h, "Home");
    assert_eq!(cursor_row(&h).as_deref(), Some("source:all"));
    key(&mut h, "up");
    assert_eq!(cursor_row(&h).as_deref(), Some("source:all"), "the top is the top");
}

/// Left and Right fold and unfold what the cursor is on (a section by its header, a folder of
/// albums by its row) and step into and out of it.
#[test]
fn left_and_right_fold_unfold_and_step_in_and_out() {
    let mut h = app();
    let folder = h.app.session.catalog.albums().find(|a| a.folder && a.parent.is_none()).map(|a| a.id.0).expect("a demo album folder");
    let inside = h.app.session.catalog.albums().find(|a| a.parent.map(|p| p.0) == Some(folder)).map(|a| a.id.0).expect("an album in it");
    exec(&mut h, "library.source", json!({"kind": "album", "id": inside}));
    toggle(&mut h);
    assert_eq!(cursor_row(&h), Some(format!("source:album:{inside}")), "on the shown album");
    key(&mut h, "left");
    assert_eq!(cursor_row(&h), Some(format!("source:folder:{folder}")), "Left steps out to the folder it is in");
    key(&mut h, "left");
    assert!(rect(&h, &format!("source:album:{inside}")).is_none(), "Left on an open folder folds it");
    key(&mut h, "right");
    assert!(rect(&h, &format!("source:album:{inside}")).is_some(), "Right unfolds it");
    key(&mut h, "right");
    let stepped = cursor_row(&h).and_then(|c| c.strip_prefix("source:album:").and_then(|n| n.parse::<u64>().ok())).expect("on an album");
    assert_eq!(
        h.app.session.catalog.albums().find(|a| a.id.0 == stepped).and_then(|a| a.parent).map(|p| p.0),
        Some(folder),
        "Right on an open folder steps in"
    );
    key(&mut h, "left");
    key(&mut h, "left");
    key(&mut h, "left");
    assert_eq!(cursor_row(&h).as_deref(), Some("sidebarSection:albums"), "out of the folder is the section's header");
    assert!(!h.app.ui.sidebar.is_collapsed(crate::sidebar::SidebarSection::Albums));
    key(&mut h, "left");
    assert!(h.app.ui.sidebar.is_collapsed(crate::sidebar::SidebarSection::Albums), "Left on a header folds the section");
    key(&mut h, "enter");
    assert!(!h.app.ui.sidebar.is_collapsed(crate::sidebar::SidebarSection::Albums), "Enter on a header is a click on it");
}

/// Typing goes to the next row whose name starts so; the letters are not the photos' shortcuts
/// meanwhile (P would flag the active photo as a pick).
#[test]
fn typing_finds_a_row_and_letters_are_not_shortcuts_meanwhile() {
    let mut h = app();
    let photo = h.app.session.selection.active.expect("an active photo");
    let flag = |h: &Headless| h.app.session.catalog.photo(photo).map(|p| format!("{:?}", p.flag));
    let before = flag(&h);
    toggle(&mut h);
    text(&mut h, "p");
    key(&mut h, "P");
    assert_eq!(cursor_row(&h).as_deref(), Some("source:picks"), "P goes to Picks");
    assert_eq!(flag(&h), before, "and flags nothing");
    key(&mut h, "escape");
    assert!(!h.app.ui.sidebar_keys, "Esc gives the keys back");
    assert_eq!(cursor_row(&h), None, "and the ring goes");
    assert_eq!(h.app.session.source, LibrarySource::All, "Esc chose nothing and went nowhere");
    key(&mut h, "P");
    assert_ne!(flag(&h), before, "P is the photos' again");
}

/// F2 opens the rename dialog of the row the cursor is on, and says so on a row with no name of
/// its own.
#[test]
fn f2_renames_the_row_under_the_cursor() {
    let mut h = app();
    let (album, name) =
        h.app.session.catalog.albums().find(|a| !a.folder && a.parent.is_none()).map(|a| (a.id.0, a.name.clone())).expect("a demo album");
    exec(&mut h, "library.source", json!({"kind": "album", "id": album}));
    toggle(&mut h);
    key(&mut h, "F2");
    assert_eq!(h.app.ui.dialog, Some(Dialog::RenameAlbum { id: album, name }));
    h.app.ui.dialog = None;
    key(&mut h, "Home");
    key(&mut h, "F2");
    assert_eq!(h.app.ui.dialog, None, "All Photos has no name to change");
    let said = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(said.contains("no name"), "{said:?}");
}

/// End takes the cursor to the last row and scrolls it into view; the mode ends on its shortcut again, on
/// a click outside the sidebar, and when the sidebar goes away (keys are never left with
/// something out of sight).
#[test]
fn the_cursor_stays_in_view_and_the_mode_ends_with_the_sidebar() {
    let mut h = app();
    let r = h.request("ui.resize", json!({"width": 1400.0, "height": 420.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    toggle(&mut h);
    key(&mut h, "End");
    h.settle(SETTLE);
    let (ring, panel) = (rect(&h, "sidebarCursor").expect("a cursor"), rect(&h, "panel:left_panel").expect("the panel"));
    assert!(panel.contains_rect(ring.shrink(1.0)), "the last row was scrolled to: {ring:?} in {panel:?}");
    assert_ne!(cursor_row(&h).as_deref(), Some("source:all"));
    toggle(&mut h);
    assert!(!h.app.ui.sidebar_keys, "the shortcut again gives the keys back");
    toggle(&mut h);
    assert!(h.app.ui.sidebar_keys);
    let r = h.request("ui.click", json!({"x": 900.0, "y": 300.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(!h.app.ui.sidebar_keys, "a click on the photos gives them the keys");
    toggle(&mut h);
    assert!(h.app.ui.sidebar_keys);
    h.app.ui.left_panel = false;
    for _ in 0..5 {
        h.step();
    }
    let before = h.app.session.selection.active;
    key(&mut h, "right");
    assert!(!h.app.ui.sidebar_keys, "no sidebar, no sidebar keys");
    assert_ne!(h.app.session.selection.active, before, "and the press was the photos', not lost");
    // asked for while the sidebar is hidden, it comes back with them
    exec(&mut h, "view.sidebarKeys", json!({"on": true}));
    assert!(h.app.ui.left_panel && h.app.ui.sidebar_keys);
    let r = h.request("engine.execute", json!({"command": "view.sidebarKeys", "params": {"on": "yes"}}), T);
    assert_eq!(r["ok"], false, "{r}");
}

/// Tab leaves the sidebar as it leaves any list: the ring goes and the keys are the photos'
/// again (it used to hand the keys to whatever egui focused next, with the ring still showing).
#[test]
fn tab_gives_the_keys_back() {
    let mut h = app();
    toggle(&mut h);
    key(&mut h, "down");
    key(&mut h, "tab");
    assert!(!h.app.ui.sidebar_keys);
    assert_eq!(cursor_row(&h), None);
    // (egui gives the first control the focus for a Tab that nothing has focused: the sidebar takes it back)
    let focused = h.view.ctx.memory(|m| m.focused());
    assert!(focused.is_none(), "the Tab left the focus on {focused:?}");
    let before = h.app.session.selection.active;
    key(&mut h, "right");
    assert_ne!(h.app.session.selection.active, before, "the arrows are the photos' again");
}

/// While an album is dragged, Esc abandons the drag as ever; the sidebar's keys wait.
#[test]
fn esc_during_a_drag_is_the_drags() {
    let mut h = app();
    let album = h.app.session.catalog.albums().find(|a| !a.folder).map(|a| a.id.0).expect("a demo album");
    toggle(&mut h);
    h.app.ui.dragging_album = Some(album);
    key(&mut h, "escape");
    assert_eq!(h.app.ui.dragging_album, None, "the drag saw its Esc");
    assert!(h.app.ui.sidebar_keys, "and the keys stay in the sidebar");
}

/// Keys that come in one frame act one after the other: Down then Enter chooses the row the
/// cursor moved to, not the one it was on.
#[test]
fn keys_of_one_frame_act_in_order() {
    let mut h = app();
    toggle(&mut h);
    for k in [egui::Key::ArrowDown, egui::Key::ArrowDown, egui::Key::Enter] {
        h.app.synthetic.push(egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
        h.app.synthetic.push(egui::Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
    }
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(cursor_row(&h).as_deref(), Some("source:picks"));
    assert_eq!(h.app.session.source, LibrarySource::Picks, "Enter chose where the cursor had got to");
}

/// A key with Shift is not the sidebar's (Shift+Enter would otherwise choose a range, which the
/// keys do not offer yet); the same letter typed again moves on to the next row that starts so;
/// a space while a word is being typed belongs to the word.
#[test]
fn shift_is_left_alone_and_typing_cycles_and_takes_spaces() {
    let mut h = app();
    toggle(&mut h);
    key(&mut h, "down");
    let r = h.request("ui.key", json!({"key": "enter", "shift": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.session.source, LibrarySource::All, "Shift+Enter chose nothing");
    // "r": Recently Added, then Recently Deleted, then round again
    key(&mut h, "Home");
    text(&mut h, "r");
    assert_eq!(cursor_row(&h).as_deref(), Some("source:recentlyAdded"));
    text(&mut h, "r");
    assert_eq!(cursor_row(&h).as_deref(), Some("source:recentlyDeleted"), "the same letter again moves on");
    // "all p" is All Photos: the space is typed, not a choice
    // (a key that is no letter ends the word: "rr" is over)
    h.app.session.source = LibrarySource::Picks;
    key(&mut h, "End");
    text(&mut h, "all");
    key(&mut h, "space");
    text(&mut h, "p");
    assert_eq!(cursor_row(&h).as_deref(), Some("source:all"));
    assert_eq!(h.app.session.source, LibrarySource::Picks, "the space chose nothing");
}

/// Moving up through a long section, the cursor's row stops below the header pinned over the
/// top of the list, never under it.
#[test]
fn the_cursor_is_never_under_the_pinned_header() {
    let mut h = app();
    let r = h.request("ui.resize", json!({"width": 1400.0, "height": 420.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    toggle(&mut h);
    key(&mut h, "End");
    let mut checked = 0;
    for _ in 0..14 {
        key(&mut h, "up");
        h.settle(SETTLE);
        let ring = rect(&h, "sidebarCursor").expect("a cursor");
        let pinned = h.app.widgets.iter().find(|(w, _)| w.starts_with("sidebarSectionPinned:")).map(|(_, r)| *r);
        if let Some(pinned) = pinned {
            assert!(
                ring.top() >= pinned.bottom() - 0.5 || cursor_row(&h).is_some_and(|c| c.starts_with("sidebarSection")),
                "the ring {ring:?} is under {pinned:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 3, "a header was pinned while moving up: {checked}");
}

/// Asked for in full screen, where no sidebar is drawn, the command refuses and says why. F2 on a keyword
/// opens its rename dialog; a click on a row moves the cursor there; the toast comes once.
#[test]
fn it_refuses_where_there_is_no_sidebar_and_follows_clicks() {
    let mut h = app();
    h.app.ui.fullscreen = true;
    let r = h.request("engine.execute", json!({"command": "view.sidebarKeys", "params": {"on": true}}), T);
    assert!(r["error"].as_str().is_some_and(|e| e.contains("full screen")), "{r}");
    assert!(!h.app.ui.sidebar_keys);
    h.app.ui.fullscreen = false;
    toggle(&mut h);
    assert!(h.app.ui.toast.is_some(), "the first time says what the keys do");
    h.app.ui.toast = None;
    let r = h.request("ui.clickWidget", json!({"id": "source:picks"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.sidebar_keys, "a click in the sidebar keeps the keys there");
    assert_eq!(cursor_row(&h).as_deref(), Some("source:picks"), "and the cursor goes where the click went");
    toggle(&mut h);
    toggle(&mut h);
    assert!(h.app.ui.toast.is_none(), "the second time the ring says it");
    let keyword = h.app.caches.keyword_tree(&h.app.session.catalog).first().map(|k| (k.path.clone(), k.name.clone())).expect("a demo keyword");
    h.app.ui.sidebar_cursor = Some(format!("keyword:{}", keyword.0));
    h.step();
    key(&mut h, "F2");
    assert_eq!(h.app.ui.dialog, Some(Dialog::RenameKeyword { from: keyword.0.clone(), to: keyword.0 }));
}

/// Find (Cmd+F) takes the keys to the search box, as any field that is focused does: the mode
/// ends and what is typed goes into the box (it used to be let go at once, and typing moved the
/// cursor). What a dialog leaves focused as it closes is no such field: the keys stay.
#[test]
fn a_field_that_takes_the_focus_takes_the_keys() {
    let mut h = app();
    toggle(&mut h);
    exec(&mut h, "view.focusSearch", json!({}));
    h.step();
    h.step();
    assert!(!h.app.ui.sidebar_keys, "Find has the keys");
    text(&mut h, "pets");
    assert!(h.view.ctx.egui_wants_keyboard_input(), "the search box kept the focus");
    assert_eq!(cursor_row(&h), None);
}

/// Given a field has the focus (Find), asking for the mode by command (the palette, the control
/// channel) takes the keys: the focus is let go, the mode stays on, and it says how it works.
#[test]
fn asking_for_the_mode_while_a_field_is_focused_lets_the_field_go() {
    let mut h = app();
    exec(&mut h, "view.focusSearch", json!({}));
    h.step();
    h.step();
    assert!(h.view.ctx.egui_wants_keyboard_input(), "the search box has the focus");
    h.app.ui.toast = None;
    exec(&mut h, "view.sidebarKeys", json!({"on": true}));
    h.step();
    h.step();
    assert!(h.app.ui.sidebar_keys, "the field did not end the mode at once");
    assert!(!h.view.ctx.egui_wants_keyboard_input(), "the field let the focus go");
    let said = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(said.contains("Esc"), "the mode said how it ends: {said:?}");
}

/// Given a dialog is open with its field focused, asking for the mode (the native menu delivers it
/// meanwhile) leaves the dialog's field its focus: a field that saves on blur must not save half a value.
#[test]
fn asking_for_the_mode_with_a_dialog_open_keeps_the_dialogs_focus() {
    let mut h = app();
    let keyword = h.app.caches.keyword_tree(&h.app.session.catalog).first().map(|k| k.path.clone()).expect("a demo keyword");
    h.app.ui.dialog = Some(Dialog::RenameKeyword { from: keyword.clone(), to: keyword });
    h.step();
    h.step();
    h.step();
    assert!(h.view.ctx.egui_wants_keyboard_input(), "the dialog's field has the focus");
    // the command's own effect, before the dialog draws its field again (it asks for the focus back each frame)
    let r = crate::menus::run_ui_command(&mut h.app, "view.sidebarKeys", &json!({"on": true}));
    assert!(matches!(r, Some(Ok(_))), "{r:?}");
    assert!(h.view.ctx.memory(|m| m.focused()).is_some(), "the dialog's field kept the focus");
    assert!(h.app.ui.dialog.is_some(), "the dialog is still open");
}

/// Two folders in one Local place, one of them named like the other with `#2` after it: F2 on
/// each renames that folder (the keys' own mark of a row drawn twice is no part of a name).
#[test]
fn f2_renames_the_folder_it_is_on_whatever_it_is_called() {
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let base = std::env::temp_dir().join(format!("lc-ui-keys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let _cleanup = Cleanup(base.clone());
    for name in ["Travel", "Travel#2"] {
        std::fs::create_dir_all(base.join(name)).unwrap();
    }
    let mut h = app();
    h.hide_home_above(&base);
    exec(&mut h, "local.addRoot", json!({"path": base.to_string_lossy()}));
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": format!("folderToggle:{}", base.to_string_lossy())}), T);
    assert_eq!(r["ok"], true, "{r}");
    let twin = base.join("Travel#2").to_string_lossy().to_string();
    let row = format!("source:local:{twin}");
    h.step_until(SETTLE, |h| h.app.widgets.iter().any(|(w, _)| *w == row));
    toggle(&mut h);
    h.app.ui.sidebar_cursor = Some(format!("local:{twin}"));
    h.step();
    key(&mut h, "F2");
    match &h.app.ui.dialog {
        Some(Dialog::TextPrompt { value, params, .. }) => {
            assert_eq!(value, "Travel#2");
            assert_eq!(params["path"], twin, "the folder under the cursor, not its neighbour");
        }
        other => panic!("no rename dialog: {other:?}"),
    }
}

/// Given a sidebar longer than the window, whose rows out of sight are not drawn (issue #1011),
/// when End is pressed, then the cursor goes to the last row all the same and that row is
/// scrolled into view: the keys pass over rows that are not drawn.
#[test]
fn the_keys_reach_rows_that_are_out_of_sight() {
    let mut h = app();
    let last = h.app.caches.keyword_tree(&h.app.session.catalog).last().map(|k| k.path.clone()).expect("a demo keyword");
    let (row, label) = (format!("source:keyword:{last}"), format!("label:keyword:{last}"));
    assert!(rect(&h, &row).is_some() && rect(&h, &label).is_none(), "the last keyword has its room, and is not drawn");
    toggle(&mut h);
    key(&mut h, "End");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.sidebar_cursor.as_deref(), Some(format!("keyword:{last}").as_str()));
    assert_eq!(cursor_row(&h).as_deref(), Some(row.as_str()), "and it is drawn, ringed");
    assert!(rect(&h, &label).is_some());
}

/// The command says what it did: on (the cursor is placed by the next frame, so none yet); off, with none (it is no longer anywhere).
#[test]
fn turning_it_off_reports_no_cursor() {
    let mut h = app();
    let on = exec(&mut h, "view.sidebarKeys", json!({"on": true}));
    assert_eq!(on["on"], true, "{on}");
    h.step();
    h.step();
    assert!(h.app.ui.sidebar_cursor.is_some(), "the cursor is on a row");
    let off = exec(&mut h, "view.sidebarKeys", json!({"on": false}));
    assert_eq!((off["on"].clone(), off["cursor"].clone()), (json!(false), Value::Null), "{off}");
}

/// Enter on "Show N hidden locations" (N of two or more) opens its list as a click on it does.
#[test]
fn enter_opens_the_list_of_hidden_locations_as_a_click_does() {
    let mut h = app();
    h.app.ui.hidden_locations = vec!["/tmp/lc-keys-a".into(), "/tmp/lc-keys-b".into()];
    h.settle(SETTLE);
    assert!(rect(&h, "source:local:restoreHidden").is_some(), "the row for the hidden places is drawn");
    toggle(&mut h);
    h.app.ui.sidebar_cursor = Some("local:restoreHidden".into());
    h.step();
    assert!(!egui::Popup::is_any_open(&h.view.ctx));
    key(&mut h, "enter");
    assert!(egui::Popup::is_any_open(&h.view.ctx), "the list of hidden places is open");
}

/// A deep tree is drawn with a narrower indent (issue #2298). Left from a folder whose parent has scrolled out of sight,
/// and so is not drawn, still goes to that parent: rows drawn and rows out of sight tell their depth by the same indent.
#[test]
fn left_goes_to_a_parent_out_of_sight_when_the_indent_is_narrow() {
    let deep = "/Users/someone/Library/CloudStorage/Nextcloud-cloud.example.com/RAW/Club/2026-09-24 Team/RAW";
    let mut h = crate::tests_panels::deep_folders_app_with(deep, [1400.0, 1200.0], 25);
    assert!(crate::panels::left::indent_step(&h.view.ctx) < crate::panels::left::INDENT_STEP, "the indent is narrow");
    // a window too short for the whole sidebar: rows out of sight are not drawn
    let r = h.request("ui.resize", json!({"width": 1400.0, "height": 520.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let child = format!("{deep}/x");
    exec(&mut h, "library.source", json!({"kind": "libraryFolder", "path": child}));
    h.settle(SETTLE);
    toggle(&mut h);
    let source = |p: &str| format!("source:libfolder:{p}");
    let drawn = |h: &Headless, p: &str| rect(h, &format!("label:libfolder:{p}")).is_some();

    assert_eq!(cursor_row(&h), Some(source(&child)), "the cursor starts on the folder that is shown");
    // (the wheel scrolls what the pointer is over: down a little at a time, until the parent is out of sight; the folder
    // the cursor is on is drawn all the same, as the keys' rows are)
    let mut parent_hidden = false;
    for _ in 0..120 {
        if !drawn(&h, deep) {
            parent_hidden = true;
            break;
        }
        h.request("ui.move", json!({"x": 120.0, "y": 300.0}), T);
        let r = h.request("ui.scroll", json!({"dy": -4.0}), T);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..4 {
            h.step();
        }
    }
    assert!(parent_hidden, "the parent scrolled out of sight and is not drawn");
    key(&mut h, "left");
    assert_eq!(cursor_row(&h), Some(source(deep)), "Left steps out to the parent, not past it");
}

/// Given the keys were left (Esc) with the cursor away from the shown row, and the shown row is another
/// one now, when they are asked in again, then the cursor starts on the shown row, not where it was
/// left; and a cursor left in one library names no row of the next.
#[test]
fn coming_in_again_starts_at_the_shown_row_and_a_new_library_forgets_the_cursor() {
    let mut h = app();
    toggle(&mut h);
    key(&mut h, "down");
    key(&mut h, "down");
    assert_eq!(cursor_row(&h).as_deref(), Some("source:picks"));
    key(&mut h, "escape");
    assert!(!h.app.ui.sidebar_keys);
    let r = h.request("ui.clickWidget", json!({"id": "source:recentlyAdded"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    toggle(&mut h);
    assert_eq!(cursor_row(&h).as_deref(), Some("source:recentlyAdded"), "where the grid is, not where the keys were left");
    key(&mut h, "escape");
    h.app.ui.sidebar_cursor = Some("keyword:gone".into());
    h.app.shown_library = None;
    h.step();
    h.step();
    assert_eq!(h.app.ui.sidebar_cursor, None, "a library's cursor is not the next one's");
}

/// Given the keys are on, when another library opens in the frame the sidebar is next drawn, then
/// the cursor starts at the shown row of that library's rows, not at the row the last library's
/// rows showed (an id of the last library may name another row in this one).
#[test]
fn a_library_opened_while_the_keys_are_on_starts_the_cursor_from_its_own_rows() {
    let mut h = app();
    toggle(&mut h);
    let first = cursor_row(&h);
    // the next library shows another source; no frame is drawn between, so the last frame's rows
    // still show the old one when the sidebar next begins
    h.app.session.execute("library.source", &json!({"kind": "recentlyAdded"})).unwrap();
    h.app.shown_library = None;
    h.step();
    h.step();
    h.step();
    assert_ne!(first.as_deref(), Some("source:recentlyAdded"));
    assert_eq!(cursor_row(&h).as_deref(), Some("source:recentlyAdded"), "the shown row of this library, not of the last one");
}
