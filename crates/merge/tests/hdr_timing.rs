//! Release timing of the same generated brackets as `hdr_heap`, using the standard allocator.
#![forbid(unsafe_code)]

use std::sync::Mutex;
use std::time::Instant;

use lightcraft_merge::{Deghost, HdrOptions, merge_hdr, no_progress};

#[path = "support/hdr_fixture.rs"]
mod fixture;

fn setting(name: &str, default: usize) -> usize {
    std::env::var(name).map(|value| value.parse().expect("integer timing parameter")).unwrap_or(default)
}

fn digest(values: impl Iterator<Item = u32>) -> u64 {
    values.fold(0xcbf29ce484222325, |hash, bits| (hash ^ u64::from(bits)).wrapping_mul(0x100000001b3))
}

#[test]
#[ignore = "explicit generated 24 MP release benchmark; see docs/hdr-memory.md"]
fn hdr_release_wall_time() {
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    assert_eq!(profile, "release", "build with --release");
    let (w, h, n) =
        (setting("LIGHTCRAFT_HDR_TIMING_WIDTH", 6000), setting("LIGHTCRAFT_HDR_TIMING_HEIGHT", 4000), setting("LIGHTCRAFT_HDR_TIMING_FRAMES", 3));
    let pixels = w.checked_mul(h).expect("timing dimensions fit usize");
    assert!(w >= 96 && h >= 64 && pixels <= 70_000_000);
    assert!((3..=7).contains(&n));
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    let scene = fixture::scene();
    let options = HdrOptions { align: true, deghost: Deghost::High };
    pool.install(|| drop(merge_hdr(fixture::brackets(&scene, 96, 64, n, |_| {}).collect(), &options, &no_progress).unwrap()));
    let frames = pool.install(|| fixture::brackets(&scene, w, h, n, |_| {}).collect::<Vec<_>>());
    let input_digest = digest(frames.iter().flat_map(|frame| frame.image.data.iter().step_by(997).flat_map(|pixel| pixel.map(f32::to_bits))));
    let phases = Mutex::new(Vec::with_capacity(16));
    let started = Instant::now();
    let result = pool
        .install(|| {
            merge_hdr(frames, &options, &|fraction, label| {
                if fraction >= 0.02 {
                    phases.lock().unwrap().push((fraction, label.to_owned(), started.elapsed().as_secs_f64()));
                }
                true
            })
        })
        .unwrap();
    let elapsed = started.elapsed().as_secs_f64();
    assert_eq!((result.radiance.width, result.radiance.height), (w, h));
    assert_eq!(result.alignments.iter().filter(|alignment| alignment.model != "identity").count(), n - 1);
    assert!(result.ghost.data.iter().any(|value| *value > 0.5));
    assert!(result.radiance.data.iter().flatten().all(|value| value.is_finite()));
    let output_digest =
        digest(result.radiance.data.iter().flat_map(|pixel| pixel.map(f32::to_bits)).chain(result.ghost.data.iter().map(|v| v.to_bits())));
    eprintln!(
        "HDR_TIME profile={profile} width={w} height={h} frames={n} threads=4 merge_seconds={elapsed:.9} input_digest={input_digest:016x} output_digest={output_digest:016x}"
    );
    for (fraction, label, seconds) in phases.into_inner().unwrap() {
        eprintln!("HDR_TIME_STAGE fraction={fraction:.2} seconds={seconds:.9} label={label:?}");
    }
}
