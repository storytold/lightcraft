//! Headless tests of the Edit panel's slider name columns: opening or closing a part of the panel
//! never moves or resizes the tracks of the sliders around it, and a few long names (Color
//! Grading's "Highlights Luminance") don't shorten every slider.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library in Detail with the Edit panel at `width` and Light, Color and Effects open.
fn edit_panel(width: f32) -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    let id = h.app.session.visible_cloned()[0].0;
    assert_eq!(h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}), T)["ok"], true);
    assert_eq!(h.request("ui.set", json!({"view": "detail", "right": "edit"}), T)["ok"], true);
    h.app.ui.right_width = width;
    h.app.ui.open_sections = vec!["light".to_string(), "color".to_string(), "effects".to_string()];
    h.settle(SETTLE);
    h
}

fn track(h: &Headless, control: &str) -> egui::Rect {
    let id = format!("slider:{control}");
    h.app.widgets.iter().find(|(w, _)| *w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn label(h: &Headless, control: &str) -> egui::Rect {
    let id = format!("sliderLabel:{control}");
    h.app.widgets.iter().find(|(w, _)| *w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// Draws a few passes so every name column has seen all of its rows.
fn redraw(h: &mut Headless) {
    for _ in 0..3 {
        h.settle(SETTLE);
    }
}

#[test]
fn opening_color_grading_leaves_the_other_tracks_where_they_are() {
    for width in [270.0, 318.0, 416.0] {
        let mut h = edit_panel(width);
        redraw(&mut h);
        let closed: Vec<egui::Rect> = ["wb.tempRel", "color.vibrance", "effects.texture", "light.exposure"].iter().map(|c| track(&h, c)).collect();
        h.app.ui.toggle_flyout("grading");
        redraw(&mut h);
        assert!(h.app.widgets.iter().any(|(w, _)| w == "slider:grading.blending"), "Color Grading is open");
        // Color Grading's own sliders don't sit in the column of its longest names either
        let lum = track(&h, "grading.highlights.lum");
        for (c, before) in ["wb.tempRel", "color.vibrance", "effects.texture", "light.exposure"].iter().zip(&closed) {
            let open = track(&h, c);
            // (x within the row: the name column's left edge is PAD_L in)
            let row_left = label(&h, c).left() - crate::widgets::PAD_L;
            eprintln!(
                "{c} at {width} pt: track starts at x {:.1} closed, {:.1} with Color Grading open",
                before.left() - row_left,
                open.left() - row_left
            );
            assert!(
                (open.left() - before.left()).abs() < 0.01 && (open.width() - before.width()).abs() < 0.01,
                "{c} at {width} pt: closed {before:?}, Color Grading open {open:?}"
            );
            // (the rows below Color Grading move down, as they should)
        }
        assert!(lum.width() > 0.0, "{lum:?}");
        h.app.ui.toggle_flyout("grading");
        redraw(&mut h);
        for (c, before) in ["wb.tempRel", "color.vibrance", "effects.texture", "light.exposure"].iter().zip(&closed) {
            let again = track(&h, c);
            assert!((again.left() - before.left()).abs() < 0.01 && (again.width() - before.width()).abs() < 0.01, "{c} after closing: {again:?}");
        }
    }
}
