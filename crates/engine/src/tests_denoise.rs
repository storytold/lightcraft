//! AI denoise through the engine: installing a model, making a photo's denoised picture in the background and for an
//! export, the Amount slider mixing it in, and the cache. A tiny stand-in model (it halves every sample) takes the
//! place of a real one, which is enough to see what the machinery does with its answer; how well real models denoise is
//! measured separately (`crates/denoise/tests/it/real_model.rs`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lcc_denoise::manifest::{DenoiserManifest, Domain, Gain};
use lcc_denoise::run::TileRunner;
use serde_json::{Value, json};

use crate::Session;
use crate::denoise::{Loader, Model, PhotoState, RunOn};
use crate::export::{ExportOptions, export_photo};
use crate::tests_xmp::{synthetic_dng_of, temp_dir};

/// A model that makes every output pixel half of its cell's colour: a picture that is plainly not the plain one.
struct Dim {
    runs: Arc<AtomicUsize>,
}

impl TileRunner for Dim {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, lcc_denoise::Error> {
        self.runs.fetch_add(1, Ordering::Relaxed);
        let t = ((input.len() / 4) as f64).sqrt() as usize;
        let side = 2 * t;
        let mut out = vec![0f32; 3 * side * side];
        for y in 0..side {
            for x in 0..side {
                let i = (y / 2) * t + x / 2;
                let (r, g, b) = (input[i], (input[t * t + i] + input[2 * t * t + i]) / 2.0, input[3 * t * t + i]);
                out[y * side + x] = r * 0.5;
                out[side * side + y * side + x] = g * 0.5;
                out[2 * side * side + y * side + x] = b * 0.5;
            }
        }
        Ok(out)
    }
}

impl Model for Dim {
    fn runner(&self, _: RunOn, threads: usize) -> (Box<dyn TileRunner + '_>, usize) {
        (Box::new(crate::denoise::Lent(self)), threads)
    }

    fn self_test(&self, _: RunOn) -> Result<Value, String> {
        Ok(json!({"ok": true}))
    }
}

fn dim_loader(runs: &Arc<AtomicUsize>) -> Loader {
    let runs = runs.clone();
    Arc::new(move |_, _| Ok(Arc::new(Dim { runs: runs.clone() }) as Arc<dyn Model>))
}

pub(crate) fn manifest(id: &str) -> DenoiserManifest {
    DenoiserManifest {
        id: id.into(),
        name: "Stand-in".into(),
        version: "1".into(),
        licence: Default::default(),
        source: None,
        sha256: None,
        size_bytes: None,
        provenance: "made up for a test".into(),
        domain: Domain::BayerToRgb,
        tile: 64,
        overlap: 16,
        gain: Gain::None,
    }
}

/// The model's `.onnx` (not a real one: the stand-in loader never reads it) with its manifest beside it.
fn model_files(dir: &Path, id: &str) -> PathBuf {
    let src = dir.join(format!("src-{id}"));
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("denoise-model.json"), serde_json::to_vec(&manifest(id)).unwrap()).unwrap();
    std::fs::write(src.join("model.onnx"), format!("not an onnx file, only a stand-in for {id}")).unwrap();
    src.join("model.onnx")
}

#[test]
fn model_installation_keeps_the_frame_pump_responsive_and_reuses_the_loaded_model() {
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;
    let mut x = setup("background-install", false);
    let path = model_files(&x.dir, "stand-in");
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let runs = x.runs.clone();
    let loads = Arc::new(AtomicUsize::new(0));
    let worker_loads = loads.clone();
    x.s.denoise.loader = Arc::new(move |_, _| {
        worker_loads.fetch_add(1, Ordering::Relaxed);
        entered_tx.send(()).map_err(|e| e.to_string())?;
        release_rx.lock().unwrap().recv_timeout(Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Ok(Arc::new(Dim { runs: runs.clone() }))
    });
    let info = x.s.execute("denoise.models.inspect", &json!({"path": path})).unwrap();
    assert_eq!(info["model"]["id"], "stand-in");
    assert_eq!(loads.load(Ordering::Relaxed), 0, "showing terms never executes the model");
    let r = x.s.execute("denoise.models.install", &json!({"path": path, "acknowledged": true, "background": true})).unwrap();
    assert_eq!(r["started"], "local-install");
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    // The loader is blocked: the frame pump must still return, reporting installation in progress.
    let state = x.s.execute("denoise.models.downloads", &json!({})).unwrap();
    assert_eq!(state["downloads"][0]["state"], "installing");
    // the built-in denoiser is in use meanwhile
    assert_eq!(x.s.execute("denoise.status", &json!({})).unwrap()["model"]["id"], "builtin");
    assert!(x.s.execute("denoise.models.install", &json!({"path": path, "acknowledged": true, "background": true})).is_err());
    release_tx.send(()).unwrap();
    for _ in 0..600 {
        let v = x.s.execute("denoise.models.downloads", &json!({})).unwrap();
        if v["downloads"][0]["state"] == "installed" {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(x.s.execute("denoise.models.downloads", &json!({})).unwrap()["downloads"][0]["state"], "installed");
    let a = x.s.denoise.active.as_ref().unwrap();
    assert!(a.model.lock().unwrap().is_some(), "the worker's tested model is reused");
    assert_eq!(loads.load(Ordering::Relaxed), 1);
    drop(x.s);
    let _ = std::fs::remove_dir_all(x.dir);
}

/// A Bayer DNG that is a smooth ramp, big enough for a 64-cell tile and a second one.
fn ramp_dng(w: usize, h: usize, cfa: &str) -> Vec<u8> {
    let data: Vec<u16> = (0..w * h).map(|i| 600 + ((i % w) * 30 + (i / w) * 35) as u16).collect();
    synthetic_dng_of(w, h, cfa, data, None, Default::default())
}

/// (Fields drop in order: the session, with its library open in `dir`, before the folder goes.)
struct Setup {
    s: Session,
    dir: lcc_testkit::TempDir,
    runs: Arc<AtomicUsize>,
}

/// A session with a library on disk holding one raw photo, and the stand-in model installed and chosen.
fn setup(tag: &str, with_model: bool) -> Setup {
    let dir = temp_dir(tag);
    let originals = dir.join("originals");
    std::fs::create_dir_all(&originals).unwrap();
    std::fs::write(originals.join("ramp.dng"), ramp_dng(192, 160, "RGGB")).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("library"), false).unwrap();
    s.execute("library.import", &json!({"paths": [originals.join("ramp.dng").to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.set_denoise_models_dir(Some(dir.join("denoise-models")));
    s.denoise.loader = dim_loader(&runs);
    if with_model {
        let onnx = model_files(&dir, "stand-in");
        s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap();
    }
    Setup { dir, s, runs }
}

fn photo(s: &Session) -> lcc_catalog::PhotoId {
    s.catalog.photos().next().unwrap().id
}

/// Pump until `n` photos are ready (the job runs on its own thread).
fn pump_until_ready(s: &mut Session, n: u64) -> Value {
    for _ in 0..600 {
        let r = s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
        if r["ready"].as_u64() >= Some(n) {
            return r;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("not ready: {}", s.execute("denoise.status", &json!({})).unwrap());
}

fn mean(img: &lcc_raster::Rgba8) -> f64 {
    let sum: u64 = img.data.iter().map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2])).sum();
    sum as f64 / (img.data.len() * 3) as f64
}

fn rendered_mean(s: &mut Session, id: lcc_catalog::PhotoId) -> f64 {
    mean(&s.render_now(id, 160, 160).unwrap().image)
}

fn set_amount(s: &mut Session, amount: f64) {
    s.execute("develop.set", &json!({"control": "enhance.denoise", "value": amount})).unwrap();
}

#[test]
fn a_model_is_installed_chosen_and_removed_with_the_terms_accepted_and_bad_manifests_are_refused() {
    let mut x = setup("model", false);
    let onnx = model_files(&x.dir, "stand-in");
    // the terms come first
    let e = x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("licence has not been accepted"), "{e}");
    let r = x.s.execute("denoise.models.list", &json!({})).unwrap();
    assert_eq!(r["model"], Value::Null);
    assert_eq!(r["runtime"], cfg!(feature = "denoise"));
    // a file with no manifest is not guessed at
    let stray = x.dir.join("stray.onnx");
    std::fs::write(&stray, b"who knows").unwrap();
    let e = x.s.execute("denoise.models.install", &json!({"path": stray.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("does not know this file"), "{e}");
    // installed, it is the model in use
    let r = x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap();
    assert_eq!((r["installed"]["id"].clone(), r["model"].clone()), (json!("stand-in"), json!("stand-in")), "{r}");
    assert!(r["installed"]["accepted"]["sha256"].as_str().is_some_and(|h| h.len() == 64), "the file's hash is recorded: {r}");
    assert!(x.s.denoise.active.is_some());
    assert_eq!(x.s.execute("denoise.status", &json!({})).unwrap()["model"]["id"], "stand-in");
    // a replacement that fails leaves the installed model as it was
    let installed = x.dir.join("denoise-models/stand-in/model.onnx");
    let original = std::fs::read(&installed).unwrap();
    std::fs::write(&onnx, "a different broken model").unwrap();
    assert!(x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).is_err());
    assert_eq!(std::fs::read(&installed).unwrap(), original, "the failed replacement left the installed file alone");
    assert_eq!(x.s.execute("denoise.models.list", &json!({})).unwrap()["model"], "stand-in");
    let onnx = model_files(&x.dir, "stand-in");
    // it can be checked again, switched off and on, and removed
    assert_eq!(x.s.execute("denoise.models.test", &json!({"id": "stand-in"})).unwrap()["ok"], true);
    assert_eq!(x.s.execute("denoise.models.select", &json!({"id": null})).unwrap()["model"], Value::Null);
    assert_eq!(x.s.denoise.active.as_ref().map(|a| a.id.as_str()), Some("builtin"), "without a model the built-in denoiser runs");
    assert!(x.s.execute("denoise.models.select", &json!({"id": "nothing"})).is_err());
    x.s.execute("denoise.models.select", &json!({"id": "stand-in"})).unwrap();
    assert!(x.s.denoise.active.is_some());
    let r = x.s.execute("denoise.models.remove", &json!({"id": "stand-in"})).unwrap();
    assert_eq!(r["model"], Value::Null);
    assert_eq!(x.s.denoise.active.as_ref().map(|a| a.id.as_str()), Some("builtin"));
    assert!(!x.dir.join("denoise-models").join("stand-in").exists());
    // ids that are not folder names, and folders that are not models, are never touched
    for bad in ["../library", "", "a/b", "settings.json"] {
        assert!(x.s.execute("denoise.models.remove", &json!({"id": bad})).is_err(), "{bad}");
    }
    assert!(x.dir.join("library").is_dir());
    // a model that fails its self-test is not left installed
    x.s.denoise.loader = Arc::new(|_, _| Err("it does not load".into()));
    let e = x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("it does not load"), "{e}");
    assert!(!x.dir.join("denoise-models").join("stand-in").exists());

    // a manifest that does not validate, or does not name the file, is refused
    x.s.denoise.loader = dim_loader(&x.runs);
    let json_path = onnx.with_file_name("denoise-model.json");
    // a tile the model cannot run on
    let mut m = manifest("stand-in");
    m.tile = 50;
    std::fs::write(&json_path, serde_json::to_vec(&m).unwrap()).unwrap();
    let e = x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("tile"), "{e}");
    // a hash that is not the file's
    let mut m = manifest("stand-in");
    m.sha256 = Some("0".repeat(64));
    std::fs::write(&json_path, serde_json::to_vec(&m).unwrap()).unwrap();
    let e = x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("SHA-256"), "{e}");
    assert!(!x.dir.join("denoise-models").join("stand-in").join("model.onnx").exists(), "nothing half-installed is left");
    // an id that would leave the folder
    let mut m = manifest("../escape");
    m.id = "../escape".into();
    std::fs::write(&json_path, serde_json::to_vec(&m).unwrap()).unwrap();
    assert!(x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).is_err());
    assert!(!x.dir.join("escape").exists());
    // garbage instead of a manifest
    std::fs::write(&json_path, b"{ not json").unwrap();
    assert!(x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).is_err());
    let _ = std::fs::remove_dir_all(&x.dir);
}

/// Another app's model folder (#1381): an `.onnx` with no manifest, then with that app's `model.json` beside it, then
/// with the `model.json` renamed. Each refusal says what is wrong and where the app's manifest is described.
#[test]
fn inspecting_another_apps_model_folder_says_what_is_missing() {
    let dir = temp_dir("denoise-foreign");
    let mut s = Session::new().with_fs();
    s.set_denoise_models_dir(Some(dir.join("denoise-models")));
    let src = dir.join("rawdenoise");
    std::fs::create_dir_all(&src).unwrap();
    let onnx = src.join("rawdenoise.onnx");
    std::fs::write(&onnx, b"not an onnx file").unwrap();
    let mut inspect = || s.execute("denoise.models.inspect", &json!({"path": onnx.to_string_lossy()})).unwrap_err().to_string();
    let e = inspect();
    assert!(e.contains("No denoise-model.json beside") && e.contains("docs/denoise.md") && !e.contains("model.json there"), "{e}");
    std::fs::write(src.join("model.json"), json!({"name": "rawdenoise", "input": {"channels": 4}, "scale": 2}).to_string()).unwrap();
    let e = inspect();
    assert!(e.contains("No denoise-model.json beside") && e.contains("The model.json there") && e.contains("docs/denoise.md"), "{e}");
    std::fs::rename(src.join("model.json"), src.join("denoise-model.json")).unwrap();
    let e = inspect();
    assert!(e.contains("not a denoise-model manifest") && e.contains("docs/denoise.md"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_photo_is_denoised_in_the_background_and_the_amount_mixes_it_in() {
    let mut x = setup("background", true);
    let id = photo(&x.s);
    // nothing is made until a photo has an amount
    let r = x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
    assert_eq!((r["queued"].clone(), r["running"].clone(), r["ready"].clone()), (json!(0), Value::Null, json!(0)), "{r}");
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Idle);
    let plain = rendered_mean(&mut x.s, id);
    let key_before = x.s.render_job(id, 160, 160, false, true).unwrap().key;

    set_amount(&mut x.s, 100.0);
    // the photo being edited is picked up by itself, and made on another thread
    let r = pump_until_ready(&mut x.s, 1);
    assert_eq!(r["queued"], 0, "{r}");
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Ready);
    assert!(x.runs.load(Ordering::Relaxed) >= 4, "a 192 × 160 mosaic is four 64-cell tiles: {}", x.runs.load(Ordering::Relaxed));
    let products: Vec<_> = std::fs::read_dir(x.dir.join("library").join("denoise")).unwrap().flatten().collect();
    assert_eq!(products.len(), 1, "one cached picture, no DNG or anything else in the library");
    assert!(products[0].path().extension().is_some_and(|e| e == "lcdn"));
    let originals: Vec<_> = std::fs::read_dir(x.dir.join("originals")).unwrap().flatten().collect();
    assert_eq!(originals.len(), 1, "the original is untouched and alone");

    // the render now comes from the denoised picture: darker at 100, in between at 50, the plain one at 0
    let full = rendered_mean(&mut x.s, id);
    assert!(full < plain - 3.0, "denoised {full} vs plain {plain}");
    set_amount(&mut x.s, 50.0);
    let half = rendered_mean(&mut x.s, id);
    assert!(half < plain - 1.0 && half > full + 1.0, "{full} < {half} < {plain}");
    set_amount(&mut x.s, 0.0);
    assert!((rendered_mean(&mut x.s, id) - plain).abs() < 0.01, "amount 0 is the plain picture again");
    // …and a render that uses it has another identity than one that does not
    assert_ne!(key_before, {
        set_amount(&mut x.s, 100.0);
        x.s.render_job(id, 160, 160, false, true).unwrap().key
    });
    // switching the Detail section off switches the denoise off with it
    set_amount(&mut x.s, 100.0);
    x.s.execute("develop.sectionEnabled", &json!({"section": "detail", "enabled": false})).unwrap();
    assert!((rendered_mean(&mut x.s, id) - plain).abs() < 0.01);
    assert_eq!(x.s.media.denoise_salt(id, &x.s.catalog.photo(id).unwrap().develop), 0);
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn what_was_made_is_found_again_by_a_new_session_and_a_new_model_starts_over() {
    let mut x = setup("again", true);
    let id = photo(&x.s);
    set_amount(&mut x.s, 100.0);
    pump_until_ready(&mut x.s, 1);
    let made = x.runs.load(Ordering::Relaxed);
    x.s.close_library().unwrap();
    drop(x.s);

    // reopened: ready without running the model again
    let mut s2 = Session::new().with_fs();
    s2.set_denoise_models_dir(Some(x.dir.join("denoise-models")));
    s2.denoise.loader = dim_loader(&x.runs);
    s2.open_library(x.dir.join("library"), false).unwrap();
    s2.denoise_pump(crate::denoise::Pace::Full);
    assert_eq!(s2.denoise_photo_state(id), PhotoState::Ready);
    assert_eq!(x.runs.load(Ordering::Relaxed), made, "nothing was made again");

    // another model's pictures are different pictures
    let onnx = model_files(&x.dir, "other");
    s2.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap();
    s2.denoise_pump(crate::denoise::Pace::Pause);
    assert!(
        matches!(s2.denoise_photo_state(id), PhotoState::Queued { .. }),
        "the first model's picture is not this model's: it is to be made: {:?}",
        s2.denoise_photo_state(id)
    );
    // and choosing the first again finds its picture where it was
    s2.execute("denoise.models.select", &json!({"id": "stand-in"})).unwrap();
    s2.denoise_pump(crate::denoise::Pace::Pause);
    assert_eq!(s2.denoise_photo_state(id), PhotoState::Ready);

    // the photo's content is something else now (a replaced file): its old picture is not its picture
    let p = s2.catalog.photo(id).unwrap().clone();
    s2.commit(
        "test",
        lcc_catalog::Op::SetContent {
            id,
            width: p.width,
            height: p.height,
            file_size: p.file_size + 1,
            content_hash: Some("ffffffffffffffffffffffffffffffff".into()),
            preview_only: None,
            preview_checked: String::new(),
        },
    )
    .unwrap();
    s2.denoise_pump(crate::denoise::Pace::Pause);
    assert_ne!(s2.denoise_photo_state(id), PhotoState::Ready, "a changed file has no picture until it is made again");
    // the old picture is not used by a render either
    assert_eq!(s2.media.denoise_salt(id, &s2.catalog.photo(id).unwrap().develop), 0);
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn an_export_makes_its_picture_when_the_queue_has_not_and_never_comes_out_without_it() {
    let mut x = setup("export", true);
    let id = photo(&x.s);
    let o = ExportOptions::from_json(&json!({"format": "png", "width": 160, "height": 160}));
    let decode = |bytes: &[u8]| {
        let d = lcc_codecs::decode(bytes, lcc_codecs::DecodeOptions::fit(160, 160)).unwrap();
        let w = d.to_working();
        w.data.iter().map(|p| f64::from(p[0] + p[1] + p[2])).sum::<f64>() / (w.data.len() * 3) as f64
    };
    let plain = decode(&export_photo(&mut x.s, id, &o, 1).unwrap().bytes);
    assert_eq!(x.runs.load(Ordering::Relaxed), 0, "no amount, no denoise");
    set_amount(&mut x.s, 100.0);
    // no pump: the export makes the picture itself
    let denoised = decode(&export_photo(&mut x.s, id, &o, 1).unwrap().bytes);
    assert!(x.runs.load(Ordering::Relaxed) >= 4);
    assert!(denoised < plain * 0.9, "denoised {denoised} vs plain {plain}");
    let products = std::fs::read_dir(x.dir.join("library").join("denoise")).unwrap().flatten().count();
    assert_eq!(products, 1);
    // a second export finds it
    let runs = x.runs.load(Ordering::Relaxed);
    let again = decode(&export_photo(&mut x.s, id, &o, 1).unwrap().bytes);
    assert_eq!(x.runs.load(Ordering::Relaxed), runs);
    assert!((again - denoised).abs() < 1e-6);
    // the pump takes in what the export made
    x.s.denoise_pump(crate::denoise::Pace::Pause);
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Ready);

    // no model: the built-in denoiser makes the picture instead (the stand-in is not asked)
    x.s.execute("denoise.models.select", &json!({"id": null})).unwrap();
    let runs = x.runs.load(Ordering::Relaxed);
    let builtin = decode(&export_photo(&mut x.s, id, &o, 1).unwrap().bytes);
    assert_eq!(x.runs.load(Ordering::Relaxed), runs);
    assert!(builtin.is_finite());
    assert_eq!(std::fs::read_dir(x.dir.join("library").join("denoise")).unwrap().flatten().count(), 2, "its picture is its own");
    set_amount(&mut x.s, 0.0);
    assert!(export_photo(&mut x.s, id, &o, 1).is_ok(), "without an amount nothing is asked of the model");
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn a_model_that_fails_fails_the_export_and_is_not_retried_by_the_queue() {
    let mut x = setup("fails", true);
    let id = photo(&x.s);
    x.s.denoise.loader = Arc::new(|_, _| Err("the model went away".into()));
    // Simulate loss of the in-memory model; successful installation now retains its tested instance.
    *x.s.denoise.active.as_ref().unwrap().model.lock().unwrap() = None;
    set_amount(&mut x.s, 80.0);
    let o = ExportOptions::from_json(&json!({"format": "png", "width": 100, "height": 100}));
    let e = export_photo(&mut x.s, id, &o, 1).err().unwrap();
    assert!(e.contains("ramp.dng") && e.contains("the model went away"), "{e}");
    // the background job fails too, says why once and does not try again every frame
    for _ in 0..200 {
        x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
        if matches!(x.s.denoise_photo_state(id), PhotoState::Failed { .. }) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    match x.s.denoise_photo_state(id) {
        PhotoState::Failed { why, unsupported } => assert!(why.contains("the model went away") && !unsupported, "{why}"),
        other => panic!("{other:?}"),
    }
    let r = x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
    assert_eq!((r["queued"].clone(), r["running"].clone()), (json!(0), Value::Null), "{r}");
    // asking again by hand does retry
    x.s.denoise.loader = dim_loader(&x.runs);
    let r = x.s.execute("denoise.queue", &json!({"retry": true})).unwrap();
    assert_eq!(r["added"], 1, "{r}");
    pump_until_ready(&mut x.s, 1);
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn a_corrupt_cache_with_a_current_header_never_silently_exports_the_plain_photo() {
    let mut x = setup("corrupt-export", true);
    let id = photo(&x.s);
    set_amount(&mut x.s, 100.0);
    let options = ExportOptions::from_json(&json!({"format": "png", "width": 160, "height": 160}));
    export_photo(&mut x.s, id, &options, 1).unwrap();
    x.s.denoise_pump(crate::denoise::Pace::Pause);
    let spec = x.s.media.denoise.spec(id).unwrap().clone();
    let mut bytes = std::fs::read(&spec.product).unwrap();
    for byte in bytes.iter_mut().rev().take(64) {
        *byte ^= 0xff;
    }
    std::fs::write(&spec.product, bytes).unwrap();
    assert!(lcc_denoise::product::is_current(&spec.product, &spec.key), "header/length still look current");
    x.s.media.forget(id);
    let error = export_photo(&mut x.s, id, &options, 1).err().unwrap();
    assert!(error.contains("AI Denoise picture could not be read"), "{error}");
    let _ = std::fs::remove_dir_all(x.dir);
}

#[test]
fn only_raw_photos_on_disk_are_denoised_and_nothing_is_queued_without_a_model() {
    let dir = temp_dir("eligible");
    let mut demo = Session::with_demo();
    demo.set_denoise_models_dir(Some(dir.join("models")));
    let id = demo.active().unwrap();
    assert_eq!(demo.denoise_photo_state(id), PhotoState::NotApplicable, "a generated photo has no sensor data");
    let e = demo.execute("denoise.queue", &json!({})).unwrap_err().to_string();
    assert!(e.contains("open a catalog on disk first"), "no model is needed, a place for the pictures is: {e}");
    // no models folder at all (the web build): the commands say so, nothing panics
    let mut web = Session::with_demo();
    for cmd in ["denoise.models.install", "denoise.models.select", "denoise.models.remove", "denoise.settings"] {
        assert!(web.execute(cmd, &json!({"path": "x", "acknowledged": true, "id": "x"})).is_err(), "{cmd}");
    }
    assert_eq!(web.execute("denoise.status", &json!({})).unwrap()["model"]["id"], "builtin");
    // no library open: nowhere to keep pictures, and the queue says so
    let x = setup("eligible2", true);
    let mut bare = Session::new().with_fs();
    bare.set_denoise_models_dir(Some(x.dir.join("denoise-models")));
    assert!(bare.denoise_products_dir().is_none());
    let e = bare.execute("denoise.queue", &json!({})).unwrap_err().to_string();
    assert!(e.contains("open a catalog on disk first"), "{e}");
    assert_eq!(bare.execute("denoise.pump", &json!({})).unwrap()["queued"], 0);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn files_that_cannot_be_denoised_fail_with_a_reason_and_never_a_panic() {
    let dir = temp_dir("makefails");
    let spec = |path: &Path| crate::denoise::JobSpec {
        path: path.to_string_lossy().into_owned(),
        read: None,
        product: dir.join("p.lcdn"),
        key: "k".into(),
        manifest: manifest("stand-in"),
        onnx: dir.join("model.onnx"),
        model: Default::default(),
        loader: dim_loader(&Arc::new(AtomicUsize::new(0))),
        parallel: 2,
        run_on: RunOn::Auto,
    };
    use crate::denoise::{MakeError, make_product};
    // not a file
    assert!(matches!(make_product(&spec(&dir.join("missing.dng")), None), Err(MakeError::Failed(_))));
    // not a raw
    let junk = dir.join("junk.dng");
    std::fs::write(&junk, b"this is not a raw file at all").unwrap();
    assert!(matches!(make_product(&spec(&junk), None), Err(MakeError::Unsupported(_))));
    // a raw cut short
    let whole = ramp_dng(192, 160, "RGGB");
    let cut = dir.join("cut.dng");
    std::fs::write(&cut, &whole[..whole.len() / 3]).unwrap();
    assert!(make_product(&spec(&cut), None).is_err());
    // a model that gives the wrong number of values is an error from the model, not a crash
    struct Wrong;
    impl TileRunner for Wrong {
        fn run(&self, _: &[f32]) -> Result<Vec<f32>, lcc_denoise::Error> {
            Ok(vec![0.0; 7])
        }
    }
    impl Model for Wrong {
        fn runner(&self, _: RunOn, threads: usize) -> (Box<dyn TileRunner + '_>, usize) {
            (Box::new(crate::denoise::Lent(self)), threads)
        }
        fn self_test(&self, _: RunOn) -> Result<Value, String> {
            Err("no".into())
        }
    }
    let good = dir.join("good.dng");
    std::fs::write(&good, &whole).unwrap();
    let mut s = spec(&good);
    s.loader = Arc::new(|_, _| Ok(Arc::new(Wrong) as Arc<dyn Model>));
    match make_product(&s, None) {
        Err(MakeError::Failed(why)) => assert!(why.contains("7 numbers"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(!dir.join("p.lcdn").exists(), "a failed job leaves no product");
    // a job told to stop stops before doing anything
    let progress = crate::denoise::Progress::default();
    progress.cancel_now();
    assert_eq!(make_product(&spec(&good), Some(&progress)), Err(MakeError::Cancelled));
    // a Bayer layout of the other kinds is denoised too, and a product that exists is not made again
    for cfa in ["BGGR", "GRBG", "GBRG"] {
        let f = dir.join(format!("{cfa}.dng"));
        std::fs::write(&f, ramp_dng(192, 160, cfa)).unwrap();
        let mut s = spec(&f);
        s.product = dir.join(format!("{cfa}.lcdn"));
        assert_eq!(make_product(&s, None), Ok(true), "{cfa}");
        assert_eq!(make_product(&s, None), Ok(false), "{cfa} is already there");
    }
    // a model for finished pictures: a damaged picture, and a raw file (not a finished picture), are refused
    let rgb_spec = |path: &Path| crate::denoise::JobSpec {
        product: dir.join("rgb.lcdn"),
        manifest: DenoiserManifest { domain: Domain::RgbToRgb, ..manifest("rgb") },
        loader: Arc::new(|_, _| Ok(Arc::new(Same) as Arc<dyn Model>)),
        ..spec(path)
    };
    let junk_png = dir.join("junk.png");
    std::fs::write(&junk_png, b"\x89PNG\r\n\x1a\nnot really").unwrap();
    assert!(matches!(make_product(&rgb_spec(&junk_png), None), Err(MakeError::Unsupported(_))), "a damaged finished picture");
    assert!(matches!(make_product(&rgb_spec(&good), None), Err(MakeError::Unsupported(_))), "a raw file is not a finished picture");
    assert!(!dir.join("rgb.lcdn").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_threads_making_the_same_picture_make_it_once() {
    let dir = temp_dir("twice");
    let f = dir.join("a.dng");
    std::fs::write(&f, ramp_dng(192, 160, "RGGB")).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let spec = crate::denoise::JobSpec {
        path: f.to_string_lossy().into_owned(),
        read: None,
        product: dir.join("p.lcdn"),
        key: "k".into(),
        manifest: manifest("stand-in"),
        onnx: dir.join("model.onnx"),
        model: Default::default(),
        loader: dim_loader(&runs),
        parallel: 2,
        run_on: RunOn::Auto,
    };
    let made: Vec<bool> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..4).map(|_| sc.spawn(|| crate::denoise::make_product(&spec, None).unwrap())).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(made.iter().filter(|m| **m).count(), 1, "{made:?}");
    assert_eq!(runs.load(Ordering::Relaxed), 4, "one photo's four tiles, once");
    assert!(lcc_denoise::product::is_current(&spec.product, "k"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_cache_is_kept_within_its_limit_and_what_photos_use_goes_last() {
    let mut x = setup("evict", true);
    let id = photo(&x.s);
    set_amount(&mut x.s, 100.0);
    pump_until_ready(&mut x.s, 1);
    let used = x.dir.join("library").join("denoise");
    // two older pictures nobody uses
    for (i, name) in ["aaaa", "bbbb"].iter().enumerate() {
        let f = used.join(format!("{name}.lcdn"));
        std::fs::write(&f, vec![7u8; 1000]).unwrap();
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(1000 - i as u64);
        std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
    }
    let size_of = |p: &Path| std::fs::metadata(p).unwrap().len();
    let mine: PathBuf = std::fs::read_dir(&used).unwrap().flatten().map(|e| e.path()).find(|p| p.file_stem().is_some_and(|s| s.len() == 32)).unwrap();
    // room for the photo's own picture and one stray: the oldest stray goes, then the other
    x.s.denoise_evict_to(size_of(&mine) + 1500, None);
    assert!(!used.join("aaaa.lcdn").exists() && used.join("bbbb.lcdn").exists() && mine.exists());
    x.s.denoise_evict_to(size_of(&mine) + 10, None);
    assert!(!used.join("bbbb.lcdn").exists() && mine.exists());
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Ready);
    // too small for even that: the photo's own picture is the last thing to go, and the photo knows
    x.s.denoise_evict_to(10, None);
    assert!(!mine.exists());
    assert_ne!(x.s.denoise_photo_state(id), PhotoState::Ready);
    // …and it is made again, because the photo still has its amount
    pump_until_ready(&mut x.s, 1);
    assert!(mine.exists());
    // clearing deletes it all
    let r = x.s.execute("denoise.clear", &json!({})).unwrap();
    assert_eq!(r["deleted"], 1, "{r}");
    assert_eq!(std::fs::read_dir(&used).unwrap().flatten().count(), 0);
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn settings_are_checked_saved_and_a_paused_pump_starts_nothing() {
    let mut x = setup("settings", true);
    let r = x.s.execute("denoise.settings", &json!({"auto": false, "cacheGb": 5, "threads": 3})).unwrap();
    assert_eq!((r["auto"].clone(), r["cacheGb"].clone(), r["threads"].clone()), (json!(false), json!(5), json!(3)), "{r}");
    for bad in [
        json!({"cacheGb": 0}),
        json!({"cacheGb": "big"}),
        json!({"threads": 0}),
        json!({"threads": 1000}),
        json!({"auto": "yes"}),
        json!({"runOn": "fastest"}),
        json!({"runOn": true}),
    ] {
        assert!(x.s.execute("denoise.settings", &bad).is_err(), "{bad}");
    }
    assert_eq!(x.s.execute("denoise.models.list", &json!({})).unwrap()["cacheGb"], 5, "a refused change changes nothing");
    // the faster of the card and the processor unless the user says otherwise, and the interface can read the choice back
    assert_eq!(x.s.execute("denoise.models.list", &json!({})).unwrap()["runOn"], "auto");
    assert_eq!(x.s.execute("denoise.settings", &json!({"runOn": "cpu"})).unwrap()["runOn"], "cpu");
    assert_eq!(x.s.execute("denoise.models.list", &json!({})).unwrap()["runOn"], "cpu");
    let status = x.s.execute("denoise.status", &json!({})).unwrap();
    assert_eq!((status["runOn"].clone(), status["device"]["kind"].clone()), (json!("cpu"), json!("none")), "no model is loaded yet: {status}");
    // the setting before `runOn` still reads as what it meant
    let models = x.s.denoise.models_dir.clone().unwrap();
    let mut st = crate::denoise::read_settings(&models);
    (st.run_on, st.gpu) = (None, Some(false));
    crate::denoise::write_settings(&models, &st).unwrap();
    assert_eq!(x.s.execute("denoise.models.list", &json!({})).unwrap()["runOn"], "cpu");
    // choosing where it runs lets the card be tried again after a set-up that closed the app
    let onnx = crate::denoise::installed_models(&models).first().unwrap().onnx.clone();
    let marker = onnx.with_file_name(crate::denoise::GPU_SETUP_MARKER);
    std::fs::write(&marker, "setting up").unwrap();
    assert_eq!(x.s.execute("denoise.settings", &json!({"runOn": "auto"})).unwrap()["runOn"], "auto");
    assert!(!marker.exists(), "the crash is forgotten");
    assert_eq!(crate::denoise::read_settings(&models).gpu, None, "the old setting is not kept beside the new one");
    // with auto off nothing is started by itself, and the photo is made when asked
    let id = photo(&x.s);
    set_amount(&mut x.s, 100.0);
    for _ in 0..3 {
        x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
    }
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Idle);
    assert!(!x.s.denoise_auto() && !x.s.denoise_busy());
    x.s.execute("denoise.settings", &json!({"auto": true})).unwrap();
    // paused: it is queued but nothing starts
    for _ in 0..3 {
        let r = x.s.execute("denoise.pump", &json!({"pace": "pause"})).unwrap();
        assert_eq!(r["running"], Value::Null);
    }
    assert!(matches!(x.s.denoise_photo_state(id), PhotoState::Queued { .. }), "{:?}", x.s.denoise_photo_state(id));
    // …and cancelling forgets it
    assert_eq!(x.s.execute("denoise.cancel", &json!({})).unwrap()["cancelled"], 1);
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Idle);
    // when the pace allows, it is made
    x.s.execute("denoise.queue", &json!({"scope": "withAmount"})).unwrap();
    assert!(x.s.denoise_busy(), "queued with a model: the headless runs wait for it");
    pump_until_ready(&mut x.s, 1);
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Ready);
    assert!(!x.s.denoise_busy());
    // a session whose library is not on disk has nowhere to keep it: the photo says so instead of waiting for ever
    if let Some(l) = x.s.library.as_mut() {
        l.on_disk = false;
    }
    match x.s.denoise_photo_state(id) {
        PhotoState::Failed { why, unsupported } => assert!(unsupported && why.contains("catalog folder"), "{why}"),
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn a_picture_that_appears_while_its_job_runs_is_ready_only_when_the_job_reports() {
    // #719: the worker saves the picture, then tears down its thread pool before it reports. A pump in that window
    // used to find the file and mark the photo Ready while the session still counted the job as running: Ready and
    // busy at once, and the activity row stayed up for a photo that said it was done.
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;
    let mut x = setup("ready-while-running", true);
    let id = photo(&x.s);
    set_amount(&mut x.s, 80.0);
    // the job loads the model afresh and waits in the loader until it is released: a worker that is at work
    *x.s.denoise.active.as_ref().unwrap().model.lock().unwrap() = None;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let runs = x.runs.clone();
    x.s.denoise.loader = Arc::new(move |_, _| {
        entered_tx.send(()).map_err(|e| e.to_string())?;
        release_rx.lock().unwrap().recv_timeout(Duration::from_secs(10)).map_err(|e| e.to_string())?;
        Ok(Arc::new(Dim { runs: runs.clone() }) as Arc<dyn Model>)
    });
    x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(x.s.denoise_photo_state(id), PhotoState::Running { .. }));
    assert!(x.s.denoise_busy());
    // the picture lands on disk before the worker says so (as it does between its save and its report)
    let (product, key) = x.s.denoise_running_product().unwrap();
    let mut rgb = lcc_raster::Rgb32f::new(192, 160);
    rgb.data.fill([0.25, 0.25, 0.25]);
    lcc_denoise::product::write(&product, &rgb, &key).unwrap();
    assert!(lcc_denoise::product::is_current(&product, &key));
    x.s.denoise.touch(); // the index is also rebuilt from the folder in this window
    let r = x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
    assert!(matches!(x.s.denoise_photo_state(id), PhotoState::Running { .. }), "the job reports its own picture");
    assert_eq!(r["ready"], 0, "{r}");
    assert!(x.s.denoise_busy());
    // the worker reports: ready and idle in the same pump
    release_tx.send(()).unwrap();
    let r = pump_until_ready(&mut x.s, 1);
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Ready);
    assert_eq!(r["running"], Value::Null, "{r}");
    assert!(!x.s.denoise_busy());
    drop(x.s);
    let _ = std::fs::remove_dir_all(x.dir);
}

#[test]
fn the_denoise_switch_validates_arguments_and_accepts_explicit_targets_without_selection() {
    let mut x = setup("toggle", false);
    let id = photo(&x.s);
    for params in [json!({"id": "bad"}), json!({"id": u64::MAX}), json!({"id": id.0, "enabled": "yes"})] {
        assert!(x.s.execute("denoise.toggle", &params).is_err());
    }
    let model = model_files(&x.dir, "toggle-model");
    x.s.execute("denoise.models.install", &json!({"path": model.to_string_lossy(), "acknowledged": true})).unwrap();
    x.s.selection = Default::default();
    let undo = x.s.undo.len();
    let r = x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": true})).unwrap();
    assert_eq!(r["amount"], 50.0);
    assert_eq!(x.s.undo.len(), undo + 1);
    x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": false})).unwrap();
    assert_eq!(x.s.develop_of(id).unwrap().denoise_amount(), 0.0);
    // the switch is its own setting: flipping it keeps the Amount (so it compares with and without), and an Amount of 0
    // does not switch it off
    x.s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": true})).unwrap();
    x.s.execute("develop.set", &json!({"control": "enhance.denoise", "value": 80.0})).unwrap();
    let off = x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": false})).unwrap();
    assert_eq!(off["amount"], 80.0);
    let d = x.s.develop_of(id).unwrap();
    assert!(!d.enhance.denoise_enabled() && d.denoise_amount() == 0.0);
    assert_eq!(x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": true})).unwrap()["amount"], 80.0);
    let d = x.s.develop_of(id).unwrap();
    assert!((d.denoise_amount() - lcc_develop::denoise_weight(d.process.process(), 0.8)).abs() < 1e-6);
    x.s.execute("develop.set", &json!({"control": "enhance.denoise", "value": 0.0})).unwrap();
    assert!(x.s.develop_of(id).unwrap().enhance.denoise_enabled(), "sliding to 0 leaves it on");
    assert_eq!(x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": true})).unwrap()["amount"], 0.0);
    // with no `enabled`, the toggle flips what the switch says
    assert_eq!(x.s.execute("denoise.toggle", &json!({"id": id.0})).unwrap()["enabled"], false);
    drop(x.s);
    let _ = std::fs::remove_dir_all(x.dir);
}

#[test]
fn denoise_row_while_queue_is_busy() {
    let mut x = setup("activity", true);
    let rows = |s: &Session| s.activity.list().into_iter().filter(|t| t.kind == "denoise").collect::<Vec<_>>();
    let r = x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
    assert!(rows(&x.s).is_empty(), "nothing to make, no row: {r}");
    set_amount(&mut x.s, 100.0);
    let mut shown = false;
    for _ in 0..600 {
        let r = x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
        let rows = rows(&x.s);
        // (what the pump left running or queued is what the row reflects until the next pump)
        if x.s.denoise_busy() {
            // one batch row counting photos, and under it the running photo's tiles
            let batch = rows.iter().find(|t| t.batch).unwrap_or_else(|| panic!("a batch row: {rows:?} {r}"));
            assert_eq!(
                (batch.label.as_str(), batch.cancellable, batch.unit, batch.total),
                ("AI Denoise", true, crate::activity::Unit::Count, 1),
                "{r}"
            );
            if !r["running"].is_null() {
                assert_eq!(rows.len(), 2, "{rows:?}");
                let photo = rows.iter().find(|t| !t.batch).unwrap_or_else(|| panic!("{rows:?}"));
                assert_eq!((photo.parent, photo.unit, photo.detail.as_str()), (Some(batch.id), crate::activity::Unit::Percent, "ramp.dng"), "{r}");
                assert_eq!(batch.detail, "ramp.dng", "{r}");
            }
            shown = true;
        } else {
            assert!(rows.is_empty(), "{r}");
        }
        if r["ready"].as_u64() >= Some(1) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(shown, "the photo never showed a row");
    assert!(rows(&x.s).is_empty(), "the row goes once the photo is made");
    let done = x.s.activity.finished();
    let last = done.last().expect("the batch leaves its summary");
    assert_eq!((last.kind, last.done, last.failed), ("denoise", 1, 0));
    let _ = std::fs::remove_dir_all(&x.dir);
}

/// A model on two graphics cards ([`Model::lanes`] 2): the batch makes two photos at once, one per card (each in its
/// own row under the batch row, both listed by `running.photos`), never more; one at a light pace.
#[test]
fn a_model_on_two_cards_makes_two_photos_at_once() {
    use std::sync::{Condvar, Mutex};
    /// [`Dim`] on "two cards", its tiles held until the test lets them go.
    struct TwoCards {
        dim: Dim,
        gate: Arc<(Mutex<bool>, Condvar)>,
    }
    impl TileRunner for TwoCards {
        fn run(&self, input: &[f32]) -> Result<Vec<f32>, lcc_denoise::Error> {
            let (open, wake) = &*self.gate;
            let mut o = open.lock().unwrap();
            while !*o {
                o = wake.wait(o).unwrap();
            }
            drop(o);
            self.dim.run(input)
        }
    }
    impl Model for TwoCards {
        fn runner(&self, _: RunOn, threads: usize) -> (Box<dyn TileRunner + '_>, usize) {
            (Box::new(crate::denoise::Lent(self)), threads)
        }
        fn lanes(&self, _: RunOn) -> usize {
            2
        }
        fn self_test(&self, _: RunOn) -> Result<Value, String> {
            Ok(json!({"ok": true}))
        }
    }
    let mut x = setup("twocards", false);
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (runs, g) = (x.runs.clone(), gate.clone());
    x.s.denoise.loader = Arc::new(move |_, _| Ok(Arc::new(TwoCards { dim: Dim { runs: runs.clone() }, gate: g.clone() }) as Arc<dyn Model>));
    let onnx = model_files(&x.dir, "stand-in");
    x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap();
    let originals = x.dir.join("originals");
    let mut paths = Vec::new();
    for i in 0..3 {
        let path = originals.join(format!("two{i}.dng"));
        std::fs::write(&path, ramp_dng(192 + 2 * (i + 1), 160, "RGGB")).unwrap();
        paths.push(path.to_string_lossy().to_string());
    }
    x.s.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = x.s.catalog.photos().map(|p| p.id.0).collect();
    x.s.execute("library.select", &json!({"ids": ids, "active": ids[0]})).unwrap();
    x.s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    x.s.execute("denoise.toggle", &json!({"enabled": true})).unwrap();
    // the model installed (and loaded) says how many cards it runs on
    assert_eq!(x.s.denoise_lanes(crate::denoise::Pace::Full), 2);
    let rows = |s: &Session| s.activity.list().into_iter().filter(|t| t.kind == "denoise" && !t.batch).count();
    let mut two = false;
    for _ in 0..400 {
        x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
        let r = x.s.execute("denoise.status", &json!({})).unwrap();
        let photos = r["running"]["photos"].as_array().map_or(0, Vec::len);
        assert!(photos <= 2, "one photo per card: {r}");
        if photos == 2 {
            assert_eq!(rows(&x.s), 2, "a row for each photo being made");
            two = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(two, "two photos never ran at once: {}", x.s.execute("denoise.status", &json!({})).unwrap());
    assert_eq!(x.s.denoise_lanes(crate::denoise::Pace::Full), 2);
    assert_eq!(x.s.denoise_lanes(crate::denoise::Pace::Light), 1, "the user is working: one photo");
    // let them go: every photo is made
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    pump_until_ready(&mut x.s, ids.len() as u64);
    let _ = std::fs::remove_dir_all(&x.dir);
}

/// the AI Denoise switch follows the Develop sliders. With Auto Sync on, every selected raw photo switches
/// with the active one, in one undo step, and their pictures are made under one batch row; with it off, only the active
/// photo. Explicit `ids` switch exactly those.
#[test]
fn the_denoise_switch_follows_auto_sync_in_one_undo_step() {
    let mut x = setup("autosync", true);
    let originals = x.dir.join("originals");
    let mut paths = Vec::new();
    for i in 0..3 {
        let path = originals.join(format!("more{i}.dng"));
        std::fs::write(&path, ramp_dng(192 + 2 * (i + 1), 160, "RGGB")).unwrap();
        paths.push(path.to_string_lossy().to_string());
    }
    x.s.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = x.s.catalog.photos().map(|p| p.id.0).collect();
    assert_eq!(ids.len(), 4);
    x.s.execute("library.select", &json!({"ids": ids, "active": ids[0]})).unwrap();
    x.s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    let on = |s: &Session, id: u64| s.develop_of(lcc_catalog::PhotoId(id)).unwrap().enhance.denoise_enabled();
    let undo = x.s.undo.len();
    let r = x.s.execute("denoise.toggle", &json!({"enabled": true})).unwrap();
    assert_eq!(r["ids"].as_array().map(Vec::len), Some(4), "{r}");
    assert_eq!(x.s.undo.len(), undo + 1, "one undo step");
    assert!(ids.iter().all(|id| on(&x.s, *id)));
    assert_eq!(x.s.develop_of(lcc_catalog::PhotoId(ids[3])).unwrap().enhance.denoise, 50.0);
    // the pictures are made under one batch row, and it says how it went
    pump_until_ready(&mut x.s, 4);
    for _ in 0..20 {
        x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
    }
    let done = x.s.activity.finished();
    let last = done.last().expect("a summary");
    assert_eq!((last.kind, last.done, last.failed), ("denoise", 4, 0), "{done:?}");
    // undo: all four go back off at once
    x.s.execute("edit.undo", &json!({})).unwrap();
    assert!(ids.iter().all(|id| !on(&x.s, *id)));
    x.s.execute("edit.redo", &json!({})).unwrap();
    // Auto Sync off: only the active photo
    x.s.execute("develop.autoSync", &json!({"on": false})).unwrap();
    let r = x.s.execute("denoise.toggle", &json!({"enabled": false})).unwrap();
    assert_eq!(r["ids"], json!([ids[0]]));
    assert!(!on(&x.s, ids[0]) && on(&x.s, ids[1]));
    // explicit ids: exactly those
    x.s.execute("denoise.toggle", &json!({"ids": [ids[1], ids[2]], "enabled": false})).unwrap();
    assert!(!on(&x.s, ids[1]) && !on(&x.s, ids[2]) && on(&x.s, ids[3]));
    assert!(x.s.execute("denoise.toggle", &json!({"ids": "all"})).is_err());
    drop(x.s);
    let _ = std::fs::remove_dir_all(x.dir);
}

/// cancelling AI Denoise's batch row stops it, and the photos it held are not queued again just because
/// they are looked at (Auto); asking for them again does queue them.
#[test]
fn a_cancelled_denoise_batch_stays_cancelled_until_asked_again() {
    let mut x = setup("cancelbatch", true);
    x.s.execute("denoise.settings", &json!({"auto": true})).unwrap();
    set_amount(&mut x.s, 100.0);
    x.s.execute("denoise.pump", &json!({"pace": "pause"})).unwrap();
    let batch = x.s.activity.list().into_iter().find(|t| t.kind == "denoise" && t.batch).expect("a batch row");
    x.s.activity.cancel(batch.id).unwrap();
    x.s.execute("denoise.pump", &json!({"pace": "pause"})).unwrap();
    assert!(!x.s.denoise_busy(), "stopped");
    for _ in 0..3 {
        x.s.execute("denoise.pump", &json!({"pace": "pause"})).unwrap();
    }
    assert!(!x.s.denoise_busy(), "not queued again by being looked at");
    x.s.execute("denoise.queue", &json!({})).unwrap();
    assert!(x.s.denoise_busy(), "asked for again");
    drop(x.s);
    let _ = std::fs::remove_dir_all(x.dir);
}

/// A job told to stop by the batch row's ✕ winds down on its own thread: until it has, no new batch row
/// appears for it. The photo it was making waits until asked for again.
#[test]
fn a_cancelled_batch_does_not_come_back_while_its_job_winds_down() {
    let mut x = setup("cancelrunning", true);
    // a model that does not load until let go: the job is running all that while
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let loading = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (g, runs, l) = (gate.clone(), x.runs.clone(), loading.clone());
    x.s.denoise.loader = Arc::new(move |_, _| {
        l.store(true, Ordering::SeqCst);
        let (open, wake) = &*g;
        let mut open = open.lock().unwrap();
        while !*open {
            open = wake.wait(open).unwrap();
        }
        Ok(Arc::new(Dim { runs: runs.clone() }) as Arc<dyn Model>)
    });
    // the installation keeps the model it tested: without forgetting it the job never asks this loader
    *x.s.denoise.active.as_ref().unwrap().model.lock().unwrap() = None;
    x.s.execute("denoise.settings", &json!({"auto": true})).unwrap();
    set_amount(&mut x.s, 100.0);
    x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
    let batches = |s: &Session| s.activity.list().into_iter().filter(|t| t.kind == "denoise" && t.batch).count();
    let batch = x.s.activity.list().into_iter().find(|t| t.kind == "denoise" && t.batch).expect("a batch row");
    // cancel once the job waits for its model (a job told to stop before that, while it reads the file, ends at once)
    let began = std::time::Instant::now();
    while !loading.load(Ordering::SeqCst) {
        assert!(began.elapsed() < std::time::Duration::from_secs(30), "the job never asked for its model");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    x.s.activity.cancel(batch.id).unwrap();
    for _ in 0..3 {
        x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
        assert!(x.s.denoise_busy(), "the job is still winding down");
        assert_eq!(batches(&x.s), 0, "no new batch row for a job that is stopping");
    }
    assert!(x.s.denoise_held(photo(&x.s)), "the photo waits until asked for again");
    let (open, wake) = &*gate;
    *open.lock().unwrap() = true;
    wake.notify_all();
    for _ in 0..200 {
        x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
        if !x.s.denoise_busy() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!x.s.denoise_busy(), "stopped");
    assert_eq!(batches(&x.s), 0);
    drop(x.s);
    let _ = std::fs::remove_dir_all(x.dir);
}

/// Browsing a folder imports nothing: "AI Denoise from ISO N" makes no pictures for the photos browsed.
#[test]
fn ai_denoise_above_an_iso_is_switched_on_at_import_and_never_by_browsing() {
    let mut x = setup("browsedenoise", true);
    // (not made because it is looked at either: it is the open photo, and pictures are made on their own)
    x.s.execute("denoise.settings", &json!({"auto": true})).unwrap();
    // "AI Denoise from ISO N" switches it on for new raw photos shot at that ISO or above (part of the look they start
    // from), and leaves the others alone
    let r = x.s.execute("library.preferences", &json!({"import": {"denoiseAboveIso": 3200}})).unwrap();
    assert_eq!(r["import"]["denoiseAboveIso"], 3200);
    assert!(x.s.execute("library.preferences", &json!({"import": {"denoiseAboveIso": 7}})).is_err());
    let mut p = (**x.s.catalog.photo(photo(&x.s)).unwrap()).clone();
    p.develop = std::sync::Arc::new(lcc_develop::DevelopSettings::default());
    p.meta.iso = Some(6400);
    crate::import::apply_import_defaults(&x.s, &mut p);
    assert!(p.develop.enhance.denoise_enabled() && p.develop.enhance.denoise == 50.0);
    assert!(p.import_look.as_ref().is_some_and(|l| l.enhance.denoise_enabled()), "where editing starts");
    p.meta.iso = Some(800);
    p.develop = std::sync::Arc::new(lcc_develop::DevelopSettings::default());
    crate::import::apply_import_defaults(&x.s, &mut p);
    assert!(!p.develop.enhance.denoise_enabled(), "below the ISO");
    // off again
    let r = x.s.execute("library.preferences", &json!({"import": {"denoiseAboveIso": 0}})).unwrap();
    assert!(r["import"].get("denoiseAboveIso").is_none(), "{r}");
    p.meta.iso = Some(25_600);
    crate::import::apply_import_defaults(&x.s, &mut p);
    assert!(!p.develop.enhance.denoise_enabled());
    x.s.execute("library.preferences", &json!({"import": {"denoiseAboveIso": 3200}})).unwrap();
    let folder = x.dir.join("browsed");
    std::fs::create_dir_all(&folder).unwrap();
    let data: Vec<u16> = (0..192 * 160).map(|i| 600 + (i % 192) as u16 * 30).collect();
    let meta = lcc_meta::Metadata { iso: Some(6400), ..Default::default() };
    std::fs::write(folder.join("high.dng"), synthetic_dng_of(192, 160, "RGGB", data, None, meta)).unwrap();
    x.s.execute("library.browse", &json!({"path": folder.to_string_lossy()})).unwrap();
    let browsed = x.s.catalog.photos().find(|p| p.local).map(|p| p.id).expect("the browsed photo");
    x.s.execute("library.select", &json!({"ids": [browsed.0]})).unwrap();
    for _ in 0..3 {
        x.s.execute("denoise.pump", &json!({"pace": "pause"})).unwrap();
    }
    assert!(!x.s.denoise_busy(), "{}", x.s.execute("denoise.status", &json!({})).unwrap());
    // imported, the same file is queued
    x.s.execute("library.import", &json!({"paths": [folder.join("high.dng").to_string_lossy()]})).unwrap();
    assert!(x.s.denoise_busy(), "imported at ISO 6400: queued");
    drop(x.s);
    let _ = std::fs::remove_dir_all(x.dir);
}

/// An import that switches AI Denoise on for more photos than the queue holds loses none: the rest
/// wait and are queued as it drains.
#[test]
fn an_import_larger_than_the_queue_makes_every_picture() {
    let mut x = setup("importbacklog", true);
    // (Auto on: the cancelled photo, selected, still isn't made until asked for)
    x.s.execute("denoise.settings", &json!({"auto": true})).unwrap();
    x.s.execute("library.preferences", &json!({"import": {"denoiseAboveIso": 3200}})).unwrap();
    x.s.denoise.queue_cap = 1;
    let folder = x.dir.join("high-iso");
    std::fs::create_dir_all(&folder).unwrap();
    let mut paths = Vec::new();
    for i in 0..3u16 {
        let data: Vec<u16> = (0..192 * 160).map(|j| 600 + (j % 192) as u16 * 30 + i * 7).collect();
        let meta = lcc_meta::Metadata { iso: Some(6400), ..Default::default() };
        let path = folder.join(format!("high-{i}.dng"));
        std::fs::write(&path, synthetic_dng_of(192, 160, "RGGB", data, None, meta)).unwrap();
        paths.push(path.to_string_lossy().into_owned());
    }
    x.s.execute("library.import", &json!({"paths": paths.clone()})).unwrap();
    assert!(x.s.denoise_busy());
    // cancelling one waiting photo takes it out of the backlog too
    let last = x.s.catalog.photos().find(|p| p.file_name == "high-2.dng").map(|p| p.id).expect("imported");
    assert_eq!(x.s.execute("denoise.cancel", &json!({"id": last.0})).unwrap()["cancelled"], 1);
    x.s.execute("library.select", &json!({"ids": [last.0]})).unwrap();
    pump_until_ready(&mut x.s, 2);
    for _ in 0..20 {
        x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
    }
    assert!(!x.s.denoise_busy());
    assert_eq!(x.s.execute("denoise.status", &json!({})).unwrap()["ready"], 2, "the cancelled photo isn't made");
    x.s.execute("denoise.queue", &json!({"ids": [last.0]})).unwrap();
    pump_until_ready(&mut x.s, 3);
    drop(x.s);
    let _ = std::fs::remove_dir_all(x.dir);
}

/// With no model installed, turning denoise on makes the picture with the built-in denoiser alone; it is not an
/// installed model, and no model can take its id.
#[test]
fn without_a_model_the_built_in_denoiser_makes_the_picture_by_itself() {
    let mut x = setup("builtin", false);
    let id = photo(&x.s);
    assert!(x.s.denoise.active.is_none(), "nothing is looked at before it is needed");
    let r = x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": true})).unwrap();
    assert_eq!(r["amount"], 50.0);
    assert_eq!(x.s.denoise.active.as_ref().map(|a| a.id.as_str()), Some("builtin"));
    let r = pump_until_ready(&mut x.s, 1);
    assert_eq!(r["queued"], 0, "{r}");
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Ready);
    assert_eq!(x.runs.load(Ordering::Relaxed), 0, "no model was involved");
    assert_eq!(x.s.execute("denoise.status", &json!({})).unwrap()["model"]["name"], "Built-in");
    assert!(x.s.execute("denoise.models.test", &json!({"id": "builtin"})).is_err(), "it is not an installed model");
    // a model cannot take its id
    let onnx = model_files(&x.dir, "builtin");
    let e = x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap_err().to_string();
    assert!(e.contains("builtin"), "{e}");
    let _ = std::fs::remove_dir_all(&x.dir);
}

/// The process's Denoise response, measured on a flat grey patch with near-Gaussian noise and a denoised twin that is a
/// 9×9 box filter of it (a stand-in for the model that leaves about a ninth of the noise): V5's straight mix leaves half
/// the noise at 50, V6's at most 0.3 of it, and the residual falls with every step of the slider.
#[test]
fn denoise_response_v6_is_mostly_clean_at_half_and_monotone() {
    use lcc_develop::{DevelopSettings, Process};
    use lcc_raster::Rgb32f;
    let (w, h) = (128usize, 128usize);
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = || {
        // sum of four uniforms: close enough to Gaussian, deterministic
        (0..4)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5
            })
            .sum::<f32>()
            * 0.05
    };
    let mut noisy = Rgb32f::new(w, h);
    for p in noisy.data.iter_mut() {
        *p = [0.18 + rnd(); 3];
    }
    let mut clean = noisy.clone();
    for y in 0..h {
        for x in 0..w {
            let (mut s, mut n) = (0.0f32, 0.0f32);
            for yy in y.saturating_sub(4)..(y + 5).min(h) {
                for xx in x.saturating_sub(4)..(x + 5).min(w) {
                    s += noisy.data[yy * w + xx][0];
                    n += 1.0;
                }
            }
            clean.data[y * w + x] = [s / n; 3];
        }
    }
    let sigma = |img: &Rgb32f| {
        let m = img.data.iter().map(|p| p[0]).sum::<f32>() / img.data.len() as f32;
        (img.data.iter().map(|p| (p[0] - m).powi(2)).sum::<f32>() / img.data.len() as f32).sqrt()
    };
    let s0 = sigma(&noisy);
    let residual = |process: Process, slider: f64| {
        let mut s = DevelopSettings { process: process.version(), ..DevelopSettings::default() };
        s.enhance.denoise = slider;
        s.enhance.denoise_on = Some(true);
        let mut mixed = noisy.clone();
        assert!(lcc_denoise_core::product::blend(&mut mixed, &clean, s.denoise_amount()));
        sigma(&mixed) / s0
    };
    let mut report = String::new();
    for slider in [0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0] {
        report += &format!("{slider:>4}: V5 {:.3}  V6 {:.3}\n", residual(Process::V5, slider), residual(Process::V6, slider));
    }
    eprintln!("residual sigma / original (denoised twin {:.3}):\n{report}", sigma(&clean) / s0);
    assert!((residual(Process::V5, 50.0) - 0.55).abs() < 0.1, "V5 keeps its straight mix");
    assert!(residual(Process::V6, 50.0) <= 0.3);
    let mut last = f32::INFINITY;
    for step in 0..=100 {
        let r = residual(Process::V6, step as f64);
        assert!(r <= last + 1e-4, "residual rises at {step}");
        last = r;
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// finished pictures (JPEG, PNG, TIFF …): a model of their own (adapted from upstream #3279)

/// A stand-in model for finished pictures: every value halved, at the same size.
struct Half {
    runs: Arc<AtomicUsize>,
}

impl TileRunner for Half {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, lcc_denoise::Error> {
        self.runs.fetch_add(1, Ordering::Relaxed);
        Ok(input.iter().map(|v| v * 0.5).collect())
    }
}

impl Model for Half {
    fn runner(&self, _: RunOn, threads: usize) -> (Box<dyn TileRunner + '_>, usize) {
        (Box::new(crate::denoise::Lent(self)), threads)
    }

    fn self_test(&self, _: RunOn) -> Result<Value, String> {
        Ok(json!({"ok": true}))
    }
}

/// A model for finished pictures that changes nothing.
struct Same;

impl TileRunner for Same {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, lcc_denoise::Error> {
        Ok(input.to_vec())
    }
}

impl Model for Same {
    fn runner(&self, _: RunOn, threads: usize) -> (Box<dyn TileRunner + '_>, usize) {
        (Box::new(crate::denoise::Lent(self)), threads)
    }

    fn self_test(&self, _: RunOn) -> Result<Value, String> {
        Ok(json!({"ok": true}))
    }
}

/// A model for finished pictures, with its manifest beside it (see [`model_files`]).
fn rgb_model_files(dir: &Path, id: &str) -> PathBuf {
    let src = dir.join(format!("src-{id}"));
    std::fs::create_dir_all(&src).unwrap();
    let m = DenoiserManifest { domain: Domain::RgbToRgb, ..manifest(id) };
    std::fs::write(src.join("denoise-model.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    std::fs::write(src.join("model.onnx"), format!("not an onnx file, only a stand-in for {id}")).unwrap();
    src.join("model.onnx")
}

/// Write `img` as a PNG in the setup's originals and import it.
fn import_png(x: &mut Setup, name: &str, img: &lcc_raster::Rgba8) -> lcc_catalog::PhotoId {
    let path = x.dir.join("originals").join(name);
    let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(&path, png).unwrap();
    x.s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    x.s.catalog.photos().find(|p| p.file_name == name).unwrap().id
}

/// What a finished picture without a model for its kind reports.
fn needs_rgb_model(state: &PhotoState) -> bool {
    matches!(state, PhotoState::Failed { why, unsupported: true } if why.contains("finished pictures"))
}

#[test]
fn a_finished_picture_has_a_model_of_its_own_and_its_amount_mixes_in_the_denoised_picture() {
    let mut x = setup("finished", true);
    let raw = photo(&x.s);
    let id = import_png(&mut x, "gradient.png", &lcc_raster::Rgba8::from_fn(150, 100, |px, py| [(40 + px) as u8, (60 + py) as u8, 120, 255]));
    let rgb_runs = Arc::new(AtomicUsize::new(0));
    let (dim, half) = (x.runs.clone(), rgb_runs.clone());
    x.s.denoise.loader = Arc::new(move |_, m: &DenoiserManifest| {
        Ok(match m.domain {
            Domain::RgbToRgb => Arc::new(Half { runs: half.clone() }) as Arc<dyn Model>,
            Domain::BayerToRgb | Domain::RgbToRgb4x => Arc::new(Dim { runs: dim.clone() }) as Arc<dyn Model>,
        })
    });
    x.s.denoise_pump(crate::denoise::Pace::Pause);
    // the raw photo's model is not the picture's: it has none yet, and switching it on asks for one of its kind
    assert!(needs_rgb_model(&x.s.denoise_photo_state(id)), "{:?}", x.s.denoise_photo_state(id));
    assert!(!needs_rgb_model(&x.s.denoise_photo_state(raw)));
    let e = x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": true})).unwrap_err().to_string();
    assert!(e.contains("finished pictures"), "{e}");

    // installed, the model for finished pictures sits beside the raw one
    let onnx = rgb_model_files(&x.dir, "stand-in-rgb");
    let r = x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap();
    assert_eq!((r["model"].clone(), r["rgbModel"].clone()), (json!("stand-in"), json!("stand-in-rgb")), "{r}");
    let list = x.s.execute("denoise.models.list", &json!({})).unwrap();
    let row = |id: &str| list["models"].as_array().unwrap().iter().find(|m| m["id"] == id).cloned().unwrap();
    assert_eq!((row("stand-in")["selected"].clone(), row("stand-in")["domain"].clone()), (json!(true), json!("bayerToRgb")));
    assert_eq!((row("stand-in-rgb")["selected"].clone(), row("stand-in-rgb")["domain"].clone()), (json!(true), json!("rgbToRgb")));
    assert_eq!(x.s.execute("denoise.status", &json!({})).unwrap()["rgbModel"]["id"], "stand-in-rgb");

    // on, its picture is made at the file's size, and the Amount mixes it in
    x.s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let plain = rendered_mean(&mut x.s, id);
    let on = x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": true})).unwrap();
    assert_eq!(on["amount"], 50.0);
    set_amount(&mut x.s, 100.0);
    pump_until_ready(&mut x.s, 1);
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Ready);
    assert!(rgb_runs.load(Ordering::Relaxed) > 0);
    assert_eq!(x.runs.load(Ordering::Relaxed), 0, "the raw model is not asked about a finished picture");
    let product = std::fs::read_dir(x.dir.join("library").join("denoise")).unwrap().flatten().next().unwrap().path();
    let header = lcc_denoise::product::read_header_at(&product).unwrap();
    assert_eq!((header.width, header.height), (150, 100));
    let denoised = rendered_mean(&mut x.s, id);
    assert!(denoised < plain * 0.8, "denoised {denoised} vs plain {plain}");
    set_amount(&mut x.s, 0.0);
    assert!((rendered_mean(&mut x.s, id) - plain).abs() < 1.0, "at 0 the plain picture is back");

    // off for finished pictures only: the raw photos keep theirs
    let r = x.s.execute("denoise.models.select", &json!({"id": null, "kind": "rgb"})).unwrap();
    assert_eq!((r["model"].clone(), r["rgbModel"].clone()), (json!("stand-in"), Value::Null));
    assert!(needs_rgb_model(&x.s.denoise_photo_state(id)));
    assert_eq!(x.s.denoise.active.as_ref().map(|a| a.id.as_str()), Some("stand-in"));
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn a_finished_pictures_denoised_picture_comes_back_in_the_plain_pictures_colours_and_on_white() {
    // with a model that changes nothing, Amount 100 must render the plain picture: the trip to sRGB for the model and
    // back to the working space for the render (and the product's half floats) lose nothing that shows
    let mut x = setup("finished-colour", false);
    let img = lcc_raster::Rgba8::from_fn(150, 100, |px, py| [(20 + px) as u8, (200 - py) as u8, ((px * 7 + py * 3) % 256) as u8, 255]);
    let id = import_png(&mut x, "colours.png", &img);
    x.s.denoise.loader = Arc::new(|_, _| Ok(Arc::new(Same) as Arc<dyn Model>));
    let onnx = rgb_model_files(&x.dir, "same");
    x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap();
    x.s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let plain = x.s.render_now(id, 160, 160).unwrap().image;
    x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": true})).unwrap();
    set_amount(&mut x.s, 100.0);
    pump_until_ready(&mut x.s, 1);
    let denoised = x.s.render_now(id, 160, 160).unwrap().image;
    assert_eq!((plain.width, plain.height), (denoised.width, denoised.height));
    let worst = plain.data.iter().zip(&denoised.data).flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c]))).max().unwrap();
    assert!(worst <= 2, "a model that changes nothing changed a pixel by {worst} levels");

    // a transparent picture is flattened onto white when shown plain: the denoised one must be too, or Amount 100 shows
    // what lies under the transparency (black here)
    let rgba = lcc_raster::Rgba8::from_fn(150, 100, |px, _| if px < 75 { [0, 0, 0, 0] } else { [90, 140, 60, 255] });
    let clear = import_png(&mut x, "half-clear.png", &rgba);
    x.s.execute("library.select", &json!({"ids": [clear.0]})).unwrap();
    let plain = x.s.render_now(clear, 160, 160).unwrap().image;
    assert!(plain.data[10][0] > 200, "the plain picture shows white where it is clear: {:?}", plain.data[10]);
    x.s.execute("denoise.toggle", &json!({"id": clear.0, "enabled": true})).unwrap();
    set_amount(&mut x.s, 100.0);
    pump_until_ready(&mut x.s, 2);
    let denoised = x.s.render_now(clear, 160, 160).unwrap().image;
    let worst = plain.data.iter().zip(&denoised.data).flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c]))).max().unwrap();
    assert!(worst <= 2, "the transparent picture's denoised twin differs by {worst} levels");
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn choosing_the_model_for_finished_pictures_leaves_the_raw_photos_queue_alone() {
    let mut x = setup("slots-queue", true);
    let raw = photo(&x.s);
    x.s.execute("denoise.queue", &json!({"ids": [raw.0]})).unwrap();
    x.s.denoise_pump(crate::denoise::Pace::Pause);
    assert!(matches!(x.s.denoise_photo_state(raw), PhotoState::Queued { .. }), "{:?}", x.s.denoise_photo_state(raw));
    let onnx = rgb_model_files(&x.dir, "stand-in-rgb");
    x.s.execute("denoise.models.install", &json!({"path": onnx.to_string_lossy(), "acknowledged": true})).unwrap();
    x.s.execute("denoise.models.select", &json!({"id": null, "kind": "rgb"})).unwrap();
    x.s.denoise_pump(crate::denoise::Pace::Pause);
    assert!(matches!(x.s.denoise_photo_state(raw), PhotoState::Queued { .. }), "{:?}", x.s.denoise_photo_state(raw));
    // the raw model changing (to the built-in denoiser) does drop it: another model's picture is another picture
    x.s.execute("denoise.models.select", &json!({"id": null})).unwrap();
    x.s.denoise_pump(crate::denoise::Pace::Pause);
    assert!(!matches!(x.s.denoise_photo_state(raw), PhotoState::Queued { .. }), "{:?}", x.s.denoise_photo_state(raw));
    let _ = std::fs::remove_dir_all(&x.dir);
}
