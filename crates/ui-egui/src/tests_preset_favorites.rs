//! Presets panel: starred presets get a Favorites group at the top, as profiles do (issue #1999).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const SETTLE: Duration = Duration::from_secs(120);

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

#[test]
fn a_starred_preset_is_listed_first_in_favorites_and_stays_in_its_group() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(SETTLE);
    h.app.run("panel.presets", json!({})).unwrap();
    h.step();
    h.step();
    assert!(rect(&h, "preset:lc.bw-selenium").is_some(), "the B&W group is in view");
    assert!(rect(&h, "presetFavorites").is_none(), "no Favorites group without favourites");

    h.app.run("preset.favorite", json!({"id": "lc.bw-selenium"})).unwrap();
    h.step();
    h.step();
    let header = rect(&h, "presetFavorites").expect("a Favorites group");
    let groups: Vec<egui::Rect> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("presetGroup:")).map(|(_, r)| *r).collect();
    assert!(!groups.is_empty());
    assert!(groups.iter().all(|g| header.top() < g.top()), "Favorites comes before every group");
    let fav = rect(&h, "presetFav:lc.bw-selenium").expect("the preset under Favorites");
    let own = rect(&h, "preset:lc.bw-selenium").expect("the preset still in B&W");
    assert!(fav.top() < own.top());
    if let Some(path) = std::env::var_os("PRESET_FAVORITES_SCREENSHOT_DIR") {
        let pixels = h.paint();
        let image =
            lightcraft_raster::Rgba8 { width: pixels.size[0], height: pixels.size[1], data: pixels.pixels.iter().map(|c| c.to_array()).collect() };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(std::path::Path::new(&path).join("lightcraft-presetfav.png"), png).unwrap();
    }

    h.app.run("preset.favorite", json!({"id": "lc.bw-selenium"})).unwrap();
    h.step();
    h.step();
    assert!(rect(&h, "presetFavorites").is_none(), "un-starring the last favourite removes the group");
    assert!(rect(&h, "presetFav:lc.bw-selenium").is_none());
}

/// The star on a row's right (shown on hover, as in the profile browser) toggles the favourite
/// without applying the preset; a click elsewhere on the row still applies it (#2017).
#[test]
fn preset_star_lists_favourites_first() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let t = Duration::from_secs(10);
    h.request("ui.set", json!({"view": "detail"}), t);
    h.request("engine.execute", json!({"command": "panel.presets"}), t);
    h.settle(SETTLE);
    let id = h.app.session.active().unwrap();
    let history = |h: &Headless| h.app.session.catalog.photo(id).unwrap().history.len();
    let before = history(&h);
    let fav = |h: &Headless| h.app.session.presets.iter().find(|p| p.id == "lc.warm-glow").map(|p| p.favorite);
    assert!(rect(&h, "presetFavorites").is_none(), "no Favorites group without a favourite");

    let r = h.request("ui.clickWidget", json!({"id": "presetStar:lc.warm-glow"}), t);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(fav(&h), Some(true));
    assert_eq!(history(&h), before, "the star doesn't apply the preset");
    h.step();
    h.step();
    let top = |h: &Headless, w: &str| rect(h, w).map(|r| r.top());
    assert!(rect(&h, "presetFav:lc.warm-glow").is_some() && rect(&h, "preset:lc.warm-glow").is_some(), "listed first and in its group");
    assert!(top(&h, "presetFavorites") < top(&h, "presetGroup:Color"), "Favorites first");

    // a click on the favourite's row applies it
    let r = h.request("ui.clickWidget", json!({"id": "presetFav:lc.warm-glow"}), t);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(history(&h), before + 1);
    assert_eq!(fav(&h), Some(true), "the row click leaves the star alone");

    // its star there takes it out of the group, and the empty group goes
    let r = h.request("ui.clickWidget", json!({"id": "presetFavStar:lc.warm-glow"}), t);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(fav(&h), Some(false));
    assert_eq!(history(&h), before + 1, "nor does the star in Favorites");
    h.step();
    h.step();
    assert!(rect(&h, "presetFavorites").is_none() && rect(&h, "presetFav:lc.warm-glow").is_none());
    h.settle(SETTLE);
}
