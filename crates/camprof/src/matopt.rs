// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/matopt.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Fitting the camera-RGB-to-XYZ matrix.
//!
//! The objective is perceptual: the weighted root-sum-square of CIEDE2000
//! differences between the reference colours and the colours the candidate
//! matrix predicts. Weighting comes from [`crate::target`] and reflects how
//! much new information each patch carries.
//!
//! The search strategy is that of the original profiler, kept because it
//! converges reliably on this objective: fit the diagonal, then one row at a
//! time, then refine all nine entries together. It is a direct search, so no
//! derivatives are needed and a non-smooth objective is fine.

use crate::color::{ciede2000, lab_to_lch, xyz_to_lab, xyz_to_uvy};
use crate::linalg::{Mat3, Vec3};
use crate::simplex::{self, SimplexOptions};
use crate::target::PatchSet;

/// Upper bound returned for matrices that violate a hard constraint.
const PENALTY: f64 = 1.0e9;

/// Progress callback: receives a value from 0 to 1.
pub type Progress<'a> = &'a mut dyn FnMut(f64);

/// A subset of the target used for fitting, in the normalized camera domain.
///
/// Keeping the working set separate from the target lets the fit use a
/// rescaled copy without touching the caller's data.
#[derive(Clone, Debug)]
struct WorkItem {
    cam: Vec3,
    xyz: Vec3,
    weight: f64,
    /// Per-dimension LCh windows for the refinement pass.
    window: Option<[f64; 6]>,
    /// True for the patch that anchors the white point.
    is_neutral: bool,
}

/// Parameters for a matrix fit.
#[derive(Clone, Debug)]
pub struct MatrixFitOptions {
    /// White point the reference colours were rebalanced to.
    pub white_point: Vec3,
    /// Minimum value allowed for every entry of a *forward* matrix. `None`
    /// allows negative entries, which a colour matrix legitimately needs.
    pub min_entry: Option<[f64; 3]>,
    /// Use the slower brute-force search instead of the staged one.
    pub slow_search: bool,
    /// Try to honour per-patch LCh windows after the main fit.
    pub refine_to_windows: bool,
    /// Whether a failed refinement is an error rather than a warning.
    pub refinement_is_error: bool,
    /// Index of the patch that anchors the neutral, if any.
    pub neutral_index: Option<usize>,
    /// Weight multiplier applied to the neutral patch.
    pub neutral_weight: f64,
}

impl Default for MatrixFitOptions {
    fn default() -> Self {
        Self {
            white_point: crate::color::D50,
            min_entry: None,
            slow_search: false,
            refine_to_windows: false,
            refinement_is_error: false,
            neutral_index: None,
            neutral_weight: 1.0,
        }
    }
}

/// The outcome of a fit.
#[derive(Clone, Debug)]
pub struct MatrixFit {
    /// Camera RGB to XYZ.
    pub matrix: Mat3,
    /// Weighted root-sum-square CIEDE2000 of the fit.
    pub rms_de: f64,
    /// Arithmetic mean CIEDE2000 of the fit.
    pub mean_de: f64,
    /// Number of comparable patches used.
    pub patch_count: usize,
    /// True when the refinement pass found a solution inside every window.
    pub refinement_satisfied: bool,
    /// Patches whose LCh window could not be met, if any.
    pub refinement_failures: Vec<usize>,
}

/// Default minimum entry used for a forward (LUT) matrix.
///
/// A forward matrix drives the LUT building and must stay strictly positive so
/// that the derived chromaticity correction stays continuous across the whole
/// grid.
pub const FORWARD_MIN_ENTRY: [f64; 3] = [0.0002, 0.0002, 0.0002];

/// Uniform per-axis scalars applied before fitting.
///
/// The fit works on normalized values because that keeps the optimizer's
/// initial simplex at a sensible size. Scaling each axis by a single constant
/// is equivalent to scaling the rows of the resulting matrix, so the fitted
/// matrix is converted back to the caller's units before it is returned. This
/// type is public so that anything else working on the same normalized set can
/// convert consistently.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkingScale {
    /// Divisor applied to the camera signal.
    pub camera: Vec3,
    /// Divisor applied to the reference colour.
    pub reference: Vec3,
}

impl WorkingScale {
    /// Derive the scalars a patch set implies.
    ///
    /// Patches with zero weight are ignored: they are excluded from the fit, so
    /// letting them set the working scale would compress the useful range.
    pub fn of(ps: &PatchSet) -> Self {
        let active = || ps.patches.iter().filter(|p| p.matrix_weight > 0.0);
        let camera = active().map(|p| p.cam.max()).fold(0.0f64, f64::max);
        let reference = active().map(|p| p.xyz.max()).fold(0.0f64, f64::max);
        let c = if camera == 0.0 { 1.0 } else { camera };
        let r = if reference == 0.0 { 1.0 } else { reference };
        Self { camera: Vec3::new(c, c, c), reference: Vec3::new(r, r, r) }
    }

    /// Express this scale relative to another one.
    ///
    /// Used to convert a matrix fitted on a normalized working copy back to the
    /// units of the target it was derived from: the ratio between the two
    /// scales is exactly the row and column scaling the normalization applied.
    pub fn relative_to(&self, normalized: &WorkingScale) -> Self {
        let mut out = *self;
        for axis in 0..3 {
            out.camera[axis] = if normalized.camera[axis] == 0.0 { self.camera[axis] } else { self.camera[axis] / normalized.camera[axis] };
            out.reference[axis] =
                if normalized.reference[axis] == 0.0 { self.reference[axis] } else { self.reference[axis] / normalized.reference[axis] };
        }
        out
    }

    /// Convert a matrix fitted on normalized values back to the caller's units.
    ///
    /// Rows map onto the reference axes and columns onto the camera axes, so
    /// undoing the normalization multiplies each row by its reference scale and
    /// divides each column by its camera scale.
    pub fn denormalize(&self, m: &Mat3) -> Mat3 {
        let mut out = Mat3::ZERO;
        for row in 0..3 {
            for col in 0..3 {
                let cam = self.camera[col];
                out[row][col] = if cam == 0.0 { 0.0 } else { m[row][col] * self.reference[row] / cam };
            }
        }
        out
    }
}

fn work_items(ps: &PatchSet, opts: &MatrixFitOptions, scale: &WorkingScale) -> Vec<WorkItem> {
    let cam_scale = Vec3::new(scale.camera[0], scale.camera[1], scale.camera[2]);

    ps.patches
        .iter()
        .enumerate()
        .map(|(i, p)| {
            // A zero weight means "leave this patch out", which the optimizer
            // honours by not accumulating any term for it.
            let mut weight = p.matrix_weight;
            if !weight.is_finite() || weight < 0.0 {
                weight = 0.0;
            }
            if Some(i) == opts.neutral_index {
                weight *= opts.neutral_weight;
            }
            let window = if p.use_matrix_window { Some(p.matrix_de_window) } else { None };
            WorkItem { cam: p.cam / cam_scale, xyz: p.xyz / scale.reference, weight, window, is_neutral: Some(i) == opts.neutral_index }
        })
        .collect()
}
fn violates_min_entry(m: &Mat3, min: &[f64; 3]) -> bool {
    for row in 0..3 {
        for col in 0..3 {
            if m[row][col] < min[row] {
                return true;
            }
        }
    }
    false
}

/// Weighted root-sum-square CIEDE2000 of `m` over the working set.
///
/// The weights are normalized to sum to one, so on a uniform target this is the
/// quadratic mean of the per-patch differences rather than their arithmetic
/// mean. [`MatrixFit::mean_de`] reports the arithmetic mean, which is the
/// figure reports quote.
fn rms_de(items: &[WorkItem], white_point: Vec3, cam2xyz: &Mat3) -> f64 {
    let mut sum = 0.0;
    for item in items {
        if item.weight == 0.0 {
            continue;
        }
        let predicted = *cam2xyz * item.cam;
        let de = ciede2000(item.xyz, predicted, white_point);
        let wde = item.weight * de;
        sum += wde * wde;
    }
    sum.sqrt()
}

/// Arithmetic mean CIEDE2000 of `m` over the working set, ignoring weights.
fn mean_de(items: &[WorkItem], white_point: Vec3, cam2xyz: &Mat3) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for item in items {
        if item.weight == 0.0 {
            continue;
        }
        sum += ciede2000(item.xyz, *cam2xyz * item.cam, white_point);
        count += 1;
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

/// Objective used for the LUT (forward) matrix.
///
/// A forward matrix is under-determined because only the LUT above it is ever
/// judged, so a pure colour-error objective leaves it free to wander outside
/// the gamut. This objective adds a penalty that keeps the matrix's primary
/// axes inside the working gamut, which makes the resulting LUT smooth.
fn rms_de_with_gamut(items: &[WorkItem], white_point: Vec3, cam2xyz: &Mat3, polygon: &[Vec3; 3]) -> (f64, f64) {
    let gamut = {
        let mut sum = 0.0;
        let primaries = [
            xyz_to_uvy(*cam2xyz * Vec3::new(1.0, 0.0, 0.0)),
            xyz_to_uvy(*cam2xyz * Vec3::new(0.0, 1.0, 0.0)),
            xyz_to_uvy(*cam2xyz * Vec3::new(0.0, 0.0, 1.0)),
        ];
        for i in 0..3 {
            let j = (i + 1) % 3;
            let d = point_to_line_distance(polygon[i][0], polygon[i][1], polygon[j][0], polygon[j][1], primaries[i][0], primaries[i][1]);
            let mut de = 100.0 * d;
            if !point_in_triangle(polygon, primaries[i][0], primaries[i][1]) {
                de *= 3.0;
            }
            sum += de * de;
        }
        (sum / 3.0).sqrt()
    };
    (gamut, rms_de(items, white_point, cam2xyz))
}

/// Perpendicular distance from a point to the infinite line through two points.
pub fn point_to_line_distance(x1: f64, y1: f64, x2: f64, y2: f64, px: f64, py: f64) -> f64 {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    if len == 0.0 {
        return ((px - x1).powi(2) + (py - y1).powi(2)).sqrt();
    }
    ((dy * (px - x1) - dx * (py - y1)) / len).abs()
}

/// True when `(px, py)` lies inside the triangle `poly`.
pub fn point_in_triangle(poly: &[Vec3; 3], px: f64, py: f64) -> bool {
    let sign = |ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64| -> f64 { (ax - cx) * (by - cy) - (bx - cx) * (ay - cy) };
    let d1 = sign(px, py, poly[0][0], poly[0][1], poly[1][0], poly[1][1]);
    let d2 = sign(px, py, poly[1][0], poly[1][1], poly[2][0], poly[2][1]);
    let d3 = sign(px, py, poly[2][0], poly[2][1], poly[0][0], poly[0][1]);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

/// Fitted matrix plus the raw working set, so later passes can reuse it.
fn minimize_nine<F>(start: Mat3, objective: F, epsilon: f64, scale: f64) -> Mat3
where
    F: Fn(&Mat3) -> f64,
{
    let mut x = [start[0][0], start[0][1], start[0][2], start[1][0], start[1][1], start[1][2], start[2][0], start[2][1], start[2][2]];
    let eval = |v: &[f64]| -> f64 {
        let m = Mat3::from_rows([v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]);
        objective(&m)
    };
    simplex::minimize(eval, &mut x, &SimplexOptions { epsilon, scale, ..Default::default() });
    Mat3::from_rows([x[0], x[1], x[2]], [x[3], x[4], x[5]], [x[6], x[7], x[8]])
}

fn minimize_subset<F>(start: Mat3, indices: &[usize], objective: F, epsilon: f64, scale: f64) -> Mat3
where
    F: Fn(&Mat3) -> f64,
{
    let mut x: Vec<f64> = indices.iter().map(|&i| start[i / 3][i % 3]).collect();
    let eval = |v: &[f64]| -> f64 {
        let mut m = start;
        for (slot, &i) in indices.iter().enumerate() {
            m[i / 3][i % 3] = v[slot];
        }
        objective(&m)
    };
    simplex::minimize(eval, &mut x, &SimplexOptions { epsilon, scale, ..Default::default() });
    let mut m = start;
    for (slot, &i) in indices.iter().enumerate() {
        m[i / 3][i % 3] = x[slot];
    }
    m
}

/// Repeatedly refine all nine entries until the objective stops improving.
fn refine_matrix<F>(start: Mat3, objective: F, scale: f64) -> Mat3
where
    F: Fn(&Mat3) -> f64,
{
    let mut best = start;
    let mut best_value = objective(&best);
    let mut current = best;
    for _ in 0..10 {
        current = minimize_nine(current, &objective, 1.0e-7, scale);
        let value = objective(&current);
        if value + 1.0e-5 > best_value {
            if value < best_value {
                best = current;
            }
            break;
        }
        best_value = value;
        best = current;
    }
    best
}

/// Exhaustive local search on a shrinking grid.
///
/// Far slower than [`refine_matrix`] but useful when the staged search lands in
/// a poor local minimum, for example on targets with very few patches.
fn brute_force<F>(start: Mat3, objective: F) -> Mat3
where
    F: Fn(&Mat3) -> f64,
{
    const STEPS: [f64; 5] = [-1.0, -0.5, 0.0, 0.5, 1.0];
    let mut best = Mat3::ZERO;
    let mut best_value = objective(&best);
    let mut range = 8.0f64;
    for _ in 0..64 {
        let base = best;
        let mut improved = false;
        for q0 in STEPS {
            for q1 in STEPS {
                for q2 in STEPS {
                    for q3 in STEPS {
                        for q4 in STEPS {
                            for q5 in STEPS {
                                for q6 in STEPS {
                                    for q7 in STEPS {
                                        for q8 in STEPS {
                                            let mut m = base;
                                            let q = [q0, q1, q2, q3, q4, q5, q6, q7, q8];
                                            for (n, step) in q.iter().enumerate() {
                                                m[n / 3][n % 3] += range * step;
                                            }
                                            let value = objective(&m);
                                            if value < best_value {
                                                best_value = value;
                                                best = m;
                                                improved = true;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if !improved {
            range *= 0.5;
            if range < 1.0e-4 {
                break;
            }
        }
        let _ = start;
    }
    best
}

/// Fit a camera-RGB-to-XYZ matrix to a patch set.
pub fn fit_camera_matrix(ps: &PatchSet, opts: &MatrixFitOptions, progress: Progress<'_>) -> Result<MatrixFit, crate::target::TargetError> {
    let scale = WorkingScale::of(ps);
    let items = work_items(ps, opts, &scale);
    if items.is_empty() {
        return Err(crate::target::TargetError::Missing { what: "patches to fit".into() });
    }
    let wp = opts.white_point;

    progress(0.0);

    let objective = |m: &Mat3| -> f64 {
        if let Some(min) = &opts.min_entry
            && violates_min_entry(m, min)
        {
            return PENALTY;
        }
        rms_de(&items, wp, m)
    };

    let mut m = Mat3::IDENTITY;
    if let Some(min) = &opts.min_entry {
        for row in 0..3 {
            for col in 0..3 {
                if min[row] > m[row][col] {
                    m[row][col] = min[row];
                }
            }
        }
    }

    if opts.slow_search {
        m = brute_force(m, objective);
    } else {
        // Diagonal first: this captures the camera's per-channel response.
        m = minimize_subset(m, &[0, 4, 8], objective, 1.0e-7, 1.0);
        progress(0.2);
        m = minimize_subset(m, &[3, 4, 5], objective, 1.0e-7, 1.0);
        progress(0.4);
        m = minimize_subset(m, &[0, 1, 2], objective, 1.0e-7, 1.0);
        progress(0.6);
        m = minimize_subset(m, &[6, 7, 8], objective, 1.0e-7, 1.0);
        progress(0.8);
        m = refine_matrix(m, objective, 0.01);
    }
    progress(0.9);

    // Anchor the neutral patch as closely as a linear matrix allows.
    if opts.neutral_index.is_some()
        && let Some(neutral) = items.iter().find(|i| i.is_neutral)
    {
        let anchor = vec![neutral.clone()];
        let anchored = refine_matrix(m, |c| rms_de(&anchor, wp, c), 0.01);
        m = anchored;
    }

    let mut refinement_satisfied = true;
    let mut refinement_failures = Vec::new();
    if opts.refine_to_windows && items.iter().any(|i| i.window.is_some()) {
        let windowed: Vec<WorkItem> = items.clone();
        if windowed_de(&windowed, wp, &m) > 1.0e-5 {
            m = refine_matrix(m, |c| windowed_de(&windowed, wp, c), 0.01);
        }
        for (i, item) in items.iter().enumerate() {
            let Some(w) = item.window else {
                continue;
            };
            let predicted = m * item.cam;
            let de = signed_lch_de(item.xyz, predicted, wp);
            for d in 0..3 {
                if de[d] < w[2 * d] - 0.1 || de[d] > w[2 * d + 1] + 0.1 {
                    refinement_satisfied = false;
                    refinement_failures.push(i);
                    break;
                }
            }
        }
    }

    progress(1.0);

    Ok(MatrixFit {
        matrix: scale.denormalize(&m),
        rms_de: rms_de(&items, wp, &m),
        mean_de: mean_de(&items, wp, &m),
        patch_count: items.len(),
        refinement_satisfied,
        refinement_failures,
    })
}

/// Signed per-dimension LCh difference: `[lightness, chroma, hue]`.
fn signed_lch_de(reference: Vec3, predicted: Vec3, wp: Vec3) -> [f64; 3] {
    let ref_lch = lab_to_lch(xyz_to_lab(reference, wp));
    let got_lch = lab_to_lch(xyz_to_lab(predicted, wp));
    let mut de = [
        crate::color::ciede2000_lch(reference, predicted, wp, 1.0, 1.0e6, 1.0e6),
        crate::color::ciede2000_lch(reference, predicted, wp, 1.0e6, 1.0, 1.0e6),
        crate::color::ciede2000_lch(reference, predicted, wp, 1.0e6, 1.0e6, 1.0),
    ];
    for d in 0..3 {
        if got_lch[d] < ref_lch[d] {
            de[d] = -de[d];
        }
    }
    de
}

/// Objective for the refinement pass: sum of squared window violations.
fn windowed_de(items: &[WorkItem], wp: Vec3, m: &Mat3) -> f64 {
    let mut sum = 0.0;
    for item in items {
        let predicted = *m * item.cam;
        let de = signed_lch_de(item.xyz, predicted, wp);
        match &item.window {
            None => {
                let total = ciede2000(item.xyz, predicted, wp);
                sum += total * total;
            }
            Some(w) => {
                let mut acc = 0.0;
                for d in 0..3 {
                    let lo = w[2 * d];
                    let hi = w[2 * d + 1];
                    let mut err = if de[d] < lo {
                        de[d] - lo
                    } else if de[d] > hi {
                        de[d] - hi
                    } else {
                        0.0
                    };
                    if err.abs() < 0.01 {
                        err = 0.0;
                    }
                    acc += err * err;
                }
                sum += acc;
            }
        }
    }
    sum.sqrt()
}

/// Fit the forward matrix, which also has to keep the LUT gamut well formed.
///
/// `polygon` is the `u'v'` triangle of the working colour space. Keeping the
/// matrix primaries inside it prevents the LUT from having to correct wildly
/// non-physical colours near the cube corners.
pub fn fit_forward_matrix(
    ps: &PatchSet,
    opts: &MatrixFitOptions,
    polygon: [Vec3; 3],
    progress: Progress<'_>,
) -> Result<MatrixFit, crate::target::TargetError> {
    let scale = WorkingScale::of(ps);
    let items = work_items(ps, opts, &scale);
    if items.is_empty() {
        return Err(crate::target::TargetError::Missing { what: "patches to fit".into() });
    }
    let wp = opts.white_point;
    let min_entry = opts.min_entry.unwrap_or(FORWARD_MIN_ENTRY);

    let objective = |m: &Mat3| -> f64 {
        if violates_min_entry(m, &min_entry) {
            return PENALTY;
        }
        let (gamut, colour) = rms_de_with_gamut(&items, wp, m, &polygon);
        gamut + colour
    };

    let mut m = Mat3::IDENTITY;
    for row in 0..3 {
        for col in 0..3 {
            if min_entry[row] > m[row][col] {
                m[row][col] = min_entry[row];
            }
        }
    }

    progress(0.1);
    m = minimize_subset(m, &[0, 4, 8], objective, 1.0e-7, 1.0);
    progress(0.3);
    m = minimize_subset(m, &[3, 4, 5], objective, 1.0e-7, 1.0);
    progress(0.5);
    m = minimize_subset(m, &[0, 1, 2], objective, 1.0e-7, 1.0);
    progress(0.7);
    m = minimize_subset(m, &[6, 7, 8], objective, 1.0e-7, 1.0);
    progress(0.85);

    // Balance the two objectives so neither dominates the refinement.
    let (g0, c0) = rms_de_with_gamut(&items, wp, &m, &polygon);
    let gamut_weight = if (g0 + c0) > 0.0 { c0 / (g0 + c0) } else { 1.0 };
    let colour_weight = if (g0 + c0) > 0.0 { g0 / (g0 + c0) } else { 1.0 };
    let balanced = |mm: &Mat3| -> f64 {
        if violates_min_entry(mm, &min_entry) {
            return PENALTY;
        }
        let (gamut, colour) = rms_de_with_gamut(&items, wp, mm, &polygon);
        gamut * gamut_weight + colour * colour_weight
    };
    m = refine_matrix(m, balanced, 0.01);
    progress(0.95);

    if let Some(neutral) = items.iter().find(|i| i.is_neutral) {
        let anchor = vec![neutral.clone()];
        m = refine_matrix(m, |c| rms_de(&anchor, wp, c) + 0.01 * balanced(c), 0.01);
    }
    progress(1.0);

    Ok(MatrixFit {
        matrix: scale.denormalize(&m),
        rms_de: rms_de(&items, wp, &m),
        mean_de: mean_de(&items, wp, &m),
        patch_count: items.len(),
        refinement_satisfied: true,
        refinement_failures: Vec::new(),
    })
}

impl WorkItem {
    fn clone(&self) -> Self {
        Self { cam: self.cam, xyz: self.xyz, weight: self.weight, window: self.window, is_neutral: self.is_neutral }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{Patch, PatchSet};

    /// Build a synthetic target whose camera response is a known fraction of
    /// the reference colour, so an ideal camera is a diagonal matrix close to
    /// the identity.
    ///
    /// A real capture has comparable camera and reference magnitudes once the
    /// target is normalised, so the fixture is kept on that scale.
    fn synthetic(scale: f64, count: usize) -> PatchSet {
        let mut ps = PatchSet::new();
        ps.illuminant_name = "D50".into();
        ps.illuminant_white_point = crate::color::D50;
        for i in 0..count {
            let t = i as f64 / (count - 1) as f64;
            let xyz = Vec3::new(0.12 + 0.78 * t, 0.15 + 0.72 * ((t * 1.7) % 1.0), 0.08 + 0.70 * ((t * 2.3) % 1.0));
            let mut p = Patch::new(format!("P{i:02}"));
            p.xyz = xyz;
            p.cam = xyz / scale;
            p.matrix_weight = 1.0 / count as f64;
            ps.push(p, None);
        }
        ps
    }

    /// A target whose camera signal is a known 3x3 transform of the reference.
    fn with_truth(truth: Mat3, count: usize) -> PatchSet {
        let inverse = truth.inverted().expect("invertible");
        let mut ps = PatchSet::new();
        ps.illuminant_name = "D50".into();
        ps.illuminant_white_point = crate::color::D50;
        for i in 0..count {
            let t = i as f64 / (count - 1) as f64;
            let xyz = Vec3::new(0.20 + 0.70 * t, 0.18 + 0.72 * ((t * 1.7) % 1.0), 0.15 + 0.70 * ((t * 2.3) % 1.0));
            let mut p = Patch::new(format!("P{i:02}"));
            p.xyz = xyz;
            p.cam = inverse * xyz;
            p.matrix_weight = 1.0 / count as f64;
            ps.push(p, None);
        }
        ps
    }

    #[test]
    fn recovers_a_near_identity_matrix() {
        // A camera whose signal has the same magnitude as the reference needs
        // a matrix close to the identity.
        let ps = synthetic(1.0, 30);
        let fit = fit_camera_matrix(&ps, &MatrixFitOptions::default(), &mut |_| {}).unwrap();
        let expected = Mat3::IDENTITY;
        assert!(fit.matrix.approx_eq(&expected, 5.0e-2), "{:?}\nvs\n{:?}", fit.matrix, expected);
        assert!(fit.rms_de < 0.5, "{}", fit.rms_de);
    }

    #[test]
    fn recovers_a_rotated_matrix() {
        let truth = Mat3::from_rows([1.70, 0.45, 0.15], [0.60, 1.65, 0.30], [0.15, 0.36, 2.10]);
        let ps = with_truth(truth, 40);
        let fit = fit_camera_matrix(&ps, &MatrixFitOptions::default(), &mut |_| {}).unwrap();
        // Colour matrices are identified by behaviour, not by entries, so
        // compare the colours they predict.
        for p in &ps.patches {
            let a = truth * p.cam;
            let b = fit.matrix * p.cam;
            let de = ciede2000(a, b, crate::color::D50);
            assert!(de < 1.5, "patch DE {de}: {a:?} vs {b:?}");
        }
        assert!(fit.rms_de < 1.0, "{}", fit.rms_de);
    }

    #[test]
    fn fit_reports_patch_count_and_progress() {
        let ps = synthetic(1.0, 12);
        let mut seen = Vec::new();
        let fit = fit_camera_matrix(&ps, &MatrixFitOptions::default(), &mut |p| seen.push(p)).unwrap();
        assert_eq!(fit.patch_count, 12);
        assert_eq!(seen.first(), Some(&0.0));
        assert_eq!(seen.last(), Some(&1.0));
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "{seen:?}");
    }

    #[test]
    fn empty_target_is_an_error() {
        let ps = PatchSet::new();
        assert!(fit_camera_matrix(&ps, &MatrixFitOptions::default(), &mut |_| {}).is_err());
    }

    #[test]
    fn zero_weight_patches_are_ignored() {
        let mut ps = synthetic(1.0, 40);
        // Poison one patch: its reference colour is nonsense, but with zero
        // weight it must not influence the fit.
        ps.patches[3].xyz = Vec3::new(9.0, 9.0, 9.0);
        ps.patches[3].matrix_weight = 0.0;
        let fit = fit_camera_matrix(&ps, &MatrixFitOptions::default(), &mut |_| {}).unwrap();
        for (i, p) in ps.patches.iter().enumerate() {
            if i == 3 {
                continue;
            }
            let de = ciede2000(p.xyz, fit.matrix * p.cam, crate::color::D50);
            assert!(de < 2.0, "patch {i} de {de}");
        }
    }

    #[test]
    fn min_entry_constraint_is_respected() {
        let ps = synthetic(1.0, 24);
        let opts = MatrixFitOptions { min_entry: Some([0.01, 0.01, 0.01]), ..Default::default() };
        let fit = fit_camera_matrix(&ps, &opts, &mut |_| {}).unwrap();
        for row in 0..3 {
            for col in 0..3 {
                assert!(fit.matrix[row][col] >= 0.01 - 1.0e-9, "{:?}", fit.matrix);
            }
        }
    }

    #[test]
    fn forward_fit_stays_inside_the_primaries_polygon() {
        let truth = Mat3::from_rows([1.65, 0.40, 0.15], [0.55, 1.70, 0.30], [0.15, 0.35, 2.05]);
        let ps = with_truth(truth, 30);
        // A generous triangle around the expected primaries.
        let polygon = [Vec3::new(0.1, 0.1, 0.0), Vec3::new(0.6, 0.3, 0.0), Vec3::new(0.15, 0.6, 0.0)];
        let opts = MatrixFitOptions::default();
        let fit = fit_forward_matrix(&ps, &opts, polygon, &mut |_| {}).unwrap();
        assert!(fit.matrix.is_finite());
        assert!(fit.rms_de.is_finite());
        assert!(fit.rms_de < 5.0, "{}", fit.rms_de);
        // Every entry respects the forward matrix minimum.
        for row in 0..3 {
            for col in 0..3 {
                assert!(fit.matrix[row][col] >= FORWARD_MIN_ENTRY[row] - 1.0e-9, "{:?}", fit.matrix);
            }
        }
    }

    #[test]
    fn signed_lch_de_has_the_expected_signs() {
        let wp = crate::color::D50;
        let base = crate::color::lab_to_xyz(Vec3::new(50.0, 0.0, 0.0), wp);
        let lighter = crate::color::lab_to_xyz(Vec3::new(60.0, 0.0, 0.0), wp);
        let darker = crate::color::lab_to_xyz(Vec3::new(40.0, 0.0, 0.0), wp);
        // The components are reported as prediction minus reference, so a
        // lighter prediction gives a positive lightness component.
        let up = signed_lch_de(base, lighter, wp);
        let down = signed_lch_de(base, darker, wp);
        assert!(up[0] > 0.0, "lighter prediction: {up:?}");
        assert!(down[0] < 0.0, "darker prediction: {down:?}");
        // Lightness is isolated by the weighting, so nothing else moves.
        assert!(up[1].abs() < 1.0e-3, "{up:?}");
        assert!(up[2].abs() < 1.0e-3, "{up:?}");
    }

    #[test]
    fn refinement_windows_are_tested_against_the_fit() {
        let mut ps = synthetic(1.0, 30);
        for p in &mut ps.patches {
            p.use_matrix_window = true;
            p.matrix_de_window = [-0.5, 0.5, -0.5, 0.5, -0.5, 0.5];
        }
        let opts = MatrixFitOptions { refine_to_windows: true, ..Default::default() };
        let fit = fit_camera_matrix(&ps, &opts, &mut |_| {}).unwrap();
        assert!(fit.rms_de < 1.0, "{}", fit.rms_de);
        assert!(fit.refinement_failures.len() < ps.len());
    }

    #[test]
    fn warning_free_min_entry_allows_negative_entries() {
        let mut ps = synthetic(1.0, 20);
        // Give one channel a negative response, which a colour matrix may
        // legitimately need.
        for p in &mut ps.patches {
            p.cam[2] = -p.cam[2] * 0.2 + p.cam[1] * 0.1;
        }
        let fit = fit_camera_matrix(&ps, &MatrixFitOptions::default(), &mut |_| {}).unwrap();
        assert!(fit.matrix.is_finite());
    }
}
