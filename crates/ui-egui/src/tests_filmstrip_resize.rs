//! The filmstrip's height: its top edge (`filmstrip:resize`) drags it between
//! [`crate::state::FILM_HEIGHT`]'s bounds, its cells grow with it, every view leaves it that room,
//! and the height survives a restart (the ui.json round trip). And the shot info strip under the
//! loupe's photo (`view.shotInfo`), which stays when zoomed in and steps aside for a tool.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{FILM_HEIGHT, RightPanel, UiState, ViewMode, Zoom};
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(60);

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn loupe() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[0];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), T);
    h.app.ui.view = ViewMode::Detail;
    h.app.ui.right = RightPanel::Edit;
    h.app.ui.filmstrip = true;
    h.settle(T);
    h
}

fn first_film_cell(h: &Headless) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w.starts_with("film:") && w[5..].parse::<u64>().is_ok()).map(|(_, r)| *r).expect("a filmstrip cell")
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn the_filmstrip_resizes_from_its_top_edge_and_the_shot_info_strip_stays_when_zoomed_in() {
    let mut h = loupe();
    // dragging the filmstrips top edge resizes it and the height is kept
    {
        let edge = widget(&h, "filmstrip:resize").expect("the resize edge");
        let before = first_film_cell(&h);
        let canvas_before = h.app.canvas_rect.expect("the canvas");
        let (x, y) = (edge.center().x, edge.center().y);
        let r = h.request("ui.drag", json!({"x": x, "y": y, "toX": x, "toY": y - 100.0, "steps": 8}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(T);
        let grown = h.app.ui.film_height();
        assert!(grown > FILM_HEIGHT.default + 60.0, "dragged up 100 points: {grown}");
        let after = first_film_cell(&h);
        assert!(after.width() > before.width() && after.height() > before.height(), "{before:?} → {after:?}");
        let canvas = h.app.canvas_rect.expect("the canvas");
        assert!(canvas.height() < canvas_before.height() - 60.0, "the photo leaves the strip its room");
        // far past the top: capped; a hostile value from a file: clamped
        let edge = widget(&h, "filmstrip:resize").expect("the edge");
        h.request("ui.drag", json!({"x": x, "y": edge.center().y, "toX": x, "toY": 0.0, "steps": 8}), T);
        h.settle(T);
        assert!(h.app.ui.film_height() <= FILM_HEIGHT.max);
        // kept across a restart
        let saved = serde_json::to_string(&h.app.ui).unwrap();
        let back = serde_json::from_str::<UiState>(&saved).unwrap().sanitized();
        assert_eq!(back.film_height(), h.app.ui.film_height());
        let odd: UiState = serde_json::from_str(r#"{"filmHeight": 1e9}"#).unwrap();
        assert_eq!(odd.sanitized().film_height, FILM_HEIGHT.max);
        // a double click puts it back
        let edge = widget(&h, "filmstrip:resize").expect("the edge");
        let ask = json!({"x": edge.center().x, "y": edge.center().y, "count": 2});
        let r = h.request("ui.click", ask, T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(T);
        assert_eq!(h.app.ui.film_height(), FILM_HEIGHT.default);
    }
    // the shot info strip stays when zoomed in and hides for tools
    {
        assert_eq!(h.request("engine.execute", json!({"command": "view.shotInfo", "params": {"on": true}}), T)["ok"], true);
        h.settle(T);
        let fit = widget(&h, "loupe:shotInfo").expect("the strip at Fit");
        let img = widget(&h, "canvas:image").expect("the photo");
        assert!(fit.top() >= img.bottom() - 1.0, "at Fit, under the photo: {fit:?} vs {img:?}");
        h.app.ui.zoom = Zoom::Percent(200.0);
        h.settle(T);
        let canvas = h.app.canvas_rect.expect("the canvas");
        let img = widget(&h, "canvas:image").expect("the photo");
        assert!(img.height() > canvas.height(), "zoomed in: {img:?} in {canvas:?}");
        let strip = widget(&h, "loupe:shotInfo").expect("the strip is still there zoomed in");
        assert!(canvas.contains_rect(strip), "on screen: {strip:?} in {canvas:?}");
        // a tool on the pixels: nothing over them
        h.app.ui.tool = "brush".into();
        h.app.ui.right = RightPanel::Masking;
        h.step();
        h.step();
        assert!(widget(&h, "loupe:shotInfo").is_none(), "hidden while a tool is armed");
        h.app.ui.tool.clear();
        h.app.ui.right = RightPanel::Edit;
        h.step();
        h.step();
        assert!(widget(&h, "loupe:shotInfo").is_some(), "back once the tool is put away");
        // View ▸ Shot Info turns it off
        assert_eq!(h.request("engine.execute", json!({"command": "view.shotInfo", "params": {}}), T)["result"]["on"], false);
        h.step();
        h.step();
        assert!(widget(&h, "loupe:shotInfo").is_none());
    }
}

#[test]
fn shot_info_reads_the_metadata_in_order() {
    use crate::panels::detail::shot_info;
    let m = lcc_catalog::Meta {
        aperture: Some(2.8),
        shutter: "1/250".into(),
        iso: Some(1600),
        focal_mm: Some(50.4),
        lens: "RF24-70mm F2.8 L IS USM".into(),
        ..Default::default()
    };
    assert_eq!(shot_info(&m, [8192, 5464]), ["f/2.8", "1/250 s", "ISO 1600", "50 mm", "RF24-70mm F2.8 L IS USM", "8192 × 5464"]);
    let hostile = lcc_catalog::Meta { aperture: Some(f32::NAN), focal_mm: Some(-1.0), shutter: "8s".into(), ..Default::default() };
    assert_eq!(shot_info(&hostile, [1, 1]), ["8s"]);
}

/// The lens's focal range (OneSlips PERF-CULL-7): a lens name that already says it (`RF24-70mm`) stays as it is, one
/// that doesn't gets it in brackets, a file without a lens name shows the range alone; a prime has one number; hostile
/// ranges are left out.
#[test]
fn shot_info_adds_the_lens_range_a_lens_name_lacks() {
    use crate::panels::detail::shot_info;
    let lens = |name: &str, range: Option<(f32, f32)>| {
        let m = lcc_catalog::Meta { lens: name.into(), lens_range: range, ..Default::default() };
        shot_info(&m, [0, 0])
    };
    assert_eq!(lens("RF24-70mm F2.8 L IS USM", Some((24.0, 70.0))), ["RF24-70mm F2.8 L IS USM"]);
    assert_eq!(lens("FE 85mm F1.4 GM", Some((85.0, 85.0))), ["FE 85mm F1.4 GM"]);
    assert_eq!(lens("Art", Some((24.0, 70.0))), ["Art (24–70 mm)"]);
    // a 7 inside 70 is not the range's 7
    assert_eq!(lens("Lens 70", Some((7.0, 14.0))), ["Lens 70 (7–14 mm)"]);
    assert_eq!(lens("", Some((85.0, 85.0))), ["(85 mm)"]);
    assert_eq!(lens("", None), Vec::<String>::new());
    for bad in [(f32::NAN, 70.0), (0.0, 70.0), (70.0, 24.0), (24.0, f32::INFINITY), (-5.0, 1e30)] {
        assert_eq!(lens("Art", Some(bad)), ["Art"], "{bad:?}");
    }
    // the catalog JSON leaves an unknown range out, so older catalogs read unchanged
    let json = serde_json::to_value(lcc_catalog::Meta::default()).expect("json");
    assert!(json.get("lens_range").is_none());
}

#[test]
fn strip_widths_and_film_cells_follow_the_height() {
    use crate::panels::detail::{film_cell_width, film_thumb_px, strip_width};
    assert_eq!(strip_width(&[]), 0.0);
    assert_eq!(strip_width(&[40.0]), 40.0);
    assert_eq!(film_cell_width(142.0), 120.0);
    assert!(film_cell_width(360.0) > 300.0 && film_cell_width(96.0) < 90.0);
    assert_eq!(film_cell_width(f32::NAN), 120.0);
    assert_eq!(film_thumb_px(60.0, f32::NAN), 128);
    assert_eq!(film_thumb_px(300.0, 2.0), 512);
    let s = FILM_HEIGHT;
    assert_eq!((s.clamp(10.0), s.clamp(1e6), s.clamp(f32::NAN)), (s.min, s.max, s.default));
}
