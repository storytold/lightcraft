use super::colour::{PROXY, file_local_look};
use super::hue_sat::{HueSat, TableGrid, fit_chroma, fit_hue_sat_grid};
use super::mono::{fit_tone_only_with, monochrome};
use super::pairs::{Attempt, EDGE_CONTRAST, Pairs, TONE_FITS, collect_pairs, displayed, fit_attempt, fit_partial, luma, search_ordered};
use super::proxy::{CLIP_TOLERANCE, LensGeometry, MIN_PREVIEW_FRACTION, proxies, structure_agrees};
use super::tone::{HighlightCheck, highlights, median};
use super::{
    BAND_NODES, CameraLook, CameraTone, CameraTransform, ChromaBands, HUE_NODES, HsvTable, Mat3, REC2020, RawImage, Rgb32f, ToneMap, luminance_2020,
};

/// The look fitted to the file's camera JPEG, and whether that JPEG is black and white ([`monochrome`]).
/// `embedded`: the file's embedded lens correction, whose vignetting the render applies ([`look_proxies`]).
pub(crate) fn fit_preview(
    raw: &RawImage,
    bytes: &[u8],
    transform: &CameraTransform,
    lens: Option<&LensGeometry>,
    embedded: Option<&lightcraft_develop::EmbeddedLens>,
) -> (Option<CameraLook>, bool) {
    if !transform.matrix_is_fallback || !file_local_look(raw.format) {
        return (None, false);
    }
    let Some((sensor, reference, clipped)) = look_proxies(raw, bytes, transform, lens, embedded) else { return (None, false) };
    let black_and_white = monochrome(&reference);
    // A camera profile pooled from many photos knows colours this photo shows too little of;
    // try its colour first and fit tone/chroma per photo (picture styles vary).
    let profile = raw.metadata.model.as_deref().and_then(crate::camera_profiles::get);
    let colour = profile.as_ref().and_then(|p| Some((p.matrix().mul(&transform.matrix.inverse()?), p.hue_sat.clone())));
    // Sony's Dynamic Range Optimizer (on by default) brightens the camera JPEG's darker regions,
    // not the raw: keep the colour fitted to the JPEG, take the tone curve without DRO.
    let dro = lightcraft_raw::embedded_preview_dynamic_range_optimized(bytes) == Some(true);
    let look = match fit_look_with(&sensor, &reference, &clipped, colour.clone(), dro) {
        Some(look) => look,
        // A black-and-white JPEG has no colour to fit: the profile's colour and the JPEG's tone curve
        // (`monochrome_look` does it without a profile).
        None if colour.is_some() && black_and_white => {
            let unclipped = without_clipped(&sensor, &clipped);
            let bright = HighlightCheck { pairs: highlights(&sensor, &reference), dro, grey: true };
            let Some(look) = fit_tone_only_with(unclipped.as_ref().unwrap_or(&sensor), &reference, colour, &bright) else { return (None, true) };
            look
        }
        None => return (None, black_and_white),
    };
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
    (Some(look), black_and_white)
}

/// Sensor level (normalised, white = 1) at or above which a raw channel counts as clipped, as in
/// the render's highlight reconstruction.
pub(super) const SENSOR_CLIP: f32 = 0.99;

/// [`fit_look_with`] for a photo without Sony's DRO.
#[cfg(test)]
pub(super) fn fit_look(sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool], colour: Option<(Mat3, Option<HsvTable>)>) -> Option<CameraLook> {
    fit_look_with(sensor, reference, clipped, colour, false)
}

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
pub(super) fn fit_look_with(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    clipped: &[bool],
    colour: Option<(Mat3, Option<HsvTable>)>,
    dro: bool,
) -> Option<CameraLook> {
    let unclipped = without_clipped(sensor, clipped);
    // from the proxies as they are: the retries' filters (clipped pixels, edges) don't remove them
    let bright = HighlightCheck { pairs: highlights(sensor, reference), dro, grey: false };
    let Some((look, attempt)) = search_ordered(sensor, reference, colour.clone(), TONE_FITS, &bright) else {
        return unclipped
            .as_ref()
            .and_then(|u| search_ordered(u, reference, colour, TONE_FITS, &bright))
            .map(|(look, _)| look)
            .or_else(|| fit_partial(unclipped.as_ref().unwrap_or(sensor), reference, &bright));
    };
    let Some(unclipped) = unclipped else { return Some(look) };
    let refit = fit_attempt(&unclipped, reference, &colour, attempt, &bright);
    if refit.is_none() && lightcraft_pipeline::profiling() {
        eprintln!("[profile] camera look: no fit without the clipped pixels on the accepted attempt, keeping the fit on all pixels");
    }
    Some(refit.unwrap_or(look))
}

/// `sensor` with its clipped pixels set to NaN (left out of every training pair), or `None` when
/// none is clipped.
pub(super) fn without_clipped(sensor: &Rgb32f, clipped: &[bool]) -> Option<Rgb32f> {
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
pub(super) fn clip_mask(camera: &Rgb32f) -> Rgb32f {
    let mut mask = camera.clone();
    mask.map_in_place(|p| if p.iter().any(|v| *v >= SENSOR_CLIP) { [1.0; 3] } else { [0.0; 3] });
    mask
}

/// Sony's global tone curve with the Dynamic Range Optimizer off (Standard creative style), pooled
/// from 12 CC0 raw.pixls.us ARWs of 10 interchangeable-lens bodies and their camera JPEGs; the file
/// lists them and how the curve was derived (`assets/ATTRIBUTION.md`). Pooled the same way from 55
/// private ILCE-7CR photos with DRO off, the curve differs by at most 1.2 L* between scene luminance
/// 0.01 and 0.5. Issue #244, `docs/camera-preview-colour.md`.
const SONY_DRO_OFF_TONE: &str = include_str!("../../../../assets/camera-tone/sony-dro-off.json");

/// [`SONY_DRO_OFF_TONE`]'s curve (`None` only if the built-in file were invalid; a test checks it).
pub(super) fn sony_dro_off_tone() -> Option<CameraTone> {
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
pub(super) fn with_dro_off_tone(look: CameraLook) -> CameraLook {
    CameraLook { tone: dro_off(&look.tone), ..look }
}

/// The curve of [`with_dro_off_tone`]: `own` lowered to Sony's without DRO wherever that is darker,
/// or `own` unchanged when it can't be.
pub(super) fn dro_off(own_tone: &CameraTone) -> CameraTone {
    let Some(dro_off) = sony_dro_off_tone() else { return *own_tone };
    let own = own_tone.knots();
    let mut knots = own.map(|[x, y]| [x, y.min(dro_off.apply(x))]);
    // Past its last knot a curve goes on as a shoulder whose rate comes from its last two knots
    // (`CameraTone::apply`): lowering the second-last knot more than the last steepens it, and the
    // extended highlights would end up above the fitted curve. Raise the second-last knot (never
    // above the fitted curve) until the rate is at most the fitted curve's.
    let ([a, b], last) = ([own[30], own[31]], knots[31]);
    let rate = ((b[1] - a[1]) / (b[0] - a[0])).clamp(0.1, 16.0) / (1.0 - b[1]).max(0.01);
    let floor = last[1] - rate * (1.0 - last[1]).max(0.01) * (b[0] - a[0]);
    knots[30][1] = knots[30][1].max(floor).min(last[1]);
    let Some(tone) = CameraTone::new(knots).and_then(|tone| tone.with_chroma(*own_tone.chroma())) else { return *own_tone };
    if !at_most(&tone, own_tone) {
        return *own_tone;
    }
    tone
}

/// Whether curve `a` is nowhere brighter than `b` over the scene luminances the finish stage's tone
/// table covers ([`ToneMap::camera`]).
pub(super) fn at_most(a: &CameraTone, b: &CameraTone) -> bool {
    use lightcraft_pipeline::tone::{GREY, LUT_MAX_EV, LUT_MIN_EV, LUT_N};
    (0..LUT_N).all(|i| {
        let x = GREY * 2f32.powf(LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32);
        a.apply(x) <= b.apply(x) + 1e-5
    })
}

/// Pixel cap for the embedded JPEG decoded for the fit. The codec checks it before decoding and
/// decodes at full size, so a 66.5 MP full-frame ILCE-7RM6 JPEG transiently needs ~200 MB (3 B/px);
/// 128 MP also admits 100 MP medium-format previews.
pub(super) const FIT_PREVIEW_MAX_PIXELS: u64 = 128_000_000;

/// A DNG's tone and chroma curves fitted to its own camera JPEG ([`dng_tone_eligible`]); the file's colour model
/// (matrix, profile tables, white balance) stays as it is: the proxy goes through the file's tables and the given
/// colour of the search is the identity. Same held-out acceptance gates as [`fit_look`]; the preview must not be a
/// thumbnail ([`MIN_PREVIEW_FRACTION`]), must show the same picture ([`structure_agrees`]), and the curve must not
/// clip more highlights than the plain render ([`clips_more`]).
pub(super) fn fit_dng_tone(
    raw: &RawImage,
    bytes: &[u8],
    transform: &CameraTransform,
    lens: Option<&lightcraft_develop::EmbeddedLens>,
) -> Option<CameraLook> {
    // Logged under LIGHTCRAFT_PROFILE so a corpus run can count the outcome per gate.
    let refuse = |why: &str| {
        if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
            eprintln!("DNGTONE refused {why}");
        }
        None
    };
    let Some((mut sensor, reference, clipped)) = proxies(raw, bytes, transform, PROXY, MIN_PREVIEW_FRACTION, None, None) else {
        return refuse("preview");
    };
    // As the loader renders it: the profile's tables around the baseline exposure.
    let xy = lightcraft_raw::color::as_shot_white_xy(raw);
    if let Some(tables) = lightcraft_raw::profile::ProfileTables::new(&raw.color.profile, lightcraft_raw::color::illuminant_weight(&raw.color, xy)) {
        let gain = 2f32.powf(transform.baseline_exposure as f32);
        sensor.map_in_place(|p| tables.apply(p.map(|v| v / gain), gain));
    }
    correct_vignetting(&mut sensor, lens, raw.orientation);
    if (sensor.width, sensor.height) != (reference.width, reference.height) {
        return refuse("preview");
    }
    if !structure_agrees(&sensor, &reference) {
        return refuse("structure");
    }
    let unclipped = without_clipped(&sensor, &clipped);
    let fit_on = unclipped.as_ref().unwrap_or(&sensor);
    let colour = Some((Mat3::IDENTITY, None));
    let bright = HighlightCheck { pairs: highlights(&sensor, &reference), dro: false, grey: false };
    let look = TONE_FITS.iter().find_map(|&tone| {
        [false, true].into_iter().find_map(|away_from_edges| {
            fit_attempt(fit_on, &reference, &colour, Attempt { tone, profile: true, away_from_edges, dark: true }, &bright)
        })
    });
    let Some(look) = look else { return refuse("fit") };
    let plain = match raw.color.profile.tone_curve.as_ref().and_then(crate::files::dng_tone_curve) {
        Some(curve) => ToneMap::camera(&curve, 0.0, 0.0, 0.0),
        None => ToneMap::new(0.0, 0.0, 0.0),
    };
    let fitted = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
    if clips_more(&sensor, &reference, &fitted, &plain) {
        return refuse("clip");
    }
    let base = look.clone();
    let fine = std::cell::OnceCell::new();
    let fine_proxies = || {
        fine.get_or_init(|| {
            let (mut fine_sensor, fine_reference, fine_clipped) = proxies(raw, bytes, transform, FINE_PROXY, MIN_PREVIEW_FRACTION, None, None)?;
            if let Some(tables) =
                lightcraft_raw::profile::ProfileTables::new(&raw.color.profile, lightcraft_raw::color::illuminant_weight(&raw.color, xy))
            {
                let gain = 2f32.powf(transform.baseline_exposure as f32);
                fine_sensor.map_in_place(|p| tables.apply(p.map(|v| v / gain), gain));
            }
            correct_vignetting(&mut fine_sensor, lens, raw.orientation);
            Some((fine_sensor, fine_reference, fine_clipped))
        })
        .clone()
    };
    let fine_pair = || fine_proxies().map(|(sensor, reference, clipped)| (without_clipped(&sensor, &clipped).unwrap_or(sensor), reference));
    let proxy = dng_colour_proxy(raw, lens);
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] DNG colour proxy: {proxy:?}");
    }
    let look = with_hue_term(look, &sensor, &reference, &clipped, fine_pair, proxy);
    let look = with_band_term(look, &sensor, &reference, &clipped, &fine_pair, proxy);
    let look = validate_dng_colour_terms(base, look, &sensor, &reference, &plain);
    if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
        eprintln!("DNGTONE accepted");
    }
    Some(look)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DngColourProxy {
    Faithful,
    ScalarVignette,
    DeferredGeometry,
    VignetteWithProfileTables,
    VignetteNotProvenPositive,
}

impl DngColourProxy {
    pub(super) fn homogeneous_terms(self) -> bool {
        matches!(self, Self::Faithful | Self::ScalarVignette)
    }

    pub(super) fn scene_luminance_terms(self) -> bool {
        self == Self::Faithful
    }
}

pub(super) fn dng_colour_proxy(raw: &RawImage, lens: Option<&lightcraft_develop::EmbeddedLens>) -> DngColourProxy {
    use lightcraft_raw::opcodes::Opcode;
    let geometry = raw
        .opcodes
        .list3
        .iter()
        .any(|op| matches!(op, Opcode::WarpRectilinear { .. } | Opcode::WarpRectilinear2 { .. } | Opcode::WarpFisheye { .. }))
        || lens.is_some_and(|l| l.warp.is_some() || l.has_stages());
    if geometry {
        return DngColourProxy::DeferredGeometry;
    }
    let coefficients: Vec<&[f64; 5]> = raw
        .opcodes
        .list3
        .iter()
        .filter_map(|op| match op {
            Opcode::FixVignetteRadial { k, .. } => Some(k),
            _ => None,
        })
        .chain(lens.and_then(|l| l.vignette.as_ref()).map(|v| &v.k))
        .collect();
    if coefficients.is_empty() {
        // GainMap is deliberately not deferred by the loader: even per-channel maps run
        // in raw development on both paths, before the camera colour model.
        return DngColourProxy::Faithful;
    }
    if coefficients.iter().any(|k| k.iter().any(|v| !v.is_finite() || *v < 0.0)) {
        return DngColourProxy::VignetteNotProvenPositive;
    }
    if raw.color.profile.hue_sat_map.iter().any(Option::is_some) || raw.color.profile.look_table.is_some() {
        return DngColourProxy::VignetteWithProfileTables;
    }
    DngColourProxy::ScalarVignette
}

/// Share of the finer proxy's median ΔE76 a colour term must remove.
const HUE_TERM_FINE_MARGIN: f64 = 0.03;

/// Edge of the finer proxy pair a colour term is checked against.
const FINE_PROXY: usize = 768;

/// Smallest share of the held-out error (median ΔE76 of the displayed colour to the camera JPEG's) a
/// hue/saturation table must remove, in each of the two folds, to be kept.
const HUE_TERM_MARGIN: f64 = 0.03;

/// CIELAB (D65 white) of a linear Rec.2020 colour.
fn lab_from_2020(p: [f64; 3]) -> [f64; 3] {
    let xyz = REC2020.to_xyz().apply(p.map(|v| v.max(0.0)));
    let white = [0.950_47, 1.0, 1.088_83];
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (x, y, z) = (f(xyz[0] / white[0]), f(xyz[1] / white[1]), f(xyz[2] / white[2]));
    [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
}

/// Median CIELAB ΔE76 between what `look` displays for the scene pairs and the camera JPEG's colour (the median, as
/// the measure of a render against its camera JPEG is: a few saturated or clipped outliers don't count).
pub(super) fn display_error(pairs: &[([f64; 3], [f64; 3])], look: &CameraLook) -> f64 {
    let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
    let correction = look.hue_sat.as_ref().and_then(HueSat::new);
    let mut distances = Vec::with_capacity(pairs.len());
    for (x, y) in pairs {
        let p = look.matrix.apply(*x);
        let p = correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from));
        let p = look.bands.as_ref().map_or(p, |b| b.apply(p.map(|v| v as f32)).map(f64::from));
        let (a, b) = (lab_from_2020(displayed(p, &tone)), lab_from_2020(*y));
        distances.push((0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt());
    }
    median(&mut distances).unwrap_or(0.0)
}

pub(super) struct ProxyPairSets {
    pub(super) all: (Pairs, Pairs),
    pub(super) folds: [(Pairs, Pairs); 2],
}

pub(super) fn spatial_pair_sets(sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool], edge_limit: Option<f32>) -> Option<ProxyPairSets> {
    let unclipped = without_clipped(sensor, clipped);
    let sensor = unclipped.as_ref().unwrap_or(sensor);
    let all = collect_pairs(sensor, reference, 0.005, edge_limit, true)?;
    let half = sensor.height / 2;
    let fold = |start, height| {
        collect_pairs(
            &sensor.clone().into_crop(0, start, sensor.width, height),
            &reference.clone().into_crop(0, start, reference.width, height),
            0.005,
            edge_limit,
            true,
        )
    };
    Some(ProxyPairSets { all, folds: [fold(0, half)?, fold(half, sensor.height - half)?] })
}

/// `look` completed with a hue/saturation table (the DNG hue/saturation map's form, applied after the file's own
/// tables) fitted to the camera JPEG: what the file's colour model leaves of the camera's per-hue colour rendering,
/// a hue shift and a saturation gain per hue node. Two resolutions are tried ([`TableGrid::SMOOTH`], few parameters,
/// and [`TableGrid::FINE`]). Two folds (top and bottom half of the proxy): a table fitted on one, with the chroma
/// curve refitted for it, must beat the look without it on the other by [`HUE_TERM_MARGIN`] in both; the better
/// resolution is then fitted on everything. Otherwise `look` is returned as it is.
pub(super) fn with_hue_term(
    look: CameraLook,
    sensor: &Rgb32f,
    reference: &Rgb32f,
    clipped: &[bool],
    fine: impl FnOnce() -> Option<(Rgb32f, Rgb32f)>,
    proxy: DngColourProxy,
) -> CameraLook {
    if !proxy.homogeneous_terms() {
        return look;
    }
    let Some(training) = spatial_pair_sets(sensor, reference, clipped, None) else { return look };
    let (pairs, bright) = &training.all;
    let folds = &training.folds;
    let with_table = |grid: &TableGrid, train: &[([f64; 3], [f64; 3])], train_bright: &[([f64; 3], [f64; 3])]| -> Option<CameraLook> {
        // the chroma curve is fitted afresh for the table (it is the curve's remaining error that the fit measures)
        let flat = look.tone.with_chroma([1.0; lightcraft_pipeline::tone::CHROMA_N])?;
        let mut next = CameraLook { hue_sat: Some(fit_hue_sat_grid(train, &look.matrix, grid)?), tone: flat, ..look.clone() };
        if let Some(tone) = fit_chroma(train_bright, &next) {
            next.tone = tone;
        }
        Some(next)
    };
    let mut best: Option<(f64, &TableGrid)> = None;
    for (name, grid) in [("smooth", &TableGrid::SMOOTH), ("fine", &TableGrid::FINE)] {
        if grid.vals > 1 && !proxy.scene_luminance_terms() {
            continue;
        }
        let mut gains = [0.0; 2];
        for (fold, gain) in gains.iter_mut().enumerate() {
            let (train, train_bright) = &folds[fold];
            let (_, test) = &folds[1 - fold];
            let Some(next) = with_table(grid, train, train_bright) else { continue };
            let before = display_error(test, &look);
            *gain = if before > 0.0 { 1.0 - display_error(test, &next) / before } else { 0.0 };
        }
        let worst = gains[0].min(gains[1]);
        if lightcraft_pipeline::profiling() {
            eprintln!("[profile] DNG hue term ({name}): held-out error change {:+.1}% / {:+.1}%", -100.0 * gains[0], -100.0 * gains[1]);
        }
        if worst >= HUE_TERM_MARGIN && best.is_none_or(|(b, _)| worst > b) {
            best = Some((worst, grid));
        }
    }
    // A coarse improvement is insufficient without an independent, higher-resolution check.
    if best.is_none() {
        return look;
    }
    let Some((fine_sensor, fine_reference)) = fine() else { return look };
    if fine_sensor.width.max(fine_sensor.height) < 4 * PROXY {
        return look;
    }
    let Some((_, fine_bright)) = collect_pairs(&fine_sensor, &fine_reference, 0.005, None, true) else { return look };
    let next = best.and_then(|(_, grid)| with_table(grid, pairs, bright));
    let next = next.filter(|next| {
        let fine_bright = &fine_bright;
        let (before, after) = (display_error(fine_bright, &look), display_error(fine_bright, next));
        if lightcraft_pipeline::profiling() {
            eprintln!("[profile] DNG hue term on the finer proxy: median ΔE76 {before:.2} -> {after:.2}");
        }
        after < before * (1.0 - HUE_TERM_FINE_MARGIN)
    });
    match next {
        Some(next) => {
            if lightcraft_pipeline::profiling() {
                eprintln!(
                    "[profile] DNG hue term: kept (median ΔE76 on the proxy {:.2} -> {:.2})",
                    display_error(bright, &look),
                    display_error(bright, &next)
                );
            }
            next
        }
        None => look,
    }
}

/// Display lightness (CIELAB L*) of the nodes of [`ChromaBands`].
const BAND_LIGHTNESS: [f64; BAND_NODES] = [8.0, 20.0, 35.0, 50.0, 65.0, 80.0, 92.0];
/// Smallest share of the held-out median ΔE76 a [`ChromaBands`] term must remove, in each fold and on the finer pair.
const BAND_TERM_MARGIN: f64 = 0.03;

/// `(r/y - 1, b/y - 1)` of a colour, `None` when it has no luminance.
fn normalised_chroma(p: [f64; 3]) -> Option<[f64; 2]> {
    let y = luma(p);
    (y > 1e-6 && y.is_finite()).then(|| [p[0] / y - 1.0, p[2] / y - 1.0]).filter(|n| n.iter().all(|v| v.is_finite() && v.abs() < 4.0))
}

/// Scene luminance of each [`ChromaBands`] node: what the tone curve turns into the node's display lightness; `None`
/// when they are not increasing.
pub(super) fn band_luminance(tone: &ToneMap) -> Option<[f32; BAND_NODES]> {
    let mut luminance = [0f32; BAND_NODES];
    for (node, lightness) in luminance.iter_mut().zip(BAND_LIGHTNESS) {
        let display = if lightness > 8.0 { ((lightness + 16.0) / 116.0).powi(3) } else { lightness / 903.3 } as f32;
        let (mut lo, mut hi) = (1e-5f32, 64.0f32);
        for _ in 0..40 {
            let mid = (lo * hi).sqrt();
            if tone.apply(mid) < display { lo = mid } else { hi = mid }
        }
        *node = (lo * hi).sqrt();
    }
    luminance.windows(2).all(|w| w[1] > w[0] * 1.05).then_some(luminance)
}

/// [`ChromaBands`] fitted to scene/JPEG pairs, on top of `look` (whose colour, tone and chroma curve the pairs are
/// displayed through): per node the 2×2 matrix taking the displayed normalised chroma to the JPEG's, by ridge
/// regression toward the identity, the pairs weighted by their scene luminance's place between the nodes.
pub(super) fn fit_bands(pairs: &[([f64; 3], [f64; 3])], look: &CameraLook) -> Option<ChromaBands> {
    let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
    let correction = look.hue_sat.as_ref().and_then(HueSat::new);
    let probe = ChromaBands::identity(band_luminance(&tone)?);
    // per node: sums of (n_p n_pᵀ) as [00, 01, 11], of (n_t n_pᵀ) as [00, 01, 10, 11], and the weight
    let mut gram = [[0.0f64; 3]; BAND_NODES];
    let mut cross = [[0.0f64; 4]; BAND_NODES];
    for (x, y) in pairs {
        let p = look.matrix.apply(*x);
        let p = correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from));
        let scene = luma(p);
        let (Some(np), Some(nt)) = (normalised_chroma(displayed(p, &tone)), normalised_chroma(*y)) else { continue };
        if scene <= 0.0 || !scene.is_finite() {
            continue;
        }
        let (j, t) = probe.locate(scene as f32);
        let t = f64::from(t);
        for (node, w) in [(j, 1.0 - t), (j + 1, t)] {
            if w <= 0.0 {
                continue;
            }
            let (g, c) = gram.get_mut(node).zip(cross.get_mut(node))?;
            g[0] += w * np[0] * np[0];
            g[1] += w * np[0] * np[1];
            g[2] += w * np[1] * np[1];
            c[0] += w * nt[0] * np[0];
            c[1] += w * nt[0] * np[1];
            c[2] += w * nt[1] * np[0];
            c[3] += w * nt[1] * np[1];
        }
    }
    let mut matrices = [[1.0f32, 0.0, 0.0, 1.0]; BAND_NODES];
    for (m, (g, c)) in matrices.iter_mut().zip(gram.iter().zip(&cross)) {
        // ridge toward the identity: an absolute part (few or colourless pixels) and a share of the node's own energy
        let ridge = 2.0 + 0.05 * (g[0] + g[2]) / 2.0;
        let (a00, a01, a11) = (g[0] + ridge, g[1], g[2] + ridge);
        let det = a00 * a11 - a01 * a01;
        if det.abs() <= 1e-9 || !det.is_finite() {
            continue;
        }
        let inverse = [a11 / det, -a01 / det, -a01 / det, a00 / det];
        let (c00, c01, c10, c11) = (c[0] + ridge, c[1], c[2], c[3] + ridge);
        let solved = [
            c00 * inverse[0] + c01 * inverse[2],
            c00 * inverse[1] + c01 * inverse[3],
            c10 * inverse[0] + c11 * inverse[2],
            c10 * inverse[1] + c11 * inverse[3],
        ];
        if !solved.iter().all(|v| v.is_finite()) {
            return None;
        }
        *m = solved.map(|v| v.clamp(-1.0, 2.5) as f32);
    }
    Some(ChromaBands { matrices, ..probe })
}

/// Smallest share of the held-out median ΔE76 a hue or offset term of [`ChromaBands`] must remove, in each fold and on
/// the finer pair.
const CHROMA_TERM_MARGIN: f64 = 0.03;
/// Normalised chroma below which a pixel says nothing about a hue.
const HUE_MIN_CHROMA: f64 = 0.02;
/// Allowed real component and magnitude of the imaginary component of each complex hue factor.
const HUE_GAIN_RANGE: (f64, f64) = (0.5, 1.8);
const HUE_TURN_LIMIT: f64 = 0.3;

/// What `look` displays of a scene pair, as normalised chroma, with the target's, and where the pair's scene luminance
/// lies between the nodes of `nodes`, the tone map's chroma scale there, and whether a scene channel was negative
/// before display clamping: `(node, position, displayed, target, scale, negative)`.
fn chroma_samples(pairs: &[([f64; 3], [f64; 3])], look: &CameraLook, nodes: &ChromaBands) -> Vec<(usize, f64, [f64; 2], [f64; 2], f64, bool)> {
    let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
    let correction = look.hue_sat.as_ref().and_then(HueSat::new);
    let mut out = Vec::with_capacity(pairs.len());
    for (x, y) in pairs {
        let p = look.matrix.apply(*x);
        let p = correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from));
        let p = look.bands.as_ref().map_or(p, |b| b.apply(p.map(|v| v as f32)).map(f64::from));
        let negative = p.iter().any(|v| *v < 0.0);
        let scene = luma(p);
        let (Some(np), Some(nt)) = (normalised_chroma(displayed(p, &tone)), normalised_chroma(*y)) else { continue };
        if scene <= 0.0 || !scene.is_finite() {
            continue;
        }
        let (j, t) = nodes.locate(scene as f32);
        let o = tone.apply(scene as f32);
        out.push((j, f64::from(t), np, nt, f64::from(tone.chroma_scale(o)), negative));
    }
    out
}

/// `look`'s bands (or the identity) with a gain and rotation of the chroma per hue fitted to the pairs: per hue node
/// the complex factor taking the displayed chroma to the JPEG's, by ridge regression toward 1.
pub(super) fn fit_hue_chroma(pairs: &[([f64; 3], [f64; 3])], look: &CameraLook) -> Option<ChromaBands> {
    let base = look.bands.clone().map_or_else(|| band_luminance(&ToneMap::camera(&look.tone, 0.0, 0.0, 0.0)).map(ChromaBands::identity), Some)?;
    // per node: Σ|n_p|², Σ n_p·n_t, Σ n_p×n_t
    let mut sums = [[0.0f64; 3]; HUE_NODES];
    for (_, _, np, nt, _, _) in chroma_samples(pairs, look, &base) {
        let energy = np[0] * np[0] + np[1] * np[1];
        if energy < HUE_MIN_CHROMA * HUE_MIN_CHROMA {
            continue;
        }
        let position = (np[1].atan2(np[0]) / std::f64::consts::TAU).rem_euclid(1.0) * HUE_NODES as f64;
        let j = (position.floor() as usize).min(HUE_NODES - 1);
        let t = position - j as f64;
        for (node, w) in [(j, 1.0 - t), ((j + 1) % HUE_NODES, t)] {
            let s = sums.get_mut(node)?;
            s[0] += w * energy;
            s[1] += w * (np[0] * nt[0] + np[1] * nt[1]);
            s[2] += w * (np[0] * nt[1] - np[1] * nt[0]);
        }
    }
    let mut table = [[1.0f32, 0.0]; HUE_NODES];
    for (node, s) in table.iter_mut().zip(&sums) {
        let ridge = 0.5 + 0.05 * s[0];
        let (gain, turn) = ((s[1] + ridge) / (s[0] + ridge), s[2] / (s[0] + ridge));
        if !(gain.is_finite() && turn.is_finite()) {
            return None;
        }
        // Refuse extreme gain or rotation rather than fitting a large colour mismatch.
        if !(HUE_GAIN_RANGE.0..=HUE_GAIN_RANGE.1).contains(&gain) || turn.abs() > HUE_TURN_LIMIT {
            return None;
        }
        *node = [gain as f32, turn as f32];
    }
    Some(ChromaBands { hue: Some(table), ..base })
}

/// `look`'s bands with a normalised-chroma offset per luminance node fitted to the pairs (the mean remaining difference
/// between the JPEG's chroma and the displayed one, shrunk toward 0).
pub(super) fn fit_chroma_offsets(pairs: &[([f64; 3], [f64; 3])], look: &CameraLook) -> Option<ChromaBands> {
    let base = look.bands.clone().map_or_else(|| band_luminance(&ToneMap::camera(&look.tone, 0.0, 0.0, 0.0)).map(ChromaBands::identity), Some)?;
    let mut sums = [[0.0f64; 3]; BAND_NODES];
    for (j, t, np, nt, k, negative) in chroma_samples(pairs, look, &base) {
        // the offset is added in scene space, before the tone map whose chroma scale multiplies it
        if negative || !(k.is_finite() && k > 0.05) {
            continue;
        }
        for (node, w) in [(j, 1.0 - t), (j + 1, t)] {
            let s = sums.get_mut(node)?;
            s[0] += w;
            s[1] += w * (nt[0] - np[0]) / k;
            s[2] += w * (nt[1] - np[1]) / k;
        }
    }
    let mut offsets = [[0.0f32; 2]; BAND_NODES];
    for (o, s) in offsets.iter_mut().zip(&sums) {
        let ridge = 2.0 + 0.05 * s[0];
        let v = [s[1] / (s[0] + ridge), s[2] / (s[0] + ridge)];
        if !v.iter().all(|c| c.is_finite()) {
            return None;
        }
        *o = v.map(|c| c.clamp(-0.15, 0.15) as f32);
    }
    Some(ChromaBands { offsets: Some(offsets), ..base })
}

/// Keep each chroma term only when it improves both spatial holdouts and a mandatory finer pair.
pub(super) fn with_chroma_stage(
    look: CameraLook,
    training: &ProxyPairSets,
    fine: &dyn Fn() -> Option<(Rgb32f, Rgb32f)>,
    edge_limit: Option<f32>,
    name: &str,
    margin: f64,
    fit: fn(&[([f64; 3], [f64; 3])], &CameraLook) -> Option<ChromaBands>,
) -> CameraLook {
    let mut gains = [0.0; 2];
    for (fold, gain) in gains.iter_mut().enumerate() {
        let train = &training.folds[fold].1;
        let test = &training.folds[1 - fold].1;
        let Some(bands) = fit(train, &look) else { return look };
        let next = CameraLook { bands: Some(bands), ..look.clone() };
        let before = display_error(test, &look);
        *gain = if before > 0.0 { 1.0 - display_error(test, &next) / before } else { 0.0 };
    }
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] DNG {name} term: held-out error change {:+.1}% / {:+.1}%", -100.0 * gains[0], -100.0 * gains[1]);
    }
    if gains.iter().any(|g| *g < margin) {
        return look;
    }
    let Some(bands) = fit(&training.all.1, &look) else { return look };
    let next = CameraLook { bands: Some(bands), ..look.clone() };
    let Some((fine_sensor, fine_reference)) = fine() else { return look };
    if fine_sensor.width.max(fine_sensor.height) < 4 * PROXY {
        return look;
    }
    let Some((_, fine_bright)) = collect_pairs(&fine_sensor, &fine_reference, 0.005, edge_limit, true) else { return look };
    let (before, after) = (display_error(&fine_bright, &look), display_error(&fine_bright, &next));
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] DNG {name} term on the finer proxy: median ΔE76 {before:.2} -> {after:.2}");
    }
    if after >= before * (1.0 - margin) {
        return look;
    }
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] DNG {name} term: kept");
    }
    next
}

fn with_band_term(
    look: CameraLook,
    sensor: &Rgb32f,
    reference: &Rgb32f,
    clipped: &[bool],
    fine: &dyn Fn() -> Option<(Rgb32f, Rgb32f)>,
    proxy: DngColourProxy,
) -> CameraLook {
    if !proxy.homogeneous_terms() {
        return look;
    }
    let look = if proxy.scene_luminance_terms() {
        let Some(training) = spatial_pair_sets(sensor, reference, clipped, None) else { return look };
        with_chroma_stage(look, &training, fine, None, "band", BAND_TERM_MARGIN, fit_bands)
    } else {
        look
    };
    // Remaining interpolation mismatches at edges must not teach hue or offset rendering.
    let Some(flat) = spatial_pair_sets(sensor, reference, clipped, Some(EDGE_CONTRAST)) else { return look };
    let look = with_chroma_stage(look, &flat, fine, Some(EDGE_CONTRAST), "hue-chroma", CHROMA_TERM_MARGIN, fit_hue_chroma);
    if proxy.scene_luminance_terms() {
        with_chroma_stage(look, &flat, fine, Some(EDGE_CONTRAST), "offset", CHROMA_TERM_MARGIN, fit_chroma_offsets)
    } else {
        look
    }
}

/// The proxy already carries the file's transform, baseline exposure, profile tables and vignetting.
/// Apply the remaining accepted colour stages through the actual render colour model, including
/// its nonnegative clamp: signed-luminance preservation alone does not bound that clamp's result.
pub(super) fn validate_dng_colour_terms(base: CameraLook, next: CameraLook, sensor: &Rgb32f, reference: &Rgb32f, plain: &ToneMap) -> CameraLook {
    let hue_sat = next.hue_sat.as_ref().and_then(HueSat::new);
    let colour = crate::files::CameraColourModel {
        wb: [1.0; 3],
        matrix: next.matrix,
        gain: 1.0,
        tables: None,
        hue_sat: hue_sat.as_ref(),
        bands: next.bands.as_ref(),
    };
    let mut corrected = sensor.clone();
    colour.apply(&mut corrected);
    let fitted = ToneMap::camera(&next.tone, 0.0, 0.0, 0.0);
    let preview = reference.data.iter().filter(|p| luminance_2020(**p) >= 0.98).count() as f64 / reference.data.len().max(1) as f64;
    if near_white(&corrected, &fitted) > near_white(sensor, plain).max(preview) + CLIP_TOLERANCE {
        if lightcraft_pipeline::profiling() {
            eprintln!("[profile] DNG colour terms refused: final colour model clips more highlights");
        }
        base
    } else {
        next
    }
}

pub(super) fn near_white(sensor: &Rgb32f, tone: &ToneMap) -> f64 {
    let n = sensor.data.iter().filter(|p| p.iter().all(|v| v.is_finite()) && tone.apply(luminance_2020(p.map(|v| v.max(0.0)))) >= 0.98).count();
    n as f64 / sensor.data.len().max(1) as f64
}

/// Whether the fitted tone map puts clearly more of the photo's proxy pixels at or near white than both the plain
/// render and the camera's own preview do ([`CLIP_TOLERANCE`]): a fit that clips what the camera clips too is the
/// camera's look, one that clips what neither does loses highlight detail.
pub(super) fn clips_more(sensor: &Rgb32f, reference: &Rgb32f, fitted: &ToneMap, plain: &ToneMap) -> bool {
    let preview = reference.data.iter().filter(|p| luminance_2020(**p) >= 0.98).count() as f64 / reference.data.len().max(1) as f64;
    near_white(sensor, fitted) > near_white(sensor, plain).max(preview) + CLIP_TOLERANCE
}

/// The proxies a photo's look is fitted on ([`proxies`] at [`PROXY`]), the sensor's with the embedded vignetting
/// correction the render applies. Without it the tone curve learns the camera JPEG's corrected
/// brightness from the uncorrected sensor, and the render then corrects it again: 21 X-S10 RAFs rendered up to 0.3 EV
/// brighter in their midtones and 0.6 EV in their shadows than their camera JPEGs, about 0.05 EV once corrected.
pub(super) fn look_proxies(
    raw: &RawImage,
    bytes: &[u8],
    transform: &CameraTransform,
    lens: Option<&LensGeometry>,
    embedded: Option<&lightcraft_develop::EmbeddedLens>,
) -> Option<(Rgb32f, Rgb32f, Vec<bool>)> {
    proxies(raw, bytes, transform, PROXY, 0.0, lens, embedded)
}

/// Multiply the sensor proxy (sensor orientation, default crop) by the embedded vignetting correction, the gain the
/// pipeline applies to the render (`optics::embedded_vignette_gain`; `lens` is in the oriented, cropped frame): the
/// camera JPEG has it applied, so a tone fitted without it brightens the render's corners twice.
pub(super) fn correct_vignetting(sensor: &mut Rgb32f, lens: Option<&lightcraft_develop::EmbeddedLens>, orientation: lightcraft_geom::Orientation) {
    let Some(vignette) = lens.and_then(|l| l.vignette) else { return };
    let (w, h) = (sensor.width as f64, sensor.height as f64);
    let (ow, oh) = if orientation.swaps_axes() { (h, w) } else { (w, h) };
    let width = sensor.width.max(1);
    for (i, p) in sensor.data.iter_mut().enumerate() {
        let (x, y) = orientation.map((i % width) as f64 + 0.5, (i / width) as f64 + 0.5, w, h);
        let gain = lightcraft_pipeline::optics::embedded_vignette_gain(&vignette, lightcraft_geom::Point::new(x, y), ow, oh) as f32;
        *p = p.map(|v| v * gain);
    }
}
