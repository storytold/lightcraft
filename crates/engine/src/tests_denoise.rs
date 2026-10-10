//! AI denoise through the engine: installing a model, making a photo's denoised picture in the background and for an
//! export, the Amount slider mixing it in, and the cache. A tiny stand-in model (it halves every sample) takes the
//! place of a real one, which is enough to see what the machinery does with its answer; how well real models denoise is
//! measured separately (`crates/denoise/tests/real_model.rs`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lightcraft_denoise::manifest::{DenoiserManifest, Domain, Gain};
use lightcraft_denoise::run::TileRunner;
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
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, lightcraft_denoise::Error> {
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
    fn runner(&self, _: RunOn, threads: usize) -> (&dyn TileRunner, usize) {
        (self, threads)
    }

    fn self_test(&self, _: RunOn) -> Result<Value, String> {
        Ok(json!({"ok": true}))
    }
}

fn dim_loader(runs: &Arc<AtomicUsize>) -> Loader {
    let runs = runs.clone();
    Arc::new(move |_, _| Ok(Arc::new(Dim { runs: runs.clone() }) as Arc<dyn Model>))
}

fn manifest(id: &str) -> DenoiserManifest {
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
    assert_eq!(x.s.execute("denoise.pump", &json!({"pace": "pause"})).unwrap()["active"], false);
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

#[test]
fn failed_model_replacement_preserves_the_installed_model() {
    let mut x = setup("replacement", true);
    let installed = x.dir.join("denoise-models/stand-in/model.onnx");
    let original = std::fs::read(&installed).unwrap();
    let source = model_files(&x.dir, "stand-in");
    std::fs::write(&source, "a different broken model").unwrap();
    assert!(x.s.execute("denoise.models.install", &json!({"path": source, "acknowledged": true})).is_err());
    assert_eq!(std::fs::read(installed).unwrap(), original);
    assert_eq!(x.s.execute("denoise.models.list", &json!({})).unwrap()["model"], "stand-in");
    drop(x.s);
    let _ = std::fs::remove_dir_all(x.dir);
}

/// A Bayer DNG that is a smooth ramp, big enough for a 64-cell tile and a second one.
fn ramp_dng(w: usize, h: usize, cfa: &str) -> Vec<u8> {
    let data: Vec<u16> = (0..w * h).map(|i| 600 + ((i % w) * 30 + (i / w) * 35) as u16).collect();
    synthetic_dng_of(w, h, cfa, data, None, Default::default())
}

struct Setup {
    dir: PathBuf,
    s: Session,
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

fn photo(s: &Session) -> lightcraft_catalog::PhotoId {
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

fn mean(img: &lightcraft_raster::Rgba8) -> f64 {
    let sum: u64 = img.data.iter().map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2])).sum();
    sum as f64 / (img.data.len() * 3) as f64
}

fn rendered_mean(s: &mut Session, id: lightcraft_catalog::PhotoId) -> f64 {
    mean(&s.render_now(id, 160, 160).unwrap().image)
}

fn set_amount(s: &mut Session, amount: f64) {
    s.execute("develop.set", &json!({"control": "enhance.denoise", "value": amount})).unwrap();
}

#[test]
fn a_model_is_installed_chosen_and_removed_with_the_terms_accepted() {
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
    // it can be checked again, switched off and on, and removed
    assert_eq!(x.s.execute("denoise.models.test", &json!({"id": "stand-in"})).unwrap()["ok"], true);
    assert_eq!(x.s.execute("denoise.models.select", &json!({"id": null})).unwrap()["model"], Value::Null);
    assert!(x.s.denoise.active.is_none());
    assert!(x.s.execute("denoise.models.select", &json!({"id": "nothing"})).is_err());
    x.s.execute("denoise.models.select", &json!({"id": "stand-in"})).unwrap();
    assert!(x.s.denoise.active.is_some());
    let r = x.s.execute("denoise.models.remove", &json!({"id": "stand-in"})).unwrap();
    assert_eq!(r["model"], Value::Null);
    assert!(x.s.denoise.active.is_none());
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
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn a_manifest_that_does_not_validate_or_name_the_file_is_refused() {
    let mut x = setup("manifest", false);
    let onnx = model_files(&x.dir, "stand-in");
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
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn an_export_makes_its_picture_when_the_queue_has_not_and_never_comes_out_without_it() {
    let mut x = setup("export", true);
    let id = photo(&x.s);
    let o = ExportOptions::from_json(&json!({"format": "png", "width": 160, "height": 160}));
    let decode = |bytes: &[u8]| {
        let d = lightcraft_codecs::decode(bytes, lightcraft_codecs::DecodeOptions::fit(160, 160)).unwrap();
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

    // no model: the export says so instead of leaving the denoise out
    x.s.execute("denoise.models.select", &json!({"id": null})).unwrap();
    let e = export_photo(&mut x.s, id, &o, 1).err().unwrap();
    assert!(e.contains("no denoise model is chosen") && e.contains("ramp.dng"), "{e}");
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
    assert!(lightcraft_denoise::product::is_current(&spec.product, &spec.key), "header/length still look current");
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
    assert!(e.contains("no denoise model is chosen"), "{e}");
    // no models folder at all (the web build): the commands say so, nothing panics
    let mut web = Session::with_demo();
    for cmd in ["denoise.models.install", "denoise.models.select", "denoise.models.remove", "denoise.settings"] {
        assert!(web.execute(cmd, &json!({"path": "x", "acknowledged": true, "id": "x"})).is_err(), "{cmd}");
    }
    assert_eq!(web.execute("denoise.pump", &json!({})).unwrap()["active"], false);
    assert_eq!(web.execute("denoise.status", &json!({})).unwrap()["enabled"], false);
    // no library open: nowhere to keep pictures, and the queue says so
    let x = setup("eligible2", true);
    let mut bare = Session::new().with_fs();
    bare.set_denoise_models_dir(Some(x.dir.join("denoise-models")));
    assert!(bare.denoise_products_dir().is_none());
    let e = bare.execute("denoise.queue", &json!({})).unwrap_err().to_string();
    assert!(e.contains("open a library on disk first"), "{e}");
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
        fn run(&self, _: &[f32]) -> Result<Vec<f32>, lightcraft_denoise::Error> {
            Ok(vec![0.0; 7])
        }
    }
    impl Model for Wrong {
        fn runner(&self, _: RunOn, threads: usize) -> (&dyn TileRunner, usize) {
            (self, threads)
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
    assert!(lightcraft_denoise::product::is_current(&spec.product, "k"));
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
    // choosing where it runs lets the card be tried again after a set-up that closed LightCraft
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
    pump_until_ready(&mut x.s, 1);
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn a_photo_whose_file_changed_has_no_picture_until_it_is_made_again() {
    let mut x = setup("changed", true);
    let id = photo(&x.s);
    set_amount(&mut x.s, 100.0);
    pump_until_ready(&mut x.s, 1);
    // the photo's content is something else now (a replaced file): its old picture is not its picture
    let p = x.s.catalog.photo(id).unwrap().clone();
    x.s.commit(
        "test",
        lightcraft_catalog::Op::SetContent {
            id,
            width: p.width,
            height: p.height,
            file_size: p.file_size + 1,
            content_hash: Some("ffffffffffffffffffffffffffffffff".into()),
            preview_only: None,
        },
    )
    .unwrap();
    x.s.denoise_pump(crate::denoise::Pace::Pause);
    assert_ne!(x.s.denoise_photo_state(id), PhotoState::Ready);
    // the old picture is not used by a render either
    assert_eq!(x.s.media.denoise_salt(id, &x.s.catalog.photo(id).unwrap().develop), 0);
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn a_photo_waits_for_its_turn_unless_pictures_are_not_made_on_their_own_or_have_nowhere_to_go() {
    let mut x = setup("waits", true);
    let id = photo(&x.s);
    set_amount(&mut x.s, 80.0);
    // automatic off: the open photo is left alone until it is asked for
    x.s.execute("denoise.settings", &json!({"auto": false})).unwrap();
    assert!(!x.s.denoise_auto());
    x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Idle);
    assert!(!x.s.denoise_busy());
    x.s.execute("denoise.queue", &json!({"ids": [id.0]})).unwrap();
    assert!(x.s.denoise_busy(), "queued with a model: the headless runs wait for it");
    pump_until_ready(&mut x.s, 1);
    assert_eq!(x.s.denoise_photo_state(id), PhotoState::Ready);
    assert!(!x.s.denoise_busy());
    // a session whose library is not on disk has nowhere to keep it: the photo says so instead of waiting for ever
    x.s.denoise.settings.auto = Some(true);
    if let Some(l) = x.s.library.as_mut() {
        l.on_disk = false;
    }
    match x.s.denoise_photo_state(id) {
        PhotoState::Failed { why, unsupported } => assert!(unsupported && why.contains("library folder"), "{why}"),
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
    let mut rgb = lightcraft_raster::Rgb32f::new(192, 160);
    rgb.data.fill([0.25, 0.25, 0.25]);
    lightcraft_denoise::product::write(&product, &rgb, &key).unwrap();
    assert!(lightcraft_denoise::product::is_current(&product, &key));
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
    assert!(x.s.execute("denoise.toggle", &json!({"id": id.0, "enabled": true})).is_err());
    assert_eq!(x.s.develop_of(id).unwrap().enhance.denoise, 0.0);
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
    assert!((x.s.develop_of(id).unwrap().denoise_amount() - 0.8).abs() < 1e-6);
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
            assert_eq!(rows.len(), 1, "{r}");
            assert_eq!((rows[0].label.as_str(), rows[0].cancellable, rows[0].unit), ("AI Denoise", false, crate::activity::Unit::Percent), "{r}");
            if !r["running"].is_null() {
                assert_eq!(rows[0].detail, "ramp.dng", "{r}");
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
    let _ = std::fs::remove_dir_all(&x.dir);
}
