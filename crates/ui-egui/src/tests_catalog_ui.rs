//! Catalog UI commands (P1.5): New / Open / Open Recent Catalog, the recent list, the
//! catalog dialogs, and the Read Metadata conflict question.

use serde_json::json;

use crate::catalog_ui::CatalogDialog;
use crate::{DacApp, Services};

fn temp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-ui-catalog-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn app_in(dir: &std::path::Path) -> DacApp {
    let mut s = dac_engine::Session::new().with_fs();
    s.open_library(dir, true).unwrap();
    DacApp::new(s, Services::default())
}

#[test]
fn new_open_and_open_recent_switch_catalogs_and_keep_the_list() {
    let root = temp("switch");
    let mut app = app_in(&root.join("First"));
    app.catalog_ui.recent_file = Some(root.join("recent.json"));
    app.catalog_ui.touch(&root.join("First"));
    let photos = app.session.catalog.len();
    assert!(photos > 0);

    // New Catalog… without a name opens the dialog; with one, creates and opens it
    app.run("catalog.new", json!({})).unwrap();
    assert!(matches!(app.catalog_ui.dialog, Some(CatalogDialog::NewCatalog { .. })));
    let r = app.run("catalog.new", json!({"parent": root.to_string_lossy(), "name": "Second"})).unwrap();
    assert!(r["entry"].as_str().unwrap().ends_with(&format!("Second.{}", dac_brand::CATALOG_EXT)), "{r}");
    assert!(app.session.catalog.is_empty());
    assert!(app.catalog_ui.dialog.is_none());
    assert_eq!(app.session.catalog_dir(), Some(root.join("Second").as_path()));

    // Open Recent lists the first catalog (not the open one) and opens it
    let items = crate::catalog_ui::recent_items(&app);
    assert_eq!(items.len(), 1, "{items:?}");
    app.run("catalog.openRecent", items[0].1.clone()).unwrap();
    assert_eq!(app.session.catalog.len(), photos);
    let saved = dac_catalog::library::RecentCatalogs::load(&root.join("recent.json"));
    assert_eq!(saved.paths.len(), 2);
    assert!(saved.paths[0].starts_with(root.join("First")));

    // a folder that is no catalog is refused (not silently made into an empty one)
    let plain = root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("x.txt"), b"x").unwrap();
    assert!(app.run("catalog.open", json!({"path": plain.to_string_lossy()})).is_err());
    assert_eq!(app.session.catalog.len(), photos, "still the first catalog");
    app.run("catalog.open", json!({"path": root.join("Second").to_string_lossy()})).unwrap();
    assert!(app.session.catalog.is_empty());

    // the startup preference
    app.run("catalog.promptAtStartup", json!({"on": true})).unwrap();
    assert!(dac_catalog::library::RecentCatalogs::load(&root.join("recent.json")).prompt_at_startup);
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn catalog_dialogs_open_and_settings_apply() {
    let root = temp("dialogs");
    let mut app = app_in(&root.join("Lib"));
    app.run("dialog.catalogSettings", json!({})).unwrap();
    let Some(CatalogDialog::Settings { settings }) = app.catalog_ui.dialog.clone() else { panic!("settings dialog") };
    assert_eq!(settings["backup"], "weekly", "{settings}");
    app.run("catalog.settings", json!({"backup": "daily", "previews": {"discardFull": "week"}})).unwrap();
    let info = app.run("catalog.info", json!({})).unwrap();
    assert_eq!(info["settings"]["backup"], "daily");
    assert_eq!(info["settings"]["previews"]["discardFull"], "week");

    app.run("dialog.exportCatalog", json!({})).unwrap();
    assert!(matches!(app.catalog_ui.dialog, Some(CatalogDialog::ExportCatalog { .. })));
    let out = root.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let r = app.run("catalog.export", json!({"parent": out.to_string_lossy(), "name": "Copy", "scope": "all"})).unwrap();
    app.run("dialog.importCatalog", json!({"path": r["entry"]})).unwrap();
    let Some(CatalogDialog::ImportCatalog { plan: Ok(plan), .. }) = app.catalog_ui.dialog.clone() else { panic!("import preview") };
    assert_eq!(plan["unchanged"].as_u64(), Some(0), "demo photos don't match by path: {plan}");
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn read_metadata_asks_before_overwriting_catalog_changes() {
    let root = temp("xmp");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let img = dac_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 150, 255]; 64] };
    let png = dac_codecs::encode_png(&dac_codecs::EncodeImage::rgba8(&img), &dac_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(src.join("a.png"), png).unwrap();
    let mut s = dac_engine::Session::new().with_fs();
    s.open_library(root.join("Lib"), false).unwrap();
    let mut app = DacApp::new(s, Services::default());
    app.run("library.import", json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    let id = app.session.catalog.photos().next().unwrap().id.0;
    app.run("photo.saveMetadataToFile", json!({"ids": [id]})).unwrap();
    // in step: no question
    app.run("photo.readMetadataFromFile", json!({"ids": [id]})).unwrap();
    assert!(app.catalog_ui.dialog.is_none());
    app.run("photo.rate", json!({"ids": [id], "rating": 5})).unwrap();
    let r = app.run("photo.readMetadataFromFile", json!({"ids": [id]})).unwrap();
    assert_eq!(r["confirm"], "dialog", "{r}");
    assert!(matches!(app.catalog_ui.dialog, Some(CatalogDialog::XmpConflict { affected: 1, .. })));
    assert_eq!(app.session.catalog.photos().next().unwrap().rating, 5, "nothing read yet");
    app.run("photo.readMetadataFromFile", json!({"ids": [id], "confirmed": true})).unwrap();
    assert_eq!(app.session.catalog.photos().next().unwrap().rating, 0);
    app.session.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
