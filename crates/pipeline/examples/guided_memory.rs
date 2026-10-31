//! Isolated fast-guided-filter benchmark (procedural log luminance; no decoded RGB or tone median).
//! `RAYON_NUM_THREADS=1 cargo run --release -p lightcraft-pipeline --example guided_memory -- new`
//! Replace `new` with `old` for the materialized-coefficient reference. Optional positional arguments:
//! width height sigma iterations (defaults: 6000 4000 120 5). Append `--heap` for one dhat-counted run.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use lightcraft_pipeline::local::{BASE_EPS, guided, guided_fast, guided_fast_step};
use lightcraft_raster::blur::gaussian;
use lightcraft_raster::resample::{Filter, resize};
use lightcraft_raster::{Plane, par_join, par_rows};
use std::hint::black_box;
use std::time::Instant;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Previous algorithm; kept here only to make before/after measurements reproducible.
fn materialized(p: &Plane, sigma: f32) -> Plane {
    let s = guided_fast_step(sigma);
    if s <= 1 {
        return guided(p, sigma, BASE_EPS);
    }
    let lo = resize(p, p.width.div_ceil(s).max(1), p.height.div_ceil(s).max(1), Filter::Box);
    let (a, b) = coefficients(&lo, sigma / s as f32);
    let (a, b) = (resize(&a, p.width, p.height, Filter::Bilinear), resize(&b, p.width, p.height, Filter::Bilinear));
    let mut q = Plane::new(p.width, p.height);
    par_rows(&mut q.data, p.width, |y, row| {
        for ((v, pv), (av, bv)) in row.iter_mut().zip(p.row(y)).zip(a.row(y).iter().zip(b.row(y))) {
            *v = av * pv + bv;
        }
    });
    q
}

fn coefficients(lo: &Plane, sg: f32) -> (Plane, Plane) {
    let (mean, corr) = par_join(|| gaussian(lo, sg), || gaussian(&lo.map(|v| v * v), sg));
    let a = corr.zip_map(&mean, |c, m| {
        let v = (c - m * m).max(0.0);
        v / (v + BASE_EPS)
    });
    let b = mean.zip_map(&a, |m, a| m - a * m);
    par_join(|| gaussian(&a, sg), || gaussian(&b, sg))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("new");
    if !matches!(mode, "old" | "new") {
        return Err("mode must be old or new".into());
    }
    let parse = |i: usize, default: usize| -> Result<usize, Box<dyn std::error::Error>> {
        Ok(args.get(i).filter(|v| !v.starts_with("--")).map(|v| v.parse()).transpose()?.unwrap_or(default))
    };
    let (w, h, sigma, n) = (parse(1, 6000)?, parse(2, 4000)?, parse(3, 120)? as f32, parse(4, 5)?.max(1));
    if w == 0 || h == 0 || w.checked_mul(h).is_none_or(|pixels| pixels > 100_000_000) {
        return Err("dimensions must be positive and at most 100 MP".into());
    }
    let heap = args.iter().any(|a| a == "--heap");
    let p = Plane::from_fn(w, h, |x, y| ((x * 7919 + y * 104729) % 97) as f32 / 97.0 - 4.0 + if x > w / 2 { 2.0 } else { 0.0 });
    let filter = || if mode == "old" { materialized(&p, sigma) } else { guided_fast(&p, sigma, BASE_EPS) };
    // Initialize Rayon and warm code paths before counting. The source is excluded from heap stats.
    black_box(filter());
    let profiler = heap.then(|| dhat::Profiler::builder().testing().build());
    let mut best = f64::MAX;
    for _ in 0..if heap { 1 } else { n } {
        let t = Instant::now();
        let q = black_box(filter());
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        if heap {
            let stats = dhat::HeapStats::get();
            println!(
                "peak_bytes={} total_bytes={} total_blocks={} retained_output_bytes={}",
                stats.max_bytes,
                stats.total_bytes,
                stats.total_blocks,
                q.data.len() * size_of::<f32>()
            );
        }
    }
    drop(profiler);
    println!("{mode} {w}x{h} sigma={sigma} step={} heap={heap} min_wall_ms={best:.3}", guided_fast_step(sigma));
    Ok(())
}
