//! Curves for tone mapping: monotone cubic Hermite splines (Fritsch–Carlson) and lookup tables.
//!
//! `Lut1` is re-exported from `craft_color_transform::curve` (for `compose_len` / `resample` at
//! `CURVE_N = 1024`), while `MonotoneCurve` preserves the legacy constructor that keeps every
//! finite control point before sorting (so saved curves with > 1024 points keep their endpoints).

pub use craft_color_transform::curve::{Lut1, MAX_LUT};
use serde::{Deserialize, Serialize};

/// A curve through control points in `0..1 × 0..1` (x strictly increasing after normalization).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonotoneCurve {
    xs: Vec<f64>,
    ys: Vec<f64>,
    ms: Vec<f64>,
}

impl MonotoneCurve {
    /// Build from points; sorts by x, merges duplicates. Fewer than two points → identity.
    pub fn new(points: &[(f64, f64)]) -> MonotoneCurve {
        let mut p: Vec<(f64, f64)> = points.iter().copied().filter(|(x, y)| x.is_finite() && y.is_finite()).collect();
        p.sort_by(|a, b| a.0.total_cmp(&b.0));
        p.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
        if p.len() < 2 {
            p = vec![(0.0, 0.0), (1.0, 1.0)];
        }
        let n = p.len();
        let xs: Vec<f64> = p.iter().map(|q| q.0).collect();
        let ys: Vec<f64> = p.iter().map(|q| q.1).collect();
        let d: Vec<f64> = (0..n - 1).map(|i| (ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i])).collect();
        let mut ms = vec![0.0; n];
        ms[0] = d[0];
        ms[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            ms[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
        }
        // Fritsch–Carlson limiter where the data is monotone within a segment.
        for i in 0..n - 1 {
            if d[i] == 0.0 {
                ms[i] = 0.0;
                ms[i + 1] = 0.0;
                continue;
            }
            let a = ms[i] / d[i];
            let b = ms[i + 1] / d[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                ms[i] = t * a * d[i];
                ms[i + 1] = t * b * d[i];
            }
        }
        MonotoneCurve { xs, ys, ms }
    }

    pub fn identity() -> MonotoneCurve {
        MonotoneCurve::new(&[(0.0, 0.0), (1.0, 1.0)])
    }

    pub fn eval(&self, x: f64) -> f64 {
        // NaN passes both bound checks below and would send the search past either end
        if x.is_nan() {
            return x;
        }
        let n = self.xs.len();
        if x <= self.xs[0] {
            return self.ys[0] + self.ms[0] * (x - self.xs[0]).min(0.0) * 0.0;
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1];
        }
        let i = match self.xs.binary_search_by(|v| v.total_cmp(&x)) {
            Ok(i) => return self.ys[i],
            Err(i) => i - 1,
        };
        let h = self.xs[i + 1] - self.xs[i];
        let t = (x - self.xs[i]) / h;
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * self.ys[i]
            + (t3 - 2.0 * t2 + t) * h * self.ms[i]
            + (-2.0 * t3 + 3.0 * t2) * self.ys[i + 1]
            + (t3 - t2) * h * self.ms[i + 1]
    }

    /// Sample into a LUT of `n` entries (2 to [`MAX_LUT`]) over `0..1`.
    pub fn to_lut(&self, n: usize) -> Lut1 {
        let n = n.clamp(2, MAX_LUT);
        Lut1 { v: (0..n).map(|i| self.eval(i as f64 / (n - 1) as f64) as f32).collect() }
    }

    pub fn points(&self) -> Vec<(f64, f64)> {
        self.xs.iter().copied().zip(self.ys.iter().copied()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_curve() {
        let c = MonotoneCurve::identity();
        for i in 0..=10 {
            let x = i as f64 / 10.0;
            assert!((c.eval(x) - x).abs() < 1e-12);
        }
        assert!(c.to_lut(256).is_identity());
    }

    #[test]
    fn passes_through_points_and_is_monotone() {
        let pts = [(0.0, 0.0), (0.25, 0.15), (0.5, 0.5), (0.75, 0.88), (1.0, 1.0)];
        let c = MonotoneCurve::new(&pts);
        for (x, y) in pts {
            assert!((c.eval(x) - y).abs() < 1e-12);
        }
        let mut prev = -1.0;
        for i in 0..=1000 {
            let y = c.eval(i as f64 / 1000.0);
            assert!(y >= prev - 1e-12);
            prev = y;
        }
    }

    #[test]
    fn no_overshoot_on_flat_segment() {
        let c = MonotoneCurve::new(&[(0.0, 0.0), (0.4, 0.5), (0.6, 0.5), (1.0, 1.0)]);
        for i in 400..=600 {
            let y = c.eval(i as f64 / 1000.0);
            assert!((y - 0.5).abs() < 1e-9, "{y}");
        }
    }

    #[test]
    fn lut_compose() {
        let a = Lut1::from_fn(1024, |x| x * x);
        let b = Lut1::from_fn(1024, |x| x.sqrt());
        let c = a.compose_len(&b, 1024);
        assert_eq!(c.v.len(), 1024);
        assert!((c.eval(0.3) - 0.3).abs() < 2e-3);
        assert_eq!(c.resample(256).v.len(), 256);
    }

    #[test]
    fn preserves_control_points_past_1024_before_sorting() {
        // 1,028 interior points followed by the curve's endpoints after index 1,024:
        // truncating to the first 1,024 points before sorting would drop (1.0, 0.25).
        let mut pts: Vec<(f64, f64)> = (1..=1028).map(|i| (i as f64 / 2000.0, 0.8 * (i as f64 / 2000.0))).collect();
        pts.push((0.0, 0.0));
        pts.push((1.0, 0.25));
        let c = MonotoneCurve::new(&pts);
        assert_eq!(c.points().len(), 1030);
        assert!((c.eval(1.0) - 0.25).abs() < 1e-12);
        let lut = c.to_lut(1024);
        assert_eq!(lut.v.len(), 1024);
        assert!((lut.eval(1.0) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn nan_input_and_short_luts_do_not_panic() {
        let c = MonotoneCurve::new(&[(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)]);
        assert!(c.eval(f64::NAN).is_nan());
        assert!(c.eval(-f64::NAN).is_nan());
        for n in [0, 1] {
            assert_eq!(c.to_lut(n).v.len(), 2, "{n}");
            assert!(Lut1::identity(n).is_identity(), "{n}");
            assert_eq!(Lut1::from_fn(n, |x| x).v.len(), 2, "{n}");
        }
        assert_eq!(Lut1 { v: vec![] }.eval(0.3), 0.3);
        assert_eq!(Lut1 { v: vec![] }.eval(-1.0), 0.0, "inputs outside 0..1 clamp, as for any table");
        assert_eq!(Lut1 { v: vec![] }.eval(2.0), 1.0);
        assert_eq!(Lut1 { v: vec![0.7] }.eval(0.3), 0.7);
    }

    proptest::proptest! {
        #[test]
        fn monotone_for_increasing_data(ys in proptest::collection::vec(0.0f64..1.0, 3..8)) {
            let mut ys = ys; ys.sort_by(|a, b| a.total_cmp(b));
            let n = ys.len();
            let pts: Vec<(f64, f64)> = ys.iter().enumerate().map(|(i, y)| (i as f64 / (n - 1) as f64, *y)).collect();
            let c = MonotoneCurve::new(&pts);
            let mut prev = f64::MIN;
            for i in 0..=200 { let y = c.eval(i as f64 / 200.0); proptest::prop_assert!(y >= prev - 1e-9); prev = y; }
        }
    }
}
