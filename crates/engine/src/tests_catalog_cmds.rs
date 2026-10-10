//! Catalog-level commands (P1.5): backup / integrity / optimise, catalog settings (preview
//! settings moved out of prefs.json), Export as Catalog + Import from Another Catalog, XMP
//! stamps on read / write, and the Previous Export source.

use std::path::{Path, PathBuf};

use dac_catalog::{PhotoId, XmpStatus};
use serde_json::json;

use crate::{LibrarySource, Session};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-catcmd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_png(path: &Path, seed: u8) {
    let (w, h) = (16usize, 12usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 13) as u8, seed, 255]).collect();
    let img = dac_raster::Rgba8 { width: w, height: h, data };
    let bytes = dac_codecs::encode_png(&dac_codecs::EncodeImage::rgba8(&img), &dac_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A library at `tag-lib` with `n` imported PNGs from `tag-src`.
fn library(tag: &str, n: u8) -> (Session, PathBuf, PathBuf) {
    let src = temp_dir(&format!("{tag}-src"));
    let lib = temp_dir(&format!("{tag}-lib"));
    let paths: Vec<String> = (0..n)
        .map(|i| {
            let p = src.join(format!("p{i}.png"));
            write_png(&p, i);
            p.to_string_lossy().into_owned()
        })
        .collect();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    if n > 0 {
        s.execute("library.import", &json!({"paths": paths})).unwrap();
    }
    (s, lib, src)
}

fn ids(s: &Session) -> Vec<PhotoId> {
    let mut v: Vec<PhotoId> = s.catalog.photos().map(|p| p.id).collect();
    v.sort();
    v
}

#[test]
fn backup_integrity_and_optimise_on_a_disk_catalog() {
    let (mut s, lib, _) = library("backup", 2);
    let info = s.execute("catalog.info", &json!({})).unwrap();
    assert_eq!(info["onDisk"], true, "{info}");
    let r = s.execute("catalog.checkIntegrity", &json!({})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let root = lib.join("my-backups");
    let r = s.execute("catalog.backup", &json!({"dir": root.to_string_lossy()})).unwrap();
    let out = PathBuf::from(r["backup"].as_str().unwrap());
    assert!(out.starts_with(&root) && out.join("catalog.redb").is_file(), "{r}");
    let r = s.execute("catalog.optimize", &json!({})).unwrap();
    assert!(r["bytesAfter"].as_u64().unwrap() > 0, "{r}");
    // the backup recorded its time: a weekly backup is not due on exit any more
    let set = s.execute("catalog.settings", &json!({"backup": "weekly"})).unwrap();
    assert!(set["lastBackup"].is_i64(), "{set}");
    assert_eq!(s.execute("catalog.backupIfDue", &json!({})).unwrap()["backup"], json!(null));
    // every exit: due now
    s.execute("catalog.settings", &json!({"backup": "everyExit", "optimize": true})).unwrap();
    let r = s.execute("catalog.backupIfDue", &json!({})).unwrap();
    assert!(r["backup"].is_string(), "{r}");
    assert!(s.execute("catalog.settings", &json!({"backup": "hourly"})).is_err());
}

#[test]
fn an_in_memory_session_has_no_catalog_to_back_up() {
    let mut s = Session::with_demo();
    assert!(s.execute("catalog.backup", &json!({})).is_err());
    assert!(s.execute("catalog.checkIntegrity", &json!({})).is_err());
    assert_eq!(s.execute("catalog.backupIfDue", &json!({})).unwrap()["backup"], json!(null));
}

#[test]
fn preview_settings_live_in_the_catalog_settings() {
    let (mut s, lib, _) = library("previews", 0);
    s.execute("library.previewSettings", &json!({"standardEdge": 1024, "discardFull": "week"})).unwrap();
    let cs = dac_catalog::library::CatalogSettings::load(&lib);
    assert_eq!(cs.previews.as_ref().and_then(|v| v["standardEdge"].as_u64()), Some(1024), "{cs:?}");
    let prefs = std::fs::read_to_string(lib.join("prefs.json")).unwrap();
    assert!(!prefs.contains("1024"), "no longer in prefs.json: {prefs}");
    s.close_library().unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.preview_prefs.standard_edge(), 1024);
    // through catalog.settings too
    let r = s.execute("catalog.settings", &json!({"previews": {"standardEdge": 2048}})).unwrap();
    assert_eq!(r["previews"]["standardEdge"], 2048, "{r}");
}

#[test]
fn preview_settings_in_an_old_prefs_file_are_migrated() {
    let lib = temp_dir("previews-old");
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        s.close_library().unwrap();
    }
    std::fs::write(lib.join("prefs.json"), br#"{"previews": {"standardEdge": 3000}}"#).unwrap();
    let _ = std::fs::remove_file(lib.join(dac_catalog::library::SETTINGS_FILE));
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.preview_prefs.standard_edge(), 3000);
    s.save_prefs().unwrap();
    let cs = dac_catalog::library::CatalogSettings::load(&lib);
    assert_eq!(cs.previews.as_ref().and_then(|v| v["standardEdge"].as_u64()), Some(3000));
}

#[test]
fn export_as_catalog_then_import_it_back_with_a_rule() {
    let (mut s, lib, _) = library("transfer", 3);
    let all = ids(&s);
    let out = lib.parent().unwrap().join(format!("lc-catcmd-out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let r = s
        .execute("catalog.export", &json!({"parent": out.to_string_lossy(), "name": "Sub", "ids": [all[0].0, all[1].0], "originals": true}))
        .unwrap();
    assert_eq!(r["photos"], 2, "{r}");
    assert_eq!(r["originalsCopied"], 2, "{r}");
    let entry = r["entry"].as_str().unwrap().to_string();
    assert!(Path::new(&entry).is_file());
    // the exported photos point at their copies: importing adds them as new photos
    let plan = s.execute("catalog.import", &json!({"path": entry, "preview": true})).unwrap();
    assert_eq!(plan["newPhotos"].as_array().unwrap().len(), 2, "{plan}");
    assert_eq!(s.catalog.len(), 3, "a preview changes nothing");
    let r = s.execute("catalog.import", &json!({"path": entry})).unwrap();
    assert_eq!(r["imported"], true);
    assert_eq!(s.catalog.len(), 5);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.len(), 3, "one undo step");
    // the open catalog itself is refused
    assert!(s.execute("catalog.import", &json!({"path": lib.to_string_lossy()})).is_err());
    assert!(s.execute("catalog.import", &json!({"path": entry, "rule": "merge"})).is_err());
}

#[test]
fn export_without_originals_keeps_paths_and_import_applies_changed_ratings() {
    let (mut s, _lib, src) = library("rules", 2);
    let all = ids(&s);
    let out = temp_dir("rules-out");
    let r = s.execute("catalog.export", &json!({"parent": out.to_string_lossy(), "name": "Away", "scope": "all"})).unwrap();
    let entry = r["entry"].as_str().unwrap().to_string();
    // edit the other catalog: rate a photo there
    {
        let mut o = Session::new().with_fs();
        o.open_library(Path::new(&entry).parent().unwrap(), false).unwrap();
        let id = o.catalog.photos().find(|p| p.file_name == "p0.png").unwrap().id;
        o.execute("photo.rate", &json!({"ids": [id.0], "rating": 4})).unwrap();
        o.close_library().unwrap();
    }
    let plan = s.execute("catalog.import", &json!({"path": entry, "preview": true})).unwrap();
    assert_eq!(plan["changedMetadata"], 1, "{plan}");
    // keep: nothing changes
    s.execute("catalog.import", &json!({"path": entry, "rule": "keep"})).unwrap();
    assert!(s.catalog.photos().all(|p| p.rating == 0));
    s.execute("catalog.import", &json!({"path": entry, "rule": "replaceMetadata"})).unwrap();
    let p0 = s.catalog.photos().find(|p| p.file_name == "p0.png").unwrap();
    assert_eq!(p0.rating, 4);
    assert_eq!(ids(&s), all, "no new photos: matched by path");
    let _ = src;
}

#[test]
fn reading_and_saving_xmp_records_a_stamp() {
    let (mut s, _lib, _) = library("xmpstamp", 1);
    let id = ids(&s)[0];
    assert_eq!(s.xmp_status(id), XmpStatus::Unknown);
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 2})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(s.xmp_status(id), XmpStatus::InSync);
    // changed in the catalog
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 3})).unwrap();
    assert_eq!(s.xmp_status(id), XmpStatus::ChangedInCatalog);
    let st = s.execute("photo.xmpStatus", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(st["photos"][0]["status"], "changedInCatalog", "{st}");
    // read back: the file's rating (2), in step
    s.execute("photo.readMetadataFromFile", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().rating, 2);
    assert_eq!(s.xmp_status(id), XmpStatus::InSync);
    // the stamp survives a reopen (journaled)
    let dir = s.catalog_dir().unwrap().to_path_buf();
    s.close_library().unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&dir, false).unwrap();
    assert!(s.catalog.photo(id).unwrap().xmp.is_some());
}

#[test]
fn auto_write_stamps_and_a_changed_sidecar_shows_on_disk() {
    let (mut s, _lib, _) = library("xmpauto", 1);
    let id = ids(&s)[0];
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 5})).unwrap();
    assert_eq!(s.xmp_status(id), XmpStatus::InSync);
    // another app rewrites the sidecar
    let p = s.catalog.photo(id).unwrap();
    let orig = crate::sidecar::file_path(p).unwrap().to_string();
    let side = crate::sidecar::find_sidecar(&orig, s.sidecar_naming(id)).unwrap();
    let mut body = std::fs::read_to_string(&side).unwrap();
    body.push_str("\n<!-- edited elsewhere -->\n");
    std::fs::write(&side, body).unwrap();
    assert_eq!(s.xmp_status(id), XmpStatus::ChangedOnDisk);
}

#[test]
fn previous_export_source_shows_the_last_exported_photos() {
    let (mut s, lib, _) = library("prevexport", 3);
    let all = ids(&s);
    s.execute("library.source", &json!({"kind": "previousExport"})).unwrap();
    assert_eq!(s.source, LibrarySource::PreviousExport);
    assert!(s.visible().is_empty(), "nothing exported yet");
    s.record_export(&[all[1], all[2]]);
    let mut v = s.visible().to_vec();
    v.sort();
    assert_eq!(v, vec![all[1], all[2]]);
    s.close_library().unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.previous_export, vec![all[1], all[2]], "kept with the view");
}
