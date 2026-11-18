//! `<cli> compare` end to end: two renders of one synthetic photo, JSON and text reports, the ΔE00 map, and errors
//! (not panics) for bad arguments and files.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");

/// The test's folder in Temp, removed when the test ends.
fn scratch(name: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-cli-compare-{name}"))
}

/// A textured colour card; `warm` pushes red up and blue down.
fn write_card(path: &Path, warm: f32) {
    let (w, h) = (360usize, 240usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let hue = x as f32 / w as f32 * std::f32::consts::TAU;
            let v = 0.2 + 0.6 * y as f32 / h as f32;
            let tex = 0.06 * ((x as f32 * 0.41).sin() * (y as f32 * 0.29).cos());
            let c = [hue.cos() + warm, (hue + 2.1).cos(), (hue + 4.2).cos() - warm];
            rgb.extend(c.map(|c| (((0.5 + 0.35 * c) * v + tex).clamp(0.0, 1.0) * 255.0).round() as u8));
        }
    }
    let img = lcc_codecs::EncodeImage::new(w as u32, h as u32, 3, lcc_codecs::Samples::U8(&rgb));
    std::fs::write(path, lcc_codecs::encode_png(&img, &lcc_codecs::EncodeMeta::default()).unwrap()).unwrap();
}

#[test]
fn compare_reports_the_difference_between_two_renders() {
    let d = scratch("report");
    let (a, b, map) = (d.join("ours.png"), d.join("theirs.png"), d.join("map.png"));
    write_card(&a, 0.0);
    write_card(&b, 0.15);

    let out = Command::new(BIN).args(["compare"]).arg(&a).arg(&a).arg("--json").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let same: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mean_same = same["overall"]["mean"].as_f64().unwrap();

    let out = Command::new(BIN).args(["compare"]).arg(&a).arg(&b).args(["--json", "--diff"]).arg(&map).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let diff: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(diff["overall"]["mean"].as_f64().unwrap() > mean_same + 1.0, "a warmer render differs: {diff}");
    assert!(!diff["hues"].as_array().unwrap().is_empty() && !diff["tones"].as_array().unwrap().is_empty());
    let png = std::fs::read(&map).unwrap();
    assert!(lcc_codecs::decode(&png, lcc_codecs::DecodeOptions::default()).is_ok(), "the ΔE00 map is a readable PNG");

    let out = Command::new(BIN).args(["compare"]).arg(&a).arg(&b).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("overall"));
}

#[test]
fn compare_rejects_bad_input_with_a_message() {
    let d = scratch("bad");
    let good = d.join("good.png");
    write_card(&good, 0.0);
    let junk = d.join("junk.tif");
    std::fs::write(&junk, b"II*\0garbage").unwrap();
    let g = good.to_str().unwrap();
    for args in [
        vec!["compare"],
        vec!["compare", g],
        vec!["compare", g, g, g],
        vec!["compare", g, junk.to_str().unwrap()],
        vec!["compare", g, "/nonexistent/x.jpg"],
        vec!["compare", g, g, "--size"],
        vec!["compare", g, g, "--size", "lots"],
        vec!["compare", g, g, "--bogus"],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert!(!out.status.success(), "{args:?} should fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("panicked"), "{args:?}: {err}");
        assert!(!err.trim().is_empty(), "{args:?} says why");
    }
}
