//! The Library's Grid as Lightroom Classic's (fork-owned): one grid without date sections, the Library Filter bar
//! above it, and \ toggling that bar in the Grid (and Show Original elsewhere, as in Lightroom Classic).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::BeforeAfter;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn library() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    let r = h.request("ui.set", json!({"module": "library", "view": "photoGrid", "leftPanel": true, "right": "none"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn has(h: &Headless, prefix: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w.starts_with(prefix))
}

fn the_grid_has_no_date_sections_and_shows_the_filter_bar(h: &mut Headless) {
    assert!(!has(h, "group:"), "no date headers");
    assert!(has(h, "filterbar"), "the Library Filter bar is on top");
}

fn backslash_toggles_the_filter_bar_in_the_grid_and_show_original_elsewhere(h: &mut Headless) {
    let r = h.request("ui.set", json!({"module": "library", "view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let r = h.request("ui.key", json!({"key": "\\"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!h.app.ui.filter_bar, "\\ hides the filter bar in the Grid");
    assert_eq!(h.app.ui.before_after, BeforeAfter::Off, "and doesn't show the original");
    let r = h.request("ui.set", json!({"module": "develop", "view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let r = h.request("ui.key", json!({"key": "\\"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.before_after, BeforeAfter::Original, "in Develop \\ shows the original");
    assert!(!h.app.ui.filter_bar, "the filter bar stays as it was");
}

#[test]
fn the_grid_is_lightroom_classics_filter_bar_histogram_filmstrip_toolbar_and_backslash() {
    let mut h = library();
    the_grid_has_no_date_sections_and_shows_the_filter_bar(&mut h);
    the_library_right_column_starts_with_the_histogram(&mut h);
    the_filmstrip_runs_under_the_grid(&mut h);
    the_grid_toolbar_has_lightroom_classics_items(&mut h);
    copy_edit_settings_is_in_develops_toolbar_not_librarys(&mut h);
    backslash_toggles_the_filter_bar_in_the_grid_and_show_original_elsewhere(&mut h);
}

/// The Library's right column starts with the Histogram, as Lightroom Classic's: the active photo's, above Quick
/// Develop.
fn the_library_right_column_starts_with_the_histogram(h: &mut Headless) {
    let top = |id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.top());
    let (histogram, quick) = (top("histogram"), top("classicPanel:quickDevelop"));
    assert!(histogram.is_some() && quick.is_some(), "{histogram:?} {quick:?}");
    assert!(histogram < quick, "Histogram first");
}

/// The filmstrip runs under the Grid too, as in Lightroom Classic, and / hides it there as elsewhere.
fn the_filmstrip_runs_under_the_grid(h: &mut Headless) {
    let bottom = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r);
    let film = bottom(h, "filmstrip").expect("the filmstrip under the Grid");
    let grid = bottom(h, "gridArea").expect("the grid");
    assert!(grid.bottom() <= film.top() + 0.5, "the grid ends above it: {grid:?} {film:?}");
    // the Grid and the strip ask for different sizes of the same thumbnails: once settled, no frame builds new jobs
    let jobs = h.app.renderer.thumb_jobs_built;
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.app.renderer.thumb_jobs_built, jobs, "no thumbnail rendered again on every frame");
    let r = h.request("engine.execute", json!({"command": "view.filmstrip"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(bottom(h, "filmstrip").is_none());
}

/// The Grid's toolbar has Lightroom Classic's items: Painter, Sort by name, colour labels next to the rating and
/// flags, rotate, previous / next and the impromptu slideshow, each doing what its command does.
fn the_grid_toolbar_has_lightroom_classics_items(h: &mut Headless) {
    for id in [
        "icon:painter",
        "dropdown:sortLabel",
        "toolbarLabel:red",
        "toolbarLabel:yellow",
        "toolbarLabel:green",
        "toolbarLabel:blue",
        "toolbarLabel:purple",
        "icon:rotateLeft",
        "icon:rotateRight",
        "icon:navPrevious",
        "icon:navNext",
        "icon:slideshowPlay",
    ] {
        assert!(h.app.widgets.iter().any(|(w, _)| w == id), "{id} in the toolbar");
    }
    let active = |h: &Headless| h.app.session.active().expect("a photo");
    let first = active(h);
    // (the toolbar acts on the selection, as in Lightroom Classic: the photo clicked)
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    // a label the photo doesn't have (a click on its own label takes it off)
    let had = h.app.session.catalog.photo(first).and_then(|p| p.label);
    let (key, want) =
        if had == Some(lcc_catalog::ColorLabel::Green) { ("blue", lcc_catalog::ColorLabel::Blue) } else { ("green", lcc_catalog::ColorLabel::Green) };
    let r = h.request("ui.clickWidget", json!({"id": format!("toolbarLabel:{key}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.catalog.photo(first).and_then(|p| p.label), Some(want));
    let r = h.request("ui.clickWidget", json!({"id": format!("toolbarLabel:{key}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.catalog.photo(first).and_then(|p| p.label), None, "a second click takes it off");
    let turned = |h: &Headless| h.app.session.develop_of(first).map(|d| d.orientation);
    let before = turned(h);
    let r = h.request("ui.clickWidget", json!({"id": "icon:rotateRight"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_ne!(turned(h), before, "rotated");
    let r = h.request("ui.clickWidget", json!({"id": "icon:navNext"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_ne!(active(h), first, "the next photo");
}

/// Copy Edit Settings is Develop's toolbar button, not the Grid's (Lightroom Classic); Library copies from the Photo
/// menu.
fn copy_edit_settings_is_in_develops_toolbar_not_librarys(h: &mut Headless) {
    assert!(!has(h, "button:copySettings") && !has(h, "icon:copyGear"), "not in the Grid");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!has(h, "button:copySettings"), "nor in Library's Loupe");
    let r = h.request("ui.set", json!({"module": "develop", "view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(h, "button:copySettings"), "Develop keeps it");
}
