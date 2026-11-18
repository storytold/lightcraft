//! What a screen reader is told about a filmstrip thumbnail: the same name as the grid's cell for
//! the same photo (file, rating, flag, label) and whether it is selected, not just the file name.
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};
use serde_json::json;

/// The demo library with its first four photos given the data a name is made of, so nothing here
/// depends on what the demo happens to carry: one star, three stars, picked with a label, rejected.
fn harness(view: ViewMode) -> (Harness<'static, LightcraftApp>, Vec<String>) {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let shown: Vec<_> = app.session.visible_cloned().into_iter().take(4).collect();
    assert_eq!(shown.len(), 4, "the demo library has at least four photos");
    let [one, three, picked, rejected] = [shown[0].0, shown[1].0, shown[2].0, shown[3].0];
    app.run("photo.rate", json!({"rating": 1, "ids": [one]})).unwrap();
    app.run("photo.rate", json!({"rating": 3, "ids": [three]})).unwrap();
    app.run("photo.flag", json!({"flag": "pick", "ids": [picked]})).unwrap();
    app.run("photo.label", json!({"label": "red", "ids": [picked]})).unwrap();
    app.run("photo.flag", json!({"flag": "reject", "ids": [rejected]})).unwrap();
    let files = shown.iter().map(|id| app.session.catalog.photo(*id).map(|p| p.file_name.clone()).expect("a demo photo")).collect();
    app.ui.left_panel = false;
    app.ui.view = view;
    app.ui.right = RightPanel::None;
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    (h, files)
}

/// The names of the cells the screen reader meets (list items).
fn cells(h: &Harness<'_, LightcraftApp>) -> Vec<String> {
    h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::ListItem).filter_map(|n| n.accesskit_node().label()).collect()
}

/// The one cell named for `file`: its name is the file name followed by `, …` or nothing, so a file
/// whose name begins another's is not taken for it.
fn cell_for(cells: &[String], file: &str) -> String {
    let named: Vec<&String> = cells.iter().filter(|c| c.as_str() == file || c.starts_with(&format!("{file}, "))).collect();
    assert_eq!(named.len(), 1, "exactly one cell is named for {file}: {cells:?}");
    named[0].clone()
}

#[test]
fn a_filmstrip_thumbnail_is_named_exactly_like_the_grid_cell_for_the_same_photo() {
    let (grid, files) = harness(ViewMode::PhotoGrid);
    let (film, _) = harness(ViewMode::Detail);
    let (grid, film) = (cells(&grid), cells(&film));
    for file in &files {
        assert_eq!(cell_for(&film, file), cell_for(&grid, file), "the filmstrip says what the grid says for {file}");
    }
}

#[test]
fn a_filmstrip_thumbnail_is_named_with_its_rating_flag_and_label() {
    let (h, files) = harness(ViewMode::Detail);
    let cells = cells(&h);
    let [one, three, picked, rejected] = [0, 1, 2, 3].map(|i| cell_for(&cells, &files[i]));
    assert!(one.ends_with(", 1 star"), "{one:?} says its one star, in the singular");
    assert!(three.ends_with(", 3 stars"), "{three:?} says its three stars");
    assert!(picked.contains(", picked"), "{picked:?} says it is picked");
    assert!(picked.ends_with(" label"), "{picked:?} names its color label");
    assert!(rejected.ends_with(", rejected"), "{rejected:?} says it is rejected");
}

#[test]
fn the_active_filmstrip_thumbnail_is_announced_as_selected() {
    let (h, _) = harness(ViewMode::Detail);
    let active = h.state().session.selection.active.and_then(|id| h.state().session.catalog.photo(id).map(|p| p.file_name.clone()));
    let active = active.expect("the demo library opens with an active photo");
    let cells = cells(&h);
    let name = cell_for(&cells, &active);
    let node = h
        .root()
        .children_recursive()
        .find(|n| n.accesskit_node().role() == Role::ListItem && n.accesskit_node().label().as_deref() == Some(name.as_str()))
        .unwrap_or_else(|| panic!("a filmstrip thumbnail named {name:?}"));
    // (a selectable cell with its selected state on, as the grid's cells are)
    assert_eq!(node.accesskit_node().is_selected(), Some(true), "the active photo's thumbnail is selected: {:?}", node.accesskit_node().data());
}
