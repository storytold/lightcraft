//! Headless tests of the Temp slider's scale: the Kelvin temperature of a raw follows Lightroom's track
//! (2000–7000 K on the left half, logarithmic up to 50000 K on the right); the relative temperature of a
//! rendered photo and every other slider stay linear.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library in Detail with the Edit panel's Light and Color sections open, on a raw photo (Kelvin
/// white balance) when `raw`, else on a demo JPEG (relative white balance).
fn edit_panel(raw: bool) -> Headless {
    use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = if raw {
        // a DNG raw photo (its file need not exist: the sliders only read its settings)
        let id = PhotoId(h.app.session.catalog.photos().map(|p| p.id.0).max().unwrap_or(0) + 1);
        let path = std::env::temp_dir().join("lc-temp-slider").join("IMG_1.dng");
        let mut photo =
            Photo::new(id, Source::File { path: path.to_string_lossy().into_owned() }, "IMG_1.dng", "DNG", 6000, 4000, "2026-10-01T00:00:00");
        photo.kind = MediaKind::Raw;
        h.app.session.commit("setup", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        id.0
    } else {
        h.app.session.visible_cloned()[0].0
    };
    assert_eq!(h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}), T)["ok"], true);
    assert_eq!(h.request("ui.set", json!({"view": "detail", "right": "edit"}), T)["ok"], true);
    h.app.ui.open_sections = vec!["light".to_string(), "color".to_string()];
    h.settle(SETTLE);
    h
}

fn widget(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// Where the slider's thumb (the hollow ring on the track's centre line) is drawn, from 0 to 1 along the track.
fn thumb(h: &Headless, control: &str) -> f32 {
    let track = widget(h, &format!("slider:{control}"));
    let rings: Vec<f32> = h
        .view
        .painted_shapes()
        .iter()
        .filter_map(|s| match s {
            egui::epaint::Shape::Circle(c) if c.radius == 7.0 && (c.center.y - track.center().y).abs() < 0.5 => Some(c.center.x),
            _ => None,
        })
        .filter(|x| (track.left() - 0.5..=track.right() + 0.5).contains(x))
        .collect();
    assert!(!rings.is_empty() && rings.iter().all(|x| (x - rings[0]).abs() < 0.01), "one thumb on {control}: {rings:?}");
    (rings[0] - track.left()) / track.width()
}

fn develop(h: &Headless) -> lightcraft_develop::DevelopSettings {
    let id = h.app.session.selection.active.unwrap();
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn set(h: &mut Headless, control: &str, value: f64) {
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": control, "value": value}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

/// The thumb sits where Lightroom puts it (within 1 % of the model fitted to Lightroom's positions), clicking or
/// dragging to the middle of the track gives 7000 K, and the far end is still 50000 K.
#[test]
fn the_kelvin_temp_slider_has_lightrooms_scale() {
    let mut h = edit_panel(true);
    for (kelvin, model) in [(3650.0, 0.165), (5400.0, 0.34), (7000.0, 0.5), (15000.0, 0.79), (20000.0, 0.849)] {
        set(&mut h, "wb.temp", kelvin);
        assert_eq!(develop(&h).wb.temp, kelvin);
        let at = thumb(&h, "wb.temp");
        assert!((at - model).abs() < 0.01, "{kelvin} K drawn at {at}, the model says {model}");
    }
    set(&mut h, "wb.temp", 4850.0);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "slider:wb.temp", "fx": 0.5}), T)["ok"], true);
    h.settle(SETTLE);
    assert_eq!(develop(&h).wb.temp, 7000.0, "a click in the middle of the track");
    // drag from As Shot on a Z 6II NEF (about 4850 K, 28 % along the track) to the middle
    set(&mut h, "wb.temp", 4850.0);
    let track = widget(&h, "slider:wb.temp");
    let from = track.left() + thumb(&h, "wb.temp") * track.width();
    let r = h.request("ui.drag", json!({"x": from, "y": track.center().y, "toX": track.center().x, "toY": track.center().y, "steps": 12}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let k = develop(&h).wb.temp;
    assert!((k - 7000.0).abs() <= 50.0, "dragged to the middle: {k} K");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "slider:wb.temp", "fx": 1.0}), T)["ok"], true);
    h.settle(SETTLE);
    assert_eq!(develop(&h).wb.temp, 50000.0);
}

/// Exposure (like every slider but the Kelvin temperature) keeps its linear track.
#[test]
fn other_sliders_stay_linear() {
    let mut h = edit_panel(true);
    set(&mut h, "light.exposure", 1.0);
    assert!((thumb(&h, "light.exposure") - 0.6).abs() < 0.005, "+1.00 of −5…+5 at 60 %");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "slider:light.exposure", "fx": 0.75}), T)["ok"], true);
    h.settle(SETTLE);
    assert!((develop(&h).light.exposure - 2.5).abs() < 0.011, "{}", develop(&h).light.exposure);
}

/// The relative temperature of a rendered photo (−100…+100, 0 = as shot, in the middle) is unchanged.
#[test]
fn the_relative_temp_slider_stays_linear() {
    let mut h = edit_panel(false);
    assert!(h.app.widgets.iter().all(|(w, _)| w != "slider:wb.temp"), "a JPEG has the relative scale");
    assert!((thumb(&h, "wb.tempRel") - 0.5).abs() < 0.005, "as shot in the middle");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "slider:wb.tempRel", "fx": 0.75}), T)["ok"], true);
    h.settle(SETTLE);
    assert!((thumb(&h, "wb.tempRel") - 0.75).abs() < 0.005, "the thumb where it was clicked");
    // +50 on the relative scale: 40 mired warmer than 6500 K
    let k = develop(&h).wb.temp;
    let rel = (1e6 / 6500.0 - 1e6 / k) / 0.8;
    assert!((rel - 50.0).abs() < 0.6, "{k} K is {rel} on the relative scale");
}
