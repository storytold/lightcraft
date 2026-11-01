//! Image and grid resampling for the scene and depth models (CPU, no model needed).
//!
//! - [`resize_rgb`]: Pillow-compatible 8-bit resampling (bilinear or bicubic, antialiased when
//!   downscaling, fixed-point like Pillow), which the reference preprocessing of EoMT
//!   (`PIL.Image.resize(…, BILINEAR)`) and Depth Anything (DPT image processor, bicubic with
//!   antialiasing) use.
//! - [`normalize_padded`]: ImageNet normalization into a zero-padded channel-first buffer.
//! - [`to_square_logits`]: a probability grid over the image resampled to the square mask grid
//!   that the rest of the AI-mask code uses (the image stretched to a square), as logits.
//!
//! The resampling filters follow the published definitions used by Pillow (HPND licence) and
//! PyTorch (BSD); this is an independent implementation, no code copied.

use crate::{Error, Result};

/// ImageNet mean and standard deviation (RGB), as the scene and depth models were trained with.
pub const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
pub const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Largest image side accepted (a cap on allocations sized by the input).
pub const MAX_SIDE: usize = 1 << 16;

/// A resampling filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// Triangle (Pillow `BILINEAR`), support 1.
    Bilinear,
    /// Keys cubic with a = −0.5 (Pillow `BICUBIC`, torchvision antialiased bicubic), support 2.
    Bicubic,
}

impl Filter {
    fn support(self) -> f64 {
        match self {
            Filter::Bilinear => 1.0,
            Filter::Bicubic => 2.0,
        }
    }

    fn eval(self, x: f64) -> f64 {
        let x = x.abs();
        match self {
            Filter::Bilinear => (1.0 - x).max(0.0),
            Filter::Bicubic => {
                let a = -0.5;
                if x < 1.0 {
                    ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0
                } else if x < 2.0 {
                    (((x - 5.0) * x + 8.0) * x - 4.0) * a
                } else {
                    0.0
                }
            }
        }
    }
}

/// Fixed-point precision of the 8-bit resampling coefficients (Pillow's).
const PRECISION_BITS: u32 = 32 - 8 - 2;

/// For each output sample: the first input index and the fixed-point weights.
fn coefficients(src: usize, dst: usize, filter: Filter) -> Vec<(usize, Vec<i32>)> {
    let scale = src as f64 / dst.max(1) as f64;
    let filterscale = scale.max(1.0);
    let support = filter.support() * filterscale;
    (0..dst)
        .map(|o| {
            let center = (o as f64 + 0.5) * scale;
            let lo = (center - support + 0.5).floor().max(0.0) as usize;
            let hi = ((center + support + 0.5).floor().max(0.0) as usize).min(src);
            let w: Vec<f64> = (lo..hi).map(|i| filter.eval((i as f64 - center + 0.5) / filterscale)).collect();
            let sum: f64 = w.iter().sum();
            let k = w
                .iter()
                .map(|v| {
                    let v = if sum != 0.0 { v / sum } else { 0.0 } * f64::from(1u32 << PRECISION_BITS);
                    (if v < 0.0 { v - 0.5 } else { v + 0.5 }) as i32
                })
                .collect();
            (lo.min(src), k)
        })
        .collect()
}

fn clip8(acc: i64) -> u8 {
    (acc >> PRECISION_BITS).clamp(0, 255) as u8
}

/// Checks an image size (and, with a real `rgb_len`, its buffer); returns the byte count.
fn check_size(w: usize, h: usize, rgb_len: usize) -> Result<usize> {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(Error::Message(format!("image of {w} × {h} pixels is empty or too large")));
    }
    let n = w.checked_mul(h).and_then(|n| n.checked_mul(3)).ok_or_else(|| Error::Message("image too large".into()))?;
    if rgb_len < n {
        return Err(Error::Message(format!("image buffer of {rgb_len} bytes for {w} × {h} RGB pixels")));
    }
    Ok(n)
}

/// An 8-bit RGB image (`w × h`, row-major) resampled to `ow × oh` like Pillow's
/// `Image.resize(…, filter)`: horizontal pass then vertical, each rounded to 8 bits.
pub fn resize_rgb(rgb: &[u8], w: usize, h: usize, ow: usize, oh: usize, filter: Filter) -> Result<Vec<u8>> {
    check_size(w, h, rgb.len())?;
    check_size(ow, oh, usize::MAX)?;
    let cx = coefficients(w, ow, filter);
    let cy = coefficients(h, oh, filter);
    // horizontal: h rows × ow columns
    let mut tmp = vec![0u8; h * ow * 3];
    for y in 0..h {
        let row = rgb.get(y * w * 3..(y + 1) * w * 3).unwrap_or(&[]);
        let out = tmp.get_mut(y * ow * 3..(y + 1) * ow * 3).unwrap_or(&mut []);
        for ((lo, k), o) in cx.iter().zip(out.as_chunks_mut::<3>().0.iter_mut()) {
            let mut acc = [1i64 << (PRECISION_BITS - 1); 3];
            for (i, kk) in k.iter().enumerate() {
                if let Some(p) = row.get((lo + i) * 3..(lo + i) * 3 + 3) {
                    for c in 0..3 {
                        acc[c] += i64::from(p[c]) * i64::from(*kk);
                    }
                }
            }
            for c in 0..3 {
                o[c] = clip8(acc[c]);
            }
        }
    }
    // vertical: oh rows × ow columns
    let mut out = vec![0u8; oh * ow * 3];
    for (oy, (lo, k)) in cy.iter().enumerate() {
        let orow = out.get_mut(oy * ow * 3..(oy + 1) * ow * 3).unwrap_or(&mut []);
        for (x, o) in orow.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let mut acc = [1i64 << (PRECISION_BITS - 1); 3];
            for (i, kk) in k.iter().enumerate() {
                if let Some(p) = tmp.get(((lo + i) * ow + x) * 3..((lo + i) * ow + x) * 3 + 3) {
                    for c in 0..3 {
                        acc[c] += i64::from(p[c]) * i64::from(*kk);
                    }
                }
            }
            for c in 0..3 {
                o[c] = clip8(acc[c]);
            }
        }
    }
    Ok(out)
}

/// An 8-bit RGB image (`w × h`) normalized with the ImageNet mean/std into a channel-first
/// `[3, ph, pw]` buffer, top-left aligned and zero-padded (`pw ≥ w`, `ph ≥ h`).
pub fn normalize_padded(rgb: &[u8], w: usize, h: usize, pw: usize, ph: usize) -> Result<Vec<f32>> {
    check_size(w, h, rgb.len())?;
    check_size(pw, ph, usize::MAX)?;
    if pw < w || ph < h {
        return Err(Error::Message(format!("cannot pad {w} × {h} to {pw} × {ph}")));
    }
    let mut out = vec![0f32; 3 * pw * ph];
    for y in 0..h {
        for x in 0..w {
            let Some(p) = rgb.get((y * w + x) * 3..(y * w + x) * 3 + 3) else { continue };
            for c in 0..3 {
                if let Some(o) = out.get_mut((c * ph + y) * pw + x) {
                    *o = (f32::from(p[c]) / 255.0 - IMAGENET_MEAN[c]) / IMAGENET_STD[c];
                }
            }
        }
    }
    Ok(out)
}

/// Python's `round()` (half to even), which the reference preprocessing uses for sizes.
pub fn round_even(x: f64) -> usize {
    x.round_ties_even().max(0.0) as usize
}

/// A `w × h` probability grid covering the whole image, resampled bilinearly to `side × side`
/// (the image stretched to a square; output cell centres at `(i + 0.5) / side`) and converted to
/// logits `ln(p / (1 − p))`, with `p` clamped to `[1e-4, 1 − 1e-4]`.
pub fn to_square_logits(probs: &[f32], w: usize, h: usize, side: usize) -> Vec<f32> {
    let n = side.saturating_mul(side);
    let p_lo: f32 = 1e-4;
    let floor = (p_lo / (1.0 - p_lo)).ln();
    if w == 0 || h == 0 || w.checked_mul(h).is_none_or(|wh| probs.len() < wh) || side > MAX_SIDE {
        return vec![floor; n];
    }
    let at = |x: usize, y: usize| probs.get(y * w + x).copied().unwrap_or(0.0);
    let axis = |i: usize, src: usize| {
        let f = ((i as f32 + 0.5) * src as f32 / side as f32 - 0.5).clamp(0.0, (src - 1) as f32);
        let i0 = (f.floor() as usize).min(src - 1);
        (i0, (i0 + 1).min(src - 1), f - i0 as f32)
    };
    let xs: Vec<_> = (0..side).map(|i| axis(i, w)).collect();
    let mut out = Vec::with_capacity(n);
    for j in 0..side {
        let (y0, y1, ty) = axis(j, h);
        for &(x0, x1, tx) in &xs {
            let top = at(x0, y0) * (1.0 - tx) + at(x1, y0) * tx;
            let bot = at(x0, y1) * (1.0 - tx) + at(x1, y1) * tx;
            let p = (top * (1.0 - ty) + bot * ty).clamp(p_lo, 1.0 - p_lo);
            out.push((p / (1.0 - p)).ln());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_keeps_flat_images_flat_and_is_bounded() {
        let flat = vec![200u8; 37 * 23 * 3];
        for f in [Filter::Bilinear, Filter::Bicubic] {
            for (ow, oh) in [(10, 7), (80, 50), (37, 23)] {
                let r = resize_rgb(&flat, 37, 23, ow, oh, f).unwrap();
                assert_eq!(r.len(), ow * oh * 3);
                assert!(r.iter().all(|v| *v == 200), "{f:?} {ow}x{oh}");
            }
        }
        // identity size is exact
        let img: Vec<u8> = (0..9 * 5 * 3).map(|i| (i * 7 % 256) as u8).collect();
        assert_eq!(resize_rgb(&img, 9, 5, 9, 5, Filter::Bicubic).unwrap(), img);
    }

    #[test]
    fn resize_rejects_bad_sizes() {
        assert!(resize_rgb(&[0; 5], 2, 2, 4, 4, Filter::Bilinear).is_err());
        assert!(resize_rgb(&[0; 12], 2, 2, 0, 4, Filter::Bilinear).is_err());
        assert!(resize_rgb(&[], 0, 0, 4, 4, Filter::Bilinear).is_err());
        assert!(normalize_padded(&[0; 12], 2, 2, 1, 4).is_err());
    }

    #[test]
    fn normalize_pads_with_zeros() {
        let p = normalize_padded(&[255; 12], 2, 2, 3, 3).unwrap();
        assert_eq!(p.len(), 27);
        assert!((p[0] - (1.0 - 0.485) / 0.229).abs() < 1e-6);
        assert_eq!(p[2], 0.0); // padded column
        assert_eq!(p[6], 0.0); // padded row
    }

    #[test]
    fn python_rounding() {
        assert_eq!(round_even(426.5), 426);
        assert_eq!(round_even(427.5), 428);
        assert_eq!(round_even(426.8), 427);
    }

    #[test]
    fn square_logits_follow_the_grid() {
        // left half certain, right half impossible, 4 × 2 grid → 8 × 8
        let probs = [1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0];
        let l = to_square_logits(&probs, 4, 2, 8);
        assert_eq!(l.len(), 64);
        assert!(l[0] > 9.0 && l[7] < -9.0);
        // the middle is undecided
        let mid = to_square_logits(&[0.0, 1.0], 2, 1, 2);
        assert!(mid[0] < 0.0 && mid[1] > 0.0);
        let even = to_square_logits(&[0.5], 1, 1, 3);
        assert!(even.iter().all(|v| v.abs() < 1e-6));
        // bad input: all "no"
        assert!(to_square_logits(&[0.5], 4, 4, 2).iter().all(|v| *v < -9.0));
    }
}
