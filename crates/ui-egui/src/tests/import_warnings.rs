//! The import-finish toast stays usable however many photos warn (#3050): warnings that say the
//! same thing about different photos are one counted line, the list is capped with "+N more",
//! a single warning keeps its photo's path, a profile or Upright mode shared by several photos
//! is named once with its count, and a press on the toast dismisses it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{Dialog, ViewMode};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);
/// How long a fixture import may take.
const IMPORT_TIMEOUT: Duration = Duration::from_secs(60);
/// The fixture photos' size in pixels; the red channel steps across and the green per photo, so
/// no two fixtures are the same bytes (an import skips a packet its content hash already has).
const TILE: (usize, usize) = (40, 30);
const RED_STRIDE: usize = 6;
const SHADE_STEP: u8 = 20;
/// The window the headless harness opens: the import review fits in it.
const WINDOW: [f32; 2] = [1300.0, 1400.0];
/// The adjustment every fixture sidecar carries, so each packet states a real edit.
const EXPOSURE: &str = "+0.50";

fn toast(h: &Headless) -> String {
    h.app.ui.toast.clone().map(|t| t.0).unwrap_or_default()
}

/// A folder of distinct PNGs; photo `i` gets its own sidecar with the `crs:` attributes
/// `attrs[i]`, a `{i}` placeholder in each keeping the packets' bytes different (one packet
/// content per import is read, not two), plus the adjustment `crs:Exposure2012`.
fn folder_with<S: AsRef<str>>(tag: &str, attrs: &[S]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-import-warnings-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (i, attrs) in attrs.iter().enumerate() {
        let data: Vec<[u8; 4]> = (0..TILE.0 * TILE.1).map(|k| [(k % TILE.0 * RED_STRIDE) as u8, (i as u8) * SHADE_STEP, 7, 255]).collect();
        let img = lightcraft_raster::Rgba8 { width: TILE.0, height: TILE.1, data };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join(format!("img{i}.png")), png).unwrap();
        let attrs = attrs.as_ref().replace("{i}", &i.to_string());
        let xmp = format!(
            "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
             xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" \
             xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\" {attrs} \
             crs:Exposure2012=\"{EXPOSURE}\"/></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>"
        );
        std::fs::write(dir.join(format!("img{i}.xmp")), xmp).unwrap();
    }
    dir
}

/// The app with an empty library and the import review open on `dir`.
fn review(dir: &Path) -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, WINDOW, 1.0);
    h.app.ui.view = ViewMode::PhotoGrid;
    let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), T);
    assert_eq!(r["result"]["scanning"], true, "{r}");
    assert!(h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "the review opens");
    h.settle(SETTLE);
    h
}

/// Confirm the review, wait for the import to end, and return its finish toast.
fn import_with<S: AsRef<str>>(tag: &str, attrs: &[S]) -> (Headless, PathBuf, String) {
    let dir = folder_with(tag, attrs);
    let mut h = review(&dir);
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    let t0 = std::time::Instant::now();
    while h.app.import.is_some() && t0.elapsed() < IMPORT_TIMEOUT {
        h.step();
    }
    assert!(h.app.import.is_none(), "the import finished");
    h.settle(SETTLE);
    let text = toast(&h);
    (h, dir, text)
}

/// [`import_with`] where photo `i` gets one `crs:` field per name in `fields`, its value
/// carrying `{i}`.
fn import(tag: &str, fields: &[&str]) -> (Headless, PathBuf, String) {
    let attrs: Vec<String> = fields.iter().map(|f| format!("crs:{f}=\"{{i}}\"")).collect();
    import_with(tag, &attrs)
}

/// The same warning about several photos is one counted line, without paths: a batch of N warned
/// files must not cover the viewer with N repeated lines (#3050).
#[test]
fn identical_warnings_are_one_counted_line() {
    let (h, dir, text) = import("same", &["AutoToneDigest", "AutoToneDigest"]);
    assert_eq!(h.app.session.catalog.len(), 2, "both photos imported");
    assert!(text.contains("Import warnings (2):"), "{text}");
    assert!(text.contains("for 2 photos"), "{text}");
    assert!(!text.contains("img0.png") && !text.contains("img1.png"), "the counted line carries no paths: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// One photo with a warning keeps the full per-photo message with its path.
#[test]
fn a_single_warning_keeps_the_photo_path() {
    let (h, dir, text) = import("one", &["AutoToneDigest"]);
    assert_eq!(h.app.session.catalog.len(), 1, "the photo imported");
    assert!(text.contains("Import warnings (1):"), "{text}");
    assert!(text.contains("img0.png"), "the path is the actionability of a single warning: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Different adjustments stay separate lines — only warnings that say the same thing merge.
#[test]
fn different_adjustments_stay_separate_lines() {
    let (h, dir, text) = import("split", &["AutoToneDigest", "ColorVariance"]);
    assert_eq!(h.app.session.catalog.len(), 2, "both photos imported");
    assert!(text.contains("Import warnings (2):"), "{text}");
    assert!(text.contains("AutoToneDigest") && text.contains("ColorVariance"), "{text}");
    assert!(!text.contains("for 2 photos"), "the two lines say different things: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The list is capped with "+N more" however many distinct warnings there are (#3050).
#[test]
fn the_toast_is_capped() {
    let (_, dir, text) = import("cap", &["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8"]);
    assert!(text.contains("Import warnings (8):"), "{text}");
    assert!(text.contains("+2 more"), "8 groups in 6 lines and a count: {text}");
    assert!(!text.contains("F7") && !text.contains("F8"), "the overflow is not listed: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same camera profile on several photos is one counted line naming the profile (#3050).
#[test]
fn one_profile_groups_into_one_counted_line() {
    let (h, dir, text) = import_with(
        "profile",
        &["crs:CameraProfile=\"Camera Faithful\" crs:AutoToneDigest=\"{i}A\"", "crs:CameraProfile=\"Camera Faithful\" crs:AutoToneDigest=\"{i}B\""],
    );
    assert_eq!(h.app.session.catalog.len(), 2, "both photos imported");
    // two warnings per photo: the profile, and the digest that keeps the packets' bytes apart
    assert!(text.contains("Import warnings (4):"), "{text}");
    assert!(text.contains("\"Camera Faithful\"") && text.contains("for 2 photos"), "{text}");
    assert!(!text.contains("img0.png") && !text.contains("img1.png"), "the counted line carries no paths: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The same Upright mode on several photos is one counted line saying it will be recalculated
/// (#3050).
#[test]
fn one_upright_mode_groups_into_one_counted_line() {
    let (h, dir, text) = import_with(
        "upright",
        &["crs:PerspectiveUpright=\"1\" crs:UprightTransform_1=\"a{i}\"", "crs:PerspectiveUpright=\"1\" crs:UprightTransform_1=\"b{i}\""],
    );
    assert_eq!(h.app.session.catalog.len(), 2, "both photos imported");
    assert!(text.contains("Import warnings (2):"), "{text}");
    assert!(text.contains("for 2 photos") && text.contains("recalculated"), "{text}");
    assert!(text.contains("Auto"), "the mode says what will happen instead: {text}");
    assert!(!text.contains("img0.png") && !text.contains("img1.png"), "the counted line carries no paths: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A press over the toast dismisses it, and its rectangle is registered for automation (#3050).
#[test]
fn a_press_on_the_toast_dismisses_it() {
    let (mut h, dir, text) = import("press", &["AutoToneDigest", "AutoToneDigest"]);
    assert!(text.contains("Import warnings (2):"), "{text}");
    assert!(h.app.ui.dialog.is_none(), "the review closed when the import ran");
    let widgets = h.request("ui.widgets", json!({}), T);
    let all = widgets["result"].as_array().expect("widgets");
    let w = all.iter().find(|w| w["id"] == "toast").expect("the toast is registered by its rectangle");
    let rect: Vec<f64> = [0usize, 1, 2, 3].map(|i| w["rect"][i].as_f64().expect("rect")).to_vec();
    let r = h.request("ui.click", json!({"x": rect[0] + rect[2] / 2.0, "y": rect[1] + rect[3] / 2.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(toast(&h), "", "the press on the toast dismissed it");
    let _ = std::fs::remove_dir_all(&dir);
}
