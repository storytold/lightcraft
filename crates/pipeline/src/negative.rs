//! Convert Negative: a scan of a film negative rendered as a positive ([`lightcraft_film`]), the
//! first thing the scene-linear stage does, so every later stage (white balance, tone, colour,
//! masks) edits the positive.
//!
//! The film model works per channel on what the scanner saw, in the channels of the scan's own
//! rendering: the source (linear Rec.2020) is taken to linear sRGB, converted, and taken back. The
//! scan's measurement is part of the settings ([`Negative::measured`]), so a draft, a zoomed window
//! and a full-size export all convert with the same numbers.

use lightcraft_color::{Mat3, REC2020, SRGB};
use lightcraft_develop::{DevelopSettings, Negative, NegativeMeasurement};
use lightcraft_film::{Analysis, Converter, Encoding, Look, Measurement, Params, Preset, Sliders, Tables, Tone, WbPreset};
use lightcraft_raster::{Rgb32f, par_rows};

pub use lightcraft_film as film;

fn to_srgb() -> Mat3 {
    REC2020.to_space(&SRGB)
}

fn to_rec2020() -> Mat3 {
    SRGB.to_space(&REC2020)
}

fn f32x3(v: [f64; 3]) -> [f32; 3] {
    v.map(|x| x as f32)
}

/// A scan sample inside 0..1 (not a number: 0).
fn unit(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

/// Whether `s` converts a negative: switched on, and its section not switched off with its eye
/// (`develop.sectionEnabled`), which keeps the settings for switching it on again.
pub fn enabled(s: &DevelopSettings) -> bool {
    s.negative.enabled && s.section_enabled("negative")
}

/// The film preset `n` names (Standard for a name no preset has).
pub fn preset_of(n: &Negative) -> &'static Preset {
    Preset::parse(&n.preset).unwrap_or_else(Preset::default_preset)
}

/// The conversion parameters and levels of `n`: the preset with the sliders on top, the advanced
/// controls when the panel is in Advanced, and the levels from the measurement (with a picked film
/// base as black), else from the film base alone.
pub fn params(n: &Negative) -> (Params, Analysis) {
    // (numbers out of range or not numbers, and scan colours outside 0..1, never reach the film model)
    let n = &n.sanitized();
    let preset = preset_of(n);
    let f = |v: f64| v as f32;
    let sliders = Sliders {
        exposure: f(n.exposure),
        brightness: f(n.brightness),
        contrast: f(n.contrast),
        temp: f(n.temp),
        tint: f(n.tint),
        saturation: f(n.saturation),
    };
    let mut p = preset.params(&sliders);
    let picked = if n.advanced { n.base.map(f32x3) } else { None };
    if n.advanced {
        if let Some(t) = Tone::parse(&n.tone) {
            p.tone = t;
        }
        if let Some(l) = Look::parse(&n.look) {
            p.look = l;
        }
        if let Some(wb) = WbPreset::parse(&n.wb) {
            p.wb = wb;
        }
        p.lights = f(n.highlights);
        p.darks = f(n.shadows);
        p.whites = f(n.whites);
        p.blacks = f(n.blacks);
        p.fade = f(n.fade);
        p.glow = f(n.glow);
        p.range = f(n.range);
    }
    let measured = n.measured.and_then(|m| {
        let m = measurement_of(&m);
        Some((m.analysis(picked)?, m.balance))
    });
    let analysis = match measured {
        Some((a, balance)) => {
            p.balance = balance;
            a
        }
        None => Analysis::from_base(picked.unwrap_or_else(|| preset.base_linear()), p.film_gamma),
    };
    (p, analysis)
}

fn measurement_of(m: &NegativeMeasurement) -> Measurement {
    Measurement { base: f32x3(m.base), black: f32x3(m.black), white: f32x3(m.white), balance: f32x3(m.balance) }
}

/// The converter of `n`, for linear sRGB samples.
pub fn converter(n: &Negative) -> Converter {
    let n = &n.sanitized();
    let (p, a) = params(n);
    let neutral = if n.advanced { n.gray.map(f32x3) } else { None };
    Converter::new(&a, &p, Encoding::Linear, neutral)
}

/// Converts `img` (linear Rec.2020) in place when the settings convert a negative.
pub fn apply(img: &mut Rgb32f, s: &DevelopSettings) {
    if !enabled(s) {
        return;
    }
    crate::timed("negative", || {
        let c = converter(&s.negative);
        let mut tables = Box::new(Tables::blank());
        c.tables(&mut tables);
        let (into, back) = (to_srgb().to_f32(), to_rec2020().to_f32());
        let mul = |m: &[[f32; 3]; 3], v: [f32; 3]| m.map(|r| r[0] * v[0] + r[1] * v[1] + r[2] * v[2]);
        let w = img.width;
        par_rows(&mut img.data, w, |_, row| {
            for px in row {
                let scan = mul(&into, *px).map(unit);
                *px = mul(&back, c.convert_with(&tables, scan));
            }
        });
    });
}

/// `src` as the rest of the pipeline sees it: converted to a positive when the settings convert a
/// negative. For the automatic adjustments (Auto, auto white balance and its selector, the B&W
/// mix), which measure the source directly.
pub fn positive<'a>(src: &'a Rgb32f, s: &DevelopSettings) -> std::borrow::Cow<'a, Rgb32f> {
    if !enabled(s) {
        return std::borrow::Cow::Borrowed(src);
    }
    let mut img = src.clone();
    apply(&mut img, s);
    std::borrow::Cow::Owned(img)
}

/// The linear sRGB colour of a source pixel (linear Rec.2020), as the panel's picker stores it.
pub fn scan_colour(rec2020: [f32; 3]) -> [f64; 3] {
    to_srgb().apply(rec2020.map(f64::from)).map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })
}

/// Measures `src` (linear Rec.2020, the whole scan) as a negative, leaving `border` (a fraction of
/// each edge) out.
///
/// `border` is 0 to 0.45 (the film model's limit); anything else, including not a number, is an
/// error.
pub fn measure(src: &Rgb32f, border: f64) -> Result<NegativeMeasurement, String> {
    use rayon::prelude::*;
    if !border.is_finite() || !(0.0..=0.45).contains(&border) {
        return Err(format!("the border must be between 0 and 0.45 of each edge, not {border}"));
    }
    let m = to_srgb().to_f32();
    let px: Vec<[f32; 3]> = src.data.par_iter().map(|v| m.map(|r| unit(r[0] * v[0] + r[1] * v[1] + r[2] * v[2]))).collect();
    let o = lightcraft_film::MeasureOptions { border: border as f32, ..Default::default() };
    let m = lightcraft_film::measure::measure(&px, src.width, src.height, &o)?;
    let d = |v: [f32; 3]| v.map(f64::from);
    Ok(NegativeMeasurement { base: d(m.base), black: d(m.black), white: d(m.white), balance: d(m.balance) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_film::{RANGE_STOPS, film};

    const BASE: [f32; 3] = [0.6, 0.35, 0.15];

    /// A negative of a grey ramp (left dark, right bright), in linear Rec.2020, with a border of
    /// unexposed film.
    fn scan(w: usize, h: usize) -> Rgb32f {
        let back = to_rec2020().to_f32();
        let mut img = Rgb32f::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let inside = x >= w / 10 && x < w - w / 10 && y >= h / 10 && y < h - h / 10;
                let s = x as f32 / (w - 1) as f32;
                let e = (s * RANGE_STOPS).exp2();
                let srgb = if inside { [0, 1, 2].map(|ch| film::transmittance(e, BASE[ch])) } else { BASE };
                img.data[y * w + x] = back.map(|r| r[0] * srgb[0] + r[1] * srgb[1] + r[2] * srgb[2]);
            }
        }
        img
    }

    #[test]
    fn off_leaves_the_image_alone() {
        let mut img = scan(60, 40);
        let before = img.clone();
        apply(&mut img, &DevelopSettings::default());
        assert_eq!(img.data, before.data);
    }

    #[test]
    fn a_measured_negative_turns_positive_and_grey_stays_grey() {
        let (w, h) = (120, 80);
        let mut img = scan(w, h);
        let measured = measure(&img, 0.12).unwrap();
        let mut s = DevelopSettings::default();
        s.negative.enabled = true;
        s.negative.measured = Some(measured);
        apply(&mut img, &s);
        let at = |x: usize| img.data[h / 2 * w + x];
        let (dark, bright) = (at(w / 5), at(w - w / 5));
        let luma = |c: [f32; 3]| c[0] + c[1] + c[2];
        assert!(luma(bright) > luma(dark) + 0.3, "the denser side is the brighter one now: {dark:?} {bright:?}");
        let mid = at(w / 2);
        for (a, b) in [(0, 1), (1, 2), (0, 2)] {
            assert!((mid[a] - mid[b]).abs() < 0.03, "a grey scene stays grey: {mid:?}");
        }
        assert!(img.data.iter().flatten().all(|v| v.is_finite()));
    }

    #[test]
    fn the_border_is_the_base_and_converts_to_black() {
        let (w, h) = (60, 40);
        let mut img = scan(w, h);
        let mut s = DevelopSettings::default();
        s.negative.enabled = true;
        s.negative.advanced = true;
        s.negative.base = Some(scan_colour(img.data[0]));
        apply(&mut img, &s);
        assert!(img.data[0].iter().all(|v| v.abs() < 1e-3), "{:?}", img.data[0]);
        // The same base ignored by the Simple panel: the preset's base is not this scan's.
        let mut simple = scan(w, h);
        s.negative.advanced = false;
        apply(&mut simple, &s);
        let gap = simple.data[0].iter().zip(img.data[0]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(gap > 1e-4, "{:?} {:?}", simple.data[0], img.data[0]);
        let mid = scan(w, h).data[h / 2 * w + w / 2];
        let mut inner = scan(w, h);
        s.negative.advanced = true;
        apply(&mut inner, &s);
        assert!(inner.data[h / 2 * w + w / 2].iter().sum::<f32>() > 0.05, "the interior is not black (scan {mid:?})");
    }

    #[test]
    fn a_picked_gray_is_gray_in_advanced_and_ignored_in_simple() {
        let (w, h) = (60, 40);
        let at = w * h / 2 + w / 3;
        let raw = scan(w, h);
        let pick = scan_colour(raw.data[at]);
        let mut s = DevelopSettings::default();
        s.negative.enabled = true;
        s.negative.gray = Some(pick);
        let spread = |c: [f32; 3]| c.iter().cloned().fold(f32::MIN, f32::max) - c.iter().cloned().fold(f32::MAX, f32::min);
        let (mut simple, mut adv) = (raw.clone(), raw.clone());
        apply(&mut simple, &s);
        s.negative.advanced = true;
        apply(&mut adv, &s);
        assert!(spread(adv.data[at]) < 0.02, "{:?}", adv.data[at]);
        let mut none = raw.clone();
        s.negative.advanced = false;
        s.negative.gray = None;
        apply(&mut none, &s);
        assert_eq!(simple.data, none.data, "Simple ignores the gray point");
    }

    #[test]
    fn simple_ignores_the_advanced_choices_and_advanced_applies_them() {
        let raw = scan(60, 40);
        let run = |advanced: bool| {
            let mut img = raw.clone();
            let negative = Negative { enabled: true, advanced, tone: "linear".into(), blacks: 40.0, fade: 80.0, ..Negative::default() };
            apply(&mut img, &DevelopSettings { negative, ..DevelopSettings::default() });
            img
        };
        let plain = {
            let mut img = raw.clone();
            apply(&mut img, &DevelopSettings { negative: Negative { enabled: true, ..Negative::default() }, ..DevelopSettings::default() });
            img
        };
        assert_eq!(run(false).data, plain.data);
        assert_ne!(run(true).data, plain.data);
    }

    #[test]
    fn the_sections_eye_switches_the_conversion_off_and_keeps_it() {
        let raw = scan(30, 20);
        let mut s = DevelopSettings { negative: Negative { enabled: true, ..Negative::default() }, ..DevelopSettings::default() };
        assert!(enabled(&s));
        s.disabled_sections.push("negative".into());
        assert!(!enabled(&s), "the eye is off");
        assert!(matches!(positive(&raw, &s), std::borrow::Cow::Borrowed(_)), "the automatic adjustments see the scan");
        let mut img = raw.clone();
        apply(&mut img, &s);
        assert_eq!(img.data, raw.data, "renders show the scan");
        assert!(!crate::lin_needs_cpu_stage(&crate::SourceInfo::default(), &s));
        assert!(s.negative.enabled, "the conversion is kept for switching the section on again");
    }

    #[test]
    fn an_unknown_preset_is_standard() {
        let n = Negative { preset: "no such film".into(), ..Negative::default() };
        assert_eq!(preset_of(&n).name, preset_of(&Negative::default()).name);
    }

    #[test]
    fn positive_borrows_when_off_and_matches_apply_when_on() {
        let raw = scan(30, 20);
        assert!(matches!(positive(&raw, &DevelopSettings::default()), std::borrow::Cow::Borrowed(_)));
        let mut s = DevelopSettings::default();
        s.negative.enabled = true;
        let mut applied = raw.clone();
        apply(&mut applied, &s);
        assert_eq!(positive(&raw, &s).data, applied.data);
    }

    #[test]
    fn the_cpu_stage_runs_for_a_negative_only_when_it_is_on() {
        let info = crate::SourceInfo::default();
        let mut s = DevelopSettings::default();
        assert!(!crate::lin_needs_cpu_stage(&info, &s) && !crate::lin_needs_cpu(&s, &info));
        s.negative.enabled = true;
        assert!(crate::lin_needs_cpu_stage(&info, &s) && crate::lin_needs_cpu(&s, &info));
    }

    #[test]
    fn measure_rejects_a_border_that_is_not_one_and_ignores_bad_pixels() {
        let mut img = scan(60, 40);
        for b in [f64::NAN, -0.1, 0.5, f64::INFINITY] {
            assert!(measure(&img, b).is_err(), "{b}");
        }
        img.data[100] = [f32::NAN, f32::INFINITY, -5.0];
        let m = measure(&img, 0.12).unwrap();
        for c in [m.base, m.black, m.white, m.balance].into_iter().flatten() {
            assert!(c.is_finite());
        }
    }

    #[test]
    fn hostile_settings_never_poison_the_image() {
        let mut img = scan(30, 20);
        img.data[3] = [f32::NAN, f32::INFINITY, -5.0];
        img.data[4] = [f32::NEG_INFINITY, f32::NAN, 1e30];
        let negative = Negative {
            enabled: true,
            advanced: true,
            preset: "no such film".into(),
            tone: "nope".into(),
            exposure: f64::NAN,
            range: f64::INFINITY,
            brightness: 1e300,
            contrast: f64::NEG_INFINITY,
            temp: f64::NAN,
            tint: -1e9,
            saturation: f64::INFINITY,
            highlights: f64::NAN,
            shadows: 1e9,
            whites: f64::NAN,
            blacks: -1e9,
            fade: f64::INFINITY,
            glow: f64::NAN,
            wb: "nope".into(),
            look: "nope".into(),
            base: Some([f64::NAN, -1.0, 1e30]),
            gray: Some([f64::NAN, 0.0, 9.0]),
            measured: Some(NegativeMeasurement { base: [0.0; 3], black: [f64::NAN; 3], white: [1.0; 3], balance: [f64::INFINITY; 3] }),
        };
        apply(&mut img, &DevelopSettings { negative, ..DevelopSettings::default() });
        assert!(img.data.iter().flatten().all(|v| v.is_finite()), "{:?}", &img.data[..6]);
        assert!(measure(&Rgb32f::new(0, 0), 0.08).is_err());
    }
}
