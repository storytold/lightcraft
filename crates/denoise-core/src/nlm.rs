//! The built-in denoiser's filter: variance stabilisation and a non-local-means average whose weights come from all
//! four colour planes of a tile at once, so the planes are cleaned together instead of each on its own.
//!
//! Noise in a raw file has variance `a · signal + b`. The generalised Anscombe transform ([`Stabiliser`]) turns that
//! into noise of variance 1 everywhere, so one threshold fits every brightness. The four planes are then rotated
//! (a Hadamard transform, which keeps unit noise) into a brightness plane and three colour-difference planes.
//! Each cell becomes the average of the cells around it whose patch, over all four planes, looks like its own
//! within the noise, in two passes: the second finds matches on the first pass's much cleaner picture, so it can
//! look further and keep finer detail.

use crate::bayer::reflect;

/// What the filter may be tuned by (the defaults are what the built-in denoiser uses).
///
/// How strongly a photo is cleaned follows its noise, measured as the standard deviation at mid-grey (0.18, white = 1):
/// below `light` it is cleaned gently and in one pass, above `heavy` as hard as the filter goes, and in between smoothly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// Search window radius, in cells, of the first pass.
    pub search: usize,
    /// Search window radius of the second pass, which finds matches on the first pass's cleaner picture; 0 skips it.
    pub search2: usize,
    /// How far above the noise two patches may differ and still be averaged in the first pass (larger blurs more),
    /// for a light and for a heavily noisy photo.
    pub h_light: f32,
    pub h_heavy: f32,
    /// The same for the second pass, which is blended in as the noise grows.
    pub h2: f32,
    /// Noise at mid-grey where the stronger settings start and where they are in full.
    pub light: f32,
    pub heavy: f32,
    /// Search radius and threshold of the colour-difference planes' own pass, guided by the cleaned brightness
    /// plane (colour noise is the cheapest to remove without losing texture); radius 0 skips it.
    pub search_c: usize,
    pub h_c: f32,
    /// Patch radius (a patch is `2 · half + 1` cells on a side) of the first and of the later passes.
    pub half: usize,
    pub half2: usize,
    /// How much looser (a multiple of the threshold) the average is that cells with few matching patches lean on in
    /// the first pass; 0 turns it off.
    pub loose: f32,
    /// The matched weight below which they lean on it, in a heavily noisy photo (scaled down with the noise).
    pub few: f32,
    /// Block-matching collaborative filtering ([`crate::bm3d`]) is what cleans the planes; with `None` the non-local-means
    /// averages above do (their settings apply only then).
    pub bm3d: Option<crate::bm3d::Params>,
}

impl Default for Tuning {
    /// The built-in denoiser's tuning: block matching on, and the non-local-means settings it falls back to when that is off.
    fn default() -> Self {
        Tuning {
            search: 3,
            search2: 5,
            h_light: 0.3,
            h_heavy: 0.45,
            h2: 0.1,
            light: 0.03,
            heavy: 0.14,
            search_c: 7,
            h_c: 1.0,
            half: 1,
            half2: 1,
            loose: 4.0,
            few: 0.6,
            bm3d: Some(crate::bm3d::Params::default()),
        }
    }
}

impl Tuning {
    /// 0 for a lightly noisy photo up to 1 for a heavily noisy one.
    pub fn strength(&self, noise: f32) -> f32 {
        let span = (self.heavy - self.light).max(1e-6);
        let x = ((noise - self.light) / span).clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    }
}

/// The generalised Anscombe transform for noise of variance `a · v + b`.
#[derive(Clone, Copy, Debug)]
pub struct Stabiliser {
    a: f32,
    b: f32,
}

/// Below this the noise has no signal-dependent part worth modelling.
const MIN_GAIN: f32 = 1e-6;

impl Stabiliser {
    /// The transform for noise of variance `a · signal + b`; a gain or offset that is not a number counts as none.
    pub fn new(a: f32, b: f32) -> Self {
        Stabiliser { a: if a.is_finite() { a.max(0.0) } else { 0.0 }, b: if b.is_finite() { b.max(1e-12) } else { 1e-12 } }
    }

    /// A value as a signal with unit noise deviation (a plain scaling when the noise has no signal-dependent part).
    pub fn forward(self, v: f32) -> f32 {
        if self.a < MIN_GAIN {
            return v / self.b.sqrt();
        }
        let x = (v + self.b / self.a) / self.a;
        2.0 * (x + 0.375).max(0.0).sqrt()
    }

    /// The inverse, with the correction for the bias a plain inverse has on averaged data.
    pub fn inverse(self, y: f32) -> f32 {
        if self.a < MIN_GAIN {
            return y * self.b.sqrt();
        }
        let x = if y >= 1.5 {
            let (i1, i2) = (1.0 / y, 1.0 / (y * y));
            y * y / 4.0 + 0.25 * 1.5f32.sqrt() * i1 - 11.0 / 8.0 * i2 + 5.0 / 8.0 * 1.5f32.sqrt() * i2 * i1 - 0.125
        } else {
            y * y / 4.0 - 0.375
        };
        self.a * x - self.b / self.a
    }
}

/// The orthonormal 4 × 4 Hadamard transform across four planes (its own inverse).
pub fn hadamard(p: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let n = p.first().map_or(0, Vec::len);
    let mut out = vec![vec![0f32; n]; 4];
    for i in 0..n {
        let g = |c: usize| p.get(c).and_then(|v| v.get(i)).copied().unwrap_or(0.0);
        let (r, g1, g2, b) = (g(0), g(1), g(2), g(3));
        let vals = [(r + g1 + g2 + b) / 2.0, (r - g1 + g2 - b) / 2.0, (r + g1 - g2 - b) / 2.0, (r - g1 - g2 + b) / 2.0];
        for (c, v) in vals.into_iter().enumerate() {
            if let Some(o) = out.get_mut(c).and_then(|o| o.get_mut(i)) {
                *o = v;
            }
        }
    }
    out
}

/// A plane with `pad` cells mirrored round every edge (`t + 2 · pad` on a side).
fn padded(src: &[f32], t: usize, pad: usize) -> Vec<f32> {
    let wd = t + 2 * pad;
    let mut out = vec![0f32; wd * wd];
    for y in 0..wd {
        let sy = reflect(y as isize - pad as isize, t);
        for x in 0..wd {
            let sx = reflect(x as isize - pad as isize, t);
            if let (Some(o), Some(v)) = (out.get_mut(y * wd + x), src.get(sy * t + sx)) {
                *o = *v;
            }
        }
    }
    out
}

/// What one [`pass`] is run with.
#[derive(Clone, Copy)]
struct Pass {
    /// Search window radius and patch radius, in cells.
    radius: usize,
    half: usize,
    /// The threshold and the floor of the weights (see [`pass`]).
    h: f32,
    floor: f32,
    /// A second, looser average (threshold `h · loose`) that cells with few matches lean on, so a cell no patch
    /// resembles is not left as it was; 0 turns it off.
    loose: f32,
    /// Matched weight below which a cell starts to lean on that looser average.
    few: f32,
}

/// One pass: every cell becomes the weighted average of the cells within `radius` whose patch (`2 · half + 1` cells
/// on a side), over all planes, is like its own. `guide` holds the planes the patches are compared on, `data` the
/// planes that are averaged (the same ones in the first pass). Weights are `exp(-(excess) / h)`, `excess` being how
/// far the mean squared difference of the patches is above `floor` (what two noisy views of one signal give).
fn pass(guide: &[Vec<f32>], data: &[Vec<f32>], t: usize, p: &Pass) -> Vec<Vec<f32>> {
    let Pass { radius, half, h, floor, loose, few } = *p;
    let pad = radius + half;
    let wd = t + 2 * pad;
    let guide: Vec<Vec<f32>> = guide.iter().map(|p| padded(p, t, pad)).collect();
    let data: Vec<Vec<f32>> = data.iter().map(|p| padded(p, t, pad)).collect();
    let (n, region, span) = (t * t, t + 2 * half, 2 * half + 1);
    let planes = data.len().max(1);
    let mut num = vec![vec![0f32; n]; planes];
    let mut den = vec![0f32; n];
    let mut num_l = vec![vec![0f32; n]; if loose > 0.0 { planes } else { 0 }];
    let mut den_l = vec![0f32; if loose > 0.0 { n } else { 0 }];
    let (mut d, mut rows) = (vec![0f32; region * region], vec![0f32; region * t]);
    let norm = 1.0 / ((span * span) as f32 * guide.len().max(1) as f32);
    let r = radius as isize;
    for oy in -r..=r {
        for ox in -r..=r {
            if ox == 0 && oy == 0 {
                continue;
            }
            let shift = oy * wd as isize + ox;
            // squared differences over the planes, for the cells and the ring round them the patch reaches
            for j in 0..region {
                let base = (j + pad - half) * wd + pad - half;
                for i in 0..region {
                    let pos = base + i;
                    let other = (pos as isize + shift) as usize;
                    let mut s = 0f32;
                    for p in &guide {
                        let (a, b) = (p.get(pos).copied().unwrap_or(0.0), p.get(other).copied().unwrap_or(0.0));
                        s += (a - b) * (a - b);
                    }
                    if let Some(v) = d.get_mut(j * region + i) {
                        *v = s;
                    }
                }
            }
            // the sum over each cell's patch: along rows, then down columns
            for j in 0..region {
                for x in 0..t {
                    let k = j * region + x;
                    let s: f32 = (0..span).map(|i| d.get(k + i).copied().unwrap_or(0.0)).sum();
                    if let Some(v) = rows.get_mut(j * t + x) {
                        *v = s;
                    }
                }
            }
            for y in 0..t {
                for x in 0..t {
                    let k = y * t + x;
                    let patch = (0..span).map(|j| rows.get(k + j * t).copied().unwrap_or(0.0)).sum::<f32>() * norm;
                    let excess = (patch - floor).max(0.0);
                    let wide = if loose > 0.0 { loose } else { 1.0 };
                    // beyond this the weight is below 5e-5: not worth the exponential and the sums
                    if excess > 10.0 * h * wide {
                        continue;
                    }
                    let pos = (y + pad) * wd + x + pad;
                    let other = (pos as isize + shift) as usize;
                    if excess <= 10.0 * h {
                        let w = (-excess / h).exp();
                        if let Some(dn) = den.get_mut(k) {
                            *dn += w;
                        }
                        for (nu, p) in num.iter_mut().zip(&data) {
                            if let (Some(nu), Some(v)) = (nu.get_mut(k), p.get(other)) {
                                *nu += w * v;
                            }
                        }
                    }
                    if loose > 0.0 {
                        let w = (-excess / (h * loose)).exp();
                        if let Some(dn) = den_l.get_mut(k) {
                            *dn += w;
                        }
                        for (nu, p) in num_l.iter_mut().zip(&data) {
                            if let (Some(nu), Some(v)) = (nu.get_mut(k), p.get(other)) {
                                *nu += w * v;
                            }
                        }
                    }
                }
            }
        }
    }
    // the cell itself counts as much as a perfect match
    let mut out = vec![vec![0f32; n]; planes];
    for (c, (o, (nu, p))) in out.iter_mut().zip(num.iter().zip(&data)).enumerate() {
        for k in 0..n {
            let own = p.get((k / t + pad) * wd + k % t + pad).copied().unwrap_or(0.0);
            if let (Some(o), Some(nu), Some(dn)) = (o.get_mut(k), nu.get(k), den.get(k)) {
                let tight = (nu + own) / (dn + 1.0);
                *o = match (num_l.get(c).and_then(|v| v.get(k)), den_l.get(k)) {
                    (Some(nl), Some(dl)) => {
                        let loose = (nl + own) / (dl + 1.0);
                        let lean = if few > 0.0 { few / (dn + few) } else { 0.0 };
                        tight + (loose - tight) * lean
                    }
                    _ => tight,
                };
            }
        }
    }
    out
}

/// Denoise four unit-noise planes of `t × t` cells (see the module description); `noise` is the photo's noise at
/// mid-grey, which sets how strongly (see [`Tuning`]).
pub fn filter(planes: &[Vec<f32>], t: usize, tune: &Tuning, noise: f32) -> Vec<Vec<f32>> {
    let y = hadamard(planes);
    let k = tune.strength(if noise.is_finite() { noise } else { 0.0 });
    // two noisy views of one signal differ by 2 in mean square: the floor the excess is counted from
    let h1 = tune.h_light + (tune.h_heavy - tune.h_light) * k;
    let first = pass(&y, &y, t, &Pass { radius: tune.search, half: tune.half, h: h1, floor: 2.0, loose: tune.loose, few: tune.few * k });
    if tune.search2 == 0 || k <= 0.0 {
        return hadamard(&first);
    }
    // the first pass's picture still carries a little noise (of unknown size), so the floor is 0 and the threshold
    // alone decides
    let second = pass(&first, &y, t, &Pass { radius: tune.search2, half: tune.half2, h: tune.h2, floor: 0.0, loose: 0.0, few: 0.0 });
    let mut blended: Vec<Vec<f32>> = first.iter().zip(&second).map(|(f, s)| f.iter().zip(s).map(|(f, s)| f + (s - f) * k).collect()).collect();
    if tune.search_c > 0 {
        // matches found on the cleaned brightness plane alone, so colour is only averaged across like brightness
        let guide = blended.get(..1).unwrap_or_default();
        let colour = pass(
            guide,
            y.get(1..).unwrap_or_default(),
            t,
            &Pass { radius: tune.search_c, half: tune.half2, h: tune.h_c, floor: 0.0, loose: 0.0, few: 0.0 },
        );
        for (b, c) in blended.iter_mut().skip(1).zip(&colour) {
            for (b, c) in b.iter_mut().zip(c) {
                *b += (c - *b) * k;
            }
        }
    }
    hadamard(&blended)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transform is undone by its inverse within a little, a step of one noise deviation is a step of about 1
    /// whatever the brightness, and with no signal-dependent noise it is a plain scaling.
    #[test]
    fn the_stabiliser_round_trips_and_gives_unit_noise() {
        let s = Stabiliser::new(0.01, 0.0005);
        for v in [0.0f32, 0.01, 0.1, 0.5, 0.9] {
            let back = s.inverse(s.forward(v));
            assert!((back - v).abs() < 0.005 + 0.03 * v, "{v} -> {back}");
        }
        // a step of one standard deviation is a step of about 1 whatever the brightness
        for v in [0.05f32, 0.2, 0.6] {
            let sigma = (0.01 * v + 0.0005).sqrt();
            let step = s.forward(v + sigma) - s.forward(v);
            assert!((step - 1.0).abs() < 0.15, "at {v}: {step}");
        }
        let flat = Stabiliser::new(0.0, 4e-4);
        assert!((flat.forward(0.1) - 5.0).abs() < 1e-4 && (flat.inverse(5.0) - 0.1).abs() < 1e-6);
    }

    /// The Hadamard transform applied twice gives the planes back, and it keeps their energy.
    #[test]
    fn the_hadamard_transform_is_its_own_inverse_and_keeps_energy() {
        let planes: Vec<Vec<f32>> = (0..4).map(|c| (0..9).map(|i| (c * 9 + i) as f32 * 0.37 - 3.0).collect()).collect();
        let back = hadamard(&hadamard(&planes));
        for (a, b) in planes.iter().flatten().zip(back.iter().flatten()) {
            assert!((a - b).abs() < 1e-4);
        }
        let energy = |p: &[Vec<f32>]| p.iter().flatten().map(|v| v * v).sum::<f32>();
        assert!((energy(&planes) - energy(&hadamard(&planes))).abs() < 1e-2);
    }

    /// Four planes of unit noise (an LCG and Box-Muller, so the tests need no dependency) round `level`.
    fn noisy(t: usize, level: f32, seed: u64) -> Vec<Vec<f32>> {
        let mut state = seed;
        let mut uniform = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 40) as f32 + 0.5) / (1u64 << 24) as f32
        };
        (0..4)
            .map(|_| {
                (0..t * t)
                    .map(|_| {
                        let (u, v) = (uniform(), uniform());
                        level + (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
                    })
                    .collect()
            })
            .collect()
    }

    /// The deviation of the planes' values around `level`.
    fn spread(planes: &[Vec<f32>], level: f32) -> f32 {
        let all: Vec<f32> = planes.iter().flatten().copied().collect();
        (all.iter().map(|v| (v - level) * (v - level)).sum::<f32>() / all.len().max(1) as f32).sqrt()
    }

    /// The colour planes' own pass leaves less noise than brightness alone would, and takes unit noise well below 0.5.
    #[test]
    fn the_colour_pass_cleans_colour_planes_further_than_brightness_alone_would() {
        let (t, noise) = (40, 0.2);
        let planes = noisy(t, 5.0, 3);
        let with = filter(&planes, t, &Tuning::default(), noise);
        let without = filter(&planes, t, &Tuning { search_c: 0, ..Tuning::default() }, noise);
        // after the Hadamard rotation the colour differences are planes 1..4 of the mixed result, so compare it all
        assert!(spread(&with, 5.0) < spread(&without, 5.0), "{} vs {}", spread(&with, 5.0), spread(&without, 5.0));
        assert!(spread(&with, 5.0) < 0.5, "unit noise should drop well below 0.5: {}", spread(&with, 5.0));
    }

    /// A cell far from its surroundings, which no patch resembles, ends up nearer to them with the looser average than without it.
    #[test]
    fn a_cell_no_patch_resembles_is_not_left_as_a_speck_in_heavy_noise() {
        let (t, noise) = (40, 0.2);
        let mut planes = noisy(t, 5.0, 9);
        // one cell far from its surroundings in every plane: nothing matches it
        let at = (t / 2) * t + t / 2;
        for p in &mut planes {
            if let Some(v) = p.get_mut(at) {
                *v += 6.0;
            }
        }
        let with = filter(&planes, t, &Tuning::default(), noise);
        let without = filter(&planes, t, &Tuning { loose: 0.0, ..Tuning::default() }, noise);
        let peak = |planes: &[Vec<f32>]| planes.iter().filter_map(|p| p.get(at)).map(|v| (v - 5.0).abs()).fold(0f32, f32::max);
        assert!(peak(&with) < peak(&without), "{} vs {}", peak(&with), peak(&without));
    }

    /// Noise of 0, NaN or next to nothing, with one cell far from the rest, still gives finite values.
    #[test]
    fn a_photo_with_no_measured_noise_stays_finite_even_with_an_odd_cell() {
        let t = 24;
        let mut planes = noisy(t, 5.0, 5);
        for p in &mut planes {
            if let Some(v) = p.get_mut(t * t / 2) {
                *v += 50.0;
            }
        }
        for noise in [0.0, f32::NAN, 1e-9] {
            assert!(filter(&planes, t, &Tuning::default(), noise).iter().flatten().all(|v| v.is_finite()), "noise {noise}");
        }
    }
}
