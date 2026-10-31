//! Esc in a text box of Settings gives the text back and leaves Settings open, and text typed in a
//! box is saved when another tab is clicked.
//!
//! Before: Esc in Copyright or Creator saved what had been typed and closed the window, and text
//! typed there was dropped when another tab was clicked.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1200.0, 900.0], 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn steps(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}

fn show(h: &mut Headless, tab: &str) {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": tab}}), T);
    assert_eq!(r["ok"], true, "app.settings {tab}: {r}");
    h.settle(T);
    steps(h);
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "click {id}: {r}");
    steps(h);
}

fn key(h: &mut Headless, key: &str) {
    assert_eq!(h.request("ui.key", json!({"key": key}), T)["ok"], true, "{key}");
    steps(h);
}

fn says(h: &Headless, part: &str) -> bool {
    h.painted_text().iter().any(|text| text.contains(part))
}

fn settings_open(h: &Headless, tab: &str) -> bool {
    h.app.ui.dialog == Some(Dialog::Settings { tab: tab.into() })
}

fn copyright(h: &mut Headless) -> Value {
    h.app.run("library.preferences", json!({})).unwrap()["import"]["copyright"].clone()
}

#[test]
fn esc_in_copyright_gives_the_text_back_and_leaves_settings_open() {
    let mut h = demo();
    show(&mut h, "import");
    let before = copyright(&mut h);
    // Given a name typed into Copyright
    click(&mut h, "field:settings.copyright");
    h.request("ui.text", json!({"text": "Example Name"}), T);
    steps(&mut h);
    assert!(says(&h, "Example Name"), "the typing shows");
    // When Esc is pressed
    key(&mut h, "Escape");
    // Then the name is gone again, nothing was saved, and Settings is still open
    assert!(!says(&h, "Example Name"), "Esc gives the text back");
    assert_eq!(copyright(&mut h), before, "nothing was saved");
    assert!(settings_open(&h, "import"), "Settings stays open: {:?}", h.app.ui.dialog);
    // And a second Esc, with no edit under way, closes Settings as it always did
    key(&mut h, "Escape");
    assert!(h.app.ui.dialog.is_none());
}

#[test]
fn return_in_copyright_saves_the_text() {
    let mut h = demo();
    show(&mut h, "import");
    click(&mut h, "field:settings.copyright");
    h.request("ui.text", json!({"text": "Example Name"}), T);
    steps(&mut h);
    key(&mut h, "Enter");
    assert_eq!(copyright(&mut h), json!("Example Name"));
    assert!(settings_open(&h, "import"));
}

#[test]
fn text_typed_in_copyright_is_saved_when_another_tab_is_opened() {
    let mut h = demo();
    show(&mut h, "import");
    click(&mut h, "field:settings.copyright");
    h.request("ui.text", json!({"text": "Example Name"}), T);
    steps(&mut h);
    // When another tab is clicked with the text still being typed
    click(&mut h, "button:settingsTab-general");
    // Then that tab opens and the text was saved, as leaving the box any other way saves it
    assert!(settings_open(&h, "general"));
    assert_eq!(copyright(&mut h), json!("Example Name"));
}

#[test]
fn esc_in_an_external_editing_box_leaves_settings_open_too() {
    let mut h = demo();
    show(&mut h, "general");
    // (External Editing is far down the General page: scroll it into view)
    assert_eq!(h.request("ui.hoverWidget", json!({"id": "settings:pageRoom"}), T)["ok"], true);
    h.request("ui.scroll", json!({"dy": -3000.0}), T);
    steps(&mut h);
    let before = h.app.ui.settings.external_editor.clone();
    click(&mut h, "field:externalEditor");
    h.request("ui.text", json!({"text": "Example Editor"}), T);
    steps(&mut h);
    key(&mut h, "Escape");
    assert_eq!(h.app.ui.settings.external_editor, before, "Esc gives the text back");
    assert!(settings_open(&h, "general"), "and Settings stays open: {:?}", h.app.ui.dialog);
}

#[test]
fn using_the_edit_menu_does_not_save_a_half_typed_text() {
    let mut h = demo();
    show(&mut h, "import");
    let before = copyright(&mut h);
    click(&mut h, "field:settings.copyright");
    h.request("ui.text", json!({"text": "Example Name"}), T);
    steps(&mut h);
    // When the field's Cut / Copy / Paste menu is opened, which takes the focus off the box
    let r = h.request("ui.clickWidget", json!({"id": "field:settings.copyright", "button": "right"}), T);
    assert_eq!(r["ok"], true, "right click: {r}");
    steps(&mut h);
    assert!(h.app.widgets.iter().any(|(w, _)| w == "field:settings.copyright:copy"), "the menu is open");
    // Then the text typed so far is still there, and nothing was saved
    assert!(says(&h, "Example Name"), "the typing stays in the box");
    assert_eq!(copyright(&mut h), before, "an unfinished edit is not saved");
    // And Esc closes only the menu: the edit goes on, and Return then saves it
    key(&mut h, "Escape");
    assert_eq!(copyright(&mut h), before, "closing the menu does not end the edit");
    assert!(settings_open(&h, "import"), "Esc closed the menu only");
    key(&mut h, "Enter");
    assert_eq!(copyright(&mut h), json!("Example Name"));
}
