//! The Profile section's Calibrated camera colour option: shown for a raw whose camera model has measured colour,
//! it switches the photo between the look fitted to the camera's JPEG (Auto) and the measured colour
//! (Colorimetric), and the next decode uses it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lcc_catalog::{Op, Photo, PhotoId, Source};
use lcc_develop::CameraColour;
use lcc_engine::media::{FileLoader, LoadOptions, SourceLevel};
use lcc_pipeline::SourceInfo;
use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The Edit panel on one raw whose decoder says whether measured colour is available; the decodes' options.
fn edit_panel(colorimetric: bool) -> (Headless, Arc<Mutex<Vec<LoadOptions>>>) {
    let mut session = lcc_engine::Session::new();
    let p = Photo::new(PhotoId(1), Source::File { path: "/photos/DSC_0001.NEF".into() }, "DSC_0001.NEF", "NEF", 6048, 4032, "2026-10-18");
    session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let loader: FileLoader = Arc::new(move |_, _, options| {
        log.lock().unwrap().push(options);
        let mut img = lcc_raster::Rgb32f::new(48, 32);
        img.data.fill([0.18; 3]);
        Ok((img, SourceInfo { raw: true, colorimetric, ..SourceInfo::default() }))
    });
    session.media.file_loader = Some(loader);
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    for (method, params) in
        [("engine.execute", json!({"command": "library.select", "params": {"ids": [1]}})), ("ui.set", json!({"view": "detail", "right": "edit"}))]
    {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    (h, seen)
}

fn shown(h: &Headless) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == "check:profile.cameraColour")
}

fn colour(h: &Headless) -> CameraColour {
    h.app.session.catalog.photo(PhotoId(1)).unwrap().develop.profile.camera_colour
}

#[test]
fn calibrated_camera_colour_switches_the_photo_and_its_decode() {
    let (mut h, seen) = edit_panel(true);
    assert!(shown(&h), "offered for a raw with measured colour");
    assert_eq!(colour(&h), CameraColour::Auto);
    let r = h.request("ui.clickWidget", json!({"id": "check:profile.cameraColour"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(colour(&h), CameraColour::Colorimetric);
    assert_eq!(seen.lock().unwrap().last().map(|o| o.colour), Some(CameraColour::Colorimetric), "decoded again with it");
    // one undo step back to Auto
    let r = h.request("engine.execute", json!({"command": "edit.undo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(colour(&h), CameraColour::Auto);
    assert!(shown(&h), "still offered");
}

#[test]
fn calibrated_camera_colour_is_not_offered_without_measured_colour() {
    let (h, _) = edit_panel(false);
    assert!(!shown(&h));
}

fn rect_of(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

#[test]
fn calibrated_camera_colour_stays_while_the_photo_is_decoded_again() {
    // the row used to follow the cached decodes: a source decoded again (an edit, crop mode) or one without the
    // decoder's facts hid it for a frame or more, and every control under it, the slider being dragged among them,
    // jumped up and back
    let (mut h, _) = edit_panel(true);
    assert!(shown(&h));
    let below = rect_of(&h, "slider:light.exposure").expect("the Exposure slider");
    let plain = Arc::new(lcc_raster::Rgb32f::new(48, 32));
    for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
        h.app.session.media.insert(PhotoId(1), level, plain.clone());
    }
    h.step();
    assert!(shown(&h), "still offered while the decodes carry no facts");
    assert_eq!(rect_of(&h, "slider:light.exposure"), Some(below), "the controls under it stay put");
}

#[test]
fn contrast_mode_segmented_switch_toggles_between_luminance_and_rgb() {
    use lcc_develop::ContrastMode;
    let (mut h, _) = edit_panel(false);
    let mode = |h: &Headless| h.app.session.catalog.photo(PhotoId(1)).unwrap().develop.light.contrast_mode;
    assert_eq!(mode(&h), ContrastMode::Luminance);
    assert!(rect_of(&h, "button:contrastMode-luminance").is_some());
    assert!(rect_of(&h, "button:contrastMode-rgb").is_some());
    let r = h.request("ui.clickWidget", json!({"id": "button:contrastMode-rgb"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(mode(&h), ContrastMode::Rgb);
    let r = h.request("engine.execute", json!({"command": "edit.undo"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(mode(&h), ContrastMode::Luminance);
}
