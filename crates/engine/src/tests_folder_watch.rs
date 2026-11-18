//! Folder polling and the new-file policy (issue #123; see [`crate::folder_watch`]).
//!
//! * Given a library folder, the first poll only records its listing; files added afterwards are
//!   found by the next poll, and only folders whose listing changed are scanned again.
//! * With new files "ask" (the default), the folder gets an "N new" count that stays until they
//!   are added (Synchronize Folder); with "add", they are imported at once.
//! * A `sync` link names `folder.synchronize` for its folder, and running it adds the files.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::Session;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-poll-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).components().collect::<PathBuf>().to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_png(path: &str, seed: u8) {
    let (w, h) = (24usize, 16usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 13) as u8, seed, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A library holding `trip/a.png` and `other/b.png`.
fn library(dir: &Scratch) -> Session {
    write_png(&dir.path("trip/a.png"), 1);
    write_png(&dir.path("other/b.png"), 2);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.path("trip"), dir.path("other")]})).unwrap();
    assert_eq!(s.catalog.len(), 2);
    s
}

fn new_total(s: &mut Session) -> u64 {
    s.execute("folder.newFiles", &json!({})).unwrap()["total"].as_u64().unwrap()
}

#[test]
fn a_poll_rescans_only_changed_folders_asks_by_default_and_notices_new_and_gone_folders() {
    let dir = Scratch::new("ask");
    let mut s = library(&dir);
    // the first poll records the listings: nothing is reported
    let r = s.execute("folder.poll", &json!({})).unwrap();
    assert_eq!(r["changed"].as_array().unwrap().len(), 0, "{r}");
    // only the folders holding photos are listed, never those above them
    assert_eq!(r["listed"], 2, "{r}");
    // nothing changed: nothing scanned
    let r = s.execute("folder.poll", &json!({})).unwrap();
    assert!(r["changed"].as_array().unwrap().is_empty(), "{r}");

    write_png(&dir.path("trip/c.png"), 3);
    write_png(&dir.path("trip/d.png"), 4);
    let r = s.execute("folder.poll", &json!({})).unwrap();
    let changed: Vec<&str> = r["changed"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert!(changed.len() == 1 && changed[0].ends_with("trip"), "only the changed folder: {r}");
    assert_eq!(r["imported"], 0);
    assert_eq!(s.catalog.len(), 2, "ask adds nothing by itself");
    assert_eq!(new_total(&mut s), 2);

    // adding them (Synchronize Folder) answers the badge
    s.execute("folder.synchronize", &json!({"path": dir.path("trip")})).unwrap();
    assert_eq!(s.catalog.len(), 4);
    assert_eq!(new_total(&mut s), 0);

    // a `sync` link names `folder.synchronize` for its folder, and running it adds the files
    write_png(&dir.path("trip/f.png"), 6);
    let l = crate::link::parse(&crate::link::sync_link(&dir.path("trip"))).unwrap();
    let r = s.execute(l.command, &l.params).unwrap();
    assert_eq!(r["imported"], 1, "{r}");

    // a new folder inside a library folder is noticed
    s.execute("folder.poll", &json!({})).unwrap();
    write_png(&dir.path("trip/day2/g.png"), 7);
    s.execute("folder.poll", &json!({})).unwrap();
    assert_eq!(crate::folder_watch::new_in(&s, &dir.path("trip")), Some(1));

    // a folder that went away is not reported as changed
    std::fs::remove_dir_all(dir.path("other")).unwrap();
    let r = s.execute("folder.poll", &json!({})).unwrap();
    assert!(r["changed"].as_array().unwrap().is_empty(), "{r}");
    assert_eq!(r["offline"], 1, "{r}");
}

#[test]
fn watch_preferences_are_checked_and_with_add_new_files_are_imported_at_once() {
    let dir = Scratch::new("add");
    let mut s = library(&dir);
    // polling is off by default, and saved as set
    let p = s.execute("folder.watchPreferences", &json!({})).unwrap();
    assert_eq!(p["poll"], false);
    assert_eq!(p["newFiles"], "ask");
    let p = s.execute("folder.watchPreferences", &json!({"poll": true, "intervalMinutes": 0, "newFiles": "add"})).unwrap();
    assert_eq!((p["poll"].as_bool(), p["intervalMinutes"].as_u64(), p["newFiles"].as_str()), (Some(true), Some(1), Some("add")));
    // hostile values are errors, never a panic, and change nothing
    for bad in [json!({"newFiles": "maybe"}), json!({"intervalMinutes": -3}), json!({"intervalMinutes": "x"})] {
        assert!(s.execute("folder.watchPreferences", &bad).is_err(), "{bad}");
    }
    assert_eq!(s.folder_watch.interval_minutes, 1);
    s.execute("folder.poll", &json!({})).unwrap();
    write_png(&dir.path("other/e.png"), 5);
    let r = s.execute("folder.poll", &json!({})).unwrap();
    assert_eq!(r["imported"], 1, "{r}");
    assert_eq!(s.catalog.len(), 3);
    assert_eq!(new_total(&mut s), 0);
}
