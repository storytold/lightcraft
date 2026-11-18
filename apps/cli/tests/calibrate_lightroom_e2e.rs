//! M11.4 end to end, synthetic: `calibrate-lightroom fit` recovers a known hidden profile.
//!
//! A fictitious camera shoots 12 procedural colour charts under white balances from tungsten to daylight (DNGs
//! written by `lcc_raw::write_dng`). Stand-in "Lightroom exports" are the app's own 16-bit ProPhoto TIFF renders
//! of those raws with a hidden calibration (matrices, a hue-twisting look table and a tone curve) installed
//! through the debug-only `<PREFIX>_CAMERA_CALIBRATIONS` folder; the export folder's name is the look. The fit
//! must write a calibration `Calibration::parse` accepts whose held-out colour error is small and better than
//! the app's uncalibrated colour, and `add` must put it into a repository with its attribution row.
//! No Adobe data is involved anywhere.

use std::path::{Path, PathBuf};
use std::process::Command;

use lcc_camdb::calibration::{self, Calibration, CameraModel, Evidence, Look};
use lcc_raw::profile::HsvTable;
use lcc_raw::{BlackLevel, Cfa, ColorData, DngWriteOptions, Mat3, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect};

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");
const MAKE: &str = "Synthica";
const MODEL: &str = "SX-1";
const LOOK: &str = "Synth Vivid";
const PHOTOS: usize = 12;

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-cli-lrcal-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// XYZ → camera under illuminant A and D65: what the hidden profile says, and what defines each photo's neutral.
fn hidden_cm() -> [Mat3; 2] {
    [Mat3([[0.92, -0.28, -0.06], [-0.48, 1.38, 0.12], [-0.12, 0.32, 0.58]]), Mat3([[0.82, -0.24, -0.06], [-0.44, 1.30, 0.16], [-0.08, 0.24, 0.70]])]
}

fn hidden_calibration() -> Calibration {
    let fm = Mat3([[0.72, 0.16, 0.08], [0.27, 0.82, -0.09], [0.03, -0.16, 0.96]]);
    let fm2 = Mat3([[0.70, 0.19, 0.07], [0.29, 0.79, -0.08], [0.02, -0.13, 0.94]]);
    // hue twist: rotate hues by up to ±12° (more where saturated) and lift saturation of reds a little
    let (hd, sd) = (36usize, 4usize);
    let mut data = Vec::with_capacity(hd * sd);
    for h in 0..hd {
        for s in 0..sd {
            let hue = h as f32 * 10.0_f32.to_radians();
            let sat = s as f32 / (sd - 1) as f32;
            data.push([12.0 * hue.sin() * sat, 1.0 + 0.15 * hue.cos().max(0.0) * sat, 1.0]);
        }
    }
    let table = HsvTable { hue_divisions: hd, sat_divisions: sd, val_divisions: 1, data, srgb_value: false };
    let curve: Vec<[f32; 2]> = (0..=16)
        .map(|i| {
            let x = i as f32 / 16.0;
            // gentle S around mid grey in a gamma-ish space
            let y = (x + 0.12 * (std::f32::consts::TAU * x).sin() * (1.0 - x)).clamp(0.0, 1.0);
            [x, y]
        })
        .collect();
    Calibration {
        version: calibration::VERSION,
        make: MAKE.into(),
        model: MODEL.into(),
        aliases: vec![],
        lens: None,
        editor: Default::default(),
        method: calibration::METHOD.into(),
        tool_version: "0".into(),
        contributor: None,
        licence: calibration::LICENCE.into(),
        camera: CameraModel { illuminants: [17, 21], color_matrix: hidden_cm(), forward_matrix: [fm, fm2], hue_sat_map: None },
        looks: vec![Look { name: LOOK.into(), baseline_exposure_offset: 0.2, look_table: Some(table), tone_curve: curve }],
        evidence: Evidence::default(),
    }
}

fn mul(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    let r = |i: usize| m.0[i][0] * v[0] + m.0[i][1] * v[1] + m.0[i][2] * v[2];
    [r(0), r(1), r(2)]
}

/// Green-normalised camera neutral for a white between illuminant A (`t` = 0) and D65 (`t` = 1).
fn neutral(t: f64) -> [f64; 3] {
    let [a, d] = hidden_cm();
    let na = mul(&a, [1.0985, 1.0, 0.3558]);
    let nd = mul(&d, [0.9505, 1.0, 1.0891]);
    let n: Vec<f64> = (0..3).map(|i| na[i] / na[1] * (1.0 - t) + nd[i] / nd[1] * t).collect();
    [n[0], n[1], n[2]]
}

/// A tiny deterministic generator.
fn rng(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*state >> 11) as f64) / ((1u64 << 53) as f64)
}

/// Photo `k`: a chart of flat colour patches (camera reflectances, saturated and grey) seen under neutral `n`.
fn photo(k: usize, n: [f64; 3]) -> RawImage {
    let (w, h, cell) = (768usize, 512usize, 64usize);
    let (black, white) = (512.0f64, 16000.0f64);
    let mut seed = 0x9e37_79b9_7f4a_7c15u64 ^ (k as u64 * 7919);
    let (cols, rows) = (w / cell, h / cell);
    let patches: Vec<[f64; 3]> = (0..cols * rows)
        .map(|i| {
            let level = 0.08 + 0.55 * rng(&mut seed);
            if i % 7 == 0 {
                return [level; 3];
            }
            // a random hue at a random saturation, in camera RGB
            let hue = rng(&mut seed) * std::f64::consts::TAU;
            let sat = 0.25 + 0.7 * rng(&mut seed);
            let c = |o: f64| (1.0 + sat * (hue + o).cos()).max(0.02);
            let v = [c(0.0), c(2.1), c(4.2)];
            let m = v.iter().copied().fold(0.0, f64::max);
            v.map(|x| level * x / m)
        })
        .collect();
    let cfa = Cfa::bayer("RGGB").unwrap();
    let mut data = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let p = patches[(y / cell) * cols + x / cell];
            let c = cfa.color_at(x, y) as usize;
            let v = (p[c] * n[c]).clamp(0.0, 1.0);
            data.push((black + v * (white - black)).round() as u16);
        }
    }
    let color = ColorData {
        illuminant: [17, 21],
        // the camera's own (DNG) matrices: not what the hidden profile says, so the app's colour starts off
        color_matrix: [
            Some(Mat3([[1.0, -0.35, -0.02], [-0.42, 1.30, 0.10], [-0.06, 0.20, 0.66]])),
            Some(Mat3([[0.90, -0.30, -0.02], [-0.40, 1.25, 0.14], [-0.04, 0.16, 0.78]])),
        ],
        as_shot_neutral: Some(n),
        ..Default::default()
    };
    let mut raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(cfa),
        bits: 14,
        black: BlackLevel::uniform(black as f32),
        white: vec![white as f32],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::Normal,
        color,
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
    };
    raw.metadata.make = Some(MAKE.into());
    raw.metadata.model = Some(MODEL.into());
    raw
}

fn run(cmd: &mut Command) -> String {
    let o = cmd.output().unwrap();
    let (out, err) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "{cmd:?} failed:\n{out}\n{err}");
    assert!(!err.contains("panicked"), "{err}");
    format!("{out}{err}")
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn fit_recovers_a_hidden_profile_and_add_files_it() {
    let root = scratch();
    let (raws, exports, hidden, out) = (root.join("raws"), root.join(LOOK), root.join("hidden"), root.join("out"));
    for d in [&raws, &exports, &hidden] {
        std::fs::create_dir_all(d).unwrap();
    }
    let cal = hidden_calibration();
    cal.validate().unwrap();
    std::fs::write(hidden.join(cal.file_name()), cal.to_json().unwrap()).unwrap();

    let env_var = format!("{}_CAMERA_CALIBRATIONS", lcc_brand::ENV_PREFIX);
    for k in 0..PHOTOS {
        let t = k as f64 / (PHOTOS - 1) as f64;
        let dng = raws.join(format!("SX1_{k:04}.dng"));
        std::fs::write(&dng, lcc_raw::write_dng(&photo(k, neutral(t)), &DngWriteOptions::default()).unwrap()).unwrap();
        let tif = exports.join(format!("SX1_{k:04}.tif"));
        run(Command::new(BIN).env(&env_var, &hidden).args([
            "render",
            s(&dng),
            "-o",
            s(&tif),
            "--opt",
            "bitDepth=16",
            "--opt",
            "colorSpace=proPhoto",
        ]));
    }

    let log = run(Command::new(BIN).env_remove(&env_var).args([
        "calibrate-lightroom",
        "fit",
        "--raws",
        s(&raws),
        "--exports",
        s(&exports),
        "-o",
        s(&out),
        "--contributor",
        "Test Person",
    ]));
    eprintln!("{log}");
    let file = out.join(cal.file_name());
    let got = Calibration::parse(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!((got.make.as_str(), got.model.as_str()), (MAKE, MODEL));
    assert_eq!(got.looks.first().map(|l| l.name.as_str()), Some(LOOK));
    assert!(got.evidence.photos as usize >= PHOTOS * 3 / 4, "{:?}", got.evidence);
    let ev = got.evidence.looks.iter().find(|l| l.name == LOOK).expect("held-out evidence for the look");
    eprintln!("held-out ΔE00 median {:.2} → {:.2}, p95 {:.2} → {:.2}", ev.before.median, ev.after.median, ev.before.p95, ev.after.p95);
    assert!(ev.held_out > 0, "{ev:?}");
    assert!(ev.after.median < 2.0, "{ev:?}");
    assert!(ev.after.median < ev.before.median, "{ev:?}");

    // add into a fake repository
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("assets")).unwrap();
    std::fs::write(repo.join("assets/ATTRIBUTION.md"), "| Path | Asset |\n|---|---|\n| `other` | x |\n").unwrap();
    run(Command::new(BIN).args(["calibrate-lightroom", "add", s(&file), "--repo", s(&repo)]));
    let rel = format!("crates/camdb/data/calibrations/{}", cal.file_name());
    let added = std::fs::read_to_string(repo.join(&rel)).unwrap();
    assert_eq!(Calibration::parse(&added).unwrap(), got, "the canonical file is the contributed calibration");
    assert_eq!(added, got.to_json().unwrap(), "written in canonical form");
    let table = std::fs::read_to_string(repo.join("assets/ATTRIBUTION.md")).unwrap();
    let rows: Vec<&str> = table.lines().filter(|l| l.starts_with(&format!("| `{rel}` |"))).collect();
    assert_eq!(rows.len(), 1, "{table}");
    assert!(rows[0].contains("Test Person") && rows[0].contains(LOOK) && rows[0].contains(calibration::LICENCE), "{}", rows[0]);

    // the same exports as another editor's: a calibration of its own, side by side with Lightroom's
    let out_c1 = root.join("out-c1");
    run(Command::new(BIN).env_remove(&env_var).args([
        "calibrate-lightroom",
        "fit",
        "--editor",
        "capture-one",
        "--raws",
        s(&raws),
        "--exports",
        s(&exports),
        "-o",
        s(&out_c1),
    ]));
    let c1_name = format!("{}_capture-one.json", cal.file_name().trim_end_matches(".json"));
    let c1 = Calibration::parse(&std::fs::read_to_string(out_c1.join(&c1_name)).unwrap()).unwrap();
    assert_eq!(c1.editor, calibration::Editor::CaptureOne);
    assert_eq!(c1.method, calibration::Editor::CaptureOne.method());
    assert_eq!(c1.file_name(), c1_name);
    assert_eq!(c1.looks.first().map(|l| l.name.as_str()), Some(format!("Capture One {LOOK}").as_str()));
    assert_eq!(c1.evidence.white_points, 0, "no Lightroom white balance is used");
    let ev = c1.evidence.looks.first().expect("held-out evidence");
    assert!(ev.after.median < 2.5 && ev.after.median < ev.before.median, "{ev:?}");
    run(Command::new(BIN).args(["calibrate-lightroom", "add", s(&out_c1.join(&c1_name)), "--repo", s(&repo)]));
    assert!(repo.join("crates/camdb/data/calibrations").join(&c1_name).is_file());
    assert!(repo.join(&rel).is_file(), "Lightroom's calibration stays");
    let table = std::fs::read_to_string(repo.join("assets/ATTRIBUTION.md")).unwrap();
    assert!(table.lines().any(|l| l.contains(&c1_name) && l.contains("Capture One")), "{table}");
    assert!(table.lines().any(|l| l.starts_with(&format!("| `{rel}` |"))), "{table}");
    let bad =
        Command::new(BIN).args(["calibrate-lightroom", "fit", "--editor", "paint", "--raws", s(&raws), "--exports", s(&exports)]).output().unwrap();
    assert!(!bad.status.success() && String::from_utf8_lossy(&bad.stderr).contains("capture-one"));
    let _ = std::fs::remove_dir_all(&root);
}
