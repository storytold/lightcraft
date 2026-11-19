//! What a screen reader is told about the Tone Curve's eye: which channel it hides, and whether
//! that channel is hidden now, not only "Show / hide".
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};

#[test]
fn the_curve_eye_names_its_channel_and_says_when_it_is_hidden() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.headless_host = true;
    app.ui.view = ViewMode::Detail;
    app.ui.right = RightPanel::Edit;
    if !app.ui.section_open("light") {
        app.ui.toggle_section("light");
    }
    if !app.ui.flyout_open("curve") {
        app.ui.toggle_flyout("curve");
    }
    app.ui.curve_channel = "red".into();
    let id = app.session.active().expect("the demo has a photo");
    // hidden: the eye shows (and is named) without hovering the graph
    app.ui.set_curve_hidden(id, "red", true);
    let mut h = Harness::builder().with_size(vec2(1400.0, 1400.0)).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    let eye = h
        .root()
        .children_recursive()
        .find(|n| n.accesskit_node().label().is_some_and(|l| l == "Red channel — Show / hide"))
        .expect("the eye is named after its channel");
    // (egui publishes a selected button as toggled on)
    assert_eq!(eye.accesskit_node().toggled(), Some(egui::accesskit::Toggled::True), "and says the channel is hidden");
}
