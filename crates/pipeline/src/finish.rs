//! The per-pixel stage: everything after the spatial planes are ready, in one parallel pass.

use lcc_color::luminance_2020;
use lcc_color::spline::{Lut1, MonotoneCurve};
use lcc_color::transfer::linear_to_srgb;
use lcc_develop::{DevelopSettings, LocalAdjustments, PointCurves, Process, ToneCurve, VignetteStyle};
use lcc_geom::Point;
use lcc_raster::Rgba8;

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

/// Gain of the sharpening detail term (per unit of `Amount / 150`), in stops of luminance per stop of detail.
pub const SHARPEN_GAIN: f32 = 7.0;

/// Soft-clip limit (stops) of the sharpening detail term for Detail `0..=100`: 0.03 stops (large
/// edge detail is flattened so halos stay small) up to 1.5 stops (practically linear).
pub fn sharpen_limit(detail: f64) -> f32 {
    let d = (detail / 100.0).clamp(0.0, 1.0) as f32;
    0.03 * 50f32.powf(d)
}

/// The sharpening boost (stops) for pixel-scale detail `det` (log luminance minus its blur at the
/// sharpen radius): linear for small detail, soft-clipped at `limit`, and gated by the Masking edge
/// mask (`mask` = Masking / 100) so flat areas and noise can be left alone.
#[inline]
pub fn sharpen_term(det: f32, limit: f32, mask: f32) -> f32 {
    let m = if mask > 0.0 { smooth(mask * 0.1, mask * 0.1 + 0.06, det.abs()) } else { 1.0 };
    SHARPEN_GAIN * m * det / (1.0 + det.abs() / limit)
}

/// Whether `s` sharpens with process V4's unsharp mask (older processes boost the Texture band).
pub fn sharpen_v4(s: &DevelopSettings) -> bool {
    s.process.process() >= lcc_develop::Process::V4
}

/// Entries per tone-curve table ([`curve_luts`], [`local_curve_luts`]; the GPU kernel reads tables of this size).
pub const CURVE_LUT_N: usize = 1024;

/// A point curve as a table over `0..1`.
fn point_lut(p: &[Point]) -> Lut1 {
    MonotoneCurve::new(&p.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>()).to_lut(CURVE_LUT_N)
}

/// A mask's local Curve as per-channel tables on encoded values (the RGB curve, then each colour
/// channel's own, as in the Tone Curve), or `None` while every channel is linear. The CPU stage
/// and the GPU kernel both take their tables from here.
pub fn local_curve_luts(c: &PointCurves) -> Option<[Lut1; 3]> {
    if c.is_identity() {
        return None;
    }
    let lut = |p: &[Point]| (!ToneCurve::point_curve_is_identity(p)).then(|| point_lut(p));
    let master = lut(&c.master);
    let per = [&c.red, &c.green, &c.blue];
    Some(std::array::from_fn(|i| match (lut(per[i]), &master) {
        (Some(ch), Some(m)) => ch.compose(m),
        (Some(ch), None) => ch,
        (None, Some(m)) => m.clone(),
        (None, None) => Lut1::identity(CURVE_LUT_N),
    }))
}

/// Curve tables `l` on encoded values `e`; for HDR renders (`hdr`) a value above SDR white continues with
/// slope 1 from the curve's end point.
#[inline]
fn eval_curves(l: &[Lut1; 3], e: [f32; 3], hdr: bool) -> [f32; 3] {
    if hdr {
        std::array::from_fn(|k| if e[k] > 1.0 { l[k].eval(1.0) + (e[k] - 1.0) } else { l[k].eval(e[k]) })
    } else {
        [l[0].eval(e[0]), l[1].eval(e[1]), l[2].eval(e[2])]
    }
}

/// Parametric region curve (encoded domain) composed with the master point curve.
fn curve_luts(c: &ToneCurve) -> Option<[Lut1; 3]> {
    let parametric = c.highlights != 0.0 || c.lights != 0.0 || c.darks != 0.0 || c.shadows != 0.0;
    let master = !ToneCurve::point_curve_is_identity(&c.master);
    let chans = [&c.red, &c.green, &c.blue].map(|p| !ToneCurve::point_curve_is_identity(p));
    if !parametric && !master && !chans.iter().any(|b| *b) {
        return None;
    }
    const N: usize = CURVE_LUT_N;
    let mut base = crate::curve::ParametricCurve::new(c).to_lut();
    let to_pts = |p: &[Point]| p.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>();
    if master {
        base = MonotoneCurve::new(&to_pts(&c.master)).to_lut(N).compose(&base);
    }
    let per = [&c.red, &c.green, &c.blue];
    Some(std::array::from_fn(|i| {
        if chans.get(i).copied().unwrap_or(false) {
            let pts = per.get(i).map(|&p| to_pts(p)).unwrap_or_default();
            MonotoneCurve::new(&pts).to_lut(N).compose(&base)
        } else {
            base.clone()
        }
    }))
}

/// Vignette (post-crop) parameters.
#[derive(Clone, Copy, Debug)]
pub struct Vig {
    pub amount: f32,
    pub start: f32,
    pub width: f32,
    pub aspect_mix: f32,
    pub power: f32,
    pub highlights: f32,
    pub style: VignetteStyle,
}

fn vignette(s: &DevelopSettings) -> Option<Vig> {
    let v = &s.vignette;
    (v.amount != 0.0).then(|| {
        let r = (v.roundness / 100.0) as f32;
        Vig {
            amount: (v.amount / 100.0) as f32,
            start: 0.15 + (v.midpoint / 100.0) as f32 * 0.95,
            width: 0.05 + (v.feather / 100.0) as f32 * 1.1,
            aspect_mix: ((r + 1.0) / 2.0).clamp(0.0, 1.0),
            power: if r >= 0.0 { 2.0 } else { 2.0 + (-r) * 6.0 },
            highlights: (v.highlights / 100.0) as f32,
            style: v.style,
        }
    })
}

/// Smallest grain cell (px) a render draws.
const GRAIN_CELL_MIN: f32 = 0.6;

/// Hash constants of [`grain_noise`] (shared with the GPU kernel).
pub const GRAIN_HASH: [u32; 4] = [0x8da6_b343, 0xd816_3841, 0xcb1a_b31f, 0x5bd1_e995];

#[inline]
fn grain_noise(x: f32, y: f32, seed: u32) -> f32 {
    // A degenerate frame (zero-width crop) maps pixels to NaN/inf coordinates: no grain there.
    if !x.is_finite() || !y.is_finite() {
        return 0.0;
    }
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

/// Where a mask's Whites act when nothing better is known (log2 luminance over middle grey): from
/// nothing at the first to full at the second (the white point is about 2.9). Mirrored in `finish.wgsl`.
pub const LOCAL_WHITES: (f32, f32) = (-0.5, 2.6);
/// Where a mask's Blacks act when nothing better is known: full at the first, nothing from the second on.
pub const LOCAL_BLACKS: (f32, f32) = (-5.5, 1.0);
/// The most a mask's Whites or Blacks at ±100 move a pixel (EV). Mirrored in `finish.wgsl`.
pub const LOCAL_WB_EV: f32 = 1.5;

/// Number of per-mask terms in [`mask_terms`].
pub const MASK_TERMS: usize = MASK_SUMS + 8;

/// Where a mask's own tone range starts in its [`mask_terms`]: Whites from nothing to full, then Blacks
/// from full to nothing (log2 luminance over middle grey; [`crate::mask_tone`] fits them to the tones
/// under the mask).
pub const MASK_TONE: usize = MASK_SUMS + 4;

/// The tone range a mask's Whites / Blacks act on when nothing better is known.
pub const DEFAULT_MASK_TONE: [f32; 4] = [LOCAL_WHITES.0, LOCAL_WHITES.1, LOCAL_BLACKS.0, LOCAL_BLACKS.1];

/// Number of alpha-weighted terms at the start of [`mask_terms`].
pub const MASK_SUMS: usize = 17 + crate::colorops::LOCAL_GRADE_TERMS;

/// Positive dehaze from process V3 (#1237) of scene-linear `c`: `dz` the strength (0..1, with a
/// local adjustment's added), `d` the dark channel over the airlight (0..1), `air` the airlight.
/// The strength eases in (`dz·(0.4 + 0.6·dz)`: +10 acts at about half V1's), the transmission is
/// floored at 0.3 (channel differences grow at most ~3x, so +100 adds no colour casts), and the
/// veil removed never exceeds 90 % of the pixel's darkest channel, so no
/// pixel is pushed to black (V1 clipped shadows next to bright sky from +10). The GPU mirrors it in
/// `finish.wgsl`. Sources: He, Sun, Tang, "Single Image Haze Removal Using Dark Channel Prior"
/// (CVPR 2009), with own easing and veil limit.
#[inline]
pub fn dehaze_v3_pixel(c: [f32; 3], dz: f32, d: f32, air: f32) -> [f32; 3] {
    let k = dz.clamp(0.0, 1.0);
    let k = k * (0.4 + 0.6 * k);
    let t = (1.0 - 0.95 * k * d).max(0.3);
    let veil = (air * (1.0 - t)).min(0.9 * c[0].min(c[1]).min(c[2]).max(0.0));
    c.map(|v| ((v - veil) / t).max(0.0))
}

/// A mask's local adjustments as the per-pixel stage uses them (alpha-weighted sums), in order:
/// exposure, temp, tint, contrast, highlights, shadows, whites, blacks, texture, clarity, dehaze,
/// saturation, hue, sharpness, noise, moiré, defringe, the colour grading
/// ([`crate::colorops::local_grade_terms`]), then the colour overlay: on (0/1), cos and sin of its
/// OkLCh hue, and its strength; then the tone range of its Whites and Blacks ([`MASK_TONE`],
/// [`DEFAULT_MASK_TONE`] here: [`with_tone`] gives it the mask's own).
pub fn mask_terms(j: &LocalAdjustments) -> [f32; MASK_TERMS] {
    let (on, cos, sin, amt) = if j.color_sat > 0.0 {
        let hue = crate::colorops::oklch_hue_of_srgb_hue(j.color_hue);
        (1.0, hue.cos(), hue.sin(), (j.color_sat / 100.0) as f32)
    } else {
        (0.0, 0.0, 0.0, 0.0)
    };
    let g = crate::colorops::local_grade_terms(j);
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
        // degrees → radians (in f64: an old −100..100 value lands on the same f32 angle as before)
        if j.hue.is_finite() { j.hue.clamp(-lcc_develop::LOCAL_HUE_MAX, lcc_develop::LOCAL_HUE_MAX).to_radians() as f32 } else { 0.0 },
        (j.sharpness / 100.0) as f32,
        (j.noise / 100.0) as f32,
        (j.moire / 100.0) as f32,
        (j.defringe / 100.0) as f32,
        g[0],
        g[1],
        g[2],
        g[3],
        g[4],
        g[5],
        g[6],
        g[7],
        g[8],
        g[9],
        g[10],
        g[11],
        on,
        cos,
        sin,
        amt,
        DEFAULT_MASK_TONE[0],
        DEFAULT_MASK_TONE[1],
        DEFAULT_MASK_TONE[2],
        DEFAULT_MASK_TONE[3],
    ]
}

/// `terms` (one per evaluated mask) with the masks' own tone ranges (`tone`, in the same order; left
/// as they are when the two lists don't line up).
pub fn with_tone(terms: &mut [[f32; MASK_TERMS]], tone: &[[f32; 4]]) {
    if terms.len() != tone.len() {
        return;
    }
    for (t, r) in terms.iter_mut().zip(tone) {
        if let Some(dst) = t.get_mut(MASK_TONE..MASK_TONE + 4) {
            dst.copy_from_slice(r);
        }
    }
}

/// Everything the per-pixel stage computes once per render: the CPU loop below and the GPU kernel
/// (`lcc-gpu`) both read their parameters from here, so the two cannot drift apart.
pub struct FinishParams {
    /// A LUT profile and its amount (0..2), applied to the display-encoded colour.
    pub lut: Option<(std::sync::Arc<crate::lut::Lut3d>, f32)>,
    pub tone: ToneMap,
    pub rgb_contrast: Option<crate::tone::RgbContrast>,
    pub ops: ColorOps,
    /// The masks' local Point Color samples, one list per evaluated mask (empty when none has any,
    /// [`crate::colorops::mask_point_ks`]).
    pub local_points: Vec<Vec<crate::colorops::PointK>>,
    /// The masks' local Color Mixers, one per evaluated mask (empty when none has one,
    /// [`crate::colorops::mask_mixer_ks`]); applied before the mask's Point Color.
    pub local_mixers: Vec<Option<crate::colorops::MixerK>>,
    /// Calibration: primaries matrix (row-major, linear Rec.2020) and shadows tint (−1..1).
    pub calib: Option<[[f32; 3]; 3]>,
    pub shadow_tint: f32,
    /// Brightness on the encoded values, right after the encode and before the Tone Curve; `None` while 0. See
    /// [`crate::brightness`].
    pub brightness: Option<crate::brightness::BrightnessK>,
    /// Tone curves (parametric ∘ point, per channel) on encoded values, 1024 entries each.
    pub curves: Option<[Lut1; 3]>,
    /// Levels (beyond Lightroom) on encoded values, after Exposure and right before the Tone Curve; `None` while
    /// neutral. See [`crate::levels`].
    pub levels: Option<crate::levels::LevelsK>,
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
    pub hl: f32,
    pub sh: f32,
    pub clar: f32,
    pub tex: f32,
    pub dehaze: f32,
    /// Dehaze's process-V3 response ([`dehaze_v3_pixel`]) instead of V1's.
    pub dehaze_v3: bool,
    /// Process-V4 sharpening (unsharp mask at the Radius, [`sharpen_term`]) instead of the
    /// Texture-band boost of V1–V3.
    pub sharpen_v4: bool,
    /// Colours above the peak keep their hue and saturation ([`hue_clip`], process V5) instead of
    /// moving toward their luminance.
    pub hue_clip: bool,
    /// 8-bit output is dithered with [`bayer_dither_offset`] (process V8) instead of rounded.
    pub dither: bool,
    pub sharpen: f32,
    pub sharpen_mask: f32,
    /// Soft-clip limit (stops) of the sharpening detail term: small damps halos (Detail 0), large keeps all (100).
    pub sharpen_limit: f32,
    /// 0..1: sharpening fades out as its blur radius drops under half a pixel.
    pub sharpen_fade: f32,
    /// Airlight after and before exposure, exposure gain and EV (see [`crate::Prepared`]).
    pub air: f32,
    pub air_pre: f32,
    pub gain: f32,
    pub ev: f32,
    /// Grain of processes V1–V6: amount, cell size (px), roughness, seed.
    pub grain: Option<(f32, f32, f32, u32)>,
    /// Grain from process V7: clumps of particles ([`crate::grain`]); `grain` is then `None`.
    pub particles: Option<crate::grain::GrainParams>,
    /// Output px → normalized oriented coordinates.
    pub out_to_norm: lcc_geom::Affine,
    pub ow: f64,
    pub oh: f64,
    pub w: usize,
    pub h: usize,
    pub px_per_long: f64,
    /// The window's offset and the whole output's size (pixels): `(0, 0, w, h)` unless windowed.
    pub view: [f32; 4],
    /// Peak linear output relative to SDR white: 1 for SDR renders; `2^max_ev` for an HDR render
    /// ([`OutputDepth::F32Hdr`] with HDR on), which keeps values above 1 instead of clipping them.
    pub peak: f32,
}

impl FinishParams {
    /// Parameters for a `w × h` render into `space`; `ev`/`air_pre` as in [`crate::Prepared`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        s: &DevelopSettings,
        frame: &Frame,
        info: &SourceInfo,
        w: usize,
        h: usize,
        px_per_long: f64,
        air_pre: f32,
        space: OutputSpace,
    ) -> FinishParams {
        let effects = s.section_enabled("effects");
        let (clar, tex, dehaze) = if effects {
            (
                (s.effects.clarity / 100.0) as f32,
                (s.effects.texture / 100.0) as f32 * crate::local::texture_fade(px_per_long),
                (s.effects.dehaze / 100.0) as f32,
            )
        } else {
            (0.0, 0.0, 0.0)
        };
        let ev = s.light.exposure as f32;
        let gain = 2f32.powf(ev);
        // A degenerate frame (zero-width crop, zero scale) has no finite grain mapping: skip grain (CPU and GPU).
        let out_to_norm = frame.out_to_norm(w, h);
        let finite_frame = px_per_long.is_finite()
            && px_per_long > 0.0
            && frame.ow.is_finite()
            && frame.oh.is_finite()
            && frame.ow.max(frame.oh) > 0.0
            && out_to_norm.0.iter().all(|v| v.is_finite());
        let v7 = match s.process.process() {
            Process::V1 | Process::V2 | Process::V3 | Process::V4 | Process::V5 | Process::V6 => false,
            Process::V7 | Process::V8 | Process::V9 => true,
        };
        let particles = if effects && finite_frame && v7 { crate::grain::GrainParams::new(&s.grain, px_per_long) } else { None };
        let grain = (s.grain.amount > 0.0 && effects && finite_frame && !v7).then(|| {
            let cell = (0.0006 + (s.grain.size / 100.0) as f32 * 0.0024) * px_per_long as f32;
            // Grain finer than GRAIN_CELL_MIN px is drawn at that size, its amplitude scaled by the ratio of cell
            // sizes: averaging the export's grain down to this preview size reduces it by about as much, so a
            // small preview doesn't show coarser, relatively stronger grain than the export.
            let fade = (cell / GRAIN_CELL_MIN).min(1.0);
            ((s.grain.amount / 100.0) as f32 * 0.13 * fade, cell.max(GRAIN_CELL_MIN), (s.grain.roughness / 100.0) as f32, s.grain.seed)
        });
        let calibration = s.section_enabled("calibration");
        let peak = s.hdr.peak();
        let (tone, rgb_contrast) = if s.light.contrast_mode == lcc_develop::ContrastMode::Rgb && (s.light.contrast != 0.0 || s.light.blacks != 0.0) {
            let base = base_tone_with_contrast_blacks(s, info, peak, 0.0, 0.0);
            let full = base_tone(s, info, peak);
            let rc = crate::tone::RgbContrast::new(&base, &full, s.light.blacks, peak);
            (base, Some(rc))
        } else {
            (base_tone(s, info, peak), None)
        };
        FinishParams {
            peak,
            calib: if calibration {
                crate::colorops::calibration_matrix(&s.calibration, s.process.process() >= lcc_develop::Process::V9)
            } else {
                None
            },
            shadow_tint: if calibration { (s.calibration.shadows_tint / 100.0) as f32 } else { 0.0 },
            tone,
            rgb_contrast,
            lut: crate::lut::get(&s.profile.id).map(|l| (l, (s.profile.amount / 100.0).clamp(0.0, 2.0) as f32)),
            ops: ColorOps::new(s),
            local_points: crate::colorops::mask_point_ks(s),
            local_mixers: crate::colorops::mask_mixer_ks(s),
            brightness: crate::brightness::BrightnessK::for_peak(s.light.brightness, peak),
            curves: curve_luts(&s.curve),
            levels: crate::levels::LevelsK::new(&s.levels),
            refine_sat: (s.curve.refine_saturation / 100.0).clamp(0.0, 1.0) as f32,
            vig: if effects { vignette(s) } else { None },
            to_out: space.from_working(),
            out_luma: space.luma(),
            out_trc: space.trc(),
            proof: None,
            hl: (s.light.highlights / 100.0) as f32,
            sh: (s.light.shadows / 100.0) as f32,
            clar,
            tex,
            dehaze,
            dehaze_v3: s.process.process() >= lcc_develop::Process::V3,
            sharpen_v4: sharpen_v4(s),
            hue_clip: s.process.process() >= lcc_develop::Process::V5,
            dither: s.process.process() >= lcc_develop::Process::V8,
            sharpen: (s.detail.sharpen_amount / 150.0) as f32,
            sharpen_mask: (s.detail.sharpen_masking / 100.0) as f32,
            sharpen_limit: sharpen_limit(s.detail.sharpen_detail),
            sharpen_fade: crate::local::sharpen_fade(crate::local::sharpen_sigma_raw(s, px_per_long, frame.ow.max(frame.oh))),
            air: air_pre * gain,
            air_pre,
            gain,
            ev,
            grain,
            particles,
            out_to_norm,
            ow: frame.ow,
            oh: frame.oh,
            w,
            h,
            px_per_long,
            view: frame.view.map_or([0.0, 0.0, w as f32, h as f32], |v| [v.x as f32, v.y as f32, v.full_w as f32, v.full_h as f32]),
        }
    }

    /// Render into a display's primaries (sRGB curve) instead of the output space, soft proof
    /// included (see [`crate::DisplaySpace`]).
    pub fn for_display(&mut self, d: &crate::DisplaySpace, proof: Option<crate::Proof>) {
        self.to_out = d.from_working;
        self.out_luma = d.luma;
        self.out_trc = OutputTrc::Srgb;
        self.proof = proof.map(|pr| pr.params_display(d));
    }
}

/// The base tone map (scene → display luminance, with Contrast, Whites and Blacks) of `s`'s
/// rendering process. This is where a new tone model branches: a retuned default tone curve (issue
/// #146) is a new [`Process`] with its own arm here, and V1's arm stays as it is, so photos edited
/// under V1 keep their look (`docs/process-versions.md`). CPU and GPU both take it from here.
/// `peak` is the HDR render's peak relative to SDR white ([`lcc_develop::Hdr::peak`]); at 1.0 (SDR)
/// every arm is the SDR tone map. An HDR render keeps the camera's look through the midtones and
/// lifts its highlights past white ([`ToneMap::camera_hdr`]), so it matches the SDR render below them.
pub fn base_tone(s: &DevelopSettings, info: &SourceInfo, peak: f32) -> ToneMap {
    base_tone_with_contrast_blacks(s, info, peak, s.light.contrast, s.light.blacks)
}

pub fn base_tone_with_contrast_blacks(s: &DevelopSettings, info: &SourceInfo, peak: f32, contrast: f64, blacks: f64) -> ToneMap {
    // the SDR rendition of an HDR edit: the HDR curve, its highlights rolled off into SDR white
    // (issue #69); SDR edits never get here (their rolloff peak is 1)
    let rolloff = s.hdr.rolloff_peak();
    if peak <= 1.0 && rolloff > 1.0 {
        return base_tone_at_with(s, info, rolloff, contrast, blacks).sdr_rolloff(rolloff);
    }
    base_tone_at_with(s, info, peak, contrast, blacks)
}

fn base_tone_at_with(s: &DevelopSettings, info: &SourceInfo, peak: f32, contrast: f64, blacks: f64) -> ToneMap {
    let l = &s.light;
    // a Source filter that renders a finished picture (a negative conversion) is shown as it is,
    // like a JPEG, not toned again as a raw
    let raw = info.raw
        && !(crate::plugin_filters::any_active(s)
            && crate::plugin_filters::Stack::resolve(info, s, lcc_develop::FilterStage::Source).neutral_base_tone());
    match s.process.process() {
        // V2 changes the Highlights/Shadows base (`local::tone_base`), V5 the over-range colours
        // ([`hue_clip`]), V6 Denoise, V7 grain: not the tone map
        Process::V1 | Process::V2 | Process::V3 | Process::V4 | Process::V5 | Process::V6 | Process::V7 => {
            match info.camera_tone.as_ref().filter(|_| raw) {
                Some(curve) => ToneMap::camera_hdr(curve, contrast, l.whites, blacks, peak),
                None if raw => ToneMap::hdr(contrast, l.whites, blacks, peak),
                None => ToneMap::display_hdr(contrast, l.whites, blacks, peak),
            }
        }
        // a raw's tone curve rolls off into white in stops: a camera curve past its last knot, the
        // generic curve from half a stop below its white point instead of clipping (upstream #3783)
        Process::V8 | Process::V9 => match info.camera_tone.as_ref().filter(|_| raw) {
            Some(curve) => ToneMap::camera_rolloff_hdr(curve, contrast, l.whites, blacks, peak),
            None if raw => ToneMap::rolloff_hdr(contrast, l.whites, blacks, peak),
            None => ToneMap::display_hdr(contrast, l.whites, blacks, peak),
        },
    }
}

/// The Display-stage plug-in filters a finish runs before its final encoding.
pub(crate) struct Post<'a, 'w> {
    pub stack: &'a crate::plugin_filters::Stack,
    pub frame: lcc_filters::FrameInfo,
    pub warnings: std::cell::RefCell<&'w mut Vec<String>>,
}

/// [`finish_with`], with the Display plug-in filters run on its samples (`linear`: on the linear
/// values of the sRGB curve, for HDR) before `store`.
fn finish_post<T: Copy + Default + Send>(
    p: &Prepared,
    fp: &FinishParams,
    exact: bool,
    post: &Post<'_, '_>,
    linear: bool,
    store: impl Fn(usize, usize, [f32; 3]) -> T + Sync + Send,
) -> Vec<T> {
    if post.stack.is_empty() {
        return finish_with(p, fp, exact, store);
    }
    let (w, h) = (p.img.width, p.img.height);
    let mut v = if linear { finish_with(p, fp, true, |_, _, e| e.map(srgb_to_linear_ext)) } else { finish_with(p, fp, exact, |_, _, e| e) };
    crate::plugin_filters::apply_display(post.stack, &mut v, w, h, post.frame, linear, &mut post.warnings.borrow_mut());
    let (ox, oy) = view_origin(fp.view);
    let mut out = vec![T::default(); v.len()];
    for_rows(&mut out, w.max(1), |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let e = v.get(y * w + x).copied().unwrap_or_default();
            *o = store(ox.wrapping_add(x), oy.wrapping_add(y), if linear { e.map(linear_to_srgb_ext) } else { e });
        }
    });
    out
}

/// Where the window being drawn starts in the whole output, in whole pixels (floored, as the GPU
/// finish does): the ordered dither's pattern belongs to the whole output, so tiles line up.
fn view_origin(view: [f32; 4]) -> (usize, usize) {
    let at = |v: f64| if v.is_finite() && v > 0.0 { v.floor() as usize } else { 0 };
    (at(view[0] as f64), at(view[1] as f64))
}

/// The sRGB curve extended to all values (odd), as the finish encodes HDR values above 1.
fn srgb_to_linear_ext(v: f32) -> f32 {
    v.signum() * lcc_color::transfer::srgb_to_linear(v.abs())
}

fn linear_to_srgb_ext(v: f32) -> f32 {
    v.signum() * lcc_color::transfer::linear_to_srgb(v.abs())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finish(
    p: &Prepared,
    s: &DevelopSettings,
    frame: &Frame,
    info: &SourceInfo,
    space: OutputSpace,
    display: Option<&crate::DisplaySpace>,
    proof: Option<crate::Proof>,
    post: &Post<'_, '_>,
) -> Rgba8 {
    let (w, h) = (p.img.width, p.img.height);
    let mut fp = FinishParams::new(s, frame, info, w, h, p.px_per_long, p.air, space);
    fp.proof = proof.map(|pr| pr.params(space));
    if let Some(d) = display {
        fp.for_display(d, proof);
    }
    let trc = fp.out_trc;
    let dither = fp.dither;
    let data = finish_post(p, &fp, false, post, false, |x, y, e| {
        let d = if dither { bayer_dither_offset(x, y) } else { 0.5 };
        match trc {
            OutputTrc::Srgb => [enc(e[0], d), enc(e[1], d), enc(e[2], d), 255],
            t => {
                let c = e.map(|v| enc(t.encode(lcc_color::transfer::srgb_to_linear(v.clamp(0.0, 1.0))), d));
                [c[0], c[1], c[2], 255]
            }
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
    post: &Post<'_, '_>,
) -> DeepImage {
    use lcc_color::transfer::srgb_to_linear;
    let (w, h) = (p.img.width, p.img.height);
    let mut fp = FinishParams::new(s, frame, info, w, h, p.px_per_long, p.air, space);
    fp.proof = proof.map(|pr| pr.params(space));
    let trc = fp.out_trc;
    let samples = match depth {
        OutputDepth::F32Linear => {
            let v = finish_post(p, &fp, true, post, false, |_, _, e| e.map(|v| srgb_to_linear(v.clamp(0.0, 1.0))));
            DeepSamples::F32(v.into_flattened())
        }
        OutputDepth::F32Hdr => {
            // the sRGB curve extended past 1 (it is unbounded), so values up to `peak` survive
            let peak = fp.peak;
            let v = finish_post(p, &fp, true, post, true, |_, _, e| e.map(|v| srgb_to_linear(v.max(0.0)).clamp(0.0, peak)));
            DeepSamples::F32(v.into_flattened())
        }
        _ => {
            let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            let v = finish_post(p, &fp, true, post, false, |_, _, e| match trc {
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
/// `exact`: encode with the exact sRGB curve instead of the (8/10-bit accurate) table. `store` also
/// gets the pixel's place in the whole output ([`view_origin`] plus its place in the window).
pub(crate) fn finish_with<T: Copy + Default + Send>(
    p: &Prepared,
    fp: &FinishParams,
    exact: bool,
    store: impl Fn(usize, usize, [f32; 3]) -> T + Sync + Send,
) -> Vec<T> {
    let (w, h) = (p.img.width, p.img.height);
    let p_lut = fp.lut.clone();
    let FinishParams {
        tone,
        rgb_contrast,
        ops,
        curves,
        vig,
        to_out,
        out_luma,
        hl,
        sh,
        clar,
        tex,
        dehaze,
        dehaze_v3,
        sharpen_v4,
        sharpen,
        sharpen_mask,
        sharpen_limit,
        sharpen_fade,
        air,
        air_pre,
        gain,
        ev,
        grain,
        particles,
        ..
    } = fp;
    let dehaze_v3 = *dehaze_v3;
    let sharpen_v4 = *sharpen_v4;
    let (hl, sh, clar, tex, dehaze, sharpen, sharpen_mask, air, air_pre, gain, ev) =
        (*hl, *sh, *clar, *tex, *dehaze, *sharpen, *sharpen_mask, *air, *air_pre, *gain, *ev);
    let (sharpen_limit, sharpen_fade) = (*sharpen_limit, *sharpen_fade);
    let rgb_contrast = rgb_contrast.as_ref();
    let mut terms: Vec<[f32; MASK_TERMS]> = p.masks.iter().map(|m| mask_terms(&m.adjust)).collect();
    with_tone(&mut terms, &p.mask_tone);
    // (only when it lines up with the evaluated masks, which it does unless a caller mixed settings)
    let local_points: &[Vec<crate::colorops::PointK>] = if fp.local_points.len() == p.masks.len() { &fp.local_points } else { &[] };
    let local_mixers: &[Option<crate::colorops::MixerK>] = if fp.local_mixers.len() == p.masks.len() { &fp.local_mixers } else { &[] };
    let local_curves: Vec<_> = p.masks.iter().filter_map(|m| Some((m, m.curve.as_ref()?))).collect();
    let out_to_norm = fp.out_to_norm;
    let long = fp.ow.max(fp.oh);
    let [vx, vy, vw, vh] = fp.view;
    let (ox, oy) = view_origin(fp.view);
    // the vignette belongs to the whole output frame, not to the window being drawn
    let aspect = vw / vh;

    let srgb = srgb_lut();
    let peak = fp.peak;
    let hdr = peak > 1.0;
    let mut out = vec![T::default(); w * h];
    // bands of rows: the particle grain splats each grain once per band (`GrainParams::apply_band`)
    const BAND: usize = 8;
    for_rows(&mut out, (w * BAND).max(1), |band, rows| {
        // display-encoded colour and gamut-warning overlay per pixel, before particle grain and LUT
        let mut enc: Vec<([f32; 3], Option<[f32; 3]>)> = Vec::with_capacity(rows.len());
        for q in 0..rows.len() {
            let (x, y) = (q % w.max(1), band * BAND + q / w.max(1));
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
            let [
                l_exp,
                l_temp,
                l_tint,
                l_con,
                l_hl,
                l_sh,
                l_wh,
                l_bl,
                l_tex,
                l_clar,
                l_dehaze,
                l_sat,
                l_hue,
                l_sharp,
                l_noise,
                l_moire,
                l_defringe,
                ..,
            ] = l;
            let l_grade: [f32; crate::colorops::LOCAL_GRADE_TERMS] = std::array::from_fn(|k| l[17 + k]);

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
                // The dark channel is a blur, not a patch minimum: near bright neighbours it credits
                // dark pixels with veil they don't hold (and a windowed render borrows the whole
                // frame's airlight), so the subtraction went past zero and crushed them to black
                // (#2504). Never attribute more veil than the pixel's own minimum channel.
                let c_min = c[0].min(c[1]).min(c[2]);
                let d = (if air_pre > 0.0 { dark.data[i] / air_pre } else { 0.0 }).min(if air > 0.0 { c_min / air } else { 0.0 }).clamp(0.0, 1.0);
                if dz > 0.0 && dehaze_v3 {
                    c = dehaze_v3_pixel(c, dz, d, air);
                } else if dz > 0.0 {
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
                // Highlights reach down into the upper midtones and ease off towards white (fitted to
                // Lightroom on a Sony A7 V raw, issue #1371); Shadows are a little gentler.
                let wh = smooth(-2.4, 1.0, base) * (1.0 - 0.7 * smooth(1.4, 3.2, base));
                let sg = if ss > 0.0 { 0.82 } else { 0.92 };
                delta += ss * 1.7 * sg * ws * ws.sqrt() + hh * 0.85 * wh;
            }
            // local Whites / Blacks: the brightest (darkest) tones under each mask, fitted to that
            // mask's own tones (crate::mask_tone), so a white dress's highlights move as a sky's do
            if l_wh != 0.0 || l_bl != 0.0 {
                for (m, t) in p.masks.iter().zip(&terms) {
                    let a = m.alpha.data.get(i).copied().unwrap_or(0.0);
                    if a <= 0.0 {
                        continue;
                    }
                    if t[6] != 0.0 {
                        let w = smooth(t[MASK_TONE], t[MASK_TONE + 1], l1);
                        delta += a * t[6] * LOCAL_WB_EV * w * w.sqrt();
                    }
                    if t[7] != 0.0 {
                        let b = 1.0 - smooth(t[MASK_TONE + 2], t[MASK_TONE + 3], l1);
                        delta += a * t[7] * LOCAL_WB_EV * b;
                    }
                }
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
            // V1–V3 sharpen by boosting the Texture band; V4 has its own pixel-scale band
            let sp_old = if sharpen_v4 { 0.0 } else { l_sharp * 0.6 + sharpen };
            if (tx != 0.0 || sp_old != 0.0)
                && let Some(b) = &p.texture_blur
            {
                let det = l_pre - b.data[i];
                let tame = 1.0 - 0.6 * smooth(0.4, 1.6, det.abs());
                delta += tx * 1.1 * det.clamp(-1.0, 1.0) * tame;
                if sp_old != 0.0 {
                    let m = if sharpen_mask > 0.0 { smooth(sharpen_mask * 0.25, sharpen_mask * 0.25 + 0.15, det.abs()) } else { 1.0 };
                    delta += sp_old * 1.3 * det.clamp(-0.8, 0.8) * m;
                }
            }
            // V4 sharpening (Amount and the mask brush's Sharpness): unsharp mask on the pixel-scale band
            let sp = if sharpen_v4 { (l_sharp * 0.6 + sharpen) * sharpen_fade } else { 0.0 };
            if sp != 0.0
                && let Some(b) = &p.sharpen_blur
            {
                delta += sp * sharpen_term(l_pre - b.data[i], sharpen_limit, sharpen_mask);
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

            // --- calibration (scene linear, before the tone map)
            if fp.calib.is_some() || fp.shadow_tint != 0.0 {
                c = crate::colorops::calibrate(c, fp.calib.as_ref(), fp.shadow_tint);
            }

            // --- tone map on luminance, highlight desaturation
            let yl = luminance_2020(c);
            let mut o = tone.apply(yl);
            let mut d = if yl > 1e-9 { c.map(|v| v * o / yl) } else { [0.0; 3] };
            let k = tone.chroma_scale(o);
            if k != 1.0 {
                d = d.map(|v| o + (v - o) * k);
            }
            if let Some(rc) = rgb_contrast {
                d = d.map(|v| rc.apply(v));
                o = rc.apply(o);
            }
            let mx = d[0].max(d[1]).max(d[2]);
            if mx > peak {
                let t = ((mx - peak) / (mx - o).max(1e-6)).clamp(0.0, 1.0);
                let grey = d.map(|v| v + (o - v) * t);
                d = if fp.hue_clip { keep_hue(d, grey, o, peak) } else { grey };
            }

            // --- primaries (the colourist wheels: lift/gamma/gain/offset, balance before the look)
            if let Some(p) = &ops.primaries {
                d = p.apply(d);
            }
            // --- colour
            d = ops.apply(d, l_sat, l_hue);
            // local Point Color: each mask's samples at the mask's opacity
            if !local_points.is_empty() || !local_mixers.is_empty() {
                d = local_point_colors(d, i, &p.masks, local_mixers, local_points);
            }
            if let Some((dir, amt)) = tint_col {
                let lab = lcc_color::perceptual::oklab_from_2020(d);
                d = lcc_color::perceptual::oklab_to_2020([lab[0], lab[1] + dir[0] * 0.08 * amt, lab[2] + dir[1] * 0.08 * amt]);
            }
            d = crate::colorops::local_grade(d, &l_grade);

            // --- vignette (display linear, post-crop)
            if let Some(v) = vig {
                let u = (vx + x as f32 + 0.5) / vw * 2.0 - 1.0;
                let vv = (vy + y as f32 + 0.5) / vh * 2.0 - 1.0;
                let sx = 1.0 + (aspect - 1.0) * v.aspect_mix;
                let sy = 1.0 + (1.0 / aspect - 1.0) * v.aspect_mix;
                let (ax, ay) = ((u * sx.max(1.0) / sx.max(sy)).abs(), (vv * sy.max(1.0) / sx.max(sy)).abs());
                let dist = (ax.powf(v.power) + ay.powf(v.power)).powf(1.0 / v.power);
                let t = smooth(v.start, v.start + v.width, dist);
                if t > 0.0 {
                    let lum = luminance_2020(d).clamp(0.0, 1.0);
                    if v.amount < 0.0 {
                        let mut f = 1.0 + v.amount * t;
                        if v.style == VignetteStyle::HighlightPriority {
                            f += (1.0 - f) * v.highlights * smooth(0.4, 1.0, lum);
                        }
                        if v.style == VignetteStyle::PaintOverlay {
                            d = d.map(|c| c * (1.0 - (-v.amount) * t) + 0.0);
                        } else {
                            d = d.map(|c| c * f);
                        }
                    } else if hdr {
                        // lighten towards SDR white; never pull HDR highlights down to it
                        d = d.map(|c| c + (1.0 - c).max(0.0) * v.amount * t * 0.85);
                    } else {
                        d = d.map(|c| c + (1.0 - c) * v.amount * t * 0.85);
                    }
                }
            }

            // --- gamut map to the output space (desaturate towards luminance until in range);
            // soft proofing maps into the proof space first and shows that in the output space
            let mut warn = None;
            let mut r = match &fp.proof {
                Some(pp) if pp.table.is_some() => {
                    let (q, dist) = pp.table.as_ref().map_or((d, 0.0), |t| t.sample(d));
                    if pp.dest_warning && dist > crate::output::PROOF_GAMUT_TOLERANCE {
                        warn = Some(crate::output::PROOF_DEST_WARNING);
                    }
                    mul3(to_out, q)
                }
                Some(pp) => {
                    let q0 = mul3(&pp.to_proof, d);
                    let (q, t) = gamut_map_to(q0, pp.luma, peak);
                    let q = if fp.hue_clip { keep_hue_in_gamut(q0, q, pp.luma, peak) } else { q };
                    if pp.dest_warning && out_of_gamut(q0, t) {
                        warn = Some(crate::output::PROOF_DEST_WARNING);
                    }
                    mul3(&pp.proof_to_out, q)
                }
                None => mul3(to_out, d),
            };
            let (mapped, t) = gamut_map_to(r, *out_luma, peak);
            if fp.proof.as_ref().is_some_and(|pp| pp.display_warning) && warn.is_none() && out_of_gamut(r, t) {
                warn = Some(crate::output::PROOF_DISPLAY_WARNING);
            }
            r = if fp.hue_clip { keep_hue_in_gamut(r, mapped, *out_luma, peak) } else { mapped };

            // --- encode, curves, grain
            let mut e = if hdr {
                // HDR: the (unbounded) sRGB curve, so values above SDR white encode above 1
                r.map(|v| {
                    if v > 1.0 {
                        linear_to_srgb(v)
                    } else if exact {
                        linear_to_srgb(v.max(0.0))
                    } else {
                        encode_srgb(srgb, v)
                    }
                })
            } else if exact {
                r.map(|v| linear_to_srgb(v.clamp(0.0, 1.0)))
            } else {
                r.map(|v| encode_srgb(srgb, v))
            };
            // --- Brightness: the one call site of the stage (see `crate::brightness`): on the final in-gamut display
            // values, right after the encode and before the Tone Curve
            if let Some(b) = &fp.brightness {
                e = b.apply(e, hdr);
            }
            // --- Levels: the one call site of the stage (see `crate::levels`): after the gamut map and the encode above, so
            // on the final in-gamut display values, and before the Tone Curve.
            if let Some(lv) = &fp.levels {
                e = lv.apply(e, hdr);
            }
            if let Some(l) = curves {
                let e0 = e;
                e = eval_curves(l, e, hdr);
                if fp.refine_sat < 1.0 {
                    e = refine_saturation(e0, e, fp.refine_sat);
                }
            }
            // --- the masks' local Curves, in mask order, each weighted by its mask (feather, Amount, invert and
            // combined components are all in the alpha)
            for (m, l) in &local_curves {
                let a = m.alpha.data[i];
                if a > 0.0 {
                    let c = eval_curves(l, e, hdr);
                    e = std::array::from_fn(|k| e[k] + (c[k] - e[k]) * a);
                }
            }
            if let Some((amt, cell, rough, seed)) = *grain {
                let n = out_to_norm.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                let (gx, gy) = ((n.x * fp.ow) as f32 / long as f32, (n.y * fp.oh) as f32 / long as f32);
                let sc = fp.px_per_long as f32 / cell;
                let mut g = grain_noise(gx * sc, gy * sc, seed);
                g = g * (1.0 - rough * 0.5) + grain_noise(gx * sc * 2.3, gy * sc * 2.3, seed ^ 0x55) * rough * 0.7;
                let mut lum = 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2];
                if hdr {
                    lum = lum.clamp(0.0, 1.0);
                }
                let k = amt * g * (0.35 + 2.6 * lum * (1.0 - lum));
                if k.is_finite() {
                    e = e.map(|v| v + k);
                }
            }
            enc.push((e, warn));
        }
        // process V7: clumps of particles over the band (OneSlips, `crate::grain`)
        if let Some(gp) = particles {
            let (mut gx, mut gy) = (Vec::with_capacity(enc.len()), Vec::with_capacity(enc.len()));
            for q in 0..enc.len() {
                let (x, y) = (q % w.max(1), band * BAND + q / w.max(1));
                let n = out_to_norm.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5));
                gx.push((n.x * fp.ow / long) as f32);
                gy.push((n.y * fp.oh / long) as f32);
            }
            let mut es: Vec<[f32; 3]> = enc.iter().map(|q| q.0).collect();
            gp.apply_band(&gx, &gy, w, &mut es);
            for (q, e) in enc.iter_mut().zip(es) {
                if e.iter().all(|v| v.is_finite()) {
                    q.0 = e;
                }
            }
        }
        for (q, (px, (e, warn))) in rows.iter_mut().zip(enc).enumerate() {
            let (x, y) = (q % w.max(1), band * BAND + q / w.max(1));
            let e = match &p_lut {
                Some((l, k)) if hdr => {
                    // the LUT sees SDR values; the part above SDR white is carried over
                    let c = e.map(|v| v.clamp(0.0, 1.0));
                    let m = l.apply(c);
                    [e[0] + (m[0] - c[0]) * k, e[1] + (m[1] - c[1]) * k, e[2] + (m[2] - c[2]) * k]
                }
                Some((l, k)) => {
                    let m = l.apply(e.map(|v| v.clamp(0.0, 1.0)));
                    [e[0] + (m[0] - e[0]) * k, e[1] + (m[1] - e[1]) * k, e[2] + (m[2] - e[2]) * k]
                }
                None => e,
            };
            *px = store(ox.wrapping_add(x), oy.wrapping_add(y), warn.unwrap_or(e));
        }
    });
    out
}

/// The masks' local Color Mixer and Point Color on display-linear Rec.2020 colour `d` of pixel `i`:
/// each mask's mixer, then its samples, at the mask's opacity there (at most 1). `mixers` and
/// `local` are each empty or one entry per mask.
#[inline]
fn local_point_colors(
    d: [f32; 3],
    i: usize,
    masks: &[crate::masks::Evaluated],
    mixers: &[Option<crate::colorops::MixerK>],
    local: &[Vec<crate::colorops::PointK>],
) -> [f32; 3] {
    use lcc_color::perceptual::{lab_to_lch, lch_to_lab, oklab_from_2020, oklab_to_2020};
    let mut lch: Option<[f32; 3]> = None;
    for (mi, m) in masks.iter().enumerate() {
        let mixer = mixers.get(mi).copied().flatten();
        let points: &[crate::colorops::PointK] = local.get(mi).map_or(&[], Vec::as_slice);
        if points.is_empty() && mixer.is_none() {
            continue;
        }
        let a = m.alpha.data.get(i).copied().unwrap_or(0.0).min(1.0);
        if a <= 0.0 {
            continue;
        }
        let mut v = lch.unwrap_or_else(|| lab_to_lch(oklab_from_2020(d)));
        if let Some(mk) = mixer {
            v = mk.apply_scaled(v, a);
        }
        for k in points {
            v = k.apply_scaled(v, a);
        }
        lch = Some(v);
    }
    lch.map_or(d, |v| oklab_to_2020(lch_to_lab(v)))
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
    gamut_map_to(r, luma, 1.0)
}

/// [`gamut_map`] into `0..peak` (an HDR render's range; `peak` = 1 is [`gamut_map`]).
#[inline]
pub fn gamut_map_to(r: [f32; 3], luma: [f32; 3], peak: f32) -> ([f32; 3], f32) {
    let yy = (luma[0] * r[0] + luma[1] * r[1] + luma[2] * r[2]).clamp(0.0, peak);
    let mut t = 1.0f32;
    for c in r {
        if c < 0.0 {
            t = t.min(yy / (yy - c).max(1e-9));
        } else if c > peak {
            t = t.min((peak - yy) / (c - yy).max(1e-9));
        }
    }
    if t < 1.0 { (r.map(|c| yy + (c - yy) * t), t) } else { (r, 1.0) }
}

/// Luminance (relative to the peak) from which an over-range colour fades from keeping its hue
/// ([`hue_clip`]) to moving toward its luminance as before V5, reached at the peak: a colour that is
/// nearly white overall (a blown sky) still goes white, as a camera's per-channel curve takes it.
pub const HUE_FADE_FROM: f32 = 0.35;
/// Stops by which the brightest channel exceeds the peak over which an over-range colour fades
/// toward its luminance whatever its hue: a colour just over white keeps its hue, one half a stop
/// over has gone white, gradually (no hard edge along an exposure ramp of a saturated colour). This
/// brightest-channel key does most of the fading: the luminance one alone left dark-luminance hues
/// (blue, violet, magenta) purple up to two stops over while warm hues of the same brightest channel
/// had gone white.
pub const HUE_FADE_EV: f32 = 0.5;

/// How far an over-range colour whose brightest channel is `hi` (and luminance `y`) has faded from
/// [`hue_clip`] to the pre-V5 mapping (0 = hue kept, 1 = moved toward its luminance).
#[inline]
fn hue_fade(y: f32, hi: f32, peak: f32) -> f32 {
    let over = if hi > peak && peak > 0.0 { (hi / peak).log2() } else { 0.0 };
    smooth(0.0, HUE_FADE_EV, over).max(smooth(HUE_FADE_FROM * peak, peak, y))
}

/// Process V5's over-range colour after the tone map: `hue_clip(d)` for a colour of luminance `o`
/// well below white and just over it, fading to `grey` (moved toward its luminance, V1–V4).
#[inline]
fn keep_hue(d: [f32; 3], grey: [f32; 3], o: f32, peak: f32) -> [f32; 3] {
    let kept = hue_clip(d, peak);
    let w = hue_fade(o, d[0].max(d[1]).max(d[2]), peak);
    [kept[0] + (grey[0] - kept[0]) * w, kept[1] + (grey[1] - kept[1]) * w, kept[2] + (grey[2] - kept[2]) * w]
}

/// [`keep_hue`] for the output's gamut mapping: `r` limited to `peak` with its hue and negative
/// channels lifted toward its luminance, fading to `mapped` ([`gamut_map_to`]'s result).
#[inline]
fn keep_hue_in_gamut(r: [f32; 3], mapped: [f32; 3], luma: [f32; 3], peak: f32) -> [f32; 3] {
    let kept = lift_negatives(hue_clip(r, peak), luma, peak);
    let y = luma[0] * r[0] + luma[1] * r[1] + luma[2] * r[2];
    let w = hue_fade(y, r[0].max(r[1]).max(r[2]), peak);
    [kept[0] + (mapped[0] - kept[0]) * w, kept[1] + (mapped[1] - kept[1]) * w, kept[2] + (mapped[2] - kept[2]) * w]
}

/// `c` with no channel above `peak`, hue and saturation kept (process V5): the brightest channel
/// becomes `peak`, the darkest stays (unless it is above `peak` too: then white) and the middle one
/// keeps its place between them. A colour in range, or a non-finite one, is unchanged.
#[inline]
pub fn hue_clip(c: [f32; 3], peak: f32) -> [f32; 3] {
    let hi = c[0].max(c[1]).max(c[2]);
    if hi.is_nan() || hi <= peak || !peak.is_finite() {
        return c;
    }
    let lo = c[0].min(c[1]).min(c[2]);
    if lo >= peak || !lo.is_finite() {
        return [peak; 3];
    }
    let k = (peak - lo) / (hi - lo);
    c.map(|v| lo + (v - lo) * k)
}

/// [`gamut_map_to`] for negative channels only (after [`hue_clip`] took the ones above `peak`):
/// `r` desaturated towards its luminance (clamped to `0..peak`) until no channel is below 0.
#[inline]
fn lift_negatives(r: [f32; 3], luma: [f32; 3], peak: f32) -> [f32; 3] {
    let yy = (luma[0] * r[0] + luma[1] * r[1] + luma[2] * r[2]).clamp(0.0, peak);
    let t = r.iter().filter(|c| **c < 0.0).fold(1.0f32, |t, c| t.min(yy / (yy - c).max(1e-9)));
    if t < 1.0 { r.map(|c| yy + (c - yy) * t) } else { r }
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

/// Refine Saturation: scale the curved colour's chroma (around its luma, encoded values) so its
/// saturation (chroma / luma) moves from the curve's towards the pre-curve one: the ratio is
/// `(s_curve / s_before)^refine`, so 1 keeps the curve, 0 restores the original saturation.
#[inline]
pub fn refine_saturation(before: [f32; 3], after: [f32; 3], refine: f32) -> [f32; 3] {
    let luma = |e: [f32; 3]| 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2];
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

/// The 8×8 ordered (Bayer) dither matrix: every threshold 0–63 once, each spread as far as possible
/// from its neighbours.
#[rustfmt::skip]
const BAYER_8X8: [u8; 64] = [
     0, 32,  8, 40,  2, 34, 10, 42,
    48, 16, 56, 24, 50, 18, 58, 26,
    12, 44,  4, 36, 14, 46,  6, 38,
    60, 28, 52, 20, 62, 30, 54, 22,
     3, 35, 11, 43,  1, 33,  9, 41,
    51, 19, 59, 27, 49, 17, 57, 25,
    15, 47,  7, 39, 13, 45,  5, 37,
    63, 31, 55, 23, 61, 29, 53, 21,
];

/// The ordered-dither offset (0 to 1, exclusive) added before truncating to 8 bits at output pixel
/// (`x`, `y`): its mean is a half, so on average a level rounds as without dither.
#[inline]
pub(crate) fn bayer_dither_offset(x: usize, y: usize) -> f32 {
    let t = BAYER_8X8.get((y & 7) * 8 + (x & 7)).copied().unwrap_or(32);
    (f32::from(t) + 0.5) / 64.0
}

/// `v` (already sRGB-encoded) to 8 bits, adding `dither` (0 to 1; a half rounds) before truncating.
/// Values within a tenth of a level of white stay white, so dithering never darkens it.
#[inline]
fn enc(v: f32, dither: f32) -> u8 {
    let y = v.clamp(0.0, 1.0) * 255.0;
    if y >= 254.9 { 255 } else { (y + dither).clamp(0.0, 255.0) as u8 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hue_term(a: &LocalAdjustments) -> f32 {
        let zero = mask_terms(&LocalAdjustments::default());
        // the one term the Hue moves
        let probe = mask_terms(&LocalAdjustments { hue: 90.0, ..Default::default() });
        let k = (0..MASK_TERMS).find(|&k| probe[k] != zero[k]).unwrap();
        mask_terms(a)[k]
    }

    #[test]
    fn a_legacy_mask_hue_renders_at_the_same_angle_as_before() {
        for old in [-100.0, -37.0, -1.0, 0.0, 12.5, 50.0, 100.0] {
            let a: LocalAdjustments = serde_json::from_value(serde_json::json!({ "hue": old })).unwrap();
            let before = (old / 100.0) as f32 * 0.6;
            assert!((hue_term(&a) - before).abs() < 1e-6, "{old}: {} vs {before}", hue_term(&a));
        }
    }

    #[test]
    fn the_mask_hue_reaches_half_a_turn_and_hostile_values_stay_finite() {
        let full = hue_term(&LocalAdjustments { hue: 180.0, ..Default::default() });
        assert!((full - std::f32::consts::PI).abs() < 1e-5);
        for bad in [f64::NAN, f64::INFINITY, -1e300] {
            let t = hue_term(&LocalAdjustments { hue: bad, ..Default::default() });
            assert!(t.is_finite() && t.abs() <= std::f32::consts::PI + 1e-5, "{bad}: {t}");
        }
    }

    #[test]
    fn grain_noise_is_finite_for_degenerate_coordinates() {
        for (x, y) in [(f32::NAN, 1.0), (1.0, f32::INFINITY), (f32::NEG_INFINITY, f32::NAN)] {
            assert_eq!(grain_noise(x, y, 7), 0.0);
        }
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
    #[test]
    fn hue_clip_limits_the_brightest_channel_and_keeps_hue() {
        assert_eq!(hue_clip([0.9, 0.5, 0.1], 1.0), [0.9, 0.5, 0.1], "in range: unchanged");
        let c = hue_clip([1.8, 0.6, 0.0], 1.0);
        assert_eq!(c[0], 1.0);
        assert!(c[2] == 0.0 && (c[1] - 1.0 / 3.0).abs() < 1e-6, "middle keeps its place: {c:?}");
        assert_eq!(hue_clip([1.5, 1.2, 1.1], 1.0), [1.0; 3], "every channel over: white");
        assert_eq!(hue_clip([3.0, 0.5, 0.25], 2.0)[0], 2.0, "an HDR peak");
        let n = hue_clip([f32::NAN, 0.2, 0.1], 1.0);
        assert!(n[1] == 0.2 && n[2] == 0.1, "non-finite input left to the encoder: {n:?}");
        let _ = hue_clip([f32::INFINITY, f32::NEG_INFINITY, 0.0], 1.0);
    }

    #[test]
    fn keep_hue_holds_saturated_colours_and_lets_near_white_ones_go_white() {
        let grey_of = |d: [f32; 3], o: f32| {
            let mx = d[0].max(d[1]).max(d[2]);
            let t = ((mx - 1.0) / (mx - o)).clamp(0.0, 1.0);
            d.map(|v| v + (o - v) * t)
        };
        let (red, o) = ([1.05, 0.05, 0.05], 0.3);
        let (got, kept) = (keep_hue(red, grey_of(red, o), o, 1.0), hue_clip(red, 1.0));
        assert!((0..3).all(|c| (got[c] - kept[c]).abs() < 1e-3), "{got:?} vs {kept:?}");
        let (blue, o) = ([0.05, 0.1, 1.02], 0.12);
        let (got, kept) = (keep_hue(blue, grey_of(blue, o), o, 1.0), hue_clip(blue, 1.0));
        assert!((0..3).all(|c| (got[c] - kept[c]).abs() < 1e-2), "{got:?} vs {kept:?}");
        let (sky, o) = ([0.85, 1.0, 1.4], 1.0);
        assert_eq!(keep_hue(sky, grey_of(sky, o), o, 1.0), grey_of(sky, o));
    }

    /// Over-range colours of every hue fade from keeping their hue to the V4 mapping alike: a saturated blue (low
    /// luminance) and a saturated orange (high luminance) whose brightest channel is equally far over white have
    /// gone the same way from one to the other, and both have (nearly) arrived a third of a stop over.
    #[test]
    fn saturated_blue_and_orange_of_equal_brightest_channel_fade_alike() {
        let dist = |a: [f32; 3], b: [f32; 3]| (0..3).map(|c| (a[c] - b[c]).powi(2)).sum::<f32>().sqrt();
        for mx in [1.3f32, 1.6, 2.0, 2.5] {
            let fade = |c: [f32; 3]| {
                let d = c.map(|v| v * mx);
                let o = luminance_2020(d).min(1.0);
                let t = ((mx - 1.0) / (mx - o).max(1e-6)).clamp(0.0, 1.0);
                let grey = d.map(|v| v + (o - v) * t);
                let kept = hue_clip(d, 1.0);
                dist(keep_hue(d, grey, o, 1.0), kept) / dist(grey, kept).max(1e-6)
            };
            let (blue, orange) = (fade([0.05, 0.1, 1.0]), fade([1.0, 0.45, 0.05]));
            assert!((blue - orange).abs() < 0.05, "{mx}: blue faded {blue}, orange {orange}");
            assert!(blue > 0.8, "{mx}: a blue {mx}x over white has (nearly) faded ({blue})");
        }
    }

    /// An exposure ramp of a saturated colour reaches white without a step.
    #[test]
    fn saturated_ramps_reach_white_without_a_step() {
        for colour in [[0.9, 0.05, 0.05], [0.05, 0.1, 0.9], [0.8, 0.0, 0.8], [0.05, 0.8, 0.1]] {
            let y = luminance_2020(colour);
            let (mut previous, mut worst) = (None::<[f32; 3]>, 0.0f32);
            for i in 0..=400 {
                let d = colour.map(|v| v * 2f32.powf(-1.0 + i as f32 / 50.0));
                let o = (y * 2f32.powf(-1.0 + i as f32 / 50.0)).min(1.0);
                let mx = d[0].max(d[1]).max(d[2]);
                let out = if mx > 1.0 {
                    let t = ((mx - 1.0) / (mx - o).max(1e-6)).clamp(0.0, 1.0);
                    keep_hue(d, d.map(|v| v + (o - v) * t), o, 1.0)
                } else {
                    d
                };
                if let Some(p) = previous {
                    worst = worst.max((0..3).map(|c| (out[c].min(1.0) - p[c].min(1.0)).abs()).fold(0.0, f32::max));
                }
                previous = Some(out);
            }
            assert!(worst < 0.06, "{colour:?}: a step of {worst}");
        }
    }

    #[test]
    fn negatives_are_lifted_toward_luminance_and_over_range_left_alone() {
        let luma = [0.2627, 0.678, 0.0593];
        let r = lift_negatives([0.6, 0.2, -0.05], luma, 1.0);
        assert!(r[2].abs() < 1e-6 && r[0] < 0.6 && r[0] > r[1]);
        assert_eq!(lift_negatives([1.6, 0.2, 0.05], luma, 1.0), [1.6, 0.2, 0.05]);
    }

    #[test]
    fn rgb_contrast_mode_preserves_neutral_and_deepens_chromatic_contrast() {
        use lcc_develop::{ContrastMode, DevelopSettings};
        use lcc_raster::Rgb32f;
        let mut s_lum = DevelopSettings::default();
        s_lum.light.contrast = 40.0;
        s_lum.light.blacks = -25.0;
        s_lum.light.contrast_mode = ContrastMode::Luminance;

        let mut s_rgb = s_lum.clone();
        s_rgb.light.contrast_mode = ContrastMode::Rgb;

        // Neutral image: luminance-based and RGB-based contrast produce nearly identical results
        let neutral = Rgb32f::filled(8, 8, [0.18, 0.18, 0.18]);
        let req = crate::RenderRequest::fit(8, 8);
        let out_lum = crate::render(&neutral, &SourceInfo::default(), &s_lum, &req).image;
        let out_rgb = crate::render(&neutral, &SourceInfo::default(), &s_rgb, &req).image;
        for (a, b) in out_lum.data.iter().zip(&out_rgb.data) {
            for c in 0..3 {
                assert!((a[c] as i32 - b[c] as i32).abs() <= 1, "neutral channel {c}: lum {} vs rgb {}", a[c], b[c]);
            }
        }

        // Chromatic image: RGB contrast shifts channels differently from pure luminance contrast
        let chromatic = Rgb32f::filled(8, 8, [0.4, 0.1, 0.05]);
        let chrom_lum = crate::render(&chromatic, &SourceInfo::default(), &s_lum, &req).image;
        let chrom_rgb = crate::render(&chromatic, &SourceInfo::default(), &s_rgb, &req).image;
        let diff =
            (chrom_lum.data[0][0] as i32 - chrom_rgb.data[0][0] as i32).abs() + (chrom_lum.data[0][2] as i32 - chrom_rgb.data[0][2] as i32).abs();
        assert!(diff > 1, "chromatic difference should be noticeable");
    }

    /// Upstream #3509: the dither keeps black, white and every exact 8-bit level exact, and splits a
    /// level half-way between two evenly over each 8×8 tile.
    #[test]
    fn bayer_dithering_preserves_extremes_and_dithers_smoothly() {
        for y in 0..8 {
            for x in 0..8 {
                let d = bayer_dither_offset(x, y);
                assert_eq!(enc(0.0, d), 0, "black stays black");
                assert_eq!(enc(1.0, d), 255, "white stays white");
                for k in 0..=255u8 {
                    assert_eq!(enc(f32::from(k) / 255.0, d), k, "an exact level has no dither noise");
                }
            }
        }
        for (lo, hi) in [(128u8, 129u8), (254, 255)] {
            let v = (f32::from(lo) + 0.5) / 255.0;
            let mut n = [0usize; 2];
            for y in 0..8 {
                for x in 0..8 {
                    let q = enc(v, bayer_dither_offset(x, y));
                    assert!(q == lo || q == hi, "{q} is not {lo} or {hi}");
                    n[usize::from(q == hi)] += 1;
                }
            }
            assert_eq!(n, [32, 32], "half-way between {lo} and {hi} splits the tile evenly");
        }
    }

    /// Without the dither (processes V1–V7) the encoding is the plain rounding it always was.
    #[test]
    fn a_half_offset_rounds_as_before() {
        for i in -100..=1100 {
            let v = i as f32 / 1000.0;
            assert_eq!(enc(v, 0.5), (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8, "{v}");
        }
        assert_eq!(enc(f32::NAN, 0.5), 0);
    }

    /// The dither's phase comes from the view origin floored to whole pixels, as the GPU finish takes it
    /// (`u32(max(floor(view), 0))` in `finish.wgsl`): a fractional origin dithers like the pixel it lies
    /// in; a negative or non-finite one counts as 0.
    #[test]
    fn a_fractional_view_origin_takes_the_pixel_it_lies_in() {
        assert_eq!(view_origin([77.0, 13.0, 512.0, 64.0]), (77, 13));
        assert_eq!(view_origin([77.6, 13.99, 512.0, 64.0]), (77, 13));
        assert_eq!(view_origin([-0.5, f32::NAN, 512.0, 64.0]), (0, 0));
        assert_eq!(view_origin([f32::INFINITY, 0.25, 512.0, 64.0]), (0, 0));
    }

    /// The table holds every threshold once, and is the bit formula the GPU finish computes it with
    /// (`bayer_dither` in `finish.wgsl`): CPU and GPU dither alike.
    #[test]
    fn the_bayer_table_is_the_gpu_formula() {
        let mut seen = [false; 64];
        for y in 0..8usize {
            for x in 0..8usize {
                let a = x ^ y;
                let t = ((a & 1) << 5) | ((y & 1) << 4) | ((a & 2) << 2) | ((y & 2) << 1) | ((a & 4) >> 1) | ((y & 4) >> 2);
                assert_eq!(usize::from(BAYER_8X8[y * 8 + x]), t, "({x}, {y})");
                seen[t] = true;
                assert_eq!(bayer_dither_offset(x + 8, y + 16), bayer_dither_offset(x, y), "the pattern tiles");
            }
        }
        assert!(seen.iter().all(|s| *s), "every threshold once");
    }
}
