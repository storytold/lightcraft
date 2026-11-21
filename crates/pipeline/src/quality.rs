//! Image-quality measurements for the Image Quality Scanner (issue #1313): the four technical
//! factors a cull by hand costs time for — blur (focus), noise, exposure and compression (JPEG
//! blockiness) — each 0..100 where 100 is clean, measured on a display-encoded luminance copy at a
//! fixed size so the numbers compare across photos, and combined into one 0..100 quality score with
//! per-factor weights.
//!
//! No learned model: the factors are classical measures with honest names (like
//! [`crate::cull`]'s). A learned aesthetic score may join them later through the AI backend
//! (`lcc_ml`, issue #488); this module says in [`QualityFactors`] what it measured and nothing it
//! did not.

use lcc_raster::Rgb32f;

/// Display-ish luminance (sRGB-encoded, 0..1) at the measure size: assisted culling's own
/// luminance ([`crate::cull::luma`]), box filtered to fit 512 px — the size it measures at — so a
/// scan and a cull of one photo measure the same picture.
fn luma(img: &Rgb32f) -> (Vec<f32>, usize, usize) {
    crate::cull::luma(img)
}

/// Which quality factors a scan weighs, and how much (a weight of 0 leaves a factor out of the
/// score; it is still measured and reported).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QualityWeights {
    pub blur: f32,
    pub noise: f32,
    pub exposure: f32,
    pub compression: f32,
}

impl Default for QualityWeights {
    /// The four factors weigh alike: a technical scan has no reason to trust one more than another.
    fn default() -> Self {
        QualityWeights { blur: 1.0, noise: 1.0, exposure: 1.0, compression: 1.0 }
    }
}

impl QualityWeights {
    /// Weights clamped to 0..=10 and swept for numbers that can't be (NaN, infinities): a weight
    /// that isn't a number measures nothing, never a NaN score.
    #[must_use]
    pub fn sane(self) -> QualityWeights {
        let one = |v: f32| if v.is_finite() { v.clamp(0.0, 10.0) } else { 0.0 };
        QualityWeights { blur: one(self.blur), noise: one(self.noise), exposure: one(self.exposure), compression: one(self.compression) }
    }
}

/// What a quality scan measured on one photo: four factors, each 0..100 where 100 is clean, and
/// the combined score. `None` where a factor could not be measured (compression on a raw photo,
/// which was never JPEG encoded).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QualityFactors {
    /// Sharpness (blur's inverse): detail energy in the sharpest areas (photo.analyze's focus
    /// measure), 0..100.
    pub sharpness: f32,
    /// Freedom from noise: 100 × (1 − the grain this photo carries against the reference below),
    /// 0..100.
    pub noise: f32,
    /// Exposure: the share of the photo that isn't clipped, plus how close its tones sit to a
    /// well-exposed middle, 0..100.
    pub exposure: f32,
    /// Freedom from JPEG blockiness, 0..100; `None` when the source was not measured for it (a raw
    /// photo has no JPEG blocks of its own to see).
    pub compression: Option<f32>,
    /// The combined 0..100 score: the factors' weighted mean. `None` when no factor has a weight.
    pub score: Option<f32>,
}

impl QualityFactors {
    /// Measure `img` (a decoded source, any size, any number of pixels) with `weights`. `jpeg`:
    /// the source's encoded form is JPEG (or was derived from one), so blockiness is measured.
    /// Never panics on a hostile buffer: an empty or one-pixel image measures cleanly.
    #[must_use]
    pub fn measure(img: &Rgb32f, weights: QualityWeights, jpeg: bool) -> QualityFactors {
        let w = weights.sane();
        let sharpness = crate::cull::sharpness(img).clamp(0.0, 100.0);
        let noise = noise_score(img);
        let exposure = exposure_score(img);
        let compression = jpeg.then(|| compression_score(img));
        let score = combine(sharpness, noise, exposure, compression, w);
        QualityFactors { sharpness, noise, exposure, compression, score }
    }

    /// The combined score again from other weights (the factors are stored as measured, so a
    /// re-weighting needn't read the photo a second time).
    #[must_use]
    pub fn rescore(&self, weights: QualityWeights) -> Option<f32> {
        combine(self.sharpness, self.noise, self.exposure, self.compression, weights.sane())
    }
}

/// The weighted mean of the factors that have a value, `None` when no weight is positive.
fn combine(sharpness: f32, noise: f32, exposure: f32, compression: Option<f32>, w: QualityWeights) -> Option<f32> {
    let mut sum = 0.0f32;
    let mut weight = 0.0f32;
    for (value, weight_of) in [(Some(sharpness), w.blur), (Some(noise), w.noise), (Some(exposure), w.exposure), (compression, w.compression)] {
        let Some(value) = value.filter(|_| weight_of > 0.0) else { continue };
        if !value.is_finite() {
            continue;
        }
        sum += weight_of * value.clamp(0.0, 100.0);
        weight += weight_of;
    }
    (weight > 0.0).then(|| (sum / weight).clamp(0.0, 100.0))
}

/// The grain a photo is measured against: the mean |photo − 3 × 3 smooth| in a clean photo's
/// flattest areas at this measurement size. A crisp photo's fine detail lands near it; a noisy
/// one reads well above, a smoothed one below (and scores full marks).
const NOISE_REFERENCE: f32 = 0.0035;

/// Noise 0..100 (100: grain-free): the grain in the flattest half of the photo, where there is no
/// real detail to mistake for grain. Grain is what a 3 × 3 smoothing takes away; the blocks with
/// the smallest spread pick where that is measured, so an edge-heavy photo (a brick wall) doesn't
/// read as noisy.
fn noise_score(img: &Rgb32f) -> f32 {
    let (l, w, h) = luma(img);
    if w < 4 || h < 4 {
        return 100.0;
    }
    // the 3 × 3 mean (clamped at the borders)
    let mut smooth = vec![0f32; l.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0f32;
            let mut n = 0u32;
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    if let Some(v) = l.get(yy * w + xx) {
                        acc += v;
                        n += 1;
                    }
                }
            }
            if let Some(slot) = smooth.get_mut(y * w + x) {
                *slot = if n > 0 { acc / n as f32 } else { 0.0 };
            }
        }
    }
    // per block: the mean grain and the block's own spread (its detail)
    const B: usize = 16;
    let mut blocks: Vec<(f32, f32)> = Vec::new();
    for by in (1..h.saturating_sub(1)).step_by(B) {
        for bx in (1..w.saturating_sub(1)).step_by(B) {
            let mut grain = 0.0f32;
            let mut sum = 0.0f32;
            let mut sum2 = 0.0f32;
            let mut n = 0u32;
            for y in by..(by + B).min(h - 1) {
                for x in bx..(bx + B).min(w - 1) {
                    let (Some(v), Some(s)) = (l.get(y * w + x), smooth.get(y * w + x)) else { continue };
                    grain += (v - s).abs();
                    sum += v;
                    sum2 += v * v;
                    n += 1;
                }
            }
            if n >= 8 {
                let mean = sum / n as f32;
                let spread = (sum2 / n as f32 - mean * mean).max(0.0).sqrt();
                blocks.push((grain / n as f32, spread));
            }
        }
    }
    if blocks.is_empty() {
        return 100.0;
    }
    let k = blocks.len() / 2;
    let mid = blocks.select_nth_unstable_by(k, |a, b| a.1.total_cmp(&b.1)).1.1;
    let mut grain = 0.0f32;
    let mut n = 0u32;
    for (g, spread) in &blocks {
        if *spread <= mid {
            grain += *g;
            n += 1;
        }
    }
    let grain = if n > 0 { grain / n as f32 } else { return 100.0 };
    (100.0 * (1.0 - (grain / NOISE_REFERENCE).clamp(0.0, 1.0))).clamp(0.0, 100.0)
}

/// Exposure 0..100: 60 % the share of the photo that isn't clipped (blown highlights or crushed
/// blacks), 40 % how close its mean tone sits to a well-exposed middle (0.42). A blown-out or
/// crushed photo reads worse than a mid-toned one.
fn exposure_score(img: &Rgb32f) -> f32 {
    let (l, _, _) = luma(img);
    let n = l.len().max(1) as f32;
    let mut clipped = 0u32;
    let mut sum = 0f32;
    for v in &l {
        if !v.is_finite() {
            continue;
        }
        if *v >= 0.985 || *v <= 0.002 {
            clipped += 1;
        }
        sum += *v;
    }
    let clean = 1.0 - clipped as f32 / n;
    let mean = (sum / n).clamp(0.0, 1.0);
    let balance = 1.0 - ((mean - 0.42).abs() / 0.42).clamp(0.0, 1.0);
    (100.0 * (0.60 * clean + 0.40 * balance)).clamp(0.0, 100.0)
}

/// JPEG blockiness 0..100 (100: no blocks): the step across the 8-pixel grid lines (where JPEG's
/// transform blocks meet) beyond what the photo's other columns carry, against that ordinary
/// energy (a photo with strong real edges tolerates a larger step). A re-compressed or heavily
/// compressed JPEG reads lower than a mild one; a photo without blocks reads near 100.
fn compression_score(img: &Rgb32f) -> f32 {
    let (l, w, h) = luma(img);
    if w < 32 || h < 32 {
        return 100.0;
    }
    // the mean |step| across each vertical boundary x − 1 | x, down the photo
    let mut across = vec![0f32; w];
    for y in 0..h {
        for x in 1..w {
            let (Some(a), Some(b)) = (l.get(y * w + x - 1), l.get(y * w + x)) else { continue };
            if let Some(slot) = across.get_mut(x) {
                *slot += (b - a).abs();
            }
        }
    }
    let scale = 1.0 / h.max(1) as f32;
    for v in &mut across {
        *v *= scale;
        if !v.is_finite() {
            *v = 0.0;
        }
    }
    let mean = |xs: &mut dyn Iterator<Item = usize>| -> f32 {
        let mut sum = 0f32;
        let mut n = 0u32;
        for x in xs {
            if let Some(v) = across.get(x) {
                sum += *v;
                n += 1;
            }
        }
        if n > 0 { sum / n as f32 } else { 0.0 }
    };
    let on = mean(&mut (8..w).step_by(8));
    let off = mean(&mut (1..w).filter(|x| x % 8 != 0));
    if on <= off || off <= 1e-6 {
        // no extra energy on the grid: no visible blocks (or a photo too flat to carry any)
        return 100.0;
    }
    let ratio = (on - off) / off;
    (100.0 * (1.0 - (ratio / 0.35).clamp(0.0, 1.0))).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Rgb32f {
        let mut img = Rgb32f { width: w, height: h, data: vec![[0f32; 3]; w * h] };
        for y in 0..h {
            for x in 0..w {
                let v = f(x, y);
                img.data[y * w + x] = [v, v, v];
            }
        }
        img
    }

    /// A clean, detailed, mid-toned photo scores high on every factor it is measured for: flat
    /// mid-tones on one side (where the grain is read) and fine detail on the other (the sharpness),
    /// never clipped.
    #[test]
    fn a_clean_photo_scores_high() {
        let img = image(256, 256, |x, y| {
            if x < 128 {
                0.45 + 0.06 * ((x / 64) % 2) as f32
            } else if (x / 4 + y / 4) % 2 == 0 {
                0.55
            } else {
                0.30
            }
        });
        let q = QualityFactors::measure(&img, QualityWeights::default(), true);
        assert!(q.sharpness > 20.0, "sharpness {}", q.sharpness);
        assert!(q.noise > 50.0, "freedom from noise {}", q.noise);
        assert!(q.exposure > 70.0, "exposure {}", q.exposure);
        assert!((0.0..=100.0).contains(&q.score.unwrap()), "{q:?}");
        assert!(q.compression.is_some_and(|c| (0.0..=100.0).contains(&c)), "{q:?}");
    }

    /// Grain reads as noise: a flat field with speckle scores lower for noise than the same field
    /// clean, and the combined score follows.
    #[test]
    fn grain_lowers_the_noise_score() {
        let clean = image(128, 128, |_, _| 0.5);
        let mut noisy = clean.clone();
        for (i, p) in noisy.data.iter_mut().enumerate() {
            let s = if i % 2 == 0 { 0.06 } else { -0.06 };
            *p = [0.5 + s, 0.5 + s, 0.5 + s];
        }
        let a = QualityFactors::measure(&clean, QualityWeights::default(), true);
        let b = QualityFactors::measure(&noisy, QualityWeights::default(), true);
        assert!(b.noise < a.noise, "grainy {} vs clean {}", b.noise, a.noise);
        assert!(b.score.unwrap() < a.score.unwrap(), "the combined score follows");
    }

    /// A blown-out or crushed photo reads worse for exposure than a mid-toned one.
    #[test]
    fn clipping_and_off_mid_tones_cost_exposure() {
        let mid = image(64, 64, |x, y| 0.35 + 0.15 * (((x + y) % 7) as f32 / 7.0));
        let blown = image(64, 64, |_, _| 1.0);
        let crushed = image(64, 64, |_, _| 0.0);
        let m = exposure_score(&mid);
        assert!(m > exposure_score(&blown), "mid {m} vs blown {}", exposure_score(&blown));
        assert!(m > exposure_score(&crushed), "mid {m} vs crushed {}", exposure_score(&crushed));
    }

    /// JPEG blocks read as compression artifacts: a photo with 8-pixel steps scores lower than the
    /// same photo without them, and a photo that was never JPEG encoded (a raw) reports `None`.
    #[test]
    fn blockiness_lowers_the_compression_score() {
        // (a gradient carries the ordinary edge energy a photo has; the tiles' 8-pixel steps sit on
        // the JPEG grid, where a real edge does not)
        let blocks = image(128, 128, |x, y| 0.30 + 0.20 * (x as f32 / 128.0) + 0.05 * ((x / 8 + y / 8) % 2) as f32);
        let smooth = image(128, 128, |x, _| 0.30 + 0.20 * (x as f32 / 128.0));
        let b = compression_score(&blocks);
        let s = compression_score(&smooth);
        assert!(b < s, "blocky {b} vs smooth {s}");
        let raw = QualityFactors::measure(&smooth, QualityWeights::default(), false);
        assert_eq!(raw.compression, None, "a raw is not measured for JPEG blocks");
    }

    /// Weights: a factor switched off stays measured but doesn't move the score, and weights that
    /// aren't numbers measure nothing rather than a NaN.
    #[test]
    fn weights_pick_the_factors_that_count() {
        let img = image(64, 64, |x, y| 0.3 + 0.2 * (((x * 3 + y) % 5) as f32 / 5.0));
        let only_blur = QualityWeights { blur: 1.0, noise: 0.0, exposure: 0.0, compression: 0.0 };
        let q = QualityFactors::measure(&img, only_blur, true);
        assert!((q.score.unwrap() - q.sharpness).abs() < 1e-4, "only blur counts");
        let none = QualityWeights { blur: 0.0, noise: 0.0, exposure: 0.0, compression: 0.0 };
        assert_eq!(QualityFactors::measure(&img, none, true).score, None, "no factor counts");
        let bad = QualityWeights { blur: f32::NAN, noise: f32::INFINITY, exposure: -3.0, compression: 2.0 };
        let q = QualityFactors::measure(&img, bad, true);
        assert!(q.score.is_some_and(|s| s.is_finite()), "hostile weights give a finite score");
    }

    /// Hostile buffers — empty, one-pixel, and one shorter than its size claims — measure without a
    /// division by zero or an index past the data.
    #[test]
    fn empty_and_tiny_images_measure_cleanly() {
        for img in [
            Rgb32f { width: 0, height: 0, data: Vec::new() },
            Rgb32f { width: 1, height: 1, data: vec![[0.5; 3]] },
            Rgb32f { width: 64, height: 64, data: vec![[0.5; 3]; 7] },
        ] {
            let q = QualityFactors::measure(&img, QualityWeights::default(), true);
            assert!(q.score.is_some_and(|s| s.is_finite()), "{q:?}");
            for v in [q.sharpness, q.noise, q.exposure] {
                assert!(v.is_finite(), "{q:?}");
            }
        }
    }

    /// A re-weighting gives the score measuring with those weights gives.
    #[test]
    fn rescoring_matches_measuring() {
        let img = image(96, 96, |x, y| 0.25 + 0.4 * (((x / 3 + y / 3) % 2) as f32));
        let w = QualityWeights { blur: 2.0, noise: 0.5, exposure: 1.5, compression: 0.0 };
        let q = QualityFactors::measure(&img, QualityWeights::default(), true);
        let again = q.rescore(w).unwrap();
        let direct = QualityFactors::measure(&img, w, true).score.unwrap();
        assert!((again - direct).abs() < 1e-3, "{again} vs {direct}");
    }
}
