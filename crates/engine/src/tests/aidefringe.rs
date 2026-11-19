//! AI Defringe through the session: installing a model, the switch, the background correction and exports
//! (docs/ai-defringe.md). The model is a synthetic network that asks for the same change everywhere.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Session;
use crate::aidefringe::{CONTRACT, MANIFEST, PhotoState};
use crate::export::{ExportOptions, export_photo};
use crate::tests_xmp::{synthetic_dng_of, temp_dir};

/// A raw whose left part is bright, with a 4 px magenta stripe on its edge, then mid grey.
fn stripe_dng(w: usize, h: usize) -> Vec<u8> {
    let data: Vec<u16> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let green = (x + y) % 2 == 1;
            match x {
                0..96 => 3600,
                96..100 if green => 500,
                96..100 => 3400,
                _ => 1100,
            }
        })
        .collect();
    synthetic_dng_of(w, h, "RGGB", data, None, Default::default())
}

/// A synthetic defringe model and its manifest in a folder of their own → the model file.
fn model_files(dir: &Path, out: [f32; 3]) -> PathBuf {
    let src = dir.join("model-src");
    std::fs::create_dir_all(&src).unwrap();
    let bytes = lightcraft_denoise::synthetic::defringe_onnx(out);
    let sha = lightcraft_denoise_core::hash::sha256_hex(&bytes);
    std::fs::write(src.join("defringe.onnx"), &bytes).unwrap();
    let m = json!({"id": "push", "name": "Push toward green-yellow", "version": "1", "licence": "MIT", "file": "defringe.onnx", "sha256": sha, "contract": CONTRACT, "notice": "a test network"});
    std::fs::write(src.join(MANIFEST), serde_json::to_vec(&m).unwrap()).unwrap();
    src.join("defringe.onnx")
}

struct Setup {
    dir: PathBuf,
    s: Session,
}

fn setup(tag: &str) -> Setup {
    let dir = temp_dir(tag);
    let originals = dir.join("originals");
    std::fs::create_dir_all(&originals).unwrap();
    std::fs::write(originals.join("stripe.dng"), stripe_dng(320, 288)).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("library"), false).unwrap();
    s.execute("library.import", &json!({"paths": [originals.join("stripe.dng").to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.set_aidefringe_models_dir(Some(dir.join("defringe-models")));
    Setup { dir, s }
}

fn photo(s: &Session) -> lightcraft_catalog::PhotoId {
    s.catalog.photos().next().unwrap().id
}

fn install(x: &mut Setup) -> Value {
    let path = model_files(&x.dir, [-3.0, 3.0, 10.0]);
    x.s.execute("aiDefringe.models.install", &json!({"path": path.to_string_lossy(), "acknowledged": true})).unwrap()
}

#[test]
fn a_model_is_installed_only_when_acknowledged_and_intact() {
    let mut x = setup("adf-install");
    let path = model_files(&x.dir, [-3.0, 3.0, 10.0]);
    let inspected = x.s.execute("aiDefringe.models.inspect", &json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!(inspected["notice"], "a test network");
    assert!(x.s.execute("aiDefringe.models.install", &json!({"path": path.to_string_lossy()})).is_err(), "not acknowledged");
    // a file that isn't the one its manifest names
    std::fs::write(&path, b"something else").unwrap();
    assert!(x.s.execute("aiDefringe.models.install", &json!({"path": path.to_string_lossy(), "acknowledged": true})).is_err());
    assert_eq!(x.s.execute("aiDefringe.status", &json!({})).unwrap()["model"], Value::Null);
    let r = install(&mut x);
    assert_eq!(r["model"]["id"], "push");
    assert_eq!(x.s.execute("aiDefringe.status", &json!({})).unwrap()["model"]["name"], "Push toward green-yellow");
    assert_eq!(x.s.execute("aiDefringe.models.remove", &json!({})).unwrap()["removed"], true);
    assert_eq!(x.s.execute("aiDefringe.status", &json!({})).unwrap()["model"], Value::Null);
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn the_switch_needs_a_model_and_the_correction_is_made_in_the_background() {
    let mut x = setup("adf-switch");
    let id = photo(&x.s);
    assert!(x.s.execute("aiDefringe.toggle", &json!({"enabled": true})).is_err(), "no model yet");
    install(&mut x);
    let r = x.s.execute("aiDefringe.toggle", &json!({"enabled": true})).unwrap();
    assert_eq!((r["enabled"].as_bool(), r["amount"].as_f64()), (Some(true), Some(100.0)));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        x.s.execute("aiDefringe.pump", &json!({})).unwrap();
        match x.s.aidefringe_state(id) {
            PhotoState::Ready { changed } => {
                assert!(changed > 0);
                break;
            }
            PhotoState::Failed { error } => panic!("{error}"),
            _ => {}
        }
        assert!(std::time::Instant::now() < deadline, "the correction never came");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(std::fs::read_dir(x.dir.join("library").join("ai-defringe")).unwrap().flatten().count(), 1);
    // switched off and on again: the Amount is kept and the correction is still there
    x.s.execute("aiDefringe.toggle", &json!({"enabled": false})).unwrap();
    assert_eq!(x.s.aidefringe_state(id), PhotoState::Off);
    x.s.execute("aiDefringe.toggle", &json!({"enabled": true})).unwrap();
    assert!(matches!(x.s.aidefringe_state(id), PhotoState::Ready { .. }));
    // a new session finds it on disk
    let Setup { dir, s } = x;
    drop(s);
    let mut s2 = Session::new().with_fs();
    s2.open_library(dir.join("library"), false).unwrap();
    s2.set_aidefringe_models_dir(Some(dir.join("defringe-models")));
    assert!(matches!(s2.aidefringe_state(id), PhotoState::Ready { .. }));
    assert_eq!(s2.execute("aiDefringe.clear", &json!({})).unwrap()["deleted"], 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_export_makes_the_correction_when_it_is_missing_and_never_comes_out_without_it() {
    let mut x = setup("adf-export");
    let id = photo(&x.s);
    let o = ExportOptions::from_json(&json!({"format": "png", "width": 320, "height": 320}));
    let decode = |bytes: &[u8]| lightcraft_codecs::decode(bytes, lightcraft_codecs::DecodeOptions::fit(320, 320)).unwrap().to_working();
    let plain = decode(&export_photo(&mut x.s, id, &o, 1).unwrap().bytes);
    install(&mut x);
    x.s.execute("aiDefringe.toggle", &json!({"enabled": true})).unwrap();
    // no pump: the export makes the correction itself
    let fixed = decode(&export_photo(&mut x.s, id, &o, 1).unwrap().bytes);
    assert_eq!((plain.width, plain.height), (fixed.width, fixed.height));
    let differs: Vec<usize> = (0..plain.data.len())
        .filter(|&i| plain.data[i].iter().zip(&fixed.data[i]).any(|(a, b)| (a - b).abs() > 1e-3))
        .map(|i| i % plain.width)
        .collect();
    assert!(!differs.is_empty(), "the export has no correction");
    let edge = plain.width * 96 / 320;
    assert!(differs.iter().all(|&x| x.abs_diff(edge) < 12), "pixels away from the stripe changed: {:?}", differs.iter().max());
    assert_eq!(std::fs::read_dir(x.dir.join("library").join("ai-defringe")).unwrap().flatten().count(), 1);
    // without a model it fails rather than export without the correction
    x.s.execute("aiDefringe.models.remove", &json!({})).unwrap();
    assert!(export_photo(&mut x.s, id, &o, 1).is_err());
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn any_defringe_shaped_onnx_installs_without_a_manifest_and_anything_else_is_refused() {
    let mut x = setup("adf-bare");
    let bare = x.dir.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    // a denoiser (the wrong shape) is refused when it is run, and nothing is installed
    let wrong = bare.join("smoothing.onnx");
    std::fs::write(&wrong, lightcraft_denoise::synthetic::smoothing_onnx()).unwrap();
    assert!(x.s.execute("aiDefringe.models.install", &json!({"path": wrong.to_string_lossy(), "acknowledged": true})).is_err());
    assert_eq!(x.s.execute("aiDefringe.status", &json!({})).unwrap()["model"], Value::Null);
    // a defringe network on its own: named after its file, its licence unknown
    let path = bare.join("My Defringe v2.onnx");
    std::fs::write(&path, lightcraft_denoise::synthetic::defringe_onnx([-3.0, 3.0, 10.0])).unwrap();
    let m = x.s.execute("aiDefringe.models.inspect", &json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!((m["name"].as_str(), m["licence"].as_str()), (Some("My Defringe v2"), Some("Not stated")));
    assert!(m["notice"].as_str().unwrap().contains("doesn't know who made it"));
    x.s.execute("aiDefringe.models.install", &json!({"path": path.to_string_lossy(), "acknowledged": true})).unwrap();
    assert_eq!(x.s.execute("aiDefringe.status", &json!({})).unwrap()["model"]["id"], "My-Defringe-v2");
    x.s.execute("aiDefringe.toggle", &json!({"enabled": true})).unwrap();
    assert!(x.s.execute("aiDefringe.make", &json!({})).unwrap()["changed"].as_u64().unwrap() > 0);
    let _ = std::fs::remove_dir_all(&x.dir);
}
