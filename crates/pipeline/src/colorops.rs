//! Colour tools in OkLCh on display-linear Rec.2020 values: vibrance, saturation, the 8-band colour
//! mixer, B&W mix, and 3-way colour grading; plus the camera-calibration matrix (scene linear).

use std::f32::consts::{PI, TAU};
use std::sync::OnceLock;

use lcc_color::perceptual::{hsv_to_rgb, lab_to_lch, lch_to_lab, oklab_from_2020, oklab_to_2020};
use lcc_color::{Mat3, REC2020, SRGB};
use lcc_develop::{Calibration, DevelopSettings, MIXER_HUES, PointColor};

/// OkLCh hue angle (radians) of a pure sRGB colour with HSV hue `deg`.
pub fn oklch_hue_of_srgb_hue(deg: f64) -> f32 {
    let c = hsv_to_rgb(deg as f32, 1.0, 1.0);
    let lin = c.map(lcc_color::transfer::srgb_to_linear);
    let m = SRGB.to_space(&REC2020).apply_f32(lin);
    lab_to_lch(oklab_from_2020(m))[2]
}

/// OkLCh hue (radians) of each colour-mixer band centre.
pub fn band_hues() -> &'static [f32; 8] {
    static H: OnceLock<[f32; 8]> = OnceLock::new();
    H.get_or_init(|| MIXER_HUES.map(oklch_hue_of_srgb_hue))
}

#[inline]
fn wrap(a: f32) -> f32 {
    (a + PI).rem_euclid(TAU) - PI
}

/// Partition-of-unity weights of hue `h` over the 8 bands (raised cosine between neighbours).
#[inline]
pub fn band_weights(h: f32) -> [f32; 8] {
    let hues = band_hues();
    let mut w = [0.0f32; 8];
    for i in 0..8 {
        let a = hues[i];
        let b = hues[(i + 1) % 8];
        let span = wrap(b - a).rem_euclid(TAU);
        let d = wrap(h - a).rem_euclid(TAU);
        if d <= span {
            let t = d / span;
            let s = 0.5 - 0.5 * (t * PI).cos();
            w[i] += 1.0 - s;
            w[(i + 1) % 8] += s;
            break;
        }
    }
    w
}

/// A colour-grading wheel as an OkLab offset (a, b) and a lightness shift.
#[derive(Clone, Copy, Debug)]
pub struct WheelK {
    pub a: f32,
    pub b: f32,
    pub lum: f32,
}

/// Number of `f32`s per Point Color sample in [`PointK::words`] (the GPU parameter layout).
pub const POINT_WORDS: usize = 10;

/// A Point Color sample resolved for the per-pixel stage: the sample (OkLab lightness, chroma, hue
/// in radians), the adjustment (hue rotation in radians, chroma scale − 1, lightness shift,
/// variance factor − 1) and the half-widths of the range box (hue radians, chroma, lightness).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointK {
    pub l: f32,
    pub c: f32,
    pub h: f32,
    pub dh: f32,
    pub sat: f32,
    pub dl: f32,
    pub var: f32,
    pub wh: f32,
    pub wc: f32,
    pub wl: f32,
}

impl PointK {
    pub fn new(p: &PointColor) -> PointK {
        let k = (p.range.clamp(0.0, 100.0) / 50.0).max(0.05) as f32;
        let f = |v: f64| (v.clamp(0.0, 100.0) / 100.0) as f32;
        PointK {
            l: p.lum as f32,
            c: p.chroma.max(0.0) as f32,
            h: (p.hue as f32).to_radians(),
            dh: (p.hue_shift.clamp(-100.0, 100.0) / 100.0) as f32 * 0.5,
            sat: (p.sat_shift.clamp(-100.0, 100.0) / 100.0) as f32,
            dl: (p.lum_shift.clamp(-100.0, 100.0) / 100.0) as f32 * 0.2,
            var: (p.variance.clamp(-100.0, 100.0) / 100.0) as f32 * 0.8,
            wh: (0.1 + 0.6 * f(p.hue_range)) * k,
            wc: (0.02 + 0.12 * f(p.sat_range)) * k,
            wl: (0.05 + 0.4 * f(p.lum_range)) * k,
        }
    }

    pub fn words(&self) -> [f32; POINT_WORDS] {
        [self.l, self.c, self.h, self.dh, self.sat, self.dl, self.var, self.wh, self.wc, self.wl]
    }

    /// How much an OkLCh colour belongs to this sample's range (0..1, soft edges).
    #[inline]
    pub fn weight(&self, l: f32, c: f32, h: f32) -> f32 {
        // hue is meaningless for near-neutral colours: a chromatic sample ignores them, a
        // near-neutral sample selects by chroma and lightness only
        let wh = if self.c < 0.02 { 1.0 } else { (1.0 - smooth(0.5 * self.wh, self.wh, wrap(h - self.h).abs())) * smooth(0.005, 0.025, c) };
        if wh <= 0.0 {
            return 0.0;
        }
        let wc = 1.0 - smooth(0.5 * self.wc, self.wc, (c - self.c).abs());
        let wl = 1.0 - smooth(0.5 * self.wl, self.wl, (l - self.l).abs());
        wh * wc * wl
    }

    /// Apply this sample's adjustment to an OkLCh colour.
    #[inline]
    pub fn apply(&self, lch: [f32; 3]) -> [f32; 3] {
        self.apply_scaled(lch, 1.0)
    }

    /// [`Self::apply`] at `strength` 0..1 (a mask's opacity there: its local Point Color).
    #[inline]
    pub fn apply_scaled(&self, lch: [f32; 3], strength: f32) -> [f32; 3] {
        let [mut l, mut c, mut h] = lch;
        let w = self.weight(l, c, h) * strength;
        if w <= 0.0 {
            return lch;
        }
        h += w * (self.var * wrap(h - self.h) + self.dh);
        c += w * self.var * (c - self.c);
        c = (c * (1.0 + w * self.sat)).max(0.0);
        l += w * (self.var * (l - self.l) + self.dl);
        [l, c, h]
    }
}

/// The Point Color samples of `points` that change something, resolved (at most
/// [`lcc_develop::MAX_POINT_COLORS`]).
pub fn point_ks(points: &[PointColor]) -> Vec<PointK> {
    points.iter().take(lcc_develop::MAX_POINT_COLORS).filter(|p| !p.is_neutral()).map(PointK::new).collect()
}

/// The masks' local Point Color samples, one list per mask the per-pixel stage evaluates (visible,
/// with components: the order of [`crate::masks::evaluate`]); empty when no mask has any, and in a
/// black-and-white rendition (Point Color does nothing there, as globally).
pub fn mask_point_ks(s: &DevelopSettings) -> Vec<Vec<PointK>> {
    if crate::is_bw(s) {
        return Vec::new();
    }
    let v: Vec<Vec<PointK>> = s.masks.iter().filter(|m| m.visible && !m.components.is_empty()).map(|m| point_ks(&m.point_colors)).collect();
    if v.iter().all(Vec::is_empty) { Vec::new() } else { v }
}

/// Words of a resolved colour mixer ([`MixerK::words`]): hue, saturation, luminance of the 8 bands.
pub const MIXER_WORDS: usize = 24;

/// Entries in each HSL-curve LUT ([`HslK`]): hue positions around the OkLCh circle, plenty for a
/// smooth spline read on the GPU as on the CPU.
pub const HSL_LUT_N: usize = 128;
/// f32s the GPU kernel reads for [`HslK`]: the hue, saturation and luminance LUTs in turn.
pub const HSL_WORDS: usize = 3 * HSL_LUT_N;

/// One HSL curve's value at hue position `x` (0..1 around the OkLCh hue circle): the monotone
/// spline through the user's points, with the circle's ends pinned at neutral (0 = red, where the
/// mixer's hue axis also starts). The curve a point anywhere shapes, not fixed colour bands.
pub fn hsl_curve_at(points: &[lcc_geom::Point], x: f64) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    let at = |p: &lcc_geom::Point| (p.x, p.y);
    let mut all: Vec<(f64, f64)> = points.iter().map(at).collect();
    all.push((0.0, 0.0));
    all.push((1.0, 0.0));
    lcc_color::spline::MonotoneCurve::new(&all).eval(x.clamp(0.0, 1.0))
}

/// The HSL curves resolved once per render: three LUTs of engine-unit deltas over the hue circle —
/// hue in radians (±0.5 at ±1, the mixer's own hue scale), saturation as a scale (±1), luminance
/// as an OkLab lightness shift (±0.18, the mixer's own). `None` when every curve is empty (and in
/// a black-and-white rendition, as the colour mixer).
#[derive(Clone, Debug)]
pub struct HslK {
    pub hue: [f32; HSL_LUT_N],
    pub sat: [f32; HSL_LUT_N],
    pub lum: [f32; HSL_LUT_N],
}

impl HslK {
    pub fn new(c: &lcc_develop::HslCurves) -> Option<HslK> {
        if c.is_neutral() {
            return None;
        }
        let lut = |points: &[lcc_geom::Point], scale: f64| {
            std::array::from_fn(|i| (hsl_curve_at(points, i as f64 / (HSL_LUT_N - 1) as f64) * scale) as f32)
        };
        Some(HslK { hue: lut(&c.hue, 0.5), sat: lut(&c.sat, 1.0), lum: lut(&c.lum, 0.18) })
    }

    /// The words the GPU kernel reads: hue, saturation and luminance LUTs in turn.
    pub fn words(&self) -> Vec<f32> {
        self.hue.iter().chain(&self.sat).chain(&self.lum).copied().collect()
    }

    /// The three curves' deltas at hue position `x` (0..1), as the per-pixel stage applies them.
    #[inline]
    pub fn at(&self, x: f32) -> [f32; 3] {
        let f = (x.clamp(0.0, 1.0) * (HSL_LUT_N - 1) as f32).min((HSL_LUT_N - 2) as f32);
        let i = f.floor() as usize;
        let t = f - i as f32;
        let lerp = |a: &[f32; HSL_LUT_N]| a[i] + (a[i + 1] - a[i]) * t;
        [lerp(&self.hue), lerp(&self.sat), lerp(&self.lum)]
    }
}

/// A colour mixer resolved for the per-pixel stage: per band, the hue shift (radians), the
/// saturation scale and the luminance shift. The global mixer ([`ColorOps`]) and each mask's local
/// one use the same maths ([`mixer_apply`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MixerK {
    pub hue: [f32; 8],
    pub sat: [f32; 8],
    pub lum: [f32; 8],
}

impl MixerK {
    pub fn new(m: &lcc_develop::Mixer) -> MixerK {
        let bands = m.bands();
        let fin = |v: f64| if v.is_finite() { v } else { 0.0 };
        MixerK {
            hue: bands.map(|b| (fin(b.hue) / 100.0) as f32 * 0.5),
            sat: bands.map(|b| (fin(b.sat) / 100.0) as f32),
            lum: bands.map(|b| (fin(b.lum) / 100.0) as f32 * 0.18),
        }
    }

    /// The words the GPU kernel reads: hue[8], sat[8], lum[8].
    pub fn words(&self) -> [f32; MIXER_WORDS] {
        let mut w = [0.0; MIXER_WORDS];
        for (slot, v) in w.iter_mut().zip(self.hue.iter().chain(&self.sat).chain(&self.lum)) {
            *slot = *v;
        }
        w
    }

    /// OkLCh `lch` through the mixer at `strength` (0..1: a mask's opacity; 1 = the full mixer).
    #[inline]
    pub fn apply_scaled(&self, lch: [f32; 3], strength: f32) -> [f32; 3] {
        mixer_apply(&self.hue, &self.sat, &self.lum, lch, strength)
    }
}

/// The colour mixer's maths on OkLCh `lch`: each band's shifts weighted by the hue's band weights,
/// hue and luminance shifts faded out towards neutrals; `strength` scales the shifts (1 = as set).
#[inline]
pub fn mixer_apply(hue: &[f32; 8], sat: &[f32; 8], lum: &[f32; 8], lch: [f32; 3], strength: f32) -> [f32; 3] {
    let [mut l, mut c, mut h] = lch;
    let w = band_weights(h);
    let (mut dh, mut ds, mut dl) = (0.0, 0.0, 0.0);
    for i in 0..8 {
        dh += w[i] * hue[i];
        ds += w[i] * sat[i];
        dl += w[i] * lum[i];
    }
    let chroma_w = (c / 0.12).min(1.0);
    h += dh * strength * chroma_w;
    c *= (1.0 + ds * strength).max(0.0);
    l += dl * strength * chroma_w * l.max(0.05).sqrt();
    [l, c, h]
}

/// The masks' local Color Mixers, one per mask the per-pixel stage evaluates (`None`: neutral);
/// empty when no mask has one, and in a black-and-white rendition (as the global mixer).
pub fn mask_mixer_ks(s: &DevelopSettings) -> Vec<Option<MixerK>> {
    if crate::is_bw(s) {
        return Vec::new();
    }
    let v: Vec<Option<MixerK>> =
        s.masks.iter().filter(|m| m.visible && !m.components.is_empty()).map(|m| (!m.mixer.is_neutral()).then(|| MixerK::new(&m.mixer))).collect();
    if v.iter().all(Option::is_none) { Vec::new() } else { v }
}

/// The colour tools' parameters, resolved once per render (fields are read by the GPU kernel).
#[derive(Clone, Debug)]
pub struct ColorOps {
    pub vibrance: f32,
    pub saturation: f32,
    pub hue: [f32; 8],
    pub sat: [f32; 8],
    pub lum: [f32; 8],
    pub mixer: bool,
    /// Point Color samples that change something (applied after the mixer).
    pub points: Vec<PointK>,
    pub bw: Option<[f32; 8]>,
    /// Wheels (shadows, midtones, highlights, global), blending, balance.
    pub grading: Option<([WheelK; 4], f32, f32)>,
    /// The colourist primaries ([`PrimariesK`]), applied before the colour tools: balance first,
    /// look after.
    pub primaries: Option<PrimariesK>,
    /// The HSL curves ([`HslK`]), applied after the colour mixer on the same OkLCh values.
    pub hsl: Option<HslK>,
    /// OkLCh hue of skin tones (protected by vibrance).
    pub skin: f32,
}

pub fn wheel(w: &lcc_develop::Wheel) -> WheelK {
    let h = oklch_hue_of_srgb_hue(w.hue);
    let s = (w.sat / 100.0) as f32 * 0.09;
    WheelK { a: s * h.cos(), b: s * h.sin(), lum: (w.lum / 100.0) as f32 * 0.12 }
}

impl ColorOps {
    pub fn new(s: &DevelopSettings) -> ColorOps {
        let mk = MixerK::new(&s.mixer);
        let g = &s.grading;
        ColorOps {
            vibrance: (s.color.vibrance / 100.0) as f32,
            saturation: (s.color.saturation / 100.0) as f32,
            hue: mk.hue,
            sat: mk.sat,
            lum: mk.lum,
            mixer: !s.mixer.is_neutral(),
            points: if crate::is_bw(s) { Vec::new() } else { point_ks(&s.point_colors) },
            bw: crate::is_bw(s).then(|| s.bw_mix.bands().map(|v| (v / 100.0) as f32)),
            grading: (!g.is_neutral()).then(|| {
                (
                    [wheel(&g.shadows), wheel(&g.midtones), wheel(&g.highlights), wheel(&g.global)],
                    (g.blending / 100.0) as f32,
                    (g.balance / 100.0) as f32,
                )
            }),
            primaries: PrimariesK::new(&s.primaries),
            hsl: if crate::is_bw(s) { None } else { HslK::new(&s.hsl_curves) },
            skin: oklch_hue_of_srgb_hue(25.0),
        }
    }

    pub fn is_identity(&self) -> bool {
        self.vibrance == 0.0
            && self.saturation == 0.0
            && !self.mixer
            && self.points.is_empty()
            && self.bw.is_none()
            && self.grading.is_none()
            && self.primaries.is_none()
            && self.hsl.is_none()
    }

    /// `local_sat` (−1..1) and `local_hue` (radians) come from masks.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3], local_sat: f32, local_hue: f32) -> [f32; 3] {
        if self.is_identity() && local_sat == 0.0 && local_hue == 0.0 {
            return rgb;
        }
        let lab = oklab_from_2020(rgb);
        // only chroma changes (vibrance / saturation, maybe grading): scale a, b directly — the same
        // result as the OkLCh round trip without its sin / cos (and atan2 unless vibrance needs the hue)
        if !self.mixer && self.points.is_empty() && self.bw.is_none() && self.hsl.is_none() && local_hue == 0.0 {
            let mut lab = lab;
            let c0 = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            let mut c = c0;
            if self.vibrance != 0.0 {
                let low = 1.0 - (c / 0.22).clamp(0.0, 1.0);
                let skin = if self.vibrance > 0.0 { 1.0 - 0.6 * (-(wrap(lab[2].atan2(lab[1]) - self.skin) / 0.35).powi(2)).exp() } else { 1.0 };
                c *= (1.0 + self.vibrance * low * low * skin * 1.2).max(0.0);
            }
            if self.saturation != 0.0 || local_sat != 0.0 {
                c *= (1.0 + self.saturation + local_sat).max(0.0);
            }
            if c0 > 0.0 {
                let k = c / c0;
                lab[1] *= k;
                lab[2] *= k;
            }
            return self.grade(lab);
        }
        let [mut l, mut c, mut h] = lab_to_lch(lab);
        if self.mixer {
            [l, c, h] = mixer_apply(&self.hue, &self.sat, &self.lum, [l, c, h], 1.0);
        }
        // the HSL curves: the hue picks the place on each curve, the mixer's own scales apply the
        // deltas (hue and luminance fading out towards neutrals, as the mixer's do)
        if let Some(hsl) = &self.hsl {
            let x = (h / TAU).rem_euclid(1.0);
            let [dh, ds, dl] = hsl.at(x);
            let chroma_w = (c / 0.12).min(1.0);
            h += dh * chroma_w;
            c *= (1.0 + ds).max(0.0);
            l += dl * chroma_w * l.max(0.05).sqrt();
        }
        for p in &self.points {
            [l, c, h] = p.apply([l, c, h]);
        }
        if self.vibrance != 0.0 {
            let low = 1.0 - (c / 0.22).clamp(0.0, 1.0);
            let skin = if self.vibrance > 0.0 { 1.0 - 0.6 * (-(wrap(h - self.skin) / 0.35).powi(2)).exp() } else { 1.0 };
            c *= (1.0 + self.vibrance * low * low * skin * 1.2).max(0.0);
        }
        if self.saturation != 0.0 || local_sat != 0.0 {
            c *= (1.0 + self.saturation + local_sat).max(0.0);
        }
        h += local_hue;
        if let Some(bw) = &self.bw {
            let w = band_weights(h);
            let mix: f32 = (0..8).map(|i| w[i] * bw[i]).sum();
            l = (l + mix * (c / 0.2).min(1.0) * 0.25).max(0.0);
            c = 0.0;
        }
        self.grade(lch_to_lab([l, c, h]))
    }

    /// Colour grading on OkLab, then back to linear Rec.2020.
    #[inline]
    fn grade(&self, mut lab: [f32; 3]) -> [f32; 3] {
        if let Some((wheels, blending, balance)) = &self.grading {
            let [ws, wm, wh] = grade_zones(lab[0], *blending, *balance);
            for (k, wt) in wheels.iter().zip([ws, wm, wh, 1.0]) {
                lab[1] += k.a * wt;
                lab[2] += k.b * wt;
                lab[0] += k.lum * wt;
            }
        }
        oklab_to_2020(lab)
    }
}

/// How much OkLab lightness `l` belongs to the shadows, midtones and highlights of colour grading
/// (`blending` 0..1, `balance` −1..1). The GPU mirrors it in `finish.wgsl`.
#[inline]
pub fn grade_zones(l: f32, blending: f32, balance: f32) -> [f32; 3] {
    let m = 0.5 - balance * 0.25;
    let width = 0.15 + blending * 0.5;
    let ws = 1.0 - smooth(m - width, m + width * 0.25, l);
    let wh = smooth(m - width * 0.25, m + width, l);
    [ws, (1.0 - ws - wh).max(0.0), wh]
}

/// A colourist wheel resolved for the per-pixel stage: the puck as a per-channel linear-Rec.2020
/// colour direction scaled by its saturation, and the master wheel as a signed amount.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PrimaryK {
    pub rgb: [f32; 3],
    pub master: f32,
}

/// The Color wheels' reaches at a full wheel (±100): ASC CDL's SOP amounts. Lift's affine term
/// (blacks move by up to half the range, whites stay), gain's multiply (up to ×1.75), gamma's
/// power input (exponent 1/(1±p) ≈ 0.57..4 — midtones move hard), offset's add.
pub const PRIMARY_LIFT_REACH: f32 = 0.5;
pub const PRIMARY_GAIN_REACH: f32 = 0.75;
pub const PRIMARY_GAMMA_REACH: f32 = 0.75;
pub const PRIMARY_OFFSET_REACH: f32 = 0.1;

/// The Log wheels' base amounts (additive refinement over narrow zones): puck and master.
pub const PRIMARY_PUCK: f32 = 0.15;
pub const PRIMARY_MASTER: f32 = 0.15;
/// The Log highlight wheel, which multiplies and so starts at twice the additive base.
pub const PRIMARY_GAIN_MASTER: f32 = 0.3;

/// How much stronger each Log wheel reads — tuned by eye on rendered photos (the Color wheels
/// above now carry the CDL reaches): shadows halved (they stack on the Color lift), midtones as
/// tuned, the highlight ×3 — it *multiplies* its zone, so the full master pull lands at ×1.9 and
/// the full push at ×0.1 without ever clamping to black or leaping to white (×10 read as a
/// blow-out: ×4 at full throw, over after a few pixels of drag). (shadow, midtone, highlight,
/// offset.)
pub const LOG_STRENGTH: [f32; 4] = [0.5, 1.0, 3.0, 0.5];

/// The colourist primaries resolved once per render: the Color wheels (lift, gamma, gain, offset —
/// wide tonal overlap) and the Log wheels (shadow, midtone, highlight, offset — narrow ranges
/// between `low` and `high`); both sets apply, complimentary as Resolve's (see
/// [`lcc_develop::Primaries`]). `None` when every wheel is neutral. The GPU kernel reads the same
/// values ([`Self::words`]).
#[derive(Clone, Copy, Debug)]
pub struct PrimariesK {
    pub lift: PrimaryK,
    pub gamma: PrimaryK,
    pub gain: PrimaryK,
    pub offset: PrimaryK,
    pub shadow: PrimaryK,
    pub midtone: PrimaryK,
    pub highlight: PrimaryK,
    pub log_offset: PrimaryK,
    pub low: f32,
    pub high: f32,
}

/// f32s the GPU kernel reads for [`PrimariesK`]: the Color wheels then the Log wheels (rgb + master
/// each, in their orders), then `low` and `high`.
pub const PRIMARY_WORDS: usize = 34;

impl PrimariesK {
    pub fn new(p: &lcc_develop::Primaries) -> Option<PrimariesK> {
        if p.is_neutral() {
            return None;
        }
        let fl = LOG_STRENGTH;
        let log = &p.log_wheels;
        Some(PrimariesK {
            lift: primary_wheel(&p.lift, PRIMARY_LIFT_REACH, PRIMARY_LIFT_REACH),
            gamma: primary_wheel(&p.gamma, PRIMARY_GAMMA_REACH, PRIMARY_GAMMA_REACH),
            gain: primary_wheel(&p.gain, PRIMARY_GAIN_REACH, PRIMARY_GAIN_REACH),
            offset: primary_wheel(&p.offset, PRIMARY_OFFSET_REACH, PRIMARY_OFFSET_REACH),
            shadow: primary_wheel(&log.shadow, PRIMARY_PUCK * fl[0], PRIMARY_MASTER * fl[0]),
            midtone: primary_wheel(&log.midtone, PRIMARY_PUCK * fl[1], PRIMARY_MASTER * fl[1]),
            highlight: primary_wheel(&log.highlight, PRIMARY_PUCK * fl[2], PRIMARY_GAIN_MASTER * fl[2]),
            log_offset: primary_wheel(&log.offset, PRIMARY_PUCK * fl[3], PRIMARY_MASTER * fl[3]),
            low: p.low.clamp(0.0, 1.0) as f32,
            high: p.high.clamp(0.0, 1.0) as f32,
        })
    }

    pub fn words(&self) -> [f32; PRIMARY_WORDS] {
        let mut w = [0.0; PRIMARY_WORDS];
        let parts: [&[f32]; 16] = [
            &self.lift.rgb,
            std::slice::from_ref(&self.lift.master),
            &self.gamma.rgb,
            std::slice::from_ref(&self.gamma.master),
            &self.gain.rgb,
            std::slice::from_ref(&self.gain.master),
            &self.offset.rgb,
            std::slice::from_ref(&self.offset.master),
            &self.shadow.rgb,
            std::slice::from_ref(&self.shadow.master),
            &self.midtone.rgb,
            std::slice::from_ref(&self.midtone.master),
            &self.highlight.rgb,
            std::slice::from_ref(&self.highlight.master),
            &self.log_offset.rgb,
            std::slice::from_ref(&self.log_offset.master),
        ];
        let mut i = 0;
        for part in parts {
            for v in part {
                w[i] = *v;
                i += 1;
            }
        }
        w[32] = self.low;
        w[33] = self.high;
        w
    }

    /// The wheels on display-linear Rec.2020 `rgb` (after the tone map). The Color wheels are ASC
    /// CDL's SOP per channel — lift's affine `x·(1−l)+l` pivots at white, gain's multiply pivots
    /// at black, gamma is a power (exponent 1/(1+p)), offset adds last — so each carries its own
    /// tonal shape with no zone weights; then the Log wheels refine over their narrow ranges
    /// between `low` and `high` (both sets, complimentary). Sources: own implementation of the
    /// published ASC CDL SOP shape and the colorist community's documented lift/gamma/gain forms;
    /// wheel semantics read from Blackmagic's *Colorist Guide to DaVinci Resolve 18*.
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let t = lcc_color::luminance_2020(rgb).clamp(0.0, 1.0);
        let [ls, lm, lh] = log_zones(t, self.low, self.high);
        let mut out = rgb;
        for k in 0..3 {
            let l = self.lift.rgb[k] + self.lift.master;
            let g = self.gain.rgb[k] + self.gain.master;
            let p = self.gamma.rgb[k] + self.gamma.master;
            let mut x = rgb[k] * (1.0 - l) + l;
            x = (x * (1.0 + g)).max(0.0);
            let e = (1.0 / (1.0 + p)).clamp(0.25, 4.0);
            x = x.max(0.0).powf(e);
            x += self.offset.rgb[k] + self.offset.master;
            x += (self.shadow.rgb[k] + self.shadow.master) * ls;
            x += (self.midtone.rgb[k] + self.midtone.master) * lm;
            x *= 1.0 + (self.highlight.rgb[k] + self.highlight.master) * lh;
            x += self.log_offset.rgb[k] + self.log_offset.master;
            out[k] = x.max(0.0);
        }
        out
    }
}

/// A wheel's puck as a linear-Rec.2020 colour direction: the pure hue scaled by its saturation
/// (`puck_amt`), and the master wheel scaled by `master_amt` ([`PRIMARY_STRENGTH`] tunes both).
fn primary_wheel(w: &lcc_develop::Wheel, puck_amt: f32, master_amt: f32) -> PrimaryK {
    let c = hsv_to_rgb(w.hue.clamp(0.0, 360.0) as f32, 1.0, 1.0);
    let lin = c.map(lcc_color::transfer::srgb_to_linear);
    let v = SRGB.to_space(&REC2020).apply_f32(lin);
    let s = (w.sat.clamp(0.0, 100.0) / 100.0) as f32 * puck_amt;
    PrimaryK { rgb: v.map(|k| k * s), master: (w.lum.clamp(-100.0, 100.0) / 100.0) as f32 * master_amt }
}

/// The Log wheels' zones: narrowly separated ranges with their boundaries at `low` and `high`
/// (0..1). The GPU mirrors it in `finish.wgsl`.
#[inline]
pub fn log_zones(t: f32, low: f32, high: f32) -> [f32; 3] {
    let low = low.clamp(0.02, 0.9);
    let hm = high.clamp(low + 0.05, 0.98);
    let ws = 1.0 - smooth(low - 0.10, low + 0.10, t);
    let wh = smooth(hm - 0.10, hm + 0.10, t);
    [ws, (1.0 - ws - wh).max(0.0), wh]
}

/// Number of per-mask colour-grading terms: (a, b, lightness) of the shadows, midtones,
/// highlights and global wheels.
pub const LOCAL_GRADE_TERMS: usize = 12;

/// A mask's colour-grading wheels as alpha-summable terms (see [`LOCAL_GRADE_TERMS`]).
pub fn local_grade_terms(a: &lcc_develop::LocalAdjustments) -> [f32; LOCAL_GRADE_TERMS] {
    let mut t = [0.0; LOCAL_GRADE_TERMS];
    for (i, w) in a.grading_wheels().iter().enumerate() {
        let k = wheel(w);
        t[i * 3] = k.a;
        t[i * 3 + 1] = k.b;
        t[i * 3 + 2] = k.lum;
    }
    t
}

/// Colour grading inside masks: the alpha-weighted sum `g` of the masks' [`local_grade_terms`]
/// applied to linear Rec.2020 `rgb`, zones as the Color Grading panel's at its default blending (50)
/// and balance (0). Sources: own design (the global grading's zones and wheels, weighted per mask).
#[inline]
pub fn local_grade(rgb: [f32; 3], g: &[f32; LOCAL_GRADE_TERMS]) -> [f32; 3] {
    if g.iter().all(|v| *v == 0.0) {
        return rgb;
    }
    let mut lab = oklab_from_2020(rgb);
    let [ws, wm, wh] = grade_zones(lab[0], 0.5, 0.0);
    for (i, wt) in [ws, wm, wh, 1.0].into_iter().enumerate() {
        lab[1] += g[i * 3] * wt;
        lab[2] += g[i * 3 + 1] * wt;
        lab[0] += g[i * 3 + 2] * wt;
    }
    oklab_to_2020(lab)
}

/// Hue rotation (OkLCh radians) of a calibration primary at ±100.
pub const CALIB_HUE: f32 = 0.5;
/// Chroma scale of a calibration primary at ±100 (`1 ± CALIB_SAT`).
pub const CALIB_SAT: f32 = 0.6;

/// The calibration panel's primaries as a white-preserving 3×3 matrix on linear Rec.2020 (row-major):
/// each primary is rotated in OkLCh hue and scaled in chroma at constant OkLab lightness, then the
/// columns are rescaled so that neutral (1, 1, 1) maps to itself. `None` when neutral.
///
/// From process V9 (`linear_sat`) Saturation instead scales the primary's distance from the grey of
/// its own luminance in linear light (fork issue #1031). In OkLab the line from Rec.2020 blue to white
/// bends towards cyan/violet, so scaling the primary's OkLab chroma moved every blue mixture in hue
/// about as much as the Hue slider did, and barely changed its chroma. In linear light the
/// primary and all its mixtures with grey stay on one straight line, so they keep their hue.
pub fn calibration_matrix(c: &Calibration, linear_sat: bool) -> Option<[[f32; 3]; 3]> {
    let prim = c.primaries();
    if prim.iter().all(|(h, s)| *h == 0.0 && *s == 0.0) {
        return None;
    }
    let mut p = [[0.0f64; 3]; 3];
    for (i, (hue, sat)) in prim.iter().enumerate() {
        let mut e = [0.0f32; 3];
        e[i] = 1.0;
        let [l, ch, h] = lab_to_lch(oklab_from_2020(e));
        let h2 = h + (hue.clamp(-100.0, 100.0) / 100.0) as f32 * CALIB_HUE;
        let gain = if linear_sat { 1.0 } else { 1.0 + (sat.clamp(-100.0, 100.0) / 100.0) as f32 * CALIB_SAT };
        let q = oklab_to_2020(lch_to_lab([l, ch * gain, h2]));
        for (r, row) in p.iter_mut().enumerate() {
            row[i] = q[r] as f64;
        }
    }
    let m = Mat3(p);
    let k = m.inverse()?.apply([1.0, 1.0, 1.0]);
    let mut out: [[f64; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|col| p[r][col] * k[col]));
    if linear_sat {
        // S_i = I + g (e_i − Y_i·1)(e_i − y)ᵀ, y the luminance weights: it moves a colour along its
        // primary's line through grey by g × how far its channel i stands above its luminance. Grey
        // and luminance stay; the primary itself gets (1 + g) × its distance from grey.
        let y: [f64; 3] = std::array::from_fn(|i| {
            let mut e = [0.0f32; 3];
            e[i] = 1.0;
            lcc_color::luminance_2020(e) as f64
        });
        for (i, (_, sat)) in prim.iter().enumerate() {
            let g = sat.clamp(-100.0, 100.0) / 100.0 * CALIB_SAT as f64;
            if g == 0.0 {
                continue;
            }
            let yi = y.get(i).copied().unwrap_or(0.0);
            let s: [[f64; 3]; 3] = std::array::from_fn(|r| {
                std::array::from_fn(|col| {
                    let u = if r == i { 1.0 - yi } else { -yi };
                    let v = if col == i { 1.0 - y[col] } else { -y[col] };
                    let id = if r == col { 1.0 } else { 0.0 };
                    id + g * u * v
                })
            });
            out = std::array::from_fn(|r| std::array::from_fn(|col| (0..3).map(|t| s[r][t] * out[t][col]).sum()));
        }
    }
    Some(out.map(|row| row.map(|v| v as f32)))
}

/// Shadows-tint strength at ±100: the green channel's relative change in deep shadows.
pub const SHADOW_TINT: f32 = 0.3;

/// Calibration in scene-linear light (before tone mapping): the primaries matrix, then the shadows
/// tint (green ↔ magenta, weighted towards dark tones, luminance kept).
#[inline]
pub fn calibrate(c: [f32; 3], m: Option<&[[f32; 3]; 3]>, shadow_tint: f32) -> [f32; 3] {
    let mut c = c;
    if let Some(m) = m {
        c = std::array::from_fn(|r| (m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2]).max(0.0));
    }
    if shadow_tint != 0.0 {
        let y0 = lcc_color::luminance_2020(c);
        let w = 1.0 - smooth(-5.0, -0.5, (y0.max(1e-7) / 0.18).log2());
        c[1] *= (1.0 - SHADOW_TINT * shadow_tint * w).max(0.0);
        let y1 = lcc_color::luminance_2020(c).max(1e-9);
        c = c.map(|v| v * y0 / y1);
    }
    c
}

#[inline]
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_color::perceptual::lab_to_lch;

    #[test]
    fn weights_partition_unity() {
        for i in 0..360 {
            let h = (i as f32).to_radians() - PI;
            let w = band_weights(h);
            let s: f32 = w.iter().sum();
            assert!((s - 1.0).abs() < 1e-4, "{i}: {s}");
        }
        // at a band centre, that band dominates
        let w = band_weights(band_hues()[5]);
        assert!(w[5] > 0.99);
    }

    #[test]
    fn neutral_is_identity() {
        let ops = ColorOps::new(&DevelopSettings::default());
        assert!(ops.is_identity());
        assert_eq!(ops.apply([0.2, 0.3, 0.4], 0.0, 0.0), [0.2, 0.3, 0.4]);
    }

    #[test]
    fn saturation_and_bw() {
        let mut s = DevelopSettings::default();
        s.color.saturation = -100.0;
        let g = ColorOps::new(&s).apply([0.4, 0.1, 0.05], 0.0, 0.0);
        assert!((g[0] - g[1]).abs() < 1e-3 && (g[1] - g[2]).abs() < 1e-3, "{g:?}");
        let mut s = DevelopSettings { treatment: lcc_develop::Treatment::Bw, ..Default::default() };
        let ops = ColorOps::new(&s);
        let b = ops.apply([0.05, 0.1, 0.5], 0.0, 0.0);
        assert!((b[0] - b[2]).abs() < 1e-3);
        // raising blue in the B&W mix brightens blue things
        s.bw_mix.blue = 80.0;
        let b2 = ColorOps::new(&s).apply([0.05, 0.1, 0.5], 0.0, 0.0);
        assert!(b2[1] > b[1]);
    }

    #[test]
    fn mixer_targets_its_band() {
        let mut s = DevelopSettings::default();
        s.mixer.blue.sat = -100.0;
        let ops = ColorOps::new(&s);
        let blue = [0.02, 0.05, 0.4];
        let red = [0.4, 0.03, 0.02];
        let cb = lab_to_lch(oklab_from_2020(ops.apply(blue, 0.0, 0.0)))[1];
        let cr0 = lab_to_lch(oklab_from_2020(red))[1];
        let cr = lab_to_lch(oklab_from_2020(ops.apply(red, 0.0, 0.0)))[1];
        assert!(cb < 0.02, "{cb}");
        assert!((cr - cr0).abs() < 0.01);
    }

    #[test]
    fn calibration_keeps_white_and_moves_primaries() {
        assert!(calibration_matrix(&Calibration::default(), false).is_none());
        let cal = Calibration { red_hue: 60.0, blue_sat: -50.0, ..Default::default() };
        let m = calibration_matrix(&cal, false).unwrap();
        let grey = calibrate([0.3, 0.3, 0.3], Some(&m), 0.0);
        assert!(grey.iter().all(|v| (v - 0.3).abs() < 1e-4), "{grey:?}");
        // red rotates in hue, blue loses chroma
        let red = [0.4, 0.05, 0.03];
        let h0 = lab_to_lch(oklab_from_2020(red))[2];
        let h1 = lab_to_lch(oklab_from_2020(calibrate(red, Some(&m), 0.0)))[2];
        assert!(h1 - h0 > 0.05, "{h0} -> {h1}");
        let blue = [0.03, 0.05, 0.4];
        let c0 = lab_to_lch(oklab_from_2020(blue))[1];
        let c1 = lab_to_lch(oklab_from_2020(calibrate(blue, Some(&m), 0.0)))[1];
        assert!(c1 < c0 * 0.97, "{c0} -> {c1}");
    }

    /// #1031: from V9 a primary's Saturation changes the chroma of its colours and leaves their hue,
    /// which the Hue slider moves; grey stays grey.
    #[test]
    fn primary_saturation_scales_chroma_not_hue() {
        let lch = |c: [f32; 3]| lab_to_lch(oklab_from_2020(c));
        let turn = |a: f32, b: f32| wrap(b - a).abs();
        for (i, px) in [[0.4f32, 0.05, 0.03], [0.05, 0.35, 0.05], [0.03, 0.05, 0.4], [0.06, 0.08, 0.4]].into_iter().enumerate() {
            let [_, c0, h0] = lch(px);
            let prim = if i < 2 { i } else { 2 };
            let cal = |hue: f64, sat: f64| {
                let mut c = Calibration::default();
                match prim {
                    0 => (c.red_hue, c.red_sat) = (hue, sat),
                    1 => (c.green_hue, c.green_sat) = (hue, sat),
                    _ => (c.blue_hue, c.blue_sat) = (hue, sat),
                }
                calibration_matrix(&c, true).unwrap()
            };
            let more = cal(0.0, 100.0);
            let less = cal(0.0, -100.0);
            let hue = cal(100.0, 0.0);
            let [_, c_more, h_more] = lch(calibrate(px, Some(&more), 0.0));
            let [_, c_less, h_less] = lch(calibrate(px, Some(&less), 0.0));
            let [_, _, h_hue] = lch(calibrate(px, Some(&hue), 0.0));
            assert!(c_more > c0 * 1.15 && c_less < c0 * 0.85, "{px:?}: chroma {c0} -> {c_less} / {c_more}");
            let rotated = turn(h0, h_hue);
            assert!(turn(h0, h_more) < rotated * 0.5 && turn(h0, h_less) < rotated * 0.5, "{px:?}: hue {h0} -> {h_less} / {h_more} (Hue: {h_hue})");
            for m in [more, less, hue] {
                let grey = calibrate([0.3, 0.3, 0.3], Some(&m), 0.0);
                assert!(grey.iter().all(|v| (v - 0.3).abs() < 1e-4), "{grey:?}");
            }
        }
    }

    #[test]
    fn shadow_tint_targets_shadows() {
        let dark = calibrate([0.01, 0.01, 0.01], None, 1.0);
        let bright = calibrate([0.8, 0.8, 0.8], None, 1.0);
        assert!(dark[1] < dark[0] * 0.85, "magenta shadows: {dark:?}");
        assert!((bright[1] - bright[0]).abs() < 1e-3, "{bright:?}");
        let y = |c: [f32; 3]| lcc_color::luminance_2020(c);
        assert!((y(dark) - 0.01).abs() < 1e-5);
        let green = calibrate([0.01, 0.01, 0.01], None, -1.0);
        assert!(green[1] > green[0]);
    }

    #[test]
    fn point_color_targets_its_range() {
        let skin = [0.35, 0.16, 0.09];
        let [l, c, h] = lab_to_lch(oklab_from_2020(skin));
        let sample =
            PointColor { lum: l as f64, chroma: c as f64, hue: (h as f64).to_degrees(), hue_shift: 60.0, sat_shift: -50.0, ..Default::default() };
        let mut s = DevelopSettings::default();
        s.point_colors.push(sample);
        let ops = ColorOps::new(&s);
        let out = lab_to_lch(oklab_from_2020(ops.apply(skin, 0.0, 0.0)));
        assert!(out[1] < c * 0.7, "chroma {c} -> {}", out[1]);
        assert!(wrap(out[2] - h) > 0.2, "hue {h} -> {}", out[2]);
        // far colours are untouched
        for far in [[0.02, 0.05, 0.4], [0.05, 0.3, 0.05], [0.3, 0.3, 0.3]] {
            let o = ops.apply(far, 0.0, 0.0);
            assert!(far.iter().zip(&o).all(|(a, b)| (a - b).abs() < 1e-4), "{far:?} -> {o:?}");
        }
        // weight: 1 at the sample, falling to 0 outside the range; a wider range reaches further
        let k = PointK::new(&sample);
        assert!((k.weight(l, c, h) - 1.0).abs() < 1e-6);
        assert_eq!(k.weight(l, c, h + 1.0), 0.0);
        let wide = PointK::new(&PointColor { range: 100.0, ..sample });
        assert!(wide.weight(l, c, h + 0.45) > k.weight(l, c, h + 0.45));
    }

    #[test]
    fn point_color_variance_compresses_towards_the_sample() {
        let p = PointColor { lum: 0.6, chroma: 0.1, hue: 40.0, variance: -100.0, range: 100.0, ..Default::default() };
        let k = PointK::new(&p);
        let (h0, near) = (40f32.to_radians(), 40f32.to_radians() + 0.1);
        let o = k.apply([0.62, 0.11, near]);
        assert!((o[2] - h0).abs() < 0.1 && (o[1] - 0.1).abs() < 0.01 && (o[0] - 0.6).abs() < 0.02, "{o:?}");
        let k = PointK::new(&PointColor { variance: 100.0, ..p });
        let o = k.apply([0.62, 0.11, near]);
        assert!(o[2] - h0 > 0.1, "{o:?}");
    }

    #[test]
    fn grading_tints_shadows_only() {
        let mut s = DevelopSettings::default();
        s.grading.shadows = lcc_develop::Wheel { hue: 220.0, sat: 60.0, lum: 0.0 };
        let ops = ColorOps::new(&s);
        let dark = ops.apply([0.01, 0.01, 0.01], 0.0, 0.0);
        let bright = ops.apply([0.8, 0.8, 0.8], 0.0, 0.0);
        assert!(dark[2] > dark[0]);
        assert!((bright[2] - bright[0]).abs() < 0.02);
    }

    #[test]
    fn mask_grading_tints_its_zone_and_scales_with_the_mask() {
        let a = lcc_develop::LocalAdjustments { grade_shadows_hue: 220.0, grade_shadows_sat: 60.0, ..Default::default() };
        let full = local_grade_terms(&a);
        let dark = local_grade([0.01, 0.01, 0.01], &full);
        let bright = local_grade([0.8, 0.8, 0.8], &full);
        assert!(dark[2] > dark[0], "shadows turn blue: {dark:?}");
        assert!((bright[2] - bright[0]).abs() < 0.02, "highlights keep their colour: {bright:?}");
        // half covered by the mask: half the tint
        let half = local_grade([0.01, 0.01, 0.01], &full.map(|v| v * 0.5));
        assert!(half[2] - half[0] > 0.0 && half[2] - half[0] < dark[2] - dark[0]);
        // neutral wheels change nothing
        let none = local_grade_terms(&lcc_develop::LocalAdjustments::default());
        assert_eq!(local_grade([0.3, 0.2, 0.1], &none), [0.3, 0.2, 0.1]);
    }

    #[test]
    fn a_mask_from_before_grading_reads_as_ungraded() {
        let a: lcc_develop::LocalAdjustments =
            serde_json::from_str(r#"{"exposure": 0.5, "color_hue": 0.0, "color_sat": 0.0, "amount": 100.0}"#).unwrap();
        assert_eq!(a.exposure, 0.5);
        assert!(a.grading_wheels().iter().all(|w| w.hue == 0.0 && w.sat == 0.0 && w.lum == 0.0));
        assert!(local_grade_terms(&a).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn the_color_wheels_are_asc_cdl_sop() {
        // the published shape: lift pivots at white, gain at black, gamma a power, offset adds
        let full = lcc_develop::Wheel { hue: 0.0, sat: 0.0, lum: 100.0 };
        let of = |wheel: lcc_develop::Wheel, set: fn(lcc_develop::Primaries, lcc_develop::Wheel) -> lcc_develop::Primaries| {
            PrimariesK::new(&set(lcc_develop::Primaries::default(), wheel)).unwrap()
        };
        // lift's master at full: blacks move by the reach, whites stay exactly
        let k = of(full, |s, w| lcc_develop::Primaries { lift: w, ..s });
        let white = k.apply([1.0, 1.0, 1.0]);
        assert!(white.iter().all(|v| (v - 1.0).abs() < 1e-6), "lift pivots at white: {white:?}");
        let black = k.apply([0.0, 0.0, 0.0]);
        assert!(black.iter().all(|v| (v - PRIMARY_LIFT_REACH).abs() < 1e-6), "lift moves blacks by its reach: {black:?}");
        // gain: black stays exactly, everything above scales
        let k = of(full, |s, w| lcc_develop::Primaries { gain: w, ..s });
        let black = k.apply([0.0, 0.0, 0.0]);
        assert!(black.iter().all(|v| *v == 0.0), "gain pivots at black: {black:?}");
        let mid = k.apply([0.5, 0.5, 0.5]);
        assert!(mid.iter().all(|v| (v - 0.5 * (1.0 + PRIMARY_GAIN_REACH)).abs() < 1e-6), "gain multiplies: {mid:?}");
        // gamma: a power — 0 and 1 stay, the middle moves hard
        let k = of(full, |s, w| lcc_develop::Primaries { gamma: w, ..s });
        let ends = [k.apply([0.0, 0.0, 0.0]), k.apply([1.0, 1.0, 1.0])];
        assert!(ends.iter().all(|c| c.iter().all(|v| *v == 0.0 || (v - 1.0).abs() < 1e-5)), "gamma keeps black and white: {ends:?}");
        let mid = k.apply([0.25, 0.25, 0.25]);
        assert!(mid.iter().all(|v| *v > 0.35), "gamma lifts midtones hard: {mid:?}");
        // offset: everything, equally
        let k = of(full, |s, w| lcc_develop::Primaries { offset: w, ..s });
        let out = k.apply([0.4, 0.4, 0.4]);
        assert!(out.iter().all(|v| (v - 0.4 - PRIMARY_OFFSET_REACH).abs() < 1e-6), "{out:?}");
    }

    #[test]
    fn the_log_wheels_carry_their_own_strengths() {
        // the Log set's calibration: shadows halved, midtones as tuned, highlights ×3 (a multiply,
        // kept under the cliff — a full pull never clamps its zone to black)
        let full = lcc_develop::Wheel { hue: 0.0, sat: 100.0, lum: 100.0 };
        let fl = LOG_STRENGTH;
        let s = lcc_develop::Primaries {
            log_wheels: lcc_develop::LogWheels { shadow: full, midtone: full, highlight: full, offset: full },
            ..Default::default()
        };
        let k = PrimariesK::new(&s).unwrap();
        assert!((k.highlight.master - PRIMARY_GAIN_MASTER * fl[2]).abs() < 1e-6);
        assert!((k.midtone.rgb[0] / k.shadow.rgb[0] - fl[1] / fl[0]).abs() < 1e-3);
        // a full pull on every wheel stays finite on real values
        for c in [[0.0, 0.0, 0.0], [0.9, 0.9, 0.9]] {
            let out = k.apply(c);
            assert!(out.iter().all(|v| v.is_finite()), "{c:?} -> {out:?}");
        }
        // the highlight wheel alone is a graded reach, never a blow-out: a full pull on a bright
        // pixel stays above black (the multiplier keeps 1 − reach positive), a full push under ×2
        let mut s = lcc_develop::Primaries::default();
        s.log_wheels.highlight = lcc_develop::Wheel { hue: 0.0, sat: 0.0, lum: -100.0 };
        let down = PrimariesK::new(&s).unwrap();
        assert!(down.apply([0.95, 0.95, 0.95]).iter().all(|v| *v > 0.0), "no clamp to black");
        s.log_wheels.highlight = lcc_develop::Wheel { hue: 0.0, sat: 0.0, lum: 100.0 };
        let up = PrimariesK::new(&s).unwrap();
        assert!(up.apply([0.95, 0.95, 0.95]).iter().all(|v| *v < 2.0), "no leap to white");
    }

    #[test]
    fn hsl_curves_neutral_when_empty_and_targeted_when_not() {
        assert!(HslK::new(&lcc_develop::HslCurves::default()).is_none());
        // a curve on one channel only still counts
        let mut s = DevelopSettings::default();
        s.hsl_curves.sat = vec![lcc_geom::Point::new(0.5, 1.0)];
        assert!(!ColorOps::new(&s).is_identity());
        // in a black-and-white rendition the curves sit out, as the mixer does
        let bw = DevelopSettings { treatment: lcc_develop::Treatment::Bw, ..s.clone() };
        assert!(ColorOps::new(&bw).hsl.is_none());
        // the spline passes through the point itself; the LUT samples it
        let pts = [lcc_geom::Point::new(0.25, -1.0)];
        assert!((hsl_curve_at(&pts, 0.25) + 1.0).abs() < 1e-9);
        assert!((hsl_curve_at(&pts, 0.0)).abs() < 1e-9, "the hue axis's ends stay neutral");
        let k = HslK::new(&lcc_develop::HslCurves { sat: pts.to_vec(), ..Default::default() }).unwrap();
        let [_, ds, _] = k.at(0.25);
        assert!(ds < -0.9, "the LUT reads the point: {ds}");
        let [_, ds0, _] = k.at(0.02);
        assert!(ds0.abs() < 0.3, "away from the point the curve eases out: {ds0}");
    }

    #[test]
    fn an_hsl_saturation_curve_touches_only_its_hue() {
        // a bump pinned between two flat points: the hue inside gains, a hue well outside stays
        let mut s = DevelopSettings::default();
        s.hsl_curves.sat = vec![lcc_geom::Point::new(0.3, 0.0), lcc_geom::Point::new(0.55, 0.9)];
        let ops = ColorOps::new(&s);
        let of_hue = |x: f32| {
            let lab = lch_to_lab([0.5, 0.15, x * std::f32::consts::TAU]);
            oklab_to_2020(lab)
        };
        let inside = of_hue(0.55);
        let outside = of_hue(0.05);
        let chroma = |rgb: [f32; 3]| lab_to_lch(oklab_from_2020(rgb))[1];
        let i = chroma(ops.apply(inside, 0.0, 0.0));
        let o = chroma(ops.apply(outside, 0.0, 0.0));
        assert!(i > chroma(inside) * 1.5, "the hue under the point gains: {} -> {i}", chroma(inside));
        assert!((o - chroma(outside)).abs() < 1e-3, "a hue beyond the pinned flat stays: {o}");
    }

    #[test]
    fn hsl_curve_points_from_outside_clamp_and_cap() {
        let v = DevelopSettings::from_untrusted_json(&serde_json::json!({
            "hsl_curves": {"hue": [[-1.0, 5.0], [2.0, -9.0]], "lum": [[0.5, 0.25]]}
        }))
        .unwrap();
        let hue = &v.hsl_curves.hue;
        assert!(hue.iter().all(|p| (0.0..=1.0).contains(&p.x) && (-1.0..=1.0).contains(&p.y)), "{hue:?}");
        // a hostile pile of points is capped on read, as the tone curve's are
        let many: Vec<[f64; 2]> = (0..600).map(|i| [i as f64 / 600.0, 0.5]).collect();
        let v = DevelopSettings::from_untrusted_json(&serde_json::json!({"hsl_curves": {"sat": many}})).unwrap();
        assert!(v.hsl_curves.sat.len() <= lcc_develop::MAX_CURVE_POINTS);
    }

    #[test]
    fn primaries_neutral_is_identity() {
        assert!(PrimariesK::new(&lcc_develop::Primaries::default()).is_none());
        // the ranges alone move nothing, whatever they are set to
        let p = lcc_develop::Primaries { low: 0.0, high: 1.0, ..Default::default() };
        assert!(PrimariesK::new(&p).is_none());
        // a Log wheel alone is an edit like any Color one
        let mut s = DevelopSettings::default();
        s.primaries.log_wheels.shadow = lcc_develop::Wheel { hue: 220.0, sat: 40.0, lum: 0.0 };
        assert!(!ColorOps::new(&s).is_identity());
        // a hue with no saturation is no move either (as the grading wheels read it)
        let mut s = DevelopSettings::default();
        s.primaries.lift.hue = 220.0;
        assert!(ColorOps::new(&s).is_identity());
    }

    #[test]
    fn a_lift_puck_tints_shadows_harder_than_highlights() {
        let mut s = DevelopSettings::default();
        s.primaries.lift = lcc_develop::Wheel { hue: 220.0, sat: 100.0, lum: 0.0 };
        let ops = ColorOps::new(&s);
        let p = ops.primaries.as_ref().unwrap();
        let dark = p.apply([0.02, 0.02, 0.02]);
        let bright = p.apply([0.8, 0.8, 0.8]);
        let white = p.apply([1.0, 1.0, 1.0]);
        assert!(dark[2] > dark[0] + 0.1, "shadows turn blue hard: {dark:?}");
        assert!(bright[2] - bright[0] < dark[2] - dark[0], "the tint fades towards white: {bright:?}");
        assert!(white.iter().all(|v| (v - 1.0).abs() < 1e-5), "white pivots: {white:?}");
    }

    #[test]
    fn a_gain_puck_scales_proportionally() {
        let mut s = DevelopSettings::default();
        s.primaries.gain = lcc_develop::Wheel { hue: 220.0, sat: 100.0, lum: 0.0 };
        let p = PrimariesK::new(&s.primaries).unwrap();
        let black = p.apply([0.0, 0.0, 0.0]);
        assert!(black.iter().all(|v| *v == 0.0), "gain pivots at black: {black:?}");
        let low = p.apply([0.2, 0.2, 0.2]);
        let high = p.apply([0.8, 0.8, 0.8]);
        // a multiply: every level scales by its channel's factor, blue harder than red
        assert!(high[2] / 0.8 - low[2] / 0.2 < 1e-4 && high[2] / 0.8 > high[0] / 0.8, "{low:?} {high:?}");
    }

    #[test]
    fn primaries_offset_moves_everything_equally() {
        let mut s = DevelopSettings::default();
        s.primaries.offset = lcc_develop::Wheel { hue: 0.0, sat: 0.0, lum: 100.0 };
        let p = PrimariesK::new(&s.primaries).unwrap();
        for c in [[0.02, 0.02, 0.02], [0.5, 0.4, 0.3], [0.95, 0.95, 0.95]] {
            let out = p.apply(c);
            let d = out[0] - c[0];
            assert!(out.iter().zip(&c).all(|(o, i)| ((o - i) - d).abs() < 1e-6), "{c:?} -> {out:?}");
            assert!(d > 0.0);
        }
    }

    #[test]
    fn the_log_wheels_stay_narrow_and_both_sets_stack() {
        // the same pull on lift (Color, wide) and shadow (Log, narrow): the wide one reaches well
        // into the midtones, the narrow one stops below them — and both together stack additively
        let pull = lcc_develop::Wheel { hue: 220.0, sat: 100.0, lum: 0.0 };
        let s = lcc_develop::Primaries { lift: pull, ..Default::default() };
        let wide = PrimariesK::new(&s).unwrap();
        let s2 = lcc_develop::Primaries { log_wheels: lcc_develop::LogWheels { shadow: pull, ..Default::default() }, ..Default::default() };
        let log = PrimariesK::new(&s2).unwrap();
        let mid = [0.5, 0.5, 0.5];
        let w = wide.apply(mid);
        let l = log.apply(mid);
        assert!(w[2] - w[0] > (l[2] - l[0]) + 0.02, "wide {w:?} vs narrow {l:?}");
        // the narrow shadows still reach true shadows
        let dark = log.apply([0.02, 0.02, 0.02]);
        assert!(dark[2] > dark[0] + 0.02, "{dark:?}");
        // complimentary: with both sets pulled, the midtones see (at least) the Color wheels' tilt
        let both =
            lcc_develop::Primaries { lift: pull, log_wheels: lcc_develop::LogWheels { shadow: pull, ..Default::default() }, ..Default::default() };
        let k = PrimariesK::new(&both).unwrap();
        let stacked_mid = k.apply(mid);
        assert!(stacked_mid[2] - stacked_mid[0] > 0.0, "both sets render together: {stacked_mid:?}");
    }

    #[test]
    fn primaries_words_round_trip_the_gpu_layout() {
        let p = lcc_develop::Primaries {
            lift: lcc_develop::Wheel { hue: 220.0, sat: 50.0, lum: -10.0 },
            gain: lcc_develop::Wheel { hue: 0.0, sat: 25.0, lum: 15.0 },
            log_wheels: lcc_develop::LogWheels {
                shadow: lcc_develop::Wheel { hue: 220.0, sat: 50.0, lum: -10.0 },
                highlight: lcc_develop::Wheel { hue: 0.0, sat: 25.0, lum: 15.0 },
                ..Default::default()
            },
            low: 0.25,
            high: 0.8,
            ..Default::default()
        };
        let k = PrimariesK::new(&p).unwrap();
        let w = k.words();
        assert_eq!(w.len(), PRIMARY_WORDS);
        assert_eq!([w[0], w[1], w[2], w[3]], [k.lift.rgb[0], k.lift.rgb[1], k.lift.rgb[2], k.lift.master]);
        assert_eq!([w[8], w[9], w[10], w[11]], [k.gain.rgb[0], k.gain.rgb[1], k.gain.rgb[2], k.gain.master]);
        assert_eq!([w[16], w[17], w[18], w[19]], [k.shadow.rgb[0], k.shadow.rgb[1], k.shadow.rgb[2], k.shadow.master]);
        assert_eq!([w[28], w[29], w[30], w[31]], [k.log_offset.rgb[0], k.log_offset.rgb[1], k.log_offset.rgb[2], k.log_offset.master]);
        assert_eq!((w[32], w[33]), (k.low, k.high));
    }

    #[test]
    fn primaries_hostile_settings_stay_finite_and_in_range() {
        // from_untrusted_json clamps through the control specs (a sat of inf reads as the default);
        // an older experimental `log` switch in the JSON is left unread
        let v = DevelopSettings::from_untrusted_json(&serde_json::json!({
            "primaries": {
                "lift": {"hue": -900.0, "sat": 1e300, "lum": -1e300},
                "gain": {"hue": 1e308, "sat": -50.0, "lum": 1e9},
                "log": true,
                "logWheels": {"shadow": {"hue": 1e9, "sat": 1e9, "lum": -1e9}},
                "low": -2.0, "high": 99.0
            }
        }))
        .unwrap();
        let p = &v.primaries;
        assert!((0.0..=360.0).contains(&p.lift.hue) && (0.0..=100.0).contains(&p.lift.sat) && (-100.0..=100.0).contains(&p.lift.lum));
        assert!((0.0..=1.0).contains(&p.low) && (0.0..=1.0).contains(&p.high));
        assert!((-100.0..=100.0).contains(&p.log_wheels.shadow.lum), "the Log wheels clamp too");
        let k = PrimariesK::new(p).unwrap();
        for c in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.2, 0.5, 0.9]] {
            let out = k.apply(c);
            assert!(out.iter().all(|v| v.is_finite() && *v >= 0.0), "{c:?} -> {out:?}");
        }
    }
}
