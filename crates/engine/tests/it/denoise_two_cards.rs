//! AI Denoise batches on one graphics card and on two (dual-GPU PR 5): the time for a batch of photos, and the same
//! pictures either way. Needs an installed model and CC0 raw files, so it is ignored by default:
//!
//! ```text
//! LC_DENOISE_MODEL=<model.onnx> LC_DENOISE_RAW=<folder of raw files> LIGHTCRAFT_GPU_HELPER=off \
//!   cargo test --release -p lightcraft-engine --features denoise --test it denoise_two_cards:: -- --ignored --nocapture
//! ```
//!
//! Run it once with `LIGHTCRAFT_GPU_HELPER=off` (one card) and once with a helper (`LIGHTCRAFT_GPU_ADAPTER=5070
//! LIGHTCRAFT_GPU_HELPER=4070`, or `=same` for two devices on one card). `LC_DENOISE_PHOTOS` is the batch's size
//! (default 8): the raw files are used in turn, each copy made different by bytes after its end (the decoder ignores
//! them, the picture's key changes), so every photo is made.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lightcraft_engine::Session;
use serde_json::json;

#[test]
#[ignore]
fn a_batch_on_one_card_or_two() {
    let (Some(model), Some(raws)) = (std::env::var_os("LC_DENOISE_MODEL"), std::env::var_os("LC_DENOISE_RAW")) else {
        lightcraft_testkit::skip!(Model, "set LC_DENOISE_MODEL and LC_DENOISE_RAW");
        return;
    };
    let n: usize = std::env::var("LC_DENOISE_PHOTOS").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    let dir = std::env::temp_dir().join(format!("lc-denoise-two-cards-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let originals = dir.join("originals");
    std::fs::create_dir_all(&originals).unwrap();
    let mut sources: Vec<PathBuf> = std::fs::read_dir(&raws).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file()).collect();
    sources.sort();
    assert!(!sources.is_empty(), "no raw files in {raws:?}");
    let mut paths = Vec::new();
    // photo 0 sets the cards up (kernels, check tile); the batch timed is the other `n`
    for i in 0..=n {
        let src = &sources[i % sources.len()];
        let mut bytes = std::fs::read(src).unwrap();
        bytes.extend(std::iter::repeat_n(0u8, i + 1));
        let ext = src.extension().map_or("raw".into(), |e| e.to_string_lossy().into_owned());
        let p = originals.join(format!("photo{i:02}.{ext}"));
        std::fs::write(&p, bytes).unwrap();
        paths.push(p.to_string_lossy().into_owned());
    }
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("library"), false).unwrap();
    s.set_denoise_models_dir(Some(dir.join("models")));
    s.execute("denoise.models.install", &json!({"path": model.to_string_lossy(), "acknowledged": true})).unwrap();
    s.execute("denoise.settings", &json!({"runOn": "gpu", "auto": false})).unwrap();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
    let pump_until = |s: &mut Session, ready: usize, most: &mut usize| {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(1800) {
            let r = s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
            let at_once = r["running"]["photos"].as_array().map_or(usize::from(!r["running"].is_null()), Vec::len);
            *most = (*most).max(at_once);
            if r["ready"].as_u64() >= Some(ready as u64) && !s.denoise_busy() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        t.elapsed().as_secs_f64()
    };
    let mut most = 0;
    s.execute("denoise.queue", &json!({"ids": [ids[0]]})).unwrap();
    let setup = pump_until(&mut s, 1, &mut most);
    println!("first photo (with the set-up) {setup:.1} s; device {}", s.execute("denoise.status", &json!({})).unwrap()["device"]);
    s.execute("denoise.queue", &json!({"ids": ids[1..]})).unwrap();
    most = 0;
    let secs = pump_until(&mut s, n + 1, &mut most);
    let status = s.execute("denoise.status", &json!({})).unwrap();
    println!("{n} photos in {secs:.1} s ({:.2} s a photo), at most {most} at once; device {}", secs / n as f64, status["device"]);
    assert_eq!(status["ready"], (n + 1) as u64, "{status}");
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
