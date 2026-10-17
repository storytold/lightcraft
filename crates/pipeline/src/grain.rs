//! Film grain: particles rendered on the spot, a stochastic Boolean model filtered by the output
//! pixel.
//!
//! Implemented from the published model of Newson, Delon & Galerne, "Realistic Film Grain
//! Rendering" (IPOL 2017), extended to clumps of irregular grains:
//!
//! - **Particles.** A *clump* is one to eleven grains around a centre; the clump centres form a
//!   Poisson process. Each grain has its own size (log-normal spread: Size Variation), shape (an
//!   ellipse of random orientation and elongation with an irregular, lobed outline: Shape),
//!   optical density and position in the clump (Clumping). Every value comes from hashes of the
//!   seed and the grain's cell, so a seed is a pattern and the same grains are drawn whatever the
//!   render size or region.
//! - **Density follows tone.** The clump density is set so that a point is covered with the
//!   expected coverage `p` of its tone: `λ·E[∫a_clump] = −ln(1 − p)`.
//! - **Exact statistics.** Each grain is softened by the output pixel's footprint, and the union
//!   is evaluated exactly as `1 − Π(1 − aᵢ)`. The Poisson generating functional then gives the
//!   mean (`p` at any scale) and the variance `(1 − p)²·(exp(λ·E[∫a_clump²]) − 1)` in closed form.
//!   The two clump integrals are measured once per setting by rasterising sample clumps
//!   ([`clump_moments`], cached).
//!
//! Resolution independence: grain geometry lives in normalized frame coordinates (a fraction of the
//! uncropped frame's long edge, like a grain on a 35 mm negative) and only the softening — one
//! output pixel — depends on the render size. A small render therefore shows what the export
//! shows downscaled: the same mean, the same (lower) variance. Once clumps are much smaller than a
//! pixel (grid thumbnails, fit-to-window previews of fine grain) the field is replaced by per-pixel
//! Gaussian noise with that exact variance, cross-faded over a band so the switch never shows.
//!
//! Shared with the GPU kernel (`crates/gpu/src/wgsl/finish.wgsl`): keep both in step.

use std::f32::consts::FRAC_1_SQRT_2;
use std::sync::{Mutex, PoisonError};

use lightcraft_develop::Grain;

/// Hash constants (shared with the GPU kernel).
pub const GRAIN_HASH: [u32; 4] = [0x2c9277b5, 0xac564b05, 0x9e37_79b9, 0x85eb_ca6b];

/// Seed salts of the red, green and blue dye layers (colour grain).
pub const CHANNEL_SALT: [u32; 3] = [0x68e3_1da4, 0xb529_7a4d, 0x1b56_c4e9];

/// Coverage range: grains always overlap into irregular clumps and pores (a sparse field would
/// read as isolated dots, a saturated one as pores in black): light tones are covered less.
pub const P_MIN: f32 = 0.2;
pub const P_MAX: f32 = 0.8;

/// Grain amplitude in the deepest shadows and brightest highlights relative to the midtones.
pub const TONE_FLOOR: f32 = 0.35;

/// Clumps a cell may hold (a bound for the loop, far above any reachable mean).
pub const MAX_PER_CELL: u32 = 48;

/// Grains a clump may hold.
pub const MAX_SUB: u32 = 11;

/// Side of a Poisson cell (grain units). Fixed, so that no slider and no render size moves the
/// cell grid: the controls reshape, rescale and regroup the same particles.
pub const CELL: f32 = 2.5;

/// Cells a pixel looks at along each axis beyond its own (the support is at most `CELL·MAX_REACH/2`).
pub const MAX_REACH: i32 = 12;

/// Edge softness of the film itself (grain units), on top of the pixel's footprint.
const FILM_SOFT: f64 = 0.4;

/// Size Variation 100: the log-radius spread (radii from 0.35× to 2.9× the mean).
const SPREAD_MAX: f64 = 0.35;

/// Shape 100: grains up to 4:1 ellipses, outlines varying by up to 30 % with angle.
const ASPECT_MAX: f64 = 3.0;
const LUMP_MAX: f64 = 0.3;

/// Clumping 100: on average five more grains per clump, spread over ±2 radii.
const CLUMP_EXTRA_MAX: f64 = 5.0;
const CLUMP_SPREAD_MIN: f64 = 0.8;
const CLUMP_SPREAD_MAX: f64 = 2.0;

/// Phases (cos, sin of k·π/8) for the grains' outline harmonics (shared with the GPU kernel).
pub const PHASES: [[f32; 2]; 16] = [
    [1.0, 0.0],
    [0.923_879_5, 0.382_683_43],
    [FRAC_1_SQRT_2, FRAC_1_SQRT_2],
    [0.382_683_43, 0.923_879_5],
    [0.0, 1.0],
    [-0.382_683_43, 0.923_879_5],
    [-FRAC_1_SQRT_2, FRAC_1_SQRT_2],
    [-0.923_879_5, 0.382_683_43],
    [-1.0, 0.0],
    [-0.923_879_5, -0.382_683_43],
    [-FRAC_1_SQRT_2, -FRAC_1_SQRT_2],
    [-0.382_683_43, -0.923_879_5],
    [0.0, -1.0],
    [0.382_683_43, -0.923_879_5],
    [FRAC_1_SQRT_2, -FRAC_1_SQRT_2],
    [0.923_879_5, -0.382_683_43],
];

/// The softening one output pixel applies, as the edge width of a grain (pixels), without clumps
/// and with the most: the width at which a small render has the grain variance of an export
/// box-downscaled to its size (measured: `small_render_matches_a_downscaled_full_size_render`).
/// Overlapping grains lose more variance to a box than to the soft footprint, so clumps need a
/// wider one.
pub const PIXEL_SOFTNESS: [f64; 2] = [0.44, 0.5];

/// Grain units are mean grain radii; the reference resolution for the Amount calibration is a
/// 6000 px long edge (a 24 MP export).
const REF_PX_PER_LONG: f64 = 6000.0;

/// Explicit particles while the softening is below this many clump radii, Gaussian noise above
/// the second bound, cross-faded between.
const EXPLICIT_UPTO: f64 = 0.6;
const GAUSSIAN_FROM: f64 = 1.1;

/// One grain of a clump: offset from the clump's centre, radius, density, elongation and
/// orientation, outline harmonics (amplitude × phase), and how far it reaches.
#[derive(Clone, Copy, Debug, Default)]
struct Sub {
    x: f32,
    y: f32,
    r: f32,
    o: f32,
    asp: f32,
    inv_asp: f32,
    ux: f32,
    uy: f32,
    h: [f32; 4],
    reach: f32,
}

/// One render's grain parameters (see the module docs). Lengths are in grain units (mean radius 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GrainParams {
    /// Output scale: brightness change per unit of coverage deviation.
    pub amp: f32,
    /// Normalized long-edge units → grain units (`1 / mean radius`).
    pub inv_r: f32,
    /// Edge softness (grain units): film softness and the output pixel's footprint.
    pub s: f32,
    /// `1 / s`.
    pub inv_s: f32,
    /// Log-radius spread of the grains (Size Variation).
    pub spread: f32,
    /// Extra elongation at most (a grain's aspect ratio is `1 + aspect·u`; Shape).
    pub aspect: f32,
    /// Outline irregularity (Shape).
    pub lump: f32,
    /// Mean number of extra grains per clump (Clumping).
    pub clump_extra: f32,
    /// How far a clump's grains lie from its centre at most, per axis (Clumping).
    pub clump_d: f32,
    /// `E[∫a_clump]`: a clump's expected coverage integral.
    pub area: f32,
    /// Largest distance at which a clump still covers a point.
    pub support: f32,
    /// Largest distance at which a grain still covers a point.
    pub sub_reach: f32,
    /// Side of a Poisson cell ([`CELL`]).
    pub cell: f32,
    /// `E[∫a_clump²] / E[∫a_clump]` for this softness (the variance formula's exponent).
    pub kappa: f32,
    /// Cross-fade weights of the explicit field and of the Gaussian noise (`cos² + sin² = 1`).
    pub w_field: f32,
    pub w_noise: f32,
    /// Colour grain, 0..1: from one grain layer for all channels to independent dye layers.
    pub color: f32,
    /// Tone response exponent: > 1 moves grain into the shadows, < 1 into the highlights.
    pub gamma: f32,
    pub seed: u32,
    /// Output pixels per normalized long edge (for the per-pixel noise).
    pub px_per_long: f32,
}

fn finite(v: f64, lo: f64, hi: f64, default: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { default }
}

/// Mean grain radius as a fraction of the frame's long edge for the Size slider (0..100): fine at
/// the low end, where most photos want it, and individual particles towards the top. On a
/// 6000 px export: 0.7 px at 0, 0.8 at the default 25, 1.2 at 50, 1.9 at 75, 3 px at 100 (4.3 to
/// 18 µm on a 35 mm frame).
pub fn radius_for_size(size: f64) -> f64 {
    let t = finite(size, 0.0, 100.0, 25.0) / 100.0;
    0.000_12 + t.powf(2.2) * 0.000_38
}

/// Standard deviation of the coverage at `p` for exponent `kappa` (fade correction off).
fn std_base(p: f64, kappa: f64) -> f64 {
    let q = 1.0 - p;
    q * (q.powf(-kappa) - 1.0).max(0.0).sqrt()
}

/// Cached [`clump_moments`]: keyed by the shape parameters and the softness.
type MomentKey = [u32; 6];
static MOMENTS: Mutex<Vec<(MomentKey, (f64, f64))>> = Mutex::new(Vec::new());

/// `(E[∫a_clump], E[∫a_clump²])` for `gp`'s shape parameters and softness, measured by rasterising
/// a fixed set of sample clumps drawn by the renderer's own generator (cached: a slider drag
/// that changes only Amount, Color or Response, or a render at the same size, measures nothing).
fn clump_moments(gp: &GrainParams) -> (f64, f64) {
    let key: MomentKey =
        [gp.spread.to_bits(), gp.aspect.to_bits(), gp.lump.to_bits(), gp.clump_extra.to_bits(), gp.clump_d.to_bits(), gp.s.to_bits()];
    if let Some(m) = MOMENTS.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|e| e.0 == key).map(|e| e.1) {
        return m;
    }
    const SAMPLES: u32 = 128;
    const GRID: usize = 28;
    let (mut a1, mut a2) = (0.0f64, 0.0f64);
    let mut subs = [Sub::default(); MAX_SUB as usize];
    for k in 0..SAMPLES {
        let h = pcg(0x51ed_270b ^ k.wrapping_mul(0x2545_f491));
        let n = gp.sub_count(h);
        let mut rb = 0.0f32;
        for (j, g) in subs.iter_mut().enumerate().take(n as usize) {
            let hj = sub_hash(h, j as u32);
            let (ox, oy) = gp.sub_offset(hj);
            *g = gp.sub_shape(hj, ox, oy);
            rb = rb.max((ox * ox + oy * oy).sqrt() + g.reach);
        }
        if !(rb > 0.0 && rb.is_finite()) {
            continue;
        }
        let step = 2.0 * rb / GRID as f32;
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for gy in 0..GRID {
            for gx in 0..GRID {
                let (px, py) = (-rb + (gx as f32 + 0.5) * step, -rb + (gy as f32 + 0.5) * step);
                let mut t = 1.0f32;
                for g in subs.iter().take(n as usize) {
                    let (dx, dy) = (px - g.x, py - g.y);
                    let d2 = dx * dx + dy * dy;
                    if d2 < g.reach * g.reach {
                        t *= 1.0 - gp.sub_cover(g, dx, dy, d2);
                    }
                }
                let a = (1.0 - t) as f64;
                s1 += a;
                s2 += a * a;
            }
        }
        let cell = (step * step) as f64;
        a1 += s1 * cell;
        a2 += s2 * cell;
    }
    let m = (a1 / SAMPLES as f64, a2 / SAMPLES as f64);
    let mut cache = MOMENTS.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= 64 {
        cache.remove(0);
    }
    cache.push((key, m));
    m
}

impl GrainParams {
    /// Parameters for a render with `px_per_long` output pixels per long edge of the uncropped
    /// frame; `None` when there is no grain. Out-of-range or non-finite settings are clamped.
    pub fn new(g: &Grain, px_per_long: f64) -> Option<GrainParams> {
        let amount = finite(g.amount, 0.0, 100.0, 0.0) / 100.0;
        let ppl = finite(px_per_long, 1.0, 1e7, 1.0);
        if amount <= 0.0 {
            return None;
        }
        let size = finite(g.size, 0.0, 100.0, 25.0);
        let variation = finite(g.size_variation, 0.0, 100.0, 40.0) / 100.0;
        let shape = finite(g.roughness, 0.0, 100.0, 50.0) / 100.0;
        let clumping = finite(g.clumping, 0.0, 100.0, 30.0) / 100.0;
        let r_norm = radius_for_size(size);
        // the softness, quantized so that nearby render sizes share their cached moments
        let pixel_soft = PIXEL_SOFTNESS[0] + (PIXEL_SOFTNESS[1] - PIXEL_SOFTNESS[0]) * clumping;
        let soft_at = |ppl: f64| {
            let s = (FILM_SOFT * FILM_SOFT + (pixel_soft / (r_norm * ppl)).powi(2)).sqrt();
            (s * 256.0).round().max(1.0) / 256.0
        };
        let (spread, aspect, lump) = (SPREAD_MAX * variation, ASPECT_MAX * shape, LUMP_MAX * shape);
        let clump_extra = CLUMP_EXTRA_MAX * clumping;
        let clump_d = CLUMP_SPREAD_MIN + (CLUMP_SPREAD_MAX - CLUMP_SPREAD_MIN) * clumping;
        let mut gp = GrainParams {
            amp: 0.0,
            inv_r: (1.0 / r_norm) as f32,
            s: 0.0,
            inv_s: 0.0,
            spread: spread as f32,
            aspect: aspect as f32,
            lump: lump as f32,
            clump_extra: clump_extra as f32,
            clump_d: clump_d as f32,
            area: 1.0,
            support: 0.0,
            sub_reach: 0.0,
            cell: CELL,
            kappa: 1.0,
            w_field: 1.0,
            w_noise: 0.0,
            color: (finite(g.color, 0.0, 100.0, 0.0) / 100.0) as f32,
            gamma: 2f64.powf(-finite(g.response, -100.0, 100.0, 0.0) / 50.0) as f32,
            seed: g.seed,
            px_per_long: ppl as f32,
        };
        let set_soft = |gp: &mut GrainParams, s: f64| {
            gp.s = s as f32;
            gp.inv_s = (1.0 / s) as f32;
            let r_max = (3.0 * spread).exp() * (1.0 + aspect).sqrt() * (1.0 + lump);
            gp.sub_reach = ((r_max * r_max + s * s).sqrt() + s) as f32;
            gp.support = (clump_d * std::f64::consts::SQRT_2 + gp.sub_reach as f64).min(CELL as f64 * MAX_REACH as f64 * 0.5 - 0.01) as f32;
        };
        // Amount 100 ≈ 0.06 (8-bit: 15 levels) standard deviation in mid-grey at the reference
        // resolution, a little more for coarse grain
        set_soft(&mut gp, soft_at(REF_PX_PER_LONG));
        let (ref1, ref2) = clump_moments(&gp);
        let kappa_ref = if ref1 > 0.0 { (ref2 / ref1).clamp(1e-6, 1.0) } else { 1.0 };
        let s = soft_at(ppl);
        set_soft(&mut gp, s);
        let (m1, m2) = clump_moments(&gp);
        gp.area = m1.max(1e-3) as f32;
        gp.kappa = if m1 > 0.0 { (m2 / m1).clamp(1e-6, 1.0) as f32 } else { 1.0 };
        let target = amount * 0.055 * (0.8 + 0.4 * size / 100.0);
        gp.amp = (target / std_base(0.5, kappa_ref).max(1e-4)) as f32;
        // a clump's scale: one grain, or a cluster
        let scale = 1.0 + 0.7 * clump_d * clump_extra.min(1.0);
        let t = ((s / scale - EXPLICIT_UPTO) / (GAUSSIAN_FROM - EXPLICIT_UPTO)).clamp(0.0, 1.0);
        (gp.w_field, gp.w_noise) = if t <= 0.0 {
            (1.0, 0.0)
        } else if t >= 1.0 {
            (0.0, 1.0)
        } else {
            let a = t * std::f64::consts::FRAC_PI_2;
            (a.cos() as f32, a.sin() as f32)
        };
        Some(gp)
    }

    /// Expected coverage for display-encoded luminance `lum` (grain is density: dark tones are
    /// covered more).
    #[inline]
    pub fn coverage(&self, lum: f32) -> f32 {
        let l = if lum.is_finite() { lum.clamp(0.0, 1.0) } else { 0.5 };
        P_MIN + (P_MAX - P_MIN) * self.shade(l)
    }

    /// `(1 − l)^γ` for luminance `l` in 0..1 (no `powf` at the default response).
    #[inline]
    fn shade(&self, l: f32) -> f32 {
        let d = (1.0 - l).max(1e-7);
        if self.gamma == 1.0 { d } else { d.powf(self.gamma) }
    }

    /// Amplitude weight for display-encoded luminance `lum`: 1 in the midtones (moved by the
    /// response), [`TONE_FLOOR`] at black and white.
    #[inline]
    pub fn tone_weight(&self, lum: f32) -> f32 {
        let l = if lum.is_finite() { lum.clamp(0.0, 1.0) } else { 0.5 };
        let m = 1.0 - self.shade(l);
        TONE_FLOOR + (1.0 - TONE_FLOOR) * 4.0 * m * (1.0 - m)
    }

    /// Expected clumps per Poisson cell at coverage `p`.
    #[inline]
    fn per_cell(&self, p: f32) -> f32 {
        -(1.0 - p).ln() / self.area * self.cell * self.cell
    }

    /// Standard deviation of the coverage the explicit field has at `p` (the Gaussian noise's
    /// amplitude): `(1 − p)·√(exp(κ·μ') − 1)`, where `μ'` counts fading-in clumps by `f²`.
    #[inline]
    pub fn coverage_std(&self, p: f32) -> f32 {
        let t = self.per_cell(p);
        let fw = t.min(1.0);
        let mu = -(1.0 - p).ln() * (1.0 - fw / (6.0 * t.max(1e-12)));
        (1.0 - p) * ((self.kappa * mu).exp() - 1.0).max(0.0).sqrt()
    }

    /// Number of grains in the clump with hash `h`.
    #[inline]
    fn sub_count(&self, h: u32) -> u32 {
        if self.clump_extra <= 0.0 {
            return 1;
        }
        let extra = (unit(pcg(h ^ GRAIN_HASH[3])) * (2.0 * self.clump_extra + 1.0)).floor();
        1 + (extra.max(0.0) as u32).min(MAX_SUB - 1)
    }

    /// Offset of the grain with hash `hj` from its clump's centre.
    #[inline]
    fn sub_offset(&self, hj: u32) -> (f32, f32) {
        let o = pcg(hj ^ GRAIN_HASH[0]);
        let d = self.clump_d;
        (d * (unit8(o) + unit8(o >> 8) - 1.0), d * (unit8(o >> 16) + unit8(o >> 24) - 1.0))
    }

    /// The grain with hash `hj` at offset (ox, oy): radius, density, elongation, orientation and
    /// outline.
    #[inline]
    fn sub_shape(&self, hj: u32, ox: f32, oy: f32) -> Sub {
        let rad = pcg(hj ^ GRAIN_HASH[3]);
        let z = 2.0 * (unit8(rad) + unit8(rad >> 8) + unit8(rad >> 16)) - 3.0;
        let r = (self.spread * z).exp();
        let o = 0.25 + 0.75 * unit8(rad >> 24);
        let shp = pcg(hj ^ GRAIN_HASH[1]);
        let (a, b) = (2.0 * unit8(shp) - 1.0, 2.0 * unit8(shp >> 8) - 1.0);
        let n = (a * a + b * b).sqrt();
        let (ux, uy) = if n > 1e-3 { (a / n, b / n) } else { (1.0, 0.0) };
        let asp = 1.0 + self.aspect * unit4(shp >> 16);
        let m = 0.5 + ((shp >> 28) & 15) as f32 * (1.0 / 30.0);
        let p2 = PHASES.get(((shp >> 20) & 15) as usize).copied().unwrap_or([1.0, 0.0]);
        let p3 = PHASES.get(((shp >> 24) & 15) as usize).copied().unwrap_or([1.0, 0.0]);
        let rr = r * asp.sqrt() * (1.0 + self.lump);
        Sub {
            x: ox,
            y: oy,
            r,
            o,
            asp,
            inv_asp: 1.0 / asp,
            ux,
            uy,
            h: [0.6 * m * p2[0], 0.6 * m * p2[1], 0.4 * m * p3[0], 0.4 * m * p3[1]],
            reach: (rr * rr + self.s * self.s).sqrt() + self.s,
        }
    }

    /// Coverage (density × softened profile) of grain `g` at offset (dx, dy) from its centre,
    /// `d2 = dx² + dy²`.
    #[inline]
    fn sub_cover(&self, g: &Sub, dx: f32, dy: f32, d2: f32) -> f32 {
        let s = self.s;
        let d = d2.sqrt();
        let mut r = g.r;
        if d > 1e-6 {
            let inv_d = 1.0 / d;
            let (c, sn) = (dx * inv_d, dy * inv_d);
            if g.asp > 1.0 {
                // the ellipse's radius in this direction (area πr² whatever the elongation)
                let (cu, cw) = (c * g.ux + sn * g.uy, sn * g.ux - c * g.uy);
                r /= (cu * cu * g.inv_asp + cw * cw * g.asp).sqrt();
            }
            if self.lump > 0.0 {
                // cos/sin of 2θ and 3θ from the direction (no trigonometry)
                let (c2, s2) = (c * c - sn * sn, 2.0 * c * sn);
                let (c3, s3) = (c * (4.0 * c * c - 3.0), sn * (3.0 - 4.0 * sn * sn));
                r *= 1.0 + self.lump * (g.h[0] * c2 + g.h[1] * s2 + g.h[2] * c3 + g.h[3] * s3);
            }
        }
        let rho2 = r * r + s * s;
        let tau = (d - rho2.sqrt()) * self.inv_s;
        if tau >= 1.0 {
            return 0.0;
        }
        let prof = if tau <= -1.0 { 1.0 } else { (1.0 - tau) * (1.0 - tau) * (2.0 + tau) * 0.25 };
        g.o * r * r / (rho2 + s * s * 0.2) * prof
    }

    /// Coverage of the clump with hash `h` at offset (dx, dy) from its centre.
    fn clump_cover(&self, h: u32, dx: f32, dy: f32) -> f32 {
        let mut t = 1.0f32;
        let rr = self.sub_reach * self.sub_reach;
        for j in 0..self.sub_count(h) {
            let hj = sub_hash(h, j);
            let (ox, oy) = self.sub_offset(hj);
            let (ex, ey) = (dx - ox, dy - oy);
            let d2 = ex * ex + ey * ey;
            if d2 >= rr {
                continue;
            }
            let g = self.sub_shape(hj, ox, oy);
            if d2 >= g.reach * g.reach {
                continue;
            }
            t *= 1.0 - self.sub_cover(&g, ex, ey, d2);
        }
        1.0 - t
    }

    /// Grain coverage at grain-unit position (x, y) for expected coverage `p` and layer seed.
    pub fn field(&self, x: f32, y: f32, p: f32, seed: u32) -> f32 {
        let t = self.per_cell(p);
        // clumps fade in over the last `fw` of the cell's mark range, so the field changes
        // continuously with tone (no particle pops in or out); the shift keeps the mean count at t
        let fw = t.clamp(1e-6, 1.0);
        let end = t + 0.5 * fw;
        let (r, c) = (self.support, self.cell);
        // the cells within reach, as a start and a bounded count
        let (fi0, fj0) = (((x - r) / c).floor(), ((y - r) / c).floor());
        let (ni, nj) = (cells_to((x + r) / c, fi0), cells_to((y + r) / c, fj0));
        let (i0, j0) = (fi0.clamp(-2.0e9, 2.0e9) as i32, fj0.clamp(-2.0e9, 2.0e9) as i32);
        // a cell holds clumps while the running product of their uniforms stays above
        // exp(−end) (Poisson arrivals in mark order, `mₖ = −ln Π uᵢ`); full weight above exp(fw − end)
        let (e_end, e_full) = ((-end).exp(), (fw - end).exp());
        let salt = pcg(seed);
        let mut keep = 1.0f32;
        for dj in 0..=nj {
            let j = j0.saturating_add(dj);
            let (cy0, cy1) = (j as f32 * c, (j as f32 + 1.0) * c);
            let dy = y - y.clamp(cy0, cy1);
            for di in 0..=ni {
                let i = i0.saturating_add(di);
                let (cx0, cx1) = (i as f32 * c, (i as f32 + 1.0) * c);
                let dx = x - x.clamp(cx0, cx1);
                if dx * dx + dy * dy >= r * r {
                    continue;
                }
                let base = cell_hash(i, j, salt);
                let mut prod = 1.0f32;
                for k in 0..MAX_PER_CELL {
                    let h = pcg(base.wrapping_add(k));
                    prod *= unit(h);
                    if prod <= e_end {
                        break;
                    }
                    let pos = pcg(h ^ GRAIN_HASH[2]);
                    let (cx, cy) = ((i as f32 + unit16(pos)) * c, (j as f32 + unit16(pos >> 16)) * c);
                    let (ex, ey) = (x - cx, y - cy);
                    if ex * ex + ey * ey >= r * r {
                        continue;
                    }
                    let a = self.clump_cover(h, ex, ey);
                    if a <= 0.0 {
                        continue;
                    }
                    let f = if prod >= e_full { 1.0 } else { ((end + prod.ln()) / fw).clamp(0.0, 1.0) };
                    keep *= 1.0 - f * a;
                }
            }
        }
        1.0 - keep
    }

    /// [`GrainParams::field`] for a band of output rows: `xs`, `ys` are the grain-unit positions of
    /// `rows × cols` pixels, row-major, on an affine grid (as an output window is), `ps` their
    /// expected coverages; the coverage goes to `out`. Visits every cell near the band once and
    /// splats each clump's grains onto the pixels they reach, testing each grain against each
    /// pixel exactly as `field` does (same particles, same weights; only the product order differs).
    pub fn field_band(&self, xs: &[f32], ys: &[f32], cols: usize, ps: &[f32], seed: u32, out: &mut [f32]) {
        let len = xs.len().min(ys.len()).min(ps.len()).min(out.len());
        let rows = len.checked_div(cols).unwrap_or(0);
        let n = rows * cols;
        let (r, c, s) = (self.support, self.cell, self.s);
        let corner = |row: usize, k: usize| (xs.get(row * cols + k).copied(), ys.get(row * cols + k).copied());
        let ((Some(x00), Some(y00)), (Some(x01), Some(y01)), (Some(x10), Some(y10))) =
            (corner(0, 0), corner(0, cols.wrapping_sub(1)), corner(rows.wrapping_sub(1), 0))
        else {
            return;
        };
        let finite = [x00, y00, x01, y01, x10, y10, r, c, s].iter().all(|v| v.is_finite()) && c > 0.0 && s > 0.0;
        let span = ((x01 - x00).abs().max((y01 - y00).abs()).max((x10 - x00).abs()).max((y10 - y00).abs()) / c.max(1e-6)) as usize;
        if !finite || span > 16 * (cols + rows) + 64 {
            // not a well-formed window (never for a render): pixel by pixel
            for (k, o) in out.iter_mut().enumerate().take(n) {
                *o = self.field(xs[k], ys[k], ps[k], seed);
            }
            return;
        }
        // along a row (bx, by) per pixel; from row to row (dx, dy)
        let (bx, by) = ((x01 - x00) / (cols - 1).max(1) as f32, (y01 - y00) / (cols - 1).max(1) as f32);
        let (dx, dy) = ((x10 - x00) / (rows - 1).max(1) as f32, (y10 - y00) / (rows - 1).max(1) as f32);
        let bb = bx * bx + by * by;
        let inv_b = if bb > 1e-24 { 1.0 / bb.sqrt() } else { 0.0 };
        // per-pixel thresholds as in `field`
        let mut ends = Vec::with_capacity(n);
        let mut e_min = 1.0f32;
        for &p in &ps[..n] {
            let t = self.per_cell(p);
            let fw = t.clamp(1e-6, 1.0);
            let end = t + 0.5 * fw;
            let e_end = (-end).exp();
            e_min = e_min.min(e_end);
            ends.push((end, fw, e_end, (fw - end).exp()));
        }
        let keep = &mut out[..n];
        keep.fill(1.0);
        // per pixel, the clump being splatted: Π(1 − aⱼ) over its grains, and the pixels it touched
        let mut clump_t = vec![1.0f32; n];
        let mut touched: Vec<usize> = Vec::new();
        let salt = pcg(seed);
        let last = (cols - 1) as f32;
        // the pixels whose centres lie in a parameter interval along a row (a hair's margin for
        // rounding: each pixel is tested exactly below)
        let chord = |a: f32, b: f32| -> Option<(usize, usize)> {
            let (lo, hi) = ((a - 0.05).ceil(), (b + 0.05).floor());
            if hi.is_nan() || lo.is_nan() || hi < lo || hi < 0.0 || lo > last {
                return None;
            }
            Some((lo.max(0.0) as usize, (hi.min(last) as usize).min(cols - 1)))
        };
        // distance of a point from row `row`'s line, and the pixels of that row within `reach` of it
        let row_reach = |row: usize, px: f32, py: f32, reach: f32| -> Option<(usize, usize)> {
            let (ddx, ddy) = (px - (x00 + row as f32 * dx), py - (y00 + row as f32 * dy));
            // the cross product: no cancellation far along the row
            let perp = if inv_b > 0.0 { (ddx * by - ddy * bx) * inv_b } else { (ddx * ddx + ddy * ddy).sqrt() };
            if perp.abs() >= reach {
                return None;
            }
            if inv_b > 0.0 {
                let kc = (ddx * bx + ddy * by) / bb;
                let half = (reach * reach - perp * perp).max(0.0).sqrt() * inv_b;
                chord(kc - half, kc + half)
            } else {
                Some((0, cols - 1))
            }
        };
        // the band is the parallelogram of its pixel centres; cells within the support of it
        let quad = [(x00, y00), (x01, y01), (x10 + (x01 - x00), y10 + (y01 - y00)), (x10, y10)];
        let (qx0, qx1) = quad.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), q| (a.min(q.0), b.max(q.0)));
        let (i0, i1) = (((qx0 - r) / c).floor() as i32, ((qx1 + r) / c).floor() as i32);
        let mut subs: [Option<Sub>; MAX_SUB as usize];
        let mut offs = [(0.0f32, 0.0f32); MAX_SUB as usize];
        for i in i0..=i1 {
            // the parallelogram's Y extent within this column of cells (grown by the support):
            // the extremes lie on its edges clipped to the column
            let (cx0, cx1) = (i as f32 * c - r, (i as f32 + 1.0) * c + r);
            let (mut y_lo, mut y_hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for e in 0..4 {
                let ((ax, ay), (bx2, by2)) = (quad[e], quad[(e + 1) % 4]);
                let ex = bx2 - ax;
                let (t0, t1) = if ex.abs() > 1e-12 {
                    let (u, v) = ((cx0 - ax) / ex, (cx1 - ax) / ex);
                    (u.min(v).max(0.0), u.max(v).min(1.0))
                } else if ax >= cx0 && ax <= cx1 {
                    (0.0, 1.0)
                } else {
                    continue;
                };
                if t0 > t1 {
                    continue;
                }
                for t in [t0, t1] {
                    let yt = ay + t * (by2 - ay);
                    y_lo = y_lo.min(yt);
                    y_hi = y_hi.max(yt);
                }
            }
            if y_lo.is_nan() || y_hi.is_nan() || y_lo > y_hi {
                continue;
            }
            let (j0, j1) = (((y_lo - r) / c).floor() as i32, ((y_hi + r) / c).floor() as i32);
            for j in j0..=j1 {
                let base = cell_hash(i, j, salt);
                let mut prod = 1.0f32;
                for k in 0..MAX_PER_CELL {
                    let h = pcg(base.wrapping_add(k));
                    prod *= unit(h);
                    if prod <= e_min {
                        break;
                    }
                    let pos = pcg(h ^ GRAIN_HASH[2]);
                    let (cx, cy) = ((i as f32 + unit16(pos)) * c, (j as f32 + unit16(pos >> 16)) * c);
                    let count = self.sub_count(h) as usize;
                    subs = [None; MAX_SUB as usize];
                    let mut offsets_known = false;
                    touched.clear();
                    for row in 0..rows {
                        if row_reach(row, cx, cy, r).is_none() {
                            continue;
                        }
                        if !offsets_known {
                            for (jj, o) in offs.iter_mut().enumerate().take(count) {
                                *o = self.sub_offset(sub_hash(h, jj as u32));
                            }
                            offsets_known = true;
                        }
                        for jj in 0..count {
                            let Some(&(ox, oy)) = offs.get(jj) else { continue };
                            let (sx, sy) = (cx + ox, cy + oy);
                            if row_reach(row, sx, sy, self.sub_reach).is_none() {
                                continue;
                            }
                            let Some(slot) = subs.get_mut(jj) else { continue };
                            let g = *slot.get_or_insert_with(|| self.sub_shape(sub_hash(h, jj as u32), ox, oy));
                            let Some((pa, pb)) = row_reach(row, sx, sy, g.reach) else { continue };
                            for q in row * cols + pa..=row * cols + pb {
                                let (Some(&(_, _, e_end, _)), Some(&x), Some(&y)) = (ends.get(q), xs.get(q), ys.get(q)) else { continue };
                                if prod <= e_end {
                                    continue;
                                }
                                let (ex, ey) = (x - sx, y - sy);
                                let d2 = ex * ex + ey * ey;
                                if d2 >= g.reach * g.reach {
                                    continue;
                                }
                                let a = self.sub_cover(&g, ex, ey, d2);
                                if a <= 0.0 {
                                    continue;
                                }
                                if let Some(t) = clump_t.get_mut(q) {
                                    if *t == 1.0 {
                                        touched.push(q);
                                    }
                                    *t *= 1.0 - a;
                                }
                            }
                        }
                    }
                    let mut ln_prod = None;
                    for &q in &touched {
                        let (Some(&(end, fw, _, e_full)), Some(t)) = (ends.get(q), clump_t.get_mut(q)) else { continue };
                        let a = 1.0 - *t;
                        *t = 1.0;
                        let f = if prod >= e_full { 1.0 } else { ((end + *ln_prod.get_or_insert_with(|| prod.ln())) / fw).clamp(0.0, 1.0) };
                        if let Some(kq) = keep.get_mut(q) {
                            *kq *= 1.0 - f * a;
                        }
                    }
                }
            }
        }
        for v in keep.iter_mut() {
            *v = 1.0 - *v;
        }
    }

    /// The grain layers (seeds) and how much each adds to every channel: one layer for all
    /// channels, or with colour grain that layer plus a dye layer per channel
    /// (`√(1 − c)·mono + √c·dye`, so the strength stays the same).
    fn layers(&self) -> Vec<(u32, [f32; 3])> {
        if self.color <= 0.0 {
            return vec![(self.seed, [1.0; 3])];
        }
        let (wm, wc) = ((1.0 - self.color).max(0.0).sqrt(), self.color.sqrt());
        let mut v = vec![(self.seed, [wm; 3])];
        for (ch, salt) in CHANNEL_SALT.iter().enumerate() {
            let mut w = [0.0; 3];
            if let Some(slot) = w.get_mut(ch) {
                *slot = wc;
            }
            v.push((self.seed ^ salt, w));
        }
        v
    }

    /// Brightness deviation of one grain layer at normalized position (gx, gy).
    #[inline]
    fn layer(&self, gx: f32, gy: f32, p: f32, seed: u32) -> f32 {
        let mut d = 0.0;
        if self.w_field > 0.0 {
            d += self.w_field * (p - self.field(gx * self.inv_r, gy * self.inv_r, p, seed));
        }
        if self.w_noise > 0.0 {
            let (ix, iy) = ((gx * self.px_per_long).floor() as i32, (gy * self.px_per_long).floor() as i32);
            d -= self.w_noise * self.coverage_std(p) * gauss(ix, iy, seed);
        }
        d
    }

    /// What grain adds to each display-encoded channel at normalized position (gx, gy) (fractions
    /// of the uncropped frame's long edge) for a pixel of display-encoded luminance `lum`.
    pub fn apply(&self, gx: f32, gy: f32, lum: f32) -> [f32; 3] {
        if !(gx.is_finite() && gy.is_finite()) {
            return [0.0; 3];
        }
        let p = self.coverage(lum);
        let amp = self.amp * self.tone_weight(lum);
        let mut out = [0.0f32; 3];
        for (seed, w) in self.layers() {
            let d = amp * self.layer(gx, gy, p, seed);
            for (o, w) in out.iter_mut().zip(w) {
                *o += w * d;
            }
        }
        out
    }

    /// [`GrainParams::apply`] for a band of output rows: adds grain to the display-encoded colours
    /// `e` of `rows × cols` pixels (row-major) at normalized positions `gx`, `gy` (an affine grid).
    pub fn apply_band(&self, gx: &[f32], gy: &[f32], cols: usize, e: &mut [[f32; 3]]) {
        let n = gx.len().min(gy.len()).min(e.len());
        let lums: Vec<f32> = e[..n].iter().map(|c| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).collect();
        let ps: Vec<f32> = lums.iter().map(|l| self.coverage(*l)).collect();
        let amps: Vec<f32> = lums.iter().map(|l| self.amp * self.tone_weight(*l)).collect();
        let (xs, ys): (Vec<f32>, Vec<f32>) = gx[..n].iter().zip(&gy[..n]).map(|(x, y)| (x * self.inv_r, y * self.inv_r)).unzip();
        let mut v = vec![0.0f32; n];
        for (seed, w) in self.layers() {
            if self.w_field > 0.0 {
                self.field_band(&xs, &ys, cols, &ps, seed, &mut v);
            }
            for k in 0..n {
                let (x, y, p) = (gx[k], gy[k], ps[k]);
                if !(x.is_finite() && y.is_finite()) {
                    continue;
                }
                let mut d = 0.0;
                if self.w_field > 0.0 {
                    d += self.w_field * (p - v[k]);
                }
                if self.w_noise > 0.0 {
                    let (ix, iy) = ((x * self.px_per_long).floor() as i32, (y * self.px_per_long).floor() as i32);
                    d -= self.w_noise * self.coverage_std(p) * gauss(ix, iy, seed);
                }
                let d = amps[k] * d;
                let c = &mut e[k];
                *c = [c[0] + w[0] * d, c[1] + w[1] * d, c[2] + w[2] * d];
            }
        }
    }
}

/// How many cells after `start` (floor of the low edge) reach up to `hi` (in cells): at most
/// [`MAX_REACH`], never negative or non-finite.
#[inline]
fn cells_to(hi: f32, start: f32) -> i32 {
    let n = hi.floor() - start;
    if n.is_finite() { n.clamp(0.0, MAX_REACH as f32) as i32 } else { 0 }
}

/// PCG output permutation (RXS-M-XS) of an LCG step.
#[inline]
pub fn pcg(v: u32) -> u32 {
    let s = v.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277_803_737);
    (w >> 22) ^ w
}

/// Hash of cell (i, j) for a layer `salt` (`pcg(seed)`).
#[inline]
fn cell_hash(i: i32, j: i32, salt: u32) -> u32 {
    pcg((i as u32).wrapping_mul(GRAIN_HASH[0]) ^ (j as u32).wrapping_mul(GRAIN_HASH[1]) ^ salt)
}

/// Hash of grain `j` of the clump with hash `h`.
#[inline]
fn sub_hash(h: u32, j: u32) -> u32 {
    pcg(h ^ (j + 1).wrapping_mul(0x9e37_79b9))
}

/// A uniform in (0, 1) from the top 24 bits.
#[inline]
fn unit(v: u32) -> f32 {
    ((v >> 8) as f32 + 0.5) * (1.0 / 16_777_216.0)
}

/// A uniform in (0, 1) from the low 16 bits.
#[inline]
fn unit16(v: u32) -> f32 {
    ((v & 0xffff) as f32 + 0.5) * (1.0 / 65_536.0)
}

/// A uniform in (0, 1) from the low 8 bits.
#[inline]
fn unit8(v: u32) -> f32 {
    ((v & 0xff) as f32 + 0.5) * (1.0 / 256.0)
}

/// A uniform in (0, 1) from the low 4 bits.
#[inline]
fn unit4(v: u32) -> f32 {
    ((v & 15) as f32 + 0.5) * (1.0 / 16.0)
}

/// Unit-variance, near-Gaussian noise for pixel (ix, iy): the sum of four uniforms.
#[inline]
fn gauss(ix: i32, iy: i32, seed: u32) -> f32 {
    // four 8-bit uniforms from one hash (their sum is smooth enough at a pixel's scale)
    let h = cell_hash(ix, iy, pcg(seed ^ 0x5bd1_e995));
    let sum = (h & 0xff) + ((h >> 8) & 0xff) + ((h >> 16) & 0xff) + (h >> 24);
    ((sum as f32 + 2.0) * (1.0 / 256.0) - 2.0) * 1.732_050_8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grain(amount: f64, size: f64, roughness: f64) -> Grain {
        Grain { amount, size, roughness, ..Default::default() }
    }

    /// Brightness deviations of the first channel over a `n × n` pixel patch for a render with
    /// `ppl` px per long edge, at constant luminance `lum`.
    fn patch(gp: &GrainParams, n: usize, lum: f32) -> Vec<f32> {
        let ppl = gp.px_per_long;
        (0..n * n).map(|i| gp.apply(((i % n) as f32 + 0.5) / ppl + 0.1003, ((i / n) as f32 + 0.5) / ppl + 0.1007, lum)[0]).collect()
    }

    fn stats(v: &[f32]) -> (f64, f64) {
        let n = v.len() as f64;
        let mean = v.iter().map(|x| *x as f64).sum::<f64>() / n;
        let var = v.iter().map(|x| (*x as f64 - mean).powi(2)).sum::<f64>() / n;
        (mean, var)
    }

    fn corr(a: &[f32], b: &[f32]) -> f64 {
        let ((ma, va), (mb, vb)) = (stats(a), stats(b));
        let c: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64 - ma) * (*y as f64 - mb)).sum::<f64>() / a.len() as f64;
        c / (va * vb).sqrt().max(1e-30)
    }

    /// Correlation of horizontally adjacent pixels.
    fn lag1(v: &[f32], n: usize) -> f64 {
        let (m, var) = stats(v);
        let mut c = 0.0;
        for y in 0..n {
            for x in 0..n - 1 {
                c += (v[y * n + x] as f64 - m) * (v[y * n + x + 1] as f64 - m);
            }
        }
        c / (n * (n - 1)) as f64 / var.max(1e-30)
    }

    fn box_down(v: &[f32], n: usize, k: usize) -> Vec<f32> {
        let m = n / k;
        (0..m * m)
            .map(|i| {
                let (x, y) = (i % m, i / m);
                let mut s = 0.0;
                for dy in 0..k {
                    for dx in 0..k {
                        s += v[(y * k + dy) * n + x * k + dx];
                    }
                }
                s / (k * k) as f32
            })
            .collect()
    }

    /// The coverage field on a `n × n` grid with spacing `step` (grain units).
    fn field(gp: &GrainParams, n: usize, step: f32, p: f32, seed: u32) -> Vec<f32> {
        (0..n * n).map(|i| gp.field((i % n) as f32 * step + 3.1, (i / n) as f32 * step + 2.7, p, seed)).collect()
    }

    #[test]
    fn field_mean_is_the_expected_coverage_and_variance_matches_theory() {
        for (size, variation, shape, clumping) in
            [(25.0, 40.0, 50.0, 30.0), (80.0, 0.0, 0.0, 0.0), (60.0, 100.0, 100.0, 100.0), (25.0, 20.0, 80.0, 70.0)]
        {
            let g = Grain { size_variation: variation, clumping, ..grain(50.0, size, shape) };
            let gp = GrainParams::new(&g, 6000.0).unwrap();
            assert_eq!((gp.w_field, gp.w_noise), (1.0, 0.0), "explicit at full size");
            for p in [0.2f32, 0.5, 0.8] {
                let v = field(&gp, 240, 0.9, p, 7);
                let (mean, var) = stats(&v);
                let sd = gp.coverage_std(p) as f64;
                assert!((mean - p as f64).abs() < 0.03 + 0.15 * sd, "{g:?} p {p}: mean {mean}");
                assert!((var.sqrt() / sd - 1.0).abs() < 0.2, "{g:?} p {p}: sd {} vs theory {sd}", var.sqrt());
            }
        }
    }

    /// A small render shows the grain a full-size export would show downscaled to that size: the
    /// same mean, a variance and a pixel-to-pixel correlation within tolerance, for both the
    /// explicit field and the Gaussian stand-in (fine grain on a small render).
    #[test]
    fn small_render_matches_a_downscaled_full_size_render() {
        for (size, clumping, k) in [(100.0, 30.0, 4usize), (60.0, 0.0, 3), (25.0, 30.0, 4), (0.0, 80.0, 3), (25.0, 30.0, 8)] {
            let g = Grain { clumping, ..grain(60.0, size, 50.0) };
            let full = GrainParams::new(&g, 3000.0).unwrap();
            let small = GrainParams::new(&g, 3000.0 / k as f64).unwrap();
            assert_eq!(full.amp, small.amp, "Amount is calibrated independently of the render size");
            let n = 120;
            for lum in [0.25f32, 0.5, 0.8] {
                let big = patch(&full, n * k, lum);
                let down = box_down(&big, n * k, k);
                let sm = patch(&small, n, lum);
                let ((m0, v0), (m1, v1)) = (stats(&down), stats(&sm));
                let sd = v0.sqrt().max(v1.sqrt());
                assert!((m0 - m1).abs() < 0.3 * sd + 3e-4, "size {size} ÷{k} lum {lum}: mean {m0} vs {m1}");
                let ratio = (v1 / v0.max(1e-30)).sqrt();
                assert!((0.72..1.38).contains(&ratio), "size {size} ÷{k} lum {lum}: sd ratio {ratio} ({} vs {})", v1.sqrt(), v0.sqrt());
                let (c0, c1) = (lag1(&down, n), lag1(&sm, n));
                assert!((c0 - c1).abs() < 0.25, "size {size} ÷{k} lum {lum}: lag-1 correlation {c0} vs {c1}");
            }
        }
    }

    #[test]
    fn grain_is_strongest_in_the_midtones_and_response_moves_it() {
        let sd = |gp: &GrainParams, lum: f32| stats(&patch(gp, 160, lum)).1.sqrt();
        let gp = GrainParams::new(&grain(50.0, 30.0, 50.0), 6000.0).unwrap();
        let (dark, mid, light) = (sd(&gp, 0.05), sd(&gp, 0.5), sd(&gp, 0.95));
        assert!(mid > dark && mid > light && dark > 0.0 && light > 0.0, "{dark} {mid} {light}");
        let shadows = GrainParams::new(&Grain { response: -100.0, ..grain(50.0, 30.0, 50.0) }, 6000.0).unwrap();
        let highlights = GrainParams::new(&Grain { response: 100.0, ..grain(50.0, 30.0, 50.0) }, 6000.0).unwrap();
        assert!(sd(&shadows, 0.15) > sd(&shadows, 0.85), "negative response favours the shadows");
        assert!(sd(&highlights, 0.85) > sd(&highlights, 0.15), "positive response favours the highlights");
        // Amount 50 in mid-grey at the reference resolution: about 0.03 (8 levels of 255)
        assert!((0.018..0.05).contains(&mid), "mid-grey sd {mid}");
    }

    /// Colour 0: one grain layer, the same in every channel. Colour 100: an independent dye layer
    /// per channel (channels uncorrelated). In between, partly shared.
    #[test]
    fn colour_grain_goes_from_one_layer_to_independent_dye_layers() {
        let at = |color: f64| {
            let gp = GrainParams::new(&Grain { color, ..grain(60.0, 60.0, 50.0) }, 3000.0).unwrap();
            let n = 90;
            let mut ch = [Vec::new(), Vec::new(), Vec::new()];
            for i in 0..n * n {
                let o = gp.apply(((i % n) as f32 + 0.5) / 3000.0, ((i / n) as f32 + 0.5) / 3000.0, 0.5);
                for k in 0..3 {
                    ch[k].push(o[k]);
                }
            }
            ch
        };
        let mono = at(0.0);
        assert!(mono[0] == mono[1] && mono[1] == mono[2], "mono grain is the same in every channel");
        let full = at(100.0);
        assert!(corr(&full[0], &full[1]).abs() < 0.15 && corr(&full[1], &full[2]).abs() < 0.15, "dye layers are independent");
        let half = at(50.0);
        let c = corr(&half[0], &half[2]);
        assert!((0.3..0.7).contains(&c), "half colour: channels share half their grain ({c})");
        // the same strength per channel whatever the colour
        let (s0, s1) = (stats(&mono[1]).1.sqrt(), stats(&full[1]).1.sqrt());
        assert!((s1 / s0 - 1.0).abs() < 0.2, "{s0} vs {s1}");
    }

    #[test]
    fn the_field_changes_continuously_with_tone() {
        let gp = GrainParams::new(&grain(50.0, 40.0, 50.0), 6000.0).unwrap();
        let mut worst = 0.0f32;
        for i in 0..1500 {
            let (x, y) = (i as f32 * 0.37, i as f32 * 0.11);
            for k in 0..60 {
                let p = 0.2 + k as f32 * 0.01;
                let jump = (gp.field(x, y, p + 1e-4, 3) - gp.field(x, y, p, 3)).abs();
                worst = worst.max(jump);
            }
        }
        assert!(worst < 0.02, "largest change for Δp = 1e-4: {worst}");
    }

    /// The band evaluation (CPU renders) gives what the per-pixel one (the GPU kernel's) gives,
    /// for straight, rotated, flipped and transposed bands, with and without colour grain and
    /// clumps.
    #[test]
    fn band_matches_per_pixel() {
        for (size, ppl, color, clumping) in
            [(25.0, 6000.0, 0.0, 30.0), (90.0, 2000.0, 70.0, 100.0), (0.0, 6000.0, 0.0, 0.0), (25.0, 900.0, 30.0, 60.0)]
        {
            let g = Grain { response: 30.0, color, clumping, size_variation: 70.0, ..grain(70.0, size, 80.0) };
            let gp = GrainParams::new(&g, ppl).unwrap();
            // (along a row, from row to row) per pixel, in output pixels
            for ((ax, ay), (rx, ry)) in
                [((1.0f32, 0.0f32), (0.0f32, 1.0f32)), ((0.97, 0.26), (-0.26, 0.97)), ((0.0, 1.0), (1.0, 0.0)), ((-0.6, 0.8), (0.8, 0.6))]
            {
                let (cols, rows) = (300, 7);
                let mut gx = Vec::new();
                let mut gy = Vec::new();
                for row in 0..rows {
                    for k in 0..cols {
                        let (u, v) = (k as f32 + 0.5, row as f32 + 0.5);
                        gx.push(0.2 + (u * ax + v * rx) / ppl as f32);
                        gy.push(0.3 + (u * ay + v * ry) / ppl as f32);
                    }
                }
                let mut px: Vec<[f32; 3]> = (0..cols * rows).map(|k| [0.1 + 0.8 * (k % 97) as f32 / 97.0; 3]).collect();
                let want: Vec<[f32; 3]> = px
                    .iter()
                    .enumerate()
                    .map(|(k, c)| {
                        let g = gp.apply(gx[k], gy[k], 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]);
                        [c[0] + g[0], c[1] + g[1], c[2] + g[2]]
                    })
                    .collect();
                gp.apply_band(&gx, &gy, cols, &mut px);
                for k in 0..cols * rows {
                    for ch in 0..3 {
                        assert!((px[k][ch] - want[k][ch]).abs() < 1e-4, "size {size} dir ({ax}, {ay}) px {k}: {:?} vs {:?}", px[k], want[k]);
                    }
                }
            }
        }
    }

    /// Settings saved before the newer grain controls existed (catalog, XMP, presets) load with
    /// their defaults and render the same grain as settings that name them at those defaults.
    #[test]
    fn settings_without_the_new_fields_are_back_compatible() {
        let old: lightcraft_develop::DevelopSettings =
            serde_json::from_value(serde_json::json!({"grain": {"amount": 20.0, "size": 30.0, "roughness": 60.0, "seed": 9}})).unwrap();
        let explicit = Grain { amount: 20.0, size: 30.0, roughness: 60.0, seed: 9, ..Grain::default() };
        assert_eq!(old.grain, explicit);
        assert_eq!((old.grain.color, old.grain.response, old.grain.size_variation, old.grain.clumping), (0.0, 0.0, 40.0, 30.0));
        let (a, b) = (GrainParams::new(&old.grain, 3000.0).unwrap(), GrainParams::new(&explicit, 3000.0).unwrap());
        assert_eq!(a, b);
        assert_eq!((a.color, a.gamma), (0.0, 1.0), "no colour grain, midtone response");
    }

    /// Through the whole pipeline: a 1/4-size render of a flat grey photo shows the grain of the
    /// full-size render box-downscaled to that size (same strength within 15 %), for grain that
    /// stays explicit and for grain that turns into per-pixel noise at the small size.
    #[test]
    fn small_pipeline_render_matches_downscaled_full_render() {
        use crate::{RenderRequest, SourceInfo, render};
        use lightcraft_raster::Rgb32f;
        // big enough that the small render's grain stands well above 8-bit rounding
        let (w, h) = (2400, 1800);
        let src = Rgb32f::filled(w, h, [0.18, 0.18, 0.18]);
        let info = SourceInfo::default();
        for size in [50.0, 100.0] {
            let s = lightcraft_develop::DevelopSettings { grain: grain(100.0, size, 50.0), ..Default::default() };
            let full = render(&src, &info, &s, &RenderRequest::fit(w, h)).image;
            let small = render(&src, &info, &s, &RenderRequest::fit(w / 4, h / 4)).image;
            assert_eq!((small.width, small.height), (w / 4, h / 4));
            let lum = |p: &[u8; 4]| (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0;
            let down: Vec<f32> = (0..(w / 4) * (h / 4))
                .map(|i| {
                    let (x, y) = (i % (w / 4), i / (w / 4));
                    let mut acc = 0.0;
                    for dy in 0..4 {
                        for dx in 0..4 {
                            acc += lum(&full.data[(y * 4 + dy) * w + x * 4 + dx]);
                        }
                    }
                    acc / 16.0
                })
                .collect();
            let sm: Vec<f32> = small.data.iter().map(lum).collect();
            let ((m0, v0), (m1, v1)) = (stats(&down), stats(&sm));
            let ratio = (v1 / v0).sqrt();
            assert!((m0 - m1).abs() < 0.5, "size {size}: mean {m0} vs {m1}");
            assert!((0.85..1.15).contains(&ratio), "size {size}: grain sd {} (small) vs {} (downscaled)", v1.sqrt(), v0.sqrt());
            assert!(v0.sqrt() > 0.3, "size {size}: visible grain ({})", v0.sqrt());
        }
    }

    /// Every control visibly changes the particles, and the seed is the pattern: another seed is
    /// an unrelated pattern, the same seed at another render size is the same particles, and
    /// Size Variation, Shape and Clumping change the particles without moving where they are.
    #[test]
    fn each_control_changes_the_particles() {
        let at = |g: Grain, ppl: f64| field(&GrainParams::new(&g, ppl).unwrap(), 150, 0.45, 0.5, g.seed);
        let base = Grain { size_variation: 40.0, clumping: 30.0, seed: 3, ..grain(50.0, 100.0, 50.0) };
        let reference = at(base, 6000.0);
        assert!(corr(&reference, &at(Grain { seed: 4, ..base }, 6000.0)).abs() < 0.1, "seed 4 is another pattern");
        assert!(corr(&reference, &at(base, 9000.0)) > 0.75, "a bigger render keeps the particles");
        for (what, lo, hi) in [
            ("size variation", Grain { size_variation: 0.0, ..base }, Grain { size_variation: 100.0, ..base }),
            ("shape", Grain { roughness: 0.0, ..base }, Grain { roughness: 100.0, ..base }),
            ("clumping", Grain { clumping: 0.0, ..base }, Grain { clumping: 100.0, ..base }),
        ] {
            let c = corr(&at(lo, 6000.0), &at(hi, 6000.0));
            assert!(c < 0.9, "{what} changes the particles ({c})");
        }
        // Shape: irregular, elongated particles have more edge for the same coverage
        let edges = |v: &[f32]| {
            let n = 150;
            let mut e = 0;
            for i in 0..n * n - 1 {
                if i % n != n - 1 && (v[i] > 0.5) != (v[i + 1] > 0.5) {
                    e += 1;
                }
            }
            e
        };
        let (round, rough) = (edges(&at(Grain { roughness: 0.0, ..base }, 6000.0)), edges(&at(Grain { roughness: 100.0, ..base }, 6000.0)));
        assert!(rough as f64 > round as f64 * 1.05, "shape 100 has more edge than shape 0 ({rough} vs {round})");
        // Clumping: particles gather, so coverage varies more between neighbourhoods
        let blocks = |v: &[f32]| stats(&box_down(v, 150, 10)).1;
        let (loose, clumped) = (blocks(&at(Grain { clumping: 0.0, ..base }, 6000.0)), blocks(&at(Grain { clumping: 100.0, ..base }, 6000.0)));
        assert!(clumped > loose * 1.2, "clumps make coverage patchier ({clumped} vs {loose})");
    }

    /// Hostile settings (NaN, infinities, far out of range) and positions never panic and give
    /// finite output.
    #[test]
    fn extreme_values_never_panic() {
        let vals = [f64::NAN, f64::INFINITY, -1e300, 0.0, 100.0];
        for &a in &[f64::NAN, 1e300, 100.0] {
            for &v in &vals {
                for ppl in [f64::NAN, 0.0, 1.0, 1e12, 6000.0] {
                    let g = Grain { amount: a, size: v, roughness: v, seed: u32::MAX, color: v, response: v, size_variation: v, clumping: v };
                    let Some(gp) = GrainParams::new(&g, ppl) else { continue };
                    for (x, y, l) in [(0.0, 0.0, 0.5), (f32::NAN, 1.0, f32::NAN), (f32::INFINITY, -1e30, 2.0), (1e9, -1e9, -1.0)] {
                        let o = gp.apply(x, y, l);
                        assert!(o.iter().all(|c| c.is_finite()), "{g:?} {ppl} {x} {y} {l} → {o:?}");
                    }
                }
            }
        }
        assert!(GrainParams::new(&grain(f64::NAN, 25.0, 50.0), 1000.0).is_none());
        assert!(GrainParams::new(&grain(-5.0, 25.0, 50.0), 1000.0).is_none());
    }
}
