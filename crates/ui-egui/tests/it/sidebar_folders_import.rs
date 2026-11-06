//! What the sidebar tells a screen reader and a hovering pointer about Folders and Local: an empty
//! Folders names its two import rows, and Local's header says it is the computer's own folders,
//! browsed without adding them.
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    i18n::{Locale, set_language},
    state::{RightPanel, ViewMode},
};

const LOCAL_HINT: &str = "Folders on this computer, browsed without adding them";

/// The demo library (imported from no folder: Folders is empty) with Local listing no places.
fn sidebar() -> Harness<'static, LightcraftApp> {
    sidebar_in(Locale::En)
}

fn sidebar_in(language: Locale) -> Harness<'static, LightcraftApp> {
    let services = Services {
        png: None,
        local_places: Some(Vec::new()),
        pick_folder: Some(Box::new(|| None)),
        pick_files: Some(Box::new(Vec::new)),
        ..Default::default()
    };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.headless_host = true;
    app.ui.left_panel = true;
    app.ui.view = ViewMode::PhotoGrid;
    app.ui.right = RightPanel::None;
    app.ui.language = language;
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    h
}

fn buttons(h: &Harness<'_, LightcraftApp>) -> Vec<String> {
    h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::Button).filter_map(|n| n.accesskit_node().label()).collect()
}

#[test]
fn an_empty_folders_names_its_two_import_rows() {
    let h = sidebar();
    let names = buttons(&h);
    let count = |name: &str| names.iter().filter(|n| *n == name).count();
    assert_eq!(count("Import from Folder…"), 1, "{names:?}");
    // (the top bar's Import Photos… button is the other one)
    assert_eq!(count("Import Photos…"), 2, "{names:?}");
}

#[test]
fn hovering_local_says_it_is_the_computers_own_folders() {
    let mut h = sidebar();
    assert!(h.query_by_label(LOCAL_HINT).is_none(), "no tooltip until the pointer is there");
    h.get_by_label("Local").hover();
    h.run_steps(30);
    assert!(h.query_by_label(LOCAL_HINT).is_some(), "the hover line is shown");
}

/// The hover line comes through the catalog, like every other piece of text.
#[test]
fn the_local_hover_line_is_translated() {
    let hint = Locale::De.tr(LOCAL_HINT).to_string();
    assert_ne!(hint, LOCAL_HINT, "the German catalog has it");
    let mut h = sidebar_in(Locale::De);
    let local = Locale::De.tr("Local").to_string();
    h.get_by_label(&local).hover();
    h.run_steps(30);
    let shown = h.query_by_label(&hint).is_some();
    set_language(Locale::En);
    assert!(shown, "the German hover line is shown");
}
