//! Opt-in same-machine denoise model-stage benchmark on one public Bayer raw.
//! LC_DENOISE_MODEL=<onnx> LC_DENOISE_RAW=<CC0 corpus/raw> cargo test --release
//! -p dac-engine --features denoise --test denoise_cpu -- --ignored --nocapture
//! LC_DENOISE_ONLY selects a file (default arw-sony-a7m3-compressed.arw).
//! The whole model stage includes packing, scale/clip protection, blending and normalization;
//! file decode, model loading and GPU setup are outside the timer. It uses the application's tile pool convention.
#![cfg(feature = "denoise")]

use dac_denoise::{
    TileRunner,
    bayer::Layout,
    run::{Control, Params, denoise_bayer},
    runtime::{CpuRunner, check_tile},
};
use dac_raster::Rgb32f;
use std::{path::Path, time::Instant};

fn agree(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    assert!(a.iter().chain(b).all(|v| v.is_finite()));
    let scale = b.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
    let error = a.iter().zip(b).fold(0f32, |m, (a, b)| m.max((a - b).abs())) / scale;
    assert!(error <= 1e-3, "maximum relative error {error:e}");
    error
}
fn tile_time(name: &str, r: &dyn TileRunner, x: &[f32]) {
    r.run(x).unwrap();
    let mut times: Vec<_> = (0..5)
        .map(|_| {
            let t = Instant::now();
            r.run(x).unwrap();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    println!("{name}, one 512-cell tile, one thread: {:.1} ms median ({:.1}..{:.1})", times[2], times[0], times[4]);
}
fn photo_time(name: &str, r: &dyn TileRunner, raw: &dac_raw::Normalized, layout: Layout, p: &Params) -> Rgb32f {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(p.parallel).build().unwrap();
    let run = || pool.install(|| denoise_bayer(&raw.data, raw.width, raw.height, layout, r, p, &Control::default())).unwrap();
    drop(run()); // warm the workers and their workspace cache
    let mut times = Vec::new();
    let mut answer = Rgb32f { width: 0, height: 0, data: Vec::new() };
    for i in 0..3 {
        let t = Instant::now();
        answer = run();
        let secs = t.elapsed().as_secs_f64();
        times.push(secs);
        println!("{name}, whole model stage, {} workers, run {i}: {secs:.3} s", p.parallel);
    }
    times.sort_by(f64::total_cmp);
    println!("{name}, whole model stage median {:.3} s", times[1]);
    answer
}
#[test]
#[ignore = "needs LC_DENOISE_MODEL and LC_DENOISE_RAW; release timing and real-model accuracy"]
fn denoise_stage_compares_cpu_and_gpu() {
    let model = std::env::var_os("LC_DENOISE_MODEL").expect("set LC_DENOISE_MODEL");
    let folder = std::env::var_os("LC_DENOISE_RAW").expect("set LC_DENOISE_RAW to public CC0 corpus/raw");
    let name = std::env::var("LC_DENOISE_ONLY").unwrap_or_else(|_| "arw-sony-a7m3-compressed.arw".into());
    let bytes = std::fs::read(Path::new(&folder).join(&name)).unwrap();
    let raw = dac_raw::decode(&bytes).unwrap().normalized().unwrap();
    let cfa = raw.cfa.as_ref().unwrap();
    assert!(cfa.is_bayer());
    let layout = Layout::from_cell([cfa.color_at(0, 0), cfa.color_at(1, 0), cfa.color_at(0, 1), cfa.color_at(1, 1)]).unwrap();
    let m = dac_denoise::known::find("rawnind-bayer").unwrap().manifest;
    let cpu = CpuRunner::load(Path::new(&model), &m).unwrap();
    let threads = std::env::var("LC_DENOISE_PARALLEL")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 16));
    let p = Params { tile: 512, overlap: 64, gain: m.gain, clip: Some(1.0), parallel: threads };
    println!(
        "{name}: {}x{}, {:.2} MP, {} tiles, {} CPU workers, {:.1} MB/workspace, {:.1} GMAC/tile",
        raw.width,
        raw.height,
        raw.width as f64 * raw.height as f64 / 1e6,
        dac_denoise::run::tile_count(raw.width, raw.height, layout, 512, 64),
        threads,
        cpu.bytes_per_tile() as f64 / 1e6,
        cpu.net().macs(512) as f64 / 1e9
    );
    let x = check_tile(512);
    let want = cpu.run(&x).unwrap();
    tile_time("pure Rust", &cpu, &x);
    let done = photo_time("pure Rust", &cpu, &raw, layout, &p);

    match dac_gpu::nn::runner(cpu.net(), 512) {
        Ok(gpu) => {
            println!("GPU: {}", gpu.adapter());
            println!("CPU/check tile vs GPU: {:e}", agree(&want, &gpu.run(&x).unwrap()));
            tile_time("GPU", &gpu, &x);
            let gp = Params { parallel: 4, ..p };
            let answer = photo_time("GPU", &gpu, &raw, layout, &gp);
            let a: Vec<_> = done.data.iter().flatten().copied().collect();
            let b: Vec<_> = answer.data.iter().flatten().copied().collect();
            println!("CPU/whole photo vs GPU: {:e}", agree(&a, &b));
        }
        Err(e) => println!("GPU unavailable: {e}"),
    }
}
