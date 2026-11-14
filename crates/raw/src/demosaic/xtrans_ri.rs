//! X-Trans demosaicing: classified linear green, residual-interpolated red and blue.
//!
//! Everything runs on the white-balanced mosaic (the as-shot multipliers): balanced, a colour edge is mostly a
//! luminance edge, and red, green and blue samples are interchangeable estimates of it.
//!
//! 1. **First-pass green** at red and blue sites: four directional candidates (linear interpolation between the
//!    nearest green on each side along the horizontal, vertical and both diagonals), each weighted by
//!    `1 / (ε + activity)²`, where activity is the mean absolute difference of same-colour pairs one step apart
//!    along that direction in the 5 × 5 window.
//! 2. **Class** of every pixel from the structure tensor of the first-pass green (5 × 5 Gaussian, σ = 1.2): the
//!    gradient angle in 8 bins, its relative strength (`√λ₁ / local mean`) in 3 and its coherence in 3 (72 classes).
//! 3. **Green** at red and blue sites: a 7 × 7 linear filter over the mosaic, one per pattern position and class
//!    ([`super::xtrans_taps`]). The filters are least-squares (LMMSE) fits on ground truth made from real X-Trans
//!    raws (raw.pixls.us, CC0) by binning each 3 × 3 block of the pattern into one full-colour pixel; they are
//!    constrained to reproduce flat colour exactly. `xtrans_bench` (`XB_WRITE`) regenerates them.
//! 4. **Red and blue** by residual interpolation (Kiku, Monno, Tanaka & Okutomi, 2013): a local linear model
//!    `C ≈ a G + b` fitted to the colour's samples in the 7 × 7 window (a guided filter, its slope pulled toward 1
//!    by `λ` and kept non-negative), averaged over the window; the residual `C − (a G + b)` at the samples is
//!    interpolated with weights `1 / (d² (ε + |G(q) − G(p)|))`.
//!
//! The image is processed in tiles with a 16-pixel halo. Beyond the image, samples repeat from the nearest pixel at
//! the same pattern position (whole 6-pixel periods), so the pattern stays intact up to the border.

use super::Mosaic;
use super::xtrans_taps::{COHERENCE, STRENGTH, TAPS};
use crate::{Cfa, Rgb32f};
use rayon::prelude::*;
use std::ops::Range;

/// Output tile side.
const TILE: usize = 160;
/// Input margin a tile needs (residual interpolation 3 + guided filter 3 + 3, green 6).
const HALO: usize = 16;
/// Side of a tile's buffers.
const SIDE: usize = TILE + 2 * HALO;
const EPS: f32 = 1e-3;
/// Radius of the residual interpolation's window.
const RESIDUAL: usize = 3;
/// Ridge of the residual-interpolation slope (toward 1).
const LAMBDA: f32 = 1e-5;
pub(super) const CLASSES: usize = 72;
/// Taps of a green filter (7 × 7).
pub(super) const TAPS_PER_FILTER: usize = 49;
/// Red/blue positions of the pattern up to its (3, 3) shift, which maps the pattern onto itself.
pub(super) const POSITIONS: usize = 8;
/// The four interpolation directions.
const DIRS: [(isize, isize); 4] = [(1, 0), (0, 1), (1, 1), (1, -1)];

/// The canonical X-Trans phase of every 6 × 6 position (colour, and the filter slot of red/blue positions).
pub(super) struct Pattern {
    colour: [u8; 36],
    /// Filter position (`0..POSITIONS`) of each red/blue phase; `u8::MAX` for green.
    slot: [u8; 36],
}

impl Pattern {
    pub(super) fn canonical() -> Pattern {
        let canon = Cfa::xtrans();
        let colour: [u8; 36] = std::array::from_fn(|i| canon.color_at(i % 6, i / 6));
        let mut slot = [u8::MAX; 36];
        let mut next = 0u8;
        for i in 0..36 {
            if colour[i] == 1 || slot[i] != u8::MAX {
                continue;
            }
            // the same neighbourhood three pixels down and right
            let twin = ((i / 6 + 3) % 6) * 6 + (i % 6 + 3) % 6;
            slot[i] = next;
            slot[twin] = next;
            next += 1;
        }
        Pattern { colour, slot }
    }

    /// The red/blue filter position of canonical phase `ph`, if it is red or blue.
    #[cfg(test)]
    pub(super) fn position(&self, ph: usize) -> Option<usize> {
        self.slot.get(ph).filter(|&&s| s != u8::MAX).map(|&s| s as usize)
    }
}

/// The offset `(ox, oy)` with `cfa(x, y) == canonical(x + ox, y + oy)`, if `cfa` is the X-Trans pattern.
fn alignment(cfa: &Cfa) -> Option<(usize, usize)> {
    if cfa.width != 6 || cfa.height != 6 || cfa.pattern.len() != 36 {
        return None;
    }
    let canon = Cfa::xtrans();
    (0..36).map(|k| (k % 6, k / 6)).find(|&(ox, oy)| (0..36).all(|i| cfa.color_at(i % 6, i / 6) == canon.color_at(i % 6 + ox, i / 6 + oy)))
}

/// Index of coordinate `i` in `0..n`: inside, itself; outside, the nearest one a whole number of 6-pixel periods
/// away (same pattern position). `n >= 6`.
#[inline]
fn source(i: isize, n: usize) -> usize {
    let n = n as isize;
    let i = if i < 0 {
        i + 6 * ((-i + 5) / 6)
    } else if i >= n {
        i - 6 * ((i - n) / 6 + 1)
    } else {
        i
    };
    i.clamp(0, n - 1) as usize
}

/// Per-phase neighbourhood tables in tile-buffer offsets.
struct Tables {
    colour: [u8; 36],
    slot: [u8; 36],
    /// Per phase and direction: offsets (+, −) of the nearest green and their distances, if both exist.
    cand: Vec<[Option<(isize, isize, f32, f32)>; 4]>,
    /// Per phase and direction: 1 / the number of same-colour pairs (q, q + d) with q in the 5 × 5 window (0 for
    /// none): the activity is their mean absolute difference.
    inv_pairs: Vec<[f32; 4]>,
    /// Per phase, colour 0 (red) / 1 (blue): 1 / the number of its samples in the 7 × 7 window (0 for none).
    inv_count: Vec<[f32; 2]>,
    /// Per phase, colour 0 / 1: the samples within `RESIDUAL` of the pixel (offset, 1 / d²).
    near: Vec<[Vec<(isize, f32)>; 2]>,
}

impl Tables {
    fn new(p: &Pattern) -> Tables {
        let s = SIDE as isize;
        let col = |px: usize, py: usize, dx: isize, dy: isize| {
            let (x, y) = ((px as isize + dx).rem_euclid(6) as usize, (py as isize + dy).rem_euclid(6) as usize);
            p.colour[y * 6 + x]
        };
        let mut cand = Vec::with_capacity(36);
        let mut inv_pairs = Vec::with_capacity(36);
        let mut inv_count = Vec::with_capacity(36);
        let mut near = Vec::with_capacity(36);
        for ph in 0..36 {
            let (px, py) = (ph % 6, ph / 6);
            cand.push(DIRS.map(|(dx, dy)| {
                let find = |sgn: isize| (1..=3isize).find(|&k| col(px, py, sgn * k * dx, sgn * k * dy) == 1);
                let (kp, kn) = (find(1)?, find(-1)?);
                Some(((kp * dy) * s + kp * dx, -(kn * dy) * s - kn * dx, kn as f32 / (kp + kn) as f32, kp as f32 / (kp + kn) as f32))
            }));
            inv_pairs.push(DIRS.map(|(dx, dy)| {
                let count = (-2..=2isize)
                    .flat_map(|qy| (-2..=2isize).map(move |qx| (qx, qy)))
                    .filter(|&(qx, qy)| col(px, py, qx, qy) == col(px, py, qx + dx, qy + dy))
                    .count();
                if count == 0 { 0.0 } else { 1.0 / count as f32 }
            }));
            inv_count.push([0u8, 2].map(|c| {
                let count = (-3..=3isize).flat_map(|dy| (-3..=3isize).map(move |dx| (dx, dy))).filter(|&(dx, dy)| col(px, py, dx, dy) == c).count();
                if count == 0 { 0.0 } else { 1.0 / count as f32 }
            }));
            near.push([0u8, 2].map(|c| {
                let r = RESIDUAL as isize;
                let mut v = Vec::new();
                for dy in -r..=r {
                    for dx in -r..=r {
                        if (dx, dy) != (0, 0) && col(px, py, dx, dy) == c {
                            v.push((dy * s + dx, 1.0 / (dx * dx + dy * dy) as f32));
                        }
                    }
                }
                v
            }));
        }
        Tables { colour: p.colour, slot: p.slot, cand, inv_pairs, inv_count, near }
    }
}

/// Demosaic an X-Trans mosaic with white-balance `gains` (camera → balanced; ignored unless finite and positive).
/// `None` when the pattern isn't X-Trans (in any phase) or the image is smaller than 24 × 24: callers fall back.
pub(crate) fn demosaic(m: &Mosaic, gains: Option<[f32; 3]>) -> Option<Rgb32f> {
    let (w, h) = (m.w, m.h);
    if w < 24 || h < 24 || m.data.len() < w.checked_mul(h)? || TAPS.len() != POSITIONS * CLASSES * TAPS_PER_FILTER {
        return None;
    }
    let (ox, oy) = alignment(m.cfa)?;
    let gains = gains.filter(|g| g.iter().all(|v| v.is_finite() && *v > 0.0)).map_or([1.0; 3], |g| g.map(|v| v / g[1]));
    let t = Tables::new(&Pattern::canonical());
    let (tiles_x, tiles_y) = (w.div_ceil(TILE), h.div_ceil(TILE));
    // every tile in parallel, each worker reusing one set of buffers
    let tiles: Vec<Vec<[f32; 3]>> = (0..tiles_x * tiles_y)
        .into_par_iter()
        .map_init(Scratch::new, |s, k| {
            tile(m, &t, gains, (ox, oy), (k % tiles_x) * TILE, (k / tiles_x) * TILE, s);
            s.out.clone()
        })
        .collect();
    let mut out = Rgb32f::new(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (ty, r) = (y / TILE, y % TILE);
        for tx in 0..tiles_x {
            let x0 = tx * TILE;
            let cols = TILE.min(w - x0);
            if let (Some(dst), Some(src)) = (row.get_mut(x0..x0 + cols), tiles.get(ty * tiles_x + tx).and_then(|b| b.get(r * TILE..r * TILE + cols)))
            {
                dst.copy_from_slice(src);
            }
        }
    });
    Some(out)
}

/// A tile's buffers (`SIDE × SIDE` planes), reused from tile to tile.
struct Scratch {
    phase: Vec<u8>,
    class: Vec<u8>,
    /// Balanced mosaic.
    v: Vec<f32>,
    /// Camera samples.
    raw: Vec<f32>,
    /// First-pass green.
    gd: Vec<f32>,
    /// Green.
    g: Vec<f32>,
    red: Vec<f32>,
    blue: Vec<f32>,
    /// Intermediate of the separable filters.
    tmp: Vec<f32>,
    /// Work planes, reused by every stage.
    w: [Vec<f32>; 9],
    /// The tile's output.
    out: Vec<[f32; 3]>,
}

impl Scratch {
    fn new() -> Scratch {
        let n = SIDE * SIDE;
        let plane = || vec![0f32; n];
        Scratch {
            phase: vec![0; n],
            class: vec![0; n],
            v: plane(),
            raw: plane(),
            gd: plane(),
            g: plane(),
            red: plane(),
            blue: plane(),
            tmp: plane(),
            w: std::array::from_fn(|_| plane()),
            out: vec![[0.0; 3]; TILE * TILE],
        }
    }
}

/// `dst = Σₖ w[k] · src shifted by (k − r) · step` over rows `ys` and columns `xs` (`step` 1: horizontal, `SIDE`:
/// vertical; `r = N / 2`). Each output pixel sums its taps in one pass over equal-length row slices, which the
/// compiler vectorises.
fn convolve<const N: usize>(src: &[f32], dst: &mut [f32], ys: Range<usize>, xs: Range<usize>, w: &[f32; N], step: usize) {
    let r = N / 2;
    let len = xs.len();
    for y in ys {
        let o = y * SIDE + xs.start;
        let (Some(d), Some(base)) = (dst.get_mut(o..o + len), o.checked_sub(r * step)) else { continue };
        let taps: [&[f32]; N] = std::array::from_fn(|k| src.get(base + k * step..base + k * step + len).unwrap_or(&[]));
        if taps.iter().any(|t| t.len() != len) {
            continue;
        }
        for (x, out) in d.iter_mut().enumerate() {
            let mut acc = 0f32;
            for (wk, t) in w.iter().zip(&taps) {
                acc += wk * t[x];
            }
            *out = acc;
        }
    }
}

/// One output tile at `(x0, y0)` into `s.out` (`TILE × TILE`, row-major, cropped by the caller at the image edge).
fn tile(m: &Mosaic, t: &Tables, gains: [f32; 3], (ox, oy): (usize, usize), x0: usize, y0: usize, s: &mut Scratch) {
    let (w, h) = (m.w, m.h);
    let n = SIDE * SIDE;
    let at = |i: usize, d: isize| (i as isize + d) as usize;
    let ones5 = [1f32; 5];
    let Scratch { phase, class, v, raw, gd, g, tmp, w: work, .. } = s;
    let [w0, w1, w2, w3, w4, w5, w6, ..] = work;

    // 1. canonical phase and balanced mosaic of every buffer pixel, with its camera sample kept for the output
    let (px0, py0) = ((x0 + ox + 6 * HALO - HALO) % 6, (y0 + oy + 6 * HALO - HALO) % 6);
    for by in 0..SIDE {
        let sy = source(y0 as isize - HALO as isize + by as isize, h);
        let row = ((py0 + by) % 6) * 6;
        for bx in 0..SIDE {
            let sx = source(x0 as isize - HALO as isize + bx as isize, w);
            let sample = m.data.get(sy * w + sx).copied().unwrap_or(0.0);
            let p = row + (px0 + bx) % 6;
            let i = by * SIDE + bx;
            phase[i] = p as u8;
            raw[i] = sample;
            v[i] = sample * gains[t.colour[p] as usize];
        }
    }
    let colour = |i: usize| t.colour[phase[i] as usize];

    // 2. first-pass green, valid on [3, SIDE − 3): per direction, the activity is a 5 × 5 box sum of same-colour
    // differences one step along it
    let (diff, act, sum, wsum) = (&mut *w0, &mut *w1, &mut *w2, &mut *w3);
    sum.fill(0.0);
    wsum.fill(0.0);
    for (d, &(dx, dy)) in DIRS.iter().enumerate() {
        let step = dy * SIDE as isize + dx;
        for by in 1..SIDE - 1 {
            for bx in 1..SIDE - 1 {
                let i = by * SIDE + bx;
                let j = at(i, step);
                diff[i] = if t.colour[phase[i] as usize] == t.colour[phase[j] as usize] { (v[j] - v[i]).abs() } else { 0.0 };
            }
        }
        convolve(diff, tmp, 1..SIDE - 1, 3..SIDE - 3, &ones5, 1);
        convolve(tmp, act, 3..SIDE - 3, 3..SIDE - 3, &ones5, SIDE);
        for by in 3..SIDE - 3 {
            for bx in 3..SIDE - 3 {
                let i = by * SIDE + bx;
                let p = phase[i] as usize;
                if t.colour[p] == 1 {
                    continue;
                }
                let Some((op, on, wp, wn)) = t.cand[p][d] else { continue };
                let a = act[i] * t.inv_pairs[p][d];
                let wt = 1.0 / ((EPS + a) * (EPS + a));
                sum[i] += wt * (v[at(i, op)] * wp + v[at(i, on)] * wn);
                wsum[i] += wt;
            }
        }
    }
    gd.copy_from_slice(v);
    for i in 3 * SIDE..n - 3 * SIDE {
        if colour(i) != 1 && wsum[i] > 0.0 {
            gd[i] = sum[i] / wsum[i];
        }
    }

    // 3. structure tensor of the first-pass green (gradients on [4, SIDE − 4), Gaussian-smoothed on [6, SIDE − 6))
    // → class of every red/blue pixel
    let gauss: [f32; 5] = std::array::from_fn(|k| {
        let d = k as f32 - 2.0;
        (-d * d / (2.0 * 1.44)).exp()
    });
    let norm = 1.0 / (gauss.iter().sum::<f32>() * gauss.iter().sum::<f32>());
    let (gxx, gyy, gxy) = (&mut *w0, &mut *w1, &mut *w2);
    for by in 4..SIDE - 4 {
        for bx in 4..SIDE - 4 {
            let i = by * SIDE + bx;
            let gx = 0.5 * (gd[i + 1] - gd[i - 1]);
            let gy = 0.5 * (gd[i + SIDE] - gd[i - SIDE]);
            gxx[i] = gx * gx;
            gyy[i] = gy * gy;
            gxy[i] = gx * gy;
        }
    }
    let (sa, sb, sc, sm) = (&mut *w3, &mut *w4, &mut *w5, &mut *w6);
    for (src, dst) in [(&*gxx, &mut *sa), (&*gyy, &mut *sb), (&*gxy, &mut *sc), (&*gd, &mut *sm)] {
        convolve(src, tmp, 4..SIDE - 4, 6..SIDE - 6, &gauss, 1);
        convolve(tmp, dst, 6..SIDE - 6, 6..SIDE - 6, &gauss, SIDE);
    }
    for by in 6..SIDE - 6 {
        for bx in 6..SIDE - 6 {
            let i = by * SIDE + bx;
            if colour(i) != 1 {
                class[i] = classify(sa[i] * norm, sb[i] * norm, sc[i] * norm, sm[i] * norm) as u8;
            }
        }
    }

    // 4. green, valid on [6, SIDE − 6)
    g.copy_from_slice(v);
    for by in 6..SIDE - 6 {
        for bx in 6..SIDE - 6 {
            let i = by * SIDE + bx;
            let pos = t.slot[phase[i] as usize];
            if pos == u8::MAX {
                continue;
            }
            let k = (pos as usize * CLASSES + class[i] as usize) * TAPS_PER_FILTER;
            let Some(taps) = TAPS.get(k..k + TAPS_PER_FILTER) else { continue };
            let mut acc = 0f32;
            for (dy, row_taps) in taps.as_chunks::<7>().0.iter().enumerate() {
                let row = i + dy * SIDE - 3 * SIDE - 3;
                for (tap, val) in row_taps.iter().zip(&v[row..row + 7]) {
                    acc += tap * val;
                }
            }
            g[i] = acc;
        }
    }

    // 5. red and blue by residual interpolation
    for (k, c) in [0u8, 2].into_iter().enumerate() {
        s.red_blue(t, k, c);
    }

    // 6. back to camera RGB; native samples as they were
    let Scratch { phase, raw, g, red, blue, out, .. } = s;
    let inv = gains.map(|v| 1.0 / v);
    for ty in 0..TILE {
        for tx in 0..TILE {
            let i = (ty + HALO) * SIDE + tx + HALO;
            let mut px = [red[i] * inv[0], g[i] * inv[1], blue[i] * inv[2]];
            px[t.colour[phase[i] as usize] as usize] = raw[i];
            out[ty * TILE + tx] = px;
        }
    }
}

impl Scratch {
    /// Stage 5 of [`tile`] for one colour (`k` 0: red, `c` 0; `k` 1: blue, `c` 2): the plane by residual
    /// interpolation from the green and the balanced mosaic, valid on `[HALO, HALO + TILE)`.
    fn red_blue(&mut self, t: &Tables, k: usize, c: u8) {
        let n = SIDE * SIDE;
        let at = |i: usize, d: isize| (i as isize + d) as usize;
        let ones7 = [1f32; 7];
        let Scratch { phase, v, g, red, blue, tmp, w: work, .. } = self;
        let [w0, w1, w2, w3, _, w5, w6, w7, w8] = work;
        // masked sums over the colour's samples: Σg, Σc, Σg², Σgc (7 × 7 boxes, valid on [9, SIDE − 9)); their
        // count depends on the phase alone
        let mut prod = [&mut *w0, &mut *w1, &mut *w2, &mut *w3];
        for i in 6 * SIDE..n - 6 * SIDE {
            let (gv, cv) = if t.colour[phase[i] as usize] == c { (g[i], v[i]) } else { (0.0, 0.0) };
            prod[0][i] = gv;
            prod[1][i] = cv;
            prod[2][i] = gv * gv;
            prod[3][i] = gv * cv;
        }
        let mut sums = [&mut *w5, &mut *w6, &mut *w7, &mut *w8];
        for (src, dst) in prod.iter().zip(sums.iter_mut()) {
            convolve(src, tmp, 6..SIDE - 6, 9..SIDE - 9, &ones7, 1);
            convolve(tmp, dst, 9..SIDE - 9, 9..SIDE - 9, &ones7, SIDE);
        }
        // the local model a, b (into the freed product planes)
        let [pa, pb, ..] = &mut prod;
        for by in 9..SIDE - 9 {
            for bx in 9..SIDE - 9 {
                let i = by * SIDE + bx;
                let inv = t.inv_count[phase[i] as usize][k];
                (pa[i], pb[i]) = if inv > 0.0 {
                    let (mg, mc) = (sums[0][i] * inv, sums[1][i] * inv);
                    let a = ((sums[3][i] * inv - mg * mc + LAMBDA) / (sums[2][i] * inv - mg * mg + LAMBDA)).max(0.0);
                    (a, mc - a * mg)
                } else {
                    (1.0, 0.0)
                };
            }
        }
        // window means of the model (7 × 7 box), valid on [12, SIDE − 12), and the tentative estimate
        let [ma, mb, tent, ..] = &mut sums;
        convolve(pa, tmp, 9..SIDE - 9, 12..SIDE - 12, &ones7, 1);
        convolve(tmp, ma, 12..SIDE - 12, 12..SIDE - 12, &ones7, SIDE);
        convolve(pb, tmp, 9..SIDE - 9, 12..SIDE - 12, &ones7, 1);
        convolve(tmp, mb, 12..SIDE - 12, 12..SIDE - 12, &ones7, SIDE);
        for by in 12..SIDE - 12 {
            for bx in 12..SIDE - 12 {
                let i = by * SIDE + bx;
                tent[i] = (ma[i] * g[i] + mb[i]) * (1.0 / 49.0);
            }
        }
        // residual interpolation, valid on [15, SIDE − 15)
        let plane = if k == 0 { &mut *red } else { &mut *blue };
        for by in HALO..HALO + TILE {
            for bx in HALO..HALO + TILE {
                let i = by * SIDE + bx;
                let p = phase[i] as usize;
                if t.colour[p] == c {
                    plane[i] = v[i];
                    continue;
                }
                let gp = g[i];
                let (mut sr, mut ws) = (0f32, 0f32);
                for &(o, inv_d2) in &t.near[p][k] {
                    let j = at(i, o);
                    let wt = inv_d2 / (EPS + (g[j] - gp).abs());
                    sr += wt * (v[j] - tent[j]);
                    ws += wt;
                }
                plane[i] = tent[i] + if ws > 0.0 { sr / ws } else { 0.0 };
            }
        }
    }
}

/// The class (`0..CLASSES`) of a pixel from its smoothed structure tensor `[[a, c], [c, b]]` and local mean green:
/// gradient angle (8 bins of π / 8: the octant of the doubled angle) × strength × coherence.
#[inline]
pub(super) fn classify(a: f32, b: f32, c: f32, mean: f32) -> usize {
    let (u, v) = (a - b, 2.0 * c);
    let octant = if v >= 0.0 {
        if u >= 0.0 {
            usize::from(v > u)
        } else if v >= -u {
            2
        } else {
            3
        }
    } else if u < 0.0 {
        if -u >= -v { 4 } else { 5 }
    } else if -v >= u {
        6
    } else {
        7
    };
    let half = 0.5 * u;
    let root = (half * half + c * c).sqrt();
    let l1 = 0.5 * (a + b) + root;
    let l2 = (0.5 * (a + b) - root).max(0.0);
    let (s1, s2) = (l1.max(0.0).sqrt(), l2.sqrt());
    let strength = s1 / (mean.max(0.0) + 0.02);
    let coherence = (s1 - s2) / (s1 + s2 + 1e-9);
    let sb = usize::from(strength >= STRENGTH[0]) + usize::from(strength >= STRENGTH[1]);
    let kb = usize::from(coherence >= COHERENCE[0]) + usize::from(coherence >= COHERENCE[1]);
    (octant * 3 + sb) * 3 + kb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demosaic::{mosaic_from_rgb, psnr, xtrans};

    fn mosaic(n: &crate::Normalized) -> Mosaic<'_> {
        Mosaic { w: n.width, h: n.height, data: &n.data, cfa: n.cfa.as_ref().unwrap() }
    }

    /// A photo-like scene: luminance structure (disc, stripes, a diagonal edge, fine texture) over smoothly varying
    /// colour.
    fn scene(w: usize, h: usize) -> Rgb32f {
        Rgb32f::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32, y as f32);
            let d = ((fx - w as f32 * 0.35).powi(2) + (fy - h as f32 * 0.4).powi(2)).sqrt();
            let lum = if d < w as f32 * 0.18 {
                0.85
            } else if x > w * 2 / 3 && (y / 3) % 2 == 0 {
                0.15
            } else if fx + fy * 0.5 > w as f32 * 1.1 {
                0.7
            } else {
                0.35 + 0.1 * ((fx * 0.7).sin() * (fy * 0.9).cos())
            };
            [lum * (0.9 + 0.1 * fx / w as f32), lum * 0.8, lum * (0.55 + 0.3 * fy / h as f32)]
        })
    }

    #[test]
    fn pattern_positions_pair_identical_neighbourhoods() {
        let p = Pattern::canonical();
        let canon = Cfa::xtrans();
        let mut seen = [0usize; POSITIONS];
        for ph in 0..36 {
            let (x, y) = (ph % 6, ph / 6);
            match p.position(ph) {
                None => assert_eq!(canon.color_at(x, y), 1),
                Some(pos) => {
                    seen[pos] += 1;
                    // every phase of a position sees the same pattern around it
                    let twin = (0..36).find(|&q| q != ph && p.position(q) == Some(pos)).unwrap();
                    let (tx, ty) = (twin % 6, twin / 6);
                    for dy in 0..6 {
                        for dx in 0..6 {
                            assert_eq!(canon.color_at(x + dx, y + dy), canon.color_at(tx + dx, ty + dy));
                        }
                    }
                }
            }
        }
        assert_eq!(seen, [2; POSITIONS]);
    }

    #[test]
    fn every_phase_of_the_pattern_is_recognised_and_other_patterns_are_not() {
        for dy in 0..6 {
            for dx in 0..6 {
                let cfa = Cfa::xtrans().shifted(dx, dy);
                let (ox, oy) = alignment(&cfa).unwrap();
                for i in 0..36 {
                    assert_eq!(cfa.color_at(i % 6, i / 6), Cfa::xtrans().color_at(i % 6 + ox, i / 6 + oy));
                }
            }
        }
        let mut odd = Cfa::xtrans();
        odd.pattern[0] = 0;
        assert!(alignment(&odd).is_none());
        assert!(alignment(&Cfa::bayer("RGGB").unwrap()).is_none());
    }

    #[test]
    fn flat_colour_is_exact_in_every_phase_and_balance() {
        for (shift, gains) in [((0, 0), None), ((1, 2), Some([1.9, 1.0, 1.6])), ((5, 3), Some([1.15, 1.0, 2.8]))] {
            let cfa = Cfa::xtrans().shifted(shift.0, shift.1);
            let n = mosaic_from_rgb(&Rgb32f::filled(61, 47, [0.3, 0.55, 0.8]), &cfa);
            let out = demosaic(&mosaic(&n), gains).unwrap();
            for p in &out.data {
                for (c, want) in [0.3f32, 0.55, 0.8].into_iter().enumerate() {
                    assert!((p[c] - want).abs() < 2e-5, "{shift:?} {p:?}");
                }
            }
        }
    }

    #[test]
    fn native_samples_are_kept() {
        let cfa = Cfa::xtrans().shifted(2, 1);
        let n = mosaic_from_rgb(&scene(70, 52), &cfa);
        let out = demosaic(&mosaic(&n), Some([1.9, 1.0, 1.6])).unwrap();
        for y in 0..52 {
            for x in 0..70 {
                assert_eq!(out.get(x, y)[cfa.color_at(x, y) as usize].to_bits(), n.data[y * 70 + x].to_bits());
            }
        }
    }

    #[test]
    fn tiles_leave_no_seams() {
        // shifting the image by a whole pattern period moves every tile boundary over the content; pixels far
        // enough from the image border must not change at all
        let (w, h) = (2 * TILE + 40, TILE + 30);
        let img = scene(w + 6, h + 6);
        let cfa = Cfa::xtrans();
        let a = demosaic(&mosaic(&mosaic_from_rgb(&img, &cfa)), Some([1.4, 1.0, 1.7])).unwrap();
        let crop = Rgb32f::from_fn(w, h, |x, y| img.get(x + 6, y + 6));
        let b = demosaic(&mosaic(&mosaic_from_rgb(&crop, &cfa)), Some([1.4, 1.0, 1.7])).unwrap();
        for y in HALO..h - HALO {
            for x in HALO..w - HALO {
                assert_eq!(a.get(x + 6, y + 6).map(f32::to_bits), b.get(x, y).map(f32::to_bits), "({x}, {y})");
            }
        }
    }

    #[test]
    fn beats_the_directional_interpolation() {
        let img = scene(150, 120);
        let cfa = Cfa::xtrans();
        let n = mosaic_from_rgb(&img, &cfa);
        let new = psnr(&img, &demosaic(&mosaic(&n), None).unwrap(), 8);
        let old = psnr(&img, &xtrans::directional(&mosaic(&n)), 8);
        assert!(new > old + 1.0, "new {new:.2} dB, directional {old:.2} dB");
    }

    #[test]
    fn hostile_samples_and_sizes_do_not_panic() {
        let cfa = Cfa::xtrans();
        let mut n = mosaic_from_rgb(&scene(40, 30), &cfa);
        for (i, v) in n.data.iter_mut().enumerate() {
            if i % 7 == 0 {
                *v = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -3.0, 1e30][i % 5];
            }
        }
        let out = demosaic(&mosaic(&n), Some([2.0, 1.0, f32::NAN])).unwrap();
        assert_eq!((out.width, out.height), (40, 30));
        // too small, or a short buffer: the caller falls back
        let small = mosaic_from_rgb(&scene(23, 40), &cfa);
        assert!(demosaic(&mosaic(&small), None).is_none());
        let short = Mosaic { w: 40, h: 30, data: &n.data[..100], cfa: &cfa };
        assert!(demosaic(&short, None).is_none());
    }
}
