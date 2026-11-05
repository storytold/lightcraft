//! Opt-in accuracy check of the built-in denoiser's noise estimator (public CC0 raws from `cargo xtask corpus --download`).
//!
//! 1. `a_quantile_of_pure_noise_blocks_...`: white noise of a known level: what fraction of the true variance a quantile of
//!    the blocks' variances is (the correction `NoiseProbe::correction` divides by).
//! 2. `the_noise_estimate_...`: noise of known levels added to 2 x 2-binned crops of many raws (which halves the pictures'
//!    own noise), then the estimates against the truth, for the first version and the current one, at 0.03 (the dark
//!    cells of a typical photo) and 0.18 (mid-grey).
//!
//!   LC_CORPUS=C:/.../corpus/raw LC_STRIDE=6 cargo test --release -p lightcraft-engine --test denoise_noise_estimate -- --ignored --nocapture
use lightcraft_denoise_core::builtin::{NoiseProbe, estimate_noise_v1, estimate_noise_with};

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

/// The environment variable `k` as a number, if it is set and parses.
fn env(k: &str) -> Option<f32> {
    std::env::var(k).ok().and_then(|v| v.parse().ok())
}

/// Prints what fraction of the true variance a quantile of pure-noise blocks' variances is, for each block size and
/// quantile: the calibration behind `NoiseProbe::correction`.
#[test]
#[ignore = "calibration printout"]
fn a_quantile_of_pure_noise_blocks_is_a_known_fraction_of_the_true_variance() {
    let (w, h) = (2048usize, 2048usize);
    let sigma = 0.01f32;
    let mut rng = Rng(3);
    let mosaic: Vec<f32> = (0..w * h).map(|i| 0.1 + 0.7 * (i % w) as f32 / w as f32 + rng.normal() * sigma).collect();
    println!("{:>6}{:>9}{:>14}  (estimated variance / true variance, correction 1; a brightness ramp, pure noise)", "block", "quantile", "fraction");
    for block in [8usize, 12, 16, 24, 32] {
        for quantile in [0.02f32, 0.05, 0.1, 0.2, 0.5] {
            let (_, b) = estimate_noise_with(&mosaic, w, h, &NoiseProbe { block, quantile, correction: 1.0 });
            println!("{block:>6}{quantile:>9}{:>14.3}", b / (sigma * sigma));
        }
    }
}

/// The 2 x 2 colour-cell binning of a window of a normalised mosaic (halves its own noise), as the quality benchmark does.
fn binned_crop(n: &lightcraft_raw::Normalized, side: usize) -> Option<(Vec<f32>, usize)> {
    let side = side.min(n.width).min(n.height) & !3;
    if side < 512 {
        return None;
    }
    let (ox, oy) = (((n.width - side) / 2) & !1, ((n.height - side) / 2) & !1);
    let half = side / 2;
    let out = (0..half * half)
        .map(|i| {
            let (x, y) = (i % half, i / half);
            let (sx, sy, cx, cy) = (x / 2, y / 2, x % 2, y % 2);
            let at = |di: usize, dj: usize| n.data[(oy + 2 * (2 * sy + dj) + cy) * n.width + ox + 2 * (2 * sx + di) + cx];
            (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4.0
        })
        .collect();
    Some((out, half))
}

/// Prints how the estimated noise compares with the noise added to crops of many raws, for the first version and for
/// several settings of the current one. Opt-in: it needs the raw corpus folder named by `LC_CORPUS`.
#[test]
#[ignore = "needs the public raw corpus: LC_CORPUS=<folder>"]
fn the_noise_estimate_follows_the_noise_added_to_many_different_pictures() {
    let dir = std::env::var("LC_CORPUS").unwrap_or_else(|_| "../../corpus/raw".into());
    let stride = env("LC_STRIDE").map_or(6, |v| v as usize).max(1);
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{dir}: {e}")).flatten().map(|e| e.path()).collect();
    files.sort();
    let probes = [
        ("v1 (old)", None),
        ("block 16, q 0.10", Some(NoiseProbe { block: 16, quantile: 0.10, correction: env("LC_C").unwrap_or(0.8) })),
        ("block 12, q 0.05", Some(NoiseProbe { block: 12, quantile: 0.05, correction: env("LC_C2").unwrap_or(0.65) })),
        ("block 24, q 0.10", Some(NoiseProbe { block: 24, quantile: 0.10, correction: env("LC_C3").unwrap_or(0.85) })),
    ];
    // sd at mid-grey 0.0062, 0.0094, 0.015, 0.038 (Canon EOS R / R6 Mark III at ISO 1600, 6400, 25600 and above)
    let levels = [("sd .0062", 2.1e-4f32, 3e-7f32), ("sd .0094", 4.9e-4, 1e-6), ("sd .0152", 1.19e-3, 1.7e-5), ("sd .0381", 6.9e-3, 2.08e-4)];
    // ratios[level][probe][where] -> estimated sd / true sd
    let mut ratios: Vec<Vec<[Vec<f32>; 2]>> = levels.iter().map(|_| probes.iter().map(|_| [Vec::new(), Vec::new()]).collect()).collect();
    let mut used = 0;
    for path in files.iter().step_by(stride) {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let Ok(raw) = lightcraft_raw::decode(&bytes) else { continue };
        if raw.cpp != 1 || !raw.cfa.as_ref().is_some_and(lightcraft_raw::Cfa::is_bayer) {
            continue;
        }
        let Ok(n) = raw.normalized() else { continue };
        let Some((mosaic, side)) = binned_crop(&n, 2048) else { continue };
        used += 1;
        for (li, &(_, a, b)) in levels.iter().enumerate() {
            let mut rng = Rng(11 + li as u64);
            let noisy: Vec<f32> = mosaic.iter().map(|&v| v + rng.normal() * (a * v.max(0.0) + b).sqrt()).collect();
            for (pi, (_, probe)) in probes.iter().enumerate() {
                let (ea, eb) = match probe {
                    None => estimate_noise_v1(&noisy, side, side),
                    Some(p) => estimate_noise_with(&noisy, side, side, p),
                };
                for (wi, at) in [0.03f32, 0.18].into_iter().enumerate() {
                    ratios[li][pi][wi].push(((ea * at + eb) / (a * at + b)).sqrt());
                }
            }
        }
    }
    println!("{used} pictures (every {stride}th raw of {dir}), each binned to {} cells, noise added at 4 levels", 1024);
    println!("estimated sd / true sd: median [10th .. 90th percentile], share within -20 % .. +25 %");
    for (li, (name, _, _)) in levels.iter().enumerate() {
        for (wi, at) in ["at 0.03", "at 0.18"].into_iter().enumerate() {
            println!("{name} {at}:");
            for (pi, (pname, _)) in probes.iter().enumerate() {
                let mut r = ratios[li][pi][wi].clone();
                r.sort_by(f32::total_cmp);
                if r.is_empty() {
                    continue;
                }
                let q = |f: f32| r[((r.len() - 1) as f32 * f) as usize];
                let within = r.iter().filter(|&&v| (0.8..=1.25).contains(&v)).count() as f32 / r.len() as f32;
                println!("    {pname:<18} {:.2} [{:.2} .. {:.2}]   {:>3.0} % within", q(0.5), q(0.1), q(0.9), within * 100.0);
            }
        }
    }
}
