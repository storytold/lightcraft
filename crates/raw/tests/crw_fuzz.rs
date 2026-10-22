//! Robustness of the Canon CRW (CIFF) decoder: truncations and bit flips of real files must return an error or
//! an image, never panic or hang. Files come from `LIGHTCRAFT_CRW_FILES` (paths separated by `;`); the test is
//! skipped when it is unset. Synthetic garbage is always exercised.

use lightcraft_raw::decode;
use std::time::{Duration, Instant};

fn lcg(s: &mut u64) -> u64 {
    *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *s >> 33
}

fn run(bytes: &[u8]) {
    let t = Instant::now();
    let _ = decode(bytes);
    assert!(t.elapsed() < Duration::from_secs(20), "decode took {:?}", t.elapsed());
}

#[test]
fn ciff_garbage_never_panics() {
    let mut s = 7u64;
    for n in [0usize, 8, 14, 64, 512, 4096] {
        let mut v: Vec<u8> = (0..n).map(|_| lcg(&mut s) as u8).collect();
        run(&v);
        if n >= 14 {
            v[..2].copy_from_slice(b"II");
            v[2..6].copy_from_slice(&26u32.to_le_bytes());
            v[6..14].copy_from_slice(b"HEAPCCDR");
            run(&v);
        }
    }
}

#[test]
fn crw_truncations_and_bit_flips() {
    let Some(list) = std::env::var_os("LIGHTCRAFT_CRW_FILES") else {
        eprintln!("skipped: LIGHTCRAFT_CRW_FILES not set");
        return;
    };
    let mut seed = 1u64;
    for p in list.to_string_lossy().split(';').filter(|p| !p.is_empty()) {
        let Ok(orig) = std::fs::read(p) else { continue };
        for k in 1..=24usize {
            run(&orig[..orig.len() * k / 25]);
        }
        run(&orig[..orig.len().min(64)]);
        for round in 0..24 {
            let mut v = orig.clone();
            // Early rounds hit the header and directory tail, later ones the whole file.
            let flips = 1 + round % 6;
            for _ in 0..flips {
                let pos = if round % 2 == 0 {
                    let tail = v.len().saturating_sub(4096);
                    tail + (lcg(&mut seed) as usize) % (v.len() - tail)
                } else {
                    (lcg(&mut seed) as usize) % v.len()
                };
                v[pos] ^= 1 << (lcg(&mut seed) % 8);
            }
            run(&v);
        }
    }
}
