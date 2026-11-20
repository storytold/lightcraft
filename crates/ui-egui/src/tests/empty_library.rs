//! LR-BEHAV-EMPTY-STATES: a library with no photos leads to Import. It opens on the Photo Grid
//! with the sidebar, the grid and the Detail view offer Import Photos… and Import Lightroom
//! Catalog… as buttons, and the top bar has an Import button for every library.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::empty_library;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn app(session: lightcraft_engine::Session) -> LightcraftApp {
    LightcraftApp::new(session, Services::default())
}

/// A picker that records that it was asked and gives no files (the person cancelled).
fn counting_picker(h: &mut Headless) -> Rc<Cell<usize>> {
    let asked = Rc::new(Cell::new(0));
    let seen = asked.clone();
    h.app.services.pick_files = Some(Box::new(move || {
        seen.set(seen.get() + 1);
        Vec::new()
    }));
    asked
}

#[test]
fn an_empty_library_opens_on_the_grid_with_the_sidebar() {
    // Given a library with no photos, left in the Detail view with the sidebar hidden
    let mut a = app(lightcraft_engine::Session::new());
    a.ui.view = ViewMode::Detail;
    a.ui.left_panel = false;
    // When it opens at launch
    empty_library::open_on_empty_library(&mut a);
    // Then it shows the Photo Grid and the sidebar, where Import is offered
    assert_eq!(a.ui.view, ViewMode::PhotoGrid);
    assert!(a.ui.left_panel);
}

#[test]
fn an_empty_library_left_on_the_square_grid_stays_there() {
    let mut a = app(lightcraft_engine::Session::new());
    a.ui.view = ViewMode::SquareGrid;
    empty_library::open_on_empty_library(&mut a);
    assert_eq!(a.ui.view, ViewMode::SquareGrid);
    assert!(a.ui.left_panel);
}

#[test]
fn a_library_with_photos_opens_as_it_was_left() {
    let mut a = app(lightcraft_engine::Session::with_demo());
    a.ui.view = ViewMode::Detail;
    a.ui.left_panel = false;
    empty_library::open_on_empty_library(&mut a);
    assert_eq!(a.ui.view, ViewMode::Detail);
    assert!(!a.ui.left_panel);
}

#[test]
fn deleted_photos_leave_the_library_empty() {
    let mut s = lightcraft_engine::Session::with_demo();
    assert!(!empty_library::is_empty(&s));
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    s.selection.ids = ids.clone();
    s.execute("photo.delete", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
    assert!(empty_library::is_empty(&s), "photos in Recently Deleted are not in the library");
}

#[test]
fn the_empty_grid_offers_import_as_buttons() {
    let mut a = app(lightcraft_engine::Session::new());
    a.ui.view = ViewMode::PhotoGrid;
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert!(has(&h, "button:emptyImportPhotos"));
    #[cfg(not(target_arch = "wasm32"))]
    assert!(has(&h, "button:emptyImportLightroom"));
    // a click asks for the photos, as File ▸ Import Photos… does
    let asked = counting_picker(&mut h);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:emptyImportPhotos"}), T)["ok"], true);
    h.step();
    assert_eq!(asked.get(), 1);
}

#[test]
fn the_empty_detail_view_offers_import_instead_of_the_grid() {
    // the dead end the critique found: Detail pointed at a grid with nothing in it
    let mut a = app(lightcraft_engine::Session::new());
    a.ui.view = ViewMode::Detail;
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert!(has(&h, "button:emptyImportPhotos"));
}

#[test]
fn detail_with_photos_but_none_selected_keeps_its_message() {
    let mut a = app(lightcraft_engine::Session::with_demo());
    a.ui.view = ViewMode::Detail;
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.app.session.selection.ids.clear();
    h.app.session.selection.active = None;
    h.step();
    h.step();
    assert!(h.app.session.active().is_none());
    assert!(!has(&h, "button:emptyImportPhotos"), "a library with photos needs no import prompt");
}

#[test]
fn the_top_bar_imports_in_every_library() {
    let mut h = Headless::new(app(lightcraft_engine::Session::with_demo()), [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert!(has(&h, "icon:import"));
    let asked = counting_picker(&mut h);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "icon:import"}), T)["ok"], true);
    h.step();
    assert_eq!(asked.get(), 1);
}

#[test]
fn the_hint_shows_the_shortcut_as_bound() {
    let mut keymap = crate::state::AppSettings::default().keymap;
    assert_eq!(empty_library::hint(&keymap, true), "or drop photos and folders into this window · ⇧⌘I");
    assert_eq!(empty_library::hint(&keymap, false), "or drop photos and folders into this window · Ctrl+Shift+I");
    // a remapped shortcut is the one shown
    crate::shortcuts::assign(&mut keymap, "file.addPhotos", Some("Cmd+Alt+I")).unwrap();
    assert_eq!(empty_library::hint(&keymap, true), "or drop photos and folders into this window · ⌥⌘I");
    // and none at all leaves the key out
    crate::shortcuts::assign(&mut keymap, "file.addPhotos", None).unwrap();
    assert_eq!(empty_library::hint(&keymap, true), "or drop photos and folders into this window");
}

/// The grid's line for a view with no photos that is not the empty library names the shortcut as
/// the menu bar lists it, in the platform's notation, and follows a rebinding.
#[test]
fn the_no_photos_line_lists_modifiers_in_the_menu_order() {
    let mut keymap = crate::state::AppSettings::default().keymap;
    let mac = empty_library::no_photos_body(&keymap, true);
    assert!(mac.contains("⇧⌘I") && !mac.contains("⌘⇧"), "{mac}");
    let other = empty_library::no_photos_body(&keymap, false);
    assert!(other.contains("Ctrl+Shift+I") && !other.contains("Cmd"), "{other}");
    crate::shortcuts::assign(&mut keymap, "file.addPhotos", Some("Cmd+Alt+Shift+I")).unwrap();
    let rebound = empty_library::no_photos_body(&keymap, true);
    assert!(rebound.contains("⌥⇧⌘I"), "{rebound}");
    crate::shortcuts::assign(&mut keymap, "file.addPhotos", None).unwrap();
    let unbound = empty_library::no_photos_body(&keymap, true);
    assert!(!unbound.contains('⌘') && !unbound.contains("Cmd") && !unbound.starts_with("or "), "{unbound}");
}

#[test]
fn a_folder_browsed_under_local_is_not_the_empty_library() {
    // its photos aren't in the library, but they are what is on screen: no import prompt
    let mut a = app(lightcraft_engine::Session::new());
    a.ui.view = ViewMode::Detail;
    a.session.source = lightcraft_engine::LibrarySource::Folder;
    assert!(empty_library::is_empty(&a.session));
    assert!(!empty_library::shown(&a.session));
    let mut h = Headless::new(a, [1300.0, 900.0], 1.0);
    h.step();
    h.step();
    assert!(!has(&h, "button:emptyImportPhotos"));
}

#[test]
fn recently_deleted_is_not_the_empty_library() {
    // the deleted photos are on screen there: no import prompt beside them
    let mut s = lightcraft_engine::Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    s.selection.ids = ids.clone();
    s.execute("photo.delete", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
    s.source = lightcraft_engine::LibrarySource::RecentlyDeleted;
    assert!(empty_library::is_empty(&s));
    assert!(!empty_library::shown(&s));
}

#[test]
fn the_import_button_leaves_the_filter_and_appearance_icons_apart() {
    // the search field is centred: a seventh icon on the right must not push it into them
    // with the menus in the window (Windows, Linux, web) and in the native menu bar (macOS)
    for (width, native_menu) in [900.0, 906.0, 1024.0, 1200.0, 1600.0].into_iter().flat_map(|w| [(w, false), (w, true)]) {
        let mut a = app(lightcraft_engine::Session::with_demo());
        // a native menu bar comes with the title bar drawn in the window (macOS): a wider left margin
        a.native_menu = native_menu;
        a.integrated_titlebar = native_menu;
        let mut h = Headless::new(a, [width, 800.0], 1.0);
        h.step();
        h.step();
        let rect = |id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("{id} at {width}"));
        let (filter, appearance, import) = (rect("icon:filter"), rect("icon:appearance"), rect("icon:import"));
        assert!(!filter.intersects(appearance), "{width} (native menu {native_menu}): filter {filter:?} meets appearance {appearance:?}");
        assert!(!filter.intersects(import), "{width} (native menu {native_menu}): filter {filter:?} meets import {import:?}");
    }
}
