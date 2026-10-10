//! Opt-in end-to-end check of AI denoise with the real RawNIND Bayer model, through the engine: install it, make a real
//! raw photo's picture in the background, render and export it with the Amount slider, and see what a file that is not
//! Bayer says.
//!
//! ```text
//! LC_DENOISE_MODEL=<model_bayer.onnx> LC_DENOISE_RAW=<folder with raw files> \
//!   cargo test --release -p dac-engine --features denoise --test denoise_real -- --ignored --nocapture
//! ```
//!
//! `LC_DENOISE_RAW` is a folder of CC0 raw files (`cargo xtask corpus --download` → `corpus/raw`); every file in it is
//! tried. Optional: `LC_DENOISE_ONLY` (a file name to try alone), `LC_DENOISE_RUN_ON=cpu|gpu|auto` (where the model runs; `cpu` to compare with the card).
#![cfg(feature = "denoise")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dac_engine::Session;
use serde_json::{Value, json};

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-denoise-real-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Mean absolute second difference of the luma: the fine detail and the noise of a picture.
fn roughness(img: &dac_raster::Rgba8) -> f64 {
    let (w, h) = (img.width, img.height);
    let luma = |x: usize, y: usize| {
        let p = img.data[y * w + x];
        0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2])
    };
    let (mut sum, mut n) = (0.0, 0usize);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            sum += (4.0 * luma(x, y) - luma(x - 1, y) - luma(x + 1, y) - luma(x, y - 1) - luma(x, y + 1)).abs();
            n += 1;
        }
    }
    sum / n.max(1) as f64
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let only = std::env::var("LC_DENOISE_ONLY").ok();
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && only.as_deref().is_none_or(|o| p.file_name().is_some_and(|n| n.to_string_lossy() == o)))
        .collect();
    v.sort();
    v
}

#[test]
#[ignore = "needs the real model and raw files (see the file's header)"]
fn the_real_model_denoises_real_raws_through_the_engine() {
    let (Some(model), Some(raws)) = (std::env::var_os("LC_DENOISE_MODEL"), std::env::var_os("LC_DENOISE_RAW")) else {
        panic!("set LC_DENOISE_MODEL and LC_DENOISE_RAW");
    };
    let dir = temp("e2e");
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("library"), false).unwrap();
    s.set_denoise_models_dir(Some(dir.join("models")));
    let t = Instant::now();
    let r = s.execute("denoise.models.install", &json!({"path": model.to_string_lossy(), "acknowledged": true})).unwrap();
    println!("installed {} in {:.1} s: self-test {}", r["installed"]["id"], t.elapsed().as_secs_f64(), r["installed"]["accepted"]["selfTest"]);
    assert_eq!(r["installed"]["id"], "rawnind-bayer");
    if let Ok(run_on) = std::env::var("LC_DENOISE_RUN_ON") {
        s.execute("denoise.settings", &json!({"runOn": run_on})).unwrap();
    }

    for file in files(Path::new(&raws)) {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let imported = s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
        let Some(id) = imported["imported"].as_array().and_then(|a| a.first()).and_then(Value::as_u64) else {
            println!("{name}: not imported ({imported})");
            continue;
        };
        s.execute("library.select", &json!({"ids": [id]})).unwrap();
        s.execute("develop.set", &json!({"control": "enhance.denoise", "value": 100})).unwrap();
        let photo = dac_engine::catalog::PhotoId(id);
        let started = Instant::now();
        let mut state = Value::Null;
        while started.elapsed() < Duration::from_secs(600) {
            s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
            state = s.execute("denoise.status", &json!({"id": id})).unwrap()["photo"].clone();
            if matches!(state["state"].as_str(), Some("ready" | "failed")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        println!("{name}: {} in {:.1} s", state, started.elapsed().as_secs_f64());
        if state["state"] == "failed" {
            // a file that is not a Bayer raw says why, and its export is the plain one
            assert_eq!(state["unsupported"], true, "{name}: {state}");
            continue;
        }
        assert_eq!(state["state"], "ready", "{name}: {state}");
        let denoised = s.render_now(photo, 1600, 1600).unwrap().image;
        s.execute("develop.set", &json!({"control": "enhance.denoise", "value": 0})).unwrap();
        let plain = s.render_now(photo, 1600, 1600).unwrap().image;
        let (a, b) = (roughness(&plain), roughness(&denoised));
        println!("{name}: roughness at 1600 px: plain {a:.3}, denoised {b:.3} ({:+.1} %)", 100.0 * (b - a) / a);
        assert!(b < a, "{name}: the denoised picture is not smoother");
        // the export is made from the same picture, without asking the queue
        s.execute("develop.set", &json!({"control": "enhance.denoise", "value": 100})).unwrap();
        let o = dac_engine::export::ExportOptions::from_json(&json!({"format": "jpeg", "width": 1200, "height": 1200}));
        let e = dac_engine::export::export_photo(&mut s, photo, &o, 1).unwrap();
        assert!(e.bytes.len() > 1000);
    }
    let status = s.execute("denoise.status", &json!({})).unwrap();
    println!("device: {}", status["device"]);
    println!("cache: {}", status["cache"]);
    let _ = std::fs::remove_dir_all(&dir);
}
