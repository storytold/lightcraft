//! Loupe and filmstrip background shades (issue #1295): a neutral grey round the photo and behind
//! the filmstrip, whatever the theme, set by command, menu or Settings, and kept with the settings.

use std::time::Duration;

use egui::Color32;
use egui::epaint::Shape;
use serde_json::json;

use crate::headless::Headless;
use crate::state::AppSettings;
use crate::theme::CanvasShade;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The filled rectangles painted this frame, with their areas.
fn fills(h: &Headless) -> Vec<(egui::Rect, Color32)> {
    h.view
        .painted_shapes()
        .iter()
        .filter_map(|s| match s {
            Shape::Rect(r) => Some((r.rect, r.fill)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_medium_grey_loupe_and_dark_grey_filmstrip_are_painted_and_saved() {
    let mut session = lcc_engine::Session::with_demo();
    let ids = session.visible_cloned();
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0].0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let grey = Color32::from_gray(128);
    let big = |h: &Headless, c: Color32| fills(h).iter().any(|(r, f)| *f == c && r.area() > 1200.0 * 800.0 * 0.25);
    assert!(!big(&h, grey), "the theme's own canvas by default");

    let r = h.app.run("view.loupeBackground", json!({"shade": "mediumGray"})).unwrap();
    assert_eq!(r["loupe"], "mediumGray");
    assert_eq!(r["filmstrip"], "theme", "the filmstrip keeps its own shade");
    h.app.run("view.filmstripBackground", json!({"shade": "darkGray"})).unwrap();
    h.settle(SETTLE);
    assert!(big(&h, grey), "the area round the photo is 50 % grey");
    let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("film:{}", ids[2].0)).map(|(_, r)| *r).expect("cell drawn");
    let dark = Color32::from_gray(64);
    assert!(fills(&h).iter().any(|(r, f)| *f == dark && r.contains_rect(cell)), "the strip behind the cells is dark grey");
    assert!(fills(&h).iter().any(|(r, f)| *f == dark && *r == cell), "an unselected cell takes the strip's shade");

    // Window ▸ Appearance shows the choices, the current one checked
    let menu = serde_json::to_value(crate::menubar::expanded(&h.app, "Appearance").unwrap()).unwrap();
    let text = menu.to_string();
    assert!(text.contains("Loupe Background") && text.contains("Filmstrip Background"), "{text}");
    fn checked_shades(v: &serde_json::Value, id: &str, out: &mut Vec<String>) {
        if let Some(a) = v.as_array() {
            a.iter().for_each(|c| checked_shades(c, id, out));
        } else if let Some(o) = v.as_object() {
            if o.get("id").and_then(|i| i.as_str()) == Some(id) && o.get("checked") == Some(&json!(true)) {
                out.push(o["params"]["shade"].as_str().unwrap_or_default().to_string());
            }
            o.values().for_each(|c| checked_shades(c, id, out));
        }
    }
    let mut on = Vec::new();
    checked_shades(&menu, "view.loupeBackground", &mut on);
    assert_eq!(on, ["mediumGray"], "{text}");
    let mut on = Vec::new();
    checked_shades(&menu, "view.filmstripBackground", &mut on);
    assert_eq!(on, ["darkGray"], "{text}");

    // saved with the settings and read back
    let saved = serde_json::to_value(&h.app.ui.settings).unwrap();
    let back: AppSettings = serde_json::from_value(saved).unwrap();
    assert_eq!((back.loupe_background, back.filmstrip_background), (CanvasShade::MediumGray, CanvasShade::DarkGray));
}

#[test]
fn background_commands_cycle_refuse_bad_shades_and_old_settings_load() {
    let mut a = LccApp::new(lcc_engine::Session::new(), Services { png: None, ..Default::default() });
    // omitted: the next shade, all the way round
    let mut seen = vec![a.ui.settings.loupe_background];
    for _ in 0..CanvasShade::ALL.len() {
        a.run("view.loupeBackground", json!({})).unwrap();
        seen.push(a.ui.settings.loupe_background);
    }
    assert_eq!(seen.first(), seen.last(), "{seen:?}");
    for s in CanvasShade::ALL {
        assert!(seen.contains(&s), "{s:?} reached by cycling");
    }
    a.run("view.filmstripBackground", json!({"shade": "white"})).unwrap();
    for p in [json!({"shade": "neon"}), json!({"shade": 128}), json!({"shade": []}), json!({"shade": ""})] {
        assert!(a.run("view.loupeBackground", p.clone()).is_err(), "{p}");
        assert!(a.run("view.filmstripBackground", p.clone()).is_err(), "{p}");
    }
    assert_eq!(a.ui.settings.filmstrip_background, CanvasShade::White, "a refused shade changes nothing");
    // settings written before the option (and an unknown shade) never fail to load
    let mut old = serde_json::to_value(AppSettings::default()).unwrap();
    let obj = old.as_object_mut().unwrap();
    obj.remove("loupeBackground");
    obj.remove("filmstripBackground");
    let s: AppSettings = serde_json::from_value(old).unwrap();
    assert_eq!((s.loupe_background, s.filmstrip_background), (CanvasShade::Theme, CanvasShade::Theme));
    let s: AppSettings = serde_json::from_value(json!({"loupeBackground": "plaid", "filmstripBackground": 7, "filmNames": false})).unwrap();
    assert_eq!((s.loupe_background, s.filmstrip_background, s.film_names), (CanvasShade::Theme, CanvasShade::Theme, false));
}
