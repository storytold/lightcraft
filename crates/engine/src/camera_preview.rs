//! Estimate the starting look of a raw without a camera colour matrix (Sony ARW, Nikon NEF, Panasonic
//! RW2, Fujifilm RAF, Canon CR2/CR3, Pentax PEF, Samsung SRW, Olympus ORF) from its own JPEG. Colour and luminance are fitted separately; the JPEG supplies correspondences only,
//! never output pixels or a replacement for RAW editing.
//! A global matrix can't follow the camera's hue-dependent rendering (the best matrix rendered a
//! lime shirt olive that the camera kept lime): a hue/saturation table fitted to the residuals
//! (applied like a DNG `ProfileHueSatMap`) corrects that when it also improves the held-out pixels.
use lightcraft_color::{D50, D65, Mat3, PROPHOTO, REC2020, bradford, luminance_2020};
use lightcraft_pipeline::tone::{CameraTone, ToneMap};
use lightcraft_raster::{
    Rgb32f,
    resample::{Filter, fit, resize},
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
    matches!(
        format,
        RawFormat::Arw
            | RawFormat::Nef
            | RawFormat::Nrw
            | RawFormat::Rw2
            | RawFormat::Raf
            | RawFormat::Cr3
            | RawFormat::Cr2
            | RawFormat::Pef
            | RawFormat::Srw
            | RawFormat::Orf
    )
}

pub(crate) fn fit_preview(raw: &RawImage, bytes: &[u8], transform: &CameraTransform) -> Option<CameraLook> {
    if !transform.matrix_is_fallback || !file_local_look(raw.format) {
        return None;
    }
    let (sensor, reference, clipped) = proxies(raw, bytes, transform, PROXY)?;
    // A camera profile pooled from many photos knows colours this photo shows too little of;
    // try its colour first and fit tone/chroma per photo (picture styles vary).
    let profile = raw.metadata.model.as_deref().and_then(crate::camera_profiles::get);
    let colour = profile.as_ref().and_then(|p| Some((p.matrix().mul(&transform.matrix.inverse()?), p.hue_sat.clone())));
    let look = fit_look(&sensor, &reference, &clipped, colour)?;
    // Sony's Dynamic Range Optimizer (on by default) brightens the camera JPEG's darker regions,
    // not the raw: keep the colour fitted to the JPEG, take the tone curve without DRO.
    let dro = lightcraft_raw::embedded_preview_dynamic_range_optimized(bytes) == Some(true);
    let look = if dro { with_dro_off_tone(look) } else { look };
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] {:?} camera look: {:?}, {:?}, hue/sat table {}, camera profile available {}, DRO tone replaced {dro}",
            raw.format,
            look.matrix.0,
            look.tone,
            look.hue_sat.is_some(),
            profile.is_some()
        );
    }
    Some(look)
}

/// Sensor level (normalised, white = 1) at or above which a raw channel counts as clipped, as in
/// the render's highlight reconstruction.
const SENSOR_CLIP: f32 = 0.99;

/// The photo's look, refitted without the proxy pixels whose sensor values are clipped.
///
/// A clipped raw pixel no longer records the scene's colour: with one channel held at the clip
/// level while the others keep rising, a white overcast sky comes out magenta or blue through the
/// camera matrix, while the camera JPEG shows it white. As correspondences such pixels teach the
/// colour matrix to pull colours toward grey, and the chroma curve to remove colour at their
/// display luminance (on a Z 6II forest under a clipped sky the curve cut colourfulness above
/// display luminance 0.4 to 13–19%, and every one of its samples there was clipped). The render
/// rebuilds clipped highlights separately (`lightcraft_raw::highlight`), so the look is fitted on
/// what the sensor measured.
///
/// The search on all pixels decides, as before, whether the photo gets a look and which attempt
/// of [`search_ordered`] gives it; only that attempt is refitted without the clipped pixels, and
/// when the refit misses the acceptance gates the look on all pixels is kept. So no photo loses
/// its look and none switches attempt (a refit that just clears the gate on all pixels must not
/// replace a look fitted away from edges). A photo that had no look gets one when the search
/// without the clipped pixels finds it, and otherwise the [`fit_partial`] look when that is clearly
/// closer to the camera JPEG than the neutral fallback.
fn fit_look(sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool], colour: Option<(Mat3, Option<HsvTable>)>) -> Option<CameraLook> {
    let unclipped = without_clipped(sensor, clipped);
    let Some((look, attempt)) = search_ordered(sensor, reference, colour.clone(), TONE_FITS) else {
        return unclipped
            .as_ref()
            .and_then(|u| search_ordered(u, reference, colour, TONE_FITS))
            .map(|(look, _)| look)
            .or_else(|| fit_partial(unclipped.as_ref().unwrap_or(sensor), reference));
    };
    let Some(unclipped) = unclipped else { return Some(look) };
    let refit = fit_attempt(&unclipped, reference, &colour, attempt);
    if refit.is_none() && lightcraft_pipeline::profiling() {
        eprintln!("[profile] camera look: no fit without the clipped pixels on the accepted attempt, keeping the fit on all pixels");
    }
    Some(refit.unwrap_or(look))
}

/// `sensor` with its clipped pixels set to NaN (left out of every training pair), or `None` when
/// none is clipped.
fn without_clipped(sensor: &Rgb32f, clipped: &[bool]) -> Option<Rgb32f> {
    if clipped.len() != sensor.data.len() || !clipped.iter().any(|c| *c) {
        return None;
    }
    let mut out = sensor.clone();
    for (p, c) in out.data.iter_mut().zip(clipped) {
        if *c {
            *p = [f32::NAN; 3];
        }
    }
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] camera look: {} of {} proxy pixels have a clipped sensor channel",
            clipped.iter().filter(|c| **c).count(),
            clipped.len()
        );
    }
    Some(out)
}

/// 1 where any channel of a camera-RGB pixel is at or above [`SENSOR_CLIP`], else 0.
fn clip_mask(camera: &Rgb32f) -> Rgb32f {
    let mut mask = camera.clone();
    mask.map_in_place(|p| if p.iter().any(|v| *v >= SENSOR_CLIP) { [1.0; 3] } else { [0.0; 3] });
    mask
}

/// Sony's global tone curve with the Dynamic Range Optimizer off (Standard creative style), pooled
/// from 12 CC0 raw.pixls.us ARWs of 10 interchangeable-lens bodies and their camera JPEGs; the file
/// lists them and how the curve was derived (`assets/ATTRIBUTION.md`). Pooled the same way from 55
/// private ILCE-7CR photos with DRO off, the curve differs by at most 1.2 L* between scene luminance
/// 0.01 and 0.5. Issue #244, `docs/camera-preview-colour.md`.
const SONY_DRO_OFF_TONE: &str = include_str!("../../../assets/camera-tone/sony-dro-off.json");

/// [`SONY_DRO_OFF_TONE`]'s curve (`None` only if the built-in file were invalid; a test checks it).
fn sony_dro_off_tone() -> Option<CameraTone> {
    #[derive(serde::Deserialize)]
    struct File {
        knots: [[f32; 2]; 32],
    }
    static TONE: std::sync::OnceLock<Option<CameraTone>> = std::sync::OnceLock::new();
    *TONE.get_or_init(|| serde_json::from_str::<File>(SONY_DRO_OFF_TONE).ok().and_then(|f| CameraTone::new(f.knots)))
}

/// `look`, fitted to a camera JPEG brightened by Sony's Dynamic Range Optimizer, with its tone
/// curve lowered to Sony's without DRO ([`SONY_DRO_OFF_TONE`]) wherever that is darker. DRO only
/// brightens, so the fitted curve bounds the curve without it from above, everywhere: where Sony's
/// curve is brighter than the photo's own (the 1″ compacts map raw values darker), the photo keeps
/// its own. Its colour (matrix, hue/saturation table and chroma curve) stays as fitted.
fn with_dro_off_tone(look: CameraLook) -> CameraLook {
    let Some(dro_off) = sony_dro_off_tone() else { return look };
    let own = look.tone.knots();
    let mut knots = own.map(|[x, y]| [x, y.min(dro_off.apply(x))]);
    // Past its last knot a curve goes on as a shoulder whose rate comes from its last two knots
    // (`CameraTone::apply`): lowering the second-last knot more than the last steepens it, and the
    // extended highlights would end up above the fitted curve. Raise the second-last knot (never
    // above the fitted curve) until the rate is at most the fitted curve's.
    let ([a, b], last) = ([own[30], own[31]], knots[31]);
    let rate = ((b[1] - a[1]) / (b[0] - a[0])).clamp(0.1, 16.0) / (1.0 - b[1]).max(0.01);
    let floor = last[1] - rate * (1.0 - last[1]).max(0.01) * (b[0] - a[0]);
    knots[30][1] = knots[30][1].max(floor).min(last[1]);
    let Some(tone) = CameraTone::new(knots).and_then(|tone| tone.with_chroma(*look.tone.chroma())) else { return look };
    if !at_most(&tone, &look.tone) {
        return look;
    }
    CameraLook { tone, ..look }
}

/// Whether curve `a` is nowhere brighter than `b` over the scene luminances the finish stage's tone
/// table covers ([`ToneMap::camera`]).
fn at_most(a: &CameraTone, b: &CameraTone) -> bool {
    use lightcraft_pipeline::tone::{GREY, LUT_MAX_EV, LUT_MIN_EV, LUT_N};
    (0..LUT_N).all(|i| {
        let x = GREY * 2f32.powf(LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32);
        a.apply(x) <= b.apply(x) + 1e-5
    })
}

/// Same-size proxies of the sensor (white-balanced, baseline exposure, through `transform`'s
/// matrix: the generic camera ≈ sRGB model) and of the file's embedded camera JPEG (linear Rec.2020),
/// and per proxy pixel whether any sensor sample it covers is clipped.
fn proxies(raw: &RawImage, bytes: &[u8], transform: &CameraTransform, size: usize) -> Option<(Rgb32f, Rgb32f, Vec<bool>)> {
    let edge = (2 * size).max(384) as u32;
    let decoded = crate::files::decode_raw_preview(bytes, lightcraft_codecs::DecodeOptions { max_size: Some((edge, edge)), max_pixels: 64_000_000 })?;
    let mut reference = decoded.to_working();
    let (a, crop) = (raw.active_area, raw.crop.clipped(raw.active_area.width, raw.active_area.height));
    if crop.width == 0 || crop.height == 0 || reference.width == 0 || reference.height == 0 {
        return None;
    }
    let matches = |w: usize, h: usize, rw: usize, rh: usize| (rw as f64 / rh as f64 / (w as f64 / h as f64) - 1.0).abs() <= 0.02;
    if !matches(crop.width, crop.height, reference.width, reference.height) {
        // Panasonic previews show the whole active area while the default crop is the in-camera aspect ratio
        if !matches(a.width, a.height, reference.width, reference.height) {
            return None;
        }
        let (sx, sy) = (reference.width as f64 / a.width as f64, reference.height as f64 / a.height as f64);
        let (x, y) = ((crop.x as f64 * sx).round() as usize, (crop.y as f64 * sy).round() as usize);
        let (w, h) = ((crop.width as f64 * sx).round() as usize, (crop.height as f64 * sy).round() as usize);
        if w < 16 || h < 16 || x + w > reference.width || y + h > reference.height {
            return None;
        }
        reference = reference.into_crop(x, y, w, h);
    }
    // Fixed, bounded proxy: the selected look cannot depend on thumbnail/export resolution.
    let k = (crop.width.max(crop.height).div_ceil(edge as usize).max(2)).div_ceil(2) * 2;
    let mut sensor = sensor_proxy(raw, k, edge as usize)?;
    let mut clipped = clip_mask(&sensor);
    let gain = 2f32.powf(transform.baseline_exposure as f32);
    let to_working = |p: [f32; 3]| transform.matrix.apply_f32(std::array::from_fn(|i| p[i] * transform.wb[i] * gain));
    if raw.format == RawFormat::Cr3 {
        // Orient the camera JPEG and the sensor identically before collecting pixel correspondences.
        sensor = sensor.into_oriented(raw.orientation);
        clipped = clipped.into_oriented(raw.orientation);
        reference = reference.into_oriented(raw.orientation);
        sensor.map_in_place(to_working);
        // Canon's JPEG can be cropped differently from the sensor. Estimate only that common
        // framing from edge directions, independently of the subsequent colour fit.
        (sensor, clipped) = align_cr3_framing(sensor, clipped, &reference);
    }
    let mut sensor = fit(&sensor, size, size, Filter::Box);
    let reference = reference_at(&reference, &sensor);
    // A proxy pixel is clipped when any sample it averages is (outside the CR3 framing the mask is
    // NaN: those pixels are left out of the pairs anyway). Resized to exactly the proxy's size, like
    // the reference, so the mask pairs pixel for pixel.
    let clipped: Vec<bool> = resize(&clipped, sensor.width, sensor.height, Filter::Box).data.iter().map(|p| p[0] > 0.0).collect();
    if raw.format != RawFormat::Cr3 {
        sensor.map_in_place(to_working);
    }
    Some((sensor, reference, clipped))
}

/// The camera JPEG at exactly the sensor proxy's size, so the two pair pixel for pixel. Fitting it
/// into the proxy's box instead keeps its own aspect, which can round a pixel short: an ILCE-7RM4's
/// 1616×1080 preview, decoded at 384×257, fits a 192×128 proxy as 191×128, and `collect_pairs`
/// then rejected every file `lightcraft-cli calibrate` was given. `proxies` has already checked
/// that the aspects agree within 2 %.
fn reference_at(reference: &Rgb32f, sensor: &Rgb32f) -> Rgb32f {
    resize(reference, sensor.width, sensor.height, Filter::Box)
}

#[derive(Clone, Copy, Debug)]
struct Cr3Framing {
    scale: f32,
    /// Translation as a fraction of the oriented frame's width and height.
    offset: [f32; 2],
}

impl Cr3Framing {
    const IDENTITY: Self = Self { scale: 1.0, offset: [0.0; 2] };

    fn source(self, x: f32, y: f32, w: usize, h: usize) -> (f32, f32) {
        let (w, h) = (w as f32, h as f32);
        ((x - w * 0.5) * self.scale + w * (0.5 + self.offset[0]), (y - h * 0.5) * self.scale + h * (0.5 + self.offset[1]))
    }
}

/// Register the JPEG's remaining framing on a fixed proxy. Edge directions survive different
/// white balance and monotone camera tone; no target RGB values train this three-parameter fit.
/// A separate third of the edges must confirm a substantial improvement and a close match.
/// A weak, flat or unrelated preview leaves the sensor framing unchanged.
fn estimate_cr3_framing(sensor: &Rgb32f, reference: &Rgb32f) -> Option<Cr3Framing> {
    if sensor.width.checked_mul(sensor.height)? != sensor.data.len() || reference.width.checked_mul(reference.height)? != reference.data.len() {
        return None;
    }
    let sensor = fit(sensor, PROXY, PROXY, Filter::Box);
    let (w, h) = (sensor.width, sensor.height);
    if w < 16 || h < 16 {
        return None;
    }
    let reference = fit(reference, w, h, Filter::Box);
    if (reference.width, reference.height) != (w, h) {
        return None;
    }
    let mut gradients = Rgb32f::new(w, h);
    let mut edges = Vec::new();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let derivative = |image: &Rgb32f| {
                [
                    luminance_2020(image.data[y * w + x + 1]) - luminance_2020(image.data[y * w + x - 1]),
                    luminance_2020(image.data[(y + 1) * w + x]) - luminance_2020(image.data[(y - 1) * w + x]),
                ]
            };
            let d = derivative(&sensor);
            gradients.data[y * w + x] = [d[0], d[1], 0.0];
            let d = derivative(&reference);
            let magnitude = d[0].hypot(d[1]);
            let value = luminance_2020(reference.data[y * w + x]);
            if (0.02..0.9).contains(&value) && magnitude.is_finite() && magnitude > 0.03 {
                edges.push((x as f32 + 0.5, y as f32 + 0.5, d, magnitude, (x * 17 + y * 11) % 3 == 0));
            }
        }
    }
    if edges.len() < 192 {
        return None;
    }
    let score = |framing: Cr3Framing, held_out: bool| -> Option<f64> {
        let (mut agreement, mut weight, mut valid, mut total) = (0.0, 0.0, 0usize, 0usize);
        for &(x, y, target, magnitude, holdout) in &edges {
            if holdout != held_out {
                continue;
            }
            total += 1;
            let (sx, sy) = framing.source(x, y, w, h);
            if sx < 1.5 || sy < 1.5 || sx > w as f32 - 1.5 || sy > h as f32 - 1.5 {
                continue;
            }
            let sample = gradients.sample_bilinear(sx, sy);
            let length = sample[0].hypot(sample[1]);
            if !length.is_finite() || length <= 0.01 {
                continue;
            }
            let k = f64::from(magnitude.min(0.25));
            agreement += k * f64::from((sample[0] * target[0] + sample[1] * target[1]) / (length * magnitude));
            weight += k;
            valid += 1;
        }
        (valid >= 64 && valid * 5 >= total * 4 && weight > 0.0).then_some(agreement / weight)
    };
    let before = score(Cr3Framing::IDENTITY, false)?;
    let mut best = (before, Cr3Framing::IDENTITY);
    // At most 10% zoom and two proxy pixels of translation; a coarse search followed by one
    // local refinement prevents unbounded optimisation on the photograph's contents.
    for s in -10..=10 {
        for tx in -4..=4 {
            for ty in -4..=4 {
                let framing = Cr3Framing { scale: 1.0 + s as f32 * 0.01, offset: [tx as f32 * 0.5 / w as f32, ty as f32 * 0.5 / h as f32] };
                if let Some(value) = score(framing, false)
                    && value > best.0
                {
                    best = (value, framing);
                }
            }
        }
    }
    let coarse = best.1;
    for s in -5..=5 {
        for tx in -4..=4 {
            for ty in -4..=4 {
                let framing = Cr3Framing {
                    scale: coarse.scale + s as f32 * 0.002,
                    offset: [coarse.offset[0] + tx as f32 * 0.125 / w as f32, coarse.offset[1] + ty as f32 * 0.125 / h as f32],
                };
                if !(0.9..=1.1).contains(&framing.scale) || framing.offset[0].abs() > 2.0 / w as f32 || framing.offset[1].abs() > 2.0 / h as f32 {
                    continue;
                }
                if let Some(value) = score(framing, false)
                    && value > best.0
                {
                    best = (value, framing);
                }
            }
        }
    }
    let (held_before, held_after) = (score(Cr3Framing::IDENTITY, true)?, score(best.1, true)?);
    if best.0 < 0.9 || best.0 < before + 0.04 || held_after < 0.9 || held_after < held_before + 0.04 {
        return None;
    }
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] CR3 JPEG framing {:?}, held-out edge agreement {held_before:.4} -> {held_after:.4}", best.1);
    }
    Some(best.1)
}

/// The sensor proxy and its clip mask (same size), both moved to the camera JPEG's framing.
fn align_cr3_framing(sensor: Rgb32f, clipped: Rgb32f, reference: &Rgb32f) -> (Rgb32f, Rgb32f) {
    let Some(framing) = estimate_cr3_framing(&sensor, reference) else { return (sensor, clipped) };
    let apply = |image: &Rgb32f| {
        let (w, h) = (image.width, image.height);
        let mut aligned = Rgb32f::new(w, h);
        for (i, pixel) in aligned.data.iter_mut().enumerate() {
            let (x, y) = framing.source((i % w) as f32 + 0.5, (i / w) as f32 + 0.5, w, h);
            *pixel = if x >= 0.5 && y >= 0.5 && x <= w as f32 - 0.5 && y <= h as f32 - 0.5 { image.sample_bilinear(x, y) } else { [f32::NAN; 3] };
        }
        aligned
    };
    (apply(&sensor), apply(&clipped))
}

/// Colour training pairs of one raw for a camera profile: white-balanced camera RGB (with the
/// baseline exposure) → its camera JPEG (linear Rec.2020). `None` for formats with their own
/// colour matrices, other files or unusable previews.
pub(crate) fn profile_pairs(raw: &RawImage, bytes: &[u8]) -> Option<Vec<([f64; 3], [f64; 3])>> {
    if !file_local_look(raw.format) || lightcraft_raw::color::has_matrix(&raw.color) {
        return None;
    }
    let transform = lightcraft_raw::color::camera_transform(raw, lightcraft_raw::color::as_shot_white_xy(raw));
    let (sensor, reference, clipped) = proxies(raw, bytes, &transform, PROFILE_PROXY)?;
    let to_camera = transform.matrix.inverse()?;
    // clipped sensor pixels don't record the scene's colour (see `fit_look`)
    let sensor = without_clipped(&sensor, &clipped).unwrap_or(sensor);
    let (pairs, _) = collect_pairs(&sensor, &reference, 0.05, None)?;
    Some(pairs.into_iter().map(|(x, y)| (to_camera.apply(x), y)).collect())
}

/// A camera profile's colour model (matrix from white-balanced camera RGB, hue/saturation table)
/// fitted to pairs pooled from many photos.
pub(crate) fn fit_profile(pairs: &[([f64; 3], [f64; 3])]) -> Option<(Mat3, Option<HsvTable>)> {
    let matrix = fit_matrix(pairs)?;
    Some((matrix, fit_hue_sat(pairs, &matrix)))
}

fn sensor_proxy(raw: &RawImage, k: usize, edge: usize) -> Option<Rgb32f> {
    Some(match raw.develop_binned(k, SENSOR_CLIP).ok()? {
        Some(sensor) => sensor,
        None if raw.cpp == 3 && raw.cfa.is_none() => fit(&raw.develop(lightcraft_raw::Method::Bilinear).ok()?, edge, edge, Filter::Box),
        None => return None,
    })
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

/// Largest luminance ratio within the 3×3 neighbourhood of pixel `i` (1 on flat areas, large on edges).
fn local_contrast(image: &Rgb32f, i: usize) -> f32 {
    let (w, h) = (image.width, image.height);
    if w == 0 {
        return f32::INFINITY;
    }
    let (x, y) = (i % w, i / w);
    let (mut lo, mut hi) = (f32::INFINITY, 0f32);
    for row in y.saturating_sub(1)..(y + 2).min(h) {
        for col in x.saturating_sub(1)..(x + 2).min(w) {
            let l = image.data.get(row * w + col).map_or(0.0, |p| luminance_2020(*p));
            lo = lo.min(l);
            hi = hi.max(l);
        }
    }
    if hi.is_finite() { hi / lo.max(1e-4) } else { f32::INFINITY }
}

/// Training pairs (sensor → JPEG, unclipped midtones) and the wider set including highlights
/// (for the chroma curve); `None` when too few, or the photo has too little colour. With
/// `edge_limit`, pixels whose 3×3 neighbourhood in either proxy spans a larger luminance ratio
/// are left out: there a small geometric mismatch between the raw and its JPEG pairs unrelated
/// colours (see [`EDGE_CONTRAST`]).
type Pairs = Vec<([f64; 3], [f64; 3])>;
fn collect_pairs(sensor: &Rgb32f, reference: &Rgb32f, min_chroma: f32, edge_limit: Option<f32>) -> Option<(Pairs, Pairs)> {
    if (sensor.width, sensor.height) != (reference.width, reference.height) || sensor.data.len() != reference.data.len() {
        return None;
    }
    let mut pairs = Vec::new();
    // Highlights too (camera JPEGs bleach colours toward white there), for the chroma curve only.
    let mut bright = Vec::new();
    let mut colour = 0;
    for (i, (input, output)) in sensor.data.iter().zip(&reference.data).enumerate() {
        if edge_limit.is_some_and(|limit| local_contrast(sensor, i) > limit || local_contrast(reference, i) > limit) {
            continue;
        }
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
        colour += usize::from(max - min > min_chroma);
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

/// Neighbourhood luminance ratio above which a pixel counts as an edge for the second attempt
/// of [`search_ordered`]. Camera JPEGs are often lens-corrected (Sony "Distortion Comp.: Auto",
/// compacts and kit zooms): on a 51 mm ILCE-7RM2 shot of a glass façade the JPEG is up to 15 px
/// of 1440 (about one proxy pixel) off the raw, and the fit failed the gate on the mismatched
/// window frames alone (held-out RMS 0.111; 0.059 for the same shot with distortion correction
/// off). Away from edges: 0.049. Issue #232.
const EDGE_CONTRAST: f32 = 3.0;

/// The look on all pixels (the search of [`search_ordered`] with [`TONE_FITS`]), as before
/// clipped pixels were left out.
#[cfg(test)]
fn fit_pairs_with(sensor: &Rgb32f, reference: &Rgb32f, colour: Option<(Mat3, Option<HsvTable>)>) -> Option<CameraLook> {
    fit_pairs_ordered(sensor, reference, colour, TONE_FITS)
}

/// The look from [`search_ordered`] without its attempt.
#[cfg(test)]
fn fit_pairs_ordered(sensor: &Rgb32f, reference: &Rgb32f, colour: Option<(Mat3, Option<HsvTable>)>, order: &[ToneFit]) -> Option<CameraLook> {
    search_ordered(sensor, reference, colour, order).map(|(look, _)| look)
}

/// The tone fits the search tries, in order: the first whose look passes the acceptance gates is
/// used. Quantile matching comes first because it cannot produce flat steps; the
/// conditional-median fit follows because on a few photos (compacts, early Micro Four Thirds) its
/// slightly lower held-out error is what clears the gates, and a photo must not lose its look to
/// the change of fit.
const TONE_FITS: &[ToneFit] = &[ToneFit::Quantile, ToneFit::ConditionalMedian];

/// One attempt of the search: tone fit, the camera profile's colour or the photo's own, all pixels
/// or only those away from edges.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Attempt {
    tone: ToneFit,
    profile: bool,
    away_from_edges: bool,
}

/// The photo's look and the attempt that produced it: its own matrix (with and without a
/// hue/saturation table), or the given colour model (a camera profile's, in the sensor proxy's
/// space), each completed with a tone and chroma curve fitted to this photo. Per tone fit in
/// `order`: the camera profile's colour (when there is one), then the photo's own, as a different
/// picture style can make a camera profile fail the gates; each fitted on all pixels and, when that
/// fails the gates, once more on the pixels away from edges (same gates): there colour pairs stay
/// valid when the camera JPEG's geometry differs slightly from the raw's (issue #232). The whole
/// search runs with one tone fit before the next is tried, so the conditional-median fallback
/// accepts exactly what the previous releases accepted.
fn search_ordered(sensor: &Rgb32f, reference: &Rgb32f, colour: Option<(Mat3, Option<HsvTable>)>, order: &[ToneFit]) -> Option<(CameraLook, Attempt)> {
    let profiles: &[bool] = if colour.is_some() { &[true, false] } else { &[false] };
    order.iter().find_map(|&tone| {
        profiles.iter().find_map(|&profile| {
            [false, true].into_iter().find_map(|away_from_edges| {
                let attempt = Attempt { tone, profile, away_from_edges };
                fit_attempt(sensor, reference, &colour, attempt).map(|look| (look, attempt))
            })
        })
    })
}

fn fit_attempt(sensor: &Rgb32f, reference: &Rgb32f, colour: &Option<(Mat3, Option<HsvTable>)>, attempt: Attempt) -> Option<CameraLook> {
    let colour = if attempt.profile { colour.clone() } else { None };
    fit_pairs_on(sensor, reference, colour, attempt.away_from_edges.then_some(EDGE_CONTRAST), attempt.tone)
}

fn fit_pairs_on(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: Option<(Mat3, Option<HsvTable>)>,
    edge_limit: Option<f32>,
    tone_fit: ToneFit,
) -> Option<CameraLook> {
    // A known colour model needs enough signal for tone fitting, not a scene rich enough to
    // learn a new colour matrix. Still reject monochrome references.
    let (pairs, bright) = collect_pairs(sensor, reference, if colour.is_some() { 0.005 } else { 0.05 }, edge_limit)?;
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
        let Some(curve) = tone_fit.fit(tone_pairs) else { continue };
        let mut look = CameraLook { matrix, tone: curve, hue_sat: hue_sat.clone() };
        if let Some(tone) = fit_chroma(&bright, &look) {
            look.tone = tone;
        }
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
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
            "[profile] camera look holdout RMS {:.5} -> {:.5} ({samples} channels{}{})",
            (before / samples as f64).sqrt(),
            (after / samples as f64).sqrt(),
            if edge_limit.is_some() { ", away from edges" } else { "" },
            if tone_fit == ToneFit::ConditionalMedian { ", conditional-median tone" } else { "" }
        );
    }
    if samples == 0 || after >= before * MIN_IMPROVEMENT || after / samples as f64 > MAX_HOLDOUT_RMS.powi(2) {
        return None;
    }
    Some(look)
}

/// Weight of the photo's own colour matrix in a [`fit_partial`] look; the rest is the identity (the
/// neutral fallback's generic camera ≈ sRGB colour). On the CC0 archive's proxies, with the
/// matrix at full weight 2 of the 23 colourful photos without a look came out more than 2° further
/// from the camera's hues than the neutral fallback, at half weight none (median hue error 9.6° with
/// the generic colour, which the tone and chroma curves make visible, 5.1° at half weight).
const PARTIAL_MATRIX_WEIGHT: f64 = 0.5;

/// Smallest rank correlation of scene and camera JPEG luminance over a [`fit_partial`] look's pairs:
/// the JPEG must show the same picture. A look that only matches the brightness distribution of an
/// unrelated picture can still beat the dark neutral fallback pixel for pixel. On the CC0 archive's
/// proxies the accepted full looks have 0.55 or more, lens-corrected compacts without one 0.53–0.77,
/// three old Coolpix files whose raw decodes wrongly 0.07–0.27.
const PARTIAL_MIN_RANK_CORRELATION: f64 = 0.5;

/// Spearman rank correlation of the scene's and the JPEG's luminance over `pairs`.
fn rank_correlation(pairs: &[([f64; 3], [f64; 3])]) -> f64 {
    let ranks = |values: Vec<f64>| {
        let mut order: Vec<usize> = (0..values.len()).collect();
        order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
        let mut ranks = vec![0.0; values.len()];
        for (rank, i) in order.into_iter().enumerate() {
            ranks[i] = rank as f64;
        }
        ranks
    };
    let scene = ranks(pairs.iter().map(|(x, _)| luma(*x)).collect());
    let jpeg = ranks(pairs.iter().map(|(_, y)| luma(*y)).collect());
    let mean = (pairs.len() as f64 - 1.0) / 2.0;
    let (covariance, variance) =
        scene.iter().zip(&jpeg).fold((0.0, 0.0), |(c, v), (a, b)| (c + (a - mean) * (b - mean), v + (a - mean) * (a - mean)));
    if variance > 0.0 { covariance / variance } else { 0.0 }
}

/// A partial look for a photo whose full look misses the acceptance gates (lens-corrected JPEGs of
/// compacts, dull or nearly colourless scenes): the tone curve by quantile matching, which needs
/// no pixel-exact registration and was the largest part of the neutral fallback's error (rendered
/// 9–22 L* too dark), the chroma curve (the fallback kept about a third of the camera's
/// colourfulness), and, when the scene has enough colour to learn one (the full look's condition),
/// the photo's own colour matrix at [`PARTIAL_MATRIX_WEIGHT`]. No hue/saturation table. Tried on
/// all pixels and then, like the full look, on the pixels away from edges; used only when it cuts
/// the held-out squared error to below [`MIN_IMPROVEMENT`] of the neutral fallback's, in linear
/// and in gamma-encoded display values, and when the JPEG shows the same picture
/// ([`PARTIAL_MIN_RANK_CORRELATION`]). There is no limit on its own error: it only has to be clearly
/// closer to the camera JPEG than what the photo would get otherwise.
fn fit_partial(sensor: &Rgb32f, reference: &Rgb32f) -> Option<CameraLook> {
    [None, Some(EDGE_CONTRAST)].into_iter().find_map(|edge_limit| fit_partial_on(sensor, reference, edge_limit))
}

fn fit_partial_on(sensor: &Rgb32f, reference: &Rgb32f, edge_limit: Option<f32>) -> Option<CameraLook> {
    // as for a known colour model: enough signal for the tone, but no monochrome reference
    let (pairs, bright) = collect_pairs(sensor, reference, 0.005, edge_limit)?;
    let related = rank_correlation(&pairs);
    if related < PARTIAL_MIN_RANK_CORRELATION {
        if lightcraft_pipeline::profiling() {
            eprintln!("[profile] camera look partial: luminance rank correlation {related:.3}, not the same picture");
        }
        return None;
    }
    let spread = |y: &[f64; 3]| y.iter().copied().fold(f64::NEG_INFINITY, f64::max) - y.iter().copied().fold(f64::INFINITY, f64::min);
    let colourful = pairs.iter().filter(|(_, y)| spread(y) > 0.05).count() >= pairs.len() / 20;
    let matrix = match colourful.then(|| fit_matrix(&pairs)).flatten() {
        Some(own) => Mat3(std::array::from_fn(|r| {
            std::array::from_fn(|c| Mat3::IDENTITY.0[r][c] + PARTIAL_MATRIX_WEIGHT * (own.0[r][c] - Mat3::IDENTITY.0[r][c]))
        })),
        None => Mat3::IDENTITY,
    };
    let tone_pairs: Vec<_> =
        pairs.iter().enumerate().filter(|(i, _)| i % 3 != 0).map(|(_, (x, y))| (luma(matrix.apply(*x).map(|v| v.max(0.0))), luma(*y))).collect();
    let mut look = CameraLook { matrix, tone: fit_tone(tone_pairs)?, hue_sat: None };
    if let Some(tone) = fit_chroma(&bright, &look) {
        look.tone = tone;
    }
    let (tone, neutral) = (ToneMap::camera(&look.tone, 0.0, 0.0, 0.0), ToneMap::new(0.0, 0.0, 0.0));
    let encoded = |v: f64| v.max(0.0).powf(1.0 / 2.2);
    let (mut before, mut after, mut before_encoded, mut after_encoded, mut samples) = (0.0, 0.0, 0.0, 0.0, 0);
    for (x, target) in pairs.iter().step_by(3) {
        let (fallback, partial) = (displayed(*x, &neutral), displayed(matrix.apply(*x), &tone));
        for c in 0..3 {
            before += (fallback[c] - target[c]).powi(2);
            after += (partial[c] - target[c]).powi(2);
            before_encoded += (encoded(fallback[c]) - encoded(target[c])).powi(2);
            after_encoded += (encoded(partial[c]) - encoded(target[c])).powi(2);
            samples += 1;
        }
    }
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] camera look partial (tone{}{}) holdout RMS {:.5} -> {:.5} ({samples} channels{})",
            if look.tone.chroma().iter().any(|k| *k != 1.0) { ", chroma" } else { "" },
            if matrix != Mat3::IDENTITY { ", damped matrix" } else { "" },
            (before / samples.max(1) as f64).sqrt(),
            (after / samples.max(1) as f64).sqrt(),
            if edge_limit.is_some() { ", away from edges" } else { "" }
        );
    }
    let better =
        after.is_finite() && after_encoded.is_finite() && after < before * MIN_IMPROVEMENT && after_encoded < before_encoded * MIN_IMPROVEMENT;
    (samples > 0 && better).then_some(look)
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
    let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
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
    let with = ToneMap::camera(&fitted, 0.0, 0.0, 0.0);
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

/// How the per-photo tone curve is fitted ([`fit_pairs_ordered`] tries them in order).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ToneFit {
    /// [`fit_tone`]: no flat steps, no pixel-exact registration needed.
    Quantile,
    /// [`fit_tone_conditional`]: the earlier fit, kept as the fallback.
    ConditionalMedian,
}

impl ToneFit {
    fn fit(self, pairs: Vec<(f64, f64)>) -> Option<CameraTone> {
        match self {
            ToneFit::Quantile => fit_tone(pairs),
            ToneFit::ConditionalMedian => fit_tone_conditional(pairs),
        }
    }
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
fn fit_tone(pairs: Vec<(f64, f64)>) -> Option<CameraTone> {
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
    if knots[31][0] < knots[0][0] * 1.5 {
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
fn fit_tone_conditional(mut pairs: Vec<(f64, f64)>) -> Option<CameraTone> {
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
    CameraTone::new(knots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_jpeg_matches_the_sensor_proxy_size_exactly() {
        // An ILCE-7RM4 ARW (9504×6336 crop) embeds a 1616×1080 preview; for the camera-profile proxy
        // it is decoded at 384×257 (`proxies`: twice the proxy edge) next to a 192×128 sensor proxy.
        let sensor = fit(&Rgb32f::new(9504, 6336), PROFILE_PROXY, PROFILE_PROXY, Filter::Box);
        assert_eq!((sensor.width, sensor.height), (192, 128));
        let jpeg = Rgb32f::new(384, 257);
        // what fitting gave before: a pixel short, so every pair was rejected
        assert_eq!(fit(&jpeg, sensor.width, sensor.height, Filter::Box).width, 191);
        let r = reference_at(&jpeg, &sensor);
        assert_eq!((r.width, r.height), (192, 128));
        // and at the single-photo proxy size, where fitting happened to land on the same size
        let sensor = fit(&Rgb32f::new(9504, 6336), PROXY, PROXY, Filter::Box);
        let r = reference_at(&Rgb32f::new(192, 129), &sensor);
        assert_eq!((r.width, r.height), (sensor.width, sensor.height));
    }

    #[test]
    fn linear_arw_gets_a_sensor_proxy_without_demosaicing() {
        use lightcraft_raw::{BlackLevel, ColorData, OpcodeLists, Orientation, RawData, RawFormat, Rect};
        let mut raw = RawImage {
            format: RawFormat::Arw,
            width: 32,
            height: 32,
            cpp: 3,
            data: RawData::F32([0.2, 0.3, 0.4].repeat(32 * 32)),
            cfa: None,
            bits: 16,
            black: BlackLevel::uniform(0.0),
            white: vec![1.0],
            active_area: Rect::new(0, 0, 32, 32),
            crop: Rect::new(0, 0, 32, 32),
            orientation: Orientation::from_exif(1),
            color: ColorData::default(),
            wb_multipliers: Some([1.0; 3]),
            linearized: true,
            opcodes: OpcodeLists::default(),
            metadata: lightcraft_meta::Metadata::default(),
        };
        let proxy = sensor_proxy(&raw, 2, 384).unwrap();
        assert_eq!((proxy.width, proxy.height), (32, 32));
        assert_eq!(proxy.data[0], [0.2, 0.3, 0.4]);
        raw.data = RawData::F32(Vec::new());
        assert!(sensor_proxy(&raw, 2, 384).is_none());
    }
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
        let tone = ToneMap::camera(&fit.tone, 0.0, 0.0, 0.0);
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
        let with = ToneMap::camera(&fit.tone, 0.0, 0.0, 0.0);
        let without = ToneMap::camera(&fit.tone.with_chroma([1.0; lightcraft_pipeline::tone::CHROMA_N]).unwrap(), 0.0, 0.0, 0.0);
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
            pool.extend(collect_pairs(&sensor, &reference, 0.05, None).unwrap().0);
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
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
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
    fn profile_fits_tone_without_learning_colour_from_a_dull_scene() {
        let matrix = Mat3([[2.0, -0.02, 0.0], [0.0, 2.0, 0.0], [0.0, -0.03, 2.0]]);
        let mut sensor = Rgb32f::new(64, 48);
        let mut reference = sensor.clone();
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            *src = [0.02 + (i % 997) as f32 * 0.00045; 3];
            let p = matrix.apply_f32(*src);
            let y = luminance_2020(p);
            *dst = p.map(|v| v * (1.0 - (-2.5 * y).exp()) / y);
        }
        assert!(fit_pairs(&sensor, &reference).is_none(), "not enough colour to learn a matrix");
        let look = fit_pairs_with(&sensor, &reference, Some((matrix, None))).unwrap();
        assert_eq!(look.matrix, matrix);
        reference.map_in_place(|p| [luminance_2020(p); 3]);
        assert!(fit_pairs_with(&sensor, &reference, Some((matrix, None))).is_none(), "monochrome JPEGs remain rejected");
    }

    #[test]
    fn rejected_profile_preserves_a_usable_per_photo_fit() {
        let matrix = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let wrong = Mat3([[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]]);
        let mut sensor = Rgb32f::new(64, 64);
        let mut reference = sensor.clone();
        let mut seed = 0x2545_f491_u32;
        for (src, dst) in sensor.data.iter_mut().zip(&mut reference.data) {
            *src = std::array::from_fn(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                0.05 + (seed % 1000) as f32 * 0.0003
            });
            let p = matrix.apply_f32(*src);
            let y = luminance_2020(p);
            *dst = p.map(|v| (v * (1.0 - (-2.5 * y).exp()) / y).clamp(0.0, 0.999));
        }
        let per_photo = fit_pairs(&sensor, &reference).unwrap();
        let look = fit_pairs_with(&sensor, &reference, Some((wrong, None))).unwrap();
        assert_eq!(look.matrix, per_photo.matrix, "a rejected camera profile must not suppress the per-photo fit");
        assert_eq!(look.tone, per_photo.tone);
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
    fn supported_raws_get_a_file_local_look() {
        // every decoded raw without a colour matrix of its own (issue #310: CR2 rendered flat with the fallback)
        let local = [
            RawFormat::Arw,
            RawFormat::Nef,
            RawFormat::Nrw,
            RawFormat::Rw2,
            RawFormat::Raf,
            RawFormat::Cr3,
            RawFormat::Cr2,
            RawFormat::Pef,
            RawFormat::Srw,
            RawFormat::Orf,
        ];
        assert!(local.into_iter().all(file_local_look));
        // DNG carries its own colour model
        assert!(!file_local_look(RawFormat::Dng));
    }

    #[test]
    fn cr3_camera_framing_is_measured_independently_of_tone_and_rejects_unrelated_edges() {
        let (w, h) = (192, 128);
        let mut sensor = Rgb32f::new(w, h);
        for (i, pixel) in sensor.data.iter_mut().enumerate() {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let value = 0.35 + 0.13 * (x * 0.15 + y * 0.22).sin() + 0.1 * (x * 0.33 - y * 0.14).sin() + 0.07 * (x * 0.09 - y * 0.31).cos();
            *pixel = [value; 3];
        }
        let expected = Cr3Framing { scale: 0.96, offset: [0.01, -0.006] };
        let mut reference = Rgb32f::new(w, h);
        for (i, pixel) in reference.data.iter_mut().enumerate() {
            let (x, y) = expected.source((i % w) as f32 + 0.5, (i / w) as f32 + 0.5, w, h);
            *pixel = sensor.sample_bilinear(x, y).map(|v| v.powf(0.7));
        }
        let measured = estimate_cr3_framing(&sensor, &reference).expect("coherent held-out edges locate camera framing despite nonlinear tone");
        assert!((measured.scale - expected.scale).abs() < 0.006, "{measured:?}");
        assert!((measured.offset[0] - expected.offset[0]).abs() < 0.003, "{measured:?}");
        assert!((measured.offset[1] - expected.offset[1]).abs() < 0.003, "{measured:?}");
        assert!(estimate_cr3_framing(&sensor, &sensor).is_none(), "already aligned pixels stay unchanged");
        reference.data.fill([0.2; 3]);
        assert!(estimate_cr3_framing(&sensor, &reference).is_none(), "flat previews cannot establish framing");
        for (i, pixel) in reference.data.iter_mut().enumerate() {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            *pixel = [0.35 + 0.13 * (x * 0.41 - y * 0.07).sin() + 0.1 * (x * 0.04 + y * 0.39).cos(); 3];
        }
        assert!(estimate_cr3_framing(&sensor, &reference).is_none(), "unrelated edges cannot change the framing");
    }

    /// The guarded fit is assessed on real sensor data, never on the JPEG fallback. A corpus
    /// download or an unfinished codec cannot make this test claim a successful RAW calibration.
    #[test]
    fn corpus_cr3_gets_a_framing_aligned_camera_look() {
        let path = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw/cr3-canon-r100-raw.cr3");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {} absent", path.display());
            return;
        };
        let raw = match lightcraft_raw::decode(&bytes) {
            Ok(raw) => raw,
            Err(lightcraft_raw::RawError::Unsupported(why)) => {
                eprintln!("skip CR3 sensor decoder: {why}");
                return;
            }
            Err(error) => panic!("CR3 corpus failed to decode: {error}"),
        };
        assert_eq!(raw.format, RawFormat::Cr3);
        assert_eq!(raw.info().developed_size(), (6000, 4000));
        let transform = lightcraft_raw::color::camera_transform(&raw, lightcraft_raw::color::as_shot_white_xy(&raw));
        let look = fit_preview(&raw, &bytes, &transform);
        eprintln!("R100 guarded colour fit accepted: {}", look.is_some());
        assert!(look.is_some(), "this R100 corpus photo has enough matching colour after camera-framing alignment");
        let (_, info) = crate::files::load_bytes(&bytes, 400).unwrap();
        assert!(info.raw && info.relative_wb);
        assert_eq!((info.as_shot_temp, info.as_shot_tint), (6500.0, 0.0));
        assert_eq!(info.camera_tone.is_some(), look.is_some());
    }

    /// A public ILCE-7CR sample (skipped without the corpus) takes its colour from the bundled ILCE-7CR profile and
    /// passes the acceptance gates with it: the fitted look keeps the profile's matrix and table, which the photo's
    /// own fit (the fallback when a profile is rejected) would not.
    #[test]
    fn corpus_a7cr_uses_the_bundled_profile() {
        let path = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw/arw-sony-a7cr-lossless-l.arw");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skip: {} absent", path.display());
                return;
            }
            Err(e) => panic!("{}: {e}", path.display()),
        };
        let (_, json) = crate::camera_profiles::BUNDLED.iter().find(|(m, _)| *m == "ILCE-7CR").expect("ILCE-7CR is built in");
        let bundled: crate::camera_profiles::CameraProfile = serde_json::from_str(json).unwrap();
        let used = crate::camera_profiles::get("ILCE-7CR").expect("ILCE-7CR profile");
        assert_eq!(*used, bundled, "a local ILCE-7CR profile overrides the built-in one; remove it to run this test");
        let raw = lightcraft_raw::decode(&bytes).unwrap();
        assert_eq!(raw.metadata.model.as_deref(), Some("ILCE-7CR"));
        let transform = lightcraft_raw::color::camera_transform(&raw, lightcraft_raw::color::as_shot_white_xy(&raw));
        let look = fit_preview(&raw, &bytes, &transform).expect("fit accepted");
        let expected = bundled.matrix().mul(&transform.matrix.inverse().unwrap());
        for (row, want) in look.matrix.0.iter().zip(expected.0) {
            for (got, want) in row.iter().zip(want) {
                assert!((got - want).abs() < 1e-12, "the look's matrix is not the profile's: {:?} vs {:?}", look.matrix.0, expected.0);
            }
        }
        assert_eq!(look.hue_sat, bundled.hue_sat, "the look's table is not the profile's");
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

    /// The public ILCE-7CR samples (61 MP; skipped without the corpus): lossless compressed L and M and compressed
    /// ARW2 all get a look fitted to their own JPEG, not the neutral fallback.
    #[test]
    fn corpus_a7cr_codings_get_a_camera_look() {
        let dir = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw");
        for name in ["arw-sony-a7cr-lossless-l.arw", "arw-sony-a7cr-lossless-m.arw", "arw-sony-a7cr-compressed.arw"] {
            let bytes = match std::fs::read(dir.join(name)) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    eprintln!("skip: {name} absent");
                    continue;
                }
                Err(e) => panic!("{name}: {e}"),
            };
            let (img, info) = crate::files::load_bytes(&bytes, 400).unwrap();
            assert_eq!((img.width, img.height), (400, 267), "{name}: framed in the default crop");
            assert!(info.camera_tone.is_some(), "{name}: no camera look fitted");
            assert!(info.relative_wb && info.as_shot_temp == 6500.0 && info.as_shot_tint == 0.0, "{name}");
        }
    }

    /// A public DC-FZ1000 II RW2 shot at 4:3 on its 3:2 sensor (skipped without the corpus): the default crop is 4:3
    /// while the embedded JPEG shows the whole sensor; the look is still fitted, against the matching part of it.
    #[test]
    fn corpus_rw2_with_an_in_camera_crop_gets_a_camera_look() {
        let path = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw/rw2-panasonic-fz1000m2-4x3.rw2");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {} absent", path.display());
            return;
        };
        let (img, info) = crate::files::load_bytes(&bytes, 400).unwrap();
        assert_eq!((img.width, img.height), (400, 300), "framed in the in-camera aspect ratio");
        assert!(info.camera_tone.is_some(), "no camera look fitted");
        assert!(info.relative_wb && info.as_shot_temp == 6500.0 && info.as_shot_tint == 0.0);
    }

    /// Issue #232 (skipped without the corpus): the public ILCE-7RM2 sample's camera JPEG is
    /// distortion-corrected, and on its glass façade the fit on all pixels misses the gate
    /// (held-out RMS 0.113): it used to open grey with the neutral fallback.
    #[test]
    fn corpus_arw_with_a_lens_corrected_preview_gets_a_camera_look() {
        let path = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw/arw-sony-a7rm2-12bit-uncompressed.arw");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {} absent", path.display());
            return;
        };
        let (_, info) = crate::files::load_bytes(&bytes, 400).unwrap();
        assert!(info.camera_tone.is_some(), "no camera look fitted");
    }

    /// A look with tone curve `f` at knots `xs` and a colour model and chroma curve to keep.
    fn dro_test_look(xs: &[f32; 32], f: &dyn Fn(f32) -> f32) -> CameraLook {
        CameraLook {
            matrix: Mat3([[1.6, -0.4, -0.2], [-0.2, 1.4, -0.2], [0.0, -0.3, 1.3]]),
            tone: CameraTone::new(xs.map(|x| [x, f(x)])).unwrap().with_chroma([1.3, 1.2, 1.1, 1.0, 1.0, 0.9, 0.6, 0.3]).unwrap(),
            hue_sat: Some(HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[5.0, 1.1, 1.0]; 8], srgb_value: false }),
        }
    }

    /// `fixed` (from `fitted`) is `min(fitted, Sony's DRO-off curve)` at every knot but the
    /// second-last, which may stay higher (at most the fitted curve) to keep the shoulder past the
    /// last knot below the fitted one; nowhere brighter than `fitted`, colour unchanged.
    fn assert_lowered(fitted: &CameraLook, fixed: &CameraLook, what: &str) {
        let dro_off = sony_dro_off_tone().unwrap();
        assert_eq!((fixed.matrix, &fixed.hue_sat, fixed.tone.chroma()), (fitted.matrix, &fitted.hue_sat, fitted.tone.chroma()), "{what}");
        for (i, (k, own)) in fixed.tone.knots().iter().zip(fitted.tone.knots()).enumerate() {
            let lowered = own[1].min(dro_off.apply(own[0]));
            assert_eq!(k[0], own[0], "{what}: knot {i}");
            if i == 30 {
                assert!(k[1] >= lowered && k[1] <= own[1], "{what}: knot {i} {} not in {lowered}..={}", k[1], own[1]);
            } else {
                assert!((k[1] - lowered).abs() < 1e-6, "{what}: knot {i} {} vs {lowered}", k[1]);
            }
        }
        assert!(at_most(&fixed.tone, &fitted.tone), "{what}: brighter than the fitted curve somewhere");
        // the same check spelled out, from deep shadows to far past the last knot (the shoulder)
        for i in 0..=2400 {
            let x = 0.001 * 1.005f32.powi(i);
            assert!(fixed.tone.apply(x) <= fitted.tone.apply(x) + 1e-5, "{what} at {x}: {} > {}", fixed.tone.apply(x), fitted.tone.apply(x));
        }
    }

    /// Issue #244: Sony's Dynamic Range Optimizer lifts the camera JPEG's shadows, not the raw's. A
    /// look fitted to such a JPEG keeps its colour (matrix, hue/saturation table, chroma curve) and
    /// its curve is lowered to Sony's without DRO wherever that is darker, never raised.
    #[test]
    fn a_dro_look_keeps_its_colour_and_is_lowered_to_the_dro_off_tone() {
        let dro_off = sony_dro_off_tone().expect("the built-in curve is valid");
        // the curve itself: black stays black, increasing, mid-grey where cameras put it (L* 50–75)
        assert_eq!(dro_off.apply(0.0), 0.0);
        let mut previous = 0.0;
        for i in 1..200 {
            let y = dro_off.apply(i as f32 * 0.005);
            assert!(y > previous && y < 1.0, "{i}: {y}");
            previous = y;
        }
        assert!((0.18..0.48).contains(&dro_off.apply(0.18)), "{}", dro_off.apply(0.18));
        let xs: [f32; 32] = std::array::from_fn(|i| 0.006 * 1.13f32.powi(i as i32));
        // a JPEG with lifted shadows: the curve comes down to Sony's, colour and chroma curve stay
        let lifted = dro_test_look(&xs, &|x| dro_off.apply(x).powf(0.8));
        let fixed = with_dro_off_tone(lifted.clone());
        assert_lowered(&lifted, &fixed, "lifted");
        assert!(fixed.tone.apply(0.02) < 0.85 * lifted.tone.apply(0.02));
        // a camera that maps raw values darker than Sony's curve keeps its own curve exactly
        let darker = dro_test_look(&xs, &|x| 0.7 * dro_off.apply(x));
        assert_eq!(with_dro_off_tone(darker.clone()).tone, darker.tone);
        // crossing curves: the lower of the two at the knots, still a valid increasing curve
        let crossing = dro_test_look(&xs, &|x| (dro_off.apply(x) * (0.6 + x)).min(0.95));
        assert_lowered(&crossing, &with_dro_off_tone(crossing.clone()), "crossing");
    }

    /// Past its last knot a camera curve continues as a shoulder whose rate comes from its last two
    /// knots. Lowering the second-last knot more than the last steepened that shoulder: the curve
    /// fitted to the CC0 NEX-5T sample (raw.pixls.us, DRO Auto) went from 0.740 to 0.843 at scene
    /// luminance 0.63, brighter than before. The lowered curve must stay at or below the fitted one
    /// through the shoulder, on that curve and on many others.
    #[test]
    fn the_lowered_curve_stays_below_the_fitted_one_past_its_last_knot() {
        let nex5t: [[f32; 2]; 32] = [
            [0.014948029, 0.017424058],
            [0.018395446, 0.023981506],
            [0.021539066, 0.030581286],
            [0.024555191, 0.0374646],
            [0.027602654, 0.044408206],
            [0.030193308, 0.049894042],
            [0.03261828, 0.055907387],
            [0.034544908, 0.06156683],
            [0.03669959, 0.06814718],
            [0.039161213, 0.075263545],
            [0.04173663, 0.0830011],
            [0.044166446, 0.088166036],
            [0.04682434, 0.095398724],
            [0.049544845, 0.1027153],
            [0.05235826, 0.11173598],
            [0.05497513, 0.119017586],
            [0.057503678, 0.12616614],
            [0.059836045, 0.13521175],
            [0.062677935, 0.14601946],
            [0.06599234, 0.15573128],
            [0.069809504, 0.16883339],
            [0.0755404, 0.1854325],
            [0.08250754, 0.20684941],
            [0.089204684, 0.23466235],
            [0.09650368, 0.25805998],
            [0.102161564, 0.27849936],
            [0.107812546, 0.29706743],
            [0.11410169, 0.31379074],
            [0.12000221, 0.33262977],
            [0.12776275, 0.35407156],
            [0.1387929, 0.38362882],
            [0.19492775, 0.43935108],
        ];
        let fitted = CameraLook { tone: CameraTone::new(nex5t).unwrap(), ..dro_test_look(&std::array::from_fn(|i| 0.01 + i as f32 * 0.01), &|x| x) };
        let fixed = with_dro_off_tone(fitted.clone());
        assert_lowered(&fitted, &fixed, "NEX-5T");
        assert!((fitted.tone.apply(0.63) - 0.7405).abs() < 1e-3 && fixed.tone.apply(0.63) <= fitted.tone.apply(0.63));
        assert_ne!(fixed.tone, fitted.tone, "the lifted mid-tones still come down");
        // curves that end at all kinds of scene luminances, slopes and heights relative to Sony's
        let dro_off = sony_dro_off_tone().unwrap();
        let mut seed = 0x9e37_79b9_u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f32::from((seed >> 16) as u16) / 65536.0
        };
        for case in 0..300 {
            let (start, span) = (0.002 + 0.03 * next(), 1.05 + 0.15 * next());
            let xs: [f32; 32] = std::array::from_fn(|i| start * span.powi(i as i32));
            let (lift, gain, top) = (0.5 + 0.6 * next(), 0.6 + 0.8 * next(), next());
            // brighter or darker than Sony's, with a last knot sometimes lowered less than the one before
            let f = |x: f32| (gain * dro_off.apply(x).powf(lift) + top * 0.2 * (x / xs[31]).powi(8)).min(0.995);
            let mut ys = xs.map(f);
            for i in 1..32 {
                ys[i] = ys[i].max(ys[i - 1]);
            }
            let Some(tone) = CameraTone::new(std::array::from_fn(|i| [xs[i], ys[i]])) else { continue };
            let fitted = CameraLook { tone, ..dro_test_look(&std::array::from_fn(|i| 0.01 + i as f32 * 0.01), &|x| x) };
            assert_lowered(&fitted, &with_dro_off_tone(fitted.clone()), &format!("case {case}"));
        }
    }

    /// Issue #244 (skipped without the corpus): the public ILCE-7RM2 sample (Standard creative
    /// style) was shot with DRO Auto, so its curve comes down to Sony's without DRO in the shadows the
    /// camera lifted; the ILCE-7M4 sample (DRO Auto, Vivid) has deeper shadows than that curve and
    /// keeps them, only its brighter highlights come down. Both keep the colour fitted to their
    /// JPEGs. The ILCE-7M3 sample (DRO off) keeps its own fit.
    #[test]
    fn corpus_arw_with_dro_is_lowered_to_the_dro_off_tone() {
        let dir = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw");
        let samples = [
            ("arw-sony-a7rm2-12bit-uncompressed.arw", true, [0.01, 0.05, 0.1]),
            ("arw-sony-a7m4-14bit.arw", true, [0.3, 0.4, 0.5]),
            ("arw-sony-a7m3-compressed.arw", false, [0.0; 3]),
        ];
        for (name, dro, lowered) in samples {
            let path = dir.join(name);
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    eprintln!("skip: {} absent", path.display());
                    continue;
                }
                Err(e) => panic!("{}: {e}", path.display()),
            };
            assert_eq!(lightcraft_raw::embedded_preview_dynamic_range_optimized(&bytes), Some(dro), "{name}");
            let mut raw = lightcraft_raw::decode(&bytes).unwrap();
            raw.opcodes.list3.retain(|op| !op.is_lens_correction());
            let transform = lightcraft_raw::color::camera_transform(&raw, lightcraft_raw::color::as_shot_white_xy(&raw));
            let look = fit_preview(&raw, &bytes, &transform).expect("a camera look");
            // the look fitted to the JPEG, as before this change
            let (sensor, reference, clipped) = proxies(&raw, &bytes, &transform, PROXY).unwrap();
            let profile = raw.metadata.model.as_deref().and_then(crate::camera_profiles::get);
            let colour = profile.as_ref().and_then(|p| Some((p.matrix().mul(&transform.matrix.inverse()?), p.hue_sat.clone())));
            let fitted = fit_look(&sensor, &reference, &clipped, colour).unwrap();
            if dro {
                assert_lowered(&fitted, &look, name);
                for x in lowered {
                    assert!(look.tone.apply(x) < 0.95 * fitted.tone.apply(x), "{name} at {x}: {} vs {}", look.tone.apply(x), fitted.tone.apply(x));
                }
            } else {
                assert_eq!((look.matrix, &look.hue_sat, look.tone), (fitted.matrix, &fitted.hue_sat, fitted.tone), "{name}");
            }
            let (_, info) = crate::files::load_bytes(&bytes, 400).unwrap();
            assert_eq!(info.camera_tone, Some(look.tone), "{name}");
        }
    }

    /// Issue #232: an ILCE-7RM2 camera JPEG is lens-corrected ("Distortion Comp.: Auto"), so on a
    /// façade of window frames the raw and its JPEG are a pixel or two apart. The colours agree
    /// everywhere away from the edges, and the look must still be found there.
    #[test]
    fn fits_a_slightly_misaligned_camera_jpeg_away_from_edges() {
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let (w, h, block, shift) = (96, 64, 8, 2);
        // Patches of 8×8 pixels, alternately dark and bright (frames and panes), varied in colour.
        let scene = |x: usize, y: usize| -> [f32; 3] {
            let (bx, by) = (x / block, y / block);
            let ev = if (bx + by) % 2 == 0 { 0.04 + (bx % 3) as f32 * 0.01 } else { 0.3 + (by % 4) as f32 * 0.08 };
            [ev * (0.7 + ((bx * 7 + by * 3) % 11) as f32 * 0.05), ev, ev * (0.7 + ((bx * 5 + by) % 13) as f32 * 0.04)]
        };
        let camera = |p: [f32; 3]| {
            let q = known.apply_f32(p);
            let y = luminance_2020(q);
            q.map(|v| v * (1.0 - (-2.5 * y).exp()) / y)
        };
        let mut sensor = Rgb32f::new(w, h);
        let mut reference = sensor.clone();
        for y in 0..h {
            for x in 0..w {
                sensor.data[y * w + x] = scene(x, y);
                // The JPEG shows the scene `shift` pixels to the right of where the raw has it.
                reference.data[y * w + x] = camera(scene(x.saturating_sub(shift), y));
            }
        }
        let fit = fit_pairs(&sensor, &reference).expect("look fitted away from the misaligned edges");
        // The look itself is right: on the aligned scene it reproduces the camera.
        let tone = ToneMap::camera(&fit.tone, 0.0, 0.0, 0.0);
        let (mut error, mut n) = (0.0, 0);
        for y in 0..h {
            for x in 0..w {
                let p = displayed(fit.matrix.apply(scene(x, y).map(f64::from)), &tone);
                let t = camera(scene(x, y));
                error += (0..3).map(|c| (p[c] - f64::from(t[c])).powi(2)).sum::<f64>();
                n += 3;
            }
        }
        assert!((error / n as f64).sqrt() < 0.03, "{}", (error / n as f64).sqrt());
        // Unrelated colours stay rejected away from edges too (covered below); a flat JPEG as well.
        reference.data.fill([0.2; 3]);
        assert!(fit_pairs(&sensor, &reference).is_none());
    }

    /// X-Trans and 16-bit Bayer RAFs use the same fixed fit at thumbnail and export sizes.
    /// The X-T20 fails the gates on all pixels (held-out RMS 0.114 against 0.10) and passes away
    /// from edges (0.056, issue #232), so it is fitted too. Skips cleanly without these CC0
    /// corpus files.
    #[test]
    fn corpus_raf_colour_and_white_balance() {
        let dir = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw");
        for (name, accepted) in [("raf-fuji-xt2-865.raf", true), ("raf-fuji-gfx100s-4503.raf", true), ("raf-fuji-xt20-compressed.raf", true)] {
            let Ok(bytes) = std::fs::read(dir.join(name)) else { continue };
            let header = crate::files::probe_bytes(name, &bytes).unwrap();
            assert_eq!(header.as_shot_wb, Some((6500.0, 0.0)), "{name}");
            let (_, small) = crate::files::load_bytes(&bytes, 400).unwrap();
            let (_, large) = crate::files::load_bytes(&bytes, 1200).unwrap();
            assert_eq!(small.camera_tone.is_some(), accepted, "{name}");
            assert_eq!(small.camera_tone, large.camera_tone, "{name}: resolution changed the fit");
            assert!(small.raw && small.relative_wb && small.as_shot_temp == 6500.0 && small.as_shot_tint == 0.0, "{name}");
        }
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

    /// A busy scene whose JPEG is the camera curve of the scene two proxy pixels to the right (a
    /// lens-corrected preview): the fitted curve follows the camera's and has no flat steps.
    #[test]
    fn tone_fit_has_no_steps_on_a_misregistered_jpeg() {
        let camera = |x: f64| 0.6 * x.powf(0.55) / (1.0 + 0.2 * x);
        let (w, h) = (96usize, 64usize);
        let mut seed = 0x2545_f491_u32;
        let mut scene = Vec::with_capacity(w * h);
        for _ in 0..w * h {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            scene.push(0.01 * (5.0 * f64::from(seed >> 8) / f64::from(1u32 << 24)).exp());
        }
        let pairs: Vec<(f64, f64)> =
            (0..h).flat_map(|y| (0..w - 2).map(move |x| (y * w + x, y * w + x + 2))).map(|(i, j)| (scene[i], camera(scene[j]))).collect();
        let curve = fit_tone(pairs).unwrap();
        let mut sorted = scene.clone();
        sorted.sort_by(f64::total_cmp);
        let (lo, hi) = (sorted[sorted.len() / 50], sorted[sorted.len() * 49 / 50]);
        let mut x = lo;
        while x < hi {
            let (a, b) = (curve.apply(x as f32), curve.apply((x * 1.02) as f32));
            assert!(b > a, "flat step at {x}: {a} -> {b}");
            assert!((f64::from(a) / camera(x) - 1.0).abs() < 0.05, "at {x}: {a} vs {}", camera(x));
            x *= 1.02;
        }
    }

    /// A synthetic photo whose JPEG is the camera look of the scene scaled per pixel by noise of
    /// `spread` (a pseudo-random, registration-free stand-in for local tone mapping): the per-bin
    /// median recovers the curve, while the JPEG's broadened distribution stretches the quantile fit.
    fn noisy_look(spread: f32) -> (Rgb32f, Rgb32f) {
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let (w, h) = (96usize, 64usize);
        let mut sensor = Rgb32f::new(w, h);
        let mut reference = sensor.clone();
        let mut seed = 0x9e37_79b9_u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f32::from((seed >> 16) as u16) / 32768.0 - 1.0
        };
        for (src, dst) in sensor.data.iter_mut().zip(&mut reference.data) {
            let ev = 0.05 + (next() + 1.0) * 0.2;
            *src = [ev * (0.8 + (next() + 1.0) * 0.1), ev, ev * (0.8 + (next() + 1.0) * 0.1)];
            let p = known.apply_f32(*src);
            let y = luminance_2020(p);
            let scale = (1.0 - (-2.5 * y).exp()) / y * (1.0 + spread * next());
            *dst = p.map(|v| v * scale);
        }
        (sensor, reference)
    }

    /// A camera rendering with constant colourfulness (no highlight bleaching) of a colourful scene
    /// under a bright neutral sky, whose sensor pixels are clipped: one channel held at the clip
    /// level turns the white sky magenta in the raw while the camera JPEG keeps it white. Returns
    /// the sensor proxy, the JPEG proxy and the clip mask.
    fn clipped_sky_scene() -> (Rgb32f, Rgb32f, Vec<bool>) {
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let (w, h) = (96usize, 64usize);
        let mut sensor = Rgb32f::new(w, h);
        let mut reference = sensor.clone();
        let mut clipped = vec![false; w * h];
        let mut seed = 0x2545_f491_u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f32::from((seed >> 16) as u16) / 65536.0
        };
        let camera = |p: [f32; 3]| {
            let y = luminance_2020(p);
            let o = 1.0 - (-2.5 * y).exp();
            p.map(|v| (v * o / y).clamp(0.0, 0.97))
        };
        // the sky: a quarter of the frame, white in the scene and in the JPEG
        let neutral = known.inverse().unwrap().apply_f32([0.32; 3]);
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            let scene = if i < w * h / 4 {
                clipped[i] = true;
                let sky = neutral.map(|v| v * (0.8 + 0.4 * next()));
                *dst = camera(known.apply_f32(sky));
                // the green sample clips, red and blue (after white balance) go on rising
                *src = [sky[0] * 1.35, sky[1] * 0.85, sky[2] * 1.4];
                continue;
            } else {
                // colourful foliage, leaves, bark and bright flowers from dark to light
                let ev = 0.02 + next() * 0.4;
                [ev * (0.6 + next() * 0.8), ev, ev * (0.5 + next() * 0.8)]
            };
            *src = scene;
            *dst = camera(known.apply_f32(scene));
        }
        (sensor, reference, clipped)
    }

    /// Mean colourfulness (distance from neutral of luminance-normalised RGB) of the unclipped
    /// pixels rendered with `look`, over that of the camera JPEG, per display-luminance band.
    fn colourfulness_ratio(look: &CameraLook, sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool], band: std::ops::Range<f64>) -> f64 {
        let chroma = |p: [f64; 3]| {
            let y = luma(p);
            p.iter().map(|v| (v / y - 1.0).powi(2)).sum::<f64>().sqrt()
        };
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
        let (mut rendered, mut camera) = (0.0, 0.0);
        for ((x, y), c) in sensor.data.iter().zip(&reference.data).zip(clipped) {
            let y = y.map(f64::from);
            if *c || !band.contains(&luma(y)) {
                continue;
            }
            rendered += chroma(displayed(look.matrix.apply(x.map(f64::from)), &tone));
            camera += chroma(y);
        }
        rendered / camera
    }

    /// Clipped sensor pixels are left out of the look's training pairs: a clipped white sky no longer
    /// dulls the colour matrix or makes the chroma curve remove colour at the sky's brightness.
    #[test]
    fn clipped_sky_does_not_dull_the_look() {
        let (sensor, reference, clipped) = clipped_sky_scene();
        let old = fit_pairs(&sensor, &reference).expect("the earlier fit accepts the look");
        let new = fit_look(&sensor, &reference, &clipped, None).expect("look without the clipped pixels");
        let (mid, bright) = (0.1..0.35, 0.35..0.75);
        let old_ratio = (
            colourfulness_ratio(&old, &sensor, &reference, &clipped, mid.clone()),
            colourfulness_ratio(&old, &sensor, &reference, &clipped, bright.clone()),
        );
        let new_ratio =
            (colourfulness_ratio(&new, &sensor, &reference, &clipped, mid), colourfulness_ratio(&new, &sensor, &reference, &clipped, bright));
        // with the clipped sky in the pairs, the bright colours lose much of their colour
        assert!(old_ratio.1 < 0.8, "earlier fit: colourfulness vs camera {old_ratio:?}, chroma {:?}", old.tone.chroma());
        // without it, the render keeps the camera's colourfulness at every brightness
        for r in [new_ratio.0, new_ratio.1] {
            assert!((0.9..1.1).contains(&r), "colourfulness vs camera {new_ratio:?}, chroma {:?}", new.tone.chroma());
        }
        assert!(new.tone.chroma().iter().all(|k| *k > 0.85), "no colour cut: {:?}", new.tone.chroma());
    }

    /// When the accepted attempt misses the gates without the clipped pixels (here: too few pixels
    /// left), the look fitted on all pixels is kept unchanged.
    #[test]
    fn clipped_fit_falls_back_to_all_pixels() {
        let (sensor, reference, _) = clipped_sky_scene();
        let all = fit_pairs(&sensor, &reference).unwrap();
        // nearly everything clipped: too few pairs remain, the earlier look is used unchanged
        let mut clipped = vec![true; sensor.data.len()];
        clipped[..100].iter_mut().for_each(|c| *c = false);
        let fallback = fit_look(&sensor, &reference, &clipped, None).expect("the fallback keeps the look");
        assert_eq!((fallback.matrix.0, fallback.tone), (all.matrix.0, all.tone));
        // nothing clipped: the same look as before, exactly
        let none = fit_look(&sensor, &reference, &vec![false; sensor.data.len()], None).unwrap();
        assert_eq!((none.matrix.0, none.tone), (all.matrix.0, all.tone));
        // without clipped pixels the refit keeps the accepted attempt (here: all pixels, own colour, quantile tone)
        let (_, attempt) = search_ordered(&sensor, &reference, None, TONE_FITS).unwrap();
        assert_eq!(attempt, Attempt { tone: ToneFit::Quantile, profile: false, away_from_edges: false });
        assert!(without_clipped(&sensor, &[true; 3]).is_none(), "a mask of another size is ignored");
    }

    /// Quantile matching is tried first; the conditional-median fit is the fallback and is used
    /// only where the quantile look misses the acceptance gates, so no photo loses its look.
    #[test]
    fn tone_fit_order_quantile_first_conditional_median_as_fallback() {
        // Clean data: both fits pass, the default order uses the quantile curve (increasing).
        let (sensor, reference) = noisy_look(0.0);
        let default = fit_pairs(&sensor, &reference).expect("clean look");
        let quantile = fit_pairs_ordered(&sensor, &reference, None, &[ToneFit::Quantile]).expect("quantile passes");
        assert_eq!(default.tone, quantile.tone);
        // Noisy data: the quantile fit misses the gates, the conditional median still passes.
        let (sensor, reference) = noisy_look(0.33);
        assert!(fit_pairs_ordered(&sensor, &reference, None, &[ToneFit::Quantile]).is_none(), "quantile alone misses the gates");
        let conditional = fit_pairs_ordered(&sensor, &reference, None, &[ToneFit::ConditionalMedian]).expect("conditional median passes");
        let default = fit_pairs(&sensor, &reference).expect("the fallback keeps the look");
        assert_eq!(default.tone, conditional.tone);
        // The order matters only through the first pass; with the fits swapped the conditional one wins even on clean data.
        let (sensor, reference) = noisy_look(0.0);
        let swapped = fit_pairs_ordered(&sensor, &reference, None, &[ToneFit::ConditionalMedian, ToneFit::Quantile]).unwrap();
        let conditional = fit_pairs_ordered(&sensor, &reference, None, &[ToneFit::ConditionalMedian]).unwrap();
        assert_eq!(swapped.tone, conditional.tone);
    }

    /// The camera look of `scene` (the known matrix of these tests, then a bright camera curve).
    fn bright_camera(p: [f32; 3]) -> [f32; 3] {
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let q = known.apply_f32(p);
        let y = luminance_2020(q);
        if y <= 0.0 { [0.0; 3] } else { q.map(|v| v * (1.0 - (-4.0 * y).exp()) / y) }
    }

    /// Mean squared error (all channels, display-linear) of `render` against `target` over `scene`.
    fn mean_error(scene: &Rgb32f, target: impl Fn([f32; 3]) -> [f32; 3], render: impl Fn([f64; 3]) -> [f64; 3]) -> f64 {
        let error: f64 = scene
            .data
            .iter()
            .map(|x| {
                let (p, t) = (render(x.map(f64::from)), target(*x));
                (0..3).map(|c| (p[c] - f64::from(t[c])).powi(2)).sum::<f64>()
            })
            .sum();
        error / (3 * scene.data.len()) as f64
    }

    /// A colourful scene with fine detail everywhere (foliage) whose camera JPEG is lens-corrected:
    /// barrel distortion moves the picture by up to about 4 proxy pixels toward the corners, so the
    /// detail no longer pairs with itself and no global look passes the gates, on all pixels or
    /// away from edges. The partial look still follows the camera's brightness, colourfulness and
    /// hues: on the registered scene it is far closer to the camera than the neutral fallback,
    /// which renders the scene dark and grey.
    #[test]
    fn partial_look_when_a_lens_corrected_jpeg_misses_the_gates() {
        let (w, h) = (96usize, 64usize);
        let scene = |u: f32, v: f32| -> [f32; 3] {
            let base = 0.1 * (1.0 + 0.6 * (0.05 * u + 0.04 * v).sin());
            let detail = 1.0 + 0.8 * (1.7 * u).sin() * (1.3 * v).sin();
            let y = base * detail;
            [y * (0.6 + 0.5 * (0.07 * u).sin().abs()), y, y * (0.5 + 0.6 * (0.09 * v + 0.03 * u).cos().abs())]
        };
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let mut sensor = Rgb32f::new(w, h);
        let mut reference = sensor.clone();
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                let k = 1.0 + 0.08 * (dx * dx + dy * dy) / (cx * cx);
                sensor.data[y * w + x] = scene(x as f32, y as f32);
                reference.data[y * w + x] = bright_camera(scene(cx + dx * k, cy + dy * k));
            }
        }
        assert!(fit_pairs(&sensor, &reference).is_none(), "the full look misses the gates");
        let look = fit_look(&sensor, &reference, &vec![false; w * h], None).expect("a partial look instead of the neutral fallback");
        assert!(look.hue_sat.is_none());
        assert_ne!(look.matrix, Mat3::IDENTITY, "a colourful scene gets the damped matrix");
        // on the registered scene, against what the camera renders
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
        let neutral = ToneMap::new(0.0, 0.0, 0.0);
        let partial = mean_error(&sensor, bright_camera, |x| displayed(look.matrix.apply(x), &tone));
        let fallback = mean_error(&sensor, bright_camera, |x| displayed(x, &neutral));
        assert!(partial < fallback * 0.25, "partial {partial:.5}, neutral fallback {fallback:.5}");
        assert!(tone.apply(0.05) < tone.apply(0.2) && tone.apply(0.2) < tone.apply(0.8));
    }

    /// An overcast, nearly colourless scene (under 5% of the pixels coloured, so no colour matrix is
    /// learnt and the full look is never tried) whose camera renders it two stops brighter than the
    /// neutral fallback: the partial look is the tone curve (and chroma curve) alone.
    #[test]
    fn partial_look_brightens_a_dull_scene_without_learning_colour() {
        let (w, h) = (96usize, 64usize);
        let mut sensor = Rgb32f::new(w, h);
        for (i, p) in sensor.data.iter_mut().enumerate() {
            let ev = 0.01 + (i % 37) as f32 * 0.004;
            let tint = if i % 50 == 0 { 1.3 } else { 1.0 + (i % 5) as f32 * 0.004 };
            *p = [ev * tint, ev, ev * (2.0 - tint)];
        }
        let camera = |p: [f32; 3]| {
            let y = luminance_2020(p);
            p.map(|v| v * (1.0 - (-12.0 * y).exp()) / y)
        };
        let mut reference = sensor.clone();
        reference.map_in_place(camera);
        assert!(fit_pairs(&sensor, &reference).is_none(), "too little colour for the full look");
        let look = fit_look(&sensor, &reference, &vec![false; w * h], None).expect("a partial look");
        assert_eq!(look.matrix, Mat3::IDENTITY, "no colour matrix from a colourless scene");
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
        let neutral = ToneMap::new(0.0, 0.0, 0.0);
        let lightness =
            |tone: &ToneMap| sensor.data.iter().map(|p| f64::from(tone.apply(luminance_2020(*p)))).sum::<f64>() / sensor.data.len() as f64;
        let target = reference.data.iter().map(|p| f64::from(luminance_2020(*p))).sum::<f64>() / reference.data.len() as f64;
        assert!((lightness(&tone) / target - 1.0).abs() < 0.05, "{} vs {target}", lightness(&tone));
        assert!(lightness(&neutral) < target * 0.6, "the neutral fallback is far darker");
    }

    /// When the neutral fallback already matches the camera JPEG, or the JPEG is unrelated or
    /// monochrome, the partial look isn't clearly better and the photo keeps the neutral fallback.
    #[test]
    fn partial_look_only_when_clearly_better_than_the_neutral_fallback() {
        let (w, h) = (96usize, 64usize);
        let mut sensor = Rgb32f::new(w, h);
        for (i, p) in sensor.data.iter_mut().enumerate() {
            let ev = 0.02 + (i % 41) as f32 * 0.012;
            *p = [ev * (0.7 + (i % 11) as f32 * 0.05), ev, ev * (0.7 + (i % 13) as f32 * 0.045)];
        }
        let none = vec![false; w * h];
        // the camera renders exactly as the neutral fallback: nothing to gain
        let neutral = ToneMap::new(0.0, 0.0, 0.0);
        let mut reference = sensor.clone();
        reference.map_in_place(|p| displayed(p.map(f64::from), &neutral).map(|v| v as f32));
        assert!(fit_partial(&sensor, &reference).is_none(), "no clear gain over the fallback");
        // an unrelated picture
        for (i, p) in reference.data.iter_mut().enumerate() {
            *p = [0.05 + (i % 7) as f32 * 0.07, 0.05 + (i % 19) as f32 * 0.02, 0.05 + (i % 29) as f32 * 0.01];
        }
        assert!(fit_look(&sensor, &reference, &none, None).is_none(), "unrelated JPEG");
        // a black-and-white JPEG of the scene
        reference.data.iter_mut().zip(&sensor.data).for_each(|(r, s)| *r = [0.8 * luminance_2020(*s).sqrt(); 3]);
        assert!(fit_look(&sensor, &reference, &none, None).is_none(), "monochrome JPEG");
    }
}
