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

/// The development halo at most (its ring's integral as a fraction of the clump's grains'), and
/// its outer radius at most (clump sizes).
const HALO_MAX: f64 = 3.0;
const HALO_R_MAX: f64 = 6.0;

/// Response ±100: the amplitude at black relative to white is `exp(∓RESPONSE_TILT)`.
const RESPONSE_TILT: f64 = 1.386_294_4;

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

/// The softening one output pixel applies to the halo (pixels). Below the reference resolution
/// the amplitude is set from the reference render's covariance (`ref_covariance`), so this only
/// shapes the halo of a small render, not its strength.
const HALO_PIXEL_SOFTNESS: f64 = 0.44;

/// Grain units are mean grain radii; the reference resolution for the Amount calibration is a
/// 6000 px long edge (a 24 MP export).
const REF_PX_PER_LONG: f64 = 6000.0;

/// Explicit particles while one output pixel's softening (the softness less the film's) is below
/// this many clump radii, Gaussian noise above the second bound, cross-faded between. Fine grain
/// on a 24 MP export (Size below about 10) is noise: particles a third of a pixel wide cost many
/// times the grain they add, and the noise is correlated as the particles are.
const EXPLICIT_UPTO: f64 = 0.35;
const GAUSSIAN_FROM: f64 = 0.8;

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
    /// The halo's inner and outer radius around a clump's centre (grain units, softened by the
    /// halo's pixel footprint), as `1 / R²` (0 without a halo).
    pub halo_in: f32,
    pub halo_out: f32,
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
    /// The halo's moments relative to `E[∫a_clump]`: `E[∫h]`, `E[∫h²]` and `E[∫a·h]`.
    pub halo_mean: f32,
    pub k_hh: f32,
    pub k_ah: f32,
    /// `area`, `kappa`, `k_hh` and `k_ah` at the reference resolution: the per-tone amplitude
    /// makes the grain's standard deviation there the same at every tone
    /// ([`GrainParams::tone_weight`]).
    pub area_ref: f32,
    pub kappa_ref: f32,
    pub k_hh_ref: f32,
    pub k_ah_ref: f32,
    /// Development halo `k`: a clump whose grains have densities `oᵢ` and radii `rᵢ` lightens its
    /// surroundings by `k·W·(P(d²/R_out²) − P(d²/R_in²))`, `W = Σ oᵢ·rᵢ²`, `P(u) = (1 − u)²`
    /// below 1: a ring of lower density around the clump whose integral is the look's `halo`
    /// times `π·W` ([`GrainParams::clump_halo`]); 0 = none.
    pub halo: f32,
    /// The halo's outer radius relative to the clump's (1 without a halo).
    pub halo_r: f32,
    /// Response: the amplitude is multiplied by `exp(tilt·(l − ½))` at luminance `l`.
    pub tilt: f32,
    /// Cross-fade weights of the explicit field and of the Gaussian noise (`cos² + sin² = 1`).
    pub w_field: f32,
    pub w_noise: f32,
    /// The noise's neighbour weight and normalisation: each pixel's noise is
    /// `(n(x) − e·Σ n(4 neighbours))·norm`, so that its correlation with the next pixel is that
    /// of the reference render box-averaged to this render's pixels ([`GrainParams::noise`]).
    pub noise_e: f32,
    pub noise_n: f32,
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

/// What Size and Roughness map to (calibrated against Lightroom Classic exports: see
/// [`GrainLook::new`] and `docs/grain.md`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GrainLook {
    /// Mean grain radius, as a fraction of the frame's long edge.
    pub radius: f64,
    /// Grain shape, 0..1: round, even beads (0) to elongated, lobed grains (1).
    pub shape: f64,
    /// Development halo: the ring's integral as a fraction of its clump's grains' (0 = none).
    pub halo: f64,
    /// The halo's outer radius, in clump sizes.
    pub halo_r: f64,
    /// Standard deviation of the grain at Amount 100 at the reference resolution (display-encoded
    /// units: 1 = 255 levels), the same at every tone.
    pub level: f64,
}

impl GrainLook {
    /// The look of Size `size` and Roughness `roughness` (both 0..100), fitted to Lightroom
    /// Classic exports at a 6000 px long edge (`docs/grain.md`, checked by
    /// `grain_matches_lightroom_classic_exports`): the grain's strength and its autocorrelation
    /// at Size 0, 25 and 100 (Roughness 50) and at Roughness 0 and 100 (Size 25). Between those
    /// points the curves are smooth guesses.
    pub fn new(size: f64, roughness: f64) -> GrainLook {
        let size = finite(size, 0.0, 100.0, 25.0);
        let rough = finite(roughness, 0.0, 100.0, 50.0) / 100.0;
        // mean radius at 6000 px: 0.3 px at Size 0 (grain finer than a pixel), 0.83 at 25, 3 at
        // 100; Roughness 0 is finer, 100 coarser
        let r_px = (0.3 + 2.7 * (size / 100.0).powf(1.17)) * (0.96 - 0.33 * rough + 0.82 * rough * rough);
        // a development halo makes the grain regular (negative autocorrelation one to three
        // pixels out): below Roughness 50, and for grain finer than Size 25
        let smooth = (1.0 - 2.0 * rough).max(0.0);
        let fine = (1.0 - size / 25.0).max(0.0);
        let halo = smooth + 1.1 * fine * fine;
        // strength at Amount 100 (8-bit levels at every tone): Size 0 46.2, 25 17.1, 100 11.7;
        // Roughness 0 and 100 are 1.45× and 1.37× Roughness 50
        let levels = (2.447 + 1.386 * (-size / 19.88).exp()).exp();
        let d = rough - 0.5;
        let rough_gain = 1.0 - 0.084 * d + 1.6384 * d * d;
        GrainLook { radius: r_px / REF_PX_PER_LONG, shape: rough, halo, halo_r: 3.2, level: levels * rough_gain / 255.0 }
    }
}

/// Standard deviation of the explicit field at `p` (coverage less halo), for a clump coverage
/// integral `area`, the moment ratios `kappa`, `k_hh`, `k_ah` and Poisson cell side `cell`:
/// `(1 − p)²·(exp(κ·μ') − 1 + μ'·(k_hh − 2·k_ah))`, where `μ' = −ln(1 − p)` counts
/// fading-in clumps by `f²` (shared with the GPU kernel).
#[inline]
pub fn coverage_std_at(p: f32, area: f32, kappa: f32, k_hh: f32, k_ah: f32, cell: f32) -> f32 {
    let q = 1.0 - p;
    let t = -q.ln() / area * cell * cell;
    let fw = t.min(1.0);
    let mu = -q.ln() * (1.0 - fw / (6.0 * t.max(1e-12)));
    let var = q * q * ((kappa * mu).exp() - 1.0 + mu * (k_hh - 2.0 * k_ah));
    var.max(0.0).sqrt()
}

/// A clump's moments ([`clump_moments`]): `E[∫a]`, `E[∫a²]` of its coverage `a`, and `E[∫h]`,
/// `E[∫h²]`, `E[∫a·h]` with its halo `h`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Moments {
    a1: f64,
    a2: f64,
    h1: f64,
    h2: f64,
    ah: f64,
}

/// The parameters a clump's statistics depend on: shape, halo and softness.
fn moment_key(gp: &GrainParams) -> MomentKey {
    [
        gp.spread.to_bits(),
        gp.aspect.to_bits(),
        gp.lump.to_bits(),
        gp.clump_extra.to_bits(),
        gp.clump_d.to_bits(),
        gp.s.to_bits(),
        gp.halo.to_bits(),
        gp.halo_r.to_bits(),
        gp.halo_in.to_bits(),
    ]
}

/// Cached [`clump_moments`]: keyed by the shape parameters, the halo and the softness.
type MomentKey = [u32; 9];
static MOMENTS: Mutex<Vec<(MomentKey, Moments)>> = Mutex::new(Vec::new());

/// The clump moments for `gp`'s shape parameters, halo and softness, measured by rasterising a
/// fixed set of sample clumps drawn by the renderer's own generator (cached: a slider drag that
/// changes only Amount, Color or Response, or a render at the same size, measures nothing).
fn clump_moments(gp: &GrainParams) -> Moments {
    let key = moment_key(gp);
    if let Some(m) = MOMENTS.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|e| e.0 == key).map(|e| e.1) {
        return m;
    }
    const SAMPLES: u32 = 512;
    // finer with a halo: the box holds the halo, the grain is a smaller part of it
    let grid: usize = if gp.halo > 0.0 { 44 } else { 28 };
    // one entry per sample clump, in parallel, summed in order (the same result every time)
    let mut per = vec![Moments::default(); SAMPLES as usize];
    lightcraft_raster::par_rows(&mut per, 1, |k, out| {
        let mut subs = [Sub::default(); MAX_SUB as usize];
        let h = pcg(0x51ed_270b ^ (k as u32).wrapping_mul(0x2545_f491));
        let n = gp.sub_count(h);
        let mut rb = 0.0f32;
        for (j, g) in subs.iter_mut().enumerate().take(n as usize) {
            let hj = sub_hash(h, j as u32);
            let (ox, oy) = gp.sub_offset(hj);
            *g = gp.sub_shape(hj, ox, oy);
            rb = rb.max((ox * ox + oy * oy).sqrt() + g.reach);
        }
        let w = gp.clump_weight(h);
        if w > 0.0 && gp.halo_out > 0.0 {
            rb = rb.max(1.0 / gp.halo_out.sqrt());
        }
        let Some(out) = out.first_mut() else { return };
        if !(rb > 0.0 && rb.is_finite()) {
            return;
        }
        let step = 2.0 * rb / grid as f32;
        let mut acc = Moments::default();
        for gy in 0..grid {
            for gx in 0..grid {
                let (px, py) = (-rb + (gx as f32 + 0.5) * step, -rb + (gy as f32 + 0.5) * step);
                let mut t = 1.0f32;
                for g in subs.iter().take(n as usize) {
                    let (dx, dy) = (px - g.x, py - g.y);
                    let d2 = dx * dx + dy * dy;
                    if d2 < g.reach * g.reach {
                        t *= 1.0 - gp.sub_cover(g, dx, dy, d2);
                    }
                }
                let hs = gp.clump_halo(w, px * px + py * py);
                let (a, h) = ((1.0 - t) as f64, hs as f64);
                acc.a1 += a;
                acc.a2 += a * a;
                acc.h1 += h;
                acc.h2 += h * h;
                acc.ah += a * h;
            }
        }
        let cell = (step * step) as f64;
        *out = Moments { a1: acc.a1 * cell, a2: acc.a2 * cell, h1: acc.h1 * cell, h2: acc.h2 * cell, ah: acc.ah * cell };
    });
    let m =
        per.iter().fold(Moments::default(), |m, e| Moments { a1: m.a1 + e.a1, a2: m.a2 + e.a2, h1: m.h1 + e.h1, h2: m.h2 + e.h2, ah: m.ah + e.ah });
    let n = SAMPLES as f64;
    let m = Moments { a1: m.a1 / n, a2: m.a2 / n, h1: m.h1 / n, h2: m.h2 / n, ah: m.ah / n };
    let mut cache = MOMENTS.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= 64 {
        cache.remove(0);
    }
    cache.push((key, m));
    m
}

/// Lags of the reference render's covariance at most, per axis ([`ref_covariance`]).
const LAG_MAX: usize = 16;

/// The covariance of the grain at the reference resolution between points `(lx, ly)·pitch` apart
/// (`lx` in `0..=lags`, `ly` in `−lags..=lags`), at coverage ½ ([`ref_covariance`]).
#[derive(Clone, Debug, PartialEq)]
struct RefCovariance {
    /// Lattice pitch, in reference pixels (1, or more for coarse grain).
    pitch: f64,
    lags: usize,
    c: Vec<f64>,
}

impl RefCovariance {
    fn at(&self, lx: i64, ly: i64) -> f64 {
        let (lx, ly) = if lx < 0 { (-lx, -ly) } else { (lx, ly) };
        let l = self.lags as i64;
        if lx > l || ly.abs() > l {
            return 0.0;
        }
        self.c.get((lx * (2 * l + 1) + ly + l) as usize).copied().unwrap_or(0.0)
    }

    /// The variance of the reference render box-averaged over blocks of `k × k` pixels, relative
    /// to its variance per pixel (`None` below one lattice pitch).
    fn block_variance(&self, k: f64) -> Option<f64> {
        let kp = k / self.pitch;
        if !(kp >= 1.0 && kp.is_finite()) {
            return None;
        }
        let l = self.lags as i64;
        let tri = |v: i64| (1.0 - v.unsigned_abs() as f64 / kp).max(0.0);
        let mut v = 0.0;
        for lx in -l..=l {
            let wx = tri(lx);
            if wx <= 0.0 {
                continue;
            }
            for ly in -l..=l {
                v += self.at(lx, ly) * wx * tri(ly);
            }
        }
        let c0 = self.at(0, 0);
        (c0 > 0.0).then(|| (v / (kp * kp) / c0).max(0.0))
    }

    /// The correlation of neighbouring pixels (horizontal and vertical alike) of the reference
    /// render box-averaged over blocks of `k × k` pixels (`None` below one lattice pitch).
    fn block_correlation(&self, k: f64) -> Option<f64> {
        let kp = k / self.pitch;
        if !(kp >= 1.0 && kp.is_finite()) {
            return None;
        }
        let l = self.lags as i64;
        let tri = |v: f64| (1.0 - v.abs() / kp).max(0.0);
        let (mut c0, mut c1) = (0.0, 0.0);
        for lx in -l..=l {
            for ly in -l..=l {
                let c = self.at(lx, ly);
                let wy = tri(ly as f64);
                c0 += c * tri(lx as f64) * wy;
                c1 += c * tri(lx as f64 - kp) * wy;
            }
        }
        (c0 > 0.0).then(|| c1 / c0)
    }
}

/// Cached [`ref_covariance`].
static REF_COVARIANCE: Mutex<Vec<(MomentKey, RefCovariance)>> = Mutex::new(Vec::new());

/// The covariance between the pixels of the reference render (`gp` set up at the reference
/// resolution, `px` its pixel in grain units, `m` its clump moments), from the same exact
/// statistics as the variance: the clump integrals `E[∫a(u)·a(u + l)]`, `E[∫h(u)·h(u + l)]` and
/// `E[∫a(u)·h(u + l) + h(u)·a(u + l)]`, measured on sample clumps rasterised at random phases.
fn ref_covariance(gp: &GrainParams, px: f64, m: &Moments) -> RefCovariance {
    let key = moment_key(gp);
    if let Some(c) = REF_COVARIANCE.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|e| e.0 == key).map(|e| e.1.clone()) {
        return c;
    }
    let reach = 2.0 * gp.support as f64;
    let pitch = px.max(reach / LAG_MAX as f64).max(1e-6);
    let lags = ((reach / pitch).ceil() as usize).clamp(1, LAG_MAX);
    let width = 2 * lags + 1;
    const SAMPLES: u32 = 128;
    let p = pitch as f32;
    // one table per sample clump, in parallel, summed in order (the same result every time)
    let mut per = vec![Vec::new(); SAMPLES as usize];
    lightcraft_raster::par_rows(&mut per, 1, |k, out| {
        let Some(out) = out.first_mut() else { return };
        let mut acc = vec![[0.0f64; 3]; (lags + 1) * width];
        let mut subs = [Sub::default(); MAX_SUB as usize];
        let h = pcg(0x51ed_270b ^ (k as u32).wrapping_mul(0x2545_f491));
        let n = gp.sub_count(h);
        let mut rb = 0.0f32;
        for (j, g) in subs.iter_mut().enumerate().take(n as usize) {
            let hj = sub_hash(h, j as u32);
            let (ox, oy) = gp.sub_offset(hj);
            *g = gp.sub_shape(hj, ox, oy);
            rb = rb.max((ox * ox + oy * oy).sqrt() + g.reach);
        }
        let w = gp.clump_weight(h);
        if w > 0.0 && gp.halo_out > 0.0 {
            rb = rb.max(1.0 / gp.halo_out.sqrt());
        }
        if !(rb > 0.0 && rb.is_finite()) {
            return;
        }
        // the lattice at a random phase: an unbiased estimate of the integrals
        let ph = pcg(h ^ 0x7f4a_7c15);
        let (fx, fy) = (unit16(ph) * p, unit16(ph >> 16) * p);
        let i0 = ((-rb - fx) / p).floor() as i64;
        let g_n = ((2.0 * rb / p).ceil() as usize + 2).min(2 * LAG_MAX + 4);
        let mut a = vec![0.0f64; g_n * g_n];
        let mut hv = vec![0.0f64; g_n * g_n];
        for gy in 0..g_n {
            for gx in 0..g_n {
                let (ux, uy) = (fx + (i0 + gx as i64) as f32 * p, fy + (i0 + gy as i64) as f32 * p);
                let mut t = 1.0f32;
                for g in subs.iter().take(n as usize) {
                    let (dx, dy) = (ux - g.x, uy - g.y);
                    let d2 = dx * dx + dy * dy;
                    if d2 < g.reach * g.reach {
                        t *= 1.0 - gp.sub_cover(g, dx, dy, d2);
                    }
                }
                let hs = gp.clump_halo(w, ux * ux + uy * uy);
                if let (Some(av), Some(hh)) = (a.get_mut(gy * g_n + gx), hv.get_mut(gy * g_n + gx)) {
                    (*av, *hh) = ((1.0 - t) as f64, hs as f64);
                }
            }
        }
        let reach_l = lags.min(g_n - 1);
        for lx in 0..=reach_l {
            for ly in -(reach_l as i64)..=reach_l as i64 {
                let mut s3 = [0.0f64; 3];
                for y in 0..g_n {
                    let y2 = y as i64 + ly;
                    if y2 < 0 || y2 >= g_n as i64 {
                        continue;
                    }
                    for x in 0..g_n - lx {
                        let (i, j) = (y * g_n + x, y2 as usize * g_n + x + lx);
                        let (Some(&a1), Some(&a2), Some(&h1), Some(&h2)) = (a.get(i), a.get(j), hv.get(i), hv.get(j)) else { continue };
                        s3[0] += a1 * a2;
                        s3[1] += h1 * h2;
                        s3[2] += a1 * h2 + h1 * a2;
                    }
                }
                if let Some(e) = acc.get_mut(lx * width + (ly + lags as i64) as usize) {
                    *e = s3;
                }
            }
        }
        *out = acc;
    });
    let mut acc = vec![[0.0f64; 3]; (lags + 1) * width];
    for table in &per {
        for (e, v) in acc.iter_mut().zip(table) {
            for (e, v) in e.iter_mut().zip(v) {
                *e += v;
            }
        }
    }
    // the covariance at coverage ½ from the integrals (as `coverage_std_at`)
    let cell = (pitch * pitch) / SAMPLES as f64;
    let (q, a1) = (0.5f64, m.a1.max(1e-9));
    let t = -q.ln() / a1 * (gp.cell as f64).powi(2);
    let mu = -q.ln() * (1.0 - t.min(1.0) / (6.0 * t.max(1e-12)));
    let c = acc.iter().map(|[aa, hh, ah]| q * q * ((mu * aa * cell / a1).exp() - 1.0 + mu * (hh - ah) * cell / a1)).collect();
    let cov = RefCovariance { pitch: pitch / px, lags, c };
    let mut cache = REF_COVARIANCE.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= 64 {
        cache.remove(0);
    }
    cache.push((key, cov.clone()));
    cov
}

impl GrainParams {
    /// Parameters for a render with `px_per_long` output pixels per long edge of the uncropped
    /// frame; `None` when there is no grain. Out-of-range or non-finite settings are clamped.
    pub fn new(g: &Grain, px_per_long: f64) -> Option<GrainParams> {
        let look = GrainLook::new(finite(g.size, 0.0, 100.0, 25.0), finite(g.roughness, 0.0, 100.0, 50.0));
        GrainParams::with_look(g, px_per_long, &look)
    }

    /// [`GrainParams::new`] with the Size and Roughness mapping given explicitly (calibration).
    pub fn with_look(g: &Grain, px_per_long: f64, look: &GrainLook) -> Option<GrainParams> {
        let amount = finite(g.amount, 0.0, 100.0, 0.0) / 100.0;
        let ppl = finite(px_per_long, 1.0, 1e7, 1.0);
        if amount <= 0.0 {
            return None;
        }
        let variation = finite(g.size_variation, 0.0, 100.0, 40.0) / 100.0;
        let shape = finite(look.shape, 0.0, 1.0, 0.5);
        let clumping = finite(g.clumping, 0.0, 100.0, 30.0) / 100.0;
        let r_norm = finite(look.radius, 1e-6, 0.01, 0.000_14);
        let halo = finite(look.halo, 0.0, HALO_MAX, 0.0);
        let halo_r = if halo > 0.0 { finite(look.halo_r, 1.05, HALO_R_MAX, 2.0) } else { 1.0 };
        // the softness, quantized so that nearby render sizes share their cached moments
        let pixel_soft = PIXEL_SOFTNESS[0] + (PIXEL_SOFTNESS[1] - PIXEL_SOFTNESS[0]) * clumping;
        let soft_at = |ppl: f64, pixel_soft: f64| {
            let s = (FILM_SOFT * FILM_SOFT + (pixel_soft / (r_norm * ppl)).powi(2)).sqrt();
            (s * 256.0).round().max(1.0) / 256.0
        };
        let (spread, aspect, lump) = (SPREAD_MAX * variation, ASPECT_MAX * shape, LUMP_MAX * shape);
        let clump_extra = CLUMP_EXTRA_MAX * clumping;
        let clump_d = CLUMP_SPREAD_MIN + (CLUMP_SPREAD_MAX - CLUMP_SPREAD_MIN) * clumping;
        // a clump's scale (grain radii): one grain, or a cluster
        let clump_size = 1.0 + 0.7 * clump_d * clump_extra.min(1.0);
        let mut gp = GrainParams {
            amp: 0.0,
            inv_r: (1.0 / r_norm) as f32,
            s: 0.0,
            inv_s: 0.0,
            halo_in: 0.0,
            halo_out: 0.0,
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
            halo_mean: 0.0,
            k_hh: 0.0,
            k_ah: 0.0,
            area_ref: 1.0,
            kappa_ref: 1.0,
            k_hh_ref: 0.0,
            k_ah_ref: 0.0,
            halo: 0.0,
            halo_r: halo_r as f32,
            tilt: (finite(g.response, -100.0, 100.0, 0.0) / 100.0 * RESPONSE_TILT) as f32,
            w_field: 1.0,
            w_noise: 0.0,
            noise_e: 0.0,
            noise_n: 1.0,
            color: (finite(g.color, 0.0, 100.0, 0.0) / 100.0) as f32,
            gamma: 2f64.powf(-finite(g.response, -100.0, 100.0, 0.0) / 50.0) as f32,
            seed: g.seed,
            px_per_long: ppl as f32,
        };
        let set_soft = |gp: &mut GrainParams, ppl: f64| {
            let s = soft_at(ppl, pixel_soft);
            let sh = soft_at(ppl, HALO_PIXEL_SOFTNESS);
            (gp.s, gp.inv_s) = (s as f32, (1.0 / s) as f32);
            let r_max = (3.0 * spread).exp() * (1.0 + aspect).sqrt() * (1.0 + lump);
            gp.sub_reach = ((r_max * r_max + s * s).sqrt() + s) as f32;
            let mut support = clump_d * std::f64::consts::SQRT_2 + gp.sub_reach as f64;
            if halo > 0.0 {
                // the ring around the clump, from its own size out to `halo_r` times it, scaled so
                // that its integral is `halo` times the grains' (`P` integrates to `πR²/3`)
                let ri2 = clump_size * clump_size + sh * sh;
                let ro2 = (halo_r * clump_size).powi(2) + sh * sh;
                (gp.halo_in, gp.halo_out) = ((1.0 / ri2) as f32, (1.0 / ro2) as f32);
                gp.halo = (3.0 * halo / (ro2 - ri2)) as f32;
                support = support.max(ro2.sqrt());
            }
            gp.support = support.min(CELL as f64 * MAX_REACH as f64 * 0.5 - 0.01) as f32;
        };
        // the grain's standard deviation at the reference resolution is `look.level` per unit of
        // Amount at every tone (`tone_weight` divides by the coverage's own deviation there)
        set_soft(&mut gp, REF_PX_PER_LONG);
        let ratios = |m: &Moments| {
            if m.a1 > 0.0 {
                let r = |v: f64| (v / m.a1).clamp(0.0, 1e3) as f32;
                (m.a1.max(1e-3) as f32, (m.a2 / m.a1).clamp(1e-6, 1.0) as f32, r(m.h1), r(m.h2), r(m.ah))
            } else {
                (1.0, 1.0, 0.0, 0.0, 0.0)
            }
        };
        let m_ref = clump_moments(&gp);
        (gp.area_ref, gp.kappa_ref, _, gp.k_hh_ref, gp.k_ah_ref) = ratios(&m_ref);
        let ref_px = 1.0 / (r_norm * REF_PX_PER_LONG);
        let ref_gp = gp;
        // below the reference resolution, the grain is the reference render box-averaged
        let ref_block = if ppl < REF_PX_PER_LONG { ref_covariance(&ref_gp, ref_px, &m_ref).block_variance(REF_PX_PER_LONG / ppl) } else { None };
        set_soft(&mut gp, ppl);
        let s = gp.s as f64;
        (gp.area, gp.kappa, gp.halo_mean, gp.k_hh, gp.k_ah) = ratios(&clump_moments(&gp));
        gp.amp = (amount * finite(look.level, 0.0, 1.0, 0.0)) as f32;
        if let Some(v) = ref_block {
            // what this render's own statistics give, scaled to the box-averaged reference render
            let own = coverage_std_at(0.5, gp.area, gp.kappa, gp.k_hh, gp.k_ah, gp.cell) as f64;
            let at_ref = coverage_std_at(0.5, gp.area_ref, gp.kappa_ref, gp.k_hh_ref, gp.k_ah_ref, gp.cell) as f64;
            if own > 0.0 && v > 0.0 {
                gp.amp *= (v.sqrt() * at_ref / own).clamp(0.25, 4.0) as f32;
            }
        }
        // the pixel's footprint (the softness less the film's) relative to a clump
        let px_soft = (s * s - FILM_SOFT * FILM_SOFT).max(0.0).sqrt();
        let t = ((px_soft / clump_size - EXPLICIT_UPTO) / (GAUSSIAN_FROM - EXPLICIT_UPTO)).clamp(0.0, 1.0);
        (gp.w_field, gp.w_noise) = if t <= 0.0 {
            (1.0, 0.0)
        } else if t >= 1.0 {
            (0.0, 1.0)
        } else {
            let a = t * std::f64::consts::FRAC_PI_2;
            (a.cos() as f32, a.sin() as f32)
        };
        if gp.w_noise > 0.0 {
            // the noise correlated with its neighbours as the reference render's pixels are, box-
            // averaged to this render's: `−2e / (1 + 4e²)` is the correlation of the next pixel
            let k = (REF_PX_PER_LONG / ppl).max(1.0);
            let rho = ref_covariance(&ref_gp, ref_px, &m_ref).block_correlation(k).unwrap_or(0.0).clamp(-0.45, 0.45);
            let e = if rho.abs() > 1e-6 { (-1.0 + (1.0 - 4.0 * rho * rho).sqrt()) / (4.0 * rho) } else { 0.0 };
            (gp.noise_e, gp.noise_n) = (e as f32, (1.0 / (1.0 + 4.0 * e * e).sqrt()) as f32);
        }
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

    /// Amplitude weight for display-encoded luminance `lum`: the inverse of the coverage's standard
    /// deviation at the reference resolution, so that the grain is equally strong at every tone,
    /// times the Response's tilt towards the shadows or the highlights.
    #[inline]
    pub fn tone_weight(&self, lum: f32) -> f32 {
        let l = if lum.is_finite() { lum.clamp(0.0, 1.0) } else { 0.5 };
        let p = P_MIN + (P_MAX - P_MIN) * self.shade(l);
        let sd = coverage_std_at(p, self.area_ref, self.kappa_ref, self.k_hh_ref, self.k_ah_ref, self.cell);
        let tilt = if self.tilt == 0.0 { 1.0 } else { (self.tilt * (l - 0.5)).exp() };
        tilt / sd.max(1e-4)
    }

    /// Expected clumps per Poisson cell at coverage `p`.
    #[inline]
    fn per_cell(&self, p: f32) -> f32 {
        -(1.0 - p).ln() / self.area * self.cell * self.cell
    }

    /// Standard deviation of the explicit field at `p` at this render's resolution (the Gaussian
    /// noise's amplitude; see [`coverage_std_at`]).
    #[inline]
    pub fn coverage_std(&self, p: f32) -> f32 {
        coverage_std_at(p, self.area, self.kappa, self.k_hh, self.k_ah, self.cell)
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
        let (r, o) = self.sub_size(hj);
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

    /// Radius and density of the grain with hash `hj`.
    #[inline]
    fn sub_size(&self, hj: u32) -> (f32, f32) {
        let rad = pcg(hj ^ GRAIN_HASH[3]);
        let z = 2.0 * (unit8(rad) + unit8(rad >> 8) + unit8(rad >> 16)) - 3.0;
        ((self.spread * z).exp(), 0.25 + 0.75 * unit8(rad >> 24))
    }

    /// The halo weight of the clump with hash `h`: `Σ oᵢ·rᵢ²` over its grains (0 without a halo).
    #[inline]
    fn clump_weight(&self, h: u32) -> f32 {
        if self.halo <= 0.0 {
            return 0.0;
        }
        (0..self.sub_count(h))
            .map(|j| {
                let (r, o) = self.sub_size(sub_hash(h, j));
                o * r * r
            })
            .sum()
    }

    /// The halo of a clump of weight `w` at squared distance `d2` from its centre: a ring from the
    /// clump's own size out to `halo_r` times it (no square root: a polynomial in `d2`).
    #[inline]
    fn clump_halo(&self, w: f32, d2: f32) -> f32 {
        let u = d2 * self.halo_out;
        if u >= 1.0 || w == 0.0 {
            return 0.0;
        }
        let v = d2 * self.halo_in;
        let inner = if v < 1.0 { (1.0 - v) * (1.0 - v) } else { 0.0 };
        self.halo * w * ((1.0 - u) * (1.0 - u) - inner)
    }

    /// Coverage (density × softened profile) of grain `g` at offset (dx, dy) from its centre,
    /// `d2 = dx² + dy²`.
    #[inline]
    fn sub_cover(&self, g: &Sub, dx: f32, dy: f32, d2: f32) -> f32 {
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
        g.o * Self::profile(d, r, self.s, self.inv_s)
    }

    /// A disc of radius `r` softened by the edge width: its value at distance `d` from the centre
    /// (integral ≈ πr²).
    #[inline]
    fn profile(d: f32, r: f32, s: f32, inv_s: f32) -> f32 {
        let rho2 = r * r + s * s;
        let tau = (d - rho2.sqrt()) * inv_s;
        if tau >= 1.0 {
            return 0.0;
        }
        let prof = if tau <= -1.0 { 1.0 } else { (1.0 - tau) * (1.0 - tau) * (2.0 + tau) * 0.25 };
        r * r / (rho2 + s * s * 0.2) * prof
    }

    /// Coverage (the union of its grains) and halo (their sum) of the clump with hash `h` at
    /// offset (dx, dy) from its centre.
    fn clump_cover(&self, h: u32, dx: f32, dy: f32) -> (f32, f32) {
        let mut t = 1.0f32;
        let rr = self.sub_reach * self.sub_reach;
        let hs = self.clump_halo(self.clump_weight(h), dx * dx + dy * dy);
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
        (1.0 - t, hs)
    }

    /// The halo's expected value at coverage `p` (its sum over the clumps).
    #[inline]
    fn halo_at(&self, p: f32) -> f32 {
        if self.halo > 0.0 { -(1.0 - p).ln() * self.halo_mean } else { 0.0 }
    }

    /// Grain coverage at grain-unit position (x, y) for expected coverage `p` and layer seed, less
    /// the halo's deviation from its mean on the light the grains let through (`1 − p`), so that
    /// the mean is `p` and the halo is as strong relative to the grains at every tone.
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
        let (mut keep, mut hs) = (1.0f32, 0.0f32);
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
                    let (a, ha) = self.clump_cover(h, ex, ey);
                    if a == 0.0 && ha == 0.0 {
                        continue;
                    }
                    let f = if prod >= e_full { 1.0 } else { ((end + prod.ln()) / fw).clamp(0.0, 1.0) };
                    keep *= 1.0 - f * a;
                    hs += f * ha;
                }
            }
        }
        1.0 - keep - (1.0 - p) * (hs - self.halo_at(p))
    }

    /// Per-pixel thresholds as in [`GrainParams::field`], for expected coverages `ps`: `(end, fade
    /// width, e^−end, e^(fw − end))` each, and the smallest `e^−end`.
    fn pixel_ends(&self, ps: &[f32]) -> (Vec<(f32, f32, f32, f32)>, f32) {
        let mut ends = Vec::with_capacity(ps.len());
        let mut e_min = 1.0f32;
        for &p in ps {
            let t = self.per_cell(p);
            let fw = t.clamp(1e-6, 1.0);
            let end = t + 0.5 * fw;
            let e_end = (-end).exp();
            e_min = e_min.min(e_end);
            ends.push((end, fw, e_end, (fw - end).exp()));
        }
        (ends, e_min)
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
        let (ends, e_min) = self.pixel_ends(&ps[..n]);
        let keep = &mut out[..n];
        keep.fill(1.0);
        // per pixel, the clump being splatted: Π(1 − aⱼ) over its grains, and the pixels it touched
        let mut clump_t = vec![1.0f32; n];
        // and Σ hⱼ (the halo), and the halo summed over the clumps
        let mut clump_h = vec![0.0f32; n];
        let mut halo = vec![0.0f32; n];
        let mut touched: Vec<usize> = Vec::new();
        let salt = pcg(seed);
        let last = (cols - 1) as f32;
        let halo_reach = if self.halo > 0.0 && self.halo_out > 0.0 { 1.0 / self.halo_out.sqrt() } else { 0.0 };
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
            let Some((y_lo, y_hi)) = column_y_extent(&quad, cx0, cx1) else {
                continue;
            };
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
                    let mut weight = None;
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
                                // as `field` rounds it: from the clump's centre, then the grain's offset
                                let (ex, ey) = ((x - cx) - ox, (y - cy) - oy);
                                let d2 = ex * ex + ey * ey;
                                if d2 >= g.reach * g.reach {
                                    continue;
                                }
                                let a = self.sub_cover(&g, ex, ey, d2);
                                if a == 0.0 {
                                    continue;
                                }
                                if let (Some(t), Some(hq)) = (clump_t.get_mut(q), clump_h.get_mut(q)) {
                                    if *t == 1.0 && *hq == 0.0 {
                                        touched.push(q);
                                    }
                                    *t *= 1.0 - a;
                                }
                            }
                        }
                        // the clump's halo, from its centre
                        if halo_reach > 0.0 {
                            let w = *weight.get_or_insert_with(|| self.clump_weight(h));
                            let Some((pa, pb)) = row_reach(row, cx, cy, halo_reach) else { continue };
                            for q in row * cols + pa..=row * cols + pb {
                                let (Some(&(_, _, e_end, _)), Some(&x), Some(&y)) = (ends.get(q), xs.get(q), ys.get(q)) else { continue };
                                if prod <= e_end {
                                    continue;
                                }
                                let (ex, ey) = (x - cx, y - cy);
                                let ha = self.clump_halo(w, ex * ex + ey * ey);
                                if ha == 0.0 {
                                    continue;
                                }
                                if let (Some(t), Some(hq)) = (clump_t.get_mut(q), clump_h.get_mut(q)) {
                                    if *t == 1.0 && *hq == 0.0 {
                                        touched.push(q);
                                    }
                                    *hq += ha;
                                }
                            }
                        }
                    }
                    let mut ln_prod = None;
                    for &q in &touched {
                        let (Some(&(end, fw, _, e_full)), Some(t), Some(hq)) = (ends.get(q), clump_t.get_mut(q), clump_h.get_mut(q)) else {
                            continue;
                        };
                        let (a, ha) = (1.0 - *t, *hq);
                        (*t, *hq) = (1.0, 0.0);
                        let f = if prod >= e_full { 1.0 } else { ((end + *ln_prod.get_or_insert_with(|| prod.ln())) / fw).clamp(0.0, 1.0) };
                        if let (Some(kq), Some(hs)) = (keep.get_mut(q), halo.get_mut(q)) {
                            *kq *= 1.0 - f * a;
                            *hs += f * ha;
                        }
                    }
                }
            }
        }
        for ((v, hs), p) in keep.iter_mut().zip(&halo).zip(ps) {
            *v = 1.0 - *v - (1.0 - p) * (hs - self.halo_at(*p));
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
            d -= self.w_noise * self.coverage_std(p) * self.noise(ix, iy, seed);
        }
        d
    }

    /// Unit-variance noise for pixel (ix, iy), correlated with its four neighbours by `noise_e`.
    #[inline]
    fn noise(&self, ix: i32, iy: i32, seed: u32) -> f32 {
        let n = gauss(ix, iy, seed);
        if self.noise_e == 0.0 {
            return n;
        }
        let around = gauss(ix.wrapping_sub(1), iy, seed)
            + gauss(ix.wrapping_add(1), iy, seed)
            + gauss(ix, iy.wrapping_sub(1), seed)
            + gauss(ix, iy.wrapping_add(1), seed);
        (n - self.noise_e * around) * self.noise_n
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
                    d -= self.w_noise * self.coverage_std(p) * self.noise(ix, iy, seed);
                }
                let d = amps[k] * d;
                let c = &mut e[k];
                *c = [c[0] + w[0] * d, c[1] + w[1] * d, c[2] + w[2] * d];
            }
        }
    }
}

/// The Y extent of the parallelogram `quad` within the column `cx0..=cx1`: its extremes lie on the
/// edges clipped to the column. `None` when no edge crosses the column.
fn column_y_extent(quad: &[(f32, f32); 4], cx0: f32, cx1: f32) -> Option<(f32, f32)> {
    let (mut y_lo, mut y_hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for e in 0..4 {
        let ((ax, ay), (bx, by)) = (quad[e], quad[(e + 1) % 4]);
        let ex = bx - ax;
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
            let yt = ay + t * (by - ay);
            y_lo = y_lo.min(yt);
            y_hi = y_hi.max(yt);
        }
    }
    (!(y_lo.is_nan() || y_hi.is_nan() || y_lo > y_hi)).then_some((y_lo, y_hi))
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

    /// Autocorrelation of an `n × n` patch at lags 0..8, averaged over the horizontal and
    /// vertical directions.
    fn acf(v: &[f32], n: usize) -> [f64; 8] {
        let (m, var) = stats(v);
        let mut out = [0.0; 8];
        for (k, o) in out.iter_mut().enumerate() {
            let mut c = 0.0;
            for y in 0..n - k {
                for x in 0..n - k {
                    let a = v[y * n + x] as f64 - m;
                    c += a * (v[y * n + x + k] as f64 - m) + a * (v[(y + k) * n + x] as f64 - m);
                }
            }
            *o = c / (2 * (n - k) * (n - k)) as f64 / var.max(1e-30);
        }
        out
    }

    /// The lag at which an autocorrelation first drops to `level` (linear between lags).
    fn width_at(ac: &[f64; 8], level: f64) -> f64 {
        for k in 1..8 {
            if ac[k] < level {
                return (k - 1) as f64 + (ac[k - 1] - level) / (ac[k - 1] - ac[k]);
            }
        }
        8.0
    }

    /// Calibration report (not a check): std (8-bit levels) and autocorrelation of the seven
    /// Lightroom Classic settings at 6000 px, for the mapping or for looks given in `GRAIN_LOOK`
    /// ("size rough radius_px shape halo halo_r level_scale;…").
    #[test]
    #[ignore]
    fn lrc_fit_report() {
        let report = |name: &str, g: Grain, look: GrainLook| {
            let gp = GrainParams::with_look(&g, 6000.0, &look).unwrap();
            let n = 256;
            let tones: Vec<String> =
                [39.0f32, 63.0, 118.0, 179.0, 231.0].iter().map(|t| format!("{:5.2}", stats(&patch(&gp, n, t / 255.0)).1.sqrt() * 255.0)).collect();
            let v = patch(&gp, n, 118.0 / 255.0);
            let ac = acf(&v, n);
            if std::env::var("GRAIN_DOWN").is_ok() {
                let big = patch(&gp, 640, 118.0 / 255.0);
                let downs: Vec<String> = [2usize, 4, 8, 16]
                    .iter()
                    .map(|k| {
                        let d = box_down(&big, 640, *k);
                        let sm = GrainParams::with_look(&g, 6000.0 / *k as f64, &look).unwrap();
                        let small = patch(&sm, 640 / k, 118.0 / 255.0);
                        format!("{:.2}/{:.2}", stats(&d).1.sqrt() * 255.0, stats(&small).1.sqrt() * 255.0)
                    })
                    .collect();
                println!("{name:<22} downscaled k2 k4 k8 k16 (export/small render): {}", downs.join("  "));
            }
            println!(
                "{name:<22} std {} mean {:+.2} | hm {:.2} 1/e {:.2} | ac {:.3} {:.3} {:.3} {:.3} | r {:.2}px wf {:.2}",
                tones.join(" "),
                stats(&v).0 * 255.0,
                width_at(&ac, 0.5),
                width_at(&ac, (-1f64).exp()),
                ac[1],
                ac[2],
                ac[3],
                ac[4],
                look.radius * 6000.0,
                gp.w_field
            );
        };
        for (a, sz, r) in [
            (25.0, 25.0, 50.0),
            (50.0, 25.0, 50.0),
            (100.0, 25.0, 50.0),
            (50.0, 0.0, 50.0),
            (50.0, 100.0, 50.0),
            (50.0, 25.0, 0.0),
            (50.0, 25.0, 100.0),
        ] {
            report(&format!("A{a} S{sz} R{r}"), grain(a, sz, r), GrainLook::new(sz, r));
        }
        if let Ok(spec) = std::env::var("GRAIN_LOOK") {
            for item in spec.split(';').filter(|s| !s.trim().is_empty()) {
                let v: Vec<f64> = item.split_whitespace().map(|x| x.parse().unwrap()).collect();
                let base = GrainLook::new(v[0], v[1]);
                let look = GrainLook { radius: v[2] / 6000.0, shape: v[3], halo: v[4], halo_r: v[5], level: base.level * v[6] };
                report(&format!("S{} R{} {:?}", v[0], v[1], &v[2..]), grain(50.0, v[0], v[1]), look);
            }
        }
    }

    /// Downscale report (not a check): grain sd and lag-1 correlation of a render at 6000/k vs the
    /// 6000 px render box-downscaled by k.
    #[test]
    #[ignore]
    fn downscale_report() {
        for (size, rough, clumping) in
            [(0.0, 50.0, 30.0), (0.0, 50.0, 80.0), (10.0, 50.0, 30.0), (25.0, 0.0, 30.0), (25.0, 50.0, 30.0), (60.0, 0.0, 30.0)]
        {
            let g = Grain { clumping, ..grain(60.0, size, rough) };
            let full = GrainParams::new(&g, 6000.0).unwrap();
            let mut line = format!("S{size} R{rough} C{clumping}:");
            for k in [2usize, 3, 4, 8] {
                let small = GrainParams::new(&g, 6000.0 / k as f64).unwrap();
                let n = 96;
                let big = patch(&full, n * k, 0.5);
                let down = box_down(&big, n * k, k);
                let sm = patch(&small, n, 0.5);
                let ratio = (stats(&sm).1 / stats(&down).1).sqrt();
                line += &format!("  k{k} sd {ratio:.2} lag1 {:.2}/{:.2} wf {:.2}", lag1(&down, n), lag1(&sm, n), small.w_field);
            }
            println!("{line}");
        }
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

    /// Grain on a flat grey chart rendered as the CPU renderer does ([`GrainParams::apply_band`]),
    /// at a 6000 px long edge: `n × n` pixels of 8-bit tone `tone`. Returns the grain's standard
    /// deviation in 8-bit levels and its autocorrelation.
    fn chart(g: &Grain, tone: f32, n: usize) -> (f64, [f64; 8]) {
        let gp = GrainParams::new(g, 6000.0).unwrap();
        let (mut gx, mut gy) = (Vec::with_capacity(n * n), Vec::with_capacity(n * n));
        for y in 0..n {
            for x in 0..n {
                gx.push((x as f32 + 0.5) / 6000.0 + 0.2);
                gy.push((y as f32 + 0.5) / 6000.0 + 0.3);
            }
        }
        let e = tone / 255.0;
        let mut px = vec![[e; 3]; n * n];
        gp.apply_band(&gx, &gy, n, &mut px);
        let v: Vec<f32> = px.iter().map(|c| (c[1] - e) * 255.0).collect();
        (stats(&v).1.sqrt(), acf(&v, n))
    }

    /// Measured from Lightroom Classic exports (Lightroom Classic 15.1.1, process version 15.4:
    /// full-size 6000 × 4000 JPEGs of a chart of flat grey steps; `docs/grain.md`): Amount, Size,
    /// Roughness; the grain's standard deviation in 8-bit levels in mid grey (118), the same from
    /// 63 to 179 within 1 %; and the lag (pixels) at which its autocorrelation falls to ½.
    const LRC_GRAIN: [(f64, f64, f64, f64, f64); 7] = [
        (25.0, 25.0, 50.0, 4.29, 1.13),
        (50.0, 25.0, 50.0, 8.55, 1.13),
        (100.0, 25.0, 50.0, 17.01, 1.14),
        (50.0, 0.0, 50.0, 23.08, 0.43),
        (50.0, 100.0, 50.0, 5.83, 3.38),
        (50.0, 25.0, 0.0, 12.42, 0.86),
        (50.0, 25.0, 100.0, 11.68, 1.54),
    ];

    /// The seven Lightroom Classic settings: the same strength (within 10 %) at every tone from 63
    /// to 179 (within 6 % of each other), and the same correlation width (within 10 %).
    #[test]
    fn grain_matches_lightroom_classic_exports() {
        for (amount, size, rough, lrc_sd, lrc_width) in LRC_GRAIN {
            let g = grain(amount, size, rough);
            let (sd, ac) = chart(&g, 118.0, 160);
            let width = width_at(&ac, 0.5);
            assert!((sd / lrc_sd - 1.0).abs() < 0.1, "A{amount} S{size} R{rough}: sd {sd:.2} levels, Lightroom Classic {lrc_sd}");
            assert!((width / lrc_width - 1.0).abs() < 0.1, "A{amount} S{size} R{rough}: width {width:.2} px, Lightroom Classic {lrc_width}");
            let tones: Vec<f64> = [63.0, 179.0].iter().map(|t| chart(&g, *t, 120).0).collect();
            for (t, v) in [63, 179].iter().zip(&tones) {
                assert!((v / sd - 1.0).abs() < 0.06, "A{amount} S{size} R{rough}: sd {v:.2} at {t} vs {sd:.2} at 118");
            }
        }
        // Roughness 0 is regular: negative autocorrelation two pixels out, as in Lightroom Classic
        // (−0.15); Size 0 is finer than a pixel, with negative correlation at one (−0.17)
        assert!(chart(&grain(50.0, 25.0, 0.0), 118.0, 160).1[2] < -0.08);
        assert!(chart(&grain(50.0, 0.0, 50.0), 118.0, 160).1[1] < -0.08);
    }

    #[test]
    fn grain_is_equally_strong_at_every_tone_and_response_tilts_it() {
        let sd = |gp: &GrainParams, lum: f32| stats(&patch(gp, 160, lum)).1.sqrt();
        let gp = GrainParams::new(&grain(50.0, 30.0, 50.0), 6000.0).unwrap();
        let (dark, mid, light) = (sd(&gp, 0.05), sd(&gp, 0.5), sd(&gp, 0.95));
        assert!((dark / mid - 1.0).abs() < 0.08 && (light / mid - 1.0).abs() < 0.08, "{dark} {mid} {light}");
        let shadows = GrainParams::new(&Grain { response: -100.0, ..grain(50.0, 30.0, 50.0) }, 6000.0).unwrap();
        let highlights = GrainParams::new(&Grain { response: 100.0, ..grain(50.0, 30.0, 50.0) }, 6000.0).unwrap();
        assert!(sd(&shadows, 0.15) > 1.5 * sd(&shadows, 0.85), "negative response favours the shadows");
        assert!(sd(&highlights, 0.85) > 1.5 * sd(&highlights, 0.15), "positive response favours the highlights");
        // Amount 50 at Size 30 at the reference resolution: about 0.03 (8 levels of 255)
        assert!((0.025..0.035).contains(&mid), "mid-grey sd {mid}");
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
        // (the shape alone: Roughness also scales the grains and adds a halo)
        let shaped = |shape: f64| {
            let look = GrainLook { shape, halo: 0.0, ..GrainLook::new(base.size, 50.0) };
            field(&GrainParams::with_look(&base, 6000.0, &look).unwrap(), 150, 0.45, 0.5, base.seed)
        };
        let (round, rough) = (edges(&shaped(0.0)), edges(&shaped(1.0)));
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
