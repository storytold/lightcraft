//! What a screen reader (and Voice Control) is told about the photo grid's date headers: the name
//! holds the text the header shows, so "click Sunday" finds it and VoiceOver reads what a sighted
//! person reads.
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};

fn grid() -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = false;
    app.ui.view = ViewMode::PhotoGrid;
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
fn a_date_header_is_named_with_the_text_it_shows() {
    let h = grid();
    let buttons: Vec<String> =
        h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::Button).filter_map(|n| n.accesskit_node().label()).collect();
    // the demo library's newest day, as the header paints it: its long date, then its count
    let shown = lightcraft_ui_egui::i18n::date_group_label("2026-09-20", false);
    let header = buttons.iter().find(|name| name.contains(&shown));
    let header = header.unwrap_or_else(|| panic!("a header named with {shown:?}: {buttons:?}"));
    assert!(header.contains("1 photo"), "and its count: {header:?}");
    assert!(!buttons.iter().any(|name| name == "2026-09-20"), "the bare key is not a name: {buttons:?}");
}
