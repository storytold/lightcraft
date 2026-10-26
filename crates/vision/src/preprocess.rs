//! Photo → model input: resize to the square input as the reference preprocessing does, then
//! `(x/255 − mean) / std` per channel, planar (CHW) `f32`.
//!
//! The reference (Hugging Face `SiglipImageProcessor`) resizes 8-bit images with Pillow, whose
//! resampling is a separable convolution that widens the filter when shrinking (antialiasing),
//! in 22-bit fixed point with an 8-bit image between the horizontal and vertical passes. This
//! module computes the same thing, so the model sees the same pixels (independent
//! implementation of that published algorithm).

use crate::manifest::{Filter, Fit, ImageSpec};
use crate::{Error, Result, Rgb8};

/// Largest image accepted (pixels): 200 MP. Bigger photos should come as a preview.
pub const MAX_PIXELS: usize = 200_000_000;
/// Longest edge accepted.
pub const MAX_EDGE: usize = 65_535;

const PRECISION_BITS: u32 = 32 - 8 - 2;

fn bilinear(x: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 { 1.0 - x } else { 0.0 }
}

fn bicubic(x: f64) -> f64 {
    const A: f64 = -0.5;
    let x = x.abs();
    if x < 1.0 {
        ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * A
    } else {
        0.0
    }
}

/// Per output sample: the first input sample and the fixed-point weights.
struct Coeffs {
    first: Vec<usize>,
    weights: Vec<Vec<i64>>,
}

/// The coefficients of the output samples in `range` (of `out_size`): each depends only on its own index, so a range
/// gives exactly those samples of the whole resampling.
fn coeffs(in_size: usize, out_size: usize, range: std::ops::Range<usize>, filter: Filter) -> Coeffs {
    let (f, support): (fn(f64) -> f64, f64) = match filter {
        Filter::Bilinear => (bilinear, 1.0),
        Filter::Bicubic => (bicubic, 2.0),
    };
    let scale = in_size as f64 / out_size.max(1) as f64;
    let filterscale = scale.max(1.0);
    let support = support * filterscale;
    let ss = 1.0 / filterscale;
    let mut first = Vec::with_capacity(range.len());
    let mut weights = Vec::with_capacity(range.len());
    for xx in range {
        let center = (xx as f64 + 0.5) * scale;
        // C casts truncate toward zero
        let xmin = ((center - support + 0.5) as i64).max(0) as usize;
        let xmax = ((center + support + 0.5) as i64).clamp(0, in_size as i64) as usize;
        let n = xmax.saturating_sub(xmin);
        let mut k: Vec<f64> = (0..n).map(|x| f(((x + xmin) as f64 - center + 0.5) * ss)).collect();
        let total: f64 = k.iter().sum();
        if total != 0.0 {
            k.iter_mut().for_each(|w| *w /= total);
        }
        let one = f64::from(1u32 << PRECISION_BITS);
        weights.push(k.iter().map(|&w| if w < 0.0 { (-0.5 + w * one) as i64 } else { (0.5 + w * one) as i64 }).collect());
        first.push(xmin);
    }
    Coeffs { first, weights }
}

fn clip8(v: i64) -> u8 {
    (v >> PRECISION_BITS).clamp(0, 255) as u8
}

/// Resample interleaved RGB `w × h` to `ow × oh` (Pillow's 8-bit convolution resampling).
fn resample(rgb: &[u8], w: usize, h: usize, ow: usize, oh: usize, filter: Filter) -> Vec<u8> {
    resample_window(rgb, w, h, (ow, oh), (0..ow, 0..oh), filter)
}

/// The columns `xs` and rows `ys` of [`resample`] to `ow × oh` (`xs.len() × ys.len()`, interleaved RGB), worked out
/// for that window only: a center crop of a very long image never makes the whole long resampled image.
fn resample_window(
    rgb: &[u8],
    w: usize,
    h: usize,
    (ow, oh): (usize, usize),
    (xs, ys): (std::ops::Range<usize>, std::ops::Range<usize>),
    filter: Filter,
) -> Vec<u8> {
    let half = 1i64 << (PRECISION_BITS - 1);
    let cw = xs.len();
    // the vertical pass's coefficients first: they say which input rows the horizontal pass needs
    let vertical = (oh != h).then(|| coeffs(h, oh, ys.clone(), filter));
    let (r0, r1) = match &vertical {
        Some(c) => (c.first.iter().copied().min().unwrap_or(0), c.first.iter().zip(&c.weights).map(|(f, k)| f + k.len()).max().unwrap_or(0).min(h)),
        None => (ys.start.min(h), ys.end.min(h)),
    };
    let rows = r1.saturating_sub(r0);
    // horizontal pass, over those rows
    let mut mid = vec![0u8; cw * rows * 3];
    let horizontal = (ow != w).then(|| coeffs(w, ow, xs.clone(), filter));
    for y in 0..rows {
        let row = (r0 + y) * w;
        for x in 0..cw {
            for ch in 0..3 {
                let v = match &horizontal {
                    Some(c) => {
                        let (Some(first), Some(k)) = (c.first.get(x), c.weights.get(x)) else { continue };
                        let mut s = half;
                        for (i, wgt) in k.iter().enumerate() {
                            s += i64::from(rgb.get((row + first + i) * 3 + ch).copied().unwrap_or(0)) * wgt;
                        }
                        clip8(s)
                    }
                    None => rgb.get((row + xs.start + x) * 3 + ch).copied().unwrap_or(0),
                };
                if let Some(o) = mid.get_mut((y * cw + x) * 3 + ch) {
                    *o = v;
                }
            }
        }
    }
    let Some(c) = vertical else { return mid };
    // vertical pass
    let mut out = vec![0u8; cw * ys.len() * 3];
    for (y, (first, k)) in c.first.iter().zip(&c.weights).enumerate() {
        for x in 0..cw {
            for ch in 0..3 {
                let mut s = half;
                for (i, wgt) in k.iter().enumerate() {
                    let r = (first + i).saturating_sub(r0);
                    s += i64::from(mid.get((r * cw + x) * 3 + ch).copied().unwrap_or(0)) * wgt;
                }
                if let Some(o) = out.get_mut((y * cw + x) * 3 + ch) {
                    *o = clip8(s);
                }
            }
        }
    }
    out
}

/// Check an input image's dimensions and buffer.
pub fn check(img: &Rgb8) -> Result<()> {
    if img.width == 0 || img.height == 0 {
        return Err(Error::Input(format!("the image is empty ({} × {})", img.width, img.height)));
    }
    if img.width > MAX_EDGE || img.height > MAX_EDGE || img.width.saturating_mul(img.height) > MAX_PIXELS {
        return Err(Error::Input(format!("the image is too large ({} × {}); use a preview", img.width, img.height)));
    }
    if img.data.len() != img.width * img.height {
        return Err(Error::Input(format!("the image buffer holds {} pixels, not {} × {}", img.data.len(), img.width, img.height)));
    }
    Ok(())
}

/// The 8-bit square input (`spec.size²`, interleaved RGB) for `img`.
pub fn resize(img: &Rgb8, spec: &ImageSpec) -> Result<Vec<u8>> {
    check(img)?;
    let side = spec.size as usize;
    if side == 0 {
        return Err(Error::Input("the model input has no size".into()));
    }
    let rgb: Vec<u8> = img.data.iter().flatten().copied().collect();
    match spec.fit {
        Fit::Squash => Ok(resample(&rgb, img.width, img.height, side, side, spec.resize)),
        Fit::CenterCrop => {
            // short edge to `side`, rounding the long edge like the reference (int(side · long / short))
            let (w, h) = (img.width, img.height);
            let (rw, rh) = if w <= h { (side, (side * h / w).max(side)) } else { ((side * w / h).max(side), side) };
            let (x0, y0) = ((rw - side) / 2, (rh - side) / 2);
            // only the crop is resampled (the same pixels as resampling it all and cropping)
            Ok(resample_window(&rgb, w, h, (rw, rh), (x0..x0 + side, y0..y0 + side), spec.resize))
        }
    }
}

/// The normalised planar input `[3, size, size]` for `img`.
pub fn pixels(img: &Rgb8, spec: &ImageSpec) -> Result<Vec<f32>> {
    let rgb = resize(img, spec)?;
    let n = (spec.size as usize).pow(2);
    let mut out = vec![0f32; 3 * n];
    // as the reference: x · (1/255), then (x − mean) / std, in f32
    let rescale = 1.0f32 / 255.0;
    for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
        for ch in 0..3 {
            if let (Some(o), Some(&v)) = (out.get_mut(ch * n + i), px.get(ch)) {
                *o = (f32::from(v) * rescale - spec.mean[ch]) / spec.std[ch];
            }
        }
    }
    Ok(out)
}

/// [`pixels`] of every image, one after the other (`[n, 3, size, size]`), resized in parallel (on the current rayon
/// pool: an indexing pace's threads).
#[cfg(not(target_arch = "wasm32"))]
pub fn batch(images: &[Rgb8], spec: &ImageSpec) -> Result<Vec<f32>> {
    use rayon::prelude::*;
    let each: Vec<Vec<f32>> = images.par_iter().map(|img| pixels(img, spec)).collect::<Result<_>>()?;
    Ok(each.concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::builtin_siglip2_b16_256;

    fn img(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Rgb8 {
        Rgb8::from_fn(w, h, f)
    }

    #[test]
    fn the_right_size_passes_through_unchanged() {
        let spec = builtin_siglip2_b16_256().image;
        let i = img(256, 256, |x, y| [x as u8, y as u8, (x ^ y) as u8]);
        let r = resize(&i, &spec).unwrap();
        assert_eq!(r, i.data.iter().flatten().copied().collect::<Vec<_>>());
        let p = pixels(&i, &spec).unwrap();
        assert_eq!(p.len(), 3 * 256 * 256);
        assert_eq!(p[0], -1.0);
        assert!((p[255] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn flat_images_stay_flat_at_any_scale() {
        let spec = builtin_siglip2_b16_256().image;
        for (w, h) in [(1, 1), (3, 700), (1000, 2), (512, 384), (100, 100)] {
            let r = resize(&img(w, h, |_, _| [10, 128, 250]), &spec).unwrap();
            assert_eq!(r.len(), 256 * 256 * 3);
            assert!(r.as_chunks::<3>().0.iter().all(|p| *p == [10, 128, 250]), "{w}×{h}");
        }
        let mut cubic = spec.clone();
        cubic.resize = Filter::Bicubic;
        cubic.fit = Fit::CenterCrop;
        let r = resize(&img(300, 200, |_, _| [0, 255, 77]), &cubic).unwrap();
        assert!(r.as_chunks::<3>().0.iter().all(|p| *p == [0, 255, 77]));
    }

    #[test]
    fn halving_averages_pairs() {
        // Pillow's bilinear at scale 2: a triangle twice as wide, clipped at the edges
        // (weights 3/7, 3/7, 1/7 for the first output, 1/7, 3/7, 3/7 for the second)
        let spec = ImageSpec { size: 2, ..builtin_siglip2_b16_256().image };
        let i = img(4, 4, |x, _| [[0, 80, 160, 240][x]; 3]);
        let r = resize(&i, &spec).unwrap();
        assert_eq!(r[0], 57); // (0·3 + 80·3 + 160·1) / 7
        assert_eq!(r[3], 183); // (80·1 + 160·3 + 240·3) / 7
    }

    #[test]
    fn a_center_crop_resamples_only_the_crop_and_gives_the_same_pixels() {
        let f = |x: usize, y: usize| [(x * 7 + y * 3) as u8, (x * x + y) as u8, (x ^ (y * 5)) as u8];
        for filter in [Filter::Bilinear, Filter::Bicubic] {
            for (w, h, ow, oh) in [(300, 200, 336, 224), (200, 300, 224, 336), (97, 61, 40, 25), (50, 50, 50, 80), (50, 50, 80, 50)] {
                let rgb: Vec<u8> = img(w, h, f).data.iter().flatten().copied().collect();
                let whole = resample(&rgb, w, h, ow, oh, filter);
                let (xs, ys) = (ow / 4..ow / 4 + ow / 2, oh / 3..oh / 3 + oh / 2);
                let window = resample_window(&rgb, w, h, (ow, oh), (xs.clone(), ys.clone()), filter);
                let mut cut = Vec::new();
                for y in ys.clone() {
                    cut.extend_from_slice(&whole[(y * ow + xs.start) * 3..(y * ow + xs.end) * 3]);
                }
                assert_eq!(window, cut, "{filter:?} {w}×{h} → {ow}×{oh}");
            }
        }
        // a 1 × 65,535 image (valid input) center-cropped for CLIP: the long resampled image is never made
        let spec = crate::manifest::builtin_clip_vit_b32_openai().image;
        assert_eq!(spec.fit, Fit::CenterCrop);
        let tall = img(1, MAX_EDGE, |_, y| [(y % 251) as u8, 9, 200]);
        let started = std::time::Instant::now();
        let r = resize(&tall, &spec).unwrap();
        assert_eq!(r.len(), (spec.size as usize).pow(2) * 3);
        assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
    }

    #[test]
    fn bad_images_are_errors() {
        let spec = builtin_siglip2_b16_256().image;
        assert!(resize(&Rgb8 { width: 0, height: 10, data: vec![] }, &spec).is_err());
        assert!(resize(&Rgb8 { width: 10, height: 10, data: vec![[0; 3]; 5] }, &spec).is_err());
        assert!(resize(&Rgb8 { width: 100_000, height: 1, data: vec![] }, &spec).is_err());
        assert!(resize(&Rgb8 { width: 60_000, height: 60_000, data: vec![] }, &spec).is_err());
    }
}
