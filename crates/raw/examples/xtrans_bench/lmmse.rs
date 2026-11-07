//! Linear minimum-mean-square-error demosaicing: per pattern phase, pixel class and colour, a fixed linear filter
//! fitted by (ridge) least squares on training images.
//!
//! - Stage 1: features are the (2r+1)² window of the white-balanced mosaic; outputs the missing colours.
//! - Stage 2: features are the mosaic window plus the (2q+1)² window of a full green plane (stage 1's); outputs
//!   all three colours (green refined, red and blue from the mosaic guided by green).
//! - Production (`positions`): stage 1, green only, one filter per red/blue pattern position up to the pattern's
//!   (3, 3) shift and class, constrained to reproduce flat colour exactly — what `crates/raw/src/demosaic/
//!   xtrans_ri.rs` runs, written out by [`write_rust`].

use super::proto::{self, M};
use lightcraft_raw::{Cfa, Rgb32f};
use rayon::prelude::*;
use std::fmt::Write as _;

/// How pixels are split into classes, each with its own filters.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Classifier {
    None,
    /// The direction of least activity (`proto::activity`, 5×5) when the least / most activity ratio is below
    /// `split` (two strengths: below `split / 2.5` is strong), else isotropic: 9 classes.
    Dir4 {
        split: f32,
    },
    /// Structure tensor of a first-pass green (`proto::green_dir`, or the 3×3 mean of the green samples when
    /// `cheap`) over a 5×5 Gaussian window, classified by [`classify`] (72 classes). Thresholds of NaN are set
    /// from training quantiles.
    Tensor {
        s: [f32; 2],
        k: [f32; 2],
        cheap: bool,
    },
}

impl Classifier {
    pub fn count(self) -> usize {
        match self {
            Classifier::None => 1,
            Classifier::Dir4 { .. } => 9,
            Classifier::Tensor { .. } => 72,
        }
    }

    /// The class of every pixel.
    pub fn classes(self, m: &M) -> Vec<u16> {
        match self {
            Classifier::None => vec![0; m.w * m.h],
            Classifier::Dir4 { split } => proto::par_plane(m.w, m.h, |x, y| dir4(m, x, y, split) as f32).into_iter().map(|v| v as u16).collect(),
            Classifier::Tensor { s, k, cheap } => {
                tensor_features(m, cheap).into_iter().map(|[a, b, c, mean]| classify(a, b, c, mean, s, k) as u16).collect()
            }
        }
    }

    /// Fill NaN thresholds from the quantiles of tensor `features` (strength 50 % / 80 %, coherence 33 % / 66 %).
    pub fn fit(self, features: &[[f32; 4]]) -> Classifier {
        let Classifier::Tensor { s, k, cheap } = self else { return self };
        let sc: Vec<(f32, f32)> = features.iter().map(|&[a, b, c, mean]| strength_coherence(a, b, c, mean)).collect();
        let q = |pick: fn(&(f32, f32)) -> f32, qs: [f32; 2]| {
            let mut v: Vec<f32> = sc.iter().map(pick).filter(|v| v.is_finite()).collect();
            v.sort_unstable_by(|a, b| a.total_cmp(b));
            qs.map(|q| v.get(((v.len() as f32 * q) as usize).min(v.len().saturating_sub(1))).copied().unwrap_or(0.0))
        };
        let s = if s[0].is_nan() { q(|p| p.0, [0.5, 0.8]) } else { s };
        let k = if k[0].is_nan() { q(|p| p.1, [0.33, 0.66]) } else { k };
        Classifier::Tensor { s, k, cheap }
    }
}

fn dir4(m: &M, x: usize, y: usize, split: f32) -> usize {
    let a = proto::DIRS.map(|(dx, dy)| proto::activity(m, x as isize, y as isize, dx, dy, 2));
    let (mut lo, mut hi, mut d) = (f32::INFINITY, 0f32, 0);
    for (k, &v) in a.iter().enumerate() {
        if v < lo {
            lo = v;
            d = k;
        }
        hi = hi.max(v);
    }
    if hi <= 1e-6 {
        return 0;
    }
    let ratio = lo / hi;
    if ratio >= split {
        0
    } else if ratio >= split / 2.5 {
        1 + d
    } else {
        5 + d
    }
}

/// Relative strength `√λ₁ / (mean + 0.02)` and coherence `(√λ₁ − √λ₂) / (√λ₁ + √λ₂)` of the tensor
/// `[[a, c], [c, b]]`. Must match `crates/raw/src/demosaic/xtrans_ri.rs`.
fn strength_coherence(a: f32, b: f32, c: f32, mean: f32) -> (f32, f32) {
    let half = 0.5 * (a - b);
    let root = (half * half + c * c).sqrt();
    let l1 = 0.5 * (a + b) + root;
    let l2 = (0.5 * (a + b) - root).max(0.0);
    let (s1, s2) = (l1.max(0.0).sqrt(), l2.sqrt());
    (s1 / (mean.max(0.0) + 0.02), (s1 - s2) / (s1 + s2 + 1e-9))
}

/// The class (`0..72`) of a pixel: gradient angle (8 bins of π / 8: the octant of the doubled angle) × strength
/// (split at `s`) × coherence (split at `k`). Must match `classify` in `crates/raw/src/demosaic/xtrans_ri.rs`.
pub fn classify(a: f32, b: f32, c: f32, mean: f32, s: [f32; 2], k: [f32; 2]) -> usize {
    let (u, v) = (a - b, 2.0 * c);
    let octant = if v >= 0.0 {
        if u >= 0.0 {
            usize::from(v > u)
        } else if v >= -u {
            2
        } else {
            3
        }
    } else if u < 0.0 {
        if -u >= -v { 4 } else { 5 }
    } else if -v >= u {
        6
    } else {
        7
    };
    let (strength, coherence) = strength_coherence(a, b, c, mean);
    let sb = usize::from(strength >= s[0]) + usize::from(strength >= s[1]);
    let kb = usize::from(coherence >= k[0]) + usize::from(coherence >= k[1]);
    (octant * 3 + sb) * 3 + kb
}

/// Per pixel: the structure tensor `(a, b, c)` of a first-pass green and the green's mean, over a 5×5 Gaussian
/// window (σ = 1.2).
pub fn tensor_features(m: &M, cheap: bool) -> Vec<[f32; 4]> {
    let g = if cheap { proto::green_mean(m) } else { proto::green_dir(m, 1e-3) };
    let (w, h) = (m.w, m.h);
    let at = |x: isize, y: isize| g[m.i(x, y)];
    let grad: Vec<[f32; 3]> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            let gx = 0.5 * (at(x + 1, y) - at(x - 1, y));
            let gy = 0.5 * (at(x, y + 1) - at(x, y - 1));
            [gx * gx, gy * gy, gx * gy]
        })
        .collect();
    let wts: Vec<f32> = (-2..=2).map(|d: i32| (-(d * d) as f32 / (2.0 * 1.44)).exp()).collect();
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            let (mut a, mut b, mut c, mut mean, mut ws) = (0f32, 0f32, 0f32, 0f32, 0f32);
            for dy in -2..=2isize {
                for dx in -2..=2isize {
                    let wt = wts[(dy + 2) as usize] * wts[(dx + 2) as usize];
                    let j = m.i(x + dx, y + dy);
                    a += wt * grad[j][0];
                    b += wt * grad[j][1];
                    c += wt * grad[j][2];
                    mean += wt * g[j];
                    ws += wt;
                }
            }
            [a / ws, b / ws, c / ws, mean / ws]
        })
        .collect()
}

/// What a filter set reads: the mosaic window of radius `r`, plus (stage 2) a green window of radius `q`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Layout {
    pub r: isize,
    /// Green window radius; `None` for stage 1.
    pub q: Option<isize>,
}

impl Layout {
    pub fn len(self) -> usize {
        let side = |r: isize| ((2 * r + 1) * (2 * r + 1)) as usize;
        side(self.r) + self.q.map_or(0, side)
    }

    /// The features of pixel (x, y) into `f` (`f.len() == self.len()`).
    #[inline]
    pub fn features(self, m: &M, g: Option<&[f32]>, x: isize, y: isize, f: &mut [f32]) {
        let mut k = 0;
        for dy in -self.r..=self.r {
            for dx in -self.r..=self.r {
                f[k] = m.at(x + dx, y + dy);
                k += 1;
            }
        }
        if let (Some(q), Some(g)) = (self.q, g) {
            for dy in -q..=q {
                for dx in -q..=q {
                    f[k] = g[m.i(x + dx, y + dy)];
                    k += 1;
                }
            }
        }
    }
}

/// Filter slots of the pattern: per 6×6 phase, or (production) per red/blue position up to the (3, 3) shift.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Slots {
    Phase,
    Positions,
}

/// The canonical X-Trans pattern's red/blue positions up to its (3, 3) shift: per phase, the position or None for
/// green. Must match `Pattern::canonical` in `crates/raw/src/demosaic/xtrans_ri.rs`.
pub fn positions() -> [Option<u8>; 36] {
    let canon = Cfa::xtrans();
    let mut slot = [None; 36];
    let mut next = 0u8;
    for i in 0..36 {
        if canon.color_at(i % 6, i / 6) == 1 || slot[i].is_some() {
            continue;
        }
        let twin = ((i / 6 + 3) % 6) * 6 + (i % 6 + 3) % 6;
        slot[i] = Some(next);
        slot[twin] = Some(next);
        next += 1;
    }
    slot
}

impl Slots {
    fn count(self) -> usize {
        match self {
            Slots::Phase => 36,
            Slots::Positions => 8,
        }
    }
    /// The slot of pixel (x, y) of the canonical pattern; None for a pixel the set doesn't filter.
    fn of(self, x: usize, y: usize, pos: &[Option<u8>; 36]) -> Option<usize> {
        let ph = (y % 6) * 6 + x % 6;
        match self {
            Slots::Phase => Some(ph),
            Slots::Positions => pos[ph].map(usize::from),
        }
    }
}

pub struct Filters {
    pub layout: Layout,
    pub classifier: Classifier,
    pub slots: Slots,
    /// `[slot × classes + class][colour]` → taps; empty when untrained (stage 1: the pixel's own colour).
    pub taps: Vec<[Vec<f32>; 3]>,
}

/// Normal equations of one filter: packed upper triangle of XᵀX and Xᵀy.
#[derive(Clone)]
struct Normal {
    ata: Vec<f64>,
    aty: Vec<f64>,
    count: u64,
}

impl Normal {
    fn add(&mut self, other: &Normal) {
        self.ata.iter_mut().zip(&other.ata).for_each(|(p, q)| *p += q);
        self.aty.iter_mut().zip(&other.aty).for_each(|(p, q)| *p += q);
        self.count += other.count;
    }
}

pub struct Trainer {
    layout: Layout,
    n: usize,
    classifier: Classifier,
    slots: Slots,
    pos: [Option<u8>; 36],
    /// `[slot × classes + class][colour]`, allocated on first use.
    acc: Vec<[Option<Normal>; 3]>,
}

impl Trainer {
    pub fn new(layout: Layout, classifier: Classifier, slots: Slots) -> Trainer {
        let k = slots.count() * classifier.count();
        Trainer { layout, n: layout.len(), classifier, slots, pos: positions(), acc: (0..k).map(|_| [None, None, None]).collect() }
    }

    /// Add every interior pixel of `m` (balanced mosaic, canonical pattern) with its balanced truth; `g` is the
    /// green plane stage 2 reads. Stage 1 learns the missing colours (production: green), stage 2 all three.
    pub fn add(&mut self, m: &M, g: Option<&[f32]>, truth: &Rgb32f) {
        let n = self.n;
        let edge = self.layout.r.max(self.layout.q.unwrap_or(0)) as usize;
        let classes = self.classifier.classes(m);
        let mut f32s = vec![0f32; n];
        let mut f = vec![0f64; n];
        for y in edge..m.h - edge {
            for x in edge..m.w - edge {
                let Some(slot) = self.slots.of(x, y, &self.pos) else { continue };
                let t = truth.get(x, y);
                if !t.iter().all(|v| v.is_finite()) {
                    continue;
                }
                self.layout.features(m, g, x as isize, y as isize, &mut f32s);
                // (NaN marks clipped truth: skip windows that touch it)
                if !f32s.iter().all(|v| v.is_finite()) {
                    continue;
                }
                f.iter_mut().zip(&f32s).for_each(|(d, &s)| *d = s as f64);
                let own = m.c[y * m.w + x] as usize;
                let slot = slot * self.classifier.count() + classes[y * m.w + x] as usize;
                for c in 0..3 {
                    let skip = match self.slots {
                        Slots::Positions => c != 1,
                        Slots::Phase => c == own && self.layout.q.is_none(),
                    };
                    if skip {
                        continue;
                    }
                    let ne = self.acc[slot][c].get_or_insert_with(|| Normal { ata: vec![0.0; n * (n + 1) / 2], aty: vec![0.0; n], count: 0 });
                    let tc = t[c] as f64;
                    ne.count += 1;
                    let mut p = 0;
                    for i in 0..n {
                        let fi = f[i];
                        ne.aty[i] += fi * tc;
                        for (a, fj) in ne.ata[p..p + n - i].iter_mut().zip(&f[i..]) {
                            *a += fi * fj;
                        }
                        p += n - i;
                    }
                }
            }
        }
    }

    /// The sum of two trainers' statistics.
    pub fn merge(mut self, other: Trainer) -> Trainer {
        for (a, b) in self.acc.iter_mut().zip(other.acc) {
            for (x, y) in a.iter_mut().zip(b) {
                match (x.as_mut(), y) {
                    (Some(x), Some(y)) => x.add(&y),
                    (None, Some(y)) => *x = Some(y),
                    _ => {}
                }
            }
        }
        self
    }

    /// Solve every filter; classes with fewer than `min` samples fall back to the slot's pooled filter.
    /// Production filters are constrained to reproduce flat colour: their taps on each colour sum to 1 for green
    /// and 0 for red and blue.
    pub fn solve(&self, ridge: f64, min: u64) -> Filters {
        let n = self.n;
        let k = self.classifier.count();
        let canon = Cfa::xtrans();
        // per slot: the colour of every tap (production; from a phase of that slot)
        let tap_colours: Vec<Vec<u8>> = (0..self.slots.count())
            .map(|slot| {
                let ph = (0..36).find(|&ph| self.slots.of(ph % 6, ph / 6, &self.pos) == Some(slot)).unwrap_or(0);
                let r = self.layout.r;
                (-r..=r)
                    .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
                    .map(|(dx, dy)| canon.color_at((ph as isize % 6 + dx).rem_euclid(6) as usize, (ph as isize / 6 + dy).rem_euclid(6) as usize))
                    .collect()
            })
            .collect();
        let pooled: Vec<[Option<Normal>; 3]> = (0..self.slots.count())
            .map(|sl| {
                std::array::from_fn(|c| {
                    let mut acc: Option<Normal> = None;
                    for cls in 0..k {
                        if let Some(ne) = &self.acc[sl * k + cls][c] {
                            match acc.as_mut() {
                                None => acc = Some(ne.clone()),
                                Some(a) => a.add(ne),
                            }
                        }
                    }
                    acc
                })
            })
            .collect();
        let taps = (0..self.slots.count() * k)
            .into_par_iter()
            .map(|slot| {
                std::array::from_fn(|c| {
                    let ne = match &self.acc[slot][c] {
                        Some(ne) if ne.count >= min => ne,
                        _ => match &pooled[slot / k][c] {
                            Some(ne) => ne,
                            None => return Vec::new(),
                        },
                    };
                    // unpack + ridge scaled by the mean diagonal
                    let mut a = vec![0f64; n * n];
                    let mut tr = 0.0;
                    let mut p = 0;
                    for i in 0..n {
                        for j in i..n {
                            a[i * n + j] = ne.ata[p];
                            a[j * n + i] = ne.ata[p];
                            p += 1;
                        }
                        tr += a[i * n + i];
                    }
                    for i in 0..n {
                        a[i * n + i] += ridge * tr / n as f64;
                    }
                    cholesky(&mut a, n);
                    let w0 = cholesky_solve(&a, &ne.aty, n);
                    let w = if self.slots == Slots::Positions { constrain(&a, &w0, &tap_colours[slot / k], c as u8, n) } else { w0 };
                    w.iter().map(|&v| v as f32).collect()
                })
            })
            .collect();
        Filters { layout: self.layout, classifier: self.classifier, slots: self.slots, taps }
    }
}

/// In place: the lower-triangular Cholesky factor of the symmetric positive definite `a`.
fn cholesky(a: &mut [f64], n: usize) {
    for j in 0..n {
        let mut d = a[j * n + j];
        for k in 0..j {
            d -= a[j * n + k] * a[j * n + k];
        }
        let d = d.max(1e-30).sqrt();
        a[j * n + j] = d;
        for i in j + 1..n {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= a[i * n + k] * a[j * n + k];
            }
            a[i * n + j] = s / d;
        }
    }
}

/// Solve `A x = b` from `A`'s Cholesky factor `l`.
fn cholesky_solve(l: &[f64], b: &[f64], n: usize) -> Vec<f64> {
    let mut y = vec![0f64; n];
    for i in 0..n {
        let mut s = b[i];
        for k in 0..i {
            s -= l[i * n + k] * y[k];
        }
        y[i] = s / l[i * n + i];
    }
    let mut x = vec![0f64; n];
    for i in (0..n).rev() {
        let mut s = y[i];
        for k in i + 1..n {
            s -= l[k * n + i] * x[k];
        }
        x[i] = s / l[i * n + i];
    }
    x
}

/// The least-squares solution `w0` (normal matrix with Cholesky factor `l`) moved onto the constraints
/// "taps of colour k sum to 1 if k == out else 0": `w = w0 − A⁻¹Cᵀ (C A⁻¹ Cᵀ)⁻¹ (C w0 − d)`.
fn constrain(l: &[f64], w0: &[f64], colours: &[u8], out: u8, n: usize) -> Vec<f64> {
    // constraint rows present in the window
    let rows: Vec<u8> = (0..3u8).filter(|k| colours.contains(k)).collect();
    let cvec = |k: u8| -> Vec<f64> { colours.iter().map(|&c| f64::from(u8::from(c == k))).collect() };
    let ainv_ct: Vec<Vec<f64>> = rows.iter().map(|&k| cholesky_solve(l, &cvec(k), n)).collect();
    let r = rows.len();
    // M = C A⁻¹ Cᵀ (r × r), e = C w0 − d
    let mut mm = vec![0f64; r * r];
    let mut e = vec![0f64; r];
    for (i, &ki) in rows.iter().enumerate() {
        let ci = cvec(ki);
        for j in 0..r {
            mm[i * r + j] = ci.iter().zip(&ainv_ct[j]).map(|(a, b)| a * b).sum();
        }
        e[i] = ci.iter().zip(w0).map(|(a, b)| a * b).sum::<f64>() - f64::from(u8::from(ki == out));
    }
    cholesky(&mut mm, r);
    let lambda = cholesky_solve(&mm, &e, r);
    let mut w = w0.to_vec();
    for (j, lj) in lambda.iter().enumerate() {
        for (wi, ai) in w.iter_mut().zip(&ainv_ct[j]) {
            *wi -= lj * ai;
        }
    }
    w
}

/// Apply the filters (balanced in, balanced out). Stage 2 needs `g`. Native samples are kept; colours a set
/// doesn't produce are the sample itself.
pub fn apply(m: &M, g: Option<&[f32]>, f: &Filters) -> Rgb32f {
    let classes = f.classifier.classes(m);
    let n = f.layout.len();
    let pos = positions();
    proto::par_image(m.w, m.h, |x, y| {
        let i = y * m.w + x;
        let own = m.c[i] as usize;
        let mut px = [m.v[i]; 3];
        let Some(slot) = f.slots.of(x, y, &pos) else { return px };
        let slot = slot * f.classifier.count() + classes[i] as usize;
        let mut feat = vec![0f32; n];
        f.layout.features(m, g, x as isize, y as isize, &mut feat);
        for (c, taps) in f.taps[slot].iter().enumerate() {
            if c != own && !taps.is_empty() {
                px[c] = taps.iter().zip(&feat).map(|(a, b)| a * b).sum();
            }
        }
        px
    })
}

/// Write production filters as the Rust source `crates/raw/src/demosaic/xtrans_taps.rs` reads.
pub fn write_rust(f: &Filters, path: &std::path::Path, provenance: &str) -> std::io::Result<()> {
    let Classifier::Tensor { s, k, cheap: false } = f.classifier else {
        return Err(std::io::Error::other("production filters use the full tensor classifier"));
    };
    if f.slots != Slots::Positions || f.layout != (Layout { r: 3, q: None }) {
        return Err(std::io::Error::other("production filters are 7×7, per red/blue position"));
    }
    let mut out = String::new();
    let _ = writeln!(out, "//! Green filters of the X-Trans demosaic ([`super::xtrans_ri`]). Generated, do not edit:");
    let _ = writeln!(out, "//! `{provenance}`.");
    let _ = writeln!(out, "//!");
    let _ = writeln!(out, "//! Least-squares fits on ground truth binned from CC0 X-Trans raws (raw.pixls.us): per red/blue pattern");
    let _ = writeln!(out, "//! position (8) and class (72), 49 taps (7 × 7, row-major).\n");
    let _ = writeln!(out, "/// Class thresholds of relative gradient strength.");
    let _ = writeln!(out, "pub(super) const STRENGTH: [f32; 2] = [{:?}, {:?}];", s[0], s[1]);
    let _ = writeln!(out, "/// Class thresholds of coherence.");
    let _ = writeln!(out, "pub(super) const COHERENCE: [f32; 2] = [{:?}, {:?}];", k[0], k[1]);
    let total = f.taps.len() * 49;
    let _ = writeln!(out, "/// `[position][class][tap]`.");
    let _ = writeln!(out, "#[rustfmt::skip]");
    let _ = writeln!(out, "pub(super) static TAPS: [f32; {total}] = [");
    for filt in &f.taps {
        let taps = &filt[1];
        if taps.len() != 49 {
            return Err(std::io::Error::other("an untrained green filter"));
        }
        for row in taps.chunks(7) {
            let line: Vec<String> = row.iter().map(|v| format!("{v:?}")).collect();
            let _ = writeln!(out, "    {},", line.join(", "));
        }
    }
    let _ = writeln!(out, "];");
    std::fs::write(path, out)
}

/// The 8 rotations and reflections of an image (training augmentation).
pub fn dihedral(img: &Rgb32f, k: usize) -> Rgb32f {
    let (w, h) = (img.width, img.height);
    let swap = k & 4 != 0;
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    Rgb32f::from_fn(ow, oh, |x, y| {
        let (mut sx, mut sy) = if swap { (y, x) } else { (x, y) };
        if k & 1 != 0 {
            sx = w - 1 - sx;
        }
        if k & 2 != 0 {
            sy = h - 1 - sy;
        }
        img.get(sx, sy)
    })
}
