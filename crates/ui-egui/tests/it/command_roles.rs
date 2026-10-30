//! A button that does something is announced as a button, and one that is on or off as one that says so.
//! AccessKit reports a button that carries an on / off state as a check box, so a command that
//! carried one ("Reset, checkbox, unchecked") told a VoiceOver user it was a setting.
use egui::accesskit::{Role, Toggled};
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};

fn harness(view: ViewMode, right: RightPanel) -> Harness<'static, LightcraftApp> {
    let services = Services { png: None, pick_folder: Some(Box::new(|| None)), ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.left_panel = true;
    app.ui.view = view;
    app.ui.right = right;
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

/// Every node named `name` (with or without its shortcut in brackets after it): its role and its on / off state.
fn named(h: &Harness<'_, LightcraftApp>, name: &str) -> Vec<(Role, Option<Toggled>)> {
    h.root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .filter(|n| n.label().is_some_and(|l| l == name || l.starts_with(&format!("{name} ("))))
        .map(|n| (n.role(), n.data().toggled()))
        .collect()
}

fn assert_command(h: &Harness<'_, LightcraftApp>, name: &str) {
    let found = named(h, name);
    assert!(!found.is_empty(), "{name:?} is in the tree");
    for (role, toggled) in found {
        assert_eq!((role, toggled), (Role::Button, None), "{name:?} is a button that does something, not a check box");
    }
}

fn assert_choice(h: &Harness<'_, LightcraftApp>, name: &str) {
    let found = named(h, name);
    assert!(!found.is_empty(), "{name:?} is in the tree");
    for (role, toggled) in found {
        assert!(
            role == Role::RadioButton && toggled.is_some(),
            "{name:?} is one choice of several: a radio button that says whether it is chosen ({role:?}, {toggled:?})"
        );
    }
}

fn assert_toggle(h: &Harness<'_, LightcraftApp>, name: &str) {
    let found = named(h, name);
    assert!(!found.is_empty(), "{name:?} is in the tree");
    for (role, toggled) in found {
        assert!(role == Role::Button && toggled.is_some(), "{name:?} is a button that says whether it is on ({role:?}, {toggled:?})");
    }
}

#[test]
fn the_edit_panels_commands_are_buttons_and_its_toggles_say_whether_they_are_on() {
    let h = harness(ViewMode::Detail, RightPanel::Edit);
    for name in ["Auto", "Reset", "Browse Profiles"] {
        assert_command(&h, name);
    }
    for name in ["B&W", "HDR"] {
        assert_toggle(&h, name);
    }
}

#[test]
fn the_top_bars_back_button_is_a_button_and_its_panel_switch_a_toggle() {
    let h = harness(ViewMode::PhotoGrid, RightPanel::None);
    assert_command(&h, "Back");
    assert_command(&h, "Forward");
    assert_toggle(&h, "Show/hide My Photos panel");
    for name in ["Photo Grid", "Detail", "Compare", "Survey"] {
        assert_choice(&h, name);
    }
}

#[test]
fn the_sidebars_create_and_browse_rows_are_buttons_and_its_sources_say_whether_they_are_shown() {
    let h = harness(ViewMode::PhotoGrid, RightPanel::None);
    for name in ["Create Album", "Create Keyword Tag", "Browse Folder…"] {
        assert_command(&h, name);
    }
    let all: Vec<_> = named(&h, "All Photos, 24 photos");
    assert!(all.iter().all(|(_, toggled)| toggled.is_some()) && !all.is_empty(), "the source row says whether it is shown: {all:?}");
}

#[test]
fn a_dialogs_save_button_is_a_button_and_its_choices_say_which_is_chosen() {
    let mut h = harness(ViewMode::PhotoGrid, RightPanel::None);
    h.state_mut().run("dialog.export", serde_json::json!({})).expect("Export opens");
    h.run_steps(6);
    assert_command(&h, "Save");
    assert_choice(&h, "JPEG");
    assert_choice(&h, "sRGB");
}

#[test]
fn settings_languages_are_choices() {
    let mut h = harness(ViewMode::PhotoGrid, RightPanel::None);
    h.state_mut().run("app.settings", serde_json::json!({})).expect("Settings opens");
    h.run_steps(6);
    for name in ["English", "Deutsch"] {
        assert_choice(&h, name);
    }
}
