//! A right-click on a text field while photos are carried (the primary button held) opens no
//! Cut / Copy / Paste menu. What the right button's release does to the drag is not asserted here.

use std::path::PathBuf;

use egui::{Pos2, Rect};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: std::time::Duration = std::time::Duration::from_secs(10);
const FIELD: &str = "field:search";

/// Two photos in a fresh folder: the grid, and the search field of the top bar.
fn app(tag: &str) -> (Headless, PathBuf) {
    use lightcraft_catalog::{Op, Photo, Source};
    let root = std::env::temp_dir().join(format!("lc-carried-field-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    for file in ["beach.png", "garden.png"] {
        let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 160, 255]; 64] };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(root.join(file), png).unwrap();
        let id = session.catalog.alloc_photo_id();
        let p = Photo::new(id, Source::File { path: root.join(file).to_string_lossy().into() }, file, "PNG", 8, 8, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1300.0, 1600.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..8 {
        h.step();
    }
    (h, root)
}

fn widget(h: &Headless, id: &str) -> Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {id}"))
}

fn button(h: &mut Headless, at: Pos2, button: egui::PointerButton, pressed: bool) {
    h.app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE });
    h.step();
}

/// Carry the first photo (primary held) over `field`, then press and release the right button there.
fn right_click_while_carrying(h: &mut Headless, field: &str) {
    let first = h.app.session.visible_cloned()[0].0;
    let from = widget(h, &format!("thumb:{first}")).center();
    let over = widget(h, field).center();
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

#[test]
fn right_click_on_a_text_field_while_carrying_opens_no_menu() {
    let (mut h, root) = app("carry");
    right_click_while_carrying(&mut h, FIELD);
    assert!(!egui::Popup::is_any_open(&h.view.ctx), "{FIELD}: no Cut / Copy / Paste menu while photos are carried");
    let _ = std::fs::remove_dir_all(&root);
}

/// Nothing changes without a drag: the same right-click opens the field's menu.
#[test]
fn right_click_on_a_text_field_still_opens_the_menu_without_a_drag() {
    let (mut h, root) = app("plain");
    let r = h.request("ui.clickWidget", json!({"id": FIELD, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert!(egui::Popup::is_any_open(&h.view.ctx), "{FIELD}: its menu");
    assert!(h.app.widgets.iter().any(|(w, _)| w == &format!("{FIELD}:copy")), "{FIELD}: Copy in its menu");
    let _ = std::fs::remove_dir_all(&root);
}

/// A drag left over without its button held (full screen skips the code that ends it) is not
/// carrying: the field's menu still opens.
#[test]
fn a_drag_left_over_without_its_button_does_not_hold_back_the_menu() {
    let (mut h, root) = app("leftover");
    let first = h.app.session.visible_cloned()[0].0;
    let at = widget(&h, FIELD).center();
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
    assert!(egui::Popup::is_any_open(&h.view.ctx), "the search field's menu opens");
    let _ = std::fs::remove_dir_all(&root);
}
