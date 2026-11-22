//! Catalog-level commands (P1.5): backup / integrity / optimise, catalog settings (preview
//! settings moved out of prefs.json), Export as Catalog + Import from Another Catalog, XMP
//! stamps on read / write, and the Previous Export source.

use std::path::{Path, PathBuf};

use lcc_catalog::{PhotoId, XmpStatus};
use serde_json::json;

use crate::{LibrarySource, Session};

#[test]
fn up_and_down_move_by_rows_and_stop_at_the_ends() {
    let mut s = Session::with_demo();
    let vis = s.visible_cloned();
    assert!(vis.len() >= 6);
    s.selection = crate::Selection::single(vis[0]);
    s.execute("library.down", &json!({"rows": 4})).unwrap();
    assert_eq!(s.selection.active, Some(vis[4]));
    s.execute("library.up", &json!({})).unwrap();
    assert_eq!(s.selection.active, Some(vis[3]), "default is one photo (the filmstrip)");
    s.execute("library.up", &json!({"rows": 99})).unwrap();
    assert_eq!(s.selection.active, Some(vis[0]), "clamped at the first photo");
    s.execute("library.down", &json!({"rows": u64::MAX})).unwrap();
    assert_eq!(s.selection.active, vis.last().copied(), "hostile counts clamp, never overflow");
}

/// The test's folder in Temp, removed when the test ends.
fn temp_dir(tag: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lc-catcmd-{tag}"))
}

fn write_png(path: &Path, seed: u8) {
    let (w, h) = (16usize, 12usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 13) as u8, seed, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A library at `tag-lib` with `n` imported PNGs from `tag-src` (Temp folders, removed when the
/// test ends) and the session that has it open. Bound in this order, `let (lib, src, s)`, the session
/// drops first: its catalog is closed when the folders go (Windows keeps an open catalog's folder).
fn library(tag: &str, n: u8) -> (lcc_testkit::TempDir, lcc_testkit::TempDir, Session) {
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
    (lib, src, s)
}

fn ids(s: &Session) -> Vec<PhotoId> {
    let mut v: Vec<PhotoId> = s.catalog.photos().map(|p| p.id).collect();
    v.sort();
    v
}

#[test]
fn backup_integrity_and_optimise_on_a_disk_catalog() {
    let (lib, _src, mut s) = library("backup", 2);
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
    // every exit: due now, but nothing changed since the backup, so none is made
    s.execute("catalog.settings", &json!({"backup": "everyExit", "optimize": true})).unwrap();
    assert_eq!(s.execute("catalog.backupIfDue", &json!({})).unwrap()["backup"], json!(null));
    // a change: backed up
    s.execute("album.create", &json!({"name": "Picks"})).unwrap();
    s.persist_if_dirty();
    let r = s.execute("catalog.backupIfDue", &json!({})).unwrap();
    assert!(r["backup"].is_string(), "{r}");
    assert!(s.execute("catalog.settings", &json!({"backup": "hourly"})).is_err());

    // an in-memory session has no catalog to back up: the commands say so, the exit-time check is quiet
    let mut mem = Session::new();
    assert!(mem.execute("catalog.backup", &json!({})).is_err());
    assert!(mem.execute("catalog.checkIntegrity", &json!({})).is_err());
    assert_eq!(mem.execute("catalog.backupIfDue", &json!({})).unwrap()["backup"], json!(null), "in memory");
}

#[test]
fn damaged_catalog_settings_are_set_aside_and_the_user_told() {
    let (lib, _src, mut s) = library("settings-damaged", 0);
    s.close_library().unwrap();
    drop(s);
    let file = lib.join(lcc_catalog::library::SETTINGS_FILE);
    std::fs::write(&file, b"{\"keepBackups\": \"lots\"").unwrap();
    let mut s = Session::new().with_fs();
    if let Err(e) = s.open_library(&lib, false) {
        panic!("{e}");
    }
    let w = s.take_notices();
    assert!(
        w.iter().any(|n| matches!(n, crate::notice::Notice::SettingsSetAside { file, kept } if file == "catalog-settings.json" && kept.starts_with("catalog-settings.json.corrupt-"))),
        "{w:?}"
    );
    assert!(s.take_notices().is_empty(), "told once");
    assert!(!file.exists() || lcc_catalog::library::CatalogSettings::load_checked(&lib).1.is_none());
    let kept = std::fs::read_dir(&lib).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("catalog-settings.json.corrupt-"));
    assert!(kept, "the damaged file is kept aside");
    let cs = lcc_catalog::library::CatalogSettings::load(&lib);
    assert!(cs.keep_backups > 0, "backups stay on: {cs:?}");
    s.close_library().unwrap();
}

#[test]
fn preview_settings_live_in_the_catalog_settings() {
    let (lib, _src, mut s) = library("previews", 0);
    s.execute("library.previewSettings", &json!({"standardEdge": 1024, "discardFull": "week"})).unwrap();
    let cs = lcc_catalog::library::CatalogSettings::load(&lib);
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
    let _ = std::fs::remove_file(lib.join(lcc_catalog::library::SETTINGS_FILE));
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.preview_prefs.standard_edge(), 3000);
    s.save_prefs().unwrap();
    let cs = lcc_catalog::library::CatalogSettings::load(&lib);
    assert_eq!(cs.previews.as_ref().and_then(|v| v["standardEdge"].as_u64()), Some(3000));
}

#[test]
fn export_as_catalog_then_import_it_back_with_a_rule() {
    // (made first, so it goes after the session that imported from it)
    let out = temp_dir("out");
    let (lib, _src, mut s) = library("transfer", 3);
    let all = ids(&s);
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
    let out = temp_dir("rules-out");
    let (_lib, src, mut s) = library("rules", 2);
    let all = ids(&s);
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
    let (_lib, _src, mut s) = library("xmpstamp", 1);
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
    // auto write stamps too, and a sidecar another app rewrites shows as changed on disk
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 5})).unwrap();
    assert_eq!(s.xmp_status(id), XmpStatus::InSync, "auto write");
    let p = s.catalog.photo(id).unwrap();
    let orig = crate::sidecar::file_path(p).unwrap().to_string();
    let side = crate::sidecar::find_sidecar(&orig, s.sidecar_naming(id)).unwrap();
    let mut body = std::fs::read_to_string(&side).unwrap();
    body.push_str("\n<!-- edited elsewhere -->\n");
    std::fs::write(&side, body).unwrap();
    assert_eq!(s.xmp_status(id), XmpStatus::ChangedOnDisk);
    // the stamp survives a reopen (journaled)
    let dir = s.catalog_dir().unwrap().to_path_buf();
    s.close_library().unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&dir, false).unwrap();
    assert!(s.catalog.photo(id).unwrap().xmp.is_some());
}

/// Issue #1310: a sidecar the library was never in step with keeps its keywords on Save Metadata.
#[test]
fn saving_keeps_keywords_only_an_unsynced_sidecar_had() {
    let (_lib, _src, mut s) = library("xmp-keep-keywords", 1);
    let id = ids(&s)[0];
    let orig = crate::sidecar::file_path(s.catalog.photo(id).unwrap()).unwrap().to_string();
    let side = crate::sidecar::sidecar_path(&orig, s.sidecar_naming(id));
    let md = lcc_meta::Metadata {
        keywords: vec!["People".into(), "Anna".into(), "harbour".into()],
        hierarchical_keywords: vec!["People|Anna".into(), "harbour".into()],
        ..Default::default()
    };
    std::fs::write(&side, lcc_meta::write_xmp(&md, None)).unwrap();
    assert_eq!(s.xmp_status(id), XmpStatus::Unknown);
    s.execute("photo.setMeta", &json!({"ids": [id.0], "keywords": ["new"]})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    let back = lcc_meta::parse_xmp(&std::fs::read_to_string(&side).unwrap()).unwrap().metadata;
    let kws = lcc_catalog::keywords::from_file(&back.keywords, &back.hierarchical_keywords);
    for k in ["new", "People|Anna", "harbour"] {
        assert!(kws.iter().any(|x| x == k), "{k} kept: {kws:?}");
    }
    // the library holds them too, so a later save (now in step) keeps them
    assert_eq!(s.xmp_status(id), XmpStatus::InSync);
    s.execute("photo.setMeta", &json!({"ids": [id.0], "title": "Quay"})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    let back = lcc_meta::parse_xmp(&std::fs::read_to_string(&side).unwrap()).unwrap().metadata;
    assert!(back.keywords.iter().any(|k| k == "harbour"), "{:?}", back.keywords);
    // once in step, a keyword removed here is removed from the file
    s.execute("photo.setMeta", &json!({"ids": [id.0], "keywords": ["new"]})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    let back = lcc_meta::parse_xmp(&std::fs::read_to_string(&side).unwrap()).unwrap().metadata;
    assert!(!back.keywords.iter().any(|k| k == "harbour"), "{:?}", back.keywords);
}

/// Issue #1310, automatic XMP writes: the first write of a sidecar the library was never in step
/// with keeps the keywords only the sidecar had, as Save Metadata to File does.
#[test]
fn auto_write_keeps_keywords_only_an_unsynced_sidecar_had() {
    let (_lib, _src, mut s) = library("xmp-auto-keep-keywords", 1);
    let id = ids(&s)[0];
    let orig = crate::sidecar::file_path(s.catalog.photo(id).unwrap()).unwrap().to_string();
    let side = crate::sidecar::sidecar_path(&orig, s.sidecar_naming(id));
    let md = lcc_meta::Metadata { keywords: vec!["harbour".into()], hierarchical_keywords: vec!["harbour".into()], ..Default::default() };
    std::fs::write(&side, lcc_meta::write_xmp(&md, None)).unwrap();
    assert_eq!(s.xmp_status(id), XmpStatus::Unknown);
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute("photo.setMeta", &json!({"ids": [id.0], "keywords": ["new"]})).unwrap();
    s.settle_sidecar_writes().unwrap();
    let back = lcc_meta::parse_xmp(&std::fs::read_to_string(&side).unwrap()).unwrap().metadata;
    let kws = lcc_catalog::keywords::from_file(&back.keywords, &back.hierarchical_keywords);
    for k in ["new", "harbour"] {
        assert!(kws.iter().any(|x| x == k), "{k} kept: {kws:?}");
    }
    assert_eq!(s.xmp_status(id), XmpStatus::InSync);
}

/// Issue #1310 through undo: undoing the edit whose auto-write adopted the sidecar's keywords
/// keeps them, so the next write doesn't remove them from the file.
#[test]
fn undo_after_an_auto_write_keeps_the_adopted_keywords() {
    let (_lib, _src, mut s) = library("xmp-auto-undo-keywords", 1);
    let id = ids(&s)[0];
    let orig = crate::sidecar::file_path(s.catalog.photo(id).unwrap()).unwrap().to_string();
    let side = crate::sidecar::sidecar_path(&orig, s.sidecar_naming(id));
    let md = lcc_meta::Metadata { keywords: vec!["harbour".into()], hierarchical_keywords: vec!["harbour".into()], ..Default::default() };
    std::fs::write(&side, lcc_meta::write_xmp(&md, None)).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute("photo.setMeta", &json!({"ids": [id.0], "keywords": ["new"]})).unwrap();
    s.settle_sidecar_writes().unwrap();
    s.undo_step().unwrap();
    assert!(s.catalog.photo(id).unwrap().meta.keywords.iter().any(|k| k == "harbour"));
    s.settle_sidecar_writes().unwrap();
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 3})).unwrap();
    s.settle_sidecar_writes().unwrap();
    let back = lcc_meta::parse_xmp(&std::fs::read_to_string(&side).unwrap()).unwrap().metadata;
    let kws = lcc_catalog::keywords::from_file(&back.keywords, &back.hierarchical_keywords);
    assert!(kws.iter().any(|x| x == "harbour"), "kept after undo: {kws:?}");
}

#[test]
fn hostile_sidecars_add_no_keywords_and_never_panic() {
    let p = lcc_catalog::Photo::new(PhotoId(1), lcc_catalog::Source::Demo { scene: 0 }, "a.jpg", "JPEG", 10, 10, "2026-01-01T00:00:00");
    for xmp in ["", "<not xmp", "\u{feff}", "<x:xmpmeta><rdf:RDF><rdf:Description><dc:subject><rdf:Bag><rdf:li>", &"<".repeat(100_000)] {
        assert!(crate::sidecar::sidecar_only_keywords(&p, xmp).is_empty(), "{xmp:.40}");
    }
    let many: Vec<String> = (0..10_000).map(|i| format!("k{i}")).collect();
    let md = lcc_meta::Metadata { keywords: many, ..Default::default() };
    assert!(crate::sidecar::sidecar_only_keywords(&p, &lcc_meta::write_xmp(&md, None)).len() <= 4096);
}

/// One photo with a written sidecar (rating 5) and that sidecar's path, with [`library`]'s folders
/// and session first (bound in this order, the session drops before the folders).
fn synced_photo(name: &str) -> (lcc_testkit::TempDir, lcc_testkit::TempDir, Session, PhotoId, PathBuf) {
    let (lib, src, mut s) = library(name, 1);
    let id = ids(&s)[0];
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 5})).unwrap();
    s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
    let p = s.catalog.photo(id).unwrap();
    let orig = crate::sidecar::file_path(p).unwrap().to_string();
    let side = crate::sidecar::find_sidecar(&orig, s.sidecar_naming(id)).unwrap();
    (lib, src, s, id, side)
}

/// Another app sets the sidecar's rating (the size changes too, so the stat sees it).
fn rerate_on_disk(side: &Path, rating: u8) {
    let body = std::fs::read_to_string(side).unwrap();
    assert!(body.contains("<xmp:Rating>5</xmp:Rating>"), "{body}");
    let body = body.replace("<xmp:Rating>5</xmp:Rating>", &format!("<xmp:Rating>{rating}</xmp:Rating>")) + "\n<!-- edited elsewhere -->\n";
    std::fs::write(side, body).unwrap();
}

#[test]
fn auto_read_applies_a_sidecar_changed_only_on_disk() {
    let (_lib, _src, mut s, id, side) = synced_photo("xmpautoread");
    assert!(s.xmp.auto_read, "on by default");
    // a malformed sidecar is an error or reported, never a panic, and nothing is read from it
    let good = std::fs::read_to_string(&side).unwrap();
    for bad in ["", "<x:xmpmeta", "\u{0}\u{1}garbage", "<?xpacket begin=\"\"?><rdf:RDF><rdf:Description xmp:Rating=\"99999999999999\"", "<<<>>>"] {
        std::fs::write(&side, bad).unwrap();
        let r = s.execute("photo.autoReadMetadata", &json!({"ids": [id.0]}));
        if let Ok(r) = r {
            assert_eq!(r["failed"].as_array().map(Vec::len), Some(1), "{bad:?} is reported: {r}");
        }
    }
    assert_eq!(s.catalog.photo(id).unwrap().rating, 5, "nothing read from a broken file");
    std::fs::write(&side, good).unwrap();
    rerate_on_disk(&side, 2);
    assert_eq!(s.xmp_status(id), XmpStatus::ChangedOnDisk);
    let r = s.execute("photo.autoReadMetadata", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["read"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(s.catalog.photo(id).unwrap().rating, 2);
    assert_eq!(s.xmp_status(id), XmpStatus::InSync);
    // one undo step, like Read Metadata from File
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().rating, 5);
}

#[test]
fn auto_read_keeps_a_conflict() {
    let (_lib, _src, mut s, id, side) = synced_photo("xmpautoconflict");
    s.execute("photo.rate", &json!({"ids": [id.0], "rating": 4})).unwrap();
    rerate_on_disk(&side, 2);
    assert_eq!(s.xmp_status(id), XmpStatus::Conflict);
    let r = s.execute("photo.autoReadMetadata", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["conflicts"], json!([id.0]), "{r}");
    assert_eq!(s.catalog.photo(id).unwrap().rating, 4, "the catalog's change is kept");
    assert_eq!(s.xmp_status(id), XmpStatus::Conflict);
}

#[test]
fn folder_sync_auto_reads_unless_turned_off() {
    let (_lib, _src, mut s, id, side) = synced_photo("xmpautosync");
    let folder = side.parent().unwrap().to_string_lossy().to_string();
    s.execute("library.xmpPreferences", &json!({"autoRead": false})).unwrap();
    rerate_on_disk(&side, 1);
    s.execute("folder.sync", &json!({"path": folder, "import": false})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().rating, 5, "off: only listed");
    s.execute("library.xmpPreferences", &json!({"autoRead": true})).unwrap();
    s.execute("folder.sync", &json!({"path": folder, "import": false})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().rating, 1);
}

#[test]
fn previous_export_source_shows_the_last_exported_photos() {
    let (lib, _src, mut s) = library("prevexport", 3);
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
