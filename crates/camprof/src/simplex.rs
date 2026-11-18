// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/simplex.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Nelder-Mead downhill simplex minimization.
//!
//! A compact, dependency-free implementation used by the matrix optimizer and
//! by the white point searches. It is a direct-search method: no derivatives
//! are needed, which suits objective functions built from colour differences.
//!
//! The implementation is deterministic. Simplex setup, reflection, expansion
//! and contraction follow the classical coefficients, and iteration stops as
//! soon as the standard deviation of the vertex values drops below the
//! requested epsilon.

/// Coefficients of the classical Nelder-Mead scheme.
const ALPHA: f64 = 1.0;
const BETA: f64 = 0.5;
const GAMMA: f64 = 2.0;

/// Tuning for a minimization run.
pub struct SimplexOptions<'a> {
    /// Convergence threshold on the standard deviation of vertex values.
    pub epsilon: f64,
    /// Size of the initial simplex, relative to the problem scale.
    pub scale: f64,
    /// Optional projection applied to every candidate point, for constrained
    /// problems such as "keep these coefficients non-negative".
    pub constrain: Option<&'a dyn Fn(&mut [f64])>,
    /// Iteration cap. Defaults to 100_000 when zero.
    pub max_iterations: usize,
}

impl core::fmt::Debug for SimplexOptions<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SimplexOptions")
            .field("epsilon", &self.epsilon)
            .field("scale", &self.scale)
            .field("constrained", &self.constrain.is_some())
            .field("max_iterations", &self.max_iterations)
            .finish()
    }
}

impl Default for SimplexOptions<'_> {
    fn default() -> Self {
        Self { epsilon: 1.0e-7, scale: 1.0, constrain: None, max_iterations: 100_000 }
    }
}

/// Result of a minimization run.
#[derive(Clone, Debug)]
pub struct SimplexResult {
    /// The best point found, also written back into the caller's slice.
    pub value: f64,
    /// Number of objective function evaluations.
    pub evaluations: usize,
    /// Iterations performed.
    pub iterations: usize,
}

/// Minimize `objective` starting from `start`, which is updated in place with
/// the best point found.
///
/// `start` must have at least one component; the dimensionality is taken from
/// its length.
pub fn minimize<F>(objective: F, start: &mut [f64], options: &SimplexOptions<'_>) -> SimplexResult
where
    F: Fn(&[f64]) -> f64,
{
    let n = start.len();
    let max_iterations = if options.max_iterations == 0 { 100_000 } else { options.max_iterations };

    let pn = options.scale * (((n + 1) as f64).sqrt() - 1.0 + n as f64) / (n as f64 * 2.0_f64.sqrt());
    let qn = options.scale * (((n + 1) as f64).sqrt() - 1.0) / (n as f64 * 2.0_f64.sqrt());

    let mut vertices = vec![vec![0.0; n]; n + 1];
    vertices[0].copy_from_slice(start);
    for i in 1..=n {
        for j in 0..n {
            vertices[i][j] = start[j] + if i - 1 == j { pn } else { qn };
        }
    }
    if let Some(c) = options.constrain {
        for v in vertices.iter_mut() {
            c(v);
        }
    }

    let mut values: Vec<f64> = vertices.iter().map(|v| objective(v)).collect();
    let mut evaluations = n + 1;
    let mut iterations = 0usize;

    let mut reflection = vec![0.0; n];
    let mut expansion = vec![0.0; n];
    let mut contraction = vec![0.0; n];
    let mut centroid = vec![0.0; n];

    for iteration in 1..=max_iterations {
        iterations = iteration;
        let mut vg = 0usize;
        for j in 0..=n {
            if values[j] > values[vg] {
                vg = j;
            }
        }
        let mut vs = 0usize;
        for j in 0..=n {
            if values[j] < values[vs] {
                vs = j;
            }
        }
        let mut vh = vs;
        for j in 0..=n {
            if values[j] > values[vh] && values[j] < values[vg] {
                vh = j;
            }
        }

        for j in 0..n {
            let mut cent = 0.0;
            for m in 0..=n {
                if m != vg {
                    cent += vertices[m][j];
                }
            }
            centroid[j] = cent / n as f64;
        }

        for j in 0..n {
            reflection[j] = centroid[j] + ALPHA * (centroid[j] - vertices[vg][j]);
        }
        if let Some(c) = options.constrain {
            c(&mut reflection);
        }
        let fr = objective(&reflection);
        evaluations += 1;

        if fr < values[vh] && fr >= values[vs] {
            vertices[vg].copy_from_slice(&reflection);
            values[vg] = fr;
        }

        if fr < values[vs] {
            for j in 0..n {
                expansion[j] = centroid[j] + GAMMA * (reflection[j] - centroid[j]);
            }
            if let Some(c) = options.constrain {
                c(&mut expansion);
            }
            let fe = objective(&expansion);
            evaluations += 1;
            if fe < fr {
                vertices[vg].copy_from_slice(&expansion);
                values[vg] = fe;
            } else {
                vertices[vg].copy_from_slice(&reflection);
                values[vg] = fr;
            }
        }

        if fr >= values[vh] {
            if fr < values[vg] {
                // Outside contraction.
                for j in 0..n {
                    contraction[j] = centroid[j] + BETA * (reflection[j] - centroid[j]);
                }
            } else {
                // Inside contraction.
                for j in 0..n {
                    contraction[j] = centroid[j] - BETA * (centroid[j] - vertices[vg][j]);
                }
            }
            if let Some(c) = options.constrain {
                c(&mut contraction);
            }
            let fc = objective(&contraction);
            evaluations += 1;
            if fc < values[vg] {
                vertices[vg].copy_from_slice(&contraction);
                values[vg] = fc;
            } else {
                // Shrink towards the best vertex.
                for row in 0..=n {
                    if row != vs {
                        for j in 0..n {
                            vertices[row][j] = vertices[vs][j] + (vertices[row][j] - vertices[vs][j]) / 2.0;
                        }
                    }
                }
                if let Some(c) = options.constrain {
                    c(&mut vertices[vg]);
                    c(&mut vertices[vh]);
                }
                values[vg] = objective(&vertices[vg]);
                evaluations += 1;
                values[vh] = objective(&vertices[vh]);
                evaluations += 1;
            }
        }

        let fsum: f64 = values.iter().sum();
        let favg = fsum / (n + 1) as f64;
        let variance: f64 = values.iter().map(|f| (f - favg).powi(2) / n as f64).sum();
        let spread = variance.sqrt();
        // The vertex values share the scale of the objective, so a purely
        // absolute threshold either stops far too early on a large objective
        // or never stops on a small one. Compare against both a relative
        // threshold and a small absolute floor.
        if spread <= options.epsilon * (1.0 + favg.abs()) {
            break;
        }
    }

    let mut best = 0usize;
    for j in 0..=n {
        if values[j] < values[best] {
            best = j;
        }
    }
    start.copy_from_slice(&vertices[best]);
    let value = objective(&vertices[best]);
    evaluations += 1;

    SimplexResult { value, evaluations, iterations }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_minimum_of_a_quadratic() {
        // Minimum at (3, -2). The default convergence threshold lands the
        // simplex within a few 1e-4 of the vertex.
        let f = |x: &[f64]| (x[0] - 3.0).powi(2) + (x[1] + 2.0).powi(2);
        let mut start = [0.0, 0.0];
        let r = minimize(f, &mut start, &SimplexOptions::default());
        assert!((start[0] - 3.0).abs() < 5.0e-4, "{start:?} ({r:?})");
        assert!((start[1] + 2.0).abs() < 5.0e-4, "{start:?} ({r:?})");
        assert!(r.value < 1.0e-6, "{:?}", r.value);
        // tighter epsilon gives a closer answer
        {
            let f = |x: &[f64]| (x[0] - 3.0).powi(2) + (x[1] + 2.0).powi(2);
            let mut loose = [0.0, 0.0];
            minimize(f, &mut loose, &SimplexOptions::default());
            let mut tight = [0.0, 0.0];
            minimize(f, &mut tight, &SimplexOptions { epsilon: 1.0e-10, ..Default::default() });
            let loose_err = (loose[0] - 3.0).abs() + (loose[1] + 2.0).abs();
            let tight_err = (tight[0] - 3.0).abs() + (tight[1] + 2.0).abs();
            assert!(tight_err <= loose_err + 1.0e-12, "{tight_err} vs {loose_err}");
            assert!(tight_err < 1.0e-3, "{tight:?}");
        }
    }

    #[test]
    fn handles_rosenbrock_valley() {
        let f = |x: &[f64]| (1.0 - x[0]).powi(2) + 100.0 * (x[1] - x[0] * x[0]).powi(2);
        let mut start = [-1.2, 1.0];
        let r = minimize(f, &mut start, &SimplexOptions { scale: 0.5, ..Default::default() });
        assert!((start[0] - 1.0).abs() < 1.0e-3, "{start:?}");
        assert!((start[1] - 1.0).abs() < 1.0e-3, "{start:?}");
        assert!(r.value < 1.0e-6, "{}", r.value);
    }

    #[test]
    fn constraint_is_applied_to_every_candidate() {
        // Unconstrained minimum is at 5, but the constraint caps at 2.
        let cap = |x: &mut [f64]| {
            if x[0] > 2.0 {
                x[0] = 2.0;
            }
        };
        let f = |x: &[f64]| (x[0] - 5.0).powi(2);
        let mut start = [0.0];
        minimize(f, &mut start, &SimplexOptions { constrain: Some(&cap), ..Default::default() });
        assert!(start[0] <= 2.0 + 1.0e-9, "{start:?}");
    }

    #[test]
    fn iteration_cap_is_respected() {
        let f = |x: &[f64]| (x[0] * 1.0e6).powi(2);
        let mut start = [1.0];
        let r = minimize(f, &mut start, &SimplexOptions { max_iterations: 5, ..Default::default() });
        assert_eq!(r.iterations, 5);
        assert!(r.evaluations >= 5);
    }
}
