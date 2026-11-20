//! Curves for tone mapping: the user's point curve as a natural cubic spline ([`PointCurve`]),
//! monotone cubic Hermite splines (Fritsch–Carlson, [`MonotoneCurve`]) and lookup tables.
//!
//! The point curve follows Lightroom Classic, whose curve is a cubic spline through the points
//! and not an interpolation that stays between them: with a few points it swings past them
//! (bumps and dips between neighbours) and is then cut off at 0 and 1, flat at the top and the
//! bottom. A point curve imported from an XMP (`ToneCurvePV2012`) renders as it does there only
//! if it is interpolated the same way. Measured on two captures of Lightroom Classic's curve
//! graph (5 and 8 points), a natural cubic spline (zero second derivative at both ends) clamped
//! to 0..1 lies within 0.9 of a 0..255 level of the drawn curve (rms 0.14 level); the other end
//! conditions tried (end slope = first / last chord, parabolic runout, not-a-knot) and the
//! monotone spline fit worse.
//!
//! [`MonotoneCurve`] never overshoots between control points, so it can't invert tones by
//! accident; it is no longer what the point curve uses.

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

    /// Sample into a LUT of `n` entries (at least 2) over `0..1`.
    pub fn to_lut(&self, n: usize) -> Lut1 {
        let n = n.max(2);
        Lut1 { v: (0..n).map(|i| self.eval(i as f64 / (n - 1) as f64) as f32).collect() }
    }

    pub fn points(&self) -> Vec<(f64, f64)> {
        self.xs.iter().copied().zip(self.ys.iter().copied()).collect()
    }
}

/// The user's point curve: a natural cubic spline through the control points (zero second
/// derivative at both ends), like Lightroom Classic's. It overshoots between points where they
/// ask for it, and the output is cut off at 0 and 1. Flat before the first point and after the
/// last. Points are taken in `0..1 × 0..1` (clamped there, as the XMP import and the
/// `develop.curve` command already do).
#[derive(Clone, Debug, PartialEq)]
pub struct PointCurve {
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// Second derivative of the spline at each point.
    m2: Vec<f64>,
}

impl PointCurve {
    /// Build from points; drops non-finite ones, sorts by x, merges duplicates (the first of equal
    /// x stays). Fewer than two points → identity.
    pub fn new(points: &[(f64, f64)]) -> PointCurve {
        let mut p: Vec<(f64, f64)> =
            points.iter().filter(|(x, y)| x.is_finite() && y.is_finite()).map(|&(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0))).collect();
        p.sort_by(|a, b| a.0.total_cmp(&b.0));
        p.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
        if p.len() < 2 {
            p = vec![(0.0, 0.0), (1.0, 1.0)];
        }
        let n = p.len();
        let xs: Vec<f64> = p.iter().map(|q| q.0).collect();
        let ys: Vec<f64> = p.iter().map(|q| q.1).collect();
        // Second derivatives m2[1..n-1] (the ends are 0) from the tridiagonal system
        //   h[k-1] m2[k-1] + 2 (h[k-1] + h[k]) m2[k] + h[k] m2[k+1] = 6 (d[k] - d[k-1]),
        // h = interval widths, d = chord slopes. Diagonally dominant, so the Thomas algorithm
        // needs no pivoting.
        let mut m2 = vec![0.0; n];
        let mut upper = vec![0.0; n]; // eliminated super-diagonal
        let mut rhs = vec![0.0; n];
        for k in 1..n - 1 {
            let (h0, h1) = (xs[k] - xs[k - 1], xs[k + 1] - xs[k]);
            let (u_prev, r_prev) = if k == 1 { (0.0, 0.0) } else { (upper[k - 1], rhs[k - 1]) };
            let pivot = 2.0 * (h0 + h1) - h0 * u_prev;
            upper[k] = h1 / pivot;
            rhs[k] = (6.0 * ((ys[k + 1] - ys[k]) / h1 - (ys[k] - ys[k - 1]) / h0) - h0 * r_prev) / pivot;
        }
        for k in (1..n - 1).rev() {
            m2[k] = rhs[k] - upper[k] * m2[k + 1];
        }
        PointCurve { xs, ys, m2 }
    }

    /// The curve at `x`, always within `0..1`; NaN in, NaN out.
    pub fn eval(&self, x: f64) -> f64 {
        // NaN passes both bound checks below and would send the search past either end
        if x.is_nan() {
            return x;
        }
        let n = self.xs.len();
        if x <= self.xs[0] {
            return self.ys[0];
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1];
        }
        let i = match self.xs.binary_search_by(|v| v.total_cmp(&x)) {
            Ok(i) => return self.ys[i],
            Err(i) => i - 1,
        };
        let h = self.xs[i + 1] - self.xs[i];
        let (b, a) = ((x - self.xs[i]) / h, (self.xs[i + 1] - x) / h);
        let y = a * self.ys[i] + b * self.ys[i + 1] + ((a * a * a - a) * self.m2[i] + (b * b * b - b) * self.m2[i + 1]) * h * h / 6.0;
        y.clamp(0.0, 1.0)
    }

    /// Sample into a LUT of `n` entries (at least 2) over `0..1`.
    pub fn to_lut(&self, n: usize) -> Lut1 {
        let n = n.max(2);
        Lut1 { v: (0..n).map(|i| self.eval(i as f64 / (n - 1) as f64) as f32).collect() }
    }
}

/// A 1-D lookup table over `0..1` with linear interpolation; inputs outside clamp.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut1 {
    pub v: Vec<f32>,
}

impl Lut1 {
    /// At least 2 entries, like [`Lut1::from_fn`].
    pub fn identity(n: usize) -> Lut1 {
        Lut1::from_fn(n, |x| x)
    }
    /// `f` sampled at `n` evenly spaced points over `0..1` (at least 2).
    pub fn from_fn(n: usize, f: impl Fn(f32) -> f32) -> Lut1 {
        let n = n.max(2);
        Lut1 { v: (0..n).map(|i| f(i as f32 / (n - 1) as f32)).collect() }
    }
    /// `v` is public, so a table shorter than 2 entries is read as constant (1) or the identity (0).
    #[inline]
    pub fn eval(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let n = self.v.len();
        if n < 2 {
            return self.v.first().copied().unwrap_or(x);
        }
        let f = x * (n - 1) as f32;
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
        let c = a.compose(&b);
        assert!((c.eval(0.3) - 0.3).abs() < 2e-3);
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

    /// Points of the two Lightroom Classic captures, in 0..255 levels as its XMP stores them, and
    /// samples of the curve it drew through them: the centre of its 2-pixel line, one sample in
    /// ~50 of 1160 traced per capture, with the stretches the graph frame cuts off left out. A
    /// graph pixel is 255 / 418 = 0.61 of a level.
    const LR_8_POINTS: [(f64, f64); 8] = [(0., 0.), (63., 63.), (70., 113.), (133., 144.), (162., 142.), (166., 74.), (234., 95.), (255., 255.)];
    const LR_8_TRACE: [(f64, f64); 24] = [
        (52.3, 3.4),
        (58.5, 33.9),
        (64.7, 76.6),
        (71.2, 119.9),
        (78.7, 151.0),
        (98.4, 149.8),
        (114.4, 121.6),
        (130.9, 137.6),
        (139.6, 178.4),
        (146.0, 208.9),
        (156.8, 198.0),
        (160.1, 167.5),
        (163.2, 122.9),
        (164.9, 92.4),
        (167.6, 48.5),
        (169.6, 18.0),
        (224.6, 19.2),
        (228.5, 49.7),
        (232.3, 80.8),
        (237.8, 124.8),
        (241.7, 155.9),
        (245.8, 186.4),
        (250.0, 217.5),
        (253.9, 248.0),
    ];
    const LR_5_POINTS: [(f64, f64); 5] = [(0., 0.), (63., 63.), (133., 144.), (137., 90.), (255., 255.)];
    const LR_5_TRACE: [(f64, f64); 14] = [
        (45.7, 3.4),
        (63.8, 69.9),
        (73.8, 123.5),
        (82.4, 177.8),
        (91.9, 231.5),
        (126.0, 218.1),
        (131.3, 163.8),
        (137.5, 83.3),
        (141.9, 29.0),
        (228.6, 31.4),
        (235.3, 85.1),
        (241.7, 139.4),
        (248.1, 193.7),
        (254.0, 248.0),
    ];

    fn levels(points: &[(f64, f64)]) -> PointCurve {
        PointCurve::new(&points.iter().map(|&(x, y)| (x / 255.0, y / 255.0)).collect::<Vec<_>>())
    }

    /// Distance, in levels, from `p` (x, y in levels) to the curve, which is sampled finely.
    fn distance_to(c: &PointCurve, p: (f64, f64)) -> f64 {
        (0..=40_000).map(|i| i as f64 / 40_000.0 * 255.0).map(|x| (x - p.0).hypot(c.eval(x / 255.0) * 255.0 - p.1)).fold(f64::MAX, f64::min)
    }

    /// The curve, in levels, at 1001 evenly spaced inputs from `from` to `to` (levels).
    fn sweep(c: &PointCurve, from: f64, to: f64) -> impl Iterator<Item = f64> + '_ {
        (0..=1000).map(move |i| c.eval((from + (to - from) * i as f64 / 1000.0) / 255.0) * 255.0)
    }

    #[test]
    fn point_curve_follows_the_lightroom_captures() {
        // Measured over every traced pixel: rms 0.14 level, at most 0.9 (at the cut-off ends).
        for (points, trace) in [(&LR_8_POINTS[..], &LR_8_TRACE[..]), (&LR_5_POINTS[..], &LR_5_TRACE[..])] {
            let c = levels(points);
            for &p in trace {
                let d = distance_to(&c, p);
                assert!(d < 1.0, "{p:?} is {d} levels from the curve");
            }
        }
        // a monotone spline is far off the same trace: it never swings past the points
        let m = MonotoneCurve::new(&LR_5_POINTS.map(|(x, y)| (x / 255.0, y / 255.0)));
        assert!(m.eval(0.42) < 0.6 && levels(&LR_5_POINTS).eval(0.42) == 1.0);
    }

    #[test]
    fn point_curve_swings_past_its_points_and_is_cut_off_at_0_and_1() {
        let c = levels(&LR_8_POINTS);
        let on = |from: f64, to: f64| sweep(&c, from, to);
        // between (70, 113) and (133, 144) it rises to ~160; between (133, 144) and (162, 142) to ~218
        assert!(on(70.0, 133.0).fold(0.0, f64::max) > 155.0);
        assert!(on(133.0, 162.0).fold(0.0, f64::max) > 210.0);
        // past (166, 74) it would dip below 0 before (234, 95): it stays at 0 instead
        assert!(on(176.0, 218.0).all(|y| y == 0.0));
        // and the capture's other curve is flat at 1 where it would rise past it
        let top = levels(&LR_5_POINTS);
        assert!((98..=119).all(|x| top.eval(x as f64 / 255.0) == 1.0));
        assert!(top.eval(96.0 / 255.0) < 1.0 && top.eval(122.0 / 255.0) < 1.0);
        for i in 0..=2000 {
            let y = c.eval(i as f64 / 2000.0);
            assert!((0.0..=1.0).contains(&y), "{y}");
        }
    }

    #[test]
    fn point_curve_identity_and_straight_lines() {
        for c in [PointCurve::new(&[(0.0, 0.0), (1.0, 1.0)]), PointCurve::new(&[]), PointCurve::new(&[(0.4, 0.9)])] {
            for i in 0..=10 {
                let x = i as f64 / 10.0;
                assert!((c.eval(x) - x).abs() < 1e-12);
            }
            assert!(c.to_lut(256).is_identity());
        }
        // two points not at the corners: a line between them, flat outside
        let c = PointCurve::new(&[(0.2, 0.1), (0.8, 0.9)]);
        assert_eq!((c.eval(0.0), c.eval(0.2), c.eval(1.0)), (0.1, 0.1, 0.9));
        assert!((c.eval(0.5) - 0.5).abs() < 1e-12);
        // three points on a line stay on it
        let c = PointCurve::new(&[(0.0, 0.2), (0.5, 0.5), (1.0, 0.8)]);
        assert!((0..=10).all(|i| (c.eval(i as f64 / 10.0) - (0.2 + 0.6 * i as f64 / 10.0)).abs() < 1e-12));
    }

    #[test]
    fn point_curve_passes_through_its_points() {
        let pts = [(0.0, 0.1), (0.2, 0.5), (0.45, 0.4), (0.7, 0.9), (1.0, 0.8)];
        let c = PointCurve::new(&pts);
        for (x, y) in pts {
            assert!((c.eval(x) - y).abs() < 1e-12, "{x}");
        }
    }

    #[test]
    fn point_curve_sorts_merges_and_ignores_what_is_not_a_point() {
        let sorted = PointCurve::new(&[(0.0, 0.0), (0.3, 0.6), (0.6, 0.3), (1.0, 1.0)]);
        // given out of order, with a repeated x (the first one stays), a NaN and an infinity
        let messy = PointCurve::new(&[(1.0, 1.0), (0.6, 0.3), (0.3, 0.6), (0.3, 0.2), (f64::NAN, 0.5), (0.5, f64::INFINITY), (0.0, 0.0)]);
        assert_eq!(sorted, messy);
        // outside 0..1 is clamped to it, as the XMP import does
        assert_eq!(PointCurve::new(&[(-3.0, -1.0), (2.0, 5.0)]), PointCurve::new(&[(0.0, 0.0), (1.0, 1.0)]));
        // equal x all round: one point is not a curve
        assert_eq!(PointCurve::new(&[(0.5, 0.2), (0.5, 0.9)]), PointCurve::new(&[]));
    }

    #[test]
    fn point_curve_nan_input_and_short_luts_do_not_panic() {
        let c = PointCurve::new(&[(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)]);
        assert!(c.eval(f64::NAN).is_nan());
        assert!(c.eval(-f64::NAN).is_nan());
        for n in [0, 1] {
            assert_eq!(c.to_lut(n).v.len(), 2, "{n}");
        }
        assert_eq!((c.eval(-1.0), c.eval(2.0)), (0.0, 1.0));
        let lut = c.to_lut(1024);
        assert!((lut.eval(0.5) - 0.6).abs() < 1e-3);
    }

    proptest::proptest! {
        #[test]
        fn point_curve_stays_finite_and_in_range(pts in proptest::collection::vec((-2.0f64..3.0, -2.0f64..3.0), 0..12), x in -1.0f64..2.0) {
            let c = PointCurve::new(&pts);
            let y = c.eval(x);
            proptest::prop_assert!((0.0..=1.0).contains(&y), "{y}");
            proptest::prop_assert!(c.to_lut(64).v.iter().all(|v| (0.0..=1.0).contains(v)));
        }

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
