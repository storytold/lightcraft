//! Process V9's Colour Grading ([`lightcraft_develop::Process::V9`]), from a fitted model (`grading_stage_model`,
//! written by `tools/lr_grading_charts.py fit`). `docs/grading-stage.md` records how it was measured and fitted. Per
//! wheel (shadows, midtones, highlights, global):
//! - an OkLab tint (a, b) by wheel hue and saturation: one smooth shape around the hue circle shared by the wheels,
//!   a size per wheel and a saturation curve per wheel;
//! - a lightness profile, the share of the tint each lightness gets, by Blending and Balance. The shadows wheel
//!   reaches well into the midtones and the global wheel peaks in the midtones;
//! - a damping: colours that already lean the tint's way get less of it (55 to 87% for saturated colours in the same
//!   direction), by their chroma and the angle between their hue and the tint's;
//! - the wheel's Luminance as a lightness change by lightness.
//!
//! The wheels act on the same input colour and their changes add. The stage replaces V8's grading at the end of
//! [`crate::colorops::ColorOps`], in the same place.

use lightcraft_develop::{ColorGrading, DevelopSettings, Process, Wheel};

use crate::grading_stage_model as m;

/// The damping's chroma levels: the chart's colour patches (`tools/lr_grading_charts.py`), where the stage samples
/// the model.
pub const CHROMA: [f32; 3] = [0.04, 0.08, 0.12];

/// Lightness grid entries (0 to 1).
pub const GRID: usize = 65;

/// One active wheel, resolved for a render.
#[derive(Clone, Debug)]
pub struct WheelStage {
    /// OkLab (a, b) at full share.
    pub tint: [f32; 2],
    /// Share of the tint by lightness.
    pub profile: [f32; GRID],
    /// Share by chroma level ([`CHROMA`]) and angle (6 bins of 30°, sampled at their centres).
    pub damping: [[f32; 6]; 3],
}

#[derive(Clone, Debug)]
pub struct GradingStage {
    /// Wheels with a saturation above 0.
    pub wheels: Vec<WheelStage>,
    /// Every wheel's Luminance together: the lightness change by lightness.
    pub lum: [f32; GRID],
}

/// `v` on a lightness grid, linear, clamped to 0..1.
fn on_grid(v: &[f32; GRID], l: f32) -> f32 {
    let p = l.clamp(0.0, 1.0) * (GRID - 1) as f32;
    let i = (p as usize).min(GRID - 2);
    match (v.get(i), v.get(i + 1)) {
        (Some(a), Some(b)) => a + (b - a) * (p - i as f32),
        _ => 0.0,
    }
}

/// (lower index, upper index, weight of the upper) of `x` in the sorted `keys`, clamped.
fn bracket(keys: &[f32], x: f32) -> (usize, usize, f32) {
    let n = keys.len();
    match (keys.first(), keys.last()) {
        (Some(lo), Some(hi)) if n >= 2 && x > *lo => {
            if x >= *hi {
                return (n - 1, n - 1, 0.0);
            }
            let i = keys.windows(2).position(|w| x <= w[1]).unwrap_or(0);
            (i, i + 1, (x - keys[i]) / (keys[i + 1] - keys[i]))
        }
        _ => (0, 0, 0.0),
    }
}

/// `c0 + Σ c[2k−1] cos(k h) + c[2k] sin(k h)`.
fn fourier(c: &[f64], h: f64) -> f64 {
    let (first, rest) = c.split_first().map_or((0.0, &[][..]), |(f, r)| (*f, r));
    first + rest.as_chunks::<2>().0.iter().enumerate().map(|(k, [c, s])| c * ((k + 1) as f64 * h).cos() + s * ((k + 1) as f64 * h).sin()).sum::<f64>()
}

/// A bump of peak 1 at `p / (p + q)`, 0 at black and white and outside them.
fn bump(x: f64, p: f64, q: f64) -> f64 {
    if x <= 0.0 || x >= 1.0 {
        return 0.0;
    }
    let peak = p / (p + q);
    (p * (x / peak).ln() + q * ((1.0 - x) / (1.0 - peak)).ln()).exp()
}

/// The tint of wheel `w` at `hue` (degrees) and `sat` (0..100): the shared direction and size around the hue circle,
/// the wheel's size and its saturation curve `s (1 + g1 (s − 1) + g2 (s − 1)²)`, `s` = sat / 100.
fn tint(w: usize, hue: f32, sat: f32) -> [f32; 2] {
    let (Some(scale), Some([g1, g2])) = (m::WHEEL_SIZE.get(w), m::WHEEL_SAT.get(w)) else { return [0.0; 2] };
    let h = f64::from(hue).to_radians();
    let s = (f64::from(sat) / 100.0).clamp(0.0, 1.0);
    let angle = h + fourier(&m::TINT_ANGLE, h);
    let size = (fourier(&m::TINT_SIZE, h) + scale).exp() * s * (1.0 + g1 * (s - 1.0) + g2 * (s - 1.0) * (s - 1.0));
    [(size * angle.cos()) as f32, (size * angle.sin()) as f32]
}

/// Two bumps whose gain and exponents are quadratic in `b` and `c`, at lightness `l` ([`m::SHARE`]).
fn share(p: &[f64; 30], b: f64, c: f64, l: f64) -> f64 {
    p.as_chunks::<15>()
        .0
        .iter()
        .map(|q| {
            let f = |i: usize| q[i] + q[i + 1] * b + q[i + 2] * b * b + q[i + 3] * c + q[i + 4] * c * c;
            f(0) * bump(l, f(5).exp(), f(10).exp())
        })
        .sum()
}

/// The share of wheel `w`'s tint by lightness, at Blending and Balance (the global wheel ignores Balance).
fn profile(w: usize, blending: f32, balance: f32) -> [f32; GRID] {
    let Some(p) = m::SHARE.get(w) else { return [0.0; GRID] };
    let b = f64::from(blending.clamp(0.0, 100.0)) / 100.0 - 0.5;
    let c = if w == 3 { 0.0 } else { f64::from(balance.clamp(-100.0, 100.0)) / 100.0 };
    std::array::from_fn(|i| share(p, b, c, i as f64 / (GRID - 1) as f64).max(0.0) as f32)
}

/// Wheel `w`'s Luminance `v` (−100..100) as a lightness change by lightness: per direction two bumps, gain and
/// exponents changing with the amount ([`m::LUM`]).
fn luminance(w: usize, v: f32) -> [f32; GRID] {
    let Some(p) = m::LUM.get(w) else { return [0.0; GRID] };
    let t = (f64::from(v) / 100.0).clamp(-1.0, 1.0);
    let (a, side) = (t.abs(), if t < 0.0 { 0 } else { 12 });
    std::array::from_fn(|i| {
        let l = i as f64 / (GRID - 1) as f64;
        let r: f64 = p[side..side + 12]
            .as_chunks::<6>()
            .0
            .iter()
            .map(|[g1, g2, p0, p1, q0, q1]| (g1 * a + g2 * a * a) * bump(l, (p0 + p1 * a).exp(), (q0 + q1 * a).exp()))
            .sum();
        (t.signum() * r) as f32
    })
}

/// Wheel `w`'s damping by chroma level and angle bin: `1 + (C / 0.12)^e (d0 + d1 cos a + d2 cos 2a)` at the bins'
/// centres (15°, 45° … 165°).
fn damping(w: usize) -> [[f32; 6]; 3] {
    let Some([d0, d1, d2, e]) = m::DAMPING.get(w).copied() else { return [[1.0; 6]; 3] };
    std::array::from_fn(|k| {
        let c = f64::from(CHROMA.get(k).copied().unwrap_or(0.0)) / 0.12;
        std::array::from_fn(|j| {
            let a = (15.0 + 30.0 * j as f64).to_radians();
            (1.0 + c.powf(e.exp()) * (d0 + d1 * a.cos() + d2 * (2.0 * a).cos())) as f32
        })
    })
}

impl GradingStage {
    /// The stage for `s`: `None` before V9 or when Colour Grading is neutral.
    pub fn new(s: &DevelopSettings) -> Option<GradingStage> {
        if s.process.process() < Process::V9 || s.grading.is_neutral() {
            return None;
        }
        let g: &ColorGrading = &s.grading;
        let wheels: [&Wheel; 4] = [&g.shadows, &g.midtones, &g.highlights, &g.global];
        let (blending, balance) = (g.blending as f32, g.balance as f32);
        let mut lum = [0.0f32; GRID];
        let mut out = Vec::new();
        for (w, wheel) in wheels.iter().enumerate() {
            if wheel.sat > 0.0 && wheel.sat.is_finite() {
                out.push(WheelStage {
                    tint: tint(w, wheel.hue as f32, wheel.sat as f32),
                    profile: profile(w, blending, balance),
                    damping: damping(w),
                });
            }
            if wheel.lum != 0.0 && wheel.lum.is_finite() {
                let l = luminance(w, wheel.lum as f32);
                lum.iter_mut().zip(l).for_each(|(a, b)| *a += b);
            }
        }
        Some(GradingStage { wheels: out, lum })
    }

    /// Colour Grading on OkLab.
    pub fn apply(&self, lab: [f32; 3]) -> [f32; 3] {
        let [l0, a0, b0] = lab;
        let c = (a0 * a0 + b0 * b0).sqrt();
        let hue = b0.atan2(a0);
        let (mut a, mut b) = (a0, b0);
        for w in &self.wheels {
            let p = on_grid(&w.profile, l0);
            if p == 0.0 {
                continue;
            }
            let diff = (hue - w.tint[1].atan2(w.tint[0]) + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            // each angle bin is the mean over 30°: linear between the bins' centres (15°, 45° … 165°), so the
            // damping has no step along a hue gradient
            let pos = (diff.abs().to_degrees() / 30.0 - 0.5).clamp(0.0, 5.0);
            let j = (pos as usize).min(4);
            let fa = pos - j as f32;
            let level = |k: usize| w.damping.get(k).map_or(1.0, |r| r[j] * (1.0 - fa) + r[j + 1] * fa);
            let damp = if c <= CHROMA[0] {
                1.0 + (level(0) - 1.0) * c / CHROMA[0]
            } else {
                let (k0, k1, f) = bracket(&CHROMA, c);
                level(k0) * (1.0 - f) + level(k1) * f
            };
            a += p * w.tint[0] * damp;
            b += p * w.tint[1] * damp;
        }
        [(l0 + on_grid(&self.lum, l0)).max(0.0), a, b]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default settings on process V9.
    fn v9() -> DevelopSettings {
        DevelopSettings { process: Process::V9.version(), ..Default::default() }
    }

    /// The stage for V9 settings with Colour Grading set by `f`.
    fn stage(f: impl Fn(&mut ColorGrading)) -> GradingStage {
        let mut s = v9();
        f(&mut s.grading);
        GradingStage::new(&s).expect("stage")
    }

    #[test]
    fn only_v8_with_grading_gets_the_stage() {
        assert!(GradingStage::new(&v9()).is_none());
        let mut s = v9();
        s.grading.shadows.hue = 200.0;
        assert!(GradingStage::new(&s).is_none(), "a hue without saturation is neutral");
        s.grading.shadows.sat = 30.0;
        assert!(GradingStage::new(&s).is_some());
        s.process = Process::V7.version();
        assert!(GradingStage::new(&s).is_none());
    }

    #[test]
    fn the_shadows_wheel_tints_greys_and_reaches_into_the_midtones() {
        // a warm shadows tint (wheel hue 30)
        let st = stage(|g| {
            g.shadows.hue = 30.0;
            g.shadows.sat = 60.0;
        });
        let dark = st.apply([0.3, 0.0, 0.0]);
        let mid = st.apply([0.6, 0.0, 0.0]);
        let bright = st.apply([0.95, 0.0, 0.0]);
        let size = |x: [f32; 3]| x[1].hypot(x[2]);
        assert!(dark[1] > 0.0 && dark[2] > 0.0, "warm: {dark:?}");
        assert!(size(mid) > 0.3 * size(dark), "the shadows wheel reaches the midtones: {mid:?} vs {dark:?}");
        assert!(size(bright) < 0.3 * size(dark), "{bright:?}");
        assert_eq!(dark[0], 0.3, "the tint leaves lightness alone");
    }

    #[test]
    fn colours_leaning_the_tints_way_get_less_of_it() {
        let st = stage(|g| {
            g.global.hue = 30.0;
            g.global.sat = 100.0;
        });
        let grey = st.apply([0.6, 0.0, 0.0]);
        let t = [grey[1], grey[2]];
        let n = t[0].hypot(t[1]);
        let along = [0.1 * t[0] / n, 0.1 * t[1] / n];
        let same = st.apply([0.6, along[0], along[1]]);
        let gain = ((same[1] - along[0]) * t[0] + (same[2] - along[1]) * t[1]) / (n * n);
        assert!(gain < 0.95 && gain > 0.3, "{gain}");
    }

    #[test]
    fn the_damping_has_no_step_along_a_hue_gradient() {
        // shadows at chroma 0.12, where the damping changes most with the angle
        let st = stage(|g| {
            g.shadows.hue = 30.0;
            g.shadows.sat = 100.0;
        });
        let grey = st.apply([0.3, 0.0, 0.0]);
        let tint = grey[2].atan2(grey[1]);
        let added = |deg: f32| {
            let h = tint + deg.to_radians();
            let lab = [0.3, 0.12 * h.cos(), 0.12 * h.sin()];
            let out = st.apply(lab);
            (out[1] - lab[1]).hypot(out[2] - lab[2])
        };
        let mut prev = added(0.0);
        for i in 1..=360 {
            let now = added(i as f32 * 0.5);
            assert!((now - prev).abs() < 0.02 * prev, "{}°: {prev} → {now}", i as f32 * 0.5);
            prev = now;
        }
    }

    #[test]
    fn saturation_and_hue_interpolate() {
        let size = |sat: f64, hue: f64| {
            let st = stage(|g| {
                g.midtones.hue = hue;
                g.midtones.sat = sat;
            });
            let x = st.apply([0.5, 0.0, 0.0]);
            (x[1].hypot(x[2]), x[2].atan2(x[1]))
        };
        let (s10, _) = size(10.0, 120.0);
        let (s25, h25) = size(25.0, 120.0);
        let (s75, _) = size(75.0, 120.0);
        assert!(s10 < s25 && s25 < s75, "{s10} {s25} {s75}");
        let (_, h_mid) = size(25.0, 135.0);
        let (_, h150) = size(25.0, 150.0);
        assert!((h25.min(h150)..=h25.max(h150)).contains(&h_mid), "{h25} {h_mid} {h150}");
    }

    #[test]
    fn luminance_moves_lightness_and_nothing_else() {
        let st = stage(|g| g.highlights.lum = 60.0);
        assert!(st.wheels.is_empty());
        let x = st.apply([0.85, 0.02, -0.01]);
        assert!(x[0] > 0.85, "{x:?}");
        assert_eq!([x[1], x[2]], [0.02, -0.01]);
        let st = stage(|g| g.highlights.lum = -100.0);
        assert!(st.apply([0.85, 0.0, 0.0])[0] < 0.85);
    }

    #[test]
    fn balance_moves_the_split() {
        let at = |balance: f64| {
            let st = stage(|g| {
                g.shadows.hue = 220.0;
                g.shadows.sat = 50.0;
                g.balance = balance;
            });
            let x = st.apply([0.55, 0.0, 0.0]);
            x[1].hypot(x[2])
        };
        // Balance towards the highlights narrows the shadows wheel's reach
        assert!(at(100.0) < at(0.0) && at(0.0) < at(-100.0), "{} {} {}", at(100.0), at(0.0), at(-100.0));
    }
}
