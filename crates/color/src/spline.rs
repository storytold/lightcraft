//! Curves for tone mapping: point curves (natural cubic splines, as Lightroom's point curves) and
//! lookup tables.
//!
//! Lightroom Classic's point curves are natural cubic splines through their points (its renders
//! of a grey ramp under single-channel curves match one within 0.05/255; a monotone spline is off
//! by up to 3/255). A natural spline can overshoot between points; the output is clamped to 0..1.

use serde::{Deserialize, Serialize};

/// A curve through control points in `0..1 × 0..1` (x strictly increasing after normalization),
/// held as a cubic Hermite spline (values and slopes at the points).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointCurve {
    xs: Vec<f64>,
    ys: Vec<f64>,
    ms: Vec<f64>,
}

impl PointCurve {
    /// The natural cubic spline through `points` (zero curvature at both ends); sorts by x, merges
    /// duplicates. Fewer than two points → identity.
    pub fn new(points: &[(f64, f64)]) -> PointCurve {
        let mut p: Vec<(f64, f64)> = points.iter().copied().filter(|(x, y)| x.is_finite() && y.is_finite()).collect();
        p.sort_by(|a, b| a.0.total_cmp(&b.0));
        p.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
        if p.len() < 2 {
            p = vec![(0.0, 0.0), (1.0, 1.0)];
        }
        let n = p.len();
        let xs: Vec<f64> = p.iter().map(|q| q.0).collect();
        let ys: Vec<f64> = p.iter().map(|q| q.1).collect();
        let h: Vec<f64> = (0..n - 1).map(|i| xs[i + 1] - xs[i]).collect();
        let d: Vec<f64> = (0..n - 1).map(|i| (ys[i + 1] - ys[i]) / h[i]).collect();
        // second derivatives: tridiagonal system (Thomas algorithm), zero at both ends
        let mut m2 = vec![0.0; n];
        if n > 2 {
            let (mut c, mut r) = (vec![0.0; n], vec![0.0; n]);
            for i in 1..n - 1 {
                let diag = 2.0 * (h[i - 1] + h[i]) - h[i - 1] * c[i - 1];
                c[i] = h[i] / diag;
                r[i] = (6.0 * (d[i] - d[i - 1]) - h[i - 1] * r[i - 1]) / diag;
            }
            for i in (1..n - 1).rev() {
                m2[i] = r[i] - c[i] * m2[i + 1];
            }
        }
        // slopes at the points reproduce the spline exactly in Hermite form
        let mut ms: Vec<f64> = (0..n - 1).map(|i| d[i] - h[i] * (2.0 * m2[i] + m2[i + 1]) / 6.0).collect();
        ms.push(d[n - 2] + h[n - 2] * (m2[n - 2] + 2.0 * m2[n - 1]) / 6.0);
        PointCurve { xs, ys, ms }
    }

    pub fn identity() -> PointCurve {
        PointCurve::new(&[(0.0, 0.0), (1.0, 1.0)])
    }

    /// The curve at `x`, clamped to 0..1; flat beyond the first and last point.
    pub fn eval(&self, x: f64) -> f64 {
        let n = self.xs.len();
        if x <= self.xs[0] {
            return self.ys[0].clamp(0.0, 1.0);
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1].clamp(0.0, 1.0);
        }
        let i = match self.xs.binary_search_by(|v| v.total_cmp(&x)) {
            Ok(i) => return self.ys[i].clamp(0.0, 1.0),
            Err(i) => i - 1,
        };
        let h = self.xs[i + 1] - self.xs[i];
        let t = (x - self.xs[i]) / h;
        let (t2, t3) = (t * t, t * t * t);
        let y = (2.0 * t3 - 3.0 * t2 + 1.0) * self.ys[i]
            + (t3 - 2.0 * t2 + t) * h * self.ms[i]
            + (-2.0 * t3 + 3.0 * t2) * self.ys[i + 1]
            + (t3 - t2) * h * self.ms[i + 1];
        y.clamp(0.0, 1.0)
    }

    /// Sample into a LUT of `n` entries over `0..1`.
    pub fn to_lut(&self, n: usize) -> Lut1 {
        Lut1 { v: (0..n).map(|i| self.eval(i as f64 / (n - 1) as f64) as f32).collect() }
    }

    pub fn points(&self) -> Vec<(f64, f64)> {
        self.xs.iter().copied().zip(self.ys.iter().copied()).collect()
    }
}

/// A 1-D lookup table over `0..1` with linear interpolation; inputs outside clamp.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut1 {
    pub v: Vec<f32>,
}

impl Lut1 {
    pub fn identity(n: usize) -> Lut1 {
        Lut1 { v: (0..n).map(|i| i as f32 / (n - 1) as f32).collect() }
    }
    pub fn from_fn(n: usize, f: impl Fn(f32) -> f32) -> Lut1 {
        Lut1 { v: (0..n).map(|i| f(i as f32 / (n - 1) as f32)).collect() }
    }
    #[inline]
    pub fn eval(&self, x: f32) -> f32 {
        let n = self.v.len();
        let f = x.clamp(0.0, 1.0) * (n - 1) as f32;
        let i = (f as usize).min(n - 2);
        let t = f - i as f32;
        self.v[i] + (self.v[i + 1] - self.v[i]) * t
    }
    /// `self ∘ inner` (apply `inner` first).
    pub fn compose(&self, inner: &Lut1) -> Lut1 {
        Lut1 { v: inner.v.iter().map(|&x| self.eval(x)).collect() }
    }
    pub fn is_identity(&self) -> bool {
        let n = self.v.len();
        self.v.iter().enumerate().all(|(i, &y)| (y - i as f32 / (n - 1) as f32).abs() < 1e-6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_curve() {
        let c = PointCurve::identity();
        for i in 0..=10 {
            let x = i as f64 / 10.0;
            assert!((c.eval(x) - x).abs() < 1e-12);
        }
        assert!(c.to_lut(256).is_identity());
    }

    /// A natural cubic spline, as Lightroom's point curves (reference values from an independent
    /// natural-spline implementation).
    #[test]
    fn natural_spline_through_the_points() {
        let s = [(0.0, 0.0), (64.0, 40.0), (128.0, 128.0), (192.0, 216.0), (255.0, 255.0)].map(|(x, y)| (x / 255.0, y / 255.0));
        let c = PointCurve::new(&s);
        for (x, y) in s {
            assert!((c.eval(x) - y).abs() < 1e-12);
        }
        for (x, y) in [(32.0, 0.060_804_332_988), (96.0, 0.311_704_648_094), (160.0, 0.692_377_074_634), (224.0, 0.942_037_731_744)] {
            assert!((c.eval(x / 255.0) - y).abs() < 1e-9, "{x}: {}", c.eval(x / 255.0));
        }
        let lift = PointCurve::new(&[(0.0, 0.0), (128.0 / 255.0, 160.0 / 255.0), (1.0, 1.0)]);
        assert!((lift.eval(64.0 / 255.0) - 0.337_440_172_920).abs() < 1e-9);
        // an overshooting spline is clamped, not inverted below 0 or above 1
        let wild = PointCurve::new(&[(0.0, 0.0), (40.0 / 255.0, 120.0 / 255.0), (60.0 / 255.0, 10.0 / 255.0), (1.0, 1.0)]);
        assert!((0..=1000).all(|i| (0.0..=1.0).contains(&wild.eval(i as f64 / 1000.0))));
        // flat beyond the end points
        let late = PointCurve::new(&[(0.1, 0.2), (1.0, 1.0)]);
        assert_eq!(late.eval(0.0), 0.2);
    }

    #[test]
    fn lut_compose() {
        let a = Lut1::from_fn(1024, |x| x * x);
        let b = Lut1::from_fn(1024, |x| x.sqrt());
        let c = a.compose(&b);
        assert!((c.eval(0.3) - 0.3).abs() < 2e-3);
    }
}
