//! Curves for tone mapping: monotone cubic Hermite splines (Fritsch–Carlson) and lookup tables,
//! backed by `craft_color_transform::curve`.

pub use craft_color_transform::curve::{Lut1, MAX_LUT, MonotoneCurve};

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
