//! Rendered regressions: label colour belongs on thumbnail chrome and the label HUD, not pixels.

use std::time::Duration;

use egui::{Color32, ColorImage, Pos2};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const SETTLE: Duration = Duration::from_secs(120);

fn app(view: ViewMode) -> Headless {
    let mut s = lightcraft_engine::Session::new();
    for id in 1..=6 {
        let p = Photo::new(PhotoId(id), Source::Demo { scene: 1 }, &format!("{id}.jpg"), "JPEG", 640, 480, "2026-10-08");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true, "group": "none"})).unwrap();
    s.execute("library.select", &json!({"ids": [1]})).unwrap();
    let mut h = Headless::new(LightcraftApp::new(s, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.app.ui.view = view;
    h.app.ui.right = crate::state::RightPanel::None;
    h.app.ui.thumb_size = 160.0;
    // Parallel UI tests can leave a quiet spell before thumbnail jobs complete. Require the
    // fixture's real textures before comparing photo pixels, as well as settled layout.
    assert!(h.step_until(SETTLE, |h| h.app.renderer.thumb_textures() == 6), "all fixture thumbnails must be ready");
    assert!(h.settle(SETTLE), "label fixture did not settle");
    h
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).unwrap().1
}

fn pixel(image: &ColorImage, p: Pos2) -> Color32 {
    image.pixels[p.y as usize * image.size[0] + p.x as usize]
}

#[test]
fn label_tints_grid_and_filmstrip_chrome_without_changing_photo_pixels() {
    for view in [ViewMode::SquareGrid, ViewMode::PhotoGrid, ViewMode::Detail] {
        let mut h = app(view);
        let widget = if view == ViewMode::Detail { "film:1" } else { "thumb:1" };
        let cell = rect(&h, widget);
        let sample = if view == ViewMode::PhotoGrid {
            egui::pos2(cell.right() - 40.0, cell.bottom() - 8.0)
        } else {
            egui::pos2(cell.left() + 4.0, cell.center().y)
        };
        let neutral = h.paint();
        let centre = pixel(&neutral, cell.center());
        let mut colours = Vec::new();
        for label in ["red", "yellow", "green", "blue", "purple"] {
            h.app.run("photo.label", json!({"label": label})).unwrap();
            h.app.ui.toast = None;
            h.step();
            let image = h.paint();
            let c = pixel(&image, sample);
            assert_ne!(c, pixel(&neutral, sample), "{view:?} {label} must tint the thumbnail surround/footer");
            assert!(!colours.contains(&c), "each label has a distinct tint: {view:?} {label}");
            colours.push(c);
            assert_eq!(pixel(&image, cell.center()), centre, "{view:?} {label} must not tint image pixels");
        }
        h.app.run("photo.label", json!({"label": "none"})).unwrap();
        h.app.ui.toast = None;
        h.step();
        assert_eq!(pixel(&h.paint(), sample), pixel(&neutral, sample), "clearing restores neutral chrome");
    }
}

#[test]
fn label_toast_has_pale_colour_and_rating_restores_neutral_style() {
    let mut h = app(ViewMode::SquareGrid);
    h.app.run("photo.label", json!({"label": "red"})).unwrap();
    h.step();
    let at = egui::pos2(h.app.canvas_rect.unwrap().center().x, h.app.canvas_rect.unwrap().bottom() - 45.0);
    let red = pixel(&h.paint(), at);
    assert!(red.r() > 180 && red.r() > red.g() && red.r() > red.b(), "pale red toast: {red:?}");
    h.request("ui.key", json!({"key": "5"}), Duration::from_secs(20));
    let rating = pixel(&h.paint(), at);
    assert!(rating.r() < 80 && rating.r() == rating.g() && rating.g() == rating.b(), "neutral rating toast: {rating:?}");
}
