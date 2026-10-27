//! What a screen reader is told about a filmstrip thumbnail: the same name as the grid's cell for
//! the same photo (file, rating, flag) and whether it is selected, not just the file name.
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_catalog::Flag;
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};

fn detail() -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = false;
    app.ui.view = ViewMode::Detail;
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

#[test]
fn a_filmstrip_thumbnail_is_named_with_its_rating_and_flag() {
    let h = detail();
    let buttons: Vec<String> =
        h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::Button).filter_map(|n| n.accesskit_node().label()).collect();
    let mut rated = 0;
    let mut flagged = 0;
    for photo in h.state().session.catalog.photos() {
        let Some(name) = buttons.iter().find(|b| b.starts_with(photo.file_name.as_str())) else { continue };
        if photo.rating > 0 {
            rated += 1;
            let stars = format!("{} star", photo.rating);
            assert!(name.contains(&stars), "{name:?} says its {stars}");
        }
        if photo.flag == Flag::Reject {
            flagged += 1;
            assert!(name.contains("rejected"), "{name:?} says it is rejected");
        }
    }
    assert!(rated >= 3, "the filmstrip shows several rated photos ({rated}): {buttons:?}");
    assert!(flagged >= 1, "and a rejected one ({flagged}): {buttons:?}");
}

#[test]
fn the_active_filmstrip_thumbnail_is_announced_as_selected() {
    let h = detail();
    let active = h.state().session.selection.active.and_then(|id| h.state().session.catalog.photo(id).map(|p| p.file_name.clone()));
    let active = active.expect("the demo library opens with an active photo");
    let node = h
        .root()
        .children_recursive()
        .find(|n| n.accesskit_node().role() == Role::Button && n.accesskit_node().label().is_some_and(|l| l.starts_with(active.as_str())))
        .unwrap_or_else(|| panic!("a filmstrip thumbnail named {active:?}"));
    // (egui announces a selected button as toggled on, as the grid's cells are)
    let toggled = node.accesskit_node().toggled();
    assert_eq!(toggled, Some(egui::accesskit::Toggled::True), "the active photo's thumbnail is selected: {:?}", node.accesskit_node().data());
}
