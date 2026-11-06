//! Headless tests of the Photo Merge dialog's preview: click to zoom in, drag to pan, click to fit.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(120);

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn press(h: &mut Headless, pos: egui::Pos2, pressed: bool) {
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
}

fn click_at(h: &mut Headless, pos: egui::Pos2) {
    h.app.synthetic.push(egui::Event::PointerMoved(pos));
    press(h, pos, true);
    press(h, pos, false);
    h.settle(T);
}

#[test]
fn a_click_on_the_merge_preview_zooms_in_and_a_drag_pans() {
    let dir = std::env::temp_dir().join(format!("lc-merge-zoom-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for (i, bytes) in lightcraft_merge::synth::bracket_dngs(1600, 1200, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, bytes).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut session = lightcraft_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    session.execute("library.select", &json!({"ids": ids})).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    crate::merge::open(&mut h.app, "merge.hdr").unwrap();
    assert!(h.step_until(T, |h| h.app.merge.preview.is_some() && h.app.merge.preview_task.is_none()), "the preview rendered");
    h.settle(T);
    assert_eq!(h.app.merge.zoom, None, "the preview opens fitted");

    let area = rect(&h, "merge.preview");
    // right of and above the middle: a point of the fitted image whatever its aspect
    let at = area.center() + egui::vec2(area.width() * 0.2, area.height() * -0.1);
    click_at(&mut h, at);
    let (u, v) = h.app.merge.zoom.expect("the click zoomed in");
    assert!(u > 0.55 && v < 0.5, "zoomed in on the point clicked ({u}, {v})");

    // drag left: the image follows the pointer, so a point further right comes to the middle
    let from = area.center();
    let to = from - egui::vec2(60.0, 0.0);
    h.app.synthetic.push(egui::Event::PointerMoved(from));
    press(&mut h, from, true);
    h.step();
    for i in 1..=6 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    press(&mut h, to, false);
    h.settle(T);
    let (u2, v2) = h.app.merge.zoom.expect("still zoomed in after a drag");
    assert!(u2 > u + 0.01 && (v2 - v).abs() < 1e-3, "the drag panned right ({u}, {v}) → ({u2}, {v2})");

    click_at(&mut h, area.center());
    assert_eq!(h.app.merge.zoom, None, "a second click fits the preview again");
    let _ = std::fs::remove_dir_all(dir);
}
