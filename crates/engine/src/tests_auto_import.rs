//! Auto Import with subfolders (issue #1109): off by default (the watched folder's top level
//! only); on, its subfolders' photos too, never hidden folders, the library's own folder or
//! folder links, within bounds that end in a warning.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::Session;
use crate::cmd::library::AutoImportWalk;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-autoimport-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A small procedural PNG (distinct per seed).
fn write_png(path: &Path, seed: u8) {
    let (w, h) = (24usize, 16usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Two scans: a file is imported once its size held between them.
fn scan_twice(s: &mut Session) -> serde_json::Value {
    s.execute("library.autoImportScan", &json!({})).unwrap();
    s.execute("library.autoImportScan", &json!({})).unwrap()
}

fn names(files: &[(String, u64)], root: &Path) -> Vec<String> {
    let mut v: Vec<String> = files.iter().map(|(p, _)| Path::new(p).strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/")).collect();
    v.sort();
    v
}

#[test]
fn nested_photos_are_imported_only_with_subfolders_on() {
    let dir = temp_dir("nested");
    write_png(&dir.join("top.png"), 1);
    write_png(&dir.join("2026").join("10").join("deep.png"), 2);
    let mut s = Session::new().with_fs();
    let r = s.execute("library.autoImport", &json!({"folder": dir.to_string_lossy()})).unwrap();
    assert_eq!(r["subfolders"], false, "off by default: {r}");
    let r = scan_twice(&mut s);
    assert_eq!(r["subfolders"], false, "{r}");
    assert_eq!(s.catalog.len(), 1, "the top level only: {r}");
    for _ in 0..2 {
        assert_eq!(s.execute("library.autoImportScan", &json!({})).unwrap()["imported"], json!([]));
    }
    let r = s.execute("library.autoImport", &json!({"subfolders": true})).unwrap();
    assert_eq!(r["subfolders"], true, "{r}");
    let r = scan_twice(&mut s);
    assert_eq!(r["subfolders"], true, "{r}");
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "the nested photo: {r}");
    assert_eq!(s.catalog.len(), 2);
    // a file still growing in a subfolder waits, as at the top level
    write_png(&dir.join("2026").join("11").join("new.png"), 3);
    assert_eq!(s.execute("library.autoImportScan", &json!({})).unwrap()["imported"], json!([]));
    assert_eq!(s.execute("library.autoImportScan", &json!({})).unwrap()["imported"].as_array().map(Vec::len), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_setting_persists_with_the_library() {
    let dir = temp_dir("persist");
    let lib = dir.join("lib");
    let watched = dir.join("watched");
    std::fs::create_dir_all(&watched).unwrap();
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        assert!(!s.import_defaults.auto_subfolders, "off by default");
        s.execute("library.autoImport", &json!({"folder": watched.to_string_lossy(), "subfolders": true})).unwrap();
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert!(s.import_defaults.auto_subfolders);
    assert_eq!(s.execute("library.autoImportScan", &json!({})).unwrap()["subfolders"], true);
    // other settings leave it alone; turning it off sticks too
    let r = s.execute("library.autoImport", &json!({"copy": true})).unwrap();
    assert_eq!(r["subfolders"], true, "{r}");
    s.execute("library.autoImport", &json!({"subfolders": false})).unwrap();
    drop(s);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert!(!s.import_defaults.auto_subfolders);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Hidden folders and the library's own folder (where copy-mode arrivals go) are never listed,
/// so copies are never imported again.
#[test]
fn hidden_folders_and_the_library_are_skipped() {
    let dir = temp_dir("skip");
    let lib = dir.join("Library");
    write_png(&dir.join("shoot").join("a.png"), 1);
    write_png(&dir.join(".cache").join("h.png"), 2);
    write_png(&dir.join("shoot").join(".h.png"), 3);
    std::fs::write(dir.join("shoot").join("notes.txt"), "not a photo").unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.autoImport", &json!({"folder": dir.to_string_lossy(), "copy": true, "subfolders": true})).unwrap();
    let r = scan_twice(&mut s);
    assert_eq!(r["imported"].as_array().map(Vec::len), Some(1), "{r}");
    assert!(lib.join("Originals").is_dir(), "copied into the library inside the watched folder");
    let listed = AutoImportWalk::of(&s).unwrap().list().unwrap();
    assert_eq!(names(&listed.files, &dir), ["shoot/a.png"], "{listed:?}");
    assert!(listed.warnings.is_empty(), "{listed:?}");
    for _ in 0..3 {
        assert_eq!(s.execute("library.autoImportScan", &json!({})).unwrap()["imported"], json!([]));
    }
    assert_eq!(s.catalog.len(), 1, "the copy isn't imported again");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The depth and entry bounds hold, each with a warning, so a huge tree can't stall the scan.
#[test]
fn bounds_stop_the_listing_with_a_warning() {
    let dir = temp_dir("bounds");
    let mut deep = dir.clone();
    for i in 0..5 {
        deep = deep.join(format!("l{i}"));
        write_png(&deep.join(format!("p{i}.png")), i);
    }
    let mut w = AutoImportWalk::new(&dir.to_string_lossy(), true, Vec::new());
    let all = w.list().unwrap();
    assert_eq!(all.files.len(), 5);
    assert!(all.warnings.is_empty(), "{all:?}");
    w.max_depth = 3;
    let l = w.list().unwrap();
    assert_eq!(names(&l.files, &dir), ["l0/l1/p1.png", "l0/p0.png"], "three levels of entries listed");
    assert!(l.warnings.iter().any(|w| w.contains("3 folder levels")), "{l:?}");
    w.max_depth = 16;
    w.max_entries = 4;
    let l = w.list().unwrap();
    assert!(l.files.len() < 5, "{l:?}");
    assert!(l.warnings.iter().any(|w| w.contains("after 4 files and folders")), "{l:?}");
    // the warnings reach the scan's result
    let mut s = Session::new().with_fs();
    s.execute("library.autoImport", &json!({"folder": dir.to_string_lossy(), "subfolders": true})).unwrap();
    let r = s.execute("library.autoImportScan", &json!({"listing": [], "listingWarnings": ["stopped"]})).unwrap();
    assert_eq!(r["listingWarnings"], json!(["stopped"]), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The default bounds: 16 levels, 200,000 entries.
#[test]
fn default_bounds() {
    let w = AutoImportWalk::new("x", true, Vec::new());
    assert_eq!((w.max_depth, w.max_entries), (16, 200_000));
}

/// A watched folder that's gone is an error for the scan (as before); a subfolder that can't be
/// read is skipped with a warning.
#[test]
fn a_missing_folder_errs_and_an_unreadable_subfolder_warns() {
    let dir = temp_dir("unreadable");
    let w = AutoImportWalk::new(&dir.join("gone").to_string_lossy(), true, Vec::new());
    assert!(w.list().is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        write_png(&dir.join("ok").join("a.png"), 1);
        let locked = dir.join("locked");
        write_png(&locked.join("b.png"), 2);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let l = AutoImportWalk::new(&dir.to_string_lossy(), true, Vec::new()).list().unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        // (root reads it anyway)
        if !names(&l.files, &dir).contains(&"locked/b.png".to_string()) {
            assert_eq!(names(&l.files, &dir), ["ok/a.png"]);
            assert!(l.warnings.iter().any(|w| w.contains("locked")), "{l:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Links to folders aren't followed (a link back up would loop); links to files are files.
#[cfg(unix)]
#[test]
fn folder_links_are_not_followed() {
    let dir = temp_dir("links");
    let outside = temp_dir("links-outside");
    write_png(&outside.join("o.png"), 1);
    write_png(&dir.join("sub").join("a.png"), 2);
    std::os::unix::fs::symlink(&outside, dir.join("sub").join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(&dir, dir.join("sub").join("loop")).unwrap();
    std::os::unix::fs::symlink(outside.join("o.png"), dir.join("sub").join("linked.png")).unwrap();
    let l = AutoImportWalk::new(&dir.to_string_lossy(), true, Vec::new()).list().unwrap();
    assert_eq!(names(&l.files, &dir), ["sub/a.png", "sub/linked.png"], "{l:?}");
    assert!(l.warnings.is_empty(), "{l:?}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

/// A name that isn't valid Unicode is skipped with a warning, not a crash or a mangled path.
#[cfg(target_os = "linux")]
#[test]
fn non_unicode_names_are_skipped() {
    use std::os::unix::ffi::OsStrExt;
    let dir = temp_dir("names");
    write_png(&dir.join("sub").join("a.png"), 1);
    let odd = dir.join("sub").join(std::ffi::OsStr::from_bytes(b"bad\xff.png"));
    if std::fs::write(&odd, b"x").is_err() {
        return; // a file system that refuses such names
    }
    let l = AutoImportWalk::new(&dir.to_string_lossy(), true, Vec::new()).list().unwrap();
    assert_eq!(names(&l.files, &dir), ["sub/a.png"], "{l:?}");
    assert!(l.warnings.iter().any(|w| w.contains("valid Unicode")), "{l:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
