//! Correlated colour temperature, tint, and white-balance adaptation.
//!
//! The Planckian locus uses the Kim et al. (2002) cubic approximation (1667–25000 K); outside that
//! range it is extended linearly in mired space. Tint is a signed offset perpendicular to the locus in
//! CIE 1960 (u, v). Tint describes the **correction**: positive adds magenta, negative adds green.
//! A positive correction neutralises a green illuminant above the locus, so its source white uses
//! `Duv = tint / TINT_SCALE`. Bradford adaptation removes that source cast.

use crate::{BRADFORD, Mat3, RgbSpace, Xy, bradford};

/// Tint units per unit of Duv.
pub const TINT_SCALE: f64 = 3000.0;
pub const MIN_K: f64 = 1667.0;
pub const MAX_K: f64 = 25000.0;

fn kim_xy(t: f64) -> Xy {
    let t = t.clamp(MIN_K, MAX_K);
    let (t2, t3) = (t * t, t * t * t);
    let x = if t <= 4000.0 {
        -0.2661239e9 / t3 - 0.2343589e6 / t2 + 0.8776956e3 / t + 0.179910
    } else {
        -3.0258469e9 / t3 + 2.1070379e6 / t2 + 0.2226347e3 / t + 0.240390
    };
    let (x2, x3) = (x * x, x * x * x);
    let y = if t <= 2222.0 {
        -1.1063814 * x3 - 1.34811020 * x2 + 2.18555832 * x - 0.20219683
    } else if t <= 4000.0 {
        -0.9549476 * x3 - 1.37418593 * x2 + 2.09137015 * x - 0.16748867
    } else {
        3.0817580 * x3 - 5.87338670 * x2 + 3.75112997 * x - 0.37001483
    };
    Xy::new(x, y)
}

pub fn xy_to_uv(p: Xy) -> (f64, f64) {
    let d = -2.0 * p.x + 12.0 * p.y + 3.0;
    (4.0 * p.x / d, 6.0 * p.y / d)
}

pub fn uv_to_xy(u: f64, v: f64) -> Xy {
    let d = 2.0 * u - 8.0 * v + 4.0;
    Xy::new(3.0 * u / d, 2.0 * v / d)
}

/// Planckian locus in (u, v) with mired-linear extension outside the approximation's range.
fn locus_uv(t: f64) -> (f64, f64) {
    let t = t.max(1000.0);
    if (MIN_K..=MAX_K).contains(&t) {
        return xy_to_uv(kim_xy(t));
    }
    let (a, b) = if t < MIN_K { (MIN_K, MIN_K + 50.0) } else { (MAX_K - 500.0, MAX_K) };
    let (ua, va) = xy_to_uv(kim_xy(a));
    let (ub, vb) = xy_to_uv(kim_xy(b));
    let (ma, mb, m) = (1e6 / a, 1e6 / b, 1e6 / t);
    let s = (m - ma) / (mb - ma);
    (ua + (ub - ua) * s, va + (vb - va) * s)
}

/// Unit normal of the locus at `t` pointing towards green (increasing v).
fn locus_normal(t: f64) -> (f64, f64) {
    let m = 1e6 / t;
    let (u0, v0) = locus_uv(1e6 / (m + 1.0));
    let (u1, v1) = locus_uv(1e6 / (m - 1.0).max(1.0));
    let (du, dv) = (u1 - u0, v1 - v0);
    let l = du.hypot(dv).max(1e-12);
    let (nx, ny) = (-dv / l, du / l);
    if ny < 0.0 { (-nx, -ny) } else { (nx, ny) }
}

/// Source illuminant chromaticity for temperature `t` (K) and correction `tint`.
/// Positive tint means a green source white, whose correction adds magenta to the image.
pub fn temp_tint_to_xy(t: f64, tint: f64) -> Xy {
    let (u, v) = locus_uv(t);
    let (nu, nv) = locus_normal(t);
    let duv = tint / TINT_SCALE;
    uv_to_xy(u + nu * duv, v + nv * duv)
}

/// Lowest Bradford cone response, relative to D65's, of a white the picture is adapted from
/// ([`adaptable_white`]): the gain on that cone stays within 20 times D65's.
const MIN_CONE: f64 = 0.05;

/// The smallest of `p`'s Bradford cone responses relative to D65's (luminance 1 each).
fn min_cone(p: Xy) -> f64 {
    let (c, d) = (BRADFORD.apply(p.to_xyz()), BRADFORD.apply(crate::D65.to_xyz()));
    (0..3).map(|i| c[i] / d[i]).fold(f64::INFINITY, f64::min)
}

/// [`temp_tint_to_xy`] as a white to adapt from: the tint is limited where a cone response of the white
/// would fall below [`MIN_CONE`] (or below the locus white's own, for a temperature whose locus white is
/// already lower). Bradford divides by the source white's cone responses, so a white with one near zero
/// (a strongly green warm white: at 2000 K from about tint +28, at 3700 K from +135) gives gains that
/// grow without bound and flip sign, and the picture jumps as the slider crosses it. The limit is
/// continuous in both temperature and tint; whites short of it are unchanged.
pub fn adaptable_white(t: f64, tint: f64) -> Xy {
    let white = temp_tint_to_xy(t, tint);
    if !(t.is_finite() && tint.is_finite()) {
        return white;
    }
    let floor = MIN_CONE.min(min_cone(temp_tint_to_xy(t, 0.0)));
    if min_cone(white) >= floor {
        return white;
    }
    // the largest share of the tint that keeps every cone at the floor
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..48 {
        let mid = 0.5 * (lo + hi);
        if min_cone(temp_tint_to_xy(t, tint * mid)) >= floor { lo = mid } else { hi = mid }
    }
    temp_tint_to_xy(t, tint * lo)
}

/// Inverse of [`temp_tint_to_xy`]: nearest locus temperature (searched in mired space) and tint.
pub fn xy_to_temp_tint(p: Xy) -> (f64, f64) {
    let (u, v) = xy_to_uv(p);
    let dist = |m: f64| {
        let (lu, lv) = locus_uv(1e6 / m);
        (u - lu).powi(2) + (v - lv).powi(2)
    };
    // coarse scan then golden-section refine over mired 20..1000 (50000 K .. 1000 K)
    let mut best = 20.0;
    let mut bd = f64::MAX;
    let mut m = 20.0;
    while m <= 1000.0 {
        let d = dist(m);
        if d < bd {
            bd = d;
            best = m;
        }
        m += 5.0;
    }
    let (mut a, mut b) = ((best - 5.0).max(20.0), (best + 5.0).min(1000.0));
    let g = 0.618_033_988_75;
    for _ in 0..60 {
        let c = b - g * (b - a);
        let d = a + g * (b - a);
        if dist(c) < dist(d) { b = d } else { a = c }
    }
    let m = (a + b) / 2.0;
    let t = 1e6 / m;
    let (lu, lv) = locus_uv(t);
    let (nu, nv) = locus_normal(t);
    let duv = (u - lu) * nu + (v - lv) * nv;
    (t, duv * TINT_SCALE)
}

/// Matrix (linear RGB in `space` → same space) that white-balances a scene lit by `src` so that it
/// renders as if lit by the space's white (von-Kries/Bradford adaptation).
pub fn wb_matrix(space: &RgbSpace, src: Xy) -> Mat3 {
    let adapt = bradford(src, space.white);
    space.from_xyz().mul(&adapt).mul(&space.to_xyz())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d65_is_about_6504k() {
        let (t, tint) = xy_to_temp_tint(crate::D65);
        assert!((t - 6504.0).abs() < 60.0, "{t}");
        assert!(tint.abs() < 15.0, "{tint}");
    }

    #[test]
    fn roundtrip_temp_tint() {
        for t in [2000.0, 2856.0, 4000.0, 5003.0, 6500.0, 9000.0, 15000.0, 30000.0, 50000.0] {
            for tint in [-100.0, -20.0, 0.0, 35.0, 150.0] {
                let (t2, tint2) = xy_to_temp_tint(temp_tint_to_xy(t, tint));
                assert!((t2 - t).abs() / t < 0.01, "{t} {tint} -> {t2}");
                assert!((tint2 - tint).abs() < 1.0, "{t} {tint} -> {tint2}");
            }
        }
    }

    #[test]
    fn warmer_is_redder() {
        let warm = temp_tint_to_xy(3000.0, 0.0);
        let cool = temp_tint_to_xy(9000.0, 0.0);
        assert!(warm.x > cool.x);
        // A positive magenta correction specifies a greener source illuminant: higher v.
        let (_, v0) = xy_to_uv(temp_tint_to_xy(5000.0, 0.0));
        let (_, v1) = xy_to_uv(temp_tint_to_xy(5000.0, 50.0));
        assert!(v1 > v0);
    }

    /// Daylight lies above the Planckian locus, so D65 reads as a small positive tint, as
    /// Lightroom's Daylight preset (5500 K, +10) has it (#188).
    #[test]
    fn daylight_has_a_positive_tint() {
        let (_, tint) = xy_to_temp_tint(crate::D65);
        assert!((5.0..15.0).contains(&tint), "{tint}");
    }

    /// A strongly green warm white (2000 K past tint +28, 3700 K past +135 on the raw slider's range) has a
    /// Bradford cone response near zero: the white to adapt from is held at the floor there, so the
    /// adaptation's gains change smoothly through it instead of passing a pole (PR #2527).
    #[test]
    fn adaptable_white_keeps_the_adaptation_finite_and_continuous() {
        let src = crate::REC2020;
        let gains = |t: f64, tint: f64| wb_matrix(&src, adaptable_white(t, tint)).apply([1.0, 1.0, 1.0]);
        let sweeps: [(f64, f64, f64, f64); 2] = [(2000.0, 2000.0, 0.0, 150.0), (3300.0, 4500.0, 150.0, 150.0)];
        for (t0, t1, k0, k1) in sweeps {
            let mut prev: Option<[f64; 3]> = None;
            for i in 0..=300 {
                let f = i as f64 / 300.0;
                let g = gains(t0 + f * (t1 - t0), k0 + f * (k1 - k0));
                assert!(g.iter().all(|v| v.is_finite() && *v > 0.0 && *v < 25.0), "{t0}..{t1} {k0}..{k1} at {f}: {g:?}");
                if let Some(p) = prev {
                    for c in 0..3 {
                        assert!((g[c] / p[c]).ln().abs() < 0.05, "{t0}..{t1} {k0}..{k1} at {f}: {p:?} -> {g:?}");
                    }
                }
                prev = Some(g);
            }
        }
        // short of the floor nothing changes, the locus included
        for (t, tint) in [(2000.0, 0.0), (2000.0, -150.0), (3700.0, 100.0), (6500.0, 150.0), (50000.0, -150.0)] {
            assert_eq!(adaptable_white(t, tint), temp_tint_to_xy(t, tint), "{t} {tint}");
        }
        // past it the tint is held: 2000 K reads the same at +34 and +42
        let (a, b) = (adaptable_white(2000.0, 34.0), adaptable_white(2000.0, 42.0));
        assert!((a.x - b.x).hypot(a.y - b.y) < 1e-6, "{a:?} {b:?}");
    }

    #[test]
    fn wb_matrix_maps_src_white_to_white() {
        let sp = crate::REC2020;
        let src = temp_tint_to_xy(3200.0, 10.0);
        let m = wb_matrix(&sp, src);
        // RGB of the source white in the space:
        let rgb = sp.from_xyz().apply(src.to_xyz());
        let out = m.apply(rgb);
        assert!((out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6, "{out:?}");
    }
}
