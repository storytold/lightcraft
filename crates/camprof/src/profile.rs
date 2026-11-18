// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/profile.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Profile fitting: from a measurement target to a complete camera profile.
//!
//! # What a profile is
//!
//! The fit produces the three things both DCP and ICC profiles are built from,
//! with the conventions those formats use:
//!
//! * `forward_matrix` — **white-balanced** camera RGB to XYZ relative to D50.
//!   "White balanced" means divided by [`Profile::white_balance`], the camera's
//!   signal for a neutral surface, so a neutral reads `(1, 1, 1)`; the matrix is
//!   constrained so that exactly that maps to D50: `forward_matrix * (1,1,1) =
//!   D50`. This is the DNG `ForwardMatrix`, and the matrix an ICC profile for a
//!   white-balancing raw converter such as Capture One is built from.
//! * `color_matrix` — XYZ under the calibration illuminant to **camera-native**
//!   RGB: the DNG `ColorMatrix`. It is derived from the forward matrix rather
//!   than fitted separately, `diag(white_balance) * forward^-1 * CAT(illum->D50)`,
//!   so the two DNG rendering paths agree exactly at the calibration white and a
//!   raw converter infers the calibration illuminant's colour temperature from a
//!   neutral. It is normalised the way the DNG SDK normalises it, so that D50
//!   maps to a camera signal whose largest channel is 1.
//! * `lut` — an optional smooth correction applied after the forward matrix; see
//!   [`ChromaTable`].
//!
//! # The fit
//!
//! 1. The reference colours are brought to the profile connection space (XYZ
//!    relative to D50) with Bradford, as both DNG and ICC do.
//! 2. The camera neutral is the mean chromaticity of the target's mid greys —
//!    the patches a photographer white-balances on. The white patch is not used:
//!    on a ColorChecker Passport it reads measurably yellower than the greys,
//!    and a profile white-balanced on it renders every grey slightly blue.
//! 3. Exposure is fixed by the same greys, so colours are judged at the
//!    lightness a photographer would set.
//! 4. The forward matrix minimises the sum of squared CIEDE2000 differences
//!    with the white constraint built into its parametrisation (six free
//!    entries, the third column follows), starting from the weighted
//!    least-squares solution.
//! 5. The correction table is fitted to what the matrix leaves.

use crate::color::{Cat, ColorError, bradford_map_white, ciede2000, xyz_to_lab, xyz_to_uvy};
use crate::illuminants;
use crate::linalg::{Mat3, Vec3};
use crate::lut::ChromaTable;
use crate::observer::Observer;
use crate::spectrum::Spectrum;
use crate::target::{self, PatchSet, TargetError, WeightOverrides};
use core::fmt;

/// Errors from profile fitting.
#[derive(Clone, Debug, PartialEq)]
pub enum ProfileError {
    /// The target could not be interpreted.
    Target(TargetError),
    /// The requested configuration is contradictory.
    Config(String),
    /// A colour conversion failed.
    Color(ColorError),
    /// The fit could not converge on a usable solution.
    Fit(String),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Target(e) => write!(f, "{e}"),
            Self::Config(m) => write!(f, "{m}"),
            Self::Color(e) => write!(f, "{e}"),
            Self::Fit(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ProfileError {}

impl From<TargetError> for ProfileError {
    fn from(e: TargetError) -> Self {
        Self::Target(e)
    }
}

impl From<ColorError> for ProfileError {
    fn from(e: ColorError) -> Self {
        Self::Color(e)
    }
}

/// Grid divisions a correction table is rendered into.
///
/// The table itself is a continuous function; these are the defaults for the
/// grids the containers store it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LutDivisions {
    /// Hue division count.
    pub hue: usize,
    /// Chroma division count.
    pub chroma: usize,
    /// Lightness division count; one produces a 2.5D table.
    pub lightness: usize,
}

impl Default for LutDivisions {
    fn default() -> Self {
        Self::DCP_DEFAULT
    }
}

impl LutDivisions {
    /// The divisions a DCP `HueSatMap` uses by default.
    pub const DCP_DEFAULT: Self = Self { hue: 90, chroma: 30, lightness: 1 };

    /// The divisions an ICC CLUT uses by default.
    pub const ICC_DEFAULT: Self = Self { hue: 48, chroma: 16, lightness: 16 };
}

/// How the correction table is fitted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LutFitOptions {
    /// Default grid the table is rendered into.
    pub divisions: LutDivisions,
    /// Regularisation of the spline; larger is smoother. See
    /// [`ChromaTable::DEFAULT_SMOOTHING`].
    pub smoothing: f64,
    /// How much of each colour's lightness the profile takes from the chart:
    /// [`ACCURATE`] (1) all of it, [`NATURAL`] (0) none, between in proportion.
    /// Hue and saturation always follow the chart.
    pub chart_lightness: f64,
}

/// The rendering that reproduces the chart: every patch's lightness, hue and
/// saturation as measured.
pub const ACCURATE: f64 = 1.0;

/// The rendering that keeps the lightness the camera itself sees — what its
/// matrix gives each colour — and takes only hue and saturation from the chart.
///
/// This is how matrix profiles, and the commercial profiles tested here,
/// render: on the three test charts X-Rite's, Capture One's and DCamProf's
/// profiles all put saturated colours 1.7 to 2.5 L* lighter than the chart's
/// reference on average, and its red 4 to 5.6 lighter, as the cameras'
/// matrices do. Reproduced exactly, the chart's reds and magentas read deeper
/// and richer than those profiles have taught the eye to expect.
pub const NATURAL: f64 = 0.0;

impl Default for LutFitOptions {
    fn default() -> Self {
        Self { divisions: LutDivisions::default(), smoothing: ChromaTable::DEFAULT_SMOOTHING, chart_lightness: ACCURATE }
    }
}

/// The forward matrix the chart alone asks for, fitted without the luminance
/// floor, and the correction table on top of it.
///
/// Where it is fitted — on the chart and between its patches — this matrix is
/// the better model: predicting each patch from the other 23, it misses by
/// less than the floored matrix does. Its weakness is only beyond the chart,
/// where a negative luminance weight takes saturated colours towards black.
/// The profile therefore renders through it wherever its luminance holds up and
/// through the floored model where it does not; see
/// [`Profile::render_balanced`].
#[derive(Clone, Debug)]
pub struct FreeModel {
    /// White-balanced camera RGB to XYZ relative to D50, white-locked like
    /// [`Profile::forward_matrix`] but with no floor on its luminance row.
    pub forward_matrix: Mat3,
    /// Correction table on top of it, with its luminance factor fading beyond
    /// the chart.
    pub lut: Option<ChromaTable>,
}

/// A colour whose luminance through the free model is at least this fraction
/// of its luminance through the floored model renders through the free model
/// alone.
pub const BLEND_TO: f64 = 1.0;

/// A colour whose luminance through the free model has fallen to this
/// fraction of its luminance through the floored model, or below, renders
/// through the floored model alone. Between the two the profile blends with a
/// smoothstep.
///
/// The two bounds were chosen by predicting each patch of three real charts
/// from the other 23: blending from 0.6 to 1.0 predicts better than either
/// model alone, and keeps the lightness of ever more saturated blues falling
/// steadily rather than dipping and recovering, which narrower or lower
/// bounds do.
pub const BLEND_FROM: f64 = 0.6;

/// A complete camera profile.
#[derive(Clone, Debug)]
pub struct Profile {
    /// Camera model, when the measurement named one.
    pub camera_name: Option<String>,
    /// EXIF light source the profile was calibrated under.
    pub illuminant_name: &'static str,
    /// The calibration illuminant spectrum.
    pub illuminant_spectrum: Option<Spectrum>,
    /// White point of the calibration illuminant, `Y = 1`.
    pub illuminant_white_point: Vec3,
    /// XYZ under the calibration illuminant to camera-native RGB: the DNG
    /// `ColorMatrix`. See the module documentation.
    pub color_matrix: Mat3,
    /// The matrix the correction table sits on; the forward matrix.
    pub lut_matrix: Mat3,
    /// White-balanced camera RGB to XYZ relative to D50, with
    /// `forward_matrix * (1, 1, 1) = D50`: the DNG `ForwardMatrix`.
    pub forward_matrix: Mat3,
    /// The camera-native signal of a neutral surface under the calibration
    /// illuminant, largest channel 1. Dividing a camera signal by it white
    /// balances that signal for the forward matrix.
    pub white_balance: Vec3,
    /// Exposure scale of the fitted target: `render` multiplies by it so that
    /// the target's grey patches come out at their reference luminance. It has
    /// no meaning outside the target and is not written into DCP or ICC files.
    pub exposure: f64,
    /// Correction table on top of the forward matrix, or `None` for a
    /// matrix-only profile.
    pub lut: Option<ChromaTable>,
    /// The chart's own free matrix and its table, when the luminance floor
    /// changed the forward matrix and the profile has a correction table.
    pub free: Option<FreeModel>,
    /// The `ForwardMatrix` a DCP with a `HueSatMap` is written with, when it
    /// differs from [`Profile::forward_matrix`].
    ///
    /// A DNG renderer converts to ProPhoto RGB and clips negative values before
    /// it applies the `HueSatMap`, so a camera colour the written matrix sends
    /// outside ProPhoto is damaged before the map can reach it; a chart fit
    /// sends saturated yellows, greens and blues there. This matrix keeps every
    /// camera primary inside ProPhoto and every channel's luminance share at
    /// the floor, so nothing is clipped, and the `HueSatMap` — measured from
    /// it — carries the profile's whole rendering ([`Profile::render_balanced`]).
    /// It is only the starting point of that map, never what the profile
    /// renders, so its own accuracy does not matter.
    pub dcp_forward_matrix: Option<Mat3>,
    /// Colour differences of the forward matrix alone, per patch.
    pub matrix_patch_errors: Vec<f64>,
    /// Colour differences after the correction table, per patch.
    pub lut_patch_errors: Vec<f64>,
    /// Reference colours of the fitted target, XYZ relative to D50.
    pub reference_xyz: Vec<Vec3>,
    /// Camera-native signals of the fitted target, in the same order.
    pub camera_rgb: Vec<Vec3>,
    /// Patch identifiers of the fitted target.
    pub patch_ids: Vec<String>,
    /// True when the correction table was built.
    pub has_lut: bool,
}

/// Options for a profile fit.
#[derive(Clone, Debug)]
pub struct FitOptions {
    /// Observer used for spectral conversions.
    pub observer_name: String,
    /// Illuminant the target was measured under. `None` uses the illuminant
    /// named in the measurement file.
    pub illuminant_name: Option<String>,
    /// Build the correction table.
    pub make_lut: bool,
    /// Correction table tuning.
    pub lut: LutFitOptions,
    /// Spectral sensitivities of the camera, when known. Recorded for
    /// diagnostics only.
    pub camera_ssf: Option<[Spectrum; 3]>,
    /// Per-class and per-patch weight multipliers. Every patch otherwise
    /// counts equally.
    pub weights: WeightOverrides,
    /// Run the matrix search for longer. The default search already restarts
    /// until it stops improving, so this only matters for very large targets.
    pub slow_search: bool,
    /// Kept for compatibility; the reference colours are no longer adapted to
    /// make one patch exactly neutral, because that bends every other patch
    /// towards that patch's tint.
    pub rebalance: bool,
    /// Discard the darkest patches that duplicate a lighter one's chroma.
    pub exclude_dark_duplicates: bool,
    /// Discard patches with almost no camera signal.
    pub exclude_weak: bool,
    /// Camera name to record in the profile.
    pub camera_name: Option<String>,
    /// Smallest share of luminance each white-balanced camera channel may have
    /// in the forward matrix, its `Y` row; `None` leaves the matrix free. See
    /// [`MIN_CHANNEL_LUMINANCE`].
    pub min_channel_luminance: Option<f64>,
    /// Give a profile with a correction table a separate DCP forward matrix
    /// whose camera primaries — its columns — lie inside ProPhoto RGB, as
    /// Adobe's own matrices do. See [`Profile::dcp_forward_matrix`].
    pub dcp_primaries_inside_prophoto: bool,
}

/// The default floor on each camera channel's share of luminance in the
/// forward matrix.
///
/// Fitted to a chart alone, the matrix of many cameras gives the blue channel
/// a negative share of luminance — the Sony A9 on a ColorChecker asks for
/// -0.42 — because on the chart's few blues that compensates for the green
/// channel's sensitivity to blue light. Beyond the chart the same weight makes
/// every colour bluer than the chart's blue darker and darker, until a blue
/// light or a deep blue flower renders black: the matrix sends it to negative
/// luminance, which no raw converter can show. Every camera channel does see
/// some luminance, so the fit keeps each share at this floor or above, and the
/// correction table puts the chart's own colours back where they belong. Adobe's
/// own matrices for these cameras give blue a share of 0.04 to 0.07.
pub const MIN_CHANNEL_LUMINANCE: f64 = 0.03;

impl Default for FitOptions {
    fn default() -> Self {
        Self {
            observer_name: "1931_2".into(),
            illuminant_name: None,
            make_lut: true,
            lut: LutFitOptions::default(),
            camera_ssf: None,
            weights: WeightOverrides::default(),
            slow_search: false,
            rebalance: false,
            exclude_dark_duplicates: false,
            exclude_weak: true,
            camera_name: None,
            min_channel_luminance: Some(MIN_CHANNEL_LUMINANCE),
            dcp_primaries_inside_prophoto: true,
        }
    }
}

/// A trace of what the fit did, for reporting.
#[derive(Clone, Debug, Default)]
pub struct FitReport {
    /// Patches read from the measurement.
    pub patches_read: usize,
    /// Patches removed because they duplicate a lighter patch's chroma.
    pub patches_dropped_dark: usize,
    /// Patches removed because their camera signal was too weak.
    pub patches_dropped_weak: usize,
    /// Patches left out because a camera channel was clipped.
    pub patches_clipped: usize,
    /// Patches used for the fit.
    pub patches_used: usize,
    /// Index of the brightest patch used as a neutral.
    pub neutral_index: usize,
    /// Identifiers of the patches the white balance was taken from.
    pub neutral_ids: Vec<String>,
    /// Identifier of the brightest of them.
    pub neutral_id: String,
    /// True when the lightest patch was among the neutrals.
    pub neutral_is_lightest: bool,
    /// Largest difference between a neutral patch's reference and the
    /// neutral axis, in CIEDE2000.
    pub neutral_de: f64,
    /// Root-mean-square CIEDE2000 of the colour matrix, which equals the
    /// forward matrix's at the calibration white.
    pub color_matrix_rms_de: f64,
    /// Root-mean-square CIEDE2000 of the forward matrix.
    pub forward_matrix_rms_de: f64,
    /// Mean CIEDE2000 of the forward matrix across the target.
    pub matrix_mean_de: f64,
    /// Mean CIEDE2000 after the correction table was applied.
    pub lut_mean_de: f64,
    /// Largest correction the table applies to a patch, in CIEDE2000.
    pub max_correction: f64,
    /// True when the correction table does nothing.
    pub lut_is_noop: bool,
    /// Mean CIEDE2000 of the free forward matrix alone, when the profile has
    /// one.
    pub free_matrix_mean_de: Option<f64>,
    /// Notes worth surfacing to the user.
    pub notes: Vec<String>,
}

/// A camera channel at or above this fraction of the white level is treated as
/// clipped.
const CLIP_LEVEL: f64 = 0.97;

/// Fit a profile.
///
/// Camera signals in `ps` are camera-native (not white balanced), in any
/// consistent units. Reference colours are XYZ under the fitting illuminant —
/// `opts.illuminant_name`, or the target's own illuminant.
pub fn fit(ps: &PatchSet, opts: &FitOptions, progress: &mut dyn FnMut(f64)) -> Result<(Profile, FitReport), ProfileError> {
    if ps.is_empty() {
        return Err(ProfileError::Target(TargetError::Missing { what: "patches".into() }));
    }
    let observer = Observer::by_name(&opts.observer_name)?;
    let illuminant_name = opts.illuminant_name.clone().unwrap_or_else(|| ps.illuminant_name.clone());
    let illuminant = illuminants::spectrum(&illuminant_name)?;
    let calib_wp = illuminants::white_point(&observer, &illuminant);
    let to_pcs = bradford_map_white(calib_wp, crate::color::D50);
    let d50 = crate::color::D50;

    let mut report = FitReport { patches_read: ps.len(), ..Default::default() };
    let mut work = ps.clone();
    work.illuminant_name = illuminant_name.clone();
    work.illuminant_white_point = calib_wp;
    if opts.exclude_weak {
        report.patches_dropped_weak = target::exclude_weak_signal_patches(&mut work);
    }
    if opts.exclude_dark_duplicates {
        report.patches_dropped_dark = target::exclude_darker_patches(&mut work, 0.01);
    }
    if work.len() < 6 {
        return Err(ProfileError::Fit(format!("{} usable patches is too few to fit a profile", work.len())));
    }
    progress(0.05);

    let n = work.len();
    let camera: Vec<Vec3> = work.patches.iter().map(|p| p.cam).collect();
    let reference: Vec<Vec3> = work.patches.iter().map(|p| to_pcs * p.xyz).collect();
    let ref_lab: Vec<Vec3> = reference.iter().map(|x| xyz_to_lab(*x, d50)).collect();

    // Weights: every patch counts the same unless the caller says otherwise.
    // A clipped patch is not a measurement of its colour, so it is left out.
    let camera_peak = camera.iter().map(|c| c.max()).fold(0.0f64, f64::max).max(1e-12);
    let mut weights = vec![1.0f64; n];
    for (i, p) in work.patches.iter().enumerate() {
        if let Some(cw) = opts.weights.class_weight.get(p.class) {
            weights[i] *= *cw;
        }
        if let Some(pw) = opts.weights.patch_weight.get(i) {
            weights[i] *= *pw;
        }
        // Camera signals from a frame are normalised to the white level; a
        // `.ti3` may be in other units, where only the relative peak is known.
        let normalised = camera_peak <= 1.0 + 1e-9;
        if normalised && camera[i].max() >= CLIP_LEVEL {
            weights[i] = 0.0;
            report.patches_clipped += 1;
            report.notes.push(format!("patch {} is clipped in the camera and was left out", p.id));
        }
        if !camera[i].is_finite() || !reference[i].is_finite() {
            weights[i] = 0.0;
        }
    }
    report.patches_used = weights.iter().filter(|w| **w > 0.0).count();

    // The neutral: mean chromaticity of the mid greys.
    let neutrals = neutral_patches(&ref_lab, &weights);
    let neutrals = if neutrals.is_empty() {
        let selection = target::find_neutral_patch(&work, calib_wp);
        report.notes.push(format!("the target has no mid-grey patches; the white balance comes from {} alone", work.patch_name(selection.index)));
        vec![selection.index]
    } else {
        neutrals
    };
    let neutral = camera_neutral(&camera, &neutrals).ok_or_else(|| ProfileError::Fit("the neutral patches carry no camera signal".into()))?;
    report.neutral_ids = neutrals.iter().map(|&i| work.patch_name(i)).collect();
    let brightest =
        neutrals.iter().copied().max_by(|a, b| reference[*a][1].partial_cmp(&reference[*b][1]).unwrap_or(core::cmp::Ordering::Equal)).unwrap_or(0);
    report.neutral_index = brightest;
    report.neutral_id = work.patch_name(brightest);
    let lightest = (0..n).max_by(|a, b| reference[*a][1].partial_cmp(&reference[*b][1]).unwrap_or(core::cmp::Ordering::Equal)).unwrap_or(0);
    report.neutral_is_lightest = neutrals.contains(&lightest);
    report.neutral_de = neutrals.iter().map(|&i| (ref_lab[i][1].powi(2) + ref_lab[i][2].powi(2)).sqrt()).fold(0.0f64, f64::max);
    if !report.neutral_is_lightest {
        // Report how far the lightest (white) patch sits from the greys' white
        // balance, because that is what a user who balances on it will see.
        let c = camera[lightest] / neutral;
        let spread = (c.max() - c.min()) / c.avg().max(1e-12);
        if spread > 0.02 {
            report.notes.push(format!(
                "white balance taken from {} grey patches; the white patch {} reads {:.1}% \
                 away from them, so balancing on it would tint the greys",
                neutrals.len(),
                work.patch_name(lightest),
                spread * 100.0
            ));
        }
    }

    let balanced: Vec<Vec3> = camera.iter().map(|c| *c / neutral).collect();
    progress(0.1);

    // The forward matrix.
    let floor = opts.min_channel_luminance;
    let fm = fit_forward_matrix(&balanced, &reference, &weights, &neutrals, floor, false, opts.slow_search, &mut |p| progress(0.1 + 0.6 * p))?;
    // The channels the floor holds, if any: then the chart alone asks for a
    // different matrix.
    let held: Vec<usize> = match floor {
        Some(f) => (0..3).filter(|&c| fm.row(1)[c] <= f + 1.0e-6).collect(),
        None => Vec::new(),
    };
    let exposure = neutral_exposure(&fm, &balanced, &reference, &neutrals);
    progress(0.75);

    // The colour matrix, derived so both DNG paths agree.
    let inverse = fm.inverted().ok_or_else(|| ProfileError::Fit("the forward matrix is singular".into()))?;
    let mut color_matrix = Mat3::from_diagonal(neutral) * inverse * to_pcs;
    let peak = (color_matrix * d50).max();
    if !(peak > 0.0) || !color_matrix.is_finite() {
        return Err(ProfileError::Fit("the colour matrix is degenerate".into()));
    }
    color_matrix = color_matrix.scaled(1.0 / peak);

    let predicted: Vec<Vec3> = balanced.iter().map(|c| fm * *c * exposure).collect();
    let matrix_errors: Vec<f64> = predicted.iter().zip(reference.iter()).map(|(p, r)| ciede2000(*r, *p, d50)).collect();
    report.matrix_mean_de = weighted_mean(&matrix_errors, &weights);
    report.forward_matrix_rms_de = weighted_rms(&matrix_errors, &weights);
    report.color_matrix_rms_de = report.forward_matrix_rms_de;

    let mut lut = None;
    let mut free = None;
    // The chart's lightness, or the camera's own, for the model the profile
    // renders the chart through: the free one when there is one, otherwise this
    // one. The floored model under a free one is only the fallback for colours
    // the free one would take towards black, and stays anchored to the chart.
    let share = opts.lut.chart_lightness.clamp(0.0, 1.0);
    let floored_targets = if held.is_empty() { lightness_targets(&predicted, &reference, share) } else { reference.clone() };
    if opts.make_lut {
        match ChromaTable::fit(&predicted, &floored_targets, &weights, opts.lut.smoothing) {
            Some(table) => {
                report.max_correction = table.max_correction;
                report.lut_is_noop = table.is_noop(1.0e-6);
                lut = Some(table);
            }
            None => report.notes.push("too few patches for a correction table; the profile is matrix only".into()),
        }
        if lut.is_some() && !held.is_empty() {
            let free_fm =
                fit_forward_matrix(&balanced, &reference, &weights, &neutrals, None, false, opts.slow_search, &mut |p| progress(0.75 + 0.2 * p))?;
            // The same exposure as the floored model, so the two agree on the
            // greys and the blend between them cannot shift lightness.
            let free_predicted: Vec<Vec3> = balanced.iter().map(|c| free_fm * *c * exposure).collect();
            let free_errors: Vec<f64> = free_predicted.iter().zip(reference.iter()).map(|(p, r)| ciede2000(*r, *p, d50)).collect();
            report.free_matrix_mean_de = Some(weighted_mean(&free_errors, &weights));
            let free_targets = lightness_targets(&free_predicted, &reference, share);
            let free_lut = ChromaTable::fit(&free_predicted, &free_targets, &weights, opts.lut.smoothing).map(|t| t.with_held_lightness(false));
            free = Some(FreeModel { forward_matrix: free_fm, lut: free_lut });
        }
    }
    let mut dcp_forward_matrix = None;
    if lut.is_some() && opts.dcp_primaries_inside_prophoto {
        let from_xyz = crate::color::prophoto_to_xyz().inverted_or_identity();
        let inside = |m: &Mat3| {
            let pp = from_xyz * *m;
            (0..3).all(|r| (0..3).all(|c| pp[r][c] >= -1.0e-4))
        };
        if !inside(&fm) {
            let base =
                fit_forward_matrix(&balanced, &reference, &weights, &neutrals, floor, true, opts.slow_search, &mut |p| progress(0.95 + 0.05 * p))?;
            dcp_forward_matrix = Some(base);
        }
    }
    if let Some(f) = floor
        && !held.is_empty()
    {
        let names = ["red", "green", "blue"];
        let channels: Vec<&str> = held.iter().map(|&c| names[c]).collect();
        let free_shares = match &free {
            Some(model) => {
                let shares: Vec<String> = held.iter().map(|&c| format!("{:.2}", model.forward_matrix.row(1)[c])).collect();
                format!(", where a free fit gives {}", shares.join(" and "))
            }
            None => String::new(),
        };
        report.notes.push(format!(
            "the {} channel's luminance share is held at {f} in the forward matrix{free_shares}; \
                 less renders colours more saturated than the chart's darker and darker{}",
            channels.join(" and "),
            if free.is_some() {
                ". The profile follows the free fit wherever its luminance holds up and the \
                     floored matrix beyond, which is also the DCP's ForwardMatrix"
            } else {
                ""
            }
        ));
    }
    progress(1.0);

    let mut prof = Profile {
        camera_name: opts.camera_name.clone(),
        illuminant_name: canonical_illuminant_name(&illuminant_name),
        illuminant_spectrum: Some(illuminant),
        illuminant_white_point: calib_wp,
        color_matrix,
        lut_matrix: fm,
        forward_matrix: fm,
        white_balance: neutral,
        exposure,
        has_lut: lut.is_some(),
        lut,
        free,
        dcp_forward_matrix,
        lut_patch_errors: matrix_errors.clone(),
        matrix_patch_errors: matrix_errors,
        reference_xyz: reference,
        camera_rgb: camera,
        patch_ids: work.patches.iter().map(|p| p.id.clone()).collect(),
    };
    if prof.lut.is_some() {
        prof.lut_patch_errors = prof.camera_rgb.iter().zip(prof.reference_xyz.iter()).map(|(c, r)| ciede2000(*r, prof.render(*c), d50)).collect();
    }
    report.lut_mean_de = weighted_mean(&prof.lut_patch_errors, &weights);
    Ok((prof, report))
}

/// The patches whose reference colour is a mid grey: near the neutral axis
/// and neither as light as a white nor as dark as a black, which are the
/// patches least affected by clipping, flare and noise.
fn neutral_patches(ref_lab: &[Vec3], weights: &[f64]) -> Vec<usize> {
    (0..ref_lab.len())
        .filter(|&i| {
            let lab = ref_lab[i];
            let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            weights[i] > 0.0 && chroma < 3.0 && (25.0..=92.0).contains(&lab[0])
        })
        .collect()
}

/// The camera's neutral: the mean chromaticity of the given patches, scaled so
/// its largest channel is 1.
fn camera_neutral(camera: &[Vec3], indices: &[usize]) -> Option<Vec3> {
    let mut sum = Vec3::ZERO;
    let mut count = 0usize;
    for &i in indices {
        let c = camera[i];
        if c.min() > 0.0 && c.is_finite() {
            sum += c / c[1];
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    let mean = sum / count as f64;
    Some(mean / mean.max())
}

/// The exposure that puts the neutral patches at their reference luminance,
/// as a geometric mean so that a dark grey counts as much as a light one.
fn neutral_exposure(fm: &Mat3, balanced: &[Vec3], reference: &[Vec3], neutrals: &[usize]) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for &i in neutrals {
        let predicted = (*fm * balanced[i])[1];
        if predicted > 0.0 && reference[i][1] > 0.0 {
            sum += (reference[i][1] / predicted).ln();
            count += 1;
        }
    }
    if count == 0 { 1.0 } else { (sum / count as f64).exp() }
}

/// A forward matrix from its six free entries: the first two columns. The
/// third follows from the white constraint `M * (1,1,1) = D50`.
fn forward_from(p: &[f64]) -> Mat3 {
    let d50 = crate::color::D50;
    Mat3::from_rows([p[0], p[3], d50[0] - p[0] - p[3]], [p[1], p[4], d50[1] - p[1] - p[4]], [p[2], p[5], d50[2] - p[2] - p[5]])
}

/// Keep each entry of the forward matrix's luminance row at `floor` or above.
///
/// With the white constraint the row is `[p[1], p[4], 1 - p[1] - p[4]]`, so the
/// allowed region is a triangle in the plane of `p[1]` and `p[4]`; a candidate
/// outside it moves to the nearest point of the triangle.
fn project_luminance_row(p: &mut [f64], floor: f64) {
    let total = (crate::color::D50[1] - 3.0 * floor).max(0.0);
    // Relative to the floor the triangle is x >= 0, y >= 0, x + y <= total.
    let (x, y) = (p[1] - floor, p[4] - floor);
    if x >= 0.0 && y >= 0.0 && x + y <= total {
        return;
    }
    let along = ((x - y + total) / 2.0).clamp(0.0, total);
    let candidates = [(0.0, y.clamp(0.0, total)), (x.clamp(0.0, total), 0.0), (along, total - along)];
    let mut best = candidates[0];
    let mut best_distance = f64::INFINITY;
    for c in candidates {
        let d = (c.0 - x).powi(2) + (c.1 - y).powi(2);
        if d < best_distance {
            best_distance = d;
            best = c;
        }
    }
    p[1] = best.0 + floor;
    p[4] = best.1 + floor;
}

/// Fit the white-locked forward matrix.
///
/// The objective is the weighted sum of squared CIEDE2000 differences, with the
/// exposure re-derived from the neutral patches for every candidate, so the
/// matrix cannot trade overall lightness for a better score. With a `floor`,
/// every camera channel keeps at least that share of luminance (see
/// [`MIN_CHANNEL_LUMINANCE`]); with `inside_prophoto`, every camera primary
/// stays inside ProPhoto RGB (see [`Profile::dcp_forward_matrix`]). The floor
/// is kept exactly, by projection; the ProPhoto condition by a penalty steep
/// enough that what remains outside is a few parts in ten thousand.
#[allow(clippy::too_many_arguments)]
fn fit_forward_matrix(
    balanced: &[Vec3],
    reference: &[Vec3],
    weights: &[f64],
    neutrals: &[usize],
    floor: Option<f64>,
    inside_prophoto: bool,
    slow: bool,
    progress: &mut dyn FnMut(f64),
) -> Result<Mat3, ProfileError> {
    let d50 = crate::color::D50;
    if let Some(f) = floor
        && !(f < d50[1] / 3.0)
    {
        return Err(ProfileError::Fit(format!("a luminance floor of {f} per channel leaves no room for a matrix")));
    }
    let project = move |p: &mut [f64]| {
        if let Some(f) = floor {
            project_luminance_row(p, f);
        }
    };
    // Start from weighted least squares, which with the constraint substituted
    // is linear in the six free entries: for each XYZ row,
    //   ref / k - D50 * c2 = a * (c0 - c2) + b * (c1 - c2).
    let k0 = {
        let mut sum = 0.0;
        let mut count = 0usize;
        for &i in neutrals {
            let level = balanced[i].avg();
            if level > 0.0 && reference[i][1] > 0.0 {
                sum += (reference[i][1] / level).ln();
                count += 1;
            }
        }
        if count == 0 { 1.0 } else { (sum / count as f64).exp() }
    };
    let mut start = [0.0f64; 6];
    for row in 0..3 {
        let (mut s00, mut s01, mut s11, mut t0, mut t1) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for i in 0..balanced.len() {
            let w = weights[i];
            if w <= 0.0 {
                continue;
            }
            let c = balanced[i];
            let x0 = c[0] - c[2];
            let x1 = c[1] - c[2];
            let y = reference[i][row] / k0 - d50[row] * c[2];
            s00 += w * x0 * x0;
            s01 += w * x0 * x1;
            s11 += w * x1 * x1;
            t0 += w * x0 * y;
            t1 += w * x1 * y;
        }
        let det = s00 * s11 - s01 * s01;
        if det.abs() < 1e-18 {
            return Err(ProfileError::Fit("the target's colours do not span the camera's space".into()));
        }
        start[row] = (t0 * s11 - t1 * s01) / det;
        start[3 + row] = (t1 * s00 - t0 * s01) / det;
    }

    let from_xyz = crate::color::prophoto_to_xyz().inverted_or_identity();
    // A camera primary a hundredth outside ProPhoto costs as much as every
    // patch being off by ten CIEDE2000.
    let total_weight: f64 = weights.iter().filter(|w| **w > 0.0).sum();
    let penalty = 1.0e6 * total_weight;
    let objective = |p: &[f64]| -> f64 {
        let m = forward_from(p);
        let k = neutral_exposure(&m, balanced, reference, neutrals);
        let mut sum = 0.0;
        for i in 0..balanced.len() {
            let w = weights[i];
            if w <= 0.0 {
                continue;
            }
            let de = ciede2000(reference[i], m * balanced[i] * k, d50);
            sum += w * de * de;
        }
        if inside_prophoto {
            let pp = from_xyz * m;
            let mut outside = 0.0;
            for r in 0..3 {
                for c in 0..3 {
                    outside += pp[r][c].min(0.0).powi(2);
                }
            }
            sum += penalty * outside;
        }
        if sum.is_finite() { sum } else { f64::MAX }
    };

    // Nelder-Mead with restarts: a simplex can stall on a ridge, and restarting
    // from the best point with a fresh simplex costs little and removes that
    // failure mode. With the ProPhoto condition the search also starts from
    // ProPhoto's own primaries, which satisfy it, and keeps the better result.
    let mut starts = vec![start.to_vec()];
    if inside_prophoto {
        let pp = crate::color::prophoto_to_xyz();
        starts.push(vec![pp[0][0], pp[1][0], pp[2][0], pp[0][1], pp[1][1], pp[2][1]]);
    }
    let restarts = if slow { 24 } else { 10 };
    let mut overall: Option<(Vec<f64>, f64)> = None;
    for (n, first) in starts.iter().enumerate() {
        let mut x = first.clone();
        project(&mut x);
        let mut best = objective(&x);
        for round in 0..restarts {
            let scale = 0.05 / (1.0 + round as f64);
            let mut trial = x.clone();
            let result = crate::simplex::minimize(
                objective,
                &mut trial,
                &crate::simplex::SimplexOptions { epsilon: 1.0e-10, scale, constrain: Some(&project), max_iterations: 20_000 },
            );
            progress((n * restarts + round + 1) as f64 / (starts.len() * restarts) as f64);
            if result.value < best - 1.0e-10 {
                best = result.value;
                x = trial;
            } else if round >= 2 {
                break;
            }
        }
        if overall.as_ref().is_none_or(|(_, b)| best < *b) {
            overall = Some((x, best));
        }
    }
    progress(1.0);
    let x = overall.map(|(x, _)| x).unwrap_or_else(|| start.to_vec());
    let m = forward_from(&x);
    if !m.is_finite() {
        return Err(ProfileError::Fit("the matrix search diverged".into()));
    }
    Ok(m)
}

fn weighted_mean(values: &[f64], weights: &[f64]) -> f64 {
    let (mut sum, mut wsum) = (0.0, 0.0);
    for (v, w) in values.iter().zip(weights.iter()) {
        if *w > 0.0 && v.is_finite() {
            sum += v * w;
            wsum += w;
        }
    }
    if wsum > 0.0 { sum / wsum } else { 0.0 }
}

fn weighted_rms(values: &[f64], weights: &[f64]) -> f64 {
    let (mut sum, mut wsum) = (0.0, 0.0);
    for (v, w) in values.iter().zip(weights.iter()) {
        if *w > 0.0 && v.is_finite() {
            sum += v * v * w;
            wsum += w;
        }
    }
    if wsum > 0.0 { (sum / wsum).sqrt() } else { 0.0 }
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 }
}

fn canonical_illuminant_name(name: &str) -> &'static str {
    crate::illuminants::ExifLightSource::from_name(name).map(|l| l.name()).unwrap_or("Unknown")
}

/// The colours a correction table is fitted towards: the chart's own, with each
/// lightness moved from what the model predicts towards the chart's by `share`.
///
/// Hue and chroma — CIELAB `a*` and `b*` — are the chart's whatever the share,
/// so at 0 a patch keeps the lightness the camera's matrix gives it and takes
/// its colour from the chart. Greys are unaffected: the matrix is white-locked
/// and the exposure comes from them, so it already predicts their lightness.
fn lightness_targets(predicted: &[Vec3], reference: &[Vec3], share: f64) -> Vec<Vec3> {
    if share >= 1.0 {
        return reference.to_vec();
    }
    let d50 = crate::color::D50;
    predicted
        .iter()
        .zip(reference.iter())
        .map(|(p, r)| {
            if !(p[1] > 0.0) || !p.is_finite() || !r.is_finite() {
                return *r;
            }
            let lp = xyz_to_lab(*p, d50);
            let lr = xyz_to_lab(*r, d50);
            let l = lp[0] + share * (lr[0] - lp[0]);
            crate::color::lab_to_xyz(Vec3::new(l, lr[1], lr[2]), d50)
        })
        .collect()
}

/// How much of the free model a colour gets, from the luminance each model
/// gives it; see [`Profile::render_balanced`].
fn free_weight(free_y: f64, floored_y: f64) -> f64 {
    if !(floored_y > 0.0) || !free_y.is_finite() {
        return 0.0;
    }
    let t = ((free_y / floored_y - BLEND_FROM) / (BLEND_TO - BLEND_FROM)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The camera signal that `cam2xyz` maps onto `white_point`, scaled so its
/// largest channel is 1.
///
/// For a matrix that takes camera-native values this is the camera's neutral.
/// It is meaningless for a forward matrix, which takes white-balanced values
/// and maps `(1, 1, 1)` to white by construction.
pub fn white_balance_of(cam2xyz: &Mat3, white_point: Vec3) -> Vec3 {
    let cam_wp = cam2xyz.inverted_or_identity() * white_point;
    cam_wp / cam_wp.max()
}

/// Apply the white balance and the matrix to a camera signal.
pub fn apply_matrix(cam2xyz: &Mat3, white_balance: Vec3, cam: Vec3) -> Vec3 {
    *cam2xyz * (cam / white_balance)
}

impl Profile {
    /// Colour difference of every patch when only the linear matrices are used.
    pub fn matrix_errors(&self) -> &[f64] {
        &self.matrix_patch_errors
    }

    /// Colour difference of every patch with the correction table applied.
    pub fn lut_errors(&self) -> &[f64] {
        &self.lut_patch_errors
    }

    /// Render a camera-native signal the way a raw converter does through the
    /// profile, at the target's exposure. The result is XYZ relative to D50.
    pub fn render(&self, cam: Vec3) -> Vec3 {
        self.render_balanced(cam / self.white_balance * self.exposure)
    }

    /// The profile's whole colour transform: white-balanced camera RGB to XYZ
    /// relative to D50, the mapping both the ICC CLUT and the DCP carry.
    ///
    /// Through the forward matrix and its correction table, unless the profile
    /// has a [`FreeModel`]: then a colour renders through the free model while
    /// that gives it at least [`BLEND_TO`] of the floored model's luminance,
    /// through the floored model once it gives [`BLEND_FROM`] or less, and
    /// through a smooth blend of the two between. On the chart both models
    /// reproduce the patches, so the blend only decides what happens between and
    /// beyond them. It is exposure invariant, like each model.
    pub fn render_balanced(&self, balanced: Vec3) -> Vec3 {
        let base = self.forward_matrix * balanced;
        let floored = match &self.lut {
            Some(t) => t.correct_xyz(base),
            None => base,
        };
        let Some(free) = &self.free else {
            return floored;
        };
        let raw = free.forward_matrix * balanced;
        let chart = match &free.lut {
            Some(t) => t.correct_xyz(raw),
            None => raw,
        };
        let w = free_weight(chart[1], floored[1]);
        chart * w + floored * (1.0 - w)
    }

    /// Render a camera-native signal the way a raw converter does with only the
    /// colour matrix: invert it to XYZ under the illuminant whose neutral the
    /// camera reports, then adapt to D50. This is the path a DCP without a
    /// forward matrix takes. At the calibration white it equals the forward
    /// matrix's rendering, which is what deriving the colour matrix guarantees.
    pub fn render_matrix_only(&self, cam: Vec3) -> Vec3 {
        let inverse = self.color_matrix.inverted_or_identity();
        let white = inverse * self.white_balance;
        if !(white[1] > 0.0) {
            return Vec3::ZERO;
        }
        let white = white / white[1];
        let adapt = bradford_map_white(white, crate::color::D50);
        // Scale so the neutral lands on D50 at the target's exposure, as the
        // DNG SDK scales its camera-to-PCS matrix.
        let scale = self.exposure / (adapt * inverse * self.white_balance)[1];
        adapt * inverse * cam * scale
    }

    /// The DNG `ColorMatrix` that goes with a white-locked forward matrix: XYZ
    /// under the calibration illuminant to camera-native RGB, normalised as the
    /// DNG SDK normalises it. For [`Profile::forward_matrix`] this is
    /// [`Profile::color_matrix`]; a DCP written with another forward matrix
    /// needs the one that agrees with it.
    pub fn color_matrix_for(&self, forward: &Mat3) -> Mat3 {
        let to_pcs = bradford_map_white(self.illuminant_white_point, crate::color::D50);
        let cm = Mat3::from_diagonal(self.white_balance) * forward.inverted_or_identity() * to_pcs;
        let peak = (cm * crate::color::D50).max();
        if peak > 0.0 { cm.scaled(1.0 / peak) } else { cm }
    }

    /// True when the profile carries a correction table.
    pub fn has_correction_table(&self) -> bool {
        self.has_lut
    }

    /// The DNG reference white point this profile was built against.
    pub fn dng_reference_white_point(&self) -> Vec3 {
        target::dng_d50()
    }

    /// The ICC reference white point this profile was built against.
    pub fn icc_reference_white_point(&self) -> Vec3 {
        target::icc_d50()
    }

    /// The Bradford transform that moves the forward matrix's white onto the
    /// DNG D50. It is the identity for a fitted profile, whose forward matrix
    /// is white-locked to D50; it matters for one loaded from elsewhere.
    pub fn dng_white_map(&self) -> Mat3 {
        let white = self.forward_matrix * Vec3::ONE;
        bradford_map_white(white, crate::color::D50)
    }

    /// Deviation between the forward matrix white point and D50, in
    /// CIEDE2000.
    pub fn icc_white_point_deviation(&self) -> f64 {
        let white = self.forward_matrix * Vec3::ONE;
        let d50 = crate::color::D50;
        ciede2000(white, d50, d50)
    }

    /// A one line summary of the profile's colour accuracy.
    pub fn summary(&self) -> String {
        format!("matrix only: mean DE {:.2}; with correction table: mean DE {:.2}", mean(&self.matrix_patch_errors), mean(&self.lut_patch_errors))
    }
}

/// The chromaticity primaries of the working colour space, in `u'v'`.
pub fn working_gamut_primaries_uv() -> [Vec3; 3] {
    let rgb2xyz = crate::color::prophoto_to_xyz();
    [xyz_to_uvy(rgb2xyz * Vec3::new(1.0, 0.0, 0.0)), xyz_to_uvy(rgb2xyz * Vec3::new(0.0, 1.0, 0.0)), xyz_to_uvy(rgb2xyz * Vec3::new(0.0, 0.0, 1.0))]
}

/// Adapt an XYZ value from one white point to another with Bradford.
pub fn adapt(xyz: Vec3, from: Vec3, to: Vec3) -> Vec3 {
    crate::color::chromatic_adaptation(Cat::Bradford, xyz, from, to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::lab_to_xyz;
    use crate::target::Patch;

    /// A ColorChecker-like target seen by a camera with a known response:
    /// reference XYZ under D50 and camera-native signals from a fixed
    /// camera-to-XYZ matrix with its own (non-neutral) channel gains.
    fn synthetic_target(camera_to_xyz: Mat3) -> PatchSet {
        let xyz_to_camera = camera_to_xyz.inverted().unwrap();
        let mut ps = PatchSet::new();
        ps.illuminant_name = "D50".into();
        ps.illuminant_white_point = crate::color::D50;
        let mut add = |id: String, lab: Vec3| {
            let xyz = lab_to_xyz(lab, crate::color::D50);
            let mut p = Patch::new(id);
            p.xyz = xyz;
            p.cam = xyz_to_camera * xyz;
            ps.push(p, None);
        };
        for i in 0..18 {
            let hue = (i as f64 * 20.0).to_radians();
            let chroma = 20.0 + (i % 3) as f64 * 15.0;
            add(format!("C{i:02}"), Vec3::new(40.0 + (i % 4) as f64 * 10.0, chroma * hue.cos(), chroma * hue.sin()));
        }
        for (i, l) in [96.0, 81.0, 66.0, 51.0, 36.0, 20.0].iter().enumerate() {
            add(format!("N{i}"), Vec3::new(*l, 0.0, 0.0));
        }
        ps
    }

    fn camera() -> Mat3 {
        // Unequal channel gains, as a real sensor has: green reads highest.
        Mat3::from_rows([0.62, 0.28, 0.09], [0.21, 0.72, 0.14], [0.05, 0.16, 0.79]) * Mat3::from_diagonal(Vec3::new(1.9, 1.0, 1.6))
    }

    #[test]
    fn a_linear_camera_is_recovered_exactly() {
        let ps = synthetic_target(camera());
        let (prof, report) = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap();
        assert!(report.matrix_mean_de < 0.01, "matrix mean DE {}", report.matrix_mean_de);
        // The forward matrix is white locked.
        let white = prof.forward_matrix * Vec3::ONE;
        assert!(white.distance(&crate::color::D50) < 1e-12, "{white:?}");
        // The white balance is the camera's response to the target's neutral.
        let truth = camera().inverted().unwrap() * crate::color::D50;
        let truth = truth / truth.max();
        assert!(prof.white_balance.distance(&truth) < 1e-6, "{:?} vs {truth:?}", prof.white_balance);
        // The colour matrix maps the calibration white onto that neutral, and
        // is DNG-normalised: D50 maps to a signal whose largest channel is 1.
        let n = prof.color_matrix * prof.illuminant_white_point;
        assert!((n / n.max()).distance(&prof.white_balance) < 1e-9, "{n:?}");
        assert!(((prof.color_matrix * crate::color::D50).max() - 1.0).abs() < 1e-12);
    }

    /// A camera whose true forward matrix gives blue a negative share of
    /// luminance, as a chart fit of many real cameras does.
    fn camera_with_negative_blue_luminance() -> Mat3 {
        Mat3::from_rows([0.66, 0.28, 0.02], [0.24, 0.98, -0.22], [0.02, -0.14, 0.95]) * Mat3::from_diagonal(Vec3::new(1.9, 1.0, 1.6))
    }

    #[test]
    fn the_luminance_row_projection_finds_the_nearest_allowed_row() {
        let floor = 0.03;
        for (a, b) in [(0.24, 1.18), (-0.2, 0.5), (0.9, 0.9), (0.5, -0.4), (0.3, 0.6), (2.0, -1.0)] {
            let mut p = [0.0, a, 0.0, 0.0, b, 0.0];
            project_luminance_row(&mut p, floor);
            let row = [p[1], p[4], 1.0 - p[1] - p[4]];
            assert!(row.iter().all(|v| *v >= floor - 1.0e-12), "{row:?} from {a}, {b}");
            // Nothing in the allowed triangle is closer to the start.
            let d = (p[1] - a).powi(2) + (p[4] - b).powi(2);
            for i in 0..=40 {
                for j in 0..=40 {
                    let x = floor + (1.0 - 3.0 * floor) * i as f64 / 40.0;
                    let y = floor + (1.0 - 3.0 * floor) * j as f64 / 40.0;
                    if 1.0 - x - y >= floor - 1.0e-12 {
                        assert!((x - a).powi(2) + (y - b).powi(2) >= d - 1.0e-12, "({x}, {y}) beats {p:?}");
                    }
                }
            }
        }
        // A row already inside is left alone.
        let mut p = [0.0, 0.3, 0.0, 0.0, 0.6, 0.0];
        project_luminance_row(&mut p, floor);
        assert_eq!((p[1], p[4]), (0.3, 0.6));
    }

    #[test]
    fn no_camera_channel_gets_a_negative_share_of_luminance() {
        let ps = synthetic_target(camera_with_negative_blue_luminance());
        // Left free, the fit recovers the camera, negative blue weight and all ...
        let free = FitOptions { min_channel_luminance: None, ..Default::default() };
        let (prof_free, report_free) = fit(&ps, &free, &mut |_| {}).unwrap();
        assert!(report_free.matrix_mean_de < 0.01, "{}", report_free.matrix_mean_de);
        assert!(prof_free.forward_matrix.row(1)[2] < -0.1, "{:?}", prof_free.forward_matrix);
        // ... and renders a camera blue beyond the chart at negative luminance.
        let deep_blue = Mat3::from_diagonal(prof_free.white_balance) * Vec3::new(0.02, 0.06, 0.5);
        assert!(prof_free.render(deep_blue)[1] < 0.0);

        // By default every channel keeps its floor, the correction table puts
        // the chart's colours back, and the deep blue stays a visible colour.
        let (prof, report) = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap();
        let row = prof.forward_matrix.row(1);
        let row = [row[0], row[1], row[2]];
        assert!(row.iter().all(|v| *v >= MIN_CHANNEL_LUMINANCE - 1.0e-9), "{row:?}");
        assert!((row.iter().sum::<f64>() - 1.0).abs() < 1.0e-12, "{row:?}");
        assert!(report.notes.iter().any(|n| n.contains("blue channel's luminance share")), "{:?}", report.notes);
        assert!(report.lut_mean_de < 0.5, "table {} (matrix {})", report.lut_mean_de, report.matrix_mean_de);
        assert!(prof.render(deep_blue)[1] > 0.0);
    }

    #[test]
    fn the_profile_follows_the_free_fit_until_its_luminance_gives_way() {
        let ps = synthetic_target(camera_with_negative_blue_luminance());
        let (prof, report) = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap();
        let free = prof.free.as_ref().expect("the floor binds, so there is a free model");
        assert!(report.free_matrix_mean_de.unwrap() < 0.01);
        // The camera is linear, so the free fit is the camera itself, and on the
        // chart the profile renders exactly through it.
        for (cam, reference) in prof.camera_rgb.iter().zip(prof.reference_xyz.iter()) {
            let de = ciede2000(*reference, prof.render(*cam), crate::color::D50);
            assert!(de < 0.05, "patch rendered {de} DE away");
        }
        // A deep blue the free fit takes to nearly no luminance renders
        // through the floored model instead.
        let balanced = Vec3::new(0.02, 0.06, 0.5);
        let floored = prof.lut.as_ref().unwrap().correct_xyz(prof.forward_matrix * balanced);
        assert!((free.forward_matrix * balanced)[1] < 0.6 * floored[1]);
        assert!(prof.render_balanced(balanced).distance(&floored) < 1.0e-12);
        // The transform is exposure invariant and continuous along a line from
        // grey into the blue corner, through the blend.
        let x = prof.render_balanced(balanced);
        assert!(prof.render_balanced(balanced * 3.0).distance(&(x * 3.0)) < 1.0e-12);
        let mut last = prof.render_balanced(Vec3::new(0.2, 0.2, 0.2));
        for step in 1..=400 {
            let s = step as f64 / 400.0;
            let c = Vec3::new(0.2, 0.2, 0.2) * (1.0 - s) + balanced * s;
            let now = prof.render_balanced(c);
            assert!(now.distance(&last) < 0.01, "jump at {s}: {last:?} -> {now:?}");
            last = now;
        }
    }

    #[test]
    fn the_dcp_forward_matrix_keeps_every_camera_colour_inside_prophoto() {
        let ps = synthetic_target(camera_with_negative_blue_luminance());
        let (prof, _) = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap();
        let base = prof.dcp_forward_matrix.expect("the chart fit leaves ProPhoto, so a DCP base is fitted");
        let from_xyz = crate::color::prophoto_to_xyz().inverted_or_identity();
        let pp = from_xyz * base;
        for r in 0..3 {
            for c in 0..3 {
                assert!(pp[r][c] > -1.0e-3, "primary {c} leaves ProPhoto: {pp:?}");
            }
        }
        // White-locked and floored like the profile's own matrix.
        assert!((base * Vec3::ONE).distance(&crate::color::D50) < 1.0e-12);
        let row = base.row(1);
        assert!([row[0], row[1], row[2]].iter().all(|v| *v >= MIN_CHANNEL_LUMINANCE - 1.0e-9), "{row:?}");
        // So every camera colour stays inside: what a DNG renderer clips is
        // never more than rounding.
        for cam in [Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.9, 0.8, 0.0)] {
            let rgb = from_xyz * (base * cam);
            assert!(rgb.min() > -1.0e-3, "{cam:?} -> {rgb:?}");
        }
        // A matrix-only fit has no HueSatMap to carry the rest, so it has none.
        let (plain, _) = fit(&ps, &FitOptions { make_lut: false, ..Default::default() }, &mut |_| {}).unwrap();
        assert!(plain.dcp_forward_matrix.is_none());
    }

    #[test]
    fn the_colour_matrix_for_the_forward_matrix_is_the_profiles_own() {
        let ps = synthetic_target(camera_with_negative_blue_luminance());
        let (prof, _) = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap();
        let derived = prof.color_matrix_for(&prof.forward_matrix);
        for r in 0..3 {
            for c in 0..3 {
                assert!((derived[r][c] - prof.color_matrix[r][c]).abs() < 1.0e-12);
            }
        }
        // Any white-locked matrix's colour matrix takes the calibration white
        // to the camera's neutral.
        let base = prof.dcp_forward_matrix.unwrap();
        let n = prof.color_matrix_for(&base) * prof.illuminant_white_point;
        assert!((n / n.max()).distance(&prof.white_balance) < 1.0e-9, "{n:?}");
    }

    #[test]
    fn the_natural_rendering_keeps_the_cameras_lightness_and_the_charts_colour() {
        // Saturated colours read lighter in the camera than they are, as the
        // test cameras' matrices do with the chart's reds.
        let mut ps = synthetic_target(camera());
        for p in &mut ps.patches {
            let lab = xyz_to_lab(p.xyz, crate::color::D50);
            let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            let lighter = lab_to_xyz(lab + Vec3::new(chroma * 0.06, 0.0, 0.0), crate::color::D50);
            p.cam = camera().inverted().unwrap() * lighter;
        }
        let accurate = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap().0;
        let natural_options = FitOptions { lut: LutFitOptions { chart_lightness: NATURAL, ..LutFitOptions::default() }, ..FitOptions::default() };
        let natural = fit(&ps, &natural_options, &mut |_| {}).unwrap().0;
        let mut accurate_lift = 0.0;
        let mut natural_lift = 0.0;
        for (i, (cam, reference)) in natural.camera_rgb.iter().zip(natural.reference_xyz.iter()).enumerate() {
            let r = xyz_to_lab(*reference, crate::color::D50);
            let a = xyz_to_lab(accurate.render(*cam), crate::color::D50);
            let n = xyz_to_lab(natural.render(*cam), crate::color::D50);
            let m = xyz_to_lab(natural.forward_matrix * (*cam / natural.white_balance) * natural.exposure, crate::color::D50);
            accurate_lift += a[0] - r[0];
            natural_lift += n[0] - r[0];
            // Natural keeps the matrix's lightness and takes the chart's a* b*.
            assert!((n[0] - m[0]).abs() < 0.6, "patch {i}: L* {} vs the matrix's {}", n[0], m[0]);
            assert!((n[1] - r[1]).abs() < 1.0 && (n[2] - r[2]).abs() < 1.0, "patch {i}: {n:?} vs {r:?}");
        }
        let count = natural.camera_rgb.len() as f64;
        assert!(accurate_lift / count < 0.2, "accurate lifts by {}", accurate_lift / count);
        assert!(natural_lift / count > 1.0, "natural lifts by only {}", natural_lift / count);
        // Greys are the same in both.
        let grey = natural.white_balance * 0.2;
        let de = ciede2000(natural.render(grey), accurate.render(grey), crate::color::D50);
        assert!(de < 0.05, "greys differ by {de} DE");
    }

    #[test]
    fn the_blend_weight_runs_smoothly_from_floored_to_free() {
        assert_eq!(free_weight(0.5, 1.0), 0.0);
        assert_eq!(free_weight(BLEND_FROM, 1.0), 0.0);
        assert_eq!(free_weight(BLEND_TO, 1.0), 1.0);
        assert_eq!(free_weight(1.4, 1.0), 1.0);
        assert_eq!(free_weight(-0.3, 1.0), 0.0);
        assert_eq!(free_weight(0.5, 0.0), 0.0);
        let mid = free_weight((BLEND_FROM + BLEND_TO) / 2.0, 1.0);
        assert!((mid - 0.5).abs() < 1.0e-12);
        // Scale invariant: only the ratio matters.
        assert!((free_weight(0.8, 1.0) - free_weight(0.08, 0.1)).abs() < 1.0e-12);
    }

    #[test]
    fn both_dng_paths_agree_at_the_calibration_white() {
        let ps = synthetic_target(camera());
        let (prof, _) = fit(&ps, &FitOptions { make_lut: false, ..Default::default() }, &mut |_| {}).unwrap();
        for cam in &prof.camera_rgb {
            let a = prof.render(*cam);
            let b = prof.render_matrix_only(*cam);
            assert!(a.distance(&b) < 1e-9, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn a_different_illuminant_derives_its_own_colour_matrix() {
        // Reference colours under D55, as a chart shot in daylight is described.
        let mut ps = synthetic_target(camera());
        let d55 = illuminants::white_point_by_name("1931_2", "D55").unwrap();
        let to_d55 = bradford_map_white(crate::color::D50, d55);
        for p in &mut ps.patches {
            p.xyz = to_d55 * p.xyz;
            p.cam = camera().inverted().unwrap() * p.xyz;
        }
        let opts = FitOptions { illuminant_name: Some("D55".into()), ..Default::default() };
        let (prof, report) = fit(&ps, &opts, &mut |_| {}).unwrap();
        assert_eq!(prof.illuminant_name, "D55");
        assert!(report.matrix_mean_de < 0.05, "{}", report.matrix_mean_de);
        // The colour matrix maps the D55 white onto the camera neutral.
        let n = prof.color_matrix * d55;
        assert!((n / n.max()).distance(&prof.white_balance) < 1e-6, "{n:?}");
    }

    #[test]
    fn the_correction_table_absorbs_a_systematic_error() {
        // Saturated colours read lighter than they are, as with a real camera.
        let mut ps = synthetic_target(camera());
        for p in &mut ps.patches {
            let lab = xyz_to_lab(p.xyz, crate::color::D50);
            let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            let lighter = lab_to_xyz(lab + Vec3::new(chroma * 0.06, 0.0, 0.0), crate::color::D50);
            p.cam = camera().inverted().unwrap() * lighter;
        }
        let (prof, report) = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap();
        assert!(prof.has_lut);
        assert!(report.lut_mean_de < report.matrix_mean_de * 0.6, "table {} vs matrix {}", report.lut_mean_de, report.matrix_mean_de);
        // Greys stay neutral through the table.
        let grey = prof.render(prof.white_balance * 0.3);
        let lab = xyz_to_lab(grey, crate::color::D50);
        assert!(lab[1].abs() < 0.05 && lab[2].abs() < 0.05, "{lab:?}");
    }

    #[test]
    fn render_matches_the_reported_errors() {
        let ps = synthetic_target(camera());
        let (prof, _) = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap();
        for (i, cam) in prof.camera_rgb.iter().enumerate() {
            let de = ciede2000(prof.reference_xyz[i], prof.render(*cam), crate::color::D50);
            assert!((de - prof.lut_patch_errors[i]).abs() < 1e-9, "patch {i}");
        }
    }

    #[test]
    fn a_clipped_patch_is_left_out() {
        let mut ps = synthetic_target(camera());
        // Normalise the camera signal to a white level the brightest patch hits.
        let peak = ps.patches.iter().map(|p| p.cam.max()).fold(0.0f64, f64::max);
        for p in &mut ps.patches {
            p.cam = p.cam / peak;
        }
        let (_, report) = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap();
        assert_eq!(report.patches_clipped, 1, "{:?}", report.notes);
        assert_eq!(report.patches_used, ps.len() - 1);
    }

    #[test]
    fn the_neutral_comes_from_the_mid_greys() {
        let ps = synthetic_target(camera());
        let (_, report) = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap();
        assert_eq!(report.neutral_ids, vec!["N1", "N2", "N3", "N4"]);
    }

    #[test]
    fn empty_target_is_rejected() {
        let ps = PatchSet::new();
        let e = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap_err();
        assert!(matches!(e, ProfileError::Fit(_) | ProfileError::Target(_)), "{e}");
        // unknown illuminant is reported
        {
            let mut ps = synthetic_target(camera());
            ps.illuminant_name = "NorthLight".into();
            let e = fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap_err();
            assert!(matches!(e, ProfileError::Color(ColorError::UnknownIlluminant(_))));
        }
    }

    #[test]
    fn working_primaries_are_a_triangle_in_uv() {
        for v in working_gamut_primaries_uv() {
            assert!(v[0] > 0.0 && v[0] < 1.0, "{v:?}");
            assert!(v[1] > 0.0 && v[1] < 1.0, "{v:?}");
        }
    }
}
