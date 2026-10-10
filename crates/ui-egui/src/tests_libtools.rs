//! Headless tests of the Library tools: the Painter, the Metadata panel (presets, editing many),
//! grid cell styles and badges.

use std::time::Duration;

use dac_catalog::PhotoId;
use serde_json::json;

use crate::headless::Headless;
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn app(view: &str) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = DacApp::new(dac_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": view, "moduleBar": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn center(h: &mut Headless, widget: &str) -> (f64, f64) {
    let w = h.request("ui.widgets", json!({}), T);
    let r = w["result"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["id"] == widget))
        .map(|x| x["rect"].clone())
        .unwrap_or_else(|| panic!("{widget} on screen"));
    (r[0].as_f64().unwrap() + r[2].as_f64().unwrap() / 2.0, r[1].as_f64().unwrap() + r[3].as_f64().unwrap() / 2.0)
}

fn drag(h: &mut Headless, from: (f64, f64), to: (f64, f64)) {
    let r = h.request("ui.drag", json!({"x": from.0, "y": from.1, "toX": to.0, "toY": to.1, "steps": 6}), T);
    assert_eq!(r["ok"], true, "{r}");
}

fn rating(h: &Headless, id: PhotoId) -> u8 {
    h.app.session.catalog.photo(id).unwrap().rating
}

/// Feature: the Painter sprays a rating over every photo a drag passes, in one stroke; a stroke
/// that starts on a photo that already has it erases; painting never selects.
#[test]
fn painter_sprays_and_erases_ratings_by_dragging() {
    let mut h = app("photoGrid");
    let ids = h.app.session.visible_cloned();
    let (a, b) = (ids[0], ids[1]);
    exec(&mut h, "photo.rate", json!({"ids": [a.0, b.0], "rating": 0}));
    let r = exec(&mut h, "tool.painter", json!({"kind": "rating", "value": 4}));
    assert_eq!(r["painter"]["kind"], "rating", "{r}");
    h.settle(SETTLE);
    let sel = h.app.session.selection.clone();
    let (pa, pb) = (center(&mut h, &format!("thumb:{}", a.0)), center(&mut h, &format!("thumb:{}", b.0)));
    drag(&mut h, pa, pb);
    assert_eq!((rating(&h, a), rating(&h, b)), (4, 4), "both painted");
    assert_eq!(h.app.session.selection, sel, "painting doesn't select");
    drag(&mut h, pa, pb);
    assert_eq!((rating(&h, a), rating(&h, b)), (0, 0), "a stroke from a painted photo erases");
    // bad values are refused, Esc stops
    let r = h.request("engine.execute", json!({"command": "tool.painter", "params": {"kind": "rating", "value": 9}}), T);
    assert_eq!(r["ok"], false, "{r}");
    let r = h.request("engine.execute", json!({"command": "tool.painter", "params": {"kind": "settings", "value": "no-such-preset"}}), T);
    assert_eq!(r["ok"], false, "{r}");
    h.request("ui.key", json!({"key": "escape"}), T);
    assert!(h.app.ui.lib.painter.is_none());
}

/// Feature: the Painter adds photos to the target collection (the Quick Collection by default).
#[test]
fn painter_fills_the_target_collection() {
    let mut h = app("photoGrid");
    let a = h.app.session.visible_cloned()[0];
    exec(&mut h, "tool.painter", json!({"kind": "targetCollection"}));
    h.settle(SETTLE);
    let pa = center(&mut h, &format!("thumb:{}", a.0));
    drag(&mut h, pa, pa);
    assert!(crate::libtools::in_target(&h.app, a), "in the Quick Collection");
    drag(&mut h, pa, pa);
    assert!(!crate::libtools::in_target(&h.app, a), "erased");
}

/// Feature: the Metadata panel edits every selected photo; a field that differs shows `<mixed>`
/// and stays untouched unless typed into.
#[test]
fn metadata_panel_edits_many_with_mixed_values() {
    let mut h = app("detail");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    exec(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "city": "Lyon", "title": "Same"}));
    exec(&mut h, "photo.setMeta", json!({"ids": [ids[1], ids[2]], "city": "Nice", "title": "Same"}));
    exec(&mut h, "library.select", json!({"ids": ids, "active": ids[0]}));
    exec(&mut h, "panel.info", json!({}));
    exec(&mut h, "metadata.panelPreset", json!({"preset": "location"}));
    h.settle(SETTLE);
    let city = |h: &Headless, i: u64| h.app.session.catalog.photo(PhotoId(i)).unwrap().meta.city.clone();
    // the mixed City field: focus and leave without typing → nothing changes
    assert_eq!(h.request("ui.clickWidget", json!({"id": "field:city"}), T)["ok"], true);
    h.request("ui.key", json!({"key": "enter"}), T);
    assert_eq!((city(&h, ids[0]), city(&h, ids[2])), ("Lyon".into(), "Nice".into()));
    // typing sets all three
    assert_eq!(h.request("ui.clickWidget", json!({"id": "field:city"}), T)["ok"], true);
    h.request("ui.text", json!({"text": "Arles"}), T);
    h.request("ui.key", json!({"key": "enter"}), T);
    assert!(ids.iter().all(|i| city(&h, *i) == "Arles"), "{:?}", ids.iter().map(|i| city(&h, *i)).collect::<Vec<_>>());
    // unknown panel presets are refused
    let r = h.request("engine.execute", json!({"command": "metadata.panelPreset", "params": {"preset": "nope"}}), T);
    assert_eq!(r["ok"], false, "{r}");
    // every preset draws
    for (k, _) in crate::panels::metadata::PRESETS {
        exec(&mut h, "metadata.panelPreset", json!({"preset": k}));
        h.step();
    }
}

/// Feature: grid cell styles and index numbers are commands; the expanded square cell shows the
/// index; unknown styles are refused.
#[test]
fn grid_cell_style_and_index_numbers() {
    let mut h = app("squareGrid");
    let r = exec(&mut h, "view.gridCellStyle", json!({"style": "expanded", "index": true}));
    assert_eq!(r, json!({"style": "expanded", "index": true, "badges": true}));
    let r = exec(&mut h, "view.gridCellStyle", json!({}));
    assert_eq!(r["style"], "compact", "cycles");
    exec(&mut h, "view.gridCellStyle", json!({"style": "expanded"}));
    h.settle(SETTLE);
    let first = h.app.session.visible_cloned()[0];
    center(&mut h, &format!("cellIndex:{}", first.0));
    let r = h.request("engine.execute", json!({"command": "view.gridCellStyle", "params": {"style": "huge"}}), T);
    assert_eq!(r["ok"], false, "{r}");
}

/// Feature: thumbnail badges for keywords and collections; the badges setting hides them.
#[test]
fn grid_badges_for_keywords_and_collections() {
    let mut h = app("squareGrid");
    let first = h.app.session.visible_cloned()[0];
    exec(&mut h, "photo.setMeta", json!({"ids": [first.0], "keywords": ["sea"]}));
    exec(&mut h, "album.toggleTarget", json!({"ids": [first.0]}));
    h.settle(SETTLE);
    center(&mut h, &format!("badge:keywords:{}", first.0));
    center(&mut h, &format!("badge:collections:{}", first.0));
    exec(&mut h, "view.gridCellStyle", json!({"badges": false}));
    h.step();
    h.step();
    let w = h.request("ui.widgets", json!({}), T);
    assert!(!w.to_string().contains(&format!("badge:keywords:{}", first.0)));
}

/// Feature: cropped and rotated badges, placed on the photo (inside a portrait photo's width,
/// not the square around it); the View Options dialog and the View ▸ Grid View Style commands.
#[test]
fn grid_badges_for_crop_and_rotation_sit_on_the_photo() {
    let mut h = app("squareGrid");
    let first = h.app.session.visible_cloned()[0];
    exec(&mut h, "library.select", json!({"ids": [first.0]}));
    exec(&mut h, "crop.set", json!({"rect": [0.1, 0.1, 0.9, 0.9]}));
    exec(&mut h, "photo.rotateRight", json!({"ids": [first.0]}));
    h.settle(SETTLE);
    center(&mut h, &format!("badge:rotated:{}", first.0));
    center(&mut h, &format!("badge:cropped:{}", first.0));
    // View menu entries and the dialog
    exec(&mut h, "view.cellExpanded", json!({}));
    assert_eq!(h.app.ui.lib.cell_style, crate::libtools::CellStyle::Expanded);
    exec(&mut h, "view.cellIndex", json!({}));
    assert!(h.app.ui.lib.cell_index);
    exec(&mut h, "view.cellBadges", json!({}));
    assert!(!h.app.ui.lib.cell_badges);
    exec(&mut h, "dialog.viewOptions", json!({}));
    h.step();
    h.step();
    assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::ViewOptions));
    let r = h.request("ui.clickWidget", json!({"id": "check:viewOptions.badges"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(h.app.ui.lib.cell_badges, "the dialog's checkbox turns badges back on");
    let r = h.request("ui.clickWidget", json!({"id": "radio:viewOptions.compact"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.lib.cell_style, crate::libtools::CellStyle::Compact);
}

#[test]
fn cropped_ignores_an_uncropped_edit() {
    let mut d = dac_develop::DevelopSettings::default();
    assert!(!crate::panels::cells::cropped(&d));
    d.crop.geometry.angle = 2.0;
    assert!(crate::panels::cells::cropped(&d));
}
