//! The photo shown in Detail has a name a screen reader can read: its file, and its rating, flag and
//! label as the grid says them, not the bare word "Photo".
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};

fn detail() -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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

/// The names of the photo's nodes ("Photo: <file>…"); the in-window "Photo" menu button is not the photo.
fn photo_names(h: &Harness<'_, LightcraftApp>) -> Vec<String> {
    h.root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .filter(|n| n.role() == Role::Button)
        .filter_map(|n| n.label())
        .filter(|l| l.starts_with("Photo: "))
        .collect()
}

#[test]
fn the_photo_in_detail_is_named_with_its_file() {
    let h = detail();
    let app = h.state();
    let file = app.session.active().and_then(|id| app.session.catalog.photo(id)).map(|p| p.file_name.clone()).expect("a photo is shown");
    assert_eq!(photo_names(&h), vec![format!("Photo: {file}")]);
}

#[test]
fn the_photo_in_detail_says_its_rating_and_flag_as_the_grid_does() {
    let mut h = detail();
    h.state_mut().run("photo.rate", serde_json::json!({"rating": 4})).expect("rates");
    h.state_mut().run("photo.flag", serde_json::json!({"flag": "pick"})).expect("flags");
    h.run_steps(4);
    let names = photo_names(&h);
    assert!(names.len() == 1 && names[0].ends_with(", 4 stars, picked"), "{names:?}");
}
