//! The app-wide store (LR-PREF-SHARED / LR-PREF-HABITS): shared by every library, each library
//! merged once (read only), profile ids remapped, marks moved out of presets.json, damaged files
//! reported, never a crash.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::Session;

/// The test's folder in Temp, removed when the test ends.
fn temp(tag: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lc-engine-store-{tag}"))
}

fn with_store(store: &Path, lib: &Path) -> Session {
    let mut s = Session::new().with_app_store(store);
    s.open_library(lib, false).unwrap();
    s
}

fn own(lib: &Path) -> Session {
    let mut s = Session::new();
    s.open_library(lib, false).unwrap();
    s
}

fn user_preset(id: &str, name: &str) -> lcc_develop::Preset {
    lcc_develop::Preset {
        id: id.into(),
        name: name.into(),
        group: "Mine".into(),
        settings: json!({"light": {"exposure": 0.5}}),
        favorite: false,
        builtin: false,
    }
}

fn export_preset(name: &str, quality: u32) -> crate::export::ExportPreset {
    serde_json::from_value(json!({"name": name, "params": {"quality": quality}})).unwrap()
}

#[test]
fn presets_and_export_presets_made_in_one_library_are_in_the_next() {
    let root = temp("share");
    let (store, a, b) = (root.join("store"), root.join("a"), root.join("b"));
    let mut s = with_store(&store, &a);
    s.presets.push(user_preset("user.warm", "Warm"));
    s.export_presets.push(export_preset("Web", 80));
    s.sort = lcc_catalog::Sort { key: lcc_catalog::SortKey::FileName, ascending: false, ..s.sort };
    s.save_prefs().unwrap();
    s.persist().unwrap();
    drop(s);
    // a library's own presets.json isn't written while the store holds them
    assert!(!a.join("presets.json").exists());
    let s = with_store(&store, &b);
    assert!(s.presets.iter().any(|p| p.id == "user.warm"));
    assert!(s.export_presets.iter().any(|p| p.name == "Web"));
    // LR-PREF-HABITS: the grid's sort follows the user into another library
    assert_eq!((s.sort.key, s.sort.ascending), (lcc_catalog::SortKey::FileName, false));
    drop(s);
    // without the store a library has its own (nothing of the store's)
    let s = own(&b);
    assert!(!s.presets.iter().any(|p| p.id == "user.warm"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_library_merges_once_read_only_and_its_photos_follow_a_remapped_profile() {
    let root = temp("merge");
    let (store, a, b) = (root.join("store"), root.join("a"), root.join("b"));
    // two libraries made without the store, each with its own `lut:look` (different files),
    // the same export preset, and a clashing one
    for (dir, body, q) in [
        (&a, "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n", 80),
        (&b, "LUT_3D_SIZE 2\n0 0 0\n0.5 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n", 60),
    ] {
        let mut s = own(dir);
        std::fs::create_dir_all(dir.join("Profiles")).unwrap();
        let f = dir.join("Profiles").join("look.cube");
        std::fs::write(&f, body).unwrap();
        s.lut_profiles.push(crate::cmd::lut_profiles::LutProfile {
            id: "lut:look".into(),
            name: "Look".into(),
            group: "G".into(),
            file: f.to_string_lossy().into(),
        });
        s.export_presets.push(export_preset("Web", 80));
        s.export_presets.push(export_preset("Print", q));
        let id = s.catalog.alloc_photo_id();
        let mut p =
            lcc_catalog::Photo::new(id, lcc_catalog::Source::File { path: "/x/a.jpg".into() }, "a.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
        std::sync::Arc::make_mut(&mut p.develop).profile.id = "lut:look".into();
        s.commit("Add", lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
        s.save_prefs().unwrap();
        s.persist().unwrap();
    }
    let b_prefs = std::fs::read(b.join("prefs.json")).unwrap();
    drop(with_store(&store, &a));
    let mut s = with_store(&store, &b);
    // b's profile is another file: it got a new id, and b's photo follows it
    let ids: Vec<_> = s.lut_profiles.iter().map(|p| p.id.clone()).collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let new_id = ids.iter().find(|i| *i != "lut:look").unwrap().clone();
    assert!(s.catalog.photos().all(|p| p.develop.profile.id == new_id));
    // the same export preset once, the clash kept as "Print 2"
    let names: Vec<_> = s.export_presets.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names.iter().filter(|n| **n == "Web").count(), 1);
    assert!(names.contains(&"Print") && names.contains(&"Print 2"), "{names:?}");
    // the merge only read b: its prefs.json keeps its own lists, also after a save
    s.save_prefs().unwrap();
    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(b.join("prefs.json")).unwrap()).unwrap();
    let before: serde_json::Value = serde_json::from_slice(&b_prefs).unwrap();
    for k in crate::app_store::SHARED_PREFS_KEYS {
        if k != "import" {
            assert_eq!(after.get(k), before.get(k), "{k}");
        }
    }
    // merged once: asking again skips it
    let r = s.execute("store.mergeLibraries", &json!({"paths": [b.to_string_lossy()]})).unwrap();
    assert_eq!(r["merged"][0]["skipped"], true);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn at_import_and_hidden_marks_move_from_a_library_into_the_store() {
    let root = temp("marks");
    let (store, a, b) = (root.join("store"), root.join("a"), root.join("b"));
    let mut s = own(&a);
    let id = s.presets.first().unwrap().id.clone();
    s.execute("preset.forImport", &json!({"id": id})).unwrap();
    s.execute("preset.hide", &json!({"id": id})).unwrap();
    drop(s);
    drop(with_store(&store, &a));
    let shared: serde_json::Value = serde_json::from_slice(&std::fs::read(store.join("shared.json")).unwrap()).unwrap();
    assert_eq!(shared["presetMarks"]["atImport"], json!([id]));
    let presets: serde_json::Value = serde_json::from_slice(&std::fs::read(store.join("presets.json")).unwrap()).unwrap();
    assert!(presets.get("atImport").is_none() && presets.get("hidden").is_none());
    let s = with_store(&store, &b);
    assert_eq!(s.preset_at_import, std::slice::from_ref(&id));
    assert_eq!(s.preset_hidden, [id]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_damaged_or_unreadable_store_is_reported_never_a_panic() {
    let root = temp("hostile");
    for (i, bytes) in [&b"{not json"[..], b"\xff\xfe\x00", b"[]", b"{\"lutProfiles\": 7}", &[b'['; 10_000][..], b""].iter().enumerate() {
        let store = root.join(format!("store{i}"));
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("shared.json"), bytes).unwrap();
        std::fs::write(store.join("presets.json"), bytes).unwrap();
        let mut s = Session::new().with_app_store(&store);
        let w = s.take_library_warnings();
        assert!(w.iter().any(|w| w.contains("shared.json") && w.contains("damaged")), "{w:?}");
        // kept aside, and the store works with defaults
        assert!(std::fs::read_dir(&store).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("shared.json.corrupt-")));
        s.export_presets.push(export_preset("Web", 80));
        s.save_prefs().unwrap();
    }
    // a store file that can't be read (a folder in its place) is never overwritten: an error, not a panic
    let store = root.join("blocked");
    std::fs::create_dir_all(store.join("shared.json")).unwrap();
    let mut s = Session::new().with_app_store(&store);
    assert!(!s.take_library_warnings().is_empty());
    s.export_presets.push(export_preset("Web", 80));
    let e = s.save_prefs().unwrap_err().to_string();
    assert!(e.contains("shared.json"), "{e}");
    // a library merged from hostile files: reported, nothing panics
    let lib = root.join("badlib");
    drop(own(&lib));
    std::fs::write(lib.join("prefs.json"), b"{\"exportPresets\": 5").unwrap();
    let mut s = Session::new().with_app_store(&root.join("store-ok"));
    let r = s.execute("store.mergeLibraries", &json!({"paths": [lib.to_string_lossy(), "/no/such/library"]})).unwrap();
    assert!(!r["merged"][0]["problems"].as_array().unwrap().is_empty());
    assert!(!r["merged"][1]["problems"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn use_library_keeps_a_copy_and_gather_reads_backups() {
    let root = temp("use");
    let (store, a, old) = (root.join("store"), root.join("a"), root.join("old"));
    let mut s = with_store(&store, &a);
    s.export_presets.push(export_preset("Web", 80));
    s.save_prefs().unwrap();
    s.execute("store.useLibrary", &json!({"keep": true})).unwrap();
    assert!(!s.uses_app_store());
    drop(s);
    let s = with_store(&store, &a);
    assert!(s.library.as_ref().unwrap().own_store);
    assert!(s.export_presets.iter().any(|p| p.name == "Web"));
    drop(s);
    // an older library's export preset only in a backup is gathered once
    drop(own(&old));
    let backup = old.join("Backups").join("2026-01-01");
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join("prefs.json"), serde_json::to_vec(&json!({"exportPresets": [{"name": "Archive", "params": {}}]})).unwrap()).unwrap();
    let mut s = Session::new().with_app_store(&store);
    let r = s.execute("store.gatherPresets", &json!({"paths": [old.to_string_lossy()]})).unwrap();
    assert_eq!(r["count"], 1);
    assert!(s.export_presets.iter().any(|p| p.name == "Archive"));
    let r = s.execute("store.gatherPresets", &json!({"paths": [old.to_string_lossy()]})).unwrap();
    assert_eq!(r["count"], 0);
    // LR-PREF-SHARED: a library whose backups go to another folder (another drive), one per
    // library name: an export preset kept only there is gathered
    let (old2, far) = (root.join("old2"), root.join("far"));
    drop(own(&old2));
    let settings = lcc_catalog::library::CatalogSettings { backup_dir: Some(far.clone()), ..lcc_catalog::library::CatalogSettings::load(&old2) };
    settings.save(&old2).unwrap();
    let backup = far.join("old2").join("2026-02-01");
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join("prefs.json"), serde_json::to_vec(&json!({"exportPresets": [{"name": "Far Away", "params": {}}]})).unwrap()).unwrap();
    let r = s.execute("store.gatherPresets", &json!({"paths": [old2.to_string_lossy()]})).unwrap();
    assert_eq!(r["count"], 1, "{r}");
    assert!(s.export_presets.iter().any(|p| p.name == "Far Away"));
    let _ = std::fs::remove_dir_all(&root);
}

/// LR-PRE-CREATE: single settings travel alone, White Balance stays out of a new preset by default,
/// Update Preset can be undone, the choices are remembered (in the store), a group typed like one
/// there is joins it, and dialog.remember refuses what it can't keep.
#[test]
fn per_setting_picks_update_with_undo_and_remembered_choices() {
    let root = temp("picks");
    let (store, a, b) = (root.join("store"), root.join("a"), root.join("b"));
    let mut s = with_store(&store, &a);
    let id = s.catalog.alloc_photo_id();
    let p = lcc_catalog::Photo::new(id, lcc_catalog::Source::File { path: "/x/a.jpg".into() }, "a.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.commit("Add", lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("develop.set", &json!({"values": {"light.exposure": 1.0, "light.contrast": 20, "wb.temp": 3000}})).unwrap();
    let r = s.execute("preset.create", &json!({"name": "Base"})).unwrap();
    let pid = r["id"].as_str().unwrap().to_string();
    let pr = s.presets.iter().find(|p| p.id == pid).unwrap();
    assert!(pr.settings.get("wb").is_none(), "White Balance is off by default: {}", pr.settings);
    // only Contrast of Light, in a group typed like one there is
    s.execute("preset.createGroup", &json!({"name": "Wedding"})).unwrap();
    let r = s.execute("preset.create", &json!({"name": "C", "group": " wedding ", "settings": ["light.contrast"]})).unwrap();
    assert_eq!(r["group"], "Wedding");
    let c = r["id"].as_str().unwrap().to_string();
    assert_eq!(s.presets.iter().find(|p| p.id == c).unwrap().settings, json!({"light": {"contrast": 20.0}}));
    assert!(s.execute("preset.create", &json!({"name": "X", "settings": ["light.nope"]})).is_err());
    // Update keeps the picked settings with the new values; Undo puts the old ones back
    s.execute("develop.set", &json!({"values": {"light.contrast": 50}})).unwrap();
    s.execute("preset.update", &json!({"id": c})).unwrap();
    assert_eq!(s.presets.iter().find(|p| p.id == c).unwrap().settings, json!({"light": {"contrast": 50.0}}));
    s.execute("preset.undoUpdate", &json!({})).unwrap();
    assert_eq!(s.presets.iter().find(|p| p.id == c).unwrap().settings, json!({"light": {"contrast": 20.0}}));
    assert!(s.execute("preset.undoUpdate", &json!({})).is_err());
    // the choices are remembered, the same in the next library
    s.execute("develop.copy", &json!({"settings": ["light.exposure"]})).unwrap();
    s.execute("dialog.remember", &json!({"dialog": "export", "choices": {"quality": 80}})).unwrap();
    assert!(s.execute("dialog.remember", &json!({"dialog": "big", "choices": "x".repeat(70_000)})).is_err());
    s.persist().unwrap();
    drop(s);
    let mut s = with_store(&store, &b);
    assert_eq!(s.dialogs.copy, lcc_develop::SettingsPick::from_ids(&["light.exposure"]));
    assert_eq!(s.dialogs.preset_group, "Wedding");
    assert_eq!(s.execute("dialog.recall", &json!({"dialog": "export"})).unwrap(), json!({"quality": 80}));
    let items = s.execute("develop.settingItems", &json!({})).unwrap();
    assert!(items["groups"].as_array().unwrap().iter().any(|g| g["group"] == "light" && !g["settings"].as_array().unwrap().is_empty()));
    drop(s);
    // hand-edited choices that don't parse are the defaults, never a damaged store
    let shared = store.join("shared.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&shared).unwrap()).unwrap();
    v["dialogs"] = json!({"copy": 7, "other": "nope"});
    std::fs::write(&shared, serde_json::to_vec(&v).unwrap()).unwrap();
    let mut s = Session::new().with_app_store(&store);
    assert!(s.take_library_warnings().is_empty());
    assert!(s.dialogs.is_default());
    let _ = std::fs::remove_dir_all(&root);
}

/// An older install's store folder as the app wrote it under an earlier name (OneSlips): presets.json
/// holds the user presets and favourites; shared.json the Presets Manager's `presetState` (hidden,
/// At Import) and the local adjustment presets.
fn older_store(dir: &Path, warm: &lcc_develop::Preset) {
    std::fs::create_dir_all(dir).unwrap();
    let bad = json!({"id": "user.broken", "name": 5});
    let presets =
        json!({"user": [warm, user_preset("user.cool", "Cool"), bad], "favorites": ["lc.warm-glow"], "profileFavorites": [], "profileRecent": []});
    std::fs::write(dir.join("presets.json"), serde_json::to_vec(&presets).unwrap()).unwrap();
    let shared = json!({
        "version": 2,
        "presetState": {"hidden": ["user.cool"], "removed": ["lc.warm-glow"], "names": {}, "groups": {}, "groupOrder": ["Mine"], "forImport": ["user.warm"]},
        "maskEffects": [{"name": "Soft skin", "adjust": {"texture": -0.4}}],
        "dcpProfiles": [{"id": "dcp:x", "file": "x.dcp"}],
        "profiles": {"bodies": {}}
    });
    std::fs::write(dir.join("shared.json"), serde_json::to_vec(&shared).unwrap()).unwrap();
}

/// Wedding notes 2026-11-05 19:50: the presets of an older install (the app under an earlier name)
/// come into the shared store with no files to pick: presets, favourites, hidden and At Import
/// marks and local adjustment presets; a preset that doesn't read costs only itself; each folder
/// once, and again on request without doubling anything; the folder is only read.
#[test]
fn an_older_installs_presets_join_the_store_once_read_only() {
    let root = temp("older");
    let (store, older) = (root.join("store"), root.join("Earlier").join(crate::app_store::DIR_NAME));
    let warm = user_preset("user.warm", "Warm");
    older_store(&older, &warm);
    let before: Vec<(String, Vec<u8>)> =
        ["presets.json", "shared.json"].iter().map(|f| (f.to_string(), std::fs::read(older.join(f)).unwrap())).collect();
    // found among the settings folders of earlier names
    assert_eq!(crate::app_store::stores_in(vec![root.join("Earlier"), root.join("Missing")]), vec![older.clone()]);

    let mut s = Session::new().with_app_store(&store);
    let r = s.execute("store.importOlderInstalls", &json!({"paths": [older.to_string_lossy()]})).unwrap();
    let m = &r["merged"][0];
    assert!(m["presets"].as_u64().unwrap() >= 2, "{r}");
    assert_eq!(m["effects"], 1, "{r}");
    assert!(m["problems"].as_array().unwrap().iter().any(|p| p.as_str().unwrap().contains("1 preset")), "the broken preset is named: {r}");
    for id in ["user.warm", "user.cool"] {
        assert!(s.presets.iter().any(|p| p.id == id), "{id} came in");
    }
    assert!(s.preset_hidden.contains(&"user.cool".to_string()));
    assert!(s.preset_at_import.contains(&"user.warm".to_string()));
    assert!(s.mask_effects.iter().any(|e| e.name == "Soft skin"));
    let n = s.presets.iter().count();

    // once: asking again skips it; forced, nothing doubles
    let r = s.execute("store.importOlderInstalls", &json!({"paths": [older.to_string_lossy()]})).unwrap();
    assert_eq!(r["merged"][0]["skipped"], true, "{r}");
    s.execute("store.importOlderInstalls", &json!({"paths": [older.to_string_lossy()], "force": true})).unwrap();
    assert_eq!(s.presets.iter().count(), n, "the same presets are kept once");
    assert_eq!(s.mask_effects.iter().filter(|e| e.name == "Soft skin").count(), 1);
    drop(s);
    // kept with the store: the next session has them
    let s = Session::new().with_app_store(&store);
    assert!(s.presets.iter().any(|p| p.id == "user.warm"));
    assert!(s.preset_hidden.contains(&"user.cool".to_string()));
    for (f, b) in before {
        assert_eq!(std::fs::read(older.join(&f)).unwrap(), b, "{f} in the older install is unchanged");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Hostile older stores: damaged files, a folder with neither file, the store itself: reported,
/// never a panic, and nothing of the store lost.
#[test]
fn a_damaged_older_install_store_is_reported_never_a_panic() {
    let root = temp("older-hostile");
    let store = root.join("store");
    let mut s = Session::new().with_app_store(&store);
    s.presets.push(user_preset("user.keep", "Keep"));
    s.save_app_store().unwrap();
    let mut paths = Vec::new();
    for (i, (presets, shared)) in [
        (&b"{not json"[..], &b"[]"[..]),
        (b"[1, 2]", b"{\"presetState\": 7, \"maskEffects\": \"x\"}"),
        (b"{\"user\": 5}", b"\xff\xfe"),
        (b"", b"{\"presetState\": {\"hidden\": [1, 2]}}"),
    ]
    .iter()
    .enumerate()
    {
        let d = root.join(format!("older{i}"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("presets.json"), presets).unwrap();
        std::fs::write(d.join("shared.json"), shared).unwrap();
        paths.push(d.to_string_lossy().to_string());
    }
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    paths.push(empty.to_string_lossy().to_string());
    paths.push(store.to_string_lossy().to_string());
    let r = s.execute("store.importOlderInstalls", &json!({"paths": paths})).unwrap();
    let merged = r["merged"].as_array().unwrap();
    assert_eq!(merged.len(), 6, "{r}");
    for m in merged {
        assert!(!m["problems"].as_array().unwrap().is_empty(), "each says what was wrong: {m}");
    }
    assert!(s.presets.iter().any(|p| p.id == "user.keep"), "the store keeps its own");
    let _ = std::fs::remove_dir_all(&root);
}

// --- an older install's camera profiles (OLDER-PROFILES) ---------------------------------------

/// A matrix-only camera profile, made here (no real profile is kept in the repository).
fn tiny_dcp(model: &str, name: &str, k: f64) -> Vec<u8> {
    let m = lcc_raw::Mat3([[0.7 + k, -0.1, -0.05], [-0.5, 1.3, 0.2], [-0.1, 0.25, 0.65]]);
    let dcp = lcc_raw::dcp::Dcp {
        model: model.into(),
        name: name.into(),
        copyright: String::new(),
        color: lcc_raw::ColorData { illuminant: [21, 0], color_matrix: [Some(m), None], ..Default::default() },
    };
    lcc_raw::dcp::write(&dcp).unwrap()
}

/// An older install's store with camera profiles `files` (`(file name, bytes)`) in its `Profiles/`
/// and `entries` as its `shared.json` lists them.
fn older_camera_store(dir: &Path, files: &[(&str, Vec<u8>)], entries: serde_json::Value) {
    let profiles = dir.join(crate::app_store::PROFILES_DIR);
    std::fs::create_dir_all(&profiles).unwrap();
    for (name, bytes) in files {
        std::fs::write(profiles.join(name), bytes).unwrap();
    }
    std::fs::write(dir.join("shared.json"), serde_json::to_vec(&json!({"version": 2, "dcpProfiles": entries})).unwrap()).unwrap();
}

/// Holds the decoder's profile lists for one test and puts them back after it.
struct Registries {
    _lock: std::sync::MutexGuard<'static, ()>,
    library: Vec<(String, lcc_raw::dcp::Dcp)>,
    active: Vec<lcc_raw::dcp::Dcp>,
}

impl Registries {
    fn hold() -> Registries {
        let lock = crate::camera_profiles::test_library_lock();
        Registries {
            _lock: lock,
            library: lcc_raw::dcp::library().into_iter().map(|(i, d)| (i, (*d).clone())).collect(),
            active: lcc_raw::dcp::installed().iter().map(|d| (**d).clone()).collect(),
        }
    }
}

impl Drop for Registries {
    fn drop(&mut self) {
        lcc_raw::dcp::install_library(std::mem::take(&mut self.library));
        lcc_raw::dcp::install(std::mem::take(&mut self.active));
    }
}

/// The camera-profiles folder and its library for a test, under `root`.
fn camera_dirs(root: &Path) -> (PathBuf, PathBuf) {
    let active = root.join("camera-profiles");
    (active.clone(), active.join("dcp"))
}

fn dcps_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    v.retain(|p| p.is_file() && p.extension().is_some_and(|x| x == "dcp"));
    v.sort();
    v
}

/// Older-install camera profiles come in through the Import Profiles & Presets install path: into
/// the camera-profile library, a lone one for a camera with none as its default, available at once,
/// counted in the report; the older folder is only read.
#[test]
fn an_older_installs_camera_profiles_are_installed() {
    let _reg = Registries::hold();
    let root = temp("older-dcp");
    let (store, older) = (root.join("store"), root.join("Earlier").join(crate::app_store::DIR_NAME));
    let (active, library) = camera_dirs(&root);
    let sony = tiny_dcp("Sony ILCE-9", "Older Sony", 0.0);
    let canon = tiny_dcp("Canon EOS R6", "Older Canon", 0.01);
    older_camera_store(
        &older,
        &[("older-sony.dcp", sony), ("older-canon.dcp", canon)],
        json!([{"id": "dcp:older-sony", "name": "Older Sony", "cameraModel": "Sony ILCE-9", "file": "C:/gone/Library Store/Profiles/older-sony.dcp", "bodies": ["55"]}]),
    );
    let before: Vec<(PathBuf, Vec<u8>)> = dcps_in(&older.join("Profiles")).into_iter().map(|p| (p.clone(), std::fs::read(&p).unwrap())).collect();
    let mut s = Session::new().with_app_store(&store);
    let r = s.merge_older_installs_in(std::slice::from_ref(&older), false, Some((&active, &library))).unwrap();
    assert_eq!(r[0].camera_profiles, 2, "{r:?}");
    assert!(r[0].camera_already.is_empty(), "{r:?}");
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v[0]["cameraProfiles"], 2, "the report counts them: {v}");
    let installed = dcps_in(&library);
    assert_eq!(installed.len(), 2, "{installed:?}");
    for p in &installed {
        let id = format!("dcp:{}", p.file_stem().unwrap().to_string_lossy());
        assert!(lcc_raw::dcp::library_get(&id).is_some(), "{id} can be chosen now");
    }
    // a lone profile for a camera with none: that camera's default, at once
    assert!(active.join(crate::camera_profiles::dcp_file_name("Canon EOS R6")).is_file());
    assert_eq!(lcc_raw::dcp::resolve_camera("", "Canon EOS R6").map(|d| d.name.clone()).as_deref(), Some("Older Canon"));
    for (p, b) in before {
        assert_eq!(std::fs::read(&p).unwrap(), b, "{} in the older install is unchanged", p.display());
    }
    assert_eq!(dcps_in(&older.join("Profiles")).len(), 2, "nothing added to the older install");
    // once: asking again skips the folder
    let r = s.merge_older_installs_in(std::slice::from_ref(&older), false, Some((&active, &library))).unwrap();
    assert!(r[0].skipped, "{r:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A camera body the older install started with a profile it brought over: its raws, browsed in
/// Import, render with it with no step of the user's (the photo isn't edited).
#[test]
fn an_older_installs_camera_profile_binds_when_its_raws_are_browsed() {
    let _reg = Registries::hold();
    let root = temp("older-dcp-bind");
    let (store, older) = (root.join("store"), root.join("Earlier").join(crate::app_store::DIR_NAME));
    let (active, library) = camera_dirs(&root);
    older_camera_store(
        &older,
        &[("body-55.dcp", tiny_dcp("Sony ILCE-9", "Body 55", 0.02))],
        json!([{"id": "dcp:body-55", "name": "Body 55", "cameraModel": "Sony ILCE-9", "file": "body-55.dcp", "bodyDefaults": ["55"]}]),
    );
    let mut s = Session::new().with_app_store(&store);
    let r = s.merge_older_installs_in(&[older], false, Some((&active, &library))).unwrap();
    assert_eq!((r[0].camera_profiles, r[0].camera_bodies), (1, 1), "{r:?}");
    let id = format!("dcp:{}", dcps_in(&library)[0].file_stem().unwrap().to_string_lossy());
    // browsed in Import: a Sony body 55 raw (Exif model ILCE-9), never imported
    let pid = s.catalog.alloc_photo_id();
    let mut p = lcc_catalog::Photo::new(
        pid,
        lcc_catalog::Source::File { path: "/nonexistent/DSC00001.ARW".into() },
        "DSC00001.ARW",
        "ARW",
        100,
        100,
        "2026-01-01",
    );
    p.kind = lcc_catalog::MediaKind::Raw;
    p.meta.camera = "Sony ILCE-9".into();
    p.meta.camera_model = "ILCE-9".into();
    p.meta.camera_serial = "55".into();
    p.local = true;
    p.set_local_baseline();
    s.commit("Add", lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    assert_eq!(s.render_job(pid, 64, 64, false, true).unwrap().settings.profile.camera, id, "the body's profile, on browse");
    let photo = s.catalog.photo(pid).unwrap();
    assert!(photo.edited.is_none() && photo.develop.profile.camera.is_empty(), "nothing stored on the photo");
    // another body of the camera keeps the camera's default
    let other = s.catalog.alloc_photo_id();
    let mut q = (**s.catalog.photo(pid).unwrap()).clone();
    (q.id, q.meta.camera_serial, q.source) = (other, "77".into(), lcc_catalog::Source::File { path: "/nonexistent/DSC00002.ARW".into() });
    s.commit("Add", lcc_catalog::Op::AddPhoto { photo: Box::new(q) }).unwrap();
    assert_eq!(s.render_job(other, 64, 64, false, true).unwrap().settings.profile.camera, "");
    let _ = std::fs::remove_dir_all(&root);
}

/// A camera profile the library has already (the same file, or the same camera and ProfileName) is
/// left out and reported as there already; the body binding goes to the one there is.
#[test]
fn an_older_installs_camera_profile_already_there_is_skipped() {
    let _reg = Registries::hold();
    let root = temp("older-dcp-dup");
    let store = root.join("store");
    let (active, library) = camera_dirs(&root);
    std::fs::create_dir_all(&library).unwrap();
    let mine = tiny_dcp("Nikon Z 6_2", "Portrait", 0.0);
    std::fs::write(library.join("nikon-z-6-2-portrait.dcp"), &mine).unwrap();
    std::fs::write(library.join("nikon-z-6-2-landscape.dcp"), tiny_dcp("Nikon Z 6_2", "Landscape", 0.03)).unwrap();
    let older = root.join("Earlier").join(crate::app_store::DIR_NAME);
    older_camera_store(
        &older,
        &[
            // the same bytes under another name
            ("copy-of-portrait.dcp", mine.clone()),
            // the same camera and ProfileName, other content
            ("landscape-v2.dcp", tiny_dcp("NIKON Z 6_2", "landscape", 0.04)),
            // new
            ("night.dcp", tiny_dcp("Nikon Z 6_2", "Night", 0.05)),
        ],
        json!([{"file": "copy-of-portrait.dcp", "cameraModel": "Nikon Z 6_2", "bodies": ["3001"]}]),
    );
    let mut s = Session::new().with_app_store(&store);
    let r = s.merge_older_installs_in(&[older], false, Some((&active, &library))).unwrap();
    assert_eq!(r[0].camera_profiles, 1, "only the new one: {r:?}");
    let mut already = r[0].camera_already.clone();
    already.sort();
    assert_eq!(already, vec!["copy-of-portrait.dcp".to_string(), "landscape-v2.dcp".to_string()], "{r:?}");
    assert_eq!(dcps_in(&library).len(), 3, "the library gained one file");
    assert_eq!(std::fs::read(library.join("nikon-z-6-2-portrait.dcp")).unwrap(), mine, "the library's own is kept");
    assert_eq!(s.body_profiles.values().collect::<Vec<_>>(), vec!["dcp:nikon-z-6-2-portrait"], "the body binds to the one there is");
    let _ = std::fs::remove_dir_all(&root);
}

/// A damaged or unreadable camera profile in an older install is left out and named; the others
/// still come, and nothing panics.
#[test]
fn a_damaged_older_install_camera_profile_is_reported() {
    let _reg = Registries::hold();
    let root = temp("older-dcp-bad");
    let store = root.join("store");
    let (active, library) = camera_dirs(&root);
    let good = tiny_dcp("Fujifilm X-T4", "Good", 0.0);
    let older = root.join("Earlier").join(crate::app_store::DIR_NAME);
    older_camera_store(
        &older,
        &[
            ("garbage.dcp", b"not a camera profile".to_vec()),
            ("truncated.dcp", good[..good.len() / 3].to_vec()),
            ("empty.dcp", Vec::new()),
            ("good.dcp", good.clone()),
        ],
        json!([{"file": "missing.dcp", "bodies": ["1"]}, 5, {"file": "good.dcp", "bodies": ["x".repeat(300)]}]),
    );
    // a folder named like a profile can't be read
    std::fs::create_dir_all(older.join("Profiles").join("folder.dcp")).unwrap();
    let mut s = Session::new().with_app_store(&store);
    let r = s.merge_older_installs_in(&[older], false, Some((&active, &library))).unwrap();
    assert_eq!(r[0].camera_profiles, 1, "{r:?}");
    let problems = r[0].problems.join("\n");
    for name in ["garbage.dcp", "truncated.dcp", "empty.dcp", "folder.dcp", "missing.dcp", "dcpProfiles"] {
        assert!(problems.contains(name), "{name} is named: {problems}");
    }
    assert_eq!(dcps_in(&library).len(), 1);
    assert!(s.body_profiles.is_empty(), "an over-long body isn't kept");
    let _ = std::fs::remove_dir_all(&root);
}
