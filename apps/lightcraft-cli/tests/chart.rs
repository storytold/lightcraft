//! Run the real chart CLI with original procedural colour measurements, never camera fixtures.
use lightcraft_color::{
    D65, Mat3, SRGB, bradford,
    camera::{CameraCalibration, CameraModel, Provenance},
    cct,
    chart::{Capture, Dataset, Patch, ValidationReport},
};
use std::process::Command;

fn data(id: u32, temp: f64) -> Dataset {
    let m = Mat3([[0.9, 0.1, 0.03], [-0.1, 1.1, 0.08], [0.02, -0.03, 0.8]]);
    let white = cct::temp_tint_to_xy(temp, 0.0);
    let patches = (0..24)
        .map(|i| {
            let p = if i == 0 {
                [0.25; 3]
            } else {
                [0.1 + ((i * 7) % 17) as f64 / 35.0, 0.1 + ((i * 11) % 17) as f64 / 35.0, 0.1 + ((i * 13) % 17) as f64 / 35.0]
            };
            let xyz = bradford(D65, white).mul(&SRGB.to_xyz()).apply(p);
            Patch { name: format!("patch{i}"), xyz, camera: m.apply(xyz).map(|v| v * 0.3) }
        })
        .collect();
    Dataset {
        version: 1,
        make: "SYNTHETIC".into(),
        model: "TEST ONLY".into(),
        provenance: Provenance {
            author: "LightCraft".into(),
            source: "original work".into(),
            license: "MIT OR Apache-2.0".into(),
            created: "2026-10-08".into(),
            chart: "procedural chart".into(),
            reference: "analytical XYZ".into(),
        },
        captures: vec![Capture { id: format!("{id:064x}"), white, neutral_patch: 0, patches }],
    }
}
#[test]
fn chart_cli_fits_validates_and_refuses_training_reuse() {
    let dir = std::env::temp_dir().join(format!("lightcraft-chart-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, id, t) in [("warm", 1, 2856.0), ("day", 2, 6504.0), ("holdout", 3, 4500.0)] {
        std::fs::write(dir.join(format!("{name}.json")), serde_json::to_vec(&data(id, t)).unwrap()).unwrap();
    }
    let run = |args: &[&str]| Command::new(env!("CARGO_BIN_EXE_lightcraft-cli")).current_dir(&dir).args(args).output().unwrap();
    let fit = run(&["chart", "fit", "profile.json", "warm.json", "day.json"]);
    assert!(fit.status.success(), "{}", String::from_utf8_lossy(&fit.stderr));
    let p: CameraCalibration = serde_json::from_slice(&std::fs::read(dir.join("profile.json")).unwrap()).unwrap();
    p.validate().unwrap();
    let _: CameraModel = p.calibration;
    let val = run(&["chart", "validate", "profile.json", "report.json", "holdout.json"]);
    assert!(val.status.success(), "{}", String::from_utf8_lossy(&val.stderr));
    let report: ValidationReport = serde_json::from_slice(&std::fs::read(dir.join("report.json")).unwrap()).unwrap();
    assert!(report.independent_captures && report.interpolation_covered);
    assert!(!report.warm_covered && !report.daylight_covered);
    assert!(report.captures[0].max_delta_e76 < 1.0);
    assert!(!run(&["chart", "validate", "profile.json", "invalid.json", "warm.json"]).status.success());
    assert!(!dir.join("invalid.json").exists());
    assert!(!run(&["chart", "fit", "profile.json", "warm.json", "day.json"]).status.success(), "cannot silently overwrite a profile");
    std::fs::File::create(dir.join("oversized.NEF")).unwrap().set_len(257 * 1024 * 1024).unwrap();
    assert!(!run(&["chart", "inspect", "oversized.NEF"]).status.success());
    std::fs::remove_dir_all(dir).unwrap();
}
