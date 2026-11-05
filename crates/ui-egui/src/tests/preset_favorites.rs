//! Issue #1999: starred presets are listed in a Favorites section at the top of the Presets panel,
//! as starred profiles are in the Profile browser, and stay in their own group too.

use crate::{LightcraftApp, Services, headless::Headless};
use serde_json::json;

/// A headless app on the demo library with the Presets panel open and laid out.
fn app() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.app.run("panel.presets", json!({})).unwrap();
    for _ in 0..5 {
        h.step();
    }
    h
}

/// Where the widget registered as `id` was drawn in the last frame, if it was.
fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// The topmost preset group header on screen (the Favorites header is `presetFavorites`).
fn first_group(h: &Headless) -> String {
    let groups = h.app.widgets.iter().filter(|(w, _)| w.starts_with("presetGroup:") || w == "presetFavorites");
    groups.min_by(|a, b| a.1.top().total_cmp(&b.1.top())).map(|(w, _)| w.clone()).unwrap()
}

/// Starring two presets makes a Favorites section the first header, listing them in list order
/// right under it, while each stays in its own group too.
#[test]
fn starred_presets_are_listed_in_favorites_on_top() {
    let mut h = app();
    assert!(rect(&h, "presetFavorites").is_none(), "no section while nothing is starred");
    let first = h.app.session.presets.first().map(|p| p.id.clone()).unwrap();
    let last = h.app.session.presets.last().map(|p| p.id.clone()).unwrap();
    h.app.run("preset.favorite", json!({"id": last})).unwrap();
    h.app.run("preset.favorite", json!({"id": first})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(first_group(&h), "presetFavorites");
    let header = rect(&h, "presetFavorites").unwrap();
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
    assert!(rect(&h, "presetFavorites").is_none());
}

/// A user group named "Favorites" or "favorites" keeps its own header, apart from the section's: a
/// click aimed at either id lands on the right row (the widget lookup takes the last match).
#[test]
fn user_groups_named_favorites_keep_their_own_header() {
    let mut h = app();
    let id = h.app.session.presets.first().map(|p| p.id.clone()).unwrap();
    h.app.run("preset.favorite", json!({"id": id})).unwrap();
    for group in ["Favorites", "favorites"] {
        h.app.run("preset.create", json!({"name": format!("Mine in {group}"), "group": group})).unwrap();
    }
    for _ in 0..3 {
        h.step();
    }
    let section = rect(&h, "presetFavorites").unwrap();
    for group in ["Favorites", "favorites"] {
        let id = format!("presetGroup:{group}");
        let user = rect(&h, &id).unwrap_or_else(|| panic!("the user's {group:?} group is listed"));
        assert_ne!(user, section, "{id} is its own row");
        assert_eq!(h.app.widgets.iter().filter(|(w, _)| *w == id).count(), 1, "{id} names one row");
    }
    assert_eq!(h.app.widgets.iter().filter(|(w, _)| w == "presetFavorites").count(), 1);
}
