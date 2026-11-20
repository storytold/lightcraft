//! AI Defringe inference ([`Model`]): a defringe network run over a whole picture, and the fixed guards that turn
//! its output into a corrected picture (docs/ai-defringe.md).
//!
//! The picture is linear Rec.2020 at the Defringe stage, scaled so its brightest highlights are near 1, with a mask
//! of the pixels next to clipped highlights. The network sees it as 256 px tiles of five planes (OkLab L, a, b, log
//! luminance and the clip mask) folded space-to-depth into 20 half-resolution channels, and gives back three
//! full-resolution planes: a change to OkLab a and b, and a gate. Tiles overlap by 32 px and are blended with linear
//! ramps; tiles with no edge are skipped. Lightness is never changed. The change is applied only near edges, is
//! bounded by the pixel's own chroma, stops at grey instead of crossing it, may not make a pixel green that wasn't,
//! may not leave a pixel more colourful than its neighbourhood, and keeps only the part that moves the pixel toward
//! the colour around it. Pixels the change leaves alone keep their exact values.
//!
//! The picture is worked through in bands of rows, so memory stays bounded on large photos; the result is the same
//! as working on it whole.

use crate::cpu::NetRunner;
use crate::net::{Net, NetError};
use crate::run::{Error, TileRunner};
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Side of a tile, in pixels (the network takes half of it).
pub const TILE: usize = 256;
/// Pixels neighbouring tiles share.
pub const OVERLAP: usize = 32;
/// Channels the network takes: five planes folded 2 × 2.
pub const IN_CHANNELS: usize = 20;
/// The largest change of OkLab chroma the network can ask for.
const DELTA_MAX: f32 = 0.15;
/// Rows worked on at a time.
const BAND: usize = 768;
/// Rows a band reads beyond its own: the guards' neighbourhoods (3 for the edge and chroma limits, 2 + 7 for the
/// pull toward the surrounding colour), rounded up.
const HALO: usize = 12;
/// Tiles run at once (each holds about 3 MB of input and output).
const BATCH: usize = 32;
/// A tile is worth running when its lightness changes this much within 7 × 7 pixels somewhere.
const EDGE: f32 = 0.05;
/// Pixels under this much change keep their exact values.
const MOVED: f32 = 1e-5;

/// A loaded defringe network.
#[derive(Clone)]
pub struct Model {
    runner: NetRunner,
}

impl Model {
    /// Check that `net` has the defringe contract (20 channels of 128 × 128 in, 3 of 256 × 256 out) and prepare it.
    pub fn new(net: &Net) -> Result<Self, NetError> {
        if net.in_channels != IN_CHANNELS || net.out_channels() != 3 {
            return Err(NetError::Invalid(format!(
                "a defringe model takes {IN_CHANNELS} channels and gives 3; this one takes {} and gives {}",
                net.in_channels,
                net.out_channels()
            )));
        }
        let runner = NetRunner::for_tiles(net, TILE / 2)?;
        let m = Self { runner };
        let out = m.runner.run(&vec![0.0; IN_CHANNELS * (TILE / 2) * (TILE / 2)]).map_err(|e| NetError::Invalid(e.to_string()))?;
        if out.len() != 3 * TILE * TILE {
            return Err(NetError::Invalid("a defringe model must give its planes at twice its input's size".into()));
        }
        Ok(m)
    }

    /// Memory one running tile needs.
    pub fn bytes_per_tile(&self) -> usize {
        self.runner.bytes_per_tile()
    }

    /// The pixels `correct` changes in a `w × h` picture `rgb` (linear Rec.2020, row by row) with clip mask `clip`,
    /// as (index, corrected colour), in index order. `progress` gets (tiles done, tiles in all); `cancel` stops it.
    pub fn correct(
        &self,
        rgb: &[[f32; 3]],
        clip: &[bool],
        w: usize,
        h: usize,
        progress: &(dyn Fn(usize, usize) + Sync),
        cancel: &AtomicBool,
    ) -> Result<Vec<(u32, [f32; 3])>, Error> {
        let n = w.checked_mul(h).filter(|&n| n > 0 && u32::try_from(n).is_ok());
        if n.is_none_or(|n| rgb.len() != n || clip.len() != n) {
            return Err(Error::Input("the picture and its clip mask don't match its size".into()));
        }
        let stop = || if cancel.load(Ordering::Relaxed) { Err(Error::Cancelled) } else { Ok(()) };
        stop()?;
        if rgb.iter().any(|p| p.iter().any(|v| !v.is_finite())) {
            return Err(Error::Input("the picture has non-finite values".into()));
        }
        let pic = Padded::new(rgb, clip, w, h);
        let ys = grid(pic.h);
        let xs = grid(pic.w);
        let tiles: Vec<(usize, usize)> = ys.iter().flat_map(|&y| xs.iter().map(move |&x| (y, x))).collect();
        let wanted: Vec<bool> = tiles.par_iter().map(|&(y, x)| pic.has_edge(y, x)).collect();
        let bands: Vec<(usize, usize, usize, usize)> = (0..h)
            .step_by(BAND)
            .map(|r0| {
                let r1 = (r0 + BAND).min(h);
                (r0, r1, r0.saturating_sub(HALO), (r1 + HALO).min(h))
            })
            .collect();
        let runs = |e0: usize, e1: usize| -> Vec<(usize, usize)> {
            tiles.iter().zip(&wanted).filter(|(t, on)| **on && t.0 < e1 && t.0 + TILE > e0).map(|(t, _)| *t).collect()
        };
        let total: usize = bands.iter().map(|b| runs(b.2, b.3).len()).sum::<usize>().max(1);
        let mut done = 0;
        progress(0, total);
        let ramp = ramp();
        let mut changed = Vec::new();
        for &(r0, r1, e0, e1) in &bands {
            stop()?;
            let rows = e1 - e0;
            let mut acc = vec![[0f32; 3]; rows * w];
            let mut wsum = vec![0f32; rows * w];
            for batch in runs(e0, e1).chunks(BATCH) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let outs: Vec<Vec<f32>> = batch.par_iter().map(|&(y, x)| self.runner.run(&pic.tile_input(y, x))).collect::<Result<_, _>>()?;
                for (&(ty, tx), out) in batch.iter().zip(&outs) {
                    if out.len() != 3 * TILE * TILE {
                        return Err(Error::Model("planes of the wrong size".into()));
                    }
                    for yy in ty.max(e0)..(ty + TILE).min(e1) {
                        let wy = ramp[yy - ty];
                        for xx in tx..(tx + TILE).min(w) {
                            let wt = wy * ramp[xx - tx];
                            let o = (yy - ty) * TILE + (xx - tx);
                            let i = (yy - e0) * w + xx;
                            for (c, a) in acc[i].iter_mut().enumerate() {
                                *a += out[c * TILE * TILE + o] * wt;
                            }
                            wsum[i] += wt;
                        }
                    }
                }
                done += batch.len();
                progress(done, total);
            }
            let raw: Vec<[f32; 3]> =
                acc.iter().zip(&wsum).map(|(a, &s)| if s > 0.0 { a.map(|v| v / s.max(1e-6)) } else { [0.0, 0.0, -20.0] }).collect();
            band(rgb, w, h, (r0, r1, e0, e1), &raw, &mut changed);
        }
        stop()?;
        progress(total, total);
        Ok(changed)
    }
}

/// Tile positions along a side of `n` (at least [`TILE`]) pixels: every `TILE − OVERLAP`, and one flush with the end.
fn grid(n: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..n - TILE).step_by(TILE - OVERLAP).collect();
    v.push(n - TILE);
    v
}

/// A tile's blend weight along one side: rising over the first [`OVERLAP`] pixels, falling over the last.
fn ramp() -> [f32; TILE] {
    let mut r = [1f32; TILE];
    for k in 0..OVERLAP {
        let v = (k + 1) as f32 / (OVERLAP + 1) as f32;
        r[k] = v;
        r[TILE - 1 - k] = v;
    }
    r
}

/// The picture, extended to at least one tile each way (colours repeated from the edge, no clip mask there).
struct Padded<'a> {
    rgb: std::borrow::Cow<'a, [[f32; 3]]>,
    clip: std::borrow::Cow<'a, [bool]>,
    w: usize,
    h: usize,
}

impl<'a> Padded<'a> {
    fn new(rgb: &'a [[f32; 3]], clip: &'a [bool], w: usize, h: usize) -> Self {
        if w >= TILE && h >= TILE {
            return Self { rgb: rgb.into(), clip: clip.into(), w, h };
        }
        let (pw, ph) = (w.max(TILE), h.max(TILE));
        let mut prgb = Vec::with_capacity(pw * ph);
        let mut pclip = Vec::with_capacity(pw * ph);
        for y in 0..ph {
            for x in 0..pw {
                prgb.push(rgb[y.min(h - 1) * w + x.min(w - 1)]);
                pclip.push(x < w && y < h && clip[y * w + x]);
            }
        }
        Self { rgb: prgb.into(), clip: pclip.into(), w: pw, h: ph }
    }

    /// Whether lightness changes by [`EDGE`] within 7 × 7 pixels somewhere in the tile at (`ty`, `tx`).
    fn has_edge(&self, ty: usize, tx: usize) -> bool {
        let (y0, x0) = (ty.saturating_sub(3), tx.saturating_sub(3));
        let (y1, x1) = ((ty + TILE + 3).min(self.h), (tx + TILE + 3).min(self.w));
        let (ww, hh) = (x1 - x0, y1 - y0);
        let l: Vec<f32> = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (y, x))).map(|(y, x)| oklab(self.rgb[y * self.w + x])[0]).collect();
        let (hi, lo) = (max_filter(&l, ww, hh, 3), max_filter(&l.iter().map(|v| -v).collect::<Vec<_>>(), ww, hh, 3));
        (ty - y0..ty - y0 + TILE).any(|y| (tx - x0..tx - x0 + TILE).any(|x| hi[y * ww + x] + lo[y * ww + x] >= EDGE))
    }

    /// The network's input for the tile at (`ty`, `tx`): its five planes folded 2 × 2 into 20 channels.
    fn tile_input(&self, ty: usize, tx: usize) -> Vec<f32> {
        const S: usize = TILE / 2;
        let mut input = vec![0f32; IN_CHANNELS * S * S];
        for y in 0..TILE {
            for x in 0..TILE {
                let i = (ty + y) * self.w + tx + x;
                let p = self.rgb[i];
                let lab = oklab(p);
                let planes = [lab[0], lab[1], lab[2], log_luminance(p), f32::from(u8::from(self.clip[i]))];
                let (sub, at) = ((y % 2) * 2 + x % 2, (y / 2) * S + x / 2);
                for (c, v) in planes.into_iter().enumerate() {
                    input[(c * 4 + sub) * S * S + at] = v;
                }
            }
        }
        input
    }
}

/// Compose rows `r0..r1` of the picture from the network's `raw` planes for rows `e0..e1` (the band and its halo),
/// adding the pixels that change to `changed`.
fn band(rgb: &[[f32; 3]], w: usize, h: usize, (r0, r1, e0, e1): (usize, usize, usize, usize), raw: &[[f32; 3]], changed: &mut Vec<(u32, [f32; 3])>) {
    // OkLab of rows e0 − 3 .. e1 + 3: the edge and chroma limits read 7 × 7 around each pixel
    let (l0, l1) = (e0.saturating_sub(3), (e1 + 3).min(h));
    let lab: Vec<[f32; 3]> = rgb[l0 * w..l1 * w].par_iter().map(|&p| oklab(p)).collect();
    let lrows = l1 - l0;
    let lp: Vec<f32> = lab.iter().map(|p| p[0]).collect();
    let chroma: Vec<f32> = lab.iter().map(|p| (p[1] * p[1] + p[2] * p[2] + 1e-12).sqrt()).collect();
    let l_hi = max_filter(&lp, w, lrows, 3);
    let l_lo = max_filter(&lp.iter().map(|v| -v).collect::<Vec<_>>(), w, lrows, 3);
    let c_hi = max_filter(&chroma, w, lrows, 3);
    let rows = e1 - e0;
    // the guarded change, for rows e0..e1
    let ab: Vec<[f32; 2]> = (0..rows * w)
        .map(|i| {
            let j = (i / w + e0 - l0) * w + i % w;
            [lab[j][1], lab[j][2]]
        })
        .collect();
    let new: Vec<[f32; 2]> = (0..rows * w)
        .into_par_iter()
        .map(|i| {
            let j = (i / w + e0 - l0) * w + i % w;
            compose(ab[i], chroma[j], l_hi[j] + l_lo[j], c_hi[j], raw[i])
        })
        .collect();
    let moved: Vec<f32> = ab.iter().zip(&new).map(|(a, n)| f32::from(u8::from(dist(*a, *n) > 1e-4))).collect();
    let keep: Vec<f32> = max_filter(&moved, w, rows, 2).iter().map(|v| 1.0 - v).collect();
    let keep_a: Vec<f32> = ab.iter().zip(&keep).map(|(a, k)| a[0] * k).collect();
    let keep_b: Vec<f32> = ab.iter().zip(&keep).map(|(a, k)| a[1] * k).collect();
    let (den, num_a, num_b) = (box_sum(&keep, w, rows, 7), box_sum(&keep_a, w, rows, 7), box_sum(&keep_b, w, rows, 7));
    let out: Vec<Option<(u32, [f32; 3])>> = (r0 * w..r1 * w)
        .into_par_iter()
        .map(|g| {
            let i = g - e0 * w;
            let (y, x) = (i / w, i % w);
            let n = ((y + 7).min(rows - 1) - y.saturating_sub(7) + 1) * ((x + 7).min(w - 1) - x.saturating_sub(7) + 1);
            let a = ab[i];
            let mut nab = new[i];
            if moved[i] > 0.0 {
                // toward the mean colour of the untouched pixels around, at most up to it
                let bg = if den[i] / n as f32 > 1e-3 { [num_a[i] / den[i], num_b[i] / den[i]] } else { [0.0, 0.0] };
                let (d, to) = ([nab[0] - a[0], nab[1] - a[1]], [bg[0] - a[0], bg[1] - a[1]]);
                let t = ((d[0] * to[0] + d[1] * to[1]) / (to[0] * to[0] + to[1] * to[1]).max(1e-8)).clamp(0.0, 1.0);
                // nor take it past grey, or make it green when it wasn't
                let ca = (a[0] * a[0] + a[1] * a[1] + 1e-12).sqrt();
                let d = not_past_grey(a, ca, [t * to[0], t * to[1]]);
                nab = [a[0] + d[0], a[1] + d[1]];
                let cn = (nab[0] * nab[0] + nab[1] * nab[1] + 1e-12).sqrt();
                if green(nab, cn) && !green(a, ca) {
                    let s = (0.01 / cn).min(1.0);
                    nab = [nab[0] * s, nab[1] * s];
                }
            }
            if dist(a, nab) <= MOVED {
                return None;
            }
            let l = lab[(y + e0 - l0) * w + x][0];
            Some((g as u32, lab_to_rgb([l, nab[0], nab[1]]).map(|v| v.max(0.0))))
        })
        .collect();
    changed.extend(out.into_iter().flatten());
}

/// Change `d` of colour `ab` (chroma `c`), shortened so that it goes to grey but not past it: the colour may
/// lose all of its chroma, but not come out on the other side of grey.
fn not_past_grey(ab: [f32; 2], c: f32, d: [f32; 2]) -> [f32; 2] {
    let against = -(d[0] * ab[0] + d[1] * ab[1]);
    if against <= 0.0 {
        return d;
    }
    let t = (c * c / against.max(1e-12)).min(1.0);
    [d[0] * t, d[1] * t]
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn green(ab: [f32; 2], c: f32) -> bool {
    let hue = ab[1].atan2(ab[0]).to_degrees().rem_euclid(360.0);
    c > 0.01 && (100.0..=170.0).contains(&hue)
}

/// The new OkLab (a, b) of a pixel with colour `ab` and chroma `c`, its 7 × 7 lightness range `range` and highest
/// chroma `c_hi`, from the network's (Δa, Δb, gate) `raw`.
fn compose(ab: [f32; 2], c: f32, range: f32, c_hi: f32, raw: [f32; 3]) -> [f32; 2] {
    let near_edge = smoothstep(0.05, 0.20, range);
    let gate = 1.0 / (1.0 + (-raw[2]).exp());
    let k = near_edge * gate * DELTA_MAX;
    let d = [k * raw[0].tanh(), k * raw[1].tanh()];
    // no larger than the pixel's own chroma (plus a little)
    let dn = (d[0] * d[0] + d[1] * d[1] + 1e-12).sqrt();
    let s = ((c + 0.01) / dn).min(1.0);
    let d = [d[0] * s, d[1] * s];
    let d = not_past_grey(ab, c, d);
    let mut new = [ab[0] + d[0], ab[1] + d[1]];
    // not green unless it was
    let cn = (new[0] * new[0] + new[1] * new[1] + 1e-12).sqrt();
    if green(new, cn) && !green(ab, c) {
        let s = (0.01 / cn).min(1.0);
        new = [new[0] * s, new[1] * s];
    }
    // no more colourful than the neighbourhood
    let cn = (new[0] * new[0] + new[1] * new[1] + 1e-12).sqrt();
    let s = (c_hi * 1.2 / cn).min(1.0);
    [new[0] * s, new[1] * s]
}

/// The largest value within `r` pixels (a (2r + 1)² square, inside the picture) of each pixel of a `w × h` plane.
fn max_filter(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut rows = vec![0f32; w * h];
    for y in 0..h {
        let line = &v[y * w..(y + 1) * w];
        for x in 0..w {
            rows[y * w + x] = line[x.saturating_sub(r)..(x + r + 1).min(w)].iter().copied().fold(f32::NEG_INFINITY, f32::max);
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = (y.saturating_sub(r)..(y + r + 1).min(h)).map(|yy| rows[yy * w + x]).fold(f32::NEG_INFINITY, f32::max);
        }
    }
    out
}

/// The sum within `r` pixels (inside the picture) of each pixel of a `w × h` plane.
fn box_sum(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut rows = vec![0f32; w * h];
    for y in 0..h {
        let mut run = 0f64;
        let mut prefix = vec![0f64; w + 1];
        for x in 0..w {
            run += f64::from(v[y * w + x]);
            prefix[x + 1] = run;
        }
        for x in 0..w {
            rows[y * w + x] = (prefix[(x + r + 1).min(w)] - prefix[x.saturating_sub(r)]) as f32;
        }
    }
    let mut out = vec![0f32; w * h];
    let mut prefix = vec![0f64; (h + 1) * w];
    for y in 0..h {
        for x in 0..w {
            prefix[(y + 1) * w + x] = prefix[y * w + x] + f64::from(rows[y * w + x]);
        }
    }
    for y in 0..h {
        let (a, b) = (y.saturating_sub(r), (y + r + 1).min(h));
        for x in 0..w {
            out[y * w + x] = (prefix[b * w + x] - prefix[a * w + x]) as f32;
        }
    }
    out
}

const REC2020_TO_709: [[f32; 3]; 3] = [[1.6605, -0.5876, -0.0728], [-0.1246, 1.1329, -0.0083], [-0.0182, -0.1006, 1.1187]];
const M1: [[f32; 3]; 3] =
    [[0.412_221_47, 0.536_332_55, 0.051_445_995], [0.211_903_5, 0.680_699_5, 0.107_396_96], [0.088_302_46, 0.281_718_85, 0.629_978_7]];
const M2: [[f32; 3]; 3] =
    [[0.210_454_26, 0.793_617_8, -0.004_072_047], [1.977_998_5, -2.428_592_2, 0.450_593_7], [0.025_904_037, 0.782_771_77, -0.808_675_77]];

fn mul(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

fn inverse(m: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let m = m.map(|r| r.map(f64::from));
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let c = |r: usize, k: usize| {
        let (r1, r2, k1, k2) = ((r + 1) % 3, (r + 2) % 3, (k + 1) % 3, (k + 2) % 3);
        m[r1][k1] * m[r2][k2] - m[r1][k2] * m[r2][k1]
    };
    [0, 1, 2].map(|i| [0, 1, 2].map(|j| (c(j, i) / det) as f32))
}

/// Linear Rec.2020 → OkLab (by way of linear Rec.709, negative values clamped first).
pub fn oklab(p: [f32; 3]) -> [f32; 3] {
    let x = mul(&REC2020_TO_709, p.map(|v| v.max(0.0))).map(|v| v.max(0.0));
    mul(&M2, mul(&M1, x).map(|v| v.max(1e-9).cbrt()))
}

/// OkLab → linear Rec.2020, the inverse of [`oklab`].
pub fn lab_to_rgb(lab: [f32; 3]) -> [f32; 3] {
    use std::sync::OnceLock;
    static INV: OnceLock<[[[f32; 3]; 3]; 3]> = OnceLock::new();
    let [m2i, m1i, ri] = INV.get_or_init(|| [inverse(&M2), inverse(&M1), inverse(&REC2020_TO_709)]);
    mul(ri, mul(m1i, mul(m2i, lab).map(|v| v * v * v)))
}

/// The luminance plane the network reads: log2 of Rec.2020 luminance, over 12.
fn log_luminance(p: [f32; 3]) -> f32 {
    let y = 0.2627 * p[0] + 0.6780 * p[1] + 0.0593 * p[2];
    (y.max(0.0) + 2f32.powi(-12)).log2() / 12.0
}

/// The clip mask a defringe model reads, for a `w × h` picture: pixels whose 5 × 5 mean of the brightest channel is
/// within 7 % of `top`.
/// Worked row by row with running sums, so it needs no picture-sized buffers beyond the mask.
pub fn clip_mask(rgb: &[[f32; 3]], w: usize, h: usize, top: f32) -> Vec<bool> {
    if w.checked_mul(h) != Some(rgb.len()) || rgb.is_empty() {
        return vec![false; rgb.len()];
    }
    // a row's 5-wide sums of the brightest channel
    let row = |y: usize| -> Vec<f64> {
        let line = &rgb[y * w..(y + 1) * w];
        let m = |x: usize| f64::from(line[x][0].max(line[x][1]).max(line[x][2]));
        let mut out = vec![0f64; w];
        let mut run: f64 = (0..3.min(w)).map(m).sum();
        for (x, o) in out.iter_mut().enumerate() {
            *o = run;
            if x + 3 < w {
                run += m(x + 3);
            }
            if x >= 2 {
                run -= m(x - 2);
            }
        }
        out
    };
    let mut cols = vec![0f64; w];
    for y in 0..3.min(h) {
        for (c, v) in cols.iter_mut().zip(row(y)) {
            *c += v;
        }
    }
    let mut mask = Vec::with_capacity(w * h);
    for y in 0..h {
        let ny = (y + 2).min(h - 1) - y.saturating_sub(2) + 1;
        for (x, c) in cols.iter().enumerate() {
            let nx = (x + 2).min(w - 1) - x.saturating_sub(2) + 1;
            mask.push(c / (nx * ny) as f64 >= 0.93 * f64::from(top));
        }
        if y + 3 < h {
            for (c, v) in cols.iter_mut().zip(row(y + 3)) {
                *c += v;
            }
        }
        if y >= 2 {
            for (c, v) in cols.iter_mut().zip(row(y - 2)) {
                *c -= v;
            }
        }
    }
    mask
}

/// The 99.9th percentile of the brightest channel (over every third pixel): the value a picture is divided by so
/// its highlights sit near 1, as the model was trained.
pub fn top(rgb: &[[f32; 3]]) -> f32 {
    let mut v: Vec<f32> = rgb.iter().step_by(3).map(|p| p[0].max(p[1]).max(p[2])).filter(|v| v.is_finite()).collect();
    if v.is_empty() {
        return 1.0;
    }
    let k = ((v.len() - 1) as f64 * 0.999) as usize;
    v.select_nth_unstable_by(k, f32::total_cmp).1.max(1e-6)
}

#[cfg(test)]
mod tests;
