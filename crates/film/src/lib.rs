//! Film-negative conversion: a scanned colour or black-and-white negative to a positive.
//!
//! Ported from Film Lab (github.com/smyrgeorge/film-lab, by the same author), whose PhotoCraft
//! plug-ins run the same maths. Hosts hand over RGB samples in 0..1 and say how they are encoded
//! ([`Encoding`]); this crate gives back the positive. Nothing in here knows a host's pixel layout;
//! [`measure`] analyses a whole scan once, so every render (a preview, a zoomed window, an export)
//! uses the same numbers.
//!
//! # The model
//!
//! A negative records scene exposure as density. In the straight part of its characteristic
//! curve, density grows by [`NEGATIVE_GAMMA`] per stop, so the light a scanner sees through the
//! film falls as the scene brightens. Reading that back:
//!
//! 1. **Film base.** The unexposed film (the border, orange on colour negative film) is the
//!    brightest the scan gets and stands for scene black. Every channel is measured relative to
//!    it, which also removes the orange mask ([`film::exposure`]).
//! 2. **Levels.** Each channel's density range between its black and white points is mapped onto
//!    the same [`RANGE_STOPS`] of scene brightness, so channels of unequal range (a scan whose
//!    blue runs much denser than its red) come out balanced, and black and white land on 0 and 1
//!    ([`Analysis`]: the base for black and the fixed range for white when only the base is
//!    known, or scan values for black and white from an analysis of the image).
//! 3. **Colour** in linear light ([`look::Colour`]): exposure, white balance (sliders, a preset,
//!    a neutral sample), the colour model, pre-saturation, mid-tone balance; black-and-white.
//! 4. **Tone** ([`tone::Curve`]): white and black points, brightness as a gamma, then in display
//!    (sRGB-encoded) space a tone profile, contrast, lights / darks, fade / glow and soft ends.
//! 5. **Toning** ([`look::Toning`]): highlight and shadow colour balance and saturation on the
//!    encoded result.
//!
//! The workflow this serves is the usual one for negatives: sample the film border, exclude
//! the border and sprocket holes from the evaluated area, convert, then edit.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod analysis;
pub mod color;
pub mod film;
pub mod look;
pub mod measure;
pub mod preset;
pub mod tone;

pub use analysis::{Analysis, ValueHistogram};
pub use color::Encoding;
pub use look::{Look, WbPreset};
pub use measure::{MeasureOptions, Measurement};
pub use preset::{PRESETS, Preset, SCAN_SPREAD, Sliders};
pub use tone::Tone;

/// The negative's contrast: density per log-exposure in the straight part of its characteristic
/// curve. Colour negative film sits near 0.6; the brightness and contrast controls absorb the
/// difference for other stocks.
pub const NEGATIVE_GAMMA: f32 = 0.6;

/// Stops of scene brightness between the film base and the positive's white when only the base
/// is known (a density range of about 1.1 at [`NEGATIVE_GAMMA`], typical of a well-exposed
/// colour negative).
pub const RANGE_STOPS: f32 = 6.0;

/// Stops the density range between the black and white points is rendered over by default
/// ([`Params::range`]). Fewer stops than the film's own [`RANGE_STOPS`] render mid-tones the way
/// a lab scan does: measured against a lab-converted reference, 3 matches its mid-tones; 6 is
/// the scene-linear rendition.
pub const DISPLAY_STOPS: f32 = 3.0;

/// Channel indices in the RGB triples.
pub const RED: usize = 0;
pub const GREEN: usize = 1;
pub const BLUE: usize = 2;

/// The user's edits. Slider ranges follow the Convert Negative panel: ±50 for the bipolar controls,
/// 0..100 for amounts, exposure in stops, pre-saturation 1..5.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// Stops of scene brightness mapped between the black and white points (default
    /// [`DISPLAY_STOPS`]); fewer stops render mid-tones brighter, as lab scans do, and
    /// [`RANGE_STOPS`] is the scene-linear rendition.
    pub range: f32,
    /// Stops; positive brightens the whole positive.
    pub exposure: f32,
    /// Mid-tone gamma, −50..50; positive brightens mid-tones without moving black or white.
    pub brightness: f32,
    /// Added to the tone profile's contrast, −50..50.
    pub contrast: f32,
    pub tone: Tone,
    /// −50..50; positive brightens the lighter tones.
    pub lights: f32,
    /// −50..50; positive brightens the darker tones.
    pub darks: f32,
    /// −50..50; moves the white point (positive brightens whites; clipped detail stays clipped).
    pub whites: f32,
    /// −50..50; moves the black point (positive lifts it for a faded look).
    pub blacks: f32,
    /// −100..100: positive lifts black for smoother shadows with no pure black; negative
    /// expands the shadow tonality.
    pub fade: f32,
    /// −100..100: positive compresses the highlights into a soft shoulder; negative expands them.
    pub glow: f32,
    /// 0..100: tonal roll-off into white.
    pub soft_highs: f32,
    /// 0..100: tonal roll-off into black.
    pub soft_lows: f32,
    pub wb: WbPreset,
    /// −50..50; positive is warmer.
    pub temp: f32,
    /// −50..50; positive is more magenta, negative more green.
    pub tint: f32,
    /// Per-channel mid-tone gammas (0.4..2.5, 1 = none): a colour balance that leaves black and
    /// white alone, what an automatic neutral balance computes from the channel medians.
    pub balance: [f32; 3],
    /// 1..5, default 3: saturation before the tone curve, which also sets colour separation.
    pub presat: f32,
    /// −50..50: saturation after the tone curve.
    pub saturation: f32,
    pub look: Look,
    /// Mid-tone colour balance, −50..50 each: red / cyan, green / magenta, blue / yellow.
    pub mids_red: f32,
    pub mids_green: f32,
    pub mids_blue: f32,
    /// Highlight toning balance, −50..50 each, and how far down it reaches (0..100).
    pub highs_red: f32,
    pub highs_green: f32,
    pub highs_blue: f32,
    pub highs_range: f32,
    /// Shadow toning balance, −50..50 each, and how far up it reaches (0..100).
    pub lows_red: f32,
    pub lows_green: f32,
    pub lows_blue: f32,
    pub lows_range: f32,
    /// Black-and-white film: the positive is the luminance (toning still colours it).
    pub monochrome: bool,
    /// Density per log-exposure of the red, green and blue channels of the scan, for levels
    /// derived from a base alone ([`Analysis::from_base`]); a measured range makes it irrelevant.
    /// A preset sets it to its [`Preset::scan_gamma`].
    pub film_gamma: [f32; 3],
}

impl Default for Params {
    fn default() -> Self {
        Self {
            range: DISPLAY_STOPS,
            exposure: 0.0,
            brightness: 0.0,
            contrast: 0.0,
            tone: Tone::LabStandard,
            lights: 0.0,
            darks: 0.0,
            whites: 0.0,
            blacks: 0.0,
            fade: 0.0,
            glow: 0.0,
            soft_highs: 0.0,
            soft_lows: 0.0,
            wb: WbPreset::AsIs,
            temp: 0.0,
            tint: 0.0,
            balance: [1.0; 3],
            presat: 3.0,
            saturation: 0.0,
            look: Look::Neutral,
            mids_red: 0.0,
            mids_green: 0.0,
            mids_blue: 0.0,
            highs_red: 0.0,
            highs_green: 0.0,
            highs_blue: 0.0,
            highs_range: 30.0,
            lows_red: 0.0,
            lows_green: 0.0,
            lows_blue: 0.0,
            lows_range: 30.0,
            monochrome: false,
            film_gamma: [NEGATIVE_GAMMA; 3],
        }
    }
}

/// A conversion ready to run on pixels of one encoding: the analysis and the edits folded into
/// per-channel factors, one exponent, one colour pipeline and one tone curve. [`Converter::convert`]
/// is the exact path (two `powf` per channel, and an `expf` when the curve has contrast);
/// [`Converter::convert_with`] runs through [`Tables`] instead, with no transcendental call per
/// sample, for hosts that meter instructions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Converter {
    enc: Encoding,
    /// Per channel, the normalised exposure `2^(u · RANGE_STOPS)` of a scan value `t` is
    /// `factor · t^(−exponent)`, where `u` is the position of its density between the black and
    /// white points (0..1): levels in density space folded into one `powf`.
    factor: [f32; 3],
    exponent: [f32; 3],
    /// `gain / (2^RANGE_STOPS − 1)`: the normalised exposure to a positive, exposure folded in.
    scale: f32,
    colour: look::Colour,
    monochrome: bool,
    /// White point as a gain on the linear positive.
    white_gain: f32,
    /// Black point: lifted (positive) or crushed (negative), as a fraction of the range.
    black_point: f32,
    /// Exponent applied to the levelled positive per channel (brightness and balance).
    gamma: [f32; 3],
    /// [`color::encode_pow_knee`] of each `gamma`.
    knee: [f32; 3],
    curve: tone::Curve,
    toning: look::Toning,
}

impl Converter {
    /// `neutral` is a scan sample (host encoding) of something that should come out grey, if the
    /// user picked one with the colour picker.
    pub fn new(a: &Analysis, p: &Params, enc: Encoding, neutral: Option<[f32; 3]>) -> Self {
        let gain = libm::exp2f(finite(p.exposure, 0.0).clamp(-10.0, 10.0));
        let stops = finite(p.range, DISPLAY_STOPS).clamp(1.0, 10.0);
        let scale = gain / (libm::exp2f(stops) - 1.0);
        let mut factor = [0.0; 3];
        let mut exponent = [0.0; 3];
        for ch in 0..3 {
            // Stops between the black and white points; the base-only default is RANGE_STOPS.
            let black = if a.black[ch].is_finite() { a.black[ch].max(1.0) } else { 1.0 };
            let white = if a.white[ch].is_finite() { a.white[ch] } else { 0.0 };
            let range = libm::log2f(white) - libm::log2f(black);
            let range = if range.is_finite() && range > 1e-3 { range } else { RANGE_STOPS };
            // 2^(u · S) = (E / black)^(S / range) = base^(S/(γ range)) · black^(−S/range) · t^(−S/(γ range)).
            let e = stops / (NEGATIVE_GAMMA * range);
            exponent[ch] = e;
            let base = if a.base[ch].is_finite() { a.base[ch].max(film::MIN_TRANSMITTANCE) } else { film::MIN_TRANSMITTANCE };
            factor[ch] = libm::powf(base, e) * libm::powf(black, -stops / range);
        }
        let levelled = |neg: [f32; 3]| {
            let mut out = [0.0; 3];
            for ch in 0..3 {
                let e = film::exposure_encoded(neg[ch], enc, factor[ch], exponent[ch]);
                out[ch] = ((e - 1.0) * scale).max(0.0);
            }
            out
        };
        // Brightness, the profile's mid-tone gamma and the per-channel balance as one exponent per
        // channel on the linear positive (a gamma in encoded space is close to the same gamma on
        // the linear value, after encoding).
        let brightness = libm::exp2f(-finite(p.brightness, 0.0).clamp(-50.0, 50.0) / 50.0) * p.tone.gamma();
        // The per-channel balance corrects a colour cast; a monochrome positive has none, and
        // unequal gammas would tint it.
        let balance = if p.monochrome { [1.0; 3] } else { p.balance };
        let mut gamma = [0.0; 3];
        let mut knee = [0.0; 3];
        for ch in 0..3 {
            gamma[ch] = brightness * finite(balance[ch], 1.0).clamp(0.4, 2.5);
            knee[ch] = color::encode_pow_knee(gamma[ch]);
        }
        Self {
            enc,
            factor,
            exponent,
            scale,
            colour: look::Colour::new(p, neutral.map(levelled)),
            monochrome: p.monochrome,
            white_gain: libm::exp2f(finite(p.whites, 0.0).clamp(-50.0, 50.0) / 100.0),
            black_point: finite(p.blacks, 0.0).clamp(-50.0, 50.0) / 100.0 * 0.2,
            gamma,
            knee,
            curve: tone::Curve::new(p),
            toning: look::Toning::new(p),
        }
    }

    pub fn encoding(&self) -> Encoding {
        self.enc
    }

    /// The levelled linear positive of channel `ch`'s scan value, exposure folded in, capped at
    /// [`LEVELS_CAP`].
    fn levelled(&self, v: f32, ch: usize) -> f32 {
        let e = film::exposure_encoded(v, self.enc, self.factor[ch], self.exponent[ch]);
        ((e - 1.0) * self.scale).clamp(0.0, LEVELS_CAP)
    }

    /// The white and black points on a linear positive, clamped to 0..1.
    fn points(&self, v: f32) -> f32 {
        let mut x = v * self.white_gain;
        if self.black_point > 0.0 {
            x = self.black_point + x * (1.0 - self.black_point);
        } else if self.black_point < 0.0 {
            x = (x + self.black_point) / (1.0 + self.black_point);
        }
        x.clamp(0.0, 1.0)
    }

    /// The colour stage, monochrome and the points on a levelled positive.
    fn graded(&self, mut p: [f32; 3]) -> [f32; 3] {
        p = self.colour.apply(p);
        if self.monochrome {
            p = [color::luminance(p).max(0.0); 3];
        }
        for v in &mut p {
            *v = self.points(*v);
        }
        p
    }

    /// One RGB sample of the negative, in the converter's encoding, as the linear positive before
    /// the gammas and the tone curve: levels, exposure, colour, the white and black points.
    fn positive(&self, neg: [f32; 3]) -> [f32; 3] {
        let mut p = [0.0f32; 3];
        for (ch, (v, n)) in p.iter_mut().zip(neg).enumerate() {
            *v = self.levelled(n, ch);
        }
        self.graded(p)
    }

    /// Channel `ch` of a linear positive (after the points) as the sRGB-encoded sample before
    /// toning: brightness (a gamma) and the tone curve, which act in sRGB-encoded space.
    fn encoded(&self, p: f32, ch: usize) -> f32 {
        self.curve.apply(color::srgb_encode_pow(p, self.gamma[ch], self.knee[ch]))
    }

    /// The toning and the host's encoding on an sRGB-encoded positive.
    fn finished(&self, mut out: [f32; 3]) -> [f32; 3] {
        out = self.toning.apply(out);
        match self.enc {
            Encoding::Srgb => out,
            Encoding::Linear => [color::srgb_decode(out[0]), color::srgb_decode(out[1]), color::srgb_decode(out[2])],
        }
    }

    /// One RGB sample of the negative, in the converter's encoding, as the positive in the same
    /// encoding, 0..1: the exact path, two `powf` per channel.
    pub fn convert(&self, neg: [f32; 3]) -> [f32; 3] {
        let p = self.positive(neg);
        if self.enc == Encoding::Linear && self.curve.is_identity() && self.toning.is_identity {
            // Encoding and decoding again would cancel: a gamma on the linear value.
            return [libm::powf(p[0], self.gamma[0]), libm::powf(p[1], self.gamma[1]), libm::powf(p[2], self.gamma[2])];
        }
        self.finished([self.encoded(p[0], 0), self.encoded(p[1], 1), self.encoded(p[2], 2)])
    }

    /// Channel `ch` of a linear positive as the output table holds it: sRGB-encoded, or in the
    /// host's encoding when `folded` (no toning to follow).
    fn output_entry(&self, p: f32, ch: usize, folded: bool) -> f32 {
        let out = self.encoded(p, ch);
        if folded && self.enc == Encoding::Linear { color::srgb_decode(out) } else { out }
    }

    /// Where a scan value sits in the levels table: as it is for an sRGB host, whose samples are
    /// already spaced the way the levels curve bends; by its square root for a linear host, so
    /// that the dense end (small values, the highlights) gets most of the entries.
    fn levels_index(&self, v: f32) -> f32 {
        match self.enc {
            Encoding::Srgb => v,
            Encoding::Linear => libm::sqrtf(v.max(0.0)),
        }
    }

    /// Fills `t` with this conversion's transfer functions, for [`Converter::convert_with`].
    pub fn tables(&self, t: &mut Tables) {
        let step = 1.0 / (TABLE_SIZE - 1) as f32;
        t.folded = self.toning.is_identity;
        for (ch, (levels, output)) in t.levels.iter_mut().zip(t.output.iter_mut()).enumerate() {
            // The scan value at which the relative exposure is 1: the black point.
            let black = libm::powf(self.factor[ch], 1.0 / self.exponent[ch]);
            t.black[ch] = self.levels_index(match self.enc {
                Encoding::Srgb => color::srgb_encode(black),
                Encoding::Linear => black,
            });
            for (i, v) in levels.iter_mut().enumerate() {
                let u = i as f32 * step;
                let scan = match self.enc {
                    Encoding::Srgb => u,
                    Encoding::Linear => u * u,
                };
                // Unfloored, so the entries run straight through the black point and an
                // interpolation lands on it; `convert_with` floors the result.
                let e = film::exposure_encoded_raw(scan, self.enc, self.factor[ch], self.exponent[ch]);
                *v = ((e - 1.0) * self.scale).min(LEVELS_CAP);
            }
            for (i, v) in output.iter_mut().enumerate() {
                *v = self.output_entry(i as f32 * step, ch, t.folded);
            }
        }
    }

    /// [`Converter::convert`] through `t`, filled by [`Converter::tables`] for this converter:
    /// the same positive to within the tables' interpolation (under a thousandth of the range)
    /// with no transcendental call per sample, which is what counts under an interpreter. The
    /// darkest inputs, where a steep gamma or a dense highlight outruns any table, take the
    /// exact path: few pixels, so little cost.
    pub fn convert_with(&self, t: &Tables, neg: [f32; 3]) -> [f32; 3] {
        let index = match self.enc {
            Encoding::Srgb => neg,
            Encoding::Linear => [libm::sqrtf(neg[0].max(0.0)), libm::sqrtf(neg[1].max(0.0)), libm::sqrtf(neg[2].max(0.0))],
        };
        let mut p = [0.0f32; 3];
        for (ch, (v, (table, (n, u)))) in p.iter_mut().zip(t.levels.iter().zip(neg.into_iter().zip(index))).enumerate() {
            // At and past the black point (and for NaN) the positive is 0; the darkest inputs
            // take the exact path; the rest interpolate.
            *v = if u >= t.black[ch] || u.is_nan() {
                0.0
            } else if u < EXACT_BELOW {
                self.levelled(n, ch)
            } else {
                lookup(table, u).max(0.0)
            };
        }
        p = self.graded(p);
        let mut out = [0.0f32; 3];
        for (ch, (o, (table, v))) in out.iter_mut().zip(t.output.iter().zip(p)).enumerate() {
            *o = if v < EXACT_BELOW { self.output_entry(v, ch, t.folded) } else { lookup(table, v) };
        }
        if t.folded { out } else { self.finished(out) }
    }
}

/// Entries per channel in [`Tables`].
pub const TABLE_SIZE: usize = 2048;

/// Where the levelled positive stops: four stops over white, deep in what the white point clips.
/// Without it a scan value near zero puts a channel at 10⁸ and the colour stage's rounding
/// then zeroes the other two.
const LEVELS_CAP: f32 = 16.0;

/// Inputs below this (the tables' first eight entries) take the exact path in
/// [`Converter::convert_with`]: a gamma of 0.2 or a highlight denser than the second entry
/// changes faster there than an interpolation can follow.
const EXACT_BELOW: f32 = 8.0 / (TABLE_SIZE - 1) as f32;

/// The per-channel transfer functions of a [`Converter`] sampled over their input range, so that
/// a sample costs an interpolation per channel and stage instead of a `powf`. 48 KiB: keep one
/// in static memory and refill it per converter with [`Converter::tables`].
pub struct Tables {
    /// Host-encoded scan value (0..1; its square root for a linear host) → levelled linear
    /// positive, exposure folded in, unfloored (negative past the black point).
    levels: [[f32; TABLE_SIZE]; 3],
    /// Linear positive after the colour stage and the points (0..1) → the output sample: the
    /// brightness gamma and the tone curve, and the host's encoding when `folded`.
    output: [[f32; TABLE_SIZE]; 3],
    /// Where in `levels` each channel reaches its black point: at and past it the positive is
    /// exactly 0, as on the exact path, rather than an interpolation's few millionths (which a
    /// lifting gamma would make visible).
    black: [f32; 3],
    /// The toning (which mixes channels, so cannot be a table) is identity, and `output` ends
    /// in the host's encoding; otherwise it ends sRGB-encoded for the toning to follow.
    folded: bool,
}

impl Tables {
    pub const fn blank() -> Self {
        Self { levels: [[0.0; TABLE_SIZE]; 3], output: [[0.0; TABLE_SIZE]; 3], black: [0.0; 3], folded: false }
    }
}

impl Default for Tables {
    fn default() -> Self {
        Self::blank()
    }
}

/// `table` at `x` (0..1, finite), linearly interpolated.
fn lookup(table: &[f32; TABLE_SIZE], x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0) * (TABLE_SIZE - 1) as f32;
    let i = x as usize;
    match (table.get(i), table.get(i + 1)) {
        (Some(a), Some(b)) => a + (b - a) * (x - i as f32),
        (Some(a), None) => *a,
        _ => 0.0,
    }
}

/// `v`, or `fallback` when it is NaN or infinite.
pub(crate) fn finite(v: f32, fallback: f32) -> f32 {
    if v.is_finite() { v } else { fallback }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: [f32; 3] = [0.6, 0.35, 0.15];

    /// No edits and the scene-linear rendition: the film model's round trip.
    fn neutral() -> Params {
        Params { tone: Tone::Linear, range: RANGE_STOPS, ..Params::default() }
    }

    /// The linear scan value a scene value `s` (0..1 of the positive's range) leaves, per channel.
    fn negative(s: f32, ch: usize) -> f32 {
        let white = libm::exp2f(RANGE_STOPS);
        film::transmittance(1.0 + s * (white - 1.0), BASE[ch])
    }

    fn scan(s: f32, enc: Encoding) -> [f32; 3] {
        [enc.encode(negative(s, 0)), enc.encode(negative(s, 1)), enc.encode(negative(s, 2))]
    }

    fn convert(p: &Params, s: f32) -> [f32; 3] {
        Converter::new(&Analysis::manual(BASE), p, Encoding::Srgb, None).convert(scan(s, Encoding::Srgb))
    }

    #[test]
    fn manual_conversion_inverts_the_film_model_in_both_encodings() {
        for enc in [Encoding::Srgb, Encoding::Linear] {
            let c = Converter::new(&Analysis::manual(BASE), &neutral(), enc, None);
            assert_eq!(c.encoding(), enc);
            for s in [0.0, 0.01, 0.1, 0.18, 0.5, 0.9, 1.0] {
                let out = c.convert(scan(s, enc));
                let want = enc.encode(s);
                for v in out {
                    assert!((v - want).abs() < 2e-3, "{enc:?} scene {s}: got {out:?}, want {want}");
                }
            }
        }
    }

    #[test]
    fn base_is_black_and_brighter_than_base_stays_black() {
        let c = Converter::new(&Analysis::manual(BASE), &neutral(), Encoding::Linear, None);
        assert_eq!(c.convert(BASE), [0.0; 3]);
        assert_eq!(c.convert([0.9, 0.9, 0.9]), [0.0; 3]);
        // Nothing transmitted at all: the brightest possible scene tone.
        assert!(c.convert([0.0; 3]).iter().all(|v| (v - 1.0).abs() < 1e-6));
    }

    #[test]
    fn exposure_brightness_whites_and_blacks_move_tones_the_right_way() {
        let base = convert(&neutral(), 0.18)[0];
        let up = convert(&Params { exposure: 1.0, ..neutral() }, 0.18)[0];
        let bright = convert(&Params { brightness: 25.0, ..neutral() }, 0.18)[0];
        let dark = convert(&Params { brightness: -25.0, ..neutral() }, 0.18)[0];
        assert!(up > base && bright > base && dark < base, "{base} {up} {bright} {dark}");
        assert!((color::srgb_decode(up) / color::srgb_decode(base) - 2.0).abs() < 0.02, "one stop doubles linear light");
        // Brightness is a gamma: black and white stay put.
        assert_eq!(convert(&Params { brightness: 40.0, ..neutral() }, 0.0)[0], 0.0);
        assert!((convert(&Params { brightness: 40.0, ..neutral() }, 1.0)[0] - 1.0).abs() < 1e-5);
        // Whites scale towards white, blacks lift or crush the black point.
        assert!(convert(&Params { whites: 50.0, ..neutral() }, 0.5)[0] > convert(&neutral(), 0.5)[0]);
        assert!(convert(&Params { whites: 50.0, ..neutral() }, 0.0)[0] == 0.0);
        assert!(convert(&Params { blacks: 50.0, ..neutral() }, 0.0)[0] > 0.05, "lifted black");
        assert!(convert(&Params { blacks: -50.0, ..neutral() }, 0.05)[0] == 0.0, "crushed black");
        assert!((convert(&Params { blacks: -50.0, ..neutral() }, 1.0)[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn white_balance_presets_looks_and_monochrome() {
        let warm = convert(&Params { temp: 50.0, ..neutral() }, 0.3);
        assert!(warm[0] > warm[1] && warm[1] > warm[2], "{warm:?}");
        let magenta = convert(&Params { tint: 50.0, ..neutral() }, 0.3);
        assert!(magenta[1] < magenta[0] && magenta[1] < magenta[2], "{magenta:?}");
        let preset = convert(&Params { wb: WbPreset::Warm, ..neutral() }, 0.3);
        assert!(preset[0] > preset[2], "{preset:?}");
        let lab = convert(&Params { look: Look::LabWarm, ..neutral() }, 0.3);
        assert!(lab[0] > lab[2], "{lab:?}");
        assert!(convert(&Params { tone: Tone::LinearDeep, ..neutral() }, 0.3)[0] < convert(&neutral(), 0.3)[0], "deep darkens mid-tones");
        let mono = convert(&Params { monochrome: true, temp: 50.0, ..neutral() }, 0.3);
        assert!(mono[0] == mono[1] && mono[1] == mono[2], "{mono:?}");
        let toned = convert(&Params { monochrome: true, lows_blue: 50.0, lows_range: 100.0, ..neutral() }, 0.1);
        assert!(toned[2] > toned[0], "toning colours a black-and-white positive: {toned:?}");
        let grey = convert(&Params { saturation: -50.0, temp: 50.0, ..neutral() }, 0.3);
        assert!((grey[0] - grey[2]).abs() < 1e-5, "{grey:?}");
    }

    #[test]
    fn unequal_channel_ranges_are_balanced_by_the_levels() {
        // A scan whose blue runs twice as dense as its red for the same scene: with black and
        // white points per channel, a scene grey still comes out grey at every tone.
        let stops = [4.0f32, 6.0, 8.0];
        let scan = |s: f32| {
            let mut v = [0.0; 3];
            for ch in 0..3 {
                v[ch] = Encoding::Srgb.encode(film::transmittance(libm::exp2f(s * stops[ch]), BASE[ch]));
            }
            v
        };
        let a = Analysis::from_scan_values(
            [Encoding::Srgb.encode(BASE[0]), Encoding::Srgb.encode(BASE[1]), Encoding::Srgb.encode(BASE[2])],
            scan(0.0),
            scan(1.0),
            Encoding::Srgb,
        )
        .unwrap();
        let c = Converter::new(&a, &neutral(), Encoding::Srgb, None);
        for s in [0.0, 0.2, 0.5, 0.8, 1.0] {
            let out = c.convert(scan(s));
            assert!((out[0] - out[1]).abs() < 4e-3 && (out[1] - out[2]).abs() < 4e-3, "scene {s}: {out:?}");
            let want = color::srgb_encode((libm::exp2f(s * RANGE_STOPS) - 1.0) / (libm::exp2f(RANGE_STOPS) - 1.0));
            assert!((out[1] - want).abs() < 4e-3, "scene {s}: {out:?} vs {want}");
        }
    }

    #[test]
    fn range_and_balance_move_midtones_without_moving_the_ends() {
        let wide = convert(&neutral(), 0.18)[0];
        let narrow = convert(&Params { range: 3.0, ..neutral() }, 0.18)[0];
        assert!(narrow > wide, "fewer stops between black and white render mid-tones brighter: {narrow} vs {wide}");
        assert_eq!(Params::default().range, DISPLAY_STOPS);
        assert_eq!(convert(&Params { range: 3.0, ..neutral() }, 0.0)[0], 0.0);
        assert!((convert(&Params { range: 3.0, ..neutral() }, 1.0)[0] - 1.0).abs() < 1e-5);
        let balanced = convert(&Params { balance: [0.8, 1.0, 1.25], ..neutral() }, 0.3);
        assert!(balanced[0] > balanced[1] && balanced[1] > balanced[2], "{balanced:?}");
        assert_eq!(convert(&Params { balance: [0.8, 1.0, 1.25], ..neutral() }, 0.0), [0.0; 3]);
        assert!(convert(&Params { balance: [0.8, 1.0, 1.25], ..neutral() }, 1.0).iter().all(|v| (v - 1.0).abs() < 1e-5));
    }

    #[test]
    fn neutral_sample_balances_the_positive() {
        // A scene grey that the scan renders with a cast (a stronger red mask than the base says).
        let a = Analysis::manual(BASE);
        let cast = [0.8, 1.0, 1.0];
        let scan_of =
            |s: f32| [Encoding::Srgb.encode(negative(s, 0) * cast[0]), Encoding::Srgb.encode(negative(s, 1)), Encoding::Srgb.encode(negative(s, 2))];
        let before = Converter::new(&a, &neutral(), Encoding::Srgb, None).convert(scan_of(0.4));
        assert!(before[0] > before[1] + 0.02, "the cast shows: {before:?}");
        let after = Converter::new(&a, &neutral(), Encoding::Srgb, Some(scan_of(0.4))).convert(scan_of(0.4));
        assert!((after[0] - after[1]).abs() < 2e-3 && (after[1] - after[2]).abs() < 2e-3, "{after:?}");
    }

    #[test]
    fn a_balance_never_tints_a_monochrome_positive() {
        let p = Params { monochrome: true, balance: [0.7, 1.0, 1.5], ..neutral() };
        for s in [0.05, 0.3, 0.7] {
            let out = convert(&p, s);
            assert!((out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6, "{out:?}");
        }
        let colour = convert(&Params { balance: [0.7, 1.0, 1.5], ..neutral() }, 0.3);
        assert!(colour[0] > colour[2] + 0.02, "the same balance does tint a colour positive: {colour:?}");
    }

    #[test]
    fn tables_match_the_exact_conversion() {
        let mut t = std::boxed::Box::new(Tables::blank());
        let cases = [
            Params::default(),
            neutral(),
            Params {
                brightness: 20.0,
                contrast: 25.0,
                temp: 15.0,
                tint: -10.0,
                saturation: 20.0,
                lows_blue: 30.0,
                lows_range: 40.0,
                ..Params::default()
            },
            Params { monochrome: true, balance: [0.8, 1.0, 1.3], ..Params::default() },
            // The steepest gammas the sliders and a balance reach, and the hardest curve.
            Params { brightness: 50.0, balance: [0.4, 1.0, 2.5], contrast: 50.0, ..Params::default() },
            Params { brightness: -50.0, balance: [2.5, 1.0, 0.4], whites: 30.0, blacks: -20.0, ..Params::default() },
        ];
        for enc in [Encoding::Srgb, Encoding::Linear] {
            // The base alone, and a measurement whose blue highlight sits two entries from black.
            let dense = [0.2, 0.1, if enc == Encoding::Linear { 0.001 } else { 0.03 }];
            let analyses = [Analysis::manual(BASE), Analysis::from_scan_values(enc.encode3(BASE), enc.encode3(BASE), dense, enc).unwrap()];
            for (p, a) in cases.iter().flat_map(|p| analyses.iter().map(move |a| (p, a))) {
                let c = Converter::new(a, p, enc, None);
                c.tables(&mut t);
                // Compared as displayed: a linear host's output is sRGB-encoded first.
                let shown = |v: f32| if enc == Encoding::Linear { color::srgb_encode(v) } else { v };
                let (mut worst, mut at) = (0.0f32, [0.0f32; 3]);
                for i in 0..=255u32 {
                    for j in (0..=255u32).step_by(15) {
                        for k in (0..=255u32).step_by(15) {
                            let neg = [i as f32 / 255.0, j as f32 / 255.0, k as f32 / 255.0];
                            // A hair above the black point no path is stable: a lifting gamma
                            // turns the rounding of a millionth into percents.
                            if (0..3).any(|ch| (0.0..1e-5).contains(&c.levelled(neg[ch], ch)) && c.levelled(neg[ch], ch) > 0.0) {
                                continue;
                            }
                            let exact = c.convert(neg);
                            let fast = c.convert_with(&t, neg);
                            for ch in 0..3 {
                                let d = (shown(exact[ch]) - shown(fast[ch])).abs();
                                if d > worst {
                                    (worst, at) = (d, neg);
                                }
                            }
                        }
                    }
                }
                let (exact, fast) = (c.convert(at), c.convert_with(&t, at));
                std::eprintln!(
                    "{enc:?}, brightness {}, contrast {}, white {:?}: the tables stray by {worst} at {at:?}: {exact:?} vs {fast:?}",
                    p.brightness,
                    p.contrast,
                    a.white
                );
                assert!(worst < 1e-3, "{enc:?}: the tables stray by {worst} (an 8-bit level is 0.0039)");
            }
        }
        // Non-finite input is black, as on the exact path.
        let c = Converter::new(&Analysis::manual(BASE), &Params::default(), Encoding::Srgb, None);
        c.tables(&mut t);
        let odd = [f32::NAN, 0.5, f32::INFINITY];
        assert!(c.convert_with(&t, odd).iter().zip(c.convert(odd)).all(|(a, b)| (a - b).abs() < 1e-3));
    }

    #[test]
    fn tone_curves_reach_linear_documents_too() {
        let strong = Params { tone: Tone::LabHard, range: RANGE_STOPS, ..Params::default() };
        let lin = Converter::new(&Analysis::manual(BASE), &strong, Encoding::Linear, None).convert(scan(0.05, Encoding::Linear))[0];
        let srgb = Converter::new(&Analysis::manual(BASE), &strong, Encoding::Srgb, None).convert(scan(0.05, Encoding::Srgb))[0];
        assert!((color::srgb_encode(lin) - srgb).abs() < 2e-3, "{lin} vs {srgb}");
        assert!(lin < 0.05, "a dark tone drops under a hard curve: {lin}");
    }

    #[test]
    fn hostile_parameters_do_not_poison_the_output() {
        let p = Params {
            exposure: f32::NAN,
            brightness: f32::INFINITY,
            temp: -1e9,
            saturation: f32::NEG_INFINITY,
            presat: f32::NAN,
            whites: 1e9,
            ..Params::default()
        };
        let out = convert(&p, 0.5);
        assert!(out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{out:?}");
        let c = Converter::new(&Analysis::manual([f32::NAN, 0.0, f32::INFINITY]), &Params::default(), Encoding::Srgb, Some([f32::NAN; 3]));
        let out = c.convert([f32::NAN, -1.0, 2.0]);
        assert!(out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{out:?}");
    }
}
