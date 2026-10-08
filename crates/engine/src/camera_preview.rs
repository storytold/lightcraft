//! Estimate the starting look of a raw without a camera colour matrix (Sony ARW, Nikon NEF) from its
//! own JPEG. Colour and luminance are fitted separately; the JPEG supplies correspondences only,
//! never output pixels or a replacement for RAW editing.
//! A global matrix can't follow the camera's hue-dependent rendering (the best matrix rendered a
//! lime shirt olive that the camera kept lime): a hue/saturation table fitted to the residuals
//! (applied like a DNG `ProfileHueSatMap`) corrects that when it also improves the held-out pixels.
use lightcraft_color::{D50, D65, Mat3, PROPHOTO, REC2020, bradford, luminance_2020};
use lightcraft_pipeline::tone::{CameraTone, ToneMap};
use lightcraft_raster::{
    Rgb32f,
    resample::{Filter, fit},
};
use lightcraft_raw::{RawFormat, RawImage, color::CameraTransform, profile::HsvTable};

#[derive(Clone, Debug)]
pub(crate) struct CameraLook {
    pub matrix: Mat3,
    pub tone: CameraTone,
    /// Hue/saturation correction after `matrix`, in linear ProPhoto RGB (DNG `ProfileHueSatMap`).
    pub hue_sat: Option<HsvTable>,
}

/// Long edge of the sensor/JPEG proxy a single photo's look is fitted on (the acceptance gates
/// below were set at this size).
const PROXY: usize = 96;
/// Long edge of the proxies pooled for a camera profile: small objects (a shirt) get 4× the samples.
pub(crate) const PROFILE_PROXY: usize = 192;

/// Raw formats whose decoder supplies vendor white-balance multipliers but no camera colour matrix:
/// their starting look is fitted to the file's own JPEG, and white balance is relative to the
/// as-shot look (`docs/camera-preview-colour.md`). The catalog's `Photo::relative_wb` matches the
/// same formats by file extension.
pub(crate) fn file_local_look(format: RawFormat) -> bool {
    matches!(format, RawFormat::Arw | RawFormat::Nef | RawFormat::Nrw)
}

pub(crate) fn fit_preview(raw: &RawImage, bytes: &[u8], transform: &CameraTransform) -> Option<CameraLook> {
    if !transform.matrix_is_fallback || !file_local_look(raw.format) {
        return None;
    }
    let (sensor, reference) = proxies(raw, bytes, transform, PROXY)?;
    // A camera profile pooled from many photos knows colours this photo shows too little of;
    // only the tone and chroma curves are fitted per photo (DRO and picture styles vary).
    let profile = raw.metadata.model.as_deref().and_then(crate::camera_profiles::get);
    let colour = profile.as_ref().and_then(|p| Some((p.matrix().mul(&transform.matrix.inverse()?), p.hue_sat.clone())));
    let look = fit_pairs_with(&sensor, &reference, colour)?;
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] {:?} camera look: {:?}, {:?}, hue/sat table {}, camera profile {}",
            raw.format,
            look.matrix.0,
            look.tone,
            look.hue_sat.is_some(),
            profile.is_some()
        );
    }
    Some(look)
}

/// Same-size proxies of the sensor (white-balanced, baseline exposure, through `transform`'s
/// matrix: the generic camera ≈ sRGB model) and of the file's embedded camera JPEG (linear Rec.2020).
fn proxies(raw: &RawImage, bytes: &[u8], transform: &CameraTransform, size: usize) -> Option<(Rgb32f, Rgb32f)> {
    let jpeg = lightcraft_raw::embedded_preview(bytes)?;
    let edge = (2 * size).max(384) as u32;
    let decoded = lightcraft_codecs::decode(&jpeg, lightcraft_codecs::DecodeOptions { max_size: Some((edge, edge)), max_pixels: 64_000_000 }).ok()?;
    let reference = decoded.to_working();
    let crop = raw.crop.clipped(raw.active_area.width, raw.active_area.height);
    if crop.width == 0 || crop.height == 0 || reference.width == 0 || reference.height == 0 {
        return None;
    }
    let aspect = crop.width as f64 / crop.height as f64;
    if (reference.width as f64 / reference.height as f64 / aspect - 1.0).abs() > 0.02 {
        return None;
    }
    // Fixed, bounded proxy: the selected look cannot depend on thumbnail/export resolution.
    let k = (crop.width.max(crop.height).div_ceil(edge as usize).max(2)).div_ceil(2) * 2;
    let sensor = raw.develop_binned(k, 0.99).ok()??;
    let mut sensor = fit(&sensor, size, size, Filter::Box);
    let reference = fit(&reference, sensor.width, sensor.height, Filter::Box);
    let gain = 2f32.powf(transform.baseline_exposure as f32);
    sensor.map_in_place(|p| transform.matrix.apply_f32(std::array::from_fn(|i| p[i] * transform.wb[i] * gain)));
    Some((sensor, reference))
}

/// Colour training pairs of one raw for a camera profile: white-balanced camera RGB (with the
/// baseline exposure) → its camera JPEG (linear Rec.2020). `None` for formats with their own
/// colour matrices, other files or unusable previews.
pub(crate) fn profile_pairs(raw: &RawImage, bytes: &[u8]) -> Option<Vec<([f64; 3], [f64; 3])>> {
    if !file_local_look(raw.format) || lightcraft_raw::color::has_matrix(&raw.color) {
        return None;
    }
    let transform = lightcraft_raw::color::camera_transform(raw, lightcraft_raw::color::as_shot_white_xy(raw));
    let (sensor, reference) = proxies(raw, bytes, &transform, PROFILE_PROXY)?;
    let to_camera = transform.matrix.inverse()?;
    let (pairs, _) = collect_pairs(&sensor, &reference)?;
    Some(pairs.into_iter().map(|(x, y)| (to_camera.apply(x), y)).collect())
}

/// A camera profile's colour model (matrix from white-balanced camera RGB, hue/saturation table)
/// fitted to pairs pooled from many photos.
pub(crate) fn fit_profile(pairs: &[([f64; 3], [f64; 3])]) -> Option<(Mat3, Option<HsvTable>)> {
    let matrix = fit_matrix(pairs)?;
    Some((matrix, fit_hue_sat(pairs, &matrix)))
}

/// A fit must cut the held-out squared error to below this share of the fallback's.
const MIN_IMPROVEMENT: f64 = 0.7;
/// ...and stay within this per-channel RMS of the camera JPEG (linear display values). On public
/// raw.pixls.us samples (eight Sony bodies) good fits that still beat the fallback 1.5–3× landed
/// at 0.065–0.093 (camera local tone, vignetting and lens processing that a global matrix + curve
/// cannot follow): visibly better renders that 0.055 rejected.
const MAX_HOLDOUT_RMS: f64 = 0.10;

fn luma(p: [f64; 3]) -> f64 {
    p[0] * 0.2627 + p[1] * 0.6780 + p[2] * 0.0593
}

/// The finish stage's tone map and chroma curve (`lightcraft_pipeline::finish`).
fn displayed(scene: [f64; 3], tone: &ToneMap) -> [f64; 3] {
    let scene = scene.map(|v| v.max(0.0));
    let y = luma(scene);
    if y <= 0.0 {
        return [0.0; 3];
    }
    let o = f64::from(tone.apply(y as f32));
    let k = f64::from(tone.chroma_scale(o as f32));
    scene.map(|v| o + (v * o / y - o) * k)
}

#[cfg(test)]
fn fit_pairs(sensor: &Rgb32f, reference: &Rgb32f) -> Option<CameraLook> {
    fit_pairs_with(sensor, reference, None)
}

/// Training pairs (sensor → JPEG, unclipped midtones) and the wider set including highlights
/// (for the chroma curve); `None` when too few, or the photo has too little colour.
type Pairs = Vec<([f64; 3], [f64; 3])>;
fn collect_pairs(sensor: &Rgb32f, reference: &Rgb32f) -> Option<(Pairs, Pairs)> {
    if (sensor.width, sensor.height) != (reference.width, reference.height) || sensor.data.len() != reference.data.len() {
        return None;
    }
    let mut pairs = Vec::new();
    // Highlights too (camera JPEGs bleach colours toward white there), for the chroma curve only.
    let mut bright = Vec::new();
    let mut colour = 0;
    for (input, output) in sensor.data.iter().zip(&reference.data) {
        let y = luminance_2020(*output);
        if !input.iter().all(|v| v.is_finite() && *v > 0.001 && *v < 1.5) || !output.iter().all(|v| v.is_finite() && *v >= 0.0) {
            continue;
        }
        if (0.015..=1.0).contains(&y) {
            bright.push((input.map(f64::from), output.map(f64::from)));
        }
        if !output.iter().all(|v| *v > 0.004 && *v < 0.98) || !(0.015..0.85).contains(&y) {
            continue;
        }
        let min = output.iter().copied().fold(f32::INFINITY, f32::min);
        let max = output.iter().copied().fold(0.0, f32::max);
        colour += usize::from(max - min > 0.05);
        pairs.push((input.map(f64::from), output.map(f64::from)));
    }
    (pairs.len() >= 256 && colour >= pairs.len() / 20).then_some((pairs, bright))
}

/// Ridge-regularised 3×3 chromaticity matrix (luminance-normalised RGB) on the training pairs.
fn fit_matrix(pairs: &[([f64; 3], [f64; 3])]) -> Option<Mat3> {
    let mut gram = [[0.0; 3]; 3];
    let mut cross = [[0.0; 3]; 3];
    for (i, (x, y)) in pairs.iter().enumerate() {
        if i % 3 == 0 {
            continue;
        }
        let (lx, ly) = (luma(*x), luma(*y));
        if lx <= 0.0 || ly <= 0.0 {
            continue;
        }
        for row in 0..3 {
            for col in 0..3 {
                // Normalising by luminance prevents a camera S-curve from corrupting colour.
                gram[row][col] += x[row] * x[col] / (lx * lx);
                cross[row][col] += y[row] * x[col] / (ly * lx);
            }
        }
    }
    let trace: f64 = (0..3).map(|i| gram[i][i]).sum();
    let inverse = Mat3(gram).inverse()?;
    let condition = trace * (0..3).map(|i| inverse.0[i][i].abs()).sum::<f64>();
    if trace <= 0.0 || !condition.is_finite() || condition > 1e6 {
        return None;
    }
    let regularization = trace * 1e-4;
    for i in 0..3 {
        gram[i][i] += regularization;
        cross[i][i] += regularization;
    }
    let matrix = Mat3(cross).mul(&Mat3(gram).inverse()?);
    matrix.0.iter().flatten().all(|v| v.is_finite() && v.abs() < 8.0).then_some(matrix)
}

/// The photo's look: its own matrix (with and without a hue/saturation table), or the given
/// colour model (a camera profile's, in the sensor proxy's space), each completed with a tone
/// and chroma curve fitted to this photo.
fn fit_pairs_with(sensor: &Rgb32f, reference: &Rgb32f, colour: Option<(Mat3, Option<HsvTable>)>) -> Option<CameraLook> {
    let (pairs, bright) = collect_pairs(sensor, reference)?;
    let candidates = match colour {
        Some(given) => vec![given],
        None => {
            let matrix = fit_matrix(&pairs)?;
            vec![(matrix, fit_hue_sat(&pairs, &matrix)), (matrix, None)]
        }
    };
    // Matrix + table when the table helps the held-out pixels, else the matrix alone. Chosen by
    // error in gamma-encoded display values (closer to what is seen: in linear values a slightly
    // missed bright rock outweighs a clearly wrong dark shirt); the acceptance gates below stay linear.
    let mut best: Option<(f64, f64, usize, CameraLook)> = None;
    for (matrix, hue_sat) in candidates {
        let correction = hue_sat.as_ref().and_then(HueSat::new);
        let colour = |x: [f64; 3]| {
            let p = matrix.apply(x);
            correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from))
        };
        let tone_pairs: Vec<_> =
            pairs.iter().enumerate().filter(|(i, _)| i % 3 != 0).map(|(_, (x, y))| (luma(colour(*x).map(|v| v.max(0.0))), luma(*y))).collect();
        let Some(curve) = fit_tone(tone_pairs) else { continue };
        let mut look = CameraLook { matrix, tone: curve, hue_sat: hue_sat.clone() };
        if let Some(tone) = fit_chroma(&bright, &look) {
            look.tone = tone;
        }
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0, 0.0);
        let (mut linear, mut perceptual, mut samples) = (0.0, 0.0, 0);
        for (x, target) in pairs.iter().step_by(3) {
            let corrected = displayed(colour(*x), &tone);
            for c in 0..3 {
                linear += (corrected[c] - target[c]).powi(2);
                perceptual += (corrected[c].max(0.0).powf(1.0 / 2.2) - target[c].max(0.0).powf(1.0 / 2.2)).powi(2);
                samples += 1;
            }
        }
        if linear.is_finite() && perceptual.is_finite() && best.as_ref().is_none_or(|b| perceptual < b.0) {
            best = Some((perceptual, linear, samples, look));
        }
    }
    let (_, after, samples, look) = best?;
    let original_tone = ToneMap::new(0.0, 0.0, 0.0);
    let before: f64 = pairs
        .iter()
        .step_by(3)
        .map(|(x, target)| {
            let original = displayed(*x, &original_tone);
            (0..3).map(|c| (original[c] - target[c]).powi(2)).sum::<f64>()
        })
        .sum();
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] camera look holdout RMS {:.5} -> {:.5} ({samples} channels)",
            (before / samples as f64).sqrt(),
            (after / samples as f64).sqrt()
        );
    }
    if samples == 0 || after >= before * MIN_IMPROVEMENT || after / samples as f64 > MAX_HOLDOUT_RMS.powi(2) {
        return None;
    }
    Some(look)
}

/// Linear Rec.2020 D65 → linear ProPhoto RGB D50, the space DNG hue/saturation tables work in.
fn to_prophoto() -> Mat3 {
    PROPHOTO.from_xyz().mul(&bradford(D65, D50)).mul(&REC2020.to_xyz())
}

/// A fitted [`CameraLook::hue_sat`] table, ready to apply to linear Rec.2020 pixels. It changes
/// hue and saturation only: luminance is restored, the camera tone curve owns it.
pub(crate) struct HueSat<'a> {
    table: &'a HsvTable,
    to: [[f32; 3]; 3],
    from: [[f32; 3]; 3],
}

impl<'a> HueSat<'a> {
    pub fn new(table: &'a HsvTable) -> Option<HueSat<'a>> {
        let to = to_prophoto();
        Some(HueSat { table, to: to.to_f32(), from: to.inverse()?.to_f32() })
    }

    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mul = |m: &[[f32; 3]; 3], v: [f32; 3]| -> [f32; 3] { std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2]) };
        let out = mul(&self.from, self.table.apply(mul(&self.to, rgb)));
        let (before, after) = (luminance_2020(rgb), luminance_2020(out));
        if before > 0.0 && after > 0.0 && out.iter().all(|v| v.is_finite()) { out.map(|v| v * before / after) } else { rgb }
    }
}

/// HSV hue (degrees) and saturation; `None` for black or non-finite colours.
fn hue_saturation(p: [f64; 3]) -> Option<(f64, f64)> {
    let max = p[0].max(p[1]).max(p[2]);
    let min = p[0].min(p[1]).min(p[2]);
    if !(max.is_finite() && min.is_finite()) || max <= 0.0 {
        return None;
    }
    let d = max - min;
    if d <= 0.0 {
        return Some((0.0, 0.0));
    }
    let h = if max == p[0] {
        ((p[1] - p[2]) / d).rem_euclid(6.0)
    } else if max == p[1] {
        (p[2] - p[0]) / d + 2.0
    } else {
        (p[0] - p[1]) / d + 4.0
    };
    Some((h * 60.0, d / max))
}

/// Table resolution: 5° hue steps (a coarser grid blurred the lime shirt into neighbouring browns
/// that want the opposite shift), saturation 0, 0.25 … 1. Value is not an axis: tone is fitted separately.
const TABLE_HUES: usize = 72;
const TABLE_SATS: usize = 5;
/// Value levels (sRGB-encoded, as DNG tables with encoding 1): cameras turn dark yellows toward
/// orange but bright yellow-greens toward green, which one shift per hue averages away.
const TABLE_VALS: usize = 5;
/// Kernel widths around each table node (hue in degrees, saturation, encoded value).
const KERNEL_HUE: f64 = 2.5;
const KERNEL_SAT: f64 = 0.15;
const KERNEL_VAL: f64 = 0.12;
/// Kernel weight at which a node keeps half of its estimate; sparse nodes shrink to identity.
const SHRINK_WEIGHT: f64 = 5.0;

/// Fit hue shifts and saturation scales of the training pairs left after `matrix`, per (hue,
/// saturation) node, kernel-weighted and shrunk toward identity where the photo has few samples.
/// Saturation 0 stays identity, so neutrals are never tinted.
fn fit_hue_sat(pairs: &[([f64; 3], [f64; 3])], matrix: &Mat3) -> Option<HsvTable> {
    let to = to_prophoto();
    let samples: Vec<(f64, f64, f64, f64, f64)> = pairs
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 3 != 0)
        .filter_map(|(_, (x, y))| {
            let p = to.apply(matrix.apply(*x));
            let (hp, sp) = hue_saturation(p)?;
            let value = f64::from(lightcraft_color::transfer::linear_to_srgb(p[0].max(p[1]).max(p[2]).min(1.0) as f32));
            let (ht, st) = hue_saturation(to.apply(*y))?;
            // hue is meaningless near neutral
            if sp < 0.08 || st < 0.02 {
                return None;
            }
            let shift = ((ht - hp + 180.0).rem_euclid(360.0) - 180.0).clamp(-30.0, 30.0);
            let log_scale = (st / sp).ln().clamp(-0.7, 0.7);
            Some((hp, sp.min(1.0), value, shift, log_scale))
        })
        .collect();
    if samples.len() < 64 {
        return None;
    }
    // Samples by table hue step: a node only looks at hues within its kernel's reach.
    let step = 360.0 / TABLE_HUES as f64;
    let mut by_hue: Vec<Vec<(f64, f64, f64, f64, f64)>> = vec![Vec::new(); TABLE_HUES];
    for sample in samples {
        if let Some(bin) = by_hue.get_mut(((sample.0.rem_euclid(360.0) / step) as usize).min(TABLE_HUES - 1)) {
            bin.push(sample);
        }
    }
    let reach = (4.0 * KERNEL_HUE / step).ceil() as usize + 1;
    use rayon::prelude::*;
    let data: Vec<[f32; 3]> = (0..TABLE_VALS * TABLE_HUES * TABLE_SATS)
        .into_par_iter()
        .map(|index| {
            let (v, h, s) = (index / (TABLE_HUES * TABLE_SATS), index / TABLE_SATS % TABLE_HUES, index % TABLE_SATS);
            if s == 0 {
                return [0.0, 1.0, 1.0];
            }
            let (val, hue, sat) = (v as f64 / (TABLE_VALS - 1) as f64, h as f64 * step, s as f64 / (TABLE_SATS - 1) as f64);
            let (mut weight, mut shift, mut log_scale) = (0.0, 0.0, 0.0);
            for offset in 0..=2 * reach {
                let Some(bin) = by_hue.get((h + TABLE_HUES * 2 + offset - reach) % TABLE_HUES) else { continue };
                for &(hp, sp, vp, dh, ls) in bin {
                    let dhue = (hp - hue + 180.0).rem_euclid(360.0) - 180.0;
                    if dhue.abs() > 4.0 * KERNEL_HUE {
                        continue;
                    }
                    let d2 = (dhue / KERNEL_HUE).powi(2) + ((sp - sat) / KERNEL_SAT).powi(2) + ((vp - val) / KERNEL_VAL).powi(2);
                    let k = (-0.5 * d2).exp() * sp;
                    weight += k;
                    shift += k * dh;
                    log_scale += k * ls;
                }
            }
            if weight <= 0.0 {
                return [0.0, 1.0, 1.0];
            }
            let shrink = 1.0 / (weight + SHRINK_WEIGHT);
            [(shift * shrink) as f32, (log_scale * shrink).exp() as f32, 1.0]
        })
        .collect();
    data.iter().all(|e| e.iter().all(|v| v.is_finite())).then_some(HsvTable {
        hue_divisions: TABLE_HUES,
        sat_divisions: TABLE_SATS,
        val_divisions: TABLE_VALS,
        data,
        srgb_value: true,
    })
}

/// Kernel width of the chroma curve's nodes (display luminance) and the weight at which a node
/// keeps half of its estimate.
const CHROMA_KERNEL: f64 = 0.08;
const CHROMA_SHRINK: f64 = 2.0;

/// The look's tone curve with a chroma-by-display-luminance curve fitted to `pairs` (two thirds
/// train, one third held out), when that lowers the held-out error. Colourfulness is compared as
/// the distance from neutral of luminance-normalised RGB.
fn fit_chroma(pairs: &[([f64; 3], [f64; 3])], look: &CameraLook) -> Option<CameraTone> {
    let correction = look.hue_sat.as_ref().and_then(HueSat::new);
    let scene = |x: &[f64; 3]| {
        let p = look.matrix.apply(*x);
        correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from))
    };
    let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0, 0.0);
    let predict = |x: &[f64; 3]| displayed(scene(x), &tone);
    let chroma = |p: [f64; 3]| {
        let y = luma(p);
        (y > 0.0).then(|| p.iter().map(|v| (v / y - 1.0).powi(2)).sum::<f64>().sqrt())
    };
    let samples: Vec<(f64, f64, f64)> = pairs
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 3 != 0)
        .filter_map(|(_, (x, y))| {
            let p = predict(x);
            let (cp, cy) = (chroma(p)?, chroma(*y)?);
            (cp > 0.05).then(|| (luma(p), cp, (cy.max(1e-3) / cp).ln().clamp(0.05f64.ln(), 2.5f64.ln())))
        })
        .collect();
    if samples.len() < 64 {
        return None;
    }
    let mut curve = [1.0f32; lightcraft_pipeline::tone::CHROMA_N];
    for (j, node) in curve.iter_mut().enumerate() {
        let at = j as f64 / (lightcraft_pipeline::tone::CHROMA_N - 1) as f64;
        let (mut weight, mut sum) = (0.0, 0.0);
        for &(o, cp, log_ratio) in &samples {
            let k = (-0.5 * ((o - at) / CHROMA_KERNEL).powi(2)).exp() * cp;
            weight += k;
            sum += k * log_ratio;
        }
        *node = (sum / (weight + CHROMA_SHRINK)).exp() as f32;
    }
    let fitted = look.tone.with_chroma(curve)?;
    let with = ToneMap::camera(&fitted, 0.0, 0.0, 0.0, 0.0);
    let error = |map: &ToneMap| -> f64 {
        pairs
            .iter()
            .step_by(3)
            .map(|(x, y)| {
                let p = displayed(scene(x), map);
                (0..3).map(|c| (p[c] - y[c]).powi(2)).sum::<f64>()
            })
            .sum()
    };
    let (before, after) = (error(&tone), error(&with));
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] ARW chroma curve {curve:?}: held-out error {before:.4} -> {after:.4}");
    }
    (after.is_finite() && after < before).then_some(fitted)
}

fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied()
}

fn fit_tone(mut pairs: Vec<(f64, f64)>) -> Option<CameraTone> {
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
    if knots[31][0] < knots[0][0] * 1.5 {
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
    CameraTone::new(&knots)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separates_nonlinear_tone_from_colour_and_keeps_sensor_headroom() {
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let mut sensor = Rgb32f::new(64, 64);
        let mut reference = sensor.clone();
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            let ev = 0.05 + (i % 31) as f32 * 0.017;
            *src = [ev * (0.8 + (i % 11) as f32 * 0.025), ev, ev * (0.8 + (i % 17) as f32 * 0.014)];
            let p = known.apply_f32(*src);
            let y = luminance_2020(p);
            *dst = p.map(|v| v * (1.0 - (-2.5 * y).exp()) / y);
        }
        let original = sensor.clone();
        let fit = fit_pairs(&sensor, &reference).unwrap();
        assert_eq!(sensor.data, original.data);
        let tone = ToneMap::camera(&fit.tone, 0.0, 0.0, 0.0, 0.0);
        let error: f64 = sensor
            .data
            .iter()
            .zip(&reference.data)
            .map(|(x, y)| {
                let p = displayed(fit.matrix.apply(x.map(f64::from)), &tone);
                (0..3).map(|c| (p[c] - f64::from(y[c])).powi(2)).sum::<f64>() / 3.0
            })
            .sum::<f64>()
            / sensor.data.len() as f64;
        assert!(error.sqrt() < 0.025, "{error}");
        // The colour transform is homogeneous; tone mapping happens only after exposure.
        let p = fit.matrix.apply([2.0, 2.0, 2.0]);
        assert!(luma(p) > 1.0);
        assert!(tone.apply(0.2) < tone.apply(0.4));
    }
    #[test]
    fn accepts_a_much_better_fit_despite_local_camera_processing() {
        // The camera JPEG departs from any global matrix + curve (local tone, vignetting): ±0.12
        // per-pixel deviations, ~0.07 RMS. The fit is still far closer than the fallback.
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let mut sensor = Rgb32f::new(64, 64);
        let mut reference = sensor.clone();
        let mut seed = 0x2545_f491_u32;
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            let ev = 0.05 + (i % 31) as f32 * 0.017;
            *src = [ev * (0.8 + (i % 11) as f32 * 0.025), ev, ev * (0.8 + (i % 17) as f32 * 0.014)];
            let p = known.apply_f32(*src);
            let y = luminance_2020(p);
            *dst = p.map(|v| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = (seed % 2001) as f32 / 1000.0 - 1.0;
                (v * (1.0 - (-2.5 * y).exp()) / y + 0.12 * noise).clamp(0.005, 0.97)
            });
        }
        assert!(fit_pairs(&sensor, &reference).is_some());
    }

    /// A camera that rotates saturated yellow-greens toward green (like Sony's "Standard" on a lime
    /// shirt) can't be followed by a matrix alone: the fitted hue/saturation table must close most
    /// of the gap, keep neutrals neutral and keep luminance.
    #[test]
    fn hue_table_follows_a_hue_dependent_camera_rendering() {
        let to = to_prophoto();
        let from = to.inverse().unwrap();
        let mut sensor = Rgb32f::new(96, 64);
        let mut reference = sensor.clone();
        let mut lime = Vec::new();
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            let ev = 0.05 + (i % 29) as f32 * 0.02;
            // mostly greys and mild colours, plus a patch of saturated yellow-green
            *src = if i % 7 == 0 {
                [ev * 0.75, ev, ev * 0.2]
            } else {
                [ev * (0.85 + (i % 11) as f32 * 0.03), ev, ev * (0.85 + (i % 13) as f32 * 0.025)]
            };
            let (h, s) = hue_saturation(to.apply(src.map(f64::from))).unwrap();
            // the camera turns hues in 50°..110° (ProPhoto) by up to +15° and boosts their saturation
            let k = (1.0 - ((h - 80.0) / 30.0).powi(2)).max(0.0) * s.min(1.0);
            let mut p = to.apply(src.map(f64::from)).map(|v| v as f32);
            let table = HsvTable {
                hue_divisions: 1,
                sat_divisions: 2,
                val_divisions: 1,
                data: vec![[0.0, 1.0, 1.0], [15.0 * k as f32, 1.0 + 0.3 * k as f32, 1.0]],
                srgb_value: false,
            };
            p = table.apply(p);
            let p = from.apply(p.map(f64::from)).map(|v| v as f32);
            let scale = luminance_2020(*src) / luminance_2020(p);
            let p = p.map(|v| v * scale);
            let y = luminance_2020(p);
            *dst = p.map(|v| v * (1.0 - (-2.5 * y).exp()) / y);
            if i % 7 == 0 {
                lime.push(i);
            }
        }
        let fit = fit_pairs(&sensor, &reference).unwrap();
        let table = fit.hue_sat.as_ref().expect("a hue/saturation table is fitted");
        let hue_sat = HueSat::new(table).unwrap();
        let hue_error = |with_table: bool| {
            lime.iter()
                .map(|&i| {
                    let p = fit.matrix.apply(sensor.data[i].map(f64::from)).map(|v| v as f32);
                    let p = if with_table { hue_sat.apply(p) } else { p };
                    let (h, _) = hue_saturation(to.apply(p.map(f64::from))).unwrap();
                    let (t, _) = hue_saturation(to.apply(reference.data[i].map(f64::from))).unwrap();
                    ((h - t + 180.0).rem_euclid(360.0) - 180.0).abs()
                })
                .sum::<f64>()
                / lime.len() as f64
        };
        let (before, after) = (hue_error(false), hue_error(true));
        assert!(after < before * 0.5, "lime hue error {before:.2}° -> {after:.2}°");
        // neutrals pass through and luminance is kept
        let grey = [0.3, 0.3, 0.3];
        assert!(hue_sat.apply(grey).iter().all(|v| (v - 0.3).abs() < 1e-4), "{:?}", hue_sat.apply(grey));
        let green = [0.2, 0.4, 0.05];
        assert!((luminance_2020(hue_sat.apply(green)) - luminance_2020(green)).abs() < 1e-5);
    }

    /// A camera that saturates shadows and bleaches highlights toward white (a per-channel curve),
    /// which a luminance tone curve can't: the fitted chroma curve must follow it, so bright
    /// warm-tinted rock renders white instead of cream.
    #[test]
    fn chroma_curve_follows_highlight_bleaching() {
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let camera_chroma = |o: f32| 1.3 - 1.1 * o;
        let mut sensor = Rgb32f::new(96, 64);
        let mut reference = sensor.clone();
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            let ev = 0.02 + (i % 37) as f32 * 0.03;
            *src = [ev * (0.75 + (i % 11) as f32 * 0.05), ev, ev * (0.7 + (i % 13) as f32 * 0.05)];
            let p = known.apply_f32(*src);
            let y = luminance_2020(p);
            let o = 1.0 - (-2.5 * y).exp();
            let k = camera_chroma(o);
            *dst = p.map(|v| (o + (v * o / y - o) * k).clamp(0.0, 0.999));
        }
        let fit = fit_pairs(&sensor, &reference).unwrap();
        // relative to the matrix, which already carries the average colourfulness
        let chroma = fit.tone.chroma();
        assert!(chroma[6] < 0.5 * chroma[1], "highlights bleach relative to shadows: {chroma:?}");
        // and the rendered highlights land on the camera's, far closer than without the curve
        let with = ToneMap::camera(&fit.tone, 0.0, 0.0, 0.0, 0.0);
        let without = ToneMap::camera(&fit.tone.with_chroma([1.0; lightcraft_pipeline::tone::CHROMA_N]).unwrap(), 0.0, 0.0, 0.0, 0.0);
        let highlight_error = |map: &ToneMap| -> f64 {
            sensor
                .data
                .iter()
                .zip(&reference.data)
                .filter(|(_, y)| luminance_2020(**y) > 0.7)
                .map(|(x, y)| {
                    let p = displayed(fit.matrix.apply(x.map(f64::from)), map);
                    (0..3).map(|c| (p[c] - f64::from(y[c])).powi(2)).sum::<f64>()
                })
                .sum()
        };
        let (a, b) = (highlight_error(&without), highlight_error(&with));
        assert!(b < 0.5 * a, "highlight error {a:.4} -> {b:.4}");
    }

    /// Pooled pairs of several "photos" recover the camera's matrix, and a photo given that
    /// colour model (a camera profile) keeps it, fitting only its own tone.
    #[test]
    fn profile_colour_is_pooled_and_then_used_as_given() {
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let photo = |seed: usize, strength: f32| {
            let mut sensor = Rgb32f::new(64, 48);
            let mut reference = sensor.clone();
            for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
                let i = i + seed * 7;
                let ev = 0.04 + (i % 23) as f32 * 0.02;
                *src = [ev * (0.7 + (i % 11) as f32 * 0.05), ev, ev * (0.7 + (i % 13) as f32 * 0.05)];
                let p = known.apply_f32(*src);
                let y = luminance_2020(p);
                // each photo has its own tone (DRO, picture style)
                *dst = p.map(|v| v * (1.0 - (-strength * y).exp()) / y);
            }
            (sensor, reference)
        };
        let mut pool = Vec::new();
        for seed in 0..4 {
            let (sensor, reference) = photo(seed, 2.0 + seed as f32 * 0.5);
            pool.extend(collect_pairs(&sensor, &reference).unwrap().0);
        }
        let (matrix, _) = fit_profile(&pool).unwrap();
        // the camera's colours (luminance-normalised: the tone curve sets brightness)
        for x in [[0.8, 1.0, 0.75], [1.1, 1.0, 0.8], [0.75, 1.0, 1.2], [1.0; 3]] {
            let (a, b) = (matrix.apply(x), known.apply(x));
            let (la, lb) = (luma(a), luma(b));
            assert!((0..3).all(|c| (a[c] / la - b[c] / lb).abs() < 0.01), "{x:?}: {a:?} vs {b:?}");
        }
        let (sensor, reference) = photo(9, 3.5);
        let look = fit_pairs_with(&sensor, &reference, Some((matrix, None))).unwrap();
        assert_eq!(look.matrix, matrix);
        assert!(look.hue_sat.is_none());
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0, 0.0);
        let error = sensor
            .data
            .iter()
            .zip(&reference.data)
            .map(|(x, y)| {
                let p = displayed(look.matrix.apply(x.map(f64::from)), &tone);
                (0..3).map(|c| (p[c] - f64::from(y[c])).powi(2)).sum::<f64>() / 3.0
            })
            .sum::<f64>()
            / sensor.data.len() as f64;
        assert!(error.sqrt() < 0.02, "this photo's own tone is followed: RMS {}", error.sqrt());
    }

    #[test]
    fn hue_sat_passes_black_and_non_finite_pixels_through() {
        let table = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[10.0, 1.5, 1.0]; 8], srgb_value: false };
        let hue_sat = HueSat::new(&table).unwrap();
        assert_eq!(hue_sat.apply([0.0; 3]), [0.0; 3]);
        let nan = hue_sat.apply([f32::NAN, 0.2, 0.1]);
        assert!(nan[0].is_nan() && nan[1] == 0.2 && nan[2] == 0.1);
        assert!(fit_hue_sat(&[], &Mat3::IDENTITY).is_none(), "too few samples");
    }

    #[test]
    fn sony_and_nikon_raws_get_a_file_local_look() {
        assert!([RawFormat::Arw, RawFormat::Nef, RawFormat::Nrw].into_iter().all(file_local_look));
        assert!(![RawFormat::Dng, RawFormat::Cr2, RawFormat::Raf].into_iter().any(file_local_look));
    }

    /// A public D7500 NEF (skipped without the corpus): its look is fitted to its own JPEG and white
    /// balance is relative to the as-shot look.
    #[test]
    fn corpus_nef_gets_a_camera_look() {
        let path = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw/nef-nikon-d7500-lossless14.nef");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {} absent", path.display());
            return;
        };
        let (_, info) = crate::files::load_bytes(&bytes, 400).unwrap();
        assert!(info.camera_tone.is_some(), "no camera look fitted");
        assert!(info.relative_wb && info.as_shot_temp == 6500.0 && info.as_shot_tint == 0.0);
    }

    #[test]
    fn rejects_monochrome_invalid_and_unrelated_previews() {
        let mut sensor = Rgb32f::new(32, 32);
        let mut reference = sensor.clone();
        for (i, p) in sensor.data.iter_mut().enumerate() {
            *p = [0.1 + (i % 13) as f32 * 0.02, 0.15, 0.1];
        }
        reference.data.fill([0.2; 3]);
        assert!(fit_pairs(&sensor, &reference).is_none());
        reference.data.fill([f32::NAN; 3]);
        assert!(fit_pairs(&sensor, &reference).is_none());
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            *src = [0.04 + (i % 11) as f32 * 0.02, 0.05 + (i % 17) as f32 * 0.01, 0.03 + (i % 23) as f32 * 0.01];
            *dst = [0.05 + (i % 7) as f32 * 0.07, 0.05 + (i % 19) as f32 * 0.02, 0.05 + (i % 29) as f32 * 0.01];
        }
        assert!(fit_pairs(&sensor, &reference).is_none());
        sensor.data.fill([0.1, 0.15, 0.12]);
        assert!(fit_pairs(&sensor, &reference).is_none());
        reference.data.truncate(8);
        assert!(fit_pairs(&sensor, &reference).is_none());
    }
}
