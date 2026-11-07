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
    color::{CameraTransform, camera_transform},
    profile::{HsvTable, ProfileLook, ToneCurve},
};

mod colour;
mod hue_sat;
mod look_fit;
mod mono;
mod pairs;
mod proxy;
mod tone;

#[cfg(test)]
use colour::PROXY;
#[cfg(test)]
pub(crate) use colour::{PROFILE_PROXY, dng_colour, dng_tone_eligible};
pub(crate) use colour::{file_local_look, starting_colour_tags, with_starting_colour};
#[cfg(test)]
pub(super) fn starting_colour_with(
    raw: &mut RawImage,
    fit: impl FnOnce(&RawImage, &CameraTransform) -> (Option<CameraLook>, bool),
    monochrome: impl FnOnce(&RawImage, &CameraTransform) -> Option<CameraLook>,
    dark: impl FnOnce(&RawImage, &CameraTransform) -> Option<CameraLook>,
) -> (Xy, CameraTransform, Option<CameraLook>) {
    let (xy, t, look) = colour::starting_colour_with(raw, fit, monochrome, dark);
    fitted_white(raw, xy, t, look)
}

/// [`colour::starting_colour`] recomputed at the white the fitted camera-to-working matrix
/// (`look.matrix · t.matrix`) gives for the as-shot multipliers, so an accepted JPEG fit
/// returns them with the fitted matrix rather than the fallback model they were fitted on.
/// A tone-only look carries the identity matrix and keeps its colour on the transform, so
/// its white is the transform's white already; the hue/saturation table and tone curve stay
/// with the look.
pub(crate) fn starting_colour(
    raw: &mut RawImage,
    bytes: &[u8],
    lens: Option<&LensGeometry>,
    embedded: Option<&lightcraft_develop::EmbeddedLens>,
) -> (Xy, CameraTransform, Option<CameraLook>) {
    let (xy, t, look) = colour::starting_colour(raw, bytes, lens, embedded);
    fitted_white(raw, xy, t, look)
}

/// Recompute the white and transform for the fitted camera-to-working matrix
/// (`look.matrix · t.matrix`) when the look supplies colour. The fitted matrix is now the
/// camera colour model, so the white the same multipliers give under it is the as-shot white
/// the transform was fitted at; rebuild the transform at that white.
fn fitted_white(raw: &RawImage, xy: Xy, mut t: CameraTransform, look: Option<CameraLook>) -> (Xy, CameraTransform, Option<CameraLook>) {
    let Some(look) = look else { return (xy, t, None) };
    if look.matrix == Mat3::IDENTITY {
        return (xy, t, Some(look));
    }
    let fitted = look.matrix.mul(&t.matrix);
    let white = fitted.apply([1.0; 3]);
    let xy = if white.iter().all(|&v| v.is_finite() && v > 1e-9) {
        let xyz = bradford(D65, D50).apply(REC2020.to_xyz().apply(white));
        let next = Xy::from_xyz(xyz);
        if next.x.is_finite() && next.y.is_finite() && next.y > 0.0 { next } else { xy }
    } else {
        xy
    };
    let rebuilt = camera_transform(raw, xy);
    t.matrix = look.matrix.mul(&rebuilt.matrix);
    t.wb = rebuilt.wb;
    t.white_xy = xy;
    (xy, t, Some(look))
}

pub(crate) use hue_sat::HueSat;
#[cfg(test)]
use hue_sat::{MIN_LUMINANCE_KEPT, NO_LUMINANCE_KEPT, fit_hue_sat, hue_saturation, restore_weight};
#[cfg(test)]
pub(crate) use look_fit::fit_preview;
#[cfg(test)]
use look_fit::{
    FIT_PREVIEW_MAX_PIXELS, SENSOR_CLIP, at_most, clips_more, correct_vignetting, fit_look, fit_look_with, sony_dro_off_tone, with_dro_off_tone,
    without_clipped,
};
#[cfg(test)]
use mono::{fit_dark_tone, fit_tone_only, monochrome, monochrome_look, to_prophoto};
#[cfg(test)]
use pairs::{
    Attempt, Pairs, TONE_FITS, collect_pairs, colour_pair, displayed, fit_attempt, fit_pairs, fit_pairs_ordered, fit_pairs_with, fit_partial,
    fit_tone_with_shadows, luma, rank_correlation_ties, search_ordered,
};
#[cfg(test)]
pub(crate) use proxy::LensGeometry;
#[cfg(test)]
use proxy::{
    Cr3Framing, MIN_PREVIEW_FRACTION, alignment, estimate_cr3_framing, into_lens_geometry, proxies, reference_at, sensor_proxy, structure_agrees,
};
pub(crate) use proxy::{fit_profile, lens_geometry, profile_pairs};
#[cfg(test)]
use tone::{HighlightCheck, ToneFit, fit_tone, fit_tone_conditional, highlights};

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
/// 13: tone curves no steeper than a log-log slope of 4 between knots (#1904).
/// 13: the Canon CR3/CR2 camera JPEG read in the colour space of the Canon maker note (#2654).
/// 13: the sensor proxy of every per-file fit gets the embedded vignetting correction the render applies (Fujifilm
/// RAF; other makes with a lens database), so the render no longer applies it twice, at the position a lens warp samples;
/// the warped candidate's alignment leaves out the pixels outside the sensor instead of reading them as black.
pub const LOOK_VERSION: u32 = 13;

#[derive(Clone, Debug)]
pub(crate) struct CameraLook {
    pub matrix: Mat3,
    pub tone: CameraTone,
    /// Hue/saturation correction after `matrix`, in linear ProPhoto RGB (DNG `ProfileHueSatMap`).
    pub hue_sat: Option<HsvTable>,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fitted_matrix_test {
use super::*;

/// An accepted JPEG fit supplies the camera's real colour matrix (for a CR2, fitted from the
/// embedded JPEG by `fit_preview`): the as-shot white and the camera transform returned alongside
/// the look must be computed with that matrix, not with the fallback sRGB model they were
/// computed with before the fit.
#[test]
fn starting_colour_uses_fitted_matrix() {
    let area = lightcraft_raw::Rect::new(0, 0, 2, 2);
    let mut raw = RawImage {
        format: RawFormat::Cr2,
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
        color: ColorData::default(),
        wb_multipliers: Some([2.0, 1.0, 1.5]),
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: Default::default(),
        metadata: lightcraft_raw::Metadata { make: Some("Canon".to_owned()), model: Some("Canon EOS 5D Mark IV".to_owned()), ..Default::default() },
        lens_metadata: Default::default(),
    };
    let fallback_xy = lightcraft_raw::color::as_shot_white_xy(&raw);
    let fallback_t = lightcraft_raw::color::camera_transform(&raw, fallback_xy);
    assert!(fallback_t.matrix_is_fallback, "a CR2 without matrices starts on the fallback model");
    let fitted = CameraLook {
        matrix: Mat3([[1.6, -0.4, -0.2], [-0.2, 1.4, -0.2], [0.0, -0.3, 1.3]]),
        tone: CameraTone::new(std::array::from_fn(|i| {
            let x = 0.006 * 1.13f32.powi(i as i32);
            [x, x]
        }))
        .unwrap(),
        hue_sat: None,
    };
    let fitted_clone = fitted.clone();
    let (xy, t, look) = starting_colour_with(
        &mut raw,
        |_, t| {
            assert!(t.matrix_is_fallback, "the fit sees the raw on the fallback model");
            (Some(fitted_clone), false)
        },
        |_, _| panic!("no black-and-white JPEG fit after an accepted fit"),
        |_, _| panic!("no dark tone fit after an accepted fit"),
    );
    assert_eq!(look.map(|look| look.matrix), Some(fitted.matrix), "the fitted look is kept");
    assert_ne!(t.matrix, fallback_t.matrix, "the transform must use the fitted matrix, not the fallback one");
    assert!(
        (xy.x - fallback_xy.x).abs() + (xy.y - fallback_xy.y).abs() > 0.005,
        "the as-shot white must be recomputed with the fitted matrix: {xy:?} vs the fallback's {fallback_xy:?}"
    );
    assert_eq!(t.white_xy, xy, "the transform is at the white it was computed for");
}
}
