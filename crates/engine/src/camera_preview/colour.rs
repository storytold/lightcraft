use super::look_fit::{fit_dng_tone, fit_preview};
use super::mono::{fit_dark_tone, monochrome_look};
use super::proxy::{LensGeometry, proxies};
use super::*;

/// Long edge of the sensor/JPEG proxy a single photo's look is fitted on (the acceptance gates
/// below were set at this size).
pub(super) const PROXY: usize = 96;

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
pub(super) fn starting_colour_with(
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
