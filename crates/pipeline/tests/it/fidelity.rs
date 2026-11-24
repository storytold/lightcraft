//! The render-fidelity reference suite (gap 3, `docs/testing-sil.md` → *Render fidelity*).
//!
//! Each tool (a develop control pushed to a telling value) renders a set of procedural `lcc-scenes`
//! photos, plus up to four CC0 photos from the gitignored `corpus/images/` when it exists, under
//! every process version, and compares each render with the reference stored locally in
//! `fidelity-refs/` (gitignored; `<PREFIX>_FIDELITY_REFS` names another folder). Only aggregate
//! numbers are committed: the per-tool tolerances in `tests/fidelity/tolerances.json` (mean
//! ΔE2000, 95th-percentile ΔE2000, SSIM). `cargo xtask fidelity --regen` (`<PREFIX>_FIDELITY_REGEN=1`)
//! writes the references from the current renderer; without references the suite skips.
//!
//! References are whatever the folder holds: regenerated from `main` before a process-version
//! retune, they gate the retune (old processes must still match), and renders of the same inputs
//! made by another editor can be dropped in to measure against it (they never leave the machine).
//!
//! The ColorChecker check needs no references: the synthetic chart of `lcc_fidelity::chart`
//! renders through the pipeline unedited and must read back near the published values.

use std::path::PathBuf;

use lcc_color::{REC2020, SRGB, transfer::srgb_to_linear};
use lcc_develop::{DevelopSettings, Process};
use lcc_fidelity::{Comparison, RefStore, chart, compare, measure_chart};
use lcc_pipeline::{RenderRequest, SourceInfo, render};
use lcc_raster::{Rgb32f, Rgba8};

const EDGE: usize = 128;

type Tool = (&'static str, fn(&mut DevelopSettings));

/// The tools measured, each pushed far enough to matter.
const TOOLS: &[Tool] = &[
    ("default", |_| {}),
    ("light.exposure", |s| s.light.exposure = 1.0),
    ("light.contrast", |s| s.light.contrast = 60.0),
    ("light.highlights", |s| s.light.highlights = -80.0),
    ("light.shadows", |s| s.light.shadows = 80.0),
    ("light.whites", |s| s.light.whites = 50.0),
    ("light.blacks", |s| s.light.blacks = -50.0),
    ("color.vibrance", |s| s.color.vibrance = 60.0),
    ("color.saturation", |s| s.color.saturation = -50.0),
    ("wb.temp", |s| s.wb.temp = 40.0),
    ("effects.texture", |s| s.effects.texture = 60.0),
    ("effects.clarity", |s| s.effects.clarity = 60.0),
    ("effects.dehaze", |s| s.effects.dehaze = 50.0),
    ("detail.sharpen", |s| s.detail.sharpen_amount = 100.0),
    ("detail.nr_luminance", |s| s.detail.nr_luminance = 60.0),
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn to_working(img: &image::RgbImage) -> Rgb32f {
    let m = SRGB.to_space(&REC2020);
    let (w, h) = (img.width() as usize, img.height() as usize);
    Rgb32f::from_fn(w, h, |x, y| {
        let p = img.get_pixel(x as u32, y as u32).0;
        m.apply_f32(p.map(|v| srgb_to_linear(f32::from(v) / 255.0)))
    })
}

/// The inputs: procedural scenes, then CC0 corpus photos when present.
fn cases() -> Vec<(String, Rgb32f)> {
    let mut out: Vec<(String, Rgb32f)> =
        lcc_scenes::demo_library().iter().take(4).map(|s| (format!("scene-{}", s.id), s.render_fit_serial(EDGE * 2))).collect();
    let dir = workspace_root().join("corpus").join("images");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg"))).collect())
        .unwrap_or_default();
    files.sort();
    for p in files.into_iter().take(4) {
        let small = std::fs::metadata(&p).is_ok_and(|m| m.len() < 32 << 20);
        if let (true, Ok(img)) = (small, image::open(&p)) {
            let img = img.thumbnail(EDGE as u32 * 2, EDGE as u32 * 2).to_rgb8();
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            out.push((format!("corpus-{name}"), to_working(&img)));
        }
    }
    out
}

fn settings(tool: fn(&mut DevelopSettings), p: Process) -> DevelopSettings {
    let mut s = DevelopSettings { process: p.version(), ..DevelopSettings::default() };
    tool(&mut s);
    s
}

fn rendered(src: &Rgb32f, s: &DevelopSettings) -> Rgba8 {
    render(src, &SourceInfo::default(), s, &RenderRequest::fit(EDGE, EDGE)).image
}

#[derive(Debug, Clone, Copy)]
struct Tolerance {
    mean_de2000: f64,
    p95_de2000: f64,
    min_ssim: f64,
}

fn tolerances() -> serde_json::Value {
    let text = include_str!("../fidelity/tolerances.json");
    serde_json::from_str(text).unwrap_or_else(|e| panic!("tests/fidelity/tolerances.json: {e}"))
}

fn tolerance(all: &serde_json::Value, tool: &str) -> Tolerance {
    let get = |key: &str, field: &str| all.get("tools").and_then(|t| t.get(key)).and_then(|t| t.get(field)).and_then(serde_json::Value::as_f64);
    let pick = |field: &str| get(tool, field).or_else(|| get("*", field)).unwrap_or_else(|| panic!("no tolerance {field} for {tool}"));
    Tolerance { mean_de2000: pick("mean_de2000"), p95_de2000: pick("p95_de2000"), min_ssim: pick("min_ssim") }
}

#[test]
fn renders_match_the_local_references_within_tolerance() {
    let root = lcc_brand::env_os("FIDELITY_REFS").map(PathBuf::from).unwrap_or_else(|| workspace_root().join("fidelity-refs"));
    let store = RefStore::new(&root);
    let regen = lcc_brand::env_is_set("FIDELITY_REGEN");
    if !regen && !root.is_dir() {
        lcc_testkit::skip!(Input, "no render-fidelity references in {} (cargo xtask fidelity --regen)", root.display());
        return;
    }
    let cases = cases();
    let all = tolerances();
    let mut report = serde_json::Map::new();
    let mut failures = Vec::new();
    let mut missing = 0usize;
    for (tool, apply) in TOOLS {
        let tol = tolerance(&all, tool);
        let mut cmps: Vec<Comparison> = Vec::new();
        for p in Process::ALL {
            let s = settings(*apply, p);
            for (name, src) in &cases {
                let case = format!("v{}-{name}", p.version().0);
                let img = rendered(src, &s);
                if regen {
                    store.save(tool, &case, &img).unwrap_or_else(|e| panic!("{e}"));
                    continue;
                }
                match store.load(tool, &case) {
                    Ok(Some(reference)) => match compare(&img, &reference) {
                        Ok(c) => cmps.push(c),
                        Err(e) => failures.push(format!("{tool} {case}: {e}")),
                    },
                    Ok(None) => missing += 1,
                    Err(e) => failures.push(format!("{tool} {case}: {e}")),
                }
            }
        }
        if cmps.is_empty() {
            continue;
        }
        let n = cmps.len() as f64;
        let mean = cmps.iter().map(|c| c.mean_de2000).sum::<f64>() / n;
        let p95 = cmps.iter().map(|c| c.p95_de2000).fold(0.0, f64::max);
        let ssim = cmps.iter().map(|c| c.ssim).fold(1.0, f64::min);
        report.insert((*tool).into(), serde_json::json!({ "mean_de2000": mean, "p95_de2000": p95, "min_ssim": ssim, "renders": cmps.len() }));
        if mean > tol.mean_de2000 || p95 > tol.p95_de2000 || ssim < tol.min_ssim {
            failures.push(format!(
                "{tool}: mean ΔE00 {mean:.3} (≤ {}), p95 {p95:.3} (≤ {}), SSIM {ssim:.4} (≥ {})",
                tol.mean_de2000, tol.p95_de2000, tol.min_ssim
            ));
        }
    }
    if regen {
        eprintln!(
            "fidelity: wrote references for {} tools × {} processes × {} inputs to {}",
            TOOLS.len(),
            Process::ALL.len(),
            cases.len(),
            root.display()
        );
        return;
    }
    if report.is_empty() {
        lcc_testkit::skip!(Input, "no render-fidelity references in {} (cargo xtask fidelity --regen)", root.display());
        return;
    }
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(report)).unwrap_or_default();
    let _ = std::fs::write(root.join("report.json"), &text);
    eprintln!("fidelity (aggregate per tool; {missing} renders without a reference):\n{text}");
    assert!(failures.is_empty(), "render fidelity outside tolerance:\n{}", failures.join("\n"));
}

/// The unedited pipeline keeps the synthetic ColorChecker near its published values under every
/// process (the base line plan item 3.1's per-body checks build on), and the measure notices a
/// colour change.
#[test]
fn the_colorchecker_chart_survives_the_unedited_pipeline() {
    let src = chart(&REC2020, 300, 200);
    let tol = tolerances().get("colorchecker_mean_de2000").and_then(serde_json::Value::as_f64).unwrap_or(f64::NAN);
    for p in Process::ALL {
        let de = measure_chart(&rendered_at(&src, &settings(|_| {}, p))).unwrap_or_else(|e| panic!("{e}"));
        assert!(de.mean <= tol, "{p:?}: chart mean ΔE00 {:.3} > {tol}; per patch {:?}", de.mean, de.patches);
        let warm = measure_chart(&rendered_at(&src, &settings(|s| s.color.saturation = -100.0, p))).unwrap_or_else(|e| panic!("{e}"));
        assert!(warm.mean > de.mean + 1.0, "{p:?}: removing the colour should move the chart ({:.3} → {:.3})", de.mean, warm.mean);
    }
}

fn rendered_at(src: &Rgb32f, s: &DevelopSettings) -> Rgba8 {
    render(src, &SourceInfo::default(), s, &RenderRequest::fit(src.width, src.height)).image
}

/// Mean CIE L*a*b* (D50) of a render.
fn mean_lab(img: &Rgba8) -> [f64; 3] {
    let mut acc = [0.0f64; 3];
    for px in &img.data {
        for (a, v) in acc.iter_mut().zip(lcc_color::lab::srgb8_to_lab([px[0], px[1], px[2]])) {
            *a += v;
        }
    }
    acc.map(|v| v / img.data.len().max(1) as f64)
}

/// One (tool, process) aggregate of the committed process baseline: averaged over the procedural
/// scenes, the render's mean L*a*b* and its mean ΔE2000 from the same process's unedited render.
fn process_aggregate(tool: fn(&mut DevelopSettings), p: Process, scenes: &[Rgb32f], plains: &[Rgba8]) -> [f64; 4] {
    let mut acc = [0.0f64; 4];
    for (src, plain) in scenes.iter().zip(plains) {
        let img = rendered(src, &settings(tool, p));
        let lab = mean_lab(&img);
        let de = compare(&img, plain).map(|c| c.mean_de2000).unwrap_or(f64::NAN);
        for (a, v) in acc.iter_mut().zip([lab[0], lab[1], lab[2], de]) {
            *a += v;
        }
    }
    acc.map(|v| v / scenes.len().max(1) as f64)
}

/// The process-retune gate (runs in CI, needs no local references): every process other than
/// [`Process::LATEST`] must render each tool on the procedural scenes as its committed aggregates in
/// `tests/fidelity/process-baseline.json` say (mean L\*, a\*, b\* and the tool's ΔE2000 from the
/// unedited render), and every process and tool must have an entry. A retune of a frozen process, or
/// a new process without a baseline, fails here; `cargo xtask fidelity --baseline`
/// (`<PREFIX>_FIDELITY_BASELINE=1`) rewrites the file, which makes the change visible in review.
#[test]
fn frozen_processes_keep_their_committed_baseline() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fidelity").join("process-baseline.json");
    let scenes: Vec<Rgb32f> = lcc_scenes::demo_library().iter().take(4).map(|s| s.render_fit_serial(EDGE * 2)).collect();
    if lcc_brand::env_is_set("FIDELITY_BASELINE") {
        let mut processes = serde_json::Map::new();
        for p in Process::ALL {
            let plains: Vec<Rgba8> = scenes.iter().map(|src| rendered(src, &settings(|_| {}, p))).collect();
            let mut tools = serde_json::Map::new();
            for (tool, apply) in TOOLS {
                let [l, a, b, de] = process_aggregate(*apply, p, &scenes, &plains).map(|v| (v * 1000.0).round() / 1000.0);
                tools.insert((*tool).into(), serde_json::json!([l, a, b, de]));
            }
            processes.insert(format!("V{}", p.version().0), serde_json::Value::Object(tools));
        }
        let doc = serde_json::json!({
            "about": "Process-retune gate (crates/pipeline/tests/it/fidelity.rs, docs/testing-sil.md): per process and tool, [mean L*, a*, b*, mean ΔE2000 from the unedited render] averaged over the procedural scenes. Aggregate numbers only. Rewrite with cargo xtask fidelity --baseline.",
            "tolerance": 0.35,
            "processes": processes,
        });
        let text = serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n";
        std::fs::write(&path, text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        eprintln!("fidelity: wrote the process baseline to {}", path.display());
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e} (cargo xtask fidelity --baseline)", path.display()));
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let tol = doc.get("tolerance").and_then(serde_json::Value::as_f64).unwrap_or(f64::NAN);
    assert!(tol.is_finite() && tol > 0.0, "process-baseline.json: no tolerance");
    let mut failures = Vec::new();
    for p in Process::ALL {
        let key = format!("V{}", p.version().0);
        let plains: Vec<Rgba8> =
            if p == Process::LATEST { Vec::new() } else { scenes.iter().map(|src| rendered(src, &settings(|_| {}, p))).collect() };
        for (tool, apply) in TOOLS {
            let Some(want) = doc.get("processes").and_then(|ps| ps.get(&key)).and_then(|t| t.get(*tool)).and_then(serde_json::Value::as_array) else {
                failures.push(format!("{key} {tool}: no baseline (cargo xtask fidelity --baseline)"));
                continue;
            };
            if p == Process::LATEST {
                continue;
            }
            let got = process_aggregate(*apply, p, &scenes, &plains);
            let want: Vec<f64> = want.iter().filter_map(serde_json::Value::as_f64).collect();
            let moved = want.len() != 4
                || got.iter().zip(&want).any(|(g, w)| (g - w).abs().partial_cmp(&tol) != Some(std::cmp::Ordering::Less) && (g - w).abs() != tol);
            if moved {
                failures.push(format!("{key} {tool}: [L a b ΔE] {got:.3?}, baseline {want:.3?} (± {tol})"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "a frozen process renders differently (a retune belongs in a new process, docs/process-versions.md; \
         a deliberate in-place fix rewrites the baseline with cargo xtask fidelity --baseline):\n{}",
        failures.join("\n")
    );
}
