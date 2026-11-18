// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/lut.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Regular-grid lookup tables and the chromaticity correction LUT.
//!
//! Two shapes are used across the engine:
//!
//! * [`Lut3D`] — a regular grid over three input channels, storing one or more
//!   output channels per node. Cubic ("trilinear") and tetrahedral
//!   interpolation are both provided because the DNG and ICC specifications
//!   mandate different ones.
//! * [`ChromaTable`] — the correction a fitted profile applies on top of its
//!   forward matrix. It maps a camera-derived XYZ value to a corrected XYZ value
//!   and is what the DCP HueSatMap and the ICC CLUT are rendered from.

use crate::linalg::Vec3;

/// Errors from LUT construction or lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LutError {
    /// A dimension was zero or otherwise unusable.
    BadDimensions(String),
    /// The node storage does not match the declared dimensions.
    BadStorage {
        /// Expected number of nodes.
        expected: usize,
        /// Number of nodes supplied.
        got: usize,
    },
}

impl core::fmt::Display for LutError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadDimensions(d) => write!(f, "invalid LUT dimensions: {d}"),
            Self::BadStorage { expected, got } => {
                write!(f, "LUT needs {expected} nodes but got {got}")
            }
        }
    }
}

impl std::error::Error for LutError {}

/// Interpolation scheme for a three-dimensional LUT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// Trilinear ("cubic") interpolation between the eight surrounding nodes.
    /// This is what the DNG HueSatMap is defined in terms of.
    Trilinear,
    /// Tetrahedral interpolation, which is exact on the diagonal and is what
    /// the ICC specification mandates for CLUTs.
    Tetrahedral,
}

/// A regular three-dimensional lookup table.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3D {
    /// Number of nodes along each input axis.
    dims: [usize; 3],
    /// Number of output channels per node.
    channels: usize,
    /// Node data, ordered with the first axis varying fastest.
    nodes: Vec<f64>,
    /// Interpolation used by [`Lut3D::lookup`].
    interpolation: Interpolation,
}

impl Lut3D {
    /// Allocate a zeroed table.
    pub fn zeros(dims: [usize; 3], channels: usize) -> Result<Self, LutError> {
        if dims.contains(&0) {
            return Err(LutError::BadDimensions(format!("{dims:?}")));
        }
        if channels == 0 {
            return Err(LutError::BadDimensions("zero output channels".into()));
        }
        Ok(Self { dims, channels, nodes: vec![0.0; dims[0] * dims[1] * dims[2] * channels], interpolation: Interpolation::Trilinear })
    }

    /// Build from node data ordered with the first axis varying fastest.
    pub fn from_nodes(dims: [usize; 3], channels: usize, nodes: Vec<f64>, interpolation: Interpolation) -> Result<Self, LutError> {
        let mut t = Self::zeros(dims, channels)?;
        let expected = t.nodes.len();
        if nodes.len() != expected {
            return Err(LutError::BadStorage { expected, got: nodes.len() });
        }
        t.nodes = nodes;
        t.interpolation = interpolation;
        Ok(t)
    }

    /// Grid dimensions.
    pub fn dims(&self) -> [usize; 3] {
        self.dims
    }

    /// Output channel count.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Total node count.
    pub fn node_count(&self) -> usize {
        self.dims[0] * self.dims[1] * self.dims[2]
    }

    /// Raw node data, first axis varying fastest.
    pub fn nodes(&self) -> &[f64] {
        &self.nodes
    }

    /// Mutable node data.
    pub fn nodes_mut(&mut self) -> &mut [f64] {
        &mut self.nodes
    }

    /// True when the table maps every node to itself, within `eps`.
    pub fn is_identity(&self, eps: f64) -> bool {
        let d = self.dims;
        for i in 0..d[0] {
            for j in 0..d[1] {
                for k in 0..d[2] {
                    let at = self.index(i, j, k);
                    let expect = [i as f64 / (d[0] - 1).max(1) as f64, j as f64 / (d[1] - 1).max(1) as f64, k as f64 / (d[2] - 1).max(1) as f64];
                    for (c, e) in expect.iter().enumerate().take(self.channels.min(3)) {
                        if (self.nodes[at + c] - e).abs() > eps {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// Offset of a node in the node array.
    #[inline]
    fn index(&self, i: usize, j: usize, k: usize) -> usize {
        ((k * self.dims[1] + j) * self.dims[0] + i) * self.channels
    }

    /// Read one node.
    pub fn node(&self, i: usize, j: usize, k: usize) -> &[f64] {
        let at = self.index(i.min(self.dims[0] - 1), j.min(self.dims[1] - 1), k.min(self.dims[2] - 1));
        &self.nodes[at..at + self.channels]
    }

    /// Write one node.
    pub fn set_node(&mut self, i: usize, j: usize, k: usize, values: &[f64]) {
        let at = self.index(i, j, k);
        if let Some(node) = self.nodes.get_mut(at..at + self.channels) {
            for (n, v) in node.iter_mut().zip(values) {
                *n = *v;
            }
        }
    }

    /// Look up a value with inputs in `[0, 1]`.
    pub fn lookup(&self, input: Vec3) -> Vec3 {
        match self.interpolation {
            Interpolation::Trilinear => self.lookup_trilinear(input),
            Interpolation::Tetrahedral => self.lookup_tetrahedral(input),
        }
    }

    /// Trilinear interpolation.
    pub fn lookup_trilinear(&self, input: Vec3) -> Vec3 {
        let d = self.dims;
        let mut base = [0usize; 3];
        let mut frac = [0.0f64; 3];
        for a in 0..3 {
            let scale = (d[a] - 1) as f64;
            let x = (input[a] * scale).clamp(0.0, scale);
            let i = x.floor();
            base[a] = (i as usize).min(d[a] - 2);
            frac[a] = x - base[a] as f64;
        }
        let mut out = [0.0f64; 3];
        for c in 0..self.channels.min(3) {
            let mut acc = 0.0;
            for corner in 0..8 {
                let i = base[0] + (corner & 1);
                let j = base[1] + ((corner >> 1) & 1);
                let k = base[2] + ((corner >> 2) & 1);
                let w = if corner & 1 == 1 { frac[0] } else { 1.0 - frac[0] }
                    * if corner & 2 == 2 { frac[1] } else { 1.0 - frac[1] }
                    * if corner & 4 == 4 { frac[2] } else { 1.0 - frac[2] };
                acc += w * self.nodes[self.index(i, j, k) + c];
            }
            out[c] = acc;
        }
        Vec3::from_array(out)
    }

    /// Tetrahedral interpolation.
    ///
    /// Splits each cell into six tetrahedra along the main diagonal and
    /// interpolates inside the one containing the sample, which reproduces
    /// grey axis behaviour exactly instead of smearing it across the cell.
    pub fn lookup_tetrahedral(&self, input: Vec3) -> Vec3 {
        let d = self.dims;
        let mut base = [0usize; 3];
        let mut f = [0.0f64; 3];
        for a in 0..3 {
            let scale = (d[a] - 1) as f64;
            let x = (input[a] * scale).clamp(0.0, scale);
            let i = x.floor();
            base[a] = (i as usize).min(d[a] - 2);
            f[a] = x - base[a] as f64;
        }
        let node = |i: usize, j: usize, k: usize, c: usize| -> f64 { self.nodes[self.index(base[0] + i, base[1] + j, base[2] + k) + c] };
        let mut out = [0.0f64; 3];
        for c in 0..self.channels.min(3) {
            let c000 = node(0, 0, 0, c);
            let c111 = node(1, 1, 1, c);
            let v = if f[0] > f[1] {
                if f[1] > f[2] {
                    // f0 >= f1 >= f2
                    let c100 = node(1, 0, 0, c);
                    let c110 = node(1, 1, 0, c);
                    c000 + f[2] * (c111 - c110) + f[1] * (c110 - c100) + f[0] * (c100 - c000)
                } else if f[0] > f[2] {
                    // f0 >= f2 >= f1
                    let c100 = node(1, 0, 0, c);
                    let c101 = node(1, 0, 1, c);
                    c000 + f[1] * (c111 - c101) + f[2] * (c101 - c100) + f[0] * (c100 - c000)
                } else {
                    // f2 >= f0 >= f1
                    let c001 = node(0, 0, 1, c);
                    let c101 = node(1, 0, 1, c);
                    c000 + f[1] * (c111 - c101) + f[0] * (c101 - c001) + f[2] * (c001 - c000)
                }
            } else if f[0] > f[2] {
                // f1 >= f0 >= f2
                let c010 = node(0, 1, 0, c);
                let c110 = node(1, 1, 0, c);
                c000 + f[2] * (c111 - c110) + f[0] * (c110 - c010) + f[1] * (c010 - c000)
            } else if f[1] > f[2] {
                // f1 >= f2 >= f0
                let c010 = node(0, 1, 0, c);
                let c011 = node(0, 1, 1, c);
                c000 + f[0] * (c111 - c011) + f[2] * (c011 - c010) + f[1] * (c010 - c000)
            } else {
                // f2 >= f1 >= f0
                let c001 = node(0, 0, 1, c);
                let c011 = node(0, 1, 1, c);
                c000 + f[0] * (c111 - c011) + f[1] * (c011 - c001) + f[2] * (c001 - c000)
            };
            out[c] = v;
        }
        Vec3::from_array(out)
    }

    /// Largest jump between neighbouring nodes along each axis.
    ///
    /// Large values indicate that the table encodes a discontinuity, which is
    /// visible as banding when the table is applied.
    pub fn max_neighbour_delta(&self) -> f64 {
        let d = self.dims;
        let mut worst: f64 = 0.0;
        for i in 0..d[0] {
            for j in 0..d[1] {
                for k in 0..d[2] {
                    let here = self.index(i, j, k);
                    for axis in 0..3 {
                        let mut next = [i, j, k];
                        if next[axis] + 1 >= d[axis] {
                            continue;
                        }
                        next[axis] += 1;
                        let there = self.index(next[0], next[1], next[2]);
                        for c in 0..self.channels {
                            let d_ = (self.nodes[there + c] - self.nodes[here + c]).abs();
                            if d_ > worst {
                                worst = d_;
                            }
                        }
                    }
                }
            }
        }
        worst
    }
}

/// The chromaticity correction a profile applies on top of its forward matrix.
///
/// A 3x3 matrix cannot describe a camera exactly, and on a ColorChecker the
/// remaining error is systematic — saturated colours render a little light, some
/// hues a little rotated — so a smooth correction of it generalises to colours
/// that are not on the chart. That claim is checked rather than assumed: fitted
/// on 23 patches of a real chart and asked to predict the 24th, the correction
/// lowers the error of the prediction by about a quarter, on each of the three
/// test cameras.
///
/// # The model
///
/// The correction is a function of chromaticity only, which is what makes it
/// exposure invariant and exactly representable as a DNG 2.5D HueSatMap. In CIE
/// 1976 `u'v'`, relative to the D50 white, it gives a chromaticity shift and a
/// luminance factor:
///
/// ```text
/// u'v'_out = u'v'_in + d(u'v'_in)        Y_out = Y_in * exp(l(u'v'_in))
/// ```
///
/// `d` and `l` are thin-plate splines through the residuals the forward matrix
/// leaves on the chart's patches, regularised so the surface stays smooth, with
/// the white point held fixed so that neutrals stay neutral.
///
/// # Beyond the chart
///
/// Outside the chart's gamut the data says nothing, and the two parts are
/// carried there differently:
///
/// - The chromaticity shift `d` fades to zero, so very saturated colours keep
///   the matrix's hue and saturation rather than an extrapolated guess.
/// - The luminance factor `l` is held at its value at the edge of the chart's
///   gamut in the same direction from white, and never exceeds what the
///   chart's own patches needed. A colour more saturated than the chart's blue
///   is therefore darkened as much as that blue was, not more and not less:
///   fading the factor away instead would make such a colour lighter than the
///   blue patch itself, so a deep blue light would glow brighter than its
///   surroundings.
///
/// Holding suits a matrix whose luminance is only trustworthy on the chart, as
/// a matrix with a floor on its luminance row is. On the chart's own free
/// matrix the factor fades with the shift instead
/// ([`ChromaTable::with_held_lightness`]), because that matrix's luminance is
/// the better guess beyond the chart for as long as it holds up.
///
/// Input and output are XYZ relative to D50, the connection space both DCP and
/// ICC profiles work in.
#[derive(Clone, Debug)]
pub struct ChromaTable {
    /// Spline centres: the patches' predicted chromaticities, relative to white.
    nodes: Vec<[f64; 2]>,
    /// Spline weights, one `[du, dv, dl]` triple per centre.
    weights: Vec<[f64; 3]>,
    /// Affine part: the constant and the `u` and `v` coefficients per output.
    affine: [[f64; 3]; 3],
    /// Distance of the outermost patch from white, in `u'v'`.
    reach: f64,
    /// The chromaticity shift is whole up to `fade[0] * reach` and gone at
    /// `fade[1] * reach`; the luminance factor is held from `fade[0] * reach`
    /// outwards.
    fade: [f64; 2],
    /// Smallest and largest luminance correction, `ln` of the factor, that any
    /// patch (or white) needed: the held factor stays inside this range.
    lightness_range: [f64; 2],
    /// Hold the luminance factor beyond the chart rather than fade it.
    hold_lightness: bool,
    /// Largest correction the table applies to any patch, in CIEDE2000.
    pub max_correction: f64,
    /// The regularisation the spline was fitted with.
    pub smoothing: f64,
}

/// D50 in `u'v'`.
fn white_uv() -> [f64; 2] {
    xyz_uv(crate::color::D50)
}

fn xyz_uv(xyz: Vec3) -> [f64; 2] {
    let d = xyz[0] + 15.0 * xyz[1] + 3.0 * xyz[2];
    if d.abs() < 1.0e-12 {
        let w = crate::color::D50;
        let dw = w[0] + 15.0 * w[1] + 3.0 * w[2];
        return [4.0 * w[0] / dw, 9.0 * w[1] / dw];
    }
    [4.0 * xyz[0] / d, 9.0 * xyz[1] / d]
}

fn uv_y_to_xyz(uv: [f64; 2], y: f64) -> Vec3 {
    let (u, v) = (uv[0], uv[1].max(1.0e-9));
    Vec3::new(y * 9.0 * u / (4.0 * v), y, y * (12.0 - 3.0 * u - 20.0 * v) / (4.0 * v))
}

/// The thin-plate spline kernel `r^2 ln r`.
fn tps_kernel(r: f64) -> f64 {
    if r <= 0.0 { 0.0 } else { r * r * r.ln() }
}

impl ChromaTable {
    /// The default regularisation, in the spline's own units.
    ///
    /// Chosen by leave-one-out validation on three real charts: it is the
    /// setting at which predicting each patch from the others improves most, so
    /// it is the one that generalises rather than the one that fits best.
    pub const DEFAULT_SMOOTHING: f64 = 1.0e-4;

    /// A table that changes nothing.
    pub fn identity() -> Self {
        Self {
            nodes: Vec::new(),
            weights: Vec::new(),
            affine: [[0.0; 3]; 3],
            reach: 1.0,
            fade: [1.15, 2.0],
            lightness_range: [0.0, 0.0],
            hold_lightness: true,
            max_correction: 0.0,
            smoothing: Self::DEFAULT_SMOOTHING,
        }
    }

    /// Fit the correction that takes `predicted` onto `reference`.
    ///
    /// Both are XYZ relative to D50, one pair per patch, and `weights` says how
    /// much each patch should count (zero leaves it out). Patches darker than a
    /// few percent are left out too: their chromaticity is mostly noise.
    /// Returns `None` when too few patches remain to say anything.
    pub fn fit(predicted: &[Vec3], reference: &[Vec3], weights: &[f64], smoothing: f64) -> Option<Self> {
        let w0 = white_uv();
        let mut nodes = Vec::new();
        let mut values = Vec::new();
        let mut node_weights = Vec::new();
        for (i, (p, r)) in predicted.iter().zip(reference.iter()).enumerate() {
            let w = weights.get(i).copied().unwrap_or(1.0);
            if !(w > 0.0) || !p.is_finite() || !r.is_finite() || p[1] <= 0.02 || r[1] <= 0.02 {
                continue;
            }
            let pu = xyz_uv(*p);
            let ru = xyz_uv(*r);
            nodes.push([pu[0] - w0[0], pu[1] - w0[1]]);
            values.push([ru[0] - pu[0], ru[1] - pu[1], (r[1] / p[1]).ln()]);
            node_weights.push(w);
        }
        if nodes.len() < 4 {
            return None;
        }
        let reach = nodes.iter().map(|n| (n[0] * n[0] + n[1] * n[1]).sqrt()).fold(0.0f64, f64::max).max(1.0e-6);
        // White needs no correction, so the range always includes zero.
        let lightness_range = values.iter().fold([0.0f64, 0.0f64], |r, v| [r[0].min(v[2]), r[1].max(v[2])]);
        // Hold the white point: a node at the origin with a correction of zero
        // and a weight that makes the smoothing all but ignore it.
        let mean_weight = node_weights.iter().sum::<f64>() / node_weights.len() as f64;
        nodes.push([0.0, 0.0]);
        values.push([0.0, 0.0, 0.0]);
        node_weights.push(mean_weight * 1.0e3);

        let n = nodes.len();
        let size = n + 3;
        let mut a = crate::linalg::DynMatrix::zeros(size, size);
        for i in 0..n {
            for j in 0..n {
                let dx = nodes[i][0] - nodes[j][0];
                let dy = nodes[i][1] - nodes[j][1];
                *a.at_mut(i, j) = tps_kernel((dx * dx + dy * dy).sqrt());
            }
            // Smoothing: a patch with more weight is followed more closely.
            *a.at_mut(i, i) += smoothing * mean_weight / node_weights[i];
            *a.at_mut(i, n) = 1.0;
            *a.at_mut(i, n + 1) = nodes[i][0];
            *a.at_mut(i, n + 2) = nodes[i][1];
            *a.at_mut(n, i) = 1.0;
            *a.at_mut(n + 1, i) = nodes[i][0];
            *a.at_mut(n + 2, i) = nodes[i][1];
        }
        let mut weights_out = vec![[0.0f64; 3]; n];
        let mut affine = [[0.0f64; 3]; 3];
        for k in 0..3 {
            let mut b = vec![0.0f64; size];
            for i in 0..n {
                b[i] = values[i][k];
            }
            let x = crate::linalg::solve_linear(&a, &b)?;
            for i in 0..n {
                weights_out[i][k] = x[i];
            }
            affine[0][k] = x[n];
            affine[1][k] = x[n + 1];
            affine[2][k] = x[n + 2];
        }
        let mut table = Self {
            nodes,
            weights: weights_out,
            affine,
            reach,
            fade: [1.15, 2.0],
            lightness_range,
            hold_lightness: true,
            max_correction: 0.0,
            smoothing,
        };
        table.max_correction = predicted
            .iter()
            .filter(|p| p.is_finite() && p[1] > 0.0)
            .map(|p| crate::color::ciede2000(*p, table.correct_xyz(*p), crate::color::D50))
            .fold(0.0f64, f64::max);
        Some(table)
    }

    /// Hold the luminance factor beyond the chart (the default), or fade it out
    /// together with the chromaticity shift.
    pub fn with_held_lightness(mut self, hold: bool) -> Self {
        self.hold_lightness = hold;
        self
    }

    /// The spline itself, without any treatment beyond the chart.
    fn spline(&self, uv: [f64; 2]) -> [f64; 3] {
        let mut out = [0.0f64; 3];
        for k in 0..3 {
            out[k] = self.affine[0][k] + self.affine[1][k] * uv[0] + self.affine[2][k] * uv[1];
        }
        for (node, w) in self.nodes.iter().zip(self.weights.iter()) {
            let dx = uv[0] - node[0];
            let dy = uv[1] - node[1];
            let phi = tps_kernel((dx * dx + dy * dy).sqrt());
            for k in 0..3 {
                out[k] += w[k] * phi;
            }
        }
        out
    }

    /// The correction `[du, dv, dl]` at a chromaticity given relative to white.
    pub fn delta(&self, uv: [f64; 2]) -> [f64; 3] {
        if self.nodes.is_empty() {
            return [0.0; 3];
        }
        let r = (uv[0] * uv[0] + uv[1] * uv[1]).sqrt() / self.reach;
        let inner = self.spline(uv);
        // The chromaticity shift fades beyond the chart's gamut with a
        // smoothstep, so it and its slope are both continuous.
        let t = ((r - self.fade[0]) / (self.fade[1] - self.fade[0])).clamp(0.0, 1.0);
        let keep = 1.0 - t * t * (3.0 - 2.0 * t);
        if !self.hold_lightness {
            return [inner[0] * keep, inner[1] * keep, inner[2] * keep];
        }
        // The luminance factor is held: beyond the edge it takes the value at
        // the edge in the same direction, kept inside what the patches needed.
        let held = if r > self.fade[0] {
            let s = self.fade[0] / r;
            self.spline([uv[0] * s, uv[1] * s])[2]
        } else {
            inner[2]
        };
        let dl = held.clamp(self.lightness_range[0], self.lightness_range[1]);
        [inner[0] * keep, inner[1] * keep, dl]
    }

    /// Apply the correction to an XYZ triple relative to D50.
    pub fn correct_xyz(&self, xyz: Vec3) -> Vec3 {
        if self.nodes.is_empty() || !xyz.is_finite() || xyz[1] <= 0.0 {
            return xyz;
        }
        let w0 = white_uv();
        let uv = xyz_uv(xyz);
        let d = self.delta([uv[0] - w0[0], uv[1] - w0[1]]);
        uv_y_to_xyz([uv[0] + d[0], uv[1] + d[1]], xyz[1] * d[2].exp())
    }

    /// True when the table changes nothing worth mentioning.
    pub fn is_noop(&self, eps: f64) -> bool {
        self.nodes.is_empty() || self.max_correction <= eps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(dims: [usize; 3]) -> Lut3D {
        let mut t = Lut3D::zeros(dims, 3).unwrap();
        for i in 0..dims[0] {
            for j in 0..dims[1] {
                for k in 0..dims[2] {
                    t.set_node(
                        i,
                        j,
                        k,
                        &[i as f64 / (dims[0] - 1).max(1) as f64, j as f64 / (dims[1] - 1).max(1) as f64, k as f64 / (dims[2] - 1).max(1) as f64],
                    );
                }
            }
        }
        t
    }

    #[test]
    fn identity_table_is_identity() {
        let t = ramp([4, 5, 6]);
        assert!(t.is_identity(1.0e-12));
        for x in [0.0, 0.2, 0.5, 0.77, 1.0] {
            let v = t.lookup(Vec3::new(x, x, x));
            assert!(v.distance(&Vec3::new(x, x, x)) < 1.0e-12, "{v:?}");
        }
        // the identity table is a noop
        {
            let t = ChromaTable::identity();
            assert!(t.is_noop(0.0));
            let xyz = Vec3::new(0.3, 0.35, 0.25);
            assert!(t.correct_xyz(xyz).distance(&xyz) < 1.0e-12);
        }
    }

    #[test]
    fn trilinear_and_tetrahedral_agree_on_the_diagonal() {
        let t = ramp([5, 5, 5]);
        for x in [0.0, 0.1, 0.33, 0.5, 0.9, 1.0] {
            let a = t.lookup_trilinear(Vec3::new(x, x, x));
            let b = t.lookup_tetrahedral(Vec3::new(x, x, x));
            assert!(a.distance(&b) < 1.0e-12, "{x}: {a:?} vs {b:?}");
        }
    }

    #[test]
    fn interpolation_hits_nodes_exactly() {
        let t = ramp([4, 4, 4]);
        for i in 0..4 {
            let x = i as f64 / 3.0;
            for interp in [Interpolation::Trilinear, Interpolation::Tetrahedral] {
                let mut t2 = t.clone();
                t2.interpolation = interp;
                let v = t2.lookup(Vec3::new(x, x, x));
                assert!((v[0] - x).abs() < 1.0e-12, "{interp:?} at {x}: {v:?}");
            }
        }
    }

    #[test]
    fn lookup_clamps_out_of_range_input() {
        let t = ramp([3, 3, 3]);
        let low = t.lookup(Vec3::new(-1.0, -1.0, -1.0));
        let high = t.lookup(Vec3::new(2.0, 2.0, 2.0));
        assert!(low.distance(&Vec3::ZERO) < 1.0e-12);
        assert!(high.distance(&Vec3::ONE) < 1.0e-12);
    }

    #[test]
    fn rejects_bad_dimensions() {
        assert!(Lut3D::zeros([0, 2, 2], 3).is_err());
        assert!(Lut3D::zeros([2, 2, 2], 0).is_err());
        assert_eq!(
            Lut3D::from_nodes([2, 2, 2], 3, vec![0.0; 10], Interpolation::Trilinear).unwrap_err(),
            LutError::BadStorage { expected: 24, got: 10 }
        );
    }

    #[test]
    fn neighbour_delta_finds_a_discontinuity() {
        let mut t = ramp([3, 3, 3]);
        t.set_node(1, 1, 1, &[0.9, 0.0, 0.0]);
        assert!(t.max_neighbour_delta() > 0.5);
        let smooth = ramp([3, 3, 3]);
        assert!(smooth.max_neighbour_delta() <= 0.5);
    }

    /// A smooth, known distortion of a set of chart-like colours: the kind of
    /// systematic error a matrix leaves, with white untouched.
    fn distorted(xyz: Vec3) -> Vec3 {
        let w = white_uv();
        let uv = xyz_uv(xyz);
        let (du, dv) = (uv[0] - w[0], uv[1] - w[1]);
        let shift = [0.08 * dv, -0.06 * du];
        uv_y_to_xyz([uv[0] + shift[0], uv[1] + shift[1]], xyz[1] * (1.0 - 0.6 * (du * du + dv * dv).sqrt()))
    }

    fn chart_like() -> Vec<Vec3> {
        let mut out = Vec::new();
        for i in 0..24 {
            let hue = i as f64 * 15.0;
            let chroma = 18.0 + (i % 4) as f64 * 12.0;
            let lab = crate::color::lch_to_lab(Vec3::new(35.0 + (i % 5) as f64 * 10.0, chroma, hue));
            out.push(crate::color::lab_to_xyz(lab, crate::color::D50));
        }
        out
    }

    #[test]
    fn a_fitted_table_recovers_a_smooth_distortion_and_keeps_white() {
        let reference = chart_like();
        let predicted: Vec<Vec3> = reference.iter().map(|r| distorted(*r)).collect();
        let weights = vec![1.0; reference.len()];
        let table = ChromaTable::fit(&predicted, &reference, &weights, 1.0e-7).expect("a table");
        for (p, r) in predicted.iter().zip(reference.iter()) {
            let de = crate::color::ciede2000(*r, table.correct_xyz(*p), crate::color::D50);
            assert!(de < 0.3, "patch corrected to {de} DE");
        }
        // White and greys stay where they were.
        for y in [0.05, 0.2, 0.9] {
            let grey = crate::color::D50 * y;
            let out = table.correct_xyz(grey);
            assert!(crate::color::ciede2000(grey, out, crate::color::D50) < 0.05, "{out:?}");
        }
    }

    #[test]
    fn the_correction_is_exposure_invariant() {
        let reference = chart_like();
        let predicted: Vec<Vec3> = reference.iter().map(|r| distorted(*r)).collect();
        let table = ChromaTable::fit(&predicted, &reference, &[1.0; 24], ChromaTable::DEFAULT_SMOOTHING).unwrap();
        let xyz = Vec3::new(0.21, 0.18, 0.09);
        let once = table.correct_xyz(xyz);
        let scaled = table.correct_xyz(xyz * 4.0);
        assert!(scaled.distance(&(once * 4.0)) < 1.0e-12, "{scaled:?} vs {:?}", once * 4.0);
    }

    #[test]
    fn beyond_the_chart_the_shift_fades_and_the_lightness_factor_holds() {
        let reference = chart_like();
        let predicted: Vec<Vec3> = reference.iter().map(|r| distorted(*r)).collect();
        let table = ChromaTable::fit(&predicted, &reference, &[1.0; 24], ChromaTable::DEFAULT_SMOOTHING).unwrap();
        for angle in [0.0f64, 1.3, 2.9, 4.4] {
            let dir = [angle.cos(), angle.sin()];
            let at = |r: f64| table.delta([dir[0] * table.reach * r, dir[1] * table.reach * r]);
            // Far beyond every patch the chromaticity shift is exactly zero ...
            let far = at(2.5);
            assert_eq!([far[0], far[1]], [0.0, 0.0]);
            // ... while the lightness factor is the one at the chart's edge,
            // and inside the range the patches asked for.
            let edge = at(table.fade[0]);
            assert!((far[2] - edge[2]).abs() < 1.0e-12, "{far:?} vs {edge:?}");
            assert!(far[2] >= table.lightness_range[0] && far[2] <= table.lightness_range[1]);
            // Everything changes continuously through the transition: a step
            // of 1e-7 changes it by no more than a modest slope allows.
            for r in [1.1, 1.15, 1.5, 2.0] {
                let a = at(r);
                let b = at(r + 1.0e-7);
                for k in 0..3 {
                    assert!((a[k] - b[k]).abs() < 1.0e-5, "jump at {r} reach: {a:?} vs {b:?}");
                }
            }
        }
    }

    #[test]
    fn a_colour_beyond_the_chart_keeps_the_lightness_factor_of_the_chart_edge() {
        // The distortion renders saturated colours dark, so the fitted table
        // lightens them; beyond the chart it must keep lightening them by the
        // same factor instead of letting them fall back to the matrix's
        // lightness, and never by more than any patch needed.
        let reference = chart_like();
        let predicted: Vec<Vec3> = reference.iter().map(|r| distorted(*r)).collect();
        let table = ChromaTable::fit(&predicted, &reference, &[1.0; 24], ChromaTable::DEFAULT_SMOOTHING).unwrap();
        let w = white_uv();
        let dir = [-0.6f64, -0.8];
        let mut held = None;
        for step in 1..=40 {
            let r = 0.1 * step as f64;
            let xyz = uv_y_to_xyz([w[0] + dir[0] * table.reach * r, w[1] + dir[1] * table.reach * r], 0.2);
            let ratio = table.correct_xyz(xyz)[1] / xyz[1];
            assert!(ratio.ln() <= table.lightness_range[1] + 1.0e-12, "{ratio} at {r} reach");
            if r > table.fade[0] {
                let first = *held.get_or_insert(ratio);
                assert!((ratio - first).abs() < 1.0e-9, "{ratio} vs {first} at {r} reach");
            }
        }
        assert!(held.expect("steps beyond the chart") > 1.05);
    }

    #[test]
    fn a_table_can_fade_its_lightness_factor_instead() {
        let reference = chart_like();
        let predicted: Vec<Vec3> = reference.iter().map(|r| distorted(*r)).collect();
        let held = ChromaTable::fit(&predicted, &reference, &[1.0; 24], ChromaTable::DEFAULT_SMOOTHING).unwrap();
        let faded = held.clone().with_held_lightness(false);
        // Identical on the chart ...
        for p in &predicted {
            assert!(held.correct_xyz(*p).distance(&faded.correct_xyz(*p)) < 1.0e-12);
        }
        // ... and far beyond it the faded table changes nothing at all.
        let far = faded.delta([faded.reach * 2.5, 0.0]);
        assert_eq!(far, [0.0, 0.0, 0.0]);
        assert!(held.delta([held.reach * 2.5, 0.0])[2] != 0.0);
    }

    #[test]
    fn too_few_patches_give_no_table() {
        let reference = chart_like();
        let predicted: Vec<Vec3> = reference.iter().map(|r| distorted(*r)).collect();
        assert!(ChromaTable::fit(&predicted[..3], &reference[..3], &[1.0; 3], 1.0e-4).is_none());
    }
}
