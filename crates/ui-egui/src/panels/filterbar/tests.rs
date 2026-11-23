//! Pointer/filter regressions run against both pickers; keyboard activation is an SDK-port accessibility improvement.
use std::time::Duration;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(15);

pub(super) fn ask(h: &mut Headless, method: &str, params: Value) {
    let result = h.request(method, params.clone(), T);
    assert_eq!(result["ok"], true, "{method} {params}: {result}");
    h.step();
    h.step();
}

pub(super) fn fixture(width: f32) -> Headless {
    let mut session = lightcraft_engine::Session::new();
    for (id, kind, rating) in [(101, MediaKind::Image, 5), (102, MediaKind::Raw, 4), (103, MediaKind::Image, 1)] {
        let mut photo = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, &format!("fixture-{id}"), "JPEG", 64, 64, "2026-11-19T00:00:00");
        photo.kind = kind;
        photo.rating = rating;
        session.commit("fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    let mut app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    app.ui.filter_bar = true;
    let mut h = Headless::new(app, [width, 900.0], 1.0);
    h.step();
    h.step();
    h
}

pub(super) fn visible(h: &mut Headless) -> Vec<u64> {
    h.app.session.visible_cloned().iter().map(|id| id.0).collect()
}

/// Use the real rendered option's text bounds, not a fixed screenshot coordinate or a direct
/// command dispatch. This also works on the previous native picker, which has no option IDs.
fn pick(h: &mut Headless, text: &str) {
    ask(h, "ui.clickWidget", json!({"id": "dropdown:filter-kind"}));
    let pos = h
        .view
        .painted_shapes()
        .iter()
        .rev()
        .find_map(|shape| match shape {
            egui::Shape::Text(shape) if shape.galley.job.text == text => Some(shape.pos + shape.galley.rect.center().to_vec2()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("visible option {text:?} not painted"));
    ask(h, "ui.click", json!({"x": pos.x, "y": pos.y}));
}

#[test]
fn filter_kind_real_input_apply_cancel_no_match_and_clear_exact_identities() {
    for width in [1100.0, 1900.0] {
        let mut h = fixture(width);
        let all = visible(&mut h);
        assert_eq!(all.len(), 3);
        assert!(all.contains(&101) && all.contains(&102) && all.contains(&103));
        ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [102]}}));
        // Clear must clear the complete filter and the search field, not only this picker.
        ask(&mut h, "engine.execute", json!({"command": "library.filter", "params": {"rating": 3, "text": "fixture"}}));
        h.app.ui.search = "fixture".into();
        let before = visible(&mut h);
        assert_eq!(before.len(), 2);
        assert!(before.contains(&101) && before.contains(&102));
        let filter = h.app.session.filter.clone();
        let selection = h.app.session.selection.clone();
        let active = h.app.session.active();
        ask(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-kind"}));
        ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
        assert_eq!(h.app.session.filter, filter, "uncommitted proposal does not change the library");
        ask(&mut h, "ui.key", json!({"key": "Escape"}));
        assert_eq!(h.app.session.filter, filter);
        assert_eq!(h.app.session.selection, selection);
        assert_eq!(h.app.session.active(), active);
        assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid, "Escape belongs to the open menu, not Back");
        assert_eq!(visible(&mut h), before);
        pick(&mut h, "Raw");
        assert_eq!(h.app.session.filter.kind, Some(MediaKind::Raw));
        assert_eq!(h.app.session.filter.rating, 3, "choosing kind preserves unrelated constraints");
        assert_eq!(h.app.session.filter.text, "fixture");
        assert_eq!(visible(&mut h), vec![102]);
        pick(&mut h, "Videos");
        assert_eq!(h.app.session.filter.kind, Some(MediaKind::Video));
        assert_eq!(visible(&mut h), Vec::<u64>::new(), "a valid filter can match no photos");
        ask(&mut h, "ui.clickWidget", json!({"id": "button:filterClear"}));
        assert_eq!(h.app.session.filter, Default::default());
        assert_eq!(h.app.ui.search, "");
        assert_eq!(visible(&mut h), all, "clear restores the exact prior order and identities");
    }
}

#[test]
fn filter_kind_pointer_choice_and_clear_keep_grid_shortcuts() {
    let mut h = fixture(1900.0);
    let all = visible(&mut h);
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [all[0]]}}));
    pick(&mut h, "Photos");
    let focused = h.view.ctx.memory(|memory| memory.focused());
    assert!(
        !h.view.ctx.egui_wants_keyboard_input(),
        "a pointer menu choice releases keyboard ownership: {focused:?}, response={:?}",
        focused.and_then(|id| h.view.ctx.read_response(id)),
    );
    let photos = visible(&mut h);
    let first = photos[0];
    let next = photos[1];
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [first]}}));
    ask(&mut h, "ui.key", json!({"key": "ArrowRight"}));
    assert_eq!(h.app.session.active(), Some(PhotoId(next)), "after a mouse pick, grid arrows still navigate photos");
    ask(&mut h, "ui.key", json!({"key": "4"}));
    assert_eq!(h.app.session.catalog.photo(PhotoId(next)).unwrap().rating, 4, "photo rating key still dispatches");
    ask(&mut h, "ui.clickWidget", json!({"id": "button:filterClear"}));
    assert_eq!(visible(&mut h), all);
    ask(&mut h, "ui.key", json!({"key": "2"}));
    assert_eq!(h.app.session.catalog.photo(PhotoId(next)).unwrap().rating, 2, "mouse Clear returns keyboard ownership to the grid");
}

#[test]
fn filter_kind_closed_hover_wheel_does_not_change_filter() {
    let mut h = fixture(1900.0);
    let before = visible(&mut h);
    let filter = h.app.session.filter.clone();
    ask(&mut h, "ui.hoverWidget", json!({"id": "dropdown:filter-kind"}));
    ask(&mut h, "ui.scroll", json!({"dy": -120, "unit": "point"}));
    for _ in 0..40 {
        h.step();
    }
    assert_eq!(h.app.session.filter, filter, "scrolling across a closed, unarmed picker does not edit it");
    assert_eq!(visible(&mut h), before);
    pick(&mut h, "Photos");
    let filter = h.app.session.filter.clone();
    let before = visible(&mut h);
    // Leaving the list and returning to the closed field must not leave pointer arming behind.
    ask(&mut h, "ui.move", json!({"x": 10, "y": 10}));
    ask(&mut h, "ui.hoverWidget", json!({"id": "dropdown:filter-kind"}));
    ask(&mut h, "ui.scroll", json!({"dy": -120, "unit": "point"}));
    for _ in 0..40 {
        h.step();
    }
    assert_eq!(h.app.session.filter, filter);
    assert_eq!(visible(&mut h), before);
}

// Clean-main control reaches the native opener but cannot choose a row by keyboard.
// The port deliberately completes that accessible route rather than treating it as parity.
#[test]
fn filter_kind_tab_enter_navigation_preserves_opener_focus() {
    let mut h = fixture(1900.0);
    let opener = h.app.widgets.iter().find(|(id, _)| id == "dropdown:filter-kind").unwrap().1;
    let mut reached = None;
    let mut seen = Vec::new();
    for _ in 0..80 {
        ask(&mut h, "ui.key", json!({"key": "Tab"}));
        let focused = h.view.ctx.memory(|memory| memory.focused());
        seen.push((focused, focused.and_then(|id| h.view.ctx.read_response(id)).map(|response| response.rect)));
        if focused.and_then(|id| h.view.ctx.read_response(id)).is_some_and(|response| response.rect == opener) {
            reached = focused;
            break;
        }
    }
    let focused = reached.unwrap_or_else(|| panic!("Tab reaches the native Type opener at {opener:?}; actual focus path: {seen:?}"));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.session.filter.kind, Some(MediaKind::Image), "keyboard choice activates Photos");
    assert_eq!(h.view.ctx.memory(|memory| memory.focused()), Some(focused), "keyboard activation returns focus to the opener");
    let filter = h.app.session.filter.clone();
    ask(&mut h, "ui.key", json!({"key": "Space"}));
    assert!(h.app.widgets.iter().any(|(id, _)| id == "filter:kind-image"), "Space opens the real SDK rows");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert_eq!(h.app.session.filter, filter, "keyboard cancellation applies nothing");
    assert_eq!(h.view.ctx.memory(|memory| memory.focused()), Some(focused), "Escape returns focus to the opener");
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid);
    ask(&mut h, "ui.key", json!({"key": "Tab"}));
    assert_ne!(h.view.ctx.memory(|memory| memory.focused()), Some(focused), "Tab can leave the opener");
}


#[test]
fn filter_kind_pointer_cancel_keeps_the_grid_keyboard_owner() {
    let mut h = fixture(1900.0);
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [101]}}));
    let filter = h.app.session.filter.clone();
    ask(&mut h, "ui.clickWidget", json!({"id": "dropdown:filter-kind"}));
    ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert_eq!(h.app.session.filter, filter);
    assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid);
    ask(&mut h, "ui.key", json!({"key": "2"}));
    assert_eq!(h.app.session.catalog.photo(PhotoId(101)).unwrap().rating, 2, "cancelling the menu gives the next rating key to the grid");
}
