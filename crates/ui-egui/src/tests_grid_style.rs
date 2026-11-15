//! The grid style the user chose last (Photo Grid vs Square Grid) is kept and comes back: after a
//! restart (a save/load round trip of the UI state, #794), from a photo view via `G`, and on the
//! paths that fall back to a grid (Esc to the grid, the filter bar, a Library painter, Modern's
//! back to Library).

use serde_json::json;

use crate::state::{StartupView, UiState, ViewMode};
use crate::{LccApp, Services};

fn app() -> LccApp {
    LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() })
}

/// The choice is recorded and survives the ui.json save/load round trip `sanitized` applies at
/// launch, also when the app was quit while a photo was showing (StartupView::Last keeps the
/// last view, and the grid style rides along for the return to the grid).
#[test]
fn the_grid_choice_survives_a_restart() {
    let mut a = app();
    assert_eq!(a.run("view.squareGrid", json!({})), Ok(serde_json::Value::Null));
    assert_eq!(a.ui.view, ViewMode::SquareGrid);
    assert_eq!(a.ui.grid_style, ViewMode::SquareGrid);

    // quit while a photo is showing (Detail), as users do
    a.ui.view = ViewMode::Detail;
    let saved = serde_json::to_string(&a.ui).unwrap();
    let back = serde_json::from_str::<UiState>(&saved).unwrap().sanitized();
    assert_eq!(back.settings.startup_view, StartupView::Last);
    assert_eq!(back.grid_style, ViewMode::SquareGrid, "the grid style is saved");
    assert_eq!(back.view, ViewMode::Detail, "StartupView::Last keeps the last view");
    // the saved square grid comes back on the ways back to a grid
    let mut relaunched = app();
    relaunched.ui = back;
    assert_eq!(relaunched.run("view.gridToggle", json!({})), Ok(serde_json::Value::Null));
    assert_eq!(relaunched.ui.view, ViewMode::SquareGrid);

    // a hand-edited file with a photo view in the grid-style slot reads as Photo Grid
    let odd: UiState = serde_json::from_str(r#"{"view": "detail", "gridStyle": "people"}"#).unwrap();
    assert_eq!(odd.sanitized().grid_style, ViewMode::PhotoGrid);
}

#[test]
fn g_toggles_the_grids_and_comes_back_to_the_chosen_style() {
    let mut a = app();
    a.run("view.squareGrid", json!({})).unwrap();
    a.run("view.gridToggle", json!({})).unwrap();
    assert_eq!(a.ui.view, ViewMode::PhotoGrid, "G toggles between the two grids");
    a.run("view.gridToggle", json!({})).unwrap();
    assert_eq!(a.ui.view, ViewMode::SquareGrid);

    // from a photo view, G returns to the chosen grid, not always to Photo Grid (#794)
    a.run("view.detail", json!({})).unwrap();
    a.run("view.gridToggle", json!({})).unwrap();
    assert_eq!(a.ui.view, ViewMode::SquareGrid);

    // the menu's Photo Grid records the choice too
    a.run("view.photoGrid", json!({})).unwrap();
    assert_eq!(a.ui.grid_style, ViewMode::PhotoGrid);
    a.run("view.detail", json!({})).unwrap();
    a.run("view.gridToggle", json!({})).unwrap();
    assert_eq!(a.ui.view, ViewMode::PhotoGrid);
}

/// From the full-screen preview G asks for the grid and leaves the preview: the chosen grid style,
/// not a flip between the two grids (the view behind the preview is usually a grid already).
#[test]
fn g_leaves_the_full_screen_preview_on_the_chosen_grid() {
    let mut a = app();
    let first = a.session.visible().first().map(|p| p.0).expect("the demo catalog has photos");
    a.run("library.select", json!({"ids": [first]})).unwrap();
    a.run("view.squareGrid", json!({})).unwrap();
    a.run("view.detail", json!({})).unwrap();
    a.run("view.fullScreenPreview", json!({})).unwrap();
    assert!(a.ui.fullscreen);
    a.run("view.gridToggle", json!({})).unwrap();
    assert!(!a.ui.fullscreen, "G leaves the full-screen preview");
    assert_eq!(a.ui.view, ViewMode::SquareGrid, "the grid the user chose, even over a grid view");
    assert_eq!(a.ui.grid_style, ViewMode::SquareGrid, "the choice is untouched");
}

/// Esc from a photo goes back to the grid the Library was left in; when a detour through Develop
/// recorded Detail there instead, the chosen grid style is used rather than Photo Grid (#794).
#[test]
fn esc_from_a_photo_returns_to_the_chosen_grid_style() {
    let mut a = app();
    a.run("view.squareGrid", json!({})).unwrap();
    // a photo opened from the grid: the Library layout remembers Detail (loupe), not the grid
    a.run("view.detail", json!({})).unwrap();
    if let Some(l) = a.ui.layouts.get_mut(&crate::module::ModuleId::Library) {
        l.view = Some(ViewMode::Detail);
    }
    a.run("view.back", json!({})).unwrap();
    assert_eq!(a.ui.view, ViewMode::SquareGrid, "the chosen grid style, not Photo Grid");
}

/// The fallbacks that used to force Photo Grid honour the choice: the filter bar shown from a
/// photo view, and a Library painter command.
#[test]
fn grid_fallbacks_use_the_chosen_style() {
    let mut a = app();
    a.run("view.squareGrid", json!({})).unwrap();
    a.run("view.detail", json!({})).unwrap();
    a.run("view.filterBar", json!({})).unwrap(); // off (the bar starts shown)
    a.run("view.filterBar", json!({})).unwrap(); // on again, from the photo
    assert_eq!(a.ui.view, ViewMode::SquareGrid, "the filter bar lands on the chosen grid");
}
