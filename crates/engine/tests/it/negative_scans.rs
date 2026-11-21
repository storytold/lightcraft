//! Real film scans through the engine (`docs/negative-conversion.md`): each one imported, set up as its manifest
//! says, converted with `negative.convert`, and checked for where its film base came from and how bright the
//! positive is. Scans of the same negative (a raw camera scan, a JPEG scan, cropped and not) must also land within a
//! few L* of each other, which is what catches a raw-only difference (a camera tone curve, a Kelvin white balance).
//!
//! Scans are private or licensed test material and never committed: they live in the gitignored `corpus/`, listed by
//! a manifest that `LC_NEGATIVE_SCANS` names. Without it the test is skipped.
//!
//! ```text
//! LC_NEGATIVE_SCANS=corpus/negative-scans.json cargo test --release -p lightcraft-engine --test it negative_scans -- --nocapture
//! ```
//!
//! The manifest (paths relative to it, or absolute):
//!
//! ```json
//! {"scans": [
//!   {"file": "Negative with strip.jpeg", "group": "skull", "base": "detected", "lstar": 57.8},
//!   {"file": "/Users/me/Downloads/DSCF1288.RAF", "group": "skull", "base": "detected", "lstar": 57.8,
//!    "settings": {"orientation": "Rotate270", "crop": {"geometry": {"angle": 0.0, "rect": {"x0": 0.0, "y0": 0.133, "x1": 1.0, "y1": 0.819}}}}}
//! ]}
//! ```
//!
//! `base` is the expected `FilmAnalysis::base_source` (`detected`, `estimated`, `sampled`); `lstar` the mean L* of a
//! reference positive of the same frame, within `tolerance` (default 3); `settings` a partial develop-settings object
//! merged before converting (`develop.merge`); `group` names scans of one negative, whose mean L* must agree within
//! `groupTolerance` of the manifest (default 2).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lightcraft_engine::Session;
use lightcraft_engine::media::SourceLevel;
use lightcraft_engine::pipeline::{RenderRequest, render};
use serde_json::{Value, json};

/// Mean CIE L* of the middle 80 % of an sRGB render (the frame's own edges, rebate and holder, left out).
fn mean_lstar(img: &lightcraft_raster::Rgba8) -> f64 {
    let (w, h) = (img.width, img.height);
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let (mut sum, mut n) = (0.0, 0usize);
    for y in h / 10..h - h / 10 {
        for x in w / 10..w - w / 10 {
            let p = img.data[y * w + x];
            let yy = 0.2126 * lin(p[0]) + 0.7152 * lin(p[1]) + 0.0722 * lin(p[2]);
            sum += if yy > 0.008856 { 116.0 * yy.cbrt() - 16.0 } else { 903.3 * yy };
            n += 1;
        }
    }
    sum / n.max(1) as f64
}

fn resolve(manifest: &Path, file: &str) -> PathBuf {
    let p = PathBuf::from(file);
    if p.is_absolute() { p } else { manifest.parent().unwrap_or(Path::new(".")).join(p) }
}

#[test]
fn real_negative_scans_convert_as_their_manifest_says() {
    let Some(manifest) = std::env::var_os("LC_NEGATIVE_SCANS").map(PathBuf::from) else {
        lightcraft_testkit::skip!(Corpus, "LC_NEGATIVE_SCANS is not set (a manifest of film scans, see the file's header)");
        return;
    };
    let m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    let group_tolerance = m["groupTolerance"].as_f64().unwrap_or(2.0);
    let mut groups: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    let mut failures = Vec::new();
    for scan in m["scans"].as_array().unwrap() {
        let file = resolve(&manifest, scan["file"].as_str().unwrap());
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        if !file.is_file() {
            lightcraft_testkit::skip!(Corpus, "{} absent", file.display());
            continue;
        }
        let mut s = Session::new().with_fs();
        let r = s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
        let id = r["imported"][0].as_u64().unwrap_or_else(|| panic!("{name}: not imported ({r})"));
        s.execute("library.select", &json!({"ids": [id], "active": id})).unwrap();
        if let Some(settings) = scan.get("settings") {
            s.execute("develop.merge", &json!({"settings": settings})).unwrap();
        }
        let r = s.execute("negative.convert", &json!({})).unwrap();
        let a = &r["photos"][0]["analysis"];
        let pid = lightcraft_engine::catalog::PhotoId(id);
        let src = s.source_now(pid, SourceLevel::Preview).unwrap();
        let info = s.source_info(pid);
        let d = s.develop_of(pid).unwrap();
        let lstar = mean_lstar(&render(&src, &info, &d, &RenderRequest::fit(800, 800)).image);
        println!("{name}: base {} {}, mean L* {lstar:.1}", a["base_source"], a["base"]);
        if let Some(want) = scan["base"].as_str()
            && a["base_source"] != want
        {
            failures.push(format!("{name}: film base {} (want {want})", a["base_source"]));
        }
        if let Some(want) = scan["lstar"].as_f64() {
            let tol = scan["tolerance"].as_f64().unwrap_or(3.0);
            if (lstar - want).abs() > tol {
                failures.push(format!("{name}: mean L* {lstar:.1} (want {want} ± {tol})"));
            }
        }
        if let Some(g) = scan["group"].as_str() {
            groups.entry(g.to_string()).or_default().push((name, lstar));
        }
    }
    for (g, scans) in &groups {
        let lo = scans.iter().map(|s| s.1).fold(f64::INFINITY, f64::min);
        let hi = scans.iter().map(|s| s.1).fold(f64::NEG_INFINITY, f64::max);
        if hi - lo > group_tolerance {
            failures.push(format!("group {g}: scans of one negative differ by {:.1} L* ({scans:?})", hi - lo));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
