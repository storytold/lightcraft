//! Headless tests of View ▸ Loupe Overlay ▸ Layout Image: a PNG with transparency drawn over the
//! photo in the loupe, fitted and centred, its opaque parts covering the photo and its transparent
//! parts leaving it alone; opacity, matte, ⌥⌘O, a file that can't be read, and the choice saved
//! with the UI state.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The graphic's opaque band: magenta, unlike anything in the demo photos.
const BAND: [u8; 4] = [255, 0, 255, 255];

fn loupe() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

/// A portrait "cover": an opaque magenta masthead across its top quarter, transparent below.
fn cover(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-layout-image-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(300, 400, |_, y| if y < 100 { BAND } else { [0, 0, 0, 0] });
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let path = dir.join("cover.png");
    std::fs::write(&path, png).unwrap();
    path
}

fn widget(h: &Headless, name: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == name).map(|(_, r)| *r)
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    h.step();
    h.step();
    r
}

fn px(img: &egui::ColorImage, p: egui::Pos2) -> [u8; 3] {
    let c = img.pixels[(p.y as usize) * img.size[0] + p.x as usize];
    [c.r(), c.g(), c.b()]
}

fn close(a: [u8; 3], b: [u8; 3], tol: i32) -> bool {
    a.iter().zip(b).all(|(x, y)| (*x as i32 - y as i32).abs() <= tol)
}

#[test]
fn the_layout_image_covers_the_photo_where_opaque_and_follows_opacity_matte_and_the_toggle() {
    let mut h = loupe();
    let path = cover("draw");
    let photo = widget(&h, "canvas:image").expect("the loupe shows a photo");
    let before = h.snapshot(SETTLE);

    let r = run(&mut h, "view.chooseLayoutImage", json!({"path": path.to_string_lossy()}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.layout_image.show, "choosing a layout image shows it");
    assert!(h.step_until(T, |h| widget(h, "canvas:layoutImage").is_some()), "the layout image is drawn once read");
    let shown = h.snapshot(SETTLE);
    let r = widget(&h, "canvas:layoutImage").unwrap();
    // fitted inside the photo, centred, with the graphic's 3:4 shape
    assert!(photo.expand(1.0).contains_rect(r), "{r:?} inside {photo:?}");
    assert!((r.center().x - photo.center().x).abs() < 1.5 && (r.center().y - photo.center().y).abs() < 1.5, "{r:?} centred on {photo:?}");
    assert!((r.width() / r.height() - 0.75).abs() < 0.02, "{r:?}");
    // the masthead covers the photo; below it the photo shows through untouched
    let masthead = egui::pos2(r.center().x, r.top() + r.height() * 0.12);
    let clear = egui::pos2(r.center().x, r.top() + r.height() * 0.7);
    assert!(close(px(&shown, masthead), [255, 0, 255], 12), "masthead {:?}", px(&shown, masthead));
    assert!(close(px(&shown, clear), px(&before, clear), 3), "{:?} vs {:?}", px(&shown, clear), px(&before, clear));

    // half opacity: the masthead blends with the photo
    let r2 = run(&mut h, "view.layoutImageOpacity", json!({"opacity": 0.5}));
    assert_eq!(r2["ok"], true, "{r2}");
    let half = h.snapshot(SETTLE);
    let m = px(&half, masthead);
    assert!(!close(m, [255, 0, 255], 12) && !close(m, px(&before, masthead), 12), "half-opaque masthead {m:?}");

    // the matte darkens the photo outside the graphic's box, and only there
    // (a 3:4 graphic leaves the sides of a wider photo, the top and bottom of a taller one)
    let side = if r.left() - photo.left() > 8.0 {
        egui::pos2((photo.left() + r.left()) / 2.0, photo.center().y)
    } else {
        egui::pos2(photo.center().x, (photo.top() + r.top()) / 2.0)
    };
    assert!(!r.expand(2.0).contains(side) && photo.contains(side), "{side:?}: in the photo, outside {r:?}");
    run(&mut h, "view.layoutImageMatte", json!({"matte": 0.75}));
    let matte = h.snapshot(SETTLE);
    let (lit, dark) = (px(&before, side), px(&matte, side));
    assert!(dark.iter().zip(lit).all(|(d, l)| *d as i32 <= l as i32 / 2 + 2), "matte {dark:?} vs {lit:?}");
    assert!(close(px(&matte, clear), px(&before, clear), 3), "inside the box the matte leaves the photo alone");
    assert!(run(&mut h, "view.layoutImageMatte", json!({}))["ok"] == false, "a matte is required");

    // ⌥⌘O hides it and shows it again
    let k = h.request("ui.key", json!({"key": "o", "cmd": true, "alt": true}), T);
    assert_eq!(k["ok"], true, "{k}");
    h.step();
    h.step();
    assert!(!h.app.ui.layout_image.show);
    assert!(widget(&h, "canvas:layoutImage").is_none(), "hidden: not drawn");
    let back = h.snapshot(SETTLE);
    assert!(close(px(&back, masthead), px(&before, masthead), 3), "hidden: the photo is untouched");
    run(&mut h, "view.layoutImage", json!({"show": true}));
    assert!(h.step_until(T, |h| widget(h, "canvas:layoutImage").is_some()));

    // saved with the UI state (the revision is not)
    let saved = serde_json::to_value(&h.app.ui).unwrap();
    assert_eq!(saved["layoutImage"]["path"], json!(path.to_string_lossy()));
    assert_eq!(saved["layoutImage"]["show"], true);
    assert_eq!(saved["layoutImage"]["opacity"], json!(0.5));
    assert!(saved["layoutImage"].get("revision").is_none());
    let back: crate::state::UiState = serde_json::from_value(saved).unwrap();
    assert_eq!(back.layout_image.path, path.to_string_lossy());
    // an older ui.json without it opens with nothing chosen
    let old: crate::state::UiState = serde_json::from_value(json!({})).unwrap();
    assert_eq!(old.layout_image, crate::state::LayoutImage::default());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_layout_image_that_cant_be_read_says_so_and_showing_none_asks_for_one() {
    let mut h = loupe();
    // nothing chosen: showing it asks for a file, which headless has no dialog for
    let r = run(&mut h, "view.layoutImage", json!({}));
    assert_eq!(r["ok"], false, "{r}");
    assert!(!h.app.ui.layout_image.show);
    assert!(run(&mut h, "view.chooseLayoutImage", json!({"path": "  "}))["ok"] == false, "an empty path is refused");

    let dir = cover("missing");
    let missing = dir.parent().unwrap().join("no-such-cover.png");
    let r = run(&mut h, "view.chooseLayoutImage", json!({"path": missing.to_string_lossy()}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| widget(h, "canvas:layoutImageError").is_some()), "the canvas says the file can't be read");
    assert!(widget(&h, "canvas:layoutImage").is_none());
    // choosing a readable one replaces the error
    let r = run(&mut h, "view.chooseLayoutImage", json!({"path": dir.to_string_lossy()}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(T, |h| widget(h, "canvas:layoutImage").is_some()));
    h.step();
    assert!(widget(&h, "canvas:layoutImageError").is_none());
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn view_loupe_overlay_lists_the_layout_image_commands() {
    let mut h = loupe();
    let names = |h: &Headless| -> Vec<(String, bool, Option<bool>)> {
        let menu = crate::menubar::expanded(&h.app, "Loupe Overlay").unwrap_or_default();
        menu.into_iter()
            .filter_map(|n| match n {
                crate::menubar::MenuNode::Item { id, enabled, checked, .. } => Some((id, enabled, checked)),
                _ => None,
            })
            .collect()
    };
    let before = names(&h);
    assert_eq!(before[0], ("view.chooseLayoutImage".to_string(), true, None));
    // nothing chosen: only Choose… is enabled
    assert!(before[1..].iter().all(|(_, enabled, _)| !enabled), "{before:?}");
    let path = cover("menu");
    run(&mut h, "view.chooseLayoutImage", json!({"path": path.to_string_lossy()}));
    let after = names(&h);
    assert_eq!(after[1], ("view.layoutImage".to_string(), true, Some(true)));
    // 100% opacity and no matte are checked by default
    let checked: Vec<&String> = after.iter().filter(|(_, _, c)| *c == Some(true)).map(|(id, ..)| id).collect();
    assert_eq!(checked, ["view.layoutImage", "view.layoutImageOpacity", "view.layoutImageMatte"]);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
