//! Process V9's mixer stage ([`lightcraft_develop::Process::V9`]): the colour mixer. `docs/mixer-stage.md` records
//! how the model was measured and fitted.
//!
//! - Each slider's effect comes from a fitted model (`mixer_stage_model`, written by `tools/lr_mixer_charts.py fit`):
//!   each band has a centre and a width on each side, shared by its Hue, Saturation and Luminance sliders; each kind
//!   of slider has a gain that grows faster than linearly with the amount and a dependence on the colour's chroma and
//!   lightness; each band scales them per slider and output ([`response`]).
//! - The bands differ from V8's: their centres sit elsewhere on the hue circle (Orange at OkLab hue 50, not 82), Blue
//!   is wider and Red and Magenta narrower.
//! - All 24 sliders act on the same input colour: their hue and lightness changes add and their chroma changes
//!   multiply.
//! - The mixer acts before the tone curves.
//!
//! The stage runs after the tone map and Process V4's calibrated Contrast and Blacks, before the curve stage
//! ([`crate::curve_stage`]). It replaces V8's mixer in [`crate::colorops::ColorOps`].

use lightcraft_color::perceptual::{lab_to_lch, lch_to_lab, oklab_from_2020, oklab_to_2020};
use lightcraft_develop::{DevelopSettings, Process};

use crate::mixer_stage_model as m;

/// Hue bins per table row (5° each, from OkLab hue 0).
pub const HUES: usize = 72;
/// Table rows per output: 3 lightness levels × 3 chroma levels, lightness major.
pub const ROWS: usize = 9;
/// Outputs per row: hue shift, chroma, lightness change.
const OUTPUTS: usize = 3;
/// Values of the combined table: [`ROWS`] × outputs × [`HUES`].
pub const TABLE_LEN: usize = ROWS * OUTPUTS * HUES;
/// Sliders in the model: mixer bands (red … magenta) × (hue, saturation, luminance).
const SLIDERS: usize = 24;
/// Where the combined table samples the model: OkLab lightness levels.
const LIGHT: [f32; 3] = [0.55, 0.78, 0.92];
/// The table's chroma levels per lightness level: bright colours reach less chroma, so their levels sit lower.
const CHROMA: [[f32; 3]; 3] = [[0.07, 0.13, 0.19], [0.045, 0.09, 0.14], [0.025, 0.05, 0.09]];

/// The stage's combined response table: per row (lightness level × 3 + chroma level), output and hue bin, the hue
/// shift (radians), the chroma factor and the lightness change of every slider together, sampled from the model.
#[derive(Clone, Debug)]
pub struct MixerStage {
    /// OkLab chroma of the table's chroma levels, per lightness level ([`CHROMA`]).
    pub chroma: [[f32; 3]; 3],
    /// OkLab lightness of the table's lightness levels ([`LIGHT`]).
    pub light: [f32; 3],
    pub table: Box<[f32; TABLE_LEN]>,
}

/// The fitted response of slider `slider` (band × 3 + kind) output `out` at amount `t` (−1 to 1) for OkLCh chroma `c`,
/// lightness `l` and hue `h` (degrees): a hue shift in degrees, a relative chroma change or a lightness change.
/// `amp · gain(t) · band(h) · level(dc, dl)` with the band's shape shared by its three sliders, and the gain and level
/// dependence shared by all bands' sliders of one kind (docs/mixer-stage.md).
fn response(slider: usize, out: usize, t: f64, c: f64, l: f64, h: f64) -> f64 {
    let (band, kind) = (slider / 3, slider % 3);
    let (Some([centre, left, right]), Some(k), Some(amp)) =
        (m::BANDS.get(band), m::KINDS.get(kind).and_then(|k| k.get(out)), m::AMPS.get(band).and_then(|a| a.get(kind)).and_then(|a| a.get(out)))
    else {
        return 0.0;
    };
    let [g1, g2, g3, g4, g5, g6, ac, al] = *k;
    let (at, dc, dl) = (t.abs(), (c - m::REF_CHROMA.0) / m::REF_CHROMA.1, (l - m::REF_LIGHT.0) / m::REF_LIGHT.1);
    let gain = if t < 0.0 { g1 * at + g2 * at * at + g3 * at * at * at } else { g4 * at + g5 * at * at + g6 * at * at * at };
    let d = (h - centre + 180.0).rem_euclid(360.0) - 180.0;
    let z = d / (if d < 0.0 { *left } else { *right }).exp();
    amp * gain * (-0.5 * z * z).exp() * (ac * dc + al * dl).exp()
}

/// The combined table of `(slider, amount)` pairs: hue and lightness changes add, chroma changes multiply.
fn combine(sliders: &[(usize, f32)]) -> Option<Box<[f32; TABLE_LEN]>> {
    let active: Vec<(usize, f64)> = sliders
        .iter()
        .filter(|(i, v)| *v != 0.0 && v.is_finite() && *i < SLIDERS)
        .map(|(i, v)| (*i, (f64::from(*v) / 100.0).clamp(-1.0, 1.0)))
        .collect();
    if active.is_empty() {
        return None;
    }
    let mut t = Box::new([0.0f32; TABLE_LEN]);
    for row in 0..ROWS {
        let (li, ci) = (row / 3, row % 3);
        let l = f64::from(LIGHT.get(li).copied().unwrap_or(0.0));
        let c = f64::from(CHROMA.get(li).and_then(|r| r.get(ci)).copied().unwrap_or(0.0));
        for out in 0..OUTPUTS {
            for bin in 0..HUES {
                let h = bin as f64 * (360.0 / HUES as f64);
                let mut acc = if out == 1 { 1.0 } else { 0.0 };
                for (slider, t) in &active {
                    let r = response(*slider, out, *t, c, l, h) as f32;
                    match out {
                        0 => acc += r.to_radians(),
                        1 => acc *= (1.0 + r).max(0.0),
                        _ => acc += r,
                    }
                }
                if let Some(slot) = t.get_mut((row * OUTPUTS + out) * HUES + bin) {
                    *slot = acc;
                }
            }
        }
    }
    Some(t)
}

/// Position of `x` between the three levels `keys`: (lower, upper, weight of the upper), clamped.
fn level(keys: &[f32; 3], x: f32) -> (usize, usize, f32) {
    if x <= keys[0] {
        (0, 0, 0.0)
    } else if x <= keys[1] {
        (0, 1, (x - keys[0]) / (keys[1] - keys[0]))
    } else if x <= keys[2] {
        (1, 2, (x - keys[1]) / (keys[2] - keys[1]))
    } else {
        (2, 2, 0.0)
    }
}

impl MixerStage {
    /// The stage for `s`: `None` before V9, or when the mixer is neutral.
    pub fn new(s: &DevelopSettings) -> Option<MixerStage> {
        if s.process.process() < Process::V9 {
            return None;
        }
        let sliders: Vec<(usize, f32)> = s.mixer.bands().iter().flat_map(|b| [b.hue, b.sat, b.lum]).enumerate().map(|(i, v)| (i, v as f32)).collect();
        combine(&sliders).map(|table| MixerStage { chroma: CHROMA, light: LIGHT, table })
    }

    /// The (hue shift, chroma factor, lightness change) for OkLCh `l`, `c`, `h`: linear in hue, bilinear in the chroma
    /// and lightness levels, clamped to the table's. Below the lowest chroma level the lightness change fades
    /// out towards neutral.
    pub fn look(&self, l: f32, c: f32, h: f32) -> [f32; 3] {
        let hd = h.to_degrees().rem_euclid(360.0) / 5.0;
        let i0 = (hd as usize) % HUES;
        let i1 = (i0 + 1) % HUES;
        let fh = hd - hd.floor();
        let (l0, l1, fl) = level(&self.light, l);
        let at = |row: usize, out: usize, bin: usize| self.table.get((row * OUTPUTS + out) * HUES + bin).copied().unwrap_or(0.0);
        // per lightness level: bilinear in its own chroma keys, the lightness change faded out below the lowest
        let per_level = |m: usize| {
            let keys = self.chroma.get(m).copied().unwrap_or([1.0; 3]);
            let (c0, c1, fc) = level(&keys, c);
            let mut r = [0.0f32; 3];
            for (out, v) in r.iter_mut().enumerate() {
                let hue = |row: usize| at(row, out, i0) * (1.0 - fh) + at(row, out, i1) * fh;
                *v = hue(m * 3 + c0) * (1.0 - fc) + hue(m * 3 + c1) * fc;
            }
            if c < keys[0] {
                r[2] *= (c / keys[0]).max(0.0);
            }
            r
        };
        let (a, b) = (per_level(l0), per_level(l1));
        std::array::from_fn(|k| a[k] * (1.0 - fl) + b[k] * fl)
    }

    /// The colour mixer on linear Rec.2020 (through OkLCh).
    pub fn apply(&self, d: [f32; 3]) -> [f32; 3] {
        let [l, c, h] = lab_to_lch(oklab_from_2020(d));
        let [dh, k, dl] = self.look(l, c, h);
        oklab_to_2020(lch_to_lab([(l + dl).max(0.0), c * k, h + dh]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_develop::Hsl;

    /// Default settings on process V9.
    fn v9() -> DevelopSettings {
        DevelopSettings { process: Process::V9.version(), ..Default::default() }
    }

    /// OkLCh of a linear Rec.2020 colour (`h` in radians).
    fn lch_of(d: [f32; 3]) -> [f32; 3] {
        lab_to_lch(oklab_from_2020(d))
    }

    /// Linear Rec.2020 of OkLCh (`h` in degrees).
    fn colour(l: f32, c: f32, h: f32) -> [f32; 3] {
        oklab_to_2020(lch_to_lab([l, c, h.to_radians()]))
    }

    /// Hue change from `a` to `b` in degrees, in -180..180.
    fn hue_shift(a: [f32; 3], b: [f32; 3]) -> f32 {
        let d = (lch_of(b)[2] - lch_of(a)[2]).to_degrees();
        (d + 180.0).rem_euclid(360.0) - 180.0
    }

    /// Every slider's fitted response stays finite and within plausible ranges over the colours the table samples,
    /// and is 0 at amount 0.
    #[test]
    fn the_fitted_responses_stay_plausible() {
        for slider in 0..SLIDERS {
            for out in 0..OUTPUTS {
                // Blue Luminance −100 darkens pale blues and raises their chroma severalfold
                let limit = [180.0, 8.0, 0.5][out];
                for t in [-1.0, -0.6, -0.25, 0.3, 0.75, 1.0] {
                    for (li, row) in CHROMA.iter().enumerate() {
                        for c in row {
                            for bin in 0..HUES {
                                let r = response(slider, out, t, f64::from(*c), f64::from(LIGHT[li]), bin as f64 * 5.0);
                                assert!(r.is_finite() && r.abs() <= limit, "slider {slider} out {out} at {t}: {r}");
                            }
                        }
                    }
                }
                assert_eq!(response(slider, out, 0.0, 0.1, 0.7, 50.0), 0.0);
            }
        }
    }

    /// Each band acts around its own centre: a slider moves colours at its band's centre more than colours half the
    /// hue circle away.
    #[test]
    fn each_band_acts_around_its_own_centre() {
        for (band, [centre, ..]) in m::BANDS.iter().enumerate() {
            let at = |h: f64| response(band * 3 + 1, 1, 1.0, 0.09, 0.75, h).abs();
            assert!(at(*centre) > 5.0 * at(centre + 180.0), "band {band}: {} vs {}", at(*centre), at(centre + 180.0));
        }
    }

    #[test]
    fn only_v8_with_a_mixer_gets_the_stage() {
        assert!(MixerStage::new(&v9()).is_none());
        let mut s = v9();
        s.calibration.blue_hue = -20.0;
        assert!(MixerStage::new(&s).is_none(), "Calibration keeps the earlier operator");
        s.mixer.orange.sat = 30.0;
        assert!(MixerStage::new(&s).is_some());
        s.process = Process::V7.version();
        assert!(MixerStage::new(&s).is_none());
    }

    #[test]
    fn a_band_moves_its_own_colours_and_leaves_the_others() {
        let mut s = v9();
        s.mixer.orange = Hsl { hue: 0.0, sat: -60.0, lum: 0.0 };
        let st = MixerStage::new(&s).expect("stage");
        // the Orange band's centre sits near OkLab hue 51
        let orange = colour(0.7, 0.1, 55.0);
        let out = st.apply(orange);
        assert!(lch_of(out)[1] < lch_of(orange)[1] * 0.7, "{:?} -> {:?}", lch_of(orange), lch_of(out));
        let blue = colour(0.6, 0.1, 255.0);
        let b = lch_of(st.apply(blue));
        assert!((b[1] / lch_of(blue)[1] - 1.0).abs() < 0.03 && (b[0] - lch_of(blue)[0]).abs() < 0.005, "{b:?}");
    }

    #[test]
    fn hue_and_luminance_follow_the_measured_direction() {
        let mut s = v9();
        s.mixer.blue.hue = -100.0;
        let st = MixerStage::new(&s).expect("stage");
        let blue = colour(0.6, 0.1, 255.0);
        // Blue Hue −100 turns blue towards aqua (lower OkLab hue)
        assert!(hue_shift(blue, st.apply(blue)) < -10.0, "{}", hue_shift(blue, st.apply(blue)));
        let mut s = v9();
        s.mixer.green.lum = -80.0;
        let st = MixerStage::new(&s).expect("stage");
        let green = colour(0.7, 0.1, 140.0);
        assert!(lch_of(st.apply(green))[0] < lch_of(green)[0] - 0.03);
    }

    #[test]
    fn amounts_interpolate_and_grow_with_the_slider() {
        let shift = |v: f64| {
            let mut s = v9();
            s.mixer.yellow.hue = v;
            let st = MixerStage::new(&s).expect("stage");
            let c = colour(0.85, 0.1, 100.0);
            hue_shift(c, st.apply(c)).abs()
        };
        let (a, b, c, d) = (shift(10.0), shift(25.0), shift(60.0), shift(100.0));
        assert!(a < b && b < c && c < d, "{a} {b} {c} {d}");
        assert!(a < b * 0.5, "a small amount does little: {a} vs {b}");
    }

    #[test]
    fn sliders_combine_on_the_same_input() {
        let one = |f: &dyn Fn(&mut DevelopSettings)| {
            let mut s = v9();
            f(&mut s);
            MixerStage::new(&s).expect("stage")
        };
        let a = one(&|s| s.mixer.orange.sat = 40.0);
        let b = one(&|s| s.mixer.orange.lum = -30.0);
        let both = one(&|s| {
            s.mixer.orange.sat = 40.0;
            s.mixer.orange.lum = -30.0;
        });
        // on a table node (lightness and chroma level 1, hue bin 11), where no interpolation blurs the rule
        let (l, c, h) = (a.light[1], a.chroma[1][1], 55f32.to_radians());
        let (ra, rb, r) = (a.look(l, c, h), b.look(l, c, h), both.look(l, c, h));
        assert!((r[1] - ra[1] * rb[1]).abs() < 1e-5, "chroma changes multiply: {ra:?} {rb:?} {r:?}");
        assert!((r[2] - ra[2] - rb[2]).abs() < 1e-5, "lightness changes add");
    }

    #[test]
    fn near_neutral_colours_keep_their_lightness() {
        let mut s = v9();
        s.mixer.red.lum = 100.0;
        s.mixer.blue.sat = 60.0;
        let st = MixerStage::new(&s).expect("stage");
        for g in [0.02f32, 0.18, 0.7] {
            let out = st.apply([g; 3]);
            assert!(out.iter().all(|v| (v - g).abs() < 5e-4 * g.max(0.1)), "{g}: {out:?}");
        }
    }
}
