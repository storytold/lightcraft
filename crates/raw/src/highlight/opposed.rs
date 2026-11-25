//! Inpaint opposed highlight reconstruction: a clipped channel is rebuilt from the mean of the two other
//! ("opposed") channels, taken as cube roots, plus a chrominance offset measured on the unclipped content right
//! around the clipped areas.
//!
//! Translated from darktable's `src/iop/hlreconstruct/opposed.c` (release 4.8.0: `_process_linear_opposed`,
//! `_calc_linear_refavg`, `_mask_dilated`; Copyright (C) 2022-2024 darktable developers, algorithm by
//! garagecoder, Iain and Hanno Schwalm), licensed GPL-3.0-or-later, so this module is used under the GPL-3.0 option
//! of the workspace licence (`NOTICE`). The linear (sRAW) variant is the one ported, since this crate reconstructs
//! on demosaiced camera RGB rather than on the mosaic. Changes from the source:
//! - the data is camera RGB before white balance with one clip level for all channels; the algorithm runs on the
//!   white-balanced values (as darktable's does) and divides the multipliers back out;
//! - borders are handled with bounds checks instead of being skipped, and the 3 × 3 cell grid covers partial cells;
//! - a pixel with all three channels clipped becomes the colour model's white (`super::reconstruct_with`'s
//!   `white`) at the brightest rebuilt level, so fully blown areas stay neutral when darkened (#523);
//! - non-finite results fall back to the clip level.

use super::Clipped;
use lcc_raster::Rgb32f;
use rayon::prelude::*;

/// darktable's `HL_POWERF`: the opposed mean is taken over cube roots.
const HL_POWERF: f32 = 3.0;
/// Pixels per side of a mask cell (darktable's `_raw_to_cmap`).
const CELL: usize = 3;
/// The fewest unclipped samples a channel's chrominance offset is measured from (linear variant).
const MIN_SAMPLES: f32 = 30.0;

/// The opposed mean for channel `c` of white-balanced `v` (`_calc_linear_refavg`).
fn refavg(v: &[f32; 3], c: usize) -> f32 {
    let r = v.map(|x| x.max(0.0).powf(1.0 / HL_POWERF));
    let opp = match c {
        0 => 0.5 * (r[1] + r[2]),
        1 => 0.5 * (r[0] + r[2]),
        _ => 0.5 * (r[0] + r[1]),
    };
    opp.powf(HL_POWERF)
}

/// Rebuild the clipped channels of `img` (camera RGB, clip level `clip`) with the inpaint-opposed method. Returns
/// the number of pixels with a clipped channel; the others are left exactly as they were.
pub(super) fn reconstruct(img: &mut Rgb32f, wb: [f32; 3], clip: f32, white: [f32; 3], clipped: &impl Clipped) -> usize {
    let (w, h) = (img.width, img.height);
    let flags: Vec<[bool; 3]> = img.data.par_iter().enumerate().map(|(i, p)| clipped(i, p)).collect();
    let count = flags.iter().filter(|f| f.iter().any(|&c| c)).count();
    if count == 0 || w == 0 || h == 0 || !(clip.is_finite() && clip > 0.0) {
        return 0;
    }
    let wb = wb.map(|m| if m.is_finite() && m > 0.0 { m } else { 1.0 });
    let white = super::sane_white(white);
    // which cells hold a clipped sample of each colour, then that grown by about three cells
    let (mw, mh) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let mut mask = vec![[false; 3]; mw * mh];
    for (i, f) in flags.iter().enumerate() {
        let (x, y) = (i % w, i / w);
        if let Some(m) = mask.get_mut((y / CELL) * mw + x / CELL) {
            for c in 0..3 {
                m[c] |= f[c];
            }
        }
    }
    let dilated: Vec<[bool; 3]> = (0..mw * mh)
        .into_par_iter()
        .map(|i| {
            let (x, y) = ((i % mw) as isize, (i / mw) as isize);
            let mut out = [false; 3];
            for dy in -3isize..=3 {
                for dx in -3isize..=3 {
                    if dx.abs() == 3 && dy.abs() == 3 {
                        continue;
                    }
                    let (xx, yy) = (x + dx, y + dy);
                    if xx < 0 || yy < 0 || xx >= mw as isize || yy >= mh as isize {
                        continue;
                    }
                    if let Some(m) = mask.get(yy as usize * mw + xx as usize) {
                        for c in 0..3 {
                            out[c] |= m[c];
                        }
                    }
                }
            }
            out
        })
        .collect();
    // the chrominance offset: unclipped samples well above black right around the clipped areas
    let (sums, cnts) = img
        .data
        .par_iter()
        .enumerate()
        .fold(
            || ([0f64; 3], [0f32; 3]),
            |(mut s, mut n), (i, p)| {
                let (x, y) = (i % w, i / w);
                let near = dilated.get((y / CELL) * mw + x / CELL).copied().unwrap_or([false; 3]);
                let f = flags.get(i).copied().unwrap_or([true; 3]);
                let v = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
                for c in 0..3 {
                    let lim = clip * wb[c];
                    if near[c] && !f[c] && v[c].is_finite() && v[c] > 0.2 * lim && v[c] < lim {
                        let d = v[c] - refavg(&v, c);
                        if d.is_finite() {
                            s[c] += d as f64;
                            n[c] += 1.0;
                        }
                    }
                }
                (s, n)
            },
        )
        .reduce(|| ([0f64; 3], [0f32; 3]), |(a, b), (c, d)| ([a[0] + c[0], a[1] + c[1], a[2] + c[2]], [b[0] + d[0], b[1] + d[1], b[2] + d[2]]));
    let chroma: [f32; 3] = std::array::from_fn(|c| if cnts[c] > MIN_SAMPLES { (sums[c] / cnts[c] as f64) as f32 } else { 0.0 });
    img.data.par_iter_mut().zip(flags.par_iter()).for_each(|(p, f)| {
        if !f.iter().any(|&c| c) {
            return;
        }
        let v = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
        let mut out = v;
        for c in 0..3 {
            let lim = clip * wb[c];
            let own = if v[c].is_finite() { v[c].max(0.0) } else { lim };
            out[c] = if f[c] {
                let r = refavg(&v, c) + chroma[c];
                let r = if r.is_finite() { own.max(r) } else { own };
                r.max(lim)
            } else {
                own
            };
        }
        if f.iter().all(|&c| c) {
            // fully blown: the colour model's white, as bright as the brightest rebuilt channel
            let level = (0..3).map(|c| out[c] / white[c]).fold(0f32, f32::max);
            out = std::array::from_fn(|c| white[c] * level);
        }
        *p = std::array::from_fn(|c| {
            let q = out[c] / wb[c];
            if q.is_finite() { q } else { clip }
        });
    });
    count
}
