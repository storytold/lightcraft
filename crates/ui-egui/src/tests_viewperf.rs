//! Ported from OneSlips (NOTES-1028 notes 15, 16 and 29): how soon a slider follows the pointer, ← / → through a
//! selection, and what a brush stroke on a synced selection sets off.

use std::time::Duration;

use lcc_catalog::PhotoId;
use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn detail() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

/// Selects the first `n` photos of the view (the first active), with Auto Sync on for more than one.
fn select_synced(h: &mut Headless, n: usize) -> Vec<PhotoId> {
    let ids: Vec<PhotoId> = h.app.session.visible_cloned().into_iter().take(n).collect();
    assert_eq!(ids.len(), n, "the demo has {n} photos");
    exec(h, "library.select", json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()}));
    if n > 1 {
        exec(h, "develop.autoSync", json!({"on": true}));
    }
    ids
}

fn press(h: &mut Headless, pos: egui::Pos2, pressed: bool) {
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
}

/// Note 15: a press on a slider and a slow drag (1 point a frame) moves the value from the first frames the pointer
/// moves (it waited for egui's 6-point click distance), and the drag is one undo step.
#[test]
fn a_slider_follows_a_slow_drag_from_its_first_move() {
    // (three photos with Auto Sync: the single photo's drag is the same path without the sync)
    {
        let n = 3usize;
        let mut h = detail();
        let ids = select_synced(&mut h, n);
        h.settle(SETTLE);
        let id = "light.exposure";
        let rect = h.app.widgets.iter().find(|(w, _)| w == &format!("slider:{id}")).map(|(_, r)| *r).expect("the Exposure slider");
        let value = |h: &Headless, p: PhotoId| lcc_develop::controls::get(&h.app.session.develop_of(p).unwrap(), id).unwrap_or(0.0);
        let before = value(&h, ids[0]);
        let undo = h.app.session.undo.len();
        let pos = rect.center() + egui::vec2(3.0, 0.0);
        h.app.synthetic.push(egui::Event::PointerMoved(pos));
        press(&mut h, pos, true);
        h.step();
        let mut moved_at = None;
        for k in 1..=5 {
            h.app.synthetic.push(egui::Event::PointerMoved(pos + egui::vec2(k as f32, 0.0)));
            h.step();
            if moved_at.is_none() && (value(&h, ids[0]) - before).abs() > 1e-9 {
                moved_at = Some(k);
            }
        }
        assert!(moved_at.is_some_and(|k| k <= 2), "n={n}: the value moves with the first points of travel: {moved_at:?}");
        assert!(value(&h, ids[0]) > before, "n={n}: to the right, up");
        if n > 1 {
            assert!((value(&h, ids[1]) - value(&h, ids[0])).abs() < 1e-9, "Auto Sync carries it live");
        }
        press(&mut h, pos + egui::vec2(5.0, 0.0), false);
        h.step();
        h.step();
        assert_eq!(h.app.session.undo.len(), undo + 1, "n={n}: one undo step");
        assert!(h.app.session.interaction.is_none());
    }
}

/// Note 16 in the app: → and ← (keys) step through the selection in Detail and Grid, keep it, and wrap.
#[test]
fn arrow_keys_step_through_the_selection_in_detail_and_grid() {
    let mut h = detail();
    for view in ["detail", "grid"] {
        let r = h.request("ui.set", json!({"view": if view == "grid" { "photoGrid" } else { view }}), T);
        assert_eq!(r["ok"], true, "{r}");
        let vis = h.app.session.visible_cloned();
        let picked = [vis[1], vis[4], vis[7]];
        exec(&mut h, "library.select", json!({"ids": picked.iter().map(|i| i.0).collect::<Vec<_>>()}));
        exec(&mut h, "develop.autoSync", json!({"on": true}));
        let mut seen = Vec::new();
        for key in ["right", "right", "right", "left"] {
            let r = h.request("ui.key", json!({"key": key}), T);
            assert_eq!(r["ok"], true, "{r}");
            seen.push(h.app.session.active().unwrap());
            assert_eq!(h.app.session.selection.ids.len(), 3, "{view}: the selection is kept");
        }
        assert_eq!(seen, vec![picked[1], picked[2], picked[0], picked[2]], "{view}: within the selection, wrapping round");
        assert!(h.app.session.auto_sync, "{view}: Auto Sync stays on");
    }
}

/// Note 29: while the photographer paints (and for a moment after each stroke) the thumbnails of the other photos an
/// edit reached keep the one they have; then they follow.
#[test]
fn while_painting_the_other_photos_thumbnails_wait() {
    let mut h = detail();
    let ids = select_synced(&mut h, 3);
    h.settle(SETTLE);
    let key = |h: &Headless| h.app.renderer.thumb(ids[1]).map(|t| t.key);
    let before = key(&h);
    assert!(before.is_some(), "the filmstrip has its thumbnail");
    h.app.paint_hold_until = h.time() + 0.9;
    exec(&mut h, "develop.set", json!({"ids": [ids[1].0], "values": {"light.exposure": 1.0}}));
    for _ in 0..90 {
        h.app.paint_hold_until = h.time() + 0.9;
        h.step();
        assert!(!h.app.renderer.is_pending(crate::render::Slot::Thumb(ids[1])), "nothing for the other photo while painting");
    }
    assert_eq!(key(&h), before, "its thumbnail waits");
    for _ in 0..60 {
        h.step();
    }
    h.settle(SETTLE);
    assert_ne!(key(&h), before, "its thumbnail follows once the painting pauses");
}

/// The mean brightness (0..255) of the loupe's picture in the headless frame.
fn loupe_brightness(h: &Headless) -> f64 {
    let img = h.app.image_rect.expect("the photo on screen");
    let shot = h.paint();
    let [w, ht] = shot.size;
    let (x0, x1) = ((img.left().max(0.0) as usize).min(w), (img.right().max(0.0) as usize).min(w));
    let (y0, y1) = ((img.top().max(0.0) as usize).min(ht), (img.bottom().max(0.0) as usize).min(ht));
    let (mut sum, mut n) = (0.0, 0.0);
    for y in (y0..y1).step_by(4) {
        for x in (x0..x1).step_by(4) {
            let c = shot.pixels[y * w + x];
            sum += f64::from(c.r()) + f64::from(c.g()) + f64::from(c.b());
            n += 3.0;
        }
    }
    if n > 0.0 { sum / n } else { 0.0 }
}

/// Ported from OneSlips (NOTES-1028 note 14): dragging an Optics slider (a lens correction changes the
/// warp) keeps the photo on screen: a picture is drawn until the next draft arrives, never a black
/// loupe.
#[test]
fn an_optics_drag_never_shows_a_black_loupe() {
    let mut h = detail();
    h.settle(SETTLE);
    let rest = loupe_brightness(&h);
    assert!(rest > 60.0, "the demo photo is on screen: {rest}");
    for (ctl, v) in [("optics.distortion", 30.0), ("optics.caRed", 20.0)] {
        exec(&mut h, "develop.beginInteraction", json!({"label": ctl}));
        for k in 1..=6 {
            exec(&mut h, "develop.set", json!({"control": ctl, "value": v * f64::from(k) / 6.0}));
            h.step();
            let shown = h.app.loupe_shown.map(|l| l.1);
            assert_ne!(shown, Some("none"), "{ctl} step {k}: the loupe draws a picture");
            let b = loupe_brightness(&h);
            assert!(b > rest * 0.6, "{ctl} step {k}: not black ({b} against {rest} at rest)");
        }
        exec(&mut h, "develop.endInteraction", json!({}));
        h.settle(SETTLE);
    }
}

/// Gap 26 / plan 4.8 (opt-in, run with
/// `<PREFIX>_PROBES=1 cargo test --release -p lcc-ui-egui wheel_scroll_100k -- --nocapture`): one frame of wheel
/// scrolling on a 100k-photo grid, timed frame by frame (a wheel notch delivered as input, then the frame drawn),
/// against the 16 ms budget. Prints the median, 95th percentile and the grid panel's own share; the ceiling it
/// asserts is loose (shared, loaded machines), so only a real regression fails.
#[test]
fn wheel_scroll_100k_frame() {
    if !lcc_testkit::probe("wheel_scroll_100k_frame") {
        return;
    }
    use std::time::Instant;
    let app = LccApp::new(crate::tests_grid::library_of(100_000), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    h.app.ui.thumb_size = 160.0;
    h.settle(SETTLE);
    let canvas = h.app.canvas_rect.expect("grid drawn");
    h.request("ui.move", json!({"x": canvas.center().x, "y": canvas.center().y}), T);
    let mut frames = Vec::new();
    let mut panel = Vec::new();
    let top0 = h.app.grid_scroll.unwrap_or(0.0);
    for _ in 0..120 {
        h.app.synthetic.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            phase: egui::TouchPhase::Move,
            modifiers: Default::default(),
        });
        let t = Instant::now();
        h.step();
        frames.push(t.elapsed().as_secs_f64() * 1e3);
        panel.push(h.app.caches.grid_stats.last_show.as_secs_f64() * 1e3);
    }
    assert!(h.app.grid_scroll.unwrap_or(0.0) > top0 + 1000.0, "the wheel scrolled the grid: {:?} from {top0}", h.app.grid_scroll);
    let pick = |v: &mut Vec<f64>, q: f64| {
        v.sort_by(f64::total_cmp);
        v.get(((v.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0)
    };
    let (median, p95, in_grid) = (pick(&mut frames, 0.5), pick(&mut frames, 0.95), pick(&mut panel, 0.5));
    eprintln!("wheel scroll 100k | frame median {median:.1} ms, p95 {p95:.1} ms, grid panel median {in_grid:.2} ms (budget 16 ms)");
    eprintln!("wheel scroll 100k | frame median: {median:.1} ms wall"); // the `cargo xtask bench` row
    assert!(median < 160.0, "wheel-scroll frame median {median:.0} ms, ten times the 16 ms budget");
}
