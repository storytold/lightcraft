//! Prototype X-Trans demosaicers for the benchmark. Plain, readable and slow on purpose: a variant that wins here
//! is rewritten for speed in `crates/raw/src/demosaic/xtrans.rs`.
//!
//! Every prototype runs on the white-balanced mosaic (camera RGB × gains) and returns camera RGB.

use lightcraft_raw::{Cfa, Normalized, Rgb32f};
use rayon::prelude::*;

pub struct M {
    pub w: usize,
    pub h: usize,
    /// White-balanced samples.
    pub v: Vec<f32>,
    /// Colour of each pixel (0 = R, 1 = G, 2 = B).
    pub c: Vec<u8>,
}

#[inline]
fn reflect(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let mut i = i.rem_euclid(period);
    if i >= n {
        i = period - i;
    }
    i as usize
}

impl M {
    pub fn new(n: &Normalized, cfa: &Cfa, gains: [f32; 3]) -> M {
        let (w, h) = (n.width, n.height);
        let c: Vec<u8> = (0..w * h).map(|i| cfa.color_at(i % w, i / w)).collect();
        let v = n.data.iter().zip(&c).map(|(&s, &k)| s * gains[k as usize]).collect();
        M { w, h, v, c }
    }
    #[inline]
    pub fn i(&self, x: isize, y: isize) -> usize {
        reflect(y, self.h) * self.w + reflect(x, self.w)
    }
    #[inline]
    pub fn col(&self, x: isize, y: isize) -> u8 {
        self.c[self.i(x, y)]
    }
    #[inline]
    pub fn at(&self, x: isize, y: isize) -> f32 {
        self.v[self.i(x, y)]
    }
}

/// Camera RGB from a balanced image.
pub fn unbalance(mut img: Rgb32f, gains: [f32; 3]) -> Rgb32f {
    for p in &mut img.data {
        for c in 0..3 {
            p[c] /= gains[c];
        }
    }
    img
}

/// Balanced green from the truth (oracle: isolates the error of the red/blue stage).
pub fn green_oracle(truth_cam: &Rgb32f, gains: [f32; 3]) -> Vec<f32> {
    truth_cam.data.iter().map(|p| p[1] * gains[1]).collect()
}

/// Mean of the green samples in the 3×3 neighbourhood (5×5 when there are none).
pub fn green_mean(m: &M) -> Vec<f32> {
    par_plane(m.w, m.h, |x, y| {
        let i = y * m.w + x;
        if m.c[i] == 1 {
            return m.v[i];
        }
        let (x, y) = (x as isize, y as isize);
        for r in [1isize, 2] {
            let (mut s, mut n) = (0.0, 0.0);
            for dy in -r..=r {
                for dx in -r..=r {
                    if m.col(x + dx, y + dy) == 1 {
                        s += m.at(x + dx, y + dy);
                        n += 1.0;
                    }
                }
            }
            if n > 0.0 {
                return s / n;
            }
        }
        m.v[i]
    })
}

pub const DIRS: [(isize, isize); 4] = [(1, 0), (0, 1), (1, 1), (1, -1)];

/// Green at non-green sites from four directional candidates (linear interpolation between the nearest green on
/// each side along the line), weighted by `1 / (ε + activity)²`, where activity is the mean absolute difference of
/// same-colour neighbours along that direction in the 5×5 window.
pub fn green_dir(m: &M, eps: f32) -> Vec<f32> {
    par_plane(m.w, m.h, |x, y| {
        let i = y * m.w + x;
        if m.c[i] == 1 {
            return m.v[i];
        }
        let (x, y) = (x as isize, y as isize);
        let (mut s, mut ws) = (0.0f32, 0.0f32);
        for &(dx, dy) in &DIRS {
            let Some(cand) = dir_candidate(m, x, y, dx, dy) else { continue };
            let a = activity(m, x, y, dx, dy, 2);
            let wt = 1.0 / (eps + a).powi(2);
            s += wt * cand;
            ws += wt;
        }
        if ws > 0.0 { s / ws } else { m.v[i] }
    })
}

/// An image computed pixel by pixel, rows in parallel.
pub fn par_image(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 3] + Sync) -> Rgb32f {
    let mut img = Rgb32f::new(w, h);
    img.data.par_chunks_mut(w.max(1)).enumerate().for_each(|(y, row)| row.iter_mut().enumerate().for_each(|(x, p)| *p = f(x, y)));
    img
}

/// A plane computed pixel by pixel, rows in parallel.
pub fn par_plane(w: usize, h: usize, f: impl Fn(usize, usize) -> f32 + Sync) -> Vec<f32> {
    let mut v = vec![0f32; w * h];
    v.par_chunks_mut(w.max(1)).enumerate().for_each(|(y, row)| row.iter_mut().enumerate().for_each(|(x, p)| *p = f(x, y)));
    v
}

/// Linear interpolation at (x, y) between the nearest green on each side along (dx, dy), up to 3 steps out.
pub fn dir_candidate(m: &M, x: isize, y: isize, dx: isize, dy: isize) -> Option<f32> {
    let find = |sgn: isize| (1..=3).find(|&k| m.col(x + sgn * k * dx, y + sgn * k * dy) == 1);
    let (kp, kn) = (find(1)?, find(-1)?);
    let (gp, gn) = (m.at(x + kp * dx, y + kp * dy), m.at(x - kn * dx, y - kn * dy));
    Some((gp * kn as f32 + gn * kp as f32) / (kp + kn) as f32)
}

/// Mean |v(q + d) − v(q)| over same-colour pairs with q in the (2r+1)² window.
pub fn activity(m: &M, x: isize, y: isize, dx: isize, dy: isize, r: isize) -> f32 {
    let (mut s, mut n) = (0.0f32, 0.0f32);
    for qy in y - r..=y + r {
        for qx in x - r..=x + r {
            if m.col(qx, qy) == m.col(qx + dx, qy + dy) {
                s += (m.at(qx + dx, qy + dy) - m.at(qx, qy)).abs();
                n += 1.0;
            }
        }
    }
    if n > 0.0 { s / n } else { 0.0 }
}

/// Red and blue by colour-difference interpolation: `C = G + Σ w (C − G) / Σ w` over same-colour samples in the
/// (2r+1)² window, `w = 1 / d²`, times `1 / (ε + |G(q) − G(p)|)` when `edge`.
pub fn rb_cd(m: &M, g: &[f32], r: isize, edge: bool) -> Rgb32f {
    let gi = |x: isize, y: isize| g[m.i(x, y)];
    par_image(m.w, m.h, |x, y| {
        let (x, y) = (x as isize, y as isize);
        let i = m.i(x, y);
        let gp = g[i];
        let mut px = [0.0, gp, 0.0];
        for c in [0u8, 2] {
            if m.c[i] == c {
                px[c as usize] = m.v[i];
                continue;
            }
            let (mut s, mut ws) = (0.0f32, 0.0f32);
            for dy in -r..=r {
                for dx in -r..=r {
                    if m.col(x + dx, y + dy) != c {
                        continue;
                    }
                    let gq = gi(x + dx, y + dy);
                    let mut wt = 1.0 / (dx * dx + dy * dy) as f32;
                    if edge {
                        wt /= 1e-3 + (gq - gp).abs();
                    }
                    s += wt * (m.at(x + dx, y + dy) - gq);
                    ws += wt;
                }
            }
            px[c as usize] = gp + if ws > 0.0 { s / ws } else { 0.0 };
        }
        px
    })
}

/// `rb_cd` with tunable weights: `w = 1 / d^dp × 1 / (ε + |G(q) − G(p)| / (G(p) + 0.01 when rel))^p`.
pub fn rb_cd2(m: &M, g: &[f32], r: isize, dp: f32, p: f32, eps: f32, rel: bool) -> Rgb32f {
    par_image(m.w, m.h, |x, y| {
        let (x, y) = (x as isize, y as isize);
        let i = m.i(x, y);
        let gp = g[i];
        let norm = if rel { 1.0 / (gp.max(0.0) + 0.01) } else { 1.0 };
        let mut px = [0.0, gp, 0.0];
        for c in [0u8, 2] {
            if m.c[i] == c {
                px[c as usize] = m.v[i];
                continue;
            }
            let (mut s, mut ws) = (0.0f32, 0.0f32);
            for dy in -r..=r {
                for dx in -r..=r {
                    let j = m.i(x + dx, y + dy);
                    if m.c[j] != c {
                        continue;
                    }
                    let d2 = (dx * dx + dy * dy) as f32;
                    let wt = d2.powf(-dp / 2.0) * (eps + (g[j] - gp).abs() * norm).powf(-p);
                    s += wt * (m.v[j] - g[j]);
                    ws += wt;
                }
            }
            px[c as usize] = gp + if ws > 0.0 { s / ws } else { 0.0 };
        }
        px
    })
}

/// Residual interpolation of red and blue guided by green: per colour, a local linear model `C ≈ a G + b` fitted
/// over the colour's samples in the (2ρ+1)² window (ridge `λ` on `a`), its coefficients averaged over the window
/// (guided filter), then the residual `C − (a G + b)` at the samples interpolated by `rb_cd`-style weights.
pub fn rb_ri(m: &M, g: &[f32], rho: isize, lambda: f32) -> Rgb32f {
    rb_ri2(m, g, rho, lambda, f32::INFINITY, 2)
}

/// `rb_ri` with the slope clamped to `[1 / amax, amax]` and a residual window of radius `rr`.
pub fn rb_ri2(m: &M, g: &[f32], rho: isize, lambda: f32, amax: f32, rr: isize) -> Rgb32f {
    let (w, h) = (m.w, m.h);
    let mut out = Rgb32f::new(w, h);
    for (k, c) in [0u8, 2].into_iter().enumerate() {
        // per-pixel model from the window's samples of colour c
        let ab: Vec<[f32; 2]> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = ((i % w) as isize, (i / w) as isize);
                let (mut n, mut sg, mut sc, mut sgg, mut sgc) = (0f32, 0f32, 0f32, 0f32, 0f32);
                for dy in -rho..=rho {
                    for dx in -rho..=rho {
                        let j = m.i(x + dx, y + dy);
                        if m.c[j] == c {
                            let (gv, cv) = (g[j], m.v[j]);
                            n += 1.0;
                            sg += gv;
                            sc += cv;
                            sgg += gv * gv;
                            sgc += gv * cv;
                        }
                    }
                }
                if n == 0.0 {
                    return [1.0, 0.0];
                }
                let (mg, mc) = (sg / n, sc / n);
                let var = sgg / n - mg * mg;
                let cov = sgc / n - mg * mc;
                let a = ((cov + lambda) / (var + lambda)).clamp(1.0 / amax, amax);
                [a, mc - a * mg]
            })
            .collect();
        // averaged coefficients → tentative estimate
        let tent: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = ((i % w) as isize, (i / w) as isize);
                let (mut sa, mut sb, mut n) = (0f32, 0f32, 0f32);
                for dy in -rho..=rho {
                    for dx in -rho..=rho {
                        let j = m.i(x + dx, y + dy);
                        sa += ab[j][0];
                        sb += ab[j][1];
                        n += 1.0;
                    }
                }
                sa / n * g[i] + sb / n
            })
            .collect();
        // residual interpolation (edge-aware, as rb_cd)
        let plane: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                if m.c[i] == c {
                    return m.v[i];
                }
                let (x, y) = ((i % w) as isize, (i / w) as isize);
                let gp = g[i];
                let (mut s, mut ws) = (0f32, 0f32);
                for dy in -rr..=rr {
                    for dx in -rr..=rr {
                        let j = m.i(x + dx, y + dy);
                        if m.c[j] != c {
                            continue;
                        }
                        let wt = 1.0 / ((dx * dx + dy * dy) as f32 * (1e-3 + (g[j] - gp).abs()));
                        s += wt * (m.v[j] - tent[j]);
                        ws += wt;
                    }
                }
                tent[i] + if ws > 0.0 { s / ws } else { 0.0 }
            })
            .collect();
        for (p, v) in out.data.iter_mut().zip(&plane) {
            p[[0, 2][k]] = *v;
        }
    }
    for (p, &gv) in out.data.iter_mut().zip(g) {
        p[1] = gv;
    }
    out
}
