//! `cargo run --release -p lcc-raw --example demosaic_bench -- RAW...`
//!
//! Demosaic quality and speed on real Bayer raws without a reference render: each 2 × 2 CFA quad is binned into
//! one full-colour pixel (R, mean G, B: no interpolation, so a ground truth at half size), that image is mosaicked
//! again in RGGB and demosaiced by every method; PSNR against the binned truth (8-pixel border excluded) and the
//! wall time of each demosaic on the full-size mosaic are printed.

use lcc_raw::demosaic::{mosaic_from_rgb, psnr};
use lcc_raw::{Cfa, Method, Rgb32f, decode, demosaic};
use std::time::Instant;

const METHODS: [(&str, Method); 5] =
    [("bilinear", Method::Bilinear), ("ppg", Method::Ppg), ("ahd", Method::Ahd), ("detail", Method::Detail), ("rcd", Method::Rcd)];

fn main() {
    let mut sums = [0f64; 5];
    let mut times = [0f64; 5];
    let mut n = 0usize;
    for path in std::env::args().skip(1) {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(raw) = decode(&bytes) else { continue };
        let Ok(norm) = raw.normalized() else { continue };
        let Some(cfa) = norm.cfa.clone().filter(|c| c.is_bayer() && norm.cpp == 1) else { continue };
        let (w, h) = (norm.width / 2, norm.height / 2);
        if w < 64 || h < 64 {
            continue;
        }
        let truth = Rgb32f::from_fn(w, h, |x, y| {
            let mut p = [0f32; 3];
            let mut gs = 0f32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (sx, sy) = (2 * x + dx, 2 * y + dy);
                let v = norm.data[sy * norm.width + sx].clamp(0.0, 1.0);
                match cfa.color_at(sx, sy) {
                    1 => gs += v * 0.5,
                    c => p[c as usize] = v,
                }
            }
            p[1] = gs;
            p
        });
        let mosaic = mosaic_from_rgb(&truth, &Cfa::bayer("RGGB").expect("pattern"));
        print!("{:<40}", path.rsplit(['/', '\\']).next().unwrap_or(&path));
        for (i, (name, m)) in METHODS.iter().enumerate() {
            let p = psnr(&demosaic(&mosaic, *m), &truth, 8);
            let t = Instant::now();
            drop(demosaic(&norm, *m));
            let ms = t.elapsed().as_secs_f64() * 1e3;
            sums[i] += p;
            times[i] += ms;
            print!(" {name} {p:6.2} dB {ms:7.0} ms |");
        }
        println!();
        n += 1;
    }
    if n > 0 {
        println!("mean over {n} raws:");
        for (i, (name, _)) in METHODS.iter().enumerate() {
            println!("  {name:<9} {:6.2} dB {:7.0} ms", sums[i] / n as f64, times[i] / n as f64);
        }
    }
}
