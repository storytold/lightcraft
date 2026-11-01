//! Load and run the MediaPipe TFLite models on the pure-Rust interpreter and time them.
//!
//! `cargo run -p lightcraft-segment --release --example tflite_bench -- <dir> [runs] [--profile]`, where
//! `<dir>` holds `selfie_multiclass_256x256.tflite` and `face_detector.tflite` +
//! `face_landmarks_detector.tflite` or `face_landmarker.task`. Inputs are a smooth synthetic image.

use std::path::Path;
use std::time::{Duration, Instant};

use lightcraft_segment::tflite::{Model, zip};

fn bytes(dir: &Path, name: &str) -> Option<Vec<u8>> {
    std::fs::read(dir.join(name)).ok().or_else(|| zip::entry(&std::fs::read(dir.join("face_landmarker.task")).ok()?, name).map(<[u8]>::to_vec))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(dir) = args.get(1) else {
        eprintln!("usage: tflite_bench <dir> [runs] [--profile]");
        return;
    };
    let runs: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50);
    let profile = args.iter().any(|a| a == "--profile");
    for name in ["selfie_multiclass_256x256.tflite", "face_detector.tflite", "face_landmarks_detector.tflite"] {
        let Some(b) = bytes(Path::new(dir), name) else {
            eprintln!("{name}: not found");
            continue;
        };
        let t = Instant::now();
        let model = match Model::from_bytes(&b) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("{name}: {e}");
                continue;
            }
        };
        let load = t.elapsed();
        let shape = &model.input_shapes()[0];
        let n: usize = shape.iter().product();
        let x: Vec<f32> = (0..n).map(|i| (i as f32 * 0.001).sin() * 0.8).collect();
        let t = Instant::now();
        model.run(&[&x]).unwrap();
        let first = t.elapsed();
        let mut ts: Vec<Duration> = (0..runs)
            .map(|_| {
                let t = Instant::now();
                model.run(&[&x]).unwrap();
                t.elapsed()
            })
            .collect();
        ts.sort();
        let p = |q: f64| ts[((ts.len() - 1) as f64 * q).round() as usize];
        println!(
            "{name}: load {:.1} ms, first run {:.1} ms (cold total {:.1} ms), warm median {:.2} ms, p95 {:.2} ms",
            load.as_secs_f64() * 1e3,
            first.as_secs_f64() * 1e3,
            (load + first).as_secs_f64() * 1e3,
            p(0.5).as_secs_f64() * 1e3,
            p(0.95).as_secs_f64() * 1e3
        );
        if profile {
            for (k, d) in model.profile(&[&x]).unwrap() {
                println!("    {k:>16}: {:.2} ms", d.as_secs_f64() * 1e3);
            }
        }
    }
}
