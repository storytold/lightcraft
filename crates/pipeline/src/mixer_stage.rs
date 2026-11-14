//! Process V9's mixer stage ([`lightcraft_develop::Process::V9`], `docs/mixer-stage.md`): the colour mixer,
//! applied the way Lightroom applies it.
//!
//! Measured on Camera Raw renders of synthetic OkLCh charts (`tools/lr_mixer_charts.py`):
//! - Each slider's effect is a table of Camera Raw's measured response (`mixer_stage.bin`): per OkLab hue (72 bins of
//!   5°), chroma level and lightness level (3 × 3), the hue shift, the relative chroma change and the lightness
//!   change, at ±25, ±50, ±75 and ±100. Amounts in between are interpolated linearly, towards 0 below 25.
//! - Lightroom's bands differ from V8's: their centres sit elsewhere on the hue circle (Orange at OkLab hue 50, not
//!   82), Blue is wider and Red and Magenta narrower, and the response grows faster than linearly with the amount.
//! - All 24 sliders act on the same input colour: their hue and lightness changes add and their chroma changes
//!   multiply.
//! - Lightroom applies the mixer before the tone curves: predicting charts with a strong curve and the mixer from the
//!   single-setting renders leaves 1.17 (OkLab × 100) in this order, 1.66 with the mixer after the curves.
//!
//! The stage runs after the tone map and Process V4's calibrated Contrast and Blacks, before the curve stage
//! ([`crate::curve_stage`]). It replaces V8's mixer in [`crate::colorops::ColorOps`].

use lightcraft_color::perceptual::{lab_to_lch, lch_to_lab, oklab_from_2020, oklab_to_2020};
use lightcraft_develop::{DevelopSettings, Process};

/// The measured table, written by `tools/lr_mixer_charts.py table` (format in its docstring).
static TABLE: &[u8] = include_bytes!("mixer_stage.bin");

/// Hue bins per table row (5° each, from OkLab hue 0).
pub const HUES: usize = 72;
/// Table rows per output: 3 lightness levels × 3 chroma levels, lightness major.
pub const ROWS: usize = 9;
/// Outputs per row: hue shift, chroma, lightness change.
const OUTPUTS: usize = 3;
/// Values of the combined table: [`ROWS`] × outputs × [`HUES`].
pub const TABLE_LEN: usize = ROWS * OUTPUTS * HUES;
/// The measured amounts.
const AMOUNTS: [f32; 8] = [-100.0, -75.0, -50.0, -25.0, 25.0, 50.0, 75.0, 100.0];
/// Sliders in the table: mixer bands (red … magenta) × (hue, saturation, luminance).
const SLIDERS: usize = 24;
const HEADER: usize = 4 + 6 * 4 + AMOUNTS.len() * 4 + 12 * 4;
const ROW_BYTES: usize = 4 + HUES;

/// The stage's combined response table: per row (lightness level × 3 + chroma level), output and hue bin, the hue
/// shift (radians), the chroma factor and the lightness change of every slider together.
#[derive(Clone, Debug)]
pub struct MixerStage {
    /// Mean OkLab chroma of the measured chroma levels on Camera Raw's neutral chart, per lightness level (its
    /// neutral render desaturates bright colours: the patches of one nominal chroma differ by about 3× in chroma).
    pub chroma: [[f32; 3]; 3],
    /// Mean OkLab lightness of the measured lightness levels.
    pub light: [f32; 3],
    pub table: Box<[f32; TABLE_LEN]>,
}

/// The little-endian `f32` at byte `off` of the table, if it is inside.
fn f32_at(off: usize) -> Option<f32> {
    TABLE.get(off..off + 4).and_then(|b| b.try_into().ok()).map(f32::from_le_bytes)
}

/// The little-endian `u32` at byte `off` of the table, if it is inside.
fn u32_at(off: usize) -> Option<u32> {
    TABLE.get(off..off + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes)
}

/// The table's level keys (chroma per lightness level, lightness), when its header and size match this code.
fn levels() -> Option<([[f32; 3]; 3], [f32; 3])> {
    let dims = [1, SLIDERS, AMOUNTS.len(), ROWS, OUTPUTS, HUES];
    let header_ok = TABLE.get(..4) == Some(b"LCCS")
        && dims.iter().enumerate().all(|(i, d)| u32_at(4 + 4 * i) == Some(*d as u32))
        && AMOUNTS.iter().enumerate().all(|(i, a)| f32_at(28 + 4 * i) == Some(*a));
    if !header_ok || TABLE.len() != HEADER + SLIDERS * AMOUNTS.len() * ROWS * OUTPUTS * ROW_BYTES {
        return None;
    }
    let at = |i: usize| f32_at(28 + 4 * AMOUNTS.len() + 4 * i);
    let chroma = [[at(0)?, at(1)?, at(2)?], [at(3)?, at(4)?, at(5)?], [at(6)?, at(7)?, at(8)?]];
    let light = [at(9)?, at(10)?, at(11)?];
    Some((chroma, light))
}

/// Measured response of `slider` at measured amount `amount`, row `row`, output `out`, hue bin `bin`.
fn measured(slider: usize, amount: usize, row: usize, out: usize, bin: usize) -> f32 {
    let off = HEADER + (((slider * AMOUNTS.len() + amount) * ROWS + row) * OUTPUTS + out) * ROW_BYTES;
    let scale = f32_at(off).unwrap_or(0.0);
    TABLE.get(off + 4 + bin).map_or(0.0, |q| scale * f32::from(i8::from_le_bytes([*q])))
}

/// The two measured amounts around `v` and the weight of the upper: `None` stands for the unedited 0.
fn bracket(v: f32) -> (Option<usize>, Option<usize>, f32) {
    let v = v.clamp(AMOUNTS[0], AMOUNTS[AMOUNTS.len() - 1]);
    let mut keys: Vec<(f32, Option<usize>)> = AMOUNTS.iter().enumerate().map(|(i, a)| (*a, Some(i))).collect();
    keys.push((0.0, None));
    keys.sort_by(|a, b| a.0.total_cmp(&b.0));
    for w in keys.windows(2) {
        if let [k0, k1] = w
            && v >= k0.0
            && v <= k1.0
        {
            let f = if k1.0 > k0.0 { (v - k0.0) / (k1.0 - k0.0) } else { 0.0 };
            return (k0.1, k1.1, f);
        }
    }
    (None, None, 0.0)
}

/// The combined table of `(slider, amount)` pairs: hue and lightness changes add, chroma changes multiply.
fn combine(sliders: &[(usize, f32)]) -> Option<Box<[f32; TABLE_LEN]>> {
    // each slider's two measured amounts and weight, found once
    let active: Vec<(usize, (Option<usize>, Option<usize>, f32))> =
        sliders.iter().filter(|(_, v)| *v != 0.0 && v.is_finite()).map(|(s, v)| (*s, bracket(*v))).collect();
    if active.is_empty() {
        return None;
    }
    let mut t = Box::new([0.0f32; TABLE_LEN]);
    for row in 0..ROWS {
        for out in 0..OUTPUTS {
            for bin in 0..HUES {
                let mut acc = if out == 1 { 1.0 } else { 0.0 };
                for (slider, (a0, a1, f)) in &active {
                    let (a0, a1, f) = (*a0, *a1, *f);
                    let at = |a: Option<usize>| a.map_or(0.0, |a| measured(*slider, a, row, out, bin));
                    let r = at(a0) * (1.0 - f) + at(a1) * f;
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

/// Position of `x` between the three measured levels `keys`: (lower, upper, weight of the upper), clamped.
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
        let (chroma, light) = levels()?;
        let sliders: Vec<(usize, f32)> = s.mixer.bands().iter().flat_map(|b| [b.hue, b.sat, b.lum]).enumerate().map(|(i, v)| (i, v as f32)).collect();
        combine(&sliders).map(|table| MixerStage { chroma, light, table })
    }

    /// The (hue shift, chroma factor, lightness change) for OkLCh `l`, `c`, `h`: linear in hue, bilinear in the chroma
    /// and lightness levels, clamped to the measured ones. Below the lowest measured chroma the lightness change fades
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

    #[test]
    fn the_table_matches_this_code() {
        let (chroma, light) = levels().expect("table header");
        assert!(chroma.iter().all(|c| c.windows(2).all(|w| w[0] < w[1])) && light.windows(2).all(|w| w[0] < w[1]), "{chroma:?} {light:?}");
        // every scale is finite and the values stay within plausible ranges
        for slider in 0..SLIDERS {
            for a in 0..AMOUNTS.len() {
                for row in 0..ROWS {
                    for out in 0..OUTPUTS {
                        let m = (0..HUES).map(|b| measured(slider, a, row, out, b).abs()).fold(0.0, f32::max);
                        // Blue Luminance −100 darkens pale blues and raises their chroma about sixfold
                        let limit = [180.0, 8.0, 0.5][out];
                        assert!(m.is_finite() && m <= limit, "slider {slider} amount {a} row {row} out {out}: {m}");
                    }
                }
            }
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
        // Lightroom's Orange centre sits near OkLab hue 51
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
        // Lightroom's Blue Hue −100 turns blue towards aqua (lower OkLab hue)
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
        assert!((a - b * 0.4).abs() < 0.05 * b.max(1.0), "below 25 the effect fades linearly: {a} vs {b}");
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
        // on a measured node (lightness and chroma level 1, hue bin 11), where no interpolation blurs the rule
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
