//! `<cli> calibrate-chart` end to end, synthetic: a fictitious camera with a hidden colour model (a forward matrix
//! and a camera neutral per light) shoots the ColorChecker Classic in daylight and under tungsten; the shots are
//! DNGs whose own (wrong) matrices stand for the colour the app had before. The corners are given in a shuffled
//! order. The entry printed must parse, carry both lights, and bring the patches far closer than before; and bad
//! input fails with a message, not a panic. No Adobe or X-Rite image data is involved: the patches are rendered
//! from the published reference values.

use std::path::Path;
use std::process::Command;

use lcc_raw::{BlackLevel, Cfa, ColorData, DngWriteOptions, Mat3, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect};

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");
const W: usize = 1200;
const H: usize = 800;
/// Chart placement: corner patch centres (dark skin, bluish green, black, white) as fractions.
const TL: [f64; 2] = [0.20, 0.25];
const BR: [f64; 2] = [0.80, 0.75];

/// The test's folder in Temp, removed when the test ends.
fn scratch(name: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-cli-chart-e2e-{name}"))
}

fn lab_to_xyz(l: [f64; 3]) -> [f64; 3] {
    let fy = (l[0] + 16.0) / 116.0;
    let (fx, fz) = (fy + l[1] / 500.0, fy - l[2] / 200.0);
    let inv = |t: f64| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f64 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    [0.9642 * inv(fx), inv(fy), 0.8249 * inv(fz)]
}

/// The chart's reference L*a*b* (X-Rite 2016, after November 2014).
const LAB: [[f64; 3]; 24] = [
    [37.54, 14.37, 14.92],
    [64.66, 19.27, 17.5],
    [49.32, -3.82, -22.54],
    [43.46, -12.74, 22.72],
    [54.94, 9.61, -24.79],
    [70.48, -32.26, -0.37],
    [62.73, 35.83, 56.5],
    [39.43, 10.75, -45.17],
    [50.57, 48.64, 16.67],
    [30.1, 22.54, -20.87],
    [71.77, -24.13, 58.19],
    [71.51, 18.24, 67.37],
    [28.37, 15.42, -49.8],
    [54.38, -39.72, 32.27],
    [42.43, 51.05, 28.62],
    [81.8, 2.67, 80.41],
    [50.63, 51.28, -14.12],
    [49.57, -29.71, -28.32],
    [95.19, -1.03, 2.93],
    [81.29, -0.57, 0.44],
    [66.89, -0.75, -0.06],
    [50.76, -0.13, 0.14],
    [35.63, -0.46, -0.48],
    [20.64, 0.07, -0.46],
];

/// A white-preserving forward matrix (rows sum to D50's white).
fn fm(a: [f64; 6]) -> Mat3 {
    let w = [0.9642, 1.0, 0.8249];
    Mat3(std::array::from_fn(|r| [a[r * 2], a[r * 2 + 1], w[r] - a[r * 2] - a[r * 2 + 1]]))
}

fn shot(path: &Path, forward: &Mat3, neutral: [f64; 3], exposure: f64) {
    let inv = forward.inverse().unwrap();
    let cam: Vec<[f64; 3]> = LAB
        .iter()
        .map(|l| {
            let wb = inv.apply(lab_to_xyz(*l));
            [wb[0] * neutral[0] * exposure, wb[1] * exposure, wb[2] * neutral[2] * exposure]
        })
        .collect();
    let ground = [0.1 * neutral[0] * exposure, 0.1 * exposure, 0.1 * neutral[2] * exposure];
    let (pw, ph) = ((BR[0] - TL[0]) / 5.0, (BR[1] - TL[1]) / 3.0);
    let cfa = Cfa::bayer("RGGB").unwrap();
    let (black, white) = (512.0f64, 16000.0f64);
    let mut data = Vec::with_capacity(W * H);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = ((x as f64 + 0.5) / W as f64, (y as f64 + 0.5) / H as f64);
            let (col, row) = (((fx - TL[0]) / pw).round(), ((fy - TL[1]) / ph).round());
            let inside = (0.0..=5.0).contains(&col)
                && (0.0..=3.0).contains(&row)
                && (fx - (TL[0] + col * pw)).abs() < pw * 0.38
                && (fy - (TL[1] + row * ph)).abs() < ph * 0.38;
            let v = if inside { cam[(row as usize) * 6 + col as usize] } else { ground };
            let c = v[cfa.color_at(x, y) as usize].clamp(0.0, 1.0);
            data.push((black + c * (white - black)).round() as u16);
        }
    }
    let mut raw = RawImage {
        format: RawFormat::Dng,
        width: W,
        height: H,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(cfa),
        bits: 14,
        black: BlackLevel::uniform(black as f32),
        white: vec![white as f32],
        active_area: Rect::new(0, 0, W, H),
        crop: Rect::new(0, 0, W, H),
        orientation: Orientation::Normal,
        // the file's own matrix: not the hidden one, the colour the app had before
        color: ColorData {
            illuminant: [21, 0],
            color_matrix: [Some(Mat3([[1.0, -0.1, -0.1], [-0.3, 1.2, 0.1], [0.0, 0.1, 0.9]])), None],
            as_shot_neutral: Some(neutral),
            ..Default::default()
        },
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
    };
    raw.metadata.make = Some("Synthica".into());
    raw.metadata.model = Some("SX-2".into());
    std::fs::write(path, lcc_raw::write_dng(&raw, &DngWriteOptions::default()).unwrap()).unwrap();
}

/// The corners in a shuffled order: white, bluish green, dark skin, black.
fn corners() -> String {
    let (tr, bl) = ([BR[0], TL[1]], [TL[0], BR[1]]);
    format!("{},{} {},{} {},{} {},{}", bl[0], bl[1], tr[0], tr[1], TL[0], TL[1], BR[0], BR[1])
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn chart_shots_give_a_measured_camera_entry() {
    let d = scratch("ok");
    let (day, tung, dcp, prev, rep) = (d.join("day.dng"), d.join("tungsten.dng"), d.join("sx2.dcp"), d.join("preview"), d.join("report.json"));
    let fm_day = fm([0.62, 0.25, 0.30, 0.78, 0.06, -0.12]);
    let fm_tung = fm([0.66, 0.20, 0.33, 0.74, 0.08, -0.16]);
    shot(&day, &fm_day, [0.48, 1.0, 0.66], 0.35);
    shot(&tung, &fm_tung, [0.82, 1.0, 0.31], 0.30);
    let c = corners();
    let out = Command::new(BIN)
        .args(["calibrate-chart", "--shot", s(&day), "--corners", &c, "--light", "daylight"])
        .args(["--shot", s(&tung), "--corners", &c, "--light", "tungsten"])
        .args(["--dcp", s(&dcp), "--preview", s(&prev), "--report", s(&rep), "--contributor", "Test Person"])
        .output()
        .unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{stdout}\n{stderr}");
    let entries = lcc_camdb::parse_toml(&stdout).unwrap();
    let e = entries.first().unwrap();
    assert_eq!((e.make.as_str(), e.model.as_str()), ("Synthica", "SX-2"));
    assert_eq!(e.provenance.source, lcc_camdb::Source::Chart);
    assert_eq!(e.provenance.contributor.as_deref(), Some("Test Person"));
    assert_eq!(e.illuminants, [17, 20], "tungsten first, then daylight");
    // the recovered forward matrices are the hidden ones
    for (got, want) in e.forward_matrix.iter().zip([fm_tung, fm_day]) {
        let got = got.unwrap();
        for (a, b) in got.0.iter().flatten().zip(want.0.iter().flatten()) {
            assert!((a - b).abs() < 0.03, "{got:?} vs {want:?}");
        }
    }
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&rep).unwrap()).unwrap();
    for sh in report["shots"].as_array().unwrap() {
        let mean = |k: &str| {
            let v: Vec<f64> = sh[k].as_array().unwrap().iter().filter_map(serde_json::Value::as_f64).collect();
            v.iter().sum::<f64>() / v.len() as f64
        };
        assert!(mean("after") < 1.0 && mean("before") > mean("after") + 2.0, "{sh}");
    }
    let profile = lcc_raw::dcp::parse(&std::fs::read(&dcp).unwrap()).unwrap();
    assert!(lcc_raw::color::has_matrix(&profile.color));
    assert!(prev.join("day.chart.png").is_file() && prev.join("tungsten.chart.png").is_file());
}

#[test]
fn bad_chart_input_fails_with_a_message() {
    let d = scratch("bad");
    let (day, bright, junk) = (d.join("day.dng"), d.join("bright.dng"), d.join("junk.dng"));
    shot(&day, &fm([0.62, 0.25, 0.30, 0.78, 0.06, -0.12]), [0.48, 1.0, 0.66], 0.35);
    // overexposed: the grey patches clip
    shot(&bright, &fm([0.62, 0.25, 0.30, 0.78, 0.06, -0.12]), [0.48, 1.0, 0.66], 3.0);
    std::fs::write(&junk, b"II*\0not a raw").unwrap();
    let c = corners();
    let other = scratch("bad-other");
    for args in [
        vec!["calibrate-chart"],
        vec!["calibrate-chart", "--shot", s(&day)],
        vec!["calibrate-chart", "--corners", &c],
        vec!["calibrate-chart", "--shot", s(&day), "--corners", "1,2,3"],
        vec!["calibrate-chart", "--shot", s(&day), "--corners", "0.1,0.1 2,0.1 2,2 0.1,2"],
        vec!["calibrate-chart", "--shot", s(&day), "--corners", "0.99,0.99 0.995,0.99 0.995,0.995 0.99,0.995"],
        vec!["calibrate-chart", "--shot", s(&day), "--corners", &c, "--light", "purple"],
        vec!["calibrate-chart", "--shot", s(&day), "--corners", &c, "--chart", "1999"],
        vec!["calibrate-chart", "--shot", s(&junk), "--corners", &c],
        vec!["calibrate-chart", "--shot", s(&bright), "--corners", &c],
        vec!["calibrate-chart", "--shot", s(&other), "--corners", &c],
        vec![
            "calibrate-chart",
            "--shot",
            s(&day),
            "--corners",
            &c,
            "--light",
            "tungsten",
            "--shot",
            s(&day),
            "--corners",
            &c,
            "--light",
            "d50",
            "--shot",
            s(&day),
            "--corners",
            &c,
            "--light",
            "shade",
        ],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} should fail");
        assert!(!err.contains("panicked") && !err.trim().is_empty(), "{args:?}: {err}");
    }
}
