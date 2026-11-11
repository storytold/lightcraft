//! Process V9's Colour Grading ([`lightcraft_develop::Process::V9`], `docs/grading-stage.md`), applied the way
//! Lightroom applies it.
//!
//! Measured on Camera Raw renders of synthetic charts (`tools/lr_grading_charts.py`, tables in
//! `grading_stage_tables`). Per wheel (shadows, midtones, highlights, global):
//! - an OkLab tint (a, b) by wheel hue (12 steps of 30°) and saturation (25, 50, 100);
//! - a lightness profile, the share of the tint each lightness gets, by Blending and Balance. Lightroom's shadows
//!   wheel reaches well into the midtones and its global wheel peaks in the midtones;
//! - a damping: colours that already lean the tint's way get less of it (55 to 87% for saturated colours in the same
//!   direction), by their chroma and the angle between their hue and the tint's;
//! - the wheel's Luminance as a lightness change by lightness.
//!
//! The wheels act on the same input colour and their changes add. The stage replaces V8's grading at the end of
//! [`crate::colorops::ColorOps`], in the same place.

use lightcraft_develop::{ColorGrading, DevelopSettings, Process, Wheel};

use crate::grading_stage_tables as t;

/// Lightness grid entries (0 to 1).
pub const GRID: usize = 65;

/// One active wheel, resolved for a render.
#[derive(Clone, Debug)]
pub struct WheelStage {
    /// OkLab (a, b) at full share.
    pub tint: [f32; 2],
    /// Share of the tint by lightness.
    pub profile: [f32; GRID],
    /// Share by chroma level and angle bin (see [`t::DAMPING`]).
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

/// Linear blend of two lightness grids, `f` = 0 gives `a`.
fn lerp_grid(a: &[f32; GRID], b: &[f32; GRID], f: f32) -> [f32; GRID] {
    std::array::from_fn(|i| a[i] * (1.0 - f) + b[i] * f)
}

/// The tint of wheel `w` at `hue` (degrees) and `sat` (0..100): linear in hue around the circle, and in saturation
/// from none at 0 through the measured saturations.
fn tint(w: usize, hue: f32, sat: f32) -> [f32; 2] {
    let Some(table) = t::TINT.get(w) else { return [0.0; 2] };
    let n = t::HUES.len();
    let step = 360.0 / n as f32;
    let h = hue.rem_euclid(360.0);
    let i = (h / step) as usize % n;
    let fh = h / step - (h / step).floor();
    let at = |hi: usize| -> [f32; 2] {
        let s = sat.clamp(0.0, t::SATS[t::SATS.len() - 1]);
        let row = &table[hi % n];
        let k = t::SATS.iter().position(|v| s <= *v).unwrap_or(t::SATS.len() - 1);
        let (s0, v0) = if k == 0 { (0.0, [0.0; 2]) } else { (t::SATS[k - 1], row[k - 1]) };
        let (s1, v1) = (t::SATS[k], row[k]);
        let f = if s1 > s0 { (s - s0) / (s1 - s0) } else { 1.0 };
        [v0[0] + (v1[0] - v0[0]) * f, v0[1] + (v1[1] - v0[1]) * f]
    };
    let (a, b) = (at(i), at(i + 1));
    [a[0] * (1.0 - fh) + b[0] * fh, a[1] * (1.0 - fh) + b[1] * fh]
}

/// The share of wheel `w`'s tint by lightness: the Blending profile, shifted by Balance (not for the global wheel).
fn profile(w: usize, blending: f32, balance: f32) -> [f32; GRID] {
    let (Some(pb), Some(pa)) = (t::PROFILE_BLEND.get(w), t::PROFILE_BALANCE.get(w)) else { return [0.0; GRID] };
    let (b0, b1, fb) = bracket(&t::BLENDS, blending);
    let blend = lerp_grid(&pb[b0], &pb[b1], fb);
    if w == 3 {
        return blend.map(|v| v.max(0.0));
    }
    let (a0, a1, fa) = bracket(&t::BALANCES, balance);
    let bal = lerp_grid(&pa[a0], &pa[a1], fa);
    let zero = t::BALANCES.iter().position(|v| *v == 0.0).unwrap_or(0);
    std::array::from_fn(|i| (blend[i] + bal[i] - pa[zero][i]).max(0.0))
}

/// Wheel `w`'s Luminance `v` as a lightness change by lightness (measured at ±50 and ±100, 0 in between).
fn luminance(w: usize, v: f32) -> [f32; GRID] {
    let Some(table) = t::LUM.get(w) else { return [0.0; GRID] };
    let mut keys: Vec<(f32, Option<&[f32; GRID]>)> = t::LUMS.iter().zip(table.iter()).map(|(a, c)| (*a, Some(c))).collect();
    keys.push((0.0, None));
    keys.sort_by(|a, b| a.0.total_cmp(&b.0));
    let amounts: Vec<f32> = keys.iter().map(|k| k.0).collect();
    let (i0, i1, f) = bracket(&amounts, v);
    let zero = [0.0; GRID];
    let at = |i: usize| keys.get(i).and_then(|k| k.1).unwrap_or(&zero);
    lerp_grid(at(i0), at(i1), f)
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
                    damping: t::DAMPING.get(w).copied().unwrap_or([[1.0; 6]; 3]),
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
            let bin = ((diff.abs().to_degrees() / 30.0) as usize).min(5);
            let level = |k: usize| w.damping.get(k).and_then(|r| r.get(bin)).copied().unwrap_or(1.0);
            let damp = if c <= t::CHROMA[0] {
                1.0 + (level(0) - 1.0) * c / t::CHROMA[0]
            } else {
                let (k0, k1, f) = bracket(&t::CHROMA, c);
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
        assert!(size(mid) > 0.3 * size(dark), "Lightroom's shadows wheel reaches the midtones: {mid:?} vs {dark:?}");
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
        assert!((s10 - 0.4 * s25).abs() < 1e-4, "below 25 the tint grows linearly from none");
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
