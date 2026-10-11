//! The per-pixel stage: everything after the spatial planes are ready, in one parallel pass.

use lightcraft_color::luminance_2020;
use lightcraft_color::spline::{Lut1, MonotoneCurve};
use lightcraft_color::transfer::{linear_to_srgb, srgb_to_linear};
use lightcraft_develop::{DevelopSettings, LocalAdjustments, POINT_CURVE_LUT_SIZE, PointCurve, Process, ToneCurve, VignetteStyle};
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

/// Parametric region curve (encoded domain) composed with the master point curve.
fn curve_luts(c: &ToneCurve, fades: [Option<(f64, f64)>; 4]) -> Option<[Lut1; 3]> {
    let parametric = c.highlights != 0.0 || c.lights != 0.0 || c.darks != 0.0 || c.shadows != 0.0;
    let master = !c.master.is_identity() || fades[0].is_some();
    let chans = std::array::from_fn::<_, 3, _>(|i| ![&c.red, &c.green, &c.blue][i].is_identity() || fades[i + 1].is_some());
    if !parametric && !master && !chans.iter().any(|b| *b) {
        return None;
    }
    const N: usize = POINT_CURVE_LUT_SIZE;
    let (s1, s2, s3) = ((c.split_shadows / 100.0) as f32, (c.split_mid / 100.0) as f32, (c.split_highlights / 100.0) as f32);
    let regions = [(0.0, s1, c.shadows), (s1, s2, c.darks), (s2, s3, c.lights), (s3, 1.0, c.highlights)];
    let mut base = Lut1::from_fn(N, |x| {
        let mut d = 0.0;
        for (a, b, amt) in regions {
            if amt == 0.0 {
                continue;
            }
            let (ctr, half) = ((a + b) / 2.0, (b - a) * 0.75 + 0.05);
            let t = ((x - ctr) / half).clamp(-1.0, 1.0);
            let win = 0.5 + 0.5 * (t * std::f32::consts::PI).cos();
            d += (amt / 100.0) as f32 * 0.22 * win;
        }
        (x + d * 4.0 * x * (1.0 - x)).clamp(0.0, 1.0)
    });
    // keep monotone
    for i in 1..N {
        base.v[i] = base.v[i].max(base.v[i - 1]);
    }
    if master {
        base = point_lut(&c.master, fades[0]).compose(&base);
    }
    let per = [&c.red, &c.green, &c.blue];
    Some(std::array::from_fn(|i| if chans[i] { point_lut(per[i], fades[i + 1]).compose(&base) } else { base.clone() }))
}

/// An imported profile is an independent encoded-domain function after the global curves.
/// Amount mixes the composed master/channel function with identity, without changing saved
/// curves or involving the global curve panel's bypass and Refine Saturation controls.
fn profile_curve_luts(s: &DevelopSettings) -> Option<[Lut1; 3]> {
    let look = s.profile.imported_look()?;
    // Serialized profiles are checked on load. Public in-process structs must not introduce
    // nonfinite samples or overflow when Amount extrapolates to 200%.
    if look.validate().is_err() {
        return None;
    }
    let k = (s.profile.amount / 100.0).clamp(0.0, 2.0);
    let per = [&look.red, &look.green, &look.blue];
    if !k.is_finite() || k == 0.0 || (look.master.is_identity() && per.iter().all(|c| c.is_identity())) {
        return None;
    }
    let master = look.master.render_lut();
    Some(std::array::from_fn(|i| {
        let mut lut = if per[i].is_identity() { master.clone() } else { per[i].render_lut().compose(&master) };
        if k != 1.0 {
            for (j, v) in lut.v.iter_mut().enumerate() {
                let x = j as f64 / (POINT_CURVE_LUT_SIZE - 1) as f64;
                *v = (x + k * (f64::from(*v) - x)) as f32;
            }
        }
        lut
    }))
}

/// Resolve profile output levels without fitting a blended function back through sparse handles.
fn point_lut(curve: &PointCurve, fade: Option<(f64, f64)>) -> Lut1 {
    let Some((black, white)) = fade else { return curve.render_lut() };
    if let Some(points) = curve.sparse_points() {
        // Preserve the existing sparse-curve profile result bit-for-bit.
        let points = if points.len() < 2 { vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)] } else { points.to_vec() };
        let points: Vec<_> = points.iter().map(|p| (p.x, black + p.y.clamp(0.0, 1.0) * (white - black))).collect();
        return MonotoneCurve::new(&points).to_lut(POINT_CURVE_LUT_SIZE);
    }
    let mut lut = curve.render_lut();
    for value in &mut lut.v {
        *value = (black + f64::from(*value).clamp(0.0, 1.0) * (white - black)) as f32;
    }
    lut
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

/// Fixed curve primaries for Process V2. The transfer function is sRGB, independently of
/// ProPhoto's export transfer function. Absent for V1 and when both curve stages are identity.
#[derive(Clone, Copy, Debug)]
pub struct CurveSpace {
    pub to_curve: [[f32; 3]; 3],
    pub from_curve: [[f32; 3]; 3],
}

/// Numerical guard for V2 curve transfer decoding only: ±1e15 encoded decodes below 1e36,
/// leaving f32 matrix headroom. It is independent of SDR/HDR peak; output mapping owns clipping.
pub const CURVE_ENCODED_LIMIT: f32 = 1e15;

/// Runtime HSV profile, applied in linear ProPhoto before camera tone and B&W.
/// Its value-axis transfer is part of the table, not an RGB encoding of this stage.
struct ImportedTable {
    table: std::sync::Arc<lightcraft_raw::profile::big_table::DecodedLookTable>,
    scaled: Option<lightcraft_raw::profile::HsvTable>,
    space: CurveSpace,
}

impl ImportedTable {
    fn new(resource: crate::ResolvedProfileTable) -> Self {
        let crate::ResolvedProfileTable { table, amount } = resource;
        // The shared eligibility gate checked every scaled entry before rendering. Keep the
        // original resource immutable; only non-unit amounts allocate a bounded transient grid.
        let scaled = (amount != 1.0).then(|| {
            let mut grid = table.hsv().clone();
            for entry in &mut grid.data {
                *entry = crate::table_entry_at_amount(*entry, amount);
            }
            grid
        });
        Self {
            table,
            scaled,
            space: CurveSpace {
                to_curve: lightcraft_color::REC2020.to_space(&lightcraft_color::PROPHOTO).to_f32(),
                from_curve: lightcraft_color::PROPHOTO.to_space(&lightcraft_color::REC2020).to_f32(),
            },
        }
    }

    fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let prophoto = mul3(&self.space.to_curve, rgb);
        mul3(&self.space.from_curve, self.scaled.as_ref().unwrap_or_else(|| self.table.hsv()).apply(prophoto))
    }
}

/// Everything the per-pixel stage computes once per render: the CPU loop below and the GPU kernel
/// (`lightcraft-gpu`) both read their parameters from here, so the two cannot drift apart.
pub struct FinishParams {
    /// CPU-only explicit table; public GPU entry declines active references.
    imported_table: Option<ImportedTable>,
    /// A LUT profile and its amount (0..2), applied to the display-encoded colour.
    pub lut: Option<(std::sync::Arc<crate::lut::Lut3d>, f32)>,
    pub tone: ToneMap,
    pub ops: ColorOps,
    /// Calibration: primaries matrix (row-major, linear Rec.2020) and shadows tint (−1..1).
    pub calib: Option<[[f32; 3]; 3]>,
    pub shadow_tint: f32,
    /// Tone curves (parametric ∘ point, per channel) on encoded values, 1024 entries each.
    pub curves: Option<[Lut1; 3]>,
    /// Imported profile master/channel curves and Amount, after global curves/refinement and
    /// before grain. Same sRGB encoding domain as the global curves, independent of their bypass.
    pub profile_curves: Option<[Lut1; 3]>,
    /// V2 curves run in these fixed primaries before proof/output mapping.
    pub curve_space: Option<CurveSpace>,
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
    pub sharpen: f32,
    pub sharpen_mask: f32,
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
        let peak = s.hdr.peak();
        let curves = curve_luts(&s.curve, crate::profiles::curve_fades(s));
        let profile_curves = profile_curve_luts(s);
        let curve_space =
            (matches!(s.process.process(), Process::V2 | Process::V3) && (curves.is_some() || profile_curves.is_some())).then(|| CurveSpace {
                to_curve: lightcraft_color::REC2020.to_space(&lightcraft_color::PROPHOTO).to_f32(),
                from_curve: lightcraft_color::PROPHOTO.to_space(&lightcraft_color::REC2020).to_f32(),
            });
        FinishParams {
            imported_table: None,
            peak,
            calib: if calibration { crate::colorops::calibration_matrix(&s.calibration) } else { None },
            shadow_tint: if calibration { (s.calibration.shadows_tint / 100.0) as f32 } else { 0.0 },
            tone: base_tone(s, info, peak),
            lut: crate::lut::get(&s.profile.id).map(|l| (l, (s.profile.amount / 100.0).clamp(0.0, 2.0) as f32)),
            ops: ColorOps::new(s),
            curves,
            profile_curves,
            curve_space,
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
            sharpen: (s.detail.sharpen_amount / 150.0) as f32,
            sharpen_mask: (s.detail.sharpen_masking / 100.0) as f32,
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
/// `peak` is the HDR render's peak relative to SDR white ([`lightcraft_develop::Hdr::peak`]); at 1.0 (SDR)
/// every arm is the SDR tone map. An HDR render keeps the camera's look through the midtones and
/// lifts its highlights past white ([`ToneMap::camera_hdr`]), so it matches the SDR render below them.
pub fn base_tone(s: &DevelopSettings, info: &SourceInfo, peak: f32) -> ToneMap {
    let l = &s.light;
    match s.process.process() {
        // All processes share the base tone map; V3 changes the spatial base.
        Process::V1 | Process::V2 | Process::V3 => match info.camera_tone.as_ref().filter(|_| info.raw) {
            Some(curve) => ToneMap::camera_hdr(curve, l.contrast, l.whites, l.blacks, peak),
            None if info.raw => ToneMap::hdr(l.contrast, l.whites, l.blacks, peak),
            None => ToneMap::display_hdr(l.contrast, l.whites, l.blacks, peak),
        },
    }
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
    imported_table: Option<crate::ResolvedProfileTable>,
) -> Rgba8 {
    let (w, h) = (p.img.width, p.img.height);
    let mut fp = FinishParams::new(s, frame, info, w, h, p.px_per_long, p.air, space);
    fp.imported_table = imported_table.map(ImportedTable::new);
    fp.proof = proof.map(|pr| pr.params(space));
    if let Some(d) = display {
        fp.for_display(d, proof);
    }
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
    imported_table: Option<crate::ResolvedProfileTable>,
) -> DeepImage {
    use lightcraft_color::transfer::srgb_to_linear;
    let (w, h) = (p.img.width, p.img.height);
    let mut fp = FinishParams::new(s, frame, info, w, h, p.px_per_long, p.air, space);
    fp.imported_table = imported_table.map(ImportedTable::new);
    fp.proof = proof.map(|pr| pr.params(space));
    let trc = fp.out_trc;
    let samples = match depth {
        OutputDepth::F32Linear => {
            let v = finish_with(p, &fp, true, |e| e.map(|v| srgb_to_linear(v.clamp(0.0, 1.0))));
            DeepSamples::F32(v.into_flattened())
        }
        OutputDepth::F32Hdr => {
            // the sRGB curve extended past 1 (it is unbounded), so values up to `peak` survive
            let peak = fp.peak;
            let v = finish_with(p, &fp, true, |e| e.map(|v| srgb_to_linear(v.max(0.0)).clamp(0.0, peak)));
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
    let FinishParams {
        tone,
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
        sharpen,
        sharpen_mask,
        air,
        air_pre,
        gain,
        ev,
        grain,
        ..
    } = fp;
    let (hl, sh, clar, tex, dehaze, sharpen, sharpen_mask, air, air_pre, gain, ev) =
        (*hl, *sh, *clar, *tex, *dehaze, *sharpen, *sharpen_mask, *air, *air_pre, *gain, *ev);
    let terms: Vec<[f32; MASK_TERMS]> = p.masks.iter().map(|m| mask_terms(&m.adjust)).collect();
    let out_to_norm = fp.out_to_norm;
    let long = fp.ow.max(fp.oh);
    let [vx, vy, vw, vh] = fp.view;
    // the vignette belongs to the whole output frame, not to the window being drawn
    let aspect = vw / vh;

    let srgb = srgb_lut();
    let peak = fp.peak;
    let hdr = peak > 1.0;
    let mut out = vec![T::default(); w * h];
    for_rows(&mut out, w, |y, row| {
        for (x, px) in row.iter_mut().enumerate() {
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
            let sp = l_sharp * 0.6;
            if (tx != 0.0 || sp != 0.0)
                && let Some(b) = &p.texture_blur
            {
                let det = l_pre - b.data[i];
                let tame = 1.0 - 0.6 * smooth(0.4, 1.6, det.abs());
                delta += tx * 1.1 * det.clamp(-1.0, 1.0) * tame;
                if sp != 0.0 {
                    let m = if sharpen_mask > 0.0 { smooth(sharpen_mask * 0.25, sharpen_mask * 0.25 + 0.15, det.abs()) } else { 1.0 };
                    delta += sp * 1.3 * det.clamp(-0.8, 0.8) * m;
                }
            }
            if sharpen != 0.0
                && let Some(b) = &p.sharpen_blur
            {
                let det = l_pre - b.data[i];
                let m = if sharpen_mask > 0.0 { smooth(sharpen_mask * 0.25, sharpen_mask * 0.25 + 0.15, det.abs()) } else { 1.0 };
                delta += sharpen * 1.3 * det.clamp(-0.8, 0.8) * m;
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

            // --- explicit profile table (scene linear, before camera tone and B&W)
            if let Some(table) = &fp.imported_table {
                c = table.apply(c);
            }

            // --- tone map on luminance, highlight desaturation
            let yl = luminance_2020(c);
            let o = tone.apply(yl);
            let mut d = if yl > 1e-9 { c.map(|v| v * o / yl) } else { [0.0; 3] };
            let k = tone.chroma_scale(o);
            if k != 1.0 {
                d = d.map(|v| o + (v - o) * k);
            }
            let mx = d[0].max(d[1]).max(d[2]);
            if mx > peak {
                let t = ((mx - peak) / (mx - o).max(1e-6)).clamp(0.0, 1.0);
                d = d.map(|v| v + (o - v) * t);
            }

            // --- colour
            d = ops.apply(d, l_sat, l_hue);
            if let Some((dir, amt)) = tint_col {
                let lab = lightcraft_color::perceptual::oklab_from_2020(d);
                d = lightcraft_color::perceptual::oklab_to_2020([lab[0], lab[1] + dir[0] * 0.08 * amt, lab[2] + dir[1] * 0.08 * amt]);
            }

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

            // V2 only: curves see fixed primaries before proof/output gamut mapping. Identity
            // curves skip even this matrix round trip, preserving the no-curve path exactly.
            if let Some(space) = fp.curve_space {
                d = working_curves(d, fp, space, srgb, exact);
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
            r = mapped;

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
            // V1 retains its original target-primary arithmetic and ordering.
            if fp.curve_space.is_none() {
                if let Some(l) = curves {
                    let e0 = e;
                    e = if hdr {
                        // above SDR white the curves continue with slope 1 from their end point
                        std::array::from_fn(|k| if e[k] > 1.0 { l[k].eval(1.0) + (e[k] - 1.0) } else { l[k].eval(e[k]) })
                    } else {
                        [l[0].eval(e[0]), l[1].eval(e[1]), l[2].eval(e[2])]
                    };
                    if fp.refine_sat < 1.0 {
                        e = refine_saturation(e0, e, fp.refine_sat);
                    }
                }
                if let Some(l) = &fp.profile_curves {
                    e = if hdr {
                        std::array::from_fn(|k| if e[k] > 1.0 { l[k].eval(1.0) + (e[k] - 1.0) } else { l[k].eval(e[k]) })
                    } else {
                        [l[0].eval(e[0]), l[1].eval(e[1]), l[2].eval(e[2])]
                    };
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
                e = e.map(|v| v + k);
            }
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
            *px = store(warn.unwrap_or(e));
        }
    });
    out
}

/// Apply V2 global curves, their saturation refinement, then imported-profile curves in
/// fixed ProPhoto D50 primaries. Signed and above-peak outputs survive until output gamut mapping;
/// only extreme finite encoded values are bounded to avoid overflow in decoding/matrix arithmetic.
/// HDR retains the slope-one extension above encoded SDR white through both LUT stages.
fn working_curves(d: [f32; 3], fp: &FinishParams, space: CurveSpace, srgb: &[f32; SRGB_LUT_N + 1], exact: bool) -> [f32; 3] {
    let hdr = fp.peak > 1.0;
    let mut e = mul3(&space.to_curve, d).map(|v| {
        if hdr && v > 1.0 {
            linear_to_srgb(v)
        } else if exact {
            linear_to_srgb(v.clamp(0.0, 1.0))
        } else {
            encode_srgb(srgb, v)
        }
    });
    let bound = |v: f32| v.clamp(-CURVE_ENCODED_LIMIT, CURVE_ENCODED_LIMIT);
    let apply =
        |l: &[Lut1; 3], e: [f32; 3]| std::array::from_fn(|k| bound(if hdr && e[k] > 1.0 { l[k].eval(1.0) + (e[k] - 1.0) } else { l[k].eval(e[k]) }));
    if let Some(l) = &fp.curves {
        let before = e;
        e = apply(l, e);
        if fp.refine_sat < 1.0 {
            e = refine_saturation(before, e, fp.refine_sat);
        }
    }
    if let Some(l) = &fp.profile_curves {
        e = apply(l, e);
    }
    mul3(&space.from_curve, e.map(|v| srgb_to_linear(bound(v))))
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

#[inline]
fn enc(v: f32) -> u8 {
    // `v` is already sRGB-encoded; round to 8 bits.
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refine_saturation_restores_the_pre_curve_saturation() {
        let before = [0.5, 0.3, 0.2];
        let after = [0.7, 0.35, 0.15]; // a contrasty curve: more saturated
        assert_eq!(refine_saturation(before, after, 1.0), after);
        let r = refine_saturation(before, after, 0.0);
        let sat = |e: [f32; 3]| (e[0] - e[2]) / (0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2]);
        assert!((sat(r) - sat(before)).abs() < 1e-4, "{r:?}");
        let half = refine_saturation(before, after, 0.5);
        assert!(sat(half) > sat(before) && sat(half) < sat(after));
        // luma is kept
        let y = |e: [f32; 3]| 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2];
        assert!((y(r) - y(after)).abs() < 1e-5);
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
    fn interior_diagonal_curve_clamps_ends_continuously() {
        use lightcraft_geom::Point;
        let mut c = ToneCurve { master: vec![Point::new(0.25, 0.25), Point::new(0.75, 0.75)].into(), ..Default::default() };
        let original = curve_luts(&c, [None; 4]).unwrap();
        for lut in &original {
            assert!((lut.eval(0.0) - 0.25).abs() < 1e-6);
            assert!((lut.eval(0.5) - 0.5).abs() < 1e-6);
            assert!((lut.eval(1.0) - 0.75).abs() < 1e-6);
        }
        c.master = c.master.sparse_points().unwrap().iter().map(|p| Point::new(p.x, p.y + 1e-6)).collect::<Vec<_>>().into();
        let moved = curve_luts(&c, [None; 4]).unwrap();
        for (a, b) in original.iter().zip(moved.iter()) {
            for x in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0] {
                assert!((a.eval(x) - b.eval(x)).abs() < 2e-6);
            }
        }
    }
    #[test]
    fn camera_white_endpoint_does_not_cap_the_upstream_hdr_process() {
        let curve = crate::tone::CameraTone::new(std::array::from_fn(|i| {
            let v = 2f32.powf(-12.0 + 12.0 * i as f32 / 31.0);
            [v, v]
        }))
        .unwrap();
        let info = SourceInfo { raw: true, camera_tone: Some(curve), ..Default::default() };
        for process in [lightcraft_develop::ProcessVersion::V1, lightcraft_develop::ProcessVersion::V2, lightcraft_develop::ProcessVersion::V3] {
            let s = DevelopSettings { process, ..Default::default() };
            let sdr = base_tone(&s, &info, 1.0);
            let expected_sdr = ToneMap::camera(&curve, 0.0, 0.0, 0.0);
            assert_eq!(sdr.lut(), expected_sdr.lut(), "{process:?}: SDR camera curve changed");
            assert_eq!(sdr.saturation_anchor(), expected_sdr.saturation_anchor());
            assert!(sdr.saturation_anchor().is_some());
            assert_eq!(sdr.apply(1.0), 1.0);
            assert_eq!(sdr.apply(16.0), 1.0);
            let hdr = base_tone(&s, &info, 8.0);
            assert!(hdr.saturation_anchor().is_none());
            for y in [0.02, 0.18, 0.4] {
                assert_eq!(hdr.apply(y), sdr.apply(y), "{process:?}: HDR camera midtone changed at {y}");
            }
            let mut previous = hdr.apply(0.99);
            assert!(previous > 1.0, "{process:?}: HDR must lift highlights before the SDR white endpoint");
            for y in [0.999, 1.0, 1.001, 1.01, 2.0, 16.0, 32.0] {
                let output = hdr.apply(y);
                assert!(output >= previous, "{process:?}: HDR is not monotone at {y}: {output} < {previous}");
                assert!(output <= 8.0, "{process:?}: HDR exceeded its peak at {y}");
                previous = output;
            }
            assert_eq!(hdr.apply(32.0), 8.0, "{process:?}: HDR must reach its peak beyond the SDR white endpoint");
        }
    }
    #[test]
    fn blended_functions_render_continuously_and_profile_fades_keep_sparse_compatibility() {
        let a = PointCurve::from(vec![Point::new(0.0, 0.0), Point::new(0.3, 0.15), Point::new(1.0, 1.0)]);
        let b = PointCurve::from(vec![Point::new(0.0, 0.1), Point::new(0.6, 0.85), Point::new(1.0, 0.95)]);
        for amount in [0.25, 0.5, 0.75, 1.5, 2.0] {
            let blended = a.blend(&b, amount).unwrap();
            let c = ToneCurve { master: blended.clone(), ..Default::default() };
            let lut = curve_luts(&c, [None; 4]).unwrap();
            for i in 0..POINT_CURVE_LUT_SIZE {
                let x = i as f64 / (POINT_CURVE_LUT_SIZE - 1) as f64;
                let want = a.eval(x).unwrap() + amount * (b.eval(x).unwrap() - a.eval(x).unwrap());
                assert!((f64::from(lut[0].eval(x as f32)) - want).abs() < 1e-6, "amount{amount},x{x}");
            }
            let faded = point_lut(&blended, Some((0.08, 0.96)));
            for (actual, source) in faded.v.iter().zip(blended.render_lut().v) {
                assert!((f64::from(*actual) - (0.08 + f64::from(source).clamp(0.0, 1.0) * 0.88)).abs() < 1e-7);
            }
        }
        let faded = point_lut(&a, Some((0.08, 0.96)));
        let legacy = MonotoneCurve::new(&[(0.0, 0.08), (0.3, 0.08 + 0.15 * (0.96 - 0.08)), (1.0, 0.96)]).to_lut(POINT_CURVE_LUT_SIZE);
        assert_eq!(faded.v, legacy.v, "legacy sparse profile pixels are unchanged");
        for endpoint in [0.0, 1.0] {
            let curve = a.blend(&b, endpoint).unwrap();
            assert_eq!(curve.render_lut().v, if endpoint == 0.0 { a.render_lut().v } else { b.render_lut().v });
        }
        let mut s = DevelopSettings::default();
        s.profile.id = lightcraft_develop::IMPORTED_LOOK_ID.into();
        s.profile.imported = Some(lightcraft_develop::ImportedLook { master: a.clone(), red: b.clone(), ..Default::default() });
        s.set_section_enabled("light", false);
        s.set_section_enabled("curve", false);
        s.curve.refine_saturation = 0.0;
        for amount in [0.0, 50.0, 100.0, 200.0] {
            s.profile.amount = amount;
            let lut = profile_curve_luts(&s);
            if amount == 0.0 {
                assert!(lut.is_none());
                continue;
            }
            let lut = lut.unwrap();
            let master = a.render_lut();
            let red = b.render_lut();
            for i in 0..POINT_CURVE_LUT_SIZE {
                let x = i as f32 / (POINT_CURVE_LUT_SIZE - 1) as f32;
                let k = amount as f32 / 100.0;
                assert!((lut[0].eval(x) - (x + k * (red.eval(master.v[i]) - x))).abs() < 1e-6);
                assert!((lut[1].eval(x) - (x + k * (master.v[i] - x))).abs() < 1e-6);
                assert_eq!(lut[1].v[i], lut[2].v[i]);
            }
        }
        s.process = lightcraft_develop::ProcessVersion::V1;
        // Verify the V1 finish ordering after a nonidentity global curve and saturation
        // refinement, not merely the construction of the independent profile tables.
        s.set_section_enabled("light", true);
        s.set_section_enabled("curve", true);
        s.curve.master = b;
        let src = lightcraft_raster::Rgb32f::filled(16, 8, [0.1, 0.2, 0.05]);
        let mut req = crate::RenderRequest::fit(16, 8);
        req.depth = OutputDepth::F32Linear;
        let rendered = |settings: &DevelopSettings| {
            let deep = crate::render(&src, &SourceInfo::default(), settings, &req).deep.unwrap();
            let DeepSamples::F32(samples) = deep.samples else { panic!("expected float samples") };
            samples
        };
        s.profile.amount = 0.0;
        let before = rendered(&s);
        s.profile.amount = 100.0;
        let after = rendered(&s);
        let luts = profile_curve_luts(&s).unwrap();
        assert_ne!(before, after);
        for (i, (&before, after)) in before.iter().zip(after).enumerate() {
            let want = lightcraft_color::transfer::srgb_to_linear(luts[i % 3].eval(linear_to_srgb(before)).clamp(0.0, 1.0));
            assert!((after - want).abs() < 1e-6, "profile must follow global curves and refinement: {after} != {want}");
        }
    }
}
