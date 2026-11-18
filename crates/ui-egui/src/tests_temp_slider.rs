//! Headless tests of the Temp slider's scale: the Kelvin temperature of a raw follows Lightroom's track
//! (2000–7000 K on the left half, logarithmic up to 50000 K on the right); the relative temperature of a
//! rendered photo and every other slider stay linear.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library in Detail with the Edit panel's Light and Color sections open, on a raw photo (Kelvin
/// white balance) when `raw`, else on a demo JPEG (relative white balance).
fn edit_panel(raw: bool) -> Headless {
    use lcc_catalog::{MediaKind, Op, Photo, PhotoId, Source};
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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
            egui::epaint::Shape::Circle(c) if c.stroke.width > 0.0 && (c.center.y - track.center().y).abs() < 0.5 => Some(c.center.x),
            _ => None,
        })
        .filter(|x| (track.left() - 0.5..=track.right() + 0.5).contains(x))
        .collect();
    assert!(!rings.is_empty() && rings.iter().all(|x| (x - rings[0]).abs() < 0.01), "one thumb on {control}: {rings:?}");
    (rings[0] - track.left()) / track.width()
}

fn develop(h: &Headless) -> lcc_develop::DevelopSettings {
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
    // a quick drag: the thumb keeps up with the pointer
    let r = h.request("ui.drag", json!({"x": from, "y": track.center().y, "toX": track.center().x, "toY": track.center().y, "steps": 2}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let k = develop(&h).wb.temp;
    assert!((k - 7000.0).abs() <= 50.0, "dragged to the middle: {k} K");
    // a slow one too: the thumb stays under the pointer, as in Lightroom Classic (Shift is the fine drag)
    set(&mut h, "wb.temp", 4850.0);
    let r = h.request("ui.drag", json!({"x": from, "y": track.center().y, "toX": track.center().x, "toY": track.center().y, "steps": 60}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let k = develop(&h).wb.temp;
    assert!((k - 7000.0).abs() <= 50.0, "a slow drag to the middle reaches it: {k} K");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "slider:wb.temp", "fx": 1.0}), T)["ok"], true);
    h.settle(SETTLE);
    assert_eq!(develop(&h).wb.temp, 50000.0);
}

/// The relative temperature of a rendered photo (−100…+100, 0 = as shot, in the middle) is unchanged.
#[test]
fn the_relative_temp_slider_stays_linear_in_every_slider_layout_and_text_size() {
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
    // two-row sliders (Settings ▸ Interface, issue #346): the track runs under the name and value, longer than on
    // one row, and clicking works as on one row
    let one = widget(&h, "slider:light.exposure");
    h.app.ui.settings.slider_layout = crate::theme::SliderLayout::TwoRows;
    h.settle(SETTLE);
    let track = widget(&h, "slider:light.exposure");
    let (name, value) = (widget(&h, "sliderLabel:light.exposure"), widget(&h, "sliderValue:light.exposure"));
    assert!(track.width() > one.width() + 40.0, "the track got longer: {one:?} → {track:?}");
    assert!(name.bottom() <= track.top() + 0.5 && value.bottom() <= track.top() + 0.5, "name and value above the track");
    assert!(name.right() <= value.left(), "the name beside the value");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "slider:light.exposure", "fx": 0.75}), T)["ok"], true);
    h.settle(SETTLE);
    assert!((develop(&h).light.exposure - 2.5).abs() < 0.06, "a click three quarters along: {}", develop(&h).light.exposure);
    assert!((thumb(&h, "light.exposure") - 0.75).abs() < 0.01, "the thumb where it was clicked");
    // back to one row: the track as before
    h.app.ui.settings.slider_layout = crate::theme::SliderLayout::OneRow;
    h.settle(SETTLE);
    assert_eq!(widget(&h, "slider:light.exposure").width(), one.width());
    // Full Size slider text (Settings ▸ Interface): the name and value get more room on one row and on two, and
    // the slider still works; Compact gives the layout back
    // (a pill is all name and value, and a click on it never sets it: `tests_pill_slider`)
    for layout in [crate::theme::SliderLayout::OneRow, crate::theme::SliderLayout::TwoRows] {
        h.app.ui.settings.slider_layout = layout;
        h.app.ui.settings.slider_text = crate::theme::SliderText::Compact;
        h.settle(SETTLE);
        let (name, value) = (widget(&h, "sliderLabel:light.exposure"), widget(&h, "sliderValue:light.exposure"));
        h.app.ui.settings.slider_text = crate::theme::SliderText::FullSize;
        h.settle(SETTLE);
        let (big_name, big_value) = (widget(&h, "sliderLabel:light.exposure"), widget(&h, "sliderValue:light.exposure"));
        assert!(big_value.width() > value.width(), "{layout:?}: the value got wider: {value:?} → {big_value:?}");
        assert!(big_name.width() * big_name.height() > name.width() * name.height(), "{layout:?}: the name got more room");
        assert_eq!(h.request("ui.clickWidget", json!({"id": "slider:light.exposure", "fx": 0.75}), T)["ok"], true);
        h.settle(SETTLE);
        assert!((develop(&h).light.exposure - 2.5).abs() < 0.06, "{layout:?}: a click three quarters along: {}", develop(&h).light.exposure);
        h.app.ui.settings.slider_text = crate::theme::SliderText::Compact;
        h.settle(SETTLE);
        assert_eq!(widget(&h, "sliderValue:light.exposure").width(), value.width(), "{layout:?}: compact again");
    }
}
