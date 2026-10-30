//! "Re-read sidecars changed by other apps" (issue #3175) through the frame tick: a sidecar another app
//! changes is read once it has been still, the thumbnail source (the catalog) and the Develop panel follow,
//! and a quiet note says so.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(60);

/// A foreign sidecar: a rating and an exposure.
const FOREIGN: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmp:Rating="4" crs:Exposure2012="+1.00"/></rdf:RDF></x:xmpmeta>"#;

fn library(tag: &str) -> (Headless, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-ui-sidecar-watch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let data: Vec<[u8; 4]> = (0..40 * 24).map(|i| [(i % 40 * 6) as u8, (i / 40 * 9) as u8, 90, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: 40, height: 24, data };
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(dir.join("a.png"), png).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    (h, dir)
}

/// Run frames for `secs` of the simulated clock.
fn run_for(h: &mut Headless, secs: f64) {
    let end = h.time() + secs;
    while h.time() < end {
        h.step();
    }
}

fn toast(h: &Headless) -> Option<String> {
    h.app.ui.toast.as_ref().map(|t| t.0.clone())
}

#[test]
fn a_sidecar_another_app_changed_is_read_and_said_quietly() {
    let (mut h, dir) = library("on");
    let id = h.app.session.catalog.photos().next().unwrap().id;
    h.app.run("library.xmpPreferences", json!({"rereadChanged": true})).unwrap();
    // the first listing only records what is there
    run_for(&mut h, 4.0);
    h.app.ui.toast = None;
    let undo = h.app.session.undo.len();
    std::fs::write(dir.join("a.xmp"), FOREIGN).unwrap();
    assert!(h.step_until(T, |h| toast(h).is_some()), "no note");
    assert_eq!(toast(&h).as_deref(), Some("Updated 1 photo from XMP sidecars"));
    let p = h.app.session.catalog.photo(id).unwrap();
    assert_eq!((p.rating, p.develop.light.exposure), (4, 1.0));
    assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step");
    assert_eq!(std::fs::read_to_string(dir.join("a.xmp")).unwrap(), FOREIGN, "not written back");
    h.app.ui.toast = None;
    run_for(&mut h, 8.0);
    assert_eq!(toast(&h), None, "read once");
    assert_eq!(h.app.session.undo.len(), undo + 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn nothing_is_read_with_the_setting_off() {
    let (mut h, dir) = library("off");
    let id = h.app.session.catalog.photos().next().unwrap().id;
    run_for(&mut h, 4.0);
    std::fs::write(dir.join("a.xmp"), FOREIGN).unwrap();
    run_for(&mut h, 10.0);
    assert_eq!(h.app.session.catalog.photo(id).unwrap().rating, 0);
    assert_eq!(toast(&h), None);
    assert!(!h.app.tasks.is_running("Sidecar Scan"));
    let _ = std::fs::remove_dir_all(dir);
}
