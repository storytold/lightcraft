//! Cost of the stages the GPU renderer left to the CPU (plan item 4.6): Even Out / Fill spots
//! (`spots.rs`; Fill still runs the scene-linear stage on the CPU, Even Out has run on the GPU since 2026-11-24) and soft proofs against an
//! output-device profile (`finish.rs`, a 3D table: the GPU path declines the render). Measured on a loupe-sized
//! frame (1920×1280) against the 60 ms loupe budget, median of several runs:
//! `cargo test -p lcc-pipeline --release cpu_only_stage_cost -- --ignored --nocapture`. Numbers: `docs/perf.md`.

use lcc_develop::{DevelopSettings, Spot, SpotMode};
use lcc_geom::Point;
use lcc_raster::Rgb32f;

use crate::geometry::Frame;
use crate::{Proof, ProofLut, RenderRequest, SourceInfo, register_proof_profile, render, spots};

const W: usize = 1920;
const H: usize = 1280;
const RUNS: usize = 7;

fn source() -> Rgb32f {
    Rgb32f::from_fn(W, H, |x, y| {
        let t = ((x * 7 + y * 13) % 97) as f32 / 96.0;
        [0.05 + 0.6 * x as f32 / W as f32, 0.1 + 0.5 * t, 0.7 - 0.5 * y as f32 / H as f32]
    })
}

fn median_ms(mut f: impl FnMut()) -> f64 {
    f();
    let mut v: Vec<f64> = (0..RUNS)
        .map(|_| {
            let t = std::time::Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    v.sort_by(f64::total_cmp);
    v[RUNS / 2]
}

fn spot(mode: SpotMode, at: (f64, f64), size: f64) -> Spot {
    Spot { mode, points: vec![Point::new(at.0, at.1)], size, feather: 50.0, ..Default::default() }
}

#[test]
#[ignore = "timing: cargo test -p lcc-pipeline --release cpu_only_stage_cost -- --ignored --nocapture"]
fn cpu_only_stage_cost_against_the_loupe_budget() {
    let src = source();
    let info = SourceInfo::default();
    let frame = Frame::new(W, H, &DevelopSettings::default(), true);
    let ppl = frame.px_per_long(W);
    // the spot stage alone
    for (name, spots_) in [
        ("1 Even Out spot, size 0.05", vec![spot(SpotMode::Flat, (0.5, 0.5), 0.05)]),
        ("1 Even Out spot, size 0.20", vec![spot(SpotMode::Flat, (0.5, 0.5), 0.20)]),
        ("10 Even Out spots, size 0.05", (0..10).map(|i| spot(SpotMode::Flat, (0.08 + 0.09 * f64::from(i), 0.5), 0.05)).collect()),
        ("1 Fill spot, size 0.05", vec![spot(SpotMode::Fill, (0.5, 0.5), 0.05)]),
        ("1 Fill spot, size 0.20", vec![spot(SpotMode::Fill, (0.5, 0.5), 0.20)]),
    ] {
        let ms = median_ms(|| {
            let mut img = src.clone();
            spots::apply(&mut img, &spots_, &frame, ppl);
            std::hint::black_box(&img);
        });
        let copy = median_ms(|| {
            std::hint::black_box(src.clone());
        });
        eprintln!("spot stage  {name:<32} {:>7.2} ms (minus the copy {:.2} ms)", ms - copy, copy);
    }
    // whole CPU render, with and without a device proof
    let req = RenderRequest::fit(W, H);
    let s = DevelopSettings::default();
    let plain = median_ms(|| {
        std::hint::black_box(render(&src, &info, &s, &req));
    });
    let n = 33;
    let data: Vec<[f32; 4]> = (0..n * n * n)
        .map(|i| {
            let (r, g, b) = (i / (n * n), i / n % n, i % n);
            let f = |v: usize| (v as f32 / (n - 1) as f32).powf(2.2) * 0.95;
            [f(r), f(g), f(b), 0.0]
        })
        .collect();
    let id = register_proof_profile(0x4_6, ProofLut::new(n, data).unwrap());
    let proofed =
        |warn: bool| RenderRequest { proof: Some(Proof { profile: id, dest_warning: warn, display_warning: warn, ..Default::default() }), ..req };
    let proof = median_ms(|| {
        std::hint::black_box(render(&src, &info, &s, &proofed(false)));
    });
    let proof_warn = median_ms(|| {
        std::hint::black_box(render(&src, &info, &s, &proofed(true)));
    });
    let with_flat = DevelopSettings { spots: vec![spot(SpotMode::Flat, (0.5, 0.5), 0.05)], ..s.clone() };
    let flat = median_ms(|| {
        std::hint::black_box(render(&src, &info, &with_flat, &req));
    });
    eprintln!("CPU render  plain                            {plain:>7.2} ms");
    eprintln!("CPU render  device proof (33³ table)         {proof:>7.2} ms (+{:.2})", proof - plain);
    eprintln!("CPU render  device proof + both warnings     {proof_warn:>7.2} ms (+{:.2})", proof_warn - plain);
    eprintln!("CPU render  1 Even Out spot                  {flat:>7.2} ms (+{:.2})", flat - plain);
}
