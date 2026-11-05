//! Peak live heap bytes of the real HDR merge on generated brackets, with no image files.
//! Kept in its own test binary because dhat's allocator and profiler are process-global.
//! The default is small enough for CI. See `docs/hdr-memory.md` for the 24 MP measurement.
#![forbid(unsafe_code)]

use std::sync::Mutex;
use std::time::Instant;

use lightcraft_merge::{Deghost, Frame, HdrOptions, merge_hdr, no_progress};
use lightcraft_raster::Rgb32f;
use rayon::prelude::*;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const SCENE_W: usize = 1200;
const SCENE_H: usize = 800;
const MARGIN: usize = 16;

/// A fixed-size procedural scene keeps fixture memory independent of the measured image size.
/// It is built before profiling, so neither it nor its median scratch counts as merge memory.
fn scene() -> Rgb32f {
    let scene = lightcraft_scenes::demo_library().into_iter().find(|s| s.kind == lightcraft_scenes::Kind::OceanSunset).unwrap();
    let mut image = scene.render(SCENE_W + 2 * MARGIN, SCENE_H + 2 * MARGIN);
    let mut green: Vec<f32> = image.data.iter().map(|p| p[1]).collect();
    green.sort_by(f32::total_cmp);
    let scale = 0.18 / green[green.len() / 2].max(1e-6);
    image.map_in_place(|p| p.map(|v| v * scale));
    image
}

/// Quantised, clipped exposures with handheld translations and a moving dark disk. Each frame
/// is made directly at its final size, without a second full-size truth or temporary frame.
fn brackets(scene: &Rgb32f, w: usize, h: usize, n: usize) -> Vec<Frame> {
    (0..n)
        .map(|i| {
            let offset = i as f32 - (n / 2) as f32;
            let ev = 2.0 * offset;
            let gain = 2f32.powf(ev);
            let mut image = Rgb32f::new(w, h);
            image.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                for (x, p) in row.iter_mut().enumerate() {
                    let sx = (x as f32 + 0.5) * SCENE_W as f32 / w as f32 - offset * 3.25;
                    let sy = (y as f32 + 0.5) * SCENE_H as f32 / h as f32 + offset * 1.75;
                    let mut value = scene.sample_bilinear(sx + MARGIN as f32, sy + MARGIN as f32);
                    if (sx - (600.0 + offset * 90.0)).powi(2) + (sy - 500.0).powi(2) < 30.0f32.powi(2) {
                        value = [0.01, 0.012, 0.02];
                    }
                    *p = value.map(|v| ((v * gain).clamp(0.0, 1.0) * 16383.0).round() / 16383.0);
                }
            });
            let mut frame = Frame::from_linear_rec2020(image, Some(f64::from(ev)));
            frame.raw = true;
            frame.clip = 1.0;
            frame
        })
        .collect()
}

fn setting(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(value) => value.parse().unwrap_or_else(|_| panic!("{name} must be a positive integer")),
        Err(std::env::VarError::NotPresent) => default,
        Err(e) => panic!("{name}: {e}"),
    }
}

#[test]
fn hdr_peak_live_bytes() {
    let (w, h, n) = (setting("LIGHTCRAFT_HDR_HEAP_WIDTH", 480), setting("LIGHTCRAFT_HDR_HEAP_HEIGHT", 320), setting("LIGHTCRAFT_HDR_HEAP_FRAMES", 3));
    let pixels = w.checked_mul(h).expect("pixel count fits usize");
    assert!(w >= 96 && h >= 64 && pixels <= 70_000_000, "use dimensions at least 96x64 and at most 70 MP");
    assert!((3..=5).contains(&n), "use 3 to 5 frames");
    let input_bytes = pixels.checked_mul(n).and_then(|v| v.checked_mul(size_of::<[f32; 3]>())).expect("input bytes fit usize");
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    let scene = scene();
    let options = HdrOptions { align: true, deghost: Deghost::High };
    // Build the pool, caches and thread locals before counting. The full brackets themselves
    // MUST be allocated after the profiler starts, or dhat would miss the retained inputs.
    pool.install(|| drop(merge_hdr(brackets(&scene, 96, 64, n), &options, &no_progress).unwrap()));
    let phases = Mutex::new(Vec::with_capacity(16));
    let profiler = dhat::Profiler::builder().testing().build();
    let started = Instant::now();
    let result = pool.install(|| {
        let frames = brackets(&scene, w, h, n);
        let stats = dhat::HeapStats::get();
        assert!(stats.curr_bytes >= input_bytes, "live heap {} missed the {input_bytes} input bytes", stats.curr_bytes);
        phases.lock().unwrap().push(("Inputs ready".to_owned(), stats.curr_bytes, stats.max_bytes));
        let progress = |_: f32, stage: &str| {
            let stats = dhat::HeapStats::get();
            phases.lock().unwrap().push((stage.to_owned(), stats.curr_bytes, stats.max_bytes));
            true
        };
        merge_hdr(frames, &options, &progress).unwrap()
    });
    let elapsed = started.elapsed();
    let stats = dhat::HeapStats::get();
    // Read stats before printing or allocating validation scratch. max_bytes includes every
    // transient allocation, including ones between progress calls, on all four worker threads.
    let (peak, returned) = (stats.max_bytes, stats.curr_bytes);
    drop(profiler);
    assert_eq!((result.radiance.width, result.radiance.height), (w, h));
    assert_eq!(result.ghost.data.len(), pixels);
    assert!(result.radiance.data.iter().flatten().all(|v| v.is_finite()));
    let warped = result.alignments.iter().filter(|a| a.model != "identity").count();
    assert_eq!(warped, n - 1, "the fixture must exercise Lanczos-3 resampling of every non-reference frame");
    assert!(result.ghost.data.iter().any(|v| *v > 0.5), "the fixture must exercise deghosting");
    // The input snapshot above proves the retained frames were counted; this also checks the peak.
    assert!(peak >= input_bytes, "peak {peak} missed the {input_bytes} input bytes");
    eprintln!(
        "HDR_HEAP width={w} height={h} pixels={pixels} frames={n} threads=4 input_rgb_bytes={input_bytes} peak_live_bytes={peak} returned_live_bytes={returned} peak_bytes_per_pixel={:.6} peak_bytes_per_pixel_per_frame={:.6} warped_frames={warped} seconds={:.3}",
        peak as f64 / pixels as f64,
        peak as f64 / pixels as f64 / n as f64,
        elapsed.as_secs_f64(),
    );
    for (stage, live, peak_so_far) in phases.into_inner().unwrap() {
        eprintln!("HDR_HEAP_STAGE stage={stage:?} live_bytes={live} peak_so_far_bytes={peak_so_far}");
    }
}
