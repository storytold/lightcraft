//! Headless tests of the Import Warnings window: an import whose XMP sidecars carry settings from
//! another editor says so in one toast line, and the window groups, selects and saves them.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn xmp(fields: &str) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0.5" {fields}/>
    </rdf:RDF></x:xmpmeta>"#
    )
}

#[test]
fn import_warnings_are_one_toast_line_and_a_window_that_selects_and_saves() {
    let dir = lcc_testkit::TempDir::new("lc-ui-import-warnings");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..3u8 {
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[i * 40, 3, 9, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join(format!("IMG_{i:02}.png")), png).unwrap();
    }
    // two photos with a profile the app doesn't have; one also kept Lightroom's Upright analysis
    // with Upright Off (nothing lost, so no warning)
    std::fs::write(
        dir.join("IMG_00.xmp"),
        xmp(r#"crs:CameraProfile="Camera Portrait" crs:PerspectiveUpright="0" crs:UprightTransformCount="6" crs:UprightTransform_1="0.9,0,0,0,0.95,0,0,0,1""#),
    )
    .unwrap();
    std::fs::write(dir.join("IMG_01.xmp"), xmp(r#"crs:CameraProfile="Camera Portrait""#)).unwrap();

    let services = Services { png: None, ..Default::default() };
    let mut h = Headless::new(LccApp::new(lcc_engine::Session::with_demo(), services), [1300.0, 900.0], 1.0);
    let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), T);
    assert_eq!(r["result"]["scanning"], true, "{r}");
    h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
    assert_eq!(h.request("ui.dialog.confirm", json!({}), T)["ok"], true);
    // (the toast is read as soon as the import ends: it fades in a few seconds)
    h.step_until(SETTLE, |h| h.app.import.is_none() && h.app.ui.toast().is_some());

    let toast = h.app.ui.toast().cloned().expect("a toast").0;
    assert!(!toast.contains('\n') && !toast.contains("IMG_0"), "one line, no file list: {toast}");
    let report = h.app.import_report.as_ref().expect("the warnings are kept");
    assert!(report.open, "a chosen import opens the window");
    assert_eq!(report.photos(), 2, "{}", report.text());
    assert_eq!(report.groups.len(), 1, "one reason, no Upright warning: {}", report.text());
    assert!(report.groups[0].title.contains("Camera Portrait"));

    // Select These Photos: both photos, in All Photos
    h.settle(Duration::from_millis(300));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:importWarnings.select.0"}), T)["ok"], true);
    h.settle(Duration::from_millis(300));
    let names: Vec<String> =
        h.app.session.selection.ids.iter().filter_map(|id| h.app.session.catalog.photo(*id)).map(|p| p.file_name.clone()).collect();
    let mut names = names;
    names.sort();
    assert_eq!(names, ["IMG_00.png", "IMG_01.png"]);

    // the full list saves as text
    let out = dir.join("report.txt");
    let r = h.request("engine.execute", json!({"command": "file.saveImportWarnings", "params": {"path": out.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("IMG_00.png") && text.contains("IMG_01.png") && !text.contains("IMG_02.png"), "{text}");

    // Close, and File ▸ More Imports ▸ Import Warnings… opens it again
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:importWarnings.close"}), T)["ok"], true);
    h.settle(Duration::from_millis(300));
    assert!(!h.app.import_report.as_ref().unwrap().open);
    let r = h.request("engine.execute", json!({"command": "dialog.importWarnings", "params": {}}), T);
    assert_eq!(r["result"]["photos"], 2, "{r}");
    assert!(h.app.import_report.as_ref().unwrap().open);
    let _ = std::fs::remove_dir_all(&dir);
}
