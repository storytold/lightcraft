//! Times the SAM 3 backbone on the GPU (wgpu) against the candle backbone on the CPU, on a procedural photo.
//!
//! `cargo run --release -p lightcraft-gpu --features sam --example sam3_backbone_bench -- [--f16] [--runs N] [--no-cpu]`
//!
//! The model directory is `LIGHTCRAFT_SAM3_DIR`, else `~/.config/lightcraft/models/sam3`. Prints the device and
//! kernel creation, the weight upload, the first and the warm forward passes, the CPU forward pass and how far the
//! two outputs are apart. `--app` then times what the app does per photo, with the model loaded as the app loads it
//! (GPU backbone, the rest in candle on the CPU): encode, click features, first click, first and second Describe.

use std::path::PathBuf;
use std::time::Instant;

use lightcraft_gpu::sam3::{self, Precision};
use lightcraft_segment::{BackboneAccel, INPUT, Sam3};

fn model_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os("LIGHTCRAFT_SAM3_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".config/lightcraft/models/sam3"),
    };
    lightcraft_segment::is_model_dir(&dir).then_some(dir)
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let precision = if args.iter().any(|a| a == "--f16") { Precision::F16 } else { Precision::F32 };
    let runs: usize = match args.iter().position(|a| a == "--runs").map(|i| args.get(i + 1).and_then(|v| v.parse().ok())) {
        None => 5,
        Some(Some(n)) if n > 0 => n,
        Some(_) => return Err("--runs needs a positive number".into()),
    };
    let cpu = !args.iter().any(|a| a == "--no-cpu");
    let app = args.iter().any(|a| a == "--app");
    let dir = model_dir().ok_or("no SAM 3 model: set LIGHTCRAFT_SAM3_DIR")?;

    let scene = &lightcraft_scenes::demo_library()[0];
    let img = scene.render_fit(1600).to_srgb8();
    let rgb: Vec<u8> = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let pixels = lightcraft_segment::mask::preprocess(&rgb, img.width, img.height, INPUT).map_err(|e| e.to_string())?;

    let t = Instant::now();
    let adapter = sam3::adapter()?;
    println!("device + kernels: {:7.1} ms  ({adapter})", t.elapsed().as_secs_f64() * 1e3);
    let reader = lightcraft_segment::WeightReader::open(&dir).map_err(|e| e.to_string())?;
    let t = Instant::now();
    let gpu = sam3::load(&reader, precision)?;
    println!(
        "weight upload:    {:7.1} ms  ({:.0} MB weights, {:.0} MB work buffers, {precision:?})",
        t.elapsed().as_secs_f64() * 1e3,
        gpu.weight_bytes() as f64 / 1e6,
        gpu.work_bytes() as f64 / 1e6
    );
    let t = Instant::now();
    let out = gpu.forward(&pixels)?;
    println!("first forward:    {:7.1} ms", t.elapsed().as_secs_f64() * 1e3);
    let mut best = f64::MAX;
    let mut sum = 0.0;
    for _ in 0..runs {
        let t = Instant::now();
        gpu.forward(&pixels)?;
        let ms = t.elapsed().as_secs_f64() * 1e3;
        best = best.min(ms);
        sum += ms;
    }
    println!("warm forward:     {:7.1} ms  (mean of {runs}: {:.1} ms)", best, sum / runs.max(1) as f64);
    if cpu {
        let model = Sam3::load_on(&dir, candle_core::Device::Cpu).map_err(|e| e.to_string())?;
        let px = candle_core::Tensor::from_vec(pixels.clone(), (1, 3, INPUT, INPUT), &candle_core::Device::Cpu).map_err(|e| e.to_string())?;
        let t = Instant::now();
        let enc = model.encode_pixels(&px).map_err(|e| e.to_string())?;
        println!("CPU (candle):     {:7.1} ms", t.elapsed().as_secs_f64() * 1e3);
        let want = enc.backbone().flatten_all().and_then(|t| t.to_vec1::<f32>()).map_err(|e| e.to_string())?;
        let scale = want.iter().fold(0f32, |m, v| m.max(v.abs()));
        let (mut max, mut sum) = (0f32, 0f64);
        for (a, b) in out.iter().zip(&want) {
            max = max.max((a - b).abs());
            sum += f64::from((a - b).abs());
        }
        println!(
            "GPU vs CPU: max abs {max:.3e}, max rel {:.3e} (scale {scale:.2}), mean abs {:.3e}",
            max / scale.max(1e-6),
            sum / want.len().max(1) as f64
        );
    }
    drop(gpu);
    if app {
        as_the_app(&dir, &rgb, (img.width, img.height), runs)?;
    }
    // no GPU work in flight when the process ends (issue #620)
    lightcraft_gpu::begin_shutdown();
    lightcraft_gpu::wait_idle(std::time::Duration::from_secs(5));
    Ok(())
}

fn ms(t: Instant) -> String {
    format!("{:.0} ms", t.elapsed().as_secs_f64() * 1e3)
}

/// The model as the app loads it, timed per photo: encode, click features, a click, two Describes.
fn as_the_app(dir: &std::path::Path, rgb: &[u8], (w, h): (usize, usize), runs: usize) -> Result<(), String> {
    let t = Instant::now();
    let mut model = Sam3::load_accelerated(dir, candle_core::Device::Cpu, sam3::backbone).map_err(|e| e.to_string())?;
    println!("app: load {} on {}", ms(t), model.backbone_device());
    let click = |x, y| lightcraft_segment::Click { x, y, positive: true };
    for run in 0..runs.max(1) {
        let t = Instant::now();
        let mut enc = model.encode(rgb, w, h).map_err(|e| e.to_string())?;
        let encode = ms(t);
        let t = Instant::now();
        model.prepare_clicks(&mut enc).map_err(|e| e.to_string())?;
        let features = ms(t);
        let t = Instant::now();
        model.segment_clicks(&mut enc, &[click(0.5, 0.5)]).map_err(|e| e.to_string())?;
        let click1 = ms(t);
        let t = Instant::now();
        model.segment_text(&mut enc, "sky", 0.5).map_err(|e| e.to_string())?;
        let text1 = ms(t);
        let t = Instant::now();
        model.segment_text(&mut enc, "the red car", 0.5).map_err(|e| e.to_string())?;
        let text2 = ms(t);
        println!("app run {run}: encode {encode}, click features {features}, click {click1}, first describe {text1}, second describe {text2}");
    }
    println!("app: the rest on {}", model.heads_device());
    Ok(())
}
