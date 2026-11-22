//! The midtone curve: identity at 0, fixed ends, monotone in the input and in the midtone, the measured anchors, the
//! slopes it continues with, and the marker.

use crate::levels_curve::{MidtoneTable, from_marker, marker};

fn grid() -> Vec<f64> {
    (-20..=20).map(|i| f64::from(i) / 20.0).collect()
}

#[test]
fn the_neutral_midtone_is_the_identity_and_every_curve_keeps_its_ends() {
    let id = MidtoneTable::new(0.0);
    for i in 0..=100 {
        let t = i as f32 / 100.0;
        assert!((id.eval(t) - t).abs() < 1e-6, "{t}");
    }
    for m in grid() {
        let c = MidtoneTable::new(m);
        assert_eq!(c.eval(0.0), 0.0, "{m}");
        assert_eq!(c.eval(1.0), 1.0, "{m}");
    }
    // not a number, or far out of range: the neutral curve, or the end of the range
    assert_eq!(MidtoneTable::new(f64::NAN), MidtoneTable::new(0.0));
    assert_eq!(MidtoneTable::new(5.0), MidtoneTable::new(1.0));
    assert_eq!(MidtoneTable::new(-5.0), MidtoneTable::new(-1.0));
}

#[test]
fn the_curves_are_monotone_in_the_input_and_in_the_midtone() {
    let tables: Vec<(f64, MidtoneTable)> = grid().into_iter().map(|m| (m, MidtoneTable::new(m))).collect();
    for (m, c) in &tables {
        let mut last = -1.0f32;
        for i in 0..=2000 {
            let v = c.eval(i as f32 / 2000.0);
            assert!(v >= last - 1e-7, "midtone {m}: falls at {i}");
            last = v;
        }
    }
    // a larger midtone never lowers any output (positive brightens, negative darkens)
    for pair in tables.windows(2) {
        for i in 0..=200 {
            let t = i as f32 / 200.0;
            assert!(pair[1].1.eval(t) >= pair[0].1.eval(t) - 1e-6, "t {t}: {} then {}", pair[0].0, pair[1].0);
        }
    }
}

#[test]
fn the_curves_agree_with_the_measured_values() {
    // aggregate calibration numbers (the middle of the input, and the slopes at the ends), to a few thousandths
    for (m, mid) in [(1.0, 0.7202), (0.5, 0.6653), (0.3, 0.6126), (-0.3, 0.3750), (-0.5, 0.3160), (-1.0, 0.2545)] {
        let c = MidtoneTable::new(m);
        assert!((f64::from(c.eval(0.5)) - mid).abs() < 3e-3, "{m}: {}", c.eval(0.5));
    }
    let (top, bottom) = (MidtoneTable::new(1.0), MidtoneTable::new(-1.0));
    assert!((top.s0 - 4.3).abs() < 0.1 && (top.s1 - 0.45).abs() < 0.05, "{} {}", top.s0, top.s1);
    assert!((bottom.s0 - 0.23).abs() < 0.03 && (bottom.s1 - 2.23).abs() < 0.1, "{} {}", bottom.s0, bottom.s1);
}

#[test]
fn beyond_the_ends_the_curve_continues_with_its_end_slopes() {
    for m in [-1.0, -0.4, 0.3, 1.0] {
        let c = MidtoneTable::new(m);
        assert!((c.eval(-0.5) - c.s0 * -0.5).abs() < 1e-6, "{m}");
        assert!((c.eval(1.5) - (1.0 + c.s1 * 0.5)).abs() < 1e-6, "{m}");
        // continuous, and nearly the same slope on both sides of each end
        let (inside0, inside1) = ((c.eval(0.001) - c.eval(0.0)) / 0.001, (c.eval(1.0) - c.eval(0.999)) / 0.001);
        assert!((inside0 / c.s0 - 1.0).abs() < 0.08, "{m}: {inside0} against {}", c.s0);
        assert!((inside1 / c.s1 - 1.0).abs() < 0.08, "{m}: {inside1} against {}", c.s1);
    }
}

#[test]
fn the_marker_moves_one_way_and_round_trips() {
    assert_eq!(marker(0.0), 0.5);
    assert!((marker(1.0) - 0.254).abs() < 0.004 && (marker(-1.0) - 0.72).abs() < 0.004);
    let mut last = 1.0;
    for m in grid() {
        let p = marker(m);
        assert!(p <= last + 1e-9, "the marker moves toward the shadows as the midtone grows: {m}");
        last = p;
    }
    // a position to a midtone and back: exact (the marker is solved on the curve), anywhere in its reach
    let (lo, hi) = (marker(1.0), marker(-1.0));
    for i in 1..200 {
        let p = lo + (hi - lo) * f64::from(i) / 200.0;
        let m = from_marker(p);
        assert!((marker(m) - p).abs() < 1e-6, "{p}: midtone {m} has marker {}", marker(m));
    }
    // a midtone to a position and back, away from the flat ends
    for m in [-0.7, -0.3, 0.1, 0.5, 0.7] {
        assert!((from_marker(marker(m)) - m).abs() < 2e-3, "{m}");
    }
    // beyond the reach: the end of the range; the neutral position: exactly 0
    assert_eq!(from_marker(0.0), 1.0);
    assert_eq!(from_marker(1.0), -1.0);
    assert_eq!(from_marker(0.5), 0.0);
    assert_eq!(from_marker(f64::NAN), 0.0);
}
