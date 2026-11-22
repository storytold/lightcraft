//! M11.4 end to end: `calibrate-lightroom add` refuses damaged or foreign files with an error exit (never a panic)
//! and leaves the repository untouched; bad arguments are errors too.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_app-cli");

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-cli-lrcal-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("assets")).unwrap();
    std::fs::write(d.join("assets/ATTRIBUTION.md"), "| Path | Asset |\n|---|---|\n").unwrap();
    d
}

#[test]
fn add_refuses_hostile_files_without_panicking() {
    let repo = scratch("add");
    let inputs: [&[u8]; 6] =
        [b"", b"{bad", b"[]", br#"{"version":1,"make":"X","model":"Y","extra":true}"#, br#"{"version":99}"#, &[0xff, 0xfe, 0x00, 0x7b]];
    for (k, bytes) in inputs.iter().enumerate() {
        let file = repo.join(format!("in{k}.json"));
        std::fs::write(&file, bytes).unwrap();
        let o = Command::new(BIN).args(["calibrate-lightroom", "add", file.to_str().unwrap(), "--repo", repo.to_str().unwrap()]).output().unwrap();
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success(), "input {k} accepted");
        assert!(!err.contains("panicked"), "input {k} panicked: {err}");
    }
    assert!(!repo.join("crates/camdb/data/calibrations").exists(), "nothing written for refused files");
    let o = Command::new(BIN).args(["calibrate-lightroom", "fit", "--raws"]).output().unwrap();
    assert!(!o.status.success());
    let _ = std::fs::remove_dir_all(&repo);
}

fn xiaomi() -> lcc_camdb::calibration::Calibration {
    let fm = lcc_raw::Mat3([[0.70, 0.18, 0.08], [0.28, 0.80, -0.08], [0.02, -0.15, 0.95]]);
    let cm = lcc_raw::Mat3([[0.9, -0.3, -0.05], [-0.5, 1.4, 0.1], [-0.1, 0.3, 0.6]]);
    lcc_camdb::calibration::Calibration {
        version: lcc_camdb::calibration::VERSION,
        make: "Xiaomi".into(),
        model: "Xiaomi 17 Ultra/2512BPNDAG".into(),
        aliases: vec![],
        lens: None,
        editor: Default::default(),
        own_matrix_only: false,
        method: lcc_camdb::calibration::METHOD.into(),
        tool_version: "26.43.7".into(),
        contributor: Some("A Contributor".into()),
        licence: lcc_camdb::calibration::LICENCE.into(),
        camera: lcc_camdb::calibration::CameraModel { illuminants: [17, 21], color_matrix: [cm, cm], forward_matrix: [fm, fm], hue_sat_map: None },
        looks: vec![lcc_camdb::calibration::Look {
            name: "Camera Standard".into(),
            baseline_exposure_offset: 0.0,
            look_table: None,
            tone_curve: vec![],
        }],
        evidence: lcc_camdb::calibration::Evidence {
            photos: 10,
            samples: 1000,
            cct: vec![],
            hue_coverage: 0.5,
            white_points: 10,
            looks: vec![lcc_camdb::calibration::LookEvidence {
                name: "Camera Standard".into(),
                photos: 8,
                held_out: 2,
                before: Default::default(),
                after: Default::default(),
            }],
        },
    }
}

#[test]
fn add_accepts_model_with_slash() {
    let repo = scratch("slash");
    let cal = xiaomi();
    let json = cal.to_json().unwrap();
    let file = repo.join("xiaomi.json");
    std::fs::write(&file, json).unwrap();
    let o = Command::new(BIN).args(["calibrate-lightroom", "add", file.to_str().unwrap(), "--repo", repo.to_str().unwrap()]).output().unwrap();
    assert!(o.status.success(), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    let dest = repo.join("crates/camdb/data/calibrations/XIAOMI_Xiaomi_17_Ultra_2512BPNDAG.json");
    assert!(dest.exists(), "calibration file was written");
    let attr = std::fs::read_to_string(repo.join("assets/ATTRIBUTION.md")).unwrap();
    assert!(attr.contains("XIAOMI_Xiaomi_17_Ultra_2512BPNDAG.json"));
    let _ = std::fs::remove_dir_all(&repo);
}

/// Issue #1015: a second fit for the same camera, lens and editor is refused without `--replace` (the first stays as
/// it was); a fit for another lens (`--lens`) gets its own file beside it.
#[test]
fn add_keeps_one_file_per_camera_lens_and_editor() {
    let repo = scratch("lens");
    let add = |file: &std::path::Path, extra: &[&str]| {
        let mut args = vec!["calibrate-lightroom", "add", file.to_str().unwrap(), "--repo", repo.to_str().unwrap()];
        args.extend_from_slice(extra);
        Command::new(BIN).args(args).output().unwrap()
    };
    let dir = repo.join("crates/camdb/data/calibrations");
    let first = xiaomi();
    let mut second = xiaomi();
    second.evidence.photos = 99;
    let (f1, f2) = (repo.join("first.json"), repo.join("second.json"));
    std::fs::write(&f1, first.to_json().unwrap()).unwrap();
    std::fs::write(&f2, second.to_json().unwrap()).unwrap();
    assert!(add(&f1, &[]).status.success());
    let plain = dir.join("XIAOMI_Xiaomi_17_Ultra_2512BPNDAG.json");
    let before = std::fs::read_to_string(&plain).unwrap();
    let o = add(&f2, &[]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("--replace") && !err.contains("panicked"), "{err}");
    assert_eq!(std::fs::read_to_string(&plain).unwrap(), before, "the first fit is not overwritten");
    // another lens: its own file, the lens-less one untouched
    let o = add(&f2, &["--lens", "Ultrawide 14mm"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let lensed = dir.join("XIAOMI_Xiaomi_17_Ultra_2512BPNDAG_Ultrawide_14mm.json");
    let back = lcc_camdb::calibration::Calibration::parse(&std::fs::read_to_string(&lensed).unwrap()).unwrap();
    assert_eq!(back.lens.as_deref(), Some("Ultrawide 14mm"));
    assert_eq!(std::fs::read_to_string(&plain).unwrap(), before);
    // the same lens again (case aside): refused; with --replace it replaces
    assert!(!add(&f1, &["--lens", "ULTRAWIDE 14mm"]).status.success());
    assert!(add(&f2, &["--replace"]).status.success());
    let replaced = lcc_camdb::calibration::Calibration::parse(&std::fs::read_to_string(&plain).unwrap()).unwrap();
    assert_eq!(replaced.evidence.photos, 99);
    // hostile lens names are errors, nothing written
    for lens in ["", "../x", "/abs", "a\\b", "a\u{7}b"] {
        let o = add(&f1, &["--lens", lens, "--replace"]);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && !err.contains("panicked"), "lens {lens:?}: {err}");
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    let _ = std::fs::remove_dir_all(&repo);
}
