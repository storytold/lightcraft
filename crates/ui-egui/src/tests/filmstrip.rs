//! Headless tests of the filmstrip: file-name labels (#266) — a name with multi-byte (CJK)
//! characters is drawn without panicking — and its stack badges, which expand / collapse a stack.

use std::time::Duration;

use lightcraft_catalog::{Op, Stack, StackId};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

#[test]
fn survey_filmstrip_size_slider_is_available() {
    let app = crate::tests_fixtures::SyntheticLibrary::with_photos("survey-filmstrip-size", 2).app();
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "survey"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let rect = h.app.widgets.iter().find(|(id, _)| *id == "slider:surveyFilmstripSize").map(|(_, r)| *r).expect("Survey slider");
    let r = h.request("ui.drag", json!({"x": rect.left() + 2.0, "y": rect.center().y, "toX": rect.right() - 2.0, "toY": rect.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.survey_filmstrip_size > crate::state::SURVEY_FILMSTRIP_SIZE_DEFAULT, "drag changes the Survey preference");
}

#[test]
fn survey_filmstrip_size_changes_geometry() {
    let app = crate::tests_fixtures::SyntheticLibrary::with_photos("survey-filmstrip-geometry", 2).app();
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "survey"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let id = h.app.session.visible_cloned()[0];
    let cell_id = format!("film:{}", id.0);
    let before = h.app.widgets.iter().find(|(w, _)| *w == cell_id).map(|(_, r)| *r).expect("filmstrip cell");
    assert!((before.width() - 120.0).abs() < 0.1, "default cell width matches the existing appearance: {before:?}");

    let r = h.request("ui.set", json!({"surveyFilmstripSize": 200.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    let after = h.app.widgets.iter().find(|(w, _)| *w == cell_id).map(|(_, r)| *r).expect("resized filmstrip cell");
    assert!(after.width() > before.width(), "the cell grows with the filmstrip: {before:?} → {after:?}");
    assert!((after.width() / (after.height() + 16.0) - 120.0 / 142.0).abs() < 0.01, "cell width preserves the 120:142 proportion: {after:?}");
    assert_eq!(crate::panels::detail::survey_filmstrip_thumbnail_size(142.0, 1.0), 256);
    assert_eq!(crate::panels::detail::survey_filmstrip_thumbnail_size(142.0, 1.5), 384);
    assert_eq!(
        lightcraft_engine::media::thumb_bucket(crate::panels::detail::survey_filmstrip_thumbnail_size(240.0, 2.0)),
        512,
        "large, dense Survey cells request the largest supported thumbnail",
    );
}

#[test]
fn survey_filmstrip_size_keeps_canvas_in_a_short_window() {
    let app = crate::tests_fixtures::SyntheticLibrary::new("survey-filmstrip-short-window").app();
    let mut h = Headless::new(app, [1200.0, 350.0], 1.0);
    let r = h.request("ui.set", json!({"view": "survey", "surveyFilmstripSize": 240.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    let canvas = h.app.canvas_rect.expect("Survey canvas");
    assert!(canvas.height().is_finite() && canvas.height() >= 120.0, "filmstrip leaves usable canvas space: {canvas:?}");
    if let Some((_, strip)) = h.app.widgets.iter().find(|(w, _)| w == "filmstrip") {
        assert!(strip.height().is_finite() && strip.height() <= 240.0, "allocated strip is bounded: {strip:?}");
        assert!(canvas.bottom() <= strip.top(), "canvas and strip do not overlap: {canvas:?}, {strip:?}");
    }
}

#[test]
fn survey_filmstrip_size_preserves_selection_and_scroll() {
    let library = crate::tests_fixtures::SyntheticLibrary::with_photos("survey-filmstrip-scroll", 26);
    let mut h = Headless::new(library.app(), [900.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "survey"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(SETTLE), "the procedural filmstrip settles");
    let strip = h.app.widgets.iter().find(|(w, _)| w == "filmstrip").map(|(_, r)| *r).expect("filmstrip");
    let r = h.request("ui.move", json!({"x": strip.center().x, "y": strip.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..3 {
        let r = h.request("ui.scroll", json!({"dy": -300.0}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
    assert!(h.settle(SETTLE), "the filmstrip scroll settles");
    let before_scroll = h.app.film_scroll.expect("filmstrip scroll position");
    assert!(before_scroll > 0.0, "the fixture scrolls horizontally: {before_scroll}");
    let before_active = h.app.session.selection.active;
    let before_selection = h.app.session.selection.ids.clone();

    let r = h.request("ui.set", json!({"surveyFilmstripSize": 205.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.session.selection.active, before_active);
    assert_eq!(h.app.session.selection.ids, before_selection);
    let after_scroll = h.app.film_scroll.expect("filmstrip remains visible");
    assert!((after_scroll - before_scroll).abs() < 1.0, "resizing preserves the scroll offset: {before_scroll} → {after_scroll}");
}

#[test]
fn survey_filmstrip_size_is_bounded() {
    let app = crate::tests_fixtures::SyntheticLibrary::new("survey-filmstrip-bounds").app();
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "survey", "surveyFilmstripSize": 10.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.survey_filmstrip_size, crate::state::SURVEY_FILMSTRIP_SIZE_MIN);
    let r = h.request("ui.set", json!({"surveyFilmstripSize": 900.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.survey_filmstrip_size, crate::state::SURVEY_FILMSTRIP_SIZE_MAX);
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let state = crate::UiState { survey_filmstrip_size: bad, ..Default::default() }.sanitized();
        assert_eq!(state.survey_filmstrip_size, crate::state::SURVEY_FILMSTRIP_SIZE_DEFAULT);
    }
}

#[test]
fn survey_filmstrip_size_preserves_grid_preference() {
    let app = crate::tests_fixtures::SyntheticLibrary::new("survey-filmstrip-independent-size").app();
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "survey", "thumbSize": 180.0, "surveyFilmstripSize": 210.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.thumb_size, 180.0);
    assert_eq!(h.app.ui.survey_filmstrip_size, 210.0);
    let r = h.request("ui.set", json!({"view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.ui.thumb_size, 180.0);
    assert!(h.app.widgets.iter().any(|(id, _)| *id == "slider:thumbSize"), "Grid retains its own slider");
}

#[test]
fn survey_filmstrip_size_preserves_filmstrip_visibility() {
    let app = crate::tests_fixtures::SyntheticLibrary::new("survey-filmstrip-visibility").app();
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "survey", "filmstrip": false, "surveyFilmstripSize": 190.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(!h.app.ui.filmstrip);
    assert!(!h.app.widgets.iter().any(|(id, _)| id == "filmstrip"));
    assert!(h.app.widgets.iter().any(|(id, _)| id == "slider:surveyFilmstripSize"));
    let r = h.request("ui.set", json!({"filmstrip": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.filmstrip);
    assert!(h.app.widgets.iter().any(|(id, _)| id == "filmstrip"));
}

#[test]
fn survey_filmstrip_size_default_and_serialization_are_compatible() {
    let legacy: crate::UiState = serde_json::from_value(json!({"thumbSize": 180.0})).expect("legacy settings");
    assert_eq!(legacy.survey_filmstrip_size, crate::state::SURVEY_FILMSTRIP_SIZE_DEFAULT);
    let mut state = legacy;
    state.survey_filmstrip_size = 203.0;
    let serialized = serde_json::to_value(&state).expect("serialize preferences");
    assert_eq!(serialized["surveyFilmstripSize"], 203.0);
    let restored: crate::UiState = serde_json::from_value(serialized).expect("restore preferences");
    assert_eq!(restored.survey_filmstrip_size, 203.0);
}

#[test]
fn filmstrip_shows_a_cjk_file_name() {
    let mut session = lightcraft_engine::Session::with_demo();
    let first = session.visible_cloned()[0];
    let source = session.catalog.photo(first).expect("demo photo").source.clone();
    // the name from #266: cutting it at byte 13 lands inside '限'
    session.catalog.apply(Op::SetFile { id: first, file_name: "202407層三限定訂閱圖(4).jpg".into(), source }).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.settings.film_names);
    let film = format!("film:{}", first.0);
    assert!(h.app.widgets.iter().any(|(w, _)| *w == film), "{film} drawn in the filmstrip");
}

#[test]
fn filmstrip_stack_badge_expands_and_collapses_the_stack() {
    // Badge routing needs three real photos, not full 24 MP demo renders.
    let library = crate::tests_fixtures::SyntheticLibrary::with_photos("filmstrip-stack-badge", 3);
    let mut app = library.app();
    let vis = app.session.visible_cloned();
    let (loose, top, member) = (vis[0], vis[1], vis[2]);
    let stack = Stack { id: StackId(1), photos: vec![top, member], collapsed: true };
    app.session.catalog.apply(Op::AddStack { stack }).unwrap();
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    // the loose photo is the one in the loupe: the stack isn't selected
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [loose.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(SETTLE), "the filmstrip settles before clicking its stack badge");
    assert!(!h.app.session.visible_cloned().contains(&member), "collapsed: only the top shows");
    let badge = format!("filmstack:{}", top.0);
    let r = h.request("ui.clickWidget", json!({"id": badge}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(SETTLE), "the expanded stack settles");
    assert!(!h.app.session.catalog.stack(StackId(1)).unwrap().collapsed, "a click on the badge expands the stack");
    assert!(h.app.session.visible_cloned().contains(&member));
    assert_eq!(h.app.session.selection.active, Some(loose), "the loupe keeps its photo");
    let r = h.request("ui.clickWidget", json!({"id": badge}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.settle(SETTLE), "the collapsed stack settles");
    assert!(h.app.session.catalog.stack(StackId(1)).unwrap().collapsed, "a second click collapses it");
    assert!(!h.app.session.visible_cloned().contains(&member));
}

/// The filmstrip rings its selected photos by the same rule as the grid, in the theme's colours:
/// the active photo's ring is heavier than the other selected photo's, and a light theme gets no
/// fixed white or grey ring.
#[test]
fn the_filmstrip_marks_the_active_photo_heavier_than_the_other_selected_one() {
    use egui::epaint::Shape;
    let mut session = lightcraft_engine::Session::with_demo();
    let ids = session.visible_cloned();
    let (a, b, c) = (ids[0], ids[1], ids[2]);
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.settings.appearance_mode = crate::state::AppearanceMode::Light;
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [a.0, b.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let t = crate::theme::Tokens::get(&h.view.ctx);
    assert!(crate::theme::current(&h.view.ctx).is_some_and(|k| !k.is_dark()));
    let active = h.app.session.selection.active.expect("active");
    let other = if active == a { b } else { a };
    let shapes = h.view.painted_shapes();
    let strokes = |id: lightcraft_catalog::PhotoId| -> Vec<(f32, egui::Color32)> {
        let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("film:{}", id.0)).map(|(_, r)| *r).expect("cell drawn");
        shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Rect(rs) if rs.stroke.width > 0.0 && rs.stroke_kind == egui::StrokeKind::Outside && cell.contains_rect(rs.rect) => {
                    Some((rs.stroke.width, rs.stroke.color))
                }
                _ => None,
            })
            .collect()
    };
    let (on_active, on_other, on_unselected) = (strokes(active), strokes(other), strokes(c));
    assert_eq!(on_active, vec![(3.0, t.ring_active), (1.0, t.ring_inner)]);
    assert_eq!(on_other, vec![(1.5, t.ring_selected)]);
    assert!(on_unselected.is_empty(), "{on_unselected:?}");
}
