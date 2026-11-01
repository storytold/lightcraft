//! Dragging the photo at 100 % rendered it in pieces: each new window of the zoomed view replaced
//! the last, so what had been sharp went soft again behind the drag, and nothing was made ahead.
//! Now the windows made stay where they are while the view pans (the renderer's pan tiles), a
//! still view makes the ones around it ahead at low priority, and a pan asks for the window it is
//! heading into.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Detail at 100 %, the photo centred, once the view is sharp (and, with `still`, once everything
/// it makes ahead is done).
fn zoomed(still: bool) -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for (method, params) in [
        ("ui.set", json!({"view": "detail"})),
        ("engine.execute", json!({"command": "view.navigate", "params": {"zoom": {"percent": 100}, "pan": [0.5, 0.5]}})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    assert!(h.step_until(SETTLE, |h| h.app.loupe_sharp == Some(1.0)), "the zoomed view gets sharp: {:?}", h.app.loupe_sharp);
    if still {
        h.settle(SETTLE);
    }
    h
}

fn press(h: &mut Headless, pos: egui::Pos2, pressed: bool) {
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
}

/// What one frame of a pan showed: a picture of the photo (the whole-frame render, magnified where
/// no window is sharp yet), and the share drawn sharp.
fn check_frame(h: &Headless, what: &str) -> f32 {
    let shown = h.app.loupe_shown.map(|(_, s)| s);
    assert!(shown.is_some_and(|s| s != "none"), "{what}: a picture of the photo is drawn, not {shown:?}");
    h.app.loupe_sharp.expect("a zoomed view says how much of it is sharp")
}

/// A drag with the primary button from the middle of the view, `steps` frames of `by` points
/// each: the frame times (ms) and the sharp share of every frame.
fn drag(h: &mut Headless, by: egui::Vec2, steps: usize) -> (Vec<f64>, Vec<f32>) {
    let at = h.app.image_rect.expect("photo").intersect(h.app.canvas_rect.expect("canvas")).center();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    press(h, at, true);
    h.step();
    let (mut ms, mut sharp) = (Vec::new(), Vec::new());
    for i in 1..=steps {
        h.app.synthetic.push(egui::Event::PointerMoved(at + by * i as f32));
        let t0 = Instant::now();
        h.step();
        ms.push(t0.elapsed().as_secs_f64() * 1e3);
        sharp.push(check_frame(h, &format!("frame {i}")));
    }
    press(h, at + by * steps as f32, false);
    h.step();
    (ms, sharp)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// A still zoomed view makes the windows around it ahead; a pan into them is sharp in every frame
/// and renders no window of the view on the way.
#[test]
fn a_pan_into_the_windows_made_ahead_is_sharp_all_the_way() {
    let mut h = zoomed(true);
    assert!(h.app.renderer.pan_tile_count() >= 3, "the windows around the view were made ahead: {}", h.app.renderer.pan_tile_count());
    let kept = h.app.renderer.memory()["textures"]["panTiles"].as_u64();
    assert_eq!(kept, Some(h.app.renderer.pan_tile_count() as u64), "the kept windows count in the renderer's memory");
    let pan = h.app.ui.pan;
    let renders = h.app.renderer.window_renders();
    // dragging the photo left brings in what lies to the right of the view
    let (ms, sharp) = drag(&mut h, egui::vec2(-12.0, 0.0), 40);
    assert!(h.app.ui.pan.0 > pan.0, "the photo panned: {pan:?} -> {:?}", h.app.ui.pan);
    assert!(sharp.iter().all(|s| *s >= 0.999), "every frame of the pan is sharp: {sharp:?}");
    assert_eq!(h.app.renderer.window_renders(), renders, "no window is rendered again on the way");
    eprintln!("pan into the windows made ahead: frames median {:.1} ms", median(ms));
}

/// A pan past what was made ahead never leaves the view empty (the whole-frame render shows where
/// nothing is sharp yet), is sharp again once it stops, and going back finds the places it passed
/// sharp at once: the windows made are kept while the view pans, not thrown away.
#[test]
fn a_long_pan_keeps_what_was_sharp() {
    let mut h = zoomed(false);
    let start = h.app.ui.pan;
    let (_, sharp) = drag(&mut h, egui::vec2(-30.0, -10.0), 40);
    assert!(h.app.ui.pan != start, "the photo panned");
    eprintln!("a long pan, sharp share per frame {sharp:?}");
    assert!(h.step_until(SETTLE, |h| h.app.loupe_sharp == Some(1.0)), "sharp again once it stops: {:?}", h.app.loupe_sharp);
    assert!(h.app.renderer.pan_tile_count() >= 1, "the windows it passed are kept");
    // back to where it started: shown sharp in the very next frame, without a new render
    let renders = h.app.renderer.window_renders();
    let r = h.request("engine.execute", json!({"command": "view.navigate", "params": {"pan": [start.0, start.1]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(check_frame(&h, "back at the start"), 1.0, "the start is still sharp");
    assert_eq!(h.app.renderer.window_renders(), renders, "and is not rendered again");
}

/// An edit makes the kept windows obsolete: they are never drawn with the old look, and a view at
/// Fit keeps none.
#[test]
fn an_edit_lets_the_kept_windows_go() {
    let mut h = zoomed(true);
    assert!(h.app.renderer.pan_tile_count() >= 1);
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"values": {"light.exposure": 1.0}}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let sharp = h.app.loupe_sharp.unwrap_or(1.0);
    assert!(sharp < 1.0 || h.app.renderer.pan_tile_count() == 0, "windows of the old look are not drawn as sharp ({sharp})");
    assert!(h.step_until(SETTLE, |h| h.app.loupe_sharp == Some(1.0)), "the new look's window arrives");
    h.settle(SETTLE);
    let look = h.app.region_view.map(|v| v.settings);
    for t in h.app.renderer.pan_tiles() {
        let v = h.app.region_tiles.get(&(false, t.key)).expect("each kept window has its view");
        assert_eq!(Some(v.settings), look, "only windows of the current look are kept");
    }
    let r = h.request("engine.execute", json!({"command": "view.navigate", "params": {"zoom": "fit"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.renderer.pan_tile_count(), 0, "a view at Fit keeps no zoom windows");
}

/// Side by side and zoomed in, the share drawn sharp counts each pane's photo only: the gap between
/// the panes shows no photo, so a still view whose windows are all there is wholly sharp.
#[test]
fn side_by_side_and_still_is_wholly_sharp() {
    let mut h = zoomed(true);
    h.app.ui.before_after = crate::state::BeforeAfter::SideBySide;
    h.step();
    assert!(h.step_until(SETTLE, |h| h.app.loupe_sharp == Some(1.0)), "both panes get sharp: {:?}", h.app.loupe_sharp);
}
