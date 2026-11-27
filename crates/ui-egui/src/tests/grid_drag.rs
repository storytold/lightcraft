//! Only the primary button drags photos out of the grid and drops them: a right-button drag over a
//! cell leaves the selection alone and carries nothing, and a right-button release while photos
//! are being dragged neither drops them nor ends the drag.

use std::time::Duration;

use egui::{Pos2, Rect};
use lightcraft_catalog::AlbumId;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// The demo library in the grid, the Albums section on the left.
fn app() -> Headless {
    let session = lightcraft_engine::Session::with_demo();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1300.0, 1000.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true, "sidebar": ["albums"]}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..6 {
        h.step();
    }
    h
}

fn widget(h: &Headless, id: &str) -> Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn button(h: &mut Headless, at: Pos2, button: egui::PointerButton, pressed: bool) {
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE });
    h.step();
}

fn glide(h: &mut Headless, from: Pos2, to: Pos2) {
    for i in 1..=8 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
}

fn selected(h: &Headless) -> Vec<u64> {
    let mut v: Vec<u64> = h.app.session.selection.ids.iter().map(|p| p.0).collect();
    v.sort_unstable();
    v
}

/// Three photos drawn at the top of the grid, in grid order.
fn three(h: &mut Headless) -> [u64; 3] {
    let v = h.app.session.visible_cloned();
    [v[0].0, v[1].0, v[2].0]
}

fn select(h: &mut Headless, ids: &[u64]) {
    h.app.run("library.select", json!({"ids": ids})).unwrap();
    h.settle(Duration::from_secs(60));
}

#[test]
fn a_right_button_drag_on_a_grid_cell_starts_no_photo_drag() {
    let mut h = app();
    let [a, b, c] = three(&mut h);
    select(&mut h, &[a, b]);
    let from = widget(&h, &format!("thumb:{c}")).center();
    button(&mut h, from, egui::PointerButton::Secondary, true);
    glide(&mut h, from, from + egui::vec2(60.0, 200.0));
    assert_eq!(h.app.ui.dragging_photos, None, "no photo drag from the secondary button");
    let mut both = vec![a, b];
    both.sort_unstable();
    assert_eq!(selected(&h), both, "the selection stays");
}

#[test]
fn a_left_button_drag_on_an_unselected_grid_cell_still_carries_just_that_photo() {
    let mut h = app();
    let [a, b, c] = three(&mut h);
    select(&mut h, &[a, b]);
    let from = widget(&h, &format!("thumb:{c}")).center();
    button(&mut h, from, egui::PointerButton::Primary, true);
    glide(&mut h, from, from + egui::vec2(60.0, 200.0));
    assert_eq!(h.app.ui.dragging_photos, Some(vec![c]));
    assert_eq!(selected(&h), vec![c]);
}

#[test]
fn a_right_button_release_over_an_album_row_neither_drops_nor_ends_the_photo_drag() {
    let mut h = app();
    let album = h.app.run("album.create", json!({"name": "Grid Target", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    h.step();
    h.step();
    let [a, ..] = three(&mut h);
    select(&mut h, &[a]);
    let row = widget(&h, &format!("source:album:{album}")).center();
    let from = widget(&h, &format!("thumb:{a}")).center();
    button(&mut h, from, egui::PointerButton::Primary, true);
    glide(&mut h, from, row);
    assert!(h.app.ui.dragging_photos.is_some(), "the drag is on");
    button(&mut h, row, egui::PointerButton::Secondary, true);
    button(&mut h, row, egui::PointerButton::Secondary, false);
    h.step();
    assert!(h.app.session.catalog.album(AlbumId(album)).unwrap().photos.is_empty(), "a right-button release is not a drop");
    assert!(h.app.ui.dragging_photos.is_some(), "and the drag goes on");
    button(&mut h, row, egui::PointerButton::Primary, false);
    h.step();
    assert_eq!(h.app.session.catalog.album(AlbumId(album)).unwrap().photos.len(), 1, "the left-button release drops");
    assert!(h.app.ui.dragging_photos.is_none());
}

#[test]
fn a_right_button_release_over_a_keyword_row_does_not_tag_the_dragged_photos() {
    let mut h = app();
    let r = h.request("ui.set", json!({"right": "keywords"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..6 {
        h.step();
    }
    let [a, ..] = three(&mut h);
    select(&mut h, &[a]);
    let before = h.app.session.catalog.photo(lightcraft_catalog::PhotoId(a)).unwrap().meta.keywords.clone();
    let row = widget(&h, "keywordRow:mountains").center();
    let from = widget(&h, &format!("thumb:{a}")).center();
    button(&mut h, from, egui::PointerButton::Primary, true);
    glide(&mut h, from, row);
    assert!(h.app.ui.dragging_photos.is_some(), "the drag is on");
    button(&mut h, row, egui::PointerButton::Secondary, true);
    button(&mut h, row, egui::PointerButton::Secondary, false);
    h.step();
    assert_eq!(h.app.session.catalog.photo(lightcraft_catalog::PhotoId(a)).unwrap().meta.keywords, before, "a right-button release is not a drop");
    assert!(h.app.ui.dragging_photos.is_some(), "and the drag goes on");
}
