//! Photos dragged from the filmstrip carry what a drag from the grid carries (the selection, or just
//! the photo when it isn't selected), land on the same sidebar targets, and a plain click on a
//! filmstrip cell still only selects.

use std::time::Duration;

use egui::{Pos2, Rect};
use lightcraft_catalog::{AlbumId, PhotoId};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// The demo library in the Detail view (filmstrip at the bottom), the Albums section on the left.
fn app() -> Headless {
    let session = lightcraft_engine::Session::with_demo();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1300.0, 1000.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "leftPanel": true, "sidebar": ["albums"]}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..6 {
        h.step();
    }
    h
}

fn widget(h: &Headless, id: &str) -> Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn cell(h: &Headless, id: u64) -> Pos2 {
    widget(h, &format!("film:{id}")).center()
}

fn button(h: &mut Headless, at: Pos2, pressed: bool) {
    button_by(h, at, egui::PointerButton::Primary, pressed);
}

fn button_by(h: &mut Headless, at: Pos2, button: egui::PointerButton, pressed: bool) {
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

fn select(h: &mut Headless, ids: &[u64]) {
    h.app.run("library.select", json!({"ids": ids})).unwrap();
    h.settle(Duration::from_secs(60));
}

fn selected(h: &Headless) -> Vec<u64> {
    let mut v: Vec<u64> = h.app.session.selection.ids.iter().map(|p| p.0).collect();
    v.sort_unstable();
    v
}

fn sorted(mut v: Vec<u64>) -> Vec<u64> {
    v.sort_unstable();
    v
}

/// Three photos in the strip that are far enough left to be drawn, in strip order.
fn three(h: &mut Headless) -> [u64; 3] {
    let v = h.app.session.visible_cloned();
    [v[0].0, v[1].0, v[2].0]
}

#[test]
fn an_unselected_filmstrip_photo_drags_just_itself() {
    let mut h = app();
    let [a, b, c] = three(&mut h);
    select(&mut h, &[a, b]);
    let from = cell(&h, c);
    button(&mut h, from, true);
    glide(&mut h, from, from + egui::vec2(60.0, -200.0));
    assert_eq!(h.app.ui.dragging_photos, Some(vec![c]), "the photo itself is dragged");
    assert_eq!(selected(&h), vec![c], "and it replaced the selection, as in the grid");
}

/// Only the primary button drags: a right-button drag over an unselected cell leaves the selection
/// as it was and starts no drag.
#[test]
fn a_right_button_drag_on_a_filmstrip_cell_starts_no_photo_drag() {
    let mut h = app();
    let [a, b, c] = three(&mut h);
    select(&mut h, &[a, b]);
    let from = cell(&h, c);
    button_by(&mut h, from, egui::PointerButton::Secondary, true);
    glide(&mut h, from, from + egui::vec2(60.0, -200.0));
    assert_eq!(h.app.ui.dragging_photos, None, "no photo drag from the secondary button");
    assert_eq!(selected(&h), sorted(vec![a, b]), "the selection stays");
}

#[test]
fn a_selected_filmstrip_photo_drags_the_whole_selection() {
    let mut h = app();
    let [a, b, c] = three(&mut h);
    select(&mut h, &[a, b, c]);
    let from = cell(&h, b);
    button(&mut h, from, true);
    glide(&mut h, from, from + egui::vec2(60.0, -200.0));
    let mut dragged = h.app.ui.dragging_photos.clone().expect("a drag");
    dragged.sort_unstable();
    assert_eq!(dragged, sorted(vec![a, b, c]));
    assert_eq!(selected(&h), dragged, "the selection is untouched");
}

#[test]
fn a_click_on_a_filmstrip_cell_selects_and_is_not_a_drag() {
    let mut h = app();
    let [a, b, _] = three(&mut h);
    select(&mut h, &[a]);
    let at = cell(&h, b);
    button(&mut h, at, true);
    assert!(h.app.ui.dragging_photos.is_none(), "pressed: not yet a drag");
    button(&mut h, at, false);
    h.step();
    assert!(h.app.ui.dragging_photos.is_none(), "released in place: no drag");
    assert_eq!(selected(&h), vec![b], "the click selected the cell");
    assert_eq!(h.app.session.selection.active, Some(PhotoId(b)), "and the loupe follows it");
}

#[test]
fn a_filmstrip_drag_dropped_on_an_album_adds_the_photos() {
    let mut h = app();
    let album = h.app.run("album.create", json!({"name": "Strip Target", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    h.step();
    h.step();
    let [a, b, _] = three(&mut h);
    select(&mut h, &[a, b]);
    let row = widget(&h, &format!("source:album:{album}")).center();
    let from = cell(&h, a);
    button(&mut h, from, true);
    glide(&mut h, from, row);
    assert!(h.app.ui.dragging_photos.is_some(), "the drag is on");
    button(&mut h, row, false);
    h.step();
    let mut members: Vec<u64> = h.app.session.catalog.album(AlbumId(album)).unwrap().photos.iter().map(|p| p.0).collect();
    members.sort_unstable();
    assert_eq!(members, sorted(vec![a, b]));
    assert!(h.app.ui.dragging_photos.is_none(), "the drag ended");
}

/// In Compare, the active photo is always one of the two shown: dragging an unselected photo from
/// the strip carries it alone and leaves the selection, the active photo and the pair as they were.
#[test]
fn a_filmstrip_drag_in_compare_keeps_the_active_photo_on_screen() {
    let mut h = app();
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    h.app.run("library.select", json!({"ids": [vis[0], vis[1]], "active": vis[0]})).unwrap();
    h.app.run("view.compare", json!({})).unwrap();
    h.settle(Duration::from_secs(60));
    let (sel, cand) = h.app.ui.compare.expect("comparing");
    let before = selected(&h);
    let active_before = h.app.session.selection.active.map(|p| p.0);
    let from = cell(&h, vis[4]);
    button(&mut h, from, true);
    glide(&mut h, from, from + egui::vec2(60.0, -200.0));
    assert_eq!(h.app.ui.dragging_photos, Some(vec![vis[4]]), "the photo is dragged");
    assert_eq!(selected(&h), before, "the selection stays");
    let active = h.app.session.selection.active.map(|p| p.0).expect("an active photo");
    assert_eq!(Some(active), active_before, "the active photo is unchanged");
    assert!(active == sel || active == cand, "active {active} is on screen ({sel} | {cand})");
    assert_eq!(h.app.ui.compare, Some((sel, cand)), "the pair stays");
}
