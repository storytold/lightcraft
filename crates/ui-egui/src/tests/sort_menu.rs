//! The bottom bar's Sort menu offers only what the view being shown uses: Recently Added is always
//! newest import first, a shuffle has no date headers, Reshuffle only means something under Random.

use crate::headless::Headless;
use crate::{LightcraftApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(Duration::from_secs(120));
    // the first request switches the widget list on, the next frame fills it
    h.request("ui.widgets", json!({}), T);
    h.step();
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
}

/// Open the Sort popup and click `id` in it (the popup closes with the click).
fn click_in_sort_menu(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": "icon:sort"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

fn row_listed(h: &mut Headless, id: &str) -> bool {
    let r = h.request("ui.clickWidget", json!({"id": "icon:sort"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let listed = h.request("ui.widgets", json!({"filter": id}), T)["result"].as_array().is_some_and(|a| a.iter().any(|w| w["id"] == id));
    // close it again with Escape
    h.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    h.step();
    listed
}

#[test]
fn recently_added_turns_the_keys_the_direction_and_reshuffle_off_and_says_why() {
    let mut h = demo();
    exec(&mut h, "library.sort", json!({"key": "random", "seed": 7}));
    exec(&mut h, "library.source", json!({"kind": "recentlyAdded"}));
    assert!(row_listed(&mut h, "sortMenu:note"), "a dim line says why the keys are off");
    click_in_sort_menu(&mut h, "sortMenu:reshuffle");
    assert_eq!(h.app.session.sort.seed, 7, "a reshuffle changes nothing there");
    click_in_sort_menu(&mut h, "sortMenu:key:fileName");
    assert_eq!(h.app.session.sort.key, lightcraft_catalog::SortKey::Random, "the keys are off");
    click_in_sort_menu(&mut h, "sortMenu:ascending");
    assert!(!h.app.session.sort.ascending, "the direction is off");

    exec(&mut h, "library.source", json!({"kind": "all"}));
    assert!(!row_listed(&mut h, "sortMenu:note"), "no note where the sort applies");
    click_in_sort_menu(&mut h, "sortMenu:reshuffle");
    assert_ne!(h.app.session.sort.seed, 7, "under Random in All Photos Reshuffle works");
}

#[test]
fn a_shuffle_turns_the_date_groups_off_and_keeps_the_choice() {
    let mut h = demo();
    exec(&mut h, "library.sort", json!({"key": "random", "group": "month"}));
    click_in_sort_menu(&mut h, "sortMenu:group:day");
    assert_eq!(h.app.session.sort.group, lightcraft_catalog::GroupBy::Month, "no date headers to group");
    exec(&mut h, "library.sort", json!({"key": "captureDate"}));
    click_in_sort_menu(&mut h, "sortMenu:group:day");
    assert_eq!(h.app.session.sort.group, lightcraft_catalog::GroupBy::Day, "groups are choosable for a date sort");
}
