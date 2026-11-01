//! Headless tests of the White Balance Selector's button.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The White Balance Selector button in the Edit panel does what W does: from Compare it opens
/// the active photo in Detail with the selector armed (it used to arm a tool Compare ignores).
/// Clicking it again disarms it.
#[test]
fn wb_selector_button_in_compare_opens_detail_like_w() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]], "active": vis[0]}}), T);
    let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = crate::state::RightPanel::Edit;
    h.settle(SETTLE);
    // open the Color section, where the selector sits
    if !h.app.widgets.iter().any(|(w, _)| w == "icon:wbPicker") {
        h.request("ui.clickWidget", json!({"id": "section:color"}), T);
        h.settle(SETTLE);
    }
    assert_eq!(h.app.ui.view, ViewMode::Compare);
    let active = h.app.session.selection.active;
    let r = h.request("ui.clickWidget", json!({"id": "icon:wbPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.view, ViewMode::Detail, "the button leaves Compare like W");
    assert_eq!(h.app.ui.tool, "wbPicker");
    assert_eq!(h.app.session.selection.active, active, "on the photo that was active in Compare");
    let r = h.request("ui.clickWidget", json!({"id": "icon:wbPicker"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "a second click disarms it");
}

/// Test notes 2026-10-25 note 13: W toggles the selector (W again puts it away without sampling);
/// while it is armed, a magnified loupe by the pointer outlines the sample and the mouse wheel
/// changes the sample's size instead of zooming the photo.
#[test]
fn w_toggles_the_selector_and_the_wheel_sizes_its_sample() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[2];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    h.app.ui.view = ViewMode::Detail;
    h.settle(SETTLE);
    let wb = |h: &Headless| h.app.session.catalog.photo(id).map(|p| (p.develop.wb.temp, p.develop.wb.tint));
    let before = wb(&h);

    h.request("ui.key", json!({"key": "w"}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "wbPicker", "W arms the selector");
    h.request("ui.key", json!({"key": "w"}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "", "W again puts it away");
    assert_eq!(wb(&h), before, "without sampling");

    h.request("ui.key", json!({"key": "w"}), T);
    h.settle(SETTLE);
    let img = h.app.image_rect.expect("the photo on screen");
    let at = img.center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.settle(SETTLE);
    h.step();
    let rect = |h: &Headless, w: &str| h.app.widgets.iter().find(|(id, _)| id == w).map(|(_, r)| *r);
    let loupe = rect(&h, "wbLoupe").expect("a loupe by the pointer while the selector is armed");
    assert!(!loupe.contains(at), "beside the pointer, not under it");
    let small = rect(&h, "wbLoupe:sample").expect("the sample outlined");
    let (size, zoom) = (h.app.ui.wb_sample, h.app.ui.zoom);
    for _ in 0..2 {
        h.app.synthetic.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 1.0),
            phase: egui::TouchPhase::Move,
            modifiers: Default::default(),
            source: egui::MouseWheelSource::Wheel,
        });
        h.step();
    }
    h.settle(SETTLE);
    h.step();
    assert_eq!(h.app.ui.wb_sample, size + 4, "two notches up: a larger sample");
    assert_eq!(h.app.ui.zoom, zoom, "the wheel doesn't zoom while the selector is armed");
    assert_eq!(h.app.ui.tool, "wbPicker");
    let large = rect(&h, "wbLoupe:sample").expect("the sample outlined");
    assert!(large.width() > small.width(), "the outline grows with the sample: {small:?} → {large:?}");
}

/// Test notes 2026-10-25 note 13: a click on a spot with no colour left (here a clipped white
/// photo) is refused in the app too: the reason shows as a toast, the white balance is unchanged
/// and the selector stays armed for the next click.
#[test]
fn a_click_on_a_clipped_spot_is_refused_and_the_selector_stays_armed() {
    let dir = std::env::temp_dir().join(format!("lc-wb-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = lightcraft_raster::Rgba8 { width: 96, height: 64, data: vec![[255, 255, 255, 255]; 96 * 64] };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let path = dir.join("white.png");
    std::fs::write(&path, bytes).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    session.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.view = ViewMode::Detail;
    h.settle(SETTLE);
    let wb = |h: &Headless| h.app.session.catalog.photo(id).map(|p| (p.develop.wb.temp, p.develop.wb.tint));
    let before = wb(&h);
    h.request("ui.key", json!({"key": "w"}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "wbPicker");
    let at = h.app.image_rect.expect("the photo on screen").center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    h.settle(SETTLE);
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("too bright"), "the reason shows: {toast:?}");
    assert_eq!(wb(&h), before, "nothing changes");
    assert_eq!(h.app.ui.tool, "wbPicker", "the selector stays armed");
    let _ = std::fs::remove_dir_all(&dir);
}
