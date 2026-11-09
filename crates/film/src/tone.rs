//! Tone shaping in display (sRGB-encoded) space: the tone profiles, the tone sliders, and the
//! lab-scanner look of a lifted toe (fade) and a soft shoulder (glow).

use crate::{Params, finite};

/// The tone profiles: three families, as lab-style negative conversion software offers them.
///
/// - **Linear**: a flat, neutral start, a straight line between the black and white points;
///   `Deep` darkens the mid-tones, `Flat` lifts black and lowers white a little.
/// - **Cinematic**: smooth, low-contrast mid-tones and shadows with strong highlight
///   separation, the look of scanned cine film; `Flat` lifts the toe further, `Rich` adds
///   contrast.
/// - **Lab**: the contrast of a lab scanner, tones close to clipping. `Standard` is the
///   default; the others change the steepness on one side or both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    Linear,
    LinearDeep,
    LinearFlat,
    Cinematic,
    CinematicFlat,
    CinematicRich,
    LabSoft,
    #[default]
    LabStandard,
    LabHard,
    LabHighlightSoft,
    LabHighlightHard,
    LabShadowSoft,
    LabShadowHard,
}

/// A profile's shape: logistic steepness on the shadow and highlight sides (0 = straight),
/// a black lift, a white ceiling and a mid-tone gamma.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Profile {
    k_lo: f32,
    k_hi: f32,
    lift: f32,
    ceiling: f32,
    gamma: f32,
}

impl Tone {
    /// Every profile with its parameter name, in menu order.
    pub const ALL: [(Tone, &'static str); 13] = [
        (Tone::Linear, "linear"),
        (Tone::LinearDeep, "linear_deep"),
        (Tone::LinearFlat, "linear_flat"),
        (Tone::Cinematic, "cinematic"),
        (Tone::CinematicFlat, "cinematic_flat"),
        (Tone::CinematicRich, "cinematic_rich"),
        (Tone::LabSoft, "lab_soft"),
        (Tone::LabStandard, "lab_standard"),
        (Tone::LabHard, "lab_hard"),
        (Tone::LabHighlightSoft, "lab_highlight_soft"),
        (Tone::LabHighlightHard, "lab_highlight_hard"),
        (Tone::LabShadowSoft, "lab_shadow_soft"),
        (Tone::LabShadowHard, "lab_shadow_hard"),
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, name)| *name == s).map(|(t, _)| *t)
    }

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(t, _)| *t == self).map_or("linear", |(_, name)| name)
    }

    /// The profile's mid-tone gamma (> 1 darkens), applied with brightness by the converter.
    pub fn gamma(self) -> f32 {
        self.profile().gamma
    }

    fn profile(self) -> Profile {
        let p = |k_lo, k_hi, lift, ceiling, gamma| Profile { k_lo, k_hi, lift, ceiling, gamma };
        match self {
            Self::Linear => p(0.0, 0.0, 0.0, 1.0, 1.0),
            Self::LinearDeep => p(0.0, 0.0, 0.0, 1.0, 1.15),
            Self::LinearFlat => p(0.0, 0.0, 0.04, 0.97, 1.0),
            Self::Cinematic => p(1.5, 3.5, 0.03, 1.0, 0.95),
            Self::CinematicFlat => p(1.0, 2.5, 0.06, 0.98, 0.95),
            Self::CinematicRich => p(2.5, 4.5, 0.0, 1.0, 1.05),
            Self::LabSoft => p(2.5, 2.5, 0.0, 1.0, 1.0),
            Self::LabStandard => p(4.0, 4.0, 0.0, 1.0, 1.0),
            Self::LabHard => p(6.0, 6.0, 0.0, 1.0, 1.0),
            Self::LabHighlightSoft => p(4.0, 2.0, 0.0, 1.0, 1.0),
            Self::LabHighlightHard => p(4.0, 6.5, 0.0, 1.0, 1.0),
            Self::LabShadowSoft => p(2.0, 4.0, 0.0, 1.0, 1.0),
            Self::LabShadowHard => p(6.5, 4.0, 0.0, 1.0, 1.0),
        }
    }
}

/// Steepness the contrast slider adds at ±50.
const CONTRAST_STEEPNESS: f32 = 4.0;
/// Steepest logistic allowed.
const MAX_STEEPNESS: f32 = 10.0;

/// The folded tone curve of one conversion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    /// Logistic steepness on the shadow and highlight sides; both ≤ 0 means "towards flat"
    /// by `−k / MAX_STEEPNESS`.
    k_lo: f32,
    k_hi: f32,
    lights: f32,
    darks: f32,
    /// Black lift (positive fade and the profile's lift) and shadow expansion (negative fade).
    lift: f32,
    expand_lows: f32,
    ceiling: f32,
    /// Highlight compression (positive glow) and expansion (negative glow).
    glow: f32,
    expand_highs: f32,
    soft_highs: f32,
    soft_lows: f32,
}

impl Curve {
    pub fn new(p: &Params) -> Self {
        let prof = p.tone.profile();
        let contrast = finite(p.contrast, 0.0).clamp(-50.0, 50.0) / 50.0 * CONTRAST_STEEPNESS;
        let fade = finite(p.fade, 0.0).clamp(-100.0, 100.0) / 100.0;
        let glow = finite(p.glow, 0.0).clamp(-100.0, 100.0) / 100.0;
        Self {
            k_lo: (prof.k_lo + contrast).clamp(-MAX_STEEPNESS, MAX_STEEPNESS),
            k_hi: (prof.k_hi + contrast).clamp(-MAX_STEEPNESS, MAX_STEEPNESS),
            lights: finite(p.lights, 0.0).clamp(-50.0, 50.0) / 100.0,
            darks: finite(p.darks, 0.0).clamp(-50.0, 50.0) / 100.0,
            lift: prof.lift + fade.max(0.0) * 0.2,
            expand_lows: (-fade).max(0.0) * 2.0,
            ceiling: prof.ceiling,
            glow: glow.max(0.0) * 2.0,
            expand_highs: (-glow).max(0.0) * 2.0,
            soft_highs: finite(p.soft_highs, 0.0).clamp(0.0, 100.0) / 100.0 * 2.0,
            soft_lows: finite(p.soft_lows, 0.0).clamp(0.0, 100.0) / 100.0 * 2.0,
        }
    }

    /// True when the curve changes nothing, so a host can skip the encode / decode around it.
    pub fn is_identity(&self) -> bool {
        self.k_lo.abs() <= 1e-6
            && self.k_hi.abs() <= 1e-6
            && self.lights == 0.0
            && self.darks == 0.0
            && self.lift <= 0.0
            && self.expand_lows <= 0.0
            && self.ceiling >= 1.0
            && self.glow <= 0.0
            && self.expand_highs <= 0.0
            && self.soft_highs <= 0.0
            && self.soft_lows <= 0.0
    }

    /// `x` (sRGB-encoded, 0..1) through the curve, 0..1.
    pub fn apply(&self, x: f32) -> f32 {
        let mut y = finite(x, 0.0).clamp(0.0, 1.0);
        // The contrast S: one symmetric logistic per side (each pinned at 0, ½ and 1), blended
        // across the middle so the join is smooth and mid-grey stays where it is.
        if (self.k_lo - self.k_hi).abs() <= 1e-6 {
            y = s_curve(y, self.k_lo);
        } else {
            let w = smoothstep((y - 0.3) / 0.4);
            y = s_curve(y, self.k_lo) * (1.0 - w) + s_curve(y, self.k_hi) * w;
        }
        // Bumps centred on the shadows (⅓) and highlights (⅔), zero at both ends.
        y += self.darks * 3.0 * y * (1.0 - y) * (1.0 - y);
        y += self.lights * 3.0 * y * y * (1.0 - y);
        y = y.clamp(0.0, 1.0);
        // Shadow and highlight tonality: expansion steepens an end, compression softens it.
        if self.expand_lows > 0.0 {
            y = remap(y, 0.0, 0.5, |t| compress_top(t, self.expand_lows));
        }
        if self.expand_highs > 0.0 {
            y = remap(y, 0.5, 1.0, |t| compress_bottom(t, self.expand_highs));
        }
        if self.glow > 0.0 {
            y = remap(y, 0.5, 1.0, |t| compress_top(t, self.glow));
        }
        if self.soft_lows > 0.0 {
            y = remap(y, 0.0, 0.3, |t| compress_bottom(t, self.soft_lows));
        }
        if self.soft_highs > 0.0 {
            y = remap(y, 0.7, 1.0, |t| compress_top(t, self.soft_highs));
        }
        // Lift and ceiling last, so black and white land where the profile and fade say.
        if self.lift > 0.0 || self.ceiling < 1.0 {
            y = self.lift + y * (self.ceiling - self.lift);
        }
        y.clamp(0.0, 1.0)
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + libm::expf(-x))
}

/// A symmetric contrast curve through (0, 0), (½, ½) and (1, 1): a logistic of steepness `k`
/// when `k > 0`, a pull towards flat when `k < 0`, identity at 0.
fn s_curve(y: f32, k: f32) -> f32 {
    if k > 1e-6 {
        let half = sigmoid(k * 0.5) - 0.5;
        (0.5 + 0.5 * (sigmoid(k * (y - 0.5)) - 0.5) / half).clamp(0.0, 1.0)
    } else if k < -1e-6 {
        y + (0.5 - y) * (-k / MAX_STEEPNESS)
    } else {
        y
    }
}

fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// `t` in 0..1 with its slope reduced towards 1 (Reinhard with the white point kept at 1).
fn compress_top(t: f32, a: f32) -> f32 {
    t * (1.0 + a) / (1.0 + a * t)
}

/// `t` in 0..1 with its slope reduced towards 0.
fn compress_bottom(t: f32, a: f32) -> f32 {
    1.0 - compress_top(1.0 - t, a)
}

/// `y` with `f` applied to the part of it inside `lo..hi` (unchanged outside).
fn remap(y: f32, lo: f32, hi: f32, f: impl Fn(f32) -> f32) -> f32 {
    if y <= lo || y >= hi {
        return y;
    }
    lo + f((y - lo) / (hi - lo)) * (hi - lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(p: Params) -> Curve {
        Curve::new(&p)
    }

    fn linear() -> Params {
        Params { tone: Tone::Linear, ..Params::default() }
    }

    #[test]
    fn names_round_trip_and_default_is_lab_standard() {
        for (tone, name) in Tone::ALL {
            assert_eq!(Tone::parse(name), Some(tone));
            assert_eq!(tone.name(), name);
        }
        assert_eq!(Tone::parse("lab"), None);
        assert_eq!(Tone::default(), Tone::LabStandard);
        assert_eq!(Params::default().tone, Tone::LabStandard);
    }

    #[test]
    fn linear_profile_is_identity_and_every_profile_is_monotonic_and_pinned() {
        let c = curve(linear());
        assert!(c.is_identity());
        for x in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert!((c.apply(x) - x).abs() < 1e-6);
        }
        for (tone, name) in Tone::ALL {
            let c = curve(Params { tone, ..Params::default() });
            let mut last = -1.0;
            for i in 0..=200 {
                let y = c.apply(i as f32 / 200.0);
                assert!(y >= last - 1e-5, "{name} not monotonic at {i}: {y} < {last}");
                last = y;
            }
            let lo = c.apply(0.0);
            let hi = c.apply(1.0);
            assert!((0.0..=0.07).contains(&lo) && (0.96..=1.0).contains(&hi), "{name}: ends {lo} {hi}");
            if tone != Tone::Linear && tone != Tone::LinearDeep {
                assert!(!c.is_identity(), "{name}");
            }
        }
    }

    #[test]
    fn lab_profiles_order_by_steepness_on_each_side() {
        let at = |tone, x: f32| curve(Params { tone, ..Params::default() }).apply(x);
        assert!(at(Tone::LabHard, 0.25) < at(Tone::LabStandard, 0.25) && at(Tone::LabStandard, 0.25) < at(Tone::LabSoft, 0.25));
        assert!(at(Tone::LabHard, 0.75) > at(Tone::LabStandard, 0.75) && at(Tone::LabStandard, 0.75) > at(Tone::LabSoft, 0.75));
        // Highlight-side variants leave the shadows where Standard has them, and mid-grey stays put.
        assert!((at(Tone::LabHighlightHard, 0.15) - at(Tone::LabStandard, 0.15)).abs() < 1e-5);
        assert!((at(Tone::LabHighlightSoft, 0.5) - 0.5).abs() < 1e-5 && (at(Tone::LabShadowHard, 0.5) - 0.5).abs() < 1e-5);
        assert!(at(Tone::LabHighlightHard, 0.85) > at(Tone::LabStandard, 0.85));
        assert!(at(Tone::LabHighlightSoft, 0.85) < at(Tone::LabStandard, 0.85));
        assert!(at(Tone::LabShadowHard, 0.15) < at(Tone::LabStandard, 0.15));
        assert!(at(Tone::LabShadowSoft, 0.15) > at(Tone::LabStandard, 0.15));
        // Cinematic: flatter shadows than Lab Standard, a lifted toe, separated highlights.
        assert!(at(Tone::Cinematic, 0.0) > 0.0 && at(Tone::Cinematic, 0.2) > at(Tone::LabStandard, 0.2));
        assert!(Tone::LinearDeep.gamma() > 1.0 && Tone::Cinematic.gamma() < 1.0 && Tone::LabStandard.gamma() == 1.0);
        assert!(at(Tone::LinearFlat, 0.0) > 0.0 && at(Tone::LinearFlat, 1.0) < 1.0);
    }

    #[test]
    fn contrast_slider_adds_and_removes_steepness() {
        let more = curve(Params { contrast: 30.0, ..Params::default() }).apply(0.25);
        let standard = curve(Params::default()).apply(0.25);
        let less = curve(Params { contrast: -50.0, ..linear() }).apply(0.25);
        assert!(more < standard, "{more} {standard}");
        assert!(less > 0.25, "negative contrast on a straight line flattens: {less}");
        assert!(curve(Params { contrast: -50.0, ..linear() }).apply(0.0) > 0.0);
    }

    #[test]
    fn lights_darks_fade_glow_and_soft_ends_act_where_they_should() {
        let lin = linear();
        let darks = curve(Params { darks: 50.0, ..lin });
        assert!(darks.apply(0.3) > 0.3 && (darks.apply(0.3) - 0.3) > (darks.apply(0.9) - 0.9), "darks lift the low end most");
        let lights = curve(Params { lights: -50.0, ..lin });
        assert!(lights.apply(0.7) < 0.7 && (0.7 - lights.apply(0.7)) > (0.1 - lights.apply(0.1)), "lights act on the high end");
        let fade = curve(Params { fade: 100.0, ..lin });
        assert!((fade.apply(0.0) - 0.2).abs() < 1e-6 && (fade.apply(1.0) - 1.0).abs() < 1e-6);
        let unfade = curve(Params { fade: -100.0, ..lin });
        assert!(unfade.apply(0.1) > 0.1 && unfade.apply(0.0) == 0.0 && (unfade.apply(0.5) - 0.5).abs() < 1e-6, "negative fade steepens the shadows");
        let glow = curve(Params { glow: 100.0, ..lin });
        assert!(glow.apply(0.75) > 0.75 && (glow.apply(1.0) - 1.0).abs() < 1e-6 && (glow.apply(0.5) - 0.5).abs() < 1e-6);
        assert!(glow.apply(0.99) - glow.apply(0.9) < 0.09, "the shoulder compresses the top");
        let unglow = curve(Params { glow: -100.0, ..lin });
        assert!(unglow.apply(0.75) < 0.75 && (unglow.apply(1.0) - 1.0).abs() < 1e-6, "negative glow expands the highlights");
        let soft = curve(Params { soft_highs: 100.0, soft_lows: 100.0, ..lin });
        assert!((soft.apply(0.5) - 0.5).abs() < 1e-6 && soft.apply(0.85) > 0.85 && soft.apply(0.15) < 0.15);
        assert!(soft.apply(0.02) < 0.02 * 0.6 && (1.0 - soft.apply(0.98)) < 0.02 * 0.6, "flatter at both ends");
        assert!(curve(Params { fade: f32::NAN, glow: f32::INFINITY, darks: -1e9, ..lin }).apply(f32::NAN).is_finite());
    }
}
