//! Edit Keyword Set (Lightroom Classic's): the nine keywords of a keyword set, typed in place.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The Keywords panel with a photo selected, beach and Weddings recently added.
fn keywords_panel() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let id = h.app.session.visible_cloned()[0].0;
    for (method, params) in [
        ("ui.set", json!({"view": "photoGrid", "right": "keywords"})),
        ("engine.execute", json!({"command": "library.select", "params": {"ids": [id]}})),
        ("engine.execute", json!({"command": "photo.setMeta", "params": {"addKeywords": ["beach", "Weddings"]}})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    h
}

fn ask(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
}

fn sets(h: &mut Headless) -> serde_json::Value {
    h.app.session.execute("keyword.sets", &json!({})).unwrap()
}

fn open_editor(h: &mut Headless) {
    ask(h, "ui.clickWidget", json!({"id": "keywordSetCombo"}));
    ask(h, "ui.clickWidget", json!({"id": "keywordSetMenu:edit"}));
}

/// Edit Set… on Recent Keywords shows the current nine in their slots; named, they become a set of
/// their own (and the current one), with what was typed in an empty slot.
#[test]
fn editing_recent_keywords_saves_them_as_a_set() {
    let mut h = keywords_panel();
    let recent = sets(&mut h)["keywords"].clone();
    open_editor(&mut h);
    let Some(crate::state::Dialog::KeywordSet { replaces, name, slots, .. }) = h.app.ui.dialog.clone() else { panic!("{:?}", h.app.ui.dialog) };
    assert_eq!((replaces, name.as_str()), (None, ""));
    assert_eq!(json!(slots.iter().filter(|k| !k.is_empty()).collect::<Vec<_>>()), recent, "the current nine");
    ask(&mut h, "ui.text", json!({"text": "Beach Weddings"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordSetSlot:3"}));
    ask(&mut h, "ui.text", json!({"text": "harbour"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    assert_eq!(h.app.ui.dialog, None);
    let s = sets(&mut h);
    assert_eq!(s["current"], "Beach Weddings");
    assert_eq!(s["keywords"][2], "harbour", "slot 3 is ⌥3");
}

/// Edit Set… on a named set edits it: renamed, it keeps its place and stays current.
#[test]
fn editing_a_named_set_renames_it() {
    let mut h = keywords_panel();
    h.app.session.execute("keyword.saveSet", &json!({"name": "Weddings", "keywords": ["ceremony", "reception"]})).unwrap();
    h.settle(SETTLE);
    open_editor(&mut h);
    let Some(crate::state::Dialog::KeywordSet { replaces, name, .. }) = h.app.ui.dialog.clone() else { panic!("{:?}", h.app.ui.dialog) };
    assert_eq!((replaces.as_deref(), name.as_str()), (Some("Weddings"), "Weddings"));
    ask(&mut h, "ui.text", json!({"text": "Ceremonies"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    let s = sets(&mut h);
    let names: Vec<&str> = s["sets"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Recent Keywords", "Ceremonies"]);
    assert_eq!(s["keywords"], json!(["ceremony", "reception"]));
}

/// A set needs a name: without one the dialog says so and stays open.
#[test]
fn a_set_needs_a_name() {
    let mut h = keywords_panel();
    open_editor(&mut h);
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::KeywordSet { .. })), "still open");
    assert!(h.app.ui.toast.is_some(), "and says why");
}

/// An empty slot is an empty button that does nothing.
#[test]
fn an_empty_slot_is_an_idle_button() {
    let mut h = keywords_panel();
    h.app.session.execute("keyword.saveSet", &json!({"name": "Weddings", "keywords": ["ceremony", "", "reception"]})).unwrap();
    h.settle(SETTLE);
    assert!(h.app.widgets.iter().any(|(w, _)| w == "kwSetEmpty:2"), "slot 2 is shown empty");
}

/// The nine fields are alike and wide enough for a keyword: a grid of equal columns, not one sized
/// by what the window had room for while it found its size.
#[test]
fn the_slots_are_alike() {
    let mut h = keywords_panel();
    open_editor(&mut h);
    let slots: Vec<egui::Rect> =
        (1..=9).map(|i| h.app.widgets.iter().find(|(w, _)| *w == format!("field:keywordSetSlot:{i}")).map(|(_, r)| *r).expect("slot")).collect();
    for r in &slots {
        assert!(r.width() >= 100.0, "{r:?}");
        assert!((r.width() - slots[0].width()).abs() < 1.0, "{r:?} vs {:?}", slots[0]);
    }
}

/// Naming Recent Keywords like a set that exists says so, in the user's words, and keeps the
/// dialog (and that set) as they were.
#[test]
fn a_taken_name_says_so() {
    let mut h = keywords_panel();
    h.app.session.execute("keyword.saveSet", &json!({"name": "Travel", "keywords": ["harbour"]})).unwrap();
    h.app.session.execute("keyword.useSet", &json!({"name": "Recent Keywords"})).unwrap();
    h.settle(SETTLE);
    open_editor(&mut h);
    ask(&mut h, "ui.text", json!({"text": "travel"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::KeywordSet { .. })), "still open");
    let toast = h.app.ui.toast.clone().map(|t| t.0).unwrap_or_default();
    assert!(toast.starts_with("There is a keyword set"), "{toast}");
    let s = sets(&mut h);
    let travel = s["sets"].as_array().unwrap().iter().find(|x| x["name"] == "Travel").cloned().unwrap();
    assert_eq!(travel["keywords"], json!(["harbour"]), "left alone");
}

/// A set deleted while its dialog was open is saved again, as a new set, rather than refused.
#[test]
fn a_set_deleted_meanwhile_is_saved_anew() {
    let mut h = keywords_panel();
    h.app.session.execute("keyword.saveSet", &json!({"name": "Weddings", "keywords": ["ceremony"]})).unwrap();
    h.settle(SETTLE);
    open_editor(&mut h);
    h.app.session.execute("keyword.deleteSet", &json!({"name": "Weddings"})).unwrap();
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    assert_eq!(h.app.ui.dialog, None);
    assert_eq!(sets(&mut h)["current"], "Weddings");
}

/// Save as a new set (Lightroom Classic's Save as New Preset): the edited set stays, and the new
/// one, under its own name, becomes current.
#[test]
fn save_as_a_new_set_keeps_the_original() {
    let mut h = keywords_panel();
    h.app.session.execute("keyword.saveSet", &json!({"name": "Weddings", "keywords": ["ceremony"]})).unwrap();
    h.settle(SETTLE);
    open_editor(&mut h);
    ask(&mut h, "ui.text", json!({"text": "Ceremonies"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "check:keywordSetAsNew"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    let s = sets(&mut h);
    let names: Vec<&str> = s["sets"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Recent Keywords", "Weddings", "Ceremonies"]);
    assert_eq!(s["current"], "Ceremonies");
}
