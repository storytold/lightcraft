//! Headless tests of the Map view's location filter bar (LRC-MAP-FILTER): Visible on Map, Tagged,
//! Untagged and None narrow the filmstrip, with counts; Visible on Map follows the view; leaving
//! the Map view gives back the grid's photos; the antimeridian, empty libraries and many photos.
//! Tiles come from the fake server of `tests_map` (never the network) or are off.

use std::collections::BTreeSet;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::tests_map::{SETTLE, T, clusters, demo, demo_sized, fake, frames, ids_of, run, state, widget};
use crate::{LightcraftApp, Services};

fn shown(h: &mut Headless) -> BTreeSet<u64> {
    h.app.session.visible_cloned().iter().map(|p| p.0).collect()
}

fn set(ids: &[u64]) -> BTreeSet<u64> {
    ids.iter().copied().collect()
}

fn filter(h: &mut Headless, mode: &str) -> Value {
    let r = run(h, "map.locationFilter", json!({"mode": mode}));
    frames(h, 3);
    r
}

/// Run frames until `done` holds (with the session at hand) or [`T`] passes.
fn until(h: &mut Headless, mut done: impl FnMut(&mut Headless) -> bool) -> bool {
    let t0 = std::time::Instant::now();
    while !done(h) {
        if t0.elapsed() > T {
            return false;
        }
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    true
}

/// The photos the filmstrip draws (its `film:<id>` widgets).
fn in_filmstrip(h: &Headless) -> BTreeSet<u64> {
    h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("film:").and_then(|id| id.parse().ok())).collect()
}

/// The photos the map pins (every cluster's).
fn pinned(h: &mut Headless) -> BTreeSet<u64> {
    clusters(&state(h)).iter().flat_map(ids_of).collect()
}

#[test]
fn each_mode_filters_the_filmstrip_and_counts_its_photos() {
    let (mut h, ids) = demo(Some(fake(false)));
    let all = shown(&mut h);
    let total = all.len();
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 48.857, "lon": 2.32, "zoom": 11}));
    frames(&mut h, 3);
    // the bar is there, with None chosen: nothing narrowed
    for id in ["map:filterBar", "map:filter:visible", "map:filter:tagged", "map:filter:untagged", "map:filter:none"] {
        assert!(widget(&h, id).is_some(), "{id}");
    }
    let s = state(&mut h)["locationFilter"].clone();
    assert_eq!(s["mode"], "none");
    assert_eq!(
        (s["visible"].as_u64(), s["tagged"].as_u64(), s["untagged"].as_u64(), s["all"].as_u64()),
        (Some(3), Some(6), Some(total as u64 - 6), Some(total as u64)),
        "{s}"
    );
    assert_eq!(shown(&mut h), all);
    let text = h.painted_text().join("\n");
    for label in ["Location Filter:", "Visible on Map (3)", "Tagged (6)", &format!("Untagged ({})", total - 6), &format!("None ({total})")] {
        assert!(text.contains(label), "{label}: {text}");
    }

    // Tagged: the six with a position, in the filmstrip and on the map
    let r = filter(&mut h, "tagged");
    assert_eq!((r["mode"].as_str(), r["shown"].as_u64()), (Some("tagged"), Some(6)), "{r}");
    assert_eq!(shown(&mut h), set(&ids));
    assert_eq!(in_filmstrip(&h), set(&ids));
    // Untagged: the others; no pins
    filter(&mut h, "untagged");
    let untagged: BTreeSet<u64> = all.difference(&set(&ids)).copied().collect();
    assert_eq!(shown(&mut h), untagged);
    assert!(in_filmstrip(&h).is_subset(&untagged) && !in_filmstrip(&h).is_empty());
    assert!(pinned(&mut h).is_empty(), "no pins for photos without a position");
    assert!(h.painted_text().join("\n").contains("the filmstrip shows the photos without a location"));
    // Visible on Map: the three in Paris, which are also the pins on screen
    filter(&mut h, "visible");
    assert_eq!(shown(&mut h), set(&ids[..3]));
    assert_eq!(in_filmstrip(&h), set(&ids[..3]));
    assert_eq!(pinned(&mut h), set(&ids[..3]), "pins and filmstrip agree");
    let area = state(&mut h)["locationFilter"]["area"].clone();
    assert!(area["west"].as_f64().unwrap() < 2.32 && area["east"].as_f64().unwrap() > 2.32, "{area}");
    // None: everything again
    filter(&mut h, "none");
    assert_eq!(shown(&mut h), all);
    assert!(h.app.session.narrow.is_none());

    // a click on the bar chooses, and the choice is highlighted (the selected button)
    let b = widget(&h, "map:filter:tagged").unwrap().center();
    h.request("ui.click", json!({"x": b.x, "y": b.y}), T);
    frames(&mut h, 3);
    assert_eq!(h.app.map.location, crate::panels::map::LocationFilter::Tagged);
    assert_eq!(shown(&mut h), set(&ids));

    // bad modes are refused and change nothing
    for bad in [json!({"mode": "nearby"}), json!({"mode": 3}), json!({"mode": ""})] {
        let r = h.request("engine.execute", json!({"command": "map.locationFilter", "params": bad}), T);
        assert_eq!(r["ok"], false, "{bad}: {r}");
    }
    assert_eq!(run(&mut h, "map.locationFilter", json!({}))["mode"], "tagged", "no mode: reports");
}

#[test]
fn visible_on_map_follows_the_view() {
    let (mut h, ids) = demo(Some(fake(false)));
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 48.857, "lon": 2.32, "zoom": 11}));
    frames(&mut h, 2);
    filter(&mut h, "visible");
    assert_eq!(shown(&mut h), set(&ids[..3]));
    // a command moves the view: the filmstrip follows at once
    run(&mut h, "map.setView", json!({"lat": 51.505, "lon": -0.1, "zoom": 11}));
    assert_eq!(shown(&mut h), set(&ids[3..5]), "London");
    assert_eq!(state(&mut h)["locationFilter"]["visible"], 2);
    // pinching out (a gesture) widens it to Paris and London once the view rests
    run(&mut h, "map.setView", json!({"lat": 50.0, "lon": 1.0, "zoom": 11}));
    frames(&mut h, 2);
    let canvas = h.app.map.rect.unwrap();
    h.request("ui.move", json!({"x": canvas.center().x, "y": canvas.center().y}), T);
    h.step();
    for _ in 0..2 {
        h.request("ui.zoom", json!({"factor": 0.125}), T);
        frames(&mut h, 2);
    }
    assert!(state(&mut h)["zoom"].as_f64().unwrap() <= 6.0);
    // the count follows the view at once; the filmstrip once the view has rested
    assert_eq!(state(&mut h)["locationFilter"]["visible"], 5);
    assert!(until(&mut h, |h| h.app.session.visible().len() == 5), "the filmstrip catches up with the view");
    assert_eq!(shown(&mut h), set(&ids[..5]));
    frames(&mut h, 2);
    assert_eq!(in_filmstrip(&h), set(&ids[..5]));
    // a drag pans New York into view (and Europe out)
    run(&mut h, "map.setView", json!({"lat": 45.0, "lon": -40.0, "zoom": 3}));
    let ny = shown(&mut h);
    assert!(ny.contains(&ids[5]), "{ny:?}");
    let (cx, cy) = (canvas.center().x, canvas.center().y);
    h.request("ui.drag", json!({"x": cx, "y": cy, "toX": cx - 500.0, "toY": cy, "steps": 8}), T);
    assert!(until(&mut h, |h| h.app.session.visible().iter().all(|p| p.0 != ids[5])), "New York left the view");
}

#[test]
fn leaving_the_map_view_gives_back_the_grids_photos() {
    let (mut h, ids) = demo(None);
    // a library filter of its own: three stars and up (the first four placed photos)
    for (i, id) in ids.iter().enumerate() {
        run(&mut h, "photo.rate", json!({"ids": [id], "rating": if i < 4 { 4 } else { 1 }}));
    }
    run(&mut h, "library.filter", json!({"rating": 3}));
    let grid = shown(&mut h);
    let library_filter = h.app.session.filter.clone();
    assert!(grid.len() >= 4 && grid.contains(&ids[0]) && !grid.contains(&ids[5]), "{grid:?}");
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 2);
    filter(&mut h, "untagged");
    let untagged = shown(&mut h);
    assert!(untagged.is_subset(&grid), "the location filter narrows the library's filter, never widens it");
    assert!(ids.iter().all(|id| !untagged.contains(id)));
    // leave: the grid shows what it showed, and the library's filter is the same
    run(&mut h, "view.photoGrid", json!({}));
    assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
    assert_eq!(shown(&mut h), grid);
    assert_eq!(h.app.session.filter, library_filter);
    assert!(h.app.session.narrow.is_none());
    frames(&mut h, 2);
    assert_eq!(shown(&mut h), grid);
    // leaving another way (D for Detail) too
    run(&mut h, "view.map", json!({}));
    assert_eq!(shown(&mut h), untagged, "the choice is kept for the session");
    h.app.ui.view = ViewMode::Detail;
    frames(&mut h, 1);
    assert_eq!(shown(&mut h), grid);
    // and the library filter can still change while on the map: the location filter sits on it
    run(&mut h, "view.map", json!({}));
    run(&mut h, "library.filter", json!({"rating": 0}));
    filter(&mut h, "tagged");
    assert_eq!(shown(&mut h), set(&ids));
}

#[test]
fn visible_on_map_across_the_antimeridian() {
    let (mut h, ids) = demo(None);
    // Fiji and Samoa, either side of ±180°
    run(&mut h, "photo.setGps", json!({"ids": [ids[0]], "lat": -17.7, "lon": 178.0}));
    run(&mut h, "photo.setGps", json!({"ids": [ids[1]], "lat": -13.8, "lon": -172.0}));
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": -15.0, "lon": 180.0, "zoom": 4}));
    frames(&mut h, 2);
    filter(&mut h, "visible");
    let s = state(&mut h)["locationFilter"].clone();
    let (west, east) = (s["area"]["west"].as_f64().unwrap(), s["area"]["east"].as_f64().unwrap());
    assert!(east < west, "the view wraps: {s}");
    assert_eq!(shown(&mut h), set(&ids[..2]), "{s}");
    assert_eq!(s["visible"], 2);
    assert_eq!(pinned(&mut h), set(&ids[..2]));
    // zoomed out to the whole world (and past it): every photo with a position
    run(&mut h, "map.setView", json!({"lat": 0.0, "lon": 180.0, "zoom": 0}));
    frames(&mut h, 2);
    let s = state(&mut h)["locationFilter"].clone();
    assert_eq!((s["area"]["west"].as_f64(), s["area"]["east"].as_f64()), (Some(-180.0), Some(180.0)), "{s}");
    assert_eq!(shown(&mut h), set(&ids));
    assert_eq!(s["visible"], 6);
}

#[test]
fn empty_and_untagged_libraries_never_crash() {
    // an empty library: every mode, every view, nothing to show
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.session.map.set_source(lightcraft_map::TileSource::offline());
    h.settle(SETTLE);
    run(&mut h, "view.map", json!({}));
    for mode in ["visible", "tagged", "untagged", "none"] {
        let r = filter(&mut h, mode);
        assert_eq!((r["shown"].as_u64(), r["all"].as_u64()), (Some(0), Some(0)), "{mode}: {r}");
        run(&mut h, "map.setView", json!({"lat": 0.0, "lon": -180.0, "zoom": 0}));
        run(&mut h, "map.fit", json!({}));
        frames(&mut h, 2);
    }
    // a library of photos without GPS: Tagged and Visible are empty, Untagged is everything
    let (mut h, ids) = demo(None);
    run(&mut h, "photo.removeGps", json!({"ids": ids}));
    let all = shown(&mut h);
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 2);
    for (mode, want) in [("tagged", BTreeSet::new()), ("visible", BTreeSet::new()), ("untagged", all.clone()), ("none", all.clone())] {
        filter(&mut h, mode);
        assert_eq!(shown(&mut h), want, "{mode}");
    }
    filter(&mut h, "tagged");
    assert!(h.painted_text().join("\n").contains("No photos match the location filter"));
    // a tiny window: still no crash, and the filter still applies
    let (mut h, ids) = demo_sized(None, [300.0, 200.0]);
    run(&mut h, "view.map", json!({}));
    frames(&mut h, 2);
    filter(&mut h, "tagged");
    assert_eq!(shown(&mut h), set(&ids));
}

/// Thousands of photos: the counts and the filmstrip's narrowing stay well inside a frame.
#[test]
fn many_photos_are_counted_and_narrowed_quickly() {
    let (mut h, _) = demo(None);
    let n = 20_000u64;
    for i in 0..n {
        let id = h.app.session.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::Demo { scene: 0 }, "many.jpg", "JPEG", 600, 400, "2026-09-20T10:00:00");
        // a grid over Europe; one in ten without a position
        if i % 10 != 0 {
            p.meta.gps = Some((35.0 + (i % 250) as f64 * 0.1, -10.0 + (i / 250) as f64 * 0.5));
        }
        h.app.session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    run(&mut h, "view.map", json!({}));
    run(&mut h, "map.setView", json!({"lat": 45.0, "lon": 5.0, "zoom": 6}));
    frames(&mut h, 2);
    let t0 = crate::now_ms();
    let r = filter(&mut h, "visible");
    let s = state(&mut h)["locationFilter"].clone();
    assert!(r["shown"].as_u64().unwrap() > 1000, "{r}");
    assert_eq!(s["visible"], r["shown"], "the count is the filmstrip's: {s}");
    assert_eq!(s["untagged"].as_u64(), Some(h.app.session.visible_base_shared().1.len() as u64 - s["tagged"].as_u64().unwrap()));
    // every photo the filmstrip shows is in the area
    let area = s["area"].clone();
    let a = |k: &str| area[k].as_f64().unwrap();
    for id in h.app.session.visible_cloned() {
        let (lat, lon) = h.app.session.catalog.photo(id).and_then(|p| p.meta.gps).unwrap();
        assert!(lat >= a("south") && lat <= a("north") && lon >= a("west") && lon <= a("east"), "{id:?}");
    }
    eprintln!("20 000 photos: count {:.2} ms, narrow + 3 frames {:.1} ms", s["countMs"].as_f64().unwrap(), crate::now_ms() - t0);
    assert!(s["countMs"].as_f64().unwrap() < 50.0, "{s}");
    let _ = PhotoId(0);
}
