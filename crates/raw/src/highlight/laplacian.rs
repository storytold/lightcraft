//! Guided Laplacian highlight reconstruction: on a quarter-size copy, the clipped region is inpainted over several
//! iterations of two multi-scale (à-trous B-spline wavelet) passes. The first rebuilds each wavelet layer's clipped
//! channels from a local linear fit against the channel with the most detail there (the guide), the second diffuses
//! the chromaticity (RGB over its norm) into the clipped region with an isotropic heat equation. The result,
//! enlarged again, replaces the clipped pixels and blends into their surroundings over about two pixels.
//!
//! Translated from darktable's `src/iop/hlreconstruct/laplacian.c` (release 4.8.0: `process_laplacian_bayer`,
//! `wavelets_process`, `guide_laplacians`, `heat_PDE_diffusion`) and the B-spline helpers of
//! `src/common/bspline.h` (Copyright (C) 2010-2024 darktable developers, algorithm by Aurélien Pierre), licensed
//! GPL-3.0-or-later, so this module is used under the GPL-3.0 option of the workspace licence (`NOTICE`). The
//! defaults are darktable's (30 iterations, a 64 px reconstruction diameter at full size, no added noise, no flat
//! colour). Changes from the source:
//! - it runs on demosaiced camera RGB (darktable interpolates the mosaic bilinearly first, then remosaics), so the
//!   bilinear interpolation and remosaic steps are left out; the replaced values are blended with the input by the
//!   blurred clip mask as there;
//! - the downscale is a 4 × 4 block mean (darktable: bilinear), the upscale bilinear;
//! - the reconstruction diameter is a fraction of the long edge (64 px of a 6000 px edge), so previews and exports
//!   match;
//! - only the clipped region's bounding box, grown by the wavelets' reach, is processed;
//! - fully clipped pixels start from the colour model's white (`super::reconstruct_with`'s `white`) at or above the
//!   clip level instead of the clipped (magenta once white balanced) value, and a rebuilt clipped channel never falls
//!   below the clip level it was recorded at;
//! - a wavelet layer without detail in any channel keeps its values (the source divides by its zero variance).

use super::Clipped;
use lcc_raster::Rgb32f;
use rayon::prelude::*;

/// Downscale factor of the reconstruction (darktable's `DS_FACTOR`).
const DS: usize = 4;
/// Iterations of the two passes (darktable's default).
const ITERATIONS: usize = 30;
/// The reconstruction diameter as a fraction of the long edge: darktable's default 64 px on a 6000 px image.
const DIAMETER: f32 = 64.0 / 6000.0;
/// The most wavelet scales (darktable's `MAX_NUM_SCALES`).
const MAX_SCALES: u32 = 12;
const B_SPLINE_SIGMA: f32 = 1.055_365_1;
const B_SPLINE_TO_LAPLACIAN: f32 = 3.182_727_4;

type Px = [f32; 4];

/// darktable's `equivalent_sigma_at_step`, iteratively.
fn equivalent_sigma(sigma: f32, s: u32) -> f32 {
    let mut r = sigma;
    for k in 1..=s.min(30) {
        r = (r * r + (2f32.powi(k as i32) * sigma).powi(2)).sqrt();
    }
    r
}

/// Rebuild the clipped channels of `img` (camera RGB, clip level `clip`) with the guided Laplacian method. Returns
/// the number of pixels with a clipped channel; pixels more than two away from all of them are left exactly as they
/// were.
pub(super) fn reconstruct(img: &mut Rgb32f, wb: [f32; 3], clip: f32, white: [f32; 3], clipped: &impl Clipped) -> usize {
    let (w, h) = (img.width, img.height);
    let flags: Vec<[bool; 3]> = img.data.par_iter().enumerate().map(|(i, p)| clipped(i, p)).collect();
    let count = flags.iter().filter(|f| f.iter().any(|&c| c)).count();
    if count == 0 || w == 0 || h == 0 || !(clip.is_finite() && clip > 0.0) {
        return 0;
    }
    let wb = wb.map(|m| if m.is_finite() && m > 0.0 { m } else { 1.0 });
    let white = super::sane_white(white);
    // the white-balanced neutral in camera RGB, for fully clipped pixels
    let neutral: [f32; 3] = std::array::from_fn(|c| white[c] / wb[c]);
    let interp: Vec<Px> = img
        .data
        .par_iter()
        .zip(flags.par_iter())
        .map(|(p, f)| {
            let p = p.map(|v| if v.is_finite() { v.max(0.0) } else { clip });
            let q = if f.iter().all(|&c| c) {
                let k = (0..3).map(|c| p[c].max(clip) / neutral[c]).fold(0f32, f32::max);
                std::array::from_fn(|c| neutral[c] * k)
            } else {
                p
            };
            [q[0], q[1], q[2], (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt()]
        })
        .collect();
    let mask: Vec<Px> = flags
        .iter()
        .map(|f| {
            let on = |b: bool| if b { 1.0 } else { 0.0 };
            [on(f[0]), on(f[1]), on(f[2]), on(f.iter().any(|&c| c))]
        })
        .collect();
    // dt_box_mean(clipping_mask, …, radius 2, 1 iteration)
    let mask = box_mean(&mask, w, h, 2);
    let (dw, dh) = (w.div_ceil(DS), h.div_ceil(DS));
    let ds_mask = downscale(&mask, w, h, dw, dh);
    let ds_img = downscale(&interp, w, h, dw, dh);
    // the scales reaching the reconstruction diameter, on the downscaled image
    let radius = (DIAMETER * w.max(h) as f32 / DS as f32).max(2.0);
    let scales = (radius.log2().ceil() as u32).clamp(1, MAX_SCALES);
    // only the clipped region and the wavelets' reach around it
    let reach = 4usize.saturating_mul(1usize << scales);
    let mut bb = (usize::MAX, usize::MAX, 0usize, 0usize);
    for (i, m) in ds_mask.iter().enumerate() {
        if m[3] > 0.0 {
            let (x, y) = (i % dw, i / dw);
            bb = (bb.0.min(x), bb.1.min(y), bb.2.max(x), bb.3.max(y));
        }
    }
    if bb.0 == usize::MAX {
        return count;
    }
    let (x0, y0) = (bb.0.saturating_sub(reach), bb.1.saturating_sub(reach));
    let (x1, y1) = (bb.2.saturating_add(reach).min(dw - 1), bb.3.saturating_add(reach).min(dh - 1));
    let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
    let crop = |src: &[Px]| -> Vec<Px> { (0..cw * ch).map(|i| src.get((y0 + i / cw) * dw + x0 + i % cw).copied().unwrap_or([0.0; 4])).collect() };
    let cmask = crop(&ds_mask);
    let mut cimg = crop(&ds_img);
    for _ in 0..ITERATIONS {
        let ratios = wavelets(&cimg, &cmask, cw, ch, scales, Variant::Guide);
        cimg = wavelets(&ratios, &cmask, cw, ch, scales, Variant::Diffuse);
    }
    // enlarge (bilinear) where the blurred mask reaches and blend in
    let sample = |x: usize, y: usize| -> [f32; 3] {
        let fx = ((x as f32 + 0.5) / DS as f32 - 0.5 - x0 as f32).clamp(0.0, (cw - 1) as f32);
        let fy = ((y as f32 + 0.5) / DS as f32 - 0.5 - y0 as f32).clamp(0.0, (ch - 1) as f32);
        let (ax, ay) = (fx.floor() as usize, fy.floor() as usize);
        let (bx, by) = ((ax + 1).min(cw - 1), (ay + 1).min(ch - 1));
        let (tx, ty) = (fx - ax as f32, fy - ay as f32);
        let at = |x: usize, y: usize| cimg.get(y * cw + x).copied().unwrap_or([0.0; 4]);
        let (a, b, c, d) = (at(ax, ay), at(bx, ay), at(ax, by), at(bx, by));
        std::array::from_fn(|k| (a[k] * (1.0 - tx) + b[k] * tx) * (1.0 - ty) + (c[k] * (1.0 - tx) + d[k] * tx) * ty)
    };
    img.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let i = y * w + x;
            let op = mask.get(i).map(|m| m[3]).unwrap_or(0.0);
            if op <= 0.0 {
                continue;
            }
            let f = flags.get(i).copied().unwrap_or([false; 3]);
            let r = sample(x, y);
            let src = *p;
            *p = std::array::from_fn(|c| {
                let own = if src[c].is_finite() { src[c] } else { clip };
                let v = op * r[c].max(0.0) + (1.0 - op) * own;
                let v = if f[c] { v.max(own.max(clip)) } else { v };
                if v.is_finite() { v } else { own }
            });
        }
    });
    count
}

/// Mean over a `(2r + 1)²` window clipped to the image, per channel.
fn box_mean(src: &[Px], w: usize, h: usize, r: usize) -> Vec<Px> {
    let horiz: Vec<Px> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            let mut s = [0f32; 4];
            for xx in a..=b {
                let p = src.get(y * w + xx).copied().unwrap_or([0.0; 4]);
                (0..4).for_each(|c| s[c] += p[c]);
            }
            s.map(|v| v / (b - a + 1) as f32)
        })
        .collect();
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
            let mut s = [0f32; 4];
            for yy in a..=b {
                let p = horiz.get(yy * w + x).copied().unwrap_or([0.0; 4]);
                (0..4).for_each(|c| s[c] += p[c]);
            }
            s.map(|v| v / (b - a + 1) as f32)
        })
        .collect()
}

/// Block means of `DS` × `DS` pixels (the last row and column of blocks may be partial).
fn downscale(src: &[Px], w: usize, h: usize, dw: usize, dh: usize) -> Vec<Px> {
    (0..dw * dh)
        .into_par_iter()
        .map(|i| {
            let (bx, by) = (i % dw, i / dw);
            let mut s = [0f32; 4];
            let mut n = 0f32;
            for y in by * DS..((by + 1) * DS).min(h) {
                for x in bx * DS..((bx + 1) * DS).min(w) {
                    let p = src.get(y * w + x).copied().unwrap_or([0.0; 4]);
                    (0..4).for_each(|c| s[c] += p[c]);
                    n += 1.0;
                }
            }
            if n > 0.0 { s.map(|v| v / n) } else { s }
        })
        .collect()
}

/// À-trous B-spline blur with holes of `mult`, negatives clipped (`blur_2D_Bspline`), edges clamped.
fn blur(src: &[Px], w: usize, h: usize, mult: usize) -> Vec<Px> {
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let tap = |i: usize, k: usize, n: usize| -> usize { (i + k * mult).saturating_sub(2 * mult).min(n - 1) };
    let vert: Vec<Px> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let mut s = [0f32; 4];
            for (k, kv) in K.iter().enumerate() {
                let p = src.get(tap(y, k, h) * w + x).copied().unwrap_or([0.0; 4]);
                (0..4).for_each(|c| s[c] += kv * p[c]);
            }
            s.map(|v| v.max(0.0))
        })
        .collect();
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let mut s = [0f32; 4];
            for (k, kv) in K.iter().enumerate() {
                let p = vert.get(y * w + tap(x, k, w)).copied().unwrap_or([0.0; 4]);
                (0..4).for_each(|c| s[c] += kv * p[c]);
            }
            s.map(|v| v.max(0.0))
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Variant {
    /// `guide_laplacians`: RGB in, chromaticity ratios and norm out.
    Guide,
    /// `heat_PDE_diffusion`: ratios and norm in, RGB (and norm) out.
    Diffuse,
}

/// `wavelets_process`: decompose, reconstruct each layer where clipped, and sum back.
fn wavelets(input: &[Px], mask: &[Px], w: usize, h: usize, scales: u32, variant: Variant) -> Vec<Px> {
    let n = w * h;
    let mut out = vec![[0f32; 4]; n];
    let mut lf_in = input.to_vec();
    for s in 0..scales {
        let mult = 1usize << s;
        let lf = blur(&lf_in, w, h, mult);
        let hf: Vec<Px> = lf_in.iter().zip(&lf).map(|(a, b)| std::array::from_fn(|c| a[c] - b[c])).collect();
        let radius_sq = equivalent_sigma(B_SPLINE_SIGMA, s * DS as u32).powi(2);
        let last = s + 1 == scales;
        let at = |x: usize, y: usize| hf.get(y * w + x).copied().unwrap_or([0.0; 4]);
        out.par_iter_mut().enumerate().for_each(|(i, o)| {
            let (x, y) = (i % w, i / w);
            let m = mask.get(i).copied().unwrap_or([0.0; 4]);
            let mut high = at(x, y);
            if m[3] > 0.0 {
                let xs = [x.saturating_sub(mult), x, (x + mult).min(w - 1)];
                let ys = [y.saturating_sub(mult), y, (y + mult).min(h - 1)];
                let mut nb = [[0f32; 4]; 9];
                for (r, &yy) in ys.iter().enumerate() {
                    for (c, &xx) in xs.iter().enumerate() {
                        nb[r * 3 + c] = at(xx, yy);
                    }
                }
                match variant {
                    Variant::Guide => guide(&mut high, &nb, &m, 1.0 / radius_sq),
                    Variant::Diffuse => diffuse(&mut high, &nb, &m),
                }
            }
            if s == 0 {
                *o = high;
            } else {
                (0..4).for_each(|c| o[c] += high[c]);
            }
            if last {
                let l = lf.get(i).copied().unwrap_or([0.0; 4]);
                (0..3).for_each(|c| o[c] = (o[c] + l[c]).max(0.0));
                let src = input.get(i).copied().unwrap_or([0.0; 4]);
                match variant {
                    Variant::Guide => {
                        let norm = (o[0] * o[0] + o[1] * o[1] + o[2] * o[2]).sqrt().max(1e-6);
                        (0..3).for_each(|c| o[c] /= norm);
                        o[3] = norm;
                    }
                    Variant::Diffuse => {
                        if m[3] > 0.0 {
                            let norm = (o[0] * o[0] + o[1] * o[1] + o[2] * o[2]).sqrt();
                            if norm > 1e-4 {
                                (0..3).for_each(|c| o[c] /= norm);
                            }
                        }
                        // the norm is not diffused: the ratios take the input's back
                        o[3] = src[3];
                        (0..3).for_each(|c| o[c] *= o[3]);
                    }
                }
                if !o.iter().all(|v| v.is_finite()) {
                    *o = [0.0; 4];
                }
            }
        });
        lf_in = lf;
    }
    out
}

/// `guide_laplacians`, one pixel: each clipped channel's detail from a linear fit on the channel with the most.
fn guide(high: &mut Px, nb: &[Px; 9], m: &Px, scale_multiplier: f32) {
    let mut mean = [0f32; 3];
    for p in nb {
        (0..3).for_each(|c| mean[c] += p[c] / 9.0);
    }
    let mut var = [0f32; 3];
    for p in nb {
        (0..3).for_each(|c| var[c] += (p[c] - mean[c]).powi(2) / 9.0);
    }
    let Some(g) = (0..3).filter(|&c| var[c] > 0.0).max_by(|&a, &b| var[a].total_cmp(&var[b])) else { return };
    let mut cov = [0f32; 3];
    for p in nb {
        (0..3).for_each(|c| cov[c] += (p[c] - mean[c]) * (p[g] - mean[g]) / 9.0);
    }
    let guide = high[g];
    for c in 0..3 {
        let a = (cov[c] / var[g]).max(0.0);
        let b = mean[c] - a * mean[g];
        let k = m[c] * scale_multiplier;
        let v = k * (a * guide + b) + (1.0 - k) * high[c];
        if v.is_finite() {
            high[c] = v;
        }
    }
}

/// `heat_PDE_diffusion`, one pixel: isotropic diffusion of the clipped channels (no flat-colour term).
fn diffuse(high: &mut Px, nb: &[Px; 9], m: &Px) {
    const KERNEL: [f32; 9] = [0.25, 0.5, 0.25, 0.5, -3.0, 0.5, 0.25, 0.5, 0.25];
    for c in 0..3 {
        let lap: f32 = nb.iter().zip(KERNEL).map(|(p, k)| p[c] * k).sum();
        let v = high[c] + m[c] * lap / B_SPLINE_TO_LAPLACIAN;
        if v.is_finite() {
            high[c] = v;
        }
    }
}
