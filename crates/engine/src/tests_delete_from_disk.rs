//! Delete from Disk (`photo.deleteFromDisk`, issue #1706): the files go to the Trash (here a temp
//! folder, never the real one) with their sidecars, the photos leave the library, and a file that
//! can't be moved stays, with its photo.
// The tests that move files to a trash run where Delete from Disk is on (`trash::available`).
#![cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};

use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::Session;
use crate::trash::TrashRoot;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-delete-disk-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(p: &Path, seed: u8) {
    let img = lightcraft_raster::Rgba8::from_fn(16, 12, |x, y| [(x * 9) as u8, (y * 11) as u8, seed, 255]);
    let b =
        crate::export::encode_image(&img, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() }).unwrap();
    std::fs::write(p, b).unwrap();
}

/// Import `files` (each a png made here) from `dir`; the session trashes into `<dir>/../bin`.
fn imported(dir: &Path, files: &[&str]) -> (Session, Vec<PhotoId>, PathBuf) {
    for (i, n) in files.iter().enumerate() {
        png(&dir.join(n), i as u8 * 40);
    }
    let mut s = Session::new().with_fs();
    let bin = dir.with_file_name(format!("{}-bin", dir.file_name().unwrap().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&bin);
    s.trash = TrashRoot::Folder(bin.clone());
    let paths: Vec<String> = files.iter().map(|n| dir.join(n).to_string_lossy().to_string()).collect();
    let r = s.execute("library.import", &json!({"paths": paths, "mode": "add"})).unwrap();
    let mut ids: Vec<PhotoId> = r["imported"].as_array().unwrap().iter().filter_map(|v| v.as_u64()).map(PhotoId).collect();
    ids.sort();
    assert_eq!(ids.len(), files.len(), "{r}");
    (s, ids, bin)
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn delete_from_disk_moves_the_file_and_its_sidecars_to_the_trash() {
    let dir = temp_dir("move");
    std::fs::write(dir.join("a.xmp"), "<x/>").unwrap();
    std::fs::write(dir.join("a.png.xmp"), "<x/>").unwrap();
    let (mut s, ids, bin) = imported(&dir, &["a.png", "b.png"]);
    let (a, b) = (ids[0], ids[1]);
    let vc = PhotoId(s.execute("photo.virtualCopy", &json!({"ids": [a.0]})).unwrap()["ids"][0].as_u64().unwrap());
    assert!(s.catalog.photo(vc).is_some());

    let r = s.execute("photo.deleteFromDisk", &json!({"ids": [a.0]})).unwrap();
    assert_eq!(r["deleted"], 2, "the photo and its virtual copy: {r}");
    assert_eq!(r["trashed"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["sidecars"].as_array().unwrap().len(), 2, "both sidecars: {r}");
    assert_eq!(r["failed"].as_array().unwrap().len(), 0, "{r}");
    for n in ["a.png", "a.xmp", "a.png.xmp"] {
        assert!(!dir.join(n).exists(), "{n} left its folder");
        assert!(bin.join("files").join(n).is_file(), "{n} is in the Trash");
        assert!(bin.join("info").join(format!("{n}.trashinfo")).is_file(), "{n} can be restored");
    }
    assert!(dir.join("b.png").is_file(), "the other photo's file stays");
    assert!(s.catalog.photo(a).is_none() && s.catalog.photo(vc).is_none());
    assert!(s.catalog.photo(b).is_some());

    // undo brings the photo back to the library, not its file
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.photo(a).is_some());
    assert!(!dir.join("a.png").exists());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&bin);
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn a_stem_sidecar_another_photo_shares_stays() {
    let dir = temp_dir("stem");
    std::fs::write(dir.join("a.xmp"), "<x/>").unwrap();
    // IMG a.jpg beside it shares `a.xmp` (as a raw + JPEG pair would), and isn't deleted
    let (mut s, ids, bin) = imported(&dir, &["a.png", "a.jpg"]);
    let r = s.execute("photo.deleteFromDisk", &json!({"ids": [ids[0].0]})).unwrap();
    assert_eq!(r["deleted"], 1, "{r}");
    assert_eq!(r["sidecars"].as_array().unwrap().len(), 0, "{r}");
    assert!(dir.join("a.xmp").is_file(), "the other photo still uses it");
    assert!(dir.join("a.jpg").is_file());
    // the pair together: everything goes
    let left = s.catalog.photos().map(|p| p.id.0).collect::<Vec<_>>();
    let r = s.execute("photo.deleteFromDisk", &json!({"ids": left})).unwrap();
    assert_eq!(r["sidecars"].as_array().unwrap().len(), 1, "{r}");
    assert!(!dir.join("a.xmp").exists());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&bin);
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn a_file_that_cant_go_to_the_trash_stays_with_its_photo() {
    let dir = temp_dir("fail");
    let (mut s, ids, bin) = imported(&dir, &["a.png"]);
    // the "Trash" is a file: nothing can be moved into it
    std::fs::write(&bin, b"").unwrap();
    let steps = s.undo.len();
    let e = s.execute("photo.deleteFromDisk", &json!({"ids": [ids[0].0]})).unwrap_err().to_string();
    assert!(e.contains("not deleted"), "{e}");
    assert!(dir.join("a.png").is_file(), "the file is untouched");
    assert!(s.catalog.photo(ids[0]).is_some(), "and still in the library");
    assert_eq!(s.undo.len(), steps, "nothing to undo");
    let _ = std::fs::remove_file(&bin);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn a_file_already_gone_is_only_taken_out_of_the_library() {
    let dir = temp_dir("gone");
    let (mut s, ids, bin) = imported(&dir, &["a.png"]);
    std::fs::remove_file(dir.join("a.png")).unwrap();
    let r = s.execute("photo.deleteFromDisk", &json!({"ids": [ids[0].0]})).unwrap();
    assert_eq!((r["deleted"].as_u64(), r["missing"].as_u64()), (Some(1), Some(1)), "{r}");
    assert!(s.catalog.photo(ids[0]).is_none());
    assert!(!bin.join("files").exists() || std::fs::read_dir(bin.join("files")).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&bin);
}

#[test]
fn delete_from_disk_wants_photos() {
    let mut s = Session::new();
    s.trash = TrashRoot::Folder(std::env::temp_dir().join("lc-delete-disk-never-used"));
    assert!(s.execute("photo.deleteFromDisk", &json!({"ids": []})).is_err());
    assert!(s.execute("photo.deleteFromDisk", &json!({"ids": [12345]})).is_err());
}
