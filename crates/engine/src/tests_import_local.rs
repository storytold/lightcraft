//! Importing a folder that was only browsed (Local) adds its photos: the import review doesn't
//! take a browsed photo for one the library already has, by path or by content.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-import-local-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(p: &Path, seed: u8) {
    let img = lcc_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, seed, 255]);
    let b =
        crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() }).unwrap();
    std::fs::write(p, b).unwrap();
}

#[test]
fn a_browsed_folder_imports_its_local_photos_and_skips_library_ones() {
    let dir = temp_dir("folder");
    png(&dir.join("a.png"), 1);
    png(&dir.join("b.png"), 2);
    // the same content as a.png, browsed in another folder: not in the library either
    let other = temp_dir("other");
    std::fs::copy(dir.join("a.png"), other.join("copy.png")).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.browse", &json!({"path": other.to_string_lossy()})).unwrap();
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    let id_of = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).map(|p| p.id).unwrap();
    let b = id_of(&s, "b.png");
    s.execute("photo.addToLibrary", &json!({"ids": [b.0]})).unwrap();

    let preview = s.execute("library.importPreview", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    let candidates = preview["candidates"].as_array().unwrap();
    let of = |name: &str| candidates.iter().find(|c| c["name"] == name).unwrap_or_else(|| panic!("{name}: {preview}"));
    assert!(of("a.png")["duplicate"].is_null(), "only browsed, so not in the library yet: {preview}");
    assert_eq!(of("b.png")["duplicate"], "path", "{preview}");

    // the review imports what isn't a duplicate: the browsed photo joins the library, as itself
    let a = id_of(&s, "a.png");
    let records = s.catalog.photos().count();
    let wanted: Vec<&str> = candidates.iter().filter(|c| c["duplicate"].is_null()).filter_map(|c| c["path"].as_str()).collect();
    s.execute("library.import", &json!({"paths": wanted})).unwrap();
    assert!(!s.catalog.photo(a).unwrap().local, "a.png is in the library now");
    assert_eq!(s.catalog.photos().count(), records, "no second record for it");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
}

/// Culling while browsing stays in the library: with auto-write on, nothing is written into the
/// browsed folder until the photo is added (unless `writeBrowsed` asks for it). Ported from
/// OneSlips' LR-IMP-BROWSE-XMP.
#[test]
fn browsing_writes_no_sidecars_until_import() {
    let dir = temp_dir("xmp");
    png(&dir.join("a.png"), 1);
    png(&dir.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    let id_of = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).map(|p| p.id).unwrap();
    let (a, b) = (id_of(&s, "a.png"), id_of(&s, "b.png"));
    let side = |name: &str| crate::sidecar::sidecar_path(&dir.join(name).to_string_lossy(), Default::default());
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 3})).unwrap();
    s.settle_sidecar_writes().unwrap();
    assert!(!side("a.png").exists(), "a browsed photo's culling is not written into the folder");
    s.execute("library.import", &json!({"paths": [dir.join("a.png").to_string_lossy()]})).unwrap();
    s.execute("photo.rate", &json!({"ids": [a.0], "rating": 4})).unwrap();
    s.settle_sidecar_writes().unwrap();
    assert!(side("a.png").exists(), "added: auto-write writes its sidecar");
    s.execute("library.xmpPreferences", &json!({"writeBrowsed": true})).unwrap();
    s.execute("photo.rate", &json!({"ids": [b.0], "rating": 2})).unwrap();
    s.settle_sidecar_writes().unwrap();
    assert!(side("b.png").exists(), "asked for: browsed photos too");
    let _ = std::fs::remove_dir_all(&dir);
}
/// The browse quick filter (`library.browse`'s `show`): the folder view is everything on disk —
/// the files the library has already included; `show: notImported` narrows it to the files an
/// import would take, and a re-browse of the same folder keeps the choice.
#[test]
fn browse_show_all_and_not_imported() {
    let dir = temp_dir("show");
    png(&dir.join("a.png"), 1);
    png(&dir.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    let id_of = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).map(|p| p.id).unwrap();
    let a = id_of(&s, "a.png");
    s.execute("photo.addToLibrary", &json!({"ids": [a.0]})).unwrap();

    // everything on disk (the default): a.png is in the library, b.png isn't
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    assert_eq!(s.visible_cloned().len(), 2, "both files");
    assert!(s.browse.as_ref().is_some_and(|b| !b.not_imported));

    // not imported: only the file an import would take, and the choice survives a re-browse
    let r = s.execute("library.browse", &json!({"path": dir.to_string_lossy(), "show": "notImported"})).unwrap();
    assert_eq!(r["notImported"], true, "{r}");
    assert_eq!(s.visible_cloned(), vec![id_of(&s, "b.png")], "only the file an import would take");
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    assert_eq!(s.visible_cloned().len(), 1, "kept across a re-browse");
    // back to everything
    s.execute("library.browse", &json!({"path": dir.to_string_lossy(), "show": "all"})).unwrap();
    assert_eq!(s.visible_cloned().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Subfolders come with a first browse of a folder (the default: a folder picked to import means
/// the tree under it); a re-browse of the same folder keeps an explicitly-off choice, and a
/// different folder starts over with subfolders in (the Import tab's browse applies the same rule).
#[test]
fn browse_takes_subfolders_by_default_and_keeps_the_same_folders_choice() {
    let dir = temp_dir("sub-default");
    png(&dir.join("a.png"), 1);
    let nested = dir.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    png(&nested.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    assert_eq!(r["subfolders"], true, "{r}");
    assert_eq!(s.visible().len(), 2, "the nested photo shows");
    // off, given: only the folder's own file
    let r = s.execute("library.browse", &json!({"path": dir.to_string_lossy(), "subfolders": false})).unwrap();
    assert_eq!(r["subfolders"], false, "{r}");
    assert_eq!(s.visible().len(), 1, "flat");
    // left out again: the choice stays
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    assert_eq!(s.visible().len(), 1, "kept across a re-browse");
    // a different folder starts over with subfolders in
    let other = temp_dir("sub-other");
    let deep = other.join("x");
    std::fs::create_dir_all(&deep).unwrap();
    png(&deep.join("c.png"), 3);
    let r = s.execute("library.browse", &json!({"path": other.to_string_lossy()})).unwrap();
    assert_eq!(r["subfolders"], true, "{r}");
    assert_eq!(s.visible().len(), 1, "the nested photo shows");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
}

/// Issue #881: browsing a folder whose photos are already in the library (Local ▸ folder) shows
/// them, as Folders ▸ the same folder does.
#[test]
fn browsing_a_folder_already_in_the_library_shows_its_photos() {
    let dir = temp_dir("in-library");
    png(&dir.join("a.png"), 1);
    png(&dir.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    assert_eq!(s.catalog.photos().filter(|p| !p.local).count(), 2);
    let r = s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    assert_eq!(r["photos"], 2, "{r}");
    assert_eq!(s.visible().len(), 2);
    // with a trailing separator too
    let r = s.execute("library.browse", &json!({"path": format!("{}/", dir.to_string_lossy())})).unwrap();
    assert_eq!(r["photos"], 2, "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}
