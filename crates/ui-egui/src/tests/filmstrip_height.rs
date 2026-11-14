//! The filmstrip's height (issue #4110): dragging its top edge makes it taller, with bigger thumbnails,
//! and the photo above gives up the room; a double-click on the edge gives the usual height back.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{FILM_HEIGHT_MAX, FILM_HEIGHT_MIN};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn loupe() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let first = h.app.session.visible_cloned()[0].0;
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn drag_edge(h: &mut Headless, dy: f32) {
    let r = h.request("ui.dragWidget", json!({"id": "filmstripResize", "dx": 0.0, "dy": dy, "steps": 10}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

#[test]
fn dragging_the_filmstrips_top_edge_makes_it_and_its_thumbnails_bigger() {
    let mut h = loupe();
    let first = h.app.session.visible_cloned()[0].0;
    let cell = format!("film:{first}");
    let (strip0, cell0, canvas0) = (widget(&h, "filmstrip"), widget(&h, &cell), h.app.canvas_rect.expect("the canvas"));
    assert_eq!(h.app.ui.film_height, FILM_HEIGHT_MIN, "the usual height to start with");
    // up by 100 points: the strip grows by that much, the photo's canvas shrinks by it, the cells grow
    drag_edge(&mut h, -100.0);
    let (strip1, cell1, canvas1) = (widget(&h, "filmstrip"), widget(&h, &cell), h.app.canvas_rect.expect("the canvas"));
    assert!((h.app.ui.film_height - (FILM_HEIGHT_MIN + 100.0)).abs() < 1.0, "{}", h.app.ui.film_height);
    assert!((strip1.height() - strip0.height() - 100.0).abs() < 1.0, "{strip0:?} → {strip1:?}");
    assert!((canvas0.height() - canvas1.height() - 100.0).abs() < 1.0, "{canvas0:?} → {canvas1:?}");
    assert!(cell1.width() > cell0.width() * 1.5 && cell1.height() > cell0.height() * 1.5, "bigger thumbnails: {cell0:?} → {cell1:?}");
    // far up: it stops at 40 % of the view (or the most it can be), the photo keeps the rest
    drag_edge(&mut h, -500.0);
    let full = canvas0.height() + FILM_HEIGHT_MIN;
    let cap = (full * 0.4).clamp(FILM_HEIGHT_MIN, FILM_HEIGHT_MAX);
    assert!((h.app.ui.film_height - cap).abs() < 1.0, "{} against the cap {cap}", h.app.ui.film_height);
    // far down: never smaller than it always was
    drag_edge(&mut h, 600.0);
    assert_eq!(h.app.ui.film_height, FILM_HEIGHT_MIN);
    // a double-click on the edge gives the usual height back
    drag_edge(&mut h, -80.0);
    assert!(h.app.ui.film_height > FILM_HEIGHT_MIN);
    let r = h.request("ui.clickWidget", json!({"id": "filmstripResize", "count": 2}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.film_height, FILM_HEIGHT_MIN);
    assert!((widget(&h, "filmstrip").height() - strip0.height()).abs() < 0.5);
}

/// A stored height out of range (an edited or old `ui.json`) is brought back into it.
#[test]
fn a_stored_filmstrip_height_is_kept_in_range() {
    let with = |v: f32| crate::UiState { film_height: v, ..Default::default() }.sanitized().film_height;
    assert_eq!(with(200.0), 200.0);
    assert_eq!(with(10_000.0), FILM_HEIGHT_MAX);
    assert_eq!(with(20.0), FILM_HEIGHT_MIN);
    assert_eq!(with(f32::NAN), FILM_HEIGHT_MIN);
    // and a short window caps what it shows
    assert_eq!(crate::state::film_height(300.0, 500.0), 200.0);
    assert_eq!(crate::state::film_height(300.0, 200.0), FILM_HEIGHT_MIN, "never below the usual height");
}

/// A `filmHeight` in `ui.json` that is not a number (hand-edited, or written by another build) gives the
/// usual height and costs none of the other saved settings.
#[test]
fn a_filmstrip_height_that_is_not_a_number_loses_no_other_setting() {
    for bad in [json!(null), json!("tall"), json!([200]), json!({"h": 200})] {
        let ui: crate::UiState = serde_json::from_value(json!({"filmHeight": bad, "thumbSize": 300.0})).unwrap_or_else(|e| panic!("{bad}: {e}"));
        assert_eq!((ui.film_height, ui.thumb_size), (FILM_HEIGHT_MIN, 300.0), "{bad}");
    }
    let ui: crate::UiState = serde_json::from_value(json!({"filmHeight": 200})).unwrap();
    assert_eq!(ui.film_height, 200.0);
    let ui: crate::UiState = serde_json::from_value(json!({})).unwrap();
    assert_eq!(ui.film_height, FILM_HEIGHT_MIN, "absent: the usual height");
}
