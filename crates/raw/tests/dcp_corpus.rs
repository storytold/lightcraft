//! A real camera profile: the Nikon D750 ColorChecker DCP dieli released under CC0
//! (github.com/dieli/d750-lightcraft-colorchecker, pinned in `D750_COLORCHECKER`, xtask/src/main.rs;
//! `cargo xtask corpus --download` puts it in the gitignored `corpus/d750-colorchecker/`).
//!
//! Calibrite PROFILER made it from two chart shots: a dual-illuminant, matrix-only profile
//! (`ColorMatrix1/2` for Cool White Fluorescent and D55; no `ForwardMatrix`, hue/saturation map,
//! look table or tone curve). The tests check that it parses as such, that the matrices blend between
//! the two illuminants by the white's inverse temperature, that the matrix-only path (no forward
//! matrix) gives a usable camera transform, and that it develops the chart NEFs it was made from.
//! Missing files are reported as skipped.

use lightcraft_color::{Xy, cct};
use lightcraft_raw::color::{camera_to_xyz_d50, camera_transform_of, illuminant_weight, xyz_to_camera};
use lightcraft_raw::dcp::{DcpProfile, parse_dcp, write_dcp};
use lightcraft_raw::{Mat3, decode};
use std::path::{Path, PathBuf};

const PROFILE: &str = "profiles/20261016_NikonD750_DualIllumination_ColorCheckerPassport.dcp";
const CHARTS: [&str; 2] = ["raw/chart-daylight_flash.NEF", "raw/chart-indoor.NEF"];

fn set_dir() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
        .join("d750-colorchecker")
}

fn read(rel: &str) -> Option<Vec<u8>> {
    let path = set_dir().join(rel);
    let bytes = std::fs::read(&path).ok();
    if bytes.is_none() {
        lightcraft_testkit::skip!(Corpus, "{} absent (cargo xtask corpus --download)", path.display());
    }
    bytes
}

fn profile() -> Option<DcpProfile> {
    read(PROFILE).map(|b| parse_dcp(&b).expect("the D750 profile parses"))
}

fn max_diff(a: &Mat3, b: &Mat3) -> f64 {
    a.0.iter().flatten().zip(b.0.iter().flatten()).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max)
}

#[test]
fn the_d750_profile_parses_as_a_dual_illuminant_matrix_only_profile() {
    let Some(p) = profile() else { return };
    assert!(p.camera_model.contains("D750"), "camera {:?}", p.camera_model);
    // CalibrationIlluminant1/2: Cool White Fluorescent (14) and D55 (20)
    assert_eq!(p.color.illuminant, [14, 20]);
    let [Some(cm1), Some(cm2)] = p.color.color_matrix else { panic!("two colour matrices: {:?}", p.color.color_matrix) };
    assert!(max_diff(&cm1, &cm2) > 1e-3, "the two illuminants have their own matrices");
    for m in [cm1, cm2] {
        assert!(m.0.iter().flatten().all(|v| v.is_finite() && v.abs() < 10.0), "{m:?}");
        assert!(m.inverse().is_some(), "invertible: {m:?}");
    }
    assert_eq!(p.color.forward_matrix, [None, None], "matrix-only: no forward matrix");
    assert!(p.color.profile.hue_sat_map.iter().all(Option::is_none), "no hue/saturation map");
    assert!(p.color.profile.look_table.is_none(), "no look table");
    // nothing of a particular shot
    assert_eq!((p.color.as_shot_neutral, p.color.analog_balance), (None, None));
    // our writer keeps what matters of it
    let again = parse_dcp(&write_dcp(&p).unwrap()).unwrap();
    assert_eq!((again.name.as_str(), again.camera_model.as_str(), again.color.illuminant), (p.name.as_str(), p.camera_model.as_str(), [14, 20]));
    for i in 0..2 {
        assert!(max_diff(&again.color.color_matrix[i].unwrap(), &p.color.color_matrix[i].unwrap()) < 1e-6);
    }
}

#[test]
fn the_d750_matrices_blend_by_inverse_temperature_between_the_two_illuminants() {
    let Some(p) = profile() else { return };
    let c = &p.color;
    let [Some(cm1), Some(cm2)] = c.color_matrix else { panic!("two colour matrices") };
    let at = |k: f64| cct::temp_tint_to_xy(k, 0.0);
    // at and beyond each calibration illuminant (4150 K / 5503 K) only its own matrix
    for k in [2800.0, 4150.0] {
        assert!((illuminant_weight(c, at(k)) - 1.0).abs() < 0.02, "{k} K: {}", illuminant_weight(c, at(k)));
        assert!(max_diff(&xyz_to_camera(c, at(k)), &cm1) < 0.02 * 3.0, "{k} K takes ColorMatrix1");
    }
    for k in [5503.0, 7500.0] {
        assert!(illuminant_weight(c, at(k)).abs() < 0.02, "{k} K: {}", illuminant_weight(c, at(k)));
        assert!(max_diff(&xyz_to_camera(c, at(k)), &cm2) < 0.02 * 3.0, "{k} K takes ColorMatrix2");
    }
    // in between: the DNG weighting (1/T − 1/T2) / (1/T1 − 1/T2), and the matrix is that blend
    let k = 4800.0;
    let w = illuminant_weight(c, at(k));
    let t = cct::xy_to_temp_tint(at(k)).0;
    let expected = (1.0 / t - 1.0 / 5503.0) / (1.0 / 4150.0 - 1.0 / 5503.0);
    assert!((w - expected).abs() < 1e-6 && w > 0.2 && w < 0.8, "weight {w}, expected {expected}");
    let mut blend = [[0.0; 3]; 3];
    for (i, row) in blend.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = w * cm1.0[i][j] + (1.0 - w) * cm2.0[i][j];
        }
    }
    assert!(max_diff(&xyz_to_camera(c, at(k)), &Mat3(blend)) < 1e-9);
    // weights fall monotonically with temperature
    let ws: Vec<f64> = [3000.0, 4300.0, 4600.0, 5000.0, 5400.0, 6500.0].iter().map(|k| illuminant_weight(c, at(*k))).collect();
    assert!(ws.windows(2).all(|p| p[0] >= p[1]), "{ws:?}");
}

#[test]
fn the_matrix_only_path_inverts_the_colour_matrix_and_adapts_to_d50() {
    let Some(p) = profile() else { return };
    let c = &p.color;
    for k in [3200.0, 4500.0, 5500.0, 6500.0] {
        let white: Xy = cct::temp_tint_to_xy(k, 0.0);
        let to_d50 = camera_to_xyz_d50(c, white);
        let expected = lightcraft_color::bradford(white, lightcraft_color::D50).mul(&xyz_to_camera(c, white).inverse().unwrap());
        assert!(max_diff(&to_d50, &expected) < 1e-9, "{k} K: no forward matrix, so XYZtoCamera⁻¹ then Bradford");
        // the camera's neutral under that white lands on D50
        let n = xyz_to_camera(c, white).apply(white.to_xyz());
        let xyz = to_d50.apply(n);
        let xy = Xy::from_xyz(xyz);
        assert!((xy.x - lightcraft_color::D50.x).abs() < 1e-6 && (xy.y - lightcraft_color::D50.y).abs() < 1e-6, "{k} K → {xy:?}");
        // the transform the pipeline uses maps white-balanced (1, 1, 1) to white
        let t = camera_transform_of(c, white);
        assert!(!t.matrix_is_fallback);
        let w = t.matrix.apply([1.0; 3]);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-6), "{k} K: {w:?}");
        assert!(t.wb.iter().all(|v| v.is_finite() && *v >= 1.0 && *v < 8.0), "{k} K: {:?}", t.wb);
    }
}

#[test]
fn the_d750_profile_takes_over_the_chart_nefs_colour() {
    let Some(p) = profile() else { return };
    let mut whites = Vec::new();
    for chart in CHARTS {
        let Some(bytes) = read(chart) else { return };
        let raw = decode(&bytes).expect("the D750 NEF decodes");
        let color = p.applied_to(&raw.color);
        assert_eq!(color.color_matrix, p.color.color_matrix, "{chart}: the profile's matrices");
        assert_eq!(color.forward_matrix, [None, None]);
        // the camera's own white balance stays and reads as a white under the profile's matrices
        let mut with = raw.clone();
        with.color = color;
        let white = lightcraft_raw::color::as_shot_white_xy(&with);
        let (k, tint) = cct::xy_to_temp_tint(white);
        assert!((2000.0..9000.0).contains(&k) && tint.abs() < 60.0, "{chart}: as-shot {k:.0} K, tint {tint:.1}");
        let t = lightcraft_raw::color::camera_transform(&with, white);
        assert!(t.matrix.0.iter().flatten().all(|v| v.is_finite()), "{chart}: {t:?}");
        whites.push(k);
    }
    // the flash shot is the cooler light's opposite: warmer light indoors reads as a lower temperature
    assert!(whites[0] > whites[1] + 300.0, "flash {:.0} K vs indoor {:.0} K", whites[0], whites[1]);
}
