//! Peak live heap bytes of the real HDR merge on generated brackets, with no image files.
//! Kept in its own test binary because dhat's allocator and profiler are process-global.
//! The default is small enough for CI. See `docs/hdr-memory.md` for the 24 MP measurement.
#![forbid(unsafe_code)]

use std::sync::Mutex;
use std::time::Instant;

use lightcraft_merge::{Deghost, Frame, HdrOptions, HdrResult, MergeError, Progress, Result, align, features, frame, merge_hdr, no_progress};

#[path = "support/hdr_f32.rs"]
mod baseline;

fn check(p: &Progress, f: f32, stage: &str) -> Result<()> {
    if p(f.clamp(0.0, 1.0), stage) { Ok(()) } else { Err(MergeError::Cancelled) }
}
use lightcraft_raster::Rgb32f;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[path = "support/hdr_fixture.rs"]
mod fixture;

use fixture::scene;

fn brackets(scene: &Rgb32f, w: usize, h: usize, n: usize, probe_storage: bool) -> impl Iterator<Item = Frame> + '_ {
    fixture::brackets(scene, w, h, n, move |i| {
        if probe_storage && i > 0 {
            let live = dhat::HeapStats::get().curr_bytes;
            let retained = i * w * h * size_of::<[f32; 3]>();
            assert!(live <= retained + 65_536, "decoded capacity survived into the next read: {live} > {retained} + metadata");
        }
    })
    .map(move |mut frame| {
        if probe_storage {
            // Model capacity left by a crop; the next read checks that it was released.
            frame.image.data.reserve_exact(w * h / 2);
        }
        frame
    })
}

fn setting(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(value) => value.parse().unwrap_or_else(|_| panic!("{name} must be a positive integer")),
        Err(std::env::VarError::NotPresent) => default,
        Err(e) => panic!("{name}: {e}"),
    }
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Original,
    Exact,
    Stream,
}

impl Mode {
    fn read() -> Self {
        match std::env::var("LIGHTCRAFT_HDR_HEAP_MODE").as_deref() {
            Ok("original") => Self::Original,
            Ok("exact") => Self::Exact,
            Ok("stream") | Err(_) => Self::Stream,
            Ok(other) => panic!("unknown heap mode {other}: use original, exact or stream"),
        }
    }
}

fn original(frames: Vec<Frame>, opts: &HdrOptions, progress: &Progress) -> Result<HdrResult> {
    let deghost = match opts.deghost {
        Deghost::None => baseline::Deghost::None,
        Deghost::Low => baseline::Deghost::Low,
        Deghost::Medium => baseline::Deghost::Medium,
        Deghost::High => baseline::Deghost::High,
    };
    let old = baseline::merge_hdr(frames, &baseline::HdrOptions { align: opts.align, deghost }, progress)?;
    Ok(HdrResult {
        radiance: old.radiance,
        ghost: old.ghost,
        reference: old.reference,
        ev: old.ev,
        ev_exif: old.ev_exif,
        alignments: old.alignments,
        color: old.color,
        raw: old.raw,
        orientation: old.orientation,
        metadata: old.metadata,
        baseline_exposure: old.baseline_exposure,
        lens: old.lens,
        wb_multipliers: old.wb_multipliers,
    })
}

fn merge(frames: impl Iterator<Item = Frame>, mode: Mode, opts: &HdrOptions, progress: &Progress) -> HdrResult {
    match mode {
        Mode::Original => original(frames.collect(), opts, progress),
        Mode::Exact => merge_hdr(frames.collect(), opts, progress),
        Mode::Stream => lightcraft_merge::hdr::merge_hdr_iter(frames.map(Ok), opts, progress),
    }
    .unwrap()
}

fn measure(w: usize, h: usize, n: usize, mode: Mode, options: HdrOptions, threads: usize, copy_check: bool) {
    let pixels = w.checked_mul(h).expect("pixel count fits usize");
    assert!(w >= 96 && h >= 64 && pixels <= 70_000_000, "use dimensions at least 96x64 and at most 70 MP");
    assert!((3..=7).contains(&n), "use 3 to 7 frames");
    let input_bytes = pixels.checked_mul(n).and_then(|v| v.checked_mul(size_of::<[f32; 3]>())).expect("input bytes fit usize");
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
    let scene = scene();
    pool.install(|| drop(merge(brackets(&scene, 96, 64, n, false), mode, &options, &no_progress)));
    let phases = Mutex::new(Vec::with_capacity(16));
    let profiler = dhat::Profiler::builder().testing().build();
    let started = Instant::now();
    // Diagnostic fault injection is deliberately held across the merge's peak. The small CI
    // control case below must reject even one extra full-size RGB input copy.
    let extra_copy = std::env::var_os("LIGHTCRAFT_HDR_HEAP_EXTRA_COPY").map(|_| Rgb32f::new(w, h));
    let probe_storage = !copy_check && w == 480 && h == 320 && matches!(mode, Mode::Stream) && extra_copy.is_none();
    let result = pool.install(|| {
        let progress = |fraction: f32, stage: &str| {
            let stats = dhat::HeapStats::get();
            if fraction >= 0.02 {
                assert!(stats.curr_bytes >= input_bytes, "the profiler missed retained inputs");
            }
            phases.lock().unwrap().push((stage.to_owned(), stats.curr_bytes, stats.max_bytes));
            true
        };
        merge(brackets(&scene, w, h, n, probe_storage), mode, &options, &progress)
    });
    std::hint::black_box(&extra_copy);
    let elapsed = started.elapsed();
    let stats = dhat::HeapStats::get();
    let (peak, returned) = (stats.max_bytes, stats.curr_bytes);
    drop(profiler);
    assert_eq!((result.radiance.width, result.radiance.height), (w, h));
    assert_eq!(result.ghost.data.len(), if options.deghost == Deghost::None { 0 } else { pixels });
    assert!(result.radiance.data.iter().flatten().all(|v| v.is_finite()));
    let warped = result.alignments.iter().filter(|a| a.model != "identity").count();
    if options.align {
        assert_eq!(warped, n - 1, "exercise every non-reference Lanczos warp");
    }
    if options.deghost != Deghost::None {
        assert!(result.ghost.data.iter().any(|v| *v > 0.5), "exercise deghosting");
    }
    assert!(peak >= input_bytes, "peak {peak} missed the {input_bytes} input bytes");
    eprintln!(
        "HDR_HEAP mode={mode:?} width={w} height={h} pixels={pixels} frames={n} threads={threads} copy_check={copy_check} input_rgb_bytes={input_bytes} peak_live_bytes={peak} returned_live_bytes={returned} peak_bytes_per_pixel={:.6} peak_bytes_per_pixel_per_frame={:.6} warped_frames={warped} seconds={:.3}",
        peak as f64 / pixels as f64,
        peak as f64 / pixels as f64 / n as f64,
        elapsed.as_secs_f64(),
    );
    for (stage, live, peak_so_far) in phases.into_inner().unwrap() {
        eprintln!("HDR_HEAP_STAGE stage={stage:?} live_bytes={live} peak_so_far_bytes={peak_so_far}");
    }
    // Fixed generated workloads, with scheduling allowances smaller than one full RGB image
    // in each large case. The control accounts for one output, three rows per frame and 128 KB
    // of metadata/worker allowance. See docs/hdr-memory.md. The old oracle has no saving budget.
    if !matches!(mode, Mode::Original) {
        let budget = if copy_check {
            input_bytes + pixels * 12 + 12 * n * w * threads + 128_000
        } else {
            match (w, h, n) {
                (480, 320, 3) => 40_000_000,
                (6000, 4000, 3) => 1_350_000_000,
                (6000, 4000, 5) => 2_100_000_000,
                (6000, 4000, 7) => 2_850_000_000,
                (10048, 6696, 3) => 3_650_000_000,
                _ => input_bytes + pixels * (n + 24) + 32_000_000,
            }
        };
        eprintln!("HDR_HEAP_BUDGET peak={peak} budget={budget}");
        assert!(peak <= budget, "HDR peak {peak} exceeds {budget} live bytes");
    }
}

#[test]
fn hdr_peak_live_bytes() {
    let (w, h, n) = (setting("LIGHTCRAFT_HDR_HEAP_WIDTH", 480), setting("LIGHTCRAFT_HDR_HEAP_HEIGHT", 320), setting("LIGHTCRAFT_HDR_HEAP_FRAMES", 3));
    let mode = Mode::read();
    let options = HdrOptions { align: true, deghost: Deghost::High };
    measure(w, h, n, mode, options, 4, false);
    if std::env::var_os("LIGHTCRAFT_HDR_HEAP_WIDTH").is_none() {
        measure(480, 320, 3, mode, HdrOptions { align: false, deghost: Deghost::None }, 1, true);
    }
}
