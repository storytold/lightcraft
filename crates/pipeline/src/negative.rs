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

/// Whether `s` converts a negative.
pub fn enabled(s: &DevelopSettings) -> bool {
    s.negative.enabled
}

/// The film preset `n` names (Standard for a name no preset has).
pub fn preset_of(n: &Negative) -> &'static Preset {
    Preset::parse(&n.preset).unwrap_or_else(Preset::default_preset)
}

/// The conversion parameters and levels of `n`: the preset with the sliders on top, the advanced
/// controls when the panel is in Advanced, and the levels from the measurement (with a picked film
/// base as black), else from the film base alone.
pub fn params(n: &Negative) -> (Params, Analysis) {
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
                let scan = mul(&into, *px).map(|v| v.clamp(0.0, 1.0));
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
pub fn measure(src: &Rgb32f, border: f64) -> Result<NegativeMeasurement, String> {
    let m = to_srgb().to_f32();
    let px: Vec<[f32; 3]> = src.data.iter().map(|v| m.map(|r| r[0] * v[0] + r[1] * v[1] + r[2] * v[2])).collect();
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
        assert!((mid[0] - mid[2]).abs() < 0.03, "a grey scene stays grey: {mid:?}");
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
        assert_ne!(simple.data[0], img.data[0]);
    }

    #[test]
    fn hostile_settings_never_poison_the_image() {
        let mut img = scan(30, 20);
        img.data[3] = [f32::NAN, f32::INFINITY, -5.0];
        let negative = Negative {
            enabled: true,
            advanced: true,
            preset: "no such film".into(),
            tone: "nope".into(),
            exposure: f64::NAN,
            range: f64::INFINITY,
            base: Some([f64::NAN, -1.0, 1e30]),
            gray: Some([0.0; 3]),
            measured: Some(NegativeMeasurement { base: [0.0; 3], black: [f64::NAN; 3], white: [1.0; 3], balance: [f64::INFINITY; 3] }),
            ..Negative::default()
        };
        apply(&mut img, &DevelopSettings { negative, ..DevelopSettings::default() });
        assert!(img.data.iter().flatten().all(|v| v.is_finite()), "{:?}", &img.data[..6]);
        assert!(measure(&Rgb32f::new(0, 0), 0.08).is_err());
    }
}
