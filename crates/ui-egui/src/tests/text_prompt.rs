//! A one-line prompt that the engine refuses stays open with its text and says why, so the name can
//! be fixed; a folder made on disk says it was made.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(60);
const CREATED: &str = "Created folder “{name}” in “{parent}”";

/// A fresh `Shoot` folder holding `Taken` and `Other`, in a temporary folder of its own.
fn shoot(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("lc-text-prompt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let shoot = root.join("Shoot");
    for name in ["Taken", "Other"] {
        std::fs::create_dir_all(shoot.join(name)).unwrap();
    }
    shoot
}

/// Remove the temporary folder `shoot` made, which holds `dir`.
fn clean(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

fn app() -> Headless {
    let session = lightcraft_engine::Session::new().with_fs();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1300.0, 900.0], 1.0);
    h.settle(SETTLE);
    h
}

fn prompt(command: &str, value: &str, params: serde_json::Value, key: &str) -> Dialog {
    Dialog::TextPrompt { title: "Title".into(), hint: "Name".into(), value: value.into(), command: command.into(), params, key: key.into() }
}

/// Open `dialog`, press its OK button, and let the frame settle.
fn confirm(h: &mut Headless, dialog: Dialog) {
    h.app.ui.dialog = Some(dialog);
    for _ in 0..4 {
        h.step();
    }
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|(t, _, _)| t.clone()).unwrap_or_default()
}

fn assert_stays_open(h: &Headless, value: &str, reason: &str) {
    match &h.app.ui.dialog {
        Some(Dialog::TextPrompt { value: kept, .. }) => assert_eq!(kept, value, "the typed text is kept"),
        other => panic!("the prompt stays open: {other:?}"),
    }
    assert!(toast(h).contains(reason), "the reason is shown: {:?}", toast(h));
}

#[test]
fn a_new_folder_whose_name_is_taken_keeps_the_prompt_open_and_says_why() {
    let dir = shoot("create-taken");
    let mut h = app();
    confirm(&mut h, prompt("folder.create", "Taken", json!({"parent": dir.to_string_lossy()}), "name"));
    assert_stays_open(&h, "Taken", "already exists");
    clean(&dir);
}

#[test]
fn a_rename_to_a_taken_name_keeps_the_prompt_open_and_says_why() {
    let dir = shoot("rename-taken");
    let mut h = app();
    confirm(&mut h, prompt("folder.rename", "Taken", json!({"path": dir.join("Other").to_string_lossy()}), "name"));
    assert_stays_open(&h, "Taken", "already exists");
    assert!(dir.join("Other").is_dir(), "nothing was renamed");
    clean(&dir);
}

#[test]
fn a_rename_that_keeps_the_name_closes_the_prompt_and_changes_nothing() {
    let dir = shoot("rename-same");
    let mut h = app();
    // Rename Folder opens with the folder's own name typed
    confirm(&mut h, prompt("folder.rename", "Other", json!({"path": dir.join("Other").to_string_lossy()}), "name"));
    assert_eq!(h.app.ui.dialog, None, "the prompt closes");
    assert!(toast(&h).is_empty(), "no error is shown: {:?}", toast(&h));
    assert!(dir.join("Other").is_dir(), "nothing was renamed");
    clean(&dir);
}

#[test]
fn the_control_channel_keeps_a_refused_prompt_open_too() {
    let dir = shoot("control-taken");
    let mut h = app();
    h.app.ui.dialog = Some(prompt("folder.create", "Taken", json!({"parent": dir.to_string_lossy()}), "name"));
    assert!(h.settle(SETTLE), "the prompt renders settled");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_ne!(r["ok"], true, "the name is refused: {r}");
    assert!(r.to_string().contains("already exists"), "{r}");
    match &h.app.ui.dialog {
        Some(Dialog::TextPrompt { value, .. }) => assert_eq!(value, "Taken", "the typed text is kept"),
        other => panic!("the prompt stays open: {other:?}"),
    }
    assert!(toast(&h).contains("already exists"), "the reason is on screen too, as with the button: {:?}", toast(&h));
    clean(&dir);
}

#[test]
fn the_control_channel_says_where_a_new_folder_was_made() {
    let dir = shoot("control-ok");
    let mut h = app();
    h.app.ui.dialog = Some(prompt("folder.create", "Selects", json!({"parent": dir.to_string_lossy()}), "name"));
    assert!(h.settle(SETTLE), "the prompt renders settled");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(dir.join("Selects").is_dir(), "made on disk");
    let parent = lightcraft_catalog::folders::folder_label(&dir.to_string_lossy());
    assert_eq!(toast(&h), CREATED.replace("{name}", "Selects").replace("{parent}", &parent));
    clean(&dir);
}

#[test]
fn a_rename_that_keeps_a_name_with_spaces_at_its_ends_changes_nothing() {
    let dir = shoot("rename-padded");
    std::fs::create_dir_all(dir.join(" Pad ")).unwrap();
    let mut h = app();
    // Rename Folder opens with the folder's own name typed, spaces and all
    confirm(&mut h, prompt("folder.rename", " Pad ", json!({"path": dir.join(" Pad ").to_string_lossy()}), "name"));
    assert_eq!(h.app.ui.dialog, None, "the prompt closes");
    assert!(toast(&h).is_empty(), "no error is shown: {:?}", toast(&h));
    assert!(dir.join(" Pad ").is_dir() && !dir.join("Pad").exists(), "the folder keeps its name");
    clean(&dir);
}

#[test]
fn a_new_folder_closes_the_prompt_and_says_where_it_is() {
    let dir = shoot("create-ok");
    let mut h = app();
    confirm(&mut h, prompt("folder.create", " Selects ", json!({"parent": dir.to_string_lossy()}), "name"));
    assert!(dir.join("Selects").is_dir(), "made on disk");
    assert_eq!(h.app.ui.dialog, None);
    let parent = lightcraft_catalog::folders::folder_label(&dir.to_string_lossy());
    assert_eq!(toast(&h), CREATED.replace("{name}", "Selects").replace("{parent}", &parent));
    clean(&dir);
}

#[test]
fn the_created_folder_toast_comes_through_the_catalog() {
    let dir = shoot("create-de");
    let path = dir.join("Selects");
    let parent = lightcraft_catalog::folders::folder_label(&dir.to_string_lossy());
    // the catalog's own value (formats have no `Locale` accessor); a language without one shows the English wording
    let catalog: std::collections::BTreeMap<String, String> = serde_json::from_str(include_str!("../../locales/de-formats.json")).unwrap();
    let own = catalog.get(CREATED).filter(|text| !text.is_empty()).map_or(CREATED, String::as_str);
    let want = own.replace("{name}", "Selects").replace("{parent}", &parent);
    crate::i18n::set_language(crate::i18n::Locale::De);
    let text = crate::panels::left::created_text(&path.to_string_lossy(), &dir.to_string_lossy());
    crate::i18n::set_language(crate::i18n::Locale::En);
    assert_eq!(text, want);
    clean(&dir);
}
