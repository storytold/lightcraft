//! Process V8's tone-curve stage ([`lightcraft_develop::Process::V8`], `docs/curve-stage.md`): Whites, the
//! parametric curve and the point curves, applied the way Lightroom applies them.
//!
//! Measured on Camera Raw renders of synthetic charts (`tools/lr_tone_charts.py`):
//! - Whites, the parametric curve, the master point curve, Contrast and Blacks act hue-preserving on linear ProPhoto
//!   RGB: the curve maps the largest and the smallest channel (on sRGB-encoded values), and the middle channel keeps
//!   its relative position between them in linear light. Predicting coloured patches from the curve measured on
//!   grey steps this way leaves 0.03 to 0.16 (OkLab × 100), against 0.3 to 3.4 for V7.
//! - The red, green and blue curves act per channel on sRGB-encoded linear ProPhoto RGB, after the master curve.
//! - Whites and the four parametric regions are tables of Camera Raw's measured output (`curve_stage_tables`):
//!   Whites reaches into the midtones (Whites −50 lowers encoded 0.6 by about 10/255), and each region is broad and
//!   overlapping. The regions add: the sum of single regions reproduces three at once within 1/255.
//!
//! The stage runs after the tone map, Process V4's calibrated Contrast and Blacks and V8's colour mixer
//! ([`crate::mixer_stage`]), before the colour stage. It
//! replaces V7's curves on encoded output values; the tone map leaves Whites to it.

use lightcraft_color::spline::{Lut1, MonotoneCurve};
use lightcraft_color::transfer::{linear_to_srgb, srgb_to_linear};
use lightcraft_develop::{DevelopSettings, Process, ToneCurve};
use lightcraft_geom::Point;

use crate::curve_stage_tables as t;
use crate::light_v3::PRO_Y;

/// Entries per table (as the shipping curve tables).
pub const N: usize = 1024;

/// Lightroom's default parametric splits: the measured region tables hold for them only.
const DEFAULT_SPLITS: [f64; 3] = [25.0, 50.0, 75.0];

/// The stage's tables: `base` (Whites, then the parametric curve, then the master point curve) acts hue-preserving,
/// `chan` (red, green, blue point curves) per channel. All on sRGB-encoded linear ProPhoto values.
#[derive(Clone, Debug)]
pub struct CurveStage {
    pub chan: [Lut1; 3],
    pub base: Lut1,
    /// Refine Saturation, 0..1, on what the curves did (1 when only Whites is set).
    pub refine: f32,
}

/// A measured delta table (257 entries over 0..1) at `x`.
fn table_at(v: &[f32; 257], x: f32) -> f32 {
    let p = x.clamp(0.0, 1.0) * 256.0;
    let i = (p as usize).min(255);
    let f = p - i as f32;
    match (v.get(i), v.get(i + 1)) {
        (Some(a), Some(b)) => a + (b - a) * f,
        _ => 0.0,
    }
}

/// The delta at `x` of a setting measured at `amounts` (sorted, without 0), linearly between the measured amounts
/// and towards 0 below the smallest; clamped to the measured range.
fn measured_delta<const K: usize>(amounts: &[f32; K], tables: &[[f32; 257]; K], amount: f32, x: f32) -> f32 {
    if amount == 0.0 || K == 0 {
        return 0.0;
    }
    // the measured amounts with 0 between the negative and positive ones
    let mut keys: Vec<(f32, Option<&[f32; 257]>)> = amounts.iter().zip(tables.iter()).map(|(a, v)| (*a, Some(v))).collect();
    keys.push((0.0, None));
    keys.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (lo, hi) = (keys.first().map_or(0.0, |k| k.0), keys.last().map_or(0.0, |k| k.0));
    let a = amount.clamp(lo, hi);
    let at = |k: &(f32, Option<&[f32; 257]>)| k.1.map_or(0.0, |v| table_at(v, x));
    for w in keys.windows(2) {
        if let [k0, k1] = w
            && a >= k0.0
            && a <= k1.0
        {
            let f = if k1.0 > k0.0 { (a - k0.0) / (k1.0 - k0.0) } else { 0.0 };
            return at(k0) * (1.0 - f) + at(k1) * f;
        }
    }
    0.0
}

/// The parametric curve: the measured regions at the default splits, else the analytic model of earlier processes.
fn parametric(c: &ToneCurve) -> Lut1 {
    let splits = [c.split_shadows, c.split_mid, c.split_highlights];
    if splits.iter().zip(DEFAULT_SPLITS).any(|(s, d)| (s - d).abs() > 1e-6) {
        return crate::curve::ParametricCurve::new(c).to_lut();
    }
    let amounts = [c.shadows, c.darks, c.lights, c.highlights].map(|v| v as f32);
    let mut lut = Lut1::from_fn(N, |x| {
        let d: f32 = amounts.iter().zip(t::REGIONS.iter()).map(|(a, r)| measured_delta(&t::REGION_AMOUNTS, r, *a, x)).sum();
        (x + d).clamp(0.0, 1.0)
    });
    monotone(&mut lut);
    lut
}

/// The measured Whites curve at `amount`, or `None` at 0.
fn whites(amount: f64) -> Option<Lut1> {
    if amount == 0.0 {
        return None;
    }
    let mut lut = Lut1::from_fn(N, |x| (x + measured_delta(&t::WHITES_AMOUNTS, &t::WHITES, amount as f32, x)).clamp(0.0, 1.0));
    monotone(&mut lut);
    Some(lut)
}

/// Keeps a table non-decreasing (tone order never reverses).
fn monotone(lut: &mut Lut1) {
    let mut m = f32::NEG_INFINITY;
    for v in &mut lut.v {
        m = m.max(*v);
        *v = m;
    }
}

impl CurveStage {
    /// The stage for `s`: `None` before V8, or when Whites, the parametric curve and every point curve are neutral.
    pub fn new(s: &DevelopSettings) -> Option<CurveStage> {
        if s.process.process() < Process::V8 {
            return None;
        }
        let c = &s.curve;
        let parametric_on = c.highlights != 0.0 || c.lights != 0.0 || c.darks != 0.0 || c.shadows != 0.0;
        let master = !ToneCurve::point_curve_is_identity(&c.master);
        let chans = [&c.red, &c.green, &c.blue].map(|p| !ToneCurve::point_curve_is_identity(p));
        let w = whites(s.light.whites);
        if w.is_none() && !parametric_on && !master && !chans.iter().any(|b| *b) {
            return None;
        }
        let to_pts = |p: &[Point]| p.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>();
        let mut base = if parametric_on { parametric(c) } else { Lut1::identity(N) };
        if let Some(w) = &w {
            base = base.compose(w);
        }
        if master {
            base = MonotoneCurve::new(&to_pts(&c.master)).to_lut(N).compose(&base);
        }
        let per = [&c.red, &c.green, &c.blue];
        let chan = std::array::from_fn(|i| match per.get(i) {
            Some(p) if chans.get(i).copied().unwrap_or(false) => MonotoneCurve::new(&to_pts(p)).to_lut(N),
            _ => Lut1::identity(N),
        });
        let curves_on = parametric_on || master || chans.iter().any(|b| *b);
        let refine = if curves_on { (c.refine_saturation / 100.0).clamp(0.0, 1.0) as f32 } else { 1.0 };
        Some(CurveStage { chan, base, refine })
    }

    /// Linear Rec.2020 in and out (display referred, after the tone map). Values above white keep slope 1.
    pub fn apply(&self, d: [f32; 3]) -> [f32; 3] {
        let (to, from) = matrices();
        let p = mul3(to, d);
        let pc = p.map(|v| v.clamp(0.0, 1.0));
        let e = pc.map(linear_to_srgb);
        let (mut hi, mut lo) = (0usize, 0usize);
        for k in 1..3 {
            if e[k] > e[hi] {
                hi = k;
            }
            if e[k] < e[lo] {
                lo = k;
            }
        }
        // the base curve on the largest and smallest channel; the middle one keeps its place in linear light
        let (la, lb) = (srgb_to_linear(self.base.eval(e[hi])), srgb_to_linear(self.base.eval(e[lo])));
        let (pa, pb) = (pc[hi], pc[lo]);
        let curved: [f32; 3] = std::array::from_fn(|k| {
            let t = if pa > pb { (pc[k] - pb) / (pa - pb) } else { 1.0 };
            self.chan[k].eval(linear_to_srgb((lb + (la - lb) * t).clamp(0.0, 1.0)))
        });
        let lin = |v: [f32; 3]| v.map(|x| srgb_to_linear(x.clamp(0.0, 1.0)));
        let mut q = lin(curved);
        if self.refine < 1.0 {
            // the saturation back towards the input's, at the curves' luminance
            let r = lin(crate::finish::refine_saturation(e, curved, self.refine));
            let y = |v: [f32; 3]| PRO_Y[0] * v[0] + PRO_Y[1] * v[1] + PRO_Y[2] * v[2];
            let (yq, yr) = (y(q), y(r));
            q = if yr > 1e-6 { r.map(|v| v * yq / yr) } else { r };
        }
        for (k, v) in q.iter_mut().enumerate() {
            if let Some(over) = p.get(k).filter(|x| **x > 1.0) {
                *v += over - 1.0;
            }
        }
        mul3(from, q)
    }
}

/// Linear Rec.2020 to linear ProPhoto and back, built once.
fn matrices() -> &'static ([[f32; 3]; 3], [[f32; 3]; 3]) {
    static M: std::sync::OnceLock<([[f32; 3]; 3], [[f32; 3]; 3])> = std::sync::OnceLock::new();
    M.get_or_init(crate::light_v3::prophoto_matrices)
}

/// Multiplies a colour by a 3 × 3 matrix.
fn mul3(m: &[[f32; 3]; 3], d: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[r][0] * d[0] + m[r][1] * d[1] + m[r][2] * d[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default settings on process V8.
    fn v8() -> DevelopSettings {
        DevelopSettings { process: Process::V8.version(), ..Default::default() }
    }

    /// V8 settings with only Whites at `v`.
    fn with_whites(v: f64) -> DevelopSettings {
        let mut s = v8();
        s.light.whites = v;
        s
    }

    /// Display-linear grey whose sRGB encoding is `e`, through the stage, back to encoded.
    fn grey(stage: &CurveStage, e: f32) -> f32 {
        linear_to_srgb(stage.apply([srgb_to_linear(e); 3])[1])
    }

    #[test]
    fn earlier_processes_and_neutral_settings_have_no_stage() {
        assert!(CurveStage::new(&v8()).is_none(), "neutral settings are the identity");
        for p in Process::ALL.iter().filter(|p| **p < Process::V8) {
            let s = DevelopSettings { process: p.version(), ..with_whites(-50.0) };
            assert!(CurveStage::new(&s).is_none(), "{p:?}");
        }
    }

    #[test]
    fn whites_reaches_into_the_midtones_as_measured() {
        let stage = CurveStage::new(&with_whites(-50.0)).expect("Whites set");
        let drop = (0.6 - grey(&stage, 0.6)) * 255.0;
        assert!((7.0..13.0).contains(&drop), "encoded 0.6 fell by {drop}/255");
        let dark = (0.05 - grey(&stage, 0.05)).abs() * 255.0;
        assert!(dark < 2.0, "the shadows hardly move: {dark}/255");
        let up = CurveStage::new(&with_whites(50.0)).expect("Whites set");
        assert!(grey(&up, 0.6) > 0.6);
    }

    #[test]
    fn regions_add_and_act_in_their_own_range() {
        let curve = |f: fn(&mut ToneCurve)| {
            let mut s = v8();
            f(&mut s.curve);
            CurveStage::new(&s).expect("a region set")
        };
        let shadows = curve(|c| c.shadows = 50.0);
        let highlights = curve(|c| c.highlights = 50.0);
        let both = curve(|c| {
            c.shadows = 50.0;
            c.highlights = 50.0;
        });
        assert!(grey(&shadows, 0.15) - 0.15 > 3.0 * (grey(&shadows, 0.85) - 0.85).abs(), "Shadows lifts the dark end");
        assert!(grey(&highlights, 0.85) - 0.85 > 3.0 * (grey(&highlights, 0.15) - 0.15).abs(), "Highlights lifts the bright end");
        for e in [0.15f32, 0.4, 0.6, 0.85] {
            let sum = e + (grey(&shadows, e) - e) + (grey(&highlights, e) - e);
            assert!((grey(&both, e) - sum).abs() < 1.5 / 255.0, "{e}");
        }
    }

    #[test]
    fn other_splits_fall_back_to_the_analytic_curve() {
        let mut s = v8();
        s.curve.highlights = 50.0;
        s.curve.split_mid = 40.0;
        let stage = CurveStage::new(&s).expect("a region set");
        assert_eq!(stage.base.v, crate::curve::ParametricCurve::new(&s.curve).to_lut().v);
    }

    #[test]
    fn tone_order_never_reverses_and_every_table_fits_the_gpu() {
        for w in [-100.0, -50.0, 37.0, 100.0] {
            let mut s = with_whites(w);
            s.curve.shadows = -100.0;
            s.curve.lights = 100.0;
            let stage = CurveStage::new(&s).expect("set");
            assert!(stage.base.v.windows(2).all(|p| p[0] <= p[1]), "{w}");
            assert!(stage.chan.iter().chain([&stage.base]).all(|l| l.v.len() == N), "{w}");
        }
    }

    #[test]
    fn the_middle_channel_keeps_its_place_in_linear_prophoto() {
        let stage = CurveStage::new(&with_whites(-100.0)).expect("Whites set");
        let (to, from) = matrices();
        let input = [0.55f32, 0.27, 0.08];
        let out = mul3(to, stage.apply(mul3(from, input)));
        let at = |p: [f32; 3]| (p[1] - p[2]) / (p[0] - p[2]);
        assert!((at(out) - at(input)).abs() < 1e-3, "{out:?}");
    }

    #[test]
    fn colour_curves_act_per_channel_after_the_master() {
        let mut s = v8();
        s.curve.red = vec![Point { x: 0.0, y: 0.0 }, Point { x: 0.5, y: 0.7 }, Point { x: 1.0, y: 1.0 }];
        let stage = CurveStage::new(&s).expect("a red curve");
        let (to, from) = matrices();
        let out = mul3(to, stage.apply(mul3(from, [srgb_to_linear(0.5); 3])));
        assert!((linear_to_srgb(out[0]) - 0.7).abs() < 2e-3, "{out:?}");
        assert!((linear_to_srgb(out[1]) - 0.5).abs() < 2e-3, "{out:?}");
    }

    #[test]
    fn hdr_values_keep_slope_one_above_white() {
        let stage = CurveStage::new(&with_whites(-50.0)).expect("Whites set");
        let (a, b) = (stage.apply([2.0; 3]), stage.apply([3.0; 3]));
        assert!((b[0] - a[0] - 1.0).abs() < 1e-3, "{a:?} {b:?}");
    }

    #[test]
    fn refine_saturation_at_zero_undoes_what_the_curves_added() {
        let mut s = v8();
        s.curve.master = vec![Point { x: 0.0, y: 0.0 }, Point { x: 0.25, y: 0.15 }, Point { x: 0.75, y: 0.85 }, Point { x: 1.0, y: 1.0 }];
        let sat = |stage: &CurveStage| {
            let (to, from) = matrices();
            let p = mul3(to, stage.apply(mul3(from, [0.5, 0.25, 0.12]))).map(linear_to_srgb);
            (p[0] - p[2]) / (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
        };
        let full = sat(&CurveStage::new(&s).expect("a curve"));
        s.curve.refine_saturation = 0.0;
        let none = sat(&CurveStage::new(&s).expect("a curve"));
        assert!(none < full, "{none} {full}");
    }
}
