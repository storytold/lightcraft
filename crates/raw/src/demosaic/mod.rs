//! Demosaicing: CFA mosaic → camera RGB ([`Rgb32f`]), values as normalised (white = 1.0).
//!
//! Methods (all from published descriptions, see the README):
//! - [`Method::Bilinear`] — average of same-colour neighbours (drafts / thumbnails); works for any pattern.
//! - [`Method::Ppg`] — Patterned Pixel Grouping (Chuan-kai Lin): gradient-selected green, colour-difference R/B.
//! - [`Method::Ahd`] — Adaptive Homogeneity-Directed (Hirakawa & Parks, IEEE TIP 2005): horizontal/vertical
//!   candidates chosen per pixel by CIELab homogeneity. The quality default for Bayer sensors.
//! - [`Method::Detail`] — Enhance ▸ Raw Details: Hamilton–Adams green blended by gradient weights (GBTF-style) and
//!   residual red/blue interpolated along edges (after Kiku et al. 2013); see `detail.rs`.
//! - [`Method::Rcd`] — Ratio Corrected Demosaicing (Luis Sanz Rodríguez), translated from RawTherapee; see `rcd.rs`.
//!
//! X-Trans (and other non-Bayer patterns):
//! - [`Method::Ahd`] → [`xtrans_ri`]: on the white-balanced mosaic ([`Options::balance`]), a classified linear
//!   (LMMSE) green trained on real X-Trans raws, then red and blue by residual interpolation. X-Trans in any phase,
//!   at least 24 × 24; otherwise `xtrans::directional`.
//! - [`Method::Ppg`] → `xtrans::directional`: our own edge-weighted colour-difference interpolation (faster).
//! - [`Method::Bilinear`] → bilinear.
//!
//! Every method reproduces a constant-colour mosaic exactly.

mod ahd;
mod bilinear;
mod detail;
mod ppg;
mod rcd;
mod xtrans;
mod xtrans_ri;
mod xtrans_taps;

use crate::{Cfa, Normalized, Rgb32f};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Method {
    Bilinear,
    Ppg,
    #[default]
    Ahd,
    /// Enhance ▸ Raw Details: gradient-weighted green and edge-aware residual red/blue (`detail.rs`). Slower than
    /// AHD; used for the enhanced DNG, not live previews. Non-Bayer patterns use the directional method.
    Detail,
    /// Ratio Corrected Demosaicing (`rcd.rs`, translated from RawTherapee). Non-Bayer patterns use the method AHD
    /// uses for them.
    Rcd,
}

/// Mirror-reflected access to a single-plane mosaic (reflection keeps Bayer parity: −k → k, w−1+k → w−1−k).
#[derive(Clone, Copy)]
pub(crate) struct Mosaic<'a> {
    pub w: usize,
    pub h: usize,
    pub data: &'a [f32],
    pub cfa: &'a Cfa,
}

#[inline]
pub(crate) fn reflect(i: isize, n: usize) -> usize {
    if i >= 0 && (i as usize) < n {
        return i as usize;
    }
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let mut i = i.rem_euclid(period);
    if i >= n {
        i = period - i;
    }
    i as usize
}

/// `v` clamped between `a` and `b`: `v.clamp(a.min(b), a.max(b))`, except that it returns `v` as is when both
/// bounds are NaN (NaN samples in a float raw), where `f32::clamp` panics.
#[inline]
pub(crate) fn clamp_between(v: f32, a: f32, b: f32) -> f32 {
    let (lo, hi) = (a.min(b), a.max(b));
    if lo <= hi { v.clamp(lo, hi) } else { v }
}

impl Mosaic<'_> {
    #[inline]
    pub fn get(&self, x: isize, y: isize) -> f32 {
        self.data[reflect(y, self.h) * self.w + reflect(x, self.w)]
    }
    #[inline]
    pub fn color(&self, x: isize, y: isize) -> u8 {
        self.cfa.color_at(reflect(x, self.w), reflect(y, self.h))
    }
}

/// Demosaic normalised raw data. `cpp == 3` data is returned as is; monochrome data is replicated to grey.
pub fn demosaic(n: &Normalized, method: Method) -> Rgb32f {
    demosaic_with(n, method, Options::default())
}

/// Options for [`demosaic_with`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Options {
    /// White-balance gains (camera RGB → balanced; the as-shot multipliers) for the X-Trans demosaic, which
    /// interpolates the balanced mosaic: in camera RGB, R − G and B − G change with brightness across every edge,
    /// and interpolation turns that into colour fringes. Bayer methods ignore it, as they do gains that are not
    /// finite and positive.
    pub balance: Option<[f32; 3]>,
}

/// [`demosaic`] with [`Options`].
pub fn demosaic_with(n: &Normalized, method: Method, opts: Options) -> Rgb32f {
    let (w, h) = (n.width, n.height);
    if w == 0 || h == 0 {
        return Rgb32f::new(w, h);
    }
    match (&n.cfa, n.cpp) {
        (_, 3) => Rgb32f { width: w, height: h, data: n.data.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2]]).collect() },
        // four colours develop from their first three; narrower data (refused by `RawImage::validate`, issue #1511)
        // repeats its last sample instead of reading past the pixel
        (_, cpp) if cpp != 1 => Rgb32f {
            width: w,
            height: h,
            data: n
                .data
                .chunks_exact(cpp.max(1))
                .map(|c| {
                    let s = |i: usize| c.get(i).or(c.last()).copied().unwrap_or(0.0);
                    [s(0), s(1), s(2)]
                })
                .collect(),
        },
        (None, _) => Rgb32f { width: w, height: h, data: n.data.iter().map(|&v| [v; 3]).collect() },
        (Some(cfa), _) => {
            let m = Mosaic { w, h, data: &n.data, cfa };
            if !cfa.is_bayer() {
                return match method {
                    Method::Bilinear => bilinear::bilinear(&m),
                    Method::Ppg => xtrans::directional(&m),
                    Method::Ahd | Method::Detail | Method::Rcd => xtrans_ri::demosaic(&m, opts.balance).unwrap_or_else(|| xtrans::directional(&m)),
                };
            }
            if w < 4 || h < 4 {
                return bilinear::bilinear(&m);
            }
            match method {
                Method::Bilinear => bilinear::bilinear(&m),
                Method::Ppg => ppg::ppg(&m),
                Method::Ahd => ahd::ahd(&m),
                Method::Detail => detail::detail(&m),
                Method::Rcd => rcd::rcd(&m),
            }
        }
    }
}

/// Build a mosaic from an RGB image by sampling each pixel's CFA colour (for tests and synthetic data).
pub fn mosaic_from_rgb(img: &Rgb32f, cfa: &Cfa) -> Normalized {
    let data = (0..img.height).flat_map(|y| (0..img.width).map(move |x| (x, y))).map(|(x, y)| img.get(x, y)[cfa.color_at(x, y) as usize]).collect();
    Normalized { width: img.width, height: img.height, cpp: 1, data, cfa: Some(cfa.clone()) }
}

/// Peak signal-to-noise ratio (dB, peak 1.0) between two images over an interior region (`border` pixels excluded).
pub fn psnr(a: &Rgb32f, b: &Rgb32f, border: usize) -> f64 {
    let mut se = 0.0f64;
    let mut n = 0usize;
    for y in border..a.height.saturating_sub(border) {
        for x in border..a.width.saturating_sub(border) {
            let (p, q) = (a.get(x, y), b.get(x, y));
            for c in 0..3 {
                se += ((p[c] - q[c]) as f64).powi(2);
                n += 1;
            }
        }
    }
    if n == 0 || se == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (1.0 / (se / n as f64)).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A photo-like scene: sharp luminance structure (disc, stripes, diagonal edge, fine texture) modulating a
    /// smoothly varying colour — edges are mostly luminance edges, as in natural images.
    fn edgy_scene(w: usize, h: usize) -> Rgb32f {
        Rgb32f::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32, y as f32);
            let d = ((fx - w as f32 * 0.35).powi(2) + (fy - h as f32 * 0.4).powi(2)).sqrt();
            let lum = if d < w as f32 * 0.18 {
                0.85
            } else if x > w * 2 / 3 && (y / 3) % 2 == 0 {
                0.15
            } else if fx + fy * 0.5 > w as f32 * 1.1 {
                0.7
            } else {
                0.35 + 0.1 * ((fx * 0.7).sin() * (fy * 0.9).cos())
            };
            let tint = [0.9 + 0.1 * (fx / w as f32), 0.8, 0.55 + 0.3 * (fy / h as f32)];
            [lum * tint[0], lum * tint[1], lum * tint[2]]
        })
    }

    fn run(img: &Rgb32f, cfa: &Cfa, m: Method) -> Rgb32f {
        demosaic(&mosaic_from_rgb(img, cfa), m)
    }

    #[test]
    fn constant_colour_is_exact_for_all_methods_and_phases() {
        let img = Rgb32f::filled(17, 13, [0.3, 0.55, 0.8]);
        for pat in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            let cfa = Cfa::bayer(pat).unwrap();
            for m in [Method::Bilinear, Method::Ppg, Method::Ahd, Method::Detail, Method::Rcd] {
                let out = run(&img, &cfa, m);
                for p in &out.data {
                    for c in 0..3 {
                        assert!((p[c] - img.data[0][c]).abs() < 1e-5, "{pat} {m:?} {p:?}");
                    }
                }
            }
        }
        let out = run(&Rgb32f::filled(19, 14, [0.3, 0.55, 0.8]), &Cfa::xtrans(), Method::Ahd);
        assert!(out.data.iter().all(|p| (p[0] - 0.3).abs() < 1e-5 && (p[1] - 0.55).abs() < 1e-5 && (p[2] - 0.8).abs() < 1e-5));
    }

    #[test]
    fn balance_reaches_x_trans_only_and_is_illuminant_invariant() {
        let img = edgy_scene(48, 36);
        // Bayer methods ignore it
        let bayer = mosaic_from_rgb(&img, &Cfa::bayer("RGGB").unwrap());
        for m in [Method::Bilinear, Method::Ppg, Method::Ahd, Method::Rcd] {
            let with = demosaic_with(&bayer, m, Options { balance: Some([1.9, 1.0, 1.6]) });
            assert_eq!(with.data, demosaic(&bayer, m).data, "{m:?}");
        }
        // a balanced scene seen under a coloured illuminant, demosaiced with that illuminant's gains and balanced
        // back, matches the neutral-illuminant result
        let cfa = Cfa::xtrans();
        let neutral = demosaic(&mosaic_from_rgb(&img, &cfa), Method::Ahd);
        let g = [1.15f32, 1.0, 2.8];
        let cam = Rgb32f { width: img.width, height: img.height, data: img.data.iter().map(|p| [p[0] / g[0], p[1], p[2] / g[2]]).collect() };
        let out = demosaic_with(&mosaic_from_rgb(&cam, &cfa), Method::Ahd, Options { balance: Some(g) });
        let back = Rgb32f { width: out.width, height: out.height, data: out.data.iter().map(|p| [p[0] * g[0], p[1], p[2] * g[2]]).collect() };
        assert!(psnr(&back, &neutral, 0) > 80.0);
        // invalid gains are ignored rather than poisoning the image
        let bad = Options { balance: Some([f32::NAN, 1.0, 0.0]) };
        assert!(demosaic_with(&mosaic_from_rgb(&img, &cfa), Method::Ahd, bad).data.iter().all(|p| p.iter().all(|v| v.is_finite())));
    }

    #[test]
    fn known_samples_are_preserved() {
        let img = edgy_scene(40, 32);
        let cfa = Cfa::bayer("GRBG").unwrap();
        for m in [Method::Bilinear, Method::Ppg, Method::Ahd, Method::Detail, Method::Rcd] {
            let out = run(&img, &cfa, m);
            for y in 0..32 {
                for x in 0..40 {
                    let c = cfa.color_at(x, y) as usize;
                    assert!((out.get(x, y)[c] - img.get(x, y)[c]).abs() < 1e-6, "{m:?}");
                }
            }
        }
    }

    #[test]
    fn tiny_and_degenerate_inputs() {
        let cfa = Cfa::bayer("RGGB").unwrap();
        for (w, h) in [(1, 1), (2, 2), (3, 5), (5, 3), (1, 7)] {
            let img = Rgb32f::filled(w, h, [0.5; 3]);
            for m in [Method::Bilinear, Method::Ppg, Method::Ahd, Method::Detail, Method::Rcd] {
                let out = run(&img, &cfa, m);
                assert_eq!((out.width, out.height), (w, h));
                assert!(out.data.iter().all(|p| p.iter().all(|v| v.is_finite())));
            }
        }
        let n = Normalized { width: 2, height: 1, cpp: 1, data: vec![0.2, 0.4], cfa: None };
        assert_eq!(demosaic(&n, Method::Ahd).data, vec![[0.2; 3], [0.4; 3]]);
        let n = Normalized { width: 1, height: 1, cpp: 3, data: vec![0.1, 0.2, 0.3], cfa: None };
        assert_eq!(demosaic(&n, Method::Ahd).data, vec![[0.1, 0.2, 0.3]]);
    }

    #[test]
    fn non_finite_samples_do_not_panic() {
        // regression: NaN samples (a float DNG) gave AHD and PPG NaN bounds for `f32::clamp`, which panics
        let cfa = Cfa::bayer("RGGB").unwrap();
        let mut patchy = mosaic_from_rgb(&edgy_scene(24, 20), &cfa);
        for (i, v) in patchy.data.iter_mut().enumerate() {
            let (x, y) = (i % 24, i / 24);
            if (8..12).contains(&x) && (6..10).contains(&y) {
                *v = f32::NAN;
            } else if i % 7 == 0 {
                *v = if i % 2 == 0 { f32::INFINITY } else { f32::NEG_INFINITY };
            }
        }
        let all_nan = Normalized { width: 9, height: 7, cpp: 1, data: vec![f32::NAN; 63], cfa: Some(cfa) };
        for n in [&patchy, &all_nan] {
            for m in [Method::Bilinear, Method::Ppg, Method::Ahd, Method::Detail, Method::Rcd] {
                let out = demosaic(n, m);
                assert_eq!((out.width, out.height), (n.width, n.height), "{m:?}");
            }
        }
        // without NaN bounds it is the `f32::clamp` it replaced, bit for bit (finite results stay identical)
        let vals = [f32::NEG_INFINITY, -3e38, -2.5, -0.0, 0.0, 1e-40, 0.5, 1.0, 3e38, f32::INFINITY];
        for v in vals {
            for a in vals {
                for b in vals {
                    assert_eq!(clamp_between(v, a, b).to_bits(), v.clamp(a.min(b), a.max(b)).to_bits(), "{v} {a} {b}");
                }
            }
        }
    }

    /// Mean chroma error (|(R−G) − (R−G)ₜ| + |(B−G) − (B−G)ₜ|) over the interior: false colour and zipper show here.
    fn false_colour(a: &Rgb32f, t: &Rgb32f) -> f64 {
        let mut s = 0.0f64;
        let mut n = 0usize;
        for y in 3..a.height - 3 {
            for x in 3..a.width - 3 {
                let (p, q) = (a.get(x, y), t.get(x, y));
                s += (((p[0] - p[1]) - (q[0] - q[1])).abs() + ((p[2] - p[1]) - (q[2] - q[1])).abs()) as f64;
                n += 1;
            }
        }
        s / n as f64
    }

    #[test]
    fn raw_details_beats_bilinear_on_edges_and_false_colour() {
        let img = edgy_scene(96, 80);
        for pat in ["RGGB", "GBRG"] {
            let cfa = Cfa::bayer(pat).unwrap();
            let bil = run(&img, &cfa, Method::Bilinear);
            let det = run(&img, &cfa, Method::Detail);
            let (pb, pd) = (psnr(&bil, &img, 3), psnr(&det, &img, 3));
            assert!(pd > pb + 3.0, "{pat}: detail {pd:.2} dB vs bilinear {pb:.2} dB");
            let (fb, fd) = (false_colour(&bil, &img), false_colour(&det, &img));
            assert!(fd < fb * 0.6, "{pat}: false colour {fd} vs bilinear {fb}");
            let pa = psnr(&run(&img, &cfa, Method::Ahd), &img, 3);
            assert!(pd > pa - 0.5, "{pat}: detail {pd:.2} dB vs AHD {pa:.2} dB");
        }
    }

    #[test]
    fn rcd_beats_bilinear_and_stays_near_ppg_on_edges() {
        let img = edgy_scene(96, 80);
        for pat in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            let cfa = Cfa::bayer(pat).unwrap();
            let pb = psnr(&run(&img, &cfa, Method::Bilinear), &img, 3);
            let pp = psnr(&run(&img, &cfa, Method::Ppg), &img, 3);
            let rcd = run(&img, &cfa, Method::Rcd);
            let pr = psnr(&rcd, &img, 3);
            assert!(pr > pb + 8.0 && pr > pp - 2.0, "{pat}: rcd {pr:.2} dB, bilinear {pb:.2}, ppg {pp:.2}");
            assert!(false_colour(&rcd, &img) < false_colour(&run(&img, &cfa, Method::Bilinear), &img) * 0.6, "{pat}");
        }
    }

    #[test]
    fn rcd_hostile_sizes_and_values() {
        let cfa = Cfa::bayer("BGGR").unwrap();
        // every small size, odd and even, including those below the 4 × 4 guard and thin strips past it
        for w in 1..12 {
            for h in [1usize, 2, 3, 4, 5, 7, 9] {
                let out = run(&edgy_scene(w, h), &cfa, Method::Rcd);
                assert_eq!((out.width, out.height), (w, h));
                assert!(out.data.iter().all(|p| p.iter().all(|v| v.is_finite())), "{w}x{h}");
            }
        }
        // zeros, negatives and huge values: the ratio and gradient divisions must not blow up
        for fill in [0.0f32, -1.0, 1e30, -1e30] {
            let n = Normalized { width: 16, height: 12, cpp: 1, data: vec![fill; 192], cfa: Some(cfa.clone()) };
            let out = demosaic(&n, Method::Rcd);
            assert_eq!(out.data.len(), 192);
            if fill.abs() < 2.0 {
                assert!(out.data.iter().all(|p| p.iter().all(|v| (v - fill).abs() < 1e-4)), "{fill}");
            }
        }
        let alt = Normalized { width: 16, height: 12, cpp: 1, data: (0..192).map(|i| if i % 3 == 0 { -0.5 } else { 2.0 }).collect(), cfa: Some(cfa) };
        assert!(demosaic(&alt, Method::Rcd).data.iter().all(|p| p.iter().all(|v| v.is_finite())));
    }

    #[test]
    fn reflect_indices() {
        assert_eq!(reflect(-1, 5), 1);
        assert_eq!(reflect(-2, 5), 2);
        assert_eq!(reflect(5, 5), 3);
        assert_eq!(reflect(6, 5), 2);
        assert_eq!(reflect(3, 1), 0);
        assert_eq!(reflect(-7, 2), 1);
    }
}
