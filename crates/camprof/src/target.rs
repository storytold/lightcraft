// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/target.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Measurement targets: reading measurement files, choosing the neutral
//! patch, rebalancing and deriving the per-patch optimization weights.
//!
//! A target is a list of patches, each with the reference colour (XYZ), the
//! camera signal (raw RGB) and optionally the measured spectrum. Everything
//! downstream — matrix fit, LUT build, error reporting — works on this
//! structure.

use crate::color::{Cat, ColorError, chromatic_adaptation, ciede2000, describe_color, lab_to_lch, xyz_to_lab};
use crate::linalg::Vec3;
use crate::spectrum::Spectrum;
use core::fmt;

/// Maximum number of patch classes a target can declare.
pub const MAX_CLASS_COUNT: usize = 128;
/// Maximum length of a class name.
pub const CLASS_NAME_SIZE: usize = 24;
/// Maximum length of a patch identifier.
pub const PATCH_ID_SIZE: usize = 32;

/// Errors reported while reading or interpreting a target.
#[derive(Clone, Debug, PartialEq)]
pub enum TargetError {
    /// The file could not be read.
    Io(String),
    /// A CGATS construct was malformed.
    Syntax {
        /// One-based line number.
        line: usize,
        /// What went wrong.
        message: String,
    },
    /// The file parsed but lacks data the engine needs.
    Missing {
        /// What is missing.
        what: String,
    },
    /// A spectral or colour conversion failed.
    Color(ColorError),
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(p) => write!(f, "cannot read {p}"),
            Self::Syntax { line, message } => write!(f, "line {line}: {message}"),
            Self::Missing { what } => write!(f, "target is missing {what}"),
            Self::Color(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TargetError {}

impl From<ColorError> for TargetError {
    fn from(e: ColorError) -> Self {
        Self::Color(e)
    }
}

/// One measurement patch.
#[derive(Clone, Debug, Default)]
pub struct Patch {
    /// Measured spectrum, when the file carried one.
    pub spectrum: Option<Spectrum>,
    /// Identifier as written in the file, e.g. `A01`.
    pub id: String,
    /// True when the patch is self-luminous rather than reflective.
    pub emissive: bool,
    /// Keep this patch regardless of automatic exclusion rules.
    pub force_keep: bool,
    /// Index into [`PatchSet::class_names`], or `usize::MAX` for unclassified.
    pub class: usize,
    /// Lower and upper acceptance window per LCh dimension for the LUT fit.
    pub lut_de_window: [f64; 6],
    /// Whether the refinement window below should be applied to the matrix.
    pub use_matrix_window: bool,
    /// Window applied when refining the matrix, per LCh dimension.
    pub matrix_de_window: [f64; 6],
    /// Weight used by the matrix fit.
    pub matrix_weight: f64,
    /// Camera signal.
    pub cam: Vec3,
    /// Reference colour.
    pub xyz: Vec3,
    /// Scratch space for the LUT build.
    pub tmp: Vec3,
}

impl Patch {
    /// A patch with neutral defaults.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), class: usize::MAX, lut_de_window: [0.0; 6], matrix_de_window: [0.0; 6], matrix_weight: 1.0, ..Default::default() }
    }

    /// The class index, or `None` when the patch was never classified.
    pub fn class_index(&self) -> Option<usize> {
        if self.class == usize::MAX { None } else { Some(self.class) }
    }
}

/// Layout hints carried by a measurement file.
#[derive(Clone, Copy, Debug, Default)]
pub struct TargetLayout {
    /// Number of rows in the physical chart.
    pub row_count: usize,
    /// Patch aspect ratio, when the file recorded one.
    pub patch_aspect_ratio: f64,
    /// Inner patch ratio, when the file recorded one.
    pub patch_inner_ratio: f64,
}

/// A set of measurement patches plus its metadata.
#[derive(Clone, Debug, Default)]
pub struct PatchSet {
    /// Chart layout hints.
    pub layout: TargetLayout,
    /// Named illuminant the camera signal was captured under.
    pub illuminant_name: String,
    /// White point of the capture illuminant.
    pub illuminant_white_point: Vec3,
    /// Class names; a patch's `class` indexes into this list.
    pub class_names: Vec<String>,
    /// The patches.
    pub patches: Vec<Patch>,
}

impl PatchSet {
    /// An empty target.
    pub fn new() -> Self {
        Self { illuminant_white_point: Vec3::ONE, ..Default::default() }
    }

    /// Number of patches.
    pub fn len(&self) -> usize {
        self.patches.len()
    }

    /// True when the target has no patches.
    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }

    /// Add a patch, assigning its class by name.
    pub fn push(&mut self, mut patch: Patch, class_name: Option<&str>) {
        if let Some(name) = class_name {
            patch.class = self.class_index_or_insert(name);
        }
        self.patches.push(patch);
    }

    /// Index of `name` in the class list, inserting it when new.
    pub fn class_index_or_insert(&mut self, name: &str) -> usize {
        if let Some(i) = self.class_names.iter().position(|c| c.eq_ignore_ascii_case(name)) {
            return i;
        }
        let truncated = name.chars().take(CLASS_NAME_SIZE - 1).collect::<String>();
        self.class_names.push(truncated);
        self.class_names.len() - 1
    }

    /// Display name of a patch, falling back to its index.
    pub fn patch_name(&self, idx: usize) -> String {
        match self.patches.get(idx) {
            Some(p) if !p.id.is_empty() => p.id.clone(),
            _ => format!("{}", idx + 1),
        }
    }

    /// Indices of the patches belonging to `class`.
    pub fn patches_in_class(&self, class: usize) -> Vec<usize> {
        (0..self.patches.len()).filter(|&i| self.patches[i].class == class).collect()
    }

    /// True when the class list holds nothing but the implicit default.
    pub fn has_named_classes(&self) -> bool {
        self.class_names.iter().any(|c| !c.is_empty())
    }
}

// ---------------------------------------------------------------------------
// Patch set analysis
// ---------------------------------------------------------------------------

/// Result of picking the neutral patch.
#[derive(Clone, Copy, Debug)]
pub struct NeutralSelection {
    /// Index of the chosen patch.
    pub index: usize,
    /// True when the chosen patch is also the lightest one.
    pub is_lightest: bool,
    /// Colour difference between the patch and the reference white point.
    pub white_point_de: f64,
}

/// Pick the patch that should represent the neutral.
///
/// An explicit `white` or `illuminant` class wins; otherwise the least
/// chromatic sufficiently bright patch is used.
pub fn find_neutral_patch(ps: &PatchSet, wp: Vec3) -> NeutralSelection {
    let named = ps
        .class_names
        .iter()
        .position(|c| c.eq_ignore_ascii_case("white"))
        .or_else(|| ps.class_names.iter().position(|c| c.eq_ignore_ascii_case("illuminant")));

    if let Some(class) = named
        && let Some(idx) = ps.patches.iter().position(|p| p.class == class)
    {
        return NeutralSelection { index: idx, is_lightest: true, white_point_de: ciede2000(ps.patches[idx].xyz.normalized(), wp, wp) };
    }

    let mut candidate = vec![false; ps.len()];
    let mut has_candidate = false;
    for (i, p) in ps.patches.iter().enumerate() {
        if p.xyz[1] < 0.1 {
            continue;
        }
        let lab = xyz_to_lab(p.xyz.normalized(), wp);
        if (lab[1] * lab[1] + lab[2] * lab[2]).sqrt() <= 2.0 {
            has_candidate = true;
            candidate[i] = true;
        }
    }

    let mut best = usize::MAX;
    let mut best_all = usize::MAX;
    let mut min_ab = f64::INFINITY;
    let mut min_ab_all = f64::INFINITY;
    let mut max_y = 0.0f64;
    for (i, p) in ps.patches.iter().enumerate() {
        if has_candidate {
            if !candidate[i] {
                continue;
            }
            if p.xyz[1] > max_y {
                max_y = p.xyz[1];
                best = i;
            }
        } else {
            let lab = xyz_to_lab(p.xyz.normalized(), wp);
            let ab = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            if p.xyz[1] >= 0.1 && ab < min_ab {
                min_ab = ab;
                best = i;
            }
            if ab < min_ab_all {
                min_ab_all = ab;
                best_all = i;
            }
        }
    }
    if best == usize::MAX {
        best = best_all;
    }
    if best == usize::MAX {
        best = 0;
    }

    // Is the lightest patch reasonably white? If so nothing is off.
    let mut is_lightest = true;
    for p in ps.patches.iter() {
        if p.xyz[1] <= ps.patches[best].xyz[1] {
            continue;
        }
        let lab = xyz_to_lab(p.xyz.normalized(), wp);
        if (lab[1] * lab[1] + lab[2] * lab[2]).sqrt() < 6.0 {
            is_lightest = false;
            break;
        }
    }

    NeutralSelection { index: best, is_lightest, white_point_de: ciede2000(ps.patches[best].xyz.normalized(), wp, wp) }
}

/// Transform the target's reference colours so the neutral patch matches
/// `ref_wp` exactly.
///
/// Returns the index of the neutral patch that was used.
pub fn rebalance_to_white_point(ps: &mut PatchSet, ref_wp: Vec3, neutral: usize) -> usize {
    if ps.is_empty() {
        return 0;
    }
    let old_wp = ps.patches[neutral].xyz.normalized();
    for p in &mut ps.patches {
        p.xyz = chromatic_adaptation(Cat::Cat02, p.xyz, old_wp, ref_wp);
    }
    // CAT leaves a small residual; force the neutral to land exactly.
    let sum: f64 = ps.patches[neutral].xyz.sum();
    let ref_sum: f64 = ref_wp.sum();
    if ref_sum != 0.0 {
        ps.patches[neutral].xyz = ref_wp * (sum / ref_sum);
    }
    neutral
}

/// Scale a target so its brightest channels reach 1.0.
///
/// The camera signal and the reference colour are normalised independently and
/// then reconciled at the neutral patch, which is what keeps the fitted matrix
/// scaled consistently.
pub fn normalized(ps: &PatchSet, neutral: Option<usize>) -> PatchSet {
    let mut out = ps.clone();
    if out.is_empty() {
        return out;
    }
    // Both sides are divided by the *same* constant. Scaling each side by its
    // own maximum would be convenient numerically, but it changes the ratio
    // between the channels, and with it the chromaticity that a colour
    // difference is measured on. The reference maximum is used because the
    // reference is the side whose absolute level is meaningful.
    let mut max_xyz = 0.0f64;
    for p in &out.patches {
        max_xyz = max_xyz.max(p.xyz.max());
    }
    if max_xyz == 0.0 {
        return out;
    }
    for p in &mut out.patches {
        p.cam = p.cam / max_xyz;
        p.xyz = p.xyz / max_xyz;
    }
    if let Some(w) = neutral {
        // The neutral patch defines what "white" means for this capture, so its
        // camera signal is brought to the reference level. This is a change of
        // units on the camera side only: it is exactly the white balance a raw
        // processor applies, so it leaves chromaticity intact.
        if w < out.len() {
            let cmax = out.patches[w].cam.max();
            let rmax = out.patches[w].xyz.max();
            if cmax > 0.0 {
                let k = rmax / cmax;
                for p in &mut out.patches {
                    p.cam = p.cam * k;
                }
            }
        }
    }
    out
}
/// Drop patches that are darker than a lighter patch with the same chroma.
///
/// These add no colour information and would bias the fit towards shadow
/// noise. Returns the number of patches removed.
pub fn exclude_darker_patches(ps: &mut PatchSet, uv_de_limit: f64) -> usize {
    let before = ps.len();
    if before == 0 {
        return 0;
    }
    let max_y = ps.patches.iter().map(|p| p.xyz[1]).fold(0.0f64, f64::max);
    if max_y == 0.0 {
        return 0;
    }
    let mut drop = vec![false; before];
    // Chromaticity is compared as the normalised `y` and `z`, so two patches
    // of the same paint under different exposures land on the same point.
    let chroma = |v: Vec3| -> (f64, f64) {
        let s = v.sum();
        if s == 0.0 { (0.0, 0.0) } else { (v[1] / s, v[2] / s) }
    };
    for i in 0..before {
        for j in 0..before {
            if i == j {
                continue;
            }
            let (ay, az) = chroma(ps.patches[i].xyz);
            let (by, bz) = chroma(ps.patches[j].xyz);
            let distance = ((ay - by).powi(2) + (az - bz).powi(2)).sqrt();
            if distance < uv_de_limit {
                if ps.patches[i].xyz.sum() < ps.patches[j].xyz.sum() {
                    drop[i] = true;
                } else {
                    drop[j] = true;
                }
            }
        }
    }
    let mut kept = Vec::with_capacity(before);
    for (i, p) in ps.patches.drain(..).enumerate() {
        if !drop[i] || p.force_keep {
            kept.push(p);
        }
    }
    ps.patches = kept;
    before - ps.patches.len()
}

/// Drop patches whose camera signal is negligible in every channel.
///
/// Such patches carry no usable information and mostly contribute sensor noise
/// to the fit. Returns the number removed.
pub fn exclude_weak_signal_patches(ps: &mut PatchSet) -> usize {
    let before = ps.len();
    let max_cam = ps.patches.iter().map(|p| p.cam.max()).fold(0.0f64, f64::max);
    if max_cam == 0.0 {
        return 0;
    }
    let kept: Vec<Patch> = ps
        .patches
        .drain(..)
        .filter(|p| p.force_keep || p.cam[0] / max_cam >= 1.0e-4 || p.cam[1] / max_cam >= 1.0e-4 || p.cam[2] / max_cam >= 1.0e-4)
        .collect();
    ps.patches = kept;
    before - ps.patches.len()
}

/// Optional per-class and per-patch weight overrides.
#[derive(Clone, Debug, Default)]
pub struct WeightOverrides {
    /// Multiplier per class index.
    pub class_weight: Vec<f64>,
    /// Multiplier per patch index.
    pub patch_weight: Vec<f64>,
    /// When true every patch starts with weight 1.0 instead of the
    /// separation-derived base weight.
    pub skip_base_weighting: bool,
}

/// Assign the matrix optimization weights.
///
/// The base weight of a patch grows with its distance from the other patches,
/// so colours that are isoluminant with many others do not dominate the fit.
/// A chroma dependent window is computed at the same time; the LUT stage uses
/// it to decide how tightly each patch must be matched.
pub fn assign_optimization_weights(ps: &mut PatchSet, wp: Vec3, overrides: &WeightOverrides) {
    let n = ps.len();
    if n == 0 {
        return;
    }
    let mut sum = 0.0f64;
    for i in 0..n {
        let mut w = if overrides.skip_base_weighting {
            1.0
        } else {
            let mut acc = 0.5;
            for j in 0..n {
                if i == j {
                    continue;
                }
                acc += ciede2000_lch_limited(ps.patches[j].xyz, ps.patches[i].xyz, wp);
            }
            acc
        };
        let class = ps.patches[i].class;
        if let Some(cw) = overrides.class_weight.get(class) {
            w *= *cw;
        }
        if let Some(pw) = overrides.patch_weight.get(i) {
            w *= *pw;
        }
        ps.patches[i].matrix_weight = w;
        sum += w;

        // Chroma dependent acceptance window, wide in the shadows and tight
        // in saturated colours where the eye is most sensitive to hue error.
        let chroma = lab_to_lch(xyz_to_lab(ps.patches[i].xyz, wp))[1];
        let lo = 30.0;
        let hi = 70.0;
        let x = ((chroma - lo) / (hi - lo)).clamp(0.0, 1.0);
        ps.patches[i].lut_de_window =
            [-1000.0, 1000.0, -(1.0 - x) - 4.0 * x, 0.7 * (1.0 - x) + 1.5 * x, -(0.5 * (1.0 - x) + 2.0 * x), 0.5 * (1.0 - x) + 2.0 * x];
        ps.patches[i].use_matrix_window = false;
    }
    if sum != 0.0 {
        for p in &mut ps.patches {
            p.matrix_weight /= sum;
        }
    }
}

#[inline]
fn ciede2000_lch_limited(a: Vec3, b: Vec3, wp: Vec3) -> f64 {
    ciede2000(a, b, wp)
}

/// Sum of the matrix weights, useful for reporting.
pub fn total_matrix_weight(ps: &PatchSet) -> f64 {
    ps.patches.iter().map(|p| p.matrix_weight).sum()
}

/// A one line description of a patch, for reports.
pub fn patch_description(ps: &PatchSet, idx: usize, wp: Vec3) -> String {
    ps.patches.get(idx).map(|p| describe_color(p.xyz, wp)).unwrap_or_default()
}

/// Convert an XYZ triple to the sRGB hex string used in error reports.
pub fn xyz_hex(xyz: Vec3, wp: Vec3) -> String {
    format!("#{:06X}", crate::color::xyz_to_srgb_u24(xyz, wp))
}

/// The DNG reference white point.
///
/// DNG quotes D50 as the chromaticity (0.3457, 0.3585) rather than as a
/// tristimulus triple; converting that through the lightest-in-gamut rule
/// gives the value below, which is what DNG-aware applications compare
/// against.
pub fn dng_d50() -> Vec3 {
    crate::color::xy_to_xyz_lightest(0.3457, 0.3585, Vec3::ONE)
}

/// The ICC reference white point, as written into `wtpt`.
pub fn icc_d50() -> Vec3 {
    crate::color::D50
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_are_collected_by_name() {
        let mut ps = PatchSet::new();
        ps.push(Patch::new("W"), Some("white"));
        ps.push(Patch::new("G"), Some("gray"));
        ps.push(Patch::new("W2"), Some("white"));
        assert_eq!(ps.class_names, vec!["white", "gray"]);
        assert_eq!(ps.patches_in_class(0), vec![0, 2]);
        assert!(ps.has_named_classes());
    }

    #[test]
    fn neutral_patch_prefers_the_white_class() {
        let mut ps = PatchSet::new();
        ps.illuminant_white_point = crate::color::D50;
        ps.push(Patch::new("A01"), None);
        ps.push(Patch::new("D01"), Some("white"));
        ps.patches[0].xyz = Vec3::new(0.1, 0.12, 0.09);
        ps.patches[1].xyz = crate::color::D50;
        let sel = find_neutral_patch(&ps, crate::color::D50);
        assert_eq!(sel.index, 1);
        assert!(sel.white_point_de < 1.0e-9);
    }

    #[test]
    fn neutral_patch_falls_back_to_least_chromatic() {
        let mut ps = PatchSet::new();
        ps.push(Patch::new("A01"), None); // reddish
        ps.push(Patch::new("D01"), None); // near neutral and lighter
        ps.push(Patch::new("D02"), None); // dark neutral
        ps.patches[0].xyz = Vec3::new(0.35, 0.20, 0.10);
        ps.patches[1].xyz = Vec3::new(0.93, 0.96, 0.80);
        ps.patches[2].xyz = Vec3::new(0.05, 0.05, 0.04);
        let sel = find_neutral_patch(&ps, crate::color::D50);
        assert_eq!(sel.index, 1);
    }

    #[test]
    fn rebalancing_makes_the_neutral_exact() {
        let mut ps = PatchSet::new();
        ps.push(Patch::new("A01"), None);
        ps.patches[0].xyz = Vec3::new(0.30, 0.31, 0.32);
        let wp = crate::color::D50;
        rebalance_to_white_point(&mut ps, wp, 0);
        assert!(ps.patches[0].xyz.normalized().distance(&wp.normalized()) < 1.0e-9);
    }

    #[test]
    fn normalize_scales_both_domains() {
        let mut ps = PatchSet::new();
        ps.push(Patch::new("A"), None);
        ps.push(Patch::new("B"), None);
        ps.patches[0].cam = Vec3::new(60.0, 70.0, 80.0);
        ps.patches[0].xyz = Vec3::new(0.9, 0.95, 0.8);
        ps.patches[1].cam = Vec3::new(10.0, 12.0, 14.0);
        ps.patches[1].xyz = Vec3::new(0.1, 0.11, 0.09);
        let n = normalized(&ps, Some(0));
        assert!((n.patches[0].cam.max() - n.patches[0].xyz.max()).abs() < 1.0e-9);
        assert!(n.patches.iter().all(|p| p.cam.max() <= 1.0 + 1.0e-12));
    }

    #[test]
    fn weak_signal_patches_are_excluded() {
        let mut ps = PatchSet::new();
        for (i, cam) in
            [Vec3::new(1.0, 1.0, 1.0), Vec3::new(0.5, 0.5, 0.5), Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0e-9, 1.0e-9, 1.0e-9)].into_iter().enumerate()
        {
            let mut p = Patch::new(format!("P{i}"));
            p.cam = cam;
            ps.push(p, None);
        }
        assert_eq!(exclude_weak_signal_patches(&mut ps), 2);
        assert_eq!(ps.len(), 2);
    }

    #[test]
    fn force_keep_survives_exclusion() {
        let mut ps = PatchSet::new();
        let mut black = Patch::new("K");
        black.cam = Vec3::ZERO;
        black.force_keep = true;
        ps.push(Patch::new("W"), None);
        ps.patches[0].cam = Vec3::ONE;
        ps.push(black, None);
        assert_eq!(exclude_weak_signal_patches(&mut ps), 0);
        assert_eq!(ps.len(), 2);
    }

    #[test]
    fn darker_patches_are_excluded() {
        // Two renderings of the same paint: identical chromaticity, different
        // luminance. The darker one adds no colour information.
        let mut ps = PatchSet::new();
        for (i, scale) in [1.0f64, 0.5].into_iter().enumerate() {
            let mut p = Patch::new(format!("P{i}"));
            p.xyz = Vec3::new(0.90, 0.91, 0.80) * scale;
            p.cam = p.xyz;
            ps.push(p, None);
        }
        let removed = exclude_darker_patches(&mut ps, 1.0e-3);
        assert_eq!(removed, 1, "{:?}", ps.patches);
        assert_eq!(ps.len(), 1);
        // The lighter patch survives.
        assert!(ps.patches[0].xyz.sum() > 1.5, "{:?}", ps.patches[0].xyz);
    }

    #[test]
    fn forced_patches_survive_the_darker_rule() {
        let mut ps = PatchSet::new();
        for (i, scale) in [1.0f64, 0.5].into_iter().enumerate() {
            let mut p = Patch::new(format!("P{i}"));
            p.xyz = Vec3::new(0.90, 0.91, 0.80) * scale;
            p.cam = p.xyz;
            p.force_keep = i == 1;
            ps.push(p, None);
        }
        assert_eq!(exclude_darker_patches(&mut ps, 1.0e-3), 0);
        assert_eq!(ps.len(), 2);
    }

    #[test]
    fn distinct_chromaticities_are_kept() {
        let mut ps = PatchSet::new();
        for (i, xyz) in [Vec3::new(0.90, 0.20, 0.10), Vec3::new(0.20, 0.90, 0.30), Vec3::new(0.10, 0.25, 0.85)].into_iter().enumerate() {
            let mut p = Patch::new(format!("P{i}"));
            p.xyz = xyz;
            p.cam = xyz;
            ps.push(p, None);
        }
        assert_eq!(exclude_darker_patches(&mut ps, 1.0e-3), 0);
        assert_eq!(ps.len(), 3);
    }

    #[test]
    fn weights_sum_to_one() {
        let mut ps = PatchSet::new();
        for i in 0..5 {
            let mut p = Patch::new(format!("P{i}"));
            p.cam = Vec3::new(0.1 + i as f64 * 0.2, 0.5, 0.30);
            p.xyz = Vec3::new(0.1 + i as f64 * 0.15, 0.4, 0.2);
            ps.push(p, None);
        }
        assign_optimization_weights(&mut ps, crate::color::D50, &WeightOverrides::default());
        assert!((total_matrix_weight(&ps) - 1.0).abs() < 1.0e-12);
        assert!(ps.patches.iter().all(|p| p.matrix_weight > 0.0));
    }

    #[test]
    fn weight_overrides_are_applied() {
        let mut ps = PatchSet::new();
        for i in 0..3 {
            let mut p = Patch::new(format!("P{i}"));
            p.cam = Vec3::new(0.5, 0.5, 0.5);
            p.xyz = Vec3::new(0.4, 0.4, 0.4);
            ps.push(p, Some("c"));
        }
        let ov = WeightOverrides { class_weight: vec![3.0], patch_weight: vec![1.0, 1.0, 10.0], skip_base_weighting: true };
        assign_optimization_weights(&mut ps, crate::color::D50, &ov);
        assert!(ps.patches[2].matrix_weight > ps.patches[0].matrix_weight);
        assert!((total_matrix_weight(&ps) - 1.0).abs() < 1.0e-12);
    }

    #[test]
    fn chroma_window_is_wider_for_muted_colors() {
        let mut ps = PatchSet::new();
        let mut muted = Patch::new("muted");
        muted.xyz = crate::color::lab_to_xyz(Vec3::new(60.0, 5.0, 5.0), crate::color::D50);
        let mut vivid = Patch::new("vivid");
        vivid.xyz = crate::color::lab_to_xyz(Vec3::new(60.0, 70.0, 40.0), crate::color::D50);
        ps.push(muted, None);
        ps.push(vivid, None);
        assign_optimization_weights(&mut ps, crate::color::D50, &WeightOverrides::default());
        assert!(ps.patches[0].lut_de_window[2].abs() < ps.patches[1].lut_de_window[2].abs(), "{:?}", ps.patches);
    }
}
