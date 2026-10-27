//! Issue #891: File ▸ Open Library… given a Lightroom Classic catalog folder doesn't make a
//! LightCraft library inside it. The library open now stays, and a dialog offers Import Lightroom
//! Catalog… with the catalog found, which asks where that `.lrcat` goes (#2672), then imports it.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn names(dir: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    v.sort();
    v
}

#[test]
fn open_library_on_a_lightroom_catalog_offers_its_import() {
    let dir = std::env::temp_dir().join(format!("lc-ui-openlrcat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("LightCraft Library");
    let lr = dir.join("Lightroom");
    std::fs::create_dir_all(lr.join("Lightroom Catalog Previews.lrdata")).unwrap();
    std::fs::create_dir_all(lr.join("Lightroom Catalog.lrcat-data")).unwrap();
    std::fs::create_dir_all(lr.join("Backups")).unwrap();
    let lrcat = lr.join("Lightroom Catalog.lrcat");
    std::fs::write(&lrcat, b"not really SQLite").unwrap();
    let before = names(&lr);

    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&lib, true).unwrap();
    let photos = session.catalog.len();
    assert!(photos > 0);
    let picked = lr.to_string_lossy().to_string();
    let png = |img: &lightcraft_raster::Rgba8| {
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
    };
    let services = Services {
        pick_folder: Some(Box::new(move || Some(picked.clone()))),
        png: Some(Box::new(png)),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        write_new: Some(Box::new(lightcraft_engine::export::write_file_new)),
        ..Default::default()
    };
    let mut h = Headless::new(LightcraftApp::new(session, services), [1200.0, 800.0], 1.0);
    h.step();

    // File ▸ Open Library…, the Lightroom folder chosen in the folder dialog
    let r = h.app.run("app.openLibrary", json!({})).unwrap();
    assert_eq!(r["opened"], false, "{r}");
    assert_eq!(r["lightroomCatalog"], json!(lrcat.to_string_lossy()), "{r}");
    h.step();
    h.step();
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::LightroomCatalog { catalog: Some(c), .. }) if *c == lrcat.to_string_lossy()),
        "{:?}",
        h.app.ui.dialog
    );
    assert!(has(&h, "button:dialogOk") && has(&h, "button:dialogCancel"));
    // the library open before is still open, and nothing was made in Lightroom's folder
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.clone()), Some(lib.clone()));
    assert_eq!(h.app.session.catalog.len(), photos);
    assert_eq!(names(&lr), before);
    if let Some(p) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") {
        let r = h.request("ui.screenshot", json!({"path": std::path::Path::new(&p).join("prompt-891.png").to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }

    // Cancel: nothing happens
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T)["ok"], true);
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert!(h.app.lightroom.is_none() && h.app.lightroom_last.is_none());

    // a previews folder given by path (control channel, agents): the same offer
    let r = h.app.run("app.openLibrary", json!({"path": lr.join("Lightroom Catalog Previews.lrdata").to_string_lossy()})).unwrap();
    assert_eq!(r["lightroomCatalog"], json!(lrcat.to_string_lossy()), "{r}");
    h.step();
    h.step();
    // Import Lightroom Catalog…: no file dialog, the window that asks where that catalog goes
    // (#2672) takes its place, as File ▸ Import Lightroom Catalog… does
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T)["ok"], true);
    h.step();
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::LightroomDestination { catalog, .. }) if *catalog == lrcat.to_string_lossy()),
        "{:?}",
        h.app.ui.dialog
    );
    // Merge: the import of that catalog starts, into the library open now. (This fake catalog
    // then fails to read: only its path matters here.)
    assert_eq!(h.request("ui.clickWidget", json!({"id": "radio:lightroomDestination-open"}), T)["ok"], true);
    h.step();
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T)["ok"], true);
    h.step();
    // A fast read failure can already have replaced the offer with its lasting result.
    assert!(!matches!(h.app.ui.dialog, Some(Dialog::LightroomCatalog { .. } | Dialog::LightroomDestination { .. })));
    assert!(h.step_until(T, |h| h.app.lightroom.is_none()));
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::LightroomResult { summary, first: None, .. }) if summary.contains("failed")));
    let last = h.app.lightroom_last.clone().unwrap_or_default();
    assert_eq!(last["path"], json!(lrcat), "{last}");
    assert_eq!(h.app.session.library.as_ref().map(|l| l.dir.clone()), Some(lib.clone()));
    assert_eq!(names(&lr), before, "the catalog is only read");
    drop(h);
    let _ = std::fs::remove_dir_all(&dir);
}
