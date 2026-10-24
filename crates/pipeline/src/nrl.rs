//! Luminance noise reduction (prototype for #1129). An à trous wavelet of linear Y; each detail level
//! is shrunk against a brightness-dependent noise model of Y measured on the frame's flattest blocks
//! ([`stats`]), and the change of Y is added to R, G and B alike. Adding instead of scaling keeps the
//! chroma (except where a channel reaches its floor, see `add_luminance`) and lets black or negative
//! pixels move. The shrinking is odd in each coefficient, so
//! symmetric noise keeps its mean; nothing of the removed part is put back.
//!
//! Amount sets the threshold (in noise sigmas of the level), Detail lowers it on the two fine levels,
//! Contrast on the three coarse ones.

use lightcraft_color::luminance_2020;
use lightcraft_raster::{Plane, Rgb32f, blur::gaussian_owned};

use crate::{for_rows, nrc_joint::JointNoise};

/// Detail levels (holes 1, 2, 4, 8, 16 px).
pub const LEVELS: usize = 5;
/// Levels with a robust (median) local energy; coarser levels average.
pub const ROBUST_LEVELS: usize = 2;

/// The sliders, 0–1, and the scale of the image filtered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub amount: f32,
    pub detail: f32,
    pub contrast: f32,
    /// Output pixels per pixel of the original (`Plan::detail_scale`, 1 at full size; see [`threshold`]).
    pub scale: f32,
}

/// Gaussian window of the local energy of coarse level `j` (px).
pub fn energy_sigma(j: usize) -> f32 {
    (2.0 * (1u32 << j) as f32 + 1.0) / 12f32.sqrt()
}

/// How far an output pixel reads its neighbours (px): the coarsest detail level and its energy
/// window. A window rendered on its own needs this much context around it.
pub fn halo() -> usize {
    let reach = |sigma: f32| lightcraft_raster::blur::box_radii(sigma).iter().sum::<usize>();
    // the à trous steps: 2 · 2^j on each side
    let wavelet: usize = (0..LEVELS).map(|j| 2usize << j).sum();
    wavelet + reach(energy_sigma(LEVELS - 1)) + 4
}

/// Threshold of level `j` in noise sigmas of that level. Amount: the noise left on a flat area falls
/// about evenly in log steps (quality probe H: 1 px RMS 78, 53, 30, 19, 13 % at 10, 25, 50, 75, 100) and
/// flattens towards 100, which would otherwise take fine texture. Detail lowers the threshold on the
/// fine levels (Amount 50: 16–51 % noise left), Contrast on the coarse ones (coarse noise and faint coarse
/// texture kept from about half to most).
fn full_size_threshold(p: &Params, j: usize) -> f32 {
    let a = p.amount.clamp(0.0, 1.0);
    let t = (2.0 * a * (1.0 - 0.25 * a)).sqrt();
    let f = if j < ROBUST_LEVELS { 1.25 - 0.5 * p.detail.clamp(0.0, 1.0) } else { 1.0 - 0.8 * p.contrast.clamp(0.0, 1.0) };
    t * f
}

/// Threshold of level `j` of an image at `p.scale` output pixels per pixel of the original. Noise
/// lives at the original's pixels (issue #960): level `j` of a preview holds the detail of level
/// `j + log2(1 / scale)` of the original, so it takes that level's full-size threshold (interpolated
/// between whole levels), and a level coarser than the original's coarsest is left alone, as the
/// full-size render leaves it. A preview then looks like the full-size render scaled down; at
/// scale 1 (and down to a fifth) this is [`full_size_threshold`] of the shifted level.
pub fn threshold(p: &Params, j: usize) -> f32 {
    let scale = if p.scale.is_finite() && p.scale > 0.0 { p.scale.min(1.0) } else { 1.0 };
    let at = |l: usize| if l < LEVELS { full_size_threshold(p, l) } else { 0.0 };
    let m = j as f32 + (1.0 / scale).log2();
    if m >= LEVELS as f32 {
        return 0.0;
    }
    let lo = m.floor();
    let frac = m - lo;
    let lo = lo as usize;
    let t = if frac <= 0.0 { at(lo) } else { at(lo) * (1.0 - frac) + at(lo + 1) * frac };
    // Below a fifth of the original's size the noise is mostly averaged away and what the frame's
    // flattest blocks measure as noise is largely texture: the thresholds fade out towards a 20th, so
    // a grid thumbnail is never further from the full-size render scaled down than one without
    // noise reduction (as the guided filter's radii fade below a pixel, `local::nr_params`).
    t * ((scale - PREVIEW_FADE.0) / (PREVIEW_FADE.1 - PREVIEW_FADE.0)).clamp(0.0, 1.0)
}

/// Scales between which [`threshold`] fades from none to full.
const PREVIEW_FADE: (f32, f32) = (0.05, 0.2);

const B3: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// Mirror `i` into `0..n` (without repeating the edge sample).
#[inline]
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    let mut i = i;
    if i < 0 {
        i = -i;
    }
    if i >= n {
        i = 2 * n - 2 - i;
    }
    i.clamp(0, n - 1) as usize
}

/// `line` with `m` mirrored samples on each side, into `pad` (len + 2m).
fn pad_row(line: &[f32], m: usize, pad: &mut Vec<f32>) {
    let n = line.len();
    pad.clear();
    pad.extend((0..m).map(|k| line[mirror(k as isize - m as isize, n)]));
    pad.extend_from_slice(line);
    pad.extend((0..m).map(|k| line[mirror((n + k) as isize, n)]));
}

/// One à trous step: separable B3 spline with holes of `2^j` px, mirrored at the borders.
fn smooth(src: &Plane, j: usize) -> Plane {
    let (w, h) = (src.width, src.height);
    let s = 1usize << j;
    let mut tmp = Plane::new(w, h);
    for_rows(&mut tmp.data, w, |y, row| {
        let mut pad = Vec::with_capacity(w + 4 * s);
        pad_row(&src.data[y * w..(y + 1) * w], 2 * s, &mut pad);
        let taps: [&[f32]; 5] = std::array::from_fn(|k| &pad[k * s..k * s + w]);
        for (x, o) in row.iter_mut().enumerate() {
            *o = B3[0] * taps[0][x] + B3[1] * taps[1][x] + B3[2] * taps[2][x] + B3[3] * taps[3][x] + B3[4] * taps[4][x];
        }
    });
    let mut out = Plane::new(w, h);
    for_rows(&mut out.data, w, |y, row| {
        let rows: [&[f32]; 5] = std::array::from_fn(|k| {
            let r = mirror(y as isize + (k as isize - 2) * s as isize, h);
            &tmp.data[r * w..(r + 1) * w]
        });
        for (x, o) in row.iter_mut().enumerate() {
            *o = B3[0] * rows[0][x] + B3[1] * rows[1][x] + B3[2] * rows[2][x] + B3[3] * rows[3][x] + B3[4] * rows[4][x];
        }
    });
    out
}

/// `E[max(min(|a|, |b|))²] / E[d²]` over the eight directions for the noise of the two fine levels
/// (white noise through the wavelet; test `directional_energy_is_calibrated`).
pub const DIRECTIONAL_MEAN_SQUARE: f32 = 1.977;

/// The directions of the fine-level energy as steps in half the level's reach: horizontal, vertical,
/// both diagonals and the four angles between (a line one pixel aside every two rows).
#[cfg(test)]
const DIRECTIONS: [(isize, isize); 8] = [(2, 0), (0, 2), (2, 2), (2, -2), (1, 2), (-1, 2), (2, 1), (2, -1)];

/// Local energy of the detail plane `d` (level `j`) as an estimate of its mean square. On the fine levels:
/// along each of eight directions (`DIRECTIONS`) the smaller `|d|` of the two samples `2^(j+1)` px to
/// either side, each sample the largest within ±1 px across the direction (the diagonals: none), the
/// largest of the eight, squared and scaled to the mean square of noise. A line or edge of any angle is
/// supported along its own direction; the pixel itself does not count, so an isolated outlier does not
/// protect itself. On the coarse levels a Gaussian mean. (The filter computes it per pixel in
/// `shrink_fine` and `shrink_coarse`; this plane form is their test reference.)
#[cfg(test)]
fn energy(d: &Plane, j: usize) -> Plane {
    let (w, h) = (d.width, d.height);
    if j < ROBUST_LEVELS {
        let half = 1usize << j;
        let s = 2 * half;
        let mut e = Plane::new(w, h);
        // |d| of row r, padded by s + 1 on each side
        let row_abs = |r: isize, pad: &mut Vec<f32>| {
            let r = mirror(r, h);
            pad_row(&d.data[r * w..(r + 1) * w], s + 1, pad);
            pad.iter_mut().for_each(|v| *v = v.abs());
        };
        for_rows(&mut e.data, w, |y, row| {
            let n = w + 2 * s + 2;
            // per step dy (−2..=2 in half reaches): |d|, its horizontal 3-max, and its vertical 3-max
            let mut plain: [Vec<f32>; 5] = std::array::from_fn(|_| Vec::with_capacity(n));
            let mut across_h: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0; n]);
            let mut across_v: [Vec<f32>; 5] = std::array::from_fn(|_| Vec::with_capacity(n));
            let (mut above, mut below) = (Vec::with_capacity(n), Vec::with_capacity(n));
            for k in 0..5 {
                let r = y as isize + (k as isize - 2) * half as isize;
                row_abs(r, &mut plain[k]);
                row_abs(r - 1, &mut above);
                row_abs(r + 1, &mut below);
                let p = &plain[k];
                across_v[k].extend(p.iter().zip(&above).zip(&below).map(|((a, b), c)| a.max(*b).max(*c)));
                for i in 1..n - 1 {
                    across_h[k][i] = p[i - 1].max(p[i]).max(p[i + 1]);
                }
            }
            fn at(rows: &[Vec<f32>; 5], dx: isize, dy: isize, s: usize, half: usize, w: usize) -> &[f32] {
                let o = (s as isize + 1 + dx * half as isize) as usize;
                &rows[(2 + dy) as usize][o..o + w]
            }
            let pairs: [(&[f32], &[f32]); 8] = DIRECTIONS.map(|(dx, dy)| {
                // near-horizontal directions look up and down, near-vertical ones left and right
                let rows = if dx.abs() == dy.abs() || dx == 0 || dy == 0 {
                    &plain
                } else if dx.abs() > dy.abs() {
                    &across_v
                } else {
                    &across_h
                };
                (at(rows, dx, dy, s, half, w), at(rows, -dx, -dy, s, half, w))
            });
            for (x, o) in row.iter_mut().enumerate() {
                let m = pairs.iter().map(|(a, b)| a[x].min(b[x])).fold(0.0f32, f32::max);
                *o = m * m / DIRECTIONAL_MEAN_SQUARE;
            }
        });
        e
    } else {
        gaussian_owned(d.map(|v| v * v), energy_sigma(j))
    }
}

/// Removes from `change` what the shrinking takes of fine level `j` (detail `d`): the directional
/// energy of `energy` and `keep = max(0, 1 − k·var / energy)` per pixel in one pass, without an energy
/// plane. `|d|` is read in place; only its vertical 3-max (the near-horizontal directions) is a plane.
fn shrink_fine(d: &Plane, j: usize, k: f32, var: &Plane, change: &mut Plane) {
    let (w, h) = (d.width, d.height);
    let half = 1usize << j;
    let s = 2 * half;
    let dd = &d.data;
    let mut v = Plane::new(w, h);
    for_rows(&mut v.data, w, |y, row| {
        let (a, b, c) = (mirror(y as isize - 1, h) * w, y * w, mirror(y as isize + 1, h) * w);
        for (x, o) in row.iter_mut().enumerate() {
            *o = dd[a + x].abs().max(dd[b + x].abs()).max(dd[c + x].abs());
        }
    });
    let vv = &v.data;
    let (hs, ss) = (half as isize, s as isize);
    let shrink = |dv: f32, var: f32, m: f32| {
        let e = m * m / DIRECTIONAL_MEAN_SQUARE;
        let keep = (1.0 - k * var / e.max(1e-30)).max(0.0);
        dv * (1.0 - keep)
    };
    for_rows(&mut change.data, w, |y, out| {
        let r = |dy: isize| mirror(y as isize + dy * hs, h) * w;
        let (rm2, rm1, r0, rp1, rp2) = (r(-2), r(-1), r(0), r(1), r(2));
        let (dr, vr) = (&dd[r0..r0 + w], &var.data[r0..r0 + w]);
        // inside: every sample a fixed offset in its row, so whole slices (vectorised)
        let (lo, hi) = (s + 1, w.saturating_sub(s + 1).max(s + 1));
        let n = hi - lo;
        if n > 0 {
            fn at(src: &[f32], row: usize, dx: isize, lo: usize, n: usize) -> &[f32] {
                &src[(row as isize + lo as isize + dx) as usize..][..n]
            }
            let p = |row: usize, dx: isize| at(dd, row, dx, lo, n);
            let (a0, a1) = (p(r0, ss), p(r0, -ss));
            let (b0, b1) = (p(rp2, 0), p(rm2, 0));
            let (c0, c1) = (p(rp2, ss), p(rm2, -ss));
            let (e0, e1) = (p(rm2, ss), p(rp2, -ss));
            let h3 = |row: usize, dx: isize| [p(row, dx - 1), p(row, dx), p(row, dx + 1)];
            let (f0, f1) = (h3(rp2, hs), h3(rm2, -hs));
            let (g0, g1) = (h3(rp2, -hs), h3(rm2, hs));
            let (v0, v1) = (at(vv, rp1, ss, lo, n), at(vv, rm1, -ss, lo, n));
            let (u0, u1) = (at(vv, rm1, ss, lo, n), at(vv, rp1, -ss, lo, n));
            let (dr_in, vr_in, out_in) = (&dr[lo..hi], &vr[lo..hi], &mut out[lo..hi]);
            for i in 0..n {
                let hx = |t: &[&[f32]; 3]| t[0][i].abs().max(t[1][i].abs()).max(t[2][i].abs());
                let m = a0[i]
                    .abs()
                    .min(a1[i].abs())
                    .max(b0[i].abs().min(b1[i].abs()))
                    .max(c0[i].abs().min(c1[i].abs()))
                    .max(e0[i].abs().min(e1[i].abs()))
                    .max(hx(&f0).min(hx(&f1)))
                    .max(hx(&g0).min(hx(&g1)))
                    .max(v0[i].min(v1[i]))
                    .max(u0[i].min(u1[i]))
                    .max(0.0);
                out_in[i] -= shrink(dr_in[i], vr_in[i], m);
            }
        }
        // the border columns: mirrored samples
        for x in (0..w).filter(|&x| x < lo || x >= lo + n) {
            let xi = x as isize;
            let p = |row: usize, dx: isize| dd[row + mirror(xi + dx, w)].abs();
            let across_h = |row: usize, dx: isize| p(row, dx - 1).max(p(row, dx)).max(p(row, dx + 1));
            let across_v = |row: usize, dx: isize| vv[row + mirror(xi + dx, w)];
            let m = p(r0, ss)
                .min(p(r0, -ss))
                .max(p(rp2, 0).min(p(rm2, 0)))
                .max(p(rp2, ss).min(p(rm2, -ss)))
                .max(p(rm2, ss).min(p(rp2, -ss)))
                .max(across_h(rp2, hs).min(across_h(rm2, -hs)))
                .max(across_h(rp2, -hs).min(across_h(rm2, hs)))
                .max(across_v(rp1, ss).min(across_v(rm1, -ss)))
                .max(across_v(rm1, ss).min(across_v(rp1, -ss)))
                .max(0.0);
            out[x] -= shrink(dr[x], vr[x], m);
        }
    });
}

/// [`shrink_fine`] for coarse level `j`: the energy is a Gaussian mean of `d²`.
fn shrink_coarse(d: &Plane, j: usize, k: f32, var: &Plane, change: &mut Plane) {
    let w = d.width;
    let e = gaussian_owned(d.map(|v| v * v), energy_sigma(j));
    for_rows(&mut change.data, w, |row, o| {
        let i0 = row * w;
        for (x, o) in o.iter_mut().enumerate() {
            let i = i0 + x;
            let keep = (1.0 - k * var.data[i] / e.data[i].max(1e-30)).max(0.0);
            *o -= d.data[i] * (1.0 - keep);
        }
    });
}

/// What the luminance NR measures before it filters: the noise model of Y and the noise gain of each
/// level. A windowed render sees only part of the frame, so both come from fixed tiles of the whole
/// frame ([`tiles`]), the same for every window.
#[derive(Clone, Debug)]
pub struct Stats {
    pub noise: JointNoise,
    /// `E[d_j²] / var(Y)` of the noise on level `j`.
    pub gains: [f32; LEVELS],
}

/// Side of the tiles the statistics are measured on (px).
const TILE: usize = 128;
/// Tiles per row and column: many small tiles follow a mostly dark frame's few bright parts better
/// than a few large ones.
const GRID: usize = 12;

/// The windows of a `full_w × full_h` frame the statistics are measured on: the whole frame up to the
/// area of `GRID²` tiles, else `GRID × GRID` tiles spread evenly over it.
pub fn tiles(full_w: usize, full_h: usize) -> Vec<crate::PixelWindow> {
    if full_w * full_h <= GRID * GRID * TILE * TILE || full_w < 2 * TILE || full_h < 2 * TILE {
        return vec![crate::PixelWindow { x: 0, y: 0, w: full_w, h: full_h }];
    }
    let at = |n: usize, k: usize| ((n - TILE) as f64 * (k as f64 + 0.5) / GRID as f64).round() as usize;
    (0..GRID * GRID).map(|k| crate::PixelWindow { x: at(full_w, k % GRID), y: at(full_h, k / GRID), w: TILE, h: TILE }).collect()
}

/// Y of linear RGB (in parallel).
fn luminance_of(img: &Rgb32f) -> Plane {
    img.map(luminance_2020)
}

/// Brightness for the noise level: the second smoothing (a few px), so each side of an edge keeps
/// its own level.
fn levels_of(img: &Rgb32f) -> (Plane, Plane, Plane) {
    let y = luminance_of(img);
    let c1 = smooth(&y, 0);
    let c2 = smooth(&c1, 1);
    (y, c1, c2)
}

/// `E[d_j²] / σ²` of white noise on level `j`: the energy of the level's impulse response.
const WHITE_GAINS: [f32; LEVELS] = [0.793_518, 0.040_266, 0.007_311_5, 0.001_698_9, 0.000_417_18];

/// The most a level's noise energy, relative to white noise, exceeds the next finer level's (see
/// `stats`). Measured on full-size raws (cat ISO 25600, candle ISO 3200): 1.1–2.1 from level to level,
/// 2.5–3 to the coarsest, where content may already count; on a 1920 px preview of the cat 12–36, which is
/// content only.
const MAX_LEVEL_STEP: f32 = 2.0;

/// Side of the blocks level `j`'s energy is measured in (px): enough samples of the level's grain.
fn block(j: usize) -> usize {
    16 << j.saturating_sub(1)
}

/// The location of the noise-only part of `v`, from its low tail: the 25 % quantile plus its distance
/// to the 10 % quantile scaled as for a normal distribution (texture and edges only raise values, so
/// a quarter of noise-only samples suffices). Low rather than high where the tail is skewed.
fn floor_of(v: &mut [f32]) -> Option<f32> {
    if v.len() < 8 {
        return None;
    }
    v.sort_unstable_by(f32::total_cmp);
    let q = |p: f32| {
        let x = p * (v.len() - 1) as f32;
        let (i, f) = (x.floor() as usize, x.fract());
        let lo = v.get(i).copied().unwrap_or(0.0);
        lo + f * (v.get(i + 1).copied().unwrap_or(lo) - lo)
    };
    let (q10, q25) = (q(0.10), q(0.25));
    Some(q25 + 0.6745 * (q25 - q10) / 0.6076)
}

/// The noise model and level gains of `tiles` (white-balanced linear RGB at output scale).
///
/// Each level's mean square is measured in blocks (`block`) with their mean Y. The model of Y
/// (`α·Y + β`) is fitted to level 0 per brightness band, each band at the noise floor of its
/// blocks (`floor_of`); each level's gain is the floor of its blocks' energy over the model.
/// Texture and edges only raise a block, so the estimate follows the flattest blocks wherever
/// they are. Noise that is smooth (demosaicing) has relatively more energy on coarser levels than
/// white noise, never less, but not much more from one level to the next (`MAX_LEVEL_STEP`); a level
/// with relatively more than all coarser ones carries texture of its scale (an image covered by it
/// has no flat blocks) and is lowered to them.
pub fn stats(tiles: &[Rgb32f]) -> Stats {
    let mut blocks: [Vec<(f32, f32)>; LEVELS] = std::array::from_fn(|_| Vec::new());
    for t in tiles.iter().filter(|t| t.width >= 16 && t.height >= 16) {
        let (y, c1, c2) = levels_of(t);
        let mut levels = vec![y, c1, c2];
        for (j, out) in blocks.iter_mut().enumerate() {
            if levels.len() < j + 2 {
                let next = smooth(&levels[j], j);
                levels.push(next);
            }
            let (a, b) = (&levels[j], &levels[j + 1]);
            let n = block(j).min(t.width).min(t.height);
            for by in 0..t.height / n {
                for bx in 0..t.width / n {
                    let (mut sy, mut se) = (0.0f64, 0.0f64);
                    for r in by * n..(by + 1) * n {
                        let row = r * t.width + bx * n..r * t.width + (bx + 1) * n;
                        for (&p, &q) in a.data[row.clone()].iter().zip(&b.data[row.clone()]) {
                            se += f64::from((p - q) * (p - q));
                        }
                        sy += levels[0].data[row].iter().map(|&v| f64::from(v)).sum::<f64>();
                    }
                    let k = (n * n) as f64;
                    out.push(((sy / k) as f32, (se / k) as f32));
                }
            }
            if j > 0 {
                levels[j] = Plane::new(0, 0);
            }
        }
    }
    let noise = fit_level0(&mut blocks[0]);
    // a level with too few blocks (a small frame) takes level 0's ratio to white noise: the least
    // that smooth noise has, so it lowers rather than raises the finer levels
    let mut gains = [0.0f32; LEVELS];
    for j in 0..LEVELS {
        let mut r: Vec<f32> = blocks[j].iter().map(|&(y, e)| e / noise.var(0, y)).collect();
        let white = if j == 0 { 1.0 } else { gains[0] / WHITE_GAINS[0] };
        gains[j] = floor_of(&mut r).unwrap_or(white * WHITE_GAINS[j]).max(0.0);
    }
    // relative to white noise, noise grows by at most MAX_LEVEL_STEP from a level to the next coarser one;
    // more is content (a small frame has few flat blocks at the coarse scales)
    for j in 1..LEVELS {
        gains[j] = gains[j].min(MAX_LEVEL_STEP * gains[j - 1] * WHITE_GAINS[j] / WHITE_GAINS[j - 1]);
    }
    for j in (0..LEVELS - 1).rev() {
        gains[j] = gains[j].min(gains[j + 1] * WHITE_GAINS[j] / WHITE_GAINS[j + 1]);
    }
    Stats { noise, gains }
}

/// The model of Y from level 0's blocks `(mean Y, mean square)` (see [`stats`]): 16 brightness bands
/// at their noise floor, a non-negative line seeded from the darker half; bands far above it (texture
/// at all their blocks) lose their weight.
fn fit_level0(blocks: &mut [(f32, f32)]) -> JointNoise {
    use crate::local::{NrcNoise, fit_nonneg_line};
    blocks.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let per = blocks.len().div_ceil(16).max(16);
    let pts: Vec<(f64, f64, f64)> = blocks
        .chunks(per)
        .filter_map(|band| {
            let mut e: Vec<f32> = band.iter().map(|b| b.1 / WHITE_GAINS[0]).collect();
            let y = band.iter().map(|b| f64::from(b.0)).sum::<f64>() / band.len() as f64;
            floor_of(&mut e).map(|v| (y, f64::from(v.max(0.0)), band.len() as f64))
        })
        .collect();
    let mut noise = JointNoise { model: NrcNoise { alpha: [0.0; 3], beta: [1e-14; 3] }, ymax: [0.0; 3], tolerance: 1.0 };
    if pts.is_empty() {
        return noise;
    }
    let (mut a, mut b) = fit_nonneg_line(&pts[..pts.len().div_ceil(2)]);
    for _ in 0..4 {
        let weighted: Vec<_> = pts
            .iter()
            .filter_map(|&(y, v, w)| {
                let excess = v / (a * y + b).max(1e-14);
                (excess <= 4.0).then_some((y, v, w * (2.0 / excess.max(2.0)).powi(4)))
            })
            .collect();
        if !weighted.is_empty() {
            (a, b) = fit_nonneg_line(&weighted);
            noise.ymax[0] = weighted.iter().map(|p| p.0 as f32).fold(0.0, f32::max);
        }
    }
    noise.model.alpha[0] = a as f32;
    noise.model.beta[0] = (b as f32).max(1e-14);
    noise
}

/// `c` with its luminance changed by `dy`, the same in each channel as far as no channel goes below
/// `min(v, 0)` (no new negative values: the undershoot next to edges would reach the later stages'
/// floors). What a floored channel cannot take is moved to the others, so the luminance change stays
/// whole as long as any channel can take it; only then do the channels' differences change: a channel
/// near its floor, also in a bright saturated colour, so the limit is per channel, not per brightness.
/// Measured on noisy saturated colours (quality probe G, Amount 100): the mean of R−G, B−G moves by
/// ≤ 0.3 % at full brightness, 1–4 % at a tenth, up to 14 % (less saturated) at a fiftieth, where the
/// noise is as large as the colour; next to an edge no more than inside. Keeping these differences
/// instead would leave the darkening undone and brighten the shadows.
fn add_luminance(c: [f32; 3], dy: f32) -> [f32; 3] {
    let floor = c.map(|v| v.min(0.0));
    if (0..3).all(|k| c[k] + dy >= floor[k]) {
        return c.map(|v| v + dy);
    }
    let weight = lightcraft_color::LUMA_2020;
    let mut out = c;
    let mut left = dy;
    let mut free = [true; 3];
    for _ in 0..3 {
        let w: f32 = (0..3).filter(|&k| free[k]).map(|k| weight[k]).sum();
        if left >= 0.0 || w <= 0.0 {
            break;
        }
        // the same change in every free channel: `left / w` each
        let step = left / w;
        let mut taken = 0.0;
        for k in 0..3 {
            if free[k] {
                let v = (out[k] + step).max(floor[k]);
                taken += (v - out[k]) * weight[k];
                free[k] = v > floor[k];
                out[k] = v;
            }
        }
        left -= taken;
        if left.abs() <= 1e-12 {
            break;
        }
    }
    out
}

/// Luminance noise reduction of linear RGB in place, with `stats` of the whole frame (`None`:
/// measured on `img`).
pub fn denoise(img: &mut Rgb32f, p: &Params, stats: Option<&Stats>) {
    denoise_y(img, p, stats, None);
}

/// [`denoise`] with the luminance `y` of `img` when the caller has it (the colour NR makes it as it
/// writes; the same values as computed here).
pub fn denoise_y(img: &mut Rgb32f, p: &Params, stats: Option<&Stats>, y: Option<Plane>) {
    if img.width < 8 || img.height < 8 || p.amount <= 0.0 {
        return;
    }
    let start = crate::profiling().then(std::time::Instant::now);
    let mut tt = start;
    let lap = |what: &str, t: &mut Option<std::time::Instant>| {
        if let Some(t0) = t {
            eprintln!("      nrl {what}: {:.0} ms", t0.elapsed().as_secs_f64() * 1e3);
            *t = Some(std::time::Instant::now());
        }
    };
    let own;
    let Stats { noise, gains } = match stats {
        Some(s) => s,
        None => {
            own = self::stats(std::slice::from_ref(&*img));
            &own
        }
    };
    lap("stats", &mut tt);
    let (w, h) = (img.width, img.height);
    let y = y.filter(|y| (y.width, y.height) == (w, h)).unwrap_or_else(|| luminance_of(img));
    let c1 = smooth(&y, 0);
    let c2 = smooth(&c1, 1);
    let var = c2.map(|v| noise.var(0, v));
    lap("y, c1, c2, var", &mut tt);
    // the change of Y: minus the part of each level the shrinking removes
    let mut change = Plane::new(w, h);
    let mut levels: Vec<Plane> = vec![y, c1, c2];
    for (j, gain) in gains.iter().enumerate() {
        if levels.len() < j + 2 {
            let next = smooth(&levels[j], j);
            levels.push(next);
        }
        lap("smooth", &mut tt);
        // the finer approximation becomes the detail
        let mut d = std::mem::replace(&mut levels[j], Plane::new(0, 0));
        let next = &levels[j + 1];
        for_rows(&mut d.data, w, |row, o| {
            for (o, n) in o.iter_mut().zip(&next.data[row * w..(row + 1) * w]) {
                *o -= n;
            }
        });
        lap("detail", &mut tt);
        let t = threshold(p, j);
        let k = t * t * gain;
        if j < ROBUST_LEVELS {
            shrink_fine(&d, j, k, &var, &mut change);
        } else {
            shrink_coarse(&d, j, k, &var, &mut change);
        }
        lap("energy + shrink", &mut tt);
    }
    for_rows(&mut img.data, w, |row, px| {
        let i0 = row * w;
        for (x, c) in px.iter_mut().enumerate() {
            *c = add_luminance(*c, change.data[i0 + x]);
        }
    });
    lap("apply", &mut tt);
    if let Some(t) = start {
        eprintln!(
            "    nr luminance (wavelet): {:.0} ms, Y noise alpha {:.3e} beta {:.3e}, level gains {:?}",
            t.elapsed().as_secs_f64() * 1e3,
            noise.model.alpha[0],
            noise.model.beta[0],
            gains
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(w: usize, h: usize, level: impl Fn(usize, usize) -> f32, sigma: impl Fn(f32) -> f32) -> (Rgb32f, Rgb32f) {
        let mut seed = 0x9e37_79b9u32;
        let mut normal = || {
            let mut u = || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed as f64 + 0.5) / (u32::MAX as f64 + 1.0)
            };
            ((-2.0 * u().ln()).sqrt() * (std::f64::consts::TAU * u()).cos()) as f32
        };
        let clean = Rgb32f::from_fn(w, h, |x, y| [level(x, y); 3]);
        let mut img = clean.clone();
        for p in &mut img.data {
            let n = sigma(p[0]) * normal();
            *p = p.map(|v| v + n);
        }
        (clean, img)
    }

    fn stats(img: &Rgb32f, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) -> (f32, f32) {
        let v: Vec<f32> = ys.flat_map(|y| xs.clone().map(move |x| (x, y))).map(|(x, y)| luminance_2020(img.data[y * img.width + x])).collect();
        let m = v.iter().sum::<f32>() / v.len() as f32;
        (m, (v.iter().map(|a| (a - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt())
    }

    /// Shot plus read noise on a dark and a bright half: the noise falls on both, the mean of the dark
    /// half stays (the old filter darkened it and left most of its noise).
    #[test]
    fn reduces_noise_in_shadows_and_keeps_the_mean() {
        let (_, mut img) = noisy(256, 128, |x, _| if x < 128 { 0.004 } else { 0.2 }, |y| (0.002 * y + 4e-6).sqrt());
        let before = [stats(&img, 16..112, 16..112), stats(&img, 144..240, 16..112)];
        denoise(&mut img, &Params { amount: 0.5, detail: 0.5, contrast: 0.0, scale: 1.0 }, None);
        let after = [stats(&img, 16..112, 16..112), stats(&img, 144..240, 16..112)];
        for (b, a) in before.iter().zip(&after) {
            assert!(a.1 < 0.5 * b.1, "noise {} -> {}", b.1, a.1);
            assert!((a.0 - b.0).abs() < 0.01 * b.0 + 2e-5, "mean {} -> {}", b.0, a.0);
        }
    }

    /// A pixel that is black before the NR moves with its surroundings.
    #[test]
    fn black_pixels_are_not_kept_black() {
        let (_, mut img) = noisy(64, 64, |_, _| 0.05, |y| (0.002 * y + 4e-6).sqrt());
        img.data[32 * 64 + 32] = [0.0; 3];
        denoise(&mut img, &Params { amount: 1.0, detail: 0.5, contrast: 0.0, scale: 1.0 }, None);
        assert!(luminance_2020(img.data[32 * 64 + 32]) > 0.01);
    }

    /// Contrast and Detail act on different levels.
    #[test]
    fn detail_and_contrast_change_the_result() {
        let (_, img) = noisy(128, 128, |x, y| 0.1 + 0.02 * ((x / 16 + y / 16) % 2) as f32, |y| (0.002 * y + 4e-6).sqrt());
        let run = |detail, contrast| {
            let mut a = img.clone();
            denoise(&mut a, &Params { amount: 0.5, detail, contrast, scale: 1.0 }, None);
            a
        };
        let base = run(0.5, 0.0);
        assert_ne!(base.data, run(0.5, 1.0).data);
        assert_ne!(base.data, run(1.0, 0.0).data);
    }

    /// Scaling the input scales the output (exposure is applied after the filter).
    #[test]
    fn scaling_the_input_scales_the_output() {
        let src = lightcraft_scenes::demo_library()[0].render(200, 133);
        let g = 2f32.powf(1.3);
        let p = Params { amount: 0.3, detail: 0.5, contrast: 0.0, scale: 1.0 };
        let mut a = src.clone();
        denoise(&mut a, &p, None);
        let mut b = src.map(|c| c.map(|v| v * g));
        denoise(&mut b, &p, None);
        for (x, y) in a.data.iter().zip(&b.data) {
            for k in 0..3 {
                assert!((x[k] - y[k] / g).abs() <= 1e-3 * x[k].abs().max(1e-3), "{x:?} vs {:?}", y.map(|v| v / g));
            }
        }
    }

    /// Gaussian noise of `sigma` added to Y alike in all channels, fixed seed.
    fn add_noise(img: &mut Rgb32f, sigma: f32, seed: u32) {
        let mut seed = seed | 1;
        let mut u = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f64 + 0.5) / (u32::MAX as f64 + 1.0)
        };
        for p in &mut img.data {
            let n = ((-2.0 * u().ln()).sqrt() * (std::f64::consts::TAU * u()).cos()) as f32 * sigma;
            *p = p.map(|v| v + n);
        }
    }

    fn y_of(img: &Rgb32f) -> Vec<f64> {
        img.data.iter().map(|&c| luminance_2020(c) as f64).collect()
    }

    /// RMS of `a − b` after a box average of `r × r` px (non-overlapping blocks).
    fn rms_at(a: &[f64], b: &[f64], w: usize, h: usize, r: usize) -> f64 {
        let (mut s, mut n) = (0.0, 0.0);
        for by in (0..h - r + 1).step_by(r) {
            for bx in (0..w - r + 1).step_by(r) {
                let mut m = 0.0;
                for y in by..by + r {
                    for x in bx..bx + r {
                        m += a[y * w + x] - b[y * w + x];
                    }
                }
                m /= (r * r) as f64;
                s += m * m;
                n += 1.0;
            }
        }
        (s / n).sqrt()
    }

    const SIGMA: f32 = 0.003;

    fn run(img: &Rgb32f, amount: f32, stats: Option<&Stats>) -> Rgb32f {
        let mut out = img.clone();
        denoise(&mut out, &Params { amount, detail: 0.5, contrast: 0.0, scale: 1.0 }, stats);
        out
    }

    /// A one pixel wide line ten noise sigmas above its flat surround keeps nearly all of its contrast at
    /// Amount 100 at any angle (the median of a 3 × 3 neighbourhood took it for noise and kept 64 %; four
    /// fixed directions kept 68 % of a line one pixel aside every two rows).
    #[test]
    fn thin_lines_are_kept() {
        let (w, h) = (256, 256);
        let mut flat = Rgb32f::filled(w, h, [0.05; 3]);
        add_noise(&mut flat, SIGMA, 7);
        let stats = super::stats(std::slice::from_ref(&flat));
        for slope in [0.0f64, 0.25, 0.5, 0.75, 1.0, 1.5] {
            let at = |y: usize| (128.0 + (y as f64 - 128.0) * slope / slope.max(1.0)).round() as usize;
            // steeper than 45°: the transposed line
            let on = |x: usize, y: usize| if slope > 1.0 { y == at(x) } else { x == at(y) };
            let clean = Rgb32f::from_fn(w, h, |x, y| [0.05 + if on(x, y) { 10.0 * SIGMA } else { 0.0 }; 3]);
            let mut img = clean.clone();
            add_noise(&mut img, SIGMA, 11);
            let out = y_of(&run(&img, 1.0, Some(&stats)));
            let mut contrast = 0.0;
            for r in 64..192 {
                let i = |x: usize, y: usize| if slope > 1.0 { x * w + y } else { y * w + x };
                let p = at(r);
                contrast += out[i(p, r)] - 0.5 * (out[i(p - 20, r)] + out[i(p + 20, r)]);
            }
            contrast /= 128.0;
            assert!(contrast > 0.85 * 10.0 * SIGMA as f64, "slope {slope}: line contrast {contrast} of {}", 10.0 * SIGMA);
        }
    }

    /// Isolated specks eight noise sigmas off do not protect themselves.
    #[test]
    fn isolated_specks_are_removed() {
        let (w, h) = (256, 256);
        let mut img = Rgb32f::filled(w, h, [0.05; 3]);
        add_noise(&mut img, SIGMA, 7);
        let specks: Vec<(usize, f32)> =
            (0..40).map(|k| ((20 + (k % 8) * 28) + (20 + (k / 8) * 45) * w, if k % 2 == 0 { 8.0 } else { -8.0 } * SIGMA)).collect();
        for &(i, d) in &specks {
            img.data[i] = img.data[i].map(|v| v + d);
        }
        let o = y_of(&run(&img, 1.0, None));
        let left = specks.iter().map(|&(i, d)| ((o[i] - 0.05) / d as f64).abs()).sum::<f64>() / specks.len() as f64;
        assert!(left < 0.15, "{:.0} % of the specks left", 100.0 * left);
    }

    /// The luminance change is whole as long as some channel can take it, no channel goes below
    /// `min(v, 0)`, and without a floor all channels change alike.
    #[test]
    fn add_luminance_keeps_the_change_and_the_floor() {
        let y = |c: [f32; 3]| luminance_2020(c);
        let c = [0.2, 0.3, 0.1];
        assert_eq!(add_luminance(c, -0.05), c.map(|v| v - 0.05));
        assert_eq!(add_luminance(c, 0.05), c.map(|v| v + 0.05));
        for (c, dy) in [([0.001, 0.02, 0.04], -0.004), ([0.0, 0.01, -0.002], -0.003), ([0.005, 0.001, 0.03], -0.02)] {
            let out = add_luminance(c, dy);
            for k in 0..3 {
                assert!(out[k] >= c[k].min(0.0) - 1e-7, "{c:?} {dy} -> {out:?}");
            }
            let room: f32 = (0..3).map(|k| (c[k] - c[k].min(0.0)) * lightcraft_color::LUMA_2020[k]).sum();
            let want = dy.max(-room);
            assert!((y(out) - y(c) - want).abs() < 1e-6, "{c:?} {dy} -> {out:?}: {} instead of {want}", y(out) - y(c));
        }
    }

    /// The fused shrinking is the plane form ([`energy`], then `keep`) bit for bit, also at the borders
    /// and on frames narrower than the reach.
    #[test]
    fn fused_shrink_matches_the_energy_planes() {
        for (w, h) in [(64usize, 48usize), (37, 23), (9, 11)] {
            let mut img = Rgb32f::from_fn(w, h, |x, y| [0.05 + if x == w / 2 || y == (x / 2) { 0.02 } else { 0.0 }; 3]);
            add_noise(&mut img, SIGMA, 3);
            let y = Plane { width: w, height: h, data: img.data.iter().map(|&c| luminance_2020(c)).collect() };
            let var = y.map(|v| 1e-5 + 1e-4 * v.abs());
            let mut c = y;
            for j in 0..LEVELS {
                let next = smooth(&c, j);
                let d = c.zip_map(&next, |a, b| a - b);
                let k = 1.3;
                let e = energy(&d, j);
                let reference: Vec<f32> =
                    (0..w * h).map(|i| 0.5 - d.data[i] * (1.0 - (1.0 - k * var.data[i] / e.data[i].max(1e-30)).max(0.0))).collect();
                let mut fused = Plane { width: w, height: h, data: vec![0.5; w * h] };
                if j < ROBUST_LEVELS {
                    shrink_fine(&d, j, k, &var, &mut fused);
                } else {
                    shrink_coarse(&d, j, k, &var, &mut fused);
                }
                assert!(fused.data.iter().zip(&reference).all(|(a, b)| a.to_bits() == b.to_bits()), "{w}×{h} level {j}");
                c = next;
            }
        }
    }

    #[test]
    fn directional_energy_is_calibrated() {
        let (w, h) = (512usize, 512usize);
        let mut img = Rgb32f::filled(w, h, [0.0; 3]);
        add_noise(&mut img, 1.0, 5);
        let mut c = Plane { width: w, height: h, data: img.data.iter().map(|&c| luminance_2020(c)).collect() };
        for j in 0..ROBUST_LEVELS {
            let next = smooth(&c, j);
            let d = c.zip_map(&next, |a, b| a - b);
            let e = energy(&d, j);
            let ratio = e.data.iter().map(|&v| f64::from(v)).sum::<f64>() / d.data.iter().map(|&v| f64::from(v * v)).sum::<f64>();
            eprintln!("level {j}: energy / mean square {ratio:.4}");
            assert!((ratio - 1.0).abs() < 0.03, "level {j}: {ratio}");
            c = next;
        }
    }

    #[test]
    fn white_gains_are_the_levels_impulse_energies() {
        let n = 257;
        let mut c = Plane::new(n, n);
        c.data[128 * n + 128] = 1.0;
        for (j, &g) in WHITE_GAINS.iter().enumerate() {
            let next = smooth(&c, j);
            let e: f32 = c.data.iter().zip(&next.data).map(|(a, b)| (a - b) * (a - b)).sum();
            assert!((e / g - 1.0).abs() < 1e-3, "level {j}: {e} vs {g}");
            c = next;
        }
    }

    /// Texture over the whole frame (no flat area) must not be taken for noise: each level's noise
    /// energy as on a flat frame with the same noise. Texture of the coarsest levels' scale everywhere
    /// (period 32 px) can only be bounded: at most `MAX_LEVEL_STEP` more per level.
    #[test]
    fn texture_is_not_taken_for_noise() {
        let (w, h) = (512usize, 512usize);
        let mut flat = Rgb32f::filled(w, h, [0.1; 3]);
        add_noise(&mut flat, SIGMA, 7);
        let reference = super::stats(std::slice::from_ref(&flat));
        let energy = |s: &Stats, j: usize| s.gains[j] * s.noise.var(0, 0.1);
        for j in 0..LEVELS {
            let white = WHITE_GAINS[j] * SIGMA * SIGMA;
            assert!(energy(&reference, j) > 0.7 * white && energy(&reference, j) < 1.3 * white, "flat level {j}");
        }
        for period in [8.0f32, 16.0, 32.0] {
            let f = std::f32::consts::TAU / period;
            let mut img = Rgb32f::from_fn(w, h, |x, y| [0.1 + 0.01 * (x as f32 * f).sin() * (y as f32 * f).sin(); 3]);
            add_noise(&mut img, SIGMA, 13);
            let st = super::stats(std::slice::from_ref(&img));
            for j in 0..LEVELS {
                let r = energy(&st, j) / energy(&reference, j);
                // period 32: no coarser level is free of it, only the growth from level to level is bounded
                let most = if period < 32.0 { 1.25 } else { 1.25 * MAX_LEVEL_STEP.powi(j as i32) };
                assert!(r > 0.8 && r < most, "period {period}, level {j}: {r}");
            }
        }
    }

    /// Quality figures of the filter on synthetic scenes (noise σ 0.003 on Y 0.05 or 0.1):
    /// `cargo test -p lightcraft-pipeline --release --lib nrl::tests::quality_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn quality_probe() {
        let (w, h) = (256usize, 256usize);
        let flat_clean = Rgb32f::filled(w, h, [0.05; 3]);
        let mut flat = flat_clean.clone();
        add_noise(&mut flat, SIGMA, 7);
        let flat_stats = super::stats(std::slice::from_ref(&flat));
        let amounts = [0.25f32, 0.5, 1.0];
        // A: noise left at 1, 4 and 16 px
        for a in amounts {
            let out = run(&flat, a, None);
            let (c, i, o) = (y_of(&flat_clean), y_of(&flat), y_of(&out));
            let f: Vec<String> =
                [1, 4, 16].iter().map(|&r| format!("{r}px {:5.1}%", 100.0 * rms_at(&o, &c, w, h, r) / rms_at(&i, &c, w, h, r))).collect();
            eprintln!("A flat noise left, amount {a}: {}", f.join("  "));
        }
        // B: vertical lines (kept contrast: line mean minus the mean 20 px to either side)
        for width in [1usize, 2, 4] {
            for amp in [5.0f32, 10.0] {
                let clean = Rgb32f::from_fn(w, h, |x, _| [0.05 + if (128..128 + width).contains(&x) { amp * SIGMA } else { 0.0 }; 3]);
                let mut img = clean.clone();
                add_noise(&mut img, SIGMA, 11);
                let contrast = |im: &Rgb32f| {
                    let y = y_of(im);
                    let (mut l, mut b) = (0.0, 0.0);
                    for r in 32..224 {
                        for x in 128..128 + width {
                            l += y[r * w + x];
                        }
                        b += 0.5 * (y[r * w + 108] + y[r * w + 128 + width + 20]) * width as f64;
                    }
                    (l - b) / (192 * width) as f64
                };
                let base = contrast(&clean);
                let f: Vec<String> = amounts.iter().map(|&a| format!("{a}: {:5.1}%", 100.0 * contrast(&run(&img, a, None)) / base)).collect();
                eprintln!("B line {width} px, {amp}σ, contrast kept: {}", f.join("  "));
            }
        }
        // C: texture (product of sines, amplitude 0.01) with noise, own statistics and those of a flat noisy image
        for period in [8.0f32, 16.0, 32.0] {
            let tex =
                |x: usize, y: usize| 0.01 * (x as f32 * std::f32::consts::TAU / period).sin() * (y as f32 * std::f32::consts::TAU / period).sin();
            let clean = Rgb32f::from_fn(w, h, |x, y| [0.1 + tex(x, y); 3]);
            let mut img = clean.clone();
            add_noise(&mut img, SIGMA, 13);
            let own = super::stats(std::slice::from_ref(&img));
            let energies = |st: &Stats| -> Vec<String> { st.gains.iter().map(|g| format!("{:.3e}", g * st.noise.var(0, 0.1))).collect() };
            eprintln!("C level noise energies period {period}: own {:?}  flat {:?}", energies(&own), energies(&flat_stats));
            let t: Vec<f64> = (0..w * h).map(|i| tex(i % w, i / w) as f64).collect();
            let tt: f64 = t.iter().map(|v| v * v).sum();
            for (name, st) in [("own stats", None), ("flat stats", Some(&flat_stats))] {
                let f: Vec<String> = amounts
                    .iter()
                    .map(|&a| {
                        let o = y_of(&run(&img, a, st));
                        let k = o.iter().zip(&t).map(|(o, t)| (o - 0.1) * t).sum::<f64>() / tt;
                        let res = (o.iter().zip(&t).map(|(o, t)| (o - 0.1 - k * t).powi(2)).sum::<f64>() / o.len() as f64).sqrt();
                        format!("{a}: texture {:5.1}% noise {:5.1}%", 100.0 * k, 100.0 * res / SIGMA as f64)
                    })
                    .collect();
                eprintln!("C texture period {period}, {name}: {}", f.join("  "));
            }
        }
        // D: isolated specks ±8σ on the flat noisy image
        let specks: Vec<(usize, f32)> =
            (0..40).map(|k| ((20 + (k % 8) * 28) + (20 + (k / 8) * 45) * w, if k % 2 == 0 { 8.0 } else { -8.0 } * SIGMA)).collect();
        let mut sp = flat.clone();
        for &(i, d) in &specks {
            sp.data[i] = sp.data[i].map(|v| v + d);
        }
        let f: Vec<String> = amounts
            .iter()
            .map(|&a| {
                let o = y_of(&run(&sp, a, Some(&flat_stats)));
                let left = specks.iter().map(|&(i, d)| ((o[i] - 0.05) / d as f64).abs()).sum::<f64>() / specks.len() as f64;
                format!("{a}: {:5.1}%", 100.0 * left)
            })
            .collect();
        eprintln!("D specks ±8σ left: {}", f.join("  "));
        // F: thin structures at other angles, statistics of the flat image: straight lines x = 128 + slope·(y − 128)
        // (sharp: one pixel per row; antialiased: split between the two nearest pixels), an antialiased ring
        // of radius 50 px and short vertical segments of 9 px. Kept: the projection of the change from the
        // background onto the structure (a blurred structure keeps less), mean of four noise seeds.
        let kept = |shape: &dyn Fn(usize, usize) -> f32, amp: f32, a: f32| {
            let s: Vec<f64> = (0..w * h).map(|i| shape(i % w, i / w) as f64).collect();
            let ss: f64 = s.iter().map(|v| v * v).sum();
            let mut sum = 0.0;
            for seed in [11u32, 13, 17, 19] {
                let mut img = Rgb32f::from_fn(w, h, |x, y| [0.05 + amp * SIGMA * shape(x, y); 3]);
                add_noise(&mut img, SIGMA, seed);
                let o = y_of(&run(&img, a, Some(&flat_stats)));
                sum += o.iter().zip(&s).map(|(o, s)| (o - 0.05) * s).sum::<f64>() / (ss * (amp * SIGMA) as f64);
            }
            100.0 * sum / 4.0
        };
        let row = |name: &str, shape: &dyn Fn(usize, usize) -> f32| {
            let f: Vec<String> = [5.0f32, 10.0]
                .iter()
                .flat_map(|&amp| amounts.iter().map(move |&a| (amp, a)))
                .map(|(amp, a)| format!("{amp}σ {a}: {:5.1}%", kept(shape, amp, a)))
                .collect();
            eprintln!("F {name}: {}", f.join("  "));
        };
        let inner = |x: usize, y: usize| (20..236).contains(&x) && (20..236).contains(&y);
        for slope in [0.0f64, 0.25, 0.5, 0.75, 1.0] {
            let centre = move |y: usize| 128.0 + (y as f64 - 128.0) * slope;
            row(&format!("line slope {slope} sharp"), &|x, y| if inner(x, y) && x as f64 == centre(y).round() { 1.0 } else { 0.0 });
            row(&format!("line slope {slope} antialiased"), &|x, y| {
                if inner(x, y) { (1.0 - (x as f64 - centre(y)).abs()).max(0.0) as f32 } else { 0.0 }
            });
        }
        row("ring r50 antialiased", &|x, y| {
            let r = ((x as f64 - 128.0).powi(2) + (y as f64 - 128.0).powi(2)).sqrt();
            (1.0 - (r - 50.0).abs()).max(0.0) as f32
        });
        row("segments 9 px", &|x, y| if x % 32 == 16 && (12..21).contains(&(y % 32)) { 1.0 } else { 0.0 });
        // G: saturated colour next to grey (edge at x = 128), noise per channel σ² = 4e-4·c + 1e-6, own statistics,
        // colours at full, 1/10 and 1/50 brightness. Chroma shift: |Δ mean (R−G, B−G)| from input to output over
        // |mean (R−G, B−G)| of the clean colour, in the colour's interior and in the 4 px next to the edge;
        // Y: relative change of its mean there.
        let noisy_rgb = |clean: &Rgb32f, seed: u32| {
            let mut z = Rgb32f::filled(w, h, [0.0; 3]);
            add_noise(&mut z, 1.0, seed);
            let mut z2 = Rgb32f::filled(w, h, [0.0; 3]);
            add_noise(&mut z2, 1.0, seed + 100);
            let mut z3 = Rgb32f::filled(w, h, [0.0; 3]);
            add_noise(&mut z3, 1.0, seed + 200);
            let mut img = clean.clone();
            for (i, p) in img.data.iter_mut().enumerate() {
                let n = [z.data[i][0], z2.data[i][0], z3.data[i][0]];
                *p = std::array::from_fn(|k| p[k] + n[k] * (4e-4 * p[k].max(0.0) + 1e-6).sqrt());
            }
            img
        };
        for (name, colour) in [("red", [0.2f32, 0.004, 0.004]), ("blue", [0.003, 0.006, 0.15]), ("green", [0.01, 0.12, 0.005])] {
            for k in [1.0f32, 0.1, 0.02] {
                let c = colour.map(|v| v * k);
                let grey = [luminance_2020(c); 3];
                let clean = Rgb32f::from_fn(w, h, |x, _| if x < 128 { c } else { grey });
                let img = noisy_rgb(&clean, 23);
                let region = |im: &Rgb32f, xs: std::ops::Range<usize>| {
                    let mut m = [0.0f64; 3];
                    let mut n = 0.0;
                    for y in 16..240 {
                        for x in xs.clone() {
                            let p = im.data[y * w + x];
                            m = std::array::from_fn(|i| m[i] + p[i] as f64);
                            n += 1.0;
                        }
                    }
                    m.map(|v| v / n)
                };
                let diff = |m: [f64; 3]| (m[0] - m[1], m[2] - m[1]);
                let (cr, cb) = diff(c.map(f64::from));
                let size = (cr * cr + cb * cb).sqrt();
                let f: Vec<String> = [0.5f32, 1.0]
                    .iter()
                    .map(|&a| {
                        let out = run(&img, a, None);
                        let part = |xs: std::ops::Range<usize>| {
                            let (i, o) = (region(&img, xs.clone()), region(&out, xs));
                            let ((ir, ib), (or, ob)) = (diff(i), diff(o));
                            let yl = |m: [f64; 3]| 0.2627 * m[0] + 0.678 * m[1] + 0.0593 * m[2];
                            (100.0 * ((or - ir).powi(2) + (ob - ib).powi(2)).sqrt() / size, 100.0 * (yl(o) / yl(i) - 1.0))
                        };
                        let ((ci, yi), (ce, ye)) = (part(16..112), part(124..128));
                        format!("{a}: inside chroma {ci:4.1}% Y {yi:+5.1}%, edge chroma {ce:4.1}% Y {ye:+5.1}%")
                    })
                    .collect();
                eprintln!("G {name} ×{k}: {}", f.join("  "));
            }
        }
        // H: slider curves, statistics of the flat image. Flat noise left (RMS at 1 and 16 px), texture kept
        // (period 8 and 32, amplitude 0.01 on 0.1 with noise) and a 5σ line one pixel aside every two rows kept.
        let measure = |p: Params| {
            let go = |img: &Rgb32f| {
                let mut o = img.clone();
                denoise(&mut o, &p, Some(&flat_stats));
                o
            };
            let (c, i, o) = (y_of(&flat_clean), y_of(&flat), y_of(&go(&flat)));
            let n1 = 100.0 * rms_at(&o, &c, w, h, 1) / rms_at(&i, &c, w, h, 1);
            let n16 = 100.0 * rms_at(&o, &c, w, h, 16) / rms_at(&i, &c, w, h, 16);
            let tex = |period: f32, amp: f64| {
                let t: Vec<f64> = (0..w * h)
                    .map(|i| {
                        amp * ((i % w) as f32 * std::f32::consts::TAU / period).sin() as f64
                            * ((i / w) as f32 * std::f32::consts::TAU / period).sin() as f64
                    })
                    .collect();
                let mut img = Rgb32f::from_fn(w, h, |x, y| [0.1 + t[y * w + x] as f32; 3]);
                add_noise(&mut img, SIGMA, 13);
                let o = y_of(&go(&img));
                100.0 * o.iter().zip(&t).map(|(o, t)| (o - 0.1) * t).sum::<f64>() / t.iter().map(|t| t * t).sum::<f64>()
            };
            let line = {
                let shape =
                    |x: usize, y: usize| if (20..236).contains(&y) && x as f64 == (128.0 + (y as f64 - 128.0) * 0.5).round() { 1.0 } else { 0.0 };
                let s: Vec<f64> = (0..w * h).map(|i| shape(i % w, i / w)).collect();
                let mut img = Rgb32f::from_fn(w, h, |x, y| [0.05 + 5.0 * SIGMA * shape(x, y) as f32; 3]);
                add_noise(&mut img, SIGMA, 11);
                let o = y_of(&go(&img));
                100.0 * o.iter().zip(&s).map(|(o, s)| (o - 0.05) * s).sum::<f64>() / (s.iter().sum::<f64>() * (5.0 * SIGMA) as f64)
            };
            format!(
                "noise 1px {n1:5.1}% 16px {n16:5.1}%  texture p8 {:5.1}% p32 {:5.1}% p32 faint {:5.1}%  line 5σ {line:5.1}%",
                tex(8.0, 0.01),
                tex(32.0, 0.01),
                tex(32.0, 0.001)
            )
        };
        for a in [0.1f32, 0.25, 0.5, 0.75, 1.0] {
            eprintln!("H amount {a} (detail 0.5, contrast 0): {}", measure(Params { amount: a, detail: 0.5, contrast: 0.0, scale: 1.0 }));
        }
        for d in [0.0f32, 0.25, 0.75, 1.0] {
            eprintln!("H detail {d} (amount 0.5, contrast 0): {}", measure(Params { amount: 0.5, detail: d, contrast: 0.0, scale: 1.0 }));
        }
        for c in [0.25f32, 0.5, 0.75, 1.0] {
            eprintln!("H contrast {c} (amount 0.5, detail 0.5): {}", measure(Params { amount: 0.5, detail: 0.5, contrast: c, scale: 1.0 }));
        }
        // E: dark colour: chroma differences kept
        for base in [[0.001f32, 0.02, 0.04], [0.0005, 0.004, 0.01]] {
            let mut img = Rgb32f::filled(w, h, base);
            add_noise(&mut img, SIGMA, 17);
            let f: Vec<String> = amounts
                .iter()
                .map(|&a| {
                    let out = run(&img, a, Some(&flat_stats));
                    let dc = img
                        .data
                        .iter()
                        .zip(&out.data)
                        .map(|(a, b)| ((b[0] - b[1]) - (a[0] - a[1])).abs().max(((b[2] - b[1]) - (a[2] - a[1])).abs()))
                        .fold(0f32, f32::max);
                    let mean = |im: &Rgb32f| y_of(im).iter().sum::<f64>() / (w * h) as f64;
                    format!("{a}: chroma Δ max {dc:.5}, Y mean {:+.2}%", 100.0 * (mean(&out) / mean(&img) - 1.0))
                })
                .collect();
            eprintln!("E dark colour {base:?}: {}", f.join("  "));
        }
    }
}
