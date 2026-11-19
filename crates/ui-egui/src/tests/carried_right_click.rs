//! A right-click while photos are carried (the primary button held) opens no context menu on a
//! sidebar row. What the right button's release does to the drag itself is not asserted here.

use std::path::PathBuf;

use egui::{Pos2, Rect};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: std::time::Duration = std::time::Duration::from_secs(10);

/// Two photos in a fresh folder, an album and a keyword: a grid with Albums, Folders and Keywords
/// in the sidebar, tall enough for all three.
fn app(tag: &str) -> (Headless, PathBuf, [String; 3]) {
    use lightcraft_catalog::{Op, Photo, Source};
    let root = std::env::temp_dir().join(format!("lc-drag-right-click-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    let mut ids = Vec::new();
    for file in ["beach.png", "garden.png"] {
        let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 160, 255]; 64] };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(root.join(file), png).unwrap();
        let id = session.catalog.alloc_photo_id();
        let p = Photo::new(id, Source::File { path: root.join(file).to_string_lossy().into() }, file, "PNG", 8, 8, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id.0);
    }
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1300.0, 1600.0], 1.0);
    let r = h.request("engine.execute", json!({"command": "album.create", "params": {"name": "Dropped"}}), T);
    let album = r["result"]["id"].as_u64().unwrap_or_else(|| panic!("{r}"));
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [ids[1]]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [ids[1]], "addKeywords": ["Travel"]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.run("library.select", json!({"ids": []})).unwrap();
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true, "sidebar": ["albums", "folders", "keywords"]}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..8 {
        h.step();
    }
    let folder = format!("source:libfolder:{}", root.to_string_lossy().replace('\\', "/"));
    (h, root, [format!("source:album:{album}"), folder, "source:keyword:Travel".into()])
}

fn widget(h: &Headless, id: &str) -> Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {id}"))
}

fn button(h: &mut Headless, at: Pos2, button: egui::PointerButton, pressed: bool) {
    h.app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE });
    h.step();
}

/// Carry the first photo (primary held) over `row`, then right-click there and let go of the
/// right button, one frame at a time with the pointer resting in between, as a person does.
fn right_click_while_carrying(h: &mut Headless, row: &str) {
    let first = h.app.session.visible_cloned()[0].0;
    let from = widget(h, &format!("thumb:{first}")).center();
    let over = widget(h, row).center();
    h.app.synthetic.push(egui::Event::PointerMoved(from));
    button(h, from, egui::PointerButton::Primary, true);
    for i in 1..=8 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (over - from) * (i as f32 / 8.0)));
        h.step();
    }
    h.step();
    assert_eq!(h.app.ui.dragging_photos, Some(vec![first]), "carrying the photo");
    button(h, over, egui::PointerButton::Secondary, true);
    h.step();
    button(h, over, egui::PointerButton::Secondary, false);
    h.step();
    h.step();
}

fn check(row: usize, tag: &str) {
    let (mut h, root, rows) = app(tag);
    let id = &rows[row];
    right_click_while_carrying(&mut h, id);
    assert!(!egui::Popup::is_any_open(&h.view.ctx), "{id}: no context menu opens while photos are carried");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn right_click_on_an_album_row_while_carrying_opens_no_menu() {
    check(0, "album");
}

#[test]
fn right_click_on_a_folder_row_while_carrying_opens_no_menu() {
    check(1, "folder");
}

#[test]
fn right_click_on_a_keyword_row_while_carrying_opens_no_menu() {
    check(2, "keyword");
}

/// Nothing changes without a drag: the same right-click opens the row's menu.
#[test]
fn right_click_on_rows_still_opens_the_menu_without_a_drag() {
    for (row, tag) in [(0, "a"), (1, "f"), (2, "k")] {
        let (mut h, root, rows) = app(&format!("plain-{tag}"));
        let r = h.request("ui.clickWidget", json!({"id": rows[row], "button": "right"}), T);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..4 {
            h.step();
        }
        assert!(egui::Popup::is_any_open(&h.view.ctx), "{}: its context menu", rows[row]);
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A drag left over without its button held (full screen skips the code that ends it) is not
/// carrying: the row's menu still opens.
#[test]
fn a_drag_left_over_without_its_button_does_not_hold_back_the_menu() {
    let (mut h, root, rows) = app("leftover");
    let first = h.app.session.visible_cloned()[0].0;
    // the right-click is the first thing the frame sees, before anything ends the leftover drag
    let at = widget(&h, &rows[0]).center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    h.step();
    h.app.ui.dragging_photos = Some(vec![first]);
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.step();
    h.step();
    assert!(egui::Popup::is_any_open(&h.view.ctx), "the album row's menu opens");
    let _ = std::fs::remove_dir_all(&root);
}
