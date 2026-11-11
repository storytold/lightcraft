//! Process V9's tone-curve stage ([`lightcraft_develop::Process::V9`], `docs/curve-stage.md`): Whites, the
//! parametric curve and the point curves, applied the way Lightroom applies them.
//!
//! Measured on Camera Raw renders of synthetic charts (`tools/lr_tone_charts.py`):
//! - Whites, the parametric curve, the master point curve, Contrast and Blacks act hue-preserving on linear ProPhoto
//!   RGB: the curve maps the largest and the smallest channel (on sRGB-encoded values), and the middle channel keeps
//!   its relative position between them in linear light. Predicting coloured patches from the curve measured on
//!   grey steps this way leaves 0.03 to 0.16 (OkLab × 100), against 0.3 to 3.4 for V8.
//! - The red, green and blue curves act per channel on sRGB-encoded linear ProPhoto RGB, after the master curve.
//! - Whites and the four parametric regions are tables of Camera Raw's measured output (`curve_stage_tables`):
//!   Whites reaches into the midtones (Whites −50 lowers encoded 0.6 by about 10/255), and each region is broad and
//!   overlapping. The regions add: the sum of single regions reproduces three at once within 1/255.
//!
//! The stage runs after the tone map and Process V4's calibrated Contrast and Blacks, before the colour stage. It
//! replaces V8's curves on encoded output values; the tone map leaves Whites to it.

use lightcraft_color::spline::{Lut1, MonotoneCurve};
use lightcraft_color::transfer::{linear_to_srgb, srgb_to_linear};
use lightcraft_develop::{DevelopSettings, Process, ToneCurve};
use lightcraft_geom::Point;

use crate::curve::ParametricCurve;
use crate::curve_stage_tables as t;
use crate::light_v3::PRO_Y;

/// Entries per table (as the shipping curve tables).
pub const N: usize = 1024;

/// The least a region must be able to move the drawn parametric curve for a drag to move its slider: half an 8-bit level.
const REACH_MIN: f64 = 0.5 / 255.0;

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

/// The delta at `x` of a setting measured at `amounts` (ascending, without 0), linearly between the measured amounts
/// and towards 0 below the smallest; clamped to the measured range. Runs per table sample, so it allocates nothing.
fn measured_delta<const K: usize>(amounts: &[f32; K], tables: &[[f32; 257]; K], amount: f32, x: f32) -> f32 {
    if amount == 0.0 || K == 0 {
        return 0.0;
    }
    // the measured amounts with 0 (delta 0) between the negative and positive ones: key `i` of K + 1
    let zero = amounts.partition_point(|a| *a < 0.0);
    let key = |i: usize| -> (f32, f32) {
        let k = match i.cmp(&zero) {
            std::cmp::Ordering::Less => i,
            std::cmp::Ordering::Equal => return (0.0, 0.0),
            std::cmp::Ordering::Greater => i - 1,
        };
        (amounts.get(k).copied().unwrap_or(0.0), tables.get(k).map_or(0.0, |v| table_at(v, x)))
    };
    let (lo, hi) = (key(0).0, key(K).0);
    let a = amount.clamp(lo, hi);
    for i in 0..K {
        let ((a0, v0), (a1, v1)) = (key(i), key(i + 1));
        if a >= a0 && a <= a1 {
            let f = if a1 > a0 { (a - a0) / (a1 - a0) } else { 0.0 };
            return v0 * (1.0 - f) + v1 * f;
        }
    }
    0.0
}

/// Whether `c` holds Lightroom's default splits, the ones the region tables are measured with.
fn default_splits(c: &ToneCurve) -> bool {
    [c.split_shadows, c.split_mid, c.split_highlights].iter().zip(DEFAULT_SPLITS).all(|(s, d)| (s - d).abs() <= 1e-6)
}

/// The measured regions at `x`, before the curve is kept monotone: the four deltas added to the diagonal.
fn regions_at(amounts: [f32; 4], x: f32) -> f32 {
    (x + regions_delta(amounts, x)).clamp(0.0, 1.0)
}

/// The sum of the four measured region deltas at `x`.
fn regions_delta(amounts: [f32; 4], x: f32) -> f32 {
    amounts.iter().zip(t::REGIONS.iter()).map(|(a, r)| measured_delta(&t::REGION_AMOUNTS, r, *a, x)).sum()
}

/// The measured parametric curve at the default splits.
fn measured_parametric(amounts: [f32; 4]) -> Lut1 {
    let mut lut = Lut1::from_fn(N, |x| regions_at(amounts, x));
    monotone(&mut lut);
    lut
}

/// The parametric curve: the measured regions at the default splits, else the analytic model of earlier processes.
fn parametric(c: &ToneCurve) -> Lut1 {
    if !default_splits(c) {
        return ParametricCurve::new(c).to_lut();
    }
    measured_parametric([c.shadows, c.darks, c.lights, c.highlights].map(|v| v as f32))
}

/// The parametric curve as `s`'s process renders it, for the Edit panel's graph and its drag: the measured regions
/// under V9 at the default splits, else the analytic [`ParametricCurve`] (earlier processes, and V9 with moved
/// splits). Whites is left out, as Lightroom's graph leaves it out.
#[derive(Clone, Debug)]
pub struct ParametricGraph {
    model: ParametricCurve,
    /// The measured curve, when it is the one rendered.
    measured: Option<Lut1>,
}

impl ParametricGraph {
    /// The graph for `s`: measured when `s`'s process is V9 or later and the splits are the defaults.
    pub fn new(s: &DevelopSettings) -> ParametricGraph {
        let model = ParametricCurve::new(&s.curve);
        let measured = (s.process.process() >= Process::V9 && default_splits(&s.curve)).then(|| measured_parametric(model.amounts.map(|a| a as f32)));
        ParametricGraph { model, measured }
    }

    /// Whether the graph is the measured curve (else the analytic one).
    pub fn is_measured(&self) -> bool {
        self.measured.is_some()
    }

    /// The curve at `x` (0 = black, 1 = white).
    pub fn at(&self, x: f64) -> f64 {
        match &self.measured {
            Some(lut) => lut.eval(if x.is_nan() { 0.0 } else { x as f32 }) as f64,
            None => self.model.at(x),
        }
    }

    /// The region that owns graph position `x` (the splits tile the graph: [`ParametricCurve::region_at`]).
    pub fn region_at(&self, x: f64) -> usize {
        self.model.region_at(x)
    }

    /// Region `region`'s slider amount.
    pub fn amount(&self, region: usize) -> f64 {
        self.model.amounts.get(region).copied().unwrap_or(0.0)
    }

    /// The amount for region `region` after a drag that grabbed the curve at `x`, with the region at `from`, and moved
    /// it `delta` in output level (a fraction of the graph's height; up is positive). The other regions stay as they
    /// are, and the level is the drawn curve's (kept monotone). A level the region cannot reach saturates at ±100; no
    /// motion, or a region that cannot visibly move the line in that direction, keeps `from`.
    pub fn dragged_amount(&self, region: usize, x: f64, from: f64, delta: f64) -> f64 {
        if delta == 0.0 || !delta.is_finite() {
            return from;
        }
        if self.measured.is_none() {
            let per_unit = self.model.level_per_unit(region, x);
            return if per_unit.abs() > f64::EPSILON { (from + delta / per_unit).clamp(-100.0, 100.0).round() } else { from };
        }
        // The measured regions are tables, not linear in the amount: search the amount that puts the drawn curve at the
        // target, on the side of `from` the drag goes to (so a flat stretch never makes the slider jump back). The drawn
        // curve is kept monotone, a running maximum, so its level at `x` reads the table entries up to `x`; the other
        // regions' share of them is the same for every amount tried.
        let Some(table) = t::REGIONS.get(region) else { return from };
        let pos = x.clamp(0.0, 1.0) as f32 * (N - 1) as f32;
        let j = (pos as usize).min(N - 2);
        let f = pos - j as f32;
        let mut others = self.model.amounts.map(|v| v as f32);
        if let Some(v) = others.get_mut(region) {
            *v = 0.0;
        }
        let entries: Vec<(f32, f32)> = (0..=j + 1)
            .map(|i| {
                let g = i as f32 / (N - 1) as f32;
                (g, regions_delta(others, g))
            })
            .collect();
        let level = |a: f64| {
            let (mut m, mut at) = (f32::NEG_INFINITY, [0.0f32; 2]);
            for (i, (g, rest)) in entries.iter().enumerate() {
                m = m.max((g + rest + measured_delta(&t::REGION_AMOUNTS, table, a as f32, *g)).clamp(0.0, 1.0));
                if let Some(v) = i.checked_sub(j).and_then(|k| at.get_mut(k)) {
                    *v = m;
                }
            }
            (at[0] + (at[1] - at[0]) * f) as f64
        };
        let (start, end) = (level(from), if delta > 0.0 { 100.0 } else { -100.0 });
        let (target, reach) = (start + delta, level(end));
        // where the drawn line is held by a peak to its left, the region may hardly move it: then the slider stays
        if (reach - start).abs() < REACH_MIN {
            return from;
        }
        if (delta > 0.0 && reach < target) || (delta < 0.0 && reach > target) {
            return end;
        }
        let (mut lo, mut hi) = if delta > 0.0 { (from, 100.0) } else { (-100.0, from) };
        // 20 halvings of the 200-wide range are well inside the rounding to whole amounts
        for _ in 0..20 {
            let mid = (lo + hi) / 2.0;
            if level(mid) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let a = if delta > 0.0 { hi } else { lo };
        a.clamp(-100.0, 100.0).round()
    }
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
    /// The stage for `s`: `None` before V9, or when Whites, the parametric curve and every point curve are neutral.
    pub fn new(s: &DevelopSettings) -> Option<CurveStage> {
        if s.process.process() < Process::V9 {
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

    /// Default settings on process V9.
    fn v9() -> DevelopSettings {
        DevelopSettings { process: Process::V9.version(), ..Default::default() }
    }

    /// V9 settings with only Whites at `v`.
    fn with_whites(v: f64) -> DevelopSettings {
        let mut s = v9();
        s.light.whites = v;
        s
    }

    /// Display-linear grey whose sRGB encoding is `e`, through the stage, back to encoded.
    fn grey(stage: &CurveStage, e: f32) -> f32 {
        linear_to_srgb(stage.apply([srgb_to_linear(e); 3])[1])
    }

    #[test]
    fn earlier_processes_and_neutral_settings_have_no_stage() {
        assert!(CurveStage::new(&v9()).is_none(), "neutral settings are the identity");
        for p in Process::ALL.iter().filter(|p| **p < Process::V9) {
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
            let mut s = v9();
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
        let mut s = v9();
        s.curve.highlights = 50.0;
        s.curve.split_mid = 40.0;
        let stage = CurveStage::new(&s).expect("a region set");
        assert_eq!(stage.base.v, crate::curve::ParametricCurve::new(&s.curve).to_lut().v);
    }

    /// The earlier implementation, which built and sorted the keys per call.
    fn measured_delta_sorted<const K: usize>(amounts: &[f32; K], tables: &[[f32; 257]; K], amount: f32, x: f32) -> f32 {
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

    #[test]
    fn measured_delta_reads_the_tables_as_the_sorting_version_did() {
        assert!(t::REGION_AMOUNTS.windows(2).all(|w| w[0] < w[1]) && t::WHITES_AMOUNTS.windows(2).all(|w| w[0] < w[1]));
        for amount in [-150.0f32, -100.0, -80.0, -75.0, -30.0, -1.0, 0.0, 0.5, 12.0, 25.0, 60.0, 100.0, 140.0] {
            for i in 0..=40 {
                let x = i as f32 / 40.0;
                for r in &t::REGIONS {
                    assert_eq!(measured_delta(&t::REGION_AMOUNTS, r, amount, x), measured_delta_sorted(&t::REGION_AMOUNTS, r, amount, x));
                }
                assert_eq!(
                    measured_delta(&t::WHITES_AMOUNTS, &t::WHITES, amount, x),
                    measured_delta_sorted(&t::WHITES_AMOUNTS, &t::WHITES, amount, x)
                );
            }
        }
    }

    /// What the graph draws is the stage's base curve, and the analytic one where the stage uses that.
    #[test]
    fn the_graph_is_the_curve_the_stage_renders() {
        let mut s = v9();
        (s.curve.shadows, s.curve.darks, s.curve.lights, s.curve.highlights) = (40.0, -30.0, 25.0, -60.0);
        let graph = ParametricGraph::new(&s);
        assert!(graph.is_measured());
        let stage = CurveStage::new(&s).expect("regions set");
        for i in 0..=64 {
            let x = i as f32 / 64.0;
            assert!((graph.at(x as f64) as f32 - stage.base.eval(x)).abs() < 1e-6, "{x}");
        }
        // moved splits and earlier processes draw the analytic curve, as they render it
        s.curve.split_mid = 40.0;
        let moved = ParametricGraph::new(&s);
        assert!(!moved.is_measured());
        assert_eq!(moved.at(0.6), ParametricCurve::new(&s.curve).at(0.6));
        s.curve.split_mid = 50.0;
        let v8 = DevelopSettings { process: Process::V8.version(), ..s };
        assert!(!ParametricGraph::new(&v8).is_measured());
    }

    /// In every region, the amount a drag picks moves the drawn curve by the drag's motion.
    #[test]
    fn a_graph_drag_puts_the_measured_curve_where_the_hand_is() {
        let mut s = v9();
        s.curve.darks = 30.0;
        let graph = ParametricGraph::new(&s);
        for (region, x) in [(0usize, 0.15f64), (1, 0.375), (2, 0.625), (3, 0.85)] {
            for delta in [-0.04, 0.03] {
                let from = graph.amount(region);
                let a = graph.dragged_amount(region, x, from, delta);
                let mut after = s.clone();
                let c = &mut after.curve;
                *[&mut c.shadows, &mut c.darks, &mut c.lights, &mut c.highlights][region] = a;
                let got = ParametricGraph::new(&after).at(x) - graph.at(x);
                assert!((got - delta).abs() < 0.004, "region {region} at {x}: moved {got}, wanted {delta} (amount {from} → {a})");
            }
            assert_eq!(graph.dragged_amount(region, x, graph.amount(region), 0.0), graph.amount(region), "no motion, no change");
            assert_eq!(graph.dragged_amount(region, x, 0.0, 1.0), 100.0, "out of reach saturates high");
            assert_eq!(graph.dragged_amount(region, x, 0.0, -1.0), -100.0, "and low");
        }
    }

    /// Where opposite regions fold the measured curve back, the drag follows the curve as drawn (held flat by the peak
    /// to its left): Lights +100 and Highlights −100 fold it above 0.75.
    #[test]
    fn a_graph_drag_follows_the_drawn_curve_where_it_is_flattened() {
        let mut s = v9();
        (s.curve.shadows, s.curve.darks, s.curve.lights, s.curve.highlights) = (-50.0, 100.0, 100.0, -100.0);
        let graph = ParametricGraph::new(&s);
        let x = 0.86;
        let raw = regions_at(graph.model.amounts.map(|v| v as f32), x as f32) as f64;
        assert!(graph.at(x) - raw > 0.1, "the stretch is flattened, or this proves nothing: {} vs {raw}", graph.at(x));
        let drawn = |region: usize, a: f64| {
            let mut after = s.clone();
            let c = &mut after.curve;
            *[&mut c.shadows, &mut c.darks, &mut c.lights, &mut c.highlights][region] = a;
            ParametricGraph::new(&after).at(x)
        };
        // Lights lowers the peak that holds the line: the drawn line moves with the hand
        let a = graph.dragged_amount(2, x, 100.0, -0.03);
        assert!((drawn(2, a) - graph.at(x) + 0.03).abs() < 0.004, "Lights {a} moved the line {}", drawn(2, a) - graph.at(x));
        // Highlights hardly moves the held line at all, so its slider stays rather than running to the end
        assert!(drawn(3, 100.0) - graph.at(x) < REACH_MIN, "Highlights cannot lift the held line");
        assert_eq!(graph.dragged_amount(3, x, -100.0, 0.03), -100.0);
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
        let mut s = v9();
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
        let mut s = v9();
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
