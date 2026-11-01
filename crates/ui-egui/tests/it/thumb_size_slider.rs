//! The grid's thumbnail size control, as a screen reader meets it: a slider with its value and range
//! that Increment and Decrement move, not a button with no value.
use egui::accesskit::{Action, ActionRequest, Role};
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, THUMB_SIZE_MAX, THUMB_SIZE_MIN, THUMB_SIZE_STEP, ViewMode},
};

const NAME: &str = "Thumbnail size";

fn grid() -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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

/// The node named "Thumbnail size": its role, value, range and the actions it takes.
fn node(h: &Harness<'_, LightcraftApp>) -> Option<(Role, Option<f64>, Option<f64>, Option<f64>, bool, bool)> {
    h.root().children_recursive().map(|n| n.accesskit_node()).find(|n| n.label().as_deref() == Some(NAME)).map(|n| {
        (
            n.role(),
            n.numeric_value(),
            n.min_numeric_value(),
            n.max_numeric_value(),
            n.data().supports_action(Action::Increment),
            n.data().supports_action(Action::Decrement),
        )
    })
}

#[test]
fn the_thumbnail_size_is_a_slider_with_its_value_and_range() {
    let h = grid();
    let (role, value, min, max, inc, dec) = node(&h).expect("the grid's bottom bar has a thumbnail size control");
    assert_eq!(role, Role::Slider);
    assert_eq!(value, Some(f64::from(h.state().ui.thumb_size)));
    assert_eq!((min, max), (Some(f64::from(THUMB_SIZE_MIN)), Some(f64::from(THUMB_SIZE_MAX))));
    assert!(inc && dec, "a screen reader can raise and lower it");
}

#[test]
fn a_screen_reader_increment_and_decrement_move_the_thumbnail_size_one_step() {
    let mut h = grid();
    let start = h.state().ui.thumb_size;
    for (action, want) in [(Action::Increment, start + THUMB_SIZE_STEP), (Action::Decrement, start)] {
        let target = h.root().children_recursive().find(|n| n.accesskit_node().label().as_deref() == Some(NAME)).unwrap().accesskit_node().locate();
        h.event(egui::Event::AccessKitActionRequest(ActionRequest { action, target_node: target.0, target_tree: target.1, data: None }));
        h.run_steps(4);
        assert_eq!(h.state().ui.thumb_size, want, "{action:?}");
    }
}

#[test]
fn the_arrow_keys_step_the_thumbnail_size_and_keep_the_focus_on_it() {
    let mut h = grid();
    let start = h.state().ui.thumb_size;
    h.root().children_recursive().find(|n| n.accesskit_node().label().as_deref() == Some(NAME)).expect("the control").focus();
    h.run_steps(3);
    for step in 1..=2 {
        h.key_press(egui::Key::ArrowDown);
        h.run_steps(3);
        assert_eq!(h.state().ui.thumb_size, start - step as f32 * THUMB_SIZE_STEP, "Down, press {step}");
    }
    for step in 1..=2 {
        h.key_press(egui::Key::ArrowUp);
        h.run_steps(3);
        assert_eq!(h.state().ui.thumb_size, start - 2.0 * THUMB_SIZE_STEP + step as f32 * THUMB_SIZE_STEP, "Up, press {step}");
    }
}
