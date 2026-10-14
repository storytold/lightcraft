//! Issue #891: Open Library given a Lightroom Classic catalog (its folder, `.lrdata` /
//! `.lrcat-data` folders, the `.lrcat` or its companion files) refuses it, creating nothing, and
//! names the catalog to import; a LightCraft library still opens, whatever else its folder holds.

use std::path::{Path, PathBuf};

use crate::lightroom_folder::{LightroomPlace, SCAN_LIMIT, lightroom_catalog_at};
use crate::{EngineError, Session};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-engine-lrfolder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A Lightroom catalog folder as Lightroom Classic leaves it (fake contents: only names count).
fn lightroom_folder(root: &Path) -> PathBuf {
    let dir = root.join("Lightroom");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Lightroom Catalog.lrcat"), b"not really SQLite").unwrap();
    for f in ["Lightroom Catalog.lrcat-wal", "Lightroom Catalog.lrcat-shm", "Lightroom Catalog.lrcat.lock"] {
        std::fs::write(dir.join(f), b"").unwrap();
    }
    for d in [
        "Lightroom Catalog Previews.lrdata",
        "Lightroom Catalog Smart Previews.lrdata",
        "Lightroom Catalog Helper.lrdata",
        "Lightroom Catalog.lrcat-data",
        "Backups",
    ] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    dir
}

fn names(dir: &Path) -> Vec<std::ffi::OsString> {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    v.sort();
    v
}

/// Refused with the catalog named, and nothing created or changed anywhere in `root`.
fn refused(root: &Path, path: &Path, catalog: &Path) {
    let before = names(&root.join("Lightroom"));
    let mut s = Session::new();
    match s.open_library(path, true) {
        Err(EngineError::LightroomCatalog { path: p, catalog: c }) => {
            assert_eq!(p, path);
            assert_eq!(c.as_deref(), Some(catalog), "{}", path.display());
        }
        other => panic!("{}: {other:?}", path.display()),
    }
    assert!(s.library.is_none());
    assert_eq!(names(&root.join("Lightroom")), before, "nothing written next to the catalog");
    if path.is_dir() {
        assert!(!path.join("catalog.snap").exists() && !path.join("catalog.lock").exists(), "no library created in {}", path.display());
    }
}

#[test]
fn each_part_of_a_lightroom_catalog_is_refused() {
    let root = temp_dir("parts");
    let dir = lightroom_folder(&root);
    let catalog = dir.join("Lightroom Catalog.lrcat");
    for p in [
        dir.clone(),
        catalog.clone(),
        dir.join("Lightroom Catalog Previews.lrdata"),
        dir.join("Lightroom Catalog Smart Previews.lrdata"),
        dir.join("Lightroom Catalog Helper.lrdata"),
        dir.join("Lightroom Catalog.lrcat-data"),
        dir.join("Lightroom Catalog.lrcat-wal"),
        dir.join("Lightroom Catalog.lrcat-shm"),
        dir.join("Lightroom Catalog.lrcat.lock"),
        dir.join("Backups"),
    ] {
        refused(&root, &p, &catalog);
    }
    let e = Session::new().open_library(&dir, false).unwrap_err().to_string();
    assert!(e.contains("Lightroom Classic catalog, not a LightCraft library") && e.contains("Import Lightroom Catalog"), "{e}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_lrdata_folder_whose_catalog_was_renamed_finds_the_one_next_to_it() {
    let root = temp_dir("renamed");
    let dir = root.join("Lightroom");
    std::fs::create_dir_all(dir.join("Old Name Previews.lrdata")).unwrap();
    std::fs::write(dir.join("New Name.lrcat"), b"x").unwrap();
    refused(&root, &dir.join("Old Name Previews.lrdata"), &dir.join("New Name.lrcat"));
    // no catalog anywhere: still refused (it is Lightroom's data), with nothing to import
    let lone = root.join("lone");
    std::fs::create_dir_all(lone.join("Gone Previews.lrdata")).unwrap();
    assert_eq!(lightroom_catalog_at(&lone.join("Gone Previews.lrdata")), Some(LightroomPlace { catalog: None }));
    assert_eq!(lightroom_catalog_at(&lone.join("Gone.lrcat")), Some(LightroomPlace { catalog: None }), "a missing .lrcat path");
    assert!(!lone.join("Gone.lrcat").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_empty_folder_still_becomes_a_library() {
    let root = temp_dir("empty");
    let lib = root.join("New Library");
    assert_eq!(lightroom_catalog_at(&lib), None, "not there yet");
    let mut s = Session::new();
    s.open_library(&lib, false).unwrap();
    assert!(s.library.is_some());
    // a folder with other files (and a folder named like a catalog, which is not one) too
    let other = root.join("Other");
    std::fs::create_dir_all(other.join("weird.lrcat")).unwrap();
    std::fs::write(other.join("notes.txt"), b"x").unwrap();
    assert_eq!(lightroom_catalog_at(&other), None);
    let _ = std::fs::remove_dir_all(&root);
}

/// Precedence: a LightCraft library (its catalog journal) opens, even with an `.lrcat` beside it.
#[test]
fn a_lightcraft_library_with_a_catalog_inside_still_opens() {
    let root = temp_dir("both");
    let lib = root.join("Lightroom");
    {
        let mut s = Session::new();
        s.open_library(&lib, true).unwrap();
        assert!(!s.catalog.is_empty());
        s.close_library().unwrap();
    }
    std::fs::write(lib.join("Lightroom Catalog.lrcat"), b"x").unwrap();
    std::fs::create_dir_all(lib.join("Lightroom Catalog Previews.lrdata")).unwrap();
    assert_eq!(lightroom_catalog_at(&lib), None);
    let mut s = Session::new();
    s.open_library(&lib, false).unwrap();
    assert!(!s.catalog.is_empty(), "the LightCraft library opened");
    let _ = std::fs::remove_dir_all(&root);
}

/// Never crash, never scan without end: a huge folder, unreadable paths, odd names.
#[test]
fn odd_folders_never_fail_the_check() {
    let root = temp_dir("odd");
    // the catalog after the scan limit isn't looked for (sorted names put it last on most file systems; either answer is fine, the check returns)
    let big = root.join("big");
    std::fs::create_dir_all(&big).unwrap();
    for i in 0..SCAN_LIMIT + 10 {
        std::fs::write(big.join(format!("{i:06}.jpg")), b"").unwrap();
    }
    let _ = lightroom_catalog_at(&big);
    assert_eq!(lightroom_catalog_at(&root.join("missing/deeper")), None);
    assert_eq!(lightroom_catalog_at(Path::new("")), None);
    assert_eq!(lightroom_catalog_at(Path::new("/")), None);
    assert!(lightroom_catalog_at(Path::new("x.LRCAT")).is_some(), "case-insensitive");
    // a one-letter name with a multibyte character never cuts inside it
    let _ = lightroom_catalog_at(&root.join("é"));
    let _ = lightroom_catalog_at(&root.join("ataé-data"));
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        // APFS (macOS) refuses names that are not valid UTF-8 (EILSEQ): there the non-UTF-8 cases are skipped and
        // the symlink cases below still run on a plain-named catalog folder
        let probe = root.join(std::ffi::OsStr::from_bytes(b"\xff"));
        let utf8_only = std::fs::create_dir(&probe).is_err();
        let _ = std::fs::remove_dir(&probe);
        if !utf8_only {
            let bad = root.join(std::ffi::OsStr::from_bytes(b"caf\xe9 Previews.lrdata"));
            std::fs::create_dir_all(&bad).unwrap();
            std::fs::write(root.join("Cat.lrcat"), b"x").unwrap();
            assert_eq!(lightroom_catalog_at(&bad), Some(LightroomPlace { catalog: Some(root.join("Cat.lrcat")) }), "non-UTF-8 name");
        }
        let lr = root.join(if utf8_only { std::ffi::OsStr::new("lr") } else { std::ffi::OsStr::from_bytes(b"\xff\xfe") });
        std::fs::create_dir_all(&lr).unwrap();
        if !utf8_only {
            std::fs::write(lr.join(std::ffi::OsStr::from_bytes(b"\xff.lrcat")), b"x").unwrap();
        }
        std::fs::write(lr.join("b.lrcat"), b"x").unwrap();
        assert_eq!(lightroom_catalog_at(&lr), Some(LightroomPlace { catalog: Some(lr.join("b.lrcat")) }), "non-UTF-8 entries skipped");
        // a symlink to a catalog folder is one too; a dangling one is not
        std::os::unix::fs::symlink(&lr, root.join("link")).unwrap();
        assert!(lightroom_catalog_at(&root.join("link")).is_some());
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();
        assert_eq!(lightroom_catalog_at(&root.join("dangling")), None);
    }
    let _ = std::fs::remove_dir_all(&root);
}
