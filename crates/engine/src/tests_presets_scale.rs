//! A library with thousands of user presets (a Lightroom preset collection imported whole) stays
//! responsive: commands that don't touch presets never re-serialize them, and opening or importing
//! doesn't compare every preset with every other.

use std::path::PathBuf;
use std::time::Instant;

use lightcraft_develop::Preset;
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
fn preset_list_generation_follows_mutable_access_only() {
    let mut s = Session::new();
    let g0 = s.presets.generation();
    let _ = s.presets.iter().filter(|p| p.builtin).count();
    let _ = s.presets.len();
    assert_eq!(s.presets.generation(), g0, "reads keep the generation");
    s.presets.push(many(1).remove(0));
    let g1 = s.presets.generation();
    assert_ne!(g1, g0);
    for p in &mut s.presets {
        p.favorite = false;
    }
    assert_ne!(s.presets.generation(), g1);
    // generations are unique across sessions (UI caches key on them alone)
    let other = Session::new();
    assert_ne!(other.presets.generation(), s.presets.generation());
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

/// Timing with a real preset collection: `LIGHTCRAFT_BENCH_PRESETS=path/to/presets.json cargo test
/// -p lightcraft-engine --release presets_scale_bench -- --ignored --nocapture`. The file is copied
/// into a temporary library; the original is only read.
#[test]
#[ignore]
fn presets_scale_bench() {
    let Ok(src) = std::env::var("LIGHTCRAFT_BENCH_PRESETS") else {
        eprintln!("set LIGHTCRAFT_BENCH_PRESETS");
        return;
    };
    let lib = temp_dir("bench");
    std::fs::copy(&src, lib.join("presets.json")).unwrap();
    let mut s = Session::new();
    let t = Instant::now();
    s.open_library(&lib, true).unwrap();
    eprintln!("open library: {:?} ({} presets)", t.elapsed(), s.presets.len());
    let t = Instant::now();
    for i in 0..20 {
        s.execute("develop.set", &json!({"control": "light.exposure", "value": i as f64 / 10.0})).unwrap();
    }
    eprintln!("develop.set: {:?} per command", t.elapsed() / 20);
    let pid = s.presets.iter().rev().find(|p| !p.builtin).map(|p| p.id.clone()).unwrap();
    let t = Instant::now();
    s.execute("preset.apply", &json!({"id": pid, "amount": 100})).unwrap();
    eprintln!("preset.apply: {:?}", t.elapsed());
    let t = Instant::now();
    s.execute("preset.favorite", &json!({"id": pid})).unwrap();
    eprintln!("preset.favorite (saves presets.json): {:?}", t.elapsed());
    let _ = std::fs::remove_dir_all(&lib);
}
