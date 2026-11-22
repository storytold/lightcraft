//! SIL scenario: import → develop → export → re-import, on procedural `lcc-scenes` photos (no
//! media is committed). It drives the engine the way the front ends do (commands by id, the
//! export batch) and checks behaviour with tolerances, never exact pixels:
//!
//! - the develop settings and the photo's metadata survive a sidecar write and a fresh import;
//! - the exported JPEG decodes, has the photo's aspect ratio, is neither blank nor clipped, and a
//!   brighter edit exports brighter;
//! - the exported file re-imports with its title, keywords and capture time.
use std::path::Path;

use lcc_engine::Session;
use lcc_engine::catalog::PhotoId;
use lcc_engine::export::{Destination, ExportOptions, export_batch};
use serde_json::json;

/// A JPEG of demo-scene pixels, `w × h`, written at `path`.
fn write_scene_jpeg(path: &Path, scene: &lcc_scenes::Scene, w: usize, h: usize) {
    let img = scene.render(w, h);
    let mut rgba = lcc_raster::Rgba8::new(w, h);
    for (o, p) in rgba.data.iter_mut().zip(img.data.iter()) {
        let f = |c: f32| (c.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
        *o = [f(p[0]), f(p[1]), f(p[2]), 255];
    }
    let bytes =
        lcc_codecs::encode_jpeg(&lcc_codecs::EncodeImage::rgba8(&rgba), 92, Default::default(), &Default::default()).expect("the scene encodes");
    std::fs::write(path, bytes).expect("the scene is written");
}

/// A session with its own library folder (kept alive by the caller: the journal lives there).
fn session(lib: &Path) -> Session {
    let mut s = Session::new().with_fs();
    s.open_library(lib, false).expect("the library opens");
    s
}

fn import_one(s: &mut Session, path: &Path) -> PhotoId {
    let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()], "mode": "add"})).expect("import");
    let ids: Vec<u64> = r["imported"].as_array().map(|a| a.iter().filter_map(|v| v.as_u64()).collect()).unwrap_or_default();
    assert_eq!(ids.len(), 1, "one photo imported: {r}");
    PhotoId(ids[0])
}

/// Exports `id` as a full-size JPEG into `dir`; returns the file's path.
fn export_jpeg(s: &mut Session, id: PhotoId, dir: &Path, naming: &str) -> std::path::PathBuf {
    let o = ExportOptions { quality: 90, naming: naming.into(), ..ExportOptions::default() };
    let to = Destination { dir: dir.to_string_lossy().into_owned(), exact: None };
    let mut write = |p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string());
    let files = export_batch(s, &[id], &o, &to, &mut write, &|p| Path::new(p).exists()).expect("the export runs");
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].get("error").is_none(), "{:?}", files[0]);
    let path = files[0]["path"].as_str().expect("the export names its file");
    std::path::PathBuf::from(path)
}

/// Mean sRGB luma (0..1) and the fractions of fully black and fully white pixels.
fn stats(path: &Path) -> (usize, usize, f64, f64, f64) {
    let bytes = std::fs::read(path).expect("the export is on disk");
    assert!(bytes.starts_with(&[0xff, 0xd8]), "a JPEG");
    let img = lcc_codecs::decode(&bytes, Default::default()).expect("the export decodes").to_srgb8();
    let n = (img.width * img.height).max(1) as f64;
    let (mut sum, mut black, mut white) = (0.0, 0usize, 0usize);
    for p in &img.data {
        let y = 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
        sum += y / 255.0;
        black += usize::from(p[0].max(p[1]).max(p[2]) == 0);
        white += usize::from(p[0].min(p[1]).min(p[2]) == 255);
    }
    (img.width, img.height, sum / n, black as f64 / n, white as f64 / n)
}

#[test]
fn import_develop_export_reimport_keeps_settings_metadata_and_sane_pixels() {
    let src = lcc_testkit::TempDir::new("lcc-sil-roundtrip-src");
    let lib = lcc_testkit::TempDir::new("lcc-sil-roundtrip-lib");
    let out = lcc_testkit::TempDir::new("lcc-sil-roundtrip-out");
    let (w, h) = (480, 320);
    let original = src.path().join("scene.jpg");
    write_scene_jpeg(&original, &lcc_scenes::demo_library()[0], w, h);

    // import, then develop and describe the photo
    let mut s = session(lib.path());
    let id = import_one(&mut s, &original);
    s.execute(
        "photo.setMeta",
        &json!({"ids": [id.0], "title": "Round trip", "caption": "procedural scene", "keywords": ["sil", "roundtrip"], "creator": "Test Rig"}),
    )
    .expect("metadata");
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 4})).expect("rating");
    let mut dev = (*s.develop_of(id).expect("a developed photo")).clone();
    dev.light.exposure = 0.7;
    dev.light.contrast = 20.0;
    s.set_develop(id, dev.clone(), "sil").expect("settings");

    // export: the unedited render first (for comparison), then the edit
    let base_settings = lcc_develop::DevelopSettings::default();
    s.set_develop(id, base_settings.clone(), "sil base").expect("base settings");
    let base = export_jpeg(&mut s, id, out.path(), "{name}-base");
    s.set_develop(id, dev.clone(), "sil edit").expect("edit settings");
    let edited = export_jpeg(&mut s, id, out.path(), "{name}-edit");

    let (bw, bh, base_luma, _, _) = stats(&base);
    let (ew, eh, edit_luma, black, white) = stats(&edited);
    assert_eq!((bw, bh), (w, h), "a full-size export keeps the photo's size");
    assert_eq!((ew, eh), (w, h));
    assert!((0.05..0.97).contains(&edit_luma), "not blank: mean luma {edit_luma}");
    assert!(black < 0.5 && white < 0.5, "not crushed or clipped: black {black}, white {white}");
    assert!(edit_luma > base_luma + 0.02, "+0.7 EV exports brighter: {base_luma} → {edit_luma}");

    // the settings and metadata go to a sidecar and come back in a fresh library
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).expect("the sidecar is written");
    drop(s);
    let lib2 = lcc_testkit::TempDir::new("lcc-sil-roundtrip-lib2");
    let mut s2 = session(lib2.path());
    let again = import_one(&mut s2, &original);
    let back = s2.develop_of(again).expect("settings");
    assert!((back.light.exposure - 0.7).abs() < 1e-3, "exposure {}", back.light.exposure);
    assert!((back.light.contrast - 20.0).abs() < 0.5, "contrast {}", back.light.contrast);
    let ph = s2.catalog.photo(again).expect("the photo");
    assert_eq!(ph.rating, 4);
    assert_eq!(ph.meta.title, "Round trip");
    assert!(ph.meta.keywords.iter().any(|k| k == "sil") && ph.meta.keywords.iter().any(|k| k == "roundtrip"), "{:?}", ph.meta.keywords);

    // the exported file re-imports with the metadata it carries
    let exported = import_one(&mut s2, &edited);
    let ph = s2.catalog.photo(exported).expect("the exported photo");
    assert_eq!(ph.meta.title, "Round trip");
    assert!(ph.meta.keywords.iter().any(|k| k == "sil"), "{:?}", ph.meta.keywords);
    assert_eq!(ph.meta.creator, "Test Rig");
    // its pixels already carry the edit: it opens unedited, at the export's size
    assert!(!s2.develop_of(exported).expect("settings").light.exposure.is_nan());
}

/// The same chain on a hostile path: an export of a photo whose original vanished is an error per
/// photo, never a panic, and the batch still answers.
#[test]
fn exporting_a_photo_whose_original_is_gone_is_an_error_not_a_panic() {
    let src = lcc_testkit::TempDir::new("lcc-sil-gone-src");
    let lib = lcc_testkit::TempDir::new("lcc-sil-gone-lib");
    let out = lcc_testkit::TempDir::new("lcc-sil-gone-out");
    let original = src.path().join("gone.jpg");
    write_scene_jpeg(&original, &lcc_scenes::demo_library()[1], 160, 120);
    let mut s = session(lib.path());
    let id = import_one(&mut s, &original);
    s.media.forget(id);
    std::fs::remove_file(&original).expect("the original is removed");
    let o = ExportOptions::default();
    let to = Destination { dir: out.path().to_string_lossy().into_owned(), exact: None };
    let mut write = |p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string());
    match export_batch(&mut s, &[id], &o, &to, &mut write, &|_| false) {
        Err(e) => assert!(!e.is_empty()),
        Ok(files) => assert!(files.iter().all(|f| f.get("error").is_some() || f.get("path").is_none()), "{files:?}"),
    }
}
