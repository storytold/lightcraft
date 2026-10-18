use std::path::{Path, PathBuf};

use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-import-dupes-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn bytes(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| (i.wrapping_mul(31) as u8) ^ seed).collect()
}

fn write(dir: &Path, name: &str, data: &[u8]) -> String {
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p.to_string_lossy().into_owned()
}

fn header_session() -> Session {
    let mut s = Session::new();
    s.media.file_probe = Some(std::sync::Arc::new(|path: &str| {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        Ok(ProbeInfo {
            format: "DNG".into(),
            kind: MediaKind::Raw,
            width: 6,
            height: 4,
            file_size: bytes.len() as u64,
            head_hash: Some(crate::files::head_hash(&bytes)),
            ..Default::default()
        })
    }));
    s
}

fn hashing_probe(s: &mut Session) {
    s.media.file_probe = Some(std::sync::Arc::new(|path: &str| {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        Ok(ProbeInfo {
            format: "DNG".into(),
            kind: MediaKind::Raw,
            width: 6,
            height: 4,
            file_size: bytes.len() as u64,
            content_hash: Some(lightcraft_preview::hash_bytes(&bytes).to_string()),
            head_hash: Some(crate::files::head_hash(&bytes)),
            ..Default::default()
        })
    }));
}

fn add(s: &mut Session, paths: &[String]) -> ImportReport {
    import(s, paths, ImportMode::Add).unwrap()
}

const BIG: usize = 600 << 10;

#[test]
fn a_copy_of_a_header_probed_photo_is_found_when_both_have_no_hash() {
    let dir = temp_dir("both");
    let data = bytes(1, BIG);
    let orig = write(&dir, "orig.dng", &data);
    let copy = write(&dir, "copy.dng", &data);
    let mut s = header_session();
    let first = add(&mut s, std::slice::from_ref(&orig));
    let id = PhotoId(first.imported[0]);
    assert_eq!(s.catalog.photo(id).and_then(|p| p.content_hash.clone()), None, "added from its headers");
    let scanned = scan(&mut s, std::slice::from_ref(&copy));
    assert_eq!((scanned[0].duplicate.as_deref(), scanned[0].existing), (Some("content"), Some(id.0)), "the review marks it");
    let second = add(&mut s, std::slice::from_ref(&copy));
    assert!(second.imported.is_empty(), "{second:?}");
    assert_eq!((second.duplicates[0].existing, second.duplicates[0].reason), (Some(id.0), "content"));
    let hash = lightcraft_preview::hash_bytes(&data).to_string();
    assert_eq!(s.catalog.photo(id).and_then(|p| p.content_hash.clone()), Some(hash), "the hash it took is kept");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn same_size_photos_with_different_heads_are_not_hashed_in_full() {
    let dir = temp_dir("heads");
    let orig = write(&dir, "orig.dng", &bytes(1, BIG));
    let other_head = write(&dir, "other-head.dng", &bytes(2, BIG));
    let mut same_head = bytes(1, BIG);
    if let Some(last) = same_head.last_mut() {
        *last ^= 0xff;
    }
    let other_tail = write(&dir, "other-tail.dng", &same_head);
    let mut s = header_session();
    add(&mut s, std::slice::from_ref(&orig));
    reset_full_hashes();
    let r = add(&mut s, std::slice::from_ref(&other_head));
    assert_eq!((r.imported.len(), r.duplicates.len()), (1, 0), "{r:?}");
    assert_eq!(full_hashes(), 0, "a different head settles it");
    let r = add(&mut s, std::slice::from_ref(&other_tail));
    assert_eq!((r.imported.len(), r.duplicates.len()), (1, 0), "the same head and other bytes after it: a new photo");
    assert_eq!(full_hashes(), 2, "only a matching head reads both files whole");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn more_than_sixteen_same_size_photos_are_still_compared() {
    let dir = temp_dir("many");
    let paths: Vec<String> = (0..20u8).map(|i| write(&dir, &format!("f{i}.dng"), &bytes(i, BIG))).collect();
    let mut s = header_session();
    let r = add(&mut s, &paths);
    assert_eq!(r.imported.len(), 20, "{r:?}");
    let copy = write(&dir, "copy-of-last.dng", &bytes(19, BIG));
    let r = add(&mut s, &[copy]);
    assert_eq!(r.duplicates.len(), 1, "{r:?}");
    let last = s.catalog.photos().find(|p| matches!(&p.source, Source::File { path } if path.ends_with("f19.dng"))).map(|p| p.id.0);
    assert_eq!(r.duplicates[0].existing, last);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn two_copies_in_one_hashless_import_keep_the_first() {
    let dir = temp_dir("twins");
    let data = bytes(3, BIG);
    let a = write(&dir, "a.dng", &data);
    let b = write(&dir, "b.dng", &data);
    let mut s = header_session();
    let r = add(&mut s, &[a.clone(), b]);
    assert_eq!(r.imported.len(), 1, "{r:?}");
    assert_eq!((r.duplicates.len(), r.duplicates[0].reason), (1, "content"));
    assert_eq!(r.duplicates[0].existing, Some(r.imported[0]), "the copy names the photo it repeats");
    let kept = s.catalog.photo(PhotoId(r.imported[0])).unwrap();
    assert!(matches!(&kept.source, Source::File { path } if *path == a));
    assert_eq!(kept.content_hash, Some(lightcraft_preview::hash_bytes(&data).to_string()), "read whole to compare: its hash is kept");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_dup_suffixed_hash_still_matches_its_base() {
    let dir = temp_dir("suffix");
    let data = bytes(4, 2048);
    let orig = write(&dir, "orig.dng", &data);
    let copy = write(&dir, "copy.dng", &data);
    let mut s = Session::new();
    hashing_probe(&mut s);
    let mut p = Photo::new(s.catalog.alloc_photo_id(), Source::File { path: orig }, "orig.dng", "DNG", 6, 4, "now");
    p.kind = MediaKind::Raw;
    p.file_size = data.len() as u64;
    p.content_hash = Some(format!("{}:dup7", lightcraft_preview::hash_bytes(&data)));
    let id = p.id;
    s.commit("existing", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let r = add(&mut s, std::slice::from_ref(&copy));
    assert_eq!(r.duplicates.len(), 1, "{r:?}");
    assert_eq!((r.duplicates[0].existing, r.duplicates[0].reason), (Some(id.0), "content"));
    let scanned = scan(&mut s, &[copy]);
    assert_eq!(scanned[0].existing, Some(id.0));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_unreadable_candidate_is_not_a_duplicate_and_does_not_fail_the_import() {
    let dir = temp_dir("gone");
    let data = bytes(5, BIG);
    let orig = write(&dir, "orig.dng", &data);
    let mut s = header_session();
    add(&mut s, std::slice::from_ref(&orig));
    std::fs::remove_file(&orig).unwrap();
    let copy = write(&dir, "copy.dng", &data);
    let r = add(&mut s, &[copy]);
    assert_eq!((r.imported.len(), r.duplicates.len(), r.failed.len()), (1, 0, 0), "{r:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_later_batch_learns_the_committed_photos_hash_and_existing_id() {
    let dir = temp_dir("later-hash");
    let data = bytes(9, BIG);
    let first = write(&dir, "a.dng", &data);
    let second = write(&dir, "b.dng", &data);
    let mut s = header_session();
    s.open_library(dir.join("library"), false).unwrap();
    let opts = ImportOptions { mode: ImportMode::Add, ..Default::default() };
    let mut job = ImportJob::new(&mut s, opts.clone()).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let ready = job.prepare_files(vec![first], &cancel);
    let a = commit_prepared(&mut s, &opts, &job.now, ready).unwrap();
    let id = PhotoId(a.imported[0]);
    assert!(s.catalog.photo(id).unwrap().content_hash.is_none());
    let ready = job.prepare_files(vec![second], &cancel);
    let b = commit_prepared(&mut s, &opts, &job.now, ready).unwrap();
    assert!(b.imported.is_empty());
    assert_eq!(b.duplicates[0].existing, Some(id.0));
    let expected = lightcraft_preview::hash_bytes(&data).to_string();
    assert_eq!(s.catalog.photo(id).unwrap().content_hash.as_ref(), Some(&expected));
    s.close_library().unwrap();
    s.open_library(dir.join("library"), false).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().content_hash.as_ref(), Some(&expected));
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_later_move_batch_compares_the_surviving_destination() {
    let dir = temp_dir("later-move");
    let data = bytes(10, BIG);
    let first = write(&dir, "a.dng", &data);
    let second = write(&dir, "b.dng", &data);
    let mut s = header_session();
    let opts = ImportOptions { mode: ImportMode::Move, destination: Some(dir.join("dest").to_string_lossy().into_owned()), ..Default::default() };
    let mut job = ImportJob::new(&mut s, opts.clone()).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let ready = job.prepare_files(vec![first.clone()], &cancel);
    let a = commit_prepared(&mut s, &opts, &job.now, ready).unwrap();
    assert!(!Path::new(&first).exists());
    let ready = job.prepare_files(vec![second.clone()], &cancel);
    let b = commit_prepared(&mut s, &opts, &job.now, ready).unwrap();
    assert!(b.imported.is_empty(), "{b:?}");
    assert_eq!(b.duplicates[0].existing, Some(a.imported[0]));
    assert!(Path::new(&second).exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn add_registers_distinct_candidates_once_across_batches() {
    let dir = temp_dir("distinct-cap");
    let paths: Vec<_> = (0..129u8).map(|i| write(&dir, &format!("f{i}.dng"), &bytes(i, BIG))).collect();
    let copy = write(&dir, "copy.dng", &bytes(0, BIG));
    let mut s = header_session();
    let opts = ImportOptions { mode: ImportMode::Add, ..Default::default() };
    let mut job = ImportJob::new(&mut s, opts.clone()).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let ready = job.prepare_files(paths, &cancel);
    let a = commit_prepared(&mut s, &opts, &job.now, ready).unwrap();
    assert_eq!(a.imported.len(), 129);
    let ready = job.prepare_files(vec![copy], &cancel);
    let b = commit_prepared(&mut s, &opts, &job.now, ready).unwrap();
    assert!(b.imported.is_empty(), "{b:?}");
    assert_eq!(b.duplicates[0].existing, Some(a.imported[0]));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn f_repair_undo_import_learning_preserves_existing_photo_sources_and_faces() {
    use crate::media::SourceLevel;
    use std::sync::Arc;
    let dir = temp_dir("undo-learning");
    let data = bytes(7, BIG);
    let original = write(&dir, "original.dng", &data);
    let duplicate = write(&dir, "duplicate.dng", &data);
    let mut s = header_session();
    let id = PhotoId(add(&mut s, &[original]).imported[0]);
    let decoded = Arc::new(lightcraft_raster::Rgb32f::new(6, 4));
    s.media.insert(id, SourceLevel::Preview, decoded.clone());
    s.media.smart_dir = Some(dir.join("smart"));
    let proxy = s.smart_preview_paths(s.catalog.photo(id).unwrap())[0].clone();
    std::fs::create_dir_all(proxy.parent().unwrap()).unwrap();
    std::fs::write(&proxy, crate::smart::encode(&decoded, None).unwrap()).unwrap();
    s.media.file_loader = Some(Arc::new(|_, _| Err("must retain decoded original".into())));
    s.faces.scanned.insert(id.0);
    let ticket = s.media.identities.ticket(s.catalog.photo(id).unwrap());
    s.execute("library.select", &serde_json::json!({"ids": [id.0]})).unwrap();
    s.execute("photo.rate", &serde_json::json!({"rating": 3})).unwrap();
    let steps = s.undo.len();
    add(&mut s, &[duplicate]);
    assert_eq!(s.undo.len(), steps, "duplicate comparison learning is system metadata");
    assert!(s.catalog.photo(id).unwrap().content_hash.is_some());
    s.undo_step().unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().rating, 0);
    assert!(s.catalog.photo(id).unwrap().content_hash.is_some());
    assert!(s.media.has_source(id, SourceLevel::Preview));
    assert!(s.faces.scanned.contains(id.0));
    assert!(ticket.current());
    assert!(Arc::ptr_eq(&s.source_now(id, SourceLevel::Preview).unwrap(), &decoded));
    assert!(s.smart_preview_paths(s.catalog.photo(id).unwrap()).contains(&proxy));
    assert!(crate::smart::load(&proxy).is_ok());
    s.redo_step().unwrap();
    assert!(s.media.has_source(id, SourceLevel::Preview));
    assert!(s.faces.scanned.contains(id.0));
    assert!(ticket.current());
    assert!(Arc::ptr_eq(&s.source_now(id, SourceLevel::Preview).unwrap(), &decoded));
    assert!(s.smart_preview_paths(s.catalog.photo(id).unwrap()).contains(&proxy));
    assert!(crate::smart::load(&proxy).is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}
