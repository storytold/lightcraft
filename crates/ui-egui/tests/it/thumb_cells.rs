//! A photo's thumbnail, in the grid and in the filmstrip, is a selectable cell: a screen reader
//! (macOS VoiceOver) reads "selected" for it, not a check box that is "checked". AccessKit's macOS
//! adapter turns a node with a toggled state into a check box, and says "selected" for an item-like
//! node (a list item) that carries a selected state, so the cell is a `ListItem` with `selected`.
use egui::accesskit::{Role, Toggled};
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_engine::Selection;
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};

fn harness(view: ViewMode) -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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
    h
}

/// What the accessibility tree says about the cell named `file`: (role, selected, toggled).
fn cell(h: &Harness<'_, LightcraftApp>, file: &str) -> Option<(Role, Option<bool>, Option<Toggled>)> {
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with(file)))
        .map(|n| {
            let a = n.accesskit_node();
            (a.role(), a.is_selected(), a.toggled())
        })
        .find(|(role, _, _)| matches!(role, Role::ListItem | Role::Button | Role::CheckBox))
}

fn two_photos(h: &mut Harness<'_, LightcraftApp>) -> (lightcraft_catalog::PhotoId, String, String) {
    let mut shown = h.state_mut().session.visible_cloned().into_iter();
    let cat = &h.state().session.catalog;
    let (a, b) = (shown.next().expect("the demo library has photos"), shown.next().expect("and a second one"));
    let name = |id| cat.photo(id).map(|p| p.file_name.clone()).expect("a demo photo");
    (a, name(a), name(b))
}

fn assert_selectable_cells(view: ViewMode) {
    let mut h = harness(view);
    let (first, first_name, second_name) = two_photos(&mut h);
    h.state_mut().session.selection = Selection::single(first);
    h.run_steps(5);
    let (role, selected, toggled) = cell(&h, &first_name).expect("the selected photo has a cell");
    assert_eq!(role, Role::ListItem, "{first_name}: a selectable cell, not a button that macOS reads as a check box");
    assert_eq!(selected, Some(true), "{first_name} reads as selected");
    assert_eq!(toggled, None, "{first_name} has no toggled state (macOS would call it a check box)");
    let (role, selected, toggled) = cell(&h, &second_name).expect("the other photo has a cell");
    assert_eq!(role, Role::ListItem);
    assert_eq!(selected, Some(false), "{second_name} is a selectable cell that is not selected");
    assert_eq!(toggled, None);

    // a selection change reaches the tree
    let second = h.state_mut().session.visible_cloned().get(1).copied().expect("a second photo");
    h.state_mut().session.selection = Selection::single(second);
    h.run_steps(5);
    assert_eq!(cell(&h, &first_name).map(|c| c.1), Some(Some(false)), "{first_name} is deselected");
    assert_eq!(cell(&h, &second_name).map(|c| c.1), Some(Some(true)), "{second_name} is selected");
}

#[test]
fn a_grid_cell_is_a_selectable_cell() {
    assert_selectable_cells(ViewMode::PhotoGrid);
}

#[test]
fn a_filmstrip_thumbnail_is_a_selectable_cell() {
    assert_selectable_cells(ViewMode::Detail);
}
