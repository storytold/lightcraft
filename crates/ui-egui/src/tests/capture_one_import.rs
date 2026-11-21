//! Issue #4250: File ▸ Import Capture One Catalog…: the menu item, the catalog's folder standing
//! for its database, and windows worded for Capture One. What an import brings into the library
//! is tested in the engine on synthetic catalogs; the database here is not one, so the import
//! fails to read it: only its place and the wording matter.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// A headless screenshot into `$LIGHTCRAFT_TEST_SHOTS`, when it is set: in English, and in German
/// (which translates these windows; the language is this test thread's own).
fn shots(h: &mut Headless, name: &str) {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") else { return };
    for (language, suffix) in [("app.language.german", "de"), ("app.language.english", "en")] {
        h.app.run(language, json!({})).unwrap();
        h.step();
        h.step();
        let path = Path::new(&dir).join(format!("{name}-{suffix}.png"));
        let r = h.request("ui.screenshot", json!({"path": path.to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
}

fn names(dir: &Path) -> Vec<std::ffi::OsString> {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    v.sort();
    v
}

/// An app with a library of demo photos open, and `Shoot.cocatalog/Shoot.cocatalogdb` beside it.
fn setup(name: &str) -> (Headless, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-ui-c1-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let package = dir.join("Shoot.cocatalog");
    std::fs::create_dir_all(package.join("Originals")).unwrap();
    std::fs::write(package.join("Shoot.cocatalogdb"), b"not really SQLite").unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("LightCraft Library"), true).unwrap();
    assert!(!session.catalog.is_empty());
    // (the encoder and the writer are for the screenshots `LIGHTCRAFT_TEST_SHOTS` asks for)
    let png = |img: &lightcraft_raster::Rgba8| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let services = Services {
        png: Some(Box::new(png)),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        write_new: Some(Box::new(lightcraft_engine::export::write_file_new)),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(session, services), [1200.0, 800.0], 1.0);
    h.step();
    (h, dir, package)
}

#[test]
fn the_menu_item_asks_where_the_catalog_goes_and_words_the_import_for_capture_one() {
    let (mut h, dir, package) = setup("ask");
    let database = package.join("Shoot.cocatalogdb");
    let before = names(&package);
    let menu = h.request("ui.menu.list", json!({}), T).to_string();
    assert!(menu.contains("file.importCaptureOne") && menu.contains("Import Capture One Catalog…"), "the File menu lists it");

    // the catalog's folder is given: the window is about its database
    let r = h.app.run("file.importCaptureOne", json!({"path": package.to_string_lossy()})).unwrap();
    assert_eq!((r["asking"].as_bool(), r["path"].as_str()), (Some(true), Some(database.to_string_lossy().as_ref())), "{r}");
    h.step();
    let Some(Dialog::LightroomDestination { catalog, name, .. }) = &h.app.ui.dialog else { panic!("{:?}", h.app.ui.dialog) };
    assert_eq!((Path::new(catalog), name.as_str()), (database.as_path(), "Shoot"));
    assert!(crate::lightroom_destination::is_capture_one(catalog));
    shots(&mut h, "capture-one-destination");

    // Merge: the import starts, worded for Capture One, and fails on this database that isn't one
    assert_eq!(h.request("ui.clickWidget", json!({"id": "radio:lightroomDestination-open"}), T)["ok"], true);
    h.step();
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T)["ok"], true);
    h.step();
    assert!(h.step_until(T, |h| h.app.lightroom.is_none()));
    let Some(Dialog::LightroomResult { summary, capture_one, first, .. }) = &h.app.ui.dialog else { panic!("{:?}", h.app.ui.dialog) };
    assert!(*capture_one && first.is_none());
    assert!(summary.starts_with("Capture One import failed: "), "{summary}");
    assert!(summary.contains("not a readable Capture One catalog"), "{summary}");
    assert!(!summary.contains("Lightroom"), "a Capture One import is not worded as Lightroom's: {summary}");
    shots(&mut h, "capture-one-result");
    assert_eq!(h.app.lightroom_last.clone().unwrap_or_default()["path"], json!(database));
    assert_eq!(names(&package), before, "the catalog is only read");
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_folder_that_is_not_one_catalog_is_refused_before_any_window_opens() {
    let (mut h, dir, package) = setup("refuse");
    let empty = dir.join("Empty.cocatalog");
    std::fs::create_dir_all(&empty).unwrap();
    let e = h.app.run("file.importCaptureOne", json!({"path": empty.to_string_lossy()})).unwrap_err();
    assert!(e.contains("no Capture One catalog database (.cocatalogdb) found"), "{e}");
    std::fs::write(package.join("Other.cocatalogdb"), b"x").unwrap();
    std::fs::rename(package.join("Shoot.cocatalogdb"), package.join("First.cocatalogdb")).unwrap();
    let e = h.app.run("file.importCaptureOne", json!({"path": package.to_string_lossy()})).unwrap_err();
    assert!(e.contains("more than one catalog database (First.cocatalogdb, Other.cocatalogdb)"), "{e}");
    assert!(h.app.ui.dialog.is_none() && h.app.lightroom.is_none());
    // a Lightroom catalog keeps its own wording
    assert!(!crate::lightroom_destination::is_capture_one("/x/Travel.lrcat"));
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

/// Opt-in: import the real catalog named by `LIGHTCRAFT_C1_CATALOG` through the menu command
/// into a scratch library. Nothing about the catalog's content is asserted or kept.
#[test]
fn a_real_catalog_imports_through_the_menu_command() {
    let Some(path) = std::env::var_os("LIGHTCRAFT_C1_CATALOG").filter(|p| !p.is_empty()) else {
        lightcraft_testkit::skip!(Input, "set LIGHTCRAFT_C1_CATALOG to a Capture One catalog to import it");
        return;
    };
    let (mut h, dir, _) = setup("real");
    let before = h.app.session.catalog.len();
    let started = h.app.run("file.importCaptureOne", json!({"path": path.to_string_lossy(), "into": "open"})).unwrap();
    assert_eq!(started["running"], true, "{started}");
    assert!(h.step_until(Duration::from_secs(600), |h| h.app.lightroom.is_none()));
    let Some(Dialog::LightroomResult { summary, capture_one: true, .. }) = &h.app.ui.dialog else { panic!("{:?}", h.app.ui.dialog) };
    assert!(summary.starts_with("Capture One import complete: "), "{summary}");
    assert!(h.app.session.catalog.len() > before);
    shots(&mut h, "capture-one-real-result");
    // Show in Library, then give the thumbnails a moment
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T)["ok"], true);
    h.step_until(Duration::from_secs(20), |_| false);
    shots(&mut h, "capture-one-real-library");
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}
