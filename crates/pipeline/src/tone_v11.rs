//! Tone curve changes introduced by process V11.
//!
//! The shared tone map remains the base for older processes. This module builds the V11 table
//! from the same camera/display inputs so V1–V10 retain their stored appearance.

use crate::SourceInfo;
use crate::tone::{CameraTone, GREY, LUT_MAX_EV, LUT_MIN_EV, LUT_N, ToneMap};
use lightcraft_develop::DevelopSettings;

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn display_value(p: f32, contrast: f32, whites: f32, blacks: f32, shadows: f32) -> f32 {
    let black_range = if blacks < 0.0 { 0.90 + 0.05 * shadows } else { 0.45 };
    let mut p = p;
    if p <= 1.0 {
        let m = GREY.powf(1.0 / 2.2);
        p += contrast * 0.35 * (p - m) * (1.0 - (2.0 * p - 1.0).powi(2));
        let up = smooth(0.18, 0.90, p);
        p += whites * 0.50 * up * (1.0 - p * 0.5);
        let lo = 1.0 - smooth(0.0, black_range, p);
        p += blacks * 0.49 * lo * (p * 2.0).min(1.0);
    } else {
        p += whites * 0.25;
    }
    p.max(0.0).powf(2.2)
}

fn display_lut(s: &DevelopSettings, peak: f32) -> Vec<f32> {
    let (contrast, whites, blacks) = ((s.light.contrast / 100.0) as f32, (s.light.whites / 100.0) as f32, (s.light.blacks / 100.0) as f32);
    let shadows = (s.light.shadows / 100.0).clamp(0.0, 1.0) as f32;
    let peak = if peak.is_finite() { peak.max(1.0) } else { 1.0 };
    let neutral = contrast == 0.0 && whites == 0.0 && blacks == 0.0;
    (0..LUT_N)
        .map(|i| {
            let ev = LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32;
            let y = GREY * 2.0f32.powf(ev);
            let mut o = display_value(y.powf(1.0 / 2.2), contrast, whites, blacks, shadows);
            if peak > 1.0 && o > 0.95 {
                let r = peak - 0.95;
                o = 0.95 + r * (1.0 - (-(o - 0.95) / r).exp());
            } else if o > 0.95 && !neutral {
                let d = (o - 0.95).min(0.1);
                o = 0.95 + d - d * d / 0.2;
            }
            o.clamp(0.0, peak)
        })
        .collect()
}

fn camera_lut(s: &DevelopSettings, curve: &CameraTone, peak: f32) -> Vec<f32> {
    let contrast = s.light.contrast;
    let whites = s.light.whites;
    let blacks = s.light.blacks;
    let neutral = contrast == 0.0 && whites == 0.0 && blacks == 0.0;
    let peak = if peak.is_finite() { peak.max(1.0) } else { 1.0 };
    let top = (2.9 - 1.6 * (whites as f32 / 100.0) + peak.log2() + 1.0).max(2.0);
    let contrast = (contrast / 100.0) as f32;
    let whites = (whites / 100.0) as f32;
    let blacks = (blacks / 100.0) as f32;
    let shadows = (s.light.shadows / 100.0).clamp(0.0, 1.0) as f32;
    (0..LUT_N)
        .map(|i| {
            let ev = LUT_MIN_EV + (LUT_MAX_EV - LUT_MIN_EV) * i as f32 / (LUT_N - 1) as f32;
            let shadow_weight = smooth(-8.0, -6.0, ev) * (1.0 - smooth(-4.0, -3.0, ev));
            let warped_ev = ev + 0.35 * shadow_weight;
            let y = curve.apply_rolloff(GREY * 2.0f32.powf(warped_ev));
            let mut o = if neutral { y } else { display_value(y.powf(1.0 / 2.2), contrast, whites, blacks, shadows) };
            if peak > 1.0 {
                o = (o + (peak - 1.0) * smooth(1.5, top, ev)).clamp(0.0, peak);
            }
            o
        })
        .collect()
}

/// Build the V11 scene-EV tone table. Camera curves retain their newer roll-off behaviour; other
/// raw inputs retain the current generic V10 map. Display sources use V11's wider tonal controls.
pub(crate) fn build(s: &DevelopSettings, info: &SourceInfo, peak: f32, base: &ToneMap) -> Vec<f32> {
    if let Some(curve) = info.camera_tone.as_ref().filter(|_| info.raw) {
        camera_lut(s, curve, peak)
    } else if info.raw {
        base.lut().to_vec()
    } else {
        display_lut(s, peak)
    }
}

/// Apply a tone table over its scene-EV domain.
#[inline]
pub(crate) fn apply(lut: &[f32], y: f32) -> f32 {
    if !y.is_finite() || y <= 0.0 || lut.len() < 2 {
        return 0.0;
    }
    let ev = (y / GREY).log2();
    let f = ((ev - LUT_MIN_EV) / (LUT_MAX_EV - LUT_MIN_EV)).clamp(0.0, 1.0) * (lut.len() - 1) as f32;
    let i = (f as usize).min(lut.len() - 2);
    let a = lut.get(i).copied().unwrap_or(0.0);
    let b = lut.get(i + 1).copied().unwrap_or(a);
    let v = a + (b - a) * (f - i as f32);
    if ev < LUT_MIN_EV { v * (y / (GREY * 2.0f32.powf(LUT_MIN_EV))) } else { v }
}

#[cfg(test)]
mod tests {
    use super::display_value;

    #[test]
    fn display_whites_are_continuous_at_reference_white() {
        for whites in [-1.0, 1.0] {
            let below = display_value(1.0 - 1e-5, 0.0, whites, 0.0, 0.0);
            let at = display_value(1.0, 0.0, whites, 0.0, 0.0);
            let above = display_value(1.0 + 1e-5, 0.0, whites, 0.0, 0.0);
            let expected_at = (1.0 + 0.25 * whites).powf(2.2);
            assert!((at - expected_at).abs() < 1e-6, "Whites {whites}: expected {expected_at}, got {at}");
            assert!((at - below).abs() < 5e-5, "Whites {whites}: below {below}, at {at}");
            assert!((above - at).abs() < 5e-5, "Whites {whites}: at {at}, above {above}");
        }
    }
}
