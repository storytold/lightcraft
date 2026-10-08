//! Image preprocessing and mask post-processing (CPU, no model needed).
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SAM 3 code of Hugging Face Transformers (`models/sam3/image_processing_sam3*.py`), Copyright The HuggingFace
//! Team and Meta Platforms, Inc.; translated to Rust on candle and restructured. See NOTICE.

use crate::{Error, Result};

/// Separable resampling weights for `src → dst` samples with an antialiased triangle filter
/// (Pillow's BILINEAR: the support widens with the downscale factor).
fn weights(src: usize, dst: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = src as f64 / dst as f64;
    let ss = scale.max(1.0);
    let support = ss;
    (0..dst)
        .map(|o| {
            let center = (o as f64 + 0.5) * scale;
            let lo = ((center - support + 0.5).floor().max(0.0)) as usize;
            let hi = ((center + support + 0.5).floor() as usize).min(src);
            let mut w: Vec<f32> = (lo..hi).map(|i| (1.0 - ((i as f64 - center + 0.5) / ss).abs()).max(0.0) as f32).collect();
            let sum: f32 = w.iter().sum();
            if sum > 0.0 {
                w.iter_mut().for_each(|v| *v /= sum);
            }
            (lo, w)
        })
        .collect()
}

/// An 8-bit RGB image (`w × h`, row-major) stretched to `side × side`, scaled to −1..1 and laid
/// out channel-first (`[3, side, side]`), as SAM 3's processor does.
pub fn preprocess(rgb: &[u8], w: usize, h: usize, side: usize) -> Result<Vec<f32>> {
    let n = w.checked_mul(h).and_then(|n| n.checked_mul(3)).ok_or_else(|| Error::Model("image too large".into()))?;
    if w == 0 || h == 0 || rgb.len() < n {
        return Err(Error::Model(format!("image buffer of {} bytes for {w} × {h}", rgb.len())));
    }
    let (wx, wy) = (weights(w, side), weights(h, side));
    // horizontal pass: h rows × side columns × 3
    let mut tmp = vec![0f32; h * side * 3];
    for y in 0..h {
        for (ox, (lo, ws)) in wx.iter().enumerate() {
            let mut acc = [0f32; 3];
            for (k, wt) in ws.iter().enumerate() {
                let i = (y * w + lo + k) * 3;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += wt * f32::from(rgb.get(i + c).copied().unwrap_or(0));
                }
            }
            if let Some(dst) = tmp.get_mut((y * side + ox) * 3..(y * side + ox) * 3 + 3) {
                dst.copy_from_slice(&acc);
            }
        }
    }
    let mut out = vec![0f32; 3 * side * side];
    for (oy, (lo, ws)) in wy.iter().enumerate() {
        for ox in 0..side {
            let mut acc = [0f32; 3];
            for (k, wt) in ws.iter().enumerate() {
                let i = ((lo + k) * side + ox) * 3;
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += wt * tmp.get(i + c).copied().unwrap_or(0.0);
                }
            }
            for (c, a) in acc.iter().enumerate() {
                // Pillow rounds to 8 bits; then /255 and (x − 0.5) / 0.5
                let v = a.round().clamp(0.0, 255.0) / 255.0;
                if let Some(d) = out.get_mut((c * side + oy) * side + ox) {
                    *d = (v - 0.5) / 0.5;
                }
            }
        }
    }
    Ok(out)
}

/// Mask `logits` (`side × side`) resampled bilinearly (half-pixel centres) to `w × h` and
/// mapped to 0..1 coverage (sigmoid).
pub fn alpha(logits: &[f32], side: usize, w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; w * h];
    if side == 0 || logits.len() < side * side {
        return out;
    }
    let sample = |x: usize, y: usize| logits.get(y.min(side - 1) * side + x.min(side - 1)).copied().unwrap_or(-20.0);
    let (sx, sy) = (side as f32 / w.max(1) as f32, side as f32 / h.max(1) as f32);
    for y in 0..h {
        let fy = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        for x in 0..w {
            let fx = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let top = sample(x0, y0) * (1.0 - tx) + sample(x0 + 1, y0) * tx;
            let bot = sample(x0, y0 + 1) * (1.0 - tx) + sample(x0 + 1, y0 + 1) * tx;
            let l = top * (1.0 - ty) + bot * ty;
            if let Some(o) = out.get_mut(y * w + x) {
                *o = 1.0 / (1.0 + (-l).exp());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_is_bounded_and_channel_first() {
        let rgb: Vec<u8> = (0..20 * 10 * 3).map(|i| (i % 256) as u8).collect();
        let p = preprocess(&rgb, 20, 10, 8).unwrap();
        assert_eq!(p.len(), 3 * 64);
        assert!(p.iter().all(|v| (-1.0..=1.0).contains(v)));
        // a flat image stays flat
        let flat = vec![255u8; 30 * 30 * 3];
        assert!(preprocess(&flat, 30, 30, 7).unwrap().iter().all(|v| (*v - 1.0).abs() < 1e-6));
    }

    #[test]
    fn short_buffers_are_errors() {
        assert!(preprocess(&[0; 5], 2, 2, 4).is_err());
        assert!(preprocess(&[], 0, 0, 4).is_err());
    }

    #[test]
    fn alpha_follows_the_logits() {
        let logits: Vec<f32> = (0..16).map(|i| if i % 4 < 2 { 10.0 } else { -10.0 }).collect();
        let a = alpha(&logits, 4, 8, 2);
        assert!(a[0] > 0.99 && a[7] < 0.01);
        assert!(alpha(&[], 4, 3, 3).iter().all(|v| *v == 0.0));
    }
}
