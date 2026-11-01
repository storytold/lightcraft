//! A plain click on the frame around a selected thumbnail (outside the picture) selects that photo alone; a click on
//! the picture keeps the selection and makes it the active photo, as in Lightroom Classic (test notes 2026-10-24
//! note 1). Grid and filmstrip.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("{id} drawn"))
}

fn click_at(h: &mut Headless, at: egui::Pos2) {
    h.events.push(egui::Event::PointerMoved(at));
    h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    // the next click is apart from this one (no double-click)
    for _ in 0..40 {
        h.step();
    }
    h.settle(SETTLE);
}

fn select_all(h: &mut Headless) {
    let r = h.request("engine.execute", json!({"command": "library.selectAll", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

/// Clicks the picture and then the frame of the second photo's cell (`cell` / `pic` widget prefixes).
fn picture_keeps_frame_replaces(h: &mut Headless, cell: &str, pic: &str) {
    let ids = h.app.session.visible_cloned();
    assert!(ids.len() >= 3, "a few photos");
    let second = ids[1];
    select_all(h);
    let n = h.app.session.selection.ids.len();
    assert_eq!(n, ids.len());
    let picture = widget(h, &format!("{pic}{}", second.0));
    click_at(h, picture.center());
    assert_eq!(h.app.session.selection.ids.len(), n, "a click on the picture keeps the selection");
    assert_eq!(h.app.session.selection.active, Some(second), "and makes that photo the active one");
    let frame = widget(h, &format!("{cell}{}", second.0));
    let picture = widget(h, &format!("{pic}{}", second.0));
    // the frame above the picture (where the file name sits)
    let at = egui::pos2(frame.center().x, (frame.top() + picture.top()) / 2.0);
    assert!(frame.contains(at) && !picture.contains(at), "{at:?} on the frame: {frame:?} / {picture:?}");
    click_at(h, at);
    assert_eq!(h.app.session.selection.ids, vec![second], "a click on the frame selects that photo alone");
    assert_eq!(h.app.session.selection.active, Some(second));
}

#[test]
fn a_click_on_a_grid_cells_frame_selects_that_photo_alone() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.view = ViewMode::SquareGrid;
    h.app.ui.thumb_size = 200.0;
    h.settle(SETTLE);
    picture_keeps_frame_replaces(&mut h, "thumb:", "thumbpic:");
}

#[test]
fn a_click_on_a_filmstrip_cells_frame_selects_that_photo_alone() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    picture_keeps_frame_replaces(&mut h, "film:", "filmpic:");
}
