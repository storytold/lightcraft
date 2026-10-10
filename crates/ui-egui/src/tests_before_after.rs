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
