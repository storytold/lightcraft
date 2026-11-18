//! Scene-linear preparation: white balance + exposure, and the spatial planes the per-pixel stage
//! needs (edge-aware base layer for highlights/shadows, clarity/texture bands, dehaze veil).

use lcc_color::cct::{temp_tint_to_xy, wb_matrix, xy_to_temp_tint};
use lcc_color::{REC2020, luminance_2020};
use lcc_develop::{DevelopSettings, Process};
use lcc_raster::blur::{gauss_taps, gaussian, gaussian_taps};
use std::sync::Arc;

use lcc_raster::{Plane, Rgb32f, par_join};

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
        && let Some(m) = lcc_raw::color::rebalance(&cc.tags, cc.developed_for, temp_tint_to_xy(t, tint))
    {
        return Some(m.to_f32());
    }
    let set = wb_matrix(&REC2020, effective_white(info, s));
    let shot = wb_matrix(&REC2020, temp_tint_to_xy(info.as_shot_temp, info.as_shot_tint));
    let m = set.mul(&shot.inverse().unwrap_or(lcc_color::Mat3::IDENTITY));
    // Normalize so neutral luminance is preserved (WB shouldn't change exposure).
    let g = m.apply([1.0, 1.0, 1.0]);
    let y = g[0] * 0.2627 + g[1] * 0.6780 + g[2] * 0.0593;
    Some(m.mul(&lcc_color::Mat3::diag(1.0 / y, 1.0 / y, 1.0 / y)).to_f32())
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
    use lcc_develop::WbMode;
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
fn effective_white(info: &SourceInfo, s: &DevelopSettings) -> lcc_color::Xy {
    if legacy_wb(info, s)
        && s.wb.mode != lcc_develop::WbMode::AsShot
        && let Some(xy) = legacy_white(s.wb.temp, s.wb.tint, (info.as_shot_temp, info.as_shot_tint))
    {
        return xy;
    }
    let (t, tint) = effective_wb(info, s);
    temp_tint_to_xy(t, tint)
}

/// Whether `s` holds a white balance written on the relative scale for a source that is edited in Kelvin today.
fn legacy_wb(info: &SourceInfo, s: &DevelopSettings) -> bool {
    info.raw && !info.relative_wb && info.legacy_relative_wb && s.wb.scale == lcc_develop::WbScale::Legacy
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
pub fn legacy_white(temp: f64, tint: f64, as_shot: (f64, f64)) -> Option<lcc_color::Xy> {
    use lcc_color::{BRADFORD, Xy};
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

/// The relative-scale `(temp, tint)` that [`legacy_to_kelvin`] would read as the Kelvin `(temp, tint)` on a source
/// whose real as-shot white is `as_shot`: the inverse of [`legacy_white`]'s composition. It runs the Bradford step
/// backwards — the old target's cone response times the old reference's over the as-shot white's — and reads the
/// result back through [`xy_to_temp_tint`]. `(6500, 0)` is the relative scale's as-shot reference, so the Kelvin
/// white of the source itself maps exactly to it. `None` when the arithmetic has nowhere to go (a non-finite input,
/// a white outside the chromaticity plane, or an as-shot cone response of zero, where the forward step discarded it).
pub fn kelvin_to_legacy(temp: f64, tint: f64, as_shot: (f64, f64)) -> Option<(f64, f64)> {
    use lcc_color::{BRADFORD, Xy};
    if !(temp.is_finite() && tint.is_finite() && as_shot.0.is_finite() && as_shot.1.is_finite()) {
        return None;
    }
    if (temp - as_shot.0).abs() < 1e-9 && (tint - as_shot.1).abs() < 1e-9 {
        // the source's own white: the relative scale's as-shot reference, exactly
        return Some((6500.0, 0.0));
    }
    let cones = |t: f64, tint: f64| BRADFORD.apply(temp_tint_to_xy(t, tint).to_xyz());
    let (target, reference, shot) = (cones(temp, tint), cones(6500.0, 0.0), cones(as_shot.0, as_shot.1));
    let inverse = BRADFORD.inverse()?;
    let mut old = [0.0; 3];
    for i in 0..3 {
        if shot[i].abs() < 1e-12 {
            return None;
        }
        old[i] = target[i] * reference[i] / shot[i];
    }
    let xy = Xy::from_xyz(inverse.apply(old));
    if !(xy.x.is_finite() && xy.y.is_finite() && xy.y > 0.0) {
        return None;
    }
    let (t, tint) = xy_to_temp_tint(xy);
    (t.is_finite() && tint.is_finite()).then_some((t, tint))
}

/// Bring the white balance of `d` onto the scale `info` is edited on, before a new value is written into it: on a
/// raw with a Kelvin scale the values are marked Kelvin, and ones still on the relative scale become the Custom
/// Kelvin / tint that renders the same ([`legacy_to_kelvin`]; a preset mode's look is kept as that custom
/// value, since the preset's Kelvin value would be a different look). Elsewhere the scale is cleared, so values
/// edited on a rendered photo or a raw without matrices never pass for Kelvin when pasted onto one that has them.
pub fn normalize_wb(d: &mut DevelopSettings, info: &SourceInfo) {
    use lcc_develop::{WbMode, WbScale};
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
        use lcc_raster::resample::{Filter, weights};
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
    use lcc_raster::resample::{Filter, resize};
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

/// Separable minimum (`2r+1` px along each row, then each column; clamped edges): a square
/// erosion. Dehaze's patch dark channel from process V3 (#1237).
pub fn min_hv(p: &Plane, r: usize) -> Plane {
    if r == 0 || p.data.is_empty() {
        return p.clone();
    }
    let rows = |src: &Plane| {
        let mut out = Plane::new(src.width, src.height);
        for_rows(&mut out.data, src.width, |y, row| min_row(src.row(y), row, r));
        out
    };
    transpose(&rows(&transpose(&rows(p))))
}

/// Minimum of `src` over `2r+1` taps (clamped to the row) into `out`, in O(1) per pixel (van Herk /
/// Gil–Werman: block-wise running minima from both ends).
fn min_row(src: &[f32], out: &mut [f32], r: usize) {
    let n = src.len();
    let k = 2 * r + 1;
    // padded with +inf: r before, enough after to complete the last block
    let len = (n + 2 * r).div_ceil(k) * k;
    let at = |i: usize| if i >= r { src.get(i - r).copied().unwrap_or(f32::INFINITY) } else { f32::INFINITY };
    let mut fwd = vec![f32::INFINITY; len];
    let mut bwd = vec![f32::INFINITY; len];
    for i in 0..len {
        let v = at(i);
        fwd[i] = if i % k == 0 { v } else { v.min(fwd[i - 1]) };
    }
    for i in (0..len).rev() {
        let v = at(i);
        bwd[i] = if i % k == k - 1 || i + 1 == len { v } else { v.min(bwd[i + 1]) };
    }
    for (x, o) in out.iter_mut().enumerate().take(n) {
        // window [x, x + 2r] in padded coordinates
        *o = bwd.get(x).copied().unwrap_or(f32::INFINITY).min(fwd.get(x + 2 * r).copied().unwrap_or(f32::INFINITY));
    }
}

/// Patch radius of dehaze's dark channel from process V3, as a fraction of the long edge.
pub const DEHAZE_PATCH: f64 = 0.005;
/// Largest [`dehaze_patch_radius`] (px).
pub const DEHAZE_PATCH_MAX: usize = 64;

/// Patch (minimum) radius of the dehaze dark channel of `s`'s process at `px_per_long`: 0 (the
/// per-pixel minimum) before V3.
pub fn dehaze_patch_radius(s: &DevelopSettings, px_per_long: f64) -> usize {
    match s.process.process() {
        Process::V1 | Process::V2 => 0,
        Process::V3 | Process::V4 | Process::V5 | Process::V6 | Process::V7 | Process::V8 | Process::V9 => {
            ((DEHAZE_PATCH * px_per_long).round().max(1.0) as usize).min(DEHAZE_PATCH_MAX)
        }
    }
}

/// Cache key of the dark-channel plane: its blur radius and patch radius.
pub fn dark_key(sigma: f32, patch: usize) -> u64 {
    u64::from(sigma.to_bits()) | ((patch as u64) << 32)
}

/// The dehaze dark channel: the Gaussian (`sigma`) of the `patch`-px minimum of each pixel's
/// darkest channel.
pub fn dark_plane(img: &Rgb32f, sigma: f32, patch: usize) -> Plane {
    gaussian(&min_hv(&img.map(dark_of), patch), sigma)
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
        Process::V2 | Process::V3 | Process::V4 | Process::V5 | Process::V6 | Process::V7 | Process::V8 | Process::V9 => {
            base_detail_radius(px_per_long)
        }
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

/// Draft colour NR blurs at half resolution from this radius (px) on: the chromaticity is smooth at such
/// radii, so a quarter of the pixels at half the radius gives the same result well within a
/// display step, in about half the time (gap 26). Sources: own design (pyramid-level blur).
pub const HALF_RES_NR_SIGMA: f32 = 4.0;

/// Output pixels per pixel of the original photo for an output of `px_per_long` pixels per unit
/// of the long edge of an original `native_long` pixels long, at most 1 ([`crate::Plan::detail_scale`]).
pub fn detail_scale(px_per_long: f64, native_long: usize) -> f32 {
    let k = (px_per_long / native_long.max(1) as f64) as f32;
    if k.is_finite() { k.clamp(0.0, 1.0) } else { 1.0 }
}

/// Smallest radius (px) noise reduction filters a preview with: [`gaussian`] makes no blur below
/// about 0.58 px and one 3-px box from there ([`lcc_raster::blur::box_sigma`]).
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
    let made = lcc_raster::blur::box_sigma(NR_MIN_SIGMA);
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
/// log-luminance, colour by blurring chromaticity (rgb / Y) and re-applying the original luminance
/// (process V1 to V7), or from V8 by the joint filter of the linear colour differences
/// ([`joint_colour_nr`], `crate::nrc_joint`). `scale`: output pixels per pixel of the original
/// ([`nr_params`]). A full-quality render whose joint colour NR measures the noise on `img`
/// ([`denoise_with`] for a draft or the whole frame's noise model).
pub fn denoise(img: &mut Rgb32f, s: &DevelopSettings, scale: f32) {
    denoise_with(img, s, scale, false, None);
}

/// [`denoise`] for a render that may be a draft. `draft`: a draft render (a slider drag) may blur
/// the chromaticity at half resolution ([`HALF_RES_NR_SIGMA`]); a full-quality render always takes
/// the full-resolution blur, so it matches the GPU path and the look the photo always had (the
/// joint colour NR of V8 has no draft form). `colour_noise`: the whole frame's noise model for the
/// joint colour NR ([`crate::colour_noise`]); `None` measures it on `img`.
pub fn denoise_with(img: &mut Rgb32f, s: &DevelopSettings, scale: f32, draft: bool, colour_noise: Option<&crate::nrc_joint::JointNoise>) {
    let (lum, col) = nr_params(s, scale);
    if col.is_some() && joint_colour_nr(s) {
        apply_nr(img, lum, None, draft);
        if let Some(p) = nrc_params(s, scale) {
            let _t = crate::profiling().then(std::time::Instant::now);
            let (detail, amount) = nrc_sliders(s);
            crate::nrc_joint::denoise(img, &p, detail, amount, colour_noise);
            if let Some(t) = _t {
                eprintln!("    nr colour (joint): {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
            }
        }
        return;
    }
    apply_nr(img, lum, col, draft);
}

/// Whether `s` gets the joint colour noise reduction (`crate::nrc_joint`, process V8 on); V1 to V7
/// keep the blurred chromaticity with its speckle median ([`SPECKLE_LO`]).
pub fn joint_colour_nr(s: &DevelopSettings) -> bool {
    s.process.process() >= lcc_develop::Process::V8
}

/// Color Detail and Color as fractions (0..=1) for the joint colour NR; a value that is not a
/// number counts as 0.
pub fn nrc_sliders(s: &DevelopSettings) -> (f32, f32) {
    let unit = |v: f64| {
        let v = (v / 100.0) as f32;
        if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
    };
    (unit(s.detail.nr_color_detail), unit(s.detail.nr_color))
}

/// Most samples the noise level of the joint colour noise reduction is measured from. The CPU and
/// the GPU sample the same pixels (see `nrc_joint::noise_sample_stride`).
pub const NRC_SAMPLES: usize = 131_072;

/// One pass of the joint colour noise reduction's reach: Gaussian windows of `sigma` px (the joint
/// filter takes the last, coarse pass's `sigma`); `step`, `k` and `pct` are kept from the pass
/// layout it was built with (`crate::nrc_joint`'s test reference reads `step`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NrcPass {
    pub sigma: f32,
    pub step: usize,
    pub k: f32,
    pub pct: f32,
}

/// Parameters of the joint colour noise reduction: `off` (unused by the joint filter) and the
/// passes (fine, then coarse).
#[derive(Clone, Debug, PartialEq)]
pub struct NrcParams {
    pub off: f32,
    pub passes: Vec<NrcPass>,
}

/// The joint colour noise reduction's parameters at `scale` output pixels per pixel of the original
/// ([`crate::Plan::detail_scale`]), `None` when Color is 0 (or not a number).
pub fn nrc_params(s: &DevelopSettings, scale: f32) -> Option<NrcParams> {
    let (detail, amount) = nrc_sliders(s);
    if amount <= 0.0 {
        return None;
    }
    let smooth = (s.detail.nr_color_smoothness / 100.0) as f32;
    let smooth = 1.0 + if smooth.is_finite() { smooth.clamp(0.0, 1.0) } else { 0.0 };
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
    /// The variance of channel `i` (0 to 2; another index gives the smallest variance) at luminance `y`.
    #[inline]
    pub fn var(&self, i: usize, y: f32) -> f32 {
        let (a, b) = (self.alpha.get(i).copied().unwrap_or(0.0), self.beta.get(i).copied().unwrap_or(0.0));
        (a * y.max(0.0) + b).max(1e-14)
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

/// Noise reduction with explicit parameters (see [`denoise`]; `draft` as there).
pub fn apply_nr(img: &mut Rgb32f, lum: Option<NrLum>, col: Option<NrColor>, draft: bool) {
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
        // the half-resolution blur is a (slightly) different look: drafts only, never a final render
        colour_nr(img, nr, draft && nr.sigma >= HALF_RES_NR_SIGMA);
        if let Some(t) = _t {
            eprintln!("    nr colour: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        }
    }
}

/// Speckle-adaptive colour NR (issue #679): the per-channel chromaticity deviations (rgb / Y units)
/// from a pixel's 3 × 3 median at which the blend towards the blurred chromaticity starts and
/// reaches full. A plain blur mix removes chroma speckle (green and magenta single-pixel noise at
/// high ISO) only by its small base weight, so it survives as the coloured dots the issue shows; a
/// pixel that disagrees with *all* of its neighbours at once is speckle, not texture or a hue edge
/// (a 3 × 3 median tracks both), and is pulled all the way in. Sources: own design (median outlier
/// test + adaptive blend).
pub const SPECKLE_LO: f32 = 0.16;
pub const SPECKLE_HI: f32 = 0.26;

/// `smooth(e0, e1, x)`: 0 below `e0`, 1 above `e1`, a smooth step between (the same easing the
/// sharpening mask uses, [`crate::finish`]).
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The chromaticity (rgb / Y) of every pixel.
fn chroma_plane(img: &Rgb32f) -> Rgb32f {
    Rgb32f::from_fn(img.width, img.height, |x, y| {
        let c = img.get(x, y).map(|v| v.max(0.0));
        let y = luminance_2020(c).max(1e-6);
        c.map(|v| v / y)
    })
}

/// The per-channel median of the 3 × 3 chromaticity neighbourhood of pixel `i` (`chroma` from
/// [`chroma_plane`]): the value most neighbours agree on. Smooth fields and hue edges pass through
/// (the median takes the pixel's own side of an edge); an isolated speckle stands alone against all
/// nine samples.
fn median_chroma3x3(chroma: &Rgb32f, x: usize, y: usize) -> [f32; 3] {
    let mut v = [[0.0f32; 9]; 3];
    let mut n = 0;
    for dy in -1..=1isize {
        for dx in -1..=1isize {
            let c = chroma.get_clamped(x as isize + dx, y as isize + dy);
            for (k, val) in c.into_iter().enumerate() {
                v[k][n] = val;
            }
            n += 1;
        }
    }
    let mut out = [0.0f32; 3];
    for (k, channel) in v.iter_mut().enumerate() {
        channel.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out[k] = channel[4];
    }
    out
}

/// One pixel of speckle-adaptive colour NR: blend the pixel's chromaticity `c0` towards the
/// luminance-weighted blurred chromaticity `cb` by `t`, harder per channel where the pixel is a
/// speckle outlier against its 3 × 3 median `med` ([`SPECKLE_LO`]..[`SPECKLE_HI`]), then re-apply
/// the luminance `yl`.
fn nr_pixel(c0: [f32; 3], cb: [f32; 3], med: [f32; 3], t: f32, yl: f32) -> [f32; 3] {
    [0, 1, 2].map(|k| {
        let extra = smooth(SPECKLE_LO, SPECKLE_HI, (c0[k] - med[k]).abs());
        let tk = t + (1.0 - t) * extra;
        ((c0[k] + (cb[k] - c0[k]) * tk) * yl).max(0.0)
    })
}

/// Colour noise reduction: the chromaticity (rgb / Y) blurred, mixed in by `nr.t`, under the
/// original luminance; with `half_res` the blur runs at half resolution ([`HALF_RES_NR_SIGMA`]).
/// Pixels that are speckle outliers against their neighbourhood are pulled in harder
/// ([`SPECKLE_LO`], [`nr_pixel`]).
///
/// The blurred chromaticity is luminance weighted (blurred rgb over its blurred Y), not a blur of
/// per-pixel rgb / Y: in deep shadows Y is near zero and noise, so per-pixel ratios explode and
/// spread coloured specks into their neighbours.
fn colour_nr(img: &mut Rgb32f, nr: NrColor, half_res: bool) {
    let w = img.width;
    let med = chroma_plane(img);
    {
        let chroma_of = |c: [f32; 3]| {
            let y = luminance_2020(c).max(1e-6);
            [c[0] / y, c[1] / y, c[2] / y]
        };
        if half_res && img.width >= 2 && img.height >= 2 {
            // chromaticity is smooth at these radii: blur it at half resolution (a quarter of
            // the pixels, half the radius) and read it back bilinearly
            let (hw, hh) = (img.width.div_ceil(2), img.height.div_ceil(2));
            let src = &*img;
            let mut half = Rgb32f::new(hw, hh);
            for_rows(&mut half.data, hw, |y, row| {
                for (x, o) in row.iter_mut().enumerate() {
                    let (sx, sy) = (2 * x as isize, 2 * y as isize);
                    let mut acc = [0.0f32; 3];
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let c = src.get_clamped(sx + dx, sy + dy).map(|v| v.max(0.0));
                        acc = [0, 1, 2].map(|k| acc[k] + 0.25 * c[k]);
                    }
                    *o = acc;
                }
            });
            let b = gaussian(&half, nr.sigma * 0.5);
            for_rows(&mut img.data, w, |y, row| {
                // half-res pixel centres sit at 2x + 0.5 in full-res pixel units
                let fy = ((y as f32 - 0.5) * 0.5).max(0.0);
                let (y0, ty) = (fy as usize, fy.fract());
                let y1 = (y0 + 1).min(hh - 1);
                let (r0, r1) = (b.data.get(y0 * hw..(y0 + 1) * hw).unwrap_or(&[]), b.data.get(y1 * hw..(y1 + 1) * hw).unwrap_or(&[]));
                for (x, p) in row.iter_mut().enumerate() {
                    let fx = ((x as f32 - 0.5) * 0.5).max(0.0);
                    let (x0, tx) = (fx as usize, fx.fract());
                    let x1 = (x0 + 1).min(hw - 1);
                    let g = |r: &[[f32; 3]], i: usize| r.get(i).copied().unwrap_or_default();
                    let (a, bb, c, d) = (g(r0, x0), g(r0, x1), g(r1, x0), g(r1, x1));
                    let cb = chroma_of([0, 1, 2].map(|k| {
                        let top = a[k] + (bb[k] - a[k]) * tx;
                        let bot = c[k] + (d[k] - c[k]) * tx;
                        top + (bot - top) * ty
                    }));
                    let yl = luminance_2020(*p);
                    let c0 = chroma_of(*p);
                    let md = median_chroma3x3(&med, x, y);
                    *p = nr_pixel(c0, cb, md, nr.t, yl);
                }
            });
        } else {
            let b = gaussian(&img.map(|c| c.map(|v| v.max(0.0))), nr.sigma);
            for_rows(&mut img.data, w, |y, row| {
                for (x, p) in row.iter_mut().enumerate() {
                    let i = y * w + x;
                    let yl = luminance_2020(*p);
                    let c0 = chroma_of(*p);
                    let Some(&bl) = b.data.get(i) else { continue };
                    let cb = chroma_of(bl);
                    let md = median_chroma3x3(&med, x, y);
                    *p = nr_pixel(c0, cb, md, nr.t, yl);
                }
            });
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
    /// The image's key without its noise reduction (`Plan::pre_nr_key`).
    pub pre_nr: u64,
    pub log_l: Option<Arc<Plane>>,
    /// Keyed by its radius, process and median radius ([`BaseKey`]).
    pub base: Option<(BaseKey, Arc<Plane>)>,
    pub clarity: Option<(u32, Arc<Plane>)>,
    pub texture: Option<(u32, Arc<Plane>)>,
    /// Sharpening blur (exact Gaussian at the sharpen radius).
    pub sharpen: Option<(u32, Arc<Plane>)>,
    /// Dark channel and its airlight.
    pub dark: Option<(u64, Arc<Plane>, f32)>,
    /// Blurred chromaticity (local Moiré / Noise).
    pub chroma: Option<(u32, Arc<Rgb32f>)>,
    /// The visible masks' alpha planes, in order ([`masks::alpha_key`]; the mattes they were
    /// evaluated with, compared by identity).
    pub masks: Option<MaskPlanes>,
}

/// Cached mask alpha planes (see [`Planes::masks`]).
#[derive(Clone)]
pub(crate) struct MaskPlanes {
    pub key: u64,
    pub mattes: Option<Arc<masks::Mattes>>,
    pub alphas: Vec<Arc<Plane>>,
}

impl MaskPlanes {
    fn fits(&self, key: u64, mattes: Option<&Arc<masks::Mattes>>) -> bool {
        self.key == key
            && match (&self.mattes, mattes) {
                (None, None) => true,
                (Some(a), Some(b)) => masks::Mattes::same(a, b),
                _ => false,
            }
    }
}

impl Planes {
    /// The planes that still fit an image differing from this one only in noise reduction, close
    /// enough for a draft: the wide blurs (base, clarity, dehaze, chromaticity). Its `key` matches
    /// no image, so only another draft takes them.
    pub fn for_other_nr(self) -> Planes {
        // (the masks are evaluated on the image itself: redone)
        Planes { key: 0, pre_nr: self.pre_nr, log_l: None, texture: None, sharpen: None, masks: None, ..self }
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
    /// Sharpening blur (exact Gaussian, see [`sharpen_sigma`]).
    pub sharpen: Option<f32>,
    /// Dehaze dark channel (Gaussian).
    pub dark: Option<f32>,
    /// Patch radius (px) of that dark channel ([`dehaze_patch_radius`]: 0 before V3).
    pub dark_patch: usize,
    /// Chromaticity blur for local Moiré / colour noise (Gaussian).
    pub chroma: Option<f32>,
}

/// Sharpening blur radius in output pixels before the floor of [`sharpen_sigma`]: Radius (source pixels) times the
/// output pixels per source pixel (`src_long` = the oriented source's long edge), so a preview and
/// an export show the same detail at the same displayed scale.
pub fn sharpen_sigma_raw(s: &DevelopSettings, px_per_long: f64, src_long: f64) -> f32 {
    let r = s.detail.sharpen_radius;
    let radius = if r.is_finite() { r.clamp(0.1, 10.0) } else { 1.0 };
    (radius * px_per_long / src_long.max(1.0)) as f32
}

/// Smallest sigma (px) of the sharpening blur. There is no ceiling: an enlarged export scales
/// the radius with it, so distinct radii stay distinct at every scale.
pub const SHARPEN_SIGMA_MIN: f32 = 0.3;

/// The sharpening blur's sigma for a raw value of [`sharpen_sigma_raw`].
pub fn sharpen_sigma(raw: f32) -> f32 {
    raw.max(SHARPEN_SIGMA_MIN)
}

/// How much of the sharpening survives at a raw sigma: none at or below 0.3 px, all from 0.5 px
/// (a blur that small cannot tell detail from the pixel grid, so it fades instead of aliasing).
pub fn sharpen_fade(raw: f32) -> f32 {
    let t = ((raw - SHARPEN_SIGMA_MIN) / (0.5 - SHARPEN_SIGMA_MIN)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Texture band radius as a fraction of the long edge.
pub const TEXTURE_SIGMA: f32 = 0.0018;
/// Smallest texture band radius (px) a render filters with; smaller previews fade the effect instead.
pub const TEXTURE_SIGMA_MIN: f32 = 0.6;

/// How much of global Texture a render at `px_per_long` shows: 1 when the texture band is at least
/// [`TEXTURE_SIGMA_MIN`], else the ratio of variances of the wanted and the floored band, so a small
/// preview (whose band is floored, i.e. relatively wider) doesn't show a stronger effect than the export.
pub fn texture_fade(px_per_long: f64) -> f32 {
    let want = TEXTURE_SIGMA * px_per_long as f32;
    if !want.is_finite() || want <= 0.0 {
        return 0.0;
    }
    (want / TEXTURE_SIGMA_MIN).powi(2).min(1.0)
}

pub fn plane_sigmas(s: &DevelopSettings, px_per_long: f64, src_long: f64, _quality: Quality) -> PlaneSigmas {
    let ppl = px_per_long as f32;
    // A hidden mask, or one with no components, does not change the picture. Its sliders must
    // not pick the tone base Clarity reads (#1214).
    let live = |m: &&lcc_develop::Mask| m.visible && !m.components.is_empty();
    let local_any = |f: fn(&lcc_develop::LocalAdjustments) -> f64| s.masks.iter().filter(live).any(|m| f(&m.adjust) != 0.0);
    let tone_active = s.light.highlights != 0.0
        || s.light.shadows != 0.0
        || s.masks.iter().filter(live).any(|m| m.adjust.highlights != 0.0 || m.adjust.shadows != 0.0);
    // Edge-aware base at ~1.5% of the long edge (EV² epsilon: edges of > ~0.6 EV are preserved)
    // From V2 it is taken of the median at 0.15%: lines and texture finer than that are detail,
    // not tone (#632).
    // (a draft takes the same width as the full render: drafts once capped it at 24 px, which
    // changed the large-scale tone of every draft wider than ~1600 px, most at 100 %, where the
    // base is ~90 px wide. The fast guided filter costs no more for a wider one.)
    let base = tone_active.then(|| (0.015 * ppl).max(1.0));
    let clarity = (s.effects.clarity != 0.0 || local_any(|a| a.clarity)).then(|| (0.012 * ppl).max(1.0));
    // local Noise and Defringe read the fine detail band too
    let v4 = crate::finish::sharpen_v4(s);
    let sharpening = s.detail.sharpen_amount != 0.0 || local_any(|a| a.sharpness);
    // before V4, sharpening boosts this band too
    let texture = (s.effects.texture != 0.0
        || (!v4 && sharpening)
        || local_any(|a| a.texture)
        || local_any(|a| a.noise)
        || s.masks.iter().filter(live).any(|m| m.adjust.defringe > 0.0))
    .then(|| (TEXTURE_SIGMA * ppl).max(TEXTURE_SIGMA_MIN));
    let dark = (s.effects.dehaze != 0.0 || local_any(|a| a.dehaze)).then(|| (0.02 * ppl).max(1.0));
    let chroma = (local_any(|a| a.moire) || s.masks.iter().filter(live).any(|m| m.adjust.noise > 0.0)).then(|| (CHROMA_SIGMA * ppl).max(1.0));
    let base_detail = if base.is_some() { tone_base_detail(s, px_per_long) } else { 0 };
    let dark_patch = if dark.is_some() { dehaze_patch_radius(s, px_per_long) } else { 0 };
    // sharpening (Amount, or a mask's Sharpness) has its own pixel-scale band, off when it would fade away
    let sharpen_raw = sharpen_sigma_raw(s, px_per_long, src_long);
    let sharpen = (v4 && sharpening && sharpen_fade(sharpen_raw) > 0.0).then(|| sharpen_sigma(sharpen_raw));
    PlaneSigmas { base, base_detail, clarity, texture, sharpen, dark, dark_patch, chroma }
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
    mattes: Option<&Arc<masks::Mattes>>,
) -> Prepared {
    let log_l = planes.log_l.get_or_insert_with(|| Arc::new(timed("log_l", || img.map(log_lum)))).clone();
    let PlaneSigmas {
        base: base_sigma,
        base_detail,
        clarity: clarity_sigma,
        texture: texture_sigma,
        sharpen: sharpen_sigma_px,
        dark: dark_sigma,
        dark_patch,
        chroma: chroma_sigma,
    } = plane_sigmas(s, px_per_long, frame.ow.max(frame.oh), q);

    let Planes { base: sb, clarity: sc, texture: st, sharpen: ss, dark: sd, chroma: sch, .. } = planes;
    let chroma_blur = chroma_sigma.map(|sg| match sch {
        Some((k, c)) if *k == sg.to_bits() => c.clone(),
        _ => {
            let c = timed("chroma", || Arc::new(gaussian(&img.map(masks::chromaticity), sg)));
            *sch = Some((sg.to_bits(), c.clone()));
            c
        }
    });
    let l = &log_l;
    let (base, (clarity_blur, ((texture_blur, sharpen_blur), dark))) = par_join(
        || base_sigma.map(|sg| plane_at(sb, base_key(s, sg, base_detail), || timed("base", || tone_base(l, sg, base_detail)))),
        || {
            par_join(
                || clarity_sigma.map(|sg| plane_at(sc, sg.to_bits(), || timed("clarity", || guided_fast(l, sg, CLARITY_EPS)))),
                || {
                    par_join(
                        || {
                            par_join(
                                || texture_sigma.map(|sg| plane_at(st, sg.to_bits(), || timed("texture", || gaussian(l, sg)))),
                                || sharpen_sigma_px.map(|sg| plane_at(ss, sg.to_bits(), || timed("sharpen", || gaussian_taps(l, &gauss_taps(sg))))),
                            )
                        },
                        || {
                            dark_sigma.map(|sg| match sd {
                                Some((k, d, air)) if *k == dark_key(sg, dark_patch) => (d.clone(), *air),
                                _ => {
                                    let d = timed("dehaze", || Arc::new(dark_plane(&img, sg, dark_patch)));
                                    let air = airlight(&d);
                                    *sd = Some((dark_key(sg, dark_patch), d.clone(), air));
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
    let key = masks::alpha_key(&s.masks, ev, img.width, img.height, mattes.map(|m| &**m));
    let visible: Vec<_> = s.masks.iter().filter(|m| m.visible && !m.components.is_empty()).collect();
    let masks = match &planes.masks {
        // the same masks on the same image: their planes, with the adjustments as they are now
        Some(c) if c.fits(key, mattes) && c.alphas.len() == visible.len() => visible
            .iter()
            .zip(&c.alphas)
            .map(|(m, a)| masks::Evaluated { id: m.id, alpha: a.clone(), adjust: m.adjust, curve: crate::finish::local_curve_luts(&m.curve) })
            .collect(),
        _ => {
            let e = timed("masks", || masks::evaluate(&s.masks, frame, img.width, img.height, &img, &log_l, ev, mattes.map(|m| &**m)));
            planes.masks = Some(MaskPlanes { key, mattes: mattes.cloned(), alphas: e.iter().map(|e| e.alpha.clone()).collect() });
            e
        }
    };
    Prepared { img, log_l, base, clarity_blur, texture_blur, sharpen_blur, dark, chroma_blur, air, masks, mask_tone: Vec::new(), px_per_long }
}

/// The airlight of the whole output frame, estimated on a small render of it (`proxy_w` px wide):
/// a windowed render must dehaze with the frame's airlight, not that of the pixels it holds.
pub(crate) fn frame_airlight(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, frame: &Frame, proxy_w: usize, proxy_h: usize) -> f32 {
    let mut img = Rgb32f::clone(&frame_proxy(src, frame, proxy_w, proxy_h));
    white_balance(&mut img, info, s);
    let ppl = frame.px_per_long(proxy_w);
    let sigma = (0.02 * ppl).max(1.0) as f32;
    airlight(&dark_plane(&img, sigma, dehaze_patch_radius(s, ppl)))
}

/// How many airlight proxies [`frame_proxy`] keeps (one per photo and frame being zoomed into; each ≤ 1.8 MB).
const FRAME_PROXIES_KEPT: usize = 4;

/// `frame.sample(src, w, h)`, kept for the last few (source, frame, size): sampling a 64 MP original down to the
/// airlight proxy reads all of it (~190 ms of CPU), and every window of a zoomed view with Dehaze asked for it again,
/// on each pan and slider tick. The proxy is taken before white balance, so a WB change still hits.
pub(crate) fn frame_proxy(src: &Rgb32f, frame: &Frame, w: usize, h: usize) -> Arc<Rgb32f> {
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
    use super::*;
    use lcc_color::cct::xy_to_uv;

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
        warm.wb.mode = lcc_develop::WbMode::Custom;
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
            let ppl = frame.px_per_long(48);
            airlight(&dark_plane(&img, (0.02 * ppl).max(1.0) as f32, dehaze_patch_radius(&s, ppl)))
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
        use lcc_develop::{Mask, MaskComponent, MaskOp, MaskShape};
        use lcc_geom::Point;
        let shape = MaskShape::Linear { start: Point::new(0.0, 0.0), end: Point::new(1.0, 0.0) };
        let component = MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape };
        let mut s = DevelopSettings::default();
        s.masks.push(Mask {
            visible: false,
            components: vec![component],
            adjust: lcc_develop::LocalAdjustments { highlights: 40.0, ..Default::default() },
            ..Default::default()
        });
        assert!(plane_sigmas(&s, 2000.0, 2000.0, Quality::Full).base.is_none());
        s.masks[0].visible = true;
        s.masks[0].components.clear();
        assert!(plane_sigmas(&s, 2000.0, 2000.0, Quality::Full).base.is_none());
        s.masks[0].components.push(MaskComponent {
            anchor: None,
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.0, 0.0), end: Point::new(1.0, 0.0) },
        });
        assert!(plane_sigmas(&s, 2000.0, 2000.0, Quality::Full).base.is_some());
    }

    #[test]
    fn exposure_doubles() {
        let mut s = DevelopSettings::default();
        s.light.exposure = 1.0;
        let mut img = Rgb32f::filled(2, 2, [0.1, 0.2, 0.3]);
        scene_linear_pre(&mut img, &SourceInfo::default(), &s);
        assert!((img.get(1, 1)[2] - 0.6).abs() < 1e-5);
    }

    /// [`kelvin_to_legacy`] inverts [`legacy_to_kelvin`] over the whole relative slider range and back: the
    /// round trip returns the start, in both directions, to the precision of the Kelvin / tint parametrisation.
    #[test]
    fn kelvin_to_legacy_inverts_legacy_to_kelvin() {
        for as_shot in [(4986.0, -2.0), (6500.0, 0.0), (3200.0, 14.0), (9500.0, -12.0)] {
            // the relative slider's whole reach: temp 0..100 (2000–13550 K around 6500), tint −150..150
            for pct in [0.0, 12.5, 25.0, 50.0, 75.0, 100.0] {
                for tint in [-150.0, -40.0, 0.0, 33.0, 150.0] {
                    let (lt, ltint) = controls_relative(pct, tint);
                    let (kt, ktint) = legacy_to_kelvin(lt, ltint, as_shot);
                    // past the Kelvin parametrisation's own reach (its search runs 1000–50000 K) the forward
                    // conversion has already clamped, so the pair is not expected to come back
                    if !(1050.0..=49_000.0).contains(&kt) {
                        continue;
                    }
                    let back = kelvin_to_legacy(kt, ktint, as_shot).unwrap();
                    let (x1, y1) = xy_to_uv(temp_tint_to_xy(lt, ltint));
                    let (x2, y2) = xy_to_uv(temp_tint_to_xy(back.0, back.1));
                    // the pair may differ by more than the slider's own step at the extreme corners (the Kelvin
                    // parametrisation is read back through a search), but never by much, and the white it renders
                    // is the same
                    assert!(
                        (back.0 - lt).abs() <= lt.abs() * 0.01 && (back.1 - ltint).abs() < 0.5,
                        "as shot {as_shot:?}: relative ({lt}, {ltint}) -> kelvin ({kt}, {ktint}) -> {back:?}"
                    );
                    assert!((x1 - x2).hypot(y1 - y2) < 1e-3, "as shot {as_shot:?}: relative ({lt}, {ltint}) -> kelvin ({kt}, {ktint}) -> {back:?}");
                }
            }
            // the other direction: every white the relative scale can hold maps back to a pair that renders the same
            for kt in [as_shot.0, 2500.0, 4200.0, 6500.0, 12000.0] {
                for ktint in [-60.0, 0.0, 45.0] {
                    let Some((lt, ltint)) = kelvin_to_legacy(kt, ktint, as_shot) else { continue };
                    // outside the slider's reach the relative scale has no value for this white: the forward
                    // conversion would clamp, so the round trip is not expected to close there
                    if !(2000.0..=13550.0).contains(&lt) || ltint.abs() > 150.0 {
                        continue;
                    }
                    let (kt2, ktint2) = legacy_to_kelvin(lt, ltint, as_shot);
                    let (x1, y1) = xy_to_uv(temp_tint_to_xy(kt, ktint));
                    let (x2, y2) = xy_to_uv(temp_tint_to_xy(kt2, ktint2));
                    assert!(
                        (x1 - x2).hypot(y1 - y2) < 1e-3,
                        "as shot {as_shot:?}: kelvin ({kt}, {ktint}) -> relative ({lt}, {ltint}) -> ({kt2}, {ktint2})"
                    );
                }
            }
            // the source's own white is the relative scale's as-shot reference, exactly
            assert_eq!(kelvin_to_legacy(as_shot.0, as_shot.1, as_shot), Some((6500.0, 0.0)));
        }
        // the exact inverse of the special case inside legacy_to_kelvin
        assert_eq!(kelvin_to_legacy(4986.0, -2.0, (4986.0, -2.0)), Some((6500.0, 0.0)));
        assert_eq!(legacy_to_kelvin(6500.0, 0.0, (4986.0, -2.0)), (4986.0, -2.0));
        // hostile numbers answer None, never a panic or a plausible lie
        assert_eq!(kelvin_to_legacy(f64::NAN, 0.0, (5000.0, 0.0)), None);
        assert_eq!(kelvin_to_legacy(5000.0, f64::INFINITY, (5000.0, 0.0)), None);
        assert_eq!(kelvin_to_legacy(5000.0, 0.0, (f64::NAN, 0.0)), None);
    }

    /// The relative scale's `(temp, tint)` for a slider temperature at `pct` % (2000–7000 K fill the left half) and tint.
    fn controls_relative(pct: f64, tint: f64) -> (f64, f64) {
        let t = if pct <= 50.0 { 2000.0 + pct * 100.0 } else { 7000.0 + (pct - 50.0) * 131.0 };
        (t, tint)
    }
}

#[cfg(test)]
mod fast_tests {
    use super::*;

    /// The previous fast path, including its two materialized coefficient resizes.
    fn resized_reference(p: &Plane, sigma: f32, eps: f32) -> Plane {
        use lcc_raster::resample::{Filter, resize};
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
mod nrc_tests {
    use super::*;
    use lcc_develop::{DevelopSettings, Process};

    fn noisy_halves(w: usize, h: usize) -> Rgb32f {
        Rgb32f::from_fn(w, h, |x, y| {
            let n = |c: u32| {
                let mut v = (x as u32).wrapping_mul(0x9e37_79b9) ^ (y as u32).wrapping_mul(0x85eb_ca6b) ^ c.wrapping_mul(0xc2b2_ae35) | 1;
                v ^= v << 13;
                v ^= v >> 17;
                v ^= v << 5;
                (v as f32 / u32::MAX as f32 - 0.5) * 0.02
            };
            let base = if x < w / 2 { [0.06, 0.03, 0.02] } else { [0.02, 0.04, 0.06] };
            [base[0] + n(1), base[1] + n(2), base[2] + n(3)]
        })
    }

    /// Edits saved under V1 to V7 keep the blurred chromaticity with its speckle median (the colour NR
    /// they always had); V8 gets the joint method of `nrc_joint`.
    #[test]
    fn colour_nr_follows_the_process() {
        let img = noisy_halves(96, 64);
        let run = |p: Process| {
            let mut s = DevelopSettings { process: p.version(), ..Default::default() };
            s.detail.nr_color = 50.0;
            let mut o = img.clone();
            denoise(&mut o, &s, 1.0);
            o
        };
        // the colour NR V1 to V7 had before the joint method: `apply_nr` with the blurred chromaticity
        let mut s = DevelopSettings { process: Process::V7.version(), ..Default::default() };
        s.detail.nr_color = 50.0;
        let (lum, col) = nr_params(&s, 1.0);
        let mut before = img.clone();
        apply_nr(&mut before, lum, col, false);
        for p in Process::ALL.into_iter().filter(|p| *p < Process::V8) {
            assert!(run(p).data == before.data, "{p:?} keeps the blurred chromaticity");
        }
        let v8 = run(Process::V8);
        assert!(v8.data != before.data, "V8 takes the joint colour NR");
        s.process = Process::V8.version();
        let mut joint = img.clone();
        let p = nrc_params(&s, 1.0).expect("Color 50 has parameters");
        crate::nrc_joint::denoise(&mut joint, &p, (s.detail.nr_color_detail / 100.0) as f32, 0.5, None);
        assert!(joint.data == v8.data, "V8 is the joint method");
        // the joint method keeps each pixel's luminance
        for (a, b) in img.data.iter().zip(&v8.data) {
            assert!((luminance_2020(*a) - luminance_2020(*b)).abs() < 1e-6);
        }
    }

    /// Hostile sizes, slider values and pixels: the joint colour NR returns without a panic, keeps the
    /// image's size, and leaves finite pixels finite where its input was finite throughout.
    #[test]
    fn joint_colour_nr_survives_hostile_input() {
        let sizes = [(0usize, 0usize), (1, 1), (2, 3), (3, 2), (1, 40), (40, 1), (5, 5), (17, 9)];
        let colours = [0.0, 25.0, 100.0, 1e30, -50.0, f64::NAN, f64::INFINITY];
        for (w, h) in sizes {
            let clean = noisy_halves(w, h);
            let mut with_nan = clean.clone();
            if let Some(p) = with_nan.data.get_mut(w * h / 2) {
                *p = [f32::NAN, f32::INFINITY, -f32::INFINITY];
            }
            for colour in colours {
                for detail in [0.0, 50.0, 100.0, f64::NAN] {
                    for smoothness in [0.0, 100.0, -1e9, f64::NAN] {
                        let mut s = DevelopSettings { process: Process::V8.version(), ..Default::default() };
                        s.detail.nr_color = colour;
                        s.detail.nr_color_detail = detail;
                        s.detail.nr_color_smoothness = smoothness;
                        for scale in [1.0, 0.25, 0.0, f32::NAN] {
                            let mut a = clean.clone();
                            denoise(&mut a, &s, scale);
                            assert_eq!((a.width, a.height, a.data.len()), (w, h, w * h));
                            assert!(
                                a.data.iter().flatten().all(|v| v.is_finite()),
                                "{w}x{h} Color {colour} detail {detail} smoothness {smoothness} scale {scale}"
                            );
                            let mut b = with_nan.clone();
                            denoise(&mut b, &s, scale);
                            assert_eq!((b.width, b.height, b.data.len()), (w, h, w * h));
                        }
                    }
                }
            }
        }
        // an odd window (one tile of a frame, cut anywhere) and an empty set of tiles
        let tiles = [noisy_halves(3, 129), Rgb32f::new(0, 0), noisy_halves(2, 2)];
        let noise = crate::nrc_joint::noise_of_tiles(&tiles);
        assert!(noise.model.alpha.iter().chain(&noise.model.beta).all(|v| v.is_finite()));
        let _ = crate::nrc_joint::noise_of_tiles(&[]);
        for (w, h) in [(0, 0), (1, 1), (255, 300), (usize::MAX / 2, 3)] {
            let _ = crate::nrc_joint::tiles(w, h);
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

    /// PROBE (ignored): float mean of a flat, noisy dark patch over three noise realisations under the
    /// joint colour NR; asserts the means stay put.
    #[test]
    #[ignore]
    fn probe_nrc_mean_drift() {
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
                ("dark, red near 0", [0.0015, 0.004, 0.009], [0.004, 0.003, 0.005]),
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
                    let mut s = DevelopSettings { process: Process::V8.version(), ..Default::default() };
                    s.detail.nr_color = nr;
                    let mut a = img.clone();
                    denoise(&mut a, &s, 1.0);
                    let m = mean(&a);
                    worst = worst.max([0, 1, 2].iter().map(|&i| (m[i] / m0[i] - 1.0).abs()).fold(0.0, f64::max));
                    println!("{name} NR {nr}: mean {m0:.5?} -> {m:.5?}");
                }
            }
        }
        println!("worst mean deviation {:.4} %", 100.0 * worst);
        assert!(worst < 0.0005, "the joint colour NR moved a channel's mean by {:.3} %", 100.0 * worst);
    }
}
