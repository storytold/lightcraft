//! GoPro GPR (VC-5) on one CC0 file per body from raw.pixls.us (URLs and SHA-256 in `RAW_SAMPLES`,
//! xtask/src/main.rs; downloaded into the gitignored corpus/raw by `cargo xtask corpus --download`).
//!
//! GPRs carry no camera JPEG, so the check is black-box on the mosaic itself: the two green sites of every 2×2 cell
//! see nearly the same light, so on a 48 × 36 grid of the active area their means must agree (Pearson r > 0.99;
//! 0.9999 or more on these files). A misread codeblock, quantization or wavelet level breaks that (the greens come
//! from different channel sums), as would a swapped layout. The FNV-1a hashes over the decoded u16 samples
//! (row-major, little-endian) pin the decode; the renders were inspected by eye at 100 %. Missing files are skipped.

use lightcraft_raw::{RawData, decode, probe_info};
use std::path::{Path, PathBuf};

fn corpus() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

/// Correlation of the two greens of each 2×2 cell, averaged over a `gw × gh` grid of the image.
fn green_pair_correlation(raw: &lightcraft_raw::RawImage, gw: usize, gh: usize) -> f64 {
    let RawData::U16(s) = &raw.data else { panic!("integer sensor data expected") };
    let cfa = raw.cfa.clone().expect("colour-filter layout");
    let greens: Vec<(usize, usize)> = [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().filter(|&(x, y)| cfa.color_at(x, y) == 1).collect();
    let [g1, g2] = greens.as_slice() else { panic!("two greens per cell expected") };
    let (mut a, mut b, mut n) = (vec![0f64; gw * gh], vec![0f64; gw * gh], vec![0f64; gw * gh]);
    for y in (0..raw.height - 1).step_by(2) {
        for x in (0..raw.width - 1).step_by(2) {
            let i = (y * gh / raw.height) * gw + x * gw / raw.width;
            a[i] += f64::from(s[(y + g1.1) * raw.width + x + g1.0]);
            b[i] += f64::from(s[(y + g2.1) * raw.width + x + g2.0]);
            n[i] += 1.0;
        }
    }
    let a: Vec<f64> = a.iter().zip(&n).map(|(s, n)| s / n).collect();
    let b: Vec<f64> = b.iter().zip(&n).map(|(s, n)| s / n).collect();
    let k = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / k, b.iter().sum::<f64>() / k);
    let cov: f64 = a.iter().zip(&b).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let va: f64 = a.iter().map(|x| (x - ma).powi(2)).sum();
    let vb: f64 = b.iter().map(|y| (y - mb).powi(2)).sum();
    cov / (va * vb).sqrt()
}

#[test]
fn gopro_gpr_bodies_decode() {
    // (file, sensor width × height, red site of the 2×2 cell, white level, pinned FNV-1a of the sensor samples)
    let cases: &[(&str, (usize, usize), (usize, usize), f32, u64)] = &[
        ("gpr-hero5-936.gpr", (4000, 3000), (0, 0), 16383.0, 0xd880_d013_5ecd_0601),
        ("gpr-hero6-2556.gpr", (4000, 3000), (0, 0), 16383.0, 0x3632_e6f2_5462_228b),
        ("gpr-hero7-2786.gpr", (4000, 3000), (0, 0), 16383.0, 0x7073_0a81_6059_27a3),
        ("gpr-hero8-3949.gpr", (4000, 3000), (0, 0), 16383.0, 0x9d61_b193_1740_ee62),
        ("gpr-hero9-4483.gpr", (5568, 4176), (0, 0), 16383.0, 0xd775_c13f_d5e2_0345),
        ("gpr-hero10-4883.gpr", (5568, 4176), (0, 1), 4095.0, 0xc5c5_001d_9323_61de),
        ("gpr-hero11-7319.gpr", (5568, 4872), (0, 1), 4095.0, 0x0583_4b55_ae70_56f7),
        ("gpr-fusion-7370.gpr", (3104, 3000), (0, 0), 16383.0, 0x9f34_c7b9_74a9_85e6),
    ];
    let mut seen = 0;
    for &(name, sensor, red, white, hash) in cases {
        let path = corpus().join("raw").join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
            continue;
        };
        let t0 = std::time::Instant::now();
        let raw = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        raw.validate().unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp), (sensor.0, sensor.1, 1), "{name}: sensor size");
        assert_eq!(raw.info(), probe_info(&bytes).unwrap(), "{name}: probe and decode disagree");
        assert_eq!(raw.cfa.as_ref().map(|c| c.color_at(red.0, red.1)), Some(0), "{name}: red site");
        assert_eq!(raw.white_at(0), white, "{name}: white level");
        let RawData::U16(samples) = &raw.data else { panic!("{name}: integer sensor data expected") };
        assert!(samples.iter().all(|&v| f32::from(v) <= white), "{name}: samples above the white level");
        let actual = samples
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .fold(0xcbf2_9ce4_8422_2325u64, |h, byte| (h ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3));
        let r = green_pair_correlation(&raw, 48, 36);
        eprintln!("{name:24} {}x{} in {ms:.0} ms: greens r = {r:.5}, hash {actual:#018x}", raw.width, raw.height);
        assert!(r > 0.99, "{name}: the two greens of each cell disagree (r = {r:.5})");
        assert_eq!(actual, hash, "{name}: sensor pixels differ from the pinned decode");
        seen += 1;
    }
    eprintln!("GoPro GPR bodies checked on {seen} files");
}
