//! Scene-linear preparation: white balance + exposure, and the spatial planes the per-pixel stage
//! needs (edge-aware base layer for highlights/shadows, clarity/texture bands, dehaze veil).

use lightcraft_color::cct::{temp_tint_to_xy, wb_matrix};
use lightcraft_color::{REC2020, luminance_2020};
use lightcraft_develop::DevelopSettings;
use lightcraft_raster::blur::gaussian;
use std::sync::Arc;

use lightcraft_raster::{Plane, Rgb32f, par_join};

use crate::geometry::{Frame, ViewWindow};
use crate::{Plan, Prepared, Quality, SourceInfo, for_rows, masks, timed};

/// White balance (relative to the source's as-shot white) and exposure, in place.
pub fn scene_linear_pre(img: &mut Rgb32f, info: &SourceInfo, s: &DevelopSettings) {
    wb_gain(img, info, s, 2f32.powf(s.light.exposure as f32));
}

/// White balance only (exposure is applied by the per-pixel stage, see [`crate::Prepared`]).
pub fn white_balance(img: &mut Rgb32f, info: &SourceInfo, s: &DevelopSettings) {
    wb_gain(img, info, s, 1.0);
}

/// The white-balance matrix (linear Rec.2020) for the settings, or `None` when the as-shot white
/// is kept. A raw source with its camera colour model ([`crate::CameraColor`]) is re-developed for
/// the new white in camera space, as Lightroom does (so a neutral under the chosen white renders
/// neutral and saturated colours move as the camera's matrices say); other sources are adapted
/// with Bradford, luminance-preserving.
pub fn wb_matrix_for(info: &SourceInfo, s: &DevelopSettings) -> Option<[[f32; 3]; 3]> {
    let (t, tint) = effective_wb(info, s);
    if (t - info.as_shot_temp).abs() < 1e-6 && (tint - info.as_shot_tint).abs() < 1e-6 {
        return None;
    }
    if let Some(cc) = info.camera_color.as_deref().filter(|_| info.raw && !info.relative_wb)
        && let Some(m) = lightcraft_raw::color::rebalance(&cc.tags, cc.developed_for, temp_tint_to_xy(t, tint))
    {
        return Some(m.to_f32());
    }
    let set = wb_matrix(&REC2020, temp_tint_to_xy(t, tint));
    let shot = wb_matrix(&REC2020, temp_tint_to_xy(info.as_shot_temp, info.as_shot_tint));
    let m = set.mul(&shot.inverse().unwrap_or(lightcraft_color::Mat3::IDENTITY));
    // Normalize so neutral luminance is preserved (WB shouldn't change exposure).
    let g = m.apply([1.0, 1.0, 1.0]);
    let y = g[0] * 0.2627 + g[1] * 0.6780 + g[2] * 0.0593;
    Some(m.mul(&lightcraft_color::Mat3::diag(1.0 / y, 1.0 / y, 1.0 / y)).to_f32())
}

fn wb_gain(img: &mut Rgb32f, info: &SourceInfo, s: &DevelopSettings, gain: f32) {
    let m = wb_matrix_for(info, s);
    let w = img.width;
    for_rows(&mut img.data, w, |_, row| {
        for p in row.iter_mut() {
            let mut c = *p;
            if let Some(m) = &m {
                c = [
                    m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
                    m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
                    m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
                ];
            }
            *p = [(c[0] * gain).max(0.0), (c[1] * gain).max(0.0), (c[2] * gain).max(0.0)];
        }
    });
}

/// The white balance actually in effect (presets resolve to their Kelvin values for raw files).
pub fn effective_wb(info: &SourceInfo, s: &DevelopSettings) -> (f64, f64) {
    use lightcraft_develop::WbMode;
    match s.wb.mode {
        WbMode::AsShot => (info.as_shot_temp, info.as_shot_tint),
        m if info.raw && !info.relative_wb => m.preset().unwrap_or((s.wb.temp, s.wb.tint)),
        _ => (s.wb.temp, s.wb.tint),
    }
}

/// Self-guided filter (He et al.) on a single plane with Gaussian windows of `sigma` px.
pub fn guided(p: &Plane, sigma: f32, eps: f32) -> Plane {
    let (ma, mb) = guided_coeffs(p, sigma, eps);
    guided_apply(p, &ma, &mb)
}

/// Blurred guided-filter coefficients (mean a, mean b) of `p`; every pass row-parallel, and the
/// two independent blurs of each step run side by side.
fn guided_coeffs(p: &Plane, sigma: f32, eps: f32) -> (Plane, Plane) {
    let (mean, corr) = par_join(|| gaussian(p, sigma), || gaussian(&p.map(|v| v * v), sigma));
    let a = corr.zip_map(&mean, |c, m| {
        let var = (c - m * m).max(0.0);
        var / (var + eps)
    });
    let b = mean.zip_map(&a, |m, a| m - a * m);
    par_join(|| gaussian(&a, sigma), || gaussian(&b, sigma))
}

/// `q = a·p + b` per pixel.
fn guided_apply(p: &Plane, a: &Plane, b: &Plane) -> Plane {
    let mut q = Plane::new(p.width, p.height);
    let w = p.width;
    for_rows(&mut q.data, w, |y, row| {
        for ((v, pv), (av, bv)) in row.iter_mut().zip(p.row(y)).zip(a.row(y).iter().zip(b.row(y))) {
            *v = av * pv + bv;
        }
    });
    q
}

/// Fast guided filter (He & Sun 2015): coefficients computed on a `s×` subsampled plane and
/// bilinearly upsampled; the output keeps full-resolution edges. Equivalent to [`guided`] for
/// large windows at a fraction of the cost.
pub fn guided_fast(p: &Plane, sigma: f32, eps: f32) -> Plane {
    use lightcraft_raster::resample::{Filter, resize};
    let s = guided_fast_step(sigma);
    if s <= 1 {
        return guided(p, sigma, eps);
    }
    let (lw, lh) = (p.width.div_ceil(s).max(1), p.height.div_ceil(s).max(1));
    let lo = resize(p, lw, lh, Filter::Box);
    let (ma, mb) = guided_coeffs(&lo, sigma / s as f32, eps);
    let up = |c: &Plane| resize(c, p.width, p.height, Filter::Bilinear);
    guided_apply(p, &up(&ma), &up(&mb))
}

/// Subsampling step of [`guided_fast`] for `sigma` (1 = no subsampling: the plain guided filter).
pub fn guided_fast_step(sigma: f32) -> usize {
    (sigma / 3.0).floor().clamp(1.0, 16.0) as usize
}

/// Luminance noise reduction: guided filter of log luminance (`sigma` px, `eps`), blended by `k`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NrLum {
    pub sigma: f32,
    pub eps: f32,
    pub k: f32,
}

/// Colour noise reduction: chromaticity blurred by `sigma` px, mixed in by `t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NrColor {
    pub sigma: f32,
    pub t: f32,
}

/// Noise-reduction parameters at an output long edge of `out_long` px (see [`denoise`]).
pub fn nr_params(s: &DevelopSettings, src_long: usize, out_long: usize) -> (Option<NrLum>, Option<NrColor>) {
    let lum = (s.detail.nr_luminance / 100.0) as f32;
    let col = (s.detail.nr_color / 100.0) as f32;
    let scale = (out_long as f32 / src_long.max(1) as f32).clamp(0.05, 1.0);
    let l = (lum > 0.0).then(|| {
        let detail = (s.detail.nr_detail / 100.0) as f32;
        NrLum { sigma: (1.0 + 2.5 * lum) * scale.max(0.4), eps: 0.002 + lum * lum * 0.25 * (1.0 - 0.7 * detail), k: lum.sqrt() }
    });
    let c = (col > 0.0).then(|| {
        let sigma = (1.5 + 6.0 * col) * scale.max(0.35) * (1.0 + (s.detail.nr_color_smoothness / 100.0) as f32);
        let keep = (s.detail.nr_color_detail / 100.0) as f32 * 0.5;
        NrColor { sigma, t: col * (1.0 - keep) }
    });
    (l, c)
}

/// Noise reduction at output resolution: luminance via an edge-aware self-guided filter on
/// log-luminance, colour by blurring chromaticity (rgb / Y) and re-applying the original luminance.
/// Radii scale with how much the source was downsampled (preview noise is already averaged out).
pub fn denoise(img: &mut Rgb32f, s: &DevelopSettings, src_long: usize, out_long: usize) {
    let (lum, col) = nr_params(s, src_long, out_long);
    let w = img.width;
    if let Some(nr) = lum {
        let _t = crate::profiling().then(std::time::Instant::now);
        let l = img.map(log_lum);
        let f = guided(&l, nr.sigma, nr.eps);
        for_rows(&mut img.data, w, |y, row| {
            for (x, p) in row.iter_mut().enumerate() {
                let i = y * w + x;
                let d = (f.data[i] - l.data[i]) * nr.k;
                let g = d.exp2();
                *p = p.map(|v| v * g);
            }
        });
        if let Some(t) = _t {
            eprintln!("    nr luminance: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        }
    }
    if let Some(nr) = col {
        let _t = crate::profiling().then(std::time::Instant::now);
        let chroma = img.map(|c| {
            let y = luminance_2020(c).max(1e-6);
            [c[0] / y, c[1] / y, c[2] / y]
        });
        let b = gaussian(&chroma, nr.sigma);
        for_rows(&mut img.data, w, |y, row| {
            for (x, p) in row.iter_mut().enumerate() {
                let i = y * w + x;
                let yl = luminance_2020(*p);
                let c0 = chroma.data[i];
                let cb = b.data[i];
                *p = [0, 1, 2].map(|k| ((c0[k] + (cb[k] - c0[k]) * nr.t) * yl).max(0.0));
            }
        });
        if let Some(t) = _t {
            eprintln!("    nr colour: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        }
    }
}

pub fn log_lum(c: [f32; 3]) -> f32 {
    (luminance_2020(c).max(1e-7) / crate::tone::GREY).log2()
}

/// Spatial planes of one (pre-exposure) image, each tagged with the radius it was computed at.
/// `key` identifies the image; see [`crate::StageCache`].
#[derive(Clone, Default)]
pub(crate) struct Planes {
    pub key: u64,
    pub log_l: Option<Arc<Plane>>,
    pub base: Option<(u32, Arc<Plane>)>,
    pub clarity: Option<(u32, Arc<Plane>)>,
    pub texture: Option<(u32, Arc<Plane>)>,
    /// Dark channel and its airlight.
    pub dark: Option<(u32, Arc<Plane>, f32)>,
    /// Blurred chromaticity (local Moiré / Noise).
    pub chroma: Option<(u32, Arc<Rgb32f>)>,
}

/// Reuse `slot` if it was computed at `sigma`, else compute and store it.
fn plane_at(slot: &mut Option<(u32, Arc<Plane>)>, sigma: f32, f: impl FnOnce() -> Plane) -> Arc<Plane> {
    match slot {
        Some((k, p)) if *k == sigma.to_bits() => p.clone(),
        _ => {
            let p = Arc::new(f());
            *slot = Some((sigma.to_bits(), p.clone()));
            p
        }
    }
}

/// Guided-filter epsilons (EV²) of the highlights/shadows base and the clarity band.
pub const BASE_EPS: f32 = 0.35;
pub const CLARITY_EPS: f32 = 0.8;

/// Dark channel of a pixel (dehaze).
#[inline]
pub fn dark_of(c: [f32; 3]) -> f32 {
    c[0].min(c[1]).min(c[2])
}

/// Radius of the local Moiré / colour-noise chromaticity blur, as a fraction of the long edge.
pub const CHROMA_SIGMA: f32 = 0.004;

/// Radii (px) of the spatial planes the settings need (`None` = not needed).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlaneSigmas {
    /// Base for highlights/shadows: edge-aware (fast guided filter, [`BASE_EPS`]), or with
    /// `base_gaussian` the Gaussian neighbourhood Lightroom's Highlights / Shadows read
    /// ([`crate::tone::LR_CONTEXT_SIGMA`]).
    pub base: Option<f32>,
    pub base_gaussian: bool,
    /// Clarity band (fast guided filter, [`CLARITY_EPS`]).
    pub clarity: Option<f32>,
    /// Texture / sharpening band (Gaussian).
    pub texture: Option<f32>,
    /// Dehaze dark channel (Gaussian).
    pub dark: Option<f32>,
    /// Chromaticity blur for local Moiré / colour noise (Gaussian).
    pub chroma: Option<f32>,
}

impl PlaneSigmas {
    /// Cache key of the base plane: its σ, negated for the Gaussian kind.
    pub fn base_key(&self) -> Option<f32> {
        self.base.map(|sg| if self.base_gaussian { -sg } else { sg })
    }
}

/// The planes `s` needs at `px_per_long` output px per unit of the source long edge. `lr_tone`:
/// the tone sliders run as Lightroom applies them ([`crate::finish::lr_tone`]).
pub fn plane_sigmas(s: &DevelopSettings, px_per_long: f64, q: Quality, lr_tone: bool) -> PlaneSigmas {
    let ppl = px_per_long as f32;
    let local_any = |f: fn(&lightcraft_develop::LocalAdjustments) -> f64| s.masks.iter().any(|m| f(&m.adjust) != 0.0);
    let tone_active =
        s.light.highlights != 0.0 || s.light.shadows != 0.0 || s.masks.iter().any(|m| m.adjust.highlights != 0.0 || m.adjust.shadows != 0.0);
    // Edge-aware base at ~1.5% of the long edge (EV² epsilon: edges of > ~0.6 EV are preserved),
    // or Lightroom's Gaussian neighbourhood
    let base = tone_active.then(|| {
        if lr_tone {
            return (crate::tone::LR_CONTEXT_SIGMA * ppl).max(1.0);
        }
        let sigma = (0.015 * ppl).max(1.0);
        if q == Quality::Draft { sigma.min(24.0) } else { sigma }
    });
    let clarity = (s.effects.clarity != 0.0 || local_any(|a| a.clarity)).then(|| (0.012 * ppl).max(1.0));
    // local Noise and Defringe read the fine detail band too
    let texture = (s.effects.texture != 0.0
        || s.detail.sharpen_amount != 0.0
        || local_any(|a| a.texture)
        || local_any(|a| a.sharpness)
        || local_any(|a| a.noise)
        || s.masks.iter().any(|m| m.adjust.defringe > 0.0))
    .then(|| (0.0018 * ppl).max(0.6));
    let dark = (s.effects.dehaze != 0.0 || local_any(|a| a.dehaze)).then(|| (0.02 * ppl).max(1.0));
    let chroma = (local_any(|a| a.moire) || s.masks.iter().any(|m| m.adjust.noise > 0.0)).then(|| (CHROMA_SIGMA * ppl).max(1.0));
    PlaneSigmas { base, base_gaussian: lr_tone, clarity, texture, dark, chroma }
}

/// The spatial planes the per-pixel stage needs for `img` (white-balanced, before exposure),
/// reusing whatever `planes` already holds for it. Missing planes are computed side by side (each
/// one alone scales poorly: the guided filters work on small subsampled grids). On Apple ProRAW a
/// windowed render reads Highlights / Shadows' neighbourhood from the whole frame
/// ([`Plan::frame_context`]).
pub(crate) fn prepare(img: Arc<Rgb32f>, plan: &Plan<'_>, q: Quality, planes: &mut Planes) -> Prepared {
    let (s, frame, px_per_long, mattes) = (&*plan.settings, &plan.frame, plan.px_per_long, plan.mattes.as_deref());
    let log_l = planes.log_l.get_or_insert_with(|| Arc::new(timed("log_l", || img.map(log_lum)))).clone();
    let sigmas = plane_sigmas(s, px_per_long, q, plan.lr_tone);
    let base_key = sigmas.base_key();
    let PlaneSigmas { base: base_sigma, base_gaussian, clarity: clarity_sigma, texture: texture_sigma, dark: dark_sigma, chroma: chroma_sigma } =
        sigmas;
    let whole = plan.frame_context.as_ref().zip(frame.view).filter(|_| base_gaussian);

    let Planes { base: sb, clarity: sc, texture: st, dark: sd, chroma: sch, .. } = planes;
    let chroma_blur = chroma_sigma.map(|sg| match sch {
        Some((k, c)) if *k == sg.to_bits() => c.clone(),
        _ => {
            let c = timed("chroma", || Arc::new(gaussian(&img.map(masks::chromaticity), sg)));
            *sch = Some((sg.to_bits(), c.clone()));
            c
        }
    });
    let l = &log_l;
    let (base, (clarity_blur, (texture_blur, dark))) = par_join(
        || {
            base_sigma.zip(base_key).map(|(sg, key)| {
                plane_at(sb, key, || {
                    timed("base", || match whole {
                        Some((ctx, view)) => context_window(ctx, view, img.width, img.height),
                        None if base_gaussian => gaussian(l, sg),
                        None => guided_fast(l, sg, BASE_EPS),
                    })
                })
            })
        },
        || {
            par_join(
                || clarity_sigma.map(|sg| plane_at(sc, sg, || timed("clarity", || guided_fast(l, sg, CLARITY_EPS)))),
                || {
                    par_join(
                        || texture_sigma.map(|sg| plane_at(st, sg, || timed("texture", || gaussian(l, sg)))),
                        || {
                            dark_sigma.map(|sg| match sd {
                                Some((k, d, air)) if *k == sg.to_bits() => (d.clone(), *air),
                                _ => {
                                    let d = timed("dehaze", || Arc::new(gaussian(&img.map(dark_of), sg)));
                                    let air = airlight(&d);
                                    *sd = Some((sg.to_bits(), d.clone(), air));
                                    (d, air)
                                }
                            })
                        },
                    )
                },
            )
        },
    );
    let base = base.unwrap_or_else(|| log_l.clone());
    let (dark, air) = match dark {
        Some((d, air)) => (Some(d), air),
        None => (None, 1.0),
    };
    let ev = s.light.exposure as f32;
    let masks = timed("masks", || masks::evaluate(&s.masks, frame, img.width, img.height, &img, &log_l, ev, mattes));
    Prepared { img, log_l, base, clarity_blur, texture_blur, dark, chroma_blur, air, masks, px_per_long }
}

/// The airlight of the whole output frame, estimated on a small render of it (`proxy_w` px wide):
/// a windowed render must dehaze with the frame's airlight, not that of the pixels it holds.
pub(crate) fn frame_airlight(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, frame: &Frame, proxy_w: usize, proxy_h: usize) -> f32 {
    let mut img = frame.sample(src, proxy_w, proxy_h);
    white_balance(&mut img, info, s);
    let sigma = (0.02 * frame.px_per_long(proxy_w)).max(1.0) as f32;
    airlight(&gaussian(&img.map(dark_of), sigma))
}

/// The whole output frame's neighbourhood for Lightroom's Highlights / Shadows on Apple ProRAW
/// (log luminance blurred by [`crate::tone::LR_CONTEXT_SIGMA`] of the long edge, as the base plane
/// of a whole render), estimated on a small render of it (`proxy_w` × `proxy_h`): its σ spans far
/// more than a zoomed window holds, so a windowed render reads it from here ([`context_window`]).
pub(crate) fn frame_context(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, frame: &Frame, proxy_w: usize, proxy_h: usize) -> Plane {
    let mut img = frame.sample(src, proxy_w, proxy_h);
    white_balance(&mut img, info, s);
    let sigma = (crate::tone::LR_CONTEXT_SIGMA as f64 * frame.px_per_long(proxy_w)).max(1.0) as f32;
    gaussian(&img.map(log_lum), sigma)
}

/// [`frame_context`] at the pixels of a `w` × `h` window at `view` of the whole output (bilinear,
/// clamped to the frame).
pub fn context_window(ctx: &Plane, view: ViewWindow, w: usize, h: usize) -> Plane {
    let (cw, ch) = (ctx.width, ctx.height);
    if cw == 0 || ch == 0 || ctx.data.len() < cw * ch {
        return Plane { width: w, height: h, data: vec![0.0; w * h] };
    }
    let (kx, ky) = (cw as f64 / view.full_w.max(1.0), ch as f64 / view.full_h.max(1.0));
    // proxy coordinate of a window pixel: its integer part and fraction
    let at = |o: f64, i: usize, k: f64, n: usize| {
        let f = ((o + i as f64 + 0.5) * k - 0.5).clamp(0.0, (n - 1) as f64);
        let i0 = (f as usize).min(n - 1);
        (i0, (i0 + 1).min(n - 1), (f - i0 as f64) as f32)
    };
    let cols: Vec<(usize, usize, f32)> = (0..w).map(|x| at(view.x, x, kx, cw)).collect();
    let mut data = vec![0.0f32; w * h];
    for_rows(&mut data, w, |y, row| {
        let (y0, y1, ty) = at(view.y, y, ky, ch);
        let (r0, r1) = (&ctx.data[y0 * cw..(y0 + 1) * cw], &ctx.data[y1 * cw..(y1 + 1) * cw]);
        for (o, &(x0, x1, tx)) in row.iter_mut().zip(&cols) {
            let top = r0[x0] + (r0[x1] - r0[x0]) * tx;
            let bottom = r1[x0] + (r1[x1] - r1[x0]) * tx;
            *o = top + (bottom - top) * ty;
        }
    });
    Plane { width: w, height: h, data }
}

/// The airlight is estimated from every `AIRLIGHT_STEP`-th value of the dark channel.
pub const AIRLIGHT_STEP: usize = 7;

/// Airlight estimate: bright end of the dark channel.
pub fn airlight(dark: &Plane) -> f32 {
    airlight_of(dark.data.iter().step_by(AIRLIGHT_STEP).copied().collect())
}

/// Airlight from the sampled dark-channel values (see [`airlight`]): their 99.5th percentile.
pub fn airlight_of(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 1.0;
    }
    let k = ((v.len() as f32) * 0.995) as usize;
    let k = k.min(v.len() - 1);
    v.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    v[k].max(0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guided_preserves_step_edges_and_flattens_texture() {
        let p = Plane::from_fn(80, 20, |x, y| if x < 40 { -2.0 } else { 2.0 } + if (x + y) % 2 == 0 { 0.05 } else { -0.05 });
        let q = guided(&p, 4.0, 0.3);
        // edge kept
        assert!(q.get(35, 10) < -1.5 && q.get(45, 10) > 1.5);
        // checkerboard texture removed away from the edge
        assert!((q.get(10, 10) - q.get(11, 10)).abs() < 0.02);
    }

    #[test]
    fn wb_as_shot_is_identity_and_warming_adds_red() {
        let s = DevelopSettings::default();
        let info = SourceInfo::default();
        let mut img = Rgb32f::filled(4, 4, [0.3, 0.3, 0.3]);
        scene_linear_pre(&mut img, &info, &s);
        assert!((img.get(0, 0)[0] - 0.3).abs() < 1e-6);
        let mut warm = s.clone();
        warm.wb.mode = lightcraft_develop::WbMode::Custom;
        warm.wb.temp = 9000.0;
        let mut img2 = Rgb32f::filled(4, 4, [0.3, 0.3, 0.3]);
        scene_linear_pre(&mut img2, &info, &warm);
        let c = img2.get(0, 0);
        assert!(c[0] > c[2], "{c:?}");
        // luminance preserved
        assert!((luminance_2020(c) - 0.3).abs() < 0.01);
    }

    #[test]
    fn exposure_doubles() {
        let mut s = DevelopSettings::default();
        s.light.exposure = 1.0;
        let mut img = Rgb32f::filled(2, 2, [0.1, 0.2, 0.3]);
        scene_linear_pre(&mut img, &SourceInfo::default(), &s);
        assert!((img.get(1, 1)[2] - 0.6).abs() < 1e-5);
    }
}

#[cfg(test)]
mod fast_tests {
    use super::*;

    #[test]
    fn fast_guided_close_to_reference() {
        let p = Plane::from_fn(160, 120, |x, y| ((x as f32 * 0.13).sin() + (y as f32 * 0.07).cos()) * 2.0 + if x > 80 { 1.5 } else { 0.0 });
        let a = guided(&p, 12.0, 0.3);
        let b = guided_fast(&p, 12.0, 0.3);
        let err: f32 = a.data.iter().zip(&b.data).map(|(u, v)| (u - v).abs()).sum::<f32>() / a.len() as f32;
        assert!(err < 0.08, "{err}");
    }
}

#[cfg(test)]
mod nr_tests {
    use super::*;

    #[test]
    fn luminance_nr_reduces_noise_keeps_mean() {
        let mut img = Rgb32f::from_fn(64, 64, |x, y| {
            let n = (((x * 7919 + y * 104729) % 97) as f32 / 97.0 - 0.5) * 0.06;
            [0.2 + n; 3]
        });
        let var = |im: &Rgb32f| {
            let m = im.data.iter().map(|p| p[1]).sum::<f32>() / im.len() as f32;
            (im.data.iter().map(|p| (p[1] - m).powi(2)).sum::<f32>() / im.len() as f32, m)
        };
        let (v0, m0) = var(&img);
        let mut s = DevelopSettings::default();
        s.detail.nr_luminance = 80.0;
        denoise(&mut img, &s, 64, 64);
        let (v1, m1) = var(&img);
        assert!(v1 < v0 * 0.5, "{v0} -> {v1}");
        assert!((m1 - m0).abs() < 0.01);
    }
}
