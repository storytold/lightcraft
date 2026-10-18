//! The Profile section's Calibrated camera colour option: shown for a raw whose camera model has measured colour,
//! it switches the photo between the look fitted to the camera's JPEG (Auto) and the measured colour
//! (Colorimetric), and the next decode uses it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_develop::CameraColour;
use lightcraft_engine::media::{FileLoader, LoadOptions};
use lightcraft_pipeline::SourceInfo;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The Edit panel on one raw whose decoder says whether measured colour is available; the decodes' options.
fn edit_panel(colorimetric: bool) -> (Headless, Arc<Mutex<Vec<LoadOptions>>>) {
    let mut session = lightcraft_engine::Session::new();
    let p = Photo::new(PhotoId(1), Source::File { path: "/photos/DSC_0001.NEF".into() }, "DSC_0001.NEF", "NEF", 6048, 4032, "2026-10-18");
    session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let loader: FileLoader = Arc::new(move |_, _, options| {
        log.lock().unwrap().push(options);
        let mut img = lightcraft_raster::Rgb32f::new(48, 32);
        img.data.fill([0.18; 3]);
        Ok((img, SourceInfo { raw: true, colorimetric, ..SourceInfo::default() }))
    });
    session.media.file_loader = Some(loader);
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
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
