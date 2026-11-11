//! Headless tests of View ▸ Loupe Background: the colour around the photo in the Detail view
//! (Theme Color by default, Black, Dark/Medium/Light Gray, White), from the View menu, the
//! right-click menu around the photo and `view.loupeBackground`; full screen follows it.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::menubar::MenuNode;
use crate::state::{AppSettings, LoupeBackground};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

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

fn set(h: &mut Headless, params: serde_json::Value) -> serde_json::Value {
    h.request("engine.execute", json!({"command": "view.loupeBackground", "params": params}), T)
}

fn pixel(img: &egui::ColorImage, p: egui::Pos2) -> [u8; 3] {
    let c = img.pixels[(p.y as usize) * img.size[0] + p.x as usize];
    [c.r(), c.g(), c.b()]
}

/// A canvas point well outside the photo: halfway into the wider gap beside or above it.
fn around_photo(h: &Headless) -> egui::Pos2 {
    let canvas = h.app.canvas_rect.expect("the loupe has a canvas");
    let img = h.app.image_rect.expect("the loupe shows the photo");
    let (left, top) = (img.left() - canvas.left(), img.top() - canvas.top());
    let p = if left >= top { egui::pos2(canvas.left() + left / 2.0, img.center().y) } else { egui::pos2(img.center().x, canvas.top() + top / 2.0) };
    assert!(left.max(top) > 8.0 && canvas.contains(p) && !img.expand(2.0).contains(p), "a gap around the photo: {canvas:?} {img:?}");
    p
}

fn text_rect(h: &Headless, text: &str) -> Option<egui::Rect> {
    fn find(shape: &egui::epaint::Shape, text: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == text => Some(t.visual_bounding_rect()),
            egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| find(s, text)),
            _ => None,
        }
    }
    h.view.painted_shapes().iter().find_map(|s| find(s, text))
}

fn right_click(h: &mut Headless, p: egui::Pos2) {
    let r = h.request("ui.click", json!({"x": p.x, "y": p.y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
fn the_command_reads_sets_and_rejects_bad_colours() {
    let mut h = loupe();
    assert_eq!(h.app.ui.settings.loupe_background, LoupeBackground::Theme);
    let r = set(&mut h, json!({}));
    assert_eq!(r["result"]["color"], "theme", "{r}");
    let r = set(&mut h, json!({"color": "white"}));
    assert_eq!(r["result"]["color"], "white", "{r}");
    assert_eq!(h.app.ui.settings.loupe_background, LoupeBackground::White);
    for bad in [json!({"color": "bogus"}), json!({"color": 3})] {
        let r = set(&mut h, bad.clone());
        assert_ne!(r["ok"], true, "{bad} is rejected: {r}");
        assert_eq!(h.app.ui.settings.loupe_background, LoupeBackground::White, "{bad} changes nothing");
    }
}

#[test]
fn white_paints_around_the_photo_and_theme_keeps_the_old_look() {
    let mut h = loupe();
    let before = h.snapshot(SETTLE);
    let (out, inside) = (around_photo(&h), h.app.image_rect.unwrap().center());
    let canvas = crate::theme::Tokens::get(&h.view.ctx).canvas;
    assert_eq!(pixel(&before, out), [canvas.r(), canvas.g(), canvas.b()], "Theme Color is the canvas colour");
    set(&mut h, json!({"color": "white"}));
    let after = h.snapshot(SETTLE);
    assert_eq!(pixel(&after, out), [255, 255, 255], "white around the photo");
    assert_eq!(pixel(&after, inside), pixel(&before, inside), "the photo keeps its pixels");
    set(&mut h, json!({"color": "mediumGray"}));
    assert_eq!(pixel(&h.snapshot(SETTLE), out), [0x77, 0x77, 0x77], "medium gray is L* 50");
}

#[test]
fn full_screen_preview_uses_the_chosen_colour() {
    let mut h = loupe();
    set(&mut h, json!({"color": "white"}));
    let r = h.request("engine.execute", json!({"command": "view.fullScreenPreview", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.fullscreen);
    let img = h.snapshot(SETTLE);
    let photo = h.app.image_rect.expect("full screen shows the photo");
    let [w, hh] = img.size;
    let p = [egui::pos2(2.0, 2.0), egui::pos2(w as f32 - 3.0, hh as f32 - 3.0)]
        .into_iter()
        .find(|p| !photo.expand(2.0).contains(*p))
        .expect("a corner outside the photo");
    assert_eq!(pixel(&img, p), [255, 255, 255]);
}

#[test]
fn the_setting_survives_a_restart_and_an_unknown_value_falls_back_to_theme() {
    let s = AppSettings { loupe_background: LoupeBackground::White, ..Default::default() };
    let back: AppSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back.loupe_background, LoupeBackground::White);
    let mut v = serde_json::to_value(AppSettings { confirm_delete: false, ..Default::default() }).unwrap();
    v["loupeBackground"] = json!("purple");
    let odd: AppSettings = serde_json::from_value(v).unwrap();
    assert_eq!(odd.loupe_background, LoupeBackground::Theme);
    assert!(!odd.confirm_delete, "the other settings survive");
}

#[test]
fn right_click_around_the_photo_offers_the_colours_and_on_the_photo_the_zoom_menu() {
    let mut h = loupe();
    let out = around_photo(&h);
    right_click(&mut h, out);
    let gray = text_rect(&h, "Light Gray").expect("the background menu opens around the photo");
    let r = h.request("ui.click", json!({"x": gray.center().x, "y": gray.center().y}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(h.app.ui.settings.loupe_background, LoupeBackground::LightGray);
    assert!(text_rect(&h, "Light Gray").is_none(), "the menu closes");
    let photo = h.app.image_rect.unwrap().center();
    right_click(&mut h, photo);
    assert!(h.painted_text().iter().any(|t| t == "Zoom"), "the photo keeps its menu: {:?}", h.painted_text());
    assert!(text_rect(&h, "Light Gray").is_none(), "without the colours");
}

#[test]
fn view_menu_lists_the_colours_with_the_current_one_checked() {
    let mut h = loupe();
    set(&mut h, json!({"color": "black"}));
    let bar = crate::menubar::menu_bar(&h.app);
    let view = &bar.iter().find(|(l, _)| l == "View").expect("a View menu").1;
    let children = view
        .iter()
        .find_map(|n| match n {
            MenuNode::Submenu { label, children } if label == "Loupe Background" => Some(children),
            _ => None,
        })
        .expect("View ▸ Loupe Background");
    let items: Vec<_> = children
        .iter()
        .filter_map(|n| match n {
            MenuNode::Item { label, checked, params, .. } => Some((label.as_str(), *checked, params["color"].clone())),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 6, "{items:?}");
    assert_eq!(items[0].0, "Theme Color");
    let on: Vec<_> = items.iter().filter(|i| i.1 == Some(true)).collect();
    assert_eq!(on.len(), 1, "{items:?}");
    assert_eq!(on[0].2, "black");
}

fn side_by_side(h: &mut Headless) -> (egui::Rect, egui::Rect) {
    h.app.ui.before_after = crate::state::BeforeAfter::SideBySide;
    h.settle(SETTLE);
    h.snapshot(SETTLE);
    let after = h.app.image_rect.expect("the After view");
    let canvas = h.app.canvas_rect.expect("canvas");
    // the Before view mirrors the After view about the canvas centre
    (after.translate(egui::vec2(2.0 * canvas.center().x - after.left() - after.right(), 0.0)), after)
}

#[test]
fn right_click_on_the_before_photo_keeps_the_zoom_menu() {
    let mut h = loupe();
    let (before, _) = side_by_side(&mut h);
    right_click(&mut h, before.center());
    assert!(h.painted_text().iter().any(|t| t == "Zoom"), "the Before photo keeps its menu: {:?}", h.painted_text());
    assert!(text_rect(&h, "Light Gray").is_none(), "without the colours");
}

#[test]
fn labels_on_the_canvas_stay_readable_on_white() {
    let mut h = loupe();
    let color = |h: &Headless, text: &str| {
        fn find(shape: &egui::epaint::Shape, text: &str) -> Option<egui::Color32> {
            match shape {
                egui::epaint::Shape::Text(t) if t.galley.job.text == text => t.galley.job.sections.first().map(|s| s.format.color),
                egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| find(s, text)),
                _ => None,
            }
        }
        h.view.painted_shapes().iter().find_map(|s| find(s, text)).expect("the label is painted")
    };
    side_by_side(&mut h);
    let theme = color(&h, "Before");
    assert_eq!(theme, crate::theme::Tokens::get(&h.view.ctx).text_dim, "Theme Color keeps the theme's label");
    set(&mut h, json!({"color": "white"}));
    h.snapshot(SETTLE);
    let on_white = color(&h, "Before");
    assert!(on_white.r() < 0x80, "dark on white: {on_white:?}");
    set(&mut h, json!({"color": "mediumGray"}));
    h.snapshot(SETTLE);
    assert!(color(&h, "Before").r() < 0x30, "dark enough on medium gray (L* 50)");
    set(&mut h, json!({"color": "black"}));
    h.snapshot(SETTLE);
    assert!(color(&h, "After").r() > 0x80, "light on black");
}

#[test]
fn full_screen_text_follows_the_colour_even_while_soft_proofing() {
    let mut h = loupe();
    h.app.ui.soft_proof = true;
    set(&mut h, json!({"color": "white"}));
    let r = h.request("engine.execute", json!({"command": "view.fullScreenPreview", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    side_by_side(&mut h);
    let label = h.view.painted_shapes().iter().find_map(|s| find_color(s, "Before")).expect("the Before label in full screen");
    assert!(label.r() < 0x80, "dark on the white full-screen background: {label:?}");
}

fn find_color(shape: &egui::epaint::Shape, text: &str) -> Option<egui::Color32> {
    match shape {
        egui::epaint::Shape::Text(t) if t.galley.job.text == text => t.galley.job.sections.first().map(|s| s.format.color),
        egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| find_color(s, text)),
        _ => None,
    }
}
