//! Opt-in quality check of the built-in denoiser on one public Bayer raw (CC0, from `cargo xtask corpus --download`).
//!
//! Noise of a known level is added to the raw's own mosaic, the built-in denoiser runs over it, and the result is
//! scored against the untouched mosaic demosaiced the same way. PSNR is measured on gamma-encoded camera RGB; the
//! detail figure is the gradient energy of the result over that of the truth (1 = as sharp, below 1 = smoother).
//!
//! LC_DENOISE_RAW=../../corpus/raw/arw-sony-a7m3-compressed.arw LC_DENOISE_OUT=/tmp/dn \
//!   cargo test --release -p lightcraft-engine --test denoise_builtin_quality -- --ignored --nocapture
//! LC_DENOISE_OUT, when set, receives side-by-side PNGs (truth | noisy | denoised) of a 100 % crop per level.

use lightcraft_codecs::encode::{EncodeImage, EncodeMeta, Samples, encode_png};
use lightcraft_denoise_core::bayer::Layout;
use lightcraft_denoise_core::builtin::{Builtin, OVERLAP, TILE, demosaic, estimate_noise};
use lightcraft_denoise_core::manifest::Gain;
use lightcraft_denoise_core::nlm::Tuning;
use lightcraft_denoise_core::run::{Control, Params, denoise_bayer};
use std::time::Instant;

struct Rng(u64);

impl Rng {
    /// The next uniform number, strictly between 0 and 1.
    fn uniform(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 + 0.5) / (1u64 << 24) as f32
    }

    /// The next standard normal number (Box-Muller).
    fn normal(&mut self) -> f32 {
        let (u, v) = (self.uniform(), self.uniform());
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
    }
}

/// The plain demosaic of a mosaic (RGGB, even size) as camera RGB planes, through the tile packer the runner uses.
fn plain(mosaic: &[f32], w: usize, h: usize) -> Vec<[f32; 3]> {
    let (cw, ch) = (w / 2, h / 2);
    let mut planes = vec![vec![0f32; cw * ch]; 4];
    for cy in 0..ch {
        for cx in 0..cw {
            for (p, (dx, dy)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
                planes[p][cy * cw + cx] = mosaic[(2 * cy + dy) * w + 2 * cx + dx];
            }
        }
    }
    // `demosaic` takes square planes: run it on square blocks and stitch
    let side = cw.min(ch);
    let mut out = vec![[0f32; 3]; w * h];
    for by in (0..ch).step_by(side) {
        for bx in (0..cw).step_by(side) {
            let (x0, y0) = (bx.min(cw - side), by.min(ch - side));
            let block: Vec<Vec<f32>> = planes.iter().map(|pl| (0..side * side).map(|i| pl[(y0 + i / side) * cw + x0 + i % side]).collect()).collect();
            let rgb = demosaic(&block, side);
            let s2 = 2 * side;
            for y in 0..s2 {
                for x in 0..s2 {
                    let o = (2 * y0 + y) * w + 2 * x0 + x;
                    out[o] = [rgb[y * s2 + x], rgb[s2 * s2 + y * s2 + x], rgb[2 * s2 * s2 + y * s2 + x]];
                }
            }
        }
    }
    out
}

/// A linear value encoded for viewing: clamped to 0..1, then gamma 2.2.
fn gamma(v: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(1.0 / 2.2)
}

/// PSNR in dB between two camera-RGB pictures after exposure `scale` and gamma, leaving out a 16-pixel border.
fn psnr(a: &[[f32; 3]], b: &[[f32; 3]], w: usize, h: usize, scale: f32) -> f64 {
    let (mut sum, mut n) = (0f64, 0f64);
    for y in 16..h - 16 {
        for x in 16..w - 16 {
            for c in 0..3 {
                let d = f64::from(gamma(a[y * w + x][c] * scale) - gamma(b[y * w + x][c] * scale));
                sum += d * d;
                n += 1.0;
            }
        }
    }
    -10.0 * (sum / n).log10()
}

/// The summed gradient of the picture's green channel after exposure `scale` and gamma, leaving out a 16-pixel border.
fn gradient(a: &[[f32; 3]], w: usize, h: usize, scale: f32) -> f64 {
    let mut sum = 0f64;
    for y in 16..h - 16 {
        for x in 16..w - 16 {
            let g = |x: usize, y: usize| f64::from(gamma(a[y * w + x][1] * scale));
            sum += (g(x + 1, y) - g(x, y)).abs() + (g(x, y + 1) - g(x, y)).abs();
        }
    }
    sum
}

/// How much of the true picture's fine structure the result keeps: the slope of the result's high-pass (green minus
/// its 5 x 5 mean) against the truth's. Noise is independent of the truth, so unlike gradient energy it can't inflate it.
fn texture(a: &[[f32; 3]], truth: &[[f32; 3]], w: usize, h: usize, scale: f32) -> f64 {
    let hp = |p: &[[f32; 3]], x: usize, y: usize| {
        let mut m = 0f64;
        for j in 0..5 {
            for i in 0..5 {
                m += f64::from(gamma(p[(y + j - 2) * w + x + i - 2][1] * scale));
            }
        }
        f64::from(gamma(p[y * w + x][1] * scale)) - m / 25.0
    };
    let (mut xy, mut xx) = (0f64, 0f64);
    for y in 16..h - 16 {
        for x in 16..w - 16 {
            let t = hp(truth, x, y);
            xy += t * hp(a, x, y);
            xx += t * t;
        }
    }
    xy / xx
}

/// A `side × side` crop of the picture from `(x0, y0)` as 8-bit RGB with a daylight white balance, for the PNGs.
fn crop_rgb8(a: &[[f32; 3]], w: usize, (x0, y0): (usize, usize), side: usize, scale: f32) -> Vec<u8> {
    let wb = [2.0f32, 1.0, 1.6];
    let mut out = Vec::with_capacity(side * side * 3);
    for y in 0..side {
        for x in 0..side {
            for c in 0..3 {
                out.push((gamma(a[(y0 + y) * w + x0 + x][c] * wb[c] * scale) * 255.0 + 0.5) as u8);
            }
        }
    }
    out
}

/// Adds noise of known levels to a public raw's mosaic, denoises it with the built-in denoiser and prints how close the
/// result is to the untouched mosaic. Opt-in: it needs the raw named by `LC_DENOISE_RAW`.
#[test]
#[ignore = "needs a public raw: LC_DENOISE_RAW=<file>"]
fn the_built_in_denoiser_scored_on_a_real_raw_with_added_noise() {
    let path = std::env::var("LC_DENOISE_RAW").unwrap_or_else(|_| "../../corpus/raw/arw-sony-a7m3-compressed.arw".into());
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let n = lightcraft_raw::decode(&bytes).unwrap().normalized().unwrap();
    let cfa = n.cfa.as_ref().unwrap();
    let layout = Layout::from_cell([cfa.color_at(0, 0), cfa.color_at(1, 0), cfa.color_at(0, 1), cfa.color_at(1, 1)]).unwrap();
    assert_eq!(layout, Layout { dx: 0, dy: 0 }, "the benchmark assumes RGGB");
    // an even-aligned window from the middle of the frame keeps the colour pattern
    let (w, h) = (1536usize, 1024usize);
    let (ox, oy) = (((n.width - w) / 2) & !1, ((n.height - h) / 2) & !1);
    let truth: Vec<f32> = (0..w * h).map(|i| n.data[(oy + i / w) * n.width + ox + i % w]).collect();
    // LC_BIN=1: average each cell with the three nearest of its own colour (a 2 x 2 binning of the colour pattern's
    // cells), which halves the reference's own noise so that noise added at a camera's low levels dominates it
    let (w, h, truth) = if std::env::var("LC_BIN").is_ok_and(|v| v != "0") {
        let (bw, bh) = (w / 2, h / 2);
        let binned: Vec<f32> = (0..bw * bh)
            .map(|i| {
                let (x, y) = (i % bw, i / bw);
                let (sx, sy, cx, cy) = (x / 2, y / 2, x % 2, y % 2);
                let at = |di: usize, dj: usize| truth[(2 * (2 * sy + dj) + cy) * w + 2 * (2 * sx + di) + cx];
                (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4.0
            })
            .collect();
        (bw, bh, binned)
    } else {
        (w, h, truth)
    };
    let mean = truth.iter().map(|v| f64::from(*v)).sum::<f64>() / truth.len() as f64;
    let scale = (0.18 / mean) as f32;
    let truth_rgb = plain(&truth, w, h);
    let own = estimate_noise(&truth, w, h);
    println!(
        "the reference's own noise by the estimator: a {:.5} b {:.6} (sd at 0.18 {:.4}); a clean reference would read about 0",
        own.0,
        own.1,
        (own.0 * 0.18 + own.1).sqrt()
    );
    println!("{path}: {}x{}, window {w}x{h}, mean {mean:.4}", n.width, n.height);
    println!("{:<14}{:>10}{:>10}{:>9}{:>9}{:>9}{:>9}", "noise (a, b)", "noisy dB", "denoised", "gain", "detail", "texture", "seconds");
    let out_dir = std::env::var("LC_DENOISE_OUT").ok();
    #[cfg(feature = "denoise")]
    let gpu = std::env::var("LC_GPU").is_ok_and(|v| v != "0").then(|| {
        let p = Tuning::default().bm3d.unwrap();
        let g = lightcraft_gpu::bm3d::Bm3dGpu::new(&p, TILE as usize).unwrap();
        println!("on the graphics card: {} ({})", g.adapter(), g.build_report());
        std::sync::Arc::new(g)
    });
    // LC_REAL=1 runs the noise levels measured on real Canon EOS R / R6 Mark III raws instead (their standard deviation at
    // mid-grey is 0.003, 0.006, 0.0095, 0.015 and 0.038: ISO <= 400, 1600, 6400, 25600 and above)
    let levels: &[(&str, f32, f32)] = if std::env::var("LC_REAL").is_ok_and(|v| v != "0") {
        &[("iso400", 4e-5, 1e-7), ("iso1600", 2.1e-4, 3e-7), ("iso6400", 4.9e-4, 1e-6), ("iso25600", 1.19e-3, 1.7e-5), ("iso51200", 6.9e-3, 2.08e-4)]
    } else {
        &[("light", 0.002, 0.0002), ("medium", 0.008, 0.001), ("heavy", 0.025, 0.004), ("extreme", 0.06, 0.012)]
    };
    for &(name, a, b) in levels {
        let mut rng = Rng(11);
        let noisy: Vec<f32> = truth.iter().map(|&v| v + rng.normal() * (a * v.max(0.0) + b).sqrt()).collect();
        let started = Instant::now();
        let params = Params { tile: TILE as usize, overlap: OVERLAP as usize, gain: Gain::None, clip: Some(0.99), parallel: 4 };
        // LC_EST=v1 (the first estimator) | true (the noise that was added) | anything else: the current estimator
        // LC_FULL=1: measure the noise on the whole frame with the same noise added, as the app does (it measures the whole
        // photo), instead of on the crop
        let measure = |mosaic: &[f32], mw: usize, mh: usize| match std::env::var("LC_EST").as_deref() {
            Ok("v1") => lightcraft_denoise_core::builtin::estimate_noise_v1(mosaic, mw, mh),
            _ => estimate_noise(mosaic, mw, mh),
        };
        let est = match std::env::var("LC_EST").as_deref() {
            Ok("true") => (a, b),
            _ if std::env::var("LC_TRUE").is_ok_and(|v| v != "0") => (a, b),
            _ if std::env::var("LC_FULL").is_ok_and(|v| v != "0") => {
                let mut rng = Rng(23);
                let frame: Vec<f32> = n.data.iter().map(|&v| v + rng.normal() * (a * v.max(0.0) + b).sqrt()).collect();
                measure(&frame, n.width, n.height)
            }
            _ => measure(&noisy, w, h),
        };
        let mut tuning = Tuning::default();
        let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
        (tuning.search, tuning.search2) = (env("LC_S1").map_or(tuning.search, |v| v as usize), env("LC_S2").map_or(tuning.search2, |v| v as usize));
        (tuning.h_light, tuning.h_heavy, tuning.h2) =
            (env("LC_HL").unwrap_or(tuning.h_light), env("LC_HH").unwrap_or(tuning.h_heavy), env("LC_H2").unwrap_or(tuning.h2));
        (tuning.light, tuning.heavy) = (env("LC_LIGHT").unwrap_or(tuning.light), env("LC_HEAVY").unwrap_or(tuning.heavy));
        (tuning.half, tuning.half2) = (env("LC_P1").map_or(tuning.half, |v| v as usize), env("LC_P2").map_or(tuning.half2, |v| v as usize));
        (tuning.loose, tuning.few) = (env("LC_LOOSE").unwrap_or(tuning.loose), env("LC_FEW").unwrap_or(tuning.few));
        (tuning.search_c, tuning.h_c) = (env("LC_SC").map_or(tuning.search_c, |v| v as usize), env("LC_HC").unwrap_or(tuning.h_c));
        // LC_NLM=1 runs the old non-local-means filter instead; LC_B_* override the block-matching filter's parameters
        if env("LC_NLM").is_some_and(|v| v > 0.0) {
            tuning.bm3d = None;
        }
        if let Some(b) = tuning.bm3d.as_mut() {
            (b.block, b.step, b.search, b.group) = (
                env("LC_B_N").map_or(b.block, |v| v as usize),
                env("LC_B_STEP").map_or(b.step, |v| v as usize),
                env("LC_B_S").map_or(b.search, |v| v as usize),
                env("LC_B_K").map_or(b.group, |v| v as usize),
            );
            (b.tau1, b.tau2) = (env("LC_B_T1").unwrap_or(b.tau1), env("LC_B_T2").unwrap_or(b.tau2));
            // LC_B_LOFF adds to every point of the threshold curve (a sweep of how hard it cleans)
            let off = env("LC_B_LOFF").unwrap_or(0.0);
            for point in &mut b.lambda {
                point.1 += off;
            }
            b.sigma = env("LC_B_SIG").unwrap_or(b.sigma);
            b.wiener = env("LC_B_WIENER").is_none_or(|v| v > 0.0);
        }
        let runner = Builtin { noise: Some(est), tuning };
        // LC_GPU=1 (with `--features denoise`) runs the block-matching filter on the graphics card
        #[cfg(feature = "denoise")]
        let out = match &gpu {
            Some(g) => {
                let on_card = lightcraft_gpu::bm3d::BuiltinRunner::new(g.clone(), runner).unwrap();
                denoise_bayer(&noisy, w, h, layout, &on_card, &params, &Control::default()).unwrap()
            }
            None => denoise_bayer(&noisy, w, h, layout, &runner, &params, &Control::default()).unwrap(),
        };
        #[cfg(not(feature = "denoise"))]
        let out = denoise_bayer(&noisy, w, h, layout, &runner, &params, &Control::default()).unwrap();
        let secs = started.elapsed().as_secs_f64();
        let noisy_rgb = plain(&noisy, w, h);
        let (p_noisy, p_out) = (psnr(&noisy_rgb, &truth_rgb, w, h, scale), psnr(&out.data, &truth_rgb, w, h, scale));
        let detail = gradient(&out.data, w, h, scale) / gradient(&truth_rgb, w, h, scale);
        let tex = texture(&out.data, &truth_rgb, w, h, scale);
        println!(
            "{:<14}{:>10.2}{:>10.2}{:>+9.2}{:>9.3}{:>9.3}{:>9.2}   noise a {:.4} (true {a}), b {:.5} (true {b})",
            name,
            p_noisy,
            p_out,
            p_out - p_noisy,
            detail,
            tex,
            secs,
            est.0,
            est.1
        );
        if let Some(dir) = &out_dir {
            let side = 400;
            let at = (w / 2 - side / 2, h / 2 - side / 2);
            let tiles =
                [crop_rgb8(&truth_rgb, w, at, side, scale), crop_rgb8(&noisy_rgb, w, at, side, scale), crop_rgb8(&out.data, w, at, side, scale)];
            let mut sheet = Vec::with_capacity(side * side * 9);
            for y in 0..side {
                for t in &tiles {
                    sheet.extend_from_slice(&t[y * side * 3..(y + 1) * side * 3]);
                }
            }
            let img = EncodeImage::new((3 * side) as u32, side as u32, 3, Samples::U8(&sheet));
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(format!("{dir}/{name}.png"), encode_png(&img, &EncodeMeta::default()).unwrap()).unwrap();
        }
    }
}
