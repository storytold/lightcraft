//! The window's parts are named for screen readers: the three bars are toolbars, and the sidebar, the
//! photos, the tool panel and the filmstrip are landmarks, so VoiceOver's rotor lists them and a
//! person can jump to one instead of walking every control.
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};

fn harness(view: ViewMode) -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    app.ui.view = view;
    app.ui.right = RightPanel::Edit;
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

/// The area each node with this role and this name covers (`None`: it reports none).
fn areas(h: &Harness<'_, LightcraftApp>, role: Role, name: &str) -> Vec<Option<egui::accesskit::Rect>> {
    let nodes = h.root().children_recursive().map(|n| n.accesskit_node());
    nodes.filter(|n| n.role() == role && n.label().as_deref() == Some(name)).map(|n| n.bounding_box()).collect()
}

/// How many nodes have this role and this name.
fn count(h: &Harness<'_, LightcraftApp>, role: Role, name: &str) -> usize {
    areas(h, role, name).len()
}

#[test]
fn the_bars_are_toolbars_and_the_panels_are_landmarks_in_detail() {
    let h = harness(ViewMode::Detail);
    for (role, name) in [
        (Role::Toolbar, "Top bar"),
        (Role::Toolbar, "Tool strip"),
        (Role::Toolbar, "Bottom bar"),
        (Role::Navigation, "Library"),
        (Role::Main, "Photos"),
        (Role::Complementary, "Tools"),
        (Role::Region, "Filmstrip"),
    ] {
        assert_eq!(count(&h, role, name), 1, "one {role:?} named {name:?}");
        let area = areas(&h, role, name).into_iter().next().flatten();
        assert!(area.is_some_and(|b| b.width() > 0.0 && b.height() > 0.0), "{name:?} reports the area it covers: {area:?}");
    }
}

#[test]
fn the_grid_is_the_main_landmark_in_the_photo_grid() {
    let h = harness(ViewMode::PhotoGrid);
    assert_eq!(count(&h, Role::Main, "Photos"), 1);
    assert_eq!(count(&h, Role::Region, "Filmstrip"), 0, "the grid has no filmstrip");
}

/// A filter that hides every photo leaves the filmstrip drawn with its reason: it stays a landmark
/// and the reason is text a screen reader reads, not paint.
#[test]
fn an_empty_filmstrip_is_still_a_landmark_and_says_why() {
    let mut session = lightcraft_engine::Session::with_demo();
    session.execute("library.filter", &serde_json::json!({"keyword": "no-such-keyword"})).expect("the filter is set");
    assert!(session.visible_cloned().is_empty(), "the filter hides every photo");
    let mut app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    app.ui.view = ViewMode::Detail;
    app.ui.right = RightPanel::Edit;
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    assert_eq!(count(&h, Role::Region, "Filmstrip"), 1, "the empty strip is still a landmark");
    let said = h
        .root()
        .children_recursive()
        .filter_map(|n| n.accesskit_node().value().or_else(|| n.accesskit_node().label()))
        .any(|t| t.starts_with("No photos match the filters"));
    assert!(said, "the reason is read, not only painted");
}
