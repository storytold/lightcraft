//! File ▸ Import Profiles & Presets…: `.cube` LUTs become profiles, everything else presets, and
//! one toast says what happened to both (issue #942).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn app() -> Headless {
    // Profile import needs no photos. Avoid unrelated demo renders while testing its notice.
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    assert!(h.settle(SETTLE), "the empty library settles before importing profiles");
    h
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-ui-preset-import-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn import(h: &mut Headless, paths: &[&std::path::Path]) -> serde_json::Value {
    let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
    // Import and its toast complete synchronously before the control reply. Settling
    // unrelated rendering afterwards advances simulated time and can expire the notice.
    h.request("engine.execute", json!({"command": "file.importPresets", "params": {"paths": paths}}), T)
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.clone().map(|t| t.0).unwrap_or_default()
}

/// A 2×2×2 identity LUT.
fn identity_cube() -> String {
    let mut s = String::from("TITLE \"Identity\"\nLUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                s += &format!("{r} {g} {b}\n");
            }
        }
    }
    s
}

/// Importing only `.cube` files reports the profiles, not "No new presets".
#[test]
fn a_cube_only_import_reports_the_profiles() {
    let dir = temp_dir("ok");
    let cube = dir.join("Warm.cube");
    std::fs::write(&cube, identity_cube()).unwrap();
    let mut h = app();
    let r = import(&mut h, &[&cube]);
    assert_eq!(r["ok"], true, "{r}");
    let t = toast(&h);
    assert!(t.contains("1 profile"), "{t}");
    assert!(!t.contains("preset"), "{t}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.cube` that can't be imported says which file and why.
#[test]
fn a_broken_cube_says_why() {
    let dir = temp_dir("bad");
    let bad = dir.join("Broken.cube");
    std::fs::write(&bad, "LUT_3D_SIZE 2\n0 0 0\n").unwrap();
    let mut h = app();
    let r = import(&mut h, &[&bad]);
    assert_eq!(r["ok"], true, "{r}");
    let t = toast(&h);
    assert!(t.contains("Broken"), "{t}");
    assert!(!t.contains("No new presets"), "{t}");
    let reason = r["result"]["profiles"]["failed"][0][1].as_str().unwrap_or_default().to_string();
    assert!(!reason.is_empty() && t.contains(&reason), "{t} / {r}");
    // with a good one alongside, both outcomes are in the one toast
    let good = dir.join("Good.cube");
    std::fs::write(&good, identity_cube()).unwrap();
    import(&mut h, &[&good, &bad]);
    let t = toast(&h);
    assert!(t.contains("1 profile") && t.contains("Broken"), "{t}");
    let _ = std::fs::remove_dir_all(&dir);
}
