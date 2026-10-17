//! Issue #1999: starred presets are listed in a Favorites section at the top of the Presets panel,
//! as starred profiles are in the Profile browser, and stay in their own group too.

use crate::{LightcraftApp, Services, headless::Headless};
use serde_json::json;

fn app() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.app.run("panel.presets", json!({})).unwrap();
    for _ in 0..5 {
        h.step();
    }
    h
}

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// The topmost preset group header on screen.
fn first_group(h: &Headless) -> String {
    let groups = h.app.widgets.iter().filter(|(w, _)| w.starts_with("presetGroup:"));
    groups.min_by(|a, b| a.1.top().total_cmp(&b.1.top())).map(|(w, _)| w.clone()).unwrap()
}

#[test]
fn starred_presets_are_listed_in_favorites_on_top() {
    let mut h = app();
    assert!(rect(&h, "presetGroup:favorites").is_none(), "no section while nothing is starred");
    let first = h.app.session.presets.first().map(|p| p.id.clone()).unwrap();
    let last = h.app.session.presets.last().map(|p| p.id.clone()).unwrap();
    h.app.run("preset.favorite", json!({"id": last})).unwrap();
    h.app.run("preset.favorite", json!({"id": first})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(first_group(&h), "presetGroup:favorites");
    let header = rect(&h, "presetGroup:favorites").unwrap();
    // in list order, right under the header
    let a = rect(&h, &format!("presetFavorite:{first}")).unwrap();
    let b = rect(&h, &format!("presetFavorite:{last}")).unwrap();
    assert!(header.bottom() <= a.top() && a.top() < b.top(), "{header:?} {a:?} {b:?}");
    // still in its own group
    assert!(rect(&h, &format!("preset:{first}")).is_some());
    // unstarring the last one removes the section
    for id in [&first, &last] {
        h.app.run("preset.favorite", json!({"id": id})).unwrap();
    }
    for _ in 0..3 {
        h.step();
    }
    assert!(rect(&h, "presetGroup:favorites").is_none());
}

#[test]
fn a_user_group_named_favorites_keeps_its_own_header() {
    let mut h = app();
    let id = h.app.session.presets.first().map(|p| p.id.clone()).unwrap();
    h.app.run("preset.favorite", json!({"id": id})).unwrap();
    h.app.run("preset.create", json!({"name": "Mine", "group": "Favorites"})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    assert!(rect(&h, "presetGroup:favorites").is_some());
    assert!(rect(&h, "presetGroup:Favorites").is_some(), "the user's group is still there");
}
