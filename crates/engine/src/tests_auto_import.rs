//! Auto Import's settings: subfolders, file naming, presets and keywords for what arrives.
//!
//! Scenarios:
//! - Given a watched folder with photos in subfolders, hidden folders and a loop, when Include
//!   Subfolders is on, then the subfolder photos are listed, hidden and library folders are not,
//!   the walk ends, and off lists only the top level.
//! - Given settings, when a scan finds a new file, then the import gets the naming (copies only),
//!   presets and keywords; a preset that vanished is left out, not a failed scan.
//! - Given prefs written before these settings existed, then they load with everything off.

use std::path::Path;

use serde_json::json;

use crate::Session;
use crate::cmd::library::AutoImportScope;

/// The test's folder in Temp, removed when the test ends.
fn tmp(tag: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-autoimport-{tag}"))
}

fn put(dir: &Path, name: &str) {
    let p = dir.join(name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b"data").unwrap();
}

fn names(dir: &Path, listed: Vec<(String, u64)>) -> Vec<String> {
    let mut v: Vec<String> = listed.into_iter().map(|(p, _)| Path::new(&p).strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/")).collect();
    v.sort();
    v
}

#[test]
fn subfolders_are_listed_only_when_asked_and_hidden_and_library_folders_are_skipped() {
    let dir = tmp("list");
    put(&dir, "top.jpg");
    put(&dir, "notes.txt");
    put(&dir, "day1/a.jpg");
    put(&dir, "day1/readme.txt");
    put(&dir, "day1/deep/b.png");
    put(&dir, ".hidden/c.jpg");
    put(&dir, "Library/Originals/d.jpg");
    let scope = |subfolders| AutoImportScope { folder: dir.to_string_lossy().to_string(), subfolders, skip: Some(dir.join("Library")) };
    assert_eq!(names(&dir, scope(false).list().unwrap()), ["notes.txt", "top.jpg"]);
    assert_eq!(names(&dir, scope(true).list().unwrap()), ["day1/a.jpg", "day1/deep/b.png", "notes.txt", "top.jpg"]);

    // a folder link loop ends
    #[cfg(unix)]
    {
        let dir = tmp("loop");
        put(&dir, "sub/a.jpg");
        std::os::unix::fs::symlink(&*dir, dir.join("sub/back")).unwrap();
        let scope = AutoImportScope { folder: dir.to_string_lossy().to_string(), subfolders: true, skip: None };
        assert_eq!(names(&dir, scope.list().unwrap()), ["sub/a.jpg"], "link loop");
    }

    // a missing watched folder is an error, not an empty listing
    let scope = AutoImportScope { folder: dir.join("nope").to_string_lossy().to_string(), subfolders: true, skip: None };
    assert!(scope.list().is_err());

    // nesting deeper than the limit is cut off
    let dir = tmp("deep");
    let mut sub = String::new();
    for i in 0..(crate::import::AUTO_IMPORT_DEPTH + 4) {
        sub.push_str(&format!("d{i}/"));
    }
    put(&dir, &format!("{sub}far.jpg"));
    put(&dir, "d0/near.jpg");
    let scope = AutoImportScope { folder: dir.to_string_lossy().to_string(), subfolders: true, skip: None };
    assert_eq!(names(&dir, scope.list().unwrap()), ["d0/near.jpg"], "depth limit");
}

fn watching(dir: &Path) -> Session {
    let mut s = Session::with_demo();
    s.execute("library.autoImport", &json!({"folder": dir.to_string_lossy()})).unwrap();
    s
}

/// Scan twice (a file must stop growing) and return the import the second scan hands out.
fn scan(s: &mut Session) -> Option<serde_json::Value> {
    s.execute("library.autoImportScan", &json!({"start": false})).unwrap();
    s.execute("library.autoImportScan", &json!({"start": false})).unwrap().get("import").cloned()
}

#[test]
fn the_import_carries_the_naming_presets_and_keywords() {
    let dir = tmp("scan");
    put(&dir, "day1/a.jpg");
    let mut s = watching(&dir);
    let preset = s.presets.first().unwrap().id.clone();
    let r = s
        .execute(
            "library.autoImport",
            &json!({"copy": true, "subfolders": true, "rename": "{date}_{seq:3}", "preset": preset, "keywords": "Tethered, Studio, tethered"}),
        )
        .unwrap();
    assert_eq!((r["subfolders"].as_bool(), r["keywords"].clone()), (Some(true), json!(["Tethered", "Studio"])), "{r}");
    let import = scan(&mut s).expect("the nested photo is imported");
    assert_eq!(import["rename"], json!("{date}_{seq:3}"));
    assert_eq!(import["preset"], json!(preset));
    assert_eq!(import["keywords"], json!(["Tethered", "Studio"]));
    assert_eq!(import["paths"].as_array().map(Vec::len), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn naming_applies_to_copies_only_and_nested_photos_wait_for_the_option() {
    let dir = tmp("add");
    put(&dir, "day1/a.jpg");
    let mut s = watching(&dir);
    s.execute("library.autoImport", &json!({"rename": "x_{seq:2}"})).unwrap();
    assert!(scan(&mut s).is_none(), "subfolders are off");
    s.execute("library.autoImport", &json!({"subfolders": true})).unwrap();
    let import = scan(&mut s).expect("now it is listed");
    assert!(import.get("rename").is_none(), "{import}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_presets_are_refused_and_vanished_ones_do_not_fail_the_scan() {
    let dir = tmp("gone-preset");
    put(&dir, "a.jpg");
    let mut s = watching(&dir);
    assert!(s.execute("library.autoImport", &json!({"preset": "no-such-preset"})).is_err());
    assert!(s.execute("library.autoImport", &json!({"metadataPreset": "no-such"})).is_err());
    s.import_defaults.auto_options.preset = Some("deleted-meanwhile".into());
    s.import_defaults.auto_options.metadata_preset = Some("deleted-too".into());
    let import = scan(&mut s).expect("still imported");
    assert!(import.get("preset").is_none() && import.get("metadataPreset").is_none(), "{import}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_written_before_these_options_load_with_everything_off() {
    let old: crate::import::ImportDefaults = serde_json::from_value(json!({"autoFolder": "/photos", "autoCopy": true})).unwrap();
    assert!(old.auto_options.is_default());
    assert_eq!(old.auto_active_folder().map(String::as_str), Some("/photos"));
    let mut on = old.clone();
    on.auto_options.subfolders = true;
    on.auto_options.keywords = vec!["k".into()];
    let back: crate::import::ImportDefaults = serde_json::from_value(serde_json::to_value(&on).unwrap()).unwrap();
    assert_eq!(back, on);
    assert!(serde_json::to_value(&old).unwrap().get("autoOptions").is_none());
}
