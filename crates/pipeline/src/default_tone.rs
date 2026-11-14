//! Lightroom's default base curve, measured (process V9, `docs/default-tone.md`).
//!
//! A camera profile without a tone curve of its own (a DCP without `ProfileToneCurve`, such as the Adobe Standard
//! profiles) leaves the base tone to the raw converter. Lightroom then applies its default curve, hue preserving on
//! linear ProPhoto channels. Up to V8 LightCraft keeps the photo's own tone instead (the curve fitted to its camera
//! JPEG, `lightcraft_engine::files`), so the same profile renders brighter and flatter than in Lightroom.
//!
//! From V9 such a raw ([`SourceInfo::default_tone`]) gets the curve measured on Camera Raw's render of a synthetic
//! chart of grey steps (`tools/lr_tone_charts.py base`, [`crate::default_tone_table`]), applied the way Lightroom
//! applies it ([`rgb_tone`]). The chart is a DNG without a baseline exposure. Camera Raw gives a raw of another format
//! its own exposure offset per camera, which LightCraft does not know: [`camera_ev`] holds the offsets measured on
//! photos, and other cameras get none.

use crate::SourceInfo;
use crate::default_tone_table::GREY;
use crate::tone::CameraTone;
use lightcraft_develop::{DevelopSettings, Process};

/// Exposure offsets per camera (make, model, stops) for a raw rendered through its Adobe Standard profile: the shift
/// of the chart's curve that leaves Lightroom's unedited renders of the camera's photos no brighter or darker on
/// average, measured per camera (`docs/default-tone.md`).
const CAMERA_EV: [(&str, &str, f32); 4] =
    [("Panasonic", "DC-S9", -0.10), ("Panasonic", "DC-S5", -0.04), ("Panasonic", "DC-S5M2", -0.03), ("Panasonic", "DC-S5M2X", -0.05)];

/// The measured exposure offset of a camera in stops, 0 for a camera not measured.
pub fn camera_ev(make: &str, model: &str) -> f32 {
    CAMERA_EV.iter().find(|(m, n, _)| make.trim().eq_ignore_ascii_case(m) && model.trim().eq_ignore_ascii_case(n)).map_or(0.0, |c| c.2)
}

/// The default curve of `info` under `s`: from process V9, for a raw whose profile has no tone curve.
pub fn for_source(s: &DevelopSettings, info: &SourceInfo) -> Option<CameraTone> {
    let v9 = match s.process.process() {
        Process::V1 | Process::V2 | Process::V3 | Process::V4 | Process::V5 | Process::V6 | Process::V7 | Process::V8 => false,
        Process::V9 => true,
    };
    curve(info.default_tone.filter(|_| v9 && info.raw)?)
}

/// The measured curve with the scene shifted by `ev` stops: 32 knots, evenly spaced in stops over the chart's grey
/// steps. Below the first knot the curve runs straight to black; past the last it rolls off into white
/// ([`CameraTone::apply_rolloff`]).
pub fn curve(ev: f32) -> Option<CameraTone> {
    if !ev.is_finite() {
        return None;
    }
    let gain = 2f32.powf(ev);
    let (x0, x1) = (GREY[0][0].ln(), GREY[GREY.len() - 1][0].ln());
    let knots: [[f32; 2]; 32] = std::array::from_fn(|i| {
        let x = (x0 + (x1 - x0) * i as f32 / 31.0).exp();
        [x / gain, grey(x).min(0.9995)]
    });
    CameraTone::new(knots)
}

/// The chart's output at chart value `x` (linear in log `x` between the grey steps).
fn grey(x: f32) -> f32 {
    let l = x.max(GREY[0][0]).ln();
    let i = GREY.partition_point(|g| g[0].ln() <= l).clamp(1, GREY.len() - 1);
    let (a, b) = (GREY[i - 1], GREY[i]);
    let t = ((l - a[0].ln()) / (b[0].ln() - a[0].ln())).clamp(0.0, 1.0);
    a[1] + (b[1] - a[1]) * t
}

/// The base tone as Lightroom applies it: `tone` on the largest and smallest channel in linear ProPhoto, the middle
/// channel keeping its place between them (linear Rec.2020 in and out).
pub fn rgb_tone(c: [f32; 3], tone: impl Fn(f32) -> f32) -> [f32; 3] {
    let (to, from) = matrices();
    let p = mul3(to, c).map(|v| v.max(0.0));
    let (mut hi, mut lo) = (0, 0);
    for k in 1..3 {
        if p[k] > p[hi] {
            hi = k;
        }
        if p[k] < p[lo] {
            lo = k;
        }
    }
    let (a, b) = (p[hi], p[lo]);
    let (fa, fb) = (tone(a), tone(b));
    let out: [f32; 3] = std::array::from_fn(|k| if a > b { fb + (fa - fb) * (p[k] - b) / (a - b) } else { fa });
    mul3(from, out)
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

    /// Settings on process `p`.
    fn on(p: Process) -> DevelopSettings {
        DevelopSettings { process: p.version(), ..Default::default() }
    }

    /// A raw rendered through a profile without a tone curve, with exposure offset `ev`.
    fn raw(ev: Option<f32>) -> SourceInfo {
        SourceInfo { raw: true, default_tone: ev, ..Default::default() }
    }

    #[test]
    fn only_v9_raws_whose_profile_has_no_curve_get_it() {
        assert!(for_source(&on(Process::V9), &raw(Some(0.0))).is_some());
        assert!(for_source(&on(Process::V7), &raw(Some(0.0))).is_none());
        assert!(for_source(&on(Process::V8), &raw(Some(0.0))).is_none());
        assert!(for_source(&on(Process::V9), &raw(None)).is_none());
        let rendered = SourceInfo { raw: false, ..raw(Some(0.0)) };
        assert!(for_source(&on(Process::V9), &rendered).is_none());
    }

    #[test]
    fn the_curve_follows_the_chart_and_the_offset_shifts_it() {
        let c = curve(0.0).unwrap();
        for g in GREY.iter().filter(|g| g[0] > 0.01 && g[0] < 0.9) {
            assert!((c.apply(g[0]) - g[1]).abs() < 0.01, "{g:?} -> {}", c.apply(g[0]));
        }
        // the chart's mid grey (scene 0.18) lands well above display 0.18: a base curve with contrast and lift
        let mid = c.apply(0.18);
        assert!(mid > 0.35 && mid < 0.45, "{mid}");
        // -0.1 stops: the scene needs 2^0.1 more light for the same output
        let d = curve(-0.1).unwrap();
        assert!((d.apply(0.18 * 2f32.powf(0.1)) - mid).abs() < 2e-3);
        assert!(curve(f32::NAN).is_none());
    }

    #[test]
    fn camera_offsets_are_by_make_and_model() {
        assert_eq!(camera_ev("Panasonic", "DC-S9"), -0.10);
        assert_eq!(camera_ev("PANASONIC ", "dc-s9"), -0.10);
        assert_eq!(camera_ev("Panasonic", "DC-S5M2"), -0.03);
        assert_eq!(camera_ev("Panasonic", "DC-G9M2"), 0.0);
        assert_eq!(camera_ev("Sony", "ILCE-7M4"), 0.0);
    }

    #[test]
    fn rgb_tone_keeps_grey_grey_and_hue_in_prophoto() {
        let c = curve(0.0).unwrap();
        let f = |v: f32| c.apply_rolloff(v);
        let g = rgb_tone([0.18; 3], f);
        assert!(g.iter().all(|v| (v - c.apply(0.18)).abs() < 1e-4), "{g:?}");
        // the middle ProPhoto channel keeps its relative place between the others
        let (to, from) = matrices();
        let p = [0.30, 0.12, 0.05];
        let o = mul3(to, rgb_tone(mul3(from, p), f));
        let (r0, r1) = ((p[1] - p[2]) / (p[0] - p[2]), (o[1] - o[2]) / (o[0] - o[2]));
        assert!((r0 - r1).abs() < 1e-3, "{r0} {r1}");
        assert!((o[0] - f(0.30)).abs() < 1e-4 && (o[2] - f(0.05)).abs() < 1e-4);
    }
}
