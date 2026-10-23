//! Estimate the starting look of a raw without a camera colour matrix (Sony ARW, Nikon NEF, Panasonic
//! RW2, Fujifilm RAF, Canon CR2/CR3, Pentax PEF, Samsung SRW, Olympus ORF) from its own JPEG. Colour and luminance are fitted separately; the JPEG supplies correspondences only,
//! never output pixels or a replacement for RAW editing.
//! A global matrix can't follow the camera's hue-dependent rendering (the best matrix rendered a
//! lime shirt olive that the camera kept lime): a hue/saturation table fitted to the residuals
//! (applied like a DNG `ProfileHueSatMap`) corrects that when it also improves the held-out pixels.
use lightcraft_color::{D50, D65, Mat3, PROPHOTO, REC2020, Xy, bradford, luminance_2020};
use lightcraft_pipeline::tone::{CameraTone, ToneMap};
use lightcraft_raster::{
    Rgb32f,
    resample::{Filter, fit, resize},
};
use lightcraft_raw::{
    ColorData, RawFormat, RawImage,
    color::CameraTransform,
    profile::{HsvTable, ProfileLook, ToneCurve},
};

/// The version of the camera-look fit: how a raw's colour matrix, hue/saturation table and tone
/// and chroma curves are chosen. Smart previews record it in their header (`look_version`), because
/// they keep the curve and the colour the fit produced; one written by an older version is rebuilt
/// from its original when that is online, and kept as it is while it is offline.
///
/// **Bump this whenever a change makes the fit give a different result for the same file** (the
/// fit, its gates, the fallbacks, the camera profiles' effect on it). Do not bump it for changes
/// that leave the fitted look alone. `RENDER_CACHE_VERSION` covers renders; this covers proxies.
///
/// 1: the fit as of #499's follow-up; 2: Sony DRO (tone curve lowered to Sony's curve without DRO)
/// and the ILCE-7CR profile (#528, #583, #568, #616); 3: the camera JPEG read in the colour space the raw's EXIF
/// states, for every make (#778).
/// 4: a tone curve from black-and-white camera JPEGs (#577).
/// 5: dark JPEG luminance pairs for the camera toe (#1307).
/// 6: the ILCA-99M2 profile.
/// 7: the sensor proxy follows the file's lens warp when the camera JPEG is lens-corrected.
/// 8: the bundled NIKON Z 6_2 profile.
/// 9: the bundled NIKON Z 8 and NIKON D5300 profiles.
/// 10: DNG tone fit for profiles with a look table and for 1024 px (Lightroom) previews.
/// 12: tone curves of low-contrast scenes (range limit 1.2, with the bright-area check for curves under 1.5, #699);
/// a tone-only fallback for near-black scenes with too few colour pairs (#1587); DNG tone fitting uses a
/// lens-corrected proxy when the file carries lens opcodes; validated A7CR distortion, including the lens data
/// stored with each smart preview.
pub const LOOK_VERSION: u32 = 12;

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
            | RawFormat::Mrw
            | RawFormat::Erf
    )
}

/// The colour a raw starts from, first available first: the file's own colour matrices (DNG); a look fitted to the
/// file's JPEG ([`fit_preview`]: with the camera profile for its model when there is one, else with colour fitted to
/// this photo alone); the model's colour matrices from measured spectral sensitivities (`lightcraft_raw::spectral`);
/// the neutral fallback. The spectral matrices only stand in for the neutral fallback, so a photo whose JPEG fit is
/// accepted renders as without them; when used they are written into `raw.color`. A photo whose JPEG is black and
/// white (a monochrome picture style) has no colour to fit, but still gives its tone curve: with the camera profile's
/// colour in [`fit_preview`], else on top of the spectral matrices or the fallback ([`monochrome_look`]). Returns the
/// as-shot white, the camera transform at it and the fitted look.
/// `lens` is the file's own lens-correction warp ([`lens_geometry`], read before the caller removes it from
/// `OpcodeList3`): the camera applied it to its JPEG, so the sensor proxy is warped the same way before pairing.
/// `embedded` is the file's embedded lens vignetting, for the DNG tone fit.
pub(crate) fn starting_colour(
    raw: &mut RawImage,
    bytes: &[u8],
    lens: Option<&LensGeometry>,
    embedded: Option<&lightcraft_develop::EmbeddedLens>,
) -> (Xy, CameraTransform, Option<CameraLook>) {
    let (xy, t, look) = starting_colour_with(
        raw,
        |raw, t| fit_preview(raw, bytes, t, lens),
        |raw, t| monochrome_look(raw, bytes, t, lens),
        |raw, t| {
            if !file_local_look(raw.format) {
                return None;
            }
            let (sensor, reference, clipped) = proxies(raw, bytes, t, PROXY, 0.0, lens)?;
            fit_dark_tone(&sensor, &reference, &clipped)
        },
    );
    if look.is_none() && dng_tone_eligible(raw, &t) {
        let look = fit_dng_tone(raw, bytes, &t, embedded);
        return (xy, t, look);
    }
    (xy, t, look)
}

/// Whether the profile's tone curve is absent or the identity (within 0.005 over the range).
fn identity_tone_curve(curve: Option<&lightcraft_raw::profile::ToneCurve>) -> bool {
    curve.is_none_or(|c| {
        (0..=32).all(|i| {
            let x = i as f32 / 32.0;
            (c.eval(x) - x).abs() < 0.005
        })
    })
}

/// A DNG with its own colour model whose tone is not (yet) defined by the file: a matrix, no
/// non-identity `ProfileToneCurve` and no `ProfileGainTableMap`. A `LookTable` does not define the tone: an
/// Adobe Standard profile carries one without a tone curve and is rendered with Lightroom's default curve on
/// top (Lightroom Classic's lossy DNGs rendered flat and dull without a fit), and the fit applies the tables
/// to its proxy, so it fits only the tone around them.
pub(crate) fn dng_tone_eligible(raw: &RawImage, t: &CameraTransform) -> bool {
    let p = &raw.color.profile;
    raw.format == RawFormat::Dng && !t.matrix_is_fallback && identity_tone_curve(p.tone_curve.as_ref()) && p.gain_table_map.is_none()
}

/// [`starting_colour`] with the JPEG fit (the look, and whether the JPEG is black and white) and the black-and-white
/// JPEG's tone fit and the near-black tone fallback passed in (tests pin the order with them).
fn starting_colour_with(
    raw: &mut RawImage,
    fit: impl FnOnce(&RawImage, &CameraTransform) -> (Option<CameraLook>, bool),
    monochrome: impl FnOnce(&RawImage, &CameraTransform) -> Option<CameraLook>,
    dark: impl FnOnce(&RawImage, &CameraTransform) -> Option<CameraLook>,
) -> (Xy, CameraTransform, Option<CameraLook>) {
    let start = |raw: &RawImage| {
        let xy = lightcraft_raw::color::as_shot_white_xy(raw);
        (xy, lightcraft_raw::color::camera_transform(raw, xy))
    };
    let (xy, t) = start(raw);
    if !t.matrix_is_fallback {
        return (xy, t, None);
    }
    let (look, black_and_white) = fit(raw, &t);
    if let Some(look) = look {
        return (xy, t, Some(look));
    }
    let (xy, t) = match raw.metadata.model.as_deref().and_then(|model| lightcraft_raw::spectral::find(raw.metadata.make.as_deref(), model)) {
        Some(camera) => {
            camera.fill(&mut raw.color);
            if lightcraft_pipeline::profiling() {
                eprintln!("[profile] {:?} camera colour from spectral sensitivities: {} {}", raw.format, camera.make, camera.model);
            }
            start(raw)
        }
        None => (xy, t),
    };
    // A black-and-white JPEG first tries its existing tone fit on the chosen colour.
    let look = if black_and_white { monochrome(raw, &t) } else { None };
    (xy, t, look.or_else(|| dark(raw, &t)))
}

/// The colour a raw starts from ([`starting_colour`]) as DNG colour tags, for a DNG of the raw's camera RGB (Photo
/// Merge, issue #965): without them a merge of CR2s, whose colour comes from a look fitted to their JPEG, rendered
/// green and dark. `None` when the file has colour matrices of its own (a DNG keeps them).
pub(crate) fn starting_colour_tags(raw: &mut RawImage, bytes: &[u8]) -> Option<ColorData> {
    if lightcraft_raw::color::has_matrix(&raw.color) {
        return None;
    }
    // the white the source shows, decided before `starting_colour` may fill in measured matrices
    let scale = crate::files::WbScaleOf::new(raw.format, &raw.color, raw.wb_multipliers, &raw.metadata);
    let white = scale.dng_white(&raw.color, lightcraft_raw::color::as_shot_white_xy(raw));
    // (a merge frame arrives without its lens warp: `lightcraft_merge` removes it before calling)
    let (_, t, look) = starting_colour(raw, bytes, None, None);
    dng_colour(&raw.color, &white, &t, look.as_ref())
}

/// Give a raw about to be written as a DNG of its own (Export as DNG, Convert / Copy as DNG) the colour it opens
/// with, when the file has no colour matrices (issue #1745: a Z 9 NEF's DNG had no `ColorMatrix1`, and other raw
/// converters refused it). The lens corrections stay in the DNG; only the fit sees the raw without them, as when
/// the photo opens.
pub(crate) fn with_starting_colour(raw: &mut RawImage, bytes: &[u8]) {
    if lightcraft_raw::color::has_matrix(&raw.color) {
        return;
    }
    let list3 = raw.opcodes.list3.clone();
    raw.opcodes.list3.retain(|op| !op.is_lens_correction());
    let tags = starting_colour_tags(raw, bytes);
    raw.opcodes.list3 = list3;
    if let Some(tags) = tags {
        raw.color = tags;
    }
}

/// DNG tags that render white-balanced camera RGB at its as-shot white as `t` with `look` on top does: one forward
/// matrix (with a colour matrix and as-shot neutral that agree with it at that white), the look's hue/saturation
/// table as `ProfileHueSatMap` and its tone curve as `ProfileToneCurve`. Not quite the same render: a DNG table
/// doesn't keep each pixel's luminance as the fitted one does ([`HueSat`]), and the chroma curve has no tag; on two
/// CC0 CR2s (EOS 550D, 40D) an HDR merge of three copies renders within mean CIE76 ΔE 1.07 and 0.47 of the photo
/// (1.47 and 0.66 without the table; 26 and 23 with no colour tags at all).
/// The colour matrices declare `declared`, the white balance the source shows ([`crate::files::WbScaleOf::dng_white`]),
/// so the DNG opens at the source's As Shot temperature and tint and a white-balance edit starts from it. Matrices
/// derived from `forward` put the declared white wherever the look and the generic camera model took it: ARW merges
/// opened at about 7000 K / +115 where the ARWs show 4800 K / +8, and any white-balance edit turned them green.
/// `forward` is used only with the as-shot neutral, so the render at As Shot doesn't depend on them.
/// `None` when the white balance or the matrix can't give tags.
pub(crate) fn dng_colour(tags: &ColorData, declared: &crate::files::DngWhite, t: &CameraTransform, look: Option<&CameraLook>) -> Option<ColorData> {
    let m = look.map_or(t.matrix, |l| l.matrix.mul(&t.matrix));
    // the DNG model renders the camera's white at unit brightness: what a look does to white's brightness goes
    // into the baseline exposure
    let white = m.apply([1.0; 3]);
    let k = (white[0] + white[1] + white[2]) / 3.0;
    if !(k.is_finite() && k > 1e-6 && t.wb.iter().all(|w| w.is_finite() && *w > 0.0)) {
        return None;
    }
    let m = Mat3(m.0.map(|r| r.map(|v| v / k)));
    let forward = bradford(D65, D50).mul(&REC2020.to_xyz()).mul(&m);
    let wb = t.wb.map(f64::from);
    let neutral = wb.map(|w| wb.iter().cloned().fold(f64::MAX, f64::min) / w);
    let (illuminant, color_matrix) = match declared {
        crate::files::DngWhite::Matrices(c) if lightcraft_raw::color::has_matrix(c) => (c.illuminant, c.color_matrix),
        crate::files::DngWhite::Matrices(_) => ([21, 0], [Some(declaring(t.white_xy, neutral)?), None]),
        crate::files::DngWhite::At(xy) => ([21, 0], [Some(declaring(*xy, neutral)?), None]),
    };
    if !color_matrix.iter().flatten().all(|m| m.0.iter().flatten().all(|v| v.is_finite())) {
        return None;
    }
    Some(ColorData {
        illuminant,
        color_matrix,
        // a second colour matrix comes with a second forward matrix (DNG spec): the same one, fitted at one white
        forward_matrix: [Some(forward), color_matrix[1].map(|_| forward)],
        camera_calibration: [None, None],
        analog_balance: None,
        as_shot_neutral: Some(neutral),
        as_shot_white_xy: None,
        baseline_exposure: t.baseline_exposure + k.log2(),
        baseline_sharpness: tags.baseline_sharpness,
        kelvin_presets: None,
        profile: ProfileLook {
            hue_sat_map: [look.and_then(|l| l.hue_sat.clone()), None],
            tone_curve: look.and_then(|l| dng_tone_curve(&l.tone)),
            ..Default::default()
        },
    })
}

/// An XYZ → camera colour matrix under which camera `neutral` is white `xy`: the generic camera model, scaled per
/// channel. `None` when the model gives no positive neutral there.
fn declaring(xy: Xy, neutral: [f64; 3]) -> Option<Mat3> {
    let g = lightcraft_raw::color::xyz_to_camera(&ColorData::default(), xy);
    let n = g.apply(xy.to_xyz());
    if !n.iter().all(|v| v.is_finite() && *v > 1e-9) {
        return None;
    }
    Some(Mat3::diag(neutral[0] / n[0], neutral[1] / n[1], neutral[2] / n[2]).mul(&g))
}

/// A camera tone curve as a DNG `ProfileToneCurve` (linear in, linear out, 1 = white): sampled densely over the
/// 12 stops below white, the range the curve is read back over (`files::dng_tone_curve`).
fn dng_tone_curve(tone: &CameraTone) -> Option<ToneCurve> {
    const N: usize = 256;
    let samples = (0..=N).map(|i| {
        let x = 2f32.powf(-12.0 + 12.0 * i as f32 / N as f32);
        [x, tone.apply(x).min(1.0)]
    });
    let flat: Vec<f64> = std::iter::once([0.0, 0.0]).chain(samples).flatten().map(f64::from).collect();
    ToneCurve::from_tag(&flat)
}

/// The look fitted to the file's camera JPEG, and whether that JPEG is black and white ([`monochrome`]).
pub(crate) fn fit_preview(raw: &RawImage, bytes: &[u8], transform: &CameraTransform, lens: Option<&LensGeometry>) -> (Option<CameraLook>, bool) {
    if !transform.matrix_is_fallback || !file_local_look(raw.format) {
        return (None, false);
    }
    let Some((sensor, reference, clipped)) = proxies(raw, bytes, transform, PROXY, 0.0, lens) else { return (None, false) };
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
const SENSOR_CLIP: f32 = 0.99;

/// [`fit_look_with`] for a photo without Sony's DRO.
#[cfg(test)]
fn fit_look(sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool], colour: Option<(Mat3, Option<HsvTable>)>) -> Option<CameraLook> {
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
fn fit_look_with(sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool], colour: Option<(Mat3, Option<HsvTable>)>, dro: bool) -> Option<CameraLook> {
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
    CameraLook { tone: dro_off(&look.tone), ..look }
}

/// The curve of [`with_dro_off_tone`]: `own` lowered to Sony's without DRO wherever that is darker,
/// or `own` unchanged when it can't be.
fn dro_off(own_tone: &CameraTone) -> CameraTone {
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
fn at_most(a: &CameraTone, b: &CameraTone) -> bool {
    use lightcraft_pipeline::tone::{GREY, LUT_MAX_EV, LUT_MIN_EV, LUT_N};
    (0..LUT_N).all(|i| {
        let x = GREY * 2f32.powf(LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32);
        a.apply(x) <= b.apply(x) + 1e-5
    })
}

/// Pixel cap for the embedded JPEG decoded for the fit. The codec checks it before decoding and
/// decodes at full size, so a 66.5 MP full-frame ILCE-7RM6 JPEG transiently needs ~200 MB (3 B/px);
/// 128 MP also admits 100 MP medium-format previews.
const FIT_PREVIEW_MAX_PIXELS: u64 = 128_000_000;

/// A DNG's tone and chroma curves fitted to its own camera JPEG ([`dng_tone_eligible`]); the file's colour model
/// (matrix, profile tables, white balance) stays as it is: the proxy goes through the file's tables and the given
/// colour of the search is the identity. Same held-out acceptance gates as [`fit_look`]; the preview must not be a
/// thumbnail ([`MIN_PREVIEW_FRACTION`]), must show the same picture ([`structure_agrees`]), and the curve must not
/// clip more highlights than the plain render ([`clips_more`]).
fn fit_dng_tone(raw: &RawImage, bytes: &[u8], transform: &CameraTransform, lens: Option<&lightcraft_develop::EmbeddedLens>) -> Option<CameraLook> {
    // Logged under LIGHTCRAFT_PROFILE so a corpus run can count the outcome per gate.
    let refuse = |why: &str| {
        if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
            eprintln!("DNGTONE refused {why}");
        }
        None
    };
    let Some((mut sensor, reference, clipped)) = proxies(raw, bytes, transform, PROXY, MIN_PREVIEW_FRACTION, None) else { return refuse("preview") };
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
    if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
        eprintln!("DNGTONE accepted");
    }
    Some(look)
}

/// Whether the fitted tone map puts clearly more of the photo's proxy pixels at or near white than both the plain
/// render and the camera's own preview do ([`CLIP_TOLERANCE`]): a fit that clips what the camera clips too is the
/// camera's look, one that clips what neither does loses highlight detail.
fn clips_more(sensor: &Rgb32f, reference: &Rgb32f, fitted: &ToneMap, plain: &ToneMap) -> bool {
    let near_white = |tone: &ToneMap| {
        let n = sensor.data.iter().filter(|p| p.iter().all(|v| v.is_finite()) && tone.apply(luminance_2020(p.map(|v| v.max(0.0)))) >= 0.98).count();
        n as f64 / sensor.data.len().max(1) as f64
    };
    let preview = reference.data.iter().filter(|p| luminance_2020(**p) >= 0.98).count() as f64 / reference.data.len().max(1) as f64;
    near_white(fitted) > near_white(plain).max(preview) + CLIP_TOLERANCE
}

/// Multiply the sensor proxy (sensor orientation, default crop) by the embedded vignetting correction, the gain the
/// pipeline applies to the render (`optics::embedded_vignette_gain`; `lens` is in the oriented, cropped frame): the
/// camera JPEG has it applied, so a tone fitted without it brightens the render's corners twice.
fn correct_vignetting(sensor: &mut Rgb32f, lens: Option<&lightcraft_develop::EmbeddedLens>, orientation: lightcraft_geom::Orientation) {
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

/// The embedded preview's long edge must be at least this share of the raw's (and 384 px): a DNG whose preview is a
/// small thumbnail is not fitted. The fit itself runs on a proxy of at most 384 px, so the 1024 px preview
/// Lightroom writes into its DNGs (15% of a 33 MP raw's long edge) is plenty.
const MIN_PREVIEW_FRACTION: f64 = 0.1;

/// Lowest preview luminance and channel value a tone-only fit trains on (the other fits use 0.015 and 0.004): a dark
/// night scene has most of its pixels below 0.015, and a curve fitted without them overshoots its shadows.
const DARK_FLOOR: (f32, f32) = (0.004, 0.001);

/// Share of the proxy by which the fitted curve may clip more highlights than the plain render and the camera's
/// preview (chosen on the CC0 and user DNGs from the clipped share of their 1440 px renders: it refuses the fits
/// that clip about 1% more than the camera does and passes the ones that clip like the camera).
const CLIP_TOLERANCE: f64 = 0.001;

/// Log-luminance correlation (5 × 5 box blurred) the unshifted proxies must reach.
const MIN_STRUCTURE: f64 = 0.85;

/// Whether the sensor proxy and the camera JPEG proxy show the same picture in the same place: their log luminance
/// correlates by at least [`MIN_STRUCTURE`] unshifted, and no shift of more than one proxy pixel (within ±3)
/// correlates better.
fn structure_agrees(sensor: &Rgb32f, reference: &Rgb32f) -> bool {
    let (w, h) = (sensor.width, sensor.height);
    if (w, h) != (reference.width, reference.height) || w < 16 || h < 16 || sensor.data.len() != w * h || reference.data.len() != w * h {
        return false;
    }
    let log = |img: &Rgb32f| -> Vec<f64> {
        let l: Vec<f64> =
            img.data.iter().map(|p| f64::from(luminance_2020(p.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 }))).max(1e-4).ln()).collect();
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let (mut sum, mut n) = (0.0, 0.0);
                for yy in y.saturating_sub(2)..(y + 3).min(h) {
                    for xx in x.saturating_sub(2)..(x + 3).min(w) {
                        if let Some(v) = l.get(yy * w + xx) {
                            sum += v;
                            n += 1.0;
                        }
                    }
                }
                sum / f64::max(n, 1.0)
            })
            .collect()
    };
    let (a, b) = (log(sensor), log(reference));
    const M: usize = 3;
    let corr = |dx: isize, dy: isize| -> f64 {
        let (mut n, mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for y in M..h - M {
            for x in M..w - M {
                let (xx, yy) = (x.saturating_add_signed(dx), y.saturating_add_signed(dy));
                let (Some(&u), Some(&v)) = (a.get(y * w + x), b.get(yy * w + xx)) else { continue };
                n += 1.0;
                sa += u;
                sb += v;
                saa += u * u;
                sbb += v * v;
                sab += u * v;
            }
        }
        let var = (saa - sa * sa / n) * (sbb - sb * sb / n);
        if n < 2.0 || var <= 1e-12 { 0.0 } else { (sab - sa * sb / n) / var.sqrt() }
    };
    let at_zero = corr(0, 0);
    let (mut best, mut shift) = (at_zero, (0isize, 0isize));
    for dy in -(M as isize)..=M as isize {
        for dx in -(M as isize)..=M as isize {
            let c = corr(dx, dy);
            if c > best + 1e-9 {
                (best, shift) = (c, (dx, dy));
            }
        }
    }
    at_zero >= MIN_STRUCTURE && shift.0.abs() <= 1 && shift.1.abs() <= 1
}

/// Same-size proxies of the sensor (white-balanced, baseline exposure, through `transform`'s
/// matrix: the generic camera ≈ sRGB model) and of the file's embedded camera JPEG (linear Rec.2020),
/// and per proxy pixel whether any sensor sample it covers is clipped.
fn proxies(
    raw: &RawImage,
    bytes: &[u8],
    transform: &CameraTransform,
    size: usize,
    min_fraction: f64,
    lens: Option<&LensGeometry>,
) -> Option<(Rgb32f, Rgb32f, Vec<bool>)> {
    let edge = (2 * size).max(384) as u32;
    let decoded = crate::files::decode_raw_preview(
        bytes,
        lightcraft_codecs::DecodeOptions { max_size: Some((edge, edge)), max_pixels: FIT_PREVIEW_MAX_PIXELS },
        None,
    )?;
    let (a, crop) = (raw.active_area, raw.crop.clipped(raw.active_area.width, raw.active_area.height));
    // a preview below `min_fraction` of the crop's long edge (or under 384 px) is refused; 0 = any
    if min_fraction > 0.0 && (decoded.source_width.max(decoded.source_height) as f64) < (min_fraction * crop.width.max(crop.height) as f64).max(384.0)
    {
        return None;
    }
    let mut reference = decoded.to_working();
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
    // The camera may have corrected its JPEG with the lens warp the file carries (Nikon Z lenses always, at up to 9 %
    // zoom; other cameras when their correction setting is on): the sensor moved into that geometry is a candidate,
    // kept below when it lines up with the JPEG better than the uncorrected one.
    let warped = lens
        .filter(|_| raw.format != RawFormat::Cr3)
        .and_then(|l| Some((into_lens_geometry(&sensor, raw, k, l)?, into_lens_geometry(&clipped, raw, k, l)?)));
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
    let mut reference_small = reference_at(&reference, &sensor);
    if let Some((warped, warped_clipped)) = warped {
        let candidate = fit(&warped, size, size, Filter::Box);
        let candidate_reference = reference_at(&reference, &candidate);
        if alignment(&candidate, &candidate_reference) > alignment(&sensor, &reference_small) {
            (sensor, reference_small, clipped) = (candidate, candidate_reference, warped_clipped);
        }
    }
    let reference = reference_small;
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
pub(crate) fn profile_pairs(raw: &RawImage, bytes: &[u8], lens: Option<&LensGeometry>) -> Option<Vec<([f64; 3], [f64; 3])>> {
    if !file_local_look(raw.format) || lightcraft_raw::color::has_matrix(&raw.color) {
        return None;
    }
    let transform = lightcraft_raw::color::camera_transform(raw, lightcraft_raw::color::as_shot_white_xy(raw));
    let (sensor, reference, clipped) = proxies(raw, bytes, &transform, PROFILE_PROXY, 0.0, lens)?;
    let to_camera = transform.matrix.inverse()?;
    // clipped sensor pixels don't record the scene's colour (see `fit_look`)
    let sensor = without_clipped(&sensor, &clipped).unwrap_or(sensor);
    let (pairs, _) = collect_pairs(&sensor, &reference, 0.05, None, false)?;
    Some(pairs.into_iter().map(|(x, y)| (to_camera.apply(x), y)).collect())
}

/// A camera profile's colour model (matrix from white-balanced camera RGB, hue/saturation table)
/// fitted to pairs pooled from many photos.
pub(crate) fn fit_profile(pairs: &[([f64; 3], [f64; 3])]) -> Option<(Mat3, Option<HsvTable>)> {
    let matrix = fit_matrix(pairs)?;
    Some((matrix, fit_hue_sat(pairs, &matrix)))
}

/// A raw's lens-correction warp, as its camera applied it to the embedded JPEG: the first `WarpRectilinear` of
/// `OpcodeList3` (its green plane; the in-file corrections LightCraft reads have no tangential terms). `None` without
/// one, or for a DNG 1.6 `WarpRectilinear2` chain (phone DNGs, which carry colour matrices and are not fitted).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LensGeometry {
    /// `kr0..kr3`: source radius = r * (kr0 + kr1 r^2 + kr2 r^4 + kr3 r^6), r over the farthest corner distance.
    k: [f64; 4],
    /// The optical centre, relative to the active area (as the opcode has it).
    center: [f64; 2],
}

pub(crate) fn lens_geometry(list3: &[lightcraft_raw::Opcode]) -> Option<LensGeometry> {
    use lightcraft_raw::Opcode;
    let ops: Vec<&Opcode> = lightcraft_raw::opcodes::applied_ops(list3).collect();
    if ops.iter().any(|op| matches!(op, Opcode::WarpRectilinear2 { .. })) {
        return None;
    }
    ops.into_iter().find_map(|op| match op {
        Opcode::WarpRectilinear { planes, center } => {
            let p = planes.get(1).or(planes.first())?;
            let k = [p[0], p[1], p[2], p[3]];
            (k.iter().chain(center).all(|v| v.is_finite()) && (0.0..=1.0).contains(&center[0]) && (0.0..=1.0).contains(&center[1]))
                .then_some(LensGeometry { k, center: *center })
        }
        _ => None,
    })
}

/// How well two same-size proxies line up: the correlation of their high-passed log luminance over the pixels both
/// have (a tone curve or colour change leaves it alone, a geometric offset lowers it). `-1` without enough pixels.
fn alignment(a: &Rgb32f, b: &Rgb32f) -> f64 {
    let (w, h) = (a.width, a.height);
    if (b.width, b.height) != (w, h) || w < 3 || h < 3 {
        return -1.0;
    }
    let lum = |img: &Rgb32f| img.data.iter().map(|p| (luminance_2020(*p).max(1e-4) as f64).ln()).collect::<Vec<f64>>();
    let high = |l: &[f64]| -> Vec<f64> {
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                    return f64::NAN;
                }
                let around = [i - w - 1, i - w, i - w + 1, i - 1, i + 1, i + w - 1, i + w, i + w + 1];
                l[i] - around.iter().map(|&j| l[j]).sum::<f64>() / 8.0
            })
            .collect()
    };
    let (ha, hb) = (high(&lum(a)), high(&lum(b)));
    let pairs: Vec<(f64, f64)> = ha.into_iter().zip(hb).filter(|(x, y)| x.is_finite() && y.is_finite()).collect();
    if pairs.len() < 64 {
        return -1.0;
    }
    let n = pairs.len() as f64;
    let (ma, mb) = (pairs.iter().map(|p| p.0).sum::<f64>() / n, pairs.iter().map(|p| p.1).sum::<f64>() / n);
    let cov: f64 = pairs.iter().map(|p| (p.0 - ma) * (p.1 - mb)).sum();
    let (va, vb): (f64, f64) = (pairs.iter().map(|p| (p.0 - ma).powi(2)).sum(), pairs.iter().map(|p| (p.1 - mb).powi(2)).sum());
    let r = cov / (va * vb).sqrt().max(1e-12);
    if r.is_finite() { r } else { -1.0 }
}

/// A `k`-binned sensor proxy of `raw` ([`sensor_proxy`]: the default crop, or the active area when the crop is
/// smaller than a block, at `1/k`) moved into the camera JPEG's geometry: each pixel samples the proxy where `lens`
/// maps it, and is NaN where that falls outside the proxy (the pairs leave it out). `None` for a proxy of another
/// size (the demosaiced fallback of linear raws).
fn into_lens_geometry(img: &Rgb32f, raw: &RawImage, k: usize, lens: &LensGeometry) -> Option<Rgb32f> {
    let a = raw.active_area;
    let c = raw.crop.clipped(a.width, a.height);
    let c = if c.width < k || c.height < k { lightcraft_raw::Rect::new(0, 0, a.width, a.height) } else { c };
    let (w, h) = (img.width, img.height);
    if k == 0 || w != c.width / k || h != c.height / k || w == 0 || h == 0 || a.width < 2 || a.height < 2 {
        return None;
    }
    let (aw, ah) = (a.width as f64 - 1.0, a.height as f64 - 1.0);
    let (cx, cy) = (lens.center[0] * aw, lens.center[1] * ah);
    let m = [(0.0, 0.0), (aw, 0.0), (0.0, ah), (aw, ah)].iter().map(|&(x, y)| (x - cx).hypot(y - cy)).fold(0.0, f64::max).max(1e-9);
    let [k0, k1, k2, k3] = lens.k;
    let kf = k as f64;
    let (x0, y0) = (c.x as f64, c.y as f64);
    let mut out = Rgb32f::new(w, h);
    for (i, px) in out.data.iter_mut().enumerate() {
        // the proxy pixel's centre in active-area pixel-index coordinates, mapped to the sensor
        let (ox, oy) = (x0 + ((i % w) as f64 + 0.5) * kf - 0.5, y0 + ((i / w) as f64 + 0.5) * kf - 0.5);
        let (dx, dy) = ((ox - cx) / m, (oy - cy) / m);
        let r2 = dx * dx + dy * dy;
        let f = k0 + r2 * (k1 + r2 * (k2 + r2 * k3));
        let (sx, sy) = ((cx + m * dx * f + 0.5 - x0) / kf, (cy + m * dy * f + 0.5 - y0) / kf);
        *px = if sx >= 0.5 && sy >= 0.5 && sx <= w as f64 - 0.5 && sy <= h as f64 - 0.5 {
            img.sample_bilinear(sx as f32, sy as f32)
        } else {
            [f32::NAN; 3]
        };
    }
    Some(out)
}

fn sensor_proxy(raw: &RawImage, k: usize, edge: usize) -> Option<Rgb32f> {
    Some(match raw.develop_binned(k, SENSOR_CLIP).ok()? {
        Some(sensor) => sensor,
        // Lens opcodes cannot be applied to a binned mosaic: they are defined at full resolution.
        None if (raw.cpp == 3 && raw.cfa.is_none()) || (raw.format == RawFormat::Dng && !raw.opcodes.list3.is_empty()) => {
            fit(&raw.develop(lightcraft_raw::Method::Bilinear).ok()?, edge, edge, Filter::Box)
        }
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
type Pair = ([f64; 3], [f64; 3]);
type Pairs = Vec<Pair>;
fn collect_pairs(sensor: &Rgb32f, reference: &Rgb32f, min_chroma: f32, edge_limit: Option<f32>, dark: bool) -> Option<(Pairs, Pairs)> {
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
        let floors = if dark { DARK_FLOOR } else { (0.015, 0.004) };
        if !colour_pair_above(*input, *output, floors) {
            continue;
        }
        let min = output.iter().copied().fold(f32::INFINITY, f32::min);
        let max = output.iter().copied().fold(0.0, f32::max);
        colour += usize::from(max - min > min_chroma);
        pairs.push((input.map(f64::from), output.map(f64::from)));
    }
    (pairs.len() >= 256 && colour >= pairs.len() / 20).then_some((pairs, bright))
}

/// The original colour-pair eligibility, also used to preserve its held-out split in tone fitting.
fn colour_pair(input: [f32; 3], output: [f32; 3]) -> bool {
    colour_pair_above(input, output, (0.015, 0.004))
}

/// [`colour_pair`] with its lowest preview luminance and channel value given (`floors`: the usual 0.015 and 0.004, or
/// [`DARK_FLOOR`] for the tone-only fit of a DNG).
fn colour_pair_above(input: [f32; 3], output: [f32; 3], (ylo, clo): (f32, f32)) -> bool {
    input.iter().all(|v| v.is_finite() && *v > 0.001 && *v < 1.5)
        && output.iter().all(|v| *v > clo && *v < 0.98)
        && (ylo..0.85).contains(&luminance_2020(output))
}

/// Fit tone independently of colour eligibility when enough dark JPEG signal is available.
/// The original pairs and their split still own the colour fit and its acceptance gates.
/// Luminance needs no channel ratios, so the binned proxy can use a lower sensor floor and
/// keep JPEG pixels with a channel near black. Quantile medians limit the effect of noise.
fn fit_tone_with_shadows(
    fit: ToneFit,
    pairs: &[([f64; 3], [f64; 3])],
    sensor: &Rgb32f,
    reference: &Rgb32f,
    edge_limit: Option<f32>,
    colour: impl Fn([f64; 3]) -> [f64; 3],
) -> Option<CameraTone> {
    let luminances = |x: [f64; 3], y: [f64; 3]| (luma(colour(x).map(|v| v.max(0.0))), luma(y));
    let original = fit.fit(pairs.iter().enumerate().filter(|(i, _)| i % 3 != 0).map(|(_, (x, y))| luminances(*x, *y)).collect());
    let mut eligible = Vec::new();
    let (mut colour_index, mut extra_index) = (0, 0);
    for (i, (input, output)) in sensor.data.iter().zip(&reference.data).enumerate() {
        if edge_limit.is_some_and(|limit| local_contrast(sensor, i) > limit || local_contrast(reference, i) > limit) {
            continue;
        }
        // About one 8-bit sRGB code above black: don't learn a flat black segment from
        // JPEG compression. The sensor proxy averages many samples before this test.
        if !input.iter().all(|v| v.is_finite() && *v > 0.0001 && *v < 1.5)
            || !output.iter().all(|v| v.is_finite() && *v >= 0.0)
            || !(0.0003..0.85).contains(&luminance_2020(*output))
        {
            continue;
        }
        // A colour holdout must never enter tone training just because the wider sample set
        // changes its index. Split the extra samples separately, keeping every colour index.
        let index = if colour_pair(*input, *output) { &mut colour_index } else { &mut extra_index };
        let held = *index % 3 == 0;
        *index += 1;
        let (x, y) = luminances(input.map(f64::from), output.map(f64::from));
        if x.is_finite() && x > 0.0 {
            eligible.push((x, y, held));
        }
    }
    let dark = eligible.iter().filter(|(_, y, _)| *y < 0.015).count();
    let held: Pairs = eligible.iter().filter(|(_, y, held)| *held && *y < 0.015).map(|(x, y, _)| ([*x; 3], [*y; 3])).collect();
    // Sparse or uncorrelated shadow noise cannot teach a toe. Tied JPEG levels share a rank,
    // so a single quantized grey also keeps the original curve.
    if dark < 96 || held.len() < 32 || rank_correlation_ties(&held) < PARTIAL_MIN_RANK_CORRELATION {
        return original;
    }
    let training = eligible.iter().filter(|(_, _, held)| !held).map(|(x, y, _)| (*x, *y)).collect();
    let Some(candidate) = fit.fit(training) else { return original };
    let error = |tone: &CameraTone| -> f64 {
        held.iter().map(|(x, y)| (f64::from(tone.apply(x[0] as f32)).powf(1.0 / 2.2) - y[0].powf(1.0 / 2.2)).powi(2)).sum()
    };
    // A wider histogram must also help independent dark pixels, not just redistribute the
    // knots. The usual held-out gates still check the completed look on the colour pairs.
    if let Some(old) = original {
        let (before, after) = (error(&old), error(&candidate));
        if lightcraft_pipeline::profiling() {
            eprintln!("[profile] camera toe: {dark} dark pairs, held-out error {before:.5} -> {after:.5}");
        }
        if !after.is_finite() || after >= before * 0.99 {
            return original;
        }
    }
    Some(candidate)
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
    search_ordered(sensor, reference, colour, order, &HighlightCheck { pairs: highlights(sensor, reference), dro: false, grey: false })
        .map(|(look, _)| look)
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
    /// Train on the dark end of the preview too ([`DARK_FLOOR`]): the tone-only fit of a DNG.
    dark: bool,
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
fn search_ordered(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: Option<(Mat3, Option<HsvTable>)>,
    order: &[ToneFit],
    bright: &HighlightCheck,
) -> Option<(CameraLook, Attempt)> {
    let profiles: &[bool] = if colour.is_some() { &[true, false] } else { &[false] };
    order.iter().find_map(|&tone| {
        profiles.iter().find_map(|&profile| {
            [false, true].into_iter().find_map(|away_from_edges| {
                let attempt = Attempt { tone, profile, away_from_edges, dark: false };
                fit_attempt(sensor, reference, &colour, attempt, bright).map(|look| (look, attempt))
            })
        })
    })
}

fn fit_attempt(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: &Option<(Mat3, Option<HsvTable>)>,
    attempt: Attempt,
    bright: &HighlightCheck,
) -> Option<CameraLook> {
    let colour = if attempt.profile { colour.clone() } else { None };
    fit_pairs_on(sensor, reference, colour, attempt.away_from_edges.then_some(EDGE_CONTRAST), attempt.tone, attempt.dark, bright)
}

fn fit_pairs_on(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: Option<(Mat3, Option<HsvTable>)>,
    edge_limit: Option<f32>,
    tone_fit: ToneFit,
    dark: bool,
    highlights: &HighlightCheck,
) -> Option<CameraLook> {
    // A known colour model needs enough signal for tone fitting, not a scene rich enough to
    // learn a new colour matrix. Still reject monochrome references.
    let (pairs, bright) = collect_pairs(sensor, reference, if colour.is_some() { 0.005 } else { 0.05 }, edge_limit, dark)?;
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
        let Some(curve) = fit_tone_with_shadows(tone_fit, &pairs, sensor, reference, edge_limit, colour) else { continue };
        let mut look = CameraLook { matrix, tone: curve, hue_sat: hue_sat.clone() };
        if let Some(tone) = fit_chroma(&bright, &look) {
            look.tone = tone;
        }
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
        if !highlights_hold(&look.tone, highlights, colour) {
            continue;
        }
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

/// [`rank_correlation`] with tied luminances sharing their mean rank, and 0 when either side has no spread. Without
/// colour to require, a JPEG may be flat (one grey for every pixel): [`rank_correlation`] ranks ties in pixel order,
/// so a flat JPEG of a scene whose pixels are stored in brightness order looks perfectly related.
fn rank_correlation_ties(pairs: &[([f64; 3], [f64; 3])]) -> f64 {
    let ranks = |values: Vec<f64>| {
        let mut order: Vec<usize> = (0..values.len()).collect();
        order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
        let mut ranks = vec![0.0; values.len()];
        let mut start = 0;
        while start < order.len() {
            let mut end = start + 1;
            while end < order.len() && values[order[end]] == values[order[start]] {
                end += 1;
            }
            let mean = (start + end - 1) as f64 / 2.0;
            for &i in &order[start..end] {
                ranks[i] = mean;
            }
            start = end;
        }
        ranks
    };
    let scene = ranks(pairs.iter().map(|(x, _)| luma(*x)).collect());
    let jpeg = ranks(pairs.iter().map(|(_, y)| luma(*y)).collect());
    let mean = (pairs.len() as f64 - 1.0) / 2.0;
    let (covariance, scene_variance, jpeg_variance) = scene
        .iter()
        .zip(&jpeg)
        .fold((0.0, 0.0, 0.0), |(c, va, vb), (a, b)| (c + (a - mean) * (b - mean), va + (a - mean) * (a - mean), vb + (b - mean) * (b - mean)));
    if scene_variance > 0.0 && jpeg_variance > 0.0 { covariance / (scene_variance * jpeg_variance).sqrt() } else { 0.0 }
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
fn fit_partial(sensor: &Rgb32f, reference: &Rgb32f, highlights: &HighlightCheck) -> Option<CameraLook> {
    [None, Some(EDGE_CONTRAST)].into_iter().find_map(|edge_limit| fit_partial_on(sensor, reference, edge_limit, highlights))
}

fn fit_partial_on(sensor: &Rgb32f, reference: &Rgb32f, edge_limit: Option<f32>, highlights: &HighlightCheck) -> Option<CameraLook> {
    // as for a known colour model: enough signal for the tone, but no monochrome reference
    let (pairs, bright) = collect_pairs(sensor, reference, 0.005, edge_limit, false)?;
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
    let tone = fit_tone_with_shadows(ToneFit::Quantile, &pairs, sensor, reference, edge_limit, |x| matrix.apply(x))?;
    let mut look = CameraLook { matrix, tone, hue_sat: None };
    if let Some(tone) = fit_chroma(&bright, &look) {
        look.tone = tone;
    }
    let (tone, neutral) = (ToneMap::camera(&look.tone, 0.0, 0.0, 0.0), ToneMap::new(0.0, 0.0, 0.0));
    if !highlights_hold(&look.tone, highlights, |x| matrix.apply(x)) {
        return None;
    }
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

/// A camera JPEG pixel's channels may differ by this much (linear) and still count as grey.
const GREY_SPREAD: f32 = 0.002;

/// Whether the camera JPEG is black and white: a monochrome picture style (Canon Monochrome, Sony B/W, Nikon
/// Monochrome), shot from a full-colour raw. All but one proxy pixel in a thousand are grey within
/// [`GREY_SPREAD`]. A toned monochrome (sepia) has colour, so it isn't one.
fn monochrome(reference: &Rgb32f) -> bool {
    let coloured = reference
        .data
        .iter()
        .filter(|p| {
            let (min, max) = p.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
            !p.iter().all(|v| v.is_finite()) || max - min > GREY_SPREAD
        })
        .count();
    reference.data.len() >= 256 && coloured.saturating_mul(1000) <= reference.data.len()
}

/// The look of a raw whose camera JPEG is black and white ([`monochrome`]; issue #577) and gave no look with a camera
/// profile's colour in [`fit_preview`] (or whose model has no profile). Such a JPEG has no colour to fit, so the photo
/// would start from the spectral matrices or the neutral fallback with the default tone curve, about 1.3 EV darker
/// and flatter than the same body's photos shot in colour. But the JPEG still shows the camera's tone curve.
/// `transform` is the colour the photo gets otherwise (the spectral matrices or the fallback's); the look keeps it and
/// adds the tone curve alone ([`fit_tone_only_with`]), as [`fit_look_with`] tries the photo's own colour after a profile's. The
/// chroma curve stays neutral: the JPEG has no colourfulness to learn.
fn monochrome_look(raw: &RawImage, bytes: &[u8], transform: &CameraTransform, lens: Option<&LensGeometry>) -> Option<CameraLook> {
    if !file_local_look(raw.format) {
        return None;
    }
    let (sensor, reference, clipped) = proxies(raw, bytes, transform, PROXY, 0.0, lens)?;
    if !monochrome(&reference) {
        return None;
    }
    // Sony's Dynamic Range Optimizer brightens a B/W JPEG as much as a colour one
    let dro = lightcraft_raw::embedded_preview_dynamic_range_optimized(bytes) == Some(true);
    let unclipped = without_clipped(&sensor, &clipped);
    let bright = HighlightCheck { pairs: highlights(&sensor, &reference), dro, grey: true };
    let look = fit_tone_only_with(unclipped.as_ref().unwrap_or(&sensor), &reference, None, &bright)?;
    let look = if dro { with_dro_off_tone(look) } else { look };
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] {:?} camera look from a black-and-white JPEG: tone {:?}, {} colour, DRO tone replaced {dro}",
            raw.format,
            look.tone,
            if transform.matrix_is_fallback { "fallback" } else { "spectral" }
        );
    }
    Some(look)
}

/// The tone curve of a black-and-white camera JPEG, on top of `colour` (a camera profile's, in the sensor proxy's
/// space) or, without one, of the sensor proxy's own colour. Fitted like [`fit_partial`]'s, by quantile matching of
/// the luminances, on all pixels and then away from edges, and used only when the JPEG shows the same picture
/// ([`PARTIAL_MIN_RANK_CORRELATION`]) and the look cuts the held-out squared luminance error to below
/// [`MIN_IMPROVEMENT`] of the default curve's, in linear and in gamma-encoded display values. A curve narrower than
/// [`NARROW_TONE_RANGE`] must also keep the JPEG's bright areas ([`highlights_hold`], by luminance).
fn fit_tone_only_with(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: Option<(Mat3, Option<HsvTable>)>,
    highlights: &HighlightCheck,
) -> Option<CameraLook> {
    [None, Some(EDGE_CONTRAST)].into_iter().find_map(|edge_limit| fit_tone_only_on(sensor, reference, &colour, edge_limit, highlights))
}

/// [`fit_tone_only_with`] for a photo without Sony's DRO, checked against its own bright areas.
#[cfg(test)]
fn fit_tone_only(sensor: &Rgb32f, reference: &Rgb32f, colour: Option<(Mat3, Option<HsvTable>)>) -> Option<CameraLook> {
    fit_tone_only_with(sensor, reference, colour, &HighlightCheck { pairs: highlights(sensor, reference), dro: false, grey: true })
}

fn fit_tone_only_on(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: &Option<(Mat3, Option<HsvTable>)>,
    edge_limit: Option<f32>,
    highlights: &HighlightCheck,
) -> Option<CameraLook> {
    // every pair counts: the JPEG has no colour to require
    let (pairs, _) = collect_pairs(sensor, reference, f32::NEG_INFINITY, edge_limit, false)?;
    fit_tone_only_pairs(sensor, reference, colour, edge_limit, &pairs, false, highlights)
}

/// A near-black frame may have enough luminance signal for tone but fewer than 256 colour pairs.
/// Try only after every existing look fails. Keep the colour of the source transform,
/// exclude clipped pixels from training, and keep their luminance at least as close to the JPEG
/// as the default curve does. Bright JPEGs need the existing fits to constrain their highlights.
fn fit_dark_tone(sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool]) -> Option<CameraLook> {
    if (sensor.width, sensor.height) != (reference.width, reference.height)
        || sensor.data.len() != reference.data.len()
        || clipped.len() != sensor.data.len()
        || sensor.data.is_empty()
        || sensor.data.iter().any(|p| !p.iter().all(|v| v.is_finite()))
        || reference.data.iter().any(|p| !p.iter().all(|v| v.is_finite() && *v >= 0.0) || luminance_2020(*p) >= 0.85)
    {
        return None;
    }
    let dark = reference.data.iter().filter(|p| luminance_2020(**p) < 0.015).count();
    if (dark as f64) < reference.data.len() as f64 * 0.9 {
        return None;
    }
    // no JPEG pixel reaches 0.85 here, so the bright-area check has nothing to hold
    let bright = HighlightCheck { pairs: highlights(sensor, reference), dro: false, grey: true };
    [None, Some(EDGE_CONTRAST)].into_iter().find_map(|edge_limit| {
        let pairs: Pairs = sensor
            .data
            .iter()
            .zip(&reference.data)
            .enumerate()
            .filter(|(i, (input, output))| {
                // The same signal floors as the camera toe, without colour ratios or counts.
                !clipped.get(*i).copied().unwrap_or(true)
                    && input.iter().all(|v| *v > 0.0001 && *v < 1.5)
                    && (0.0003..0.85).contains(&luminance_2020(**output))
                    && edge_limit.is_none_or(|limit| local_contrast(sensor, *i) <= limit && local_contrast(reference, *i) <= limit)
            })
            .map(|(_, (input, output))| (input.map(f64::from), output.map(f64::from)))
            .collect();
        if pairs.len() < 256 {
            return None;
        }
        let look = fit_tone_only_pairs(sensor, reference, &None, edge_limit, &pairs, true, &bright)?;
        let (tone, neutral) = (ToneMap::camera(&look.tone, 0.0, 0.0, 0.0), ToneMap::new(0.0, 0.0, 0.0));
        for ((input, target), clipped) in sensor.data.iter().zip(&reference.data).zip(clipped) {
            if *clipped {
                let target = luma(target.map(f64::from));
                let before = luma(displayed(input.map(f64::from), &neutral));
                let after = luma(displayed(input.map(f64::from), &tone));
                if !after.is_finite() || (after - target).abs() > (before - target).abs() {
                    return None;
                }
            }
        }
        Some(look)
    })
}

fn fit_tone_only_pairs(
    sensor: &Rgb32f,
    reference: &Rgb32f,
    colour: &Option<(Mat3, Option<HsvTable>)>,
    edge_limit: Option<f32>,
    pairs: &Pairs,
    dark: bool,
    highlights: &HighlightCheck,
) -> Option<CameraLook> {
    let kind = if dark { "near-black scene" } else { "black-and-white JPEG" };
    let related = rank_correlation_ties(pairs);
    if related < PARTIAL_MIN_RANK_CORRELATION {
        if lightcraft_pipeline::profiling() {
            eprintln!("[profile] camera look from a {kind}: luminance rank correlation {related:.3}, not the same picture");
        }
        return None;
    }
    let (matrix, hue_sat) = colour.clone().unwrap_or((Mat3::IDENTITY, None));
    let correction = hue_sat.as_ref().and_then(HueSat::new);
    let coloured = |x: [f64; 3]| {
        let p = matrix.apply(x);
        correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from))
    };
    let tone = if dark {
        // Split this wider set once. The camera toe's separate colour/extra split belongs to
        // the original colour pairs and would put some of these held-out pixels into training.
        fit_tone(
            pairs.iter().enumerate().filter(|(i, _)| i % 3 != 0).map(|(_, (x, y))| (luma(coloured(*x).map(|v| v.max(0.0))), luma(*y))).collect(),
        )?
    } else {
        fit_tone_with_shadows(ToneFit::Quantile, pairs, sensor, reference, edge_limit, coloured)?
    };
    let look = CameraLook { matrix, tone, hue_sat: hue_sat.clone() };
    if !highlights_hold(&look.tone, highlights, coloured) {
        return None;
    }
    let (tone, neutral) = (ToneMap::camera(&look.tone, 0.0, 0.0, 0.0), ToneMap::new(0.0, 0.0, 0.0));
    let encoded = |v: f64| v.max(0.0).powf(1.0 / 2.2);
    let (mut before, mut after, mut before_encoded, mut after_encoded, mut samples) = (0.0, 0.0, 0.0, 0.0, 0);
    for (x, target) in pairs.iter().step_by(3) {
        let (fallback, toned, target) = (luma(displayed(*x, &neutral)), luma(displayed(coloured(*x), &tone)), luma(*target));
        before += (fallback - target).powi(2);
        after += (toned - target).powi(2);
        before_encoded += (encoded(fallback) - encoded(target)).powi(2);
        after_encoded += (encoded(toned) - encoded(target)).powi(2);
        samples += 1;
    }
    if lightcraft_pipeline::profiling() {
        eprintln!(
            "[profile] camera look from a {kind}: holdout luminance RMS {:.5} -> {:.5} ({samples} pixels{})",
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

/// [`HueSat::apply`] restores the luminance in full only if the table kept at least this share of it. A colour
/// the table darkens to [`NO_LUMINANCE_KEPT`] or less (only near-black noise, whose channels go negative) keeps
/// its colour; in between the two are blended, so noisy shadows show no step.
const MIN_LUMINANCE_KEPT: f32 = 0.25;
const NO_LUMINANCE_KEPT: f32 = 0.15;

/// How much of the table's colour a pixel gets when the table kept `share` of its luminance: 0 up to
/// [`NO_LUMINANCE_KEPT`], 1 from [`MIN_LUMINANCE_KEPT`], a smoothstep between.
fn restore_weight(share: f32) -> f32 {
    if !share.is_finite() {
        return 0.0;
    }
    let t = ((share - NO_LUMINANCE_KEPT) / (MIN_LUMINANCE_KEPT - NO_LUMINANCE_KEPT)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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
        // restoring the luminance divides by the table's: near black, where noise makes some channels
        // negative, the table can bring it close to 0, and the gain would turn the pixel white (#820)
        if before <= 0.0 || after <= 0.0 || !out.iter().all(|v| v.is_finite()) {
            return rgb;
        }
        let w = restore_weight(after / before);
        if w >= 1.0 {
            return out.map(|v| v * before / after);
        }
        if w <= 0.0 {
            return rgb;
        }
        std::array::from_fn(|i| rgb[i] + (out[i] * before / after - rgb[i]) * w)
    }

    /// [`Self::apply`] to `N` pixels at once (`rgb[channel][lane]`), bit for bit, through
    /// [`HsvTable::apply_lanes`], with the guards as selects.
    #[inline]
    pub fn apply_lanes<const N: usize>(&self, rgb: [[f32; N]; 3]) -> [[f32; N]; 3] {
        let mul = |m: &[[f32; 3]; 3], v: &[[f32; N]; 3]| {
            let mut out = [[0.0f32; N]; 3];
            for (row, out) in m.iter().zip(&mut out) {
                for l in 0..N {
                    out[l] = row[0] * v[0][l] + row[1] * v[1][l] + row[2] * v[2][l];
                }
            }
            out
        };
        let out = mul(&self.from, &self.table.apply_lanes(mul(&self.to, &rgb)));
        let mut restored = rgb;
        for l in 0..N {
            let (input, out) = ([rgb[0][l], rgb[1][l], rgb[2][l]], [out[0][l], out[1][l], out[2][l]]);
            let (before, after) = (luminance_2020(input), luminance_2020(out));
            let untouched = (before <= 0.0) | (after <= 0.0) | !(out[0].is_finite() & out[1].is_finite() & out[2].is_finite());
            let w = restore_weight(after / before);
            for (c, restored) in restored.iter_mut().enumerate() {
                let full = out[c] * before / after;
                let blend = input[c] + (full - input[c]) * w;
                // (`w >= 1` and `w <= 0` exclude each other)
                restored[l] = if untouched | (w <= 0.0) {
                    input[c]
                } else if w >= 1.0 {
                    full
                } else {
                    blend
                };
            }
        }
        restored
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

/// The bright areas a narrow tone curve is checked against ([`highlights_hold`]): every proxy pixel
/// whose camera JPEG luminance is 0.85 or more, with its sensor value as developed (for a clipped
/// pixel a lower bound of the scene). Taken from the proxies before any training filter (clipped
/// pixels, edges, the sensor value range of [`collect_pairs`]), so no retry of the search can
/// leave them out.
fn highlights(sensor: &Rgb32f, reference: &Rgb32f) -> Pairs {
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
struct HighlightCheck {
    /// The frame's [`highlights`].
    pairs: Pairs,
    /// The photo's curve will be lowered to Sony's without DRO ([`with_dro_off_tone`]) after the
    /// fit, so the check looks at it lowered.
    dro: bool,
    /// The camera JPEG is black and white ([`fit_tone_only_with`]): the check compares luminance, as that
    /// fit's gates do, since the JPEG has no colour to match.
    grey: bool,
}

/// Whether a look with this `tone` and `colour` (sensor value to the finish stage's input) keeps
/// the frame's [`highlights`] (camera JPEG luminance 0.85 and up, left out of the fit) no further
/// from the camera JPEG than the neutral fallback does, with the tone curve as the photo will get
/// it (lowered for DRO), in luminance alone for a black-and-white JPEG. Only asked of curves narrower than
/// [`NARROW_TONE_RANGE`]: true for wider ones, and for frames without such pixels.
fn highlights_hold(tone: &CameraTone, check: &HighlightCheck, colour: impl Fn([f64; 3]) -> [f64; 3]) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn steep_toe_scene() -> (Rgb32f, Rgb32f) {
        let mut sensor = Rgb32f::new(96, 64);
        let mut reference = sensor.clone();
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            // A broad scene with a steep camera toe below the old JPEG floor of 0.015.
            // Permute brightness so the held-out pixels span the same range independently.
            let y = 0.002 + ((i * 7919) % 6144) as f32 / 6144.0 * 0.25;
            let rgb = [0.8 + (i % 11) as f32 * 0.04, 1.0, 0.8 + (i % 17) as f32 * 0.025];
            *src = rgb.map(|v| v * y / luminance_2020(rgb));
            let display = 3.0 * y * (y / (y + 0.02)).powi(2);
            *dst = src.map(|v| v * display / y);
        }
        (sensor, reference)
    }

    #[test]
    fn camera_tone_follows_the_jpegs_deep_shadows() {
        let (sensor, reference) = steep_toe_scene();
        let look = fit_pairs_with(&sensor, &reference, Some((Mat3::IDENTITY, None))).unwrap();
        let dark: Vec<_> = sensor.data.iter().zip(&reference.data).filter(|(_, p)| luminance_2020(**p) < 0.015).collect();
        assert!(dark.len() > 256);
        let error = dark.iter().map(|(x, y)| (look.tone.apply(luminance_2020(**x)) - luminance_2020(**y)).abs()).sum::<f32>() / dark.len() as f32;
        assert!(error < 0.001, "deep-shadow luminance error {error}");
        assert_eq!(look.matrix, Mat3::IDENTITY);
    }

    #[test]
    fn colour_holdouts_stay_out_of_the_wider_tone_training() {
        let (sensor, reference) = steep_toe_scene();
        let fit = |reference: &Rgb32f| {
            let (pairs, _) = collect_pairs(&sensor, reference, 0.005, None, false).unwrap();
            fit_tone_with_shadows(ToneFit::Quantile, &pairs, &sensor, reference, None, |x| x).unwrap()
        };
        let expected = fit(&reference);
        let mut changed = reference.clone();
        let mut index = 0;
        for (x, y) in sensor.data.iter().zip(&mut changed.data) {
            if colour_pair(*x, *y) {
                if index % 3 == 0 {
                    *y = y.map(|v| v * 1.1);
                    assert!(colour_pair(*x, *y));
                }
                index += 1;
            }
        }
        assert_eq!(fit(&changed), expected);
    }

    #[test]
    fn camera_toe_is_robust_to_shadow_noise() {
        let (sensor, clean) = steep_toe_scene();
        let mut noisy = clean.clone();
        for (i, p) in noisy.data.iter_mut().enumerate() {
            if luminance_2020(*p) < 0.015 {
                // Quantized JPEG levels with bounded noise and occasional compression outliers.
                let noise = ((i * 17 % 11) as f32 - 5.0) * 0.00004;
                *p = p.map(|v| ((v + noise).max(0.0) / 0.0003).round() * 0.0003);
                if i % 23 == 0 {
                    *p = [0.014; 3];
                }
            }
        }
        let look = fit_pairs_with(&sensor, &noisy, Some((Mat3::IDENTITY, None))).unwrap();
        let dark: Vec<_> = sensor.data.iter().zip(&clean.data).filter(|(_, p)| luminance_2020(**p) < 0.015).collect();
        let error = dark.iter().map(|(x, y)| (look.tone.apply(luminance_2020(**x)) - luminance_2020(**y)).abs()).sum::<f32>() / dark.len() as f32;
        assert!(error < 0.001, "noisy shadow luminance error {error}");
        assert_eq!(look.tone.apply(0.0), 0.0);
        assert_eq!(look.matrix, Mat3::IDENTITY);
        for i in 1..1000 {
            assert!(look.tone.apply(i as f32 / 1000.0) >= look.tone.apply((i - 1) as f32 / 1000.0));
        }
    }

    #[test]
    fn camera_toe_requires_lower_independent_dark_error() {
        for holdout_matches_original in [true, false] {
            let mut sensor = Rgb32f::new(96, 64);
            let mut reference = sensor.clone();
            for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
                let y = if i < 600 { 0.0015 + (i % 200) as f32 * 0.00001 } else { 0.02 + ((i * 7919) % 6144) as f32 / 6144.0 * 0.23 };
                *src = [y; 3];
                // Midtones teach the original slope of 3. Dark training teaches 0.3,
                // but the independent dark holdout can agree with either slope.
                let slope = if i < 600 && !(holdout_matches_original && i % 3 == 0) { 0.3 } else { 3.0 };
                *dst = [y * slope; 3];
                assert_eq!(colour_pair(*src, *dst), i >= 600);
            }
            let (pairs, _) = collect_pairs(&sensor, &reference, f32::NEG_INFINITY, None, false).unwrap();
            let original = fit_tone(pairs.iter().enumerate().filter(|(i, _)| i % 3 != 0).map(|(_, (x, y))| (luma(*x), luma(*y))).collect()).unwrap();
            // Both sample sets start at a multiple of three, so this split matches
            // the helper's separate colour and extra-sample indices.
            let candidate = fit_tone(
                sensor
                    .data
                    .iter()
                    .zip(&reference.data)
                    .enumerate()
                    .filter(|(i, _)| i % 3 != 0)
                    .map(|(_, (x, y))| (luma(x.map(f64::from)), luma(y.map(f64::from))))
                    .collect(),
            )
            .unwrap();
            let held: Pairs =
                sensor.data.iter().zip(&reference.data).take(600).step_by(3).map(|(x, y)| (x.map(f64::from), y.map(f64::from))).collect();
            assert_eq!(held.len(), 200);
            assert!(held.iter().all(|(_, y)| (0.0003..0.015).contains(&luma(*y))));
            assert!(rank_correlation_ties(&held) > 0.99);
            let error = |tone: &CameraTone| {
                held.iter().map(|(x, y)| (f64::from(tone.apply(luma(*x) as f32)).powf(1.0 / 2.2) - luma(*y).powf(1.0 / 2.2)).powi(2)).sum::<f64>()
            };
            assert_ne!(candidate, original);
            let selected = fit_tone_with_shadows(ToneFit::Quantile, &pairs, &sensor, &reference, None, |x| x).unwrap();
            if holdout_matches_original {
                assert!(error(&candidate) > error(&original) + 0.1);
                assert_eq!(selected, original, "a correlated but misleading training set must keep the original curve");
            } else {
                assert!(error(&candidate) < error(&original) * 0.99);
                assert_eq!(selected, candidate, "matching dark training and holdout must accept the candidate");
            }
            let mut previous = 0.0;
            assert_eq!(selected.apply(0.0), 0.0);
            for i in 0..10_000 {
                let y = selected.apply(10f32.powf(-8.0 + i as f32 / 1000.0));
                assert!(y.is_finite() && y >= previous && y <= 1.0);
                previous = y;
            }
            assert_eq!(selected.apply(f32::NAN), 0.0);
            assert_eq!(selected.apply(f32::INFINITY), 0.0);
        }
    }

    #[test]
    fn unreliable_dark_pairs_keep_the_original_tone_and_colour() {
        let (sensor, reference) = steep_toe_scene();
        let dark: Vec<_> = reference.data.iter().enumerate().filter(|(_, p)| luminance_2020(**p) < 0.015).map(|(i, _)| i).collect();
        let mut excluded = sensor.clone();
        for &i in &dark {
            excluded.data[i] = [f32::NAN; 3];
        }
        let colour = Some((Mat3::IDENTITY, None));
        let original = fit_pairs_with(&excluded, &reference, colour.clone()).unwrap();
        for case in 0..6 {
            let (mut x, mut y) = (sensor.clone(), reference.clone());
            for (j, &i) in dark.iter().enumerate() {
                match case {
                    0 => y.data[i] = [0.0; 3],                                            // JPEG black gives no measured shadow signal
                    1 => x.data[i] = [0.00001; 3],                                        // below the sensor noise floor
                    2 if j >= 95 => y.data[i] = [0.0; 3],                                 // too few dark pairs
                    3 => x.data[i] = [0.003; 3],                                          // no scene luminance spread
                    4 => y.data[i] = [0.003; 3],                                          // one quantized grey, not a toe
                    5 => y.data[i] = [0.0004 + ((j * 7919) % 1000) as f32 * 0.000014; 3], // unrelated noise
                    _ => {}
                }
            }
            let look = fit_pairs_with(&x, &y, colour.clone()).unwrap();
            assert_eq!(look.tone, original.tone, "case {case}");
            assert_eq!(look.matrix, original.matrix, "case {case}");
            assert!(look.hue_sat.is_none());
        }
    }

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
            pixel_aspect: 1.0,
            opcodes: OpcodeLists::default(),
            metadata: lightcraft_meta::Metadata::default(),
            lens_metadata: Default::default(),
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
            pool.extend(collect_pairs(&sensor, &reference, 0.05, None, false).unwrap().0);
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
    fn hue_sat_never_turns_near_black_noise_white() {
        // a saturating table, as fitted to high-ISO Sony files; near-black noise with mixed-sign channels
        let table = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[10.0, 1.5, 1.0]; 8], srgb_value: false };
        let hue_sat = HueSat::new(&table).unwrap();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0
        };
        let mut worst = 0.0f32;
        for _ in 0..200_000 {
            let rgb = [0.002 + 0.004 * next(), 0.002 + 0.004 * next(), 0.002 + 0.004 * next()];
            if luminance_2020(rgb) <= 0.0 {
                continue;
            }
            let out = hue_sat.apply(rgb);
            let scale = rgb.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            worst = worst.max(out.iter().fold(0.0f32, |m, v| m.max(v.abs())) / scale);
        }
        assert!(worst < 20.0, "a near-black pixel came out {worst}× its largest channel");
    }

    /// `apply_lanes` gives `apply`'s bits for any colour: random, near-black noise (the restore's blend), grey,
    /// negative, tiny, above 1, huge, NaN and infinite components; on a fitted look's shape and a saturating table.
    #[test]
    fn hue_sat_lanes_match_one_by_one_bit_for_bit() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let unit = |r: u64, i: usize| ((r >> (16 * i)) & 0xffff) as f32 / 65535.0;
        let fitted = (0..72 * 5 * 5)
            .map(|_| {
                let r = next();
                [unit(r, 0) * 60.0 - 30.0, 0.5 + unit(r, 1), 1.0]
            })
            .collect();
        let fitted = HsvTable { hue_divisions: 72, sat_divisions: 5, val_divisions: 5, data: fitted, srgb_value: true };
        let saturating = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[10.0, 1.5, 1.0]; 8], srgb_value: false };
        let special = [0.0, -0.0, 1e-30, -1e-6, 0.002, 0.5, 1.0, 3.0, 1e30, f32::INFINITY, f32::NEG_INFINITY, f32::NAN];
        let mut colours = Vec::new();
        for a in special {
            for b in special {
                colours.extend(special.map(|c| [a, b, c]));
            }
        }
        for _ in 0..60_000 {
            let r = next();
            let (offset, scale) = [(0.15, 1.3), (0.15e-3, 1.3e-3), (0.6, 5.0), (-0.002, 0.008)][(r >> 62) as usize];
            colours.push(std::array::from_fn(|i| unit(r, i) * scale - offset));
        }
        for table in [fitted, saturating] {
            let hue_sat = HueSat::new(&table).unwrap();
            for chunk in colours.chunks(16) {
                let rgb: [[f32; 16]; 3] = std::array::from_fn(|c| std::array::from_fn(|l| chunk.get(l).map_or(0.0, |p| p[c])));
                let out = hue_sat.apply_lanes(rgb);
                for (l, p) in chunk.iter().enumerate() {
                    let (got, want) = ([out[0][l], out[1][l], out[2][l]], hue_sat.apply(*p));
                    assert_eq!(got.map(f32::to_bits), want.map(f32::to_bits), "{p:?}: {got:?} vs {want:?}");
                }
            }
        }
    }

    #[test]
    fn hue_sat_restore_weight_blends_smoothly_between_the_thresholds() {
        assert_eq!(restore_weight(0.0), 0.0);
        assert_eq!(restore_weight(NO_LUMINANCE_KEPT), 0.0);
        assert_eq!(restore_weight(MIN_LUMINANCE_KEPT), 1.0);
        assert_eq!(restore_weight(1.0), 1.0);
        assert_eq!(restore_weight(f32::NAN), 0.0);
        // monotonic and without a step: neighbouring shares 0.001 apart differ by little
        let mut last = 0.0f32;
        for i in 0..=400 {
            let w = restore_weight(i as f32 * 0.001);
            assert!(w >= last && w - last < 0.02, "step at {}: {last} → {w}", i as f32 * 0.001);
            last = w;
        }
    }

    #[test]
    fn hue_sat_keeps_colours_the_table_leaves_bright_exactly_as_before() {
        // the blend only touches colours whose luminance the table cut below MIN_LUMINANCE_KEPT
        let table = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[10.0, 1.5, 1.0]; 8], srgb_value: false };
        let hue_sat = HueSat::new(&table).unwrap();
        let mul = |m: &[[f32; 3]; 3], v: [f32; 3]| -> [f32; 3] { std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2]) };
        for rgb in [[0.4, 0.2, 0.1], [0.05, 0.3, 0.2], [0.18, 0.18, 0.18], [0.6, 0.1, 0.5]] {
            let out = mul(&hue_sat.from, table.apply(mul(&hue_sat.to, rgb)));
            let (before, after) = (luminance_2020(rgb), luminance_2020(out));
            assert!(after >= MIN_LUMINANCE_KEPT * before);
            assert_eq!(hue_sat.apply(rgb), out.map(|v| v * before / after));
        }
    }

    fn raw_of(format: RawFormat, make: &str, model: &str, color: lightcraft_raw::ColorData) -> RawImage {
        let area = lightcraft_raw::Rect::new(0, 0, 2, 2);
        RawImage {
            format,
            width: 2,
            height: 2,
            cpp: 1,
            data: lightcraft_raw::RawData::U16(vec![0; 4]),
            cfa: None,
            bits: 14,
            black: Default::default(),
            white: vec![16383.0],
            active_area: area,
            crop: area,
            orientation: Default::default(),
            color,
            wb_multipliers: Some([2.0, 1.0, 1.5]),
            linearized: false,
            pixel_aspect: 1.0,
            opcodes: Default::default(),
            metadata: lightcraft_raw::Metadata { make: Some(make.to_owned()), model: Some(model.to_owned()), ..Default::default() },
            lens_metadata: Default::default(),
        }
    }

    /// The lens warp the colour fit follows is the first `WarpRectilinear`'s green plane; no warp, a DNG 1.6 chain
    /// (phone DNGs, which carry colour matrices), a centre outside the area or a non-finite term give none.
    #[test]
    fn lens_geometry_reads_the_first_rectilinear_warp() {
        use lightcraft_raw::Opcode;
        let warp = |k0: f64, c: [f64; 2]| Opcode::WarpRectilinear {
            planes: vec![[9.0, 0.0, 0.0, 0.0, 0.0, 0.0], [k0, 0.1, -0.02, 0.003, 0.0, 0.0], [9.0, 0.0, 0.0, 0.0, 0.0, 0.0]],
            center: c,
        };
        assert_eq!(lens_geometry(&[warp(0.97, [0.5, 0.4])]), Some(LensGeometry { k: [0.97, 0.1, -0.02, 0.003], center: [0.5, 0.4] }));
        let single = Opcode::WarpRectilinear { planes: vec![[1.02, 0.0, 0.0, 0.0, 0.0, 0.0]], center: [0.5, 0.5] };
        assert_eq!(lens_geometry(std::slice::from_ref(&single)).map(|g| g.k[0]), Some(1.02));
        assert!(lens_geometry(&[]).is_none());
        assert!(lens_geometry(&[warp(1.0, [1.5, 0.5])]).is_none());
        assert!(lens_geometry(&[warp(f64::NAN, [0.5, 0.5])]).is_none());
        let mut p = [0.0; lightcraft_raw::opcodes::WR2_LEN];
        p[0] = 1.0;
        p[18] = 1e6;
        let chain = Opcode::WarpRectilinear2 { planes: vec![p], center: [0.5, 0.5], reciprocal: false, optional: false };
        assert!(lens_geometry(&[chain, single]).is_none());
    }

    /// A binned sensor proxy moved into the camera JPEG's geometry: the identity warp leaves it as it is, a warp
    /// whose source radius is 0.8 of the output's samples nearer the centre, output pixels whose source falls outside
    /// the proxy are NaN (left out of the pairs), and a proxy of another size is left alone.
    #[test]
    fn the_sensor_proxy_follows_the_cameras_lens_warp() {
        let mut raw = raw_of(RawFormat::Nef, "NIKON CORPORATION", "NIKON Z 6_2", Default::default());
        raw.active_area = lightcraft_raw::Rect::new(0, 0, 64, 48);
        raw.crop = raw.active_area;
        // a 1/4 proxy whose pixels hold their own coordinates
        let mut img = Rgb32f::new(16, 12);
        for (i, p) in img.data.iter_mut().enumerate() {
            *p = [(i % 16) as f32, (i / 16) as f32, 1.0];
        }
        let geometry = |k0: f64| LensGeometry { k: [k0, 0.0, 0.0, 0.0], center: [0.5, 0.5] };
        let same = into_lens_geometry(&img, &raw, 4, &geometry(1.0)).unwrap();
        assert!(same.data.iter().zip(&img.data).all(|(a, b)| (0..3).all(|c| (a[c] - b[c]).abs() < 1e-4)));
        // the corner pixel's centre (1.5, 1.5) maps to (31.5 - 0.8 * 30, 23.5 - 0.8 * 22) = (7.5, 5.9) on the sensor
        let zoomed = into_lens_geometry(&img, &raw, 4, &geometry(0.8)).unwrap();
        assert!((zoomed.data[0][0] - 1.5).abs() < 1e-4 && (zoomed.data[0][1] - 1.1).abs() < 1e-4, "{:?}", zoomed.data[0]);
        let outside = into_lens_geometry(&img, &raw, 4, &geometry(1.3)).unwrap();
        assert!(outside.data[0][0].is_nan() && outside.data[6 * 16 + 8][0].is_finite());
        assert!(into_lens_geometry(&Rgb32f::new(15, 12), &raw, 4, &geometry(1.0)).is_none());
    }

    /// A raw with an in-file lens correction (Fujifilm, Panasonic, the A7R IVA, Nikon) may have a lens-corrected
    /// camera JPEG: the proxies the fit pairs line up with it at least as well as the uncorrected sensor, on every
    /// corpus file the colour fit uses that carries such a warp, and better on those whose JPEG is corrected.
    #[test]
    fn corpus_lens_corrected_jpegs_pair_with_the_warped_sensor() {
        let dir = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            eprintln!("skip: no corpus at {}", dir.display());
            return;
        };
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        let (mut checked, mut gain) = (0, 0.0);
        for path in paths {
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let Ok(mut raw) = lightcraft_raw::decode(&bytes) else { continue };
            let Some(lens) = lens_geometry(&raw.opcodes.list3) else { continue };
            if !file_local_look(raw.format) || lightcraft_raw::color::has_matrix(&raw.color) {
                continue;
            }
            raw.opcodes.list3.retain(|op| !op.is_lens_correction());
            let t = lightcraft_raw::color::camera_transform(&raw, lightcraft_raw::color::as_shot_white_xy(&raw));
            let (Some((plain, reference, _)), Some((chosen, chosen_reference, _))) =
                (proxies(&raw, &bytes, &t, PROXY, 0.0, None), proxies(&raw, &bytes, &t, PROXY, 0.0, Some(&lens)))
            else {
                continue;
            };
            let (before, after) = (alignment(&plain, &reference), alignment(&chosen, &chosen_reference));
            eprintln!("{}: alignment {before:.3} -> {after:.3}", path.display());
            // (chosen on camera RGB, measured after the colour transform: a near tie may flip by a little)
            assert!(after >= before - 0.01, "{}: {before:.3} -> {after:.3}", path.display());
            gain += after - before;
            checked += 1;
        }
        eprintln!("{checked} corpus raws with an in-file lens warp, mean gain {:.3}", gain / checked.max(1) as f64);
        assert!(checked == 0 || gain > 0.0);
    }

    /// Step 1: a file's own colour matrices win over everything, including a spectral row for its model; no JPEG fit
    /// is tried.
    #[test]
    fn a_files_own_matrices_come_first() {
        let own = Mat3([[0.6, -0.1, -0.05], [-0.4, 1.3, 0.1], [-0.1, 0.2, 0.7]]);
        let color = lightcraft_raw::ColorData { illuminant: [21, 0], color_matrix: [Some(own), None], ..Default::default() };
        let mut raw = raw_of(RawFormat::Cr2, "Canon", "Canon EOS 600D", color.clone());
        let (_, t, look) = starting_colour_with(
            &mut raw,
            |_, _| panic!("no JPEG fit for a raw with its own matrices"),
            |_, _| panic!("no black-and-white JPEG fit for a raw with its own matrices"),
            |_, _| panic!("no dark tone fit for a raw with its own matrices"),
        );
        assert_eq!(raw.color, color);
        assert!(!t.matrix_is_fallback && look.is_none());
    }

    /// Steps 2 and 3 before step 4: an accepted JPEG fit (with or without a camera profile) is kept for a model that
    /// has spectral matrices, and the spectral matrices are not written. Photos that had a fitted look before the
    /// spectral table render exactly as they did.
    #[test]
    fn an_accepted_jpeg_fit_comes_before_the_spectral_matrices() {
        for (make, model) in [("SONY", "ILCE-7M4"), ("Canon", "Canon EOS 600D")] {
            assert!(lightcraft_raw::spectral::find(Some(make), model).is_some(), "{model}");
            let mut raw = raw_of(RawFormat::Cr2, make, model, Default::default());
            let fitted = dro_test_look(&std::array::from_fn(|i| 0.006 * 1.13f32.powi(i as i32)), &|x| x.sqrt());
            let (xy, t, look) = starting_colour_with(
                &mut raw,
                |raw, t| {
                    assert!(t.matrix_is_fallback && !lightcraft_raw::color::has_matrix(&raw.color), "the fit sees the raw without spectral matrices");
                    (Some(fitted.clone()), false)
                },
                |_, _| panic!("no black-and-white JPEG fit after an accepted fit"),
                |_, _| panic!("no dark tone fit after an accepted fit"),
            );
            assert_eq!(look.map(|l| l.matrix), Some(fitted.matrix), "{model}");
            assert_eq!(raw.color, lightcraft_raw::ColorData::default(), "{model}: spectral matrices left out");
            assert!(t.matrix_is_fallback, "{model}");
            assert_eq!(xy, lightcraft_raw::color::as_shot_white_xy(&raw));
        }
    }

    /// Step 4: when the JPEG fit fails (here: no camera JPEG at all), a model with a spectral row takes its matrices,
    /// whether or not it has a camera profile (the ILCE-7M4 has a bundled one).
    #[test]
    fn a_failed_jpeg_fit_falls_back_to_the_spectral_matrices() {
        assert!(crate::camera_profiles::get("ILCE-7M4").is_some());
        let camera = lightcraft_raw::spectral::find(Some("SONY"), "ILCE-7M4").unwrap();
        let mut raw = raw_of(RawFormat::Arw, "SONY", "ILCE-7M4", Default::default());
        let (_, t, look) = starting_colour(&mut raw, &[], None, None);
        assert!(look.is_none());
        assert_eq!(raw.color.color_matrix, camera.color_matrix.map(Some));
        assert!(!t.matrix_is_fallback);

        let camera = lightcraft_raw::spectral::find(Some("Canon"), "Canon EOS 600D").unwrap();
        let mut raw = raw_of(RawFormat::Cr2, "Canon", "Canon EOS 600D", Default::default());
        let (xy, t, look) =
            starting_colour_with(&mut raw, |_, _| (None, false), |_, _| panic!("a colour JPEG never takes the black-and-white step"), |_, _| None);
        assert!(look.is_none());
        assert_eq!(raw.color.illuminant, [17, 21]);
        assert_eq!(raw.color.forward_matrix, camera.forward_matrix.map(Some));
        assert!(!t.matrix_is_fallback);
        assert_eq!(xy, lightcraft_raw::color::as_shot_white_xy(&raw));
        // white-balanced white stays white
        let w = t.matrix.apply([1.0; 3]);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-4), "{w:?}");
    }

    /// After step 4 or 5: a photo whose JPEG gave no fit and is black and white gets its tone curve on top of the
    /// colour it has by then, the spectral matrices for a model in the table, the neutral fallback's otherwise
    /// (issue #577). The black-and-white fit sees that transform.
    #[test]
    fn a_black_and_white_jpeg_adds_its_tone_on_top_of_the_colour_it_gets_otherwise() {
        let toned = CameraLook {
            matrix: Mat3::IDENTITY,
            tone: dro_test_look(&std::array::from_fn(|i| 0.006 * 1.13f32.powi(i as i32)), &|x| x.sqrt()).tone,
            hue_sat: None,
        };
        let camera = lightcraft_raw::spectral::find(Some("Canon"), "Canon EOS 600D").unwrap();
        let mut raw = raw_of(RawFormat::Cr2, "Canon", "Canon EOS 600D", Default::default());
        let (xy, t, look) = starting_colour_with(
            &mut raw,
            |_, _| (None, true),
            |raw, t| {
                assert!(!t.matrix_is_fallback && raw.color.forward_matrix == camera.forward_matrix.map(Some), "the spectral matrices are in");
                Some(toned.clone())
            },
            |_, _| panic!("an accepted monochrome look keeps its tone"),
        );
        assert!(look.as_ref().is_some_and(|l| l.matrix == Mat3::IDENTITY && l.hue_sat.is_none()), "the tone curve on top");
        assert!(!t.matrix_is_fallback);
        assert_eq!(xy, lightcraft_raw::color::as_shot_white_xy(&raw));

        let mut raw = raw_of(RawFormat::Cr2, "Canon", "Canon EOS 7D", Default::default());
        let (_, t, look) = starting_colour_with(
            &mut raw,
            |_, _| (None, true),
            |_, t| {
                assert!(t.matrix_is_fallback, "outside the table: the neutral fallback's colour");
                Some(toned.clone())
            },
            |_, _| panic!("an accepted monochrome look keeps its tone"),
        );
        assert!(look.is_some() && t.matrix_is_fallback);
        assert_eq!(raw.color, lightcraft_raw::ColorData::default());
    }

    /// Step 5: a camera outside the table whose fit fails keeps the neutral fallback.
    #[test]
    fn a_camera_outside_the_table_keeps_the_fallback() {
        let mut raw = raw_of(RawFormat::Cr2, "Canon", "Canon EOS 7D", Default::default());
        let (_, t, look) = starting_colour(&mut raw, &[], None, None);
        assert!(look.is_none() && t.matrix_is_fallback);
        assert_eq!(raw.color, lightcraft_raw::ColorData::default());
        // without a model there is no table row
        let mut raw = raw_of(RawFormat::Cr2, "Canon", "", Default::default());
        raw.metadata.model = None;
        assert!(starting_colour(&mut raw, &[], None, None).1.matrix_is_fallback);
    }

    /// The colour precedence on public raws (each skipped without its corpus file): a photo whose JPEG fit is
    /// accepted keeps it and its colour data; one whose fit fails takes its spectral matrices when its model has
    /// them; a DNG keeps its own matrices. White balance stays relative to the as-shot look for every raw without
    /// matrices of its own.
    #[test]
    fn corpus_colour_precedence() {
        let dir = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw");
        for name in ["arw-sony-a7m4-14bit.arw", "cr2-canon-5d3.cr2", "nef-nikon-d5100-lossless.nef", "cr2-canon-7d.cr2", "dng-canon-5d3-16bit.dng"] {
            let Ok(bytes) = std::fs::read(dir.join(name)) else {
                lightcraft_testkit::skip!(Corpus, "{name} absent");
                continue;
            };
            let mut raw = lightcraft_raw::decode(&bytes).unwrap();
            let lens = lens_geometry(&raw.opcodes.list3);
            raw.opcodes.list3.retain(|op| !op.is_lens_correction());
            let own = lightcraft_raw::color::has_matrix(&raw.color);
            let (make, model) = (raw.metadata.make.as_deref(), raw.metadata.model.as_deref().unwrap_or_default());
            let measured = lightcraft_raw::spectral::find(make, model).is_some();
            let covered = measured || lightcraft_raw::kelvin_presets::covers(make, model);
            let before = raw.color.clone();
            let (_, t, fitted) = starting_colour(&mut raw, &bytes, lens.as_ref(), None);
            let spectral = !own && fitted.is_none() && measured;
            eprintln!("{name}: own matrices {own}, JPEG fit {}, spectral {spectral}", fitted.is_some());
            assert_eq!(raw.color != before, spectral, "{name}");
            assert_eq!(t.matrix_is_fallback, !own && !spectral, "{name}");
            let (_, info) = crate::files::load_bytes(&bytes, 400).unwrap();
            assert_eq!(info.camera_tone.is_some(), fitted.is_some(), "{name}");
            // white balance: relative only without matrices anywhere (or presets); a covered camera edits in Kelvin,
            // through the spectral matrices or its presets, whichever look it got (issue #730)
            assert_eq!(info.relative_wb, !own && !covered, "{name}");
            assert_eq!(info.legacy_relative_wb, !own && covered, "{name}");
            if covered && !own {
                assert!(
                    (2000.0..=50000.0).contains(&info.as_shot_temp) && info.as_shot_temp != 6500.0 && info.as_shot_tint.abs() < 100.0,
                    "{name}: {info:?}"
                );
            }
            // the same as the catalog decides from the probe (format and camera)
            let probe = crate::files::probe_bytes(name, &bytes).unwrap();
            let mut p = lightcraft_catalog::Photo::new(
                lightcraft_catalog::PhotoId(1),
                lightcraft_catalog::Source::File { path: name.into() },
                name,
                &probe.format,
                1,
                1,
                "",
            );
            p.kind = probe.kind;
            p.meta = probe.meta.clone();
            assert_eq!(p.relative_wb(), info.relative_wb, "{name}");
            assert_eq!(probe.as_shot_wb, Some((info.as_shot_temp, info.as_shot_tint)), "{name}");
        }
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
            RawFormat::Mrw,
            RawFormat::Erf,
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
            lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
            return;
        };
        let raw = match lightcraft_raw::decode(&bytes) {
            Ok(raw) => raw,
            Err(lightcraft_raw::RawError::Unsupported(why)) => {
                lightcraft_testkit::skip!(Feature, "CR3 sensor decoder: {why}");
                return;
            }
            Err(error) => panic!("CR3 corpus failed to decode: {error}"),
        };
        assert_eq!(raw.format, RawFormat::Cr3);
        assert_eq!(raw.info().developed_size(), (6000, 4000));
        let transform = lightcraft_raw::color::camera_transform(&raw, lightcraft_raw::color::as_shot_white_xy(&raw));
        let look = fit_preview(&raw, &bytes, &transform, None).0;
        eprintln!("R100 guarded colour fit accepted: {}", look.is_some());
        assert!(look.is_some(), "this R100 corpus photo has enough matching colour after camera-framing alignment");
        let (_, info) = crate::files::load_bytes(&bytes, 400).unwrap();
        assert!(info.raw && info.relative_wb);
        assert_eq!((info.as_shot_temp, info.as_shot_tint), (6500.0, 0.0));
        assert_eq!(info.camera_tone.is_some(), look.is_some());
    }

    /// A public sample of `model` (skipped without the corpus) takes its colour from the bundled profile for `model`
    /// and passes the acceptance gates with it: the fitted look keeps the profile's matrix and table, which the
    /// photo's own fit (the fallback when a profile is rejected) would not.
    fn corpus_uses_the_bundled_profile(file: &str, model: &str) {
        let path = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw")
            .join(file);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
                return;
            }
            Err(e) => panic!("{}: {e}", path.display()),
        };
        let (_, json) = crate::camera_profiles::BUNDLED.iter().find(|(m, _)| *m == model).expect("the model is built in");
        let bundled: crate::camera_profiles::CameraProfile = serde_json::from_str(json).unwrap();
        let used = crate::camera_profiles::get(model).expect("a profile for the model");
        assert_eq!(*used, bundled, "a local {model} profile overrides the built-in one; remove it to run this test");
        let raw = lightcraft_raw::decode(&bytes).unwrap();
        assert_eq!(raw.metadata.model.as_deref(), Some(model));
        let transform = lightcraft_raw::color::camera_transform(&raw, lightcraft_raw::color::as_shot_white_xy(&raw));
        let look = fit_preview(&raw, &bytes, &transform, None).0.expect("fit accepted");
        let expected = bundled.matrix().mul(&transform.matrix.inverse().unwrap());
        for (row, want) in look.matrix.0.iter().zip(expected.0) {
            for (got, want) in row.iter().zip(want) {
                assert!((got - want).abs() < 1e-12, "the look's matrix is not the profile's: {:?} vs {:?}", look.matrix.0, expected.0);
            }
        }
        assert_eq!(look.hue_sat, bundled.hue_sat, "the look's table is not the profile's");
    }

    #[test]
    fn corpus_a7cr_uses_the_bundled_profile() {
        corpus_uses_the_bundled_profile("arw-sony-a7cr-lossless-l.arw", "ILCE-7CR");
    }

    #[test]
    fn corpus_a99m2_uses_the_bundled_profile() {
        corpus_uses_the_bundled_profile("arw-sony-a99m2.arw", "ILCA-99M2");
    }

    /// The public Z 6II NEF wasn't one of the photos its bundled profile was fitted to.
    #[test]
    fn corpus_z6ii_uses_the_bundled_profile() {
        corpus_uses_the_bundled_profile("nef-nikon-z6ii-lossless.nef", "NIKON Z 6_2");
    }

    /// The public Z 8 NEF (Z 14-24 f/2.8 S) wasn't one of the photos its bundled profile was fitted to.
    #[test]
    fn corpus_z8_uses_the_bundled_profile() {
        corpus_uses_the_bundled_profile("nef-nikon-z8-lossless.nef", "NIKON Z 8");
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
            lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
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
                    lightcraft_testkit::skip!(Corpus, "{name} absent");
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
            lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
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
            lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
            return;
        };
        let (_, info) = crate::files::load_bytes(&bytes, 400).unwrap();
        assert!(info.camera_tone.is_some(), "no camera look fitted");
    }

    /// A merged DNG of a raw's camera RGB (issue #965) carries the colour the raw opens with: its tags give the same
    /// white balance, colour and brightness as the fitted look at the as-shot white, the look's hue/saturation
    /// table and its tone curve; without a look, those of the camera transform.
    #[test]
    fn dng_colour_renders_like_the_raws_starting_colour() {
        use lightcraft_raw::color::{as_shot_white_of, camera_transform_of};
        let t = camera_transform_of(&ColorData::default(), Xy::new(0.36, 0.37));
        assert!(t.wb.iter().any(|w| (w - 1.0).abs() > 0.1), "a white balance to keep: {:?}", t.wb);
        let look = dro_test_look(&std::array::from_fn(|i| 0.005 * 1.2f32.powi(i as i32)), &|x| 0.95 * x / (x + 0.1));
        let samples = [[0.2, 0.3, 0.1], [0.05, 0.02, 0.4], [0.6, 0.6, 0.6], [0.01, 0.5, 0.03]];
        let render = |m: &Mat3, wb: [f32; 3], ev: f64, c: [f64; 3]| m.apply(std::array::from_fn(|i| c[i] * wb[i] as f64)).map(|v| v * ev.exp2());
        for look in [Some(&look), None] {
            let dng = dng_colour(&ColorData::default(), &crate::files::DngWhite::At(t.white_xy), &t, look).unwrap();
            let back = camera_transform_of(&dng, as_shot_white_of(&dng, None));
            for i in 0..3 {
                assert!((back.wb[i] - t.wb[i]).abs() < 1e-4 * t.wb[i], "{:?} vs {:?}", back.wb, t.wb);
            }
            let m = look.map_or(t.matrix, |l| l.matrix.mul(&t.matrix));
            for c in samples {
                let (want, got) = (render(&m, t.wb, t.baseline_exposure, c), render(&back.matrix, back.wb, dng.baseline_exposure, c));
                assert!(want.iter().zip(got).all(|(a, b)| (a - b).abs() < 1e-5), "{c:?}: {want:?} vs {got:?}");
            }
            assert_eq!(dng.profile.hue_sat_for(1.0), look.and_then(|l| l.hue_sat.clone()));
            let tone = dng.profile.tone_curve.as_ref().and_then(crate::files::dng_tone_curve);
            match look {
                Some(look) => {
                    let tone = tone.expect("the look's tone curve");
                    let worst =
                        (0..=120).map(|i| 2f32.powf(-10.0 + i as f32 / 12.0)).map(|x| (tone.apply(x) - look.tone.apply(x)).abs()).fold(0.0, f32::max);
                    // (the reader resamples a `ProfileToneCurve` at 32 knots: 0.0031 of white at worst here)
                    assert!(worst < 4e-3, "{worst}");
                }
                None => assert!(tone.is_none()),
            }
        }
    }

    /// The merged DNG declares the white balance its source shows, also when the look's matrix doesn't keep white
    /// neutral, as fitted looks often don't, and renders at As Shot as the source does. Real ARW merges declared their
    /// colour matrix from the look: As Shot about 7000 K / +115 where the ARWs show 4800 K / +8.
    #[test]
    fn dng_colour_declares_the_white_balance_the_source_shows() {
        use crate::files::DngWhite;
        use lightcraft_raw::color::{as_shot_white_of, camera_transform_of, wb_multipliers};
        let tags = ColorData::default();
        let as_shot = [2.5039f32, 1.0, 1.5078];
        let t = camera_transform_of(&tags, as_shot_white_of(&tags, Some(as_shot)));
        let mut look = dro_test_look(&std::array::from_fn(|i| 0.005 * 1.2f32.powi(i as i32)), &|x| 0.95 * x / (x + 0.1));
        look.matrix = Mat3([[1.75, -0.4, -0.2], [-0.2, 1.35, -0.2], [0.05, -0.3, 1.3]]);
        let tint = look.matrix.apply([1.0; 3]);
        assert!((tint[0] - tint[1]).abs() > 0.1, "a look that tints white: {tint:?}");
        let renders_as_shot = |dng: &ColorData| {
            let r = camera_transform_of(dng, as_shot_white_of(dng, None));
            assert!(r.wb.iter().zip(t.wb).all(|(a, b)| (a - b).abs() < 1e-4 * b), "{:?} vs {:?}", r.wb, t.wb);
            let m = look.matrix.mul(&t.matrix);
            for c in [[0.2, 0.3, 0.1], [0.05, 0.02, 0.4], [0.6, 0.6, 0.6]] {
                let want = m.apply(std::array::from_fn(|i| c[i] * t.wb[i] as f64)).map(|v| v * t.baseline_exposure.exp2());
                let got = r.matrix.apply(std::array::from_fn(|i| c[i] * r.wb[i] as f64)).map(|v| v * dng.baseline_exposure.exp2());
                assert!(want.iter().zip(got).all(|(a, b)| (a - b).abs() < 1e-4), "{c:?}: {want:?} vs {got:?}");
            }
        };
        // a white the source's scale shows (Sony's presets, the relative scale's 6500 K / 0)
        for xy in [lightcraft_color::cct::temp_tint_to_xy(5600.0, 3.0), lightcraft_color::cct::temp_tint_to_xy(6500.0, 0.0)] {
            let dng = dng_colour(&tags, &DngWhite::At(xy), &t, Some(&look)).unwrap();
            let back = as_shot_white_of(&dng, None);
            assert!((back.x - xy.x).abs() < 1e-6 && (back.y - xy.y).abs() < 1e-6, "as shot {back:?}, shown {xy:?}");
            renders_as_shot(&dng);
        }
        // a camera with measured matrices (the ILCE-7M3): every white read through them, as for the source
        let camera = lightcraft_raw::spectral::find(Some("SONY"), "ILCE-7M3").expect("a covered camera");
        let mut model = tags.clone();
        assert!(camera.fill(&mut model));
        let shown = lightcraft_raw::spectral::as_shot_white_through(camera, &tags, Some(as_shot)).unwrap();
        let dng = dng_colour(&tags, &DngWhite::Matrices(Box::new(model.clone())), &t, Some(&look)).unwrap();
        let back = as_shot_white_of(&dng, None);
        assert!((back.x - shown.x).abs() < 1e-5 && (back.y - shown.y).abs() < 1e-5, "as shot {back:?}, shown {shown:?}");
        for w in [Xy::new(0.3127, 0.329), Xy::new(0.4476, 0.4074), Xy::new(0.3457, 0.3585)] {
            let (a, b) = (wb_multipliers(&model, w), wb_multipliers(&dng, w));
            assert!(a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-6 * a), "{w:?}: {a:?} vs {b:?}");
        }
        renders_as_shot(&dng);
    }

    /// Photo Merge gives a panorama the white balance of its frames' vendor multipliers (issue #1144). A raw without
    /// colour matrices (CR2, NEF…) is merged with the tags above, whose neutral is that white balance already: taking
    /// it from the multipliers again leaves the merged DNG rendering as the tags do.
    #[test]
    fn a_merges_white_balance_from_the_multipliers_is_the_one_the_starting_colour_tags_have() {
        use lightcraft_raw::color::{as_shot_white_of, camera_transform_of};
        for (model, multipliers) in [("Canon EOS 6D Mark II", [2.0, 1.0, 1.5]), ("Canon EOS 600D", [1.9, 1.0, 1.7])] {
            let mut raw = raw_of(RawFormat::Cr2, "Canon", model, Default::default());
            raw.wb_multipliers = Some(multipliers);
            let tags = starting_colour_tags(&mut raw, &[]).expect("tags for a raw without colour matrices");
            let neutral = tags.as_shot_neutral.expect("the tags carry the as-shot white balance");
            // the reciprocal of the multipliers, up to the scale a neutral is given in
            let scale = neutral[1] * multipliers[1] as f64;
            for i in 0..3 {
                assert!((neutral[i] * multipliers[i] as f64 - scale).abs() < 1e-3, "{model}: {neutral:?} vs 1 / {multipliers:?}");
            }
            let mut merged = lightcraft_merge::FrameColor::Camera(Box::new(tags.clone()));
            merged.set_as_shot_multipliers(multipliers);
            let lightcraft_merge::FrameColor::Camera(merged) = merged else { panic!("camera colour") };
            let (a, b) = (camera_transform_of(&tags, as_shot_white_of(&tags, None)), camera_transform_of(&merged, as_shot_white_of(&merged, None)));
            for i in 0..3 {
                assert!((a.wb[i] - b.wb[i]).abs() < 1e-4 * a.wb[i], "{model}: {:?} vs {:?}", a.wb, b.wb);
            }
            for (ra, rb) in a.matrix.0.iter().zip(b.matrix.0) {
                assert!(ra.iter().zip(rb).all(|(x, y)| (x - y).abs() < 1e-4), "{model}: {:?} vs {:?}", a.matrix, b.matrix);
            }
            assert_eq!(a.baseline_exposure, b.baseline_exposure);
        }
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
                    lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
                    continue;
                }
                Err(e) => panic!("{}: {e}", path.display()),
            };
            assert_eq!(lightcraft_raw::embedded_preview_dynamic_range_optimized(&bytes), Some(dro), "{name}");
            let mut raw = lightcraft_raw::decode(&bytes).unwrap();
            raw.opcodes.list3.retain(|op| !op.is_lens_correction());
            let transform = lightcraft_raw::color::camera_transform(&raw, lightcraft_raw::color::as_shot_white_xy(&raw));
            let look = fit_preview(&raw, &bytes, &transform, None).0.expect("a camera look");
            // the look fitted to the JPEG, as before this change
            let (sensor, reference, clipped) = proxies(&raw, &bytes, &transform, PROXY, 0.0, None).unwrap();
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
        // (the GFX100S shares the GFX 100's measured sensor, so its white balance is in Kelvin through those
        // matrices; the X-T2 and X-T20 have none and stay relative)
        for (name, accepted, kelvin) in
            [("raf-fuji-xt2-865.raf", true, false), ("raf-fuji-gfx100s-4503.raf", true, true), ("raf-fuji-xt20-compressed.raf", true, false)]
        {
            let Ok(bytes) = std::fs::read(dir.join(name)) else { continue };
            let header = crate::files::probe_bytes(name, &bytes).unwrap();
            let (_, small) = crate::files::load_bytes(&bytes, 400).unwrap();
            let (_, large) = crate::files::load_bytes(&bytes, 1200).unwrap();
            assert_eq!(small.camera_tone.is_some(), accepted, "{name}");
            assert_eq!(small.camera_tone, large.camera_tone, "{name}: resolution changed the fit");
            assert_eq!(header.as_shot_wb, Some((small.as_shot_temp, small.as_shot_tint)), "{name}");
            assert!(small.raw && small.relative_wb != kelvin && small.legacy_relative_wb == kelvin, "{name}: {small:?}");
            if kelvin {
                assert!(small.as_shot_temp != 6500.0 && (2000.0..50000.0).contains(&small.as_shot_temp), "{name}: {}", small.as_shot_temp);
            } else {
                assert!(small.as_shot_temp == 6500.0 && small.as_shot_tint == 0.0, "{name}");
            }
        }
    }

    /// Sony ARWs without measured matrices read Kelvin from their white-balance presets
    /// (`lightcraft_raw::kelvin_presets`): the ILCE-3500 sample was shot at 5600 K on the camera's Kelvin setting and
    /// the ILCE-7S sample on its Shade preset (7500 K on Sony's scale); the SLT-A33's Auto white is within 20 K of its
    /// Daylight preset. Header and decode agree. Skips cleanly without these CC0 corpus files.
    #[test]
    fn corpus_arw_white_balance_in_kelvin_through_the_presets() {
        let dir = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw");
        for (name, kelvin) in [("arw-sony-a3500-5600k.arw", 5600.0), ("arw-sony-a7s-shade.arw", 7500.0), ("arw-sony-a33.arw", 5311.0)] {
            let Ok(bytes) = std::fs::read(dir.join(name)) else {
                eprintln!("skip: {name} absent");
                continue;
            };
            let header = crate::files::probe_bytes(name, &bytes).unwrap();
            let (_, info) = crate::files::load_bytes(&bytes, 400).unwrap();
            assert_eq!(header.as_shot_wb, Some((info.as_shot_temp, info.as_shot_tint)), "{name}");
            assert!(info.raw && !info.relative_wb && info.legacy_relative_wb, "{name}: {info:?}");
            assert!(
                (info.as_shot_temp - kelvin).abs() <= 20.0 && info.as_shot_tint.abs() <= 2.0,
                "{name}: {} {}",
                info.as_shot_temp,
                info.as_shot_tint
            );
            let camera = format!("SONY {}", lightcraft_meta::extract(&bytes).model.unwrap_or_default());
            assert!(!lightcraft_catalog::relative_wb_camera("ARW", &camera), "{camera}");
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
        let check = HighlightCheck { pairs: highlights(&sensor, &reference), dro: false, grey: false };
        let (_, attempt) = search_ordered(&sensor, &reference, None, TONE_FITS, &check).unwrap();
        assert_eq!(attempt, Attempt { tone: ToneFit::Quantile, profile: false, away_from_edges: false, dark: false });
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

    fn near_black_scene() -> (Rgb32f, Rgb32f) {
        let mut sensor = Rgb32f::new(96, 64);
        let mut reference = sensor.clone();
        for (i, (input, output)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            let y = if i % 60 == 0 { 0.3 + (i % 7) as f32 * 0.01 } else { 0.006 + ((i * 7919) % 6144) as f32 / 6144.0 * 0.075 };
            let rgb = [1.15, 1.0, 0.75];
            *input = rgb.map(|v| v * y / luminance_2020(rgb));
            *output = input.map(|v| v * 0.08);
        }
        (sensor, reference)
    }

    #[test]
    fn a_near_black_scene_gets_tone_without_learning_colour() {
        let (sensor, reference) = near_black_scene();
        assert!(collect_pairs(&sensor, &reference, 0.005, None, false).is_none(), "too few midtones even for the partial look");
        let mut raw = raw_of(RawFormat::Nef, "NIKON CORPORATION", "NIKON D610", Default::default());
        let (_, _, look) = starting_colour_with(
            &mut raw,
            |_, _| (fit_look(&sensor, &reference, &vec![false; sensor.data.len()], None), false),
            |_, _| None,
            |_, _| fit_dark_tone(&sensor, &reference, &vec![false; sensor.data.len()]),
        );
        let look = look.expect("tone from the dark pixels");
        assert_eq!(look.matrix, Mat3::IDENTITY);
        assert!(look.hue_sat.is_none());
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
        let error = |tone: &ToneMap| {
            sensor
                .data
                .iter()
                .zip(&reference.data)
                .map(|(x, y)| (luma(displayed(x.map(f64::from), tone)) - luma(y.map(f64::from))).powi(2))
                .sum::<f64>()
        };
        assert!(error(&tone) < error(&ToneMap::new(0.0, 0.0, 0.0)) * 0.001);
        assert_eq!(look.tone.apply(0.0), 0.0);
        let mut previous = 0.0;
        for i in 0..10_000 {
            let y = look.tone.apply(i as f32 / 10_000.0);
            assert!(y.is_finite() && y >= previous && y <= 1.0);
            previous = y;
        }
        // Models with spectral matrices keep them before the dark tone is fitted.
        let mut raw = raw_of(RawFormat::Cr2, "Canon", "Canon EOS 600D", Default::default());
        let (_, t, look) = starting_colour_with(
            &mut raw,
            |_, _| (None, false),
            |_, _| None,
            |raw, t| {
                assert!(!t.matrix_is_fallback && lightcraft_raw::color::has_matrix(&raw.color));
                fit_dark_tone(&sensor, &reference, &vec![false; sensor.data.len()])
            },
        );
        assert!(!t.matrix_is_fallback);
        assert_eq!(look.expect("tone on top of spectral colour").matrix, Mat3::IDENTITY);
    }

    #[test]
    fn dark_tone_holdout_never_enters_training() {
        let (sensor, mut reference) = near_black_scene();
        let none = vec![false; sensor.data.len()];
        let original = fit_dark_tone(&sensor, &reference, &none).expect("dark tone");
        // Every pixel clears the wider signal floors. Changing only its held-out third must
        // change the measured error, but cannot change the fitted curve.
        for p in reference.data.iter_mut().step_by(3) {
            *p = p.map(|v| v * 1.03);
        }
        let changed = fit_dark_tone(&sensor, &reference, &none).expect("still clearly better than the fallback");
        assert_eq!(changed.tone, original.tone);
    }

    #[test]
    fn dark_tone_requires_signal_correlation_and_unclipped_highlights() {
        let (sensor, reference) = near_black_scene();
        let none = vec![false; sensor.data.len()];
        let mut unrelated = reference.clone();
        for (i, p) in unrelated.data.iter_mut().enumerate() {
            *p = reference.data[(i * 3571) % reference.data.len()];
        }
        assert!(fit_dark_tone(&sensor, &unrelated, &none).is_none(), "unrelated dark JPEG");
        let mut flat = reference.clone();
        flat.data.fill([0.002; 3]);
        assert!(fit_dark_tone(&sensor, &flat, &none).is_none(), "a quantized flat JPEG cannot teach tone");
        let mut sparse = reference.clone();
        sparse.data.iter_mut().skip(80).for_each(|p| *p = [0.0; 3]);
        assert!(fit_dark_tone(&sensor, &sparse, &none).is_none(), "fewer than 256 usable pairs");
        let mut bright = reference.clone();
        bright.data[0] = [0.95; 3];
        assert!(fit_dark_tone(&sensor, &bright, &none).is_none(), "dark pairs cannot constrain a bright highlight");
        let mut clipped = none;
        clipped[0] = true;
        let original = fit_dark_tone(&sensor, &reference, &clipped).expect("tone without the clipped pixel");
        let mut changed = sensor.clone();
        changed.data[0] = [0.18; 3];
        let look = fit_dark_tone(&changed, &reference, &clipped).expect("the clipped pixel is still closer to its JPEG");
        assert_eq!(look.tone, original.tone, "clipped samples cannot change the fitted curve");
        let mut highlight = reference.clone();
        highlight.data[0] = [0.8; 3];
        assert!(fit_dark_tone(&changed, &highlight, &clipped).is_none(), "the curve cannot darken a clipped highlight away from the JPEG");
        assert!(fit_dark_tone(&sensor, &Rgb32f::new(64, 96), &clipped).is_none(), "mismatched dimensions");
        assert!(fit_dark_tone(&sensor, &reference, &[]).is_none(), "mismatched clip mask");
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
        let check = HighlightCheck { pairs: highlights(&sensor, &reference), dro: false, grey: false };
        assert!(fit_partial(&sensor, &reference, &check).is_none(), "no clear gain over the fallback");
        // an unrelated picture
        for (i, p) in reference.data.iter_mut().enumerate() {
            *p = [0.05 + (i % 7) as f32 * 0.07, 0.05 + (i % 19) as f32 * 0.02, 0.05 + (i % 29) as f32 * 0.01];
        }
        assert!(fit_look(&sensor, &reference, &none, None).is_none(), "unrelated JPEG");
        // a black-and-white JPEG of the scene
        reference.data.iter_mut().zip(&sensor.data).for_each(|(r, s)| *r = [0.8 * luminance_2020(*s).sqrt(); 3]);
        assert!(fit_look(&sensor, &reference, &none, None).is_none(), "monochrome JPEG");
    }

    /// A coloured scene whose camera JPEG is black and white: the camera's tone curve applied to the scene's
    /// luminance, grey in every pixel.
    fn black_and_white_scene() -> (Rgb32f, Rgb32f) {
        let (w, h) = (96usize, 64usize);
        let mut sensor = Rgb32f::new(w, h);
        for (i, p) in sensor.data.iter_mut().enumerate() {
            let ev = 0.01 + (i % 53) as f32 * 0.009;
            *p = [ev * (0.6 + (i % 11) as f32 * 0.08), ev, ev * (0.6 + (i % 13) as f32 * 0.07)];
        }
        let mut reference = sensor.clone();
        reference.map_in_place(|p| [1.0 - (-3.0 * luminance_2020(p)).exp(); 3]);
        (sensor, reference)
    }

    #[test]
    fn black_and_white_jpegs_are_recognised() {
        let (sensor, mut reference) = black_and_white_scene();
        assert!(monochrome(&reference));
        assert!(!monochrome(&sensor), "a colour JPEG");
        // a few coloured pixels (JPEG noise) are allowed, one in a hundred is colour
        let n = reference.data.len();
        reference.data.iter_mut().take(n / 1000).for_each(|p| p[0] += 0.05);
        assert!(monochrome(&reference));
        reference.data.iter_mut().take(n / 100).for_each(|p| p[0] += 0.05);
        assert!(!monochrome(&reference));
        // a sepia-toned monochrome
        let (_, mut toned) = black_and_white_scene();
        toned.map_in_place(|p| [p[0] * 1.08, p[1], p[2] * 0.85]);
        assert!(!monochrome(&toned));
        assert!(!monochrome(&Rgb32f::new(8, 8)), "too small to tell");
        // the check reads pixels, not the picture style: a colour JPEG whose pixels are all grey within 0.002 counts
        let (_, mut faint) = black_and_white_scene();
        faint.map_in_place(|p| [p[0] + 0.001, p[1], p[2] - 0.0005]);
        assert!(monochrome(&faint));
        faint.map_in_place(|p| [p[0] + 0.003, p[1], p[2]]);
        assert!(!monochrome(&faint));
        // pixels that aren't numbers can't vouch for grey
        let (_, mut broken) = black_and_white_scene();
        broken.data.iter_mut().take(n / 100).for_each(|p| *p = [f32::NAN; 3]);
        assert!(!monochrome(&broken));
    }

    /// Issue #577: a black-and-white JPEG gives no look (no colour to fit), but its tone curve renders the scene's
    /// brightness like the camera, with the colour left as it was and the chroma curve neutral.
    #[test]
    fn a_black_and_white_jpeg_gives_its_tone_curve() {
        let (sensor, reference) = black_and_white_scene();
        let none = vec![false; sensor.data.len()];
        assert!(fit_look(&sensor, &reference, &none, None).is_none(), "no colour to fit");
        let look = fit_tone_only(&sensor, &reference, None).expect("a tone curve");
        assert!(look.matrix == Mat3::IDENTITY && look.hue_sat.is_none(), "the colour is left as it was");
        assert!(look.tone.chroma().iter().all(|k| *k == 1.0), "the chroma curve stays neutral: {:?}", look.tone.chroma());
        let n = sensor.data.len() as f64;
        let mean = |tone: &ToneMap| sensor.data.iter().map(|p| luma(displayed(p.map(f64::from), tone))).sum::<f64>() / n;
        let target = reference.data.iter().map(|p| f64::from(luminance_2020(*p))).sum::<f64>() / n;
        let toned = mean(&ToneMap::camera(&look.tone, 0.0, 0.0, 0.0));
        assert!((toned / target - 1.0).abs() < 0.05, "{toned} vs {target}");
        assert!(mean(&ToneMap::new(0.0, 0.0, 0.0)) < target * 0.8, "the default curve is darker");
        // with a camera profile's colour, the look carries it
        let matrix = Mat3([[1.2, -0.15, -0.05], [-0.1, 1.15, -0.05], [-0.02, -0.1, 1.12]]);
        let look = fit_tone_only(&sensor, &reference, Some((matrix, None))).expect("a tone curve with the profile's colour");
        assert_eq!(look.matrix, matrix);
    }

    /// A camera profile's colour can fail the checks where the photo's own colour passes, so [`monochrome_look`] tries
    /// again on the transform the photo has, as [`fit_look`] tries the photo's own colour after a profile's.
    #[test]
    fn a_rejected_profile_leaves_the_tone_curve_on_the_photos_own_colour() {
        // saturated colours, and a profile whose colour gets their luminance wrong (red and blue swapped)
        let hues = [[1.0, 0.15, 0.1], [0.1, 1.0, 0.15], [0.15, 0.1, 1.0], [1.0, 0.9, 0.1], [0.1, 0.9, 1.0], [1.0, 0.1, 0.9]];
        let mut sensor = Rgb32f::new(96, 64);
        for (i, p) in sensor.data.iter_mut().enumerate() {
            let ev = 0.02 + (i % 47) as f32 * 0.008;
            *p = hues[i % 6].map(|c| c * ev);
        }
        // a JPEG a little brighter than the default curve, so a curve has to fit closely to be clearly better
        let neutral = ToneMap::new(0.0, 0.0, 0.0);
        let mut reference = sensor.clone();
        reference.map_in_place(|p| [(1.15 * luma(displayed(p.map(f64::from), &neutral))) as f32; 3]);
        let swapped = Mat3([[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]]);
        assert!(fit_tone_only(&sensor, &reference, Some((swapped, None))).is_none(), "the profile's colour fails the checks");
        let own = fit_tone_only(&sensor, &reference, None);
        assert!(own.is_some(), "the photo's own colour passes");
        // and the step runs for a model with a bundled profile (the ILCE-7M4 also has spectral matrices)
        assert!(crate::camera_profiles::get("ILCE-7M4").is_some());
        let mut raw = raw_of(RawFormat::Arw, "SONY", "ILCE-7M4", Default::default());
        let (_, t, look) = starting_colour_with(
            &mut raw,
            |_, _| (None, true),
            |_, t| {
                assert!(!t.matrix_is_fallback, "on the spectral matrices");
                own.clone()
            },
            |_, _| None,
        );
        assert!(look.is_some() && !t.matrix_is_fallback);
    }

    /// [`monochrome_look`] on a synthetic raw file: an ILCE-7M4 (it has a bundled camera profile) whose camera JPEG,
    /// in a minimal TIFF, is black and white gets the tone curve, on the colour of the transform it is given; the
    /// same file with a tinted JPEG gets nothing from this step.
    #[test]
    fn monochrome_look_reads_the_files_black_and_white_jpeg() {
        use lightcraft_raw::{BlackLevel, ColorData, OpcodeLists, Orientation, RawData, Rect};
        let (w, h) = (192usize, 128usize);
        let level = |x: usize, y: usize| 0.01 + 0.5 * (x as f32 / w as f32) * (0.3 + 0.7 * y as f32 / h as f32);
        let raw = RawImage {
            format: RawFormat::Arw,
            width: w,
            height: h,
            cpp: 3,
            data: RawData::F32((0..w * h).flat_map(|i| [level(i % w, i / w); 3]).collect()),
            cfa: None,
            bits: 16,
            black: BlackLevel::uniform(0.0),
            white: vec![1.0],
            active_area: Rect::new(0, 0, w, h),
            crop: Rect::new(0, 0, w, h),
            orientation: Orientation::from_exif(1),
            color: ColorData::default(),
            wb_multipliers: Some([1.0; 3]),
            linearized: true,
            pixel_aspect: 1.0,
            opcodes: OpcodeLists::default(),
            metadata: lightcraft_meta::Metadata { make: Some("SONY".into()), model: Some("ILCE-7M4".into()), ..Default::default() },
            lens_metadata: Default::default(),
        };
        assert!(crate::camera_profiles::get("ILCE-7M4").is_some());
        // the camera's rendering: a bright curve on the scene's luminance, as a grey (or tinted) 8-bit sRGB JPEG
        let file = |tint: [f32; 3]| {
            let srgb = |v: f32| if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            let pixels: Vec<u8> = (0..w * h)
                .flat_map(|i| {
                    let v = 1.0 - (-3.0 * level(i % w, i / w)).exp();
                    tint.map(|t| (srgb(v * t).clamp(0.0, 1.0) * 255.0).round() as u8)
                })
                .collect();
            let image = lightcraft_codecs::EncodeImage::new(w as u32, h as u32, 3, lightcraft_codecs::Samples::U8(&pixels));
            let jpeg = lightcraft_codecs::encode_jpeg(&image, 95, lightcraft_codecs::ChromaSubsampling::S444, &Default::default()).unwrap();
            // II*, IFD0 at 8 with JPEGInterchangeFormat (0x201) and its length (0x202), the JPEG after it
            let mut tiff = b"II*\0\x08\0\0\0\x02\0".to_vec();
            for (tag, value) in [(0x0201u16, 38u32), (0x0202, jpeg.len() as u32)] {
                tiff.extend(tag.to_le_bytes());
                tiff.extend(4u16.to_le_bytes());
                tiff.extend(1u32.to_le_bytes());
                tiff.extend(value.to_le_bytes());
            }
            tiff.extend(0u32.to_le_bytes());
            assert_eq!(tiff.len(), 38);
            tiff.extend(jpeg);
            tiff
        };
        let t = lightcraft_raw::color::camera_transform(&raw, lightcraft_raw::color::as_shot_white_xy(&raw));
        let look = monochrome_look(&raw, &file([1.0; 3]), &t, None).expect("a tone curve from the black-and-white JPEG");
        assert!(look.matrix == Mat3::IDENTITY && look.hue_sat.is_none(), "on the transform's colour");
        let curve = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
        let mid = curve.apply(0.25);
        assert!((mid - (1.0 - (-0.75f32).exp())).abs() < 0.05, "the camera's curve at 0.25: {mid}");
        assert!(monochrome_look(&raw, &file([1.0, 0.9, 0.8]), &t, None).is_none(), "a tinted JPEG is not black and white");
    }

    /// The black-and-white fit has the partial look's checks: a JPEG of another picture, or one the default curve
    /// already renders, gives no look.
    #[test]
    fn a_black_and_white_jpeg_gives_a_tone_curve_only_when_clearly_better() {
        let (sensor, mut reference) = black_and_white_scene();
        let neutral = ToneMap::new(0.0, 0.0, 0.0);
        reference.data.iter_mut().zip(&sensor.data).for_each(|(r, s)| *r = [luma(displayed(s.map(f64::from), &neutral)) as f32; 3]);
        assert!(monochrome(&reference));
        assert!(fit_tone_only(&sensor, &reference, None).is_none(), "no clear gain over the default curve");
        for (i, p) in reference.data.iter_mut().enumerate() {
            *p = [0.05 + (i % 29) as f32 * 0.02; 3];
        }
        assert!(fit_tone_only(&sensor, &reference, None).is_none(), "another picture");
        // a flat grey JPEG ranks nothing, even when the scene's pixels are stored in brightness order (with ties
        // ranked in pixel order it looked perfectly related, and the curve flattened the photo to one grey)
        let mut sorted = sensor.clone();
        sorted.data.sort_by(|a, b| luminance_2020(*a).total_cmp(&luminance_2020(*b)));
        reference.data.iter_mut().for_each(|p| *p = [0.4; 3]);
        assert!(fit_tone_only(&sorted, &reference, None).is_none(), "a flat JPEG");
    }

    /// ILCE-7RM6 full-frame: the 66.5 MP camera JPEG exceeded the old 64 MP cap, so no look was fitted.
    #[test]
    fn corpus_arw6_full_frame_gets_a_camera_look() {
        const { assert!(FIT_PREVIEW_MAX_PIXELS >= 100_000_000) };
        let path = std::env::var_os("LIGHTCRAFT_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
            .join("raw/arw6-sony-ilce7rm6-ff-land-c.arw");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {} absent", path.display());
            return;
        };
        let (_, info) = crate::files::load_bytes(&bytes, 400).unwrap();
        assert!(info.camera_tone.is_some(), "no camera look fitted");
    }

    /// A smooth synthetic scene (sensor proxy) and its camera rendering, `dx` pixels to the right.
    fn scene_pair(w: usize, h: usize, dx: usize) -> (Rgb32f, Rgb32f) {
        let scene = |x: f32, y: f32| -> [f32; 3] {
            let l = 0.03 + 0.4 * (0.5 + 0.5 * (x * 0.21).sin() * (y * 0.17).cos()) + 0.2 * ((x - 30.0).hypot(y - 20.0) < 12.0) as u8 as f32;
            [l * (0.8 + 0.3 * (y / h as f32)), l, l * (1.1 - 0.4 * (x / w as f32))]
        };
        let camera = |p: [f32; 3]| {
            let y = luminance_2020(p);
            p.map(|v| v * (1.0 - (-2.5 * y).exp()) / y)
        };
        let mut sensor = Rgb32f::new(w, h);
        let mut reference = sensor.clone();
        for y in 0..h {
            for x in 0..w {
                sensor.data[y * w + x] = scene(x as f32, y as f32);
                reference.data[y * w + x] = camera(scene(x as f32 - dx as f32, y as f32));
            }
        }
        (sensor, reference)
    }

    /// The structural gate accepts the same picture whatever its tone, and refuses a shifted, a broken (stripes), a
    /// flat and a differently sized preview.
    #[test]
    fn dng_structure_gate_refuses_shifted_broken_and_unrelated_previews() {
        let (sensor, reference) = scene_pair(96, 64, 0);
        assert!(structure_agrees(&sensor, &reference));
        assert!(structure_agrees(&sensor, &scene_pair(96, 64, 1).1), "one proxy pixel (lens correction) is tolerated");
        assert!(!structure_agrees(&sensor, &scene_pair(96, 64, 3).1), "differently framed");
        let mut stripes = reference.clone();
        for (i, p) in stripes.data.iter_mut().enumerate() {
            *p = if (i / 96) % 4 < 2 { [0.6, 0.1, 0.5] } else { [0.05, 0.4, 0.1] };
        }
        assert!(!structure_agrees(&sensor, &stripes), "a broken preview");
        let mut flat = reference.clone();
        flat.data.fill([0.2; 3]);
        assert!(!structure_agrees(&sensor, &flat));
        assert!(!structure_agrees(&sensor, &scene_pair(95, 64, 0).1));
    }

    /// With the file's colour given as the identity, only the tone is fitted: the colour stays and a camera that
    /// renders mid-grey brighter than the default tone map is followed.
    #[test]
    fn dng_tone_fit_keeps_the_files_colour_and_follows_the_camera_tone() {
        let (mut sensor, mut reference) = scene_pair(96, 64, 0);
        for image in [&mut sensor, &mut reference] {
            for p in image.data.iter_mut().step_by(5) {
                *p = [p[0] * 1.6, p[1], p[2] * 0.5];
            }
        }
        let colour = Some((Mat3::IDENTITY, None));
        let check = HighlightCheck { pairs: highlights(&sensor, &reference), dro: false, grey: false };
        let look =
            fit_attempt(&sensor, &reference, &colour, Attempt { tone: ToneFit::Quantile, profile: true, away_from_edges: false, dark: true }, &check)
                .expect("tone fitted");
        assert_eq!(look.matrix, Mat3::IDENTITY);
        assert!(look.hue_sat.is_none());
        let (fitted, default) = (ToneMap::camera(&look.tone, 0.0, 0.0, 0.0), ToneMap::new(0.0, 0.0, 0.0));
        let target = 1.0 - (-2.5f32 * 0.18).exp();
        assert!((fitted.apply(0.18) - target).abs() < 0.03, "{} vs {target}", fitted.apply(0.18));
        assert!(fitted.apply(0.18) > default.apply(0.18) + 0.05, "{} vs default {}", fitted.apply(0.18), default.apply(0.18));
        assert!(!clips_more(&sensor, &reference, &fitted, &fitted), "a tone never clips more than itself");
    }

    /// The highlight gate compares the share of near-white pixels of the fitted tone with the plain render's and the
    /// camera preview's, in one direction only: clipping what the camera clips is not refused.
    #[test]
    fn dng_highlight_gate_compares_near_white_shares() {
        let mut sensor = Rgb32f::new(32, 32);
        for (i, p) in sensor.data.iter_mut().enumerate() {
            *p = [0.02 + 1.5 * i as f32 / 1024.0; 3];
        }
        let (bright, dark) = (ToneMap::new(0.0, 1.0, 0.0), ToneMap::new(0.0, -1.0, 0.0));
        let mut white = Rgb32f::new(32, 32);
        white.data.iter_mut().for_each(|p| *p = [1.0; 3]);
        let mut black = Rgb32f::new(32, 32);
        black.data.iter_mut().for_each(|p| *p = [0.1; 3]);
        assert!(clips_more(&sensor, &black, &bright, &dark), "clips more than the plain render and a preview without white");
        assert!(!clips_more(&sensor, &black, &dark, &bright));
        assert!(!clips_more(&sensor, &black, &bright, &bright));
        assert!(!clips_more(&sensor, &white, &bright, &dark), "the camera preview clips as much");
    }

    /// A dark scene's shadows below the usual training floor still reach the tone-only fit.
    #[test]
    fn dng_tone_fit_trains_on_dark_pixels() {
        let mut sensor = Rgb32f::new(64, 64);
        let mut reference = Rgb32f::new(64, 64);
        for (i, (s, r)) in sensor.data.iter_mut().zip(reference.data.iter_mut()).enumerate() {
            let v = 0.0005 + 0.004 * (i % 64) as f32 / 64.0 + if i % 7 == 0 { 0.2 } else { 0.0 };
            *s = [v, v * 1.1, v * 0.9];
            *r = [(v * 3.0).min(0.9), (v * 3.3).min(0.9), (v * 2.7).min(0.9)];
        }
        let usual = collect_pairs(&sensor, &reference, 0.0, None, false).map_or(0, |p| p.0.len());
        let dark = collect_pairs(&sensor, &reference, 0.0, None, true).map_or(0, |p| p.0.len());
        assert!(dark > usual, "{dark} vs {usual}");
    }

    /// The embedded vignetting correction reaches the sensor proxy as the pipeline applies it to the render.
    #[test]
    fn dng_sensor_proxy_gets_the_embedded_vignetting_correction() {
        use lightcraft_develop::{EmbeddedLens, EmbeddedVignette};
        let lens = EmbeddedLens {
            vignette: Some(EmbeddedVignette { k: [1.0, 0.0, 0.0, 0.0, 0.0], center: lightcraft_geom::Point::new(0.5, 0.5), radius: 0.6 }),
            ..Default::default()
        };
        let mut proxy = Rgb32f::new(96, 64);
        proxy.data.fill([0.2; 3]);
        correct_vignetting(&mut proxy, Some(&lens), lightcraft_geom::Orientation::Normal);
        let (centre, corner) = (proxy.data[32 * 96 + 48][1] / 0.2, proxy.data[0][1] / 0.2);
        assert!((centre - 1.0).abs() < 0.01 && (1.9..2.0).contains(&corner), "centre {centre}, corner {corner}");
        let mut plain = Rgb32f::new(96, 64);
        plain.data.fill([0.2; 3]);
        correct_vignetting(&mut plain, None, lightcraft_geom::Orientation::Normal);
        assert!(plain.data.iter().all(|p| *p == [0.2; 3]));
    }

    /// Lens opcodes prevent mosaic binning. The DNG fit still gets a proxy, with the file's
    /// full-resolution corrections applied once rather than silently losing the tone fit.
    #[test]
    fn dng_sensor_proxy_applies_lens_opcodes_before_downsampling() {
        use lightcraft_raw::Opcode;
        let bytes = crate::tests_xmp::synthetic_dng_with(None, Default::default());
        let mut raw = lightcraft_raw::decode(&bytes).unwrap();
        raw.data = lightcraft_raw::RawData::U16(vec![4000; raw.width * raw.height]);
        raw.opcodes.list3 = vec![Opcode::FixVignetteRadial { k: [1.0, 0.0, 0.0, 0.0, 0.0], center: [0.5, 0.5] }];
        assert!(raw.develop_binned(2, SENSOR_CLIP).unwrap().is_none());
        let proxy = sensor_proxy(&raw, 2, 32).expect("a corrected CFA proxy even when binning is unavailable");
        assert_eq!((proxy.width, proxy.height), (32, 24));
        let center = proxy.data[12 * 32 + 16][1];
        let corner = proxy.data[0][1];
        assert!((1.8..2.1).contains(&(corner / center)), "one vignetting gain, not zero or two: {corner} / {center}");
        let full = raw.develop(lightcraft_raw::Method::Bilinear).unwrap();
        assert_eq!(proxy.data, full.data, "the same opcode interpretation as raw development");
        let small = sensor_proxy(&raw, 2, 16).unwrap();
        assert_eq!(small.data, fit(&full, 16, 16, Filter::Box).data, "correct first, then reduce");
    }

    /// Which DNGs get a tone fitted to their camera JPEG: a colour matrix, a tone curve that is absent or the
    /// identity, with or without a look table; and no other raw format.
    #[test]
    fn dng_tone_gates_follow_the_files_own_tags() {
        use lightcraft_raw::profile::{HsvTable, ToneCurve};
        let own = Mat3([[0.6, -0.1, -0.05], [-0.4, 1.3, 0.1], [-0.1, 0.2, 0.7]]);
        let color = lightcraft_raw::ColorData { illuminant: [21, 0], color_matrix: [Some(own), None], ..Default::default() };
        let eligible = |format: RawFormat, color: lightcraft_raw::ColorData| {
            let raw = raw_of(format, "Make", "Model", color);
            let t = lightcraft_raw::color::camera_transform(&raw, lightcraft_raw::color::as_shot_white_xy(&raw));
            dng_tone_eligible(&raw, &t)
        };
        assert!(eligible(RawFormat::Dng, color.clone()));
        assert!(!eligible(RawFormat::Cr2, color.clone()), "other formats keep their own path");
        assert!(!eligible(RawFormat::Dng, Default::default()), "no matrix, no colour to keep");
        let mut identity = color.clone();
        identity.profile.tone_curve = ToneCurve::from_tag(&[0.0, 0.0, 0.5, 0.5, 1.0, 1.0]);
        assert!(identity.profile.tone_curve.is_some() && eligible(RawFormat::Dng, identity), "an identity ProfileToneCurve is no tone");
        let mut curved = color.clone();
        curved.profile.tone_curve = ToneCurve::from_tag(&[0.0, 0.0, 0.18, 0.4, 1.0, 1.0]);
        assert!(curved.profile.tone_curve.is_some() && !eligible(RawFormat::Dng, curved), "the file's own tone curve is kept");
        let mut looked = color;
        looked.profile.look_table = HsvTable::from_tags(&[1, 2, 1], &[0.0, 1.0, 1.0, 0.0, 1.0, 1.0], 0);
        assert!(looked.profile.look_table.is_some() && eligible(RawFormat::Dng, looked), "a look table defines colour, not tone");
    }

    /// The 1024 px preview Lightroom Classic writes into a 7008 px DNG is fitted; a 256 px thumbnail is not.
    #[test]
    fn dng_tone_fit_takes_lightroom_sized_previews() {
        let floor = |raw_long: f64| (MIN_PREVIEW_FRACTION * raw_long).max(384.0);
        assert!(1024.0 >= floor(7008.0));
        assert!(256.0 < floor(7008.0));
        assert!(1024.0 >= floor(9504.0), "a 61 MP raw too");
    }

    /// Pairs whose scene luminance spans `ratio` from the darkest to the brightest pixel, mapped
    /// to the JPEG through a steep camera curve (a hazy scene the camera stretches).
    fn low_contrast_pairs(ratio: f64) -> Vec<(f64, f64)> {
        (0..4096).map(|i| 0.08 * ratio.powf(i as f64 / 4095.0)).map(|x| (x, (0.2 + (x - 0.08) * 3.0).min(0.95))).collect()
    }

    /// The ratio the tone fits test: scene luminance at the middle of the brightest 1/32 over that
    /// of the darkest 1/32 (their last and first knots).
    fn knot_range(mut scene: Vec<f64>) -> f64 {
        scene.sort_by(f64::total_cmp);
        let n = scene.len();
        scene[31 * n / 32 + n / 64] / scene[n / 64]
    }

    /// Issue #631: a low-contrast scene (its darker and brighter parts under 1.5× apart) still gets
    /// a tone curve; only a nearly uniform one doesn't. Both tone fits use the same limit.
    #[test]
    fn tone_fits_accept_low_contrast_scenes() {
        for fit in [fit_tone, fit_tone_conditional] {
            let pairs = low_contrast_pairs(1.47);
            let range = knot_range(pairs.iter().map(|p| p.0).collect());
            assert!((1.4..1.5).contains(&range), "the scene sits just below the old limit of 1.5: {range}");
            let curve = fit(pairs).expect("a low-contrast scene, as in the burst of issue #631");
            assert!((curve.apply(0.1) - 0.26).abs() < 0.01, "follows the camera curve: {}", curve.apply(0.1));
            assert!(fit(low_contrast_pairs(1.1)).is_none(), "a nearly uniform frame");
        }
    }

    /// The same scene end to end: colour varies, luminance spans under 1.5×, and the camera
    /// stretches it; the look is accepted and lands on the JPEG instead of the neutral fallback.
    #[test]
    fn a_low_contrast_scene_gets_a_camera_look() {
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let mut sensor = Rgb32f::new(96, 64);
        let mut reference = sensor.clone();
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            let ev = 0.06 + (i % 31) as f32 * 0.0008;
            *src = [ev * (0.8 + (i % 11) as f32 * 0.025), ev, ev * (0.8 + (i % 17) as f32 * 0.014)];
            let p = known.apply_f32(*src);
            let y = luminance_2020(p);
            *dst = p.map(|v| v * (0.2 + (y - 0.07) * 3.0) / y);
        }
        let look = fit_pairs(&sensor, &reference).expect("a camera look, not the neutral fallback");
        let range = knot_range(sensor.data.iter().map(|x| luma(look.matrix.apply(x.map(f64::from)))).collect());
        assert!(range < 1.5, "the scene sits below the old limit of 1.5: {range}");
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
        let error: f64 = sensor
            .data
            .iter()
            .zip(&reference.data)
            .map(|(x, y)| {
                let p = displayed(look.matrix.apply(x.map(f64::from)), &tone);
                (0..3).map(|c| (p[c] - f64::from(y[c])).powi(2)).sum::<f64>() / 3.0
            })
            .sum::<f64>()
            / sensor.data.len() as f64;
        assert!(error.sqrt() < 0.02, "{}", error.sqrt());
    }

    /// Codex review of #631: a low-contrast scene's curve is fitted to its midtones only, and the
    /// camera JPEG's bright areas (luminance 0.85 and up) are in neither the fit nor the held-out
    /// gates. With 1.2 alone, this frame's curve passed the gates and rendered its bright sky
    /// (camera JPEG 0.93) at about 0.28. The second review found the same through the retries'
    /// filters: a clipped sky (left out by the refit without clipped pixels), scattered bright
    /// pixels (left out away from edges) and sensor values over 1.5 (out of the pairs' range).
    #[test]
    fn a_narrow_curve_that_darkens_the_bright_areas_is_refused() {
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        for (case, sky_value, is_sky) in [
            ("bright sky", 0.9, &(|i: usize| i < 5600) as &dyn Fn(usize) -> bool),
            ("clipped sky", 1.0, &|i: usize| i < 5600),
            ("scattered bright pixels", 0.9, &|i: usize| i.is_multiple_of(8)),
            ("sky over the pairs' range", 1.6, &|i: usize| i < 5600),
        ] {
            let mut sensor = Rgb32f::new(96, 64);
            let mut reference = sensor.clone();
            let mut clipped = vec![false; sensor.data.len()];
            for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
                if is_sky(i) {
                    // bright, nearly white in the camera JPEG
                    *src = [sky_value; 3];
                    *dst = [0.93; 3];
                    clipped[i] = case == "clipped sky";
                    continue;
                }
                let ev = 0.06 + (i % 31) as f32 * 0.0008;
                *src = [ev * (0.8 + (i % 11) as f32 * 0.025), ev, ev * (0.8 + (i % 17) as f32 * 0.014)];
                let p = known.apply_f32(*src);
                let y = luminance_2020(p);
                // nearly flat midtones: the fitted curve's extrapolation can't reach the sky
                *dst = p.map(|v| v * (0.2 + (y - 0.07) * 0.05) / y);
            }
            let Some(look) = fit_look(&sensor, &reference, &clipped, None) else { continue };
            let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
            let correction = look.hue_sat.as_ref().and_then(HueSat::new);
            let p = look.matrix.apply([f64::from(sky_value); 3]);
            let p = correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from));
            let sky = luma(displayed(p, &tone));
            assert!(sky > 0.8, "{case}: an accepted look must not darken what the camera renders at 0.93: {sky}");
        }
    }

    /// Third Codex review of #631: for a Sony photo shot with DRO, the fitted curve is lowered to
    /// Sony's without DRO after the fit, so it must be checked as lowered. On this frame the
    /// fitted curve keeps the sky (0.81 against the fallback's 0.79), and lowered it rendered the
    /// sky at 0.61 where the camera JPEG has 0.93.
    #[test]
    fn a_narrow_curve_is_checked_as_lowered_for_dro() {
        let known = Mat3([[1.8, -0.4, -0.1], [-0.2, 1.5, -0.1], [-0.05, -0.3, 1.7]]);
        let mut sensor = Rgb32f::new(96, 64);
        let mut reference = sensor.clone();
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            if i < 5600 {
                *src = [0.9; 3];
                *dst = [0.93; 3];
                continue;
            }
            let ev = 0.06 + (i % 31) as f32 * 0.0008;
            *src = [ev * (0.8 + (i % 11) as f32 * 0.025), ev, ev * (0.8 + (i % 17) as f32 * 0.014)];
            let p = known.apply_f32(*src);
            let y = luminance_2020(p);
            *dst = p.map(|v| v * (0.6 + (y - 0.07) * 0.25) / y);
        }
        let neutral = luma(displayed([0.9; 3], &ToneMap::new(0.0, 0.0, 0.0)));
        if let Some(look) = fit_look_with(&sensor, &reference, &vec![false; sensor.data.len()], None, true) {
            let look = with_dro_off_tone(look);
            let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
            let correction = look.hue_sat.as_ref().and_then(HueSat::new);
            let p = look.matrix.apply([0.9; 3]);
            let p = correction.as_ref().map_or(p, |c| c.apply(p.map(|v| v as f32)).map(f64::from));
            let sky = luma(displayed(p, &tone));
            assert!(sky > neutral - 0.02, "the DRO-lowered curve must not darken the sky past the fallback ({neutral}): {sky}");
        }
    }

    /// A black-and-white JPEG of a low-contrast scene (issues #577 and #631): grey pixels, scene luminance spanning
    /// under 1.5×, which the old range limit refused before any gate. Its tone curve is fitted.
    #[test]
    fn a_low_contrast_black_and_white_jpeg_gives_its_tone_curve() {
        let mut sensor = Rgb32f::new(96, 64);
        let mut reference = sensor.clone();
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            let ev = 0.06 + (i % 31) as f32 * 0.0008;
            *src = [ev * (0.8 + (i % 11) as f32 * 0.025), ev, ev * (0.8 + (i % 17) as f32 * 0.014)];
            *dst = [0.2 + (luminance_2020(*src) - 0.06) * 3.0; 3];
        }
        let range = knot_range(sensor.data.iter().map(|x| luma(x.map(f64::from))).collect());
        assert!(range < 1.5, "the scene sits below the old limit of 1.5: {range}");
        let look = fit_tone_only(&sensor, &reference, None).expect("the JPEG's tone curve");
        let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
        let error = sensor.data.iter().zip(&reference.data).map(|(x, y)| (luma(displayed(x.map(f64::from), &tone)) - f64::from(y[0])).powi(2));
        let rms = (error.sum::<f64>() / sensor.data.len() as f64).sqrt();
        assert!(rms < 0.02, "{rms}");
    }

    /// [`a_narrow_curve_that_darkens_the_bright_areas_is_refused`] for a black-and-white JPEG: its tone curve is
    /// fitted to nearly flat midtones under 1.5×, and the bright sky (camera JPEG 0.93) is left out of the fit.
    #[test]
    fn a_narrow_black_and_white_curve_that_darkens_the_bright_areas_is_refused() {
        let mut sensor = Rgb32f::new(96, 64);
        let mut reference = sensor.clone();
        for (i, (src, dst)) in sensor.data.iter_mut().zip(&mut reference.data).enumerate() {
            if i < 5600 {
                *src = [0.9; 3];
                *dst = [0.93; 3];
                continue;
            }
            let ev = 0.06 + (i % 31) as f32 * 0.0008;
            *src = [ev * (0.8 + (i % 11) as f32 * 0.025), ev, ev * (0.8 + (i % 17) as f32 * 0.014)];
            *dst = [0.2 + (luminance_2020(*src) - 0.06) * 0.05; 3];
        }
        if let Some(look) = fit_tone_only(&sensor, &reference, None) {
            let sky = luma(displayed(look.matrix.apply([0.9; 3]), &ToneMap::camera(&look.tone, 0.0, 0.0, 0.0)));
            assert!(sky > 0.8, "an accepted curve must not darken what the camera renders at 0.93: {sky}");
        }
    }
}
