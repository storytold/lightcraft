//! `camera.exportDcp` and `develop.exportLut` end to end through `<cli> run`: a synthetic raw imported, its camera
//! colour written as a `.dcp` that reads back, its look as a `.cube` that reads back, and errors (not panics) for a
//! missing path or no photo.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");

/// The test's folder in Temp, removed when the test ends.
fn scratch(name: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-cli-colour-export-{name}"))
}

fn raw_dng(path: &Path) {
    let (w, h) = (96usize, 64usize);
    let mut raw = lcc_raw::RawImage {
        format: lcc_raw::RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: lcc_raw::RawData::U16((0..w * h).map(|i| 1200 + (i % 89) as u16 * 50).collect()),
        cfa: lcc_raw::Cfa::bayer("RGGB"),
        bits: 14,
        black: lcc_raw::BlackLevel::uniform(512.0),
        white: vec![16000.0],
        active_area: lcc_raw::Rect::new(0, 0, w, h),
        crop: lcc_raw::Rect::new(0, 0, w, h),
        orientation: lcc_raw::Orientation::Normal,
        color: lcc_raw::ColorData {
            illuminant: [21, 0],
            color_matrix: [Some(lcc_raw::Mat3([[0.8, -0.2, -0.05], [-0.45, 1.3, 0.15], [-0.08, 0.22, 0.62]])), None],
            as_shot_neutral: Some([0.5, 1.0, 0.7]),
            ..Default::default()
        },
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: Default::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
    };
    raw.metadata.make = Some("NIKON CORPORATION".into());
    raw.metadata.model = Some("NIKON D3".into());
    std::fs::write(path, lcc_raw::write_dng(&raw, &Default::default()).unwrap()).unwrap();
}

fn run(args: &[&str]) -> (bool, String, String) {
    let o = Command::new(BIN).args(args).env(lcc_brand::env_var("NO_PREFS"), "1").env(lcc_brand::env_var("GPU"), "0").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn a_photo_exports_its_camera_profile_and_look() {
    let d = scratch("ok");
    let (dng, dcp, cube) = (d.join("D3.dng"), d.join("out.dcp"), d.join("look.cube"));
    raw_dng(&dng);
    let (ok, out, err) = run(&["run", "--import", dng.to_str().unwrap(), "camera.exportDcp", &format!("path={}", dcp.display())]);
    assert!(ok, "{out}\n{err}");
    let profile = lcc_raw::dcp::parse(&std::fs::read(&dcp).unwrap()).unwrap();
    assert_eq!(profile.model, "Nikon D3");
    assert!(lcc_raw::color::has_matrix(&profile.color));

    let (ok, out, err) = run(&[
        "run",
        "--import",
        dng.to_str().unwrap(),
        "develop.set",
        "values={\"light.exposure\": 1}",
        "develop.exportLut",
        &format!("path={}", cube.display()),
    ]);
    assert!(ok, "{out}\n{err}");
    let lut = lcc_pipeline_cube(&std::fs::read_to_string(&cube).unwrap());
    assert_eq!(lut, 33 * 33 * 33);
}

/// Data lines of a `.cube` file, after checking its header.
fn lcc_pipeline_cube(text: &str) -> usize {
    assert!(text.lines().any(|l| l.trim() == "LUT_3D_SIZE 33"), "{}", &text[..200.min(text.len())]);
    text.lines().filter(|l| l.split_whitespace().count() == 3 && l.split_whitespace().all(|v| v.parse::<f32>().is_ok())).count()
}

#[test]
fn exports_without_a_path_or_a_photo_fail_with_a_message() {
    let d = scratch("bad");
    let dng = d.join("D3.dng");
    raw_dng(&dng);
    let jpg = d.join("x.jpg");
    std::fs::write(&jpg, b"\xff\xd8\xff not a jpeg").unwrap();
    for args in [
        vec!["run", "camera.exportDcp", "path=/tmp/never.dcp"],
        vec!["run", "--import", dng.to_str().unwrap(), "camera.exportDcp"],
        vec!["run", "--import", dng.to_str().unwrap(), "develop.exportLut"],
        vec!["run", "--import", dng.to_str().unwrap(), "camera.exportDcp", &format!("path={}", dng.display())],
    ] {
        let (ok, out, err) = run(&args);
        assert!(!ok, "{args:?} should fail: {out}");
        assert!(!err.contains("panicked") && !err.trim().is_empty(), "{args:?}: {err}");
    }
    assert!(dng.is_file() && std::fs::read(&dng).unwrap().len() > 1000, "the original is never overwritten");
}
