//! Scene-linear preparation: white balance + exposure, and the spatial planes the per-pixel stage
//! needs (edge-aware base layer for highlights/shadows, clarity/texture bands, dehaze veil).

use lightcraft_color::cct::{temp_tint_to_xy, wb_matrix, xy_to_temp_tint};
use lightcraft_color::{REC2020, luminance_2020};
use lightcraft_develop::{DevelopSettings, Process};
use lightcraft_raster::blur::gaussian;
use std::sync::Arc;

use lightcraft_raster::{Plane, Rgb32f, par_join};

use crate::geometry::Frame;
use crate::{Prepared, Quality, SourceInfo, for_rows, masks, timed};

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
    let set = wb_matrix(&REC2020, effective_white(info, s));
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

/// The white balance actually in effect (presets resolve to their Kelvin values for raw files; settings written on
/// the relative scale before a raw's camera matrices reached its white balance give the same colour change on the
/// Kelvin scale, see [`legacy_to_kelvin`]).
pub fn effective_wb(info: &SourceInfo, s: &DevelopSettings) -> (f64, f64) {
    use lightcraft_develop::WbMode;
    match s.wb.mode {
        WbMode::AsShot => (info.as_shot_temp, info.as_shot_tint),
        _ if legacy_wb(info, s) => legacy_to_kelvin(s.wb.temp, s.wb.tint, (info.as_shot_temp, info.as_shot_tint)),
        m if info.raw && !info.relative_wb => m.preset().unwrap_or((s.wb.temp, s.wb.tint)),
        _ => (s.wb.temp, s.wb.tint),
    }
}

/// The source white the render adapts from the as-shot white to: [`effective_wb`] as a chromaticity, except that a
/// white balance still on the relative scale is composed exactly ([`legacy_white`]) rather than read back through
/// its rounded Kelvin / tint.
fn effective_white(info: &SourceInfo, s: &DevelopSettings) -> lightcraft_color::Xy {
    if legacy_wb(info, s)
        && s.wb.mode != lightcraft_develop::WbMode::AsShot
        && let Some(xy) = legacy_white(s.wb.temp, s.wb.tint, (info.as_shot_temp, info.as_shot_tint))
    {
        return xy;
    }
    let (t, tint) = effective_wb(info, s);
    temp_tint_to_xy(t, tint)
}

/// Whether `s` holds a white balance written on the relative scale for a source that is edited in Kelvin today.
fn legacy_wb(info: &SourceInfo, s: &DevelopSettings) -> bool {
    info.raw && !info.relative_wb && info.legacy_relative_wb && s.wb.scale == lightcraft_develop::WbScale::Legacy
}

/// The white that renders exactly as `(temp, tint)` did on the relative scale, where 6500 K / 0 meant *as shot* and
/// the source's real as-shot white is `as_shot`.
///
/// The relative render adapted the picture from 6500 K / 0 to `(temp, tint)` (Bradford, [`wb_matrix_for`]); the
/// Kelvin render adapts it from `as_shot` to the result. Bradford adaptations are von Kries scalings in one cone
/// space, so they compose exactly: the new white's cone response is the old target's times the as-shot white's
/// over the old reference's, and the two renders give the same matrix (up to the scale the luminance
/// normalisation removes). `None` when the arithmetic has nowhere to go (a non-finite input, a white outside the
/// chromaticity plane).
pub fn legacy_white(temp: f64, tint: f64, as_shot: (f64, f64)) -> Option<lightcraft_color::Xy> {
    use lightcraft_color::{BRADFORD, Xy};
    if !(temp.is_finite() && tint.is_finite() && as_shot.0.is_finite() && as_shot.1.is_finite()) {
        return None;
    }
    let cones = |t: f64, tint: f64| BRADFORD.apply(temp_tint_to_xy(t, tint).to_xyz());
    let (old, reference, shot) = (cones(temp, tint), cones(6500.0, 0.0), cones(as_shot.0, as_shot.1));
    let inverse = BRADFORD.inverse()?;
    let mut new = [0.0; 3];
    for i in 0..3 {
        if reference[i].abs() < 1e-12 {
            return None;
        }
        new[i] = old[i] * shot[i] / reference[i];
    }
    let xy = Xy::from_xyz(inverse.apply(new));
    (xy.x.is_finite() && xy.y.is_finite() && xy.y > 0.0).then_some(xy)
}

/// The Kelvin / tint of [`legacy_white`]: what a white balance written on the relative scale reads as, and becomes
/// when it is next edited ([`normalize_wb`]). Exact wherever the white has a Kelvin value (1000–50000 K after the
/// composition; the relative slider reached 2000–13550 K around 6500 K, so only a far-blue as-shot white pushed
/// to the slider's blue end lands beyond, and clamps), to the precision of the Kelvin / tint parametrisation
/// (better than 0.01 % away from its 4000 K seam). Unchanged when the arithmetic has nowhere to go.
pub fn legacy_to_kelvin(temp: f64, tint: f64, as_shot: (f64, f64)) -> (f64, f64) {
    if temp == 6500.0 && tint == 0.0 && as_shot.0.is_finite() && as_shot.1.is_finite() {
        // the relative scale's as-shot reference, exactly (a Custom value left where As Shot was stays as shot)
        return as_shot;
    }
    match legacy_white(temp, tint, as_shot).map(xy_to_temp_tint) {
        Some((t, tint)) if t.is_finite() && tint.is_finite() => (t, tint),
        _ => (temp, tint),
    }
}

/// Bring the white balance of `d` onto the scale `info` is edited on, before a new value is written into it: on a
/// raw with a Kelvin scale the values are marked Kelvin, and ones still on the relative scale become the Custom
/// Kelvin / tint that renders the same ([`legacy_to_kelvin`]; a preset mode's look is kept as that custom
/// value, since the preset's Kelvin value would be a different look). Elsewhere the scale is cleared, so values
/// edited on a rendered photo or a raw without matrices never pass for Kelvin when pasted onto one that has them.
pub fn normalize_wb(d: &mut DevelopSettings, info: &SourceInfo) {
    use lightcraft_develop::{WbMode, WbScale};
    if !info.raw || info.relative_wb {
        d.wb.scale = WbScale::Legacy;
        return;
    }
    if legacy_wb(info, d) && d.wb.mode != WbMode::AsShot {
        let (t, tint) = legacy_to_kelvin(d.wb.temp, d.wb.tint, (info.as_shot_temp, info.as_shot_tint));
        d.wb.temp = t;
        d.wb.tint = tint;
        d.wb.mode = WbMode::Custom;
    }
    d.wb.scale = WbScale::Kelvin;
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

/// `q = a·p + b` per pixel. Reduced coefficients are sampled with the same separable
/// weights and accumulation order as a bilinear resize, without full-size coefficient planes.
fn guided_apply(p: &Plane, a: &Plane, b: &Plane) -> Plane {
    let mut q = Plane::new(p.width, p.height);
    let w = p.width;
    if (a.width, a.height) != (p.width, p.height) {
        use lightcraft_raster::resample::{Filter, weights};
        let wx = weights(a.width, w, Filter::Bilinear);
        let wy = weights(a.height, p.height, Filter::Bilinear);
        // Bilinear upsampling has at most four taps (including zero-weight boundary taps).
        // Keep only those horizontally sampled rows per band, reusing them for neighbouring
        // output rows. Scratch scales with width and active bands, not image area.
        const BAND: usize = 32;
        for_rows(&mut q.data, w.saturating_mul(BAND), |band, rows| {
            let mut cache: [(usize, Vec<f32>, Vec<f32>); 4] = std::array::from_fn(|_| (usize::MAX, vec![0.0; w], vec![0.0; w]));
            let mut bias = vec![0.0; w];
            for (dy, row) in rows.chunks_mut(w).enumerate() {
                let y = band * BAND + dy;
                let (top, ys) = &wy[y];
                bias.fill(0.0);
                for (dy, yw) in ys.iter().enumerate() {
                    let cy = top + dy;
                    let (cached_y, aa, bb) = &mut cache[cy % 4];
                    if *cached_y != cy {
                        let (ar, br) = (a.row(cy), b.row(cy));
                        for ((av, bv), (left, xs)) in aa.iter_mut().zip(bb.iter_mut()).zip(&wx) {
                            *av = 0.0;
                            *bv = 0.0;
                            for (dx, xw) in xs.iter().enumerate() {
                                *av += ar[left + dx] * xw;
                                *bv += br[left + dx] * xw;
                            }
                        }
                        *cached_y = cy;
                    }
                    for (av, ax) in row.iter_mut().zip(aa) {
                        *av += *ax * yw;
                    }
                    for (bv, bx) in bias.iter_mut().zip(bb) {
                        *bv += *bx * yw;
                    }
                }
                for ((v, pv), bv) in row.iter_mut().zip(p.row(y)).zip(&bias) {
                    *v = *v * pv + bv;
                }
            }
        });
        return q;
    }
    for_rows(&mut q.data, w, |y, row| {
        for ((v, pv), (av, bv)) in row.iter_mut().zip(p.row(y)).zip(a.row(y).iter().zip(b.row(y))) {
            *v = av * pv + bv;
        }
    });
    q
}

/// Fast guided filter (He & Sun 2015): coefficients computed on a `s×` subsampled plane and
/// sampled bilinearly at full resolution; the output keeps full-resolution edges. Equivalent to [`guided`] for
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
    guided_apply(p, &ma, &mb)
}

/// Separable median (`2r+1` px along each row, then each column; clamped edges). It keeps the
/// steps between regions where they are and removes features thinner than about `r` px.
pub fn median_hv(p: &Plane, r: usize) -> Plane {
    if r == 0 || p.data.is_empty() {
        return p.clone();
    }
    let rows = |src: &Plane| {
        let mut out = Plane::new(src.width, src.height);
        for_rows(&mut out.data, src.width, |y, row| median_row(src.row(y), row, r));
        out
    };
    transpose(&rows(&transpose(&rows(p))))
}

/// Running median of `src` over `2r+1` taps (clamped edges) into `out`: a sorted window that
/// drops the leaving value and inserts the entering one.
fn median_row(src: &[f32], out: &mut [f32], r: usize) {
    let n = src.len();
    if n == 0 {
        return;
    }
    let at = |i: isize| total_key(src[i.clamp(0, n as isize - 1) as usize]);
    let r_ = r as isize;
    let mut win: Vec<i32> = (-r_..=r_).map(at).collect();
    win.sort_unstable();
    for (x, o) in out.iter_mut().enumerate() {
        *o = f32::from_bits(total_key_inv(win[r]));
        let (old, new) = (at(x as isize - r_), at(x as isize + r_ + 1));
        if old != new {
            // Ranks by counting the smaller keys in one branch-free pass the compiler vectorizes.
            // Binary searches here mispredicted on most steps and, with `remove` + `insert`, cost
            // 2.4 s per 24 MP at r = 9 on one core.
            let (mut i, mut j) = (0u32, 0u32);
            for &v in &win {
                i += u32::from(v < old);
                j += u32::from(v < new);
            }
            let (i, j) = (i as usize, j as usize);
            // `old` sits at `i`; shift the keys between the two ranks by one over it
            if j > i {
                win.copy_within(i + 1..j, i);
                win[j - 1] = new;
            } else {
                win.copy_within(j..i, j + 1);
                win[j] = new;
            }
        }
    }
}

/// `v`'s bits as an `i32` that orders like [`f32::total_cmp`] (the same transform it applies), so
/// equal keys are equal bits.
fn total_key(v: f32) -> i32 {
    let b = v.to_bits() as i32;
    b ^ (((b >> 31) as u32) >> 1) as i32
}

/// The bits [`total_key`] came from (the transform is its own inverse: it keeps the sign bit).
fn total_key_inv(k: i32) -> u32 {
    (k ^ (((k >> 31) as u32) >> 1) as i32) as u32
}

fn transpose(p: &Plane) -> Plane {
    let (w, h) = (p.width, p.height);
    let mut t = Plane::new(h, w);
    for_rows(&mut t.data, h, |x, row| {
        for (y, v) in row.iter_mut().enumerate() {
            *v = p.data[y * w + x];
        }
    });
    t
}

/// Detail scale of the highlights/shadows base from process V2, as a fraction of the long edge:
/// the base is the edge-aware filter of the [`median_hv`] of log luminance at this radius, so
/// twigs, lines and fine texture brighter or darker than their surroundings take their
/// surroundings' shift (and keep their contrast, as with Exposure) while region boundaries stay
/// sharp (no halos). V1's base is the edge-aware filter of log luminance itself (issue #632).
pub const BASE_DETAIL: f64 = 0.0015;
/// Largest [`base_detail_radius`] (px).
pub const BASE_DETAIL_MAX: usize = 24;

/// Median radius (px) of the highlights/shadows base at `px_per_long` (see [`BASE_DETAIL`]).
pub fn base_detail_radius(px_per_long: f64) -> usize {
    ((BASE_DETAIL * px_per_long).round().max(0.0) as usize).min(BASE_DETAIL_MAX)
}

/// Median radius (px) of the highlights/shadows base of `s`'s rendering process at `px_per_long`:
/// 0 (no median) under V1, [`base_detail_radius`] from V2.
pub fn tone_base_detail(s: &DevelopSettings, px_per_long: f64) -> usize {
    match s.process.process() {
        Process::V1 => 0,
        Process::V2 | Process::V3 | Process::V4 => base_detail_radius(px_per_long),
    }
}

/// The highlights/shadows base layer of log luminance `l`: the fast guided filter ([`BASE_EPS`])
/// of its [`median_hv`] at `detail` px ([`tone_base_detail`]); at 0, of `l` itself (V1).
pub fn tone_base(l: &Plane, sigma: f32, detail: usize) -> Plane {
    if detail == 0 {
        return guided_fast(l, sigma, BASE_EPS);
    }
    guided_fast(&median_hv(l, detail), sigma, BASE_EPS)
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

/// Output pixels per pixel of the original photo for an output of `px_per_long` pixels per unit
/// of the long edge of an original `native_long` pixels long, at most 1 ([`crate::Plan::detail_scale`]).
pub fn detail_scale(px_per_long: f64, native_long: usize) -> f32 {
    let k = (px_per_long / native_long.max(1) as f64) as f32;
    if k.is_finite() { k.clamp(0.0, 1.0) } else { 1.0 }
}

/// Smallest radius (px) noise reduction filters a preview with: [`gaussian`] makes no blur below
/// about 0.58 px and one 3-px box from there ([`lightcraft_raster::blur::box_sigma`]).
const NR_MIN_SIGMA: f32 = 1.0;

/// Below this fade a preview's noise reduction is not drawn at all (less than 1 % of a 1-px blur).
const NR_MIN_BLEND: f32 = 0.01;

/// Radius and blend weight that stand in for a blur of `sigma` px: below [`NR_MIN_SIGMA`] the
/// smallest blur the filter makes, blended in by the ratio of their variances, which keeps the
/// smoothing's second moment (how much it softens the finest detail) that of `sigma`.
fn nr_radius(sigma: f32) -> (f32, f32) {
    if sigma >= NR_MIN_SIGMA {
        return (sigma, 1.0);
    }
    let made = lightcraft_raster::blur::box_sigma(NR_MIN_SIGMA);
    (NR_MIN_SIGMA, (sigma / made).powi(2).min(1.0))
}

/// Noise-reduction parameters at `scale` output pixels per pixel of the original
/// ([`crate::Plan::detail_scale`]; see [`denoise`]). Noise lives at the original's pixels, so the
/// radii are the full-size radii times `scale`, and the guided filter's `eps` (the variance it
/// treats as noise) follows the noise variance, which averaging `1/scale × 1/scale` pixels into one
/// multiplies by `scale²`. A radius below [`NR_MIN_SIGMA`] fades instead ([`nr_radius`]): previews look like
/// the full-size render scaled down (issue #960). At `scale` 1 these are the full-size parameters.
pub fn nr_params(s: &DevelopSettings, scale: f32) -> (Option<NrLum>, Option<NrColor>) {
    let lum = (s.detail.nr_luminance / 100.0) as f32;
    let col = (s.detail.nr_color / 100.0) as f32;
    let scale = if scale.is_finite() { scale.clamp(0.0, 1.0) } else { 1.0 };
    let l = (lum > 0.0).then(|| nr_radius((1.0 + 2.5 * lum) * scale)).filter(|&(_, w)| w >= NR_MIN_BLEND).map(|(sigma, w)| {
        let detail = (s.detail.nr_detail / 100.0) as f32;
        let eps = ((0.002 + lum * lum * 0.25 * (1.0 - 0.7 * detail)) * scale * scale).max(1e-7);
        NrLum { sigma, eps, k: lum.sqrt() * w }
    });
    let smooth = 1.0 + (s.detail.nr_color_smoothness / 100.0) as f32;
    let c = (col > 0.0).then(|| nr_radius((1.5 + 6.0 * col) * scale * smooth)).filter(|&(_, w)| w >= NR_MIN_BLEND).map(|(sigma, w)| {
        let keep = (s.detail.nr_color_detail / 100.0) as f32 * 0.5;
        NrColor { sigma, t: col * (1.0 - keep) * w }
    });
    (l, c)
}

/// Noise reduction at output resolution: luminance via an edge-aware self-guided filter on
/// log-luminance, colour by blurring chromaticity (rgb / Y) and re-applying the original luminance.
/// `scale`: output pixels per pixel of the original ([`nr_params`]).
/// `lum_stats`: the statistics of the wavelet luminance NR from the whole frame (`None`: from `img`).
/// `colour_noise`: the joint colour NR's noise model of the whole frame ([`crate::colour_noise`];
/// `None`: measured on `img`).
pub fn denoise(
    img: &mut Rgb32f,
    s: &DevelopSettings,
    scale: f32,
    lum_stats: Option<&crate::nrl::Stats>,
    colour_noise: Option<&crate::nrc_joint::JointNoise>,
) {
    let y = denoise_colour(img, s, scale, colour_noise);
    denoise_luminance(img, s, scale, lum_stats, y);
}

/// Whether the luminance NR runs as the wavelet filter after the colour NR ([`denoise_luminance`])
/// rather than as the old filter before it.
pub fn wavelet_luminance_nr(s: &DevelopSettings, scale: f32) -> bool {
    nr_params(s, scale).0.is_some() && wavelet_luminance(s)
}

/// The luminance NR that runs before the colour NR: main's guided filter on log luminance, unless the
/// wavelet luminance NR runs after the colour NR instead ([`wavelet_luminance_nr`]).
pub fn luminance_before_colour(img: &mut Rgb32f, s: &DevelopSettings, scale: f32) {
    let Some(nr) = nr_params(s, scale).0.filter(|_| !wavelet_luminance_nr(s, scale)) else { return };
    let w = img.width;
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

/// The first part of [`denoise`]: everything but the wavelet luminance NR. Its result does not depend
/// on Luminance, Detail and Contrast when [`wavelet_luminance_nr`] holds, so a render can keep it while
/// those sliders move. When the joint colour NR ran and the wavelet luminance NR follows, the luminance
/// of the result (that NR's input, made as the colour NR writes each row). `colour_noise`: see [`denoise`].
pub fn denoise_colour(img: &mut Rgb32f, s: &DevelopSettings, scale: f32, colour_noise: Option<&crate::nrc_joint::JointNoise>) -> Option<Plane> {
    let (_, col) = nr_params(s, scale);
    let w = img.width;
    // From process V4 the wavelet luminance NR runs after the colour NR, which leaves Y unchanged.
    let new_lum = wavelet_luminance_nr(s, scale);
    luminance_before_colour(img, s, scale);
    if col.is_some() && joint_colour_nr(s) {
        let p = nrc_params(s, scale)?;
        return crate::nrc_joint::denoise(
            img,
            &p,
            (s.detail.nr_color_detail / 100.0) as f32,
            (s.detail.nr_color / 100.0) as f32,
            new_lum,
            colour_noise,
        );
    } else if let Some(nr) = col {
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
    None
}

/// The second part of [`denoise`]: the wavelet luminance NR (when [`wavelet_luminance_nr`]).
/// `y`: the luminance of `img` when the caller has it ([`denoise_colour`]'s result), else it is computed.
pub fn denoise_luminance(img: &mut Rgb32f, s: &DevelopSettings, scale: f32, lum_stats: Option<&crate::nrl::Stats>, y: Option<Plane>) {
    if wavelet_luminance_nr(s, scale) {
        let d = &s.detail;
        let p = crate::nrl::Params {
            amount: (d.nr_luminance / 100.0) as f32,
            detail: (d.nr_detail / 100.0) as f32,
            contrast: (d.nr_contrast / 100.0) as f32,
            scale,
        };
        crate::nrl::denoise_y(img, &p, lum_stats, y);
    }
}

/// Whether `s` gets the wavelet luminance noise reduction (`nrl`, process V4 on), else main's guided
/// filter on log luminance.
pub fn wavelet_luminance(s: &DevelopSettings) -> bool {
    s.process.process() >= lightcraft_develop::Process::V4
}

/// Most samples the noise level of the new colour noise reduction is measured from. The CPU and
/// the GPU sample the same pixels (see `nrc_joint::noise_sample_stride`).
pub const NRC_SAMPLES: usize = 131_072;

/// One self-guided pass of the new colour noise reduction: Gaussian windows of `sigma` px, the
/// coefficients computed on a plane subsampled by `step`, `eps = k ×` the `pct`-quantile of the
/// local variance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NrcPass {
    pub sigma: f32,
    pub step: usize,
    pub k: f32,
    pub pct: f32,
}

/// Parameters of the new colour noise reduction: the offset of the square roots and the passes
/// (fine, then coarse).
#[derive(Clone, Debug, PartialEq)]
pub struct NrcParams {
    pub off: f32,
    pub passes: Vec<NrcPass>,
}

/// The new colour noise reduction's parameters at `scale` output pixels per pixel of the original
/// ([`crate::Plan::detail_scale`]), `None` when Color is 0.
pub fn nrc_params(s: &DevelopSettings, scale: f32) -> Option<NrcParams> {
    let amount = (s.detail.nr_color / 100.0) as f32;
    if amount <= 0.0 {
        return None;
    }
    let detail = (s.detail.nr_color_detail / 100.0) as f32;
    let smooth = 1.0 + (s.detail.nr_color_smoothness / 100.0) as f32;
    let scale = if scale.is_finite() { scale.clamp(0.05, 1.0) } else { 1.0 };
    let sigma = (2.0 + 10.0 * amount) * smooth * scale;
    let k = 64.0 * amount * (1.5 - detail);
    // the coefficients vary over about sigma: a grid of sigma / 2 holds them
    let pass = |sigma: f32| NrcPass { sigma, step: (sigma / 2.0).floor().clamp(1.0, 16.0) as usize, k, pct: 0.2 };
    // fine, then coarse (three times wider); the joint method takes the coarse one's reach
    Some(NrcParams { off: 0.003, passes: vec![pass(sigma), pass(sigma * 3.0)] })
}

/// Chroma noise variance per channel as a function of the local luminance: `α · Y + β` (shot plus read noise).
#[derive(Clone, Copy, Debug)]
pub struct NrcNoise {
    pub alpha: [f32; 3],
    pub beta: [f32; 3],
}

impl NrcNoise {
    #[inline]
    pub fn var(&self, i: usize, y: f32) -> f32 {
        (self.alpha[i] * y.max(0.0) + self.beta[i]).max(1e-14)
    }
}

/// Weighted least squares `v = a · l + b` over `(l, v, weight)` with `a, b ≥ 0`: the free fit if both come out
/// non-negative, else the better of the line through the origin (`b = 0`) and the constant (`a = 0`).
pub fn fit_nonneg_line(pts: &[(f64, f64, f64)]) -> (f64, f64) {
    let sw: f64 = pts.iter().map(|p| p.2).sum::<f64>().max(1e-300);
    let (ml, mv) = (pts.iter().map(|p| p.2 * p.0).sum::<f64>() / sw, pts.iter().map(|p| p.2 * p.1).sum::<f64>() / sw);
    let (cov, vl) = pts.iter().fold((0.0, 0.0), |(c, v), p| (c + p.2 * (p.0 - ml) * (p.1 - mv), v + p.2 * (p.0 - ml) * (p.0 - ml)));
    if vl > 0.0 {
        let a = cov / vl;
        let b = mv - a * ml;
        if a >= 0.0 && b >= 0.0 {
            return (a, b);
        }
    }
    let sse = |a: f64, b: f64| pts.iter().map(|p| p.2 * (p.1 - a * p.0 - b).powi(2)).sum::<f64>();
    let ll: f64 = pts.iter().map(|p| p.2 * p.0 * p.0).sum();
    let origin = if ll > 0.0 { (pts.iter().map(|p| p.2 * p.0 * p.1).sum::<f64>() / ll).max(0.0) } else { 0.0 };
    let constant = mv.max(0.0);
    if sse(origin, 0.0) < sse(0.0, constant) { (origin, 0.0) } else { (0.0, constant) }
}

/// Whether `s` gets the joint colour noise reduction (`nrc_joint`, process V3 on), else main's
/// blurred chromaticity.
pub fn joint_colour_nr(s: &DevelopSettings) -> bool {
    s.process.process() >= lightcraft_develop::Process::V3
}

pub fn log_lum(c: [f32; 3]) -> f32 {
    (luminance_2020(c).max(1e-7) / crate::tone::GREY).log2()
}

/// Spatial planes of one (pre-exposure) image, each tagged with the radius it was computed at.
/// `key` identifies the image; see [`crate::StageCache`].
#[derive(Clone, Default)]
pub(crate) struct Planes {
    pub key: u64,
    /// The image's key without its noise reduction (`Plan::pre_nr_key`).
    pub pre_nr: u64,
    pub log_l: Option<Arc<Plane>>,
    /// Keyed by its radius, process and median radius ([`BaseKey`]).
    pub base: Option<(BaseKey, Arc<Plane>)>,
    pub clarity: Option<(u32, Arc<Plane>)>,
    pub texture: Option<(u32, Arc<Plane>)>,
    /// Dark channel and its airlight.
    pub dark: Option<(u32, Arc<Plane>, f32)>,
    /// Blurred chromaticity (local Moiré / Noise).
    pub chroma: Option<(u32, Arc<Rgb32f>)>,
}

impl Planes {
    /// The planes that still fit an image differing from this one only in noise reduction, close
    /// enough for a draft: the wide blurs (base, clarity, dehaze, chromaticity). Its `key` matches
    /// no image, so only another draft takes them.
    pub fn for_other_nr(self) -> Planes {
        Planes { key: 0, pre_nr: self.pre_nr, log_l: None, texture: None, ..self }
    }
}

/// What the highlights/shadows base plane was computed with: its radius (`f32` bits), the
/// rendering process and the median radius ([`tone_base_detail`]). The base differs between
/// processes, so a photo moved to another one (Update to Current Process, undo) recomputes it.
pub type BaseKey = (u32, Process, usize);

/// The [`BaseKey`] of a base at `sigma` for `s` (`detail`: [`PlaneSigmas::base_detail`]).
pub fn base_key(s: &DevelopSettings, sigma: f32, detail: usize) -> BaseKey {
    (sigma.to_bits(), s.process.process(), detail)
}

/// Reuse `slot` if it was computed with `key` (a radius's `f32` bits, or a [`BaseKey`]), else
/// compute and store it.
pub fn plane_at<K: Copy + PartialEq, P>(slot: &mut Option<(K, Arc<P>)>, key: K, f: impl FnOnce() -> P) -> Arc<P> {
    match slot {
        Some((k, p)) if *k == key => p.clone(),
        _ => {
            let p = Arc::new(f());
            *slot = Some((key, p.clone()));
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
    /// Edge-aware base for highlights/shadows ([`tone_base`]: fast guided filter, [`BASE_EPS`]; from
    /// process V2 of the [`BASE_DETAIL`] median).
    pub base: Option<f32>,
    /// Median radius (px) of that base ([`tone_base_detail`]: 0 under V1).
    pub base_detail: usize,
    /// Clarity band (fast guided filter, [`CLARITY_EPS`]).
    pub clarity: Option<f32>,
    /// Texture / sharpening band (Gaussian).
    pub texture: Option<f32>,
    /// Dehaze dark channel (Gaussian).
    pub dark: Option<f32>,
    /// Chromaticity blur for local Moiré / colour noise (Gaussian).
    pub chroma: Option<f32>,
}

pub fn plane_sigmas(s: &DevelopSettings, px_per_long: f64, q: Quality) -> PlaneSigmas {
    let ppl = px_per_long as f32;
    // A hidden mask, or one with no components, does not change the picture. Its sliders must
    // not pick the tone base Clarity reads (#1214).
    let live = |m: &&lightcraft_develop::Mask| m.visible && !m.components.is_empty();
    let local_any = |f: fn(&lightcraft_develop::LocalAdjustments) -> f64| s.masks.iter().filter(live).any(|m| f(&m.adjust) != 0.0);
    let tone_active = s.light.highlights != 0.0
        || s.light.shadows != 0.0
        || s.masks.iter().filter(live).any(|m| m.adjust.highlights != 0.0 || m.adjust.shadows != 0.0);
    // Edge-aware base at ~1.5% of the long edge (EV² epsilon: edges of > ~0.6 EV are preserved)
    // From V2 it is taken of the median at 0.15%: lines and texture finer than that are detail,
    // not tone (#632).
    let base = tone_active.then(|| {
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
        || s.masks.iter().filter(live).any(|m| m.adjust.defringe > 0.0))
    .then(|| (0.0018 * ppl).max(0.6));
    let dark = (s.effects.dehaze != 0.0 || local_any(|a| a.dehaze)).then(|| (0.02 * ppl).max(1.0));
    let chroma = (local_any(|a| a.moire) || s.masks.iter().filter(live).any(|m| m.adjust.noise > 0.0)).then(|| (CHROMA_SIGMA * ppl).max(1.0));
    let base_detail = if base.is_some() { tone_base_detail(s, px_per_long) } else { 0 };
    PlaneSigmas { base, base_detail, clarity, texture, dark, chroma }
}

/// The spatial planes the per-pixel stage needs for `img` (white-balanced, before exposure),
/// reusing whatever `planes` already holds for it. Missing planes are computed side by side (each
/// one alone scales poorly: the guided filters work on small subsampled grids).
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare(
    img: Arc<Rgb32f>,
    s: &DevelopSettings,
    frame: &Frame,
    px_per_long: f64,
    q: Quality,
    planes: &mut Planes,
    mattes: Option<&masks::Mattes>,
) -> Prepared {
    let log_l = planes.log_l.get_or_insert_with(|| Arc::new(timed("log_l", || img.map(log_lum)))).clone();
    let PlaneSigmas { base: base_sigma, base_detail, clarity: clarity_sigma, texture: texture_sigma, dark: dark_sigma, chroma: chroma_sigma } =
        plane_sigmas(s, px_per_long, q);

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
        || base_sigma.map(|sg| plane_at(sb, base_key(s, sg, base_detail), || timed("base", || tone_base(l, sg, base_detail)))),
        || {
            par_join(
                || clarity_sigma.map(|sg| plane_at(sc, sg.to_bits(), || timed("clarity", || guided_fast(l, sg, CLARITY_EPS)))),
                || {
                    par_join(
                        || texture_sigma.map(|sg| plane_at(st, sg.to_bits(), || timed("texture", || gaussian(l, sg)))),
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
    let mut img = Rgb32f::clone(&frame_proxy(src, frame, proxy_w, proxy_h));
    white_balance(&mut img, info, s);
    let sigma = (0.02 * frame.px_per_long(proxy_w)).max(1.0) as f32;
    airlight(&gaussian(&img.map(dark_of), sigma))
}

/// How many airlight proxies [`frame_proxy`] keeps (one per photo and frame being zoomed into; each ≤ 1.8 MB).
const FRAME_PROXIES_KEPT: usize = 4;

/// `frame.sample(src, w, h)`, kept for the last few (source, frame, size): sampling a 64 MP original down to the
/// airlight proxy reads all of it (~190 ms of CPU), and every window of a zoomed view with Dehaze asked for it again,
/// on each pan and slider tick. The proxy is taken before white balance, so a WB change still hits.
fn frame_proxy(src: &Rgb32f, frame: &Frame, w: usize, h: usize) -> Arc<Rgb32f> {
    type Entry = (u64, Frame, usize, usize, Arc<Rgb32f>);
    static CACHE: std::sync::Mutex<Vec<Entry>> = std::sync::Mutex::new(Vec::new());
    let key = crate::optics::fingerprint(src);
    let found = |c: &[Entry]| c.iter().find(|e| e.0 == key && e.1 == *frame && (e.2, e.3) == (w, h)).map(|e| e.4.clone());
    if let Some(img) = found(&CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)) {
        return img;
    }
    let img = Arc::new(frame.sample(src, w, h));
    let mut c = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    c.push((key, frame.clone(), w, h, img.clone()));
    if c.len() > FRAME_PROXIES_KEPT {
        c.remove(0);
    }
    img
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
    /// Edits saved under V1 and V2 keep main's colour noise reduction (blurred chromaticity); V3 gets
    /// the joint method of `nrc_joint`.
    #[test]
    fn colour_nr_follows_the_process() {
        use lightcraft_develop::{DevelopSettings, Process};
        let (w, h) = (96usize, 64usize);
        let img = Rgb32f::from_fn(w, h, |x, y| {
            let n = |c: u32| {
                let mut v = (x as u32).wrapping_mul(0x9e37_79b9) ^ (y as u32).wrapping_mul(0x85eb_ca6b) ^ c.wrapping_mul(0xc2b2_ae35) | 1;
                v ^= v << 13;
                v ^= v >> 17;
                v ^= v << 5;
                (v as f32 / u32::MAX as f32 - 0.5) * 0.02
            };
            let base = if x < w / 2 { [0.06, 0.03, 0.02] } else { [0.02, 0.04, 0.06] };
            [base[0] + n(1), base[1] + n(2), base[2] + n(3)]
        });
        let run = |p: Process| {
            let mut s = DevelopSettings { process: p.version(), ..Default::default() };
            s.detail.nr_color = 50.0;
            let mut o = img.clone();
            denoise(&mut o, &s, 1.0, None, None);
            o
        };
        let (v1, v2, v3) = (run(Process::V1), run(Process::V2), run(Process::V3));
        assert!(v1.data == v2.data, "V1 and V2 share main's colour NR");
        // main's colour NR clamps at 0 and keeps each pixel's luminance; the joint method keeps it too
        assert!(v2.data != v3.data, "V3 takes the joint colour NR");
        let mut s = DevelopSettings { process: Process::V3.version(), ..Default::default() };
        s.detail.nr_color = 50.0;
        let mut joint = img.clone();
        let p = nrc_params(&s, 1.0).expect("Color 50 has parameters");
        crate::nrc_joint::denoise(&mut joint, &p, (s.detail.nr_color_detail / 100.0) as f32, 0.5, false, None);
        assert!(joint.data == v3.data, "V3 is the joint method");
    }

    /// Edits up to V3 keep main's luminance noise reduction (guided filter on log luminance); V4 gets the
    /// wavelet filter of `nrl`, after the colour NR.
    #[test]
    fn luminance_nr_follows_the_process() {
        use lightcraft_develop::{DevelopSettings, Process};
        let (w, h) = (96usize, 64usize);
        let img = Rgb32f::from_fn(w, h, |x, y| {
            let mut v = (x as u32).wrapping_mul(0x9e37_79b9) ^ (y as u32).wrapping_mul(0x85eb_ca6b) | 1;
            v ^= v << 13;
            v ^= v >> 17;
            v ^= v << 5;
            let n = (v as f32 / u32::MAX as f32 - 0.5) * 0.02;
            let base = if x < w / 2 { 0.05 } else { 0.12 };
            [base + n; 3]
        });
        let run = |p: Process| {
            let mut s = DevelopSettings { process: p.version(), ..Default::default() };
            s.detail.nr_luminance = 50.0;
            let mut o = img.clone();
            denoise(&mut o, &s, 1.0, None, None);
            o
        };
        let (v2, v3, v4) = (run(Process::V2), run(Process::V3), run(Process::V4));
        assert!(v2.data == v3.data, "V2 and V3 share main's luminance NR");
        assert!(v3.data != v4.data, "V4 takes the wavelet luminance NR");
        let mut wavelet = img.clone();
        crate::nrl::denoise(&mut wavelet, &crate::nrl::Params { amount: 0.5, detail: 0.5, contrast: 0.0, scale: 1.0 }, None);
        assert!(wavelet.data == v4.data, "V4 is the wavelet filter");
    }

    /// Float mean of a flat, noisy dark patch over three noise realisations; linear modes assert drift bounds.
    #[test]
    #[ignore]
    fn probe_nrc_mean_drift() {
        use lightcraft_develop::DevelopSettings;
        let (w, h) = (768usize, 768usize);
        let mut worst = 0.0f64;
        for seed0 in [0x2545_f491_4f6c_dd1du64, 0x9e37_79b9_7f4a_7c15, 0x1234_5678_9abc_def1] {
            let mut seed = seed0;
            let mut gauss = || {
                (0..4)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        (seed >> 11) as f32 / (1u64 << 53) as f32 - 0.5
                    })
                    .sum::<f32>()
                    * 1.732
            };
            for (name, base, sigma) in [
                ("dark warm, blue near 0", [0.006f32, 0.004, 0.0008], [0.002f32, 0.0015, 0.003]),
                ("cat-like, red near 0", [0.0015, 0.004, 0.009], [0.004, 0.003, 0.005]),
            ] {
                let mut img = Rgb32f::new(w, h);
                for p in img.data.iter_mut() {
                    *p = [0, 1, 2].map(|i| base[i] + sigma[i] * gauss());
                }
                let mean = |im: &Rgb32f| {
                    let n = im.data.len() as f64;
                    [0, 1, 2].map(|i| im.data.iter().map(|p| p[i] as f64).sum::<f64>() / n)
                };
                let m0 = mean(&img);
                for nr in [25.0, 100.0] {
                    let mut s = DevelopSettings::default();
                    s.detail.nr_color = nr;
                    let mut a = img.clone();
                    denoise(&mut a, &s, 1.0, None, None);
                    let m = mean(&a);
                    let clipped0 = mean(&img.map(|p| p.map(|v| v.max(0.0))));
                    let sd = |im: &Rgb32f, mm: [f64; 3]| {
                        [0, 1, 2].map(|i| (im.data.iter().map(|p| (p[i] as f64 - mm[i]).powi(2)).sum::<f64>() / im.data.len() as f64).sqrt())
                    };
                    worst = worst.max([0, 1, 2].iter().map(|&i| (m[i] / m0[i] - 1.0).abs()).fold(0.0, f64::max));
                    println!(
                        "{name} NR {nr}: true mean {m0:.5?}, clipped noisy mean {clipped0:.5?}, after v2 {m:.5?} → {:+.3?} % vs true; noise sd {:.5?} → {:.5?}",
                        [0, 1, 2].map(|i| 100.0 * (m[i] / m0[i] - 1.0)),
                        sd(&img, m0),
                        sd(&a, m)
                    );
                }
            }
        }
        println!("worst mean deviation {:.4} %", 100.0 * worst);
        assert!(worst < 0.0005, "the joint colour NR moved a channel's mean by {:.3} %", 100.0 * worst);
    }

    /// PROBE (prints, never fails): what colour NR v2 keeps of real colour. A clean synthetic scene (an
    /// equal-luminance red/green edge, a 2 px red line on grey, a saturated blue patch near black, a flat grey
    /// patch) plus noise; each measure compares the output with the clean scene.
    #[test]
    #[ignore]
    fn probe_nrc_real_colour() {
        use lightcraft_develop::DevelopSettings;
        let (w, h) = (768usize, 512usize);
        let y_of = |c: [f32; 3]| luminance_2020(c);
        // two colours of the same luminance 0.03
        let red = [0.08f32, 0.0155, 0.02];
        let green = {
            let g = [0.012f32, 0.04, 0.015];
            let k = y_of(red) / y_of(g);
            g.map(|v| v * k)
        };
        let grey = [0.03f32; 3];
        let blue_dark = [0.001f32, 0.0015, 0.02];
        // a bright saturated green next to black (a lit leaf against a dark background)
        let leaf = [0.02f32, 0.12, 0.01];
        let clean = |x: usize, y: usize| -> [f32; 3] {
            if y < 256 {
                if x < 384 { red } else { green }
            } else if x < 384 {
                if (190..192).contains(&x) { red } else { grey }
            } else if x < 576 {
                blue_dark
            } else if y < 384 {
                grey
            } else if x < 672 {
                leaf
            } else {
                [0.0005f32; 3]
            }
        };
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut gauss = || {
            (0..4)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 11) as f32 / (1u64 << 53) as f32 - 0.5
                })
                .sum::<f32>()
                * 1.732
        };
        let mut img = Rgb32f::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let c = clean(x, y);
                // shot + read noise per channel
                img.data[y * w + x] = c.map(|v| v + (0.00002 + 0.004 * v.max(0.0)).sqrt() * 0.6 * gauss());
            }
        }
        let chroma = |c: [f32; 3]| {
            let y = y_of(c);
            c.map(|v| v - y)
        };
        let dist = |a: [f32; 3], b: [f32; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        for nr in [25.0, 100.0] {
            let mut s = DevelopSettings::default();
            s.detail.nr_color = nr;
            let mut o = img.clone();
            denoise(&mut o, &s, 1.0, None, None);
            let px = |x: usize, y: usize| o.data[y * w + x];
            // edge: chroma error 1..8 px either side, relative to the step between the two colours
            let step = dist(chroma(red), chroma(green));
            let mut e = 0.0;
            let mut n = 0.0;
            for y in 40..216 {
                for d in 1..=8 {
                    for x in [384 - d, 383 + d] {
                        e += dist(chroma(px(x, y)), chroma(clean(x, y)));
                        n += 1.0;
                    }
                }
            }
            // line: share of the line's chroma kept (mean over its rows)
            let (mut kept, mut m) = (0.0, 0.0);
            for y in 300..480 {
                for x in 190..192 {
                    let c = chroma(px(x, y));
                    let r = chroma(red);
                    let rg = chroma(grey);
                    let full = [0, 1, 2].map(|i| r[i] - rg[i]);
                    let got = [0, 1, 2].map(|i| c[i] - rg[i]);
                    kept += (got[0] * full[0] + got[1] * full[1] + got[2] * full[2]) / (full[0] * full[0] + full[1] * full[1] + full[2] * full[2]);
                    m += 1.0;
                }
            }
            // saturated blue near black: chroma of the patch's mean kept
            let mut acc = [0.0f32; 3];
            let mut cnt = 0.0;
            for y in 300..480 {
                for x in 420..540 {
                    acc = [0, 1, 2].map(|i| acc[i] + px(x, y)[i]);
                    cnt += 1.0;
                }
            }
            let mean = acc.map(|v| v / cnt);
            let sat_kept = dist(chroma(mean), [0.0; 3]) / dist(chroma(blue_dark), [0.0; 3]);
            // grey: residual chroma noise
            let mut sd = 0.0;
            let mut k = 0.0;
            for y in 270..370 {
                for x in 600..760 {
                    sd += dist(chroma(px(x, y)), [0.0; 3]).powi(2);
                    k += 1.0;
                }
            }
            let mut sd0 = 0.0;
            for y in 270..370 {
                for x in 600..760 {
                    sd0 += dist(chroma(img.data[y * w + x]), [0.0; 3]).powi(2);
                }
            }
            // leaf next to black: chroma kept in the leaf's 12 px next to the black (mean over rows)
            let (mut lk, mut ln) = (0.0f32, 0.0f32);
            for y in 400..500 {
                for x in 660..672 {
                    let c = chroma(px(x, y));
                    let r = chroma(leaf);
                    lk += (c[0] * r[0] + c[1] * r[1] + c[2] * r[2]) / (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]);
                    ln += 1.0;
                }
            }
            println!("NR {nr}: leaf next to black keeps {:.0} % of its chroma (12 px next to the black)", 100.0 * lk / ln);
            println!(
                "NR {nr}: edge error {:.1} % of the step | red line kept {:.0} % | dark blue chroma kept {:.0} % | grey chroma noise {:.5} → {:.5}",
                100.0 * e / n / step,
                100.0 * kept / m,
                100.0 * sat_kept,
                (sd0 / k).sqrt(),
                (sd / k).sqrt()
            );
        }
    }

    #[test]
    fn nrc_noise_fit_is_non_negative() {
        // a falling cloud would give a negative slope: the constant fits instead
        let (a, b) = fit_nonneg_line(&[(0.0, 4.0, 1.0), (1.0, 3.0, 1.0), (2.0, 2.0, 1.0)]);
        assert!(a == 0.0 && (b - 3.0).abs() < 1e-9);
        // a line through a negative intercept: refitted through the origin
        let (a, b) = fit_nonneg_line(&[(1.0, 1.0, 1.0), (2.0, 3.0, 1.0), (3.0, 5.0, 1.0)]);
        assert!(a > 0.0 && b == 0.0);
        // an exact shot + read noise line is found as it is
        let (a, b) = fit_nonneg_line(&[(0.0, 2.0, 1.0), (1.0, 5.0, 1.0), (2.0, 8.0, 1.0)]);
        assert!((a - 3.0).abs() < 1e-9 && (b - 2.0).abs() < 1e-9);
    }
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

    // A windowed render's airlight proxy is kept per (source, frame, size): the airlight is the one sampling anew gives,
    // and another source or frame of the same size gets its own, never the proxy kept for the last one
    #[test]
    fn the_kept_airlight_proxy_matches_a_fresh_one_and_misses_on_another_source_or_frame() {
        let (info, s) = (SourceInfo::default(), DevelopSettings::default());
        let hazy = Rgb32f::from_fn(300, 200, |x, y| {
            let v = 0.05 + 0.6 * x as f32 / 300.0;
            [v, v * 0.95, v * (0.8 + 0.2 * y as f32 / 200.0)]
        });
        let other = Rgb32f::from_fn(300, 200, |x, y| [0.02 + 0.1 * ((x + y) % 7) as f32 / 7.0; 3]);
        let fresh = |src: &Rgb32f, frame: &Frame| {
            let mut img = frame.sample(src, 48, 32);
            white_balance(&mut img, &info, &s);
            airlight(&gaussian(&img.map(dark_of), (0.02 * frame.px_per_long(48)).max(1.0) as f32))
        };
        let frame = crate::frame_for(&hazy, &info, &s, true);
        let kept = frame_proxy(&hazy, &frame, 48, 32);
        assert!(Arc::ptr_eq(&kept, &frame_proxy(&hazy, &frame, 48, 32)), "the second ask is served from the cache");
        assert_eq!(frame_airlight(&hazy, &info, &s, &frame, 48, 32).to_bits(), fresh(&hazy, &frame).to_bits());
        assert_eq!(frame_airlight(&hazy, &info, &s, &frame, 48, 32).to_bits(), fresh(&hazy, &frame).to_bits());
        // another source of the same size
        assert_ne!(fresh(&other, &frame), fresh(&hazy, &frame));
        assert_eq!(frame_airlight(&other, &info, &s, &frame, 48, 32).to_bits(), fresh(&other, &frame).to_bits());
        // the same source, cropped to its darker half
        let mut cropped = frame.clone();
        cropped.crop.rect.x1 = 0.5;
        assert_ne!(fresh(&hazy, &cropped), fresh(&hazy, &frame));
        assert_eq!(frame_airlight(&hazy, &info, &s, &cropped, 48, 32).to_bits(), fresh(&hazy, &cropped).to_bits());
    }

    #[test]
    fn a_hidden_or_empty_mask_does_not_request_the_tone_base() {
        use lightcraft_develop::{Mask, MaskComponent, MaskOp, MaskShape};
        use lightcraft_geom::Point;
        let shape = MaskShape::Linear { start: Point::new(0.0, 0.0), end: Point::new(1.0, 0.0) };
        let component = MaskComponent { name: None, op: MaskOp::Add, invert: false, shape };
        let mut s = DevelopSettings::default();
        s.masks.push(Mask {
            visible: false,
            components: vec![component],
            adjust: lightcraft_develop::LocalAdjustments { highlights: 40.0, ..Default::default() },
            ..Default::default()
        });
        assert!(plane_sigmas(&s, 2000.0, Quality::Full).base.is_none());
        s.masks[0].visible = true;
        s.masks[0].components.clear();
        assert!(plane_sigmas(&s, 2000.0, Quality::Full).base.is_none());
        s.masks[0].components.push(MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.0, 0.0), end: Point::new(1.0, 0.0) },
        });
        assert!(plane_sigmas(&s, 2000.0, Quality::Full).base.is_some());
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

    /// The previous fast path, including its two materialized coefficient resizes.
    fn resized_reference(p: &Plane, sigma: f32, eps: f32) -> Plane {
        use lightcraft_raster::resample::{Filter, resize};
        let s = guided_fast_step(sigma);
        if s <= 1 {
            return guided(p, sigma, eps);
        }
        let lo = resize(p, p.width.div_ceil(s).max(1), p.height.div_ceil(s).max(1), Filter::Box);
        let (a, b) = guided_coeffs(&lo, sigma / s as f32, eps);
        let (a, b) = (resize(&a, p.width, p.height, Filter::Bilinear), resize(&b, p.width, p.height, Filter::Bilinear));
        Plane::from_fn(p.width, p.height, |x, y| a.get(x, y) * p.get(x, y) + b.get(x, y))
    }

    #[test]
    fn sampled_coefficients_match_materialized_resizes() {
        for (w, h) in [(0, 0), (1, 1), (1, 19), (23, 1), (2, 3), (17, 13), (64, 48), (161, 119)] {
            let planes = [
                Plane::filled(w, h, -3.25),
                Plane::from_fn(w, h, |x, y| ((x * 7919 + y * 104729) % 97) as f32 / 7.0 - 8.0),
                Plane::from_fn(w, h, |x, y| {
                    if x == w / 2 && y == h / 2 {
                        9.0
                    } else if x < w / 2 {
                        -5.0
                    } else {
                        2.0
                    }
                }),
            ];
            // Step 1 fallback, every small step, Draft's cap, and the maximum step (16).
            for sigma in [0.0, 2.99, 5.99, 6.0, 9.0, 12.0, 24.0, 48.0, 200.0] {
                for eps in [BASE_EPS, CLARITY_EPS, 0.0001] {
                    for p in &planes {
                        let expected = resized_reference(p, sigma, eps);
                        let actual = guided_fast(p, sigma, eps);
                        assert_eq!((actual.width, actual.height), (w, h));
                        assert_eq!(actual.data, expected.data, "{w}x{h}, sigma {sigma}, eps {eps}");
                    }
                }
            }
        }
    }

    #[test]
    fn sampled_tone_base_keeps_the_median_before_the_guided_filter() {
        let p = Plane::from_fn(73, 51, |x, y| if x % 9 == 0 { 3.0 } else { -4.0 + ((x + y) % 7) as f32 * 0.03 });
        for sigma in [6.0, 24.0, 120.0] {
            for detail in [0, 1, 3, BASE_DETAIL_MAX] {
                let median = median_hv(&p, detail);
                assert_eq!(tone_base(&p, sigma, detail).data, resized_reference(&median, sigma, BASE_EPS).data, "{sigma}, {detail}");
            }
        }
    }

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
        denoise(&mut img, &s, 1.0, None, None);
        let (v1, m1) = var(&img);
        assert!(v1 < v0 * 0.5, "{v0} -> {v1}");
        assert!((m1 - m0).abs() < 0.01);
    }
}
