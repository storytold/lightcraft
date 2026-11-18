//! A library with thousands of user presets (a Lightroom preset collection imported whole) stays
//! responsive: commands that don't touch presets never re-serialize them, and opening or importing
//! doesn't compare every preset with every other.

use std::path::PathBuf;
use std::time::Instant;

use lcc_develop::Preset;
use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-presets-scale-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn many(n: usize) -> Vec<Preset> {
    (0..n)
        .map(|i| Preset {
            id: format!("user.coll-{}.look-{i}", i / 100),
            name: format!("Look {i}"),
            group: format!("Collection {}", i / 100),
            settings: json!({"light": {"exposure": (i % 40) as f64 / 10.0 - 2.0, "contrast": (i % 50) as f64}, "color": {"vibrance": (i % 30) as f64}}),
            favorite: false,
            builtin: false,
        })
        .collect()
}

#[test]
fn develop_commands_do_not_reserialize_unchanged_presets() {
    let lib = temp_dir("no-reserialize");
    let mut s = Session::new();
    s.open_library(&lib, true).unwrap();
    let added = s.add_presets(many(2000));
    assert_eq!(added.len(), 2000);
    s.persist().unwrap();
    assert!(std::fs::metadata(lib.join("presets.json")).unwrap().len() > 100_000, "presets saved");
    let before = crate::library::presets_serialized();
    for i in 0..20 {
        s.execute("develop.set", &json!({"control": "light.exposure", "value": i as f64 / 10.0})).unwrap();
    }
    assert_eq!(crate::library::presets_serialized(), before, "slider commands left the preset list alone");
    // a preset change is still saved
    let pid = added[7].clone();
    s.execute("preset.favorite", &json!({"id": pid})).unwrap();
    assert!(crate::library::presets_serialized() > before);
    s.close_library().unwrap();
    drop(s);
    let mut t = Session::new();
    t.open_library(&lib, false).unwrap();
    assert!(t.presets.iter().any(|p| p.id == pid && p.favorite), "favourite persisted");
    assert_eq!(t.presets.iter().filter(|p| !p.builtin).count(), 2000);
    let _ = std::fs::remove_dir_all(&lib);
}

/// With background saves on (the desktop app), a preset change doesn't write presets.json inside
/// the command: the file follows on the saver's thread, and closing the library waits for it.
#[test]
fn presets_are_saved_off_the_ui_thread_when_asked() {
    let lib = temp_dir("background");
    let mut s = Session::new();
    s.open_library(&lib, true).unwrap();
    s.background_saves = true;
    let added = s.add_presets(many(3000));
    let before = crate::library::presets_serialized();
    s.persist().unwrap();
    assert_eq!(crate::library::presets_serialized(), before, "not serialized on this thread");
    s.flush_presets();
    let text = std::fs::read_to_string(lib.join("presets.json")).unwrap();
    assert!(text.contains(&added[2999]), "the saver wrote the new presets");
    // a favourite toggled, the library closed at once: the save is waited for
    let pid = added[11].clone();
    s.execute("preset.favorite", &json!({"id": pid})).unwrap();
    s.close_library().unwrap();
    drop(s);
    let mut t = Session::new();
    t.open_library(&lib, false).unwrap();
    assert!(t.presets.iter().any(|p| p.id == pid && p.favorite), "favourite saved before the library closed");
    assert_eq!(t.presets.iter().filter(|p| !p.builtin).count(), 3000);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn importing_thousands_of_presets_is_not_quadratic() {
    let mut s = Session::new();
    let t = Instant::now();
    let added = s.add_presets(many(20_000));
    assert_eq!(added.len(), 20_000);
    // the same presets again are all skipped as duplicates
    assert!(s.add_presets(many(20_000)).is_empty());
    // a clashing id with different settings gets a fresh, unique one
    let mut clash = many(1).remove(0);
    clash.settings = json!({"light": {"exposure": 3.0}});
    let id = s.add_presets(vec![clash]).remove(0);
    assert_ne!(id, "user.coll-0.look-0");
    assert_eq!(s.presets.iter().filter(|p| p.id == id).count(), 1);
    // quadratic was ~200 M comparisons here (tens of seconds in debug builds)
    assert!(t.elapsed().as_secs() < 20, "took {:?}", t.elapsed());
}

/// Presets sharing a name and id all get unique ids in one call, and the result says which of the
/// batch were skipped as already present.
#[test]
fn presets_sharing_a_name_and_id_get_unique_ids_and_repeats_are_reported() {
    let mut s = Session::new();
    let same: Vec<Preset> = (0..5_000)
        .map(|i| Preset {
            id: "user.x".into(),
            name: "Look".into(),
            group: "Coll".into(),
            settings: json!({"light": {"exposure": i}}),
            favorite: false,
            builtin: false,
        })
        .collect();
    let each = s.add_presets_each(same);
    assert_eq!(each.len(), 5_000);
    assert!(each.iter().all(Option::is_some));
    let ids: std::collections::HashSet<_> = s.presets.iter().filter(|p| p.group == "Coll").map(|p| p.id.as_str()).collect();
    assert_eq!(ids.len(), 5_000);
    let Some(last) = s.presets.last().cloned() else { return };
    let again = s.add_presets_each(vec![last.clone(), Preset { settings: json!({"light": {"exposure": -1}}), ..last }]);
    assert_eq!(again.first(), Some(&None));
    assert!(again.get(1).is_some_and(Option::is_some));
}

/// Hostile import (upstream #3680): `0`, `0.0` and `-0.0` are one number, so thousands of presets
/// spelling the same settings in different ways are one preset, found without a linear scan.
#[test]
fn integer_and_float_spellings_of_one_number_are_one_preset() {
    let spellings = [json!(0), json!(0.0), json!(-0.0)];
    let many_spelled: Vec<Preset> = (0..2_000usize)
        .map(|i| {
            let slots: serde_json::Map<String, serde_json::Value> =
                (0..15).map(|k| (format!("s{k}"), spellings[(i / 3usize.pow(k % 7)) % 3].clone())).collect();
            Preset {
                id: "user.x".into(),
                name: "Look".into(),
                group: "Coll".into(),
                settings: json!({"light": slots}),
                favorite: false,
                builtin: false,
            }
        })
        .collect();
    let mut s = Session::new();
    let t0 = Instant::now();
    let each = s.add_presets_each(many_spelled);
    assert!(t0.elapsed().as_secs() < 20, "a crafted import must not go quadratic");
    assert!(each.first().is_some_and(Option::is_some));
    // only presets whose numbers differ (0 vs 0.0 vs -0.0 are equal) remain; all the slots here are zero
    assert_eq!(each.iter().filter(|e| e.is_some()).count(), 1);
}

/// With the shared settings store (`Library Store`), a command that leaves the presets alone
/// doesn't copy or serialize them: shared.json is checked after every command, and building it
/// from a deep copy of every preset made each command (and the frame loop) take seconds with a
/// ten-thousand-preset collection.
#[test]
fn commands_with_the_shared_store_do_not_copy_thousands_of_presets() {
    let root = temp_dir("shared-store");
    let (store, lib) = (root.join("store"), root.join("lib"));
    let mut s = Session::new().with_app_store(&store);
    s.open_library(&lib, true).unwrap();
    let added = s.add_presets(many(10_000));
    s.preset_hidden = added.iter().take(9_500).cloned().collect();
    s.persist().unwrap();
    let _ = crate::app_store::content_copies();
    let t = Instant::now();
    for i in 0..20 {
        s.execute("develop.set", &json!({"control": "light.exposure", "value": i as f64 / 10.0})).unwrap();
    }
    let per = t.elapsed() / 20;
    eprintln!("develop.set with 10 000 presets in the shared store: {per:?} each");
    assert_eq!(crate::app_store::content_copies(), 0, "no command copied the preset list for shared.json");
    // a mark is still saved
    let pid = added[9_999].clone();
    s.execute("preset.favorite", &json!({"id": pid})).unwrap();
    s.close_library().unwrap();
    drop(s);
    let mut t = Session::new().with_app_store(&store);
    t.open_library(&lib, false).unwrap();
    assert!(t.presets.iter().any(|p| p.id == pid && p.favorite), "favourite saved in the store");
    assert_eq!(t.preset_hidden.len(), 9_500, "hidden marks kept");
    let _ = std::fs::remove_dir_all(&root);
}

/// A real store copied in (`<PREFIX>_BENCH_STORE` = a `Library Store` folder copy): how long a
/// slider command takes with it. Ignored: needs the folder.
#[test]
#[ignore]
fn shared_store_bench() {
    let Some(src) = lcc_brand::env_os("BENCH_STORE") else { return };
    let root = temp_dir("store-bench");
    let store = root.join("store");
    std::fs::create_dir_all(&store).unwrap();
    for e in std::fs::read_dir(&src).unwrap() {
        let e = e.unwrap();
        if e.path().is_file() {
            std::fs::copy(e.path(), store.join(e.file_name())).unwrap();
        }
    }
    let t = Instant::now();
    let mut s = Session::new().with_app_store(&store);
    s.open_library(root.join("lib"), true).unwrap();
    eprintln!("open: {:?}, {} presets, {} hidden", t.elapsed(), s.presets.len(), s.preset_hidden.len());
    s.persist().unwrap();
    let t = Instant::now();
    for i in 0..10 {
        s.execute("develop.set", &json!({"control": "light.exposure", "value": i as f64 / 10.0})).unwrap();
    }
    eprintln!("develop.set: {:?} each", t.elapsed() / 10);
    let t = Instant::now();
    for _ in 0..10 {
        s.persist().unwrap();
    }
    eprintln!("persist alone: {:?} each", t.elapsed() / 10);
    let t = Instant::now();
    for _ in 0..10 {
        let _ = s.content();
    }
    eprintln!("content(): {:?} each", t.elapsed() / 10);
    let _ = std::fs::remove_dir_all(&root);
}
