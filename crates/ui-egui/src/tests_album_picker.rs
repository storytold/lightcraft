//! The album picker in the smart-album rule editor: a tree like the sidebar's, a search that
//! narrows it as you type, keys to pick with, and albums that can't be picked shown greyed.

use std::time::Duration;

use dac_catalog::Rule;
use serde_json::json;

use crate::headless::Headless;
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn id(v: serde_json::Value) -> u64 {
    v["id"].as_u64().expect("an id")
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn value(h: &Headless) -> serde_json::Value {
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    let Some(Rule::Field { value, .. }) = rules.rules.first() else { panic!("no rule") };
    value.clone()
}

struct Library {
    h: Headless,
    utils: u64,
    excluded: u64,
    uncurated: u64,
    trip: u64,
}

/// [UTILS] > Excluded (smart), Uncurated (smart); Trip; and the rule editor on a new album with one
/// Album rule.
fn library() -> Library {
    let app = DacApp::new(dac_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let utils = id(exec(&mut h, "album.create", json!({"name": "[UTILS]", "folder": true})));
    let excluded = id(exec(&mut h, "album.createSmart", json!({"name": "Excluded", "rules": {"rating": 1}, "parent": utils})));
    let uncurated = id(exec(&mut h, "album.createSmart", json!({"name": "Uncurated", "rules": {"rating": 0}, "parent": utils})));
    let trip = id(exec(&mut h, "album.create", json!({"name": "Trip", "addSelected": false})));
    exec(&mut h, "dialog.smartAlbum", json!({"name": "Travel"}));
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    rules.rules = vec![serde_json::from_value(json!({"field": "album", "op": "isNot", "value": null})).unwrap()];
    h.settle(SETTLE);
    Library { h, utils, excluded, uncurated, trip }
}

/// With nothing typed, the picker shows the sidebar's tree: folders start closed and open on click.
#[test]
fn the_tree_opens_folder_by_folder() {
    let Library { mut h, utils, excluded, trip, .. } = library();
    click(&mut h, "albumPicker:rules-0");
    assert!(has(&h, &format!("albumPickerFolder:{utils}:rules-0")) && has(&h, &format!("albumPickerItem:{trip}:rules-0")));
    assert!(!has(&h, &format!("albumPickerItem:{excluded}:rules-0")), "inside a closed folder");
    click(&mut h, &format!("albumPickerFolder:{utils}:rules-0"));
    click(&mut h, &format!("albumPickerItem:{excluded}:rules-0"));
    assert_eq!(value(&h), json!(excluded));
    assert!(!has(&h, &format!("albumPickerItem:{trip}:rules-0")), "picking closes it");
    // it opens again on the chosen album's folder
    click(&mut h, "albumPicker:rules-0");
    assert!(has(&h, &format!("albumPickerItem:{excluded}:rules-0")));
}

/// Typing narrows the albums to those whose path holds every word; Enter picks the first, the
/// arrow keys move through them.
#[test]
fn typing_finds_and_keys_pick() {
    let Library { mut h, excluded, uncurated, trip, .. } = library();
    click(&mut h, "albumPicker:rules-0");
    let r = h.request("ui.text", json!({"text": "utils"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, &format!("albumPickerItem:{excluded}:rules-0")) && has(&h, &format!("albumPickerItem:{uncurated}:rules-0")));
    assert!(!has(&h, &format!("albumPickerItem:{trip}:rules-0")), "Trip doesn't match");
    for key in ["ArrowDown", "Enter"] {
        let r = h.request("ui.key", json!({"key": key}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    assert_eq!(value(&h), json!(uncurated), "the second match");
}

/// Albums that can't be picked (the one being edited, one that would loop) are shown greyed, not
/// hidden, and clicking them does nothing.
#[test]
fn albums_that_would_loop_are_greyed() {
    let Library { mut h, excluded, .. } = library();
    let rules = json!({"ruleSet": {"rules": [{"field": "album", "op": "isNot", "value": excluded}]}});
    let travel = id(exec(&mut h, "album.createSmart", json!({"name": "Travel", "rules": rules})));
    exec(&mut h, "dialog.smartAlbum", json!({"id": excluded}));
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    rules.rules = vec![serde_json::from_value(json!({"field": "album", "op": "is", "value": null})).unwrap()];
    h.settle(SETTLE);
    click(&mut h, "albumPicker:rules-0");
    assert!(has(&h, &format!("albumPickerItem:{travel}:rules-0")), "shown");
    click(&mut h, &format!("albumPickerItem:{travel}:rules-0"));
    assert_eq!(value(&h), json!(null), "but not picked");
}

/// After opening a folder, typing still goes to the search: the box keeps the keyboard.
#[test]
fn typing_after_a_folder_click_still_searches() {
    let Library { mut h, utils, excluded, trip, .. } = library();
    click(&mut h, "albumPicker:rules-0");
    click(&mut h, &format!("albumPickerFolder:{utils}:rules-0"));
    assert!(has(&h, &format!("albumPickerItem:{excluded}:rules-0")));
    let r = h.request("ui.text", json!({"text": "trip"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, &format!("albumPickerItem:{trip}:rules-0")) && !has(&h, &format!("albumPickerItem:{excluded}:rules-0")), "searching for trip");
}
