//! Assisted culling measurements on a decoded source (any size; measured on a fixed-size copy so
//! scores compare across photos): focus (whole-frame or, given a face region, just around it —
//! see [`face_sharpness`]), clipping, and a tiny signature for finding similar shots (bursts).
//! Classical image statistics — no learned models.

use lightcraft_raster::Rgb32f;

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

/// Focus 0..100 of a luma grid `l` (`w × h`): detail energy (Laplacian) of the sharpest areas —
/// the 90th percentile of 16 × 16-pixel blocks, so a sharp subject on a soft background still
/// reads as sharp.
fn sharpness_of(l: &[f32], w: usize, h: usize) -> f32 {
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

/// Focus 0..100 of the whole photo (see [`sharpness_of`]).
pub fn sharpness(img: &Rgb32f) -> f32 {
    let (l, w, h) = luma(img);
    sharpness_of(&l, w, h)
}

/// A crop of `img` around the normalized rect `[x0, y0, x1, y1]`, padded by `margin` on every
/// side (a face box itself is mostly flat skin: the surrounding hair, eyes and background give
/// the Laplacian measure something to read). Degenerate or out-of-range rects fall back to the
/// whole photo.
fn crop_around(img: &Rgb32f, rect: [f32; 4], margin: f32) -> Rgb32f {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 {
        return img.clone();
    }
    let [x0, y0, x1, y1] = rect;
    if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) || x1 <= x0 || y1 <= y0 {
        return img.clone();
    }
    let (mx, my) = ((x1 - x0) * margin, (y1 - y0) * margin);
    let px = |v: f32, n: usize| ((v * n as f32).round() as isize).clamp(0, n as isize) as usize;
    let (cx0, cx1) = (px(x0 - mx, w), px(x1 + mx, w));
    let (cy0, cy1) = (px(y0 - my, h), px(y1 + my, h));
    let (cw, ch) = (cx1.saturating_sub(cx0).max(1), cy1.saturating_sub(cy0).max(1));
    let mut out = Rgb32f::new(cw, ch);
    for y in 0..ch {
        for x in 0..cw {
            out.data[y * cw + x] = img.data[(cy0 + y).min(h - 1) * w + (cx0 + x).min(w - 1)];
        }
    }
    out
}

/// Focus 0..100 of the photo at and around a normalized face rect `[x0, y0, x1, y1]` (see
/// [`crate::cull`] module docs): lets Assisted Culling reject a photo whose detected face is
/// blurred even when the rest of the frame (background, other subjects) reads sharp, and pick a
/// burst's sharpest face rather than its sharpest frame.
pub fn face_sharpness(img: &Rgb32f, rect: [f32; 4]) -> f32 {
    let crop = crop_around(img, rect, 0.4);
    let (l, w, h) = luma(&crop);
    sharpness_of(&l, w, h)
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

    #[test]
    fn face_sharpness_catches_a_blurred_face_the_whole_frame_score_misses() {
        // a fine checkerboard everywhere, blurred (to flat) on the left half only: a whole-frame
        // score is pulled up by the still-sharp right half even when the "face" (left half) is
        // the blurred part.
        let checker = Rgb32f::from_fn(256, 160, |x, _| if (x / 2) % 2 == 0 { [0.75; 3] } else { [0.25; 3] });
        let soft = blur(&checker, 6);
        let mut half = checker.clone();
        for y in 0..half.height {
            for x in 0..half.width / 2 {
                half.data[y * half.width + x] = soft.data[y * half.width + x];
            }
        }
        let whole = sharpness(&half);
        let blurred_face = face_sharpness(&half, [0.1, 0.3, 0.4, 0.7]);
        let sharp_face = face_sharpness(&half, [0.6, 0.3, 0.9, 0.7]);
        assert!(blurred_face < sharp_face - 10.0, "blurred-half face {blurred_face} vs sharp-half face {sharp_face}");
        assert!(blurred_face < whole - 5.0, "a face-targeted score should catch a blurred face that the whole-frame score ({whole}) smooths over");
    }

    #[test]
    fn face_sharpness_falls_back_to_the_whole_photo_for_a_bad_rect() {
        let img = lightcraft_scenes::demo_library()[0].render(240, 160);
        assert_eq!(face_sharpness(&img, [f32::NAN; 4]), sharpness(&img));
        assert_eq!(face_sharpness(&img, [0.5, 0.5, 0.5, 0.5]), sharpness(&img), "empty rect");
    }
}
