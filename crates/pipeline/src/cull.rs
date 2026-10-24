//! Assisted culling measurements on a decoded source (any size; measured on a fixed-size copy so
//! scores compare across photos): focus, clipping, a tiny signature for finding similar shots
//! (bursts), and classical aesthetic / eyes-open heuristics. No learned models and no weights.

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

/// Classical aesthetic, 1..10 (NIMA-like scale): sharpness, exposure, contrast, colour, a weak
/// rule-of-thirds term. Honest about being a heuristic, not a trained head.
pub fn aesthetic(img: &Rgb32f) -> f32 {
    let (l, w, h) = luma(img);
    let n = l.len().max(1) as f32;
    let mean = l.iter().sum::<f32>() / n;
    let var = l
        .iter()
        .map(|v| {
            let d = v - mean;
            d * d
        })
        .sum::<f32>()
        / n;
    let contrast = var.sqrt().clamp(0.0, 0.5) / 0.5;
    let exposure = 1.0 - (mean - 0.45).abs().clamp(0.0, 0.45) / 0.45;
    let clip = 1.0 - clipped(img).clamp(0.0, 1.0);
    let sharp = (sharpness(img) / 100.0).clamp(0.0, 1.0);
    let (iw, ih) = (img.width.max(1), img.height.max(1));
    let step = ((iw * ih) / 4096).max(1);
    let mut chroma = 0.0f32;
    let mut cn = 0u32;
    for p in img.data.iter().step_by(step) {
        chroma += (p[0] - p[1]).abs() + (p[1] - p[2]).abs();
        cn += 1;
    }
    let colour = (chroma / cn.max(1) as f32 / 0.4).clamp(0.0, 1.0);
    let thirds = rule_of_thirds(&l, w, h);
    let s = 0.28 * sharp + 0.22 * exposure + 0.18 * contrast + 0.16 * clip + 0.10 * colour + 0.06 * thirds;
    (1.0 + 9.0 * s.clamp(0.0, 1.0)).clamp(1.0, 10.0)
}

/// Combined 0..1 cull score from classical measures (no personal model).
pub fn cull_score(sharp: f32, clip: f32, best: bool, grouped: bool, eyes: Option<f32>, aesthetic_10: f32) -> f32 {
    let focus = (sharp / 100.0).clamp(0.0, 1.0);
    let clean = 1.0 - clip.clamp(0.0, 1.0);
    let burst = if grouped { if best { 1.0 } else { 0.35 } } else { 0.75 };
    let eyes_t = eyes.unwrap_or(0.55);
    let aes = ((aesthetic_10 - 1.0) / 9.0).clamp(0.0, 1.0);
    (0.34 * focus + 0.18 * clean + 0.18 * burst + 0.15 * eyes_t + 0.15 * aes).clamp(0.0, 1.0)
}

/// Eyes-open 0..1 from YuNet-style landmarks on a luma image: local contrast around each eye.
/// Five-point landmarks cannot measure eyelid gap; this is an honest pupil/iris contrast heuristic.
/// `landmarks` are fractions of width/height (y down); the first two are the eyes.
pub fn eyes_open(img: &Rgb32f, landmarks: &[(f32, f32)]) -> Option<f32> {
    let eyes = match landmarks.get(0..2) {
        Some(e) if e.iter().all(|(x, y)| x.is_finite() && y.is_finite()) => e,
        _ => return None,
    };
    let (l, w, h) = luma(img);
    if w < 8 || h < 8 {
        return None;
    }
    let mut scores = [0.0f32; 2];
    for (i, (x, y)) in eyes.iter().enumerate() {
        if let Some(slot) = scores.get_mut(i) {
            *slot = eye_contrast(&l, w, h, *x, *y);
        }
    }
    Some(((scores[0] + scores[1]) * 0.35 + scores[0].min(scores[1]) * 0.30).clamp(0.0, 1.0))
}

fn rule_of_thirds(l: &[f32], w: usize, h: usize) -> f32 {
    if w < 8 || h < 8 {
        return 0.5;
    }
    let mut at = 0.0f32;
    let mut centre = 0.0f32;
    let mut n_at = 0u32;
    let mut n_c = 0u32;
    let (t1x, t2x, t1y, t2y) = (w / 3, (2 * w) / 3, h / 3, (2 * h) / 3);
    let r = (w.min(h) / 10).max(1);
    let acc = |x0: usize, y0: usize, out: &mut f32, n: &mut u32| {
        for y in y0.saturating_sub(r)..(y0 + r + 1).min(h) {
            for x in x0.saturating_sub(r)..(x0 + r + 1).min(w) {
                if let Some(v) = l.get(y.saturating_mul(w).saturating_add(x)) {
                    *out += *v;
                    *n += 1;
                }
            }
        }
    };
    acc(t1x, t1y, &mut at, &mut n_at);
    acc(t2x, t1y, &mut at, &mut n_at);
    acc(t1x, t2y, &mut at, &mut n_at);
    acc(t2x, t2y, &mut at, &mut n_at);
    acc(w / 2, h / 2, &mut centre, &mut n_c);
    let a = at / n_at.max(1) as f32;
    let c = centre / n_c.max(1) as f32;
    (0.55 + 0.45 * (a - c).abs().clamp(0.0, 0.4) / 0.4).clamp(0.0, 1.0)
}

fn eye_contrast(l: &[f32], w: usize, h: usize, nx: f32, ny: f32) -> f32 {
    let x = ((nx.clamp(0.0, 1.0) * w as f32).round() as usize).min(w.saturating_sub(1));
    let y = ((ny.clamp(0.0, 1.0) * h as f32).round() as usize).min(h.saturating_sub(1));
    let r = (w.min(h) / 16).max(2);
    let mut sum = 0.0f32;
    let mut sum2 = 0.0f32;
    let mut n = 0u32;
    let mut centre = 0.0f32;
    let mut cn = 0u32;
    let cr = (r / 3).max(1);
    for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
        for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
            if let Some(v) = l.get(yy.saturating_mul(w).saturating_add(xx)) {
                sum += *v;
                sum2 += *v * *v;
                n += 1;
                if yy.abs_diff(y) <= cr && xx.abs_diff(x) <= cr {
                    centre += *v;
                    cn += 1;
                }
            }
        }
    }
    if n < 4 {
        return 0.5;
    }
    let mean = sum / n as f32;
    let var = (sum2 / n as f32 - mean * mean).max(0.0);
    let ring = var.sqrt().clamp(0.0, 0.25) / 0.25;
    let pupil = if cn > 0 { (mean - centre / cn as f32).clamp(0.0, 0.4) / 0.4 } else { 0.0 };
    (0.55 * ring + 0.45 * pupil).clamp(0.0, 1.0)
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
        let aest = aesthetic(&img);
        assert!((1.0..=10.0).contains(&aest), "{aest}");
        assert!(cull_score(80.0, 0.01, true, true, Some(0.9), aest) > cull_score(20.0, 0.4, false, true, Some(0.1), 2.0));
    }

    #[test]
    fn eyes_open_needs_two_landmarks_and_is_in_range() {
        let img = lightcraft_scenes::demo_library()[0].render(240, 160);
        assert!(eyes_open(&img, &[]).is_none());
        assert!(eyes_open(&img, &[(0.3, 0.4)]).is_none());
        let s = eyes_open(&img, &[(0.35, 0.4), (0.62, 0.4)]).expect("two eyes");
        assert!((0.0..=1.0).contains(&s), "{s}");
    }
}
