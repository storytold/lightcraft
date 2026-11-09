//! Assisted culling measurements on a decoded source (any size; measured on a fixed-size copy so
//! scores compare across photos): focus, clipping, and a tiny signature for finding similar
//! shots (bursts). Classical image statistics — no learned models.

use lightcraft_geom::Rect;
use lightcraft_raster::Rgb32f;

/// Long edge the measurements run at.
const EDGE: usize = 512;

/// Display-ish luminance (sRGB-encoded) of the pixels `x0..x1` × `y0..y1` of `img` resampled to fit
/// `EDGE` (box filter, never enlarged). The caller keeps the box inside the image.
fn luma_region(img: &Rgb32f, (x0, y0, x1, y1): (usize, usize, usize, usize)) -> (Vec<f32>, usize, usize) {
    let (w, h) = (x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1));
    let stride = img.width.max(1);
    let s = (EDGE as f64 / w.max(h) as f64).min(1.0);
    let (ow, oh) = (((w as f64 * s).round() as usize).max(8), ((h as f64 * s).round() as usize).max(8));
    let mut out = vec![0f32; ow * oh];
    for (y, row) in out.chunks_mut(ow).enumerate() {
        let (ya, yb) = (y * h / oh, ((y + 1) * h / oh).max(y * h / oh + 1).min(h));
        for (x, v) in row.iter_mut().enumerate() {
            let (xa, xb) = (x * w / ow, ((x + 1) * w / ow).max(x * w / ow + 1).min(w));
            let mut acc = 0.0f32;
            for yy in ya..yb {
                for xx in xa..xb {
                    let c = img.data.get((y0 + yy) * stride + x0 + xx).copied().unwrap_or_default();
                    acc += 0.2627 * c[0] + 0.678 * c[1] + 0.0593 * c[2];
                }
            }
            let l = acc / ((yb - ya) * (xb - xa)) as f32;
            *v = lightcraft_color::transfer::linear_to_srgb(l.clamp(0.0, 1.0));
        }
    }
    (out, ow, oh)
}

/// [`luma_region`] of the whole image.
fn luma(img: &Rgb32f) -> (Vec<f32>, usize, usize) {
    luma_region(img, (0, 0, img.width, img.height))
}

/// Focus 0..100 of a luminance plane: detail energy (Laplacian) of the sharpest areas — the 90th
/// percentile of 16 × 16-pixel blocks, so a sharp subject on a soft background still reads as sharp.
fn focus_of(l: &[f32], w: usize, h: usize) -> f32 {
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

/// Focus 0..100 of the whole frame (see [`focus_of`]).
pub fn sharpness(img: &Rgb32f) -> f32 {
    let (l, w, h) = luma(img);
    focus_of(&l, w, h)
}

/// The shortest side, in pixels, a subject box needs to be scored by [`sharpness_in`].
pub const MIN_SUBJECT_PX: usize = 24;

/// The pixels of `rect` (normalized 0..1 on `img`, y down), clamped to the image; `None` for a
/// box that isn't finite or is thinner than [`MIN_SUBJECT_PX`] on either side.
fn pixel_box(img: &Rgb32f, rect: &Rect) -> Option<(usize, usize, usize, usize)> {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 || img.data.len() < w.checked_mul(h)? || ![rect.x0, rect.y0, rect.x1, rect.y1].iter().all(|v| v.is_finite()) {
        return None;
    }
    let axis = |a: f64, b: f64, n: usize| {
        let (lo, hi) = (a.min(b).clamp(0.0, 1.0), a.max(b).clamp(0.0, 1.0));
        let (lo, hi) = ((lo * n as f64).floor() as usize, ((hi * n as f64).ceil() as usize).min(n));
        (lo.min(n), hi)
    };
    let ((x0, x1), (y0, y1)) = (axis(rect.x0, rect.x1, w), axis(rect.y0, rect.y1, h));
    (x1.saturating_sub(x0) >= MIN_SUBJECT_PX && y1.saturating_sub(y0) >= MIN_SUBJECT_PX).then_some((x0, y0, x1, y1))
}

/// How many pixels `rect` (normalized 0..1 on `img`) spans on its shorter side once clamped to the
/// image; 0 for a box that isn't usable. Lets a caller decide whether a bigger source is needed.
pub fn subject_px(img: &Rgb32f, rect: &Rect) -> usize {
    let (w, h) = (img.width as f64, img.height as f64);
    if ![rect.x0, rect.y0, rect.x1, rect.y1].iter().all(|v| v.is_finite()) {
        return 0;
    }
    let side = |a: f64, b: f64, n: f64| (a.max(b).clamp(0.0, 1.0) - a.min(b).clamp(0.0, 1.0)) * n;
    side(rect.x0, rect.x1, w).min(side(rect.y0, rect.y1, h)).floor() as usize
}

/// Focus 0..100 inside `rect` (normalized 0..1 on `img`, y down; clamped to the image), on the
/// same scale and with the same measure as [`sharpness`] — the crop is measured at the source's own
/// pixel scale (shrunk to the long edge the frame measure uses when it is larger, never enlarged).
/// `None` when the box isn't finite or is smaller than [`MIN_SUBJECT_PX`] pixels on a side:
/// too little to judge.
pub fn sharpness_in(img: &Rgb32f, rect: &Rect) -> Option<f32> {
    let b = pixel_box(img, rect)?;
    let (l, w, h) = luma_region(img, b);
    Some(focus_of(&l, w, h))
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

    /// A textured frame (deterministic noise on a gradient) so every part of it has detail to lose.
    fn textured(w: usize, h: usize) -> Rgb32f {
        let mut img = Rgb32f { width: w, height: h, data: vec![[0.0; 3]; w * h] };
        for (i, px) in img.data.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            let n = (i as u32).wrapping_mul(2_654_435_761) >> 24;
            let v = 0.25 + 0.3 * x as f32 / w as f32 + 0.25 * (n as f32 / 255.0);
            *px = [v, v * 0.9, v * 0.8 + 0.05 * (y as f32 / h as f32)];
        }
        img
    }

    /// `base` with the pixels inside `rect` taken from `inside`.
    fn patched(base: &Rgb32f, inside: &Rgb32f, rect: &Rect) -> Rgb32f {
        let mut out = base.clone();
        let (w, h) = (base.width, base.height);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = ((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64);
                if fx >= rect.x0 && fx < rect.x1 && fy >= rect.y0 && fy < rect.y1 {
                    out.data[y * w + x] = inside.data[y * w + x];
                }
            }
        }
        out
    }

    #[test]
    fn focus_is_measured_inside_the_rect() {
        let sharp = textured(480, 320);
        let soft = blur(&sharp, 4);
        let patch = Rect { x0: 0.2, y0: 0.2, x1: 0.5, y1: 0.7 };
        let elsewhere = Rect { x0: 0.6, y0: 0.2, x1: 0.9, y1: 0.7 };
        // a sharp patch on a blurred frame: sharp inside, soft outside
        let img = patched(&soft, &sharp, &patch);
        let (inside, outside) = (sharpness_in(&img, &patch).unwrap(), sharpness_in(&img, &elsewhere).unwrap());
        assert!(inside > outside + 25.0, "inside {inside} vs outside {outside}");
        // and the other way round: a blurred patch on a sharp frame
        let img = patched(&sharp, &soft, &patch);
        let (inside, outside) = (sharpness_in(&img, &patch).unwrap(), sharpness_in(&img, &elsewhere).unwrap());
        assert!(outside > inside + 25.0, "inside {inside} vs outside {outside}");
        // the whole frame is the same measure as `sharpness`
        let whole = Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 };
        assert_eq!(sharpness_in(&img, &whole), Some(sharpness(&img)));
        // a box larger than the frame, or reversed, is clamped / ordered
        let big = Rect { x0: -2.0, y0: -2.0, x1: 3.0, y1: 3.0 };
        assert_eq!(sharpness_in(&img, &big), Some(sharpness(&img)));
        let swapped = Rect { x0: 0.5, y0: 0.7, x1: 0.2, y1: 0.2 };
        assert_eq!(sharpness_in(&img, &swapped), sharpness_in(&img, &patch));
    }

    #[test]
    fn degenerate_subject_boxes_are_refused_not_panicked_on() {
        let img = textured(480, 320);
        let r = |x0, y0, x1, y1| Rect { x0, y0, x1, y1 };
        for rect in [
            r(0.5, 0.5, 0.5, 0.5),
            r(0.5, 0.5, 0.51, 0.9),
            r(0.1, 0.1, 0.9, 0.11),
            r(-3.0, -3.0, -2.0, -2.0),
            r(2.0, 2.0, 3.0, 3.0),
            r(f64::NAN, 0.0, 1.0, 1.0),
            r(0.0, 0.0, f64::INFINITY, 1.0),
            r(0.0, f64::NEG_INFINITY, 1.0, 1.0),
        ] {
            assert_eq!(sharpness_in(&img, &rect), None, "{rect:?}");
        }
        // empty and tiny images, and one whose buffer is short
        let whole = r(0.0, 0.0, 1.0, 1.0);
        assert_eq!(sharpness_in(&Rgb32f { width: 0, height: 0, data: vec![] }, &whole), None);
        assert_eq!(sharpness_in(&Rgb32f { width: 1, height: 1, data: vec![[0.5; 3]] }, &whole), None);
        assert_eq!(sharpness_in(&Rgb32f { width: 480, height: 320, data: vec![[0.5; 3]; 10] }, &whole), None);
        // the frame measures don't panic on those either
        let _ = sharpness(&Rgb32f { width: 480, height: 320, data: vec![[0.5; 3]; 10] });
        let _ = sharpness(&Rgb32f { width: 0, height: 0, data: vec![] });
        assert_eq!(subject_px(&img, &r(0.0, 0.0, 0.5, 0.5)), 160);
        assert_eq!(subject_px(&img, &r(f64::NAN, 0.0, 1.0, 1.0)), 0);
    }
}
