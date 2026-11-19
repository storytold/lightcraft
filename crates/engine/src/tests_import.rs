//! Import: recursive folders, duplicate detection by content, add vs copy into the library.

use std::path::Path;

use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-import-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    // Tests that take a folder "offline" rename it to this sibling; a left-over one (from a run
    // whose process id is reused) is non-empty and would make that rename fail.
    let _ = std::fs::remove_dir_all(d.with_extension("offline"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A small procedural PNG (distinct per seed).
fn write_png(path: &Path, seed: u8) {
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    // undated files take the file's date: noon UTC, 14 January in every zone the tests run in
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_768_392_000);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

fn exif_clock_block(original: Option<&str>, digitized: Option<&str>) -> Vec<u8> {
    use lcc_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value};

    let mut exif = IfdBuilder::new();
    if let Some(value) = original {
        exif.set(0x9003, Value::Ascii(value.into()));
    }
    if let Some(value) = digitized {
        exif.set(0x9004, Value::Ascii(value.into()));
    }
    let mut ifd0 = IfdBuilder::new();
    ifd0.set_child(0x8769, exif);
    TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
}

fn write_png_with_metadata(path: &Path, seed: u8, exif: Option<&[u8]>, xmp: Option<&str>) {
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    let metadata = lcc_codecs::EncodeMeta { exif, xmp, ..Default::default() };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &metadata).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    // Keep the same known file-date fallback as write_png, distinct from the regression timestamp.
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_768_392_000);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

fn ids(v: &Value, key: &str) -> usize {
    v[key].as_array().map(Vec::len).unwrap_or(0)
}

/// `expand_report_subfolders`: nested folders are walked only when asked, and the skipped (library) folder is
/// left out with or without them.
#[test]
fn expand_report_subfolders_respects_the_inclusion_flag_and_the_skipped_folder() {
    {
        let src = temp_dir("subfolder-discovery");
        let nested = src.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let root_file = src.join("root.jpg");
        let nested_file = nested.join("nested.jpg");
        let unsupported_file = src.join("notes.txt");
        let sidecar_file = src.join("root.xmp");

        std::fs::write(&root_file, b"test-root-content").unwrap();
        std::fs::write(&nested_file, b"test-nested-content").unwrap();
        std::fs::write(&unsupported_file, b"not an image").unwrap();
        std::fs::write(&sidecar_file, b"<xmp></xmp>").unwrap();

        let src_str = src.to_string_lossy().to_string();

        let slashes = |v: Vec<String>| {
            let mut v: Vec<String> = v.into_iter().map(|p| p.replace('\\', "/")).collect();
            v.sort();
            v
        };
        let root_norm = root_file.to_string_lossy().replace('\\', "/");
        let nested_norm = nested_file.to_string_lossy().replace('\\', "/");

        // With subfolders: true -> both root and nested photos are discovered
        let with_sub = crate::import::expand_report_subfolders(std::slice::from_ref(&src_str), None, crate::walk::Limits::default(), true);
        let files_with_sub = slashes(with_sub.files);
        assert!(files_with_sub.contains(&root_norm));
        assert!(files_with_sub.contains(&nested_norm));
        assert_eq!(files_with_sub.len(), 2);

        // With subfolders: false -> only root photo is discovered; nested directory is excluded
        let without_sub = crate::import::expand_report_subfolders(std::slice::from_ref(&src_str), None, crate::walk::Limits::default(), false);
        let files_without_sub = slashes(without_sub.files);
        assert_eq!(files_without_sub, vec![root_norm]);
        assert_eq!(without_sub.unsupported, 1, "notes.txt counted as unsupported, sidecar is ignored");

        let _ = std::fs::remove_dir_all(src);
    }
    {
        let src = temp_dir("subfolder-skip");
        let lib = src.join("Library");
        std::fs::create_dir_all(&lib).unwrap();
        let img_root = src.join("root.jpg");
        let img_lib = lib.join("ignored.jpg");
        std::fs::write(&img_root, b"root").unwrap();
        std::fs::write(&img_lib, b"lib").unwrap();

        let src_str = src.to_string_lossy().to_string();

        // With subfolders: false and skip: Some(&lib), files in lib are skipped
        let e = crate::import::expand_report_subfolders(std::slice::from_ref(&src_str), Some(&lib), crate::walk::Limits::default(), false);
        assert_eq!(e.files.len(), 1);
        assert!(e.files[0].replace('\\', "/").ends_with("root.jpg"));

        // If skip matches the scanned directory itself, nothing is expanded
        let e_skip_self = crate::import::expand_report_subfolders(std::slice::from_ref(&src_str), Some(&src), crate::walk::Limits::default(), false);
        assert_eq!(e_skip_self.files.len(), 0);

        let _ = std::fs::remove_dir_all(src);
    }
}

/// A file that was only browsed (Local) is not "already in the library": the scan offers it, and
/// importing it brings the browsed record (with its edits) into the library.
#[test]
fn a_browsed_photos_own_file_is_offered_and_joins_the_library_when_imported() {
    let src = temp_dir("browsed-own");
    let f = src.join("a.png");
    write_png(&f, 75);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [f], "local": true})).unwrap();
    let id = r["imported"][0].as_u64().unwrap();
    s.execute("photo.rate", &json!({"ids": [id], "rating": 3})).unwrap();
    let scan = s.execute("library.importPreview", &json!({"paths": [f]})).unwrap();
    assert_eq!(scan["duplicates"], 0, "{scan}");
    assert_eq!(scan["candidates"][0]["duplicate"], Value::Null, "{scan}");
    assert_eq!(scan["candidates"][0]["existing"], id, "the candidate still names the browsed record: {scan}");
    assert_eq!(scan["candidates"][0]["local"], true, "and says it was only browsed: {scan}");
    let r = s.execute("library.import", &json!({"paths": [f]})).unwrap();
    assert_eq!(r["imported"], json!([id]), "{r}");
    let p = s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap();
    assert!(!p.local && p.rating == 3, "the same record, now in the library, edits kept");
    let scan = s.execute("library.importPreview", &json!({"paths": [f]})).unwrap();
    assert_eq!(scan["duplicates"], 1, "now it is in the library: {scan}");
    let _ = std::fs::remove_dir_all(src);
}

/// A browse record in Recently Deleted still counts as the path's: the scan lists the file as it did,
/// not as a new browsed file.
#[test]
fn a_deleted_browse_record_still_counts_as_the_files_own() {
    let src = temp_dir("browsed-deleted");
    let f = src.join("a.png");
    write_png(&f, 76);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [f], "local": true})).unwrap();
    let id = r["imported"][0].as_u64().unwrap();
    // (photo.delete leaves browsed photos alone now: a record trashed by an older build)
    s.catalog.apply(lcc_catalog::Op::SetDeleted { id: lcc_catalog::PhotoId(id), deleted: true }).unwrap();
    let scan = s.execute("library.importPreview", &json!({"paths": [f]})).unwrap();
    assert_eq!(scan["candidates"][0]["duplicate"], "path", "{scan}");
    assert_eq!(scan["candidates"][0].get("local"), None, "{scan}");
    let _ = std::fs::remove_dir_all(src);
}

/// "Import as new" replaces the trashed library photo with the same content when the browsed twin
/// is promoted, as it does for a new file.
#[test]
fn promoting_a_browsed_twin_as_new_replaces_the_trashed_library_photo() {
    let src = temp_dir("browsed-fresh");
    let original = src.join("original.png");
    let twin = src.join("browsed.png");
    write_png(&original, 77);
    std::fs::copy(&original, &twin).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [original]})).unwrap();
    let old = r["imported"][0].as_u64().unwrap();
    let r = s.execute("library.import", &json!({"paths": [twin], "local": true})).unwrap();
    let local = r["imported"][0].as_u64().unwrap();
    s.execute("photo.delete", &json!({"ids": [old]})).unwrap();
    let r = s.execute("library.import", &json!({"paths": [twin], "onDeleted": "fresh"})).unwrap();
    assert_eq!(r["imported"], json!([local]), "{r}");
    let trashed = s.catalog.photos().filter(|p| p.deleted).count();
    assert_eq!(trashed, 0, "the trashed twin is gone");
    assert!(s.catalog.photo(lcc_catalog::PhotoId(local)).is_some_and(|p| p.in_library()));
    let _ = std::fs::remove_dir_all(src);
}

/// Restoring a trashed library photo by the content of a browsed file doesn't relink it onto the
/// path the browse record holds: that would leave two records on one file.
#[test]
fn restoring_by_a_browsed_twin_does_not_relink_onto_its_path() {
    let src = temp_dir("browsed-restore");
    let original = src.join("original.png");
    let renamed = src.join("renamed.png");
    write_png(&original, 78);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [original]})).unwrap();
    let old = r["imported"][0].as_u64().unwrap();
    std::fs::rename(&original, &renamed).unwrap();
    s.execute("photo.delete", &json!({"ids": [old]})).unwrap();
    let r = s.execute("library.import", &json!({"paths": [renamed], "local": true})).unwrap();
    let local = r["imported"][0].as_u64().unwrap();
    let r = s.execute("library.import", &json!({"paths": [renamed]})).unwrap();
    assert_eq!(r["restored"], json!([old]), "{r}");
    let on_path = s.catalog.photos().filter(|p| matches!(&p.source, lcc_catalog::Source::File { path } if Path::new(path) == renamed)).count();
    assert_eq!(on_path, 1, "one record holds the renamed file");
    assert!(s.catalog.photo(lcc_catalog::PhotoId(local)).is_some_and(|p| p.local), "the browse record is left alone");
    let _ = std::fs::remove_dir_all(src);
}

#[test]
fn local_twin_does_not_block_reimport_after_permanent_delete_and_restart() {
    use lcc_catalog::PhotoId;
    for mode in ["add", "copy"] {
        let src = temp_dir(&format!("local-twin-src-{mode}"));
        let lib = temp_dir(&format!("local-twin-lib-{mode}"));
        let original = src.join("original.png");
        let twin = src.join("browsed.png");
        write_png(&original, 71);
        std::fs::copy(&original, &twin).unwrap();
        let bytes = std::fs::read(&original).unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let r = s.execute("library.import", &json!({"paths": [original], "mode": mode})).unwrap();
        let old = r["imported"][0].as_u64().unwrap();
        let r = s.execute("library.import", &json!({"paths": [twin], "local": true})).unwrap();
        let local = PhotoId(r["imported"][0].as_u64().unwrap());
        let before = s.catalog.photo(local).unwrap().clone();
        s.execute("photo.delete", &json!({"ids": [old]})).unwrap();
        s.execute("library.source", &json!({"kind": "recentlyDeleted"})).unwrap();
        s.execute("library.select", &json!({"ids": [old]})).unwrap();
        s.execute("photo.deletePermanently", &json!({"ids": [old]})).unwrap();
        s.close_library().unwrap();
        s.open_library(&lib, false).unwrap();
        let scan = s.execute("library.importPreview", &json!({"paths": [original]})).unwrap();
        assert_eq!(scan["duplicates"], 0, "a browsed copy is not a library duplicate: {scan}");
        let r = s.execute("library.import", &json!({"paths": [original], "mode": mode})).unwrap();
        assert_eq!(ids(&r, "imported"), 1, "{r}");
        assert_eq!(ids(&r, "restored"), 0);
        assert_eq!(ids(&r, "duplicates"), 0);
        assert_ne!(r["imported"][0], old);
        assert_eq!(s.catalog.photo(local).unwrap(), &before, "leave the other path's browse record alone");
        assert_eq!(s.catalog.photos().filter(|p| p.in_library()).count(), 1);
        let scan = s.execute("library.importPreview", &json!({"paths": [original]})).unwrap();
        assert_eq!(scan["duplicates"], 1, "the actual library photo still counts");
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
        assert_eq!(std::fs::read(&twin).unwrap(), bytes);
        s.close_library().unwrap();
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(lib);
    }
}

#[test]
fn local_twin_import_matches_the_library_photo_before_promoting() {
    use lcc_catalog::PhotoId;
    let src = temp_dir("local-twin-priority");
    let original = src.join("original.png");
    let twin = src.join("browsed.png");
    write_png(&original, 72);
    std::fs::copy(&original, &twin).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [original]})).unwrap();
    let id = r["imported"][0].as_u64().unwrap();
    s.execute("photo.rate", &json!({"ids": [id], "rating": 5})).unwrap();
    let r = s.execute("library.import", &json!({"paths": [twin], "local": true})).unwrap();
    let local = PhotoId(r["imported"][0].as_u64().unwrap());
    let scan = s.execute("library.importPreview", &json!({"paths": [twin]})).unwrap();
    assert_eq!(scan["duplicates"], 1, "{scan}");
    assert_eq!(scan["candidates"][0]["existing"], id);
    let r = s.execute("library.import", &json!({"paths": [twin]})).unwrap();
    assert_eq!(ids(&r, "imported"), 0);
    assert_eq!(r["duplicates"][0]["existing"], id);
    s.execute("photo.delete", &json!({"ids": [id]})).unwrap();
    let r = s.execute("library.import", &json!({"paths": [twin], "onDeleted": "skip"})).unwrap();
    assert_eq!(ids(&r, "imported"), 0, "skip must not promote the local twin: {r}");
    assert_eq!(r["duplicates"][0]["existingDeleted"], true);
    let r = s.execute("library.import", &json!({"paths": [twin]})).unwrap();
    assert_eq!(r["restored"], json!([id]));
    assert!(s.catalog.photo(local).unwrap().local);
    assert_eq!(s.catalog.photo(PhotoId(id)).unwrap().rating, 5);
    let _ = std::fs::remove_dir_all(src);
}

#[test]
fn local_twins_are_deduplicated_when_promoted_in_one_import() {
    let src = temp_dir("local-twin-batch");
    let a = src.join("a.png");
    let b = src.join("b.png");
    let c = src.join("c.png");
    write_png(&a, 73);
    std::fs::copy(&a, &b).unwrap();
    std::fs::copy(&a, &c).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [a, b], "local": true})).unwrap();
    assert_eq!(ids(&r, "imported"), 2, "browsing shows both paths: {r}");
    let scan = s.execute("library.importPreview", &json!({"paths": [a, b, c]})).unwrap();
    assert_eq!(scan["duplicates"], 2, "only one candidate per content: {scan}");
    let r = s.execute("library.import", &json!({"paths": [a, b, c]})).unwrap();
    assert_eq!(ids(&r, "imported"), 1, "{r}");
    assert_eq!(ids(&r, "duplicates"), 2);
    assert_eq!(s.catalog.photos().filter(|p| p.in_library()).count(), 1);
    assert_eq!(s.catalog.photos().filter(|p| p.local).count(), 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photos().filter(|p| p.in_library()).count(), 0);
    assert_eq!(s.catalog.photos().filter(|p| p.local).count(), 2);
    let _ = std::fs::remove_dir_all(src);
}

#[test]
fn local_twin_promotion_counts_across_background_import_batches() {
    use crate::import::{ImportJob, ImportOptions, commit_prepared};
    let src = temp_dir("local-twin-worker");
    let a = src.join("a.png");
    let b = src.join("b.png");
    let c = src.join("c.png");
    write_png(&a, 74);
    std::fs::copy(&a, &b).unwrap();
    std::fs::copy(&a, &c).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [a, b], "local": true})).unwrap();
    let id = r["imported"][0].as_u64().unwrap();
    s.execute("photo.rate", &json!({"ids": [id], "rating": 4})).unwrap();
    let mut job = ImportJob::new(&mut s, ImportOptions::default()).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let first = job.prepare(&[a.to_string_lossy().to_string()], &cancel);
    let report = commit_prepared(&mut s, &job.opts, job.now(), first).unwrap();
    assert_eq!(report.imported, [id], "promote the existing record with its edits");
    assert_eq!(s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap().rating, 4);
    let next = job.prepare(&[b.to_string_lossy().to_string(), c.to_string_lossy().to_string()], &cancel);
    let report = commit_prepared(&mut s, &job.opts, job.now(), next).unwrap();
    assert!(report.imported.is_empty());
    assert_eq!(report.duplicates.len(), 2);
    assert!(report.duplicates.iter().all(|d| d.existing == Some(id)));
    assert_eq!(s.catalog.photos().filter(|p| p.in_library()).count(), 1);
    let _ = std::fs::remove_dir_all(src);
}

#[test]
fn recursive_import_with_duplicates() {
    let src = temp_dir("src");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("trip/b.png"), 2);
    write_png(&src.join("trip/day2/c.PNG"), 3);
    std::fs::copy(src.join("a.png"), src.join("trip/a-copy.png")).unwrap(); // same bytes
    write_png(&src.join(".hidden/x.png"), 9);
    std::fs::write(src.join("notes.txt"), "not a photo").unwrap();
    std::fs::write(src.join("trip/broken.jpg"), "garbage").unwrap();

    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(r["scanned"], 5, "{r}"); // a, b, c, a-copy, broken (hidden + txt skipped)
    assert_eq!(ids(&r, "imported"), 3, "{r}");
    assert_eq!(ids(&r, "duplicates"), 1, "{r}");
    assert_eq!(r["duplicates"][0]["reason"], "content");
    assert_eq!(ids(&r, "failed"), 1, "{r}");
    assert_eq!(s.catalog.len(), 3);
    assert!(s.catalog.photos().all(|p| p.content_hash.as_ref().is_some_and(|h| h.len() == 32) && p.width == 48));

    // importing again: everything is a duplicate (by path), one undo step for the first import
    let r2 = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(ids(&r2, "imported"), 0);
    assert_eq!(r2["duplicates"].as_array().unwrap().iter().filter(|d| d["reason"] == "path").count(), 3);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.len(), 0);
    let _ = std::fs::remove_dir_all(&src);
}

/// Files whose content a photo already has, imported anyway (`importDuplicates`, as Synchronize
/// Folder does with "Don't import files already in the catalog" unticked): a file with the bytes
/// of a library photo, and two copies of one new picture, each come in as a photo of their own, and
/// the photos stay told apart: no two keep the same content key.
#[test]
fn files_imported_although_a_photo_has_their_content_get_keys_of_their_own() {
    let src = temp_dir("importdups");
    write_png(&src.join("a.png"), 1);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    std::fs::create_dir_all(src.join("topic")).unwrap();
    std::fs::copy(src.join("a.png"), src.join("topic/a.png")).unwrap();
    write_png(&src.join("topic/b.png"), 2);
    std::fs::copy(src.join("topic/b.png"), src.join("topic/b-again.png")).unwrap();
    let topic = src.join("topic").to_string_lossy().to_string();
    // without the param: the copies stay out
    let r = s.execute("library.importPreview", &json!({"paths": [topic]})).unwrap();
    assert_eq!(r["duplicates"], 2, "{r}");
    let r = s.execute("library.import", &json!({"paths": [topic]})).unwrap();
    assert_eq!((ids(&r, "imported"), ids(&r, "duplicates")), (1, 2), "{r}");
    s.execute("edit.undo", &json!({})).unwrap();
    // this import says otherwise: all three come in
    let r = s.execute("library.import", &json!({"paths": [topic], "importDuplicates": true})).unwrap();
    assert_eq!((ids(&r, "imported"), ids(&r, "duplicates")), (3, 0), "{r}");
    assert_eq!(s.catalog.len(), 4);
    let mut keys: Vec<String> = s.catalog.photos().filter_map(|p| p.content_hash.clone()).collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 4, "four photos, four content keys: {keys:?}");
    assert!(!s.import_defaults.import_duplicates, "one import's choice is not remembered by itself");
    // each has its own content key, so caches keyed by content never mix the photos
    let cache_keys: std::collections::HashSet<String> = s.catalog.photos().map(|p| crate::media::content_key(p)).collect();
    assert_eq!(cache_keys.len(), 4, "{cache_keys:?}");
    let _ = std::fs::remove_dir_all(&src);

    // a photo in Recently Deleted is still restored, not copied
    let (mut s, src, id) = trashed_photo("dup-copies-trash");
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "importDuplicates": true})).unwrap();
    assert_eq!(r["restored"], json!([id]), "{r}");
    assert_eq!(s.catalog.len(), 1, "restored, not copied");
    let _ = std::fs::remove_dir_all(&src);
}

/// A library whose only photo of one content is a Duplicate (key `<hash>:dup<id>`): the original
/// with the content's own key was imported first, the copy in `topic/` came in anyway, then the
/// original was removed for good. Returns the session, the folder and the Duplicate's id.
fn only_a_duplicate_keyed_photo_remains(tag: &str) -> (Session, std::path::PathBuf, u64) {
    let src = temp_dir(tag);
    write_png(&src.join("a.png"), 1);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    let original = r["imported"][0].as_u64().unwrap();
    std::fs::create_dir_all(src.join("topic")).unwrap();
    std::fs::copy(src.join("a.png"), src.join("topic/a.png")).unwrap();
    let topic = src.join("topic").to_string_lossy().to_string();
    let r = s.execute("library.import", &json!({"paths": [topic], "importDuplicates": true})).unwrap();
    let twin = r["imported"][0].as_u64().unwrap();
    let key = s.catalog.photo(lcc_catalog::PhotoId(twin)).unwrap().content_hash.clone().unwrap();
    assert!(key.contains(":dup"), "{key}");
    s.execute("photo.delete", &json!({"ids": [original]})).unwrap();
    s.execute("photo.deletePermanently", &json!({"ids": [original]})).unwrap();
    assert!(s.catalog.photo(lcc_catalog::PhotoId(original)).is_none());
    (s, src, twin)
}

/// A Duplicate left as the only photo of its content (key `<hash>:dup<id>`) still stands for that content: in an
/// import, in a Synchronize Folder scan, and when a Fresh import replaces it from the trash.
#[test]
fn a_duplicate_keyed_photo_still_stands_for_its_content() {
    // The Duplicate left alone still has the content: importing another copy without
    // `importDuplicates` finds it and leaves the file out, as it did while the original was there.
    {
        let (mut s, src, twin) = only_a_duplicate_keyed_photo_remains("dupkey-import");
        write_png(&src.join("c.png"), 1);
        let r = s.execute("library.import", &json!({"paths": [src.join("c.png").to_string_lossy()]})).unwrap();
        assert_eq!((ids(&r, "imported"), ids(&r, "duplicates")), (0, 1), "{r}");
        assert_eq!(r["duplicates"][0]["existing"].as_u64(), Some(twin), "{r}");
        let _ = std::fs::remove_dir_all(&src);
    }
    // The same for a Synchronize Folder scan: the copy is a duplicate, not new.
    {
        let (mut s, src, twin) = only_a_duplicate_keyed_photo_remains("dupkey-scan");
        std::fs::copy(src.join("a.png"), src.join("topic/c.png")).unwrap();
        let r = s.execute("folder.scanChanges", &json!({"path": src.join("topic").to_string_lossy()})).unwrap();
        assert_eq!((ids(&r, "new"), ids(&r, "duplicates")), (0, 1), "{r}");
        assert_eq!(r["duplicates"][0]["existing"].as_u64(), Some(twin), "{r}");
        let _ = std::fs::remove_dir_all(&src);
    }
    // A trashed Duplicate (key `<hash>:dup<id>`) is replaced by a Fresh import of a file with its
    // content, like any trashed photo.
    {
        let (mut s, src, twin) = only_a_duplicate_keyed_photo_remains("dupkey-fresh");
        s.execute("photo.delete", &json!({"ids": [twin]})).unwrap();
        write_png(&src.join("c.png"), 1);
        let r = s.execute("library.import", &json!({"paths": [src.join("c.png").to_string_lossy()], "onDeleted": "fresh"})).unwrap();
        assert_eq!(ids(&r, "imported"), 1, "{r}");
        assert!(s.catalog.photo(lcc_catalog::PhotoId(twin)).is_none(), "the trashed record is replaced");
        assert_eq!(s.catalog.len(), 1);
        let _ = std::fs::remove_dir_all(&src);
    }
}

#[test]
fn copy_into_library_and_persist() {
    let src = temp_dir("copysrc");
    let lib = temp_dir("copylib");
    write_png(&src.join("one.png"), 4);
    write_png(&src.join("sub/one.png"), 5); // same name, different content
    let mut s = Session::new().with_fs();
    assert!(s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "mode": "copy"})).is_err(), "copy needs a library");
    s.open_library(&lib, false).unwrap();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "mode": "copy"})).unwrap();
    assert_eq!(ids(&r, "imported"), 2, "{r}");
    let paths: Vec<String> = s
        .catalog
        .photos()
        .map(|p| match &p.source {
            lcc_catalog::Source::File { path } => path.clone(),
            _ => panic!(),
        })
        .collect();
    for p in &paths {
        assert!(Path::new(p).starts_with(lib.join("Originals")), "{p}");
        assert!(Path::new(p).exists());
    }
    assert_ne!(paths[0], paths[1], "unique names");
    // originals deleted from the source: the library copies remain usable after a restart
    std::fs::remove_dir_all(&src).unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.catalog.len(), 2);
    let id = s.catalog.photos().next().unwrap().id;
    assert!(s.render_now(id, 32, 32).is_ok());
    // the library folder itself is never re-imported
    let r = s.execute("library.import", &json!({"paths": [lib.to_string_lossy()]})).unwrap();
    assert_eq!(r["scanned"], 0, "{r}");
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn civil_dates() {
    assert_eq!(crate::import::civil(0), "1970-01-01T00:00:00");
    assert_eq!(crate::import::civil(951_782_400), "2000-02-29T00:00:00");
    assert_eq!(crate::import::civil(1_790_000_000), "2026-09-21T14:13:20");
}

#[test]
fn the_system_clock_is_local_time_with_its_offset() {
    let now = crate::import::system_clock();
    let told = lcc_catalog::dates::instant(&now).unwrap();
    let real = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    assert!((told - real).abs() <= 5, "{now}");
    assert!(matches!(now.get(19..20), Some("+" | "-" | "Z")), "{now}");
}

#[test]
fn exif_clock_import_uses_valid_fallback_dates_and_keeps_raw_tags() {
    const INVALID: &str = "2026:01:01 12:34:56:99";
    const XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:CreateDate="2026-01-05T10:00:00Z"/></rdf:RDF></x:xmpmeta>"#;
    let dir = temp_dir("exif-clock-fallbacks");
    let digitized_path = dir.join("digitized.png");
    let digitized_exif = exif_clock_block(Some(INVALID), Some("2026:01:02 10:00:00"));
    write_png_with_metadata(&digitized_path, 1, Some(&digitized_exif), None);
    let xmp_path = dir.join("xmp.png");
    let malformed_only_exif = exif_clock_block(Some(INVALID), None);
    write_png_with_metadata(&xmp_path, 2, Some(&malformed_only_exif), Some(XMP));

    let mut s = Session::new().with_fs();
    let imported = s.execute("library.import", &json!({"paths": [digitized_path.to_string_lossy(), xmp_path.to_string_lossy()]})).unwrap();
    assert_eq!(ids(&imported, "imported"), 2, "{imported}");
    let digitized = s.catalog.photos().find(|photo| photo.file_name == "digitized.png").unwrap();
    let digitized_id = digitized.id.0;
    assert_eq!(digitized.captured.as_deref(), Some("2026-01-02T10:00:00"));
    let xmp = s.catalog.photos().find(|photo| photo.file_name == "xmp.png").unwrap();
    let xmp_id = xmp.id.0;
    assert_eq!(xmp.captured.as_deref(), Some("2026-01-05T10:00:00+00:00"));

    let all = s.execute("photo.allMetadata", &json!({"id": digitized_id})).unwrap();
    assert!(all["exif"].as_array().unwrap().iter().any(|row| row["tag"] == json!(0x9003) && row["value"] == INVALID), "{all}");
    let all = s.execute("photo.allMetadata", &json!({"id": xmp_id})).unwrap();
    assert!(all["exif"].as_array().unwrap().iter().any(|row| row["tag"] == json!(0x9003) && row["value"] == INVALID), "{all}");
    assert!(all["xmp"].as_array().unwrap().iter().any(|row| row["name"] == "xmp:CreateDate" && row["value"] == "2026-01-05T10:00:00Z"), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exif_clock_malformed_capture_cannot_fabricate_tracklog_match() {
    use lcc_catalog::PhotoId;

    const INVALID: &str = "2026:01:01 12:34:56:99";
    const GPX: &str = r#"<gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1"><trk><trkseg><trkpt lat="45" lon="7"><time>2026-01-01T12:34:56Z</time></trkpt></trkseg></trk></gpx>"#;
    let dir = temp_dir("exif-clock-tracklog");
    let malformed_path = dir.join("malformed.png");
    let malformed_exif = exif_clock_block(Some(INVALID), None);
    write_png_with_metadata(&malformed_path, 3, Some(&malformed_exif), None);
    let valid_path = dir.join("valid.png");
    let valid_exif = exif_clock_block(Some("2026:01:01 12:34:56"), None);
    write_png_with_metadata(&valid_path, 4, Some(&valid_exif), None);

    let mut s = Session::new().with_fs();
    let imported = s.execute("library.import", &json!({"paths": [malformed_path.to_string_lossy()]})).unwrap();
    assert_eq!(ids(&imported, "imported"), 1, "{imported}");
    let bad_id = imported["imported"][0].as_u64().unwrap();
    assert_eq!(
        s.catalog.photo(PhotoId(bad_id)).unwrap().captured,
        lcc_catalog::dates::local(1_768_392_000),
        "malformed EXIF must leave the normal file-date fallback in effect"
    );
    let all = s.execute("photo.allMetadata", &json!({"id": bad_id})).unwrap();
    assert!(
        all["exif"].as_array().unwrap().iter().any(|row| row["tag"] == json!(0x9003) && row["value"] == INVALID),
        "raw EXIF remains available: {all}"
    );

    // Leave both undo and redo populated so a no-match call must preserve the existing history branch.
    s.execute("photo.setMeta", &json!({"ids": [bad_id], "title": "history control"})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    s.execute("library.select", &json!({"ids": [bad_id]})).unwrap();
    assert!(!s.undo.is_empty() && !s.redo.is_empty());
    let state = |session: &Session| {
        (
            session.catalog.to_snapshot(),
            session.catalog.revision,
            session.selection.clone(),
            session.commits(),
            session.undo.iter().map(|entry| (entry.label.clone(), entry.op.clone())).collect::<Vec<_>>(),
            session.redo.iter().map(|entry| (entry.label.clone(), entry.op.clone())).collect::<Vec<_>>(),
        )
    };
    let before = state(&s);
    let dry = s.execute("photo.autoTagTracklog", &json!({"gpx": GPX, "ids": [bad_id], "maxGap": 0, "dryRun": true})).unwrap();
    assert_eq!(dry["tagged"], 0, "{dry}");
    assert_eq!(dry["skipped"]["outside"], 1, "the file-date fallback is outside the one-point track: {dry}");
    assert_eq!(state(&s), before, "dry-run changed catalog, selection or history");
    let actual = s.execute("photo.autoTagTracklog", &json!({"gpx": GPX, "ids": [bad_id], "maxGap": 0})).unwrap();
    assert_eq!(actual["tagged"], 0, "{actual}");
    assert_eq!(actual["skipped"]["outside"], 1, "{actual}");
    assert_eq!(state(&s), before, "unmatched tracklog call changed catalog, selection or history");
    assert_eq!(s.catalog.photo(PhotoId(bad_id)).unwrap().meta.gps, None);

    let imported = s.execute("library.import", &json!({"paths": [valid_path.to_string_lossy()]})).unwrap();
    assert_eq!(ids(&imported, "imported"), 1, "{imported}");
    let valid_id = imported["imported"][0].as_u64().unwrap();
    assert_eq!(s.catalog.photo(PhotoId(valid_id)).unwrap().captured.as_deref(), Some("2026-01-01T12:34:56"));
    s.execute("library.select", &json!({"ids": [valid_id]})).unwrap();
    let undo_before = s.undo.len();
    let tagged = s.execute("photo.autoTagTracklog", &json!({"gpx": GPX, "ids": [valid_id], "maxGap": 0})).unwrap();
    assert_eq!(tagged["tagged"], 1, "{tagged}");
    assert_eq!(s.catalog.photo(PhotoId(valid_id)).unwrap().meta.gps, Some((45.0, 7.0)));
    assert_eq!(s.undo.len(), undo_before + 1, "valid tagging is one undo step");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(PhotoId(valid_id)).unwrap().meta.gps, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn browsing_a_folder_lists_its_photos_without_adding_them() {
    use crate::LibrarySource;
    let dir = std::env::temp_dir().join(format!("lc-browse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let png = |p: &std::path::Path, seed: u8| {
        let img = lcc_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, seed, 255]);
        let b = crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() })
            .unwrap();
        std::fs::write(p, b).unwrap();
    };
    png(&dir.join("a.png"), 1);
    png(&dir.join("b.png"), 2);
    png(&dir.join("sub/c.png"), 3);
    std::fs::write(dir.join("notes.txt"), "x").unwrap();
    let mut s = Session::new().with_fs();
    // flat, given: a first browse takes subfolders by default (tests_import_local covers that)
    let r = s.execute("library.browse", &serde_json::json!({"path": dir.to_string_lossy(), "subfolders": false})).unwrap();
    assert_eq!(r["photos"], 2, "{r}");
    assert_eq!(s.source, LibrarySource::Folder);
    assert_eq!(s.visible_cloned().len(), 2);
    // not in the library
    s.execute("library.source", &serde_json::json!({"kind": "all"})).unwrap();
    assert!(s.visible_cloned().is_empty(), "browsed photos stay out of All Photos");
    assert_eq!(s.execute("catalog.stats", &serde_json::json!({})).unwrap()["photos"], 0, "nor in the counts");
    assert!(s.catalog.date_groups().is_empty());
    // subfolders; browsing again reuses the photos
    let r = s.execute("library.browse", &serde_json::json!({"path": dir.to_string_lossy(), "subfolders": true})).unwrap();
    assert_eq!((r["photos"].as_u64(), r["new"].as_u64()), (Some(3), Some(1)));
    // add one to the library
    let first = s.visible_cloned()[0];
    s.execute("photo.addToLibrary", &serde_json::json!({"ids": [first.0]})).unwrap();
    // importing the folder for real brings in the rest (no duplicates of the browsed ones)
    let r = s.execute("library.import", &serde_json::json!({"paths": [dir.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(2), "{r}");
    s.execute("library.source", &serde_json::json!({"kind": "all"})).unwrap();
    assert_eq!(s.visible_cloned().len(), 3);
    assert_eq!(s.catalog.photos().count(), 3);
    assert!(s.execute("library.browse", &serde_json::json!({"path": dir.join("a.png").to_string_lossy()})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// PERF-CULL-1: the Local view reads only what the library lacks, in the sort's file-time order
/// for the date sorts, so the first screen is read first; other sorts follow their key.
#[test]
fn browsing_queues_unknown_files_in_the_order_the_view_shows_them() {
    use lcc_catalog::{Sort, SortKey};
    let dir = temp_dir("browse-order");
    let set_time = |p: &Path, secs: u64| {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    };
    for (name, seed, secs) in [("a.png", 1u8, 300u64), ("b.png", 2, 100), ("c.png", 3, 200)] {
        write_png(&dir.join(name), seed);
        set_time(&dir.join(name), 1_700_000_000 + secs);
    }
    let mut s = Session::new().with_fs();
    let known = dir.join("b.png").to_string_lossy().to_string();
    s.execute("library.import", &json!({"paths": [known.clone()]})).unwrap();
    let files: Vec<String> = ["a.png", "b.png", "c.png", "gone.png"].iter().map(|n| dir.join(n).to_string_lossy().to_string()).collect();
    let (input, _) = crate::import::ScanInput::new(&mut s, &[]);
    let names = |v: Vec<String>| v.iter().map(|f| Path::new(f).file_name().unwrap().to_string_lossy().to_string()).collect::<Vec<_>>();
    // newest first, the file already in the library left out, an unreadable one last
    // the listing keeps the known file, so a browse doesn't forget its record
    let all = crate::import::browse_candidates(&input, files.clone(), &Sort::default());
    assert!(all.iter().any(|c| c.path == known && c.duplicate.is_some()));
    let q = crate::import::browse_queue(&input, files.clone(), &Sort { ascending: false, ..Sort::default() });
    assert_eq!(names(q), ["a.png", "c.png", "gone.png"]);
    let q = crate::import::browse_queue(&input, files.clone(), &Sort { ascending: true, ..Sort::default() });
    assert_eq!(names(q), ["c.png", "a.png", "gone.png"]);
    let q = crate::import::browse_queue(&input, files.clone(), &Sort { key: SortKey::FileName, ascending: false, ..Sort::default() });
    assert_eq!(names(q), ["gone.png", "c.png", "a.png"]);
    let q = crate::import::browse_queue(&input, files, &Sort { key: SortKey::Rating, ..Sort::default() });
    assert_eq!(names(q), ["a.png", "c.png", "gone.png"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Photos a.png, b.png, c.png imported from `old`, then: a renamed into `moved`; b renamed, with
/// an impostor (same name and size, other bytes) under its old name; c gone, with two impostors.
fn renamed_and_ambiguous_scene(tag: &str) -> (std::path::PathBuf, Session) {
    let dir = temp_dir(tag);
    std::fs::create_dir_all(dir.join("old")).unwrap();
    std::fs::create_dir_all(dir.join("moved/real")).unwrap();
    for (n, seed) in [("a.png", 1), ("b.png", 2), ("c.png", 3)] {
        write_png(&dir.join("old").join(n), seed);
    }
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.join("old").to_string_lossy()]})).unwrap();
    let impostor = |src: &Path, dst: &Path| {
        let mut b = std::fs::read(src).unwrap();
        let k = b.len() - 20;
        b[k] ^= 0xff;
        std::fs::write(dst, b).unwrap();
    };
    std::fs::rename(dir.join("old/a.png"), dir.join("moved/Trip-001.png")).unwrap();
    impostor(&dir.join("old/b.png"), &dir.join("moved/b.png"));
    std::fs::rename(dir.join("old/b.png"), dir.join("moved/real/Trip-002.png")).unwrap();
    impostor(&dir.join("old/c.png"), &dir.join("moved/c.png"));
    impostor(&dir.join("old/c.png"), &dir.join("moved/real/c.png"));
    std::fs::remove_file(dir.join("old/c.png")).unwrap();
    (dir, s)
}

/// A renamed file whose size is unknown (0) but whose content hash is known is still found by
/// content: the hash alone is enough to confirm it is the same photo.
#[test]
fn find_missing_matches_renamed_file_by_content_without_size() {
    use lcc_catalog::{Op, Photo, Source};
    let dir = temp_dir("missing-no-size");
    std::fs::create_dir_all(dir.join("found")).unwrap();
    let content = b"same pixels as the original";
    let renamed = dir.join("found").join("Trip-001.png");
    std::fs::write(&renamed, content).unwrap();
    let hash = lcc_preview::hash_bytes(content).to_string();
    let mut s = Session::new();
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(
        id,
        Source::File { path: dir.join("gone").join("orig.png").to_string_lossy().into_owned() },
        "orig.png",
        "PNG",
        48,
        32,
        "2026-01-01T10:00:00",
    );
    p.file_size = 0;
    p.content_hash = Some(hash);
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let r = s.execute("library.findMissing", &json!({"folder": dir.join("found").to_string_lossy()})).unwrap();
    let found = r["found"].as_array().map(Vec::len);
    assert_eq!(found, Some(1), "renamed file matched by content without a recorded size: {r}");
    assert_eq!(r["found"][0]["to"], json!(renamed.to_string_lossy().to_string()), "{r}");
    assert_eq!(r["found"][0]["by"], json!("content"), "{r}");
    assert_eq!(r["missing"], 0, "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issues #105, #104: the app plans Find Missing on a worker thread (no session) and relinks the plan
/// afterwards; that gives exactly what the synchronous command gives — renamed files by content,
/// impostors and look-alikes skipped (`ambiguous`).
#[test]
fn find_missing_planned_on_a_worker_matches_the_command() {
    use crate::cmd::missing::{find_candidates, plan_find_missing};
    let (dir, mut s) = renamed_and_ambiguous_scene("missing-worker");
    let folder = dir.join("moved").to_string_lossy().to_string();
    let candidates = find_candidates(&s.catalog);
    let f = folder.clone();
    let plan = std::thread::spawn(move || plan_find_missing(&candidates, &f)).join().unwrap().unwrap();
    assert_eq!((plan.found.len(), plan.missing, plan.ambiguous.len()), (2, 1, 1), "{plan:?}");
    let applied = s.execute("library.findMissing", &plan.to_json()).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.execute("library.missing", &json!({})).unwrap().as_array().map(Vec::len), Some(3), "undo restores all three");
    let direct = s.execute("library.findMissing", &json!({"folder": folder})).unwrap();
    assert_eq!(applied, direct);
    assert_eq!(direct["found"].as_array().map(Vec::len), Some(2), "{direct}");
    assert_eq!(direct["ambiguous"].as_array().map(Vec::len), Some(1), "{direct}");
    // Issue #105: renamed files are found by content, the content match wins over a same-name same-size impostor,
    // and photos it can't tell apart are skipped and reported
    let found = direct["found"].as_array().unwrap();
    let to = |name: &str| {
        found.iter().find(|f| f["from"].as_str().unwrap().ends_with(name)).map(|f| (f["to"].as_str().unwrap().to_string(), f["by"].clone()))
    };
    assert_eq!(to("a.png"), Some((dir.join("moved").join("Trip-001.png").to_string_lossy().to_string(), json!("content"))));
    assert_eq!(
        to("b.png"),
        Some((dir.join("moved").join("real").join("Trip-002.png").to_string_lossy().to_string(), json!("content"))),
        "not the impostor"
    );
    assert_eq!(direct["missing"], 1);
    assert_eq!(direct["ambiguous"][0]["candidates"].as_array().map(Vec::len), Some(2), "{direct}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #104: a plan made on a worker is re-checked when it is applied: a photo relinked
/// meanwhile is left alone, and a file that is now another photo's original isn't used.
#[test]
fn find_missing_plan_is_rechecked_when_applied() {
    use crate::cmd::missing::{find_candidates, plan_find_missing};
    let (dir, mut s) = renamed_and_ambiguous_scene("missing-recheck");
    let plan = plan_find_missing(&find_candidates(&s.catalog), &dir.join("moved").to_string_lossy()).unwrap();
    let to = |name: &str| plan.found.iter().find(|f| f.from.ends_with(name)).unwrap().clone();
    let (a, b) = (to("a.png"), to("b.png"));
    // meanwhile: a is relinked by hand (elsewhere), and the ambiguous c is pointed at b's file
    std::fs::copy(&a.to, dir.join("a-elsewhere.png")).unwrap();
    s.execute("photo.relink", &json!({"id": a.id.0, "path": dir.join("a-elsewhere.png").to_string_lossy()})).unwrap();
    let c = plan.ambiguous[0].id;
    s.execute("photo.relink", &json!({"id": c.0, "path": b.to})).unwrap();
    let r = s.execute("library.findMissing", &plan.to_json()).unwrap();
    assert_eq!(r["found"].as_array().map(Vec::len), Some(0), "{r}");
    assert_eq!(r["ambiguous"].as_array().map(Vec::len), Some(0), "c isn't missing any more: {r}");
    assert_eq!(r["missing"], 1, "b stays missing: its file is c's now: {r}");
    let path_of = |id: lcc_catalog::PhotoId| match &s.catalog.photo(id).unwrap().source {
        lcc_catalog::Source::File { path } => path.clone(),
        _ => String::new(),
    };
    assert_eq!(path_of(a.id), dir.join("a-elsewhere.png").to_string_lossy());
    assert_eq!(path_of(b.id), b.from);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library made on Windows names its originals with Windows paths; opened on macOS or Linux
/// they are missing until Find Missing finds them by name, there as everywhere: the name follows
/// the last `/` or `\`, any letter case, the size checked when one is on record, look-alikes
/// reported rather than guessed, and undo puts back the path exactly as spelled.
#[test]
fn find_missing_finds_the_files_of_a_library_made_on_windows() {
    use lcc_catalog::{Op, Photo, PhotoId, Source};
    let dir = temp_dir("missing-windows");
    for (rel, bytes) in [("found/a.jpg", &b"x"[..]), ("found/deep/b.jpg", b"xyz"), ("found/one/twin.jpg", b"x"), ("found/two/twin.jpg", b"x")] {
        std::fs::create_dir_all(dir.join(rel).parent().unwrap()).unwrap();
        std::fs::write(dir.join(rel), bytes).unwrap();
    }
    let mut s = Session::new();
    // as a Windows library may list them: no content hash, and a size only for b
    let mut add = |path: &str, size: u64| {
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: path.into() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
        p.file_size = size;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let (a, b, twin) = (add(r"C:\Users\me\Photos\a.jpg", 0), add(r"D:\Shoot\2019\B.JPG", 3), add(r"C:\Users\me\Photos\twin.jpg", 0));
    let path_of = |s: &Session, id: PhotoId| match &s.catalog.photo(id).unwrap().source {
        Source::File { path } => path.clone(),
        _ => String::new(),
    };
    let r = s.execute("library.findMissing", &json!({"folder": dir.join("found").to_string_lossy()})).unwrap();
    assert_eq!(path_of(&s, a), dir.join("found").join("a.jpg").to_string_lossy(), "found by its name: {r}");
    assert_eq!(path_of(&s, b), dir.join("found").join("deep").join("b.jpg").to_string_lossy(), "found by its name, any case, and size: {r}");
    assert_eq!(path_of(&s, twin), r"C:\Users\me\Photos\twin.jpg", "two files of its name: not guessed: {r}");
    assert_eq!(r["ambiguous"][0]["candidates"].as_array().map(Vec::len), Some(2), "both look-alikes reported: {r}");
    assert_eq!(r["missing"], 1, "{r}");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(
        (path_of(&s, a), path_of(&s, b)),
        (r"C:\Users\me\Photos\a.jpg".to_string(), r"D:\Shoot\2019\B.JPG".to_string()),
        "one undo step puts back the Windows paths as they were spelled"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #2853: virtual copies share their original's file, so Find Missing relinks them to it,
/// both when the original is found in the same search and when it was relinked before. A file an
/// unrelated photo uses is still refused.
#[test]
fn find_missing_relinks_virtual_copies_to_their_originals_file() {
    use lcc_catalog::{Op, Photo, PhotoId, Source};
    let dir = temp_dir("missing-copies");
    std::fs::create_dir_all(dir.join("new")).unwrap();
    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        std::fs::write(dir.join("new").join(name), b"x").unwrap();
    }
    let old = |name: &str| dir.join("old").join(name).to_string_lossy().to_string();
    let new = |name: &str| dir.join("new").join(name).to_string_lossy().to_string();
    let mut s = Session::new();
    let mut add = |path: String, copy_of: Option<PhotoId>| {
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
        p.file_size = 1;
        p.copy_of = copy_of;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    // a and its two copies are all missing; b was relinked already, its copy wasn't; c's file
    // is used by an unrelated photo
    let a = add(old("a.jpg"), None);
    let a_copies = [add(old("a.jpg"), Some(a)), add(old("a.jpg"), Some(a))];
    let b = add(new("b.jpg"), None);
    let b_copy = add(old("b.jpg"), Some(b));
    add(new("c.jpg"), None);
    let stranger = add(old("c.jpg"), None);
    let path_of = |s: &Session, id: PhotoId| match &s.catalog.photo(id).unwrap().source {
        Source::File { path } => path.clone(),
        _ => String::new(),
    };
    let r = s.execute("library.findMissing", &json!({"folder": dir.join("new").to_string_lossy()})).unwrap();
    for id in [a, a_copies[0], a_copies[1]] {
        assert_eq!(path_of(&s, id), new("a.jpg"), "{r}");
    }
    assert_eq!(path_of(&s, b_copy), new("b.jpg"), "{r}");
    assert_eq!(path_of(&s, stranger), old("c.jpg"), "another photo's file is not taken: {r}");
    assert_eq!(r["missing"], 1, "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// On macOS and Linux `\` may be part of a file name. Find Missing names the files it finds by
/// the same rule as the missing photos, so `a\b.jpg` isn't relinked to a `b.jpg` beside it
/// unasked: the two are reported as look-alikes.
#[cfg(unix)]
#[test]
fn find_missing_does_not_take_a_look_alike_for_a_file_name_with_a_backslash() {
    use lcc_catalog::{Op, Photo, Source};
    let dir = temp_dir("missing-backslash");
    std::fs::create_dir_all(dir.join("found")).unwrap();
    for name in [r"a\b.jpg", "b.jpg"] {
        std::fs::write(dir.join("found").join(name), b"x").unwrap();
    }
    let mut s = Session::new();
    let gone = dir.join("gone").join(r"a\b.jpg").to_string_lossy().to_string();
    let id = s.catalog.alloc_photo_id();
    let p = Photo::new(id, Source::File { path: gone.clone() }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let r = s.execute("library.findMissing", &json!({"folder": dir.join("found").to_string_lossy()})).unwrap();
    assert!(matches!(&s.catalog.photo(id).unwrap().source, Source::File { path } if *path == gone), "not relinked to b.jpg: {r}");
    assert_eq!(r["ambiguous"][0]["candidates"].as_array().map(Vec::len), Some(2), "both reported: {r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A record made without its file being read (a Lightroom catalog import whose original wasn't
/// there) has no exposure information. Locating the file, by hand or with Find Missing Photos,
/// reads it from the file; undo takes the record back to what it was (adapted from upstream #3969).
#[test]
fn a_found_photo_reads_the_exposure_its_record_never_had() {
    use lcc_catalog::{Op, Photo, Source};
    use lcc_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value as Tv};
    let dir = temp_dir("found-reads-exposure");
    let mut exif = IfdBuilder::new();
    exif.set(0x829a, Tv::Rational(vec![(1, 250)]));
    exif.set(0x829d, Tv::Rational(vec![(28, 10)]));
    exif.set(0x8827, Tv::Short(vec![400]));
    exif.set(0x920a, Tv::Rational(vec![(50, 1)]));
    let mut ifd0 = IfdBuilder::new();
    ifd0.set(0x010f, Tv::Ascii("Canon".into()));
    ifd0.set(0x0110, Tv::Ascii("Canon EOS 5D".into()));
    ifd0.set_child(0x8769, exif);
    let block = TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap();
    let mut s = Session::new().with_fs();
    let mut ids = Vec::new();
    for (name, seed) in [("a.png", 1), ("b.png", 2)] {
        let file = dir.join("found").join(name);
        write_png_with_metadata(&file, seed, Some(&block), None);
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(
            id,
            Source::File { path: dir.join("gone").join(name).to_string_lossy().to_string() },
            name,
            "PNG",
            48,
            32,
            "2026-01-01T00:00:00",
        );
        p.file_size = std::fs::metadata(&file).unwrap().len();
        s.commit("Add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    let exposure = |p: &Photo| (p.meta.shutter.clone(), p.meta.aperture, p.meta.iso, p.meta.focal_mm);
    assert_eq!(exposure(s.catalog.photo(ids[0]).unwrap()), (String::new(), None, None, None));
    s.execute("photo.relink", &json!({"id": ids[0].0, "path": dir.join("found/a.png").to_string_lossy()})).unwrap();
    let a = s.catalog.photo(ids[0]).unwrap();
    assert_eq!(exposure(a), ("1/250".into(), Some(2.8), Some(400), Some(50.0)), "Locate reads the file's exposure");
    let r = s.execute("library.findMissing", &json!({"folder": dir.join("found").to_string_lossy()})).unwrap();
    assert_eq!(r["found"].as_array().map(Vec::len), Some(1), "{r}");
    let b = s.catalog.photo(ids[1]).unwrap();
    assert_eq!(exposure(b), ("1/250".into(), Some(2.8), Some(400), Some(50.0)), "Find Missing Photos reads the file's exposure");
    s.execute("edit.undo", &json!({})).unwrap();
    let b = s.catalog.photo(ids[1]).unwrap();
    assert_eq!(exposure(b), (String::new(), None, None, None));
    assert!(matches!(&b.source, Source::File { path } if path.contains("gone")), "{:?}", b.source);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_files_are_found_and_relinked() {
    let dir = std::env::temp_dir().join(format!("lc-missing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("old")).unwrap();
    std::fs::create_dir_all(dir.join("moved/deeper")).unwrap();
    let png = |p: &std::path::Path, seed: u8| {
        let img = lcc_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, seed, 255]);
        let b = crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() })
            .unwrap();
        std::fs::write(p, b).unwrap();
    };
    png(&dir.join("old/a.png"), 1);
    png(&dir.join("old/b.png"), 2);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &serde_json::json!({"paths": [dir.join("old").to_string_lossy()]})).unwrap();
    assert_eq!(s.execute("library.missing", &serde_json::json!({})).unwrap().as_array().map(Vec::len), Some(0));
    // the files move away
    std::fs::rename(dir.join("old/a.png"), dir.join("moved/a.png")).unwrap();
    std::fs::rename(dir.join("old/b.png"), dir.join("moved/deeper/b.png")).unwrap();
    let m = s.execute("library.missing", &serde_json::json!({})).unwrap();
    assert_eq!(m.as_array().map(Vec::len), Some(2));
    s.execute("library.source", &serde_json::json!({"kind": "missing"})).unwrap();
    assert_eq!(s.visible_cloned().len(), 2, "the Missing Photos source lists them");
    s.execute("library.source", &serde_json::json!({"kind": "all"})).unwrap();
    // one by hand
    let id = m[0]["id"].as_u64().unwrap();
    let name = std::path::Path::new(m[0]["path"].as_str().unwrap()).file_name().unwrap().to_string_lossy().to_string();
    let new = if name == "a.png" { dir.join("moved/a.png") } else { dir.join("moved/deeper/b.png") };
    s.execute("photo.relink", &serde_json::json!({"id": id, "path": new.to_string_lossy()})).unwrap();
    assert_eq!(s.execute("library.missing", &serde_json::json!({})).unwrap().as_array().map(Vec::len), Some(1));
    assert!(s.execute("photo.relink", &serde_json::json!({"id": id, "path": dir.join("nope.png").to_string_lossy()})).is_err());
    // the rest by searching a folder
    let r = s.execute("library.findMissing", &serde_json::json!({"folder": dir.join("moved").to_string_lossy()})).unwrap();
    assert_eq!((r["found"].as_array().map(Vec::len), r["missing"].as_u64()), (Some(1), Some(0)), "{r}");
    // undo points the photo back at the old path but doesn't move any file
    s.execute("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(s.execute("library.missing", &serde_json::json!({})).unwrap().as_array().map(Vec::len), Some(1));
    assert!(dir.join("moved/a.png").exists() && dir.join("moved/deeper/b.png").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recently_added_covers_recent_imports_newest_first() {
    let dir = std::env::temp_dir().join(format!("lc-recent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let png = |name: &str, seed: u8| {
        let img = lcc_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, seed, 255]);
        let b = crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() })
            .unwrap();
        std::fs::write(dir.join(name), b).unwrap();
        dir.join(name).to_string_lossy().to_string()
    };
    let mut s = Session::new().with_fs();
    // three imports: 60 days ago, 10 days ago, today
    for (when, name, seed) in [("2026-08-01T09:00:00", "old.png", 1), ("2026-09-20T09:00:00", "mid.png", 2), ("2026-09-30T09:00:00", "new.png", 3)] {
        let path = png(name, seed);
        let w = when.to_string();
        s.clock = Box::new(move || w.clone());
        s.execute("library.import", &serde_json::json!({"paths": [path]})).unwrap();
    }
    s.execute("library.source", &serde_json::json!({"kind": "recentlyAdded"})).unwrap();
    let names: Vec<String> = s.visible_cloned().iter().map(|id| s.catalog.photo(*id).unwrap().file_name.clone()).collect();
    assert_eq!(names, ["new.png", "mid.png"], "the last 30 days, newest import first");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recently_added_keeps_an_import_batch_in_file_order() {
    use lcc_catalog::{Op, Photo, PhotoId, Source};
    let mut s = Session::new();
    // one import batch: one `imported` string, ids running opposite to the names
    let mut by_name = Vec::new();
    for (n, name) in ["IMG_0001.png", "IMG_0002.png", "IMG_0003.png", "IMG_0004.png"].iter().enumerate() {
        let id = PhotoId(5000 - n as u64);
        let p = Photo::new(id, Source::File { path: name.to_string() }, name, "PNG", 16, 12, "2026-09-30T09:00:00");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        by_name.push(name.to_string());
    }
    s.clock = Box::new(|| "2026-10-01T09:00:00".to_string());
    for ascending in [true, false] {
        s.execute("library.sort", &serde_json::json!({"key": "captureDate", "ascending": ascending})).unwrap();
        s.execute("library.source", &serde_json::json!({"kind": "recentlyAdded"})).unwrap();
        let names: Vec<String> = s.visible_cloned().iter().map(|id| s.catalog.photo(*id).unwrap().file_name.clone()).collect();
        assert_eq!(names, by_name, "ascending: {ascending}");
    }
}

/// Copy imports: a destination folder, flat / by-month folders, renamed copies (numbered in
/// import order), and a metadata preset on every photo.
#[test]
fn copy_with_destination_organize_rename_and_metadata_preset() {
    let src = temp_dir("orgsrc");
    let dest = temp_dir("orgdest");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    s.execute("metadata.savePreset", &json!({"name": "Studio", "fields": {"copyright": "© Studio", "creator": "Sam"}})).unwrap();
    assert!(s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "metadataPreset": "Nope"})).is_err());
    assert!(s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "mode": "copy", "organize": "weekly"})).is_err());
    let r = s
        .execute(
            "library.import",
            &json!({"paths": [src.to_string_lossy()], "mode": "copy", "destination": dest.to_string_lossy(), "organize": "flat",
                    "rename": "Shoot-{seq:3}", "renameStart": 7, "metadataPreset": "Studio"}),
        )
        .unwrap();
    assert_eq!(ids(&r, "imported"), 2, "{r}");
    let mut names: Vec<String> = s.catalog.photos().map(|p| p.file_name.clone()).collect();
    names.sort();
    assert_eq!(names, ["Shoot-007.png", "Shoot-008.png"], "catalogued under the new names");
    assert!(dest.join("Shoot-007.png").exists() && dest.join("Shoot-008.png").exists(), "flat: straight into the destination");
    assert!(s.catalog.photos().all(|p| p.meta.copyright == "© Studio" && p.meta.creator == "Sam"));
    // by month, no library needed when a destination is given
    let dest2 = temp_dir("orgdest2");
    write_png(&src.join("c.png"), 3);
    let r = s
        .execute(
            "library.import",
            &json!({"paths": [src.join("c.png").to_string_lossy()], "mode": "copy", "destination": dest2.to_string_lossy(), "organize": "month"}),
        )
        .unwrap();
    assert_eq!(ids(&r, "imported"), 1, "{r}");
    let p = s.catalog.photos().find(|p| p.file_name == "c.png").unwrap();
    let lcc_catalog::Source::File { path } = &p.source else { panic!() };
    let rel = Path::new(path).strip_prefix(&dest2).unwrap();
    assert_eq!(rel.components().count(), 3, "YYYY/YYYY-MM/c.png: {rel:?}");
    assert_eq!(rel.parent().unwrap().file_name().unwrap().len(), 7);
    for d in [&src, &dest, &dest2] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// The rename counter at the end of `usize`: the last value is used once (one undo step, the copy stays on
/// disk); the file after it fails with an actionable message and the earlier successes are kept.
#[test]
fn import_rename_sequence_uses_the_last_value_once_then_reports_exhaustion() {
    {
        let base = temp_dir("rename-sequence-last");
        let (src, dest, lib) = (base.join("source"), base.join("destination"), base.join("library"));
        write_png(&src.join("one.png"), 1);
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();

        let last = usize::MAX;
        let r = s
            .execute(
                "library.import",
                &json!({
                    "paths": [src.join("one.png").to_string_lossy()],
                    "mode": "copy", "destination": dest.to_string_lossy(), "organize": "flat",
                    "rename": "Shot-{seq}", "renameStart": last as u64
                }),
            )
            .unwrap();

        assert_eq!(ids(&r, "imported"), 1, "{r}");
        assert_eq!(r["failed"].as_array().map(Vec::len), Some(0), "{r}");
        assert!(dest.join(format!("Shot-{last}.png")).is_file());
        assert_eq!(s.catalog.len(), 1);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.catalog.len(), 0, "the import remains one undo step");
        assert!(dest.join(format!("Shot-{last}.png")).is_file(), "undo leaves a successful copy on disk");
        let _ = std::fs::remove_dir_all(base);
    }
    {
        let base = temp_dir("rename-sequence-exhausted");
        let (src, dest, lib) = (base.join("source"), base.join("destination"), base.join("library"));
        let paths: Vec<_> = (1..=3)
            .map(|i| {
                let path = src.join(format!("{i:02}.png"));
                write_png(&path, i as u8);
                path
            })
            .collect();
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();

        let start = usize::MAX - 1;
        let r = s
            .execute(
                "library.import",
                &json!({
                    "paths": paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
                    "mode": "copy", "destination": dest.to_string_lossy(), "organize": "flat",
                    "rename": "Shot-{seq}", "renameStart": start as u64
                }),
            )
            .unwrap();

        assert_eq!(ids(&r, "imported"), 2, "the final two representable values are used: {r}");
        assert_eq!(r["failed"].as_array().map(Vec::len), Some(1), "{r}");
        let failure = r["failed"][0][1].as_str().unwrap_or_default();
        assert!(failure.contains("sequence") && failure.contains("renameStart"), "actionable failure: {failure}");
        assert_eq!(s.catalog.len(), 2);
        assert!(dest.join(format!("Shot-{start}.png")).is_file());
        assert!(dest.join(format!("Shot-{}.png", usize::MAX)).is_file());
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 2, "the exhausted file gets no destination entry");
        assert!(!paths[2].file_name().is_some_and(|name| dest.join(name).exists()), "the exhausted source gets no copy");
        let _ = std::fs::remove_dir_all(base);
    }
}

#[test]
fn import_rename_sequence_keeps_flushed_copy_and_move_results() {
    for mode in ["copy", "move"] {
        let base = temp_dir(&format!("rename-sequence-flush-{mode}"));
        let (src, dest, lib) = (base.join("source"), base.join("destination"), base.join("library"));
        let paths: Vec<_> = (1..=4)
            .map(|i| {
                let path = src.join(format!("{i:02}.png"));
                write_png(&path, i as u8);
                path
            })
            .collect();
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();

        let start = usize::MAX - 2;
        let r = s
            .execute(
                "library.import",
                &json!({
                    "paths": paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
                    "mode": mode, "destination": dest.to_string_lossy(), "organize": "flat",
                    "rename": "Holiday", "renameStart": start as u64
                }),
            )
            .unwrap();

        assert_eq!(ids(&r, "imported"), 3, "{mode}: earlier transfers survive sequence exhaustion: {r}");
        assert_eq!(r["failed"].as_array().map(Vec::len), Some(1), "{mode}: {r}");
        assert_eq!(s.catalog.len(), 3, "{mode}: all successful transfers are catalogued");
        let mut output_names: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        output_names.sort();
        assert_eq!(output_names, ["Holiday-1.png", "Holiday-2.png", "Holiday.png"], "{mode}");
        match mode {
            "copy" => assert!(paths.iter().all(|path| path.is_file()), "copy leaves every source in place"),
            "move" => {
                assert!(paths.iter().take(3).all(|path| !path.exists()), "successful moves remove sources after cataloguing");
                assert!(paths[3].is_file(), "the failed source remains in place");
                assert_eq!(r["moved"].as_array().map(Vec::len), Some(3), "{r}");
            }
            _ => panic!("unexpected case in a test"),
        }
        let _ = std::fs::remove_dir_all(base);
    }
}

#[test]
fn import_rename_sequence_exhaustion_persists_across_batches() {
    let base = temp_dir("rename-sequence-batches");
    let (src, dest, lib) = (base.join("source"), base.join("destination"), base.join("library"));
    let first = src.join("one.png");
    let second = src.join("two.png");
    write_png(&first, 1);
    write_png(&second, 2);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let opts = crate::import::ImportOptions {
        mode: crate::import::ImportMode::Copy,
        destination: Some(dest.to_string_lossy().into_owned()),
        organize: crate::import::Organize::Flat,
        rename: Some("Shot-{seq}".into()),
        rename_start: usize::MAX,
        ..Default::default()
    };
    let mut job = crate::import::ImportJob::new(&mut s, opts).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);

    let prepared = job.prepare(std::slice::from_ref(&first.to_string_lossy().to_string()), &cancel);
    let first_result = crate::import::commit_prepared(&mut s, &job.opts, job.now(), prepared).unwrap();
    assert_eq!(ids(&serde_json::to_value(first_result).unwrap(), "imported"), 1);
    let prepared = job.prepare(std::slice::from_ref(&second.to_string_lossy().to_string()), &cancel);
    let second_result = crate::import::commit_prepared(&mut s, &job.opts, job.now(), prepared).unwrap();
    let second_result = serde_json::to_value(second_result).unwrap();

    assert_eq!(ids(&second_result, "imported"), 0, "the exhausted job does not restart its sequence");
    assert_eq!(second_result["failed"].as_array().map(Vec::len), Some(1), "{second_result}");
    assert_eq!(s.catalog.len(), 1);
    assert!(dest.join(format!("Shot-{}.png", usize::MAX)).is_file());
    assert!(!dest.join("Shot-1.png").exists(), "no wraparound or reuse");
    assert_eq!(
        (job.done.load(std::sync::atomic::Ordering::Relaxed), job.total.load(std::sync::atomic::Ordering::Relaxed)),
        (2, 2),
        "both the copied and exhausted files advance import progress"
    );
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn import_rename_sequence_controls_ignore_the_counter_when_not_renaming() {
    let base = temp_dir("rename-sequence-controls");
    let (src, dest, lib) = (base.join("source"), base.join("destination"), base.join("library"));
    let added = src.join("add.png");
    let copied = src.join("copy.png");
    write_png(&added, 1);
    write_png(&copied, 2);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();

    let added_report = s
        .execute(
            "library.import",
            &json!({
                "paths": [added.to_string_lossy()], "mode": "add", "rename": "Ignored-{seq}", "renameStart": usize::MAX as u64
            }),
        )
        .unwrap();
    let copied_report = s
        .execute(
            "library.import",
            &json!({
                "paths": [copied.to_string_lossy()], "mode": "copy", "destination": dest.to_string_lossy(), "organize": "flat",
                "renameStart": usize::MAX as u64
            }),
        )
        .unwrap();

    assert_eq!(ids(&added_report, "imported"), 1, "Add does not use rename numbering: {added_report}");
    assert_eq!(ids(&copied_report, "imported"), 1, "an empty rename template does not use the counter: {copied_report}");
    assert!(dest.join("copy.png").is_file());
    assert_eq!(s.catalog.len(), 2);
    let _ = std::fs::remove_dir_all(base);
}

#[cfg(target_pointer_width = "32")]
#[test]
fn import_rename_sequence_rejects_a_start_outside_usize() {
    let s = Session::new();
    let too_large = u64::from(u32::MAX) + 1;
    let parsed = crate::cmd::library::import_params(&s, &json!({"paths": ["unused.png"], "renameStart": too_large}));
    assert!(parsed.is_err(), "32-bit import parameters must not truncate renameStart");
}

/// Copy with a custom folder template: `{date:%Y}/{date:%Y%m%d}` files a photo under
/// `2026/20260114/` (an undated file by the import time); templates that would leave the
/// destination are refused.
#[test]
fn copy_with_a_folder_template() {
    let src = temp_dir("tplsrc");
    let dest = temp_dir("tpldest");
    write_png(&src.join("a.png"), 1);
    let mut s = Session::new().with_fs();
    s.clock = Box::new(|| "2026-01-14T05:58:48".to_string());
    for bad in ["../{date}", "/tmp/{date}", "{date:%Y}/../x", "C:/x"] {
        let r = s.execute(
            "library.import",
            &json!({"paths": [src.to_string_lossy()], "mode": "copy", "destination": dest.to_string_lossy(), "organize": bad}),
        );
        assert!(r.is_err(), "{bad} should be refused");
    }
    assert_eq!(s.catalog.len(), 0);
    let r = s
        .execute(
            "library.import",
            &json!({"paths": [src.to_string_lossy()], "mode": "copy", "destination": dest.to_string_lossy(), "organize": "{date:%Y}/{date:%Y%m%d}"}),
        )
        .unwrap();
    assert_eq!(ids(&r, "imported"), 1, "{r}");
    assert!(dest.join("2026").join("20260114").join("a.png").is_file());
    for d in [&src, &dest] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Auto import: files in the watched folder are added once their size held between two scans,
/// into the named album; non-photos are tried once; the selection stays put.
#[test]
fn auto_import_watched_folder() {
    let dir = temp_dir("watch");
    let mut s = Session::new().with_fs();
    assert!(s.execute("library.autoImport", &json!({"folder": dir.join("nope").to_string_lossy()})).is_err());
    s.execute("library.autoImport", &json!({"folder": dir.to_string_lossy(), "album": "Tethered"})).unwrap();
    assert_eq!(s.execute("library.autoImportScan", &json!({})).unwrap()["imported"], json!([]));
    write_png(&dir.join("one.png"), 1);
    std::fs::write(
        dir.join("one.xmp"),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:CameraProfile="Color" crs:Exposure2012="0.5"/></rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
    std::fs::write(dir.join("notes.txt"), "not a photo").unwrap();
    // first sight: wait (it may still be copying)
    assert_eq!(s.execute("library.autoImportScan", &json!({})).unwrap()["imported"], json!([]));
    let r = s.execute("library.autoImportScan", &json!({})).unwrap();
    assert_eq!(r["imported"].as_array().unwrap().len(), 1, "{r}");
    assert!(r["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "cameraProfile" && w["profile"] == "Color"), "{r}");
    let album = s.catalog.albums().find(|a| a.name == "Tethered").expect("album").id;
    assert_eq!(s.catalog.album_count(album), 1);
    assert!(s.selection.ids.is_empty(), "arrivals don't take the selection");
    // nothing new: nothing happens, and the text file isn't retried
    for _ in 0..2 {
        assert_eq!(s.execute("library.autoImportScan", &json!({})).unwrap()["imported"], json!([]));
    }
    write_png(&dir.join("two.png"), 2);
    s.execute("library.autoImportScan", &json!({})).unwrap();
    s.execute("library.autoImportScan", &json!({})).unwrap();
    assert_eq!(s.catalog.album_count(album), 2);
    s.execute("library.autoImport", &json!({"folder": null})).unwrap();
    assert_eq!(s.execute("library.autoImportScan", &json!({})).unwrap()["folder"], json!(null));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Smart previews: with the original offline the photo still renders (and edits apply) from
/// its proxy; without the proxy it can't be opened.
#[test]
fn smart_previews_stand_in_for_offline_originals() {
    let src = temp_dir("smartsrc");
    let lib = temp_dir("smartlib");
    write_png(&src.join("a.png"), 7);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!(r["built"], 1, "{r}");
    assert_eq!(s.execute("photo.smartPreview", &json!({})).unwrap(), json!({"smartPreview": true, "originalOnline": true}));
    let online = s.render_now(id, 48, 32).unwrap().image;
    // the drive goes away
    std::fs::rename(&src, src.with_extension("offline")).unwrap();
    s.media.forget(id);
    assert_eq!(s.execute("photo.smartPreview", &json!({})).unwrap()["originalOnline"], false);
    let offline = s.render_now(id, 48, 32).expect("renders from the smart preview").image;
    let diff: f64 = online.data.iter().zip(&offline.data).map(|(a, b)| (a[0] as f64 - b[0] as f64).abs()).sum::<f64>() / online.data.len() as f64;
    assert!(diff < 6.0, "the proxy looks like the original: {diff}");
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    s.media.forget(id);
    assert_ne!(s.render_now(id, 48, 32).unwrap().image.data, offline.data, "edits apply offline");
    // without the proxy it can't be opened
    s.execute("library.smartPreviews", &json!({"discard": true})).unwrap();
    s.media.forget(id);
    assert!(s.render_now(id, 48, 32).is_err());
    let _ = std::fs::remove_dir_all(src.with_extension("offline"));
    let _ = std::fs::remove_dir_all(&lib);
}

/// The long edge the grid asks thumbnails for in the tests of missing photos below.
const CELL: usize = 256;

/// Waits for `s`'s thumbnail cache to finish its disk writes (as quitting does).
fn drain_thumbnail_writes(s: &Session) {
    let t = std::time::Instant::now();
    while s.media.rendered.queued_bytes() > 0 && t.elapsed() < std::time::Duration::from_secs(30) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(s.media.rendered.queued_bytes(), 0, "the thumbnail cache's writes are done");
}

/// The mean difference per channel value of two pictures of one size.
fn mean_diff(a: &lcc_raster::Rgba8, b: &lcc_raster::Rgba8) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height), "one size");
    let sum: f64 = a.data.iter().zip(&b.data).map(|(p, q)| (0..3).map(|i| (f64::from(p[i]) - f64::from(q[i])).abs()).sum::<f64>()).sum();
    sum / (a.data.len() * 3).max(1) as f64
}

fn import_png(s: &mut Session, path: &Path) -> lcc_catalog::PhotoId {
    let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap())
}

/// A photo whose original is missing (a drive that isn't mounted) keeps its last grid thumbnail through an update
/// that changes the thumbnails' keys, as Lightroom keeps a missing photo's preview; once the original is back, its
/// thumbnail renders afresh. A missing photo with nothing cached still fails, and a file that is there but can't be
/// read shows its error, never an old thumbnail.
#[test]
fn a_missing_photo_keeps_its_thumbnail_through_an_update() {
    let src = temp_dir("keptsrc");
    let lib = temp_dir("keptlib");
    for (name, seed) in [("a.png", 7), ("b.png", 9), ("c.png", 11)] {
        write_png(&src.join(name), seed);
    }
    // seen online: a and c get their grid thumbnails, b is never shown
    let (a, b, c, before) = {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let (a, b, c) = (import_png(&mut s, &src.join("a.png")), import_png(&mut s, &src.join("b.png")), import_png(&mut s, &src.join("c.png")));
        let before = s.thumb_job(a, CELL).unwrap().run().rendered.expect("a renders online").image;
        s.thumb_job(c, CELL).unwrap().run().rendered.expect("c renders online");
        drain_thumbnail_writes(&s);
        (a, b, c, before)
    };
    // the drive goes away; c turns up elsewhere as a file that can't be read
    std::fs::rename(&src, src.with_extension("offline")).unwrap();
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("c.png"), b"not a picture").unwrap();
    // the next start comes after more updates than earlier versions' thumbnails are looked for under: only the
    // thumbnail the photo kept can show
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.media.thumb_version += crate::media::KEPT_VERSIONS + 1;
    let offline = s.thumb_job(a, CELL).unwrap().run();
    assert_eq!(offline.quick, Some(crate::media::QuickSource::Kept), "says it is the kept thumbnail");
    assert!(offline.placement.is_none(), "made with the look and crop the photo had then: it claims neither");
    let shown = offline.rendered.expect("the missing photo shows its kept thumbnail").image;
    let diff = mean_diff(&shown, &before);
    assert!(diff < 6.0, "the thumbnail it had before the update: {diff}");
    assert!(s.thumb_job(b, CELL).unwrap().run().rendered.is_err(), "a missing photo with nothing cached has no picture");
    assert!(s.thumb_job(c, CELL).unwrap().run().rendered.is_err(), "a file that is there but can't be read shows its error");
    // the drive is back, and a's pixels changed meanwhile: its thumbnail renders afresh from the file
    std::fs::remove_dir_all(&src).unwrap();
    std::fs::rename(src.with_extension("offline"), &src).unwrap();
    write_png(&src.join("a.png"), 200);
    let online = s.thumb_job(a, CELL).unwrap().run();
    assert!(online.loaded.is_some(), "decoded from the original, not taken from a cache");
    let diff = mean_diff(&online.rendered.expect("a renders online again").image, &before);
    assert!(diff > 30.0, "the file's new pixels, not the kept thumbnail: {diff}");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// A thumbnail cached by an earlier version of the app, before photos kept one whatever made it, still shows for a
/// missing photo: it is looked for under the renderer versions before this one.
#[test]
fn a_missing_photo_finds_a_thumbnail_an_earlier_version_cached() {
    let _keys = crate::sensor_maps::keep_generation();
    let src = temp_dir("oldsrc");
    let lib = temp_dir("oldlib");
    write_png(&src.join("a.png"), 7);
    let (a, before) = {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let a = import_png(&mut s, &src.join("a.png"));
        // an app three versions back, which kept no thumbnail of its own
        s.media.thumb_version -= 3;
        let before = s.thumb_job(a, CELL).unwrap().run().rendered.expect("renders online").image;
        drain_thumbnail_writes(&s);
        let p = s.catalog.photo(a).unwrap().clone();
        assert!(s.media.rendered.remove(crate::media::kept_thumb_key(&p)), "this version keeps one");
        (a, before)
    };
    std::fs::rename(&src, src.with_extension("offline")).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let shown = s.thumb_job(a, CELL).unwrap().run().rendered.expect("the earlier version's thumbnail").image;
    let diff = mean_diff(&shown, &before);
    assert!(diff < 6.0, "the thumbnail it had: {diff}");
    let _ = std::fs::remove_dir_all(src.with_extension("offline"));
    let _ = std::fs::remove_dir_all(&lib);
}

/// A thumbnail cached before photos kept one is kept the next time it is shown from the cache, so the photo keeps
/// it through any number of later updates.
#[test]
fn a_thumbnail_cached_before_photos_kept_one_is_kept_once_shown() {
    let _keys = crate::sensor_maps::keep_generation();
    let src = temp_dir("backsrc");
    let lib = temp_dir("backlib");
    write_png(&src.join("a.png"), 7);
    let (a, before) = {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let a = import_png(&mut s, &src.join("a.png"));
        let before = s.thumb_job(a, CELL).unwrap().run().rendered.expect("renders online").image;
        drain_thumbnail_writes(&s);
        let p = s.catalog.photo(a).unwrap().clone();
        s.media.rendered.remove(crate::media::kept_thumb_key(&p));
        (a, before)
    };
    // the next start shows it from the cache (the original still there)
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let r = s.thumb_job(a, CELL).unwrap().run();
        assert!(r.rendered.is_ok() && r.loaded.is_none(), "shown from the cache");
        drain_thumbnail_writes(&s);
    }
    std::fs::rename(&src, src.with_extension("offline")).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.media.thumb_version += crate::media::KEPT_VERSIONS + 1;
    let shown = s.thumb_job(a, CELL).unwrap().run().rendered.expect("the thumbnail kept when it was last shown").image;
    let diff = mean_diff(&shown, &before);
    assert!(diff < 6.0, "the thumbnail it had: {diff}");
    let _ = std::fs::remove_dir_all(src.with_extension("offline"));
    let _ = std::fs::remove_dir_all(&lib);
}

/// The kept thumbnail is the look the photo had when it was last shown: an undo back to a look whose thumbnail was
/// cached already replaces the kept thumbnail of the edit undone.
#[test]
fn a_missing_photo_keeps_the_look_an_undo_went_back_to() {
    let _keys = crate::sensor_maps::keep_generation();
    let src = temp_dir("undosrc");
    let lib = temp_dir("undolib");
    write_png(&src.join("a.png"), 7);
    // cached by a version that kept none
    let (a, plain) = {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let a = import_png(&mut s, &src.join("a.png"));
        let plain = s.thumb_job(a, CELL).unwrap().run().rendered.expect("renders").image;
        drain_thumbnail_writes(&s);
        let p = s.catalog.photo(a).unwrap().clone();
        assert!(s.media.rendered.remove(crate::media::kept_thumb_key(&p)));
        (a, plain)
    };
    // the next start: an edit is rendered (the first kept thumbnail), then undone back to the cached look
    let edited = {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let as_shot = (*s.catalog.photo(a).unwrap().develop).clone();
        let mut bright = as_shot.clone();
        bright.light.exposure = 2.0;
        s.set_develop(a, bright, "Exposure").unwrap();
        let edited = s.thumb_job(a, CELL).unwrap().run().rendered.expect("renders the edit").image;
        assert!(mean_diff(&plain, &edited) > 20.0, "the edit shows");
        s.set_develop(a, as_shot, "Undo").unwrap();
        let back = s.thumb_job(a, CELL).unwrap().run();
        assert!(back.rendered.is_ok() && back.loaded.is_none(), "shown from the cache");
        drain_thumbnail_writes(&s);
        edited
    };
    std::fs::rename(&src, src.with_extension("offline")).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.media.thumb_version += crate::media::KEPT_VERSIONS + 1;
    let shown = s.thumb_job(a, CELL).unwrap().run().rendered.expect("the kept thumbnail").image;
    assert!(mean_diff(&shown, &plain) < 6.0, "the look it went back to");
    assert!(mean_diff(&shown, &edited) > 20.0, "not the edit undone");
    let _ = std::fs::remove_dir_all(src.with_extension("offline"));
    let _ = std::fs::remove_dir_all(&lib);
}

/// The loupe's stand-in shows the photo's cached view: that is no grid thumbnail, and is never kept as one (a
/// view-sized or full-size copy for every photo seen in the loupe).
#[test]
fn the_loupes_stand_in_keeps_no_thumbnail() {
    let _keys = crate::sensor_maps::keep_generation();
    let src = temp_dir("loupesrc");
    let lib = temp_dir("loupelib");
    write_png(&src.join("a.png"), 7);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let a = import_png(&mut s, &src.join("a.png"));
    s.thumb_job(a, CELL).unwrap().run().rendered.expect("the grid thumbnail renders");
    s.loupe_job(a, 1600, 1600, true).unwrap().run().rendered.expect("the view renders");
    let r = s.quick_view_job(a, 1600, true).unwrap().run();
    assert_eq!(r.quick, Some(crate::media::QuickSource::Cached), "the cached view stands in");
    drain_thumbnail_writes(&s);
    let p = s.catalog.photo(a).unwrap().clone();
    let kept = s.media.rendered.get_latest(crate::media::kept_thumb_key(&p)).expect("the grid thumbnail is kept");
    assert!(kept.width.max(kept.height) <= CELL, "the kept thumbnail is the grid's, not the view: {}x{}", kept.width, kept.height);
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// The grid's stand-in for an edited photo shows its cached thumbnail before the render job does: what it shows is
/// kept, as when the render job shows it from the cache.
#[test]
fn the_grids_stand_in_keeps_the_edited_thumbnail_it_shows() {
    let _keys = crate::sensor_maps::keep_generation();
    let src = temp_dir("standinsrc");
    let lib = temp_dir("standinlib");
    write_png(&src.join("a.png"), 7);
    let bright = |s: &Session, a| {
        let mut bright = (*s.catalog.photo(a).unwrap().develop).clone();
        bright.light.exposure = 2.0;
        bright
    };
    // cached by a version that kept none
    let a = {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let a = import_png(&mut s, &src.join("a.png"));
        s.set_develop(a, bright(&s, a), "Exposure").unwrap();
        s.thumb_job(a, CELL).unwrap().run().rendered.expect("the edit renders");
        drain_thumbnail_writes(&s);
        let p = s.catalog.photo(a).unwrap().clone();
        assert!(s.media.rendered.remove(crate::media::kept_thumb_key(&p)), "kept when rendered");
        a
    };
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    // (the same edit again: one look, one cached thumbnail)
    s.set_develop(a, bright(&s, a), "Exposure").unwrap();
    let p = s.catalog.photo(a).unwrap().clone();
    let job = s.thumb_job(a, CELL).unwrap();
    let r = s.quick_thumb_job(&job).expect("the grid's stand-in").run();
    assert_eq!(r.quick, Some(crate::media::QuickSource::Cached), "the cached thumbnail stands in");
    drain_thumbnail_writes(&s);
    assert!(s.media.rendered.get_latest(crate::media::kept_thumb_key(&p)).is_some(), "what the stand-in showed is kept");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// Issue #1534: the decoder facts a render interprets its settings against (a camera colour model,
/// the file's local tone map, its semantic mattes) must be stored with the smart preview, so an
/// edit made while the original is offline renders the same picture as the online source instead
/// of the header-facts fallback.
#[test]
fn smart_previews_keep_the_decoder_facts_offline_edits_need() {
    use lcc_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape, WbMode};
    use lcc_pipeline::SourceInfo;
    use lcc_pipeline::masks::{MatteKind, Mattes};
    use lcc_raster::Rgb32f;

    // A smooth scene: the proxy's 8-bit sRGB round trip stays within a code step, so a mismatch
    // left between the offline and the online render is metadata the proxy failed to keep.
    let pixels = Rgb32f::from_fn(96, 64, |x, y| {
        let t = ((x * 7 + y * 3) % 96) as f32 / 96.0;
        [0.02 + t * 1.2, 0.02 + t * 0.6, 0.02 + t * 0.25]
    });
    let camera_color = lcc_pipeline::CameraColor {
        tags: lcc_raw::ColorData {
            illuminant: [21, 17],
            color_matrix: [Some(lcc_color::Mat3([[0.9, 0.2, -0.15], [-0.3, 1.25, 0.08], [0.02, -0.12, 0.85]])), None],
            as_shot_neutral: Some([0.5, 1.0, 0.7]),
            baseline_exposure: 0.3,
            ..Default::default()
        },
        developed_for: lcc_color::Xy { x: 0.32, y: 0.35 },
    };
    let mut mattes = Mattes::default();
    mattes.push(
        MatteKind::Sky,
        lcc_raster::Image { width: 96, height: 64, data: (0..96 * 64usize).map(|i| if (i % 96) / 24 == 1 { 255 } else { 0 }).collect() },
    );
    let local_tone = lcc_pipeline::local_tone::LocalTone {
        map: lcc_raw::gaintable::GainTableMap {
            points_v: 1,
            points_h: 2,
            points_n: 1,
            spacing_v: 1.0,
            spacing_h: 1.0,
            origin_v: 0.0,
            origin_h: 0.0,
            weights: [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 0.0, 0.0],
            gamma: 1.0,
            gains: vec![1.0, 4.0],
        },
        placement: lcc_raw::gaintable::SourcePlacement::new([0.0, 0.0, 1.0, 1.0], lcc_geom::Orientation::Normal),
    };
    // the facts differ per case; the rest is the raw source's usual shape
    let info = |camera_color: Option<lcc_pipeline::CameraColor>, local_tone: Option<lcc_pipeline::local_tone::LocalTone>, mattes: Option<Mattes>| {
        SourceInfo {
            raw: true,
            as_shot_temp: 5200.0,
            as_shot_tint: 8.0,
            camera_color: camera_color.map(std::sync::Arc::new),
            local_tone: local_tone.map(std::sync::Arc::new),
            mattes: mattes.map(std::sync::Arc::new),
            ..Default::default()
        }
    };

    /// Build the proxy of `pixels`+`info`, apply `edit`, then render the proxy's own pixels and
    /// stored facts online, and the same proxy offline (the original renamed away): how many
    /// pixels differ. Identical stored pixels and identical edits must render identically; what
    /// the proxy failed to keep shows up as a difference.
    fn offline_diff(tag: &str, pixels: &Rgb32f, info: &SourceInfo, edit: DevelopSettings) -> usize {
        let src = temp_dir(&format!("smartmeta-src-{tag}"));
        let lib = temp_dir(&format!("smartmeta-lib-{tag}"));
        write_png(&src.join("a.png"), 3);
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let (p, i) = (pixels.clone(), info.clone());
        s.media.file_loader = Some(std::sync::Arc::new(move |_path, _max_edge, _options| Ok((p.clone(), i.clone()))));
        s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
        let id = s.active().unwrap();
        let r = s.execute("library.smartPreviews", &json!({})).unwrap();
        assert_eq!(r["built"], 1, "{r}");
        s.set_develop(id, edit, "reproduction").unwrap();
        let dir = s.media.smart_dir.clone().unwrap();
        let stored = crate::smart::load(&crate::smart::sharded_file_path(&dir, s.catalog.photo(id).unwrap())).unwrap();
        let mut online = s.render_job(id, 96, 96, false, true).unwrap();
        // the proxy's own (post-JPEG) pixels, with the facts the original source had: the offline
        // render must reach the same picture through the facts the proxy stored
        online.source = crate::media::SourceRef::Loaded(Box::new(crate::media::DecodedSource::new(stored.image, Some(info.clone()))));
        let online = online.run().rendered.unwrap().image;
        // the drive goes away
        std::fs::rename(&src, src.with_extension("offline")).unwrap();
        s.media.rendered.clear();
        s.media.clear_sources();
        s.media.forget(id);
        let offline = s.render_now(id, 96, 96).expect("renders from the smart preview").image;
        assert_eq!(online.data.len(), offline.data.len(), "{tag}: same grid");
        let differing = online.data.iter().zip(&offline.data).filter(|(a, b)| a != b).count();
        let _ = std::fs::remove_dir_all(src.with_extension("offline"));
        let _ = std::fs::remove_dir_all(&lib);
        differing
    }

    // a Kelvin white balance is evaluated through the camera's colour model
    let mut wb = DevelopSettings::default();
    wb.wb.mode = WbMode::Custom;
    wb.wb.temp = 3000.0;
    wb.wb.tint = 20.0;
    let differing = offline_diff("wb", &pixels, &info(Some(camera_color), None, None), wb);
    assert_eq!(differing, 0, "the camera colour model is kept offline");

    // an AI mask uses the mattes that came with the file
    let mut sky = DevelopSettings::default();
    sky.masks.push(Mask {
        components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Sky }],
        adjust: LocalAdjustments { exposure: 2.0, ..Default::default() },
        ..Default::default()
    });
    let differing = offline_diff("sky", &pixels, &info(None, None, Some(mattes)), sky);
    assert_eq!(differing, 0, "the file's mattes are kept offline");

    // Camera local tone mapping reads the file's gain table map
    let mut tone = DevelopSettings::default();
    tone.profile.camera_local_tone = true;
    tone.light.exposure = -3.0;
    let differing = offline_diff("tone", &pixels, &info(None, Some(local_tone), None), tone);
    assert_eq!(differing, 0, "the local tone map is kept offline");
}

/// Issue #106: a smart preview cut short (crash, full drive) counted as built forever and failed
/// exactly when the original went offline. Writes are atomic now, and a damaged proxy is not
/// reported as present and is rebuilt.
#[test]
fn damaged_smart_previews_are_rebuilt_and_failed_writes_leave_none() {
    let src = temp_dir("smartdmg-src");
    let lib = temp_dir("smartdmg-lib");
    write_png(&src.join("a.png"), 5);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let dir = s.media.smart_dir.clone().unwrap();
    let file = crate::smart::sharded_file_path(&dir, s.catalog.photo(id).unwrap());
    {
        let _fault = lcc_catalog::safe_file::fail_writes_after(64);
        let r = s.execute("library.smartPreviews", &json!({})).unwrap();
        assert_eq!((r["built"].as_u64(), r["failed"].as_array().map(Vec::len)), (Some(0), Some(1)), "{r}");
    }
    assert!(!file.exists(), "no partial proxy");
    assert_eq!(std::fs::read_dir(file.parent().unwrap()).unwrap().count(), 0, "no temp file left in shard");
    assert_eq!(s.execute("photo.smartPreview", &json!({})).unwrap()["smartPreview"], false);

    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!((r["built"].as_u64(), r["repaired"].as_u64()), (Some(1), Some(0)), "{r}");
    assert!(crate::smart::is_valid(&file));
    // cut short, as by a crash before the fix
    let full = std::fs::read(&file).unwrap();
    std::fs::write(&file, &full[..full.len() / 2]).unwrap();
    assert!(!crate::smart::is_valid(&file));
    assert_eq!(s.execute("photo.smartPreview", &json!({})).unwrap()["smartPreview"], false, "not counted as there");
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!((r["built"].as_u64(), r["repaired"].as_u64()), (Some(1), Some(1)), "{r}");
    assert_eq!(std::fs::read(&file).unwrap(), full, "rebuilt whole");
    // the app's background build (issue #104) checks and repairs the same way, on a worker thread
    std::fs::write(&file, &full[..full.len() / 2]).unwrap();
    let r = s.execute("library.smartPreviews", &json!({"background": true})).unwrap();
    assert_eq!(r["what"], "smart previews", "{r}");
    let t0 = std::time::Instant::now();
    let r = loop {
        let r = s.execute("library.previewProgress", &json!({})).unwrap();
        if r["running"] == false || t0.elapsed() > std::time::Duration::from_secs(60) {
            break r;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!((r["done"].as_u64(), r["failed"].as_u64(), r["repaired"].as_u64()), (Some(1), Some(0), Some(1)), "{r}");
    assert_eq!(std::fs::read(&file).unwrap(), full, "rebuilt whole in the background");
    // its row in the activity stack (issue #345) goes with it
    let t0 = std::time::Instant::now();
    while !s.activity.list().is_empty() && t0.elapsed() < std::time::Duration::from_secs(10) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(s.activity.list().is_empty(), "{:?}", s.activity.list());
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// LUT profiles: a .cube imports as a creative profile (into the library), renders differently
/// (Amount 0 = no change), is listed in the profile browser and comes back with the library.
#[test]
fn cube_luts_become_profiles() {
    let src = temp_dir("cubesrc");
    let lib = temp_dir("cubelib");
    // a warm look: red up, blue down
    let n = 9;
    let mut cube = String::from("TITLE \"Warm Test\"\nLUT_3D_SIZE 9\n");
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let f = |v: usize| v as f32 / (n - 1) as f32;
                cube.push_str(&format!("{} {} {}\n", (f(r) * 1.15).min(1.0), f(g), f(b) * 0.8));
            }
        }
    }
    std::fs::create_dir_all(src.join("Film Looks")).unwrap();
    std::fs::write(src.join("Film Looks/warm.cube"), &cube).unwrap();
    std::fs::write(src.join("Film Looks/broken.cube"), "LUT_3D_SIZE 4\n0 0 0\n").unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, true).unwrap();
    let r = s.execute("profile.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!((r["imported"].as_array().unwrap().len(), r["failed"].as_array().unwrap().len()), (1, 1), "{r}");
    let id = r["imported"][0]["id"].as_str().unwrap().to_string();
    assert_eq!((r["imported"][0]["name"].as_str(), r["imported"][0]["group"].as_str()), (Some("Warm Test"), Some("Film Looks")));
    let menu = s.profile_menu();
    assert!(menu["groups"].as_array().unwrap().iter().any(|g| g["name"] == "Film Looks"), "{menu}");
    let photo = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [photo.0]})).unwrap();
    let base = s.render_now(photo, 48, 32).unwrap().image;
    s.execute("develop.profile", &json!({"id": id, "amount": 100})).unwrap();
    let warm = s.render_now(photo, 48, 32).unwrap().image;
    let mean = |img: &lcc_raster::Rgba8, k: usize| img.data.iter().map(|p| p[k] as f64).sum::<f64>() / img.data.len() as f64;
    assert!(
        mean(&warm, 0) > mean(&base, 0) && mean(&warm, 2) < mean(&base, 2),
        "warmer: R {} → {}, B {} → {}",
        mean(&base, 0),
        mean(&warm, 0),
        mean(&base, 2),
        mean(&warm, 2)
    );
    s.execute("develop.profile", &json!({"id": id, "amount": 0})).unwrap();
    assert_eq!(s.render_now(photo, 48, 32).unwrap().image.data, base.data, "Amount 0 = the photo as it was");
    assert_eq!(s.profile_info(&id), Some(("Warm Test", "Film Looks")));
    s.execute("profile.favorite", &json!({"id": id})).unwrap();
    assert!(s.profile_favorites.contains(&id));
    // `profiles.list` lists it next to the built-in profiles (#618)
    let listed = |s: &mut Session| {
        let list = s.execute("profiles.list", &json!({})).unwrap();
        let list = list.as_array().unwrap();
        assert!(list.iter().any(|p| p["id"] == "lc.color"), "built-in profiles stay listed");
        list.iter().find(|p| p["id"] == id.as_str()).cloned()
    };
    assert_eq!(listed(&mut s), Some(json!({"id": id, "name": "Warm Test", "group": "Film Looks", "favorite": true, "imported": true})));
    // the library keeps it
    drop(s);
    lcc_pipeline::lut::unregister(&id);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert!(s.lut_profiles.iter().any(|p| p.id == id));
    assert!(s.profile_favorites.contains(&id), "imported LUT favorite survives reopen");
    assert_eq!(s.profile_info(&id), Some(("Warm Test", "Film Looks")));
    assert!(lcc_pipeline::lut::get(&id).is_some(), "registered again on open");
    assert!(listed(&mut s).is_some_and(|p| p["favorite"] == true), "listed after reopen");
    s.execute("profile.deleteImported", &json!({"id": id})).unwrap();
    assert!(lcc_pipeline::lut::get(&id).is_none());
    assert_eq!(listed(&mut s), None, "a deleted profile is no longer listed");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// Without a library on disk an imported LUT lives only in this session: no copy of it is
/// left in the temp folder (#1952).
#[test]
fn cube_import_without_a_library_leaves_no_temp_copy() {
    let src = temp_dir("cubemem");
    let mut cube = String::from("TITLE \"Temp Copy Check\"\nLUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                cube.push_str(&format!("{r} {g} {b}\n"));
            }
        }
    }
    std::fs::write(src.join("mem.cube"), &cube).unwrap();
    let leak = std::env::temp_dir().join("app-imported-temp-copy-check.cube");
    let _ = std::fs::remove_file(&leak);
    let mut s = Session::new().with_fs();
    let r = s.execute("profile.import", &json!({"paths": [src.join("mem.cube").to_string_lossy()]})).unwrap();
    let id = r["imported"][0]["id"].as_str().unwrap().to_string();
    assert!(lcc_pipeline::lut::get(&id).is_some(), "usable in this session");
    let file = s.lut_profiles.iter().find(|p| p.id == id).unwrap().file.clone();
    assert!(file.is_empty(), "no file kept without a library: {file}");
    drop(s);
    assert!(!leak.exists(), "{} left behind", leak.display());
    lcc_pipeline::lut::unregister(&id);
    let _ = std::fs::remove_dir_all(&src);
}

/// `prefs.json` records each imported profile's file, but only the library's own copy in
/// `Profiles/` is read or deleted: a path out through `..`, a path elsewhere, a file past the size
/// cap or a link is never followed (issue #2123). Delete Imported Profile used to remove whatever
/// `<lib>/Profiles/../..` named, and opening the library read any file it named, unbounded.
#[test]
fn lut_profile_paths_in_prefs_stay_inside_profiles() {
    let src = temp_dir("cubeprefs-src");
    let lib = temp_dir("cubeprefs-lib");
    let outside = temp_dir("cubeprefs-outside");
    let mut cube = String::from("LUT_3D_SIZE 2\n");
    for i in 0..8 {
        cube.push_str(&format!("{} {} {}\n", i & 1, (i >> 1) & 1, (i >> 2) & 1));
    }
    for name in ["a", "b", "c"] {
        std::fs::write(src.join(format!("{name}.cube")), &cube).unwrap();
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, true).unwrap();
    let r = s.execute("profile.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let ids: Vec<String> = r["imported"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids.len(), 3, "{r}");
    drop(s);
    for id in &ids {
        lcc_pipeline::lut::unregister(id);
    }

    // what a crafted prefs.json names: files outside the library, one of them through `..`
    let victim = outside.join("victim.cube");
    std::fs::write(&victim, &cube).unwrap();
    let prefs = lib.join("prefs.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&prefs).unwrap()).unwrap();
    let traversal = lib.join("Profiles").join("..").join("..").join(outside.file_name().unwrap()).join("victim.cube");
    let files = [traversal.to_string_lossy().to_string(), victim.to_string_lossy().to_string()];
    let mut patched = 0;
    for list in v.as_object_mut().unwrap().values_mut().filter_map(|x| x.as_array_mut()) {
        for p in list.iter_mut().filter(|p| p.get("file").is_some()) {
            if let Some(i) = ids.iter().position(|id| p["id"] == id.as_str()).filter(|&i| i < 2) {
                p["file"] = json!(files[i]);
                patched += 1;
            }
        }
    }
    assert_eq!(patched, 2, "{v}");
    std::fs::write(&prefs, serde_json::to_vec(&v).unwrap()).unwrap();
    // the third profile's own copy grows past the cap (sparse: no 64 MB written)
    let third = lib.join("Profiles").join(format!("{}.cube", ids[2].trim_start_matches("lut:")));
    std::fs::OpenOptions::new().write(true).open(&third).unwrap().set_len((64 << 20) + 1).unwrap();

    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.lut_profiles.len(), 3, "the list itself is kept");
    for id in &ids {
        assert!(lcc_pipeline::lut::get(id).is_none(), "{id}: nothing outside Profiles/ or past the cap is read");
    }
    for id in &ids[..2] {
        s.execute("profile.deleteImported", &json!({"id": id})).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), cube, "the file outside the library is left alone");
    // the library's own copy is still deleted
    s.execute("profile.deleteImported", &json!({"id": ids[2]})).unwrap();
    assert!(!third.exists());
    for d in [&src, &lib, &outside] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Folder rename / move on disk: files and sidecars go along, the photos are relinked and still render; clashes
/// and moving into itself are refused. Rename / Move Folder are undo steps (issue #97): undo renames the folder back on disk (files and
/// sidecars along) and restores the photos' paths, redo repeats it; a destination taken in the
/// meantime is never overwritten — the step is refused, reported and stays on the stack.
#[test]
fn folder_rename_and_move_undo_and_redo_on_disk() {
    let root = temp_dir("folders-undo");
    let a = root.join("Trip");
    write_png(&a.join("one.png"), 3);
    std::fs::write(a.join("one.xmp"), "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>").unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [a.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let path = |s: &Session| match &s.catalog.photo(id).unwrap().source {
        lcc_catalog::Source::File { path } => path.clone(),
        _ => panic!(),
    };
    // clashes and names that are paths are refused
    std::fs::create_dir_all(root.join("Taken")).unwrap();
    assert!(s.execute("folder.rename", &json!({"path": a.to_string_lossy(), "name": "Taken"})).is_err(), "name taken");
    assert!(s.execute("folder.rename", &json!({"path": a.to_string_lossy(), "name": "a/b"})).is_err());
    let undo0 = s.undo.len();
    let b = root.join("Italy");
    let r = s.execute("folder.rename", &json!({"path": a.to_string_lossy(), "name": "Italy"})).unwrap();
    assert_eq!(r["relinked"], 1, "{r}");
    assert_eq!(s.undo.len(), undo0 + 1, "one undo step");
    assert!(s.undo.last().unwrap().label.contains("Rename Folder"));
    assert!(b.join("one.png").exists() && !a.exists());

    s.execute("edit.undo", &json!({})).unwrap();
    assert!(a.join("one.png").exists() && a.join("one.xmp").exists() && !b.exists(), "renamed back on disk");
    assert_eq!(path(&s), a.join("one.png").to_string_lossy());
    s.media.forget(id);
    assert!(s.render_now(id, 16, 16).is_ok());

    s.execute("edit.redo", &json!({})).unwrap();
    assert!(b.join("one.png").exists() && b.join("one.xmp").exists() && !a.exists());
    assert_eq!(path(&s), b.join("one.png").to_string_lossy());

    // the old name is taken now: undo is refused, nothing is overwritten, the step stays
    std::fs::create_dir_all(&a).unwrap();
    std::fs::write(a.join("keep.txt"), "mine").unwrap();
    let undo_n = s.undo.len();
    let e = s.execute("edit.undo", &json!({})).unwrap_err().to_string();
    assert!(e.contains("already exists"), "{e}");
    assert_eq!(s.undo.len(), undo_n, "the step stays on the stack");
    assert_eq!(std::fs::read_to_string(a.join("keep.txt")).unwrap(), "mine");
    assert!(b.join("one.png").exists());
    assert_eq!(path(&s), b.join("one.png").to_string_lossy());
    std::fs::remove_dir_all(&a).unwrap();

    // move into another folder, undo, redo
    assert!(s.execute("folder.move", &json!({"path": b.to_string_lossy(), "into": b.join("deeper").to_string_lossy()})).is_err(), "not into itself");
    let into = root.join("Archive");
    let r = s.execute("folder.move", &json!({"path": b.to_string_lossy(), "into": into.to_string_lossy()})).unwrap();
    assert_eq!(r["relinked"], 1, "{r}");
    let moved = into.join("Italy");
    assert_eq!(path(&s), moved.join("one.png").to_string_lossy());
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(b.join("one.png").exists() && !moved.exists());
    assert_eq!(path(&s), b.join("one.png").to_string_lossy());
    // the redo target is taken (even by an empty folder): refused, nothing replaced
    std::fs::create_dir_all(&moved).unwrap();
    assert!(s.execute("edit.redo", &json!({})).is_err());
    assert!(b.join("one.png").exists());
    std::fs::remove_dir(&moved).unwrap();
    s.execute("edit.redo", &json!({})).unwrap();
    assert!(moved.join("one.png").exists() && moved.join("one.xmp").exists() && !b.exists());
    assert_eq!(path(&s), moved.join("one.png").to_string_lossy());
    let _ = std::fs::remove_dir_all(&root);
}

/// Assisted culling: a burst (same scene, seconds apart) of a sharp and two soft shots, plus an
/// unrelated photo: the burst is grouped, the sharp one is its best, blurry shots can be rejected.
#[test]
fn assisted_culling_groups_bursts_and_scores_focus() {
    let dir = temp_dir("cull");
    let scene = lcc_scenes::demo_library()[0].render(480, 320);
    let soften = |img: &lcc_raster::Rgb32f, r: usize| {
        let (w, h) = (img.width, img.height);
        let mut out = img.clone();
        for y in 0..h {
            for x in 0..w {
                let (mut acc, mut n) = ([0f32; 3], 0.0);
                for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                    for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                        let c = img.data[yy * w + xx];
                        acc = [acc[0] + c[0], acc[1] + c[1], acc[2] + c[2]];
                        n += 1.0;
                    }
                }
                out.data[y * w + x] = acc.map(|v| v / n);
            }
        }
        out
    };
    let save = |img: &lcc_raster::Rgb32f, name: &str| {
        let data: Vec<[u8; 4]> = img
            .data
            .iter()
            .map(|c| {
                let e = |v: f32| (lcc_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8;
                [e(c[0]), e(c[1]), e(c[2]), 255]
            })
            .collect();
        let png = lcc_codecs::encode_png(
            &lcc_codecs::EncodeImage::rgba8(&lcc_raster::Rgba8 { width: img.width, height: img.height, data }),
            &Default::default(),
        )
        .unwrap();
        std::fs::write(dir.join(name), png).unwrap();
    };
    save(&soften(&scene, 3), "a_soft.png");
    save(&scene, "b_sharp.png");
    save(&soften(&scene, 6), "c_softer.png");
    save(&lcc_scenes::demo_library()[7].render(480, 320), "d_other.png");
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
    let by_name = |s: &Session, n: &str| s.catalog.photos().find(|p| p.file_name == n).unwrap().id;
    for (i, n) in ["a_soft.png", "b_sharp.png", "c_softer.png"].iter().enumerate() {
        let id = by_name(&s, n);
        s.commit("t", lcc_catalog::Op::SetCaptured { id, captured: Some(format!("2026-05-01T10:00:0{i}")) }).unwrap();
    }
    let other = by_name(&s, "d_other.png");
    s.commit("t", lcc_catalog::Op::SetCaptured { id: other, captured: Some("2026-05-01T10:00:04".into()) }).unwrap();
    let r = s.execute("photo.analyze", &json!({"ids": ids, "rejectBelow": 0.0, "pickBest": true})).unwrap();
    assert_eq!(r["groups"], 1, "{r}");
    let a = |s: &Session, n: &str| s.catalog.photo(by_name(s, n)).unwrap().analysis.unwrap();
    let (soft, sharp, softer, odd) = (a(&s, "a_soft.png"), a(&s, "b_sharp.png"), a(&s, "c_softer.png"), a(&s, "d_other.png"));
    eprintln!("sharpness: sharp {} soft {} softer {} other {}", sharp.sharpness, soft.sharpness, softer.sharpness, odd.sharpness);
    assert!(sharp.sharpness > soft.sharpness && soft.sharpness > softer.sharpness);
    assert!(sharp.best && !soft.best && sharp.group.is_some() && sharp.group == softer.group);
    assert!(odd.group.is_none(), "an unrelated photo isn't in the burst");
    assert_eq!(s.catalog.photo(by_name(&s, "b_sharp.png")).unwrap().flag, lcc_catalog::Flag::Pick);
    // reject the blurry ones (by score), as a rule too
    let cut = (soft.sharpness + sharp.sharpness) / 2.0;
    s.execute("photo.analyze", &json!({"ids": ids, "rejectBelow": cut})).unwrap();
    assert_eq!(s.catalog.photo(by_name(&s, "c_softer.png")).unwrap().flag, lcc_catalog::Flag::Reject);
    s.execute("library.filter", &json!({"ruleSet": {"rules": [{"field": "bestOfGroup", "op": "is", "value": true}]}})).unwrap();
    let vis = s.visible_cloned();
    assert!(vis.contains(&by_name(&s, "b_sharp.png")) && !vis.contains(&by_name(&s, "a_soft.png")) && vis.contains(&other));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Classical aesthetic and cullScore persist without optional models; eyes-open is skipped when
/// no detector and no face regions (not stored as 0). A photo listed twice is analysed once.
#[test]
fn assisted_culling_scores_a_photo_once_and_persists_aesthetic_and_cull_score() {
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0];
    // a photo listed twice is analysed once and is not a burst with itself
    let r = s.execute("photo.analyze", &json!({"ids": [id.0, id.0]})).unwrap();
    assert_eq!(r["groups"], 0, "a photo listed twice is not a burst of two: {r}");
    assert_eq!(r["photos"].as_array().map(Vec::len), Some(1), "{r}");
    let row = &r["photos"][0];
    assert!(row["aesthetic"].as_f64().is_some(), "{r}");
    assert!(row["cullScore"].as_f64().is_some(), "{r}");
    let an = s.catalog.photo(id).unwrap().analysis.unwrap();
    assert!(an.group.is_none() && !an.best, "{an:?}");
    let aesthetic = an.aesthetic.expect("aesthetic");
    assert!((1.0..=10.0).contains(&aesthetic), "{an:?}");
    let score = an.cull_score.expect("cull_score");
    assert!((0.0..=1.0).contains(&score), "{an:?}");
    assert!(row["eyesOpen"].is_null() || row["eyesOpen"].is_number(), "{r}");
}

/// Assisted culling: two look-alike shots one second apart across midnight form a burst at the end of
/// every month, including those with fewer than 31 days; the burst limits are parameters.
#[test]
fn assisted_culling_groups_bursts_across_month_end_and_within_adjustable_limits() {
    let dir = temp_dir("cull-month-end");
    for (name, (w, h)) in [("a.png", (480, 320)), ("b.png", (482, 321))] {
        let img = lcc_scenes::demo_library()[0].render(w, h);
        let data: Vec<[u8; 4]> = img
            .data
            .iter()
            .map(|c| {
                let e = |v: f32| (lcc_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8;
                [e(c[0]), e(c[1]), e(c[2]), 255]
            })
            .collect();
        let png = lcc_codecs::encode_png(
            &lcc_codecs::EncodeImage::rgba8(&lcc_raster::Rgba8 { width: img.width, height: img.height, data }),
            &Default::default(),
        )
        .unwrap();
        std::fs::write(dir.join(name), png).unwrap();
    }
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
    let by_name = |s: &Session, n: &str| s.catalog.photos().find(|p| p.file_name == n).unwrap().id;
    let (a, b) = (by_name(&s, "a.png"), by_name(&s, "b.png"));
    for (before, after) in [
        ("2026-01-31T23:59:59", "2026-02-01T00:00:00"),
        ("2026-02-28T23:59:59", "2026-03-01T00:00:00"),
        ("2026-04-30T23:59:59", "2026-05-01T00:00:00"),
        ("2026-06-30T23:59:59", "2026-07-01T00:00:00"),
        ("2026-09-30T23:59:59", "2026-10-01T00:00:00"),
        ("2026-11-30T23:59:59", "2026-12-01T00:00:00"),
        ("2026-12-31T23:59:59", "2027-01-01T00:00:00"),
    ] {
        s.commit("t", lcc_catalog::Op::SetCaptured { id: a, captured: Some(before.into()) }).unwrap();
        s.commit("t", lcc_catalog::Op::SetCaptured { id: b, captured: Some(after.into()) }).unwrap();
        let r = s.execute("photo.analyze", &json!({"ids": ids})).unwrap();
        assert_eq!(r["groups"], 1, "shots 1 s apart ({before} / {after}) must form a burst: {r}");
    }
    // `burstGap` and `similarity` move the burst limits (#2275), and values out of range are refused before
    // anything is measured or stored
    s.commit("t", lcc_catalog::Op::SetCaptured { id: a, captured: Some("2026-05-01T10:00:00".into()) }).unwrap();
    s.commit("t", lcc_catalog::Op::SetCaptured { id: b, captured: Some("2026-05-01T10:00:15".into()) }).unwrap();
    // 15 s apart: past the default 10 s, inside a 20 s gap
    assert_eq!(s.execute("photo.analyze", &json!({"ids": ids})).unwrap()["groups"], 0);
    assert_eq!(s.execute("photo.analyze", &json!({"ids": ids, "burstGap": 20})).unwrap()["groups"], 1);
    // the look-alike pair is a burst at the default similarity, not when it must be identical
    let strict = s.execute("photo.analyze", &json!({"ids": ids, "burstGap": 20, "similarity": 1.0})).unwrap();
    assert_eq!(strict["groups"], 0, "{strict}");
    let undo_before = s.commits();
    for bad in
        [json!({"burstGap": -1}), json!({"burstGap": 86401}), json!({"burstGap": "10"}), json!({"similarity": 1.5}), json!({"similarity": -0.1})]
    {
        let mut p = bad.clone();
        p["ids"] = json!(ids);
        assert!(s.execute("photo.analyze", &p).is_err(), "{bad} must be refused");
    }
    assert_eq!(s.commits(), undo_before, "bad values change nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Find Dust Spots: soft dark spots on a smooth sky are found and healed (one undo step).
#[test]
fn dust_spots_are_found_and_healed() {
    let dir = temp_dir("dust");
    let (w, h) = (900usize, 600usize);
    let spots = [(180.0f32, 140.0f32), (600.0, 300.0)];
    let data: Vec<[u8; 4]> = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let mut v = 0.5 + 0.3 * (y / h as f32);
            for (cx, cy) in spots {
                v -= 0.07 * (-((x - cx).powi(2) + (y - cy).powi(2)) / (2.0 * 25.0)).exp();
            }
            let b = (v * 255.0) as u8;
            [b, b, (b as f32 * 1.15).min(255.0) as u8, 255]
        })
        .collect();
    let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&lcc_raster::Rgba8 { width: w, height: h, data }), &Default::default()).unwrap();
    std::fs::write(dir.join("sky.png"), png).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.join("sky.png").to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let before = s.render_now(id, 900, 600).unwrap().image;
    let r = s.execute("spot.findDust", &json!({})).unwrap();
    assert_eq!(r["added"], 2, "{r}");
    let after = s.render_now(id, 900, 600).unwrap().image;
    let at = |img: &lcc_raster::Rgba8, x: usize, y: usize| img.data[y * 900 + x][0] as i32;
    for (cx, cy) in spots {
        assert!(at(&after, cx as usize, cy as usize) > at(&before, cx as usize, cy as usize) + 6, "the spot at {cx},{cy} is lifted");
    }
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.develop_of(id).unwrap().spots.is_empty(), "one undo step");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Smart previews can live outside the library: the folder is saved per library, used for
/// building, status and offline rendering, changing it needs an explicit choice for the
/// previews already built, and an unavailable folder is an error (never a silent fallback).
#[test]
fn smart_previews_folder_is_chosen_per_library() {
    let src = temp_dir("smartloc-src");
    let lib = temp_dir("smartloc-lib");
    let away = temp_dir("smartloc-away");
    write_png(&src.join("a.png"), 7);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let loc = s.execute("library.smartPreviewsLocation", &json!({})).unwrap();
    assert_eq!(loc["custom"], false);
    assert_eq!(loc["path"], lib.join("Smart Previews").to_string_lossy().as_ref());
    s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!(crate::smart::stats(&lib.join("Smart Previews")).0, 1);
    // A flat proxy from older builds remains discoverable and migrates to the new sharded layout.
    let dir = lib.join("Smart Previews");
    let photo = s.catalog.photo(id).unwrap();
    let shard = crate::smart::sharded_file_path(&dir, photo);
    let flat = dir.join(crate::smart::file_name(photo));
    std::fs::rename(&shard, &flat).unwrap();
    assert_eq!(s.execute("photo.smartPreview", &json!({})).unwrap()["smartPreview"], true);

    // relative paths and missing drives are refused, and nothing is created on this drive
    assert!(s.execute("library.smartPreviewsLocation", &json!({"path": "rel/dir"})).is_err());
    let gone = away.join("unplugged/previews");
    let err = s.execute("library.smartPreviewsLocation", &json!({"path": gone.to_string_lossy()})).unwrap_err().to_string();
    assert!(err.contains("not available"), "{err}");
    assert!(!away.join("unplugged").exists());
    // the previews already built need an explicit choice
    let to = away.join("proxies");
    let err = s.execute("library.smartPreviewsLocation", &json!({"path": to.to_string_lossy()})).unwrap_err().to_string();
    assert!(err.contains("existing"), "{err}");
    assert_eq!(s.execute("library.smartPreviewsLocation", &json!({})).unwrap()["custom"], false);

    let r = s.execute("library.smartPreviewsLocation", &json!({"path": to.to_string_lossy(), "existing": "move"})).unwrap();
    assert_eq!((r["custom"].clone(), r["handled"].clone(), r["count"].clone()), (json!(true), json!(1), json!(1)), "{r}");
    assert_eq!(crate::smart::stats(&lib.join("Smart Previews")).0, 0);
    assert_eq!(s.execute("photo.smartPreview", &json!({})).unwrap()["smartPreview"], true);

    // the setting is saved with the library
    drop(s); // one session per library (issue #99)
    let mut again = Session::new().with_fs();
    again.open_library(&lib, false).unwrap();
    assert_eq!(again.execute("library.smartPreviewsLocation", &json!({})).unwrap()["path"], to.to_string_lossy().as_ref());

    // offline original: the render comes from the proxy in the chosen folder
    std::fs::rename(&src, src.with_extension("offline")).unwrap();
    again.media.forget(id);
    again.render_now(id, 48, 32).expect("renders from the smart preview in the chosen folder");

    // the chosen drive goes away: building says so and creates nothing on this drive
    let drive = away.join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    let on_drive = drive.join("proxies");
    again.execute("library.smartPreviewsLocation", &json!({"path": on_drive.to_string_lossy(), "existing": "leave"})).unwrap();
    std::fs::remove_dir_all(&drive).unwrap();
    let loc = again.execute("library.smartPreviewsLocation", &json!({})).unwrap();
    assert_eq!(loc["available"], false, "{loc}");
    let err = again.execute("library.smartPreviews", &json!({"ids": [id.0]})).unwrap_err().to_string();
    assert!(err.contains("not available"), "{err}");
    assert!(!drive.exists());

    // back to the library's own folder, discarding the proxies left in the first chosen folder
    let r = again.execute("library.smartPreviewsLocation", &json!({"reset": true})).unwrap();
    assert_eq!(r["custom"], false, "{r}");
    assert_eq!(r["path"], lib.join("Smart Previews").to_string_lossy().as_ref());
    for d in [&lib, &away, &src.with_extension("offline")] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Missing Photos covers library photos only: a small library next to many Local browse
/// records (and a deleted photo) checks — and lists, and counts — the library files alone.
#[test]
fn missing_photos_skip_local_browse_records() {
    use lcc_catalog::{Op, Photo, PhotoId, Source};
    let gone = std::env::temp_dir().join(format!("lc-missing-scope-{}", std::process::id()));
    let mut s = Session::new();
    let mut ops = Vec::new();
    for i in 0..205u64 {
        let path = gone.join(format!("IMG_{i:04}.jpg")).to_string_lossy().to_string();
        let mut p = Photo::new(PhotoId(i + 1), Source::File { path }, &format!("IMG_{i:04}.jpg"), "JPEG", 60, 40, "2026-01-01T00:00:00");
        p.local = i >= 5; // 5 library photos, 200 seen while browsing
        p.deleted = i == 4;
        ops.push(Op::AddPhoto { photo: Box::new(p) });
    }
    s.commit("Add", Op::Batch { ops }).unwrap();
    let mut checked = 0;
    let lost = crate::cmd::missing::missing_with(&s.catalog, |_| {
        checked += 1;
        false
    });
    assert_eq!((checked, lost.len()), (4, 4), "only the non-deleted library files are checked");
    assert_eq!(crate::cmd::missing::candidates(&s.catalog).len(), 4, "the sidebar count checks the same files");
    assert_eq!(s.execute("library.missing", &json!({})).unwrap().as_array().map(Vec::len), Some(4));
    s.execute("library.source", &json!({"kind": "missing"})).unwrap();
    assert_eq!(s.visible_cloned().len(), 4, "the view lists what the count counts");
}

/// `library.browse` follows `library.source`'s rule for the active photo (issue #1447): a library
/// photo the folder doesn't show gives way to the folder's first photo; one it shows stays.
#[test]
fn browsing_a_folder_keeps_or_picks_the_active_photo() {
    let dir = temp_dir("browse-active");
    write_png(&dir.join("a.png"), 1);
    write_png(&dir.join("b.png"), 2);
    let mut s = Session::with_demo().with_fs();
    let library = s.visible_cloned()[0];
    s.execute("library.select", &json!({"ids": [library.0]})).unwrap();
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    let vis = s.visible_cloned();
    assert_eq!(vis.len(), 2);
    assert_eq!(s.selection.active, Some(vis[0]), "the folder's first photo, not {library:?}");
    s.execute("library.select", &json!({"ids": [vis[1].0]})).unwrap();
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    assert_eq!(s.selection.active, Some(vis[1]), "a photo the folder shows stays active");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Import a file that is in Recently Deleted (issues #298, #706): by default the photo comes
/// back with its edits; `onDeleted: skip` leaves it there but the report says it is in the trash;
/// `onDeleted: fresh` replaces the trashed record with a new one.
fn trashed_photo(tag: &str) -> (Session, std::path::PathBuf, u64) {
    let src = temp_dir(tag);
    write_png(&src.join("a.png"), 1);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = r["imported"][0].as_u64().unwrap();
    s.execute("photo.rate", &json!({"ids": [id], "rating": 4})).unwrap();
    s.execute("photo.delete", &json!({"ids": [id]})).unwrap();
    (s, src, id)
}

/// A Fresh import with files whose content a photo has imported anyway: the trashed photo it
/// replaces holds no content any more, so the new photo takes the content's own key (a `:dup` key
/// would leave the content keyed by no photo, and later copies unnoticed).
#[test]
fn a_fresh_import_of_a_trashed_photos_file_keeps_the_contents_own_key() {
    let (mut s, src, id) = trashed_photo("fresh-own-key");
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "importDuplicates": true, "onDeleted": "fresh"})).unwrap();
    assert_eq!(ids(&r, "imported"), 1, "{r}");
    assert!(s.catalog.photo(lcc_catalog::PhotoId(id)).is_none(), "the trashed record is replaced");
    let keys: Vec<String> = s.catalog.photos().filter_map(|p| p.content_hash.clone()).collect();
    assert!(keys.len() == 1 && !keys[0].contains(':'), "{keys:?}");
    let _ = std::fs::remove_dir_all(&src);
}

/// Issue #706: import a photo, delete it, import the same file again. Before, the import skipped
/// it as "already in the library" (a duplicate by path) while the grid showed it deleted, and the
/// user was stuck. Now the photo is restored: it is back in All Photos, with its edits.
#[test]
fn importing_a_deleted_photo_again_brings_it_back() {
    let (mut s, src, id) = trashed_photo("trash-reimport");
    assert_eq!(s.visible_cloned().len(), 0, "deleted: not in All Photos");
    let r = s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    assert_eq!(r["restored"], json!([id]), "{r}");
    assert_eq!(ids(&r, "duplicates"), 0, "not reported as a duplicate: {r}");
    let p = s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap();
    assert!(!p.deleted && p.rating == 4, "back with its edits");
    assert_eq!(s.catalog.len(), 1, "no second record");
    assert_eq!(s.visible_cloned(), vec![lcc_catalog::PhotoId(id)], "shown again in All Photos");
    // undo puts it back in Recently Deleted
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap().deleted);
    let _ = std::fs::remove_dir_all(&src);
}

/// The same sequence with the library on disk and a restart between the delete and the import:
/// the trashed record is read back from the catalog and restored, not refused.
#[test]
fn importing_a_deleted_photo_again_after_reopening_the_library_brings_it_back() {
    let src = temp_dir("trash-reimport-reopen");
    write_png(&src.join("a.png"), 1);
    let lib = src.join("library");
    let id = {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let r = s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()], "albumName": "Trip"})).unwrap();
        let id = r["imported"][0].as_u64().unwrap();
        s.execute("photo.rate", &json!({"ids": [id], "rating": 2})).unwrap();
        s.execute("photo.delete", &json!({"ids": [id]})).unwrap();
        s.close_library().unwrap();
        id
    };
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert!(s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap().deleted, "deleted state persisted");
    let r = s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    assert_eq!(r["restored"], json!([id]), "{r}");
    assert_eq!(ids(&r, "duplicates"), 0, "{r}");
    let p = s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap();
    assert!(!p.deleted && p.rating == 2, "restored with its edits");
    let album = s.catalog.albums().find(|a| a.name == "Trip").unwrap();
    assert_eq!(album.photos, vec![lcc_catalog::PhotoId(id)], "still in its album");
    assert_eq!(s.visible_cloned().len(), 1);
    let _ = std::fs::remove_dir_all(&src);
}

/// A deleted photo whose file was moved meanwhile: importing the file from its new place restores
/// the photo and points it at the file (matched by content), instead of bringing back a record
/// whose file is missing.
#[test]
fn importing_a_deleted_photo_from_its_new_place_restores_and_relinks_it() {
    let (mut s, src, id) = trashed_photo("trash-reimport-moved");
    std::fs::create_dir_all(src.join("moved")).unwrap();
    std::fs::rename(src.join("a.png"), src.join("moved/b.png")).unwrap();
    let r = s.execute("library.import", &json!({"paths": [src.join("moved/b.png").to_string_lossy()]})).unwrap();
    assert_eq!(r["restored"], json!([id]), "{r}");
    let p = s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap();
    assert!(!p.deleted && p.rating == 4);
    assert_eq!(p.source, lcc_catalog::Source::File { path: src.join("moved/b.png").to_string_lossy().to_string() });
    assert_eq!(p.file_name, "b.png");
    assert_eq!(s.catalog.len(), 1);
    let _ = std::fs::remove_dir_all(&src);
}

/// Only an import brings a deleted photo back. Looking at its folder in Local, and Auto Import
/// seeing its file again in the watched folder (a copy: the source is not a path the library
/// knows), leave it in Recently Deleted.
#[test]
fn browsing_and_auto_import_never_restore_a_deleted_photo() {
    let (mut s, src, id) = trashed_photo("trash-no-silent-restore");
    let r = s.execute("library.browse", &json!({"path": src.to_string_lossy()})).unwrap();
    assert_eq!(r["new"], 0, "{r}");
    assert!(s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap().deleted, "browsing only looks");
    assert_eq!(s.catalog.len(), 1);
    // auto import in copy mode: the trashed photo's copy is in the library's folder, so the watched
    // folder's file is new by path and a duplicate by content
    let lib = src.join("library");
    let watched = src.join("watched");
    write_png(&watched.join("w.png"), 7);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("library.import", &json!({"paths": [watched.join("w.png").to_string_lossy()], "mode": "copy"})).unwrap();
    let copied = r["imported"][0].as_u64().unwrap();
    s.execute("photo.delete", &json!({"ids": [copied]})).unwrap();
    s.execute("library.autoImport", &json!({"folder": watched.to_string_lossy(), "copy": true})).unwrap();
    for _ in 0..3 {
        let r = s.execute("library.autoImportScan", &json!({})).unwrap();
        assert_eq!(r["imported"], json!([]), "{r}");
    }
    assert!(s.catalog.photo(lcc_catalog::PhotoId(copied)).unwrap().deleted, "still deleted");
    assert_eq!(s.catalog.len(), 1, "no second copy");
    let _ = std::fs::remove_dir_all(&src);
}

/// `onDeleted` for a file whose photo is in Recently Deleted (issues #298, #706): an unknown value is refused;
/// `skip` leaves the photo there and reports the duplicate with `existingDeleted`; `restore` brings it back with its
/// edits, into the import's album; `fresh` replaces the trashed record with a new photo.
#[test]
fn on_deleted_skips_restores_or_replaces_a_trashed_photo() {
    let (mut s, src, id) = trashed_photo("trash-on-deleted");
    let e = s.execute("library.import", &json!({"paths": [], "onDeleted": "bogus"})).unwrap_err().to_string();
    assert!(e.contains("onDeleted"), "{e}");

    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "onDeleted": "skip"})).unwrap();
    assert_eq!(ids(&r, "imported"), 0, "{r}");
    assert_eq!(r["duplicates"][0]["existing"], id, "{r}");
    assert_eq!(r["duplicates"][0]["existingDeleted"], true, "{r}");
    assert!(s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap().deleted, "skip: left in the trash");

    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "onDeleted": "restore", "albumName": "Back"})).unwrap();
    assert_eq!(r["restored"], json!([id]), "{r}");
    assert_eq!(ids(&r, "duplicates"), 0, "{r}");
    let p = s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap();
    assert!(!p.deleted && p.rating == 4, "restored with its edits");
    let album = r["album"].as_u64().expect("album created for the restored photos");
    assert_eq!(s.catalog.album(lcc_catalog::AlbumId(album)).unwrap().photos.len(), 1);

    s.execute("photo.delete", &json!({"ids": [id]})).unwrap();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "onDeleted": "fresh"})).unwrap();
    assert_eq!(ids(&r, "imported"), 1, "{r}");
    let new = r["imported"][0].as_u64().unwrap();
    assert_ne!(new, id);
    assert!(s.catalog.photo(lcc_catalog::PhotoId(id)).is_none(), "the trashed record is gone");
    let p = s.catalog.photo(lcc_catalog::PhotoId(new)).unwrap();
    assert!(!p.deleted && p.rating == 0, "a new photo without the old edits");
    assert_eq!(s.catalog.len(), 1);
    let _ = std::fs::remove_dir_all(&src);
}

/// A trashed photo imported again as a new one owns its stem sidecar: its own old record is not a
/// rival for `a.xmp` (issue #1905's ranking), so the fresh photo reads it.
#[test]
fn a_fresh_reimport_reads_its_own_stem_sidecar() {
    let (mut s, src, id) = trashed_photo("trash-fresh-sidecar");
    std::fs::write(
        src.join("a.xmp"),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="3"/></rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "onDeleted": "fresh"})).unwrap();
    assert_eq!(ids(&r, "imported"), 1, "{r}");
    let new = r["imported"][0].as_u64().unwrap();
    assert_ne!(new, id);
    assert_eq!(s.catalog.photo(lcc_catalog::PhotoId(new)).unwrap().rating, 3, "the fresh photo reads a.xmp: {r}");
    let _ = std::fs::remove_dir_all(&src);
}

/// Two trashed photos that share an album and a two-photo stack: emptying (or permanently
/// deleting) them together must not fail or leave dangling references.
fn two_trashed_in_album_and_stack(tag: &str) -> (Session, std::path::PathBuf, [u64; 2], u64) {
    let src = temp_dir(tag);
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let ids = [r["imported"][0].as_u64().unwrap(), r["imported"][1].as_u64().unwrap()];
    let al = s.execute("album.create", &json!({"name": "Trip"})).unwrap()["id"].as_u64().unwrap();
    s.execute("album.addPhotos", &json!({"id": al, "ids": ids})).unwrap();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    s.execute("stack.group", &json!({"ids": ids})).unwrap();
    s.execute("photo.delete", &json!({"ids": ids})).unwrap();
    (s, src, ids, al)
}

#[test]
fn emptying_trashed_photos_of_one_album_and_stack_leaves_nothing_dangling() {
    let (mut s, src, ids, al) = two_trashed_in_album_and_stack("trash-empty-refs");
    let r = s.execute("library.emptyRecentlyDeleted", &json!({})).unwrap();
    assert_eq!(r["deleted"], 2, "{r}");
    assert_eq!(s.catalog.len(), 0);
    assert!(s.catalog.album(lcc_catalog::AlbumId(al)).unwrap().photos.is_empty(), "album still lists a removed photo");
    assert!(ids.iter().all(|i| s.catalog.stack_of(lcc_catalog::PhotoId(*i)).is_none()));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.len(), 2, "one undo step brings both back");
    assert_eq!(s.catalog.album(lcc_catalog::AlbumId(al)).unwrap().photos.len(), 2);
    assert!(s.catalog.stack_of(lcc_catalog::PhotoId(ids[0])).is_none(), "soft-delete already left the stack");
    assert!(s.catalog.photos().all(|p| p.deleted), "back in Recently Deleted");
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn deleting_several_photos_permanently_leaves_nothing_dangling() {
    let (mut s, src, ids, al) = two_trashed_in_album_and_stack("trash-perm-refs");
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    let files = [src.join("a.png"), src.join("b.png")].map(|path| {
        let bytes = std::fs::read(&path).unwrap();
        (path, bytes)
    });
    let snapshot = s.catalog.to_snapshot();
    s.drain_log();
    s.execute("photo.deletePermanently", &json!({"ids": ids})).unwrap();
    assert!(s.catalog.album(lcc_catalog::AlbumId(al)).unwrap().photos.is_empty());
    assert!(ids.iter().all(|i| s.catalog.photo(lcc_catalog::PhotoId(*i)).is_none()));
    let removed = s.catalog.to_snapshot();
    let log: String = s.drain_log().iter().map(lcc_catalog::Catalog::op_to_log_line).collect();
    let mut reopened = lcc_catalog::Catalog::from_snapshot(&snapshot).unwrap();
    reopened.replay(&log).unwrap();
    assert_eq!(reopened.to_snapshot(), removed);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.to_snapshot(), snapshot, "one undo restores photos, album and stack");
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(s.catalog.to_snapshot(), removed);
    for (path, bytes) in files {
        assert_eq!(std::fs::read(path).unwrap(), bytes, "permanent deletion only removes catalog records");
    }
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn fresh_import_of_two_trashed_photos_in_one_album_and_stack_works() {
    let (mut s, src, trashed, al) = two_trashed_in_album_and_stack("trash-fresh-refs");
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "onDeleted": "fresh"})).unwrap();
    assert_eq!(ids(&r, "imported"), 2, "{r}");
    assert!(s.catalog.album(lcc_catalog::AlbumId(al)).unwrap().photos.is_empty());
    assert!(trashed.iter().all(|i| s.catalog.photo(lcc_catalog::PhotoId(*i)).is_none()));
    let _ = std::fs::remove_dir_all(&src);
}

/// Issue #374: launch arguments never turn into a walk of a whole drive or home folder. An empty
/// argument is ignored; `.`, `..` and (on Windows) `/` are resolved against the working folder a
/// launcher picked, which is often a drive root; a root is replaced by its memory card's DCIM
/// folder or refused, and so is the home folder.
#[test]
fn launch_paths_never_become_a_drive_root_or_the_home_folder() {
    use crate::import::{LaunchPath, launch_path};
    let root = if cfg!(windows) { Path::new("C:\\") } else { Path::new("/") };
    let home = if cfg!(windows) { Path::new("C:\\Users\\me") } else { Path::new("/home/me") };
    let none = |_: &Path| None;
    let refused = |r: Option<LaunchPath>| matches!(r, Some(LaunchPath::Refused { .. }));
    for empty in ["", "  "] {
        assert_eq!(launch_path(empty, Some(root), Some(home), none), None, "{empty:?}");
    }
    // the working folder is the drive root: `.` (and `..`) name the whole drive
    for arg in [".", "..", "./", ".\\..", "/"] {
        let arg = if cfg!(windows) { arg.to_string() } else { arg.replace('\\', "/") };
        assert!(refused(launch_path(&arg, Some(root), Some(home), none)), "{arg:?}");
    }
    assert!(refused(launch_path("..", Some(&home.join("Pictures")), Some(home), none)), "up to the home folder");
    assert!(refused(launch_path(&home.to_string_lossy(), None, Some(home), none)));
    assert!(refused(launch_path(".", None, Some(home), none)), "relative, no working folder");
    // a memory card's root: its camera folder
    let dcim = |r: &Path| Some(r.join("DCIM"));
    assert_eq!(launch_path(".", Some(root), Some(home), dcim), Some(LaunchPath::Import(root.join("DCIM").to_string_lossy().to_string())));
    // ordinary folders and files resolve to absolute paths
    let pics = home.join("Pictures");
    assert_eq!(launch_path("Pictures", Some(home), Some(home), none), Some(LaunchPath::Import(pics.to_string_lossy().to_string())));
    assert_eq!(
        launch_path(&pics.join("a.jpg").to_string_lossy(), None, Some(home), none),
        Some(LaunchPath::Import(pics.join("a.jpg").to_string_lossy().to_string()))
    );
}

/// Folder expansion is bounded (issue #374) and says what it left out.
#[test]
fn expand_stops_at_the_walk_limits_and_reports_what_the_walks_left_out() {
    // Issue #374: a folder import is bounded — a tree deeper or larger than the limits is not walked
    // to the end (it stops and says so), and what it found is still returned.
    {
        let d = temp_dir("bounded");
        let mut deep = d.clone();
        for i in 0..8 {
            deep = deep.join(format!("d{i}"));
            std::fs::create_dir_all(&deep).unwrap();
            std::fs::write(deep.join(format!("p{i}.jpg")), b"x").unwrap();
        }
        let top = [d.to_string_lossy().to_string()];
        let (all, cut) = crate::import::expand_within(&top, None, crate::walk::Limits::default());
        assert_eq!((all.len(), cut), (8, false));
        let (some, cut) = crate::import::expand_within(&top, None, crate::walk::Limits { max_depth: 3, max_entries: 1000 });
        assert_eq!((some.len(), cut), (2, true), "{some:?}");
        let (few, cut) = crate::import::expand_within(&top, None, crate::walk::Limits { max_depth: 32, max_entries: 5 });
        assert!(cut && few.len() < 8, "{few:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
    // Expanding folders reports what the walks left out — files of an unsupported type, a walk that
    // stopped at a bound — and returns the same files as `expand_within`; a file named on its own is
    // attempted whatever its extension and is not counted as unsupported.
    {
        use crate::import::{expand_report, expand_within};
        let d = temp_dir("expand-report");
        write_png(&d.join("a.png"), 1);
        write_png(&d.join("sub/b.png"), 2);
        std::fs::write(d.join("notes.txt"), "x").unwrap();
        std::fs::write(d.join("sub/readme.md"), "x").unwrap();
        std::fs::write(d.join("sub/.hidden.txt"), "x").unwrap();
        let named = temp_dir("expand-report-named");
        std::fs::write(named.join("odd.dat"), "x").unwrap();
        let paths = [d.to_string_lossy().to_string(), named.join("odd.dat").to_string_lossy().to_string()];
        let limits = crate::walk::Limits::default();

        let e = expand_report(&paths, None, limits);
        assert_eq!(e.files.len(), 3, "both photos and the named file: {:?}", e.files);
        assert_eq!((e.unsupported, e.unreadable, e.truncated), (2, 0, false), "notes.txt and readme.md; the hidden file is not counted: {e:?}");
        assert_eq!(expand_within(&paths, None, limits), (e.files.clone(), false), "expand_within is the same walk");

        // stopping one level down: the second folder is never listed, so its files are not counted
        let e = expand_report(&paths[..1], None, crate::walk::Limits { max_depth: 1, max_entries: 1000 });
        assert_eq!((e.files.len(), e.unsupported, e.truncated), (1, 1, true), "{e:?}");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&named);
    }
}

/// Raw containers the app cannot decode but shows by their embedded JPEG are picked up from a folder
/// (the extensions are in `import::EXTENSIONS`; the files differ so none is a duplicate) and import as preview only.
#[test]
fn folder_import_picks_up_undecodable_raw_containers_as_preview_only() {
    use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let src = temp_dir("preview-only-exts");
    let px: Vec<u8> = (0..40 * 30).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
    let jpeg = encode_jpeg(&EncodeImage::new(40, 30, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    // CRW / X3F: their magic, then the JPEG. IIQ / KDC / MOS / ERF: a TIFF whose IFD0 only points at the JPEG.
    let tiff = {
        let off = 8 + 2 + 24 + 4;
        let mut f = b"II*\0\x08\0\0\0\x02\0".to_vec();
        for (tag, v) in [(513u16, off as u32), (514, jpeg.len() as u32)] {
            f.extend_from_slice(&tag.to_le_bytes());
            f.extend_from_slice(&[4, 0, 1, 0, 0, 0]);
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&[0, 0, 0, 0]);
        f.extend_from_slice(&jpeg);
        f
    };
    let with_magic = |magic: &[u8]| [magic, &[0x11u8; 64][..], &jpeg].concat();
    let files: [(&str, Vec<u8>); 6] = [
        ("a.CRW", with_magic(b"II\x1a\0\0\0HEAPCCDR")),
        ("c.x3f", with_magic(b"FOVb\x02\0\x02\0")),
        ("d.iiq", [&tiff[..], b"d"].concat()),
        ("e.kdc", [&tiff[..], b"e"].concat()),
        ("f.mos", [&tiff[..], b"f"].concat()),
        ("g.erf", [&tiff[..], b"g"].concat()),
    ];
    for (name, bytes) in &files {
        std::fs::write(src.join(name), bytes).unwrap();
    }
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(r["scanned"], 6, "{r}");
    assert_eq!(ids(&r, "imported"), 6, "{r}");
    assert_eq!(ids(&r, "failed"), 0, "{r}");
    assert!(s.catalog.photos().all(|p| p.preview_only.is_some() && p.kind == lcc_catalog::MediaKind::Raw && (p.width, p.height) == (40, 30)));
    let _ = std::fs::remove_dir_all(&src);
}

/// Rewrite the JSON header line of the smart preview at `file`.
fn edit_smart_header(file: &Path, edit: impl FnOnce(&mut Value)) {
    let bytes = std::fs::read(file).unwrap();
    let start = b"LCSP1\n".len();
    let nl = start + bytes[start..].iter().position(|b| *b == b'\n').unwrap();
    let mut head: Value = serde_json::from_slice(&bytes[start..nl]).unwrap();
    edit(&mut head);
    let mut out = bytes[..start].to_vec();
    out.extend_from_slice(head.to_string().as_bytes());
    out.extend_from_slice(&bytes[nl..]);
    std::fs::write(file, out).unwrap();
}

fn smart_header(file: &Path) -> Value {
    let bytes = std::fs::read(file).unwrap();
    let start = b"LCSP1\n".len();
    let nl = start + bytes[start..].iter().position(|b| *b == b'\n').unwrap();
    serde_json::from_slice(&bytes[start..nl]).unwrap()
}

/// A camera tone curve with a recognisable shape (`k` changes it).
fn test_tone(k: f32) -> lcc_pipeline::tone::CameraTone {
    lcc_pipeline::tone::CameraTone::new(std::array::from_fn(|i| {
        let x = 0.004 * 1.17f32.powi(i as i32);
        [x, 0.95 * (1.0 - (-k * x).exp())]
    }))
    .unwrap()
}

/// `plain`, but reporting `tone` as the camera look the (current) fit gives the file; counts its calls.
fn look_loader(
    plain: crate::media::FileLoader,
    tone: lcc_pipeline::tone::CameraTone,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> crate::media::FileLoader {
    std::sync::Arc::new(move |path, max_edge, options| {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (img, mut info) = plain(path, max_edge, options)?;
        info.camera_tone = Some(tone);
        Ok((img, info))
    })
}

/// The follow-up to the camera tone curve change (#499): a smart preview keeps the look the fit
/// gave it, so one written by an older fit is rebuilt when its original is online, kept while the
/// original is offline, and not rebuilt again once current. Only raws that take the per-file
/// camera look go stale (a PNG's proxy never does), and the opening scan runs once per fit version.
#[test]
fn smart_previews_from_an_older_look_fit_are_refreshed() {
    for format in ["ARW", "DNG"] {
        smart_look_refresh_for(format);
    }
}

fn smart_look_refresh_for(format: &str) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let src = temp_dir(&format!("smartlook-src-{format}"));
    let lib = temp_dir(&format!("smartlook-lib-{format}"));
    write_png(&src.join("a.png"), 3);
    write_png(&src.join("b.png"), 9);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy(), src.join("b.png").to_string_lossy()]})).unwrap();
    let find = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).unwrap().id;
    let (id, png) = (find(&s, "a.png"), find(&s, "b.png"));
    // `a` stands for a raw that takes the per-file camera look (the test loader decodes its pixels)
    let mut raw = (**s.catalog.photo(id).unwrap()).clone();
    raw.format = format.into();
    raw.kind = lcc_catalog::MediaKind::Raw;
    s.catalog.apply(lcc_catalog::Op::RemovePhoto { id }).unwrap();
    s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(raw) }).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().relative_wb(), format == "ARW");
    assert!(!s.catalog.photo(png).unwrap().relative_wb());
    s.execute("library.select", &json!({"ids": [id.0, png.0], "active": id.0})).unwrap();
    let dir = s.media.smart_dir.clone().unwrap();
    let file = crate::smart::sharded_file_path(&dir, s.catalog.photo(id).unwrap());
    let png_file = crate::smart::sharded_file_path(&dir, s.catalog.photo(png).unwrap());
    let (old, new) = (test_tone(1.5), test_tone(3.5));
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let plain = s.media.file_loader.clone().unwrap();
    s.media.file_loader = Some(look_loader(plain.clone(), old, calls.clone()));
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!((r["built"].as_u64(), r["refreshed"].as_u64()), (Some(2), Some(0)), "{r}");
    assert_eq!(smart_header(&file)["look_version"], json!(crate::camera_preview::LOOK_VERSION), "the writer stamps the fit version");
    assert!(!crate::smart::is_stale(&file));

    // current header: kept as it is, the original is not even read
    let before = std::fs::read(&file).unwrap();
    calls.store(0, Ordering::SeqCst);
    s.media.file_loader = Some(look_loader(plain.clone(), new, calls.clone()));
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!((r["built"].as_u64(), r["refreshed"].as_u64()), (Some(2), Some(0)), "{r}");
    assert!(std::fs::read(&file).unwrap() == before, "a current proxy is not rebuilt");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // old headers (no field), originals offline: kept unchanged, and still marked stale
    for f in [&file, &png_file] {
        edit_smart_header(f, |h| {
            h.as_object_mut().unwrap().remove("look_version");
        });
    }
    assert!(crate::smart::is_stale(&file), "no field = version 0");
    let old_bytes = std::fs::read(&file).unwrap();
    let png_bytes = std::fs::read(&png_file).unwrap();
    std::fs::rename(&src, src.with_extension("offline")).unwrap();
    s.media.forget(id);
    s.media.forget(png);
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!((r["built"].as_u64(), r["refreshed"].as_u64(), r["staleKept"].as_u64()), (Some(2), Some(0), Some(1)), "{r}");
    assert!(r["failed"].as_array().unwrap().is_empty(), "{r}");
    assert!(std::fs::read(&file).unwrap() == old_bytes, "offline: the stored preview and curve are untouched");
    assert!(crate::smart::is_stale(&file), "and it stays marked for the next time");
    assert_eq!(crate::smart::decode(&old_bytes).unwrap().1, Some(old), "the old look is still there");
    // ... and the opening refresh keeps it too, then marks the folder checked
    assert!(!crate::smart::look_checked(&dir));
    crate::cmd::previews::refresh_stale_smart_previews(&mut s).unwrap().join().unwrap();
    assert!(std::fs::read(&file).unwrap() == old_bytes);
    assert!(crate::smart::look_checked(&dir));

    // the original comes back: the scan has run at this version, so the next opening leaves it;
    // Build Smart Previews rebuilds it with the current look
    std::fs::rename(src.with_extension("offline"), &src).unwrap();
    calls.store(0, Ordering::SeqCst);
    assert!(crate::cmd::previews::refresh_stale_smart_previews(&mut s).is_none(), "checked once per version");
    assert!(std::fs::read(&file).unwrap() == old_bytes);
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!((r["refreshed"].as_u64(), calls.load(Ordering::SeqCst)), (Some(1), 1), "{r}");
    assert_eq!(smart_header(&file)["look_version"], json!(crate::camera_preview::LOOK_VERSION));
    assert_eq!(crate::smart::decode(&std::fs::read(&file).unwrap()).unwrap().1, Some(new), "rebuilt with the current look");
    assert!(!crate::smart::is_stale(&file));
    assert!(std::fs::read(&png_file).unwrap() == png_bytes, "a PNG's proxy is never rebuilt for the look");

    // a folder checked at an older version (or not at all) is scanned once at the next opening
    edit_smart_header(&file, |h| h["look_version"] = json!(0));
    std::fs::write(dir.join("look-version"), b"0\n").unwrap();
    assert!(!crate::smart::look_checked(&dir));
    s.media.forget(id);
    calls.store(0, Ordering::SeqCst);
    crate::cmd::previews::refresh_stale_smart_previews(&mut s).unwrap().join().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "only the raw's original is read");
    assert!(!crate::smart::is_stale(&file));
    assert!(std::fs::read(&png_file).unwrap() == png_bytes);
    assert!(crate::smart::look_checked(&dir));
    // once only
    let rebuilt = std::fs::read(&file).unwrap();
    assert!(crate::cmd::previews::refresh_stale_smart_previews(&mut s).is_none());
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!((r["refreshed"].as_u64(), calls.load(Ordering::SeqCst)), (Some(0), 1), "{r}");
    assert!(std::fs::read(&file).unwrap() == rebuilt);

    // Build Smart Previews refreshes a stale one the same way
    edit_smart_header(&file, |h| h["look_version"] = json!(0));
    s.media.file_loader = Some(plain);
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!(r["refreshed"], 1, "{r}");
    assert_eq!(crate::smart::decode(&std::fs::read(&file).unwrap()).unwrap().1, None, "this fit gives no curve: none is kept");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// Issue #1534 (review): a proxy's encoding version is migrated independently of the per-file
/// camera look. A DNG keeps an absolute white balance, so it never takes that look; at head its
/// existing proxy was kept at an older encoding even with its original online, so the decoder
/// facts the newer encoding carries never reached it through a normal rebuild.
#[test]
fn a_proxy_with_an_older_encoding_is_refreshed_without_the_camera_look() {
    let src = temp_dir("smartenc-src");
    let lib = temp_dir("smartenc-lib");
    write_png(&src.join("a.png"), 3);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    // the test loader decodes the catalogue photo's "DNG" pixels
    let pixels = lcc_raster::Rgb32f::from_fn(48, 32, |x, y| [0.02 + (x % 9) as f32 * 0.05, 0.03 + (y % 7) as f32 * 0.04, 0.06]);
    let info = lcc_pipeline::SourceInfo { raw: true, as_shot_temp: 5200.0, as_shot_tint: 6.0, ..Default::default() };
    let (p, i) = (pixels.clone(), info.clone());
    s.media.file_loader = Some(std::sync::Arc::new(move |_path, _max_edge, _options| Ok((p.clone(), i.clone()))));
    s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let mut raw = (**s.catalog.photo(id).unwrap()).clone();
    raw.format = "DNG".into();
    raw.kind = lcc_catalog::MediaKind::Raw;
    s.catalog.apply(lcc_catalog::Op::RemovePhoto { id }).unwrap();
    s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(raw) }).unwrap();
    assert!(s.catalog.photo(id).unwrap().develops_raw() && !s.catalog.photo(id).unwrap().fitted_look(), "a DNG does not take the camera look");
    s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    let dir = s.media.smart_dir.clone().unwrap();
    let file = crate::smart::sharded_file_path(&dir, s.catalog.photo(id).unwrap());
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!((r["built"].as_u64(), r["refreshed"].as_u64()), (Some(1), Some(0)), "{r}");
    assert_eq!(smart_header(&file)["enc"], json!(crate::smart::ENCODE_VERSION));

    // an existing proxy from the previous encoding, its original online: the migration must reach
    // it through a normal rebuild even though the photo never takes the camera look
    edit_smart_header(&file, |h| h["enc"] = json!(crate::smart::ENCODE_VERSION - 1));
    let stale_bytes = std::fs::read(&file).unwrap();
    assert!(crate::smart::is_stale(&file), "the older encoding is stale even without the camera look");
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!((r["built"].as_u64(), r["refreshed"].as_u64(), r["staleKept"].as_u64()), (Some(1), Some(1), Some(0)), "{r}");
    assert_eq!(smart_header(&file)["enc"], json!(crate::smart::ENCODE_VERSION), "migrated to the current encoding");
    assert!(!crate::smart::is_stale(&file));
    assert_ne!(std::fs::read(&file).unwrap(), stale_bytes, "the proxy was rebuilt");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// Issue #1534 (review): a cached proxy source is not the original. After an offline edit rendered
/// from a stale proxy, Build Smart Previews read that cached source back and refreshed the proxy
/// from itself, stamping its older look as current. A stale refresh reads the original only, so an
/// offline original leaves the stored look — and the bytes — as they are.
#[test]
fn a_stale_refresh_never_rebuilds_a_proxy_from_itself() {
    let src = temp_dir("smartself-src");
    let lib = temp_dir("smartself-lib");
    write_png(&src.join("a.png"), 3);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let pixels = lcc_raster::Rgb32f::from_fn(48, 32, |x, y| [0.02 + (x % 9) as f32 * 0.05, 0.03 + (y % 7) as f32 * 0.04, 0.06]);
    let info = lcc_pipeline::SourceInfo { raw: true, as_shot_temp: 5200.0, as_shot_tint: 6.0, ..Default::default() };
    let (p, i) = (pixels.clone(), info.clone());
    s.media.file_loader = Some(std::sync::Arc::new(move |_path, _max_edge, _options| Ok((p.clone(), i.clone()))));
    s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    // a raw that takes the per-file camera look, so an older fit makes its proxy stale
    let mut raw = (**s.catalog.photo(id).unwrap()).clone();
    raw.format = "ARW".into();
    raw.kind = lcc_catalog::MediaKind::Raw;
    s.catalog.apply(lcc_catalog::Op::RemovePhoto { id }).unwrap();
    s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(raw) }).unwrap();
    assert!(s.catalog.photo(id).unwrap().fitted_look());
    s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    let dir = s.media.smart_dir.clone().unwrap();
    let file = crate::smart::sharded_file_path(&dir, s.catalog.photo(id).unwrap());
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!(r["built"], 1, "{r}");
    edit_smart_header(&file, |h| h["look_version"] = json!(0));
    let stale_bytes = std::fs::read(&file).unwrap();
    assert!(crate::smart::is_stale(&file), "the stored look is behind this build's fit");

    // the original goes offline, then a preview-size render loads the proxy into the source cache
    std::fs::rename(&src, src.with_extension("offline")).unwrap();
    s.media.forget(id);
    let _ = s.render_now(id, 600, 600).expect("renders from the smart preview");

    // Build Smart Previews must not take that cached proxy for the original: it can't refresh a
    // stale proxy from itself
    let r = s.execute("library.smartPreviews", &json!({})).unwrap();
    assert_eq!((r["refreshed"].as_u64(), r["staleKept"].as_u64()), (Some(0), Some(1)), "{r}");
    assert_eq!(std::fs::read(&file).unwrap(), stale_bytes, "the offline proxy is preserved");
    assert!(crate::smart::is_stale(&file), "still stale: the original was not read");
    let _ = std::fs::remove_dir_all(src.with_extension("offline"));
    let _ = std::fs::remove_dir_all(&lib);
}

/// Auto Sync of a Temperature edit gives every selected raw the same Kelvin, even raws whose stored white balance
/// is still on the legacy relative scale (6500 K = as shot) with different as-shot whites (#425).
#[test]
fn auto_sync_white_balance_gives_every_raw_the_same_kelvin() {
    let src = temp_dir("autosync-wb-src");
    let lib = temp_dir("autosync-wb-lib");
    for i in 0..3u8 {
        write_png(&src.join(format!("{i}.png")), i);
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let paths: Vec<String> = (0..3).map(|i| src.join(format!("{i}.png")).to_string_lossy().into_owned()).collect();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<lcc_catalog::PhotoId> = s.catalog.photos().map(|p| p.id).collect();
    assert_eq!(ids.len(), 3);
    // fitted-look raws of a camera with a Kelvin scale, each shot under its own light
    for (id, as_shot) in ids.iter().zip([5000.0, 6000.0, 5000.0]) {
        let mut raw = (**s.catalog.photo(*id).unwrap()).clone();
        raw.format = "NEF".into();
        raw.kind = lcc_catalog::MediaKind::Raw;
        raw.meta.camera = "NIKON CORPORATION NIKON Z5_2".into();
        raw.as_shot_wb = Some((as_shot, 4.0));
        s.catalog.apply(lcc_catalog::Op::RemovePhoto { id: *id }).unwrap();
        s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(raw) }).unwrap();
        assert!(s.source_info(*id).legacy_relative_wb, "the stored white balance is on the legacy scale");
    }
    // the active photo's white balance was edited before (now on the Kelvin scale); the others never were
    s.execute("library.select", &json!({"ids": [ids[0].0], "active": ids[0].0})).unwrap();
    s.execute("develop.set", &json!({"control": "wb.temp", "value": 5000})).unwrap();
    let raw_ids: Vec<u64> = ids.iter().map(|i| i.0).collect();
    s.execute("library.select", &json!({"ids": raw_ids, "active": ids[0].0})).unwrap();
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    s.execute("develop.set", &json!({"control": "wb.temp", "value": 4000})).unwrap();
    let source = s.develop_of(ids[0]).unwrap().wb;
    assert_eq!(source.temp, 4000.0);
    for id in &ids {
        let wb = s.develop_of(*id).unwrap().wb;
        assert_eq!((wb.temp, wb.tint, wb.scale), (source.temp, source.tint, lcc_develop::WbScale::Kelvin), "photo {id:?}");
    }
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&lib);
}

/// Hostile input for the smart previews' look bookkeeping: the folder's marker file and a proxy's `look_version`.
#[test]
fn smart_preview_look_markers_and_versions_are_read_safely() {
    // The look marker: missing, damaged or older than this build's fit version counts as unchecked.
    {
        let dir = temp_dir("smartlook-marker");
        assert!(!crate::smart::look_checked(&dir), "no marker");
        let cur = crate::camera_preview::LOOK_VERSION;
        let enc = crate::smart::ENCODE_VERSION;
        for (text, checked) in [
            (String::new(), false),
            ("banana".into(), false),
            ("-1".into(), false),
            (format!("{}", cur.saturating_sub(1)), false),
            ("9".repeat(200), false),
            // Written before the encode version was recorded: only says the look was checked, so the
            // folder is scanned once more and the old-encoding proxies are rebuilt.
            (format!("{cur}\n"), false),
            (format!("{cur} {}\n", enc.saturating_sub(1)), false),
            (format!("{cur} banana\n"), false),
            (format!("{cur} {enc}\n"), true),
            (format!(" {} {}\n", cur + 1, enc + 1), true),
        ] {
            std::fs::write(dir.join("look-version"), &text).unwrap();
            assert_eq!(crate::smart::look_checked(&dir), checked, "{text:?}");
        }
        std::fs::remove_file(dir.join("look-version")).unwrap();
        crate::smart::mark_look_checked(&dir).unwrap();
        assert!(crate::smart::look_checked(&dir));
        assert!(crate::smart::mark_look_checked(&dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
    // A hostile or damaged `look_version` never panics: anything that is not a non-negative integer
    // counts as 0 (rebuilt when the original is online); a version from a newer build is left alone.
    {
        let dir = temp_dir("smartlook-bad");
        let img = lcc_scenes::demo_library()[0].render(32, 20);
        let file = dir.join("a.lcsp");
        std::fs::write(&file, crate::smart::encode(&img, None).unwrap()).unwrap();
        let cur = u64::from(crate::camera_preview::LOOK_VERSION);
        for bad in [json!("1"), json!(-1), json!(1.5), json!(null), json!([]), json!({}), json!(true), json!(1e30), json!(-0.0)] {
            let shown = bad.to_string();
            edit_smart_header(&file, |h| h["look_version"] = bad);
            assert!(crate::smart::is_valid(&file), "{shown}");
            assert!(crate::smart::is_stale(&file), "{shown} counts as version 0");
            assert!(crate::smart::decode(&std::fs::read(&file).unwrap()).is_ok(), "{shown}");
        }
        for fine in [json!(cur), json!(cur + 1), json!(u64::MAX)] {
            let shown = fine.to_string();
            edit_smart_header(&file, |h| h["look_version"] = fine);
            assert!(!crate::smart::is_stale(&file), "{shown}");
        }
        // not a header at all, or missing
        assert!(!crate::smart::is_stale(&dir.join("missing.lcsp")));
        std::fs::write(&file, b"LCSP1\n{not json\n\xFF\xD8").unwrap();
        assert!(!crate::smart::is_stale(&file));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Files an import can't take are reported, never catalogued: one gone since the review, one still being copied in.
#[test]
fn an_import_reports_files_gone_or_still_being_copied() {
    {
        let src = temp_dir("gone-since-review");
        write_png(&src.join("a.png"), 1);
        write_png(&src.join("b.png"), 2);
        let mut s = Session::new().with_fs();
        s.execute("library.importPreview", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        std::fs::remove_file(src.join("b.png")).unwrap();
        let r = s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy(), src.join("b.png").to_string_lossy()]})).unwrap();
        assert_eq!((ids(&r, "imported"), ids(&r, "failed")), (1, 1), "{r}");
        assert_eq!(s.catalog.len(), 1, "no photo for a file that isn't there");
        let _ = std::fs::remove_dir_all(&src);
    }
    // A file skipped as still being copied in is named in the import's result (`still_copying`), not left out silently,
    // so the caller can say so instead of calling it unreadable.
    {
        let dir = temp_dir("still-copying");
        let raw = dir.join("IMG_0001.NEF");
        std::fs::write(&raw, short_raw()).unwrap();
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [raw.to_string_lossy()]})).unwrap();
        assert_eq!(ids(&r, "imported"), 0, "{r}");
        let copying = r["still_copying"].as_array().unwrap();
        assert_eq!(copying.len(), 1, "{r}");
        assert!(copying[0].as_str().unwrap().ends_with("IMG_0001.NEF"), "{r}");
        assert!(r["failed"].as_array().unwrap().is_empty(), "{r}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A HEIC laid out like an iPhone's (a grid of HEVC pictures, a Display P3 profile, a quarter turn
/// in the container, a thumbnail) imports as a normal photo that develops and exports upright like
/// a JPEG. In a build without the codecs' `heif` feature it is reported as failed, saying why.
#[test]
fn heic_imports_as_a_normal_photo_or_says_why_not() {
    use lcc_heif::testdata::{Prop, Spec, build};
    let src = temp_dir("heic");
    let photo = |x: u32, y: u32| [((x * 7 + y * 13) % 200 + 30) as u16, 110, 150];
    let p3 = lcc_codecs::icc::write_named(lcc_codecs::NamedSpace::DisplayP3);
    let bytes = build(&Spec { props: vec![Prop::Icc(p3), Prop::Irot(1)], thumbnail: Some(&photo), ..Spec::new(96, 64, &photo) });
    std::fs::write(src.join("IMG_0001.HEIC"), &bytes).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    if !lcc_codecs::Format::Heif.can_decode() {
        assert_eq!((ids(&r, "imported"), ids(&r, "failed")), (0, 1), "{r}");
        assert!(r["failed"].to_string().contains("isn't included in this build"), "{r}");
        let _ = std::fs::remove_dir_all(&src);
        return;
    }
    assert_eq!((ids(&r, "imported"), ids(&r, "failed")), (1, 0), "{r}");
    let p = s.catalog.photos().next().unwrap();
    let id = p.id;
    // Upright (the container's turn applied) and not preview-only.
    assert_eq!((p.kind, p.preview_only.clone(), p.width, p.height), (lcc_catalog::MediaKind::Image, None, 64, 96));
    s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    let plain = crate::export::export_photo(&mut s, id, &crate::export::ExportOptions::from_json(&json!({"format": "png"})), 1).unwrap();
    assert_eq!((plain.width, plain.height), (64, 96));
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    let brighter = crate::export::export_photo(&mut s, id, &crate::export::ExportOptions::from_json(&json!({"format": "png"})), 1).unwrap();
    let mean = |png: &[u8]| {
        let d = lcc_codecs::decode(png, Default::default()).unwrap();
        d.image.data.iter().map(|p| p[1]).sum::<f32>() / d.image.data.len() as f32
    };
    assert!(mean(&brighter.bytes) > mean(&plain.bytes) * 1.3, "exposure +1 brightens the HEIC");
    let _ = std::fs::remove_dir_all(&src);
}

/// Camera tags from #2290's #2284 fixture, including an orientation that must not rotate HEIF pixels.
fn heic_camera_exif(date: &str) -> Vec<u8> {
    use lcc_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value, tags as t};
    let mut exif = IfdBuilder::new();
    exif.set(t::DATE_TIME_ORIGINAL, Value::Ascii(date.into()));
    exif.set(t::OFFSET_TIME_ORIGINAL, Value::Ascii("-05:00".into()));
    let mut ifd0 = IfdBuilder::new();
    ifd0.set(t::MAKE, Value::Ascii("Apple".into()));
    ifd0.set(t::MODEL, Value::Ascii("iPhone 15 Pro".into()));
    ifd0.set(t::ORIENTATION, Value::Short(vec![6]));
    ifd0.set_child(t::EXIF_IFD, exif);
    TiffWriter::new(ByteOrder::Little, false).write(&[ifd0]).unwrap()
}

/// Issues #693, #2284: a HEIC's EXIF reaches the catalog and All Metadata; retain the camera's civil time and offset instead of falling back to the file's
/// modification time. An evening shot must keep its local date when UTC falls on the next day.
#[test]
fn a_heic_is_dated_by_its_exif_and_not_by_the_file() {
    use lcc_heif::testdata::{Spec, build};
    let photo = |x: u32, y: u32| [((x * 7 + y * 13) % 200 + 30) as u16, 110, 150];
    for (date, iso, display) in [
        ("2026:10:18 13:00:00", "2026-10-18T13:00:00-05:00", "October 18, 2026 at 1:00:00 PM"),
        ("2026:10:18 20:15:11", "2026-10-18T20:15:11-05:00", "October 18, 2026 at 8:15:11 PM"),
    ] {
        let src = temp_dir("heic-capture-time");
        let bytes = build(&Spec { exif: Some(heic_camera_exif(date)), ..Spec::new(96, 64, &photo) });
        let path = src.join("IMG_0002.HEIC");
        std::fs::write(&path, &bytes).unwrap();
        let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800);
        std::fs::File::options().write(true).open(&path).unwrap().set_times(std::fs::FileTimes::new().set_modified(modified)).unwrap();
        // Metadata is available in every build; the probe and import require the HEVC decoder.
        assert_eq!(lcc_meta::extract(&bytes).capture_time.map(|d| d.to_iso()).as_deref(), Some(iso));
        let info = crate::files::probe_bytes("IMG_0002.HEIC", &bytes);
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        if lcc_codecs::Format::Heif.can_decode() {
            let info = info.unwrap();
            assert_eq!(info.captured.as_deref(), Some(iso));
            assert_eq!(info.meta.camera, "Apple iPhone 15 Pro");
            assert_eq!((ids(&r, "imported"), ids(&r, "failed")), (1, 0), "{r}");
            let p = s.catalog.photos().next().unwrap();
            assert_eq!(p.captured.as_deref(), Some(iso), "the time the camera wrote, not the file's");
            assert_eq!(p.meta.camera, "Apple iPhone 15 Pro");
            assert_eq!((p.width, p.height), (96, 64), "Exif alone must not turn a HEIC");
            assert_eq!(lcc_catalog::dates::display_time(p.date()), display);
            // issue #693: a HEIC's EXIF reaches All Metadata too, as a JPEG's does
            let heic_id = p.id.0;
            let all = s.execute("photo.allMetadata", &json!({"id": heic_id})).unwrap();
            assert!(all["exif"].as_array().unwrap().iter().any(|row| row["name"] == "Model" && row["value"] == "iPhone 15 Pro"), "{all}");
        } else {
            assert!(info.unwrap_err().contains("isn't included in this build"));
            assert_eq!((ids(&r, "imported"), ids(&r, "failed")), (0, 1), "{r}");
            assert!(r["failed"].to_string().contains("isn't included in this build"), "{r}");
            assert_eq!(s.catalog.photos().count(), 0);
        }
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// HEIF applies its container transform once. Adding Exif Orientation 6 must change neither the
/// probe's dimensions nor the loaded pixels, with or without a container rotation.
#[test]
fn heic_exif_orientation_does_not_rotate_the_picture_twice() {
    use lcc_heif::testdata::{Prop, Spec, build};
    let photo = |x: u32, y: u32| [((x * 7 + y * 13) % 200 + 30) as u16, 110, 150];
    for (props, size) in [(vec![], (96, 64)), (vec![Prop::Irot(3)], (64, 96))] {
        let plain = build(&Spec { props: props.clone(), ..Spec::new(96, 64, &photo) });
        let bytes = build(&Spec { props, exif: Some(heic_camera_exif("2026:10:18 13:00:00")), ..Spec::new(96, 64, &photo) });
        assert_eq!(lcc_meta::extract(&bytes).orientation, Some(lcc_geom::Orientation::Rotate90));
        if lcc_codecs::Format::Heif.can_decode() {
            let header = lcc_codecs::read_header(&bytes).unwrap();
            assert_eq!((header.width, header.height, header.orientation), (size.0, size.1, 1));
            let info = crate::files::probe_bytes("rotated.HEIC", &bytes).unwrap();
            assert_eq!((info.width, info.height), size);
            let (image, _) = crate::files::load_bytes(&bytes, 96).unwrap();
            let (without_exif, _) = crate::files::load_bytes(&plain, 96).unwrap();
            assert_eq!((image.width, image.height), (size.0 as usize, size.1 as usize));
            assert_eq!(image.data, without_exif.data, "Exif must not turn the decoded pixels again");
        } else {
            assert!(crate::files::probe_bytes("rotated.HEIC", &bytes).unwrap_err().contains("isn't included in this build"));
            assert!(crate::files::load_bytes(&bytes, 96).unwrap_err().contains("isn't included in this build"));
        }
    }
}

/// Issue #705: Nikon (and most cameras) write `.JPG` and `.NEF`, and a user may rename to `.Jpg`.
/// The folder scan, `library.browse` and the sidecar lookup match extensions in any letter
/// case. (The file dialog's filters, the one place that was case-sensitive on Linux before
/// #342, are checked in `apps/app/src/dialog_filter.rs`.)
#[test]
fn upper_and_mixed_case_extensions_are_found_by_import_browse_and_sidecar_lookup() {
    use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let src = temp_dir("upper-exts");
    let jpeg = |seed: u8| {
        let px: Vec<u8> = (0..40 * 30).flat_map(|i| [(i % 251) as u8, seed, 200]).collect();
        encode_jpeg(&EncodeImage::new(40, 30, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap()
    };
    std::fs::write(src.join("a.JPG"), jpeg(1)).unwrap();
    std::fs::write(src.join("b.Jpg"), jpeg(2)).unwrap();
    std::fs::write(src.join("c.JPEG"), jpeg(3)).unwrap();
    write_png(&src.join("d.PNG"), 4);
    // a raw container the app shows by its embedded JPEG (a TIFF whose IFD0 points at one)
    let nef = {
        let j = jpeg(5);
        let off = 8 + 2 + 24 + 4;
        let mut f = b"II*\0\x08\0\0\0\x02\0".to_vec();
        for (tag, v) in [(513u16, off as u32), (514, j.len() as u32)] {
            f.extend_from_slice(&tag.to_le_bytes());
            f.extend_from_slice(&[4, 0, 1, 0, 0, 0]);
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&[0, 0, 0, 0]);
        f.extend_from_slice(&j);
        f
    };
    std::fs::write(src.join("e.NEF"), nef).unwrap();
    std::fs::write(src.join("notes.TXT"), "not a photo").unwrap();
    // sidecars: stem-named with an upper-case extension, and full-named beside a mixed-case photo
    let sidecar = |rating: u8| {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{rating}"/></rdf:RDF></x:xmpmeta>"#
        )
    };
    std::fs::write(src.join("a.XMP"), sidecar(4)).unwrap();
    std::fs::write(src.join("b.Jpg.xmp"), sidecar(2)).unwrap();

    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(r["scanned"], 5, "{r}"); // the five photos; notes.TXT is not one
    assert_eq!(ids(&r, "imported"), 5, "{r}");
    assert_eq!(ids(&r, "failed"), 0, "{r}");
    assert_eq!(r["sidecars"], 2, "{r}");
    let names: Vec<(String, String, u8)> = s.catalog.photos().map(|p| (p.file_name.clone(), p.format.clone(), p.rating)).collect();
    let by_name = |n: &str| names.iter().find(|(f, _, _)| f == n).cloned().unwrap_or_else(|| panic!("{n} missing from {names:?}"));
    assert_eq!(by_name("a.JPG").2, 4, "a.XMP read");
    assert_eq!(by_name("b.Jpg").2, 2, "b.Jpg.xmp read");
    assert_eq!(by_name("c.JPEG").1, "JPEG");
    assert_eq!(by_name("d.PNG").1, "PNG");
    let nef = s.catalog.photos().find(|p| p.file_name == "e.NEF").unwrap();
    assert!(nef.kind == lcc_catalog::MediaKind::Raw && nef.preview_only.is_some(), "{nef:?}");

    // Browse Folder (the Local section) lists the same five, without adding them to the library
    let mut b = Session::new().with_fs();
    let r = b.execute("library.browse", &json!({"path": src.to_string_lossy()})).unwrap();
    assert_eq!(r["photos"], 5, "{r}");
    assert_eq!(r["new"], 5, "{r}");
    assert_eq!(r["failed"], 0, "{r}");
    let visible = b.visible_cloned();
    let shown: Vec<String> = visible.iter().filter_map(|id| b.catalog.photo(*id).map(|p| p.file_name.clone())).collect();
    for n in ["a.JPG", "b.Jpg", "c.JPEG", "d.PNG", "e.NEF"] {
        assert!(shown.iter().any(|f| f == n), "{n} not shown: {shown:?}");
    }
    let _ = std::fs::remove_dir_all(&src);
}

/// A raw beside a sidecar as Lightroom Classic / Camera Raw write it (xpacket wrapper, one
/// `rdf:Description` holding attributes and child elements): rating, label, crop and develop
/// sliders all arrive on import, in Add and in Copy mode (issue #43).
#[test]
fn raw_with_lightroom_sidecar_imports_rating_label_crop_and_develop() {
    use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let src = temp_dir("lr-sidecar");
    let nef = {
        let px: Vec<u8> = (0..60 * 40).flat_map(|i| [(i % 251) as u8, 90, 200]).collect();
        let j = encode_jpeg(&EncodeImage::new(60, 40, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        let off = 8 + 2 + 24 + 4;
        let mut f = b"II*\0\x08\0\0\0\x02\0".to_vec();
        for (tag, v) in [(513u16, off as u32), (514, j.len() as u32)] {
            f.extend_from_slice(&tag.to_le_bytes());
            f.extend_from_slice(&[4, 0, 1, 0, 0, 0]);
            f.extend_from_slice(&v.to_le_bytes());
        }
        f.extend_from_slice(&[0, 0, 0, 0]);
        f.extend_from_slice(&j);
        f
    };
    std::fs::write(src.join("DSC_0001.NEF"), nef).unwrap();
    let xmp = r#"<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 7.0-c000 1.000000, 0000/00/00-00:00:00        ">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
    xmlns:exif="http://ns.adobe.com/exif/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   xmp:ModifyDate="2026-09-01T10:00:00+10:00"
   xmp:Rating="4"
   xmp:Label="Red"
   tiff:Make="NIKON CORPORATION"
   crs:Version="17.0"
   crs:ProcessVersion="15.4"
   crs:WhiteBalance="As Shot"
   crs:Exposure2012="+0.75"
   crs:Contrast2012="+20"
   crs:Shadows2012="+30"
   crs:HasSettings="True"
   crs:CropTop="0.1"
   crs:CropLeft="0.2"
   crs:CropBottom="0.9"
   crs:CropRight="0.8"
   crs:CropAngle="2"
   crs:CropConstrainToWarp="0"
   crs:HasCrop="True"
   crs:AlreadyApplied="False">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>beach</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <crs:ToneCurvePV2012>
    <rdf:Seq>
     <rdf:li>0, 0</rdf:li>
     <rdf:li>255, 255</rdf:li>
    </rdf:Seq>
   </crs:ToneCurvePV2012>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;
    std::fs::write(src.join("DSC_0001.xmp"), xmp).unwrap();
    let dest = temp_dir("lr-sidecar-dest");
    for args in
        [json!({"paths": [src.to_string_lossy()]}), json!({"paths": [src.to_string_lossy()], "mode": "copy", "destination": dest.to_string_lossy()})]
    {
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &args).unwrap();
        assert_eq!(ids(&r, "imported"), 1, "{r}");
        assert_eq!(r["sidecars"], 1, "{r}");
        let p = s.catalog.photos().next().unwrap();
        assert_eq!(p.rating, 4, "{args}");
        assert_eq!(p.label, Some(lcc_catalog::ColorLabel::Red), "{args}");
        assert!(p.meta.keywords.iter().any(|k| k == "beach"));
        let d = p.develop.to_json();
        assert_eq!(d["light"]["exposure"], 0.75, "{d}");
        assert_eq!(d["light"]["contrast"], 20.0, "{d}");
        // Lightroom's corners (.2, .1) and (.8, .9) on the stored 3:2 image at a 2° straighten: the app's rect about the
        // same centre, its sides those of the turned rectangle, the angle the other way (`catalog_crop`)
        let c = &d["crop"]["geometry"];
        let r = |k: &str| c["rect"][k].as_f64().unwrap();
        assert!(((r("x0") + r("x1")) / 2.0 - 0.5).abs() < 1e-6 && ((r("y0") + r("y1")) / 2.0 - 0.5).abs() < 1e-6, "{c}");
        let (t, dx, dy) = (2f64.to_radians(), 0.6 * 1.5, 0.8);
        let want = (dx * t.cos() + dy * t.sin()) / (dy * t.cos() - dx * t.sin());
        assert!(((r("x1") - r("x0")) * 1.5 / (r("y1") - r("y0")) - want).abs() < 1e-6, "{c}");
        assert!((c["angle"].as_f64().unwrap() + 2.0).abs() < 1e-6, "{c}");
    }
    // the user's own later edits survive a folder sync: the sidecar is older than them, so it is
    // not read again over them
    for (args, folder) in [
        (json!({"paths": [src.to_string_lossy()]}), src.clone()),
        (json!({"paths": [src.to_string_lossy()], "mode": "copy", "destination": dest.to_string_lossy(), "organize": "flat"}), dest.clone()),
    ] {
        let _ = std::fs::remove_dir_all(&dest);
        let mut s = Session::new().with_fs().with_system_clock();
        s.execute("library.import", &args).unwrap();
        let id = s.catalog.photos().next().unwrap().id;
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        s.execute("photo.rate", &json!({"rating": 1})).unwrap();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": -1.0})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().rating, 1);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        s.execute("folder.synchronize", &json!({"path": folder.to_string_lossy(), "readMetadata": true})).unwrap();
        let p = s.catalog.photo(id).unwrap();
        assert_eq!(p.rating, 1, "a re-read must not undo the user's rating");
        assert_eq!(p.develop.to_json()["light"]["exposure"], -1.0, "nor their develop edits");
    }
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dest);

    // malformed sidecars never panic and never stop the photo from coming in
    for bad in [
        "",
        "<x:xmpmeta",
        "<rdf:RDF><rdf:Description xmp:Rating=\"NaN\" crs:HasCrop=\"True\" crs:CropLeft=\"inf\" crs:CropRight=\"-1e309\"/>",
        "\u{0}\u{feff}<<<>>>",
    ] {
        let dir = temp_dir("bad-sidecar");
        write_png(&dir.join("b.png"), 3);
        std::fs::write(dir.join("b.xmp"), bad).unwrap();
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
        assert!(ids(&r, "imported") >= 1, "{bad:?}: {r}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A raw cut short, as one still being copied in: its TIFF header declares one strip of 4000 bytes at 64, only 100 there.
fn short_raw() -> Vec<u8> {
    let mut b = vec![0u8; 64];
    b[0..4].copy_from_slice(b"II*\0");
    b[4..8].copy_from_slice(&8u32.to_le_bytes());
    b[8..10].copy_from_slice(&2u16.to_le_bytes());
    for (at, tag, v) in [(10usize, 0x0111u16, 64u32), (22, 0x0117, 4000)] {
        b[at..at + 2].copy_from_slice(&tag.to_le_bytes());
        b[at + 2..at + 4].copy_from_slice(&4u16.to_le_bytes());
        b[at + 4..at + 8].copy_from_slice(&1u32.to_le_bytes());
        b[at + 8..at + 12].copy_from_slice(&v.to_le_bytes());
    }
    b.extend([0u8; 100]);
    b
}

/// Lightroom keeps spots and mask shapes on the stored image, before its orientation, as it does the crop; the app
/// keeps them on the oriented photo. A brushed Content-Aware Remove on a raw turned to portrait (8) comes in where
/// Lightroom shows it: (0.69, 0.9) on the stored frame is (0.9, 0.31) on the photo turned a quarter counter-clockwise.
/// Before, it landed on the stored-frame spot of the turned photo (sunset g04: on the stairs, not the camera rig).
#[test]
fn lightroom_spots_and_masks_on_a_turned_raw_land_where_lightroom_shows_them() {
    use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let px: Vec<u8> = (0..60 * 40).flat_map(|i| [(i % 251) as u8, 90, 200]).collect();
    let j = encode_jpeg(&EncodeImage::new(60, 40, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    let off = 8 + 2 + 24 + 4;
    let mut nef = b"II*     ".to_vec();
    for (tag, v) in [(513u16, off as u32), (514, j.len() as u32)] {
        nef.extend_from_slice(&tag.to_le_bytes());
        nef.extend_from_slice(&[4, 0, 1, 0, 0, 0]);
        nef.extend_from_slice(&v.to_le_bytes());
    }
    nef.extend_from_slice(&[0, 0, 0, 0]);
    nef.extend_from_slice(&j);
    let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
    for (exif, spot_at, centre_at, swapped) in [("8", (0.9, 0.31), (0.3, 0.8), true), ("1", (0.69, 0.9), (0.2, 0.3), false)] {
        let src = temp_dir("lr-turned-spots");
        std::fs::write(src.join("DSC_0003.NEF"), &nef).unwrap();
        let xmp = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" tiff:Orientation="{exif}" crs:Version="17.0" crs:ProcessVersion="15.4" crs:HasSettings="True">
 <crs:RetouchAreas><rdf:Seq><rdf:li><rdf:Description crs:SpotType="heal_patchmatch" crs:SourceState="sourceAutoComputed" crs:Opacity="1">
  <crs:Masks><rdf:Seq><rdf:li><rdf:Description crs:What="Mask/Paint" crs:Radius="0.03" crs:MaskValue="1"><crs:Dabs><rdf:Seq><rdf:li>d 0.69 0.9</rdf:li><rdf:li>d 0.7 0.88</rdf:li></rdf:Seq></crs:Dabs></rdf:Description></rdf:li></rdf:Seq></crs:Masks>
 </rdf:Description></rdf:li></rdf:Seq></crs:RetouchAreas>
 <crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1" crs:CorrectionActive="true" crs:LocalExposure2012="0.5">
  <crs:CorrectionMasks><rdf:Seq><rdf:li crs:What="Mask/CircularGradient" crs:Top="0.2" crs:Left="0.1" crs:Bottom="0.4" crs:Right="0.3" crs:Angle="10" crs:Feather="50" crs:Version="2"/></rdf:Seq></crs:CorrectionMasks>
 </rdf:Description></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections>
</rdf:Description></rdf:RDF></x:xmpmeta>"#
        );
        std::fs::write(src.join("DSC_0003.xmp"), xmp).unwrap();
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        assert_eq!(ids(&r, "imported"), 1, "{r}");
        let p = s.catalog.photos().next().unwrap();
        let spot = p.develop.spots.first().expect("the spot came in");
        // Content-Aware Remove comes in as our content-aware fill, not a heal from an automatic source
        assert_eq!(spot.mode, lcc_develop::SpotMode::Fill);
        let at = spot.points.first().expect("its path");
        assert!(near(at.x, spot_at.0) && near(at.y, spot_at.1), "orientation {exif}: spot at {at:?}, want {spot_at:?}");
        let comp = p.develop.masks.first().and_then(|m| m.components.first()).expect("the radial mask came in");
        let lcc_develop::MaskShape::Radial { center, rx, ry, .. } = &comp.shape else { panic!("not radial: {:?}", comp.shape) };
        assert!(near(center.x, centre_at.0) && near(center.y, centre_at.1), "orientation {exif}: radial at {center:?}, want {centre_at:?}");
        // the ellipse was wider than tall on the stored frame (0.2 × 0.2 of a 3:2 image): turned, it is taller
        assert_eq!(rx < ry, swapped, "orientation {exif}: rx {rx}, ry {ry}");
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// A photo turned in Lightroom comes in turned: Lightroom writes the whole orientation as the sidecar's
/// `tiff:Orientation`, and the photo takes what it adds to the file's own (none here, so a half turn for 3).
#[test]
fn a_raw_turned_in_lightroom_imports_turned() {
    use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let px: Vec<u8> = (0..60 * 40).flat_map(|i| [(i % 251) as u8, 90, 200]).collect();
    let j = encode_jpeg(&EncodeImage::new(60, 40, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    let off = 8 + 2 + 24 + 4;
    let mut nef = b"II*\0\x08\0\0\0\x02\0".to_vec();
    for (tag, v) in [(513u16, off as u32), (514, j.len() as u32)] {
        nef.extend_from_slice(&tag.to_le_bytes());
        nef.extend_from_slice(&[4, 0, 1, 0, 0, 0]);
        nef.extend_from_slice(&v.to_le_bytes());
    }
    nef.extend_from_slice(&[0, 0, 0, 0]);
    nef.extend_from_slice(&j);
    for (exif, settings, want) in [
        ("3", true, lcc_geom::Orientation::Rotate180),
        ("8", true, lcc_geom::Orientation::Rotate270),
        ("1", true, lcc_geom::Orientation::Normal),
        // a packet without develop settings (a metadata-only writer) doesn't turn the photo
        ("3", false, lcc_geom::Orientation::Normal),
    ] {
        let src = temp_dir("lr-turned");
        std::fs::write(src.join("DSC_0002.NEF"), &nef).unwrap();
        let crs = if settings { r#"crs:Version="17.0" crs:ProcessVersion="15.4" crs:Exposure2012="+0.50" crs:HasSettings="True""# } else { "" };
        let xmp = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmp:Rating="2" tiff:Orientation="{exif}" {crs}/></rdf:RDF></x:xmpmeta>"#
        );
        std::fs::write(src.join("DSC_0002.xmp"), xmp).unwrap();
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        assert_eq!(ids(&r, "imported"), 1, "{r}");
        let p = s.catalog.photos().next().unwrap();
        assert_eq!(p.rating, 2);
        assert_eq!(p.develop.orientation, want, "tiff:Orientation {exif}, settings {settings}");
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// A straightened crop from a Lightroom sidecar has the shape Lightroom exports: its four edges are two opposite
/// corners of the turned crop on the stored image, as in a catalog. Crop values and the sizes Lightroom Classic
/// exported, from real wedding frames (3:2 bodies; the last one shot upright, turned by the sidecar).
#[test]
fn a_straightened_lightroom_crop_keeps_the_shape_lightroom_exports() {
    use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let px: Vec<u8> = (0..60 * 40).flat_map(|i| [(i % 251) as u8, 90, 200]).collect();
    let j = encode_jpeg(&EncodeImage::new(60, 40, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    let off = 8 + 2 + 24 + 4;
    let mut nef = b"II*\0\x08\0\0\0\x02\0".to_vec();
    for (tag, v) in [(513u16, off as u32), (514, j.len() as u32)] {
        nef.extend_from_slice(&tag.to_le_bytes());
        nef.extend_from_slice(&[4, 0, 1, 0, 0, 0]);
        nef.extend_from_slice(&v.to_le_bytes());
    }
    nef.extend_from_slice(&[0, 0, 0, 0]);
    nef.extend_from_slice(&j);
    // (left, top, right, bottom, angle, sidecar orientation, Lightroom's export width × height)
    for (l, t, r, b, angle, orientation, export) in [
        (0.018036, 0.06095, 0.981964, 0.93905, -2.42, 1, (7687.0, 5127.0)),
        (0.042377, 0.0, 0.957623, 1.0, 2.38, 1, (7718.0, 5148.0)),
        (0.078904, 0.100632, 0.917008, 0.800023, -4.602, 1, (4596.0, 3064.0)),
        (0.026218, 0.09446, 0.973782, 0.90554, -3.98, 8, (4960.0, 7436.0)),
    ] {
        let src = temp_dir("lr-straightened");
        std::fs::write(src.join("DSC_0003.NEF"), &nef).unwrap();
        let xmp = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" tiff:Orientation="{orientation}" crs:Version="17.0" crs:ProcessVersion="15.4" crs:HasSettings="True" crs:HasCrop="True" crs:CropLeft="{l}" crs:CropTop="{t}" crs:CropRight="{r}" crs:CropBottom="{b}" crs:CropAngle="{angle}"/></rdf:RDF></x:xmpmeta>"#
        );
        std::fs::write(src.join("DSC_0003.xmp"), xmp).unwrap();
        let mut s = Session::new().with_fs();
        let res = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        assert_eq!(ids(&res, "imported"), 1, "{res}");
        let d = &s.catalog.photos().next().unwrap().develop;
        let g = d.crop.geometry;
        // the crop's width / height on the photo as shown (turned by its orientation)
        let (ow, oh) = if d.orientation.swaps_axes() { (40.0, 60.0) } else { (60.0, 40.0) };
        let aspect = g.rect.width() * ow / (g.rect.height() * oh);
        let want = export.0 / export.1;
        assert!((aspect - want).abs() / want < 0.002, "angle {angle}: aspect {aspect:.4}, Lightroom exported {want:.4}");
        assert!((g.angle + angle).abs() < 1e-9, "{g:?}");
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// Sidecars are found and read whatever their name's case, header or byte-order mark, so a
/// raw's rating and crop never silently stay behind while the file's own IPTC comes in (issue #43).
#[test]
fn raw_sidecar_rating_and_crop_survive_naming_and_encoding_variants() {
    use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let px: Vec<u8> = (0..60 * 40).flat_map(|i| [(i % 251) as u8, 90, 200]).collect();
    let j = encode_jpeg(&EncodeImage::new(60, 40, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    let mut nef = b"II*\0\x08\0\0\0\x02\0".to_vec();
    for (tag, v) in [(513u16, 38u32), (514, j.len() as u32)] {
        nef.extend_from_slice(&tag.to_le_bytes());
        nef.extend_from_slice(&[4, 0, 1, 0, 0, 0]);
        nef.extend_from_slice(&v.to_le_bytes());
    }
    nef.extend_from_slice(&[0, 0, 0, 0]);
    nef.extend_from_slice(&j);
    let body = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmp:Rating="3" crs:HasCrop="True" crs:CropLeft="0.2" crs:CropTop="0.1" crs:CropRight="0.8" crs:CropBottom="0.9" crs:CropAngle="1.5"/></rdf:RDF></x:xmpmeta>"#;
    let cases = [
        ("darktable-full", "DSC_0001.NEF.xmp", body.to_string()),
        ("lower-full", "DSC_0001.nef.xmp", body.to_string()),
        ("upper-full", "DSC_0001.NEF.XMP", body.to_string()),
        ("mixed-stem", "DSC_0001.Xmp", body.to_string()),
        ("bom-and-decl", "DSC_0001.xmp", format!("\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- written by another app -->\n{body}")),
    ];
    for (name, file, text) in cases {
        let src = temp_dir(&format!("sc-variant-{name}"));
        std::fs::write(src.join("DSC_0001.NEF"), &nef).unwrap();
        std::fs::write(src.join(file), text).unwrap();
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        assert_eq!((ids(&r, "imported"), r["sidecars"].as_u64()), (1, Some(1)), "{name}: {r}");
        let p = s.catalog.photos().next().unwrap();
        assert_eq!(p.rating, 3, "{name}");
        let c = p.develop.to_json()["crop"]["geometry"].clone();
        // (Lightroom's 1.5° straighten turns the app's crop the other way: `catalog_crop`)
        assert!((c["angle"].as_f64().unwrap_or(0.0) + 1.5).abs() < 1e-6, "{name}: {c}");
        assert!((c["rect"]["x0"].as_f64().unwrap_or(0.0) - 0.2).abs() < 0.05, "{name}: {c}");
        if name == "lower-full" {
            // a move takes that sidecar along
            let dest = temp_dir("sc-variant-dest");
            let mut s = Session::new().with_fs();
            let args = json!({"paths": [src.to_string_lossy()], "mode": "move", "destination": dest.to_string_lossy(), "organize": "flat"});
            let r = s.execute("library.import", &args).unwrap();
            assert_eq!(s.catalog.photos().next().unwrap().rating, 3, "{r}");
            assert!(std::fs::read_dir(&dest).unwrap().flatten().any(|e| e.file_name().to_string_lossy().to_lowercase().ends_with(".xmp")), "{r}");
            assert!(!src.join(file).exists(), "no sidecar left behind: {r}");
            let _ = std::fs::remove_dir_all(&dest);
        }
        let _ = std::fs::remove_dir_all(&src);
    }
}

/// Issue #1001: a Lightroom preset that names a profile applies it once the user has imported a
/// profile of that name (before or after the preset), maps the basic Adobe profiles to ours, and
/// reports the ones it can't use.
#[test]
fn presets_apply_the_imported_profile_they_name() {
    let src = temp_dir("lookref");
    let preset = |file: &str, name: &str, look: &str| {
        let x = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
             <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
               crs:PresetType="Normal" crs:Name="{name}" crs:UUID="{file}" crs:Exposure2012="0" crs:GrainAmount="20">
              <crs:Look><rdf:Description crs:Name="{look}" crs:Amount="0.6" crs:UUID="1234ABCD">
               <crs:Group><rdf:Alt><rdf:li xml:lang="x-default">Film Pack</rdf:li></rdf:Alt></crs:Group>
              </rdf:Description></crs:Look>
             </rdf:Description></rdf:RDF></x:xmpmeta>"#
        );
        std::fs::write(src.join(format!("{file}.xmp")), x).unwrap();
    };
    preset("a1", "Portra Soft", "Portra 400");
    preset("a2", "Mono", "Adobe Monochrome");
    preset("a3", "Artsy", "Artistic 01");
    let mut s = Session::with_demo();
    let r = s.execute("preset.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let by_name = |r: &serde_json::Value, n: &str| r["imported"].as_array().unwrap().iter().find(|i| i["name"] == n).cloned().unwrap();
    assert_eq!(by_name(&r, "Portra Soft")["unmapped"], json!(["Profile “Portra 400” (not imported yet)"]), "{r}");
    assert_eq!(by_name(&r, "Mono")["profile"], json!({"name": "Adobe Monochrome", "id": "lc.mono"}));
    assert_eq!(by_name(&r, "Artsy")["unmapped"], json!(["Profile “Artistic 01” (Lightroom's own)"]));
    let photo = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [photo.0]})).unwrap();
    let pid = |s: &Session, n: &str| s.presets.iter().find(|p| p.name == n).unwrap().id.clone();
    let apply = |s: &mut Session, n: &str| {
        let id = pid(s, n);
        s.execute("preset.apply", &json!({"id": id})).unwrap();
        s.develop_of(photo).unwrap()
    };
    // not imported yet: only grain comes across, the photo keeps its profile
    let d = apply(&mut s, "Portra Soft");
    assert_eq!((d.profile.id.as_str(), d.grain.amount), ("lc.color", 20.0));
    // the basic Adobe profiles are ours
    assert_eq!(apply(&mut s, "Mono").profile.id, "lc.mono");
    s.execute("develop.profile", &json!({"id": "lc.color", "amount": 100})).unwrap();
    assert_eq!(apply(&mut s, "Artsy").profile.id, "lc.color", "Lightroom's own creative profiles aren't ours");
    // importing the profile afterwards makes the preset use it, at the preset's amount
    let mut cube = String::from("TITLE \"Portra 400\"\nLUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                cube.push_str(&format!("{r} {g} {b}\n"));
            }
        }
    }
    std::fs::create_dir_all(src.join("profiles/Film Pack")).unwrap();
    std::fs::write(src.join("profiles/Film Pack/portra.cube"), &cube).unwrap();
    let r = s.execute("profile.import", &json!({"paths": [src.join("profiles").to_string_lossy()]})).unwrap();
    let lut = r["imported"][0]["id"].as_str().unwrap().to_string();
    let d = apply(&mut s, "Portra Soft");
    assert_eq!((d.profile.id.as_str(), d.profile.amount, d.grain.amount), (lut.as_str(), 60.0, 20.0));
    // a preset imported after the profile resolves straight away
    preset("a4", "Portra Hard", "portra  400");
    let r = s.execute("preset.import", &json!({"paths": [src.join("a4.xmp").to_string_lossy()]})).unwrap();
    assert_eq!((r["imported"][0]["profile"]["id"].as_str(), r["imported"][0]["unmapped"].clone()), (Some(lut.as_str()), json!([])), "{r}");
    // deleting the profile leaves the photo's own profile alone again
    s.execute("profile.deleteImported", &json!({"id": lut})).unwrap();
    s.execute("develop.profile", &json!({"id": "lc.vivid", "amount": 100})).unwrap();
    assert_eq!(apply(&mut s, "Portra Soft").profile.id, "lc.vivid");
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn import_preserves_embedded_rating_and_label() {
    use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let src = temp_dir("embedded-rating-test");
    let px = [128u8; 40 * 30 * 3];
    let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="5" xmp:Label="Blue"/></rdf:RDF></x:xmpmeta>"#;
    let meta = EncodeMeta { xmp: Some(xmp), ..Default::default() };
    let bytes = encode_jpeg(&EncodeImage::new(40, 30, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &meta).unwrap();
    std::fs::write(src.join("photo.jpg"), &bytes).unwrap();

    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.join("photo.jpg").to_string_lossy()]})).unwrap();
    assert_eq!(ids(&r, "imported"), 1);
    let p = s.catalog.photos().next().unwrap();
    assert_eq!(p.rating, 5);
    assert_eq!(p.label, Some(lcc_catalog::ColorLabel::Blue));

    let _ = std::fs::remove_dir_all(&src);
}

/// A raw container the app shows by its embedded JPEG: a TIFF whose IFD0 points at one.
fn write_raw(path: &Path) {
    use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let px: Vec<u8> = (0..48 * 32).flat_map(|i| [(i % 251) as u8, 91, 201]).collect();
    let j = encode_jpeg(&EncodeImage::new(48, 32, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    let off = 8 + 2 + 24 + 4;
    let mut f = b"II*\0\x08\0\0\0\x02\0".to_vec();
    for (tag, v) in [(513u16, off as u32), (514, j.len() as u32)] {
        f.extend_from_slice(&tag.to_le_bytes());
        f.extend_from_slice(&[4, 0, 1, 0, 0, 0]);
        f.extend_from_slice(&v.to_le_bytes());
    }
    f.extend_from_slice(&[0, 0, 0, 0]);
    f.extend_from_slice(&j);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, f).unwrap();
}

/// An edited TIFF of a raw: the same name, a TIFF (not a raw) to the app.
fn write_tiff(path: &Path, seed: u8) {
    use lcc_codecs::{EncodeImage, EncodeMeta, Samples, TiffCompression, encode_tiff};
    let (w, h) = (48usize, 32usize);
    let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed]).collect();
    let bytes = encode_tiff(&EncodeImage::new(w as u32, h as u32, 3, Samples::U8(&px)), TiffCompression::Deflate, &EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Issue #1905 (the import side of #1523): the stem sidecar `IMG_1.xmp` belongs to the raw first,
/// so a TIFF or JPEG arriving beside a raw of the same name reads its own `IMG_1.TIF.xmp`, never
/// the raw's packet, with or without the raw. Before, the arriving file took the raw's rating,
/// label, metadata and develop settings.
#[test]
fn a_file_arriving_beside_its_raws_stem_reads_its_own_sidecar() {
    let sidecar = |rating: u8| {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{rating}"/></rdf:RDF></x:xmpmeta>"#
        )
    };
    let read = |s: &Session, name: &str| -> u8 { s.catalog.photos().find(|p| p.file_name == name).map(|p| p.rating).unwrap_or_default() };

    // the raw and the edited TIFF of it, same name, in one import
    let src = temp_dir("stem-pair");
    write_raw(&src.join("IMG_1.NEF"));
    write_tiff(&src.join("IMG_1.TIF"), 7);
    std::fs::write(src.join("IMG_1.xmp"), sidecar(5)).unwrap();
    std::fs::write(src.join("IMG_1.TIF.xmp"), sidecar(2)).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!((ids(&r, "imported"), ids(&r, "failed")), (2, 0), "{r}");
    assert_eq!(r["sidecars"], 2, "{r}");
    assert_eq!(read(&s, "IMG_1.NEF"), 5, "the raw reads the stem sidecar, which is its own");
    assert_eq!(read(&s, "IMG_1.TIF"), 2, "the TIFF reads its own sidecar, never the raw's");
    let _ = std::fs::remove_dir_all(&src);

    // the TIFF with no sidecar of its own: nothing of the raw's packet comes across
    let src = temp_dir("stem-none");
    write_raw(&src.join("IMG_2.NEF"));
    write_tiff(&src.join("IMG_2.TIF"), 8);
    std::fs::write(src.join("IMG_2.xmp"), sidecar(4)).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!((ids(&r, "imported"), ids(&r, "failed")), (2, 0), "{r}");
    assert_eq!(r["sidecars"], 1, "{r}");
    assert_eq!(read(&s, "IMG_2.NEF"), 4);
    assert_eq!(read(&s, "IMG_2.TIF"), 0, "the raw's packet is not the TIFF's");
    let _ = std::fs::remove_dir_all(&src);

    // the raw already in the library, the edited TIFF arriving later
    use lcc_catalog::{MediaKind, Op, Photo, Source};
    let src = temp_dir("stem-after");
    write_raw(&src.join("IMG_3.NEF"));
    write_tiff(&src.join("IMG_3.TIF"), 9);
    std::fs::write(src.join("IMG_3.xmp"), sidecar(3)).unwrap();
    let mut s = Session::new().with_fs();
    let raw = {
        let id = s.catalog.alloc_photo_id();
        let path = src.join("IMG_3.NEF").to_string_lossy().into_owned();
        let mut p = Photo::new(id, Source::File { path }, "IMG_3.NEF", "NEF", 48, 32, "2026-01-14T05:58:48");
        p.kind = MediaKind::Raw;
        p
    };
    s.catalog.apply(Op::AddPhoto { photo: Box::new(raw) }).unwrap();
    let r = s.execute("library.import", &json!({"paths": [src.join("IMG_3.TIF").to_string_lossy()]})).unwrap();
    assert_eq!((ids(&r, "imported"), ids(&r, "failed")), (1, 0), "{r}");
    assert_eq!(r["sidecars"], 0, "{r}");
    assert_eq!(read(&s, "IMG_3.TIF"), 0, "the catalogued raw owns IMG_3.xmp");
    let _ = std::fs::remove_dir_all(&src);
}

/// `levels` folders named `d` nested below `root` (created); the innermost one's path.
fn nested(root: &Path, levels: usize) -> std::path::PathBuf {
    let mut dir = root.to_path_buf();
    for _ in 0..levels {
        dir = dir.join("d");
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A scan says that its folder walk stopped at the depth limit (there may be more photos), and
/// does not when the tree ends within it.
#[test]
fn scan_with_reports_a_walk_that_stopped_at_a_bound() {
    use crate::import::{ScanInput, ScanProgress, scan_with};
    let scan = |dir: &Path| {
        let mut s = Session::new().with_fs();
        let (input, paths) = ScanInput::new(&mut s, &[dir.to_string_lossy().to_string()]);
        scan_with(input, &paths, &ScanProgress::default())
    };
    // the deepest folder a walk lists is one level above the limit; one more folder is never entered
    let cut = temp_dir("scan-cut");
    write_png(&cut.join("top.png"), 1);
    write_png(&nested(&cut, crate::walk::MAX_DEPTH - 1).join("last.png"), 2);
    nested(&cut, crate::walk::MAX_DEPTH);
    let out = scan(&cut);
    assert_eq!(out.candidates.len(), 2, "what was reached is still returned");
    assert_eq!((out.truncated, out.unreadable, out.unsupported), (true, 0, 0));

    let whole = temp_dir("scan-whole");
    write_png(&whole.join("top.png"), 1);
    write_png(&nested(&whole, crate::walk::MAX_DEPTH - 1).join("last.png"), 2);
    let out = scan(&whole);
    assert_eq!(out.candidates.len(), 2);
    assert_eq!((out.truncated, out.unreadable, out.unsupported), (false, 0, 0));
    let _ = std::fs::remove_dir_all(&cut);
    let _ = std::fs::remove_dir_all(&whole);
}

/// A folder this process is not allowed to list, until it is dropped (Unix: no permissions;
/// Windows: a deny entry for Everyone on listing it).
struct Unlistable(std::path::PathBuf);

impl Unlistable {
    /// `None` when the folder can still be listed (the super-user ignores permissions) or the
    /// means to deny it is missing: the test then has nothing to check.
    fn new(dir: &Path) -> Option<Unlistable> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).ok()?;
        }
        #[cfg(windows)]
        {
            let denied = std::process::Command::new("icacls").arg(dir).args(["/deny", "*S-1-1-0:(RD)"]).output();
            if !denied.is_ok_and(|o| o.status.success()) {
                return None;
            }
        }
        // from here the folder is restored on drop, whatever the test does
        let guard = Unlistable(dir.to_path_buf());
        std::fs::read_dir(dir).is_err().then_some(guard)
    }
}

impl Drop for Unlistable {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("icacls").arg(&self.0).args(["/remove:d", "*S-1-1-0"]).output();
        }
    }
}

/// A folder the scan can't list is counted — by the scan and by `library.importPreview` — and the
/// photos around it are still found.
#[test]
fn an_unlistable_folder_is_reported_by_the_scan() {
    use crate::import::{ScanInput, ScanProgress, scan_with};
    let src = temp_dir("unlistable");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("locked").join("b.png"), 2);
    let Some(locked) = Unlistable::new(&src.join("locked")) else {
        eprintln!("skipped: this process can still list a folder it was denied");
        let _ = std::fs::remove_dir_all(&src);
        return;
    };
    let mut s = Session::new().with_fs();
    let (input, paths) = ScanInput::new(&mut s, &[src.to_string_lossy().to_string()]);
    let out = scan_with(input, &paths, &ScanProgress::default());
    assert_eq!(out.candidates.len(), 1, "only the photo outside the folder");
    assert_eq!((out.unreadable, out.unsupported, out.truncated), (1, 0, false));
    let r = s.execute("library.importPreview", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!((r["scanned"].clone(), r["skipped"].clone()), (json!(1), json!({"unreadable": 1, "unsupported": 0})), "{r}");
    drop(locked);
    let _ = std::fs::remove_dir_all(&src);
}

/// `library.importPreview` reports the walk's diagnostics next to the keys it always returned, and
/// still leaves its probes for the import that follows.
#[test]
fn import_preview_reports_what_the_scan_skipped() {
    let src = temp_dir("preview-diag");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("sub/b.png"), 2);
    std::fs::write(src.join("notes.txt"), b"not a photo").unwrap();
    std::fs::write(src.join("sub/readme.md"), b"x").unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.importPreview", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!((r["scanned"].clone(), r["duplicates"].clone()), (json!(2), json!(0)), "{r}");
    assert_eq!(ids(&r, "candidates"), 2, "{r}");
    assert_eq!(r["scanLimited"], false, "{r}");
    assert_eq!(r["skipped"], json!({"unreadable": 0, "unsupported": 2}), "notes.txt and readme.md are reported: {r}");
    for f in [src.join("a.png"), src.join("sub").join("b.png")] {
        let key = f.to_string_lossy().to_string();
        assert!(s.import_probes.contains_key(&key), "the probe of {key} is kept for the import: {:?}", s.import_probes.keys());
    }
    // a second preview reuses them: a file that has not changed is not read again
    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (counted, probe) = (reads.clone(), s.media.file_probe.clone().unwrap());
    s.media.file_probe = Some(std::sync::Arc::new(move |path: &str| {
        counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        probe(path)
    }));
    let again = s.execute("library.importPreview", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    assert_eq!(again["scanned"], 2, "{again}");
    assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 0, "the first preview's probes are reused");

    // a file named on its own is tried, not reported as skipped
    let r = s.execute("library.importPreview", &json!({"paths": [src.join("notes.txt").to_string_lossy()]})).unwrap();
    assert_eq!((r["scanned"].clone(), r["skipped"]["unsupported"].clone()), (json!(1), json!(0)), "{r}");

    // a walk that stopped at the depth limit says so
    let deep = temp_dir("preview-deep");
    write_png(&deep.join("top.png"), 3);
    nested(&deep, crate::walk::MAX_DEPTH);
    let r = s.execute("library.importPreview", &json!({"paths": [deep.to_string_lossy()]})).unwrap();
    assert_eq!((r["scanned"].clone(), r["scanLimited"].clone()), (json!(1), json!(true)), "{r}");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&deep);
}

/// Upstream issue #4319: `library.import` says what its folder walk left out, as `library.importPreview` does, so a
/// script that imports a folder directly can tell a file that was skipped from one that is missing. A file named on its
/// own is tried (and fails), never counted as skipped; XMP sidecars aren't counted.
#[test]
fn import_reports_what_the_folder_walk_skipped() {
    let src = temp_dir("import-skipped");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("sub/b.png"), 2);
    std::fs::write(src.join("a.xmp"), b"x").unwrap();
    std::fs::write(src.join("notes.txt"), b"not a photo").unwrap();
    std::fs::write(src.join("sub/readme.md"), b"x").unwrap();
    let mut s = Session::new().with_fs();
    let preview = s.execute("library.importPreview", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "mode": "add"})).unwrap();
    assert_eq!(ids(&r, "imported"), 2, "{r}");
    assert_eq!(r["skipped"], json!({"unreadable": 0, "unsupported": 2}), "notes.txt and readme.md are reported: {r}");
    assert_eq!(r["skipped"], preview["skipped"], "the import and its preview agree");

    let r = s.execute("library.import", &json!({"paths": [src.join("notes.txt").to_string_lossy()], "mode": "add"})).unwrap();
    assert_eq!(r["failed"].as_array().map(Vec::len), Some(1), "a file named on its own is tried: {r}");
    assert_eq!(r["skipped"], json!({"unreadable": 0, "unsupported": 0}), "{r}");
    let _ = std::fs::remove_dir_all(&src);
}

/// XMP sidecars are read with their photos, so they are not "skipped": the expansion, the scan and
/// `library.importPreview` count them (in any letter case, in subfolders too) as nothing, and still
/// count every other file that is not a photo.
#[test]
fn xmp_sidecars_are_not_counted_as_unsupported() {
    use crate::import::{ScanInput, ScanProgress, expand_report, scan_with};
    let mut s = Session::new().with_fs();
    // sidecars beside a photo, a sidecar without one, in any case, one deeper; plus a text file
    let mixed = temp_dir("sidecars-mixed");
    write_png(&mixed.join("a.png"), 1);
    std::fs::write(mixed.join("a.xmp"), b"x").unwrap();
    std::fs::write(mixed.join("b.XMP"), b"x").unwrap();
    std::fs::create_dir_all(mixed.join("sub")).unwrap();
    std::fs::write(mixed.join("sub").join("c.Xmp"), b"x").unwrap();
    std::fs::write(mixed.join("c.txt"), b"x").unwrap();
    // only photos and sidecars
    let tidy = temp_dir("sidecars-only");
    write_png(&tidy.join("a.png"), 2);
    std::fs::write(tidy.join("a.xmp"), b"x").unwrap();
    std::fs::write(tidy.join("b.XMP"), b"x").unwrap();
    for (dir, unsupported) in [(&mixed, 1usize), (&tidy, 0)] {
        let path = dir.to_string_lossy().to_string();
        let e = expand_report(std::slice::from_ref(&path), None, crate::walk::Limits::default());
        assert_eq!((e.files.len(), e.unsupported), (1, unsupported), "expand_report {e:?}");
        let (input, paths) = ScanInput::new(&mut s, std::slice::from_ref(&path));
        let out = scan_with(input, &paths, &ScanProgress::default());
        assert_eq!((out.candidates.len(), out.unsupported), (1, unsupported), "scan_with");
        let r = s.execute("library.importPreview", &json!({"paths": [path]})).unwrap();
        assert_eq!((r["scanned"].clone(), r["skipped"]["unsupported"].clone()), (json!(1), json!(unsupported)), "{r}");
    }
    let _ = std::fs::remove_dir_all(&mixed);
    let _ = std::fs::remove_dir_all(&tidy);
}

/// A batch-edit recipe changes the imported photos' actual develop settings (not just an option
/// label), renders, is one undo step, and can be saved as a preset that stores only what it names.
#[test]
fn a_batch_edit_recipe_changes_imported_photos() {
    let src = temp_dir("recipe");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    let r = s
        .execute(
            "library.import",
            &json!({
                "paths": [src.to_string_lossy()],
                "recipe": [{"id": "light.exposure", "value": 0.5}, {"id": "color.vibrance", "value": 20}],
            }),
        )
        .unwrap();
    assert_eq!(ids(&r, "imported"), 2, "{r}");
    for p in s.catalog.photos() {
        assert_eq!(p.develop.light.exposure, 0.5, "{}", p.file_name);
        assert_eq!(p.develop.color.vibrance, 20.0, "{}", p.file_name);
        assert!(p.history.iter().any(|h| h.label == "Import adjustments"), "{}", p.file_name);
    }
    // the edit really renders (the recipe is applied to the settings, not only recorded)
    let id = s.catalog.photos().next().unwrap().id;
    assert!(s.render_now(id, 32, 32).is_ok());
    // saving the recipe as a preset keeps only the settings it names
    let p = s.execute("preset.saveRecipe", &json!({"name": "Warm Trip", "recipe": [{"id": "light.exposure", "value": 0.5}]})).unwrap();
    let saved = s.presets.iter().find(|x| x.id == p["id"].as_str().unwrap()).unwrap();
    assert_eq!(saved.settings.pointer("/light/exposure"), Some(&json!(0.5)), "{}", saved.settings);
    assert!(saved.settings.get("color").is_none(), "an unnamed group is not stored: {}", saved.settings);
    // one undo step for the whole import
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.len(), 0);
    let _ = std::fs::remove_dir_all(&src);
}

/// A malformed recipe, or one naming a control a recipe may not name, is refused before anything
/// is imported, and by the command that saves one: unknown ids, controls that belong to one photo,
/// geometry and crop controls (the slider refits the crop around them), white balance (a
/// temperature means something else on each kind of file), and lists longer than the control list.
#[test]
fn a_bad_recipe_is_refused() {
    let mut s = Session::new().with_fs();
    let err = s.execute("library.import", &json!({"paths": ["/nonexistent-lc"], "recipe": [{"id": "nope.nope", "value": 1}]})).unwrap_err();
    assert!(format!("{err}").contains("unknown recipe control"), "{err}");
    let err = s.execute("library.import", &json!({"paths": ["/nonexistent-lc"], "recipe": "x"})).unwrap_err();
    assert!(format!("{err}").contains("recipe"), "{err}");
    assert_eq!(s.catalog.len(), 0);
    let too_many: Vec<Value> = (0..lcc_develop::controls::CONTROLS.len() + 1).map(|_| json!({"id": "light.exposure", "value": 1})).collect();
    for (bad, why) in [
        (json!([{"value": 1}]), "needs an `id`"),
        (json!([{"id": "light.exposure"}]), "numeric"),
        (json!([{"id": "light.exposure", "value": "high"}]), "numeric"),
        (json!([7]), "needs an `id`"),
        (json!([{"id": "pointColor.0.hueShift", "value": 5}]), "unknown recipe control"),
        (json!([{"id": "redEye.0.pupilSize", "value": 5}]), "unknown recipe control"),
        (json!([{"id": "crop.angle", "value": 5}]), "geometry"),
        (json!([{"id": "geometry.rotate", "value": 5}]), "geometry"),
        (json!([{"id": "wb.temp", "value": 5200}]), "white balance"),
        (json!([{"id": "wb.tint", "value": 5}]), "white balance"),
        (json!([{"id": "light.exposure", "value": 1}, {"id": "crop.angle", "value": 5}]), "geometry"),
        (json!(too_many), "at most"),
    ] {
        let err = s.execute("library.import", &json!({"paths": ["/nonexistent-lc"], "recipe": bad})).unwrap_err().to_string();
        assert!(err.contains(why), "{why}: {err}");
        let before = s.presets.len();
        let err = s.execute("preset.saveRecipe", &json!({"name": "Bad", "recipe": bad})).unwrap_err().to_string();
        assert!(err.contains(why), "{why}: {err}");
        assert_eq!(s.presets.len(), before, "nothing saved for {why}");
    }
    // the job itself refuses what the command would (a recipe built in code, not from JSON)
    let opts = crate::import::ImportOptions { recipe: vec![("crop.angle".into(), 5.0)], ..Default::default() };
    assert!(crate::import::ImportJob::new(&mut s, opts).is_err());
    // no name, a name that is too long, or nothing in the recipe: no preset
    let one = json!([{"id": "light.exposure", "value": 1}]);
    for params in [json!({"recipe": one}), json!({"name": "  ", "recipe": one}), json!({"name": "x".repeat(101), "recipe": one})] {
        assert!(s.execute("preset.saveRecipe", &params).is_err(), "{params}");
    }
    assert!(s.execute("preset.saveRecipe", &json!({"name": "x".repeat(100), "recipe": one})).is_ok(), "a name of 100 characters is fine");
    assert!(s.execute("preset.saveRecipe", &json!({"name": "Empty", "recipe": []})).is_err());
    assert!(s.execute("preset.saveRecipe", &json!({"name": "Empty"})).is_err());
    assert!(!s.presets.iter().any(|p| p.name == "Empty"));
}

/// The recipe's values go through the control specs like a slider's: past the range they stop at
/// it, for the photo and for the preset. A control named twice counts once, the last value wins.
#[test]
fn recipe_values_are_clamped_like_a_slider() {
    let src = temp_dir("recipe-clamp");
    write_png(&src.join("a.png"), 1);
    let mut s = Session::new().with_fs();
    let recipe = json!([{"id": "light.exposure", "value": 1}, {"id": "light.exposure", "value": 99}, {"id": "light.contrast", "value": -1000}]);
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "recipe": recipe})).unwrap();
    let p = s.catalog.photos().next().unwrap();
    assert_eq!((p.develop.light.exposure, p.develop.light.contrast), (5.0, -100.0));
    let saved = s.execute("preset.saveRecipe", &json!({"name": "Clamped", "recipe": recipe})).unwrap();
    let saved = s.presets.iter().find(|x| x.id == saved["id"].as_str().unwrap()).unwrap();
    assert_eq!(saved.settings.pointer("/light/exposure"), Some(&json!(5.0)), "{}", saved.settings);
    assert_eq!(saved.settings.pointer("/light/contrast"), Some(&json!(-100.0)), "{}", saved.settings);
    let _ = std::fs::remove_dir_all(&src);
}

/// A session whose files probe as DNG raws shot at 4800 K, tint +12 (the pixels stay the PNG's;
/// nothing here renders them), to see what an import does to a raw's as-shot white balance.
fn session_with_raw_probe() -> Session {
    let mut s = Session::new().with_fs();
    let real = s.media.file_probe.clone().unwrap();
    s.media.file_probe = Some(std::sync::Arc::new(move |path: &str| {
        let mut info = real(path)?;
        info.kind = lcc_catalog::MediaKind::Raw;
        info.format = "DNG".into();
        info.as_shot_wb = Some((4800.0, 12.0));
        Ok(info)
    }));
    s
}

fn import_one(s: &mut Session, path: &Path, recipe: Value) -> lcc_catalog::PhotoId {
    let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()], "recipe": recipe})).unwrap();
    lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap_or_else(|| panic!("{path:?} not imported: {r}")))
}

fn white_balance(s: &Session, id: lcc_catalog::PhotoId) -> lcc_develop::WhiteBalance {
    s.catalog.photo(id).unwrap().develop.wb
}

/// A recipe never touches white balance: a raw keeps the as-shot temperature, tint and scale it was
/// imported with, whatever else the recipe sets, and a recipe cannot name a temperature or tint.
#[test]
fn a_recipe_leaves_a_raws_as_shot_white_balance_alone() {
    use lcc_develop::{WbMode, WbScale};
    let src = temp_dir("recipe-wb");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("b.png"), 2);
    let mut s = session_with_raw_probe();
    let plain = import_one(&mut s, &src.join("a.png"), json!([]));
    let w = white_balance(&s, plain);
    assert_eq!((w.mode, w.temp, w.tint, w.scale), (WbMode::AsShot, 4800.0, 12.0, WbScale::Kelvin), "{w:?}");
    let with = import_one(&mut s, &src.join("b.png"), json!([{"id": "light.exposure", "value": 0.5}, {"id": "color.vibrance", "value": 30}]));
    assert_eq!(white_balance(&s, with), w, "the as-shot white balance is as the import left it");
    assert_eq!(s.catalog.photo(with).unwrap().develop.light.exposure, 0.5);
    for id in ["wb.temp", "wb.tint"] {
        let err = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "recipe": [{"id": id, "value": 5200}]})).unwrap_err();
        assert!(err.to_string().contains("white balance"), "{err}");
    }
    let _ = std::fs::remove_dir_all(&src);
}

/// Saved as a preset and applied to a photo as it was imported, a recipe gives the settings
/// importing with it gives (for a raw too), and the preset holds a control the recipe sets to its
/// default: that is a reset, not "nothing named".
#[test]
fn a_recipe_saved_as_a_preset_gives_what_the_recipe_gives() {
    let src = temp_dir("recipe-preset");
    let recipes = [
        json!([{"id": "light.exposure", "value": 0.5}, {"id": "color.vibrance", "value": 20}]),
        json!([{"id": "light.exposure", "value": 99}]),
        json!([{"id": "detail.sharpenAmount", "value": 0}, {"id": "grain.amount", "value": 30}]),
        json!([{"id": "vignette.amount", "value": -25}, {"id": "effects.clarity", "value": 15}, {"id": "mixer.red.hue", "value": 10}]),
    ];
    let mut s = session_with_raw_probe();
    for (i, recipe) in recipes.iter().enumerate() {
        write_png(&src.join(format!("with{i}.png")), 2 * i as u8);
        write_png(&src.join(format!("plain{i}.png")), 2 * i as u8 + 1);
        let with = import_one(&mut s, &src.join(format!("with{i}.png")), recipe.clone());
        let plain = import_one(&mut s, &src.join(format!("plain{i}.png")), json!([]));
        let saved = s.execute("preset.saveRecipe", &json!({"name": format!("Recipe {i}"), "recipe": recipe})).unwrap();
        s.selection = crate::Selection::single(plain);
        s.execute("preset.apply", &json!({"id": saved["id"], "ids": [plain.0]})).unwrap();
        let (with, plain) = (s.catalog.photo(with).unwrap(), s.catalog.photo(plain).unwrap());
        assert_eq!(with.develop, plain.develop, "recipe {recipe}");
    }
    // the sharpening reset (a raw's default is 40) is in the preset although 0 is the control's default
    let reset = s.presets.iter().find(|p| p.name == "Recipe 2").unwrap();
    assert_eq!(reset.settings.pointer("/detail/sharpen_amount"), Some(&json!(0.0)), "{}", reset.settings);
    assert_eq!(s.catalog.photos().find(|p| p.file_name == "with2.png").unwrap().develop.detail.sharpen_amount, 0.0);
    let _ = std::fs::remove_dir_all(&src);
}

/// The recipe applies to the photos an import adds. A file the library has (by path or by
/// content), a trashed one that comes back, and a browsed one that joins keep their own
/// settings; a trashed one imported afresh is a new photo and gets the recipe.
#[test]
fn a_recipe_only_touches_the_photos_an_import_adds() {
    let src = temp_dir("recipe-skipped");
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("b.png"), 2);
    write_png(&src.join("c.png"), 3);
    std::fs::copy(src.join("a.png"), src.join("a-copy.png")).unwrap(); // same bytes as a
    let recipe = json!([{"id": "color.vibrance", "value": 20}]);
    let adjusted = |s: &Session, name: &str| {
        let p = s.catalog.photos().find(|p| p.file_name == name && !p.deleted).unwrap_or_else(|| panic!("no {name}"));
        (p.develop.color.vibrance, p.history.iter().any(|h| h.label == "Import adjustments"))
    };

    let mut s = Session::new().with_fs();
    let first = import_one(&mut s, &src.join("a.png"), json!([]));
    s.selection = crate::Selection::single(first);
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.5})).unwrap();
    let a_before = s.catalog.photo(first).unwrap().develop.clone();

    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "recipe": recipe})).unwrap();
    assert_eq!(ids(&r, "imported"), 2, "{r}"); // b and c
    assert_eq!(ids(&r, "duplicates"), 2, "{r}"); // a (same path) and a-copy (same content)
    assert_eq!(adjusted(&s, "b.png"), (20.0, true));
    assert_eq!(adjusted(&s, "c.png"), (20.0, true));
    assert_eq!(adjusted(&s, "a.png"), (0.0, false), "a duplicate is not edited");
    assert_eq!(s.catalog.photo(first).unwrap().develop, a_before, "nor is the photo it duplicates");
    assert_eq!(s.catalog.len(), 3);

    // in Recently Deleted: restored with its own settings, no recipe
    s.execute("photo.delete", &json!({"ids": [first.0]})).unwrap();
    let r = s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()], "recipe": recipe})).unwrap();
    assert_eq!((ids(&r, "imported"), ids(&r, "restored")), (0, 1), "{r}");
    assert_eq!(s.catalog.photo(first).unwrap().develop, a_before, "a restored photo keeps its edits");
    assert!(!s.catalog.photo(first).unwrap().history.iter().any(|h| h.label == "Import adjustments"));

    // …imported afresh, it is a new photo and gets the recipe
    s.execute("photo.delete", &json!({"ids": [first.0]})).unwrap();
    let r = s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()], "recipe": recipe, "onDeleted": "fresh"})).unwrap();
    assert_eq!(ids(&r, "imported"), 1, "{r}");
    assert_eq!(adjusted(&s, "a.png"), (20.0, true));

    // browsed first (Local), then imported: the record joins the library as it is, as it does for
    // a preset or keywords
    let other = temp_dir("recipe-browsed");
    write_png(&other.join("d.png"), 4);
    s.execute("library.browse", &json!({"path": other.to_string_lossy()})).unwrap();
    let r = s.execute("library.import", &json!({"paths": [other.join("d.png").to_string_lossy()], "recipe": recipe})).unwrap();
    assert_eq!(ids(&r, "imported"), 1, "{r}");
    assert_eq!(adjusted(&s, "d.png"), (0.0, false), "a browsed photo that joins keeps its own settings");
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&other);
}

/// A saved recipe never takes another preset's id: with presets A and B, A deleted, a recipe saved
/// as "B" is its own preset and applying it applies the recipe, not the older B. Saving the same
/// recipe under the same name again is that preset, and the presets are in presets.json.
#[test]
fn saved_recipe_presets_get_distinct_ids_and_survive_reopening() {
    let lib = temp_dir("recipe-presets-lib");
    let src = temp_dir("recipe-presets-src");
    write_png(&src.join("a.png"), 1);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let photo = import_one(&mut s, &src.join("a.png"), json!([]));
    s.selection = crate::Selection::single(photo);
    let create = |s: &mut Session, name: &str| s.execute("preset.create", &json!({"name": name})).unwrap()["id"].as_str().unwrap().to_string();
    let (a, old_b) = (create(&mut s, "A"), create(&mut s, "B"));
    s.execute("preset.delete", &json!({"id": a})).unwrap();
    let recipe = json!([{"id": "light.exposure", "value": 0.7}]);
    let saved = s.execute("preset.saveRecipe", &json!({"name": "B", "group": "User Presets", "recipe": recipe})).unwrap();
    let new_b = saved["id"].as_str().unwrap().to_string();
    assert_ne!(new_b, old_b);
    let mut all: Vec<&str> = s.presets.iter().map(|p| p.id.as_str()).collect();
    let n = all.len();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), n, "every preset has its own id");
    s.execute("preset.apply", &json!({"id": new_b})).unwrap();
    assert_eq!(s.catalog.photo(photo).unwrap().develop.light.exposure, 0.7, "the saved recipe is what gets applied");
    // the same recipe, name and group: the preset it already is
    let again = s.execute("preset.saveRecipe", &json!({"name": "B", "group": "User Presets", "recipe": recipe})).unwrap();
    assert_eq!(again["id"], saved["id"]);
    assert_eq!(s.presets.iter().filter(|p| p.name == "B").count(), 2);
    drop(s);

    let mut t = Session::new().with_fs();
    t.open_library(&lib, false).unwrap();
    let mut bs: Vec<&str> = t.presets.iter().filter(|p| p.name == "B").map(|p| p.id.as_str()).collect();
    bs.sort();
    let mut expected = [old_b.as_str(), new_b.as_str()];
    expected.sort();
    assert_eq!(bs, expected, "both are still there after reopening");
    assert!(!t.presets.iter().any(|p| p.name == "A"), "the deleted one stays deleted");
    drop(t);
    let _ = std::fs::remove_dir_all(&lib);
    let _ = std::fs::remove_dir_all(&src);
}

/// Constrain Crop is on for new photos (item 11 of the evening retest), without counting as an
/// edit or hiding the camera preview; Settings ▸ Import can turn that off; a Lightroom sidecar
/// keeps the value it always read (off), and photos already in the library keep theirs.
#[test]
fn new_photos_start_with_constrain_crop_on() {
    let src = temp_dir("constrain");
    write_png(&src.join("new.png"), 1);
    write_png(&src.join("lr.png"), 2);
    std::fs::write(
        src.join("lr.xmp"),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0.5" crs:HasCrop="True" crs:CropLeft="0.1" crs:CropTop="0.1" crs:CropRight="0.9" crs:CropBottom="0.9" crs:CropAngle="3"/></rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [src.to_string_lossy()], "mode": "add"})).unwrap();
    assert_eq!(ids(&r, "imported"), 2, "{r}");
    let by_name = |s: &Session, n: &str| s.catalog.photos().find(|p| p.file_name == n).cloned().unwrap();
    let new = by_name(&s, "new.png");
    assert!(new.develop.geometry.constrain_crop, "a new photo starts constrained");
    assert!(!new.is_edited(), "which is not an edit");
    assert!(crate::import::has_import_look(&new), "the camera preview still stands in for it");
    let lr = by_name(&s, "lr.png");
    assert!(!lr.develop.geometry.constrain_crop, "a Lightroom sidecar without the flag: off, as it always read");
    // (Lightroom's 3° straighten turns the app's crop the other way: `catalog_crop`)
    assert!((lr.develop.crop.geometry.angle + 3.0).abs() < 1e-9, "its crop came along");
    // the preference off: new photos start unconstrained
    let p = s.execute("library.preferences", &json!({"import": {"constrainCrop": false}})).unwrap();
    assert_eq!(p["import"]["cropUnconstrained"], true, "{p}");
    write_png(&src.join("later.png"), 3);
    s.execute("library.import", &json!({"paths": [src.join("later.png").to_string_lossy()], "mode": "add"})).unwrap();
    assert!(!by_name(&s, "later.png").develop.geometry.constrain_crop);
    // …and photos already in the library keep theirs either way
    assert!(by_name(&s, "new.png").develop.geometry.constrain_crop);
    s.execute("library.preferences", &json!({"import": {"constrainCrop": true}})).unwrap();
    assert!(!by_name(&s, "later.png").develop.geometry.constrain_crop);
    let _ = std::fs::remove_dir_all(&src);
}

/// The Before view starts from the photo as it came into the library (with the import's
/// adjustments), and Set Before is kept with the photo when the library is opened again. A journal
/// record of a hostile Set Before is refused, never a panic.
#[test]
fn before_is_the_import_state_and_set_before_is_kept() {
    let root = temp_dir("before");
    let (src, lib) = (root.join("src"), root.join("lib"));
    write_png(&src.join("a.png"), 1);
    write_png(&src.join("b.png"), 2);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [src.join("a.png").to_string_lossy()]})).unwrap();
    let a = s.catalog.photos().next().unwrap().id;
    s.selection = crate::Selection { ids: vec![a], active: Some(a) };
    s.execute("library.import", &json!({"paths": [src.join("b.png").to_string_lossy()], "recipe": [{"id": "light.exposure", "value": 0.7}]}))
        .unwrap();
    let b = s.catalog.photos().find(|p| p.id != a).unwrap().id;
    s.selection = crate::Selection { ids: vec![b], active: Some(b) };
    let before = |s: &Session| s.before_settings(s.catalog.photo(s.active().unwrap()).unwrap()).light.exposure;
    assert_eq!(before(&s), 0.7, "the before is the photo as imported, with the import preset");
    // an edit, then Set Before: kept with the photo
    s.execute("develop.set", &json!({"values": {"light.exposure": 1.2}})).unwrap();
    s.execute("beforeAfter.copyAfterToBefore", &json!({})).unwrap();
    s.execute("develop.set", &json!({"values": {"light.exposure": -0.4}})).unwrap();
    assert_eq!(before(&s), 1.2);
    s.close_library().unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.selection = crate::Selection { ids: vec![b], active: Some(b) };
    assert_eq!(before(&s), 1.2, "Set Before survives reopening the library");
    // back to the import state, also undoable
    s.execute("beforeAfter.resetBefore", &json!({})).unwrap();
    assert_eq!(before(&s), 0.7);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(before(&s), 1.2);
    for bad in [
        json!({"op": "setBefore", "id": "x"}),
        json!({"op": "setBefore", "id": b.0, "before": 7}),
        json!({"op": "setBefore", "id": b.0, "before": {"light": {"exposure": "hot"}}}),
    ] {
        assert!(serde_json::from_value::<lcc_catalog::Op>(bad.clone()).is_err(), "{bad}");
    }
    // a Set Before for a photo that isn't there is an error
    let missing = lcc_catalog::Op::SetBefore { id: lcc_catalog::PhotoId(u64::MAX), before: None };
    assert!(s.commit("Set Before", missing).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

/// Reloading a relinked virtual copy reads its own file and leaves the photos on other files unchanged; reloading
/// the original leaves the relinked copy unchanged.
#[test]
fn reload_of_a_relinked_virtual_copy_touches_only_the_photos_on_that_file() {
    for from_copy in [true, false] {
        reload_relinked_virtual_copy(from_copy);
    }
}

/// Reloading a copy refreshes its family and keeps the explicit metadata target; selected copies all get the
/// metadata after the plan is deduplicated to one probe per file.
#[test]
fn reload_of_a_copy_refreshes_its_family_and_fills_the_metadata_targets() {
    for select_sibling in [false, true] {
        reload_copy_family_metadata(select_sibling);
    }
}

fn reload_copy_family_metadata(select_sibling: bool) {
    use crate::cmd::reload;
    let dir = temp_dir(&format!("reload-copy-family-{select_sibling}"));
    let path = dir.join("original.png");
    write_png(&path, 10);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let original = s.active().unwrap();
    let r = s.execute("photo.virtualCopy", &json!({"ids": [original.0]})).unwrap();
    let copy = lcc_catalog::PhotoId(r["ids"][0].as_u64().unwrap());
    let r = s.execute("photo.virtualCopy", &json!({"ids": [original.0]})).unwrap();
    let sibling = lcc_catalog::PhotoId(r["ids"][0].as_u64().unwrap());
    s.execute("develop.set", &json!({"ids": [copy.0], "control": "light.exposure", "value": 0.7})).unwrap();
    let before: Vec<_> = [original, copy, sibling].map(|id| s.catalog.photo(id).unwrap().clone()).into();
    let old_items = reload::plan(&mut s, &json!({"ids": [original.0]})).unwrap();
    let old_info = reload::file_probe(&s).unwrap()(&path.to_string_lossy()).unwrap();
    write_png(&path, 200);
    let targets = if select_sibling { vec![copy.0, sibling.0] } else { vec![copy.0] };
    let items = reload::plan(&mut s, &json!({"ids": targets})).unwrap();
    assert_eq!(items.len(), 1, "one probe per shared file");
    assert_eq!(items[0].id, copy, "keep the explicit metadata target");
    let mut info = reload::file_probe(&s).unwrap()(&path.to_string_lossy()).unwrap();
    info.meta.camera = "new camera metadata".into();
    let changed = reload::apply(&mut s, items, vec![Ok(info)]).unwrap();
    assert_eq!(changed.len(), 3);
    for old in &before {
        let new = s.catalog.photo(old.id).unwrap();
        assert_ne!(new.content_hash, old.content_hash);
        assert_eq!(new.develop, old.develop);
        assert_eq!(new.meta.camera, if old.id == copy || (select_sibling && old.id == sibling) { "new camera metadata" } else { "" });
    }
    assert!(reload::apply(&mut s, old_items, vec![Ok(old_info)]).unwrap().is_empty(), "the newer copy reload supersedes its original's old result");
    s.execute("edit.undo", &json!({})).unwrap();
    for old in &before {
        assert_eq!(s.catalog.photo(old.id).unwrap(), old);
    }
    s.execute("edit.redo", &json!({})).unwrap();
    for old in &before {
        assert_ne!(s.catalog.photo(old.id).unwrap().content_hash, old.content_hash);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

fn reload_relinked_virtual_copy(from_copy: bool) {
    let dir = temp_dir(&format!("reload-relinked-copy-{from_copy}"));
    let original_path = dir.join("original.png");
    let copy_path = dir.join("other.png");
    write_png(&original_path, 10);
    write_png(&copy_path, 20);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [original_path.to_string_lossy()]})).unwrap();
    let original = lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    let mut copies = Vec::new();
    for _ in 0..2 {
        let r = s.execute("photo.virtualCopy", &json!({"ids": [original.0]})).unwrap();
        copies.push(lcc_catalog::PhotoId(r["ids"][0].as_u64().unwrap()));
    }
    let (copy, sibling) = (copies[0], copies[1]);
    s.execute("photo.relink", &json!({"id": copy.0, "path": copy_path.to_string_lossy()})).unwrap();
    assert_eq!(s.catalog.photo(copy).unwrap().copy_of, Some(original));
    let before: Vec<_> = [original, sibling, copy].into_iter().map(|id| s.catalog.photo(id).unwrap().clone()).collect();
    write_png(&original_path, 100);
    write_png(&copy_path, 200);

    if from_copy {
        // A relinked copy keeps its own probe; same-file siblings still share one original probe.
        let items = crate::cmd::reload::plan(&mut s, &json!({"ids": [copy.0, original.0, sibling.0]})).unwrap();
        assert_eq!(items.iter().map(|item| item.id).collect::<Vec<_>>(), vec![copy, original]);
        assert_eq!(items[0].path, copy_path.to_string_lossy());
        assert_eq!(items[1].path, original_path.to_string_lossy());
    }
    let target = if from_copy { copy } else { original };
    let changed = if from_copy { vec![copy] } else { vec![original, sibling] };
    let r = s.execute("photo.reload", &json!({"ids": [target.0]})).unwrap();
    assert_eq!(r["reloaded"], json!(changed.iter().map(|id| id.0).collect::<Vec<_>>()));
    for old in &before {
        let new = s.catalog.photo(old.id).unwrap();
        if changed.contains(&old.id) {
            assert_ne!(new.content_hash, old.content_hash);
            assert_eq!(new.develop, old.develop);
        } else {
            assert_eq!(new, old, "a photo using another file stays unchanged");
        }
    }
    s.execute("edit.undo", &json!({})).unwrap();
    for old in &before {
        assert_eq!(s.catalog.photo(old.id).unwrap(), old, "reload remains one undo step");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
