//! 100 % means the photo's own pixels as it is shown: after the crop and the user's rotation.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library in a 1400×900 window at 1 px per point, the first photo open in Detail.
fn detail() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn edit(h: &mut Headless, f: impl FnOnce(&mut lightcraft_develop::DevelopSettings)) {
    let id = h.app.session.active().expect("a photo is open");
    let mut s = (*h.app.session.develop_of(id).expect("settings")).clone();
    f(&mut s);
    h.app.session.set_develop(id, s, "Test").expect("edit");
}

fn at_100(h: &mut Headless) -> (f32, f32) {
    h.request("engine.execute", json!({"command": "view.zoom100"}), T);
    h.settle(SETTLE);
    let r = h.app.image_rect.expect("the loupe drew the photo");
    (r.width(), r.height())
}

fn size_of(h: &Headless) -> (f32, f32) {
    let id = h.app.session.active().expect("a photo is open");
    let p = h.app.session.catalog.photo(id).expect("photo");
    (p.width as f32, p.height as f32)
}

// Given an uncropped photo, 100 % is its pixel size
#[test]
fn an_uncropped_photo_at_100_is_its_pixel_size() {
    let mut h = detail();
    let (nw, nh) = size_of(&h);
    let (w, hh) = at_100(&mut h);
    assert!((w - nw).abs() < 1.5 && (hh - nh).abs() < 1.5, "{w}×{hh} for {nw}×{nh}");
}

// Given a crop to half the width and a third of the height, 100 % is the cropped pixels
#[test]
fn a_cropped_photo_at_100_is_the_crops_pixel_size() {
    let mut h = detail();
    let (nw, nh) = size_of(&h);
    edit(&mut h, |s| s.crop.geometry.rect = lightcraft_geom::Rect { x0: 0.25, y0: 0.2, x1: 0.75, y1: 0.2 + 1.0 / 3.0 });
    let (w, hh) = at_100(&mut h);
    assert!((w - nw / 2.0).abs() < 1.5 && (hh - nh / 3.0).abs() < 1.5, "{w}×{hh} for a crop of {}×{}", nw / 2.0, nh / 3.0);
}

// Given a photo rotated a quarter turn, 100 % is its pixel size with the axes swapped
#[test]
fn a_rotated_photo_at_100_is_its_pixel_size_swapped() {
    let mut h = detail();
    let (nw, nh) = size_of(&h);
    edit(&mut h, |s| s.orientation = lightcraft_geom::Orientation::Rotate90);
    let (w, hh) = at_100(&mut h);
    assert!((w - nh).abs() < 1.5 && (hh - nw).abs() < 1.5, "{w}×{hh} for {nh}×{nw}");
}

/// Zoomed in, side by side and stacked Before / After views each stay in their pane: the margins
/// around the panes and the gap between them show the background, as at Fit. The whole-frame
/// renders used to be drawn unclipped, so the After's (magnified, blurred) covered the Before
/// pane's margins.
#[test]
fn zoomed_before_after_views_stay_in_their_panes() {
    use crate::state::{BeforeAfter, Zoom};
    let mut h = detail();
    for mode in [BeforeAfter::SideBySide, BeforeAfter::TopBottom] {
        let mut shots = Vec::new();
        for zoom in [Zoom::Fit, Zoom::Percent(100.0)] {
            h.app.ui.before_after = mode;
            h.app.ui.zoom = zoom;
            h.app.ui.pan = (0.3, 0.4);
            h.settle(SETTLE);
            let img = h.app.image_rect.expect("After view on screen");
            let canvas = h.app.canvas_rect.expect("canvas");
            shots.push((h.paint(), img, canvas));
        }
        let (zoomed, img, canvas) = &shots[1];
        assert!(!canvas.contains_rect(*img), "{mode:?}: 100 % reaches past the pane ({img:?})");
        // outside both panes: the canvas' edges and the gap between the panes
        let c = canvas.center();
        let points = [
            egui::pos2(canvas.left() + 4.0, c.y),
            egui::pos2(c.x, canvas.top() + 4.0),
            egui::pos2(c.x, canvas.bottom() - 4.0),
            match mode {
                BeforeAfter::SideBySide => c,
                _ => egui::pos2(canvas.left() + 200.0, c.y),
            },
        ];
        for q in points {
            let px = |shot: &egui::ColorImage| shot[(q.x as usize, q.y as usize)];
            assert_eq!(px(zoomed), px(&shots[0].0), "{mode:?}: background at {q:?}, zoomed in as at Fit");
        }
    }
}

/// A crop of no size (or a hair wide), from a sidecar, gave the loupe a NaN aspect:
/// the photo was laid out at a NaN size, which egui's debug checks stop on (a panic in a debug
/// build) and nothing could be drawn or clicked. It now lays out at a finite size, fitted and at
/// 100 %.
#[test]
fn a_crop_of_no_size_lays_out_at_a_finite_size() {
    let mut h = detail();
    for rect in [[0.5, 0.5, 0.5, 0.5], [0.0, 0.0, 1e-9, 1.0], [0.0, 0.3, 1.0, 0.3]] {
        // as a sidecar can set it: `crop.set` refuses a crop this small
        edit(&mut h, |d| d.crop.geometry.rect = lightcraft_geom::Rect::new(rect[0], rect[1], rect[2], rect[3]));
        for zoom in ["view.zoomFit", "view.zoom100"] {
            let r = h.request("engine.execute", json!({"command": zoom}), T);
            assert_eq!(r["ok"], true, "{zoom}: {r}");
            h.settle(SETTLE);
            let img = h.app.image_rect.expect("the loupe laid the photo out");
            assert!(img.min.is_finite() && img.max.is_finite(), "{rect:?} at {zoom}: {img:?}");
            let r = h.request(
                "ui.pointer",
                json!({"events": [{"kind": "down", "x": 0.5, "y": 0.5}, {"kind": "drag", "x": 0.6, "y": 0.4}, {"kind": "up", "x": 0.6, "y": 0.4}]}),
                T,
            );
            assert_eq!(r["ok"], true, "{r}");
            h.step();
        }
    }
}
