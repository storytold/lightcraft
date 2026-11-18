// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/linalg.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Small fixed-size linear algebra used throughout the colour pipeline.
//!
//! Only the shapes the engine actually needs are implemented: 3-vectors and
//! 3x3 matrices, plus a dense row-major matrix used by the spectral and
//! optimization code. Everything is plain `f64` and stack allocated, so the
//! hot loops stay allocation free.

use core::fmt;
use core::ops::{Add, AddAssign, Div, Index, IndexMut, Mul, Neg, Sub, SubAssign};

/// A three component vector: either a tristimulus value, a camera signal or a
/// chromaticity triple, depending on context.
#[derive(Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    /// Components.
    pub v: [f64; 3],
}

/// A 3x3 matrix, row major.
#[derive(Clone, Copy, PartialEq, Default)]
pub struct Mat3 {
    /// Rows, then columns.
    pub v: [[f64; 3]; 3],
}

impl Vec3 {
    /// Zero vector.
    pub const ZERO: Self = Self { v: [0.0; 3] };
    /// Ones vector.
    pub const ONE: Self = Self { v: [1.0; 3] };

    /// Build from components.
    #[inline]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { v: [x, y, z] }
    }

    /// Build from an array.
    #[inline]
    pub const fn from_array(v: [f64; 3]) -> Self {
        Self { v }
    }

    /// Largest component.
    #[inline]
    pub fn max(&self) -> f64 {
        let mut m = self.v[0];
        if self.v[1] > m {
            m = self.v[1];
        }
        if self.v[2] > m {
            m = self.v[2];
        }
        m
    }

    /// Smallest component.
    #[inline]
    pub fn min(&self) -> f64 {
        let mut m = self.v[0];
        if self.v[1] < m {
            m = self.v[1];
        }
        if self.v[2] < m {
            m = self.v[2];
        }
        m
    }

    /// Sum of all components.
    #[inline]
    pub fn sum(&self) -> f64 {
        self.v[0] + self.v[1] + self.v[2]
    }

    /// Arithmetic mean of all components.
    #[inline]
    pub fn avg(&self) -> f64 {
        self.sum() / 3.0
    }

    /// Component-wise absolute value.
    #[inline]
    pub fn abs(&self) -> Self {
        Self::new(self.v[0].abs(), self.v[1].abs(), self.v[2].abs())
    }

    /// Component-wise reciprocal.
    #[inline]
    pub fn recip(&self) -> Self {
        Self::new(1.0 / self.v[0], 1.0 / self.v[1], 1.0 / self.v[2])
    }

    /// True when every component is finite.
    #[inline]
    pub fn is_finite(&self) -> bool {
        self.v.iter().all(|c| c.is_finite())
    }

    /// True when every component is exactly zero.
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.v == [0.0; 3]
    }

    /// True when any component is outside the unit range.
    #[inline]
    pub fn outside_unit_range(&self) -> bool {
        self.v.iter().any(|&c| !(0.0..=1.0).contains(&c))
    }

    /// Clamp every component into `[0, 1]`.
    #[inline]
    pub fn clamp01(&self) -> Self {
        Self::new(self.v[0].clamp(0.0, 1.0), self.v[1].clamp(0.0, 1.0), self.v[2].clamp(0.0, 1.0))
    }

    /// Flush negative components to zero.
    #[inline]
    pub fn clamp_low(&self) -> Self {
        Self::new(self.v[0].max(0.0), self.v[1].max(0.0), self.v[2].max(0.0))
    }

    /// Clamp only the upper bound.
    #[inline]
    pub fn clamp_high(&self) -> Self {
        Self::new(self.v[0].min(1.0), self.v[1].min(1.0), self.v[2].min(1.0))
    }

    /// Euclidean length.
    #[inline]
    pub fn length(&self) -> f64 {
        self.dot(self).sqrt()
    }

    /// Dot product.
    #[inline]
    pub fn dot(&self, o: &Self) -> f64 {
        self.v[0] * o.v[0] + self.v[1] * o.v[1] + self.v[2] * o.v[2]
    }

    /// Divide by the largest component so the vector peaks at 1.0.
    #[inline]
    pub fn normalized(&self) -> Self {
        let m = self.max();
        if m == 0.0 { *self } else { *self / m }
    }

    /// Euclidean distance to another vector.
    #[inline]
    pub fn distance(&self, o: &Self) -> f64 {
        (*self - *o).length()
    }

    /// Index of the smallest, middle and largest component.
    #[inline]
    pub fn min_mid_max(&self) -> (usize, usize, usize) {
        let mut mini = 0;
        let mut maxi = 2;
        if self.v[1] < self.v[0] {
            mini = 1;
        }
        if self.v[2] < self.v[mini] {
            mini = 2;
        }
        if self.v[0] > self.v[2] {
            maxi = 0;
        }
        if self.v[1] > self.v[maxi] {
            maxi = 1;
        }
        let midi = match (mini, maxi) {
            (0, 2) => 1,
            (0, _) => 2,
            (1, 2) => 0,
            (1, _) => 2,
            (_, 1) => 0,
            _ => 1,
        };
        (mini, midi, maxi)
    }

    /// Index of the largest component.
    #[inline]
    pub fn argmax(&self) -> usize {
        if self.v[0] >= self.v[1] && self.v[0] >= self.v[2] {
            0
        } else if self.v[1] >= self.v[2] {
            1
        } else {
            2
        }
    }
}

impl fmt::Debug for Vec3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:.6}, {:.6}, {:.6}]", self.v[0], self.v[1], self.v[2])
    }
}

impl Index<usize> for Vec3 {
    type Output = f64;
    #[inline]
    fn index(&self, i: usize) -> &f64 {
        &self.v[i]
    }
}

impl IndexMut<usize> for Vec3 {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut f64 {
        &mut self.v[i]
    }
}

impl From<[f64; 3]> for Vec3 {
    fn from(v: [f64; 3]) -> Self {
        Self { v }
    }
}

impl From<Vec3> for [f64; 3] {
    fn from(v: Vec3) -> Self {
        v.v
    }
}

impl Add for Vec3 {
    type Output = Self;
    #[inline]
    fn add(self, o: Self) -> Self {
        Self::new(self.v[0] + o.v[0], self.v[1] + o.v[1], self.v[2] + o.v[2])
    }
}

impl AddAssign for Vec3 {
    #[inline]
    fn add_assign(&mut self, o: Self) {
        *self = *self + o;
    }
}

impl Sub for Vec3 {
    type Output = Self;
    #[inline]
    fn sub(self, o: Self) -> Self {
        Self::new(self.v[0] - o.v[0], self.v[1] - o.v[1], self.v[2] - o.v[2])
    }
}

impl SubAssign for Vec3 {
    #[inline]
    fn sub_assign(&mut self, o: Self) {
        *self = *self - o;
    }
}

impl Mul<f64> for Vec3 {
    type Output = Self;
    #[inline]
    fn mul(self, k: f64) -> Self {
        Self::new(self.v[0] * k, self.v[1] * k, self.v[2] * k)
    }
}

impl Mul<Vec3> for f64 {
    type Output = Vec3;
    #[inline]
    fn mul(self, v: Vec3) -> Vec3 {
        v * self
    }
}

impl Div<f64> for Vec3 {
    type Output = Self;
    #[inline]
    fn div(self, k: f64) -> Self {
        Self::new(self.v[0] / k, self.v[1] / k, self.v[2] / k)
    }
}

impl Div<Vec3> for Vec3 {
    type Output = Self;
    /// Component-wise division, used for channel balancing.
    #[inline]
    fn div(self, o: Self) -> Self {
        Self::new(self.v[0] / o.v[0], self.v[1] / o.v[1], self.v[2] / o.v[2])
    }
}

impl Neg for Vec3 {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self::new(-self.v[0], -self.v[1], -self.v[2])
    }
}

impl Mat3 {
    /// Identity matrix.
    pub const IDENTITY: Self = Self { v: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
    /// Zero matrix.
    pub const ZERO: Self = Self { v: [[0.0; 3]; 3] };

    /// Build from row-major rows.
    #[inline]
    pub const fn from_rows(r0: [f64; 3], r1: [f64; 3], r2: [f64; 3]) -> Self {
        Self { v: [r0, r1, r2] }
    }

    /// Build from a nested array.
    #[inline]
    pub const fn from_array(v: [[f64; 3]; 3]) -> Self {
        Self { v }
    }

    /// Diagonal matrix from a vector.
    #[inline]
    pub fn from_diagonal(d: Vec3) -> Self {
        Self::from_rows([d.v[0], 0.0, 0.0], [0.0, d.v[1], 0.0], [0.0, 0.0, d.v[2]])
    }

    /// The row at `i`.
    #[inline]
    pub fn row(&self, i: usize) -> Vec3 {
        Vec3::from_array(self.v[i])
    }

    /// The column at `j`.
    #[inline]
    pub fn col(&self, j: usize) -> Vec3 {
        Vec3::new(self.v[0][j], self.v[1][j], self.v[2][j])
    }

    /// Sum of every row, i.e. the result of transforming white.
    #[inline]
    pub fn row_sums(&self) -> Vec3 {
        Vec3::new(self.v[0][0] + self.v[0][1] + self.v[0][2], self.v[1][0] + self.v[1][1] + self.v[1][2], self.v[2][0] + self.v[2][1] + self.v[2][2])
    }

    /// Transpose.
    #[inline]
    pub fn transposed(&self) -> Self {
        Self::from_rows(self.col(0).v, self.col(1).v, self.col(2).v)
    }

    /// Determinant.
    #[inline]
    pub fn determinant(&self) -> f64 {
        let a = &self.v;
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    }

    /// Inverse. Returns `None` for a singular matrix.
    pub fn inverted(&self) -> Option<Self> {
        let det = self.determinant();
        if !det.is_finite() || det.abs() < 1.0e-14 {
            return None;
        }
        let a = &self.v;
        let inv_det = 1.0 / det;
        Some(Self::from_rows(
            [
                (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * inv_det,
                (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * inv_det,
                (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * inv_det,
            ],
            [
                (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * inv_det,
                (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * inv_det,
                (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * inv_det,
            ],
            [
                (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * inv_det,
                (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * inv_det,
                (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * inv_det,
            ],
        ))
    }

    /// Inverse, falling back to the identity when singular. Used on paths
    /// where a degenerate target should degrade instead of aborting.
    pub fn inverted_or_identity(&self) -> Self {
        self.inverted().unwrap_or(Self::IDENTITY)
    }

    /// Largest absolute difference against another matrix.
    pub fn max_abs_diff(&self, o: &Self) -> f64 {
        let mut m: f64 = 0.0;
        for i in 0..3 {
            for j in 0..3 {
                let d = (self.v[i][j] - o.v[i][j]).abs();
                if d > m {
                    m = d;
                }
            }
        }
        m
    }

    /// True when the two matrices agree to `eps` in every entry.
    pub fn approx_eq(&self, o: &Self, eps: f64) -> bool {
        self.max_abs_diff(o) <= eps
    }

    /// True when the two matrices are bit-for-bit equal.
    pub fn exact_eq(&self, o: &Self) -> bool {
        self.v == o.v
    }

    /// True when every entry is finite.
    pub fn is_finite(&self) -> bool {
        self.v.iter().flatten().all(|c| c.is_finite())
    }

    /// Scale every entry.
    #[inline]
    pub fn scaled(&self, k: f64) -> Self {
        let mut m = *self;
        for row in &mut m.v {
            for c in row {
                *c *= k;
            }
        }
        m
    }

    /// Scale so that row 1 (the luminance row) sums to one.
    pub fn normalize_luminance_row(&self) -> Self {
        let s = self.row(1).sum();
        if s.abs() < 1.0e-14 { *self } else { self.scaled(1.0 / s) }
    }
}

impl fmt::Debug for Mat3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "[{:+.6} {:+.6} {:+.6}]", self.v[0][0], self.v[0][1], self.v[0][2])?;
        writeln!(f, "[{:+.6} {:+.6} {:+.6}]", self.v[1][0], self.v[1][1], self.v[1][2])?;
        write!(f, "[{:+.6} {:+.6} {:+.6}]", self.v[2][0], self.v[2][1], self.v[2][2])
    }
}

impl Index<usize> for Mat3 {
    type Output = [f64; 3];
    #[inline]
    fn index(&self, i: usize) -> &[f64; 3] {
        &self.v[i]
    }
}

impl IndexMut<usize> for Mat3 {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut [f64; 3] {
        &mut self.v[i]
    }
}

impl Mul<Mat3> for Mat3 {
    type Output = Mat3;
    #[inline]
    fn mul(self, o: Mat3) -> Mat3 {
        let mut m = Mat3::ZERO;
        for i in 0..3 {
            for k in 0..3 {
                let a = self.v[i][k];
                if a == 0.0 {
                    continue;
                }
                for j in 0..3 {
                    m.v[i][j] += a * o.v[k][j];
                }
            }
        }
        m
    }
}

impl Mul<Vec3> for Mat3 {
    type Output = Vec3;
    #[inline]
    fn mul(self, v: Vec3) -> Vec3 {
        Vec3::new(
            self.v[0][0] * v.v[0] + self.v[0][1] * v.v[1] + self.v[0][2] * v.v[2],
            self.v[1][0] * v.v[0] + self.v[1][1] * v.v[1] + self.v[1][2] * v.v[2],
            self.v[2][0] * v.v[0] + self.v[2][1] * v.v[1] + self.v[2][2] * v.v[2],
        )
    }
}

impl Mul<f64> for Mat3 {
    type Output = Mat3;
    #[inline]
    fn mul(self, k: f64) -> Mat3 {
        self.scaled(k)
    }
}

impl Add for Mat3 {
    type Output = Mat3;
    #[inline]
    fn add(self, o: Mat3) -> Mat3 {
        let mut m = self;
        for i in 0..3 {
            for j in 0..3 {
                m.v[i][j] += o.v[i][j];
            }
        }
        m
    }
}

/// A dense row-major matrix of dynamic size.
///
/// Used where the problem size is only known at runtime: the least-squares
/// solve behind the matrix optimizer and the spectral reference tables.
#[derive(Clone, Debug, PartialEq)]
pub struct DynMatrix {
    rows: usize,
    cols: usize,
    data: Vec<f64>,
}

impl DynMatrix {
    /// Allocate a zeroed matrix.
    pub fn zeros(rows: usize, cols: usize) -> Self {
        Self { rows, cols, data: vec![0.0; rows * cols] }
    }

    /// Row count.
    #[inline]
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Column count.
    #[inline]
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Element access.
    #[inline]
    pub fn at(&self, r: usize, c: usize) -> f64 {
        self.data[r * self.cols + c]
    }

    /// Mutable element access.
    #[inline]
    pub fn at_mut(&mut self, r: usize, c: usize) -> &mut f64 {
        &mut self.data[r * self.cols + c]
    }

    /// Raw row-major storage.
    #[inline]
    pub fn as_slice(&self) -> &[f64] {
        &self.data
    }

    /// Raw mutable row-major storage.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [f64] {
        &mut self.data
    }

    /// Copy one row out.
    pub fn row_vec(&self, r: usize) -> Vec<f64> {
        self.data[r * self.cols..(r + 1) * self.cols].to_vec()
    }

    /// Copy one column out.
    pub fn col_vec(&self, c: usize) -> Vec<f64> {
        (0..self.rows).map(|r| self.at(r, c)).collect()
    }

    /// Matrix product.
    pub fn mul(&self, o: &Self) -> Self {
        // (a shape mismatch multiplies the overlap: no caller makes one)
        let inner = self.cols.min(o.rows);
        let mut out = Self::zeros(self.rows, o.cols);
        for i in 0..self.rows {
            for k in 0..inner {
                let a = self.at(i, k);
                if a == 0.0 {
                    continue;
                }
                for j in 0..o.cols {
                    let v = a * o.at(k, j);
                    *out.at_mut(i, j) += v;
                }
            }
        }
        out
    }

    /// Transpose.
    pub fn transposed(&self) -> Self {
        let mut out = Self::zeros(self.cols, self.rows);
        for i in 0..self.rows {
            for j in 0..self.cols {
                *out.at_mut(j, i) = self.at(i, j);
            }
        }
        out
    }

    /// Matrix times column vector.
    pub fn mul_vec(&self, v: &[f64]) -> Vec<f64> {
        (0..self.rows).map(|r| v.iter().take(self.cols).enumerate().map(|(c, x)| self.at(r, c) * x).sum()).collect()
    }
}

/// Solve a small dense symmetric positive definite system with Gaussian
/// elimination and partial pivoting. Returns `None` when singular.
pub fn solve_linear(a: &DynMatrix, b: &[f64]) -> Option<Vec<f64>> {
    let n = a.rows();
    if n == 0 || a.cols() != n || b.len() != n {
        return None;
    }
    let mut m = a.clone();
    let mut x = b.to_vec();
    for col in 0..n {
        let mut pivot = col;
        let mut best = m.at(col, col).abs();
        for r in (col + 1)..n {
            let v = m.at(r, col).abs();
            if v > best {
                best = v;
                pivot = r;
            }
        }
        if best < 1.0e-14 {
            return None;
        }
        if pivot != col {
            for c in 0..n {
                let t = m.at(col, c);
                *m.at_mut(col, c) = m.at(pivot, c);
                *m.at_mut(pivot, c) = t;
            }
            x.swap(col, pivot);
        }
        let d = m.at(col, col);
        for r in (col + 1)..n {
            let f = m.at(r, col) / d;
            if f == 0.0 {
                continue;
            }
            for c in col..n {
                let v = m.at(r, c) - f * m.at(col, c);
                *m.at_mut(r, c) = v;
            }
            x[r] -= f * x[col];
        }
    }
    for col in (0..n).rev() {
        let mut s = x[col];
        for c in (col + 1)..n {
            s -= m.at(col, c) * x[c];
        }
        x[col] = s / m.at(col, col);
    }
    Some(x)
}

/// Least-squares solution of `a * x = b` via the normal equations.
///
/// The systems in this crate are tiny (at most a handful of unknowns) and
/// reasonably conditioned after normalization, so the normal-equation route is
/// both adequate and cheap.
pub fn least_squares(a: &DynMatrix, b: &[f64]) -> Option<Vec<f64>> {
    let at = a.transposed();
    let ata = at.mul(a);
    let atb = at.mul_vec(b);
    solve_linear(&ata, &atb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(a: [[f64; 3]; 3]) -> Mat3 {
        Mat3::from_array(a)
    }

    #[test]
    fn matrix_inverse_roundtrip() {
        let a = m([[2.0, -1.0, 0.5], [0.25, 3.0, -0.75], [1.0, 0.0, 1.0]]);
        let inv = a.inverted().expect("invertible");
        let round = a * inv;
        assert!(round.approx_eq(&Mat3::IDENTITY, 1.0e-12), "{round:?}");
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let a = m([[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 1.0, 1.0]]);
        assert!(a.inverted().is_none());
        // solve linear detects singular
        {
            let mut a = DynMatrix::zeros(2, 2);
            *a.at_mut(0, 0) = 1.0;
            *a.at_mut(0, 1) = 2.0;
            *a.at_mut(1, 0) = 2.0;
            *a.at_mut(1, 1) = 4.0;
            assert!(solve_linear(&a, &[1.0, 2.0]).is_none());
        }
    }

    #[test]
    fn min_mid_max_ordering() {
        assert_eq!(Vec3::new(0.2, 0.9, 0.5).min_mid_max(), (0, 2, 1));
        assert_eq!(Vec3::new(0.9, 0.2, 0.5).min_mid_max(), (1, 2, 0));
        assert_eq!(Vec3::new(0.5, 0.9, 0.2).min_mid_max(), (2, 0, 1));
    }

    #[test]
    fn least_squares_recovers_exact_solution() {
        // Overdetermined but consistent system.
        let mut a = DynMatrix::zeros(4, 2);
        let rows = [[1.0, 0.0], [1.0, 1.0], [1.0, 2.0], [1.0, 3.0]];
        for (i, r) in rows.iter().enumerate() {
            for (j, v) in r.iter().enumerate() {
                *a.at_mut(i, j) = *v;
            }
        }
        let b = [2.0, 4.0, 6.0, 8.0];
        let x = least_squares(&a, &b).expect("solvable");
        assert!((x[0] - 2.0).abs() < 1.0e-10, "{x:?}");
        assert!((x[1] - 2.0).abs() < 1.0e-10, "{x:?}");
    }

    #[test]
    fn normalize_luminance_row_sums_to_one() {
        let a = m([[2.0, 0.0, 0.0], [1.0, 2.0, 1.0], [0.0, 0.0, 4.0]]);
        let n = a.normalize_luminance_row();
        assert!((n.row(1).sum() - 1.0).abs() < 1.0e-15);
    }

    #[test]
    fn dyn_matrix_transpose_roundtrip() {
        let mut a = DynMatrix::zeros(2, 3);
        for i in 0..2 {
            for j in 0..3 {
                *a.at_mut(i, j) = (i * 3 + j) as f64;
            }
        }
        let t = a.transposed();
        assert_eq!(t.rows(), 3);
        assert_eq!(t.cols(), 2);
        for i in 0..2 {
            for j in 0..3 {
                assert_eq!(t.at(j, i), a.at(i, j));
            }
        }
    }
}
