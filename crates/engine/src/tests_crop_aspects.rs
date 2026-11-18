//! The crop tool's own aspect ratios (issue #1854): saved under a name, in any number, reordered,
//! deleted, built-in ones hidden, all kept with the library preferences.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-crop-aspects-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `crop.aspects` as (name, hidden) pairs, in menu order.
fn listed(s: &mut Session) -> Vec<(String, bool)> {
    let r = s.execute("crop.aspects", &json!({})).unwrap();
    r["aspects"].as_array().unwrap().iter().map(|a| (a["name"].as_str().unwrap().to_string(), a["hidden"] == true)).collect()
}

fn custom_names(s: &Session) -> Vec<String> {
    s.crop_aspects.custom.iter().map(|a| a.name.clone()).collect()
}

#[test]
fn saving_adds_named_ratios_without_a_limit_and_replaces_one_by_name() {
    let mut s = Session::with_demo();
    for i in 1..=12 {
        s.execute("crop.saveAspect", &json!({"name": format!("Format {i}"), "aspect": [i, 7]})).unwrap();
    }
    assert_eq!(s.crop_aspects.custom.len(), 12, "more than Lightroom's five");
    let r = s.execute("crop.saveAspect", &json!({"name": "Instagram Portrait", "aspect": [4, 5]})).unwrap();
    assert!(r["aspects"].as_array().unwrap().iter().any(|a| a["name"] == "Instagram Portrait" && a["aspect"] == json!([4.0, 5.0])));
    // the same name (any case) replaces the ratio in place, keeping its position
    s.execute("crop.saveAspect", &json!({"name": "format 3", "aspect": "1080x1350"})).unwrap();
    assert_eq!(s.crop_aspects.custom.len(), 13);
    assert_eq!(s.crop_aspects.custom[2].name, "format 3");
    assert_eq!(s.crop_aspects.custom[2].aspect, [1080.0, 1350.0]);
    // a saved ratio is listed after the built-ins
    let names: Vec<String> = listed(&mut s).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names.get(10).map(String::as_str), Some("Format 1"));
    assert_eq!(names.last().map(String::as_str), Some("Instagram Portrait"));
}

/// What `crop.saveAspect` keeps, `crop.aspect` applies: a ratio whose numbers round to zero
/// hundredths (the precision `crop.aspect` stores) is refused when saved and dropped on load,
/// instead of becoming a menu entry that can't be used (#1856 review).
#[test]
fn bad_tiny_and_rounded_ratios_names_and_hidden_flags_are_refused_and_change_nothing() {
    let mut s = Session::with_demo();
    for tiny in [[0.001, 0.001], [0.0049, 0.0049], [3.0, 0.004]] {
        let applied = s.execute("crop.aspect", &json!({"aspect": tiny}));
        assert!(applied.is_err(), "crop.aspect refuses {tiny:?}");
        assert!(!crate::cmd::crop_aspects::valid_ratio(tiny[0], tiny[1]), "{tiny:?}");
        assert!(s.execute("crop.saveAspect", &json!({"name": "Tiny", "aspect": tiny})).is_err(), "saving {tiny:?}");
    }
    assert!(s.crop_aspects.custom.is_empty());
    for small in [[0.005, 0.005], [0.01, 0.01], [3.0, 2.0]] {
        s.execute("crop.saveAspect", &json!({"name": "Small", "aspect": small})).unwrap();
        s.execute("crop.aspect", &json!({"aspect": small})).unwrap();
    }
    let loaded = crate::cmd::crop_aspects::CropAspects {
        custom: vec![
            crate::cmd::crop_aspects::CustomAspect { name: "Tiny".into(), aspect: [0.001, 0.001] },
            crate::cmd::crop_aspects::CustomAspect { name: "Small".into(), aspect: [0.005, 0.005] },
        ],
        hidden: Vec::new(),
    }
    .sanitized();
    assert_eq!(loaded.custom.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Small"]);

    // CodeRabbit on #3762: a ratio that is over 20:1 once rounded to hundredths is refused (what `crop.aspect` stores
    // is the rounded pair), and `hidden` must be a boolean when it is given.
    assert!(s.execute("crop.aspect", &json!({"aspect": [1, 0.044]})).is_err(), "22.7:1 once rounded");
    assert!(s.execute("crop.saveAspect", &json!({"name": "Thin", "aspect": [1, 0.044]})).is_err());
    s.execute("crop.aspect", &json!({"aspect": [1, 0.05]})).unwrap();
    for bad in [json!("yes"), json!(1), json!([true])] {
        let e = s.execute("crop.hideAspect", &json!({"aspect": "4x5", "hidden": bad})).unwrap_err().to_string();
        assert!(e.contains("true or false"), "{bad}: {e}");
    }
    assert!(s.crop_aspects.hidden.is_empty());
    s.execute("crop.hideAspect", &json!({"aspect": "4x5", "hidden": null})).unwrap();
    assert_eq!(s.crop_aspects.hidden, ["4x5"]);

    // bad ratios and names are refused and change nothing
    s.execute("crop.saveAspect", &json!({"name": "Print", "aspect": [7, 5]})).unwrap();
    let before = s.crop_aspects.clone();
    for p in [
        json!({"name": "", "aspect": [3, 2]}),
        json!({"name": "   ", "aspect": [3, 2]}),
        json!({"aspect": [3, 2]}),
        json!({"name": "No ratio"}),
        json!({"name": "Zero", "aspect": [0, 2]}),
        json!({"name": "Negative", "aspect": [-3, 2]}),
        json!({"name": "Too wide", "aspect": [1000, 1]}),
        json!({"name": "One number", "aspect": [3]}),
        json!({"name": "Text", "aspect": "wide"}),
        json!({"name": "Free", "aspect": [3, 2]}),
        json!({"name": "original", "aspect": [3, 2]}),
        json!({"name": "x".repeat(200), "aspect": [3, 2]}),
    ] {
        assert!(s.execute("crop.saveAspect", &p).is_err(), "refused: {p}");
    }
    assert_eq!(s.crop_aspects, before);
}

#[test]
fn saved_ratios_move_and_delete() {
    let mut s = Session::with_demo();
    for (name, w, h) in [("A4", 210, 297), ("Print", 7, 5), ("Banner", 3, 1)] {
        s.execute("crop.saveAspect", &json!({"name": name, "aspect": [w, h]})).unwrap();
    }
    s.execute("crop.moveAspect", &json!({"name": "banner", "to": 0})).unwrap();
    assert_eq!(custom_names(&s), ["Banner", "A4", "Print"]);
    // past the end: last
    s.execute("crop.moveAspect", &json!({"name": "Banner", "to": 99})).unwrap();
    assert_eq!(custom_names(&s), ["A4", "Print", "Banner"]);
    s.execute("crop.moveAspect", &json!({"name": "Print", "to": 0})).unwrap();
    assert_eq!(custom_names(&s), ["Print", "A4", "Banner"]);
    assert!(s.execute("crop.moveAspect", &json!({"name": "Nope", "to": 0})).is_err());
    assert!(s.execute("crop.moveAspect", &json!({"name": "A4"})).is_err(), "needs `to`");
    s.execute("crop.deleteAspect", &json!({"name": "a4"})).unwrap();
    assert_eq!(custom_names(&s), ["Print", "Banner"]);
    assert!(s.execute("crop.deleteAspect", &json!({"name": "A4"})).is_err(), "already gone");
    assert!(s.execute("crop.deleteAspect", &json!({"name": "Original"})).is_err(), "built-ins can't be deleted");
}

#[test]
fn built_in_ratios_hide_and_show_but_free_and_original_stay() {
    let mut s = Session::with_demo();
    s.execute("crop.hideAspect", &json!({"aspect": "8.5x11"})).unwrap();
    s.execute("crop.hideAspect", &json!({"aspect": "16x10", "hidden": true})).unwrap();
    s.execute("crop.hideAspect", &json!({"aspect": "16x10"})).unwrap();
    assert_eq!(s.crop_aspects.hidden, ["8.5x11", "16x10"], "hiding twice is one entry");
    let hidden: Vec<String> = listed(&mut s).into_iter().filter(|(_, h)| *h).map(|(n, _)| n).collect();
    assert_eq!(hidden, ["8.5 × 11", "16 × 10"]);
    s.execute("crop.hideAspect", &json!({"aspect": "8.5x11", "hidden": false})).unwrap();
    assert_eq!(s.crop_aspects.hidden, ["16x10"]);
    for key in ["free", "original", "7x5", "nonsense"] {
        assert!(s.execute("crop.hideAspect", &json!({"aspect": key})).is_err(), "{key}");
    }
    // a hidden ratio still applies by its value (only the menu leaves it out); a preset takes the
    // crop's orientation (#2253), so 16 × 10 may come out portrait
    s.execute("crop.aspect", &json!({"aspect": "16x10"})).unwrap();
    let id = s.active().unwrap();
    let (w, h) = s.develop_of(id).unwrap().crop.aspect.unwrap();
    assert_eq!((w.max(h), w.min(h)), (1600, 1000));
}

#[test]
fn saved_and_hidden_ratios_persist_with_the_library() {
    let lib = temp_dir("persist");
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, true).unwrap();
        s.execute("crop.saveAspect", &json!({"name": "Instagram Portrait", "aspect": [4, 5]})).unwrap();
        s.execute("crop.saveAspect", &json!({"name": "Cinema", "aspect": [2.39, 1]})).unwrap();
        s.execute("crop.moveAspect", &json!({"name": "Cinema", "to": 0})).unwrap();
        s.execute("crop.hideAspect", &json!({"aspect": "5x7"})).unwrap();
        s.close_library().unwrap();
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(custom_names(&s), ["Cinema", "Instagram Portrait"]);
    assert_eq!(s.crop_aspects.custom[0].aspect, [2.39, 1.0]);
    assert_eq!(s.crop_aspects.hidden, ["5x7"]);
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&lib);
}

/// A hand-edited prefs.json can't put a ratio in the menu that `crop.aspect` would refuse, an
/// empty or duplicate name, or hide Free / Original: those entries are dropped on load.
#[test]
fn a_hand_edited_prefs_file_loads_only_valid_ratios() {
    let lib = temp_dir("hand-edited");
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, true).unwrap();
        s.execute("crop.saveAspect", &json!({"name": "Keep", "aspect": [3, 2]})).unwrap();
        s.close_library().unwrap();
    }
    let prefs = lib.join("prefs.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&prefs).unwrap()).unwrap();
    v["cropAspects"] = json!({
        "custom": [
            {"name": "Keep", "aspect": [3, 2]},
            {"name": "keep", "aspect": [5, 4]},
            {"name": "", "aspect": [5, 4]},
            {"name": "Flat", "aspect": [0, 4]},
            {"name": "Wild", "aspect": [1e9, 1]},
            {"name": "Free", "aspect": [1, 1]},
            {"name": "Fine", "aspect": [5, 4]}
        ],
        "hidden": ["free", "original", "4x3", "4x3", "bogus"]
    });
    std::fs::write(&prefs, serde_json::to_vec(&v).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(custom_names(&s), ["Keep", "Fine"]);
    assert_eq!(s.crop_aspects.hidden, ["4x3"]);
    s.close_library().unwrap();
    let _ = std::fs::remove_dir_all(&lib);

    // a saved ratio that doesn't read is left out; the other ratios, and the rest of the preferences, still load
    let read: crate::cmd::crop_aspects::CropAspects = serde_json::from_value(json!({
        "custom": [{"name": "Pano", "aspect": [3, 1]}, {"name": "Broken", "aspect": "3x1"}, {"name": "Story", "aspect": [9, 16]}],
        "hidden": ["4x5", 7],
    }))
    .unwrap();
    let names: Vec<&str> = read.custom.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["Pano", "Story"]);
    assert_eq!(read.hidden, ["4x5"]);
    let not_lists: crate::cmd::crop_aspects::CropAspects = serde_json::from_value(json!({"custom": 3, "hidden": "4x5"})).unwrap();
    assert_eq!(not_lists, Default::default());
}

/// A change to the saved ratios that prefs.json can't take isn't kept in the session either: the
/// command fails and the menu still lists what is on disk.
#[test]
fn a_ratio_change_that_cant_be_saved_is_not_kept() {
    use lcc_catalog::{MemStore, Store};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[derive(Clone, Default)]
    struct Failing {
        files: MemStore,
        fail: Arc<AtomicBool>,
    }
    impl Store for Failing {
        fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
            self.files.read(name)
        }
        fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
            if self.fail.load(Ordering::Relaxed) && name == "prefs.json" {
                return Err(std::io::Error::other("disk full"));
            }
            self.files.write_atomic(name, data)
        }
        fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
            self.files.append(name, data)
        }
        fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
            self.files.truncate(name, len)
        }
        fn describe(&self) -> String {
            "failing".into()
        }
    }
    let files = Failing::default();
    let mut s = Session::new();
    let stores =
        crate::library::LibraryStores { dir: "failing".into(), catalog: Box::new(MemStore::new()), files: Box::new(files.clone()), on_disk: false };
    s.open_library_in(stores, false).unwrap();
    s.execute("crop.saveAspect", &json!({"name": "Pano", "aspect": [3, 1]})).unwrap();
    s.execute("crop.saveAspect", &json!({"name": "Story", "aspect": [9, 16]})).unwrap();
    let kept = s.crop_aspects.clone();
    files.fail.store(true, Ordering::Relaxed);
    for (command, params) in [
        ("crop.saveAspect", json!({"name": "Wide", "aspect": [21, 9]})),
        ("crop.deleteAspect", json!({"name": "Pano"})),
        ("crop.moveAspect", json!({"name": "Story", "to": 0})),
        ("crop.hideAspect", json!({"aspect": "4x5"})),
    ] {
        let e = s.execute(command, &params).unwrap_err().to_string();
        assert!(e.contains("disk full"), "{command}: {e}");
        assert_eq!(s.crop_aspects, kept, "{command} kept a change prefs.json doesn't hold");
    }
    files.fail.store(false, Ordering::Relaxed);
    s.execute("crop.deleteAspect", &json!({"name": "Pano"})).unwrap();
    assert_eq!(s.crop_aspects.custom.len(), 1);
}
