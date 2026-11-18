// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/evaluate.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Profile evaluation: how closely a profile reproduces measured colours.
//!
//! Evaluation is always relative to a measurement target. For every patch the
//! module compares the reference colour with the colour the profile predicts,
//! and reports the overall CIEDE2000 difference plus its decomposition into a
//! lightness, a chroma and a hue component. Summary statistics (mean, median,
//! 90th percentile, worst) are what tell you whether an error is a systematic
//! shift or a few bad patches.

use crate::color::{ciede2000, ciède2000_components, describe_color, xyz_to_srgb_u24};
use crate::linalg::{Mat3, Vec3};
use crate::lut::ChromaTable;
use crate::profile::Profile;
use crate::target::PatchSet;
use core::fmt;

/// Which rendering path was evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupType {
    /// The linear colour matrix only.
    Matrix,
    /// The engine's own chromaticity correction table.
    NativeLut,
    /// A DNG HueSatMap.
    DcpHueSatMap,
    /// An ICC CLUT.
    IccLut,
}

impl LookupType {
    /// Human readable name, as used in report headings.
    pub fn label(self) -> &'static str {
        match self {
            Self::Matrix => "Matrix",
            Self::NativeLut => "Native LUT",
            Self::DcpHueSatMap => "DCP HSM LUT",
            Self::IccLut => "ICC LUT",
        }
    }

    /// Padding that aligns the summary lines under the label.
    pub fn pad(self) -> &'static str {
        match self {
            Self::Matrix => "      ",
            Self::NativeLut => "          ",
            Self::DcpHueSatMap => "           ",
            Self::IccLut => "       ",
        }
    }
}

/// Per-patch result.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchError {
    /// Index of the patch in the evaluated target.
    pub index: usize,
    /// The colour the profile predicted.
    pub predicted_xyz: Vec3,
    /// `[overall, lightness, chroma, hue]` differences, rounded to 0.001.
    pub errors: [f64; 4],
}

/// Aggregate statistics for one error dimension.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ErrorStats {
    /// Arithmetic mean of the absolute differences.
    pub mean: f64,
    /// Median absolute difference.
    pub median: f64,
    /// 90th percentile absolute difference.
    pub p90: f64,
    /// Largest absolute difference.
    pub max: f64,
    /// Indices of the worst patches, worst first.
    pub worst: Vec<usize>,
    /// Indices of the best patches, best first.
    pub best: Vec<usize>,
}

/// The result of an evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct Evaluation {
    /// Rendering path that was evaluated.
    pub lookup_type: LookupType,
    /// Reference white point used for the differences.
    pub white_point: Vec3,
    /// One entry per patch.
    pub patches: Vec<PatchError>,
    /// Statistics for `[overall, lightness, chroma, hue]`.
    pub stats: [ErrorStats; 4],
}

/// Names of the four error dimensions, for reports.
pub const ERROR_DIMENSIONS: [&str; 4] = ["Overall DE", "Lightness DE", "Chroma DE", "Hue DE"];

impl Evaluation {
    /// Number of patches evaluated.
    pub fn patch_count(&self) -> usize {
        self.patches.len()
    }

    /// Mean overall difference.
    pub fn mean_de(&self) -> f64 {
        self.stats[0].mean
    }

    /// Largest overall difference.
    pub fn max_de(&self) -> f64 {
        self.stats[0].max
    }

    /// The patch with the largest overall difference.
    pub fn worst_patch(&self) -> Option<&PatchError> {
        self.stats[0].worst.first().and_then(|i| {
            self.patches.iter().find(|p| p.index == *i && p.errors[0].abs() > 0.0).or_else(|| self.patches.iter().find(|p| p.index == *i))
        })
    }

    /// One line summary.
    pub fn summary(&self) -> String {
        format!(
            "{}: mean DE {:.2}, median {:.2}, p90 {:.2}, max {:.2}",
            self.lookup_type.label(),
            self.stats[0].mean,
            self.stats[0].median,
            self.stats[0].p90,
            self.stats[0].max
        )
    }

    /// Render a full text report.
    pub fn report(&self, ps: &PatchSet, per_patch: bool) -> String {
        let mut out = String::new();
        let name = self.lookup_type.label();
        let _ = fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "{} patch match average DE {:.2}, DE LCh {:.2} {:.2} {:.2}\n",
                name, self.stats[0].mean, self.stats[1].mean, self.stats[2].mean, self.stats[3].mean
            ),
        );
        let pad = self.lookup_type.pad();
        for (label, idx) in [("median", 1usize), ("p90", 2), ("max", 3)] {
            let _ = fmt::Write::write_fmt(
                &mut out,
                format_args!(
                    "{}{:>7} DE {:.2}, DE LCh {:.2} {:.2} {:.2}\n",
                    pad, label, self.stats[0][idx], self.stats[1][idx], self.stats[2][idx], self.stats[3][idx],
                ),
            );
        }
        for (dim, label) in ERROR_DIMENSIONS.iter().enumerate() {
            let stats = &self.stats[dim];
            let _ = fmt::Write::write_fmt(&mut out, format_args!("{} worst patches for {label}:\n", stats.worst.len().min(5)));
            for idx in stats.worst.iter().take(5) {
                if let Some(e) = self.patches.iter().find(|p| p.index == *idx) {
                    let _ = fmt::Write::write_fmt(&mut out, format_args!("{}\n", self.patch_line(ps, e)));
                }
            }
        }
        if per_patch {
            out.push('\n');
            for e in &self.patches {
                let _ = fmt::Write::write_fmt(&mut out, format_args!("{}\n", self.patch_line(ps, e)));
            }
        }
        out
    }

    fn patch_line(&self, ps: &PatchSet, e: &PatchError) -> String {
        let cam = ps.patches.get(e.index).map(|p| p.cam).unwrap_or(Vec3::ZERO);
        let reference = ps.patches.get(e.index).map(|p| p.xyz).unwrap_or(Vec3::ZERO);
        format!(
            "{:>5} RGB {:.3} {:.3} {:.3} XYZref {:.3} {:.3} {:.3} XYZcam {:.3} {:.3} {:.3} \
             sRGB #{:06X} #{:06X} DE {:.2} DE LCh {:+.2} {:+.2} {:+.2} ({})",
            ps.patch_name(e.index),
            cam[0],
            cam[1],
            cam[2],
            reference[0],
            reference[1],
            reference[2],
            e.predicted_xyz[0],
            e.predicted_xyz[1],
            e.predicted_xyz[2],
            xyz_to_srgb_u24(reference, self.white_point),
            xyz_to_srgb_u24(e.predicted_xyz, self.white_point),
            e.errors[0],
            e.errors[1],
            e.errors[2],
            e.errors[3],
            describe_color(reference, self.white_point),
        )
    }
}

impl core::ops::Index<usize> for ErrorStats {
    type Output = f64;
    fn index(&self, i: usize) -> &f64 {
        match i {
            0 => &self.mean,
            1 => &self.median,
            2 => &self.p90,
            _ => &self.max,
        }
    }
}

/// Evaluate a set of predictions against a target.
///
/// `predict` receives the camera signal of a patch and returns the colour the
/// profile claims it produces.
pub fn evaluate_with<F>(ps: &PatchSet, white_point: Vec3, lookup_type: LookupType, mut predict: F) -> Evaluation
where
    F: FnMut(usize, Vec3) -> Vec3,
{
    let mut patches = Vec::with_capacity(ps.len());
    for (i, p) in ps.patches.iter().enumerate() {
        let predicted = predict(i, p.cam);
        let (overall, lightness, chroma, hue) = ciède2000_components(p.xyz, predicted, white_point);
        let mut errors = [overall, lightness, chroma, hue];
        for e in &mut errors {
            // Rounded to a thousandth, which is the precision reports quote.
            *e = (*e * 1000.0).round() / 1000.0;
            if *e == 0.0 {
                *e = 0.0; // normalise -0.0
            }
        }
        patches.push(PatchError { index: i, predicted_xyz: predicted, errors });
    }
    let stats = compute_stats(&patches);
    Evaluation { lookup_type, white_point, patches, stats }
}

/// The transform from the profile connection space (D50) to the illuminant a
/// target's reference colours are stated under.
///
/// A profile renders into D50, while a target's reference colours are XYZ
/// under the light it was measured in; comparing the two directly would count
/// the illuminant difference as error.
fn pcs_to_target(prof: &Profile) -> Mat3 {
    crate::color::bradford_map_white(crate::color::D50, prof.illuminant_white_point)
}

/// Evaluate a fitted profile against a target whose reference colours are
/// under the profile's calibration illuminant.
pub fn evaluate_profile(prof: &Profile, ps: &PatchSet) -> Evaluation {
    let lookup_type = if prof.lut.is_some() { LookupType::NativeLut } else { LookupType::Matrix };
    let wp = prof.illuminant_white_point;
    let to_target = pcs_to_target(prof);
    evaluate_with(ps, wp, lookup_type, |_, cam| to_target * prof.render(cam))
}

/// Evaluate the profile's colour matrix path only, ignoring the correction
/// table.
pub fn evaluate_matrix_only(prof: &Profile, ps: &PatchSet) -> Evaluation {
    let wp = prof.illuminant_white_point;
    let to_target = pcs_to_target(prof);
    evaluate_with(ps, wp, LookupType::Matrix, |_, cam| to_target * prof.render_matrix_only(cam))
}

/// Evaluate a bare correction table applied on top of a matrix.
pub fn evaluate_table(cam2xyz: Mat3, table: &ChromaTable, ps: &PatchSet, wp: Vec3) -> Evaluation {
    evaluate_with(ps, wp, LookupType::NativeLut, |_, cam| table.correct_xyz(cam2xyz * cam))
}

/// Compute the summary statistics of a set of patch errors.
fn compute_stats(patches: &[PatchError]) -> [ErrorStats; 4] {
    let mut stats: [ErrorStats; 4] = Default::default();
    if patches.is_empty() {
        return stats;
    }
    for dim in 0..4 {
        let mut sorted: Vec<(f64, usize)> = patches.iter().map(|p| (p.errors[dim].abs(), p.index)).collect();
        sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
        let n = sorted.len();
        let sum: f64 = sorted.iter().map(|(v, _)| *v).sum();
        stats[dim].mean = sum / n as f64;
        stats[dim].median = sorted[n / 2].0;
        stats[dim].p90 = sorted[(9 * n) / 10].0;
        stats[dim].max = sorted[n - 1].0;
        stats[dim].worst = sorted.iter().rev().take(5).map(|(_, i)| *i).collect();
        stats[dim].best = sorted.iter().take(5).map(|(_, i)| *i).collect();
    }
    stats
}

/// Colour difference of a single patch rendered through the profile, with the
/// reference given under the profile's calibration illuminant.
pub fn patch_de(prof: &Profile, reference: Vec3, cam: Vec3) -> f64 {
    ciede2000(reference, pcs_to_target(prof) * prof.render(cam), prof.illuminant_white_point)
}

/// True when the evaluation is within a typical acceptance threshold.
///
/// A mean under 2 DE and a maximum under 6 DE is the bar most camera
/// profiling workflows use for a profile that behaves well in practice.
pub fn is_acceptable(evaluation: &Evaluation) -> bool {
    evaluation.mean_de() < 2.0 && evaluation.max_de() < 6.0
}

/// One row of a per-patch report, ready for display.
///
/// A user interface wants the camera signal, the reference colour and the
/// predicted colour side by side, with the error decomposition alongside it.
/// This gathers all of it, so a front end does not have to reach into the
/// profile and the evaluation separately and keep the two in step.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchRow {
    /// Patch identifier, or its index when the measurement had no name.
    pub id: String,
    /// Camera signal the patch was measured at.
    pub camera: Vec3,
    /// Reference colour from the measurement.
    pub reference: Vec3,
    /// Colour the profile predicts for `camera`.
    pub predicted: Vec3,
    /// `[overall, lightness, chroma, hue]` differences.
    pub errors: [f64; 4],
    /// `#RRGGBB` of the reference colour.
    pub reference_hex: String,
    /// `#RRGGBB` of the predicted colour.
    pub predicted_hex: String,
}

impl Profile {
    /// Per-patch rows describing how this profile renders its own target.
    ///
    /// Empty when the profile was loaded from a file rather than fitted, since a
    /// stored profile carries no target to compare against.
    pub fn patch_rows(&self) -> Vec<PatchRow> {
        // The fitted target is stored in the connection space, as is what the
        // profile renders.
        let wp = crate::color::D50;
        self.camera_rgb
            .iter()
            .zip(self.reference_xyz.iter())
            .enumerate()
            .map(|(i, (cam, reference))| {
                let predicted = self.render(*cam);
                let (overall, lightness, chroma, hue) = ciède2000_components(*reference, predicted, wp);
                // Reported to a thousandth, with negative zero normalised away.
                let round = |v: f64| {
                    let r = (v * 1000.0).round() / 1000.0;
                    if r == 0.0 { 0.0 } else { r }
                };
                PatchRow {
                    id: self.patch_ids.get(i).filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| format!("{}", i + 1)),
                    camera: *cam,
                    reference: *reference,
                    predicted,
                    errors: [round(overall), round(lightness), round(chroma), round(hue)],
                    reference_hex: format!("#{:06X}", xyz_to_srgb_u24(*reference, wp)),
                    predicted_hex: format!("#{:06X}", xyz_to_srgb_u24(predicted, wp)),
                }
            })
            .collect()
    }
}
/// Group an evaluation by patch class, returning one evaluation per class.
///
/// Patches that were never classified are reported together under the
/// `"unclassified"` name, so every patch appears in exactly one group.
pub fn by_class(prof: &Profile, ps: &PatchSet) -> Vec<(String, Evaluation)> {
    let lookup = if prof.lut.is_some() { LookupType::NativeLut } else { LookupType::Matrix };
    let wp = prof.illuminant_white_point;
    let to_target = pcs_to_target(prof);

    let mut groups: Vec<(String, Vec<usize>)> =
        ps.class_names.iter().enumerate().map(|(class, name)| (name.clone(), ps.patches_in_class(class))).collect();
    let unclassified: Vec<usize> = (0..ps.len()).filter(|i| ps.patches[*i].class_index().is_none()).collect();
    if !unclassified.is_empty() {
        groups.push(("unclassified".to_string(), unclassified));
    }

    let mut out = Vec::new();
    for (name, indices) in groups {
        if indices.is_empty() {
            continue;
        }
        let mut subset = PatchSet::new();
        subset.illuminant_name = ps.illuminant_name.clone();
        subset.illuminant_white_point = ps.illuminant_white_point;
        for i in indices {
            subset.patches.push(ps.patches[i].clone());
        }
        let eval = evaluate_with(&subset, wp, lookup, |_, cam| to_target * prof.render(cam));
        out.push((name, eval));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{self, FitOptions};
    use crate::target::{Patch, PatchSet};

    fn target(count: usize) -> PatchSet {
        let cam2xyz = Mat3::from_rows([0.62, 0.28, 0.09], [0.21, 0.72, 0.14], [0.05, 0.16, 0.79]);
        let xyz2cam = cam2xyz.inverted().unwrap();
        let mut ps = PatchSet::new();
        ps.illuminant_name = "D50".into();
        ps.illuminant_white_point = crate::color::D50;
        for i in 0..count {
            let t = i as f64 / (count - 1) as f64;
            let xyz = Vec3::new(0.12 + 0.80 * t, 0.10 + 0.85 * ((t * 1.7) % 1.0), 0.06 + 0.75 * ((t * 2.3) % 1.0));
            let mut p = Patch::new(format!("P{i:02}"));
            p.cam = xyz2cam * xyz;
            p.xyz = xyz;
            ps.push(p, None);
        }
        let mut white = Patch::new("W01");
        white.xyz = crate::color::D50 * 0.95;
        white.cam = xyz2cam * white.xyz;
        ps.push(white, Some("white"));
        ps
    }

    fn fitted(count: usize) -> (Profile, PatchSet) {
        let ps = target(count);
        let (prof, _) = profile::fit(&ps, &FitOptions::default(), &mut |_| {}).unwrap();
        // The fit normalises its working copy, so evaluate against the target
        // scaled the same way.
        (prof, ps)
    }

    #[test]
    fn perfect_prediction_has_zero_error() {
        let ps = target(10);
        let eval = evaluate_with(&ps, crate::color::D50, LookupType::Matrix, |i, _| ps.patches[i].xyz);
        assert_eq!(eval.mean_de(), 0.0);
        assert_eq!(eval.max_de(), 0.0);
    }

    #[test]
    fn statistics_are_ordered_correctly() {
        let ps = target(20);
        // Add a growing error so the ordering is known.
        let eval = evaluate_with(&ps, crate::color::D50, LookupType::Matrix, |i, _| {
            let lab = crate::color::xyz_to_lab(ps.patches[i].xyz, crate::color::D50);
            crate::color::lab_to_xyz(lab + Vec3::new(i as f64 * 0.3, 0.0, 0.0), crate::color::D50)
        });
        for dim in 0..4 {
            let s = &eval.stats[dim];
            assert!(s.median <= s.p90 + 1.0e-9, "{s:?}");
            assert!(s.p90 <= s.max + 1.0e-9, "{s:?}");
            assert!(!s.worst.is_empty());
            assert!(!s.best.is_empty());
        }
    }

    #[test]
    fn worst_patch_is_the_largest_error() {
        let ps = target(12);
        let eval = evaluate_with(&ps, crate::color::D50, LookupType::Matrix, |i, _| {
            if i == 5 {
                let lab = crate::color::xyz_to_lab(ps.patches[i].xyz, crate::color::D50);
                crate::color::lab_to_xyz(lab + Vec3::new(10.0, 0.0, 0.0), crate::color::D50)
            } else {
                ps.patches[i].xyz
            }
        });
        assert_eq!(eval.worst_patch().map(|p| p.index), Some(5));
    }

    #[test]
    fn evaluation_of_a_fitted_profile_is_accurate() {
        let (prof, ps) = fitted(40);
        let eval = evaluate_profile(&prof, &ps);
        // A profile is defined in the units of the measurement it was fitted
        // from, so it must reproduce that measurement directly.
        assert!(eval.mean_de() < 3.0, "{}", eval.mean_de());
        assert!(eval.max_de() < 12.0, "{}", eval.max_de());
        // Converting the target to another unit system must not change the
        // profile's accuracy, so both evaluations agree after the conversion.
        // (The residual difference is the lightness term, which does depend on
        // the absolute level.)
        let scaled = crate::target::normalized(&ps, Some(crate::target::find_neutral_patch(&ps, crate::color::D50).index));
        let eval_scaled = evaluate_profile(&prof, &scaled);
        assert!((eval.mean_de() - eval_scaled.mean_de()).abs() < 1.0, "{} vs {}", eval.mean_de(), eval_scaled.mean_de());
        // the matrix alone is no better than the matrix with its table
        let matrix_only = evaluate_matrix_only(&prof, &ps);
        assert!(matrix_only.mean_de() >= eval.mean_de() - 1.0e-9, "matrix only {} vs {}", matrix_only.mean_de(), eval.mean_de());
    }

    #[test]
    fn acceptance_threshold_uses_mean_and_max() {
        let ps = target(10);
        let perfect = evaluate_with(&ps, crate::color::D50, LookupType::Matrix, |i, _| ps.patches[i].xyz);
        assert!(is_acceptable(&perfect));
        let bad = evaluate_with(&ps, crate::color::D50, LookupType::Matrix, |i, _| {
            let lab = crate::color::xyz_to_lab(ps.patches[i].xyz, crate::color::D50);
            crate::color::lab_to_xyz(lab + Vec3::new(20.0, 20.0, 0.0), crate::color::D50)
        });
        assert!(!is_acceptable(&bad));
    }

    #[test]
    fn per_class_grouping_covers_every_class() {
        let (prof, ps) = fitted(20);
        let groups = by_class(&prof, &ps);
        assert!(!groups.is_empty());
        for (name, eval) in &groups {
            assert!(!name.is_empty());
            assert!(eval.patch_count() > 0);
        }
        // The fixture classifies exactly one patch as "white"; everything else
        // is unclassified, and every patch must be covered exactly once.
        let total: usize = groups.iter().map(|(_, e)| e.patch_count()).sum();
        let names: Vec<&str> = groups.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(total, ps.len(), "{names:?}");
        assert!(names.contains(&"white"), "{names:?}");
        assert!(names.contains(&"unclassified"), "{names:?}");
    }

    #[test]
    fn patch_de_matches_the_evaluation() {
        let (prof, ps) = fitted(16);
        let eval = evaluate_profile(&prof, &ps);
        for p in &eval.patches {
            let reference = ps.patches[p.index].xyz;
            let cam = ps.patches[p.index].cam;
            let de = patch_de(&prof, reference, cam);
            assert!((de - p.errors[0]).abs() < 5.0e-3, "{} vs {}", de, p.errors[0]);
        }
    }

    #[test]
    fn empty_target_evaluates_to_zero_statistics() {
        let ps = PatchSet::new();
        let eval = evaluate_with(&ps, crate::color::D50, LookupType::Matrix, |_, _| Vec3::ZERO);
        assert_eq!(eval.patch_count(), 0);
        assert_eq!(eval.mean_de(), 0.0);
        assert!(eval.worst_patch().is_none());
    }
}
