//! `calibrate-lightroom probe` / `probe-check` end to end, synthetic: probes for a real camera model, and stand-in
//! "Lightroom exports" made by the app itself — the base probe as it renders it, the −1 EV step rendered at −1 EV
//! (mapped right) and the +1 EV step rendered at +1.5 EV (as if Lightroom's +1 looked like the app's +1.5). The check
//! must find the default renders alike, leave the −1 step where the import maps it and move the +1 step to about
//! +1.5. No Adobe data is involved.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");

/// The test's folder in Temp, removed when the test ends.
fn scratch(name: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-cli-probe-e2e-{name}"))
}

fn run(cmd: &mut Command) -> String {
    let o = cmd.output().unwrap();
    let (out, err) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "{cmd:?} failed:\n{out}\n{err}");
    assert!(!err.contains("panicked"), "{err}");
    out.into_owned()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn render(probe: &Path, out: &Path, exposure: Option<f64>) {
    let mut args = vec!["render".to_string(), s(probe).into(), "-o".into(), s(out).into()];
    for o in ["bitDepth=16", "colorSpace=proPhoto", "metadata=none"] {
        args.extend(["--opt".to_string(), o.to_string()]);
    }
    if let Some(ev) = exposure {
        args.extend(["--set".to_string(), format!("light.exposure={ev}")]);
    }
    run(Command::new(BIN).args(&args));
}

#[test]
fn probe_check_measures_how_lightroom_sliders_map() {
    let root = scratch("main");
    let (probes, exports) = (root.join("probes"), root.join("exports"));
    std::fs::create_dir_all(&exports).unwrap();
    run(Command::new(BIN).args(["calibrate-lightroom", "probe", "-o", s(&probes), "--make", "SONY", "--model", "ILCE-7M3"]));
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(probes.join("probe.json")).unwrap()).unwrap();
    assert!(manifest["files"].as_array().unwrap().len() > 50);
    let base = probes.join("probe_base.dng");
    assert!(base.is_file() && probes.join("probe_exposure2012_p1.dng").is_file() && probes.join("probe_exposure2012_m1.dng").is_file());

    render(&base, &exports.join("probe_base.tif"), None);
    render(&base, &exports.join("probe_exposure2012_m1.tif"), Some(-1.0));
    render(&base, &exports.join("probe_exposure2012_p1.tif"), Some(1.5));

    let out = run(Command::new(BIN).args(["calibrate-lightroom", "probe-check", "--probes", s(&probes), "--exports", s(&exports), "--json"]));
    let r: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(r["base"]["median"].as_f64().unwrap() < 1.0, "the app's default render matches its own: {}", r["base"]);
    let step = |stem: &str| r["steps"].as_array().unwrap().iter().find(|s| s["stem"] == stem).cloned().unwrap_or_else(|| panic!("{stem}: {r}"));
    let minus = step("probe_exposure2012_m1");
    assert_eq!(minus["control"], "light.exposure");
    assert_eq!(minus["mapped"].as_f64(), Some(-1.0));
    assert!((minus["best"].as_f64().unwrap() + 1.0).abs() < 0.15, "{minus}");
    let plus = step("probe_exposure2012_p1");
    assert_eq!(plus["mapped"].as_f64(), Some(1.0));
    assert!((plus["best"].as_f64().unwrap() - 1.5).abs() < 0.15, "Lightroom's +1 looks like the app's +1.5: {plus}");
    assert!(plus["error_best"].as_f64().unwrap() < plus["error_mapped"].as_f64().unwrap() * 0.5, "{plus}");
    assert!(r["missing"].as_array().unwrap().len() > 40, "the steps without an export are listed");

    let text = run(Command::new(BIN).args(["calibrate-lightroom", "probe-check", "--probes", s(&probes), "--exports", s(&exports)]));
    assert!(text.contains("light.exposure") && text.contains("default render"), "{text}");
}

#[test]
fn probe_commands_reject_bad_input_with_a_message() {
    let root = scratch("bad");
    let r = s(&root);
    let junk = root.join("junk.arw");
    std::fs::write(&junk, b"not a raw").unwrap();
    for args in [
        vec!["calibrate-lightroom", "probe"],
        vec!["calibrate-lightroom", "probe", "-o", r],
        vec!["calibrate-lightroom", "probe", "-o", r, "--make", "Nobody", "--model", "Nothing 1"],
        vec!["calibrate-lightroom", "probe", "-o", r, "--like", junk.to_str().unwrap()],
        vec!["calibrate-lightroom", "probe", "-o", r, "--bogus"],
        vec!["calibrate-lightroom", "probe-check"],
        vec!["calibrate-lightroom", "probe-check", "--probes", r, "--exports", r],
        vec!["calibrate-lightroom", "probe-check", "--probes", "/nonexistent", "--exports", "/nonexistent"],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert!(!out.status.success(), "{args:?} should fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("panicked") && !err.trim().is_empty(), "{args:?}: {err}");
    }
}
