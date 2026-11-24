//! Camera-matching looks: fit the camera's own JPEG rendering from a person's raw + JPEG pairs.
//!
//! Each pair gives colour samples: the app's render of the raw (display-encoded RGB, 0..1) and the
//! camera's JPEG of the same frame at the same place. [`fit`] turns them into a 3D look table that,
//! applied after the app's render, brings it to the camera's tone and colour:
//!
//! 1. a monotone tone curve per channel (binned means, made monotone by pooling adjacent
//!    violators), which carries the camera's contrast and white balance shift;
//! 2. a residual 3D grid for the colour the curves can't carry (hue twists, saturation),
//!    accumulated with trilinear weights, shrunk towards zero where samples are few and spread
//!    into the empty nodes from their neighbours, so the table stays smooth.
//!
//! Nothing here is derived from another app: the samples come from the person's own photos at run
//! time and nothing of them is kept besides the fitted table.
//!
//! Sources: own design (pool-adjacent-violators: Ayer et al. 1955; the `.cube` layout: the Cube
//! LUT specification 1.0, red fastest).

/// Grid points per axis of the fitted table.
pub const GRID: usize = 17;

/// Tone-curve bins per channel.
const BINS: usize = 64;

/// Most samples used; more are thinned evenly.
pub const MAX_SAMPLES: usize = 2_000_000;

/// Fewest usable samples a fit accepts.
pub const MIN_SAMPLES: usize = 64;

/// A fitted camera look.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraLook {
    /// `GRID³` output colours, red changing fastest (the `.cube` order).
    pub table: Vec<[f32; 3]>,
    /// Mean absolute RGB difference (0..255) between render and JPEG before the look.
    pub error_before: f32,
    /// The same after the look.
    pub error_after: f32,
    /// Samples the fit used.
    pub samples: usize,
}

impl CameraLook {
    /// The look as `.cube` text titled `title`.
    pub fn to_cube(&self, title: &str) -> String {
        let clean: String = title.chars().filter(|c| *c != '"' && !c.is_control()).collect();
        let mut s = format!("TITLE \"{clean}\"\nLUT_3D_SIZE {GRID}\n");
        for c in &self.table {
            s.push_str(&format!("{:.6} {:.6} {:.6}\n", c[0], c[1], c[2]));
        }
        s
    }

    /// Apply the look to one display-encoded colour (trilinear).
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        apply_table(&self.table, c)
    }
}

fn finite01(c: [f32; 3]) -> bool {
    c.iter().all(|v| v.is_finite())
}

fn clamp01(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

fn node(i: usize, j: usize, k: usize) -> usize {
    i + GRID * (j + GRID * k)
}

fn apply_table(t: &[[f32; 3]], c: [f32; 3]) -> [f32; 3] {
    let n = (GRID - 1) as f32;
    let pos = |v: f32| {
        let x = clamp01(v) * n;
        let i = (x.floor() as usize).min(GRID - 2);
        (i, x - i as f32)
    };
    let ((r0, fr), (g0, fg), (b0, fb)) = (pos(c[0]), pos(c[1]), pos(c[2]));
    let mut out = [0.0f32; 3];
    for (dk, wb) in [(0, 1.0 - fb), (1, fb)] {
        for (dj, wg) in [(0, 1.0 - fg), (1, fg)] {
            for (di, wr) in [(0, 1.0 - fr), (1, fr)] {
                let w = wr * wg * wb;
                if let Some(v) = t.get(node(r0 + di, g0 + dj, b0 + dk)) {
                    for ch in 0..3 {
                        out[ch] += w * v[ch];
                    }
                }
            }
        }
    }
    out
}

/// A monotone curve on 0..1, `BINS + 1` points.
fn fit_curve(samples: &[([f32; 3], [f32; 3])], ch: usize) -> Vec<f32> {
    let mut sum = vec![0.0f64; BINS + 1];
    let mut cnt = vec![0.0f64; BINS + 1];
    for (s, t) in samples {
        let x = f64::from(clamp01(s[ch])) * BINS as f64;
        let i = (x.round() as usize).min(BINS);
        if let (Some(a), Some(b)) = (sum.get_mut(i), cnt.get_mut(i)) {
            *a += f64::from(clamp01(t[ch]));
            *b += 1.0;
        }
    }
    // pool adjacent violators over the filled bins
    let mut blocks: Vec<(f64, f64, usize, usize)> = Vec::new(); // (mean, weight, first, last)
    for i in 0..=BINS {
        let (Some(&s), Some(&w)) = (sum.get(i), cnt.get(i)) else { continue };
        if w <= 0.0 {
            continue;
        }
        blocks.push((s / w, w, i, i));
        while blocks.len() >= 2 {
            let (Some(b), Some(a)) = (blocks.last().copied(), blocks.get(blocks.len() - 2).copied()) else { break };
            if a.0 <= b.0 {
                break;
            }
            blocks.truncate(blocks.len() - 2);
            let w = a.1 + b.1;
            blocks.push(((a.0 * a.1 + b.0 * b.1) / w, w, a.2, b.3));
        }
    }
    // knots at each block's centre; identity ends unless the samples reach them
    let mut knots: Vec<(f64, f64)> = blocks.iter().map(|b| ((b.2 + b.3) as f64 / 2.0 / BINS as f64, b.0)).collect();
    if knots.first().is_none_or(|k| k.0 > 0.0) {
        let y = knots.first().map_or(0.0, |k| k.1.min(0.0_f64.max(k.1 - k.0)));
        knots.insert(0, (0.0, y.max(0.0)));
    }
    if knots.last().is_none_or(|k| k.0 < 1.0) {
        let y = knots.last().map_or(1.0, |k| (k.1 + (1.0 - k.0)).min(1.0).max(k.1));
        knots.push((1.0, y));
    }
    (0..=BINS)
        .map(|i| {
            let x = i as f64 / BINS as f64;
            let j = knots.iter().position(|k| k.0 >= x).unwrap_or(knots.len().saturating_sub(1));
            let hi = knots.get(j).copied().unwrap_or((1.0, 1.0));
            let lo = if j == 0 { hi } else { knots.get(j - 1).copied().unwrap_or(hi) };
            let y = if hi.0 > lo.0 { lo.1 + (hi.1 - lo.1) * (x - lo.0) / (hi.0 - lo.0) } else { hi.1 };
            y.clamp(0.0, 1.0) as f32
        })
        .collect()
}

fn eval_curve(c: &[f32], v: f32) -> f32 {
    let x = clamp01(v) * BINS as f32;
    let i = (x.floor() as usize).min(BINS - 1);
    let f = x - i as f32;
    let a = c.get(i).copied().unwrap_or(v);
    let b = c.get(i + 1).copied().unwrap_or(a);
    a + (b - a) * f
}

/// Fit a look from `(render, camera JPEG)` colour samples (display-encoded, 0..1).
pub fn fit(samples: &[([f32; 3], [f32; 3])]) -> Result<CameraLook, crate::CamprofError> {
    let usable: Vec<([f32; 3], [f32; 3])> = {
        let step = samples.len().div_ceil(MAX_SAMPLES).max(1);
        samples.iter().step_by(step).filter(|(s, t)| finite01(*s) && finite01(*t)).copied().collect()
    };
    if usable.len() < MIN_SAMPLES {
        return Err(crate::CamprofError::Fit(format!("too few matching pixels ({}; at least {MIN_SAMPLES})", usable.len())));
    }
    let curves: [Vec<f32>; 3] = [fit_curve(&usable, 0), fit_curve(&usable, 1), fit_curve(&usable, 2)];
    let curve = |c: [f32; 3]| [eval_curve(&curves[0], c[0]), eval_curve(&curves[1], c[1]), eval_curve(&curves[2], c[2])];
    // residual grid
    let n3 = GRID * GRID * GRID;
    let mut acc = vec![[0.0f64; 3]; n3];
    let mut wsum = vec![0.0f64; n3];
    let n = (GRID - 1) as f32;
    for (s, t) in &usable {
        let base = curve(*s);
        let r = [clamp01(t[0]) - base[0], clamp01(t[1]) - base[1], clamp01(t[2]) - base[2]];
        let pos = |v: f32| {
            let x = clamp01(v) * n;
            let i = (x.floor() as usize).min(GRID - 2);
            (i, x - i as f32)
        };
        let ((r0, fr), (g0, fg), (b0, fb)) = (pos(s[0]), pos(s[1]), pos(s[2]));
        for (dk, wb) in [(0, 1.0 - fb), (1, fb)] {
            for (dj, wg) in [(0, 1.0 - fg), (1, fg)] {
                for (di, wr) in [(0, 1.0 - fr), (1, fr)] {
                    let w = f64::from(wr * wg * wb);
                    let k = node(r0 + di, g0 + dj, b0 + dk);
                    if let (Some(a), Some(ws)) = (acc.get_mut(k), wsum.get_mut(k)) {
                        for ch in 0..3 {
                            a[ch] += w * f64::from(r[ch]);
                        }
                        *ws += w;
                    }
                }
            }
        }
    }
    // shrink towards zero where samples are few
    const SHRINK: f64 = 4.0;
    let mut res: Vec<[f32; 3]> = acc.iter().zip(&wsum).map(|(a, &w)| a.map(|v| (v / (w + SHRINK)) as f32)).collect();
    let mut conf: Vec<f32> = wsum.iter().map(|&w| (w / (w + SHRINK)) as f32).collect();
    // spread into weak nodes from their neighbours, then smooth lightly
    for _ in 0..GRID * 2 {
        let (prev, prev_c) = (res.clone(), conf.clone());
        for k in 0..GRID {
            for j in 0..GRID {
                for i in 0..GRID {
                    let here = node(i, j, k);
                    let own = prev_c.get(here).copied().unwrap_or(0.0);
                    if own >= 0.5 {
                        continue;
                    }
                    let mut sum = [0.0f32; 3];
                    let mut w = 0.0f32;
                    for (di, dj, dk) in [(-1i32, 0, 0), (1, 0, 0), (0, -1, 0), (0, 1, 0), (0, 0, -1), (0, 0, 1)] {
                        let (a, b, c) = (i as i32 + di, j as i32 + dj, k as i32 + dk);
                        if a < 0 || b < 0 || c < 0 || a >= GRID as i32 || b >= GRID as i32 || c >= GRID as i32 {
                            continue;
                        }
                        let o = node(a as usize, b as usize, c as usize);
                        let (Some(v), Some(&cw)) = (prev.get(o), prev_c.get(o)) else { continue };
                        for ch in 0..3 {
                            sum[ch] += cw * v[ch];
                        }
                        w += cw;
                    }
                    if w > 0.0 {
                        let spread = (w / 6.0).min(1.0) * (1.0 - own);
                        if let (Some(r), Some(c)) = (res.get_mut(here), conf.get_mut(here)) {
                            for ch in 0..3 {
                                r[ch] = r[ch] * own / (own + spread).max(1e-6) + sum[ch] / w * spread / (own + spread).max(1e-6);
                            }
                            *c = (own + spread * 0.9).min(1.0);
                        }
                    }
                }
            }
        }
    }
    let mut table = Vec::with_capacity(n3);
    for k in 0..GRID {
        for j in 0..GRID {
            for i in 0..GRID {
                let c = [i as f32 / n, j as f32 / n, k as f32 / n];
                let b = curve(c);
                let r = res.get(node(i, j, k)).copied().unwrap_or([0.0; 3]);
                table.push([clamp01(b[0] + r[0]), clamp01(b[1] + r[1]), clamp01(b[2] + r[2])]);
            }
        }
    }
    let err = |f: &dyn Fn([f32; 3]) -> [f32; 3]| {
        let total: f64 = usable
            .iter()
            .map(|(s, t)| {
                let o = f(*s);
                (0..3).map(|c| f64::from((o[c] - clamp01(t[c])).abs())).sum::<f64>() / 3.0
            })
            .sum();
        (total / usable.len() as f64 * 255.0) as f32
    };
    let error_before = err(&|c| c.map(clamp01));
    let error_after = err(&|c| apply_table(&table, c));
    Ok(CameraLook { table, error_before, error_after, samples: usable.len() })
}

/// Colour samples from one pair of 8-bit RGBA images (render `a`, camera JPEG `b`) of the same frame,
/// matched by position (the sizes may differ, e.g. by a pixel of rounding), skipping clipped pixels;
/// `None` when a size is zero or a buffer is shorter than its size says.
pub fn samples_from_images(a: &[[u8; 4]], aw: usize, ah: usize, b: &[[u8; 4]], bw: usize, bh: usize) -> Option<Vec<([f32; 3], [f32; 3])>> {
    let (na, nb) = (aw.checked_mul(ah)?, bw.checked_mul(bh)?);
    if na == 0 || nb == 0 || a.len() < na || b.len() < nb {
        return None;
    }
    let step = na.div_ceil(MAX_SAMPLES).max(1);
    let f = |p: &[u8; 4]| [f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0];
    let mut out = Vec::with_capacity(na / step + 1);
    for i in (0..na).step_by(step) {
        let (x, y) = (i % aw, i / aw);
        let bx = ((x as f64 + 0.5) / aw as f64 * bw as f64) as usize;
        let by = ((y as f64 + 0.5) / ah as f64 * bh as f64) as usize;
        let (Some(pa), Some(pb)) = (a.get(i), b.get(by.min(bh - 1) * bw + bx.min(bw - 1))) else { continue };
        // clipped pixels say nothing about the curve
        if pa[..3].contains(&255) || pb[..3].contains(&255) {
            continue;
        }
        out.push((f(pa), f(pb)));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up "camera rendering": contrastier, warmer, with a hue twist in the greens.
    fn camera(c: [f32; 3]) -> [f32; 3] {
        let s = |v: f32| (v * v * (3.0 - 2.0 * v)) * 0.7 + v * 0.3;
        let (r, g, b) = (s(c[0]), s(c[1]), s(c[2]));
        let g2 = g * 0.9 + b * 0.1;
        [(r * 1.05).min(1.0), g2, (b * 0.92).min(1.0)]
    }

    fn pseudo(i: usize) -> f32 {
        ((i as u64).wrapping_mul(2654435761) % 10007) as f32 / 10007.0
    }

    fn synthetic(n: usize) -> Vec<([f32; 3], [f32; 3])> {
        (0..n)
            .map(|i| {
                let s = [pseudo(i * 3), pseudo(i * 3 + 1), pseudo(i * 3 + 2)];
                (s, camera(s))
            })
            .collect()
    }

    #[test]
    fn a_fit_reproduces_a_synthetic_camera_look() {
        let look = fit(&synthetic(60_000)).unwrap();
        assert!(look.error_before > 5.0, "{look:?}");
        assert!(look.error_after < look.error_before * 0.2, "before {} after {}", look.error_before, look.error_after);
        // held-out colours
        let mut worst = 0.0f32;
        for i in 0..500 {
            let s = [pseudo(i * 7 + 100_001), pseudo(i * 7 + 100_002), pseudo(i * 7 + 100_003)];
            let (o, t) = (look.apply(s), camera(s));
            worst = worst.max((0..3).map(|c| (o[c] - t[c]).abs()).fold(0.0, f32::max));
        }
        assert!(worst < 0.06, "worst held-out difference {worst}");
    }

    #[test]
    fn identical_pairs_give_a_neutral_look() {
        let s: Vec<_> = synthetic(20_000).into_iter().map(|(a, _)| (a, a)).collect();
        let look = fit(&s).unwrap();
        assert!(look.error_after < 1.5, "{}", look.error_after);
        let g = look.apply([0.4, 0.5, 0.6]);
        assert!((g[0] - 0.4).abs() < 0.02 && (g[1] - 0.5).abs() < 0.02 && (g[2] - 0.6).abs() < 0.02, "{g:?}");
    }

    #[test]
    fn a_narrow_sample_set_still_gives_a_smooth_full_table() {
        // only dark greys: the rest of the cube must stay finite, in range and ordered
        let s: Vec<_> = (0..5_000).map(|i| pseudo(i) * 0.3).map(|v| ([v, v, v], [v * 0.8, v * 0.8, v * 0.8])).collect();
        let look = fit(&s).unwrap();
        assert!(look.table.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        let (lo, hi) = (look.apply([0.5, 0.5, 0.5]), look.apply([0.9, 0.9, 0.9]));
        assert!(hi[1] > lo[1], "{lo:?} {hi:?}");
    }

    #[test]
    fn hostile_samples_are_refused_or_ignored_never_a_panic() {
        assert!(fit(&[]).is_err());
        let nan = [f32::NAN; 3];
        let inf = [f32::INFINITY, -1.0, 7.0];
        assert!(fit(&vec![(nan, nan); 1000]).is_err());
        let mut mixed = synthetic(2000);
        mixed.extend(std::iter::repeat_n((inf, nan), 500));
        mixed.extend(std::iter::repeat_n(([-5.0, 9.0, 1e30], [2.0, -2.0, 0.5]), 500));
        let look = fit(&mixed).unwrap();
        assert!(look.table.iter().flatten().all(|v| v.is_finite()));
        assert!(look.apply([f32::NAN, f32::INFINITY, -3.0]).iter().all(|v| v.is_finite()));
        assert!(samples_from_images(&[[0; 4]; 2], 2, 2, &[[0; 4]; 4], 2, 2).is_none());
        assert!(samples_from_images(&[], 0, 0, &[], 0, 0).is_none());
        assert!(samples_from_images(&[], usize::MAX, 3, &[[0; 4]], 1, 1).is_none());
        // a JPEG a pixel off in size still pairs every render pixel
        let got = samples_from_images(&[[10, 20, 30, 255]; 12], 4, 3, &[[11, 21, 31, 255]; 15], 5, 3).unwrap();
        assert_eq!(got.len(), 12);
    }

    #[test]
    fn the_cube_text_has_the_grid_and_a_clean_title() {
        let look = fit(&synthetic(5000)).unwrap();
        let t = look.to_cube("My \"Cam\"\n");
        assert!(t.starts_with("TITLE \"My Cam\"\n"));
        assert_eq!(t.lines().filter(|l| l.starts_with(|c: char| c.is_ascii_digit())).count(), GRID * GRID * GRID);
    }
}
