//! Content hashes worked out after a browsed (Local) photo joins the library (`crate::hashing`):
//! browsing leaves a raw's hash unset, and Add to My Photos or an import of the file works it out,
//! moving a smart preview made under the old cache key along.

use std::path::{Path, PathBuf};

use lightcraft_catalog::{Op, PhotoId};
use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-hashing-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(p: &Path) {
    let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, 40, 255]);
    let b =
        crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() }).unwrap();
    std::fs::write(p, b).unwrap();
}

/// A browsed photo whose hash was left unset, as the light probe leaves a raw's: (session, photo, its file).
fn browsed_without_hash(tag: &str) -> (Session, PhotoId, String) {
    let dir = temp_dir(tag);
    let file = dir.join("a.png");
    png(&file);
    let mut s = Session::new().with_fs();
    s.execute("library.browse", &json!({"path": dir.to_string_lossy()})).unwrap();
    let p = s.catalog.photos().find(|p| p.local).unwrap().clone();
    s.catalog
        .apply(Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: None,
            preview_only: None,
            preview_checked: String::new(),
        })
        .unwrap();
    (s, p.id, file.to_string_lossy().to_string())
}

#[test]
fn adding_a_browsed_photo_works_out_its_content_hash() {
    let (mut s, id, file) = browsed_without_hash("add");
    s.execute("photo.addToLibrary", &json!({"ids": [id.0]})).unwrap();
    let p = s.catalog.photo(id).unwrap();
    assert!(!p.local);
    assert_eq!(p.content_hash, crate::hashing::file_hash(&file), "as an import works it out");
    assert!(!s.hashes_queued());
    // not an undo step: undoing Add to My Photos keeps the hash
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.photo(id).unwrap().content_hash.is_some());
}

#[test]
fn importing_a_browsed_file_works_out_its_content_hash() {
    let (mut s, id, file) = browsed_without_hash("import");
    s.execute("library.import", &json!({"paths": [file], "mode": "add"})).unwrap();
    let p = s.catalog.photo(id).unwrap();
    assert!(!p.local, "the browsed record joined the library");
    assert_eq!(p.content_hash, crate::hashing::file_hash(&file));
}

/// In the app the hash is worked out on a worker thread: the photo waits in the queue, and a file
/// changed meanwhile is left alone (its next probe has the say).
#[test]
fn a_queued_hash_is_recorded_only_for_the_file_it_was_worked_out_for() {
    let (mut s, id, file) = browsed_without_hash("queued");
    s.hash_in_background = true;
    s.execute("photo.addToLibrary", &json!({"ids": [id.0]})).unwrap();
    assert!(s.hashes_queued());
    assert!(s.catalog.photo(id).unwrap().content_hash.is_none());
    let jobs = s.take_hash_jobs();
    assert_eq!(jobs.len(), 1);
    let done = jobs[0].run().unwrap();
    // the photo was relinked to another file meanwhile: not recorded
    let relink = |path: String| Op::Relink { id, file_name: "a.png".into(), source: lightcraft_catalog::Source::File { path }, format: None };
    s.catalog.apply(relink(format!("{file}.moved"))).unwrap();
    assert!(!s.learn_content_hash(done.clone()));
    s.catalog.apply(relink(file.clone())).unwrap();
    assert!(s.learn_content_hash(done.clone()));
    assert_eq!(s.catalog.photo(id).unwrap().content_hash, crate::hashing::file_hash(&file));
    assert!(!s.learn_content_hash(done), "once");
    // a file whose size changed since it was queued isn't hashed
    let (mut s, id, file) = browsed_without_hash("changed");
    s.hash_in_background = true;
    s.execute("photo.addToLibrary", &json!({"ids": [id.0]})).unwrap();
    std::fs::write(&file, b"another file now").unwrap();
    assert!(s.take_hash_jobs()[0].run().is_none());
}

/// A smart preview made while the photo had no hash (named after its path and size) is moved to
/// the name its hash gives, so it isn't built again or left behind.
#[test]
fn a_smart_preview_follows_the_photo_to_its_hash() {
    let (mut s, id, _) = browsed_without_hash("smart");
    let smart = temp_dir("smart-dir");
    s.media.smart_dir = Some(smart.clone());
    let before = s.catalog.photo(id).unwrap().clone();
    let old = crate::smart::sharded_file_path(&smart, &before);
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"proxy").unwrap();
    s.execute("photo.addToLibrary", &json!({"ids": [id.0]})).unwrap();
    let after = s.catalog.photo(id).unwrap().clone();
    assert!(after.content_hash.is_some());
    let new = crate::smart::sharded_file_path(&smart, &after);
    assert_ne!(old, new);
    assert!(!old.exists());
    assert_eq!(std::fs::read(&new).unwrap(), b"proxy");
}
