use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use crate::Session;

fn big_dng(seed: u32) -> Vec<u8> {
    let (w, h) = (2000usize, 1500usize);
    let mut x = 0x9e37_79b9u32 ^ seed;
    let data: Vec<u16> = (0..w * h)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            256 + (x % 15000) as u16
        })
        .collect();
    let meta = lightcraft_meta::Metadata { make: Some("Example".into()), model: Some("Camera".into()), ..Default::default() };
    crate::tests_xmp::synthetic_dng_of(w, h, "RGGB", data, None, meta)
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

struct Counted {
    whole: Arc<AtomicUsize>,
    headers: Arc<AtomicUsize>,
}

fn counted_session() -> (Session, Counted) {
    let mut s = Session::new().with_fs();
    let (whole, headers) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (file_probe, header_probe) = (s.media.file_probe.clone().unwrap(), s.media.header_probe.clone().unwrap());
    let w = whole.clone();
    s.media.file_probe = Some(Arc::new(move |path: &str| {
        w.fetch_add(1, Ordering::Relaxed);
        file_probe(path)
    }));
    let h = headers.clone();
    s.media.header_probe = Some(Arc::new(move |path: &str| {
        h.fetch_add(1, Ordering::Relaxed);
        header_probe(path)
    }));
    (s, Counted { whole, headers })
}

fn reads(c: &Counted) -> (usize, usize) {
    (c.headers.swap(0, Ordering::Relaxed), c.whole.swap(0, Ordering::Relaxed))
}

fn import(s: &mut Session, paths: &[&Path], mode: &str, dest: &Path) -> Value {
    let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    s.execute("library.import", &json!({"paths": paths, "mode": mode, "destination": dest.to_string_lossy(), "organize": "flat"})).unwrap()
}

fn hash_of(s: &Session, id: &Value) -> Option<String> {
    s.catalog.photo(lightcraft_catalog::PhotoId(id.as_u64().unwrap())).and_then(|p| p.content_hash.clone())
}

#[test]
fn library_import_add_copy_move_read_raws_from_headers() {
    let base = crate::tests_xmp::temp_dir("import-headers-modes");
    let (s_dir, dest) = (base.join("card"), base.join("out"));
    let (mut s, counted) = counted_session();
    let added = write(&s_dir, "added.dng", &big_dng(1));
    let r = import(&mut s, &[&added], "add", &dest);
    assert_eq!(reads(&counted), (1, 0), "Add reads the raw's headers: {r}");
    assert_eq!(hash_of(&s, &r["imported"][0]), None, "a raw described from its headers has no hash");
    let copied_bytes = big_dng(2);
    let copied = write(&s_dir, "copied.dng", &copied_bytes);
    let r = import(&mut s, &[&copied], "copy", &dest);
    assert_eq!(reads(&counted), (1, 0), "Copy too: {r}");
    assert_eq!(hash_of(&s, &r["imported"][0]), Some(lightcraft_preview::hash_bytes(&copied_bytes).to_string()), "the copy hashed it");
    let moved = write(&s_dir, "moved.dng", &big_dng(3));
    let r = import(&mut s, &[&moved], "move", &dest);
    assert_eq!(reads(&counted), (1, 0), "Move too: {r}");
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    assert!(!moved.exists() && dest.join("moved.dng").exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn import_preview_marks_a_hashless_copy_as_a_duplicate() {
    let base = crate::tests_xmp::temp_dir("import-headers-preview");
    let bytes = big_dng(4);
    let orig = write(&base.join("first"), "orig.dng", &bytes);
    let copy = write(&base.join("second"), "copy.dng", &bytes);
    let (mut s, counted) = counted_session();
    let r = import(&mut s, &[&orig], "add", &base.join("out"));
    let id = r["imported"][0].as_u64();
    reads(&counted);
    let preview = s.execute("library.importPreview", &json!({"paths": [copy.to_string_lossy()]})).unwrap();
    assert_eq!(reads(&counted), (1, 0), "the review reads the raw's headers: {preview}");
    let c = &preview["candidates"][0];
    assert_eq!((c["duplicate"].as_str(), c["existing"].as_u64()), (Some("content"), id), "{preview}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_copied_raw_imported_from_headers_still_has_a_full_hash() {
    let base = crate::tests_xmp::temp_dir("import-headers-copy");
    let bytes = big_dng(5);
    let card = write(&base.join("card"), "a.dng", &bytes);
    let (mut s, counted) = counted_session();
    let preview = s.execute("library.importPreview", &json!({"paths": [card.to_string_lossy()]})).unwrap();
    assert_eq!(preview["scanned"], 1, "{preview}");
    let r = import(&mut s, &[&card], "copy", &base.join("out"));
    assert_eq!(reads(&counted), (1, 0), "probed once, by the review, from its headers: {r}");
    assert_eq!(hash_of(&s, &r["imported"][0]), Some(lightcraft_preview::hash_bytes(&bytes).to_string()));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn g_one_metadata_read_is_saved_before_immediate_reopen() {
    let base = crate::tests_xmp::temp_dir("g-one-metadata");
    let bytes = big_dng(81);
    let original = write(&base, "original.dng", &bytes);
    let library = base.join("library");
    let mut s = Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    let r = import(&mut s, &[&original], "add", &base.join("out"));
    let id = r["imported"][0].clone();
    assert_eq!(hash_of(&s, &id), None);
    let undo = s.undo.len();
    s.execute("photo.allMetadata", &json!({"id": id})).unwrap();
    assert_eq!(s.originals.counts(), (1, 1), "metadata and embedded XMP share the same original buffer");
    assert_eq!(s.undo.len(), undo, "learning is system bookkeeping");
    drop(s);
    let mut reopened = Session::new().with_fs();
    reopened.open_library(&library, false).unwrap();
    assert_eq!(hash_of(&reopened, &id), Some(lightcraft_preview::hash_bytes(&bytes).to_string()));
    drop(reopened);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_direct_original_export_is_learned_by_persist() {
    let base = crate::tests_xmp::temp_dir("g-original-export");
    let bytes = big_dng(82);
    let original = write(&base, "original.dng", &bytes);
    let mut s = Session::new().with_fs();
    let r = import(&mut s, &[&original], "add", &base.join("out"));
    let id = r["imported"][0].clone();
    assert_eq!(hash_of(&s, &id), None);
    let options = crate::export::ExportOptions { format: crate::export::ExportFormat::Original, ..Default::default() };
    let exported = crate::export::export_photo(&mut s, lightcraft_catalog::PhotoId(id.as_u64().unwrap()), &options, 1).unwrap();
    assert_eq!(exported.bytes, bytes);
    s.persist().unwrap();
    assert_eq!(hash_of(&s, &id), Some(lightcraft_preview::hash_bytes(&bytes).to_string()));
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_unchanged_reload_learns_without_an_undo_step() {
    let base = crate::tests_xmp::temp_dir("g-unchanged-reload");
    let bytes = big_dng(83);
    let original = write(&base, "original.dng", &bytes);
    let mut s = Session::new().with_fs();
    let r = import(&mut s, &[&original], "add", &base.join("out"));
    let id = r["imported"][0].clone();
    s.execute("library.select", &json!({"ids": [id]})).unwrap();
    let undo = s.undo.len();
    s.execute("photo.reload", &json!({})).unwrap();
    assert_eq!(s.originals.counts(), (1, 1), "whole probe hashes the same buffer once");
    assert_eq!(s.undo.len(), undo);
    assert_eq!(hash_of(&s, &id), Some(lightcraft_preview::hash_bytes(&bytes).to_string()));
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_quarantined_relink_validates_same_read_before_primary_authorization() {
    for changed in [false, true] {
        let base = crate::tests_xmp::temp_dir(&format!("g-relink-validation-{changed}"));
        let mut old = big_dng(84);
        let mut current = if changed { big_dng(85) } else { old.clone() };
        let len = old.len().max(current.len());
        old.resize(len, 0);
        current.resize(len, 0);
        assert_eq!(old.len(), current.len());
        let a = write(&base, "old.dng", &old);
        let b = write(&base, "relinked.dng", &current);
        let mut s = Session::new().with_fs();
        let r = import(&mut s, &[&a], "add", &base.join("out"));
        let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        let p = s.catalog.photo(id).unwrap().clone();
        let old_hash = lightcraft_preview::hash_bytes(&old).to_string();
        s.apply_system(lightcraft_catalog::Op::SetContent {
            id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: Some(old_hash.clone()),
            preview_only: p.preview_only.clone(),
            preview_checked: p.preview_checked.clone(),
        })
        .unwrap();
        s.commit(
            "Relink",
            lightcraft_catalog::Op::Relink {
                id,
                file_name: "relinked.dng".into(),
                source: lightcraft_catalog::Source::File { path: b.to_string_lossy().into_owned() },
                format: None,
            },
        )
        .unwrap();
        assert!(!s.media.content_keys(s.catalog.photo(id).unwrap()).contains(&old_hash));
        assert_eq!(s.originals.counts(), (0, 0));
        let current_source = s.media.identities.ticket(s.catalog.photo(id).unwrap());
        let read = s.media.file_bytes.clone().unwrap();
        assert_eq!(read(b.to_str().unwrap()).unwrap(), current);
        assert!(!s.media.content_keys(s.catalog.photo(id).unwrap()).contains(&old_hash), "reader never updates owner authorization");
        s.persist().unwrap();
        let expected = lightcraft_preview::hash_bytes(&current).to_string();
        let p = s.catalog.photo(id).unwrap();
        assert_eq!(p.content_hash.as_deref(), Some(expected.as_str()));
        assert!(s.media.content_keys(p).contains(&expected));
        assert!(current_source.current(), "validated hash reconciliation keeps the already-retired source lifecycle stable");
        assert!(!s.media.content_keys(p).contains(&format!("file:{}:{}", b.display(), current.len())));
        assert_eq!(s.originals.counts(), (1, 1), "reconcile uses the same full read");
        read(b.to_str().unwrap()).unwrap();
        assert_eq!(s.originals.counts(), (2, 1), "authorized hashed source avoids repeat hashing");
        drop(s);
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[test]
fn g_same_bytes_relink_reconciles_the_probe_without_set_content_or_reread() {
    let base = crate::tests_xmp::temp_dir("g-relink-probe-reconcile");
    let bytes = big_dng(86);
    let a = write(&base, "old.dng", &bytes);
    let b = write(&base, "new.dng", &bytes);
    let mut s = Session::new().with_fs();
    let r = import(&mut s, &[&a], "add", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    s.execute("photo.allMetadata", &json!({"id": id.0})).unwrap();
    let before = s.originals.counts();
    let expected = s.catalog.photo(id).unwrap().content_hash.clone().unwrap();
    s.execute("photo.relink", &json!({"id": id.0, "path": b.to_string_lossy()})).unwrap();
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.content_hash.as_deref(), Some(expected.as_str()));
    assert!(s.media.content_keys(p).contains(&expected), "validated unchanged relink must reconcile even without SetContent");
    assert!(!s.media.content_keys(p).contains(&format!("file:{}:{}", b.display(), bytes.len())));
    let after = s.originals.counts();
    assert_eq!((after.0 - before.0, after.1 - before.1), (1, 1));
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_first_render_writes_thumbnail_and_view_under_its_own_hash() {
    for view in [false, true] {
        let base = crate::tests_xmp::temp_dir(&format!("g-render-hash-writer-{view}"));
        let bytes = big_dng(87);
        let path = write(&base, "original.dng", &bytes);
        let mut s = Session::new().with_fs();
        let r = import(&mut s, &[&path], "add", &base.join("out"));
        let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        let job = if view { s.loupe_job(id, 512, 512, true) } else { s.thumb_job(id, 256) }.unwrap();
        let old = if view { job.view_cache.as_ref() } else { job.cache.as_ref() }.unwrap().1;
        let result = job.run();
        assert!(result.rendered.is_ok());
        s.accept(&result);
        assert_eq!(s.catalog.photo(id).unwrap().content_hash.as_deref(), Some(lightcraft_preview::hash_bytes(&bytes).to_string().as_str()));
        let next = if view { s.loupe_job(id, 512, 512, true) } else { s.thumb_job(id, 256) }.unwrap();
        let (cache, key) = if view { next.view_cache.as_ref() } else { next.cache.as_ref() }.unwrap();
        assert_ne!(*key, old);
        assert!(cache.get_at(next.cache_generation, *key).is_some(), "worker must name pixels from the hash of its own read");
        assert_eq!(s.originals.counts(), (1, 1));
        drop(s);
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[test]
fn g_first_smart_build_writes_under_its_own_read_hash() {
    let base = crate::tests_xmp::temp_dir("g-smart-hash-writer");
    let bytes = big_dng(88);
    let path = write(&base, "original.dng", &bytes);
    let mut s = Session::new().with_fs();
    s.open_library(base.join("library"), false).unwrap();
    let r = import(&mut s, &[&path], "add", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    let out = s.execute("library.smartPreviews", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(out["built"], 1, "{out}");
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.content_hash.as_deref(), Some(lightcraft_preview::hash_bytes(&bytes).to_string().as_str()));
    let primary = crate::smart::sharded_file_path(s.media.smart_dir.as_ref().unwrap(), p);
    assert!(crate::smart::is_valid(&primary), "first smart writer uses its own read hash before owner learns it");
    assert_eq!(s.originals.counts(), (1, 1));
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_completed_render_replaced_before_accept_cannot_become_current_source() {
    let base = crate::tests_xmp::temp_dir("g-render-replaced-before-accept");
    let mut old = big_dng(89);
    let mut new = big_dng(90);
    let len = old.len().max(new.len());
    old.resize(len, 0);
    new.resize(len, 0);
    let path = write(&base, "original.dng", &old);
    let mut s = Session::new().with_fs();
    let r = import(&mut s, &[&path], "add", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    let result = s.loupe_job(id, 512, 512, true).unwrap().run();
    assert!(result.rendered.is_ok() && result.loaded.is_some());
    let replacement = write(&base, "replacement.dng", &new);
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(replacement, &path).unwrap();
    s.accept(&result);
    assert!(s.catalog.photo(id).unwrap().content_hash.is_none());
    s.media.file_loader = Some(Arc::new(|_, _| Err("replacement decoder must run".into())));
    assert!(s.render_now(id, 512, 512).is_err(), "old decoded pixels cannot be accepted under current lifecycle");
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_copy_and_duplicate_keep_their_native_read_contracts() {
    let base = crate::tests_xmp::temp_dir("g-transfer-read-contracts");
    let bytes = big_dng(91);
    let card = write(&base.join("card"), "original.dng", &bytes);
    let mut s = Session::new().with_fs();
    let result = import(&mut s, &[&card], "copy", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(result["imported"][0].as_u64().unwrap());
    assert_eq!(s.originals.counts(), (2, 2), "one source stream, one destination verification");
    assert_eq!(s.catalog.photo(id).unwrap().content_hash.as_deref(), Some(lightcraft_preview::hash_bytes(&bytes).to_string().as_str()));
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let before = s.originals.counts();
    let result = s.execute("photo.duplicate", &json!({})).unwrap();
    assert_eq!(s.originals.counts(), before, "Duplicate keeps its OS clone without original byte reads");
    let duplicate = lightcraft_catalog::PhotoId(result["ids"][0].as_u64().unwrap());
    assert!(s.catalog.photo(duplicate).unwrap().content_hash.as_deref().unwrap().ends_with(&format!(":dup{}", duplicate.0)));
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_failed_replacement_refreshes_wanted_before_same_read_recovery() {
    let base = crate::tests_xmp::temp_dir("g-failed-replacement-recovery");
    let mut old = big_dng(92);
    let mut current = big_dng(93);
    let len = old.len().max(current.len());
    old.resize(len, 0);
    current.resize(len, 0);
    let path = write(&base, "original.dng", &old);
    let mut s = Session::new().with_fs();
    s.open_library(base.join("library"), false).unwrap();
    let r = import(&mut s, &[&path], "add", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    s.execute("photo.allMetadata", &json!({"id": id.0})).unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    std::fs::write(&path, &current).unwrap();
    let expected = lightcraft_preview::hash_bytes(&current).to_string();
    let fault = lightcraft_catalog::safe_file::fail_writes_after(0);
    assert!(
        s.commit(
            "Replacement",
            lightcraft_catalog::Op::SetContent {
                id,
                width: p.width,
                height: p.height,
                file_size: p.file_size,
                content_hash: Some(expected.clone()),
                preview_only: p.preview_only.clone(),
                preview_checked: p.preview_checked.clone()
            }
        )
        .is_err()
    );
    drop(fault);
    let before = s.originals.counts();
    assert_eq!(s.media.file_bytes.as_ref().unwrap()(path.to_str().unwrap()).unwrap(), current);
    s.persist().unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().content_hash.as_deref(), Some(expected.as_str()));
    assert!(s.media.content_keys(s.catalog.photo(id).unwrap()).contains(&expected));
    let after = s.originals.counts();
    assert_eq!((after.0 - before.0, after.1 - before.1), (1, 1));
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_export_batch_drains_learning_even_when_destination_write_fails() {
    for fail in [false, true] {
        let base = crate::tests_xmp::temp_dir(&format!("g-export-completion-{fail}"));
        let bytes = big_dng(94);
        let original = write(&base, "original.dng", &bytes);
        let library = base.join("library");
        let mut s = Session::new().with_fs();
        s.open_library(&library, false).unwrap();
        let imported = import(&mut s, &[&original], "add", &base.join("out"));
        let id = lightcraft_catalog::PhotoId(imported["imported"][0].as_u64().unwrap());
        let options = crate::export::ExportOptions { format: crate::export::ExportFormat::Original, ..Default::default() };
        let to = crate::export::Destination { dir: base.join("exports").to_string_lossy().into_owned(), exact: None };
        let result = crate::export::export_batch(
            &mut s,
            &[id],
            &options,
            &to,
            &mut |_, _| {
                if fail { Err("synthetic destination failure".into()) } else { Ok(()) }
            },
            &|_| false,
        );
        assert_eq!(result.is_err(), fail, "{result:?}");
        assert_eq!(s.originals.counts(), (1, 1));
        assert_eq!(
            s.catalog.photo(id).unwrap().content_hash,
            Some(lightcraft_preview::hash_bytes(&bytes).to_string()),
            "completion accepts a successful original read even when export writing fails"
        );
        drop(s);
        let mut reopened = Session::new().with_fs();
        reopened.open_library(&library, false).unwrap();
        assert_eq!(reopened.catalog.photo(id).unwrap().content_hash, Some(lightcraft_preview::hash_bytes(&bytes).to_string()));
        drop(reopened);
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[test]
fn g_prepared_hash_cannot_revive_a_source_replaced_before_owner_commit() {
    let base = crate::tests_xmp::temp_dir("g-prepared-source-replaced");
    let mut old = big_dng(97);
    let mut new = big_dng(98);
    let len = old.len().max(new.len());
    old.resize(len, 0);
    new.resize(len, 0);
    let original = write(&base, "original.dng", &old);
    let duplicate = write(&base, "duplicate.dng", &old);
    let mut s = Session::new().with_fs();
    let imported = import(&mut s, &[&original], "add", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(imported["imported"][0].as_u64().unwrap());
    assert!(s.catalog.photo(id).unwrap().content_hash.is_none());
    let opts = crate::import::ImportOptions::default();
    let mut job = crate::import::ImportJob::new(&mut s, opts.clone()).unwrap();
    let prepared = job.prepare_files(vec![duplicate.to_string_lossy().into_owned()], &std::sync::atomic::AtomicBool::new(false));
    let time = std::fs::metadata(&original).unwrap().modified().unwrap();
    let replacement = write(&base, "replacement.dng", &new);
    std::fs::File::options().write(true).open(&replacement).unwrap().set_times(std::fs::FileTimes::new().set_modified(time)).unwrap();
    std::fs::rename(&original, base.join("retired.dng")).unwrap();
    std::fs::rename(replacement, &original).unwrap();
    let report = crate::import::commit_prepared(&mut s, &opts, job.now(), prepared).unwrap();
    assert!(s.catalog.photo(id).unwrap().content_hash.is_none(), "Prepared.hashes cannot bypass the rejected read proof: {report:?}");
    assert!(!report.failed.is_empty(), "stale comparison must request retry instead of reporting a current duplicate: {report:?}");
    s.execute("photo.allMetadata", &json!({"id": id.0})).unwrap();
    let hash = lightcraft_preview::hash_bytes(&new).to_string();
    assert_eq!(s.catalog.photo(id).unwrap().content_hash.as_deref(), Some(hash.as_str()));
    assert!(s.media.content_keys(s.catalog.photo(id).unwrap()).contains(&hash));
    drop((job, s));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_native_ready_cannot_adopt_a_later_same_path_probe() {
    let base = crate::tests_xmp::temp_dir("g-native-ready-provenance");
    let old = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![1024; 32 * 24], None, Default::default());
    let new = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![2048; 32 * 24], None, Default::default());
    let path = write(&base, "original.dng", &old);
    let mut s = Session::new().with_fs();
    let opts = crate::import::ImportOptions { headers_only: false, ..Default::default() };
    let mut job = crate::import::ImportJob::new(&mut s, opts.clone()).unwrap();
    let prepared = job.prepare_files(vec![path.to_string_lossy().into_owned()], &std::sync::atomic::AtomicBool::new(false));
    let replacement = write(&base, "replacement.dng", &new);
    std::fs::rename(&path, base.join("retired.dng")).unwrap();
    std::fs::rename(replacement, &path).unwrap();
    s.originals.in_scope(|| s.media.file_probe.as_ref().unwrap()(path.to_str().unwrap())).unwrap();
    let report = crate::import::commit_prepared(&mut s, &opts, job.now(), prepared).unwrap();
    s.persist().unwrap();
    assert!(
        report.imported.is_empty() && !report.failed.is_empty(),
        "a changed live proof must leave the catalog unchanged and request retry: {report:?}"
    );
    drop((job, s));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_native_prepared_item_cannot_cross_library_lifetimes() {
    let base = crate::tests_xmp::temp_dir("g-native-ready-library");
    let bytes = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![1024; 32 * 24], None, Default::default());
    let path = write(&base, "original.dng", &bytes);
    let mut s = Session::new().with_fs();
    s.open_library(base.join("one"), false).unwrap();
    let opts = crate::import::ImportOptions { headers_only: false, ..Default::default() };
    let mut job = crate::import::ImportJob::new(&mut s, opts.clone()).unwrap();
    let prepared = job.prepare_files(vec![path.to_string_lossy().into_owned()], &std::sync::atomic::AtomicBool::new(false));
    s.open_library(base.join("two"), false).unwrap();
    assert!(
        crate::import::commit_prepared(&mut s, &opts, job.now(), prepared).is_err(),
        "an import prepared by the previous library must request retry"
    );
    assert_eq!(s.catalog.photos().count(), 0);
    drop((job, s));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_quarantined_clone_and_duplicate_index_do_not_trust_the_old_hash() {
    let base = crate::tests_xmp::temp_dir("g-quarantined-dupes");
    let mut old = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![1024; 32 * 24], None, Default::default());
    let mut new = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![2048; 32 * 24], None, Default::default());
    let len = old.len().max(new.len());
    old.resize(len, 0);
    new.resize(len, 0);
    let path = write(&base, "old.dng", &old);
    let replacement = write(&base, "current.dng", &new);
    let candidate = write(&base, "candidate.dng", &old);
    let mut s = Session::new().with_fs();
    let first = import(&mut s, &[&path], "add", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(first["imported"][0].as_u64().unwrap());
    s.commit(
        "Relink",
        lightcraft_catalog::Op::Relink {
            id,
            source: lightcraft_catalog::Source::File { path: replacement.to_string_lossy().into_owned() },
            file_name: "current.dng".into(),
            format: None,
        },
    )
    .unwrap();
    assert!(s.media.identities.needs_validation(s.catalog.photo(id).unwrap()));
    let before = s.originals.counts();
    let duplicate = s.execute("photo.duplicate", &json!({"ids": [id.0]})).unwrap();
    let copy = lightcraft_catalog::PhotoId(duplicate["ids"][0].as_u64().unwrap());
    assert_eq!(s.originals.counts(), before, "OS clone does not read original bytes");
    assert!(s.catalog.photo(copy).unwrap().content_hash.is_none(), "unverified relink hash cannot certify cloned bytes");
    let result = import(&mut s, &[&candidate], "add", &base.join("out"));
    assert_eq!(result["imported"].as_array().unwrap().len(), 1, "old H1 must not be indexed as the relinked H2 content: {result}");
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}
#[test]
fn g_quarantined_duplicate_index_does_not_trust_the_old_hash() {
    let base = crate::tests_xmp::temp_dir("g-quarantined-index");
    let mut old = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![1024; 32 * 24], None, Default::default());
    let mut new = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![2048; 32 * 24], None, Default::default());
    let len = old.len().max(new.len());
    old.resize(len, 0);
    new.resize(len, 0);
    let path = write(&base, "old.dng", &old);
    let replacement = write(&base, "current.dng", &new);
    let candidate = write(&base, "candidate.dng", &old);
    let mut s = Session::new().with_fs();
    let first = import(&mut s, &[&path], "add", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(first["imported"][0].as_u64().unwrap());
    s.commit(
        "Relink",
        lightcraft_catalog::Op::Relink {
            id,
            source: lightcraft_catalog::Source::File { path: replacement.to_string_lossy().into_owned() },
            file_name: "current.dng".into(),
            format: None,
        },
    )
    .unwrap();
    assert!(s.media.identities.needs_validation(s.catalog.photo(id).unwrap()));
    let result = import(&mut s, &[&candidate], "add", &base.join("out"));
    assert_eq!(result["imported"].as_array().unwrap().len(), 1, "old H1 must not be indexed as the relinked H2 content: {result}");
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_paired_original_read_rejects_reload_but_accepts_hash_learning() {
    for replace in [false, true] {
        let base = crate::tests_xmp::temp_dir(&format!("g-pair-lifecycle-{replace}"));
        let old = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![1024; 32 * 24], None, Default::default());
        let new = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![2048; 32 * 24], None, Default::default());
        let path = write(&base, "original.dng", &old);
        let mut s = Session::new().with_fs();
        let r = import(&mut s, &[&path], "add", &base.join("out"));
        let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        if !replace {
            let p = s.catalog.photo(id).unwrap().clone();
            s.apply_system(lightcraft_catalog::Op::SetContent {
                id,
                width: p.width,
                height: p.height,
                file_size: p.file_size,
                content_hash: None,
                preview_only: p.preview_only.clone(),
                preview_checked: p.preview_checked.clone(),
            })
            .unwrap();
        }
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let resume = std::sync::Mutex::new(resume_rx);
        let loader = s.media.file_loader.clone().unwrap();
        s.media.denoise.loader = Some(Arc::new(move |path, edge, _| {
            started_tx.send(()).unwrap();
            resume.lock().unwrap().recv().unwrap();
            let (image, info) = loader(path, edge)?;
            let twin = lightcraft_raster::Rgb32f::filled(image.width, image.height, [0.25; 3]);
            Ok((image, Some(twin), info))
        }));
        s.media.denoise.set(id, crate::media::DenoiseSpec { product: base.join("old.lcdn"), key: "old-product".into(), make: None });
        let source = s.loupe_job(id, 512, 512, true).unwrap().source;
        let worker = std::thread::spawn(move || source.load_source());
        started_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        let before = s.catalog.photo(id).unwrap().develop.clone();
        if replace {
            let next = write(&base, "next.dng", &new);
            std::fs::rename(&path, base.join("retired.dng")).unwrap();
            std::fs::rename(next, &path).unwrap();
            let result = s.execute("photo.reload", &json!({"ids": [id.0]})).unwrap();
            assert_eq!(result["reloaded"], json!([id.0]), "real Reload must retire the old pair: {result}");
        } else {
            let p = s.catalog.photo(id).unwrap().clone();
            s.apply_system(lightcraft_catalog::Op::SetContent {
                id,
                width: p.width,
                height: p.height,
                file_size: p.file_size,
                content_hash: Some(lightcraft_preview::hash_bytes(&old).to_string()),
                preview_only: p.preview_only.clone(),
                preview_checked: p.preview_checked.clone(),
            })
            .unwrap();
        }
        resume_tx.send(()).unwrap();
        let result = worker.join().unwrap();
        assert_eq!(result.is_err(), replace, "a stale denoise/original pair must fail after Reload; mere hash learning stays valid");
        assert_eq!(s.catalog.photo(id).unwrap().develop, before);
        drop(s);
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[test]
fn g_divergent_virtual_copy_learns_its_own_original_after_master_relink() {
    let base = crate::tests_xmp::temp_dir("g-copy-physical-owner");
    let old = big_dng(101);
    let new = big_dng(102);
    let a = write(&base, "old.dng", &old);
    let b = write(&base, "new.dng", &new);
    let mut s = Session::new().with_fs();
    let r = import(&mut s, &[&a], "add", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    let copy = lightcraft_catalog::PhotoId(s.execute("photo.virtualCopy", &json!({"ids": [id.0]})).unwrap()["ids"][0].as_u64().unwrap());
    s.commit(
        "Relink",
        lightcraft_catalog::Op::Relink {
            id,
            source: lightcraft_catalog::Source::File { path: b.to_string_lossy().into_owned() },
            file_name: "new.dng".into(),
            format: None,
        },
    )
    .unwrap();
    assert_eq!(s.catalog.photo(copy).unwrap().source, lightcraft_catalog::Source::File { path: a.to_string_lossy().into_owned() });
    assert_eq!(s.media.file_bytes.as_ref().unwrap()(a.to_str().unwrap()).unwrap(), old);
    s.persist().unwrap();
    let expected = lightcraft_preview::hash_bytes(&old).to_string();
    assert_eq!(s.catalog.photo(copy).unwrap().content_hash.as_deref(), Some(expected.as_str()));
    assert!(s.catalog.photo(id).unwrap().content_hash.is_none());
    assert!(s.media.content_keys(s.catalog.photo(copy).unwrap()).contains(&expected));
    assert_eq!(s.originals.counts(), (1, 1));
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_copy_as_dng_learns_the_verified_final_bytes_without_an_extra_read() {
    let base = crate::tests_xmp::temp_dir("g-copy-dng-final-evidence");
    let bytes = big_dng(103);
    let raw = lightcraft_raw::decode(&bytes).unwrap();
    let bytes = lightcraft_raw::write_dng(&raw, &lightcraft_raw::DngWriteOptions { xmp: Some("<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description xmlns:xmp='http://ns.adobe.com/xap/1.0/' xmp:Rating='3'/></rdf:RDF></x:xmpmeta>".into()), ..Default::default() }).unwrap();
    let path = write(&base.join("card"), "source.nef", &bytes);
    let source_hash = lightcraft_preview::hash_bytes(&bytes).to_string();
    let mut s = Session::new().with_fs();
    let result = s.execute("library.import", &json!({"paths": [path.to_string_lossy()], "mode": "copy", "destination": base.join("out").to_string_lossy(), "organize": "flat", "dng": true})).unwrap();
    let id = lightcraft_catalog::PhotoId(result["imported"][0].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap();
    let lightcraft_catalog::Source::File { path: final_path } = &p.source else { panic!("file") };
    let final_bytes = std::fs::read(final_path).unwrap();
    let final_hash = lightcraft_preview::hash_bytes(&final_bytes).to_string();
    assert_ne!(source_hash, final_hash, "conversion changes the encoded bytes");
    assert_eq!(p.format, "DNG");
    assert_eq!(p.file_size, final_bytes.len() as u64);
    assert_eq!(p.content_hash.as_deref(), Some(final_hash.as_str()), "only verified final-file bytes may supply the destination hash");
    assert_eq!(
        s.originals.counts(),
        (4, 3),
        "existing source transfer, destination verification, conversion input, final DNG readback; no additional read"
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_native_duplicate_result_rechecks_its_input_read_proof_at_owner_commit() {
    let base = crate::tests_xmp::temp_dir("g-dupe-input-proof");
    let mut old = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![1024; 32 * 24], None, Default::default());
    let mut new = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![2048; 32 * 24], None, Default::default());
    let len = old.len().max(new.len());
    old.resize(len, 0);
    new.resize(len, 0);
    let original = write(&base, "original.dng", &old);
    let candidate = write(&base, "candidate.dng", &old);
    let mut s = Session::new().with_fs();
    let first = import(&mut s, &[&original], "add", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(first["imported"][0].as_u64().unwrap());
    let opts = crate::import::ImportOptions { headers_only: false, ..Default::default() };
    let mut job = crate::import::ImportJob::new(&mut s, opts.clone()).unwrap();
    let prepared = job.prepare_files(vec![candidate.to_string_lossy().into_owned()], &std::sync::atomic::AtomicBool::new(false));
    let time = std::fs::metadata(&candidate).unwrap().modified().unwrap();
    let replacement = write(&base, "replacement.dng", &new);
    FileTimesFixture::restore(&replacement, time);
    std::fs::rename(&candidate, base.join("retired.dng")).unwrap();
    std::fs::rename(replacement, &candidate).unwrap();
    let report = crate::import::commit_prepared(&mut s, &opts, job.now(), prepared).unwrap();
    assert!(
        report.duplicates.is_empty() && !report.failed.is_empty(),
        "a changed incoming file must not be reported as a current duplicate: {report:?}"
    );
    assert_eq!(s.catalog.photo(id).unwrap().content_hash.as_deref(), Some(lightcraft_preview::hash_bytes(&old).to_string().as_str()));
    drop((job, s));
    std::fs::remove_dir_all(base).unwrap();
}

struct FileTimesFixture;
impl FileTimesFixture {
    fn restore(path: &Path, time: std::time::SystemTime) {
        std::fs::File::options().write(true).open(path).unwrap().set_times(std::fs::FileTimes::new().set_modified(time)).unwrap();
    }
}

#[test]
fn g_first_whole_read_reconciles_facts_from_a_stale_partial_probe() {
    let base = crate::tests_xmp::temp_dir("g-cached-header-facts");
    let mut old = big_dng(104);
    let data = (0..1500usize * 2000).map(|i| 256 + ((i * 7919) % 15000) as u16).collect();
    let mut new = crate::tests_xmp::synthetic_dng_of(1500, 2000, "RGGB", data, None, Default::default());
    let len = old.len().max(new.len());
    old.resize(len, 0);
    new.resize(len, 0);
    let path = write(&base, "original.dng", &old);
    let mut s = Session::new().with_fs();
    let cached = s.media.header_probe.as_ref().unwrap()(path.to_str().unwrap()).unwrap();
    assert!(cached.content_hash.is_none());
    cached.native_read.as_ref().unwrap().release(); // Expired/unsupported observations cannot certify cached facts.
    assert_eq!((cached.width, cached.height), (2000, 1500));
    s.import_probes.insert(path.to_string_lossy().into_owned(), cached);
    let replacement = write(&base, "replacement.dng", &new);
    std::fs::rename(&path, base.join("retired.dng")).unwrap();
    std::fs::rename(replacement, &path).unwrap();
    let r = import(&mut s, &[&path], "add", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    assert_eq!(s.originals.counts(), (0, 0));
    assert_eq!(s.media.file_bytes.as_ref().unwrap()(path.to_str().unwrap()).unwrap(), new);
    s.persist().unwrap();
    let p = s.catalog.photo(id).unwrap();
    assert_eq!((p.width, p.height), (1500, 2000), "the learned hash and physical facts must describe the SAME read");
    assert_eq!(p.content_hash.as_deref(), Some(lightcraft_preview::hash_bytes(&new).to_string().as_str()));
    assert_eq!(s.originals.counts(), (1, 1));
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}
#[test]
fn g_copy_cannot_confirm_a_hash_using_a_different_sources_cached_header() {
    let base = crate::tests_xmp::temp_dir("g-copied-cached-header-facts");
    let mut old = big_dng(104);
    let data = (0..1500usize * 2000).map(|i| 256 + ((i * 7919) % 15000) as u16).collect();
    let mut new = crate::tests_xmp::synthetic_dng_of(1500, 2000, "RGGB", data, None, Default::default());
    let len = old.len().max(new.len());
    old.resize(len, 0);
    new.resize(len, 0);
    let path = write(&base, "original.dng", &old);
    let mut s = Session::new().with_fs();
    let cached = s.media.header_probe.as_ref().unwrap()(path.to_str().unwrap()).unwrap();
    assert!(cached.content_hash.is_none());
    assert_eq!((cached.width, cached.height), (2000, 1500));
    s.import_probes.insert(path.to_string_lossy().into_owned(), cached);
    let replacement = write(&base, "replacement.dng", &new);
    std::fs::rename(&path, base.join("retired.dng")).unwrap();
    std::fs::rename(replacement, &path).unwrap();
    let r = import(&mut s, &[&path], "copy", &base.join("out"));
    let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap();
    let actual_hash = lightcraft_preview::hash_bytes(&new).to_string();
    assert!(
        p.content_hash.as_deref() != Some(actual_hash.as_str()) || (p.width, p.height) == (1500, 2000),
        "destination proof cannot bind new bytes to stale source metadata"
    );
    drop(s);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn g_final_review_native_duplicate_requires_a_live_input_proof() {
    for missing in [false, true] {
        let base = crate::tests_xmp::temp_dir(&format!("g-review-dupe-proof-{missing}"));
        let mut old = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![1024; 32 * 24], None, Default::default());
        let mut new = crate::tests_xmp::synthetic_dng_of(32, 24, "RGGB", vec![2048; 32 * 24], None, Default::default());
        let len = old.len().max(new.len());
        old.resize(len, 0);
        new.resize(len, 0);
        let original = write(&base, "original.dng", &old);
        let candidate = write(&base, "candidate.dng", &old);
        let mut s = Session::new().with_fs();
        let first = import(&mut s, &[&original], "add", &base.join("out"));
        let id = lightcraft_catalog::PhotoId(first["imported"][0].as_u64().unwrap());
        assert!(s.source_hash_authorized(id));
        let mut cached = s.originals.in_scope(|| s.media.file_probe.as_ref().unwrap()(candidate.to_str().unwrap())).unwrap();
        cached.native_read.as_ref().unwrap().release();
        if missing {
            cached.native_read = s.originals.in_scope(|| crate::originals::probe_evidence(candidate.to_str().unwrap(), &cached, None));
        }
        s.import_probes.insert(candidate.to_string_lossy().into_owned(), cached);
        let time = std::fs::metadata(&candidate).unwrap().modified().unwrap();
        let replacement = write(&base, "replacement.dng", &new);
        FileTimesFixture::restore(&replacement, time);
        std::fs::rename(&candidate, base.join("retired.dng")).unwrap();
        std::fs::rename(replacement, &candidate).unwrap();
        let opts = crate::import::ImportOptions { headers_only: false, ..Default::default() };
        let mut job = crate::import::ImportJob::new(&mut s, opts.clone()).unwrap();
        let prepared = job.prepare_files(vec![candidate.to_string_lossy().into_owned()], &std::sync::atomic::AtomicBool::new(false));
        let report = crate::import::commit_prepared(&mut s, &opts, job.now(), prepared).unwrap();
        assert!(report.duplicates.is_empty() && !report.failed.is_empty(), "unavailable native input proof must not skip a changed file: {report:?}");
        drop(job);
        let retry = import(&mut s, &[&candidate], "add", &base.join("out"));
        assert_eq!(retry["imported"].as_array().unwrap().len(), 1, "retry must discard the unusable cached observation: {retry}");
        assert!(retry["duplicates"].as_array().unwrap().is_empty());
        drop(s);
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[test]
fn g_final_review_legacy_proxy_migration_checks_same_read_physical_facts_after_reopen() {
    for changed in [false, true] {
        let base = crate::tests_xmp::temp_dir(&format!("g-review-proxy-facts-{changed}"));
        let mut old = big_dng(111);
        let samples = (0..1500usize * 2000).map(|i| 256 + ((i * 7919) % 15000) as u16).collect();
        let mut new = crate::tests_xmp::synthetic_dng_of(1500, 2000, "RGGB", samples, None, Default::default());
        let len = old.len().max(new.len());
        old.resize(len, 0);
        new.resize(len, 0);
        let original = write(&base, "original.dng", &old);
        let library = base.join("library");
        let smart_dir = crate::smart::dir(&library);
        std::fs::create_dir_all(&smart_dir).unwrap();
        crate::smart::mark_look_checked(&smart_dir).unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(&library, false).unwrap();
        let r = import(&mut s, &[&original], "add", &base.join("out"));
        let id = lightcraft_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
        let photo = s.catalog.photo(id).unwrap().clone();
        assert!(photo.content_hash.is_none());
        let alias = crate::media::content_key(&photo);
        let smart = s.media.smart_dir.clone().unwrap();
        let old_proxy = crate::smart::paths_for_keys(&smart, std::slice::from_ref(&alias))[1].clone();
        std::fs::create_dir_all(old_proxy.parent().unwrap()).unwrap();
        std::fs::write(&old_proxy, crate::smart::encode(&lightcraft_raster::Rgb32f::new(4, 3), None).unwrap()).unwrap();
        s.persist().unwrap();
        drop(s);
        let current = if changed { &new } else { &old };
        std::fs::write(&original, current).unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(&library, false).unwrap();
        assert_eq!(s.media.file_bytes.as_ref().unwrap()(original.to_str().unwrap()).unwrap(), *current);
        s.persist().unwrap();
        let hash = lightcraft_preview::hash_bytes(current).to_string();
        let p = s.catalog.photo(id).unwrap();
        assert_eq!(p.content_hash.as_deref(), Some(hash.as_str()));
        let primary = crate::smart::paths_for_keys(&smart, std::slice::from_ref(&hash))[0].clone();
        if changed {
            assert_eq!((p.width, p.height), (1500, 2000));
            assert!(!crate::smart::is_valid(&primary), "old path pixels must not be migrated under the replacement hash");
            assert!(!crate::smart::is_valid(&old_proxy));
        } else {
            assert!(crate::smart::is_valid(&primary), "unchanged legacy learning still migrates without another original pass");
        }
        assert_eq!(s.originals.counts(), (1, 1));
        drop(s);
        std::fs::remove_dir_all(base).unwrap();
    }
}
