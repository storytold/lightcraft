//! Headless tests of Guided Upright guide selection and Delete (issue: remove one guide): a click
//! near a guide selects it, Delete removes the selected guide alone, a click away clears the
//! selection, and with no selection Delete removes nothing (never the photo).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Crop with Guided Upright on and two horizontal guides drawn (in lens-corrected coordinates).
fn guided() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for (method, params) in [
        ("ui.set", json!({"view": "detail"})),
        ("engine.execute", json!({"command": "tool.guidedUpright"})),
        ("engine.execute", json!({"command": "geometry.guides", "params": {"guides": [[0.2, 0.3, 0.8, 0.3], [0.2, 0.7, 0.8, 0.7]]}})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    h
}

fn guides_len(h: &Headless) -> usize {
    let id = h.app.session.active().expect("a photo");
    h.app.session.catalog.photo(id).expect("the photo").develop.geometry.guides.len()
}

fn guide_at(h: &Headless, i: usize) -> egui::Pos2 {
    guide_rect(h, i).center()
}

fn guide_rect(h: &Headless, i: usize) -> egui::Rect {
    h.app.widgets.iter().rev().find(|(w, _)| *w == format!("uprightGuide:{i}")).map(|(_, r)| *r).expect("the guide is on screen")
}

/// A click near a guide selects it (Delete has a visible target); the guides stay put.
#[test]
fn a_click_near_a_guide_selects_it() {
    let mut h = guided();
    assert_eq!(guides_len(&h), 2);
    let id = h.app.session.active().expect("a photo");
    let r = h.request("ui.click", json!({"x": guide_at(&h, 0).x, "y": guide_at(&h, 0).y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.selected_guide, Some((id.0, 0)), "the clicked guide is selected");
    assert_eq!(guides_len(&h), 2, "selecting removes nothing");
}

/// Delete removes the selected guide alone; the other one stays, and nothing stays selected.
#[test]
fn delete_removes_the_selected_guide() {
    let mut h = guided();
    let r = h.request("ui.click", json!({"x": guide_at(&h, 1).x, "y": guide_at(&h, 1).y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.selected_guide.map(|(_, i)| i), Some(1));
    let r = h.request("ui.key", json!({"key": "Delete"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 1, "one guide is gone");
    assert_eq!(h.app.ui.selected_guide, None, "and nothing is selected afterwards");
    // the survivor is the first guide: the second one's endpoints are gone
    let id = h.app.session.active().expect("a photo");
    let d = h.app.session.catalog.photo(id).expect("the photo");
    let (a, b) = d.develop.geometry.guides[0];
    assert!(
        (a.x - 0.2).abs() < 1e-6 && (a.y - 0.3).abs() < 1e-6 && (b.x - 0.8).abs() < 1e-6 && (b.y - 0.3).abs() < 1e-6,
        "the first guide survived: {a:?}–{b:?}"
    );
}

/// A click away from every guide clears the selection (and removes nothing).
#[test]
fn a_click_away_clears_the_selection() {
    let mut h = guided();
    let r = h.request("ui.click", json!({"x": guide_at(&h, 0).x, "y": guide_at(&h, 0).y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.selected_guide.is_some());
    let img = h.app.image_rect.expect("the photo on screen");
    // below both guides, far from them
    let r = h.request("ui.click", json!({"x": img.center().x, "y": img.top() + 0.95 * img.height()}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.selected_guide, None);
    assert_eq!(guides_len(&h), 2, "and removes nothing");
}

/// With nothing selected Delete removes no guide and never deletes the photo: the tool is open.
#[test]
fn delete_with_no_selection_removes_nothing() {
    let mut h = guided();
    let undo = h.app.session.undo.len();
    let r = h.request("ui.key", json!({"key": "Delete"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 2, "no guide is removed");
    assert_eq!(h.app.session.undo.len(), undo, "no edit either");
    assert!(h.app.session.active().is_some(), "the photo is still active");
}

/// Drawing a new guide clears the selection first, so Delete after a draw removes the new guide.
#[test]
fn drawing_a_new_guide_clears_the_selection() {
    let mut h = guided();
    let r = h.request("ui.click", json!({"x": guide_at(&h, 0).x, "y": guide_at(&h, 0).y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.selected_guide.is_some());
    let img = h.app.image_rect.expect("the photo on screen");
    // a drag across the middle of the photo (clear of both guides) draws a third guide
    let r = h.request(
        "ui.drag",
        json!({"x": img.left() + 0.3 * img.width(), "y": img.top() + 0.5 * img.height(), "toX": img.left() + 0.7 * img.width(), "toY": img.top() + 0.5 * img.height()}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 3, "the drawn guide is added");
    assert_eq!(h.app.ui.selected_guide, None, "and the click's selection is gone");
}

/// A press on an endpoint drags it: the guide follows the pointer — the other end and the other
/// guide untouched — the moved guide ends up selected, and nothing is added (one edit, on release).
#[test]
fn dragging_a_guide_endpoint_moves_it() {
    let mut h = guided();
    assert_eq!(guides_len(&h), 2);
    let id = h.app.session.active().expect("a photo");
    let img = h.app.image_rect.expect("the photo on screen");
    let before = guide_rect(&h, 0);
    let from = before.left_top();
    let to = egui::pos2(from.x + 24.0, from.y + 60.0);
    // ui.pointer speaks normalized image coordinates
    let norm = |p: egui::Pos2| (((p.x - img.left()) / img.width()) as f64, ((p.y - img.top()) / img.height()) as f64);
    let (fx, fy) = norm(from);
    let (tx, ty) = norm(to);
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": fx, "y": fy}, {"kind": "drag", "x": tx, "y": ty}, {"kind": "up", "x": tx, "y": ty}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 2, "no guide is added");
    let d = h.app.session.catalog.photo(id).expect("the photo");
    let (a, b) = d.develop.geometry.guides[0];
    assert!((a.x - tx).abs() < 1e-3 && (a.y - ty).abs() < 1e-3, "the pressed end followed the pointer: {a:?} vs ({tx}, {ty})");
    assert!((b.x - 0.8).abs() < 1e-6 && (b.y - 0.3).abs() < 1e-6, "the far end stays: {b:?}");
    assert_eq!(h.app.ui.selected_guide, Some((id.0, 0)), "the moved guide is selected");
}

/// A press-release on an endpoint without moving it selects the guide and changes nothing: no
/// coordinate moves and no undo entry appears for a click.
#[test]
fn clicking_an_endpoint_selects_it_without_changing_it() {
    let mut h = guided();
    let id = h.app.session.active().expect("a photo");
    let undo = h.app.session.undo.len();
    let img = h.app.image_rect.expect("the photo on screen");
    let at = guide_rect(&h, 1).left_top();
    let x = ((at.x - img.left()) / img.width()) as f64;
    let y = ((at.y - img.top()) / img.height()) as f64;
    let r = h.request("ui.pointer", json!({"events": [{"kind": "down", "x": x, "y": y}, {"kind": "up", "x": x, "y": y}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.selected_guide, Some((id.0, 1)), "the endpoint's guide is selected");
    assert_eq!(guides_len(&h), 2);
    let d = h.app.session.catalog.photo(id).expect("the photo");
    let (a, b) = d.develop.geometry.guides[1];
    assert!(
        (a.x - 0.2).abs() < 1e-6 && (a.y - 0.7).abs() < 1e-6 && (b.x - 0.8).abs() < 1e-6 && (b.y - 0.7).abs() < 1e-6,
        "the guide is untouched: {a:?}–{b:?}"
    );
    assert_eq!(h.app.session.undo.len(), undo, "a click is not an edit");
}

/// A new guide starts at the press point itself: a press, one far jump, a release — the guide
/// spans exactly those two points (it used to start where the drag threshold was crossed, so the
/// anchor was already at the far end).
#[test]
fn a_new_guide_starts_at_the_press_point() {
    let mut h = guided();
    assert_eq!(guides_len(&h), 2);
    let img = h.app.image_rect.expect("the photo on screen");
    let p1 = egui::pos2(img.left() + 0.35 * img.width(), img.top() + 0.55 * img.height());
    let p2 = egui::pos2(img.left() + 0.6 * img.width(), img.top() + 0.55 * img.height());
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": 0.35, "y": 0.55}, {"kind": "drag", "x": 0.6, "y": 0.55}, {"kind": "up", "x": 0.6, "y": 0.55}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 3, "the drawn guide is added");
    assert_eq!(h.app.ui.selected_guide, None, "drawing a new guide leaves nothing selected");
    let rect = guide_rect(&h, 2);
    assert!(rect.left_top().distance(p1) < 2.0, "the guide starts at the press point: {rect:?} vs {p1:?}");
    assert!(rect.right_top().distance(p2) < 2.0, "and ends at the release point: {rect:?} vs {p2:?}");
}

/// A slow frame can swallow press, move and release into a single batch: egui then reports
/// `primary_pressed` with `press_origin` already cleared by the release, and the gesture must
/// still start at the press point — the guide spans the drag, not a zero-length stub at the
/// release. The control channel spreads pointer events one per frame, so the batch is pushed
/// raw into one frame here.
#[test]
fn a_drag_whose_press_and_release_share_one_frame_still_draws() {
    let mut h = guided();
    assert_eq!(guides_len(&h), 2);
    let undo = h.app.session.undo.len();
    let img = h.app.image_rect.expect("the photo on screen");
    let p1 = egui::pos2(img.left() + 0.35 * img.width(), img.top() + 0.55 * img.height());
    let p2 = egui::pos2(img.left() + 0.6 * img.width(), img.top() + 0.55 * img.height());
    h.events.push(egui::Event::PointerMoved(p1));
    h.events.push(egui::Event::PointerButton { pos: p1, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.events.push(egui::Event::PointerMoved(p2));
    h.events.push(egui::Event::PointerButton { pos: p2, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 3, "the drag that shared one frame still draws its guide");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one edit was written");
    let rect = guide_rect(&h, 2);
    assert!(rect.left_top().distance(p1) < 2.0, "the guide starts at the press point: {rect:?} vs {p1:?}");
    assert!(rect.right_top().distance(p2) < 2.0, "and ends at the release point: {rect:?} vs {p2:?}");
}

/// The right button is the pan / context menu's, never the guides': on a guide it selects
/// nothing and draws nothing.
#[test]
fn a_right_click_on_a_guide_selects_nothing() {
    let mut h = guided();
    let r = h.request("ui.click", json!({"x": guide_at(&h, 0).x, "y": guide_at(&h, 0).y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.selected_guide, None, "the right button selects nothing");
    assert_eq!(guides_len(&h), 2, "and draws nothing");
}

/// The navigator pans the zoomed photo: a press on it never reaches the guides — it neither
/// selects one under it nor starts drawing a new guide from it.
#[test]
fn a_press_on_the_navigator_draws_no_guide() {
    let mut h = guided();
    let r = h.request("engine.execute", json!({"command": "view.zoom100"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let nav = h.app.widgets.iter().rev().find(|(w, _)| *w == "canvas:navigator").map(|(_, r)| *r).expect("navigator shown when zoomed");
    let img = h.app.image_rect.expect("the photo on screen");
    let c = nav.center();
    let (nx, ny) = ((c.x - img.left()) / img.width(), (c.y - img.top()) / img.height());
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": nx, "y": ny}, {"kind": "drag", "x": nx + 0.05, "y": ny}, {"kind": "up", "x": nx + 0.05, "y": ny}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 2, "the press on the navigator drew no guide");
    assert_eq!(h.app.ui.selected_guide, None, "and selected nothing");
}

/// The context menu (anything floating above the photo) owns its own press: a left press on the
/// menu that sits over a guide line selects nothing under it and starts no guide gesture — the
/// canvas has to actually receive the press, not merely lie under the pointer.
#[test]
fn a_press_on_a_menu_over_a_guide_selects_nothing() {
    let mut h = guided();
    let id = h.app.session.active().expect("a photo");
    let undo = h.app.session.undo.len();
    let line = guide_at(&h, 0);
    // right-click 30px up-left of that point: the context menu opens with its top-left corner
    // at the pointer (gap 0), so its frame covers the guide from 30px inside every edge
    let r = h.request("ui.click", json!({"x": line.x - 30.0, "y": line.y - 30.0, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.view.ctx.any_popup_open(), "the context menu is open over the photo");
    let img = h.app.image_rect.expect("the photo on screen");
    let (lx, ly) = ((line.x - img.left()) / img.width(), (line.y - img.top()) / img.height());
    let r = h.request("ui.pointer", json!({"events": [{"kind": "down", "x": lx, "y": ly}, {"kind": "up", "x": lx, "y": ly}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.selected_guide, None, "the press under the menu selects nothing");
    assert!(h.app.gesture.is_none(), "and starts no gesture");
    assert_eq!(guides_len(&h), 2, "and draws nothing");
    assert_eq!(h.app.session.undo.len(), undo, "no edit was written");
    let d = h.app.session.catalog.photo(id).expect("the photo");
    let (a, b) = d.develop.geometry.guides[0];
    assert!(
        (a.x - 0.2).abs() < 1e-6 && (a.y - 0.3).abs() < 1e-6 && (b.x - 0.8).abs() < 1e-6 && (b.y - 0.3).abs() < 1e-6,
        "the guide is untouched: {a:?}–{b:?}"
    );
}

/// Same for an endpoint under the menu: a press-drag on it moves nothing and selects nothing —
/// the endpoint-drag gesture never starts because the menu, not the canvas, took the press.
#[test]
fn a_press_on_a_menu_over_an_endpoint_moves_nothing() {
    let mut h = guided();
    let id = h.app.session.active().expect("a photo");
    let undo = h.app.session.undo.len();
    let end = guide_rect(&h, 0).left_top();
    let r = h.request("ui.click", json!({"x": end.x - 30.0, "y": end.y - 30.0, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.view.ctx.any_popup_open(), "the context menu is open over the photo");
    let img = h.app.image_rect.expect("the photo on screen");
    let (ex, ey) = ((end.x - img.left()) / img.width(), (end.y - img.top()) / img.height());
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": ex, "y": ey}, {"kind": "drag", "x": ex + 0.05, "y": ey + 0.08}, {"kind": "up", "x": ex + 0.05, "y": ey + 0.08}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.selected_guide, None, "the endpoint's press under the menu selects nothing");
    assert!(h.app.gesture.is_none(), "and starts no gesture");
    assert_eq!(guides_len(&h), 2, "and draws nothing");
    assert_eq!(h.app.session.undo.len(), undo, "no edit was written");
    let d = h.app.session.catalog.photo(id).expect("the photo");
    let (a, b) = d.develop.geometry.guides[0];
    assert!(
        (a.x - 0.2).abs() < 1e-6 && (a.y - 0.3).abs() < 1e-6 && (b.x - 0.8).abs() < 1e-6 && (b.y - 0.3).abs() < 1e-6,
        "the endpoint under the menu did not move: {a:?}–{b:?}"
    );
}

/// The press can leave the menu within the same frame it started (one batch: down on the menu,
/// pointer onto bare photo, release): containment of the *current* pointer would pass then, but
/// the press belongs to the menu — the press point decides, so no guide is drawn from it.
#[test]
fn a_press_that_leaves_the_menu_in_one_frame_draws_nothing() {
    let mut h = guided();
    let undo = h.app.session.undo.len();
    let end = guide_rect(&h, 0).left_top();
    // right-click up-left of the endpoint: the menu opens with its top-left at the pointer
    let r = h.request("ui.click", json!({"x": end.x - 30.0, "y": end.y - 30.0, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.view.ctx.any_popup_open(), "the context menu is open over the photo");
    // one batch: go down 1px inside the menu, then straight off it to bare photo, far below
    // both guides, and release there — the current pointer never sees the menu again
    let img = h.app.image_rect.expect("the photo on screen");
    let menu = end - egui::vec2(29.0, 29.0);
    let bare = egui::pos2(img.center().x, img.top() + 0.95 * img.height());
    let rel = |p: egui::Pos2| (((p.x - img.left()) / img.width()) as f64, ((p.y - img.top()) / img.height()) as f64);
    let (mx, my) = rel(menu);
    let (bx, by) = rel(bare);
    let r = h.request(
        "ui.pointer",
        json!({"events": [{"kind": "down", "x": mx, "y": my}, {"kind": "drag", "x": bx, "y": by}, {"kind": "up", "x": bx, "y": by}]}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(guides_len(&h), 2, "the press that left the menu drew no guide");
    assert_eq!(h.app.ui.selected_guide, None, "and selected nothing");
    assert!(h.app.gesture.is_none(), "and leaves no gesture behind");
    assert_eq!(h.app.session.undo.len(), undo, "no edit was written");
}
