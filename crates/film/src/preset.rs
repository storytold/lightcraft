//! Presets: the character of a film stock as a conversion recipe.
//!
//! A preset carries what belongs to the *film*: the contrast of each colour layer (density per
//! log-exposure), the typical colour of its mask, a tone profile, a colour model, saturation,
//! a warmth offset and a touch of toning. It does not carry what belongs to the *scan* (the
//! actual film base and the black and white points), which is measured; the preset's mask
//! colour is only the fallback when nothing was measured.
//!
//! The recipes are starting points derived from the stocks' published characteristic curves
//! and the way they are known to render, named after the stock they take after. Names are
//! lower-case words and numbers, which a host that labels a choice from its value renders
//! capitalised (`portra 400` → "Portra 400"); [`Preset::parse`] accepts any spelling of them.

use crate::{Look, NEGATIVE_GAMMA, Params, Tone, color, finite};

/// How much more density each channel of a scan of colour negative film spans than the layer's
/// own gamma gives, red, green, blue: a scan's blue runs far denser than its red (the orange
/// mask, the dyes' side absorptions, the scanner's or camera's channel sensitivities). Only the
/// levels derived from a base alone use it ([`Preset::scan_gamma`]); a measurement makes it
/// irrelevant.
///
/// Provisional: fitted on three frames of one Kodak Ektar 100 strip photographed against
/// daylight, matching the colour of each frame's mean to the photographer's own positive (the
/// best fit per preset ran 0.62 to 0.78 for red and 1.30 to 1.52 for blue). Without it, those
/// frames came out strongly blue. More scans are needed to settle it.
pub const SCAN_SPREAD: [f32; 3] = [0.7, 1.0, 1.4];

/// One recipe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    /// The parameter value and its menu entry, as shown ([`Preset::parse`] takes any spelling).
    pub name: &'static str,
    /// Density per log-exposure of the red, green and blue layers (as the film's data gives
    /// it; a scan spreads them further, [`SCAN_SPREAD`]).
    pub gamma: [f32; 3],
    /// Typical mask colour of an unexposed, normally scanned frame, sRGB-encoded 0..1: the
    /// fallback film base.
    pub base: [f32; 3],
    pub tone: Tone,
    pub look: Look,
    pub presat: f32,
    /// Offsets added to the sliders.
    pub contrast: f32,
    pub temp: f32,
    pub tint: f32,
    pub saturation: f32,
    /// Highlight and shadow toning (red, green, blue balance, −50..50) at the default range.
    pub highs: [f32; 3],
    pub lows: [f32; 3],
    pub monochrome: bool,
}

/// Every preset, in menu order. The first is the default.
///
/// The film names are the trademarks of their makers (Eastman Kodak, FUJIFILM, CineStill), used
/// to say which stock a recipe takes after; the recipes are this project's own, built from the
/// stocks' published characteristics and the way they are known to render, and are starting
/// points to tune against your own scans.
pub const PRESETS: [Preset; 12] = [
    // A neutral lab scan: even layer contrast, neutral colour, the lab tone profile.
    Preset {
        name: "Standard",
        gamma: [NEGATIVE_GAMMA; 3],
        base: [0.80, 0.52, 0.36],
        tone: Tone::LabStandard,
        look: Look::Neutral,
        presat: 3.0,
        contrast: 0.0,
        temp: 0.0,
        tint: 0.0,
        saturation: 0.0,
        highs: [0.0; 3],
        lows: [0.0; 3],
        monochrome: false,
    },
    // Kodak Portra 160: the gentlest of the family. Low contrast, pastel saturation, warm
    // natural skin, soft warm highlights.
    Preset {
        name: "Portra 160",
        gamma: [0.56, 0.58, 0.60],
        base: [0.79, 0.53, 0.37],
        tone: Tone::LabSoft,
        look: Look::Neutral,
        presat: 2.4,
        contrast: -4.0,
        temp: 4.0,
        tint: 1.0,
        saturation: -8.0,
        highs: [3.0, 1.0, -4.0],
        lows: [0.0, 0.0, 1.0],
        monochrome: false,
    },
    // Kodak Portra 400: the portrait standard. Moderate contrast, warm natural skin, muted but
    // full colour, warm highlights, a touch of cool in the shadows.
    Preset {
        name: "Portra 400",
        gamma: [0.58, 0.60, 0.63],
        base: [0.79, 0.53, 0.37],
        tone: Tone::LabSoft,
        look: Look::Neutral,
        presat: 2.8,
        contrast: 0.0,
        temp: 9.0,
        tint: -2.0,
        saturation: -2.0,
        highs: [5.0, 1.0, -6.0],
        lows: [-1.0, 1.0, 0.0],
        monochrome: false,
    },
    // Kodak Ektar 100: the saturated one. Steep layers, strong separation, vivid reds and
    // blues, clean neutrals, contrast to match.
    Preset {
        name: "Ektar 100",
        gamma: [0.63, 0.65, 0.69],
        base: [0.78, 0.50, 0.33],
        tone: Tone::LabStandard,
        look: Look::Neutral,
        presat: 4.0,
        contrast: 10.0,
        temp: 3.0,
        tint: 1.0,
        saturation: 14.0,
        highs: [2.0, 0.0, -2.0],
        lows: [0.0, 0.0, -3.0],
        monochrome: false,
    },
    // Kodak Gold 200: golden. Warm yellow highlights, warm neutrals, saturated reds and
    // yellows, the minilab rendering.
    Preset {
        name: "Gold 200",
        gamma: [0.60, 0.62, 0.66],
        base: [0.82, 0.54, 0.35],
        tone: Tone::LabStandard,
        look: Look::LabWarm,
        presat: 3.2,
        contrast: 2.0,
        temp: 12.0,
        tint: 3.0,
        saturation: 6.0,
        highs: [8.0, 3.0, -10.0],
        lows: [3.0, 0.0, -3.0],
        monochrome: false,
    },
    // Kodak ColorPlus 200: the everyday stock. Warm like Gold but softer and less saturated,
    // warm shadows, a little faded: the look of old snapshots.
    Preset {
        name: "ColorPlus 200",
        gamma: [0.58, 0.60, 0.63],
        base: [0.83, 0.55, 0.36],
        tone: Tone::LabSoft,
        look: Look::LabWarm,
        presat: 2.8,
        contrast: -2.0,
        temp: 10.0,
        tint: 2.0,
        saturation: -2.0,
        highs: [6.0, 2.0, -8.0],
        lows: [4.0, 1.0, -2.0],
        monochrome: false,
    },
    // Kodak UltraMax 400: the punchy consumer stock. Saturated, contrasty, warm, strong blues.
    Preset {
        name: "UltraMax 400",
        gamma: [0.61, 0.63, 0.67],
        base: [0.81, 0.53, 0.35],
        tone: Tone::LabStandard,
        look: Look::LabWarm,
        presat: 3.5,
        contrast: 6.0,
        temp: 6.0,
        tint: 2.0,
        saturation: 10.0,
        highs: [4.0, 2.0, -6.0],
        lows: [1.0, 0.0, -1.0],
        monochrome: false,
    },
    // Fujifilm Superia X-TRA 400: Fuji's greens. Cooler than Kodak, a steeper green layer,
    // vivid, teal shadows, a touch of magenta in skin.
    Preset {
        name: "Superia 400",
        gamma: [0.60, 0.64, 0.66],
        base: [0.76, 0.50, 0.34],
        tone: Tone::LabStandard,
        look: Look::LabCool,
        presat: 3.4,
        contrast: 6.0,
        temp: -5.0,
        tint: -5.0,
        saturation: 8.0,
        highs: [0.0, 3.0, -2.0],
        lows: [-3.0, 2.0, 5.0],
        monochrome: false,
    },
    // Fujifilm C200: clean and cool. Soft contrast, restrained saturation, cool neutrals, a
    // green-cyan lean in the shadows.
    Preset {
        name: "C200",
        gamma: [0.58, 0.61, 0.63],
        base: [0.77, 0.51, 0.35],
        tone: Tone::LabSoft,
        look: Look::LabCool,
        presat: 2.8,
        contrast: 0.0,
        temp: -4.0,
        tint: -3.0,
        saturation: -4.0,
        highs: [0.0, 2.0, 0.0],
        lows: [-2.0, 2.0, 3.0],
        monochrome: false,
    },
    // Fujifilm Pro 400H: the pastel wedding stock. Flat layers, airy highlights, mint greens,
    // cyan shadows, soft skin; made to be overexposed.
    Preset {
        name: "Pro 400H",
        gamma: [0.55, 0.58, 0.60],
        base: [0.78, 0.52, 0.36],
        tone: Tone::LabSoft,
        look: Look::Clean,
        presat: 2.3,
        contrast: -8.0,
        temp: -3.0,
        tint: -6.0,
        saturation: -6.0,
        highs: [-2.0, 2.0, 2.0],
        lows: [-4.0, 3.0, 5.0],
        monochrome: false,
    },
    // CineStill 800T: tungsten cine stock shot in daylight. Low, cinematic contrast, muted
    // colour, teal-blue shadows against warm orange highlights. (The halation it is known for
    // is not a colour and is not modelled.)
    Preset {
        name: "CineStill 800",
        gamma: [0.55, 0.57, 0.60],
        base: [0.80, 0.52, 0.36],
        tone: Tone::Cinematic,
        look: Look::Clean,
        presat: 2.6,
        contrast: 0.0,
        temp: -3.0,
        tint: 2.0,
        saturation: -8.0,
        highs: [10.0, 2.0, -8.0],
        lows: [-8.0, 0.0, 12.0],
        monochrome: false,
    },
    // Black-and-white: a paper-like straight line with some contrast, no colour.
    Preset {
        name: "Monochrome",
        gamma: [0.65; 3],
        base: [0.74; 3],
        tone: Tone::Linear,
        look: Look::Neutral,
        presat: 3.0,
        contrast: 15.0,
        temp: 0.0,
        tint: 0.0,
        saturation: 0.0,
        highs: [0.0; 3],
        lows: [0.0; 3],
        monochrome: true,
    },
];

/// The sliders a user sets on top of a preset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sliders {
    pub exposure: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub temp: f32,
    pub tint: f32,
    pub saturation: f32,
}

/// A preset name reduced to what [`Preset::parse`] compares: its letters and digits, lower-case.
fn name_key(s: &str) -> impl Iterator<Item = char> + '_ {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase())
}

impl Preset {
    /// The preset with this name, spelled any way: case, spaces, hyphens and underscores do not
    /// count (`portra 400`, `portra400` and `PORTRA_400` are the same preset).
    pub fn parse(name: &str) -> Option<&'static Preset> {
        PRESETS.iter().find(|p| name_key(p.name).eq(name_key(name)))
    }

    pub fn default_preset() -> &'static Preset {
        &PRESETS[0]
    }

    /// The fallback film base in linear light.
    pub fn base_linear(&self) -> [f32; 3] {
        [color::srgb_decode(self.base[0]), color::srgb_decode(self.base[1]), color::srgb_decode(self.base[2])]
    }

    /// Density per log-exposure of each channel as a scan sees it: [`Preset::gamma`] spread by
    /// [`SCAN_SPREAD`] for colour film, as it is for black-and-white (no mask, one dye).
    pub fn scan_gamma(&self) -> [f32; 3] {
        if self.monochrome {
            return self.gamma;
        }
        [self.gamma[0] * SCAN_SPREAD[0], self.gamma[1] * SCAN_SPREAD[1], self.gamma[2] * SCAN_SPREAD[2]]
    }

    /// The conversion parameters for this preset with `s` on top.
    pub fn params(&self, s: &Sliders) -> Params {
        let d = Params::default();
        Params {
            exposure: finite(s.exposure, 0.0),
            brightness: finite(s.brightness, 0.0),
            contrast: self.contrast + finite(s.contrast, 0.0),
            tone: self.tone,
            temp: self.temp + finite(s.temp, 0.0),
            tint: self.tint + finite(s.tint, 0.0),
            presat: self.presat,
            saturation: self.saturation + finite(s.saturation, 0.0),
            look: self.look,
            highs_red: self.highs[0],
            highs_green: self.highs[1],
            highs_blue: self.highs[2],
            lows_red: self.lows[0],
            lows_green: self.lows[1],
            lows_blue: self.lows[2],
            monochrome: self.monochrome,
            film_gamma: self.scan_gamma(),
            ..d
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Analysis, Converter, Encoding, RANGE_STOPS, film};

    #[test]
    fn presets_parse_and_the_first_is_standard() {
        assert_eq!(Preset::default_preset().name, "Standard");
        for p in &PRESETS {
            assert_eq!(Preset::parse(p.name).map(|q| q.name), Some(p.name));
            assert!(p.gamma.iter().all(|g| (0.4..=0.8).contains(g)), "{}", p.name);
            assert!(p.base.iter().all(|b| (0.3..=0.95).contains(b)), "{}", p.name);
        }
        assert!(Preset::parse("portra").is_none());
    }

    #[test]
    fn names_parse_in_any_spelling() {
        for (spelling, name) in [
            ("portra 400", "Portra 400"),
            ("portra400", "Portra 400"),
            ("PORTRA_400", "Portra 400"),
            ("pro 400h", "Pro 400H"),
            ("CineStill-800", "CineStill 800"),
        ] {
            assert_eq!(Preset::parse(spelling).map(|p| p.name), Some(name), "{spelling}");
        }
        assert!(Preset::parse("kodachrome").is_none() && Preset::parse("").is_none());
        // A host shows a choice with a space exactly as written (PhotoCraft does), so the names
        // are the menu's text: capitalised words and numbers, distinct however they are spelled.
        for p in &PRESETS {
            assert!(p.name.starts_with(|c: char| c.is_ascii_uppercase()), "{}", p.name);
            assert!(p.name.chars().all(|c| c.is_ascii_alphanumeric() || c == ' '), "{}", p.name);
            assert_eq!(PRESETS.iter().filter(|q| name_key(q.name).eq(name_key(p.name))).count(), 1, "{}", p.name);
        }
    }

    #[test]
    fn every_preset_converts_a_ramp_monotonically_and_finitely() {
        let white = libm::exp2f(RANGE_STOPS);
        for preset in &PRESETS {
            let base = preset.base_linear();
            let p = preset.params(&Sliders::default());
            let c = Converter::new(&Analysis::from_base(base, p.film_gamma), &p, Encoding::Srgb, None);
            let mut last = [-1.0f32; 3];
            for i in 0..=40 {
                let s = i as f32 / 40.0;
                let e = 1.0 + s * (white - 1.0);
                let scan = [
                    color::srgb_encode(film::transmittance(e, base[0])),
                    color::srgb_encode(film::transmittance(e, base[1])),
                    color::srgb_encode(film::transmittance(e, base[2])),
                ];
                let out = c.convert(scan);
                assert!(out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{}: {out:?}", preset.name);
                let y = color::luminance(out);
                assert!(y >= color::luminance(last) - 1e-3, "{} not monotonic at {s}: {out:?} after {last:?}", preset.name);
                if preset.monochrome {
                    assert!((out[0] - out[1]).abs() < 1e-5 && (out[1] - out[2]).abs() < 1e-5, "{}: {out:?}", preset.name);
                }
                last = out;
            }
            assert!(color::luminance(last) > 0.9, "{} reaches white: {last:?}", preset.name);
        }
    }

    #[test]
    fn sliders_add_to_the_preset() {
        let golden = Preset::parse("gold 200").unwrap();
        let p = golden.params(&Sliders { temp: 10.0, contrast: -8.0, exposure: f32::NAN, ..Sliders::default() });
        assert_eq!(p.temp, golden.temp + 10.0);
        assert_eq!(p.contrast, golden.contrast - 8.0);
        assert_eq!(p.exposure, 0.0);
        assert_eq!(p.look, Look::LabWarm);
        assert_eq!(p.film_gamma, golden.scan_gamma());
    }

    #[test]
    fn a_scan_spreads_colour_layers_but_not_black_and_white() {
        let portra = Preset::parse("portra 400").unwrap();
        let g = portra.scan_gamma();
        assert!(g[0] < portra.gamma[0] && g[1] == portra.gamma[1] && g[2] > portra.gamma[2], "{g:?}");
        let mono = Preset::parse("monochrome").unwrap();
        assert_eq!(mono.scan_gamma(), mono.gamma);
    }
}
