//! Camera profiling end to end (ported from OneSlips, LR-PROF-CREATE): a made-up camera photographs a
//! ColorChecker (painted procedurally into a DNG mosaic: no photograph is involved), the chart is
//! found, a profile is built and filed among the imported camera profiles, and the chart photo
//! develops with it. Profiles are filed into a temporary folder (never the user's settings).

use std::path::PathBuf;

use lcc_camprof::ChartCorners;
use serde_json::json;

use crate::Session;
use crate::chart_profiling::{self as cp, OnExists};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-chartprof-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const W: usize = 360;
const H: usize = 240;
fn truth() -> ChartCorners {
    ChartCorners::from_rect(90.0, 60.0, 180.0, 120.0)
}

fn lit_dng(light: &str, true_matrix: bool) -> Vec<u8> {
    cp::demo_chart_dng(W, H, &truth(), light, true_matrix).unwrap()
}

#[test]
fn a_chart_photo_becomes_a_camera_profile_the_photo_develops_with() {
    let dir = temp_dir("e2e");
    let path = dir.join("CHART_0001.dng");
    std::fs::write(&path, lit_dng("D55", false)).unwrap();
    let mut s = Session::new();
    let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = lcc_catalog::PhotoId(r["imported"][0].as_u64().unwrap());
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();

    let f = s.execute("profile.findChart", &json!({})).unwrap();
    assert_eq!(f["found"], true, "{f}");
    assert_eq!(f["chart"]["turns"], 0);
    for (got, want) in f["chart"]["corners"].as_array().unwrap().iter().zip(truth().points()) {
        assert!((got[0].as_f64().unwrap() * W as f64 - want[0]).abs() < 6.0);
        assert!((got[1].as_f64().unwrap() * H as f64 - want[1]).abs() < 6.0);
    }
    assert!(s.activity.list().is_empty(), "the task row is gone");

    let prep = cp::prepare_path(&path.to_string_lossy()).unwrap();
    let opts = lcc_camprof::BuildOptions { name: "Studio D55".into(), ..Default::default() };
    let out = cp::build((&prep, &cp::Request { illuminant: "D55".into(), ..Default::default() }), None, &opts).unwrap();
    let report = cp::report_json(&[&prep], &out);
    assert!(report["meanDeltaE"].as_f64().unwrap() < 1.0, "{report}");
    assert!(out.dcp.color.profile.hue_sat_map[0].is_some());

    let lib = dir.join("dcp");
    let first = cp::install(&mut s, &lib, &out.dcp, &[], OnExists::Fail, &[id]).unwrap();
    let pid = first["id"].as_str().unwrap().to_string();
    assert!(pid.starts_with("dcp:"), "{first}");
    assert_eq!(first["applied"], json!([id.0]));
    assert_eq!(s.catalog.photo(id).unwrap().develop.profile.camera, pid, "the chart photo develops with it");
    assert!(lcc_raw::dcp::library_get(&pid).is_some(), "chosen like an imported profile right away");
    let back = lcc_raw::dcp::parse(&std::fs::read(first["file"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(back.name, "Studio D55");

    // the same name again: refused, asked about, kept beside it, or replaced in place
    assert!(cp::install(&mut s, &lib, &out.dcp, &[], OnExists::Fail, &[]).unwrap_err().to_string().contains("keepBoth"));
    let ask = cp::install(&mut s, &lib, &out.dcp, &[], OnExists::Ask, &[]).unwrap();
    assert_eq!(ask["exists"]["usedBy"], 1, "{ask}");
    assert_eq!(ask["keepBothName"], "Studio D55 2");
    let both = cp::install(&mut s, &lib, &out.dcp, &[], OnExists::KeepBoth, &[]).unwrap();
    assert_eq!(both["name"], "Studio D55 2");
    assert_ne!(both["id"], first["id"]);
    let replaced = cp::install(&mut s, &lib, &out.dcp, &[], OnExists::Replace, &[]).unwrap();
    assert_eq!((replaced["id"].as_str(), replaced["replaced"].as_bool()), (Some(pid.as_str()), Some(true)));

    // Replace keeps the old profile beside the new one, and undo puts it back
    let req = cp::Request { illuminant: "D55".into(), ..Default::default() };
    let photo = |natural| lcc_camprof::BuildOptions { name: "Swap Test".into(), natural, ..Default::default() };
    let swap_a = cp::build((&prep, &req), None, &photo(true)).unwrap().dcp;
    let swap_b = cp::build((&prep, &req), None, &photo(false)).unwrap().dcp;
    let lib = dir.join("dcp-swap");
    let r = cp::install(&mut s, &lib, &swap_a, &[], OnExists::Fail, &[]).unwrap();
    let file = PathBuf::from(r["file"].as_str().unwrap());
    let old = std::fs::read(&file).unwrap();
    let r2 = cp::install(&mut s, &lib, &swap_b, &[], OnExists::Replace, &[]).unwrap();
    assert_eq!(r2["replaced"], true);
    let new = std::fs::read(&file).unwrap();
    assert_ne!(old, new, "written over");
    // the old one is kept beside it
    let kept: Vec<_> = std::fs::read_dir(lib.join(".replaced")).unwrap().flatten().map(|e| std::fs::read(e.path()).unwrap()).collect();
    assert_eq!(kept, std::slice::from_ref(&old));
    // undo puts it back, redo the new one again
    assert_eq!(s.undo_step().unwrap(), "Replace Camera Profile");
    assert_eq!(std::fs::read(&file).unwrap(), old);
    s.redo_step().unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), new);
    assert!(!lib.join(format!("{}.tmp", file.file_name().unwrap().to_string_lossy())).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_lights_make_a_dual_illuminant_profile() {
    let dir = temp_dir("dual");
    let (a, d) = (dir.join("CHART_A.dng"), dir.join("CHART_D65.dng"));
    std::fs::write(&a, lit_dng("A", true)).unwrap();
    std::fs::write(&d, lit_dng("D65", true)).unwrap();
    let (pa, pd) = (cp::prepare_path(&a.to_string_lossy()).unwrap(), cp::prepare_path(&d.to_string_lossy()).unwrap());
    let (ra, rd) =
        (cp::Request { illuminant: "tungsten".into(), ..Default::default() }, cp::Request { illuminant: "D65".into(), ..Default::default() });
    let out = cp::build((&pd, &rd), Some((&pa, &ra)), &Default::default()).unwrap();
    assert_eq!(out.dcp.color.illuminant, [17, 21], "calibration 1 is the lower colour temperature");
    assert_eq!(out.calibrations.len(), 2);
    assert!(out.calibrations.iter().all(|c| c.built.mean_delta_e < 1.0));
    // the same light twice is refused
    assert!(cp::build((&pd, &rd), Some((&pd, &rd)), &Default::default()).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_chart_requests_are_errors() {
    let dir = temp_dir("hostile");
    let path = dir.join("CHART.dng");
    std::fs::write(&path, lit_dng("D55", false)).unwrap();
    let junk = dir.join("junk.dng");
    std::fs::write(&junk, b"II*\0\x08\0\0\0\xff\xff garbage").unwrap();
    let jpeg = dir.join("not-raw.jpg");
    std::fs::write(&jpeg, b"\xff\xd8\xff\xe0 not really").unwrap();
    let p = path.to_string_lossy().to_string();
    let mut s = Session::new();
    for (cmd, bad) in [
        ("profile.createFromChart", json!({"path": p, "illuminant": "Moon"})),
        ("profile.createFromChart", json!({"path": p, "rendering": "vivid"})),
        ("profile.createFromChart", json!({"path": p, "onExists": "maybe"})),
        ("profile.createFromChart", json!({"path": p, "corners": [[0, 0], [1, 0]]})),
        ("profile.createFromChart", json!({"path": p, "corners": "everywhere"})),
        ("profile.createFromChart", json!({"path": p, "corners": [[0.5, 0.5], [0.5, 0.5], [0.5, 0.5], [0.5, 0.5]]})),
        ("profile.createFromChart", json!({"path": p, "corners": [[1e308, -1e308], [0, 0], [0, 0], [0, 0]]})),
        ("profile.createFromChart", json!({"path": p, "second": 3})),
        ("profile.createFromChart", json!({"path": p, "second": {}})),
        ("profile.createFromChart", json!({"path": p, "second": {"path": p}})),
        ("profile.createFromChart", json!({"path": p, "name": "x".repeat(500)})),
        ("profile.createFromChart", json!({"path": p, "chart": "x".repeat(500)})),
        ("profile.createFromChart", json!({"path": dir.join("missing.dng").to_string_lossy()})),
        ("profile.createFromChart", json!({"path": junk.to_string_lossy()})),
        ("profile.createFromChart", json!({})),
        ("profile.createFromChart", json!({"ids": "seven"})),
        ("profile.findChart", json!({"path": jpeg.to_string_lossy()})),
        ("profile.findChart", json!({"path": junk.to_string_lossy()})),
        ("profile.findChart", json!({"path": ""})),
    ] {
        assert!(s.execute(cmd, &bad).is_err(), "{cmd} {bad}");
    }
    assert!(s.activity.list().is_empty(), "no task row is left behind");
    assert!(cp::prepare(b"").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_full_disk_mid_write_leaves_the_old_profile_intact() {
    let dir = temp_dir("fulldisk");
    let file = dir.join("p.dcp");
    std::fs::write(&file, b"the old profile").unwrap();
    let full = |p: &std::path::Path, b: &[u8]| -> std::io::Result<()> {
        std::fs::write(p, &b[..b.len() / 2])?;
        Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "no space left on device"))
    };
    assert!(cp::write_atomic(&file, b"a new profile, longer", &full).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"the old profile");
    assert!(!file.with_extension("dcp.tmp").exists(), "no half-written file is left behind");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A real SpyderCHECKR 24 shot on a Panasonic DMC-G7, read straight from its RW2: the chart is found as a
/// SpyderCHECKR and a profile is fitted. The photo is the user's, so it is not in the repository: set
/// `<PREFIX>_TEST_SPYDER_RW2` to the file (any RW2 of a SpyderCHECKR 24 lit by daylight).
#[test]
fn a_spydercheckr_rw2_is_found_and_profiled() {
    let Some(path) = lcc_brand::env_os("TEST_SPYDER_RW2").filter(|p| !p.is_empty()).map(PathBuf::from) else {
        lcc_testkit::skip!(Corpus, "set <PREFIX>_TEST_SPYDER_RW2 to an RW2 of a SpyderCHECKR 24");
        return;
    };
    let prep = cp::prepare_path(&path.to_string_lossy()).unwrap();
    let located = prep.located.expect("the chart is found by its colours");
    assert_eq!(located.chart.key, "spyder24");
    let req = cp::Request { illuminant: "D55".into(), ..Default::default() };
    let out = cp::build((&prep, &req), None, &Default::default()).unwrap();
    let built = &out.calibrations[0].built;
    // Measured on the user's DMC-G7 frame: the light on that card is uneven. Evened out, the matrix
    // fits to 3.7 and the profile to 1.4 (5.2 and 2.7 without).
    assert!(built.notes.iter().any(|n| n.contains("correction kept")), "{:?}", built.notes);
    assert!(built.matrix_mean_delta_e < 4.2, "{}", built.matrix_mean_delta_e);
    assert!(built.mean_delta_e < 2.0, "{}", built.mean_delta_e);
    let uneven = lcc_camprof::BuildOptions { even_light: false, ..Default::default() };
    let raw = cp::build((&prep, &req), None, &uneven).unwrap();
    assert!(raw.calibrations[0].built.matrix_mean_delta_e > built.matrix_mean_delta_e + 0.5);
}
