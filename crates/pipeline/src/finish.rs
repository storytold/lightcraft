//! The per-pixel stage: everything after the spatial planes are ready, in one parallel pass.
//!
//! **Tone curves** (parametric + master + red/green/blue point curves, Refine Saturation) run in
//! one fixed curve space, whatever the output space: linear Rec.2020 → linear ProPhoto (ROMM)
//! primaries, Bradford-adapted to D50 like every D50 target here, encoded with the sRGB transfer
//! curve (the "Melissa RGB" convention Lightroom shows its RGB readouts in; Lightroom Classic's
//! red / green / blue curves on a grey ramp pass within 0.3/255 of their points in this
//! encoding). The parametric ∘ master curve is hue-preserving ([`apply_curves`]), as Lightroom's
//! is; the channel curves act per channel. Only the 0..1 part of each channel goes through the
//! curve tables: the part outside (a channel above white, or a negative channel of a colour
//! outside ProPhoto) is carried past the curve unchanged, then the output's gamut mapping handles
//! it. So a preset looks the same exported to sRGB, Display P3, Adobe RGB or ProPhoto, up to that
//! final gamut mapping.
//!
//! **Sharpening** acts on the finished image, as Lightroom's does (after the tone curves: under a
//! flat or a steep master curve Lightroom's sharpening follows the same law on its output, not on
//! its input). See [`Sharpen`]: the per-pixel stage runs in two passes when it is on — the colour
//! of every pixel, then a Gaussian blur of their display-linear luminance (σ from Amount, Radius
//! and Detail, in source pixels) and a luminance gain from each pixel's ratio to that blur.

use lightcraft_color::spline::{Lut1, PointCurve};
use lightcraft_color::transfer::{linear_to_srgb, srgb_to_linear};
use lightcraft_color::{PROPHOTO, REC2020, luminance_2020};
use lightcraft_develop::{DevelopSettings, LocalAdjustments, ToneCurve, VignetteStyle};
use lightcraft_geom::Point;
use lightcraft_raster::Rgba8;

use crate::colorops::ColorOps;
use crate::geometry::Frame;
use crate::local::log_lum;
use crate::output::{DeepImage, DeepSamples, OutputDepth, OutputSpace, OutputTrc};
use crate::tone::ToneMap;
use crate::{Prepared, SourceInfo, for_rows};

#[inline]
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Parametric curve regions as Lightroom shapes them, fitted to Lightroom Classic 15.6 renders of a
/// grey ramp (each region at ±30 / ±60 with the default splits and at ±60 with splits 13 / 62 / 76,
/// and a preset's four regions together; grey curve within 1.3/255 for that preset, single regions
/// mostly within 1–3/255). Each region moves the encoded value by `±A · |amount/100|^η · bump`, a
/// smooth peak of height 1 over its span ([`param_geometry`]); the regions apply one after the other
/// (Lights, Shadows, Highlights, then Darks), each on the previous result, which is how Lightroom
/// combines them (adding them overshoots in the highlights). Per region (Shadows, Darks, Lights,
/// Highlights) and sign (`[positive, negative]`): (A, sharpness, η).
const PARAM_BUMP: [[(f32, f32, f32); 2]; 4] = [
    [(0.1034, 0.6642, 1.3277), (0.1002, 0.7514, 0.7513)],
    [(0.2099, 0.9492, 1.0966), (0.2774, 0.856, 1.0663)],
    [(0.2307, 2.9818, 0.8182), (0.2038, 2.8146, 0.9803)],
    [(0.1337, 1.9981, 0.9593), (0.1204, 1.8523, 1.0215)],
];
/// The regions' spans from the splits (see [`param_geometry`]).
const PARAM_GEOM: [f32; 8] = [0.6174, 0.0017, 0.0199, 0.6383, 0.6208, 0.0, 0.1852, 0.3864];
/// The order Lightroom applies the regions in (indices into [`PARAM_BUMP`]).
const PARAM_ORDER: [usize; 4] = [2, 0, 3, 1];

/// Each region's (start, peak, end) from the shadows / midtones / highlights splits: Shadows over
/// 0..midtones split, Darks from 0 to past the highlights split, Lights from near 0 to 1,
/// Highlights over midtones split..1; the peaks are blends of the splits.
fn param_geometry(s1: f32, s2: f32, s3: f32) -> [(f32, f32, f32); 4] {
    let g = PARAM_GEOM;
    [
        (0.0, g[0] * s1 + g[1] * s2, s2),
        (0.0, g[2] * s1 + g[3] * s2, s3 + g[4] * (1.0 - s3)),
        (g[5] * s1, g[6] * s2 + (1.0 - g[6]) * s3, 1.0),
        (s2, s3 + g[7] * (1.0 - s3), 1.0),
    ]
}

/// A peak of height 1 at `c` over `l..r` (0 outside), of sharpness `p`.
fn param_bump(x: f32, (l, c, r): (f32, f32, f32), p: f32) -> f32 {
    let span = (r - l).max(1e-6);
    let t = ((x - l) / span).clamp(0.0, 1.0);
    let tc = ((c - l) / span).clamp(1e-3, 1.0 - 1e-3);
    let q = p * (1.0 - tc) / tc;
    (t / tc).powf(p) * ((1.0 - t) / (1.0 - tc)).powf(q)
}

/// Tone curve tables on curve-space encoded values: `[0]` the parametric region curve composed
/// with the master point curve, `[1..4]` the red / green / blue point curves (identity if unset).
fn curve_luts(c: &ToneCurve) -> Option<[Lut1; 4]> {
    let parametric = c.highlights != 0.0 || c.lights != 0.0 || c.darks != 0.0 || c.shadows != 0.0;
    let master = !ToneCurve::point_curve_is_identity(&c.master);
    let chans = [&c.red, &c.green, &c.blue].map(|p| !ToneCurve::point_curve_is_identity(p));
    if !parametric && !master && !chans.iter().any(|b| *b) {
        return None;
    }
    const N: usize = 1024;
    let (s1, s2, s3) = ((c.split_shadows / 100.0) as f32, (c.split_mid / 100.0) as f32, (c.split_highlights / 100.0) as f32);
    let geometry = param_geometry(s1, s2, s3);
    let amounts = [c.shadows, c.darks, c.lights, c.highlights].map(|a| (a.clamp(-100.0, 100.0) / 100.0) as f32);
    let mut base = Lut1::from_fn(N, |x| {
        let mut y = x;
        for i in PARAM_ORDER {
            let a = amounts[i];
            if a == 0.0 {
                continue;
            }
            let (amp, p, eta) = PARAM_BUMP[i][usize::from(a < 0.0)];
            y = (y + a.signum() * amp * a.abs().powf(eta) * param_bump(y, geometry[i], p)).clamp(0.0, 1.0);
        }
        y
    });
    // keep monotone
    for i in 1..N {
        base.v[i] = base.v[i].max(base.v[i - 1]);
    }
    let to_pts = |p: &[Point]| p.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>();
    if master {
        base = PointCurve::new(&to_pts(&c.master)).to_lut(N).compose(&base);
    }
    let per = [&c.red, &c.green, &c.blue];
    let chan = |i: usize| if chans[i] { PointCurve::new(&to_pts(per[i])).to_lut(N) } else { Lut1::from_fn(N, |x| x) };
    Some([base, chan(0), chan(1), chan(2)])
}

/// Post-crop vignette strength, fitted to Lightroom Classic renders (Apple ProRAW at ±50): Highlight
/// and Colour Priority are an exposure change before the tone map (so the tone curve's toe darkens
/// shadows more than highlights, as in Lightroom), `exp(k · amount · falloff)`. k for darkening in
/// Highlight Priority, darkening in Colour Priority, and lightening. Mean ΔE00 vs Lightroom at −50:
/// 1.67 / 1.71; lightening (+50) stays far off (6.1: Lightroom's has another shape).
pub const VIG_STRENGTH: [f32; 3] = [4.0, 4.5, 1.5];

/// Vignette (post-crop) parameters.
#[derive(Clone, Copy, Debug)]
pub struct Vig {
    pub amount: f32,
    /// ln exposure gain at full falloff (Highlight / Colour Priority; see [`VIG_STRENGTH`]).
    pub strength: f32,
    pub start: f32,
    pub width: f32,
    pub aspect_mix: f32,
    pub power: f32,
    pub highlights: f32,
    pub style: VignetteStyle,
}

impl Vig {
    /// 0 inside the vignette, rising to 1 towards the corners of a `w` × `h` frame at pixel (x, y).
    #[inline]
    pub fn falloff(&self, x: usize, y: usize, w: usize, h: usize) -> f32 {
        let aspect = w as f32 / h as f32;
        let u = (x as f32 + 0.5) / w as f32 * 2.0 - 1.0;
        let vv = (y as f32 + 0.5) / h as f32 * 2.0 - 1.0;
        let sx = 1.0 + (aspect - 1.0) * self.aspect_mix;
        let sy = 1.0 + (1.0 / aspect - 1.0) * self.aspect_mix;
        let (ax, ay) = ((u * sx.max(1.0) / sx.max(sy)).abs(), (vv * sy.max(1.0) / sx.max(sy)).abs());
        let dist = (ax.powf(self.power) + ay.powf(self.power)).powf(1.0 / self.power);
        smooth(self.start, self.start + self.width, dist)
    }
}

fn vignette(s: &DevelopSettings) -> Option<Vig> {
    let v = &s.vignette;
    (v.amount != 0.0).then(|| {
        let r = (v.roundness / 100.0) as f32;
        let amount = (v.amount / 100.0) as f32;
        let k = match v.style {
            _ if amount > 0.0 => VIG_STRENGTH[2],
            VignetteStyle::ColorPriority => VIG_STRENGTH[1],
            _ => VIG_STRENGTH[0],
        };
        Vig {
            amount,
            strength: k * amount,
            // Midpoint / Feather 50: the falloff Lightroom's renders fit (from half the way out
            // to the corners, over the whole remaining distance)
            start: 0.025 + (v.midpoint / 100.0) as f32 * 0.95,
            width: 0.05 + (v.feather / 100.0) as f32 * 1.9,
            aspect_mix: ((r + 1.0) / 2.0).clamp(0.0, 1.0),
            power: if r >= 0.0 { 2.0 } else { 2.0 + (-r) * 6.0 },
            highlights: (v.highlights / 100.0) as f32,
            style: v.style,
        }
    })
}

/// Hash constants of [`grain_noise`] (shared with the GPU kernel).
pub const GRAIN_HASH: [u32; 4] = [0x8da6_b343, 0xd816_3841, 0xcb1a_b31f, 0x5bd1_e995];

#[inline]
fn grain_noise(x: f32, y: f32, seed: u32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let h = |i: i32, j: i32| {
        let mut v = (i as u32).wrapping_mul(GRAIN_HASH[0]) ^ (j as u32).wrapping_mul(GRAIN_HASH[1]) ^ seed.wrapping_mul(GRAIN_HASH[2]);
        v ^= v >> 13;
        v = v.wrapping_mul(GRAIN_HASH[3]);
        v ^= v >> 15;
        (v & 0xffff) as f32 / 32768.0 - 1.0
    };
    let (i, j) = (x0 as i32, y0 as i32);
    let (u, v) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let a = h(i, j) + (h(i + 1, j) - h(i, j)) * u;
    let b = h(i, j + 1) + (h(i + 1, j + 1) - h(i, j + 1)) * u;
    a + (b - a) * v
}

/// Number of per-mask terms in [`mask_terms`].
pub const MASK_TERMS: usize = 21;

/// Number of alpha-weighted terms at the start of [`mask_terms`].
pub const MASK_SUMS: usize = 17;

/// A mask's local adjustments as the per-pixel stage uses them (alpha-weighted sums), in order:
/// exposure, temp, tint, contrast, highlights, shadows, whites, blacks, texture, clarity, dehaze,
/// saturation, hue, sharpness, noise, moiré, defringe, then the colour overlay: on (0/1), cos and
/// sin of its OkLCh hue, and its strength.
pub fn mask_terms(j: &LocalAdjustments) -> [f32; MASK_TERMS] {
    let (on, cos, sin, amt) = if j.color_sat > 0.0 {
        let hue = crate::colorops::oklch_hue_of_srgb_hue(j.color_hue);
        (1.0, hue.cos(), hue.sin(), (j.color_sat / 100.0) as f32)
    } else {
        (0.0, 0.0, 0.0, 0.0)
    };
    [
        j.exposure as f32,
        (j.temp / 100.0) as f32,
        (j.tint / 100.0) as f32,
        (j.contrast / 100.0) as f32,
        (j.highlights / 100.0) as f32,
        (j.shadows / 100.0) as f32,
        (j.whites / 100.0) as f32,
        (j.blacks / 100.0) as f32,
        (j.texture / 100.0) as f32,
        (j.clarity / 100.0) as f32,
        (j.dehaze / 100.0) as f32,
        (j.saturation / 100.0) as f32,
        (j.hue / 100.0) as f32 * 0.6,
        (j.sharpness / 100.0) as f32,
        (j.noise / 100.0) as f32,
        (j.moire / 100.0) as f32,
        (j.defringe / 100.0) as f32,
        on,
        cos,
        sin,
        amt,
    ]
}

/// Everything the per-pixel stage computes once per render: the CPU loop below and the GPU kernel
/// (`lightcraft-gpu`) both read their parameters from here, so the two cannot drift apart.
pub struct FinishParams {
    /// A LUT profile and its amount (0..2), applied to the display-encoded colour.
    pub lut: Option<(std::sync::Arc<crate::lut::Lut3d>, f32)>,
    pub tone: ToneMap,
    pub ops: ColorOps,
    /// Calibration: primaries matrix (row-major, linear Rec.2020) and shadows tint (−1..1).
    pub calib: Option<[[f32; 3]; 3]>,
    pub shadow_tint: f32,
    /// Tone curves on curve-space encoded values, 1024 entries each: parametric ∘ master, then
    /// red / green / blue (see [`apply_curves`] and the module docs).
    pub curves: Option<[Lut1; 4]>,
    /// Linear Rec.2020 → linear curve space, its inverse, and the curve space's luminance weights.
    pub curve_in: [[f32; 3]; 3],
    pub curve_out: [[f32; 3]; 3],
    pub curve_luma: [f32; 3],
    /// Refine Saturation as 0..1 (1 = the curves' own saturation).
    pub refine_sat: f32,
    pub vig: Option<Vig>,
    /// Linear Rec.2020 → linear output RGB, the output's luminance weights (gamut mapping) and its
    /// encoding curve (see [`crate::output`]).
    pub to_out: [[f32; 3]; 3],
    pub out_luma: [f32; 3],
    pub out_trc: OutputTrc,
    /// Soft proofing (CPU only; the GPU path declines proof renders).
    pub proof: Option<crate::output::ProofParams>,
    /// Global Highlights / Shadows (−1..1) of the scene-linear local tone step; 0 when they run
    /// as Lightroom applies them instead (`lr_hs`).
    pub hl: f32,
    pub sh: f32,
    /// Highlights / Shadows after the tone map, as Lightroom applies them on a source with a DNG
    /// profile tone curve ([`crate::tone::LrHs`]); the base plane is then the neighbourhood blur
    /// ([`crate::tone::LR_CONTEXT_SIGMA`]).
    pub lr_hs: Option<crate::tone::LrHs>,
    pub clar: f32,
    pub tex: f32,
    pub dehaze: f32,
    /// Sharpening on the finished image ([`Sharpen`]).
    pub sharpen: Option<Sharpen>,
    /// Airlight after and before exposure, exposure gain and EV (see [`crate::Prepared`]).
    pub air: f32,
    pub air_pre: f32,
    pub gain: f32,
    pub ev: f32,
    /// Grain: amount, cell size (px), roughness, seed.
    pub grain: Option<(f32, f32, f32, u32)>,
    /// Output px → normalized oriented coordinates.
    pub out_to_norm: lightcraft_geom::Affine,
    pub ow: f64,
    pub oh: f64,
    pub w: usize,
    pub h: usize,
    pub px_per_long: f64,
}

/// The tone curves' space (see the module docs): linear Rec.2020 → linear ProPhoto (D50), its
/// inverse, and ProPhoto's luminance weights.
pub fn curve_space() -> ([[f32; 3]; 3], [[f32; 3]; 3], [f32; 3]) {
    let m = REC2020.to_space(&PROPHOTO);
    let mi = m.inverse().unwrap_or_else(|| PROPHOTO.to_space(&REC2020));
    (m.to_f32(), mi.to_f32(), PROPHOTO.luma().map(|v| v as f32))
}

impl FinishParams {
    /// Parameters for a `w × h` render into `space`; `ev`/`air_pre` as in [`crate::Prepared`];
    /// `px_per_src`: output px per source px ([`crate::Plan::px_per_src`]).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        s: &DevelopSettings,
        frame: &Frame,
        info: &SourceInfo,
        w: usize,
        h: usize,
        px_per_long: f64,
        px_per_src: f64,
        air_pre: f32,
        space: OutputSpace,
    ) -> FinishParams {
        let effects = s.section_enabled("effects");
        let (clar, tex, dehaze) = if effects {
            ((s.effects.clarity / 100.0) as f32, (s.effects.texture / 100.0) as f32, (s.effects.dehaze / 100.0) as f32)
        } else {
            (0.0, 0.0, 0.0)
        };
        let ev = s.light.exposure as f32;
        let gain = 2f32.powf(ev);
        let grain = (s.grain.amount > 0.0 && effects).then(|| {
            let cell = (0.0006 + (s.grain.size / 100.0) as f32 * 0.0024) * px_per_long as f32;
            ((s.grain.amount / 100.0) as f32 * 0.13, cell.max(0.6), (s.grain.roughness / 100.0) as f32, s.grain.seed)
        });
        let calibration = s.section_enabled("calibration");
        let (curve_in, curve_out, curve_luma) = curve_space();
        let lr_tone = lr_tone(info);
        let lr_hs = if lr_tone { crate::tone::LrHs::new(s.light.highlights, s.light.shadows) } else { None };
        let tone = if let Some(curve) = info.camera_tone.as_ref().filter(|_| info.raw) {
            // Highlights / Shadows run before Contrast, as in Lightroom
            ToneMap::camera_split(curve, s.light.exposure, s.light.contrast, s.light.whites, s.light.blacks, lr_hs.is_some())
        } else if info.raw {
            ToneMap::new(s.light.contrast, s.light.whites, s.light.blacks)
        } else {
            ToneMap::display(s.light.contrast, s.light.whites, s.light.blacks)
        };
        FinishParams {
            calib: if calibration { crate::colorops::calibration_matrix(&s.calibration) } else { None },
            shadow_tint: if calibration { (s.calibration.shadows_tint / 100.0) as f32 } else { 0.0 },
            tone,
            lut: crate::lut::get(&s.profile.id).map(|l| (l, (s.profile.amount / 100.0).clamp(0.0, 2.0) as f32)),
            ops: ColorOps::new(s),
            curves: curve_luts(&s.curve),
            curve_in,
            curve_out,
            curve_luma,
            refine_sat: (s.curve.refine_saturation / 100.0).clamp(0.0, 1.0) as f32,
            vig: if effects { vignette(s) } else { None },
            to_out: space.from_working(),
            out_luma: space.luma(),
            out_trc: space.trc(),
            proof: None,
            hl: if lr_tone { 0.0 } else { (s.light.highlights / 100.0) as f32 },
            sh: if lr_tone { 0.0 } else { (s.light.shadows / 100.0) as f32 },
            lr_hs,
            clar,
            tex,
            dehaze,
            sharpen: Sharpen::new(s, info.baseline_sharpness, px_per_src),
            air: air_pre * gain,
            air_pre,
            gain,
            ev,
            grain,
            out_to_norm: frame.out_to_norm(w, h),
            ow: frame.ow,
            oh: frame.oh,
            w,
            h,
            px_per_long,
        }
    }
}

/// Whether the source's tone runs as Lightroom's on a DNG profile tone curve (its base operator,
/// Contrast / Blacks / Whites in the tone map, Highlights / Shadows after it; see [`crate::tone`]
/// and [`crate::tone::LrHs`]).
pub fn lr_tone(info: &SourceInfo) -> bool {
    info.raw && info.camera_tone.is_some_and(|c| c.is_per_channel())
}

pub(crate) fn finish(p: &Prepared, s: &DevelopSettings, frame: &Frame, info: &SourceInfo, space: OutputSpace, proof: Option<crate::Proof>) -> Rgba8 {
    let (w, h) = (p.img.width, p.img.height);
    let mut fp = FinishParams::new(s, frame, info, w, h, p.px_per_long, p.px_per_src, p.air, space);
    fp.proof = proof.map(|pr| pr.params(space));
    let trc = fp.out_trc;
    let data = finish_with(p, &fp, false, |e| match trc {
        OutputTrc::Srgb => [enc(e[0]), enc(e[1]), enc(e[2]), 255],
        t => {
            let x = e.map(|v| enc(t.encode(lightcraft_color::transfer::srgb_to_linear(v.clamp(0.0, 1.0)))));
            [x[0], x[1], x[2], 255]
        }
    });
    Rgba8 { width: w, height: h, data }
}

/// [`finish`] into 16-bit display-encoded or 32-bit float linear samples (output primaries).
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_deep(
    p: &Prepared,
    s: &DevelopSettings,
    frame: &Frame,
    info: &SourceInfo,
    space: OutputSpace,
    depth: OutputDepth,
    proof: Option<crate::Proof>,
) -> DeepImage {
    use lightcraft_color::transfer::srgb_to_linear;
    let (w, h) = (p.img.width, p.img.height);
    let mut fp = FinishParams::new(s, frame, info, w, h, p.px_per_long, p.px_per_src, p.air, space);
    fp.proof = proof.map(|pr| pr.params(space));
    let trc = fp.out_trc;
    let samples = match depth {
        OutputDepth::F32Linear => {
            let v = finish_with(p, &fp, true, |e| e.map(|v| srgb_to_linear(v.clamp(0.0, 1.0))));
            DeepSamples::F32(v.into_flattened())
        }
        _ => {
            let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            let v = finish_with(p, &fp, true, |e| match trc {
                OutputTrc::Srgb => e.map(q),
                t => e.map(|v| q(t.encode(srgb_to_linear(v.clamp(0.0, 1.0))))),
            });
            DeepSamples::U16(v.into_flattened())
        }
    };
    DeepImage { width: w, height: h, space, samples }
}

/// The per-pixel stage: every output pixel's colour in the output primaries, encoded with the sRGB
/// curve (tone curves and grain applied; not clamped), handed to `store` for the final encoding.
/// `exact`: encode with the exact sRGB curve instead of the (8/10-bit accurate) table.
pub(crate) fn finish_with<T: Copy + Default + Send>(
    p: &Prepared,
    fp: &FinishParams,
    exact: bool,
    store: impl Fn([f32; 3]) -> T + Sync + Send,
) -> Vec<T> {
    let (w, h) = (p.img.width, p.img.height);
    let p_lut = fp.lut.clone();
    let FinishParams { tone, ops, curves, vig, to_out, out_luma, hl, sh, clar, tex, dehaze, air, air_pre, gain, ev, grain, .. } = fp;
    let (hl, sh, clar, tex, dehaze, air, air_pre, gain, ev) = (*hl, *sh, *clar, *tex, *dehaze, *air, *air_pre, *gain, *ev);
    let terms: Vec<[f32; MASK_TERMS]> = p.masks.iter().map(|m| mask_terms(&m.adjust)).collect();
    let out_to_norm = fp.out_to_norm;
    let long = fp.ow.max(fp.oh);

    let srgb = srgb_lut();
    let enc_lut = (!exact).then_some(srgb);
    // a pixel's finished colour (display-linear Rec.2020, graded) and its local Sharpness
    let colour = |x: usize, y: usize| -> ([f32; 3], f32) {
        {
            let i = y * w + x;
            let raw = p.img.data[i];
            let mut c = if gain == 1.0 { raw } else { raw.map(|v| v * gain) };
            let l_pre = p.log_l.data[i];
            let l0 = l_pre + ev;

            // --- local (mask) contributions: alpha-weighted sums of the masks' terms
            let mut l = [0.0f32; MASK_SUMS];
            let mut tint_col: Option<([f32; 3], f32)> = None;
            for (m, t) in p.masks.iter().zip(&terms) {
                let a = m.alpha.data[i];
                if a <= 0.0 {
                    continue;
                }
                for k in 0..MASK_SUMS {
                    l[k] += a * t[k];
                }
                if t[MASK_SUMS] > 0.0 {
                    tint_col = Some(([t[MASK_SUMS + 1], t[MASK_SUMS + 2], 0.0], a * t[MASK_SUMS + 3]));
                }
            }
            let [l_exp, l_temp, l_tint, l_con, l_hl, l_sh, l_wh, l_bl, l_tex, l_clar, l_dehaze, l_sat, l_hue, l_sharp, l_noise, l_moire, l_defringe] =
                l;

            // --- local Moiré (and the colour part of Noise): chromaticity towards its blur
            let mt = (l_moire + 0.5 * l_noise.max(0.0)).clamp(-1.0, 1.0);
            if mt != 0.0
                && let Some(cb) = &p.chroma_blur
            {
                let (y, ch0, cb) = (luminance_2020(c), crate::masks::chromaticity(raw), cb.data[i]);
                c = std::array::from_fn(|k| ((ch0[k] + (cb[k] - ch0[k]) * mt) * y).max(0.0));
            }
            // --- local Defringe: desaturate purple / green fringes along edges
            let df = l_defringe.clamp(0.0, 1.0);
            if df > 0.0
                && let Some(b) = &p.texture_blur
            {
                let k = df * defringe_weight(c, p.log_l.data[i] - b.data[i]);
                if k > 0.0 {
                    let y = luminance_2020(c);
                    c = c.map(|v| v + (y - v) * k);
                }
            }

            // --- dehaze (scene linear)
            let dz = dehaze + l_dehaze;
            if dz != 0.0
                && let Some(dark) = &p.dark
            {
                let d = (dark.data[i] / air_pre).clamp(0.0, 1.0);
                if dz > 0.0 {
                    let t = (1.0 - 0.95 * dz.min(1.0) * d).max(0.12);
                    c = c.map(|v| ((v - air * (1.0 - t)) / t).max(0.0));
                } else {
                    let k = (-dz).min(1.0) * 0.7 * (0.35 + 0.65 * d);
                    c = c.map(|v| v + (air * 0.9 - v) * k);
                }
            }

            // --- local exposure / temp / tint
            if l_exp != 0.0 {
                let g = l_exp.exp2();
                c = c.map(|v| v * g);
            }
            if l_temp != 0.0 || l_tint != 0.0 {
                let y0 = luminance_2020(c);
                c = [c[0] * (1.0 + 0.3 * l_temp), c[1] * (1.0 - 0.22 * l_tint), c[2] * (1.0 - 0.3 * l_temp).max(0.0)];
                let y1 = luminance_2020(c).max(1e-9);
                c = c.map(|v| v * y0 / y1);
            }

            // --- local tone in log luminance
            let l1 = if dz != 0.0 || l_exp != 0.0 { log_lum(c) } else { l0 };
            let shift = l1 - l0;
            let base = p.base.data[i] + ev + shift;
            let mut delta = 0.0f32;
            let (hh, ss) = (hl + l_hl, sh + l_sh);
            if hh != 0.0 || ss != 0.0 {
                let ws = 1.0 - smooth(-4.8, 0.3, base);
                let wh = smooth(-1.0, 2.8, base);
                delta += ss * 1.7 * ws * ws.sqrt() + hh * 1.7 * wh;
            }
            if l_wh != 0.0 {
                delta += l_wh * 0.8 * smooth(0.5, 3.0, l1);
            }
            if l_bl != 0.0 {
                delta += l_bl * 0.8 * (1.0 - smooth(-6.0, -1.5, l1));
            }
            if l_con != 0.0 {
                delta += l_con * 0.14 * l1.clamp(-6.0, 4.0);
            }
            let cl = clar + l_clar;
            if cl != 0.0
                && let Some(b) = &p.clarity_blur
            {
                let det = (l_pre - b.data[i]).clamp(-2.5, 2.5);
                let mid = (-(base / 3.2).powi(2)).exp();
                delta += cl * 0.85 * det * (0.35 + 0.65 * mid);
            }
            let tx = tex + l_tex;
            if tx != 0.0
                && let Some(b) = &p.texture_blur
            {
                let det = l_pre - b.data[i];
                let tame = 1.0 - 0.6 * smooth(0.4, 1.6, det.abs());
                delta += tx * 1.1 * det.clamp(-1.0, 1.0) * tame;
            }
            // local Noise: smooth (or, negative, boost) small-amplitude detail, keep edges
            if l_noise != 0.0
                && let Some(b) = &p.texture_blur
            {
                let det = l_pre - b.data[i];
                delta -= l_noise.clamp(-1.0, 1.0) * 0.9 * det * (1.0 - smooth(0.1, 0.5, det.abs()));
            }
            if delta != 0.0 {
                let g = delta.exp2();
                c = c.map(|v| v * g);
            }

            // --- post-crop vignette, Highlight / Colour Priority: an exposure change before the
            // tone map (see [`VIG_STRENGTH`]); Highlights spares the bright parts of a darkening one
            if let Some(v) = vig
                && v.style != VignetteStyle::PaintOverlay
            {
                let t = v.falloff(x, y, w, h);
                if t > 0.0 {
                    let mut e = v.strength * t;
                    if e < 0.0 && v.style == VignetteStyle::HighlightPriority && v.highlights > 0.0 {
                        e *= 1.0 - v.highlights * smooth(0.4, 1.0, tone.apply(luminance_2020(c)).clamp(0.0, 1.0));
                    }
                    let g = e.exp();
                    c = c.map(|q| q * g);
                }
            }

            // --- calibration (scene linear, before the tone map)
            if fp.calib.is_some() || fp.shadow_tint != 0.0 {
                c = crate::colorops::calibrate(c, fp.calib.as_ref(), fp.shadow_tint);
            }

            // --- tone map: on luminance with highlight desaturation, or (a DNG profile tone
            // curve) per channel, hue-preserving. Highlights / Shadows as Lightroom applies them
            // on a DNG profile tone curve: a luminance gain from the pixel's and its
            // neighbourhood's display luminance, before Whites / Blacks (inside the map when
            // those are set) and Contrast (Lightroom's order)
            let hs = fp.lr_hs.as_ref().map(|lr| (lr, tone.apply_context(crate::tone::GREY * base.exp2()).max(1e-6).log2()));
            let inside = hs.is_some() && tone.hs_inside();
            let mut d = if tone.per_channel() {
                match hs.filter(|_| inside) {
                    Some((lr, ctx)) => tone.apply_rgb_hs(c, |y| lr.gain(y.max(1e-6).log2(), ctx)),
                    None => tone.apply_rgb(c),
                }
            } else {
                let yl = luminance_2020(c);
                let o = tone.apply(yl);
                let mut d = if yl > 1e-9 { c.map(|v| v * o / yl) } else { [0.0; 3] };
                let k = tone.chroma_scale(o);
                if k != 1.0 {
                    d = d.map(|v| o + (v - o) * k);
                }
                let mx = d[0].max(d[1]).max(d[2]);
                if mx > 1.0 {
                    let t = ((mx - 1.0) / (mx - o).max(1e-6)).clamp(0.0, 1.0);
                    d = d.map(|v| v + (o - v) * t);
                }
                d
            };
            if let Some((lr, ctx)) = hs {
                if !inside {
                    let k = lr.gain(luminance_2020(d).max(1e-6).log2(), ctx);
                    if k != 0.0 {
                        let g = k.exp2();
                        d = d.map(|v| v * g);
                    }
                }
                d = tone.apply_contrast_rgb(d);
            }

            // --- colour (grading follows the tone curves, below)
            d = ops.apply(d, l_sat, l_hue);
            if let Some((dir, amt)) = tint_col {
                let lab = lightcraft_color::perceptual::oklab_from_2020(d);
                d = lightcraft_color::perceptual::oklab_to_2020([lab[0], lab[1] + dir[0] * 0.08 * amt, lab[2] + dir[1] * 0.08 * amt]);
            }

            // --- Paint Overlay vignette (display linear, post-crop): towards black or white
            if let Some(v) = vig
                && v.style == VignetteStyle::PaintOverlay
            {
                let t = v.falloff(x, y, w, h);
                if t > 0.0 {
                    if v.amount < 0.0 {
                        d = d.map(|c| c * (1.0 + v.amount * t));
                    } else {
                        d = d.map(|c| c + (1.0 - c) * v.amount * t * 0.85);
                    }
                }
            }

            // --- tone curves, in the fixed curve space (see the module docs), then colour grading
            if let Some(l) = curves {
                d = apply_curves(d, l, fp, enc_lut);
            }
            d = ops.grade(d);
            (d, l_sharp)
        }
    };
    // gamut map, encode, grain, LUT profile
    let emit = |d: [f32; 3], x: usize, y: usize| -> T {
        {
            // --- gamut map to the output space (desaturate towards luminance until in range);
            // soft proofing maps into the proof space first and shows that in the output space
            let mut warn = None;
            let mut r = match &fp.proof {
                Some(pp) => {
                    let q0 = mul3(&pp.to_proof, d);
                    let (q, t) = gamut_map(q0, pp.luma);
                    if pp.dest_warning && out_of_gamut(q0, t) {
                        warn = Some(crate::output::PROOF_DEST_WARNING);
                    }
                    mul3(&pp.proof_to_out, q)
                }
                None => mul3(to_out, d),
            };
            let (mapped, t) = gamut_map(r, *out_luma);
            if fp.proof.is_some_and(|pp| pp.display_warning) && warn.is_none() && out_of_gamut(r, t) {
                warn = Some(crate::output::PROOF_DISPLAY_WARNING);
            }
            r = mapped;

            // --- encode, grain
            let mut e = if exact { r.map(|v| linear_to_srgb(v.clamp(0.0, 1.0))) } else { r.map(|v| encode_srgb(srgb, v)) };
            if let Some((amt, cell, rough, seed)) = *grain {
                let n = out_to_norm.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                let (gx, gy) = ((n.x * fp.ow) as f32 / long as f32, (n.y * fp.oh) as f32 / long as f32);
                let sc = fp.px_per_long as f32 / cell;
                let mut g = grain_noise(gx * sc, gy * sc, seed);
                g = g * (1.0 - rough * 0.5) + grain_noise(gx * sc * 2.3, gy * sc * 2.3, seed ^ 0x55) * rough * 0.7;
                let lum = 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2];
                let k = amt * g * (0.35 + 2.6 * lum * (1.0 - lum));
                e = e.map(|v| v + k);
            }
            let e = match &p_lut {
                Some((l, k)) => {
                    let m = l.apply(e.map(|v| v.clamp(0.0, 1.0)));
                    [e[0] + (m[0] - e[0]) * k, e[1] + (m[1] - e[1]) * k, e[2] + (m[2] - e[2]) * k]
                }
                None => e,
            };
            store(warn.unwrap_or(e))
        }
    };
    let mut out = vec![T::default(); w * h];
    let Some(shp) = &fp.sharpen else {
        for_rows(&mut out, w, |y, row| {
            for (x, px) in row.iter_mut().enumerate() {
                *px = emit(colour(x, y).0, x, y);
            }
        });
        return out;
    };
    // sharpening: every pixel's colour, then the blur of their luminance, then the gain
    let mut mid = vec![([0.0f32; 3], 0.0f32); w * h];
    for_rows(&mut mid, w, |y, row| {
        for (x, px) in row.iter_mut().enumerate() {
            *px = colour(x, y);
        }
    });
    let lum = lightcraft_raster::Plane { width: w, height: h, data: mid.iter().map(|(d, _)| luminance_2020(*d).max(0.0)).collect() };
    let blur = crate::local::sharpen_blur(&lum, shp.sigma);
    let log_blur = |x: usize, y: usize| blur.data[y.min(h - 1) * w + x.min(w - 1)].max(1e-9).log2();
    for_rows(&mut out, w, |y, row| {
        for (x, px) in row.iter_mut().enumerate() {
            let i = y * w + x;
            let (d, local) = mid[i];
            let grad = if shp.mask > 0.0 {
                let gx = log_blur(x + 1, y) - log_blur(x.saturating_sub(1), y);
                let gy = log_blur(x, y + 1) - log_blur(x, y.saturating_sub(1));
                0.5 * gx.hypot(gy)
            } else {
                0.0
            };
            let g = shp.gain(lum.data[i], blur.data[i], local, grad);
            *px = emit(if g == 1.0 { d } else { d.map(|v| v * g) }, x, y);
        }
    });
    out
}

/// A colour needing more desaturation than this (scale < `GAMUT_WARN`) to fit is out of gamut for
/// the gamut warnings (tolerates rounding at the gamut boundary).
const GAMUT_WARN: f32 = 0.995;

/// For the gamut warnings: `r` needed desaturating by `t` to fit, and isn't simply a neutral
/// beyond white or below black (that's clipping, which the clipping warnings show).
#[inline]
fn out_of_gamut(r: [f32; 3], t: f32) -> bool {
    let (lo, hi) = (r[0].min(r[1]).min(r[2]), r[0].max(r[1]).max(r[2]));
    t < GAMUT_WARN && (hi - lo) > 0.02 * hi.abs().max(1e-3) && hi - lo > 1e-3 && (lo < -1e-3 || (hi > 1.0 && lo < 1.0))
}

#[inline]
fn mul3(m: &[[f32; 3]; 3], d: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * d[0] + m[0][1] * d[1] + m[0][2] * d[2],
        m[1][0] * d[0] + m[1][1] * d[1] + m[1][2] * d[2],
        m[2][0] * d[0] + m[2][1] * d[1] + m[2][2] * d[2],
    ]
}

/// Desaturate linear `r` towards its luminance (weights `luma`) until every channel is in 0..1;
/// returns the mapped colour and the chroma scale used (1 = already in gamut).
#[inline]
pub fn gamut_map(r: [f32; 3], luma: [f32; 3]) -> ([f32; 3], f32) {
    let yy = (luma[0] * r[0] + luma[1] * r[1] + luma[2] * r[2]).clamp(0.0, 1.0);
    let mut t = 1.0f32;
    for c in r {
        if c < 0.0 {
            t = t.min(yy / (yy - c).max(1e-9));
        } else if c > 1.0 {
            t = t.min((1.0 - yy) / (c - yy).max(1e-9));
        }
    }
    if t < 1.0 { (r.map(|c| yy + (c - yy) * t), t) } else { (r, 1.0) }
}

/// Local Defringe weight of a scene-linear colour `c` whose log luminance differs by `det` from
/// its fine blur: high on strong edges with a purple or green cast.
#[inline]
pub fn defringe_weight(c: [f32; 3], det: f32) -> f32 {
    let y = luminance_2020(c).max(1e-6);
    let purple = (c[0].min(c[2]) - c[1]) / y;
    let green = (c[1] - c[0].max(c[2])) / y;
    smooth(0.04, 0.3, det.abs()) * smooth(0.02, 0.2, purple).max(smooth(0.02, 0.2, green))
}

// ---- Sharpening, fitted to Lightroom Classic 15.6 renders of an Apple ProRAW at full size: Amount
// 10…150 at Radius 1.0, Radius 0.5…3 and Detail 0…100 at Amount 50, Amount 25…150 at Radius 1.4,
// Masking 50 / 100 (mean ΔE00 to Lightroom's 0.05–0.6 up to Amount 100, unsharpened 0.09–1.97).

/// Amount knots of the sharpening tables: the slider on a file with `BaselineSharpness`
/// [`SHARPEN_BS_REF`] (other files scale the slider by their own BaselineSharpness / that).
pub const SHARPEN_AMT: [f32; 8] = [0.0, 10.0, 25.0, 40.0, 50.0, 75.0, 98.0, 150.0];
/// The `BaselineSharpness` of the file the tables were measured on.
const SHARPEN_BS_REF: f32 = 1.5;
/// Per amount knot at Radius 1.0, Detail 25: gain `k`, limit `L` (log2) and blur σ (source px).
const SHARPEN_K: [f32; 8] = [0.0, 0.395, 1.374, 2.915, 4.491, 12.202, 28.011, 144.696];
const SHARPEN_LIMIT: [f32; 8] = [0.16, 0.16, 0.34, 0.48, 0.556, 0.664, 0.636, 0.494];
const SHARPEN_SIGMA: [f32; 8] = [1.143, 1.143, 0.971, 0.836, 0.754, 0.6, 0.53, 0.459];
/// Radius knots, and (k, L, σ) at each relative to Radius 1.0, measured at Amount 50.
const SHARPEN_RADII: [f32; 5] = [0.5, 1.0, 1.4, 2.0, 3.0];
const SHARPEN_RADIUS_F: [[f32; 3]; 5] = [[2.972, 0.8, 0.561], [1.0, 1.0, 1.0], [0.75, 1.041, 1.39], [0.481, 1.003, 2.039], [0.27, 0.919, 3.294]];
/// The Radius factors of k and σ act as these powers at each amount knot (Radius matters more for
/// k as Amount grows: at Radius 1.4 k is 0.87× its Radius 1.0 value at Amount 25, 0.36× at 150).
const SHARPEN_RADIUS_POW: [[f32; 2]; 8] = [[0.3, 0.65], [0.3, 0.65], [0.49, 0.774], [0.8, 0.91], [1.0, 1.0], [1.9, 1.1], [2.75, 1.19], [3.53, 1.216]];
/// Detail knots, and (k, L, σ) at each relative to Detail 25, measured at Amount 50, Radius 1.4.
const SHARPEN_DETAILS: [f32; 4] = [0.0, 25.0, 50.0, 100.0];
const SHARPEN_DETAIL_F: [[f32; 3]; 4] = [[0.245, 0.46, 1.581], [1.0, 1.0, 1.0], [1.8, 1.18, 0.839], [3.474, 1.352, 0.724]];

/// Piecewise-linear `ys` over ascending `xs` at `x`, clamped to the ends.
fn interp<const N: usize>(xs: &[f32; N], ys: impl Fn(usize) -> f32, x: f32) -> f32 {
    let i = xs.iter().rposition(|k| *k <= x).unwrap_or(0).min(N.saturating_sub(2));
    let (x0, x1) = (xs[i], xs[(i + 1).min(N - 1)]);
    let t = if x1 > x0 { ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { 0.0 };
    ys(i) + (ys((i + 1).min(N - 1)) - ys(i)) * t
}

/// Lightroom's capture sharpening, on the finished image (after the tone curves and grading): a
/// pixel's display-linear luminance `Y` and its Gaussian blur `B` give a log2 gain
/// `L·tanh(k·(Y / B − 1) / L)` that scales the colour. Masking keeps it to edges: it fades in as
/// the blur's log2 gradient (per source pixel) goes from `SHARPEN_MASK_AT·Masking` over
/// `SHARPEN_MASK_WIDTH` (reached at Masking 25, from nothing at 0).
#[derive(Clone, Debug, PartialEq)]
pub struct Sharpen {
    /// σ of the luminance blur, output px.
    pub sigma: f32,
    /// k and L at each [`SHARPEN_AMT`] knot, Radius and Detail applied.
    pub k: [f32; 8],
    pub limit: [f32; 8],
    /// The amount (table units), and per unit of local Sharpness.
    pub amount: f32,
    pub local: f32,
    /// Masking (0..1), and output px per source px (the mask's gradient is per source pixel).
    pub mask: f32,
    pub px_per_src: f32,
}

/// Masking: the log2 gradient (per source px) at which sharpening starts, per unit of Masking, and
/// the width it fades in over (Masking 50 / 100 within ΔE00 0.18 / 0.10 of Lightroom; without the
/// mask 0.53 / 0.63).
pub const SHARPEN_MASK_AT: f32 = 0.0292;
pub const SHARPEN_MASK_WIDTH: f32 = 0.4667;

impl Sharpen {
    /// Below this σ (output px) the luminance blur is the identity to < 0.1 %: no sharpening.
    pub const MIN_SIGMA: f32 = crate::local::SHARPEN_MIN_SIGMA;

    /// Sharpening for `s` on a file with `BaselineSharpness` `bs`, rendered at `px_per_src` output
    /// px per source px; `None` when it does nothing.
    pub fn new(s: &DevelopSettings, bs: f32, px_per_src: f64) -> Option<Sharpen> {
        let scale = if bs.is_finite() { bs.clamp(0.0, 4.0) / SHARPEN_BS_REF } else { 1.0 / SHARPEN_BS_REF };
        let amount = (s.detail.sharpen_amount as f32).clamp(0.0, 150.0) * scale;
        let local = s.masks.iter().any(|m| m.adjust.sharpness != 0.0);
        if amount <= 0.0 && !local {
            return None;
        }
        let radius = if s.detail.sharpen_radius.is_finite() { (s.detail.sharpen_radius as f32).clamp(0.5, 3.0) } else { 1.0 };
        let detail = if s.detail.sharpen_detail.is_finite() { (s.detail.sharpen_detail as f32).clamp(0.0, 100.0) } else { 25.0 };
        // the factors' logs, linear between the radius knots
        let rf = |j: usize| interp(&SHARPEN_RADII, |i| SHARPEN_RADIUS_F[i][j].ln(), radius);
        let df = |j: usize| interp(&SHARPEN_DETAILS, |i| SHARPEN_DETAIL_F[i][j], detail);
        let (rk, rl, rs) = (rf(0), rf(1), rf(2));
        let k = std::array::from_fn(|i| SHARPEN_K[i] * (rk * SHARPEN_RADIUS_POW[i][0]).exp() * df(0));
        let limit = std::array::from_fn(|i| SHARPEN_LIMIT[i] * rl.exp() * df(1));
        // one blur for the image: the global amount's σ (a local-only sharpening: Amount 50's)
        let a = if amount > 0.0 { amount.min(150.0) } else { 50.0 };
        let sigma_src = interp(&SHARPEN_AMT, |i| SHARPEN_SIGMA[i] * (rs * SHARPEN_RADIUS_POW[i][1]).exp(), a) * df(2);
        let sigma = sigma_src * px_per_src as f32;
        (sigma >= Self::MIN_SIGMA && sigma.is_finite()).then_some(Sharpen {
            sigma,
            k,
            limit,
            amount,
            // local Sharpness ±100 adds or takes ±90 (table units, scaled like the slider)
            local: 90.0 * scale,
            mask: (s.detail.sharpen_masking as f32 / 100.0).clamp(0.0, 1.0),
            px_per_src: px_per_src as f32,
        })
    }

    /// The gain for a pixel of luminance `y` whose blur is `blur`, with local Sharpness `local`
    /// (−1..1) and the blur's log2 gradient `grad` (per output px; used by Masking).
    #[inline]
    pub fn gain(&self, y: f32, blur: f32, local: f32, grad: f32) -> f32 {
        let a = self.amount + local * self.local;
        if a == 0.0 || y.is_nan() || y <= 0.0 || blur.is_nan() || blur <= 0.0 {
            return 1.0;
        }
        let at = a.abs().min(150.0);
        let k = interp(&SHARPEN_AMT, |i| self.k[i], at) * a.signum();
        let lim = interp(&SHARPEN_AMT, |i| self.limit[i], at).max(1e-3);
        let mut m = lim * (k * (y / blur - 1.0) / lim).tanh();
        if self.mask > 0.0 {
            let e0 = SHARPEN_MASK_AT * self.mask;
            let width = SHARPEN_MASK_WIDTH * (self.mask * 4.0).min(1.0);
            m *= smooth(e0, e0 + width, grad * self.px_per_src.max(1e-6));
        }
        m.exp2()
    }
}

/// The tone curves on display-linear Rec.2020 `d`, in the curve space (see the module docs), on
/// the 0..1 part of each curve-space channel (`enc`: the encoding table, else the exact curve).
/// The parametric ∘ master curve is hue-preserving, as Lightroom applies it: the largest and
/// smallest channel go through it and the middle one keeps its relative position between them
/// (linear). The red / green / blue curves then act per channel. The part outside 0..1 is added
/// back and the result returned to Rec.2020. Refine Saturation adjusts the curved colour's
/// saturation and then restores the curve's (linear) luminance, so it changes colour, not tone.
#[inline]
pub fn apply_curves(d: [f32; 3], l: &[Lut1; 4], fp: &FinishParams, enc: Option<&[f32; SRGB_LUT_N + 1]>) -> [f32; 3] {
    let q = mul3(&fp.curve_in, d);
    let qc = q.map(|v| v.clamp(0.0, 1.0));
    let encode = |v: f32| match enc {
        Some(t) => encode_srgb(t, v),
        None => linear_to_srgb(v),
    };
    let dec = srgb_decode_lut();
    let e0 = qc.map(encode);
    let (mx, mn) = (qc[0].max(qc[1]).max(qc[2]), qc[0].min(qc[1]).min(qc[2]));
    let (hi, lo) = (decode_srgb(dec, l[0].eval(encode(mx))), decode_srgb(dec, l[0].eval(encode(mn))));
    let b = if mx - mn > 1e-9 { qc.map(|v| lo + (hi - lo) * (v - mn) / (mx - mn)) } else { [hi; 3] };
    let e: [f32; 3] = std::array::from_fn(|k| l[k + 1].eval(encode(b[k])));
    let mut lin = e.map(|v| decode_srgb(dec, v));
    if fp.refine_sat < 1.0 {
        let y = |c: [f32; 3]| fp.curve_luma[0] * c[0] + fp.curve_luma[1] * c[1] + fp.curve_luma[2] * c[2];
        let r = refine_saturation(e0, e, fp.refine_sat, fp.curve_luma).map(|v| decode_srgb(dec, v));
        let (y0, y1) = (y(lin), y(r));
        lin = if y1 > 1e-6 { r.map(|v| v * y0 / y1) } else { r };
    }
    let q1: [f32; 3] = std::array::from_fn(|k| lin[k] + (q[k] - qc[k]));
    mul3(&fp.curve_out, q1)
}

/// Refine Saturation: scale the curved colour's chroma (around its luma with weights `luma`,
/// encoded values) so its saturation (chroma / luma) moves from the curve's towards the
/// pre-curve one: the ratio is `(s_curve / s_before)^refine`, so 1 keeps the curve, 0 restores
/// the original saturation.
#[inline]
pub fn refine_saturation(before: [f32; 3], after: [f32; 3], refine: f32, luma: [f32; 3]) -> [f32; 3] {
    let luma = |e: [f32; 3]| luma[0] * e[0] + luma[1] * e[1] + luma[2] * e[2];
    let chroma = |e: [f32; 3]| e[0].max(e[1]).max(e[2]) - e[0].min(e[1]).min(e[2]);
    let (y0, y1) = (luma(before), luma(after));
    let s0 = chroma(before) / y0.max(1e-4);
    let s1 = chroma(after) / y1.max(1e-4);
    if s1 <= 1e-6 || s0 <= 1e-6 {
        return after;
    }
    let k = (s0 / s1).powf(1.0 - refine).clamp(0.0, 4.0);
    after.map(|v| y1 + (v - y1) * k)
}

pub const SRGB_LUT_N: usize = 4096;

/// Linear → sRGB-encoded table (`SRGB_LUT_N + 1` entries over 0..1), interpolated linearly.
pub fn srgb_lut() -> &'static [f32; SRGB_LUT_N + 1] {
    static LUT: std::sync::OnceLock<Box<[f32; SRGB_LUT_N + 1]>> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = Box::new([0.0f32; SRGB_LUT_N + 1]);
        for (i, v) in t.iter_mut().enumerate() {
            *v = linear_to_srgb(i as f32 / SRGB_LUT_N as f32);
        }
        t
    })
}

/// Linear → sRGB-encoded (clamped to 0..1) by an interpolated table: within 2e-5 of the exact
/// curve (≪ one 8-bit or 10-bit step), several times faster than `powf`.
#[inline]
fn encode_srgb(lut: &[f32; SRGB_LUT_N + 1], v: f32) -> f32 {
    let f = v.clamp(0.0, 1.0) * SRGB_LUT_N as f32;
    let i = (f as usize).min(SRGB_LUT_N - 1);
    let t = f - i as f32;
    lut[i] + (lut[i + 1] - lut[i]) * t
}

/// sRGB-encoded → linear table (`SRGB_LUT_N + 1` entries over 0..1), interpolated linearly.
static SRGB_DECODE: std::sync::LazyLock<Box<[f32; SRGB_LUT_N + 1]>> = std::sync::LazyLock::new(|| {
    let mut t = Box::new([0.0f32; SRGB_LUT_N + 1]);
    for (i, v) in t.iter_mut().enumerate() {
        *v = srgb_to_linear(i as f32 / SRGB_LUT_N as f32);
    }
    t
});

pub fn srgb_decode_lut() -> &'static [f32; SRGB_LUT_N + 1] {
    &SRGB_DECODE
}

/// sRGB-encoded (clamped to 0..1) → linear by an interpolated table: within 1e-7 of the exact
/// curve.
#[inline]
fn decode_srgb(lut: &[f32; SRGB_LUT_N + 1], v: f32) -> f32 {
    encode_srgb(lut, v)
}

#[inline]
fn enc(v: f32) -> u8 {
    // `v` is already sRGB-encoded; round to 8 bits.
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const BT709: [f32; 3] = [0.2126, 0.7152, 0.0722];

    #[test]
    fn refine_saturation_restores_the_pre_curve_saturation() {
        let before = [0.5, 0.3, 0.2];
        let after = [0.7, 0.35, 0.15]; // a contrasty curve: more saturated
        assert_eq!(refine_saturation(before, after, 1.0, BT709), after);
        let r = refine_saturation(before, after, 0.0, BT709);
        let sat = |e: [f32; 3]| (e[0] - e[2]) / (0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2]);
        assert!((sat(r) - sat(before)).abs() < 1e-4, "{r:?}");
        let half = refine_saturation(before, after, 0.5, BT709);
        assert!(sat(half) > sat(before) && sat(half) < sat(after));
        // luma is kept
        let y = |e: [f32; 3]| 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2];
        assert!((y(r) - y(after)).abs() < 1e-5);
    }

    #[test]
    fn sharpening_follows_lightroom_at_its_measured_settings() {
        // Lightroom's defaults for the ProRAW (BaselineSharpness 1.5): Amount 50, Radius 1.4; its
        // own fit there: k 3.37, L 0.584, σ 1.047 source px
        let mut s = DevelopSettings::default();
        s.detail.sharpen_amount = 50.0;
        s.detail.sharpen_radius = 1.4;
        let sh = Sharpen::new(&s, 1.5, 1.0).unwrap();
        assert!((sh.sigma - 1.047).abs() < 0.01, "{}", sh.sigma);
        let log_gain = |y: f32| sh.gain(y, 1.0, 0.0, 1.0).log2();
        assert!((log_gain(1.001) / 0.001 - 3.37).abs() < 0.05, "{}", log_gain(1.001) / 0.001);
        assert!((log_gain(3.0) - 0.584).abs() < 0.01 && (log_gain(0.2) + 0.584).abs() < 0.01);
        // at half size the blur is half as wide (in output px)
        assert!((Sharpen::new(&s, 1.5, 0.5).unwrap().sigma - sh.sigma / 2.0).abs() < 1e-4);
        // the amount is relative to BaselineSharpness 1.5: Amount 75 on a file without it is the same
        s.detail.sharpen_amount = 75.0;
        let plain = Sharpen::new(&s, 1.0, 1.0).unwrap();
        assert!((plain.sigma - sh.sigma).abs() < 1e-4 && (plain.amount - sh.amount).abs() < 1e-4, "{plain:?} vs {sh:?}");
        // Masking spares flat areas, keeps edges
        s.detail.sharpen_masking = 100.0;
        let masked = Sharpen::new(&s, 1.0, 1.0).unwrap();
        assert_eq!(masked.gain(1.1, 1.0, 0.0, 0.0), 1.0);
        assert_eq!(masked.gain(1.1, 1.0, 0.0, 1.0), sh.gain(1.1, 1.0, 0.0, 1.0));
        // nothing to do without an amount
        s.detail.sharpen_amount = 0.0;
        assert_eq!(Sharpen::new(&s, 1.5, 1.0), None);
    }

    #[test]
    fn srgb_table_matches_the_exact_curve() {
        let lut = srgb_lut();
        let mut worst = 0.0f32;
        for i in 0..=200_000 {
            // dense near black, where the curve bends most
            let v = (i as f32 / 200_000.0).powi(3);
            worst = worst.max((encode_srgb(lut, v) - linear_to_srgb(v)).abs());
        }
        assert!(worst < 2e-5, "{worst}");
        assert_eq!(encode_srgb(lut, -1.0), 0.0);
        assert!((encode_srgb(lut, 2.0) - 1.0).abs() < 1e-6);
    }
}
