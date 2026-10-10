//! Opt-in end-to-end check of the real RawNIND Bayer model on real mosaics with noise added:
//!
//! ```text
//! LC_DENOISE_MODEL=<model_bayer.onnx> LC_DENOISE_MOSAICS=<folder> LC_DENOISE_OUT=<folder> \
//!   cargo test --release -p dac-denoise --features runtime --test real_model -- --ignored --nocapture
//! ```
//!
//! `LC_DENOISE_MOSAICS` holds `<name>.mosaic.f32` (little-endian f32, normalised: black 0, white 1, not white
//! balanced), `<name>.rgb.f32` (a clean demosaic of it, camera RGB, interleaved) and `<name>.txt` (`width`,
//! `height`, `cfa`, `wb` lines). Noise is Poisson-Gaussian, made here. Optional: `LC_DENOISE_PARALLEL` (tiles at
//! once, default 8), `LC_DENOISE_ONLY` (one name), `LC_DENOISE_ISO` (noise strength, default 1.0; 0 adds none),
//! `LC_DENOISE_FULL` (the whole frame instead of a 2048 × 1536 crop).

#![cfg(feature = "runtime")]

use std::path::{Path, PathBuf};
use std::time::Instant;

use dac_denoise::bayer::Layout;
use dac_denoise::manifest::{DenoiserManifest, Domain, Gain};
use dac_denoise::run::{Control, Params, denoise_bayer};
use dac_denoise::runtime::CpuRunner;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        ((self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        (-2.0 * self.next().ln()).sqrt() * (std::f64::consts::TAU * self.next()).cos()
    }
}

fn read_f32(path: &Path) -> Vec<f32> {
    std::fs::read(path).unwrap().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

struct Dump {
    width: usize,
    height: usize,
    layout: Layout,
    wb: [f32; 3],
    mosaic: Vec<f32>,
    rgb: Vec<f32>,
}

fn load(dir: &Path, name: &str) -> Dump {
    let text = std::fs::read_to_string(dir.join(format!("{name}.txt"))).unwrap();
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string())).unwrap();
    let cell = match field("cfa").as_str() {
        "RGGB" => [0, 1, 1, 2],
        "BGGR" => [2, 1, 1, 0],
        "GRBG" => [1, 0, 2, 1],
        "GBRG" => [1, 2, 0, 1],
        other => panic!("cfa {other}"),
    };
    let wb: Vec<f32> = field("wb").split_whitespace().map(|v| v.parse().unwrap()).collect();
    Dump {
        width: field("width").parse().unwrap(),
        height: field("height").parse().unwrap(),
        layout: Layout::from_cell(cell).unwrap(),
        wb: [wb[0], wb[1], wb[2]],
        mosaic: read_f32(&dir.join(format!("{name}.mosaic.f32"))),
        rgb: read_f32(&dir.join(format!("{name}.rgb.f32"))),
    }
}

/// Crop with an even origin so the Bayer phase is kept.
fn crop(d: &Dump, x0: usize, y0: usize, w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let mut m = Vec::with_capacity(w * h);
    let mut c = Vec::with_capacity(w * h * 3);
    for y in y0..y0 + h {
        m.extend_from_slice(&d.mosaic[y * d.width + x0..y * d.width + x0 + w]);
        c.extend_from_slice(&d.rgb[(y * d.width + x0) * 3..(y * d.width + x0 + w) * 3]);
    }
    (m, c)
}

/// 0 = red, 1 = green, 2 = blue at a mosaic position.
fn site(layout: Layout, x: usize, y: usize) -> usize {
    match ((x + layout.dx) % 2, (y + layout.dy) % 2) {
        (0, 0) => 0,
        (1, 1) => 2,
        _ => 1,
    }
}

/// The simplest demosaic (each missing colour is the mean of the same-coloured samples in the 3 × 3 around),
/// as the baseline a user would get from the noisy picture without AI.
fn bilinear(m: &[f32], w: usize, h: usize, layout: Layout) -> Vec<f32> {
    let mut out = vec![0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0f32; 3];
            let mut n = [0f32; 3];
            for yy in y.saturating_sub(1)..(y + 2).min(h) {
                for xx in x.saturating_sub(1)..(x + 2).min(w) {
                    let c = site(layout, xx, yy);
                    sum[c] += m[yy * w + xx];
                    n[c] += 1.0;
                }
            }
            for c in 0..3 {
                out[(y * w + x) * 3 + c] = if n[c] > 0.0 { sum[c] / n[c] } else { 0.0 };
            }
        }
    }
    out
}

/// Signal-to-error ratio in dB of `got` against `want` (both interleaved RGB), ignoring a border.
fn snr(got: &[f32], want: &[f32], w: usize, h: usize, border: usize) -> f64 {
    let (mut sig, mut err) = (0f64, 0f64);
    for y in border..h - border {
        for x in border..w - border {
            for c in 0..3 {
                let (g, t) = (f64::from(got[(y * w + x) * 3 + c]), f64::from(want[(y * w + x) * 3 + c]));
                sig += t * t;
                err += (g - t).powi(2);
            }
        }
    }
    10.0 * (sig / err.max(1e-30)).log10()
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// A viewable PNG of interleaved camera-RGB panels side by side: white balanced, one exposure for all, gamma.
fn write_png(path: &Path, panels: &[&[f32]], w: usize, h: usize, wb: [f32; 3], exposure: f32) {
    let total_w = w * panels.len();
    let mut raw = Vec::with_capacity((total_w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0u8);
        for p in panels {
            for x in 0..w {
                for c in 0..3 {
                    let v = (p[(y * w + x) * 3 + c] * wb[c] * exposure).clamp(0.0, 1.0);
                    raw.push((v.powf(1.0 / 2.2) * 255.0 + 0.5) as u8);
                }
            }
        }
    }
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        png.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut k = kind.to_vec();
        k.extend_from_slice(body);
        png.extend_from_slice(&k);
        png.extend_from_slice(&crc32(&k).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(total_w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::write(path, png).unwrap();
}

#[test]
#[ignore = "needs a real model and mosaics: see the module docs"]
fn the_real_model_denoises_real_mosaics() {
    let (Some(model), Some(dir)) = (std::env::var_os("LC_DENOISE_MODEL"), std::env::var_os("LC_DENOISE_MOSAICS")) else { return };
    let dir = PathBuf::from(dir);
    let out_dir = std::env::var_os("LC_DENOISE_OUT").map(PathBuf::from);
    let parallel: usize = std::env::var("LC_DENOISE_PARALLEL").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    let iso: f64 = std::env::var("LC_DENOISE_ISO").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let names: Vec<String> = match std::env::var("LC_DENOISE_ONLY") {
        Ok(n) => vec![n],
        Err(_) => ["arw-sony-a7m3-compressed", "cr2-canon-6d", "nef-nikon-d5100-lossless", "dng-google-pixel2xl", "pef-pentax-k3"]
            .map(String::from)
            .to_vec(),
    };
    let manifest = DenoiserManifest {
        id: "rawnind-bayer".into(),
        name: "RawNIND Bayer".into(),
        version: "1".into(),
        licence: Default::default(),
        source: None,
        sha256: None,
        size_bytes: None,
        provenance: String::new(),
        domain: Domain::BayerToRgb,
        tile: 512,
        overlap: 64,
        gain: Gain::MatchMean { nominal: 1.0e6, max_deviation: 0.05 },
    };
    let runner = CpuRunner::load(Path::new(&model), &manifest).unwrap();
    let params = Params { tile: 512, overlap: 64, gain: manifest.gain, clip: Some(1.0), parallel };
    for name in names {
        let d = load(&dir, &name);
        // a crop of about 2 × 1.5 tiles, away from the edges, even origin
        let full = std::env::var_os("LC_DENOISE_FULL").is_some();
        let (w, h) = if full { (d.width & !1, d.height & !1) } else { (2048usize, 1536usize) };
        let (x0, y0) = (((d.width - w) / 3) & !1, ((d.height - h) / 2) & !1);
        let (clean, reference) = crop(&d, x0, y0, w, h);
        // Poisson-Gaussian noise: variance a·signal + b, as a sensor at high ISO
        let (a, b) = (1.5e-3 * iso, (4.0e-3 * iso.sqrt()).powi(2));
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ name.len() as u64);
        let noisy: Vec<f32> = clean
            .iter()
            .map(|&v| {
                let v = f64::from(v);
                let sigma = (a * v.max(0.0) + b).sqrt();
                ((v + sigma * rng.gauss()) as f32).min(1.0)
            })
            .collect();
        let started = Instant::now();
        let done = std::sync::atomic::AtomicUsize::new(0);
        let progress = |n: usize, total: usize| done.store(n * 1000 / total, std::sync::atomic::Ordering::Relaxed);
        let denoised = denoise_bayer(&noisy, w, h, d.layout, &runner, &params, &Control { cancel: None, progress: Some(&progress) }).unwrap();
        let secs = started.elapsed().as_secs_f64();
        let baseline = bilinear(&noisy, w, h, d.layout);
        let clean_bilinear = bilinear(&clean, w, h, d.layout);
        let den: Vec<f32> = denoised.data.iter().flat_map(|p| p.iter().copied()).collect();
        let (s_noisy, s_den) = (snr(&baseline, &reference, w, h, 64), snr(&den, &reference, w, h, 64));
        let s_floor = snr(&clean_bilinear, &reference, w, h, 64);
        println!(
            "{name}: {w}×{h} crop, {} tiles, parallel {parallel}, {secs:.1} s | SNR vs clean demosaic: noisy bilinear {s_noisy:.2} dB, denoised {s_den:.2} dB (clean bilinear {s_floor:.2} dB)",
            dac_denoise::run::tile_count(w, h, d.layout, 512, 64)
        );
        if let Some(out) = &out_dir {
            std::fs::create_dir_all(out).unwrap();
            let mean = reference.iter().map(|&v| f64::from(v)).sum::<f64>() / reference.len() as f64;
            let exposure = (0.18 / mean.max(1e-6)) as f32;
            // three panels, a 512 × 384 window each, at 1:1
            let (pw, ph, px, py) = (512usize, 384usize, 700usize, 500usize);
            let window =
                |buf: &[f32]| -> Vec<f32> { (0..ph).flat_map(|y| buf[((py + y) * w + px) * 3..((py + y) * w + px + pw) * 3].to_vec()).collect() };
            write_png(&out.join(format!("{name}.png")), &[&window(&baseline), &window(&den), &window(&reference)], pw, ph, d.wb, exposure);
        }
        if iso >= 4.0 {
            assert!(s_den > s_noisy + 2.0, "{name}: denoised {s_den:.2} dB is not clearly better than noisy {s_noisy:.2} dB");
        }
    }
}
