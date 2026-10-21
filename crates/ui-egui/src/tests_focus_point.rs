//! Headless tests of the camera's focus point (#1480): View ▸ Show Focus Point draws the box where the
//! camera focused, through the user's rotation and crop; Toggle Zoom (Z) centres on it; the Info panel
//! names the focus mode.

use std::sync::Arc;
use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_meta::FocusPoint;
use serde_json::json;

use crate::headless::Headless;
use crate::state::Zoom;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn focus(x0: f64, y0: f64, x1: f64, y1: f64) -> FocusPoint {
    FocusPoint {
        rect: lightcraft_geom::Rect { x0, y0, x1, y1 },
        mode: Some("AF-C".into()),
        area: Some("Wide".into()),
        tracking: Some("Animal Eye Tracking".into()),
    }
}

/// The demo library in Detail with the Info panel, its first photo active and given `f` as its focus point.
fn detail_with_focus(f: Option<FocusPoint>) -> (Headless, PhotoId) {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = *h.app.session.visible_cloned().first().expect("a demo photo");
    let mut meta = h.app.session.catalog.photo(id).expect("photo").meta.clone();
    meta.focus = f;
    h.app.session.catalog.apply(Op::SetMeta { id, meta: Box::new(meta) }).expect("set meta");
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("ui.set", json!({"view": "detail", "right": "info"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    (h, id)
}

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn run(h: &mut Headless, cmd: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": cmd, "params": params}), T);
    assert_eq!(r["ok"], true, "{cmd}: {r}");
    h.settle(SETTLE);
    r["result"].clone()
}

/// Where `(x, y)` (fractions of the drawn photo) is on screen.
fn on_image(h: &Headless, x: f32, y: f32) -> egui::Pos2 {
    let img = h.app.image_rect.expect("the photo is drawn");
    egui::pos2(img.left() + x * img.width(), img.top() + y * img.height())
}

#[test]
fn the_overlay_draws_on_the_focus_point_and_follows_rotation() {
    let (mut h, _) = detail_with_focus(Some(focus(0.20, 0.30, 0.26, 0.36)));
    assert!(widget(&h, "canvas:focusPoint").is_none(), "off by default");
    // the Info panel names how the camera focused
    assert!(widget(&h, "info:focus").is_some(), "no Focus row in the Info panel");
    assert_eq!(run(&mut h, "view.focusPoint", json!({}))["on"], true);
    let b = widget(&h, "canvas:focusPoint").expect("the focus box is drawn");
    let want = on_image(&h, 0.23, 0.33);
    assert!((b.center() - want).length() < 1.5, "box at {:?}, want {want:?}", b.center());
    let img = h.app.image_rect.unwrap();
    assert!((b.width() - 0.06 * img.width()).abs() < 1.5, "{b:?} in {img:?}");
    // Rotate Right turns the photo 90° clockwise: the upright point (x, y) is drawn at (1 − y, x)
    run(&mut h, "photo.rotateRight", json!({}));
    let b = widget(&h, "canvas:focusPoint").expect("still drawn after rotating");
    let want = on_image(&h, 0.67, 0.23);
    assert!((b.center() - want).length() < 1.5, "rotated box at {:?}, want {want:?}", b.center());
    // and the command turns it off again
    assert_eq!(run(&mut h, "view.focusPoint", json!({"on": false}))["on"], false);
    assert!(widget(&h, "canvas:focusPoint").is_none());
}

#[test]
fn toggle_zoom_centres_on_the_focus_point() {
    let (mut h, _) = detail_with_focus(Some(focus(0.70, 0.20, 0.74, 0.26)));
    assert_eq!(h.app.ui.zoom, Zoom::Fit);
    let r = h.request("ui.key", json!({"key": "z"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(matches!(h.app.ui.zoom, Zoom::Percent(_)), "{:?}", h.app.ui.zoom);
    let (u, v) = h.app.ui.pan;
    assert!((u - 0.72).abs() < 1e-3 && (v - 0.23).abs() < 1e-3, "pan {u} {v}");
    // the zoomed view is centred on it (the canvas centre shows the focus point)
    let canvas = h.app.canvas_rect.expect("canvas");
    let at = on_image(&h, 0.72, 0.23);
    let img = h.app.image_rect.unwrap();
    // (unless the photo's edge stops the pan: here the point is well inside, so it is centred)
    assert!(img.width() > canvas.width(), "zoomed in");
    assert!((at.x - canvas.center().x).abs() < 3.0, "{at:?} vs {:?}", canvas.center());
    // Z again: back to Fit; with Zoom to Focus Point off, Z keeps the current pan
    run(&mut h, "view.zoomToggle", json!({}));
    assert_eq!(h.app.ui.zoom, Zoom::Fit);
    assert_eq!(run(&mut h, "view.zoomToFocus", json!({}))["on"], false);
    h.app.ui.pan = (0.5, 0.5);
    run(&mut h, "view.zoomToggle", json!({}));
    assert_eq!(h.app.ui.pan, (0.5, 0.5));
}

#[test]
fn without_a_focus_point_zoom_and_info_are_unchanged() {
    let (mut h, _) = detail_with_focus(None);
    run(&mut h, "view.focusPoint", json!({"on": true}));
    assert!(widget(&h, "canvas:focusPoint").is_none());
    assert!(widget(&h, "info:focus").is_none());
    h.app.ui.pan = (0.4, 0.6);
    run(&mut h, "view.zoomToggle", json!({}));
    assert!(matches!(h.app.ui.zoom, Zoom::Percent(_)));
    assert_eq!(h.app.ui.pan, (0.4, 0.6), "no focus point: Z zooms where it did before");
}

/// The box maps through the crop (and a point cropped away gives no box, so Z zooms as before).
#[test]
fn focus_box_follows_the_crop_and_drops_a_point_cropped_away() {
    let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 1 }, "a.jpg", "JPEG", 6000, 4000, "2026-10-13T00:00:00");
    p.meta.focus = Some(focus(0.70, 0.45, 0.80, 0.55));
    let b = crate::panels::detail::focus_box(&p).expect("box");
    assert!((b.x0 - 0.70).abs() < 1e-9 && (b.y1 - 0.55).abs() < 1e-9, "{b:?}");
    // keep the right half: x 0.5..1 becomes 0..1
    let mut d = (*p.develop).clone();
    d.crop.geometry.rect = lightcraft_geom::Rect { x0: 0.5, y0: 0.0, x1: 1.0, y1: 1.0 };
    p.develop = Arc::new(d.clone());
    let b = crate::panels::detail::focus_box(&p).expect("box");
    assert!((b.x0 - 0.40).abs() < 1e-6 && (b.x1 - 0.60).abs() < 1e-6, "{b:?}");
    // keep the left half: the point is gone
    d.crop.geometry.rect = lightcraft_geom::Rect { x0: 0.0, y0: 0.0, x1: 0.5, y1: 1.0 };
    p.develop = Arc::new(d);
    assert!(crate::panels::detail::focus_box(&p).is_none());
    // a zero-size photo or a garbage box never panics
    let mut z = Photo::new(PhotoId(2), Source::Demo { scene: 1 }, "z.jpg", "JPEG", 0, 0, "2026-10-13T00:00:00");
    z.meta.focus = Some(focus(f64::NAN, 0.0, f64::INFINITY, 1.0));
    assert!(crate::panels::detail::focus_box(&z).is_none());
    z.meta.focus = Some(focus(0.4, 0.4, 0.6, 0.6));
    assert!(crate::panels::detail::focus_box(&z).is_some());
}

#[test]
fn compare_and_survey_draw_each_photos_focus_point() {
    let (mut h, id) = detail_with_focus(Some(focus(0.20, 0.30, 0.26, 0.36)));
    let other = h.app.session.visible_cloned().get(1).copied().expect("a second photo");
    run(&mut h, "view.focusPoint", json!({"on": true}));
    run(&mut h, "library.select", json!({"ids": [id.0, other.0], "active": id.0}));
    for view in ["view.compare", "view.survey"] {
        run(&mut h, view, json!({}));
        assert!(widget(&h, &format!("compare:focusPoint:{}", id.0)).is_some(), "{view}: no focus box");
        assert!(widget(&h, &format!("compare:focusPoint:{}", other.0)).is_none(), "{view}: a box on a photo without one");
    }
}

/// A photo imported before focus points were read gets its point from the background reader the frame loop drives
/// (`focus.pump`, started by itself once the library is open), and Photo ▸ Read Focus Points runs it on demand.
#[test]
fn the_frame_loop_reads_focus_points_for_photos_imported_earlier() {
    let dir = std::env::temp_dir().join(format!("lc-ui-focus-scan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let add = |app: &mut LightcraftApp, id: u64, name: &str| {
        let path = dir.join(name);
        std::fs::write(&path, lightcraft_meta::focus::synthetic_arw(3, 200_000)).expect("write");
        let mut p = Photo::new(PhotoId(id), Source::File { path: path.to_string_lossy().into() }, name, "ARW", 8640, 5760, "2026-10-13T00:00:00");
        p.meta.camera = "SONY ILCE-1".into();
        app.session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).expect("add");
    };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.settings.gpu = false;
    add(&mut app, 9001, "old.ARW");
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let has_point = |h: &Headless, id: u64| h.app.session.catalog.photo(PhotoId(id)).is_some_and(|p| p.meta.focus.is_some());
    assert!(h.step_until(SETTLE, |h| has_point(h, 9001)), "read by itself");
    assert!(h.step_until(T, |h| h.app.session.activity.list().iter().all(|t| t.kind != "focus")), "its row goes when it is done");
    // a photo added later is read when asked (the menu item runs the command)
    add(&mut h.app, 9002, "later.ARW");
    let spec = h.app.session.commands().into_iter().find(|c| c.id == "photo.readFocusPoints").expect("the command");
    assert_eq!(spec.menu, vec!["Photo"]);
    let r = run(&mut h, "photo.readFocusPoints", json!({}));
    assert_eq!(r["queued"], 1, "{r}");
    assert!(h.step_until(SETTLE, |h| has_point(h, 9002)));
    let _ = std::fs::remove_dir_all(&dir);
}
