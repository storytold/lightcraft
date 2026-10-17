//! Headless tests of the thumbnail overlay the grid (justified and square) and the filmstrip share:
//! the virtual-copy tag, the stack badge, the placeholder and the failure mark, and the one way a
//! colour label shows.

use std::{cell::Cell, rc::Rc, time::Duration};

use egui::epaint::Shape;
use egui::{Color32, Rect};
use lightcraft_catalog::{ColorLabel, PhotoId};
use serde_json::json;

use crate::headless::{Headless, RenderGate};
use crate::i18n::Locale;
use crate::render::Slot;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo() -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.view = ViewMode::PhotoGrid;
    Headless::new(app, [1400.0, 900.0], 1.0)
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
}

fn widget(h: &Headless, id: &str) -> Option<Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn first_photos(h: &mut Headless, n: usize) -> Vec<PhotoId> {
    h.app.session.visible_cloned().into_iter().take(n).collect()
}

/// Hover `id` and wait for a tooltip text that satisfies `says`.
fn hover_says(h: &mut Headless, id: &str, says: impl Fn(&str) -> bool) -> bool {
    let r = h.request("ui.hoverWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step_until(Duration::from_secs(5), |h| h.painted_text().iter().any(|t| says(t)))
}

#[test]
fn a_virtual_copy_is_tagged_in_the_filmstrip_as_it_is_in_the_grid() {
    let mut h = demo();
    let master = first_photos(&mut h, 1)[0];
    run(&mut h, "photo.virtualCopy", json!({"ids": [master.0]}));
    let copy = h.app.session.catalog.photos().find(|p| p.copy_name.is_some()).map(|p| p.id).expect("a virtual copy");
    h.settle(SETTLE);
    assert!(widget(&h, &format!("copy:{}", copy.0)).is_some(), "the grid tags the copy");
    assert!(widget(&h, &format!("copy:{}", master.0)).is_none(), "the master is not tagged");
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(widget(&h, &format!("film:copy:{}", copy.0)).is_some(), "the filmstrip tags the copy too");
    assert!(widget(&h, &format!("film:copy:{}", master.0)).is_none(), "and not the master");
}

#[test]
fn the_filmstrip_stack_badge_toggles_its_stack_like_the_grid_s() {
    let mut h = demo();
    let ids = first_photos(&mut h, 2);
    run(&mut h, "stack.group", json!({"ids": [ids[0].0, ids[1].0], "collapsed": false}));
    h.request("ui.set", json!({"view": "detail"}), T);
    h.settle(SETTLE);
    let collapsed = |h: &Headless| h.app.session.catalog.stack_of(ids[0]).map(|s| s.collapsed);
    assert_eq!(collapsed(&h), Some(false));
    let r = h.request("ui.clickWidget", json!({"id": format!("film:stack:{}", ids[0].0)}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(collapsed(&h), Some(true), "a click on the filmstrip's badge collapses the stack");
}

#[test]
fn the_stack_badge_tooltip_is_translated_in_the_grid_and_the_filmstrip() {
    let mut h = demo();
    let ids = first_photos(&mut h, 2);
    run(&mut h, "stack.group", json!({"ids": [ids[0].0, ids[1].0], "collapsed": false}));
    h.settle(SETTLE);
    let r = h.request("ui.set", json!({"language": "de"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let german = Locale::De.tr("Stack — click to collapse (S)");
    let said = |t: &str| t == german;
    assert!(hover_says(&mut h, &format!("stack:{}", ids[0].0), said), "{:?}", h.painted_text());
    h.request("ui.move", json!({"x": 1, "y": 1}), T);
    h.request("ui.set", json!({"view": "detail"}), T);
    h.settle(SETTLE);
    assert!(hover_says(&mut h, &format!("film:stack:{}", ids[0].0), said), "{:?}", h.painted_text());
    crate::i18n::set_language(Locale::En);
}

fn placeholder_in(h: &Headless, cell: Rect) -> bool {
    h.view.painted_shapes().iter().any(|s| matches!(s, Shape::Rect(r) if r.fill == Color32::from_gray(38) && cell.expand(1.0).contains_rect(r.rect)))
}

#[test]
fn a_thumbnail_that_is_not_drawn_yet_has_the_same_placeholder_in_the_filmstrip_and_the_grid() {
    let mut h = demo();
    // renders wait: no thumbnail exists
    h.app.renderer.set_offload(Box::new(RenderGate::new(Rc::new(Cell::new(false)))));
    let id = first_photos(&mut h, 1)[0];
    for _ in 0..10 {
        h.step();
    }
    let grid = widget(&h, &format!("thumb:{}", id.0)).expect("grid cell");
    assert!(placeholder_in(&h, grid), "the grid's placeholder");
    h.request("ui.set", json!({"view": "detail"}), T);
    for _ in 0..10 {
        h.step();
    }
    let film = widget(&h, &format!("film:{}", id.0)).expect("filmstrip cell");
    assert!(placeholder_in(&h, film), "the filmstrip draws the grid's placeholder too");
}

#[test]
fn a_failed_thumbnail_shows_a_mark_that_explains_itself_in_the_grid_and_the_filmstrip() {
    let mut h = demo();
    h.app.renderer.set_offload(Box::new(RenderGate::failing(Rc::new(Cell::new(true)), |s| matches!(s, Slot::Thumb(_)))));
    let id = first_photos(&mut h, 1)[0];
    let (grid_mark, film_mark) = (format!("badge:failed:{}", id.0), format!("film:failed:{}", id.0));
    assert!(h.step_until(SETTLE, |h| widget(h, &grid_mark).is_some()), "the grid's mark is a widget");
    assert!(!h.painted_text().iter().any(|t| t == "!"), "drawn as an icon, not a glyph");
    let says = |t: &str| t.contains("This photo can't be opened") && t.contains("no decoder for this raw") && t.contains("Reload from Disk");
    assert!(hover_says(&mut h, &grid_mark, says), "{:?}", h.painted_text());
    h.request("ui.move", json!({"x": 1, "y": 1}), T);
    h.request("ui.set", json!({"view": "detail"}), T);
    assert!(h.step_until(SETTLE, |h| widget(h, &film_mark).is_some()), "the filmstrip draws the mark");
    assert!(hover_says(&mut h, &film_mark, says), "{:?}", h.painted_text());
}

fn label_dot_in(h: &Headless, cell: Rect) -> bool {
    let red = crate::theme::label_color(ColorLabel::Red);
    h.view.painted_shapes().iter().any(|s| matches!(s, Shape::Circle(c) if c.fill == red && cell.contains(c.center)))
}

/// A band along a cell's lower edge the width of the cell: the footer a label once tinted over
/// the justified grid's photo.
fn footer_band_in(h: &Headless, cell: Rect) -> bool {
    h.view.painted_shapes().iter().any(|s| {
        matches!(s, Shape::Rect(r) if (r.rect.width() - cell.width()).abs() < 0.5 && (r.rect.height() - 24.0).abs() < 0.5 && (r.rect.bottom() - cell.bottom()).abs() < 0.5 && r.fill.a() > 0 && r.fill != Color32::from_black_alpha(120))
    })
}

#[test]
fn a_label_is_the_dot_in_the_marks_row_on_every_surface_and_no_footer_covers_the_photo() {
    let mut h = demo();
    let id = first_photos(&mut h, 1)[0];
    run(&mut h, "photo.label", json!({"ids": [id.0], "label": "red"}));
    // not selected, not hovered: the footer would show on its own
    let other = first_photos(&mut h, 2)[1];
    run(&mut h, "library.select", json!({"ids": [other.0]}));
    h.step();
    for (view, cell) in [(ViewMode::PhotoGrid, "thumb"), (ViewMode::SquareGrid, "thumb"), (ViewMode::Detail, "film")] {
        h.app.ui.view = view;
        h.settle(SETTLE);
        let r = widget(&h, &format!("{cell}:{}", id.0)).unwrap_or_else(|| panic!("{view:?}: the photo's cell"));
        assert!(label_dot_in(&h, r), "{view:?}: the label dot");
        assert!(!footer_band_in(&h, r), "{view:?}: no tinted footer over the photo");
    }
}

#[test]
fn with_ratings_and_flags_off_the_justified_grid_still_shows_the_label() {
    let mut h = demo();
    let ids = first_photos(&mut h, 2);
    run(&mut h, "photo.label", json!({"ids": [ids[0].0], "label": "red"}));
    run(&mut h, "library.select", json!({"ids": [ids[1].0]}));
    h.app.ui.settings.grid_badges = crate::state::GridBadges::Never;
    h.settle(SETTLE);
    let cell = widget(&h, &format!("thumb:{}", ids[0].0)).expect("the photo's cell");
    assert!(label_dot_in(&h, cell), "the label is not a rating or a flag: it stays, the grid has no surround to tint");
}

#[test]
fn right_clicking_the_filmstrips_stack_badge_opens_the_photo_menu() {
    let mut h = demo();
    let ids = first_photos(&mut h, 2);
    run(&mut h, "stack.group", json!({"ids": [ids[0].0, ids[1].0], "collapsed": false}));
    h.request("ui.set", json!({"view": "detail"}), T);
    h.settle(SETTLE);
    assert!(!egui::Popup::is_any_open(&h.view.ctx));
    let r = h.request("ui.clickWidget", json!({"id": format!("film:stack:{}", ids[0].0), "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(egui::Popup::is_any_open(&h.view.ctx), "the photo's menu, as on the rest of the cell");
}

#[test]
fn right_clicking_the_grids_stack_badge_opens_the_photo_menu() {
    let mut h = demo();
    let ids = first_photos(&mut h, 2);
    run(&mut h, "stack.group", json!({"ids": [ids[0].0, ids[1].0], "collapsed": false}));
    h.settle(SETTLE);
    assert!(!egui::Popup::is_any_open(&h.view.ctx));
    let r = h.request("ui.clickWidget", json!({"id": format!("stack:{}", ids[0].0), "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(egui::Popup::is_any_open(&h.view.ctx), "the photo's menu, as on the rest of the cell");
}
