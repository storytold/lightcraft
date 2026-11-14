//! Ground-truth scenes for the X-Trans benchmark from real raws: every pattern-aligned 3 × 3 block of an X-Trans
//! mosaic (centre green, green corners, a red pair and a blue pair on opposite sides) holds all three colours with
//! one common centroid, so its per-colour means are a full-colour pixel with no interpolation at a third of the
//! resolution.
//!
//! Output: one `.rgbf` per X-Trans file (see `xtrans_bench`), white balanced with the as-shot multipliers and scaled
//! so the 99th percentile of green is 0.7. Blocks with a clipped sample are NaN (skipped by training and scoring).
//!
//! `cargo run --release -p lightcraft-raw --example xtrans_truth -- <raw dir> <out dir>`

use lightcraft_raw::{Cfa, Rgb32f, decode};
use std::path::Path;

const CLIP: f32 = 0.97;

/// The offset (0..3, 0..3) of the 3 × 3 blocks whose centre is green with the same colour on opposite sides.
fn block_origin(cfa: &Cfa) -> Option<(usize, usize)> {
    (0..3).flat_map(|oy| (0..3).map(move |ox| (ox, oy))).find(|&(ox, oy)| {
        // every block of the 6 × 6 tile at this offset must be symmetric
        (0..2).all(|by| {
            (0..2).all(|bx| {
                let (cx, cy) = (ox + 3 * bx + 1, oy + 3 * by + 1);
                let c = |dx: isize, dy: isize| cfa.color_at((cx as isize + dx) as usize, (cy as isize + dy) as usize);
                c(0, 0) == 1
                    && [(-1, -1), (1, -1), (-1, 1), (1, 1)].iter().all(|&(dx, dy)| c(dx, dy) == 1)
                    && c(-1, 0) == c(1, 0)
                    && c(0, -1) == c(0, 1)
                    && c(-1, 0) != 1
                    && c(0, -1) != 1
                    && c(-1, 0) != c(0, -1)
            })
        })
    })
}

fn save(p: &Path, img: &Rgb32f) {
    let mut b = Vec::with_capacity(8 + img.data.len() * 12);
    b.extend((img.width as u32).to_le_bytes());
    b.extend((img.height as u32).to_le_bytes());
    for px in &img.data {
        for v in px {
            b.extend(v.to_le_bytes());
        }
    }
    std::fs::write(p, b).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = Path::new(&args[1]);
    let out = Path::new(&args[2]);
    std::fs::create_dir_all(out).unwrap();
    let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
    files.sort();
    for p in files {
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Ok(raw) = decode(&bytes) else { continue };
        let Ok(n) = raw.normalized() else { continue };
        let Some(cfa) = n.cfa.as_ref().filter(|c| n.cpp == 1 && c.width == 6 && c.height == 6) else { continue };
        let Some((ox, oy)) = block_origin(cfa) else {
            eprintln!("{}: X-Trans layout not recognised", p.display());
            continue;
        };
        let wb = raw.wb_multipliers.unwrap_or([1.0; 3]);
        let wb = wb.map(|v| v / wb[1]);
        let (bw, bh) = ((n.width - ox) / 3, (n.height - oy) / 3);
        let mut img = Rgb32f::new(bw, bh);
        for by in 0..bh {
            for bx in 0..bw {
                let (mut s, mut k, mut clipped) = ([0f32; 3], [0f32; 3], false);
                for dy in 0..3 {
                    for dx in 0..3 {
                        let (x, y) = (ox + 3 * bx + dx, oy + 3 * by + dy);
                        let v = n.data[y * n.width + x];
                        clipped |= v >= CLIP || !v.is_finite();
                        let c = cfa.color_at(x, y) as usize;
                        s[c] += v;
                        k[c] += 1.0;
                    }
                }
                img.data[by * bw + bx] = if clipped { [f32::NAN; 3] } else { std::array::from_fn(|c| s[c] / k[c] * wb[c]) };
            }
        }
        let mut g: Vec<f32> = img.data.iter().map(|p| p[1]).filter(|v| v.is_finite()).collect();
        if g.len() < img.data.len() / 2 {
            eprintln!("{}: mostly clipped, skipped", p.display());
            continue;
        }
        g.sort_unstable_by(|a, b| a.total_cmp(b));
        let p99 = g[g.len() * 99 / 100].max(1e-6);
        let scale = 0.7 / p99;
        for px in &mut img.data {
            // (NaN marks a clipped block: `max` would turn it into 0)
            for v in px.iter_mut().filter(|v| v.is_finite()) {
                *v = (*v * scale).max(0.0);
            }
        }
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        save(&out.join(format!("{stem}.rgbf")), &img);
        println!("{stem}: {bw}×{bh}, wb {:.2}/{:.2}, exposure ×{scale:.2}", wb[0], wb[2]);
    }
}
