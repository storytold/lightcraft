//! Colour: white-balance presets, the colour models (looks), mid-tone colour balance and
//! highlight / shadow toning. Everything here is a per-channel gain or a small matrix, so the
//! conversion stays within the host's instruction budget.

use crate::{Params, color, finite};

/// White-balance presets: fixed temperature / tint offsets for film stocks whose balance a
/// neutral conversion tends to miss. `AsIs` leaves the sliders alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WbPreset {
    #[default]
    AsIs,
    /// A touch of warmth, what most colour negative film wants after an inversion.
    Warm,
    Cool,
    /// Tungsten-balanced cine stock shot in daylight: a stronger warm correction.
    TungstenFilm,
}

impl WbPreset {
    pub const ALL: [(WbPreset, &'static str); 4] =
        [(WbPreset::AsIs, "as_is"), (WbPreset::Warm, "warm"), (WbPreset::Cool, "cool"), (WbPreset::TungstenFilm, "tungsten_film")];

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, name)| *name == s).map(|(t, _)| *t)
    }

    /// (temperature, tint) offsets in slider units.
    pub fn offsets(self) -> (f32, f32) {
        match self {
            Self::AsIs => (0.0, 0.0),
            Self::Warm => (8.0, 2.0),
            Self::Cool => (-8.0, -2.0),
            Self::TungstenFilm => (18.0, 4.0),
        }
    }
}

/// Colour models: how the positive's colours are rendered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    /// Colour accuracy over character.
    #[default]
    Neutral,
    /// The rendering of a minilab scanner: teal blues, golden yellows, warm neutrals.
    LabWarm,
    /// The same family, less warm.
    LabCool,
    /// Print-like depth: denser shadows with a touch of green, lifted saturation.
    Print,
    /// Luminosity first: clean contrast, restrained colour.
    Clean,
}

impl Look {
    pub const ALL: [(Look, &'static str); 5] =
        [(Look::Neutral, "neutral"), (Look::LabWarm, "lab_warm"), (Look::LabCool, "lab_cool"), (Look::Print, "print"), (Look::Clean, "clean")];

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, name)| *name == s).map(|(t, _)| *t)
    }

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(t, _)| *t == self).map_or("neutral", |(_, name)| name)
    }

    /// A row-major matrix on linear RGB (rows sum to 1, so neutrals stay neutral), plus
    /// temperature / tint offsets, a saturation factor and a shadow tint (linear RGB gains
    /// weighted towards black).
    pub fn recipe(self) -> Recipe {
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        match self {
            Self::Neutral => Recipe { matrix: identity, temp: 0.0, tint: 0.0, saturation: 1.0, shadow_tint: [1.0; 3] },
            Self::LabWarm => Recipe {
                matrix: [[1.00, 0.06, -0.06], [0.02, 1.00, -0.02], [-0.08, 0.12, 0.96]],
                temp: 5.0,
                tint: 1.0,
                saturation: 1.12,
                shadow_tint: [1.0; 3],
            },
            Self::LabCool => Recipe {
                matrix: [[1.00, 0.03, -0.03], [0.0, 1.00, 0.0], [-0.06, 0.10, 0.96]],
                temp: 0.0,
                tint: 0.0,
                saturation: 1.08,
                shadow_tint: [1.0; 3],
            },
            Self::Print => Recipe { matrix: identity, temp: 2.0, tint: -1.0, saturation: 1.1, shadow_tint: [0.97, 1.02, 0.98] },
            Self::Clean => Recipe { matrix: identity, temp: 0.0, tint: 0.0, saturation: 0.92, shadow_tint: [1.0; 3] },
        }
    }
}

/// What a [`Look`] does, see [`Look::recipe`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Recipe {
    pub matrix: [[f32; 3]; 3],
    pub temp: f32,
    pub tint: f32,
    pub saturation: f32,
    pub shadow_tint: [f32; 3],
}

/// Per-channel gains for a temperature / tint pair (slider units, −50..50 each; larger values
/// from presets are allowed), applied in linear light.
pub fn white_balance(temp: f32, tint: f32) -> [f32; 3] {
    let t = finite(temp, 0.0).clamp(-100.0, 100.0) / 50.0;
    let u = finite(tint, 0.0).clamp(-100.0, 100.0) / 50.0;
    [(1.0 + 0.4 * t).max(0.05), (1.0 - 0.3 * u).max(0.05), (1.0 - 0.4 * t).max(0.05)]
}

/// Gains that make the linear positive `sample` neutral (the colour picker on a grey card or a
/// white wall): each channel to the sample's luminance. `None` for a sample without colour.
pub fn neutralise(sample: [f32; 3]) -> Option<[f32; 3]> {
    let y = color::luminance(sample);
    if !y.is_finite() || y <= 1e-6 || sample.iter().any(|v| !v.is_finite() || *v <= 1e-6) {
        return None;
    }
    Some([(y / sample[0]).clamp(0.2, 5.0), (y / sample[1]).clamp(0.2, 5.0), (y / sample[2]).clamp(0.2, 5.0)])
}

/// Weight of the highlights for toning: 0 below `1 − reach`, 1 at white (`reach` 0..1).
pub fn highs_weight(y: f32, reach: f32) -> f32 {
    let reach = reach.clamp(0.05, 1.0);
    smoothstep((y - (1.0 - reach)) / reach)
}

/// Weight of the shadows for toning: 1 at black, 0 above `reach` (`reach` 0..1).
pub fn lows_weight(y: f32, reach: f32) -> f32 {
    let reach = reach.clamp(0.05, 1.0);
    smoothstep((reach - y) / reach)
}

/// Weight of the mid-tones: 1 at middle grey, 0 at black and white.
pub fn mids_weight(y: f32) -> f32 {
    let y = y.clamp(0.0, 1.0);
    4.0 * y * (1.0 - y)
}

fn smoothstep(x: f32) -> f32 {
    let x = finite(x, 0.0).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// The colour part of a conversion, folded from [`Params`]: everything in linear light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colour {
    /// White balance (sliders, preset, look and neutral sample) as gains.
    pub gains: [f32; 3],
    pub matrix: [[f32; 3]; 3],
    /// Saturation before the tone curve (pre-saturation and the look), as a chroma factor.
    pub presat: f32,
    /// Mid-tone balance gains at full weight (red/cyan, green/magenta, blue/yellow).
    pub mids: [f32; 3],
    pub shadow_tint: [f32; 3],
    /// The matrix is not the identity.
    mixes: bool,
    /// The mid-tone balance or the shadow tint is not neutral, so the luminance weights are needed.
    weighted: bool,
    pub is_identity: bool,
}

impl Colour {
    /// `neutral` is a linear positive sample that should come out grey, if the user picked one.
    pub fn new(p: &Params, neutral: Option<[f32; 3]>) -> Self {
        let recipe = p.look.recipe();
        let (pt, pu) = p.wb.offsets();
        let mut gains = white_balance(finite(p.temp, 0.0) + pt + recipe.temp, finite(p.tint, 0.0) + pu + recipe.tint);
        if let Some(n) = neutral.and_then(neutralise) {
            for (g, k) in gains.iter_mut().zip(n) {
                *g *= k;
            }
        }
        let presat = (finite(p.presat, 3.0).clamp(1.0, 5.0) / 3.0 * recipe.saturation).max(0.0);
        let mut mids = [1.0; 3];
        for (m, v) in mids.iter_mut().zip([p.mids_red, p.mids_green, p.mids_blue]) {
            *m = 1.0 + finite(v, 0.0).clamp(-50.0, 50.0) / 50.0 * 0.25;
        }
        let mixes = recipe.matrix != Look::Neutral.recipe().matrix;
        let weighted = mids != [1.0; 3] || recipe.shadow_tint != [1.0; 3];
        let is_identity = gains == [1.0; 3] && !mixes && (presat - 1.0).abs() < 1e-6 && !weighted;
        Self { gains, matrix: recipe.matrix, presat, mids, shadow_tint: recipe.shadow_tint, mixes, weighted, is_identity }
    }

    /// One linear positive sample (levelled, 0..1-ish) through the colour pipeline.
    pub fn apply(&self, mut p: [f32; 3]) -> [f32; 3] {
        if self.is_identity {
            return p;
        }
        for (v, g) in p.iter_mut().zip(self.gains) {
            *v *= g;
        }
        if self.mixes {
            let m = &self.matrix;
            p = [
                m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
                m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
                m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
            ];
        }
        let y = color::luminance(p).max(0.0);
        if !self.weighted {
            for v in &mut p {
                *v = (y + (*v - y) * self.presat).max(0.0);
            }
            return p;
        }
        // Perceptual weights from the luminance; a square root is one instruction.
        let yl = libm::sqrtf(y);
        let w_mid = mids_weight(yl);
        let w_low = lows_weight(yl, 0.35);
        for (ch, v) in p.iter_mut().enumerate() {
            let mut x = y + (*v - y) * self.presat;
            x *= 1.0 + (self.mids[ch] - 1.0) * w_mid;
            x *= 1.0 + (self.shadow_tint[ch] - 1.0) * w_low;
            *v = x.max(0.0);
        }
        p
    }
}

/// Highlight and shadow toning, applied to the encoded positive after the tone curve: a
/// red / cyan, green / magenta and blue / yellow balance for each end, with a `range` that
/// says how far from the end the toning reaches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Toning {
    highs: [f32; 3],
    highs_reach: f32,
    lows: [f32; 3],
    lows_reach: f32,
    /// Saturation after the curve, as a chroma factor.
    saturation: f32,
    pub is_identity: bool,
}

impl Toning {
    pub fn new(p: &Params) -> Self {
        let slider = |v: f32| finite(v, 0.0).clamp(-50.0, 50.0) / 50.0 * 0.15;
        let reach = |v: f32| 0.2 + finite(v, 30.0).clamp(0.0, 100.0) / 100.0 * 0.6;
        let highs = [slider(p.highs_red), slider(p.highs_green), slider(p.highs_blue)];
        let lows = [slider(p.lows_red), slider(p.lows_green), slider(p.lows_blue)];
        let saturation = (1.0 + finite(p.saturation, 0.0).clamp(-50.0, 50.0) / 50.0).max(0.0);
        let is_identity = highs == [0.0; 3] && lows == [0.0; 3] && (saturation - 1.0).abs() < 1e-6;
        Self { highs, highs_reach: reach(p.highs_range), lows, lows_reach: reach(p.lows_range), saturation, is_identity }
    }

    /// One encoded RGB sample (0..1) toned and saturated, 0..1.
    pub fn apply(&self, mut c: [f32; 3]) -> [f32; 3] {
        if self.is_identity {
            return c;
        }
        let y = (c[0] + c[1] + c[2]) / 3.0;
        let w_hi = highs_weight(y, self.highs_reach);
        let w_lo = lows_weight(y, self.lows_reach);
        for (ch, x) in c.iter_mut().enumerate() {
            let v = y + (*x - y) * self.saturation;
            *x = (v + self.highs[ch] * w_hi + self.lows[ch] * w_lo).clamp(0.0, 1.0);
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_looks_parse_and_keep_neutrals_neutral() {
        for (preset, name) in WbPreset::ALL {
            assert_eq!(WbPreset::parse(name), Some(preset));
        }
        for (look, name) in Look::ALL {
            assert_eq!(Look::parse(name), Some(look));
            let m = look.recipe().matrix;
            for row in m {
                assert!((row.iter().sum::<f32>() - 1.0).abs() < 1e-6, "{name}: {row:?}");
            }
        }
        assert_eq!(Look::parse("frontier"), None);
        let warm = Colour::new(&Params { look: Look::LabWarm, ..Params::default() }, None);
        let grey = warm.apply([0.3, 0.3, 0.3]);
        assert!(grey[0] > grey[2], "the warm lab look warms neutrals: {grey:?}");
        let neutral = Colour::new(&Params::default(), None);
        assert!(neutral.is_identity);
        assert_eq!(neutral.apply([0.2, 0.4, 0.6]), [0.2, 0.4, 0.6]);
    }

    #[test]
    fn toning_colours_the_ends_and_post_saturation_scales_chroma() {
        let warm_highs = Toning::new(&Params { highs_blue: -50.0, highs_range: 10.0, ..Params::default() });
        assert!(!warm_highs.is_identity);
        let hi = warm_highs.apply([0.95, 0.95, 0.95]);
        assert!(hi[2] < hi[0] && hi[0] == 0.95, "yellow highlights: {hi:?}");
        assert_eq!(warm_highs.apply([0.3, 0.3, 0.3]), [0.3, 0.3, 0.3], "mid-tones untouched at a short range");
        let cool_lows = Toning::new(&Params { lows_blue: 50.0, lows_range: 100.0, ..Params::default() });
        let lo = cool_lows.apply([0.1, 0.1, 0.1]);
        assert!(lo[2] > lo[0], "{lo:?}");
        assert_eq!(cool_lows.apply([1.0; 3]), [1.0; 3]);
        let grey = Toning::new(&Params { saturation: -50.0, ..Params::default() }).apply([0.2, 0.5, 0.8]);
        assert!((grey[0] - grey[2]).abs() < 1e-6, "{grey:?}");
        assert!(Toning::new(&Params::default()).is_identity);
    }

    #[test]
    fn neutral_sample_is_made_grey() {
        let sample = [0.5, 0.4, 0.3];
        let c = Colour::new(&Params::default(), Some(sample));
        let out = c.apply(sample);
        assert!((out[0] - out[1]).abs() < 1e-5 && (out[1] - out[2]).abs() < 1e-5, "{out:?}");
        assert_eq!(neutralise([0.0, 0.5, 0.5]), None);
        assert_eq!(neutralise([f32::NAN; 3]), None);
    }

    #[test]
    fn presat_mids_and_toning_helpers() {
        let vivid = Colour::new(&Params { presat: 5.0, ..Params::default() }, None).apply([0.6, 0.3, 0.3]);
        let flat = Colour::new(&Params { presat: 1.0, ..Params::default() }, None).apply([0.6, 0.3, 0.3]);
        assert!(vivid[0] - vivid[1] > 0.3 && flat[0] - flat[1] < 0.3, "{vivid:?} {flat:?}");
        let mids = Colour::new(&Params { mids_red: 50.0, ..Params::default() }, None);
        let mid = mids.apply([0.2, 0.2, 0.2]);
        assert!(mid[0] > mid[1], "{mid:?}");
        let black = mids.apply([0.0; 3]);
        assert_eq!(black, [0.0; 3]);
        assert_eq!(highs_weight(1.0, 0.5), 1.0);
        assert_eq!(highs_weight(0.4, 0.5), 0.0);
        assert_eq!(lows_weight(0.0, 0.3), 1.0);
        assert_eq!(lows_weight(0.5, 0.3), 0.0);
        assert_eq!(mids_weight(0.5), 1.0);
        assert_eq!(white_balance(0.0, 0.0), [1.0; 3]);
    }
}
