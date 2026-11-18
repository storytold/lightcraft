//! Real library/tool panel moves preserve the selected tool and saved arrangement.
use egui::vec2;
use egui_kittest::Harness;
use lcc_ui_egui::{LccApp, Services, UiState, state::RightPanel};
use serde_json::json;

fn harness(state: Option<UiState>, width: f32, scale: f32) -> Harness<'static, LccApp> {
    lcc_engine::scene_disk::use_build_dir();
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    if let Some(state) = state {
        app.ui = state;
    } else {
        app.ui.left_panel = true;
        app.ui.right = RightPanel::Edit;
    }
    let mut h = Harness::builder().with_size(vec2(width, 900.0)).with_pixels_per_point(scale).build_ui_state(
        |ui, app: &mut LccApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(8);
    h
}

#[test]
fn invalid_docking_requests_preserve_layout_and_selected_tool() {
    let mut h = harness(None, 1400.0, 1.0);
    let before = serde_json::to_value(&h.state().ui.docking).unwrap();
    for params in [
        json!({"operation":"close","panel":"canvas"}),
        json!({"operation":"move","panel":"tools","target":"canvas","zone":"center"}),
        json!({"operation":"float","panel":"tools","rect":[0,0,-1,100]}),
        json!({"operation":"move","panel":"unknown","target":"library","zone":"center"}),
    ] {
        assert!(h.state_mut().run("ui.dock", params).is_err());
        assert_eq!(serde_json::to_value(&h.state().ui.docking).unwrap(), before);
        assert_eq!(h.state().ui.right, RightPanel::Edit);
    }
}
