//! Headless tests of clicks in the two-view Before / After layouts.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{BeforeAfter, ViewMode, Zoom};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Click at a screen position with real egui pointer events (as `ui.pointer` sends them).
fn click_at(h: &mut Headless, pos: egui::Pos2) {
    h.app.synthetic.push(egui::Event::PointerMoved(pos));
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.settle(SETTLE);
}

/// The white-balance selector picks the same point of the photo whichever view of a side-by-side
/// or top/bottom Before / After layout is clicked, at Fit and zoomed in. Clicks on the Before
/// view used to be mapped through the After view's rectangle, so they sampled the photo's edge.
#[test]
fn wb_selector_picks_the_same_point_in_the_before_view() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[2];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    h.app.ui.view = ViewMode::Detail;
    let wb = |h: &Headless| h.app.session.catalog.photo(id).map(|p| (p.develop.wb.temp, p.develop.wb.tint));
    // lay the view out and arm the selector; returns the After view's image rect and the canvas
    let arm = |h: &mut Headless, mode: BeforeAfter, zoom: Zoom| {
        h.request("engine.execute", json!({"command": "develop.reset"}), T);
        h.app.ui.before_after = mode;
        h.app.ui.zoom = zoom;
        h.app.ui.pan = Default::default();
        h.settle(SETTLE);
        let r = h.request("engine.execute", json!({"command": "tool.wbPicker"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        (h.app.image_rect.expect("After view on screen"), h.app.canvas_rect.expect("canvas"))
    };
    for mode in [BeforeAfter::SideBySide, BeforeAfter::TopBottom] {
        // the Before view is the After view moved by one pane: measured at Fit, where both are
        // centred in their panes and so mirror each other about the canvas centre
        let (img, canvas) = arm(&mut h, mode, Zoom::Fit);
        let shift = match mode {
            BeforeAfter::SideBySide => egui::vec2(img.left() + img.right() - 2.0 * canvas.center().x, 0.0),
            _ => egui::vec2(0.0, img.top() + img.bottom() - 2.0 * canvas.center().y),
        };
        assert!(shift.length() > 100.0, "{mode:?}: two views, {shift:?} apart");
        h.request("engine.execute", json!({"command": "tool.wbPicker"}), T);
        for zoom in [Zoom::Fit, Zoom::Percent(100.0)] {
            let mut picked = Vec::new();
            for before_view in [false, true] {
                let (img, canvas) = arm(&mut h, mode, zoom);
                // the middle of the After pane (on the photo at Fit and zoomed in), or the same
                // point of the photo in the Before view
                let after = canvas.center() + shift / 2.0;
                assert!(img.contains(after), "{mode:?} {zoom:?}: {after:?} on the photo {img:?}");
                click_at(&mut h, if before_view { after - shift } else { after });
                assert_eq!(h.app.ui.tool, "", "{mode:?} {zoom:?}: the selector is done after one click");
                picked.push(wb(&h));
            }
            assert_ne!(picked[0], Some((6500.0, 0.0)), "{mode:?} {zoom:?}: the click picked a white balance");
            assert_eq!(picked[1], picked[0], "{mode:?} {zoom:?}: the Before view picks the After view's point");
        }
    }
}

/// Mean (r, g, b) of the 41 × 41 screen pixels around `at`.
fn mean_around(shot: &egui::ColorImage, at: egui::Pos2) -> [f32; 3] {
    let mut sum = [0.0f32; 3];
    let mut n = 0.0;
    for y in (at.y as usize - 20)..=(at.y as usize + 20) {
        for x in (at.x as usize - 20)..=(at.x as usize + 20) {
            let c = shot[(x, y)];
            sum[0] += f32::from(c.r());
            sum[1] += f32::from(c.g());
            sum[2] += f32::from(c.b());
            n += 1.0;
        }
    }
    sum.map(|s| s / n)
}

/// Zoomed in to compare a detail, every Before / After view still shows the edited photo on its
/// After side. A wipe drew its line through the middle of the photo, which zoomed in is mostly off
/// screen: the whole view was the Before, the original photo (#1351).
#[test]
fn zoomed_in_the_after_side_shows_the_edits() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 1.5}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let shot = |h: &mut Headless, mode: BeforeAfter, zoom: Zoom| {
        h.app.ui.before_after = mode;
        h.app.ui.zoom = zoom;
        // (far enough from the middle of the photo that, zoomed in, it is off screen)
        h.app.ui.pan = (0.25, 0.75);
        h.settle(SETTLE);
        h.paint()
    };
    for zoom in [Zoom::Fit, Zoom::Percent(100.0), Zoom::Percent(400.0)] {
        let (edited, original) = (shot(&mut h, BeforeAfter::Off, zoom), shot(&mut h, BeforeAfter::Original, zoom));
        let c = h.app.canvas_rect.expect("canvas").center();
        let luma = |m: [f32; 3]| m.iter().sum::<f32>();
        for mode in [BeforeAfter::SideBySide, BeforeAfter::TopBottom, BeforeAfter::Split, BeforeAfter::SplitTopBottom] {
            let s = shot(&mut h, mode, zoom);
            let canvas = h.app.canvas_rect.expect("canvas");
            // the middle of the After pane, or of the After side of the line
            let (after, before) = match mode {
                BeforeAfter::SideBySide | BeforeAfter::Split => {
                    (c + egui::vec2(canvas.width() / 4.0, 0.0), c - egui::vec2(canvas.width() / 4.0, 0.0))
                }
                _ => (c + egui::vec2(0.0, canvas.height() / 4.0), c - egui::vec2(0.0, canvas.height() / 4.0)),
            };
            let (a, b) = (mean_around(&s, after), mean_around(&s, before));
            if matches!(mode, BeforeAfter::Split | BeforeAfter::SplitTopBottom) {
                // a wipe shows the loupe's own pixels on each side of the line
                let (e, o) = (mean_around(&edited, after), mean_around(&original, before));
                assert!((luma(a) - luma(e)).abs() < 6.0, "{mode:?} {zoom:?}: the After side {a:?} is the edited photo {e:?}");
                assert!((luma(b) - luma(o)).abs() < 6.0, "{mode:?} {zoom:?}: the Before side {b:?} is the original {o:?}");
            } else {
                // two views of the same point of the photo: the After one is the brighter
                assert!(luma(a) > luma(b) + 30.0, "{mode:?} {zoom:?}: the After {a:?} is the edited photo, brighter than the Before {b:?}");
            }
        }
    }
}
