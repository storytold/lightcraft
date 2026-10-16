//! Demosaicing: CFA mosaic → camera RGB ([`Rgb32f`]), values as normalised (white = 1.0).
//!
//! Methods (all from published descriptions, see the README):
//! - [`Method::Bilinear`] — average of same-colour neighbours (drafts / thumbnails); works for any pattern.
//! - [`Method::Ppg`] — Patterned Pixel Grouping (Chuan-kai Lin): gradient-selected green, colour-difference R/B.
//! - [`Method::Ahd`] — Adaptive Homogeneity-Directed (Hirakawa & Parks, IEEE TIP 2005): horizontal/vertical
//!   candidates chosen per pixel by CIELab homogeneity. The quality default for Bayer sensors.
//! - Non-Bayer patterns (X-Trans, …) always use `xtrans::directional`, our own edge-weighted colour-difference
//!   interpolation.
//!
//! [`remosaic_rggb`] goes the other way, for callers that need Bayer geometry from a sensor that has none.
//!
//! Every method reproduces a constant-colour mosaic exactly.

mod ahd;
mod bilinear;
mod ppg;
mod xtrans;

use crate::{Cfa, Normalized, Rgb32f};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Method {
    Bilinear,
    Ppg,
    #[default]
    Ahd,
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
                    _ => xtrans::directional(&m),
                };
            }
            if w < 4 || h < 4 {
                return bilinear::bilinear(&m);
            }
            match method {
                Method::Bilinear => bilinear::bilinear(&m),
                Method::Ppg => ppg::ppg(&m),
                Method::Ahd => ahd::ahd(&m),
            }
        }
    }
}

/// A non-Bayer mosaic (Fujifilm X-Trans, …) rebuilt as an RGGB mosaic of the same size, so that tools
/// written around Bayer geometry can read a sensor they were not written for: the AI denoiser, whose
/// model contract is four RGGB planes. The mosaic is interpolated as [`demosaic`] would and one channel is
/// kept per site, the one RGGB wants there, without building the full RGB picture in between.
///
/// Where the sensor already carries that colour at that site the interpolation returns the sample
/// itself, so those sites keep their original value and their original noise, while the rest are
/// interpolated and so already a little smoother. Anything reading the result sees noise that varies
/// from site to site, and the interpolation's own errors: this is an approximation of the sensor, not
/// the sensor.
///
/// `None` when the mosaic is not one this can rebuild: already demosaiced data (`cpp != 1`), no CFA, a
/// CFA that is already Bayer (read that one as it is), a pattern whose size does not match its colours
/// or holding a colour that is not red, green or blue (RGBE, CYGM) or missing one of the three, or a
/// size that does not match the samples.
pub fn remosaic_rggb(n: &Normalized) -> Option<Normalized> {
    let cfa = n.cfa.as_ref()?;
    if n.cpp != 1 || cfa.is_bayer() {
        return None;
    }
    if cfa.width == 0 || cfa.height == 0 || cfa.width.checked_mul(cfa.height) != Some(cfa.pattern.len()) {
        return None;
    }
    if !(0..=2).all(|c| cfa.pattern.contains(&c)) || cfa.pattern.iter().any(|&c| c > 2) {
        return None;
    }
    if n.width == 0 || n.height == 0 || n.width.checked_mul(n.height) != Some(n.data.len()) {
        return None;
    }
    let data = xtrans::rggb(&Mosaic { w: n.width, h: n.height, data: &n.data, cfa });
    Some(Normalized { width: n.width, height: n.height, cpp: 1, data, cfa: Some(Cfa::bayer_static("RGGB")) })
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

    fn smooth_scene(w: usize, h: usize) -> Rgb32f {
        Rgb32f::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            [0.2 + 0.6 * fx * fy, 0.3 + 0.4 * (fx * 3.0).sin().abs() * 0.5 + 0.2 * fy, 0.1 + 0.5 * (1.0 - fx) * (0.5 + 0.5 * (fy * 5.0).cos())]
        })
    }

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
            for m in [Method::Bilinear, Method::Ppg, Method::Ahd] {
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
    fn known_samples_are_preserved() {
        let img = edgy_scene(40, 32);
        let cfa = Cfa::bayer("GRBG").unwrap();
        for m in [Method::Bilinear, Method::Ppg, Method::Ahd] {
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
    fn psnr_thresholds_and_ranking() {
        let smooth = smooth_scene(96, 80);
        let edgy = edgy_scene(96, 80);
        let cfa = Cfa::bayer("RGGB").unwrap();
        let p = |img: &Rgb32f, m| psnr(img, &run(img, &cfa, m), 4);
        let (sb, sp, sa) = (p(&smooth, Method::Bilinear), p(&smooth, Method::Ppg), p(&smooth, Method::Ahd));
        assert!(sb > 38.0 && sp > 38.0 && sa > 38.0, "smooth: bilinear {sb:.1} ppg {sp:.1} ahd {sa:.1}");
        let (eb, ep, ea) = (p(&edgy, Method::Bilinear), p(&edgy, Method::Ppg), p(&edgy, Method::Ahd));
        eprintln!("smooth: bilinear {sb:.1} ppg {sp:.1} ahd {sa:.1}; edgy: bilinear {eb:.1} ppg {ep:.1} ahd {ea:.1}");
        assert!(eb > 20.0, "edgy bilinear {eb:.1}");
        assert!(ep > eb + 6.0, "ppg {ep:.1} vs bilinear {eb:.1}");
        assert!(ea > eb + 6.0, "ahd {ea:.1} vs bilinear {eb:.1}");
        let xt = Cfa::xtrans();
        let xp = psnr(&edgy, &run(&edgy, &xt, Method::Ahd), 6);
        let xb = psnr(&edgy, &run(&edgy, &xt, Method::Bilinear), 6);
        assert!(xp > 20.0 && xp >= xb - 0.5, "xtrans directional {xp:.1} bilinear {xb:.1}");
    }

    #[test]
    fn tiny_and_degenerate_inputs() {
        let cfa = Cfa::bayer("RGGB").unwrap();
        for (w, h) in [(1, 1), (2, 2), (3, 5), (5, 3), (1, 7)] {
            let img = Rgb32f::filled(w, h, [0.5; 3]);
            for m in [Method::Bilinear, Method::Ppg, Method::Ahd] {
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
            for m in [Method::Bilinear, Method::Ppg, Method::Ahd] {
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

    #[test]
    fn reflect_indices() {
        assert_eq!(reflect(-1, 5), 1);
        assert_eq!(reflect(-2, 5), 2);
        assert_eq!(reflect(5, 5), 3);
        assert_eq!(reflect(6, 5), 2);
        assert_eq!(reflect(3, 1), 0);
        assert_eq!(reflect(-7, 2), 1);
    }

    /// The single-pass re-mosaic must give exactly what demosaicing and then keeping each site's RGGB
    /// colour gives: every phase of the pattern, the borders, and samples with no structure to hide behind.
    #[test]
    fn remosaic_matches_the_demosaic_sampled_as_rggb() {
        let mut seed = 2024u32;
        for (dx, dy) in [(0, 0), (1, 2), (5, 3)] {
            let cfa = Cfa::xtrans().shifted(dx, dy);
            let (w, h) = (37, 29);
            let data: Vec<f32> = (0..w * h)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 8) as f32 / (1u32 << 24) as f32
                })
                .collect();
            let n = Normalized { width: w, height: h, cpp: 1, data, cfa: Some(cfa) };
            let want = mosaic_from_rgb(&demosaic(&n, Method::Ahd), &Cfa::bayer("RGGB").unwrap());
            let got = remosaic_rggb(&n).unwrap();
            assert_eq!((got.width, got.height, got.cpp), (w, h, 1));
            assert!(got.data.iter().zip(&want.data).all(|(a, b)| a.to_bits() == b.to_bits()), "phase ({dx}, {dy})");
        }
    }

    #[test]
    fn remosaic_keeps_a_constant_colour_exactly() {
        let colour = [0.3f32, 0.55, 0.8];
        let n = mosaic_from_rgb(&Rgb32f::filled(19, 14, colour), &Cfa::xtrans());
        let r = remosaic_rggb(&n).unwrap();
        let cfa = r.cfa.clone().unwrap();
        assert_eq!(cfa.name(), "RGGB");
        for y in 0..r.height {
            for x in 0..r.width {
                let want = colour[cfa.color_at(x, y) as usize];
                assert!((r.data[y * r.width + x] - want).abs() < 1e-5, "({x}, {y})");
            }
        }
    }

    /// The sites where X-Trans already carries the colour RGGB wants keep their own sample, untouched.
    #[test]
    fn remosaic_keeps_the_samples_the_two_patterns_share() {
        let img = edgy_scene(40, 32);
        let xt = Cfa::xtrans();
        let n = mosaic_from_rgb(&img, &xt);
        let r = remosaic_rggb(&n).unwrap();
        let rggb = r.cfa.clone().unwrap();
        let mut shared = 0;
        for y in 0..32 {
            for x in 0..40 {
                let c = rggb.color_at(x, y);
                if xt.color_at(x, y) != c {
                    continue;
                }
                shared += 1;
                assert_eq!(r.data[y * 40 + x].to_bits(), img.get(x, y)[c as usize].to_bits(), "({x}, {y})");
            }
        }
        // about two sites in five: 20/36 greens against RGGB's half, 8/36 reds and blues against its quarters
        assert!(shared > 40 * 32 / 4, "only {shared} shared sites");
    }

    /// Reading the re-mosaic back as Bayer costs a little against demosaicing the X-Trans mosaic directly,
    /// but must stay close: it is the picture the denoiser's Bayer contract starts from.
    #[test]
    fn remosaic_round_trip_stays_close_to_the_direct_demosaic() {
        for img in [smooth_scene(96, 80), edgy_scene(96, 80)] {
            let n = mosaic_from_rgb(&img, &Cfa::xtrans());
            let direct = psnr(&img, &demosaic(&n, Method::Ahd), 6);
            let through = psnr(&img, &demosaic(&remosaic_rggb(&n).unwrap(), Method::Ahd), 6);
            eprintln!("direct {direct:.1} dB, through RGGB {through:.1} dB");
            assert!(through > 24.0, "through RGGB {through:.1} dB");
            assert!(through > direct - 2.0, "through RGGB {through:.1} dB against direct {direct:.1} dB");
        }
    }

    #[test]
    fn remosaic_refuses_what_it_cannot_rebuild() {
        let img = smooth_scene(12, 10);
        let xtrans = || mosaic_from_rgb(&img, &Cfa::xtrans());
        // already Bayer: the caller reads that mosaic as it is
        assert!(remosaic_rggb(&mosaic_from_rgb(&img, &Cfa::bayer("GRBG").unwrap())).is_none());
        // already demosaiced
        let mut three = xtrans();
        three.cpp = 3;
        assert!(remosaic_rggb(&three).is_none());
        // no colour filter array at all (a monochrome sensor)
        let mut mono = xtrans();
        mono.cfa = None;
        assert!(remosaic_rggb(&mono).is_none());
        // a fourth colour (RGBE), a pattern missing blue, and patterns whose size does not match their colours
        // (a zero side would divide by zero when looking a colour up, a short pattern would read past its end)
        for (width, height, pattern) in
            [(2, 2, vec![0, 1, 1, 3]), (2, 2, vec![0, 1, 1, 1]), (0, 2, vec![0, 1, 2]), (2, 2, vec![0, 1, 2]), (6, 6, vec![])]
        {
            let mut odd = xtrans();
            odd.cfa = Some(Cfa { width, height, pattern });
            assert!(remosaic_rggb(&odd).is_none(), "{width}×{height}");
        }
        // samples that do not match the size
        let mut short = xtrans();
        short.data.pop();
        assert!(remosaic_rggb(&short).is_none());
        let mut empty = xtrans();
        (empty.width, empty.height, empty.data) = (0, 0, Vec::new());
        assert!(remosaic_rggb(&empty).is_none());
    }

    /// The re-mosaic on a real non-Bayer file, which no synthetic pattern stands in for (the active-area crop
    /// moves the pattern's phase, and sensor noise is what the denoiser is given the result for):
    /// `LC_XTRANS_RAW=<file> cargo test --release -p lightcraft-raw --lib -- --ignored --nocapture remosaic_a_real_file`
    #[test]
    #[ignore = "needs LC_XTRANS_RAW pointing at a real non-Bayer raw file"]
    fn remosaic_a_real_file() {
        let Some(path) = std::env::var_os("LC_XTRANS_RAW") else {
            lightcraft_testkit::skip!(Input, "set LC_XTRANS_RAW=<path to a non-Bayer raw file>");
            return;
        };
        let raw = crate::decode(&std::fs::read(&path).unwrap()).unwrap();
        let n = raw.normalized().unwrap();
        let cfa = n.cfa.clone().unwrap();
        assert!(!cfa.is_bayer(), "not a non-Bayer file");
        let t = std::time::Instant::now();
        let r = remosaic_rggb(&n).unwrap();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!((r.width, r.height), (n.width, n.height));
        assert!(r.data.iter().all(|v| v.is_finite()));
        let rggb = r.cfa.clone().unwrap();
        let (mut shared, mut kept) = (0usize, 0usize);
        for y in 0..n.height {
            for x in 0..n.width {
                if cfa.color_at(x, y) == rggb.color_at(x, y) {
                    shared += 1;
                    kept += usize::from(r.data[y * n.width + x].to_bits() == n.data[y * n.width + x].to_bits());
                }
            }
        }
        let t = std::time::Instant::now();
        let direct = demosaic(&n, Method::Ahd);
        let direct_ms = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "{}×{}: re-mosaic {ms:.0} ms (a full demosaic {direct_ms:.0} ms), {shared} shared sites ({:.1} %), {kept} of them \
             untouched, PSNR through RGGB against the direct demosaic {:.1} dB",
            n.width,
            n.height,
            100.0 * shared as f64 / (n.width * n.height) as f64,
            psnr(&direct, &demosaic(&r, Method::Ahd), 8)
        );
        assert_eq!(kept, shared, "a shared site lost its own sample");
    }

    #[test]
    #[ignore]
    fn throughput() {
        let (w, h) = (6000, 4000);
        let img = smooth_scene(w, h);
        let cfa = Cfa::bayer("RGGB").unwrap();
        let n = mosaic_from_rgb(&img, &cfa);
        for m in [Method::Bilinear, Method::Ppg, Method::Ahd] {
            let t = std::time::Instant::now();
            let _ = demosaic(&n, m);
            let s = t.elapsed().as_secs_f64();
            println!("{m:?}: {:.1} MP/s ({:.0} ms for 24 MP)", (w * h) as f64 / 1e6 / s, s * 1e3);
        }
        let n = mosaic_from_rgb(&img, &Cfa::xtrans());
        let t = std::time::Instant::now();
        let _ = demosaic(&n, Method::Ahd);
        println!("X-Trans directional: {:.1} MP/s", (w * h) as f64 / 1e6 / t.elapsed().as_secs_f64());
    }
}
