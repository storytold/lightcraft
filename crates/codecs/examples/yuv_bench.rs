//! Times `yuv::Converter::to_srgb8` and `to_linear_rec2020` on synthetic 3840×2160 4:2:0 pictures:
//! 8-bit BT.709 (the sRGB fast path) and 10-bit BT.2020 HLG.
//!
//! `cargo run --release -p lightcraft-codecs --example yuv_bench [-- RUNS [FILTER]]`, FILTER a
//! part of a case's name ("8-bit BT.709 to_srgb8") to time only matching cases; `RAYON_NUM_THREADS=1`
//! for one thread. On a busy machine, time one case on one thread with `/usr/bin/time` at two run
//! counts: the difference in user time over the difference in runs is the CPU time per picture,
//! which other processes barely change.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use lightcraft_codecs::yuv::{Chroma, ColourDescription, Converter, Picture, Plane, Samples};

const W: usize = 3840;
const H: usize = 2160;

/// A smooth gradient with a little hash noise (so nothing is constant), scaled to `max`.
fn plane(w: usize, h: usize, max: u32, seed: u32) -> Vec<u32> {
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as u32, (i / w) as u32);
            let n = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503) ^ seed) >> 27;
            ((x * max / w as u32 + y * max / h as u32) / 2 + n).min(max)
        })
        .collect()
}

fn time(label: &str, runs: usize, mut f: impl FnMut()) {
    if !std::env::args().nth(2).is_none_or(|filter| label.contains(&filter)) {
        return;
    }
    f();
    let mut ms: Vec<f64> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(f64::total_cmp);
    if let (Some(min), Some(median)) = (ms.first(), ms.get(ms.len() / 2)) {
        println!("{label}: min {min:.2} ms, median {median:.2} ms ({runs} runs)");
    }
}

fn main() {
    let runs = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(20usize);
    let threads = std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "all".into());
    println!("{W}x{H} 4:2:0, threads: {threads}");
    let (cw, ch) = (W / 2, H / 2);

    let y8: Vec<u8> = plane(W, H, 235, 1).into_iter().map(|v| v.max(16) as u8).collect();
    let cb8: Vec<u8> = plane(cw, ch, 200, 2).into_iter().map(|v| (v + 28) as u8).collect();
    let cr8: Vec<u8> = plane(cw, ch, 200, 3).into_iter().map(|v| (v + 28) as u8).collect();
    let pic8 = Picture {
        width: W,
        height: H,
        bit_depth: 8,
        chroma: Chroma::C420,
        y: Plane { samples: Samples::U8(&y8), stride: W },
        cb: Plane { samples: Samples::U8(&cb8), stride: cw },
        cr: Plane { samples: Samples::U8(&cr8), stride: cw },
    };
    let c709 = Converter::new(ColourDescription::BT709, 8, W, H).unwrap();
    let mut out8 = vec![[0u8; 4]; W * H];
    time("8-bit BT.709 to_srgb8", runs, || c709.to_srgb8(&pic8, &mut out8).unwrap());

    let y10: Vec<u16> = plane(W, H, 940, 4).into_iter().map(|v| v.max(64) as u16).collect();
    let cb10: Vec<u16> = plane(cw, ch, 800, 5).into_iter().map(|v| (v + 112) as u16).collect();
    let cr10: Vec<u16> = plane(cw, ch, 800, 6).into_iter().map(|v| (v + 112) as u16).collect();
    let pic10 = Picture {
        width: W,
        height: H,
        bit_depth: 10,
        chroma: Chroma::C420,
        y: Plane { samples: Samples::U16(&y10), stride: W },
        cb: Plane { samples: Samples::U16(&cb10), stride: cw },
        cr: Plane { samples: Samples::U16(&cr10), stride: cw },
    };
    let hlg = ColourDescription { primaries: 9, transfer: 18, matrix: 9, full_range: false };
    let chlg = Converter::new(hlg, 10, W, H).unwrap();
    time("10-bit HLG to_srgb8", runs, || chlg.to_srgb8(&pic10, &mut out8).unwrap());
    let mut lin = vec![[0f32; 3]; W * H];
    time("10-bit HLG to_linear_rec2020", runs, || chlg.to_linear_rec2020(&pic10, &mut lin).unwrap());
    time("8-bit BT.709 to_linear_rec2020", runs, || c709.to_linear_rec2020(&pic8, &mut lin).unwrap());
}
