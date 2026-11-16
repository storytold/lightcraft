//! The Map view's location filter (LRC-MAP-FILTER): Visible on Map, Tagged, Untagged, None, with
//! their counts; it narrows the filmstrip (through `Session::narrow`) over the library's own filter,
//! Visible on Map follows the view once it rests, pins come from the photos before it, and leaving
//! the Map view gives back exactly the grid's list. The map is never online in tests.

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

/// Paris ×3, London ×2, New York ×1 on the demo library's first six photos.
const PLACES: [(f64, f64); 6] =
    [(48.8584, 2.2945), (48.8606, 2.3376), (48.8530, 2.3499), (51.5007, -0.1246), (51.5081, -0.0759), (40.7484, -73.9857)];

fn demo() -> (Headless, Vec<u64>) {
    let mut h = Headless::new(
        LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() }),
        [1300.0, 1000.0],
        1.0,
    );
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(6).map(|p| p.0).collect();
    for (id, (lat, lon)) in ids.iter().zip(PLACES) {
        h.app.run("photo.setGps", json!({"ids": [id], "lat": lat, "lon": lon})).unwrap();
    }
    h.app.run("view.map", json!({})).unwrap();
    frames(&mut h, 4);
    (h, ids)
}

fn frames(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn filter(h: &mut Headless, mode: &str) -> Value {
    let r = h.app.run("map.locationFilter", json!({"mode": mode})).unwrap();
    frames(h, 3);
    r
}

fn shown(h: &mut Headless) -> Vec<u64> {
    let mut v: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    v.sort_unstable();
    v
}

fn sorted(mut v: Vec<u64>) -> Vec<u64> {
    v.sort_unstable();
    v
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

#[test]
fn tagged_untagged_and_none_narrow_the_filmstrip_and_count() {
    let (mut h, ids) = demo();
    let all = shown(&mut h);
    let r = filter(&mut h, "tagged");
    assert_eq!(
        (r["tagged"].as_u64(), r["untagged"].as_u64(), r["all"].as_u64()),
        (Some(6), Some(all.len() as u64 - 6), Some(all.len() as u64)),
        "{r}"
    );
    assert_eq!(r["shown"], 6);
    assert_eq!(shown(&mut h), sorted(ids.clone()), "the filmstrip holds the tagged photos");
    // Untagged: the others, no pins, and a note saying they can be dragged on
    filter(&mut h, "untagged");
    let s = h.app.run("map.state", json!({})).unwrap();
    assert!(s["pins"].as_array().unwrap().is_empty(), "{s}");
    assert_eq!(shown(&mut h).len(), all.len() - 6);
    assert!(ids.iter().all(|id| !shown(&mut h).contains(id)));
    assert!(widget(&h, "map:untaggedNote").is_some());
    // the bar shows each choice with its count
    let bar = widget(&h, "map:filterBar").expect("the location filter bar");
    for m in ["visible", "tagged", "untagged", "none"] {
        assert!(widget(&h, &format!("map:filter:{m}")).is_some_and(|r| bar.contains(r.center())), "{m}");
    }
    // a click on Tagged in the bar
    let at = widget(&h, "map:filter:tagged").unwrap().center();
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerMoved(at));
        h.step();
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    frames(&mut h, 3);
    assert_eq!(h.app.run("map.state", json!({})).unwrap()["locationFilter"]["mode"], "tagged");
    assert_eq!(shown(&mut h), sorted(ids.clone()));
    filter(&mut h, "none");
    assert_eq!(shown(&mut h), all);
    assert!(h.app.run("map.locationFilter", json!({"mode": "somewhere"})).is_err());
}

/// The narrowing sits on the library's filter, is never saved as it, and leaving the Map view gives
/// back exactly the grid's list; coming back brings the choice back.
#[test]
fn it_sits_on_the_librarys_filter_and_leaving_the_map_restores_the_grid() {
    let (mut h, ids) = demo();
    // the library's own filter: rated three stars or more
    let everything = shown(&mut h);
    h.app.run("library.filter", json!({"rating": 3, "ratingOp": "atLeast"})).unwrap();
    frames(&mut h, 2);
    let grid = shown(&mut h);
    assert!(grid.len() < everything.len() && !grid.is_empty(), "the library's filter narrows: {} of {}", grid.len(), everything.len());
    let filter_before = h.app.session.filter.clone();
    filter(&mut h, "tagged");
    let narrowed = shown(&mut h);
    assert!(narrowed.iter().all(|id| grid.contains(id) && ids.contains(id)), "{narrowed:?}");
    assert_eq!(h.app.session.filter, filter_before, "the library's filter is untouched");
    h.app.run("view.photoGrid", json!({})).unwrap();
    frames(&mut h, 2);
    assert!(h.app.session.narrow.is_none());
    assert_eq!(shown(&mut h), grid, "the grid's list, exactly");
    h.app.run("view.map", json!({})).unwrap();
    frames(&mut h, 2);
    assert_eq!(shown(&mut h), narrowed, "the choice is kept for the session");
}

/// Visible on Map: the photos inside the view; a command's view applies at once, a pan once the
/// view rests; the count follows the view at once.
#[test]
fn visible_on_map_follows_the_view() {
    let (mut h, ids) = demo();
    h.app.run("map.setView", json!({"lat": 48.857, "lon": 2.32, "zoom": 11})).unwrap();
    frames(&mut h, 2);
    let r = filter(&mut h, "visible");
    assert_eq!(r["visible"], 3, "{r}");
    assert_eq!(shown(&mut h), sorted(ids[..3].to_vec()), "Paris's three");
    assert!(r["area"]["south"].as_f64().unwrap() < 48.85 && r["area"]["north"].as_f64().unwrap() > 48.86, "{r}");
    // a command's view applies at once
    h.app.run("map.setView", json!({"lat": 51.5, "lon": -0.1, "zoom": 10})).unwrap();
    frames(&mut h, 2);
    assert_eq!(shown(&mut h), sorted(ids[3..5].to_vec()), "London's two");
    // a pan: the count at once, the filmstrip only once the view rests
    h.app.ui.map.center_lat = 40.75;
    h.app.ui.map.center_lon = -73.98;
    h.step();
    let s = h.app.run("map.state", json!({})).unwrap();
    assert_eq!(s["locationFilter"]["visible"], 1, "{s}");
    assert_eq!(shown(&mut h), sorted(ids[3..5].to_vec()), "not yet: the view hasn't rested");
    // (headless time is the simulated clock: frames, not sleeps)
    let rest = h.time() + 0.3;
    while h.time() < rest {
        h.step();
    }
    frames(&mut h, 2);
    assert_eq!(shown(&mut h), vec![ids[5]], "New York's one once the view rested");
    // the whole world at low zoom, and across the antimeridian
    h.app.run("map.setView", json!({"lat": 0, "lon": 180, "zoom": 1})).unwrap();
    frames(&mut h, 2);
    assert_eq!(shown(&mut h).len(), 6, "every tagged photo");
}

/// An empty library: every count is 0, nothing breaks.
#[test]
fn an_empty_library_counts_nothing() {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), Services::default()), [900.0, 700.0], 1.0);
    h.app.run("view.map", json!({})).unwrap();
    frames(&mut h, 2);
    for m in ["visible", "tagged", "untagged", "none"] {
        let r = filter(&mut h, m);
        assert_eq!((r["visible"].as_u64(), r["all"].as_u64(), r["shown"].as_u64()), (Some(0), Some(0), Some(0)), "{m}: {r}");
    }
}
