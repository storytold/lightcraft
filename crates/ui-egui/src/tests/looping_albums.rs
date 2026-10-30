//! A smart album on a loop holds nothing, and says why: its empty view and its sidebar row name
//! the album the loop goes through. An empty album that is not on a loop says nothing of loops,
//! and neither does one that only tests a looping album.

use std::time::Duration;

use lightcraft_catalog::{Album, AlbumId, Filter, Op};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
const TITLE: &str = "This album is in a loop, so it shows nothing";

fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

/// A smart album with `any` of the rules.
fn smart(id: u64, name: &str, rules: serde_json::Value) -> Album {
    let filter: Filter = serde_json::from_value(json!({"ruleSet": {"match": "any", "rules": rules}})).unwrap();
    Album { smart: Some(Box::new(filter)), ..Album::new(AlbumId(id), name) }
}

fn is_in(id: u64) -> serde_json::Value {
    json!({"field": "album", "op": "is", "value": id})
}

fn show(h: &mut Headless, album: u64) {
    let r = h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "album", "id": album}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

fn painted(h: &Headless) -> Vec<String> {
    h.painted_text()
}

/// Alpha (10) and Beta (11) test each other; Gamma (12) tests Alpha without being on the loop;
/// Delta (13) tests itself; Quiet (14) is an ordinary smart album that happens to match nothing.
fn with_a_loop() -> Headless {
    let mut h = demo();
    let rating = json!({"field": "rating", "op": "gte", "value": 1});
    let none = json!({"field": "keywords", "op": "contains", "value": "no-such-keyword"});
    for album in [
        smart(10, "Alpha", json!([rating, is_in(11)])),
        smart(11, "Beta", json!([rating, is_in(10)])),
        smart(12, "Gamma", json!([is_in(10)])),
        smart(13, "Delta", json!([rating, is_in(13)])),
        smart(14, "Quiet", json!([none])),
    ] {
        h.app.session.commit("Add album", Op::AddAlbum { album }).unwrap();
    }
    h.settle(SETTLE);
    h
}

// Scenario: an album on a loop is empty, and its empty view names the album it loops through
#[test]
fn an_album_on_a_loop_says_it_is_empty_because_of_the_loop() {
    let mut h = with_a_loop();
    show(&mut h, 10);
    assert_eq!(h.app.session.visible_cloned().len(), 0);
    let text = painted(&h);
    assert!(text.iter().any(|t| t == TITLE), "{text:?}");
    assert!(text.iter().any(|t| t.contains("“Beta”") && t.contains("Edit Smart Album…")), "the body names the album the loop goes through: {text:?}");
    assert!(text.iter().all(|t| t != "No photos"), "{text:?}");
    // the other way round, and an album that tests itself
    show(&mut h, 11);
    assert!(painted(&h).iter().any(|t| t.contains("“Alpha”")));
    show(&mut h, 13);
    let text = painted(&h);
    assert!(text.iter().any(|t| t == TITLE), "{text:?}");
    assert!(text.iter().any(|t| t.starts_with("It tests itself")), "{text:?}");
}

// Scenario: a filter on top doesn't hide the reason
#[test]
fn the_loop_is_the_reason_even_with_a_filter_on() {
    let mut h = with_a_loop();
    show(&mut h, 10);
    let r = h.request("engine.execute", json!({"command": "library.filter", "params": {"rating": 3}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(painted(&h).iter().any(|t| t == TITLE));
}

// Scenario: an album that is not on a loop says nothing of loops
#[test]
fn an_empty_album_off_every_loop_does_not_blame_a_loop() {
    let mut h = with_a_loop();
    // empty by its own rule
    show(&mut h, 14);
    let text = painted(&h);
    assert!(text.iter().all(|t| t != TITLE && !t.starts_with("It leads back")), "{text:?}");
    assert!(text.iter().any(|t| t == "No photos"), "{text:?}");
    // empty because the album it tests holds nothing, but not on the loop itself
    show(&mut h, 12);
    assert_eq!(h.app.session.visible_cloned().len(), 0);
    let text = painted(&h);
    assert!(text.iter().all(|t| t != TITLE), "{text:?}");
}

// Scenario: the sidebar marks the looping albums, and only those (the mark predates the loop rule:
// a loop is among the problems a smart album reports)
#[test]
fn the_sidebar_marks_the_albums_on_a_loop() {
    let h = with_a_loop();
    assert!(h.app.widgets.iter().any(|(w, _)| w == "source:album:10"), "the rows are on screen");
    let has = |id: u64| h.app.widgets.iter().any(|(w, _)| *w == format!("albumProblem:{id}"));
    assert!(has(10) && has(11) && has(13), "the albums on a loop carry the mark");
    assert!(!has(12) && !has(14), "an album off every loop does not");
    let text = painted(&h);
    assert!(text.iter().any(|t| t.starts_with('⚠') && t.contains("Alpha")), "{text:?}");
}

#[test]
fn the_note_names_the_album_the_loop_goes_through() {
    use lightcraft_catalog::Catalog;
    let mut c = Catalog::new();
    for album in [smart(1, "Alpha", json!([is_in(2)])), smart(2, "Beta", json!([is_in(1)])), smart(3, "Plain", json!([]))] {
        c.apply(Op::AddAlbum { album }).unwrap();
    }
    let (title, body) = crate::i18n::album_loop_note(&c, AlbumId(1)).expect("on a loop");
    assert_eq!(title, TITLE);
    assert!(body.contains("Beta"));
    assert!(crate::i18n::album_loop_note(&c, AlbumId(3)).is_none());
}

// Scenario: in a narrow grid the message wraps instead of running off its edges
#[test]
fn the_message_wraps_in_a_narrow_grid() {
    let mut h = with_a_loop();
    let r = h.request("ui.resize", json!({"width": 900, "height": 700}), T);
    assert_eq!(r["ok"], true, "{r}");
    show(&mut h, 10);
    let canvas = h.app.canvas_rect.expect("the grid is drawn");
    let lines: Vec<(f32, usize, egui::Rect)> = h
        .view
        .painted_shapes()
        .into_iter()
        .filter_map(|s| match s {
            egui::epaint::Shape::Text(t) if t.galley.job.text.starts_with("It leads back") => {
                Some((t.galley.size().x, t.galley.rows.len(), t.visual_bounding_rect()))
            }
            _ => None,
        })
        .collect();
    let (_, rows, bounds) = lines.first().copied().expect("the body is painted");
    assert!(rows > 1, "the body is wider than the grid, so it takes more than one row");
    assert!(bounds.left() >= canvas.left() && bounds.right() <= canvas.right(), "{bounds:?} stays inside {canvas:?}");
}
