//! Library's Classic panels (P1.4), engine side: Quick Develop on many photos, folder operations
//! on disk, collection export/import and keyword attributes.

use serde_json::json;

use crate::Session;

fn demo_with(n: usize) -> (Session, Vec<u64>) {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.visible_cloned().iter().take(n).map(|p| p.0).collect();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    (s, ids)
}

/// Quick Develop's choices reach every selected photo in one undo step; the selection stays.
#[test]
fn quick_set_applies_to_every_selected_photo_in_one_step() {
    let (mut s, ids) = demo_with(3);
    let active = s.selection.active;
    let undo = s.undo.len();
    let r = s.execute("develop.quickSet", &json!({"treatment": "bw"})).unwrap();
    assert_eq!(r["changed"], 3, "{r}");
    assert_eq!(s.undo.len(), undo + 1, "one undo step");
    assert_eq!(s.selection.active, active, "the selection is put back");
    for id in &ids {
        assert_eq!(s.develop_of(dac_catalog::PhotoId(*id)).unwrap().treatment, dac_develop::Treatment::Bw);
    }
    s.undo_step().unwrap();
    for id in &ids {
        assert_eq!(s.develop_of(dac_catalog::PhotoId(*id)).unwrap().treatment, dac_develop::Treatment::Color);
    }
    // crop ratio and white balance
    s.execute("develop.quickSet", &json!({"aspect": "1x1"})).unwrap();
    for id in &ids {
        let (w, h) = s.develop_of(dac_catalog::PhotoId(*id)).unwrap().crop.aspect.unwrap();
        assert_eq!(w, h);
    }
    s.execute("develop.quickSet", &json!({"wb": "tungsten"})).unwrap();
    for id in &ids {
        assert_eq!(s.develop_of(dac_catalog::PhotoId(*id)).unwrap().wb.mode, dac_develop::WbMode::Tungsten);
    }
    // bad input is an error, not a panic
    assert!(s.execute("develop.quickSet", &json!({"treatment": "sepia"})).is_err());
    assert!(s.execute("develop.quickSet", &json!({"wb": "custom"})).is_err());
    assert!(s.execute("develop.quickSet", &json!({})).is_err());
    assert!(s.execute("develop.quickSet", &json!({"aspect": "axb"})).unwrap()["errors"].as_array().is_some_and(|e| !e.is_empty()));
}

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let d = std::env::temp_dir().join(format!("classic-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Scratch(d)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_png(path: &str, seed: u8) {
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = dac_raster::Rgba8 { width: w, height: h, data };
    let bytes = dac_codecs::encode_png(&dac_codecs::EncodeImage::rgba8(&img), &dac_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn add_file_photo(s: &mut Session, path: &str) -> dac_catalog::PhotoId {
    let id = s.catalog.alloc_photo_id();
    let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy().to_string();
    let p = dac_catalog::Photo::new(id, dac_catalog::Source::File { path: path.into() }, &name, "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.catalog.apply(dac_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

/// Folders: a folder created on disk is one undo step (undo removes it, redo makes it again); an
/// empty folder can be deleted (and undone); a folder holding anything is refused.
#[test]
fn folders_are_created_and_deleted_on_disk_undoably() {
    let d = Scratch::new("create");
    let mut s = Session::with_demo();
    let r = s.execute("folder.create", &json!({"parent": d.path(""), "name": "Trips"})).unwrap();
    let made = r["path"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&made).is_dir());
    s.undo_step().unwrap();
    assert!(!std::path::Path::new(&made).exists(), "undo removes it");
    s.redo_step().unwrap();
    assert!(std::path::Path::new(&made).is_dir(), "redo makes it again");
    // a name with a slash, a missing parent and an existing folder are errors
    assert!(s.execute("folder.create", &json!({"parent": d.path(""), "name": "a/b"})).is_err());
    assert!(s.execute("folder.create", &json!({"parent": d.path("nope"), "name": "x"})).is_err());
    assert!(s.execute("folder.create", &json!({"parent": d.path(""), "name": "Trips"})).is_err());
    // delete: empty only, undoable
    s.execute("folder.delete", &json!({"path": made})).unwrap();
    assert!(!std::path::Path::new(&made).exists());
    s.undo_step().unwrap();
    assert!(std::path::Path::new(&made).is_dir());
    std::fs::write(d.path("Trips/keep.txt"), b"x").unwrap();
    assert!(s.execute("folder.delete", &json!({"path": made})).is_err(), "not empty");
    assert!(std::path::Path::new(&d.path("Trips/keep.txt")).is_file(), "nothing was removed");
    // undoing the creation now fails (it holds a file) and the file stays
    s.redo_step().ok();
    while s.undo_step().is_ok() {}
    assert!(std::path::Path::new(&d.path("Trips/keep.txt")).is_file());
}

/// Synchronize Folder adds the files the library lacks, lists photos whose file is gone, and
/// with removeMissing moves them to Recently Deleted; Update Folder Location relinks a moved folder.
#[test]
fn a_folder_synchronises_and_relocates() {
    let d = Scratch::new("sync");
    std::fs::create_dir_all(d.path("shoot/sub")).unwrap();
    let mut s = Session::new().with_fs();
    // a known photo whose file exists, one whose file is gone, and a new file on disk
    write_png(&d.path("shoot/a.png"), 1);
    write_png(&d.path("shoot/sub/new.png"), 2);
    let a = add_file_photo(&mut s, &d.path("shoot/a.png"));
    let gone = add_file_photo(&mut s, &d.path("shoot/gone.png"));
    let r = s.execute("folder.sync", &json!({"path": d.path("shoot"), "dryRun": true})).unwrap();
    assert_eq!(r["new"], 1, "{r}");
    assert_eq!(r["missing"], json!([gone.0]), "{r}");
    let r = s.execute("folder.sync", &json!({"path": d.path("shoot"), "removeMissing": true})).unwrap();
    assert_eq!(r["removed"], 1, "{r}");
    let r = s.execute("folder.sync", &json!({"path": d.path("shoot"), "dryRun": true})).unwrap();
    assert_eq!(r["missing"], json!([]), "{r}");
    // an offline folder is an error that says what to do
    assert!(s.execute("folder.sync", &json!({"path": d.path("offline")})).unwrap_err().to_string().contains("relocate"));
    // the folder moved outside the app: point the photos at the new place
    std::fs::rename(d.path("shoot"), d.path("moved")).unwrap();
    let r = s.execute("folder.relocate", &json!({"path": d.path("shoot"), "to": d.path("moved")})).unwrap();
    assert!(r["relinked"].as_u64().unwrap() >= 1, "{r}");
    let now = match &s.catalog.photo(a).unwrap().source {
        dac_catalog::Source::File { path } => path.clone(),
        _ => String::new(),
    };
    assert_eq!(now, d.path("moved/a.png"));
    s.undo_step().unwrap();
    assert!(matches!(&s.catalog.photo(a).unwrap().source, dac_catalog::Source::File { path } if *path == d.path("shoot/a.png")));
    assert!(s.execute("folder.relocate", &json!({"path": d.path("nothing-here"), "to": d.path("moved")})).is_err());
}

/// Collections: a set with a collection and a smart collection inside exports as a definition
/// and imports back (one undo step) with its photos matched; broken definitions are errors.
#[test]
fn collection_definitions_round_trip() {
    let d = Scratch::new("coll");
    let mut s = Session::new().with_fs();
    let a = add_file_photo(&mut s, &d.path("a.jpg"));
    let b = add_file_photo(&mut s, &d.path("b.jpg"));
    let set = s.execute("album.create", &json!({"name": "Trips", "folder": true})).unwrap()["id"].as_u64().unwrap();
    let col = s.execute("album.create", &json!({"name": "Rome", "parent": set})).unwrap()["id"].as_u64().unwrap();
    s.selection = crate::Selection { ids: vec![a, b], active: Some(a) };
    s.execute("album.addPhotos", &json!({"id": col, "ids": [a.0, b.0]})).unwrap();
    s.execute("album.createSmart", &json!({"name": "Good", "parent": set, "rules": {"ruleSet": {"match": "any", "rules": [{"field": "rating", "op": "gte", "value": 4}, {"group": {"match": "all", "rules": [{"field": "city", "op": "is", "value": "rome"}]}}]}}})).unwrap();
    let file = d.path("trips.json");
    s.execute("album.exportDefinition", &json!({"id": set, "path": file})).unwrap();
    // into another library holding the same files
    let mut t = Session::new().with_fs();
    add_file_photo(&mut t, &d.path("b.jpg"));
    let undo = t.undo.len();
    let r = t.execute("album.importDefinition", &json!({"path": file})).unwrap();
    assert_eq!(r["created"], 3, "{r}");
    assert_eq!(r["photos"], 1, "{r}");
    assert_eq!(r["unmatched"], 1, "{r}");
    assert_eq!(t.undo.len(), undo + 1, "one undo step");
    let trips = t.catalog.albums().find(|x| x.name == "Trips").unwrap();
    assert!(trips.folder);
    let good = t.catalog.albums().find(|x| x.name == "Good").unwrap();
    assert_eq!(good.parent, Some(trips.id));
    assert!(good.is_smart());
    // a new album after the import gets a fresh id
    let n = t.execute("album.create", &json!({"name": "After"})).unwrap()["id"].as_u64().unwrap();
    assert_eq!(t.catalog.albums().filter(|x| x.id.0 == n).count(), 1);
    t.undo_step().unwrap();
    t.undo_step().unwrap();
    assert!(t.catalog.albums().all(|x| x.name != "Trips"));
    // broken input
    std::fs::write(d.path("bad.json"), "{nope").unwrap();
    assert!(t.execute("album.importDefinition", &json!({"path": d.path("bad.json")})).is_err());
    assert!(t.execute("album.importDefinition", &json!({"definition": {"format": "other", "collections": []}})).is_err());
    assert!(
        t.execute(
            "album.importDefinition",
            &json!({"definition": {"format": "collection-definition", "collections": [{"name": "x", "kind": "smart"}]}})
        )
        .is_err()
    );
    assert!(t.execute("album.exportDefinition", &json!({"id": 999_999})).is_err());
}

/// The volumes the library's photos are on, with free space where the system tells.
#[test]
fn volumes_list_the_disks_with_their_space() {
    let d = Scratch::new("vol");
    let mut s = Session::with_demo();
    add_file_photo(&mut s, &d.path("x.jpg"));
    let v = s.execute("library.volumes", &json!({})).unwrap();
    let list = v.as_array().unwrap();
    assert!(!list.is_empty(), "{v}");
    #[cfg(unix)]
    assert!(list.iter().any(|x| x["total"].as_u64().unwrap_or(0) > 0), "{v}");
    assert_eq!(crate::cmd::folders::disk_space("/definitely/not/here"), None);
}

/// Keywording: built-in keyword sets can be used (⌥1–⌥9) but not deleted, a user set of the same
/// name replaces one; the keyword shortcut (⇧K) toggles its keyword on the selection.
#[test]
fn builtin_keyword_sets_and_the_keyword_shortcut() {
    let (mut s, ids) = demo_with(2);
    let sets = s.execute("keyword.sets", &json!({})).unwrap();
    assert!(sets["sets"].as_array().unwrap().iter().any(|x| x["name"] == "Outdoor Photography" && x["builtin"] == true), "{sets}");
    s.execute("keyword.useSet", &json!({"name": "outdoor photography"})).unwrap();
    assert_eq!(crate::cmd::keywords::current_keywords(&s).first().map(String::as_str), Some("Landscape"));
    let r = s.execute("keyword.toggleFromSet", &json!({"index": 1})).unwrap();
    assert_eq!(r["keyword"], "Landscape");
    assert!(s.execute("keyword.deleteSet", &json!({"name": "Outdoor Photography"})).is_err(), "built-in sets stay");
    s.execute("keyword.saveSet", &json!({"name": "Outdoor Photography", "keywords": ["Hike"]})).unwrap();
    assert_eq!(crate::cmd::keywords::current_keywords(&s), ["Hike"]);
    s.execute("keyword.deleteSet", &json!({"name": "Outdoor Photography"})).unwrap();
    // the shortcut
    assert!(s.execute("keyword.toggleShortcut", &json!({})).is_err(), "none set yet");
    s.execute("keyword.setShortcut", &json!({"keyword": " Travel|Rome "})).unwrap();
    let has = |s: &Session, k: &str| ids.iter().all(|i| s.catalog.photo(dac_catalog::PhotoId(*i)).unwrap().meta.keywords.iter().any(|x| x == k));
    s.execute("keyword.toggleShortcut", &json!({})).unwrap();
    assert!(has(&s, "Travel|Rome"));
    s.execute("keyword.toggleShortcut", &json!({})).unwrap();
    assert!(!has(&s, "Travel|Rome"));
    s.execute("keyword.setShortcut", &json!({"keyword": ""})).unwrap();
    assert_eq!(s.keyword_shortcut, None);
}
