//! Dial ticks to value changes: speed, the acceleration curve, modifiers and direction.
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT: the acceleration curve, its
//! clamps and the speed model are TactileBridge's `CaptureOneAdjustment.GetAcceleration` and
//! `ApplyAdjustment`.

use std::collections::BTreeMap;

use crate::binding::Modifier;
use crate::settings::{MotionSettings, Tuning};

/// The largest acceleration factor, whatever the multipliers say.
pub const MAX_ACCELERATION: f64 = 25.0;
/// The most ticks one input event may carry (a hostile or confused client can't jump a value
/// further than this in one go).
pub const MAX_TICKS: i32 = 1000;

/// Remembers when each value last got a tick, to measure turning speed.
#[derive(Clone, Debug, Default)]
pub struct Accelerator {
    last_tick_ms: BTreeMap<String, f64>,
}

impl Accelerator {
    /// The acceleration for a tick of `target` at `now_ms`: the floor when ticks are slow, the
    /// ceiling when they are fast, and a quadratic curve between (calm at moderate speed,
    /// explosive only near the fast end, like a mouse).
    pub fn acceleration(&mut self, target: &str, now_ms: f64, tuning: &Tuning, motion: &MotionSettings) -> f64 {
        let t = tuning.sanitized();
        let min = t.acceleration_min.max(0.1);
        let global = if motion.acceleration.is_finite() { motion.acceleration.clamp(0.1, 4.0) } else { 1.0 };
        let max = (t.acceleration_max * t.acceleration * global).clamp(min, MAX_ACCELERATION.max(min));
        let prev = self.last_tick_ms.insert(target.to_string(), now_ms);
        let Some(prev) = prev else { return min };
        let gap = now_ms - prev;
        if !gap.is_finite() {
            return min;
        }
        let (fast, slow) = (f64::from(t.fast_ms), f64::from(t.slow_ms));
        if gap >= slow {
            return min;
        }
        if gap <= fast {
            return max;
        }
        let x = (slow - gap) / (slow - fast);
        min + (max - min) * x * x
    }

    /// Forget a value's timing (its gesture ended).
    pub fn reset(&mut self, target: &str) {
        self.last_tick_ms.remove(target);
    }
}

/// The step factor of the held modifiers (Fine wins over Coarse when both are held).
pub fn modifier_factor(held: &[Modifier], motion: &MotionSettings) -> f64 {
    let d = MotionSettings::default();
    if held.contains(&Modifier::Fine) {
        if motion.fine.is_finite() { motion.fine.clamp(0.01, 1.0) } else { d.fine }
    } else if held.contains(&Modifier::Coarse) {
        if motion.coarse.is_finite() { motion.coarse.clamp(1.0, 20.0) } else { d.coarse }
    } else {
        1.0
    }
}

/// The value change for `ticks` of a value whose step is `step` units per tick: ticks × step ×
/// speed × acceleration × modifier, turned around when inverted (global XOR per value), limited to
/// `max_delta`. Never NaN or infinite.
pub fn delta(ticks: i32, step: f64, acceleration: f64, modifier: f64, tuning: &Tuning, motion: &MotionSettings, max_delta: f64) -> f64 {
    let t = tuning.sanitized();
    let ticks = ticks.clamp(-MAX_TICKS, MAX_TICKS);
    let invert = motion.invert ^ t.invert;
    let ticks = f64::from(if invert { -ticks } else { ticks });
    let global = if motion.speed.is_finite() { motion.speed } else { 1.0 };
    let speed = (global * t.speed).clamp(0.1, 5.0);
    let d = ticks * step * speed * acceleration * modifier;
    if !d.is_finite() {
        return 0.0;
    }
    let lim = if max_delta.is_finite() && max_delta > 0.0 { max_delta } else { 0.0 };
    d.clamp(-lim, lim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_and_slow_ticks_get_the_floor_fast_ticks_the_ceiling() {
        let (t, m) = (Tuning::default(), MotionSettings::default());
        let mut a = Accelerator::default();
        assert_eq!(a.acceleration("x", 0.0, &t, &m), 1.0, "first tick");
        assert_eq!(a.acceleration("x", 1000.0, &t, &m), 1.0, "slow");
        assert_eq!(a.acceleration("x", 1010.0, &t, &m), 10.0, "fast");
        // halfway between fast (20) and slow (200): quadratic, so a quarter of the way up
        let mid = a.acceleration("x", 1010.0 + 110.0, &t, &m);
        assert!((mid - (1.0 + 9.0 * 0.25)).abs() < 1e-9, "{mid}");
        // targets are timed separately
        assert_eq!(a.acceleration("y", 1121.0, &t, &m), 1.0);
        a.reset("x");
        assert_eq!(a.acceleration("x", 1122.0, &t, &m), 1.0);
    }

    #[test]
    fn ceiling_scales_with_both_multipliers_and_is_capped() {
        let t = Tuning { acceleration: 4.0, acceleration_max: 15.0, ..Tuning::default() };
        let m = MotionSettings { acceleration: 4.0, ..MotionSettings::default() };
        let mut a = Accelerator::default();
        a.acceleration("x", 0.0, &t, &m);
        assert_eq!(a.acceleration("x", 1.0, &t, &m), MAX_ACCELERATION);
        let calm = Tuning { acceleration: 0.1, acceleration_max: 1.0, acceleration_min: 2.0, ..Tuning::default() };
        a.acceleration("y", 0.0, &calm, &m);
        assert_eq!(a.acceleration("y", 1.0, &calm, &m), 2.0, "never below the floor");
    }

    #[test]
    fn time_going_backwards_or_nan_is_harmless() {
        let (t, m) = (Tuning::default(), MotionSettings::default());
        let mut a = Accelerator::default();
        a.acceleration("x", 100.0, &t, &m);
        assert_eq!(a.acceleration("x", 50.0, &t, &m), 10.0, "negative gap counts as fast");
        assert_eq!(a.acceleration("x", f64::NAN, &t, &m), 1.0);
        assert_eq!(a.acceleration("x", 10.0, &t, &m), 1.0, "after NaN: no usable gap");
    }

    #[test]
    fn deltas_speed_invert_modifiers_and_limits() {
        let (t, m) = (Tuning::default(), MotionSettings::default());
        assert!((delta(3, 0.01, 1.0, 1.0, &t, &m, 10.0) - 0.03).abs() < 1e-12);
        let slow = Tuning { speed: 0.5, ..t.clone() };
        assert!((delta(2, 1.0, 1.0, 1.0, &slow, &m, 100.0) - 1.0).abs() < 1e-12);
        let inv = Tuning { invert: true, ..t.clone() };
        assert_eq!(delta(1, 1.0, 1.0, 1.0, &inv, &m, 100.0), -1.0);
        let both = MotionSettings { invert: true, ..m.clone() };
        assert_eq!(delta(1, 1.0, 1.0, 1.0, &inv, &both, 100.0), 1.0, "global XOR per value");
        assert_eq!(delta(1, 1.0, 1.0, modifier_factor(&[Modifier::Fine], &m), &t, &m, 100.0), 0.25);
        assert_eq!(delta(1, 1.0, 1.0, modifier_factor(&[Modifier::Coarse, Modifier::Fine], &m), &t, &m, 100.0), 0.25);
        assert_eq!(delta(1, 1.0, 1.0, modifier_factor(&[Modifier::Coarse], &m), &t, &m, 100.0), 4.0);
        assert_eq!(delta(i32::MAX, 1.0, 25.0, 4.0, &t, &m, 200.0), 200.0, "limited to the range");
        assert_eq!(delta(i32::MIN, 1.0, 25.0, 4.0, &t, &m, 200.0), -200.0);
        assert_eq!(delta(1, f64::NAN, 1.0, 1.0, &t, &m, 200.0), 0.0);
        assert_eq!(delta(1, 1.0, 1.0, 1.0, &t, &m, f64::NAN), 0.0);
        let fast_global = MotionSettings { speed: 100.0, ..m.clone() };
        assert_eq!(delta(1, 1.0, 1.0, 1.0, &t, &fast_global, 100.0), 5.0, "speed product capped at 5");
    }
}
