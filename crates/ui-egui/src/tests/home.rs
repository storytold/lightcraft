//! LR-BEHAV-HOME: the Home screen is a view like the others, offers the ways into a library, lists
//! the recent ones, and can be what the app opens on. No library opens on it unless asked.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::panels::empty_library;
use crate::state::{StartupView, UiState, ViewMode};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn headless(session: lightcraft_engine::Session) -> Headless {
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1400.0, 900.0], 1.0);
    h.settle(T);
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn home(h: &mut Headless) {
    h.request("engine.execute", json!({"command": "view.home"}), T);
    assert_eq!(h.app.ui.view, ViewMode::Home);
    h.settle(T);
}

#[test]
fn view_home_shows_the_ways_into_a_library() {
    let mut h = headless(lightcraft_engine::Session::with_demo());
    assert!(!has(&h, "button:homeImportPhotos"), "not on the grid");
    home(&mut h);
    for id in ["button:homeImportPhotos", "button:homeOpenLibrary", "button:homeAllPhotos", "button:homeHelp", "button:homeDiscord"] {
        assert!(has(&h, id), "{id}");
    }
}

#[test]
fn all_photos_leaves_for_the_grid() {
    let mut h = headless(lightcraft_engine::Session::with_demo());
    home(&mut h);
    h.request("ui.clickWidget", json!({"id": "button:homeAllPhotos"}), T);
    h.settle(T);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
    assert!(!has(&h, "button:homeAllPhotos"));
}

#[test]
fn all_photos_shows_the_whole_library_whatever_source_was_picked() {
    let mut h = headless(lightcraft_engine::Session::with_demo());
    h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "recentlyDeleted"}}), T);
    h.settle(T);
    home(&mut h);
    h.request("ui.clickWidget", json!({"id": "button:homeAllPhotos"}), T);
    h.settle(T);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
    assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::All);
}

/// A folder for a library of its own, gone when the test ends.
struct TempLibrary(std::path::PathBuf);

impl TempLibrary {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("lc-ui-home-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempLibrary(dir)
    }
}

impl Drop for TempLibrary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn open_library_goes_on_to_the_grid_once_a_library_opens() {
    let library = TempLibrary::new("open");
    let picked = library.0.to_string_lossy().into_owned();
    let mut h = headless(lightcraft_engine::Session::new().with_fs());
    h.app.services.pick_folder = Some(Box::new(move || Some(picked.clone())));
    home(&mut h);
    h.request("ui.clickWidget", json!({"id": "button:homeOpenLibrary"}), T);
    h.settle(T);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
}

#[test]
fn a_cancelled_open_library_stays_on_the_screen() {
    let mut h = headless(lightcraft_engine::Session::new().with_fs());
    h.app.services.pick_folder = Some(Box::new(|| None));
    home(&mut h);
    h.request("ui.clickWidget", json!({"id": "button:homeOpenLibrary"}), T);
    h.settle(T);
    assert_eq!(h.app.ui.view, ViewMode::Home);
}

#[test]
fn import_photos_leaves_for_the_grid_where_they_show_up() {
    let mut h = headless(lightcraft_engine::Session::with_demo());
    h.app.services.pick_files = Some(Box::new(Vec::new));
    home(&mut h);
    h.request("ui.clickWidget", json!({"id": "button:homeImportPhotos"}), T);
    h.settle(T);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
}

#[test]
fn a_source_picked_in_the_sidebar_shows_in_the_grid() {
    let mut h = headless(lightcraft_engine::Session::with_demo());
    home(&mut h);
    h.settle(T);
    assert_eq!(h.app.ui.view, ViewMode::Home, "nothing picked: it stays");
    h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "recentlyDeleted"}}), T);
    h.settle(T);
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
}

#[test]
fn coming_back_with_another_source_stays_on_the_screen() {
    let mut h = headless(lightcraft_engine::Session::with_demo());
    h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "recentlyDeleted"}}), T);
    h.settle(T);
    home(&mut h);
    h.settle(T);
    assert_eq!(h.app.ui.view, ViewMode::Home);
}

#[test]
fn recent_libraries_are_listed_up_to_six() {
    let mut h = headless(lightcraft_engine::Session::with_demo());
    h.app.ui.settings.recent_libraries = (0..8).map(|i| format!("/no/such/library-{i}")).collect();
    home(&mut h);
    assert!(has(&h, "home:recent:0") && has(&h, "home:recent:5"));
    assert!(!has(&h, "home:recent:6"));
}

#[test]
fn no_recent_libraries_no_list() {
    let mut h = headless(lightcraft_engine::Session::with_demo());
    h.app.ui.settings.recent_libraries.clear();
    home(&mut h);
    assert!(!has(&h, "home:recent:0"));
}

#[test]
fn a_missing_recent_library_stays_put_when_clicked() {
    let mut h = headless(lightcraft_engine::Session::with_demo());
    h.app.ui.settings.recent_libraries = vec!["/no/such/library".into()];
    home(&mut h);
    h.request("ui.clickWidget", json!({"id": "home:recent:0"}), T);
    h.settle(T);
    assert_eq!(h.app.ui.view, ViewMode::Home, "no library opened: nothing to leave for");
}

#[test]
fn the_menu_ticks_the_view() {
    let mut h = headless(lightcraft_engine::Session::with_demo());
    home(&mut h);
    assert_eq!(crate::menus::menubar::checked(&h.app, "view.home"), Some(true));
    h.request("engine.execute", json!({"command": "view.photoGrid"}), T);
    assert_eq!(crate::menus::menubar::checked(&h.app, "view.home"), Some(false));
}

#[test]
fn startup_opens_on_home_only_when_asked() {
    let mut ui = UiState { view: ViewMode::Detail, ..Default::default() };
    ui = ui.sanitized();
    assert_eq!(ui.view, ViewMode::Detail, "the default is the last view");
    ui.settings.startup_view = StartupView::Home;
    assert_eq!(ui.sanitized().view, ViewMode::Home);
}

#[test]
fn startup_view_home_round_trips_in_the_settings_file() {
    let text = serde_json::to_string(&StartupView::Home).unwrap();
    assert_eq!(text, "\"home\"");
    assert_eq!(serde_json::from_str::<StartupView>(&text).unwrap(), StartupView::Home);
}

#[test]
fn an_empty_library_opening_on_home_stays_there_with_the_sidebar() {
    let mut a = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    a.ui.view = ViewMode::Home;
    a.ui.left_panel = false;
    empty_library::open_on_empty_library(&mut a);
    assert_eq!(a.ui.view, ViewMode::Home);
    assert!(a.ui.left_panel);
}
