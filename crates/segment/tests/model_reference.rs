//! Model references: our own inference with the real, openly licensed checkpoints on procedural
//! `lcc-scenes` photos, against small numeric summaries stored in `tests/data/model-references.json`
//! (per output: element count, mean, standard deviation, min, max and the share above the mask
//! threshold), within the tolerances stored there. A regression net for the ports (a kernel,
//! weight-loading or preprocessing change that moves the outputs shows up here), not a comparison
//! with the upstream implementations (the `*_reference.rs` tests do that, by hand).
//!
//! Models (each in `<dir>/<model folder>`, as `examples/fetch_reference_models.rs` downloads them,
//! pinned by size and SHA-256): SAM 2.1 Hiera-Tiny, BiRefNet_lite, EoMT-S COCO, MediaPipe Selfie
//! Multiclass and the MediaPipe Face Landmarker (BlazeFace + FaceMesh). Everything runs on the CPU.
//!
//! `<PREFIX>_MODEL_REF_DIR=<dir> cargo test -p lcc-segment --release --test model_reference -- --nocapture`;
//! each model is skipped (`lcc_testkit::skip!`) when its folder is missing. With
//! `<PREFIX>_MODEL_REF_WRITE=1` the summaries of the models present are written back to the JSON
//! file instead (after a deliberate change to a model's outputs; review the diff).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use candle_core::Device;
use lcc_scenes::Kind;
use lcc_segment::birefnet::BiRefNet;
use lcc_segment::scene::{self, SceneModel};
use lcc_segment::{Click, Sam2, mediapipe, models, people};
use serde_json::{Value, json};

const REF_FILE: &str = "tests/data/model-references.json";
/// The photos: procedural scenes (no faces or people in them; the outputs still exercise every op).
const SCENES: &[(Kind, u32)] = &[(Kind::AlpineLake, 1), (Kind::Beach, 2), (Kind::MistyForest, 3)];
const W: usize = 640;
const H: usize = 427;

static WRITE: Mutex<()> = Mutex::new(());

fn photo(kind: Kind, seed: u32) -> Vec<u8> {
    let img = lcc_scenes::render_srgb_linear(kind, seed, W, H).to_srgb8();
    img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect()
}

/// Summary of one output: `[n, mean, std, min, max, share above threshold]`.
fn summary(v: &[f32], threshold: f32) -> Value {
    assert!(v.iter().all(|x| x.is_finite()), "non-finite output");
    let n = v.len().max(1) as f64;
    let mean = v.iter().map(|x| f64::from(*x)).sum::<f64>() / n;
    let var = v.iter().map(|x| (f64::from(*x) - mean).powi(2)).sum::<f64>() / n;
    let min = v.iter().fold(f32::INFINITY, |m, x| m.min(*x));
    let max = v.iter().fold(f32::NEG_INFINITY, |m, x| m.max(*x));
    let above = v.iter().filter(|x| **x > threshold).count() as f64 / n;
    json!([v.len(), mean, var.sqrt(), min, max, above])
}

type Outputs = BTreeMap<String, Value>;

fn run_sam2(dir: &Path) -> Outputs {
    let model = Sam2::load_on(dir, Device::Cpu).unwrap();
    let mut out = Outputs::new();
    for &(kind, seed) in SCENES {
        let enc = model.encode(&photo(kind, seed), W, H).unwrap();
        for (name, clicks) in [
            ("centre", vec![Click { x: 0.5, y: 0.6, positive: true }]),
            ("two", vec![Click { x: 0.3, y: 0.8, positive: true }, Click { x: 0.5, y: 0.2, positive: false }]),
        ] {
            let p = model.segment_clicks(&enc, &clicks).unwrap();
            out.insert(format!("{kind:?}.{name}.logits"), summary(&p.logits, 0.0));
            out.insert(format!("{kind:?}.{name}.score"), summary(&[p.score], 0.5));
        }
    }
    out
}

fn run_birefnet(dir: &Path) -> Outputs {
    let model = BiRefNet::load_on(dir, Device::Cpu).unwrap();
    let mut out = Outputs::new();
    for &(kind, seed) in SCENES {
        let m = model.predict(&photo(kind, seed), W, H).unwrap();
        out.insert(format!("{kind:?}.matte"), summary(&m.data, 0.5));
    }
    out
}

fn run_scene(dir: &Path) -> Outputs {
    let model = SceneModel::load_on(dir, Device::Cpu).unwrap();
    let sky = scene::class_ids(|l| l.contains("sky"));
    let mut out = Outputs::new();
    for &(kind, seed) in SCENES {
        let s = model.segment(&photo(kind, seed), W, H).unwrap();
        out.insert(format!("{kind:?}.probs"), summary(&s.probs, 0.5));
        out.insert(format!("{kind:?}.sky"), summary(&s.class_probability(&sky), 0.5));
    }
    out
}

fn tflite(bytes: &[u8]) -> lcc_tflite::Model {
    lcc_tflite::Model::from_bytes(bytes).unwrap()
}

fn run_people(dir: &Path) -> Outputs {
    let net = tflite(&std::fs::read(dir.join(people::PEOPLE_FILE)).unwrap());
    let side = mediapipe::SELFIE_INPUT;
    let (sx, sy) = (side as f32 / W as f32, side as f32 / H as f32);
    let mut out = Outputs::new();
    for &(kind, seed) in SCENES {
        let input = mediapipe::warp(&photo(kind, seed), W, H, &[sx, 0.0, 0.0, 0.0, sy, 0.0], side, -1.0, 1.0);
        let logits = net.run(&[&input]).unwrap().remove(0);
        out.insert(format!("{kind:?}.logits"), summary(&logits, 0.0));
    }
    out
}

fn run_face(dir: &Path) -> Outputs {
    let bundle = std::fs::read(dir.join(people::FACE_FILE)).unwrap();
    let detector = tflite(lcc_tflite::zip::entry(&bundle, "face_detector.tflite").unwrap());
    let mesh = tflite(lcc_tflite::zip::entry(&bundle, "face_landmarks_detector.tflite").unwrap());
    let crop = mediapipe::mesh_crop_with([W as f32 * 0.5, H as f32 * 0.5], 0.2, H as f32 * 0.6);
    let mut out = Outputs::new();
    for &(kind, seed) in SCENES {
        let rgb = photo(kind, seed);
        let x = mediapipe::window(&rgb, W, H, (100, 0, H), mediapipe::BLAZEFACE_INPUT);
        for (i, o) in detector.run(&[&x]).unwrap().iter().enumerate() {
            out.insert(format!("{kind:?}.detector{i}"), summary(o, 0.0));
        }
        let x = mediapipe::warp(&rgb, W, H, &crop, mediapipe::MESH_INPUT, 0.0, 1.0);
        for (i, o) in mesh.run(&[&x]).unwrap().iter().enumerate() {
            out.insert(format!("{kind:?}.mesh{i}"), summary(o, 0.0));
        }
    }
    out
}

fn numbers(v: &Value) -> Vec<f64> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()
}

fn check(key: &str, folder: &str, run: fn(&Path) -> Outputs) {
    let Some(dir) = lcc_brand::env_os("MODEL_REF_DIR").map(|r| PathBuf::from(r).join(folder)).filter(|d| d.is_dir()) else {
        lcc_testkit::skip!(Model, "set {} to a folder holding {folder}/", lcc_brand::env_var("MODEL_REF_DIR"));
        return;
    };
    let t = std::time::Instant::now();
    let got = run(&dir);
    eprintln!("{key}: ran in {:?}", t.elapsed());
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(REF_FILE);
    let _guard = WRITE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut refs: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    if lcc_brand::env_os("MODEL_REF_WRITE").is_some() {
        refs["models"][key]["outputs"] = serde_json::to_value(&got).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&refs).unwrap() + "\n").unwrap();
        eprintln!("{key}: wrote {} summaries to {REF_FILE}", got.len());
        return;
    }
    let model = &refs["models"][key];
    let rtol = model["rtol"].as_f64().unwrap();
    let share_tol = model["share_tol"].as_f64().unwrap();
    let want = model["outputs"].as_object().unwrap_or_else(|| panic!("{key}: no stored outputs in {REF_FILE}"));
    assert_eq!(want.len(), got.len(), "{key}: number of outputs");
    let mut worst = 0f64;
    for (name, w) in want {
        let (w, g) = (numbers(w), numbers(got.get(name).unwrap_or_else(|| panic!("{key}: no output {name}"))));
        assert!(w.len() == 6 && g.len() == 6, "{key} {name}: malformed summary");
        assert_eq!(g[0], w[0], "{key} {name}: element count");
        // mean/std/min/max relative to the output's own range, the share in absolute terms
        let scale = w[2].abs().max(w[3].abs()).max(w[4].abs()).max(1e-3);
        for (i, label) in [(1, "mean"), (2, "std"), (3, "min"), (4, "max")] {
            let err = (g[i] - w[i]).abs() / scale;
            worst = worst.max(err);
            assert!(err <= rtol, "{key} {name}: {label} {} vs stored {} (relative error {err:.2e} > {rtol:.0e})", g[i], w[i]);
        }
        assert!((g[5] - w[5]).abs() <= share_tol, "{key} {name}: share above threshold {} vs stored {}", g[5], w[5]);
    }
    eprintln!("{key}: {} outputs within tolerance (worst relative error {worst:.2e})", want.len());
}

#[test]
fn sam2() {
    check("sam2.1-hiera-tiny", models::SAM2.dir_name, run_sam2);
}

#[test]
fn birefnet() {
    check("birefnet-lite", "birefnet-lite", run_birefnet);
}

#[test]
fn eomt() {
    check("eomt-s-coco", models::SCENE.dir_name, run_scene);
}

#[test]
fn mediapipe_people() {
    check("mediapipe-selfie-multiclass", models::PEOPLE.dir_name, run_people);
}

#[test]
fn mediapipe_face() {
    check("mediapipe-face-landmarker", models::FACE.dir_name, run_face);
}
