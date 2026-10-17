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
