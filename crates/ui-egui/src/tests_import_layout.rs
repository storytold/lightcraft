//! The import review's layout on short and narrow screens (iPad, iPhone).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const SETTLE: Duration = Duration::from_secs(20);

fn folder_of_photos(name: &str, n: usize) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-import-layout-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(48, 32, |x, y| [(x * 5) as u8, (y * 7) as u8, 90, 255]);
    let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
    let png = lightcraft_engine::export::encode_image(&img, &o).unwrap();
    for i in 0..n {
        std::fs::write(dir.join(format!("p{i:02}.png")), &png).unwrap();
    }
    dir
}

fn review(size: [f32; 2], name: &str) -> (Headless, std::path::PathBuf) {
    let services = Services {
        png: Some(Box::new(|img: &lightcraft_raster::Rgba8| {
            lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
        })),
        write: Some(Box::new(lightcraft_engine::export::write_file)),
        import_copies: true,
        ..Default::default()
    };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    let dir = folder_of_photos(name, 30);
    let r = h.request("engine.execute", json!({"command": "file.addFolder", "params": {"path": dir.to_string_lossy()}}), SETTLE);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h.step();
    h.step();
    (h, dir)
}

fn rect(h: &mut Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

const WIDE: [(&str, [f32; 2]); 4] =
    [("ipad-landscape", [1194.0, 834.0]), ("ipad-portrait", [834.0, 1194.0]), ("iphone-landscape", [852.0, 393.0]), ("desktop", [1440.0, 900.0])];

/// Where there is the width, the options sit in a column beside the photos, which take the
/// dialog's whole height; the buttons stay at the bottom of the options, on screen.
#[test]
fn wide_screens_put_the_options_beside_the_photos() {
    for (name, size) in WIDE {
        let (mut h, dir) = review(size, name);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]));
        let window = rect(&mut h, "dialog:window").unwrap_or_else(|| panic!("{name}: no dialog"));
        assert!(screen.contains_rect(window), "{name}: the dialog fits the screen: {window:?}");
        let grid = rect(&mut h, "grid:importPhotos").unwrap_or_else(|| panic!("{name}: no photo grid"));
        let options = rect(&mut h, "panel:importOptions").unwrap_or_else(|| panic!("{name}: no options column"));
        assert!(options.left() >= grid.right(), "{name}: options beside the photos: {grid:?} {options:?}");
        assert!(grid.height() >= 0.6 * window.height(), "{name}: the photos take the height: {grid:?} in {window:?}");
        for id in ["button:dialogOk", "button:dialogCancel"] {
            let b = rect(&mut h, id).unwrap_or_else(|| panic!("{name}: no {id}"));
            assert!(window.contains_rect(b) && options.left() <= b.left(), "{name}: {id} in the options column: {b:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A phone held upright has no room beside the photos: photos and options are two tabs, and
/// the buttons stay at the bottom of both.
#[test]
fn a_narrow_screen_shows_photos_and_options_as_tabs() {
    let size = [393.0, 852.0];
    let (mut h, dir) = review(size, "iphone-portrait");
    let window = rect(&mut h, "dialog:window").unwrap();
    let grid = rect(&mut h, "grid:importPhotos").expect("the photos first");
    assert!(grid.height() >= 0.6 * window.height(), "the photos take the height: {grid:?} in {window:?}");
    assert!(rect(&mut h, "field:importKeywords").is_none(), "the options are on their own tab");
    let ok = rect(&mut h, "button:dialogOk").expect("Import on the photos tab");
    assert!(window.contains_rect(ok));
    let r = h.request("ui.clickWidget", json!({"id": "button:importTab-options"}), SETTLE);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(rect(&mut h, "grid:importPhotos").is_none(), "the options replace the photos");
    assert!(rect(&mut h, "panel:importOptions").is_some());
    assert!(window.contains_rect(rect(&mut h, "button:dialogOk").expect("Import on the options tab")));
    let r = h.request("ui.clickWidget", json!({"id": "button:importTab-photos"}), SETTLE);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(rect(&mut h, "grid:importPhotos").is_some());
    let _ = std::fs::remove_dir_all(dir);
}

/// With `LC_SHOTS=<dir>`: the review at phone, tablet and desktop sizes as PNGs, to look at.
#[test]
fn screenshots_when_asked() {
    let out = std::env::var("LC_SHOTS").unwrap_or_default();
    if out.is_empty() {
        return;
    }
    for (name, size) in [
        ("ipad-landscape", [1194.0, 834.0]),
        ("ipad-portrait", [834.0, 1194.0]),
        ("iphone-portrait", [393.0, 852.0]),
        ("iphone-landscape", [852.0, 393.0]),
        ("desktop", [1440.0, 900.0]),
    ] {
        let (mut h, dir) = review(size, name);
        let r = h.request("ui.screenshot", json!({"path": format!("{out}/{name}.png"), "headless": true}), SETTLE);
        assert_eq!(r["ok"], true, "{r}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
