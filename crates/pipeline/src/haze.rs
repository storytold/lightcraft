//! The haze map of process V4's Dehaze (issue #1237): a dark-channel-prior estimate of how much of
//! each pixel is haze, computed once per image on a fixed-size proxy of the whole frame.
//!
//! Every render of the photo (any preview size, a zoomed window, the export, the GPU) samples the
//! same map by frame-normalised coordinates, so they all dehaze alike and none needs a margin or a
//! mid-render airlight readback.
//!
//! Built on a proxy of the whole output frame, long edge `P = min(`[`PROXY_EDGE`]`, native)`, after
//! white balance (before exposure), in the gamma encoding `e = √lin` (scale-invariant: the map
//! doesn't change with Exposure):
//! 1. airlight `A`: the per-channel median of the top [`AIR_TOP`] of the patch dark channel,
//!    leaving out the brightest [`AIR_SKIP_BRIGHT`] of the frame (the sun, clipped highlights),
//!    pulled [`AIR_NEUTRAL`] of the way toward neutral and floored at [`AIR_FLOOR`] (`A'`);
//! 2. normalised dark `g = min_c(e_c / A'_c)` (0..[`G_MAX`]), also the guide;
//! 3. patch dark channel `d = minfilter(g, `[`PATCH`]`·P)` ([`lightcraft_raster::morph`]);
//! 4. guided refinement of `d` by `g` ([`GUIDE_RADIUS`]`·P`, [`GUIDE_EPS`]), keeping the
//!    coefficients `a`, `b` at proxy resolution: a full-resolution pixel's haze is
//!    `d̃ = clamp(a·g_px + b, 0, 1)` with its own `g_px`, so edges stay sharp at any size;
//! 5. an exposure-compensation LUT ([`COMP_N`] amounts): how much the recovery darkens the median
//!    at each amount, so Dehaze clears haze without making the photo darker.
//!
//! [`dehaze_at`] is the per-pixel V4 Dehaze the CPU renderer applies with it (the GPU's `finish`
//! kernel mirrors it).

use std::sync::{Arc, Mutex};

use lightcraft_color::luminance_2020;
use lightcraft_develop::{DevelopSettings, Process};
use lightcraft_raster::{Plane, Rgb32f};

use crate::SourceInfo;
use crate::geometry::Frame;

/// Long edge (px) of the proxy the map is built on (smaller frames use their own size).
pub const PROXY_EDGE: usize = 768;
/// Share of the patch dark channel (its brightest end) the airlight is picked from.
pub const AIR_TOP: f32 = 0.001;
/// Share of the brightest pixels (the sun, its glare, clipped highlights) left out of the airlight.
pub const AIR_SKIP_BRIGHT: f32 = 0.01;
/// How far the airlight is pulled toward a neutral of its own luminance (0 = as picked).
pub const AIR_NEUTRAL: f32 = 0.7;
/// Lowest airlight channel (encoded): no division by zero, no runaway gain on dark photos.
pub const AIR_FLOOR: f32 = 0.05;
/// Upper clamp of the normalised dark `g`.
pub const G_MAX: f32 = 1.5;
/// Patch (min filter) radius as a fraction of the proxy's long edge.
pub const PATCH: f32 = 0.012;
/// Guided refinement radius as a fraction of the proxy's long edge (Gaussian windows with the
/// spread of a box of this radius: `σ = r/√3`).
pub const GUIDE_RADIUS: f32 = 0.05;
/// Guided refinement epsilon (in units of `g²`).
pub const GUIDE_EPS: f32 = 1e-3;
/// Entries of the exposure-compensation LUT: amounts `0.05, 0.10, …, 1.0`.
pub const COMP_N: usize = 20;
/// Largest exposure compensation before the [`COMP_POWER`] (EV).
pub const COMP_MAX_EV: f32 = 1.4;
/// Share (as a power) of the measured median darkening the compensation gives back.
pub const COMP_POWER: f32 = 0.55;
/// The compensation LUT measures every `COMP_STEP`-th proxy pixel.
pub const COMP_STEP: usize = 7;

/// Haze strength at +100: `ω = OMEGA·k^OMEGA_POWER`.
pub const OMEGA: f32 = 0.95;
pub const OMEGA_POWER: f32 = 0.6;
/// Lowest transmission at +100 (`t ≥ lerp(1, T_FLOOR, k)`): bounds the gain at `1/T_FLOOR`.
pub const T_FLOOR: f32 = 0.35;
/// Sky tolerance: pixels within this of the airlight (encoded, per channel, relative to its brightest
/// channel) are recovered less.
pub const SKY_K: f32 = 0.3;
/// Share of the input's chromaticity blended back after recovery (keeps colours from swinging).
pub const CHROMA_KEEP: f32 = 0.5;
/// Negative Dehaze: veil strength at −100 and the share of it that doesn't depend on the haze.
pub const VEIL: f32 = 0.45;
pub const VEIL_BASE: f32 = 0.4;
/// Brightest channel (linear, pre-exposure) of the colour the veil mixes toward: the airlight,
/// scaled down when brighter, so a veil over a bright sky or sun doesn't clip it to white.
pub const VEIL_TOP: f32 = 1.0;

/// The haze map of one photo (see the module docs). Frame-normalised: [`Self::coeffs`] takes
/// coordinates in 0..1 over the whole output frame.
#[derive(Clone, Debug, PartialEq)]
pub struct HazeMap {
    /// Guided-filter coefficients at proxy resolution: `d̃ = clamp(a·g + b, 0, 1)`.
    pub a: Plane,
    pub b: Plane,
    /// Airlight `A'` in the square-root encoding ([`encode`]), every channel ≥ [`AIR_FLOOR`].
    pub air: [f32; 3],
    /// Exposure compensation (a gain ≥ 1) at amounts `0.05·(i+1)`.
    pub comp: [f32; COMP_N],
}

/// The square-root encoding the map works in (`√lin`; cheap per pixel on the CPU and the GPU);
/// non-finite and negative values encode as 0.
#[inline]
pub fn encode(v: f32) -> f32 {
    if v.is_finite() && v > 0.0 { v.sqrt() } else { 0.0 }
}

/// Inverse of [`encode`] (`v²`).
#[inline]
pub fn decode(v: f32) -> f32 {
    if v.is_finite() && v > 0.0 { v * v } else { 0.0 }
}

fn encode3(c: [f32; 3]) -> [f32; 3] {
    c.map(encode)
}

/// `1 / max(A_lin, AIR_FLOOR²)` of the encoded airlight `air`: what [`norm_dark_lin`] multiplies by.
fn inv_air_lin(air: [f32; 3]) -> [f32; 3] {
    air.map(|a| 1.0 / decode(a).max(AIR_FLOOR * AIR_FLOOR))
}

/// Normalised dark `min_c(e_c / A_c)` of a linear pixel, clamped to 0..[`G_MAX`] (0 for NaN), with
/// `inv` = [`inv_air_lin`]. It encodes once instead of three times: the encoding is monotonic, so
/// `min_c(e_c / A_c) = √min_c(lin_c / A_lin_c)`.
#[inline]
fn norm_dark_lin(lin: [f32; 3], inv: [f32; 3]) -> f32 {
    let r = (0..3).fold(f32::INFINITY, |m, i| {
        let v = lin[i];
        let v = if v.is_finite() && v > 0.0 { v } else { 0.0 };
        m.min(v * inv[i])
    });
    let g = encode(r);
    if g.is_finite() { g.clamp(0.0, G_MAX) } else { 0.0 }
}

/// One axis of the bilinear sampling: `(i0, i1, f)` of frame-normalised `n` (outside 0..1, the
/// edge's; NaN, the middle) over `len` proxy pixel centres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    i0: usize,
    i1: usize,
    f: f32,
}

impl Axis {
    pub fn new(n: f32, len: usize) -> Axis {
        let last = len.saturating_sub(1);
        let n = if n.is_finite() { n } else { 0.5 };
        let p = (n * len as f32 - 0.5).clamp(0.0, last as f32);
        let i0 = p.floor() as usize;
        Axis { i0, i1: (i0 + 1).min(last), f: p - i0 as f32 }
    }

    /// `v0` to `v1` by `f`.
    #[inline]
    fn mix(&self, v0: f32, v1: f32) -> f32 {
        v0 + (v1 - v0) * self.f
    }

    /// `v[i0]` to `v[i1]` by `f` (0 outside `v`).
    #[inline]
    fn lerp(&self, v: &[f32]) -> f32 {
        let at = |i: usize| v.get(i).copied().unwrap_or(0.0);
        self.mix(at(self.i0), at(self.i1))
    }
}

/// The map's coefficients `a`, `b` blended between two proxy rows ([`HazeMap::row`]): one output
/// row's vertical half of the bilinear sampling, done once per row instead of once per pixel.
#[derive(Clone, Debug, Default)]
pub struct HazeRow {
    a: Vec<f32>,
    b: Vec<f32>,
}

impl HazeRow {
    /// The coefficients `(a, b)` at the column `x` ([`Axis::new`] of the pixel's `nx` over the
    /// proxy width): [`HazeMap::coeffs`] with this row's `ny`.
    #[inline]
    pub fn coeffs(&self, x: &Axis) -> (f32, f32) {
        (x.lerp(&self.a), x.lerp(&self.b))
    }
}

impl HazeMap {
    /// The map of `img`: the white-balanced (linear, pre-exposure) proxy of the whole frame.
    pub fn build(img: &Rgb32f) -> HazeMap {
        let (w, h) = (img.width, img.height);
        if w == 0 || h == 0 || img.data.len() != w.saturating_mul(h) {
            return HazeMap::empty();
        }
        let long = w.max(h) as f32;
        let r = ((PATCH * long).round() as usize).max(1);
        let e = img.map(encode3);
        // 1. airlight
        let dark = crate::timed("haze dark", || lightcraft_raster::morph::min_filter(&e.map(crate::local::dark_of), r));
        let picked = crate::timed("haze air", || haze_colour(&e, &dark)).unwrap_or([1.0; 3]);
        let y = luminance_2020(picked);
        let air = picked.map(|c| {
            let v = c + (y - c) * AIR_NEUTRAL;
            if v.is_finite() { v.max(AIR_FLOOR) } else { 1.0 }
        });
        // 2.-4. normalised dark, patch minimum, guided refinement
        let inv = inv_air_lin(air);
        let g = img.map(|p| norm_dark_lin(p, inv));
        let d = lightcraft_raster::morph::min_filter(&g, r);
        let sigma = (GUIDE_RADIUS * long / 3f32.sqrt()).max(1.0);
        let (a, b) = crate::timed("haze guided", || crate::masks::guided_cross_coeffs(&g, &d, sigma, GUIDE_EPS));
        let sane = |p: Plane| p.map(|v| if v.is_finite() { v } else { 0.0 });
        let mut map = HazeMap { a: sane(a), b: sane(b), air, comp: [1.0; COMP_N] };
        // 5. exposure compensation
        map.comp = crate::timed("haze comp", || map.measure_comp(img, &e, &g));
        map
    }

    /// A map that finds no haze anywhere (for an empty image).
    fn empty() -> HazeMap {
        HazeMap { a: Plane::new(1, 1), b: Plane::new(1, 1), air: [1.0; 3], comp: [1.0; COMP_N] }
    }

    /// Bytes the map holds (its coefficient planes and itself).
    pub fn bytes(&self) -> usize {
        (self.a.data.len() + self.b.data.len()) * std::mem::size_of::<f32>() + std::mem::size_of::<HazeMap>()
    }

    /// Proxy size (px).
    pub fn size(&self) -> (usize, usize) {
        (self.a.width, self.a.height)
    }

    /// [`Self::air`] in linear light (pre-exposure).
    pub fn air_lin(&self) -> [f32; 3] {
        self.air.map(decode)
    }

    /// The column [`Axis`] of frame-normalised `nx` (0..1 over the whole output frame).
    pub fn column(&self, nx: f32) -> Axis {
        Axis::new(nx, self.a.width)
    }

    /// The coefficients blended between the two proxy rows around frame-normalised `ny` (0..1 over
    /// the whole output frame): [`HazeRow::coeffs`] then samples any column of it.
    pub fn row(&self, ny: f32) -> HazeRow {
        let (w, h) = self.size();
        let y = Axis::new(ny, h);
        let rows = |p: &Plane| -> Vec<f32> {
            let row = |i: usize| p.data.get(i.saturating_mul(w)..i.saturating_add(1).saturating_mul(w)).unwrap_or(&[]);
            row(y.i0).iter().zip(row(y.i1)).map(|(v0, v1)| y.mix(*v0, *v1)).collect()
        };
        HazeRow { a: rows(&self.a), b: rows(&self.b) }
    }

    /// The coefficients `(a, b)` at frame-normalised point `(nx, ny)` (0..1 over the whole output
    /// frame; outside, the edge's), bilinear over proxy pixel centres: the two rows blended first,
    /// then the two columns, exactly like [`Self::row`] + [`HazeRow::coeffs`] and the GPU.
    pub fn coeffs(&self, nx: f32, ny: f32) -> (f32, f32) {
        let (w, h) = self.size();
        let (x, y) = (Axis::new(nx, w), Axis::new(ny, h));
        let at = |p: &Plane, xi: usize, yi: usize| p.data.get(yi.saturating_mul(w).saturating_add(xi)).copied().unwrap_or(0.0);
        let bil = |p: &Plane| {
            let col = |xi: usize| y.mix(at(p, xi, y.i0), at(p, xi, y.i1));
            x.mix(col(x.i0), col(x.i1))
        };
        (bil(&self.a), bil(&self.b))
    }

    /// Exposure compensation (gain ≥ 1) at amount `k` (linear between the LUT's entries; 1 at 0 and
    /// for negative amounts).
    pub fn comp(&self, k: f32) -> f32 {
        if k.is_nan() || k <= 0.0 {
            return 1.0;
        }
        let f = (k.min(1.0) * COMP_N as f32).clamp(0.0, COMP_N as f32);
        let i = f.floor() as usize;
        let at = |j: usize| if j == 0 { 1.0 } else { self.comp.get(j - 1).copied().unwrap_or(1.0) };
        let (lo, hi) = (at(i), at((i + 1).min(COMP_N)));
        lo + (hi - lo) * (f - i as f32)
    }

    /// The compensation LUT: for each amount, the ratio of the median luminance before and after
    /// the recovery (every [`COMP_STEP`]-th proxy pixel), clamped to `1..2^COMP_MAX_EV` and raised
    /// to [`COMP_POWER`].
    fn measure_comp(&self, img: &Rgb32f, e: &Rgb32f, g: &Plane) -> [f32; COMP_N] {
        let Some(ctx) = Ctx::new(self, 1.0) else {
            return [1.0; COMP_N];
        };
        let samples: Vec<([f32; 3], f32, f32)> = img
            .data
            .iter()
            .zip(&e.data)
            .zip(g.data.iter().zip(self.a.data.iter().zip(&self.b.data)))
            .step_by(COMP_STEP)
            .map(|((c, ev), (gv, (a, b)))| {
                (c.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 }), (a * gv + b).clamp(0.0, 1.0), ctx.sky_distance(*ev))
            })
            .collect();
        let before = median(samples.iter().map(|(c, ..)| luminance_2020(*c)).collect());
        let max = 2f32.powf(COMP_MAX_EV);
        let mut lut = [1.0f32; COMP_N];
        // (one amount per task: each is a pass over the samples and a median)
        lightcraft_raster::par_rows(&mut lut, 1, |i, out| {
            let k = (i + 1) as f32 / COMP_N as f32;
            let am = Amount::new(self, k);
            let after = median(samples.iter().map(|(c, d, m)| luminance_2020(recover_with(&ctx, &am, *c, *d, || sky_weight_at(*m)).0)).collect());
            let ratio = before / after;
            let ratio = if ratio.is_finite() {
                ratio
            } else if before > 0.0 {
                max
            } else {
                1.0
            };
            for o in out.iter_mut() {
                *o = ratio.clamp(1.0, max).powf(COMP_POWER);
            }
        });
        lut
    }
}

/// The airlight colour: the per-channel median of the pixels whose patch dark channel is in its
/// top [`AIR_TOP`], among the pixels that aren't in the brightest [`AIR_SKIP_BRIGHT`] (by
/// luminance of `e`). Skipping those keeps the sun, its glare and clipped highlights (often
/// magenta after white balance, and far above scene white in an HDR merge) out of the airlight;
/// the median keeps one odd pixel from setting its colour. `None` for an empty image.
fn haze_colour(e: &Rgb32f, dark: &Plane) -> Option<[f32; 3]> {
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    let ys: Vec<f32> = e.data.iter().map(|p| finite(luminance_2020(*p))).collect();
    let ymax = nth_from_top(ys.clone(), AIR_SKIP_BRIGHT)?;
    let kept = || e.data.iter().zip(&dark.data).zip(&ys).filter(move |(_, y)| **y <= ymax).map(|((p, d), _)| (*p, finite(*d)));
    let thr = nth_from_top(kept().map(|(_, d)| d).collect(), AIR_TOP)?;
    let cand: Vec<[f32; 3]> = kept().filter(|(_, d)| *d >= thr).map(|(p, _)| p).collect();
    if cand.is_empty() {
        return None;
    }
    Some(std::array::from_fn(|c| median(cand.iter().map(|p| p[c]).collect())))
}

/// The value with a `share` (0..1) of `v` at or above it (`None` when empty).
fn nth_from_top(mut v: Vec<f32>, share: f32) -> Option<f32> {
    let last = v.len().checked_sub(1)?;
    let k = (((v.len() as f32) * (1.0 - share)) as usize).min(last);
    Some(*v.select_nth_unstable_by(k, f32::total_cmp).1)
}

/// Median of `v` (0 when empty).
fn median(mut v: Vec<f32>) -> f32 {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return 0.0;
    }
    let k = v.len() / 2;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// What the per-pixel recovery needs of one render (one map, one exposure gain), computed once
/// instead of per pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ctx {
    /// `1 / gain`: back from the exposed colour to the pre-exposure one the map judges.
    inv_gain: f32,
    /// The airlight, encoded ([`HazeMap::air`]).
    air: [f32; 3],
    /// [`inv_air_lin`] of it.
    inv_air_lin: [f32; 3],
    /// `1 /` the airlight's brightest channel (encoded, at least [`AIR_FLOOR`]).
    inv_top: f32,
    /// The airlight, linear and exposed.
    air_g: [f32; 3],
    /// The colour negative amounts veil toward ([`veil_air`]), exposed.
    veil_g: [f32; 3],
    /// Linear (pre-exposure) bounds of the pixels that can be within [`SKY_K`] of the airlight:
    /// a pixel with any channel outside them gets the full recovery without encoding it.
    sky_lo: [f32; 3],
    sky_hi: [f32; 3],
}

impl Ctx {
    /// The values of `map` at exposure `gain` (`None` unless the gain is positive and finite).
    pub fn new(map: &HazeMap, gain: f32) -> Option<Ctx> {
        if !(gain > 0.0 && gain.is_finite()) {
            return None;
        }
        let top = air_top(map);
        let reach = SKY_K * top;
        Some(Ctx {
            inv_gain: 1.0 / gain,
            air: map.air,
            inv_air_lin: inv_air_lin(map.air),
            inv_top: 1.0 / top,
            air_g: map.air_lin().map(|v| v * gain),
            veil_g: veil_air(map).map(|v| v * gain),
            sky_lo: map.air.map(|a| decode(a - reach)),
            sky_hi: map.air.map(|a| decode(a + reach)),
        })
    }

    /// Haze `d̃` (0..1) of a pixel with linear (pre-exposure) colour `lin` and the map's
    /// coefficients `(a, b)` where it sits ([`HazeMap::coeffs`]).
    #[inline]
    pub fn dark_with(&self, a: f32, b: f32, lin: [f32; 3]) -> f32 {
        let d = a * norm_dark_lin(lin, self.inv_air_lin) + b;
        if d.is_finite() { d.clamp(0.0, 1.0) } else { 0.0 }
    }

    /// How far an encoded pixel `e` is from the airlight: its largest channel difference, relative
    /// to the airlight's brightest channel (so the same photo exposed brighter or darker is judged
    /// alike).
    #[inline]
    fn sky_distance(&self, e: [f32; 3]) -> f32 {
        (0..3).map(|i| (e[i] - self.air[i]).abs()).fold(0.0f32, f32::max) * self.inv_top
    }

    /// [`sky_weight_at`] of a linear (pre-exposure) pixel; encodes it only when it can be near the
    /// airlight.
    #[inline]
    fn sky_weight(&self, lin: [f32; 3]) -> f32 {
        if (0..3).all(|i| lin[i] >= self.sky_lo[i] && lin[i] <= self.sky_hi[i]) { sky_weight_at(self.sky_distance(encode3(lin))) } else { 1.0 }
    }
}

/// What the per-pixel recovery needs of one amount, computed once per amount instead of per pixel
/// (a render without mask Dehaze has one amount).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Amount {
    /// The amount, −1..1.
    pub k: f32,
    /// Haze strength `OMEGA·k^OMEGA_POWER` (positive amounts).
    omega: f32,
    /// Lowest transmission `lerp(1, T_FLOOR, k)` (positive amounts).
    tmin: f32,
    /// Exposure compensation ([`HazeMap::comp`]).
    comp: f32,
}

impl Amount {
    /// The values of amount `k` (global plus masks; clamped to −1..1, 0 when not finite).
    pub fn new(map: &HazeMap, k: f32) -> Amount {
        let k = if k.is_finite() { k.clamp(-1.0, 1.0) } else { 0.0 };
        let kp = k.max(0.0);
        Amount { k, omega: OMEGA * kp.powf(OMEGA_POWER), tmin: 1.0 + (T_FLOOR - 1.0) * kp, comp: map.comp(k) }
    }
}

/// The V4 Dehaze of one pixel: `c` is its white-balanced, exposed colour, `(a, b)` the map's
/// coefficients where it sits in the whole output frame ([`HazeMap::coeffs`], [`HazeRow::coeffs`]:
/// a window dehazes exactly like the whole render), `ctx` the render's values and `am` the amount.
/// Returns the dehazed colour; a non-finite result falls back to `c`.
#[inline]
pub fn dehaze_at(ctx: &Ctx, am: &Amount, a: f32, b: f32, c: [f32; 3]) -> [f32; 3] {
    if am.k == 0.0 {
        return c;
    }
    let lin = c.map(|v| v * ctx.inv_gain);
    let d = ctx.dark_with(a, b, lin);
    let (j, w) = recover_with(ctx, am, c, d, || ctx.sky_weight(lin));
    finish_recovery(am, c, j, w)
}

/// The exposure compensation and chroma blend (positive amounts) of the recovered `j` of the
/// exposed colour `c`, and the fallback to `c` when anything is not finite. `w` is the pixel's sky
/// weight ([`sky_weight_at`]): the compensation makes up for the recovery's darkening, so a pixel
/// the sky tolerance left alone isn't brightened (no light band at the horizon).
fn finish_recovery(am: &Amount, c: [f32; 3], mut j: [f32; 3], w: f32) -> [f32; 3] {
    if am.k > 0.0 {
        let comp = 1.0 + (am.comp - 1.0) * w;
        j = j.map(|v| v * comp);
        // keep J's luminance, blend some of the input's chromaticity back
        let (yj, yc) = (luminance_2020(j), luminance_2020(c));
        if yj > 1e-9 && yc > 1e-9 {
            let s = yj / yc;
            j = std::array::from_fn(|i| j[i] + (c[i] * s - j[i]) * CHROMA_KEEP);
        }
    }
    if j.iter().all(|v| v.is_finite()) { j } else { c }
}

/// The colour negative amounts veil toward (linear, pre-exposure): the airlight, scaled so its
/// brightest channel is at most [`VEIL_TOP`].
pub fn veil_air(map: &HazeMap) -> [f32; 3] {
    let air_lin = map.air_lin();
    let top = air_lin.iter().fold(0.0f32, |m, v| m.max(*v));
    let s = if top > VEIL_TOP { VEIL_TOP / top } else { 1.0 };
    air_lin.map(|v| v * s)
}

/// The airlight's brightest channel (encoded, at least [`AIR_FLOOR`]).
fn air_top(map: &HazeMap) -> f32 {
    map.air.iter().fold(AIR_FLOOR, |m, v| m.max(*v))
}

/// The sky tolerance of a pixel at [`Ctx::sky_distance`] `m`: 0 at the airlight, rising smoothly
/// to 1 at [`SKY_K`] (1 for NaN).
fn sky_weight_at(m: f32) -> f32 {
    if m.is_nan() { 1.0 } else { crate::optics::smoothstep(0.0, SKY_K, m) }
}

/// The recovery of the exposed colour `c` with haze `d` before its exposure compensation and
/// chroma blend (what the compensation LUT measures), with the pixel's sky weight given (computed
/// only when needed). Returns the recovered colour and that weight (1 when the amount isn't
/// positive).
fn recover_with(ctx: &Ctx, am: &Amount, c: [f32; 3], d: f32, sky: impl FnOnce() -> f32) -> ([f32; 3], f32) {
    let d = if d.is_finite() { d.clamp(0.0, 1.0) } else { 0.0 };
    if am.k > 0.0 {
        let air = ctx.air_g;
        // the map measures haze in the encoding; the recovery is linear, so is its haze share
        // (`min(e/A)` of an encoded pixel is `√min(c/A)` of the linear one)
        let t = (1.0 - am.omega * decode(d)).max(am.tmin);
        // sky tolerance: pixels close to the airlight colour are left nearly alone; the gain
        // above 1 fades in smoothly over 0..SKY_K, so the horizon gets no band
        let s = sky();
        let inv_t = 1.0 / (t / (t + (1.0 - t) * s).max(1e-6)).clamp(T_FLOOR, 1.0);
        (std::array::from_fn(|i| (air[i] + (c[i] - air[i]) * inv_t).max(0.0)), s)
    } else if am.k < 0.0 {
        // a veil toward the airlight, mixed in a square-root encoding (lifts the shadows more
        // than the median)
        let t = 1.0 - VEIL * -am.k * (VEIL_BASE + (1.0 - VEIL_BASE) * d);
        let mix = |v: f32, a: f32| {
            let r = v.max(0.0).sqrt() * t + a.max(0.0).sqrt() * (1.0 - t);
            r * r
        };
        (std::array::from_fn(|i| mix(c[i], ctx.veil_g[i])), 1.0)
    } else {
        (c, 1.0)
    }
}

/// Whether `s` renders under process V4, whose Dehaze uses a [`HazeMap`].
pub fn is_v4(s: &DevelopSettings) -> bool {
    match s.process.process() {
        Process::V1 | Process::V2 | Process::V3 => false,
        Process::V4 => true,
    }
}

/// Whether `s` dehazes with a [`HazeMap`]: process V4 with Dehaze (the Effects slider, its section
/// on, or a mask's that the render evaluates: visible, with components) not zero.
pub fn wanted(s: &DevelopSettings) -> bool {
    let masks = s.masks.iter().any(|m| m.visible && !m.components.is_empty() && m.adjust.dehaze != 0.0);
    is_v4(s) && ((s.section_enabled("effects") && s.effects.dehaze != 0.0) || masks)
}

/// Proxy size (px) of the map of `frame` (the whole output frame).
pub fn proxy_size(frame: &Frame) -> (usize, usize) {
    let (nw, nh) = frame.native_size();
    let native = nw.max(nh);
    let edge = if native.is_finite() && native >= 1.0 { (native.round() as usize).min(PROXY_EDGE) } else { PROXY_EDGE };
    frame.fit(edge.max(1), edge.max(1))
}

/// How many maps [`for_frame`] keeps (the least recently used goes first). A 768-px proxy's map
/// is about 3.1 MB.
pub const CACHE_KEEP: usize = 6;

/// One cached map: its full key, and what a draft may reuse it for (the same source content,
/// process and proxy size).
struct Cached {
    key: u64,
    src: u64,
    shape: (Process, usize, usize),
    map: Arc<HazeMap>,
}

/// The maps [`for_frame`] keeps: at most [`CACHE_KEEP`], the least recently used first out.
struct MapCache(Vec<Cached>);

impl MapCache {
    const fn new() -> MapCache {
        MapCache(Vec::new())
    }

    /// The map keyed `key`, or (`reuse`) the most recently used one of that source and shape. A
    /// hit becomes the most recently used.
    fn get(&mut self, key: u64, reuse: Option<(u64, (Process, usize, usize))>) -> Option<Arc<HazeMap>> {
        let c = &mut self.0;
        let i = c
            .iter()
            .rposition(|e| e.key == key)
            .or_else(|| reuse.and_then(|(src, shape)| c.iter().rposition(|e| e.src == src && e.shape == shape)))?;
        let e = c.remove(i);
        let map = e.map.clone();
        c.push(e);
        Some(map)
    }

    fn bytes(&self) -> usize {
        self.0.iter().map(|e| e.map.bytes()).sum()
    }

    fn put(&mut self, e: Cached) {
        let c = &mut self.0;
        c.retain(|o| o.key != e.key);
        c.push(e);
        while c.len() > CACHE_KEEP {
            c.remove(0);
        }
    }
}

static CACHE: Mutex<MapCache> = Mutex::new(MapCache::new());

fn cache() -> std::sync::MutexGuard<'static, MapCache> {
    CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Bytes the maps [`for_frame`] keeps hold.
pub fn cache_bytes() -> usize {
    cache().bytes()
}

/// Drop every map [`for_frame`] keeps (the next render of a dehazed V4 photo builds its map again).
pub fn clear_cache() {
    cache().0.clear();
    CLEARS.with(|n| n.set(n.get().wrapping_add(1)));
}

std::thread_local! {
    static CLEARS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many times this thread called [`clear_cache`]: lets a caller's tests check when it drops
/// the maps although other tests share the cache (and clear it) in parallel.
pub fn clears_on_this_thread() -> u64 {
    CLEARS.with(std::cell::Cell::get)
}

/// The haze map of `src` developed with `s` through `frame` (the whole output frame, before any
/// window), from a cache of the last [`CACHE_KEEP`] maps used (keyed by the source's content
/// fingerprint, the frame, the white balance, the camera local tone mapping, the process and the
/// proxy size).
///
/// A `draft` (an interactive drag) that misses reuses the most recently used map of the same
/// source, process and proxy size instead of building one: a White Balance, crop or lens drag
/// would otherwise rebuild it on every tick. The map is smooth, so the stand-in is close; the
/// final-quality render after the drag builds the exact one.
pub fn for_frame(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings, frame: &Frame, draft: bool) -> Arc<HazeMap> {
    // (an empty source has nothing to sample: no haze)
    if src.width == 0 || src.height == 0 || src.data.is_empty() {
        return Arc::new(HazeMap::empty());
    }
    let (pw, ph) = proxy_size(frame);
    let wb = crate::local::wb_matrix_for(info, s).map(|m| m.map(|r| r.map(f32::to_bits)));
    let fp = crate::optics::fingerprint(src);
    let shape = (s.process.process(), pw, ph);
    let local_tone = crate::local_tone::enabled(info, s);
    let key = crate::hash_of((fp, format!("{frame:?}"), wb, local_tone, shape));
    if let Some(map) = cache().get(key, draft.then_some((fp, shape))) {
        return map;
    }
    let map = Arc::new(crate::timed("haze map", || {
        let mut img = crate::timed("haze proxy", || frame.sample(src, pw, ph));
        // (the pixels the map is applied to went through the camera's local tone mapping)
        crate::local_tone::apply_frame(&mut img, info, s, frame);
        crate::local::white_balance(&mut img, info, s);
        HazeMap::build(&img)
    }));
    cache().put(Cached { key, src: fp, shape, map: map.clone() });
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lin_of_encoded(e: [f32; 3]) -> [f32; 3] {
        e.map(decode)
    }

    /// The V4 Dehaze of one pixel of linear (pre-exposure) colour `lin` with haze `d` at amount
    /// `k`, exposed by `gain`: [`dehaze_at`] with the haze given.
    fn recover(map: &HazeMap, lin: [f32; 3], gain: f32, d: f32, k: f32) -> [f32; 3] {
        let ctx = Ctx::new(map, gain).expect("a positive gain");
        let am = Amount::new(map, k);
        let c = lin.map(|v| v * gain);
        let (j, w) = recover_with(&ctx, &am, c, d, || ctx.sky_weight(lin));
        finish_recovery(&am, c, j, w)
    }

    /// Haze `d̃` of a pixel of linear colour `lin` at frame-normalised `(nx, ny)`.
    fn dark(map: &HazeMap, nx: f32, ny: f32, lin: [f32; 3]) -> f32 {
        let (a, b) = map.coeffs(nx, ny);
        Ctx::new(map, 1.0).expect("gain 1").dark_with(a, b, lin)
    }

    /// A hazy scene: a dark, contrasty foreground (bottom) under a bright bluish-grey veil (top),
    /// with a small white object in the foreground.
    fn hazy(w: usize, h: usize) -> Rgb32f {
        Rgb32f::from_fn(w, h, |x, y| {
            let fy = y as f32 / h as f32;
            if (w / 2..w / 2 + 3).contains(&x) && (h * 3 / 4..h * 3 / 4 + 3).contains(&y) {
                return [1.0, 1.0, 1.0];
            }
            if fy < 0.4 {
                lin_of_encoded([0.70, 0.74, 0.80])
            } else {
                let t = ((x / 6 + y / 6) % 2) as f32;
                lin_of_encoded([0.10 + 0.25 * t, 0.12 + 0.2 * t, 0.08 + 0.1 * t])
            }
        })
    }

    #[test]
    fn airlight_is_the_haze_not_a_small_white_object() {
        let m = HazeMap::build(&hazy(200, 150));
        // the veil [0.70, 0.74, 0.80] pulled halfway to neutral, not the white object's 1.0
        let y = luminance_2020([0.70, 0.74, 0.80]);
        for (i, c) in [0.70, 0.74, 0.80].into_iter().enumerate() {
            let want = c + (y - c) * AIR_NEUTRAL;
            assert!((m.air[i] - want).abs() < 1e-3, "air {:?} want ch{i} {want}", m.air);
        }
        // haze is high in the veil, low in the foreground
        let (top, bottom) = (dark(&m, 0.5, 0.1, lin_of_encoded([0.70, 0.74, 0.80])), dark(&m, 0.3, 0.8, lin_of_encoded([0.10, 0.12, 0.08])));
        assert!(top > 0.8 && bottom < 0.3, "top {top} bottom {bottom}");
    }

    #[test]
    fn a_bright_magenta_sun_is_not_the_airlight() {
        // an HDR merge's sun: clipped, magenta after white balance, 15x scene white (~0.5 % of
        // the frame); as the airlight it turned skies cyan
        let mut img = hazy(200, 150);
        for y in 10..18 {
            for x in 20..40 {
                if let Some(p) = img.data.get_mut(y * 200 + x) {
                    *p = [15.5, 5.0, 14.0];
                }
            }
        }
        let m = HazeMap::build(&img);
        let y = luminance_2020([0.70, 0.74, 0.80]);
        for (i, c) in [0.70, 0.74, 0.80].into_iter().enumerate() {
            let want = c + (y - c) * AIR_NEUTRAL;
            assert!((m.air[i] - want).abs() < 1e-3, "air {:?} want ch{i} {want}", m.air);
        }
    }

    #[test]
    fn a_flat_grey_has_constant_haze_and_is_left_alone() {
        for v in [0.02f32, 0.18, 0.5, 1.0] {
            let img = Rgb32f::filled(64, 48, [v; 3]);
            let m = HazeMap::build(&img);
            assert!(m.air.iter().all(|a| a.is_finite() && *a >= AIR_FLOOR), "{:?}", m.air);
            let d0 = dark(&m, 0.0, 0.0, [v; 3]);
            for (nx, ny) in [(0.5, 0.5), (1.0, 1.0), (0.2, 0.9)] {
                assert!((dark(&m, nx, ny, [v; 3]) - d0).abs() < 1e-4);
            }
            assert!(m.comp.iter().all(|c| (*c - 1.0).abs() < 1e-3), "{:?}", m.comp);
            for k in [-1.0, -0.5, 0.25, 0.5, 1.0] {
                let out = recover(&m, [v; 3], 1.0, d0, k);
                for c in out {
                    assert!((c - v).abs() <= v * 2e-3 + 1e-6, "v {v} k {k} -> {out:?}");
                }
            }
        }
    }

    #[test]
    fn black_white_and_nan_images_never_give_nan() {
        let imgs = [
            Rgb32f::filled(32, 32, [0.0; 3]),
            Rgb32f::filled(32, 32, [1.0; 3]),
            Rgb32f::filled(32, 32, [f32::NAN; 3]),
            Rgb32f::filled(32, 32, [f32::INFINITY, -1.0, f32::NAN]),
            Rgb32f::from_fn(32, 32, |x, _| if x % 3 == 0 { [f32::NAN, 0.5, 0.2] } else { [0.3, 0.2, 0.1] }),
            Rgb32f::new(0, 0),
            Rgb32f::new(5, 0),
            Rgb32f { width: 4, height: 4, data: vec![[0.5; 3]; 3] },
        ];
        for img in &imgs {
            let m = HazeMap::build(img);
            assert!(m.air.iter().all(|a| a.is_finite() && *a >= AIR_FLOOR), "{:?}", m.air);
            assert!(m.air_lin().iter().all(|a| a.is_finite()));
            assert!(m.a.data.iter().chain(&m.b.data).all(|v| v.is_finite()));
            assert!(m.comp.iter().all(|c| c.is_finite() && *c >= 1.0));
            for px in [[0.0; 3], [1.0; 3], [f32::NAN; 3], [f32::INFINITY; 3], [-1.0; 3]] {
                for (nx, ny) in [(0.5, 0.5), (f32::NAN, 2.0), (-3.0, f32::INFINITY)] {
                    let d = dark(&m, nx, ny, px);
                    assert!((0.0..=1.0).contains(&d), "{d}");
                    for k in [-1.0, -0.3, 0.0, 0.4, 1.0, f32::NAN, 7.0] {
                        let out = recover(&m, px, 1.0, d, k);
                        let fallback = px.map(|v| v * 1.0);
                        assert!(
                            out.iter().all(|v| v.is_finite()) || out.iter().zip(fallback).all(|(a, b)| a.to_bits() == b.to_bits()),
                            "{px:?} k {k} -> {out:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn deterministic() {
        let img = hazy(160, 120);
        assert_eq!(HazeMap::build(&img), HazeMap::build(&img));
    }

    #[test]
    fn coefficients_are_frame_normalised() {
        let m = HazeMap::build(&hazy(100, 80));
        // corners land on the corner proxy pixels, and outside is the edge's
        let (w, h) = m.size();
        let at = |p: &Plane, x: usize, y: usize| p.data[y * w + x];
        assert_eq!(m.coeffs(0.0, 0.0), (at(&m.a, 0, 0), at(&m.b, 0, 0)));
        assert_eq!(m.coeffs(1.0, 1.0), (at(&m.a, w - 1, h - 1), at(&m.b, w - 1, h - 1)));
        assert_eq!(m.coeffs(-5.0, 9.0), m.coeffs(0.0, 1.0));
    }

    #[test]
    fn row_sampling_is_bilinear_sampling() {
        // the renderer blends two proxy rows once per output row, then two columns per pixel: the
        // same arithmetic, in the same order, as `coeffs` (and the GPU)
        let m = HazeMap::build(&hazy(100, 80));
        for ny in [-1.0, 0.0, 0.013, 0.37, 0.5, 0.999, 1.0, 3.0, f32::NAN] {
            let row = m.row(ny);
            for nx in [-1.0, 0.0, 0.021, 0.44, 0.5, 0.97, 1.0, 2.0, f32::NAN] {
                let (a, b) = row.coeffs(&m.column(nx));
                let (ca, cb) = m.coeffs(nx, ny);
                assert_eq!((a.to_bits(), b.to_bits()), (ca.to_bits(), cb.to_bits()), "({nx}, {ny})");
            }
        }
        // a malformed map samples zeros, never panics
        let bad = HazeMap { a: Plane { width: 4, height: 4, data: vec![1.0; 3] }, ..HazeMap::empty() };
        assert_eq!(bad.row(0.9).coeffs(&bad.column(0.9)).0, 0.0);
        assert_eq!(bad.coeffs(0.9, 0.9).0, 0.0);
    }

    #[test]
    fn compensation_brightens_more_with_the_amount_and_is_bounded() {
        let m = HazeMap::build(&hazy(200, 150));
        assert_eq!(m.comp(0.0), 1.0);
        assert_eq!(m.comp(-0.5), 1.0);
        let max = 2f32.powf(COMP_MAX_EV).powf(COMP_POWER);
        let mut last = 1.0;
        for i in 0..=40 {
            let c = m.comp(i as f32 / 40.0);
            assert!(c >= last - 1e-6 && c <= max + 1e-6, "{i}: {c} after {last}");
            last = c;
        }
        assert!(m.comp(1.0) > 1.0, "{:?}", m.comp);
        assert_eq!(m.comp(1.0), m.comp[COMP_N - 1]);
        assert_eq!(m.comp(5.0), m.comp(1.0));
    }

    #[test]
    fn dehaze_adds_contrast_and_negative_adds_a_veil() {
        let m = HazeMap::build(&hazy(200, 150));
        let (p1, p2) = (lin_of_encoded([0.55, 0.57, 0.60]), lin_of_encoded([0.45, 0.47, 0.50]));
        let y = |c: [f32; 3]| luminance_2020(c);
        let before = y(p1) - y(p2);
        let after = y(recover(&m, p1, 1.0, 0.5, 0.5)) - y(recover(&m, p2, 1.0, 0.5, 0.5));
        assert!(after > before * 1.1, "contrast {before} -> {after}");
        // without the exposure compensation the hazy tones darken
        let ctx = Ctx::new(&m, 1.0).expect("gain 1");
        assert!(y(recover_with(&ctx, &Amount::new(&m, 0.5), p1, 0.5, || ctx.sky_weight(p1)).0) < y(p1));
        let veiled = recover(&m, [0.01; 3], 1.0, 0.5, -1.0);
        assert!(y(veiled) > 0.02, "a veil lifts black: {veiled:?}");
        let less = y(recover(&m, p1, 1.0, 0.5, -1.0)) - y(recover(&m, p2, 1.0, 0.5, -1.0));
        assert!(less < before * 0.9, "negative lowers contrast: {before} -> {less}");
    }

    #[test]
    fn proxy_is_capped_at_the_proxy_edge() {
        let s = DevelopSettings::default();
        let big = Frame::new(4000, 3000, &s, true);
        assert_eq!(proxy_size(&big), (768, 576));
        let small = Frame::new(300, 200, &s, true);
        assert_eq!(proxy_size(&small), (300, 200));
    }

    #[test]
    fn the_cache_evicts_the_least_recently_used_and_stays_bounded() {
        let shape = (Process::V4, 10, 10);
        let entry = |key: u64, src: u64| Cached { key, src, shape, map: Arc::new(HazeMap::empty()) };
        let mut c = MapCache::new();
        for k in 0..CACHE_KEEP as u64 {
            c.put(entry(k, 100 + k));
        }
        let first = c.get(0, None).expect("cached");
        // more maps than it keeps: the one used on every tick stays, the oldest unused goes
        for k in 0..3u64 {
            c.put(entry(1000 + k, 1000 + k));
            assert!(c.get(0, None).is_some_and(|m| Arc::ptr_eq(&m, &first)), "evicted the map in use");
        }
        assert_eq!(c.0.len(), CACHE_KEEP);
        assert!(c.get(1, None).is_none() && c.get(2, None).is_none(), "the least recently used went");
        // a draft that misses reuses the latest of the same source and shape; a final render doesn't
        assert!(c.get(77, None).is_none());
        assert!(c.get(77, Some((100, shape))).is_some_and(|m| Arc::ptr_eq(&m, &first)));
        assert!(c.get(77, Some((100, (Process::V4, 11, 10)))).is_none(), "another proxy size");
        assert!(c.get(77, Some((5, shape))).is_none(), "another source");
    }

    #[test]
    fn the_cache_counts_its_bytes() {
        let map = Arc::new(HazeMap::build(&hazy(120, 90)));
        assert!(map.bytes() >= 2 * 120 * 90 * 4, "{}", map.bytes());
        let mut c = MapCache::new();
        assert_eq!(c.bytes(), 0);
        c.put(Cached { key: 1, src: 1, shape: (Process::V4, 120, 90), map: map.clone() });
        c.put(Cached { key: 2, src: 2, shape: (Process::V4, 1, 1), map: Arc::new(HazeMap::empty()) });
        assert_eq!(c.bytes(), map.bytes() + HazeMap::empty().bytes());
        // (the shared cache: other tests use it, so only that it answers)
        let _ = cache_bytes();
    }

    #[test]
    fn wanted_ignores_masks_the_render_skips() {
        use lightcraft_develop::{LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
        let comp = MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: lightcraft_geom::Point::new(0.5, 0.0), end: lightcraft_geom::Point::new(0.5, 1.0) },
        };
        let mask = Mask { components: vec![comp], adjust: LocalAdjustments { dehaze: 40.0, ..Default::default() }, ..Default::default() };
        let mut s = DevelopSettings { process: Process::V4.version(), masks: vec![mask.clone()], ..DevelopSettings::default() };
        assert!(wanted(&s));
        s.masks = vec![Mask { visible: false, ..mask.clone() }];
        assert!(!wanted(&s), "a hidden mask");
        s.masks = vec![Mask { components: vec![], ..mask }];
        assert!(!wanted(&s), "a mask without components");
    }

    #[test]
    fn wanted_only_for_v4_with_dehaze() {
        use lightcraft_develop::ProcessVersion;
        let mut s = DevelopSettings { process: Process::V4.version(), ..DevelopSettings::default() };
        assert!(!wanted(&s));
        s.effects.dehaze = 30.0;
        assert!(wanted(&s));
        s.disabled_sections.push("effects".into());
        assert!(!wanted(&s));
        s.process = ProcessVersion(2);
        s.disabled_sections.clear();
        assert!(!wanted(&s));
    }
}
