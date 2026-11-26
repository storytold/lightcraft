//! A prompt drawn over a dialog takes the Escape pressed in the prompt's first pass: the focused field of the dialog
//! under it keeps its typing, and the dialog stays open. The real case of the covered-dialog tests that pass `blocked`
//! to `dialogs::show` without drawing a prompt.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn escape(pressed: bool) -> egui::Event {
    egui::Event::Key { key: egui::Key::Escape, physical_key: Some(egui::Key::Escape), pressed, repeat: false, modifiers: egui::Modifiers::NONE }
}

fn album_name(h: &Headless) -> Option<String> {
    match &h.app.ui.dialog {
        Some(Dialog::NewAlbum { name, .. }) => Some(name.clone()),
        _ => None,
    }
}

/// A New Album dialog with "Only a draft" typed into its focused name field.
fn typed_draft() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    h.step();
    h.app.run("dialog.newAlbum", json!({"name": ""})).expect("New Album opens");
    h.settle(T);
    let r = h.request("ui.clickWidget", json!({"id": "field:albumName"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.text", json!({"text": "Only a draft"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(album_name(&h).as_deref(), Some("Only a draft"), "the typing reached the field");
    h
}

#[test]
fn an_escape_in_a_prompts_first_pass_leaves_the_covered_dialogs_field_alone() {
    let mut h = typed_draft();
    // The quit prompt appears in the very frame the Escape arrives in, after the host read its input.
    h.events.push(escape(true));
    h.set_after_raw_input_hook(|app| app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into())));
    h.step();
    h.events.push(escape(false));
    h.step();
    h.step();
    assert_eq!(album_name(&h).as_deref(), Some("Only a draft"), "the Escape did not cancel the covered field's typing or close its dialog");
}

#[test]
fn an_escape_at_a_settled_prompt_leaves_the_covered_dialogs_field_alone() {
    let mut h = typed_draft();
    h.app.quit_prompt = Some(crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
    for _ in 0..3 {
        h.step();
    }
    h.events.push(escape(true));
    h.step();
    h.events.push(escape(false));
    h.step();
    h.step();
    assert_eq!(album_name(&h).as_deref(), Some("Only a draft"), "the Escape did not cancel the covered field's typing or close its dialog");
}

/// The control: with no prompt, the same Escape reaches the focused field, which closes its dialog (a dialog's one
/// field cancels the dialog on Escape), so the two tests above would see a leak.
#[test]
fn without_a_prompt_the_escape_closes_the_dialog() {
    let mut h = typed_draft();
    h.events.push(escape(true));
    h.step();
    h.events.push(escape(false));
    h.step();
    h.step();
    assert_eq!(album_name(&h), None, "the field's Escape closed New Album");
}
