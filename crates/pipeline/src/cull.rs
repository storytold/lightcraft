//! Assisted culling measurements on a decoded source (any size; measured on a fixed-size copy so
//! scores compare across photos): focus, clipping, and a tiny signature for finding similar
//! shots (bursts). Classical image statistics — no learned models.

use lightcraft_raster::{Rgb32f, Rgba8};

/// Long edge the measurements run at.
const EDGE: usize = 512;

/// Display-ish luminance (sRGB-encoded) of `img` resampled to fit `EDGE` (box filter).
fn luma(img: &Rgb32f) -> (Vec<f32>, usize, usize) {
    let (w, h) = (img.width.max(1), img.height.max(1));
    let s = (EDGE as f64 / w.max(h) as f64).min(1.0);
    let (ow, oh) = (((w as f64 * s).round() as usize).max(8), ((h as f64 * s).round() as usize).max(8));
    let mut out = vec![0f32; ow * oh];
    for (y, row) in out.chunks_mut(ow).enumerate() {
        let (y0, y1) = (y * h / oh, ((y + 1) * h / oh).max(y * h / oh + 1).min(h));
        for (x, v) in row.iter_mut().enumerate() {
            let (x0, x1) = (x * w / ow, ((x + 1) * w / ow).max(x * w / ow + 1).min(w));
            let mut acc = 0.0f32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let c = img.data[yy * w + xx];
                    acc += 0.2627 * c[0] + 0.678 * c[1] + 0.0593 * c[2];
                }
            }
            let l = acc / ((y1 - y0) * (x1 - x0)) as f32;
            *v = lightcraft_color::transfer::linear_to_srgb(l.clamp(0.0, 1.0));
        }
    }
    (out, ow, oh)
}

/// Focus 0..100: detail energy (Laplacian) of the sharpest areas — the 90th percentile of
/// 16 × 16-pixel blocks, so a sharp subject on a soft background still reads as sharp.
pub fn sharpness(img: &Rgb32f) -> f32 {
    let (l, w, h) = luma(img);
    let b = 16;
    let mut blocks = Vec::new();
    for by in (1..h.saturating_sub(1)).step_by(b) {
        for bx in (1..w.saturating_sub(1)).step_by(b) {
            let mut e = 0.0f32;
            let mut n = 0;
            for y in by..(by + b).min(h - 1) {
                for x in bx..(bx + b).min(w - 1) {
                    let c = l[y * w + x];
                    let lap = 4.0 * c - l[y * w + x - 1] - l[y * w + x + 1] - l[(y - 1) * w + x] - l[(y + 1) * w + x];
                    e += lap * lap;
                    n += 1;
                }
            }
            if n > 0 {
                blocks.push(e / n as f32);
            }
        }
    }
    if blocks.is_empty() {
        return 0.0;
    }
    let k = (blocks.len() as f32 * 0.9) as usize;
    let k = k.min(blocks.len() - 1);
    let (_, v, _) = blocks.select_nth_unstable_by(k, f32::total_cmp);
    // map energy to 0..100: a crisp, ordinary photo at 512 px reads ≈ 50–80 (calibrated on the
    // CC0 corpus), soft ones well below
    100.0 * (1.0 - (-*v / 0.012).exp())
}

/// Share of clipped pixels (crushed blacks + blown highlights), 0..1.
pub fn clipped(img: &Rgb32f) -> f32 {
    let n = img.data.len().max(1);
    let c = img.data.iter().filter(|p| p.iter().any(|v| *v >= 0.985) || p.iter().all(|v| *v <= 0.002)).count();
    c as f32 / n as f32
}

/// A tiny look-alike signature: 8 × 8 luminance, normalised to zero mean and unit norm.
pub fn signature(img: &Rgb32f) -> [f32; 64] {
    let (l, w, h) = luma(img);
    let mut s = [0f32; 64];
    for (i, v) in s.iter_mut().enumerate() {
        let (cx, cy) = (i % 8, i / 8);
        let (x0, x1) = (cx * w / 8, (cx + 1) * w / 8);
        let (y0, y1) = (cy * h / 8, (cy + 1) * h / 8);
        let mut acc = 0.0;
        for y in y0..y1 {
            for x in x0..x1 {
                acc += l[y * w + x];
            }
        }
        *v = acc / ((x1 - x0) * (y1 - y0)).max(1) as f32;
    }
    let mean = s.iter().sum::<f32>() / 64.0;
    s.iter_mut().for_each(|v| *v -= mean);
    let norm = s.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
    s.iter_mut().for_each(|v| *v /= norm);
    s
}

/// Similarity of two signatures: 1 = the same picture, 0 = unrelated.
pub fn similarity(a: &[f32; 64], b: &[f32; 64]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>().clamp(-1.0, 1.0)
}

/// Luminance of `img` box-resampled to exactly `ow × oh` (sRGB-encoded, 0..1): the input of the
/// perceptual hashes, which compare pictures at different sizes.
fn small_luma(img: &Rgb32f, ow: usize, oh: usize) -> Vec<f32> {
    let (w, h) = (img.width.max(1), img.height.max(1));
    let mut out = vec![0f32; ow * oh];
    for (y, row) in out.chunks_mut(ow).enumerate() {
        let (y0, y1) = (y * h / oh, ((y + 1) * h / oh).max(y * h / oh + 1).min(h));
        for (x, v) in row.iter_mut().enumerate() {
            let (x0, x1) = (x * w / ow, ((x + 1) * w / ow).max(x * w / ow + 1).min(w));
            let mut acc = 0.0f32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let c = img.data.get(yy * w + xx).copied().unwrap_or([0.0; 3]);
                    acc += 0.2627 * c[0] + 0.678 * c[1] + 0.0593 * c[2];
                }
            }
            let l = acc / ((y1 - y0) * (x1 - x0)).max(1) as f32;
            *v = lightcraft_color::transfer::linear_to_srgb(l.clamp(0.0, 1.0));
        }
    }
    out
}

/// Luminance of a display-encoded (sRGB, 8-bit) picture box-resampled to `ow × oh`, 0..1: the same
/// input [`small_luma`] makes from a linear picture, for a rendered thumbnail.
fn small_luma_srgb8(img: &Rgba8, ow: usize, oh: usize) -> Vec<f32> {
    let (w, h) = (img.width.max(1), img.height.max(1));
    let mut out = vec![0f32; ow * oh];
    for (y, row) in out.chunks_mut(ow).enumerate() {
        let (y0, y1) = (y * h / oh, ((y + 1) * h / oh).max(y * h / oh + 1).min(h));
        for (x, v) in row.iter_mut().enumerate() {
            let (x0, x1) = (x * w / ow, ((x + 1) * w / ow).max(x * w / ow + 1).min(w));
            // decoded to linear light, averaged and encoded again: the same numbers `small_luma` makes
            // from the linear picture (up to the 8-bit rounding)
            let mut acc = 0.0f32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let c = img.data.get(yy * w + xx).copied().unwrap_or([0; 4]);
                    let d = lightcraft_color::transfer::decode_srgb8;
                    acc += 0.2627 * d(c[0]) + 0.678 * d(c[1]) + 0.0593 * d(c[2]);
                }
            }
            let l = acc / ((y1 - y0) * (x1 - x0)).max(1) as f32;
            *v = lightcraft_color::transfer::linear_to_srgb(l.clamp(0.0, 1.0));
        }
    }
    out
}

/// Side of the luminance square the DCT hash starts from.
const DCT_SIDE: usize = 32;
/// Low frequencies the DCT hash keeps along each axis (`DCT_LOW × DCT_LOW` coefficients).
const DCT_LOW: usize = 8;

/// A 64-bit perceptual hash of the picture (Find Duplicates): the luminance at 32 × 32, its 2-D
/// DCT, and the 8 × 8 lowest frequencies minus the DC term, each bit set where the coefficient is
/// above their median. The same picture at another size, compression or with mild tone edits
/// differs in a few bits; another picture of the same scene in many; unrelated pictures in about
/// half. Compare with [`hamming`].
pub fn dct_hash(img: &Rgb32f) -> u64 {
    dct_bits(&small_luma(img, DCT_SIDE, DCT_SIDE))
}

/// [`dct_hash`] of a rendered, display-encoded picture (a grid thumbnail).
pub fn dct_hash_srgb8(img: &Rgba8) -> u64 {
    dct_bits(&small_luma_srgb8(img, DCT_SIDE, DCT_SIDE))
}

/// The DCT hash of a `DCT_SIDE × DCT_SIDE` luminance square.
fn dct_bits(l: &[f32]) -> u64 {
    // cos(π (2 i + 1) k / (2 N)) for the frequencies kept
    let mut cos = [[0f32; DCT_SIDE]; DCT_LOW];
    for (k, row) in cos.iter_mut().enumerate() {
        for (i, c) in row.iter_mut().enumerate() {
            *c = (std::f32::consts::PI * (2 * i + 1) as f32 * k as f32 / (2 * DCT_SIDE) as f32).cos();
        }
    }
    // rows first (each row → DCT_LOW horizontal frequencies), then columns
    let mut rows = [[0f32; DCT_LOW]; DCT_SIDE];
    for (y, out) in rows.iter_mut().enumerate() {
        for (u, o) in out.iter_mut().enumerate() {
            *o = (0..DCT_SIDE).map(|x| l.get(y * DCT_SIDE + x).copied().unwrap_or(0.0) * cos[u][x]).sum();
        }
    }
    let mut low = [0f32; DCT_LOW * DCT_LOW];
    for v in 0..DCT_LOW {
        for u in 0..DCT_LOW {
            low[v * DCT_LOW + u] = (0..DCT_SIDE).map(|y| rows[y][u] * cos[v][y]).sum();
        }
    }
    // the DC term (index 0) says only how bright the picture is: left out of the hash and the median
    let mut sorted = low[1..].to_vec();
    sorted.sort_by(f32::total_cmp);
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    low.iter().skip(1).fold(0u64, |bits, v| (bits << 1) | u64::from(*v > median))
}

/// A 64-bit difference hash (Find Duplicates): the luminance at 9 × 8, each bit set where a pixel
/// is brighter than its right-hand neighbour. Cheap and robust to tone changes; used beside
/// [`dct_hash`] so a match needs two unrelated hashes to agree.
pub fn diff_hash(img: &Rgb32f) -> u64 {
    diff_bits(&small_luma(img, 9, 8))
}

/// [`diff_hash`] of a rendered, display-encoded picture (a grid thumbnail).
pub fn diff_hash_srgb8(img: &Rgba8) -> u64 {
    diff_bits(&small_luma_srgb8(img, 9, 8))
}

/// The difference hash of a 9 × 8 luminance block.
fn diff_bits(l: &[f32]) -> u64 {
    let mut bits = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            let (a, b) = (l.get(y * 9 + x).copied().unwrap_or(0.0), l.get(y * 9 + x + 1).copied().unwrap_or(0.0));
            bits = (bits << 1) | u64::from(a > b);
        }
    }
    bits
}

/// Bits in which two hashes differ (0 = the same picture; ~32 = unrelated).
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blur(img: &Rgb32f, r: usize) -> Rgb32f {
        let (w, h) = (img.width, img.height);
        let mut out = img.clone();
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0f32; 3];
                let mut n = 0.0;
                for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                    for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                        let c = img.data[yy * w + xx];
                        acc = [acc[0] + c[0], acc[1] + c[1], acc[2] + c[2]];
                        n += 1.0;
                    }
                }
                out.data[y * w + x] = acc.map(|v| v / n);
            }
        }
        out
    }

    #[test]
    fn blur_lowers_focus_and_signatures_match() {
        let img = lightcraft_scenes::demo_library()[0].render(480, 320);
        let soft = blur(&img, 4);
        let (a, b) = (sharpness(&img), sharpness(&soft));
        assert!(a > b + 10.0, "sharp {a} vs blurred {b}");
        assert!(similarity(&signature(&img), &signature(&soft)) > 0.95, "the same picture");
        let other = lightcraft_scenes::demo_library()[5].render(480, 320);
        assert!(similarity(&signature(&img), &signature(&other)) < 0.9);
        assert!(clipped(&img) < 0.5);
    }

    /// The same picture at half the size and softened hashes within a few bits; another scene is
    /// far away; a hash is a stable function of the picture (same input, same bits).
    #[test]
    fn perceptual_hashes_match_the_same_picture_at_another_size_and_separate_scenes() {
        let scenes = lightcraft_scenes::demo_library();
        let img = scenes[0].render(480, 320);
        let half = scenes[0].render(240, 160);
        let soft = blur(&img, 2);
        assert_eq!(dct_hash(&img), dct_hash(&img));
        assert!(hamming(dct_hash(&img), dct_hash(&half)) <= 4, "{}", hamming(dct_hash(&img), dct_hash(&half)));
        assert!(hamming(diff_hash(&img), diff_hash(&half)) <= 6, "{}", hamming(diff_hash(&img), diff_hash(&half)));
        assert!(hamming(dct_hash(&img), dct_hash(&soft)) <= 4);
        let mut nearest = u32::MAX;
        for s in scenes.iter().skip(1) {
            let other = s.render(480, 320);
            nearest = nearest.min(hamming(dct_hash(&img), dct_hash(&other)));
        }
        assert!(nearest > 12, "another scene must be well apart from a duplicate: nearest {nearest} bits");
    }

    /// A display-encoded 8-bit copy of a picture (what a grid thumbnail is) hashes like the linear
    /// picture: the two paths may be mixed in one run.
    #[test]
    fn the_srgb8_hashes_agree_with_the_linear_ones() {
        let img = lightcraft_scenes::demo_library()[0].render(480, 320);
        let encoded = Rgba8 {
            width: img.width,
            height: img.height,
            data: img
                .data
                .iter()
                .map(|c| {
                    let e = |v: f32| (lightcraft_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8;
                    [e(c[0]), e(c[1]), e(c[2]), 255]
                })
                .collect(),
        };
        // (a smooth synthetic scene has many coefficients and neighbour pairs all but equal, where 8-bit rounding
        // decides the bit: within the distance a duplicate is allowed, and a run hashes every photo the same way)
        assert!(hamming(dct_hash(&img), dct_hash_srgb8(&encoded)) <= 8, "{}", hamming(dct_hash(&img), dct_hash_srgb8(&encoded)));
        assert!(hamming(diff_hash(&img), diff_hash_srgb8(&encoded)) <= 8, "{}", hamming(diff_hash(&img), diff_hash_srgb8(&encoded)));
        let none = Rgba8 { width: 0, height: 0, data: Vec::new() };
        let _ = (dct_hash_srgb8(&none), diff_hash_srgb8(&none));
    }

    /// A degenerate picture (one pixel, or empty) hashes without panicking.
    #[test]
    fn perceptual_hashes_take_degenerate_pictures() {
        let one = Rgb32f { width: 1, height: 1, data: vec![[0.5, 0.5, 0.5]] };
        let _ = (dct_hash(&one), diff_hash(&one));
        let empty = Rgb32f { width: 0, height: 0, data: Vec::new() };
        let _ = (dct_hash(&empty), diff_hash(&empty));
        let lying = Rgb32f { width: 50, height: 50, data: vec![[0.2, 0.3, 0.4]; 10] };
        let _ = (dct_hash(&lying), diff_hash(&lying));
    }
}
