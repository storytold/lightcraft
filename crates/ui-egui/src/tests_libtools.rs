//! Headless tests of the Library tools: the Painter, the Metadata panel (presets, editing many),
//! grid cell styles and badges.

use std::time::Duration;

use lcc_catalog::PhotoId;
use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn app(view: &str) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
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

/// Issue #281: Cmd/Ctrl+A in a Metadata field selects the field's text, not every photo, and the
/// edit is saved to the photo it started on — even when the selection changes before it ends.
#[test]
fn select_all_in_a_metadata_field_selects_the_text_not_the_photos() {
    let mut h = app("photoGrid");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    exec(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "title": "One"}));
    exec(&mut h, "photo.setMeta", json!({"ids": [ids[1]], "title": "Two"}));
    exec(&mut h, "photo.setMeta", json!({"ids": [ids[2]], "title": "Three"}));
    exec(&mut h, "library.select", json!({"ids": [ids[0]], "active": ids[0]}));
    exec(&mut h, "panel.info", json!({}));
    exec(&mut h, "metadata.panelPreset", json!({"preset": "default"}));
    h.settle(SETTLE);
    let title = |h: &Headless, i: u64| h.app.session.catalog.photo(PhotoId(i)).unwrap().meta.title.clone();
    assert_eq!(h.request("ui.clickWidget", json!({"id": "field:title"}), T)["ok"], true);
    // the shortcut (egui's Cmd modifier: ⌘ on macOS, Ctrl elsewhere)
    assert_eq!(h.request("ui.key", json!({"key": "a", "cmd": true}), T)["ok"], true);
    assert_eq!(h.app.session.selected(), vec![PhotoId(ids[0])], "the photo selection stays");
    // the macOS menu bar's Select All item reached by ⌘A goes to the field, not the command
    let ctx = h.view.ctx.clone();
    assert!(crate::menubar::forward_to_text_field(&mut h.app, &ctx, Some("Cmd+A")));
    assert!(!crate::menubar::forward_to_text_field(&mut h.app, &ctx, Some("Cmd+Shift+E")), "other shortcuts run their command");
    h.settle(SETTLE);
    assert_eq!(h.app.session.selected(), vec![PhotoId(ids[0])], "the photo selection stays");
    // the whole text is selected: typing replaces it
    h.request("ui.text", json!({"text": "Edited"}), T);
    // the selection changes while the field is still being edited (e.g. the native menu's Select
    // All): the edit still saves to the photo it began on only
    exec(&mut h, "library.selectAll", json!({}));
    h.request("ui.key", json!({"key": "enter"}), T);
    h.settle(SETTLE);
    assert_eq!(title(&h, ids[0]), "Edited");
    assert_eq!((title(&h, ids[1]), title(&h, ids[2])), ("Two".into(), "Three".into()), "other photos keep their titles");
}

/// Issue #266: with filters on, the loupe's filter pill and the info overlay share the top-left
/// corner without overlapping.
#[test]
fn the_filter_pill_and_the_info_overlay_do_not_overlap() {
    let mut h = app("detail");
    let first = h.app.session.visible_cloned()[0];
    exec(&mut h, "photo.rate", json!({"ids": [first.0], "rating": 5}));
    exec(&mut h, "library.select", json!({"ids": [first.0], "active": first.0}));
    exec(&mut h, "library.filter", json!({"rating": 4}));
    for overlay in [crate::state::InfoOverlay::Basic, crate::state::InfoOverlay::Exposure] {
        h.app.ui.info_overlay = overlay;
        h.settle(SETTLE);
        let rect = |h: &Headless, w: &str| h.app.widgets.iter().find(|(id, _)| id == w).map(|(_, r)| *r).unwrap_or_else(|| panic!("{w} drawn"));
        let (pill, info) = (rect(&h, "loupe:filters"), rect(&h, "canvas:infoOverlay"));
        assert!(!pill.intersects(info), "{overlay:?}: pill {pill:?} over the info overlay {info:?}");
    }
}

/// Feature: grid cell styles and index numbers are commands; the expanded square cell shows the
/// index; unknown styles are refused.
fn grid_cell_style_and_index_numbers(h: &mut Headless) {
    h.app.ui.dialog = None;
    h.step();
    let r = exec(h, "view.gridCellStyle", json!({"style": "expanded", "index": true}));
    assert_eq!(r, json!({"style": "expanded", "index": true, "badges": true}));
    let r = exec(h, "view.gridCellStyle", json!({}));
    assert_eq!(r["style"], "compact", "cycles");
    exec(h, "view.gridCellStyle", json!({"style": "expanded"}));
    h.settle(SETTLE);
    let first = h.app.session.visible_cloned()[0];
    center(h, &format!("cellIndex:{}", first.0));
    let r = h.request("engine.execute", json!({"command": "view.gridCellStyle", "params": {"style": "huge"}}), T);
    assert_eq!(r["ok"], false, "{r}");
}

/// Feature: thumbnail badges for keywords and collections; the badges setting hides them.
fn grid_badges_for_keywords_and_collections(h: &mut Headless) {
    let first = h.app.session.visible_cloned()[0];
    exec(h, "photo.setMeta", json!({"ids": [first.0], "keywords": ["sea"]}));
    exec(h, "album.toggleTarget", json!({"ids": [first.0]}));
    h.settle(SETTLE);
    center(h, &format!("badge:keywords:{}", first.0));
    center(h, &format!("badge:collections:{}", first.0));
    exec(h, "view.gridCellStyle", json!({"badges": false}));
    h.step();
    h.step();
    let w = h.request("ui.widgets", json!({}), T);
    assert!(!w.to_string().contains(&format!("badge:keywords:{}", first.0)));
}

#[test]
fn grid_cells_show_badges_styles_and_index_numbers() {
    let mut h = app("squareGrid");
    grid_badges_for_crop_and_rotation_sit_on_the_photo(&mut h);
    grid_cell_style_and_index_numbers(&mut h);
    grid_badges_for_keywords_and_collections(&mut h);
}

/// Feature: cropped and rotated badges, placed on the photo (inside a portrait photo's width,
/// not the square around it); the View Options dialog and the View ▸ Grid View Style commands.
fn grid_badges_for_crop_and_rotation_sit_on_the_photo(h: &mut Headless) {
    let first = h.app.session.visible_cloned()[0];
    exec(h, "library.select", json!({"ids": [first.0]}));
    exec(h, "crop.set", json!({"rect": [0.1, 0.1, 0.9, 0.9]}));
    exec(h, "photo.rotateRight", json!({"ids": [first.0]}));
    h.settle(SETTLE);
    center(h, &format!("badge:rotated:{}", first.0));
    center(h, &format!("badge:cropped:{}", first.0));
    // View menu entries and the dialog
    exec(h, "view.cellExpanded", json!({}));
    assert_eq!(h.app.ui.lib.cell_style, crate::libtools::CellStyle::Expanded);
    exec(h, "view.cellIndex", json!({}));
    assert!(h.app.ui.lib.cell_index);
    exec(h, "view.cellBadges", json!({}));
    assert!(!h.app.ui.lib.cell_badges);
    exec(h, "dialog.viewOptions", json!({}));
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
    let mut d = lcc_develop::DevelopSettings::default();
    assert!(!crate::panels::cells::cropped(&d));
    d.crop.geometry.angle = 2.0;
    assert!(crate::panels::cells::cropped(&d));
}

/// G19: a plug-in's custom metadata field edits every selected photo too: differing values read
/// `< mixed >` and stay as they are unless typed into; typing sets them all (and the catalog's copy
/// the Library filter searches).
#[test]
fn plugin_fields_edit_every_selected_photo() {
    // install into memory, never into the real user's settings folder
    lcc_engine::cmd::plugins::use_in_memory_manager();
    let manifest = r#"{"id":"test.uifields","name":"Jobs","version":"1.0.0","metadataFields":[{"key":"job","label":"Job","type":"text"}]}"#;
    let reply = r#"{"ok":{}}"#;
    let esc = |s: &str| s.replace('"', "\\\"");
    let src = format!(
        r#"(module (memory (export "memory") 2)
  (data (i32.const 16) "{m}") (data (i32.const 8192) "{r}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {ml}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 65536))
  (func (export "lcc_handle") (param i32 i32) (result i64) (i64.or (i64.shl (i64.const {rl}) (i64.const 32)) (i64.const 8192))))"#,
        m = esc(manifest),
        r = esc(reply),
        ml = manifest.len(),
        rl = reply.len()
    );
    let dir = lcc_testkit::TempDir::new("plugin-fields-ui");
    std::fs::create_dir_all(&dir).unwrap();
    let wasm = dir.join("fields.wasm");
    std::fs::write(&wasm, wat::parse_str(&src).unwrap()).unwrap();
    let mut h = app("detail");
    exec(&mut h, "plugin.install", json!({"path": wasm.display().to_string()}));
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    exec(&mut h, "plugin.setField", json!({"plugin": "test.uifields", "field": "job", "value": "A", "id": ids[0]}));
    exec(&mut h, "plugin.setField", json!({"plugin": "test.uifields", "field": "job", "value": "B", "id": ids[1]}));
    exec(&mut h, "library.select", json!({"ids": ids, "active": ids[0]}));
    exec(&mut h, "panel.info", json!({}));
    exec(&mut h, "metadata.panelPreset", json!({"preset": "minimal"}));
    h.settle(SETTLE);
    let job = |h: &Headless, i: u64| h.app.session.catalog.photo(PhotoId(i)).unwrap().plugin_fields.get("test.uifields/job").cloned();
    let widget = "pluginField:test.uifields:job";
    assert!(h.painted_text().iter().any(|t| t.contains(crate::panels::metadata::MIXED)), "the field reads mixed");
    // focus and leave the mixed field without typing: nothing changes
    assert_eq!(h.request("ui.clickWidget", json!({"id": widget}), T)["ok"], true);
    h.request("ui.key", json!({"key": "enter"}), T);
    assert_eq!((job(&h, ids[0]), job(&h, ids[1])), (Some(json!("A")), Some(json!("B"))));
    // typing sets both
    assert_eq!(h.request("ui.clickWidget", json!({"id": widget}), T)["ok"], true);
    h.request("ui.text", json!({"text": "C"}), T);
    h.request("ui.key", json!({"key": "enter"}), T);
    assert_eq!((job(&h, ids[0]), job(&h, ids[1])), (Some(json!("C")), Some(json!("C"))));
    exec(&mut h, "plugin.uninstall", json!({"id": "test.uifields"}));
    let _ = std::fs::remove_dir_all(&dir);
}
