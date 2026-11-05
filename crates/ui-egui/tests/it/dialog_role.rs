//! An open dialog is exposed to a screen reader as a dialog (a modal one, an alert dialog for the
//! prompts), not as a group that happens to carry the dialog's title.
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{LightcraftApp, Services};

fn harness() -> Harness<'static, LightcraftApp> {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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

/// `(role, title, modal)` of every window-like node in the tree.
fn windows(h: &Harness<'_, LightcraftApp>) -> Vec<(Role, String, bool)> {
    h.root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .filter(|n| matches!(n.role(), Role::Window | Role::Dialog | Role::AlertDialog))
        .map(|n| (n.role(), n.label().unwrap_or_default(), n.data().is_modal()))
        .collect()
}

#[test]
fn every_dialog_is_a_modal_dialog_with_its_title() {
    for command in ["dialog.export", "dialog.rename", "dialog.newAlbum", "dialog.createPreset", "dialog.contactSheet", "dialog.allMetadata"] {
        let mut h = harness();
        h.state_mut().run(command, serde_json::json!({})).unwrap_or_else(|e| panic!("{command}: {e}"));
        h.run_steps(6);
        let found = windows(&h);
        assert!(!found.is_empty(), "{command}: the dialog is in the tree");
        for (role, title, modal) in found {
            assert_eq!(role, Role::Dialog, "{command}: {title:?} is announced as a dialog");
            assert!(!title.trim().is_empty(), "{command}: the dialog has its title");
            assert!(modal, "{command}: {title:?} is modal, so the rest of the window is not read");
        }
    }
}

#[test]
fn settings_is_a_dialog_too() {
    let mut h = harness();
    h.state_mut().run("app.settings", serde_json::json!({})).expect("Settings opens");
    h.run_steps(6);
    let found = windows(&h);
    assert!(found.iter().any(|(role, title, _)| *role == Role::Dialog && title.as_str() == "Settings"), "{found:?}");
    assert!(found.iter().all(|(role, ..)| *role != Role::Window), "{found:?}");
}

#[test]
fn the_quit_prompt_and_a_settings_notice_are_modal_alert_dialogs() {
    let mut h = harness();
    h.state_mut().quit_prompt = Some(lightcraft_ui_egui::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.run_steps(6);
    let found = windows(&h);
    assert!(!found.is_empty(), "the quit prompt is in the tree");
    for (role, title, modal) in found {
        assert_eq!((role, modal), (Role::AlertDialog, true), "{title:?} interrupts with a question: a modal alert dialog");
    }
    let mut h = harness();
    h.state_mut().notices.push("Settings could not be saved.".into());
    h.run_steps(6);
    let found = windows(&h);
    assert!(!found.is_empty(), "the notice is in the tree");
    for (role, title, modal) in found {
        assert_eq!((role, modal), (Role::AlertDialog, true), "{title:?} is a modal alert dialog");
    }
}

#[test]
fn a_library_that_could_not_be_opened_is_a_modal_alert_dialog() {
    let mut h = harness();
    h.state_mut().library_problem = Some(lightcraft_ui_egui::panels::library_problem::LibraryProblem::new("/no/such/library", "No catalog here."));
    h.run_steps(6);
    let found = windows(&h);
    assert!(!found.is_empty(), "the library problem is in the tree");
    for (role, title, modal) in found {
        assert_eq!((role, modal), (Role::AlertDialog, true), "{title:?} is a modal alert dialog");
    }
}

/// The description of every alert dialog in the tree.
fn alert_descriptions(h: &Harness<'_, LightcraftApp>) -> Vec<String> {
    h.root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .filter(|n| n.role() == Role::AlertDialog)
        .map(|n| n.data().description().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn an_alert_carries_what_it_says_as_its_description() {
    let mut h = harness();
    h.state_mut().quit_prompt = Some(lightcraft_ui_egui::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    h.run_steps(6);
    let said = alert_descriptions(&h);
    assert!(said.iter().any(|d| d.contains("1 task is still running: Import")), "the quit prompt names the task: {said:?}");
    let mut h = harness();
    h.state_mut().library_problem = Some(lightcraft_ui_egui::panels::library_problem::LibraryProblem::new("/no/such/library", "No catalog here."));
    h.run_steps(6);
    let said = alert_descriptions(&h);
    assert!(
        said.iter().any(|d| d.contains("/no/such/library") && d.contains("No catalog here.") && d.contains("untouched")),
        "the library alert has its path, error and explanation: {said:?}"
    );
}
