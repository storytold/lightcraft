use super::look_fit::dro_off;
use super::pairs::{Pairs, displayed, luma};
use super::*;

/// How the per-photo tone curve is fitted ([`fit_pairs_ordered`] tries them in order).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ToneFit {
    /// [`fit_tone`]: no flat steps, no pixel-exact registration needed.
    Quantile,
    /// [`fit_tone_conditional`]: the earlier fit, kept as the fallback.
    ConditionalMedian,
}

impl ToneFit {
    pub(super) fn fit(self, pairs: Vec<(f64, f64)>) -> Option<CameraTone> {
        match self {
            ToneFit::Quantile => fit_tone(pairs),
            ToneFit::ConditionalMedian => fit_tone_conditional(pairs),
        }
    }
}

/// The bright areas a narrow tone curve is checked against ([`highlights_hold`]): every proxy pixel
/// whose camera JPEG luminance is 0.85 or more, with its sensor value as developed (for a clipped
/// pixel a lower bound of the scene). Taken from the proxies before any training filter (clipped
/// pixels, edges, the sensor value range of [`collect_pairs`]), so no retry of the search can
/// leave them out.
pub(super) fn highlights(sensor: &Rgb32f, reference: &Rgb32f) -> Pairs {
    if sensor.data.len() != reference.data.len() {
        return Vec::new();
    }
    sensor
        .data
        .iter()
        .zip(&reference.data)
        .filter(|(x, y)| x.iter().chain(y.iter()).all(|v| v.is_finite() && *v >= 0.0) && luminance_2020(**y) >= 0.85)
        .map(|(x, y)| (x.map(f64::from), y.map(f64::from)))
        .collect()
}

/// Smallest ratio between the scene luminance of the brightest and of the darkest 1/32 of the
/// pairs a tone curve is fitted to (its last and first knot). Below it the training pairs are
/// nearly uniform (under 0.26 EV from their darker to their brighter parts): the curve would be
/// fitted to a sliver of the tone range and the rest of it extrapolated. Above it the acceptance
/// gates judge the curve. This was 1.5, which rejected low-contrast scenes the curve fits very
/// well: in a hazy ILCE-7RM4 burst the frames at 1.46–1.49 opened with the neutral fallback (no full or partial
/// look, 18 L* darker than the camera JPEG) while the frame at 1.51 fitted at a held-out RMS of
/// 0.013, as the others do once allowed (issue #631). Across 135 public Sony files the lowest is 4.0.
/// A curve under [`NARROW_TONE_RANGE`] must also pass [`highlights_hold`].
const MIN_TONE_RANGE: f32 = 1.2;

/// [`MIN_TONE_RANGE`] before issue #631. A curve fitted to a narrower range than this must also
/// render the photo's bright areas no further from the camera JPEG than the neutral fallback does
/// ([`highlights_hold`]). Neither the fit nor the held-out gates see those areas (camera JPEG
/// luminance 0.85 and up, left out of the training pairs by [`collect_pairs`]), and a curve fitted
/// to a sliver of the tone range extrapolates them: in review, a synthetic frame whose curve passed
/// the gates at a held-out RMS of 0.0004 rendered its bright sky at 0.29 where the camera JPEG has
/// 0.93.
const NARROW_TONE_RANGE: f32 = 1.5;

/// What a narrow tone curve is checked against ([`highlights_hold`]).
pub(super) struct HighlightCheck {
    /// The frame's [`highlights`].
    pub(super) pairs: Pairs,
    /// The photo's curve will be lowered to Sony's without DRO ([`with_dro_off_tone`]) after the
    /// fit, so the check looks at it lowered.
    pub(super) dro: bool,
    /// The camera JPEG is black and white ([`fit_tone_only_with`]): the check compares luminance, as that
    /// fit's gates do, since the JPEG has no colour to match.
    pub(super) grey: bool,
}

/// Whether a look with this `tone` and `colour` (sensor value to the finish stage's input) keeps
/// the frame's [`highlights`] (camera JPEG luminance 0.85 and up, left out of the fit) no further
/// from the camera JPEG than the neutral fallback does, with the tone curve as the photo will get
/// it (lowered for DRO), in luminance alone for a black-and-white JPEG. Only asked of curves narrower than
/// [`NARROW_TONE_RANGE`]: true for wider ones, and for frames without such pixels.
pub(super) fn highlights_hold(tone: &CameraTone, check: &HighlightCheck, colour: impl Fn([f64; 3]) -> [f64; 3]) -> bool {
    let knots = tone.knots();
    if knots[31][0] >= knots[0][0] * NARROW_TONE_RANGE {
        return true;
    }
    // the curve as the photo will get it
    let used = if check.dro { dro_off(tone) } else { *tone };
    let (map, neutral) = (ToneMap::camera(&used, 0.0, 0.0, 0.0), ToneMap::new(0.0, 0.0, 0.0));
    let (mut look, mut fallback) = (0.0, 0.0);
    for (x, target) in &check.pairs {
        let (rendered, base) = (displayed(colour(*x), &map), displayed(*x, &neutral));
        if check.grey {
            look += (luma(rendered) - luma(*target)).powi(2);
            fallback += (luma(base) - luma(*target)).powi(2);
            continue;
        }
        for c in 0..3 {
            look += (rendered[c] - target[c]).powi(2);
            fallback += (base[c] - target[c]).powi(2);
        }
    }
    let hold = look.is_finite() && look <= fallback;
    if !hold && lightcraft_pipeline::profiling() {
        eprintln!("[profile] camera look: narrow tone curve renders the bright areas worse than the fallback ({look:.3} > {fallback:.3})");
    }
    hold
}

/// The camera's tone curve from scene/JPEG luminance pairs, by quantile matching: knot `i` pairs
/// the median of the `i`-th 1/32 of the sorted scene luminances with the median of the `i`-th
/// 1/32 of the sorted JPEG luminances. A tone curve is monotone, so it maps each quantile of the
/// scene to the same quantile of the JPEG; matching the two distributions needs no pixel-exact
/// registration and is increasing by construction.
///
/// Binning the pairs by scene luminance and taking the JPEG's median per bin instead (followed by
/// isotonic pooling) fails on real files: camera JPEGs are lens-corrected and sharpened, so on a
/// 96 px proxy of a busy scene neighbouring pairs disagree by more than the gap between bins.
/// The conditional medians then zig-zag, pooling turns them into flat steps (13 of 31 segments
/// on a Z 6II forest, issue #475) that render as posterised patches, and regression toward the
/// mean flattens both ends of the curve (lifted blacks, dull highlights).
pub(super) fn fit_tone(pairs: Vec<(f64, f64)>) -> Option<CameraTone> {
    if pairs.len() < 128 || !pairs.iter().all(|(x, y)| x.is_finite() && *x > 0.0 && y.is_finite()) {
        return None;
    }
    let mut xs: Vec<f64> = pairs.iter().map(|p| p.0).collect();
    let mut ys: Vec<f64> = pairs.iter().map(|p| p.1).collect();
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    let n = xs.len();
    let mut knots = [[0.0; 2]; 32];
    for (i, knot) in knots.iter_mut().enumerate() {
        let (a, b) = (i * n / 32, (i + 1) * n / 32);
        let mid = a + (b - a) / 2;
        *knot = [*xs.get(mid)? as f32, *ys.get(mid)? as f32];
    }
    if knots[31][0] < knots[0][0] * MIN_TONE_RANGE {
        return None;
    }
    CameraTone::new(knots)
}

fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied()
}

/// The earlier tone fit, kept as the fallback of [`ToneFit`]: the JPEG's median per equal-population
/// bin of scene luminance, then pooled-adjacent-violators isotonic regression. It follows the
/// per-pixel pairs closely when the two images are registered, which is why it can clear the
/// acceptance gates where [`fit_tone`] just misses them, but on lens-corrected JPEGs of busy scenes
/// the pooling leaves flat steps (issue #475).
pub(super) fn fit_tone_conditional(mut pairs: Vec<(f64, f64)>) -> Option<CameraTone> {
    if pairs.len() < 128 || !pairs.iter().all(|(x, y)| x.is_finite() && *x > 0.0 && y.is_finite()) {
        return None;
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut knots = [[0.0; 2]; 32];
    for (i, knot) in knots.iter_mut().enumerate() {
        let bin = pairs.get(i * pairs.len() / 32..(i + 1) * pairs.len() / 32)?;
        let mut xs: Vec<_> = bin.iter().map(|p| p.0).collect();
        let mut ys: Vec<_> = bin.iter().map(|p| p.1).collect();
        *knot = [median(&mut xs)? as f32, median(&mut ys)? as f32];
    }
    if knots[31][0] < knots[0][0] * MIN_TONE_RANGE {
        return None;
    }
    // Pool adjacent violating bins (isotonic regression): no reversals or arbitrary polynomial.
    let mut blocks: Vec<(f32, usize)> = Vec::new();
    for knot in knots {
        blocks.push((knot[1], 1));
        while blocks.len() >= 2 {
            let (a, an) = *blocks.get(blocks.len() - 2)?;
            let (b, bn) = *blocks.last()?;
            if a <= b {
                break;
            }
            blocks.truncate(blocks.len() - 2);
            blocks.push(((a * an as f32 + b * bn as f32) / (an + bn) as f32, an + bn));
        }
    }
    let mut i = 0;
    for (y, n) in blocks {
        for knot in knots.get_mut(i..i + n)? {
            knot[1] = y;
        }
        i += n;
    }
    CameraTone::new(knots)
}
