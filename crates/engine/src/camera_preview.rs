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

mod colour;
mod hue_sat;
mod look_fit;
mod mono;
mod pairs;
mod proxy;
mod tone;

#[cfg(test)]
pub(crate) use colour::{PROFILE_PROXY, dng_colour, dng_tone_eligible};
#[cfg(test)]
use colour::{PROXY, starting_colour_with};
pub(crate) use colour::{file_local_look, starting_colour, starting_colour_tags, with_starting_colour};
pub(crate) use hue_sat::HueSat;
#[cfg(test)]
use hue_sat::{MIN_LUMINANCE_KEPT, NO_LUMINANCE_KEPT, fit_hue_sat, hue_saturation, restore_weight};
#[cfg(test)]
pub(crate) use look_fit::fit_preview;
#[cfg(test)]
use look_fit::{
    DngColourProxy, FIT_PREVIEW_MAX_PIXELS, SENSOR_CLIP, at_most, band_luminance, clips_more, correct_vignetting, display_error, dng_colour_proxy,
    fit_bands, fit_chroma_offsets, fit_hue_chroma, fit_look, fit_look_with, near_white, sony_dro_off_tone, spatial_pair_sets,
    validate_dng_colour_terms, with_chroma_stage, with_dro_off_tone, with_hue_term, without_clipped,
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
    CLIP_TOLERANCE, Cr3Framing, MIN_PREVIEW_FRACTION, alignment, estimate_cr3_framing, into_lens_geometry, proxies, reference_at, sensor_proxy,
    structure_agrees,
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
/// 14: DNG fits add validated hue/saturation, luminance-band, hue-chroma and offset colour terms.
pub const LOOK_VERSION: u32 = 14;

#[derive(Clone, Debug)]
pub(crate) struct CameraLook {
    pub matrix: Mat3,
    pub tone: CameraTone,
    /// Hue/saturation correction after `matrix`, in linear ProPhoto RGB (DNG `ProfileHueSatMap`).
    pub hue_sat: Option<HsvTable>,
    /// Additional luminance-band and hue chroma rendering, after the hue/saturation table.
    pub bands: Option<ChromaBands>,
}

/// Number of luminance nodes of [`ChromaBands`].
pub(crate) const BAND_NODES: usize = 7;
/// Number of hue nodes of [`ChromaBands::hue`].
pub(crate) const HUE_NODES: usize = 24;

/// A 2×2 matrix per luminance node acting on the luminance-normalised chroma `(r/y - 1, b/y - 1)` of a scene colour
/// (so the matrix keeps neutrals and luminance; the tone curve's chroma scale commutes with it), linearly
/// interpolated in log luminance between the nodes and constant beyond them: the camera's colour rendering that
/// differs with lightness (shadows turned bluer, highlights bleached) rather than with hue. Two optional terms follow
/// the matrix: a gain and rotation of that chroma per hue ([`Self::hue`]) and an offset of it per luminance node
/// ([`Self::offsets`], the lightness-dependent cast the camera adds).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChromaBands {
    /// Scene luminance of each node, increasing.
    pub luminance: [f32; BAND_NODES],
    /// `[m00, m01, m10, m11]` of each node: `(r', b') = M (r, b)`.
    pub matrices: [[f32; 4]; BAND_NODES],
    /// Per hue node (the angle of the chroma, evenly spaced round the circle): `[a, b]`, the chroma `c` becoming
    /// `(a + ib) c` (a gain and a rotation), linearly interpolated round the circle.
    pub hue: Option<[[f32; 2]; HUE_NODES]>,
    /// `(Δr, Δb)` added to the normalised chroma at each luminance node (after the matrix and the hue term).
    pub offsets: Option<[[f32; 2]; BAND_NODES]>,
}

impl ChromaBands {
    /// The identity term at the nodes `luminance`.
    pub fn identity(luminance: [f32; BAND_NODES]) -> ChromaBands {
        ChromaBands { luminance, matrices: [[1.0, 0.0, 0.0, 1.0]; BAND_NODES], hue: None, offsets: None }
    }

    /// The node below luminance `y` and the position between it and the next (in log luminance).
    fn locate(&self, y: f32) -> (usize, f32) {
        let first = self.luminance[0];
        let last = self.luminance[BAND_NODES - 1];
        if y <= first {
            return (0, 0.0);
        }
        if y >= last {
            return (BAND_NODES - 2, 1.0);
        }
        let j = self.luminance.iter().position(|l| *l > y).unwrap_or(BAND_NODES - 1).max(1);
        let Some((&a, &b)) = self.luminance.get(j - 1).zip(self.luminance.get(j)) else { return (0, 0.0) };
        (j - 1, ((y / a).ln() / (b / a).ln()).clamp(0.0, 1.0))
    }

    /// The matrix at scene luminance `y`.
    #[cfg(test)]
    fn at(&self, y: f32) -> [f32; 4] {
        let (j, t) = self.locate(y);
        let Some((lo, hi)) = self.matrices.get(j).zip(self.matrices.get(j + 1)) else { return [1.0, 0.0, 0.0, 1.0] };
        std::array::from_fn(|i| lo[i] * (1.0 - t) + hi[i] * t)
    }

    /// The `(a, b)` of the hue term at the chroma `(r, b)`.
    fn hue_at(table: &[[f32; 2]; HUE_NODES], r: f32, b: f32) -> [f32; 2] {
        let position = (b.atan2(r) / std::f32::consts::TAU).rem_euclid(1.0) * HUE_NODES as f32;
        let j = (position.floor() as usize).min(HUE_NODES - 1);
        let t = position - j as f32;
        let Some((p, q)) = table.get(j).zip(table.get((j + 1) % HUE_NODES)) else { return [1.0, 0.0] };
        [p[0] * (1.0 - t) + q[0] * t, p[1] * (1.0 - t) + q[1] * t]
    }

    /// Linear Rec.2020 in, linear Rec.2020 out, luminance kept.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        const W: [f32; 3] = [0.2627, 0.6780, 0.0593];
        let y = W[0] * rgb[0] + W[1] * rgb[1] + W[2] * rgb[2];
        if y <= 1e-6 || !y.is_finite() {
            return rgb;
        }
        let (r, b) = (rgb[0] / y - 1.0, rgb[2] / y - 1.0);
        let (j, t) = self.locate(y);
        let Some((lo, hi)) = self.matrices.get(j).zip(self.matrices.get(j + 1)) else { return rgb };
        let m: [f32; 4] = std::array::from_fn(|i| lo[i] * (1.0 - t) + hi[i] * t);
        let (mut r2, mut b2) = (m[0] * r + m[1] * b, m[2] * r + m[3] * b);
        if let Some(table) = &self.hue {
            let [a, c] = Self::hue_at(table, r2, b2);
            (r2, b2) = (a * r2 - c * b2, c * r2 + a * b2);
        }
        if let Some(offsets) = &self.offsets {
            let Some((a, b)) = offsets.get(j).zip(offsets.get(j + 1)) else { return rgb };
            r2 += a[0] * (1.0 - t) + b[0] * t;
            b2 += a[1] * (1.0 - t) + b[1] * t;
        }
        let g2 = -(W[0] * r2 + W[2] * b2) / W[1];
        let out = [y * (1.0 + r2), y * (1.0 + g2), y * (1.0 + b2)];
        if out.iter().all(|v| v.is_finite()) { out } else { rgb }
    }
}

#[cfg(test)]
mod tests;
